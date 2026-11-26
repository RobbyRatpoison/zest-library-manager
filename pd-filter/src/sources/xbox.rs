//! Xbox / Game Pass for PC as a library source (port of PlayDate's `xbox` plugin: `xbox_live.py`, `__init__.py` and the sign-in route in `routes.py`; install, launch, uninstall and the
//! Xodus Nexus import wait for the shared runners layer). Sign-in is Microsoft's own: a Live sign-in page that ends on a blank page whose address carries `code=` (the user pastes it),
//! exchanged for a Live token, then an Xbox user token, then an XSTS token that every Xbox call carries. The library is the account's title history, filtered to titles with a Package
//! Family Name (real Store packages, not Steam games that merely report achievements), with playtime from the user stats service. Tokens live in `config.json["xbox"]` as PlayDate keeps
//! them (the XSTS expiry stays a number) and are never logged. Microsoft rotates the Live refresh token on every use, so the new one is saved before the next step.
use super::{Action, AuthBegin, AuthStatus, Caps, GameRef, InstalledRow, Launch, LibrarySource, SourceCtx};
use crate::importer::GameRecord;
use crate::jobs::Update;
use crate::net::{enc, log, set_activity, Cancel, Http};
use crate::steamclient::Fetch;
use regex::Regex;
use rusqlite::types::Value as Sql;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

const CLIENT_ID: &str = "38cd2fa8-66fd-4760-afb2-405eb65d5b0c";
const REDIRECT_URI: &str = "https://login.live.com/oauth20_desktop.srf";
const SCOPE: &str = "Xboxlive.signin Xboxlive.offline_access";
const AUTHORIZE_URL: &str = "https://login.live.com/oauth20_authorize.srf";
const TOKEN_URL: &str = "https://login.live.com/oauth20_token.srf";
const USER_AUTH_URL: &str = "https://user.auth.xboxlive.com/user/authenticate";
const XSTS_URL: &str = "https://xsts.auth.xboxlive.com/xsts/authorize";
const PROFILE_URL: &str = "https://profile.xboxlive.com/users/me/profile/settings?settings=Gamertag";
const STATS_URL: &str = "https://userstats.xboxlive.com/batch";
const EXPIRED: &str = "Xbox session expired \u{2014} please reconnect";

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex { cell.get_or_init(|| Regex::new(pat).unwrap()) }
fn now() -> i64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0) }
fn to_i64(v: &Value) -> Option<i64> { match v { Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f.trunc() as i64)), Value::String(s) => s.trim().parse().ok(), _ => None } }
fn id_str(v: Option<&Value>) -> String { match v { Some(Value::String(s)) => s.clone(), Some(Value::Number(n)) => n.to_string(), _ => String::new() } }
fn text<'a>(v: &'a Value, k: &str) -> &'a str { v.get(k).and_then(|x| x.as_str()).unwrap_or("") }
fn truthy(v: Option<&Value>) -> bool { match v { None | Some(Value::Null) => false, Some(Value::Bool(b)) => *b, Some(Value::String(s)) => !s.is_empty(), Some(Value::Number(n)) => n.as_f64() != Some(0.0), Some(Value::Array(a)) => !a.is_empty(), Some(Value::Object(o)) => !o.is_empty() } }

// ---------------------------------------------------------------- pure helpers (checked against the Python)

