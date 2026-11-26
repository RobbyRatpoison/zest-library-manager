//! PlayStation as a library source (port of PlayDate's `playstation` plugin: `psn.py` + `store.py`). Sony has no public library API; this follows what the PlayStation
//! app does: the user pastes their NPSSO, it is exchanged for a short-lived access token and a long-lived refresh token (only the refresh token is kept), the purchased
//! games come from a persisted GraphQL query and playtime from the game list. Store pages give metadata, art and the demo/DLC verdict. The NPSSO, codes and tokens are
//! never logged. Purchase dates (read by a userscript in the browser) are not part of this port yet.
use super::{AuthBegin, AuthStatus, Caps, GameRef, LibrarySource, SourceCtx};
use crate::importer::GameRecord;
use crate::jobs::Update;
use crate::metadata::PageMeta;
use crate::net::{enc, log, Cancel, Http};
use crate::steamclient::Fetch;
use regex::Regex;
use rusqlite::types::Value as Sql;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const AUTH_BASE: &str = "https://ca.account.sony.com/api/authz/v3/oauth";
const CLIENT_ID: &str = "09515159-7237-4370-9b40-3806e67c0891";
const BASIC: &str = "Basic MDk1MTUxNTktNzIzNy00MzcwLTliNDAtMzgwNmU2N2MwODkxOnVjUGprYTV0bnRCMktxc1A=";
const REDIRECT: &str = "com.scee.psxandroid.scecompcall://redirect";
const SCOPE: &str = "psn:mobile.v2.core psn:clientapp";
const GRAPHQL: &str = "https://web.np.playstation.com/api/graphql/v1/op";
const PURCHASED_HASH: &str = "827a423f6a8ddca4107ac01395af2ec0eafd8396fc7fa204aaf9b7ed2eefa168";
const GAMELIST: &str = "https://m.np.playstation.com/api/gamelist/v2/users/me/titles";
const PROFILE: &str = "https://us-prof.np.community.playstation.net/userProfile/v1/users/me/profile2";
const UA: &str = "Mozilla/5.0 (Linux; Android 13) AppleWebKit/537.36 (KHTML, like Gecko) Mobile Safari/537.36";
const STORE_UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36";
const EXPIRED: &str = "Your PlayStation login expired. Disconnect and connect again.";
const GAME_CLASSES: &[&str] = &["FULL_GAME", "PREMIUM_EDITION", "GAME_BUNDLE"];
const NON_GAME_CLASSES: &[&str] = &["DEMO", "LEVEL", "ITEM", "ADD_ON", "ADDON", "AVATAR", "THEME", "SEASON_PASS", "CONSUMABLE", "VIRTUAL_CURRENCY", "EXPANSION", "BETA"];

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex { cell.get_or_init(|| Regex::new(pat).unwrap()) }
fn sv<'a>(v: &'a Value, k: &str) -> Option<&'a str> { v.get(k).and_then(|x| x.as_str()) }

// ---------------------------------------------------------------- pure helpers: sign-in text, durations, times

/// `parse_npsso`: the NPSSO from the page's JSON, a cookie string or the bare value; None unless it is 32 to 128 letters and digits.
pub fn parse_npsso(raw: &str) -> Option<String> {
    static J: OnceLock<Regex> = OnceLock::new(); static C: OnceLock<Regex> = OnceLock::new(); static OK: OnceLock<Regex> = OnceLock::new();
    let raw = raw.trim();
    let value = match re(&J, r#""npsso"\s*:\s*"([^"]+)""#).captures(raw).or_else(|| re(&C, r"\bnpsso=([A-Za-z0-9]+)").captures(raw)) { Some(m) => m[1].to_string(), None => raw.to_string() };
    let value = value.trim().trim_matches(|c| c == '"' || c == '\'').to_string();
    re(&OK, r"^[A-Za-z0-9]{32,128}$").is_match(&value).then_some(value)
}
pub fn looks_like_address(raw: &str) -> bool { static A: OnceLock<Regex> = OnceLock::new(); re(&A, r"^\s*https?://").is_match(raw) && parse_npsso(raw).is_none() }

/// `parse_duration`: minutes from an ISO 8601 duration such as `PT228H56M33S` (0 when empty or unreadable).
pub fn parse_duration(text: &str) -> i64 {
    static D: OnceLock<Regex> = OnceLock::new();
    let Some(c) = re(&D, r"^P(?:(\d+)D)?T?(?:(\d+)H)?(?:(\d+)M)?(?:(\d+(?:\.\d+)?)S)?$").captures(text) else { return 0 };
    let f = |i: usize| c.get(i).map_or(0.0, |m| m.as_str().parse::<f64>().unwrap_or(0.0));
    ((f(1) * 86400.0 + f(2) * 3600.0 + f(3) * 60.0 + f(4)) / 60.0).round_ties_even() as i64
}

/// `parse_time`: an ISO time as a Unix time (fractions dropped, a missing zone read as UTC); None when unreadable.
pub fn parse_time(text: &str) -> Option<i64> {
    static T: OnceLock<Regex> = OnceLock::new();
    let t = text.replace('Z', "+00:00");
    let c = re(&T, r"^(\d{4})-(\d{2})-(\d{2})(?:[T ](\d{2}):(\d{2})(?::(\d{2})(?:\.\d+)?)?)?([+-]\d{2}:\d{2})?$").captures(&t)?;
    let n = |i: usize| c.get(i).map_or(0, |m| m.as_str().parse::<i64>().unwrap_or(0));
    let (y, mo, d) = (n(1), n(2), n(3));
    let dim = match mo { 1 | 3 | 5 | 7 | 8 | 10 | 12 => 31, 4 | 6 | 9 | 11 => 30, 2 => if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 { 29 } else { 28 }, _ => return None };
    if y < 1 || d < 1 || d > dim || n(4) > 23 || n(5) > 59 || n(6) > 59 { return None; }
    let off = c.get(7).map_or(0, |m| { let s = m.as_str(); let sign = if s.starts_with('-') { -1 } else { 1 }; sign * (s[1..3].parse::<i64>().unwrap_or(0) * 3600 + s[4..6].parse::<i64>().unwrap_or(0) * 60) });
    let (yy, mm) = if mo <= 2 { (y - 1, mo + 9) } else { (y, mo - 3) };
    let era = yy.div_euclid(400); let yoe = yy - era * 400; let doy = (153 * mm + 2) / 5 + d - 1; let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146097 + doe - 719468) * 86400 + n(4) * 3600 + n(5) * 60 + n(6) - off)
}

fn clean(name: Option<&str>) -> String {
    static TM: OnceLock<Regex> = OnceLock::new(); static WS: OnceLock<Regex> = OnceLock::new();
    let n = re(&TM, "[\u{2122}\u{00ae}\u{00a9}]").replace_all(name.unwrap_or(""), "");
    re(&WS, r"\s+").replace_all(&n, " ").trim().to_string()
}

// ---------------------------------------------------------------- merging the two lists

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Entry { pub name: String, pub concept_id: Option<String>, pub title_id: Option<String>, pub platform: String, pub playtime: i64, pub last_played: Option<i64>, pub plus: bool, pub product_id: Option<String>, store_named: bool }

/// `merge`: one entry per game (a PS4 and a PS5 version share a concept), in the order Python builds them. Entries without a name are dropped.
pub fn merge(purchased: &[Value], played: &[Value]) -> Vec<(String, Entry)> {
    let mut out: Vec<(String, Entry)> = vec![]; let mut index: HashMap<String, usize> = HashMap::new(); let mut by_title: HashMap<String, String> = HashMap::new();
    let mut entry = |key: &str, out: &mut Vec<(String, Entry)>| -> usize { *index.entry(key.to_string()).or_insert_with(|| { out.push((key.to_string(), Entry::default())); out.len() - 1 }) };
    let idstr = |v: Option<&Value>| -> Option<String> { match v { Some(Value::String(s)) if !s.is_empty() => Some(s.clone()), Some(Value::Number(n)) if n.as_i64() != Some(0) => Some(n.to_string()), _ => None } };
    for t in played {
        let concept = t.get("concept").filter(|c| c.is_object()).cloned().unwrap_or(Value::Null);
        let cid = idstr(concept.get("id"));
        let key = cid.clone().or_else(|| idstr(t.get("titleId")));
        let Some(key) = key else { continue };
        let i = entry(&key, &mut out); let e = &mut out[i].1;
        if e.name.is_empty() { e.name = clean(sv(t, "name").filter(|n| !n.is_empty()).or_else(|| sv(&concept, "name"))); }
        e.concept_id = cid;
        if e.title_id.is_none() { e.title_id = idstr(t.get("titleId")); }
        e.playtime += parse_duration(sv(t, "playDuration").unwrap_or(""));
        if let Some(lp) = parse_time(sv(t, "lastPlayedDateTime").unwrap_or("")) { if lp != 0 && e.last_played.map_or(true, |x| lp > x) { e.last_played = Some(lp); } }
        e.plus = e.plus || sv(t, "service") == Some("ps_plus");
        if e.platform.is_empty() { e.platform = sv(t, "category").unwrap_or("").to_string(); }
        let tids: Vec<String> = match concept.get("titleIds").and_then(|a| a.as_array()).filter(|a| !a.is_empty()) { Some(a) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(), None => idstr(t.get("titleId")).into_iter().collect() };
        for tid in tids { by_title.insert(tid, key.clone()); }
    }
    for g in purchased {
        let cid = idstr(g.get("conceptId"));
        let key = cid.clone().or_else(|| idstr(g.get("titleId")).and_then(|t| by_title.get(&t).cloned())).or_else(|| idstr(g.get("titleId")));
        let Some(key) = key else { continue };
        let i = entry(&key, &mut out); let e = &mut out[i].1;
        let gname = clean(sv(g, "name"));
        if !gname.is_empty() && (sv(g, "platform") == Some("PS5") || !e.store_named) { e.name = gname; e.store_named = true; }
        if e.concept_id.is_none() { e.concept_id = cid; }
        if e.title_id.is_none() { e.title_id = idstr(g.get("titleId")); }
        let pid = idstr(g.get("productId"));
        if pid.is_some() && (sv(g, "platform") == Some("PS5") || e.product_id.is_none()) { e.product_id = pid; }
        e.plus = e.plus || sv(g, "membership") == Some("PS_PLUS");
        if let Some(p) = sv(g, "platform").filter(|p| !p.is_empty()) { e.platform = p.to_string(); }
    }
    out.into_iter().filter(|(_, e)| !e.name.is_empty()).collect()
}

