//! Menu -> Check for Updates. Asks GitHub for a newer Zest, shows its notes, and downloads the file for this install: a Flatpak bundle is installed for the user (through the host),
//! anything else lands in Downloads with the folder opened. Zest does not replace itself or roll an update back yet, so a download is something the user finishes.
use crate::ui_kit::{self, Kind};
use pd_filter::net::Cancel;
use pd_filter::updater::{self, Install, Update};
use std::sync::{mpsc, Arc, Mutex};

enum Msg { Checked(Result<Option<Update>, String>), Downloaded(Result<String, String>) }

pub struct UpdateDialog {
    root: std::path::PathBuf, beta: bool, auto: bool, status: String, found: Option<Update>, busy: bool, progress: Arc<Mutex<(u64, u64)>>, cancel: Cancel,
    tx: mpsc::Sender<Msg>, rx: mpsc::Receiver<Msg>, ctx: egui::Context, asked: bool,
}
pub enum UpdateOut { None, Close, /// The user changed a setting (`beta_updates`, `check_updates`): value to store.
    Pref(&'static str, bool), /// A file or folder to open.
    Open(String), /// The update was swapped in: quit so the supervisor can start the new program.
    Restart }

impl UpdateDialog {
    pub fn new(ctx: &egui::Context, root: std::path::PathBuf, beta: bool, auto: bool) -> Self {
        let (tx, rx) = mpsc::channel();
        let mut d = UpdateDialog { root, beta, auto, status: String::new(), found: None, busy: false, progress: Arc::new(Mutex::new((0, 0))), cancel: Cancel::new(), tx, rx, ctx: ctx.clone(), asked: false };
        d.check(); d
    }
    fn check(&mut self) {
        self.busy = true; self.found = None; self.asked = true; self.status = "Checking...".into();
        let (tx, ctx, beta, root) = (self.tx.clone(), self.ctx.clone(), self.beta, self.root.clone());
        std::thread::spawn(move || { let r = updater::check(&pd_filter::net::UreqHttp::new(), updater::REPOS, beta, &updater::failed_versions(&root)); let _ = tx.send(Msg::Checked(r)); ctx.request_repaint(); });
    }
    fn download(&mut self, u: &Update) {
        let Some(a) = u.asset.clone() else { return };
        self.busy = true; self.cancel = Cancel::new(); self.status = format!("Downloading {}...", a.name);
        let (tx, ctx, cancel, prog, root, version) = (self.tx.clone(), self.ctx.clone(), self.cancel.clone(), self.progress.clone(), self.root.clone(), u.version.clone());
        std::thread::spawn(move || {
            let http = pd_filter::net::UreqHttp::new();
            let r = updater::download(&http, &a, &updater::download_dir(), &cancel, &mut |d, t| { if let Ok(mut p) = prog.lock() { *p = (d, t); } ctx.request_repaint(); }).and_then(|path| {
                if Install::current() == Install::Flatpak {
                    // inside a sandbox the bundle is installed on the host: flatpak itself is not in here
                    let args = updater::flatpak_install_args(&path);
                    let (prog, a) = pd_filter::sandbox::wrap("flatpak", &args, None, &[], true);
                    let out = std::process::Command::new(prog).args(a).output().map_err(|e| format!("Could not run flatpak: {e}"))?;
                    if !out.status.success() { return Err(format!("flatpak could not install the update ({}). The file is at {}; a system-wide install needs: sudo flatpak install --system -y --reinstall {}", String::from_utf8_lossy(&out.stderr).trim(), path.display(), path.display())); }
                    return Ok(format!("INSTALLED:{}", path.display()));
                }
                // installed by Zest-Setup.exe: the new installer runs silently, closes this Zest (which quits anyway), installs and starts the new version
                if Install::current() == Install::WindowsSetup && path.extension().map_or(false, |e| e.eq_ignore_ascii_case("exe")) {
                    let mut c = std::process::Command::new(&path);
                    c.args(updater::setup_silent_args()).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
                    #[cfg(windows)] { use std::os::windows::process::CommandExt; c.creation_flags(0x0000_0008); }   // DETACHED_PROCESS: it outlives Zest
                    return match c.spawn() { Ok(_) => Ok("SETUP:".to_string()), Err(e) => Ok(format!("SWAPFAIL:Could not start the installer: {e}.\n{}", path.display())) };
                }
                // a tarball or zip of the program itself: swap it in and let the supervisor start it (the download stays in Downloads if that cannot be done here)
                let n = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
                if n.ends_with(".tar.gz") || (n.ends_with(".zip") && Install::current() == Install::Windows) {
                    return match pd_filter::selfupdate::install(&root, &path, &version) { Ok(()) => Ok("SWAPPED:".to_string()), Err(e) => Ok(format!("SWAPFAIL:{e}\n{}", path.display())) };
                }
                Ok(format!("FILE:{}", path.display()))
            });
            let _ = tx.send(Msg::Downloaded(r)); ctx.request_repaint();
        });
    }
    pub fn show(&mut self, ctx: &egui::Context, accent: egui::Color32) -> UpdateOut {
        let mut out = UpdateOut::None;
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Checked(r) => { self.busy = false; match r { Ok(Some(u)) => { self.status = String::new(); self.found = Some(u); } Ok(None) => self.status = format!("You're up to date (Zest {}).", updater::VERSION), Err(e) => self.status = e } }
                Msg::Downloaded(r) => { self.busy = false; match r {
                    Ok(s) if s.starts_with("INSTALLED:") => self.status = "Installed. Restart Zest to use the new version.".into(),
                    Ok(s) if s.starts_with("SETUP:") => { self.status = "Installing the update. Zest closes now and starts again when it is done.".into(); out = UpdateOut::Restart; }
                    Ok(s) if s.starts_with("SWAPPED:") => { self.status = "Updated. Zest restarts now; if the new version does not start, the old one comes back.".into(); out = UpdateOut::Restart; }
                    Ok(s) if s.starts_with("SWAPFAIL:") => { let (why, p) = s.trim_start_matches("SWAPFAIL:").split_once('\n').unwrap_or((&s, "")); self.status = format!("{why} The download is at {p}."); if let Some(d) = std::path::Path::new(p).parent() { out = UpdateOut::Open(d.to_string_lossy().to_string()); } }
                    Ok(s) => { let p = s.trim_start_matches("FILE:").to_string(); self.status = format!("Downloaded to {p}"); if let Some(d) = std::path::Path::new(&p).parent() { out = UpdateOut::Open(d.to_string_lossy().to_string()); } }
                    Err(e) => self.status = e } }
            }
        }
        let (d, t) = self.progress.lock().map(|p| *p).unwrap_or((0, 0));
        let mut download = false; let mut recheck = false; let mut page: Option<String> = None;
        let (mut beta, mut auto) = (self.beta, self.auto);
        let found = self.found.clone();
        let res = ui_kit::modal(ctx, "update", "Check for Updates", 40.0, |ui, r| {
            ui.label(ui_kit::muted(format!("This is Zest {}.", updater::VERSION), 0.85 * r));
            ui.add_space(0.3 * r);
            if let Some(u) = &found {
                ui.label(egui::RichText::new(format!("Zest {} is available{}", u.version, if u.prerelease { " (beta)" } else { "" })).color(ui_kit::TEXT()).strong().size(r));
                if !u.notes.is_empty() { egui::ScrollArea::vertical().max_height(12.0 * r).auto_shrink([false, true]).show(ui, |ui| { ui.label(ui_kit::muted(u.notes.clone(), 0.8 * r)); }); }
                ui.add_space(0.3 * r);
                ui.horizontal(|ui| {
                    let label = if matches!(Install::current(), Install::Flatpak | Install::WindowsSetup) || u.asset.as_ref().map_or(false, |a| a.name.ends_with(".tar.gz") || a.name.ends_with(".zip")) { "DOWNLOAD AND INSTALL" } else { "DOWNLOAD" };
                    if ui_kit::button(ui, label, Kind::Primary, accent, r).clicked() && !self.busy { download = true; }
                    if ui_kit::button(ui, "RELEASE PAGE", Kind::Plain, accent, r).clicked() { page = Some(u.page.clone()); }
                    if self.busy && ui_kit::button(ui, "CANCEL", Kind::Plain, accent, r).clicked() { self.cancel.cancel(); }
                });
                if self.busy && t > 0 { ui.add(egui::ProgressBar::new(d as f32 / t as f32).desired_width(20.0 * r)); }
                ui.label(ui_kit::muted("Zest swaps itself and keeps the old version for two failed starts. Your data is not rolled back: keep a backup.", 0.75 * r));
            } else if ui_kit::button(ui, "CHECK AGAIN", Kind::Plain, accent, r).clicked() && !self.busy { recheck = true; }
            if !self.status.is_empty() { ui.add_space(0.3 * r); ui.label(ui_kit::muted(self.status.clone(), 0.85 * r)); }
            ui.add_space(0.5 * r);
            ui.checkbox(&mut beta, egui::RichText::new("Include beta versions").size(0.85 * r).color(ui_kit::TEXT()));
            ui.checkbox(&mut auto, egui::RichText::new("Look for updates once a day when Zest starts").size(0.85 * r).color(ui_kit::TEXT()));
        });
        if beta != self.beta { self.beta = beta; out = UpdateOut::Pref("beta_updates", beta); if !self.busy { self.check(); } }
        if auto != self.auto { self.auto = auto; out = UpdateOut::Pref("check_updates", auto); }
        if download { if let Some(u) = found { self.download(&u); } }
        if recheck { self.check(); }
        if let Some(p) = page { out = UpdateOut::Open(p); }
        if res.closed { return UpdateOut::Close; }
        out
    }
}
