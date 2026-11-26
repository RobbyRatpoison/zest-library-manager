//! The job runner: a pool of worker threads over a `WorkQueue`, one `PoolBackoff` gate, cancel and a progress callback, shaped like
//! `bulk_rescrape_games` (3 workers, 0.5 s between games, a 429 re-queues the game and the whole pool waits). The per-game work for a Steam game is
//! `rescrape_steam`; what it returns is a list of column updates, written by `apply`.
use crate::net::{Cancel, PoolBackoff, Wait, WorkQueue};
use crate::steamclient::{Listing, AppinfoDates, Fetch, Steam};
use rusqlite::{types::Value as Sql, Connection};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A column update, in the order Python's dict had it.
pub type Update = Vec<(&'static str, Sql)>;
fn t(s: impl Into<String>) -> Sql { Sql::Text(s.into()) }
fn i(n: i64) -> Sql { Sql::Integer(n) }
fn oi(n: Option<i64>) -> Sql { n.map_or(Sql::Null, Sql::Integer) }

/// `bulk_rescrape_games`' Steam branch: store data, reviews, tags and (with a key) achievements; `None` when nothing came back (counted as failed).
/// The same 429 handling as Python: any of the four requests can raise it and the whole game is retried later.
pub fn rescrape_steam(steam: &Steam, appid: i64, dates: AppinfoDates, today: &str) -> Fetch<Option<Update>> { rescrape_steam_with(steam, appid, dates, today, true, true) }

/// `rescrape_steam` with the two choices of the Re-scrape tab: store info and reviews (store data, reviews, tags), and achievements (needs the account's key).
pub fn rescrape_steam_with(steam: &Steam, appid: i64, dates: AppinfoDates, today: &str, meta: bool, cheevo: bool) -> Fetch<Option<Update>> { rescrape_steam_phase(steam, appid, dates, today, meta, cheevo, Phase::All) }

/// Which part of a Steam game's re-scrape to run. A batch of new games is done in two passes so every card gets its name, developer, genres, date and description first (`Core`:
/// the store page only), and the slower reviews, tags and achievements follow (`Rest`). `All` is the whole thing for one game.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum Phase { #[default] All, Core, Rest }

pub fn rescrape_steam_phase(steam: &Steam, appid: i64, dates: AppinfoDates, today: &str, meta: bool, cheevo: bool, phase: Phase) -> Fetch<Option<Update>> {
    let store = if meta && phase != Phase::Rest { steam.store_data(appid, dates)? } else { None };
    let reviews = if meta && phase != Phase::Core { steam.reviews(appid)? } else { None };
    let tags = if meta && phase != Phase::Core { steam.tags(appid)? } else { None };
    let cheevos = if cheevo && phase != Phase::Core && steam.has_key() { steam.cheevos(appid)? } else { None };
    let mut u: Update = Vec::new();
    if let Some(s) = &store {
        u.push(("name_from_store", i(1)));
        let d = &s.data;
        u.extend([("name", t(&d.name)), ("developers", t(&d.developers)), ("publishers", t(&d.publishers)), ("release_date", oi(s.release_date)), ("genres", t(&d.genres)),
                  ("categories", t(&d.categories)), ("is_free", i(d.is_free)), ("metacritic_score", oi(d.metacritic_score))]);
        if let Some(sd) = &d.short_description { u.push(("short_description", t(sd))); }
    }
    if let Some(r) = &reviews {
        u.extend([("review_score", t(&r.review_score)), ("review_percentage", i(r.review_percentage)), ("weighted_percentage", i(r.weighted_percentage)),
                  ("total_reviews", i(r.total_reviews)), ("positive_reviews", i(r.positive_reviews))]);
    }
    if let Some(tg) = &tags { u.push(("tags", t(tg.join(",")))); }
    if let Some(c) = &cheevos {
        u.extend([("total_achievements", i(c.total)), ("unlocked_achievements", i(c.unlocked))]);
        if c.completed { u.push(("completion_status", t("Completed"))); }
    }
    // the first pass of a batch does not stamp the game as done (the second pass does, see `bulk_rescrape`), so a game whose reviews and tags were never read is read again next time
    if phase != Phase::Core && (store.is_some() || reviews.is_some() || tags.is_some()) { u.push(("meta_fetched", t(today))); }
    else if meta && phase != Phase::Rest && store.is_none() && matches!(steam.store_listing(appid)?, Listing::Gone) {
        // nothing came back and the store page itself is gone (a delisted or removed game): that is an answer, so the game counts as tried today and is not queued again after every sync,
        // as PlayDate does when it adds a game. A failed request or an unclear answer is not a removal and is tried again later.
        u.push(("meta_fetched", t(today)));
    }
    if cheevos.is_some() { u.push(("cheevos_fetched", t(today))); }
    Ok((!u.is_empty()).then_some(u))
}

/// Playtime and last played for one Steam game: the web API's answer when there is a key (a failed call is not an error here), else Steam's own files.
pub fn playtime_update(steam: &Steam, appid: i64, local: &std::collections::HashMap<i64, (i64, Option<i64>)>) -> Update {
    let api = steam.player_data(appid).ok().flatten();
    let own = local.get(&appid).copied();
    let playtime = api.as_ref().map(|g| g.playtime_forever).or(own.map(|l| l.0));
    let last = api.as_ref().and_then(|g| g.last_played).filter(|t| *t > 0).or(own.and_then(|l| l.1));
    let mut u: Update = Vec::new();
    if let Some(p) = playtime { u.push(("playtime_forever", i(p))); }
    if let Some(l) = last { u.push(("last_played", i(l))); }
    u
}

/// `update_game_data`: writes every given column of one game. Column names are `&'static str` from this crate, never user input.
pub fn apply(conn: &Connection, appid: i64, u: &Update) -> rusqlite::Result<()> {
    if u.is_empty() { return Ok(()); }
    let sql = format!("UPDATE games SET {} WHERE appid = ?", u.iter().map(|(c, _)| format!("{c} = ?")).collect::<Vec<_>>().join(", "));
    let mut p: Vec<&dyn rusqlite::ToSql> = u.iter().map(|(_, v)| v as &dyn rusqlite::ToSql).collect();
    p.push(&appid);
    conn.execute(&sql, p.as_slice()).map(|_| ())
}

/// `_sweep_achievement_completion_status`: completion status follows the stored achievement counts (Steam rows only).
pub fn sweep_achievement_status(conn: &Connection, promote: bool, downgrade: bool) -> rusqlite::Result<(usize, usize)> {
    let up = if promote { conn.execute("UPDATE games SET completion_status = 'Completed' WHERE total_achievements > 0 AND unlocked_achievements = total_achievements AND completion_status != 'Completed' AND (platform = 'steam' OR platform IS NULL)", [])? } else { 0 };
    let down = if downgrade { conn.execute("UPDATE games SET completion_status = 'Beaten' WHERE total_achievements > 0 AND unlocked_achievements < total_achievements AND completion_status = 'Completed' AND (platform = 'steam' OR platform IS NULL)", [])? } else { 0 };
    Ok((up, down))
}

// ---------------------------------------------------------------- the pool

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A game finished (`done`), or came back with nothing (`failed`). `total` is the size of the whole job.
    Done { appid: i64, total: usize }, Failed { appid: i64, total: usize },
    /// A 429: the pool is waiting (`Wait` says how long and which attempt).
    RateLimit(Wait),
}
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Counts { pub done: usize, pub failed: usize, pub aborted: bool, pub cancelled: bool }

