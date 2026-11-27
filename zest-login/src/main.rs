//! zest-login: a sign-in window for one store, started by Zest when the player presses Sign in and gone again when it is done.
//!
//! It shows the store's own login page in the system web view (WebKitGTK, WebView2, WKWebView), so the password is typed on the store's page and never reaches Zest. When the sign-in
//! has produced what Zest needs, it prints ONE line of JSON to stdout and exits:
//!   {"ok":true,"value":"..."}      the value the store's paste box would have taken
//!   {"ok":false,"error":"closed"}  the window was closed first ("timeout" after the time limit, or an error message)
//! The window is private (nothing is kept between runs). The value is never written anywhere but that line.
//!
//! Usage: zest-login '<spec json>'   (or --spec-file <path>)
//!   {"title":"Sign in to GOG","url":"https://...","timeout_secs":600,
//!    "done":{"kind":"url","contains":"code="}}                                      value = the address once it contains the text
//!    "done":{"kind":"body","url_contains":"/redirect","text_contains":"code"}      value = the page's text on a matching address
//!    "done":{"kind":"cookies","url":"https://www.humblebundle.com/","names":["_simpleauth_sess"],"header":false,"url_contains":"/home"}
//!                                                                                   value = the cookie (one name) or a Cookie header (several, or header:true)
use serde::Deserialize;
use std::time::{Duration, Instant};
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::WindowBuilder;
use wry::WebViewBuilder;

