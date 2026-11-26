//! Data > Import from Playnite: choose a Playnite backup, see how its games matched this library, then pick what to bring in: dates and playtime (fill or overwrite),
//! completion statuses through a mapping you can change, and categories as groups. The last import can be undone. The logic is `pd_filter::playnite`.
use crate::filter_dialog::FilterDialog;
use crate::ui_kit::{self, Kind};
use pd_filter::playnite::{self as pn, Backup, Matched, Merged, Options};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};

pub enum PlayniteOut { None, Close, /// The library changed: the pages redraw.
    Changed, /// Games were added (appid, name, is Steam): the fill-in jobs run on them, and the pages redraw.
    Added(Vec<(i64, String, bool)>) }

struct Scan { backup: Backup, matched: Matched, merged: BTreeMap<i64, Merged>, plan: pn::NewPlan }
/// A Playnite status found on matched games: its name, how many, and what to give them.
struct StatusRow { name: String, count: usize, to: String }
struct CatRow { name: String, count: usize, on: bool, group: String }

pub struct PlayniteDialog {
    db: PathBuf, root: PathBuf,
    rx_file: Option<Receiver<Option<PathBuf>>>, rx_scan: Option<Receiver<Result<Scan, String>>>,
    scan: Option<Scan>, file: String,
    dates: [bool; 3], overwrite: bool, skip_bulk: bool, bulk_n: usize, add_missing: bool,
    statuses: Vec<StatusRow>, cats: Vec<CatRow>,
    msg: Option<(String, bool)>,
}

impl PlayniteDialog {
    pub fn new(db: PathBuf, root: PathBuf) -> Self {
        let mut d = PlayniteDialog { db, root, rx_file: None, rx_scan: None, scan: None, file: String::new(), dates: [true, true, true], overwrite: false, skip_bulk: true, bulk_n: 10, add_missing: false, statuses: vec![], cats: vec![], msg: None };
        if let Some(p) = std::env::var_os("ZEST_PLAYNITE_ZIP") { d.start(PathBuf::from(p)); }   // headless checks
        d
    }

