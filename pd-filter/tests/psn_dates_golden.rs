//! The PlayStation purchase-date matcher against the real `psn.apply_purchase_dates` (`gen_psn_dates_golden.py`).
use pd_filter::sources::playstation::*;
use serde_json::Value;

fn golden() -> Value { serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/psn_dates_golden.json")).expect("run gen_psn_dates_golden.py")).unwrap() }

#[test] fn skus_split_like_python() {
    for c in golden()["titles"].as_array().unwrap() {
        let sku = c[0].as_str().unwrap_or("");
        assert_eq!(title_id_of(sku).map_or(Value::Null, Value::from), c[1], "title id of {c}");
        assert_eq!(product_of(sku).map_or(Value::Null, Value::from), if c[0].is_null() { Value::Null } else { c[2].clone() }, "product of {c}");
    }
}
#[test] fn purchases_match_and_set_the_same_dates_as_python() {
    let g = golden();
    let rows: Vec<DateRow> = g["rows"].as_array().unwrap().iter().map(|r| (r[0].as_i64().unwrap(), r[1].as_str().unwrap().to_string(), r[2].as_str().unwrap_or("").to_string(), r[3].as_str().unwrap_or("").to_string(), r[4].as_i64())).collect();
    let purchases: Vec<PurchaseIn> = g["purchases"].as_array().unwrap().iter().map(|p| (p["date"].as_str().unwrap().to_string(), p["name"].as_str().unwrap().to_string(), p["sku"].as_str().unwrap().to_string())).collect();
    let (read, matched, set) = match_purchase_dates(&purchases, &rows);
    assert_eq!((read as i64, matched as i64, set.len() as i64), (g["result"]["purchases"].as_i64().unwrap(), g["result"]["matched"].as_i64().unwrap(), g["result"]["updated"].as_i64().unwrap()));
    // the dates the Python left in its database for the playstation rows
    let mut mine: std::collections::HashMap<i64, i64> = rows.iter().filter_map(|r| r.4.map(|d| (r.0, d))).collect();
    for (id, ts) in set { mine.insert(id, ts); }
    for (id, d) in g["dates"].as_object().unwrap() { let id: i64 = id.parse().unwrap(); if id == -99 { continue; } assert_eq!(mine.get(&id).copied(), d.as_i64().or(Some(0)).filter(|d| *d != 0 || mine.contains_key(&id)), "appid {id}"); }
}
