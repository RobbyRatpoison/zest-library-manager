//! Parsers for what Steam's web endpoints and ProtonDB send back (a port of the parsing half of `scrapers.py`). They take the response body as text, so they
//! are tested on recorded real responses with no network. The client that fetches the bodies comes separately; nothing here knows an API key.
use scraper::{Html, Selector};
use serde_json::Value;

fn s<'a>(v: &'a Value, k: &str) -> &'a str { v.get(k).and_then(|x| x.as_str()).unwrap_or("") }
fn round_even(v: f64) -> f64 { let f = v.floor(); let d = v - f; if (d - 0.5).abs() < 1e-9 { if f % 2.0 == 0.0 { f } else { f + 1.0 } } else { v.round() } }

// ---------------------------------------------------------------- dates, text

/// `_parse_steam_date`: "Jan 5, 2020", "5 Jan, 2020" or "January 5, 2020" -> Unix time at midnight UTC.
pub fn parse_steam_date(raw: &str) -> Option<i64> {
    const ABBR: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    const FULL: [&str; 12] = ["january", "february", "march", "april", "may", "june", "july", "august", "september", "october", "november", "december"];
    let t: Vec<String> = raw.trim().replace(',', " ,").split_whitespace().map(|x| x.to_lowercase()).collect();
    let month = |w: &str, names: &[&str; 12]| names.iter().position(|n| *n == w).map(|i| i as u32 + 1);
    let num = |w: &str, max: usize| (!w.is_empty() && w.len() <= max && w.chars().all(|c| c.is_ascii_digit())).then(|| w.parse::<i64>().ok()).flatten();
    // forms: [%b %d , %Y] [%d %b , %Y] [%B %d , %Y]
    let (m, d, y) = match t.as_slice() {
        [a, b, c, y] if c == "," => {
            if let (Some(m), Some(d)) = (month(a, &ABBR), num(b, 2)) { (m, d, y) }
            else if let (Some(d), Some(m)) = (num(a, 2), month(b, &ABBR)) { (m, d, y) }
            else if let (Some(m), Some(d)) = (month(a, &FULL), num(b, 2)) { (m, d, y) }
            else { return None }
        }
        _ => return None,
    };
    let y = num(y, 4).filter(|y| y.to_string().len() == 4)?;
    if d < 1 || d > 31 { return None; }
    // days from civil
    let (yy, mm) = if m <= 2 { (y - 1, m as i64 + 9) } else { (y, m as i64 - 3) };
    let era = yy.div_euclid(400); let yoe = yy - era * 400;
    let doy = (153 * mm + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    // reject impossible dates (Feb 30) the way strptime does: round-trip the day of month
    let back = { let z = days + 719468; let era = z.div_euclid(146097); let doe = z - era * 146097; let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); let mp = (5 * doy + 2) / 153; (doy - (153 * mp + 2) / 5 + 1) as i64 };
    if back != d { return None; }
    Some(days * 86400)
}

/// `clean_description`: store blurb to plain text (tags removed, entities decoded, runs of spaces collapsed, paragraph breaks kept to one blank line).
pub fn clean_description(text: &str) -> String {
    use regex::Regex;
    use std::sync::OnceLock;
    static TAG: OnceLock<Regex> = OnceLock::new(); static WS: OnceLock<Regex> = OnceLock::new(); static NL1: OnceLock<Regex> = OnceLock::new(); static NL3: OnceLock<Regex> = OnceLock::new();
    if text.is_empty() { return String::new(); }
    let t = TAG.get_or_init(|| Regex::new(r"<[^>]{1,1000}>").unwrap()).replace_all(text, " ");
    let t = html_escape::decode_html_entities(&t).into_owned();
    let t = WS.get_or_init(|| Regex::new(r"[ \t\r\x0c\x0b]+").unwrap()).replace_all(&t, " ").into_owned();
    let t = NL1.get_or_init(|| Regex::new(r" ?\n ?").unwrap()).replace_all(&t, "\n").into_owned();
    NL3.get_or_init(|| Regex::new(r"\n{3,}").unwrap()).replace_all(&t, "\n\n").trim().to_string()
}

/// `utils.weighted_review_score`: pulls a low-count score toward 50. `half_trust` = the count + 1 at which a score is trusted halfway (default 10).
pub fn weighted_review_score(percent: f64, count: i64, half_trust: f64) -> i64 {
    if count == 0 { return 0; }
    let p = percent / 100.0;
    let ht = half_trust.max(2.0);
    let shrink = 2f64.powf(-((count + 1) as f64).ln() / ht.ln());
    round_even((p - (p - 0.5) * shrink) * 100.0) as i64
}

// ---------------------------------------------------------------- store appdetails

#[derive(Debug, Clone, PartialEq, Default)]
pub struct StoreData {
    pub name: String, pub kind: String, pub developers: String, pub publishers: String, pub genres: String, pub categories: String,
    pub is_free: i64, pub metacritic_score: Option<i64>, pub short_description: Option<String>,
    /// The date string Steam's API gave ("5 Jan, 2020"), for the release date fallback.
    pub api_date: String,
}

