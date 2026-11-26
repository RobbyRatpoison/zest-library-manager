//! Epic Games as a library source (port of PlayDate's `epic_games` plugin: `epic.py`, listing half). Sign-in is the pasted `authorizationCode` (Epic's own public launcher
//! credentials, the same ones Legendary uses); the library is the launcher's assets list, each item enriched from the catalog and the store's GraphQL API. Tokens live in
//! `config.json["epic_games"]` exactly as PlayDate keeps them, so an existing login carries over. Tokens and codes are never logged. Not ported: Wine/launcher setup (a
//! native Linux launcher exists now), install watching, uninstall, the one-time listing repair of PlayDate's own database, art download (core's job).
use super::{Action, AuthBegin, AuthStatus, Caps, GameRef, InstalledRow, Launch, LibrarySource, SourceCtx};
use crate::importer::GameRecord;
use crate::jobs::Update;
use crate::net::{enc, log, Cancel, Http};
use crate::steamclient::{Fetch, RateLimited};
use regex::Regex;
use rusqlite::types::Value as Sql;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const BASIC: &str = "Basic MzRhMDJjZjhmNDQxNGUyOWIxNTkyMTg3NmRhMzZmOWE6ZGFhZmJjY2M3Mzc3NDUwMzlkZmZlNTNkOTRmYzc2Y2Y=";
const TOKEN_URL: &str = "https://account-public-service-prod.ol.epicgames.com/account/api/oauth/token";
const AUTH_URL: &str = "https://www.epicgames.com/id/login?redirectUrl=https%3A%2F%2Fwww.epicgames.com%2Fid%2Fapi%2Fredirect%3FclientId%3D34a02cf8f4414e29b15921876da36f9a%26responseType%3Dcode";
const ASSETS_URL: &str = "https://launcher-public-service-prod06.ol.epicgames.com/launcher/api/public/assets/Windows?label=Live";
const CATALOG_URL: &str = "https://catalog-public-service-prod06.ol.epicgames.com/catalog/api/shared/namespace";
const ENTITLEMENTS_URL: &str = "https://entitlement-public-service-prod08.ol.epicgames.com/entitlement/api/account";
const GRAPHQL: &str = "https://store.epicgames.com/graphql";
const GQL_HEADERS: &[(&str, &str)] = &[
    ("User-Agent", "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36"),
    ("Content-Type", "application/json"), ("Referer", "https://store.epicgames.com/"), ("Origin", "https://store.epicgames.com"),
];
const EXPIRED: &str = "Epic Games session expired, please connect again.";

const RATINGS_QUERY: &str = "query getProductResult($sandboxId: String!, $locale: String!) { RatingsPolls { getProductResult(sandboxId: $sandboxId, locale: $locale) { averageRating pollResult { id total } } } }";
const ELEMENT: &str = "title namespace offerType productSlug urlSlug description tags { id } pcReleaseDate releaseDate offerMappings { pageSlug pageType } catalogNs { mappings(pageType: \"productHome\") { pageSlug pageType } }";
const TAGS_QUERY: &str = "query catalogTags($namespace: String!) { Catalog { tags(namespace: $namespace, start: 0, count: 999) { elements { id name } } } }";

fn sv<'a>(v: &'a Value, k: &str) -> Option<&'a str> { v.get(k).and_then(|x| x.as_str()) }
fn search_query() -> String { format!("query searchStoreQuery($namespace: String, $country: String!, $locale: String) {{ Catalog {{ searchStore(namespace: $namespace, country: $country, locale: $locale) {{ elements {{ {ELEMENT} }} }} }} }}") }
fn keyword_query() -> String { format!("query keywordStoreQuery($keywords: String!, $country: String!, $locale: String) {{ Catalog {{ searchStore(keywords: $keywords, country: $country, locale: $locale, count: 20) {{ elements {{ {ELEMENT} }} }} }} }}") }

// ---------------------------------------------------------------- pure helpers (identical to epic.py, see tests/epic_golden.rs)

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes(); let mut out = vec![]; let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => { out.push(b' '); i += 1; }
            b'%' if i + 2 < b.len() && b[i + 1].is_ascii_hexdigit() && b[i + 2].is_ascii_hexdigit() => { out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap_or(b'?')); i += 3; }
            c => { out.push(c); i += 1; }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `_extract_auth_code`: the bare code, the JSON body Epic shows, or an address with `?code=`. Empty when nothing is found.
pub fn extract_auth_code(raw: &str) -> String {
    let raw = raw.trim();
    if raw.starts_with('{') {
        if let Ok(Value::Object(o)) = serde_json::from_str::<Value>(raw) {
            let pick = |k: &str| o.get(k).and_then(|v| v.as_str()).filter(|s| !s.is_empty());
            return pick("authorizationCode").or_else(|| pick("code")).unwrap_or("").trim().to_string();
        }
    }
    if raw.contains("code=") {
        let no_frag = raw.split('#').next().unwrap_or("");
        let query = no_frag.split_once('?').map(|(_, q)| q).unwrap_or("");
        for pair in query.split('&') {
            if let Some((k, v)) = pair.split_once('=') { if percent_decode(k) == "code" { return percent_decode(v).trim().to_string(); } }
        }
        return String::new();
    }
    raw.to_string()
}

/// `_clean_epic_slug`: only the bare product slug (Epic sometimes appends "/home").
pub fn clean_slug(slug: Option<&str>) -> String { let s = slug.unwrap_or("").trim().trim_matches('/'); s.split('/').next().unwrap_or("").to_string() }
fn hex_id(s: &str) -> bool { s.len() == 32 && s.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)) }

/// `_store_page_slug`: the real store page slug from a searchStore element ('' when there is none, or only an internal id).
pub fn store_page_slug(el: &Value) -> String {
    let mut maps: Vec<&Value> = el.pointer("/catalogNs/mappings").and_then(|m| m.as_array()).map(|a| a.iter().collect()).unwrap_or_default();
    maps.extend(el.get("offerMappings").and_then(|m| m.as_array()).into_iter().flatten());
    for m in maps { if sv(m, "pageType") == Some("productHome") { let s = clean_slug(sv(m, "pageSlug")); if !s.is_empty() { return s; } } }
    let pick = ["productSlug", "urlSlug"].iter().filter_map(|k| sv(el, k)).find(|s| !s.is_empty());
    let s = clean_slug(pick);
    if hex_id(&s) { String::new() } else { s }
}

fn truthy(v: Option<&Value>) -> bool { match v { None | Some(Value::Null) => false, Some(Value::Bool(b)) => *b, Some(Value::String(s)) => !s.is_empty(), Some(Value::Number(n)) => n.as_f64() != Some(0.0), Some(Value::Array(a)) => !a.is_empty(), Some(Value::Object(o)) => !o.is_empty() } }

const NON_GAME: &[&str] = &["addons", "digitalextras", "software"];
fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex { cell.get_or_init(|| Regex::new(pat).unwrap()) }

/// `_is_dlc_or_extra`: DLC, a digital extra, a non-game application or a dev kit rather than a standalone game.
pub fn is_dlc_or_extra(entry: &Value, name: &str) -> bool {
    static BONUS: OnceLock<Regex> = OnceLock::new(); static KIT: OnceLock<Regex> = OnceLock::new();
    if truthy(entry.get("mainGameItem")) { return true; }
    let cats: HashSet<&str> = entry.get("categories").and_then(|c| c.as_array()).into_iter().flatten().filter_map(|c| sv(c, "path")).collect();
    if cats.iter().any(|c| NON_GAME.contains(c)) { return true; }
    if cats.contains("engines") && re(&KIT, r"(?i)\b(dev\s?kit|mod\s?kit|sdk|editor)\b").is_match(name) { return true; }
    re(&BONUS, r"(?i)\b(soundtrack|art\s*book|ost)\b").is_match(name)
}

/// The reason shown by the junk scan for an entry `is_dlc_or_extra` flags.
pub fn junk_reason(entry: &Value) -> &'static str {
    let cats: HashSet<&str> = entry.get("categories").and_then(|c| c.as_array()).into_iter().flatten().filter_map(|c| sv(c, "path")).collect();
    if truthy(entry.get("mainGameItem")) || cats.contains("addons") { "DLC / add-on" } else if cats.contains("digitalextras") { "digital extra (soundtrack / artbook)" }
    else if cats.contains("software") { "application, not a game" } else if cats.contains("engines") { "dev kit" } else { "not a standalone game" }
}