/// `get_auth_url`: Microsoft's sign-in page for the Xbox app id.
pub fn auth_url() -> String {
    let q = |s: &str| enc(s).replace("%20", "+");
    let params = [("client_id", CLIENT_ID), ("response_type", "code"), ("approval_prompt", "auto"), ("scope", SCOPE), ("redirect_uri", REDIRECT_URI)];
    format!("{AUTHORIZE_URL}?{}", params.iter().map(|(k, v)| format!("{}={}", q(k), q(v))).collect::<Vec<_>>().join("&"))
}
/// `_parse_not_after`: the XSTS expiry (`2026-11-02T10:00:00.1234567Z`) as a timestamp, 0 when absent or unreadable.
pub fn parse_not_after(raw: &str) -> i64 {
    static R: OnceLock<Regex> = OnceLock::new();
    let t = raw.trim_end_matches('Z'); let t = t.split('.').next().unwrap_or("");
    let Some(c) = re(&R, r"^(\d{4})-(\d{1,2})-(\d{1,2})T(\d{1,2}):(\d{1,2}):(\d{1,2})$").captures(t) else { return 0 };
    let n = |i: usize| c[i].parse::<i64>().unwrap_or(0);
    super::itch_io::utc_ts(n(1), n(2), n(3), n(4), n(5), n(6)).unwrap_or(0)
}
/// `_parse_iso_ts`: an ISO 8601 time as a timestamp (a trailing Z is dropped, fractions beyond microseconds are cut, any written offset is ignored and the time read as UTC).
pub fn parse_iso_ts(raw: &str) -> Option<i64> {
    static R: OnceLock<Regex> = OnceLock::new();
    if raw.is_empty() { return None; }
    let mut s = raw.trim_end_matches('Z').to_string();
    if let Some((head, frac)) = s.split_once('.') { s = format!("{head}.{}", frac.chars().take(6).collect::<String>()); }
    let c = re(&R, r"^(\d{4})-(\d{2})-(\d{2})(?:[T ](\d{2})(?::(\d{2})(?::(\d{2})(?:\.\d{1,6})?)?)?)?(?:[+-]\d{2}(?::?\d{2})?)?$").captures(&s)?;
    let n = |i: usize| c.get(i).map_or(0, |m| m.as_str().parse::<i64>().unwrap_or(0));
    super::itch_io::utc_ts(n(1), n(2), n(3), n(4), n(5), n(6))
}
/// `_is_pc_title`: a real Store package has a Package Family Name.
pub fn is_pc_title(t: &Value) -> bool { truthy(t.get("pfn")) }
/// `_extract_store_id`: the Microsoft Store product id, used for the store link and for launching.
pub fn store_id(t: &Value) -> Option<String> {
    t.get("detail").and_then(|d| d.get("availabilities")).and_then(|a| a.as_array()).and_then(|a| a.first()).and_then(|a| a.get("ProductId")).and_then(|p| p.as_str()).filter(|p| !p.is_empty()).map(String::from)
}
/// `_build_metadata`: developer, publisher, genres, release date and (only when the total is known) achievements.
pub fn build_metadata(t: &Value, today: &str) -> Update {
    let s = |x: String| Sql::Text(x);
    let detail = t.get("detail").filter(|d| d.is_object()).cloned().unwrap_or(json!({}));
    let mut out: Update = vec![];
    if truthy(detail.get("developerName")) { out.push(("developers", s(text(&detail, "developerName").to_string()))); }
    if truthy(detail.get("publisherName")) { out.push(("publishers", s(text(&detail, "publisherName").to_string()))); }
    if let Some(g) = detail.get("genres").and_then(|g| g.as_array()).filter(|g| !g.is_empty()) { out.push(("genres", s(g.iter().map(|x| x.as_str().unwrap_or("")).collect::<Vec<_>>().join(",")))); }
    if let Some(ts) = parse_iso_ts(text(&detail, "releaseDate")).filter(|t| *t != 0) { out.push(("release_date", Sql::Integer(ts))); }
    // totalAchievements is unreliable on this endpoint (0 even when progress is filled in), so nothing is written unless a total is really there
    if let Some(a) = t.get("achievement").filter(|a| truthy(a.get("totalAchievements"))) {
        out.push(("unlocked_achievements", Sql::Integer(a.get("currentAchievements").filter(|c| truthy(Some(c))).and_then(to_i64).unwrap_or(0))));
        out.push(("total_achievements", Sql::Integer(a.get("totalAchievements").and_then(to_i64).unwrap_or(0))));
        out.push(("cheevos_fetched", s(today.to_string())));
    }
    out
}
/// `_pick_xbox_image`: the title's own artwork for a slot, best type first.
pub fn pick_image(t: &Value, kind: &str) -> Option<String> {
    let mut by_type: HashMap<String, String> = HashMap::new();
    for img in t.get("images").and_then(|i| i.as_array()).into_iter().flatten() {
        let (ty, url) = (text(img, "type"), text(img, "url"));
        if !ty.is_empty() && !url.is_empty() { by_type.entry(ty.to_string()).or_insert_with(|| url.to_string()); }
    }
    let order: &[&str] = match kind { "vertical" => &["Poster", "BoxArt"], "horizontal" => &["TitledHeroArt", "Hero", "SuperHeroArt"], "icon" => &["Logo"], _ => &[] };
    order.iter().find_map(|t| by_type.get(*t).cloned())
}
/// `_fetch_playtimes`' parsing: `{titleId: minutes}` from the user stats answer.
pub fn playtimes_from(v: &Value) -> HashMap<String, i64> {
    let mut out = HashMap::new();
    for coll in v.get("statlistscollection").and_then(|c| c.as_array()).into_iter().flatten() {
        for stat in coll.get("stats").and_then(|s| s.as_array()).into_iter().flatten() {
            let id = id_str(stat.get("titleid")); if id.is_empty() { continue; }
            out.insert(id, stat.get("value").filter(|v| truthy(Some(v))).and_then(to_i64).unwrap_or(0));
        }
    }
    out
}
/// The library records for a title history: PC titles only, playtime and last played from the stats, new games below `min_play` minutes left out (`known` = games already imported).
pub fn records_from(titles: &[Value], playtimes: &HashMap<String, i64>, known: &HashSet<String>, min_play: i64, now: i64) -> Vec<GameRecord> {
    let mut out = vec![];
    let mut seen = HashSet::new();
    for t in titles.iter().filter(|t| is_pc_title(t)) {
        let (id, name) = (id_str(t.get("titleId")), text(t, "name").to_string());
        if id.is_empty() || name.is_empty() || !seen.insert(id.clone()) { continue; }
        let minutes = playtimes.get(&id).copied();
        if !known.contains(&id) && min_play > 0 && minutes.unwrap_or(0) < min_play { continue; }
        let meta = build_metadata(t, "");
        let get = |c: &str| meta.iter().find(|(k, _)| *k == c).and_then(|(_, v)| if let Sql::Text(s) = v { Some(s.clone()) } else { None });
        out.push(GameRecord { platform_id: id, name, slug: store_id(t), appname: Some(text(t, "pfn").to_string()).filter(|p| !p.is_empty()), playtime_forever: minutes,
            last_played: t.get("titleHistory").and_then(|h| h.get("lastTimePlayed")).and_then(|l| l.as_str()).and_then(parse_iso_ts), date_added: Some(now),
            developers: get("developers"), publishers: get("publishers"), genres: get("genres"),
            release_date: meta.iter().find(|(k, _)| *k == "release_date").and_then(|(_, v)| if let Sql::Integer(i) = v { Some(*i) } else { None }), ..Default::default() });
    }
    out
}

/// A package family name or Store ID that is safe to put inside a PowerShell string.
fn safe_token(s: &str) -> bool { !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')) }

fn powershell(script: &str) -> Option<String> {
    let o = { use crate::proc::Hide; std::process::Command::new("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command", script]).stdin(std::process::Stdio::null()).hidden().output().ok()? };
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
}
/// The installed Appx packages of this user, by lower-cased package family name -> install folder (`Get-AppxPackage`; no admin needed).
pub fn parse_appx(json: &str) -> HashMap<String, String> {
    let v: Value = serde_json::from_str(json).unwrap_or(Value::Null);
    let items: Vec<Value> = match v { Value::Array(a) => a, Value::Object(_) => vec![v], _ => vec![] };
    items.iter().filter_map(|p| Some((p.get("PackageFamilyName")?.as_str()?.to_lowercase(), p.get("InstallLocation").and_then(|l| l.as_str()).unwrap_or("").to_string()))).collect()
}
fn uwp_packages() -> Option<HashMap<String, String>> { powershell("Get-AppxPackage | Select-Object PackageFamilyName, InstallLocation | ConvertTo-Json -Compress").map(|o| parse_appx(&o)) }
/// The launchable Application User Model ID (`<family>!<AppId>`) from the Start menu's list.
fn uwp_aumid(pfn: &str) -> Option<String> { powershell(&format!("Get-StartApps | Where-Object {{$_.AppID -like '{pfn}!*'}} | Select-Object -First 1 -ExpandProperty AppID")).filter(|a| !a.is_empty()) }

// ---------------------------------------------------------------- the source

pub struct XboxSource { http: Arc<dyn Http>, config_dir: PathBuf, db: PathBuf, lock: Mutex<()>, home: PathBuf }

