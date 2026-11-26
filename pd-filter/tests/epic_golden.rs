//! The Epic source's parsing and rules against the real PlayDate plugin (`gen_epic_golden.py`).
use pd_filter::net::{Http, NetError, Response};
use pd_filter::sources::epic::*;
use rusqlite::types::Value as Sql;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

fn golden() -> Value { serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/epic_golden.json")).expect("run gen_epic_golden.py")).unwrap() }
fn opt(v: &Value) -> Option<&str> { v.as_str() }

#[test] fn codes_slugs_and_titles_match_python() {
    let g = golden();
    for c in g["auth"].as_array().unwrap() { assert_eq!(extract_auth_code(c["s"].as_str().unwrap()), c["out"].as_str().unwrap(), "auth code {}", c["s"]); }
    for c in g["clean"].as_array().unwrap() { assert_eq!(clean_slug(c["s"].as_str()), c["out"].as_str().unwrap(), "clean slug {}", c["s"]); }
    for c in g["store_slugs"].as_array().unwrap() { assert_eq!(store_page_slug(&c["el"]), c["out"].as_str().unwrap(), "store slug {}", c["el"]); }
    for c in g["titles"].as_array().unwrap() {
        let (a, b) = (c["a"].as_str().unwrap(), c["b"].as_str().unwrap());
        assert_eq!(title_key(a), c["key"].as_str().unwrap(), "key {a}"); assert_eq!(title_matches(a, b), c["out"].as_bool().unwrap(), "match {a} / {b}");
    }
}

#[test] fn the_dlc_rule_matches_python() { for c in golden()["dlc"].as_array().unwrap() { assert_eq!(is_dlc_or_extra(&c["entry"], c["name"].as_str().unwrap()), c["out"].as_bool().unwrap(), "{} / {}", c["entry"], c["name"]); } }

#[test] fn the_right_store_offer_is_picked() {
    for c in golden()["picks"].as_array().unwrap() {
        let els = c["els"].as_array().unwrap();
        let got = pick_store_element(els, "ns", opt(&c["name"])).map(|e| els.iter().position(|x| x == e).unwrap());
        assert_eq!(got.map(|i| i as i64), c["out"].as_i64(), "pick {} in {}", c["name"], c["els"]);
    }
}

fn col(u: &[(&'static str, Sql)], k: &str) -> Value { u.iter().find(|(c, _)| *c == k).map(|(_, v)| match v { Sql::Text(t) => json!(t), Sql::Integer(i) => json!(i), _ => Value::Null }).unwrap_or(Value::Null) }

#[test] fn catalog_metadata_and_store_merging_match_python() {
    let g = golden();
    for c in g["extract"].as_array().unwrap() {
        let m = extract_metadata(&c["entry"]); let o = &c["out"];
        assert_eq!((m.developers.as_deref(), m.publishers.as_deref(), m.genres.as_deref(), m.slug.as_deref()), (opt(&o["developers"]), opt(&o["publishers"]), opt(&o["genres"]), opt(&o["platform_slug"])), "extract {}", c["entry"]);
    }
    for c in g["merges"].as_array().unwrap() {
        let (meta, store) = (&c["meta"], &c["store"]);
        let m = CatalogMeta { developers: opt(&meta["developers"]).map(String::from), publishers: None, genres: None, slug: opt(&meta["platform_slug"]).map(String::from) };
        let s = StoreData { tags: opt(&store["tags"]).map(String::from), slug: opt(&store["platform_slug"]).map(String::from), description: None, release: store["_release_date"].as_i64() };
        let u = merge_store(&m, &s, c["current"].as_i64());
        for k in ["developers", "tags", "platform_slug", "release_date"] { assert_eq!(col(&u, k), c["out"].get(k).cloned().unwrap_or(Value::Null), "merge {k} for {c}"); }
    }
}

#[test] fn release_dates_labels_ratings_and_entitlements_match_python() {
    let g = golden();
    let now = 1_762_000_000;   // the placeholders in the cases are 2099 and the rest are in the past, so any recent clock agrees with Python's
    for c in g["releases"].as_array().unwrap() { assert_eq!(parse_pc_release_at(opt(&c["s"]), now), c["out"].as_i64(), "release {}", c["s"]); }
    for c in g["labels"].as_array().unwrap() { assert_eq!(review_score_label(c["p"].as_i64().unwrap(), c["t"].as_i64().unwrap()), c["out"].as_str().unwrap(), "label {c}"); }
    for c in g["ratings"].as_array().unwrap() {
        let mine = ratings_columns(&c["in"]);
        match (&mine, c["out"].as_object().filter(|o| !o.is_empty())) {
            (None, None) => {}
            (Some(u), Some(o)) => for k in ["review_percentage", "review_score", "total_reviews", "weighted_percentage"] { assert_eq!(col(u, k), o[k], "ratings {k} for {}", c["in"]); },
            _ => panic!("ratings disagree for {}", c["in"]),
        }
    }
    let (ns, cid) = entitlement_date_maps(g["ents"].as_array().unwrap());
    let to = |v: &Value| -> HashMap<String, i64> { v.as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_i64().unwrap())).collect() };
    assert_eq!(ns, to(&g["ent_ns"])); assert_eq!(cid, to(&g["ent_cid"]));
}

#[test] fn art_comes_from_the_catalogs_key_images() {
    let g = golden(); let a = g["art"].as_array().unwrap();
    assert_eq!(art_urls(&a[0]), vec![("vertical", "v1".to_string()), ("horizontal", "h1".to_string())]);
    assert!(art_urls(&a[1]).is_empty() && art_urls(&a[2]).is_empty());
}

/// A fake Epic answering the store GraphQL as the generator's fake did, so `store_data` can be compared end to end.
struct Gql { ns_els: Value, kw_els: Value }
impl Http for Gql {
    fn get(&self, _u: &str, _h: &[(&str, &str)], _t: Duration) -> Result<Response, NetError> { Err(NetError("not used".into())) }
    fn post(&self, _u: &str, _h: &[(&str, &str)], body: &str, _t: Duration) -> Result<Response, NetError> {
        let q = serde_json::from_str::<Value>(body).unwrap()["query"].as_str().unwrap().to_string();
        let b = if q.contains("catalogTags") { json!({"data": {"Catalog": {"tags": {"elements": [{"id": "t1", "name": "Co-op"}, {"id": "t2", "name": "Action"}]}}}}) }
                else if q.contains("keywordStoreQuery") { json!({"data": {"Catalog": {"searchStore": {"elements": self.kw_els}}}}) } else { json!({"data": {"Catalog": {"searchStore": {"elements": self.ns_els}}}}) };
        Ok(Response { status: 200, retry_after: None, body: b.to_string() })
    }
}

#[test] fn store_data_end_to_end_matches_python() {
    for c in golden()["store"].as_array().unwrap() {
        let h: Arc<dyn Http> = Arc::new(Gql { ns_els: c["ns_els"].clone(), kw_els: c["kw_els"].clone() });
        let s = EpicSource::new(h, std::env::temp_dir(), std::env::temp_dir().join("none.db"));
        let d = s.store_data("ns1", opt(&c["name"]));
        let o = &c["out"];
        assert_eq!((d.tags.as_deref(), d.slug.as_deref(), d.description.as_deref(), d.release), (opt(&o["tags"]), opt(&o["platform_slug"]), opt(&o["_description"]), o["_release_date"].as_i64()), "store data {c}");
    }
}
