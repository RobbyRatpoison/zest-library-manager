//! The library database's tables (PlayDate's `database.init_db()`): the `games` table with every column, its indexes and the `blacklist` table. Safe to run on a library that
//! already exists (a missing column is added, nothing else is touched), which is also how an older library gets columns newer versions added.
use rusqlite::Connection;
use std::path::Path;

/// Every column of `games` beyond its first two (`appid`, `name`), with PlayDate's declared types.
pub const GAME_COLUMNS: &[(&str, &str)] = &[
    ("playtime_forever", "INT"),
    ("installed", "INT"),
    ("last_played", "INTEGER"),
    ("date_added", "INTEGER"),
    ("developers", "TEXT"),
    ("publishers", "TEXT"),
    ("completion_status", "TEXT"),
    ("tags", "TEXT"),
    ("release_date", "INTEGER"),
    ("unlocked_achievements", "INT"),
    ("total_achievements", "INT"),
    ("review_score", "TEXT"),
    ("review_percentage", "INT"),
    ("metacritic_score", "INT"),
    ("short_description", "TEXT"),
    ("short_description_checked", "TEXT"),
    ("vertical_art_source", "TEXT"),
    ("horizontal_art_source", "TEXT"),
    ("icon_source", "TEXT"),
    ("icon_hash", "TEXT"),
    ("edge_vertical", "TEXT"),
    ("edge_horizontal", "TEXT"),
    ("weighted_percentage", "INT"),
    ("total_reviews", "INT"),
    ("positive_reviews", "INT"),
    ("groups", "TEXT"),
    ("genres", "TEXT"),
    ("categories", "TEXT"),
    ("is_free", "INT"),
    ("art_fetched", "TEXT"),
    ("meta_fetched", "TEXT"),
    ("cheevos_fetched", "TEXT"),
    ("protondb_tier", "TEXT"),
    ("protondb_confidence", "TEXT"),
    ("protondb_fetched", "TEXT"),
    ("hltb_main", "INT"),
    ("hltb_extras", "INT"),
    ("hltb_completionist", "INT"),
    ("hltb_id", "INT"),
    ("hltb_matched_name", "TEXT"),
    ("hltb_match_score", "INT"),
    ("hltb_fetched", "TEXT"),
    ("platform", "TEXT"),
    ("platform_id", "TEXT"),
    ("platform_slug", "TEXT"),
    ("platform_ns", "TEXT"),
    ("platform_appname", "TEXT"),
    ("steam_appid", "INTEGER"),
    ("meta_backfill_fetched", "TEXT"),
    ("install_path", "TEXT"),
    ("wine_prefix", "TEXT"),
    ("runner_path", "TEXT"),
    ("platform_executable", "TEXT"),
    ("launch_args", "TEXT"),
    ("duplicate_of", "TEXT"),
    ("duplicate_auto", "INT"),
    ("name_from_store", "INT"),
    ("tag_similarity", "REAL"),
    ("sg_dlc_win", "INT")
];

/// Columns only Zest uses (Python ignores them, and `tests/schema_golden.json` is Python's exact table, so they are kept apart): the stored pixel size and estimated JPEG quality of each cover,
/// for the Cover Quality filter.
pub const ZEST_COLUMNS: &[(&str, &str)] = &[("vertical_art_w", "INTEGER"), ("vertical_art_h", "INTEGER"), ("horizontal_art_w", "INTEGER"), ("horizontal_art_h", "INTEGER"), ("vertical_art_q", "INTEGER"), ("horizontal_art_q", "INTEGER")];

const INDEXES: &[(&str, &str)] = &[("installed", "installed"), ("completion_status", "completion_status"), ("last_played", "last_played"), ("playtime_forever", "playtime_forever"), ("platform", "platform"),
    ("name", "name"), ("duplicate_of", "duplicate_of"), ("release_date", "release_date"), ("hltb_fetched", "hltb_fetched"), ("meta_fetched", "meta_fetched"), ("art_fetched", "art_fetched"), ("cheevos_fetched", "cheevos_fetched")];

