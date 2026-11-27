//! Menu -> Data: back up the data folder to a zip, restore a backup, export the library as CSV. The long ones run on a thread; the dialog shows what they report.
use crate::ui_kit::{self, Kind};
use pd_filter::net::Cancel;
use rusqlite::types::Value as Sql;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};

enum Msg { Picked(Option<PathBuf>), Done(Result<String, String>, Kind2), /// A filter file was read: (name, tree), or why not (empty = the dialog was cancelled).
    Filter(Result<(String, serde_json::Value), String>) }
#[derive(Clone, Copy, PartialEq)] pub enum Kind2 { Backup, Restore, Csv, FilterFile }

pub enum DataOut { None, Close, /// A filter was imported: save it under this name (the app gives it an id).
    SaveFilter(String, serde_json::Value), /// A backup finished (its time goes into `state.json` for the update dialog's nag).
    BackedUp, /// A restore finished: the open library has been replaced, so the app must restart to use it.
    Restored, /// Columns were copied in from another database: the pages redraw.
    Imported, /// Open the Playnite import.
    OpenPlaynite }

pub struct DataDialog {
    include_art: bool, /// Backup: put the store sign-ins in it (off by default; they are written unencrypted).
    include_sign_ins: bool, busy: bool, status: String,
    /// Which CSV columns to export (`backup::CSV_HEADERS` order) and whether the list is open.
    csv_cols: [bool; 17], csv_open: bool, dbi: crate::dbimport_ui::DbImportUi,
    root: PathBuf, db: PathBuf, filter_query: (String, Vec<Sql>),
    tx: mpsc::Sender<Msg>, rx: mpsc::Receiver<Msg>, ctx: egui::Context, cancel: Cancel,
    confirm_restore: Option<PathBuf>, progress: Arc<Mutex<String>>,
    /// Filter import / export: the saved filters' names, the one chosen to export, and an imported filter waiting for a free name.
    filters: Vec<(String, serde_json::Value)>, f_pick: String, f_pending: Option<(String, serde_json::Value)>, f_new: String, f_msg: String,
}

fn stamp() -> String {
    let s = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let d = pd_filter::backup::ts_to_date(&Sql::Integer(s)).replace('-', "");
    let t = s.rem_euclid(86400); format!("{d}_{:02}{:02}{:02}", t / 3600, t % 3600 / 60, t % 60)
}

