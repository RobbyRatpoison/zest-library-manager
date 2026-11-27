//! The Plugins dialog: the third-party plugins found in the data folder's `plugins/` folder, each with its sign-in and a "sync library" button. Talking to a
//! plugin takes time (the process may have to start), so every call runs on a thread and the dialog polls its answers.
use crate::ui_kit::{self, Kind};
use pd_filter::plugin::adapter::PluginSource;
use pd_filter::sources::{Caps, LibrarySource};
use std::sync::{mpsc, Arc};

enum Msg { Folder(usize, Option<std::path::PathBuf>), Status(usize, Result<(bool, Option<String>), String>), Begin(usize, Result<(String, Vec<String>, String), String>), Done(usize, Result<(bool, Option<String>), String>), /// The optional second sign-in (Battle.net's account): connected and who, or why not.
    Account(usize, Result<(bool, Option<String>), String>) }

#[derive(Default)]
struct Row { /// The card is expanded (collapsed shows the name and a one-line summary).
    open: bool, /// The launcher folder shown for a launcher-based store (looked up at most every two seconds), and when.
    folder: Option<(Option<String>, std::time::Instant)>, /// Is the launcher installed (looked up at most every two seconds), and when.
    launcher: Option<(bool, std::time::Instant)>, /// The next UNINSTALL press is the confirming one.
    confirm_un: bool, /// Xbox: the three-choice question after SYNC WITH NEXUS is showing.
    nexus_ask: bool, /// macOS: asked whether to download Wine before this store's launcher is installed.
    wine_ask: bool, dir_edit: Option<String>, connected: Option<bool>, user: Option<String>, busy: bool, error: Option<String>, pasting: Option<(Vec<String>, String)>, value: String, account: Option<(bool, Option<String>)>, account_busy: bool, account_error: Option<String> }

/// One source in the list: a built-in (no plugin process) or a plugin (its manifest and whether it is running).
pub struct Entry { pub src: Arc<dyn LibrarySource>, pub plugin: Option<Arc<PluginSource>> }

pub struct PluginsDialog {
    plugins: Vec<Entry>,
    errors: Vec<(String, String)>,
    legacy: Vec<pd_filter::plugin::manifest::Legacy>,
    folder: String,
    rows: Vec<Row>,
    /// The order the cards are listed in: by name, whatever order the sources were registered in.
    order: Vec<usize>,
    tx: mpsc::Sender<Msg>, rx: mpsc::Receiver<Msg>,
    ctx: egui::Context,
    pending_url: Option<String>,
    /// The app's count of browser results seen; when it moves, every source's sign-in status is asked again.
    seen_gen: u64,
    /// What Windows games will run with, found once when the dialog opens.
    runner: String,
    /// Stores that sync when Zest starts (`state.json`), set by the app after opening and after each change.
    pub startup: Vec<String>,
    /// Which build of a game to download, per store (`windows`, `linux`, `mac`, `auto`, `ask`), as saved.
    /// The Xbox games waiting for approval to be deleted, re-read from disk every second or so.
    pub xbox_cache: Option<(std::time::Instant, Vec<(String, i64)>)>,
}

pub enum PluginsOut { None, Close, OpenUrl(String), Sync(String), OpenFolder, Companion(String), SetGamesDir(String, String), /// Install this store's launcher into a Wine prefix.
    InstallLauncher(String), /// Start the installed launcher.
    StartLauncher(String), /// Remove the installed launcher (its prefix folder).
    UninstallLauncher(String), /// Xbox: ask Xodus Nexus which games the account owns.
    /// Sync this store's library when Zest starts: on or off.
    StartupSync(String, bool), /// Xbox: run the Nexus import in this mode (group, add, replace), chosen just now.
    XboxMode(String), /// Xbox: delete the games Nexus does not report, as listed.
    XboxApprove, /// Xbox: keep them (deletes nothing).
    XboxKeep }


