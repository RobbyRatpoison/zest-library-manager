//! Blacklist -> Find Library Junk: games that are probably not real games (port of PlayDate's "Find Library Junk"). Two lists to review, nothing is changed until a button is pressed:
//! Steam games no longer on the account (delete them; there is nothing left for a sync to add back) and games whose title looks like a beta, demo, soundtrack or dev kit (blacklist
//! them so a sync never brings them back, or whitelist them to stop being asked). The dialog only collects the choice; the app deletes, blacklists or saves the whitelist.
use crate::ui_kit::{self, Kind};
use pd_filter::junk::{self, Selection};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{mpsc, Arc};

pub enum JunkOut {
    None, Close,
    /// Delete these games (no longer owned; not blacklisted).
    Delete(Vec<i64>),
    /// Delete and blacklist.
    Blacklist(Vec<i64>),
    /// Keep them and stop suggesting them.
    Whitelist(Vec<i64>),
}

enum Msg { Done(Result<(Selection, String), String>), Deep(Vec<Hit>) }
/// One game a store's own check flagged: (appid, name, platform, why).
type Hit = (i64, String, String, String);
#[derive(Clone, Copy, PartialEq)]
enum Arm { Delete, Blacklist, Deep }

pub struct JunkDialog {
    sel: Selection, scanning: bool, scanned: bool,
    /// How the Steam ownership check went (skipped without a key, failed, done).
    note: String,
    pub status: String,
    owned_on: HashSet<i64>, pat_on: HashSet<i64>,
    /// The Deep Plugin Scan: games each connected store's current import rules would no longer import, its checkboxes, and whether it is running / has run.
    deep: Vec<Hit>, deep_on: HashSet<i64>, deep_running: bool, deep_ran: bool,
    /// A destructive button was pressed once: the next press does it.
    arm: Option<Arm>,
    db: PathBuf, root: PathBuf, tx: mpsc::Sender<Msg>, rx: mpsc::Receiver<Msg>, ctx: egui::Context,
}

