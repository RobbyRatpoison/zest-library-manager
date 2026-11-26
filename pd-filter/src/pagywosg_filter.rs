//! A saved PAGYWOSG filter and what it says about one game.
//!
//! The filter is stored as a [`Spec`]: the builder's inputs (event, SteamGifts group, tags, conditions, appid lists). Appids are stored once, grouped by who
//! the entry was verified for. The SQL tree is generated from the spec when the filter runs ([`expand_tree`]), and the per-game answer ("which categories
//! does this game qualify for, and why") is computed from the same spec ([`verdict`]), so there is one set of rules for both. PlayDate (Python) kept the
//! rules three times (SQL, JS SQL, JS per-game check) and rebuilt the conditions by parsing its own generated SQL.
//!
//! Differences from PlayDate, on purpose: every condition kind is checked (the Python panel could only recover two kinds from a saved filter); a review
//! tier and "released before/after" categories are checked; a game never fetched (NULL) does not match "0 achievements", like the SQL; Santa gifts keep their
//! year line in the panel as well as the tooltip.
use crate::pagywosg::{cond_sql, op, Param};
use crate::pagywosg_event::{Auto, Cond};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Pool { Wins, All }
impl Pool { fn name(self) -> &'static str { if self == Pool::Wins { "wins" } else { "all" } } }

/// The Santa/Snowball list's label (the builder's checkbox adds it; entries carry the year the gift was given).
pub const SANTA_LABEL: &str = "Secret Santa / Snowballs";

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PoolSpec { pub tags: Vec<String>, pub conds: Vec<Cond> }

/// Appids that share who they were verified for (and the Santa year): stored once instead of per game.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Group { pub verifiers: Vec<String>, pub year: Option<i64>, pub ids: Vec<i64> }

/// The games one category contributes by name (the mod-verified entries, icaio's lists, Santa gifts).
#[derive(Debug, Clone, PartialEq)]
pub struct List { pub label: String, pub pool: Pool, pub auto: bool, pub santa: bool, pub groups: Vec<Group> }
impl List {
    fn contains(&self, appid: i64) -> Option<&Group> { self.groups.iter().find(|g| g.ids.binary_search(&appid).is_ok()) }
    pub fn all_ids(&self) -> Vec<i64> { let mut v: Vec<i64> = self.groups.iter().flat_map(|g| g.ids.iter().copied()).collect(); v.sort(); v.dedup(); v }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Spec {
    pub event_id: Option<i64>, pub event_name: String,
    /// The group that holds the user's SteamGifts wins; `None` = no SG wins (only Santa gifts can count as wins).
    pub sg_group: Option<String>,
    /// Completion statuses to keep; empty or all five = no restriction.
    pub statuses: Vec<String>,
    /// Categories restricted to the user's own verified entries (already applied to `lists`; kept to show and to refresh).
    pub personal: Vec<String>,
    pub wins: PoolSpec, pub all: PoolSpec,
    pub lists: Vec<List>,
}

fn strs(v: &Value) -> Vec<String> { v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default() }
fn conds_from(v: &Value) -> Vec<Cond> {
    v.as_array().map(|a| a.iter().filter_map(|c| { let c = c.as_array()?; Some((c.first()?.as_str()?.to_string(), c.get(1)?.as_str()?.to_string(), c.get(2)?.as_str()?.to_string())) }).collect()).unwrap_or_default()
}
fn pool_from(v: &Value) -> PoolSpec { PoolSpec { tags: strs(&v["tags"]), conds: conds_from(&v["conds"]) } }
fn pool_json(p: &PoolSpec) -> Value { json!({"tags": p.tags, "conds": p.conds.iter().map(|(c, o, v)| json!([c, o, v])).collect::<Vec<_>>()}) }

impl Spec {
    pub fn from_json(v: &Value) -> Spec {
        Spec {
            event_id: v["event"]["id"].as_i64(), event_name: v["event"]["name"].as_str().unwrap_or("").into(),
            sg_group: v["sg_group"].as_str().map(String::from), statuses: strs(&v["statuses"]), personal: strs(&v["personal"]),
            wins: pool_from(&v["wins"]), all: pool_from(&v["all"]),
            lists: v["lists"].as_array().map(|a| a.iter().map(|l| List {
                label: l["label"].as_str().unwrap_or("").into(), pool: if l["pool"].as_str() == Some("wins") { Pool::Wins } else { Pool::All },
                auto: l["auto"].as_bool().unwrap_or(false), santa: l["santa"].as_bool().unwrap_or(false),
                groups: l["groups"].as_array().map(|g| g.iter().map(|g| {
                    let mut ids: Vec<i64> = g["ids"].as_array().map(|a| a.iter().filter_map(|x| x.as_i64()).collect()).unwrap_or_default();
                    ids.sort(); ids.dedup();
                    Group { verifiers: strs(&g["verifiers"]), year: g["year"].as_i64(), ids }
                }).collect()).unwrap_or_default(),
            }).collect()).unwrap_or_default(),
        }
    }
    pub fn to_json(&self) -> Value {
        json!({"v": 1, "event": {"id": self.event_id, "name": self.event_name}, "sg_group": self.sg_group, "statuses": self.statuses, "personal": self.personal,
               "wins": pool_json(&self.wins), "all": pool_json(&self.all),
               "lists": self.lists.iter().map(|l| {
                   let mut o = json!({"label": l.label, "pool": l.pool.name(), "groups": l.groups.iter().map(|g| {
                       let mut o = json!({"ids": g.ids});
                       if !g.verifiers.is_empty() { o["verifiers"] = json!(g.verifiers); }
                       if let Some(y) = g.year { o["year"] = json!(y); }
                       o
                   }).collect::<Vec<_>>()});
                   if l.auto { o["auto"] = json!(true); }
                   if l.santa { o["santa"] = json!(true); }
                   o
               }).collect::<Vec<_>>()})
    }
    /// The saved-filter tree: a root marked `pagywosg` holding one `pag_filter` node (the compiler and the UI both know it).
    pub fn to_tree(&self) -> Value { json!({"type": "group", "logic": "AND", "pagywosg": true, "items": [{"type": "pag_filter", "spec": self.to_json()}]}) }
    /// The spec inside a saved tree, if it is one of these filters.
    pub fn from_tree(tree: &Value) -> Option<Spec> {
        fn find(n: &Value) -> Option<&Value> {
            if n["type"] == "pag_filter" { return Some(&n["spec"]); }
            n["items"].as_array()?.iter().find_map(find)
        }
        find(tree).map(Spec::from_json)
    }
    fn is_win(&self, groups_csv: &str) -> bool { self.sg_group.as_deref().map_or(false, |g| csv_has(groups_csv, g)) }
}

// ───────────────────────── tree ─────────────────────────

fn tag_items(tags: &[String]) -> Vec<Value> {
    // several tags collapse into one list-valued condition (it compiles to a parenthesised OR) instead of dozens of near-identical nodes
    match tags.len() {
        0 => vec![],
        1 => vec![json!({"type": "condition", "column": "tags", "operator": "LIKE", "value": tags[0]})],
        _ => vec![json!({"type": "condition", "column": "tags", "operator": "LIKE", "value": tags})],
    }
}
fn or_of(mut items: Vec<Value>) -> Value { if items.len() == 1 { items.remove(0) } else { json!({"type": "group", "logic": "OR", "items": items}) } }
fn list_node(l: &List) -> Option<Value> {
    let ids = l.all_ids();
    if ids.is_empty() { None } else { Some(json!({"type": "appid_list", "appids": ids, "label": l.label, "auto": l.auto})) }
}

/// The ordinary filter tree for a spec: Steam games that either qualify as a SteamGifts win (or Santa gift) AND match a wins criterion, or match an
/// all-games criterion; limited to the chosen completion statuses.
pub fn expand_tree(s: &Spec) -> Value {
    let pool_items = |p: &PoolSpec, pool: Pool| -> Vec<Value> {
        let mut v = tag_items(&p.tags);
        v.extend(p.conds.iter().map(|(c, o, x)| json!({"type": "pag_cond", "column": c, "operator": o, "value": x})));
        v.extend(s.lists.iter().filter(|l| l.pool == pool && !l.santa).filter_map(list_node));
        v
    };
    let santa_node = s.lists.iter().find(|l| l.santa).and_then(list_node);
    let (wins, all) = (pool_items(&s.wins, Pool::Wins), pool_items(&s.all, Pool::All));
    let mut main = Vec::new();
    if !wins.is_empty() && (s.sg_group.is_some() || santa_node.is_some()) {
        let group = s.sg_group.as_ref().map(|g| json!({"type": "condition", "column": "groups", "operator": "LIKE", "value": g}));
        let qualifier = match (group, santa_node) { (Some(g), Some(n)) => json!({"type": "group", "logic": "OR", "items": [g, n]}), (Some(g), None) => g, (None, Some(n)) => n, (None, None) => unreachable!() };
        main.push(json!({"type": "group", "logic": "AND", "items": [qualifier, or_of(wins)]}));
    }
    if !all.is_empty() { main.push(or_of(all)); }
    let mut root = vec![json!({"type": "condition", "column": "platform", "operator": "=", "value": "steam"})];
    match main.len() { 0 => {}, 1 => root.push(main.remove(0)), _ => root.push(json!({"type": "group", "logic": "OR", "items": main})) }
    if !s.statuses.is_empty() && s.statuses.len() < 5 {
        root.push(or_of(s.statuses.iter().map(|v| json!({"type": "condition", "column": "completion_status", "operator": "=", "value": v})).collect()));
    }
    json!({"type": "group", "logic": "AND", "items": root})
}

/// The same filter in the shape PlayDate's builder saves it, so the advanced editor can show it as ordinary nested groups, conditions and appid lists and the user can change any of them:
/// `expand_tree` with each criterion as a plain condition (the operator's tree token), plus the markers (`pagywosg`, the event, the verified map in its compact form) that
/// [`spec_from_python_tree`] reads back, so the edited filter still has qualifications.
pub fn python_tree(s: &Spec) -> Value {
    fn plain(n: &Value) -> Value {
        match n["type"].as_str() {
            Some("pag_cond") => {
                let tok = crate::pagywosg::op(n["operator"].as_str().unwrap_or("")).map(|o| o.tree_op);
                match tok { Some(t) => json!({"type": "condition", "column": n["column"], "operator": t, "value": n["value"]}), None => n.clone() }
            }
            Some("group") => { let mut g = n.clone(); g["items"] = Value::Array(n["items"].as_array().map(|a| a.iter().map(plain).collect()).unwrap_or_default()); g }
            _ => n.clone(),
        }
    }
    let mut t = plain(&expand_tree(s));
    t["pagywosg"] = json!(true);
    t["pagywosg_event"] = json!({"id": s.event_id, "name": s.event_name});
    if !s.personal.is_empty() { t["pagywosg_personal_cats"] = json!(s.personal); }
    let mut compact = Vec::new();
    for l in &s.lists {
        for g in &l.groups {
            let mut e = json!({"cat": l.label, "pool": if l.pool == Pool::Wins { "wins" } else { "all" }, "verifiers": g.verifiers, "auto": l.auto || l.santa});
            if let Some(y) = g.year { e["year"] = json!(y); }
            compact.push(json!({"ids": g.ids, "e": [e]}));
        }
    }
    t["pagywosg_verified"] = json!({"_c": compact});
    t
}

/// A `pag_cond` node as SQL: the same text the "already matched" check uses, with bound values. `1=1` for a condition that has no form.
pub fn cond_node_sql(node: &Value, params: &mut Vec<Value>) -> String {
    let (c, o, v) = (node["column"].as_str().unwrap_or(""), node["operator"].as_str().unwrap_or(""), node["value"].as_str().unwrap_or(""));
    match cond_sql(c, o, v) {
        Some((sql, ps)) => { for p in ps { params.push(match p { Param::Text(s) => Value::String(s), Param::Int(i) => Value::from(i) }); } format!("({sql})") }
        None => "1=1".into(),
    }
}

// ───────────────────────── building a spec from the builder's state ─────────────────────────

/// What the user chose in the builder, on top of the event data.
#[derive(Debug, Clone, Default)]
pub struct Selection {
    pub tags_wins: Vec<String>, pub tags_all: Vec<String>, pub conds_wins: Vec<Cond>, pub conds_all: Vec<Cond>,
    pub personal: BTreeSet<String>, pub sg_group: Option<String>, pub statuses: Vec<String>,
    /// Santa/Snowball gifts to count as wins: (appid, year).
    pub santa: Option<Vec<(i64, Option<i64>)>>,
}

/// The saved spec for an event and a selection: lists come from the event's appid data (the whole list, owned or not), personal categories keep only entries verified
/// for `sg_username`.
pub fn spec_from_auto(auto: &Auto, sel: &Selection, sg_username: &str) -> Spec {
    let sg = sg_username.to_lowercase();
    let order: BTreeMap<&str, usize> = auto.cat_order.iter().enumerate().map(|(i, c)| (c.as_str(), i)).collect();
    let mut lists: BTreeMap<(Pool, usize, String), (bool, BTreeMap<(Vec<String>, Option<i64>), Vec<i64>>)> = BTreeMap::new();
    for (pool, p) in [(Pool::Wins, &auto.wins), (Pool::All, &auto.all)] {
        // every id of the category, not only the games in the library today (PlayDate does the same): a game bought or won later matches without rebuilding the filter
        for g in p.games.iter() {
            for c in &g.categories {
                let verifiers = c.verifiers.clone().unwrap_or_default();
                if sel.personal.contains(&c.cat) && (sg.is_empty() || !verifiers.iter().any(|v| v.to_lowercase() == sg)) { continue; }
                let slot = lists.entry((pool, *order.get(c.cat.as_str()).unwrap_or(&usize::MAX), c.cat.clone())).or_insert((c.auto, BTreeMap::new()));
                slot.1.entry((verifiers, None)).or_default().push(g.appid);
            }
        }
    }
    let mut out_lists: Vec<List> = lists.into_iter().map(|((pool, _, label), (auto, groups))| List {
        label, pool, auto, santa: false,
        groups: groups.into_iter().map(|((verifiers, year), mut ids)| { ids.sort(); ids.dedup(); Group { verifiers, year, ids } }).collect(),
    }).collect();
    if let Some(gifts) = &sel.santa {
        let mut by_year: BTreeMap<Option<i64>, Vec<i64>> = BTreeMap::new();
        for (a, y) in gifts { by_year.entry(*y).or_default().push(*a); }
        if !by_year.is_empty() {
            out_lists.push(List { label: SANTA_LABEL.into(), pool: Pool::Wins, auto: true, santa: true,
                groups: by_year.into_iter().map(|(year, mut ids)| { ids.sort(); ids.dedup(); Group { verifiers: vec![], year, ids } }).collect() });
        }
    }
    Spec { event_id: Some(auto.event_id), event_name: auto.event_name.clone(), sg_group: sel.sg_group.clone(), statuses: sel.statuses.clone(),
           personal: sel.personal.iter().cloned().collect(), wins: PoolSpec { tags: sel.tags_wins.clone(), conds: sel.conds_wins.clone() },
           all: PoolSpec { tags: sel.tags_all.clone(), conds: sel.conds_all.clone() }, lists: out_lists }
}

// ───────────────────────── one game ─────────────────────────

/// The fields of a game the rules look at.
#[derive(Debug, Clone, Default)]
pub struct PagGame {
    pub appid: i64, pub name: String, pub tags: String, pub groups: String, pub developers: String, pub publishers: String, pub genres: String, pub categories: String,
    /// Unix time, UTC.
    pub release_date: Option<i64>, pub total_achievements: Option<i64>,
    pub hltb_main: Option<i64>, pub hltb_extras: Option<i64>, pub hltb_completionist: Option<i64>,
    pub review_score: Option<String>,
    /// The win came from a DLC, not the game itself (PAGYWOSG wants that mentioned).
    pub sg_dlc_win: bool,
}

/// One line of the panel: what matched, which kind of entry it counts as, and who verified it.
#[derive(Debug, Clone, PartialEq)]
pub struct Qual { pub desc: String, pub label: String, pub verified: bool, pub verifiers: Vec<String> }

fn ascii_lower(s: &str) -> String { s.to_ascii_lowercase() }
/// SQLite's `LIKE` ignores case for ASCII only.
fn ci_eq(a: &str, b: &str) -> bool { a.eq_ignore_ascii_case(b) }
fn ci_contains(hay: &str, needle: &str) -> bool { ascii_lower(hay).contains(&ascii_lower(needle)) }
/// `',' || col || ',' LIKE '%,val,%'`: the list holds the value as a whole element.
pub fn csv_has(csv: &str, val: &str) -> bool { csv.split(',').any(|e| ci_eq(e, val)) }

const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const WEEKDAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
fn ord(n: i64) -> String { let v = n % 100; let s = if (11..=13).contains(&v) { "th" } else { match n % 10 { 1 => "st", 2 => "nd", 3 => "rd", _ => "th" } }; format!("{n}{s}") }

/// (year, month, day, weekday 0=Sunday) of a Unix time in UTC.
pub fn civil(ts: i64) -> (i64, i64, i64, i64) {
    let days = ts.div_euclid(86400);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (y, m, d, (days + 4).rem_euclid(7))
}

fn num_col<'a>(g: &'a PagGame, col: &str) -> Option<(Option<i64>, &'static str, bool)> {
    // (value, label, shown in hours)
    Some(match col {
        "hltb_main" => (g.hltb_main, "HLTB main", true), "hltb_extras" => (g.hltb_extras, "HLTB main+extras", true), "hltb_completionist" => (g.hltb_completionist, "HLTB completionist", true),
        "total_achievements" => (g.total_achievements, "Achievements", false), _ => return None,
    })
}
fn fmt_num(v: i64, hours: bool) -> String { if hours { format!("{}h", (v as f64 / 60.0).round() as i64) } else { v.to_string() } }
fn field<'a>(g: &'a PagGame, col: &str) -> Option<(&'a str, &'static str)> {
    Some(match col { "name" => (&g.name, "Title"), "developers" => (&g.developers, "Developer"), "publishers" => (&g.publishers, "Publisher"), "genres" => (&g.genres, "Genre"), "categories" => (&g.categories, "Category"), "tags" => (&g.tags, "Tags"), "groups" => (&g.groups, "Group"), _ => return None })
}
fn appid_text(g: &PagGame) -> String { g.appid.to_string() }