impl XboxSource {
    pub fn new(http: Arc<dyn Http>, config_dir: PathBuf, db: PathBuf) -> Self { XboxSource { http, config_dir, db, lock: Mutex::new(()), home: crate::installdir::home() } }
    fn cfg_path(&self) -> PathBuf { self.config_dir.join("config.json") }
    fn root(&self) -> Value { std::fs::read_to_string(self.cfg_path()).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).unwrap_or_else(|| json!({})) }
    fn section(&self) -> Map<String, Value> { crate::account::plugin_section(&self.config_dir, "xbox") }
    /// Edits `config.json["xbox"]` keeping every other key (`None` removes the section).
    fn edit(&self, f: impl FnOnce(&mut Map<String, Value>)) {
        crate::account::plugin_section_edit(&self.config_dir, "xbox", f);   // secrets go to the keyring, the rest stays in config.json
    }
    fn str_of(&self, k: &str) -> Option<String> { self.section().get(k).and_then(|v| v.as_str()).filter(|v| !v.is_empty()).map(String::from) }
    /// Minutes a new game needs before it is imported (`xbox_settings.min_playtime`; 0 = everything). Set in PlayDate's plugin card, read here.
    fn min_playtime(&self) -> i64 { self.root().get("xbox_settings").and_then(|s| s.get("min_playtime")).and_then(to_i64).unwrap_or(0).max(0) }
    fn known(&self) -> HashSet<String> {
        let Ok(c) = rusqlite::Connection::open_with_flags(&self.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { return HashSet::new() };
        let Ok(mut st) = c.prepare("SELECT platform_id FROM games WHERE platform = 'xbox' AND platform_id IS NOT NULL") else { return HashSet::new() };
        st.query_map([], |r| r.get::<_, String>(0)).into_iter().flatten().flatten().collect()
    }

    fn send(&self, url: &str, headers: &[(&str, &str)], body: &str, secs: u64) -> Result<crate::net::Response, String> { self.http.post(url, headers, body, Duration::from_secs(secs)).map_err(|e| e.to_string()) }
    /// Steps 3 and 4: a Live access token -> user token -> (XSTS token, uhs, xuid, gamertag, expiry).
    fn xsts_for(&self, live_access: &str) -> Result<(String, String, String, String, i64), String> {
        let h = [("Content-Type", "application/json"), ("x-xbl-contract-version", "1")];
        let body = json!({"RelyingParty": "http://auth.xboxlive.com", "TokenType": "JWT", "Properties": {"AuthMethod": "RPS", "SiteName": "user.auth.xboxlive.com", "RpsTicket": format!("d={live_access}")}});
        let r = self.send(USER_AUTH_URL, &h, &body.to_string(), 15)?;
        if r.status >= 400 { return Err(format!("Xbox Live auth failed (HTTP {})", r.status)); }
        let user: Value = serde_json::from_str(&r.body).map_err(|_| "Xbox Live auth failed: unexpected answer".to_string())?;
        let user_token = user.get("Token").and_then(|t| t.as_str()).ok_or("Xbox Live auth failed: no user token")?;
        let body = json!({"RelyingParty": "http://xboxlive.com", "TokenType": "JWT", "Properties": {"SandboxId": "RETAIL", "UserTokens": [user_token]}});
        let r = self.send(XSTS_URL, &h, &body.to_string(), 15)?;
        if r.status >= 400 { return Err(format!("Xbox Live auth failed (HTTP {})", r.status)); }
        let d: Value = serde_json::from_str(&r.body).map_err(|_| "Xbox Live auth failed: unexpected answer".to_string())?;
        let claims = d.get("DisplayClaims").and_then(|c| c.get("xui")).and_then(|x| x.as_array()).and_then(|x| x.first()).ok_or("Xbox Live auth failed: no account claims")?;
        let token = d.get("Token").and_then(|t| t.as_str()).ok_or("Xbox Live auth failed: no XSTS token")?;
        let (uhs, xuid) = (text(claims, "uhs"), text(claims, "xid"));
        if uhs.is_empty() || xuid.is_empty() { return Err("Xbox Live auth failed: no account claims".into()); }
        Ok((token.to_string(), uhs.to_string(), xuid.to_string(), claims.get("gtg").and_then(|g| g.as_str()).unwrap_or("Connected").to_string(), parse_not_after(text(&d, "NotAfter"))))
    }
    fn live_form(fields: &[(&str, &str)]) -> String { fields.iter().map(|(k, v)| format!("{}={}", enc(k), enc(v))).collect::<Vec<_>>().join("&") }
    /// `refresh_session`: Some(true) = refreshed, Some(false) = Microsoft refused the refresh token (sign in again), None = could not be done (nothing consumed or kept for later).
    fn refresh_session(&self) -> Option<bool> {
        let _g = self.lock.lock().ok()?;
        let rt = self.str_of("refresh_token")?;
        let form = Self::live_form(&[("client_id", CLIENT_ID), ("grant_type", "refresh_token"), ("refresh_token", &rt), ("scope", SCOPE), ("redirect_uri", REDIRECT_URI)]);
        let r = match self.send(TOKEN_URL, &[("Content-Type", "application/x-www-form-urlencoded")], &form, 15) { Ok(r) => r, Err(e) => { log(&format!("Xbox refresh: could not reach Microsoft: {e}")); return None } };
        if matches!(r.status, 400 | 401) { log(&format!("Xbox refresh rejected (HTTP {}): sign in again", r.status)); return Some(false); }
        if r.status >= 400 { return None; }
        let live: Value = serde_json::from_str(&r.body).ok()?;
        // the new refresh token is kept before the next step, so a failure after this still leaves the account refreshable
        if let Some(n) = live.get("refresh_token").and_then(|t| t.as_str()).filter(|t| !t.is_empty()) { self.edit(|m| { m.insert("refresh_token".into(), json!(n)); }); }
        match self.xsts_for(live.get("access_token").and_then(|t| t.as_str())?) {
            Ok((token, uhs, xuid, _, exp)) => { self.edit(|m| { m.insert("xsts_token".into(), json!(token)); m.insert("uhs".into(), json!(uhs)); m.insert("xuid".into(), json!(xuid)); m.insert("xsts_expires_at".into(), json!(exp)); }); log("Xbox: session refreshed"); Some(true) }
            Err(e) => { log(&format!("Xbox refresh: XBL/XSTS step failed: {e}")); None }
        }
    }
    /// Refresh when the XSTS token is within five minutes of expiry, or its expiry was never recorded.
    fn ensure_fresh(&self) {
        if self.str_of("refresh_token").is_none() { return; }
        let exp = self.section().get("xsts_expires_at").and_then(to_i64).unwrap_or(0);
        if exp != 0 && now() < exp - 300 { return; }
        self.refresh_session();
    }
    fn auth_headers(&self) -> Vec<(String, String)> {
        self.ensure_fresh();
        let s = self.section(); let g = |k: &str| s.get(k).and_then(|v| v.as_str()).unwrap_or("None").to_string();
        vec![("Authorization".into(), format!("XBL3.0 x={};{}", g("uhs"), g("xsts_token"))), ("x-xbl-contract-version".into(), "2".into()), ("Accept-Language".into(), "en-US".into())]
    }
    fn api_get(&self, url: &str, secs: u64) -> Result<crate::net::Response, String> {
        let h = self.auth_headers(); let hs: Vec<(&str, &str)> = h.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        self.http.get(url, &hs, Duration::from_secs(secs)).map_err(|e| e.to_string())
    }
    fn api_post(&self, url: &str, body: &Value, secs: u64) -> Result<crate::net::Response, String> {
        let mut h = self.auth_headers(); h.push(("Content-Type".into(), "application/json".into()));
        let hs: Vec<(&str, &str)> = h.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        self.http.post(url, &hs, &body.to_string(), Duration::from_secs(secs)).map_err(|e| e.to_string())
    }
    /// `check_connection`: true = accepted, false = sign in again, None = could not tell.
    fn check(&self) -> Option<bool> {
        self.str_of("xsts_token")?;
        for attempt in 1..=2 {
            let r = self.api_get(PROFILE_URL, 15).ok()?;
            if r.status == 200 { return Some(true); }
            if matches!(r.status, 401 | 403) && attempt == 1 { match self.refresh_session() { None => return None, Some(false) => return Some(false), Some(true) => continue } }
            return if matches!(r.status, 401 | 403) { Some(false) } else { None };
        }
        Some(false)
    }
    /// `_fetch_playtimes`: one batched request for every title; a failure is logged and gives nothing.
    fn playtimes(&self, xuid: &str, ids: &[String]) -> HashMap<String, i64> {
        if ids.is_empty() { return HashMap::new(); }
        let body = json!({"arrangebyfield": "xuid", "stats": ids.iter().map(|t| json!({"name": "MinutesPlayed", "titleid": t})).collect::<Vec<_>>(), "xuids": [xuid]});
        match self.api_post(STATS_URL, &body, 20) {
            Ok(r) if r.status < 400 => serde_json::from_str::<Value>(&r.body).map(|v| playtimes_from(&v)).unwrap_or_default(),
            Ok(r) => { log(&format!("Xbox: playtime fetch failed: HTTP {}", r.status)); HashMap::new() }
            Err(e) => { log(&format!("Xbox: playtime fetch failed: {e}")); HashMap::new() }
        }
    }
    /// `_fetch_single_title`: one title by Package Family Name.
    fn single_title(&self, pfn: &str, fields: &str) -> Result<Option<Value>, String> {
        let r = self.api_post(&format!("https://titlehub.xboxlive.com/titles/batch/decoration/{fields}"), &json!({"pfns": [pfn], "windowsPhoneProductIds": []}), 15)?;
        if r.status >= 400 { return Err(format!("HTTP {}", r.status)); }
        Ok(serde_json::from_str::<Value>(&r.body).ok().and_then(|v| v.get("titles").and_then(|t| t.as_array()).and_then(|t| t.first()).cloned()))
    }
    /// The title's own artwork (`art_urls`), any subset of vertical, horizontal and icon.
    pub fn art_urls(&self, pfn: &str) -> Vec<(&'static str, String)> {
        if self.str_of("xsts_token").is_none() || pfn.is_empty() { return vec![]; }
        let Ok(Some(t)) = self.single_title(pfn, "detail,image,achievement") else { return vec![] };
        ["vertical", "horizontal", "icon"].iter().filter_map(|k| pick_image(&t, k).map(|u| (*k, u))).collect()
    }
}