// ---------------------------------------------------------------- the store page

/// `store.parse_page`: the useful parts of a concept page ({products, media}), or None when it holds no product.
pub fn parse_page(html: &str) -> Option<Value> {
    static E: OnceLock<Regex> = OnceLock::new();
    let docs: Vec<Value> = re(&E, r#"(?s)<script id="(?:env:[^"]+|__NEXT_DATA__)" type="application/json">(.*?)</script>"#).captures_iter(html).filter_map(|c| serde_json::from_str(&c[1]).ok()).collect();
    let mut products: Map<String, Value> = Map::new(); let mut media: Map<String, Value> = Map::new();
    fn walk<'a>(v: &'a Value, out: &mut Vec<&'a Map<String, Value>>) {
        match v { Value::Object(o) => { out.push(o); for x in o.values() { walk(x, out); } } Value::Array(a) => for x in a { walk(x, out); }, _ => {} }
    }
    for doc in &docs {
        let mut objs = vec![]; walk(doc, &mut objs);
        for o in objs {
            if let Some(pid) = o.get("id").and_then(|i| i.as_str()) {
                if o.get("storeDisplayClassification").map_or(false, truthy) || o.contains_key("publisherName") {
                    let p = products.entry(pid.to_string()).or_insert_with(|| Value::Object(Map::new())).as_object_mut().unwrap();
                    for (src, dst) in [("name", "name"), ("storeDisplayClassification", "cls"), ("topCategory", "top"), ("platforms", "platforms"), ("publisherName", "publisher"), ("releaseDate", "release"), ("descriptions", "descriptions"), ("localizedGenres", "genres"), ("starRating", "rating")] {
                        if let Some(v) = o.get(src).filter(|v| !v.is_null()) { if !p.contains_key(dst) { p.insert(dst.into(), v.clone()); } }
                    }
                    if let Some(m) = o.get("media").and_then(|m| m.as_array()) { if !p.contains_key("media") { p.insert("media".into(), Value::Array(m.iter().filter(|x| x.is_object()).cloned().collect())); } }
                }
            }
            if sv(&Value::Object(o.clone()), "__typename") == Some("Media") && o.get("role").map_or(false, truthy) && o.get("url").map_or(false, truthy) && o.get("type").and_then(|t| t.as_str()) == Some("IMAGE") {
                if let Some(role) = o.get("role").and_then(|r| r.as_str()) { if !media.contains_key(role) { media.insert(role.to_string(), o["url"].clone()); } }
            }
        }
    }
    if products.is_empty() { return None; }
    Some(json!({"products": Value::Object(products), "media": Value::Object(media)}))
}
fn truthy(v: &Value) -> bool { match v { Value::Null => false, Value::Bool(b) => *b, Value::String(s) => !s.is_empty(), Value::Number(n) => n.as_f64() != Some(0.0), Value::Array(a) => !a.is_empty(), Value::Object(o) => !o.is_empty() } }

/// `slug_for`: what is saved in `platform_slug` (the store page: `concept/<id>`, else `product/<id>`, else empty).
pub fn slug_for(concept_id: Option<&str>, product_id: Option<&str>) -> String {
    match (concept_id.filter(|c| !c.is_empty()), product_id.filter(|p| !p.is_empty())) { (Some(c), _) => format!("concept/{c}"), (None, Some(p)) => format!("product/{p}"), _ => String::new() }
}

/// `pick_product`: the record's product for an entitlement: the exact id, else the page's own full game, else the first.
pub fn pick_product<'a>(rec: Option<&'a Value>, product_id: Option<&str>) -> Option<&'a Value> {
    let prods = rec?.get("products")?.as_object()?;
    if let Some(id) = product_id.filter(|p| !p.is_empty()) { return prods.get(id); }   // a specific product we cannot find: say nothing rather than guess
    prods.values().find(|p| p.get("cls").and_then(|c| c.as_str()).map_or(false, |c| GAME_CLASSES.contains(&c))).or_else(|| prods.values().next())
}

/// `classify`: (is_game, reason). Only what is positively a demo, DLC or other add-on is excluded.
pub fn classify(name: &str, rec: Option<&Value>, product_id: Option<&str>, allow_demos: bool) -> (bool, String) {
    static N: OnceLock<Regex> = OnceLock::new();
    if !name.is_empty() && re(&N, r"(?i)\bdemo\b|\bplaytest\b|\b(?:open|closed)\s+beta\b|\bbeta\s+(?:version|test)\b|\btrial\s+(?:version|edition)\b|\(\s*(?:trial|beta)\s*\)").is_match(name) {
        return if allow_demos { (true, "demo allowed".into()) } else { (false, "name says demo, trial version or beta".into()) };
    }
    let Some(p) = pick_product(rec, product_id) else { return (true, "no store data".into()) };
    let (cls, top) = (sv(p, "cls"), sv(p, "top"));
    if allow_demos && (cls == Some("DEMO") || top == Some("DEMO")) { return (true, "demo allowed".into()); }
    if let Some(c) = cls { if NON_GAME_CLASSES.contains(&c) { return (false, format!("store says {c}")); } }
    if let Some(t) = top.filter(|t| !t.is_empty()) { if t != "GAME" { return (false, format!("store category {t}")); } }
    (true, cls.filter(|c| !c.is_empty()).unwrap_or("unclassified").to_string())
}

fn ts_midnight(iso: &str) -> Option<i64> { super::nintendo::release_ts(iso) }

/// `store.metadata`: genres, publisher and release date from the page (only what it has).
pub fn metadata(rec: Option<&Value>, product_id: Option<&str>) -> Update {
    let Some(p) = pick_product(rec, product_id).or_else(|| pick_product(rec, None)) else { return vec![] };
    let mut out: Update = vec![];
    let genres: Vec<&str> = p.get("genres").and_then(|g| g.as_array()).map(|a| a.iter().filter_map(|g| g.get("value").and_then(|v| v.as_str()).filter(|v| !v.is_empty())).collect()).unwrap_or_default();
    if !genres.is_empty() { out.push(("genres", Sql::Text(genres.join(",")))); }
    if let Some(pb) = sv(p, "publisher").filter(|s| !s.is_empty()) { out.push(("publishers", Sql::Text(pb.trim().to_string()))); }
    if let Some(ts) = sv(p, "release").and_then(ts_midnight) { out.push(("release_date", Sql::Integer(ts))); }
    out
}

/// `store.description`: the long text, else the short one, with its line breaks kept.
pub fn description(rec: Option<&Value>, product_id: Option<&str>) -> Option<String> {
    static B: OnceLock<Regex> = OnceLock::new();
    let p = pick_product(rec, product_id).or_else(|| pick_product(rec, None))?;
    let texts: HashMap<String, String> = p.get("descriptions").and_then(|d| d.as_array()).into_iter().flatten().filter_map(|d| Some((d.get("type")?.as_str()?.to_string(), d.get("value")?.as_str()?.to_string()))).collect();
    let text = texts.get("LONG").filter(|t| !t.is_empty()).or_else(|| texts.get("SHORT").filter(|t| !t.is_empty()))?;
    let text = re(&B, r"(?i)<\s*(?:br\s*/?|/p|/li)\s*>").replace_all(text, "\n").into_owned();
    Some(crate::steamweb::clean_description(&text)).filter(|d| !d.is_empty())
}

/// `store.art_urls`: the store's own art, each only where the store has it.
pub fn art_urls(rec: Option<&Value>, product_id: Option<&str>) -> Vec<(&'static str, String)> {
    let p = pick_product(rec, product_id).or_else(|| pick_product(rec, None));
    let mut roles: HashMap<String, String> = rec.and_then(|r| r.get("media")).and_then(|m| m.as_object()).map(|m| m.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).collect()).unwrap_or_default();
    for m in p.and_then(|p| p.get("media")).and_then(|m| m.as_array()).into_iter().flatten() {
        if let (Some(role), Some(url)) = (sv(m, "role"), sv(m, "url").filter(|u| !u.is_empty())) { if sv(m, "type") == Some("IMAGE") { roles.insert(role.to_string(), url.to_string()); } }
    }
    let mut out = vec![];
    if let Some(u) = roles.get("PORTRAIT_BANNER") { out.push(("vertical", u.clone())); }
    if let Some(u) = ["GAMEHUB_COVER_ART", "EDITION_KEY_ART", "BACKGROUND"].iter().find_map(|r| roles.get(*r)) { out.push(("horizontal", u.clone())); }
    if let Some(u) = roles.get("MASTER") { out.push(("icon", u.clone())); }
    out
}

// ---------------------------------------------------------------- the source

enum PsnErr { Auth(String), Other(String) }
impl PsnErr { fn text(self) -> String { match self { PsnErr::Auth(m) | PsnErr::Other(m) => m } } }

