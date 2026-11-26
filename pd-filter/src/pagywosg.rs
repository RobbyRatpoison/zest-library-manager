//! PAGYWOSG ("Play A Game You Won On SteamGifts") rules: the event id formula, the operator table, the category classifier and the
//! condition-to-SQL translation. A port of `pagywosg.py` (`pagywosg_event_id`, `OP_REGISTRY`, `classify_category`, `redundant_where_sql`),
//! checked against golden cases generated from the real Python (`tests/gen_pagywosg_golden.py`) on every category name of every real event.
//!
//! Differences on purpose: digits are ASCII only (Python's `\d` also takes other scripts' digits; no event uses them); a malformed number
//! where Python would raise makes the condition skip instead of crashing.
use regex::Regex;
use std::collections::BTreeMap;
use std::sync::LazyLock;

/// Event 1 is June 2019; one event per month with no skipped months (verified against the live API, events 1-87).
pub fn event_id(year: i32, month: i32) -> i32 { 1 + (year - 2019) * 12 + (month - 6) }

macro_rules! rx {
    ($p:expr) => {{ static R: LazyLock<Regex> = LazyLock::new(|| Regex::new($p).unwrap()); &*R }};
}

// ───────────────────────── operator table ─────────────────────────

/// One operator: neutral name, the token it has in a saved filter tree, and the shape it is compiled/checked in.
pub struct Op { pub name: &'static str, pub tree_op: &'static str, pub kind: &'static str, pub fmt: Option<&'static str> }

pub const OPS: &[Op] = &[
    Op { name: "month_is", tree_op: "STRFTIME_MONTH", kind: "strftime", fmt: Some("%m") },
    Op { name: "day_is", tree_op: "STRFTIME_DAY", kind: "strftime", fmt: Some("%d") },
    Op { name: "year_is", tree_op: "STRFTIME_YEAR", kind: "strftime", fmt: Some("%Y") },
    Op { name: "weekday_is", tree_op: "STRFTIME_WEEKDAY", kind: "strftime_weekday", fmt: None },
    Op { name: "contains", tree_op: "LIKE", kind: "contains", fmt: None },
    Op { name: "not_contains", tree_op: "NOT LIKE", kind: "not_contains", fmt: None },
    Op { name: "equals", tree_op: "=", kind: "equals", fmt: None },
    Op { name: "starts_with", tree_op: "STARTS_WITH", kind: "starts_with", fmt: None },
    Op { name: "starts_with_any", tree_op: "STARTS_WITH_ANY", kind: "starts_with_any", fmt: None },
    Op { name: "ends_with", tree_op: "ENDS_WITH", kind: "ends_with", fmt: None },
    Op { name: "title_word", tree_op: "TITLE_WORD", kind: "title_word", fmt: None },
    Op { name: "gt", tree_op: ">", kind: "numeric_gt", fmt: None },
    Op { name: "lt", tree_op: "<", kind: "numeric_lt", fmt: None },
    Op { name: "gte", tree_op: ">=", kind: "numeric_gte", fmt: None },
    Op { name: "length_eq", tree_op: "LENGTH_EQ", kind: "title_length", fmt: None },
    Op { name: "digit_count_gte", tree_op: "DIGIT_COUNT_GTE", kind: "digit_count", fmt: None },
    Op { name: "consecutive_repeat", tree_op: "CONSECUTIVE_REPEAT", kind: "consecutive_repeat", fmt: None },
    Op { name: "has_special_char", tree_op: "HAS_SPECIAL_CHAR", kind: "has_special_char", fmt: None },
    Op { name: "range_incl", tree_op: "RANGE_INCL", kind: "numeric_range", fmt: None },
    Op { name: "tag_substring", tree_op: "TAG_SUBSTRING", kind: "tag_substring", fmt: None },
    Op { name: "nth_weekday", tree_op: "NTH_WEEKDAY", kind: "nth_weekday", fmt: None },
    Op { name: "all_caps", tree_op: "ALL_CAPS", kind: "all_caps", fmt: None },
    Op { name: "contains_all", tree_op: "CONTAINS_ALL", kind: "contains_all", fmt: None },
    Op { name: "tag_count_eq", tree_op: "TAG_COUNT_EQ", kind: "tag_count", fmt: None },
    Op { name: "single_word", tree_op: "SINGLE_WORD", kind: "single_word", fmt: None },
];