pub enum Outcome { Done, Failed, RateLimited(Option<f64>) }

pub struct PoolCfg { pub name: &'static str, pub workers: usize, /// Pause after each game (0.5 s for the bulk re-scrape).
    pub gap: Duration, /// When set, decides each pause instead (see `GapPolicy`).
    pub policy: Option<Arc<GapPolicy>> }

/// The art job's pause. A game the player is looking at is fetched with a short pause so its cover turns up quickly; every second saved that way is owed, and the games after it
/// wait a little longer until the debt is paid, so over a whole run the request rate stays what the normal pause allows (the art sources rate-limit). A pause is never more than twice
/// the normal one.
pub struct GapPolicy { base: Duration, fast: Duration, fast_rank: u32, rank: Arc<dyn Fn(&i64) -> u32 + Send + Sync>, debt: Mutex<Duration> }
impl GapPolicy {
    pub fn new(base: Duration, fast: Duration, fast_rank: u32, rank: Arc<dyn Fn(&i64) -> u32 + Send + Sync>) -> Self { GapPolicy { base, fast, fast_rank, rank, debt: Mutex::new(Duration::ZERO) } }
    /// The pause to take after finishing `appid`.
    pub fn after(&self, appid: i64) -> Duration {
        let mut debt = self.debt.lock().unwrap();
        if (self.rank)(&appid) <= self.fast_rank { *debt += self.base.saturating_sub(self.fast); self.fast }
        else { let pay = (*debt).min(self.base); *debt -= pay; self.base + pay }
    }
}

/// Runs `process` for every item on `cfg.workers` threads and returns when the queue is empty, the job is cancelled or the backoff gave up.
/// A rate-limited item is put back on the queue (`push`, like the bulk re-scrape; use `push_priority` on the queue yourself for populate-style pools).
pub fn run_pool<T: Send + Clone + 'static>(queue: Arc<WorkQueue<T>>, cfg: PoolCfg, cancel: Cancel, process: Arc<dyn Fn(&T) -> Outcome + Send + Sync>,
                                            id_of: Arc<dyn Fn(&T) -> i64 + Send + Sync>, on_event: Arc<dyn Fn(Event) + Send + Sync>) -> Counts {
    let total = queue.len();
    let backoff = Arc::new(PoolBackoff::new(cfg.name));
    let counts = Arc::new(Mutex::new(Counts::default()));
    let active = Arc::new(AtomicUsize::new(0));
    let handles: Vec<_> = (0..cfg.workers.max(1)).map(|_| {
        let (queue, backoff, cancel, process, id_of, on_event, counts, active) = (queue.clone(), backoff.clone(), cancel.clone(), process.clone(), id_of.clone(), on_event.clone(), counts.clone(), active.clone());
        let (gap, policy) = (cfg.gap, cfg.policy.clone());
        let pause = move |appid: i64| policy.as_ref().map_or(gap, |p| p.after(appid));
        std::thread::spawn(move || {
            active.fetch_add(1, Ordering::SeqCst);
            loop {
                if cancel.is_set() { return; }
                let Some(item) = queue.next() else { return };
                if !backoff.wait_ready(&cancel) { return; }
                let appid = id_of(&item);
                match process(&item) {
                    Outcome::Done => { counts.lock().unwrap().done += 1; on_event(Event::Done { appid, total }); sleep_unless(&cancel, pause(appid)); }
                    Outcome::Failed => { counts.lock().unwrap().failed += 1; on_event(Event::Failed { appid, total }); sleep_unless(&cancel, pause(appid)); }
                    Outcome::RateLimited(ra) => {
                        queue.push(item);
                        on_event(Event::RateLimit(backoff.preview(ra)));
                        if !backoff.on_rate_limited(&cancel, ra) { counts.lock().unwrap().failed += 1; return; }
                    }
                }
            }
        })
    }).collect();
    for h in handles { let _ = h.join(); }
    let mut c = counts.lock().unwrap().clone();
    c.aborted = backoff.aborted(); c.cancelled = cancel.is_set();
    c
}

fn sleep_unless(cancel: &Cancel, d: Duration) {
    let end = std::time::Instant::now() + d;
    while !cancel.is_set() { let now = std::time::Instant::now(); if now >= end { return; } std::thread::sleep((end - now).min(Duration::from_millis(100))); }
}

// ---------------------------------------------------------------- the bulk re-scrape

pub struct Rescrape {
    pub db: std::path::PathBuf, pub root: std::path::PathBuf, pub steam: Arc<Steam>, pub http: Arc<dyn crate::net::Http>, pub sources: Arc<crate::sources::Registry>,
    pub appinfo: Arc<dyn Fn(i64) -> AppinfoDates + Send + Sync>, pub today: String, pub account_id: Option<String>,
    /// The Re-scrape tab's choices for a Steam game: store info and reviews, achievements.
    pub meta: bool, pub cheevo: bool, /// Which part of a Steam game to read (`Phase::All` unless a batch is done in two passes).
    pub phase: Phase,
    /// Steam's own files' playtime and last-played time per game (read once), the fallback when the web API cannot say.
    pub local_play: Arc<std::collections::HashMap<i64, (i64, Option<i64>)>>,
}

