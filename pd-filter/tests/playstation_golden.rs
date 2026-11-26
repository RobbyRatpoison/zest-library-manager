//! The PlayStation source's parsing, merging, store-page reading and demo/DLC verdicts against the real PlayDate plugin (`gen_playstation_golden.py`).
use pd_filter::sources::playstation::*;
use rusqlite::types::Value as Sql;
use serde_json::{json, Value};

fn golden() -> Value { serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/playstation_golden.json")).expect("run gen_playstation_golden.py")).unwrap() }

#[test] fn npsso_durations_and_times_match_python() {
    let g = golden();
    for c in g["npsso"].as_array().unwrap() { let r = c["raw"].as_str().unwrap(); assert_eq!(parse_npsso(r).as_deref(), c["out"].as_str(), "npsso {r:?}"); assert_eq!(looks_like_address(r), c["addr"].as_bool().unwrap(), "address {r:?}"); }
    for c in g["durations"].as_array().unwrap() { assert_eq!(parse_duration(c["s"].as_str().unwrap_or("")), c["out"].as_i64().unwrap(), "duration {}", c["s"]); }
    for c in g["times"].as_array().unwrap() { assert_eq!(parse_time(c["s"].as_str().unwrap_or("")), c["out"].as_i64(), "time {}", c["s"]); }
    for c in g["slugs"].as_array().unwrap() {
        let s = |k: &str| match &c[k] { Value::Null => None, Value::String(t) => Some(t.clone()), v if v.as_i64() == Some(0) => None, v => Some(v.to_string()) };
        assert_eq!(slug_for(s("c").as_deref(), s("p").as_deref()), c["out"].as_str().unwrap(), "slug {c}");
    }
}

#[test] fn merging_the_two_lists_matches_python() {
    for c in golden()["merges"].as_array().unwrap() {
        let (p, q) = (c["purchased"].as_array().unwrap(), c["played"].as_array().unwrap());
        let mine: Vec<Value> = merge(p, q).into_iter().map(|(k, e)| json!([k, {"name": e.name, "concept_id": e.concept_id, "title_id": e.title_id, "platform": e.platform, "playtime": e.playtime, "last_played": e.last_played, "plus": e.plus, "product_id": e.product_id}])).collect();
        let theirs: Vec<Value> = c["out"].as_array().unwrap().iter().map(|kv| {
            let (k, e) = (&kv[0], &kv[1]);
            let mut o = json!({"name": e["name"], "concept_id": e["concept_id"], "title_id": e["title_id"], "platform": e["platform"], "playtime": e["playtime"], "last_played": e["last_played"], "plus": e["plus"], "product_id": e.get("product_id").cloned().unwrap_or(Value::Null)});
            if o["concept_id"].is_null() { o["concept_id"] = Value::Null }
            json!([k, o])
        }).collect();
        assert_eq!(mine, theirs, "merge of {} purchased and {} played", p.len(), q.len());
    }
}

#[test] fn store_pages_are_read_the_same_way() {
    let g = golden();
    for (k, html) in g["pages"].as_object().unwrap() {
        let mine = parse_page(html.as_str().unwrap());
        let theirs = if g["parsed"][k].is_null() { None } else { Some(g["parsed"][k].clone()) };
        assert_eq!(mine, theirs, "page {k}");
    }
}

#[test] fn the_demo_and_dlc_verdicts_match_python() {
    for c in golden()["classify"].as_array().unwrap() {
        let rec = if c["page"] == "none" { None } else { Some(&golden_rec(&c["page"])) };
        let (ok, why) = classify(c["name"].as_str().unwrap(), rec, c["pid"].as_str(), c["allow"].as_bool().unwrap());
        assert_eq!(json!([ok, why]), c["out"], "classify {c}");
    }
}
fn golden_rec(page: &Value) -> Value { golden()["parsed"][page.as_str().unwrap()].clone() }

#[test] fn metadata_description_and_art_match_python() {
    let g = golden();
    for c in g["meta"].as_array().unwrap() {
        let rec = g["parsed"][c["page"].as_str().unwrap()].clone();
        let rec_opt = if rec.is_null() { None } else { Some(&rec) };
        let pid = c["pid"].as_str();
        let mine: Vec<(String, Value)> = metadata(rec_opt, pid).into_iter().map(|(k, v)| (k.to_string(), match v { Sql::Text(t) => json!(t), Sql::Integer(i) => json!(i), _ => Value::Null })).collect();
        let theirs: Vec<(String, Value)> = c["meta"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        assert_eq!(mine, theirs, "metadata {c}");
        assert_eq!(description(rec_opt, pid), c["desc"].as_str().map(String::from), "description {}", c["page"]);
        let art: std::collections::BTreeMap<String, String> = art_urls(rec_opt, pid).into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        let theirs: std::collections::BTreeMap<String, String> = c["art"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string())).collect();
        assert_eq!(art, theirs, "art {} {:?}", c["page"], pid);
        assert_eq!(pick_product(rec_opt, pid).and_then(|p| p.get("name")).and_then(|n| n.as_str()), c["pick"].as_str(), "pick {} {:?}", c["page"], pid);
    }
}
