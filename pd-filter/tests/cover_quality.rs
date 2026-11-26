//! The Cover Quality filter (`cover_quality`): ok / missing / wrong shape / low resolution, for the view's cover.
use pd_filter::{build_condition_sql, Ctx};
use serde_json::json;

fn db() -> rusqlite::Connection {
    let c = rusqlite::Connection::open_in_memory().unwrap();
    c.execute_batch("CREATE TABLE games (appid INTEGER, edge_vertical TEXT, edge_horizontal TEXT, vertical_art_w INTEGER, horizontal_art_w INTEGER, vertical_art_q INTEGER, horizontal_art_q INTEGER);
        INSERT INTO games VALUES (1, '-', '-', 600, 460, 95, 95);      -- fits, sharp
        INSERT INTO games VALUES (2, '-', 'y/1', 200, 300, 90, NULL);    -- vertical fits but small; horizontal wrong shape and small
        INSERT INTO games VALUES (3, 'y/1', NULL, 600, NULL, 95, NULL);  -- vertical wrong shape; no horizontal cover
        INSERT INTO games VALUES (4, NULL, NULL, NULL, NULL, NULL, NULL);  -- no covers
        INSERT INTO games VALUES (5, '-', '-', NULL, NULL, NULL, NULL);    -- fits, size never measured").unwrap();
    c
}

fn ids(c: &rusqlite::Connection, op: &str, val: &str, horizontal: bool) -> Vec<i64> {
    let ctx = Ctx { horizontal, ..Default::default() };
    let sql = build_condition_sql(&json!({"column": "cover_quality", "operator": op, "value": val}), &mut vec![], &ctx);
    let mut s = c.prepare(&format!("SELECT appid FROM games WHERE {sql} ORDER BY appid")).unwrap();
    s.query_map([], |r| r.get(0)).unwrap().flatten().collect()
}

#[test]
fn vertical_view() {
    let c = db();
    assert_eq!(ids(&c, "=", "ok", false), vec![1, 5]);
    assert_eq!(ids(&c, "=", "low", false), vec![2]);
    assert_eq!(ids(&c, "=", "wrong", false), vec![3]);
    assert_eq!(ids(&c, "=", "missing", false), vec![4]);
    assert_eq!(ids(&c, "!=", "ok", false), vec![2, 3, 4]);
}

#[test]
fn horizontal_view_uses_its_own_columns() {
    let c = db();
    assert_eq!(ids(&c, "=", "ok", true), vec![1, 5]);
    assert_eq!(ids(&c, "=", "low", true), vec![2]);
    assert_eq!(ids(&c, "=", "wrong", true), vec![2]);
    assert_eq!(ids(&c, "=", "missing", true), vec![3, 4]);
}

#[test]
fn unknown_value_matches_everything() {
    let c = db();
    assert_eq!(ids(&c, "=", "nonsense", false).len(), 5);
}

#[test]
fn saved_at_quality_95() {
    let c = db();
    assert_eq!(ids(&c, "=", "q95", false), vec![1, 3]);
    assert_eq!(ids(&c, "=", "q95", true), vec![1]);
    assert_eq!(ids(&c, "!=", "q95", false), vec![2, 4, 5]);   // 4 and 5 were never measured: not 95, so they are listed here
}