    fn start(&mut self, path: PathBuf) {
        self.file = path.display().to_string(); self.scan = None; self.msg = Some(("Reading the backup...".into(), true));
        let (tx, rx) = channel(); self.rx_scan = Some(rx);
        let db = self.db.clone();
        std::thread::spawn(move || {
            let r = (|| -> Result<Scan, String> {
                let backup = pn::read_backup(&path)?;
                let conn = rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| e.to_string())?;
                let matched = pn::match_games(&backup.games, &conn)?;
                let merged = pn::merge(&backup, &matched);
                let plan = pn::plan_new(&backup, &matched, &conn);
                Ok(Scan { backup, matched, merged, plan })
            })();
            let _ = tx.send(r);
        });
    }
    fn loaded(&mut self, s: Scan) {
        // the statuses and categories that matched games actually carry
        let mut st: BTreeMap<String, usize> = BTreeMap::new(); let mut ct: BTreeMap<String, usize> = BTreeMap::new();
        for m in s.merged.values() { if let Some(n) = &m.status { *st.entry(n.clone()).or_default() += 1; } for c in &m.categories { *ct.entry(c.clone()).or_default() += 1; } }
        self.statuses = st.into_iter().map(|(name, count)| { let to = pn::default_status(&name).unwrap_or("").to_string(); StatusRow { name, count, to } }).collect();
        self.cats = ct.into_iter().map(|(name, count)| CatRow { group: name.clone(), name, count, on: false }).collect();
        self.msg = None; self.scan = Some(s);
    }

    pub fn show(&mut self, ctx: &egui::Context) -> PlayniteOut {
        let accent = ui_kit::accent();
        if let Some(p) = self.rx_file.as_ref().and_then(|rx| rx.try_recv().ok()) { self.rx_file = None; if let Some(p) = p { self.start(p); } }
        if let Some(r) = self.rx_scan.as_ref().and_then(|rx| rx.try_recv().ok()) { self.rx_scan = None; match r { Ok(s) => self.loaded(s), Err(e) => self.msg = Some((e, false)) } }
        if self.rx_file.is_some() || self.rx_scan.is_some() { ctx.request_repaint_after(std::time::Duration::from_millis(100)); }
        let (mut close, mut changed) = (false, false);
        let mut added_out: Vec<(i64, String, bool)> = vec![];
        let o = ui_kit::modal_full(ctx, "playnite", "Import from Playnite", 40.0, |_, _| {}, |ui, r| {
            ui.label(ui_kit::muted("Reads a Playnite backup (the zip from Playnite's Backup feature) and copies what you choose onto the games you already have. Games are only added if you tick the box that offers it, and the last import can be undone.", 0.82 * r));
            ui.add_space(0.3 * r);
            ui.horizontal(|ui| {
                if ui_kit::button(ui, "CHOOSE A BACKUP...", Kind::Plain, accent, r).clicked() && self.rx_file.is_none() && self.rx_scan.is_none() {
                    let (tx, rx) = channel(); self.rx_file = Some(rx);
                    std::thread::spawn(move || { let _ = tx.send(rfd::FileDialog::new().set_title("Choose a Playnite backup").add_filter("Zip", &["zip"]).pick_file()); });
                }
                if pn::has_undo(&self.root) && ui_kit::button(ui, "UNDO THE LAST IMPORT", Kind::Plain, accent, r).clicked() {
                    self.msg = Some(match rusqlite::Connection::open(&self.db).map_err(|e| e.to_string()).and_then(|mut c| { c.busy_timeout(std::time::Duration::from_secs(10)).ok(); pn::undo(&mut c, &self.root) }) {
                        Ok(n) => { changed = true; (format!("Undid the last import: {n} change(s) put back or removed."), true) } Err(e) => (e, false) });
                }
            });
            if !self.file.is_empty() { ui.label(ui_kit::muted(self.file.clone(), 0.78 * r)); }
            if let Some((m, ok)) = &self.msg { ui.label(egui::RichText::new(m).size(0.85 * r).color(if *ok { egui::Color32::from_rgb(0x8b, 0xb4, 0x2c) } else { crate::ui_kit::text_danger() })); }
            let Some(scan) = &self.scan else { return };
            let by_id = scan.matched.matches.iter().filter(|m| m.by_id).count(); let by_title = scan.matched.matches.len() - by_id;
            ui.add_space(0.5 * r);
            ui.label(egui::RichText::new("What matched").color(ui_kit::TEXT()).strong().size(r));
            ui.label(ui_kit::muted(format!("{} games in the backup: {by_id} matched by the store's own id, {by_title} by title, {} not in this library.", scan.backup.games.len(), scan.matched.unmatched.len()), 0.85 * r));
            if !scan.matched.unmatched.is_empty() && ui_kit::button(ui, "COPY THE NAMES THAT DID NOT MATCH", Kind::Plain, accent, r).clicked() {
                let text = scan.matched.unmatched.iter().map(|i| { let g = &scan.backup.games[*i]; g.name.clone().unwrap_or_else(|| g.game_id.clone()) }).collect::<Vec<_>>().join("\n");
                ui.ctx().copy_text(text);
            }
            if !scan.plan.add.is_empty() {
                let p = &scan.plan;
                ui.checkbox(&mut self.add_missing, egui::RichText::new(format!("Also add the {} Steam and GOG games that are not in this library", p.add.len())).size(0.88 * r));
                if ui_kit::button(ui, "COPY THE NAMES THAT WOULD BE ADDED", Kind::Plain, accent, r).clicked() {
                    let text = p.add.iter().map(|n| format!("{} ({})", n.name, n.platform)).collect::<Vec<_>>().join("\n");
                    ui.ctx().copy_text(text);
                }
                ui.label(ui_kit::muted(format!("Left out: {} that look like demos, tests or tools, {} already in your blacklist, {} from other stores. Added games are filled in afterwards; the import can be undone and then removes them.", p.junk, p.blacklisted, p.other_store), 0.78 * r));
            }
            ui.add_space(0.5 * r);
            ui.label(egui::RichText::new("Dates and playtime").color(ui_kit::TEXT()).strong().size(r));
            ui.horizontal(|ui| { for (i, l) in ["Date Added", "Last Played", "Playtime"].iter().enumerate() { ui.checkbox(&mut self.dates[i], egui::RichText::new(*l).size(0.88 * r)); } });
            ui.horizontal(|ui| {
                if ui_kit::pill(ui, "FILL BLANKS ONLY", !self.overwrite, accent, 0.8 * r).clicked() { self.overwrite = false; }
                if ui_kit::pill(ui, "OVERWRITE", self.overwrite, accent, 0.8 * r).clicked() { self.overwrite = true; }
            });
            ui.label(ui_kit::muted("Filling keeps what you have (for Date Added, the earlier of the two).", 0.78 * r));
            ui.checkbox(&mut self.skip_bulk, egui::RichText::new(format!("Ignore a Date Added shared by {} or more games (Playnite sets it when it imports a whole library at once)", self.bulk_n)).size(0.85 * r));
            if !self.statuses.is_empty() {
                ui.add_space(0.5 * r);
                ui.label(egui::RichText::new("Completion status").color(ui_kit::TEXT()).strong().size(r));
                ui.label(ui_kit::muted("What each of Playnite's statuses becomes in Zest. This only sets the status; every game is still imported. Choose \"Keep Zest's status\" to leave those games' status as it is. Only games still at Never Played are changed, so a status you set yourself is never replaced. Playnite's Completed becomes Beaten by default: here Completed means 100% of the achievements, which Zest sets itself.", 0.78 * r));
                let opts: Vec<(String, String)> = std::iter::once(("".to_string(), "Keep Zest's status".to_string())).chain(pn::PD_STATUSES.iter().map(|s| (s.to_string(), s.to_string()))).collect();
                for (i, row) in self.statuses.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.allocate_ui_with_layout(egui::vec2(14.0 * r, 1.4 * r), egui::Layout::left_to_right(egui::Align::Center), |ui| { ui.set_min_width(14.0 * r); ui.label(egui::RichText::new(format!("{} ({})", row.name, row.count)).size(0.88 * r).color(ui_kit::TEXT())); });
                        ui.label(ui_kit::muted("->", 0.85 * r));
                        let shown = if row.to.is_empty() { "Keep Zest's status".to_string() } else { row.to.clone() };
                        if let Some(v) = FilterDialog::combo(ui, egui::Id::new(("pn_status", i)), &shown, &opts, 11.0 * r, r) { row.to = v; }
                    });
                }
            }
            if !self.cats.is_empty() {
                ui.add_space(0.5 * r);
                ui.label(egui::RichText::new("Categories as groups").color(ui_kit::TEXT()).strong().size(r));
                ui.label(ui_kit::muted("Tick the Playnite categories to bring over; each becomes a group of that name (change the name if you like).", 0.78 * r));
                for (i, row) in self.cats.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut row.on, egui::RichText::new(format!("{} ({})", row.name, row.count)).size(0.88 * r));
                        if row.on { ui.add(egui::TextEdit::singleline(&mut row.group).id(egui::Id::new(("pn_group", i))).desired_width(14.0 * r).font(egui::FontId::proportional(0.88 * r))); }
                    });
                }
            }
            ui.add_space(0.6 * r);
            if ui_kit::button(ui, "IMPORT", Kind::Primary, accent, r).clicked() {
                let o = Options { date_added: self.dates[0], last_played: self.dates[1], playtime: self.dates[2], overwrite: self.overwrite, skip_bulk_dates: if self.skip_bulk { self.bulk_n } else { 0 },
                    status_map: self.statuses.iter().filter(|s| !s.to.is_empty()).map(|s| (s.name.trim().to_lowercase(), s.to.clone())).collect(),
                    groups: self.cats.iter().filter(|c| c.on && !c.group.trim().is_empty()).map(|c| (c.name.clone(), c.group.trim().to_string())).collect() };
                self.msg = Some(match rusqlite::Connection::open(&self.db).map_err(|e| e.to_string()).and_then(|mut c| { c.busy_timeout(std::time::Duration::from_secs(10)).ok(); let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0); pn::import(&mut c, &self.root, &scan.backup, &scan.matched, &o, self.add_missing, now) }) {
                    Ok((rep, added)) => { changed = true; added_out = added.iter().map(|a| (a.appid, a.name.clone(), a.platform == "steam")).collect(); (format!("Done: {}Date Added {}, Last Played {}, Playtime {}, status {}, grouped {}.{}", if rep.added > 0 { format!("added {} game(s), ", rep.added) } else { String::new() }, rep.date_added, rep.last_played, rep.playtime, rep.status, rep.grouped, if rep.skipped_bulk_dates > 0 { format!(" {} shared dates were ignored.", rep.skipped_bulk_dates) } else { String::new() }), true) }
                    Err(e) => (e, false) });
            }
        }, |_, _| {});
        if o.closed || close { close = true; }
        if close { PlayniteOut::Close } else if !added_out.is_empty() { PlayniteOut::Added(added_out) } else if changed { PlayniteOut::Changed } else { PlayniteOut::None }
    }
}
