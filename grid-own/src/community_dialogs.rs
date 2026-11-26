//! Community Tools: the Secret Santa / Snowballs gift list and the Monthly in a Month filter builder.
use crate::filter_dialog::FilterDialog;
use crate::ui_kit::{self, Kind, MUTED, TEXT};
use egui::{Color32, RichText};
use pd_filter::miam;
use pd_filter::pagywosg_event::{load_gifts, save_gifts, Gift};
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::mpsc;

const GOOD: Color32 = Color32::from_rgb(0x5c, 0xb8, 0x5c);
const BAD: Color32 = Color32::from_rgb(0xe0, 0x6c, 0x6c);
const STATUSES: [&str; 5] = ["Never Played", "Unfinished", "Beaten", "Completed", "Won't Play"];

fn read_db(path: &PathBuf) -> Option<rusqlite::Connection> { rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok() }

// ───────────────────────── Secret Santa / Snowballs ─────────────────────────

pub struct SantaDialog {
    db: PathBuf, root: PathBuf, groups: Vec<String>,
    gifts: Vec<Gift>, saved: Vec<Gift>,
    search: String, results: Vec<(i64, String)>, searched: String,
    group: String, year: String, status: Option<(String, bool)>,
}

impl SantaDialog {
    pub fn open(db: PathBuf, root: PathBuf, groups: Vec<String>) -> SantaDialog {
        let gifts = load_gifts(&root);
        SantaDialog { db, root, groups, saved: gifts.clone(), gifts, search: String::new(), results: vec![], searched: String::new(), group: String::new(), year: String::new(), status: None }
    }
    fn dirty(&self) -> bool { self.gifts != self.saved }
    fn add(&mut self, appid: i64, name: String, year: Option<i64>) -> bool {
        if self.gifts.iter().any(|g| g.appid == appid) { return false; }
        self.gifts.push(Gift { appid, name, year });
        true
    }
    fn lookup(&self, sql: &str, p: String) -> Vec<(i64, String)> {
        let Some(c) = read_db(&self.db) else { return vec![] };
        let Ok(mut st) = c.prepare(sql) else { return vec![] };
        st.query_map([p], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?.unwrap_or_default()))).map(|it| it.flatten().collect()).unwrap_or_default()
    }

    /// Returns true when the dialog should close.
    pub fn show(&mut self, ctx: &egui::Context) -> bool {
        let accent = ui_kit::accent();
        let (mut save, mut close) = (false, false);
        let (dirty, status) = (self.dirty(), self.status.clone());
        let o = ui_kit::modal_full(ctx, "santa", "Secret Santa / Snowballs", 32.0, |_, _| {}, |ui, r| {
            ui.label(ui_kit::muted("Games received as PAGYWOSG / Play or Pay Discord Secret Santa or Snowball gifts. They count as wins: tick the box in the PAGYWOSG filter builder to include them.", 0.85 * r));
            ui.add_space(0.3 * r);
            FilterDialog::input(ui, &mut self.search, f32::INFINITY, r, "Find a single game...");
            let q = self.search.trim().to_string();
            if q != self.searched {
                self.searched = q.clone();
                self.results = if q.len() < 2 { vec![] } else { self.lookup("SELECT appid, name FROM games WHERE platform = 'steam' AND duplicate_of IS NULL AND name LIKE ? ORDER BY name COLLATE NOCASE LIMIT 12", format!("%{q}%")) };
            }
            let mut add = None;
            for (a, n) in &self.results { if ui_kit::button(ui, &format!("+ {n}"), Kind::Plain, accent, r).clicked() { add = Some((*a, n.clone())); } }
            if let Some((a, n)) = add { self.add(a, n, None); self.search.clear(); self.searched.clear(); self.results.clear(); }
            ui.add_space(0.3 * r);
            ui.horizontal(|ui| {
                let opts: Vec<(String, String)> = self.groups.iter().map(|g| (g.clone(), g.clone())).collect();
                let shown = if self.group.is_empty() { "Add all games from a group...".to_string() } else { self.group.clone() };
                if let Some(p) = FilterDialog::combo(ui, egui::Id::new("santa_group"), &shown, &opts, 16.0 * r, r) { self.group = p; }
                FilterDialog::input(ui, &mut self.year, 4.5 * r, r, "Year");
                if ui_kit::button(ui, "Add group", Kind::Plain, accent, r).clicked() && !self.group.is_empty() {
                    let year = self.year.trim().parse::<i64>().ok();
                    let found = self.lookup("SELECT appid, name FROM games WHERE platform = 'steam' AND duplicate_of IS NULL AND ',' || groups || ',' LIKE ? ORDER BY name COLLATE NOCASE", format!("%,{},%", self.group));
                    let n = found.into_iter().filter(|(a, n)| self.add(*a, n.clone(), year)).count();
                    self.status = Some((format!("Added {n} game{} from \"{}\".", if n == 1 { "" } else { "s" }, self.group), true));
                }
            });
            ui.add_space(0.3 * r);
            ui.label(RichText::new(format!("GIFT LIST ({})", self.gifts.len())).size(0.78 * r).color(MUTED()));
            let mut remove = None;
            egui::ScrollArea::vertical().max_height(18.0 * r).auto_shrink([false, true]).show(ui, |ui| {
                for (i, g) in self.gifts.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.allocate_ui_with_layout(egui::vec2(17.0 * r, 1.8 * r), egui::Layout::left_to_right(egui::Align::Center), |ui| { ui.add(egui::Label::new(RichText::new(&g.name).size(0.88 * r).color(TEXT())).truncate()); });
                        let mut y = g.year.map(|y| y.to_string()).unwrap_or_default();
                        if FilterDialog::input(ui, &mut y, 4.5 * r, r, "Year").changed() { g.year = y.trim().parse().ok(); }
                        if ui_kit::button(ui, "\u{2715}", Kind::Plain, accent, r).clicked() { remove = Some(i); }
                    });
                }
            });
            if let Some(i) = remove { self.gifts.remove(i); }
        }, |ui, r| {
            ui.horizontal(|ui| {
                if ui_kit::button(ui, "Save", if dirty { Kind::Primary } else { Kind::Plain }, accent, r).clicked() { save = true; }
                if ui_kit::button(ui, "Close", Kind::Plain, accent, r).clicked() { close = true; }
                if let Some((t, good)) = &status { ui.label(RichText::new(t).size(0.85 * r).color(if *good { GOOD } else { BAD })); }
            });
        });
        if save {
            self.status = Some(match save_gifts(&self.root, &self.gifts) { Ok(()) => { self.saved = self.gifts.clone(); (format!("Saved {} gifts.", self.gifts.len()), true) } Err(e) => (format!("Could not save: {e}"), false) });
        }
        o.closed || close
    }
}

