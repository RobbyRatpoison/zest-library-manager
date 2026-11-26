//! What the HLTB dialog does to the library (PlayDate's `hltb.py` routes and `bulk_hltb_confirm_matches`): list the matches, search for another one, choose one, confirm a stored
//! match by fetching its times, say a game has no HowLongToBeat page, re-scrape one game, and confirm many at once. `hltb_fetched` is the state: NULL/`0` never fetched, `unconfirmed`
//! matched below the threshold (no times), `no_match` nothing found (or dismissed by hand), a date = confirmed. HLTB being unreachable never changes what is stored.
use crate::hltbclient::{Hltb, HltbData};
use crate::jobs::apply;
use crate::net::Cancel;
use rusqlite::types::Value as Sql;
use rusqlite::{params, Connection};

fn t(s: &str) -> Sql { Sql::Text(s.to_string()) }
fn ot(v: Option<i64>) -> Sql { v.map_or(Sql::Null, Sql::Integer) }
const UNREACHABLE: &str = "Couldn't reach HowLongToBeat. Nothing was changed. Try again shortly.";

/// One game with an HLTB match.
#[derive(Clone, Debug, PartialEq)]
pub struct Match { pub appid: i64, pub name: String, pub hltb_id: i64, pub matched_name: String, pub score: Option<i64>, pub main: Option<i64>, pub extras: Option<i64>, pub completionist: Option<i64>, pub fetched: String }

#[derive(Debug, Default, PartialEq)]
pub struct Overview { pub matches: Vec<Match>, pub no_match: Vec<(i64, String)>, pub unfetched: i64 }

/// `/api/hltb/matches`.
pub fn overview(conn: &Connection) -> rusqlite::Result<Overview> {
    let matches = conn.prepare("SELECT appid, COALESCE(name,''), hltb_id, COALESCE(hltb_matched_name,''), hltb_match_score, hltb_main, hltb_extras, hltb_completionist, COALESCE(hltb_fetched,'') FROM games WHERE hltb_id IS NOT NULL ORDER BY name COLLATE NOCASE")?
        .query_map([], |r| Ok(Match { appid: r.get(0)?, name: r.get(1)?, hltb_id: r.get(2)?, matched_name: r.get(3)?, score: r.get(4)?, main: r.get(5)?, extras: r.get(6)?, completionist: r.get(7)?, fetched: r.get(8)? }))?.flatten().collect();
    let no_match = conn.prepare("SELECT appid, COALESCE(name,'') FROM games WHERE hltb_fetched = 'no_match' ORDER BY name COLLATE NOCASE")?.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.flatten().collect();
    let unfetched = conn.query_row("SELECT COUNT(*) FROM games WHERE hltb_fetched = '0' OR hltb_fetched IS NULL", [], |r| r.get(0))?;
    Ok(Overview { matches, no_match, unfetched })
}

/// "Paste HLTB URL or game ID": the id out of `https://howlongtobeat.com/game/12345` or a bare number.
pub fn parse_hltb_id(input: &str) -> Option<i64> {
    let s = input.trim().trim_end_matches('/');
    if let Ok(n) = s.parse::<i64>() { return (n > 0).then_some(n); }
    let after = s.split("/game/").nth(1)?;
    let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse::<i64>().ok().filter(|n| *n > 0)
}

fn name_of(conn: &Connection, appid: i64) -> Result<String, String> { conn.query_row("SELECT COALESCE(name,'') FROM games WHERE appid = ?", [appid], |r| r.get(0)).map_err(|_| "Game not found".to_string()) }

/// `/api/hltb/<id>/search`: up to eight candidates for the name (or the text the user typed).
pub fn search(hltb: &Hltb, name: &str) -> Vec<HltbData> { hltb.candidates(name) }

/// `/api/hltb/<id>/select`: the user chose entry `hltb_id` (with the search's score); its times are fetched and the game is confirmed.
pub fn select(conn: &Connection, hltb: &Hltb, appid: i64, hltb_id: i64, score: Option<i64>, today: &str) -> Result<(), String> {
    let name = name_of(conn, appid)?;
    let r = hltb.fetch_by_id(&name, hltb_id).map_err(|_| UNREACHABLE.to_string())?;
    if !r.times_available { return Err("HowLongToBeat has no time data for that entry.".into()); }
    apply(conn, appid, &vec![("hltb_fetched", t(today)), ("hltb_id", Sql::Integer(hltb_id)), ("hltb_match_score", ot(score)), ("hltb_matched_name", r.matched_name.as_deref().map_or(Sql::Null, t)),
        ("hltb_main", ot(r.main)), ("hltb_extras", ot(r.extras)), ("hltb_completionist", ot(r.completionist))]).map_err(|e| e.to_string())
}