pub struct PlaystationSource { http: Arc<dyn Http>, config_dir: PathBuf, db: PathBuf, token: Mutex<(Option<String>, Option<Instant>)>, store_cache: Mutex<Option<Map<String, Value>>>, library: Mutex<Option<(Instant, Vec<(String, Entry)>)>> }

impl PlaystationSource {
    pub fn new(http: Arc<dyn Http>, config_dir: PathBuf, db: PathBuf) -> Self { PlaystationSource { http, config_dir, db, token: Mutex::new((None, None)), store_cache: Mutex::new(None), library: Mutex::new(None) } }
    fn secret(&self, k: &str) -> Option<String> { crate::account::plugin_secret_get(&self.config_dir, "playstation", k).filter(|v| !v.is_empty()) }
    fn set(&self, k: &str, v: Option<&str>) { let _ = crate::account::plugin_secret_set(&self.config_dir, "playstation", k, v); }

    fn form(&self, body: &[(&str, &str)]) -> Result<Value, PsnErr> {
        let b = body.iter().map(|(k, v)| format!("{}={}", enc(k), enc(v))).collect::<Vec<_>>().join("&");
        let r = self.http.post(&format!("{AUTH_BASE}/token"), &[("Authorization", BASIC), ("Content-Type", "application/x-www-form-urlencoded")], &b, Duration::from_secs(20)).map_err(|e| { log(&format!("PlayStation: {e}")); PsnErr::Other("Could not reach Sony.".into()) })?;
        let j: Value = serde_json::from_str(&r.body).unwrap_or(Value::Null);
        if r.status != 200 || sv(&j, "access_token").map_or(true, str::is_empty) { return Err(PsnErr::Auth(format!("Sony did not accept the login (HTTP {}).", r.status))); }
        Ok(j)
    }
    fn remember(&self, body: &Value) {
        let secs = body.get("expires_in").and_then(|e| e.as_i64().or_else(|| e.as_str()?.parse().ok())).filter(|e| *e != 0).unwrap_or(3600);
        *self.token.lock().unwrap() = (sv(body, "access_token").map(String::from), Some(Instant::now() + Duration::from_secs((secs - 60).max(0) as u64)));
        if let Some(rt) = sv(body, "refresh_token").filter(|r| !r.is_empty()) {
            self.set("refresh_token", Some(rt));
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
            let life = body.get("refresh_token_expires_in").and_then(|e| e.as_i64().or_else(|| e.as_str()?.parse().ok())).unwrap_or(0) as f64;
            self.set("refresh_expires", Some(&format!("{}", now + life)));
        }
    }
    fn access_token(&self) -> Result<String, PsnErr> {
        { let g = self.token.lock().unwrap(); if let (Some(t), Some(until)) = (&g.0, g.1) { if Instant::now() < until { return Ok(t.clone()); } } }
        let refresh = self.secret("refresh_token").ok_or_else(|| PsnErr::Auth("Not connected.".into()))?;
        let body = self.form(&[("refresh_token", &refresh), ("grant_type", "refresh_token"), ("token_format", "jwt"), ("scope", SCOPE)])?;
        self.remember(&body);
        Ok(sv(&body, "access_token").unwrap_or("").to_string())
    }
    /// `check_connection`: Some(true) the saved login works, Some(false) it does not, None Sony could not be reached.
    pub fn check_connection(&self) -> Option<bool> {
        self.secret("refresh_token")?;
        match self.access_token() { Ok(_) => Some(true), Err(PsnErr::Auth(_)) => Some(false), Err(PsnErr::Other(_)) => None }
    }
    fn api_get(&self, url: &str) -> Result<crate::net::Response, PsnErr> {
        let tok = self.access_token()?;
        let r = self.http.get(url, &[("Authorization", &format!("Bearer {tok}")), ("Content-Type", "application/json"), ("User-Agent", UA)], Duration::from_secs(30)).map_err(|e| { log(&format!("PlayStation: {e}")); PsnErr::Other("Could not reach Sony.".into()) })?;
        if r.status == 401 || r.status == 403 { *self.token.lock().unwrap() = (None, None); return Err(PsnErr::Auth(format!("Sony rejected the saved login (HTTP {}).", r.status))); }
        Ok(r)
    }
    fn online_id(&self) -> Option<String> {
        let r = self.api_get(&format!("{PROFILE}?fields=onlineId")).ok()?;
        serde_json::from_str::<Value>(&r.body).ok()?.pointer("/profile/onlineId")?.as_str().map(String::from)
    }
    fn fetch_purchased(&self) -> Result<Vec<Value>, PsnErr> {
        let (mut games, mut start) = (vec![], 0usize);
        for _ in 0..100 {
            let vars = json!({"isActive": true, "platform": ["ps4", "ps5"], "size": 100, "start": start, "sortBy": "ACTIVE_DATE", "sortDirection": "desc"}).to_string();
            let ext = json!({"persistedQuery": {"version": 1, "sha256Hash": PURCHASED_HASH}}).to_string();
            let r = self.api_get(&format!("{GRAPHQL}?operationName=getPurchasedGameList&variables={}&extensions={}", enc(&vars), enc(&ext)))?;
            let body: Value = serde_json::from_str(&r.body).unwrap_or(Value::Null);
            let Some(batch) = body.pointer("/data/purchasedTitlesRetrieve/games").and_then(|g| g.as_array()).filter(|_| body.pointer("/data/purchasedTitlesRetrieve").map_or(false, |b| b.is_object())) else {
                // the key names only, never the content
                log(&format!("PlayStation: purchased list answered HTTP {}, shape {}", r.status, body.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>().join(",")).unwrap_or_else(|| "not an object".into())));
                return Err(PsnErr::Other("Sony did not return the purchased games list.".into()));
            };
            if batch.is_empty() { break; }
            start += batch.len(); games.extend(batch.iter().cloned());
        }
        Ok(games)
    }
    fn fetch_played(&self) -> Result<Vec<Value>, PsnErr> {
        let (mut titles, mut offset) = (vec![], 0usize);
        for _ in 0..100 {
            let r = self.api_get(&format!("{GAMELIST}?limit=200&offset={offset}"))?;
            let body: Value = serde_json::from_str(&r.body).unwrap_or(Value::Null);
            let Some(batch) = body.get("titles").and_then(|t| t.as_array()).filter(|_| r.status == 200) else { log(&format!("PlayStation: played list answered HTTP {}", r.status)); return Err(PsnErr::Other("Sony did not return the played games list.".into())) };
            if batch.is_empty() { break; }
            offset += batch.len(); titles.extend(batch.iter().cloned());
            if let Some(total) = body.get("totalItemCount").and_then(|t| t.as_u64()) { if offset as u64 >= total { break; } }
        }
        Ok(titles)
    }

    // ---- the store page cache (`playstation_store.json`, the file PlayDate uses)
    fn store_path(&self) -> PathBuf { self.config_dir.join("playstation_store.json") }
    fn store_get(&self, key: &str, url_for: &str, force: bool) -> Option<Value> {
        let mut g = self.store_cache.lock().unwrap();
        let cache = g.get_or_insert_with(|| std::fs::read_to_string(self.store_path()).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v.as_object().cloned()).unwrap_or_default());
        let cached = cache.get(key).cloned();
        if cached.is_some() && !force { return cached; }
        drop(g);
        let rec = self.fetch_store(url_for);
        let mut g = self.store_cache.lock().unwrap(); let cache = g.as_mut().unwrap();
        match rec {
            None => cached,
            Some(mut r) => {
                r["fetched"] = json!(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0));
                cache.insert(key.to_string(), r.clone());
                let tmp = self.store_path().with_extension("json.tmp");
                if std::fs::write(&tmp, Value::Object(cache.clone()).to_string()).is_ok() { let _ = std::fs::rename(&tmp, self.store_path()); }
                std::thread::sleep(Duration::from_millis(if cfg!(test) { 0 } else { 250 }));   // be gentle with a page this size
                Some(r)
            }
        }
    }
    fn fetch_store(&self, url: &str) -> Option<Value> {
        match self.http.get(url, &[("User-Agent", STORE_UA), ("Accept-Language", "en-US")], Duration::from_secs(30)) {
            Ok(r) if r.status == 200 => { let p = parse_page(&r.body); if p.is_none() { log(&format!("PlayStation store: a page had no product data")); } p }
            Ok(r) => { log(&format!("PlayStation store: a page answered HTTP {}", r.status)); None }
            Err(e) => { log(&format!("PlayStation store: not reachable ({e})")); None }
        }
    }
    /// `rec_for_slug`: the cached or fresh record a saved slug points at (a bare number is an older-style concept id).
    pub fn rec_for_slug(&self, slug: &str, force: bool) -> Option<Value> {
        let slug = slug.trim(); if slug.is_empty() { return None; }
        let (kind, ident) = match slug.split_once('/') { Some((k, i)) => (k, i), None => ("concept", slug) };
        if ident.is_empty() { return None; }
        if kind == "product" { self.store_get(&format!("p:{ident}"), &format!("https://store.playstation.com/en-us/product/{ident}"), force) } else { self.store_get(ident, &format!("https://store.playstation.com/en-us/concept/{ident}"), force) }
    }

    fn library(&self) -> Result<Vec<(String, Entry)>, PsnErr> {
        if let Some((at, g)) = self.library.lock().unwrap().as_ref() { if at.elapsed() < Duration::from_secs(90) { return Ok(g.clone()); } }   // a bulk re-scrape reads the account once
        let purchased = match self.fetch_purchased() { Err(PsnErr::Auth(m)) => return Err(PsnErr::Auth(m)), r => r.ok() };
        let played = match self.fetch_played() { Err(PsnErr::Auth(m)) => return Err(PsnErr::Auth(m)), r => r.ok() };
        if purchased.is_none() && played.is_none() { return Err(PsnErr::Other("Sony could not be read.".into())); }
        let g = merge(&purchased.unwrap_or_default(), &played.unwrap_or_default());
        *self.library.lock().unwrap() = Some((Instant::now(), g.clone()));
        Ok(g)
    }
    fn ids(&self) -> (HashSet<String>, HashSet<String>) {
        let Ok(c) = rusqlite::Connection::open_with_flags(&self.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { return Default::default() };
        let col = |sql: &str| -> HashSet<String> { c.prepare(sql).ok().map(|mut s| s.query_map([], |r| r.get::<_, Option<String>>(0)).map(|it| it.flatten().flatten().collect()).unwrap_or_default()).unwrap_or_default() };
        (col("SELECT platform_id FROM games WHERE platform = 'playstation'"), col("SELECT platform_id FROM blacklist WHERE platform_id IS NOT NULL"))
    }
}

