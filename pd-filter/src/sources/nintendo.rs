//! Nintendo eShop as a library source (port of PlayDate's `nintendo` plugin: `nintendo.py` + `store.py`). Nintendo has no public library API: the account's
//! Virtual Game Cards page embeds a short-lived token and the shop GraphQL address, and the page's own query is replayed with the user's pasted Cookie header.
//! Metadata comes from nintendo.com's public search-only Algolia index, matched by name. The cookie and the token are never logged.
use super::{AuthBegin, AuthStatus, Caps, GameRef, LibrarySource, SourceCtx};
use crate::hltb::sequence_ratio;
use crate::importer::GameRecord;
use crate::jobs::Update;
use crate::metadata::PageMeta;
use crate::net::{enc, log, Cancel, Http};
use crate::steamclient::Fetch;
use regex::Regex;
use rusqlite::types::Value as Sql;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const PAGE_URL: &str = "https://accounts.nintendo.com/portal/vgcs";
const PAGE_SIZE: i64 = 300;
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";
const ALGOLIA_URL: &str = "https://U3B6GR4UA3-dsn.algolia.net/1/indexes/store_game_en_us/query";
const MATCH: f64 = 0.90;
const VIEWS_TTL: Duration = Duration::from_secs(90);
const EXPIRED: &str = "Your Nintendo session expired. Disconnect and connect again.";

const QUERY: &str = "query getVgcsVgcs(\n  $idToken: String!\n  $country: CountryCode!\n  $language: LanguageCode!\n  $shopId: Int!\n  $limit: Int!\n  $nasLanguage: String!\n  $offset: Int!\n  $order: RequestableVgcViewOrder!\n  $sortBy: RequestableVgcViewSortBy!\n  $vgcViewType: VgcViewTypeInput\n  $vgcViewStatus: VgcViewStatusInput\n) @inContext(country: $country, language: $language, shopId: $shopId) {\n  account {\n    vgc {\n      vgcViewsV2(\n        idToken: $idToken,\n        limit: $limit,\n        language: $nasLanguage,\n        offset: $offset,\n        order: $order,\n        sortBy: $sortBy,\n        isHidden: false,\n        vgcViewType: $vgcViewType,\n        vgcViewStatus: $vgcViewStatus,\n      ) {\n        offsetInfo { total offset }\n        views {\n          id\n          applicationId\n          applicationName\n          apparentPlatform\n          publisher\n          hasReleasedApplication\n          hasReleasedAddOnContents\n        }\n      }\n    }\n  }\n}\n";

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex { cell.get_or_init(|| Regex::new(pat).unwrap()) }

// ---------------------------------------------------------------- pure helpers (checked against the Python)

/// `_clean_name`: trademark signs and "full game" removed, whitespace collapsed, a trailing "for Nintendo Switch" label dropped.
pub fn clean_name(name: &str) -> String {
    static TM: OnceLock<Regex> = OnceLock::new(); static FG: OnceLock<Regex> = OnceLock::new(); static WS: OnceLock<Regex> = OnceLock::new(); static SW: OnceLock<Regex> = OnceLock::new();
    let n = re(&TM, "[\u{2122}\u{00ae}\u{00a9}]").replace_all(name, "");
    let n = re(&FG, r"(?i)\bfull game\b").replace_all(&n, "");
    let n = re(&WS, r"\s+").replace_all(&n, " ").trim().to_string();
    let t = re(&SW, r"(?i)\s+for Nintendo Switch(?: 2)?$").replace(&n, "").trim().to_string();
    if t.is_empty() { n } else { t }
}

/// `store._norm`
pub fn store_norm(name: &str) -> String {
    static TM: OnceLock<Regex> = OnceLock::new(); static NW: OnceLock<Regex> = OnceLock::new(); static WS: OnceLock<Regex> = OnceLock::new();
    let n = re(&TM, "[\u{2122}\u{00ae}\u{00a9}]").replace_all(name, "").to_lowercase();
    let n = re(&NW, r"[^\w\s]").replace_all(&n, " ");
    re(&WS, r"\s+").replace_all(&n, " ").trim().to_string()
}

/// `store._categories`: Steam-style modes from the store's player count text.
pub fn categories(player_count: &str) -> String {
    let pc = player_count.to_lowercase();
    if pc.contains("single") { return "Single-player".into(); }
    static N: OnceLock<Regex> = OnceLock::new();
    let nums: Vec<i64> = re(&N, r"\d+").find_iter(&pc).filter_map(|m| m.as_str().parse().ok()).collect();
    if nums.is_empty() { return String::new(); }
    let mut lo = *nums.iter().min().unwrap();
    let hi = if pc.contains('+') { 99 } else { *nums.iter().max().unwrap() };
    if pc.contains("up to") { lo = 1; }
    if lo > 1 { return "Multi-player".into(); }
    if hi > 1 { "Single-player,Multi-player".into() } else { "Single-player".into() }
}

