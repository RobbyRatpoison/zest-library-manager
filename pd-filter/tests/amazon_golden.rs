//! The Amazon Games source's pure parts against the real PlayDate plugin (`gen_amazon_golden.py` runs `plugins/amazon_games/amazon_games.py` with fixed randomness,
//! a fake `requests` and an in-memory database).
use pd_filter::sources::amazon_games::*;
use serde_json::{json, Value};
use std::collections::HashSet;

fn golden() -> Value { serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/amazon_golden.json")).expect("run gen_amazon_golden.py")).unwrap() }

#[test] fn the_sign_in_page_challenge_and_client_id_match_python() {
    let g = golden(); let a = &g["auth"];
    let bytes: Vec<u8> = a["random"].as_array().unwrap().iter().map(|b| b.as_u64().unwrap() as u8).collect();
    let (verifier, challenge) = pkce(&bytes);
    assert_eq!(verifier, a["verifier"].as_str().unwrap());
    let cid = client_id(a["serial"].as_str().unwrap());
    assert_eq!(cid, a["client_id"].as_str().unwrap());
    assert_eq!(auth_url(&cid, &challenge), a["url"].as_str().unwrap());
}
#[test] fn pasted_results_give_the_same_code_and_registration_body_as_python() {
    let g = golden(); let a = &g["auth"];
    for c in g["exchange"].as_array().unwrap() {
        let raw = c["raw"].as_str().unwrap();
        let mine = code_from(raw);
        if c["ok"] == true {
            let code = mine.unwrap_or_else(|| panic!("no code for {raw:?}"));
            assert_eq!(Some(code.as_str()), c["posted"]["auth_data"]["authorization_code"].as_str(), "{raw:?}");
            assert_eq!(register_body(&code, a["client_id"].as_str().unwrap(), a["verifier"].as_str().unwrap(), a["serial"].as_str().unwrap()), c["posted"], "{raw:?}");
        } else { assert!(mine.is_none(), "{raw:?}"); }
    }
}
#[test] fn entitlement_requests_match_python() {
    for c in golden()["entitlement_bodies"].as_array().unwrap() { assert_eq!(entitlements_body(c["next"].as_str(), c["serial"].as_str().unwrap()), c["body"]); }
}
fn row(r: &pd_filter::importer::GameRecord) -> Value { json!({"platform_id": r.platform_id, "name": r.name, "platform_appname": r.appname, "genres": r.genres, "installed": r.installed.map_or(0, |b| b as i64)}) }
#[test] fn api_and_nile_libraries_give_the_same_games_as_python() {
    let g = golden();
    for c in g["api"].as_array().unwrap() {
        let entries: Vec<Value> = c["pages"].as_array().unwrap().iter().flat_map(|p| p.as_array().unwrap().clone()).collect();
        let mut seen = HashSet::new();
        let mut mine: Vec<Value> = records_from_entitlements(&entries, &mut seen, 1).iter().map(row).collect();
        mine.reverse();   // the golden lists rows newest first
        assert_eq!(Value::Array(mine), c["rows"], "api {}", c["name"]);
    }
    for c in g["nile"].as_array().unwrap() {
        let installed = nile_installed_ids(&c["installed"]);
        let mut mine: Vec<Value> = records_from_nile(&c["library"], &installed, &mut HashSet::new(), 1).iter().map(row).collect();
        mine.reverse();
        assert_eq!(Value::Array(mine), c["rows"], "nile {}", c["name"]);
    }
}