// ───────────────────────── Monthly in a Month ─────────────────────────

pub struct MiamDialog {
    db: PathBuf, saved: Map<String, Value>,
    rx: Option<mpsc::Receiver<Result<(miam::Sheet, Vec<i64>), String>>>, sheet: Option<(miam::Sheet, Vec<i64>)>, error: Option<String>,
    statuses: BTreeSet<String>, name: String, status: Option<(String, bool)>, confirm: bool,
}

pub struct MiamOut { pub close: bool, pub saved: Option<Map<String, Value>> }

impl MiamDialog {
    pub fn open(db: PathBuf, saved: Map<String, Value>, ctx: &egui::Context) -> MiamDialog {
        let (tx, rx) = mpsc::channel();
        let (db2, ctx2) = (db.clone(), ctx.clone());
        std::thread::spawn(move || {
            let res = (|| {
                let sheet = miam::fetch_sheet(&pd_filter::net::UreqHttp::new())?;
                let c = read_db(&db2).ok_or("the library could not be opened")?;
                let mut owned = std::collections::HashSet::new();
                let mut st = c.prepare("SELECT appid FROM games WHERE platform = 'steam'").map_err(|e| e.to_string())?;
                for a in st.query_map([], |r| r.get::<_, i64>(0)).map_err(|e| e.to_string())?.flatten() { owned.insert(a); }
                let mine: Vec<i64> = sheet.eligible.iter().copied().filter(|a| owned.contains(a)).collect();
                Ok((sheet, mine))
            })();
            let _ = tx.send(res);
            ctx2.request_repaint();
        });
        MiamDialog { db, saved, rx: Some(rx), sheet: None, error: None, statuses: ["Never Played", "Unfinished"].iter().map(|s| s.to_string()).collect(), name: "Monthly in a Month".into(), status: None, confirm: false }
    }
    pub fn settled(&self) -> bool { self.rx.is_none() }

