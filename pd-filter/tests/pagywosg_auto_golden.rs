//! Replays `/api/pagywosg-auto` golden cases (real Python route, real events, throwaway database) through `build_auto`.
use pd_filter::pagywosg_event::*;
use rusqlite::Connection;
use serde_json::Value;

fn names(v: &Value) -> Names { let mut n = Names::new(); for (k, x) in v.as_object().unwrap() { n.set(k.parse().unwrap(), x.as_str().unwrap().into()); } n }

#[test]
fn auto_matches_the_python_route_on_real_events() {
    let g: Value = serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/pagywosg_auto_golden.json")).expect("run gen_pagywosg_auto_golden.py first")).unwrap();
    let cols: Vec<&str> = g["cols"].as_array().unwrap().iter().map(|c| c.as_str().unwrap()).collect();
    for case in g["cases"].as_array().unwrap() {
        let db = Connection::open_in_memory().unwrap();
        db.execute(&format!("CREATE TABLE games ({})", cols.join(", ")), []).unwrap();
        for r in case["db"].as_array().unwrap() {
            let vals: Vec<rusqlite::types::Value> = r.as_array().unwrap().iter().map(|v| match v { Value::Null => rusqlite::types::Value::Null, Value::Number(n) if n.is_i64() => rusqlite::types::Value::Integer(n.as_i64().unwrap()), Value::String(s) => rusqlite::types::Value::Text(s.clone()), o => panic!("{o}") }).collect();
            db.execute(&format!("INSERT INTO games VALUES ({})", vec!["?"; cols.len()].join(",")), rusqlite::params_from_iter(vals)).unwrap();
        }
        let ev = Event::from_json(&case["event"]);
        let supp = Supplements { icaio_giveaways: names(&case["ga"]), icaio_wishlist: names(&case["wl"]), santa: names(&case["santa"]) };
        let personal: Vec<String> = case["personal"].as_array().unwrap().iter().map(|p| p.as_str().unwrap().into()).collect();
        let got = build_auto(&ev, &supp, &personal, &db, case["sg_user"].as_str().unwrap()).unwrap().to_json();
        let py = &case["py"];
        assert_eq!(got["event"]["name"], py["event"]["name"]); // the route labels the event with today's id; the id formula has its own test
        for k in ["tags", "conds", "skipped", "personal_candidates", "default_personal"] { assert_eq!(&got[k], &py[k], "event {} key {k}", ev.id); }
        for pool in ["wins", "all"] {
            for k in ["appids", "appid_sources"] { assert_eq!(&got[pool][k], &py[pool][k], "event {} {pool}.{k}", ev.id); }
            let (a, b) = (got[pool]["games"].as_array().unwrap(), py[pool]["games"].as_array().unwrap());
            assert_eq!(a.len(), b.len(), "event {} {pool} game count", ev.id);
            for (x, y) in a.iter().zip(b) { assert_eq!(x, y, "event {} {pool} game", ev.id); }
        }
    }
}
