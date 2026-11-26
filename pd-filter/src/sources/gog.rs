//! GOG as a library source (port of PlayDate's `gog` plugin: `gog.py`, `__init__.py` and the sign-in route in `routes.py`; the listing half, install and launch wait for the
//! shared runners layer). GOG Galaxy's own OAuth2 client (the public id and secret every third-party client uses) signs the user in: the browser shows a login page that ends on an
//! `on_login_success` address carrying `code=`, which the user pastes. The library comes from `getFilteredProducts`, metadata and ratings from GOG's public catalogue,
//! purchase dates from the order history. Tokens live in `config.json["gog"]` exactly as PlayDate keeps them (numbers stay numbers), and are never logged.
use super::{Action, AuthBegin, AuthStatus, Caps, GameRef, Installed, InstalledRow, Launch, LibrarySource, SourceCtx};
use crate::importer::GameRecord;
use crate::jobs::Update;
use crate::net::{enc, log, Cancel, Http};
use crate::steamclient::Fetch;
use regex::Regex;
use rusqlite::types::Value as Sql;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

const CLIENT_ID: &str = "46899977096215655";
const CLIENT_SECRET: &str = "9d85c43b1482497dbbce61f6e4aa173a433796eeae2ca8c5f6129f2dc4de46d9";
const AUTH_URL: &str = "https://auth.gog.com/auth";
const TOKEN_URL: &str = "https://auth.gog.com/token";
const REDIRECT: &str = "https://embed.gog.com/on_login_success?origin=client";
const UA: &str = "GOGGalaxyClient/2.0.45.189 (Windows; 10)";
const PRODUCTS: &str = "https://embed.gog.com/account/getFilteredProducts";
const USER_DATA: &str = "https://embed.gog.com/userData.json";
const V2_GAME: &str = "https://api.gog.com/v2/games";
const RATINGS: &str = "https://reviews.gog.com/v1/products";
const ACHIEVEMENTS: &str = "https://gameplay.gog.com/clients";
const GAMESDB: &str = "https://gamesdb.gog.com/platforms/gog/external_releases";
const ORDERS: &str = "https://embed.gog.com/account/settings/orders/data?canceled=0&completed=1&in_progress=1&not_redeemed=1&pending=1&redeemed=1&page=";
const RECONNECT: &str = "Not connected to GOG \u{2014} please reconnect";
const EXPIRED: &str = "GOG session expired \u{2014} please reconnect";

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex { cell.get_or_init(|| Regex::new(pat).unwrap()) }
fn pause(ms: u64) { if !cfg!(test) { std::thread::sleep(Duration::from_millis(ms)); } }
fn truthy(v: Option<&Value>) -> bool { match v { None | Some(Value::Null) => false, Some(Value::Bool(b)) => *b, Some(Value::String(s)) => !s.is_empty(), Some(Value::Number(n)) => n.as_f64() != Some(0.0), Some(Value::Array(a)) => !a.is_empty(), Some(Value::Object(o)) => !o.is_empty() } }
/// Python's `str(x)` for a JSON id.
fn id_str(v: Option<&Value>) -> String { match v { Some(Value::String(s)) => s.clone(), Some(Value::Number(n)) => n.to_string(), Some(Value::Bool(b)) => if *b { "True".into() } else { "False".into() }, _ => "None".into() } }
fn round_even(x: f64) -> i64 { x.round_ties_even() as i64 }
/// Python's `int(x)` / `float(x)` on a JSON value that may be a number or numeric text; None when Python would raise.
fn to_f64(v: &Value) -> Option<f64> { match v { Value::Number(n) => n.as_f64(), Value::String(s) => s.trim().parse::<f64>().ok(), Value::Bool(b) => Some(*b as i64 as f64), _ => None } }
fn to_i64(v: &Value) -> Option<i64> { match v { Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f.trunc() as i64)), Value::String(s) => s.trim().parse::<i64>().ok(), Value::Bool(b) => Some(*b as i64), _ => None } }

// ---------------------------------------------------------------- pure helpers

/// The sign-in box: a pasted `on_login_success?...&code=` address, or the bare code. (`routes.py`.)
pub fn parse_code(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    if raw.is_empty() { return Err("code is required".into()); }
    if raw.starts_with("http") {
        let q = raw.split_once('?').map(|(_, q)| q.split('#').next().unwrap_or("")).unwrap_or("");
        let code = q.split('&').filter_map(|kv| kv.split_once('=')).filter(|(k, v)| *k == "code" && !v.is_empty()).map(|(_, v)| unquote(v)).next().unwrap_or_default();
        let code = code.trim().to_string();
        if code.is_empty() { return Err("No code= found in that URL".into()); }
        Ok(code)
    } else { Ok(raw.to_string()) }
}
pub(crate) fn unquote(s: &str) -> String {
    let b = s.as_bytes(); let mut out = Vec::with_capacity(b.len()); let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => { out.push(b' '); i += 1; }
            b'%' if i + 2 < b.len() && b[i + 1].is_ascii_hexdigit() && b[i + 2].is_ascii_hexdigit() => { out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap_or(b'?')); i += 3; }
            c => { out.push(c); i += 1; }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `_norm_title`: lower case letters and digits only, "&" read as "and", a trailing " - Amazon ..." dropped.
pub fn norm_title(t: &str) -> String {
    static A: OnceLock<Regex> = OnceLock::new(); static N: OnceLock<Regex> = OnceLock::new();
    let t = re(&A, r"(?i)\s[-\u{2013}\u{2014}]\s*amazon\b.*$").replace(t, "").into_owned();
    re(&N, r"[^a-z0-9]+").replace_all(&t.to_lowercase().replace('&', "and"), "").into_owned()
}

/// `_GOODIES_NAME_RE`: GOG sometimes files bonus content under category GAME.
pub fn is_goodies(name: &str) -> bool { static G: OnceLock<Regex> = OnceLock::new(); re(&G, r"(?i)\bgoodies?\s*(?:collection|pack)\b|\bdigital\s+goodies\b").is_match(name) }
/// The sync's keep/skip rule: only an explicit category other than GAME, or a goodies pack filed as GAME, is not a game (a product the catalogue does not know is kept).
pub fn is_non_game(category: Option<&str>, name: &str) -> bool { match category { Some(c) if c != "GAME" => true, Some(_) => is_goodies(name), None => false } }

/// `datetime.fromisoformat(str(x).replace('Z', '+00:00'))` as a Unix time; a value without a zone is read as UTC. None when Python would raise.
pub fn release_ts(text: &str) -> Option<i64> {
    static T: OnceLock<Regex> = OnceLock::new();
    let t = text.replace('Z', "+00:00");
    let c = re(&T, r"^(\d{4})-(\d{2})-(\d{2})(?:[T ](\d{2})(?::(\d{2})(?::(\d{2})(?:[.,]\d{1,6})?)?)?)?(?:([+-])(\d{2})(?::?(\d{2}))?)?$").captures(&t)?;
    let n = |i: usize| c.get(i).map_or(0, |m| m.as_str().parse::<i64>().unwrap_or(0));
    let (y, mo, d) = (n(1), n(2), n(3));
    let dim = match mo { 1 | 3 | 5 | 7 | 8 | 10 | 12 => 31, 4 | 6 | 9 | 11 => 30, 2 => if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 { 29 } else { 28 }, _ => return None };
    if y < 1 || d < 1 || d > dim || n(4) > 23 || n(5) > 59 || n(6) > 59 || n(9) > 59 || n(8) > 23 { return None; }
    let off = c.get(7).map_or(0, |m| (if m.as_str() == "-" { -1 } else { 1 }) * (n(8) * 3600 + n(9) * 60));
    let (yy, mm) = if mo <= 2 { (y - 1, mo + 9) } else { (y, mo - 3) };
    let era = yy.div_euclid(400); let yoe = yy - era * 400; let doy = (153 * mm + 2) / 5 + d - 1; let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146097 + doe - 719468) * 86400 + n(4) * 3600 + n(5) * 60 + n(6) - off)
}

fn review_score_label(percent: i64, total: i64) -> &'static str {
    if total == 0 { return "No Reviews"; } if total < 10 { return "Not Enough Reviews"; }
    if percent >= 95 && total >= 500 { "Overwhelmingly Positive" } else if percent >= 80 && total >= 50 { "Very Positive" } else if percent >= 80 { "Positive" }
    else if percent >= 70 { "Mostly Positive" } else if percent >= 40 { "Mixed" } else if percent >= 20 { "Mostly Negative" }
    else if total >= 500 { "Overwhelmingly Negative" } else if total >= 50 { "Very Negative" } else { "Negative" }
}