impl LibrarySource for XboxSource {
    fn id(&self) -> &str { "xbox" }
    fn label(&self) -> &str { "Xbox" }
    fn caps(&self) -> Caps { Caps { auth: true, sync: true, rescrape: true, description: true, art: true, store_page: true, launch: true, uninstall: true, installed_scan: true, ..Default::default() } }

    /// Linux: Nexus's install root, so an install, uninstall or deleted folder shows up while Zest is open (deep: a game turns "ready" when its marker file lands inside its folder).
    fn watch_paths(&self) -> Vec<(PathBuf, bool)> { if cfg!(target_os = "linux") { vec![(super::xbox_nexus::install_root(&self.home), true)] } else { vec![] } }

    /// Xbox games the store tells apart from emulated Xbox ROMs by having a Store ID or package name. Linux: Xodus Nexus's game folders; Windows: the Appx packages.
    fn installed_scan(&self) -> Option<Vec<InstalledRow>> {
        let c = rusqlite::Connection::open_with_flags(&self.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
        let mut st = c.prepare("SELECT platform_id, COALESCE(platform_slug, ''), COALESCE(platform_appname, '') FROM games WHERE platform = 'xbox' AND platform_id IS NOT NULL AND (COALESCE(platform_slug, '') != '' OR COALESCE(platform_appname, '') != '')").ok()?;
        let games: Vec<(String, String, String)> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).ok()?.flatten().collect();
        if cfg!(windows) {
            let pk = uwp_packages()?;
            return Some(games.into_iter().map(|(id, _, pfn)| match pk.get(&pfn.to_lowercase()) { Some(loc) => InstalledRow { platform_id: id, installed: true, install_path: Some(loc.clone()) }, None => InstalledRow { platform_id: id, installed: false, install_path: None } }).collect());
        }
        if !cfg!(target_os = "linux") { return None; }
        // no Nexus found means nothing is installed through it
        let found = super::xbox_nexus::scan_installed(&self.home);
        Some(games.into_iter().map(|(id, slug, _)| match found.get(&slug.to_uppercase()).filter(|i| i.ready) {
            Some(i) => InstalledRow { platform_id: id, installed: true, install_path: Some(i.path.to_string_lossy().to_string()) },
            None => InstalledRow { platform_id: id, installed: false, install_path: None },
        }).collect())
    }
    fn launch(&self, g: &GameRef) -> Result<Launch, String> {
        use super::xbox_nexus as nx;
        if cfg!(target_os = "linux") {
            if g.slug.is_empty() { return Err("Missing Store ID for this game.".into()); }
            let launcher = nx::find_launcher(&self.home).ok_or_else(|| format!("Xodus Nexus was not found. {}", nx::install_hint()))?;
            let spawn = |args: Vec<String>| Action::Spawn { program: launcher.to_string_lossy().to_string(), args, dir: None, env: vec![] };
            // Nexus cannot be told to install a game: open it and let the user do that there (nothing is recorded as played)
            let hand_off = |msg: &str| Ok(Launch { action: Action::Many(vec![spawn(vec![]), Action::Message(msg.into())]), record: false });
            return match nx::scan_installed(&self.home).get(&g.slug.to_uppercase()) {
                None => hand_off("Opened Xodus Nexus. Install this game there, then launch it from Zest."),
                Some(i) if !i.ready => hand_off("This game's install in Xodus Nexus is incomplete. Opened Nexus so you can finish it there."),
                Some(i) => Ok(Launch { action: spawn(vec!["--launch-game".into(), i.id.clone()]), record: true }),
            };
        }
        if cfg!(windows) {
            if g.appname.is_empty() || !safe_token(&g.appname) { return Err("Missing package info for this game.".into()); }
            if uwp_packages().map_or(false, |p| !p.contains_key(&g.appname.to_lowercase())) {
                if !g.slug.is_empty() && safe_token(&g.slug) { return Ok(Launch { action: Action::Many(vec![Action::OpenUrl(format!("ms-windows-store://pdp/?productid={}", g.slug)), Action::Message("Not installed: opened the Microsoft Store so you can install it. Launch again once it finishes.".into())]), record: false }); }
                return Err("Game is not installed and has no Store link.".into());
            }
            let aumid = uwp_aumid(&g.appname).ok_or("Could not find this game in the Start Menu. Is it still installed?")?;
            return Ok(Launch { action: Action::Spawn { program: "explorer.exe".into(), args: vec![format!("shell:AppsFolder\\{aumid}")], dir: None, env: vec![] }, record: true });
        }
        Err("Xbox / Microsoft Store games can only be installed and launched on Windows and, through Xodus Nexus, on Linux.".into())
    }
    fn uninstall(&self, g: &GameRef) -> Result<Action, String> {
        if cfg!(windows) {
            if !safe_token(&g.appname) { return Err("Missing package info for this game.".into()); }
            let script = format!("Get-AppxPackage | Where-Object {{$_.PackageFamilyName -eq '{}'}} | Remove-AppxPackage", g.appname);
            return Ok(Action::Many(vec![Action::Spawn { program: "powershell.exe".into(), args: vec!["-NoProfile".into(), "-NonInteractive".into(), "-Command".into(), script], dir: None, env: vec![] }, Action::Message("Uninstalling. It disappears from the installed list at the next check.".into())]));
        }
        if cfg!(target_os = "linux") {
            // as Nexus's own uninstall does, while Nexus is closed (it would write its old list back); open, the user does it there
            if g.slug.is_empty() { return Err("Missing Store ID for this game.".into()); }
            // the tests never look at this machine's real processes
            let running: &dyn Fn() -> bool = if cfg!(test) { &|| false } else { &super::xbox_nexus::nexus_running };
            let dir = super::xbox_nexus::remove_game(&self.home, &g.slug, running)?;
            // the folder is gone already: the app only marks the game not installed and says so
            return Ok(Action::RemoveInstall { dir: dir.to_string_lossy().to_string(), delete: true });
        }
        Err("Xbox / Microsoft Store games can only be uninstalled on Windows and, through Xodus Nexus, on Linux.".into())
    }
    fn auth_status(&self) -> Result<AuthStatus, String> {
        if self.str_of("xsts_token").is_none() { return Ok(AuthStatus { connected: false, username: None }); }
        // asked each time the dialog opens, as PlayDate's status route did: a refused login shows as signed out
        if self.check() == Some(false) { return Ok(AuthStatus { connected: false, username: None }); }
        Ok(AuthStatus { connected: true, username: Some(self.str_of("gamertag").unwrap_or_else(|| "Connected".into())) })
    }
    fn paste_login_kept(&self) -> bool { false }   // the sign-in window works (confirmed live 2026-11-22)
    fn web_login_done(&self) -> Option<serde_json::Value> { Some(serde_json::json!({"kind": "url", "contains": "oauth20_desktop.srf?code="})) }
    fn auth_begin(&self) -> Result<AuthBegin, String> {
        Ok(AuthBegin { url: auth_url(), placeholder: "Paste the address of the blank page you land on".into(), instructions: vec![
            "Sign in to your Microsoft account in your browser.".into(),
            "You land on a blank page. Copy the full address from the address bar; it contains code=.".into(),
            "Paste that address below. Games you only play through Game Pass and no longer have access to still show up: Microsoft does not say which titles you can open now.".into()] })
    }
    fn auth_complete(&self, value: &str) -> Result<AuthStatus, String> {
        let code = super::gog::parse_code(value)?;
        let form = Self::live_form(&[("client_id", CLIENT_ID), ("code", &code), ("grant_type", "authorization_code"), ("redirect_uri", REDIRECT_URI), ("scope", SCOPE)]);
        let r = self.send(TOKEN_URL, &[("Content-Type", "application/x-www-form-urlencoded")], &form, 15).map_err(|e| { log(&format!("Xbox: {e}")); format!("Xbox Live auth failed: {e}") })?;
        if r.status >= 400 { log(&format!("Xbox: Live token exchange answered HTTP {}", r.status)); return Err(format!("Xbox Live auth failed (HTTP {})", r.status)); }
        let live: Value = serde_json::from_str(&r.body).map_err(|_| "Xbox Live auth failed: unexpected answer".to_string())?;
        let access = live.get("access_token").and_then(|t| t.as_str()).ok_or("Xbox Live auth failed: no access token")?;
        let (token, uhs, xuid, gamertag, exp) = self.xsts_for(access)?;
        let refresh = live.get("refresh_token").and_then(|t| t.as_str()).map(String::from);
        // a new login replaces the whole section, as PlayDate does
        self.edit(|m| { m.clear(); m.insert("refresh_token".into(), refresh.map_or(Value::Null, |r| json!(r))); m.insert("xsts_token".into(), json!(token)); m.insert("uhs".into(), json!(uhs));
            m.insert("xuid".into(), json!(xuid)); m.insert("gamertag".into(), json!(gamertag)); m.insert("xsts_expires_at".into(), json!(exp)); });
        log(&format!("Xbox connected as {gamertag:?}"));
        Ok(AuthStatus { connected: true, username: Some(gamertag) })
    }
    fn auth_disconnect(&self) -> Result<(), String> {
        crate::account::plugin_remove(&self.config_dir, "xbox").map_err(|_| "Could not save".to_string())
    }