pub fn op(name: &str) -> Option<&'static Op> { OPS.iter().find(|o| o.name == name) }

// ───────────────────────── classifier ─────────────────────────

/// What a classified category asks for.
#[derive(Debug, Clone, PartialEq)]
pub enum Val { One(String), Many(Vec<String>) }

/// Where an appid list came from (the caller supplies the lists; the classifier only says which one applies).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppidSource { IcaioGiveaways, IcaioWishlist, Santa, Verified }

#[derive(Debug, Clone, PartialEq)]
pub enum Class {
    Tag { tag: String, reason: &'static str },
    Cond { col: &'static str, op: &'static str, val: Val, reason: &'static str },
    /// `auto`: not a mod-verified list (so no "mod verified" label); `force_wins`: counts as the wins pool whatever the category's pool is.
    Appids { source: AppidSource, auto: bool, force_wins: bool, reason: &'static str },
    Skip { reason: &'static str },
}

impl Class {
    /// True when a regex recognised the category; false when it fell through to the verified list or was unhandled.
    pub fn is_automated(&self) -> bool {
        !matches!(self, Class::Appids { reason: "verified_fallback", .. } | Class::Skip { reason: "unhandled" })
    }
}

const MONTHS: [(&str, i32); 12] = [("january", 1), ("february", 2), ("march", 3), ("april", 4), ("may", 5), ("june", 6), ("july", 7), ("august", 8), ("september", 9), ("october", 10), ("november", 11), ("december", 12)];
const WEEKDAYS: [(&str, i32); 7] = [("sunday", 0), ("monday", 1), ("tuesday", 2), ("wednesday", 3), ("thursday", 4), ("friday", 5), ("saturday", 6)];
const WORDNUMS: [(&str, i64); 10] = [("one", 1), ("two", 2), ("three", 3), ("four", 4), ("five", 5), ("six", 6), ("seven", 7), ("eight", 8), ("nine", 9), ("ten", 10)];
const ORDINALS: [(&str, i32); 10] = [("1st", 1), ("first", 1), ("2nd", 2), ("second", 2), ("3rd", 3), ("third", 3), ("4th", 4), ("fourth", 4), ("5th", 5), ("fifth", 5)];
/// Steam's own `review_score_desc` strings, stored verbatim in the review_score column.
const REVIEW_TIERS: [&str; 9] = ["Overwhelmingly Positive", "Very Positive", "Mostly Positive", "Positive", "Mixed", "Mostly Negative", "Negative", "Very Negative", "Overwhelmingly Negative"];
/// Named punctuation kept short and unambiguous ("period"/"comma"/"dash" turn up in unrelated category text).
const PUNCT_NAMES: [(&str, &str); 5] = [("question mark", "?"), ("exclamation mark", "!"), ("exclamation point", "!"), ("apostrophe", "'"), ("colon", ":")];

fn lookup<T: Copy>(table: &[(&str, T)], key: &str) -> Option<T> { let k = key.to_lowercase(); table.iter().find(|(n, _)| *n == k).map(|(_, v)| *v) }

/// Seconds since the epoch of January 1st 00:00 UTC of `year`.
pub fn year_start_ts(year: i64) -> i64 {
    // days from civil (Howard Hinnant), month 1 day 1
    let y = year - 1;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (1 + 9) + 2) / 5; // March-based day of year for January: month 1 -> mp = 10
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146097 + doe - 719468) * 86400
}

fn word_count(s: &str) -> Option<i64> { lookup(&WORDNUMS, s).or_else(|| s.parse().ok()) }

fn cond(col: &'static str, op: &'static str, val: impl Into<String>, reason: &'static str) -> Vec<Class> {
    vec![Class::Cond { col, op, val: Val::One(val.into()), reason }]
}

/// Splits "A, B or C" into its words, each becoming a whole-word title condition.
fn title_words(text: &str) -> Vec<Class> {
    rx!(r"(?i),|\bor\b").split(text).map(str::trim).filter(|w| !w.is_empty())
        .map(|w| Class::Cond { col: "name", op: "title_word", val: Val::One(w.to_string()), reason: "title_word" }).collect()
}

/// Classifies one category name (the part before any "(win)"/"(backlog)"), in the same order of rules as the Python.
/// `has_verified`/`has_icaio_ga`/`has_icaio_wl`/`has_santa` say whether the matching appid list is non-empty.
pub fn classify(base: &str, has_verified: bool, has_icaio_ga: bool, has_icaio_wl: bool, has_santa: bool) -> Vec<Class> {
    if let Some(m) = rx!(r"(?i)^\s*tag\s+(.+)$").captures(base) { return vec![Class::Tag { tag: m[1].trim().to_string(), reason: "tag" }]; }
    // early events named tag categories "<TagName> tag"
    if let Some(m) = rx!(r"(?i)^(.+?)\s+tag$").captures(base) { return vec![Class::Tag { tag: m[1].trim().to_string(), reason: "tag" }]; }
    // "Strategy tag (any kind of)": substring of any tag, not an exact tag
    if let Some(m) = rx!(r"(?i)^(.+?)\s+tag\s*\(any(?:\s+kind\s+of)?\)$").captures(base) { return cond("tags", "tag_substring", m[1].trim(), "tag_substring"); }

    static MONTH_IN: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!(r"(?i)released?\s+(?:in|during)\s+({})\b", MONTHS.iter().map(|m| m.0).collect::<Vec<_>>().join("|"))).unwrap());
    if let Some(m) = MONTH_IN.captures(base) { return cond("release_date", "month_is", lookup(&MONTHS, &m[1]).unwrap().to_string(), "date"); }
    if let Some(m) = rx!(r"(?i)released?\s+in\s+([0-9]{4})\b").captures(base) { return cond("release_date", "year_is", &m[1], "date"); }

