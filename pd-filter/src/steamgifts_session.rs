//! One SteamGifts wins sync as the browser drives it: the userscript reads a page in the user's logged-in tab, posts its HTML, and is told whether to read
//! another. A port of the session/page/pass2-plan/finish routes of `steamgifts.py`, kept free of any HTTP so it can be tested with the page fixtures.
//!
//! Flow: [`Session::begin`] -> [`Session::page_public`] for page 1, 2, ... while `more` -> (opt-in, only while logged in as the account)
//! [`Session::pass2_plan`] then [`Session::page_private`] for each page asked for -> [`Session::finish`], which writes the group.
use crate::blaeo::GroupSources;
use crate::steamgifts::*;
use rusqlite::Connection;
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq)]
pub enum Mode { /// The browser is logged in as the account being synced: every field, and the private pass is possible.
    Full, /// Viewing someone else's page or logged out: the public pass only (invite-only giveaways lose their code).
    Degraded }

#[derive(Debug, Clone, PartialEq)]
pub struct Begun { pub username: String, pub mode: Mode, pub pass2: bool, pub full_refresh: bool, pub public_url: String, pub public_page_size: i64,
    /// Set when the account had no SteamGifts name yet and the browser's login was adopted: the caller saves it with `account::set_sg_username`.
    pub adopted_username: Option<String> }

#[derive(Debug, Clone, PartialEq)]
pub struct PublicReply { pub parsed: usize, pub new: usize, pub changed: usize, pub more: bool, pub pages: Option<i64>, pub total: Option<i64> }

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Progress { pub public_pages: i64, pub private_pages: i64, pub new_wins: usize, pub changed_wins: usize, pub wins_total: usize }

#[derive(Debug)]
pub struct Session {
    /// The wins file's content as it is now; the caller saves it after each page (a crash then loses little).
    pub store: Value,
    pub username: String, pub mode: Mode, pub full_refresh: bool, pub pass2: bool, pub progress: Progress,
}

fn now_iso() -> String {
    let s = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let (y, m, d, _) = crate::pagywosg_filter::civil(s);
    let sod = s.rem_euclid(86400);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}+00:00", sod / 3600, sod % 3600 / 60, sod % 60)
}

impl Session {
    /// `configured`: the account's saved SteamGifts name (may be empty); `logged_in_as` / `viewing`: what the browser page shows.
    pub fn begin(store: Value, configured: &str, logged_in_as: &str, viewing: &str, full_refresh: bool, pass2: bool) -> Result<(Session, Begun), String> {
        let (configured, logged_in_as, viewing) = (configured.trim(), logged_in_as.trim(), viewing.trim());
        // first time: adopt whoever the browser is logged in as
        let adopted = (configured.is_empty() && !logged_in_as.is_empty()).then(|| logged_in_as.to_string());
        let username = if !configured.is_empty() { configured.to_string() } else if let Some(a) = &adopted { a.clone() } else if !viewing.is_empty() { viewing.to_string() } else { logged_in_as.to_string() };
        if username.is_empty() { return Err("No SteamGifts username configured.".into()); }
        let mode = if !logged_in_as.is_empty() && logged_in_as.eq_ignore_ascii_case(&username) { Mode::Full } else { Mode::Degraded };
        let pass2 = pass2 && mode == Mode::Full;
        let mut store = store;
        store["username"] = json!(username);
        let wins_total = store["wins"].as_array().map_or(0, |a| a.len());
        let begun = Begun { username: username.clone(), mode: mode.clone(), pass2, full_refresh, public_url: format!("/user/{username}/giveaways/won"), public_page_size: PUBLIC_PAGE_SIZE, adopted_username: adopted };
        Ok((Session { store, username, mode, full_refresh, pass2, progress: Progress { wins_total, ..Default::default() } }, begun))
    }

