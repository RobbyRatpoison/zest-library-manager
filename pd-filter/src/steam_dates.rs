//! Steam activation dates through the Companion userscript (`date_import.py`, `steam_date_import.user.js`'s Steam half): Zest says which games to read (a plan), the userscript
//! fetches each game's Steam Help page in the signed-in tab and sends the earliest "activated" date back, and each answer becomes that game's Date Added.
//! Unlike PlayDate, a single game is written straight away too (PlayDate filled the Edit form and let you save).
use crate::companion::SteamGiftsHandler;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Default)]
struct Run { /// Games the licenses list has matched so far (the bar moves while it is still being read).
    lic_matched: usize, plan: Vec<(i64, String)>, answered: Vec<i64>, done: usize, failed: usize, last: Option<std::time::Instant>, started: Option<std::time::Instant>, summary: Option<String>, to_db: bool }

/// A run that gets no answer for this long (the script answers every few seconds once it has the list) is called stopped (the tab was closed, the browser went away).
const IDLE_LIMIT: std::time::Duration = std::time::Duration::from_secs(30);
/// How long the browser has to open the Help page and ask for the list before the run is called never started.
const START_LIMIT: std::time::Duration = std::time::Duration::from_secs(180);

pub struct SteamDates { db: PathBuf, steam_id: String, run: Mutex<Run>, written: Mutex<Vec<(i64, String)>>, store: std::sync::Arc<dyn crate::steamsession::SecretStore> }

impl SteamDates {
    pub fn new(db: PathBuf, steam_id: String) -> SteamDates { Self::with_store(db, steam_id, std::sync::Arc::new(crate::steamsession::SecretTool)) }
    /// Where the saved sign-in is looked for (the keyring in the app; tests pass their own).
    pub fn with_store(db: PathBuf, steam_id: String, store: std::sync::Arc<dyn crate::steamsession::SecretStore>) -> SteamDates { SteamDates { db, steam_id, run: Mutex::new(Run::default()), written: Mutex::new(Vec::new()), store } }
    /// Is there a sign-in to read with: a session in memory, or a saved one to trade for a session?
    pub fn can_read(&self) -> bool { crate::steamsignin::help_session().is_some() || self.store.get(&self.steam_id).is_some() }
    /// The games the next browser run reads (named so the page can show which one it is on). Replaces any earlier plan.
    pub fn set_plan(&self, games: Vec<(i64, String)>) { if let Ok(mut r) = self.run.lock() { *r = Run { plan: games, started: Some(std::time::Instant::now()), to_db: true, ..Default::default() }; } }
    /// As `set_plan`, but a date read is only handed to the open Edit dialog (`take_written`), not saved: the dialog's SAVE does that.
    pub fn set_plan_for_dialog(&self, games: Vec<(i64, String)>) { if let Ok(mut r) = self.run.lock() { *r = Run { plan: games, started: Some(std::time::Instant::now()), to_db: false, ..Default::default() }; } }
    /// The dates saved since this was last asked, (appid, 'YYYY-MM-DD'): an open Edit dialog shows its own copy of Date Added and must pick the new one up (or its SAVE would put the old one back).
    pub fn take_written(&self) -> Vec<(i64, String)> { self.written.lock().map(|mut w| std::mem::take(&mut *w)).unwrap_or_default() }
    /// A run in progress (the browser has asked for its plan and has answers left): (label, games in the plan, answered), for the Bulk Ops progress bar.
    pub fn progress(&self) -> Option<(String, usize, usize)> {
        let r = self.run.lock().ok()?;
        (r.started.is_some() && r.answered.len() < r.plan.len()).then(|| ("Steam dates".to_string(), r.plan.len(), r.answered.len().max(r.lic_matched.min(r.plan.len()))))
    }
    /// Stop (Bulk Ops' Stop button): the plan is dropped, so the browser's next answer is refused and its script ends the run.
    pub fn cancel(&self) {
        if let Ok(mut r) = self.run.lock() { if r.started.is_some() { r.summary = Some(format!("Steam dates stopped: {} saved, {} not found", r.done, r.failed)); } *r = Run { summary: r.summary.take(), ..Default::default() }; }
        crate::net::set_activity("");
    }
    /// Called often by the app: the one-line outcome of a run that just ended (all answered, or no answer for `IDLE_LIMIT`), once.
    pub fn tick(&self) -> Option<String> {
        let mut r = self.run.lock().ok()?;
        if let Some(t) = r.last { if r.answered.len() < r.plan.len() && t.elapsed() > IDLE_LIMIT {
            crate::net::set_activity("");
            r.summary = Some(format!("Steam dates stopped: {} of {} read, the browser stopped answering ({} saved)", r.done + r.failed, r.plan.len(), r.done));
            r.last = None; r.started = None;
        } }
        if r.last.is_none() { if let Some(t) = r.started { if r.answered.is_empty() && t.elapsed() > START_LIMIT {
            crate::net::set_activity("");
            r.summary = Some("Steam dates: the browser never started reading (is the Zest Companion script installed and Steam open in your browser?)".to_string());
            r.started = None;
        } } }
        r.summary.take()
    }
    fn write(&self, appid: i64, date: &str) -> Result<(), String> {
        let ts = crate::date_to_ts(&Value::String(date.to_string())).ok_or("that is not a date")?;
        let c = Connection::open(&self.db).map_err(|e| e.to_string())?; c.busy_timeout(std::time::Duration::from_secs(10)).ok();
        c.execute("UPDATE games SET date_added = ? WHERE appid = ?", rusqlite::params![ts, appid]).map_err(|e| e.to_string())?;
        Ok(())
    }
}

