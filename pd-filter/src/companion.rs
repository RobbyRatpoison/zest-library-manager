//! The local listener for `native/userscript/zest_companion.user.js`.
//!
//! Zest opens a service's page in the user's browser with `#zest=<port>.<token>.<service>`; the userscript then posts what it
//! found (a session cookie, an NPSSO, PlayStation purchases) back here. Loopback only (127.0.0.1, random port), every request needs
//! the session token in `X-Zest-Token`, and a `Host` header that is not this listener is refused (DNS rebinding). Secrets are
//! handed to the source's own `auth_complete` and never echoed, logged or kept. Standard library only: one blocking accept
//! thread, so an idle listener costs no CPU.
use crate::sources::Registry;
use serde_json::{json, Value};
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

const MAX_HEADER: usize = 32 * 1024;
const MAX_BODY: usize = 16 * 1024 * 1024; // a long PlayStation history is a few MB

/// What the app may want to react to (refresh a status line, run the purchase-date import).
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A source accepted a sign-in (platform id, account name).
    Connected { platform: String, username: Option<String> },
    /// A source refused the captured value.
    Refused { platform: String },
    /// A source's data read in the browser became library records; saving them to the library is the caller's job (the same importer a sync uses).
    Imported { platform: String, records: Vec<crate::importer::GameRecord> },
    /// A SteamGifts wins import ended (what to tell the user, and whether it worked).
    SteamGifts { message: String, ok: bool },
    /// PlayStation purchases arrived; applying them to the library is the caller's job.
    PurchaseDates { service: String, online_id: String, read: usize, matched: usize, set: usize },
}

/// The SteamGifts import's side of the listener: one JSON in, one JSON out per step (`session`, `page`, `pass2-plan`, `finish`); `Err` is the message for the browser tab.
pub trait SteamGiftsHandler: Send + Sync { fn call(&self, route: &str, body: &Value) -> Result<Value, String>; }

pub struct Listener { pub port: u16, pub token: String, stop: Arc<AtomicBool> }

/// The source a service belongs to.
pub fn platform_of(service: &str) -> Option<&'static str> {
    Some(match service { "humble" => "humble", "nintendo" => "nintendo", "psn-login" | "psn-dates" => "playstation", "indiegala" => "indiegala", "itch" => "itch_io", "xbox" => "xbox", "amazon" => "amazon_games", "gog" => "gog", _ => return None })
}

/// The services the userscript knows, with the page Zest opens for each.
pub const SERVICES: &[(&str, &str)] = &[
    ("humble", "https://www.humblebundle.com/"),
    ("indiegala", "https://www.indiegala.com/library"),
    ("itch", "https://itch.io/user/settings/api-keys"),
    // sign-ins that end on a page whose address carries the code: the page to open comes from the source's own `auth_begin` (it holds the attempt's secrets), so no fixed address here
    ("xbox", ""), ("amazon", ""), ("gog", ""),
    ("nintendo", "https://accounts.nintendo.com/portal/vgcs"),
    ("psn-login", "https://ca.account.sony.com/api/v1/ssocookie"),
    ("psn-dates", "https://store.playstation.com/"),
    // the signed-in user's own won page (the private one); the script finds the public page from the name it shows
    ("steamgifts", "https://www.steamgifts.com/giveaways/won"),
    ("steamgifts-full", "https://www.steamgifts.com/giveaways/won"),
    // Steam activation dates: the first page depends on the game, so Zest builds the address (`arm_url_for`)
    ("steam-dates", ""),
];

/// What the Plugins dialog can ask the browser to do for a source: (service, button text).
pub fn actions(platform: &str) -> &'static [(&'static str, &'static str)] {
    match platform {
        "playstation" => &[("psn-login", "SIGN IN WITH BROWSER"), ("psn-dates", "IMPORT PURCHASE DATES")],
        _ => &[],
    }
}

