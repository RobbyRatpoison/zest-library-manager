//! Runs a Steam re-scrape on a background thread and hands the UI its progress. One job at a time (`HomeState::sync`).
use pd_filter::jobs::{self, Counts, Event, Rescrape};
use pd_filter::net::{Cancel, UreqHttp, Wait};
use pd_filter::steamclient::{AppinfoDates, Creds, Steam};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use std::time::Instant;

enum Msg { Ev(Event), End(Counts) }

pub struct SyncJob {
    cancel: Cancel, rx: mpsc::Receiver<Msg>,
    pub label: String, pub total: usize, pub done: usize, pub failed: usize,
    /// A 429 is being waited out: what and since when.
    pub wait: Option<(Wait, Instant)>,
    /// The game an Edit dialog asked about (reload it when the job ends).
    pub target: Option<i64>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Kind { Rescrape, ProtonDb, Hltb, Backfill, Art, ArtEdit, PluginSync, PluginInstall, LauncherInstall, XboxOwned, Populate, HltbConfirm, PurchaseDates }

/// One art operation from the Edit dialog's art tabs.
#[derive(Clone)]
pub enum ArtOp { Order(pd_filter::artclient::Kind), FromSteam(pd_filter::artclient::Kind), Url(pd_filter::artclient::Kind, String) }

pub struct Params { pub kind: Kind, pub meta: bool, pub cheevo: bool, pub art_kinds: Vec<pd_filter::artclient::Kind>, pub art_source: String, pub sources: Arc<pd_filter::sources::Registry>, pub plugin_platform: Option<String>, pub art_op: Option<ArtOp>, pub sgdb_key: Option<String>, pub art_prefs: serde_json::Value, pub prefer_fit: bool, pub hltb_threshold: i64, pub db: PathBuf, pub root: PathBuf, pub steam_root: Option<PathBuf>, pub half_trust: f64, pub promote: bool, pub downgrade: bool,
    /// `Kind::Art`: ranks the games still waiting (what is on screen first, see `pd_filter::artfocus`); None = the order the list was given in.
    pub art_rank: Option<Arc<dyn Fn(&i64) -> u32 + Send + Sync>> }

pub fn start(p: Params, appids: Vec<i64>, label: &str, target: Option<i64>, ctx: egui::Context) -> Result<SyncJob, String> {
    if appids.is_empty() { return Err("Nothing to re-scrape".into()); }
    let acct = pd_filter::account::load_account(&p.root);
    let creds = acct.map(|a| Creds { api_key: a.api_key, steam_id: a.steam_id });
    let steam = Arc::new(Steam::new(Arc::new(UreqHttp::new()), creds, p.half_trust));
    let today: String = rusqlite::Connection::open_in_memory().and_then(|c| c.query_row("SELECT date('now','localtime')", [], |r| r.get(0))).map_err(|e| e.to_string())?;
    let appinfo = Arc::new(p.steam_root.as_deref().map(pd_filter::steamlocal::read_appinfo).unwrap_or_default());
    let (tx, rx) = mpsc::channel();
    let cancel = Cancel::new();
    // a batch of re-scrapes runs in two passes (core store fields, then reviews and tags), so the progress counts both
    let two_pass = p.kind == Kind::Rescrape && p.meta && appids.len() >= 4;
    let total = if two_pass { appids.len() * 2 } else { appids.len() };
    let (c2, tx2, db) = (cancel.clone(), tx.clone(), p.db.clone());
    let (promote, downgrade, p_kind, hltb_threshold, root) = (p.promote, p.downgrade, p.kind, p.hltb_threshold, p.root.clone());
    let acct_id: Option<String> = db.file_stem().and_then(|n| n.to_str()).and_then(|n| n.strip_prefix("games_")).map(String::from);
    let (sources, plugin_platform) = (p.sources.clone(), p.plugin_platform.clone());
    let steam_root = p.steam_root.clone();
    let (p_meta, p_cheevo, art_kinds, art_source) = (p.meta, p.cheevo, p.art_kinds.clone(), p.art_source.clone());
    let (sgdb_key, art_prefs, prefer_fit, art_op) = (p.sgdb_key.clone(), p.art_prefs.clone(), p.prefer_fit, p.art_op.clone());
    let art_rank = p.art_rank.clone();
    std::thread::spawn(move || {
        let ctx2 = ctx.clone();
        let cb: Arc<dyn Fn(Event) + Send + Sync> = { let tx = tx2.clone(); Arc::new(move |e| { let _ = tx.send(Msg::Ev(e)); ctx2.request_repaint(); }) };
        let ai = appinfo.clone();
        if p_kind == Kind::PluginSync {
            let counts = run_plugin_sync(&sources, plugin_platform.as_deref().unwrap_or(""), &db, &root, steam, appinfo, &today, &c2, &cb);
            let _ = tx2.send(Msg::End(counts)); ctx.request_repaint(); return;
        }
        if p_kind == Kind::PurchaseDates {
            let counts = run_purchase_dates(&sources, &db, &root, steam, appinfo, &today, &appids, &c2, &cb);
            let _ = tx2.send(Msg::End(counts)); ctx.request_repaint(); return;
        }
        if p_kind == Kind::HltbConfirm {
            let counts = match rusqlite::Connection::open(&db) {
                Ok(conn) => {
                    conn.busy_timeout(std::time::Duration::from_secs(10)).ok();
                    let h = pd_filter::hltbclient::Hltb::new(Arc::new(UreqHttp::new()));
                    let day = rusqlite::Connection::open_in_memory().and_then(|c| c.query_row("SELECT date('now')", [], |r| r.get::<_, String>(0))).unwrap_or_default();
                    let total = appids.len();
                    let c = pd_filter::hltb_ops::confirm_many(&conn, &h, &appids, &day, &c2, &mut |_, _| { cb(Event::Done { appid: 0, total }); });
                    Counts { done: c.done + c.cleared, failed: c.failed, aborted: c.aborted, cancelled: c.cancelled }
                }
                Err(_) => Counts { failed: appids.len(), ..Default::default() },
            };
            let _ = tx2.send(Msg::End(counts)); ctx.request_repaint(); return;
        }
        if p_kind == Kind::Populate {
            let id = pd_filter::account::load_account(&root).map(|a| a.steam_id).unwrap_or_default();
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
            pd_filter::net::set_activity("Reading your Steam library");
            let counts = match pd_filter::populate::run(&db, &steam, steam_root.as_deref(), &id, now) {
                Ok(ids) => { pd_filter::net::log(&format!("populate: {} new games", ids.len())); pd_filter::populate::set_added(&ids); Counts { done: ids.len(), ..Default::default() } }
                Err(e) => { pd_filter::net::log(&format!("populate: {e}")); pd_filter::net::set_note(&e); Counts { failed: 1, ..Default::default() } }
            };
            pd_filter::net::set_activity("");
            let _ = tx2.send(Msg::End(counts)); ctx.request_repaint(); return;
        }
        if p_kind == Kind::XboxOwned {
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
            let r = pd_filter::xbox_owned::import(&db, &pd_filter::installdir::home(), &pd_filter::net::UreqHttp::new(), now);
            pd_filter::net::set_activity("");
            // the Xbox card's "last sync" line follows a Nexus sync too (it says what the account holds now)
            let ended = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(now);
            let (counts, status) = match r {
                Ok((msg, o, listed)) => { pd_filter::net::log(&format!("xbox owned import: {msg}")); pd_filter::net::set_note(&msg);
                    (Counts { done: 1, ..Default::default() }, pd_filter::status::SyncStatus { at: ended, found: listed, added: o.added, updated: o.marked, blocked: 0, error: None }) }
                Err(e) => { pd_filter::net::log(&format!("xbox owned import: {e}")); pd_filter::net::set_note(&e);
                    (Counts { failed: 1, ..Default::default() }, pd_filter::status::SyncStatus { at: ended, error: Some(e), ..Default::default() }) }
            };
            if let Err(e) = pd_filter::status::record(&root, "xbox", &status) { pd_filter::net::log(&format!("xbox owned import: the status could not be saved: {e}")); }
            let _ = tx2.send(Msg::End(counts)); ctx.request_repaint(); return;
        }
        if p_kind == Kind::LauncherInstall {
            let platform = plugin_platform.clone().unwrap_or_default();
            let r = pd_filter::launcher_installer::install_here(&platform, &root, &c2, &mut |_, detail| pd_filter::net::set_activity(detail));
            pd_filter::net::set_activity("");
            let counts = match r {
                Ok(()) => { pd_filter::net::set_note("The launcher is installed. Sign in to it, then press SYNC LIBRARY on this store's card."); Counts { done: 1, ..Default::default() } }
                Err(e) if e == "Cancelled" => { pd_filter::net::set_note("Launcher install cancelled"); Counts { cancelled: true, ..Default::default() } }
                Err(e) => { pd_filter::net::log(&format!("launcher install {platform}: {e}")); pd_filter::net::set_note(&e); Counts { failed: 1, ..Default::default() } }
            };
            let _ = tx2.send(Msg::End(counts)); ctx.request_repaint(); return;
        }
        if p_kind == Kind::PluginInstall {
            let counts = run_plugin_install(&sources, plugin_platform.as_deref().unwrap_or(""), appids[0], &db, &root, steam, appinfo, &today, &c2);
            let _ = tx2.send(Msg::End(counts)); ctx.request_repaint(); return;
        }
        if p_kind == Kind::ArtEdit {
            let counts = run_art_op(&sources, &db, &root, appids[0], art_op.clone().unwrap(), sgdb_key, art_prefs, prefer_fit, &today, &cb);
            let _ = tx2.send(Msg::End(counts)); ctx.request_repaint(); return;
        }
        if p_kind == Kind::Art {
            use pd_filter::artclient::{Art, Kind as AK};
            let http: Arc<dyn pd_filter::net::Http> = Arc::new(UreqHttp::new());
            let store = Arc::new(crate::artstore::FsStore { root: root.clone(), db: db.clone(), stage: false });
            let art = Arc::new(Art::new(http, store, Arc::new(crate::artstore::DbHooks::new(db.clone(), Some(sources.clone()))), sgdb_key));
            let (prefs, pdb, srcs) = (art_prefs, db.clone(), sources.clone());
            let orders: Arc<dyn Fn(i64, AK) -> (Option<Vec<String>>, bool) + Send + Sync> = Arc::new(move |appid, k| {
                let platform = if appid < 0 { rusqlite::Connection::open(&pdb).ok().and_then(|c| c.query_row("SELECT platform FROM games WHERE appid = ?", [appid], |r| r.get::<_, Option<String>>(0)).ok().flatten()).unwrap_or_else(|| "steam".into()) } else { "steam".into() };
                let saved: Option<Vec<String>> = prefs.get(&platform).and_then(|p| p.get(k.name())).and_then(|l| l.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect());
                let has_store = srcs.get(&platform).map_or(false, |s| s.caps().art);
                (grid_core::art::source_order(k.name(), has_store, saved.as_deref(), None), prefer_fit && k != AK::Icon)
            });
            let counts = jobs::bulk_art(db.clone(), art, orders, art_kinds, art_source, today, appids, c2, cb, art_rank);
            let _ = tx2.send(Msg::End(counts)); ctx.request_repaint(); return;
        }
        if p_kind == Kind::Backfill {
            let http: Arc<dyn pd_filter::net::Http> = Arc::new(UreqHttp::new());
            let ai = appinfo.clone();
            let lookup: Arc<dyn Fn(i64) -> AppinfoDates + Send + Sync> = Arc::new(move |id| ai.get(&id).map(|a| AppinfoDates { original: a.original_release_date, steam: a.steam_release_date }).unwrap_or_default());
            let counts = jobs::bulk_backfill(db.clone(), steam, http, sources.clone(), lookup, today, appids, std::time::Duration::from_secs(3), c2, cb);
            let _ = tx2.send(Msg::End(counts)); ctx.request_repaint(); return;
        }
        if p_kind == Kind::Hltb {
            let http: Arc<dyn pd_filter::net::Http> = Arc::new(UreqHttp::new());
            let map = Arc::new(pd_filter::hltbclient::steam_hltb_map(&root, http.as_ref()));
            let utc: String = rusqlite::Connection::open_in_memory().and_then(|c| c.query_row("SELECT date('now')", [], |r| r.get(0))).unwrap_or_default();
            let counts = jobs::bulk_hltb(db.clone(), Arc::new(pd_filter::hltbclient::Hltb::new(http)), map, hltb_threshold, utc, appids, c2, cb);
            let _ = tx2.send(Msg::End(counts)); ctx.request_repaint(); return;
        }
        if p_kind == Kind::ProtonDb {
            let counts = jobs::bulk_protondb(db.clone(), steam, today, appids, c2, cb);
            let _ = tx2.send(Msg::End(counts)); ctx.request_repaint(); return;
        }
        let http: Arc<dyn pd_filter::net::Http> = Arc::new(UreqHttp::new());
        let local_play: std::collections::HashMap<i64, (i64, Option<i64>)> = match (&steam_root, &acct_id) { (Some(r), Some(id)) => pd_filter::steamlocal::local_library(r, Some(id)).into_iter().map(|a| (a.appid, (a.playtime_forever, a.last_played))).collect(), _ => Default::default() };
        let local_play = Arc::new(local_play);
        let make = |phase: jobs::Phase| Rescrape { local_play: local_play.clone(), db: db.clone(), root: root.clone(), steam: steam.clone(), http: http.clone(), sources: sources.clone(), account_id: acct_id.clone(), meta: p_meta, cheevo: p_cheevo, phase,
            appinfo: { let ai = ai.clone(); Arc::new(move |id| ai.get(&id).map(|a| AppinfoDates { original: a.original_release_date, steam: a.steam_release_date }).unwrap_or_default()) }, today: today.clone() };
        // a batch is done in two passes: first every game's core store fields (the card gets its name, developer, genres, date and description), then reviews, tags and achievements
        let counts = if two_pass {
            let c1 = jobs::bulk_rescrape(make(jobs::Phase::Core), appids.clone(), c2.clone(), cb.clone());
            if c1.cancelled || c1.aborted { c1 } else {
                let c2n = jobs::bulk_rescrape(make(jobs::Phase::Rest), appids, c2, cb);
                jobs::Counts { done: c2n.done, failed: c1.failed + c2n.failed, aborted: c2n.aborted, cancelled: c2n.cancelled }
            }
        } else { jobs::bulk_rescrape(make(jobs::Phase::All), appids, c2, cb) };
        if let Ok(c) = rusqlite::Connection::open(&db) { let _ = c.busy_timeout(std::time::Duration::from_secs(10)); let _ = jobs::sweep_achievement_status(&c, promote, downgrade); }
        let _ = tx2.send(Msg::End(counts)); ctx.request_repaint();
    });
    drop(tx);
    Ok(SyncJob { cancel, rx, label: label.into(), total, done: 0, failed: 0, wait: None, target })
}

impl SyncJob {
    pub fn cancel(&self) { self.cancel.cancel(); }
    /// Drain the progress; `Some(counts)` once the job has ended.
    pub fn poll(&mut self) -> Option<Counts> {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Ev(Event::Done { .. }) => { self.done += 1; self.wait = None; }
                Msg::Ev(Event::Failed { .. }) => { self.failed += 1; self.wait = None; }
                Msg::Ev(Event::RateLimit(w)) => self.wait = Some((w, Instant::now())),
                Msg::End(c) => return Some(c),
            }
        }
        None
    }
    /// (what it is doing, total, finished, the rate-limit wait text), for the Bulk Ops dialog's progress bar.
    pub fn progress(&self) -> (String, usize, usize, Option<String>) {
        let wait = self.wait.as_ref().map(|(w, at)| format!("Steam is rate-limiting, waiting {:.0}s (attempt {}/{})", (w.delay_secs - at.elapsed().as_secs_f64()).max(0.0), w.attempt, w.of));
        (self.label.clone(), self.total, self.done + self.failed, wait)
    }
    /// The text of the progress pill.
    pub fn text(&self) -> String {
        let n = self.done + self.failed;
        let act = pd_filter::net::activity();
        if !act.is_empty() && self.wait.is_none() { return format!("{}  \u{b7}  {act}", self.label); }
        match &self.wait {
            Some((w, at)) => { let left = (w.delay_secs - at.elapsed().as_secs_f64()).max(0.0); format!("{} {n}/{}: Steam is rate-limiting, waiting {:.0}s (attempt {}/{})", self.label, self.total, left, w.attempt, w.of) }
            None if self.total == 0 => self.label.clone(),
            // a store's sync is one task that does not know its size in advance: count the games, not "N/1"
            None if self.label.starts_with("Syncing ") && self.label != "Syncing" => format!("{}: {n} game{}", self.label, if n == 1 { "" } else { "s" }),
            None => format!("{} {n}/{}", self.label, self.total),
        }
    }
}