pub fn title_key(t: &str) -> String { t.to_lowercase().chars().filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit()).collect() }
/// `_title_matches`: one title contains the other ("Halcyon 6" and "Halcyon 6 Starbase Commander").
pub fn title_matches(name: &str, title: &str) -> bool { let (a, b) = (title_key(name), title_key(title)); !a.is_empty() && !b.is_empty() && (b.contains(&a) || a.contains(&b)) }

/// `_pick_store_element`: the searchStore element for the game itself (the base game's offer, titled like it when a name is given).
pub fn pick_store_element<'a>(elements: &'a [Value], namespace: &str, name: Option<&str>) -> Option<&'a Value> {
    let base: Vec<&Value> = elements.iter().filter(|e| sv(e, "offerType") == Some("BASE_GAME") && e.get("namespace").map_or(true, |n| n.is_null() || n.as_str() == Some(namespace))).collect();
    match name {
        None => base.first().copied().or_else(|| elements.first()),
        Some(n) => base.into_iter().find(|e| title_matches(n, sv(e, "title").unwrap_or(""))),
    }
}

/// `_parse_pc_release` with the clock given: Epic's date as a Unix time; a date more than a year ahead (a 2099 placeholder) is ignored.
pub fn parse_pc_release_at(value: Option<&str>, now: i64) -> Option<i64> {
    let ts = super::playstation::parse_time(value.filter(|v| !v.is_empty())?)?;
    (ts <= now + 365 * 86400).then_some(ts)
}
fn now_ts() -> i64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0) }

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CatalogMeta { pub developers: Option<String>, pub publishers: Option<String>, pub genres: Option<String>, pub slug: Option<String> }

/// `_extract_metadata`: developer, publisher, genres and slug from a catalog entry (no release date on purpose: the catalog's dates are listing dates).
pub fn extract_metadata(entry: &Value) -> CatalogMeta {
    let custom = |k: &str| -> String { entry.pointer(&format!("/customAttributes/{k}/value")).and_then(|v| v.as_str()).unwrap_or("").to_string() };
    let top = |k: &str| sv(entry, k).unwrap_or("").to_string();
    let first = |xs: [String; 3]| xs.into_iter().find(|s| !s.is_empty()).unwrap_or_default();
    let devs = first([custom("developerName"), custom("developerDisplayName"), top("developer")]).trim().to_string();
    let publisher = first([custom("publisherName"), custom("publisherDisplayName"), top("publisher")]); let publisher = if publisher.trim().is_empty() { devs.clone() } else { publisher.trim().to_string() };
    let genres = custom("genres").trim().to_string();
    let slug = clean_slug(sv(entry, "urlSlug"));
    let some = |s: String| Some(s).filter(|s| !s.is_empty());
    CatalogMeta { developers: some(devs), publishers: some(publisher), genres: some(genres), slug: some(slug).filter(|s| !hex_id(s)) }
}

/// What `_fetch_epic_store_data` finds on the store page.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StoreData { pub tags: Option<String>, pub slug: Option<String>, pub description: Option<String>, pub release: Option<i64> }

/// `_apply_store_data` + `_extract_metadata` as column updates: store tags and slug win; Epic's release date only when the game has none or Epic's is earlier.
pub fn merge_store(meta: &CatalogMeta, store: &StoreData, current_release: Option<i64>) -> Vec<(&'static str, Sql)> {
    let mut out = vec![];
    let t = |s: &Option<String>| s.clone().map(Sql::Text);
    for (k, v) in [("developers", &meta.developers), ("publishers", &meta.publishers), ("genres", &meta.genres)] { if let Some(v) = t(v) { out.push((k, v)); } }
    if let Some(v) = t(&store.tags) { out.push(("tags", v)); }
    if let Some(v) = t(&store.slug).or_else(|| t(&meta.slug)) { out.push(("platform_slug", v)); }
    if let Some(ts) = store.release { if current_release.map_or(true, |c| c == 0 || ts < c) { out.push(("release_date", Sql::Integer(ts))); } }
    out
}

/// `utils.review_score_label`.
pub fn review_score_label(percent: i64, total: i64) -> &'static str {
    if total == 0 { return "No Reviews"; } if total < 10 { return "Not Enough Reviews"; }
    if percent >= 95 && total >= 500 { "Overwhelmingly Positive" } else if percent >= 80 && total >= 50 { "Very Positive" } else if percent >= 80 { "Positive" }
    else if percent >= 70 { "Mostly Positive" } else if percent >= 40 { "Mixed" } else if percent >= 20 { "Mostly Negative" }
    else if total >= 500 { "Overwhelmingly Negative" } else if total >= 50 { "Very Negative" } else { "Negative" }
}

/// `_fetch_epic_ratings`' arithmetic: the store's average (out of 5) and poll totals as the review columns. None when there is no average.
pub fn ratings_columns(result: &Value) -> Option<Vec<(&'static str, Sql)>> {
    let avg = result.get("averageRating").and_then(|a| a.as_f64())?;
    let pct = (avg / 5.0 * 100.0).round_ties_even() as i64;
    let total: i64 = result.get("pollResult").and_then(|p| p.as_array()).into_iter().flatten().map(|p| p.get("total").and_then(|t| t.as_i64()).unwrap_or(0)).sum();
    let p = avg / 5.0;
    let weighted = ((p - (p - 0.5) * 2f64.powf(-((total + 1) as f64).log10())) * 100.0).round_ties_even() as i64;
    Some(vec![("review_percentage", Sql::Integer(pct)), ("review_score", Sql::Text(review_score_label(pct, total).into())), ("total_reviews", Sql::Integer(total)), ("weighted_percentage", Sql::Integer(weighted))])
}

/// `_fetch_entitlement_date_maps`: the earliest grant time by namespace (never the platform-wide `epic` one) and by catalog item id.
pub fn entitlement_date_maps(ents: &[Value]) -> (HashMap<String, i64>, HashMap<String, i64>) {
    let (mut by_ns, mut by_cid): (HashMap<String, i64>, HashMap<String, i64>) = Default::default();
    for e in ents {
        let raw = ["grantDate", "createdDate", "created"].iter().filter_map(|k| sv(e, k)).find(|s| !s.is_empty());
        let Some(ts) = raw.and_then(super::playstation::parse_time) else { continue };
        let (ns, cid) = (sv(e, "namespace").unwrap_or(""), sv(e, "catalogItemId").unwrap_or(""));
        if !ns.is_empty() && ns != "epic" { let x = by_ns.entry(ns.to_string()).or_insert(ts); if ts < *x { *x = ts; } }
        if !cid.is_empty() { let x = by_cid.entry(cid.to_string()).or_insert(ts); if ts < *x { *x = ts; } }
    }
    (by_ns, by_cid)
}

/// What the sync does with the assets list: catalog items to fetch per namespace (in the order met), and the launch name chosen for each item.
#[derive(Debug, Default, PartialEq)]
pub struct Plan { pub by_ns: Vec<(String, Vec<(String, String)>)>, pub best: HashMap<String, String> }

/// The planning part of `_do_sync_library`. Epic lists a game several times (entitlement and installable); the first record with a build version is the one to launch.
pub fn plan_sync(assets: &[Value], existing: &HashSet<String>, blacklisted: &HashSet<String>) -> Plan {
    let mut p = Plan::default(); let mut names: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for a in assets {
        if sv(a, "namespace") == Some("ue") { continue; }
        let ns = sv(a, "namespace").unwrap_or("fn").to_string();
        let (cid, aname) = (sv(a, "catalogItemId").unwrap_or(""), sv(a, "appName").unwrap_or(""));
        if cid.is_empty() || aname.is_empty() || existing.contains(cid) || blacklisted.contains(cid) { continue; }
        names.entry(cid.to_string()).or_default().push((aname.to_string(), sv(a, "buildVersion").unwrap_or("").to_string()));
        match p.by_ns.iter_mut().find(|(n, _)| *n == ns) { Some((_, v)) => v.push((cid.to_string(), aname.to_string())), None => p.by_ns.push((ns, vec![(cid.to_string(), aname.to_string())])) }
    }
    for (cid, entries) in names { let best = entries.iter().find(|(_, b)| !b.is_empty()).unwrap_or(&entries[0]).0.clone(); p.best.insert(cid, best); }
    p
}

/// `_download_epic_art`'s choice: the catalog's own tall and wide key images.
pub fn art_urls(entry: &Value) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = vec![];
    for img in entry.get("keyImages").and_then(|k| k.as_array()).into_iter().flatten() {
        let (t, u) = (sv(img, "type").unwrap_or(""), sv(img, "url").unwrap_or(""));
        for (kind, want) in [("vertical", "DieselGameBoxTall"), ("horizontal", "DieselGameBox")] { if t == want && !u.is_empty() && !out.iter().any(|(k, _)| *k == kind) { out.push((kind, u.to_string())); } }
    }
    out
}

