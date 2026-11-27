//! EA App as a library source (port of PlayDate's `plugins/ea_app/ea.py`). Sign-in is a pasted access token (EA's token page, a redirect address or the bare token); the library is
//! the account's owned PC base games from EA's Juno GraphQL service, named with their store slug, content id (a game granted under two offer ids is imported once) and art,
//! with the date each was granted from the identity gateway. The token is an implicit-grant one: about four hours, no refresh, so an expired token means connecting again.
//! Install, launch and uninstall only open the EA App (as in PlayDate, which tried a native launch and removed it); install detection reads the Wine prefix's registry and
//! `installerdata.xml` files (`ea_manifest.rs`). The install watcher is not built: the scan runs at start and after a sync.
use super::{Action, AuthBegin, AuthStatus, Caps, GameRef, InstalledRow, Launch, LibrarySource, SourceCtx};
use crate::importer::GameRecord;
use crate::net::{log, Cancel, Http};
use regex::Regex;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

const IDENTITY_URL: &str = "https://gateway.ea.com/proxy/identity/pids/me";
const JUNO_API: &str = "https://service-aggregation-layer.juno.ea.com/graphql";
const TOKEN_PAGE_SIGNED_IN: &str = "https://accounts.ea.com/connect/auth?client_id=ORIGIN_JS_SDK&response_type=token&redirect_uri=nucleus:rest&prompt=none";
const TOKEN_PAGE: &str = "https://accounts.ea.com/connect/auth?client_id=ORIGIN_JS_SDK&response_type=token&redirect_uri=nucleus:rest";   // no prompt=none: a signed-out browser gets EA's sign-in page and then the token, not a login_required answer
const UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 QtWebEngine/5.8.0";

const ENTITLEMENTS_QUERY: &str = "query getEntitlements($limit: Int, $next: String) { me { ownedGameProducts(locale: \"DEFAULT\" entitlementEnabled: true storefronts: [EA] type: [DIGITAL_FULL_GAME, PACKAGED_FULL_GAME] platforms: [PC] paging: { limit: $limit, next: $next }) { next items { originOfferId product { baseItem { gameType } } } } } }";
const GAMES_QUERY: &str = "query getOffers($offerIds: [String!]!) { legacyOffers(offerIds: $offerIds, locale: \"DEFAULT\") { offerId: id contentId } gameProducts(offerIds: $offerIds, locale: \"DEFAULT\") { items { id originOfferId gameSlug baseItem { title keyArt { largestImage { path } } packArt { largestImage { path } } } } } }";

fn now() -> i64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0) }
fn text<'a>(v: &'a Value, k: &str) -> &'a str { v.get(k).and_then(|x| x.as_str()).unwrap_or("") }

// ---------------------------------------------------------------- pure parts (checked against the Python)