    // "released on 2nd Thursday": checked before the plain day rule, which would grab the "2"
    static NTH: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!(r"(?i)released?\s+on\s+(?:the\s+)?(1st|2nd|3rd|4th|5th|first|second|third|fourth|fifth)\s+({})\b", WEEKDAYS.iter().map(|m| m.0).collect::<Vec<_>>().join("|"))).unwrap());
    if let Some(m) = NTH.captures(base) {
        return cond("release_date", "nth_weekday", format!("{}:{}", lookup(&ORDINALS, &m[1]).unwrap(), lookup(&WEEKDAYS, &m[2]).unwrap()), "nth_weekday");
    }
    if let Some(m) = rx!(r"(?i)released?\s+on\s+(?:the\s+)?([0-9]+)(?:st|nd|rd|th)?\b(?:\s*day)?").captures(base) { return cond("release_date", "day_is", &m[1], "date"); }
    static ON_WEEKDAY: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!(r"(?i)released?\s+on\s+(?:a\s+)?({})\b", WEEKDAYS.iter().map(|m| m.0).collect::<Vec<_>>().join("|"))).unwrap());
    if let Some(m) = ON_WEEKDAY.captures(base) { return cond("release_date", "weekday_is", lookup(&WEEKDAYS, &m[1]).unwrap().to_string(), "date"); }

    if let Some(m) = rx!(r"(?i)\bstart(?:s|ing)\s+with\s+(?:the\s+letters?\s+)?([A-Za-z](?:\s*[,/]\s*[A-Za-z])*(?:\s*,?\s*or\s+[A-Za-z])?)").captures(base) {
        let letters: Vec<String> = rx!(r"\b[A-Za-z]\b").find_iter(&m[1]).map(|l| l.as_str().to_uppercase()).collect();
        if !letters.is_empty() { return vec![Class::Cond { col: "name", op: "starts_with_any", val: Val::Many(letters), reason: "starts_with" }]; }
    }

    // "two 3s or two 4s in appID": N occurrences of a digit, one clause per digit; before the plain appid-contains rule
    if rx!(r"(?i)\bin\s+(?:the\s+)?(?:app\s*id|(?:their\s+)?steam\s*(?:app\s*)?id)\b").is_match(base) {
        let groups: Vec<_> = rx!(r"(?i)\b(one|two|three|four|five|six|seven|eight|nine|ten|[0-9]+)\s+([0-9])s\b").captures_iter(base).collect();
        if !groups.is_empty() {
            return groups.iter().filter_map(|g| word_count(&g[1]).map(|n| Class::Cond { col: "appid", op: "digit_count_gte", val: Val::One(format!("{}:{}", &g[2], n)), reason: "appid_digit_count" })).collect();
        }
        if let Some(m) = rx!(r"(?i)\bcontains?\s+(?:a\s+)?([0-9])\b").captures(base) { return cond("appid", "contains", &m[1], "appid"); }
        // 'Games with "45" in their Steam AppID'
        if let Some(m) = rx!(r#"(?i)\bwith\s+["“”]?([0-9]+)["“”]?\s+in\b"#).captures(base) { return cond("appid", "contains", &m[1], "appid"); }
    }
    let m = rx!(r"(?i)steam\s*id\s+containing\s+(\w+)").captures(base).or_else(|| rx!(r"(?i)\b(\w+)\s+in\s+(?:their\s+)?steam\s*(?:app\s*)?id").captures(base));
    if let Some(m) = m { return cond("appid", "contains", &m[1], "appid"); }
    if let Some(m) = rx!(r"(?i)\bgame\s+id'?s?\s+(?:that\s+)?(?:contains?|has)\s+(?:the\s+number\s+)?(?:a\s+)?(\w+)\b").captures(base) { return cond("appid", "contains", &m[1], "appid"); }
    if let Some(m) = rx!(r"(?i)\bdon'?t\s+have\s+(?:the\s+number\s+)?(?:a\s+)?(\w+)\s+in\s+(?:their|the)\s+(?:steam\s+)?id\b").captures(base) { return cond("appid", "not_contains", &m[1], "appid"); }
    // "20 or 01 in the ID of game": a list of substrings, any one qualifies
    if let Some(m) = rx!(r"(?i)^(.+?)\s+in\s+(?:the\s+)?id\s+of\s+(?:the\s+)?game\b").captures(base) {
        let tokens: Vec<_> = rx!(r"(?i),|\bor\b").split(&m[1]).map(str::trim).filter(|t| !t.is_empty()).collect();
        if !tokens.is_empty() { return tokens.into_iter().map(|t| Class::Cond { col: "appid", op: "contains", val: Val::One(t.to_string()), reason: "appid" }).collect(); }
    }
    // a run of N equal digits in a row (not just N anywhere)
    if let Some(m) = rx!(r"(?i)\b(one|two|three|four|five|six|seven|eight|nine|ten|[0-9]+)\s+(?:identical\s+consecutive|consecutive\s+identical)\s+digits?\b").captures(base) {
        if let Some(n) = word_count(&m[1]) { if n >= 2 { return cond("appid", "consecutive_repeat", n.to_string(), "appid_consecutive_repeat"); } }
    }
    // the explicit "word" asks for a whole-word match, so it goes before the plain quoted-substring rule
    if let Some(m) = rx!(r#"(?i)\bword\s+["“”]([^"“”]+)["“”]\s+in\s+(?:the|their|its)\s+title"#).captures(base) { return cond("name", "title_word", m[1].trim(), "title_quote_word"); }
    if let Some(m) = rx!(r#"(?i)["“”]([^"“”]+)["“”]\s+(?:anywhere\s+)?in\s+(?:the|their|its)\s+title"#).captures(base) { return cond("name", "contains", m[1].trim(), "title_quote"); }
    if let Some(m) = rx!(r"(?i)\b(?:a|an)\s+([0-9]+)\s+in\s+(?:the|their|its)\s+title\b").captures(base) { return cond("name", "contains", &m[1], "title_char"); }
    if let Some(m) = rx!(r#"(?i)["“”]([^"“”]+)["“”]\s+prefix\b"#).captures(base) { return cond("name", "starts_with", m[1].trim(), "title_prefix_quote"); }
    if let Some(m) = rx!(r"(?i)exactly\s+([0-9]+)\s+characters?\b.*?\btitle\b").captures(base) { return cond("name", "length_eq", &m[1], "title_length"); }
    // the source spells it "special charaters" in places; both spellings mean any punctuation, symbol or non-Latin character
    if rx!(r"(?i)\bspecial\s+charac?ters?\b").is_match(base) { return cond("name", "has_special_char", "1", "special_char"); }
    if rx!(r"(?i)\bonly\s+capital\s+letters?\b").is_match(base) { return cond("name", "all_caps", "1", "all_caps"); }
    static PUNCT: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!(r"(?i)\b({})s?\b", PUNCT_NAMES.iter().map(|p| regex::escape(p.0)).collect::<Vec<_>>().join("|"))).unwrap());
    if let Some(m) = PUNCT.captures(base) { return cond("name", "contains", lookup(&PUNCT_NAMES, &m[1]).unwrap(), "title_char"); }
    if let Some(m) = rx!(r#"(?i)["“”]([^"“”]+)["“”]\s+in\s+(?:the|their|its)\s+(developer|publisher)s?\s+name"#).captures(base) {
        let col = if m[2].eq_ignore_ascii_case("developer") { "developers" } else { "publishers" };
        return cond(col, "contains", m[1].trim(), "dev_pub_quote");
    }
    // "Name/Developer/Publisher including the letters R, U, O and K": every letter must be present (an AND)
    if let Some(m) = rx!(r"(?i)^(name|developer|publisher)\s+including\s+the\s+letters?\s+([A-Za-z](?:\s*,\s*[A-Za-z])*(?:\s*,?\s*and\s+[A-Za-z])?)\b").captures(base) {
        let w = m[1].to_lowercase();
        let col = if w == "name" { "name" } else if w == "developer" { "developers" } else { "publishers" };
        let letters: String = rx!(r"(?i),|\band\b").split(&m[2]).map(str::trim).filter(|p| p.chars().count() == 1 && p.chars().all(|c| c.is_alphabetic())).map(|p| p.to_uppercase()).collect();
        return cond(col, "contains_all", letters, "letters_required");
    }
    if let Some(m) = rx!(r"(?i)^game\s+title\s+with\s+(.+)$").captures(base) { return title_words(&m[1]); }
    if let Some(m) = rx!(r"(?i)\bwith\s+(.+?)\s+in\s+(?:the|its|their)\s+(?:name|title)\b").captures(base) { return title_words(&m[1]); }

    if let Some(m) = rx!(r"(?i)(?:(?:under|less\s+than|at\s+most|shorter\s+than|below)\s+(?P<h_lt>[0-9]+)\s*h\b|(?:over|more\s+than|at\s+least|longer\s+than|above)\s+(?P<h_gt>[0-9]+)\s*h\b|(?P<h_plus>[0-9]+)\s*h\+).*?hltb\s*(?P<field>completionist\b|(?:main\s*\+\s*)?extras?\b|main\b)?").captures(base) {
        let hours: i128 = ["h_lt", "h_gt", "h_plus"].iter().find_map(|g| m.name(g)).and_then(|x| x.as_str().parse().ok()).unwrap_or(0);
        let f = m.name("field").map(|x| x.as_str().to_lowercase()).unwrap_or_default();
        let op = if m.name("h_lt").is_some() { "lt" } else { "gte" };
        let val = (hours * 60).to_string();
        if f.contains("completionist") { return cond("hltb_completionist", op, val, "hltb"); }
        if f.contains("extra") { return cond("hltb_extras", op, val, "hltb"); }
        if f.contains("main") { return cond("hltb_main", op, val, "hltb"); }
        // no field named ("any HLTB time"): any of the three
        return ["hltb_main", "hltb_extras", "hltb_completionist"].iter().map(|c| Class::Cond { col: c, op, val: Val::One(val.clone()), reason: "hltb" }).collect();
    }

    // exactly zero (the less-than SQL treats 0 as "not fetched yet" and excludes it)
    if rx!(r"(?i)\b(?:without|with\s+no)\s+achievements?\b").is_match(base) { return cond("total_achievements", "equals", "0", "achievements"); }
    // a range needs both bounds in one condition (several conditions from one category are OR'd)
    if let Some(m) = rx!(r"(?i)\b([0-9]+)\s*(?:to|-)\s*([0-9]+)\s*achievements?\b").captures(base) {
        if let (Ok(a), Ok(b)) = (m[1].parse::<i128>(), m[2].parse::<i128>()) { let (lo, hi) = if a > b { (b, a) } else { (a, b) }; return cond("total_achievements", "range_incl", format!("{lo}:{hi}"), "achievements_range"); }
    }
    if let Some(m) = rx!(r"(?i)(?:(?:under|less\s+than|at\s+most|fewer\s+than|below)\s+(?P<a_lt>[0-9]+)\s*achievements?\b|(?:over|more\s+than|at\s+least|above)\s+(?P<a_gt>[0-9]+)\s*achievements?\b|(?P<a_plus>[0-9]+)\s*\+\s*achievements?\b)").captures(base) {
        let count = ["a_lt", "a_gt", "a_plus"].iter().find_map(|g| m.name(g)).map(|x| x.as_str().to_string()).unwrap_or_default();
        return cond("total_achievements", if m.name("a_lt").is_some() { "lt" } else { "gte" }, count.parse::<i128>().map(|n| n.to_string()).unwrap_or(count), "achievements");
    }
    if let Some(m) = rx!(r"(?i)^([0-9]+)\s*achievements?$").captures(base) { return cond("total_achievements", "equals", &m[1], "achievements"); }
    if let Some(m) = rx!(r"(?i)^([0-9]+)\s*tags?$").captures(base) { return cond("tags", "tag_count_eq", &m[1], "tag_count"); }
    if rx!(r"(?i)\btitle\s+with\s+only\s+one\s+word\b").is_match(base) { return cond("name", "single_word", "1", "single_word"); }

    static TIER: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!(r"(?i)\b({})\b", REVIEW_TIERS.iter().map(|t| regex::escape(t)).collect::<Vec<_>>().join("|"))).unwrap());
    if let Some(m) = TIER.captures(base) { let tier = REVIEW_TIERS.iter().find(|t| t.eq_ignore_ascii_case(&m[1])).unwrap(); return cond("review_score", "equals", *tier, "review_tier"); }

    if let Some(m) = rx!(r"(?i)released?\s+before\s+([0-9]{4})\b").captures(base) { return cond("release_date", "lt", year_start_ts(m[1].parse().unwrap()).to_string(), "date"); }
    if let Some(m) = rx!(r"(?i)released?\s+after\s+([0-9]{4})\b").captures(base) { return cond("release_date", "gte", year_start_ts(m[1].parse::<i64>().unwrap() + 1).to_string(), "date"); }

    if rx!(r"(?i)gifter").is_match(base) { return vec![Class::Skip { reason: "gifter" }]; }
    if base.contains("icaio has made a GA for") {
        return vec![if has_icaio_ga { Class::Appids { source: AppidSource::IcaioGiveaways, auto: true, force_wins: false, reason: "icaio_ga" } } else { Class::Skip { reason: "icaio_ga" } }];
    }
    if base.contains("icaio") && base.to_lowercase().contains("wishlist") {
        return vec![if has_icaio_wl { Class::Appids { source: AppidSource::IcaioWishlist, auto: true, force_wins: false, reason: "icaio_wishlist" } } else { Class::Skip { reason: "icaio_wishlist" } }];
    }
    // Santa gifts count as wins whatever the category's pool says
    if rx!(r"(?i)snowball|secret.santa").is_match(base) {
        return vec![if has_santa { Class::Appids { source: AppidSource::Santa, auto: true, force_wins: true, reason: "santa" } } else { Class::Skip { reason: "santa" } }];
    }
    if has_verified { return vec![Class::Appids { source: AppidSource::Verified, auto: false, force_wins: false, reason: "verified_fallback" }]; }
    vec![Class::Skip { reason: "unhandled" }]
}

