//! The matching and infobox parsing of `metadata.rs` against the real `metadata.py` (`gen_metadata_golden.py`; the wiki pages are recorded from PCGamingWiki).
use pd_filter::metadata::*;
use serde_json::Value;

fn golden() -> Value { serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/metadata_golden.json")).expect("run gen_metadata_golden.py")).unwrap() }

#[test]
fn names_queries_and_variants_match_python() {
    for c in golden()["names"].as_array().unwrap() {
        let n = c["name"].as_str().unwrap();
        assert_eq!(clean_query(n), c["clean"].as_str().unwrap(), "clean {n:?}");
        assert_eq!(norm(n), c["norm"].as_str().unwrap(), "norm {n:?}");
        let q: Vec<String> = c["queries"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
        assert_eq!(search_queries(n), q, "queries {n:?}");
        let v: Vec<(String, &str)> = c["variants"].as_array().unwrap().iter().map(|v| (v[0].as_str().unwrap().to_string(), v[1].as_str().unwrap())).collect();
        let mine: Vec<(String, &str)> = search_variants(n).into_iter().map(|(t, k)| (t, if k == VariantKind::Subtitle { "subtitle" } else { "edition" })).collect();
        assert_eq!(mine, v, "variants {n:?}");
    }
}

#[test]
fn similarity_matches_python() {
    let g = golden(); let mut bad = vec![];
    for c in g["sim"].as_array().unwrap() { let (a, b) = (c["a"].as_str().unwrap(), c["b"].as_str().unwrap()); if (similar(a, b) - c["sim"].as_f64().unwrap()).abs() > 1e-12 { bad.push(format!("{a:?} {b:?}")); } }
    assert!(bad.is_empty(), "{bad:?}");
}

#[test]
fn dates_match_python() {
    for c in golden()["dates"].as_array().unwrap() { assert_eq!(parse_date(c["s"].as_str().unwrap()), c["ts"].as_i64(), "date {:?}", c["s"]); }
}

#[test]
fn infoboxes_of_real_and_synthetic_pages_match_python() {
    let g = golden(); let pages = g["pages"].as_object().unwrap();
    for (t, want) in g["boxes"].as_object().unwrap() {
        let mut title = t.clone(); let mut wt = pages[t].as_str().unwrap().to_string();
        if let Some(r) = redirect_target(&wt) { title = r.clone(); wt = pages.get(&r).and_then(|v| v.as_str()).unwrap_or("").to_string(); }
        let b = infobox_from_wikitext(&title, &wt);
        let s = |k: &str| want.get(k).and_then(|v| v.as_str()).map(String::from);
        assert_eq!(b.title, want["_title"].as_str().unwrap(), "{t} title");
        assert_eq!(b.steam_appid, want.get("steam_appid").and_then(|v| v.as_i64()), "{t} appid");
        assert_eq!(b.gog_id, s("gog_id"), "{t} gog");
        assert_eq!((b.developers, b.publishers, b.genres, b.categories), (s("developers"), s("publishers"), s("genres"), s("categories")), "{t} text fields");
        assert_eq!(b.release_date, want.get("release_date").and_then(|v| v.as_i64()), "{t} date");
    }
}

#[test]
fn judge_matches_python() {
    for c in golden()["judge"].as_array().unwrap() {
        let cands: Vec<(i64, String)> = c["cands"].as_array().unwrap().iter().map(|x| (x["id"].as_i64().unwrap(), x["name"].as_str().unwrap().to_string())).collect();
        let b = c["box"].as_object().map(|_| PcgwBox { developers: Some("x".into()), ..Default::default() });
        let r = judge_candidates(c["name"].as_str().unwrap(), &cands, b.as_ref(), c["down"].as_bool().unwrap(), c["base"].as_str());
        let want = c["res"].as_array().map(|a| (a[0].as_i64(), a[1].as_str().unwrap().to_string()));
        assert_eq!(r.map(|(i, k)| (i, k.as_str().to_string())), want, "judge {c}");
    }
}
