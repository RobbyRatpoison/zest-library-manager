//! The Nintendo source's pure parts against the real PlayDate plugin (`gen_nintendo_golden.py`).
use pd_filter::sources::nintendo::*;
use rusqlite::types::Value as Sql;
use serde_json::{json, Value};
use std::collections::HashMap;

fn golden() -> Value { serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/nintendo_golden.json")).expect("run gen_nintendo_golden.py")).unwrap() }

#[test] fn names_and_player_counts_match_python() {
    let g = golden();
    for c in g["names"].as_array().unwrap() { let n = c["n"].as_str().unwrap(); assert_eq!(clean_name(n), c["clean"].as_str().unwrap(), "clean {n:?}"); assert_eq!(store_norm(n), c["norm"].as_str().unwrap(), "norm {n:?}"); }
    for c in g["counts"].as_array().unwrap() { assert_eq!(categories(c["c"].as_str().unwrap()), c["out"].as_str().unwrap(), "categories {}", c["c"]); }
}

#[test] fn store_entries_become_the_same_columns() {
    let g = golden();
    let vocab: HashMap<String, String> = g["vocab"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string())).collect();
    for c in g["meta"].as_array().unwrap() {
        let mine: Vec<(String, Value)> = metadata_from_entry(&c["h"], &vocab).into_iter().map(|(k, v)| (k.to_string(), match v { Sql::Text(t) => json!(t), Sql::Integer(i) => json!(i), _ => Value::Null })).collect();
        let theirs: Vec<(String, Value)> = c["out"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        assert_eq!(mine, theirs, "entry {}", c["h"]);
    }
    for c in g["desc"].as_array().unwrap() { assert_eq!(description_from_entry(&c["h"]), c["out"].as_str().map(String::from), "{}", c["h"]); }
}

#[test] fn cookie_handling_matches_python() {
    for c in golden()["cookies"].as_array().unwrap() {
        let raw = c["raw"].as_str().unwrap();
        match normalise_cookie(raw) {
            Ok(ck) => { assert!(c["ok"].as_bool().unwrap(), "{raw:?}"); assert_eq!(Some(ck.as_str()), c["cookie"].as_str(), "{raw:?}"); }
            Err(m) => { assert!(!c["ok"].as_bool().unwrap(), "{raw:?}"); assert_eq!(m, c["msg"].as_str().unwrap(), "{raw:?}"); }
        }
    }
}

#[test] fn the_account_page_is_read_the_same_way() {
    let g = golden();
    for c in g["pages"].as_array().unwrap() {
        let mine = parse_page(c["url"].as_str().unwrap(), c["status"].as_u64().unwrap() as u16, c["body"].as_str().unwrap());
        match (&mine, c["out"].as_object()) {
            (Some(p), Some(o)) => {
                assert_eq!((p.id_token.as_str(), p.client_id.as_str(), p.graphql.as_str(), p.country.as_str(), p.lang.as_str(), p.nickname.as_str()),
                    (o["id_token"].as_str().unwrap(), o["client_id"].as_str().unwrap(), o["graphql"].as_str().unwrap(), o["country"].as_str().unwrap(), o["lang"].as_str().unwrap(), o["nickname"].as_str().unwrap()), "{}", c["label"]);
            }
            (None, None) => {}
            _ => panic!("{}: signed-in verdict differs (mine: {})", c["label"], mine.is_some()),
        }
    }
    for c in g["embedded"].as_array().unwrap() { assert_eq!(embedded(c["text"].as_str().unwrap(), c["id"].as_str().unwrap()), if c["out"].is_null() { None } else { Some(c["out"].clone()) }, "{}", c["text"]); }
}