// ───────────────────────── conditions to SQL ─────────────────────────

/// A bound SQL parameter.
#[derive(Debug, Clone, PartialEq)]
pub enum Param { Text(String), Int(i64) }

/// Columns a condition may name (they are put into SQL text, so anything else is refused).
pub const COLUMNS: [&str; 19] = ["name", "appid", "tags", "genres", "categories", "groups", "developers", "publishers", "release_date", "date_added", "last_played", "total_achievements",
    "hltb_main", "hltb_extras", "hltb_completionist", "review_score", "review_percentage", "playtime_forever", "completion_status"];

const COMMA_SEP_COLS: [&str; 6] = ["tags", "groups", "genres", "categories", "developers", "publishers"];

const TITLE_NORM_SQL: &str = "REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(LOWER(name), ':', ' '), '!', ' '), '?', ' '), ',', ' '), '.', ' '), '''', ' '), '(', ' '), ')', ' '), '[', ' '), ']', ' '), '{', ' '), '}', ' '), ';', ' '), '\"', ' ')";

/// Nested replace() chain that strips a-z, 0-9 and space from the lowercased expression: whatever is left had a "special" character.
fn strip_alnum_sql(expr: &str) -> String {
    let mut sql = format!("lower({expr})");
    for c in "abcdefghijklmnopqrstuvwxyz0123456789 ".chars() { sql = format!("replace({sql}, '{c}', '')"); }
    sql
}