/// Whether one condition holds for a game, and the sentence for it. The answer always agrees with what [`cond_sql`] selects in the library.
pub fn check_cond(col: &str, op_name: &str, val: &str, g: &PagGame) -> Option<String> {
    let kind = op(op_name)?.kind;
    let int = |s: &str| s.trim().parse::<i64>().ok();
    match kind {
        "strftime" | "strftime_weekday" | "nth_weekday" => {
            if col != "release_date" { return None; }
            let ts = g.release_date.filter(|t| *t != 0)?;
            let (y, m, d, w) = civil(ts);
            match op_name {
                "month_is" => (m == int(val)?).then(|| format!("Released in {}", MONTHS.get((m - 1) as usize).copied().unwrap_or(val))),
                "day_is" => (d == int(val)?).then(|| format!("Released on the {}", ord(d))),
                "year_is" => (y.to_string() == val).then(|| format!("Released in {val}")),
                "weekday_is" => (w.to_string() == val).then(|| format!("Released on a {}", WEEKDAYS[w as usize])),
                _ => { // nth_weekday "N:W"
                    let (n, wd) = val.split_once(':')?;
                    let (n, wd) = (int(n)?, int(wd)?);
                    (w == wd && d >= (n - 1) * 7 + 1 && d <= n * 7).then(|| format!("Released on the {} {} of the month", ord(n), WEEKDAYS.get(wd as usize).copied().unwrap_or("?")))
                }
            }
        }
        "numeric_gte" | "numeric_gt" | "numeric_lt" => {
            let want = int(val)?;
            if col == "release_date" {
                let t = g.release_date?;
                let year = civil(want).0;
                return match kind {
                    "numeric_lt" => (t > 0 && t < want).then(|| format!("Released before {year}")),
                    "numeric_gte" => (t >= want).then(|| format!("Released in {year} or later")),
                    _ => (t > want).then(|| format!("Released after {year}")),
                };
            }
            let (v, label, h) = num_col(g, col)?;
            let v = v?;
            match kind {
                "numeric_gte" => (v >= want).then(|| format!("{label} ≥ {}", fmt_num(want, h))),
                "numeric_gt" => (v > want).then(|| format!("{label} > {}", fmt_num(want, h))),
                _ => (v > 0 && v < want).then(|| format!("{label} < {}", fmt_num(want, h))),
            }
        }
        "numeric_range" => {
            let (lo, hi) = val.split_once(':')?;
            let (lo, hi) = (int(lo)?, int(hi)?);
            let (v, label, h) = num_col(g, col)?;
            let v = v?;
            (v >= lo && v <= hi).then(|| format!("{label} between {} and {}", fmt_num(lo, h), fmt_num(hi, h)))
        }
        "equals" => {
            if col == "review_score" { return (g.review_score.as_deref() == Some(val)).then(|| format!("Reviews are {val}")); }
            let (v, label, h) = num_col(g, col)?;
            let want = int(val)?;
            (v? == want).then(|| format!("{label} = {}", fmt_num(want, h)))
        }
        "contains" => {
            if col == "appid" { return appid_text(g).contains(val).then(|| format!("AppID contains {val}")); }
            let (text, label) = field(g, col)?;
            match col {
                "name" => ci_contains(text, val).then(|| format!("Title contains \"{val}\"")),
                _ => csv_has(text, val).then(|| format!("{label} includes {val}")),
            }
        }
        "not_contains" => {
            if col == "appid" { return (!appid_text(g).contains(val)).then(|| format!("AppID does not contain {val}")); }
            let (text, label) = field(g, col)?;
            let has = if col == "name" { ci_contains(text, val) } else { csv_has(text, val) };
            (!has).then(|| format!("{label} does not include {val}"))
        }
        "starts_with" => { let (t, label) = field(g, col)?; ascii_lower(t).starts_with(&ascii_lower(val)).then(|| format!("{label} starts with \"{val}\"")) }
        "ends_with" => {
            let (t, label) = if col == "appid" { (appid_text(g), "AppID") } else { let (t, l) = field(g, col)?; (t.to_string(), l) };
            ascii_lower(&t).ends_with(&ascii_lower(val)).then(|| format!("{label} ends with \"{val}\""))
        }
        "title_word" => {
            if col != "name" { return None; }
            let norm: String = ascii_lower(&g.name).chars().map(|c| if ":!?,.'()[]{};\"".contains(c) { ' ' } else { c }).collect();
            format!(" {norm} ").contains(&format!(" {} ", ascii_lower(val))).then(|| format!("Title word \"{val}\""))
        }
        "title_length" => (col == "name" && g.name.chars().count() as i64 == int(val)?).then(|| format!("Title is {val} characters")),
        "digit_count" => {
            if col != "appid" { return None; }
            let (d, c) = val.split_once(':')?;
            (appid_text(g).matches(d).count() as i64 >= int(c)?).then(|| format!("AppID has {c}+ \"{d}\"s"))
        }
        "consecutive_repeat" => {
            if col != "appid" { return None; }
            let n = int(val)?;
            if n <= 0 { return None; }
            let run = "0123456789".chars().any(|d| appid_text(g).contains(&d.to_string().repeat(n as usize)));
            run.then(|| format!("AppID has {n} identical digits in a row"))
        }
        "has_special_char" => (col == "name" && g.name.chars().any(|c| !(c.is_ascii_alphanumeric() || c == ' '))).then(|| "Title has a special character".to_string()),
        "tag_substring" => (col == "tags" && ci_contains(&g.tags, val)).then(|| format!("Tags include one containing \"{val}\"")),
        "all_caps" => { let (t, label) = field(g, col)?; (t.to_ascii_uppercase() == t).then(|| format!("{label} is all uppercase")) }
        "contains_all" => {
            let (t, label) = field(g, col)?;
            val.chars().all(|c| ci_contains(t, &c.to_string())).then(|| format!("{label} includes letters {}", val.chars().map(String::from).collect::<Vec<_>>().join(", ")))
        }
        "tag_count" => {
            let n = if g.tags.is_empty() { 0 } else { g.tags.matches(',').count() as i64 + 1 };
            (col == "tags" && n == int(val)?).then(|| format!("Has exactly {val} tags"))
        }
        "single_word" => { let t = g.name.trim(); (col == "name" && !t.contains(' ')).then(|| "Title is a single word".to_string()) }
        _ => None,
    }
}

