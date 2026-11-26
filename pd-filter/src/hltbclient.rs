//! The HowLongToBeat client (what `howlongtobeatpy` does for `scrapers.fetch_hltb_data` / `fetch_hltb_by_id` / `search_hltb_results`).
//! The library re-downloads the site's home page and every script on it before EVERY lookup to find the search endpoint, then asks for a token.
//! Here the endpoint is found once and cached; it is looked up again only when a search fails with the cached one. The token is still fetched per
//! search (it is short-lived). `None`/`Err(Unreachable)` means "HLTB did not answer", never "no match": callers must not clear stored data on it.
use crate::hltb::{clean_name, hours_to_minutes, parse_search, strip_edition, Entry};
use crate::net::{enc, Http};
use scraper::{Html, Selector};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const BASE: &str = "https://howlongtobeat.com/";
const UA: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:128.0) Gecko/20100101 Firefox/128.0";
const T: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq)]
pub struct Unreachable;

/// A matched game, times in minutes. `fetched` is "unconfirmed" below the threshold (times None) or the date.
#[derive(Debug, Clone, PartialEq)]
pub struct HltbData { pub id: i64, pub matched_name: String, pub main: Option<i64>, pub extras: Option<i64>, pub completionist: Option<i64>, pub score: i64, pub fetched: String }

/// `fetch_hltb_by_id`'s answer when HLTB was reached.
#[derive(Debug, Clone, PartialEq)]
pub struct ById { pub matched_name: Option<String>, pub main: Option<i64>, pub extras: Option<i64>, pub completionist: Option<i64>, pub times_available: bool }

struct Auth { token: String, key: Option<String>, value: Option<String> }

pub struct Hltb { http: Arc<dyn Http>, endpoint: Mutex<Option<String>> }

impl Hltb {
    pub fn new(http: Arc<dyn Http>) -> Self { Hltb { http, endpoint: Mutex::new(None) } }

    fn get(&self, url: &str) -> Option<String> {
        match self.http.get(url, &[("User-Agent", UA), ("Referer", BASE)], T) { Ok(r) if r.status == 200 => Some(r.body), Ok(_) => None, Err(e) => { crate::net::log(&format!("[hltb] {e}")); None } }
    }