    pub fn show(&mut self, ctx: &egui::Context) -> MiamOut {
        if let Some(rx) = &self.rx {
            match rx.try_recv() {
                Ok(Ok(s)) => { self.sheet = Some(s); self.rx = None; }
                Ok(Err(e)) => { self.error = Some(e); self.rx = None; }
                Err(mpsc::TryRecvError::Empty) => { ctx.request_repaint_after(std::time::Duration::from_millis(200)); }
                Err(_) => { self.error = Some("the loader stopped unexpectedly".into()); self.rx = None; }
            }
        }
        let accent = ui_kit::accent();
        let mut out = MiamOut { close: false, saved: None };
        let _ = &self.db;
        let (mut save, mut close) = (false, false);
        let mut name = std::mem::take(&mut self.name);
        let status = self.status.clone();
        let o = ui_kit::modal_full(ctx, "miam", "Monthly in a Month", 34.0, |_, _| {}, |ui, r| {
            ui.label(ui_kit::muted("Beat 6 games from the community list. The filter lists the games on it that you own, in the statuses you choose.", 0.85 * r));
            ui.add_space(0.3 * r);
            ui.horizontal_wrapped(|ui| {
                ui.label(ui_kit::muted("Completion status", 0.85 * r));
                for s in STATUSES { let on = self.statuses.contains(s); if ui_kit::pill(ui, s, on, accent, 0.78 * r).clicked() { if on { self.statuses.remove(s); } else { self.statuses.insert(s.to_string()); } } }
            });
            ui.add_space(0.3 * r);
            match (&self.sheet, &self.error) {
                (Some((s, mine)), _) => {
                    egui::Grid::new("miam_stats").num_columns(2).spacing([16.0, 2.0]).show(ui, |ui| {
                        for (k, v) in [("Games on list", s.total), ("Excluded (N / N?)", s.total - s.eligible.len()), ("Eligible from list", s.eligible.len()), ("In your library", mine.len())] {
                            ui.label(ui_kit::muted(k, 0.85 * r)); ui.label(RichText::new(v.to_string()).strong().size(0.9 * r).color(TEXT())); ui.end_row();
                        }
                    });
                }
                (None, Some(e)) => { ui.label(RichText::new(format!("Could not fetch the game list: {e}")).color(BAD).size(0.88 * r)); }
                _ => { ui.label(ui_kit::muted("Fetching the game list...", 0.88 * r)); }
            }
        }, |ui, r| {
            ui.horizontal(|ui| {
                FilterDialog::input(ui, &mut name, 16.0 * r, r, "Filter name...");
                if ui_kit::button(ui, "Save filter", Kind::Primary, accent, r).clicked() { save = true; }
                if ui_kit::button(ui, "Close", Kind::Plain, accent, r).clicked() { close = true; }
            });
            if let Some((t, good)) = &status { ui.label(RichText::new(t).size(0.85 * r).color(if *good { GOOD } else { BAD })); }
        });
        self.name = name;
        if save {
            let name = self.name.trim().to_string();
            self.status = Some(match (&self.sheet, name.is_empty(), self.statuses.is_empty()) {
                (None, _, _) => ("The game list has not loaded.".into(), false),
                (_, true, _) => ("Give the filter a name first.".into(), false),
                (_, _, true) => ("Select at least one completion status.".into(), false),
                (Some((_, mine)), _, _) if mine.is_empty() => ("None of the games on the list are in your library: nothing to save.".into(), false),
                (Some((_, mine)), _, _) => {
                    if self.saved.contains_key(&name) && !self.confirm { self.confirm = true; ("A filter with that name exists. Press Save again to replace it.".into(), false) }
                    else {
                        let id = self.saved.get(&name).and_then(|e| e.get("id")).and_then(|v| v.as_str()).map(String::from).unwrap_or_else(crate::filter_model::new_id);
                        let st: Vec<String> = STATUSES.iter().filter(|s| self.statuses.contains(**s)).map(|s| s.to_string()).collect();
                        self.saved.insert(name.clone(), json!({"id": id, "tree": miam::tree(mine, &st)}));
                        out.saved = Some(self.saved.clone());
                        self.confirm = false;
                        (format!("Saved as \"{name}\"."), true)
                    }
                }
            });
        }
        out.close = o.closed || close;
        out
    }
}

