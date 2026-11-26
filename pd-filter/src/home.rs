//! Port of the Home page's data assembly (`index.index`): which games each shelf shows, and how shelves group into rows.
//! Pinned by golden data captured from the real route on the project's test database (`tests/gen_golden.py`).
use crate::shelf::shelf_query;
use rusqlite::types::Value as SqlValue;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq)]
pub struct ShelfGames { pub id: String, pub appids: Vec<i64> }

#[derive(Debug, Clone, PartialEq)]
pub enum HomeRow { Single(String), Split { shelves: Vec<String>, row_height: Option<i64> } }

fn sql_value(v: &Value) -> SqlValue {
    match v {
        Value::Null => SqlValue::Null,
        Value::Bool(b) => SqlValue::Integer(*b as i64),
        Value::Number(n) => n.as_i64().map(SqlValue::Integer).unwrap_or_else(|| SqlValue::Real(n.as_f64().unwrap_or(0.0))),
        Value::String(s) => SqlValue::Text(s.clone()),
        other => SqlValue::Text(other.to_string()),
    }
}

/// The shelves that are shown, in display order (`visible` defaults to true).
fn visible_shelves(state: &Value) -> Vec<&Value> {
    state.get("shelves").and_then(|v| v.as_array()).map(|a| a.iter().filter(|s| s.get("visible").map_or(true, crate::truthy)).collect()).unwrap_or_default()
}

/// Which games each visible shelf shows. Shelves are fetched in dedup-priority order (a shelf that dedups skips games an earlier
/// one already took); the result is returned in display order. A widget shelf (clock, charts) or a query that fails shows nothing.
pub fn home_shelves(conn: &rusqlite::Connection, state: &Value, supplements: &HashMap<String, Vec<i64>>, priority: Option<&[String]>) -> Vec<ShelfGames> {
    let shelves = visible_shelves(state);
    let key = |s: &&Value| -> i64 {
        if s.get("dedup").map_or(true, crate::truthy) { s.get("dedup_priority").and_then(|v| v.as_i64()).unwrap_or(999) } else { 9999 }
    };
    let mut order: Vec<&Value> = shelves.clone();
    order.sort_by_key(|s| key(s));   // stable, like Python's sorted()
    let mut used: HashSet<i64> = HashSet::new();
    let mut by_id: HashMap<String, Vec<i64>> = HashMap::new();
    for shelf in order {
        let id = shelf.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let Some(q) = shelf_query(shelf, state, supplements, priority) else { by_id.insert(id, vec![]); continue };
        let limit = shelf.get("limit").and_then(|v| v.as_i64()).unwrap_or(10);
        let dedup = shelf.get("dedup").map_or(true, crate::truthy);
        // ", appid" makes rows that tie on the sort value come back in a fixed order (SQLite leaves ties unspecified).
        let tiebreak = if q.order.contains("RANDOM()") { "" } else { ", appid" };
        let sql = format!("SELECT * FROM games WHERE {} ORDER BY {}{tiebreak}", q.where_, q.order);
        let rows: Result<Vec<i64>, _> = conn.prepare(&sql).and_then(|mut st| {
            let col = st.column_index("appid")?;
            let it = st.query_map(rusqlite::params_from_iter(q.params.iter().map(sql_value)), |r| r.get::<_, i64>(col))?;
            it.collect()
        });
        let Ok(rows) = rows else { by_id.insert(id, vec![]); continue };
        let mut games = Vec::new();
        for appid in rows {
            if dedup && used.contains(&appid) { continue; }
            games.push(appid);
            if dedup { used.insert(appid); }
            if limit != 0 && games.len() as i64 >= limit { break; }
        }
        by_id.insert(id, games);
    }
    shelves.iter().map(|s| { let id = s.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(); ShelfGames { appids: by_id.remove(&id).unwrap_or_default(), id } }).collect()
}

/// Rows of the page: shelves sharing a `split_group` form one side-by-side row, emitted where the first member appears.
pub fn home_rows(state: &Value) -> Vec<HomeRow> {
    let shelves = visible_shelves(state);
    let (mut rows, mut seen) = (Vec::new(), HashSet::new());
    for s in &shelves {
        let id = s.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        match s.get("split_group").filter(|g| crate::truthy(g)).map(|g| g.to_string()) {
            Some(group) => {
                if !seen.insert(group.clone()) { continue; }
                let members: Vec<&&Value> = shelves.iter().filter(|m| m.get("split_group").map(|g| g.to_string()) == Some(group.clone())).collect();
                rows.push(HomeRow::Split {
                    shelves: members.iter().map(|m| m.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string()).collect(),
                    row_height: members[0].get("row_height").and_then(|v| v.as_i64()),
                });
            }
            None => rows.push(HomeRow::Single(id)),
        }
    }
    rows
}