impl LibrarySource for PlaystationSource {
    fn id(&self) -> &str { "playstation" }
    fn label(&self) -> &str { "PlayStation" }
    fn apply_purchase_dates(&self, online_id: &str, purchases: &[PurchaseIn]) -> Result<(usize, usize, usize), String> { self.apply_dates(online_id, purchases) }
    fn scan_junk(&self) -> Result<Vec<super::JunkHit>, String> {
        let rows: Vec<(i64, String, String)> = {
            let Ok(c) = rusqlite::Connection::open_with_flags(&self.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { return Ok(vec![]) };
            let mut st = c.prepare("SELECT appid, COALESCE(name,''), COALESCE(platform_slug,'') FROM games WHERE platform = 'playstation' AND COALESCE(playtime_forever, 0) = 0 AND COALESCE(installed, 0) = 0 AND COALESCE(completion_status, 'Never Played') = 'Never Played'").map_err(|e| e.to_string())?;
            let v: Vec<_> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map_err(|e| e.to_string())?.flatten().collect();
            v
        };
        let mut out = Vec::new();
        for (appid, name, slug) in rows {
            let rec = self.rec_for_slug(&slug, false);
            let (ok, why) = classify(&name, rec.as_ref(), None, false);
            if !ok { out.push(super::JunkHit { appid, name, reason: why }); }
        }
        Ok(out)
    }
    fn caps(&self) -> Caps { Caps { auth: true, sync: true, rescrape: true, description: true, store_page_metadata: true, store_page: true, purchase_dates: true, ..Default::default() } }
    fn store_url(&self, g: &GameRef) -> Option<String> { (!g.slug.is_empty()).then(|| format!("https://store.playstation.com/en-us/{}", g.slug)) }

    fn auth_status(&self) -> Result<AuthStatus, String> {
        if self.secret("refresh_token").is_none() { return Ok(AuthStatus { connected: false, username: None }); }
        if self.check_connection() == Some(false) { return Ok(AuthStatus { connected: false, username: None }); }
        Ok(AuthStatus { connected: true, username: Some(self.secret("online_id").unwrap_or_else(|| "PlayStation Account".into())) })
    }
    /// The sign-in window opens PlayStation's sign-in; Sony's session cookie (`npsso`) is what the paste flow copied from the ssocookie page, so it is read straight from the window.
    // No sign-in window (tried and removed, 2026-11-22): Sony's sign-in page runs Akamai bot detection. In the embedded browser its script fails ("null is not an object") and the page's own
    // login check (`/api/v1/ssocookie`) is answered 429, while the same address answers 400 to a plain request, so it is the browser being refused, not a rate limit. The page then says
    // "Something went wrong". Tried: Sony's authorize address, the account site, an automatic Sign In click, a Chrome user agent, WebGL and hardware drawing, camera requests allowed.
    // Signing in goes through the browser: SIGN IN WITH BROWSER, or pasting the ssocookie text.
    fn auth_begin(&self) -> Result<AuthBegin, String> {
        Ok(AuthBegin { url: "https://ca.account.sony.com/api/v1/ssocookie".into(), placeholder: "Paste the text of that page here".into(), instructions: vec![
            "Sign in to PlayStation in your browser, then open the page that opens now (it shows a short line of text starting with {\"npsso\":).".into(),
            "Copy all of that text and paste it below. Only the refresh token Sony gives back is kept; the NPSSO itself is not stored.".into()] })
    }
    fn auth_complete(&self, value: &str) -> Result<AuthStatus, String> {
        let Some(npsso) = parse_npsso(value) else {
            return Err(if looks_like_address(value) { "That is the address of the page, not what it shows. Open the page while signed in and paste the text on it (it starts with {\"npsso\":).".into() } else { "That does not look like an NPSSO. It is a 64 character code.".into() });
        };
        let url = format!("{AUTH_BASE}/authorize?access_type=offline&client_id={CLIENT_ID}&redirect_uri={}&response_type=code&scope={}", enc(REDIRECT), enc(SCOPE));
        let r = self.http.get_raw(&url, &[("Cookie", &format!("npsso={npsso}")), ("User-Agent", UA)], Duration::from_secs(20)).map_err(|e| { log(&format!("PlayStation: connect failed: {e}")); "Could not reach Sony.".to_string() })?;
        static C: OnceLock<Regex> = OnceLock::new();
        let loc = r.header_all("location").first().map(|s| s.to_string()).unwrap_or_default();
        let Some(code) = re(&C, r"[?&]code=([^&]+)").captures(&loc).map(|m| m[1].to_string()) else { return Err("Sony did not accept that NPSSO. Sign in again and copy a fresh one.".into()) };
        let body = self.form(&[("code", &code), ("redirect_uri", REDIRECT), ("grant_type", "authorization_code"), ("token_format", "jwt")]).map_err(PsnErr::text)?;
        self.remember(&body);
        // the purchase history is only served to the store website's session: open one now, while the NPSSO is at hand (kept in memory, never saved)
        match web_session(self.http.as_ref(), &npsso) {
            Ok(c) => { *WEB_SESSION.lock().unwrap() = Some(c); log("PlayStation: store website session open (purchase dates can be read until Zest quits)"); }
            Err(e) => log(&format!("PlayStation: no store website session, so no purchase dates: {e}")),
        }
        let name = self.online_id();
        if let Some(n) = &name { self.set("online_id", Some(n)); }
        log("PlayStation: connected");
        Ok(AuthStatus { connected: true, username: Some(name.unwrap_or_else(|| "PlayStation Account".into())) })
    }
    fn auth_disconnect(&self) -> Result<(), String> {
        for k in ["refresh_token", "refresh_expires", "online_id"] { self.set(k, None); }
        *self.token.lock().unwrap() = (None, None); *self.library.lock().unwrap() = None; Ok(())
    }

    fn sync(&self, _cx: &SourceCtx, sink: &mut dyn FnMut(Vec<GameRecord>), cancel: &Cancel) -> Result<usize, String> {
        if self.secret("refresh_token").is_none() { return Err("Not connected".into()); }
        let mut notes = vec![];
        crate::net::set_activity("PlayStation: reading your purchased games");
        let purchased = match self.fetch_purchased() { Ok(p) => p, Err(PsnErr::Auth(_)) => return Err(EXPIRED.into()), Err(PsnErr::Other(m)) => { notes.push(m); vec![] } };
        crate::net::set_activity("PlayStation: reading your play history");
        let played = match self.fetch_played() { Ok(p) => p, Err(PsnErr::Auth(_)) => return Err(EXPIRED.into()), Err(PsnErr::Other(m)) => { notes.push(m); vec![] } };
        if purchased.is_empty() && played.is_empty() { return Err(if notes.is_empty() { "No games were returned for this account.".into() } else { notes.join(" ") }); }
        let games = merge(&purchased, &played);
        let (existing, blacklisted) = self.ids();
        let demos_ok = self.config_dir.join("playstation_allow_demos.flag").exists();
        let pid_of = |e: &Entry| e.concept_id.clone().or_else(|| e.title_id.clone()).unwrap_or_default();
        let fresh: Vec<&(String, Entry)> = games.iter().filter(|(_, e)| { let p = pid_of(e); !existing.contains(&p) && !blacklisted.contains(&p) }).collect();
        let mut keep: HashSet<String> = HashSet::new();
        for (i, (key, e)) in fresh.iter().enumerate() {
            if cancel.is_set() { return Ok(0); }
            crate::net::set_activity(&format!("PlayStation: reading store pages ({}/{})", i + 1, fresh.len()));
            let rec = self.rec_for_slug(&slug_for(e.concept_id.as_deref(), e.product_id.as_deref()), false);
            let (ok, why) = classify(&e.name, rec.as_ref(), e.product_id.as_deref(), demos_ok);
            if ok { keep.insert(key.clone()); } else { log(&format!("PlayStation: skipped {:?} ({why})", e.name)); }
        }
        let mut batch = vec![];
        for (key, e) in &games {
            let p = pid_of(e);
            if p.is_empty() || blacklisted.contains(&p) { continue; }
            if !existing.contains(&p) && !keep.contains(key) { continue; }
            let slug = Some(slug_for(e.concept_id.as_deref(), e.product_id.as_deref())).filter(|s| !s.is_empty());
            batch.push(GameRecord { platform_id: p, name: e.name.clone(), slug, appname: e.title_id.clone(), playtime_forever: Some(e.playtime).filter(|t| *t > 0), last_played: e.last_played, ..Default::default() });
        }
        crate::net::set_activity("");
        let n = batch.len(); sink(batch);
        if let Some(cookie) = web_cookie() {
            if !cancel.is_set() {
                crate::net::set_activity("PlayStation: reading your purchase history");
                match self.dates_from_history(&cookie, None) {
                    Ok(set) => { let m = format!("{} purchase date{} set", set.len(), if set.len() == 1 { "" } else { "s" }); log(&format!("PlayStation sync: {m}")); crate::net::set_note(&m); }
                    Err(e) => { log(&format!("PlayStation sync: purchase dates: {e}")); notes.push(e); }
                }
                crate::net::set_activity("");
            }
        }
        if !notes.is_empty() { log(&format!("PlayStation sync: {}", notes.join(" "))); }
        Ok(n)
    }

    /// Bulk Ops > Purchase dates: read with the store website's session opened at sign-in.
    fn purchase_dates(&self, _cx: &SourceCtx, appids: &[i64]) -> Result<Vec<(i64, Option<i64>)>, String> {
        let cookie = web_cookie().ok_or(NO_WEB_SESSION)?;
        let set: HashMap<i64, i64> = self.dates_from_history(&cookie, Some(appids))?.into_iter().collect();
        Ok(appids.iter().map(|a| (*a, set.get(a).copied())).collect())
    }

    /// `sync_game`: the account's title, playtime and last played, and the store page's publisher, release date, description. Genres only for a game with no Steam match.
    fn rescrape(&self, cx: &SourceCtx, g: &GameRef) -> Fetch<Option<Update>> {
        let (steam_appid, slug): (Option<i64>, String) = rusqlite::Connection::open_with_flags(&cx.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()
            .and_then(|c| c.query_row("SELECT steam_appid, COALESCE(platform_slug,'') FROM games WHERE appid = ?", [g.appid], |r| Ok((r.get(0)?, r.get(1)?))).ok()).unwrap_or((None, g.slug.clone()));
        let mut out: Update = vec![];
        let mut product_id = None;
        match self.library() {
            Ok(lib) => if let Some((_, e)) = lib.iter().find(|(_, e)| e.concept_id.clone().or_else(|| e.title_id.clone()).as_deref() == Some(g.platform_id.as_str())) {
                product_id = e.product_id.clone();
                if !e.name.is_empty() && e.name != g.name { out.push(("name", Sql::Text(e.name.clone()))); }
                if e.playtime > 0 { out.push(("playtime_forever", Sql::Integer(e.playtime))); }
                if let Some(lp) = e.last_played { out.push(("last_played", Sql::Integer(lp))); }
            },
            Err(PsnErr::Auth(_)) => log("PlayStation: the saved login no longer works"),
            Err(PsnErr::Other(m)) => log(&format!("PlayStation: {m}")),
        }
        if let Some(rec) = self.rec_for_slug(&slug, false) {
            for (c, v) in metadata(Some(&rec), product_id.as_deref()) { if steam_appid.is_some() && c == "genres" { continue; } out.retain(|(k, _)| *k != c); out.push((c, v)); }
            if let Some(d) = description(Some(&rec), product_id.as_deref()) { out.push(("short_description", Sql::Text(d))); }
        }
        Ok((!out.is_empty()).then_some(out))
    }
    fn description(&self, _cx: &SourceCtx, g: &GameRef) -> Option<String> { description(self.rec_for_slug(&g.slug, false).as_ref(), None) }
    fn store_page_metadata(&self, slug: &str) -> PageMeta {
        let mut m = PageMeta::default();
        for (c, v) in metadata(self.rec_for_slug(slug, false).as_ref(), None) { match (c, v) { ("genres", Sql::Text(t)) => m.genres = Some(t), ("publishers", Sql::Text(t)) => m.publishers = Some(t), ("release_date", Sql::Integer(i)) => m.release_date = Some(i), _ => {} } }
        m
    }
}

#[cfg(test)]
mod behaviour {
    use super::*;
    use crate::net::{Cancel, NetError, Raw, Response};

    const NPSSO: &str = "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ01";

    /// A fake Sony and PlayStation store. Records the token requests and every URL asked.
    struct Site { good_npsso: bool, token_ok: bool, refresh_ok: bool, token_posts: Mutex<Vec<String>>, asked: Mutex<Vec<String>>, api_status: u16 }
    fn decode(s: &str) -> String {
        let b = s.as_bytes(); let mut out = vec![]; let mut i = 0;
        while i < b.len() { if b[i] == b'%' && i + 2 < b.len() + 0 && i + 2 <= b.len() - 1 { out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap_or(b'?')); i += 3; } else { out.push(b[i]); i += 1; } }
        String::from_utf8_lossy(&out).into_owned()
    }
    fn page(products: Value) -> String { format!(r#"<html><script id="__NEXT_DATA__" type="application/json">{}</script></html>"#, json!({"props": {"products": products}})) }
    fn product(id: &str, cls: &str) -> Value { json!({"id": id, "name": "n", "storeDisplayClassification": cls, "topCategory": "GAME", "publisherName": "Pub Co", "releaseDate": "2020-05-01T00:00:00Z", "localizedGenres": [{"value": "Action"}, {"value": "RPG"}], "descriptions": [{"type": "LONG", "value": "Hello<br>World"}]}) }
    impl Http for Site {
        fn get_raw(&self, u: &str, h: &[(&str, &str)], _t: Duration) -> Result<Raw, NetError> {
            self.asked.lock().unwrap().push(u.to_string());
            let cookie_ok = self.good_npsso && h.iter().any(|(k, v)| *k == "Cookie" && *v == format!("npsso={NPSSO}"));
            Ok(if cookie_ok { Raw { status: 302, headers: vec![("Location".into(), format!("{REDIRECT}/?code=THECODE&cid=1"))], body: String::new() } } else { Raw { status: 200, headers: vec![], body: "login".into() } })
        }
        fn post(&self, u: &str, h: &[(&str, &str)], body: &str, _t: Duration) -> Result<Response, NetError> {
            assert!(u.ends_with("/oauth/token") && h.iter().any(|(k, v)| *k == "Authorization" && *v == BASIC));
            self.token_posts.lock().unwrap().push(body.to_string());
            let ok = if body.contains("grant_type=authorization_code") { self.token_ok && body.contains("code=THECODE") } else { self.refresh_ok && body.contains("refresh_token=RT1") };
            let tok = |s: u16, b: Value| Ok(Response { status: s, retry_after: None, body: b.to_string() });
            if ok { tok(200, json!({"access_token": "AT", "refresh_token": "RT1", "expires_in": 3600, "refresh_token_expires_in": 5184000})) } else { tok(400, json!({"error": "invalid_grant"})) }
        }
        fn get(&self, u: &str, h: &[(&str, &str)], _t: Duration) -> Result<Response, NetError> {
            self.asked.lock().unwrap().push(u.to_string());
            let r = |s: u16, b: String| Ok(Response { status: s, retry_after: None, body: b });
            if u.contains("store.playstation.com") {
                return match u.rsplit('/').next().unwrap() {
                    "C1" => r(200, page(json!([product("P1", "FULL_GAME")]))),
                    "C2" => r(200, page(json!([product("P2", "ADD_ON")]))),
                    _ => r(404, "nope".into()),
                };
            }
            assert!(h.iter().any(|(k, v)| *k == "Authorization" && *v == "Bearer AT"), "api calls carry the access token");
            if self.api_status != 200 { return r(self.api_status, "{}".into()); }
            if u.starts_with(PROFILE) { return r(200, json!({"profile": {"onlineId": "PlayerOne"}}).to_string()); }
            if u.starts_with(GRAPHQL) {
                let q = decode(u);
                let g = |c: &str, t: &str, n: &str, p: &str, pl: &str| json!({"conceptId": c, "titleId": t, "name": n, "productId": p, "platform": pl});
                let games = if q.contains("\"start\":0") { vec![g("C1", "PPSA1_00", "Game One\u{2122}", "P1", "PS5"), g("C2", "PPSA2_00", "Game Two Add-On", "P2", "PS5")] } else if q.contains("\"start\":2") { vec![g("C3", "PPSA3_00", "Removed Game", "P3", "PS5"), g("C4", "PPSA4_00", "Game Four (Demo)", "P4", "PS5"), g("C5", "PPSA5_00", "Game Five", "P5", "PS5")] } else { vec![] };
                return r(200, json!({"data": {"purchasedTitlesRetrieve": {"games": games}}}).to_string());
            }
            if u.starts_with(GAMELIST) {
                let t = json!({"titleId": "PPSA1_00", "concept": {"id": "C1", "titleIds": ["PPSA1_00"]}, "name": "Game One", "playDuration": "PT2H30M", "lastPlayedDateTime": "2024-01-02T03:04:05Z", "category": "ps5_native_game"});
                return r(200, json!({"titles": if u.contains("offset=0") { vec![t] } else { vec![] }, "totalItemCount": 1}).to_string());
            }
            r(404, "{}".into())
        }
    }
    fn site() -> Site { Site { good_npsso: true, token_ok: true, refresh_ok: true, token_posts: Mutex::new(vec![]), asked: Mutex::new(vec![]), api_status: 200 } }
    fn setup(tag: &str, site: Site, refresh: bool) -> (PlaystationSource, Arc<Site>, PathBuf) {
        let d = std::env::temp_dir().join(format!("zest_psn_{tag}_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("config.json"), "{}").unwrap();
        if refresh { crate::account::plugin_secret_set(&d, "playstation", "refresh_token", Some("RT1")).unwrap(); }
        let db = d.join("games_1.db");
        rusqlite::Connection::open(&db).unwrap().execute_batch("CREATE TABLE games (appid INTEGER PRIMARY KEY, name TEXT, platform TEXT, platform_id TEXT, platform_slug TEXT, steam_appid INTEGER); CREATE TABLE blacklist (appid INTEGER, platform_id TEXT, platform TEXT);").unwrap();
        let s = Arc::new(site); (PlaystationSource::new(s.clone(), d.clone(), db), s, d)
    }
    fn cx(d: &PathBuf, s: Arc<Site>) -> SourceCtx { let h: Arc<dyn Http> = s; SourceCtx { http: h.clone(), steam: Arc::new(crate::steamclient::Steam::new(h, None, 10.0)), root: d.clone(), db: d.join("games_1.db"), today: "2026-11-01".into(), appinfo: Arc::new(|_| Default::default()), account_id: None } }
    fn config_text(d: &PathBuf) -> String { std::fs::read_to_string(d.join("config.json")).unwrap() }

    #[test] fn signing_in_exchanges_the_npsso_and_keeps_only_the_refresh_token() {
        let (s, site, d) = setup("signin", site(), false);
        assert_eq!(s.auth_status().unwrap(), AuthStatus { connected: false, username: None });
        let st = s.auth_complete(&format!(r#"{{"npsso":"{NPSSO}"}}"#)).unwrap();
        assert_eq!(st, AuthStatus { connected: true, username: Some("PlayerOne".into()) });
        let cfg = config_text(&d);
        assert!(cfg.contains("RT1") && cfg.contains("PlayerOne") && cfg.contains("refresh_expires"));
        assert!(!cfg.contains(NPSSO), "the NPSSO is never stored");
        assert_eq!(site.token_posts.lock().unwrap().len(), 1);
        assert_eq!(s.auth_status().unwrap().username.as_deref(), Some("PlayerOne"));
        s.auth_disconnect().unwrap();
        assert!(!config_text(&d).contains("RT1"));
        assert_eq!(s.auth_status().unwrap(), AuthStatus { connected: false, username: None });
    }
    #[test] fn a_rejected_login_saves_nothing_and_says_why() {
        let (s, _, d) = setup("reject1", Site { good_npsso: false, ..site() }, false);
        assert!(s.auth_complete(NPSSO).unwrap_err().contains("did not accept that NPSSO"));
        assert!(!config_text(&d).contains("refresh_token"));
        let (s, _, d) = setup("reject2", Site { token_ok: false, ..site() }, false);
        assert!(s.auth_complete(NPSSO).unwrap_err().contains("HTTP 400"));
        assert!(!config_text(&d).contains("refresh_token"));
        assert!(s.auth_complete("short").unwrap_err().contains("does not look like an NPSSO"));
        assert!(s.auth_complete("https://ca.account.sony.com/api/v1/ssocookie").unwrap_err().contains("address of the page"));
    }
    #[test] fn the_access_token_is_refreshed_once_and_reused() {
        let (s, site, _d) = setup("refresh", site(), true);
        assert_eq!(s.check_connection(), Some(true));
        assert!(s.online_id().is_some() && s.online_id().is_some());
        let posts = site.token_posts.lock().unwrap();
        assert_eq!(posts.len(), 1, "one refresh serves later calls");
        assert!(posts[0].contains("grant_type=refresh_token") && posts[0].contains("refresh_token=RT1"));
    }
    #[test] fn a_dead_refresh_token_shows_as_signed_out_and_sync_says_to_reconnect() {
        let (s, _, d) = setup("dead", Site { refresh_ok: false, ..site() }, true);
        assert_eq!(s.check_connection(), Some(false));
        assert_eq!(s.auth_status().unwrap(), AuthStatus { connected: false, username: None });
        let e = s.sync(&cx(&d, Arc::new(site())), &mut |_| {}, &Cancel::new()).unwrap_err();
        assert!(e.contains("expired"), "{e}");
        let (s, _, d) = setup("notconn", site(), false);
        assert_eq!(s.sync(&cx(&d, Arc::new(site())), &mut |_| {}, &Cancel::new()).unwrap_err(), "Not connected");
    }
    #[test] fn a_rejected_api_call_ends_a_sync_with_the_reconnect_message() {
        let (s, _, d) = setup("api403", Site { api_status: 403, ..site() }, true);
        assert!(s.sync(&cx(&d, Arc::new(site())), &mut |_| {}, &Cancel::new()).unwrap_err().contains("expired"));
    }
    #[test] fn sync_pages_both_lists_skips_demos_dlc_and_removed_games_and_keeps_known_ones() {
        let (s, site, d) = setup("sync", site(), true);
        let c = rusqlite::Connection::open(d.join("games_1.db")).unwrap();
        c.execute("INSERT INTO blacklist VALUES (-9, 'C3', 'playstation')", []).unwrap();
        c.execute("INSERT INTO games (appid, name, platform, platform_id) VALUES (-1, 'Game Two Add-On', 'playstation', 'C2')", []).unwrap();   // already in the library: stays even though the store calls it an add-on
        let mut got = vec![]; let n = s.sync(&cx(&d, site.clone()), &mut |b| got.extend(b), &Cancel::new()).unwrap();
        let ids: Vec<&str> = got.iter().map(|r| r.platform_id.as_str()).collect();
        assert_eq!(ids, ["C1", "C2", "C5"], "C3 is blacklisted, C4's name says demo; C5 has no store page so it is kept");
        assert_eq!(n, 3);
        let one = &got[0];
        assert_eq!((one.name.as_str(), one.slug.as_deref(), one.appname.as_deref(), one.playtime_forever, one.last_played), ("Game One", Some("concept/C1"), Some("PPSA1_00"), Some(150), Some(1704164645)));
        assert_eq!(got[2].playtime_forever, None, "no play history means no playtime");
        let paged = site.asked.lock().unwrap().iter().filter(|u| u.starts_with(GRAPHQL)).count();
        assert_eq!(paged, 3, "two batches and the empty one that ends the walk");
        assert!(d.join("playstation_store.json").exists(), "store pages are cached");
    }
    #[test] fn demos_are_kept_when_the_user_allows_them_and_a_cancelled_sync_adds_nothing() {
        let (s, site, d) = setup("demos", site(), true);
        std::fs::write(d.join("playstation_allow_demos.flag"), "").unwrap();
        let mut got = vec![]; s.sync(&cx(&d, site.clone()), &mut |b| got.extend(b), &Cancel::new()).unwrap();
        assert!(got.iter().any(|r| r.platform_id == "C4"));
        let (s, site, d) = setup("cancel", self::site(), true);
        let cancel = Cancel::new(); cancel.cancel();
        let mut got = vec![]; assert_eq!(s.sync(&cx(&d, site), &mut |b| got.extend(b), &cancel).unwrap(), 0); assert!(got.is_empty());
    }
    #[test] fn rescrape_reads_the_account_and_the_store_page() {
        let (s, site, d) = setup("rescrape", site(), true);
        let c = rusqlite::Connection::open(d.join("games_1.db")).unwrap();
        c.execute("INSERT INTO games (appid, name, platform, platform_id, platform_slug) VALUES (-1, 'Old Name', 'playstation', 'C1', 'concept/C1')", []).unwrap();
        let g = GameRef { appid: -1, name: "Old Name".into(), platform: "playstation".into(), platform_id: "C1".into(), slug: "concept/C1".into(), ..Default::default() };
        let upd = s.rescrape(&cx(&d, site.clone()), &g).unwrap().unwrap();
        let get = |k: &str| upd.iter().find(|(c, _)| *c == k).map(|(_, v)| v.clone());
        assert_eq!(get("name"), Some(Sql::Text("Game One".into())));
        assert_eq!(get("playtime_forever"), Some(Sql::Integer(150)));
        assert_eq!(get("genres"), Some(Sql::Text("Action,RPG".into())));
        assert_eq!(get("publishers"), Some(Sql::Text("Pub Co".into())));
        assert_eq!(get("short_description"), Some(Sql::Text("Hello\nWorld".into())));
        // a game with a Steam match keeps Steam's genres
        c.execute("UPDATE games SET steam_appid = 10 WHERE appid = -1", []).unwrap();
        let upd = s.rescrape(&cx(&d, site.clone()), &g).unwrap().unwrap();
        assert!(!upd.iter().any(|(k, _)| *k == "genres"));
    }
}