impl Listener {
    pub fn start(registry: Arc<Registry>, on_event: Arc<dyn Fn(Event) + Send + Sync>) -> std::io::Result<Listener> { Self::start_with(registry, on_event, None) }
    /// `sg`: the SteamGifts wins import, when this run has one (it needs the library and the account).
    pub fn start_with(registry: Arc<Registry>, on_event: Arc<dyn Fn(Event) + Send + Sync>, sg: Option<Arc<dyn SteamGiftsHandler>>) -> std::io::Result<Listener> {
        let sock = TcpListener::bind(("127.0.0.1", 0))?;
        let port = sock.local_addr()?.port();
        let token = new_token();
        let stop = Arc::new(AtomicBool::new(false));
        let (t, s) = (token.clone(), stop.clone());
        std::thread::Builder::new().name("zest-companion".into()).spawn(move || {
            for conn in sock.incoming() {
                if s.load(Ordering::Relaxed) { break; }
                let Ok(conn) = conn else { continue };
                // One request at a time is plenty (one userscript, a few posts per run); a stalled client is cut off by the timeouts.
                handle(conn, port, &t, &registry, &*on_event, sg.as_deref());
            }
        })?;
        Ok(Listener { port, token, stop })
    }
    /// The address to open in the browser for `service`, or None for an unknown one.
    pub fn arm_url(&self, service: &str) -> Option<String> {
        SERVICES.iter().find(|(s, url)| *s == service && !url.is_empty()).map(|(s, url)| format!("{url}#zest={}.{}.{s}", self.port, self.token))
    }
    /// The address to open for a service whose first page comes from the source (`base` = its `auth_begin` address).
    pub fn arm_url_for(&self, service: &str, base: &str) -> Option<String> {
        SERVICES.iter().find(|(s, url)| *s == service && url.is_empty()).map(|(s, _)| format!("{}#zest={}.{}.{s}", base.split('#').next().unwrap_or(base), self.port, self.token))
    }
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(("127.0.0.1", self.port)); // wake the accept
    }
}
impl Drop for Listener { fn drop(&mut self) { self.stop(); } }

/// 24 URL-safe characters from the OS-seeded hasher keys (`RandomState` is seeded from the system's random source).
fn new_token() -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut out = String::new();
    for i in 0..24u64 { let mut h = RandomState::new().build_hasher(); h.write_u64(i); out.push(A[(h.finish() % A.len() as u64) as usize] as char); }
    out
}

fn reply(mut c: &TcpStream, status: u16, body: &Value) {
    let text = match status { 200 => "OK", 400 => "Bad Request", 401 => "Unauthorized", 403 => "Forbidden", 404 => "Not Found", 411 => "Length Required", 413 => "Payload Too Large", _ => "Error" };
    let b = body.to_string();
    let _ = write!(c, "HTTP/1.1 {status} {text}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n{b}", b.len());
}
fn fail(c: &TcpStream, status: u16, msg: &str) { reply(c, status, &json!({"ok": false, "message": msg})); }

struct Req { method: String, path: String, host: String, token: String, body: Vec<u8> }

/// Read one request. Err carries the status and message to send back.
fn read_request(c: &mut TcpStream) -> Result<Req, (u16, &'static str)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") { break p; }
        if buf.len() > MAX_HEADER { return Err((400, "headers too large")); }
        let n = c.read(&mut chunk).map_err(|_| (400, "could not read the request"))?;
        if n == 0 { return Err((400, "incomplete request")); }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or("").split(' ');
    let (method, path) = (first.next().unwrap_or("").to_string(), first.next().unwrap_or("").to_string());
    let (mut host, mut token, mut len) = (String::new(), String::new(), None::<usize>);
    for l in lines {
        let Some((k, v)) = l.split_once(':') else { continue };
        match k.trim().to_ascii_lowercase().as_str() {
            "host" => host = v.trim().to_string(),
            "x-zest-token" => token = v.trim().to_string(),
            "content-length" => len = v.trim().parse().ok(),
            _ => {}
        }
    }
    let mut body = buf[head_end + 4..].to_vec();
    if method == "POST" {
        let len = len.ok_or((411, "Content-Length required"))?;
        if len > MAX_BODY { return Err((413, "body too large")); }
        while body.len() < len {
            let n = c.read(&mut chunk).map_err(|_| (400, "could not read the body"))?;
            if n == 0 { return Err((400, "incomplete body")); }
            body.extend_from_slice(&chunk[..n]);
        }
        body.truncate(len);
    }
    Ok(Req { method, path, host, token, body })
}