/// True when the condition selects the game (what the filter does), without the sentence.
pub fn cond_matches(col: &str, op_name: &str, val: &str, g: &PagGame) -> bool { check_cond(col, op_name, val, g).is_some() }

/// Everything the panel and the tooltip show for one game: each matching condition and each category list the game is on, in the order of the filter.
pub fn verdict(spec: &Spec, g: &PagGame) -> Vec<Qual> {
    let is_win = spec.is_win(&g.groups);
    let santa_gift = spec.lists.iter().any(|l| l.auto && l.pool == Pool::Wins && l.contains(g.appid).is_some());
    let label = |pool: Pool| -> String {
        let win = if g.sg_dlc_win { "(win)*" } else { "(win)" };
        if pool == Pool::Wins { return win.into(); }
        if spec.sg_group.is_none() { return "(backlog)".into(); }
        if is_win { win.into() } else { "(backlog)".into() }
    };
    let mut out = Vec::new();
    for (pool, p) in [(Pool::Wins, &spec.wins), (Pool::All, &spec.all)] {
        if pool == Pool::Wins && !is_win && !santa_gift { continue; }
        let lab = if pool == Pool::Wins && !is_win && santa_gift { "(santa/snowball)".to_string() } else { label(pool) };
        for t in &p.tags { if csv_has(&g.tags, t) { out.push(Qual { desc: format!("Tags includes {t}"), label: lab.clone(), verified: false, verifiers: vec![] }); } }
        for (c, o, v) in &p.conds { if let Some(d) = check_cond(c, o, v, g) { out.push(Qual { desc: d, label: lab.clone(), verified: false, verifiers: vec![] }); } }
    }
    for l in &spec.lists {
        let Some(grp) = l.contains(g.appid) else { continue };
        // a Santa gift always shows (its year is the evidence); other automatic wins are covered by the conditions above
        if l.auto && l.pool == Pool::Wins && grp.year.is_none() { continue; }
        if l.pool == Pool::Wins && !is_win && grp.year.is_none() { continue; }
        let lab = match grp.year { Some(y) => format!("(santa/snowball, {y})"), None => label(l.pool) };
        out.push(Qual { desc: l.label.clone(), label: lab, verified: !l.auto, verifiers: grp.verifiers.clone() });
    }
    out
}

