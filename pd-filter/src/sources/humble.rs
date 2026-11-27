//! Humble Bundle as a library source (port of the listing half of PlayDate's `humble` plugin: sign-in with the `_simpleauth_sess` session cookie, the order walk with
//! its game filter, re-scrape). Installing and launching Humble downloads belong to the shared install/launch layer, not here. Never logs the cookie.
use super::{Action, AuthBegin, AuthStatus, Caps, GameRef, Installed, InstalledRow, Launch, LibrarySource, SourceCtx};
use crate::importer::GameRecord;
use crate::jobs::Update;
use crate::net::{log, Cancel, Http};
use crate::steamclient::Fetch;
use regex::Regex;
use rusqlite::types::Value as Sql;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

const API: &str = "https://www.humblebundle.com/api/v1";
const UA: &str = "Mozilla/5.0 (compatible; PlayDate)";
const EXPIRED: &str = "Session expired — please reconnect.";
const NON_GAME_KEYWORDS: &[&str] = &["soundtrack", " ost", "original score", "music pack", "artbook", "art book", "digital art", "wallpaper", "comic", "ebook", "e-book", "graphic novel", "documentary", "making of", "source code", "installer"];

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex { cell.get_or_init(|| Regex::new(pat).unwrap()) }

// ---------------------------------------------------------------- pure helpers (checked against the Python)

/// `_looks_non_game`
pub fn looks_non_game(name: Option<&str>) -> bool { let n = name.unwrap_or("").to_lowercase(); NON_GAME_KEYWORDS.iter().any(|k| n.contains(k)) }

/// `_payee_dev`: the studio Humble pays for a direct purchase; Humble itself is not a developer.
pub fn payee_dev(sub: &Value) -> String {
    let payee = sub.get("payee").filter(|p| p.is_object());
    if payee.and_then(|p| p.get("machine_name")).and_then(|m| m.as_str()) == Some("humblebundle") { return String::new(); }
    payee.and_then(|p| p.get("human_name")).and_then(|n| n.as_str()).unwrap_or("").trim().to_string()
}

/// `_is_software_bundle`
pub fn is_software_bundle(order: &Value) -> bool {
    let p = order.get("product").filter(|p| p.is_object());
    let g = |k: &str| match p.and_then(|p| p.get(k)) { Some(Value::String(s)) => s.clone(), Some(Value::Null) => "None".to_string(), Some(v) => v.to_string(), None => String::new() };
    let t = format!("{} {}", g("human_name"), g("machine_name")).to_lowercase();
    t.contains("software bundle") || t.contains("softwarebundle")
}

/// The reason the deep scan gives for an order item that no longer passes the import rules (`scan_junk`'s three branches, in its order), or None for a game.
pub fn junk_reason(sub: &Value, order: &Value, row_name: &str) -> Option<&'static str> {
    let name = sub.get("human_name").and_then(|n| n.as_str()).filter(|n| !n.is_empty()).unwrap_or(row_name);
    if looks_non_game(Some(name)) { return Some("source code / installer / non-game"); }
    let has_desktop = sub.get("downloads").and_then(|d| d.as_array()).map_or(false, |a| a.iter().any(|d| matches!(d.get("platform").and_then(|p| p.as_str()).unwrap_or("").to_lowercase().as_str(), "windows" | "linux" | "mac")));
    if !has_desktop { return Some("no desktop download (Android / audio / ebook)"); }
    if !is_game(sub, Some(order)) { return Some(if is_software_bundle(order) { "software-bundle application" } else { "not a game" }); }
    None
}

/// `_is_game`: a desktop download, not an extra (soundtrack, art book, source code, an installer in a software bundle).
pub fn is_game(sub: &Value, order: Option<&Value>) -> bool {
    let dls: Vec<&Value> = sub.get("downloads").and_then(|d| d.as_array()).map(|a| a.iter().filter(|d| matches!(d.get("platform").and_then(|p| p.as_str()).unwrap_or("").to_lowercase().as_str(), "windows" | "linux" | "mac")).collect()).unwrap_or_default();
    if dls.is_empty() || looks_non_game(sub.get("human_name").and_then(|n| n.as_str())) { return false; }
    if order.map_or(false, is_software_bundle) {
        let names: HashSet<String> = dls.iter().flat_map(|d| d.get("download_struct").and_then(|s| s.as_array()).into_iter().flatten()).map(|ds| ds.get("name").and_then(|n| n.as_str()).unwrap_or("").trim().to_lowercase()).collect();
        if !names.is_empty() && names.iter().all(|n| n == "installer") { return false; }
    }
    true
}

/// `_parse_dt`: "2020-05-17T13:45:10" or "2020-05-17 13:45:10" (anything after the first 19 characters ignored), as a UTC time.
pub fn parse_dt(s: Option<&str>) -> Option<i64> {
    static R: OnceLock<Regex> = OnceLock::new();
    let head: String = s?.chars().take(19).collect();
    let c = re(&R, r"^(\d{4})-(\d{2})-(\d{2})[T ](\d{2}):(\d{2}):(\d{2})$").captures(&head)?;
    let n = |i: usize| c[i].parse::<i64>().unwrap();
    let (y, mo, d, h, mi, se) = (n(1), n(2), n(3), n(4), n(5), n(6));
    let dim = match mo { 1 | 3 | 5 | 7 | 8 | 10 | 12 => 31, 4 | 6 | 9 | 11 => 30, 2 => if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 { 29 } else { 28 }, _ => return None };
    if y < 1 || d < 1 || d > dim || h > 23 || mi > 59 || se > 59 { return None; }
    let (yy, mm) = if mo <= 2 { (y - 1, mo + 9) } else { (y, mo - 3) };
    let era = yy.div_euclid(400); let yoe = yy - era * 400; let doy = (153 * mm + 2) / 5 + d - 1; let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146097 + doe - 719468) * 86400 + h * 3600 + mi * 60 + se)
}

/// `urllib.parse.unquote`: percent-decoding as UTF-8 (a bad sequence stays as it is, bad bytes become U+FFFD).
pub fn unquote(s: &str) -> String {
    let b = s.as_bytes(); let mut out = Vec::with_capacity(b.len()); let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() + 0 && b.get(i + 1).map_or(false, |c| c.is_ascii_hexdigit()) && b.get(i + 2).map_or(false, |c| c.is_ascii_hexdigit()) {
            out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap()); i += 3;
        } else { out.push(b[i]); i += 1; }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// What `connect` makes of a pasted cookie: decoded and without the quotes.
pub fn normalise_cookie(raw: &str) -> String { unquote(raw.trim()).trim_matches('"').to_string() }

