//! Replays the golden results of Python's parsing over recorded real responses (`capture_net.py` + `gen_net_golden.py`).
use pd_filter::steamweb::*;
use serde_json::Value;
use std::io::Read;

const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests");
fn golden() -> Value { serde_json::from_str(&std::fs::read_to_string(format!("{DIR}/net_golden.json")).expect("run capture_net.py then gen_net_golden.py")).unwrap() }
fn data(name: &str) -> Option<String> { std::fs::read(format!("{DIR}/net_data/{name}")).ok().map(|b| String::from_utf8_lossy(&b).into_owned()) }
fn store_html(a: &str) -> Option<String> {
    let f = std::fs::File::open(format!("{DIR}/net_data/{a}.store.html.gz")).ok()?;
    let mut s = String::new(); flate2::read::GzDecoder::new(f).read_to_string(&mut s).ok()?; Some(s)
}

#[test]
fn dates_clean_text_and_weighted_scores_match_python() {
    let g = golden();
    for c in g["dates"].as_array().unwrap() { assert_eq!(parse_steam_date(c["raw"].as_str().unwrap()), c["py"].as_i64(), "date {:?}", c["raw"]); }
    for c in g["clean"].as_array().unwrap() { assert_eq!(clean_description(c["raw"].as_str().unwrap()), c["py"].as_str().unwrap(), "clean {:?}", c["raw"]); }
    for c in g["weighted"].as_array().unwrap() { assert_eq!(weighted_review_score(c["percent"].as_f64().unwrap(), c["count"].as_i64().unwrap(), 10.0), c["py"].as_i64().unwrap(), "weighted {c}"); }
}

#[test]
fn recorded_store_pages_reviews_tags_and_protondb_match_python() {
    let g = golden();
    let (mut n_store, mut n_rev, mut n_tags, mut n_pdb, mut bad) = (0, 0, 0, 0, Vec::new());
    for (a, py) in g["games"].as_object().unwrap() {
        let appid: i64 = a.parse().unwrap();
        // store data + release date
        if let Some(body) = data(&format!("{a}.appdetails.json")).filter(|_| py.get("store").is_some()) {
            let mine = parse_appdetails(&body, appid);
            match (&mine, &py["store"]) {
                (None, Value::Null) => {}
                (Some(m), p) if !p.is_null() => {
                    n_store += 1;
                    let ai = &py["appinfo"];
                    let rel = release_date(ai["original_release_date"].as_i64(), ai["steam_release_date"].as_i64(), &m.api_date, || store_html(a).and_then(|h| parse_store_page_date(&h)));
                    let same = m.name == p["name"].as_str().unwrap() && m.kind == p["type"].as_str().unwrap() && m.developers == p["developers"].as_str().unwrap() && m.publishers == p["publishers"].as_str().unwrap()
                        && m.genres == p["genres"].as_str().unwrap() && m.categories == p["categories"].as_str().unwrap() && m.is_free == p["is_free"].as_i64().unwrap()
                        && m.metacritic_score == p["metacritic_score"].as_i64() && m.short_description.as_deref() == p["short_description"].as_str() && rel == p["release_date"].as_i64();
                    if !same { bad.push(format!("store {a}: rust {m:?} release {rel:?}\n   python {p}")); }
                }
                _ => bad.push(format!("store {a}: rust {mine:?} python {}", py["store"])),
            }
        }
        // reviews
        if let (Some(body), Some(p)) = (data(&format!("{a}.reviews.json")), py.get("reviews").filter(|p| !p.is_null())) {
            n_rev += 1;
            let m = parse_reviews(&body, 10.0).unwrap();
            if m.review_score != p["review_score"].as_str().unwrap() || m.review_percentage != p["review_percentage"].as_i64().unwrap() || m.weighted_percentage != p["weighted_percentage"].as_i64().unwrap()
                || m.total_reviews != p["total_reviews"].as_i64().unwrap() || m.positive_reviews != p["positive_reviews"].as_i64().unwrap() { bad.push(format!("reviews {a}: {m:?} vs {p}")); }
        }
        // tags and the store page date
        if let Some(html) = store_html(a).filter(|_| py.get("tags").is_some()) {
            let mine = parse_tags(&html).map(|t| t.join(","));
            let want = py["tags"].get("tags").and_then(|v| v.as_str()).map(String::from);
            if py["tags"].is_null() { if mine.is_some() { bad.push(format!("tags {a}: rust {mine:?} python none")); } } else { n_tags += 1; if mine != want { bad.push(format!("tags {a}: rust {mine:?}\n   python {want:?}")); } }
            if parse_store_page_date(&html) != py["store_date"].as_i64() { bad.push(format!("store date {a}: {:?} vs {}", parse_store_page_date(&html), py["store_date"])); }
        }
        // ProtonDB
        if let Some(body) = data(&format!("{a}.protondb.json")).filter(|b| b.trim_start().starts_with('{')) {
            let mine = parse_protondb(&body);
            let want = py["protondb"].as_object().map(|o| (o["protondb_tier"].as_str().unwrap().to_string(), o["protondb_confidence"].as_str().map(String::from)));
            n_pdb += 1;
            if mine != want { bad.push(format!("protondb {a}: {mine:?} vs {want:?}")); }
        }
    }
    println!("compared: store {n_store}, reviews {n_rev}, tags {n_tags}, protondb {n_pdb}");
    assert!(n_store > 10 && n_rev > 10 && n_tags > 10, "the capture should hold real games");
    assert!(bad.is_empty(), "{} differ:\n{}", bad.len(), bad.iter().take(6).cloned().collect::<Vec<_>>().join("\n"));
}

#[test]
fn recorded_key_endpoints_match_python() {
    let g = golden();
    for (a, c) in g["cheevos"].as_object().unwrap() {
        let mine = parse_cheevos(c["status"].as_u64().unwrap() as u16, &data(&format!("{a}.cheevos.json")).unwrap());
        let py = &c["py"];
        match (mine, py.is_null()) {
            (None, true) => {}
            (Some(m), false) => { assert_eq!((m.total, m.unlocked), (py["total_achievements"].as_i64().unwrap(), py["unlocked_achievements"].as_i64().unwrap()), "cheevos {a}"); assert_eq!(m.completed, py.get("completion_status").is_some(), "cheevos completed {a}"); }
            (m, _) => panic!("cheevos {a}: rust {m:?} python {py}"),
        }
    }
    for (a, py) in g["schema"].as_object().unwrap() { assert_eq!(parse_schema_total(&data(&format!("{a}.schema.json")).unwrap()), py.as_i64(), "schema {a}"); }
    if let Some(body) = data("owned.json") {
        let games = parse_owned_games(&body).unwrap();
        let mut ids: Vec<i64> = games.iter().map(|g| g.appid).collect(); ids.sort();
        let want: Vec<i64> = g["owned"]["appids"].as_array().unwrap().iter().map(|v| v.as_i64().unwrap()).collect();
        assert_eq!(ids, want);
    }
    assert_eq!(parse_owned_games("{\"response\":{}}").unwrap_err(), "No games returned. Is your Steam Profile set to Public?");
    assert_eq!(parse_owned_games("nope").unwrap_err(), "Steam sent invalid data. Try again in a few minutes.");
    assert_eq!(parse_cheevos(403, ""), None);
    assert_eq!(parse_cheevos(400, "").map(|c| c.total), Some(0));
}