#[allow(dead_code)] fn _unused(_: &Path) {}

/// `rescrape_artwork` / `artwork_from_steam` / `download_artwork` for one game and one art type.
#[allow(clippy::too_many_arguments)]
fn run_art_op(sources: &pd_filter::sources::Registry, db: &Path, root: &Path, appid: i64, op: ArtOp, sgdb_key: Option<String>, prefs: serde_json::Value, prefer_fit: bool, today: &str, cb: &Arc<dyn Fn(Event) + Send + Sync>) -> Counts {
    use pd_filter::artclient::{Art, ArtStore, Kind as AK};
    let http: Arc<dyn pd_filter::net::Http> = Arc::new(UreqHttp::new());
    // the Edit dialog's art operations are staged: the new picture goes to the staging folder and reaches the library when the dialog is saved (artstore::commit)
    let store = Arc::new(crate::artstore::FsStore { root: root.to_path_buf(), db: db.to_path_buf(), stage: true });
    let hooks = Arc::new(crate::artstore::DbHooks::new(db.to_path_buf(), Some(Arc::new(pd_filter::sources::Registry::builtin_with(root, db)))));
    let art = Art::new(http.clone(), store.clone(), hooks.clone(), sgdb_key);
    let conn = rusqlite::Connection::open(db).ok();
    let (name, icon_hash, platform): (String, String, String) = conn.as_ref().and_then(|c| c.query_row("SELECT COALESCE(name,''), COALESCE(icon_hash,''), COALESCE(platform,'steam') FROM games WHERE appid = ?", [appid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).ok()).unwrap_or_default();
    let (kind, tag) = match &op {
        ArtOp::Order(k) => {
            let saved: Option<Vec<String>> = prefs.get(&platform).and_then(|p| p.get(k.name())).and_then(|l| l.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect());
            let order = grid_core::art::source_order(k.name(), sources.get(&platform).map_or(false, |s| s.caps().art), saved.as_deref(), None);
            let (sgdb_id, nm) = if appid < 0 && !name.is_empty() { (art.sgdb_search_game_id(&name), Some(name.as_str())) } else { (None, None) };
            (*k, art.download_art(*k, appid, None, "auto", order.as_deref(), prefer_fit && *k != AK::Icon, sgdb_id, nm, if *k == AK::Icon { &icon_hash } else { "" }))
        }
        ArtOp::FromSteam(k) => {
            // the Steam copy (duplicate link, else the backfill's match), else a look-up by name kept only when sure (PCGamingWiki or a confirmed match)
            let copy = pd_filter::artclient::Hooks::steam_copy_of(hooks.as_ref(), appid).or_else(|| {
                let pcgw = pd_filter::metadata::Pcgw::new(http.clone());
                match pd_filter::metadata::resolve(http.as_ref(), &pcgw, &name) { (Some(id), pd_filter::metadata::Confidence::Pcgw | pd_filter::metadata::Confidence::Confirmed, _) => Some((id, String::new())), _ => None }
            });
            match copy { Some((sid, ih)) => { pd_filter::net::log(&format!("art from Steam game {sid} for {appid}")); (*k, art.download_from_steam(appid, sid, *k, &ih)) } None => (*k, "missing".into()) }
        }
        ArtOp::Url(k, url) => {
            let u = url.trim();
            let tag = if u.starts_with("http://") || u.starts_with("https://") {
                if art.download_from_url(appid, u, *k) == "custom" {
                    let host = u.split("://").nth(1).and_then(|r| r.split('/').next()).unwrap_or("").split(':').next().unwrap_or("").to_lowercase();
                    if host == "steamgriddb.com" || host.ends_with(".steamgriddb.com") { match k { AK::Vertical => "sgdb_grid", AK::Horizontal => "sgdb_grid_wide", AK::Icon => "sgdb_icon" }.to_string() } else { "custom".to_string() }
                } else { "missing".to_string() }
            } else {
                let path = u.strip_prefix("file://").unwrap_or(u);
                match std::fs::read(path) { Ok(b) if ArtStore::save(store.as_ref(), *k, appid, &b) => "custom".to_string(), _ => "missing".to_string() }
            };
            (*k, tag)
        }
    };
    let ok = tag != "missing";
    pd_filter::net::log(&format!("art op for {appid} ({}, {platform}): {}", kind.name(), if ok { format!("got a picture from {tag}, staged") } else { "no source had a picture".to_string() }));
    // nothing is written to the library here: what the source was (and whether it was the automatic order) is kept with the staged picture, and `artstore::commit` records it when the dialog is saved
    if ok { crate::artstore::stage_note(root, kind, appid, &tag, matches!(op, ArtOp::Order(_))); }
    let _ = (&conn, today);
    cb(if ok { Event::Done { appid, total: 1 } } else { Event::Failed { appid, total: 1 } });
    Counts { done: ok as usize, failed: !ok as usize, ..Default::default() }
}

/// A plugin's library sync: its batches are imported as they arrive (`importer::import`: appids, blacklist, dedupe); `done` counts games added or refreshed.
#[allow(clippy::too_many_arguments)]
fn run_plugin_sync(sources: &pd_filter::sources::Registry, platform: &str, db: &Path, root: &Path, steam: Arc<Steam>, appinfo: Arc<std::collections::HashMap<i64, pd_filter::steamlocal::AppInfo>>, today: &str, cancel: &Cancel, cb: &Arc<dyn Fn(Event) + Send + Sync>) -> Counts {
    use pd_filter::importer::import;
    let Some(src) = sources.get(platform) else { pd_filter::net::log(&format!("sync: no source for {platform}")); return Counts { failed: 1, ..Default::default() } };
    let Ok(mut conn) = rusqlite::Connection::open(db) else { return Counts { failed: 1, ..Default::default() } };
    let _ = conn.busy_timeout(std::time::Duration::from_secs(10));
    let http: Arc<dyn pd_filter::net::Http> = Arc::new(UreqHttp::new());
    let cx = pd_filter::sources::SourceCtx { http, steam, root: root.to_path_buf(), db: db.to_path_buf(), today: today.to_string(),
        appinfo: Arc::new(move |id| appinfo.get(&id).map(|a| AppinfoDates { original: a.original_release_date, steam: a.steam_release_date }).unwrap_or_default()), account_id: None };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let (mut added, mut updated, mut skipped) = (0usize, 0usize, 0usize);
    let mut new_ids: Vec<i64> = vec![];
    let mut sink = |batch: Vec<pd_filter::importer::GameRecord>| {
        match import(&mut conn, platform, &batch, now) {
            Ok(st) => { for _ in 0..(st.added + st.updated) { cb(Event::Done { appid: 0, total: 0 }); } added += st.added; updated += st.updated; skipped += st.skipped_blacklisted; new_ids.extend(st.new_appids); if st.added + st.updated > 0 { library_grew(); } }
            Err(e) => pd_filter::net::log(&format!("sync {platform}: import failed: {e}")),
        }
    };
    let r = src.sync(&cx, &mut sink, cancel);
    pd_filter::net::set_activity("");
    let mut c = Counts { done: added + updated, cancelled: cancel.is_set(), ..Default::default() };
    // stamped when the sync ends, not when it began
    let ended = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(now);
    let status = pd_filter::status::SyncStatus { at: ended, found: *r.as_ref().unwrap_or(&(added + updated + skipped)), added, updated, blocked: skipped, error: r.as_ref().err().cloned() };
    // always leave a trace: the log and the Plugins dialog's "last sync" line (a success used to leave nothing at all)
    pd_filter::net::log(&format!("sync {platform}: {}", pd_filter::status::describe(&status, ended)));
    match pd_filter::status::record(root, platform, &status) {
        Ok(()) => pd_filter::net::log(&format!("sync {platform}: status saved in {}", root.display())),
        Err(e) => pd_filter::net::log(&format!("sync {platform}: the status could not be saved in {}: {e}", root.display())),
    }
    if r.is_err() { c.failed = 1; cb(Event::Failed { appid: 0, total: 0 }); }
    let merged = if r.is_ok() { tidy_same_store(&mut conn, platform, root) } else { 0 };
    if merged > 0 { pd_filter::net::set_note(&format!("{merged} same-store duplicate{} merged", if merged == 1 { "" } else { "s" })); }
    if c.failed == 0 { after_import(src, &cx, &conn, platform, &new_ids, root, db, today, cancel); }
    c
}

/// What follows any import of new games: a source whose list cannot carry everything (GOG, Xbox: ratings, achievements) gets its new games re-scraped once, then the new
/// games get covers and the Steam / PCGamingWiki metadata a store list lacks. A failure here never fails the import.
#[allow(clippy::too_many_arguments)]
/// The games a store sync or a browser import just added, for the app to run the jobs the sync itself does not (HLTB).
static PLUGIN_NEW: std::sync::Mutex<Vec<i64>> = std::sync::Mutex::new(Vec::new());
/// Bumped whenever a running store sync has written games or their covers, so the app can show them while the sync goes on (details and covers of new
/// games can take minutes after the list itself is in).
static LIBRARY_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub fn library_gen() -> u64 { LIBRARY_GEN.load(std::sync::atomic::Ordering::Relaxed) }
fn library_grew() { LIBRARY_GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed); }
pub fn take_plugin_new() -> Vec<i64> { PLUGIN_NEW.lock().map(|mut g| std::mem::take(&mut *g)).unwrap_or_default() }