impl SteamGiftsHandler for SteamDates {
    fn call(&self, route: &str, body: &Value) -> Result<Value, String> {
        let mut r = self.run.lock().map_err(|_| "the import is in an unusable state".to_string())?;
        match route {
            "steam/plan" => {
                if r.plan.is_empty() { return Err("Zest has no games to read. Start it from Zest again.".into()); }
                r.answered.clear(); r.done = 0; r.failed = 0; r.last = Some(std::time::Instant::now()); r.summary = None;
                crate::net::set_activity(&format!("Steam dates: 0 of {}", r.plan.len()));
                Ok(json!({ "steam_id": self.steam_id, "items": r.plan.iter().map(|(a, n)| json!({ "appid": a, "name": n })).collect::<Vec<_>>() }))
            }
            "steam/result" => {
                let appid = body.get("appid").and_then(|v| v.as_i64()).ok_or("no game")?;
                if !r.plan.iter().any(|(a, _)| *a == appid) { return Err("that game is not in the list".into()); }
                if let Some(page) = body.get("steam_id").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
                    if page != self.steam_id { return Err(format!("Steam is signed in as {page} but Zest is set up for {}. Nothing was imported.", self.steam_id)); }
                }
                if r.answered.contains(&appid) { return Ok(json!({ "done": r.done, "failed": r.failed, "total": r.plan.len() })); }
                match body.get("date").and_then(|v| v.as_str()).filter(|d| !d.is_empty()) {
                    Some(d) => { if r.to_db { self.write(appid, d)?; } if let Ok(mut w) = self.written.lock() { w.push((appid, d.to_string())); } r.done += 1; }
                    None => r.failed += 1,
                }
                r.answered.push(appid); r.last = Some(std::time::Instant::now());
                let (done, failed, total) = (r.done, r.failed, r.plan.len());
                crate::net::set_activity(&if done + failed >= total { String::new() } else { format!("Steam dates: {} of {total}", done + failed) });
                if done + failed >= total { crate::net::log(&format!("Steam activation dates: {done} saved, {failed} not found")); r.summary = Some(if r.to_db { format!("Steam dates: {done} saved, {failed} not found") } else if done > 0 { "Steam date read: it is in the Edit dialog, press SAVE to keep it".to_string() } else { "Steam date: not found for this game".to_string() }); r.last = None; r.started = None; }
                Ok(json!({ "done": done, "failed": failed, "total": total }))
            }
            _ => Err("no such step".into()),
        }
    }
}


// ───────────────────────── reading the Help pages directly, with the sign-in's session ─────────────────────────