    /// One public page. `more` follows PlayDate's rules: stop on an empty page, stop when an incremental sync reaches a page that is all known and unchanged (past page 1),
    /// otherwise go on while the total says there are more pages or the page has a Next link (the total can lag SteamGifts' real count).
    pub fn page_public(&mut self, page: i64, html: &str) -> PublicReply {
        let parsed = parse_won_public(html);
        let (new, changed) = merge_public_page(&mut self.store, &parsed.wins, self.full_refresh);
        self.store["last_sync_public"] = json!(now_iso());
        if let Some(t) = parsed.total.filter(|t| *t != 0) { self.store["total"] = json!(t); }
        let all_known = new == 0 && changed == 0 && !parsed.wins.is_empty();
        let incremental_stop = all_known && !self.full_refresh && page > 1;
        let more = if parsed.wins.is_empty() || incremental_stop { false } else { parsed.pages.map_or(false, |p| p != 0 && page < p) || parsed.has_next };
        self.progress.public_pages = self.progress.public_pages.max(page);
        self.progress.new_wins += new;
        self.progress.changed_wins += changed;
        self.progress.wins_total = self.store["wins"].as_array().map_or(0, |a| a.len());
        PublicReply { parsed: parsed.wins.len(), new, changed, more, pages: parsed.pages, total: parsed.total }
    }

    /// The private pages worth reading: only when the private pass was asked for and the browser is logged in as the account.
    pub fn pass2_plan(&self) -> Option<Vec<i64>> { self.pass2.then(|| pending_pass2_pages(&self.store)) }

    /// One private page: (rows read, wins flipped to received).
    pub fn page_private(&mut self, page: i64, html: &str) -> (usize, usize) {
        let p = parse_won_private(html);
        let flipped = apply_private_rows(&mut self.store, &p.rows);
        self.store["last_sync_private"] = json!(now_iso());
        self.progress.private_pages = self.progress.private_pages.max(page);
        self.progress.changed_wins += flipped;
        (p.rows.len(), flipped)
    }

    /// Writes the group (see [`apply_wins`]); the caller saves `store` and `gs` afterwards.
    pub fn finish(&mut self, conn: &Connection, gs: &mut GroupSources, steam: &dyn SteamLookup) -> rusqlite::Result<Summary> {
        apply_wins(conn, gs, &mut self.store, self.full_refresh, steam)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{Http, NetError, Response};
    use std::time::Duration;
    fn fx(n: &str) -> String { std::fs::read_to_string(format!("{}/tests/fixtures/steamgifts/{n}", env!("CARGO_MANIFEST_DIR"))).unwrap() }
    fn begin(full: bool, pass2: bool, logged: &str) -> Session { Session::begin(empty_store(), "SampleWinner", logged, "SampleWinner", full, pass2).unwrap().0 }

    #[test] fn the_username_is_adopted_once_and_the_mode_follows_who_is_logged_in() {
        let (_, b) = Session::begin(empty_store(), "", "Me", "Other", false, true).unwrap();
        assert_eq!((b.username.as_str(), b.mode.clone(), b.pass2, b.adopted_username.as_deref()), ("Me", Mode::Full, true, Some("Me")));
        assert_eq!(b.public_url, "/user/Me/giveaways/won");
        let (_, b) = Session::begin(empty_store(), "Winner", "someone else", "Winner", false, true).unwrap();
        assert_eq!((b.mode, b.pass2, b.adopted_username), (Mode::Degraded, false, None), "the private pass needs the right login");
        let (_, b) = Session::begin(empty_store(), "winner", "WINNER", "", false, false).unwrap();
        assert_eq!(b.mode, Mode::Full, "names compare without case");
        let (_, b) = Session::begin(empty_store(), "", "", "Viewed", false, false).unwrap();
        assert_eq!((b.username.as_str(), b.mode), ("Viewed", Mode::Degraded));
        assert!(Session::begin(empty_store(), "", "", "", false, false).unwrap_err().contains("No SteamGifts username"));
    }