fn after_import(src: &Arc<dyn pd_filter::sources::LibrarySource>, cx: &pd_filter::sources::SourceCtx, conn: &rusqlite::Connection, platform: &str, new_ids: &[i64], root: &Path, db: &Path, today: &str, cancel: &Cancel) {
    if !new_ids.is_empty() { if let Ok(mut g) = PLUGIN_NEW.lock() { g.extend_from_slice(new_ids); } }
    if src.enrich_new() && !new_ids.is_empty() && !cancel.is_set() {
        for (i, appid) in new_ids.iter().enumerate() {
            if cancel.is_set() { break; }
            pd_filter::net::set_activity(&format!("{}: reading details of new games ({}/{})", src.label(), i + 1, new_ids.len()));
            let row = conn.query_row("SELECT name, platform_id, COALESCE(platform_slug, ''), COALESCE(platform_appname, '') FROM games WHERE appid = ?", [appid], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?)));
            let Ok((name, platform_id, slug, appname)) = row else { continue };
            let g = pd_filter::sources::GameRef { appid: *appid, name, platform: platform.to_string(), platform_id, slug, appname, ..Default::default() };
            if let Ok(Some(u)) = src.rescrape(cx, &g) { if jobs::apply(conn, *appid, &u).is_ok() { library_grew(); } }
        }
        pd_filter::net::set_activity("");
    }
    if !new_ids.is_empty() && !cancel.is_set() {
        // each step shows its count on the status line, like the HLTB job does
        let progress = |what: &'static str, label: String| -> Arc<dyn Fn(Event) + Send + Sync> {
            let n = Arc::new(std::sync::atomic::AtomicUsize::new(0)); let total = new_ids.len();
            Arc::new(move |e| if matches!(e, Event::Done { .. } | Event::Failed { .. }) {
                if matches!(e, Event::Done { .. }) { library_grew(); }   // a cover or details arrived: shown while the rest still runs
                let i = n.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                pd_filter::net::set_activity(&format!("{label}: {what} ({}/{total})", i.min(total)));
            })
        };
        let (art_cb, fill_cb) = (progress("covers for new games", src.label().to_string()), progress("looking up details of new games (PCGamingWiki, Steam)", src.label().to_string()));
        pd_filter::net::set_activity(&format!("{}: covers for new games (0/{})", src.label(), new_ids.len()));
        let http2: Arc<dyn pd_filter::net::Http> = Arc::new(UreqHttp::new());
        let st2 = cx.steam.clone(); let ai2 = cx.appinfo.clone();
        let store = Arc::new(crate::artstore::FsStore { root: root.to_path_buf(), db: db.to_path_buf(), stage: false });
        let art = Arc::new(pd_filter::artclient::Art::new(http2.clone(), store, Arc::new(crate::artstore::DbHooks::new(db.to_path_buf(), Some(Arc::new(pd_filter::sources::Registry::builtin_with(root, db))))), pd_filter::account::sgdb_key(root)));
        let has_store = src.caps().art;
        let orders: Arc<dyn Fn(i64, pd_filter::artclient::Kind) -> (Option<Vec<String>>, bool) + Send + Sync> = Arc::new(move |_, k| (grid_core::art::source_order(k.name(), has_store, None, None), k != pd_filter::artclient::Kind::Icon));
        jobs::bulk_art(db.to_path_buf(), art, orders, vec![pd_filter::artclient::Kind::Vertical, pd_filter::artclient::Kind::Horizontal, pd_filter::artclient::Kind::Icon], "auto".into(), today.to_string(), new_ids.to_vec(), cancel.clone(), art_cb, None);
        jobs::bulk_backfill(db.to_path_buf(), st2, http2, Arc::new(pd_filter::sources::Registry::builtin_with(root, db)), Arc::new(move |id| ai2(id)), today.to_string(), new_ids.to_vec(), std::time::Duration::from_millis(300), cancel.clone(), fill_cb);
        pd_filter::net::set_activity("");
    }
}