// ───────────────────────── BLAEO ─────────────────────────

use pd_filter::blaeo;

enum BPhase { Loading, Failed(String), Ready }

pub struct BlaeoDialog {
    db: PathBuf, gs_path: PathBuf,
    rx: Option<mpsc::Receiver<Result<(blaeo::Plan, blaeo::GroupSources), String>>>,
    phase: BPhase, plan: blaeo::Plan, gs: blaeo::GroupSources, sel: blaeo::Selection, status: Option<(String, bool)>,
}

pub struct BlaeoOut { pub close: bool, /// The library changed (reload what is shown).
    pub changed: bool }

impl BlaeoDialog {
    pub fn open(db: PathBuf, root: PathBuf, steam_id: String, ctx: &egui::Context) -> BlaeoDialog {
        let account_id = db.file_stem().and_then(|n| n.to_str()).and_then(|n| n.strip_prefix("games_")).map(String::from);
        let gs_path = root.join(match &account_id { Some(id) => format!("group_sources_{id}.json"), None => "group_sources.json".into() });
        let (tx, rx) = mpsc::channel();
        let (db2, gs2, ctx2) = (db.clone(), gs_path.clone(), ctx.clone());
        std::thread::spawn(move || {
            let res = (|| {
                let rows = blaeo::fetch_all(&pd_filter::net::UreqHttp::new(), &blaeo::profile_url(&steam_id), std::time::Duration::from_millis(500))?;
                let conn = read_db(&db2).ok_or("the library could not be opened")?;
                let local = blaeo::load_local(&conn, &rows.iter().map(|r| r.appid).collect::<Vec<_>>());
                let gs = blaeo::GroupSources::load(&gs2);
                Ok((blaeo::plan(rows, &local, &gs), gs))
            })();
            let _ = tx.send(res);
            ctx2.request_repaint();
        });
        BlaeoDialog { db, gs_path, rx: Some(rx), phase: BPhase::Loading, plan: Default::default(), gs: blaeo::GroupSources::empty(), sel: Default::default(), status: None }
    }
    pub fn settled(&self) -> bool { self.rx.is_none() }

    fn apply(&mut self) -> bool {
        let res = (|| -> Result<blaeo::Applied, String> {
            let conn = rusqlite::Connection::open(&self.db).map_err(|e| e.to_string())?;
            conn.busy_timeout(std::time::Duration::from_secs(10)).ok();
            let d = blaeo::apply(&conn, &mut self.gs, &self.plan, &self.sel).map_err(|e| e.to_string())?;
            self.gs.save(&self.gs_path).map_err(|e| format!("could not save the group ownership file: {e}"))?;
            Ok(d)
        })();
        match res {
            Ok(d) => {
                let mut parts = vec![format!("{} status change{}", d.status, if d.status == 1 { "" } else { "s" }), format!("{} added to lists", d.additions), format!("{} removed from lists", d.removals), format!("{} renamed list{}", d.renames, if d.renames == 1 { "" } else { "s" })];
                if d.protected > 0 { parts.push(format!("{} kept because something else also put them there", d.protected)); }
                self.status = Some((format!("Applied: {}.", parts.join(", ")), true));
                // what was applied is now the library's state: look again at BLAEO next time
                self.plan = blaeo::Plan { rows: std::mem::take(&mut self.plan.rows), lists: self.plan.lists.clone(), members: self.plan.members.clone(), ..Default::default() };
                self.sel = Default::default();
                true
            }
            Err(e) => { self.status = Some((e, false)); false }
        }
    }