    fn art_urls(&self, g: &GameRef) -> Vec<(String, String)> { Self::art_urls(self, &g.appname).into_iter().map(|(k, v)| (k.to_string(), v)).collect() }
    fn store_url(&self, g: &GameRef) -> Option<String> { Some(g.slug.clone()).filter(|s| !s.is_empty()).map(|s| format!("https://www.xbox.com/en-US/games/store/game/{s}")) }
    fn enrich_new(&self) -> bool { true }

    fn sync(&self, cx: &SourceCtx, sink: &mut dyn FnMut(Vec<GameRecord>), cancel: &Cancel) -> Result<usize, String> {
        let xuid = self.str_of("xuid").ok_or("Not connected")?;
        set_activity("Xbox: reading your title history");
        let r = self.api_get(&format!("https://titlehub.xboxlive.com/users/xuid({xuid})/titles/titlehistory/decoration/detail,image,achievement"), 20).map_err(|e| format!("Could not reach Xbox: {e}"))?;
        if matches!(r.status, 401 | 403) { return Err(EXPIRED.into()); }
        if r.status >= 400 { return Err(format!("Xbox answered HTTP {}", r.status)); }
        let titles: Vec<Value> = serde_json::from_str::<Value>(&r.body).ok().and_then(|v| v.get("titles").and_then(|t| t.as_array()).cloned()).unwrap_or_default();
        let owned: Vec<&Value> = titles.iter().filter(|t| is_pc_title(t)).collect();
        log(&format!("Xbox sync: {} of {} titles are real Xbox PC titles (have a pfn)", owned.len(), titles.len()));
        if cancel.is_set() { return Ok(0); }
        set_activity("Xbox: reading playtime");
        let ids: Vec<String> = owned.iter().map(|t| id_str(t.get("titleId"))).filter(|i| !i.is_empty()).collect();
        let playtimes = self.playtimes(&xuid, &ids);
        // what the account holds now is Xodus Nexus's list when Nexus is set up (never in the tests, which must not reach the real one); the play history then only fills in playtime and details
        let home = crate::installdir::home();
        let nexus = if cfg!(test) { None } else { crate::xbox_owned::backend_binary(&home) };
        let held = nexus.and_then(|_| {
            set_activity("Xbox: asking Xodus Nexus what you own");
            match crate::xbox_owned::fetch(&home, cx.http.as_ref(), now()) { Ok(v) => Some(v), Err(e) => { log(&format!("Xbox sync: the owned list from Nexus was not used: {e}")); None } }
        });
        let min_play = if held.is_some() { 0 } else { self.min_playtime() };   // every owned game is wanted, however little it was played
        let records = records_from(&titles, &playtimes, &self.known(), min_play, now());
        let mut n = records.len();
        if n > 0 { sink(records); }
        if let Some((kinds, catalog)) = held {
            if cancel.is_set() { return Ok(n); }
            // the owned games the history never listed are added from the catalog, and every game gets its Owned / Game Pass / Played, not owned group
            let mut conn = rusqlite::Connection::open(&cx.db).map_err(|e| e.to_string())?;
            conn.busy_timeout(Duration::from_secs(10)).ok();
            let (added, marked) = crate::xbox_owned::apply(&mut conn, &kinds, &catalog, now()).map_err(|e| e.to_string())?;
            log(&format!("Xbox sync: Nexus lists {} owned or Game Pass products; {added} added from the catalog, {marked} marked", kinds.len()));
            n += added;
        }
        Ok(n)
    }