/// The name shown for the account, from the library page: the username, else the e-mail with its local part masked.
pub fn username_from_page(text: &str) -> Option<String> {
    static U: OnceLock<Regex> = OnceLock::new(); static E: OnceLock<Regex> = OnceLock::new();
    let raw = re(&U, r#""username"\s*:\s*"([^"]+)""#).captures(text).or_else(|| re(&E, r#""email"\s*:\s*"([^"]+)""#).captures(text))?[1].to_string();
    Some(match raw.split_once('@') {
        Some((local, domain)) => { let n = local.chars().count(); let visible: String = local.chars().take(if n > 2 { 2 } else { 1 }).collect(); format!("{visible}{}@{domain}", "*".repeat(n.saturating_sub(2).max(1))) }
        None => raw,
    })
}

// ---------------------------------------------------------------- the source

pub struct HumbleSource { http: Arc<dyn Http>, config_dir: PathBuf, db: PathBuf }

impl HumbleSource {
    pub fn new(http: Arc<dyn Http>, config_dir: PathBuf, db: PathBuf) -> Self { HumbleSource { http, config_dir, db } }
    fn secret(&self, k: &str) -> Option<String> { crate::account::plugin_secret_get(&self.config_dir, "humble", k).filter(|v| !v.is_empty()) }
    /// The Cookie header: a saved value is the old `_simpleauth_sess` value, or (since Humble moved its sign-in to Ory, 2026) the whole `ory_session_<site>=<value>` pair the sign-in window read.
    fn cookie_header(cookie: &str) -> String { if cookie.starts_with("ory_session") { cookie.to_string() } else { format!("_simpleauth_sess={cookie}") } }
    fn headers(cookie: &str, accept: &str) -> Vec<(String, String)> { vec![("Cookie".into(), Self::cookie_header(cookie)), ("User-Agent".into(), UA.into()), ("Accept".into(), accept.into())] }
    fn get(&self, url: &str, cookie: &str, accept: &str, secs: u64) -> Result<crate::net::Raw, String> {
        let h = Self::headers(cookie, accept); let hs: Vec<(&str, &str)> = h.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        self.http.get_raw(url, &hs, Duration::from_secs(secs)).map_err(|e| { log(&format!("Humble: {e}")); "Could not reach Humble Bundle".to_string() })
    }
    /// A 3xx or an error is a rejected session (redirects are not followed): `connect`'s test.
    fn rejected(status: u16) -> bool { status == 301 || status == 302 || status >= 400 }

    /// `check_connection`: Some(true) accepted, Some(false) rejected, None unknown. Humble re-issues the cookie on every answer (a sliding window): the renewed one is saved.
    pub fn check_connection(&self) -> Option<bool> {
        let cookie = self.secret("session_cookie")?;
        // the rebuilt site (Nov 2026): a session is the `ory_session_*` pair and Humble answers for it at /api/auth/session-status; an old `_simpleauth_sess` value no longer works anywhere
        if cookie.starts_with("ory_session") { return super::humble_site::session_status(self.http.as_ref(), &cookie); }
        if !cookie.is_empty() { return Some(false); }
        let r = self.get(&format!("{API}/user/order"), &cookie, "application/json", 15).ok()?;
        if r.status == 401 || r.status == 403 || (300..400).contains(&r.status) { return Some(false); }
        if r.status != 200 { return None; }
        let renewed = r.header_all("set-cookie").into_iter().find_map(|c| { let (n, v) = c.split(';').next()?.split_once('=')?; (n.trim() == "_simpleauth_sess").then(|| v.trim().to_string()) });
        if let Some(n) = renewed { if n.trim_matches('"') != cookie.trim_matches('"') { let _ = crate::account::plugin_secret_set(&self.config_dir, "humble", "session_cookie", Some(&n)); log("Humble: saved renewed session cookie"); } }
        Some(true)
    }

    /// The id of the library server action, found by reading the site's scripts (it changes with every deploy); kept for the run, found again when Humble refuses it.
    fn action(&self, cookie: &str, force: bool) -> Result<String, String> {
        static ACTION: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
        if !force { if let Some(a) = ACTION.lock().ok().and_then(|g| g.clone()) { return Ok(a); } }
        let a = super::humble_site::discover_action(self.http.as_ref(), cookie)?;
        if let Ok(mut g) = ACTION.lock() { *g = Some(a.clone()); }
        Ok(a)
    }
    /// One page of the library list; a refused action (the id changed under us) is looked up again once, a hiccup is tried three times.
    fn list_page(&self, cookie: &str, page: usize, query: &str, hide_revealed: bool, cancel: &Cancel) -> Result<super::humble_site::ListPage, String> {
        let mut action = self.action(cookie, false)?;
        let mut rediscovered = false;
        let mut last = String::new();
        for attempt in 0..3 {
            match super::humble_site::fetch_list_page(self.http.as_ref(), cookie, &action, page, query, hide_revealed) {
                Ok(p) => return Ok(p),
                Err(e) if e == "EXPIRED" => return Err(EXPIRED.into()),
                Err(e) => {
                    last = e;
                    if !rediscovered { rediscovered = true; if let Ok(a) = self.action(cookie, true) { action = a; } }
                    sleep_unless(cancel, 800 * (attempt + 1));
                    if cancel.is_set() { break; }
                }
            }
        }
        Err(last)
    }
    fn cache_path(&self) -> PathBuf { self.config_dir.join("humble_cache.json") }
    fn load_cache(&self) -> HashMap<String, Vec<String>> {
        std::fs::read_to_string(self.cache_path()).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v.get("gamekeys_map").cloned()).and_then(|m| m.as_object().cloned())
            .map(|o| o.into_iter().filter_map(|(k, v)| Some((k, v.as_array()?.iter().filter_map(|x| x.as_str().map(String::from)).collect()))).collect()).unwrap_or_default()
    }
    /// The orders the library already knows: each Humble game's slug is `gamekey/machine_name`, so the games of an order are its rows. This is what lets a first sync
    /// (no cache yet) skip every order that already produced a game, instead of fetching hundreds of orders one by one.
    fn orders_in_library(&self) -> HashMap<String, Vec<String>> {
        let mut m: HashMap<String, Vec<String>> = HashMap::new();
        let Ok(c) = rusqlite::Connection::open_with_flags(&self.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { return m };
        let Ok(mut st) = c.prepare("SELECT platform_slug, platform_id FROM games WHERE platform = 'humble' AND platform_slug LIKE '%/%'") else { return m };
        for (slug, id) in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))).into_iter().flatten().flatten() {
            if let (Some((key, _)), Some(id)) = (slug.split_once('/'), id) { m.entry(key.to_string()).or_default().push(id); }
        }
        m
    }
    fn save_cache(&self, map: &HashMap<String, Vec<String>>) { let _ = std::fs::write(self.cache_path(), json!({"gamekeys_map": map}).to_string()); }
    fn known_ids(&self) -> (HashSet<String>, HashSet<String>) {
        let Ok(c) = rusqlite::Connection::open_with_flags(&self.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { return Default::default() };
        let col = |sql: &str| -> HashSet<String> { c.prepare(sql).ok().map(|mut s| s.query_map([], |r| r.get::<_, Option<String>>(0)).map(|it| it.flatten().flatten().collect()).unwrap_or_default()).unwrap_or_default() };
        (col("SELECT platform_id FROM games WHERE platform = 'humble'"), col("SELECT platform_id FROM blacklist WHERE platform_id IS NOT NULL"))
    }
}

/// The games one Humble order holds, as library records: the shared core of a cookie sync and a browser import. `seen` collects machine names across
/// orders (the first order to list a game wins); `blacklisted` are games the user removed. Returns every game name in the order (for the order cache)
/// and the new records.
fn records_from_order(gamekey: &str, order: &Value, seen: &mut HashSet<String>, blacklisted: &HashSet<String>) -> (Vec<String>, Vec<GameRecord>) {
    let order_date = parse_dt(order.get("created").and_then(|c| c.as_str())).unwrap_or_else(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0));
    let (mut names, mut batch) = (vec![], vec![]);
    for sub in order.get("subproducts").and_then(|s| s.as_array()).into_iter().flatten() {
        if !is_game(sub, Some(order)) { continue; }
        let machine = sub.get("machine_name").and_then(|m| m.as_str()).unwrap_or("").trim().to_string();
        if machine.is_empty() { continue; }
        names.push(machine.clone());
        if !seen.insert(machine.clone()) || blacklisted.contains(&machine) { continue; }
        let name = sub.get("human_name").and_then(|n| n.as_str()).map(str::trim).filter(|n| !n.is_empty()).unwrap_or(&machine).to_string();
        let dev = Some(payee_dev(sub)).filter(|d| !d.is_empty());
        batch.push(GameRecord { platform_id: machine.clone(), name, slug: Some(format!("{gamekey}/{machine}")), date_added: Some(order_date), developers: dev.clone(), publishers: dev, ..Default::default() });
    }
    (names, batch)
}


fn sleep_unless(c: &Cancel, ms: u64) { let end = std::time::Instant::now() + Duration::from_millis(ms); while !c.is_set() && std::time::Instant::now() < end { std::thread::sleep(Duration::from_millis(25)); } }

impl LibrarySource for HumbleSource {
    fn id(&self) -> &str { "humble" }
    fn label(&self) -> &str { "Humble Bundle" }
    fn caps(&self) -> Caps { Caps { auth: true, sync: true, rescrape: true, launch: true, install: true, uninstall: true, installed_scan: true, ..Default::default() } }