fn cleared() -> crate::jobs::Update { vec![("hltb_fetched", t("no_match")), ("hltb_id", Sql::Null), ("hltb_matched_name", Sql::Null), ("hltb_match_score", Sql::Null), ("hltb_main", Sql::Null), ("hltb_extras", Sql::Null), ("hltb_completionist", Sql::Null)] }

/// `DELETE /api/hltb/<id>`: this game has no HowLongToBeat page. It becomes `no_match`, which the startup catch-up never refetches, so the same wrong suggestion does not come back.
pub fn clear(conn: &Connection, appid: i64) -> Result<(), String> { apply(conn, appid, &cleared()).map_err(|e| e.to_string()) }

/// `POST /api/hltb/<id>`: look the game up again by name at `threshold`. Nothing found is stored as `no_match`; HLTB being unreachable keeps what is there.
pub fn rescrape(conn: &Connection, hltb: &Hltb, appid: i64, threshold: i64, today: &str) -> Result<Option<HltbData>, String> {
    let name = name_of(conn, appid)?;
    match hltb.fetch_data(&name, threshold, today).map_err(|_| "Couldn't reach HowLongToBeat. Existing data kept. Try again shortly.".to_string())? {
        Some(d) => { apply(conn, appid, &vec![("hltb_id", Sql::Integer(d.id)), ("hltb_matched_name", t(&d.matched_name)), ("hltb_main", ot(d.main)), ("hltb_extras", ot(d.extras)), ("hltb_completionist", ot(d.completionist)),
            ("hltb_match_score", Sql::Integer(d.score)), ("hltb_fetched", t(&d.fetched))]).map_err(|e| e.to_string())?; Ok(Some(d)) }
        None => { apply(conn, appid, &cleared()).map_err(|e| e.to_string())?; Ok(None) }
    }
}

/// What confirming one stored match came to.
#[derive(Debug, PartialEq)]
pub enum Confirmed { Saved, /// HLTB answered but the entry has no times: the game moved to `no_match`.
    Cleared }

/// Copies confirmed data to the copies of the same game (`_propagate_hltb`): the games that point at `canonical` as their `duplicate_of`.
fn propagate(conn: &Connection, appid: i64, u: &crate::jobs::Update) {
    let canonical: String = conn.query_row("SELECT COALESCE(duplicate_of, CAST(appid AS TEXT)) FROM games WHERE appid = ?", [appid], |r| r.get(0)).unwrap_or_else(|_| appid.to_string());
    let ids: Vec<i64> = conn.prepare("SELECT appid FROM games WHERE duplicate_of = ?1 AND appid != ?2").and_then(|mut s| s.query_map(params![canonical, appid], |r| r.get(0)).map(|r| r.flatten().collect())).unwrap_or_default();
    for d in ids { let _ = apply(conn, d, u); }
}

fn confirm_update(r: &crate::hltbclient::ById, today: &str) -> crate::jobs::Update {
    vec![("hltb_matched_name", r.matched_name.as_deref().map_or(Sql::Null, t)), ("hltb_main", ot(r.main)), ("hltb_extras", ot(r.extras)), ("hltb_completionist", ot(r.completionist)), ("hltb_fetched", t(today))]
}

/// `POST /api/hltb/<id>/confirm`: the stored match's times are fetched and the game is marked confirmed (and its duplicate copies).
pub fn confirm(conn: &Connection, hltb: &Hltb, appid: i64, today: &str) -> Result<Confirmed, String> {
    let (name, id): (String, Option<i64>) = conn.query_row("SELECT COALESCE(name,''), hltb_id FROM games WHERE appid = ?", [appid], |r| Ok((r.get(0)?, r.get(1)?))).map_err(|_| "Game not found".to_string())?;
    let id = id.filter(|i| *i != 0).ok_or("No HLTB match to confirm.")?;
    let r = hltb.fetch_by_id(&name, id).map_err(|_| "Couldn't reach HowLongToBeat. Your match was kept. Try again shortly.".to_string())?;
    if r.times_available { let u = confirm_update(&r, today); apply(conn, appid, &u).map_err(|e| e.to_string())?; propagate(conn, appid, &u); Ok(Confirmed::Saved) }
    else { apply(conn, appid, &cleared()).map_err(|e| e.to_string())?; Ok(Confirmed::Cleared) }
}

#[derive(Debug, Default, PartialEq)]
pub struct ConfirmCounts { pub done: usize, pub failed: usize, pub cleared: usize, pub aborted: bool, pub cancelled: bool }