    fn rescrape(&self, cx: &SourceCtx, g: &GameRef) -> Fetch<Option<Update>> {
        if g.appname.is_empty() || self.str_of("xsts_token").is_none() { return Ok(None); }
        let title = match self.single_title(&g.appname, "detail,image,achievement") { Ok(Some(t)) => t, Ok(None) => return Ok(None), Err(e) => { log(&format!("Xbox: rescrape fetch failed for appid {}: {e}", g.appid)); return Ok(None) } };
        let mut out = build_metadata(&title, &cx.today);
        out.push(("meta_fetched", Sql::Text(cx.today.clone())));
        let tid = id_str(title.get("titleId"));
        if let Some(m) = self.playtimes(&self.str_of("xuid").unwrap_or_default(), &[tid.clone()]).get(&tid) { out.push(("playtime_forever", Sql::Integer(*m))); }
        Ok(Some(out))
    }
    fn description(&self, _cx: &SourceCtx, g: &GameRef) -> Option<String> {
        if g.appname.is_empty() { return None; }
        let t = self.single_title(&g.appname, "detail").ok().flatten()?;
        let d = t.get("detail")?;
        Some(text(d, "shortDescription").to_string()).filter(|s| !s.is_empty()).or_else(|| Some(text(d, "description").to_string()).filter(|s| !s.is_empty()))
    }
}

#[cfg(test)]
mod behaviour {
    use super::*;
    use crate::net::{NetError, Response};

