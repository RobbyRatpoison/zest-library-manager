//! Adding the games a Steam account owns to the library (PlayDate's `scrapers._add_new`, phase 1): the owned list from the Steam Web API when there is a key, else from Steam's own
//! files (`localconfig.vdf` for playtime, app manifests and `appinfo.vdf` for names); only games not already in the library or on the blacklist, only real games (not tools, demos or
//! soundtracks), newest first; one placeholder row each. The caller then runs the fill-in jobs (store data, reviews, tags, art, ProtonDB, HLTB) on the returned ids.
use crate::steamclient::Steam;
use crate::steamlocal as sl;
use rusqlite::{params, Connection};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// One game as the list gave it.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate { pub appid: i64, pub name: String, pub playtime_forever: i64, pub last_played: Option<i64>, pub icon_hash: String }

/// A game to insert.
#[derive(Clone, Debug, PartialEq)]
pub struct NewGame { /// The date Steam's licenses list gave, when it had one (else the insert's `now`, until the Help page gives the real one).
    pub date_added: Option<i64>, pub appid: i64, pub name: String, pub playtime_forever: i64, pub last_played: Option<i64>, pub icon_hash: String, pub status: &'static str, pub installed: bool }

/// Phase 1b: the list walked from its end (so the newest purchase is added first), minus what the library already has, what was removed on purpose and what `appinfo.vdf` says is
/// not a game (an app Steam does not describe is taken as a game).
pub fn new_games(cands: &[Candidate], existing: &HashSet<i64>, blacklisted: &HashSet<i64>, kind: &dyn Fn(i64) -> Option<String>, installed: &HashSet<i64>) -> Vec<NewGame> {
    cands.iter().rev().filter(|c| !existing.contains(&c.appid) && !blacklisted.contains(&c.appid) && kind(c.appid).map_or(true, |k| k.eq_ignore_ascii_case("game")))
        .map(|c| { let pt = c.playtime_forever.max(0); NewGame { date_added: None, appid: c.appid, name: c.name.clone(), playtime_forever: pt, last_played: c.last_played, icon_hash: c.icon_hash.clone(),
            status: if pt > 0 { "Unfinished" } else { "Never Played" }, installed: installed.contains(&c.appid) } }).collect()
}

/// `batch_insert_placeholder_games`: one transaction, rows already there are skipped, every fetch phase starts at `'0'`.
pub fn insert_placeholders(conn: &mut Connection, games: &[NewGame], now: i64) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    {
        let mut st = tx.prepare("INSERT OR IGNORE INTO games (appid, name, playtime_forever, last_played, date_added, completion_status, installed, icon_hash, platform, art_fetched, meta_fetched, cheevos_fetched, protondb_fetched, hltb_fetched)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'steam', '0', '0', '0', '0', '0')")?;
        for g in games { st.execute(params![g.appid, g.name, g.playtime_forever, g.last_played, g.date_added.unwrap_or(now), g.status, g.installed as i64, g.icon_hash])?; }
    }
    tx.commit()
}

/// Phase 1a: the owned games. With a key the Web API (names and icons included); without one Steam's own files, which know only what was played or installed.
pub fn candidates(steam: &Steam, steam_root: Option<&Path>, steam_id: &str, appinfo: &HashMap<i64, sl::AppInfo>) -> Result<Vec<Candidate>, String> {
    if steam.has_key() { return steam.owned_games().map(|v| v.into_iter().map(|g| Candidate { appid: g.appid, name: g.name, playtime_forever: g.playtime_forever, last_played: g.last_played, icon_hash: g.icon_hash }).collect()); }
    let root = steam_root.ok_or("Could not read the library from Steam's files. Make sure Steam is installed and has been started at least once, or add a Steam API key in Settings.")?;
    let local = sl::local_library(root, Some(steam_id));
    if local.is_empty() { return Err("Could not read the library from Steam's files. Make sure Steam is installed and has been started at least once, or add a Steam API key in Settings.".into()); }
    let names: HashMap<i64, String> = sl::manifests(root).into_iter().map(|m| (m.appid, m.name)).collect();
    Ok(local.into_iter().map(|g| Candidate { appid: g.appid, name: names.get(&g.appid).cloned().filter(|n| !n.is_empty()).or_else(|| appinfo.get(&g.appid).map(|a| a.name.clone())).unwrap_or_default(),
        playtime_forever: g.playtime_forever, last_played: g.last_played, icon_hash: String::new() }).collect())
}

