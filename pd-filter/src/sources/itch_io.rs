//! itch.io as a library source (port of PlayDate's `itch_io` plugin: `itch_io.py`, `__init__.py` and the sign-in route in `routes.py`; install, launch and uninstall wait for the
//! shared runners layer). The user generates an API key on itch.io and pastes it; the library is the account's owned download keys (`api.itch.io/profile/owned-keys`), metadata comes
//! from the same key's game record, and genre, tags and release date from the game's own page for a game no Steam or PCGamingWiki page knows. The key lives in
//! `config.json["itch_io"]` as PlayDate keeps it and is never logged (it is part of some request addresses, which `net::log` strips anyway).
use super::{Action, AuthBegin, AuthStatus, Caps, GameRef, Installed, InstalledRow, Launch, LibrarySource, SourceCtx};
use crate::importer::GameRecord;
use crate::jobs::Update;
use crate::metadata::PageMeta;
use crate::net::{log, set_activity, Cancel, Http};
use crate::steamclient::Fetch;
use regex::Regex;
use rusqlite::types::Value as Sql;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

const API: &str = "https://itch.io/api/1";
const API_NEW: &str = "https://api.itch.io";
const KEYS_URL: &str = "https://itch.io/user/settings/api-keys";
const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";
const REJECTED: &str = "API key rejected \u{2014} please reconnect";

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex { cell.get_or_init(|| Regex::new(pat).unwrap()) }
fn pause(ms: u64) { if !cfg!(test) { std::thread::sleep(Duration::from_millis(ms)); } }
fn ua() -> String { format!("Zest/{} (+https://github.com/RobbyRatpoison/zest-library-manager)", env!("CARGO_PKG_VERSION")) }
/// Python's `str(x)` for a JSON id.
fn id_str(v: Option<&Value>) -> String { match v { Some(Value::String(s)) => s.clone(), Some(Value::Number(n)) => n.to_string(), _ => String::new() } }
fn text<'a>(v: &'a Value, k: &str) -> &'a str { v.get(k).and_then(|x| x.as_str()).unwrap_or("") }

// ---------------------------------------------------------------- pure helpers (checked against the Python)

fn days_in_month(y: i64, m: i64) -> i64 { match m { 1 | 3 | 5 | 7 | 8 | 10 | 12 => 31, 4 | 6 | 9 | 11 => 30, _ => if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 { 29 } else { 28 } } }
/// Seconds since 1970 for a UTC calendar time; None when the date does not exist.
pub(crate) fn utc_ts(y: i64, mo: i64, d: i64, h: i64, mi: i64, s: i64) -> Option<i64> {
    if !(1..=12).contains(&mo) || d < 1 || d > days_in_month(y, mo) || h > 23 || mi > 59 || s > 59 || y < 1 { return None; }
    let (yy, mm) = if mo <= 2 { (y - 1, mo + 9) } else { (y, mo - 3) };
    let era = yy.div_euclid(400); let yoe = yy - era * 400; let doy = (153 * mm + 2) / 5 + d - 1; let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146097 + doe - 719468) * 86400 + h * 3600 + mi * 60 + s)
}

/// `_parse_date`: the API's `created_at` / `published_at` (`2020-05-01T12:30:00.000000Z`, `...Z`, or `YYYY-MM-DD HH:MM:SS`), read as UTC.
pub fn parse_date(s: &str) -> Option<i64> {
    static A: OnceLock<Regex> = OnceLock::new(); static B: OnceLock<Regex> = OnceLock::new();
    let c = re(&A, r"^(\d{4})-(\d{1,2})-(\d{1,2})T(\d{1,2}):(\d{1,2}):(\d{1,2})(?:\.\d{1,6})?Z$").captures(s).or_else(|| re(&B, r"^(\d{4})-(\d{1,2})-(\d{1,2}) (\d{1,2}):(\d{1,2}):(\d{1,2})$").captures(s))?;
    let n = |i: usize| c[i].parse::<i64>().unwrap_or(0);
    utc_ts(n(1), n(2), n(3), n(4), n(5), n(6))
}

const MONTHS: [&str; 12] = ["january", "february", "march", "april", "may", "june", "july", "august", "september", "october", "november", "december"];
fn month_of(name: &str, full: bool) -> Option<i64> {
    let n = name.to_lowercase();
    MONTHS.iter().position(|m| if full { *m == n } else { m[..3] == n }).map(|i| i as i64 + 1)
}

/// `_parse_page_date`: the page's release date (`Oct 12, 2020`, `October 12, 2020`, `Oct 2020`, `2020`), as midnight UTC.
pub fn parse_page_date(s: &str) -> Option<i64> {
    static DAY: OnceLock<Regex> = OnceLock::new(); static MON: OnceLock<Regex> = OnceLock::new(); static YEAR: OnceLock<Regex> = OnceLock::new();
    let s = s.trim();
    let day = re(&DAY, r"^([A-Za-z]+)\s+(\d{1,2}),\s+(\d{4})$"); let mon = re(&MON, r"^([A-Za-z]+)\s+(\d{4})$");
    for full in [false, true] {   // `%b %d, %Y` then `%B %d, %Y`: a short month name must be exactly three letters, a long one a whole name
        if let Some(c) = day.captures(s) { if let Some(m) = month_of(&c[1], full).filter(|_| full || c[1].len() == 3) { return utc_ts(c[3].parse().ok()?, m, c[2].parse().ok()?, 0, 0, 0); } }
    }
    for full in [false, true] {
        if let Some(c) = mon.captures(s) { if let Some(m) = month_of(&c[1], full).filter(|_| full || c[1].len() == 3) { return utc_ts(c[2].parse().ok()?, m, 1, 0, 0, 0); } }
    }
    if re(&YEAR, r"^\d{4}$").is_match(s) { return utc_ts(s.parse().ok()?, 1, 1, 0, 0, 0); }
    None
}

/// `_tag_key`: lower case, every run of other characters one space.
pub fn tag_key(t: &str) -> String { static R: OnceLock<Regex> = OnceLock::new(); re(&R, r"[^a-z0-9]+").replace_all(&t.to_lowercase(), " ").trim().to_string() }

fn is_upper(s: &str) -> bool { s.chars().any(|c| c.is_uppercase()) && !s.chars().any(|c| c.is_lowercase()) }
fn is_lower(s: &str) -> bool { s.chars().any(|c| c.is_lowercase()) && !s.chars().any(|c| c.is_uppercase()) }

/// `_itch_tag_allowlist`: `{key: canonical tag}` for every tag in the library (`tag_lists` = each game's `tags` column, in table order). The spelling kept is the most common
/// mixed-case one (a one-off `RELAXING` does not win over `Relaxing`), the first seen among equals.
pub fn allowlist<'a>(tag_lists: impl IntoIterator<Item = &'a str>) -> HashMap<String, String> {
    let mut seen: Vec<(String, Vec<String>)> = vec![];
    for list in tag_lists { for t in list.split(',') { let t = t.trim(); if t.is_empty() { continue; } let k = tag_key(t);
        match seen.iter_mut().find(|(sk, _)| *sk == k) { Some((_, v)) => v.push(t.to_string()), None => seen.push((k, vec![t.to_string()])) } } }
    seen.into_iter().map(|(k, casings)| {
        let mixed: Vec<&String> = casings.iter().filter(|c| !is_upper(c) && !is_lower(c)).collect();
        let pool: Vec<&String> = if mixed.is_empty() { casings.iter().collect() } else { mixed };
        let mut counts: Vec<(&String, usize)> = vec![];
        for c in pool { match counts.iter_mut().find(|(x, _)| *x == c) { Some((_, n)) => *n += 1, None => counts.push((c, 1)) } }
        let best = counts.iter().fold(None::<&(&String, usize)>, |b, x| match b { Some(b) if b.1 >= x.1 => Some(b), _ => Some(x) }).map(|(c, _)| (*c).clone()).unwrap_or_default();
        (k, best)
    }).collect()
}

