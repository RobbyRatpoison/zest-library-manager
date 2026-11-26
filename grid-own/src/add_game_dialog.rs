//! Menu -> Add Game: a Steam game found by name, or a game by name only (Manual). The dialog only collects the choice; the app adds the row and queues the fill-in jobs.
use crate::ui_kit::{self, Kind};
use pd_filter::addgame::Hit;
use std::path::PathBuf;
use std::sync::mpsc;

pub enum AddOut { None, Close, Steam(i64, String), /// A manual game: its name, and the program and arguments to run (either may be empty).
    Custom(String, String, String) }

enum Msg { Hits(String, Result<Vec<Hit>, String>), Apps(Vec<pd_filter::installed_apps::App>), File(Option<String>) }

pub struct AddGameDialog {
    exe: String, args: String, apps: Option<Vec<pd_filter::installed_apps::App>>, app_q: String, scanning: bool, picking: bool,
    steam: bool, text: String, hits: Vec<Hit>, searched: Option<String>, busy: bool,
    /// A line under the field: what went wrong or what was found.
    pub status: String,
    db: PathBuf, tx: mpsc::Sender<Msg>, rx: mpsc::Receiver<Msg>, ctx: egui::Context,
}

impl AddGameDialog {
    pub fn new(ctx: &egui::Context, db: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel();
        AddGameDialog { exe: String::new(), args: String::new(), apps: None, app_q: String::new(), scanning: false, picking: false, steam: std::env::var_os("ZEST_ADD_MANUAL").is_none(), text: String::new(), hits: vec![], searched: None, busy: false, status: String::new(), db, tx, rx, ctx: ctx.clone() }
    }
    fn search(&mut self) {
        let term = self.text.trim().to_string();
        if term.chars().count() < 2 { self.status = "Type at least two letters.".into(); return; }
        self.busy = true; self.status = "Searching Steam...".into();
        let (tx, ctx, db) = (self.tx.clone(), self.ctx.clone(), self.db.clone());
        std::thread::spawn(move || {
            let existing: std::collections::HashSet<i64> = rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()
                .and_then(|c| c.prepare("SELECT appid FROM games").ok().map(|mut s| s.query_map([], |r| r.get::<_, i64>(0)).map(|r| r.flatten().collect()).unwrap_or_default())).unwrap_or_default();
            let r = pd_filter::addgame::steam_search(&pd_filter::net::UreqHttp::new(), &term, &existing);
            let _ = tx.send(Msg::Hits(term, r)); ctx.request_repaint();
        });
    }
    pub fn show(&mut self, ctx: &egui::Context, accent: egui::Color32) -> AddOut {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Hits(term, r) => { self.busy = false; match r { Ok(h) => { self.status = if h.is_empty() { "Nothing found.".into() } else { String::new() }; self.hits = h; self.searched = Some(term); } Err(e) => self.status = e } }
                Msg::Apps(v) => { self.scanning = false; self.apps = Some(v); }
                Msg::File(p) => { self.picking = false; if let Some(p) = p { self.exe = p; self.args.clear(); } }
            }
        }
        // the installed programs are looked up once, the first time the manual tab is shown
        if !self.steam && self.apps.is_none() && !self.scanning {
            self.scanning = true;
            let (tx, ctx2) = (self.tx.clone(), self.ctx.clone());
            std::thread::spawn(move || { let _ = tx.send(Msg::Apps(pd_filter::installed_apps::scan())); ctx2.request_repaint(); });
        }
        let mut out = AddOut::None;
        let res = ui_kit::modal(ctx, "add_game", "Add Game", 38.0, |ui, r| {
            ui.horizontal(|ui| {
                if ui_kit::pill(ui, "STEAM GAME", self.steam, accent, 0.8 * r).clicked() { self.steam = true; self.status.clear(); }
                if ui_kit::pill(ui, "MANUAL GAME", !self.steam, accent, 0.8 * r).clicked() { self.steam = false; self.status.clear(); }
            });
            ui.add_space(0.4 * r);
            ui.label(ui_kit::muted(if self.steam { "Search Steam by name, or paste a Steam AppID or store link, and pick the game. It is added like any Steam game and its details are fetched." } else { "A game with no store behind it: add it by name; Zest looks for its details and cover." }, 0.82 * r));
            ui.add_space(0.3 * r);
            let mut enter = false; let mut enter_add = false; let mut add_now = false;
            ui.horizontal(|ui| {
                let hint = if self.steam { "Game name, AppID or store link" } else { "Name" };
                let resp = ui.add(egui::TextEdit::singleline(&mut self.text).hint_text(hint).desired_width(22.0 * r));
                enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if self.steam { if ui_kit::button(ui, "SEARCH", Kind::Primary, accent, r).clicked() || enter { self.search(); } }
                else { enter_add = enter; }
            });
            if !self.steam {
                ui.add_space(0.4 * r);
                ui.label(ui_kit::muted("Program to run (optional): choose it, or pick one of the installed programs below.", 0.8 * r));
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.exe).hint_text("Program path").desired_width(22.0 * r));
                    if ui_kit::button(ui, "BROWSE...", Kind::Plain, accent, r).clicked() && !self.picking {
                        self.picking = true; let (tx, ctx2) = (self.tx.clone(), self.ctx.clone());
                        std::thread::spawn(move || { let _ = tx.send(Msg::File(rfd::FileDialog::new().set_title("Choose the program to run").pick_file().map(|p| p.to_string_lossy().to_string()))); ctx2.request_repaint(); });
                    }
                });
                if !self.args.is_empty() || !self.exe.is_empty() { ui.add(egui::TextEdit::singleline(&mut self.args).hint_text("Arguments (optional)").desired_width(f32::INFINITY)); }
                ui.add_space(0.3 * r);
                ui.label(egui::RichText::new("Installed programs").color(ui_kit::TEXT()).size(0.9 * r).strong());
                ui.add(egui::TextEdit::singleline(&mut self.app_q).hint_text("Search installed programs...").desired_width(f32::INFINITY));
                egui::ScrollArea::vertical().id_salt("apps").max_height(13.0 * r).auto_shrink([false, true]).show(ui, |ui| {
                    match &self.apps {
                        None => { ui.label(ui_kit::muted("Looking for installed programs...", 0.82 * r)); }
                        Some(apps) => {
                            let found = pd_filter::installed_apps::filter(apps, &self.app_q);
                            if found.is_empty() { ui.label(ui_kit::muted(if apps.is_empty() { "No installed programs were found; use BROWSE... instead." } else { "Nothing matches." }, 0.82 * r)); }
                            let mut pick = None;
                            for a in found.iter().take(200) {
                                ui.horizontal(|ui| {
                                    if ui_kit::button(ui, "USE", Kind::Plain, accent, r).clicked() { pick = Some((*a).clone()); }
                                    ui.label(egui::RichText::new(&a.name).color(ui_kit::TEXT()).size(0.88 * r));
                                    let full = if a.args.is_empty() { a.program.clone() } else { format!("{} {}", a.program, a.args) };
                                    let shown: String = if full.chars().count() > 60 { format!("{}...", full.chars().take(57).collect::<String>()) } else { full.clone() };
                                    ui.label(ui_kit::muted(shown, 0.72 * r)).on_hover_text(full);
                                });
                            }
                            if let Some(a) = pick { self.exe = a.program; self.args = a.args; if self.text.trim().is_empty() { self.text = a.name; } }
                        }
                    }
                });
                ui.add_space(0.4 * r);
                if ui_kit::button(ui, "ADD GAME", Kind::Primary, accent, r).clicked() { add_now = true; }
            }
            if add_now || enter_add {
                let n = self.text.trim().to_string();
                if n.is_empty() { self.status = "Enter a name for the game.".into(); } else { out = AddOut::Custom(n, self.exe.trim().to_string(), self.args.trim().to_string()); }
            }
            if !self.status.is_empty() { ui.add_space(0.2 * r); ui.label(ui_kit::muted(self.status.clone(), 0.82 * r)); }
            if self.steam {
                ui.add_space(0.3 * r);
                egui::ScrollArea::vertical().max_height(18.0 * r).auto_shrink([false, true]).show(ui, |ui| {
                    for h in &self.hits {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(&h.name).color(ui_kit::TEXT()).size(0.9 * r));
                            ui.label(ui_kit::muted(format!("#{}", h.appid), 0.75 * r));
                            if h.in_library { ui.label(ui_kit::muted("in your library", 0.8 * r)); }
                            else if ui_kit::button(ui, "ADD", Kind::Plain, accent, r).clicked() { out = AddOut::Steam(h.appid, h.name.clone()); }
                        });
                        ui.separator();
                    }
                });
            }
        });
        if res.closed && matches!(out, AddOut::None) { return AddOut::Close; }
        out
    }
}