fn names(v: Option<&Value>) -> Vec<String> { v.and_then(|a| a.as_array()).into_iter().flatten().filter_map(|x| x.get("name").and_then(|n| n.as_str()).filter(|n| !n.is_empty()).map(String::from)).collect() }

/// `fetch_gog_metadata`'s reading of the two answers: (category, columns). `v2` is the catalogue entry; `ratings` the average rating answer (None = it failed).
/// `category` is None when the catalogue has no such product (a 404 is `Value::Null` here); columns are what is known.
pub fn metadata_cols(v2: &Value, ratings: Option<&Value>, half_trust: f64) -> (Option<String>, Vec<(&'static str, Sql)>) {
    let mut out: Vec<(&'static str, Sql)> = vec![];
    let emb = v2.get("_embedded").filter(|e| e.is_object()).cloned().unwrap_or(Value::Null);
    let product = emb.get("product").cloned().unwrap_or(Value::Null);
    let href = v2.pointer("/_links/store/href").and_then(|h| h.as_str()).unwrap_or("");
    if href.contains("/game/") { out.push(("platform_slug", Sql::Text(href.rsplit("/game/").next().unwrap_or("").trim_end_matches('/').to_string()))); }
    let category = product.get("category").and_then(|c| c.as_str()).map(String::from);
    let devs = names(emb.get("developers")); if !devs.is_empty() { out.push(("developers", Sql::Text(devs.join(",")))); }
    let pubs = emb.get("publisher").and_then(|p| p.get("name")).and_then(|n| n.as_str()).map(str::trim).unwrap_or(""); if !pubs.is_empty() { out.push(("publishers", Sql::Text(pubs.to_string()))); }
    let rd = ["globalReleaseDate", "gogReleaseDate"].iter().filter_map(|k| product.get(*k)).find(|v| truthy(Some(v)));
    if let Some(rd) = rd { let text = match rd { Value::String(s) => s.clone(), other => other.to_string() }; if let Some(ts) = release_ts(&text) { out.push(("release_date", Sql::Integer(ts))); } }
    let genres = names(emb.get("tags")); if !genres.is_empty() { out.push(("genres", Sql::Text(genres.join(",")))); }
    let props = names(emb.get("properties")); if !props.is_empty() { out.push(("tags", Sql::Text(props.join(",")))); }
    if let Some(r) = ratings.filter(|r| r.is_object()) {
        let value = r.get("value").filter(|v| truthy(Some(v))).map_or(Some(0.0), to_f64);
        let count = r.get("count").filter(|v| truthy(Some(v))).map_or(Some(0), to_i64);
        if let (Some(value), Some(count)) = (value, count) {
            let percent = round_even(value * 20.0);
            let weighted = if count == 0 { 0 } else { let p = percent as f64 / 100.0; let ht = half_trust.max(2.0); round_even((p - (p - 0.5) * 2f64.powf(-((count + 1) as f64).ln() / ht.ln())) * 100.0) };
            out.push(("review_percentage", Sql::Integer(percent))); out.push(("weighted_percentage", Sql::Integer(weighted))); out.push(("total_reviews", Sql::Integer(count)));
            out.push(("positive_reviews", Sql::Integer(round_even(percent as f64 / 100.0 * count as f64)))); out.push(("review_score", Sql::Text(review_score_label(percent, count).into())));
        }
    }
    (category, out)
}

/// `fetch_gog_achievements`' reading: (unlocked, total). A 404 is a game without achievements; any other failure is None.
pub fn achievements_from(status: u16, body: Option<&Value>) -> Option<(i64, i64)> {
    if status == 404 { return Some((0, 0)); }
    if status >= 400 { return None; }
    let body = body?.as_object()?;
    let total = match body.get("total_count") { Some(v) if truthy(Some(v)) => to_i64(v)?, _ => 0 };
    let unlocked = body.get("items").and_then(|i| i.as_array()).map_or(0, |a| a.iter().filter(|i| truthy(i.get("date_unlocked"))).count() as i64);
    Some((unlocked, total))
}

/// `fetch_purchase_dates`' reading of the order pages: the earliest purchase time and the title per product id. Refunded orders and products are skipped.
pub fn purchase_dates(pages: &[Value]) -> (BTreeMap<String, i64>, BTreeMap<String, String>) {
    let (mut dates, mut titles): (BTreeMap<String, i64>, BTreeMap<String, String>) = Default::default();
    for page in pages {
        for order in page.get("orders").and_then(|o| o.as_array()).into_iter().flatten() {
            let ts = match order.get("date") { Some(v) if truthy(Some(v)) => v, _ => continue };
            if order.get("status").and_then(|s| s.as_str()) == Some("refund") { continue; }
            let Some(ts_i) = to_i64(ts) else { continue };
            for prod in order.get("products").and_then(|p| p.as_array()).into_iter().flatten() {
                if truthy(prod.get("isRefunded")) { continue; }
                let gid = id_str(prod.get("id"));
                if dates.get(&gid).map_or(true, |d| ts_i < *d) { dates.insert(gid.clone(), ts_i); }
                let title = prod.get("title").and_then(|t| t.as_str()).filter(|t| !t.is_empty()).map(String::from).or_else(|| titles.get(&gid).cloned()).unwrap_or_default();
                titles.insert(gid, title);
            }
        }
    }
    (dates, titles)
}

/// `_pack_contents`: the ids of the games a pack or bundle includes, from the catalogue entry.
pub fn pack_contents(v2: &Value) -> Vec<String> {
    static G: OnceLock<Regex> = OnceLock::new();
    v2.pointer("/_links/includesGames").and_then(|l| l.as_array()).into_iter().flatten()
        .filter_map(|l| re(&G, r"/games/(\d+)").captures(l.get("href").and_then(|h| h.as_str()).unwrap_or("")).map(|m| m[1].to_string())).collect()
}

/// `art_urls`: GOG's own vertical cover (the Galaxy size), from the public game database.
pub fn art_url(body: &Value) -> Option<String> {
    let fmt = body.pointer("/game/vertical_cover/url_format")?.as_str()?;
    fmt.contains("{formatter}").then(|| fmt.replace("{formatter}", "_glx_vertical_cover").replace("{ext}", "jpg"))
}

/// `fetch_description`: the lead (else the full) text with tags removed and spaces collapsed.
pub fn description_from(body: &Value) -> Option<String> {
    static TAG: OnceLock<Regex> = OnceLock::new(); static WS: OnceLock<Regex> = OnceLock::new();
    let d = body.get("description").filter(|d| truthy(Some(d))).and_then(|d| d.as_object())?;
    let lead = d.get("lead").filter(|v| truthy(Some(v)));
    let text = match lead.or_else(|| d.get("full")) { Some(Value::String(s)) => s.clone(), Some(Value::Null) | None => String::new(), _ => return None };
    let t = re(&TAG, r"<[^>]+>").replace_all(&text, " ");
    let t = re(&WS, r"\s+").replace_all(&t, " ").trim().to_string();
    (!t.is_empty()).then_some(t)
}

/// `_fetch_username`: (username, galaxy user id) from `userData.json`.
pub fn username_from(body: &Value) -> (Option<String>, Option<String>) {
    let s = |k: &str| body.get(k).filter(|v| truthy(Some(v))).map(|v| match v { Value::String(s) => s.clone(), o => o.to_string() });
    (s("username").or_else(|| s("galaxyUserId")), s("galaxyUserId"))
}

// ---------------------------------------------------------------- the source

enum GogErr { Auth(String), Other(String) }

/// A program on PATH (DOSBox for the old games).
fn which(name: &str) -> Option<PathBuf> { std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join(name)).find(|f| f.is_file())) }

pub struct GogSource { http: Arc<dyn Http>, config_dir: PathBuf, db: PathBuf }