    /// Pick up the loader's answer (a closed dialog that is still reading BLAEO keeps being polled by the app, so the menu can say when it is done).
    pub fn poll(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.rx {
            match rx.try_recv() {
                Ok(Ok((p, gs))) => { self.sel = blaeo::Selection::default_for(&p); self.plan = p; self.gs = gs; self.phase = BPhase::Ready; self.rx = None; }
                Ok(Err(e)) => { self.phase = BPhase::Failed(e); self.rx = None; }
                Err(mpsc::TryRecvError::Empty) => { ctx.request_repaint_after(std::time::Duration::from_millis(250)); }
                Err(_) => { self.phase = BPhase::Failed("the loader stopped unexpectedly".into()); self.rx = None; }
            }
        }
    }
    /// Still reading BLAEO.
    pub fn loading(&self) -> bool { matches!(self.phase, BPhase::Loading) }
    /// The menu line for a finished background read: "BLAEO results ready" or "BLAEO sync failed" (PlayDate's hamburger entry); None while it is still loading.
    pub fn notice(&self) -> Option<&'static str> { match self.phase { BPhase::Loading => None, BPhase::Failed(_) => Some("BLAEO sync failed"), BPhase::Ready => Some("BLAEO results ready") } }

    pub fn show(&mut self, ctx: &egui::Context) -> BlaeoOut {
        self.poll(ctx);
        let accent = ui_kit::accent();
        let (mut apply, mut close) = (false, false);
        let screen_h = ctx.content_rect().height();
        let status = self.status.clone();
        let o = ui_kit::modal_full(ctx, "blaeo", "Sync BLAEO", 44.0, |_, _| {}, |ui, r| {
            ui.label(ui_kit::muted("What your BLAEO account says compared with this library. Tick what to take over; nothing changes until you press Apply. A status change that moves a game back starts unticked.", 0.85 * r));
            ui.add_space(0.3 * r);
            match &self.phase {
                BPhase::Loading => { ui.label(ui_kit::muted("Reading your BLAEO games...", 0.9 * r)); }
                BPhase::Failed(e) => { ui.label(RichText::new(e).color(BAD).size(0.9 * r)); }
                BPhase::Ready if self.plan.is_empty() => { ui.label(RichText::new(format!("Nothing to change: {} games on BLAEO agree with the library.", self.plan.rows.len())).color(TEXT()).size(0.9 * r)); }
                BPhase::Ready => {
                    egui::ScrollArea::vertical().max_height((screen_h - 22.0 * r).max(12.0 * r)).auto_shrink([false, true]).show(ui, |ui| {
                        let p = &self.plan;
                        let sel = &mut self.sel;
                        section(ui, r, accent, "STATUS CHANGES", p.status_changes.len(), &mut sel.status, p.status_changes.iter().map(|c| (c.appid, format!("{}: {} \u{2192} {}{}", c.name, c.from, c.to, if c.lowers { "   (moves the game back)" } else { "" }))).collect());
                        section(ui, r, accent, "ADDED TO LISTS", p.additions.len(), &mut sel.additions, p.additions.iter().map(|a| (a.appid, format!("{}: + {}", a.name, a.lists.join(", ")))).collect());
                        let mut rem: HashSet<i64> = sel.removals.iter().map(|(a, _)| *a).collect();
                        let before = rem.clone();
                        section(ui, r, accent, "REMOVED FROM LISTS", p.removals.len(), &mut rem, p.removals.iter().map(|x| (x.appid, format!("{}: \u{2212} {}", x.name, x.list_name))).collect());
                        if rem != before { sel.removals = p.removals.iter().filter(|x| rem.contains(&x.appid)).map(|x| (x.appid, x.list_name.clone())).collect(); }
                        if !p.renames.is_empty() {
                            ui.label(RichText::new("RENAMED LISTS").strong().size(0.78 * r).color(MUTED()));
                            for x in &p.renames {
                                let key = (x.from.clone(), x.to.clone());
                                let mut on = sel.renames.contains(&key);
                                if ui.checkbox(&mut on, RichText::new(format!("{} \u{2192} {}", x.from, x.to)).size(0.85 * r).color(TEXT())).changed() { if on { sel.renames.insert(key); } else { sel.renames.remove(&key); } }
                            }
                        }
                    });
                }
            }
        }, |ui, r| {
            ui.horizontal(|ui| {
                let can = matches!(self.phase, BPhase::Ready) && !self.plan.is_empty();
                if ui_kit::button(ui, "Apply", if can { Kind::Primary } else { Kind::Plain }, accent, r).clicked() && can { apply = true; }
                if ui_kit::button(ui, "Close", Kind::Plain, accent, r).clicked() { close = true; }
            });
            if let Some((t, good)) = &status { ui.label(RichText::new(t).size(0.85 * r).color(if *good { GOOD } else { BAD })); }
        });
        let changed = apply && self.apply();
        BlaeoOut { close: o.closed || close, changed }
    }
}

