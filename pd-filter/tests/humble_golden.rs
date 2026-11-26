//! The Humble source's filters, date parsing and sign-in against the real PlayDate plugin (`gen_humble_golden.py`).
use pd_filter::net::{Http, NetError, Raw, Response};
use pd_filter::sources::humble::*;
use pd_filter::sources::LibrarySource;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

fn golden() -> Value { serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/humble_golden.json")).expect("run gen_humble_golden.py")).unwrap() }

#[test] fn the_game_filter_matches_python() {
    let g = golden();
    for c in g["is_game"].as_array().unwrap() { assert_eq!(is_game(&c["sub"], if c["order"].is_null() { None } else { Some(&c["order"]) }), c["out"].as_bool().unwrap(), "is_game {}", c["sub"]); }
    for c in g["looks"].as_array().unwrap() { assert_eq!(looks_non_game(c["n"].as_str()), c["out"].as_bool().unwrap(), "looks_non_game {}", c["n"]); }
    for c in g["payees"].as_array().unwrap() { assert_eq!(payee_dev(&c["sub"]), c["out"].as_str().unwrap(), "payee {}", c["sub"]); }
}

#[test] fn dates_match_python() { for c in golden()["parse"].as_array().unwrap() { assert_eq!(parse_dt(c["s"].as_str()), c["out"].as_i64(), "parse_dt {}", c["s"]); } }

/// A Humble that answers the order list with a status and the library page with a status and body (or a failure).
struct Site { order: u16, page: Option<(u16, String)> }
impl Http for Site {
    fn get(&self, _u: &str, _h: &[(&str, &str)], _t: Duration) -> Result<Response, NetError> { Err(NetError("not used".into())) }
    fn get_raw(&self, u: &str, _h: &[(&str, &str)], _t: Duration) -> Result<Raw, NetError> {
        if u.ends_with("/user/order") { return Ok(Raw { status: self.order, headers: vec![], body: "[]".into() }); }
        match &self.page { Some((s, b)) => Ok(Raw { status: *s, headers: vec![], body: b.clone() }), None => Err(NetError("Io".into())) }
    }
}

#[test] fn signing_in_matches_python() {
    for c in golden()["connects"].as_array().unwrap() {
        let d = std::env::temp_dir().join(format!("zest_hg_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); std::fs::write(d.join("config.json"), "{}").unwrap();
        let page = c["page"].as_array().map(|p| (p[0].as_u64().unwrap() as u16, p[1].as_str().unwrap().to_string()));
        let s = HumbleSource::new(Arc::new(Site { order: c["order_status"].as_u64().unwrap() as u16, page }), d.clone(), d.join("g.db"));
        let r = s.auth_complete(c["raw"].as_str().unwrap());
        // Humble's rebuilt site (Nov 2026) signs in through Ory: a pasted old-site cookie is refused on purpose, with a pointer to the sign-in window, where the Python accepted it
        if !c["raw"].as_str().unwrap().trim().starts_with("ory_session") {
            assert!(matches!(&r, Err(m) if m.contains("use SIGN IN")), "an old-site cookie must be refused now: {r:?}");
            continue;
        }
        match (r, c["ok"].as_bool().unwrap()) {
            (Ok(st), true) => {
                assert_eq!(st.username.as_deref(), c["msg"].as_str(), "{c}");
                assert_eq!(pd_filter::account::plugin_secret_get(&d, "humble", "session_cookie").as_deref(), c["cookie"].as_str(), "{c}");
                assert_eq!(pd_filter::account::plugin_secret_get(&d, "humble", "username").as_deref(), c["username"].as_str(), "{c}");
            }
            (Err(m), false) => { assert_eq!(m, c["msg"].as_str().unwrap(), "{c}"); assert!(pd_filter::account::plugin_secret_get(&d, "humble", "session_cookie").is_none()); }
            (r, ok) => panic!("verdict differs (python ok={ok}, mine {r:?}): {c}"),
        }
    }
}
