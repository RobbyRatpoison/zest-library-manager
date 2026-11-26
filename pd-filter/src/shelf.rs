//! Port of `index._build_shelf_query`: the WHERE / ORDER BY / params of one Home page shelf.
//! Pinned by golden cases generated from the Python code (`tests/gen_golden.py`).
use crate::query::hide_duplicates_where;
use crate::{auto_cast_int_division, build_tree_sql, is_safe_sql, strip_sql_wrapper, truthy, Ctx};
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::OnceLock;

/// `config.BUILTIN_FILTERS`: (key, WHERE) for the SQL-backed presets. The widget presets have no SQL.
pub const BUILTIN_WHERE: &[(&str, &str)] = &[
    ("all_games", "1=1"),
    ("installed", "installed = 1"),
    ("not_installed", "installed = 0"),
    ("not_beaten", "completion_status IN ('Never Played', 'Unfinished')"),
    ("beaten", "completion_status IN ('Beaten', 'Completed')"),
    ("never_played", "completion_status = 'Never Played'"),
    ("unfinished", "completion_status = 'Unfinished'"),
    ("beaten_only", "completion_status = 'Beaten'"),
    ("completed", "completion_status = 'Completed'"),
    ("wont_play", "completion_status = 'Won''t Play'"),
];
pub const WIDGET_PRESETS: &[&str] = &["clock", "completion_pie", "achievement_pie"];

/// `index.SORT_COLUMNS` keys (the sorts a shelf may use).
pub const SORT_COLUMNS: &[&str] = &[
    "name", "playtime_forever", "last_played", "release_date", "date_added", "review_percentage", "weighted_percentage",
    "hltb_main", "hltb_extras", "hltb_completionist", "hltb_min", "hltb_max", "achievement_percent", "achievement_remaining",
    "total_reviews", "tag_similarity", "RANDOM()",
];

#[derive(Debug, Clone, PartialEq)]
pub struct ShelfQuery { pub where_: String, pub order: String, pub params: Vec<Value> }

fn builtin(key: &str) -> Option<&'static str> { BUILTIN_WHERE.iter().find(|(k, _)| *k == key).map(|(_, w)| *w) }
fn is_builtin_key(key: &str) -> bool { builtin(key).is_some() || WIDGET_PRESETS.contains(&key) }

fn s<'a>(v: &'a Value, k: &str) -> Option<&'a str> { v.get(k).and_then(|x| x.as_str()) }

/// `_filter_tree_to_sql`
pub fn filter_tree_to_sql(tree: &Value, params: &mut Vec<Value>, ctx: &Ctx) -> String {
    if !truthy(tree) { return "1=1".into(); }
    if let Some(cs) = tree.get("custom_sql").filter(|v| truthy(v)).and_then(|v| v.as_str()) {
        let cs = strip_sql_wrapper(cs);
        if !cs.is_empty() { return if is_safe_sql(&cs) { auto_cast_int_division(&cs) } else { "1=0".into() }; }
    }
    let sql = build_tree_sql(tree, params, ctx);
    if sql.is_empty() { "1=1".into() } else { sql }
}