/// The store's owned-apps list as a second source (`cookie` = the store session of the Steam sign-in): the games it lists that the first list lacks, and only games
/// (`steamowned::store_only`, then the untyped few looked up in the store, each answer remembered for a month). A failure only logs; the first list still stands.
pub fn store_extras(cookie: &str, cands: &[Candidate], appinfo: &HashMap<i64, sl::AppInfo>, http: &dyn crate::net::Http, checked_file: &Path, now: i64) -> Vec<Candidate> {
    let owned = match crate::steamowned::fetch_owned_apps(http, cookie) { Ok(o) => o, Err(e) => { crate::net::log(&format!("owned apps: {e}")); return vec![] } };
    let (mut extra, unknown) = crate::steamowned::store_only(&owned, cands, appinfo);
    let checked = crate::steamowned::load_checked(checked_file, now);
    let to_ask: Vec<i64> = unknown.iter().filter(|id| !checked.contains(id)).copied().collect();
    let (kept, settled) = crate::steamowned::resolve_unknown(http, &to_ask);
    extra.extend(kept);
    if !settled.is_empty() { crate::steamowned::save_checked(checked_file, &settled, now); }
    crate::net::log(&format!("owned apps: the store lists {} apps; {} more games than the Web API ({} apps had no type, {} looked up now)", owned.len(), extra.len(), unknown.len(), to_ask.len()));
    extra
}

/// New games wait with `date_added = 0` ("no date": sorts last, groups under no year) until the store's licenses list gives the real one, so none shows at the front of a recently-added
/// list under today's date and then jumps back. This reads the list (newest-added appids asked for first) and writes each matched game's date; a game it cannot match gets today's date
/// (first seen) so none stays undated, and is returned: the Help pages may still know its real date. A list that cannot be read leaves every game that way.
pub fn date_from_licenses(db: &Path, wanted: &[(i64, String)], cookie: &str, http: &dyn crate::net::Http, pause: std::time::Duration, now: i64) -> Vec<i64> {
    if wanted.is_empty() { return vec![]; }
    let mut ordered = wanted.to_vec(); ordered.sort_by(|a, b| b.0.cmp(&a.0));
    let found = match crate::steamlicenses::read_dates(http, cookie, &ordered, if ordered.len() >= 100 { 60 } else { 5 }, pause) {
        Ok(f) => { crate::net::log(&format!("populate: the licenses list dated {} of {} new games", f.len(), ordered.len())); f }
        Err(e) => { crate::net::log(&format!("populate: the licenses list could not be read ({e}); new games keep today's date until the Help pages give theirs")); HashMap::new() }
    };
    let Ok(conn) = Connection::open(db) else { return ordered.iter().map(|(a, _)| *a).collect() };
    conn.busy_timeout(std::time::Duration::from_secs(10)).ok();
    let mut undated = Vec::new();
    for (appid, _) in &ordered {
        let ts = found.get(appid).and_then(|d| crate::date_to_ts(&serde_json::Value::String(d.clone())));
        let _ = conn.execute("UPDATE games SET date_added = ?1 WHERE appid = ?2 AND date_added = 0", params![ts.unwrap_or(now), appid]);
        if ts.is_none() { undated.push(*appid); }
    }
    undated
}

/// What the background half of a sync (the store's list and the dates) found, for whoever asks next.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Late { /// Games the store's list added after the sync itself finished.
    pub added: Vec<i64>, /// New games the licenses list could not date (the Help pages may).
    pub undated: Vec<i64>, /// How many games were up for dating (the first half's and the store list's).
    pub total: usize }
static LATE: std::sync::Mutex<Option<Late>> = std::sync::Mutex::new(None);
static BACKGROUND: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static BACKGROUND_STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// The background half's result, once, when it has finished.
pub fn take_late() -> Option<Late> { LATE.lock().ok().and_then(|mut g| g.take()) }
/// Is the background half running now?
pub fn background_running() -> bool { BACKGROUND.load(std::sync::atomic::Ordering::SeqCst) }
/// Did the last sync start a background half (once)? Then it dates its games itself and the caller must not read their Help pages as well.
pub fn take_background_started() -> bool { BACKGROUND_STARTED.swap(false, std::sync::atomic::Ordering::SeqCst) }

/// The games the last populate added, for whoever asks next (the app's toast and the purchase-date reads must count these, not every game whose details are still missing).
static LAST_ADDED: std::sync::Mutex<Vec<i64>> = std::sync::Mutex::new(Vec::new());
pub fn set_added(ids: &[i64]) { if let Ok(mut g) = LAST_ADDED.lock() { *g = ids.to_vec(); } }
pub fn take_added() -> Vec<i64> { LAST_ADDED.lock().map(|mut g| std::mem::take(&mut *g)).unwrap_or_default() }