/// `bulk_rescrape_games`: 3 workers, 0.5 s between games. A Steam game is re-scraped from Steam; any other game goes to its source's `rescrape` (if it has one)
/// and then through the metadata backfill (PCGamingWiki, Steam search; `rerun`, so a game stamped done or no_match is tried again), the source's data winning,
/// as in Python. A game whose platform has no source and nothing to backfill counts as failed.
pub fn bulk_rescrape(job: Rescrape, appids: Vec<i64>, cancel: Cancel, on_event: Arc<dyn Fn(Event) + Send + Sync>) -> Counts {
    use crate::metadata::{backfill, Backfill, Pcgw, Row};
    use crate::sources::{GameRef, SourceCtx};
    let job = Arc::new(job);
    let pcgw = Arc::new(Pcgw::new(job.http.clone()));
    let cx = Arc::new(SourceCtx { http: job.http.clone(), steam: job.steam.clone(), root: job.root.clone(), db: job.db.clone(), today: job.today.clone(), appinfo: job.appinfo.clone(), account_id: job.account_id.clone() });
    let queue = Arc::new(WorkQueue::new(appids));
    let j = job.clone();
    let process: Arc<dyn Fn(&i64) -> Outcome + Send + Sync> = Arc::new(move |&appid| {
        let Ok(conn) = Connection::open(&j.db) else { return Outcome::Failed };
        let _ = conn.busy_timeout(Duration::from_secs(10));
        let platform: String = conn.query_row("SELECT COALESCE(platform,'steam') FROM games WHERE appid = ?", [appid], |r| r.get(0)).unwrap_or_else(|_| "steam".into());
        let result: Fetch<Option<Update>> = if platform == "steam" && appid > 0 {
            // `scrape_single` also refreshes playtime and last played: the web API when there is a key, else Steam's own files
            // the second pass of a batch: a game whose first pass did not read the store page is left alone (that pass already counted it), one that did gets its reviews, tags and achievements and is then stamped done
            let core_ok: i64 = conn.query_row("SELECT COALESCE(name_from_store, 0) FROM games WHERE appid = ?", [appid], |r| r.get(0)).unwrap_or(0);
            if j.phase == Phase::Rest && core_ok != 1 { return Outcome::Done; }
            rescrape_steam_phase(&j.steam, appid, (j.appinfo)(appid), &j.today, j.meta, j.cheevo, j.phase).map(|o| {
                let mut u = o.unwrap_or_default();
                if j.phase != Phase::Rest { u.extend(playtime_update(&j.steam, appid, &j.local_play)); }
                if j.phase == Phase::Rest && j.meta && !u.iter().any(|(c, _)| *c == "meta_fetched") { u.push(("meta_fetched", t(&j.today))); }
                (!u.is_empty()).then_some(u)
            })
        } else {
            let (Some(row), Some(gr)) = (Row::load(&conn, appid), conn.query_row("SELECT COALESCE(name,''), COALESCE(platform_id,''), COALESCE(platform_slug,''), COALESCE(platform_appname,''), COALESCE(installed,0), install_path FROM games WHERE appid = ?", [appid],
                |r| Ok(GameRef { appid, name: r.get(0)?, platform: platform.clone(), platform_id: r.get::<_, rusqlite::types::Value>(1).map(|v| match v { Sql::Text(t) => t, Sql::Integer(i) => i.to_string(), _ => String::new() })?, slug: r.get(2)?, appname: r.get(3)?, installed: r.get::<_, i64>(4)? != 0, install_path: r.get::<_, Option<String>>(5)?.filter(|p| !p.is_empty()), executable: None })).ok()) else { return Outcome::Failed };
            let from_source = match j.sources.for_game(&platform, &gr.platform_id) { Some(s) => match s.rescrape(&cx, &gr) { Ok(m) => m.unwrap_or_default(), Err(e) => return Outcome::RateLimited(e.0) }, None => vec![] };
            let b = Backfill { steam: &j.steam, http: j.http.as_ref(), pcgw: &pcgw, plugins: j.sources.as_ref(), today: &j.today };
            match backfill(&b, appid, &row, &*j.appinfo, false, true) {
                Err(e) => return Outcome::RateLimited(e.0),
                Ok(back) => {
                    // the backfill's columns first, the source's over them (plugin data wins), each column once
                    let mut merged: Update = back.unwrap_or_default();
                    for (c, v) in from_source { if let Some(slot) = merged.iter_mut().find(|(k, _)| *k == c) { slot.1 = v; } else { merged.push((c, v)); } }
                    Ok((!merged.is_empty()).then_some(merged))
                }
            }
        };
        match result {
            Err(e) => Outcome::RateLimited(e.0),
            Ok(None) => Outcome::Failed,
            Ok(Some(u)) => match apply(&conn, appid, &u) {
                Ok(()) => Outcome::Done,
                Err(e) => { crate::net::log(&format!("[bulk_rescrape] database update failed for {appid}: {e}")); Outcome::Failed }
            },
        }
    });
    // the first pass of a batch asks only Steam's store details endpoint, which allows about 200 requests per 5 minutes: three workers each pausing 4.5 s is about 40 a minute, under that
    // (at the normal 0.5 s pause the first pass ran into the limit after about 200 games and every worker then sat in the long wait)
    let gap = if job.phase == Phase::Core { Duration::from_millis(4500) } else { Duration::from_millis(500) };
    run_pool(queue, PoolCfg { name: "bulk_rescrape", workers: 3, gap, policy: None }, cancel, process, Arc::new(|a| *a), on_event)
}

/// `bulk_protondb_scrape_games`: tier and confidence for each Steam game, 2 workers, no delay. A missing page or `pending` clears the stored tier
/// (stamped fetched); a failed request writes nothing, so a network error never wipes data (Python did).
pub fn bulk_protondb(db: std::path::PathBuf, steam: Arc<Steam>, today: String, appids: Vec<i64>, cancel: Cancel, on_event: Arc<dyn Fn(Event) + Send + Sync>) -> Counts {
    let queue = Arc::new(WorkQueue::new(appids));
    let process: Arc<dyn Fn(&i64) -> Outcome + Send + Sync> = Arc::new(move |&appid| {
        let (tier, conf) = match steam.protondb(appid) {
            Ok(Some((t, c))) => (Sql::Text(t), c.map_or(Sql::Null, Sql::Text)),
            Ok(None) => (Sql::Null, Sql::Null),
            Err(e) => { crate::net::log(&format!("[protondb] fetch failed for {appid}: {}", e.0)); return Outcome::Failed; }
        };
        let u: Update = vec![("protondb_fetched", t(&today)), ("protondb_tier", tier), ("protondb_confidence", conf)];
        match Connection::open(&db).and_then(|c| { c.busy_timeout(Duration::from_secs(10))?; apply(&c, appid, &u) }) { Ok(()) => Outcome::Done, Err(e) => { crate::net::log(&format!("[bulk_protondb] database update failed for {appid}: {e}")); Outcome::Failed } }
    });
    run_pool(queue, PoolCfg { name: "bulk_protondb", workers: 2, gap: Duration::ZERO, policy: None }, cancel, process, Arc::new(|a| *a), on_event)
}