/// `init_db` on an open connection.
pub fn ensure(c: &Connection) -> rusqlite::Result<()> {
    c.execute("CREATE TABLE IF NOT EXISTS games (appid INTEGER PRIMARY KEY, name TEXT)", [])?;
    let have: Vec<String> = c.prepare("PRAGMA table_info(games)")?.query_map([], |r| r.get::<_, String>(1))?.flatten().collect();
    for (name, ty) in GAME_COLUMNS.iter().chain(ZEST_COLUMNS) { if !have.iter().any(|h| h == name) { c.execute(&format!("ALTER TABLE games ADD COLUMN {name} {ty}"), [])?; } }
    for (idx, col) in INDEXES { c.execute(&format!("CREATE INDEX IF NOT EXISTS idx_games_{idx} ON games({col})"), [])?; }
    c.execute("CREATE TABLE IF NOT EXISTS blacklist (appid INTEGER PRIMARY KEY, name TEXT, date_blacklisted TEXT, platform_id TEXT, platform TEXT)", [])?;
    // a blacklist from before those two columns existed
    for col in ["platform_id", "platform"] { if c.prepare(&format!("SELECT {col} FROM blacklist LIMIT 0")).is_err() { c.execute(&format!("ALTER TABLE blacklist ADD COLUMN {col} TEXT"), [])?; } }
    repair_empty_numbers(c)?;
    Ok(())
}

/// The repair for an existing library at startup (`schema::ensure` only runs when a library is made or lacks a column, so it would never reach one that already exists). Does nothing,
/// and takes no write lock, when there is nothing to repair. Returns how many values it fixed.
pub fn repair_library(db: &Path) -> usize {
    let Ok(c) = Connection::open(db) else { return 0 };
    c.busy_timeout(std::time::Duration::from_secs(10)).ok();
    let dirty = ["playtime_forever", "review_percentage", "weighted_percentage", "total_reviews", "positive_reviews", "total_achievements", "unlocked_achievements"].iter()
        .any(|col| c.query_row(&format!("SELECT 1 FROM games WHERE typeof({col}) = 'text' AND trim({col}) = '' LIMIT 1"), [], |_| Ok(())).is_ok());
    if !dirty { return 0; }
    let before = c.total_changes();
    if repair_empty_numbers(&c).is_err() { return 0; }
    let n = (c.total_changes() - before) as usize;
    crate::net::log(&format!("library: {n} empty number value(s) repaired"));
    n
}

/// A number column that holds the empty string (a row from an import without the value, or from an old PlayDate) sorts AFTER every number in SQLite, so a game with no playtime led
/// the "most played" list. Playtime becomes 0; the other counts become NULL (unknown), which sorts last. Safe to repeat.
pub fn repair_empty_numbers(c: &Connection) -> rusqlite::Result<()> {
    for (col, to) in [("playtime_forever", "0"), ("review_percentage", "NULL"), ("weighted_percentage", "NULL"), ("total_reviews", "NULL"), ("positive_reviews", "NULL"), ("total_achievements", "NULL"), ("unlocked_achievements", "NULL")] {
        c.execute(&format!("UPDATE games SET {col} = {to} WHERE typeof({col}) = 'text' AND trim({col}) = ''"), [])?;
    }
    Ok(())
}