/// `_parse_token_input`: the access token and its life in seconds out of whatever was pasted (a bare token, EA's JSON, a redirect address or fragment).
pub fn parse_token_input(raw: &str) -> (Option<String>, Option<i64>) {
    static A: OnceLock<Regex> = OnceLock::new(); static E: OnceLock<Regex> = OnceLock::new(); static J: OnceLock<Regex> = OnceLock::new(); static B: OnceLock<Regex> = OnceLock::new(); static C: OnceLock<Regex> = OnceLock::new();
    let raw = raw.trim().trim_matches(|c| c == '"' || c == '\'').trim();
    if raw.starts_with('{') {
        if let Ok(o) = serde_json::from_str::<Value>(raw) {
            let tok = o.get("access_token").and_then(|t| t.as_str()).unwrap_or("");
            let exp = o.get("expires_in").and_then(|e| e.as_i64().or_else(|| e.as_str().and_then(|s| s.parse().ok())));
            return ((!tok.is_empty()).then(|| tok.to_string()), exp);
        }
    }
    if let Some(m) = A.get_or_init(|| Regex::new(r#"(?:^|[#?&\s"])access_token=([^&\s"']+)"#).unwrap()).captures(raw) {
        let exp = E.get_or_init(|| Regex::new(r"(?:^|[#?&\s])expires_in=(\d+)").unwrap()).captures(raw).and_then(|c| c[1].parse().ok());
        return (Some(m[1].to_string()), exp);
    }
    if let Some(m) = J.get_or_init(|| Regex::new(r#""access_token"\s*:\s*"([^"]+)""#).unwrap()).captures(raw) { return (Some(m[1].to_string()), None); }
    let has_ws = B.get_or_init(|| Regex::new(r"\s").unwrap()).is_match(raw);
    let has_sym = C.get_or_init(|| Regex::new(r"[={}:/?]").unwrap()).is_match(raw);
    if !raw.is_empty() && !has_ws && !has_sym { return (Some(raw.to_string()), None); }
    (None, None)
}

/// `_resolve_ea_store_slug`'s candidates: the short slug, then each franchise prefix (`need-for-speed/need-for-speed-heat`, `need-for/...`), longest first.
pub fn slug_candidates(short: &str) -> Vec<String> {
    if short.is_empty() || short.contains('/') { return vec![]; }
    let parts: Vec<&str> = short.split('-').collect();
    let mut v = vec![short.to_string()];
    for i in (1..parts.len()).rev() { v.push(format!("{}/{short}", parts[..i].join("-"))); }
    v
}

/// The offer ids of owned base games out of one page of the entitlements answer, and the cursor for the next page.
pub fn entitlement_page(data: &Value) -> (Vec<String>, Option<String>) {
    let products = &data["me"]["ownedGameProducts"];
    let ids = products["items"].as_array().map(|a| a.as_slice()).unwrap_or(&[]).iter().filter_map(|i| {
        let id = text(i, "originOfferId"); if id.is_empty() { return None; }
        (i["product"]["baseItem"]["gameType"].as_str() == Some("BASE_GAME")).then(|| id.to_string())
    }).collect();
    (ids, products["next"].as_str().filter(|n| !n.is_empty()).map(String::from))
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Detail { pub offer_id: String, pub content_id: String, pub name: String, pub slug: String, pub horiz_url: String, pub vert_url: String }

/// `_fetch_games_detail`'s joining of the two answers: each legacy offer with its product (by offer id, else content id, else offer id as product id).
pub fn details(data: &Value) -> Vec<Detail> {
    let items = data["gameProducts"]["items"].as_array().cloned().unwrap_or_default();
    let by = |key: &str, k: &str| items.iter().find(|p| !text(p, key).is_empty() && text(p, key) == k).cloned();
    let mut out = vec![];
    for lo in data["legacyOffers"].as_array().map(|a| a.as_slice()).unwrap_or(&[]) {
        if !lo.is_object() { continue; }
        let (offer, content) = (text(lo, "offerId"), text(lo, "contentId"));
        let Some(p) = by("originOfferId", offer).or_else(|| by("id", content)).or_else(|| by("id", offer)) else { continue };
        let base = &p["baseItem"];
        let img = |k: &str| base[k]["largestImage"]["path"].as_str().unwrap_or("").to_string();
        out.push(Detail { offer_id: offer.into(), content_id: content.into(), name: Some(text(base, "title").trim()).filter(|n| !n.is_empty()).unwrap_or(offer).into(), slug: text(&p, "gameSlug").trim().into(), horiz_url: img("keyArt"), vert_url: img("packArt") });
    }
    out
}

/// `_fetch_entitlement_dates`: the earliest grant time per offer among the ACTIVE entitlements.
pub fn entitlement_dates(body: &Value) -> std::collections::HashMap<String, i64> {
    let ents = match &body["entitlements"]["entitlement"] { Value::Array(a) => a.clone(), o @ Value::Object(_) => vec![o.clone()], _ => vec![] };
    let mut out = std::collections::HashMap::new();
    for e in ents {
        let (offer, granted) = (text(&e, "productId"), text(&e, "grantDate"));
        if offer.is_empty() || granted.is_empty() || text(&e, "status") != "ACTIVE" { continue; }
        // `%Y-%m-%dT%H:%MZ`; anything else is skipped, as the Python's strptime did
        static R: OnceLock<Regex> = OnceLock::new();
        let Some(c) = R.get_or_init(|| Regex::new(r"^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})Z$").unwrap()).captures(granted) else { continue };
        let n = |i: usize| c[i].parse::<i64>().unwrap_or(0);
        let Some(ts) = super::itch_io::utc_ts(n(1), n(2), n(3), n(4), n(5), 0) else { continue };
        out.entry(offer.to_string()).and_modify(|t: &mut i64| *t = (*t).min(ts)).or_insert(ts);
    }
    out
}

// ---------------------------------------------------------------- the source

/// `_find_wine_exe`: `EADesktop.exe` (or another program) inside the EA App's folder in the prefix: the non-versioned `EA Desktop/EA Desktop/` first, then any versioned folder except `compatibility32`.
pub fn find_in_ea_folder(prefix: &Path, exe: &str) -> Option<PathBuf> {
    let base = prefix.join("drive_c/Program Files/Electronic Arts/EA Desktop");
    if !base.is_dir() { return None; }
    let preferred = base.join("EA Desktop").join(exe);
    if preferred.is_file() { return Some(preferred); }
    fn walk(d: &Path, exe: &str) -> Option<PathBuf> {
        let mut subs: Vec<PathBuf> = vec![];
        let mut files = vec![];
        for e in std::fs::read_dir(d).ok()?.flatten() { let p = e.path(); if p.is_dir() { if !e.file_name().to_string_lossy().eq_ignore_ascii_case("compatibility32") { subs.push(p); } } else { files.push(e.file_name()); } }
        if files.iter().any(|f| f.to_string_lossy() == exe) { return Some(d.join(exe)); }
        subs.sort();
        subs.iter().find_map(|s| walk(s, exe))
    }
    walk(&base, exe)
}
fn wine_launcher(prefix: &Path) -> Option<PathBuf> { find_in_ea_folder(prefix, "EADesktop.exe") }
/// `_find_native_launcher`: the EA App on Windows, in its usual folders (the registry lookup PlayDate tried first is not used).
fn native_launcher() -> Option<PathBuf> {
    ["PROGRAMFILES", "PROGRAMFILES(X86)", "PROGRAMW6432", "LOCALAPPDATA"].iter().filter_map(|e| std::env::var_os(e)).map(PathBuf::from)
        .flat_map(|b| [b.join("Electronic Arts/EA Desktop/EA Desktop/EADesktop.exe"), b.join("Electronic Arts/EA Desktop/EADesktop.exe"), b.join("EA Desktop/EADesktop.exe")]).find(|p| p.is_file())
}
/// Whether the EA App itself is among the prefix's running programs (PlayDate asked whether its local service had bound port 3216).
pub fn app_running(procs: &[(u32, String)]) -> bool {
    procs.iter().any(|(_, prog)| { let l = prog.to_lowercase(); l.contains("eadesktop.exe") || l.contains("ealocalhostsvc") || l.contains("eabackgroundservice") })
}

pub struct EaSource { http: Arc<dyn Http>, config_dir: PathBuf, db: PathBuf }

impl EaSource {
    pub fn new(http: Arc<dyn Http>, config_dir: PathBuf, db: PathBuf) -> Self { EaSource { http, config_dir, db } }
    fn get(&self, key: &str) -> Option<String> { crate::account::plugin_secret_get(&self.config_dir, "ea_app", key).filter(|v| !v.is_empty()) }
    fn expires_at(&self) -> i64 {
        std::fs::read_to_string(self.config_dir.join("config.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v["ea_app"]["expires_at"].as_i64()).unwrap_or(0)
    }
    /// The token while it is good (no refresh exists for this kind), else None.
    fn token(&self) -> Option<String> { let t = self.get("access_token")?; (now() < self.expires_at() - 60).then_some(t) }
    fn headers<'a>(token: &'a str, auth: &'a [String; 3]) -> Vec<(&'a str, &'a str)> {
        let _ = token;
        vec![("Accept", "application/json"), ("X-Origin-Platform", "Win32"), ("Origin", "https://www.ea.com"), ("User-Agent", UA), ("Authorization", &auth[0]), ("AuthToken", &auth[1]), ("X-AuthToken", &auth[2])]
    }
    fn authed(token: &str) -> [String; 3] { [format!("Bearer {token}"), token.to_string(), token.to_string()] }
    fn juno(&self, token: &str, query: &str, vars: Value) -> Result<Value, String> {
        let a = Self::authed(token); let mut h = Self::headers(token, &a); h.push(("Content-Type", "application/json"));
        let r = self.http.post(JUNO_API, &h, &json!({"query": query, "variables": vars}).to_string(), Duration::from_secs(30)).map_err(|e| e.to_string())?;
        if r.status >= 400 { return Err(format!("EA answered HTTP {}", r.status)); }
        let v: Value = serde_json::from_str(&r.body).map_err(|_| "EA sent an answer Zest could not read".to_string())?;
        Ok(v.get("data").cloned().unwrap_or(Value::Null))
    }
    fn entitlements(&self, token: &str) -> Result<Vec<String>, String> {
        let (mut ids, mut next) = (vec![], None::<String>);
        loop {
            let data = self.juno(token, ENTITLEMENTS_QUERY, json!({"limit": 100, "next": next}))?;
            let (page, n) = entitlement_page(&data);
            ids.extend(page);
            match n { Some(n) => next = Some(n), None => break }
        }
        Ok(ids)
    }
    fn details_for(&self, token: &str, offers: &[String]) -> Vec<Detail> {
        let mut out = vec![];
        for chunk in offers.chunks(100) {
            match self.juno(token, GAMES_QUERY, json!({"offerIds": chunk})) { Ok(d) => out.extend(details(&d)), Err(e) => log(&format!("EA: game details failed for a chunk: {e}")) }
        }
        out
    }
    fn dates(&self, token: &str, pid: &str) -> std::collections::HashMap<String, i64> {
        if pid.is_empty() { return Default::default(); }
        let a = Self::authed(token); let mut h = Self::headers(token, &a); h.push(("X-Expand-Results", "true"));
        match self.http.get(&format!("https://gateway.ea.com/proxy/identity/pids/{pid}/entitlements"), &h, Duration::from_secs(40)) {
            Ok(r) if r.status < 400 => serde_json::from_str::<Value>(&r.body).map(|v| entitlement_dates(&v)).unwrap_or_default(),
            _ => { log("EA: the entitlement dates could not be read"); Default::default() }
        }
    }
    /// The slug as ea.com wants it: the short one when its page exists, else the first franchise-prefixed one that does.
    fn resolve_slug(&self, short: &str) -> String {
        for c in slug_candidates(short) {
            if let Ok((r, _)) = self.http.get_final(&format!("https://www.ea.com/games/{c}"), &[("User-Agent", UA)], Duration::from_secs(8)) { if r.status == 200 { return c; } }
        }
        short.to_string()
    }
    fn prefix(&self) -> Option<PathBuf> { super::rockstar::prefix_setting(&self.config_dir, "ea_app") }
    /// `_open_ea_app`: start the EA App (natively on Windows, in its Wine prefix on Linux) and tell the user to finish there. An app that is already open is not started again.
    fn open_app(&self, verb: &str, name: &str) -> Result<Action, String> {
        let say = format!("EA App: {} \"{name}\" there.", verb.to_lowercase());
        if cfg!(windows) {
            let exe = native_launcher().ok_or("EA App is not installed")?;
            return Ok(Action::Many(vec![Action::Message(say), Action::Spawn { program: exe.to_string_lossy().to_string(), args: vec![], dir: None, env: vec![] }]));
        }
        if cfg!(target_os = "macos") { return Err("The EA App has no macOS version".into()); }
        let prefix = self.prefix().ok_or("EA App not configured. Set up its Wine prefix in the Plugins dialog.")?;
        let exe = wine_launcher(&prefix).ok_or("EA App not installed in the Wine prefix")?;
        if app_running(&crate::wine::prefix_processes(&prefix)) { return Ok(Action::Message(format!("EA App is already open: {} \"{name}\" there.", verb.to_lowercase()))); }
        // winegstreamer off (it aborts the app's web view), DXVK settings for its GPU process; the app runs directly, not in a virtual desktop
        let mut env: Vec<(String, String)> = [("WINEDEBUG", "-all"), ("WINEDLLOVERRIDES", "winegstreamer="), ("DXVK_ASYNC", "1"), ("DXVK_LOG_LEVEL", "none"), ("WINEFSYNC", "1"), ("WINEESYNC", "1")].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let icd = "/usr/share/vulkan/icd.d/nvidia_icd.x86_64.json";
        if Path::new(icd).exists() { env.push(("VK_ICD_FILENAMES".into(), icd.into())); }
        let spawn = crate::wine::launch_action_full(&exe, &prefix, &["-ls=Launcher".to_string()], None, &env)?;
        Ok(Action::Many(vec![Action::Message(say), spawn]))
    }
    fn existing(&self) -> (HashSet<String>, HashSet<String>, HashSet<String>, HashSet<String>) {
        let (mut offers, mut content, mut slugs, mut black) = (HashSet::new(), HashSet::new(), HashSet::new(), HashSet::new());
        if let Ok(c) = rusqlite::Connection::open_with_flags(&self.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) {
            if let Ok(mut st) = c.prepare("SELECT COALESCE(platform_id,''), COALESCE(platform_appname,''), COALESCE(platform_slug,'') FROM games WHERE platform = 'ea_app'") {
                for (o, a, s) in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))).into_iter().flatten().flatten() {
                    offers.insert(o); if !a.is_empty() { content.insert(a); } if !s.is_empty() { slugs.insert(s); }
                }
            }
            if let Ok(mut st) = c.prepare("SELECT platform_id FROM blacklist WHERE platform_id IS NOT NULL") { for b in st.query_map([], |r| r.get::<_, String>(0)).into_iter().flatten().flatten() { black.insert(b); } }
        }
        (offers, content, slugs, black)
    }
}

impl LibrarySource for EaSource {
    fn id(&self) -> &str { "ea_app" }
    fn label(&self) -> &str { "EA App" }
    fn caps(&self) -> Caps { Caps { auth: true, sync: true, art: true, store_page: true, purchase_dates: true, launch: true, uninstall: true, installed_scan: true, ..Default::default() } }
    fn purchase_dates(&self, cx: &SourceCtx, appids: &[i64]) -> Result<Vec<(i64, Option<i64>)>, String> {
        let token = self.token().ok_or("Not connected to EA: please connect again")?;
        let dates = self.dates(&token, &self.get("pid").unwrap_or_default());
        if dates.is_empty() { return Err("Could not read your EA entitlements. Reconnect EA and try again.".into()); }
        Ok(super::platform_rows(&cx.db, appids).into_iter().map(|(id, pid, _)| (id, dates.get(&pid).copied())).collect())
    }

    fn auth_status(&self) -> Result<AuthStatus, String> {
        let ok = self.token().is_some();
        Ok(AuthStatus { connected: ok, username: ok.then(|| self.get("display_name").unwrap_or_else(|| "EA Account".into())) })
    }
    /// The sign-in window opens ea.com's own login (what Playnite's EA plugin does; the token page's login is the one EA's bot check refused in an embedded browser). Once signed in,
    /// the window hands back accounts.ea.com's cookies and `auth_complete` asks the token page for the token with them.
    fn paste_login_kept(&self) -> bool { false }   // the sign-in window works (confirmed live 2026-11-22)
    fn web_login_url(&self) -> Option<String> { Some("https://www.ea.com/login".into()) }
    fn web_login_done(&self) -> Option<serde_json::Value> {
        // part one: ea.com's own sign-in ends on its home page (its session cookie appears); part two: the same window asks the token page, which answers with the token as text
        Some(serde_json::json!({"kind": "cookies", "url": "https://accounts.ea.com/", "names": ["PIM-SESSION-ID"], "url_not_contains": "signin.ea.com",
            "then": {"url": TOKEN_PAGE_SIGNED_IN, "done": {"kind": "body", "url_contains": "accounts.ea.com/connect/auth", "text_contains": "access_token"}}}))
    }
    fn auth_begin(&self) -> Result<AuthBegin, String> {
        Ok(AuthBegin { url: TOKEN_PAGE.into(), placeholder: "Paste the access_token (or the whole page text)".into(), instructions: vec![
            "Sign in at ea.com in your browser first.".into(),
            "The page that opens shows a short piece of text with an access_token in it. Copy all of it (or just the token).".into(),
            "Paste it below. The token lasts about four hours; connect again when it runs out.".into()] })
    }
    fn auth_complete(&self, value: &str) -> Result<AuthStatus, String> {
        let (tok, exp) = parse_token_input(value);
        let Some(tok) = tok else {
            return Err(if value.contains("login_required") { "EA says you are not signed in. Sign in at ea.com in the same browser first, then open the token page again".into() } else { "Could not extract an access token from the input".into() });
        };
        let mut life = exp.unwrap_or(3600);
        // EA reports the token's true remaining life; a pasted token carries none
        if let Ok(r) = self.http.get(&format!("https://accounts.ea.com/connect/tokeninfo?access_token={}", crate::net::enc(&tok)), &[], Duration::from_secs(10)) {
            if r.status == 200 { if let Some(e) = serde_json::from_str::<Value>(&r.body).ok().and_then(|v| v["expires_in"].as_i64()).filter(|e| *e != 0) { life = e; } }
        }
        let a = Self::authed(&tok);
        let mut h = Self::headers(&tok, &a); h.push(("X-Expand-Results", "true"));
        let r = self.http.get(IDENTITY_URL, &h, Duration::from_secs(10)).map_err(|_| "Could not reach EA to verify the token. Check your connection and try again".to_string())?;
        if matches!(r.status, 400 | 401 | 403) { return Err("EA rejected that token. Make sure you are signed in to ea.com, open the token page again and paste the access_token it shows".into()); }
        let profile = if r.status == 200 { serde_json::from_str::<Value>(&r.body).ok().map(|v| v["pid"].clone()).unwrap_or(Value::Null) } else { Value::Null };
        let pid = match &profile["pidId"] { Value::String(s) => s.clone(), Value::Number(n) => n.to_string(), _ => String::new() };
        if pid.is_empty() { return Err("EA did not recognise that token. Paste the access_token value from the token page".into()); }
        let mut name = text(&profile, "displayName").to_string();
        if name.is_empty() {
            // the identity answer omits it: the first persona has it
            let get = |u: &str| self.http.get(u, &Self::headers(&tok, &a), Duration::from_secs(10)).ok().filter(|r| r.status == 200).and_then(|r| serde_json::from_str::<Value>(&r.body).ok());
            name = get(&format!("https://gateway.ea.com/proxy/identity/pids/{pid}/personas")).and_then(|v| v["personas"]["personaUri"][0].as_str().map(String::from))
                .and_then(|uri| get(&format!("https://gateway.ea.com/proxy/identity{uri}"))).and_then(|v| v["persona"]["displayName"].as_str().map(String::from)).unwrap_or_default();
        }
        if name.is_empty() { name = pid.clone(); }
        let set = |k: &str, v: &str| crate::account::plugin_secret_set(&self.config_dir, "ea_app", k, Some(v)).map_err(|_| "Could not save the sign-in".to_string());
        set("access_token", &tok)?; set("refresh_token", "")?; set("display_name", &name)?; set("pid", &pid)?;
        crate::account::plugin_number_set(&self.config_dir, "ea_app", "expires_at", now() + life).map_err(|_| "Could not save the sign-in".to_string())?;
        log("EA: connected");
        self.auth_status()
    }
    fn auth_disconnect(&self) -> Result<(), String> { crate::account::plugin_remove(&self.config_dir, "ea_app").map_err(|_| "Could not save".to_string()) }

    fn sync(&self, _cx: &SourceCtx, sink: &mut dyn FnMut(Vec<GameRecord>), cancel: &Cancel) -> Result<usize, String> {
        let token = self.token().ok_or("Not connected to EA: please connect again")?;
        let offers = self.entitlements(&token).map_err(|e| format!("Failed to fetch EA library: {e}"))?;
        log(&format!("EA sync: {} owned game offer ids", offers.len()));
        let dates = self.dates(&token, &self.get("pid").unwrap_or_default());
        let (known, mut seen_content, mut seen_slugs, black) = self.existing();
        let new: Vec<String> = offers.iter().filter(|o| !known.contains(*o) && !black.contains(*o)).cloned().collect();
        let detail = self.details_for(&token, &new);
        let mut records = vec![];
        for offer in &new {
            if cancel.is_set() { break; }
            let d = detail.iter().find(|d| &d.offer_id == offer).cloned().unwrap_or_default();
            // EA grants one game under two offer ids (a legacy SKU and a reissue): the second is skipped
            if (!d.content_id.is_empty() && seen_content.contains(&d.content_id)) || (d.content_id.is_empty() && !d.slug.is_empty() && seen_slugs.contains(&d.slug)) { continue; }
            if !d.content_id.is_empty() { seen_content.insert(d.content_id.clone()); }
            if !d.slug.is_empty() { seen_slugs.insert(d.slug.clone()); }
            let slug = if d.slug.is_empty() { None } else { Some(self.resolve_slug(&d.slug)) };
            records.push(GameRecord { platform_id: offer.clone(), name: if d.name.is_empty() { offer.clone() } else { d.name.clone() }, slug, appname: Some(d.content_id.clone()).filter(|c| !c.is_empty()), date_added: dates.get(offer).copied(), ..Default::default() });
        }
        let n = records.len();
        log(&format!("EA sync: {n} new games"));
        sink(records);
        Ok(n)
    }

    fn art_urls(&self, g: &GameRef) -> Vec<(String, String)> {
        let Some(token) = self.token() else { return vec![] };
        if g.platform_id.is_empty() { return vec![]; }
        let mut v = vec![];
        if let Some(d) = self.details_for(&token, std::slice::from_ref(&g.platform_id)).into_iter().next() {
            if !d.vert_url.is_empty() { v.push(("vertical".to_string(), d.vert_url)); }
            if !d.horiz_url.is_empty() { v.push(("horizontal".to_string(), d.horiz_url)); }
        }
        v
    }
    fn store_url(&self, g: &GameRef) -> Option<String> { (!g.slug.is_empty()).then(|| format!("https://www.ea.com/games/{}", g.slug)) }

    fn installed_scan(&self) -> Option<Vec<InstalledRow>> {
        let c = rusqlite::Connection::open_with_flags(&self.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
        let mut st = c.prepare("SELECT platform_id, COALESCE(name,''), COALESCE(install_path,''), COALESCE(platform_executable,''), COALESCE(platform_appname,''), COALESCE(installed,0) FROM games WHERE platform = 'ea_app' AND platform_id IS NOT NULL").ok()?;
        let rows: Vec<(String, String, String, String, String, i64)> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))).ok()?.flatten().collect();
        let prefix = self.prefix();
        let reg = prefix.as_deref().map(super::ea_manifest::load_registry);
        let mut out = vec![];
        for (id, name, path, exe, content_id, installed) in rows {
            if !path.is_empty() {
                // a recorded folder: still there, or EA's Cleanup.exe gutted it (the empty shell it leaves does not count); a gone one also forgets its path so a reinstall elsewhere is found
                if super::ea_manifest::install_present(&path, Some(&exe)) { out.push(InstalledRow { platform_id: id, installed: true, install_path: None }); }
                else { out.push(InstalledRow { platform_id: id, installed: false, install_path: Some(String::new()) }); }
            } else if let (Some(p), Some(reg), 0) = (&prefix, &reg, installed) {
                // no folder on record: the exact content id first, then the manifest title
                let found = (!content_id.is_empty()).then(|| super::ea_manifest::find_by_content_id(p, reg, &content_id).map(|m| m.0)).flatten()
                    .or_else(|| super::ea_manifest::find_by_name(p, reg, &name));
                if let Some(dir) = found { crate::net::log(&format!("EA install scan: {name:?} is installed in {dir}")); out.push(InstalledRow { platform_id: id, installed: true, install_path: Some(dir) }); }
            }
        }
        Some(out)
    }

    /// EA's Cleanup.exe guts a game folder and leaves the folder itself, so the whole subtree is watched (PlayDate's watcher did the same).
    fn watch_paths(&self) -> Vec<(PathBuf, bool)> { if cfg!(windows) { vec![] } else { self.prefix().map(|p| vec![(p.join("drive_c/Program Files/EA Games"), true)]).unwrap_or_default() } }

    /// Install, launch and uninstall are all "open the EA App": it does the work (the same approach as Lutris).
    fn launch(&self, g: &GameRef) -> Result<Launch, String> { self.open_app(if g.installed { "Play" } else { "Install" }, &g.name).map(|action| Launch { action, record: false }) }
    fn uninstall(&self, g: &GameRef) -> Result<Action, String> { self.open_app("Uninstall", &g.name) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{NetError, Response};
    fn tmpdir(tag: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("zest_ea_{tag}_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); std::fs::write(d.join("config.json"), "{\"other\": 1}").unwrap(); d }
    fn cx(d: &PathBuf) -> SourceCtx { let h: Arc<dyn Http> = Arc::new(crate::net::UreqHttp::new()); SourceCtx { http: h.clone(), steam: Arc::new(crate::steamclient::Steam::new(h, None, 10.0)), root: d.clone(), db: d.join("games.db"), today: "2026-11-04".into(), appinfo: Arc::new(|_| Default::default()), account_id: None } }
    fn golden() -> Value { serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/ea_golden.json")).unwrap()).unwrap() }
    fn ok(body: Value) -> Result<Response, NetError> { Ok(Response { status: 200, retry_after: None, body: body.to_string() }) }

    #[test] fn pasted_tokens_and_slug_candidates_agree_with_the_real_python() {
        let g = golden();
        for t in g["tokens"].as_array().unwrap() {
            let (raw, want) = (t[0].as_str().unwrap(), &t[1]);
            let (tok, exp) = parse_token_input(raw);
            assert_eq!((json!(tok), json!(exp)), (want[0].clone(), want[1].clone()), "{raw:?}");
        }
        for (slug, w) in g["slugs"].as_object().unwrap() {
            let want: Vec<String> = w["probed"].as_array().unwrap().iter().map(|u| u.as_str().unwrap().trim_start_matches("https://www.ea.com/games/").to_string()).collect();
            assert_eq!(slug_candidates(slug), want, "{slug:?}");
        }
    }
    #[test] fn answers_become_offers_details_and_dates() {
        let page = json!({"me": {"ownedGameProducts": {"next": "c2", "items": [
            {"originOfferId": "O1", "product": {"baseItem": {"gameType": "BASE_GAME"}}}, {"originOfferId": "O2", "product": {"baseItem": {"gameType": "DLC"}}}, {"product": {}}]}}});
        assert_eq!(entitlement_page(&page), (vec!["O1".to_string()], Some("c2".to_string())));
        let d = details(&json!({"legacyOffers": [{"offerId": "O1", "contentId": "C1"}, {"offerId": "O9", "contentId": "C9"}, "x"],
            "gameProducts": {"items": [{"id": "P1", "originOfferId": "O1", "gameSlug": "battlefield-1", "baseItem": {"title": " Battlefield 1 ", "keyArt": {"largestImage": {"path": "http://k"}}, "packArt": {"largestImage": {"path": "http://p"}}}}]}}));
        assert_eq!(d, vec![Detail { offer_id: "O1".into(), content_id: "C1".into(), name: "Battlefield 1".into(), slug: "battlefield-1".into(), horiz_url: "http://k".into(), vert_url: "http://p".into() }]);
        let dates = entitlement_dates(&json!({"entitlements": {"entitlement": [
            {"productId": "O1", "grantDate": "2020-05-01T12:30Z", "status": "ACTIVE"}, {"productId": "O1", "grantDate": "2019-01-01T00:00Z", "status": "ACTIVE"},
            {"productId": "O2", "grantDate": "2019-01-01T00:00Z", "status": "DISABLED"}, {"productId": "O3", "grantDate": "bad", "status": "ACTIVE"}]}}));
        assert_eq!(dates.len(), 1); assert_eq!(dates["O1"], 1546300800);
    }

    struct Fake;
    impl Http for Fake {
        fn get(&self, url: &str, _h: &[(&str, &str)], _t: Duration) -> Result<Response, NetError> {
            if url.contains("tokeninfo") { return ok(json!({"expires_in": 14400})); }
            if url.ends_with("/pids/me") { return ok(json!({"pid": {"pidId": 777}})); }
            if url.ends_with("/personas") { return ok(json!({"personas": {"personaUri": ["/pids/777/personas/5"]}})); }
            if url.ends_with("/personas/5") { return ok(json!({"persona": {"displayName": "Rob"}})); }
            if url.ends_with("/entitlements") { return ok(json!({"entitlements": {"entitlement": [{"productId": "O1", "grantDate": "2020-05-01T12:30Z", "status": "ACTIVE"}]}})); }
            if url.contains("ea.com/games/") { return Ok(Response { status: if url.ends_with("battlefield/battlefield-1") { 200 } else { 404 }, retry_after: None, body: String::new() }); }
            Err(NetError(format!("unexpected {url}")))
        }
        fn post(&self, _u: &str, _h: &[(&str, &str)], body: &str, _t: Duration) -> Result<Response, NetError> {
            if body.contains("getEntitlements") { return ok(json!({"data": {"me": {"ownedGameProducts": {"next": null, "items": [
                {"originOfferId": "O1", "product": {"baseItem": {"gameType": "BASE_GAME"}}}, {"originOfferId": "O1b", "product": {"baseItem": {"gameType": "BASE_GAME"}}}, {"originOfferId": "OOLD", "product": {"baseItem": {"gameType": "BASE_GAME"}}}]}}}})); }
            ok(json!({"data": {"legacyOffers": [{"offerId": "O1", "contentId": "C1"}, {"offerId": "O1b", "contentId": "C1"}],
                "gameProducts": {"items": [{"originOfferId": "O1", "gameSlug": "battlefield-1", "baseItem": {"title": "Battlefield 1"}}, {"originOfferId": "O1b", "gameSlug": "battlefield-1", "baseItem": {"title": "Battlefield 1 (reissue)"}}]}}}))
        }
    }

    #[test] fn a_pasted_token_signs_in_and_sync_adds_new_games_once_with_dates_and_slugs() {
        let d = tmpdir("flow"); let db = d.join("games_1.db");
        rusqlite::Connection::open(&db).unwrap().execute_batch("CREATE TABLE games (platform TEXT, platform_id TEXT, platform_appname TEXT, platform_slug TEXT); CREATE TABLE blacklist (platform_id TEXT); INSERT INTO games VALUES ('ea_app','OOLD','COLD','old');").unwrap();
        let s = EaSource::new(Arc::new(Fake), d.clone(), db);
        assert!(!s.auth_status().unwrap().connected && s.sync(&cx(&d), &mut |_| {}, &Cancel::new()).unwrap_err().contains("connect again"));
        assert!(s.auth_complete("two words").unwrap_err().contains("Could not extract") && s.auth_complete("error=login_required&x").unwrap_err().contains("not signed in"));
        let st = s.auth_complete("nucleus:rest#access_token=abc&expires_in=100").unwrap();
        assert_eq!((st.connected, st.username.as_deref()), (true, Some("Rob")));
        let cfg: Value = serde_json::from_str(&std::fs::read_to_string(d.join("config.json")).unwrap()).unwrap();
        assert_eq!((cfg["ea_app"]["pid"].as_str(), cfg["ea_app"]["access_token"].as_str(), cfg["other"].as_i64()), (Some("777"), Some("abc"), Some(1)));
        assert!(cfg["ea_app"]["expires_at"].as_i64().unwrap() > now() + 14000, "EA's own report of the token's life wins");
        let mut got = vec![]; assert_eq!(s.sync(&cx(&d), &mut |b| got.extend(b), &Cancel::new()).unwrap(), 1);
        let r = &got[0];
        assert_eq!((r.platform_id.as_str(), r.name.as_str(), r.slug.as_deref(), r.appname.as_deref(), r.date_added), ("O1", "Battlefield 1", Some("battlefield/battlefield-1"), Some("C1"), Some(1588336200)));
        let g = GameRef { platform_id: "O1".into(), slug: "battlefield/battlefield-1".into(), ..Default::default() };
        assert_eq!(s.store_url(&g).as_deref(), Some("https://www.ea.com/games/battlefield/battlefield-1"));
        assert_eq!(s.art_urls(&g).len(), 0, "this fake offers no art");
        s.auth_disconnect().unwrap(); assert!(!s.auth_status().unwrap().connected);
        let cfg: Value = serde_json::from_str(&std::fs::read_to_string(d.join("config.json")).unwrap()).unwrap(); assert!(cfg.get("ea_app").is_none() && cfg["other"] == 1);
    }
    #[test] fn an_expired_token_is_not_a_connection() {
        let d = tmpdir("exp"); let s = EaSource::new(Arc::new(Fake), d.clone(), d.join("g.db"));
        crate::account::plugin_secret_set(&d, "ea_app", "access_token", Some("old")).unwrap(); crate::account::plugin_number_set(&d, "ea_app", "expires_at", now() - 10).unwrap();
        assert!(!s.auth_status().unwrap().connected);
    }
    #[cfg(unix)]   // the fixture prefix's drive links are Unix symlinks
    #[test] fn the_install_scan_finds_installs_and_forgets_gutted_ones() {
        let d = tmpdir("scan"); let db = d.join("games.db");
        let fixture = crate::sources::ea_manifest::fixture_prefix();
        let root = std::fs::canonicalize(&fixture).unwrap();
        let bej = root.join("drive_c/Program Files (x86)/Origin Games/Bejeweled 3");
        let hospital = root.join("drive_c/Program Files/EA Games/Theme Hospital");
        let c = rusqlite::Connection::open(&db).unwrap();
        c.execute_batch("CREATE TABLE games (appid INTEGER, name TEXT, platform TEXT, platform_id TEXT, install_path TEXT, platform_executable TEXT, platform_appname TEXT, installed INTEGER);").unwrap();
        let ins = "INSERT INTO games VALUES (?1, ?2, 'ea_app', ?3, ?4, ?5, ?6, ?7)";
        c.execute(ins, rusqlite::params![-1, "Bejeweled 3", "O-bej", None::<String>, None::<String>, "70619", 0]).unwrap();                         // matched by content id
        c.execute(ins, rusqlite::params![-2, "Battlefield V", "O-bfv", None::<String>, None::<String>, None::<String>, 0]).unwrap();              // matched by its manifest title
        c.execute(ins, rusqlite::params![-3, "Theme Hospital", "O-th", hospital.to_string_lossy().to_string(), None::<String>, None::<String>, 1]).unwrap();  // gutted by Cleanup.exe
        c.execute(ins, rusqlite::params![-4, "Dungeon Keeper", "O-dk", "/no/such/folder", None::<String>, None::<String>, 1]).unwrap();           // folder gone
        c.execute(ins, rusqlite::params![-5, "Unknown Game", "O-unk", None::<String>, None::<String>, None::<String>, 0]).unwrap();               // nothing found: untouched
        c.execute(ins, rusqlite::params![-6, "Still There", "O-st", bej.to_string_lossy().to_string(), None::<String>, None::<String>, 1]).unwrap();
        drop(c);
        let s = EaSource::new(Arc::new(Fake), d.clone(), db.clone());
        // no prefix: only the recorded folders are judged
        let rows = s.installed_scan().unwrap();
        let get = |rows: &[InstalledRow], id: &str| rows.iter().find(|r| r.platform_id == id).cloned();
        assert!(get(&rows, "O-bej").is_none() && get(&rows, "O-unk").is_none());
        assert_eq!(get(&rows, "O-th"), Some(InstalledRow { platform_id: "O-th".into(), installed: false, install_path: Some(String::new()) }));
        assert_eq!(get(&rows, "O-dk").map(|r| r.installed), Some(false));
        assert_eq!(get(&rows, "O-st"), Some(InstalledRow { platform_id: "O-st".into(), installed: true, install_path: None }));
        crate::account::plugin_secret_set(&d, "ea_app", "prefix", Some(&root.to_string_lossy())).unwrap();
        let rows = s.installed_scan().unwrap();
        assert_eq!(get(&rows, "O-bej"), Some(InstalledRow { platform_id: "O-bej".into(), installed: true, install_path: Some(bej.to_string_lossy().to_string()) }));
        assert_eq!(get(&rows, "O-bfv").and_then(|r| r.install_path), Some(root.join("drive_e/Games/Battlefield V").to_string_lossy().to_string()));
        assert!(get(&rows, "O-unk").is_none(), "a game nothing matches is left alone");
        assert!(s.caps().launch && s.caps().uninstall && s.caps().installed_scan && !s.caps().install);
    }
    #[cfg(not(windows))]
    #[test] fn launch_and_uninstall_open_the_app_in_its_prefix_unless_it_is_already_open() {
        let d = tmpdir("launch"); let pfx = d.join("pfx");
        let s = EaSource::new(Arc::new(Fake), d.clone(), d.join("g.db"));
        let g = GameRef { appid: -1, name: "Bejeweled 3".into(), platform: "ea_app".into(), platform_id: "O1".into(), installed: true, ..Default::default() };
        assert!(s.launch(&g).unwrap_err().contains("not configured"));
        std::fs::create_dir_all(pfx.join("drive_c")).unwrap();
        crate::account::plugin_secret_set(&d, "ea_app", "prefix", Some(&pfx.to_string_lossy())).unwrap();
        assert!(s.launch(&g).unwrap_err().contains("not installed"));
        // the versioned folder is found, compatibility32 is skipped, the plain folder is preferred
        let base = pfx.join("drive_c/Program Files/Electronic Arts/EA Desktop");
        std::fs::create_dir_all(base.join("compatibility32")).unwrap(); std::fs::write(base.join("compatibility32/EADesktop.exe"), "x").unwrap();
        assert_eq!(find_in_ea_folder(&pfx, "EADesktop.exe"), None);
        std::fs::create_dir_all(base.join("13.0.1")).unwrap(); std::fs::write(base.join("13.0.1/EADesktop.exe"), "x").unwrap();
        assert_eq!(find_in_ea_folder(&pfx, "EADesktop.exe"), Some(base.join("13.0.1/EADesktop.exe")));
        std::fs::create_dir_all(base.join("EA Desktop")).unwrap(); std::fs::write(base.join("EA Desktop/EADesktop.exe"), "x").unwrap();
        let exe = base.join("EA Desktop/EADesktop.exe");
        assert_eq!(find_in_ea_folder(&pfx, "EADesktop.exe"), Some(exe.clone()));
        crate::wine::TEST_RUNTIME.with(|t| *t.borrow_mut() = Some(Ok(crate::wine::Runtime::Wine { wine: "/usr/bin/wine".into() })));
        let l = s.launch(&g).unwrap();
        assert!(!l.record, "opening the launcher is not playing the game");
        match &l.action {
            Action::Many(v) => {
                assert!(matches!(&v[0], Action::Message(m) if m.contains("play") && m.contains("Bejeweled 3")));
                match &v[1] { Action::Spawn { args, env, .. } => {
                    assert_eq!(args, &vec![exe.to_string_lossy().to_string(), "-ls=Launcher".to_string()]);
                    assert!(env.contains(&("WINEDLLOVERRIDES".into(), Some("winegstreamer=".into()))) && env.contains(&("WINEPREFIX".into(), Some(pfx.to_string_lossy().to_string()))));
                } a => panic!("{a:?}") }
            }
            a => panic!("{a:?}"),
        }
        let notinst = GameRef { installed: false, ..g.clone() };
        match s.launch(&notinst).unwrap().action { Action::Many(v) => assert!(matches!(&v[0], Action::Message(m) if m.contains("install"))), a => panic!("{a:?}") }
        match s.uninstall(&g).unwrap() { Action::Many(v) => assert!(matches!(&v[0], Action::Message(m) if m.contains("uninstall"))), a => panic!("{a:?}") }
        assert!(app_running(&[(1, "C:\\Program Files\\EA\\EADesktop.exe".into())]) && app_running(&[(2, "EALocalHostSvc.exe".into())]));
        assert!(!app_running(&[(3, "C:\\Games\\Bejeweled3.exe".into())]) && !app_running(&[]));
    }
}