    /// A fake Microsoft and Xbox Live: the Live token endpoint, the two Xbox token steps, title history, the batch lookups and the profile check.
    struct Site { live_status: u16, history_status: u16, profile_status: u16, refresh_status: u16, seen: Mutex<Vec<(String, String)>> }
    fn fake() -> Site { Site { live_status: 200, history_status: 200, profile_status: 200, refresh_status: 200, seen: Mutex::new(vec![]) } }
    fn title(id: &str, name: &str, pfn: Option<&str>) -> Value {
        json!({"titleId": id, "name": name, "pfn": pfn, "detail": {"developerName": "Dev", "publisherName": "Pub", "genres": ["Action", "Racing"], "releaseDate": "2019-03-04T10:00:00.1234567Z", "availabilities": [{"ProductId": format!("STORE{id}")}], "shortDescription": "Vroom"},
            "titleHistory": {"lastTimePlayed": "2021-06-01T12:00:00Z"}, "achievement": {"currentAchievements": 4, "totalAchievements": 10}, "images": [{"type": "Poster", "url": "https://img/p.jpg"}, {"type": "Logo", "url": "https://img/l.png"}]})
    }
    impl Http for Site {
        fn get(&self, u: &str, h: &[(&str, &str)], _t: Duration) -> Result<Response, NetError> {
            self.seen.lock().unwrap().push((u.to_string(), String::new()));
            let auth = h.iter().find(|(k, _)| *k == "Authorization").map(|(_, v)| *v).unwrap_or("");
            let r = |s: u16, b: Value| Ok(Response { status: s, retry_after: None, body: b.to_string() });
            if u == PROFILE_URL { return r(self.profile_status, json!({})); }
            assert!(auth.starts_with("XBL3.0 x=UHS"), "{auth}");
            if u.contains("titlehistory") { return if self.history_status == 200 { r(200, json!({"titles": [title("1", "Forza", Some("Fz.PFN")), title("2", "Steam Thing", None), title("3", "Tried Once", Some("T.PFN")), title("1", "Forza dup", Some("Fz.PFN"))]})) } else { r(self.history_status, json!({})) }; }
            panic!("unexpected GET {u}");
        }
        fn post(&self, u: &str, h: &[(&str, &str)], body: &str, _t: Duration) -> Result<Response, NetError> {
            self.seen.lock().unwrap().push((u.to_string(), body.to_string()));
            let r = |s: u16, b: Value| Ok(Response { status: s, retry_after: None, body: b.to_string() });
            if u == TOKEN_URL {
                if body.contains("grant_type=refresh_token") { assert!(body.contains("refresh_token=RT0")); return if self.refresh_status == 200 { r(200, json!({"access_token": "LIVE2", "refresh_token": "RT2"})) } else { r(self.refresh_status, json!({})) }; }
                assert!(body.contains("code=THECODE") && body.contains("grant_type=authorization_code"));
                return if self.live_status == 200 { r(200, json!({"access_token": "LIVE1", "refresh_token": "RT1"})) } else { r(self.live_status, json!({})) };
            }
            if u == USER_AUTH_URL { assert!(body.contains("d=LIVE")); return r(200, json!({"Token": "USERTOK"})); }
            if u == XSTS_URL { assert!(body.contains("USERTOK")); return r(200, json!({"Token": "XSTS", "NotAfter": "2099-01-01T00:00:00.0000000Z", "DisplayClaims": {"xui": [{"uhs": "UHS", "xid": "XUID1", "gtg": "Racer"}]}})); }
            assert!(h.iter().any(|(k, v)| *k == "Authorization" && v.starts_with("XBL3.0 x=UHS;")));
            if u == STATS_URL { return r(200, json!({"statlistscollection": [{"stats": [{"titleid": "1", "value": "120"}, {"titleid": "3", "value": 5}]}]})); }
            if u.contains("/titles/batch/") { return r(200, json!({"titles": [title("1", "Forza", Some("Fz.PFN"))]})); }
            panic!("unexpected POST {u}");
        }
    }
    const FAR: i64 = 4_000_000_000;
    fn setup(tag: &str, site: Site, section: Option<Value>) -> (XboxSource, Arc<Site>, PathBuf) {
        let d = std::env::temp_dir().join(format!("zest_xbox_{tag}_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        if let Some(s) = section { std::fs::write(d.join("config.json"), json!({"xbox": s, "xbox_settings": {"min_playtime": 0}, "other": 1}).to_string()).unwrap(); }
        let db = d.join("games.db");
        rusqlite::Connection::open(&db).unwrap().execute_batch("CREATE TABLE games (appid INTEGER PRIMARY KEY, platform TEXT, platform_id TEXT); INSERT INTO games VALUES (-9, 'xbox', '3');").unwrap();
        let site = Arc::new(site);
        (XboxSource::new(site.clone(), d.clone(), db), site, d)
    }
    fn signed_in(expires: i64) -> Value { json!({"refresh_token": "RT0", "xsts_token": "XSTS0", "uhs": "UHS", "xuid": "XUID1", "gamertag": "Racer", "xsts_expires_at": expires}) }
    fn cx(d: &PathBuf, site: Arc<Site>) -> SourceCtx {
        SourceCtx { http: site, steam: Arc::new(crate::steamclient::Steam::new(Arc::new(crate::net::UreqHttp::new()), None, 10.0)), root: d.clone(), db: d.join("games.db"), today: "2026-11-01".into(), appinfo: Arc::new(|_| Default::default()), account_id: None }
    }
    fn g() -> GameRef { GameRef { appid: -5, name: "Forza".into(), platform: "xbox".into(), platform_id: "1".into(), slug: "STORE1".into(), appname: "Fz.PFN".into(), ..Default::default() } }

    #[test] fn sign_in_walks_the_three_token_steps_and_keeps_the_session_with_a_numeric_expiry() {
        let (s, _, d) = setup("auth", fake(), None);
        assert!(s.auth_complete("https://login.live.com/oauth20_desktop.srf?lc=1033").is_err());
        let st = s.auth_complete("https://login.live.com/oauth20_desktop.srf?code=THECODE&lc=1033").unwrap();
        assert_eq!(st, AuthStatus { connected: true, username: Some("Racer".into()) });
        let cfg: Value = serde_json::from_str(&std::fs::read_to_string(d.join("config.json")).unwrap()).unwrap();
        assert_eq!(cfg["xbox"]["refresh_token"], "RT1"); assert_eq!(cfg["xbox"]["xuid"], "XUID1"); assert!(cfg["xbox"]["xsts_expires_at"].as_i64().unwrap() > 4_000_000_000);
        s.auth_disconnect().unwrap(); assert!(!s.auth_status().unwrap().connected);
        let (s, _, _) = setup("bad", Site { live_status: 400, ..fake() }, None);
        assert_eq!(s.auth_complete("THECODE").unwrap_err(), "Xbox Live auth failed (HTTP 400)");
    }
    #[test] fn sync_keeps_pc_titles_once_with_playtime_store_link_and_metadata_and_applies_the_minimum_to_new_games_only() {
        let (s, site, d) = setup("sync", fake(), Some(signed_in(FAR)));
        let mut got = vec![];
        assert_eq!(s.sync(&cx(&d, site.clone()), &mut |b| got.extend(b), &Cancel::new()).unwrap(), 2);
        let f = &got[0];
        assert_eq!((f.platform_id.as_str(), f.name.as_str(), f.slug.as_deref(), f.appname.as_deref(), f.playtime_forever), ("1", "Forza", Some("STORE1"), Some("Fz.PFN"), Some(120)));
        assert_eq!((f.last_played, f.release_date, f.developers.as_deref(), f.genres.as_deref()), (Some(1622548800), Some(1551693600), Some("Dev"), Some("Action,Racing")));
        assert_eq!(s.store_url(&g()).as_deref(), Some("https://www.xbox.com/en-US/games/store/game/STORE1"));
        // a minimum playtime keeps out a new game under it ("Tried Once" has 5 minutes) but never a game already imported (id 3 is known here)
        std::fs::write(d.join("config.json"), json!({"xbox": signed_in(FAR), "xbox_settings": {"min_playtime": 60}}).to_string()).unwrap();
        let mut got = vec![]; s.sync(&cx(&d, site.clone()), &mut |b| got.extend(b), &Cancel::new()).unwrap();
        assert_eq!(got.iter().map(|r| r.platform_id.as_str()).collect::<Vec<_>>(), ["1", "3"]);
        assert!(s.enrich_new());
    }
    #[test] fn an_expiring_session_is_refreshed_first_and_a_refused_one_or_an_http_error_ends_the_sync() {
        let (s, site, d) = setup("refresh", fake(), Some(signed_in(1)));
        assert_eq!(s.sync(&cx(&d, site.clone()), &mut |_| {}, &Cancel::new()).unwrap(), 2);
        let cfg: Value = serde_json::from_str(&std::fs::read_to_string(d.join("config.json")).unwrap()).unwrap();
        assert_eq!((cfg["xbox"]["refresh_token"].as_str(), cfg["xbox"]["xsts_token"].as_str()), (Some("RT2"), Some("XSTS")));   // the rotated refresh token was kept
        assert_eq!(cfg["other"], 1);
        let (s, site, d) = setup("401", Site { history_status: 401, ..fake() }, Some(signed_in(FAR)));
        assert_eq!(s.sync(&cx(&d, site), &mut |_| {}, &Cancel::new()).unwrap_err(), EXPIRED);
        let (s, site, d) = setup("500", Site { history_status: 500, ..fake() }, Some(signed_in(FAR)));
        assert!(s.sync(&cx(&d, site), &mut |_| {}, &Cancel::new()).unwrap_err().contains("HTTP 500"));
        let (s, site, d) = setup("nologin", fake(), None);
        assert_eq!(s.sync(&cx(&d, site), &mut |_| {}, &Cancel::new()).unwrap_err(), "Not connected");
    }
    #[test] fn the_dialog_signs_out_a_refused_login_and_keeps_one_it_cannot_check() {
        let (s, _, _) = setup("st_ok", fake(), Some(signed_in(FAR)));
        assert_eq!(s.auth_status().unwrap(), AuthStatus { connected: true, username: Some("Racer".into()) });
        let (s, _, _) = setup("st_dead", Site { profile_status: 401, refresh_status: 400, ..fake() }, Some(signed_in(FAR)));
        assert!(!s.auth_status().unwrap().connected);
        let (s, _, _) = setup("st_403", Site { profile_status: 403, ..fake() }, Some(signed_in(FAR)));
        assert!(!s.auth_status().unwrap().connected);   // refused, refreshed, refused again
        let (s, _, _) = setup("st_503", Site { profile_status: 503, ..fake() }, Some(signed_in(FAR)));
        assert!(s.auth_status().unwrap().connected);    // Xbox having a bad day is not a sign-out
    }
    #[test] fn rescrape_description_and_art_use_the_single_title_lookup() {
        let (s, site, d) = setup("one", fake(), Some(signed_in(FAR)));
        let u = s.rescrape(&cx(&d, site.clone()), &g()).unwrap().unwrap();
        let get = |k: &str| u.iter().find(|(c, _)| *c == k).map(|(_, v)| v.clone());
        assert_eq!((get("unlocked_achievements"), get("total_achievements"), get("cheevos_fetched")), (Some(Sql::Integer(4)), Some(Sql::Integer(10)), Some(Sql::Text("2026-11-01".into()))));
        assert_eq!((get("playtime_forever"), get("meta_fetched")), (Some(Sql::Integer(120)), Some(Sql::Text("2026-11-01".into()))));
        assert_eq!(s.description(&cx(&d, site), &g()).as_deref(), Some("Vroom"));
        assert_eq!(s.art_urls("Fz.PFN"), vec![("vertical", "https://img/p.jpg".to_string()), ("icon", "https://img/l.png".to_string())]);
        assert_eq!(LibrarySource::art_urls(&s, &g()), vec![("vertical".to_string(), "https://img/p.jpg".to_string()), ("icon".to_string(), "https://img/l.png".to_string())]);
        assert!(s.rescrape(&cx(&d, setup("none", fake(), None).1), &GameRef::default()).unwrap().is_none());
    }
}