#[derive(Deserialize, Clone)]
struct Spec { title: String, url: String, #[serde(default = "ten_minutes")] timeout_secs: u64, done: Done, #[serde(default)] user_agent: Option<String> }
fn ten_minutes() -> u64 { 600 }

#[derive(Deserialize, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Done {
    Url { contains: String },
    /// The page's text on a matching address; with `token_len` the answer is the first run of exactly that many letters and digits instead (a page that shows a key), and `text_contains` may be empty.
    Body { url_contains: String, #[serde(default)] text_contains: String, #[serde(default)] token_len: Option<usize>,
        /// On a matching address, click the first link or button with exactly this text (a page that hides the key behind a View button).
        #[serde(default)] click: Option<String> },
    Cookies { url: String, names: Vec<String>, #[serde(default)] header: bool, #[serde(default)] url_contains: Option<String>,
        /// A page address that must NOT count (a sign-in start page that already shows an anonymous visitor's cookies).
        #[serde(default)] url_not_contains: Option<String>,
        /// `names: ["*"]` only: the sign-in is not done until a cookie with this name prefix exists (the session cookie, as against the anonymous ones).
        #[serde(default)] need_prefix: Option<String>,
        /// The sign-in is not done until the page's own `/api/` answer (fetched from inside the page, with its cookies) contains this text, spaces ignored: the website itself says it is signed in.
        #[serde(default)] probe_contains: Option<String>,
        /// What to do once this is satisfied instead of giving the answer: go to this address and wait for that (a store whose sign-in leaves a session that a second page turns into the token).
        #[serde(default)] then: Option<Box<Then>>,
        /// On pages whose address contains `click_on`, click the first link or button with exactly this text (any letter case): a landing page whose Sign In button has to be pressed.
        #[serde(default)] click: Option<String>, #[serde(default)] click_on: Option<String>,
        /// Not done until some page whose address contains this has been shown (a sign-in that has to be gone through before an address counts as the way back).
        #[serde(default)] seen: Option<String>,
        /// Let the page use the camera and the like instead of refusing (a sign-in page that needs the answer to work).
        #[serde(default)] allow_permissions: bool,
        /// Turn on WebGL and hardware drawing (a bot-detection script that reads the graphics card fails without them).
        #[serde(default)] graphics: bool,
        /// Only say in the trace that this is satisfied and keep the window open (for finding out what a sign-in does).
        #[serde(default)] observe: bool },
    /// Cookies from several addresses at once, as `{"<url>": "<cookie value>"}`. Done when every jar that is not `optional` has its cookies and the page is not on a
    /// `url_not_contains` address; optional jars get `grace_secs` more to show up (a sign-in that sets cookies on sibling sites one after another), then the answer is given without them.
    Jars { jars: Vec<Jar>, #[serde(default)] url_not_contains: Option<String>, #[serde(default = "six")] grace_secs: u64 },
}
#[derive(Deserialize, Clone)]
struct Then { url: String, done: Done }
#[derive(Deserialize, Clone)]
struct Jar { url: String, names: Vec<String>, #[serde(default)] optional: bool, /// The answer's key for this jar (the address when absent): two jars on one address need two keys.
    #[serde(default)] key: Option<String> }
fn six() -> u64 { 6 }

/// The answer for `Jars`: Some(JSON object) once every required jar is complete and the optional ones are complete or `grace_over`; None while waiting. `have[i]` = the cookies of `jars[i]`.
fn jars_value(jars: &[Jar], have: &[Vec<(String, String)>], grace_over: bool) -> Option<String> {
    let mut out = serde_json::Map::new();
    let mut waiting = false;
    for (j, h) in jars.iter().zip(have) {
        match cookie_value(&j.names, false, h) {
            Some(v) => { out.insert(j.key.clone().unwrap_or_else(|| j.url.clone()), serde_json::Value::String(v)); }
            None if j.optional => waiting = true,
            None => return None,
        }
    }
    if waiting && !grace_over { return None; }
    Some(serde_json::Value::Object(out).to_string())
}

enum Ev { Page { url: String, text: String }, Probe { url: String, text: String }, Url(String), /// The page's Cancel button or the controller's B (Game Mode has no title bar to close with).
    Cancel }

fn out(ok: bool, key: &str, v: &str) -> ! {
    trace(&format!("finished: ok={ok} {key} ({} characters{})", v.chars().count(), if ok { String::new() } else { format!(": {v}") }));   // a failure's reason is a message; a value only has its length
    println!("{}", serde_json::json!({ "ok": ok, key: v }));
    std::process::exit(if ok { 0 } else { 1 });
}

/// The finish check for a page, split out so it can be tested: Some(value) when `done` is met by this page (Cookies is decided by the caller, which owns the cookie jar).
/// The first run of exactly `n` letters and digits in the text (a key shown on a page).
fn token_of_len(text: &str, n: usize) -> Option<String> {
    text.split(|c: char| !c.is_ascii_alphanumeric()).find(|w| w.len() == n).map(String::from)
}
fn page_done(done: &Done, url: &str, text: &str) -> Option<String> {
    match done {
        Done::Url { contains } => url.contains(contains.as_str()).then(|| url.to_string()),
        Done::Body { url_contains, text_contains, token_len, .. } => {
            if !(url.contains(url_contains.as_str()) && text.contains(text_contains.as_str())) { return None; }
            match token_len { Some(n) => token_of_len(text, *n), None => Some(text.trim().to_string()) }
        }
        Done::Cookies { .. } | Done::Jars { .. } => None,
    }
}
/// A Cookie header (`a=b; c=d`) or, for one wanted name without `header`, just that cookie's value; None until every wanted name is present and non-empty.
/// `names == ["*"]` takes the whole jar instead: a JSON object `{name: value}` of every non-empty cookie, once one of them starts with `need_prefix` (when given).
fn cookie_value(names: &[String], header: bool, have: &[(String, String)]) -> Option<String> { cookie_value_with(names, header, have, None) }
fn cookie_value_with(names: &[String], header: bool, have: &[(String, String)], need_prefix: Option<&str>) -> Option<String> {
    if names.len() == 1 && names[0] == "*" {
        let all: serde_json::Map<String, serde_json::Value> = have.iter().filter(|(_, v)| !v.is_empty()).map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone()))).collect();
        if all.is_empty() { return None; }
        if let Some(pre) = need_prefix { if !all.keys().any(|k| k.starts_with(pre) && !k.contains("login")) { return None; } }
        return Some(serde_json::Value::Object(all).to_string());
    }
    // `ory_session_*`: every cookie whose name starts with the text before the star (a session cookie whose name carries a per-site suffix), as a Cookie header
    if names.len() == 1 && names[0].ends_with('*') && names[0] != "*" {
        let pre = &names[0][..names[0].len() - 1];
        let hits: Vec<String> = have.iter().filter(|(k, v)| k.starts_with(pre) && !v.is_empty()).map(|(k, v)| format!("{k}={v}")).collect();
        return (!hits.is_empty()).then(|| hits.join("; "));
    }
    let mut found = vec![];
    for n in names { found.push((n.clone(), have.iter().find(|(k, v)| k == n && !v.is_empty())?.1.clone())); }
    if found.is_empty() { return None; }
    Some(if names.len() == 1 && !header { found[0].1.clone() } else { found.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; ") })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let raw = match args.first().map(String::as_str) {
        Some("--spec-file") => std::fs::read_to_string(args.get(1).map(String::as_str).unwrap_or("")).unwrap_or_default(),
        Some(s) => s.to_string(),
        None => out(false, "error", "no spec given"),
    };
    let spec: Spec = serde_json::from_str(&raw).unwrap_or_else(|e| out(false, "error", &format!("bad spec: {e}")));
    trace(&format!("start: {} ({}), looking for {}", spec.title, bare(&spec.url), match &spec.done { Done::Url { contains } => format!("an address with {contains:?}"), Done::Body { url_contains, text_contains, .. } => format!("{text_contains:?} on an address with {url_contains:?}"),
        Done::Cookies { names, url_contains, url_not_contains, .. } => format!("cookies {names:?} (address must contain {url_contains:?}, not {url_not_contains:?})"), Done::Jars { jars, .. } => format!("cookies at {} addresses", jars.len()) }));
    if !(spec.url.starts_with("https://") || spec.url.starts_with("http://")) { out(false, "error", "the address must be http(s)"); }

    let event_loop = EventLoopBuilder::<Ev>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    // tall enough for a store's whole sign-in form (Steam's runs long): most of the screen's height, 740 to 1000 points, and still resizable; never taller than the screen
    // (a Steam Deck's 800 px in Game Mode cut the bottom of Steam's QR code off)
    let screen_h = event_loop.primary_monitor().map(|m| m.size().to_logical::<f64>(m.scale_factor()).height);
    let height = screen_h.map(|h| (h * 0.85).clamp(740.0_f64.min(h * 0.95), 1000.0)).unwrap_or(900.0);
    // gamescope (Steam Deck Game Mode) shows one window filling the screen anyway: start maximized so the page is laid out for the whole screen
    let gamescope = std::env::var_os("GAMESCOPE_WAYLAND_DISPLAY").is_some() || std::env::var("XDG_SESSION_DESKTOP").map_or(false, |d| d == "gamescope");
    // there the window is the whole screen: asking to be maximized did nothing under gamescope, and at 660 wide Steam's page stacks its QR code under the form, below the
    // bottom of an 800 px screen (at full width it sits beside the form)
    let size = match (gamescope, event_loop.primary_monitor()) { (true, Some(m)) => m.size().to_logical::<f64>(m.scale_factor()), _ => tao::dpi::LogicalSize::new(660.0, height) };
    let window = WindowBuilder::new().with_title(&spec.title).with_inner_size(size).with_maximized(gamescope).build(&event_loop).unwrap_or_else(|e| out(false, "error", &format!("no window: {e}")));

    // every page reports its address and text once loaded (a JSON page's text is what an Epic or PlayStation sign-in ends on)
    let report = r#"
        (function () {
            // what goes wrong on a page, for the trace: failed requests (address without its query, and the status) and script errors; never a body, a header or a value
            function strip(u) { return String(u || '').split('?')[0].split('#')[0].slice(0, 120); }
            function note(m) { try { window.ipc.postMessage(JSON.stringify({ dbg: String(m).slice(0, 200) })); } catch (e) {} }
            try { var of = window.fetch; if (of) window.fetch = function () { var a = arguments, u = (typeof a[0] === 'string' ? a[0] : (a[0] && a[0].url)) || ''; return of.apply(this, a).then(function (r) { if (!r.ok) note('fetch ' + r.status + ' ' + strip(u)); return r; }, function (e) { note('fetch failed ' + strip(u)); throw e; }); }; } catch (e) {}
            try { var ox = XMLHttpRequest.prototype.open; XMLHttpRequest.prototype.open = function (m, u) { var self = this; this.addEventListener('loadend', function () { if (self.status === 0 || self.status >= 400) note('xhr ' + self.status + ' ' + strip(u)); }); return ox.apply(this, arguments); }; } catch (e) {}
            window.addEventListener('error', function (e) { if (e.target && e.target !== window && (e.target.src || e.target.href)) note('failed to load ' + strip(e.target.src || e.target.href)); else note('script error: ' + String(e.message).slice(0, 120) + ' at ' + strip(e.filename) + ':' + e.lineno); }, true);
            window.addEventListener('unhandledrejection', function (e) { note('unhandled rejection: ' + String((e.reason && e.reason.message) || e.reason).slice(0, 120)); });
            var last = '';
            // a page that changes without loading (a key revealed by a View button) is reported again when its text changes
            function again() { try { var t = ((document.body && document.body.innerText) || '') + '\\n' + Array.prototype.map.call(document.querySelectorAll('input:not([type=hidden]):not([type=password]),textarea'), function (e) { return e.value || ''; }).join('\\n'); if (t !== last) { last = t; send(); } } catch (e) {} }
            function press() { try { if (!window.__zc || window.__zdone || location.href.indexOf(window.__zu) < 0) return; var els = document.querySelectorAll('a,button'); for (var i = 0; i < els.length; i++) { if ((els[i].textContent || '').trim().toLowerCase() === window.__zc.toLowerCase()) { window.__zdone = true; els[i].click(); return; } } } catch (e) {} }
            setInterval(function () { press(); again(); }, 1000);
            function send() { try { window.ipc.postMessage(JSON.stringify({ u: location.href, t: ((document.body && document.body.innerText) || '') + '\n' + Array.prototype.map.call(document.querySelectorAll('input:not([type=hidden]):not([type=password]),textarea'), function (e) { return e.value || ''; }).join('\n') })); } catch (e) {} }
            function probe() { if (location.host !== 'account.battle.net') return; try { fetch('/api/', { credentials: 'include' }).then(function (r) { return r.text(); }).then(function (t) { window.ipc.postMessage(JSON.stringify({ u: location.href, probe: t })); }).catch(function () {}); } catch (e) {} }
            window.addEventListener('load', function () { send(); setTimeout(send, 1200); probe(); setTimeout(probe, 1500); setTimeout(probe, 4000); });
        })();"#;
    // Game Mode (gamescope) draws no title bar, so the window could not be closed without signing in: a Cancel button floats over every page there, and the
    // controller's B does the same (the page reads it through the browser's gamepad API)
    let cancel = if gamescope { r#"
        (function () {
            function quit() { try { window.ipc.postMessage(JSON.stringify({ cancel: true })); } catch (e) {} }
            function add() {
                if (!document.body || document.getElementById('__zest_cancel')) return;
                var b = document.createElement('button'); b.id = '__zest_cancel'; b.textContent = '✕ CANCEL';
                b.setAttribute('style', 'position:fixed;top:10px;right:10px;z-index:2147483647;padding:10px 16px;font:bold 15px sans-serif;letter-spacing:1px;color:#fff;background:#a32a2a;border:0;border-radius:6px;cursor:pointer');
                b.addEventListener('click', quit); document.body.appendChild(b);
            }
            document.addEventListener('DOMContentLoaded', add); window.addEventListener('load', add); setInterval(add, 1000);
            var held = false;
            setInterval(function () { try { var ps = navigator.getGamepads ? navigator.getGamepads() : []; var b = false; for (var i = 0; i < ps.length; i++) { if (ps[i] && ps[i].buttons[1] && ps[i].buttons[1].pressed) b = true; }
                if (b && !held) quit(); held = b; } catch (e) {} }, 100);
        })();"# } else { "" };
    let (p1, p2, done_nav) = (proxy.clone(), proxy.clone(), spec.done.clone());
    let mut builder = WebViewBuilder::new()
        .with_url(&spec.url)
        .with_incognito(true)
        .with_initialization_script(&format!("{}{report}{cancel}", match &spec.done { Done::Body { click: Some(t), url_contains, .. } => format!("window.__zc = {}; window.__zu = {};\n", serde_json::json!(t), serde_json::json!(url_contains)), Done::Cookies { click: Some(t), click_on: Some(u), .. } => format!("window.__zc = {}; window.__zu = {};\n", serde_json::json!(t), serde_json::json!(u)), _ => String::new() }))
        .with_navigation_handler(move |url| { if let Done::Url { .. } = &done_nav { if page_done(&done_nav, &url, "").is_some() { let _ = p1.send_event(Ev::Url(url.clone())); } } true })
        .with_ipc_handler(move |req| {
            let body = req.body();
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
                if let Some(d) = v["dbg"].as_str() { trace(&format!("  page note: {d}")); return; }
                if v["cancel"].as_bool() == Some(true) { let _ = p2.send_event(Ev::Cancel); return; }
                if let Some(t) = v["probe"].as_str() { let _ = p2.send_event(Ev::Probe { url: v["u"].as_str().unwrap_or("").to_string(), text: t.to_string() }); return; }
                let _ = p2.send_event(Ev::Page { url: v["u"].as_str().unwrap_or("").to_string(), text: v["t"].as_str().unwrap_or("").to_string() }); }
        });
    if let Some(ua) = &spec.user_agent { builder = builder.with_user_agent(ua); }
    #[cfg(target_os = "linux")]
    let webview = { use tao::platform::unix::WindowExtUnix; use wry::WebViewBuilderExtUnix; builder.build_gtk(window.default_vbox().expect("a gtk window has a box")) };
    #[cfg(not(target_os = "linux"))]
    let webview = builder.build(&window);
    let webview = webview.unwrap_or_else(|e| out(false, "error", &format!("the web view could not start: {e}")));
    // a sign-in page has no business with the camera, microphone, location or notifications (PlayStation's asked for the camera over and over): every permission request is refused
    #[cfg(target_os = "linux")]
    if matches!(&spec.done, Done::Cookies { graphics: true, .. }) { use wry::WebViewExtUnix; use webkit2gtk::{HardwareAccelerationPolicy, SettingsExt, WebViewExt}; if let Some(st) = webview.webview().settings() { st.set_enable_webgl(true); st.set_hardware_acceleration_policy(HardwareAccelerationPolicy::Always); } }
    #[cfg(target_os = "linux")]
    { use wry::WebViewExtUnix; use webkit2gtk::{PermissionRequestExt, WebViewExt}; let allow = matches!(&spec.done, Done::Cookies { allow_permissions: true, .. }); webview.webview().connect_permission_request(move |_, request| { if allow { request.allow(); } else { request.deny(); } true }); }

    let started = Instant::now();
    let limit = Duration::from_secs(spec.timeout_secs.max(30));
    // the address of the last page that finished loading: the start address is NOT used (a store's login redirect has not happened yet, and an anonymous visitor already has cookies)
    let mut current_url = String::new();
    let mut authed = false;
    let seen_flag = std::cell::Cell::new(false);
    let observed = std::cell::Cell::new(false);
    let mut done_now = spec.done.clone();   // a sign-in with a `then` step changes what it waits for once the first part is done
    let jars_first_ok: std::cell::Cell<Option<Instant>> = std::cell::Cell::new(None);
    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(800));
        if started.elapsed() > limit { out(false, "error", "timeout"); }
        let check_cookies = |url: &str, authed: bool| -> Option<String> {
            if let Done::Jars { jars, url_not_contains, grace_secs } = &done_now {
                if url.is_empty() { return None; }
                if let Some(c) = url_not_contains { if c.split('|').any(|p| !p.is_empty() && url.contains(p)) { return None; } }
                let have: Vec<Vec<(String, String)>> = jars.iter().map(|j| webview.cookies_for_url(&j.url).map(|v| v.into_iter().map(|c| (c.name().to_string(), c.value().to_string())).collect()).unwrap_or_default()).collect();
                let required_ok = jars.iter().zip(&have).all(|(j, h)| j.optional || cookie_value(&j.names, false, h).is_some());
                if required_ok && jars_first_ok.get().is_none() { jars_first_ok.set(Some(Instant::now())); }
                let over = jars_first_ok.get().map_or(false, |t| t.elapsed() >= Duration::from_secs(*grace_secs));
                return jars_value(jars, &have, over);
            }
            let Done::Cookies { url: cu, names, header, url_contains, url_not_contains, need_prefix, probe_contains, seen, .. } = &done_now else { return None };
            if seen.is_some() && !seen_flag.get() { return None; }
            if probe_contains.is_some() && !authed { return None; }
            if url.is_empty() { return None; }
            if let Some(c) = url_contains { if !url.contains(c.as_str()) { return None; } }
            if let Some(c) = url_not_contains { if c.split('|').any(|p| !p.is_empty() && url.contains(p)) { return None; } }
            let have: Vec<(String, String)> = if names.len() == 1 && names[0] == "*" {
                // the whole store, as PlayDate does: the session cookies can sit on a sibling battle.net domain that a per-address lookup leaves out
                webview.cookies().ok()?.into_iter().filter(|c| c.domain().map(|d| d.contains(site_of(cu))).unwrap_or(true)).map(|c| (c.name().to_string(), c.value().to_string())).collect()
            } else { webview.cookies_for_url(cu).ok()?.into_iter().map(|c| (c.name().to_string(), c.value().to_string())).collect() };
            let v = cookie_value_with(names, *header, &have, need_prefix.as_deref());
            if v.is_some() && matches!(&done_now, Done::Cookies { observe: true, .. }) { if !observed.replace(true) { trace("  satisfied here (observe only: the window stays open)"); } return None; }
            v
        };
        match event {
            Event::UserEvent(Ev::Url(u)) => out(true, "value", &u),
            Event::UserEvent(Ev::Probe { url, text }) => {
                if let Done::Cookies { probe_contains: Some(want), .. } = &done_now {
                    authed = text.split_whitespace().collect::<String>().contains(want.as_str());
                    current_url = url.clone();
                    if authed { if let Some(v) = check_cookies(&current_url, authed) { out(true, "value", &v); } }
                }
            }
            Event::UserEvent(Ev::Page { url, text }) => {
                current_url = url.clone();
                trace(&format!("page: {}", bare(&url)));
                if let Done::Cookies { seen: Some(sn), .. } = &done_now { if url.contains(sn.as_str()) { seen_flag.set(true); } }
                // a page that should show a key: how long its words are (never the words), to see why none matched
                if let Done::Body { token_len: Some(n), url_contains, .. } = &done_now { if url.contains(url_contains.as_str()) {
                    let longest = text.split(|c: char| !c.is_ascii_alphanumeric()).map(|w| w.len()).max().unwrap_or(0);
                    trace(&format!("  page text: {} characters, longest letters-and-digits run {longest}, runs of exactly {n}: {}", text.chars().count(), text.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| w.len() == *n).count()));
                } }
                if let Done::Jars { jars, .. } = &done_now { for j in jars { let names: Vec<String> = webview.cookies_for_url(&j.url).map(|v| v.into_iter().map(|c| c.name().to_string()).collect()).unwrap_or_default(); trace(&format!("  cookies at {}: {names:?}", bare(&j.url))); } }
                if let Done::Cookies { url: cu, .. } = &done_now { let names: Vec<String> = webview.cookies_for_url(cu).map(|v| v.into_iter().map(|c| c.name().to_string()).collect()).unwrap_or_default(); trace(&format!("  cookies at {cu}: {names:?}")); }
                if let Some(v) = page_done(&done_now, &url, &text).or_else(|| check_cookies(&url, authed)) {
                    let next = if let Done::Cookies { then: Some(t), .. } = &done_now { Some((t.url.clone(), t.done.clone())) } else { None };
                    match next {
                        Some((u, d)) => { trace(&format!("  signed in; now asking {} for the token", bare(&u))); done_now = d; current_url.clear(); let _ = webview.load_url(&u); }
                        None => { trace("  done"); out(true, "value", &v); }
                    }
                }
            }
            // a sign-in that finishes without a page load (a script setting a cookie) is caught by looking at the jar every so often
            Event::NewEvents(StartCause::ResumeTimeReached { .. }) => { if let Some(v) = check_cookies(&current_url, authed) { out(true, "value", &v); } }
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } | Event::UserEvent(Ev::Cancel) => out(false, "error", "closed"),
            _ => {}
        }
    });
}