    /// `SearchInformations`: the path of the POST search endpoint inside one of the site's scripts, e.g. `api/finder`.
    pub fn endpoint_from_script(js: &str) -> Option<String> {
        use std::sync::OnceLock;
        static RE: OnceLock<regex::Regex> = OnceLock::new();
        let re = RE.get_or_init(|| regex::Regex::new(r#"(?is)fetch\s*\(\s*["']/api/([a-zA-Z0-9_/]+)[^"']*["']\s*,\s*\{[^}]*method:\s*["']POST["'][^}]*\}"#).unwrap());
        re.captures(js).map(|c| format!("api/{}", c[1].trim_end_matches('/')))
    }

    /// Scripts under `/_next/static/chunks/` listed on the home page.
    pub fn script_urls(home: &str) -> Vec<String> {
        let doc = Html::parse_document(home);
        let sel = Selector::parse("script[src]").unwrap();
        doc.select(&sel).filter_map(|s| s.value().attr("src")).filter(|s| s.contains("/_next/static/chunks/")).map(|s| if s.starts_with("http") { s.to_string() } else { format!("{BASE}{}", s.trim_start_matches('/')) }).collect()
    }

    fn discover(&self) -> Option<String> {
        let home = self.get(BASE)?;
        Self::script_urls(&home).into_iter().find_map(|u| self.get(&u).and_then(|js| Self::endpoint_from_script(&js)))
    }

    /// The cached endpoint, found on first use. Falls back to the library's static default when the scripts do not show one.
    fn endpoint(&self, refresh: bool) -> (String, bool) {
        let mut g = self.endpoint.lock().unwrap();
        if refresh { *g = None; }
        if let Some(e) = g.as_ref() { return (e.clone(), true); }
        let found = self.discover();
        let e = found.clone().unwrap_or_else(|| "api/s".into());
        if found.is_some() { *g = found; }
        (e, false)
    }

    fn auth(&self, path: &str) -> Option<Auth> {
        let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_millis();
        let body = self.get(&format!("{BASE}{path}/init?t={ms}"))?;
        let j: Value = serde_json::from_str(&body).ok()?;
        let o = j.as_object()?;
        let (mut key, mut value) = (None, None);
        for (k, v) in o {
            let lk = k.to_lowercase();
            let sv = v.as_str().map(String::from).unwrap_or_else(|| v.to_string());
            if lk.contains("key") { key = Some(sv) } else if lk.contains("val") { value = Some(sv) }
        }
        Some(Auth { token: o.get("token").map(|t| t.as_str().map(String::from).unwrap_or_else(|| t.to_string())).unwrap_or_else(|| "None".into()), key, value })
    }

    fn payload(name: &str, a: Option<&Auth>) -> String {
        let mut p = json!({"searchType": "games", "searchTerms": name.split_whitespace().collect::<Vec<_>>(), "searchPage": 1, "size": 20,
            "searchOptions": {"games": {"userId": 0, "platform": "", "sortCategory": "popular", "rangeCategory": "main", "rangeTime": {"min": 0, "max": 0},
                "gameplay": {"perspective": "", "flow": "", "genre": "", "difficulty": ""}, "rangeYear": {"max": "", "min": ""}, "modifier": ""},
                "users": {"sortCategory": "postcount"}, "lists": {"sortCategory": "follows"}, "filter": "", "sort": 0, "randomizer": 0}, "useCache": true});
        if let Some(Auth { key: Some(k), value, .. }) = a { p[k] = json!(value.clone().unwrap_or_else(|| "None".into())); }
        p.to_string()
    }

    fn post_search(&self, path: &str, name: &str) -> Option<String> {
        let a = self.auth(path);
        let mut h: Vec<(&str, String)> = vec![("content-type", "application/json".into()), ("accept", "*/*".into()), ("User-Agent", UA.into()), ("Referer", BASE.into()), ("Origin", BASE.into())];
        if let Some(a) = &a { h.push(("x-auth-token", a.token.clone())); h.push(("x-hp-key", a.key.clone().unwrap_or_else(|| "None".into()))); h.push(("x-hp-val", a.value.clone().unwrap_or_else(|| "None".into()))); }
        let hs: Vec<(&str, &str)> = h.iter().map(|(k, v)| (*k, v.as_str())).collect();
        match self.http.post(&format!("{BASE}{path}"), &hs, &Self::payload(name, a.as_ref()), T) { Ok(r) if r.status == 200 => Some(r.body), Ok(_) => None, Err(e) => { crate::net::log(&format!("[hltb] {e}")); None } }
    }

    /// The raw answer of a name search. A failure with a cached endpoint refreshes it once (the site moves it now and then).
    fn raw(&self, name: &str) -> Option<String> {
        let (path, cached) = self.endpoint(false);
        if let Some(b) = self.post_search(&path, name) { return Some(b); }
        if !cached { return None; }
        let (path, _) = self.endpoint(true);
        self.post_search(&path, name)
    }

    /// `HowLongToBeat(0.0).search(query, similarity_case_sensitive=False)`: None = no answer.
    pub fn search(&self, query: &str) -> Option<Vec<Entry>> {
        if query.is_empty() { return None; }
        parse_search(&self.raw(query)?, query, false, 0.0, None)
    }

    /// `HowLongToBeat().search_from_id`: the game page's title, then a name search keeping only that id (exactly one hit).
    pub fn search_from_id(&self, id: i64) -> Option<Entry> {
        if id == 0 { return None; }
        let page = self.get(&format!("{BASE}game?id={}", enc(&id.to_string())))?;
        let title = cut_title(&page)?;
        let hits = parse_search(&self.raw(&title)?, &title, true, 0.4, Some(id))?;
        (hits.len() == 1).then(|| hits.into_iter().next().unwrap())
    }

    fn best(&self, query: &str) -> Result<Option<(Entry, i64)>, Unreachable> {
        let Some(r) = self.search(query) else { return Err(Unreachable) };
        // `max(results, key=similarity)`: the first of equal maxima
        // a title that differs only in punctuation and case ("Neverwinter Nights Diamond" and "Neverwinter Nights: Diamond") is the same title: it scores 100, not 98
        let alnum = |t: &str| -> String { t.chars().filter(|c| c.is_alphanumeric()).flat_map(|c| c.to_lowercase()).collect() };
        let q = alnum(query);
        let sim = |e: &Entry| if !q.is_empty() && alnum(&e.game_name) == q { 1.0 } else { e.similarity };
        let mut best: Option<&Entry> = None;
        for e in &r { if best.map_or(true, |b| sim(e) > sim(b)) { best = Some(e); } }
        Ok(best.map(|b| (b.clone(), (sim(b) * 100.0).round_ties_even() as i64)))
    }

    /// `fetch_hltb_data`: Ok(None) = reached, nothing found; Err = could not reach HLTB.
    pub fn fetch_data(&self, name: &str, threshold: i64, today: &str) -> Result<Option<HltbData>, Unreachable> {
        let cleaned = clean_name(name);
        let (mut best, mut score) = match self.best(&cleaned)? { Some((b, s)) => (Some(b), s), None => (None, 0) };
        let mut paren_used = false;
        if best.is_none() || score < threshold {
            let mut seen = vec![cleaned.clone()];
            let mut cands: Vec<(String, bool)> = vec![];
            let shorter = strip_edition(&cleaned);
            if !seen.contains(&shorter) { seen.push(shorter.clone()); cands.push((shorter, false)); }
            let np = clean_name(&paren_free(name));
            if !np.is_empty() && !seen.contains(&np) { cands.push((np, true)); }
            let mut best_eff = score;
            for (q, pen) in cands {
                let Ok(r) = self.best(&q) else { continue };
                let Some((alt, alt_score)) = r else { continue };
                let eff = alt_score - if pen { 15 } else { 0 };
                if eff > best_eff { best = Some(alt); score = alt_score; paren_used = pen; best_eff = eff; }
            }
        }
        let Some(b) = best else { return Ok(None) };
        let eff = if paren_used { score - 15 } else { score };
        if eff < threshold {
            return Ok(Some(HltbData { id: b.game_id, matched_name: b.game_name, main: None, extras: None, completionist: None, score, fetched: "unconfirmed".into() }));
        }
        Ok(Some(HltbData { id: b.game_id, matched_name: b.game_name, main: hours_to_minutes(b.main_story), extras: hours_to_minutes(b.main_extra), completionist: hours_to_minutes(b.completionist), score, fetched: today.into() }))
    }

    /// `fetch_hltb_by_id`: direct lookup, then a name search for that id.
    pub fn fetch_by_id(&self, name: &str, id: i64) -> Result<ById, Unreachable> {
        let mk = |e: &Entry| ById { matched_name: Some(e.game_name.clone()), main: hours_to_minutes(e.main_story), extras: hours_to_minutes(e.main_extra), completionist: hours_to_minutes(e.completionist), times_available: true };
        if let Some(e) = self.search_from_id(id) { return Ok(mk(&e)); }
        match self.search(name) {
            None => Err(Unreachable),
            Some(r) => Ok(r.iter().find(|e| e.game_id == id).map(mk).unwrap_or(ById { matched_name: None, main: None, extras: None, completionist: None, times_available: false })),
        }
    }

    /// `search_hltb_results`: up to 8 candidates for the user to choose from, best first. Empty on any failure.
    pub fn candidates(&self, name: &str) -> Vec<HltbData> {
        let cleaned = clean_name(name);
        let mut r = self.search(&cleaned).unwrap_or_default();
        if r.is_empty() { let s = strip_edition(&cleaned); if s != cleaned { r = self.search(&s).unwrap_or_default(); } }
        r.sort_by(|a, b| b.similarity.partial_cmp(&a.similarity).unwrap_or(std::cmp::Ordering::Equal));   // stable, like Python's sorted(reverse=True)
        r.into_iter().take(8).map(|e| HltbData { id: e.game_id, matched_name: e.game_name.clone(), main: hours_to_minutes(e.main_story), extras: hours_to_minutes(e.main_extra), completionist: hours_to_minutes(e.completionist), score: (e.similarity * 100.0).round_ties_even() as i64, fetched: String::new() }).collect()
    }
}

/// `_load_steam_hltb_map`: {steam appid: HLTB id}, from a third party's GitHub file, cached in `steam_hltb_map.json` for 7 days. Differences from Python: a
/// download only replaces the cache when it is a JSON object (Python wrote whatever came), and when the download fails a stale cache is still used
/// (Python used an empty map, so every game went through the name search).
pub fn steam_hltb_map(dir: &std::path::Path, http: &dyn Http) -> std::collections::HashMap<i64, i64> {
    const URL: &str = "https://raw.githubusercontent.com/zpangwin/sg-data/master/steam-to-hltb-mappings/steam-to-hltb-map.min.json";
    let path = dir.join("steam_hltb_map.json");
    let age = std::fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).map_or(u64::MAX, |d| d.as_secs());
    if age > 7 * 86400 {
        match http.get(URL, &[("User-Agent", "Mozilla/5.0")], Duration::from_secs(30)) {
            Ok(r) if r.status == 200 && serde_json::from_str::<Value>(&r.body).map_or(false, |v| v.is_object()) => { let _ = std::fs::write(&path, &r.body); crate::net::log("[hltb] steam_hltb_map refreshed from GitHub"); }
            Ok(r) => crate::net::log(&format!("[hltb] steam_hltb_map download gave HTTP {}", r.status)),
            Err(e) => crate::net::log(&format!("[hltb] could not download steam_hltb_map: {e}")),
        }
    }
    let Some(Value::Object(o)) = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()) else { return Default::default() };
    o.iter().filter_map(|(k, v)| Some((k.parse().ok()?, v.as_i64()?))).collect()
}