fn same(a: &str, b: &str) -> bool { a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0 }

fn handle(mut c: TcpStream, port: u16, token: &str, registry: &Registry, on_event: &(dyn Fn(Event) + Send + Sync), sg: Option<&dyn SteamGiftsHandler>) {
    let _ = c.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = c.set_write_timeout(Some(Duration::from_secs(10)));
    let req = match read_request(&mut c) { Ok(r) => r, Err((s, m)) => return fail(&c, s, m) };
    if req.host != format!("127.0.0.1:{port}") && req.host != format!("localhost:{port}") { return fail(&c, 403, "unexpected Host"); }
    if !same(&req.token, token) { return fail(&c, 401, "bad token"); }
    let json_body = || serde_json::from_slice::<Value>(&req.body).map_err(|_| ());
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/zest/hello") => reply(&c, 200, &json!({"ok": true, "app": "zest"})),
        ("POST", "/zest/capture") => {
            let Ok(b) = json_body() else { return fail(&c, 400, "not JSON") };
            let s = |k: &str| b.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
            // What each service sends, and which source's sign-in takes it. The value goes straight to `auth_complete`.
            let (platform, value) = match s("service").as_str() {
                "humble" => ("humble", s("session_cookie")),
                "nintendo" => ("nintendo", s("cookie")),
                "playstation" => ("playstation", s("npsso")),
                "xbox" => ("xbox", s("address")),
                "amazon" => ("amazon_games", s("address")),
                "gog" => ("gog", s("address")),
                "itch" => ("itch_io", s("api_key")),
                _ => return fail(&c, 400, "unknown service"),
            };
            if value.is_empty() { return fail(&c, 400, "nothing captured"); }
            let Some(src) = registry.get(platform) else { return fail(&c, 400, "that source is not available") };
            match src.auth_complete(&value) {
                Ok(st) => {
                    let who = st.username.clone();
                    on_event(Event::Connected { platform: platform.into(), username: who.clone() });
                    reply(&c, 200, &json!({"ok": true, "message": format!("Connected{}. You can close this tab.", who.map(|w| format!(" as {w}")).unwrap_or_default())}));
                }
                Err(e) => { on_event(Event::Refused { platform: platform.into() }); fail(&c, 200, &e) }
            }
        }
        ("POST", "/zest/plan") => {
            // Which order keys the browser should bother reading; the source decides, using what the library already holds.
            let Ok(b) = json_body() else { return fail(&c, 400, "not JSON") };
            let platform = match b.get("service").and_then(Value::as_str).unwrap_or("") { "humble" => "humble", _ => return fail(&c, 400, "unknown service") };
            let Some(src) = registry.get(platform) else { return fail(&c, 400, "that source is not available") };
            let keys: Vec<String> = b.get("keys").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect()).unwrap_or_default();
            let read = src.import_plan(&keys).unwrap_or_else(|| keys.clone());
            reply(&c, 200, &json!({"ok": true, "read": read}));
        }
        ("POST", "/zest/import") => {
            // For sources whose login can't be captured as a cookie: the browser reads the data and posts it, the source turns it into records.
            let Ok(b) = json_body() else { return fail(&c, 400, "not JSON") };
            let platform = match b.get("service").and_then(Value::as_str).unwrap_or("") { "humble" => "humble", "nintendo" => "nintendo", "indiegala" => "indiegala", _ => return fail(&c, 400, "unknown service") };
            let Some(src) = registry.get(platform) else { return fail(&c, 400, "that source is not available") };
            match src.import_pushed(b.get("data").unwrap_or(&Value::Null)) {
                Ok(records) => {
                    let n = records.len();
                    on_event(Event::Imported { platform: platform.into(), records });
                    reply(&c, 200, &json!({"ok": true, "count": n, "message": format!("Received {n} game{}. You can close this tab.", if n == 1 { "" } else { "s" })}));
                }
                Err(e) => { on_event(Event::Refused { platform: platform.into() }); fail(&c, 200, &e) }
            }
        }
        ("POST", "/zest/purchase-dates") => {
            let Ok(b) = json_body() else { return fail(&c, 400, "not JSON") };
            let service = b.get("service").and_then(Value::as_str).unwrap_or("").to_string();
            if service != "playstation" { return fail(&c, 400, "unknown service"); }
            let Some(src) = registry.get("playstation") else { return fail(&c, 400, "that source is not available") };
            let text = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            let purchases: Vec<(String, String, String)> = b.get("purchases").and_then(Value::as_array).map(|a| a.iter()
                .map(|p| (text(p, "date"), text(p, "name"), text(p, "sku")))
                .filter(|p| !p.0.is_empty() && !p.2.is_empty()).collect()).unwrap_or_default();
            let online_id = text(&b, "online_id");
            match src.apply_purchase_dates(&online_id, &purchases) {
                Ok((read, matched, set)) => {
                    on_event(Event::PurchaseDates { service, online_id, read, matched, set });
                    reply(&c, 200, &json!({"ok": true, "message": format!("Read {read} purchase{}, matched {matched} game{}, set {set} date{}. You can close this tab.", if read == 1 { "" } else { "s" }, if matched == 1 { "" } else { "s" }, if set == 1 { "" } else { "s" })}));
                }
                Err(e) => { on_event(Event::Refused { platform: "playstation".into() }); fail(&c, 200, &e) }
            }
        }
        (m, p) if p.starts_with("/zest/steam/") && (m == "POST" || m == "GET") => {
            let Some(h) = sg else { return fail(&c, 400, "the Steam date import is not available") };
            let body = if m == "POST" { match json_body() { Ok(b) => b, Err(_) => return fail(&c, 400, "not JSON") } } else { Value::Null };
            match h.call(&format!("steam/{}", &p["/zest/steam/".len()..]), &body) {
                Ok(mut v) => { if let Some(o) = v.as_object_mut() { o.insert("ok".into(), json!(true)); } reply(&c, 200, &v) }
                Err(e) => fail(&c, 200, &e),
            }
        }
        (m, p) if p.starts_with("/zest/sg/") && (m == "POST" || m == "GET") => {
            let Some(h) = sg else { return fail(&c, 400, "the SteamGifts import is not available") };
            let body = if m == "POST" { match json_body() { Ok(b) => b, Err(_) => return fail(&c, 400, "not JSON") } } else { Value::Null };
            match h.call(&p["/zest/sg/".len()..], &body) {
                Ok(mut v) => { if let Some(o) = v.as_object_mut() { o.insert("ok".into(), json!(true)); } reply(&c, 200, &v) }
                Err(e) => fail(&c, 200, &e),
            }
        }
        _ => fail(&c, 404, "no such route"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::{AuthStatus, Caps, LibrarySource};
    use std::sync::Mutex;

    struct Fake { seen: Mutex<Vec<String>> }
    impl LibrarySource for Fake {
        fn id(&self) -> &str { "humble" }
        fn label(&self) -> &str { "Fake Humble" }
        fn caps(&self) -> Caps { Caps { auth: true, ..Default::default() } }
        fn import_plan(&self, keys: &[String]) -> Option<Vec<String>> { Some(keys.iter().filter(|k| k.starts_with('n')).cloned().collect()) }
        fn import_pushed(&self, d: &Value) -> Result<Vec<crate::importer::GameRecord>, String> {
            let n = d.get("names").and_then(Value::as_array).ok_or("No names were sent")?;
            Ok(n.iter().filter_map(Value::as_str).map(|n| crate::importer::GameRecord { platform_id: n.into(), name: n.into(), ..Default::default() }).collect())
        }
        fn auth_complete(&self, v: &str) -> Result<AuthStatus, String> {
            self.seen.lock().unwrap().push(v.to_string());
            if v == "good" { Ok(AuthStatus { connected: true, username: Some("rob".into()) }) } else { Err("Session cookie is invalid or expired.".into()) }
        }
    }
    fn setup() -> (Listener, Arc<Fake>, Arc<Mutex<Vec<Event>>>) {
        let fake = Arc::new(Fake { seen: Mutex::new(vec![]) });
        let mut r = Registry::empty(); r.register(fake.clone()).unwrap();
        let events = Arc::new(Mutex::new(vec![]));
        let e2 = events.clone();
        let l = Listener::start(Arc::new(r), Arc::new(move |e| e2.lock().unwrap().push(e))).unwrap();
        (l, fake, events)
    }
    fn http(l: &Listener, method: &str, path: &str, host: Option<&str>, token: Option<&str>, body: &str) -> (u16, Value) {
        let mut c = TcpStream::connect(("127.0.0.1", l.port)).unwrap();
        let host = host.map(String::from).unwrap_or(format!("127.0.0.1:{}", l.port));
        let tok = token.map(|t| format!("X-Zest-Token: {t}\r\n")).unwrap_or_default();
        write!(c, "{method} {path} HTTP/1.1\r\nHost: {host}\r\n{tok}Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        let mut out = String::new(); c.read_to_string(&mut out).unwrap();
        let status = out.split(' ').nth(1).unwrap().parse().unwrap();
        (status, serde_json::from_str(out.split("\r\n\r\n").nth(1).unwrap()).unwrap())
    }

    #[test] fn arm_urls_carry_the_port_token_and_service_the_userscript_parses() {
        let (l, _, _) = setup();
        let u = l.arm_url("humble").unwrap();
        assert_eq!(u, format!("https://www.humblebundle.com/#zest={}.{}.humble", l.port, l.token));
        assert!(l.token.len() == 24 && l.token.chars().all(|c| c.is_ascii_alphanumeric()));
        assert!(l.arm_url("steam").is_none());
        for (s, url) in SERVICES {
            let u = if url.is_empty() { assert!(l.arm_url(s).is_none()); l.arm_url_for(s, "https://login.example.com/start?x=1#old").unwrap() } else { assert!(l.arm_url_for(s, "https://x/").is_none()); l.arm_url(s).unwrap() };
            assert!(u.ends_with(&format!("#zest={}.{}.{s}", l.port, l.token)), "{u}");
            // every service belongs to a source, except the SteamGifts import (not a library source: it has its own routes)
            assert!(platform_of(s).is_some() || s.starts_with("steamgifts") || *s == "steam-dates", "{s} has no source");
        }
        assert!(l.arm_url_for("xbox", "https://login.example.com/start?x=1#old").unwrap().starts_with("https://login.example.com/start?x=1#zest="));
        // every button the Plugins dialog can show names a service the userscript knows
        for p in ["humble", "nintendo", "playstation", "indiegala", "itch_io", "xbox", "amazon_games", "gog"] { for (s, _) in actions(p) { assert!(SERVICES.iter().any(|(x, _)| x == s) && platform_of(s) == Some(p), "{p} {s}"); } }
    }
    #[test] fn a_capture_goes_to_the_sources_sign_in_and_the_secret_is_never_echoed() {
        let (l, fake, ev) = setup();
        let (st, b) = http(&l, "POST", "/zest/capture", None, Some(&l.token), r#"{"service":"humble","session_cookie":"good"}"#);
        assert_eq!(st, 200); assert_eq!(b["ok"], true); assert!(b["message"].as_str().unwrap().contains("rob"));
        assert!(!b.to_string().contains("good\""), "reply must not echo the secret");
        assert_eq!(*fake.seen.lock().unwrap(), vec!["good".to_string()]);
        assert_eq!(ev.lock().unwrap()[0], Event::Connected { platform: "humble".into(), username: Some("rob".into()) });
    }
    #[test] fn a_refused_value_comes_back_as_not_ok_with_the_sources_reason() {
        let (l, _, ev) = setup();
        let (st, b) = http(&l, "POST", "/zest/capture", None, Some(&l.token), r#"{"service":"humble","session_cookie":"stale"}"#);
        assert_eq!((st, b["ok"].clone()), (200, json!(false)));
        assert!(b["message"].as_str().unwrap().contains("expired"));
        assert_eq!(ev.lock().unwrap()[0], Event::Refused { platform: "humble".into() });
    }
    #[test] fn wrong_token_wrong_host_and_bad_input_are_refused_before_any_source_runs() {
        let (l, fake, _) = setup();
        let body = r#"{"service":"humble","session_cookie":"good"}"#;
        assert_eq!(http(&l, "POST", "/zest/capture", None, None, body).0, 401);
        assert_eq!(http(&l, "POST", "/zest/capture", None, Some("nope"), body).0, 401);
        assert_eq!(http(&l, "POST", "/zest/capture", Some("evil.example"), Some(&l.token), body).0, 403);
        assert_eq!(http(&l, "POST", "/zest/capture", None, Some(&l.token), "not json").0, 400);
        assert_eq!(http(&l, "POST", "/zest/capture", None, Some(&l.token), r#"{"service":"steam","x":"y"}"#).0, 400);
        assert_eq!(http(&l, "POST", "/zest/capture", None, Some(&l.token), r#"{"service":"humble","session_cookie":" "}"#).0, 400);
        assert_eq!(http(&l, "POST", "/zest/capture", None, Some(&l.token), r#"{"service":"nintendo","cookie":"a=b"}"#).0, 400, "no nintendo source registered");
        assert_eq!(http(&l, "GET", "/zest/nope", None, Some(&l.token), "").0, 404);
        assert!(fake.seen.lock().unwrap().is_empty());
    }
    #[test] fn browser_data_becomes_records_through_the_source_and_needs_the_token() {
        let (l, _, ev) = setup();
        let body = r#"{"service":"humble","data":{"names":["a","b"]}}"#;
        assert_eq!(http(&l, "POST", "/zest/import", None, None, body).0, 401);
        let (st, b) = http(&l, "POST", "/zest/import", None, Some(&l.token), body);
        assert_eq!(st, 200); assert!(b["message"].as_str().unwrap().starts_with("Received 2 games.")); assert_eq!(b["count"], 2);
        match &ev.lock().unwrap()[0] { Event::Imported { platform, records } => { assert_eq!(platform, "humble"); assert_eq!(records.len(), 2); } e => panic!("{e:?}") }
        let (st, b) = http(&l, "POST", "/zest/import", None, Some(&l.token), r#"{"service":"humble","data":{}}"#);
        assert_eq!((st, b["ok"].clone()), (200, json!(false)));
        assert_eq!(http(&l, "POST", "/zest/import", None, Some(&l.token), r#"{"service":"steam","data":{}}"#).0, 400);
    }
    #[test] fn the_plan_route_returns_what_the_source_wants_read() {
        let (l, _, _) = setup();
        let (st, b) = http(&l, "POST", "/zest/plan", None, Some(&l.token), r#"{"service":"humble","keys":["new1","old1","new2",5]}"#);
        assert_eq!(st, 200); assert_eq!(b["read"], json!(["new1", "new2"]));
        assert_eq!(http(&l, "POST", "/zest/plan", None, None, "{}").0, 401);
        assert_eq!(http(&l, "POST", "/zest/plan", None, Some(&l.token), r#"{"service":"steam","keys":[]}"#).0, 400);
    }
    #[test] fn hello_answers_with_a_good_token() {
        let (l, _, _) = setup();
        assert_eq!(http(&l, "GET", "/zest/hello", None, Some(&l.token), "").1["app"], "zest");
    }
    #[test] fn purchases_are_handed_to_the_source_with_incomplete_rows_dropped_and_its_answer_reported() {
        struct Psn(Mutex<Vec<(String, Vec<(String, String, String)>)>>);
        impl LibrarySource for Psn {
            fn id(&self) -> &str { "playstation" } fn label(&self) -> &str { "PSN" } fn caps(&self) -> Caps { Caps::default() }
            fn apply_purchase_dates(&self, o: &str, p: &[(String, String, String)]) -> Result<(usize, usize, usize), String> {
                self.0.lock().unwrap().push((o.to_string(), p.to_vec()));
                if o == "other" { Err("This browser is signed in as other".into()) } else { Ok((p.len(), 1, 1)) }
            }
        }
        let psn = Arc::new(Psn(Mutex::new(vec![])));
        let mut r = Registry::empty(); r.register(psn.clone()).unwrap();
        let events = Arc::new(Mutex::new(vec![])); let e2 = events.clone();
        let l = Listener::start(Arc::new(r), Arc::new(move |e| e2.lock().unwrap().push(e))).unwrap();
        let body = r#"{"service":"playstation","online_id":"rob","purchases":[{"date":"2020-01-02T00:00:00Z","name":"A","sku":"S1"},{"date":"","name":"B","sku":"S2"},{"date":"2021-01-01T00:00:00Z","name":"C","sku":""}]}"#;
        let (st, b) = http(&l, "POST", "/zest/purchase-dates", None, Some(&l.token), body);
        assert_eq!(st, 200); assert!(b["message"].as_str().unwrap().starts_with("Read 1 purchase, matched 1 game, set 1 date."), "{b}");
        assert_eq!(psn.0.lock().unwrap()[0], ("rob".to_string(), vec![("2020-01-02T00:00:00Z".to_string(), "A".to_string(), "S1".to_string())]));
        assert_eq!(events.lock().unwrap()[0], Event::PurchaseDates { service: "playstation".into(), online_id: "rob".into(), read: 1, matched: 1, set: 1 });
        // a different account is refused with the source's own message
        let (_, b) = http(&l, "POST", "/zest/purchase-dates", None, Some(&l.token), r#"{"service":"playstation","online_id":"other","purchases":[{"date":"2020-01-02T00:00:00Z","name":"A","sku":"S1"}]}"#);
        assert_eq!(b["ok"], false); assert!(b["message"].as_str().unwrap().contains("signed in as other"));
        assert_eq!(events.lock().unwrap()[1], Event::Refused { platform: "playstation".into() });
    }
    #[test] fn an_oversized_body_is_refused_without_reading_it() {
        let (l, _, _) = setup();
        let mut c = TcpStream::connect(("127.0.0.1", l.port)).unwrap();
        write!(c, "POST /zest/capture HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-Zest-Token: {}\r\nContent-Length: {}\r\n\r\n", l.port, l.token, MAX_BODY + 1).unwrap();
        let mut out = String::new(); c.read_to_string(&mut out).unwrap();
        assert!(out.starts_with("HTTP/1.1 413"));
    }
    #[test] fn the_steam_date_routes_reach_the_handler_with_the_token_and_arm_with_a_base_address() {
        struct H; impl SteamGiftsHandler for H { fn call(&self, route: &str, _b: &Value) -> Result<Value, String> { if route == "steam/plan" { Ok(json!({"items": []})) } else { Err("no".into()) } } }
        let l = Listener::start_with(Arc::new(Registry::builtin()), Arc::new(|_| {}), Some(Arc::new(H))).unwrap();
        let (st, b) = http(&l, "POST", "/zest/steam/plan", None, Some(&l.token), "{}"); assert_eq!(st, 200); assert_eq!(b["ok"], true);
        assert_eq!(http(&l, "POST", "/zest/steam/plan", None, None, "{}").0, 401);
        let (st, b) = http(&l, "POST", "/zest/steam/other", None, Some(&l.token), "{}"); assert_eq!((st, b["ok"].clone()), (200, json!(false)));
        let u = l.arm_url_for("steam-dates", "https://help.steampowered.com/en/wizard/HelpWithGame/?appid=10").unwrap();
        assert!(u.starts_with("https://help.steampowered.com/en/wizard/HelpWithGame/?appid=10#zest=") && u.ends_with(".steam-dates"));
    }
    #[test] fn stop_ends_the_thread_and_frees_the_port() {
        let (l, _, _) = setup(); let p = l.port; l.stop();
        std::thread::sleep(Duration::from_millis(200));
        assert!(TcpListener::bind(("127.0.0.1", p)).is_ok());
    }
}