/// The whole of phase 1: asks Steam, filters, inserts. Returns the ids to fill in, newest first (empty = nothing new). With a store session the games come in at once with no date
/// (0) and a background half (`finish_in_background`) adds what the store's list has beyond the Web API's and dates everything from the licenses list.
pub fn run(db: &Path, steam: &Steam, steam_root: Option<&Path>, steam_id: &str, now: i64) -> Result<Vec<i64>, String> {
    let appinfo = steam_root.map(sl::read_appinfo).unwrap_or_default();
    let cands = candidates(steam, steam_root, steam_id, &appinfo)?;
    let http = crate::net::UreqHttp::new();
    if crate::steamsignin::store_session().is_none() { crate::steamsession::restore(&http, &crate::steamsession::SecretTool, steam_id); }   // a saved sign-in is traded for a session
    let cookie = crate::steamsignin::store_session();
    let mut conn = Connection::open(db).map_err(|e| e.to_string())?;
    conn.busy_timeout(std::time::Duration::from_secs(10)).ok();
    let ids = |sql: &str| -> HashSet<i64> { conn.prepare(sql).ok().map(|mut s| s.query_map([], |r| r.get::<_, i64>(0)).map(|r| r.flatten().collect()).unwrap_or_default()).unwrap_or_default() };
    let (existing, black) = (ids("SELECT appid FROM games"), ids("SELECT appid FROM blacklist"));
    let installed: HashSet<i64> = steam_root.map(|r| sl::installed_appids(&sl::manifests(r)).into_iter().collect()).unwrap_or_default();
    let kind = |id: i64| appinfo.get(&id).map(|a| a.kind.clone());
    let mut fresh = new_games(&cands, &existing, &black, &kind, &installed);
    if cookie.is_some() { for g in fresh.iter_mut() { g.date_added = Some(0); } }
    insert_placeholders(&mut conn, &fresh, now).map_err(|e| e.to_string())?;
    let first: Vec<(i64, String)> = fresh.iter().map(|g| (g.appid, g.name.clone())).collect();
    if let Some(cookie) = cookie {
        let mut have = existing; have.extend(first.iter().map(|(a, _)| *a));
        BACKGROUND_STARTED.store(true, std::sync::atomic::Ordering::SeqCst);
        finish_in_background(db.to_path_buf(), cookie, cands, appinfo, have, black, installed, first.clone(), now);
    }
    Ok(first.into_iter().map(|(a, _)| a).collect())
}

