//! Menu -> Emulators: which emulator plays which system, where its program and your ROM folders are, and a scan that adds the ROMs found as games.
//! The rules live in `pd_filter::emulators` (a port of PlayDate's `emulators.py`, the same `emulators.json`); this is only the window.
use crate::ui_kit::{self, Kind};
use egui::RichText;
use pd_filter::emulators as emu;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};

/// What the app does after a frame: nothing, close, or add art/details jobs for the games a scan found.
pub enum EmuOut { None, Close, Added(Vec<(i64, String)>) }

enum Msg { Folder(usize, String, Option<PathBuf>), Program(usize, Option<PathBuf>), Scanned(Result<Vec<(i64, String, String)>, String>), Warnings(Vec<(String, Vec<(String, String, String)>)>), Cores(usize, String, Vec<(String, String)>) }

struct PlatRow { id: String, label: String, dirs: Vec<String>, core: String, found_cores: Option<Vec<(String, String)>> }
struct Row { id: String, name: String, custom: bool, enabled: bool, binary: String, args: String, args_orig: String, has_cores: bool, plats: Vec<PlatRow> }

#[derive(PartialEq)] enum Mode { List, Add }

pub struct EmulatorsDialog {
    root: PathBuf, db: PathBuf, rows: Vec<Row>, mode: Mode, status: String, busy: bool, progress: Arc<Mutex<String>>,
    warnings: BTreeMap<String, Vec<(String, String, String)>>,
    add_filter: String, custom_name: String, custom_system: String, custom_program: String,
    tx: mpsc::Sender<Msg>, rx: mpsc::Receiver<Msg>, ctx: egui::Context,
}

fn load_rows(root: &std::path::Path) -> Vec<Row> {
    emu::load(root).iter().map(|e| {
        let k = emu::known(emu::entry_id(e)); let a = emu::args(e).join(" ");
        Row { id: emu::entry_id(e).into(), name: emu::display_name(e), custom: e.get("custom").and_then(Value::as_bool).unwrap_or(false), enabled: emu::enabled(e), binary: emu::binary(e),
            args: a.clone(), args_orig: a, has_cores: k.map_or(false, |k| !k.cores_search.is_empty()),
            plats: emu::entry_platforms(e).into_iter().map(|p| PlatRow { label: emu::platform_label(&p), dirs: { let mut d = emu::platform_dirs(e, &p); if d.is_empty() { d.push(String::new()); } d }, core: emu::stored_core(e, &p), found_cores: None, id: p }).collect() }
    }).collect()
}