const TAG_ALIASES: &[(&str, &str)] = &[("pixel art", "Pixel Graphics"), ("rhythmic gameplay", "Rhythm"), ("platforming", "Platformer"), ("third-person", "Third Person"),
    ("random maps & levels", "Procedural Generation"), ("voiced characters", "Voice Acting")];

/// The ISO date the store gives, as midnight UTC of that day (Python: `fromisoformat(...).astimezone(utc)` with the time zeroed, as a timestamp).
pub fn release_ts(iso: &str) -> Option<i64> {
    static R: OnceLock<Regex> = OnceLock::new();
    let c = re(&R, r"^(\d{4})-(\d{2})-(\d{2})(?:[T ](\d{2}):(\d{2}):(\d{2})(?:\.\d+)?)?(Z|[+-]\d{2}:\d{2})?$").captures(iso)?;
    let n = |i: usize| c.get(i).map_or(0, |m| m.as_str().parse::<i64>().unwrap_or(0));
    let (y, mo, d) = (n(1), n(2), n(3));
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) { return None; }
    let off = c.get(7).map_or(0, |m| { let t = m.as_str(); if t == "Z" { 0 } else { let sign = if t.starts_with('-') { -1 } else { 1 }; sign * (t[1..3].parse::<i64>().unwrap_or(0) * 3600 + t[4..6].parse::<i64>().unwrap_or(0) * 60) } });
    let (yy, mm) = if mo <= 2 { (y - 1, mo + 9) } else { (y, mo - 3) };
    let era = yy.div_euclid(400); let yoe = yy - era * 400; let doy = (153 * mm + 2) / 5 + d - 1; let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let local = (era * 146097 + doe - 719468) * 86400 + n(4) * 3600 + n(5) * 60 + n(6);
    Some((local - off).div_euclid(86400) * 86400)
}

/// `store.metadata_from_entry`: the columns a store entry gives. `vocab` = the library's tags by lower-case name (a store tag is used only when the library has it).
pub fn metadata_from_entry(h: &Value, vocab: &HashMap<String, String>) -> Update {
    let t = |s: String| Sql::Text(s);
    let mut out: Update = vec![];
    let genres: Vec<&str> = h.get("gameGenreLabels").and_then(|g| g.as_array()).map(|a| a.iter().filter_map(|x| x.as_str()).filter(|x| !x.is_empty()).collect()).unwrap_or_default();
    if !genres.is_empty() { out.push(("genres", t(genres.join(",")))); }
    let cats = categories(h.get("playerCount").and_then(|p| p.as_str()).unwrap_or(""));
    if !cats.is_empty() { out.push(("categories", t(cats))); }
    let mut tags: Vec<String> = vec![];
    for label in h.get("gameFeatureLabels").and_then(|g| g.as_array()).into_iter().flatten().filter_map(|x| x.as_str()) {
        let key = label.trim().to_lowercase();
        let mapped = TAG_ALIASES.iter().find(|(k, _)| *k == key).map(|(_, v)| v.to_string()).unwrap_or(key.clone());
        if let Some(canon) = vocab.get(&mapped.to_lowercase()) { if !tags.contains(canon) { tags.push(canon.clone()); } }
    }
    if !tags.is_empty() { out.push(("tags", t(tags.join(",")))); }
    if let Some(ts) = h.get("releaseDate").and_then(|r| r.as_str()).filter(|r| !r.is_empty()).and_then(release_ts) { out.push(("release_date", Sql::Integer(ts))); }
    if let Some(d) = h.get("softwareDeveloper").and_then(|d| d.as_str()).map(|d| d.trim()).filter(|d| !d.is_empty()) { out.push(("developers", t(d.to_string()))); }
    if let Some(p) = h.get("softwarePublisher").and_then(|d| d.as_str()).map(|d| d.trim()).filter(|d| !d.is_empty()) { out.push(("publishers", t(p.to_string()))); }
    out
}

fn strip_tags(s: &str) -> String { static T: OnceLock<Regex> = OnceLock::new(); re(&T, r"<[^>]+>").replace_all(s, "").trim().to_string() }
/// `store.description_from_entry`
pub fn description_from_entry(h: &Value) -> Option<String> { Some(strip_tags(h.get("description").and_then(|d| d.as_str()).unwrap_or(""))).filter(|d| !d.is_empty()) }

