//! Replays the duplicate-detection golden cases generated from the real Python (`gen_dupes_golden.py`) and checks the intended differences.
use pd_filter::dupes::*;
use rusqlite::Connection;
use serde_json::Value;

fn load() -> Value { serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/dupes_golden.json")).expect("run gen_dupes_golden.py first")).unwrap() }

fn db(rows: &Value, with_column: bool) -> Connection {
    let c = Connection::open_in_memory().unwrap();
    c.execute("CREATE TABLE games (appid INTEGER PRIMARY KEY, name TEXT, platform TEXT, duplicate_of TEXT, duplicate_auto INTEGER DEFAULT 0)", []).unwrap();
    for r in rows.as_array().unwrap() {
        c.execute("INSERT INTO games (appid, name, platform, duplicate_of, duplicate_auto) VALUES (?, ?, ?, ?, ?)",
            rusqlite::params![r["appid"].as_i64().unwrap(), r["name"].as_str(), r["platform"].as_str(), r.get("duplicate_of").and_then(|v| v.as_str()), r.get("duplicate_auto").and_then(|v| v.as_i64()).unwrap_or(0)]).unwrap();
    }
    if with_column { c.execute("ALTER TABLE games ADD COLUMN dup_checked INTEGER", []).unwrap(); } else { ensure_column(&c).unwrap(); c.execute("UPDATE games SET dup_checked = NULL", []).unwrap(); }
    c
}
fn state(c: &Connection) -> Vec<(i64, Option<String>, i64)> {
    c.prepare("SELECT appid, duplicate_of, COALESCE(duplicate_auto, 0) FROM games ORDER BY appid").unwrap().query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().flatten().collect()
}
fn expect(v: &Value) -> Vec<(i64, Option<String>, i64)> { v.as_array().unwrap().iter().map(|e| (e["appid"].as_i64().unwrap(), e["duplicate_of"].as_str().map(String::from), e["duplicate_auto"].as_i64().unwrap())).collect() }
fn prio(v: &Value) -> Vec<String> { v.as_array().unwrap().iter().map(|p| p.as_str().unwrap().to_string()).collect() }

#[test]
fn normalisers_match_python() {
    for c in load()["norm"].as_array().unwrap() {
        let (name, py) = (c["name"].as_str().unwrap(), c["py"].as_str().unwrap());
        assert_eq!(norm_edition(name), py, "norm_edition({name:?})");
    }
}

#[test]
fn first_examination_matches_pythons_auto_detection() {
    let g = load();
    let (mut n, mut bad) = (0, Vec::new());
    for c in g["detect"].as_array().unwrap() {
        let conn = db(&c["rows"], true);
        examine_new(&conn, &prio(&c["priority"])).unwrap();
        // Python's auto_detect also leaves duplicate_auto = 0 on unlinked rows; ours does too
        n += 1;
        if state(&conn) != expect(&c["expect"]) { if bad.len() < 3 { bad.push(format!("rows {}\n priority {}\n rust   {:?}\n python {:?}", c["rows"], c["priority"], state(&conn), expect(&c["expect"]))); } else { bad.push(String::new()); } }
    }
    assert!(bad.is_empty(), "{} of {n} cases differ:\n{}", bad.len(), bad.iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join("\n"));
}

/// Two games pointing at each other: Python leaves both hidden when they are on one platform or rank equally.
fn has_loop(st: &[(i64, Option<String>, i64)]) -> bool {
    st.iter().any(|(a, d, _)| d.as_ref().and_then(|d| d.parse::<i64>().ok()).map_or(false, |t| st.iter().any(|(b, e, _)| *b == t && e.as_deref() == Some(&a.to_string()))))
}

#[test]
fn realign_matches_pythons_manual_link_alignment() {
    let g = load();
    let (mut n, mut loops, mut bad) = (0, 0, Vec::new());
    for c in g["align"].as_array().unwrap() {
        let conn = db(&c["rows"], true);
        realign(&conn, &prio(&c["priority"])).unwrap();
        let (mine, py) = (state(&conn), expect(&c["expect"]));
        n += 1;
        if has_loop(&py) {
            // intended difference: Python hides both copies of a mutual pair it cannot rank; Zest always leaves one shown
            loops += 1;
            let hidden = |st: &[(i64, Option<String>, i64)]| st.iter().filter(|r| r.1.is_some()).count();
            assert!(!has_loop(&mine) && hidden(&mine) < hidden(&py), "loop case not resolved: {mine:?} vs python {py:?}");
            continue;
        }
        if mine != py { if bad.len() < 3 { bad.push(format!("rows {}\n priority {}\n rust   {:?}\n python {:?}", c["rows"], c["priority"], mine, py)); } else { bad.push(String::new()); } }
    }
    assert!(loops > 0 && loops < n, "fixtures should contain some, not only, loop cases ({loops} of {n})");
    assert!(bad.is_empty(), "{} of {n} cases differ:\n{}", bad.len(), bad.iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join("\n"));
}

fn rows_json(v: &[(i64, &str, &str)]) -> Value { Value::Array(v.iter().map(|(a, n, p)| serde_json::json!({"appid": a, "name": n, "platform": p})).collect()) }
fn p3() -> Vec<String> { ["steam", "gog", "epic_games"].iter().map(|s| s.to_string()).collect() }

#[test]
fn three_copies_end_as_one_star_at_the_best_platform() {
    for order in [[1, 2, 3], [3, 2, 1], [2, 1, 3]] {
        let all = [(1, "Portal", "epic_games"), (2, "portal", "gog"), (3, "Portal!", "steam")];
        let rows: Vec<_> = order.iter().map(|i| all[*i as usize - 1]).collect();
        let c = db(&rows_json(&rows), true);
        examine_new(&c, &p3()).unwrap();
        assert_eq!(state(&c), vec![(1, Some("3".into()), 1), (2, Some("3".into()), 1), (3, None, 0)], "insertion order {order:?}");
    }
}

#[test]
fn only_new_games_are_examined_and_nothing_is_relinked() {
    let c = db(&rows_json(&[(1, "Portal", "steam"), (2, "Portal", "gog")]), false);
    assert_eq!(examine_new(&c, &p3()).unwrap(), Detected { linked: 1, examined: 2 });
    unlink(&c, 2).unwrap();                                        // the user says these are two games
    assert_eq!(examine_new(&c, &p3()).unwrap(), Detected { linked: 0, examined: 0 });
    c.execute("UPDATE games SET dup_checked = NULL", []).unwrap(); // even a full re-examination respects the "no"
    examine_new(&c, &p3()).unwrap();
    assert_eq!(state(&c)[1], (2, None, 2));
    // a new copy still links to the shown game
    c.execute("INSERT INTO games (appid, name, platform) VALUES (3, 'Portal', 'epic_games')", []).unwrap();
    assert_eq!(examine_new(&c, &p3()).unwrap().linked, 1);
    assert_eq!(state(&c)[2], (3, Some("1".into()), 1));
}

#[test]
fn a_new_higher_priority_copy_takes_over_the_group() {
    let c = db(&rows_json(&[(1, "Portal", "gog"), (2, "Portal", "epic_games")]), false);
    examine_new(&c, &p3()).unwrap();
    c.execute("INSERT INTO games (appid, name, platform) VALUES (3, 'Portal', 'steam')", []).unwrap();
    examine_new(&c, &p3()).unwrap();
    assert_eq!(state(&c), vec![(1, Some("3".into()), 1), (2, Some("3".into()), 1), (3, None, 0)]);
}

#[test]
fn edition_names_are_suggestions_never_links() {
    let c = db(&rows_json(&[(1, "Hades", "steam"), (2, "Hades - Definitive Edition", "gog"), (3, "", "gog"), (4, "!!!", "steam"), (5, "!!", "gog")]), false);
    examine_new(&c, &p3()).unwrap();
    assert!(state(&c).iter().all(|r| r.1.is_none()), "no automatic link: {:?}", state(&c));   // also: names that normalise to nothing never match
    let s = suggestions(&c, &p3()).unwrap();
    assert_eq!(s.len(), 1);
    assert_eq!((s[0].appid, s[0].target), (2, 1));
    approve(&c, &[(2, 1)], &p3()).unwrap();
    assert_eq!(state(&c)[1], (2, Some("1".into()), 0));
    assert!(suggestions(&c, &p3()).unwrap().is_empty());
    // "not a duplicate" removes it for good
    let c = db(&rows_json(&[(1, "Hades", "steam"), (2, "Hades - Remastered", "gog")]), false);
    reject(&c, &[2]).unwrap();
    assert!(suggestions(&c, &p3()).unwrap().is_empty());
}

#[test]
fn same_platform_siblings_are_each_linked_and_unlisted_platforms_rank_last() {
    let c = db(&rows_json(&[(1, "Portal", "steam"), (2, "Portal", "gog"), (3, "portal", "gog"), (4, "Portal", "humble")]), false);
    examine_new(&c, &p3()).unwrap();
    assert_eq!(state(&c), vec![(1, None, 0), (2, Some("1".into()), 1), (3, Some("1".into()), 1), (4, Some("1".into()), 1)]);
}

#[test]
fn a_rename_never_links_and_drops_only_links_that_no_longer_match() {
    let c = db(&rows_json(&[(1, "Portal", "steam"), (2, "Portal", "gog"), (3, "Other", "epic_games")]), false);
    examine_new(&c, &p3()).unwrap();
    c.execute("UPDATE games SET name = 'Portal Reloaded' WHERE appid = 2", []).unwrap();
    assert_eq!(on_rename(&c, 2).unwrap(), 1);
    assert_eq!(state(&c)[1], (2, None, 0));
    // renaming game 3 to match does not link it
    c.execute("UPDATE games SET name = 'Portal' WHERE appid = 3", []).unwrap();
    on_rename(&c, 3).unwrap();
    examine_new(&c, &p3()).unwrap();
    assert_eq!(state(&c)[2], (3, None, 0));
    // a link made by hand survives a rename
    link_manual(&c, 2, 1, &p3()).unwrap();
    c.execute("UPDATE games SET name = 'Totally different' WHERE appid = 2", []).unwrap();
    assert_eq!(on_rename(&c, 2).unwrap(), 0);
    assert_eq!(state(&c)[1], (2, Some("1".into()), 0));
}

#[test]
fn deleting_the_shown_copy_promotes_the_next_one() {
    let c = db(&rows_json(&[(1, "Portal", "steam"), (2, "Portal", "gog"), (3, "Portal", "epic_games")]), false);
    examine_new(&c, &p3()).unwrap();
    c.execute("DELETE FROM games WHERE appid = 1", []).unwrap();
    on_delete(&c, 1, &p3()).unwrap();
    assert_eq!(state(&c), vec![(2, None, 0), (3, Some("2".into()), 1)]);
    c.execute("DELETE FROM games WHERE appid = 2", []).unwrap();
    on_delete(&c, 2, &p3()).unwrap();
    assert_eq!(state(&c), vec![(3, None, 0)]);
}

#[test]
fn a_priority_change_repoints_links_without_creating_any() {
    let c = db(&rows_json(&[(1, "Portal", "steam"), (2, "Portal", "gog"), (3, "Other", "epic_games")]), false);
    examine_new(&c, &p3()).unwrap();
    let flipped: Vec<String> = ["gog", "steam", "epic_games"].iter().map(|s| s.to_string()).collect();
    realign(&c, &flipped).unwrap();
    assert_eq!(state(&c), vec![(1, Some("2".into()), 1), (2, None, 0), (3, None, 0)]);
}

#[test]
fn the_column_is_added_once_and_existing_games_count_as_examined() {
    let c = Connection::open_in_memory().unwrap();
    c.execute("CREATE TABLE games (appid INTEGER PRIMARY KEY, name TEXT, platform TEXT, duplicate_of TEXT, duplicate_auto INTEGER DEFAULT 0)", []).unwrap();
    c.execute("INSERT INTO games (appid, name, platform) VALUES (1, 'A', 'steam'), (2, 'A', 'gog')", []).unwrap();
    assert!(ensure_column(&c).unwrap());
    assert!(!ensure_column(&c).unwrap());
    assert_eq!(examine_new(&c, &p3()).unwrap().examined, 0);   // Python's links (none here) are kept, nothing is guessed
    assert_eq!(hidden_count(&c).unwrap(), 0);
}

/// Real-library comparison (needs ZEST_TEST_DB = a COPY of a games db, ZEST_TEST_PRIO = comma separated platform order): clears the automatic links,
/// re-examines everything and lists how the result differs from what Python had linked. Run with `--ignored --nocapture`.
#[test]
#[ignore]
fn real_library_against_pythons_links() {
    let (Ok(src), Ok(prio)) = (std::env::var("ZEST_TEST_DB"), std::env::var("ZEST_TEST_PRIO")) else { return };
    let tmp = std::env::temp_dir().join(format!("zest-dupes-{}.db", std::process::id()));
    std::fs::copy(&src, &tmp).unwrap();
    let c = Connection::open(&tmp).unwrap();
    ensure_column(&c).unwrap();
    let prio: Vec<String> = prio.split(',').map(String::from).collect();
    let py: std::collections::HashMap<i64, (String, i64)> = c.prepare("SELECT appid, duplicate_of, COALESCE(duplicate_auto,0) FROM games WHERE duplicate_of IS NOT NULL AND duplicate_of != ''").unwrap()
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, (r.get::<_, String>(1)?, r.get::<_, i64>(2)?)))).unwrap().flatten().collect();
    c.execute("UPDATE games SET duplicate_of = NULL, duplicate_auto = 0 WHERE duplicate_auto = 1", []).unwrap();
    c.execute("UPDATE games SET dup_checked = NULL", []).unwrap();
    let d = examine_new(&c, &prio).unwrap();
    let mine: std::collections::HashMap<i64, String> = c.prepare("SELECT appid, duplicate_of FROM games WHERE duplicate_of IS NOT NULL AND duplicate_of != '' AND duplicate_auto = 1").unwrap()
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))).unwrap().flatten().collect();
    let py_auto: std::collections::HashMap<i64, String> = py.iter().filter(|(_, v)| v.1 == 1).map(|(k, v)| (*k, v.0.clone())).collect();
    let name = |id: i64| -> String { c.query_row("SELECT name || ' [' || platform || ']' FROM games WHERE appid = ?", [id], |r| r.get(0)).unwrap_or_default() };
    let same = mine.iter().filter(|(k, v)| py_auto.get(k) == Some(v)).count();
    let only_py: Vec<_> = py_auto.iter().filter(|(k, _)| !mine.contains_key(k)).collect();
    let only_me: Vec<_> = mine.iter().filter(|(k, _)| !py_auto.contains_key(k)).collect();
    let diff: Vec<_> = mine.iter().filter(|(k, v)| py_auto.get(k).map_or(false, |p| p != *v)).collect();
    println!("examined {} linked {} | python auto links {} | identical {} | only python {} | only zest {} | different target {}", d.examined, d.linked, py_auto.len(), same, only_py.len(), only_me.len(), diff.len());
    for (k, v) in only_py.iter().take(8) { println!("  only python: {} -> {}", name(**k), name(v.parse().unwrap())); }
    for (k, v) in only_me.iter().take(8) { println!("  only zest:   {} -> {}", name(**k), name(v.parse().unwrap())); }
    for (k, v) in diff.iter().take(8) { println!("  different:   {} python -> {}  zest -> {}", name(**k), name(py_auto[k].parse().unwrap()), name(v.parse().unwrap())); }
    let _ = std::fs::remove_file(&tmp);
}