use std::collections::HashSet;

/// One checklist: a heading with Select all / none and a row per game, keyed by appid.
fn section(ui: &mut egui::Ui, r: f32, accent: Color32, title: &str, n: usize, on: &mut HashSet<i64>, rows: Vec<(i64, String)>) {
    if n == 0 { return; }
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("{title} ({n})")).strong().size(0.78 * r).color(MUTED()));
        if ui_kit::button(ui, "All", Kind::Plain, accent, r).clicked() { for (a, _) in &rows { on.insert(*a); } }
        if ui_kit::button(ui, "None", Kind::Plain, accent, r).clicked() { for (a, _) in &rows { on.remove(a); } }
    });
    for (a, text) in rows.iter().take(500) {
        let mut v = on.contains(a);
        if ui.checkbox(&mut v, RichText::new(text).size(0.85 * r).color(TEXT())).changed() { if v { on.insert(*a); } else { on.remove(a); } }
    }
    if rows.len() > 500 { ui.label(ui_kit::muted(format!("... and {} more (All / None apply to every one)", rows.len() - 500), 0.8 * r)); }
    ui.add_space(0.4 * r);
}

// ───────────────────────── SteamGifts wins that are not in the library ─────────────────────────

use pd_filter::steamgifts::{self as sgw, UnmatchedRow};

pub struct SgUnmatchedDialog {
    db: PathBuf, store_path: PathBuf, gs_path: PathBuf, steam_root: Option<PathBuf>, steam_id: String,
    rows: Vec<UnmatchedRow>, status: Option<(String, bool)>, filter: String,
}

pub struct SgUnmatchedOut { pub close: bool, /// Games added to the library by this dialog (their art is fetched next).
    pub added: Vec<i64>, pub changed: bool }

impl SgUnmatchedDialog {
    pub fn open(db: PathBuf, root: PathBuf, steam_root: Option<PathBuf>, steam_id: String) -> SgUnmatchedDialog {
        let account_id = db.file_stem().and_then(|n| n.to_str()).and_then(|n| n.strip_prefix("games_")).map(String::from);
        let gs_path = root.join(match &account_id { Some(id) => format!("group_sources_{id}.json"), None => "group_sources.json".into() });
        let store_path = root.join(format!("steamgifts_wins_{steam_id}.json"));
        let mut d = SgUnmatchedDialog { db, store_path, gs_path, steam_root, steam_id, rows: vec![], status: None, filter: String::new() };
        d.reload();
        d
    }
    fn reload(&mut self) {
        let store = sgw::load_store(&self.store_path);
        let lib: HashSet<i64> = read_db(&self.db).and_then(|c| { let mut st = c.prepare("SELECT appid FROM games").ok()?; let v: HashSet<i64> = st.query_map([], |r| r.get::<_, i64>(0)).ok()?.flatten().collect(); Some(v) }).unwrap_or_default();
        self.rows = sgw::unmatched_rows(&store, &lib);
    }
    /// What Steam's own files here know about a game (the store no longer lists it).
    fn local(&self, appid: i64) -> sgw::LocalSteam {
        let Some(root) = &self.steam_root else { return Default::default() };
        let info = pd_filter::steamlocal::read_appinfo(root).get(&appid).cloned();
        let lib = pd_filter::steamlocal::local_library(root, Some(&self.steam_id)).into_iter().find(|g| g.appid == appid);
        let installed = pd_filter::steamlocal::installed_appids(&pd_filter::steamlocal::manifests(root)).contains(&appid);
        sgw::LocalSteam { name: info.as_ref().map(|i| i.name.clone()), release_date: info.as_ref().and_then(|i| i.steam_release_date.or(i.original_release_date)), playtime: lib.as_ref().map_or(0, |g| g.playtime_forever),
            last_played: lib.and_then(|g| g.last_played), installed, cheevos: pd_filter::steamstats::account32(&self.steam_id).and_then(|a| pd_filter::steamstats::local_counts(root, a, appid)) }
    }
    fn run(&mut self, code: &str, credit: bool) -> Result<Option<i64>, String> {
        let conn = rusqlite::Connection::open(&self.db).map_err(|e| e.to_string())?;
        conn.busy_timeout(std::time::Duration::from_secs(10)).ok();
        let mut store = sgw::load_store(&self.store_path);
        let mut gs = blaeo::GroupSources::load(&self.gs_path);
        let out = if credit {
            sgw::adopt_dlc_base(&conn, &mut gs, &mut store, code)?;
            None
        } else {
            let http = pd_filter::net::UreqHttp::new();
            let steam = sgw::WebSteam::new(&http, std::time::Duration::ZERO);
            let today: String = conn.query_row("SELECT date('now','localtime')", [], |r| r.get(0)).map_err(|e| e.to_string())?;
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
            let a = sgw::adopt_win(&conn, &mut gs, &mut store, code, &steam, &|id| self.local(id), &today, now)?;
            self.status = Some((format!("Added {} to your library and to \"Won on SteamGifts\".", a.name), true));
            (!a.already_present).then_some(a.appid)
        };
        sgw::save_store(&self.store_path, &store).map_err(|e| e.to_string())?;
        gs.save(&self.gs_path).map_err(|e| e.to_string())?;
        Ok(out)
    }