/// `get_text(strip=True)`: the pieces of text under a node, each trimmed, joined with nothing.
fn stripped_text(el: scraper::ElementRef) -> String { el.text().map(str::trim).filter(|t| !t.is_empty()).collect() }

/// `store_page_metadata`'s parsing: genre, tags (only those the library already has) and release date from the game page's info panel.
pub fn page_meta(html: &str, allow: &HashMap<String, String>) -> PageMeta {
    use scraper::{Html, Selector};
    let doc = Html::parse_document(html);
    let (panel, tr, td, a) = (Selector::parse(".game_info_panel_widget").unwrap(), Selector::parse("tr").unwrap(), Selector::parse("td").unwrap(), Selector::parse("a").unwrap());
    let Some(panel) = doc.select(&panel).next() else { return PageMeta::default() };
    let mut rows: HashMap<String, (Vec<String>, String)> = HashMap::new();
    for row in panel.select(&tr) {
        let cells: Vec<_> = row.select(&td).collect();
        if cells.len() == 2 { rows.insert(stripped_text(cells[0]).to_lowercase(), (cells[1].select(&a).map(stripped_text).collect(), stripped_text(cells[1]))); }
    }
    let mut out = PageMeta::default();
    if let Some((links, _)) = rows.get("genre").filter(|(l, _)| !l.is_empty()) {
        let g = links.iter().map(|g| g.trim()).filter(|g| !g.is_empty()).collect::<Vec<_>>().join(",");
        if !g.is_empty() { out.genres = Some(g); }
    }
    if let Some((links, _)) = rows.get("tags").filter(|(l, _)| !l.is_empty()) {
        let mut kept: Vec<String> = vec![];
        for t in links { if let Some(canon) = allow.get(&tag_key(t)) { if !kept.contains(canon) { kept.push(canon.clone()); } } }
        if !kept.is_empty() { out.tags = Some(kept.join(",")); }
    }
    if let Some(ts) = rows.get("release date").and_then(|(_, t)| parse_page_date(t)) { out.release_date = Some(ts); }
    out
}

/// The creator of an itch game record (`display_name`, else `username`).
fn creator(game: &Value) -> String { let u = game.get("user").filter(|u| u.is_object()); u.map(|u| { let d = text(u, "display_name"); if d.is_empty() { text(u, "username") } else { d } }).unwrap_or("").to_string() }
/// A game's cover: the still cover if it has one, else the animated one.
fn cover(game: &Value) -> Option<String> { let c = text(game, "still_cover_url"); let c = if c.is_empty() { text(game, "cover_url") } else { c }; (!c.is_empty()).then(|| c.to_string()) }

/// The library records for the account's owned keys: games and tools only (not assets, comics, soundtracks), one record per game (a game claimed in two bundles comes back
/// twice with the same id; the first key wins). `now` stands in for a key with no readable date.
pub fn records_from(entries: &[Value], now: i64) -> Vec<GameRecord> {
    let (mut seen, mut out) = (std::collections::HashSet::new(), vec![]);
    for e in entries {
        let Some(game) = e.get("game").filter(|g| g.as_object().map_or(false, |o| !o.is_empty())) else { continue };
        // None / "" count as a game: older records carry no classification
        if !matches!(game.get("classification"), None | Some(Value::Null)) && !matches!(text(game, "classification"), "game" | "tool" | "") { continue; }
        let id = id_str(game.get("id")); if id.is_empty() || !seen.insert(id.clone()) { continue; }
        let title = text(game, "title"); let who = creator(game);
        out.push(GameRecord { platform_id: id.clone(), name: if title.is_empty() { format!("itch.io game {id}") } else { title.to_string() },
            slug: Some(text(game, "url").to_string()).filter(|u| !u.is_empty()), appname: Some(id_str(e.get("id"))).filter(|k| !k.is_empty()),
            date_added: Some(parse_date(text(e, "created_at")).unwrap_or(now)), release_date: parse_date(text(game, "published_at")),
            developers: Some(who.clone()).filter(|w| !w.is_empty()), publishers: Some(who).filter(|w| !w.is_empty()), ..Default::default() });
    }
    out
}

/// What a re-scrape takes from the game record: the page address, title, creator and release date.
pub fn rescrape_columns(game: &Value, fallback_name: &str) -> Update {
    let t = |s: String| Sql::Text(s);
    let mut out: Update = vec![("platform_slug", t(text(game, "url").to_string())), ("name", t(Some(text(game, "title")).filter(|n| !n.is_empty()).unwrap_or(fallback_name).to_string()))];
    let who = creator(game);
    if !who.is_empty() { out.push(("developers", t(who.clone()))); out.push(("publishers", t(who))); }
    if let Some(ts) = parse_date(text(game, "published_at")) { out.push(("release_date", Sql::Integer(ts))); }
    out
}

/// `_pick_upload`: the upload to install, from the game's upload list. Uploads of type `default` come first (soundtracks, manuals and the like are other types);
/// then this system's own build, then a Windows one (run through Wine/Proton), then whatever is first.
pub fn pick_upload(uploads: &[Value], os: crate::native_exe::Os) -> Option<&Value> {
    use crate::native_exe::Os;
    let default: Vec<&Value> = uploads.iter().filter(|u| u.get("type").and_then(|t| t.as_str()) == Some("default")).collect();
    let cands: Vec<&Value> = if default.is_empty() { uploads.iter().collect() } else { default };
    let has = |u: &Value, k: &str| u.get("platforms").and_then(|p| p.get(k)).map_or(false, |v| !v.is_null() && v != &Value::Bool(false) && v != &Value::String(String::new()));
    let first = |k: &str| cands.iter().copied().find(|u| has(u, k));
    let order: &[&str] = match os { Os::Linux => &["linux", "windows"], Os::Mac => &["osx", "windows"], Os::Windows => &["windows"] };
    // failing those, whatever is first: a build for another system is still downloaded (Rob 2026-11-25: the files are wanted even when Zest cannot run them here)
    order.iter().find_map(|k| first(k)).or_else(|| cands.first().copied())
}

/// A zip whose entries all sit in one top-level folder: its contents move up into the install folder (as PlayDate did) so the game is not buried one level down.
fn flatten_single_folder(dir: &std::path::Path) {
    let entries: Vec<_> = match std::fs::read_dir(dir) { Ok(r) => r.flatten().collect(), Err(_) => return };
    let [only] = entries.as_slice() else { return };
    let inner = only.path();
    if !inner.is_dir() { return; }
    let Ok(rd) = std::fs::read_dir(&inner) else { return };
    let items: Vec<_> = rd.flatten().map(|e| e.file_name()).collect();
    if items.iter().any(|n| dir.join(n).exists() && dir.join(n) != inner) { return; }
    for n in &items { let _ = std::fs::rename(inner.join(n), dir.join(n)); }
    let _ = std::fs::remove_dir(&inner);
}

/// Native Linux builds come out of archives without their execute bit on some tools: files with no extension or a launcher-like one get it (as PlayDate did).
fn mark_executables(dir: &std::path::Path) {
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() { mark_executables(&p); continue; }
            let ext = p.extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default();
            if matches!(ext.as_str(), "" | "sh" | "x86_64" | "x86" | "amd64" | "linux" | "appimage") {
                if let Ok(m) = std::fs::metadata(&p) { let mut perm = m.permissions(); perm.set_mode(perm.mode() | 0o111); let _ = std::fs::set_permissions(&p, perm); }
            }
        }
    }
    #[cfg(not(unix))] { let _ = dir; }
}

// ---------------------------------------------------------------- the source

pub struct ItchSource { http: Arc<dyn Http>, config_dir: PathBuf, db: PathBuf }

enum Failure { Rejected, Down(String) }
impl Failure { fn text(self) -> String { match self { Failure::Rejected => REJECTED.into(), Failure::Down(m) => m } } }

