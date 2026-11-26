//! Port of `library._library_query`: the whole Library SELECT for a saved state (sort, filter, hidden platforms,
//! duplicate hiding). Pinned by golden cases generated from the Python code (`tests/gen_golden.py`).
//! Not ported on purpose: `resolve_active_filter_tree`'s expansion of compact `pagywosg_verified` payloads
//! (it only changes the tree sent to the UI, never the SQL).
use crate::{auto_cast_int_division, build_tree_sql, is_safe_sql, strip_sql_wrapper, truthy, Ctx, SAFE_COLUMNS};
use regex::Regex;
use serde_json::Value;
use std::sync::OnceLock;

pub const PLATFORM_PRIORITY_DEFAULT: &[&str] = &["steam", "gog", "epic_games", "ea_app", "ubisoft", "itch_io", "custom"];

const HLTB_MIN: &str = "CASE WHEN NULLIF(hltb_main, 0) IS NULL AND NULLIF(hltb_extras, 0) IS NULL AND NULLIF(hltb_completionist, 0) IS NULL THEN NULL ELSE MIN(COALESCE(NULLIF(hltb_main, 0), 999999999), COALESCE(NULLIF(hltb_extras, 0), 999999999), COALESCE(NULLIF(hltb_completionist, 0), 999999999)) END";
const HLTB_MAX: &str = "CASE WHEN NULLIF(hltb_main, 0) IS NULL AND NULLIF(hltb_extras, 0) IS NULL AND NULLIF(hltb_completionist, 0) IS NULL THEN NULL ELSE MAX(COALESCE(NULLIF(hltb_main, 0), 0), COALESCE(NULLIF(hltb_extras, 0), 0), COALESCE(NULLIF(hltb_completionist, 0), 0)) END";
const ACH_PERCENT: &str = "CASE WHEN CAST(total_achievements AS INTEGER) > 0 THEN (100.0 * CAST(unlocked_achievements AS INTEGER) / CAST(total_achievements AS INTEGER)) ELSE NULL END";
const ACH_REMAINING: &str = "CASE WHEN CAST(total_achievements AS INTEGER) > 0 THEN (CAST(total_achievements AS INTEGER) - CAST(unlocked_achievements AS INTEGER)) ELSE NULL END";

/// `VIRTUAL_SORT_COLS`: sort names that expand to SQL expressions (NULL = no data, always sorted last).
pub(crate) fn virtual_sort_expr(col: &str) -> Option<&'static str> {
    Some(match col {
        "hltb_main" => "NULLIF(hltb_main, 0)",
        "hltb_extras" => "NULLIF(hltb_extras, 0)",
        "hltb_completionist" => "NULLIF(hltb_completionist, 0)",
        "hltb_min" => HLTB_MIN,
        "hltb_max" => HLTB_MAX,
        "achievement_percent" => ACH_PERCENT,
        "achievement_remaining" => ACH_REMAINING,
        "tag_similarity" => "tag_similarity",
        "metacritic_score" => "metacritic_score",
        _ => return None,
    })
}

fn platform_name_ok(p: &str) -> bool {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^[a-z][a-z0-9_]*$").unwrap()).is_match(p)
}

/// `database._platform_rank_sql`
fn platform_rank_sql(column: &str, priority: Option<&[String]>) -> String {
    let default: Vec<String> = PLATFORM_PRIORITY_DEFAULT.iter().map(|s| s.to_string()).collect();
    let src: &[String] = match priority { Some(p) if !p.is_empty() => p, _ => &default };
    let order: Vec<&String> = src.iter().filter(|p| platform_name_ok(p)).collect();
    let whens: Vec<String> = order.iter().enumerate().map(|(i, p)| format!("WHEN '{p}' THEN {i}")).collect();
    format!("(CASE {column} {} ELSE {} END)", whens.join(" "), order.len())
}

/// `database.hide_duplicates_where`: returns (where, params).
pub fn hide_duplicates_where(where_: &str, params: &[Value], priority: Option<&[String]>) -> (String, Vec<Value>) {
    if where_.is_empty() || where_ == "1=1" {
        return ("(duplicate_of IS NULL OR duplicate_of = '')".into(), params.to_vec());
    }
    let (mine, theirs) = (platform_rank_sql("games.platform", priority), platform_rank_sql("_g.platform", priority));
    let top = "COALESCE(NULLIF(games.duplicate_of, ''), CAST(games.appid AS TEXT))";
    let rank = format!("({theirs} < {mine} OR ({theirs} = {mine} AND _g.appid < games.appid))");
    let better = format!("(EXISTS (SELECT 1 FROM games AS _g WHERE _g.appid = CAST({top} AS INTEGER) AND _g.appid != games.appid AND {rank} AND ({where_})) OR EXISTS (SELECT 1 FROM games AS _g WHERE _g.duplicate_of = {top} AND _g.appid != games.appid AND {rank} AND ({where_})))");
    let grouped = "(games.duplicate_of IS NOT NULL AND games.duplicate_of != '' OR CAST(games.appid AS TEXT) IN (SELECT duplicate_of FROM games WHERE duplicate_of IS NOT NULL AND duplicate_of != ''))";
    let mut p = params.to_vec(); p.extend_from_slice(params); p.extend_from_slice(params);
    (format!("({where_}) AND (NOT {grouped} OR NOT {better})"), p)
}