/// The second half of a sync, on its own thread: the games only the store's list knows (inserted undated, like the first half's), then the licenses list dates all of them.
#[allow(clippy::too_many_arguments)]
fn finish_in_background(db: std::path::PathBuf, cookie: String, cands: Vec<Candidate>, appinfo: HashMap<i64, sl::AppInfo>, have: HashSet<i64>, black: HashSet<i64>, installed: HashSet<i64>, first: Vec<(i64, String)>, now: i64) {
    BACKGROUND.store(true, std::sync::atomic::Ordering::SeqCst);
    std::thread::spawn(move || {
        let http = crate::net::UreqHttp::new();
        crate::net::set_activity("Reading your Steam library: the store's list");
        let extras = store_extras(&cookie, &cands, &appinfo, &http, &db.with_file_name("owned_checked.json"), now);
        let kind = |id: i64| appinfo.get(&id).map(|a| a.kind.clone());
        let mut more = new_games(&extras, &have, &black, &kind, &installed);
        for g in more.iter_mut() { g.date_added = Some(0); }
        let mut added = Vec::new();
        if !more.is_empty() {
            if let Ok(mut conn) = Connection::open(&db) { conn.busy_timeout(std::time::Duration::from_secs(10)).ok(); if insert_placeholders(&mut conn, &more, now).is_ok() { added = more.iter().map(|g| g.appid).collect(); } }
        }
        let wanted: Vec<(i64, String)> = first.into_iter().chain(more.iter().map(|g| (g.appid, g.name.clone()))).collect();   // (kept whole: `total` below counts it)
        crate::net::set_activity("Reading your Steam library: dates");
        let undated = date_from_licenses(&db, &wanted, &cookie, &http, std::time::Duration::from_millis(2500), now);
        crate::net::set_activity("");
        if let Ok(mut g) = LATE.lock() { *g = Some(Late { added, undated, total: wanted.len() }); }
        BACKGROUND.store(false, std::sync::atomic::Ordering::SeqCst);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    fn c(id: i64, name: &str, pt: i64) -> Candidate { Candidate { appid: id, name: name.into(), playtime_forever: pt, last_played: if pt > 0 { Some(1700000000) } else { None }, icon_hash: "h".into() } }
    #[test] fn new_games_skip_owned_removed_and_non_games_and_come_newest_first() {
        let cands = vec![c(1, "Old", 0), c(2, "Have", 5), c(3, "Removed", 0), c(4, "Tool", 0), c(5, "Newest", 120), c(6, "NoInfo", 0)];
        let kinds: HashMap<i64, &str> = [(1, "game"), (4, "tool"), (5, "Game")].into_iter().collect();
        let out = new_games(&cands, &[2].into_iter().collect(), &[3].into_iter().collect(), &|id| kinds.get(&id).map(|k| k.to_string()), &[5].into_iter().collect());
        assert_eq!(out.iter().map(|g| g.appid).collect::<Vec<_>>(), vec![6, 5, 1], "reversed, an app with no appinfo entry counts as a game");
        let g5 = &out[1]; assert_eq!((g5.status, g5.installed, g5.playtime_forever), ("Unfinished", true, 120));
        assert_eq!((out[0].status, out[0].installed), ("Never Played", false));
    }
    #[test] fn pending_games_get_their_license_date_or_today_and_the_unmatched_are_reported() {
        struct Site; impl crate::net::Http for Site { fn get(&self, _u: &str, _h: &[(&str, &str)], _t: std::time::Duration) -> Result<crate::net::Response, crate::net::NetError> {
            Ok(crate::net::Response { status: 200, retry_after: None, body: r#"<table><tr><td class="license_date_col">Mar 1, 2020</td><td>Ten - Retail (WW)</td><td class="license_acquisition_col">Retail</td></tr></table>"#.into() }) } }
        let dir = std::env::temp_dir().join(format!("zest_pop_{}", std::process::id())); let _ = std::fs::remove_dir_all(&dir); std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("games_1.db"); crate::schema::init_db(&db).unwrap();
        let ng = |id: i64, n: &str| NewGame { date_added: Some(0), appid: id, name: n.into(), playtime_forever: 0, last_played: None, icon_hash: String::new(), status: "Never Played", installed: false };
        { let mut c = Connection::open(&db).unwrap(); insert_placeholders(&mut c, &[ng(10, "Ten"), ng(11, "Eleven"), ng(12, "Twelve")], 1_700_000_000).unwrap();
          assert_eq!(c.query_row("SELECT COUNT(*) FROM games WHERE date_added = 0", [], |r| r.get::<_, i64>(0)).unwrap(), 3, "they wait with no date"); }
        let undated = date_from_licenses(&db, &[(10, "Ten".into()), (11, "Eleven".into()), (12, "Twelve".into())], "1%7C%7Cx", &Site, std::time::Duration::from_millis(1), 1_700_000_000);
        assert_eq!(undated, vec![12, 11], "asked newest appid first; the two it cannot match come back");
        let d = |id: i64| Connection::open(&db).unwrap().query_row("SELECT date_added FROM games WHERE appid = ?", [id], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!((d(10), d(11), d(12)), (1583020800, 1_700_000_000, 1_700_000_000), "a matched game gets its license date, the others today's");
        // a date that arrived meanwhile (the Help pages) is not overwritten: only games still at 0 are touched
        Connection::open(&db).unwrap().execute("UPDATE games SET date_added = 5 WHERE appid = 12", []).unwrap();
        date_from_licenses(&db, &[(12, "Twelve".into())], "x", &Site, std::time::Duration::from_millis(1), 1_800_000_000); assert_eq!(d(12), 5);
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test] fn placeholders_are_inserted_once_with_every_phase_unfetched() {
        let mut conn = Connection::open_in_memory().unwrap(); crate::schema::ensure(&conn).unwrap();
        let g = NewGame { date_added: None, appid: 10, name: "Ten".into(), playtime_forever: 3, last_played: Some(5), icon_hash: "ic".into(), status: "Unfinished", installed: true };
        insert_placeholders(&mut conn, &[g.clone(), NewGame { date_added: None, appid: 11, name: String::new(), playtime_forever: 0, last_played: None, icon_hash: String::new(), status: "Never Played", installed: false }], 1000).unwrap();
        insert_placeholders(&mut conn, &[NewGame { name: "Changed".into(), ..g }], 2000).unwrap();   // a second time changes nothing
        let r: (String, String, i64, i64, String, String, String, String, String, i64) = conn.query_row("SELECT name, platform, installed, date_added, completion_status, art_fetched, meta_fetched, cheevos_fetched, hltb_fetched, playtime_forever FROM games WHERE appid = 10", [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?))).unwrap();
        assert_eq!(r, ("Ten".into(), "steam".into(), 1, 1000, "Unfinished".into(), "0".into(), "0".into(), "0".into(), "0".into(), 3));
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM games", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
    }
    #[test] fn without_a_key_or_steam_the_message_says_what_to_do() {
        let steam = Steam::new(std::sync::Arc::new(crate::net::UreqHttp::new()), None, 10.0);
        assert!(candidates(&steam, None, "1", &HashMap::new()).unwrap_err().contains("Steam API key"));
    }
}