impl DataDialog {
    pub fn new(ctx: &egui::Context, root: PathBuf, db: PathBuf, filter_query: (String, Vec<Sql>), saved: &serde_json::Map<String, serde_json::Value>) -> Self {
        let (tx, rx) = mpsc::channel();
        DataDialog { dbi: crate::dbimport_ui::DbImportUi::new(), csv_cols: [true; 17], csv_open: false, include_art: true, include_sign_ins: false, busy: false, status: String::new(), root, db, filter_query, tx, rx, ctx: ctx.clone(), cancel: Cancel::new(), filters: saved.iter().map(|(n, e)| (n.clone(), e.get("tree").cloned().unwrap_or_else(|| e.clone()))).collect(), f_pick: String::new(), f_pending: None, f_new: String::new(), f_msg: String::new(), confirm_restore: None, progress: Arc::new(Mutex::new(String::new())) }
    }
    fn run(&mut self, kind: Kind2, work: impl FnOnce(&Cancel, &dyn Fn(String)) -> Result<String, String> + Send + 'static) {
        self.busy = true; self.cancel = Cancel::new(); self.status = String::new();
        let (tx, ctx, cancel) = (self.tx.clone(), self.ctx.clone(), self.cancel.clone());
        let prog = self.progress.clone();
        std::thread::spawn(move || {
            let report = |s: String| { if let Ok(mut p) = prog.lock() { *p = s; } ctx.request_repaint(); };
            let r = work(&cancel, &report);
            let _ = tx.send(Msg::Done(r, kind)); ctx.request_repaint();
        });
    }
    pub fn show(&mut self, ctx: &egui::Context, accent: egui::Color32) -> DataOut {
        let mut out = DataOut::None;
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Picked(p) => { self.busy = false; self.confirm_restore = p; }
                Msg::Filter(r) => {
                    self.busy = false;
                    match r {
                        Err(e) => self.f_msg = e,
                        Ok((name, tree)) => {
                            if self.filters.iter().any(|(n, _)| *n == name) { self.f_msg = format!("A filter named \"{name}\" already exists. Enter a new name:"); self.f_new.clear(); self.f_pending = Some((name, tree)); }
                            else { self.f_msg = format!("Imported \"{name}\"."); self.filters.push((name.clone(), tree.clone())); out = DataOut::SaveFilter(name, tree); }
                        }
                    }
                }
                Msg::Done(r, k) => {
                    self.busy = false; if let Ok(mut p) = self.progress.lock() { p.clear(); }
                    match r {
                        Ok(s) => { self.status = s; if k == Kind2::Backup && !self.status.is_empty() && !self.status.starts_with("Cancelled") { out = DataOut::BackedUp; } if k == Kind2::Restore { out = DataOut::Restored; } }
                        Err(e) => self.status = e,
                    }
                }
            }
        }
        let live = self.progress.lock().map(|p| p.clone()).unwrap_or_default();
        let res = ui_kit::modal(ctx, "data", "Data", 36.0, |ui, r| {
            ui.label(egui::RichText::new("Backup").color(ui_kit::TEXT()).strong().size(r));
            ui.label(ui_kit::muted("Saves your library, settings and (optionally) every cover into one zip. PlayDate can restore it too.", 0.82 * r));
            ui.add_space(0.3 * r);
            ui.checkbox(&mut self.include_art, egui::RichText::new("Include cover art (much bigger, restores without re-downloading)").size(0.85 * r).color(ui_kit::TEXT()));
            ui.checkbox(&mut self.include_sign_ins, egui::RichText::new("Include store sign-ins (saved unencrypted in the backup; API keys are always included)").size(0.85 * r).color(ui_kit::TEXT()));
            ui.add_space(0.3 * r);
            ui.horizontal(|ui| {
                if ui_kit::button(ui, "BACK UP...", Kind::Primary, accent, r).clicked() && !self.busy {
                    let (root, art, sign_ins) = (self.root.clone(), self.include_art, self.include_sign_ins);
                    self.run(Kind2::Backup, move |cancel, report| {
                        let Some(path) = rfd::FileDialog::new().set_title("Save backup").set_file_name(format!("zest_backup_{}.zip", stamp())).add_filter("Zip", &["zip"]).save_file() else { return Ok(String::new()) };
                        let n = pd_filter::backup::create(&root, &path, art, sign_ins, &mut |d, t| report(format!("Backing up: {d} of {t}")), cancel)?;
                        Ok(format!("Backed up {n} files to {}", path.display()))
                    });
                }
                if ui_kit::button(ui, "RESTORE...", Kind::Plain, accent, r).clicked() && !self.busy {
                    let (tx, ctx2) = (self.tx.clone(), self.ctx.clone());
                    self.busy = true;
                    std::thread::spawn(move || { let f = rfd::FileDialog::new().set_title("Choose a backup").add_filter("Zip", &["zip"]).pick_file(); let _ = tx.send(Msg::Picked(f)); ctx2.request_repaint(); });
                }
                if self.busy && ui_kit::button(ui, "CANCEL", Kind::Plain, accent, r).clicked() { self.cancel.cancel(); }
            });
            if let Some(p) = self.confirm_restore.clone() {
                ui.add_space(0.4 * r);
                ui.label(egui::RichText::new(format!("Replace your library with {}?", p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default())).color(ui_kit::TEXT()).size(0.9 * r));
                ui.label(ui_kit::muted("Your current library and settings are overwritten. Zest has to restart afterwards. Back up first if unsure.", 0.8 * r));
                ui.horizontal(|ui| {
                    if ui_kit::button(ui, "YES, RESTORE", Kind::Danger, accent, r).clicked() {
                        self.confirm_restore = None; let root = self.root.clone();
                        self.run(Kind2::Restore, move |cancel, report| {
                            let rs = pd_filter::backup::restore(&root, &p, &mut |d, t| report(format!("Restoring: {d} of {t}")), cancel)?;
                            Ok(format!("Restored {} item(s){}. Restart Zest to use the restored library.", rs.restored.len(), if rs.skipped.iter().any(|s| s.starts_with("games_")) { "; a database was refused (see the log)" } else { "" }))
                        });
                    }
                    if ui_kit::button(ui, "CANCEL", Kind::Plain, accent, r).clicked() { self.confirm_restore = None; }
                });
            }
            ui.add_space(0.6 * r);
            ui.label(egui::RichText::new("Folders").color(ui_kit::TEXT()).strong().size(r));
            ui.horizontal(|ui| {
                if ui_kit::button(ui, "OPEN DATA FOLDER", Kind::Plain, accent, r).clicked() { crate::open_url(&self.root.to_string_lossy()); }
                if ui_kit::button(ui, "OPEN PROGRAM FOLDER", Kind::Plain, accent, r).clicked() {
                    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.to_path_buf())) { crate::open_url(&dir.to_string_lossy()); }
                }
            });
            ui.add_space(0.6 * r);
            ui.label(egui::RichText::new("Import from Playnite").color(ui_kit::TEXT()).strong().size(r));
            ui.label(ui_kit::muted("Dates, playtime, statuses and categories from a Playnite backup.", 0.82 * r));
            if ui_kit::button(ui, "IMPORT FROM PLAYNITE...", Kind::Plain, accent, r).clicked() { out = DataOut::OpenPlaynite; }
            ui.add_space(0.6 * r);
            if self.dbi.show(ui, r, accent, &self.db) { out = DataOut::Imported; }
            ui.add_space(0.6 * r);
            ui.label(egui::RichText::new("Export").color(ui_kit::TEXT()).strong().size(r));
            ui.label(ui_kit::muted("The library as a CSV spreadsheet: name, status, tags, playtime, dates, reviews and achievements.", 0.82 * r));
            ui.add_space(0.3 * r);
            if ui_kit::button(ui, if self.csv_open { "COLUMNS: HIDE" } else { "COLUMNS..." }, Kind::Plain, accent, r).clicked() { self.csv_open = !self.csv_open; }
            if self.csv_open {
                ui.horizontal_wrapped(|ui| { for (i, h) in pd_filter::backup::CSV_HEADERS.iter().enumerate() { ui.checkbox(&mut self.csv_cols[i], egui::RichText::new(*h).size(0.85 * r)); } });
            }
            ui.add_space(0.3 * r);
            ui.horizontal(|ui| {
                for (label, filtered) in [("EXPORT ALL GAMES...", false), ("EXPORT WHAT THE LIBRARY SHOWS...", true)] {
                    if ui_kit::button(ui, label, Kind::Plain, accent, r).clicked() && !self.busy {
                        let (db, q, cols) = (self.db.clone(), self.filter_query.clone(), self.csv_cols);
                        self.run(Kind2::Csv, move |_, _| {
                            let Some(path) = rfd::FileDialog::new().set_title("Export CSV").set_file_name(format!("zest_library_{}.csv", stamp())).add_filter("CSV", &["csv"]).save_file() else { return Ok(String::new()) };
                            let c = rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| e.to_string())?;
                            let text = pd_filter::backup::csv_text_cols(&c, if filtered { Some((q.0.as_str(), q.1.as_slice())) } else { None }, &cols)?;
                            std::fs::write(&path, text).map_err(|e| format!("Could not write {}: {e}", path.display()))?;
                            Ok(format!("Exported to {}", path.display()))
                        });
                    }
                }
            });
            ui.add_space(0.6 * r);
            ui.label(egui::RichText::new("Filters").color(ui_kit::TEXT()).strong().size(r));
            ui.label(ui_kit::muted("Share a saved filter as a small file (PlayDate reads and writes the same format).", 0.82 * r));
            ui.add_space(0.3 * r);
            if self.filters.is_empty() { ui.label(ui_kit::muted("You have no saved filters to export.", 0.8 * r)); }
            else {
                egui::ScrollArea::vertical().id_salt("data_filters").max_height(5.0 * r).auto_shrink([false, true]).show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| { for (n, _) in &self.filters { if ui_kit::pill(ui, n, self.f_pick == *n, accent, 0.75 * r).clicked() { self.f_pick = n.clone(); } } });
                });
            }
            ui.horizontal(|ui| {
                if ui_kit::button(ui, "EXPORT SELECTED...", Kind::Plain, accent, r).clicked() && !self.busy {
                    match self.filters.iter().find(|(n, _)| *n == self.f_pick).cloned() {
                        None => self.f_msg = "Select a filter to export.".into(),
                        Some((name, tree)) => {
                            self.f_msg.clear();
                            self.run(Kind2::FilterFile, move |_, _| {
                                let Some(path) = rfd::FileDialog::new().set_title("Export filter").set_file_name(format!("playdate-filter-{}.json", name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect::<String>())).add_filter("JSON", &["json"]).save_file() else { return Ok(String::new()) };
                                std::fs::write(&path, filter_file_text(&name, &tree)).map_err(|e| format!("Could not write {}: {e}", path.display()))?;
                                Ok(format!("Exported \"{name}\" to {}", path.display()))
                            });
                        }
                    }
                }
                if ui_kit::button(ui, "IMPORT...", Kind::Plain, accent, r).clicked() && !self.busy {
                    let (tx, ctx2) = (self.tx.clone(), self.ctx.clone());
                    self.busy = true; self.f_msg.clear(); self.f_pending = None;
                    std::thread::spawn(move || {
                        let r = match rfd::FileDialog::new().set_title("Choose a filter file").add_filter("JSON", &["json"]).pick_file() {
                            None => Err(String::new()),
                            Some(p) => std::fs::read_to_string(&p).map_err(|e| format!("Could not read the file: {e}")).and_then(|t| parse_filter_file(&t)),
                        };
                        let _ = tx.send(Msg::Filter(r)); ctx2.request_repaint();
                    });
                }
            });
            if let Some((name, tree)) = self.f_pending.clone() {
                ui.label(ui_kit::muted(self.f_msg.clone(), 0.82 * r));
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.f_new).hint_text("New name...").desired_width(14.0 * r));
                    let new = self.f_new.trim().to_string();
                    if ui_kit::button(ui, "SAVE", Kind::Primary, accent, r).clicked() && !new.is_empty() {
                        if self.filters.iter().any(|(n, _)| *n == new) { self.f_msg = format!("\"{new}\" also already exists. Try another name:"); }
                        else { self.f_pending = None; self.f_msg = format!("Imported \"{new}\"."); self.filters.push((new.clone(), tree.clone())); out = DataOut::SaveFilter(new, tree); let _ = &name; }
                    }
                    if ui_kit::button(ui, "CANCEL", Kind::Plain, accent, r).clicked() { self.f_pending = None; self.f_msg = "Import cancelled.".into(); }
                });
            } else if !self.f_msg.is_empty() { ui.label(ui_kit::muted(self.f_msg.clone(), 0.82 * r)); }
            ui.add_space(0.5 * r);
            let line = if !live.is_empty() { live.clone() } else { self.status.clone() };
            if !line.is_empty() { ui.label(ui_kit::muted(line, 0.85 * r)); }
        });
        if res.closed && matches!(out, DataOut::None) && !self.busy { return DataOut::Close; }
        out
    }
}