// ---------------------------------------------------------------- purchase dates (read by the Companion userscript in the user's browser)

/// A purchase the browser read: ISO time, store name, sku.
pub type PurchaseIn = (String, String, String);
/// A library row as the matcher sees it: appid, name, `platform_slug`, `platform_appname`, `date_added`.
pub type DateRow = (i64, String, String, String, Option<i64>);

/// `_title_id_of`: `UP4040-PPSA30074_00-DEMOBLADESFOFIRE-U001` -> `PPSA30074_00`.
pub fn title_id_of(sku: &str) -> Option<String> {
    static T: OnceLock<Regex> = OnceLock::new();
    let parts: Vec<&str> = sku.split('-').collect();
    (parts.len() >= 3 && re(&T, r"^[A-Z]{4}\d{5}_\d{2}$").is_match(parts[1])).then(|| parts[1].to_string())
}
/// `_product_of`: `UP4040-PPSA30074_00-DEMOBLADESFOFIRE-U001` -> `UP4040-PPSA30074_00-DEMOBLADESFOFIRE`.
pub fn product_of(sku: &str) -> Option<String> {
    if sku.matches('-').count() >= 3 { sku.rsplit_once('-').map(|(a, _)| a.to_string()) } else { Some(sku.to_string()).filter(|s| !s.is_empty()) }
}