/// `re.sub(r'\s*[\(\[][^\)\]]*[\)\]]\s*', ' ', name)`
fn paren_free(name: &str) -> String {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"\s*[\(\[][^\)\]]*[\)\]]\s*").unwrap()).replace_all(name, " ").into_owned()
}

/// `__cut_game_title`: "How long is A Way Out? | HowLongToBeat" -> "A Way Out" (`title[12:-17].strip()`, by characters).
fn cut_title(page: &str) -> Option<String> {
    let doc = Html::parse_document(page);
    let t: Vec<char> = doc.select(&Selector::parse("title").ok()?).next()?.text().collect::<String>().chars().collect();
    if t.len() < 29 { return Some(String::new()).filter(|s| !s.is_empty()); }
    let s: String = t[12..t.len() - 17].iter().collect();
    Some(s.trim().to_string()).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{NetError, Response};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test] fn endpoint_and_scripts() {
        assert_eq!(Hltb::endpoint_from_script(r#"x;fetch("/api/finder/v2?x=1",{method:"POST",headers:{a:1}});"#), Some("api/finder/v2".into()));
        assert_eq!(Hltb::endpoint_from_script(r#"fetch("/api/other",{method:"GET"})"#), None);
        let home = r#"<script src="/_next/static/chunks/a.js"></script><script src="/other.js"></script><script src="https://x/_next/static/chunks/b.js"></script>"#;
        assert_eq!(Hltb::script_urls(home), ["https://howlongtobeat.com/_next/static/chunks/a.js", "https://x/_next/static/chunks/b.js"]);
    }
    #[test] fn title_cut() {
        assert_eq!(cut_title("<title>How long is A Way Out? | HowLongToBeat</title>"), Some("A Way Out".into()));
    }
    #[test] fn paren_removal() { assert_eq!(clean_name(&paren_free("Game (2010) [GOTY] Edition")), "Game Edition"); }

    /// Counts requests per kind; serves a home page, one script, a token and a search answer.
    struct Site { gets: AtomicUsize, scripts: AtomicUsize, posts: AtomicUsize, fail_first_post: bool }
    impl Http for Site {
        fn get(&self, u: &str, _h: &[(&str, &str)], _t: Duration) -> Result<Response, NetError> {
            self.gets.fetch_add(1, Ordering::SeqCst);
            let ok = |b: &str| Ok(Response { status: 200, retry_after: None, body: b.into() });
            if u.ends_with(".js") { self.scripts.fetch_add(1, Ordering::SeqCst); ok(r#"fetch("/api/find",{method:"POST"})"#) }
            else if u.contains("/init?t=") { ok(r#"{"token":"tok","hpKey":"k1","hpVal":"v1"}"#) }
            else { ok(r#"<script src="/_next/static/chunks/a.js"></script>"#) }
        }
        fn post(&self, u: &str, h: &[(&str, &str)], body: &str, _t: Duration) -> Result<Response, NetError> {
            let n = self.posts.fetch_add(1, Ordering::SeqCst);
            if self.fail_first_post && n == 0 { return Ok(Response { status: 404, retry_after: None, body: String::new() }); }
            assert!(u.ends_with("/api/find")); assert!(h.contains(&("x-auth-token", "tok")) && h.contains(&("x-hp-key", "k1")) && h.contains(&("x-hp-val", "v1")));
            let j: Value = serde_json::from_str(body).unwrap(); assert_eq!(j["k1"], "v1"); assert!(j["searchTerms"].as_array().unwrap().len() >= 2);
            Ok(Response { status: 200, retry_after: None, body: r#"{"data":[{"game_id":7,"game_name":"Portal 2","comp_main":36000,"comp_plus":72000,"comp_100":108000},{"game_id":8,"game_name":"Portal","comp_main":10800}]}"#.into() })
        }
    }
    #[test] fn endpoint_is_found_once_and_found_again_after_a_failure() {
        let s = Arc::new(Site { gets: 0.into(), scripts: 0.into(), posts: 0.into(), fail_first_post: false });
        let h = Hltb::new(s.clone());
        let a = h.fetch_data("Portal 2", 75, "2026-11-01").unwrap().unwrap();
        assert_eq!((a.id, a.main, a.extras, a.completionist, a.score, a.fetched.as_str()), (7, Some(600), Some(1200), Some(1800), 100, "2026-11-01"));
        h.fetch_data("Portal 2", 75, "d").unwrap();
        assert_eq!(s.scripts.load(Ordering::SeqCst), 1, "the site's script was read once for two lookups");
    }
    /// A cached endpoint that stops answering is looked up again once.
    #[test] fn a_failing_cached_endpoint_is_refreshed() {
        let s = Arc::new(Site { gets: 0.into(), scripts: 0.into(), posts: 0.into(), fail_first_post: false });
        let h = Hltb::new(s.clone());
        h.search("Portal 2").unwrap();                       // discovers + caches (script read 1)
        s.posts.store(0, Ordering::SeqCst);
        let h2 = Hltb { http: Arc::new(Site { gets: 0.into(), scripts: 0.into(), posts: 0.into(), fail_first_post: true }), endpoint: Mutex::new(Some("api/find".into())) };
        assert!(h2.search("Portal 2").is_some(), "the 404 made it re-read the site and try again");
        assert_eq!(h2.endpoint.lock().unwrap().as_deref(), Some("api/find"));
    }
    #[test] fn unreachable_is_not_no_match() {
        struct Down; impl Http for Down { fn get(&self, _: &str, _: &[(&str, &str)], _: Duration) -> Result<Response, NetError> { Err(NetError("Dns".into())) } }
        let h = Hltb::new(Arc::new(Down));
        assert_eq!(h.fetch_data("Portal 2", 75, "d"), Err(Unreachable));
        assert_eq!(h.fetch_by_id("Portal 2", 7), Err(Unreachable));
        assert!(h.candidates("x").is_empty());
    }
    #[test] fn a_title_that_differs_only_in_punctuation_is_an_exact_match() {
        let s = Arc::new(Site { gets: 0.into(), scripts: 0.into(), posts: 0.into(), fail_first_post: false });
        let h = Hltb::new(s);
        let a = h.fetch_data("Portal: 2", 100, "d").unwrap().unwrap();
        assert_eq!((a.fetched.as_str(), a.score, a.matched_name.as_str()), ("d", 100, "Portal 2"), "the colon does not cost two points");
    }
    #[test] fn below_threshold_is_unconfirmed_without_times() {
        let s = Arc::new(Site { gets: 0.into(), scripts: 0.into(), posts: 0.into(), fail_first_post: false });
        let h = Hltb::new(s);
        let a = h.fetch_data("Portal 2 Deluxe Zzz Qqq", 99, "d").unwrap().unwrap();
        assert_eq!((a.fetched.as_str(), a.main), ("unconfirmed", None));
    }
    #[test] fn candidates_sorted_best_first() {
        let s = Arc::new(Site { gets: 0.into(), scripts: 0.into(), posts: 0.into(), fail_first_post: false });
        let c = Hltb::new(s).candidates("Portal 2");
        assert_eq!(c.iter().map(|x| x.id).collect::<Vec<_>>(), [7, 8]); assert_eq!(c[0].score, 100);
    }
}