impl ItchSource {
    pub fn new(http: Arc<dyn Http>, config_dir: PathBuf, db: PathBuf) -> Self { ItchSource { http, config_dir, db } }
    fn secret(&self, k: &str) -> Option<String> { crate::account::plugin_secret_get(&self.config_dir, "itch_io", k).filter(|v| !v.is_empty()) }
    fn api_key(&self) -> Option<String> { self.secret("api_key") }
    fn get(&self, url: &str, bearer: Option<&str>, secs: u64) -> Result<crate::net::Response, String> {
        let (ua, auth) = (ua(), bearer.map(|b| format!("Bearer {b}")));
        let mut h: Vec<(&str, &str)> = vec![("User-Agent", &ua), ("Accept", "application/json")];
        if let Some(a) = &auth { h.push(("Authorization", a)); }
        self.http.get(url, &h, Duration::from_secs(secs)).map_err(|e| e.to_string())
    }
    /// The key's own game record (`/api/1/<key>/game/<id>`): None when itch could not be asked or has no such game.
    fn game_record(&self, key: &str, game_id: &str) -> Result<Option<Value>, Failure> {
        let r = self.get(&format!("{API}/{key}/game/{game_id}"), None, 10).map_err(|e| { log(&format!("itch.io: {e}")); Failure::Down(e) })?;
        if r.status == 401 { return Err(Failure::Rejected); }
        if r.status >= 400 { return Ok(None); }
        Ok(serde_json::from_str::<Value>(&r.body).ok().and_then(|v| v.get("game").filter(|g| g.as_object().map_or(false, |o| !o.is_empty())).cloned()))
    }
    /// The game's uploads (`/<key>/download-key/<id>/uploads`, else the game's own): the key's list can come back empty for a game the key does not cover.
    fn uploads(&self, key: &str, game_id: &str, download_key: &str) -> Result<Vec<Value>, String> {
        let fetch = |url: String| -> Result<Vec<Value>, String> {
            let r = self.get(&url, None, 15)?;
            if r.status >= 400 { return Err(format!("itch.io answered HTTP {}", r.status)); }
            let v: Value = serde_json::from_str(&r.body).map_err(|_| "Unexpected answer from itch.io".to_string())?;
            if let Some(e) = v.get("errors").and_then(|e| e.as_array()).filter(|e| !e.is_empty()) { return Err(e.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")); }
            Ok(v.get("uploads").and_then(|u| u.as_array()).cloned().unwrap_or_default())
        };
        if !download_key.is_empty() {
            let u = fetch(format!("{API}/{key}/download-key/{download_key}/uploads"))?;
            if !u.is_empty() { return Ok(u); }
            log(&format!("itch.io: download-key uploads empty for game {game_id}, trying the game endpoint"));
        }
        fetch(format!("{API}/{key}/game/{game_id}/uploads"))
    }
    /// A time-limited download address for one upload (itch answers with a redirect to it).
    fn download_url(&self, key: &str, upload_id: &str, download_key: &str) -> Result<String, String> {
        let q = if download_key.is_empty() { String::new() } else { format!("?download_key_id={download_key}") };
        let (ua, url) = (ua(), format!("{API}/{key}/upload/{upload_id}/download{q}"));
        let r = self.http.get_raw(&url, &[("User-Agent", &ua), ("Accept", "application/json")], Duration::from_secs(15)).map_err(|e| e.to_string())?;
        if matches!(r.status, 301 | 302 | 303 | 307 | 308) { if let Some(l) = r.header_all("Location").first().filter(|l| !l.is_empty()) { return Ok(l.to_string()); } }
        if r.status >= 400 { return Err(format!("itch.io answered HTTP {}", r.status)); }
        let v: Value = serde_json::from_str(&r.body).map_err(|_| "Unexpected answer from itch.io".to_string())?;
        Ok(text(&v, "url").to_string())
    }
    fn tag_lists(&self) -> Vec<String> {
        let Ok(c) = rusqlite::Connection::open_with_flags(&self.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { return vec![] };
        let Ok(mut st) = c.prepare("SELECT tags FROM games WHERE tags IS NOT NULL AND tags != '' ORDER BY appid") else { return vec![] };
        st.query_map([], |r| r.get::<_, String>(0)).into_iter().flatten().flatten().collect()
    }
    /// The store's cover for a game (`art_urls`): one image, offered for both the vertical and the horizontal slot.
    pub fn art_urls(&self, game_id: &str) -> Vec<(&'static str, String)> {
        let Some(key) = self.api_key() else { return vec![] };
        match self.game_record(&key, game_id) { Ok(Some(g)) => cover(&g).map(|c| vec![("vertical", c.clone()), ("horizontal", c)]).unwrap_or_default(), _ => vec![] }
    }
}

impl LibrarySource for ItchSource {
    fn id(&self) -> &str { "itch_io" }
    fn label(&self) -> &str { "itch.io" }
    fn caps(&self) -> Caps { Caps { auth: true, sync: true, rescrape: true, description: true, art: true, store_page_metadata: true, store_page: true, launch: true, install: true, uninstall: true, installed_scan: true, ..Default::default() } }

    fn auth_status(&self) -> Result<AuthStatus, String> {
        // as PlayDate's status route: a stored key means connected (it is checked when a sync runs)
        Ok(match self.api_key() { Some(_) => AuthStatus { connected: true, username: Some(self.secret("username").unwrap_or_else(|| "Connected".into())) }, None => AuthStatus { connected: false, username: None } })
    }
    /// The sign-in window opens the API keys page (a signed-out visitor is sent to the login first); the window presses the first View button (keys are shown shortened) and is done once a key (40 letters and digits) is on the page.
    fn web_login_done(&self) -> Option<serde_json::Value> { Some(serde_json::json!({"kind": "body", "url_contains": "/user/settings/api-keys", "token_len": 40, "click": "View"})) }
    fn paste_login_kept(&self) -> bool { false }   // the sign-in window works (confirmed live 2026-11-22)
    fn auth_begin(&self) -> Result<AuthBegin, String> {
        Ok(AuthBegin { url: KEYS_URL.into(), placeholder: "Paste your API key here...".into(), instructions: vec![
            "Sign in to itch.io in your browser. The page opens on your API keys.".into(),
            "Click Generate new API key.".into(),
            "Copy the key and paste it below.".into()] })
    }
    fn auth_complete(&self, value: &str) -> Result<AuthStatus, String> {
        let key = value.trim();
        if key.is_empty() { return Err("No API key provided".into()); }
        let r = self.get(&format!("{API}/{key}/me"), None, 10)?;
        if matches!(r.status, 401 | 403 | 404) { return Err("Invalid API key".into()); }
        if r.status >= 400 { return Err(format!("itch.io answered HTTP {}", r.status)); }
        let body: Value = serde_json::from_str(&r.body).map_err(|_| "Unexpected answer from itch.io".to_string())?;
        if let Some(errs) = body.get("errors").filter(|e| !e.is_null()) {
            let list: Vec<String> = errs.as_array().map(|a| a.iter().filter_map(|e| e.as_str().map(String::from)).collect()).unwrap_or_default();
            return Err(if list.is_empty() { "itch.io refused the key".into() } else { list.join(", ") });
        }
        let u = body.get("user").cloned().unwrap_or(Value::Null);
        let name = Some(text(&u, "username")).filter(|n| !n.is_empty()).or(Some(text(&u, "display_name")).filter(|n| !n.is_empty())).unwrap_or("itch.io user").to_string();
        for (k, v) in [("api_key", key), ("username", name.as_str())] { crate::account::plugin_secret_set(&self.config_dir, "itch_io", k, Some(v)).map_err(|_| "Could not save the key".to_string())?; }
        log(&format!("itch.io connected as {name:?}"));
        Ok(AuthStatus { connected: true, username: Some(name) })
    }
    fn auth_disconnect(&self) -> Result<(), String> {
        // PlayDate drops the whole section
        for k in ["api_key", "username"] { crate::account::plugin_secret_set(&self.config_dir, "itch_io", k, None).map_err(|_| "Could not save".to_string())?; }
        Ok(())
    }

    fn art_urls(&self, g: &GameRef) -> Vec<(String, String)> { Self::art_urls(self, &g.platform_id).into_iter().map(|(k, v)| (k.to_string(), v)).collect() }
    fn games_dir(&self) -> Option<PathBuf> { Some(self.dir()) }
    fn install(&self, _cx: &SourceCtx, g: &GameRef, progress: &mut dyn FnMut(&str, u64, u64), cancel: &Cancel) -> Result<Installed, String> {
        let key = self.api_key().ok_or("Not connected to itch.io")?;
        let os = crate::native_exe::Os::current();
        progress(&format!("{}: asking itch.io for the download", g.name), 0, 0);
        let uploads = self.uploads(&key, &g.platform_id, &g.appname).map_err(|e| format!("Failed to list downloads: {e}"))?;
        let upload = pick_upload(&uploads, os).ok_or("No downloads available.")?;
        let url = self.download_url(&key, &id_str(upload.get("id")), &g.appname).map_err(|e| format!("Failed to get the download URL: {e}"))?;
        if url.is_empty() { return Err("itch.io returned an empty download address".into()); }
        let filename = Some(text(upload, "filename")).filter(|f| !f.is_empty() && !f.contains(['/', '\\'])).unwrap_or("game.zip").to_string();
        let dir = self.dir().join(format!("{}_{}", super::indiegala::safe_dirname(&g.name), g.platform_id));
        crate::installdir::check_writable(&dir)?;
        let archive = dir.join(format!("_dl_{filename}"));
        let name = g.name.clone();
        log(&format!("itch.io: downloading {filename:?} for {name:?}"));
        let fail = |e: String| { let _ = std::fs::remove_dir_all(&dir); e };
        super::folder::download(self.http.as_ref(), &url, &archive, cancel, &mut |d, t| progress(&format!("Downloading {name}: {} of {} MB", super::folder::mb(d), super::folder::mb(t)), d, t)).map_err(fail)?;
        let mut note = None;
        if crate::archive::kind_of(&filename).is_none() && !crate::archive::is_zip_payload(&archive) {
            // a single program (an AppImage, a .exe, a .sh): it is the game
            let dest = dir.join(&filename);
            std::fs::rename(&archive, &dest).map_err(|e| fail(format!("Could not save the download: {e}")))?;
        } else {
            let named = dir.join(&filename);   // unpack() decides by the file name, so it gets the real one
            std::fs::rename(&archive, &named).map_err(|e| fail(format!("Could not save the download: {e}")))?;
            progress(&format!("Unpacking {name}"), 0, 0);
            note = super::folder::unpack(&named, &dir, cancel, &mut |d, t| progress(&format!("Unpacking {name}: {} of {} MB", super::folder::mb(d), super::folder::mb(t)), d, t)).map_err(fail)?;
        }
        flatten_single_folder(&dir);
        mark_executables(&dir);
        // a game shipped as a Windows setup program: run it once into the game's folder
        match super::folder::run_setup(&dir, &self.prefix(g.appid), cancel, &mut |m| progress(&format!("{name}: {m}"), 0, 0)) {
            Ok(Some(n)) => { log(&format!("itch.io: {name:?} {n}")); }
            Ok(None) => {}
            Err(e) if e == "Cancelled" => return Err(fail(e)),
            Err(e) => { log(&format!("itch.io: {name:?}: {e}")); note = Some(e); }
        }
        let picked = crate::native_exe::pick_executable(&dir, os);
        log(&format!("itch.io install complete: {name:?} -> {} (exe: {:?})", dir.display(), picked.path));
        // several equally likely programs: say which one was taken (there is no picker yet, PlayDate asked the user)
        if note.is_none() && picked.ambiguous && picked.candidates.len() > 1 {
            note = Some(format!("Installed {name}. Zest found several programs in it ({}) and will start {}.", picked.candidates.join(", "), picked.path.as_deref().unwrap_or("the first")));
        }
        Ok(Installed { install_path: dir.to_string_lossy().to_string(), executable: picked.path, note })
    }
    fn installed_scan(&self) -> Option<Vec<InstalledRow>> {
        // games Zest manages (a folder) are installed while the folder exists; the rest are the itch app's, by butler.db
        let butler = butler_db_path().map(|p| butler_ids(&p)).unwrap_or_default();
        let c = rusqlite::Connection::open_with_flags(&self.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
        let mut st = c.prepare("SELECT platform_id, COALESCE(install_path, '') FROM games WHERE platform = 'itch_io' AND platform_id IS NOT NULL").ok()?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).ok()?.flatten()
            .map(|(id, path)| InstalledRow { installed: if path.is_empty() { butler.contains(&id) } else { std::path::Path::new(&path).is_dir() }, platform_id: id, install_path: None }).collect();
        Some(rows)
    }
    fn launch(&self, g: &GameRef) -> Result<Launch, String> {
        if !g.installed { return Err(format!("{} is not installed yet. Install it from Zest, with the itch app (Zest sees those), or put its files in a folder under {}.", g.name, self.dir().display())); }
        let os = crate::native_exe::Os::current();
        if let Some(dir) = g.install_path.as_deref().map(std::path::Path::new).filter(|d| d.is_dir()) {
            match super::folder::choose_executable(dir, g.executable.as_deref()) {
                // a browser game opens in the web browser; otherwise the itch app's own install may still have it
                super::folder::Choice::Nothing => if let Some(page) = super::folder::html_game(dir) { return Ok(super::folder::open_html(&page)); },
                c => return super::folder::launch_choice(dir, c, &|abs, _| self.run_file(abs, dir, g.appid)),
            }
        }
        // the itch app's own install
        if let Some((_, exe)) = butler_db_path().and_then(|p| cave_verdict(&p, &g.platform_id)).and_then(|v| pick_verdict_executable(&v, os)) {
            let cwd = exe.parent().map(|p| p.to_path_buf()).unwrap_or_default();
            return self.run_file(&exe, &cwd, g.appid);
        }
        if g.install_path.as_deref().map_or(false, |d| std::path::Path::new(d).is_dir()) { return Err(format!("No supported executable found in {}.", g.name)); }
        Err(format!("The files for {} are not where Zest last saw them. Install it again with the itch app, or put them in a folder under {}.", g.name, self.dir().display()))
    }
    fn uninstall(&self, g: &GameRef) -> Result<Action, String> {
        match g.install_path.as_deref().filter(|p| !p.is_empty()) {
            Some(dir) => Ok(Action::RemoveInstall { delete: crate::installdir::is_inside(std::path::Path::new(dir), &self.dir()), dir: dir.to_string() }),
            None => Err(format!("{} was installed by the itch app. Uninstall it there.", g.name)),
        }
    }
    fn store_url(&self, g: &GameRef) -> Option<String> { Some(g.slug.clone()).filter(|s| s.starts_with("http")) }

    fn sync(&self, _cx: &SourceCtx, sink: &mut dyn FnMut(Vec<GameRecord>), cancel: &Cancel) -> Result<usize, String> {
        let key = self.api_key().ok_or("Not connected")?;
        let (mut owned, mut page): (Vec<Value>, u32) = (vec![], 1);
        loop {
            if cancel.is_set() { return Ok(0); }
            set_activity(&format!("itch.io: reading your library (page {page})"));
            let r = match self.get(&format!("{API_NEW}/profile/owned-keys?page={page}"), Some(&key), 20) {
                Ok(r) => r,
                // a page that cannot be read ends the list; what came before is still imported (as PlayDate did), but a first page that fails is a failed sync
                Err(e) => { log(&format!("itch.io library: page {page} fetch failed: {e}")); if page == 1 { return Err(format!("Could not reach itch.io: {e}")); } break }
            };
            if r.status == 401 { return Err(REJECTED.into()); }
            if r.status >= 400 { log(&format!("itch.io library: page {page} answered HTTP {}", r.status)); if page == 1 { return Err(format!("itch.io answered HTTP {}", r.status)); } break; }
            let data: Value = match serde_json::from_str(&r.body) { Ok(d) => d, Err(_) => { log(&format!("itch.io library: page {page} was not JSON")); if page == 1 { return Err("Unexpected answer from itch.io".into()); } break } };
            if let Some(errs) = data.get("errors").filter(|e| !e.is_null()) {
                let list: Vec<String> = errs.as_array().map(|a| a.iter().filter_map(|e| e.as_str().map(String::from)).collect()).unwrap_or_default();
                return Err(if list.is_empty() { "itch.io refused the request".into() } else { list.join(", ") });
            }
            let keys = data.get("owned_keys").and_then(|k| k.as_array()).cloned().unwrap_or_default();
            log(&format!("itch.io library: page {page}, {} keys", keys.len()));
            let per_page = data.get("per_page").and_then(|p| p.as_u64()).unwrap_or(10) as usize;
            let n = keys.len();
            owned.extend(keys);
            if n == 0 || n < per_page { break; }
            page += 1; pause(300);
        }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        let records = records_from(&owned, now);
        log(&format!("itch.io sync: {} games from {} owned items", records.len(), owned.len()));
        let count = records.len();
        if count > 0 { sink(records); }
        Ok(count)
    }

    fn rescrape(&self, cx: &SourceCtx, g: &GameRef) -> Fetch<Option<Update>> {
        let Some(key) = self.api_key() else { log("itch.io rescrape: not connected"); return Ok(None) };
        let id = if g.platform_id.is_empty() { return Ok(None) } else { g.platform_id.clone() };
        let mut out: Update = vec![("meta_fetched", Sql::Text(cx.today.clone())), ("cheevos_fetched", Sql::Text(cx.today.clone()))];
        match self.game_record(&key, &id) {
            // no answer at all (offline) is not a success: nothing is stamped
            Err(Failure::Down(_)) => return Ok(None),
            Err(f) => { log(&format!("itch.io rescrape: {}", f.text())); return Ok(None); }
            Ok(Some(game)) => out.extend(rescrape_columns(&game, &g.name)),
            Ok(None) => {}
        }
        Ok(Some(out))
    }
    fn description(&self, _cx: &SourceCtx, g: &GameRef) -> Option<String> {
        let key = self.api_key()?;
        let game = self.game_record(&key, &g.platform_id).ok().flatten()?;
        Some(text(&game, "short_text").to_string()).filter(|t| !t.is_empty())
    }
    fn store_page_metadata(&self, slug: &str) -> PageMeta {
        if slug.is_empty() || !slug.contains("itch.io/") { return PageMeta::default(); }
        let r = match self.http.get(slug, &[("User-Agent", BROWSER_UA)], Duration::from_secs(15)) { Ok(r) => r, Err(e) => { log(&format!("itch store_page_metadata: {e}")); return PageMeta::default(); } };
        if r.status >= 400 { return PageMeta::default(); }
        page_meta(&r.body, &allowlist(self.tag_lists().iter().map(String::as_str)))
    }
}


// ---------------------------------------------------------------- install detection and launch (the itch app's butler.db, or a folder Zest manages)

/// butler.db: where the itch app records its installs.
fn butler_db_path() -> Option<PathBuf> {
    let home = crate::installdir::home();
    let mut c = vec![home.join(".config/itch/db/butler.db"), home.join("Library/Application Support/itch/db/butler.db")];
    if let Some(a) = std::env::var_os("APPDATA") { c.push(PathBuf::from(a).join("itch/db/butler.db")); }
    c.into_iter().find(|p| p.exists())
}
/// `game_id`s the itch app has installed.
pub fn butler_ids(db: &std::path::Path) -> std::collections::HashSet<String> {
    let Ok(c) = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { return Default::default() };
    let Ok(mut st) = c.prepare("SELECT game_id FROM caves") else { return Default::default() };
    st.query_map([], |r| r.get::<_, rusqlite::types::Value>(0)).into_iter().flatten().flatten().map(|v| match v { rusqlite::types::Value::Integer(i) => i.to_string(), rusqlite::types::Value::Text(t) => t, _ => String::new() }).collect()
}
/// `_find_cave_verdict`: the itch app's description of an install (`basePath` and its launch `candidates`).
pub fn cave_verdict(db: &std::path::Path, game_id: &str) -> Option<Value> {
    let c = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    let id: i64 = game_id.parse().ok()?;
    let v: Option<String> = c.query_row("SELECT verdict FROM caves WHERE game_id = ? AND verdict IS NOT NULL", [id], |r| r.get(0)).ok()?;
    serde_json::from_str(&v.filter(|v| !v.is_empty())?).ok()
}
/// `_pick_executable`: (flavor, absolute path) of the best runnable candidate in a verdict for this OS; an existing file only.
pub fn pick_verdict_executable(verdict: &Value, os: crate::native_exe::Os) -> Option<(String, PathBuf)> {
    use crate::native_exe::Os;
    let base = text(verdict, "basePath"); let cands = verdict.get("candidates").and_then(|c| c.as_array()).filter(|c| !c.is_empty())?;
    if base.is_empty() { return None; }
    let prefs: &[&str] = match os { Os::Mac => &["osx", "appimage", "script", "linux"], Os::Windows => &["windows"], Os::Linux => &["linux", "appimage", "script"] };
    for f in prefs { for c in cands { if text(c, "flavor") == *f { let p = PathBuf::from(base).join(text(c, "path")); if p.exists() { return Some((f.to_string(), p)); } } } }
    let c = &cands[0]; let p = PathBuf::from(base).join(text(c, "path"));
    p.exists().then(|| (Some(text(c, "flavor")).filter(|f| !f.is_empty()).unwrap_or("unknown").to_string(), p))
}

impl ItchSource {
    /// The Wine prefix for a game (PlayDate's folder, so an existing prefix is reused).
    fn prefix(&self, appid: i64) -> PathBuf {
        // PlayDate kept itch prefixes in one folder of their own; a games folder the user chose holds them instead
        let configured = std::fs::read_to_string(self.config_dir.join("config.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).map_or(false, |c| c.get("itch_io").and_then(|i| i.get("games_dir")).and_then(|d| d.as_str()).map_or(false, |d| !d.is_empty()));
        if cfg!(target_os = "linux") && !configured { crate::installdir::home().join(".local/share/playdate-itch/prefixes").join(appid.to_string()) } else { self.dir().join(".prefixes").join(appid.to_string()) }
    }
    fn dir(&self) -> PathBuf { crate::installdir::games_dir(&self.config_dir, "itch_io", crate::installdir::default_dir("itch", "itch")) }
    fn run_file(&self, exe: &std::path::Path, cwd: &std::path::Path, appid: i64) -> Result<Launch, String> { super::folder::run_file(exe, cwd, Some(&self.prefix(appid))) }
}

#[cfg(test)]
mod behaviour {
    use super::*;
    use crate::net::{NetError, Response};
    use serde_json::json;
    use std::sync::Mutex;

    /// A fake itch.io: the key check, the owned-keys pages, the game records and a game page. Records every URL asked.
    struct Site { key_ok: bool, keys_status: u16, fail_page: Option<u32>, game_status: Option<u16>, asked: Mutex<Vec<String>>, zip: Vec<u8>, uploads: Value }
    fn fake() -> Site { Site { key_ok: true, keys_status: 200, fail_page: None, game_status: None, asked: Mutex::new(vec![]), zip: std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/sample.zip")).unwrap(), uploads: json!({"uploads": []}) } }
    fn entry(key_id: i64, gid: i64, title: &str, class: &str) -> Value {
        json!({"id": key_id, "created_at": "2020-05-01T12:30:00.000000Z", "game": {"id": gid, "title": title, "url": format!("https://dev.itch.io/g{gid}"), "classification": class,
            "published_at": "2019-03-04T00:00:00Z", "still_cover_url": format!("https://img/{gid}.png"), "user": {"display_name": "Dev Person", "username": "dev"}}})
    }
    const PAGE: &str = r#"<html><div class="game_info_panel_widget"><table>
        <tr><td>Genre</td><td><a>Platformer</a>, <a>Puzzle</a></td></tr>
        <tr><td>Tags</td><td><a>2D</a> <a>Pixel Art</a> <a>Cursed Thing</a></td></tr>
        <tr><td>Release date</td><td><abbr>Oct 12, 2020</abbr></td></tr></table></div></html>"#;
    impl Http for Site {
        fn get(&self, u: &str, h: &[(&str, &str)], _t: Duration) -> Result<Response, NetError> {
            self.asked.lock().unwrap().push(u.to_string());
            assert!(h.iter().any(|(k, _)| *k == "User-Agent"));
            let r = |s: u16, b: Value| Ok(Response { status: s, retry_after: None, body: b.to_string() });
            if u == "https://itch.io/api/1/GOODKEY/me" { return if self.key_ok { r(200, json!({"user": {"username": "pixelfan", "display_name": "Pixel Fan"}})) } else { r(404, json!({"errors": ["invalid key"]})) }; }
            if u.starts_with(API_NEW) {
                assert!(h.iter().any(|(k, v)| *k == "Authorization" && *v == "Bearer GOODKEY"));
                if self.keys_status != 200 { return r(self.keys_status, json!({})); }
                let page: u32 = u.rsplit('=').next().unwrap().parse().unwrap();
                if self.fail_page == Some(page) { return Err(NetError("timed out".into())); }
                return match page {
                    1 => r(200, json!({"per_page": 3, "owned_keys": [entry(1, 11, "Game A", "game"), entry(2, 12, "Soundtrack", "soundtrack"), entry(3, 13, "Tool B", "tool")]})),
                    2 => r(200, json!({"per_page": 3, "owned_keys": [entry(4, 11, "Game A again", "game"), entry(5, 14, "", ""), json!({"id": 6, "game": {}})]})),
                    _ => r(200, json!({"per_page": 3, "owned_keys": []})),
                };
            }
            if u.ends_with("/uploads") { return if u.contains("/download-key/") { r(200, json!({"uploads": []})) } else { r(200, self.uploads.clone()) }; }
            if let (Some(st), true) = (self.game_status, u.contains("/game/")) { return if st == 0 { Err(NetError("offline".into())) } else { r(st, json!({})) }; }
            if u == "https://itch.io/api/1/GOODKEY/game/11" { return r(200, json!({"game": {"id": 11, "title": "Game A Renamed", "url": "https://dev.itch.io/g11", "short_text": "Hop and skip", "still_cover_url": "https://img/11.png", "published_at": "2019-03-04T00:00:00Z", "user": {"username": "devuser"}}})); }
            if u.starts_with("https://itch.io/api/1/GOODKEY/game/") { return r(404, json!({})); }
            if u == "https://dev.itch.io/g11" { return Ok(Response { status: 200, retry_after: None, body: PAGE.into() }); }
            r(404, json!({}))
        }
        fn get_raw(&self, u: &str, h: &[(&str, &str)], _t: Duration) -> Result<crate::net::Raw, NetError> {
            self.asked.lock().unwrap().push(u.to_string());
            assert!(h.iter().any(|(k, _)| *k == "User-Agent"));
            if u.contains("/upload/") && u.contains("/download") { return Ok(crate::net::Raw { status: 302, headers: vec![("Location".into(), "https://cdn.example/file.zip".into())], body: String::new() }); }
            Ok(crate::net::Raw { status: 404, headers: vec![], body: String::new() })
        }
        fn get_stream(&self, u: &str, _h: &[(&str, &str)], on_head: &mut dyn FnMut(u16, Option<u64>) -> bool, on_chunk: &mut dyn FnMut(&[u8]) -> bool) -> Result<(), NetError> {
            self.asked.lock().unwrap().push(u.to_string());
            if !on_head(200, Some(self.zip.len() as u64)) { return Ok(()); }
            for c in self.zip.chunks(400) { if !on_chunk(c) { return Ok(()); } }
            Ok(())
        }
    }
    fn setup(tag: &str, site: Site, key: bool) -> (ItchSource, Arc<Site>, PathBuf) {
        let d = std::env::temp_dir().join(format!("zest_itch_{tag}_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        if key { std::fs::write(d.join("config.json"), json!({"itch_io": {"api_key": "GOODKEY", "username": "pixelfan", "games_dir": d.join("games").to_string_lossy()}, "other": 1}).to_string()).unwrap(); }
        let db = d.join("games.db");
        let c = rusqlite::Connection::open(&db).unwrap();
        c.execute_batch("CREATE TABLE games (appid INTEGER PRIMARY KEY, tags TEXT); INSERT INTO games VALUES (-1, '2D,Pixel Graphics'), (-2, 'Pixel Art,PIXEL ART,Pixel Art');").unwrap();
        let site = Arc::new(site);
        (ItchSource::new(site.clone(), d.clone(), db), site, d)
    }
    fn cx(d: &PathBuf, site: Arc<Site>) -> SourceCtx {
        SourceCtx { http: site, steam: Arc::new(crate::steamclient::Steam::new(Arc::new(crate::net::UreqHttp::new()), None, 10.0)), root: d.clone(), db: d.join("games.db"), today: "2026-11-01".into(), appinfo: Arc::new(|_| Default::default()), account_id: None }
    }
    fn g(id: &str) -> GameRef { GameRef { appid: -5, name: "Old Name".into(), platform: "itch_io".into(), platform_id: id.into(), ..Default::default() } }

    #[test] fn the_upload_for_this_system_is_chosen_with_default_types_first_then_a_windows_build_then_anything() {
        use crate::native_exe::Os;
        let ups = vec![json!({"id": 1, "type": "soundtrack", "platforms": {"linux": "all"}}), json!({"id": 2, "type": "default", "platforms": {"windows": "all"}}),
            json!({"id": 3, "type": "default", "platforms": {"linux": "all"}}), json!({"id": 4, "type": "default", "platforms": {"osx": "all"}})];
        let id = |u: Option<&Value>| u.map(|u| u["id"].as_i64().unwrap());
        assert_eq!((id(pick_upload(&ups, Os::Linux)), id(pick_upload(&ups, Os::Mac)), id(pick_upload(&ups, Os::Windows))), (Some(3), Some(4), Some(2)));
        let only_win = vec![json!({"id": 9, "type": "default", "platforms": {"windows": "all", "linux": null}})];
        assert_eq!(id(pick_upload(&only_win, Os::Linux)), Some(9));   // Wine/Proton
        let odd = vec![json!({"id": 7, "type": "default", "platforms": {}}), json!({"id": 8, "type": "default"})];
        assert_eq!(id(pick_upload(&odd, Os::Linux)), Some(7));
        assert_eq!(id(pick_upload(&[json!({"id": 5, "type": "book"})], Os::Linux)), Some(5));   // nothing of type default: the list as it is
        assert!(pick_upload(&[], Os::Linux).is_none());
        // a Mac-only game is still downloaded on Linux and Windows (the files are wanted even if they cannot run here)
        let mac_only = vec![json!({"id": 6, "type": "default", "platforms": {"osx": "all"}})];
        assert_eq!((id(pick_upload(&mac_only, Os::Linux)), id(pick_upload(&mac_only, Os::Windows)), id(pick_upload(&mac_only, Os::Mac))), (Some(6), Some(6), Some(6)));
    }
    #[test] fn install_lists_uploads_follows_the_download_redirect_unpacks_into_the_games_folder_and_reports_the_key_it_used() {
        let site = Site { uploads: json!({"uploads": [{"id": 77, "type": "default", "filename": "game.zip", "platforms": {"linux": "all"}}]}), ..fake() };
        let (s, site, d) = setup("install", site, true);
        let mut g = g("11"); g.appname = "1".into(); g.name = "Game: A!".into();
        let mut says: Vec<String> = vec![];
        let got = s.install(&cx(&d, site.clone()), &g, &mut |m, _, _| says.push(m.to_string()), &Cancel::new()).unwrap();
        let dir = d.join("games").join(format!("{}_11", super::super::indiegala::safe_dirname("Game: A!")));
        assert_eq!(got.install_path, dir.to_string_lossy());
        assert!(dir.join("empty.txt").is_file() && !dir.join("game.zip").exists() && !dir.join("_dl_game.zip").exists(), "unpacked, archive removed");
        let asked = site.asked.lock().unwrap().clone();
        assert!(asked.iter().any(|u| u.ends_with("/GOODKEY/download-key/1/uploads")) && asked.iter().any(|u| u.ends_with("/GOODKEY/game/11/uploads")), "falls back to the game endpoint when the key lists nothing");
        assert!(asked.iter().any(|u| u.ends_with("/upload/77/download?download_key_id=1")) && asked.iter().any(|u| u == "https://cdn.example/file.zip"));
        assert!(says.iter().any(|m| m.starts_with("Downloading Game")) && s.caps().install);
        // no uploads, and not connected
        let (s, site, d) = setup("install_none", fake(), true);
        assert_eq!(s.install(&cx(&d, site), &g, &mut |_, _, _| {}, &Cancel::new()).unwrap_err(), "No downloads available.");
        let (s, site, d) = setup("install_nokey", fake(), false);
        assert_eq!(s.install(&cx(&d, site), &g, &mut |_, _, _| {}, &Cancel::new()).unwrap_err(), "Not connected to itch.io");
    }
    #[test] fn a_single_top_folder_is_flattened_and_launchers_get_the_execute_bit() {
        let d = std::env::temp_dir().join(format!("zest_itch_flat_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("Game-1.0/data")).unwrap(); std::fs::write(d.join("Game-1.0/run.sh"), "#!/bin/sh\n").unwrap(); std::fs::write(d.join("Game-1.0/data/a.dat"), "x").unwrap(); std::fs::write(d.join("Game-1.0/data/b.png"), "x").unwrap();
        flatten_single_folder(&d);
        assert!(d.join("run.sh").is_file() && d.join("data/a.dat").is_file() && !d.join("Game-1.0").exists());
        mark_executables(&d);
        #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; let mode = |p: &str| std::fs::metadata(d.join(p)).unwrap().permissions().mode() & 0o111;
            assert!(mode("run.sh") == 0o111 && mode("data/b.png") == 0); }
        // two entries at the top: left alone
        std::fs::create_dir_all(d.join("other")).unwrap(); flatten_single_folder(&d); assert!(d.join("run.sh").is_file());
    }
    #[test] fn sign_in_checks_the_key_keeps_it_and_leaves_other_settings_alone() {
        let (s, site, d) = setup("auth", fake(), false);
        assert_eq!(s.auth_status().unwrap(), AuthStatus { connected: false, username: None });
        assert!(s.auth_complete("  ").is_err());
        assert_eq!(s.auth_complete("BADKEY").unwrap_err(), "Invalid API key");
        assert_eq!(s.auth_complete(" GOODKEY\n").unwrap(), AuthStatus { connected: true, username: Some("pixelfan".into()) });
        let cfg: Value = serde_json::from_str(&std::fs::read_to_string(d.join("config.json")).unwrap()).unwrap();
        assert_eq!(cfg["itch_io"], json!({"api_key": "GOODKEY", "username": "pixelfan"}));
        assert_eq!(s.auth_status().unwrap().username.as_deref(), Some("pixelfan"));
        s.auth_disconnect().unwrap();
        assert!(!s.auth_status().unwrap().connected);
        assert!(site.asked.lock().unwrap().iter().all(|u| !u.contains("page=")));
    }
    #[test] fn sync_walks_the_pages_keeps_games_and_tools_once_and_stamps_each_with_its_key() {
        let (s, site, d) = setup("sync", fake(), true);
        let mut got: Vec<GameRecord> = vec![];
        let n = s.sync(&cx(&d, site.clone()), &mut |b| got.extend(b), &Cancel::new()).unwrap();
        assert_eq!(n, 3);
        let names: Vec<&str> = got.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["Game A", "Tool B", "itch.io game 14"]);   // the soundtrack is dropped, Game A's second key is not a second game, a blank title gets a name
        let a = &got[0];
        assert_eq!((a.platform_id.as_str(), a.slug.as_deref(), a.appname.as_deref()), ("11", Some("https://dev.itch.io/g11"), Some("1")));
        assert_eq!((a.date_added, a.release_date), (Some(1588336200), Some(1551657600)));
        assert_eq!((a.developers.as_deref(), a.publishers.as_deref()), (Some("Dev Person"), Some("Dev Person")));
        assert_eq!(site.asked.lock().unwrap().iter().filter(|u| u.contains("owned-keys")).count(), 3);
    }
    #[test] fn sync_fails_on_a_rejected_key_a_first_page_error_or_a_not_connected_account_and_keeps_what_it_has_after_a_later_failure() {
        let (s, site, d) = setup("nokey", fake(), false);
        assert_eq!(s.sync(&cx(&d, site), &mut |_| {}, &Cancel::new()).unwrap_err(), "Not connected");
        let (s, site, d) = setup("401", Site { keys_status: 401, ..fake() }, true);
        assert_eq!(s.sync(&cx(&d, site), &mut |_| {}, &Cancel::new()).unwrap_err(), REJECTED);
        let (s, site, d) = setup("down1", Site { fail_page: Some(1), ..fake() }, true);
        assert!(s.sync(&cx(&d, site), &mut |_| {}, &Cancel::new()).unwrap_err().starts_with("Could not reach itch.io"));
        let (s, site, d) = setup("down2", Site { fail_page: Some(2), ..fake() }, true);
        let mut got = vec![];
        assert_eq!(s.sync(&cx(&d, site), &mut |b| got.extend(b), &Cancel::new()).unwrap(), 2);
        assert_eq!(got.len(), 2);
        let (s, site, d) = setup("cancel", fake(), true);
        let c = Cancel::new(); c.cancel();
        let mut called = false;
        assert_eq!(s.sync(&cx(&d, site), &mut |_| called = true, &c).unwrap(), 0); assert!(!called);
    }
    #[test] fn rescrape_description_art_and_page_metadata_use_the_key_and_the_librarys_own_tags() {
        let (s, site, d) = setup("meta", fake(), true);
        let u = s.rescrape(&cx(&d, site.clone()), &g("11")).unwrap().unwrap();
        let get = |k: &str| u.iter().find(|(c, _)| *c == k).map(|(_, v)| v.clone());
        assert_eq!(get("name"), Some(Sql::Text("Game A Renamed".into()))); assert_eq!(get("developers"), Some(Sql::Text("devuser".into()))); assert_eq!(get("release_date"), Some(Sql::Integer(1551657600)));
        assert_eq!((get("meta_fetched"), get("cheevos_fetched")), (Some(Sql::Text("2026-11-01".into())), Some(Sql::Text("2026-11-01".into()))));
        // a game itch no longer knows is still stamped, as PlayDate did, with nothing else
        let u = s.rescrape(&cx(&d, site.clone()), &g("99")).unwrap().unwrap(); assert_eq!(u.len(), 2);
        assert_eq!(s.description(&cx(&d, site.clone()), &g("11")).as_deref(), Some("Hop and skip"));
        assert_eq!(s.art_urls("11"), vec![("vertical", "https://img/11.png".to_string()), ("horizontal", "https://img/11.png".to_string())]);
        assert_eq!(LibrarySource::art_urls(&s, &g("11")), vec![("vertical".to_string(), "https://img/11.png".to_string()), ("horizontal".to_string(), "https://img/11.png".to_string())]);
        let m = s.store_page_metadata("https://dev.itch.io/g11");
        assert_eq!((m.genres.as_deref(), m.tags.as_deref(), m.release_date), (Some("Platformer,Puzzle"), Some("2D,Pixel Art"), Some(1602460800)));
        assert!(s.store_page_metadata("https://example.com/x").genres.is_none());
        assert!(s.caps().sync && s.caps().auth && !s.enrich_new());
        // not connected: nothing is asked and nothing stamped
        let (s, site, d) = setup("meta_nokey", fake(), false);
        assert!(s.rescrape(&cx(&d, site.clone()), &g("11")).unwrap().is_none() && s.description(&cx(&d, site), &g("11")).is_none() && s.art_urls("11").is_empty());
    }
    #[test] fn launch_runs_the_best_file_in_the_games_folder_and_explains_what_it_cannot_do() {
        let (s, _, d) = setup("launch", fake(), true);
        let elf = [0x7fu8, b'E', b'L', b'F', 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let gdir = d.join("games/Game_1"); std::fs::create_dir_all(&gdir).unwrap(); std::fs::write(gdir.join("game.x86_64"), elf).unwrap(); std::fs::write(gdir.join("readme.txt"), "x").unwrap();
        let mut gr = GameRef { appid: -5, name: "Game".into(), platform: "itch_io".into(), platform_id: "11".into(), installed: true, install_path: Some(gdir.to_string_lossy().into()), ..Default::default() };
        let l = s.launch(&gr).unwrap();
        assert_eq!(l.action, Action::Spawn { program: gdir.join("game.x86_64").to_string_lossy().into(), args: vec![], dir: Some(gdir.to_string_lossy().into()), env: vec![] }); assert!(l.record);
        #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; assert!(std::fs::metadata(gdir.join("game.x86_64")).unwrap().permissions().mode() & 0o111 != 0); }
        // a chosen executable beats the guess
        std::fs::write(gdir.join("other.sh"), "#!/bin/sh").unwrap(); gr.executable = Some("other.sh".into());
        assert!(matches!(s.launch(&gr).unwrap().action, Action::Spawn { ref program, .. } if program.ends_with("other.sh")));
        // a Windows-only game cannot run yet (without saying nothing)
        let wdir = d.join("games/Win_2"); std::fs::create_dir_all(&wdir).unwrap(); std::fs::write(wdir.join("Win.exe"), "MZ").unwrap();
        let w = GameRef { install_path: Some(wdir.to_string_lossy().into()), executable: None, ..gr.clone() };
        if !cfg!(windows) {
            crate::wine::TEST_RUNTIME.with(|t| *t.borrow_mut() = Some(Err(crate::wine::NO_WINE_MSG.into())));
            assert!(s.launch(&w).unwrap_err().contains("No Wine or Proton"));
            crate::wine::TEST_RUNTIME.with(|t| *t.borrow_mut() = Some(Ok(crate::wine::Runtime::Wine { wine: "/usr/bin/wine".into() })));
            match s.launch(&w).unwrap().action { Action::Spawn { program, args, env, .. } => { assert_eq!((program.as_str(), args), ("/usr/bin/wine", vec![wdir.join("Win.exe").to_string_lossy().to_string()])); assert!(env.iter().any(|(k, v)| k == "WINEPREFIX" && v.as_deref().map_or(false, |p| p.starts_with(d.to_string_lossy().as_ref())))); } a => panic!("{a:?}") }
        }
        // nothing to run, a missing folder, and a game that is not installed each say so
        let edir = d.join("games/Empty_3"); std::fs::create_dir_all(&edir).unwrap();
        assert!(s.launch(&GameRef { install_path: Some(edir.to_string_lossy().into()), executable: None, ..gr.clone() }).unwrap_err().contains("No supported executable"));
        assert!(s.launch(&GameRef { install_path: Some("/nonexistent/x".into()), executable: None, platform_id: "999999999".into(), ..gr.clone() }).unwrap_err().contains("not where Zest last saw"));
        assert!(s.launch(&GameRef { installed: false, ..gr }).unwrap_err().contains("not installed"));
    }
    #[test] fn butler_installs_are_read_from_its_database_and_the_right_flavour_is_picked() {
        let d = std::env::temp_dir().join(format!("zest_itch_butler_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        let (lin, win) = (d.join("lin"), d.join("win")); std::fs::create_dir_all(&lin).unwrap(); std::fs::create_dir_all(&win).unwrap(); std::fs::write(lin.join("run"), "x").unwrap(); std::fs::write(win.join("g.exe"), "x").unwrap();
        let db = d.join("butler.db");
        let c = rusqlite::Connection::open(&db).unwrap();
        c.execute_batch("CREATE TABLE caves (id TEXT, game_id INTEGER, verdict TEXT)").unwrap();
        let v = json!({"basePath": d.to_string_lossy(), "candidates": [{"flavor": "windows", "path": "win/g.exe"}, {"flavor": "linux", "path": "lin/run"}, {"flavor": "linux", "path": "lin/missing"}]});
        c.execute("INSERT INTO caves VALUES ('a', 11, ?), ('b', 12, NULL)", [v.to_string()]).unwrap();
        assert_eq!(butler_ids(&db), ["11".to_string(), "12".to_string()].into_iter().collect());
        let verdict = cave_verdict(&db, "11").unwrap();
        assert!(cave_verdict(&db, "12").is_none() && cave_verdict(&db, "99").is_none() && cave_verdict(&db, "x").is_none());
        use crate::native_exe::Os;
        assert_eq!(pick_verdict_executable(&verdict, Os::Linux), Some(("linux".to_string(), lin.join("run"))));
        assert_eq!(pick_verdict_executable(&verdict, Os::Windows), Some(("windows".to_string(), win.join("g.exe"))));
        assert_eq!(pick_verdict_executable(&verdict, Os::Mac), Some(("linux".to_string(), lin.join("run"))));   // mac prefers osx, appimage, script, then linux
        assert!(pick_verdict_executable(&json!({"basePath": d.to_string_lossy(), "candidates": [{"flavor": "linux", "path": "gone"}]}), Os::Linux).is_none());
    }
    #[test] fn install_scan_follows_the_folder_or_the_itch_app_and_uninstall_deletes_only_inside_the_games_folder() {
        let (s, _, d) = setup("scan", fake(), true);
        let db = d.join("games.db"); let c = rusqlite::Connection::open(&db).unwrap();
        c.execute_batch("DROP TABLE games; CREATE TABLE games (appid INTEGER PRIMARY KEY, platform TEXT, platform_id TEXT, install_path TEXT);").unwrap();
        let there = d.join("there"); std::fs::create_dir_all(&there).unwrap();
        c.execute("INSERT INTO games VALUES (-1,'itch_io','888888881',?), (-2,'itch_io','888888882','/gone'), (-3,'itch_io','888888883',NULL), (-4,'gog','888888884',NULL)", [there.to_string_lossy()]).unwrap();
        let rows = s.installed_scan().unwrap();
        let by = |id: &str| rows.iter().find(|r| r.platform_id == id).map(|r| r.installed);
        assert_eq!((by("888888881"), by("888888882"), by("888888883"), by("888888884")), (Some(true), Some(false), Some(false), None));
        let inside = s.dir().join("Game_1").to_string_lossy().to_string();
        let g = |p: Option<&str>| GameRef { name: "G".into(), install_path: p.map(String::from), ..Default::default() };
        assert_eq!(s.uninstall(&g(Some(&inside))).unwrap(), Action::RemoveInstall { dir: inside.clone(), delete: true });
        assert_eq!(s.uninstall(&g(Some("/elsewhere/Game"))).unwrap(), Action::RemoveInstall { dir: "/elsewhere/Game".into(), delete: false });
        assert!(s.uninstall(&g(None)).unwrap_err().contains("itch app"));
        assert!(s.caps().launch && s.caps().uninstall && s.caps().installed_scan);
    }
    #[test] fn a_rejected_key_or_no_answer_stamps_nothing() {
        for st in [401u16, 0] {
            let (s, site, d) = setup(&format!("rej{st}"), Site { game_status: Some(st), ..fake() }, true);
            assert!(s.rescrape(&cx(&d, site), &g("11")).unwrap().is_none(), "status {st}");
        }
    }
}