/// Whether any line carries the "won a DLC, not the game" footnote.
pub fn needs_dlc_note(quals: &[Qual]) -> bool { quals.iter().any(|q| q.label.ends_with('*')) }

/// Appids named by the spec's lists plus every list label (for "which categories exist in this filter").
pub fn list_labels(spec: &Spec) -> Vec<String> { let mut seen = HashSet::new(); spec.lists.iter().filter(|l| seen.insert(l.label.clone())).map(|l| l.label.clone()).collect() }

/// How many games in the library the saved filter lists (before hiding duplicates).
pub fn count_matches(db: &rusqlite::Connection, spec: &Spec) -> Result<i64, String> {
    let mut params = Vec::new();
    let sql = crate::build_tree_sql(&expand_tree(spec), &mut params, &crate::Ctx::default());
    let ps: Vec<rusqlite::types::Value> = params.iter().map(|p| match p {
        Value::String(s) => rusqlite::types::Value::Text(s.clone()), Value::Number(n) => rusqlite::types::Value::Integer(n.as_i64().unwrap_or(0)), o => rusqlite::types::Value::Text(o.to_string()),
    }).collect();
    db.query_row(&format!("SELECT COUNT(*) FROM games WHERE {sql}"), rusqlite::params_from_iter(ps), |r| r.get(0)).map_err(|e| e.to_string())
}