fn int(v: &str) -> Option<i64> { v.trim().parse().ok() }

/// One condition as a WHERE fragment with its parameters, or `None` when the operator has no form here (or the value is malformed).
/// This is `redundant_where_sql`'s per-condition body: the check "does the library game already match without the appid list".
pub fn cond_sql(col: &str, op_name: &str, val: &str) -> Option<(String, Vec<Param>)> {
    if !COLUMNS.contains(&col) { return None; }
    let kind = op(op_name)?.kind;
    let t = |s: String| Param::Text(s);
    Some(match kind {
        "contains" if COMMA_SEP_COLS.contains(&col) => (format!("(',' || {col} || ',') LIKE ?"), vec![t(format!("%,{val},%"))]),
        "contains" if col == "name" => ("name LIKE ?".into(), vec![t(format!("%{val}%"))]),
        // Python left every other column (appid) without a form here, so such games never counted as "already matched"
        "contains" if COLUMNS.contains(&col) => (format!("{col} LIKE ?"), vec![t(format!("%{val}%"))]),
        "strftime" => {
            let fmt = op(op_name)?.fmt?;
            // Python's version lacked 'unixepoch' (a Julian-day reading: never matched) and the empty-date guard; the filter compiler's STRFTIME_* has both
            (format!("({col} IS NOT NULL AND {col} != 0 AND strftime('{fmt}', {col}, 'unixepoch') = ?)"), vec![t(if fmt == "%Y" { val.to_string() } else { format!("{:02}", int(val)?) })])
        }
        "strftime_weekday" => (format!("({col} IS NOT NULL AND {col} != 0 AND strftime('%w', datetime({col}, 'unixepoch')) = ?)"), vec![t(val.to_string())]),
        "starts_with" => (format!("{col} LIKE ?"), vec![t(format!("{val}%"))]),
        "ends_with" => (format!("{col} LIKE ?"), vec![t(format!("%{val}"))]),
        // SQLite LOWER() is ASCII-only; Python lowercased the word as Unicode, so a word like "Ünï" could never match
        "title_word" => (format!("(' ' || {TITLE_NORM_SQL} || ' ') LIKE ?"), vec![t(format!("% {} %", val.to_ascii_lowercase()))]),
        "numeric_gte" => (format!("{col} >= ?"), vec![Param::Int(int(val)?)]),
        "numeric_gt" => (format!("{col} > ?"), vec![Param::Int(int(val)?)]),
        "numeric_lt" => (format!("({col} IS NOT NULL AND {col} > 0 AND {col} < ?)"), vec![Param::Int(int(val)?)]),
        "title_length" => (format!("length({col}) = ?"), vec![Param::Int(int(val)?)]),
        "digit_count" => {
            let (d, c) = val.split_once(':')?;
            (format!("(length({col}) - length(replace({col}, ?, ''))) >= ?"), vec![t(d.to_string()), Param::Int(int(c)?)])
        }
        "consecutive_repeat" => {
            let n = int(val)?;
            if n < 0 { return None; }
            let parts: Vec<String> = (0..10).map(|_| format!("replace({col}, ?, '') != {col}")).collect();
            (format!("({})", parts.join(" OR ")), "0123456789".chars().map(|d| t(d.to_string().repeat(n as usize))).collect())
        }
        "has_special_char" => (format!("length({}) > 0", strip_alnum_sql(col)), vec![]),
        "numeric_range" => {
            let (lo, hi) = val.split_once(':')?;
            (format!("({col} >= ? AND {col} <= ?)"), vec![Param::Int(int(lo)?), Param::Int(int(hi)?)])
        }
        "equals" => (format!("{col} = ?"), vec![t(val.to_string())]),
        "tag_substring" => (format!("{col} LIKE ?"), vec![t(format!("%{val}%"))]),
        "nth_weekday" => {
            let (n, w) = val.split_once(':')?;
            let n = int(n)?;
            let (lo, hi) = ((n - 1) * 7 + 1, n * 7);
            (format!("({col} IS NOT NULL AND {col} != 0 AND strftime('%w', datetime({col}, 'unixepoch')) = ? AND strftime('%d', {col}, 'unixepoch') BETWEEN ? AND ?)"),
             vec![t(w.to_string()), t(format!("{lo:02}")), t(format!("{hi:02}"))])
        }
        "all_caps" => (format!("upper({col}) = {col}"), vec![]),
        "contains_all" => (format!("({})", val.chars().map(|_| format!("{col} LIKE ?")).collect::<Vec<_>>().join(" AND ")), val.chars().map(|c| t(format!("%{c}%"))).collect()),
        "not_contains" if COMMA_SEP_COLS.contains(&col) => (format!("(',' || {col} || ',') NOT LIKE ?"), vec![t(format!("%,{val},%"))]),
        "not_contains" => (format!("{col} NOT LIKE ?"), vec![t(format!("%{val}%"))]),
        "tag_count" => (format!("(CASE WHEN {col} = '' OR {col} IS NULL THEN 0 ELSE length({col}) - length(replace({col}, ',', '')) + 1 END) = ?"), vec![Param::Int(int(val)?)]),
        "single_word" => (format!("replace(trim({col}), ' ', '') = trim({col})"), vec![]),
        _ => return None,
    })
}

