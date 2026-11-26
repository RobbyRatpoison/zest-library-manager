//! Port of `metadata.py`: find a Steam counterpart for a game (PCGamingWiki infobox, then Steam's store search with fuzzy matching) and fill the
//! fields a game is missing. The matching and parsing parts are pure and checked against the real Python (`tests/gen_metadata_golden.py`); the clients
//! sit behind the `Http` trait. Plugins (store pages, descriptions) come in through `Hooks`, since they live in the Python plugin host.
use crate::hltb::sequence_ratio;
use crate::jobs::Update;
use crate::net::{enc, Http};
use crate::steamclient::{AppinfoDates, Fetch, RateLimited, Steam};
use regex::Regex;
use rusqlite::types::Value as Sql;
use serde_json::Value;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const SIM_PCGW: f64 = 0.72;
const SIM_CONFIRMED: f64 = 0.90;
const SIM_MAYBE: f64 = 0.72;
const SIM_EDITION_BASE: f64 = 0.85;
const MAX_SEARCH_TRIES: usize = 4;
const UA: &str = concat!("Zest/", env!("CARGO_PKG_VERSION"), " (game library manager; https://github.com/RobbyRatpoison/zest-library-manager)");

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex { cell.get_or_init(|| Regex::new(pat).unwrap()) }

// ---------------------------------------------------------------- names

/// `_clean_query`: trademark glyphs, a trailing (YYYY), Humble/GOG "DRM-free" suffixes removed; whitespace collapsed. Keeps case and punctuation.
pub fn clean_query(name: &str) -> String {
    static TM: OnceLock<Regex> = OnceLock::new(); static YEAR: OnceLock<Regex> = OnceLock::new(); static DIST: OnceLock<Regex> = OnceLock::new(); static WS: OnceLock<Regex> = OnceLock::new();
    let n = re(&TM, "[™®©℠]").replace_all(name, "");
    let n = re(&YEAR, r"\s*\((?:19|20)\d{2}\)\s*$").replace_all(&n, "");
    let n = re(&DIST, r"(?i)\s*[\-–(]?\s*(?:(?:windows|win32|win64|macos|mac|osx|linux)\s+)?(?:drm[\s\-]?free(?:\s+build)?|humble(?:\s+bundle)?\s+original)\s*\)?\s*$").replace_all(&n, "");
    re(&WS, r"\s+").replace_all(&n, " ").trim().to_string()
}

/// `_norm`: as `clean_query`, then punctuation to spaces and lowercase.
pub fn norm(name: &str) -> String {
    static NW: OnceLock<Regex> = OnceLock::new(); static WS: OnceLock<Regex> = OnceLock::new();
    // `_norm` drops the glyphs, year and distribution noise exactly like `_clean_query` does (same three patterns, same order), then flattens
    let c = clean_query_no_ws(name);
    let n = re(&NW, r"[^\w\s]").replace_all(&c, " ");
    re(&WS, r"\s+").replace_all(&n, " ").trim().to_lowercase()
}
fn clean_query_no_ws(name: &str) -> String {
    static TM: OnceLock<Regex> = OnceLock::new(); static YEAR: OnceLock<Regex> = OnceLock::new(); static DIST: OnceLock<Regex> = OnceLock::new();
    let n = re(&TM, "[™®©℠]").replace_all(name, "");
    let n = re(&YEAR, r"\s*\((?:19|20)\d{2}\)\s*$").replace_all(&n, "");
    re(&DIST, r"(?i)\s*[\-–(]?\s*(?:(?:windows|win32|win64|macos|mac|osx|linux)\s+)?(?:drm[\s\-]?free(?:\s+build)?|humble(?:\s+bundle)?\s+original)\s*\)?\s*$").replace_all(&n, "").into_owned()
}

/// `_similar`
pub fn similar(a: &str, b: &str) -> f64 { sequence_ratio(&norm(a), &norm(b)) }