// ───────────────────────── filters saved by PlayDate (Python) ─────────────────────────

/// What came out of reading a PlayDate-built saved filter.
#[derive(Debug, Clone, PartialEq)]
pub struct Converted { pub spec: Spec, /// Conditions whose SQL text was not one the builder writes (left out).
    pub skipped: usize }

fn unquote(s: &str) -> String { s.replace("''", "'") }

/// The builder's condition SQL (`pagCondToTree`'s `custom_expr`) read back into (column, operator, value). `None` for text it does not recognise.
pub fn parse_custom_expr(sql: &str) -> Option<(String, String, String)> {
    use regex::Regex;
    use std::sync::LazyLock;
    macro_rules! rx { ($p:expr) => {{ static R: LazyLock<Regex> = LazyLock::new(|| Regex::new($p).unwrap()); &*R }}; }
    let sql = sql.trim();
    let t = |c: &str, o: &str, v: String| Some((c.to_string(), o.to_string(), v));
    if sql.contains("LOWER(name)") && sql.starts_with("(' ' || REPLACE(") {
        let m = rx!(r"LIKE '% (.*) %'$").captures(sql)?;
        return t("name", "title_word", unquote(&m[1]));
    }
    if sql.starts_with("length(replace(") && sql.ends_with("> 0") {
        let m = rx!(r"lower\((\w+)\)").captures(sql)?;
        return t(&m[1], "has_special_char", "1".into());
    }
    if let Some(m) = rx!(r"^length\((\w+)\) = (\d+)$").captures(sql) { return t(&m[1], "length_eq", m[2].into()); }
    if let Some(m) = rx!(r"^\(length\((\w+)\) - length\(replace\((\w+), '(\d)', ''\)\)\) >= (\d+)$").captures(sql) { if m[1] == m[2] { return t(&m[1], "digit_count_gte", format!("{}:{}", &m[3], &m[4])); } }
    if let Some(m) = rx!(r"^\(replace\((\w+), '(\d+)', ''\) != (\w+) OR ").captures(sql) { if m[1] == m[3] { return t(&m[1], "consecutive_repeat", m[2].len().to_string()); } }
    if let Some(m) = rx!(r"^\((\w+) >= (-?\d+) AND (\w+) <= (-?\d+)\)$").captures(sql) { if m[1] == m[3] { return t(&m[1], "range_incl", format!("{}:{}", &m[2], &m[4])); } }
    if let Some(m) = rx!(r"^\(strftime\('%w', datetime\((\w+), 'unixepoch'\)\) = '(\d)' AND strftime\('%d', (\w+), 'unixepoch'\) BETWEEN '(\d+)' AND '(\d+)'\)$").captures(sql) {
        if m[1] == m[3] { let hi: i64 = m[5].parse().ok()?; return t(&m[1], "nth_weekday", format!("{}:{}", hi / 7, &m[2])); }
    }
    if let Some(m) = rx!(r"^upper\((\w+)\) = (\w+)$").captures(sql) { if m[1] == m[2] { return t(&m[1], "all_caps", "1".into()); } }
    if let Some(m) = rx!(r"^\((\w+) LIKE '%.%'(?: AND \w+ LIKE '%.%')*\)$").captures(sql) {
        let letters: String = rx!(r"LIKE '%(.)%'").captures_iter(sql).map(|c| c[1].to_string()).collect();
        return t(&m[1], "contains_all", letters);
    }
    if let Some(m) = rx!(r"^\(CASE WHEN (\w+) = '' OR \w+ IS NULL THEN 0 ELSE length\(\w+\) - length\(replace\(\w+, ',', ''\)\) \+ 1 END\) = (\d+)$").captures(sql) { return t(&m[1], "tag_count_eq", m[2].into()); }
    if let Some(m) = rx!(r"^replace\(trim\((\w+)\), ' ', ''\) = trim\((\w+)\)$").captures(sql) { if m[1] == m[2] { return t(&m[1], "single_word", "1".into()); } }
    if let Some(m) = rx!(r"^(\w+) LIKE '%(.*)%'$").captures(sql) { return t(&m[1], "tag_substring", unquote(&m[2])); }
    if let Some(m) = rx!(r"^(\w+) LIKE '%(.*)'$").captures(sql) { return t(&m[1], "ends_with", unquote(&m[2])); }
    if let Some(m) = rx!(r"^(\w+) LIKE '(.*)%'$").captures(sql) { return t(&m[1], "starts_with", unquote(&m[2])); }
    None
}