/// `bulk_hltb_confirm_matches`: confirm each game's stored match in turn. After five unreachable answers in a row the run stops with the matches kept. `progress(done so far, total)`.
pub fn confirm_many(conn: &Connection, hltb: &Hltb, appids: &[i64], today: &str, cancel: &Cancel, progress: &mut dyn FnMut(usize, usize)) -> ConfirmCounts {
    let mut c = ConfirmCounts::default();
    let mut streak = 0;
    for (n, &appid) in appids.iter().enumerate() {
        if cancel.is_set() { c.cancelled = true; break; }
        let row: Option<(String, Option<i64>)> = conn.query_row("SELECT COALESCE(name,''), hltb_id FROM games WHERE appid = ?", [appid], |r| Ok((r.get(0)?, r.get(1)?))).ok();
        let Some((name, Some(id))) = row.filter(|(_, i)| i.map_or(false, |i| i != 0)) else { c.failed += 1; progress(n + 1, appids.len()); continue };
        match hltb.fetch_by_id(&name, id) {
            Err(_) => { streak += 1; c.failed += 1; if streak >= 5 { c.aborted = true; break; } }
            Ok(r) => {
                streak = 0;
                if r.times_available { let u = confirm_update(&r, today); if apply(conn, appid, &u).is_ok() { propagate(conn, appid, &u); c.done += 1; } else { c.failed += 1; } }
                else if apply(conn, appid, &cleared()).is_ok() { c.cleared += 1; } else { c.failed += 1; }
            }
        }
        progress(n + 1, appids.len());
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{Http, NetError, Response};
    use std::sync::Arc;
    use std::time::Duration;
    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE games (appid INTEGER PRIMARY KEY, name TEXT, duplicate_of TEXT, hltb_id INTEGER, hltb_matched_name TEXT, hltb_match_score INTEGER, hltb_main INTEGER, hltb_extras INTEGER, hltb_completionist INTEGER, hltb_fetched TEXT);
            INSERT INTO games (appid, name, hltb_id, hltb_matched_name, hltb_match_score, hltb_fetched) VALUES (1, 'Portal 2', 7, 'Portal 2', 90, 'unconfirmed'), (2, 'Zed', NULL, NULL, NULL, 'no_match'), (3, 'Fresh', NULL, NULL, NULL, '0'),
                (4, 'Gone', 99, 'Gone', 80, 'unconfirmed');
            INSERT INTO games (appid, name, duplicate_of, hltb_fetched) VALUES (5, 'Portal 2 (GOG)', '1', '0');").unwrap();
        c
    }
    /// An HLTB that knows entry 7 (Portal 2: 10 / 20 / 30 hours) and nothing else; `down` makes every request fail.
    struct Site { down: bool }
    impl Http for Site {
        fn get(&self, u: &str, _h: &[(&str, &str)], _t: Duration) -> Result<Response, NetError> {
            if self.down { return Err(NetError("Io".into())); }
            let ok = |b: &str| Ok(Response { status: 200, retry_after: None, body: b.into() });
            if u.ends_with(".js") { ok(r#"fetch("/api/find",{method:"POST"})"#) } else if u.contains("/init?t=") { ok(r#"{"token":"t","k":"a","v":"b"}"#) }
            else if u.contains("game?id=7") { ok("<title>How long is Portal 2? | HowLongToBeat</title>") } else if u.contains("game?id=") { ok("<title>Nothing | HowLongToBeat</title>") } else { ok(r#"<script src="/_next/static/chunks/a.js"></script>"#) } }
        fn post(&self, _u: &str, _h: &[(&str, &str)], b: &str, _t: Duration) -> Result<Response, NetError> {
            if self.down { return Err(NetError("Io".into())); }
            let body = if b.contains("Portal") { r#"{"data":[{"game_id":7,"game_name":"Portal 2","comp_main":36000,"comp_plus":72000,"comp_100":108000}]}"# } else { r#"{"data":[]}"# };
            Ok(Response { status: 200, retry_after: None, body: body.into() }) }
    }
    fn hltb(down: bool) -> Hltb { Hltb::new(Arc::new(Site { down })) }

    #[test] fn the_overview_lists_matches_no_matches_and_the_unfetched_count() {
        let o = overview(&db()).unwrap();
        assert_eq!(o.matches.iter().map(|m| (m.appid, m.fetched.as_str())).collect::<Vec<_>>(), vec![(4, "unconfirmed"), (1, "unconfirmed")], "by name, any case");
        assert_eq!(o.no_match, vec![(2, "Zed".to_string())]); assert_eq!(o.unfetched, 2);
    }
    #[test] fn pasted_addresses_and_numbers_give_the_hltb_id() {
        assert_eq!(parse_hltb_id("https://howlongtobeat.com/game/12345"), Some(12345)); assert_eq!(parse_hltb_id(" https://howlongtobeat.com/game/7/ "), Some(7));
        assert_eq!(parse_hltb_id("https://howlongtobeat.com/game/88?x=1"), Some(88)); assert_eq!(parse_hltb_id("4242"), Some(4242));
        assert_eq!(parse_hltb_id("0"), None); assert_eq!(parse_hltb_id("portal"), None); assert_eq!(parse_hltb_id("https://howlongtobeat.com/"), None);
    }
    #[test] fn clearing_parks_the_game_as_no_match_and_unreachable_changes_nothing() {
        let c = db();
        clear(&c, 1).unwrap();
        let r: (Option<i64>, String, Option<String>) = c.query_row("SELECT hltb_id, hltb_fetched, hltb_matched_name FROM games WHERE appid = 1", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!(r, (None, "no_match".into(), None));
        // HLTB down: confirm and select say so and keep the stored match
        let down = hltb(true);
        assert!(confirm(&c, &down, 4, "2026-11-04").unwrap_err().contains("Your match was kept"));
        assert!(select(&c, &down, 4, 99, Some(80), "2026-11-04").unwrap_err().contains("Nothing was changed"));
        assert!(rescrape(&c, &down, 4, 75, "2026-11-04").unwrap_err().contains("Existing data kept"));
        assert_eq!(c.query_row("SELECT hltb_id, hltb_fetched FROM games WHERE appid = 4", [], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))).unwrap(), (99, "unconfirmed".into()));
        assert!(confirm(&c, &down, 3, "x").unwrap_err().contains("No HLTB match to confirm"));
        assert!(clear(&c, 12345).is_ok(), "a missing game is not an error, as in Python");
        let s = confirm_many(&c, &down, &[4, 4, 4, 4, 4, 4, 4], "x", &Cancel::new(), &mut |_, _| {});
        assert!(s.aborted && s.failed == 5 && s.done == 0, "{s:?}");
    }

    #[test] fn confirming_saves_the_times_for_the_game_and_its_copies_and_an_empty_entry_moves_it_to_no_match() {
        let c = db(); let h = hltb(false);
        assert_eq!(confirm(&c, &h, 1, "2026-11-04").unwrap(), Confirmed::Saved);
        let g = |id: i64| c.query_row("SELECT hltb_fetched, hltb_main, hltb_extras, hltb_completionist FROM games WHERE appid = ?", [id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?, r.get::<_, Option<i64>>(2)?, r.get::<_, Option<i64>>(3)?))).unwrap();
        assert_eq!(g(1), ("2026-11-04".into(), Some(600), Some(1200), Some(1800)));
        assert_eq!(g(5), ("2026-11-04".into(), Some(600), Some(1200), Some(1800)), "the GOG copy gets the same times");
        // game 4's entry (99) has nothing: it is moved to no_match
        assert_eq!(confirm(&c, &h, 4, "2026-11-04").unwrap(), Confirmed::Cleared);
        assert_eq!(c.query_row("SELECT hltb_fetched, hltb_id FROM games WHERE appid = 4", [], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?))).unwrap(), ("no_match".into(), None));
    }
    #[test] fn choosing_an_entry_confirms_it_with_its_score_and_a_bulk_run_counts_each_outcome() {
        let c = db(); let h = hltb(false);
        select(&c, &h, 3, 7, Some(88), "2026-11-04").unwrap();
        assert_eq!(c.query_row("SELECT hltb_id, hltb_match_score, hltb_fetched, hltb_matched_name FROM games WHERE appid = 3", [], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?))).unwrap(), (7, 88, "2026-11-04".into(), "Portal 2".into()));
        assert!(select(&c, &h, 3, 99, None, "x").unwrap_err().contains("no time data"));
        let c = db();
        let mut seen = vec![];
        let n = confirm_many(&c, &h, &[1, 4, 3, 2], "2026-11-04", &Cancel::new(), &mut |d, t| seen.push((d, t)));
        assert_eq!((n.done, n.cleared, n.failed, n.aborted), (1, 1, 2, false), "1 confirmed, 4 cleared, 3 and 2 have no stored match");
        assert_eq!(seen.last(), Some(&(4, 4)));
        let stop = Cancel::new(); stop.cancel();
        assert!(confirm_many(&c, &h, &[1], "x", &stop, &mut |_, _| {}).cancelled);
        // a re-scrape that finds nothing parks the game, one that finds Portal 2 stores it
        assert!(rescrape(&c, &h, 3, 75, "2026-11-04").unwrap().is_none());
        assert_eq!(c.query_row("SELECT hltb_fetched FROM games WHERE appid = 3", [], |r| r.get::<_, String>(0)).unwrap(), "no_match");
        c.execute("UPDATE games SET name = 'Portal 2' WHERE appid = 3", []).unwrap();
        let d = rescrape(&c, &h, 3, 75, "2026-11-04").unwrap().unwrap(); assert_eq!((d.id, d.main), (7, Some(600)));
        assert_eq!(search(&h, "Portal 2").first().map(|d| d.id), Some(7));
    }
}