/// "Oct 1, 2017", "Oct 1" (no year: this year, as the userscript did) or "1 Oct, 2017" as `YYYY-MM-DD`.
pub fn parse_help_date(text: &str, this_year: i64) -> Option<String> {
    use regex::Regex;
    use std::sync::OnceLock;
    static MF: OnceLock<Regex> = OnceLock::new(); static DF: OnceLock<Regex> = OnceLock::new();
    let mf = MF.get_or_init(|| Regex::new(r"^([A-Za-z]{3,9})\.?\s+(\d{1,2})(?:,?\s+(\d{4}))?$").unwrap());
    let df = DF.get_or_init(|| Regex::new(r"^(\d{1,2})\s+([A-Za-z]{3,9})\.?(?:,?\s+(\d{4}))?$").unwrap());
    let t = text.replace('\u{a0}', " "); let t = t.trim();
    let (mon, day, year) = if let Some(c) = mf.captures(t) { (c[1].to_string(), c[2].to_string(), c.get(3).map(|m| m.as_str().to_string())) }
        else if let Some(c) = df.captures(t) { (c[2].to_string(), c[1].to_string(), c.get(3).map(|m| m.as_str().to_string())) } else { return None };
    const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    let m = MONTHS.iter().position(|m| mon.to_lowercase().starts_with(m))? + 1;
    let d: u32 = day.parse().ok()?; if !(1..=31).contains(&d) { return None; }
    let y: i64 = year.map(|y| y.parse().ok()).unwrap_or(Some(this_year))?;
    Some(format!("{y:04}-{m:02}-{d:02}"))
}

/// The earliest date on a Help page for a game (`/en/wizard/HelpWithGame/?appid=N`): the purchase rows, else the "Activated:" line. The same two places the userscript read.
pub fn parse_help_page(html: &str, this_year: i64) -> Option<String> {
    use scraper::{ElementRef, Html, Selector};
    let doc = Html::parse_document(html);
    let mut dates: Vec<String> = Vec::new();
    if let Ok(sel) = Selector::parse(".LineItemRow span:first-child") {
        for el in doc.select(&sel) { let t: String = el.text().collect(); if let Some(d) = parse_help_date(t.split('-').next().unwrap_or(""), this_year) { dates.push(d); } }
    }
    if dates.is_empty() {
        if let Ok(sel) = Selector::parse(".account_details .help_highlight_text") {
            for el in doc.select(&sel) {
                let label: String = el.text().collect();
                if label.trim() != "Activated:" { continue; }
                if let Some(next) = el.next_siblings().filter_map(ElementRef::wrap).next() { let t: String = next.text().collect(); if let Some(d) = parse_help_date(&t, this_year) { dates.push(d); } }
            }
        }
    }
    dates.sort(); dates.into_iter().next()
}

pub enum HelpErr { /// Steam sent the request to its sign-in page: the session is no longer good.
    Expired, Failed(String) }

/// One game's Help page as the signed-in player. Ok(None) = the page loaded and shows no purchase of this game.
pub fn fetch_help_date(http: &dyn crate::net::Http, cookie: &str, appid: i64, this_year: i64) -> Result<Option<String>, HelpErr> {
    let url = format!("https://help.steampowered.com/en/wizard/HelpWithGame/?appid={appid}");
    let header = format!("steamLoginSecure={}", cookie.trim());
    let (r, end) = http.get_final(&url, &[("Cookie", header.as_str())], std::time::Duration::from_secs(25)).map_err(|e| HelpErr::Failed(e.to_string()))?;
    if end.contains("/Login") || end.contains("/login") { return Err(HelpErr::Expired); }
    // a signed-out visitor gets the same address back with a sign-in form, not a redirect: that is the session gone (or never accepted)
    if r.status == 200 && crate::steamsignin::help_page_signed_out(&r.body) { crate::net::log(&format!("steam dates: the Help page for {appid} is the signed-out page ({} bytes)", r.body.len())); return Err(HelpErr::Expired); }
    // what the page held, never its content: enough to tell a changed layout from a game that was never bought
    crate::net::log(&format!("steam dates: Help page for {appid}: status {}, {} bytes, purchase rows {}, activated line {}", r.status, r.body.len(), r.body.contains("LineItemRow"), r.body.contains("Activated:")));
    match r.status { 200 => Ok(parse_help_page(&r.body, this_year)), 429 => Err(HelpErr::Failed("Steam asked Zest to slow down".into())), c => Err(HelpErr::Failed(format!("Steam answered {c}"))) }
}