// ---------------------------------------------------------------- the source

enum EpicErr { Auth(String), Other(String) }
impl EpicErr { fn text(self) -> String { match self { EpicErr::Auth(m) | EpicErr::Other(m) => m } } }

/// The folder of `.item` manifests Epic's launcher keeps for installed games: ProgramData on Windows, Application Support on macOS, inside the Wine prefix on Linux.
pub fn manifests_dir(prefix: Option<&std::path::Path>) -> Option<PathBuf> {
    let d = if cfg!(windows) { PathBuf::from(std::env::var_os("PROGRAMDATA").unwrap_or_else(|| "C:\\ProgramData".into())).join("Epic/EpicGamesLauncher/Data/Manifests") }
        else if cfg!(target_os = "macos") { crate::installdir::home().join("Library/Application Support/Epic/EpicGamesLauncher/Data/Manifests") }
        else { prefix?.join("drive_c/ProgramData/Epic/EpicGamesLauncher/Data/Manifests") };
    d.is_dir().then_some(d)
}
/// `{AppName: InstallLocation}` from every `.item` manifest in the folder.
pub fn read_manifests(dir: &std::path::Path) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if !e.file_name().to_string_lossy().ends_with(".item") { continue; }
        let Some(v) = std::fs::read_to_string(e.path()).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) else { continue };
        let name = v["AppName"].as_str().unwrap_or("");
        if !name.is_empty() { out.insert(name.to_string(), v["InstallLocation"].as_str().unwrap_or("").to_string()); }
    }
    out
}
/// A manifest's install folder as a path on this machine: inside a Wine prefix the Windows path is translated, elsewhere it is already real.
fn host_install_path(prefix: Option<&std::path::Path>, loc: &str) -> String {
    match prefix { Some(p) if cfg!(target_os = "linux") => super::legacy_games::win_path_to_host(p, loc), _ => loc.to_string() }
}

pub struct EpicSource { http: Arc<dyn Http>, config_dir: PathBuf, db: PathBuf, token: Mutex<Option<(String, Instant)>>, tags: Mutex<HashMap<String, String>> }

impl EpicSource {
    pub fn new(http: Arc<dyn Http>, config_dir: PathBuf, db: PathBuf) -> Self { EpicSource { http, config_dir, db, token: Mutex::new(None), tags: Mutex::new(HashMap::new()) } }
    /// Hands a launcher link to the system: Windows and macOS open it; on Linux the launcher runs in its Wine prefix, which takes the link through `wine start`.
    fn send(&self, url: String) -> Result<Action, String> {
        if !cfg!(target_os = "linux") { return Ok(Action::OpenUrl(url)); }
        let prefix = super::rockstar::prefix_setting(&self.config_dir, "epic_games").ok_or("Epic launcher not set up. Install it from the Plugins dialog (INSTALL LAUNCHER) or give its Wine prefix.")?;
        // a live launcher takes the link itself; winegstreamer is switched off as PlayDate does (it crashes the launcher's web view)
        crate::wine::protocol_action(&url, &prefix, false, &[("WINEDLLOVERRIDES", "winegstreamer=")], &|_| None)
    }
    fn secret(&self, k: &str) -> Option<String> { crate::account::plugin_secret_get(&self.config_dir, "epic_games", k).filter(|v| !v.is_empty()) }
    fn set(&self, k: &str, v: Option<&str>) { let _ = crate::account::plugin_secret_set(&self.config_dir, "epic_games", k, v); }
    fn connected(&self) -> bool { self.secret("access_token").is_some() && self.secret("refresh_token").is_some() }