/// `_search_queries`: the cleaned title, then shorter ones (trailing words dropped, never under two words), at most 4.
pub fn search_queries(name: &str) -> Vec<String> {
    let c = clean_query(name); let words: Vec<&str> = c.split_whitespace().collect();
    let mut out = if words.is_empty() { vec![] } else { vec![words.join(" ")] };
    let mut n = words.len() as i64 - 1;
    while n >= 2 && out.len() < MAX_SEARCH_TRIES { out.push(words[..n as usize].join(" ")); n -= 1; }
    out
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VariantKind { Subtitle, Edition }

/// `_steam_search_variants`: the title before a subtitle, and the title without a trailing edition word.
pub fn search_variants(name: &str) -> Vec<(String, VariantKind)> {
    static SPLIT: OnceLock<Regex> = OnceLock::new(); static ED: OnceLock<Regex> = OnceLock::new();
    let clean = clean_query(name); let mut out = vec![];
    let split = re(&SPLIT, r"\s+[-\u{2013}\u{2014}:]\s+|:\s+");
    let base = match split.find(&clean) { Some(m) => clean[..m.start()].trim().to_string(), None => clean.trim().to_string() };
    if !base.is_empty() && base.to_lowercase() != clean.to_lowercase() { out.push((base, VariantKind::Subtitle)); }
    let stripped = re(&ED, r"(?i)\s+(?:hd\s+)?(?:remaster(?:ed)?|definitive edition|complete edition|deluxe edition|special edition|enhanced edition|anniversary edition|game of the year edition|goty edition)\s*$").replace_all(&clean, "").trim().to_string();
    if !stripped.is_empty() && stripped.to_lowercase() != clean.to_lowercase() { out.push((stripped, VariantKind::Edition)); }
    out
}

// ---------------------------------------------------------------- PCGamingWiki: parsing

#[derive(Debug, Clone, PartialEq, Default)]
pub struct PcgwBox { pub title: String, pub steam_appid: Option<i64>, pub gog_id: Option<String>, pub developers: Option<String>, pub publishers: Option<String>,
    pub genres: Option<String>, pub categories: Option<String>, pub release_date: Option<i64> }
impl PcgwBox {
    /// Any of `_USEFUL_BOX_KEYS` present.
    pub fn useful(&self) -> bool { self.developers.is_some() || self.publishers.is_some() || self.genres.is_some() || self.categories.is_some() || self.release_date.map_or(false, |d| d != 0) }
    fn field(&self, f: &str) -> Option<&String> { match f { "developers" => self.developers.as_ref(), "publishers" => self.publishers.as_ref(), "genres" => self.genres.as_ref(), "categories" => self.categories.as_ref(), _ => None } }
}

/// `_REDIRECT_RE`: `#REDIRECT [[Target]]` at the start of the text.
pub fn redirect_target(wt: &str) -> Option<String> {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"(?i)^\s*#redirect\s*\[\[\s*([^\]|#]+)").captures(wt).map(|c| c[1].trim().to_string())
}

/// `_pcgw_parse_date`: 'March 28, 1997', 'Mar 28, 1997', 'September 1995', '1994' -> start of that period (UTC); a `<ref` tail is dropped.
pub fn parse_date(s: &str) -> Option<i64> {
    static REF: OnceLock<Regex> = OnceLock::new(); static F: OnceLock<[Regex; 5]> = OnceLock::new();
    let s = re(&REF, r"(?s)<ref.*").replace_all(s, ""); let s = s.trim().trim_matches(',').trim();
    const FULL: [&str; 12] = ["january", "february", "march", "april", "may", "june", "july", "august", "september", "october", "november", "december"];
    const ABBR: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    let fs = F.get_or_init(|| {
        let full = FULL.join("|"); let abbr = ABBR.join("|");
        [format!(r"(?i)^({full})\s+(\d{{1,2}}),\s+(\d{{4}})$"), format!(r"(?i)^({abbr})\s+(\d{{1,2}}),\s+(\d{{4}})$"), format!(r"(?i)^({full})\s+(\d{{4}})$"), format!(r"(?i)^({abbr})\s+(\d{{4}})$"), r"^(\d{4})$".to_string()].map(|p| Regex::new(&p).unwrap())
    });
    for (i, r) in fs.iter().enumerate() {
        let Some(c) = r.captures(s) else { continue };
        let names: &[&str; 12] = if i == 0 || i == 2 { &FULL } else { &ABBR };
        let (m, d, y): (i64, i64, i64) = match i {
            0 | 1 => (names.iter().position(|n| n.eq_ignore_ascii_case(&c[1])).unwrap() as i64 + 1, c[2].parse().unwrap(), c[3].parse().unwrap()),
            2 | 3 => (names.iter().position(|n| n.eq_ignore_ascii_case(&c[1])).unwrap() as i64 + 1, 1, c[2].parse().unwrap()),
            _ => (1, 1, c[1].parse().unwrap()),
        };
        if y < 1 { continue; }
        let (yy, mm) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
        let era = yy.div_euclid(400); let yoe = yy - era * 400; let doy = (153 * mm + 2) / 5 + d - 1; let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        let days = era * 146097 + doe - 719468;
        // reject impossible days (Feb 30): the round trip must give the same day of month
        let z = days + 719468; let e2 = z.div_euclid(146097); let doe2 = z - e2 * 146097; let yoe2 = (doe2 - doe2 / 1460 + doe2 / 36524 - doe2 / 146096) / 365; let doy2 = doe2 - (365 * yoe2 + yoe2 / 4 - yoe2 / 100); let mp = (5 * doy2 + 2) / 153;
        if (doy2 - (153 * mp + 2) / 5 + 1) != d { continue; }
        return Some(days * 86400);
    }
    None
}