impl GogSource {
    pub fn new(http: Arc<dyn Http>, config_dir: PathBuf, db: PathBuf) -> Self { GogSource { http, config_dir, db } }
    /// `launch` with the program lookup supplied (tests choose what is "installed").
    fn launch_with(&self, g: &GameRef, which: &dyn Fn(&str) -> Option<PathBuf>) -> Result<Launch, String> {
        use super::gog_launch as gl;

        if !g.installed { return Err(format!("{} is not installed yet.", g.name)); }
        let dir = g.install_path.as_deref().map(std::path::Path::new).filter(|d| d.is_dir()).ok_or("Game folder not found \u{2014} it may have been moved or deleted.")?;
        match gl::dosbox_launch(dir, &g.platform_id, which) {
            gl::Dosbox::Run { bin, args, cwd } => return Ok(Launch { action: Action::Spawn { program: bin.to_string_lossy().to_string(), args, dir: Some(cwd.to_string_lossy().to_string()), env: vec![] }, record: true }),
            gl::Dosbox::Missing => return Err("This is a DOSBox game and DOSBox is not installed. Install dosbox, dosbox-staging or dosbox-x.".into()),
            gl::Dosbox::NotDosbox => {}
        }
        let run = |abs: &std::path::Path, cwd: &std::path::Path| super::folder::run_file(abs, cwd, Some(&dir.join("prefix")));
        let known = g.executable.clone().filter(|r| dir.join(r).is_file()).or_else(|| match gl::find_executable(dir, &g.platform_id, which) { gl::GogExe::File(f) => Some(f), _ => None });
        if let Some(rel) = known { let abs = dir.join(&rel); let cwd = abs.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| dir.to_path_buf()); return run(&abs, &cwd); }
        // no usable goggame-*.info (or nothing it names exists): the shared scan, which also knows Windows programs; a tie between equally good files is asked about
        match super::folder::choose_executable(dir, None) {
            super::folder::Choice::Nothing => Err(format!("No supported executable found in {}.", g.name)),
            c => super::folder::launch_choice(dir, c, &run),
        }
    }
    fn dir(&self) -> PathBuf { crate::installdir::games_dir(&self.config_dir, "gog", crate::installdir::default_dir("GOG", "GOG")) }
    fn section(&self) -> Map<String, Value> { crate::account::plugin_section(&self.config_dir, "gog") }
    /// Edits `config.json["gog"]` keeping every other key; `None` removes the whole section.
    fn edit(&self, f: impl FnOnce(&mut Map<String, Value>)) {
        crate::account::plugin_section_edit(&self.config_dir, "gog", f);   // secrets go to the keyring, the rest stays in config.json
    }
    fn now() -> i64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0) }
    fn galaxy_id(&self) -> Option<String> { match self.section().get("galaxy_user_id") { Some(Value::String(s)) if !s.is_empty() => Some(s.clone()), Some(Value::Number(n)) => Some(n.to_string()), _ => None } }
    fn has_tokens(&self) -> bool { let s = self.section(); ["access_token", "refresh_token"].iter().all(|k| s.get(*k).and_then(|v| v.as_str()).map_or(false, |v| !v.is_empty())) }

    fn headers<'a>(extra: &[(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> { let mut h = vec![("User-Agent", UA), ("Accept", "application/json")]; h.extend_from_slice(extra); h }
    fn token_request(&self, fields: &[(&str, &str)]) -> Result<Value, GogErr> {
        let mut all: Vec<(&str, &str)> = vec![("client_id", CLIENT_ID), ("client_secret", CLIENT_SECRET)]; all.extend_from_slice(fields);
        let body = all.iter().map(|(k, v)| format!("{}={}", enc(k), enc(v))).collect::<Vec<_>>().join("&");
        let r = self.http.post(TOKEN_URL, &Self::headers(&[("Content-Type", "application/x-www-form-urlencoded")]), &body, Duration::from_secs(15)).map_err(|e| { log(&format!("GOG: {e}")); GogErr::Other("Could not reach GOG.".into()) })?;
        if r.status != 200 { log(&format!("GOG: token request answered HTTP {}", r.status)); return Err(GogErr::Auth(format!("Token exchange failed (HTTP {})", r.status))); }
        let j: Value = serde_json::from_str(&r.body).map_err(|_| GogErr::Other("Unexpected token response".into()))?;
        for k in ["access_token", "refresh_token"] { if j.get(k).and_then(|v| v.as_str()).map_or(true, str::is_empty) && !(k == "refresh_token" && fields.iter().any(|f| f.0 == "grant_type" && f.1 == "refresh_token")) { return Err(GogErr::Other(format!("Unexpected token response (missing '{k}')"))); } }
        Ok(j)
    }
    fn store_tokens(&self, j: &Value) {
        let expires = Self::now() + j.get("expires_in").and_then(to_i64).unwrap_or(3600);
        let (at, rt) = (j["access_token"].as_str().unwrap_or("").to_string(), j.get("refresh_token").and_then(|v| v.as_str()).map(String::from));
        self.edit(|m| { m.insert("access_token".into(), json!(at)); if let Some(rt) = rt { m.insert("refresh_token".into(), json!(rt)); } m.insert("expires_at".into(), json!(expires)); });
    }
    /// A valid access token, refreshed when it expires within a minute.
    fn access_token(&self) -> Result<String, GogErr> {
        let s = self.section();
        let (Some(at), Some(rt)) = (s.get("access_token").and_then(|v| v.as_str()).filter(|v| !v.is_empty()), s.get("refresh_token").and_then(|v| v.as_str()).filter(|v| !v.is_empty())) else { return Err(GogErr::Auth(RECONNECT.into())) };
        if Self::now() < s.get("expires_at").and_then(to_i64).unwrap_or(0) - 60 { return Ok(at.to_string()); }
        match self.token_request(&[("grant_type", "refresh_token"), ("refresh_token", rt)]) {
            Ok(j) => { self.store_tokens(&j); Ok(j["access_token"].as_str().unwrap_or("").to_string()) }
            Err(GogErr::Other(m)) => { log(&format!("GOG: token refresh failed ({m})")); Err(GogErr::Other(m)) }
            Err(GogErr::Auth(_)) => Err(GogErr::Auth(RECONNECT.into())),
        }
    }
    fn get(&self, url: &str, token: Option<&str>, secs: u64) -> Result<crate::net::Response, String> {
        let bearer = token.map(|t| format!("Bearer {t}"));
        let mut extra: Vec<(&str, &str)> = vec![]; if let Some(b) = &bearer { extra.push(("Authorization", b)); }
        self.http.get(url, &Self::headers(&extra), Duration::from_secs(secs)).map_err(|e| e.to_string())
    }
    fn json(r: &crate::net::Response) -> Option<Value> { serde_json::from_str(&r.body).ok() }

    /// `fetch_gog_metadata`: (category, columns), or None when the catalogue could not be asked at all. A catalogue 404 is a product with no category.
    pub fn fetch_metadata(&self, gog_id: &str, half_trust: f64) -> Option<(Option<String>, Vec<(&'static str, Sql)>)> {
        let r = match self.get(&format!("{V2_GAME}/{gog_id}"), None, 15) { Ok(r) => r, Err(e) => { log(&format!("GOG metadata: v2 fetch failed for {gog_id}: {e}")); return None } };
        if r.status == 404 { log(&format!("GOG metadata: {gog_id} not in v2 catalog (404)")); return Some((None, vec![])); }
        if r.status >= 400 { log(&format!("GOG metadata: v2 answered HTTP {} for {gog_id}", r.status)); return None; }
        let v2 = Self::json(&r)?;
        let ratings = self.get(&format!("{RATINGS}/{gog_id}/averageRating"), None, 10).ok().filter(|r| r.status == 200).and_then(|r| Self::json(&r));
        Some(metadata_cols(&v2, ratings.as_ref(), half_trust))
    }
    fn fetch_achievements(&self, gog_id: &str, galaxy_id: &str, token: &str) -> Option<(i64, i64)> {
        let r = self.get(&format!("{ACHIEVEMENTS}/{gog_id}/users/{galaxy_id}/achievements"), Some(token), 15).ok()?;
        achievements_from(r.status, Self::json(&r).as_ref())
    }
    fn half_trust(root: &std::path::Path) -> f64 { std::fs::read_to_string(root.join("state.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|s| s.get("review_half_trust_count").and_then(to_f64)).unwrap_or(10.0) }

    /// `fetch_purchase_dates` plus `import_purchase_dates`' matching: id first, then a pack's contents, then the normalised title.
    fn dates_for(&self, token: &str, games: &[(String, String)], cancel: &Cancel) -> BTreeMap<String, i64> {
        let mut pages: Vec<Value> = vec![]; let (mut page, mut total) = (1usize, 1usize);
        while page <= total {
            if cancel.is_set() { return Default::default(); }
            let mut got = None;
            for attempt in 0..3 {
                match self.get(&format!("{ORDERS}{page}"), Some(token), 20) {
                    Ok(r) if r.status < 400 => if let Some(v) = Self::json(&r) { got = Some(v); break },
                    Ok(r) => log(&format!("GOG orders page {page} attempt {}: HTTP {}", attempt + 1, r.status)),
                    Err(e) => log(&format!("GOG orders page {page} attempt {}: {e}", attempt + 1)),
                }
                pause(1500);
            }
            let Some(v) = got else { return Default::default() };
            total = v.get("totalPages").filter(|t| truthy(Some(t))).and_then(to_i64).unwrap_or(1).max(1) as usize;
            pages.push(v); page += 1; pause(300);
        }
        let (mut dates, titles) = purchase_dates(&pages);
        let lib: HashSet<&str> = games.iter().map(|(id, _)| id.as_str()).collect();
        if games.iter().any(|(id, _)| !dates.contains_key(id)) {
            crate::net::set_activity("GOG: reading bundle contents");
            let orphans: Vec<(String, i64)> = dates.iter().filter(|(k, _)| !lib.contains(k.as_str())).map(|(k, v)| (k.clone(), *v)).collect();
            for (pid, ts) in orphans {
                if cancel.is_set() { break; }
                let Ok(r) = self.get(&format!("{V2_GAME}/{pid}"), None, 15) else { continue };
                if r.status != 200 { continue; }
                for gid in Self::json(&r).map(|v| pack_contents(&v)).unwrap_or_default() { if dates.get(&gid).map_or(true, |d| ts < *d) { dates.insert(gid, ts); } }
                pause(200);
            }
        }
        let mut by_title: BTreeMap<String, i64> = BTreeMap::new();
        for (gid, ts) in &dates { let key = norm_title(titles.get(gid).map_or("", |t| t.as_str())); if !key.is_empty() && by_title.get(&key).map_or(true, |d| ts < d) { by_title.insert(key, *ts); } }
        games.iter().filter_map(|(id, name)| dates.get(id).copied().or_else(|| by_title.get(&norm_title(name)).copied()).map(|ts| (id.clone(), ts))).collect()
    }

    fn ids(&self) -> (HashSet<String>, HashSet<String>) {
        let Ok(c) = rusqlite::Connection::open_with_flags(&self.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { return Default::default() };
        let col = |sql: &str| -> HashSet<String> { c.prepare(sql).ok().map(|mut s| s.query_map([], |r| r.get::<_, Option<String>>(0)).map(|it| it.flatten().flatten().collect()).unwrap_or_default()).unwrap_or_default() };
        (col("SELECT platform_id FROM games WHERE platform = 'gog'"), col("SELECT platform_id FROM blacklist WHERE platform_id IS NOT NULL"))
    }
    /// Products the catalogue called something other than a game; remembered so the next sync does not ask again (delete `gog_nongames.json` to forget).
    fn nongames_path(&self) -> PathBuf { self.config_dir.join("gog_nongames.json") }
    fn load_nongames(&self) -> HashSet<String> { std::fs::read_to_string(self.nongames_path()).ok().and_then(|t| serde_json::from_str::<Vec<String>>(&t).ok()).unwrap_or_default().into_iter().collect() }
    fn save_nongames(&self, s: &HashSet<String>) { let mut v: Vec<&String> = s.iter().collect(); v.sort(); let _ = std::fs::write(self.nongames_path(), serde_json::to_string(&v).unwrap_or_default()); }
}

impl LibrarySource for GogSource {
    fn id(&self) -> &str { "gog" }
    fn label(&self) -> &str { "GOG" }
    fn caps(&self) -> Caps { Caps { auth: true, sync: true, rescrape: true, description: true, store_page: true, purchase_dates: true, art: true, install: true, launch: true, uninstall: true, installed_scan: true, ..Default::default() } }
    fn purchase_dates(&self, cx: &SourceCtx, appids: &[i64]) -> Result<Vec<(i64, Option<i64>)>, String> {
        let token = match self.access_token() { Ok(t) => t, Err(GogErr::Auth(m)) | Err(GogErr::Other(m)) => return Err(m) };
        let rows = super::platform_rows(&cx.db, appids);
        let names: std::collections::HashMap<i64, String> = rusqlite::Connection::open_with_flags(&cx.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok().map(|c| appids.iter().map(|id| (*id, c.query_row("SELECT COALESCE(name,'') FROM games WHERE appid = ?", [id], |r| r.get::<_, String>(0)).unwrap_or_default())).collect()).unwrap_or_default();
        let list: Vec<(String, String)> = rows.iter().map(|(id, pid, _)| (pid.clone(), names.get(id).cloned().unwrap_or_default())).collect();
        crate::net::set_activity("GOG: reading your order history for purchase dates");
        let dates = self.dates_for(&token, &list, &Cancel::new());
        crate::net::set_activity("");
        if dates.is_empty() && !list.is_empty() { return Err("Could not read your GOG order history. Reconnect GOG and try again.".into()); }
        Ok(rows.into_iter().map(|(id, pid, _)| (id, dates.get(&pid).copied())).collect())
    }
    fn art_urls(&self, g: &GameRef) -> Vec<(String, String)> { Self::art_urls(self, &g.platform_id).into_iter().map(|(k, v)| (k.to_string(), v)).collect() }
    fn store_url(&self, g: &GameRef) -> Option<String> { (!g.slug.is_empty()).then(|| format!("https://www.gog.com/en/game/{}", g.slug)) }
    fn enrich_new(&self) -> bool { true }

    fn games_dir(&self) -> Option<PathBuf> { Some(self.dir()) }
    fn installed_scan(&self) -> Option<Vec<InstalledRow>> {
        let c = rusqlite::Connection::open_with_flags(&self.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
        let mut st = c.prepare("SELECT platform_id, COALESCE(install_path, '') FROM games WHERE platform = 'gog' AND platform_id IS NOT NULL AND install_path IS NOT NULL AND install_path != ''").ok()?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).ok()?.flatten()
            .map(|(id, path)| InstalledRow { installed: std::path::Path::new(&path).is_dir(), platform_id: id, install_path: None }).collect();
        Some(rows)
    }
    fn install(&self, _cx: &SourceCtx, g: &GameRef, progress: &mut dyn FnMut(&str, u64, u64), cancel: &Cancel) -> Result<Installed, String> {
        use super::gog_install as gi;
        let token = match self.access_token() { Ok(t) => t, Err(GogErr::Auth(m)) | Err(GogErr::Other(m)) => return Err(m) };
        let bearer = format!("Bearer {token}");
        let fetch = |url: &str| -> Result<Vec<u8>, String> {
            let (st, body) = self.http.get_bytes(url, &Self::headers(&[("Authorization", &bearer)]), Duration::from_secs(60)).map_err(|e| e.to_string())?;
            if st >= 400 { return Err(format!("HTTP {st}")); }
            Ok(body)
        };
        let json_of = |url: &str, secs: u64| -> Result<(u16, Value), String> { let r = self.get(url, Some(&token), secs)?; let st = r.status; Ok((st, serde_json::from_str(&r.body).unwrap_or(Value::Null))) };
        progress(&format!("{}: asking GOG for the download", g.name), 0, 0);
        let (mut chosen, mut build) = (String::new(), None::<Value>);
        for os in gi::os_order("auto", cfg!(windows)) {
            match json_of(&format!("{}/products/{}/os/{os}/builds?generation=2", gi::CS_BASE, g.platform_id), 15) {
                Ok((200, v)) => if let Some(b) = v.get("items").and_then(|i| i.as_array()).and_then(|a| a.first()).cloned() { chosen = os.to_string(); build = Some(b); break },
                Ok((st, _)) if st != 404 => log(&format!("GOG install: builds for {os} answered HTTP {st}")),
                Err(e) => log(&format!("GOG install: builds for {os}: {e}")),
                _ => {}
            }
        }
        let build = build.ok_or_else(|| format!("No downloadable builds found for {:?}", g.name))?;
        let (st, links) = json_of(&format!("{}/products/{}/secure_link?generation=2&path=/&_version=2", gi::CS_BASE, g.platform_id), 15).map_err(|e| format!("CDN token acquisition failed: {e}"))?;
        let cdn = links.get("urls").filter(|u| u.as_array().map_or(false, |a| !a.is_empty())).cloned().ok_or_else(|| format!("CDN token acquisition failed: HTTP {st}, no download addresses"))?;
        let link = build.get("link").and_then(|l| l.as_str()).ok_or("Meta manifest failed: the build has no link")?;
        let meta = fetch(link).and_then(|b| gi::parse_manifest(&b)).map_err(|e| format!("Meta manifest failed: {e}"))?;
        let dir = self.dir().join(gi::install_dir_name(&meta, &g.name));
        progress(&format!("{}: reading the file list", g.name), 0, 0);
        let (files, total) = gi::collect_files(&meta, &fetch, &mut |m| log(m));
        log(&format!("GOG install: {} files, {:.1} MB, {chosen} build of {:?}", files.len(), total as f64 / 1e6, g.name));
        if files.is_empty() { return Err("No files found in depot manifests".into()); }
        crate::installdir::check_writable(&dir)?;
        crate::installdir::check_disk_space(&dir, total)?;
        let name = g.name.clone();
        let r = gi::download_files(&dir, &files, total, &cdn, &fetch, &|| cancel.is_set(), &mut |d, t| progress(&format!("Downloading {name}: {} of {} MB", super::folder::mb(d), super::folder::mb(t)), d, t));
        if let Err(e) = r { let _ = std::fs::remove_dir_all(&dir); return Err(e); }
        let exe = match super::gog_launch::find_executable(&dir, &g.platform_id, &|n| which(n)) { super::gog_launch::GogExe::File(f) => Some(f), _ => None };
        log(&format!("GOG install complete: {:?} -> {} (exe: {exe:?})", g.name, dir.display()));
        Ok(Installed { install_path: dir.to_string_lossy().to_string(), executable: exe, note: None })
    }
    fn launch(&self, g: &GameRef) -> Result<Launch, String> { self.launch_with(g, &which) }
    fn uninstall(&self, g: &GameRef) -> Result<Action, String> {
        // Windows: a GOG game has its own uninstaller (unins000.exe) registered; running it removes the game properly, and only without one are the files removed here
        if cfg!(windows) { if let Ok(a) = crate::winuninstall::action(g) { return Ok(a); } }
        let dir = g.install_path.as_deref().filter(|p| !p.is_empty()).ok_or_else(|| format!("{} is not installed.", g.name))?;
        Ok(Action::RemoveInstall { delete: crate::installdir::is_inside(std::path::Path::new(dir), &self.dir()), dir: dir.to_string() })
    }
    fn auth_status(&self) -> Result<AuthStatus, String> {
        if !self.has_tokens() { return Ok(AuthStatus { connected: false, username: None }); }
        let name = self.section().get("username").and_then(|v| v.as_str()).filter(|v| !v.is_empty()).map(String::from);
        Ok(AuthStatus { connected: true, username: Some(name.unwrap_or_else(|| "GOG Account".into())) })
    }
    fn web_login_done(&self) -> Option<serde_json::Value> { Some(serde_json::json!({"kind": "url", "contains": "on_login_success?origin=client&code="})) }
    fn paste_login_kept(&self) -> bool { false }   // the sign-in window works (confirmed live 2026-11-22)
    fn auth_begin(&self) -> Result<AuthBegin, String> {
        let url = format!("{AUTH_URL}?client_id={CLIENT_ID}&redirect_uri={}&response_type=code&layout=client2", enc(REDIRECT));
        Ok(AuthBegin { url, placeholder: "https://embed.gog.com/on_login_success?origin=client&code=...".into(), instructions: vec![
            "Open the GOG login page, which opens in your browser, and log in to your GOG account.".into(),
            "After you log in the browser lands on a page that may look blank or broken. Copy the whole address from the address bar (it contains code=) and paste it below.".into()] })
    }
    fn auth_complete(&self, value: &str) -> Result<AuthStatus, String> {
        let code = parse_code(value)?;
        let j = self.token_request(&[("grant_type", "authorization_code"), ("code", &code), ("redirect_uri", REDIRECT)]).map_err(|e| match e { GogErr::Auth(m) | GogErr::Other(m) => m })?;
        self.store_tokens(&j);
        let at = j["access_token"].as_str().unwrap_or("").to_string();
        let (name, galaxy) = match self.get(USER_DATA, Some(&at), 10) { Ok(r) if r.status < 400 => Self::json(&r).map(|b| username_from(&b)).unwrap_or((None, None)), _ => (None, None) };
        self.edit(|m| { m.insert("username".into(), name.clone().map_or(Value::Null, Value::from)); m.insert("galaxy_user_id".into(), galaxy.clone().map_or(Value::Null, Value::from)); });
        log("GOG: connected");
        Ok(AuthStatus { connected: true, username: Some(name.unwrap_or_else(|| "GOG Account".into())) })
    }
    fn auth_disconnect(&self) -> Result<(), String> {
        crate::account::plugin_remove(&self.config_dir, "gog").map_err(|e| e.to_string())
    }

    fn sync(&self, cx: &SourceCtx, sink: &mut dyn FnMut(Vec<GameRecord>), cancel: &Cancel) -> Result<usize, String> {
        let token = match self.access_token() { Ok(t) => t, Err(GogErr::Auth(m)) | Err(GogErr::Other(m)) => return Err(m) };
        let mut products: Vec<Value> = vec![]; let (mut page, mut total) = (1i64, 1i64);
        loop {
            if cancel.is_set() { return Ok(0); }
            crate::net::set_activity(&format!("GOG: reading your library (page {page}{})", if total > 1 { format!(" of {total}") } else { String::new() }));
            let r = match self.get(&format!("{PRODUCTS}?mediaType=1&page={page}"), Some(&token), 20) { Ok(r) => r, Err(e) => { log(&format!("GOG library: page {page} fetch failed: {e}")); break } };
            if r.status == 401 { return Err(EXPIRED.into()); }
            if r.status >= 400 { log(&format!("GOG library: page {page} answered HTTP {}", r.status)); break; }
            let Some(data) = Self::json(&r) else { log(&format!("GOG library: page {page} was not readable")); break };
            total = data.get("totalPages").filter(|t| truthy(Some(t))).and_then(to_i64).unwrap_or(1);
            products.extend(data.get("products").and_then(|p| p.as_array()).cloned().unwrap_or_default());
            if page >= total { break; }
            page += 1; pause(300);
        }
        let games: Vec<&Value> = products.iter().filter(|p| truthy(p.get("isGame")) && !truthy(p.get("isMovie"))).collect();
        log(&format!("GOG sync: {} games from {} products", games.len(), products.len()));
        if products.is_empty() { return Err("GOG returned no products. Try again, or disconnect and connect again.".into()); }
        let (existing, blacklisted) = self.ids();
        let mut nongames = self.load_nongames();
        let half = Self::half_trust(&cx.root);
        // ratings and achievements are read by the re-scrape that follows a first import (`enrich_new`)
        let mut batch: Vec<GameRecord> = vec![]; let mut fresh: Vec<(usize, String, String)> = vec![];
        let new_total = games.iter().filter(|p| { let id = id_str(p.get("id")); !existing.contains(&id) && !blacklisted.contains(&id) && !nongames.contains(&id) }).count();
        for p in games.iter() {
            let id = id_str(p.get("id"));
            if blacklisted.contains(&id) || nongames.contains(&id) { continue; }
            let name = p.get("title").and_then(|t| t.as_str()).map(String::from).unwrap_or_else(|| format!("GOG Game {id}"));
            let slug = p.get("slug").and_then(|s| s.as_str()).filter(|s| !s.is_empty()).map(String::from);
            if existing.contains(&id) { batch.push(GameRecord { platform_id: id, name, ..Default::default() }); continue; }
            let n = fresh.len() + 1;
            crate::net::set_activity(&format!("GOG: reading new games ({n}/{new_total})"));
            if cancel.is_set() { return Ok(0); }
            let mut rec = GameRecord { platform_id: id.clone(), name: name.clone(), slug, ..Default::default() };
            match self.fetch_metadata(&id, half) {
                Some((category, cols)) => {
                    if is_non_game(category.as_deref(), &name) { log(&format!("GOG sync: skipped non-game {name:?} (category={category:?})")); nongames.insert(id); continue; }
                    for (c, v) in cols { match (c, v) {
                        ("platform_slug", Sql::Text(t)) => rec.slug = Some(t), ("developers", Sql::Text(t)) => rec.developers = Some(t), ("publishers", Sql::Text(t)) => rec.publishers = Some(t),
                        ("release_date", Sql::Integer(t)) => rec.release_date = Some(t), ("genres", Sql::Text(t)) => rec.genres = Some(t), ("tags", Sql::Text(t)) => rec.tags = Some(t), _ => {} } }
                }
                None => {}
            }
            fresh.push((batch.len(), id, name)); batch.push(rec);
            pause(300);
        }
        self.save_nongames(&nongames);
        if !fresh.is_empty() && !cancel.is_set() {
            crate::net::set_activity("GOG: reading your order history for purchase dates");
            let list: Vec<(String, String)> = fresh.iter().map(|(_, id, name)| (id.clone(), name.clone())).collect();
            let dates = self.dates_for(&token, &list, cancel);
            for (at, id, _) in &fresh { if let Some(ts) = dates.get(id) { batch[*at].date_added = Some(*ts); } }
        }
        crate::net::set_activity("");
        let n = batch.len(); sink(batch);
        Ok(n)
    }

    /// `rescrape`: catalogue metadata and ratings, the account's achievements, and the store slug.
    fn rescrape(&self, cx: &SourceCtx, g: &GameRef) -> Fetch<Option<Update>> {
        let gog_id = if g.platform_id.is_empty() { g.appid.abs().to_string() } else { g.platform_id.clone() };
        let Some((_, cols)) = self.fetch_metadata(&gog_id, Self::half_trust(&cx.root)) else { return Ok(None) };
        let mut out: Update = cols;
        out.push(("meta_fetched", Sql::Text(cx.today.clone())));
        let galaxy = self.galaxy_id();
        if let (Some(galaxy), Ok(token)) = (galaxy, self.access_token()) {
            if let Some((unlocked, total)) = self.fetch_achievements(&gog_id, &galaxy, &token) {
                out.push(("unlocked_achievements", Sql::Integer(unlocked))); out.push(("total_achievements", Sql::Integer(total))); out.push(("cheevos_fetched", Sql::Text(cx.today.clone())));
            }
        }
        Ok(Some(out))
    }
    fn description(&self, _cx: &SourceCtx, g: &GameRef) -> Option<String> {
        let id = if g.platform_id.is_empty() { g.appid.abs().to_string() } else { g.platform_id.clone() };
        let r = self.get(&format!("https://api.gog.com/products/{id}?expand=description"), None, 10).ok().filter(|r| r.status < 400)?;
        description_from(&Self::json(&r)?)
    }
}

impl GogSource {
    /// The store's own vertical cover for a game (`art_urls`), for the Artwork Sources "Store" option.
    pub fn art_urls(&self, gog_id: &str) -> Vec<(&'static str, String)> {
        let Ok(r) = self.get(&format!("{GAMESDB}/{gog_id}"), None, 15) else { return vec![] };
        if r.status != 200 { log(&format!("GOG art_urls: HTTP {} for {gog_id}", r.status)); return vec![]; }
        Self::json(&r).and_then(|b| art_url(&b)).map(|u| vec![("vertical", u)]).unwrap_or_default()
    }
}

#[cfg(test)]
mod behaviour {
    use super::*;
    use crate::net::{NetError, Response};
    use std::sync::Mutex;

    /// A fake GOG: sign-in, the account's product pages, the public catalogue, ratings, achievements, orders. Records every URL asked.
    struct Site { token_ok: bool, refresh_ok: bool, products_status: u16, asked: Mutex<Vec<String>>, posts: Mutex<Vec<String>> }
    fn fake() -> Site { Site { token_ok: true, refresh_ok: true, products_status: 200, asked: Mutex::new(vec![]), posts: Mutex::new(vec![]) } }
    fn prod(id: i64, title: &str, game: bool) -> Value { json!({"id": id, "title": title, "slug": format!("slug_{id}"), "isGame": game, "isMovie": false}) }
    fn v2(category: &str, slug: &str) -> Value { json!({"_links": {"store": {"href": format!("https://www.gog.com/en/game/{slug}")}}, "_embedded": {"product": {"category": category, "globalReleaseDate": "2015-05-18T00:00:00+0300"}, "developers": [{"name": "Dev Co"}], "publisher": {"name": "Pub Co"}, "tags": [{"name": "Action"}], "properties": [{"name": "Open World"}]}}) }
    impl Http for Site {
        fn post(&self, u: &str, h: &[(&str, &str)], body: &str, _t: Duration) -> Result<Response, NetError> {
            assert_eq!(u, TOKEN_URL); assert!(h.iter().any(|(k, _)| *k == "User-Agent"));
            self.posts.lock().unwrap().push(body.to_string());
            assert!(body.contains(&format!("client_id={CLIENT_ID}")) && body.contains("client_secret="));
            let ok = if body.contains("grant_type=authorization_code") { self.token_ok && body.contains("code=THECODE") && body.contains("redirect_uri=https%3A%2F%2Fembed.gog.com") } else { self.refresh_ok && body.contains("refresh_token=RT0") };
            let r = |s: u16, b: Value| Ok(Response { status: s, retry_after: None, body: b.to_string() });
            if ok { r(200, json!({"access_token": "AT1", "refresh_token": "RT1", "expires_in": 3600})) } else { r(400, json!({"error": "invalid_grant"})) }
        }
        fn get(&self, u: &str, h: &[(&str, &str)], _t: Duration) -> Result<Response, NetError> {
            self.asked.lock().unwrap().push(u.to_string());
            let r = |s: u16, b: Value| Ok(Response { status: s, retry_after: None, body: b.to_string() });
            let authed = h.iter().any(|(k, v)| *k == "Authorization" && v.starts_with("Bearer "));
            if u.starts_with(USER_DATA) { assert!(authed); return r(200, json!({"username": "gogger", "galaxyUserId": "5555"})); }
            if u.starts_with(PRODUCTS) {
                assert!(authed);
                if self.products_status != 200 { return r(self.products_status, json!({})); }
                return if u.ends_with("page=1") { r(200, json!({"totalPages": 2, "products": [prod(1001, "Game One", true), prod(1002, "Game Two DLC", true), json!({"id": 1003, "title": "A Movie", "isGame": true, "isMovie": true}), prod(1004, "Not a game", false)]})) }
                else { r(200, json!({"totalPages": 2, "products": [prod(1005, "Cyberpunk Digital Goodies", true), prod(1006, "Removed", true), prod(1007, "Existing Game", true), prod(1008, "Unknown To Catalogue", true)]})) };
            }
            if u.starts_with(ORDERS) {
                assert!(authed);
                return r(200, json!({"totalPages": 1, "orders": [{"date": 1600000000, "products": [{"id": 1001, "title": "Game One"}]}, {"date": 1500000000, "products": [{"id": 1008, "title": "Unknown To Catalogue"}]}]}));
            }
            if let Some(rest) = u.strip_prefix(&format!("{V2_GAME}/")) {
                return match rest { "1001" => r(200, v2("GAME", "game_one")), "1002" => r(200, v2("DLC", "dlc")), "1005" => r(200, v2("GAME", "goodies")), "1008" => r(404, json!({})), _ => r(404, json!({})) };
            }
            if u.contains("/products/1001/os/linux/builds") { assert!(authed); return r(200, json!({"items": [{"link": "https://cdn/meta/BUILD"}]})); }
            if u.contains("/products/3003/os/linux/builds") { return r(200, json!({"items": [{"link": "https://cdn/meta/BAD"}]})); }
            if u.contains("/builds") { return r(404, json!({})); }
            if u.contains("/secure_link") { assert!(authed); return r(200, json!({"urls": [{"url_format": "https://chunks{path}?t={token}", "parameters": {"path": "/store/x/", "token": "T"}}]})); }
            if u.starts_with(RATINGS) { return r(200, json!({"value": 4.5, "count": 1234})); }
            if u.starts_with(ACHIEVEMENTS) { assert!(authed); return r(200, json!({"total_count": 10, "items": [{"date_unlocked": "x"}, {"date_unlocked": null}, {"date_unlocked": "y"}]})); }
            if u.contains("/products/") && u.contains("expand=description") { return r(200, json!({"description": {"lead": "A <b>great</b>  game"}})); }
            if u.starts_with(GAMESDB) { return r(200, json!({"game": {"vertical_cover": {"url_format": "https://img/x{formatter}.{ext}"}}})); }
            r(404, json!({}))
        }
        fn get_bytes(&self, u: &str, h: &[(&str, &str)], _t: Duration) -> Result<(u16, Vec<u8>), NetError> {
            use std::io::Write;
            self.asked.lock().unwrap().push(u.to_string());
            assert!(h.iter().any(|(k, v)| *k == "Authorization" && v.starts_with("Bearer ")), "content system calls carry the token");
            let z = |b: &[u8]| { let mut e = flate2::write::ZlibEncoder::new(vec![], flate2::Compression::default()); e.write_all(b).unwrap(); e.finish().unwrap() };
            let ok = |b: Vec<u8>| Ok((200, b));
            let item = |md5: &str| json!({"path": "start.sh", "flags": ["executable"], "chunks": [{"compressedMd5": md5, "size": 5}]});
            match u {
                "https://cdn/meta/BUILD" => ok(z(json!({"installDirectory": "Game One", "depots": [{"manifest": "11223344", "languages": ["*"]}]}).to_string().as_bytes())),
                "https://cdn/meta/BAD" => ok(z(json!({"installDirectory": "Broken", "depots": [{"manifest": "55667788", "languages": ["*"]}]}).to_string().as_bytes())),
                _ if u.ends_with("/11/22/11223344") => ok(z(json!({"depot": {"items": [item("aa11bb22")]}}).to_string().as_bytes())),
                _ if u.ends_with("/55/66/55667788") => ok(z(json!({"depot": {"items": [item("dead0000")]}}).to_string().as_bytes())),
                _ if u == "https://chunks/store/x/aa/11/aa11bb22?t=T" => ok(z(b"#!sh\n")),
                _ => Ok((404, vec![])),
            }
        }
    }
    fn setup(tag: &str, site: Site, tokens: Option<i64>) -> (GogSource, Arc<Site>, PathBuf) {
        let d = std::env::temp_dir().join(format!("zest_gog_{tag}_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        let cfg = match tokens { Some(exp) => json!({"gog": {"access_token": "AT0", "refresh_token": "RT0", "expires_at": exp, "username": "gogger", "galaxy_user_id": "5555", "games_dir": "/g"}, "other": 1}), None => json!({"other": 1}) };
        std::fs::write(d.join("config.json"), cfg.to_string()).unwrap();
        let db = d.join("games_1.db");
        rusqlite::Connection::open(&db).unwrap().execute_batch("CREATE TABLE games (appid INTEGER, platform TEXT, platform_id TEXT); CREATE TABLE blacklist (appid INTEGER, platform_id TEXT, platform TEXT);
            INSERT INTO games VALUES (-1, 'gog', '1007'); INSERT INTO blacklist VALUES (-9, '1006', 'gog');").unwrap();
        let s = Arc::new(site); (GogSource::new(s.clone(), d.clone(), db), s, d)
    }
    fn cx(d: &PathBuf, s: Arc<Site>) -> SourceCtx { let h: Arc<dyn Http> = s; SourceCtx { http: h.clone(), steam: Arc::new(crate::steamclient::Steam::new(h, None, 10.0)), root: d.clone(), db: d.join("games_1.db"), today: "2026-11-01".into(), appinfo: Arc::new(|_| Default::default()), account_id: None } }
    fn cfg(d: &PathBuf) -> Value { serde_json::from_str(&std::fs::read_to_string(d.join("config.json")).unwrap()).unwrap() }
    const FAR: i64 = 4_000_000_000;

    #[test] fn install_picks_the_linux_build_downloads_the_chunks_then_launches_and_uninstalls_inside_the_games_folder() {
        let (s, site, d) = setup("install", fake(), Some(FAR));
        let games = d.join("games");
        let mut c = cfg(&d); c["gog"]["games_dir"] = json!(games.to_string_lossy()); std::fs::write(d.join("config.json"), c.to_string()).unwrap();
        let g = GameRef { appid: -3, name: "Game One".into(), platform: "gog".into(), platform_id: "1001".into(), ..Default::default() };
        let mut says = vec![];
        let got = s.install(&cx(&d, site.clone()), &g, &mut |m, _, _| says.push(m.to_string()), &Cancel::new()).unwrap();
        let dir = games.join("Game One");
        assert_eq!((got.install_path.as_str(), got.executable.as_deref()), (dir.to_str().unwrap(), Some("start.sh")));
        assert_eq!(std::fs::read(dir.join("start.sh")).unwrap(), b"#!sh\n");
        #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; assert_eq!(std::fs::metadata(dir.join("start.sh")).unwrap().permissions().mode() & 0o111, 0o111); }
        assert!(says.iter().any(|m| m.starts_with("Downloading Game One")));
        // launch runs the script from its folder; uninstall deletes only inside the games folder
        let inst = GameRef { installed: true, install_path: Some(dir.to_string_lossy().into()), executable: got.executable.clone(), ..g.clone() };
        match s.launch(&inst).unwrap().action { Action::Spawn { program, dir: cwd, .. } => assert_eq!((program, cwd), (dir.join("start.sh").to_string_lossy().to_string(), Some(dir.to_string_lossy().to_string()))), _ => panic!("spawn expected") }
        assert_eq!(s.uninstall(&inst).unwrap(), Action::RemoveInstall { delete: true, dir: dir.to_string_lossy().to_string() });
        let elsewhere = GameRef { install_path: Some("/opt/else".into()), ..inst.clone() };
        assert_eq!(s.uninstall(&elsewhere).unwrap(), Action::RemoveInstall { delete: false, dir: "/opt/else".into() });
        assert!(s.launch(&GameRef { installed: false, ..inst.clone() }).unwrap_err().contains("not installed"));
        // no builds, and a chunk the CDN does not have: clear errors and nothing left behind
        let none = GameRef { platform_id: "2002".into(), name: "No Builds".into(), ..g.clone() };
        assert!(s.install(&cx(&d, site.clone()), &none, &mut |_, _, _| {}, &Cancel::new()).unwrap_err().starts_with("No downloadable builds"));
        let bad = GameRef { platform_id: "3003".into(), name: "Broken".into(), ..g.clone() };
        let e = s.install(&cx(&d, site.clone()), &bad, &mut |_, _, _| {}, &Cancel::new()).unwrap_err();
        assert!(e.starts_with("Download failed for start.sh"), "{e}"); assert!(!games.join("Broken").exists());
        // cancelled: stops and removes the folder
        let c = Cancel::new(); c.cancel();
        assert_eq!(s.install(&cx(&d, site), &GameRef { name: "Game One".into(), ..g }, &mut |_, _, _| {}, &c).unwrap_err(), "Cancelled");
        assert!(!games.join("Game One").exists());
    }
    #[test] fn installed_scan_follows_the_folder_and_launch_covers_windows_builds_and_dosbox() {
        let (s, _, d) = setup("launchcases", fake(), Some(FAR));
        let (win, dos) = (d.join("w"), d.join("d"));
        std::fs::create_dir_all(&win).unwrap(); std::fs::write(win.join("Game.exe"), "x").unwrap();
        std::fs::create_dir_all(&dos).unwrap(); std::fs::write(dos.join("goggame-77.info"), r#"{"playTasks":[{"type":"FileTask","isPrimary":true,"path":"DOSBOX\\dosbox.exe","arguments":"-noconsole"}]}"#).unwrap();
        // the scan: a folder that exists is installed, a missing one is not, games without a folder are left out
        rusqlite::Connection::open(d.join("games_1.db")).unwrap().execute_batch(&format!("INSERT INTO games VALUES (-5, 'gog', '55'), (-6, 'gog', '66'); ALTER TABLE games ADD COLUMN install_path TEXT; UPDATE games SET install_path = '{}' WHERE appid = -5; UPDATE games SET install_path = '/no/such/dir' WHERE appid = -6;", win.display())).unwrap();
        let rows = s.installed_scan().unwrap();
        assert_eq!(rows.iter().map(|r| (r.platform_id.as_str(), r.installed)).collect::<Vec<_>>(), vec![("55", true), ("66", false)]);
        // a Windows build: Wine/Proton in a prefix inside the install folder
        let g = GameRef { appid: -5, name: "Win".into(), platform: "gog".into(), platform_id: "55".into(), installed: true, install_path: Some(win.to_string_lossy().into()), ..Default::default() };
        crate::wine::TEST_RUNTIME.with(|t| *t.borrow_mut() = Some(Ok(crate::wine::Runtime::Wine { wine: "/usr/bin/wine".into() })));
        match s.launch(&g).unwrap().action { Action::Spawn { program, args, env, .. } => { assert_eq!((program.as_str(), args), ("/usr/bin/wine", vec![win.join("Game.exe").to_string_lossy().to_string()])); assert!(env.contains(&("WINEPREFIX".into(), Some(win.join("prefix").to_string_lossy().to_string())))); } _ => panic!("spawn expected") }
        assert!(win.join("prefix").is_dir());
        crate::wine::TEST_RUNTIME.with(|t| *t.borrow_mut() = Some(Err(crate::wine::NO_WINE_MSG.into())));
        assert!(s.launch(&g).unwrap_err().contains("No Wine or Proton"));
        // DOSBox: with none installed the game says so (the lookup finds nothing in this check), and a folder with nothing to run says that
        let gd = GameRef { appid: -7, platform_id: "77".into(), install_path: Some(dos.to_string_lossy().into()), ..g.clone() };
        let e = s.launch_with(&gd, &|_| None).unwrap_err();
        assert!(e.contains("DOSBox is not installed"), "{e}");
        let empty = d.join("e"); std::fs::create_dir_all(&empty).unwrap();
        assert!(s.launch(&GameRef { install_path: Some(empty.to_string_lossy().into()), executable: None, ..g }).unwrap_err().contains("No supported executable"));
    }
    #[test] fn signing_in_from_a_pasted_address_stores_numbers_as_numbers_and_disconnect_keeps_the_rest() {
        let (s, _, d) = setup("signin", fake(), None);
        assert!(!s.auth_status().unwrap().connected);
        let st = s.auth_complete("https://embed.gog.com/on_login_success?origin=client&code=THECODE").unwrap();
        assert_eq!(st, AuthStatus { connected: true, username: Some("gogger".into()) });
        let c = cfg(&d);
        assert_eq!(c["gog"]["access_token"], "AT1"); assert_eq!(c["gog"]["refresh_token"], "RT1"); assert_eq!(c["gog"]["galaxy_user_id"], "5555");
        assert!(c["gog"]["expires_at"].is_i64(), "expires_at stays a number, as PlayDate reads it");
        assert!(s.auth_status().unwrap().connected);
        s.auth_disconnect().unwrap();
        let c = cfg(&d); assert!(c.get("gog").is_none() && c["other"] == 1);
        assert!(!s.auth_status().unwrap().connected);
        assert!(s.auth_complete("THECODE").is_ok(), "a bare code works too");
    }
    #[test] fn a_bad_sign_in_saves_nothing() {
        let (s, _, d) = setup("badin", Site { token_ok: false, ..fake() }, None);
        assert!(s.auth_complete("https://embed.gog.com/on_login_success?origin=client&code=THECODE").unwrap_err().contains("HTTP 400"));
        assert!(s.auth_complete("https://embed.gog.com/on_login_success?origin=client").unwrap_err().contains("No code="));
        assert_eq!(s.auth_complete("  ").unwrap_err(), "code is required");
        assert!(cfg(&d).get("gog").is_none());
    }
    #[test] fn an_expired_token_is_refreshed_once_and_other_settings_survive() {
        let (s, site, d) = setup("refresh", fake(), Some(1));
        assert!(matches!(s.access_token(), Ok(t) if t == "AT1")); assert!(matches!(s.access_token(), Ok(t) if t == "AT1"));
        assert_eq!(site.posts.lock().unwrap().len(), 1);
        let c = cfg(&d); assert_eq!(c["gog"]["games_dir"], "/g"); assert_eq!(c["gog"]["refresh_token"], "RT1"); assert!(c["gog"]["expires_at"].as_i64().unwrap() > 1_700_000_000);
        let (s, _, _) = setup("refresh_bad", Site { refresh_ok: false, ..fake() }, Some(1));
        assert!(matches!(s.access_token(), Err(GogErr::Auth(m)) if m == RECONNECT));
        let (s, _, d) = setup("fresh_token", fake(), Some(FAR));
        assert!(matches!(s.access_token(), Ok(t) if t == "AT0")); assert_eq!(cfg(&d)["gog"]["access_token"], "AT0");
    }
    #[test] fn sync_reads_every_page_and_keeps_only_real_new_games_with_metadata_and_purchase_dates() {
        let (s, site, d) = setup("sync", fake(), Some(FAR));
        let mut got = vec![]; let n = s.sync(&cx(&d, site.clone()), &mut |b| got.extend(b), &Cancel::new()).unwrap();
        let ids: Vec<&str> = got.iter().map(|r| r.platform_id.as_str()).collect();
        assert_eq!(ids, ["1001", "1007", "1008"], "the movie, the non-game, the DLC and the goodies pack are out; the blacklisted one is skipped; one the catalogue does not know is kept");
        assert_eq!(n, 3);
        let one = &got[0];
        assert_eq!((one.name.as_str(), one.slug.as_deref(), one.developers.as_deref(), one.publishers.as_deref(), one.genres.as_deref(), one.tags.as_deref(), one.release_date, one.date_added), ("Game One", Some("game_one"), Some("Dev Co"), Some("Pub Co"), Some("Action"), Some("Open World"), Some(1431896400), Some(1600000000)));
        assert_eq!((got[1].slug.clone(), got[1].date_added, got[1].developers.clone()), (None, None, None), "a game already in the library is not re-read");
        assert_eq!((got[2].slug.as_deref(), got[2].date_added), (Some("slug_1008"), Some(1500000000)), "the product list's slug when the catalogue has none; its date by id");
        let ng: Vec<String> = serde_json::from_str(&std::fs::read_to_string(d.join("gog_nongames.json")).unwrap()).unwrap(); assert_eq!(ng, ["1002", "1005"]);
        // the second sync does not ask the catalogue about them again
        site.asked.lock().unwrap().clear();
        s.sync(&cx(&d, site.clone()), &mut |_| {}, &Cancel::new()).unwrap();
        let asked = site.asked.lock().unwrap().join("\n");
        assert!(!asked.contains("/games/1002") && !asked.contains("/games/1005"), "{asked}");
    }
    #[test] fn a_rejected_session_or_empty_answer_fails_the_sync_and_cancel_adds_nothing() {
        let (s, site, d) = setup("401", Site { products_status: 401, ..fake() }, Some(FAR));
        assert!(s.sync(&cx(&d, site), &mut |_| {}, &Cancel::new()).unwrap_err().contains("expired"));
        let (s, site, d) = setup("500", Site { products_status: 500, ..fake() }, Some(FAR));
        assert!(s.sync(&cx(&d, site), &mut |_| {}, &Cancel::new()).unwrap_err().contains("no products"));
        let (s, site, d) = setup("noconn", fake(), None);
        assert!(s.sync(&cx(&d, site), &mut |_| {}, &Cancel::new()).unwrap_err().contains("reconnect"));
        let (s, site, d) = setup("cancel", fake(), Some(FAR)); let c = Cancel::new(); c.cancel();
        let mut got = vec![]; assert_eq!(s.sync(&cx(&d, site), &mut |b| got.extend(b), &c).unwrap(), 0); assert!(got.is_empty());
    }
    #[test] fn rescrape_adds_ratings_and_achievements_and_description_and_art_work() {
        let (s, site, d) = setup("rescrape", fake(), Some(FAR));
        let g = GameRef { appid: -4, name: "Game One".into(), platform: "gog".into(), platform_id: "1001".into(), ..Default::default() };
        let u = s.rescrape(&cx(&d, site.clone()), &g).unwrap().unwrap();
        let get = |k: &str| u.iter().find(|(c, _)| *c == k).map(|(_, v)| v.clone());
        assert_eq!(get("review_percentage"), Some(Sql::Integer(90))); assert_eq!(get("review_score"), Some(Sql::Text("Very Positive".into()))); assert_eq!(get("total_reviews"), Some(Sql::Integer(1234)));
        assert_eq!((get("unlocked_achievements"), get("total_achievements"), get("cheevos_fetched")), (Some(Sql::Integer(2)), Some(Sql::Integer(10)), Some(Sql::Text("2026-11-01".into()))));
        assert_eq!(get("platform_slug"), Some(Sql::Text("game_one".into()))); assert_eq!(get("meta_fetched"), Some(Sql::Text("2026-11-01".into())));
        assert_eq!(s.description(&cx(&d, site.clone()), &g).as_deref(), Some("A great game"));
        assert_eq!(s.art_urls("1001"), vec![("vertical", "https://img/x_glx_vertical_cover.jpg".to_string())]);
        assert_eq!(LibrarySource::art_urls(&s, &g), vec![("vertical".to_string(), "https://img/x_glx_vertical_cover.jpg".to_string())]);
        assert!(s.enrich_new() && s.caps().sync && s.caps().auth);
        // without a Galaxy user id there are no achievement columns
        std::fs::write(d.join("config.json"), json!({"gog": {"access_token": "AT0", "refresh_token": "RT0", "expires_at": FAR}}).to_string()).unwrap();
        let u = s.rescrape(&cx(&d, site), &g).unwrap().unwrap(); assert!(!u.iter().any(|(k, _)| *k == "total_achievements"));
    }
}
