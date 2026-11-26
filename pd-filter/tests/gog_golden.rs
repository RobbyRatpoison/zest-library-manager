//! The GOG source's parsing against the real PlayDate plugin (`gen_gog_golden.py` runs `plugins/gog/gog.py`, `__init__.py` and `routes.py` with a fake `requests`).
use pd_filter::sources::gog::*;
use rusqlite::types::Value as Sql;
use serde_json::{json, Map, Value};

fn golden() -> Value { serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/gog_golden.json")).expect("run gen_gog_golden.py")).unwrap() }
fn sql(v: &Sql) -> Value { match v { Sql::Text(t) => json!(t), Sql::Integer(i) => json!(i), _ => Value::Null } }

#[test] fn metadata_and_ratings_match_python() {
    for c in golden()["metadata"].as_array().unwrap() {
        let (status, body) = (c["v2"][0].as_i64().unwrap(), &c["v2"][1]);
        let ratings = if c["ratings"].is_null() { None } else if c["ratings"][0] == 200 { Some(c["ratings"][1].clone()) } else { None };
        let ht = c["half_trust"].as_f64().unwrap();
        // what fetch_metadata does around metadata_cols: 404 = a product with no category, other failures = nothing
        let mine: Value = if status == 404 { json!({"_category": null}) } else if status >= 400 { Value::Null } else {
            let (cat, cols) = metadata_cols(body, ratings.as_ref(), ht);
            let mut m = Map::new(); m.insert("_category".into(), cat.map_or(Value::Null, Value::from));
            for (k, v) in cols { m.insert(k.into(), sql(&v)); } Value::Object(m)
        };
        assert_eq!(mine, c["out"], "v2 {status} {body} ratings {} ht {ht}", c["ratings"]);
    }
}
#[test] fn achievements_match_python() {
    for c in golden()["achievements"].as_array().unwrap() {
        let body = if c["body"].is_string() { None } else { Some(&c["body"]) };
        let mine = achievements_from(c["status"].as_u64().unwrap() as u16, body).map(|(u, t)| json!({"unlocked_achievements": u, "total_achievements": t}));
        assert_eq!(mine.unwrap_or(Value::Null), c["out"], "{c}");
    }
}
#[test] fn purchase_dates_match_python() {
    let g = golden();
    for c in g["purchase"].as_array().unwrap() {
        let pages = c["pages"].as_array().unwrap();
        if c["out"].is_null() { continue; }   // a page that cannot be fetched: the walk itself fails (covered by the behaviour tests)
        let (d, t) = purchase_dates(pages);
        assert_eq!(json!({"dates": d, "titles": t}), c["out"], "{c}");
    }
    assert!(g["purchase_failed"].is_null());
}
#[test] fn titles_packs_art_descriptions_codes_and_names_match_python() {
    let g = golden();
    for c in g["norm"].as_array().unwrap() { assert_eq!(norm_title(c["s"].as_str().unwrap_or("")), c["out"].as_str().unwrap(), "{c}"); }
    for c in g["packs"].as_array().unwrap() { let v = pack_contents(&c["body"]); assert_eq!(json!(if c["status"] == 200 { v } else { vec![] }), c["out"], "{c}"); }
    for c in g["arts"].as_array().unwrap() { let v = if c["status"] == 200 { art_url(&c["body"]) } else { None }; assert_eq!(v.map_or(Value::Null, Value::from), c["out"].as_object().and_then(|o| o.get("vertical")).cloned().unwrap_or(Value::Null), "{c}"); }
    for c in g["descriptions"].as_array().unwrap() { let v = if c["status"].as_u64().unwrap() < 400 { description_from(&c["body"]) } else { None }; assert_eq!(v.map_or(Value::Null, Value::from), c["out"], "{c}"); }
    for c in g["codes"].as_array().unwrap() {
        let mine = match parse_code(c["raw"].as_str().unwrap()) { Ok(v) => json!(["ok", v]), Err(e) => json!(["err", e]) };
        assert_eq!(mine, c["out"], "{c}");
    }
    for c in g["usernames"].as_array().unwrap() { let (u, id) = username_from(&c["body"]); assert_eq!(json!([u, id]), c["out"], "{c}"); }
    for c in g["non_game"].as_array().unwrap() { assert_eq!(is_non_game(c["category"].as_str(), c["name"].as_str().unwrap()), c["out"].as_bool().unwrap(), "{c}"); }
}