/// The body of `_pcgw_infobox` once the page's section-0 wikitext is known.
pub fn infobox_from_wikitext(title: &str, wt: &str) -> PcgwBox {
    static APP: OnceLock<Regex> = OnceLock::new(); static GOG: OnceLock<Regex> = OnceLock::new(); static DEV: OnceLock<Regex> = OnceLock::new(); static PUB: OnceLock<Regex> = OnceLock::new();
    static GEN: OnceLock<Regex> = OnceLock::new(); static MODE: OnceLock<Regex> = OnceLock::new(); static DATE: OnceLock<Regex> = OnceLock::new();
    let mut b = PcgwBox { title: title.to_string(), ..Default::default() };
    b.steam_appid = re(&APP, r"(?i)\|\s*steam[ _]?appid\s*=\s*(\d+)").captures(wt).and_then(|c| c[1].parse().ok());
    b.gog_id = re(&GOG, r"(?i)\|\s*gogcom id\s*=\s*(\d+)").captures(wt).map(|c| c[1].to_string());
    let uniq = |r: &Regex| -> Vec<String> { let mut v: Vec<String> = vec![]; for c in r.captures_iter(wt) { let s = c[1].trim().to_string(); if !s.is_empty() && !v.contains(&s) { v.push(s); } } v };
    let devs = uniq(re(&DEV, r"(?i)\{\{\s*Infobox game/row/developer\s*\|\s*([^}|]+?)\s*(?:\||\}\})"));
    let pubs = uniq(re(&PUB, r"(?i)\{\{\s*Infobox game/row/publisher\s*\|\s*([^}|]+?)\s*(?:\||\}\})"));
    if !devs.is_empty() { b.developers = Some(devs.join(",")); }
    if !pubs.is_empty() { b.publishers = Some(pubs.join(",")); }
    if let Some(c) = re(&GEN, r"(?i)\{\{\s*Infobox game/row/taxonomy/genres\s*\|\s*([^}]*?)\s*\}\}").captures(wt) {
        if !c[1].trim().is_empty() { b.genres = Some(c[1].split(',').map(str::trim).filter(|g| !g.is_empty()).collect::<Vec<_>>().join(",")); }
    }
    if let Some(c) = re(&MODE, r"(?i)\{\{\s*Infobox game/row/taxonomy/modes\s*\|\s*([^}]*?)\s*\}\}").captures(wt) {
        if !c[1].trim().is_empty() {
            let mut cats: Vec<&str> = vec![];
            for x in c[1].split(',') { let m = match x.trim().to_lowercase().as_str() { "singleplayer" => Some("Single-player"), "multiplayer" => Some("Multi-player"), _ => None }; if let Some(m) = m { if !cats.contains(&m) { cats.push(m); } } }
            if !cats.is_empty() { b.categories = Some(cats.join(",")); }
        }
    }
    let dates: Vec<i64> = re(&DATE, r"(?i)\{\{\s*Infobox game/row/date\s*\|\s*[^|]+\|\s*([^}|]+?)\s*(?:\||\}\})").captures_iter(wt).filter_map(|c| parse_date(&c[1])).filter(|d| *d != 0).collect();
    b.release_date = dates.into_iter().min();
    b
}

// ---------------------------------------------------------------- Steam search verdict

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Confidence { Pcgw, Confirmed, Maybe, None, Retry }
impl Confidence { pub fn as_str(&self) -> &'static str { match self { Confidence::Pcgw => "pcgw", Confidence::Confirmed => "confirmed", Confidence::Maybe => "maybe", Confidence::None => "none", Confidence::Retry => "retry" } } }

pub type Verdict = (Option<i64>, Confidence);

/// `_judge_candidates`: None when nothing is close enough.
pub fn judge_candidates(name: &str, candidates: &[(i64, String)], b: Option<&PcgwBox>, pcgw_down: bool, base: Option<&str>) -> Option<Verdict> {
    if candidates.is_empty() { return None; }
    let want = norm(name);
    if let Some(c) = candidates.iter().find(|c| norm(&c.1) == want) { return Some((Some(c.0), Confidence::Confirmed)); }
    let score_of = |c: &(i64, String)| { let mut sc = similar(name, &c.1); if let Some(b) = base { if norm(&c.1) == norm(b) { sc = sc.max(SIM_EDITION_BASE); } } sc };
    let mut best = &candidates[0]; let mut bs = score_of(best);
    for c in &candidates[1..] { let s = score_of(c); if s > bs { best = c; bs = s; } }
    if bs >= SIM_CONFIRMED { return Some((Some(best.0), Confidence::Confirmed)); }
    if !pcgw_down && bs >= SIM_MAYBE {
        if b.map_or(false, |b| b.useful()) { return Some((None, Confidence::None)); }
        return Some((Some(best.0), Confidence::Maybe));
    }
    None
}

// ---------------------------------------------------------------- PCGamingWiki: the throttled client

/// "PCGamingWiki could not answer" (throttled or errored), as distinct from "answered, nothing there".
#[derive(Debug, Clone, PartialEq)]
pub struct Unavailable;

pub struct Pcgw { http: Arc<dyn Http>, st: Mutex<(Option<Instant>, Option<Instant>)>, min_gap: Duration, cooldown: Duration }
impl Pcgw {
    /// 1.5 s between requests, 180 s stand-down after a 429 (PCGW 429s after ~20 quick hits).
    pub fn new(http: Arc<dyn Http>) -> Self { Self::with_timing(http, Duration::from_millis(1500), Duration::from_secs(180)) }
    pub fn with_timing(http: Arc<dyn Http>, min_gap: Duration, cooldown: Duration) -> Self { Pcgw { http, st: Mutex::new((None, None)), min_gap, cooldown } }