/// `appdetails_entry`: the `{success, data}` entry for `appid`; Steam sometimes answers under another key (Cloudbuilt 262390 comes back keyed by its DLC), so
/// fall back to the entry whose own `steam_appid` matches.
pub fn appdetails_entry(json: &Value, appid: i64) -> Option<&Value> {
    let obj = json.as_object()?;
    if let Some(e) = obj.get(&appid.to_string()).filter(|e| !e.is_null() && e.as_object().map_or(true, |o| !o.is_empty())) { return Some(e); }
    obj.values().find(|e| e.get("data").and_then(|d| d.get("steam_appid")).map_or(false, |v| v.to_string().trim_matches('"') == appid.to_string()))
}

/// `fetch_store_data`'s parsing: None when the app is unknown to the store (`success` false). The release date is decided by `release_date`.
pub fn parse_appdetails(body: &str, appid: i64) -> Option<StoreData> {
    let json: Value = serde_json::from_str(body).ok()?;
    let entry = appdetails_entry(&json, appid)?;
    if !entry.get("success").and_then(|v| v.as_bool()).unwrap_or(false) { return None; }
    let d = entry.get("data")?;
    let list = |k: &str| d.get(k).and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(",")).unwrap_or_default();
    let desc = |k: &str| d.get(k).and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.get("description").and_then(|v| v.as_str())).collect::<Vec<_>>().join(",")).unwrap_or_default();
    let blurb = clean_description(s(d, "short_description"));
    Some(StoreData {
        name: s(d, "name").to_string(), kind: s(d, "type").to_string(), developers: list("developers"), publishers: list("publishers"), genres: desc("genres"), categories: desc("categories"),
        is_free: d.get("is_free").map_or(0, |v| if v.as_bool().unwrap_or(false) || v.as_i64().map_or(false, |n| n != 0) { 1 } else { 0 }),
        metacritic_score: d.get("metacritic").and_then(|m| m.get("score")).and_then(|v| v.as_i64()),
        short_description: (!blurb.is_empty()).then_some(blurb),
        api_date: d.get("release_date").and_then(|r| r.get("date")).and_then(|v| v.as_str()).unwrap_or("").to_string(),
    })
}

/// The release date `fetch_store_data` keeps: appinfo's original (else Steam's) date when it has one; otherwise the store page's date, else the API's.
/// `store_page_date` is only called when it is needed (it costs a request).
pub fn release_date(appinfo_original: Option<i64>, appinfo_steam: Option<i64>, api_date: &str, store_page_date: impl FnOnce() -> Option<i64>) -> Option<i64> {
    let vdf = match appinfo_original { Some(o) if o != 0 => Some(o), _ => appinfo_steam.or(appinfo_original.filter(|o| *o == 0).map(|_| 0)) };
    // Python: `original or steam` is None only when both are missing; 0 or 0 stays 0
    if let Some(v) = vdf.or_else(|| if appinfo_original.is_some() || appinfo_steam.is_some() { Some(0) } else { None }) { return Some(v); }
    store_page_date().or_else(|| parse_steam_date(api_date))
}

// ---------------------------------------------------------------- reviews

#[derive(Debug, Clone, PartialEq)]
pub struct Reviews { pub review_score: String, pub review_percentage: i64, pub weighted_percentage: i64, pub total_reviews: i64, pub positive_reviews: i64 }

/// `fetch_review_data`'s parsing, for the answer of `IUserReviewsService/GetAppReviews/v1` (the summary sits under `response`, as the store's retired
/// `appreviews` endpoint had it at the top). An answer without a summary is a failed fetch (None), never "No Reviews".
pub fn parse_reviews(body: &str, half_trust: f64) -> Option<Reviews> {
    let json: Value = serde_json::from_str(body).ok()?;
    let sum = json.get("response").and_then(|r| r.get("query_summary")).filter(|s| s.is_object()).cloned()?;
    let total = sum.get("total_reviews").and_then(|v| v.as_i64()).unwrap_or(0);
    let positive = sum.get("total_positive").and_then(|v| v.as_i64()).unwrap_or(0);
    let score = if total == 0 { "No Reviews".to_string() } else if total < 10 { "Not Enough Reviews".to_string() } else { sum.get("review_score_desc").and_then(|v| v.as_str()).unwrap_or("No Reviews").to_string() };
    let percent = if total > 0 { ((positive as f64 / total as f64) * 100.0).trunc() as i64 } else { 0 };
    Some(Reviews { review_score: score, review_percentage: percent, weighted_percentage: weighted_review_score(percent as f64, total, half_trust), total_reviews: total, positive_reviews: positive })
}

// ---------------------------------------------------------------- store page HTML