    fn token_post(&self, body: &str) -> Result<Value, EpicErr> {
        let r = self.http.post(TOKEN_URL, &[("Authorization", BASIC), ("Content-Type", "application/x-www-form-urlencoded"), ("Accept", "application/json")], body, Duration::from_secs(15))
            .map_err(|e| { log(&format!("Epic: {e}")); EpicErr::Other("Could not reach Epic.".into()) })?;
        let j: Value = serde_json::from_str(&r.body).unwrap_or(Value::Null);
        if r.status != 200 || sv(&j, "access_token").map_or(true, str::is_empty) {
            let why = sv(&j, "errorMessage").map(|m| format!(": {}", m.chars().take(200).collect::<String>())).unwrap_or_default();   // Epic's own message, never the request
            return Err(EpicErr::Auth(format!("Epic did not accept the login (HTTP {}){why}", r.status)));
        }
        Ok(j)
    }
    /// A valid access token. Epic's refresh tokens rotate, so the new one is saved at once; the access token is cached in memory for its lifetime.
    fn access_token(&self) -> Result<String, EpicErr> {
        if let Some((t, until)) = self.token.lock().unwrap().as_ref() { if Instant::now() < *until { return Ok(t.clone()); } }
        let refresh = self.secret("refresh_token").filter(|_| self.secret("access_token").is_some()).ok_or_else(|| EpicErr::Auth("Epic Games account not connected.".into()))?;
        let j = self.token_post(&format!("grant_type=refresh_token&refresh_token={}", enc(&refresh))).map_err(|e| match e { EpicErr::Auth(_) => EpicErr::Auth(EXPIRED.into()), o => o })?;
        let at = sv(&j, "access_token").unwrap_or("").to_string();
        self.set("access_token", Some(&at));
        if let Some(rt) = sv(&j, "refresh_token").filter(|r| !r.is_empty()) { self.set("refresh_token", Some(rt)); }
        let secs = j.get("expires_in").and_then(|e| e.as_i64().or_else(|| e.as_str()?.parse().ok())).unwrap_or(7200);
        *self.token.lock().unwrap() = Some((at.clone(), Instant::now() + Duration::from_secs((secs - 60).max(0) as u64)));
        Ok(at)
    }
    fn api_get(&self, url: &str, timeout: u64) -> Result<crate::net::Response, EpicErr> {
        let at = self.access_token()?;
        let r = self.http.get(url, &[("Authorization", &format!("Bearer {at}"))], Duration::from_secs(timeout)).map_err(|e| { log(&format!("Epic: {e}")); EpicErr::Other("Could not reach Epic.".into()) })?;
        if r.status == 401 { *self.token.lock().unwrap() = None; return Err(EpicErr::Auth(EXPIRED.into())); }
        Ok(r)
    }
    fn gql(&self, query: &str, vars: Value, timeout: u64) -> Option<Value> {
        let body = json!({"query": query, "variables": vars}).to_string();
        match self.http.post(GRAPHQL, GQL_HEADERS, &body, Duration::from_secs(timeout)) {
            Ok(r) if r.status == 200 => serde_json::from_str(&r.body).ok(),
            Ok(r) => { log(&format!("Epic store: HTTP {}", r.status)); None }
            Err(e) => { log(&format!("Epic store: not reachable ({e})")); None }
        }
    }
    fn tag_names(&self) -> HashMap<String, String> {
        { let g = self.tags.lock().unwrap(); if !g.is_empty() { return g.clone(); } }
        let mut map = HashMap::new();
        if let Some(b) = self.gql(TAGS_QUERY, json!({"namespace": "epic"}), 15) {
            for el in b.pointer("/data/Catalog/tags/elements").and_then(|e| e.as_array()).into_iter().flatten() { if let (Some(i), Some(n)) = (sv(el, "id").filter(|s| !s.is_empty()), sv(el, "name").filter(|s| !s.is_empty())) { map.insert(i.to_string(), n.to_string()); } }
        }
        *self.tags.lock().unwrap() = map.clone(); map
    }
    fn store_elements(&self, query: &str, mut vars: Value) -> Vec<Value> {
        vars["country"] = json!("US"); vars["locale"] = json!("en-US");
        self.gql(query, vars, 10).and_then(|b| b.pointer("/data/Catalog/searchStore/elements").and_then(|e| e.as_array().cloned())).unwrap_or_default()
    }
    /// `_fetch_epic_store_data`: tags, slug, description and release date from the store's offer for the game ({} when no offer fits, e.g. a delisted game).
    pub fn store_data(&self, namespace: &str, name: Option<&str>) -> StoreData {
        let els = self.store_elements(&search_query(), json!({"namespace": namespace}));
        let mut el = pick_store_element(&els, namespace, name).cloned();
        if el.is_none() { if let Some(n) = name.filter(|n| !n.is_empty()) {
            let hits: Vec<Value> = self.store_elements(&keyword_query(), json!({"keywords": n})).into_iter().filter(|h| sv(h, "namespace") == Some(namespace)).collect();
            el = pick_store_element(&hits, namespace, name).cloned();
        } }
        let Some(el) = el else { return StoreData::default() };
        let ids: Vec<&str> = el.get("tags").and_then(|t| t.as_array()).into_iter().flatten().filter_map(|t| sv(t, "id")).filter(|s| !s.is_empty()).collect();
        let tags = if ids.is_empty() { None } else { let m = self.tag_names(); Some(ids.iter().filter_map(|i| m.get(*i).cloned()).collect::<Vec<_>>()).filter(|v| !v.is_empty()).map(|v| v.join(",")) };
        let now = now_ts();
        StoreData { tags, slug: Some(store_page_slug(&el)).filter(|s| !s.is_empty()), description: sv(&el, "description").map(|d| d.trim().to_string()).filter(|d| !d.is_empty()), release: parse_pc_release_at(sv(&el, "pcReleaseDate"), now) }
    }
    fn ratings(&self, namespace: &str) -> Vec<(&'static str, Sql)> {
        self.gql(RATINGS_QUERY, json!({"sandboxId": namespace, "locale": "en-US"}), 10).and_then(|b| b.pointer("/data/RatingsPolls/getProductResult").cloned()).and_then(|r| ratings_columns(&r)).unwrap_or_default()
    }
    fn catalog_url(ns: &str, ids: &[&str], dlc: bool) -> String {
        let mut u = format!("{CATALOG_URL}/{}/bulk/items?", enc(ns));
        for i in ids { u.push_str(&format!("id={}&", enc(i))); }
        u.push_str(if dlc { "country=US&locale=en&includeDLCDetails=true&includeMainGameDetails=true" } else { "country=US&locale=en&includeDLCDetails=false" }); u
    }
    fn catalog_entry(&self, ns: &str, cid: &str) -> Result<Option<Value>, RateLimited> {
        match self.api_get(&Self::catalog_url(ns, &[cid], false), 15) {
            Ok(r) if r.status == 429 => Err(RateLimited(r.retry_after)),
            Ok(r) if r.status == 200 => Ok(serde_json::from_str::<Value>(&r.body).ok().and_then(|b| b.get(cid).cloned()).filter(|e| !e.is_null())),
            Ok(r) => { log(&format!("Epic catalog fetch: HTTP {}", r.status)); Ok(None) }
            Err(e) => { log(&format!("Epic catalog fetch: {}", e.text())); Ok(None) }
        }
    }
    fn game_row(&self, appid: i64) -> (String, Option<i64>) {
        rusqlite::Connection::open_with_flags(&self.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()
            .and_then(|c| c.query_row("SELECT COALESCE(platform_ns,''), release_date FROM games WHERE appid = ?", [appid], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?))).ok()).unwrap_or_default()
    }
    /// platform_id -> name for the Epic games already in the library.
    fn known_names(&self) -> HashMap<String, String> {
        let Ok(c) = rusqlite::Connection::open_with_flags(&self.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { return Default::default() };
        c.prepare("SELECT platform_id, name FROM games WHERE platform = 'epic_games' AND platform_id IS NOT NULL AND name IS NOT NULL").ok()
            .map(|mut s| s.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).map(|it| it.flatten().collect()).unwrap_or_default()).unwrap_or_default()
    }
    fn ids(&self) -> (HashSet<String>, HashSet<String>) {
        let Ok(c) = rusqlite::Connection::open_with_flags(&self.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { return Default::default() };
        let col = |sql: &str| -> HashSet<String> { c.prepare(sql).ok().map(|mut s| s.query_map([], |r| r.get::<_, Option<String>>(0)).map(|it| it.flatten().flatten().collect()).unwrap_or_default()).unwrap_or_default() };
        (col("SELECT platform_id FROM games WHERE platform = 'epic_games'"), col("SELECT platform_id FROM blacklist WHERE platform_id IS NOT NULL"))
    }
    fn entitlement_dates(&self) -> (HashMap<String, i64>, HashMap<String, i64>) {
        let Some(account) = self.secret("account_id") else { return Default::default() };
        match self.api_get(&format!("{ENTITLEMENTS_URL}/{}/entitlements?count=5000", enc(&account)), 30) {
            Ok(r) if r.status == 200 => serde_json::from_str::<Value>(&r.body).ok().and_then(|v| v.as_array().map(|a| entitlement_date_maps(a))).unwrap_or_default(),
            _ => { log("Epic: purchase dates unavailable, new games get today's date"); Default::default() }
        }
    }
    /// `_disambiguate_same_names`, for the games of this sync: two products under one title (a base game and a standalone expansion) are renamed from their own store page.
    fn disambiguate(&self, recs: &mut [GameRecord]) {
        let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, r) in recs.iter().enumerate() { groups.entry(title_key(&r.name)).or_default().push(i); }
        let mut taken: HashSet<String> = groups.keys().cloned().collect();
        let mut keys: Vec<&String> = groups.keys().collect(); keys.sort();
        let plan: Vec<(String, Vec<usize>)> = keys.into_iter().map(|k| (k.clone(), groups[k].clone())).collect();
        for (key, idx) in plan {
            let nss: HashSet<&Option<String>> = idx.iter().map(|i| &recs[*i].namespace).collect();
            if key.is_empty() || idx.len() < 2 || nss.len() < 2 { continue; }
            for i in idx {
                let Some(ns) = recs[i].namespace.clone() else { continue };
                let els = self.store_elements(&search_query(), json!({"namespace": ns}));
                let title = pick_store_element(&els, &ns, None).and_then(|e| sv(e, "title")).unwrap_or("").trim().to_string();
                let tk = title_key(&title);
                if tk.is_empty() || tk == key || taken.contains(&tk) { continue; }
                log(&format!("Epic names: {:?} shares its name with another Epic game, renamed {:?}", recs[i].name, title));
                recs[i].name = title; taken.insert(tk);
            }
        }
    }
}

impl LibrarySource for EpicSource {
    fn id(&self) -> &str { "epic_games" }
    fn label(&self) -> &str { "Epic Games" }
    /// Epic's own tall and wide key images from the game's catalog entry (the Artwork Sources "Store" option); `[]` when signed out or the entry has none. Epic has no icon art.
    fn art_urls(&self, g: &GameRef) -> Vec<(String, String)> {
        let (ns, _) = self.game_row(g.appid);
        if ns.is_empty() || g.platform_id.is_empty() { return vec![]; }
        match self.catalog_entry(&ns, &g.platform_id) { Ok(Some(e)) => art_urls(&e).into_iter().map(|(k, v)| (k.to_string(), v)).collect(), _ => vec![] }
    }
    fn caps(&self) -> Caps { Caps { auth: true, sync: true, rescrape: true, launch: true, installed_scan: true, description: true, art: true, store_page: true, purchase_dates: true, ..Default::default() } }
    fn purchase_dates(&self, cx: &SourceCtx, appids: &[i64]) -> Result<Vec<(i64, Option<i64>)>, String> {
        if self.secret("account_id").is_none() { return Err("Epic Games is not connected. Connect it and try again.".into()); }
        let (ns, cid) = self.entitlement_dates();
        if ns.is_empty() && cid.is_empty() { return Err("Could not read your Epic entitlements. Reconnect Epic and try again.".into()); }
        Ok(super::platform_rows(&cx.db, appids).into_iter().map(|(id, pid, pns)| (id, cid.get(&pid).or_else(|| ns.get(&pns)).copied())).collect())
    }
    fn store_url(&self, g: &GameRef) -> Option<String> { let s = g.slug.trim(); (!s.is_empty()).then(|| format!("https://store.epicgames.com/p/{s}")) }