/// PlayDate's filter file: `{"playdate_filter": {"name": ..., "tree": ...}}`.
fn filter_file_text(name: &str, tree: &serde_json::Value) -> String {
    serde_json::to_string_pretty(&serde_json::json!({ "playdate_filter": { "name": name, "tree": tree } })).unwrap_or_default()
}

/// Read a filter file; the error is what the dialog shows (an empty one when the user cancelled the picker).
fn parse_filter_file(text: &str) -> Result<(String, serde_json::Value), String> {
    let v: serde_json::Value = serde_json::from_str(text).map_err(|_| "Could not parse the file.".to_string())?;
    let pf = v.get("playdate_filter").ok_or_else(|| "Invalid filter file.".to_string())?;
    let name = pf.get("name").and_then(|n| n.as_str()).map(|s| s.trim().to_string()).unwrap_or_default();
    match pf.get("tree") { Some(t) if !name.is_empty() && !t.is_null() && t.as_object().map_or(true, |o| !o.is_empty()) => Ok((name, t.clone())), _ => Err("Invalid filter file.".into()) }
}

#[cfg(test)]
mod filter_file_tests {
    use super::*;
    #[test]
    fn a_filter_file_round_trips_and_bad_ones_are_refused() {
        let tree = serde_json::json!({"type": "group", "logic": "AND", "items": [{"type": "condition", "column": "installed", "operator": "=", "value": "1"}]});
        let text = filter_file_text("Backlog", &tree);
        assert!(text.contains("\"playdate_filter\""));
        assert_eq!(parse_filter_file(&text).unwrap(), ("Backlog".to_string(), tree));
        assert_eq!(parse_filter_file("{").unwrap_err(), "Could not parse the file.");
        assert_eq!(parse_filter_file("{\"playdate_filter\": {\"name\": \"\", \"tree\": {\"a\": 1}}}").unwrap_err(), "Invalid filter file.");
        assert_eq!(parse_filter_file("{\"playdate_filter\": {\"name\": \"x\"}}").unwrap_err(), "Invalid filter file.");
        assert_eq!(parse_filter_file("{\"other\": 1}").unwrap_err(), "Invalid filter file.");
    }
}