/// `config.DEFAULT_SHELVES` (generated from the Python by `tests/gen_golden.py`, verified against it by the tests): the shelves used
/// when `state.json` has none.
pub fn default_shelves() -> Value { serde_json::from_str(include_str!("../data/default_shelves.json")).expect("default_shelves.json") }

/// `index.achievement_bucket`
pub fn achievement_bucket(unlocked: f64, total: f64) -> &'static str {
    let pct = unlocked / total * 100.0;
    if pct <= 25.0 { "0-25%" } else if pct <= 50.0 { "26-50%" } else if pct <= 75.0 { "51-75%" } else { "76-100%" }
}

/// The numbers behind the two chart widgets: Steam games per completion status, and per achievement-completion quartile.
pub fn widget_counts(conn: &rusqlite::Connection) -> (std::collections::BTreeMap<String, i64>, std::collections::BTreeMap<String, i64>) {
    use rusqlite::types::ValueRef;
    let mut completion = std::collections::BTreeMap::new();
    if let Ok(mut st) = conn.prepare("SELECT completion_status, COUNT(*) as cnt FROM games WHERE completion_status IS NOT NULL AND platform = 'steam' GROUP BY completion_status") {
        if let Ok(rows) = st.query_map([], |r| Ok((r.get::<_, String>(0).unwrap_or_default(), r.get::<_, i64>(1)?))) { for r in rows.flatten() { completion.insert(r.0, r.1); } }
    }
    let mut ach = std::collections::BTreeMap::new();
    if let Ok(mut st) = conn.prepare("SELECT unlocked_achievements, total_achievements FROM games WHERE platform = 'steam' AND total_achievements > 0") {
        let _ = st.query_map([], |r| {
            let num = |v: ValueRef| -> Option<f64> { match v { ValueRef::Integer(i) => Some(i as f64), ValueRef::Real(f) => Some(f), ValueRef::Null => Some(0.0), _ => None } };
            // Python divides `(unlocked or 0) / total`; a text value there raises and the whole loop is abandoned (it sits in a try/except)
            Ok((num(r.get_ref(0)?), num(r.get_ref(1)?)))
        }).map(|rows| {
            for r in rows.flatten() {
                let (Some(u), Some(t)) = r else { break };
                if t == 0.0 { break; }
                *ach.entry(achievement_bucket(u, t).to_string()).or_insert(0) += 1;
            }
        });
    }
    (completion, ach)
}

pub struct ShelfSql { pub sql: String, pub params: Vec<Value> }
const SHELF_COLS: &str = "appid, name, installed, completion_status, platform, total_achievements, unlocked_achievements, review_percentage, weighted_percentage, total_reviews, hltb_main, hltb_extras, hltb_completionist";

fn tiebreak(order: &str) -> String { if order.contains("RANDOM()") { order.to_string() } else { format!("{order}, appid") } }

/// `/api/shuffle-shelf`: the shelf's games in random order (its limit, 0 = unlimited).
pub fn shuffle_query(shelf: &Value, state: &Value, supp: &HashMap<String, Vec<i64>>, priority: Option<&[String]>) -> Option<ShelfSql> {
    let q = shelf_query(shelf, state, supp, priority)?;
    let limit = shelf.get("limit").and_then(|v| v.as_i64()).unwrap_or(10);
    let mut params = q.params;
    params.push(Value::from(if limit != 0 { limit } else { -1 }));
    Some(ShelfSql { sql: format!("SELECT {SHELF_COLS} FROM games WHERE {} ORDER BY RANDOM() LIMIT ?", q.where_), params })
}

/// `/api/refill-shelf`: more games for a shelf, skipping ones already on screen. `limit` overrides the saved limit (clamped 0..=200).
pub fn refill_query(shelf: &Value, state: &Value, supp: &HashMap<String, Vec<i64>>, priority: Option<&[String]>, exclude: &[i64], limit: Option<i64>) -> Option<ShelfSql> {
    let q = shelf_query(shelf, state, supp, priority)?;
    let limit = limit.or_else(|| shelf.get("limit").and_then(|v| v.as_i64())).unwrap_or(10).clamp(0, 200);
    let sql_limit = if limit != 0 { limit } else { -1 };
    let order = tiebreak(&q.order);
    let mut params = q.params;
    if exclude.is_empty() {
        params.push(Value::from(sql_limit));
        Some(ShelfSql { sql: format!("SELECT {SHELF_COLS} FROM games WHERE {} ORDER BY {order} LIMIT ?", q.where_), params })
    } else {
        params.extend(exclude.iter().map(|a| Value::from(*a)));
        params.push(Value::from(sql_limit));
        let ph = exclude.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        Some(ShelfSql { sql: format!("SELECT {SHELF_COLS} FROM games WHERE ({}) AND appid NOT IN ({ph}) ORDER BY {order} LIMIT ?", q.where_), params })
    }
}