impl EmulatorsDialog {
    pub fn new(ctx: &egui::Context, root: PathBuf, db: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel();
        let d = EmulatorsDialog { rows: load_rows(&root), root, db, mode: Mode::List, status: String::new(), busy: false, progress: Arc::new(Mutex::new(String::new())), warnings: BTreeMap::new(),
            add_filter: String::new(), custom_name: String::new(), custom_system: "snes".into(), custom_program: String::new(), tx, rx, ctx: ctx.clone() };
        d.check_flatpak(); d
    }
    /// Flatpak access is asked of `flatpak info`, which can take a moment: off the UI thread.
    fn check_flatpak(&self) {
        let (root, tx, ctx) = (self.root.clone(), self.tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let w: Vec<_> = emu::load(&root).iter().map(|e| (emu::entry_id(e).to_string(), emu::flatpak_warnings(e))).filter(|(_, w)| !w.is_empty()).collect();
            let _ = tx.send(Msg::Warnings(w)); ctx.request_repaint();
        });
    }
    fn reload(&mut self) { self.rows = load_rows(&self.root); self.check_flatpak(); }
    /// Write one emulator's edited fields to `emulators.json`.
    fn save(&mut self, i: usize) {
        let r = &self.rows[i];
        let mut u = emu::Update { binary: Some(r.binary.trim().to_string()), enabled: Some(r.enabled), ..Default::default() };
        u.platform_dirs = Some(r.plats.iter().map(|p| (p.id.clone(), p.dirs.iter().map(|d| d.trim().to_string()).filter(|d| !d.is_empty()).collect())).collect());
        if r.args.trim() != r.args_orig.trim() { u.args = Some(r.args.split_whitespace().map(String::from).collect()); }
        if r.has_cores { u.cores = Some(r.plats.iter().map(|p| (p.id.clone(), p.core.trim().to_string())).collect()); }
        if let Err(e) = emu::update_emulator(&self.root, &r.id, &u) { self.status = format!("Could not save: {e}"); } else { self.rows[i].args_orig = self.rows[i].args.clone(); }
        self.check_flatpak();
    }
    fn scan(&mut self, only: Option<(String, String)>) {
        if self.busy { return; }
        self.busy = true; self.status = "Scanning...".into();
        let (root, db, tx, ctx, prog) = (self.root.clone(), self.db.clone(), self.tx.clone(), self.ctx.clone(), self.progress.clone());
        std::thread::spawn(move || {
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
            let r = rusqlite::Connection::open(&db).map_err(|e| e.to_string()).and_then(|c| {
                let _ = c.busy_timeout(std::time::Duration::from_secs(10));
                let (p2, c2) = (prog.clone(), ctx.clone());
                let report = move |s: &str| { if let Ok(mut p) = p2.lock() { *p = s.to_string(); } c2.request_repaint(); };
                emu::scan(&root, &c, only.as_ref().map(|(a, b)| (a.as_str(), b.as_str())), now, &report)
            });
            let _ = tx.send(Msg::Scanned(r)); ctx.request_repaint();
        });
    }
    fn browse(&self, msg: impl FnOnce(Option<PathBuf>) -> Msg + Send + 'static, folder: bool, title: &'static str) {
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let d = rfd::FileDialog::new().set_title(title).set_directory(pd_filter::installdir::home());
            let p = if folder { d.pick_folder() } else { d.pick_file() };
            let _ = tx.send(msg(p)); ctx.request_repaint();
        });
    }

    pub fn show(&mut self, ctx: &egui::Context, accent: egui::Color32) -> EmuOut {
        let mut out = EmuOut::None; let mut save: Option<usize> = None;
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Folder(i, plat, Some(p)) => { if let Some(pr) = self.rows.get_mut(i).and_then(|r| r.plats.iter_mut().find(|x| x.id == plat)) { let s = p.to_string_lossy().to_string(); if let Some(slot) = pr.dirs.iter_mut().find(|d| d.is_empty()) { *slot = s; } else { pr.dirs.push(s); } save = Some(i); } }
                Msg::Program(i, Some(p)) => { if let Some(r) = self.rows.get_mut(i) { r.binary = p.to_string_lossy().into(); save = Some(i); } }
                Msg::Folder(..) | Msg::Program(..) => {}
                Msg::Warnings(w) => self.warnings = w.into_iter().collect(),
                Msg::Cores(i, plat, found) => { if let Some(pr) = self.rows.get_mut(i).and_then(|r| r.plats.iter_mut().find(|x| x.id == plat)) { if pr.core.is_empty() { if let Some(f) = found.first() { pr.core = f.1.clone(); save = Some(i); } } pr.found_cores = Some(found); } }
                Msg::Scanned(r) => {
                    self.busy = false;
                    match r {
                        Ok(a) => {
                            let mut per: BTreeMap<String, usize> = BTreeMap::new(); for (_, _, p) in &a { *per.entry(emu::platform_label(p)).or_default() += 1; }
                            self.status = if a.is_empty() { "No new games found.".into() } else { format!("{} game{} added ({}). Cover art is being fetched.", a.len(), if a.len() == 1 { "" } else { "s" }, per.iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join(", ")) };
                            if !a.is_empty() { out = EmuOut::Added(a.into_iter().map(|(id, n, _)| (id, n)).collect()); }
                        }
                        Err(e) => self.status = e,
                    }
                }
            }
        }
        let progress = self.progress.lock().map(|p| p.clone()).unwrap_or_default();
        let (mut remove, mut scan_req, mut scan_all, mut to_add, mut custom_add, mut mode_switch) = (None::<String>, None::<(String, String)>, false, None::<String>, false, None::<Mode>);
        let mut cores_for: Option<(usize, String)> = None; let mut browse_prog: Option<usize> = None; let mut browse_dir: Option<(usize, String)> = None; let mut detect: Option<usize> = None;
        let res = ui_kit::modal(ctx, "emulators", "Emulators", 46.0, |ui, r| {
            ui_kit::style_fields(ui);
            ui.horizontal(|ui| {
                if ui_kit::pill(ui, "MY EMULATORS", self.mode == Mode::List, accent, 0.8 * r).clicked() { mode_switch = Some(Mode::List); }
                if ui_kit::pill(ui, "ADD AN EMULATOR", self.mode == Mode::Add, accent, 0.8 * r).clicked() { mode_switch = Some(Mode::Add); }
            });
            ui.add_space(0.3 * r);
            if self.mode == Mode::Add {
                ui.label(ui_kit::muted("Pick the emulator you use. Zest looks for its program and, for PlayStation 3 and Vita, for its game folder; you point it at your ROM folders afterwards.", 0.82 * r));
                ui.add(egui::TextEdit::singleline(&mut self.add_filter).hint_text("Filter by emulator or system").desired_width(24.0 * r));
                let f = self.add_filter.trim().to_lowercase();
                let have: Vec<&str> = self.rows.iter().map(|x| x.id.as_str()).collect();
                egui::ScrollArea::vertical().max_height(15.0 * r).id_salt("emu_known").auto_shrink([false, true]).show(ui, |ui| {
                    for k in emu::known_list() {
                        let systems: Vec<String> = k.platforms.iter().map(|p| emu::platform_label(p)).collect();
                        if !f.is_empty() && !k.name.to_lowercase().contains(&f) && !systems.iter().any(|s| s.to_lowercase().contains(&f)) { continue; }
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&k.name).color(ui_kit::TEXT()).size(0.9 * r));
                            ui.label(ui_kit::muted(systems.join(", "), 0.78 * r));
                            if have.contains(&k.id.as_str()) { ui.label(ui_kit::muted("added", 0.8 * r)); } else if ui_kit::button(ui, "ADD", Kind::Plain, accent, r).clicked() { to_add = Some(k.id.clone()); }
                        });
                    }
                });
                ui.separator();
                ui.label(RichText::new("Not listed? Add your own").color(ui_kit::TEXT()).size(0.9 * r));
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.custom_name).hint_text("Name").desired_width(10.0 * r));
                    ui_kit::style_dropdown(ui, r);
                    egui::ComboBox::from_id_salt("emu_custom_system").selected_text(RichText::new(emu::platform_label(&self.custom_system)).size(0.9 * r)).height(18.0 * r).show_ui(ui, |ui| {
                        let names = emu::platform_names();
                        let s = ui_kit::combo_search(ui, egui::Id::new("emu_custom_system"), names.len(), r);
                        let mut n = 0;
                        for (id, label) in names.iter().filter(|(_, l)| s.matches(l)) {
                            let hit = ui.selectable_label(self.custom_system == *id, label).clicked() || (s.enter && n == 0);
                            n += 1;
                            if hit { self.custom_system = id.clone(); if s.enter { ui.close(); } }
                        }
                        if n == 0 { ui.label(ui_kit::muted("Nothing matches", 0.85 * r)); }
                    });
                });
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.custom_program).hint_text("Program (optional now)").desired_width(20.0 * r));
                    if ui_kit::button(ui, "ADD CUSTOM", Kind::Primary, accent, r).clicked() { custom_add = true; }
                });
            } else if self.rows.is_empty() {
                ui.label(ui_kit::muted("No emulators yet. Use ADD AN EMULATOR; Zest then finds its program and you give it your ROM folders.", 0.88 * r));
            } else {
                egui::ScrollArea::vertical().max_height(f32::INFINITY).id_salt("emu_list").auto_shrink([false, true]).show(ui, |ui| {
                    for i in 0..self.rows.len() {
                        let labels: Vec<&str> = self.rows[i].plats.iter().map(|p| p.label.as_str()).collect();
                        let title = format!("{}   {}{}", self.rows[i].name, labels.iter().take(3).cloned().collect::<Vec<_>>().join(", "), if labels.len() > 3 { format!(" +{}", labels.len() - 3) } else { String::new() });
                        egui::CollapsingHeader::new(RichText::new(title).size(0.95 * r).color(ui_kit::TEXT())).id_salt(("emu", &self.rows[i].id)).default_open(self.rows.len() == 1).show(ui, |ui| {
                            let row = &mut self.rows[i];
                            if let Some(ws) = self.warnings.get(&row.id) { for (p, m, fix) in ws { ui.label(RichText::new(format!("{}: {m}", emu::platform_label(p))).size(0.82 * r).color(egui::Color32::from_rgb(0xe0, 0xa0, 0x30)));
                                ui.horizontal(|ui| { ui.label(ui_kit::muted(format!("Fix: {fix}"), 0.78 * r)); if ui_kit::button(ui, "COPY", Kind::Plain, accent, r).clicked() { ui.ctx().copy_text(fix.clone()); } }); } }
                            ui.horizontal(|ui| {
                                if ui.checkbox(&mut row.enabled, RichText::new("Use this emulator").size(0.85 * r).color(ui_kit::TEXT())).changed() { save = Some(i); }
                                if ui_kit::button(ui, "REMOVE", Kind::Danger, accent, r).clicked() { remove = Some(row.id.clone()); }
                            });
                            ui.horizontal(|ui| {
                                ui.label(ui_kit::muted("Program", 0.82 * r));
                                if ui.add(egui::TextEdit::singleline(&mut row.binary).desired_width(22.0 * r)).lost_focus() { save = Some(i); }
                                if ui_kit::button(ui, "BROWSE", Kind::Plain, accent, r).clicked() { browse_prog = Some(i); }
                                if !row.custom && ui_kit::button(ui, "DETECT", Kind::Plain, accent, r).clicked() { detect = Some(i); }
                            });
                            if !row.binary.is_empty() && !std::path::Path::new(&row.binary).is_file() && !row.binary.contains(std::path::MAIN_SEPARATOR) { ui.label(ui_kit::muted("Found on the search path when it runs.", 0.75 * r)); }
                            else if !row.binary.is_empty() && !std::path::Path::new(&row.binary).is_file() { ui.label(RichText::new("That file does not exist.").size(0.78 * r).color(egui::Color32::from_rgb(0xe0, 0x70, 0x70))); }
                            ui.horizontal(|ui| {
                                ui.label(ui_kit::muted("Arguments", 0.82 * r));
                                if ui.add(egui::TextEdit::singleline(&mut row.args).desired_width(22.0 * r)).lost_focus() { save = Some(i); }
                            });
                            ui.label(ui_kit::muted("{rom} is the game file; {rom_dir} and {rom_name} (no extension) and, for RetroArch, {core} are also filled in. Separate with spaces.", 0.72 * r));
                            for pr in row.plats.iter_mut() {
                                ui.add_space(0.2 * r);
                                ui.label(RichText::new(&pr.label).size(0.9 * r).color(ui_kit::TEXT()).strong());
                                let mut drop: Option<usize> = None;
                                let many = pr.dirs.len() > 1; let pid = pr.id.clone();
                                for (di, d) in pr.dirs.iter_mut().enumerate() {
                                    ui.horizontal(|ui| {
                                        if ui.add(egui::TextEdit::singleline(d).hint_text("ROM folder").desired_width(24.0 * r)).lost_focus() { save = Some(i); }
                                        if ui_kit::button(ui, "BROWSE", Kind::Plain, accent, r).clicked() { browse_dir = Some((i, pid.clone())); }
                                        if !d.trim().is_empty() && !std::path::Path::new(d.trim()).is_dir() { ui.label(RichText::new("not found").size(0.75 * r).color(egui::Color32::from_rgb(0xe0, 0x70, 0x70))); }
                                        if many && ui_kit::button(ui, "X", Kind::Plain, accent, r).clicked() { drop = Some(di); }
                                    });
                                }
                                if let Some(di) = drop { pr.dirs.remove(di); save = Some(i); }
                                ui.horizontal(|ui| {
                                    if ui_kit::button(ui, "ADD FOLDER", Kind::Plain, accent, r).clicked() { if !pr.dirs.iter().any(|d| d.is_empty()) { pr.dirs.push(String::new()); } }
                                    if ui_kit::button(ui, "SCAN THIS SYSTEM", Kind::Primary, accent, r).clicked() { scan_req = Some((row.id.clone(), pr.id.clone())); }
                                });
                                if row.has_cores {
                                    ui.horizontal(|ui| {
                                        ui.label(ui_kit::muted("Core", 0.82 * r));
                                        if ui.add(egui::TextEdit::singleline(&mut pr.core).hint_text("path to the libretro core").desired_width(22.0 * r)).lost_focus() { save = Some(i); }
                                        if ui_kit::button(ui, "FIND", Kind::Plain, accent, r).clicked() { cores_for = Some((i, pr.id.clone())); }
                                    });
                                    if let Some(f) = &pr.found_cores {
                                        if f.is_empty() { ui.label(ui_kit::muted("No matching core is installed.", 0.78 * r)); }
                                        for (n, p) in f { if ui.small_button(RichText::new(format!("use {n}")).size(0.8 * r)).clicked() { pr.core = p.clone(); save = Some(i); } }
                                    }
                                }
                            }
                        });
                        ui.separator();
                    }
                });
            }
            ui.add_space(0.3 * r);
            ui.horizontal(|ui| {
                if self.mode == Mode::List && !self.rows.is_empty() && ui_kit::button(ui, if self.busy { "SCANNING..." } else { "SCAN ALL" }, Kind::Primary, accent, r).clicked() { scan_all = true; }
                if self.busy && !progress.is_empty() { ui.label(ui_kit::muted(progress.clone(), 0.82 * r)); }
            });
            if !self.status.is_empty() { ui.label(ui_kit::muted(self.status.clone(), 0.82 * r)); }
        });
        if let Some(m) = mode_switch { self.mode = m; self.status.clear(); }
        if let Some(id) = to_add { match emu::add_emulator(&self.root, &id) { Ok(_) => { self.reload(); self.mode = Mode::List; self.status = "Added. Set its ROM folders, then scan.".into(); } Err(e) => self.status = e } }
        if custom_add {
            let args = vec![]; match emu::add_custom_emulator(&self.root, &self.custom_name, &self.custom_system, &self.custom_program, &args) {
                Ok(_) => { self.reload(); self.mode = Mode::List; self.custom_name.clear(); self.custom_program.clear(); self.status = "Added. Set its ROM folder, then scan.".into(); } Err(e) => self.status = e }
        }
        if let Some(id) = remove { let _ = emu::remove_emulator(&self.root, &id); self.reload(); self.status = "Removed. Games already added stay in your library.".into(); }
        if let Some(i) = detect { let id = self.rows[i].id.clone(); let p = emu::auto_detect_binary(&id); if p.is_empty() { self.status = "Could not find the program; use BROWSE.".into(); } else { self.rows[i].binary = p; save = Some(i); self.status = "Found it.".into(); } }
        if let Some(i) = browse_prog { self.browse(move |p| Msg::Program(i, p), false, "Emulator program"); }
        if let Some((i, p)) = browse_dir { self.browse(move |d| Msg::Folder(i, p, d), true, "ROM folder"); }
        if let Some((i, p)) = cores_for { let (tx, ctx2) = (self.tx.clone(), self.ctx.clone()); std::thread::spawn(move || { let f = emu::list_cores_for_platform(&p); let _ = tx.send(Msg::Cores(i, p, f)); ctx2.request_repaint(); }); }
        if let Some(i) = save { self.save(i); }
        if scan_all { self.scan(None); }
        if let Some(s) = scan_req { self.scan(Some(s)); }
        if res.closed { return EmuOut::Close; }
        out
    }
}