    pub fn show(&mut self, ctx: &egui::Context) -> SgUnmatchedOut {
        let accent = ui_kit::accent();
        let (mut close, mut act): (bool, Option<(String, bool)>) = (false, None);
        let screen_h = ctx.content_rect().height();
        let status = self.status.clone();
        let o = ui_kit::modal_full(ctx, "sg_unmatched", "SteamGifts wins not in your library", 54.0, |_, _| {}, |ui, r| {
            ui.label(ui_kit::muted("Wins from your last sync that did not match a game in this library, each with the reason. A delisted game you still own can be added from Steam's own files on this computer; a DLC or soundtrack can be credited to its base game if you own that.", 0.85 * r));
            ui.add_space(0.3 * r);
            if self.rows.is_empty() { ui.label(ui_kit::muted("Nothing to show. Run Sync SteamGifts Wins first, or every win matched.", 0.9 * r)); return; }
            FilterDialog::input(ui, &mut self.filter, f32::INFINITY, r, "Search wins...");
            let q = self.filter.to_lowercase();
            egui::ScrollArea::vertical().max_height((screen_h - 20.0 * r).max(12.0 * r)).auto_shrink([false, true]).show(ui, |ui| {
                for row in self.rows.iter().filter(|x| q.is_empty() || x.name.to_lowercase().contains(&q) || x.reason.to_lowercase().contains(&q)) {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(&row.name).size(0.9 * r).color(TEXT()));
                        ui.label(ui_kit::muted(format!("{}{}", row.reason, if row.received == Some(true) { "" } else { "  (not confirmed received)" }), 0.8 * r));
                        if row.adoptable && ui_kit::button(ui, "Add to library", Kind::Primary, accent, r).clicked() { act = Some((row.code.clone(), false)); }
                        if row.dlc_base_owned && ui_kit::button(ui, &format!("Credit {}", row.dlc_base_name.clone().unwrap_or_else(|| "base game".into())), Kind::Plain, accent, r).clicked() { act = Some((row.code.clone(), true)); }
                    });
                    ui.separator();
                }
            });
        }, |ui, r| {
            ui.horizontal(|ui| { if ui_kit::button(ui, "Close", Kind::Plain, accent, r).clicked() { close = true; } if let Some((t, good)) = &status { ui.label(RichText::new(t).size(0.85 * r).color(if *good { GOOD } else { BAD })); } });
        });
        let mut out = SgUnmatchedOut { close: o.closed || close, added: vec![], changed: false };
        if let Some((code, credit)) = act {
            match self.run(&code, credit) {
                Ok(added) => { out.changed = true; if let Some(a) = added { out.added.push(a); } if credit { self.status = Some(("Credited the base game and added it to \"Won on SteamGifts\".".into(), true)); } self.reload(); }
                Err(e) => self.status = Some((e, false)),
            }
        }
        out
    }
}