/// `_strip_search_condition`: drop a top-level `name LIKE` condition (the quick-search bar) from a tree.
pub fn strip_search_condition(node: &Value) -> Value {
    let Some(items) = node.get("items").and_then(|v| v.as_array()).filter(|a| !a.is_empty()) else { return node.clone() };
    let kept: Vec<Value> = items.iter().filter(|it| !(it.get("type").and_then(|v| v.as_str()) == Some("condition")
        && it.get("column").and_then(|v| v.as_str()) == Some("name") && it.get("operator").and_then(|v| v.as_str()) == Some("LIKE"))).cloned().collect();
    if kept.len() == 1 && kept[0].get("type").and_then(|v| v.as_str()) == Some("group") { return kept[0].clone(); }
    let mut out = node.clone();
    out["items"] = Value::Array(kept);
    out
}

#[derive(Debug, Clone, PartialEq)]
pub struct LibraryQuery {
    pub query: String,
    pub params: Vec<Value>,
    pub sort_col: String,
    pub sort_ord: String,
    pub hidden_platforms: Vec<String>,
    pub active_filter_name: Option<String>,
}

/// `effective_platform_priority` is a runtime fact (saved order plus registered plugin platforms), so the caller passes it in.
pub fn library_query(state: &Value, supplements: &std::collections::HashMap<String, Vec<i64>>, priority: Option<&[String]>) -> LibraryQuery {
    let mut sort_col = state.get("sort").and_then(|v| v.as_str()).unwrap_or("name").to_string();
    let mut sort_ord = state.get("order").and_then(|v| v.as_str()).unwrap_or("ASC").to_string();
    if sort_ord != "ASC" && sort_ord != "DESC" { sort_ord = "ASC".into(); }
    if sort_col == "random" { sort_col = "RANDOM()".into(); sort_ord = String::new(); }
    else if let Some(expr) = virtual_sort_expr(&sort_col) { sort_col = format!("CASE WHEN ({expr}) IS NULL THEN 1 ELSE 0 END, ({expr})"); }
    else if !SAFE_COLUMNS.contains(&sort_col.as_str()) { sort_col = "name".into(); }
    if sort_col == "name" { sort_col = "name COLLATE NOCASE".into(); }

    let mut ctx = Ctx { supplements: supplements.clone(), horizontal: state.get("artwork_orientation").and_then(|v| v.as_str()) == Some("horizontal"), ..Default::default() };
    if let Some(sf) = state.get("saved_filters").and_then(|v| v.as_object()) { ctx.saved_filters = sf.clone(); }

    let mut params: Vec<Value> = Vec::new();
    let mut where_ = "1=1".to_string();

    // resolve_active_filter_tree: a {saved_filter: name} sentinel points at a saved filter; a deleted one resolves to nothing
    let raw = state.get("filter_tree");
    let active_filter_name = raw.and_then(|r| r.as_object()).and_then(|o| o.get("saved_filter")).and_then(|v| v.as_str()).map(|s| s.to_string());
    let tree: Option<Value> = match raw {
        Some(Value::Object(o)) if o.contains_key("saved_filter") => {
            let name = o["saved_filter"].as_str().unwrap_or("");
            match ctx.saved_filters.get(name).filter(|v| truthy(v)) {
                Some(sf) => Some(match sf.as_object() { Some(o) if o.contains_key("tree") => o["tree"].clone(), _ => sf.clone() }),
                None => None,
            }
        }
        Some(v) => Some(v.clone()),
        None => None,
    };
    if let Some(tree) = tree.filter(truthy) {
        let custom = strip_sql_wrapper(tree.get("custom_sql").and_then(|v| v.as_str()).unwrap_or(""));
        if !custom.is_empty() {
            where_ = if is_safe_sql(&custom) { auto_cast_int_division(&custom) } else { "1=0".into() };
        } else if tree.is_object() {
            let sql = build_tree_sql(&strip_search_condition(&tree), &mut params, &ctx);
            if !sql.is_empty() && sql != "1=1" { where_ = sql; }
        }
    }

    let hidden_platforms: Vec<String> = state.get("hidden_platforms").and_then(|v| v.as_array()).map(|a| a.iter()
        .filter_map(|p| p.as_str()).filter(|p| platform_name_ok(p)).map(|p| p.to_string()).collect()).unwrap_or_default();
    if !hidden_platforms.is_empty() {
        let mut conds = Vec::new();
        if hidden_platforms.iter().any(|p| p == "steam") { conds.push("platform != 'steam'".to_string()); }
        let non_steam: Vec<&String> = hidden_platforms.iter().filter(|p| *p != "steam").collect();
        if !non_steam.is_empty() {
            conds.push(format!("platform NOT IN ({})", non_steam.iter().map(|_| "?").collect::<Vec<_>>().join(",")));
            params.extend(non_steam.iter().map(|p| Value::String((*p).clone())));
        }
        let cond = conds.join(" AND ");
        where_ = if where_ == "1=1" { cond } else { format!("({where_}) AND ({cond})") };
    }

    if state.get("hide_duplicates").map_or(true, truthy) {
        let (w, p) = hide_duplicates_where(&where_, &params, priority);
        where_ = w; params = p;
    }

    let mut query = format!("SELECT * FROM games WHERE {where_} ORDER BY {sort_col} {sort_ord}").trim_end().to_string();
    if !sort_ord.is_empty() { query.push_str(tie_break()); }   // not for Random
    LibraryQuery { query, params, sort_col, sort_ord, hidden_platforms, active_filter_name }
}

