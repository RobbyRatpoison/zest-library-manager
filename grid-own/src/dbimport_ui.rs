//! The "Import data from another database" section of the Data dialog (PlayDate's three steps in one place): choose a .db file, map its columns onto the library's, run.
use crate::filter_dialog::FilterDialog;
use crate::ui_kit::{self, Kind};
use pd_filter::dbimport;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};

pub struct DbImportUi {
    open: bool,
    rx: Option<Receiver<Option<PathBuf>>>,
    ext: Option<PathBuf>,
    info: Option<dbimport::Inspect>,
    table: usize, appid_col: String, maps: Vec<(String, String)>, normalize: bool,
    msg: Option<(String, bool)>,
}

impl DbImportUi {
    /// The date box starts ticked, as PlayDate's `normalize-dates` checkbox does (its import then writes Unix timestamps into date columns instead of text).
    pub fn new() -> DbImportUi { DbImportUi { open: false, rx: None, ext: None, info: None, table: 0, appid_col: String::new(), maps: vec![], normalize: true, msg: None } }

    fn load(&mut self, path: PathBuf, db: &Path) {
        let r = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| e.to_string()).and_then(|c| dbimport::inspect(&path, &c));
        match r {
            Ok(info) => {
                self.table = info.tables.iter().position(|t| t == "games").unwrap_or(0);
                self.appid_col = String::new(); self.maps = vec![]; self.msg = None;
                self.info = Some(info); self.ext = Some(path); self.pick_table(self.table);
            }
            Err(e) => { self.info = None; self.msg = Some((e, false)); }
        }
    }
    fn pick_table(&mut self, i: usize) {
        self.table = i;
        let Some(info) = &self.info else { return };
        let cols = &info.columns[i];
        self.appid_col = cols.iter().find(|(n, _)| n.eq_ignore_ascii_case("appid")).or(cols.first()).map(|c| c.0.clone()).unwrap_or_default();
        // start with the columns both sides share (apart from the appid), as the most likely wish
        self.maps = cols.iter().filter(|(n, _)| *n != self.appid_col && info.local.iter().any(|(l, _)| l == n && l != "appid")).map(|(n, _)| (n.clone(), n.clone())).take(8).collect();
    }

    /// Draws the section. Returns true when the library changed (the app then redraws).
    pub fn show(&mut self, ui: &mut egui::Ui, r: f32, accent: egui::Color32, db: &Path) -> bool {
        let mut changed = false;
        ui.label(egui::RichText::new("Import from another database").color(ui_kit::TEXT()).strong().size(r));
        ui.label(ui_kit::muted("Copies columns from another SQLite file (a PlayDate or Zest library, or your own) into this library. Games are matched by AppID: only games in both are changed.", 0.82 * r));
        ui.add_space(0.3 * r);
        if let Some(p) = self.rx.as_ref().and_then(|rx| rx.try_recv().ok()) { self.rx = None; if let Some(p) = p { self.load(p, db); } }
        if self.rx.is_some() { ui.ctx().request_repaint_after(std::time::Duration::from_millis(100)); }
        if !self.open {
            if ui_kit::button(ui, "CHOOSE A DATABASE FILE...", Kind::Plain, accent, r).clicked() {
                self.open = true; let (tx, rx) = channel(); self.rx = Some(rx);
                std::thread::spawn(move || { let _ = tx.send(rfd::FileDialog::new().set_title("Choose a database").add_filter("SQLite", &["db", "sqlite", "sqlite3"]).pick_file()); });
            }
        }
        if let Some((m, ok)) = &self.msg { ui.label(egui::RichText::new(m).size(0.85 * r).color(if *ok { egui::Color32::from_rgb(0x8b, 0xb4, 0x2c) } else { crate::ui_kit::text_danger() })); }
        let Some(info) = &self.info else { if self.rx.is_none() && self.open { self.open = false; } return false };
        ui.add_space(0.3 * r);
        ui.label(ui_kit::muted(format!("File: {}", self.ext.as_ref().map(|p| p.display().to_string()).unwrap_or_default()), 0.8 * r));
        let tables: Vec<(String, String)> = info.tables.iter().map(|t| (t.clone(), t.clone())).collect();
        let mut new_table = None;
        ui.horizontal(|ui| {
            ui.label(ui_kit::muted("Table", 0.85 * r));
            if let Some(t) = FilterDialog::combo(ui, egui::Id::new("dbi_table"), &info.tables[self.table], &tables, 12.0 * r, r) { new_table = info.tables.iter().position(|x| *x == t); }
        });
        let src_cols: Vec<(String, String)> = info.columns[self.table].iter().map(|(n, t)| (n.clone(), format!("{n} ({})", t.to_lowercase()))).collect();
        let label_of = |n: &str| src_cols.iter().find(|(v, _)| v == n).map(|(_, l)| l.clone()).unwrap_or_else(|| n.to_string());
        ui.horizontal(|ui| {
            ui.label(ui_kit::muted("AppID column (matches the games)", 0.85 * r));
            if let Some(c) = FilterDialog::combo(ui, egui::Id::new("dbi_appid"), &label_of(&self.appid_col), &src_cols, 14.0 * r, r) { self.appid_col = c; }
        });
        let tgt_cols: Vec<(String, String)> = info.local.iter().filter(|(n, _)| n != "appid").map(|(n, t)| (n.clone(), format!("{n} ({})", t.to_lowercase()))).collect();
        ui.add_space(0.2 * r);
        ui.label(ui_kit::muted("From the file  ->  into the library", 0.82 * r));
        let mut del = None;
        for (i, (s, t)) in self.maps.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                if let Some(c) = FilterDialog::combo(ui, egui::Id::new(("dbi_src", i)), &label_of(s), &src_cols, 13.0 * r, r) { *s = c; }
                ui.label(ui_kit::muted("->", 0.85 * r));
                let tl = tgt_cols.iter().find(|(v, _)| v == t).map(|(_, l)| l.clone()).unwrap_or_else(|| t.clone());
                if let Some(c) = FilterDialog::combo(ui, egui::Id::new(("dbi_tgt", i)), &tl, &tgt_cols, 13.0 * r, r) { *t = c; }
                if ui_kit::button(ui, "X", Kind::Plain, accent, r).clicked() { del = Some(i); }
            });
        }
        if let Some(i) = del { self.maps.remove(i); }
        ui.horizontal(|ui| {
            if ui_kit::button(ui, "+ ADD COLUMN", Kind::Plain, accent, r).clicked() { self.maps.push((src_cols.first().map(|c| c.0.clone()).unwrap_or_default(), tgt_cols.first().map(|c| c.0.clone()).unwrap_or_default())); }
        });
        // PlayDate's `refreshTypeWarnings`: a text column into a number column (or the other way round) is flagged as soon as it is chosen, not only after the run
        let mismatches: Vec<String> = self.maps.iter().filter_map(|(s, t)| {
            let st = info.columns[self.table].iter().find(|(n, _)| n == s).map(|x| x.1.as_str())?;
            let tt = info.local.iter().find(|(n, _)| n == t).map(|x| x.1.as_str())?;
            dbimport::type_warning(st, tt).map(|_| format!("{s} ({}) -> {t} ({}): values will be imported as-is and may not sort correctly", st.to_lowercase(), tt.to_lowercase()))
        }).collect();
        if !mismatches.is_empty() {
            ui.label(egui::RichText::new("\u{26A0} Type mismatches detected:").strong().size(0.85 * r).color(egui::Color32::from_rgb(0xe6, 0xa2, 0x3c)));
            for m in &mismatches { ui.label(egui::RichText::new(format!("\u{2022} {m}")).size(0.82 * r).color(egui::Color32::from_rgb(0xe6, 0xa2, 0x3c))); }
        }
        ui.checkbox(&mut self.normalize, egui::RichText::new("Convert dates to the library's date format").size(0.88 * r));
        let (mut run, mut reset) = (false, false);
        ui.horizontal(|ui| {
            if ui_kit::button(ui, "RUN IMPORT", Kind::Primary, accent, r).clicked() { run = true; }
            if ui_kit::button(ui, "START OVER", Kind::Plain, accent, r).clicked() { reset = true; }
        });
        if run {
            let tname = info.tables[self.table].clone(); let ext = self.ext.clone().unwrap();
            let res = rusqlite::Connection::open(db).map_err(|e| e.to_string()).and_then(|mut c| { c.busy_timeout(std::time::Duration::from_secs(10)).ok(); dbimport::execute(&ext, &mut c, &tname, &self.appid_col, &self.maps, self.normalize) });
            match res {
                Ok(o) => { changed = o.updated > 0; self.msg = Some((format!("Updated {} game(s); {} skipped (not in your library, or nothing to copy).{}", o.updated, o.skipped, if o.warnings.is_empty() { String::new() } else { format!(" Notes: {}", o.warnings.join(" ")) }), true)); self.info = None; self.ext = None; self.open = false; }
                Err(e) => self.msg = Some((e, false)),
            }
        }
        if reset { self.info = None; self.ext = None; self.open = false; self.msg = None; }
        if let Some(t) = new_table { self.pick_table(t); }
        changed
    }
}