    /// `_pcgw_get`: serialised (the lock is held for the request), rate limited, JSON or `Unavailable`.
    fn get(&self, params: &[(&str, String)]) -> Result<Value, Unavailable> {
        let mut st = self.st.lock().unwrap();
        if st.1.map_or(false, |b| Instant::now() < b) { return Err(Unavailable); }
        if let Some(last) = st.0 { let since = last.elapsed(); if since < self.min_gap { std::thread::sleep(self.min_gap - since); } }
        let q = params.iter().map(|(k, v)| format!("{k}={}", enc(v))).collect::<Vec<_>>().join("&");
        let r = self.http.get(&format!("https://www.pcgamingwiki.com/w/api.php?{q}"), &[("User-Agent", UA)], Duration::from_secs(10));
        st.0 = Some(Instant::now());
        match r {
            Ok(r) if r.status == 429 => { st.1 = Some(Instant::now() + self.cooldown); crate::net::log(&format!("PCGW 429 -- standing down for {}s", self.cooldown.as_secs())); Err(Unavailable) }
            Ok(r) if (200..300).contains(&r.status) => serde_json::from_str(&r.body).map_err(|_| Unavailable),
            Ok(r) => { crate::net::log(&format!("PCGW request failed: HTTP {}", r.status)); Err(Unavailable) }
            Err(e) => { crate::net::log(&format!("PCGW request failed: {e}")); Err(Unavailable) }
        }
    }

    fn section0(&self, title: &str) -> Result<String, Unavailable> {
        let j = self.get(&[("action", "parse".into()), ("page", title.into()), ("prop", "wikitext".into()), ("section", "0".into()), ("format", "json".into())])?;
        Ok(j.pointer("/parse/wikitext/*").and_then(|v| v.as_str()).unwrap_or("").to_string())
    }

    /// `_pcgw_find_page`: (title, wikitext) of the best match, `Ok(None)` for no good match.
    fn find_page(&self, name: &str) -> Result<Option<(String, String)>, Unavailable> {
        let clean = clean_query(name);
        let wt = self.section0(&clean)?;
        if wt.to_lowercase().contains("{{infobox game") { return Ok(Some((clean, wt))); }
        for q in search_queries(name) {
            let j = self.get(&[("action", "query".into()), ("list", "search".into()), ("format", "json".into()), ("srsearch", q), ("srlimit", "6".into()), ("srprop", String::new())])?;
            let hits: Vec<String> = j.pointer("/query/search").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|h| h.get("title").and_then(|t| t.as_str()).map(String::from)).collect()).unwrap_or_default();
            if hits.is_empty() { continue; }
            let mut best = &hits[0]; let mut bs = similar(name, best);
            for h in &hits[1..] { let s = similar(name, h); if s > bs { best = h; bs = s; } }
            if bs < SIM_PCGW { continue; }
            let wt = self.section0(best)?;
            return Ok(Some((best.clone(), wt)));
        }
        Ok(None)
    }

    /// `_pcgw_infobox`: the parsed infobox of the page for `name`; `Ok(None)` = no matching page.
    pub fn infobox(&self, name: &str) -> Result<Option<PcgwBox>, Unavailable> {
        let Some((mut title, mut wt)) = self.find_page(name)? else { return Ok(None) };
        if let Some(t) = redirect_target(&wt) { title = t; wt = self.section0(&title)?; }
        let b = infobox_from_wikitext(&title, &wt);
        crate::net::log(&format!("PCGW: {name:?} -> {title}"));
        Ok(Some(b))
    }
}

// ---------------------------------------------------------------- resolving a Steam appid

/// `_steam_search_candidates`: the store search's apps.
pub fn steam_search_candidates(http: &dyn Http, name: &str) -> Vec<(i64, String)> {
    let url = format!("https://store.steampowered.com/api/storesearch/?term={}&l=english&cc=US", enc(&clean_query(name)));
    match http.get(&url, &[("User-Agent", UA)], Duration::from_secs(8)) {
        Ok(r) if (200..300).contains(&r.status) => serde_json::from_str::<Value>(&r.body).ok().and_then(|j| j.get("items").and_then(|i| i.as_array()).map(|a| a.iter().filter_map(|i| {
            if i.get("type").and_then(|t| t.as_str()) != Some("app") { return None; }
            let id = i.get("id").and_then(|v| v.as_i64()).filter(|i| *i != 0)?; let n = i.get("name").and_then(|v| v.as_str()).filter(|n| !n.is_empty())?; Some((id, n.to_string()))
        }).collect())).unwrap_or_default(),
        Ok(r) => { crate::net::log(&format!("Steam search {name:?}: HTTP {}", r.status)); vec![] }
        Err(e) => { crate::net::log(&format!("Steam search {name:?}: {e}")); vec![] }
    }
}