/// None for a widget shelf (clock and the two charts have no SQL).
pub fn shelf_query(shelf: &Value, state: &Value, supplements: &HashMap<String, Vec<i64>>, priority: Option<&[String]>) -> Option<ShelfQuery> {
    static ORDER_BY: OnceLock<Regex> = OnceLock::new();
    static WHERE_PREFIX: OnceLock<Regex> = OnceLock::new();
    static NAME_OK: OnceLock<Regex> = OnceLock::new();
    let filter_key = s(shelf, "filter_key").filter(|k| !k.is_empty()).or_else(|| s(shelf, "preset")).unwrap_or("all_games").to_string();
    if WIDGET_PRESETS.contains(&filter_key.as_str()) { return None; }
    if s(shelf, "preset").map_or(false, |p| WIDGET_PRESETS.contains(&p)) && builtin(&filter_key).is_none() && !WIDGET_PRESETS.contains(&filter_key.as_str()) { return None; }

    let saved = state.get("saved_filters").and_then(|v| v.as_object());
    let mut ctx = Ctx { supplements: supplements.clone(), horizontal: s(state, "artwork_orientation") == Some("horizontal"), ..Default::default() };
    if let Some(sf) = saved { ctx.saved_filters = sf.clone(); }

    let mut params: Vec<Value> = Vec::new();
    let custom = s(shelf, "custom_sql").unwrap_or("").trim().to_string();
    let tree = shelf.get("filter_tree");
    let mut where_: String;
    if !custom.is_empty() {
        if !is_safe_sql(&custom) { return Some(ShelfQuery { where_: "1=0".into(), order: "name ASC".into(), params: vec![] }); }
        let w = ORDER_BY.get_or_init(|| Regex::new(r"(?i)\s*\bORDER\s+BY\s+.+$").unwrap()).replace(&custom, "").trim().to_string();
        where_ = WHERE_PREFIX.get_or_init(|| Regex::new(r"(?i)^\s*WHERE\s+").unwrap()).replace(&w, "").trim().to_string();
    } else if tree.map_or(false, |t| t.is_object() && (t.get("items").map_or(false, truthy) || t.get("custom_sql").map_or(false, truthy))) {
        where_ = filter_tree_to_sql(tree.unwrap(), &mut params, &ctx);
    } else if let Some(w) = builtin(&filter_key) {
        where_ = w.to_string();
    } else if let Some(sf) = saved.and_then(|m| m.get(&filter_key)) {
        let t = match sf.as_object() { Some(o) if o.contains_key("tree") => &o["tree"], _ => sf };
        where_ = filter_tree_to_sql(t, &mut params, &ctx);
    } else {
        where_ = "1=1".into();
    }

    let name_ok = NAME_OK.get_or_init(|| Regex::new(r"^[a-z][a-z0-9_]*$").unwrap());
    let hidden: Vec<&str> = shelf.get("hidden_platforms").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|p| p.as_str()).filter(|p| name_ok.is_match(p)).collect()).unwrap_or_default();
    if !hidden.is_empty() {
        let mut conds = Vec::new();
        if hidden.contains(&"steam") { conds.push("platform != 'steam'".to_string()); }
        let non_steam: Vec<&&str> = hidden.iter().filter(|p| **p != "steam").collect();
        if !non_steam.is_empty() { conds.push(format!("platform NOT IN ({})", non_steam.iter().map(|p| format!("'{p}'")).collect::<Vec<_>>().join(","))); }
        let plat = conds.join(" AND ");
        where_ = if where_ == "1=1" { plat } else { format!("({where_}) AND ({plat})") };
    }

    if state.get("hide_duplicates").map_or(true, truthy) {
        let (w, p) = hide_duplicates_where(&where_, &params, priority);
        where_ = w; params = p;
    }

    let sort_col = s(shelf, "sort_col").filter(|c| !c.is_empty());
    let sort_dir = s(shelf, "sort_dir").filter(|d| !d.is_empty()).unwrap_or("DESC");
    let order = match sort_col {
        Some(c) if SORT_COLUMNS.contains(&c) => {
            if c == "RANDOM()" { "RANDOM()".to_string() }
            else if let Some(expr) = crate::query::virtual_sort_expr(c) { format!("({expr}) {sort_dir}{}", crate::query::tie_break()) }   // games that tie: newest added first, as in the Library
            else { format!("{c}{} {sort_dir}{}", if c == "name" { " COLLATE NOCASE" } else { "" }, crate::query::tie_break()) }
        }
        _ => "name COLLATE NOCASE ASC".to_string(),
    };
    Some(ShelfQuery { where_, order, params })
}

#[allow(dead_code)]
fn _keep(_: bool) -> bool { is_builtin_key("") }

#[cfg(test)]
mod tie_break_tests {
    use super::*;
    #[test] fn a_shelf_sort_breaks_ties_newest_added_first_but_shuffle_and_the_default_are_left_alone() {
        let q = |shelf: Value| shelf_query(&shelf, &serde_json::json!({}), &HashMap::new(), None).unwrap().order;
        assert_eq!(q(serde_json::json!({"filter_key": "all_games", "sort_col": "playtime_forever", "sort_dir": "DESC"})), "playtime_forever DESC, date_added DESC, appid DESC");
        assert_eq!(q(serde_json::json!({"filter_key": "all_games", "sort_col": "RANDOM()"})), "RANDOM()");
        assert_eq!(q(serde_json::json!({"filter_key": "all_games"})), "name COLLATE NOCASE ASC");
    }
}