    fn auth_status(&self) -> Result<AuthStatus, String> { Ok(AuthStatus { connected: self.connected(), username: self.connected().then(|| self.secret("display_name")).flatten() }) }
    fn web_login_done(&self) -> Option<serde_json::Value> { Some(serde_json::json!({"kind": "body", "url_contains": "/id/api/redirect", "text_contains": "authorizationCode"})) }
    fn paste_login_kept(&self) -> bool { false }   // the sign-in window works (confirmed live 2026-11-22)
    fn auth_begin(&self) -> Result<AuthBegin, String> {
        Ok(AuthBegin { url: AUTH_URL.into(), placeholder: "Paste the authorizationCode value".into(), instructions: vec![
            "Zest opens the Epic login page in your browser. Log in to your Epic Games account.".into(),
            "You land on a page showing a code. Copy the value next to authorizationCode (the whole page text also works) and paste it below.".into(),
            "If the page is blocked by a verification check, sign in with your regular browser; Zest never needs your password.".into()] })
    }
    fn auth_complete(&self, value: &str) -> Result<AuthStatus, String> {
        let code = extract_auth_code(value);
        if code.is_empty() { return Err("Could not extract an authorization code from the input".into()); }
        let j = self.token_post(&format!("grant_type=authorization_code&code={}", enc(&code))).map_err(EpicErr::text)?;
        let name = sv(&j, "displayName").unwrap_or("").to_string();
        for (k, v) in [("access_token", sv(&j, "access_token")), ("refresh_token", sv(&j, "refresh_token")), ("account_id", sv(&j, "account_id"))] {
            match v.filter(|s| !s.is_empty()) { Some(v) => self.set(k, Some(v)), None => if k != "account_id" { return Err("Unexpected token response from Epic.".into()) } }
        }
        self.set("display_name", Some(&name)); self.set("expires_at", None);
        *self.token.lock().unwrap() = None;
        log(&format!("Epic: connected as {name:?}"));
        Ok(AuthStatus { connected: true, username: Some(name) })
    }
    fn auth_disconnect(&self) -> Result<(), String> { for k in ["access_token", "refresh_token", "expires_at", "account_id", "display_name"] { self.set(k, None); } *self.token.lock().unwrap() = None; Ok(()) }