/// A store's download-and-unpack for one game: progress goes to the status line, the result to the database (installed, folder, chosen executable) and a note for the app to show.
#[allow(clippy::too_many_arguments)]
/// Bulk Ops > Date importer > purchase dates: for the non-Steam games of the selection, ask each store's account when the game was bought and write it as Date Added
/// (PlayDate's `fetch_purchase_dates` plus its bulk import, which overwrites). A store that cannot do it, or whose account refuses, fails its games with one note.
#[allow(clippy::too_many_arguments)]
fn run_purchase_dates(sources: &pd_filter::sources::Registry, db: &Path, root: &Path, steam: Arc<Steam>, appinfo: Arc<std::collections::HashMap<i64, pd_filter::steamlocal::AppInfo>>, today: &str, appids: &[i64], cancel: &Cancel, cb: &Arc<dyn Fn(Event) + Send + Sync>) -> Counts {
    let total = appids.len();
    let Ok(conn) = rusqlite::Connection::open(db) else { return Counts { failed: total, ..Default::default() } };
    let _ = conn.busy_timeout(std::time::Duration::from_secs(10));
    let mut by_platform: std::collections::BTreeMap<String, Vec<i64>> = Default::default();
    for id in appids {
        let plat: String = conn.query_row("SELECT COALESCE(platform,'steam') FROM games WHERE appid = ?", [id], |r| r.get(0)).unwrap_or_else(|_| "steam".into());
        by_platform.entry(plat).or_default().push(*id);
    }
    let http: Arc<dyn pd_filter::net::Http> = Arc::new(UreqHttp::new());
    let appinfo2 = appinfo.clone();
    let cx = pd_filter::sources::SourceCtx { http, steam, root: root.to_path_buf(), db: db.to_path_buf(), today: today.to_string(),
        appinfo: Arc::new(move |id| appinfo2.get(&id).map(|a| AppinfoDates { original: a.original_release_date, steam: a.steam_release_date }).unwrap_or_default()), account_id: None };
    let mut counts = Counts::default();
    let mut notes: Vec<String> = vec![];
    for (plat, ids) in by_platform {
        if cancel.is_set() { counts.cancelled = true; break; }
        let label = sources.label(&plat);
        let Some(src) = sources.get(&plat).filter(|s| s.caps().purchase_dates) else {
            // Steam games have their own route (activation dates in the browser); a store without a date feed has nothing to read
            if plat != "steam" { notes.push(format!("{label} has no purchase dates to read")); }
            for id in ids { counts.failed += 1; cb(Event::Failed { appid: id, total }); }
            continue;
        };
        pd_filter::net::set_activity(&format!("{label}: reading purchase dates"));
        match src.purchase_dates(&cx, &ids) {
            Ok(found) => for (id, ts) in found {
                match ts {
                    Some(t) => { let _ = conn.execute("UPDATE games SET date_added = ?1 WHERE appid = ?2", rusqlite::params![t, id]); counts.done += 1; cb(Event::Done { appid: id, total }); }
                    None => { counts.failed += 1; cb(Event::Failed { appid: id, total }); }
                }
            },
            Err(e) => { pd_filter::net::log(&format!("purchase dates {plat}: {e}")); notes.push(e); for id in ids { counts.failed += 1; cb(Event::Failed { appid: id, total }); } }
        }
    }
    pd_filter::net::set_activity("");
    if !notes.is_empty() { pd_filter::net::set_note(&notes.join(" ")); }
    counts
}