/// A builder `condition` node (tree token such as STRFTIME_MONTH or LIKE) as (column, operator, value), for the columns that are not the structural ones.
fn condition_node(n: &Value) -> Option<(String, String, String)> {
    let col = n["column"].as_str()?;
    let tok = n["operator"].as_str()?;
    let val = match &n["value"] { Value::String(s) => s.clone(), Value::Number(x) => x.to_string(), _ => return None };
    let name = crate::pagywosg::OPS.iter().find(|o| o.tree_op == tok)?.name;
    Some((col.to_string(), name.to_string(), val))
}

/// Reads a saved filter built by PlayDate's PAGYWOSG builder into a [`Spec`] (so it gets qualifications and can be refreshed). `None` when the tree is not one.
/// Lists come from the tree's `pagywosg_verified` map (plain or compacted); the tree's own appid nodes carry the same ids.
pub fn spec_from_python_tree(tree: &Value) -> Option<Converted> {
    if tree.get("pagywosg").and_then(|v| v.as_bool()) != Some(true) { return None; }
    let mut spec = Spec { event_id: tree["pagywosg_event"]["id"].as_i64(), event_name: tree["pagywosg_event"]["name"].as_str().unwrap_or("").into(), personal: strs(&tree["pagywosg_personal_cats"]), ..Default::default() };
    let mut skipped = 0usize;
    let mut statuses: Vec<String> = Vec::new();

    fn is_groups(n: &Value) -> bool { n["type"] == "condition" && n["column"] == "groups" }
    fn walk(n: &Value, pool: Pool, spec: &mut Spec, statuses: &mut Vec<String>, skipped: &mut usize) {
        match n["type"].as_str().unwrap_or("") {
            "condition" => {
                let col = n["column"].as_str().unwrap_or("");
                match col {
                    "platform" | "groups" => {}
                    "completion_status" => { if let Some(v) = n["value"].as_str() { statuses.push(v.to_string()); } }
                    "tags" => {
                        let list: Vec<String> = match &n["value"] { Value::String(s) => vec![s.clone()], v => strs(v) };
                        let p = if pool == Pool::Wins { &mut spec.wins } else { &mut spec.all };
                        for t in list { if !p.tags.contains(&t) { p.tags.push(t); } }
                    }
                    _ => match condition_node(n) {
                        Some(c) => { (if pool == Pool::Wins { &mut spec.wins } else { &mut spec.all }).conds.push(c); }
                        None => *skipped += 1,
                    },
                }
            }
            "custom_expr" => match n["sql"].as_str().and_then(parse_custom_expr) {
                Some(c) => { (if pool == Pool::Wins { &mut spec.wins } else { &mut spec.all }).conds.push(c); }
                None => *skipped += 1,
            },
            "group" => {
                let items = n["items"].as_array().cloned().unwrap_or_default();
                if n["logic"] == "AND" {
                    // the wins branch: AND[ groups = G  or  OR[groups, santa list], criteria ]
                    let direct = items.iter().find(|i| is_groups(i));
                    let wrapped = items.iter().find(|i| i["type"] == "group" && i["logic"] == "OR" && i["items"].as_array().map_or(false, |a| a.iter().any(is_groups)));
                    let g = direct.or_else(|| wrapped.and_then(|w| w["items"].as_array().and_then(|a| a.iter().find(|i| is_groups(i)))));
                    if let Some(g) = g {
                        spec.sg_group = g["value"].as_str().map(String::from);
                        for i in &items { if wrapped.map_or(false, |w| std::ptr::eq(w, i)) || is_groups(i) { continue; } walk(i, Pool::Wins, spec, statuses, skipped); }
                        return;
                    }
                }
                for i in &items { walk(i, pool, spec, statuses, skipped); }
            }
            _ => {}   // appid_list / appid_list_ref: the same ids are in pagywosg_verified
        }
    }
    walk(tree, Pool::All, &mut spec, &mut statuses, &mut skipped);
    spec.statuses = if statuses.len() == 5 { vec![] } else { statuses };

    // lists: one per (pool, category), ids grouped by (verifiers, year); Santa gifts are the list the builder's checkbox adds
    let mut order: Vec<(Pool, String)> = Vec::new();
    let mut lists: BTreeMap<(Pool, String), (bool, bool, BTreeMap<(Vec<String>, Option<i64>), Vec<i64>>)> = BTreeMap::new();
    let mut take = |ids: Vec<i64>, entries: &Value| {
        for e in entries.as_array().into_iter().flatten() {
            let (cat, pool) = (e["cat"].as_str().unwrap_or("").to_string(), if e["pool"] == "wins" { Pool::Wins } else { Pool::All });
            if cat.is_empty() { continue; }
            if !order.contains(&(pool, cat.clone())) { order.push((pool, cat.clone())); }
            let auto = e["auto"].as_bool().unwrap_or(false);
            let slot = lists.entry((pool, cat.clone())).or_insert((auto, cat == SANTA_LABEL && auto && pool == Pool::Wins, BTreeMap::new()));
            slot.2.entry((strs(&e["verifiers"]), e["year"].as_i64())).or_default().extend(ids.iter().copied());
        }
    };
    if let Some(pv) = tree["pagywosg_verified"].as_object() {
        for (k, v) in pv {
            if k == "_c" {
                for g in v.as_array().into_iter().flatten() { take(g["ids"].as_array().map(|a| a.iter().filter_map(|x| x.as_i64()).collect()).unwrap_or_default(), &g["e"]); }
            } else if let Ok(id) = k.parse::<i64>() { take(vec![id], v); }
        }
    }
    order.sort_by_key(|(p, _)| *p);   // stable: wins lists first, otherwise in the order they were met
    for (pool, cat) in order {
        if let Some((auto, santa, groups)) = lists.remove(&(pool, cat.clone())) {
            spec.lists.push(List { label: cat, pool, auto, santa, groups: groups.into_iter().map(|((verifiers, year), mut ids)| { ids.sort(); ids.dedup(); Group { verifiers, year, ids } }).collect() });
        }
    }
    Some(Converted { spec, skipped })
}

