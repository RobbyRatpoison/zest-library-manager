//! The itch.io source's parsing against the real PlayDate plugin (`gen_itch_golden.py` runs `plugins/itch_io/itch_io.py` with stubbed app modules and a fake session).
use pd_filter::sources::itch_io::*;
use serde_json::{json, Value};

fn golden() -> Value { serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/itch_golden.json")).expect("run gen_itch_golden.py")).unwrap() }
fn ts(o: Option<i64>) -> Value { o.map_or(Value::Null, |t| json!(t)) }

#[test] fn api_and_page_dates_match_python() {
    let g = golden();
    for c in g["dates"].as_array().unwrap() { let d = c[0].as_str().unwrap_or(""); if c[0].is_null() { continue; } assert_eq!(ts(parse_date(d)), c[1], "date {d:?}"); }
    for c in g["page_dates"].as_array().unwrap() { let d = c[0].as_str().unwrap(); assert_eq!(ts(parse_page_date(d)), c[1], "page date {d:?}"); }
}
#[test] fn tag_keys_and_the_tag_allowlist_match_python() {
    let g = golden();
    for c in g["tag_keys"].as_array().unwrap() { assert_eq!(tag_key(c[0].as_str().unwrap()), c[1].as_str().unwrap(), "{c}"); }
    for c in g["allow"].as_array().unwrap() {
        let lists: Vec<&str> = c["lists"].as_array().unwrap().iter().map(|l| l.as_str().unwrap()).collect();
        let mine: serde_json::Map<String, Value> = allowlist(lists).into_iter().map(|(k, v)| (k, json!(v))).collect();
        assert_eq!(Value::Object(mine), c["out"], "{c}");
    }
}
#[test] fn game_pages_give_the_same_genres_tags_and_release_date_as_python() {
    for c in golden()["pages"].as_array().unwrap() {
        let lists: Vec<&str> = c["lists"].as_array().unwrap().iter().map(|l| l.as_str().unwrap()).collect();
        let m = page_meta(c["html"].as_str().unwrap(), &allowlist(lists));
        let mut o = serde_json::Map::new();
        if let Some(g) = m.genres { o.insert("genres".into(), json!(g)); }
        if let Some(t) = m.tags { o.insert("tags".into(), json!(t)); }
        if let Some(r) = m.release_date { o.insert("release_date".into(), json!(r)); }
        assert_eq!(Value::Object(o), c["out"], "page {}", c["name"]);
    }
}