impl PluginsDialog {
    pub fn open(ctx: &egui::Context, plugins: Vec<Entry>, errors: Vec<(String, String)>, legacy: Vec<pd_filter::plugin::manifest::Legacy>, folder: String) -> Self {
        let (tx, rx) = mpsc::channel();
        let rows = plugins.iter().map(|_| Row::default()).collect();
        let mut order: Vec<usize> = (0..plugins.len()).collect();
        order.sort_by_key(|&i| plugins[i].src.label().to_lowercase());
        let d = PluginsDialog { plugins, errors, legacy, folder, rows, order, tx, rx, ctx: ctx.clone(), xbox_cache: None, pending_url: None, seen_gen: 0, startup: Vec::new(), runner: pd_filter::wine::describe(&pd_filter::installdir::home(), pd_filter::wine::SYSTEM_COMPAT_DIRS, pd_filter::wine::WINE_DIRS, &std::env::var_os("PATH").unwrap_or_default()) };
        d   // nothing is asked yet: a row looks up its sign-in when it is opened
    }
    fn spawn(&self, f: impl FnOnce(&dyn LibrarySource, &mpsc::Sender<Msg>) + Send + 'static, i: usize) {
        let (p, tx, ctx) = (self.plugins[i].src.clone(), self.tx.clone(), self.ctx.clone());
        std::thread::spawn(move || { f(p.as_ref(), &tx); ctx.request_repaint(); });
    }
    /// A row was opened: look up whether it is signed in (some stores need a web request for that, so it is not done for every row when the dialog opens).
    fn expanded(&mut self, i: usize) {
        if self.plugins[i].src.caps().auth && !self.rows[i].busy { self.ask_status(i); }
        if self.plugins[i].src.account_login().is_some() { self.ask_account(i); }
    }
    fn ask_status(&self, i: usize) { self.spawn(move |p, tx| { let _ = tx.send(Msg::Status(i, p.auth_status().map(|v| (v.connected, v.username)))); }, i); }
    fn ask_account(&self, i: usize) { self.spawn(move |p, tx| { if let Some(a) = p.account_status() { let _ = tx.send(Msg::Account(i, Ok((a.connected, a.username)))); } }, i); }
    fn pump(&mut self) {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Folder(i, p) => { if let Some(p) = p { self.rows[i].dir_edit = Some(p.to_string_lossy().into_owned()); } }
                Msg::Status(i, r) | Msg::Done(i, r) => { let row = &mut self.rows[i]; row.busy = false; match r { Ok((c, u)) => { row.connected = Some(c); row.user = u; row.error = None; if c { row.pasting = None; row.value.clear(); } } Err(e) => row.error = Some(e) } }
                Msg::Account(i, r) => { let row = &mut self.rows[i]; row.account_busy = false; match r { Ok(v) => { row.account = Some(v); row.account_error = None; } Err(e) => row.account_error = Some(e) } }
                Msg::Begin(i, r) => { let row = &mut self.rows[i]; row.busy = false; match r { Ok((url, ins, ph)) => { row.pasting = Some((ins, ph)); row.error = None; if !url.is_empty() { self.pending_url = Some(url); } } Err(e) => row.error = Some(e) } }
            }
        }
    }
    pub fn show(&mut self, ctx: &egui::Context, statuses: &std::collections::HashMap<String, pd_filter::status::SyncStatus>, working: Option<(String, usize, usize, Option<String>)>, gen: u64) -> PluginsOut {
        if gen != self.seen_gen { self.seen_gen = gen; for i in 0..self.plugins.len() { if self.plugins[i].src.caps().auth && (self.rows[i].open || self.rows[i].connected.is_some()) { self.rows[i].busy = false; self.ask_status(i); } } }
        self.pump();
        let root0: std::path::PathBuf = std::path::Path::new(&self.folder).parent().map(|p| p.to_path_buf()).unwrap_or_default();   // the data folder (the plugins folder sits in it)
        // a worker finishing wakes the window from another thread; that wake-up was sometimes lost (the result showed only after the mouse moved), so while anything is working the dialog looks again by itself
        if self.rows.iter().any(|r| r.busy || r.account_busy) { ctx.request_repaint_after(std::time::Duration::from_millis(150)); }
        let _r = ui_kit::rem(ctx);
        let accent = ui_kit::accent();
        let mut out = PluginsOut::None;
        if let Some(u) = self.pending_url.take() { out = PluginsOut::OpenUrl(u); }
        let res = ui_kit::modal(ctx, "plugins", "Plugins and libraries", 46.0, |ui, r| {
            ui.label(ui_kit::muted("Library sources: the stores built into Zest, and plugins that add more. A plugin is a folder with a plugin.json; put it in the folder below and restart Zest.", 0.85 * r));
            ui.add_space(0.3 * r);
            if let Some((label, total, done, wait)) = &working { ui.label(egui::RichText::new(format!("Working: {label}  {done} / {total}{}", wait.as_ref().map(|w| format!("  \u{b7}  {w}")).unwrap_or_default())).size(0.85 * r).color(accent)); ui.add_space(0.4 * r); }
            { let id = egui::Id::new("psn_busy_note");
              if working.is_none() { ui.ctx().data_mut(|d| d.remove::<String>(id)); }
              else if let Some(n) = ui.ctx().data(|d| d.get_temp::<String>(id)) { ui.label(egui::RichText::new(n).size(0.85 * r).color(egui::Color32::from_rgb(230, 170, 60))); ui.add_space(0.4 * r); } }
            let max_h = (ui.ctx().content_rect().height() - 16.0 * r).max(12.0 * r);
            egui::ScrollArea::vertical().max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
                // the folder button scrolls with the list: above it, the pad's Up from the first visible plugin went to the button and skipped the plugins scrolled out of view
                if ui_kit::button(ui, "OPEN PLUGIN FOLDER", Kind::Plain, accent, r).clicked() { out = PluginsOut::OpenFolder; }
                ui.label(ui_kit::muted(self.folder.clone(), 0.7 * r));
                if !cfg!(windows) { ui.label(ui_kit::muted(self.runner.clone(), 0.75 * r)); }
                ui.add_space(0.6 * r);
                if self.plugins.is_empty() && self.errors.is_empty() { ui.label(ui_kit::muted("No library sources yet.", 0.9 * r)); }
                for i in self.order.clone() {
                    let p = self.plugins[i].src.clone(); let plugin = self.plugins[i].plugin.clone(); let caps: Caps = p.caps();
                    egui::Frame::new().fill(crate::ui_kit::INPUT()).stroke(egui::Stroke::new(1.0_f32, ui_kit::EDGE())).corner_radius(4).inner_margin(egui::Margin::same((0.8 * r) as i8)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        // a card opens and closes (a chevron or its name); one waiting for a pasted value stays open
                        let open = self.rows[i].open || self.rows[i].pasting.is_some();
                        ui.horizontal(|ui| {
                            if ui_kit::chevron(ui, open, r).clicked() { self.rows[i].open = !open; if open { self.rows[i].pasting = None; } else { self.expanded(i); } }
                            let name = ui.add(egui::Label::new(egui::RichText::new(p.label()).size(1.0 * r).strong().color(crate::ui_kit::heading())).sense(egui::Sense::click()));
                            if name.clicked() { self.rows[i].open = !open; if open { self.rows[i].pasting = None; } else { self.expanded(i); } }
                            if name.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
                            let kind = match &plugin { Some(pl) => format!("plugin v{}  \u{b7}  {}", pl.manifest().version, if pl.running() { "running" } else { "idle" }), None => "built in".to_string() };
                            ui.label(ui_kit::muted(kind, 0.75 * r));
                        });
                        if !open {
                            // collapsed: whether it is signed in and when it last synced, in one line
                            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
                            let row = &self.rows[i];
                            let mut bits: Vec<String> = vec![];
                            if caps.auth { bits.push(match (&row.connected, &row.user) { (Some(true), Some(u)) => format!("Signed in as {u}"), (Some(true), None) => "Signed in".into(), (Some(false), _) => "Not signed in".into(), (None, _) => "Open to check".into() }); }
                            if caps.sync { bits.push(match statuses.get(p.id()) { Some(st) => pd_filter::status::describe_short(st, now), None => "Not synced yet".into() }); }
                            if !bits.is_empty() { ui.label(ui_kit::muted(bits.join("  \u{b7}  "), 0.78 * r)); }
                            return;
                        }
                        let mut can: Vec<&str> = vec![];
                        for (on, n) in [(caps.auth, "sign in"), (caps.sync, "sync"), (caps.rescrape, "re-scrape"), (caps.description, "descriptions"), (caps.art, "art"), (caps.launch, "launch"), (caps.uninstall, "uninstall"), (caps.installed_scan, "install detection")] { if on { can.push(n); } }
                        ui.label(ui_kit::muted(format!("Can: {}", if can.is_empty() { "nothing yet".to_string() } else { can.join(", ") }), 0.72 * r));
                        if caps.sync {
                            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
                            let syncing_now = working.as_ref().map(|(l, ..)| l.starts_with("Syncing") && l.contains(p.label())).unwrap_or(false);
                            match statuses.get(p.id()) {
                                _ if syncing_now => { ui.label(egui::RichText::new("Syncing now...").size(0.8 * r).color(accent)); }
                                Some(st) => { ui.label(egui::RichText::new(pd_filter::status::describe(st, now)).size(0.8 * r).color(if st.error.is_some() { crate::ui_kit::warning() } else { egui::Color32::from_rgb(0x8b, 0xb4, 0x2c) })); }
                                None => { ui.label(ui_kit::muted("Not synced yet", 0.8 * r)); }
                            }
                        }
                        let row = &mut self.rows[i];
                        if caps.auth {
                            let status = match (&row.connected, &row.user) { (Some(true), Some(u)) => format!("Signed in as {u}"), (Some(true), None) => "Signed in".into(), (Some(false), _) => "Not signed in".into(), (None, _) => "Checking...".into() };
                            ui.label(ui_kit::muted(status, 0.8 * r));
                        }
                        if let Some(e) = &row.error { ui.label(egui::RichText::new(e).size(0.8 * r).color(crate::ui_kit::text_danger())); }
                        if let Some((ins, ph)) = row.pasting.clone() {
                            for (n, line) in ins.iter().enumerate() { ui.label(ui_kit::muted(format!("{}. {line}", n + 1), 0.8 * r)); }
                            ui.horizontal(|ui| {
                                ui_kit::style_fields(ui);
                                ui.add(egui::TextEdit::singleline(&mut row.value).hint_text(ph).desired_width(20.0 * r).font(egui::FontId::proportional(0.9 * r)));
                                if ui_kit::button(ui, "CONNECT", Kind::Primary, accent, r).clicked() && !row.busy && !row.value.trim().is_empty() {
                                    row.busy = true; let v = row.value.trim().to_string();
                                    let (p2, tx, ctx2) = (p.clone(), self.tx.clone(), self.ctx.clone());
                                    std::thread::spawn(move || { let r = p2.auth_complete(&v); pd_filter::net::log(&format!("sign-in {} (pasted value): {}", p2.id(), match &r { Ok(a) => format!("connected = {}", a.connected), Err(e) => format!("failed: {e}") })); let _ = tx.send(Msg::Done(i, r.map(|v| (v.connected, v.username)))); ctx2.request_repaint(); });
                                }
                            });
                        }
                        ui.horizontal(|ui| {
                            if caps.auth {
                                if row.connected == Some(true) {
                                    if p.sign_in_hint().is_none() && ui_kit::button(ui, "DISCONNECT", Kind::Plain, accent, r).clicked() && !row.busy {   // a store signed in inside its launcher has nothing to disconnect here
                                        row.busy = true; let (p2, tx, ctx2) = (p.clone(), self.tx.clone(), self.ctx.clone());
                                        std::thread::spawn(move || { let _ = tx.send(Msg::Done(i, p2.auth_disconnect().map(|_| (false, None)))); ctx2.request_repaint(); });
                                    }
                                } else {
                                    // a store whose own login page works in an embedded window gets the window first (the password is typed on the store's page); the paste flow stays beside it
                                    let web = p.web_login_done().filter(|_| pd_filter::weblogin::helper_path().is_some());
                                    if let Some(hint) = p.sign_in_hint() {
                                        // done inside the store's own launcher: shown, but not pressable
                                        ui.add_enabled_ui(false, |ui| ui_kit::button(ui, "SIGN IN", Kind::Plain, accent, r)).inner.on_disabled_hover_text(hint);
                                    } else if ui_kit::button(ui, "SIGN IN", Kind::Plain, accent, r).clicked() && !row.busy {
                                        row.busy = true; let (p2, tx, ctx2) = (p.clone(), self.tx.clone(), self.ctx.clone());
                                        if let Some(done) = web.clone() {
                                            let label = p.label().to_string();
                                            std::thread::spawn(move || {
                                                let r = p2.auth_begin().and_then(|b| {
                                                    let h = pd_filter::weblogin::helper_path().ok_or("The sign-in window is not installed.".to_string())?;
                                                    let v = pd_filter::weblogin::run_as(&h, &format!("Sign in to {label}"), p2.web_login_url().unwrap_or_else(|| b.url.clone()).as_str(), &done, 600, p2.web_login_agent())?;
                                                    p2.auth_complete(&v)
                                                });
                                                pd_filter::net::log(&format!("sign-in {}: {}", p2.id(), match &r { Ok(v) => format!("connected = {}", v.connected), Err(e) => format!("failed: {e}") }));
                                                let _ = tx.send(Msg::Done(i, r.map(|v| (v.connected, v.username)))); ctx2.request_repaint();
                                            });
                                        } else {
                                            std::thread::spawn(move || {
                                                let b = p2.auth_begin().map(|v| (v.url, v.instructions, v.placeholder));
                                                let _ = tx.send(Msg::Begin(i, b)); ctx2.request_repaint();
                                            });
                                        }
                                    }
                                    if web.is_some() && p.paste_login_kept() && ui_kit::button(ui, "SIGN IN IN BROWSER", Kind::Plain, accent, r).clicked() && !row.busy {
                                        row.busy = true; let (p2, tx, ctx2) = (p.clone(), self.tx.clone(), self.ctx.clone());
                                        std::thread::spawn(move || {
                                            let b = p2.auth_begin().map(|v| (v.url, v.instructions, v.placeholder));
                                            let _ = tx.send(Msg::Begin(i, b)); ctx2.request_repaint();
                                        });
                                    }
                                }
                            }
                            if caps.sync && ui_kit::button_h(ui, "SYNC LIBRARY", Kind::Primary, accent, r, 3.1).clicked() {   // the same height as SIGN IN and DISCONNECT beside it
                                // one job at a time: say so here, in the dialog, instead of starting nothing silently
                                if let Some((label, ..)) = &working { ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("psn_busy_note"), format!("{label} is still running. Press Sync Library again when it has finished."))); }
                                else { out = PluginsOut::Sync(p.id().to_string()); }
                            }
                            if cfg!(any(target_os = "linux", target_os = "macos", windows)) && pd_filter::launcher_installer::installer_for(p.id()).is_some() {
                                // the launcher's buttons: INSTALL when it is not there; START and UNINSTALL when it is
                                let stale = row.launcher.map_or(true, |(_, t)| t.elapsed() >= std::time::Duration::from_secs(2));
                                if stale { row.launcher = Some((pd_filter::launcher_installer::launcher_exe(&root0, p.id()).is_some(), std::time::Instant::now())); }
                                let installed = row.launcher.map_or(false, |(v, _)| v);
                                let blocked = |ui: &egui::Ui, what: &str| if let Some((label, ..)) = &working { ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("psn_busy_note"), format!("{label} is still running. {what} when it has finished."))); true } else { false };
                                if !installed {
                                    if row.wine_ask {
                                        // macOS without Wine: say what will be downloaded and ask first
                                        ui.label(egui::RichText::new(format!("{} needs Wine to run on a Mac. Zest can download it into its own folder (about {} MB, nothing else is installed).", p.label(), pd_filter::macwine::WINE_MB)).size(0.82 * r).color(ui_kit::TEXT()));
                                        if ui_kit::button(ui, "DOWNLOAD WINE AND INSTALL", Kind::Plain, accent, r).clicked() && !blocked(ui, "Install the launcher") { row.wine_ask = false; out = PluginsOut::InstallLauncher(p.id().to_string()); }
                                        if ui_kit::button(ui, "CANCEL", Kind::Plain, accent, r).clicked() { row.wine_ask = false; }
                                    } else if ui_kit::button(ui, "INSTALL LAUNCHER", Kind::Plain, accent, r).clicked() && !blocked(ui, "Install the launcher") {
                                        if cfg!(target_os = "macos") && pd_filter::wine::resolve_system().is_err() { row.wine_ask = true; } else { out = PluginsOut::InstallLauncher(p.id().to_string()); }
                                    }
                                } else {
                                    if ui_kit::button(ui, "START LAUNCHER", Kind::Plain, accent, r).clicked() { out = PluginsOut::StartLauncher(p.id().to_string()); }
                                    if row.confirm_un {
                                        if ui_kit::button(ui, if cfg!(windows) { "YES, OPEN ITS UNINSTALLER" } else { "YES, UNINSTALL (DELETES ITS GAMES TOO)" }, Kind::Plain, accent, r).clicked() && !blocked(ui, "Uninstall the launcher") { row.confirm_un = false; row.launcher = None; out = PluginsOut::UninstallLauncher(p.id().to_string()); }
                                        if ui_kit::button(ui, "CANCEL", Kind::Plain, accent, r).clicked() { row.confirm_un = false; }
                                    } else if ui_kit::button(ui, "UNINSTALL LAUNCHER", Kind::Plain, accent, r).clicked() { row.confirm_un = true; }
                                }
                            }
                            if cfg!(target_os = "linux") && p.id() == "xbox" && pd_filter::xbox_owned::backend_binary(&pd_filter::installdir::home()).is_some() && ui_kit::button(ui, "SYNC WITH NEXUS", Kind::Plain, accent, r).on_hover_text("Ask Xodus Nexus which games your Microsoft account owns or has through Game Pass, then choose what Zest does with that list").clicked() {
                                if let Some((label, ..)) = &working { ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("psn_busy_note"), format!("{label} is still running. Import when it has finished."))); }
                                else { row.nexus_ask = true; }
                            }
                            // Xbox: without Nexus the sync lists only the games you played; with it, the games you own
                            if cfg!(target_os = "linux") && p.id() == "xbox" && pd_filter::xbox_owned::backend_binary(&pd_filter::installdir::home()).is_none()
                                && ui_kit::button(ui, "GET NEXUS", Kind::Plain, accent, r).on_hover_text("Optional: Nexus Launcher (Xodus Nexus) lets Zest list the games your account owns or has through Game Pass, not only the ones you played. Install it, open it once, sign in and let it finish setting up, then sync again.").clicked() {
                                out = PluginsOut::OpenUrl("https://github.com/domsvoboda/nexus-launcher".to_string());
                            }
                            if row.busy { ui.label(ui_kit::muted("working...", 0.75 * r)); }
                        });
                        // a store that reads its games from a launcher's own files: which folder (Wine prefix) that is, to change or forget
                        // Mac: the launcher runs through Wine, and on the one Mac tried (an Intel MacBook, macOS 12) none worked: Ubisoft Connect never drew its window, EA's
                        // installer needs a Windows service that does not start, Battle.net and Legacy Games failed. Newer Macs may do better, so it is said, not blocked
                        if cfg!(target_os = "macos") && pd_filter::launcher_installer::installer_for(p.id()).is_some() {
                            ui.label(ui_kit::muted("On a Mac this launcher runs through Wine and will likely not work: on the Mac tested so far, launchers either failed to install or never opened their window. Newer Macs may do better.", 0.78 * r));
                        }
                        if cfg!(target_os = "linux") && p.sign_in_hint().is_some() {
                            if row.folder.as_ref().map_or(true, |(_, t)| t.elapsed() >= std::time::Duration::from_secs(2)) {
                                row.folder = Some((p.launcher_folder().map(|f| f.to_string_lossy().into_owned()), std::time::Instant::now()));
                            }
                            let cur: Option<String> = row.folder.as_ref().and_then(|(f, _)| f.clone());
                            ui.label(ui_kit::muted(format!("Launcher folder: {}", cur.clone().unwrap_or_else(|| "not set".to_string())), 0.78 * r));
                            ui.horizontal(|ui| {
                                if ui_kit::button(ui, "BROWSE", Kind::Plain, accent, r).on_hover_text("Choose the folder (Wine prefix) the launcher is installed in: the one with drive_c inside").clicked() && !row.busy {
                                    row.busy = true; let (p2, tx, ctx2) = (p.clone(), self.tx.clone(), self.ctx.clone());
                                    let start = cur.clone().map(std::path::PathBuf::from).unwrap_or_else(pd_filter::installdir::home);
                                    std::thread::spawn(move || {
                                        let r = match rfd::FileDialog::new().set_title("Launcher folder (the Wine prefix)").set_directory(start).pick_folder() {
                                            Some(dir) => p2.auth_complete(&dir.to_string_lossy()).map(|v| (v.connected, v.username)),
                                            None => p2.auth_status().map(|v| (v.connected, v.username)),   // cancelled: nothing changes
                                        };
                                        let _ = tx.send(Msg::Done(i, r)); ctx2.request_repaint();
                                    });
                                    row.folder = None;
                                }
                                if cur.is_some() && ui_kit::button(ui, "FORGET", Kind::Plain, accent, r).on_hover_text("Stop using this folder (the folder itself is not touched)").clicked() && !row.busy {
                                    row.busy = true; let (p2, tx, ctx2) = (p.clone(), self.tx.clone(), self.ctx.clone());
                                    std::thread::spawn(move || { let _ = tx.send(Msg::Done(i, p2.auth_disconnect().map(|_| (false, None)))); ctx2.request_repaint(); });
                                    row.folder = None;
                                }
                            });
                        }
                        if let Some(al) = p.account_login() {
                            let state = match &row.account { Some((true, u)) => format!("{}: signed in{}", al.label, u.as_ref().map(|u| format!(" as {u}")).unwrap_or_default()), Some((false, _)) => format!("{}: not signed in (optional: adds games you own but have not installed, and purchase dates)", al.label), None => format!("{}: checking...", al.label) };
                            ui.label(ui_kit::muted(state, 0.8 * r));
                            if let Some(e) = &row.account_error { ui.label(egui::RichText::new(e).size(0.8 * r).color(crate::ui_kit::text_danger())); }
                            ui.horizontal(|ui| {
                                if matches!(row.account, Some((true, _))) {
                                    if ui_kit::button(ui, "DISCONNECT ACCOUNT", Kind::Plain, accent, r).clicked() && !row.account_busy {
                                        row.account_busy = true; let (p2, tx, ctx2) = (p.clone(), self.tx.clone(), self.ctx.clone());
                                        std::thread::spawn(move || { let _ = tx.send(Msg::Account(i, p2.account_disconnect().map(|_| (false, None)))); ctx2.request_repaint(); });
                                    }
                                } else if ui_kit::button(ui, "SIGN IN TO ACCOUNT", Kind::Plain, accent, r).clicked() && !row.account_busy {
                                    match pd_filter::weblogin::helper_path() {
                                        None => row.account_error = Some("This needs the sign-in window, which this build does not include.".into()),
                                        Some(h) => {
                                            row.account_busy = true; row.account_error = None;
                                            let (p2, tx, ctx2, al2) = (p.clone(), self.tx.clone(), self.ctx.clone(), al.clone());
                                            std::thread::spawn(move || {
                                                let r = pd_filter::weblogin::run(&h, &format!("Sign in to {}", al2.label), &al2.url, &al2.done, 600).and_then(|v| p2.account_complete(&v)).map(|name| (true, name));
                                                let _ = tx.send(Msg::Account(i, r)); ctx2.request_repaint();
                                            });
                                        }
                                    }
                                }
                                if row.account_busy { ui.label(ui_kit::muted("working...", 0.75 * r)); }
                            });
                        }
                        if p.id() == "xbox" && cfg!(target_os = "linux") && pd_filter::xbox_owned::backend_binary(&pd_filter::installdir::home()).is_some() {
                            if row.nexus_ask {
                                // asked every time: what Zest does with the list of games Nexus says the account owns
                                ui.label(egui::RichText::new("What should Zest do with Nexus's list of the games you own?").size(0.82 * r).color(ui_kit::TEXT()));
                                ui.horizontal_wrapped(|ui| {
                                    for (key, text, tip) in [("group", "GROUP ONLY", "Sets the Owned / Game Pass / Played, not owned groups. Adds and removes nothing."),
                                                              ("add", "ADD MISSING", "Also adds the games you own that your play history never listed."),
                                                              ("replace", "ADD AND REMOVE", "Also deletes the Xbox games Nexus does not report. You see the list and confirm before anything is deleted.")] {
                                        if ui_kit::button(ui, text, Kind::Plain, accent, r).on_hover_text(tip).clicked() { row.nexus_ask = false; out = PluginsOut::XboxMode(key.to_string()); self.xbox_cache = None; }
                                    }
                                    if ui_kit::button(ui, "CANCEL", Kind::Plain, accent, r).clicked() { row.nexus_ask = false; }
                                });
                            }
                            if self.xbox_cache.as_ref().map_or(true, |(t, _)| t.elapsed() >= std::time::Duration::from_secs(1)) {
                                self.xbox_cache = Some((std::time::Instant::now(), pd_filter::xbox_owned::read_pending(&root0)));
                            }
                            let pend = self.xbox_cache.as_ref().map(|(_, p)| p.clone()).unwrap_or_default();
                            if !pend.is_empty() {
                                ui.label(egui::RichText::new(format!("{} Xbox games are not on Nexus's list and would be deleted:", pend.len())).size(0.82 * r).color(ui_kit::TEXT()));
                                let mut names: Vec<String> = pend.iter().take(12).map(|(n, m)| if *m > 0 { format!("{n} ({} h played)", (*m as f32 / 60.0 * 10.0).round() / 10.0) } else { n.clone() }).collect();
                                if pend.len() > 12 { names.push(format!("and {} more", pend.len() - 12)); }
                                ui.label(ui_kit::muted(names.join(", "), 0.78 * r));
                                ui.horizontal(|ui| {
                                    if ui_kit::button(ui, &format!("DELETE THESE {} GAMES", pend.len()), Kind::Primary, accent, r).on_hover_text("Deletes them and their covers. This asks again the next time.").clicked() { out = PluginsOut::XboxApprove; self.xbox_cache = None; }
                                    if ui_kit::button(ui, "KEEP THEM", Kind::Plain, accent, r).on_hover_text("Deletes nothing.").clicked() { out = PluginsOut::XboxKeep; self.xbox_cache = None; }
                                });
                            }
                        }
                        if caps.sync && caps.auth {
                            let mut on = self.startup.iter().any(|s| s == p.id());
                            if ui.checkbox(&mut on, egui::RichText::new("Sync this library when Zest starts").size(0.82 * r).color(ui_kit::TEXT())).changed() { out = PluginsOut::StartupSync(p.id().to_string(), on); }
                        }
                        if let Some(dir) = p.games_dir() {
                            ui.horizontal(|ui| {
                                ui.label(ui_kit::muted("Games folder", 0.75 * r));
                                let buf = row.dir_edit.get_or_insert_with(|| dir.to_string_lossy().into_owned());
                                let edited = buf.trim() != dir.to_string_lossy();
                                crate::filter_dialog::FilterDialog::input(ui, buf, 24.0 * r, r, "Full path of the folder");
                                if edited && ui_kit::button(ui, "SAVE", Kind::Primary, accent, r).clicked() { out = PluginsOut::SetGamesDir(p.id().to_string(), buf.clone()); }
                                if edited && ui_kit::button(ui, "RESET", Kind::Plain, accent, r).clicked() { out = PluginsOut::SetGamesDir(p.id().to_string(), String::new()); }
                                if ui_kit::button(ui, "BROWSE", Kind::Plain, accent, r).clicked() {
                                    // the system's folder chooser (through the desktop portal on Linux), off the UI thread
                                    let (tx, ctx2, start) = (self.tx.clone(), self.ctx.clone(), dir.clone());
                                    std::thread::spawn(move || { let p = rfd::FileDialog::new().set_title("Games folder").set_directory(start).pick_folder(); let _ = tx.send(Msg::Folder(i, p)); ctx2.request_repaint(); });
                                }
                                if ui_kit::button(ui, "OPEN", Kind::Plain, accent, r).clicked() { let _ = std::fs::create_dir_all(&dir); out = PluginsOut::OpenUrl(dir.to_string_lossy().to_string()); }
                            });
                        }
                    let steps = pd_filter::companion::actions(p.id());
                        if !steps.is_empty() {
                            ui.horizontal(|ui| {
                                for (service, text) in steps { if ui_kit::button(ui, text, Kind::Plain, accent, r).clicked() { out = PluginsOut::Companion(service.to_string()); } }
                                if ui_kit::button(ui, "INSTALL USERSCRIPT", Kind::Plain, accent, r).clicked() { out = PluginsOut::OpenUrl(pd_filter::companion::INSTALL_URL.to_string()); }
                            });
                            ui.label(ui_kit::muted("These use the Zest Companion userscript in your browser (it needs Tampermonkey): it opens the page and reads it while you are signed in. INSTALL USERSCRIPT opens it on GitHub, where Tampermonkey offers to install it.", 0.72 * r));
                        }
                    });
                    ui.add_space(0.5 * r);
                }
                if !self.legacy.is_empty() {
                    ui.add_space(0.4 * r);
                    ui.label(egui::RichText::new("PlayDate plugins found in this folder").size(0.95 * r).strong().color(crate::ui_kit::heading()));
                    ui.label(ui_kit::muted("These are written for PlayDate and cannot run in Zest. Your games from them are already in the library and stay as they are.", 0.78 * r));
                    ui.add_space(0.3 * r);
                    for l in &self.legacy {
                        let built_in = self.plugins.iter().any(|e| e.src.id() == l.platform && e.plugin.is_none());
                        let planned = pd_filter::sources::KNOWN_PLATFORMS.iter().any(|(id, _)| *id == l.platform);
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(&l.name).size(0.9 * r).color(crate::ui_kit::heading()));
                            ui.label(ui_kit::muted(format!("v{}  \u{b7}  {}", l.version, if built_in { "built into Zest now, this plugin is not used" } else if planned { "Zest will include this store built in" } else { "needs a Zest version from its author" }), 0.75 * r));
                        });
                    }
                }
                for (name, e) in &self.errors { ui.label(egui::RichText::new(format!("{name}: {e}")).size(0.8 * r).color(crate::ui_kit::warning())); }
            });
        });
        if res.closed { return PluginsOut::Close; }
        out
    }
}

impl PluginsDialog {
    /// After a games-folder change was saved (or refused): show the folder now in use again.
    pub fn folder_changed(&mut self) { for r in &mut self.rows { r.dir_edit = None; } }
}