impl JunkDialog {
    pub fn new(ctx: &egui::Context, db: PathBuf, root: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel();
        let mut d = JunkDialog { sel: Selection::default(), scanning: false, scanned: false, note: String::new(), status: String::new(), owned_on: HashSet::new(), pat_on: HashSet::new(), deep: Vec::new(), deep_on: HashSet::new(), deep_running: false, deep_ran: false, arm: None, db, root, tx, rx, ctx: ctx.clone() };
        d.scan();
        d
    }
    fn scan(&mut self) {
        self.scanning = true; self.status = "Scanning your library...".into(); self.arm = None;
        let (tx, ctx, db, root) = (self.tx.clone(), self.ctx.clone(), self.db.clone(), self.root.clone());
        std::thread::spawn(move || {
            let r = (|| {
                let state: serde_json::Value = std::fs::read_to_string(root.join("state.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
                let (rows, black) = junk::load(&db)?;
                let creds = pd_filter::account::load_account(&root).map(|a| pd_filter::steamclient::Creds { api_key: a.api_key, steam_id: a.steam_id });
                let steam = pd_filter::steamclient::Steam::new(Arc::new(pd_filter::net::UreqHttp::new()), creds, 10.0);
                let owned = junk::owned_appids(&steam);
                let note = match &owned { Ok(Some(_)) => "Checked against your current Steam library.".to_string(), Ok(None) => "Not checked against Steam: no API key is set (Settings).".to_string(), Err(e) => format!("The Steam ownership check failed: {e}") };
                let sel = junk::select(&rows, &black, &junk::dismissed(&state), owned.as_ref().ok().and_then(|o| o.as_ref()));
                Ok::<_, String>((sel, note))
            })();
            let _ = tx.send(Msg::Done(r)); ctx.request_repaint();
        });
    }
    /// PlayDate's Deep Plugin Scan: ask each connected store whether its games would still pass today's import rules (slow: some stores are asked over the network).
    fn deep_scan(&mut self) {
        self.deep_running = true; self.status = "Asking each store about your games...".into(); self.arm = None;
        let (tx, ctx, db, root) = (self.tx.clone(), self.ctx.clone(), self.db.clone(), self.root.clone());
        std::thread::spawn(move || {
            let state: serde_json::Value = std::fs::read_to_string(root.join("state.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
            let skip = junk::dismissed(&state);
            let reg = pd_filter::sources::Registry::builtin_with(&root, &db);
            let mut hits: Vec<Hit> = Vec::new();
            for src in reg.all() {
                match src.scan_junk() {
                    Ok(v) => hits.extend(v.into_iter().filter(|h| !skip.contains(&h.appid)).map(|h| (h.appid, h.name, src.id().to_string(), h.reason))),
                    Err(e) => pd_filter::net::log(&format!("Deep scan: {}: {e}", src.label())),
                }
            }
            let _ = tx.send(Msg::Deep(hits)); ctx.request_repaint();
        });
    }
    /// The app did something with `ids`: they leave both lists and the line under the lists says what happened.
    pub fn done(&mut self, ids: &[i64], message: String) {
        self.sel.owned.retain(|(a, _)| !ids.contains(a)); self.sel.patterns.retain(|f| !ids.contains(&f.appid)); self.deep.retain(|h| !ids.contains(&h.0));
        for i in ids { self.owned_on.remove(i); self.pat_on.remove(i); self.deep_on.remove(i); }
        self.arm = None; self.status = message;
    }

    pub fn show(&mut self, ctx: &egui::Context, accent: egui::Color32) -> JunkOut {
        while let Ok(m) = self.rx.try_recv() {
            let r = match m {
                Msg::Deep(hits) => { self.deep_running = false; self.deep_ran = true; self.status = if hits.is_empty() { "The stores found nothing to flag.".into() } else { String::new() }; self.deep_on.clear(); self.deep = hits; continue; }
                Msg::Done(r) => r,
            };
            self.scanning = false; self.scanned = true;
            match r {
                Ok((sel, note)) => { self.status = if sel.owned.is_empty() && sel.patterns.is_empty() { "No candidates found.".into() } else { String::new() }; self.sel = sel; self.note = note; self.owned_on.clear(); self.pat_on.clear(); }
                Err(e) => self.status = format!("Could not scan: {e}"),
            }
        }
        let mut out = JunkOut::None;
        let res = ui_kit::modal(ctx, "junk", "Find Library Junk", 44.0, |ui, r| {
            ui.label(ui_kit::muted("Entries that probably are not real games. Nothing here is a sure thing, so review each list before removing anything.", 0.85 * r));
            ui.add_space(0.4 * r);
            ui.horizontal(|ui| {
                if ui_kit::button(ui, if self.scanning { "SCANNING..." } else { "SCAN AGAIN" }, Kind::Plain, accent, r).clicked() && !self.scanning { self.scan(); }
                if !self.status.is_empty() { ui.label(ui_kit::muted(self.status.clone(), 0.85 * r)); }
            });
            if !self.scanned { return; }
            ui.add_space(0.4 * r);
            let max_h = (ctx.content_rect().height() * 0.72 - 8.0 * r).max(10.0 * r);
            egui::ScrollArea::vertical().id_salt("junk_all").max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
                ui.label(egui::RichText::new("NO LONGER OWNED").strong().color(ui_kit::TEXT()).size(0.85 * r));
                ui.label(ui_kit::muted("Free-to-play games with 5 minutes or less of playtime are skipped (Steam hides lightly played free games from this check even when owned). They cannot be added back by a sync since they are off your account, so deleting is enough. Still own it? Whitelist it.", 0.8 * r));
                ui.label(ui_kit::muted(self.note.clone(), 0.8 * r));
                if self.sel.owned.is_empty() { ui.label(ui_kit::muted("No ownership issues found.", 0.85 * r)); }
                else {
                    let ids: Vec<i64> = self.sel.owned.iter().map(|(a, _)| *a).collect();
                    if select_all(ui, &ids, &mut self.owned_on) { self.arm = None; }
                    egui::ScrollArea::vertical().id_salt("junk_owned").max_height(7.0 * r).auto_shrink([false, true]).show(ui, |ui| {
                        for (a, n) in &self.sel.owned { let mut on = self.owned_on.contains(a); if ui.checkbox(&mut on, egui::RichText::new(n).size(0.88 * r)).changed() { toggle(&mut self.owned_on, *a, on); self.arm = None; } }
                    });
                    let picked: Vec<i64> = ids.iter().copied().filter(|a| self.owned_on.contains(a)).collect();
                    ui.horizontal(|ui| {
                        let armed = self.arm == Some(Arm::Delete) && !picked.is_empty();
                        let label = if armed { format!("CLICK AGAIN: DELETE {} GAMES", picked.len()) } else { "DELETE SELECTED".to_string() };
                        if ui_kit::button(ui, &label, Kind::Danger, accent, r).clicked() { if picked.is_empty() { self.status = "No games selected.".into(); } else if armed { out = JunkOut::Delete(picked.clone()); } else { self.arm = Some(Arm::Delete); } }
                        if ui_kit::button(ui, "WHITELIST SELECTED", Kind::Plain, accent, r).clicked() { if picked.is_empty() { self.status = "No games selected.".into(); } else { out = JunkOut::Whitelist(picked.clone()); } }
                        if armed && ui_kit::button(ui, "CANCEL", Kind::Plain, accent, r).clicked() { self.arm = None; }
                    });
                }
                ui.add_space(0.8 * r);
                ui.label(egui::RichText::new("TITLE-PATTERN MATCHES").strong().color(ui_kit::TEXT()).size(0.85 * r));
                ui.label(ui_kit::muted("Stores do not tag beta clients, demos, prototypes, soundtracks or dev kits differently from real games, so titles of every library are checked for common patterns. Blacklisted games are deleted and skipped on every later sync; whitelisted ones are only dismissed from this list.", 0.8 * r));
                if self.sel.patterns.is_empty() { ui.label(ui_kit::muted("No candidates found.", 0.85 * r)); }
                else {
                    let ids: Vec<i64> = self.sel.patterns.iter().map(|f| f.appid).collect();
                    if select_all(ui, &ids, &mut self.pat_on) { self.arm = None; }
                    egui::ScrollArea::vertical().id_salt("junk_patterns").max_height(9.0 * r).auto_shrink([false, true]).show(ui, |ui| {
                        for f in &self.sel.patterns {
                            let mut on = self.pat_on.contains(&f.appid);
                            ui.horizontal(|ui| {
                                if ui.checkbox(&mut on, egui::RichText::new(&f.name).size(0.88 * r)).changed() { toggle(&mut self.pat_on, f.appid, on); self.arm = None; }
                                ui.label(ui_kit::muted(format!("{}  ·  {}", crate::home::platform_label(&f.platform), f.reasons.join(", ")), 0.78 * r));
                            });
                        }
                    });
                    let picked: Vec<i64> = ids.iter().copied().filter(|a| self.pat_on.contains(a)).collect();
                    ui.horizontal(|ui| {
                        let armed = self.arm == Some(Arm::Blacklist) && !picked.is_empty();
                        let label = if armed { format!("CLICK AGAIN: BLACKLIST {} GAMES", picked.len()) } else { "BLACKLIST SELECTED".to_string() };
                        if ui_kit::button(ui, &label, Kind::Danger, accent, r).clicked() { if picked.is_empty() { self.status = "No games selected.".into(); } else if armed { out = JunkOut::Blacklist(picked.clone()); } else { self.arm = Some(Arm::Blacklist); } }
                        if ui_kit::button(ui, "WHITELIST SELECTED", Kind::Plain, accent, r).clicked() { if picked.is_empty() { self.status = "No games selected.".into(); } else { out = JunkOut::Whitelist(picked.clone()); } }
                        if armed && ui_kit::button(ui, "CANCEL", Kind::Plain, accent, r).clicked() { self.arm = None; }
                    });
                }
                ui.add_space(0.8 * r);
                ui.label(egui::RichText::new("DEEP PLUGIN SCAN").strong().color(ui_kit::TEXT()).size(0.85 * r));
                ui.label(ui_kit::muted("Asks each connected store (Epic, Humble, PlayStation) whether a game you never touched would still be imported today, and lists the ones that are now DLC, extras, applications or demos. Slow: it contacts the stores. Games with playtime, installed ones and ones you set a status on are never listed.", 0.8 * r));
                ui.horizontal(|ui| {
                    if ui_kit::button(ui, if self.deep_running { "ASKING THE STORES..." } else if self.deep_ran { "SCAN AGAIN" } else { "RUN DEEP SCAN" }, Kind::Plain, accent, r).clicked() && !self.deep_running { self.deep_scan(); }
                });
                if self.deep_ran && self.deep.is_empty() { ui.label(ui_kit::muted("No candidates found.", 0.85 * r)); }
                if !self.deep.is_empty() {
                    let ids: Vec<i64> = self.deep.iter().map(|h| h.0).collect();
                    if select_all(ui, &ids, &mut self.deep_on) { self.arm = None; }
                    egui::ScrollArea::vertical().id_salt("junk_deep").max_height(9.0 * r).auto_shrink([false, true]).show(ui, |ui| {
                        for (a, name, plat, why) in &self.deep {
                            let mut on = self.deep_on.contains(a);
                            ui.horizontal(|ui| {
                                if ui.checkbox(&mut on, egui::RichText::new(name).size(0.88 * r)).changed() { toggle(&mut self.deep_on, *a, on); self.arm = None; }
                                ui.label(ui_kit::muted(format!("{}  ·  {}", crate::home::platform_label(plat), why), 0.78 * r));
                            });
                        }
                    });
                    let picked: Vec<i64> = ids.iter().copied().filter(|a| self.deep_on.contains(a)).collect();
                    ui.horizontal(|ui| {
                        let armed = self.arm == Some(Arm::Deep) && !picked.is_empty();
                        let label = if armed { format!("CLICK AGAIN: BLACKLIST {} GAMES", picked.len()) } else { "BLACKLIST SELECTED".to_string() };
                        if ui_kit::button(ui, &label, Kind::Danger, accent, r).clicked() { if picked.is_empty() { self.status = "No games selected.".into(); } else if armed { out = JunkOut::Blacklist(picked.clone()); } else { self.arm = Some(Arm::Deep); } }
                        if ui_kit::button(ui, "WHITELIST SELECTED", Kind::Plain, accent, r).clicked() { if picked.is_empty() { self.status = "No games selected.".into(); } else { out = JunkOut::Whitelist(picked.clone()); } }
                        if armed && ui_kit::button(ui, "CANCEL", Kind::Plain, accent, r).clicked() { self.arm = None; }
                    });
                }
            });
        });
        if res.closed && matches!(out, JunkOut::None) { return JunkOut::Close; }
        out
    }
}

fn toggle(set: &mut HashSet<i64>, id: i64, on: bool) { if on { set.insert(id); } else { set.remove(&id); } }

/// A "Select all" box that is on when every row is.
fn select_all(ui: &mut egui::Ui, ids: &[i64], on: &mut HashSet<i64>) -> bool {
    let mut all = !ids.is_empty() && ids.iter().all(|i| on.contains(i));
    let changed = ui.checkbox(&mut all, "Select all").changed();
    if changed { for i in ids { toggle(on, *i, all); } }
    changed
}