/// `apply_purchase_dates`' matching: the earliest purchase of each game, found by its title id (inside the sku), its store product id, or exactly the same title.
/// Returns (purchases with a readable time, games matched, the dates to set). A date only ever moves earlier: Zest's own is the day the game was first seen.
pub fn match_purchase_dates(purchases: &[PurchaseIn], rows: &[DateRow]) -> (usize, usize, Vec<(i64, i64)>) {
    let (mut by_title, mut by_product, mut by_name): (HashMap<String, i64>, HashMap<String, i64>, HashMap<String, i64>) = Default::default();
    let mut usable = 0;
    for (date, name, sku) in purchases {
        let Some(ts) = parse_time(date) else { continue };
        usable += 1;
        let sku = sku.trim();
        let n = clean(Some(name)).to_lowercase();
        for (table, key) in [(&mut by_title, title_id_of(sku)), (&mut by_product, product_of(sku)), (&mut by_name, Some(n).filter(|n| !n.is_empty()))] {
            if let Some(k) = key { let e = table.entry(k).or_insert(ts); if ts < *e { *e = ts; } }
        }
    }
    let (mut matched, mut set) = (0, vec![]);
    for (appid, name, slug, appname, added) in rows {
        let times: Vec<i64> = [by_title.get(appname).copied(), slug.strip_prefix("product/").and_then(|p| by_product.get(p).copied()), by_name.get(&clean(Some(name)).to_lowercase()).copied()].into_iter().flatten().collect();
        let Some(ts) = times.into_iter().min() else { continue };
        matched += 1;
        if added.map_or(true, |a| a == 0 || ts < a) { set.push((*appid, ts)); }
    }
    (usable, matched, set)
}

