//! Replays the SteamGifts golden cases (real Python on the test fixtures): the page parsers, merge and pass-2 planning, and `apply_wins` on four library states.
use pd_filter::blaeo::GroupSources;
use pd_filter::net::{Http, NetError, Response};
use pd_filter::steamgifts::*;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::time::Duration;

fn golden() -> Value { serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/steamgifts_golden.json")).expect("run gen_steamgifts_golden.py first")).unwrap() }
fn fx(n: &str) -> String { std::fs::read_to_string(format!("{}/tests/fixtures/steamgifts/{n}", env!("CARGO_MANIFEST_DIR"))).unwrap() }

#[test]
fn the_page_parsers_match_python_on_the_fixtures() {
    let g = golden();
    for (key, file) in [("public1", "won_public_page1.html"), ("public2", "won_public_page2.html")] {
        let p = parse_won_public(&fx(file));
        let want = &g["parse"][key];
        assert_eq!(Value::Array(p.wins.clone()), want["wins"], "{key} wins");
        assert_eq!((p.total, p.pages, p.has_next, p.logged_in_as.clone()), (want["total"].as_i64(), want["pages"].as_i64(), want["has_next"] == true, want["logged_in_as"].as_str().map(String::from)), "{key}");
    }
    let q = parse_won_private(&fx("won_private_page1.html"));
    assert_eq!((Value::Array(q.rows), q.has_next), (g["parse"]["private"]["rows"].clone(), false));
    // a name that looks like a Steam key is dropped; a page with no wins reads as empty
    let tricky = fx("won_public_page1.html").replace("Example Game One", "Example ABCDE-FGHIJ-KLMNO Game");
    assert_eq!(parse_won_public(&tricky).wins[0]["name"], Value::Null);
    assert_eq!(g["parse"]["scrubbed"], Value::Null);
    let e = parse_won_public("<html><body>nothing</body></html>");
    assert_eq!((e.wins.len(), e.total, e.pages, e.has_next, e.logged_in_as), (0, None, None, false, None));
}

#[test]
fn merging_and_the_private_pass_match_python() {
    let g = golden();
    let (p1, p2) = (parse_won_public(&fx("won_public_page1.html")), parse_won_public(&fx("won_public_page2.html")));
    let mut store = json!({"version": 1, "wins": []});
    let mut steps = vec![];
    for wins in [p1.wins.clone(), p2.wins.clone(), p1.wins.clone()] { let (n, c) = merge_public_page(&mut store, &wins, false); steps.push(json!([n, c])); }
    let mut changed = p1.wins.clone(); changed[2]["received"] = json!(true); changed[0]["points"] = json!(99);
    let (n, c) = merge_public_page(&mut store, &changed, false); steps.push(json!([n, c]));
    assert_eq!(Value::Array(steps), g["merge"]["steps"]);
    let flips = apply_private_rows(&mut store, &[json!({"code": "CCCC3", "ended_ts": 1, "received": true}), json!({"code": "DDDD4", "ended_ts": 1, "received": false}), json!({"code": "NOPE1", "ended_ts": 1, "received": true})]);
    assert_eq!(flips as i64, g["merge"]["flips"]);
    assert_eq!(store, g["merge"]["store"]);
    assert_eq!(json!(pending_pass2_pages(&store)), g["merge"]["pass2"]);
    assert_eq!(json!(pending_pass2_pages(&g["merge"]["pass2_big"]["store"])), g["merge"]["pass2_big"]["pages"]);
    // a full refresh tags what it re-confirms, and only that
    let mut fr = json!({"version": 1, "wins": [{"code": "ZZZZZ", "name": "Bogus", "steam_ref": "app/999999", "won_ts": 1, "gifter": "x", "points": 1, "received": true, "appid": null}]});
    merge_public_page(&mut fr, &p1.wins, true);
    assert_eq!(fr, g["merge"]["full_refresh_tags"]);
}

/// Steam answered from the golden tables.
struct Fake { pkg: Value, details: Value }
impl Http for Fake {
    fn get(&self, url: &str, _: &[(&str, &str)], _: Duration) -> Result<Response, NetError> {
        let ok = |v: Value| Ok(Response { status: 200, retry_after: None, body: v.to_string() });
        if let Some(rest) = url.split("packageids=").nth(1) {
            let sid = rest.split('&').next().unwrap();
            return match self.pkg[sid].as_array().filter(|a| !a.is_empty()) {
                Some(apps) => ok(json!({sid: {"success": true, "data": {"apps": apps.iter().map(|a| json!({"id": a})).collect::<Vec<_>>()}}})),
                None => ok(json!({sid: {"success": false}})),
            };
        }
        let aid = url.split("appids=").nth(1).unwrap().split('&').next().unwrap();
        if aid == "100888" { return Err(NetError("boom".into())); }
        match &self.details[aid] { Value::Null => ok(json!({aid: {"success": false}})), d => ok(json!({aid: {"success": true, "data": d}})) }
    }
}

#[test]
fn applying_wins_matches_python_on_four_library_states() {
    let g = golden();
    let a = &g["apply"];
    for case in a["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE games (appid INTEGER PRIMARY KEY, name TEXT, `groups` TEXT, sg_dlc_win INTEGER)", []).unwrap();
        for r in a["lib"].as_array().unwrap() {
            let r = r.as_array().unwrap();
            conn.execute("INSERT INTO games VALUES (?, ?, ?, ?)", rusqlite::params![r[0].as_i64(), r[1].as_str(), r[2].as_str(), r[3].as_i64()]).unwrap();
        }
        let mut store = case["input"]["store"].clone();
        let mut gs = GroupSources(case["input"]["gs"].clone());
        let http = Fake { pkg: a["pkg"].clone(), details: a["details"].clone() };
        let steam = WebSteam::new(&http, Duration::ZERO);
        let s = apply_wins(&conn, &mut gs, &mut store, case["full"] == true, &steam).unwrap();
        let w = &case["summary"];
        assert_eq!((s.games_in_group as i64, s.groups_added as i64, s.groups_removed as i64, s.unresolved_refs as i64, s.wins_total as i64),
                   (w["games_in_group"].as_i64().unwrap(), w["groups_added"].as_i64().unwrap(), w["groups_removed"].as_i64().unwrap(), w["unresolved_refs"].as_i64().unwrap(), w["wins_total"].as_i64().unwrap()), "{name}: summary");
        assert_eq!(Value::Array(s.unmatched), w["unmatched"], "{name}: unmatched");
        assert_eq!(store, case["store"], "{name}: wins file");
        assert_eq!(gs.0["sources"], case["gs"]["sources"], "{name}: sources");
        assert_eq!(gs.0["assignments"], case["gs"]["assignments"], "{name}: assignments");
        let rows: Vec<Value> = conn.prepare("SELECT appid, name, `groups`, sg_dlc_win FROM games ORDER BY appid").unwrap()
            .query_map([], |r| Ok(json!([r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, Option<i64>>(3)?]))).unwrap().flatten().collect();
        assert_eq!(Value::Array(rows), case["db"], "{name}: library");
    }
}