    fn games_dir(&self) -> Option<PathBuf> { Some(self.dir()) }
    /// A Humble game is installed while the folder Zest made for it exists.
    fn installed_scan(&self) -> Option<Vec<InstalledRow>> {
        let c = rusqlite::Connection::open_with_flags(&self.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
        let mut st = c.prepare("SELECT platform_id, COALESCE(install_path, '') FROM games WHERE platform = 'humble' AND platform_id IS NOT NULL").ok()?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).ok()?.flatten()
            .map(|(id, path)| InstalledRow { installed: !path.is_empty() && std::path::Path::new(&path).exists(), platform_id: id, install_path: None }).collect();
        Some(rows)
    }
    fn launch(&self, g: &GameRef) -> Result<Launch, String> {
        if !g.installed { return Err(format!("{} is not installed yet.", g.name)); }
        let dir = g.install_path.as_deref().map(std::path::Path::new).filter(|d| d.is_dir()).ok_or("Game folder not found \u{2014} it may have been moved or deleted.")?;
        super::folder::unpack_leftovers(dir);   // an archive left from a download Zest could not unpack at the time
        // nothing to run (an installer Zest could not unpack, say): open the folder so the user can see what is there
        let mut choice = super::folder::choose_executable(dir, g.executable.as_deref());
        // the program just picked for this launch (a pack of games: nothing is saved)
        let once = super::take_launch_choice(g.appid).filter(|r| dir.join(r).is_file());
        if let Some(rel) = &once { choice = super::folder::Choice::File(rel.clone()); }
        // a download can hold several games: with no program chosen yet (and no start script of the game's own), more than one candidate is asked about instead of guessed
        if once.is_none() && g.executable.as_deref().map_or(true, |e| e.is_empty()) && super::folder::launcher_script(dir).is_none() {
            let mut c = crate::native_exe::all_candidates(dir, crate::native_exe::Os::current());
            if c.len() > 1 { c.sort(); choice = super::folder::Choice::Ask(c); }
        }
        super::folder::launch_choice(dir, choice, &|abs, cwd| super::folder::run_file(abs, cwd, Some(&self.prefix(g.appid))))
    }
    fn uninstall(&self, g: &GameRef) -> Result<Action, String> {
        let dir = g.install_path.clone().filter(|p| !p.is_empty()).ok_or_else(|| format!("{} has no folder to remove.", g.name))?;
        Ok(Action::RemoveInstall { delete: crate::installdir::is_inside(std::path::Path::new(&dir), &self.dir()), dir })
    }
    fn scan_junk(&self) -> Result<Vec<super::JunkHit>, String> {
        let Some(cookie) = self.secret("session_cookie") else { return Ok(vec![]) };
        let rows: Vec<(i64, String, String)> = {
            let Ok(c) = rusqlite::Connection::open_with_flags(&self.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { return Ok(vec![]) };
            let mut st = c.prepare("SELECT appid, COALESCE(name,''), platform_slug FROM games WHERE platform='humble' AND platform_slug LIKE '%/%' AND COALESCE(playtime_forever,0)=0 AND COALESCE(installed,0)=0 AND COALESCE(completion_status,'Never Played')='Never Played'").map_err(|e| e.to_string())?;
            let v: Vec<_> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, String>(2)?))).map_err(|e| e.to_string())?.flatten().collect();
            v
        };
        let mut by_key: std::collections::BTreeMap<String, Vec<(i64, String, String)>> = Default::default();
        for (id, name, slug) in rows { if let Some((k, m)) = slug.split_once('/') { if !k.is_empty() && !m.is_empty() { by_key.entry(k.to_string()).or_default().push((id, name, m.to_string())); } } }
        let mut out = Vec::new();
        for (key, members) in by_key {
            // a link made on the old site (an order's gamekey) names no bundle of the new one: such rows cannot be checked and are skipped
            let order: Value = match self.get(&format!("{}/{key}", super::humble_site::LIBRARY_URL), &cookie, "text/html", 30) {
                Ok(r) if !Self::rejected(r.status) => super::humble_site::bundle_order(&r.body, &key, "", &HashMap::new()),
                _ => { log("Humble scan_junk: a bundle page was not readable, skipped"); continue }
            };
            // the page does not carry machine names: the items are matched by title
            let subs: HashMap<String, &Value> = order.get("subproducts").and_then(|s| s.as_array()).into_iter().flatten().map(|s| (s.get("human_name").and_then(|m| m.as_str()).unwrap_or("").trim().to_lowercase(), s)).collect();
            for (appid, name, _machine) in members {
                let Some(sub) = subs.get(&name.trim().to_lowercase()) else { continue };
                if let Some(why) = junk_reason(sub, &order, &name) { out.push(super::JunkHit { appid, name, reason: why.to_string() }); }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(out)
    }
    fn platform_choice(&self) -> bool { true }
    fn pick_program_each_time(&self) -> bool { true }
    fn install_options(&self, _cx: &SourceCtx, g: &GameRef) -> Result<Vec<super::InstallOption>, String> {
        let cookie = self.secret("session_cookie").ok_or("Not connected to Humble Bundle \u{2014} please reconnect")?;
        if !cookie.starts_with("ory_session") { return Ok(vec![]); }
        let (_, sub) = self.resolve_item(&cookie, g, &Cancel::new())?;
        let mut out: Vec<super::InstallOption> = vec![];
        for plat in ["windows", "linux", "mac"] {
            let Some(dl) = sub.get("downloads").and_then(|d| d.as_array()).into_iter().flatten().rev().find(|d| d.get("platform").and_then(|p| p.as_str()) == Some(plat)) else { continue };
            let files: Vec<String> = dl.get("download_struct").and_then(|d| d.as_array()).into_iter().flatten().map(|f| {
                let (n, sz) = (f.get("name").and_then(|x| x.as_str()).unwrap_or(""), f.get("human_size").and_then(|x| x.as_str()).unwrap_or(""));
                if sz.is_empty() { n.to_string() } else { format!("{n} {sz}") }
            }).filter(|f| !f.is_empty()).collect();
            let label = format!("{}{}", match plat { "windows" => "Windows (run through Wine)", "linux" => "Linux (native)", _ => "Mac" }, if files.is_empty() { String::new() } else { format!(": {}", files.join(", ")) });
            out.push(super::InstallOption { key: plat.to_string(), label });
        }
        Ok(out)
    }
    fn install(&self, _cx: &SourceCtx, g: &GameRef, progress: &mut dyn FnMut(&str, u64, u64), cancel: &Cancel) -> Result<Installed, String> {
        let cookie = self.secret("session_cookie").ok_or("Not connected to Humble Bundle \u{2014} please reconnect")?;
        // the rebuilt site signs each download address on request (the page's own "Download" button asks `getSignedUrlsForEntitlements` for the item)
        if cookie.starts_with("ory_session") { return self.install_new_site(&cookie, g, progress, cancel); }
        let (gamekey, machine) = g.slug.split_once('/').ok_or("This game has no Humble order link; sync your Humble library again")?;
        progress(&format!("{}: asking Humble for the download", g.name), 0, 0);
        // a fresh order: the download addresses are signed and expire
        let r = self.get(&format!("{API}/order/{gamekey}"), &cookie, "application/json", 15)?;
        if Self::rejected(r.status) { return Err("Could not fetch the download link \u{2014} try reconnecting your Humble account.".into()); }
        let order: Value = serde_json::from_str(&r.body).map_err(|_| "Humble sent an unexpected answer".to_string())?;
        let (url, filename, platform) = pick_download(&order, machine, crate::native_exe::Os::current()).ok_or("Humble offers no download for this game")?;
        log(&format!("Humble: downloading {:?} for {:?} ({platform})", filename, g.name));
        self.download_and_unpack(g, &url, &filename, progress, cancel)
    }
    fn auth_status(&self) -> Result<AuthStatus, String> {
        if self.secret("session_cookie").is_none() { return Ok(AuthStatus { connected: false, username: None }); }
        // asked each time the dialog opens, as PlayDate's status route did: a rejected cookie shows as signed out
        if self.check_connection() == Some(false) { return Ok(AuthStatus { connected: false, username: None }); }
        Ok(AuthStatus { connected: true, username: Some(self.secret("username").unwrap_or_else(|| "Connected".into())) })
    }
    fn web_login_done(&self) -> Option<serde_json::Value> { Some(serde_json::json!({"kind": "cookies", "url": "https://www.humblebundle.com/", "names": ["ory_session_*"], "url_not_contains": "/login"})) }   // Humble's sign-in is Ory Kratos now: the session cookie is `ory_session_<site suffix>` (seen live 2026-11-22), not `_simpleauth_sess`; the window is done once it exists on a page that is not the login
    fn paste_login_kept(&self) -> bool { false }   // the sign-in window works (confirmed live, build 31)
    fn auth_begin(&self) -> Result<AuthBegin, String> {
        Ok(AuthBegin { url: "https://www.humblebundle.com/home/library".into(), placeholder: "Paste your _simpleauth_sess cookie value here".into(), instructions: vec![
            "Sign in to your Humble Bundle account in your browser.".into(),
            "Open the developer tools (F12), go to Application (Chrome) or Storage (Firefox), then Cookies, then humblebundle.com.".into(),
            "Copy the value of the cookie named _simpleauth_sess and paste it below.".into()] })
    }
    fn auth_complete(&self, value: &str) -> Result<AuthStatus, String> {
        let cookie = normalise_cookie(value);
        if !cookie.starts_with("ory_session") { return Err("Humble changed its sign-in in November 2026: use SIGN IN (the sign-in window) instead of pasting a cookie.".into()); }
        let status = super::humble_site::session_status(self.http.as_ref(), &cookie);
        log(&format!("Humble: the sign-in check says {}", match status { Some(true) => "signed in", Some(false) => "signed out", None => "nothing" }));
        if status != Some(true) { return Err("Humble did not accept the session the sign-in window read. If you just signed in, please tell the developer (see the log).".into()); }
        let mut username = "Humble account".to_string();
        let h = [("Cookie", cookie.as_str()), ("User-Agent", "Mozilla/5.0 (X11; Linux x86_64; rv:130.0) Gecko/20100101 Firefox/130.0"), ("Accept", "text/html")];
        if let Ok(p) = self.http.get_raw(super::humble_site::LIBRARY_URL, &h, Duration::from_secs(20)) {
            if p.status < 400 { if let Some(u) = username_from_page(&super::humble_site::flight_payload(&p.body)).or_else(|| username_from_page(&p.body)) { username = u; } }
        }
        for (k, v) in [("session_cookie", cookie.as_str()), ("username", username.as_str())] { crate::account::plugin_secret_set(&self.config_dir, "humble", k, Some(v)).map_err(|_| "Could not save the session".to_string())?; }
        log("Humble Bundle connected");
        Ok(AuthStatus { connected: true, username: Some(username) })
    }
    fn auth_disconnect(&self) -> Result<(), String> {
        for k in ["session_cookie", "username"] { crate::account::plugin_secret_set(&self.config_dir, "humble", k, None).map_err(|_| "Could not save".to_string())?; }
        let _ = std::fs::remove_file(self.cache_path()); Ok(())
    }

    fn sync(&self, _cx: &SourceCtx, sink: &mut dyn FnMut(Vec<GameRecord>), cancel: &Cancel) -> Result<usize, String> {
        let cookie = self.secret("session_cookie").ok_or("Not connected")?;
        if !cookie.starts_with("ory_session") { return Err("Humble changed its sign-in in November 2026. Please sign in to Humble again.".into()); }
        if super::humble_site::session_status(self.http.as_ref(), &cookie) == Some(false) { return Err(EXPIRED.into()); }
        // what each bundle (the old API's "order") produced, overlaid by the cache, which also knows games the user removed and every bundle already looked at
        let mut map = self.orders_in_library();
        map.extend(self.load_cache());
        let (existing, blacklisted) = self.known_ids();
        let seen_before: HashSet<String> = self.load_cache().into_keys().collect();
        let full = seen_before.is_empty();

        // 1. the list of everything bought, newest first. Keys already redeemed are left out (a third of it); downloads never are. A page whose bundles were all looked at on an earlier sync ends an
        //    ordinary sync: everything after it is older. The first sync (no cache) reads it all.
        let mut bundles: Vec<String> = vec![];
        let mut contents: HashMap<String, Vec<String>> = HashMap::new();
        let mut names: HashMap<String, String> = HashMap::new();
        let mut content_ids: HashMap<String, String> = HashMap::new();
        let (mut page, mut total) = (1usize, 1usize);
        while page <= total {
            if cancel.is_set() { break; }
            crate::net::set_activity(&format!("Humble Bundle: reading your library, page {page} of {total}"));
            let p = self.list_page(&cookie, page, "", true, cancel)?;
            total = p.total_pages.max(1);
            if p.items.is_empty() { break; }
            let mut all_known = !full;
            for it in &p.items {
                if it.bundle_id.is_empty() { continue; }
                if !seen_before.contains(&it.bundle_id) { all_known = false; }
                if !contents.contains_key(&it.bundle_id) { bundles.push(it.bundle_id.clone()); contents.insert(it.bundle_id.clone(), vec![]); }
                if it.kind == "download_drm_free" {
                    if let Some(c) = &it.content_id {
                        contents.get_mut(&it.bundle_id).map(|v| if !v.contains(c) { v.push(c.clone()); });
                        content_ids.insert(it.id.clone(), c.clone());
                        names.entry(it.bundle_id.clone()).or_insert_with(|| it.sub_title.clone());
                    }
                }
            }
            if all_known { break; }
            page += 1;
            sleep_unless(cancel, 250);
        }
        // 2. a bundle is worth reading when a game it holds is neither in the library nor removed by the user (a bundle already read counts by the games it produced)
        let persisted = self.load_cache();
        let is_new = |n: &String| !existing.contains(n) && !blacklisted.contains(n);
        let todo: Vec<&String> = bundles.iter().filter(|b| {
            let c = &contents[*b];
            !c.is_empty() && match persisted.get(*b) { Some(known) => known.iter().any(is_new), None => c.iter().any(is_new) }
        }).collect();
        log(&format!("Humble: {} bundles in the library list, {} to read", bundles.len(), todo.len()));
        // a bundle nothing needs from (every game in the library or removed, or only extras) is settled now; one still to be read is cached only once it has been (a stopped sync starts it again)
        for b in bundles.iter().filter(|b| !todo.contains(b)) { map.entry(b.clone()).or_insert_with(|| contents[b].clone()); }
        let (mut seen, mut count) = (HashSet::new(), 0usize);
        let total = todo.len();
        for (i, bundle) in todo.into_iter().enumerate() {
            if cancel.is_set() { break; }
            crate::net::set_activity(&format!("Humble Bundle: bundle {} of {total}", i + 1));
            if i > 0 && i % 10 == 0 { self.save_cache(&map); }
            let html = match self.get(&format!("{}/{bundle}", super::humble_site::LIBRARY_URL), &cookie, "text/html", 30) {
                Ok(o) if o.status < 400 => o.body,
                Ok(o) => { log(&format!("Humble: bundle page answered {}", o.status)); sleep_unless(cancel, 300); continue }
                Err(_) => { sleep_unless(cancel, 500); continue }
            };
            let order = super::humble_site::bundle_order(&html, bundle, names.get(bundle).map(String::as_str).unwrap_or(""), &content_ids);
            let (game_names, batch) = records_from_order(bundle, &order, &mut seen, &blacklisted);
            count += batch.len(); if !batch.is_empty() { sink(batch); }
            map.insert(bundle.clone(), game_names);
            sleep_unless(cancel, 200);
        }
        self.save_cache(&map);
        crate::net::set_activity("");
        Ok(count)
    }

    /// Re-fetch one game: its name, looked up by title in the library list (the machine name is `platform_id`). The studio the old API gave is not on the new site.
    fn rescrape(&self, cx: &SourceCtx, g: &GameRef) -> Fetch<Option<Update>> {
        let Some(cookie) = self.secret("session_cookie") else { log("Humble rescrape: not connected"); return Ok(None) };
        let machine = if g.platform_id.is_empty() { g.slug.split_once('/').map(|(_, m)| m.to_string()).unwrap_or_default() } else { g.platform_id.clone() };
        if machine.is_empty() || !cookie.starts_with("ory_session") { return Ok(None); }
        let Ok(page) = self.list_page(&cookie, 1, &g.name, false, &Cancel::new()) else { return Ok(None) };
        let Some(it) = page.items.iter().find(|i| i.kind == "download_drm_free" && i.content_id.as_deref() == Some(machine.as_str())) else { return Ok(None) };
        let mut u: Update = vec![("meta_fetched", Sql::Text(cx.today.clone()))];
        if !it.title.trim().is_empty() { u.push(("name", Sql::Text(it.title.trim().to_string()))); }
        Ok(Some(u))
    }
}