// ---------------------------------------------------------------- purchase dates read by Zest itself, with the store website's session
// Sony serves the transaction history only to the website's own session (its app token lacks the scope). The website's sign-in address hands an NPSSO cookie
// to Sony's authorize step with the website's client (scope web:core), which answers with a code without any login page (no bot check), and the session
// address turns that into the website's cookies. Opened at sign-in while the pasted NPSSO is at hand; kept in memory only.

const WEB_SIGNIN: &str = "https://web.np.playstation.com/api/session/v1/signin?redirect_uri=https%3A%2F%2Fio.playstation.com%2Fcentral%2Fauth%2Flogin%3FpostSignInURL%3Dhttps%253A%252F%252Fstore.playstation.com%252Fen-us%252F%26cancelURL%3Dhttps%253A%252F%252Fstore.playstation.com%252Fen-us%252F&smcid=web%3Astore";
const WEB_UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0 Safari/537.36";
const HISTORY: &str = "https://web.np.playstation.com/api/graphql/v1//op";
/// Persisted-query hashes of transactionHistoryRetrieve, newest first (the userscript's list; Sony rotates them now and then).
const HISTORY_HASHES: [&str; 2] = ["496f6f30273e14e92498da51447f773b1c9e30c3cfefb8739627c31673d08751", "f04fe7c7d8498bee5cd0615400eceb07d77eab097d118475ac9fa0c8446b3a42"];
const NO_WEB_SESSION: &str = "Sony only gives purchase dates to a website session, which Zest opens when you sign in and keeps until it quits. Sign in to PlayStation again (paste a fresh NPSSO), then try again.";
static WEB_SESSION: Mutex<Option<String>> = Mutex::new(None);
fn web_cookie() -> Option<String> { WEB_SESSION.lock().ok()?.clone() }

/// Adds the cookies a `Set-Cookie` answer sets to `jar` (a later value replaces an earlier one of the same name).
pub fn take_cookies(jar: &mut Vec<(String, String)>, set_cookies: &[&str]) {
    for h in set_cookies {
        let nv = h.split(';').next().unwrap_or("");
        let Some((n, v)) = nv.split_once('=') else { continue };
        let n = n.trim().to_string(); if n.is_empty() { continue; }
        jar.retain(|(k, _)| *k != n); jar.push((n, v.trim().to_string()));
    }
}
fn cookie_header(jar: &[(String, String)]) -> String { jar.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; ") }
/// Only Sony's own sites ever get the cookies: an https address on playstation.com or sony.com.
fn sony_https(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else { return false };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("").to_lowercase();
    host == "playstation.com" || host.ends_with(".playstation.com") || host.ends_with(".sony.com")
}

/// Opens the store website's session with an NPSSO: the cookies to send with the history query.
pub fn web_session(http: &dyn Http, npsso: &str) -> Result<String, String> {
    let t = Duration::from_secs(20);
    let get = |url: &str, cookie: &str| http.get_raw(url, &[("User-Agent", WEB_UA), ("Accept", "text/html,application/json"), ("Cookie", cookie)], t).map_err(|e| format!("could not reach Sony ({e})"));
    let mut jar: Vec<(String, String)> = vec![];
    let r = get(WEB_SIGNIN, "")?; take_cookies(&mut jar, &r.header_all("set-cookie"));
    let authorize = r.header_all("location").first().map(|s| s.to_string()).filter(|l| sony_https(l) && l.contains("/authorize")).ok_or(format!("the website's sign-in answered HTTP {} without the authorize step", r.status))?;
    // the NPSSO goes to Sony's account site only
    let r = get(&authorize, &format!("npsso={npsso}"))?;
    let mut next = r.header_all("location").first().map(|s| s.to_string()).unwrap_or_default();
    if !next.contains("code=") { return Err("Sony did not accept the NPSSO for the store website (it asked for a sign-in)".into()); }
    for _ in 0..5 {
        if !sony_https(&next) { break; }
        let r = get(&next, &cookie_header(&jar))?; take_cookies(&mut jar, &r.header_all("set-cookie"));
        match r.header_all("location").first() { Some(l) if (300..400).contains(&r.status) => next = l.to_string(), _ => break }
    }
    if !jar.iter().any(|(k, _)| k == "pdccws_p") { return Err("Sony did not open a website session".into()); }
    Ok(cookie_header(&jar))
}

pub fn history_url(hash: &str, end: &str, limit: usize) -> String {
    let vars = json!({"startDate": "1994-12-03T00:00:00.000Z", "endDate": end, "limit": limit}).to_string();
    let ext = json!({"persistedQuery": {"version": 1, "sha256Hash": hash}}).to_string();
    format!("{HISTORY}?operationName=transactionHistoryRetrieve&variables={}&extensions={}", enc(&vars), enc(&ext))
}
/// One page's product purchases: (row id, (date, name, sku)). Top-ups, refunds and subscription charges have no products and are skipped.
pub fn history_rows(transactions: &[Value]) -> Vec<(String, PurchaseIn)> {
    let mut out = vec![];
    for t in transactions {
        let Some(date) = sv(t, "date").filter(|d| !d.is_empty()) else { continue };
        for p in t.pointer("/purchaseDetails/productPurchases").and_then(|v| v.as_array()).into_iter().flatten() {
            let Some(sku) = sv(p, "skuId").filter(|s| !s.is_empty()) else { continue };
            let line = sv(p, "orderItemId").map(String::from).or_else(|| p.get("orderItemId").map(|v| v.to_string())).unwrap_or_else(|| sku.to_string());
            out.push((format!("{}:{line}", t.get("id").map(|v| v.as_str().map(String::from).unwrap_or_else(|| v.to_string())).unwrap_or_default()), (date.to_string(), sv(p, "productName").unwrap_or("").to_string(), sku.to_string())));
        }
    }
    out
}
/// Walks the history page by page, as the userscript's psnCollect does: the next persisted-query hash when Sony no longer knows one, a smaller page when a
/// gateway caps it, and whatever was read when a later page fails. `page(url)` is the parsed reply.
pub fn collect_history(page: &mut dyn FnMut(&str) -> Result<Value, String>, now_iso: &str) -> Result<Vec<PurchaseIn>, String> {
    let (mut all, mut seen) = (vec![], HashSet::new());
    let (mut hash, mut limit, mut end) = (0usize, 50usize, now_iso.to_string());
    for _ in 0..400 {
        let v = page(&history_url(HISTORY_HASHES[hash], &end, limit))?;
        let block = v.pointer("/data/transactionHistoryRetrieve").filter(|b| b.get("transactions").map_or(false, |t| t.is_array()));
        let Some(block) = block else {
            let err = v.get("errors").and_then(|e| e.as_array()).map(|a| a.iter().map(|e| sv(e, "message").or_else(|| e.pointer("/extensions/code").and_then(|c| c.as_str())).unwrap_or("error").to_string()).collect::<Vec<_>>().join("; ")).filter(|s| !s.is_empty()).unwrap_or_else(|| "unexpected reply".into());
            if !all.is_empty() { break; }
            if err.to_lowercase().replace(' ', "").contains("persistedquery") && hash + 1 < HISTORY_HASHES.len() { hash += 1; continue; }
            if limit > 15 { limit = 15; continue; }
            return Err(format!("Sony did not return the purchase history: {err}"));
        };
        for (id, row) in history_rows(block["transactions"].as_array().map(|a| a.as_slice()).unwrap_or(&[])) { if seen.insert(id) { all.push(row); } }
        match (block.get("hasMore").and_then(|v| v.as_bool()).unwrap_or(false), sv(block, "nextEndDate")) { (true, Some(n)) if !n.is_empty() => end = n.to_string(), _ => break }
    }
    Ok(all)
}

impl PlaystationSource {
    /// Reads the purchase history with the website session and sets `date_added` on the matched games (all PlayStation games, or only `only`).
    /// Returns (appid, purchase time) for each game it set. An overwrite, as Bulk Ops' purchase dates for every store.
    fn dates_from_history(&self, cookie: &str, only: Option<&[i64]>) -> Result<Vec<(i64, i64)>, String> {
        let rid = || format!("{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        let mut page = |url: &str| -> Result<Value, String> {
            let id = rid();
            let r = self.http.get(url, &[("Cookie", cookie), ("User-Agent", WEB_UA), ("Accept", "application/json"), ("content-type", "application/json"),
                ("apollographql-client-name", "@sie-ppr-web-checkout/app"), ("apollographql-client-version", "2.184.0"), ("x-psn-storefront-type", "checkout:store"),
                ("x-psn-app-ver", "@sie-ppr-web-checkout/app/v2.184.0"), ("x-psn-request-id", &id)], Duration::from_secs(30)).map_err(|e| format!("could not reach Sony ({e})"))?;
            std::thread::sleep(Duration::from_millis(120));   // gentle between pages, as the userscript
            serde_json::from_str(&r.body).map_err(|_| if r.status == 401 || r.status == 403 { NO_WEB_SESSION.to_string() } else { format!("Sony answered HTTP {} without data", r.status) })
        };
        let now = history_now();
        let purchases = collect_history(&mut page, &now)?;
        if purchases.is_empty() { return Err("No purchases were found on this account.".into()); }
        let conn = rusqlite::Connection::open(&self.db).map_err(|e| e.to_string())?;
        let _ = conn.busy_timeout(Duration::from_secs(10));
        let rows: Vec<DateRow> = {
            let mut st = conn.prepare("SELECT appid, COALESCE(name, ''), COALESCE(platform_slug, ''), COALESCE(platform_appname, '') FROM games WHERE platform = 'playstation'").map_err(|e| e.to_string())?;
            let v = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, None))).map_err(|e| e.to_string())?.flatten().filter(|r: &DateRow| only.map_or(true, |o| o.contains(&r.0))).collect();
            v
        };
        // no stored date passed in: every matched game gets its earliest purchase
        let (usable, matched, set) = match_purchase_dates(&purchases, &rows);
        for (appid, ts) in &set { conn.execute("UPDATE games SET date_added = ?1 WHERE appid = ?2", rusqlite::params![ts, appid]).map_err(|e| e.to_string())?; }
        log(&format!("PlayStation purchase dates (website session): {usable} purchase(s) read, {matched} game(s) matched, {} date(s) set", set.len()));
        Ok(set)
    }
}

