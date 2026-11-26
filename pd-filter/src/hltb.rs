//! HowLongToBeat, the pure parts (a port of the matching in `scrapers.py` and of what `howlongtobeatpy` does with a search answer). The requests themselves
//! (finding the search endpoint in the site's scripts, the auth token, the POST) belong to the network client. Similarity is Python's `difflib.SequenceMatcher`
//! ratio reimplemented exactly, because the match thresholds (75 / 99) were tuned against it.
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::OnceLock;

/// `SequenceMatcher(None, a, b).ratio()` with the default `autojunk=True`.
pub fn sequence_ratio(a: &str, b: &str) -> f64 {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let (la, lb) = (a.len(), b.len());
    if la + lb == 0 { return 1.0; }
    // chain b: index of every element; with 200 or more elements the popular ones (more than n/100 + 1 occurrences) are dropped
    let mut b2j: HashMap<char, Vec<usize>> = HashMap::new();
    for (j, c) in b.iter().enumerate() { b2j.entry(*c).or_default().push(j); }
    if lb >= 200 { let ntest = lb / 100 + 1; b2j.retain(|_, v| v.len() <= ntest); }
    let longest = |alo: usize, ahi: usize, blo: usize, bhi: usize| -> (usize, usize, usize) {
        let (mut besti, mut bestj, mut bestsize) = (alo, blo, 0usize);
        let mut j2len: HashMap<usize, usize> = HashMap::new();
        for i in alo..ahi {
            let mut new: HashMap<usize, usize> = HashMap::new();
            if let Some(js) = b2j.get(&a[i]) {
                for &j in js {
                    if j < blo { continue; }
                    if j >= bhi { break; }
                    let k = j2len.get(&(j.wrapping_sub(1))).copied().unwrap_or(0) + 1;
                    new.insert(j, k);
                    if k > bestsize { besti = i + 1 - k; bestj = j + 1 - k; bestsize = k; }
                }
            }
            j2len = new;
        }
        // extend the match over elements that were dropped as popular (no explicit junk exists here)
        while besti > alo && bestj > blo && a[besti - 1] == b[bestj - 1] { besti -= 1; bestj -= 1; bestsize += 1; }
        while besti + bestsize < ahi && bestj + bestsize < bhi && a[besti + bestsize] == b[bestj + bestsize] { bestsize += 1; }
        (besti, bestj, bestsize)
    };
    let mut queue = vec![(0usize, la, 0usize, lb)];
    let mut matches = 0usize;
    while let Some((alo, ahi, blo, bhi)) = queue.pop() {
        let (i, j, k) = longest(alo, ahi, blo, bhi);
        if k > 0 {
            matches += k;
            if alo < i && blo < j { queue.push((alo, i, blo, j)); }
            if i + k < ahi && j + k < bhi { queue.push((i + k, ahi, j + k, bhi)); }
        }
    }
    2.0 * matches as f64 / (la + lb) as f64
}

/// `JSONResultParser.similar`: the ratio, minus 0.1 when the searched name has numbers and the candidate has none of them.
pub fn similar(a: Option<&str>, b: Option<&str>, numbers: &[String], case_sensitive: bool) -> f64 {
    static CLEAN: OnceLock<Regex> = OnceLock::new();
    let (Some(a), Some(b)) = (a, b) else { return 0.0 };
    let mut sim = if case_sensitive { sequence_ratio(a, b) } else { sequence_ratio(&a.to_lowercase(), &b.to_lowercase()) };
    if !numbers.is_empty() {
        let cleaned = CLEAN.get_or_init(|| Regex::new(r"([^\s\w]|_)+").unwrap()).replace_all(b, "").into_owned();
        let found = cleaned.split(' ').any(|w| !w.is_empty() && w.chars().all(|c| c.is_numeric()) && numbers.iter().any(|n| n == w));
        if !found { sim -= 0.1; }
    }
    sim
}

/// The numbers (whole space-separated digit words) in the searched name.
pub fn name_numbers(name: &str) -> Vec<String> { name.split(' ').filter(|w| !w.is_empty() && w.chars().all(|c| c.is_ascii_digit())).map(String::from).collect() }

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Entry {
    pub game_id: i64, pub game_name: String, pub game_alias: Option<String>,
    pub main_story: Option<f64>, pub main_extra: Option<f64>, pub completionist: Option<f64>,
    pub similarity: f64,
}

