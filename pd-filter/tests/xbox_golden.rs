//! The Xbox source's pure parts against the real PlayDate plugin (`gen_xbox_golden.py` runs `plugins/xbox/xbox_live.py`'s own sync on an in-memory database with a fake title history).
use pd_filter::sources::xbox::*;
use rusqlite::types::Value as Sql;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

fn golden() -> Value { serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/xbox_golden.json")).expect("run gen_xbox_golden.py")).unwrap() }
fn ts(o: Option<i64>) -> Value { o.map_or(Value::Null, |t| json!(t)) }

#[test] fn the_sign_in_address_and_the_dates_match_python() {
    let g = golden();
    assert_eq!(auth_url(), g["auth_url"].as_str().unwrap());
    for c in g["not_after"].as_array().unwrap() { if c[0].is_null() { continue; } assert_eq!(parse_not_after(c[0].as_str().unwrap()), c[1].as_i64().unwrap(), "{c}"); }
    for c in g["iso"].as_array().unwrap() { if c[0].is_null() { continue; } assert_eq!(ts(parse_iso_ts(c[0].as_str().unwrap())), c[1], "{c}"); }
}
#[test] fn metadata_images_and_playtimes_match_python() {
    let g = golden();
    for c in g["meta"].as_array().unwrap() {
        let mut mine = Map::new();
        for (k, v) in build_metadata(&c["title"], "TODAY") { mine.insert(k.into(), match v { Sql::Text(t) => json!(t), Sql::Integer(i) => json!(i), _ => Value::Null }); }
        let mut theirs = c["out"].as_object().unwrap().clone();
        if theirs.contains_key("cheevos_fetched") { theirs.insert("cheevos_fetched".into(), json!("TODAY")); }
        assert_eq!(Value::Object(mine), Value::Object(theirs), "{}", c["title"]);
    }
    for c in g["images"].as_array().unwrap() { for k in ["vertical", "horizontal", "icon"] { assert_eq!(pick_image(&json!({"images": c["images"]}), k).map_or(Value::Null, |u| json!(u)), c["out"][k], "{k} {c}"); } }
    // a stat Microsoft answers for a title we asked about is read the same way; Python's own answer is per requested id, so only the shapes are compared
    for c in g["stats"].as_array().unwrap() { let mine = playtimes_from(&c["in"]); assert_eq!(mine.get("1").copied().map(|m| json!(m)).unwrap_or(Value::Null), c["out"].get("1").cloned().unwrap_or(Value::Null), "{c}"); }
}
#[test] fn the_library_the_sync_builds_matches_python_with_and_without_a_minimum() {
    for c in golden()["sync"].as_array().unwrap() {
        let playtimes: HashMap<String, i64> = c["playtimes"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())).unwrap())).collect();
        let existing: HashSet<String> = c["existing"].as_array().unwrap().iter().map(|e| e.as_str().unwrap().to_string()).collect();
        let titles: Vec<Value> = c["titles"].as_array().unwrap().clone();
        let mine: Vec<Value> = records_from(&titles, &playtimes, &existing, c["min_play"].as_i64().unwrap(), 1).into_iter().filter(|r| !existing.contains(&r.platform_id))
            .map(|r| json!({"platform_id": r.platform_id, "name": r.name, "platform_appname": r.appname, "platform_slug": r.slug, "last_played": r.last_played, "playtime_forever": r.playtime_forever.unwrap_or(0),
                "developers": r.developers, "publishers": r.publishers, "genres": r.genres, "release_date": r.release_date})).collect();
        let theirs: Vec<Value> = c["rows"].as_array().unwrap().iter().map(|r| { let mut o = Map::new();
            for k in ["platform_id", "name", "platform_appname", "last_played", "playtime_forever", "developers", "publishers", "genres", "release_date"] { o.insert(k.into(), r[k].clone()); }
            o.insert("platform_slug".into(), if r["platform_slug"] == json!("") { Value::Null } else { r["platform_slug"].clone() }); Value::Object(o) }).collect();
        assert_eq!(mine, theirs, "min {} existing {}", c["min_play"], c["existing"]);
    }
}
