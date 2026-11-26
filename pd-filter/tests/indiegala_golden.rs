//! The IndieGala source's page parsing against the real PlayDate plugin (`gen_indiegala_golden.py` runs `plugins/indiegala/indiegala.py` with stubbed app modules).
use pd_filter::sources::indiegala::*;
use serde_json::{json, Value};

fn golden() -> Value { serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/indiegala_golden.json")).expect("run gen_indiegala_golden.py")).unwrap() }

#[test] fn library_pages_parse_like_python() {
    for c in golden()["pages"].as_array().unwrap() {
        let mine: Vec<Value> = parse_page(c["html"].as_str().unwrap()).into_iter().map(|g| json!({"prod_id": g.prod_id, "prod_name": g.prod_name, "date_str": g.date_str, "img_url": g.img_url, "showcase_url": g.showcase_url, "store_url": g.store_url})).collect();
        assert_eq!(Value::Array(mine), c["out"], "page {}", c["name"]);
    }
}
#[test] fn listing_dates_parse_like_python() {
    for c in golden()["dates"].as_array().unwrap() { if c[0].is_null() { continue; } assert_eq!(parse_dt(c[0].as_str().unwrap()).map_or(Value::Null, |t| json!(t)), c[1], "date {}", c[0]); }
}

#[test] fn folder_matching_and_names_match_python() {
    let g = golden();
    for c in g["names"].as_array().unwrap() { assert_eq!(normalize_name(c[0].as_str().unwrap()), c[1].as_str().unwrap(), "{c}"); }
    let cands: Vec<(i64, String)> = vec![(1, "Game One".into()), (2, "Game Two".into()), (3, "The Long Dark".into()), (4, "Long Dark Deluxe".into()), (5, "Gamé Öne".into()), (6, "".into())];
    for c in g["best"].as_array().unwrap() { assert_eq!(find_best_match(c[0].as_str().unwrap(), &cands).map_or(Value::Null, |i| json!(i)), if c[1].is_null() { Value::Null } else { c[1][0].clone() }, "{c}"); }
    for c in g["safe"].as_array().unwrap() { assert_eq!(safe_dirname(c[0].as_str().unwrap()), c[1].as_str().unwrap(), "{c}"); }
}