/// `bulk_hltb_scrape_games`: by the Steam-to-HLTB id map when the game is in it (score 100), else a name search at `threshold`; nothing found stores
/// `no_match`. A game whose HLTB could not be reached counts as failed and keeps what it has. 2 workers, 0.5 s between games.
pub fn bulk_hltb(db: std::path::PathBuf, hltb: Arc<crate::hltbclient::Hltb>, map: Arc<std::collections::HashMap<i64, i64>>, threshold: i64, today: String, appids: Vec<i64>,
                 cancel: Cancel, on_event: Arc<dyn Fn(Event) + Send + Sync>) -> Counts {
    let names: std::collections::HashMap<i64, String> = Connection::open(&db).ok().map(|c| appids.iter().filter_map(|a| c.query_row("SELECT COALESCE(name,'') FROM games WHERE appid = ?", [a], |r| r.get::<_, String>(0)).ok().map(|n| (*a, n))).collect()).unwrap_or_default();
    let queue = Arc::new(WorkQueue::new(appids));
    let process: Arc<dyn Fn(&i64) -> Outcome + Send + Sync> = Arc::new(move |&appid| {
        let name = names.get(&appid).cloned().unwrap_or_default();
        if name.is_empty() { return Outcome::Failed; }
        let ot = |n: Option<i64>| n.map_or(Sql::Null, Sql::Integer);
        let u: Update = if let Some(&hid) = map.get(&appid) {
            match hltb.fetch_by_id(&name, hid) {
                Err(_) => return Outcome::Failed,
                Ok(b) => vec![("hltb_id", i(hid)), ("hltb_fetched", t(&today)), ("hltb_match_score", i(100)), ("hltb_matched_name", b.matched_name.map_or(Sql::Null, Sql::Text)),
                              ("hltb_main", ot(b.main)), ("hltb_extras", ot(b.extras)), ("hltb_completionist", ot(b.completionist))],
            }
        } else {
            match hltb.fetch_data(&name, threshold, &today) {
                Err(_) => return Outcome::Failed,
                Ok(Some(d)) => vec![("hltb_id", i(d.id)), ("hltb_matched_name", t(&d.matched_name)), ("hltb_main", ot(d.main)), ("hltb_extras", ot(d.extras)), ("hltb_completionist", ot(d.completionist)),
                                    ("hltb_match_score", i(d.score)), ("hltb_fetched", t(&d.fetched))],
                Ok(None) => vec![("hltb_fetched", t("no_match")), ("hltb_id", Sql::Null), ("hltb_matched_name", Sql::Null), ("hltb_match_score", Sql::Null), ("hltb_main", Sql::Null), ("hltb_extras", Sql::Null), ("hltb_completionist", Sql::Null)],
            }
        };
        match Connection::open(&db).and_then(|c| { c.busy_timeout(Duration::from_secs(10))?; apply(&c, appid, &u) }) { Ok(()) => Outcome::Done, Err(e) => { crate::net::log(&format!("[bulk_hltb] database update failed for {appid}: {e}")); Outcome::Failed } }
    });
    run_pool(queue, PoolCfg { name: "bulk_hltb", workers: 2, gap: Duration::from_millis(500), policy: None }, cancel, process, Arc::new(|a| *a), on_event)
}

/// `_backfill_batch` (the manual "Fill Missing Data", rerun = true): one game at a time with `inter_delay` between games. A Steam 429 pauses once
/// (Retry-After capped at 300 s, else 30 s) and retries the game; a second 429 counts it failed. Nothing found for a game (`None`) counts as failed.
/// Non-Steam store pages and descriptions come from the game's source (`sources::Registry` as the backfill's hooks).
pub fn bulk_backfill(db: std::path::PathBuf, steam: Arc<Steam>, http: Arc<dyn crate::net::Http>, sources: Arc<crate::sources::Registry>, appinfo: Arc<dyn Fn(i64) -> AppinfoDates + Send + Sync>, today: String,
                     appids: Vec<i64>, inter_delay: Duration, cancel: Cancel, on_event: Arc<dyn Fn(Event) + Send + Sync>) -> Counts {
    use crate::metadata::{backfill, Backfill, Pcgw, Row};
    let pcgw = Pcgw::new(http.clone());
    let total = appids.len();
    let mut c = Counts::default();
    'games: for appid in appids {
        if cancel.is_set() { break; }
        for attempt in 0..2 {
            let Ok(conn) = Connection::open(&db) else { c.failed += 1; on_event(Event::Failed { appid, total }); break };
            let _ = conn.busy_timeout(Duration::from_secs(10));
            let Some(row) = Row::load(&conn, appid) else { c.failed += 1; on_event(Event::Failed { appid, total }); break };
            let b = Backfill { steam: &steam, http: http.as_ref(), pcgw: &pcgw, plugins: sources.as_ref(), today: &today };
            match backfill(&b, appid, &row, &*appinfo, false, true) {
                Ok(Some(u)) => match apply(&conn, appid, &u) {
                    Ok(()) => { c.done += 1; on_event(Event::Done { appid, total }); }
                    Err(e) => { crate::net::log(&format!("_backfill_batch: database update failed for {appid}: {e}")); c.failed += 1; on_event(Event::Failed { appid, total }); }
                },
                Ok(None) => { c.failed += 1; on_event(Event::Failed { appid, total }); }
                Err(e) if attempt == 0 => {
                    let delay = e.0.filter(|r| *r > 0.0).map_or(30.0, |r| r.min(300.0));
                    on_event(Event::RateLimit(Wait { attempt: 1, of: 1, delay_secs: delay, server_requested: e.0.map_or(false, |r| r > 0.0) }));
                    drop(conn);
                    if !sleep_cancellable(&cancel, Duration::from_secs_f64(delay)) { c.aborted = true; break 'games; }
                    continue;
                }
                Err(_) => { c.failed += 1; on_event(Event::Failed { appid, total }); }
            }
            break;
        }
        if !sleep_cancellable(&cancel, inter_delay) { break; }
    }
    c.cancelled = cancel.is_set();
    c
}

/// Sleeps `d`; false if the job was cancelled meanwhile.
fn sleep_cancellable(cancel: &Cancel, d: Duration) -> bool {
    let end = std::time::Instant::now() + d;
    loop { if cancel.is_set() { return false; } let now = std::time::Instant::now(); if now >= end { return true; } std::thread::sleep((end - now).min(Duration::from_millis(100))); }
}