/// The WHERE fragment (OR of every tag and condition) that finds library games the pool's criteria already match, with its parameters.
/// `None` when there is nothing to query. `conds` are `(col, op, val)` with neutral operator names.
pub fn redundant_where(conds: &[(String, String, String)], tags: &[String]) -> Option<(String, Vec<Param>)> {
    let (mut parts, mut params) = (Vec::<String>::new(), Vec::<Param>::new());
    for tag in tags { parts.push("(',' || tags || ',') LIKE ?".into()); params.push(Param::Text(format!("%,{tag},%"))); }
    for (col, o, v) in conds { if let Some((p, ps)) = cond_sql(col, o, v) { parts.push(p); params.extend(ps); } }
    if parts.is_empty() { None } else { Some((parts.join(" OR "), params)) }
}

/// Convenience for callers holding appid maps: `{appid: name}` from sorted pairs.
pub type AppidNames = BTreeMap<i64, String>;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn event_ids() { assert_eq!(event_id(2019, 6), 1); assert_eq!(event_id(2026, 8), 87); assert_eq!(event_id(2026, 11), 90); }
    #[test]
    fn year_start() { assert_eq!(year_start_ts(1970), 0); assert_eq!(year_start_ts(2000), 946684800); assert_eq!(year_start_ts(2019), 1546300800); }
}