/// The site a cookie address belongs to, as the last two parts of its host (`https://accounts.nintendo.com/` -> `nintendo.com`): the whole-jar lookup keeps cookies on that site only.
fn site_of(url: &str) -> &str {
    let host = url.split("://").nth(1).unwrap_or(url).split(['/', ':']).next().unwrap_or("");
    let dots: Vec<usize> = host.match_indices('.').map(|(i, _)| i).collect();
    if dots.len() >= 2 { &host[dots[dots.len() - 2] + 1..] } else { host }
}

/// A line for the diagnostics file (`zest-login.log` in the temp folder): where the window is (address without its query) and which cookie NAMES the jar holds, never a value.
fn trace(line: &str) {
    use std::io::Write;
    let p = std::env::temp_dir().join("zest-login.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) { let _ = writeln!(f, "{line}"); }
}
fn bare(url: &str) -> String { url.split(['?', '#']).next().unwrap_or(url).to_string() }

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn an_address_finishes_a_url_sign_in_only_when_it_has_the_code() {
        let d = Done::Url { contains: "code=".into() };
        assert_eq!(page_done(&d, "https://embed.gog.com/on_login_success?origin=client&code=abc", ""), Some("https://embed.gog.com/on_login_success?origin=client&code=abc".into()));
        assert_eq!(page_done(&d, "https://login.gog.com/login", ""), None);
    }
    #[test] fn a_page_text_finishes_a_body_sign_in_on_the_right_address() {
        let d = Done::Body { url_contains: "/id/api/redirect".into(), text_contains: "authorizationCode".into(), token_len: None, click: None };
        assert_eq!(page_done(&d, "https://www.epicgames.com/id/api/redirect?clientId=x", " {\"authorizationCode\":\"zz\"} \n"), Some("{\"authorizationCode\":\"zz\"}".into()));
        assert_eq!(page_done(&d, "https://www.epicgames.com/id/login", "authorizationCode"), None, "the text on the wrong page does not count");
        assert_eq!(page_done(&d, "https://www.epicgames.com/id/api/redirect", "please wait"), None);
    }
    #[test] fn a_cookie_address_names_its_site() {
        assert_eq!(site_of("https://accounts.nintendo.com/"), "nintendo.com");
        assert_eq!(site_of("https://account.battle.net/login"), "battle.net");
        assert_eq!(site_of("https://example.com:8080/x"), "example.com");
    }
    #[test] fn a_page_that_shows_a_key_gives_the_key_alone() {
        let d = Done::Body { url_contains: "/api-keys".into(), text_contains: String::new(), token_len: Some(40), click: None };
        let key = "aB3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1dE3fG5hI7j";
        assert_eq!(page_done(&d, "https://itch.io/user/settings/api-keys", &format!("Your keys\n{key}\nRevoke")), Some(key.to_string()));
        assert_eq!(page_done(&d, "https://itch.io/user/settings/api-keys", "Your keys\nno key yet"), None);
        assert_eq!(page_done(&d, "https://itch.io/login", key), None);
    }
    #[test] fn a_cookie_whose_name_carries_a_suffix_is_found_by_its_prefix() {
        let have = vec![("__cf_bm".to_string(), "x".to_string()), ("ory_session_stoiccohens510ht9xek".to_string(), "S3CR3T".to_string()), ("ory_kratos_continuity".to_string(), "c".to_string())];
        assert_eq!(cookie_value_with(&["ory_session_*".to_string()], false, &have, None).as_deref(), Some("ory_session_stoiccohens510ht9xek=S3CR3T"));
        assert_eq!(cookie_value_with(&["ory_session_*".to_string()], false, &have[..1], None), None, "no such cookie yet");
        assert_eq!(cookie_value_with(&["ory_session_*".to_string()], false, &[("ory_session_a".to_string(), String::new())], None), None, "an empty value does not count");
    }
    #[test] fn cookies_finish_only_when_every_wanted_one_is_there() {
        let have = vec![("sid".to_string(), "v1".to_string()), ("empty".to_string(), String::new()), ("other".to_string(), "o".to_string())];
        assert_eq!(cookie_value(&["sid".into()], false, &have), Some("v1".into()));
        assert_eq!(cookie_value(&["sid".into()], true, &have), Some("sid=v1".into()));
        assert_eq!(cookie_value(&["sid".into(), "other".into()], false, &have), Some("sid=v1; other=o".into()));
        assert_eq!(cookie_value(&["sid".into(), "missing".into()], false, &have), None);
        assert_eq!(cookie_value(&["empty".into()], false, &have), None, "an empty value is not a sign-in");
        assert_eq!(cookie_value(&[], false, &have), None);
    }
    #[test] fn the_whole_jar_is_taken_as_json_once_the_session_cookie_is_there() {
        let anon = vec![("a".to_string(), "1".to_string())];
        let both = vec![("a".to_string(), "1".to_string()), ("BA-tassadar".to_string(), "s".to_string()), ("e".to_string(), String::new())];
        assert_eq!(cookie_value_with(&["*".into()], false, &anon, None), Some(r#"{"a":"1"}"#.into()));
        assert_eq!(cookie_value_with(&["*".into()], false, &anon, Some("BA-")), None, "only anonymous cookies: not signed in yet");
        // compared as JSON, not text: the key order depends on serde_json's features, which a build of the whole workspace changes
        let json = |v: Option<String>| v.map(|t| serde_json::from_str::<serde_json::Value>(&t).unwrap());
        assert_eq!(json(cookie_value_with(&["*".into()], false, &both, Some("BA-"))), json(Some(r#"{"BA-tassadar":"s","a":"1"}"#.into())), "empty values are left out");
        assert_eq!(cookie_value_with(&["*".into()], false, &[], None), None);
        let mid_login = vec![("BA-tassadar-login.key".to_string(), "k".to_string())];
        assert_eq!(cookie_value_with(&["*".into()], false, &mid_login, Some("BA-")), None, "a login-phase cookie is not a session");
    }
    #[test] fn several_jars_finish_when_the_required_ones_are_there_and_optional_ones_get_a_grace_period() {
        let jars = vec![Jar { url: "https://help/".into(), names: vec!["sls".into()], optional: false, key: None }, Jar { url: "https://community/".into(), names: vec!["sls".into()], optional: true, key: None }];
        let help = vec![("sls".to_string(), "H".to_string())]; let comm = vec![("sls".to_string(), "C".to_string())];
        assert_eq!(jars_value(&jars, &[vec![], comm.clone()], true), None, "a required jar is missing");
        assert_eq!(jars_value(&jars, &[help.clone(), vec![]], false), None, "waiting for the optional one");
        assert_eq!(jars_value(&jars, &[help.clone(), vec![]], true), Some(r#"{"https://help/":"H"}"#.into()), "grace over: given without it");
        let json = |v: Option<String>| v.map(|t| serde_json::from_str::<serde_json::Value>(&t).unwrap());
        assert_eq!(json(jars_value(&jars, &[help, comm], false)), json(Some(r#"{"https://community/":"C","https://help/":"H"}"#.into())), "both there: no need to wait");
    }
    #[test] fn a_spec_parses_in_each_form() {
        let a: Spec = serde_json::from_str(r#"{"title":"t","url":"https://x/","done":{"kind":"url","contains":"code="}}"#).unwrap(); assert_eq!(a.timeout_secs, 600);
        let b: Spec = serde_json::from_str(r#"{"title":"t","url":"https://x/","timeout_secs":90,"done":{"kind":"body","url_contains":"/r","text_contains":"t"}}"#).unwrap(); assert_eq!(b.timeout_secs, 90);
        let c: Spec = serde_json::from_str(r#"{"title":"t","url":"https://x/","done":{"kind":"cookies","url":"https://x/","names":["a"]}}"#).unwrap();
        assert!(matches!(c.done, Done::Cookies { header: false, .. }));
    }
}