#[cfg(all(test, target_os = "linux"))]
mod nexus_tests {
    use super::*;
    fn tmp(tag: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("zest_xbox_{tag}_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d }
    fn game(root: &std::path::Path, name: &str, store: &str, ready: bool) {
        let g = root.join(name); std::fs::create_dir_all(g.join("bin")).unwrap();
        std::fs::write(g.join("MicrosoftGame.config"), format!("<Game><StoreId>{store}</StoreId><ExecutableList><Executable Name=\"bin\\Game.exe\"/></ExecutableList></Game>")).unwrap();
        if ready { std::fs::write(g.join("bin/Game.exe"), "x").unwrap(); std::fs::write(g.join(".xodus-streaming.msixvc"), "y").unwrap(); }
    }
    fn source(d: &PathBuf) -> XboxSource {
        let h: Arc<dyn Http> = Arc::new(crate::net::UreqHttp::new());
        let mut s = XboxSource::new(h, d.clone(), d.join("games.db")); s.home = d.join("home"); s
    }
    fn launcher(d: &PathBuf) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = d.join("home/.local/bin/xodus-nexus"); std::fs::create_dir_all(p.parent().unwrap()).unwrap(); std::fs::write(&p, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap(); p
    }
    fn gr(slug: &str) -> GameRef { GameRef { appid: -5, name: "Forza".into(), platform: "xbox".into(), platform_id: "1".into(), slug: slug.into(), appname: "Fz.PFN".into(), ..Default::default() } }

    #[test] fn launch_asks_nexus_for_a_ready_game_and_opens_nexus_for_a_missing_or_unfinished_one() {
        let d = tmp("launch"); let s = source(&d);
        assert!(s.launch(&gr("")).unwrap_err().contains("Store ID"));
        assert!(s.launch(&gr("AAAAAAAAAAAA")).unwrap_err().contains("Xodus Nexus was not found"));
        let exe = launcher(&d);
        let root = d.join("home/Games/Xodus Nexus"); game(&root, "Ready", "AAAAAAAAAAAA", true); game(&root, "Half", "BBBBBBBBBBBB", false);
        let l = s.launch(&gr("aaaaaaaaaaaa")).unwrap(); assert!(l.record);
        let id = super::super::xbox_nexus::launch_id(&root.join("Ready"));
        assert_eq!(l.action, Action::Spawn { program: exe.to_string_lossy().to_string(), args: vec!["--launch-game".into(), id], dir: None, env: vec![] });
        for (slug, word) in [("BBBBBBBBBBBB", "incomplete"), ("CCCCCCCCCCCC", "Install this game there")] {
            let l = s.launch(&gr(slug)).unwrap(); assert!(!l.record, "a hand-off is not a play");
            match l.action { Action::Many(v) => { assert!(matches!(&v[0], Action::Spawn { args, .. } if args.is_empty())); assert!(matches!(&v[1], Action::Message(m) if m.contains(word))); } a => panic!("{a:?}") }
        }
        assert!(s.uninstall(&gr("CCCCCCCCCCCC")).unwrap_err().contains("no folder"));
        let ready = std::fs::canonicalize(root.join("Ready")).unwrap();
        assert_eq!(s.uninstall(&gr("AAAAAAAAAAAA")).unwrap(), Action::RemoveInstall { dir: ready.to_string_lossy().to_string(), delete: true });
        assert!(!ready.exists(), "Nexus closed: Zest removed the game itself");
    }
    #[test] fn the_scan_marks_ready_nexus_games_installed_clears_the_rest_and_leaves_emulated_xbox_roms_alone() {
        let d = tmp("scan"); let s = source(&d);
        let root = d.join("home/Games/Xodus Nexus"); game(&root, "Ready", "AAAAAAAAAAAA", true); game(&root, "Half", "BBBBBBBBBBBB", false);
        rusqlite::Connection::open(d.join("games.db")).unwrap().execute_batch("CREATE TABLE games (platform TEXT, platform_id TEXT, platform_slug TEXT, platform_appname TEXT);
            INSERT INTO games VALUES ('xbox','1','AAAAAAAAAAAA','P1'),('xbox','2','BBBBBBBBBBBB','P2'),('xbox','3','CCCCCCCCCCCC','P3'),('xbox','rom',NULL,NULL),('gog','g','AAAAAAAAAAAA',NULL);").unwrap();
        let rows = s.installed_scan().unwrap();
        assert_eq!(rows.iter().map(|r| (r.platform_id.as_str(), r.installed, r.install_path.is_some())).collect::<Vec<_>>(), vec![("1", true, true), ("2", false, false), ("3", false, false)]);
        assert_eq!(rows[0].install_path.as_deref(), Some(&*std::fs::canonicalize(root.join("Ready")).unwrap().to_string_lossy()));
    }
    #[test] fn appx_output_is_read_for_one_package_or_many_and_names_are_checked_before_they_go_into_a_script() {
        let m = parse_appx(r#"[{"PackageFamilyName":"Microsoft.SeaofThieves_8wekyb3d8bbwe","InstallLocation":"C:\\Games\\SoT"},{"PackageFamilyName":"X.Y","InstallLocation":null}]"#);
        assert_eq!(m.get("microsoft.seaofthieves_8wekyb3d8bbwe").map(String::as_str), Some("C:\\Games\\SoT")); assert_eq!(m.get("x.y").map(String::as_str), Some(""));
        assert_eq!(parse_appx(r#"{"PackageFamilyName":"One.Pkg","InstallLocation":"D:\\p"}"#).len(), 1);
        assert!(parse_appx("").is_empty());
        assert!(safe_token("Microsoft.SeaofThieves_8wekyb3d8bbwe") && !safe_token("x'; Remove-Item *") && !safe_token(""));
    }
}