fn hours(v: &Value, k: &str) -> Option<f64> { v.get(k).and_then(|x| x.as_f64()).map(|s| (s / 3600.0 * 100.0).round() / 100.0) }

/// What the library does with a search answer: every game in `data`, with its times in hours and its similarity to `searched` (case-insensitive).
/// `min_similarity` 0 keeps everything (what `fetch_hltb_data` asks for); `only_id` keeps just that game and ignores similarity.
pub fn parse_search(body: &str, searched: &str, case_sensitive: bool, min_similarity: f64, only_id: Option<i64>) -> Option<Vec<Entry>> {
    let json: Value = serde_json::from_str(body).ok()?;
    let numbers = name_numbers(searched);
    let (case_sensitive, min_similarity) = if only_id.is_some() { (false, 0.0) } else { (case_sensitive, min_similarity) };
    let mut out = Vec::new();
    for g in json.get("data")?.as_array()? {
        let id = g.get("game_id").and_then(|v| v.as_i64()).unwrap_or(-1);
        if only_id.map_or(false, |w| w != id) { continue; }
        let name = g.get("game_name").and_then(|v| v.as_str());
        let alias = g.get("game_alias").and_then(|v| v.as_str());
        let sim = similar(Some(searched), name, &numbers, case_sensitive).max(similar(Some(searched), alias, &numbers, case_sensitive));
        let e = Entry { game_id: id, game_name: name.unwrap_or("").to_string(), game_alias: alias.map(String::from), main_story: hours(g, "comp_main"), main_extra: hours(g, "comp_plus"), completionist: hours(g, "comp_100"), similarity: sim };
        if min_similarity.abs() < 1e-9 || e.similarity >= min_similarity { out.push(e); }
    }
    Some(out)
}

// ---------------------------------------------------------------- name handling (scrapers.py)

/// `_hltb_clean_name`: the title as HowLongToBeat spells it: no (R)/(TM), no trailing "(2010)", punctuation turned into spaces.
pub fn clean_name(name: &str) -> String {
    static SOK: OnceLock<Regex> = OnceLock::new(); static WS: OnceLock<Regex> = OnceLock::new(); static PUNCT: OnceLock<Regex> = OnceLock::new();
    let name: String = name.chars().take(500).collect();
    let name: String = name.chars().filter(|c| !matches!(c, '®' | '™' | '©')).collect();
    let mut name = name.trim_end().to_string();
    let cs: Vec<char> = name.chars().collect();
    if name.ends_with(')') && cs.len() >= 6 && cs[cs.len() - 6] == '(' && cs[cs.len() - 5..cs.len() - 1].iter().all(|c| c.is_ascii_digit()) { name = cs[..cs.len() - 6].iter().collect::<String>().trim_end().to_string(); }
    let name = PUNCT.get_or_init(|| Regex::new(r"[^\w\s]").unwrap()).replace_all(&name, " ").into_owned();
    let name = SOK.get_or_init(|| Regex::new(r"(?i)\bsokpop\s+s\d+\b").unwrap()).replace_all(&name, "").into_owned();
    WS.get_or_init(|| Regex::new(r"\s{2,}").unwrap()).replace_all(&name, " ").trim().to_string()
}

/// `_hltb_strip_edition`: a trailing edition word Steam adds and HowLongToBeat omits ("Definitive Edition", "Remastered", "GOTY"...).
pub fn strip_edition(name: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(?i)\s+(?:definitive|remastered|hd remaster|hd edition|game of the year|goty|complete|ultimate|enhanced|special|anniversary|directors? cut|deluxe|gold|platinum|standard|extended|legendary)(?:\s+(?:edition|version|cut))?\s*$").unwrap());
    let stripped = re.replace(name, "").trim().to_string();
    if stripped != name { stripped } else { name.to_string() }
}

/// `_hours_to_minutes`: hours (a float) to whole minutes, None for a missing or negative value. Rounds halves to even like Python.
pub fn hours_to_minutes(v: Option<f64>) -> Option<i64> {
    let v = v.filter(|v| *v >= 0.0)? * 60.0;
    let f = v.floor();
    Some(if (v - f - 0.5).abs() < 1e-9 { if f % 2.0 == 0.0 { f } else { f + 1.0 } } else { v.round() } as i64)
}
