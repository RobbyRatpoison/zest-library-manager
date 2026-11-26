//! Date Added from the store's licenses page (`store.steampowered.com/account/licenses/`): every license the account holds with the date it was added (a purchase, a key activation,
//! a free license), 100 to a page, newest first, so a whole library is ~50 requests where the Help page is one request per game. The rows name the license, not an app, so a row is
//! matched to a library game by title (exactly, after the cleanup below; never fuzzily: Quake 2 is not Quake). The dates agree with the Help page's (checked on 78 games).
use crate::net::Http;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq)]
pub struct Row { /// `YYYY-MM-DD`.
    pub date: String, pub item: String, pub how: String }

#[derive(Debug, Default, PartialEq)]
pub struct Page { pub rows: Vec<Row>, /// The `?continuationToken=...&offset=...` of the next page.
    pub next: Option<String>, /// The "of N" in "licenses 1-100 of N".
    pub total: Option<usize> }

fn clean(x: &str) -> String {
    static TAG: OnceLock<Regex> = OnceLock::new(); static REMOVE: OnceLock<Regex> = OnceLock::new();
    let rm = REMOVE.get_or_init(|| Regex::new(r#"(?s)<div class="free_license_remove_link">.*?</div>"#).unwrap());
    let tag = TAG.get_or_init(|| Regex::new(r"<[^>]+>").unwrap());
    let without = rm.replace_all(x, "").to_string();
    let s = tag.replace_all(&without, "");
    s.replace("&amp;", "&").replace("&#39;", "'").replace("&quot;", "\"").replace("&lt;", "<").replace("&gt;", ">").replace("&nbsp;", " ").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// One page of the table. A row whose date cannot be read is skipped.
pub fn parse_page(html: &str) -> Page {
    static ROW: OnceLock<Regex> = OnceLock::new(); static NEXT: OnceLock<Regex> = OnceLock::new(); static TOTAL: OnceLock<Regex> = OnceLock::new();
    let row = ROW.get_or_init(|| Regex::new(r#"(?s)<td\s+class="license_date_col">(.*?)</td>\s*<td>(.*?)</td>\s*<td class="license_acquisition_col">(.*?)</td>"#).unwrap());
    let next = NEXT.get_or_init(|| Regex::new(r#"license_paginator_next"\s+href="\?([^"]+)""#).unwrap());
    let total = TOTAL.get_or_init(|| Regex::new(r"licenses\s+\d+-\d+\s+of\s+(\d+)").unwrap());
    let rows = row.captures_iter(html).filter_map(|c| Some(Row { date: crate::steam_dates::parse_help_date(&clean(&c[1]), 2000)?, item: clean(&c[2]), how: clean(&c[3]) })).collect();
    Page { rows, next: next.captures(html).map(|c| c[1].replace("&amp;", "&")), total: total.captures(html).and_then(|c| c[1].parse().ok()) }
}

// ───────────────────────── matching a license title to a game title ─────────────────────────

/// Words that make a trailing `(...)` or `[...]` a region tag: WW, ROW, RoW, Global, Rest of World, NA, EU, LATAM, RU/CIS, WW ex CN, PRC-restricted, Not China, Key Only ...
/// (matched by word set, so a tag nobody has listed yet still counts).
fn is_region(t: &str) -> bool {
    const W: [&str; 51] = ["retail", "digital", "ww", "row", "global", "rest", "of", "world", "worldwide", "na", "eu", "emea", "latam", "cis", "ru", "apac", "asia", "cn", "prc", "china", "sea", "mena", "uk", "us", "usa", "sa", "br", "jp", "kr", "tr", "in", "ex", "only", "not",
        "restricted", "key", "region", "locked", "tier", "1", "2", "3", "dach", "anz", "oceania", "europe", "latin", "america", "russia", "korea", "japan"];
    let words: Vec<String> = t.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(String::from).collect();
    !words.is_empty() && words.iter().all(|w| W.contains(&w.as_str()))
}

/// The game's title out of a license title: the bundle-month suffix, the retail/region wrappers (`- Retail - [Digital] (RoW)`, `PC Digital`, a bare trailing `ROW`), `- Post Launch`.
pub fn strip_item(raw: &str) -> String {
    static MONTH: OnceLock<Regex> = OnceLock::new(); static GROUP: OnceLock<Regex> = OnceLock::new(); static RETAIL: OnceLock<Regex> = OnceLock::new(); static BARE: OnceLock<Regex> = OnceLock::new();
    static POST: OnceLock<Regex> = OnceLock::new(); static ID: OnceLock<Regex> = OnceLock::new();
    let month = MONTH.get_or_init(|| Regex::new(r"(?i)\s*-\s*(?:jan|feb|mar|apr|may|jun|jul|aug|sep|oct|nov|dec)[a-z]* \d{4}$").unwrap());
    let group = GROUP.get_or_init(|| Regex::new(r"\s*[\(\[]([^()\[\]]*)[\)\]]\s*$").unwrap());
    let retail = RETAIL.get_or_init(|| Regex::new(r"(?i)\s*[-|]?\s*\b(retail|digital|pc digital|steam key|key only|key-only)\b\s*-?\s*$").unwrap());
    let bare = BARE.get_or_init(|| Regex::new(r"\s+(ROW|RoW|WW|NA|EU|LATAM|CIS)$").unwrap());
    let post = POST.get_or_init(|| Regex::new(r"(?i)\s*-\s*post[- ]launch$").unwrap());
    let id = ID.get_or_init(|| Regex::new(r"\s*\(\s*\d+\s*\)\s*$").unwrap());
    let mut s = month.replace(raw, "").to_string();
    for _ in 0..4 {
        s = s.trim_matches(|c| c == ' ' || c == '-').to_string();
        if let Some(c) = group.captures(&s) {
            let inner = c[1].trim().to_lowercase();
            if is_region(&inner) || matches!(inner.as_str(), "digital" | "digital retail" | "retail" | "post-launch" | "post launch" | "full release" | "release") { s.truncate(c.get(0).unwrap().start()); continue; }
        }
        if let Some(m) = retail.find(&s) { s.truncate(m.start()); continue; }
        if let Some(m) = bare.find(&s) { s.truncate(m.start()); continue; }
        if let Some(m) = post.find(&s) { s.truncate(m.start()); continue; }
        break;
    }
    id.replace(&s, "").trim_matches(|c| c == ' ' || c == '-').to_string()
}

/// A title as a comparison key: lower case, no trademark marks, `&` as "and", punctuation as spaces, roman numerals as digits. With `editions` the edition words (Standard, Deluxe,
/// Definitive ...) go too, for a second try.
pub fn norm(s: &str, editions: bool) -> String {
    static ED: OnceLock<Regex> = OnceLock::new();
    let mut t: String = s.to_lowercase().replace(['\u{2122}', '\u{ae}'], "").replace('&', " and ");
    t = t.chars().map(|c| if c.is_alphanumeric() { c } else { ' ' }).collect();
    if editions { t = ED.get_or_init(|| Regex::new(r"\b(standard|deluxe|definitive|premium|complete|ultimate|gold|goty|game of the year|digital|limited|special|legendary|launch|default|copy)\b( edition)?").unwrap()).replace_all(&t, " ").to_string(); }
    t.split_whitespace().map(|w| match w { "ii" => "2", "iii" => "3", "iv" => "4", "v" => "5", "vi" => "6", "vii" => "7", "viii" => "8", "ix" => "9", "x" => "10", o => o }).collect::<Vec<_>>().join(" ")
}

/// Demos, playtests, betas, promos and soundtracks hold licenses too; they never match a game, so they are not looked at.
fn noise(item: &str) -> bool {
    static N: OnceLock<Regex> = OnceLock::new();
    N.get_or_init(|| Regex::new(r"(?i)\bdemo\b|playtest|beta testing|limited free promotional|free on-demand|store install|\bsoundtrack\b|\bost\b|closed beta|guest pass|friend.s pass|reviewer|dedicated server|\bsdk\b|advertising").unwrap()).is_match(item)
}

/// The earliest license date for each game: `games` = (appid, name). A title that two library games share is left alone (which one is it?), as is any license that names no game.
/// An exact match on the cleaned title comes first, then the same with edition words dropped.
pub fn match_dates(rows: &[Row], games: &[(i64, String)]) -> HashMap<i64, String> {
    let (mut exact, mut loose): (HashMap<String, Vec<i64>>, HashMap<String, Vec<i64>>) = (HashMap::new(), HashMap::new());
    for (id, name) in games { exact.entry(norm(name, false)).or_default().push(*id); loose.entry(norm(name, true)).or_default().push(*id); }
    let mut best: HashMap<i64, String> = HashMap::new();
    for r in rows.iter().filter(|r| !noise(&r.item)) {
        let t = strip_item(&r.item);
        let hit = exact.get(&norm(&t, false)).or_else(|| loose.get(&norm(&t, true))).filter(|v| v.len() == 1).map(|v| v[0]);
        if let Some(id) = hit { let e = best.entry(id).or_insert_with(|| r.date.clone()); if r.date < *e { *e = r.date.clone(); } }
    }
    best
}

/// Reads pages newest first until every wanted game has a date, the list ends, or `max_pages` are read (a bit over 2 s apart). `wanted` = (appid, name). Err = the first page failed
/// (signed out, the store down), a message for the user; a later failure just ends the read with what was found.
pub fn read_dates(http: &dyn Http, store_cookie: &str, wanted: &[(i64, String)], max_pages: usize, pause: Duration) -> Result<HashMap<i64, String>, String> {
    read_dates_with(http, store_cookie, wanted, max_pages, pause, &|_, _| {})
}
/// `read_dates`, telling `progress(pages read, games matched so far)` after each page.
pub fn read_dates_with(http: &dyn Http, store_cookie: &str, wanted: &[(i64, String)], max_pages: usize, pause: Duration, progress: &dyn Fn(usize, usize)) -> Result<HashMap<i64, String>, String> {
    let header = format!("steamLoginSecure={}", store_cookie.trim());
    let (mut url, mut rows, mut found) = ("https://store.steampowered.com/account/licenses/".to_string(), Vec::<Row>::new(), HashMap::new());
    let want: HashSet<i64> = wanted.iter().map(|(a, _)| *a).collect();
    for n in 0..max_pages {
        let r = match http.get(&url, &[("Cookie", header.as_str())], Duration::from_secs(30)) {
            Ok(r) if r.status < 400 => r,
            Ok(r) => { if n == 0 { return Err(format!("The Steam store answered {}.", r.status)); } break }
            Err(_) => { if n == 0 { return Err("Could not reach the Steam store.".into()); } break }
        };
        let page = parse_page(&r.body);
        if n == 0 && page.rows.is_empty() { return Err("The Steam store gave no licenses list (the sign-in may have expired).".into()); }
        rows.extend(page.rows);
        found = match_dates(&rows, wanted);
        progress(n + 1, found.len());
        if want.iter().all(|a| found.contains_key(a)) { break; }
        let Some(next) = page.next else { break };
        url = format!("https://store.steampowered.com/account/licenses/?{next}");
        std::thread::sleep(pause);
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{NetError, Response};
    fn row(d: &str, i: &str) -> Row { Row { date: d.into(), item: i.into(), how: "Retail".into() } }
    #[test] fn license_titles_are_cleaned_of_retail_and_region_wrappers() {
        for (raw, want) in [("Persona 4 Golden - Retail - [Digital] (RoW)", "Persona 4 Golden"), ("SONIC X SHADOW GENERATIONS - Retail - [Digital] (WW)", "SONIC X SHADOW GENERATIONS"), ("Sonic Frontiers Retail - [Digital] (RoW)", "Sonic Frontiers"),
            ("Assetto Corsa ROW", "Assetto Corsa"), ("Call of Juarez 2: Bound in Blood Retail", "Call of Juarez 2: Bound in Blood"), ("Hi-Fi RUSH PC Digital (WW)", "Hi-Fi RUSH"), ("Aliens: Fireteam Elite (Retail- Global)", "Aliens: Fireteam Elite"),
            ("Some Game (RU/CIS)", "Some Game"), ("Some Game [EU]", "Some Game"), ("Some Game (LATAM)", "Some Game"), ("Some Game - Retail (WW ex CN)", "Some Game"), ("Some Game (PRC-restricted)", "Some Game"), ("Some Game (Not China)", "Some Game"),
            ("Ghostwire: Tokyo (Post-Launch) (WW)", "Ghostwire: Tokyo"), ("There Is No Light - Post Launch", "There Is No Light"), ("Hades - Jul 2025", "Hades"), ("Plain Name", "Plain Name"), ("The Game (Remastered)", "The Game (Remastered)")] {
            assert_eq!(strip_item(raw), want, "{raw}");
        }
    }
    #[test] fn titles_match_exactly_after_cleanup_and_never_fuzzily() {
        let games = vec![(1, "Persona 4 Golden".to_string()), (2, "Quake".to_string()), (3, "EVERSPACE\u{2122}".to_string()), (4, "Yakuza 4 Remastered".to_string()), (5, "Doom 3".to_string()), (6, "Same".to_string()), (7, "Same".to_string())];
        let rows = vec![row("2026-03-17", "Persona 4 Golden - Retail - [Digital] (RoW)"), row("2026-02-01", "Quake 2"), row("2025-05-05", "EVERSPACE"), row("2025-01-01", "Yakuza 4 Remastered Retail - [Digital] (WW)"),
            row("2024-01-01", "Doom III Deluxe Edition"), row("2023-01-01", "Same"), row("2026-03-01", "Persona 4 Golden Demo"), row("2020-01-01", "Persona 4 Golden - Retail - [Digital] (EU)")];
        let m = match_dates(&rows, &games);
        assert_eq!(m.get(&1).map(String::as_str), Some("2020-01-01"), "the earliest license wins; the demo is noise");
        assert_eq!(m.get(&2), None, "Quake 2 is not Quake"); assert_eq!(m.get(&3).map(String::as_str), Some("2025-05-05"), "the trademark mark is ignored");
        assert_eq!(m.get(&4).map(String::as_str), Some("2025-01-01")); assert_eq!(m.get(&5).map(String::as_str), Some("2024-01-01"), "roman numeral and edition word");
        assert!(m.get(&6).is_none() && m.get(&7).is_none(), "two library games with one title: neither");
    }
    const PAGE1: &str = r#"<span>Showing licenses 1-2 of 4925</span><table>
        <tr data-panel="x"><td  class="license_date_col">Nov 18, 2026</td><td>
            STUNTBOOST </td><td class="license_acquisition_col">
            Steam Store </td></tr>
        <tr><td  class="license_date_col">Nov 1, 2026</td><td><div class="free_license_remove_link"><a href="javascript:RemoveFreeLicense( 765319, 'x' );">Remove</a></div> Once Human &amp; Friends </td><td class="license_acquisition_col"> Complimentary </td></tr></table>
        <a class="license_paginator_next" href="?continuationToken=1773766479:73884586629047890&amp;offset=100">Next</a>"#;
    #[test] fn a_licenses_page_is_read_with_its_next_link_and_total() {
        let p = parse_page(PAGE1);
        assert_eq!(p.rows, vec![Row { date: "2026-11-18".into(), item: "STUNTBOOST".into(), how: "Steam Store".into() }, Row { date: "2026-11-01".into(), item: "Once Human & Friends".into(), how: "Complimentary".into() }]);
        assert_eq!(p.next.as_deref(), Some("continuationToken=1773766479:73884586629047890&offset=100")); assert_eq!(p.total, Some(4925));
        assert_eq!(parse_page("<html>signed out</html>"), Page::default());
    }
    struct Site { pages: Vec<&'static str>, seen: std::sync::Mutex<Vec<String>> }
    impl Http for Site { fn get(&self, u: &str, h: &[(&str, &str)], _t: Duration) -> Result<Response, NetError> {
        assert!(h.iter().any(|(k, v)| *k == "Cookie" && v.starts_with("steamLoginSecure=")));
        let mut s = self.seen.lock().unwrap(); s.push(u.to_string()); let n = s.len() - 1;
        Ok(Response { status: 200, retry_after: None, body: self.pages.get(n).copied().unwrap_or("").into() }) } }
    const PAGE2: &str = r#"<table><tr><td class="license_date_col">Mar 1, 2026</td><td>Older Game - Retail (WW)</td><td class="license_acquisition_col">Retail</td></tr></table>"#;
    #[test] fn reading_stops_when_every_wanted_game_has_a_date_and_follows_the_next_link() {
        let site = Site { pages: vec![PAGE1, PAGE2, PAGE2], seen: Default::default() };
        let want = vec![(1, "STUNTBOOST".to_string()), (2, "Older Game".to_string())];
        let got = read_dates(&site, "1%7C%7Cx", &want, 5, Duration::from_millis(1)).unwrap();
        assert_eq!((got.get(&1).map(String::as_str), got.get(&2).map(String::as_str)), (Some("2026-11-18"), Some("2026-03-01")));
        let seen = site.seen.lock().unwrap(); assert_eq!(seen.len(), 2, "stopped after the page that completed the list"); assert!(seen[1].ends_with("?continuationToken=1773766479:73884586629047890&offset=100"));
        drop(seen);
        let one = Site { pages: vec![PAGE1, PAGE2], seen: Default::default() };
        read_dates(&one, "x", &[(1, "STUNTBOOST".into())], 5, Duration::from_millis(1)).unwrap(); assert_eq!(one.seen.lock().unwrap().len(), 1, "found on the first page: no second request");
        assert!(read_dates(&Site { pages: vec!["<html></html>"], seen: Default::default() }, "x", &want, 5, Duration::from_millis(1)).unwrap_err().contains("expired"));
    }
}