fn run_plugin_install(sources: &pd_filter::sources::Registry, platform: &str, appid: i64, db: &Path, root: &Path, steam: Arc<Steam>, appinfo: Arc<std::collections::HashMap<i64, pd_filter::steamlocal::AppInfo>>, today: &str, cancel: &Cancel) -> Counts {
    let fail = |m: String| { pd_filter::net::log(&format!("install: {m}")); pd_filter::net::set_note(&m); pd_filter::net::set_activity(""); Counts { failed: 1, cancelled: cancel.is_set(), ..Default::default() } };
    let Some(src) = sources.get(platform) else { return fail(format!("no source for {platform}")) };
    let Ok(conn) = rusqlite::Connection::open(db) else { return fail("the library could not be opened".into()) };
    let _ = conn.busy_timeout(std::time::Duration::from_secs(10));
    let g = conn.query_row("SELECT COALESCE(name,''), COALESCE(platform_id,''), COALESCE(platform_slug,''), COALESCE(platform_appname,''), COALESCE(installed,0), install_path, platform_executable FROM games WHERE appid = ?", [appid],
        |r| Ok(pd_filter::sources::GameRef { appid, name: r.get(0)?, platform: platform.to_string(), platform_id: r.get(1)?, slug: r.get(2)?, appname: r.get(3)?, installed: r.get::<_, i64>(4)? != 0, install_path: r.get::<_, Option<String>>(5)?.filter(|p| !p.is_empty()), executable: r.get::<_, Option<String>>(6)?.filter(|p| !p.is_empty()) }));
    let Ok(g) = g else { return fail("that game is not in the library".into()) };
    let http: Arc<dyn pd_filter::net::Http> = Arc::new(UreqHttp::new());
    let cx = pd_filter::sources::SourceCtx { http, steam, root: root.to_path_buf(), db: db.to_path_buf(), today: today.to_string(),
        appinfo: Arc::new(move |id| appinfo.get(&id).map(|a| AppinfoDates { original: a.original_release_date, steam: a.steam_release_date }).unwrap_or_default()), account_id: None };
    let r = src.install(&cx, &g, &mut |text, _, _| pd_filter::net::set_activity(text), cancel);
    pd_filter::net::set_activity("");
    match r {
        Ok(done) => {
            let _ = conn.execute("UPDATE games SET installed = 1, install_path = ?1, platform_executable = COALESCE(?2, platform_executable) WHERE appid = ?3", rusqlite::params![done.install_path, done.executable, appid]);
            pd_filter::net::log(&format!("install: {} is in {}", g.name, done.install_path));
            pd_filter::net::set_note(&done.note.unwrap_or_else(|| format!("Installed {}. Launch it again to play.", g.name)));
            Counts { done: 1, ..Default::default() }
        }
        Err(e) => if e == "Cancelled" { pd_filter::net::set_note("Install cancelled"); Counts { cancelled: true, ..Default::default() } } else { fail(e) },
    }
}

