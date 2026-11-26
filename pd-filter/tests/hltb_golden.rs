//! HowLongToBeat's pure parts against the real Python (`gen_hltb_golden.py`): difflib similarity, the library's number rule, name cleaning, result parsing.
use pd_filter::hltb::*;
use serde_json::Value;

fn golden() -> Value { serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/hltb_golden.json")).expect("run gen_hltb_golden.py")).unwrap() }

#[test]
fn similarity_matches_pythons_difflib_exactly() {
    let g = golden();
    let mut bad = Vec::new();
    for c in g["sim"].as_array().unwrap() {
        let (a, b, cs) = (c["a"].as_str().unwrap(), c["b"].as_str().unwrap(), c["cs"].as_bool().unwrap());
        let ratio = if cs { sequence_ratio(a, b) } else { sequence_ratio(&a.to_lowercase(), &b.to_lowercase()) };
        let s = similar(Some(a), Some(b), &name_numbers(a), cs);
        if (ratio - c["ratio"].as_f64().unwrap()).abs() > 1e-12 || (s - c["sim"].as_f64().unwrap()).abs() > 1e-12 { if bad.len() < 6 { bad.push(format!("{a:?} vs {b:?} cs {cs}: rust {ratio} {s} python {} {}", c["ratio"], c["sim"])); } else { bad.push(String::new()); } }
    }
    assert!(bad.is_empty(), "{} of {} differ:\n{}", bad.len(), g["sim"].as_array().unwrap().len(), bad.iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join("\n"));
}

#[test]
fn name_helpers_and_minutes_match_python() {
    let g = golden();
    for c in g["names"].as_array().unwrap() {
        let n = c["name"].as_str().unwrap();
        assert_eq!(clean_name(n), c["clean"].as_str().unwrap(), "clean {n:?}");
        assert_eq!(strip_edition(&clean_name(n)), c["strip"].as_str().unwrap(), "strip {n:?}");
    }
    for c in g["mins"].as_array().unwrap() { assert_eq!(hours_to_minutes(c["h"].as_f64()), c["py"].as_i64(), "minutes {c}"); }
}

#[test]
fn a_search_answer_is_parsed_like_the_library_does() {
    let g = golden();
    let body = g["answer"].as_str().unwrap();
    for (key, (min, id, cs)) in [("0.0-None-False", (0.0, None, false)), ("0.4-None-True", (0.4, None, true)), ("0.0-11-True", (0.0, Some(11), true))] {
        let mine = parse_search(body, "Portal 2", cs, min, id).unwrap();
        let py = g["parsed"][key].as_array().unwrap();
        assert_eq!(mine.len(), py.len(), "{key}");
        for (m, p) in mine.iter().zip(py) {
            assert_eq!((m.game_id, m.game_name.as_str()), (p["id"].as_i64().unwrap(), p["name"].as_str().unwrap()), "{key}");
            assert_eq!((m.main_story, m.main_extra, m.completionist), (p["main"].as_f64(), p["extra"].as_f64(), p["comp"].as_f64()), "{key} times");
            assert!((m.similarity - p["sim"].as_f64().unwrap()).abs() < 1e-12, "{key} similarity");
        }
    }
    assert!(parse_search("not json", "x", true, 0.0, None).is_none());
}