    #[test] fn pages_run_until_the_last_and_an_incremental_sync_stops_at_the_first_known_page() {
        let mut s = begin(false, false, "SampleWinner");
        let r1 = s.page_public(1, &fx("won_public_page1.html"));
        assert_eq!((r1.parsed, r1.new, r1.more, r1.total), (6, 6, true, Some(9)));
        let r2 = s.page_public(2, &fx("won_public_page2.html"));
        assert_eq!((r2.parsed, r2.new, r2.more), (3, 3, false), "page 2 is the last: no Next link, and page 2 is not before the total's last page");
        assert_eq!(s.progress, Progress { public_pages: 2, private_pages: 0, new_wins: 9, changed_wins: 0, wins_total: 9 });
        assert_eq!(s.store["total"], 9);
        // a second sync: page 1 is all known, but it is page 1 so the sync goes on; page 2 known stops
        let mut again = Session::begin(s.store.clone(), "SampleWinner", "SampleWinner", "", false, false).unwrap().0;
        assert!(again.page_public(1, &fx("won_public_page1.html")).more);
        assert!(!again.page_public(2, &fx("won_public_page2.html")).more);
        // a full refresh never stops early, and tags what it re-confirmed
        let mut full = Session::begin(s.store.clone(), "SampleWinner", "SampleWinner", "", true, false).unwrap().0;
        assert!(full.page_public(1, &fx("won_public_page1.html")).more);
        assert!(full.store["wins"].as_array().unwrap().iter().any(|w| w["_seen_this_run"] == true));
        assert!(!s.page_public(3, "<html></html>").more, "an empty page ends it");
    }

    #[test] fn the_private_pass_is_planned_only_when_asked_and_flips_awaiting_wins() {
        assert!(begin(false, false, "SampleWinner").pass2_plan().is_none());
        assert!(begin(false, true, "nobody").pass2_plan().is_none(), "degraded: no private pass");
        let mut s = begin(false, true, "SampleWinner");
        s.page_public(1, &fx("won_public_page1.html")); s.page_public(2, &fx("won_public_page2.html"));
        // CCCC3 (awaiting) and DDDD4 (not received) are unresolved: they sit on private page 1 (the plan also names the page after, capped at the last one)
        assert_eq!(s.pass2_plan(), Some(vec![1]));
        let (rows, flipped) = s.page_private(1, &fx("won_private_page1.html"));
        assert_eq!((rows, flipped), (4, 0));
        assert_eq!(s.progress.private_pages, 1);
        assert!(s.store["last_sync_private"].as_str().unwrap().ends_with("+00:00"));
    }

    struct Nothing;
    impl Http for Nothing { fn get(&self, _: &str, _: &[(&str, &str)], _: Duration) -> Result<Response, NetError> { Err(NetError("offline".into())) } }

    #[test] fn finishing_writes_the_group_for_received_wins() {
        let mut s = begin(false, false, "SampleWinner");
        s.page_public(1, &fx("won_public_page1.html")); s.page_public(2, &fx("won_public_page2.html"));
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE games (appid INTEGER PRIMARY KEY, name TEXT, `groups` TEXT, sg_dlc_win INTEGER)", []).unwrap();
        for (a, n) in [(100010, "Example Game One"), (100030, "Example Game Three"), (100070, "Example Game Six")] { conn.execute("INSERT INTO games VALUES (?, ?, '', NULL)", rusqlite::params![a, n]).unwrap(); }
        let mut gs = GroupSources::empty();
        let steam = WebSteam::new(&Nothing, Duration::ZERO);
        let sum = s.finish(&conn, &mut gs, &steam).unwrap();
        // game one and game six have received wins; game three is still awaiting feedback
        assert_eq!((sum.groups_added, sum.games_in_group), (2, 2));
        let g: Vec<(i64, String)> = conn.prepare("SELECT appid, `groups` FROM games ORDER BY appid").unwrap().query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().flatten().collect();
        assert_eq!(g, vec![(100010, "Won on SteamGifts".into()), (100030, "".into()), (100070, "Won on SteamGifts".into())]);
    }
}