/// `fetch_tag_data`'s parsing: every `a.app_tag` on the store page (the hidden overflow ones too), trimmed, without the "+" button. None when there are none.
pub fn parse_tags(html: &str) -> Option<Vec<String>> {
    let doc = Html::parse_document(html);
    let sel = Selector::parse("a.app_tag").ok()?;
    let tags: Vec<String> = doc.select(&sel).map(|a| a.text().collect::<String>().trim().to_string()).filter(|t| t != "+").collect();
    (!tags.is_empty()).then_some(tags)
}

/// `_scrape_store_page_date`'s parsing: the date shown under "Release date" on the store page.
pub fn parse_store_page_date(html: &str) -> Option<i64> {
    let doc = Html::parse_document(html);
    let sel = Selector::parse(".release_date .date").ok()?;
    let el = doc.select(&sel).next()?;
    let text: String = el.text().map(|t| t.trim()).collect::<Vec<_>>().join("");   // get_text(strip=True)
    parse_steam_date(&text)
}

// ---------------------------------------------------------------- ProtonDB

/// `fetch_protondb_data`'s parsing: (tier, confidence), None for a missing page (404, handled by the caller), an empty tier or `pending`.
pub fn parse_protondb(body: &str) -> Option<(String, Option<String>)> {
    let json: Value = serde_json::from_str(body).ok()?;
    let tier = s(&json, "tier");
    if tier.is_empty() || tier == "pending" { return None; }
    Some((tier.to_string(), json.get("confidence").and_then(|v| v.as_str()).map(String::from)))
}

// ---------------------------------------------------------------- the key-needing endpoints

#[derive(Debug, Clone, PartialEq)]
pub struct OwnedGame { pub appid: i64, pub name: String, pub playtime_forever: i64, pub last_played: Option<i64>, pub icon_hash: String }

/// The games of a `GetOwnedGames` answer. Err carries the message Python showed: no games usually means a private profile.
pub fn parse_owned_games(body: &str) -> Result<Vec<OwnedGame>, &'static str> {
    let json: Value = serde_json::from_str(body).map_err(|_| "Steam sent invalid data. Try again in a few minutes.")?;
    let games = json.get("response").and_then(|r| r.get("games")).and_then(|g| g.as_array()).filter(|g| !g.is_empty()).ok_or("No games returned. Is your Steam Profile set to Public?")?;
    Ok(games.iter().filter_map(|g| Some(OwnedGame {
        appid: g.get("appid")?.as_i64()?, name: s(g, "name").to_string(), playtime_forever: g.get("playtime_forever").and_then(|v| v.as_i64()).unwrap_or(0),
        last_played: g.get("rtime_last_played").and_then(|v| v.as_i64()).filter(|t| *t != 0), icon_hash: s(g, "img_icon_url").to_string(),
    })).collect())
}

#[derive(Debug, Clone, PartialEq)]
pub struct Cheevos { pub total: i64, pub unlocked: i64, pub completed: bool }

/// `fetch_cheevo_data`'s parsing by HTTP status: 400 = no achievements; 403 = not owned or a private profile (None, expected); success false = none.
pub fn parse_cheevos(status: u16, body: &str) -> Option<Cheevos> {
    if status == 400 { return Some(Cheevos { total: 0, unlocked: 0, completed: false }); }
    if status == 403 || !(200..300).contains(&status) { return None; }
    let json: Value = serde_json::from_str(body).ok()?;
    let ps = json.get("playerstats").cloned().unwrap_or(Value::Null);
    if !ps.get("success").and_then(|v| v.as_bool()).unwrap_or(false) { return Some(Cheevos { total: 0, unlocked: 0, completed: false }); }
    let list = ps.get("achievements").and_then(|a| a.as_array()).cloned().unwrap_or_default();
    let total = list.len() as i64;
    let unlocked = list.iter().filter(|a| a.get("achieved").and_then(|v| v.as_i64()) == Some(1)).count() as i64;
    Some(Cheevos { total, unlocked, completed: total > 0 && unlocked == total })
}

/// `fetch_achievement_schema_total`'s parsing: how many achievements the game defines.
pub fn parse_schema_total(body: &str) -> Option<i64> {
    let json: Value = serde_json::from_str(body).ok()?;
    let stats = json.get("game").and_then(|g| g.get("availableGameStats")).cloned().unwrap_or(Value::Null);
    Some(stats.get("achievements").and_then(|a| a.as_array()).map_or(0, |a| a.len() as i64))
}

/// The request for a game's review summary. `languages: ["all"]` with `purchase_type: 1` keeps the all-language totals the retired store endpoint gave;
/// the singular `language` or leaving them out silently shrinks the counts by 20 to 60 percent (checked on real games).
pub const REVIEWS_URL: &str = "https://api.steampowered.com/IUserReviewsService/GetAppReviews/v1/";
pub fn reviews_input_json(appid: i64) -> String { serde_json::json!({"appid": appid, "languages": ["all"], "purchase_type": 1, "num_per_page": 1}).to_string() }