/// `bulk_art_scrape_games`: 3 workers, 0.8 s between games. `orders(appid, kind)` = the source order and the prefer-fit flag the settings give that game
/// (None order = the original chain). A non-Steam game with source "steam" takes its Steam copy's art (none known = failed); other kinds follow Python.
/// Difference: a kind that came back "missing" does not overwrite the source of a cover the game already has (Python wrote 'missing' over it).
pub fn bulk_art(db: std::path::PathBuf, art: Arc<crate::artclient::Art>, orders: Arc<dyn Fn(i64, crate::artclient::Kind) -> (Option<Vec<String>>, bool) + Send + Sync>,
                kinds: Vec<crate::artclient::Kind>, source: String, today: String, appids: Vec<i64>, cancel: Cancel, on_event: Arc<dyn Fn(Event) + Send + Sync>,
                rank: Option<Arc<dyn Fn(&i64) -> u32 + Send + Sync>>) -> Counts {
    use crate::artclient::Kind;
    let queue = Arc::new(match &rank { Some(r) => WorkQueue::with_ranker(appids, r.clone()), None => WorkQueue::new(appids) });
    let policy = rank.map(|r| Arc::new(GapPolicy::new(Duration::from_millis(800), Duration::from_millis(250), crate::artfocus::VISIBLE, r)));
    let process: Arc<dyn Fn(&i64) -> Outcome + Send + Sync> = Arc::new(move |&appid| {
        let Ok(conn) = Connection::open(&db) else { return Outcome::Failed };
        let _ = conn.busy_timeout(Duration::from_secs(10));
        let (name, icon_hash): (String, String) = conn.query_row("SELECT COALESCE(name,''), COALESCE(icon_hash,'') FROM games WHERE appid = ?", [appid], |r| Ok((r.get(0)?, r.get(1)?))).unwrap_or_default();
        let col = |k: Kind| match k { Kind::Vertical => "vertical_art_source", Kind::Horizontal => "horizontal_art_source", Kind::Icon => "icon_source" };
        let mut u: Update = vec![];
        let mut push = |k: Kind, tag: String, art: &crate::artclient::Art| { if tag != "missing" || !art.has_cover(k, appid) { u.push((col(k), t(tag))); } };
        if appid < 0 && source == "steam" {
            let Some((sid, ih)) = art.steam_copy_of(appid) else { crate::net::log(&format!("[bulk_art_scrape] {appid}: no Steam copy known for this game")); return Outcome::Failed };
            let mut any = false;
            for &k in &kinds { if art.download_from_steam(appid, sid, k, &ih) == "steam" { u.push((col(k), t("steam"))); any = true; } }
            if !any { crate::net::log(&format!("[bulk_art_scrape] {appid}: no Steam copy with art for this game")); return Outcome::Failed; }
            crate::net::log(&format!("[bulk_art_scrape] {appid}: art taken from Steam game {sid}"));
        } else if appid < 0 {
            let sgdb_id = if name.is_empty() { None } else { art.sgdb_search_game_id(&name) };
            let src = if source == "sgdb" { "sgdb" } else { "auto" };
            for &k in &kinds { let (o, pf) = orders(appid, k); let tag = art.download_art(k, appid, None, src, o.as_deref(), pf, sgdb_id, Some(&name), ""); push(k, tag, &art); }
        } else {
            let assets = if source != "sgdb" { art.steam_assets(appid) } else { Default::default() };
            for &k in &kinds { let (o, pf) = orders(appid, k); let tag = art.download_art(k, appid, Some(assets.clone()), &source, o.as_deref(), pf, None, None, if k == Kind::Icon { &icon_hash } else { "" }); push(k, tag, &art); }
        }
        u.push(("art_fetched", t(&today)));
        match apply(&conn, appid, &u) { Ok(()) => Outcome::Done, Err(e) => { crate::net::log(&format!("[bulk_art_scrape] database update failed for {appid}: {e}")); Outcome::Failed } }
    });
    run_pool(queue, PoolCfg { name: "bulk_art", workers: 3, gap: Duration::from_millis(800), policy }, cancel, process, Arc::new(|a| *a), on_event)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{Http, NetError, Response};

    struct Seq(Mutex<Vec<Result<Response, NetError>>>);
    impl Http for Seq { fn get(&self, _u: &str, _h: &[(&str, &str)], _t: Duration) -> Result<Response, NetError> { let mut g = self.0.lock().unwrap(); if g.is_empty() { Err(NetError("x".into())) } else { g.remove(0) } } }
    fn collect() -> (Arc<dyn Fn(Event) + Send + Sync>, Arc<Mutex<Vec<Event>>>) { let v = Arc::new(Mutex::new(vec![])); let v2 = v.clone(); (Arc::new(move |e| v2.lock().unwrap().push(e)), v) }
    fn cfg(w: usize) -> PoolCfg { PoolCfg { name: "t", workers: w, gap: Duration::ZERO, policy: None } }

    #[test] fn short_pauses_after_visible_games_are_paid_back_so_the_overall_rate_holds() {
        let p = GapPolicy::new(Duration::from_millis(800), Duration::from_millis(250), 0, Arc::new(|a: &i64| if *a < 10 { 0 } else { 3 }));
        let mut total = Duration::ZERO;
        for a in [1, 2, 3, 4] { assert_eq!(p.after(a), Duration::from_millis(250)); total += Duration::from_millis(250); }   // four visible games: 4 x 550 ms owed
        for a in [11, 12, 13, 14, 15, 16] { let d = p.after(a); assert!(d >= Duration::from_millis(800) && d <= Duration::from_millis(1600)); total += d; }
        // after the debt is paid the normal pause comes back
        assert_eq!(p.after(20), Duration::from_millis(800));
        // ten games at the normal 800 ms would have taken 8 s: the run took the same
        assert_eq!(total, Duration::from_millis(8000));
    }
    #[test] fn pool_counts_and_orders_progress() {
        let q = Arc::new(WorkQueue::new(1..=10i64));
        let (cb, ev) = collect();
        let c = run_pool(q, cfg(3), Cancel::new(), Arc::new(|&a| if a % 5 == 0 { Outcome::Failed } else { Outcome::Done }), Arc::new(|a| *a), cb);
        assert_eq!((c.done, c.failed, c.aborted, c.cancelled), (8, 2, false, false));
        assert_eq!(ev.lock().unwrap().len(), 10);
        assert!(ev.lock().unwrap().iter().all(|e| matches!(e, Event::Done { total: 10, .. } | Event::Failed { total: 10, .. })));
    }
    #[test] fn a_429_requeues_and_the_item_is_retried() {
        let q = Arc::new(WorkQueue::new([1i64, 2, 3]));
        let hit = Arc::new(AtomicUsize::new(0)); let h = hit.clone();
        let (cb, ev) = collect();
        // Python waits 15 s on a first 429 without Retry-After; give a 0.05 s server-sent wait so the test is quick
        let c = run_pool(q, cfg(2), Cancel::new(), Arc::new(move |&a| if a == 2 && h.fetch_add(1, Ordering::SeqCst) == 0 { Outcome::RateLimited(Some(0.05)) } else { Outcome::Done }), Arc::new(|a| *a), cb);
        assert_eq!((c.done, c.failed), (3, 0));
        let rl: Vec<_> = ev.lock().unwrap().iter().filter_map(|e| if let Event::RateLimit(w) = e { Some(w.clone()) } else { None }).collect();
        assert_eq!(rl.len(), 1); assert_eq!((rl[0].attempt, rl[0].of, rl[0].server_requested), (1, 4, true));
    }
    #[test] fn cancel_stops_the_pool() {
        let q = Arc::new(WorkQueue::new(1..=1000i64)); let cancel = Cancel::new(); let c2 = cancel.clone();
        let (cb, _) = collect();
        let c = run_pool(q.clone(), cfg(2), cancel, Arc::new(move |&a| { if a == 5 { c2.cancel(); } Outcome::Done }), Arc::new(|a| *a), cb);
        assert!(c.cancelled && c.done < 1000 && q.len() > 900);
    }

    fn mem() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE games (appid INTEGER PRIMARY KEY, name TEXT, platform TEXT, completion_status TEXT, total_achievements INTEGER, unlocked_achievements INTEGER, tags TEXT, meta_fetched TEXT, review_score TEXT, developers TEXT);
            INSERT INTO games VALUES (1,'A','steam','Beaten',10,10,NULL,'0',NULL,NULL),(2,'B','steam','Completed',10,9,NULL,'0',NULL,NULL),(3,'C','gog','Beaten',5,5,NULL,'0',NULL,NULL);").unwrap(); c
    }
    #[test] fn apply_writes_only_given_columns() {
        let c = mem();
        apply(&c, 1, &vec![("tags", t("a,b")), ("meta_fetched", t("2026-11-01"))]).unwrap();
        let r: (String, String, String) = c.query_row("SELECT tags, meta_fetched, name FROM games WHERE appid=1", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!(r, ("a,b".into(), "2026-11-01".into(), "A".into()));
    }
    #[test] fn sweep_is_steam_only_and_switchable() {
        let c = mem();
        assert_eq!(sweep_achievement_status(&c, true, true).unwrap(), (1, 1));
        let s: Vec<String> = c.prepare("SELECT completion_status FROM games ORDER BY appid").unwrap().query_map([], |r| r.get(0)).unwrap().map(|x| x.unwrap()).collect();
        assert_eq!(s, ["Completed", "Beaten", "Beaten"]);
        assert_eq!(sweep_achievement_status(&c, false, false).unwrap(), (0, 0));
    }

    #[test] fn rescrape_steam_builds_python_dict() {
        let details = r#"{"9":{"success":true,"data":{"name":"Nine","type":"game","steam_appid":9,"developers":["D"],"publishers":["P"],"genres":[{"description":"Action"}],"categories":[{"description":"Single-player"}],"is_free":false,"metacritic":{"score":80},"short_description":"<b>Hi</b>","release_date":{"date":"5 Jan, 2020"}}}}"#;
        let reviews = r#"{"response":{"query_summary":{"total_reviews":100,"total_positive":90,"review_score_desc":"Very Positive"}}}"#;
        let page = r#"<a class="app_tag"> RPG </a><a class="app_tag">+</a><a class="app_tag">Indie</a>"#;
        let cheevo = r#"{"playerstats":{"success":true,"achievements":[{"achieved":1},{"achieved":1}]}}"#;
        let ok = |b: &str| Ok(Response { status: 200, retry_after: None, body: b.into() });
        let h = Arc::new(Seq(Mutex::new(vec![ok(details), ok(reviews), ok(page), ok(cheevo)])));
        let s = Steam::new(h, Some(crate::steamclient::Creds { api_key: "k".into(), steam_id: "1".into() }), 10.0);
        let u = rescrape_steam(&s, 9, AppinfoDates { original: Some(1577836800), steam: None }, "2026-11-01").unwrap().unwrap();
        let keys: Vec<_> = u.iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, ["name_from_store", "name", "developers", "publishers", "release_date", "genres", "categories", "is_free", "metacritic_score", "short_description",
            "review_score", "review_percentage", "weighted_percentage", "total_reviews", "positive_reviews", "tags", "total_achievements", "unlocked_achievements", "completion_status", "meta_fetched", "cheevos_fetched"]);
        assert!(u.contains(&("tags", t("RPG,Indie")))); assert!(u.contains(&("short_description", t("Hi"))));
    }
    #[test] fn the_rescrape_tab_choices_skip_what_is_switched_off() {
        let ok = |b: &str| Ok(Response { status: 200, retry_after: None, body: b.into() });
        let cheevo = r#"{"playerstats":{"success":true,"achievements":[{"achieved":1},{"achieved":0}]}}"#;
        let key = || Some(crate::steamclient::Creds { api_key: "k".into(), steam_id: "1".into() });
        // achievements only: one request, no store data
        let s = Steam::new(Arc::new(Seq(Mutex::new(vec![ok(cheevo)]))), key(), 10.0);
        let u = rescrape_steam_with(&s, 9, AppinfoDates::default(), "d", false, true).unwrap().unwrap();
        assert_eq!(u.iter().map(|(c, _)| *c).collect::<Vec<_>>(), ["total_achievements", "unlocked_achievements", "cheevos_fetched"]);
        // store data only: the achievements request is never made
        let s = Steam::new(Arc::new(Seq(Mutex::new(vec![ok(r#"{"9":{"success":false}}"#), ok(r#"{"response":{"query_summary":{"total_reviews":0,"total_positive":0}}}"#), ok("<html></html>")]))), key(), 10.0);
        let u = rescrape_steam_with(&s, 9, AppinfoDates::default(), "d", true, false).unwrap().unwrap();
        assert!(!u.iter().any(|(c, _)| c.contains("achievements")), "{:?}", u.iter().map(|(c, _)| *c).collect::<Vec<_>>());
    }
    #[test] fn playtime_comes_from_steams_own_files_when_the_web_api_has_no_key() {
        let s = Steam::new(Arc::new(Seq(Mutex::new(vec![]))), None, 10.0);
        let mut local = std::collections::HashMap::new();
        local.insert(7, (120, Some(1_700_000_000)));
        let u = playtime_update(&s, 7, &local);
        assert_eq!(u.iter().map(|(c, v)| (*c, v.clone())).collect::<Vec<_>>(), vec![("playtime_forever", i(120)), ("last_played", i(1_700_000_000))]);
        assert!(playtime_update(&s, 8, &local).is_empty(), "a game Steam's files do not know changes nothing");
    }
    /// A store that no longer has the app: the API says success false, the page redirects to the front page.
    struct GoneStore(&'static str);
    impl Http for GoneStore {
        fn get(&self, _u: &str, _h: &[(&str, &str)], _t: Duration) -> Result<Response, NetError> { Ok(Response { status: 200, retry_after: None, body: r#"{"9":{"success":false}}"#.into() }) }
        fn get_raw(&self, _u: &str, _h: &[(&str, &str)], _t: Duration) -> Result<crate::net::Raw, NetError> {
            match self.0 { "live" => Ok(crate::net::Raw { status: 200, headers: vec![], body: String::new() }), "error" => Err(NetError("down".into())),
                _ => Ok(crate::net::Raw { status: 302, headers: vec![("Location".into(), "https://store.steampowered.com/".into())], body: String::new() }) }
        }
    }
    #[test] fn a_game_whose_store_page_is_gone_is_marked_tried_but_a_hiccup_or_a_live_page_is_not() {
        let at = |kind: &'static str| rescrape_steam(&Steam::new(Arc::new(GoneStore(kind)), None, 10.0), 9, AppinfoDates::default(), "d").unwrap();
        assert_eq!(at("gone"), Some(vec![("meta_fetched", t("d"))]), "removed from the store: tried today, not asked again after every sync");
        assert_eq!(at("live"), None, "the page is there, so the empty answer was a glitch: try again later");
        assert_eq!(at("error"), None, "a failed request is not a removal");
        // without the store option nothing was asked, so nothing is stamped
        let s = Steam::new(Arc::new(GoneStore("gone")), None, 10.0);
        assert_eq!(rescrape_steam_with(&s, 9, AppinfoDates::default(), "d", false, false).unwrap(), None);
    }
    #[test] fn rescrape_steam_nothing_back_and_no_answer_about_the_page_changes_nothing() {
        let s = Steam::new(Arc::new(Seq(Mutex::new(vec![]))), None, 10.0);
        assert_eq!(rescrape_steam(&s, 9, AppinfoDates::default(), "d").unwrap(), None);
    }
    #[test] fn rescrape_429_in_the_third_request_gives_up_the_game() {
        let r = |st| Ok(Response { status: st, retry_after: Some(7.0), body: String::new() });
        let s = Steam::new(Arc::new(Seq(Mutex::new(vec![r(404), r(404), r(429)]))), None, 10.0);
        assert_eq!(rescrape_steam(&s, 9, AppinfoDates::default(), "d").unwrap_err().0, Some(7.0));
    }

    #[test] fn protondb_job_clears_on_404_keeps_on_failure() {
        let db = std::env::temp_dir().join(format!("zest_pdb_{}.db", std::process::id())); let _ = std::fs::remove_file(&db);
        let c = Connection::open(&db).unwrap();
        c.execute_batch("CREATE TABLE games (appid INTEGER PRIMARY KEY, protondb_tier TEXT, protondb_confidence TEXT, protondb_fetched TEXT);
            INSERT INTO games VALUES (1,'old','x','0'),(2,'old','x','0'),(3,'old','x','0');").unwrap();
        struct By;
        impl Http for By { fn get(&self, u: &str, _h: &[(&str, &str)], _t: Duration) -> Result<Response, NetError> {
            if u.contains("/1.json") { Ok(Response { status: 200, retry_after: None, body: r#"{"tier":"gold","confidence":"good"}"#.into() }) }
            else if u.contains("/2.json") { Ok(Response { status: 404, retry_after: None, body: String::new() }) } else { Err(NetError("Io".into())) } } }
        let st = Arc::new(Steam::new(Arc::new(By), None, 10.0));
        let (cb, _) = collect();
        let n = bulk_protondb(db.clone(), st, "2026-11-01".into(), vec![1, 2, 3], Cancel::new(), cb);
        assert_eq!((n.done, n.failed), (2, 1));
        let rows: Vec<(Option<String>, String)> = c.prepare("SELECT protondb_tier, protondb_fetched FROM games ORDER BY appid").unwrap().query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(|x| x.unwrap()).collect();
        assert_eq!(rows, [(Some("gold".into()), "2026-11-01".into()), (None, "2026-11-01".into()), (Some("old".into()), "0".into())]);
        let _ = std::fs::remove_file(&db);
    }

    #[test] fn hltb_job_uses_the_map_then_names_and_keeps_data_when_unreachable() {
        use crate::hltbclient::Hltb;
        let db = std::env::temp_dir().join(format!("zest_hltb_{}.db", std::process::id())); let _ = std::fs::remove_file(&db);
        let c = Connection::open(&db).unwrap();
        c.execute_batch("CREATE TABLE games (appid INTEGER PRIMARY KEY, name TEXT, hltb_id INTEGER, hltb_matched_name TEXT, hltb_main INTEGER, hltb_extras INTEGER, hltb_completionist INTEGER, hltb_match_score INTEGER, hltb_fetched TEXT);
            INSERT INTO games (appid, name, hltb_main, hltb_fetched) VALUES (1,'Portal 2',5,'old'),(2,'Zzz',5,'old');").unwrap();
        struct Site;
        impl Http for Site {
            fn get(&self, u: &str, _h: &[(&str, &str)], _t: Duration) -> Result<Response, NetError> {
                let ok = |b: &str| Ok(Response { status: 200, retry_after: None, body: b.into() });
                if u.ends_with(".js") { ok(r#"fetch("/api/find",{method:"POST"})"#) } else if u.contains("/init?t=") { ok(r#"{"token":"t","k":"a","v":"b"}"#) }
                else if u.contains("game?id=") { ok("<title>How long is Portal 2? | HowLongToBeat</title>") } else { ok(r#"<script src="/_next/static/chunks/a.js"></script>"#) } }
            fn post(&self, _u: &str, _h: &[(&str, &str)], b: &str, _t: Duration) -> Result<Response, NetError> {
                if b.contains("Zzz") { return Err(NetError("Io".into())); }
                Ok(Response { status: 200, retry_after: None, body: r#"{"data":[{"game_id":7,"game_name":"Portal 2","comp_main":36000,"comp_plus":72000,"comp_100":108000}]}"#.into() }) }
        }
        let (cb, _) = collect();
        let n = bulk_hltb(db.clone(), Arc::new(Hltb::new(Arc::new(Site))), Arc::new([(1i64, 7i64)].into_iter().collect()), 99, "2026-11-01".into(), vec![1, 2], Cancel::new(), cb);
        assert_eq!((n.done, n.failed), (1, 1));
        let r: Vec<(Option<i64>, Option<i64>, String)> = c.prepare("SELECT hltb_id, hltb_main, hltb_fetched FROM games ORDER BY appid").unwrap().query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().map(|x| x.unwrap()).collect();
        assert_eq!(r, [(Some(7), Some(600), "2026-11-01".into()), (None, Some(5), "old".into())]);
        let _ = std::fs::remove_file(&db);
    }

    #[test] fn backfill_job_writes_only_gaps() {
        let db = std::env::temp_dir().join(format!("zest_bf_{}.db", std::process::id())); let _ = std::fs::remove_file(&db);
        let c = Connection::open(&db).unwrap();
        c.execute_batch("CREATE TABLE games (appid INTEGER PRIMARY KEY, name TEXT, platform TEXT, platform_id TEXT, platform_slug TEXT, steam_appid INTEGER, meta_backfill_fetched TEXT, developers TEXT, publishers TEXT, genres TEXT, categories TEXT, tags TEXT, release_date INTEGER, review_score TEXT, review_percentage INTEGER, weighted_percentage INTEGER, total_reviews INTEGER, positive_reviews INTEGER, is_free INTEGER, metacritic_score INTEGER, short_description TEXT, total_achievements INTEGER, unlocked_achievements INTEGER, cheevos_fetched TEXT);
            INSERT INTO games (appid, name, platform, developers, meta_backfill_fetched) VALUES (620,'Portal 2','steam','Mine','0'),(999,'Gone','steam',NULL,'0');").unwrap();
        struct S;
        impl Http for S { fn get(&self, u: &str, _h: &[(&str, &str)], _t: Duration) -> Result<Response, NetError> {
            let ok = |b: &str| Ok(Response { status: 200, retry_after: None, body: b.into() });
            if u.contains("appids=620") { ok(r#"{"620":{"success":true,"data":{"name":"Portal 2","type":"game","steam_appid":620,"developers":["Valve"],"publishers":["Valve"],"genres":[{"description":"Action"}],"release_date":{"date":"19 Apr, 2011"}}}}"#) }
            else if u.contains("GetAppReviews") { ok(r#"{"response":{"query_summary":{"total_reviews":100,"total_positive":90,"review_score_desc":"Very Positive"}}}"#) }
            else if u.contains("/app/620/") { ok(r#"<a class="app_tag">Puzzle</a>"#) }
            else if u.contains("pcgamingwiki") { ok(r#"{"error":{}}"#) } else { Err(NetError("no".into())) } } }
        let h: Arc<dyn Http> = Arc::new(S);
        let (cb, ev) = collect();
        let n = bulk_backfill(db.clone(), Arc::new(Steam::new(h.clone(), None, 10.0)), h, Arc::new(crate::sources::Registry::builtin()), Arc::new(|_| AppinfoDates { original: Some(5), steam: None }), "2026-11-01".into(), vec![620, 999, 12345], Duration::ZERO, Cancel::new(), cb);
        assert_eq!((n.done, n.failed), (2, 1), "{:?}", ev.lock().unwrap());
        let r: (String, String, String, i64, String) = c.query_row("SELECT developers, publishers, tags, release_date, meta_backfill_fetched FROM games WHERE appid=620", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).unwrap();
        assert_eq!(r, ("Mine".into(), "Valve".into(), "Puzzle".into(), 5, "2026-11-01".into()));
        let _ = std::fs::remove_file(&db);
    }

    #[test] fn rescrape_of_a_non_steam_game_goes_through_its_source_then_the_backfill() {
        use crate::sources::{Caps, GameRef, LibrarySource, Registry, SourceCtx};
        struct Gog;
        impl LibrarySource for Gog {
            fn id(&self) -> &str { "gog" } fn label(&self) -> &str { "GOG" } fn caps(&self) -> Caps { Caps { rescrape: true, ..Default::default() } }
            fn rescrape(&self, _cx: &SourceCtx, g: &GameRef) -> Fetch<Option<Update>> { Ok(Some(vec![("developers", t("Source Dev")), ("meta_fetched", t(&format!("by {}", g.platform_id)))])) }
        }
        struct Net; impl Http for Net { fn get(&self, u: &str, _h: &[(&str, &str)], _t: Duration) -> Result<Response, NetError> {
            let ok = |b: &str| Ok(Response { status: 200, retry_after: None, body: b.into() });
            if u.contains("pcgamingwiki") { ok(r#"{"error":{}}"#) } else if u.contains("storesearch") { ok(r#"{"items":[]}"#) } else { Err(NetError("no".into())) } } }
        let db = std::env::temp_dir().join(format!("zest_rs_{}.db", std::process::id())); let _ = std::fs::remove_file(&db);
        let c = Connection::open(&db).unwrap();
        c.execute_batch("CREATE TABLE games (appid INTEGER PRIMARY KEY, name TEXT, platform TEXT, platform_id TEXT, platform_slug TEXT, platform_appname TEXT, installed INTEGER, install_path TEXT, steam_appid INTEGER, meta_backfill_fetched TEXT, developers TEXT, publishers TEXT, genres TEXT, categories TEXT, tags TEXT, release_date INTEGER, review_score TEXT, is_free INTEGER, metacritic_score INTEGER, short_description TEXT, total_achievements INTEGER, meta_fetched TEXT);
            INSERT INTO games (appid, name, platform, platform_id, installed) VALUES (-3,'Some Game','gog','1207',0),(-4,'Nothing Here','epic_games','x',0);").unwrap();
        let mut reg = Registry::empty(); reg.register(Arc::new(Gog)).unwrap();
        let http: Arc<dyn Http> = Arc::new(Net);
        let job = Rescrape { db: db.clone(), root: std::env::temp_dir(), local_play: Default::default(), steam: Arc::new(Steam::new(http.clone(), None, 10.0)), http, sources: Arc::new(reg), appinfo: Arc::new(|_| AppinfoDates::default()), today: "2026-11-01".into(), account_id: None, meta: true, cheevo: true, phase: Phase::All };
        let (cb, _) = collect();
        let n = bulk_rescrape(job, vec![-3, -4], Cancel::new(), cb);
        // -3: source data written; -4: no source, nothing found anywhere -> the backfill stamps no_match, which is still a write, so it counts as done
        assert_eq!((n.done, n.failed), (2, 0));
        let r: (String, String) = c.query_row("SELECT developers, meta_fetched FROM games WHERE appid=-3", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(r, ("Source Dev".into(), "by 1207".into()));
        let b: String = c.query_row("SELECT meta_backfill_fetched FROM games WHERE appid=-4", [], |r| r.get(0)).unwrap();
        assert_eq!(b, "no_match");
        let _ = std::fs::remove_file(&db);
    }
}