/// Opens (making it when needed) the library database at `path` and makes sure its tables are complete.
pub fn init_db(path: &Path) -> Result<Connection, String> {
    let c = Connection::open(path).map_err(|e| format!("Could not open the library: {e}"))?;
    c.busy_timeout(std::time::Duration::from_secs(10)).ok();
    ensure(&c).map_err(|e| format!("Could not set up the library: {e}"))?;
    Ok(c)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn a_new_database_has_exactly_the_tables_columns_and_indexes_init_db_makes() {
        let g: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/schema_golden.json")).unwrap()).unwrap();
        let c = Connection::open_in_memory().unwrap(); ensure(&c).unwrap(); ensure(&c).unwrap();   // twice: repeating is harmless
        let cols = |t: &str| -> Vec<(String, String, i64)> { c.prepare(&format!("PRAGMA table_info({t})")).unwrap().query_map([], |r| Ok((r.get(1)?, r.get(2)?, r.get(5)?))).unwrap().flatten().filter(|(n, _, _): &(String, String, i64)| !ZEST_COLUMNS.iter().any(|z| z.0 == n)).collect() };
        let want = |k: &str| -> Vec<(String, String, i64)> { g[k].as_array().unwrap().iter().map(|r| (r[0].as_str().unwrap().into(), r[1].as_str().unwrap().into(), r[2].as_i64().unwrap())).collect() };
        assert_eq!(cols("games"), want("games")); assert_eq!(cols("blacklist"), want("blacklist"));
        let mut idx: Vec<(String, Vec<String>)> = c.prepare("PRAGMA index_list(games)").unwrap().query_map([], |r| r.get::<_, String>(1)).unwrap().flatten()
            .map(|n| { let cs = c.prepare(&format!("PRAGMA index_info({n})")).unwrap().query_map([], |r| r.get::<_, String>(2)).unwrap().flatten().collect(); (n, cs) }).collect();
        idx.sort();
        let want_idx: Vec<(String, Vec<String>)> = g["indexes"].as_array().unwrap().iter().map(|r| (r[0].as_str().unwrap().into(), r[1].as_array().unwrap().iter().map(|x| x.as_str().unwrap().into()).collect())).collect();
        assert_eq!(idx, want_idx);
    }
    #[test] fn an_old_library_gets_the_missing_columns_and_keeps_its_rows() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE games (appid INTEGER PRIMARY KEY, name TEXT, playtime_forever INT); INSERT INTO games VALUES (1, 'Old', 90); CREATE TABLE blacklist (appid INTEGER PRIMARY KEY, name TEXT);").unwrap();
        ensure(&c).unwrap();
        assert_eq!(c.query_row("SELECT name, playtime_forever, installed, hltb_fetched FROM games", [], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, Option<i64>>(2)?, r.get::<_, Option<String>>(3)?))).unwrap(), ("Old".into(), 90, None, None));
        c.execute("INSERT INTO blacklist (appid, platform_id, platform) VALUES (2, 'x', 'gog')", []).unwrap();
    }
    #[test] fn an_empty_string_in_a_number_column_is_repaired_so_it_cannot_lead_a_descending_sort() {
        let c = Connection::open_in_memory().unwrap(); ensure(&c).unwrap();
        c.execute("INSERT INTO games (appid, name, playtime_forever, review_percentage, total_achievements) VALUES (1, 'a', '', '', ' '), (2, 'b', 600, 90, 10), (3, 'c', NULL, NULL, NULL)", []).unwrap();
        ensure(&c).unwrap();
        let top: i64 = c.query_row("SELECT appid FROM games ORDER BY playtime_forever DESC LIMIT 1", [], |r| r.get(0)).unwrap(); assert_eq!(top, 2, "the game with 600 minutes leads, not the empty one");
        let (pt, rv, ach): (i64, Option<i64>, Option<i64>) = c.query_row("SELECT playtime_forever, review_percentage, total_achievements FROM games WHERE appid = 1", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!((pt, rv, ach), (0, None, None)); assert_eq!(c.query_row("SELECT playtime_forever FROM games WHERE appid = 2", [], |r| r.get::<_, i64>(0)).unwrap(), 600, "real values are left alone");
    }
}

#[cfg(test)]
mod repair_library_tests {
    use super::*;
    #[test] fn an_existing_library_is_repaired_once_and_a_clean_one_is_left_alone() {
        let d = std::env::temp_dir().join(format!("zest-repair-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        let db = d.join("games_1.db"); init_db(&db).unwrap();
        { let c = Connection::open(&db).unwrap(); c.execute("INSERT INTO games (appid, name, playtime_forever, total_reviews) VALUES (1, 'Eternal dark winter', '', ''), (2, 'real', 600, 5)", []).unwrap(); }
        assert_eq!(repair_library(&db), 2, "the empty playtime and the empty review count");
        assert_eq!(repair_library(&db), 0, "nothing left to do");
        let c = Connection::open(&db).unwrap();
        assert_eq!(c.query_row("SELECT appid FROM games ORDER BY playtime_forever DESC LIMIT 1", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
        assert_eq!(c.query_row("SELECT playtime_forever FROM games WHERE appid = 1", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    }
}