/// What decides the order of games that tie on the chosen sort (hundreds share 0 hours, one review score or one release day): the one added most recently comes first, and where
/// that ties as well (a bulk import) the higher appid. Before this the order was whatever SQLite happened to produce, in practice ascending appid.
pub const TIE_BREAK: &str = ", date_added DESC, appid DESC";
static TIE_BREAK_ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
/// Switches the tie-break off for a test that compares whole result lists with Python's (which left ties to SQLite); the app never does.
pub fn set_tie_break(on: bool) { TIE_BREAK_ON.store(on, std::sync::atomic::Ordering::SeqCst); }
/// `TIE_BREAK`, or nothing when switched off.
pub fn tie_break() -> &'static str { if TIE_BREAK_ON.load(std::sync::atomic::Ordering::SeqCst) { TIE_BREAK } else { "" } }

/// The order a Library sort starts in when it is chosen (PlayDate's `_sortDefaultOrder`): names and times to finish go up, everything else (hours, dates, scores, counts, similarity) down.
pub fn default_sort_order(col: &str) -> &'static str {
    match col {
        "name" | "hltb_main" | "hltb_extras" | "hltb_completionist" | "hltb_min" | "hltb_max" | "achievement_remaining" => "ASC",
        "playtime_forever" | "release_date" | "last_played" | "date_added" | "review_percentage" | "weighted_percentage" | "metacritic_score" | "achievement_percent" | "total_reviews" | "tag_similarity" => "DESC",
        _ => "ASC",   // anything PlayDate had no entry for
    }
}

#[cfg(test)]
mod default_order_tests {
    use super::default_sort_order as d;
    #[test] fn each_sort_starts_in_playdates_default_order() {
        for c in ["name", "hltb_main", "hltb_extras", "hltb_completionist", "hltb_min", "hltb_max", "achievement_remaining"] { assert_eq!(d(c), "ASC", "{c}"); }
        for c in ["playtime_forever", "release_date", "last_played", "date_added", "review_percentage", "weighted_percentage", "metacritic_score", "achievement_percent", "total_reviews", "tag_similarity"] { assert_eq!(d(c), "DESC", "{c}"); }
        assert_eq!(d("something_new"), "ASC");
    }

    #[cfg(test)] mod tie_break_tests {
        use crate::query::library_query;
        #[test] fn games_that_tie_come_newest_added_first_and_random_is_left_alone() {
            let q = library_query(&serde_json::json!({"sort": "playtime_forever", "order": "DESC"}), &Default::default(), None);
            assert!(q.query.ends_with("ORDER BY playtime_forever DESC, date_added DESC, appid DESC"), "{}", q.query);
            let c = rusqlite::Connection::open_in_memory().unwrap(); crate::schema::ensure(&c).unwrap();
            c.execute("INSERT INTO games (appid, name, playtime_forever, date_added) VALUES (1, 'old', 0, 100), (2, 'new', 0, 900), (3, 'newest, same day', 0, 900), (4, 'played', 50, 1)", []).unwrap();
            let ids: Vec<i64> = c.prepare(&q.query.replace("SELECT *", "SELECT appid")).unwrap().query_map([], |r| r.get(0)).unwrap().flatten().collect();
            assert_eq!(ids, vec![4, 3, 2, 1], "most played first; the unplayed ones newest added first, the higher appid on the same date");
            assert!(!library_query(&serde_json::json!({"sort": "random"}), &Default::default(), None).query.contains("date_added"), "Random has no tie to break");
        }
    }
}