    fn launch(&self, g: &GameRef) -> Result<Launch, String> {
        let name = if !g.appname.trim().is_empty() { g.appname.trim() } else { g.slug.trim() };
        if name.is_empty() { return Err("Epic game has no app name, try syncing the library again".into()); }
        let url = if g.installed { format!("com.epicgames.launcher://apps/{name}?action=launch&silent=true") } else { format!("com.epicgames.launcher://apps/{name}?action=install") };
        Ok(Launch { action: self.send(url)?, record: g.installed })
    }
    fn scan_junk(&self) -> Result<Vec<super::JunkHit>, String> {
        if !self.connected() { return Ok(vec![]); }
        let rows: Vec<(i64, String, String, String)> = {
            let Ok(c) = rusqlite::Connection::open_with_flags(&self.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { return Ok(vec![]) };
            let mut st = c.prepare("SELECT appid, COALESCE(name,''), platform_id, platform_ns FROM games WHERE platform='epic_games' AND platform_id IS NOT NULL AND platform_ns IS NOT NULL AND platform_ns != '' AND COALESCE(playtime_forever,0)=0 AND COALESCE(installed,0)=0 AND COALESCE(completion_status,'Never Played')='Never Played' AND (duplicate_of IS NULL OR duplicate_auto=1)").map_err(|e| e.to_string())?;
            let v: Vec<_> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).map_err(|e| e.to_string())?.flatten().collect();
            v
        };
        let mut by_ns: std::collections::BTreeMap<String, Vec<(i64, String, String)>> = Default::default();
        for (id, name, cid, ns) in rows { by_ns.entry(ns).or_default().push((id, name, cid)); }
        let mut out = Vec::new();
        for (ns, group) in by_ns {
            let mut entries: HashMap<String, Value> = HashMap::new();
            for chunk in group.chunks(50) {
                let mut u = format!("{CATALOG_URL}/{}/bulk/items?", enc(&ns));
                for (_, _, cid) in chunk { u.push_str(&format!("id={}&", enc(cid))); }
                u.push_str("country=US&locale=en&includeMainGameDetails=true");
                match self.api_get(&u, 25) {
                    Ok(r) if r.status == 200 => { if let Some(o) = serde_json::from_str::<Value>(&r.body).ok().and_then(|v| v.as_object().cloned()) { entries.extend(o); } }
                    Ok(r) => log(&format!("Epic scan_junk catalog fetch [{ns}]: HTTP {}", r.status)),
                    Err(e) => log(&format!("Epic scan_junk catalog fetch [{ns}]: {}", e.text())),
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            for (appid, name, cid) in group {
                let Some(e) = entries.get(&cid).filter(|e| !e.is_null()) else { continue };
                let title = sv(e, "title").filter(|t| !t.is_empty()).unwrap_or(&name).to_string();
                if is_dlc_or_extra(e, &title) { out.push(super::JunkHit { appid, name, reason: junk_reason(e).to_string() }); }
            }
        }
        Ok(out)
    }
    fn installed_scan(&self) -> Option<Vec<InstalledRow>> {
        let c = rusqlite::Connection::open_with_flags(&self.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
        let mut st = c.prepare("SELECT platform_id, COALESCE(NULLIF(platform_appname, ''), platform_slug, '') FROM games WHERE platform = 'epic_games' AND platform_id IS NOT NULL").ok()?;
        let rows: Vec<(String, String)> = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).ok()?.flatten().collect();
        let prefix = super::rockstar::prefix_setting(&self.config_dir, "epic_games");
        let dir = manifests_dir(prefix.as_deref());
        // no manifest folder (no launcher or no prefix): nothing is installed, so stale flags are cleared (as PlayDate)
        let installed = dir.as_deref().map(read_manifests).unwrap_or_default();
        Some(rows.into_iter().map(|(id, name)| match installed.get(name.trim()).filter(|_| !name.trim().is_empty()) {
            Some(loc) => InstalledRow { installed: true, install_path: Some(host_install_path(prefix.as_deref(), loc)), platform_id: id },
            None => InstalledRow { installed: false, install_path: Some(String::new()), platform_id: id },
        }).collect())
    }

    fn sync(&self, _cx: &SourceCtx, sink: &mut dyn FnMut(Vec<GameRecord>), cancel: &Cancel) -> Result<usize, String> {
        if !self.connected() { return Err("Not connected".into()); }
        crate::net::set_activity("Epic Games: reading your library");
        let r = self.api_get(ASSETS_URL, 20).map_err(EpicErr::text)?;
        let assets = match serde_json::from_str::<Value>(&r.body).ok().and_then(|v| v.as_array().cloned()).filter(|_| r.status == 200) { Some(a) => a, None => return Err(format!("Failed to fetch Epic library (HTTP {})", r.status)) };
        let (existing, blacklisted) = self.ids();
        let plan = plan_sync(&assets, &existing, &blacklisted);
        let total: usize = plan.best.len();
        let mut sent = 0usize;
        // Games already in the library are listed too (a bare record: the importer only refreshes what a record carries), so the sync says how many Epic has, as the other sources do.
        let names = self.known_names(); let mut listed: HashSet<&str> = HashSet::new();
        let known: Vec<GameRecord> = assets.iter().filter(|a| sv(a, "namespace") != Some("ue") && !sv(a, "appName").unwrap_or("").is_empty()).filter_map(|a| sv(a, "catalogItemId")).filter(|c| existing.contains(*c) && listed.insert(c))
            .filter_map(|c| names.get(c).map(|n| GameRecord { platform_id: c.to_string(), name: n.clone(), ..Default::default() })).collect();
        sent += known.len(); if !known.is_empty() { sink(known); }
        log(&format!("Epic sync: {} assets, {} already in the library, {total} new games to process", assets.len(), sent));
        if total == 0 { crate::net::set_activity(""); return Ok(sent); }
        let (ns_dates, cid_dates) = self.entitlement_dates();
        let mut done = 0usize;
        let mut seen: HashSet<String> = HashSet::new();
        let mut store_cache: HashMap<(String, String), StoreData> = HashMap::new();
        for (ns, items) in &plan.by_ns {
            let cids: Vec<&str> = items.iter().map(|(c, _)| c.as_str()).collect();
            for chunk in cids.chunks(50) {
                if cancel.is_set() { crate::net::set_activity(""); return Ok(sent); }
                crate::net::set_activity(&format!("Epic Games: reading catalog ({}/{total})", done.min(total)));
                let catalog: Value = match self.api_get(&Self::catalog_url(ns, chunk, true), 20) {
                    Ok(r) if r.status == 200 => serde_json::from_str(&r.body).unwrap_or(Value::Null),
                    Ok(r) => { log(&format!("Epic catalog batch: HTTP {}", r.status)); Value::Null }
                    Err(EpicErr::Auth(m)) => return Err(m),
                    Err(EpicErr::Other(m)) => { log(&format!("Epic catalog batch: {m}")); Value::Null }
                };
                let mut batch: Vec<GameRecord> = vec![];
                for cid in chunk {
                    if !seen.insert(cid.to_string()) { continue; }
                    done += 1;
                    let entry = catalog.get(*cid).cloned().filter(|e| e.is_object()).unwrap_or_else(|| json!({}));
                    let appname = plan.best.get(*cid).cloned().unwrap_or_default();
                    let name = sv(&entry, "title").filter(|t| !t.is_empty()).map(String::from).unwrap_or_else(|| appname.clone());
                    if is_dlc_or_extra(&entry, &name) { log(&format!("Epic sync: skipping {name:?} (DLC / digital extra / non-game app)")); continue; }
                    let key = (ns.clone(), name.clone());
                    if !store_cache.contains_key(&key) { let d = self.store_data(ns, Some(&name)); store_cache.insert(key.clone(), d); }
                    let store = &store_cache[&key];
                    let meta = extract_metadata(&entry);
                    batch.push(GameRecord {
                        platform_id: cid.to_string(), name, slug: store.slug.clone().or(meta.slug.clone()), appname: Some(appname).filter(|a| !a.is_empty()), namespace: Some(ns.clone()),
                        date_added: cid_dates.get(*cid).or_else(|| ns_dates.get(ns)).copied(), release_date: store.release,
                        developers: meta.developers, publishers: meta.publishers, genres: meta.genres, tags: store.tags.clone(), description: store.description.clone(), ..Default::default()
                    });
                }
                self.disambiguate(&mut batch);
                sent += batch.len(); if !batch.is_empty() { sink(batch); }
                std::thread::sleep(Duration::from_millis(if cfg!(test) { 0 } else { 300 }));
            }
        }
        crate::net::set_activity("");
        Ok(sent)
    }

    /// `scrape_single`: catalog entry, store tags and slug, release date (only if earlier), ratings. Art is core's job.
    fn rescrape(&self, cx: &SourceCtx, g: &GameRef) -> Fetch<Option<Update>> {
        if !self.connected() { log("Epic rescrape: not connected"); return Ok(None); }
        let (ns, release) = self.game_row(g.appid);
        if ns.is_empty() { return Ok(None); }
        let Some(entry) = self.catalog_entry(&ns, &g.platform_id)? else { return Ok(None) };
        let mut out: Update = vec![("meta_fetched", Sql::Text(cx.today.clone()))];
        out.extend(merge_store(&extract_metadata(&entry), &self.store_data(&ns, Some(&g.name)), release));
        out.extend(self.ratings(&ns));
        Ok(Some(out))
    }
    fn description(&self, _cx: &SourceCtx, g: &GameRef) -> Option<String> {
        let (ns, _) = self.game_row(g.appid);
        if ns.is_empty() { return None; }
        self.store_data(&ns, Some(&g.name)).description
    }
}

#[cfg(test)]
mod behaviour {
    use super::*;
    use crate::net::{NetError, Response};

    /// A fake Epic: token endpoint, launcher assets, catalog, entitlements and store GraphQL. Records what was asked.
    struct Site { token_ok: bool, refresh_ok: bool, assets_status: u16, token_posts: Mutex<Vec<String>>, asked: Mutex<Vec<String>> }
    fn site() -> Site { Site { token_ok: true, refresh_ok: true, assets_status: 200, token_posts: Mutex::new(vec![]), asked: Mutex::new(vec![]) } }
    fn r(s: u16, b: Value) -> Result<Response, NetError> { Ok(Response { status: s, retry_after: None, body: b.to_string() }) }
    fn q_vars_ns(q: &Value) -> String { q["variables"]["namespace"].as_str().unwrap_or("").to_string() }
    fn offer() -> Value { json!({"title": "Game One", "namespace": "ns1", "offerType": "BASE_GAME", "description": "Blurb", "tags": [{"id": "t1"}], "pcReleaseDate": "2020-01-02T00:00:00Z",
        "catalogNs": {"mappings": [{"pageType": "productHome", "pageSlug": "game-one-ab12/home"}]}}) }
    impl Http for Site {
        fn post(&self, u: &str, h: &[(&str, &str)], body: &str, _t: Duration) -> Result<Response, NetError> {
            if u == TOKEN_URL {
                assert!(h.iter().any(|(k, v)| *k == "Authorization" && *v == BASIC));
                self.token_posts.lock().unwrap().push(body.to_string());
                let ok = if body.contains("authorization_code") { self.token_ok && body.contains("code=GOODCODE") } else { self.refresh_ok && body.contains("refresh_token=RT1") };
                return if ok { r(200, json!({"access_token": "AT2", "refresh_token": "RT2", "expires_in": 7200, "account_id": "acc", "displayName": "Rob"})) } else { r(400, json!({"errorMessage": "bad grant"})) };
            }
            let q0: Value = serde_json::from_str(body).unwrap(); let q = q0["query"].as_str().unwrap();
            self.asked.lock().unwrap().push(format!("gql:{}", q.split_whitespace().nth(1).unwrap_or("")));
            if q.contains("catalogTags") { return r(200, json!({"data": {"Catalog": {"tags": {"elements": [{"id": "t1", "name": "Co-op"}]}}}})); }
            if q.contains("getProductResult") { return r(200, json!({"data": {"RatingsPolls": {"getProductResult": {"averageRating": 4.5, "pollResult": [{"total": 100}]}}}})); }
            let ns = q_vars_ns(&q0);
            let mut o = offer(); if ns == "nsB" { o["title"] = json!("Game One - Aiko's Choice"); o["namespace"] = json!("nsB"); }
            r(200, json!({"data": {"Catalog": {"searchStore": {"elements": [o]}}}}))
        }
        fn get(&self, u: &str, h: &[(&str, &str)], _t: Duration) -> Result<Response, NetError> {
            self.asked.lock().unwrap().push(u.to_string());
            assert!(h.iter().any(|(k, v)| *k == "Authorization" && *v == "Bearer AT2"), "api calls carry the refreshed access token: {u}");
            if u.starts_with(ASSETS_URL.split('?').next().unwrap()) {
                if self.assets_status != 200 { return r(self.assets_status, json!({})); }
                return r(200, json!([
                    {"namespace": "ns1", "catalogItemId": "c1", "appName": "Entitle1", "buildVersion": ""}, {"namespace": "ns1", "catalogItemId": "c1", "appName": "Real1", "buildVersion": "1.0"},
                    {"namespace": "ns2", "catalogItemId": "c2", "appName": "Ost", "buildVersion": "1"}, {"namespace": "ue", "catalogItemId": "cu", "appName": "UE", "buildVersion": "1"},
                    {"namespace": "ns3", "catalogItemId": "c3", "appName": "Known", "buildVersion": "1"}, {"namespace": "ns4", "catalogItemId": "c4", "appName": "Removed", "buildVersion": "1"}]));
            }
            if u.contains("/entitlements") { return r(200, json!([{"namespace": "ns1", "catalogItemId": "x", "grantDate": "2021-03-04T05:06:07Z"}])); }
            if u.contains("/ns1/bulk/items") { return r(200, json!({"c1": {"title": "Game One", "urlSlug": "game-one/home", "customAttributes": {"developerName": {"value": "Dev"}, "genres": {"value": "Action,RPG"}}}})); }
            if u.contains("/ns2/bulk/items") { return r(200, json!({"c2": {"title": "Game Two Soundtrack", "categories": [{"path": "digitalextras"}]}})); }
            r(404, json!({}))
        }
    }
    fn setup(tag: &str, site: Site, tokens: bool) -> (EpicSource, Arc<Site>, PathBuf) {
        let d = std::env::temp_dir().join(format!("zest_epic_{tag}_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("config.json"), "{}").unwrap();
        if tokens { for (k, v) in [("access_token", "AT1"), ("refresh_token", "RT1"), ("account_id", "acc"), ("display_name", "Rob")] { crate::account::plugin_secret_set(&d, "epic_games", k, Some(v)).unwrap(); } }
        let db = d.join("games_1.db");
        rusqlite::Connection::open(&db).unwrap().execute_batch("CREATE TABLE games (appid INTEGER PRIMARY KEY, name TEXT, platform TEXT, platform_id TEXT, platform_slug TEXT, platform_ns TEXT, release_date INTEGER); CREATE TABLE blacklist (appid INTEGER, platform_id TEXT, platform TEXT);").unwrap();
        let s = Arc::new(site); (EpicSource::new(s.clone(), d.clone(), db), s, d)
    }
    fn cx(d: &PathBuf, s: Arc<Site>) -> SourceCtx { let h: Arc<dyn Http> = s; SourceCtx { http: h.clone(), steam: Arc::new(crate::steamclient::Steam::new(h, None, 10.0)), root: d.clone(), db: d.join("games_1.db"), today: "2026-11-01".into(), appinfo: Arc::new(|_| Default::default()), account_id: None } }
    fn config_text(d: &PathBuf) -> String { std::fs::read_to_string(d.join("config.json")).unwrap() }

    #[test] fn signing_in_exchanges_the_pasted_code_and_never_stores_it() {
        let (s, site, d) = setup("signin", site(), false);
        assert_eq!(s.auth_status().unwrap(), AuthStatus { connected: false, username: None });
        let st = s.auth_complete(r#"{"authorizationCode":"GOODCODE","redirectUrl":"x"}"#).unwrap();
        assert_eq!(st, AuthStatus { connected: true, username: Some("Rob".into()) });
        let cfg = config_text(&d);
        assert!(cfg.contains("AT2") && cfg.contains("RT2") && cfg.contains("acc") && cfg.contains("Rob") && !cfg.contains("GOODCODE"));
        assert_eq!(site.token_posts.lock().unwrap().len(), 1);
        s.auth_disconnect().unwrap();
        assert!(!config_text(&d).contains("RT2")); assert_eq!(s.auth_status().unwrap().connected, false);
    }
    #[test] fn a_rejected_code_saves_nothing_and_says_why() {
        let (s, _, d) = setup("reject", Site { token_ok: false, ..site() }, false);
        let e = s.auth_complete("GOODCODE").unwrap_err(); assert!(e.contains("HTTP 400") && e.contains("bad grant"), "{e}");
        assert!(!config_text(&d).contains("access_token"));
        assert!(s.auth_complete("   ").unwrap_err().contains("authorization code"));
    }
    #[test] fn the_rotating_refresh_token_is_saved_and_the_access_token_reused() {
        let (s, site, d) = setup("rotate", site(), true);
        rusqlite::Connection::open(d.join("games_1.db")).unwrap().execute("INSERT INTO games (appid, name, platform, platform_id, platform_ns) VALUES (-1, 'Game One', 'epic_games', 'c1', 'ns1')", []).unwrap();
        let g = GameRef { appid: -1, name: "Game One".into(), platform_id: "c1".into(), ..Default::default() };
        let _ = (s.rescrape(&cx(&d, site.clone()), &g), s.rescrape(&cx(&d, site.clone()), &g));
        let posts = site.token_posts.lock().unwrap();
        assert_eq!(posts.len(), 1, "one refresh serves later calls"); assert!(posts[0].contains("grant_type=refresh_token") && posts[0].contains("refresh_token=RT1"));
        let cfg = config_text(&d); assert!(cfg.contains("RT2") && !cfg.contains("RT1") && cfg.contains("AT2"));
    }
    #[test] fn a_dead_refresh_token_asks_to_connect_again() {
        let (s, site, d) = setup("dead", Site { refresh_ok: false, ..site() }, true);
        let e = s.sync(&cx(&d, site.clone()), &mut |_| {}, &Cancel::new()).unwrap_err(); assert!(e.contains("expired"), "{e}");
        let (s, site, d) = setup("noconn", self::site(), false);
        assert_eq!(s.sync(&cx(&d, site), &mut |_| {}, &Cancel::new()).unwrap_err(), "Not connected");
    }
    #[test] fn sync_adds_only_new_non_dlc_games_with_dates_and_store_data() {
        let (s, site, d) = setup("sync", site(), true);
        let c = rusqlite::Connection::open(d.join("games_1.db")).unwrap();
        c.execute("INSERT INTO games (appid, name, platform, platform_id) VALUES (-1, 'Known', 'epic_games', 'c3')", []).unwrap();
        c.execute("INSERT INTO blacklist VALUES (-9, 'c4', 'epic_games')", []).unwrap();
        let mut got = vec![]; let n = s.sync(&cx(&d, site.clone()), &mut |b| got.extend(b), &Cancel::new()).unwrap();
        assert_eq!(n, 2, "the known game is counted as listed, and the one new game");
        assert_eq!(got.len(), 2, "c2 is a soundtrack, c4 was removed, `ue` is Unreal's own");
        assert_eq!((got[0].platform_id.as_str(), got[0].name.as_str(), got[0].appname.clone(), got[0].developers.clone()), ("c3", "Known", None, None), "a known game is a bare record, nothing to overwrite");
        let g = &got[1];
        assert_eq!((g.platform_id.as_str(), g.name.as_str(), g.appname.as_deref(), g.namespace.as_deref()), ("c1", "Game One", Some("Real1"), Some("ns1")), "the installable record names the launch");
        assert_eq!(g.slug.as_deref(), Some("game-one-ab12"), "the store page slug beats urlSlug");
        assert_eq!((g.developers.as_deref(), g.genres.as_deref(), g.tags.as_deref(), g.description.as_deref()), (Some("Dev"), Some("Action,RPG"), Some("Co-op"), Some("Blurb")));
        assert_eq!((g.date_added, g.release_date), (Some(1614834367), Some(1577923200)));
        let asked = site.asked.lock().unwrap();
        assert!(!asked.iter().any(|u| u.contains("ns3") || u.contains("ns4")), "no catalog calls for games already known or removed");
    }
    #[test] fn a_cancelled_sync_stops_before_the_catalog_and_a_bad_assets_answer_is_reported() {
        let (s, site, d) = setup("cancel", site(), true);
        let cancel = Cancel::new(); cancel.cancel();
        let mut got = vec![]; s.sync(&cx(&d, site.clone()), &mut |b| got.extend(b), &cancel).unwrap(); assert!(got.iter().all(|r| r.developers.is_none()), "nothing new was read");
        assert!(!site.asked.lock().unwrap().iter().any(|u| u.contains("/bulk/items")));
        let (s, site, d) = setup("assets500", Site { assets_status: 500, ..self::site() }, true);
        assert!(s.sync(&cx(&d, site), &mut |_| {}, &Cancel::new()).unwrap_err().contains("HTTP 500"));
    }
    #[test] fn rescrape_reads_catalog_store_and_ratings_and_keeps_an_earlier_release_date() {
        let (s, site, d) = setup("rescrape", site(), true);
        let c = rusqlite::Connection::open(d.join("games_1.db")).unwrap();
        c.execute("INSERT INTO games (appid, name, platform, platform_id, platform_ns, release_date) VALUES (-1, 'Game One', 'epic_games', 'c1', 'ns1', 1500000000)", []).unwrap();
        let g = GameRef { appid: -1, name: "Game One".into(), platform: "epic_games".into(), platform_id: "c1".into(), ..Default::default() };
        let u = s.rescrape(&cx(&d, site.clone()), &g).unwrap().unwrap();
        let get = |k: &str| u.iter().find(|(c, _)| *c == k).map(|(_, v)| v.clone());
        assert_eq!(get("meta_fetched"), Some(Sql::Text("2026-11-01".into()))); assert_eq!(get("developers"), Some(Sql::Text("Dev".into())));
        assert_eq!(get("platform_slug"), Some(Sql::Text("game-one-ab12".into()))); assert_eq!(get("tags"), Some(Sql::Text("Co-op".into())));
        assert_eq!(get("release_date"), None, "Epic's 2020 date is later than the 2017 one the game has");
        assert_eq!((get("review_percentage"), get("review_score")), (Some(Sql::Integer(90)), Some(Sql::Text("Very Positive".into()))));
        c.execute("UPDATE games SET release_date = NULL", []).unwrap();
        assert_eq!(s.rescrape(&cx(&d, site.clone()), &g).unwrap().unwrap().iter().find(|(c, _)| *c == "release_date").map(|(_, v)| v.clone()), Some(Sql::Integer(1577923200)));
        assert_eq!(s.description(&cx(&d, site), &g).as_deref(), Some("Blurb"));
    }
    #[test] fn store_art_is_the_catalogs_tall_and_wide_key_images_first_of_each_and_nothing_when_signed_out() {
        let e = json!({"keyImages": [{"type": "Thumbnail", "url": "t"}, {"type": "DieselGameBoxTall", "url": "tall1"}, {"type": "DieselGameBox", "url": "wide"}, {"type": "DieselGameBoxTall", "url": "tall2"}, {"type": "DieselGameBox", "url": ""}]});
        assert_eq!(art_urls(&e), vec![("vertical", "tall1".to_string()), ("horizontal", "wide".to_string())]);
        assert!(art_urls(&json!({})).is_empty());
        let d = std::env::temp_dir().join(format!("zest_epic_storeart_unit_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        let s = EpicSource::new(Arc::new(crate::net::UreqHttp::new()), d.clone(), d.join("games.db"));
        assert!(LibrarySource::art_urls(&s, &GameRef { appid: 1, platform_id: "cid".into(), ..Default::default() }).is_empty(), "no namespace on record");
        assert!(s.caps().art);
    }
    #[cfg(not(target_os = "linux"))]
    #[test] fn launch_and_links() {
        let (s, _, _) = setup("launch", site(), false);
        let g = GameRef { appname: "Real1".into(), installed: true, slug: "game-one".into(), ..Default::default() };
        assert_eq!(s.launch(&g).unwrap(), Launch { action: Action::OpenUrl("com.epicgames.launcher://apps/Real1?action=launch&silent=true".into()), record: true });
        assert!(s.launch(&GameRef::default()).is_err());
        assert_eq!(s.store_url(&g).as_deref(), Some("https://store.epicgames.com/p/game-one")); assert_eq!(s.store_url(&GameRef::default()), None);
    }
    #[cfg(target_os = "linux")]
    #[test] fn launch_goes_through_the_wine_prefix_and_the_scan_reads_the_manifests() {
        let (s, _, d) = setup("launch", site(), false);
        let g = GameRef { appname: "Real1".into(), installed: true, slug: "game-one".into(), ..Default::default() };
        assert!(s.launch(&g).unwrap_err().contains("not set up"));
        assert!(s.launch(&GameRef::default()).is_err());
        assert_eq!(s.store_url(&g).as_deref(), Some("https://store.epicgames.com/p/game-one")); assert_eq!(s.store_url(&GameRef::default()), None);
        let pfx = d.join("pfx"); let man = pfx.join("drive_c/ProgramData/Epic/EpicGamesLauncher/Data/Manifests"); std::fs::create_dir_all(&man).unwrap();
        crate::account::plugin_secret_set(&d, "epic_games", "prefix", Some(&pfx.to_string_lossy())).unwrap();
        crate::wine::TEST_RUNTIME.with(|t| *t.borrow_mut() = Some(Ok(crate::wine::Runtime::Wine { wine: "/usr/bin/wine".into() })));
        match s.launch(&g).unwrap() { Launch { action: Action::Spawn { args, env, .. }, record: true } => { assert_eq!(args, vec!["start".to_string(), "com.epicgames.launcher://apps/Real1?action=launch&silent=true".to_string()]); assert!(env.contains(&("WINEDLLOVERRIDES".into(), Some("winegstreamer=".into())))); } l => panic!("{l:?}") }
        match s.launch(&GameRef { installed: false, ..g.clone() }).unwrap() { Launch { action: Action::Spawn { args, .. }, record: false } => assert!(args[1].ends_with("?action=install")), l => panic!("{l:?}") }
        // the scan: a manifest with a matching AppName is installed, at its translated folder
        std::fs::write(man.join("A.item"), r#"{"AppName": "Real1", "InstallLocation": "C:\\Program Files\\Epic Games\\GameOne"}"#).unwrap();
        std::fs::write(man.join("junk.txt"), "x").unwrap(); std::fs::write(man.join("B.item"), "not json").unwrap();
        let c = rusqlite::Connection::open(d.join("games_1.db")).unwrap();
        c.execute_batch("DROP TABLE IF EXISTS games; CREATE TABLE games (platform TEXT, platform_id TEXT, platform_appname TEXT, platform_slug TEXT); INSERT INTO games VALUES ('epic_games','1','Real1','a'),('epic_games','2','Other','b'),('epic_games','3','','');").unwrap();
        let rows = s.installed_scan().unwrap();
        assert_eq!(rows.iter().map(|r| (r.platform_id.as_str(), r.installed)).collect::<Vec<_>>(), vec![("1", true), ("2", false), ("3", false)]);
        assert_eq!(rows[0].install_path.as_deref(), Some(&*pfx.join("drive_c/Program Files/Epic Games/GameOne").to_string_lossy()));
    }
    #[test] fn two_products_under_one_title_are_renamed_from_their_store_titles() {
        let (s, _, _) = setup("names", site(), true);
        let mk = |id: &str, ns: &str| GameRecord { platform_id: id.into(), name: "Game One".into(), namespace: Some(ns.into()), ..Default::default() };
        let mut same = vec![mk("1", "ns1"), mk("2", "ns1")];   // one namespace listed twice (entitlement + installable) is not a collision
        s.disambiguate(&mut same); assert_eq!((same[0].name.as_str(), same[1].name.as_str()), ("Game One", "Game One"));
        let mut two = vec![mk("1", "ns1"), mk("2", "nsB")];
        s.disambiguate(&mut two); assert_eq!((two[0].name.as_str(), two[1].name.as_str()), ("Game One", "Game One - Aiko's Choice"), "only the one whose own title differs is renamed");
    }
}

