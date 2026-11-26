//! The filter builder's data: the fields and their operators (the web's `FM_FILTER_CONFIG`), the simple-mode rows, and the conversion between rows and
//! the saved filter tree (`{type: 'group', logic, items: [{type: 'condition', column, operator, value}]}`, the shape `pd_filter::build_tree_sql` compiles).
//! No UI here, so it can be tested.

use serde_json::{json, Value};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind { Select, Multi, Text, Number, Date }

#[derive(Clone, Copy, Debug)]
pub enum Opts { None, Static(&'static [(&'static str, &'static str)]), Unique(&'static str), Platforms, Saved }

pub type Ops = &'static [(&'static str, &'static str)];

#[derive(Clone, Copy, Debug)]
pub struct Field { pub key: &'static str, pub label: &'static str, pub kind: Kind, pub ops: Ops, pub opts: Opts, pub default: &'static str }

pub const NO_VALUE: [&str; 2] = ["IS NULL", "IS NOT NULL"];
pub fn needs_no_value(op: &str) -> bool { NO_VALUE.contains(&op) }

macro_rules! ops { ($($o:expr),* $(,)?) => { &[$($o),*] } }

const OPS_TAG: Ops = ops![("LIKE", "includes"), ("NOT LIKE", "excludes"), ("IS NULL", "not set"), ("IS NOT NULL", "is set")];
const OPS_MULTI: Ops = ops![("LIKE", "is"), ("NOT LIKE", "is not"), ("IS NULL", "not set"), ("IS NOT NULL", "is set")];
const OPS_IS: Ops = ops![("=", "is"), ("!=", "is not"), ("IS NULL", "not set"), ("IS NOT NULL", "is set")];
const OPS_IS_ONLY: Ops = ops![("=", "is"), ("IS NULL", "not set"), ("IS NOT NULL", "is set")];
const OPS_NAME: Ops = ops![("LIKE", "contains"), ("NOT LIKE", "does not contain"), ("=", "exactly matches"), ("STARTS_WITH", "starts with"), ("IS NULL", "not set"), ("IS NOT NULL", "is set")];
const OPS_APPID: Ops = ops![("LIKE", "contains"), ("=", "exactly matches")];
const OPS_TEXT: Ops = ops![("LIKE", "contains"), ("=", "exactly matches"), ("IS NULL", "not set"), ("IS NOT NULL", "is set")];
const OPS_DATE: Ops = ops![("=", "on (exact)"), (">", "after"), ("<", "before"), ("STRFTIME_MONTH", "month is"), ("STRFTIME_DAY", "day is"), ("STRFTIME_YEAR", "year is"),
    ("STRFTIME_WEEKDAY", "weekday is"), ("LIKE", "matches pattern"), ("IS NULL", "not set"), ("IS NOT NULL", "is set")];
const OPS_LAST_PLAYED: Ops = ops![("=", "on (exact)"), (">", "after"), ("<", "before"), ("STRFTIME_MONTH", "month is"), ("STRFTIME_DAY", "day is"), ("STRFTIME_YEAR", "year is"),
    ("STRFTIME_WEEKDAY", "weekday is"), ("LIKE", "matches pattern"), ("IS NULL", "never played"), ("IS NOT NULL", "has been played")];
const OPS_MORE: Ops = ops![(">", "more than"), ("<", "less than"), ("=", "exactly"), ("IS NULL", "not set"), ("IS NOT NULL", "is set")];
const OPS_OVER: Ops = ops![(">", "over"), ("<", "under"), ("=", "is"), ("IS NULL", "not set"), ("IS NOT NULL", "is set")];
const OPS_HLTB: Ops = ops![("=", "="), ("!=", "\u{2260}"), ("<", "<"), ("<=", "\u{2264}"), (">", ">"), (">=", "\u{2265}"), ("IS NULL", "not fetched"), ("IS NOT NULL", "is set")];
const OPS_PROTON: Ops = ops![("=", "is"), ("!=", "is not"), ("IS NULL", "not fetched"), ("IS NOT NULL", "is set")];
const OPS_EQ_NE: Ops = ops![("=", "is"), ("!=", "is not")];
const OPS_SAVED: Ops = ops![("=", "games in"), ("!=", "games not in")];

const YES_NO: Opts = Opts::Static(&[("1", "Yes"), ("0", "No")]);
const STATUS: Opts = Opts::Static(&[("Never Played", "Never Played"), ("Unfinished", "Unfinished"), ("Beaten", "Beaten"), ("Completed", "Completed"), ("Won't Play", "Won't Play")]);
const REVIEW: Opts = Opts::Static(&[("Overwhelmingly Positive", "Overwhelmingly Positive"), ("Very Positive", "Very Positive"), ("Positive", "Positive"), ("Mostly Positive", "Mostly Positive"),
    ("Mixed", "Mixed"), ("Mostly Negative", "Mostly Negative"), ("Negative", "Negative"), ("Very Negative", "Very Negative"), ("Overwhelmingly Negative", "Overwhelmingly Negative"),
    ("No Reviews", "No Reviews"), ("Not Enough Reviews", "Not Enough Reviews")]);
const TIERS: Opts = Opts::Static(&[("platinum", "platinum"), ("gold", "gold"), ("silver", "silver"), ("bronze", "bronze"), ("borked", "borked")]);
const CONF: Opts = Opts::Static(&[("strong", "strong"), ("good", "good"), ("weak", "weak")]);
const VART: Opts = Opts::Static(&[("capsule_2x", "capsule_2x"), ("capsule", "capsule"), ("sgdb_grid", "sgdb_grid"), ("custom", "custom"), ("missing", "missing")]);
const HART: Opts = Opts::Static(&[("header", "header"), ("sgdb_grid_wide", "sgdb_grid_wide"), ("custom", "custom"), ("missing", "missing")]);
const ICONS: Opts = Opts::Static(&[("steam", "steam"), ("sgdb_icon", "sgdb_icon"), ("custom", "custom"), ("missing", "missing")]);
const QUALITY: Opts = Opts::Static(&[("low", "Low resolution"), ("wrong", "Wrong shape for the card"), ("missing", "No cover"), ("ok", "Fits and sharp enough"), ("q95", "Saved at JPEG quality 95")]);
const FIT: Opts = Opts::Static(&[("wrong", "Wrong shape for the card"), ("fits", "Fits the card"), ("none", "No cover")]);

const fn f(key: &'static str, label: &'static str, kind: Kind, ops: Ops, opts: Opts, default: &'static str) -> Field { Field { key, label, kind, ops, opts, default } }

/// The fields in the web's order (`FM_FILTER_CONFIG`). "Saved Filter" is a reference to another saved filter (`saved_filter_ref` in the tree).
pub const FIELDS: &[Field] = &[
    f("tags", "Tag", Kind::Multi, OPS_TAG, Opts::Unique("tags"), ""),
    f("completion_status", "Completion Status", Kind::Select, OPS_IS, STATUS, ""),
    f("installed", "Installed", Kind::Select, OPS_IS_ONLY, YES_NO, "1"),
    f("name", "Game Title", Kind::Text, OPS_NAME, Opts::None, ""),
    f("appid", "AppID", Kind::Text, OPS_APPID, Opts::None, ""),
    f("groups", "Group", Kind::Multi, OPS_MULTI, Opts::Unique("groups"), ""),
    f("release_date", "Release Date", Kind::Date, OPS_DATE, Opts::None, ""),
    f("date_added", "Date Added", Kind::Date, OPS_DATE, Opts::None, ""),
    f("last_played", "Last Played", Kind::Date, OPS_LAST_PLAYED, Opts::None, ""),
    f("playtime_forever", "Playtime (mins)", Kind::Number, OPS_MORE, Opts::None, ""),
    f("review_percentage", "Review %", Kind::Number, OPS_OVER, Opts::None, ""),
    f("weighted_percentage", "Weighted Review %", Kind::Number, OPS_OVER, Opts::None, ""),
    f("metacritic_score", "Metacritic Score", Kind::Number, OPS_OVER, Opts::None, ""),
    f("review_score", "Review Score", Kind::Select, OPS_IS, REVIEW, ""),
    f("developers", "Developer", Kind::Text, OPS_TEXT, Opts::None, ""),
    f("publishers", "Publisher", Kind::Text, OPS_TEXT, Opts::None, ""),
    f("unlocked_achievements", "Unlocked Achievements", Kind::Number, OPS_MORE, Opts::None, ""),
    f("total_achievements", "Total Achievements", Kind::Number, OPS_MORE, Opts::None, ""),
    f("total_reviews", "Total Reviews", Kind::Number, OPS_MORE, Opts::None, ""),
    f("positive_reviews", "Positive Reviews", Kind::Number, OPS_MORE, Opts::None, ""),
    f("genres", "Genre", Kind::Multi, OPS_MULTI, Opts::Unique("genres"), ""),
    f("categories", "Category", Kind::Multi, OPS_MULTI, Opts::Unique("categories"), ""),
    f("is_free", "Free to Play", Kind::Select, OPS_IS_ONLY, YES_NO, "1"),
    f("protondb_tier", "ProtonDB Tier", Kind::Select, OPS_PROTON, TIERS, ""),
    f("protondb_confidence", "ProtonDB Confidence", Kind::Select, OPS_IS, CONF, ""),
    f("hltb_main", "HLTB Main Story (min)", Kind::Number, OPS_HLTB, Opts::None, ""),
    f("hltb_extras", "HLTB Main+Extras (min)", Kind::Number, OPS_HLTB, Opts::None, ""),
    f("hltb_completionist", "HLTB Completionist (min)", Kind::Number, OPS_HLTB, Opts::None, ""),
    f("vertical_art_source", "Vertical Art Source", Kind::Select, OPS_IS, VART, ""),
    f("horizontal_art_source", "Horizontal Art Source", Kind::Select, OPS_IS, HART, ""),
    f("icon_source", "Icon Source", Kind::Select, OPS_IS, ICONS, ""),
    f("cover_fit", "Cover Shape (this view)", Kind::Select, OPS_EQ_NE, FIT, "wrong"),
    f("cover_quality", "Cover Quality (this view)", Kind::Select, OPS_EQ_NE, QUALITY, "low"),
    f("saved_filter", "Saved Filter", Kind::Select, OPS_SAVED, Opts::Saved, ""),
    f("platform", "Library", Kind::Select, OPS_IS, Opts::Platforms, ""),
];

pub fn field(key: &str) -> Option<&'static Field> { FIELDS.iter().find(|f| f.key == key) }

pub fn is_multi_col(key: &str) -> bool { matches!(key, "tags" | "groups" | "genres" | "categories") }

const COMPARE_OPS: [&str; 6] = ["=", "!=", ">", "<", ">=", "<="];
const COMPARE_NUMERIC: [&str; 8] = ["playtime_forever", "review_percentage", "weighted_percentage", "metacritic_score", "unlocked_achievements", "total_achievements", "total_reviews", "positive_reviews"];
const COMPARE_DATE: [&str; 3] = ["release_date", "date_added", "last_played"];

/// `_compatibleCols`: the columns a numeric or date column can be compared with (the other columns of its own kind).
pub fn compare_cols(col: &str) -> Vec<&'static str> {
    if COMPARE_NUMERIC.contains(&col) { COMPARE_NUMERIC.iter().copied().filter(|c| *c != col).collect() }
    else if COMPARE_DATE.contains(&col) { COMPARE_DATE.iter().copied().filter(|c| *c != col).collect() }
    else { Vec::new() }
}
pub fn can_compare(col: &str, op: &str) -> bool { COMPARE_OPS.contains(&op) && !compare_cols(col).is_empty() }

/// One line of the simple builder. `val_col` set = the right-hand side is another column; `name` = the saved filter's name (a `saved_filter` row keeps its id in `text`).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Row { pub col: String, pub op: String, pub text: String, pub vals: Vec<String>, pub pill_in: String, pub val_col: String, pub name: String }

impl Row {
    pub fn new(col: &str) -> Row {
        let fd = field(col).unwrap_or(&FIELDS[0]);
        Row { col: fd.key.into(), op: fd.ops[0].0.into(), text: fd.default.into(), vals: Vec::new(), pill_in: String::new(), val_col: String::new(), name: String::new() }
    }
    /// The row's column changed: first operator and the column's default value.
    pub fn set_col(&mut self, col: &str) { *self = Row::new(col); }
    pub fn uses_pills(&self) -> bool { is_multi_col(&self.col) && (self.op == "LIKE" || self.op == "NOT LIKE") }