/// Now as `YYYY-MM-DDTHH:MM:SS.000Z` (the history query's end date).
fn history_now() -> String {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let (y, m, d, _) = crate::pagywosg_filter::civil(t);
    let sod = t.rem_euclid(86400);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.000Z", sod / 3600, sod % 3600 / 60, sod % 60)
}

impl PlaystationSource {
    /// Applies purchases read in the browser: refuses a different account than the connected one, then sets `date_added` on matched games.
    /// Returns (purchases read, games matched, dates set).
    pub fn apply_dates(&self, online_id: &str, purchases: &[PurchaseIn]) -> Result<(usize, usize, usize), String> {
        let mine = self.secret("online_id").unwrap_or_default().to_lowercase();
        if !online_id.is_empty() && !mine.is_empty() && online_id.to_lowercase() != mine {
            return Err(format!("This browser is signed in to PlayStation as {online_id}, but Zest is connected as {}.", self.secret("online_id").unwrap_or_default()));
        }
        let conn = rusqlite::Connection::open(&self.db).map_err(|e| e.to_string())?;
        let _ = conn.busy_timeout(Duration::from_secs(10));
        let rows: Vec<DateRow> = {
            let mut st = conn.prepare("SELECT appid, COALESCE(name, ''), COALESCE(platform_slug, ''), COALESCE(platform_appname, ''), date_added FROM games WHERE platform = 'playstation'").map_err(|e| e.to_string())?;
            let v = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get::<_, Option<i64>>(4)?))).map_err(|e| e.to_string())?.flatten().collect();
            v
        };
        let (usable, matched, set) = match_purchase_dates(purchases, &rows);
        for (appid, ts) in &set { conn.execute("UPDATE games SET date_added = ?1 WHERE appid = ?2", rusqlite::params![ts, appid]).map_err(|e| e.to_string())?; }
        log(&format!("PlayStation purchase dates: {usable} purchase(s) read, {matched} game(s) matched, {} date(s) set", set.len()));
        Ok((usable, matched, set.len()))
    }
}

#[cfg(test)]
mod web_dates_tests {
    use super::*;
    #[test] fn cookies_are_collected_by_name_and_only_sony_sites_get_them() {
        let mut jar = vec![];
        take_cookies(&mut jar, &["pdccws_p=aaa; Domain=.playstation.com; Path=/; HttpOnly", "bm_sz=x; Max-Age=4"]);
        take_cookies(&mut jar, &["pdccws_p=bbb; Path=/", "broken", "=nameless"]);
        assert_eq!(cookie_header(&jar), "bm_sz=x; pdccws_p=bbb");
        for ok in ["https://web.np.playstation.com/api/x", "https://ca.account.sony.com/a?b", "https://playstation.com/"] { assert!(sony_https(ok), "{ok}"); }
        for no in ["http://web.np.playstation.com/", "https://evil.com/?playstation.com", "https://playstation.com.evil.com/", "https://notplaystation.com/"] { assert!(!sony_https(no), "{no}"); }
    }
    #[test] fn the_history_is_read_page_by_page_with_the_hash_and_size_fallbacks() {
        let tx = |id: &str, date: &str, sku: &str, name: &str| json!({"id": id, "date": date, "purchaseDetails": {"productPurchases": [{"skuId": sku, "productName": name, "orderItemId": 1}]}});
        let pages = vec![
            json!({"errors": [{"message": "PersistedQueryNotFound"}]}),
            json!({"errors": [{"message": "limit too large"}]}),
            json!({"data": {"transactionHistoryRetrieve": {"transactions": [tx("t1", "2024-05-01T10:00:00Z", "UP4040-PPSA30074_00-GAME-U001", "Game"), {"id": "t2", "date": "2024-04-01T00:00:00Z", "purchaseDetails": {}}], "hasMore": true, "nextEndDate": "2024-04-01T00:00:00Z"}}}),
            json!({"data": {"transactionHistoryRetrieve": {"transactions": [tx("t1", "2024-05-01T10:00:00Z", "UP4040-PPSA30074_00-GAME-U001", "Game"), tx("t3", "2020-01-01T00:00:00Z", "EP0001-CUSA00001_00-OLD-0001", "Old")], "hasMore": false}}}),
        ];
        let mut urls = vec![]; let mut it = pages.into_iter();
        let got = collect_history(&mut |u: &str| { urls.push(u.to_string()); Ok(it.next().unwrap()) }, "2026-11-25T00:00:00.000Z").unwrap();
        assert_eq!(got, vec![("2024-05-01T10:00:00Z".to_string(), "Game".to_string(), "UP4040-PPSA30074_00-GAME-U001".to_string()), ("2020-01-01T00:00:00Z".to_string(), "Old".to_string(), "EP0001-CUSA00001_00-OLD-0001".to_string())], "the repeated row once, the top-up skipped");
        assert!(urls[0].contains(HISTORY_HASHES[0]) && urls[1].contains(HISTORY_HASHES[1]), "the next hash after an unknown one");
        assert!(urls[1].contains("%22limit%22%3A50") && urls[2].contains("%22limit%22%3A15"), "a smaller page after a refused one");
        assert!(urls[3].contains("2024-04-01T00%3A00%3A00Z"), "the next page ends where the last one said");
        let bad = || Ok(json!({"errors": [{"message": "Access denied"}]}));
        assert!(collect_history(&mut |_: &str| bad(), "x").unwrap_err().contains("Access denied"));
        assert!(history_now().ends_with(".000Z") && history_now().len() == 24);
    }
}

#[cfg(test)]
mod deep_scan_tests {
    use super::*;
    use crate::net::{NetError, Response};
    struct Offline;
    impl Http for Offline { fn get(&self, _u: &str, _h: &[(&str, &str)], _t: Duration) -> Result<Response, NetError> { Ok(Response { status: 404, retry_after: None, body: String::new() }) } }
    #[test] fn the_deep_scan_flags_demos_and_add_ons_by_name_or_store_record() {
        let d = std::env::temp_dir().join(format!("zest_psn_scan_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("config.json"), "{}").unwrap();
        // a cached store record for concept 777 says its product is an add-on; concept 888 is a game
        std::fs::write(d.join("playstation_store.json"), json!({"777": {"fetched": 4102444800u64, "products": {"p": {"cls": "ADD_ON", "top": "ADD_ON"}}}, "888": {"fetched": 4102444800u64, "products": {"p": {"cls": "GAME", "top": "GAME"}}}}).to_string()).unwrap();
        let db = d.join("games_1.db");
        rusqlite::Connection::open(&db).unwrap().execute_batch("CREATE TABLE games (appid INTEGER, name TEXT, platform TEXT, platform_slug TEXT, playtime_forever INTEGER, installed INTEGER, completion_status TEXT);
            INSERT INTO games VALUES (1, 'Cool Game', 'playstation', '888', 0, 0, 'Never Played');
            INSERT INTO games VALUES (2, 'Cool Game Demo', 'playstation', '888', 0, 0, 'Never Played');
            INSERT INTO games VALUES (3, 'Cool Game Costume Pack', 'playstation', '777', 0, 0, 'Never Played');
            INSERT INTO games VALUES (4, 'Played Demo', 'playstation', '888', 30, 0, 'Never Played');
            INSERT INTO games VALUES (5, 'Beaten Costume Pack', 'playstation', '777', 0, 0, 'Beaten');").unwrap();
        let s = PlaystationSource::new(Arc::new(Offline), d.clone(), db);
        let got: Vec<(i64, String)> = s.scan_junk().unwrap().into_iter().map(|h| (h.appid, h.reason)).collect();
        assert_eq!(got, vec![(2, "name says demo, trial version or beta".to_string()), (3, "store says ADD_ON".to_string())], "{got:?}");
    }
}