/// `_embedded`: the JSON in `<... id="X" data-json="...">`.
pub fn embedded(text: &str, id: &str) -> Option<Value> {
    let r = Regex::new(&format!(r#"id="{}"\s+data-json="([^"]*)""#, regex::escape(id))).ok()?;
    serde_json::from_str(&html_escape::decode_html_entities(&r.captures(text)?[1])).ok()
}

pub struct Page { pub id_token: String, pub client_id: String, pub graphql: String, pub country: String, pub lang: String, pub nickname: String }

/// `_load_page`'s parsing; None = not signed in.
pub fn parse_page(final_url: &str, status: u16, body: &str) -> Option<Page> {
    if status != 200 || !final_url.contains("/portal/vgcs") { return None; }
    page_from_embedded(embedded(body, "data"), embedded(body, "state"), embedded(body, "meta"))
}
/// The page's three embedded JSON blocks (`#data`, `#state`, `#meta`) as a `Page`; the userscript sends the same blocks it reads from the open page.
pub fn page_from_embedded(data: Option<Value>, state: Option<Value>, meta: Option<Value>) -> Option<Page> {
    let (data, state) = (data.filter(|d| d.get("idToken").and_then(|t| t.as_str()).map_or(false, |t| !t.is_empty()))?, state.filter(|s| !s.is_null() && s.as_object().map_or(true, |o| !o.is_empty()))?);
    let user = state.get("user").cloned().unwrap_or(Value::Null);
    let country = meta.as_ref().and_then(|m| m.get("countries")).and_then(|c| c.as_array()).and_then(|a| a.iter().find(|c| c.get("id") == user.get("countryId")).and_then(|c| c.get("code")).and_then(|c| c.as_str())).unwrap_or("US").to_string();
    let s = |v: &Value, k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    Some(Page { id_token: s(&data, "idToken"), client_id: s(&data, "savannaClientId"), graphql: s(&data, "shopGraphQLApiUrl"), country, lang: Some(s(&state, "lang")).filter(|l| !l.is_empty()).unwrap_or_else(|| "en-US".into()), nickname: s(&user, "nickname") })
}

/// `connect`'s cookie handling: the popup's JSON jar or a pasted `Cookie:` header, as a header value.
pub fn normalise_cookie(raw: &str) -> Result<String, String> {
    let mut c = raw.trim().to_string();
    if c.starts_with('{') {
        let jar: Value = serde_json::from_str(&c).map_err(|_| "Could not read the login session".to_string())?;
        let o = jar.as_object().filter(|o| !o.is_empty()).ok_or("No cookies captured. Try signing in again.")?;
        c = o.iter().filter_map(|(k, v)| v.as_str().map(|v| format!("{k}={v}"))).collect::<Vec<_>>().join("; ");
    }
    if c.to_lowercase().starts_with("cookie:") { c = c[7..].trim().to_string(); }
    if !c.contains('=') { return Err("That does not look like a Cookie header".into()); }
    Ok(c)
}

// ---------------------------------------------------------------- the source

/// Library records for the account's Virtual Game Cards: names cleaned, DLC-only cards skipped.
fn records_from_views(views: &[Value]) -> Vec<GameRecord> {
    let mut batch = vec![];
    for v in views {
        let (tid, name) = (v.get("applicationId").and_then(|t| t.as_str()).unwrap_or(""), clean_name(v.get("applicationName").and_then(|t| t.as_str()).unwrap_or("")));
        if tid.is_empty() || name.is_empty() { continue; }
        // a DLC-only card is not a game (Playnite's plugin skips these too)
        if !truthy(v.get("hasReleasedApplication")) && truthy(v.get("hasReleasedAddOnContents")) { continue; }
        batch.push(GameRecord { platform_id: tid.to_string(), name, publishers: v.get("publisher").and_then(|p| p.as_str()).map(|p| p.trim().to_string()).filter(|p| !p.is_empty()), ..Default::default() });
    }
    batch
}

pub struct NintendoSource { http: Arc<dyn Http>, config_dir: PathBuf, db: PathBuf, views: Mutex<Option<(Instant, Vec<Value>)>> }

impl NintendoSource {
    pub fn new(http: Arc<dyn Http>, config_dir: PathBuf, db: PathBuf) -> Self { NintendoSource { http, config_dir, db, views: Mutex::new(None) } }
    fn cookie(&self) -> Option<String> { crate::account::plugin_secret_get(&self.config_dir, "nintendo", "cookie").filter(|c| !c.is_empty()) }

    fn load_page(&self, cookie: &str) -> Result<Page, String> {
        // A signed-in session gets the page itself. A signed-out one is redirected to Nintendo's login page, which never answers a
        // script (the request hangs until it times out), so a redirect to it is answered here as "not accepted" without following it.
        match self.http.get_raw(PAGE_URL, &[("Cookie", cookie), ("User-Agent", UA)], Duration::from_secs(20)) {
            Ok(r) if r.status == 200 => return parse_page(PAGE_URL, 200, &r.body).ok_or_else(|| EXPIRED.to_string()),
            Ok(r) if (300..400).contains(&r.status) && r.headers.iter().any(|(k, v)| k.eq_ignore_ascii_case("location") && v.contains("login")) => return Err(EXPIRED.to_string()),
            _ => {}
        }
        match self.http.get_final(PAGE_URL, &[("Cookie", cookie), ("User-Agent", UA)], Duration::from_secs(20)) {
            Ok((r, url)) => parse_page(&url, r.status, &r.body).ok_or_else(|| EXPIRED.to_string()),
            Err(e) => { log(&format!("Nintendo: could not reach the account page: {e}")); Err("Could not reach Nintendo".into()) }
        }
    }

    /// All Virtual Game Cards, oldest acquired first.
    fn fetch_views(&self, p: &Page, cancel: &Cancel) -> Result<Vec<Value>, String> {
        let (mut views, mut offset) = (vec![], 0i64);
        loop {
            if cancel.is_set() { return Ok(views); }
            let body = json!({"operationName": "getVgcsVgcs", "query": QUERY, "variables": {"idToken": p.id_token, "country": p.country, "language": p.lang.chars().take(2).collect::<String>(), "nasLanguage": p.lang,
                "shopId": 3, "limit": PAGE_SIZE, "offset": offset, "order": "ASC", "sortBy": "ACTIVATED_DATE"}}).to_string();
            let r = self.http.post(&p.graphql, &[("Content-Type", "application/json"), ("User-Agent", UA), ("X-Nintendo-Savanna-Client-Id", &p.client_id)], &body, Duration::from_secs(30)).map_err(|e| format!("Could not reach Nintendo ({e})"))?;
            let j: Value = serde_json::from_str(&r.body).unwrap_or(Value::Null);
            if r.status != 200 || j.get("errors").map_or(false, |e| !e.is_null() && e.as_array().map_or(true, |a| !a.is_empty())) {
                // the GraphQL error text only, never the request
                log(&format!("Nintendo: query failed HTTP {}: {}", r.status, j.get("errors").map(|e| e.to_string().chars().take(300).collect::<String>()).unwrap_or_default()));
                return Err("Nintendo rejected the library request. Try reconnecting.".into());
            }
            let block = j.pointer("/data/account/vgc/vgcViewsV2").cloned().unwrap_or(Value::Null);
            let batch = block.get("views").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            let total = block.pointer("/offsetInfo/total").and_then(|t| t.as_i64()).unwrap_or(0);
            offset += batch.len() as i64; let empty = batch.is_empty();
            views.extend(batch);
            if empty || offset >= total { return Ok(views); }
        }
    }

    fn cached_views(&self) -> Option<Vec<Value>> {
        let cookie = self.cookie()?;
        if let Some((at, v)) = self.views.lock().unwrap().as_ref() { if at.elapsed() < VIEWS_TTL { return Some(v.clone()); } }
        match self.load_page(&cookie).and_then(|p| self.fetch_views(&p, &Cancel::new())) {
            Ok(v) => { *self.views.lock().unwrap() = Some((Instant::now(), v.clone())); Some(v) }
            Err(e) => { log(&format!("Nintendo: could not re-read the Virtual Game Cards list: {e}")); None }
        }
    }

    // ---- the store index
    fn query(&self, text: &str, n: usize) -> Vec<Value> {
        let body = json!({"params": format!("query={}&hitsPerPage={n}", enc(text))}).to_string();
        match self.http.post(ALGOLIA_URL, &[("Content-Type", "application/json"), ("X-Algolia-Application-Id", "U3B6GR4UA3"), ("X-Algolia-API-Key", "a29c6927638bfd8cee23993e51e721c9")], &body, Duration::from_secs(15)) {
            Ok(r) if (200..300).contains(&r.status) => serde_json::from_str::<Value>(&r.body).ok().and_then(|j| j.get("hits").and_then(|h| h.as_array()).cloned()).unwrap_or_default(),
            Ok(r) => { log(&format!("Nintendo store search failed: HTTP {}", r.status)); vec![] }
            Err(e) => { log(&format!("Nintendo store search failed: {e}")); vec![] }
        }
    }
    fn is_game(h: &Value) -> bool { h.get("topLevelCategoryCode").and_then(|c| c.as_str()) == Some("GAMES") && !truthy(h.get("dlcType")) && !truthy(h.get("isUpgrade")) }
    pub fn find_by_name(&self, name: &str) -> Option<Value> {
        static DEMO: OnceLock<Regex> = OnceLock::new();
        if name.is_empty() || re(&DEMO, r"(?i)\b(demo|trial|prologue|playtest|beta)\b").is_match(name) { return None; }
        let want = store_norm(name);
        let (mut best, mut score) = (None, 0.0);
        for h in self.query(name, 12) {
            if !Self::is_game(&h) || h.get("url").and_then(|u| u.as_str()).map_or(true, str::is_empty) { continue; }
            let s = sequence_ratio(&want, &store_norm(h.get("title").and_then(|t| t.as_str()).unwrap_or("")));
            if s > score { best = Some(h); score = s; }
        }
        if score >= MATCH { best } else { None }
    }
    pub fn find_by_slug(&self, slug: &str) -> Option<Value> {
        if slug.is_empty() { return None; }
        let key = slug.trim_matches('/').rsplit('/').next().unwrap_or("").replace('-', " ");
        self.query(&key, 20).into_iter().find(|h| Self::is_game(h) && h.get("url").and_then(|u| u.as_str()).unwrap_or("").trim_end_matches('/') == slug.trim_end_matches('/'))
    }
    fn entry_for(&self, slug: &str, name: &str) -> Option<Value> { self.find_by_slug(slug).or_else(|| self.find_by_name(name)) }
    /// The full description from the product page (the index has only a one-line blurb, or nothing).
    fn page_description(&self, h: &Value) -> Option<String> {
        static ND: OnceLock<Regex> = OnceLock::new();
        let url = format!("https://www.nintendo.com{}", h.get("url")?.as_str()?);
        let r = self.http.get(&url, &[("User-Agent", "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36")], Duration::from_secs(20)).ok().filter(|r| (200..300).contains(&r.status))?;
        let state: Value = serde_json::from_str(&re(&ND, r#"(?s)<script id="__NEXT_DATA__"[^>]*>(.*?)</script>"#).captures(&r.body)?[1]).ok()?;
        let nsuid = h.get("nsuid").map(|n| n.to_string().trim_matches('"').to_string());
        for (k, rec) in state.pointer("/props/pageProps/initialApolloState")?.as_object()? {
            if k.starts_with("Product:") && rec.get("nsuid").map(|n| n.to_string().trim_matches('"').to_string()) == nsuid {
                if let Some(d) = rec.get("description").and_then(|d| d.as_str()).filter(|d| !d.trim().is_empty()) { return Some(strip_tags(d)); }
            }
        }
        None
    }
    fn vocab(&self) -> HashMap<String, String> {
        let mut v = HashMap::new();
        let Ok(c) = rusqlite::Connection::open_with_flags(&self.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { return v };
        let Ok(mut st) = c.prepare("SELECT tags FROM games WHERE tags IS NOT NULL AND tags != '' ORDER BY appid") else { return v };
        for row in st.query_map([], |r| r.get::<_, String>(0)).into_iter().flatten().flatten() { for t in row.split(',') { let t = t.trim(); v.entry(t.to_lowercase()).or_insert_with(|| t.to_string()); } }
        v
    }
}

fn truthy(v: Option<&Value>) -> bool { match v { None | Some(Value::Null) => false, Some(Value::Bool(b)) => *b, Some(Value::String(s)) => !s.is_empty(), Some(Value::Number(n)) => n.as_f64() != Some(0.0), Some(Value::Array(a)) => !a.is_empty(), Some(Value::Object(o)) => !o.is_empty() } }

impl LibrarySource for NintendoSource {
    fn id(&self) -> &str { "nintendo" }
    fn label(&self) -> &str { "Nintendo eShop" }
    fn caps(&self) -> Caps { Caps { auth: true, sync: true, rescrape: true, description: true, store_page_metadata: true, store_page: true, ..Default::default() } }
    fn store_url(&self, g: &GameRef) -> Option<String> { (!g.slug.is_empty()).then(|| format!("https://www.nintendo.com{}", g.slug)) }

    fn auth_status(&self) -> Result<AuthStatus, String> {
        let c = self.cookie().is_some();
        Ok(AuthStatus { connected: c, username: c.then(|| crate::account::plugin_secret_get(&self.config_dir, "nintendo", "nickname").filter(|n| !n.is_empty()).unwrap_or_else(|| "Nintendo Account".into())) })
    }
    /// The sign-in window opens the Virtual Game Cards page (a signed-out visitor is sent to the login first); once the page itself is showing, the window hands back the account's cookies.
    fn web_login_done(&self) -> Option<serde_json::Value> { Some(serde_json::json!({"kind": "cookies", "url": "https://accounts.nintendo.com/", "names": ["*"], "url_contains": "portal/vgcs", "url_not_contains": "login"})) }
    fn paste_login_kept(&self) -> bool { false }   // the sign-in window works (confirmed live 2026-11-22)
    fn auth_begin(&self) -> Result<AuthBegin, String> {
        Ok(AuthBegin { url: "https://accounts.nintendo.com/portal/vgcs/?sort=activated_date&order=desc".into(), placeholder: "Paste the Cookie header value here".into(), instructions: vec![
            "Sign in at the Virtual Game Cards page in your browser.".into(),
            "Open the developer tools (F12), go to Network, reload, click the \"vgcs\" document request, and copy the whole Cookie request header.".into(),
            "Paste it below.".into()] })
    }
    fn auth_complete(&self, value: &str) -> Result<AuthStatus, String> {
        let cookie = normalise_cookie(value)?;
        let page = self.load_page(&cookie).map_err(|e| if e == EXPIRED { "Nintendo did not accept that session. Sign in and copy the Cookie header again.".to_string() } else { e })?;
        crate::account::plugin_secret_set(&self.config_dir, "nintendo", "cookie", Some(&cookie)).map_err(|_| "Could not save the session".to_string())?;
        crate::account::plugin_secret_set(&self.config_dir, "nintendo", "nickname", Some(&page.nickname)).map_err(|_| "Could not save the session".to_string())?;
        log("Nintendo: connected");
        Ok(AuthStatus { connected: true, username: Some(if page.nickname.is_empty() { "Nintendo Account".into() } else { page.nickname }) })
    }
    fn auth_disconnect(&self) -> Result<(), String> {
        for k in ["cookie", "nickname"] { crate::account::plugin_secret_set(&self.config_dir, "nintendo", k, None).map_err(|_| "Could not save".to_string())?; }
        *self.views.lock().unwrap() = None; Ok(())
    }

    /// The account page's embedded blocks as read in the user's signed-in browser: `{"data": {...}, "state": {...}, "meta": {...}}`. The `idToken` in them is
    /// what the library query needs (the cookie only ever fetched this page), so the list is fetched here; no session is kept.
    fn import_pushed(&self, data: &Value) -> Result<Vec<GameRecord>, String> {
        let page = page_from_embedded(data.get("data").cloned(), data.get("state").cloned(), data.get("meta").cloned()).ok_or("Nintendo's page did not hold a signed-in session. Sign in and try again.")?;
        Ok(records_from_views(&self.fetch_views(&page, &Cancel::new())?))
    }

    fn sync(&self, _cx: &SourceCtx, sink: &mut dyn FnMut(Vec<GameRecord>), cancel: &Cancel) -> Result<usize, String> {
        let cookie = self.cookie().ok_or("Not connected")?;
        let page = self.load_page(&cookie)?;
        let views = self.fetch_views(&page, cancel)?;
        let batch = records_from_views(&views);
        let n = batch.len();
        sink(batch);
        Ok(n)
    }

    /// `sync_game` + `ensure_slugs`: a game with no store page gets one first, then the account list and the store page state replace what is stored. Genres, categories
    /// and tags only for a game with no Steam match (Steam's are richer); nothing is ever blanked.
    fn rescrape(&self, cx: &SourceCtx, g: &GameRef) -> Fetch<Option<Update>> {
        let (steam_appid, mut slug): (Option<i64>, String) = rusqlite::Connection::open_with_flags(&cx.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()
            .and_then(|c| c.query_row("SELECT steam_appid, COALESCE(platform_slug,'') FROM games WHERE appid = ?", [g.appid], |r| Ok((r.get(0)?, r.get(1)?))).ok()).unwrap_or((None, g.slug.clone()));
        let mut out: Update = vec![];
        if slug.is_empty() { if let Some(h) = self.find_by_name(&g.name) { slug = h.get("url").and_then(|u| u.as_str()).unwrap_or("").to_string(); } }
        let mut name = g.name.clone();
        if let Some(v) = self.cached_views().and_then(|vs| vs.into_iter().find(|v| v.get("applicationId").and_then(|t| t.as_str()) == Some(g.platform_id.as_str()))) {
            let n = clean_name(v.get("applicationName").and_then(|t| t.as_str()).unwrap_or(""));
            if !n.is_empty() && n != g.name { out.push(("name", Sql::Text(n.clone()))); name = n; }
            if let Some(p) = v.get("publisher").and_then(|p| p.as_str()).map(|p| p.trim()).filter(|p| !p.is_empty()) { out.push(("publishers", Sql::Text(p.to_string()))); }
        }
        if let Some(h) = self.entry_for(&slug, &name) {
            for (c, v) in metadata_from_entry(&h, &self.vocab()) {
                if steam_appid.is_some() && ["genres", "categories", "tags"].contains(&c) { continue; }
                out.retain(|(k, _)| *k != c); out.push((c, v));
            }
            if let Some(u) = h.get("url").and_then(|u| u.as_str()) { out.retain(|(k, _)| *k != "platform_slug"); out.push(("platform_slug", Sql::Text(u.to_string()))); }
            if let Some(d) = self.page_description(&h).or_else(|| description_from_entry(&h)) { out.push(("short_description", Sql::Text(d))); }
        } else if !slug.is_empty() && slug != g.slug { out.push(("platform_slug", Sql::Text(slug))); }
        Ok((!out.is_empty()).then_some(out))
    }
    fn description(&self, _cx: &SourceCtx, g: &GameRef) -> Option<String> {
        let h = self.find_by_slug(&g.slug)?;
        self.page_description(&h).or_else(|| description_from_entry(&h))
    }
    fn store_page_metadata(&self, slug: &str) -> PageMeta {
        let Some(h) = self.find_by_slug(slug) else { return PageMeta::default() };
        let mut m = PageMeta::default();
        for (c, v) in metadata_from_entry(&h, &self.vocab()) {
            match (c, v) { ("genres", Sql::Text(t)) => m.genres = Some(t), ("categories", Sql::Text(t)) => m.categories = Some(t), ("tags", Sql::Text(t)) => m.tags = Some(t), ("developers", Sql::Text(t)) => m.developers = Some(t), ("publishers", Sql::Text(t)) => m.publishers = Some(t), ("release_date", Sql::Integer(i)) => m.release_date = Some(i), _ => {} }
        }
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{NetError, Response};

    /// A fake nintendo.com: the account page, a two-page GraphQL answer, and the Algolia index.
    struct Site { page_ok: bool, gql_calls: Mutex<Vec<i64>>, hits: Value }
    fn page() -> String {
        let el = |i: &str, v: Value| format!(r#"<div id="{i}" data-json="{}"></div>"#, html_escape::encode_double_quoted_attribute(&v.to_string()));
        el("data", json!({"idToken": "TOK", "savannaClientId": "CID", "shopGraphQLApiUrl": "https://shop.example/graphql"})) + &el("state", json!({"lang": "en-US", "user": {"nickname": "Zelda Fan", "countryId": 1}})) + &el("meta", json!({"countries": [{"id": 1, "code": "US"}]}))
    }
    impl Http for Site {
        fn get(&self, _u: &str, _h: &[(&str, &str)], _t: Duration) -> Result<Response, NetError> { Ok(Response { status: 200, retry_after: None, body: String::new() }) }
        fn get_raw(&self, _u: &str, h: &[(&str, &str)], _t: Duration) -> Result<crate::net::Raw, NetError> {
            assert!(h.iter().any(|(k, v)| *k == "Cookie" && !v.is_empty()));
            if self.page_ok { Ok(crate::net::Raw { status: 200, headers: vec![], body: page() }) }
            else { Ok(crate::net::Raw { status: 302, headers: vec![("Location".into(), "https://accounts.nintendo.com/login?post_login_redirect_uri=x".into())], body: String::new() }) }
        }
        fn get_final(&self, u: &str, h: &[(&str, &str)], t: Duration) -> Result<(Response, String), NetError> {
            assert!(h.iter().any(|(k, v)| *k == "Cookie" && !v.is_empty()));
            if self.page_ok { Ok((Response { status: 200, retry_after: None, body: page() }, format!("{u}/?sort=x"))) } else { Ok((Response { status: 200, retry_after: None, body: String::new() }, "https://accounts.nintendo.com/login".into())) }.map(|x: (Response, String)| { let _ = t; x })
        }
        fn post(&self, u: &str, _h: &[(&str, &str)], body: &str, _t: Duration) -> Result<Response, NetError> {
            let ok = |b: Value| Ok(Response { status: 200, retry_after: None, body: b.to_string() });
            if u.contains("algolia") { return ok(json!({"hits": self.hits})); }
            let off = serde_json::from_str::<Value>(body).unwrap()["variables"]["offset"].as_i64().unwrap();
            self.gql_calls.lock().unwrap().push(off);
            let v = |id: &str, n: &str, rel: bool, dlc: bool| json!({"applicationId": id, "applicationName": n, "publisher": " Pub ", "hasReleasedApplication": rel, "hasReleasedAddOnContents": dlc});
            let views = if off == 0 { vec![v("T1", "Hades\u{2122} for Nintendo Switch", true, false), v("T2", "Some DLC Pack", false, true)] } else { vec![v("T3", "Celeste", true, true)] };
            ok(json!({"data": {"account": {"vgc": {"vgcViewsV2": {"offsetInfo": {"total": 3, "offset": off}, "views": views}}}}}))
        }
    }
    fn tmp(tag: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("zest_nin_{tag}_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); std::fs::write(d.join("config.json"), "{}").unwrap(); d }
    fn src(tag: &str, page_ok: bool, hits: Value) -> (NintendoSource, Arc<Site>, PathBuf) {
        let d = tmp(tag); let s = Arc::new(Site { page_ok, gql_calls: Mutex::new(vec![]), hits });
        (NintendoSource::new(s.clone(), d.clone(), d.join("games_1.db")), s, d)
    }
    fn cx(d: &PathBuf, s: Arc<Site>) -> SourceCtx {
        let h: Arc<dyn Http> = s; SourceCtx { http: h.clone(), steam: Arc::new(crate::steamclient::Steam::new(h, None, 10.0)), root: d.clone(), db: d.join("games_1.db"), today: "2026-11-01".into(), appinfo: Arc::new(|_| Default::default()), account_id: None }
    }

    #[test] fn sign_in_stores_the_session_where_playdate_keeps_it_and_sign_out_removes_it() {
        let (s, _, d) = src("auth", true, json!([]));
        assert_eq!(s.auth_status().unwrap(), AuthStatus { connected: false, username: None });
        assert!(s.auth_complete("nonsense").unwrap_err().contains("Cookie header"));
        let st = s.auth_complete("Cookie: a=1; b=2").unwrap(); assert_eq!(st, AuthStatus { connected: true, username: Some("Zelda Fan".into()) });
        assert_eq!(crate::account::plugin_secret_get(&d, "nintendo", "cookie").as_deref(), Some("a=1; b=2")); assert_eq!(crate::account::plugin_secret_get(&d, "nintendo", "nickname").as_deref(), Some("Zelda Fan"));
        assert!(s.auth_begin().unwrap().url.starts_with("https://accounts.nintendo.com/portal/vgcs"));
        s.auth_disconnect().unwrap(); assert!(!s.auth_status().unwrap().connected); assert!(crate::account::plugin_secret_get(&d, "nintendo", "cookie").is_none());
    }
    #[test] fn a_session_nintendo_rejects_is_not_saved() {
        let (s, _, d) = src("bad", false, json!([]));
        assert!(s.auth_complete("a=1").unwrap_err().contains("did not accept")); assert!(crate::account::plugin_secret_get(&d, "nintendo", "cookie").is_none());
    }
    #[test] fn sync_pages_through_the_list_cleans_names_and_skips_dlc_only_cards() {
        let (s, site, d) = src("sync", true, json!([]));
        assert_eq!(s.sync(&cx(&d, site.clone()), &mut |_| {}, &Cancel::new()).unwrap_err(), "Not connected");
        s.auth_complete("a=1").unwrap();
        let mut got = vec![]; let n = s.sync(&cx(&d, site.clone()), &mut |b| got.extend(b), &Cancel::new()).unwrap();
        assert_eq!(n, 2); assert_eq!(*site.gql_calls.lock().unwrap(), [0, 2], "two pages: offsets 0 and 2");
        assert_eq!((got[0].platform_id.as_str(), got[0].name.as_str(), got[0].publishers.as_deref()), ("T1", "Hades", Some("Pub")));
        assert_eq!(got[1].platform_id, "T3", "the DLC-only card T2 is skipped; a game that also has add-ons stays");
    }
    #[test] fn the_store_lookup_matches_by_name_and_skips_demos_and_dlc() {
        let hits = json!([{"title": "Hades Demo", "url": "/store/products/hades-demo-switch/", "topLevelCategoryCode": "GAMES"}, {"title": "Hades", "url": "/store/products/hades-switch/", "topLevelCategoryCode": "GAMES", "dlcType": "DLC"},
            {"title": "Hades", "url": "/store/products/hades-switch/", "topLevelCategoryCode": "GAMES", "genre": "x"}, {"title": "Hades II", "url": "/store/products/hades-ii/", "topLevelCategoryCode": "GAMES"}]);
        let (s, _, _) = src("store", true, hits);
        assert_eq!(s.find_by_name("Hades").unwrap()["url"], "/store/products/hades-switch/");
        assert!(s.find_by_name("Hades Demo").is_none(), "a demo name never matches");
        assert!(s.find_by_name("Completely Different").is_none());
        assert_eq!(s.find_by_slug("/store/products/hades-ii").unwrap()["title"], "Hades II");
        assert_eq!(s.store_url(&GameRef { slug: "/store/products/x/".into(), ..Default::default() }).unwrap(), "https://www.nintendo.com/store/products/x/");
        assert!(s.store_url(&GameRef::default()).is_none());
    }
    #[test] fn rescrape_without_a_steam_match_takes_the_stores_genres_and_with_one_leaves_them() {
        let hits = json!([{"title": "Celeste", "url": "/store/products/celeste-switch/", "topLevelCategoryCode": "GAMES", "gameGenreLabels": ["Platformer"], "playerCount": "Single player", "softwareDeveloper": "Maddy", "releaseDate": "2018-01-25T00:00:00.000Z"}]);
        let (s, site, d) = src("rescrape", true, hits);
        let db = d.join("games_1.db");
        rusqlite::Connection::open(&db).unwrap().execute_batch("CREATE TABLE games (appid INTEGER PRIMARY KEY, name TEXT, tags TEXT, steam_appid INTEGER, platform_slug TEXT); INSERT INTO games (appid, name) VALUES (-1, 'Celeste'), (-2, 'Celeste'); UPDATE games SET steam_appid = 504230 WHERE appid = -2;").unwrap();
        let g = |a: i64| GameRef { appid: a, name: "Celeste".into(), platform: "nintendo".into(), platform_id: "T3".into(), ..Default::default() };
        let cols = |u: &Update| u.iter().map(|(c, _)| *c).collect::<Vec<_>>();
        let u1 = s.rescrape(&cx(&d, site.clone()), &g(-1)).unwrap().unwrap();
        assert_eq!(cols(&u1), ["genres", "categories", "release_date", "developers", "platform_slug"]);
        let u2 = s.rescrape(&cx(&d, site.clone()), &g(-2)).unwrap().unwrap();
        assert_eq!(cols(&u2), ["release_date", "developers", "platform_slug"], "genres and categories left to the Steam data");
    }

    #[test] fn a_page_read_in_the_browser_lists_the_same_games_without_any_cookie() {
        let (s, site, d) = src("pushed", true, json!([]));
        let mut synced = vec![]; s.auth_complete("a=1").unwrap(); s.sync(&cx(&d, site.clone()), &mut |b| synced.extend(b), &Cancel::new()).unwrap();
        let (p, _, _) = src("pushed2", true, json!([]));
        let blocks = json!({"data": {"idToken": "TOK", "savannaClientId": "CID", "shopGraphQLApiUrl": "https://shop.example/graphql"},
            "state": {"lang": "en-US", "user": {"nickname": "Zelda Fan", "countryId": 1}}, "meta": {"countries": [{"id": 1, "code": "US"}]}});
        assert_eq!(p.import_pushed(&blocks).unwrap(), synced);
        assert!(p.cookie().is_none(), "nothing is stored as a session");
        assert!(p.import_pushed(&json!({"data": {}, "state": {}, "meta": {}})).is_err(), "a signed-out page has no idToken");
        assert!(p.import_pushed(&json!({})).is_err());
    }
}