/// A condition node of a PlayDate-built filter (`condition` or `custom_expr`) as (column, operator, value).
pub fn read_cond(n: &Value) -> Option<(String, String, String)> {
    match n["type"].as_str()? { "custom_expr" => parse_custom_expr(n["sql"].as_str()?), "condition" => condition_node(n), _ => None }
}

/// The spec of any saved PAGYWOSG filter: one Zest saved, or one PlayDate built (read through [`spec_from_python_tree`]).
pub fn spec_of(tree: &Value) -> Option<Spec> { Spec::from_tree(tree).or_else(|| spec_from_python_tree(tree).map(|c| c.spec)) }

#[cfg(test)]
mod python_tree_tests {
    use super::*;
    #[test]
    fn the_editable_tree_reads_back_to_the_same_filter() {
        let spec = Spec { event_id: Some(88), event_name: "November 2026".into(), sg_group: Some("Won on SteamGifts".into()), statuses: vec!["Never Played".into(), "Unfinished".into()], personal: vec![],
            wins: PoolSpec { tags: vec!["Adventure".into(), "Story Rich".into()], conds: vec![("name".into(), "starts_with".into(), "A".into())] },
            all: PoolSpec { tags: vec!["Psychological".into()], conds: vec![] },
            lists: vec![List { label: "Games that are slow".into(), pool: Pool::Wins, auto: false, santa: false, groups: vec![Group { verifiers: vec!["bob".into()], year: None, ids: vec![10, 20] }] },
                        List { label: "Games in spring".into(), pool: Pool::All, auto: true, santa: false, groups: vec![Group { verifiers: vec![], year: None, ids: vec![5, 6, 7] }] }] };
        let t = python_tree(&spec);
        assert_eq!(t["pagywosg"], true);
        // the criteria are ordinary conditions the advanced builder can edit
        assert!(t.to_string().contains("\"operator\":\"STARTS_WITH\""), "{t}");
        assert!(!t.to_string().contains("pag_cond"));
        let back = spec_from_python_tree(&t).unwrap();
        assert_eq!(back.skipped, 0);
        let b = back.spec;
        assert_eq!((b.event_id, b.event_name.as_str(), b.sg_group.as_deref()), (Some(88), "November 2026", Some("Won on SteamGifts")));
        assert_eq!(b.statuses, spec.statuses);
        assert_eq!(b.wins, spec.wins); assert_eq!(b.all, spec.all);
        assert_eq!(b.lists.iter().map(|l| (l.label.clone(), l.pool, l.all_ids())).collect::<Vec<_>>(), spec.lists.iter().map(|l| (l.label.clone(), l.pool, l.all_ids())).collect::<Vec<_>>());
    }
}

#[cfg(test)]
mod whole_list_tests {
    use super::*;
    use crate::pagywosg_event::{Auto, CatEntry, GameRow, Pool as EvPool};
    #[test]
    fn a_saved_list_holds_every_id_of_the_category_not_only_the_owned_games() {
        let game = |appid, in_library| GameRow { appid, name: format!("G{appid}"), categories: vec![CatEntry { cat: "Any game icaio has made a GA for".into(), auto: true, verifiers: None }], in_library, redundant: false };
        let auto = Auto { event_id: 1, event_name: "E".into(), cat_order: vec!["Any game icaio has made a GA for".into()], all: EvPool { games: vec![game(10, true), game(20, false), game(30, false)], ..Default::default() }, ..Default::default() };
        let spec = spec_from_auto(&auto, &Selection::default(), "");
        assert_eq!(spec.lists.len(), 1);
        assert_eq!(spec.lists[0].all_ids(), vec![10, 20, 30], "games you do not own yet are in the list, so they match once you do");
    }
}