#[cfg(test)]
mod deep_scan_tests {
    use super::*;
    use crate::net::{NetError, Response};
    use std::sync::Mutex;
    /// A fake Epic: the token endpoint and the catalog's bulk lookup (one namespace); records the catalog URLs asked.
    struct Site { catalog: Value, asked: Mutex<Vec<String>> }
    impl Http for Site {
        fn get(&self, u: &str, h: &[(&str, &str)], _t: Duration) -> Result<Response, NetError> {
            assert!(h.iter().any(|(k, v)| *k == "Authorization" && v.starts_with("Bearer ")), "the bearer token is sent");
            self.asked.lock().unwrap().push(u.to_string());
            Ok(Response { status: 200, retry_after: None, body: self.catalog.to_string() })
        }
        fn post(&self, _u: &str, _h: &[(&str, &str)], _b: &str, _t: Duration) -> Result<Response, NetError> {
            Ok(Response { status: 200, retry_after: None, body: json!({"access_token": "AT", "refresh_token": "RT2", "expires_in": 7200}).to_string() })
        }
    }
    #[test] fn the_deep_scan_flags_untouched_dlc_and_extras_only() {
        let d = std::env::temp_dir().join(format!("zest_epic_scan_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("config.json"), "{}").unwrap();
        crate::account::plugin_secret_set(&d, "epic_games", "access_token", Some("old")).unwrap(); crate::account::plugin_secret_set(&d, "epic_games", "refresh_token", Some("RT")).unwrap();
        let db = d.join("games_1.db");
        rusqlite::Connection::open(&db).unwrap().execute_batch("CREATE TABLE games (appid INTEGER, name TEXT, platform TEXT, platform_id TEXT, platform_ns TEXT, playtime_forever INTEGER, installed INTEGER, completion_status TEXT, duplicate_of TEXT, duplicate_auto INTEGER);
            INSERT INTO games VALUES (1, 'Real Game', 'epic_games', 'c1', 'ns', 0, 0, 'Never Played', NULL, 0);
            INSERT INTO games VALUES (2, 'Real Game Season Pass', 'epic_games', 'c2', 'ns', 0, 0, 'Never Played', NULL, 0);
            INSERT INTO games VALUES (3, 'Real Game Soundtrack', 'epic_games', 'c3', 'ns', 0, 0, 'Never Played', NULL, 0);
            INSERT INTO games VALUES (4, 'Played DLC', 'epic_games', 'c4', 'ns', 90, 0, 'Never Played', NULL, 0);
            INSERT INTO games VALUES (5, 'Dev Kit', 'epic_games', 'c5', 'ns', 0, 0, 'Never Played', NULL, 0);").unwrap();
        let site = Arc::new(Site { asked: Mutex::new(vec![]), catalog: json!({
            "c1": {"title": "Real Game", "categories": [{"path": "games"}, {"path": "applications"}]},
            "c2": {"title": "Real Game Season Pass", "mainGameItem": {"id": "c1"}, "categories": [{"path": "addons"}]},
            "c3": {"title": "Real Game Soundtrack", "categories": [{"path": "digitalextras"}]},
            "c4": {"title": "Played DLC", "mainGameItem": {"id": "c1"}, "categories": [{"path": "addons"}]},
            "c5": {"title": "Dev Kit", "categories": [{"path": "engines"}]}}) });
        let s = EpicSource::new(site.clone(), d.clone(), db);
        let got: Vec<(i64, String)> = s.scan_junk().unwrap().into_iter().map(|h| (h.appid, h.reason)).collect();
        assert_eq!(got, vec![(2, "DLC / add-on".to_string()), (3, "digital extra (soundtrack / artbook)".to_string()), (5, "dev kit".to_string())], "{got:?}");
        let asked = site.asked.lock().unwrap();
        assert_eq!(asked.len(), 1, "one namespace, one request: {asked:?}");
        assert!(asked[0].contains("id=c1") && asked[0].contains("includeMainGameDetails=true") && !asked[0].contains("c4"), "the played game is not asked about: {}", asked[0]);
    }
}