/// `_resolve`: (steam appid, confidence, PCGW box).
pub fn resolve(http: &dyn Http, pcgw: &Pcgw, name: &str) -> (Option<i64>, Confidence, Option<PcgwBox>) {
    if name.trim().is_empty() { return (None, Confidence::None, None); }
    let (b, down) = match pcgw.infobox(name) { Ok(b) => (b, false), Err(_) => (None, true) };
    if let Some(id) = b.as_ref().and_then(|b| b.steam_appid) { return (Some(id), Confidence::Pcgw, b); }
    if let Some((id, c)) = judge_candidates(name, &steam_search_candidates(http, name), b.as_ref(), down, None) { return (id, c, b); }
    for (term, kind) in search_variants(name) {
        let v = judge_candidates(name, &steam_search_candidates(http, &term), b.as_ref(), down, (kind == VariantKind::Edition).then_some(term.as_str()));
        if let Some((id, c)) = v { return (id, c, b); }
    }
    if down && b.is_none() { return (None, Confidence::Retry, None); }
    (None, Confidence::None, b)
}

// ---------------------------------------------------------------- the backfill

/// What the plugin host provides (Python plugins). `NoPlugins` for a build without one.
#[derive(Default, Debug, Clone)]
pub struct PageMeta { pub genres: Option<String>, pub categories: Option<String>, pub tags: Option<String>, pub developers: Option<String>, pub publishers: Option<String>, pub release_date: Option<i64> }
pub trait Hooks: Send + Sync {
    fn store_page_metadata(&self, _platform: &str, _slug: &str) -> PageMeta { PageMeta::default() }
    fn description(&self, _platform: &str, _appid: i64, _platform_id: &str) -> String { String::new() }
}
pub struct NoPlugins;
impl Hooks for NoPlugins {}

/// The columns `backfill_metadata` reads.
#[derive(Debug, Clone, Default)]
pub struct Row {
    pub name: String, pub platform: Option<String>, pub platform_id: String, pub platform_slug: String, pub steam_appid: Option<i64>, pub meta_backfill_fetched: Option<String>,
    pub developers: Option<String>, pub publishers: Option<String>, pub genres: Option<String>, pub categories: Option<String>, pub tags: Option<String>, pub release_date: Option<i64>,
    pub review_score: Option<String>, pub is_free: Option<i64>, pub metacritic_score: Option<i64>, pub short_description: Option<String>, pub total_achievements: Option<i64>,
}
impl Row {
    pub const SELECT: &'static str = "SELECT name, platform, platform_id, platform_slug, steam_appid, meta_backfill_fetched, developers, publishers, genres, categories, tags, release_date, review_score, is_free, metacritic_score, short_description, total_achievements FROM games WHERE appid = ?";
    pub fn load(conn: &rusqlite::Connection, appid: i64) -> Option<Row> {
        conn.query_row(Self::SELECT, [appid], |r| Ok(Row { name: r.get::<_, Option<String>>(0)?.unwrap_or_default(), platform: r.get(1)?, platform_id: r.get::<_, Option<String>>(2)?.unwrap_or_default(), platform_slug: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
            steam_appid: r.get(4)?, meta_backfill_fetched: r.get::<_, Option<Sql>>(5)?.map(|v| match v { Sql::Text(t) => t, Sql::Integer(i) => i.to_string(), _ => String::new() }), developers: r.get(6)?, publishers: r.get(7)?, genres: r.get(8)?, categories: r.get(9)?, tags: r.get(10)?,
            release_date: r.get(11)?, review_score: r.get(12)?, is_free: r.get(13)?, metacritic_score: r.get(14)?, short_description: r.get(15)?, total_achievements: r.get(16)? })).ok()
    }
    fn gap(&self, f: &str) -> bool { empty(match f { "developers" => &self.developers, "publishers" => &self.publishers, "genres" => &self.genres, "categories" => &self.categories, "tags" => &self.tags, _ => &None }) }
}
fn empty(v: &Option<String>) -> bool { v.as_deref().map_or(true, |s| s.trim().is_empty()) }
fn t(s: &str) -> Sql { Sql::Text(s.to_string()) }

pub struct Backfill<'a> { pub steam: &'a Steam, pub http: &'a dyn Http, pub pcgw: &'a Pcgw, pub plugins: &'a dyn Hooks, pub today: &'a str }