/// After an import (or a sync that found nothing new): fold this store's same-name copies into the earliest, delete the removed rows' cached covers and leave a note for the status line.
fn tidy_same_store(conn: &mut rusqlite::Connection, platform: &str, root: &Path) -> usize {
    match pd_filter::importer::merge_same_store(conn, platform) {
        Ok(m) => {
            for appid in &m.removed { for sub in ["vertical", "horizontal", "icons"] { let _ = std::fs::remove_file(root.join("static/img/library").join(sub).join(format!("{appid}.jpg"))); } }
            if m.merged > 0 { pd_filter::net::log(&format!("{platform}: {} same-store copies merged into the earliest ({:?})", m.merged, m.removed)); }
            m.merged
        }
        Err(e) => { pd_filter::net::log(&format!("{platform}: merging same-store copies failed: {e}")); 0 }
    }
}


/// "Which build?": finds the builds a store offers for one game on a worker thread (it needs the network) and leaves the answer in `slot` for the window to show.
pub fn fetch_install_options(src: Arc<dyn pd_filter::sources::LibrarySource>, g: pd_filter::sources::GameRef, root: std::path::PathBuf, db: std::path::PathBuf,
    slot: Arc<std::sync::Mutex<Option<(i64, String, Result<Vec<pd_filter::sources::InstallOption>, String>)>>>, ctx: egui::Context) {
    std::thread::spawn(move || {
        let http: Arc<dyn pd_filter::net::Http> = Arc::new(UreqHttp::new());
        let steam = Arc::new(Steam::new(http.clone(), None, 10.0));
        let cx = pd_filter::sources::SourceCtx { http, steam, root, db, today: String::new(), appinfo: Arc::new(|_| AppinfoDates::default()), account_id: None };
        let r = src.install_options(&cx, &g);
        if let Ok(mut s) = slot.lock() { *s = Some((g.appid, g.platform.clone(), r)); }
        ctx.request_repaint();
    });
}