    /// `_condIsComplete`: a column and a value, or an operator that needs none.
    pub fn complete(&self) -> bool {
        if self.col.is_empty() { return false; }
        if self.col == "saved_filter" { return !self.text.is_empty(); }
        if needs_no_value(&self.op) { return true; }
        if !self.val_col.is_empty() && can_compare(&self.col, &self.op) { return true; }
        if self.uses_pills() { !self.vals.is_empty() } else { !self.text.trim().is_empty() }
    }

    pub fn to_node(&self) -> Value {
        if self.col == "saved_filter" {
            let mut n = json!({"type": "saved_filter_ref", "id": self.text, "name": self.name});
            if self.op == "!=" { n["negate"] = json!(true); }
            return n;
        }
        if !self.val_col.is_empty() && can_compare(&self.col, &self.op) && !needs_no_value(&self.op) {
            return json!({"type": "condition", "column": self.col, "operator": self.op, "value_col": self.val_col});
        }
        let value = if needs_no_value(&self.op) { json!("") }
            else if self.uses_pills() { if self.vals.len() == 1 { json!(self.vals[0]) } else { json!(self.vals) } }
            else { json!(self.text) };
        json!({"type": "condition", "column": self.col, "operator": self.op, "value": value})
    }
}

pub fn value_text(v: &Value) -> String {
    match v { Value::String(s) => s.clone(), Value::Number(n) => n.to_string(), Value::Bool(b) => (*b as i64).to_string(), _ => String::new() }
}

fn is_search(it: &Value) -> bool {
    it.get("type").and_then(|v| v.as_str()) == Some("condition") && it.get("column").and_then(|v| v.as_str()) == Some("name") && it.get("operator").and_then(|v| v.as_str()) == Some("LIKE")
}

/// `_strip_search_condition`: the quick-search box's `name LIKE` condition is not part of the filter.
pub fn without_search(tree: &Value) -> Value {
    let Some(items) = tree.get("items").and_then(|v| v.as_array()).filter(|a| !a.is_empty()) else { return tree.clone() };
    let kept: Vec<Value> = items.iter().filter(|it| !is_search(it)).cloned().collect();
    if kept.len() == 1 && kept[0].get("type").and_then(|v| v.as_str()) == Some("group") { return kept[0].clone(); }
    let mut out = tree.clone();
    out["items"] = Value::Array(kept);
    out
}

/// `buildSearchTree`: the quick-search words as a top-level `name LIKE` condition ANDed with the tree, which is how the web page shows the search
/// (as a "Game Title contains" row) when the Filters dialog opens. A plain AND list of rows takes the condition as its first row (the same filter,
/// and the builder can still show it); anything else (OR, custom SQL, a game list, a nested group) is nested under it, like the web page does.
pub fn with_search(tree: &Value, q: &str) -> Value {
    let base = without_search(tree);
    let q = q.trim();
    if q.is_empty() { return base; }
    let node = json!({"type": "condition", "column": "name", "operator": "LIKE", "value": q});
    let has_sql = base.get("custom_sql").and_then(|v| v.as_str()).map_or(false, |s| !s.is_empty());
    let items = base.get("items").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    if !has_sql && items.is_empty() { return json!({"type": "group", "logic": "AND", "items": [node]}); }
    let flat_and = !has_sql && base.get("logic").and_then(|v| v.as_str()).map_or(true, |l| l.eq_ignore_ascii_case("AND"))
        && items.iter().all(|it| it.get("type").and_then(|v| v.as_str()) == Some("condition"));
    if flat_and { let mut out = base.clone(); let mut all = vec![node]; all.extend(items); out["items"] = Value::Array(all); return out; }
    json!({"type": "group", "logic": "AND", "items": [node, base]})
}

/// What rows can and cannot say: the builder shows only flat lists of conditions.
#[derive(Debug, PartialEq)]
pub enum FromTree { Rows { or: bool, rows: Vec<Row> }, Unsupported(&'static str) }

/// `_consolidateMultiValueConditions`: an OR of several `LIKE`/`=` conditions on one tag-like column is one condition with several values.
fn consolidate(items: &[Value], or: bool) -> Vec<Value> {
    if !or { return items.to_vec(); }
    let mut buckets: Vec<(String, String, Vec<Value>)> = Vec::new();
    let mut rest: Vec<Value> = Vec::new();
    for it in items {
        let col = it.get("column").and_then(|v| v.as_str()).unwrap_or("");
        let op = it.get("operator").and_then(|v| v.as_str()).unwrap_or("");
        let single = it.get("type").and_then(|v| v.as_str()) == Some("condition") && is_multi_col(col) && (op == "LIKE" || op == "=")
            && it.get("value_col").is_none() && !it.get("value").map_or(false, |v| v.is_array());
        if single {
            match buckets.iter_mut().find(|b| b.0 == col && b.1 == op) { Some(b) => b.2.push(it["value"].clone()), None => buckets.push((col.into(), op.into(), vec![it["value"].clone()])) }
        } else { rest.push(it.clone()); }
    }
    let mut out: Vec<Value> = buckets.into_iter().map(|(c, o, v)| json!({"type": "condition", "column": c, "operator": o, "value": if v.len() == 1 { v[0].clone() } else { Value::Array(v) }})).collect();
    out.extend(rest);
    out
}

pub fn rows_from_tree(tree: &Value) -> FromTree {
    if tree.get("custom_sql").and_then(|v| v.as_str()).map_or(false, |s| !s.trim().is_empty()) { return FromTree::Unsupported("custom SQL"); }
    if tree.get("pagywosg").map_or(false, |v| v.as_bool().unwrap_or(true)) { return FromTree::Unsupported("a PAGYWOSG filter"); }
    let or = tree.get("logic").and_then(|v| v.as_str()) == Some("OR");
    let items = tree.get("items").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let mut rows = Vec::new();
    for it in consolidate(&items, or) {
        match it.get("type").and_then(|v| v.as_str()) {
            Some("condition") => {}
            Some("group") => return FromTree::Unsupported("nested groups"),
            Some("saved_filter_ref") => {
                let id = it.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                rows.push(Row { col: "saved_filter".into(), op: if it.get("negate").and_then(|v| v.as_bool()).unwrap_or(false) { "!=".into() } else { "=".into() }, text: id,
                    name: it.get("name").and_then(|v| v.as_str()).unwrap_or("").into(), ..Default::default() });
                continue;
            }
            Some("appid_list") => return FromTree::Unsupported("a list of games"),
            Some("custom_expr") => return FromTree::Unsupported("an expression"),
            _ => return FromTree::Unsupported("something the simple builder does not know"),
        }
        let col = it.get("column").and_then(|v| v.as_str()).unwrap_or("");
        let Some(fd) = field(col) else { return FromTree::Unsupported("a column the builder does not list") };
        if let Some(vc) = it.get("value_col").and_then(|v| v.as_str()) {
            let op = it.get("operator").and_then(|v| v.as_str()).unwrap_or("=");
            if !can_compare(col, op) || !compare_cols(col).contains(&vc) { return FromTree::Unsupported("a comparison between two columns the builder cannot pair"); }
            rows.push(Row { col: col.into(), op: op.into(), val_col: vc.into(), ..Default::default() });
            continue;
        }
        let op = it.get("operator").and_then(|v| v.as_str()).unwrap_or(fd.ops[0].0).to_string();
        let mut row = Row { col: col.into(), op: op.clone(), text: String::new(), vals: Vec::new(), pill_in: String::new(), ..Default::default() };
        match it.get("value") {
            Some(Value::Array(a)) => row.vals = a.iter().map(value_text).filter(|s| !s.is_empty()).collect(),
            Some(v) if row.uses_pills() => { let s = value_text(v); if !s.is_empty() { row.vals = vec![s]; } }
            Some(v) => row.text = value_text(v),
            None => {}
        }
        rows.push(row);
    }
    FromTree::Rows { or, rows }
}


/// The advanced builder's tree: conditions, nested groups, raw expressions, and anything else (a list of games) kept as it is.
#[derive(Clone, Debug, PartialEq)]
pub enum Node { Cond(Row), Group(Group), Expr(String), Opaque(Value) }
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Group { pub or: bool, pub items: Vec<Node>, pub collapsed: bool }

fn row_from_condition(it: &Value) -> Option<Row> {
    match it.get("type").and_then(|v| v.as_str()) {
        Some("saved_filter_ref") => return Some(Row { col: "saved_filter".into(), op: if it.get("negate").and_then(|v| v.as_bool()).unwrap_or(false) { "!=".into() } else { "=".into() },
            text: it.get("id").and_then(|v| v.as_str()).unwrap_or("").into(), name: it.get("name").and_then(|v| v.as_str()).unwrap_or("").into(), ..Default::default() }),
        Some("condition") | None => {}
        _ => return None,
    }
    let col = it.get("column").and_then(|v| v.as_str())?;
    let fd = field(col)?;
    let op = it.get("operator").and_then(|v| v.as_str()).unwrap_or(fd.ops[0].0).to_string();
    if let Some(vc) = it.get("value_col").and_then(|v| v.as_str()) { return Some(Row { col: col.into(), op, val_col: vc.into(), ..Default::default() }); }
    let mut row = Row { col: col.into(), op, ..Default::default() };
    match it.get("value") {
        Some(Value::Array(a)) => row.vals = a.iter().map(value_text).filter(|s| !s.is_empty()).collect(),
        Some(v) if row.uses_pills() => { let s = value_text(v); if !s.is_empty() { row.vals = vec![s]; } }
        Some(v) => row.text = value_text(v),
        None => {}
    }
    Some(row)
}

impl Group {
    /// Any saved tree as a group; what the builder has no row for (an unknown column, a list of games) stays as it was.
    pub fn from_tree(tree: &Value) -> Group {
        let or = tree.get("logic").and_then(|v| v.as_str()).map_or(false, |l| l.eq_ignore_ascii_case("OR"));
        let items = tree.get("items").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let nodes = consolidate(&items, or).into_iter().map(|it| match it.get("type").and_then(|v| v.as_str()) {
            Some("group") => Node::Group(Group::from_tree(&it)),
            Some("custom_expr") => Node::Expr(it.get("sql").and_then(|v| v.as_str()).unwrap_or("").to_string()),
            _ => match row_from_condition(&it) { Some(r) => Node::Cond(r), None => Node::Opaque(it) },
        }).collect();
        Group { or, items: nodes, collapsed: false }
    }

    /// The tree the builder saves: complete conditions only, empty groups and empty expressions left out.
    pub fn to_value(&self) -> Value {
        let items: Vec<Value> = self.items.iter().filter_map(|n| match n {
            Node::Cond(r) => r.complete().then(|| r.to_node()),
            Node::Group(g) => { let v = g.to_value(); v["items"].as_array().map_or(false, |a| !a.is_empty()).then_some(v) }
            Node::Expr(e) => (!e.trim().is_empty()).then(|| json!({"type": "custom_expr", "sql": e.trim()})),
            Node::Opaque(v) => Some(v.clone()),
        }).collect();
        json!({"type": "group", "logic": if self.or { "OR" } else { "AND" }, "items": items})
    }
}

pub fn tree_from_rows(or: bool, rows: &[Row]) -> Value {
    json!({"type": "group", "logic": if or { "OR" } else { "AND" }, "items": rows.iter().filter(|r| r.complete()).map(|r| r.to_node()).collect::<Vec<_>>()})
}

pub fn is_empty_tree(tree: &Value) -> bool {
    tree.get("items").and_then(|v| v.as_array()).map_or(true, |a| a.is_empty()) && !tree.get("custom_sql").and_then(|v| v.as_str()).map_or(false, |s| !s.trim().is_empty())
}

/// The quick filters of the footer (`BUILTIN_FILTERS`, groups general and status): (label, tree). "All Games" is the empty tree.
pub fn quick_filters() -> (Vec<(&'static str, Value)>, Vec<(&'static str, Value)>) {
    let c = |col: &str, val: &str| json!({"type": "condition", "column": col, "operator": "=", "value": val});
    let g = |logic: &str, items: Vec<Value>| json!({"type": "group", "logic": logic, "items": items});
    let general = vec![
        ("All Games", g("AND", vec![])),
        ("Installed", g("AND", vec![c("installed", "1")])),
        ("Not Installed", g("AND", vec![c("installed", "0")])),
        ("Never Played / Unfinished", g("OR", vec![c("completion_status", "Never Played"), c("completion_status", "Unfinished")])),
        ("Beaten / Completed", g("OR", vec![c("completion_status", "Beaten"), c("completion_status", "Completed")])),
    ];
    let status = ["Never Played", "Unfinished", "Beaten", "Completed", "Won't Play"].iter().map(|s| (*s, g("AND", vec![c("completion_status", s)]))).collect();
    (general, status)
}

/// A random id in the usual 8-4-4-4-12 form, for a new saved filter.
pub fn new_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(1);
    let mut x = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1) ^ N.fetch_add(1, Ordering::Relaxed).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let mut next = || { x = x.wrapping_add(0x9E37_79B9_7F4A_7C15); let mut z = x; z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9); z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB); z ^ (z >> 31) };
    let (a, b) = (next(), next());
    format!("{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}", (a >> 32) as u32, (a >> 16) as u16, a as u16 & 0xfff, (b >> 48) as u16 & 0x3fff | 0x8000, b & 0xffff_ffff_ffff)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_round_trip() {
        let rows = vec![Row { col: "tags".into(), op: "LIKE".into(), vals: vec!["Indie".into(), "RPG".into()], ..Default::default() },
                        Row { col: "playtime_forever".into(), op: ">".into(), text: "60".into(), ..Default::default() },
                        Row { col: "last_played".into(), op: "IS NULL".into(), ..Default::default() }];
        let tree = tree_from_rows(false, &rows);
        assert_eq!(tree["items"].as_array().unwrap().len(), 3);
        assert_eq!(tree["items"][0]["value"], json!(["Indie", "RPG"]));
        assert_eq!(tree["items"][2]["value"], json!(""));
        match rows_from_tree(&tree) { FromTree::Rows { or, rows: back } => { assert!(!or); assert_eq!(back.len(), 3); assert_eq!(back[0].vals, vec!["Indie", "RPG"]); assert_eq!(back[1].text, "60"); } o => panic!("{o:?}") }
    }

    #[test]
    fn incomplete_rows_are_left_out() {
        let tree = tree_from_rows(true, &[Row::new("name"), Row { col: "name".into(), op: "LIKE".into(), text: "zelda".into(), ..Default::default() }]);
        assert_eq!(tree["logic"], "OR");
        assert_eq!(tree["items"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn an_or_of_tag_conditions_becomes_one_row_with_values() {
        let tree = json!({"type": "group", "logic": "OR", "items": [
            {"type": "condition", "column": "tags", "operator": "LIKE", "value": "A"}, {"type": "condition", "column": "tags", "operator": "LIKE", "value": "B"},
            {"type": "condition", "column": "installed", "operator": "=", "value": "1"}]});
        match rows_from_tree(&tree) { FromTree::Rows { or, rows } => { assert!(or); assert_eq!(rows.len(), 2); assert_eq!(rows[0].vals, vec!["A", "B"]); } o => panic!("{o:?}") }
    }

    #[test]
    fn what_the_simple_builder_cannot_show_is_reported() {
        let nested = json!({"type": "group", "logic": "AND", "items": [{"type": "group", "logic": "OR", "items": []}]});
        assert_eq!(rows_from_tree(&nested), FromTree::Unsupported("nested groups"));
        let sql = json!({"type": "group", "logic": "AND", "items": [], "custom_sql": "installed = 1"});
        assert_eq!(rows_from_tree(&sql), FromTree::Unsupported("custom SQL"));
        let list = json!({"type": "group", "logic": "AND", "items": [{"type": "appid_list", "appids": [1, 2]}]});
        assert_eq!(rows_from_tree(&list), FromTree::Unsupported("a list of games"));
    }

    #[test]
    fn saved_filter_references_and_column_comparisons_round_trip() {
        let tree = json!({"type": "group", "logic": "AND", "items": [
            {"type": "saved_filter_ref", "id": "abc", "name": "Backlog", "negate": true},
            {"type": "condition", "column": "unlocked_achievements", "operator": ">=", "value_col": "total_achievements"}]});
        let FromTree::Rows { rows, .. } = rows_from_tree(&tree) else { panic!() };
        assert_eq!((rows[0].col.as_str(), rows[0].op.as_str(), rows[0].text.as_str()), ("saved_filter", "!=", "abc"));
        assert_eq!(rows[1].val_col, "total_achievements");
        assert_eq!(tree_from_rows(false, &rows)["items"], tree["items"]);
        assert!(!can_compare("name", "=") && !can_compare("playtime_forever", "LIKE") && can_compare("last_played", "<"));
    }

    #[test]
    fn nested_groups_expressions_and_opaque_nodes_round_trip() {
        let tree = json!({"type": "group", "logic": "OR", "items": [
            {"type": "condition", "column": "installed", "operator": "=", "value": "1"},
            {"type": "group", "logic": "AND", "items": [{"type": "condition", "column": "playtime_forever", "operator": ">", "value": "60"}, {"type": "custom_expr", "sql": "name LIKE 'a%'"}]},
            {"type": "appid_list", "appids": [1, 2]}]});
        let g = Group::from_tree(&tree);
        assert!(g.or && g.items.len() == 3);
        assert!(matches!(&g.items[1], Node::Group(sub) if sub.items.len() == 2 && matches!(sub.items[1], Node::Expr(_))));
        assert_eq!(g.to_value()["items"].as_array().unwrap().len(), 3);
        assert_eq!(g.to_value()["items"][2], tree["items"][2]);
        let mut empty = Group::default(); empty.items.push(Node::Group(Group::default())); empty.items.push(Node::Expr("  ".into()));
        assert!(empty.to_value()["items"].as_array().unwrap().is_empty());
    }

    #[test]
    fn the_search_condition_is_not_part_of_the_filter() {
        let tree = json!({"type": "group", "logic": "AND", "items": [
            {"type": "condition", "column": "name", "operator": "LIKE", "value": "zel"},
            {"type": "group", "logic": "AND", "items": [{"type": "condition", "column": "installed", "operator": "=", "value": "1"}]}]});
        let base = without_search(&tree);
        assert_eq!(base["items"][0]["column"], "installed");   // a lone nested group takes the tree's place
        assert!(is_empty_tree(&without_search(&json!({"type": "group", "logic": "AND", "items": [{"type": "condition", "column": "name", "operator": "LIKE", "value": "x"}]}))));
    }

    #[test]
    fn the_search_is_seeded_into_the_filter_as_a_title_row() {
        let cond = |c: &str, v: &str| json!({"type": "condition", "column": c, "operator": "LIKE", "value": v});
        let flat = json!({"type": "group", "logic": "AND", "items": [cond("tags", "Indie")]});
        let t = with_search(&flat, " zel ");
        assert_eq!(t["items"], json!([cond("name", "zel"), cond("tags", "Indie")]));   // flat AND: one more row
        assert_eq!(with_search(&json!({"type": "group", "logic": "AND", "items": []}), "zel")["items"], json!([cond("name", "zel")]));
        assert_eq!(with_search(&flat, "  "), flat);   // no words: the filter as it is
        let or = json!({"type": "group", "logic": "OR", "items": [cond("tags", "A"), cond("tags", "B")]});
        let t = with_search(&or, "zel");
        assert_eq!(t["logic"], "AND");
        assert_eq!(t["items"][1], or);   // an OR cannot take another row: nested, as the web page does
        let sql = json!({"type": "group", "logic": "AND", "items": [], "custom_sql": "playtime_forever > 0"});
        assert_eq!(with_search(&sql, "zel")["items"][1]["custom_sql"], "playtime_forever > 0");
        // an old search is replaced, not doubled
        let had = json!({"type": "group", "logic": "AND", "items": [cond("name", "old"), cond("tags", "Indie")]});
        assert_eq!(with_search(&had, "new")["items"], json!([cond("name", "new"), cond("tags", "Indie")]));
    }

    #[test]
    fn every_field_has_its_default_operator_and_ids_look_right() {
        for fd in FIELDS { assert!(!fd.ops.is_empty(), "{}", fd.key); let r = Row::new(fd.key); assert_eq!(r.col, fd.key); }
        let id = new_id();
        assert_eq!(id.len(), 36);
        assert_ne!(id, new_id());
    }
}