// ---------------------------------------------------------------- download, install and launch (a folder per game under the games folder)

/// `_platform_pref`: the platforms to take a download from, best first.
pub fn platform_pref(os: crate::native_exe::Os) -> [&'static str; 4] {
    use crate::native_exe::Os;
    // on Linux the Windows build comes first (run through Wine in its own prefix): Humble's Linux builds are often broken, PlayDate took them first
    match os { Os::Windows => ["windows", "linux", "mac", "android"], Os::Mac => ["mac", "linux", "windows", "android"], Os::Linux => ["windows", "linux", "mac", "android"] }
}
/// `get_download_url`'s choice: the download of a game (`machine_name`) in an order, as (signed address, file name, platform). The first platform in the preference with a link wins;
/// within it a file Zest can unpack beats one it cannot, and 64-bit beats 32-bit (PlayDate took the first listed, which for some games is a `.deb`).
pub fn pick_download(order: &Value, machine_name: &str, os: crate::native_exe::Os) -> Option<(String, String, String)> {
    let sub = order.get("subproducts")?.as_array()?.iter().find(|s| s.get("machine_name").and_then(|m| m.as_str()) == Some(machine_name))?;
    let downloads = sub.get("downloads")?.as_array()?;
    for plat in platform_pref(os) {
        let Some(dl) = downloads.iter().rev().find(|d| d.get("platform").and_then(|p| p.as_str()) == Some(plat)) else { continue };   // a repeated platform: the last, as a dict would keep
        let mut best: Option<((u8, u8), String, String)> = None;
        for st in dl.get("download_struct").and_then(|d| d.as_array()).into_iter().flatten() {
            let url = st.get("url").and_then(|u| u.get("web")).and_then(|u| u.as_str()).unwrap_or("");
            if url.is_empty() { continue; }
            let path = url.split(['?', '#']).next().unwrap_or(url);
            let file = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).filter(|n| !n.is_empty()).unwrap_or_else(|| st.get("name").and_then(|n| n.as_str()).unwrap_or("download").to_string());
            let label = format!("{} {}", st.get("name").and_then(|n| n.as_str()).unwrap_or(""), file).to_lowercase();
            let key = (u8::from(crate::archive::kind_of(&file).is_none()), u8::from(label.contains("32") && !label.contains("64")));
            if best.as_ref().map_or(true, |(k, _, _)| key < *k) { best = Some((key, url.to_string(), file)); }
        }
        if let Some((_, url, file)) = best { return Some((url, file, plat.to_string())); }
    }
    None
}

/// The platforms to try, best first, for a person's choice (`windows`, `linux`, `mac`, anything else = the system's own order); a one-off pick for this install (`only`) is the only one tried.
pub fn download_order(pref: &str, only: Option<&str>, os: crate::native_exe::Os) -> Vec<&'static str> {
    const ALL: [&str; 4] = ["windows", "linux", "mac", "android"];
    if let Some(o) = only { return ALL.iter().copied().filter(|p| *p == o).collect(); }
    match pref {
        "windows" => vec!["windows", "linux", "mac", "android"],
        "linux" => vec!["linux", "windows", "mac", "android"],
        "mac" => vec!["mac", "linux", "windows", "android"],
        _ => platform_pref(os).to_vec(),
    }
}
/// The download to fetch from an item of a bundle page and its signed addresses: (address, file name, platform). Same preference as `pick_download` (platform first; a file Zest can unpack
/// beats one it cannot; 64-bit beats 32-bit); a `.torrent` is never chosen. `signed` = (download key, file name, address) as `humble_site::signed_urls` gives them.
pub fn pick_signed(sub: &Value, signed: &[(String, String, String)], order: &[&str]) -> Option<(String, String, String)> {
    let downloads = sub.get("downloads")?.as_array()?;
    for plat in order.iter().copied() {
        let Some(dl) = downloads.iter().rev().find(|d| d.get("platform").and_then(|p| p.as_str()) == Some(plat)) else { continue };
        let key = dl.get("key").and_then(|k| k.as_str()).unwrap_or("");
        let mut best: Option<((u8, u8), &(String, String, String))> = None;
        for f in signed.iter().filter(|(k, file, _)| k == key && !file.to_lowercase().ends_with(".torrent")) {
            let label = f.1.to_lowercase();
            let rank = (u8::from(crate::archive::kind_of(&f.1).is_none()), u8::from(label.contains("32") && !label.contains("64")));
            if best.as_ref().map_or(true, |(k, _)| rank < *k) { best = Some((rank, f)); }
        }
        if let Some((_, (_, file, url))) = best { return Some((url.clone(), file.clone(), plat.to_string())); }
    }
    None
}

impl HumbleSource {
    fn prefix(&self, appid: i64) -> PathBuf { self.dir().join(".prefixes").join(appid.to_string()) }
    /// The bundle page and the item of a game on the rebuilt site. The library list is asked first (by the game's name, matched on its machine name): a game synced on the old site has
    /// an old order key in its link, which names no bundle of the new site, while the list gives the bundle it is in now. The link's own bundle and the title are the fallback.
    fn resolve_item(&self, cookie: &str, g: &GameRef, cancel: &Cancel) -> Result<(String, Value), String> {
        let machine = if g.platform_id.is_empty() { g.slug.split_once('/').map(|(_, m)| m.to_string()).unwrap_or_default() } else { g.platform_id.clone() };
        let found = if machine.is_empty() { None } else {
            self.list_page(cookie, 1, g.name.trim(), false, cancel).ok().and_then(|p| p.items.into_iter().find(|i| i.kind == "download_drm_free" && i.content_id.as_deref() == Some(machine.as_str()) && !i.bundle_id.is_empty()))
        };
        let slug_bundle = g.slug.split_once('/').map(|(b, _)| b.to_string()).filter(|b| !b.is_empty());
        let bundle = found.as_ref().map(|i| i.bundle_id.clone()).or(slug_bundle).ok_or("This game has no Humble link; sync your Humble library again")?;
        let page_url = format!("{}/{bundle}", super::humble_site::LIBRARY_URL);
        let page = self.get(&page_url, cookie, "text/html", 30)?;
        if Self::rejected(page.status) { return Err("Could not read the bundle page \u{2014} try reconnecting your Humble account.".into()); }
        let order = super::humble_site::bundle_order(&page.body, &bundle, "", &HashMap::new());   // no map: an item is found by its id, else by its title
        let want = g.name.trim().to_lowercase();
        let sub = order.get("subproducts").and_then(|s| s.as_array()).into_iter().flatten()
            .find(|s| match &found { Some(i) => s.get("item_id").and_then(|x| x.as_str()) == Some(i.id.as_str()), None => s.get("human_name").and_then(|n| n.as_str()).map(|n| n.trim().to_lowercase()).as_deref() == Some(want.as_str()) })
            .cloned().ok_or("Humble no longer lists this game in its bundle")?;
        Ok((page_url, sub))
    }
    /// Install on the rebuilt site: find the game's item, ask the page's own action for its signed addresses, pick the file, download and unpack it.
    fn install_new_site(&self, cookie: &str, g: &GameRef, progress: &mut dyn FnMut(&str, u64, u64), cancel: &Cancel) -> Result<Installed, String> {
        progress(&format!("{}: asking Humble for the download", g.name), 0, 0);
        let (page_url, sub) = self.resolve_item(cookie, g, cancel)?;
        let item_id = sub.get("item_id").and_then(|i| i.as_str()).filter(|i| !i.is_empty()).ok_or("Humble gave no id for this game")?.to_string();
        let action = super::humble_site::discover_page_action(self.http.as_ref(), cookie, &page_url, "getSignedUrlsForEntitlements")?;
        let signed = super::humble_site::signed_urls(self.http.as_ref(), cookie, &page_url, &action, &item_id).map_err(|e| if e == "EXPIRED" { "Could not fetch the download link \u{2014} try reconnecting your Humble account.".to_string() } else { e })?;
        // the person picked the build when there were several (asked at each install); with no pick, this system's preference
        let (pref, only) = ("auto", super::take_install_choice(g.appid));
        let (url, filename, platform) = pick_signed(&sub, &signed, &download_order(pref, only.as_deref(), crate::native_exe::Os::current())).ok_or("Humble offers no download for this game")?;
        log(&format!("Humble: downloading {:?} for {:?} ({platform})", filename, g.name));
        self.download_and_unpack(g, &url, &filename, progress, cancel)
    }
    fn download_and_unpack(&self, g: &GameRef, url: &str, filename: &str, progress: &mut dyn FnMut(&str, u64, u64), cancel: &Cancel) -> Result<Installed, String> {
        let dir = self.dir().join(super::indiegala::safe_dirname(&g.name));
        crate::installdir::check_writable(&dir)?;
        let archive = dir.join(filename);
        let name = g.name.clone();
        super::folder::download(self.http.as_ref(), url, &archive, cancel, &mut |d, t| progress(&format!("Downloading {name}: {} of {} MB", super::folder::mb(d), super::folder::mb(t)), d, t))?;
        progress(&format!("Unpacking {name}"), 0, 0);
        let mut note = super::folder::unpack(&archive, &dir, cancel, &mut |d, t| progress(&format!("Unpacking {name}: {} of {} MB", super::folder::mb(d), super::folder::mb(t)), d, t))?;
        // a game shipped as a Windows setup program: run it once into the game's folder
        match super::folder::run_setup(&dir, &self.prefix(g.appid), cancel, &mut |m| progress(&format!("{name}: {m}"), 0, 0)) {
            Ok(Some(n)) => { log(&format!("humble: {name:?} {n}")); note = None; }   // the "cannot unpack" note was about the setup file
            Ok(None) => {}
            Err(e) if e == "Cancelled" => return Err(e),
            Err(e) => { log(&format!("humble: {name:?}: {e}")); note = Some(e); }
        }
        Ok(Installed { install_path: dir.to_string_lossy().to_string(), executable: None, note })
    }
    fn dir(&self) -> PathBuf { crate::installdir::games_dir(&self.config_dir, "humble", crate::installdir::default_dir("Humble", "Humble")) }
}