/// `backfill_metadata`: what to write for one game (`None` = leave it: not found, or PCGW throttled and the Steam guess weak). Columns come in Python's
/// order. `force` applies a fuzzy ("maybe") Steam match and implies `rerun`; `rerun` re-attempts a game already stamped.
pub fn backfill(b: &Backfill, appid: i64, row: &Row, dates: &dyn Fn(i64) -> AppinfoDates, force: bool, rerun: bool) -> Fetch<Option<Update>> {
    let rerun = rerun || force;
    let is_steam = row.platform.as_deref().unwrap_or("steam") == "steam";
    if let Some(f) = &row.meta_backfill_fetched { if !f.is_empty() && f != "0" && f != "unconfirmed" && !rerun { return Ok(None); } }
    let mut steam_appid = row.steam_appid;
    let mut pcgw_box: Option<PcgwBox> = None;
    if is_steam { steam_appid = Some(appid); }
    else if steam_appid.is_none() || force || row.meta_backfill_fetched.as_deref() == Some("unconfirmed") {
        let (id, conf, bx) = resolve(b.http, b.pcgw, &row.name);
        steam_appid = id; pcgw_box = bx;
        if conf == Confidence::Retry { return Ok(None); }
        if conf == Confidence::Maybe && !force { return Ok(Some(vec![("steam_appid", id.map_or(Sql::Null, Sql::Integer)), ("meta_backfill_fetched", t("unconfirmed"))])); }
    }
    let mut out: Update = vec![("meta_backfill_fetched", t(b.today))];
    let has = |out: &Update, k: &str| out.iter().any(|(c, _)| *c == k);
    if !is_steam && row.steam_appid.is_some() && steam_appid.is_none() { out.push(("steam_appid", Sql::Null)); }
    const BOX: [&str; 4] = ["developers", "publishers", "genres", "categories"];

    if let Some(sid) = steam_appid {
        if !is_steam { out.push(("steam_appid", Sql::Integer(sid))); }
        let store = b.steam.store_data(sid, dates(sid))?;
        let reviews = b.steam.reviews(sid)?;
        let tags = b.steam.tags(sid)?;
        if let Some(s) = &store {
            for f in BOX { if row.gap(f) { let v = match f { "developers" => &s.data.developers, "publishers" => &s.data.publishers, "genres" => &s.data.genres, _ => &s.data.categories }; if !v.trim().is_empty() { out.push((f_static(f), t(v))); } } }
        }
        if row.gap("tags") { if let Some(tg) = tags.as_ref().filter(|x| !x.is_empty()) { out.push(("tags", t(&tg.join(",")))); } }
        if let Some(s) = &store {
            if row.release_date.is_none() { if let Some(d) = s.release_date { out.push(("release_date", Sql::Integer(d))); } }
            if row.metacritic_score.is_none() { if let Some(m) = s.data.metacritic_score { out.push(("metacritic_score", Sql::Integer(m))); } }
            if row.short_description.as_deref().map_or(true, str::is_empty) { if let Some(d) = &s.data.short_description { out.push(("short_description", t(d))); } }
            if row.is_free.is_none() { out.push(("is_free", Sql::Integer(s.data.is_free))); }
        }
        if is_steam && row.total_achievements.is_none() { out.extend(achievement_counts(b.steam, appid, b.today)?); }
        if empty(&row.review_score) { if let Some(r) = reviews.as_ref().filter(|r| r.total_reviews != 0) {
            out.extend([("review_score", t(&r.review_score)), ("review_percentage", Sql::Integer(r.review_percentage)), ("weighted_percentage", Sql::Integer(r.weighted_percentage)), ("total_reviews", Sql::Integer(r.total_reviews)), ("positive_reviews", Sql::Integer(r.positive_reviews))]);
        } }
        if pcgw_box.is_none() && BOX.iter().any(|f| !has(&out, f) && row.gap(f)) { pcgw_box = b.pcgw.infobox(&row.name).unwrap_or(None); }
        if let Some(bx) = &pcgw_box {
            for f in BOX { if !has(&out, f) && row.gap(f) { if let Some(v) = bx.field(f) { out.push((f_static(f), t(v))); } } }
            if !has(&out, "release_date") && row.release_date.is_none() { if let Some(d) = bx.release_date.filter(|d| *d != 0) { out.push(("release_date", Sql::Integer(d))); } }
        }
    } else if let Some(bx) = &pcgw_box {
        for f in BOX { if row.gap(f) { if let Some(v) = bx.field(f) { out.push((f_static(f), t(v))); } } }
        if row.release_date.is_none() { if let Some(d) = bx.release_date.filter(|d| *d != 0) { out.push(("release_date", Sql::Integer(d))); } }
    }

    if steam_appid.is_none() && !row.platform_slug.is_empty() {
        let page = b.plugins.store_page_metadata(row.platform.as_deref().unwrap_or(""), &row.platform_slug);
        let pairs: [(&'static str, &Option<String>); 5] = [("genres", &page.genres), ("categories", &page.categories), ("tags", &page.tags), ("developers", &page.developers), ("publishers", &page.publishers)];
        for (f, v) in pairs { if !has(&out, f) && row.gap(f) { if let Some(v) = v.as_ref().filter(|v| !v.is_empty()) { out.push((f, t(v))); } } }
        if !has(&out, "release_date") && row.release_date.is_none() { if let Some(d) = page.release_date.filter(|d| *d != 0) { out.push(("release_date", Sql::Integer(d))); } }
    }
    if !has(&out, "short_description") && empty(&row.short_description) && !is_steam {
        let d = crate::steamweb::clean_description(&b.plugins.description(row.platform.as_deref().unwrap_or(""), appid, &row.platform_id));
        if !d.is_empty() { out.push(("short_description", t(&d))); }
    }
    let filled = out.iter().filter(|(c, _)| *c != "steam_appid" && *c != "meta_backfill_fetched").count();
    if filled == 0 && steam_appid.is_none() {
        let mut o: Update = vec![("meta_backfill_fetched", t("no_match"))];
        if row.steam_appid.is_some() { o.push(("steam_appid", Sql::Null)); }
        return Ok(Some(o));
    }
    Ok(Some(out))
}
fn f_static(f: &str) -> &'static str { match f { "developers" => "developers", "publishers" => "publishers", "genres" => "genres", _ => "categories" } }

/// `_achievement_counts`: the account's own progress, else the public schema total with 0 unlocked; empty with no key or on failure.
pub fn achievement_counts(steam: &Steam, appid: i64, today: &str) -> Fetch<Update> {
    let (total, unlocked) = match steam.cheevos(appid)? { Some(c) => (c.total, c.unlocked), None => match steam.schema_total(appid)? { Some(n) => (n, 0), None => return Ok(vec![]) } };
    Ok(vec![("total_achievements", Sql::Integer(total)), ("unlocked_achievements", Sql::Integer(unlocked)), ("cheevos_fetched", t(today))])
}

#[allow(dead_code)] fn _rl(_: RateLimited) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{NetError, Response};

    /// Answers by URL substring (first rule that matches); anything else is a connection failure.
    struct Fake(Vec<(&'static str, Result<Response, NetError>)>);
    impl Http for Fake { fn get(&self, u: &str, _h: &[(&str, &str)], _t: Duration) -> Result<Response, NetError> { self.0.iter().find(|(k, _)| u.contains(k)).map(|(_, v)| v.clone()).unwrap_or(Err(NetError("no rule".into()))) } }
    fn ok(b: &str) -> Result<Response, NetError> { Ok(Response { status: 200, retry_after: None, body: b.into() }) }
    fn http(status: u16) -> Result<Response, NetError> { Ok(Response { status, retry_after: None, body: String::new() }) }
    const DETAILS: &str = r#"{"620":{"success":true,"data":{"name":"Portal 2","type":"game","steam_appid":620,"developers":["Valve"],"publishers":["Valve"],"genres":[{"description":"Action"}],"categories":[{"description":"Single-player"}],"is_free":false,"short_description":"Hi","release_date":{"date":"19 Apr, 2011"}}}}"#;
    const REVIEWS: &str = r#"{"response":{"query_summary":{"total_reviews":100,"total_positive":90,"review_score_desc":"Very Positive"}}}"#;
    const TAGS: &str = r#"<a class="app_tag">Puzzle</a>"#;
    const WIKI_HADES: &str = r#"{"parse":{"wikitext":{"*":"{{Infobox game\n{{Infobox game/row/developer|Supergiant}}\n|steam appid = 1145360\n}}"}}}"#;
    const WIKI_OLD: &str = r#"{"parse":{"wikitext":{"*":"{{Infobox game\n{{Infobox game/row/developer|Bullfrog}}\n{{Infobox game/row/date|DOS|1993}}\n}}"}}}"#;
    const NO_PAGE: &str = r#"{"error":{"code":"missingtitle"}}"#;

    fn run(rules: Vec<(&'static str, Result<Response, NetError>)>, row: Row, force: bool) -> Fetch<Option<Update>> {
        let h: Arc<dyn Http> = Arc::new(Fake(rules));
        let steam = Steam::new(h.clone(), None, 10.0);
        let pcgw = Pcgw::with_timing(h.clone(), Duration::ZERO, Duration::from_secs(60));
        let b = Backfill { steam: &steam, http: h.as_ref(), pcgw: &pcgw, plugins: &NoPlugins, today: "2026-11-01" };
        backfill(&b, if row.platform.as_deref().unwrap_or("steam") == "steam" { 620 } else { -3 }, &row, &|_| AppinfoDates { original: Some(1303171200), steam: None }, force, false)
    }
    fn cols(u: &Update) -> Vec<&'static str> { u.iter().map(|(c, _)| *c).collect() }
    fn get<'a>(u: &'a Update, c: &str) -> Option<&'a Sql> { u.iter().find(|(k, _)| *k == c).map(|(_, v)| v) }

    #[test] fn steam_game_fills_only_blanks_and_stamps() {
        let row = Row { name: "Portal 2".into(), platform: Some("steam".into()), developers: Some("Mine".into()), ..Default::default() };
        let u = run(vec![("appdetails", ok(DETAILS)), ("GetAppReviews", ok(REVIEWS)), ("/app/620/", ok(TAGS))], row, false).unwrap().unwrap();
        assert_eq!(cols(&u), ["meta_backfill_fetched", "publishers", "genres", "categories", "tags", "release_date", "short_description", "is_free", "review_score", "review_percentage", "weighted_percentage", "total_reviews", "positive_reviews"]);
        assert!(get(&u, "developers").is_none(), "an existing value is never overwritten");
    }
    #[test] fn already_done_is_skipped_unless_rerun() {
        let row = Row { name: "x".into(), platform: Some("steam".into()), meta_backfill_fetched: Some("2026-10-01".into()), ..Default::default() };
        assert_eq!(run(vec![], row, false), Ok(None));
    }
    #[test] fn non_steam_resolves_by_exact_name_through_steam_search() {
        let row = Row { name: "Portal 2".into(), platform: Some("gog".into()), ..Default::default() };
        let search = r#"{"items":[{"type":"app","id":999,"name":"Portal 2 Soundtrack"},{"type":"app","id":620,"name":"Portal 2"}]}"#;
        let u = run(vec![("pcgamingwiki", ok(NO_PAGE)), ("storesearch", ok(search)), ("appdetails", ok(DETAILS)), ("GetAppReviews", ok(REVIEWS)), ("/app/620/", ok(TAGS))], row, false).unwrap().unwrap();
        assert_eq!(get(&u, "steam_appid"), Some(&Sql::Integer(620))); assert_eq!(get(&u, "tags"), Some(&Sql::Text("Puzzle".into())));
    }
    #[test] fn a_maybe_is_parked_not_applied_until_forced() {
        let row = Row { name: "Close Name Game Deluxe".into(), platform: Some("gog".into()), ..Default::default() };
        let search = r#"{"items":[{"type":"app","id":620,"name":"Close Name Game"}]}"#;
        let rules = || vec![("pcgamingwiki", ok(NO_PAGE)), ("storesearch", ok(search)), ("appdetails", ok(DETAILS)), ("GetAppReviews", ok(REVIEWS)), ("/app/620/", ok(TAGS))];
        let u = run(rules(), row.clone(), false).unwrap().unwrap();
        assert_eq!(cols(&u), ["steam_appid", "meta_backfill_fetched"]); assert_eq!(get(&u, "meta_backfill_fetched"), Some(&Sql::Text("unconfirmed".into())));
        let u = run(rules(), row, true).unwrap().unwrap();
        assert!(get(&u, "tags").is_some() && get(&u, "steam_appid") == Some(&Sql::Integer(620)));
    }
    #[test] fn a_delisted_game_takes_what_pcgw_has() {
        let row = Row { name: "Syndicate (1993)".into(), platform: Some("gog".into()), ..Default::default() };
        let u = run(vec![("pcgamingwiki", ok(WIKI_OLD)), ("storesearch", ok(r#"{"items":[]}"#))], row, false).unwrap().unwrap();
        assert_eq!(cols(&u), ["meta_backfill_fetched", "developers", "release_date"]); assert!(get(&u, "steam_appid").is_none());
    }
    #[test] fn pcgw_page_with_a_steam_id_wins_over_search() {
        let row = Row { name: "Hades".into(), platform: Some("gog".into()), ..Default::default() };
        let details = DETAILS.replace("620", "1145360");
        let u = run(vec![("pcgamingwiki", ok(WIKI_HADES)), ("appdetails", ok(&details)), ("GetAppReviews", ok(REVIEWS)), ("/app/1145360/", ok(TAGS))], row, false).unwrap().unwrap();
        assert_eq!(get(&u, "steam_appid"), Some(&Sql::Integer(1145360)));
    }
    #[test] fn nothing_anywhere_is_no_match_and_clears_a_stale_id() {
        let row = Row { name: "Totally Unknown".into(), platform: Some("gog".into()), steam_appid: Some(5), ..Default::default() };
        let u = run(vec![("pcgamingwiki", ok(NO_PAGE)), ("storesearch", ok(r#"{"items":[]}"#))], row, true).unwrap().unwrap();
        assert_eq!(u, vec![("meta_backfill_fetched", Sql::Text("no_match".into())), ("steam_appid", Sql::Null)]);
    }
    #[test] fn throttled_pcgw_and_no_steam_answer_means_retry_later() {
        let row = Row { name: "Whatever".into(), platform: Some("gog".into()), ..Default::default() };
        assert_eq!(run(vec![("pcgamingwiki", http(429)), ("storesearch", ok(r#"{"items":[]}"#))], row, false), Ok(None));
    }
    #[test] fn steam_throttling_propagates() {
        let row = Row { name: "Portal 2".into(), platform: Some("steam".into()), ..Default::default() };
        assert!(run(vec![("appdetails", http(429))], row, false).is_err());
    }
    #[test] fn pcgw_stands_down_after_a_429() {
        let h = Arc::new(Fake(vec![("pcgamingwiki", http(429))]));
        let p = Pcgw::with_timing(h, Duration::ZERO, Duration::from_secs(60));
        assert_eq!(p.infobox("A Game"), Err(Unavailable));
        // second call must not even ask: swap in a failing-loud fake via a fresh client sharing nothing, so check the cooldown flag directly
        assert!(p.st.lock().unwrap().1.is_some());
    }
}