impl SteamDates {
    /// The store's licenses list (`steamlicenses`) dates most games in a few requests, where the Help page costs one per game: read it first for a run of three or more games, answer
    /// the plan for every game it matched, and return the appids it could not date (those go on to the Help pages). Anything that goes wrong returns the whole list unchanged.
    fn licenses_first(&self, http: &dyn crate::net::Http, planned: Vec<(i64, String)>, pause: std::time::Duration) -> Vec<i64> {
        let all: Vec<i64> = planned.iter().map(|(a, _)| *a).collect();
        if planned.len() < 3 { return all; }
        if crate::steamsignin::store_session().is_none() { crate::steamsession::restore(http, &*self.store, &self.steam_id); }
        let Some(cookie) = crate::steamsignin::store_session() else { return all };
        let pages = if planned.len() >= 100 { 60 } else { 5 };
        let total = planned.len();
        crate::net::set_activity("Steam dates: reading your licenses list");
        // every page counts as a sign of life (the run is not called stopped while it reads) and moves the bar by the games matched so far
        let found = match crate::steamlicenses::read_dates_with(http, &cookie, &planned, pages, pause, &|p, m| {
            crate::net::set_activity(&format!("Steam dates: reading your licenses list, page {p} ({m} of {total} games matched)"));
            if let Ok(mut r) = self.run.lock() { r.last = Some(std::time::Instant::now()); r.lic_matched = m; }
        }) { Ok(f) => f, Err(e) => { crate::net::log(&format!("steam dates: the licenses list could not be read ({e})")); return all } };
        crate::net::log(&format!("steam dates: the licenses list dated {} of {} games", found.len(), planned.len()));
        let mut left = Vec::new();
        for id in all {
            match found.get(&id) {
                Some(d) => { if self.call("steam/result", &json!({ "appid": id, "date": d })).is_err() { return vec![]; } }   // the plan was dropped (Stop)
                None => left.push(id),
            }
        }
        left
    }
    /// Ends the run with a message (a sign-in problem, say) in place of a summary.
    pub fn fail(&self, msg: &str) {
        if let Ok(mut r) = self.run.lock() { *r = Run { summary: Some(format!("Steam dates: {msg}")), ..Default::default() }; }
        crate::net::set_activity("");
    }
    /// Reads the planned games' Help pages itself, with the session the Steam sign-in left (`steamsignin::help_session`), one every `pause`, and answers its own plan like the userscript
    /// would. Stops when the plan is dropped (Stop), when Steam signs the session out, or after repeated failures. Runs on its own thread.
    pub fn run_direct(self: &std::sync::Arc<Self>, http: std::sync::Arc<dyn crate::net::Http>, pause: std::time::Duration) {
        let me = self.clone();
        std::thread::spawn(move || {
            let planned: Vec<(i64, String)> = match me.call("steam/plan", &json!({})) { Ok(v) => v["items"].as_array().map(|a| a.iter().filter_map(|i| Some((i["appid"].as_i64()?, i["name"].as_str().unwrap_or("").to_string()))).collect()).unwrap_or_default(), Err(e) => { me.fail(&e); return } };
            let items: Vec<i64> = me.licenses_first(&*http, planned, std::time::Duration::from_millis(2500));
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
            let year = crate::pick::year_of(now).unwrap_or(2026);
            let mut in_a_row = 0;
            // a sign-in saved earlier (the refresh token in the keyring) is traded for a session when none is in memory
            if crate::steamsignin::help_session().is_none() { crate::steamsession::restore(&*http, &*me.store, &me.steam_id); }
            for appid in items {
                let mut renewed = false;
                let body = loop {
                    let Some(cookie) = crate::steamsignin::help_session() else { me.fail("Steam is not signed in. Press the button again to sign in."); return };
                    match fetch_help_date(&*http, &cookie, appid, year) {
                        Ok(Some(d)) => { in_a_row = 0; break json!({ "appid": appid, "date": d }) }
                        Ok(None) => { in_a_row = 0; break json!({ "appid": appid }) }
                        Err(HelpErr::Expired) => {
                            // the day-long session ran out: the saved sign-in gives a new one (once per game, so a refusal does not loop)
                            crate::steamsignin::clear_help_session();
                            if !renewed { renewed = true; if crate::steamsession::restore(&*http, &*me.store, &me.steam_id) { continue; } }
                            me.fail("Steam signed you out. Press the button again to sign in."); return
                        }
                        Err(HelpErr::Failed(m)) => { in_a_row += 1; if in_a_row >= 5 { me.fail(&format!("stopped after repeated errors ({m})")); return } break json!({ "appid": appid }) }
                    }
                };
                if me.call("steam/result", &body).is_err() { return; }   // the plan was dropped (Stop) or does not hold this game
                std::thread::sleep(pause);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup(tag: &str) -> (SteamDates, PathBuf) {
        let d = std::env::temp_dir().join(format!("zest_sd_{tag}_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        let db = d.join("games_1.db"); crate::schema::init_db(&db).unwrap();
        let c = Connection::open(&db).unwrap();
        c.execute("INSERT INTO games (appid, name, date_added) VALUES (10, 'Ten', 5), (20, 'Twenty', 5)", []).unwrap();
        let s = SteamDates::with_store(db.clone(), "76561198000000001".into(), std::sync::Arc::new(crate::steamsession::NoSecrets)); s.set_plan(vec![(10, "Ten".into()), (20, "Twenty".into())]); (s, db)
    }
    fn added(db: &PathBuf, a: i64) -> i64 { Connection::open(db).unwrap().query_row("SELECT date_added FROM games WHERE appid = ?", [a], |r| r.get(0)).unwrap() }
    #[test] fn a_finished_run_gives_one_summary_and_a_silent_one_is_called_stopped() {
        let (s, _) = setup("d"); s.call("steam/plan", &json!({})).unwrap();
        s.call("steam/result", &json!({ "appid": 10, "date": "2017-10-01" })).unwrap();
        assert_eq!(s.tick(), None, "not finished yet");
        s.call("steam/result", &json!({ "appid": 20 })).unwrap();
        assert_eq!(s.tick().as_deref(), Some("Steam dates: 1 saved, 1 not found")); assert_eq!(s.tick(), None, "told once");
        let (q, _) = setup("e"); q.call("steam/plan", &json!({})).unwrap(); q.call("steam/result", &json!({ "appid": 10, "date": "2017-10-01" })).unwrap();
        q.run.lock().unwrap().last = Some(std::time::Instant::now() - IDLE_LIMIT - std::time::Duration::from_secs(1));
        assert!(q.tick().unwrap().contains("1 of 2 read")); assert_eq!(q.tick(), None);
    }
    #[test] fn a_date_read_for_the_edit_dialog_is_handed_over_and_not_saved() {
        let (s, db) = setup("g"); s.set_plan_for_dialog(vec![(10, "Ten".into())]);
        s.call("steam/plan", &json!({})).unwrap();
        s.call("steam/result", &json!({ "appid": 10, "date": "2017-10-01" })).unwrap();
        assert_eq!(added(&db, 10), 5, "the library is untouched: the dialog's SAVE does the saving");
        assert_eq!(s.take_written(), vec![(10, "2017-10-01".to_string())]); assert!(s.take_written().is_empty());
        assert!(s.tick().unwrap().contains("press SAVE"));
        // a bulk run still saves
        s.set_plan(vec![(20, "Twenty".into())]); s.call("steam/plan", &json!({})).unwrap();
        s.call("steam/result", &json!({ "appid": 20, "date": "2017-10-01" })).unwrap();
        assert_ne!(added(&db, 20), 5);
    }
    #[test] fn the_plan_names_the_games_and_results_set_date_added_only_for_planned_games() {
        let (s, db) = setup("a");
        let p = s.call("steam/plan", &json!({})).unwrap(); assert_eq!(p["items"].as_array().unwrap().len(), 2); assert_eq!(p["steam_id"], "76561198000000001");
        let r = s.call("steam/result", &json!({ "appid": 10, "date": "2017-10-01", "steam_id": "76561198000000001" })).unwrap();
        assert_eq!((r["done"].as_i64(), r["failed"].as_i64(), r["total"].as_i64()), (Some(1), Some(0), Some(2)));
        assert_eq!(added(&db, 10), 1506816000);
        let r = s.call("steam/result", &json!({ "appid": 20, "date": null })).unwrap(); assert_eq!(r["failed"], 1); assert_eq!(added(&db, 20), 5, "no date found leaves it alone");
        assert!(s.call("steam/result", &json!({ "appid": 99, "date": "2017-10-01" })).is_err(), "a game outside the plan is refused");
    }
    #[test] fn another_account_a_repeat_and_a_bad_date_change_nothing() {
        let (s, db) = setup("b"); s.call("steam/plan", &json!({})).unwrap();
        let e = s.call("steam/result", &json!({ "appid": 10, "date": "2017-10-01", "steam_id": "76561198999999999" })).unwrap_err(); assert!(e.contains("Nothing was imported")); assert_eq!(added(&db, 10), 5);
        assert!(s.call("steam/result", &json!({ "appid": 10, "date": "soon" })).is_err()); assert_eq!(added(&db, 10), 5);
        s.call("steam/result", &json!({ "appid": 10, "date": "2018-01-01" })).unwrap(); let once = added(&db, 10);
        s.call("steam/result", &json!({ "appid": 10, "date": "2019-01-01" })).unwrap(); assert_eq!(added(&db, 10), once, "an answer already taken is not applied twice");
        let (empty, _) = setup("c"); empty.set_plan(vec![]); assert!(empty.call("steam/plan", &json!({})).is_err());
    }

    #[test] fn help_dates_are_read_in_the_forms_steam_shows() {
        assert_eq!(parse_help_date("Oct 1, 2017", 2026).as_deref(), Some("2017-10-01"));
        assert_eq!(parse_help_date("Mar 25", 2026).as_deref(), Some("2026-03-25"));
        assert_eq!(parse_help_date("1 Oct, 2017", 2026).as_deref(), Some("2017-10-01"));
        assert_eq!(parse_help_date("Sept\u{a0}3,\u{a0}2019", 2026).as_deref(), Some("2019-09-03"));
        assert_eq!(parse_help_date("soon", 2026), None); assert_eq!(parse_help_date("Foo 3, 2019", 2026), None); assert_eq!(parse_help_date("Oct 40, 2019", 2026), None);
    }
    #[test] fn the_earliest_date_on_a_help_page_wins_and_the_activated_line_is_the_fallback() {
        let rows = r#"<div class="LineItemRow"><span>Nov 3, 2019 - Purchase</span></div><div class="LineItemRow"><span>Jan 2, 2018 - Purchase</span></div>"#;
        assert_eq!(parse_help_page(rows, 2026).as_deref(), Some("2018-01-02"));
        let act = r#"<div class="account_details"><span class="help_highlight_text">Activated:</span><span>Feb 9, 2020</span></div>"#;
        assert_eq!(parse_help_page(act, 2026).as_deref(), Some("2020-02-09"));
        assert_eq!(parse_help_page("<html>You do not own this</html>", 2026), None);
    }
    struct FakeHelp { expire_after: Option<usize>, calls: Mutex<usize> }
    impl crate::net::Http for FakeHelp {
        fn get(&self, url: &str, h: &[(&str, &str)], t: std::time::Duration) -> Result<crate::net::Response, crate::net::NetError> { self.get_final(url, h, t).map(|r| r.0) }
        fn get_final(&self, url: &str, h: &[(&str, &str)], _t: std::time::Duration) -> Result<(crate::net::Response, String), crate::net::NetError> {
            assert!(h.iter().any(|(k, v)| *k == "Cookie" && v.contains("steamLoginSecure=")), "the session goes along");
            let mut n = self.calls.lock().unwrap(); *n += 1;
            if self.expire_after.map_or(false, |e| *n > e) { return Ok((crate::net::Response { status: 200, retry_after: None, body: String::new() }, "https://help.steampowered.com/en/wizard/Login?x".into())); }
            let body = if url.ends_with("appid=10") { r#"<div class="LineItemRow"><span>Oct 1, 2017 - Purchase</span></div>"#.to_string() } else { "<html>nothing</html>".to_string() };
            Ok((crate::net::Response { status: 200, retry_after: None, body }, url.to_string()))
        }
    }
    fn wait_summary(s: &SteamDates) -> String { for _ in 0..200 { if let Some(m) = s.tick() { return m; } std::thread::sleep(std::time::Duration::from_millis(25)); } panic!("no summary"); }
    #[test] fn a_direct_run_saves_dates_and_a_signed_out_session_stops_it_with_a_message() {
        let (s, db) = setup("h"); let s = std::sync::Arc::new(s);
        crate::steamsignin::set_help_session("76561198000000001%7C%7Cx");
        s.run_direct(std::sync::Arc::new(FakeHelp { expire_after: None, calls: Mutex::new(0) }), std::time::Duration::from_millis(1));
        assert_eq!(wait_summary(&s), "Steam dates: 1 saved, 1 not found"); assert_eq!(added(&db, 10), 1506816000); assert_eq!(added(&db, 20), 5);
        let (t, db2) = setup("i"); let t = std::sync::Arc::new(t);
        t.run_direct(std::sync::Arc::new(FakeHelp { expire_after: Some(1), calls: Mutex::new(0) }), std::time::Duration::from_millis(1));
        assert!(wait_summary(&t).contains("signed you out")); assert_eq!(added(&db2, 20), 5, "the game after the sign-out is left alone");
        assert!(crate::steamsignin::help_session().is_none(), "an expired session is forgotten");
    }
    struct Both;
    impl crate::net::Http for Both {
        fn get(&self, url: &str, h: &[(&str, &str)], _t: std::time::Duration) -> Result<crate::net::Response, crate::net::NetError> {
            assert!(url.contains("/account/licenses/") && h.iter().any(|(k, v)| *k == "Cookie" && v.contains("steamLoginSecure=")));
            let body = r#"<table><tr><td class="license_date_col">Mar 1, 2020</td><td>Ten - Retail - [Digital] (RoW)</td><td class="license_acquisition_col">Retail</td></tr>
                <tr><td class="license_date_col">Feb 2, 2019</td><td>Twenty</td><td class="license_acquisition_col">Steam Store</td></tr>
                <tr><td class="license_date_col">Oct 1, 2017</td><td>Thirty (WW)</td><td class="license_acquisition_col">Retail</td></tr></table>"#;
            Ok(crate::net::Response { status: 200, retry_after: None, body: body.into() })
        }
        fn get_final(&self, url: &str, _h: &[(&str, &str)], _t: std::time::Duration) -> Result<(crate::net::Response, String), crate::net::NetError> {
            panic!("the Help pages must not be asked: {url}")
        }
    }
    #[test] fn the_licenses_list_dates_a_run_of_games_without_touching_the_help_pages() {
        let (s, db) = setup("l"); Connection::open(&db).unwrap().execute("INSERT INTO games (appid, name, date_added) VALUES (30, 'Thirty', 5)", []).unwrap();
        s.set_plan(vec![(10, "Ten".into()), (20, "Twenty".into()), (30, "Thirty".into())]); let s = std::sync::Arc::new(s);
        crate::steamsignin::set_store_session("76561198000000001%7C%7Cstore");   // no Help session is needed: every game is on the licenses list
        s.run_direct(std::sync::Arc::new(Both), std::time::Duration::from_millis(1));
        assert_eq!(wait_summary(&s), "Steam dates: 3 saved, 0 not found");
        assert_eq!((added(&db, 10), added(&db, 20), added(&db, 30)), (1583020800, 1549065600, 1506816000), "all three from the licenses list");
    }
    #[test] fn the_bar_moves_while_the_licenses_list_is_read_and_a_slow_read_is_not_called_stopped() {
        let (s, _) = setup("p"); s.call("steam/plan", &json!({})).unwrap();
        assert_eq!(s.progress().map(|p| p.2), Some(0));
        { let mut r = s.run.lock().unwrap(); r.lic_matched = 1; r.last = Some(std::time::Instant::now() - IDLE_LIMIT - std::time::Duration::from_secs(5)); }
        assert_eq!(s.progress().map(|p| p.2), Some(1), "the games matched so far count");
        // a page read refreshes the sign of life, as `licenses_first` does for every page
        { let mut r = s.run.lock().unwrap(); r.last = Some(std::time::Instant::now()); }
        assert_eq!(s.tick(), None, "not stopped"); assert!(s.progress().is_some(), "and the bar is still there for a reopened dialog");
    }
}