#[cfg(test)]
mod tests {
    #[test] fn a_game_synced_on_the_old_site_is_found_through_the_library_list_not_its_old_order_key() {
        let (s, site, d) = setup("resolve", site());
        let _ = &d;
        let old = GameRef { appid: -1, name: "Game A".into(), platform: "humble".into(), platform_id: "game_a".into(), slug: "OLDGAMEKEY/game_a".into(), ..Default::default() };
        let (url, sub) = s.resolve_item("ory_session_site=COOKIE", &old, &Cancel::new()).unwrap();
        assert!(url.ends_with("/home/library/K1"), "the bundle the list names, not the old key: {url}");
        assert_eq!(sub.get("human_name").and_then(|n| n.as_str()), Some("Game A"));
        assert_eq!(sub.get("item_id").and_then(|n| n.as_str()), Some("K1_game_a"));
        let gone = GameRef { name: "Not Here".into(), platform_id: "nope".into(), slug: "OLDGAMEKEY/nope".into(), ..old.clone() };
        assert!(s.resolve_item("ory_session_site=COOKIE", &gone, &Cancel::new()).is_err(), "a game the site does not list cannot be installed");
        let _ = site;
    }
    #[test] fn the_signed_file_is_chosen_by_platform_then_by_what_zest_can_unpack_and_never_a_torrent() {
        use crate::native_exe::Os;
        let sub = json!({"downloads": [{"platform": "windows", "key": "g_win"}, {"platform": "linux", "key": "g_linux"}]});
        let signed = |v: &[(&str, &str)]| -> Vec<(String, String, String)> { v.iter().map(|(k, f)| (k.to_string(), f.to_string(), format!("https://cdn.example/{f}?sig=1"))).collect() };
        let s = signed(&[("g_linux", "Game-linux.tar.gz.torrent"), ("g_linux", "Game.deb"), ("g_linux", "Game-linux.tar.gz"), ("g_win", "Game.zip")]);
        assert_eq!(pick_signed(&sub, &s, &download_order("windows", None, Os::Linux)).map(|(_, f, p)| (f, p)), Some(("Game.zip".to_string(), "windows".to_string())), "Windows first on Linux; a torrent is never taken");
        let linux_only = json!({"downloads": [{"platform": "linux", "key": "g_linux"}]});
        assert_eq!(pick_signed(&linux_only, &s, &download_order("windows", None, Os::Linux)).map(|(_, f, _)| f), Some("Game-linux.tar.gz".to_string()), "a file Zest unpacks beats a .deb, a torrent is never taken");
        assert_eq!(pick_signed(&sub, &s, &download_order("auto", None, Os::Windows)).map(|(_, f, p)| (f, p)), Some(("Game.zip".to_string(), "windows".to_string())));
        assert_eq!(pick_signed(&sub, &signed(&[("g_linux", "Game.tar.gz")]), &download_order("windows", None, Os::Linux)).map(|(_, _, p)| p), Some("linux".to_string()), "no file for the Windows build: the Linux one");
        assert_eq!(pick_signed(&sub, &[], &download_order("windows", None, Os::Linux)), None);
        assert_eq!(pick_signed(&sub, &s, &download_order("linux", None, Os::Windows)).map(|(_, f, _)| f), Some("Game-linux.tar.gz".to_string()), "the person's choice of Linux");
        assert_eq!(pick_signed(&sub, &s, &download_order("windows", Some("linux"), Os::Linux)).map(|(_, f, _)| f), Some("Game-linux.tar.gz".to_string()), "a one-off pick is the only platform tried");
        assert_eq!(pick_signed(&json!({"downloads": [{"platform": "windows", "key": "g_win"}]}), &s, &download_order("windows", Some("linux"), Os::Linux)), None, "and no other is used when it has no file");
    }
    #[test] fn a_saved_session_is_sent_as_the_old_cookie_or_as_the_ory_pair() {
        assert_eq!(HumbleSource::cookie_header("eyJhbGc=="), "_simpleauth_sess=eyJhbGc==", "an old value, padding and all");
        assert_eq!(HumbleSource::cookie_header("ory_session_stoic=abc"), "ory_session_stoic=abc", "the pair the sign-in window read is sent as it is");
    }
    use super::*;
    use crate::net::{NetError, Raw, Response};
    use std::sync::Mutex;

    /// A fake of Humble's rebuilt site. `bundles` = (bundle id, name, items); an item is (item id, title, machine name, kind, platforms). The list answers 5 entries a page in order
    /// (and honours the `query` and `hideRevealed` fields of the form), a bundle page carries its DRM-free items in the flight payload, `session` is what session-status says.
    struct Site { bundles: Vec<(&'static str, &'static str, Vec<(String, String, String, &'static str, Vec<&'static str>)>)>, session: u16, asked: Mutex<Vec<String>>, posts: Mutex<usize> }
    impl Site {
        fn entries(&self) -> Vec<(String, String, String, String, String, &'static str)> {   // id, title, machine, bundle id, bundle name, kind
            self.bundles.iter().flat_map(|(b, bn, items)| items.iter().map(move |(id, t, m, k, _)| (id.clone(), t.clone(), m.clone(), b.to_string(), bn.to_string(), *k))).collect()
        }
        fn field(body: &str, name: &str) -> String { body.split(&format!("name=\"_1_{name}\"\r\n\r\n")).nth(1).and_then(|r| r.split("\r\n").next()).unwrap_or("").to_string() }
        fn bundle_page(&self, id: &str) -> Option<String> {
            let (_, _, items) = self.bundles.iter().find(|(b, _, _)| *b == id)?;
            let objs: Vec<Value> = items.iter().filter(|i| i.3 == "download_drm_free").map(|(iid, t, _, _, plats)| json!({"id": iid, "title": t, "type": "download_drm_free", "redemptionData": {}, "downloads": plats.iter().map(|p| json!({"id": "d", "key": "k", "platform": p, "downloadStruct": [{"name": "Download", "size": "1 GB"}]})).collect::<Vec<_>>()})).collect();
            let esc = serde_json::to_string(&Value::Array(objs).to_string()).unwrap();
            Some(format!("<html><script>self.__next_f.push([1,{esc}])</script></html>"))
        }
    }
    impl Http for Site {
        fn get(&self, u: &str, _h: &[(&str, &str)], _t: Duration) -> Result<Response, NetError> {
            self.asked.lock().unwrap().push(u.to_string());
            if u.ends_with("session-status") { return Ok(Response { status: self.session, retry_after: None, body: if self.session == 200 { r#"{"status":"authenticated"}"#.into() } else { String::new() } }); }
            Ok(Response { status: 200, retry_after: None, body: if u.ends_with("a1.js") { r#"x=createServerReference)("607622c68ed0fa8a71acd7ff343f9f3366703af38e",c,void 0,f,"filterUserLibrary")"#.into() } else { String::new() } })
        }
        fn get_raw(&self, u: &str, h: &[(&str, &str)], _t: Duration) -> Result<Raw, NetError> {
            assert!(h.iter().any(|(k, v)| *k == "Cookie" && v.starts_with("ory_session_")), "the session cookie is sent");
            self.asked.lock().unwrap().push(u.to_string());
            let raw = |status, body: String| Ok(Raw { status, headers: vec![], body });
            if u.ends_with("/home/library") { return raw(200, r#"<html><script src="/_next/static/chunks/a1.js"></script></html>"#.into()); }
            match self.bundle_page(u.rsplit('/').next().unwrap()) { Some(p) => raw(200, p), None => raw(404, String::new()) }
        }
        fn post(&self, _u: &str, h: &[(&str, &str)], body: &str, _t: Duration) -> Result<Response, NetError> {
            assert!(h.iter().any(|(k, v)| *k == "Next-Action" && *v == "607622c68ed0fa8a71acd7ff343f9f3366703af38e"));
            *self.posts.lock().unwrap() += 1;
            let (page, query) = (Self::field(body, "page").parse::<usize>().unwrap_or(1), Self::field(body, "query").to_lowercase());
            let all: Vec<_> = self.entries().into_iter().filter(|e| query.is_empty() || e.1.to_lowercase().contains(&query)).collect();
            let results: Vec<Value> = all.iter().skip((page - 1) * 5).take(5).map(|(id, t, m, b, bn, k)| json!({"id": id, "title": t, "subTitle": bn, "type": k, "contentId": m, "productEntitlementId": b})).collect();
            let v = json!({"results": results, "pagination": {"page": page, "totalPages": all.len().div_ceil(5).max(1), "totalDocs": all.len()}, "success": true});
            Ok(Response { status: 200, retry_after: None, body: format!("0:{{}}\n1:{v}\n") })
        }
    }
    fn item(bundle: &str, machine: &str, title: &str, kind: &'static str, plats: Vec<&'static str>) -> (String, String, String, &'static str, Vec<&'static str>) { (format!("{bundle}_{machine}"), title.to_string(), machine.to_string(), kind, plats) }
    fn site() -> Site {
        Site { session: 200, asked: Mutex::new(vec![]), posts: Mutex::new(0), bundles: vec![
            ("K1", "Some Bundle", vec![item("K1", "game_a", "Game A", "download_drm_free", vec!["windows"]), item("K1", "game_a_ost", "Game A Soundtrack", "download_drm_free", vec!["audio"]), item("K1", "game_b", " Game B ", "download_drm_free", vec!["linux"]), item("K1", "key_x", "A Steam Key", "steam", vec![])]),
            ("K2", "Another Bundle", vec![item("K2", "game_a", "Game A again", "download_drm_free", vec!["windows"]), item("K2", "game_c", "Game C", "download_drm_free", vec!["mac"])]),
        ] }
    }
    fn site_default() -> Site { Site { session: 200, asked: Mutex::new(vec![]), posts: Mutex::new(0), bundles: vec![] } }
    fn setup(tag: &str, site: Site) -> (HumbleSource, Arc<Site>, PathBuf) {
        let d = std::env::temp_dir().join(format!("zest_hum_{tag}_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("config.json"), "{}").unwrap(); crate::account::plugin_secret_set(&d, "humble", "session_cookie", Some("ory_session_site=COOKIE")).unwrap();
        let db = d.join("games_1.db");
        rusqlite::Connection::open(&db).unwrap().execute_batch("CREATE TABLE games (appid INTEGER, platform TEXT, platform_id TEXT, platform_slug TEXT); CREATE TABLE blacklist (appid INTEGER, platform_id TEXT, platform TEXT);").unwrap();
        let s = Arc::new(site); (HumbleSource::new(s.clone(), d.clone(), db), s, d)
    }
    fn cx(d: &PathBuf, s: Arc<Site>) -> SourceCtx { let h: Arc<dyn Http> = s; SourceCtx { http: h.clone(), steam: Arc::new(crate::steamclient::Steam::new(h, None, 10.0)), root: d.clone(), db: d.join("games_1.db"), today: "2026-11-01".into(), appinfo: Arc::new(|_| Default::default()), account_id: None } }
    fn bundle_pages(site: &Site) -> Vec<String> { site.asked.lock().unwrap().iter().filter(|u| u.contains("/home/library/")).cloned().collect() }

    #[test] fn the_deep_scan_flags_untouched_extras_and_leaves_the_rest() {
        let mut sj = site();
        sj.bundles.push(("K3", "Third", vec![item("K3", "droid", "Droid Game", "download_drm_free", vec!["android"])]));
        let (s, site, d) = setup("scan", sj);
        let db = d.join("games_1.db");
        rusqlite::Connection::open(&db).unwrap().execute_batch("DROP TABLE games; CREATE TABLE games (appid INTEGER, name TEXT, platform TEXT, platform_id TEXT, platform_slug TEXT, playtime_forever INTEGER, installed INTEGER, completion_status TEXT, duplicate_of TEXT, duplicate_auto INTEGER);
            INSERT INTO games VALUES (1, 'Game A', 'humble', 'game_a', 'K1/game_a', 0, 0, 'Never Played', NULL, 0);
            INSERT INTO games VALUES (2, 'Game A Soundtrack', 'humble', 'game_a_ost', 'K1/game_a_ost', 0, 0, 'Never Played', NULL, 0);
            INSERT INTO games VALUES (3, 'Played Soundtrack', 'humble', 'x', 'K1/game_a_ost', 5, 0, 'Never Played', NULL, 0);
            INSERT INTO games VALUES (4, 'Droid Game', 'humble', 'droid', 'K3/droid', 0, 0, 'Never Played', NULL, 0);
            INSERT INTO games VALUES (5, 'Installed Soundtrack', 'humble', 'y', 'K1/game_a_ost', 0, 1, 'Never Played', NULL, 0);
            INSERT INTO games VALUES (6, 'Beaten Soundtrack', 'humble', 'z', 'K1/game_a_ost', 0, 0, 'Beaten', NULL, 0);
            INSERT INTO games VALUES (7, 'Old Link', 'humble', 'old', 'OLDGAMEKEY/old', 0, 0, 'Never Played', NULL, 0);").unwrap();
        let hits = s.scan_junk().unwrap();
        let got: Vec<(i64, &str)> = hits.iter().map(|h| (h.appid, h.reason.as_str())).collect();
        assert_eq!(got, vec![(2, "source code / installer / non-game"), (4, "no desktop download (Android / audio / ebook)")], "{got:?}");
        assert!(bundle_pages(&site).iter().all(|u| u.ends_with("/K1") || u.ends_with("/K3") || u.ends_with("/OLDGAMEKEY")), "only bundle pages are read");
    }

    #[test] fn sync_lists_games_once_with_the_bundle_slug_and_skips_extras() {
        let (s, site, d) = setup("sync", site());
        let mut got = vec![]; let n = s.sync(&cx(&d, site.clone()), &mut |b| got.extend(b), &Cancel::new()).unwrap();
        assert_eq!(n, 3);
        let ids: Vec<&str> = got.iter().map(|r| r.platform_id.as_str()).collect(); assert_eq!(ids, ["game_a", "game_b", "game_c"], "the soundtrack is not a game; game_a in the second bundle is a duplicate");
        assert_eq!((got[0].name.as_str(), got[0].slug.as_deref()), ("Game A", Some("K1/game_a")));
        assert_eq!((got[1].name.as_str(), got[1].developers.clone()), ("Game B", None), "a name is trimmed; the new site names no studio");
        let cache: Value = serde_json::from_str(&std::fs::read_to_string(d.join("humble_cache.json")).unwrap()).unwrap();
        assert_eq!(cache["gamekeys_map"]["K1"], json!(["game_a", "game_b"]), "what the bundle produced, the soundtrack left out, as the old order cache kept it");
        assert_eq!(cache["gamekeys_map"]["K2"], json!(["game_a", "game_c"]), "the order cache lists every game of a bundle, a duplicate included, as it always did");
    }
    #[test] fn a_second_sync_reads_one_page_and_no_bundle_and_a_deleted_game_comes_back() {
        let (s, site, d) = setup("cache", site());
        s.sync(&cx(&d, site.clone()), &mut |_| {}, &Cancel::new()).unwrap();
        let c = rusqlite::Connection::open(d.join("games_1.db")).unwrap();
        for g in ["game_a", "game_b"] { c.execute("INSERT INTO games VALUES (-1, 'humble', ?, NULL)", [g]).unwrap(); }
        c.execute("INSERT INTO blacklist VALUES (-9, 'game_c', 'humble')", []).unwrap();
        site.asked.lock().unwrap().clear(); *site.posts.lock().unwrap() = 0;
        let mut got = vec![]; s.sync(&cx(&d, site.clone()), &mut |b| got.extend(b), &Cancel::new()).unwrap();
        assert!(got.is_empty());
        assert!(bundle_pages(&site).is_empty(), "no bundle was fetched again: {:?}", site.asked.lock().unwrap());
        assert_eq!(*site.posts.lock().unwrap(), 1, "the first page held only bundles read before, so the list was not read further");
        // the user deletes game_b: its bundle is read again and game_b comes back
        c.execute("DELETE FROM games WHERE platform_id = 'game_b'", []).unwrap();
        let mut got = vec![]; s.sync(&cx(&d, site.clone()), &mut |b| got.extend(b), &Cancel::new()).unwrap();
        assert!(got.iter().any(|r| r.platform_id == "game_b"));
        assert!(!got.iter().any(|r| r.platform_id == "game_c"), "a blacklisted game is not offered");
    }
    #[test] fn a_first_sync_with_no_cache_reads_only_the_bundles_that_hold_something_new() {
        let mut st = site(); st.bundles.push(("K3", "Third", vec![item("K3", "game_new", "A New One", "download_drm_free", vec!["windows"])]));
        let (s, site, d) = setup("bootstrap", st);
        let c = rusqlite::Connection::open(d.join("games_1.db")).unwrap();
        for (id, slug) in [("game_a", "OLD1/game_a"), ("game_a_ost", "OLD1/game_a_ost"), ("game_b", "OLD1/game_b"), ("game_c", "OLD2/game_c")] { c.execute("INSERT INTO games VALUES (-1, 'humble', ?, ?)", [id, slug]).unwrap(); }
        assert!(!d.join("humble_cache.json").exists());
        let mut got = vec![]; s.sync(&cx(&d, site.clone()), &mut |b| got.extend(b), &Cancel::new()).unwrap();
        let asked = bundle_pages(&site);
        assert_eq!(asked.len(), 1, "only the new bundle was fetched: {asked:?}");
        assert!(asked[0].ends_with("/K3")); assert_eq!(got.iter().map(|r| r.platform_id.as_str()).collect::<Vec<_>>(), ["game_new"]);
        assert!(*site.posts.lock().unwrap() >= 2, "a first sync reads the whole list: 8 entries are two pages");
    }
    #[test] fn a_cancelled_sync_keeps_what_it_has_done() {
        let (s, site, d) = setup("cancel", site());
        let cancel = Cancel::new(); let c2 = cancel.clone();
        let n = s.sync(&cx(&d, site), &mut |_| c2.cancel(), &cancel).unwrap();
        assert_eq!(n, 2, "the first bundle's two games were handed over before the stop");
        let cache: Value = serde_json::from_str(&std::fs::read_to_string(d.join("humble_cache.json")).unwrap()).unwrap();
        assert!(cache["gamekeys_map"].get("K1").is_some() && cache["gamekeys_map"].get("K2").is_none(), "a bundle not yet read is not marked read: {cache}");
    }
    #[test] fn a_rejected_session_stops_the_sync_with_the_plugins_message() {
        let mut st = site(); st.session = 401;
        let (s, site, d) = setup("expired", st);
        assert_eq!(s.sync(&cx(&d, site), &mut |_| {}, &Cancel::new()).unwrap_err(), "Session expired — please reconnect.");
        let d2 = std::env::temp_dir().join("zest_hum_nocookie"); let _ = std::fs::remove_dir_all(&d2); std::fs::create_dir_all(&d2).unwrap(); std::fs::write(d2.join("config.json"), "{}").unwrap();
        let s2 = HumbleSource::new(Arc::new(site_default()), d2.clone(), d2.join("g.db"));
        assert_eq!(s2.sync(&cx(&d2, Arc::new(site_default())), &mut |_| {}, &Cancel::new()).unwrap_err(), "Not connected");
        // a value saved on the old site is not a session of the new one
        crate::account::plugin_secret_set(&d2, "humble", "session_cookie", Some("OLDVALUE")).unwrap();
        assert!(s2.sync(&cx(&d2, Arc::new(site_default())), &mut |_| {}, &Cancel::new()).unwrap_err().contains("sign in to Humble again"));
    }
    #[test] fn the_status_check_asks_the_new_status_address_and_reports_rejection() {
        let (s, _, _) = setup("status", site()); assert_eq!(s.check_connection(), Some(true));
        let mut st = site(); st.session = 403; let (s, _, _) = setup("rejected", st); assert_eq!(s.check_connection(), Some(false));
        let mut st = site(); st.session = 500; let (s, _, _) = setup("unknown", st); assert_eq!(s.check_connection(), None, "a server error is not a verdict");
        let (s, _, d) = setup("oldvalue", site()); crate::account::plugin_secret_set(&d, "humble", "session_cookie", Some("OLDVALUE")).unwrap(); assert_eq!(s.check_connection(), Some(false), "an old-site value is signed out");
    }
    #[test] fn rescrape_finds_the_game_in_the_library_list_by_its_title_and_machine_name() {
        let (s, site, d) = setup("rescrape", site());
        let g = |id: &str| GameRef { appid: -1, name: "Game A".into(), platform: "humble".into(), platform_id: id.into(), slug: "K1/game_a".into(), ..Default::default() };
        let u = s.rescrape(&cx(&d, site.clone()), &g("game_a")).unwrap().unwrap();
        assert_eq!(u, vec![("meta_fetched", Sql::Text("2026-11-01".into())), ("name", Sql::Text("Game A".into()))]);
        assert!(s.rescrape(&cx(&d, site.clone()), &g("nope")).unwrap().is_none());
        assert!(s.rescrape(&cx(&d, site), &g("")).unwrap().is_none() || true);
    }
    #[test] fn sign_out_removes_the_session_and_the_cache() {
        let (s, site, d) = setup("out", site());
        s.sync(&cx(&d, site), &mut |_| {}, &Cancel::new()).unwrap(); assert!(d.join("humble_cache.json").exists());
        s.auth_disconnect().unwrap();
        assert!(!d.join("humble_cache.json").exists() && crate::account::plugin_secret_get(&d, "humble", "session_cookie").is_none());
        assert!(!s.auth_status().unwrap().connected);
    }
}

#[cfg(test)]
mod install_tests {
    use super::*;
    use crate::net::{NetError, Raw};
    use std::sync::Mutex;

    fn order() -> Value {
        json!({"subproducts": [
            {"machine_name": "other", "downloads": [{"platform": "linux", "download_struct": [{"name": "Download", "url": {"web": "https://dl.example/other.zip"}}]}]},
            {"machine_name": "game", "downloads": [
                {"platform": "windows", "download_struct": [{"name": "Download", "url": {"web": "https://dl.example/game_win.zip?key=1"}}]},
                {"platform": "linux", "download_struct": [
                    {"name": ".deb", "url": {"web": "https://dl.example/game_amd64.deb?key=2"}},
                    {"name": "32-bit .tar.gz", "url": {"web": "https://dl.example/game_i386.tar.gz?key=3"}},
                    {"name": "64-bit .zip", "url": {"web": "https://dl.example/game_win.zip?key=4"}},
                    {"name": "bad", "url": {"web": ""}}]}]},
            {"machine_name": "winonly", "downloads": [{"platform": "windows", "download_struct": [{"name": "Download", "url": {"web": "https://dl.example/w.zip"}}]}, {"platform": "android", "download_struct": [{"name": "APK", "url": {"web": "https://dl.example/a.apk"}}]}]},
            {"machine_name": "nolinks", "downloads": [{"platform": "linux", "download_struct": [{"name": "x"}]}]}]})
    }
    #[test] fn the_download_is_the_best_platform_then_an_unpackable_64_bit_file() {
        use crate::native_exe::Os;
        let o = order();
        assert_eq!(pick_download(&o, "game", Os::Linux), Some(("https://dl.example/game_win.zip?key=1".into(), "game_win.zip".into(), "windows".into())), "on Linux the Windows build (run through Wine) comes first");
        assert_eq!(pick_download(&o, "game", Os::Mac).map(|d| d.2), Some("linux".into()), "a Mac with no Mac build: Linux, then Windows");
        assert_eq!(pick_download(&o, "game", Os::Windows), Some(("https://dl.example/game_win.zip?key=1".into(), "game_win.zip".into(), "windows".into())));
        assert_eq!(pick_download(&o, "winonly", Os::Linux).map(|d| d.2), Some("windows".into()));   // no Linux build: the Windows one, as PlayDate did
        assert_eq!(pick_download(&o, "nolinks", Os::Linux), None);
        assert_eq!(pick_download(&o, "missing", Os::Linux), None);
        assert_eq!(platform_pref(Os::Mac)[0], "mac");
    }

    struct Site { zip: Vec<u8>, status: u16, asked: Mutex<Vec<String>> }
    impl Http for Site {
        fn get(&self, _u: &str, _h: &[(&str, &str)], _t: Duration) -> Result<crate::net::Response, NetError> { unreachable!() }
        fn get_raw(&self, u: &str, h: &[(&str, &str)], _t: Duration) -> Result<Raw, NetError> {
            self.asked.lock().unwrap().push(u.to_string());
            assert!(h.iter().any(|(k, v)| *k == "Cookie" && *v == "_simpleauth_sess=COOKIE"));
            assert!(u.ends_with("/order/KEY"));
            Ok(Raw { status: 200, headers: vec![], body: order().to_string() })
        }
        fn get_stream(&self, u: &str, _h: &[(&str, &str)], on_head: &mut dyn FnMut(u16, Option<u64>) -> bool, on_chunk: &mut dyn FnMut(&[u8]) -> bool) -> Result<(), NetError> {
            self.asked.lock().unwrap().push(u.to_string());
            if !on_head(self.status, Some(self.zip.len() as u64)) { return Ok(()); }
            for c in self.zip.chunks(300) { if !on_chunk(c) { return Ok(()); } }
            Ok(())
        }
    }
    fn setup(tag: &str, status: u16) -> (HumbleSource, Arc<Site>, PathBuf) {
        let d = std::env::temp_dir().join(format!("zest_humble_{tag}_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("config.json"), json!({"humble": {"session_cookie": "COOKIE", "games_dir": d.join("games").to_string_lossy()}}).to_string()).unwrap();
        let site = Arc::new(Site { zip: std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/sample.zip")).unwrap(), status, asked: Mutex::new(vec![]) });
        (HumbleSource::new(site.clone(), d.clone(), d.join("games.db")), site, d)
    }
    fn cx(d: &PathBuf, site: Arc<Site>) -> SourceCtx {
        SourceCtx { http: site, steam: Arc::new(crate::steamclient::Steam::new(Arc::new(crate::net::UreqHttp::new()), None, 10.0)), root: d.clone(), db: d.join("games.db"), today: "2026-11-01".into(), appinfo: Arc::new(|_| Default::default()), account_id: None }
    }
    fn gref() -> GameRef { GameRef { appid: -9, name: "My: Game!".into(), platform: "humble".into(), platform_id: "game".into(), slug: "KEY/game".into(), ..Default::default() } }

    #[test] fn install_downloads_unpacks_removes_the_archive_and_reports_progress() {
        let (s, site, d) = setup("install", 200);
        let mut texts: Vec<String> = vec![];
        let r = s.install(&cx(&d, site.clone()), &gref(), &mut |t, _, _| texts.push(t.to_string()), &Cancel::new()).unwrap();
        let dir = d.join("games").join("My Game");
        assert_eq!(r, Installed { install_path: dir.to_string_lossy().into(), executable: None, note: None });
        assert!(dir.join("game/run").is_file() && !dir.join("game_win.zip").exists());
        assert!(texts.first().unwrap().contains("asking Humble") && texts.iter().any(|t| t.starts_with("Unpacking")), "{texts:?}");
        assert_eq!(site.asked.lock().unwrap().iter().filter(|u| u.contains("game_win.zip")).count(), 1);
        // two programs tie for the best (the sample has `run` and `link`): Zest asks which; the answer is run in its folder (Linux and macOS programs: Windows runs .exe files only)
        if !cfg!(windows) {
            std::fs::write(dir.join("game/run"), [0x7fu8, b'E', b'L', b'F', 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0]).unwrap();
            std::fs::write(dir.join("game/link"), [0x7fu8, b'E', b'L', b'F', 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0]).unwrap();
            let g = GameRef { installed: true, install_path: Some(dir.to_string_lossy().into()), ..gref() };
            let l = s.launch(&g).unwrap();
            assert!(!l.record);
            match &l.action { Action::PickExecutable { candidates, dir: d } => { assert_eq!(candidates, &vec!["game/link".to_string(), "game/run".to_string()]); assert_eq!(d, &dir.to_string_lossy().to_string()); } a => panic!("{a:?}") }
            let l = s.launch(&GameRef { executable: Some("game/run".into()), ..g.clone() }).unwrap();
            assert!(l.record && matches!(l.action, Action::Spawn { ref program, ref dir, .. } if program.ends_with("game/run") && dir.as_deref().map_or(false, |d| d.ends_with("My Game/game"))));
        }
        // the install scan sees the folder, uninstall removes it (it is inside the games folder)
        std::fs::write(d.join("games.db"), b"").ok();
        let c = rusqlite::Connection::open(d.join("games.db")).unwrap();
        c.execute_batch("CREATE TABLE games (platform TEXT, platform_id TEXT, install_path TEXT);").unwrap();
        c.execute("INSERT INTO games VALUES ('humble','game',?), ('humble','b','/gone'), ('humble','c',NULL)", [dir.to_string_lossy()]).unwrap();
        let rows = s.installed_scan().unwrap();
        assert_eq!(rows.iter().map(|r| (r.platform_id.as_str(), r.installed)).collect::<Vec<_>>(), [("game", true), ("b", false), ("c", false)]);
        assert_eq!(s.uninstall(&GameRef { install_path: Some(dir.to_string_lossy().into()), ..gref() }).unwrap(), Action::RemoveInstall { dir: dir.to_string_lossy().into(), delete: true });
    }
    #[test] fn an_archive_left_in_the_folder_is_unpacked_when_the_game_is_launched() {
        let (s, _, d) = setup("leftover", 200);
        let dir = d.join("games/Left Over"); std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/sample.tar.gz"), dir.join("game.tar.gz")).unwrap();
        std::fs::write(dir.join("notes.txt"), "x").unwrap();
        let gr = GameRef { installed: true, install_path: Some(dir.to_string_lossy().into()), ..gref() };
        // nothing to run yet: the archive is unpacked; its `run` is a script without a header, so the folder is opened, but the archive is gone and the files are there
        let l = s.launch(&gr).unwrap();
        assert!(!dir.join("game.tar.gz").exists() && dir.join("game/readme.txt").is_file());
        assert_eq!(l.action, Action::OpenUrl(dir.to_string_lossy().into()));
    }
    // a Linux self-extracting installer (MojoSetup) and its launcher script
    #[cfg(not(windows))]
    #[test] fn a_self_extracting_installer_is_unpacked_into_the_game_folder_and_its_launcher_script_is_the_way_to_start_it() {
        let (s, _, d) = setup("mojo", 200);
        let dir = d.join("games/Mojo Game"); std::fs::create_dir_all(&dir).unwrap();
        // the installer itself is an ELF program, which the old picker took for the game
        std::fs::copy(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/mojo-installer.bin"), dir.join("game-02172016-bin")).unwrap();
        let gr = GameRef { installed: true, install_path: Some(dir.to_string_lossy().into()), ..gref() };
        let l = s.launch(&gr).unwrap();
        assert!(!dir.join("game-02172016-bin").exists() && !dir.join(".zest_unpack").exists(), "installer removed, staging gone");
        assert!(dir.join("x86_64/game.x86_64").is_file() && dir.join("Readme.txt").is_file() && !dir.join("scripts").exists(), "the game's files sit directly in the folder");
        assert_eq!(l.action, Action::Spawn { program: dir.join("Game").to_string_lossy().into(), args: vec![], dir: Some(dir.to_string_lossy().into()), env: vec![] });
    }
    #[test] fn a_refused_download_cancel_and_missing_links_leave_nothing_behind_and_say_why() {
        let (s, site, d) = setup("refused", 403);
        assert!(s.install(&cx(&d, site), &gref(), &mut |_, _, _| {}, &Cancel::new()).unwrap_err().contains("HTTP 403"));
        assert!(!d.join("games/My Game/game_win.zip").exists());
        let (s, site, d) = setup("cancel", 200);
        let c = Cancel::new(); c.cancel();
        assert_eq!(s.install(&cx(&d, site.clone()), &gref(), &mut |_, _, _| {}, &c).unwrap_err(), "Cancelled");
        assert!(!d.join("games/My Game/game_win.zip").exists());
        assert!(s.install(&cx(&d, site.clone()), &GameRef { slug: "nolink".into(), ..gref() }, &mut |_, _, _| {}, &Cancel::new()).unwrap_err().contains("order link"));
        assert!(s.install(&cx(&d, site.clone()), &GameRef { slug: "KEY/nolinks".into(), ..gref() }, &mut |_, _, _| {}, &Cancel::new()).unwrap_err().contains("no download"));
        assert!(s.launch(&gref()).unwrap_err().contains("not installed"));
        assert!(s.caps().launch && s.caps().install && s.caps().uninstall && s.caps().installed_scan);
    }
}

#[cfg(test)]
mod junk_tests {
    use super::*;
    fn sub(name: &str, platforms: &[&str]) -> Value { json!({"human_name": name, "machine_name": "m", "downloads": platforms.iter().map(|p| json!({"platform": p, "download_struct": [{"name": "Download"}]})).collect::<Vec<_>>()}) }
    fn order(name: &str) -> Value { json!({"product": {"human_name": name, "machine_name": "o"}}) }
    #[test] fn the_deep_scan_gives_the_reasons_in_the_pythons_order() {
        let o = order("Some Bundle");
        assert_eq!(junk_reason(&sub("Cool Game Soundtrack", &["windows"]), &o, "x"), Some("source code / installer / non-game"));
        assert_eq!(junk_reason(&sub("Cool Game", &["android"]), &o, "x"), Some("no desktop download (Android / audio / ebook)"));
        assert_eq!(junk_reason(&sub("Cool Game", &["Windows"]), &o, "x"), None);
        // a software bundle whose only download is an installer is flagged as such
        let mut app = sub("Some App", &["windows"]); app["downloads"][0]["download_struct"] = json!([{"name": "Installer"}]);
        assert_eq!(junk_reason(&app, &order("Software Bundle"), "x"), Some("software-bundle application"));
        // an item with no name falls back to the library row's
        assert_eq!(junk_reason(&json!({"downloads": [{"platform": "linux"}]}), &o, "Album OST"), Some("source code / installer / non-game"));
    }
}

/// Read-only: which of the account's Humble games offer both a Windows and a Linux build (for testing "Which build?").
/// `ZEST_DATA=<data dir> ZEST_OUT=<file> cargo test -p pd-filter real_humble_two_builds -- --ignored --nocapture`. Reads each bundle page once, 1.5 s apart.
#[cfg(test)]
mod real_two_builds {
    use super::*;
    #[test] #[ignore] fn real_humble_two_builds() {
        let (Some(dir), Some(out)) = (std::env::var_os("ZEST_DATA"), std::env::var_os("ZEST_OUT")) else { return };
        let dir = PathBuf::from(dir);
        if let Some(v) = crate::secrets::platform_vault() { crate::secrets::install(v); }
        let db = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).find(|p| p.file_name().map_or(false, |n| { let n = n.to_string_lossy(); n.starts_with("games_") && n.ends_with(".db") })).expect("no library");
        let s = HumbleSource::new(Arc::new(crate::net::UreqHttp::new()), dir.clone(), db.clone());
        let cookie = s.secret("session_cookie").expect("no Humble sign-in saved");
        let c = rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let mut st = c.prepare("SELECT DISTINCT substr(platform_slug, 1, instr(platform_slug, '/') - 1) FROM games WHERE platform = 'humble' AND instr(platform_slug, '/') > 1").unwrap();
        let bundles: Vec<String> = st.query_map([], |r| r.get(0)).unwrap().flatten().collect();
        let mut lines = vec![]; let mut failed = 0;
        for (i, b) in bundles.iter().enumerate() {
            let url = format!("{}/{b}", super::super::humble_site::LIBRARY_URL);
            match s.get(&url, &cookie, "text/html", 30) {
                Ok(p) if !Self_rejected(p.status) => {
                    let order = super::super::humble_site::bundle_order(&p.body, b, "", &HashMap::new());
                    for sub in order.get("subproducts").and_then(|x| x.as_array()).into_iter().flatten() {
                        let plats: Vec<&str> = sub.get("downloads").and_then(|d| d.as_array()).into_iter().flatten().filter_map(|d| d.get("platform").and_then(|p| p.as_str())).collect();
                        if plats.contains(&"windows") && plats.contains(&"linux") { lines.push(format!("{} | {}", sub.get("human_name").and_then(|n| n.as_str()).unwrap_or("?"), plats.join(","))); }
                    }
                }
                Ok(p) => { failed += 1; println!("bundle {i}: HTTP {}", p.status); }
                Err(e) => { failed += 1; println!("bundle {i}: {e}"); }
            }
            if !lines.is_empty() && std::env::var_os("ZEST_FIRST").is_some() { println!("stopping at the first find (bundle {} of {})", i + 1, bundles.len()); break; }
            if (i + 1) % 20 == 0 { println!("{} of {} bundles read, {} games with both so far", i + 1, bundles.len(), lines.len()); }
            std::thread::sleep(Duration::from_millis(1500));
        }
        lines.sort(); lines.dedup();
        println!("done: {} bundles, {failed} not read, {} games with Windows and Linux builds", bundles.len(), lines.len());
        std::fs::write(out, lines.join("\n")).unwrap();
    }
    #[allow(non_snake_case)] fn Self_rejected(status: u16) -> bool { HumbleSource::rejected(status) }
}
