//! The first-run window: shown when Zest has no library and cannot tell which Steam account to use (no `config.json` account, no Steam files, no `--steam-id`). It runs as a child process
//! of the program itself (`zest --setup <data folder>`) so it can have its own event loop, and ends with a library in the data folder (made from a Steam ID, or restored from a
//! backup) or with nothing, when it is closed.
use crate::ui_kit::{self, Kind};
use egui_software_backend::{BufferMutRef, ColorFieldOrder, EguiSoftwareRender};
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowId};

pub struct App {
    root: PathBuf, window: Option<Rc<Window>>, surface: Option<softbuffer::Surface<winit::event_loop::OwnedDisplayHandle, Rc<Window>>>,
    ctx: egui::Context, egui_winit: Option<egui_winit::State>, renderer: EguiSoftwareRender,
    form: crate::account_form::AccountForm, status: Option<(String, bool)>, restoring: Option<mpsc::Receiver<Result<String, String>>>, done: bool,
    /// Progress of a running restore: the file is chosen (false while the picker is open), entries done, entries in the backup.
    rp: (Arc<AtomicBool>, Arc<AtomicUsize>, Arc<AtomicUsize>),
    /// ZEST_SETUP_SHOT=<png>: draw a few frames without showing the window, save the picture and stop (a test).
    shot: Option<(PathBuf, u32)>,
    /// The questions are answered and a library exists: the caller (`Shell` in main.rs) takes the window over and carries on in it.
    pub finished: bool,
    /// When the loop must wake to draw again (egui asked for a repaint later; nothing else wakes it, so a restore's counter would only move with the mouse).
    next_wake: Option<Instant>,
    /// The "opening your library" message has been drawn, so the slow hand-over can start.
    announced: bool,
    /// PlayDate libraries found on this machine, offered for import: (folder, (Steam account, games) per library).
    playdate: Vec<(PathBuf, Vec<(String, i64)>)>,
    /// A running PlayDate import: its answer, and (files done, files in all).
    importing: Option<mpsc::Receiver<Result<String, String>>>, ip: (Arc<AtomicBool>, Arc<AtomicUsize>, Arc<AtomicUsize>),
    /// Presses from the gamepad thread (the same one the main window keeps after the hand-over), so the window works with a controller alone (Steam Deck Game Mode).
    pub pad: Option<crate::gamepad::Queue>,
    /// The controller has been used: the focused control gets the same ring the main window draws (buttons draw none of their own).
    pad_used: bool,
    /// The right stick's vertical position (thousandths, up positive) from the gamepad thread, and when it was last read: it scrolls the window.
    pub pad_scroll: Option<crate::gamepad::Scroll>, stick_at: Instant,
}

/// Runs the window; true when a library exists afterwards.
pub fn run(root: PathBuf) -> bool {
    let event_loop = EventLoop::new().expect("event loop");
    let mut app = App::new(root.clone(), egui::Context::default());
    let _ = event_loop.run_app(&mut app);
    pd_filter::setup::find_library(&root).is_some()
}

impl App {
    pub fn new(root: PathBuf, ctx: egui::Context) -> App {
        crate::theme::install_fonts(&ctx, false);
        App { root, window: None, surface: None, ctx, egui_winit: None, renderer: EguiSoftwareRender::new(ColorFieldOrder::Bgra), form: { let mut f = crate::account_form::AccountForm::default(); f.detect(true); f }, status: None, restoring: None, done: false, rp: Default::default(), shot: std::env::var_os("ZEST_SETUP_SHOT").map(|p| (PathBuf::from(p), 0)), finished: false, next_wake: None, announced: false, playdate: pd_filter::import_playdate::find(&pd_filter::installdir::home()).into_iter().map(|d| { let s = pd_filter::import_playdate::summary(&d); (d, s) }).collect(), importing: None, ip: Default::default(), pad: None, pad_used: false, pad_scroll: None, stick_at: Instant::now() }
    }
    /// The window, its drawing surface and egui's window state, for the main app to carry on with once `finished`.
    pub fn take_handles(&mut self) -> Option<(Rc<Window>, softbuffer::Surface<winit::event_loop::OwnedDisplayHandle, Rc<Window>>, egui_winit::State)> {
        Some((self.window.take()?, self.surface.take()?, self.egui_winit.take()?))
    }
    fn create(&mut self) {
        let given = (self.form.steam_id.trim().to_string(), Some(self.form.api_key.trim().to_string()).filter(|k| !k.is_empty()));
        match pd_filter::setup::first_run(&self.root, Some(given), None) {
            Ok(m) => {
                // first_run keeps no label; the form's is saved with the account
                let a = pd_filter::account::Account { steam_id: m.steam_id, api_key: self.form.api_key.trim().to_string(), label: self.form.label.trim().to_string() };
                if !a.label.is_empty() { let _ = pd_filter::account::save_account(&self.root, &a, true); }
                self.done = true
            }
            Err(e) => self.status = Some((e, false)),
        }
    }
    fn restore(&mut self) {
        let root = self.root.clone();
        let (tx, rx) = mpsc::channel(); self.restoring = Some(rx); self.status = None;
        let (picked, done, total) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        self.rp = (picked.clone(), done.clone(), total.clone());
        std::thread::spawn(move || {
            let Some(zip) = rfd::FileDialog::new().set_title("Choose a backup").add_filter("Zip", &["zip"]).pick_file() else { let _ = tx.send(Err(String::new())); return };
            picked.store(true, Ordering::Relaxed);
            let r = pd_filter::backup::restore(&root, &zip, &mut |d, t| { done.store(d, Ordering::Relaxed); total.store(t, Ordering::Relaxed); }, &pd_filter::net::Cancel::new()).map(|r| format!("Restored {} item(s).", r.restored.len()));
            let _ = tx.send(r);
        });
    }
    /// Copies a PlayDate library (`from`, or a folder the person picks) into Zest's folder on a worker thread; the window shows the progress and carries on when it is done.
    fn import(&mut self, from: Option<PathBuf>) {
        let root = self.root.clone();
        let (tx, rx) = mpsc::channel(); self.importing = Some(rx); self.status = None;
        let (picked, done, total) = (Arc::new(AtomicBool::new(from.is_some())), Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        self.ip = (picked.clone(), done.clone(), total.clone());
        std::thread::spawn(move || {
            let from = match from { Some(f) => f, None => match rfd::FileDialog::new().set_title("Choose PlayDate's folder (the one with games_... .db in it)").pick_folder() { Some(f) => f, None => { let _ = tx.send(Err(String::new())); return } } };
            picked.store(true, Ordering::Relaxed);
            let r = pd_filter::import_playdate::import(&from, &root, &mut |d, t| { done.store(d, Ordering::Relaxed); total.store(t, Ordering::Relaxed); }, &pd_filter::net::Cancel::new())
                .map(|i| format!("Imported {} librar{} and {} cover(s).", i.libraries, if i.libraries == 1 { "y" } else { "ies" }, i.covers));
            let _ = tx.send(r);
        });
    }
    /// Ends the process at once (as the main window does: the Wayland teardown of a clipboard thread can crash otherwise); the exit code says whether a library exists.
    fn finish(&self) -> ! { std::process::exit(if pd_filter::setup::find_library(&self.root).is_some() { 0 } else { 1 }) }
    fn frame(&mut self, _el: &ActiveEventLoop) {
        let (Some(window), Some(st)) = (self.window.clone(), self.egui_winit.as_mut()) else { return };
        let sz = window.inner_size(); let (w, h) = (sz.width.max(1) as usize, sz.height.max(1) as usize);
        if let Some(s) = self.surface.as_mut() { s.resize(NonZeroU32::new(w as u32).unwrap(), NonZeroU32::new(h as u32).unwrap()).ok(); }
        if let Some(rx) = &self.restoring {
            match rx.try_recv() {
                Ok(Ok(m)) => { self.restoring = None; if pd_filter::setup::find_library(&self.root).is_some() { self.done = true; } else { self.status = Some((format!("{m} That backup holds no library."), false)); } }
                Ok(Err(e)) => { self.restoring = None; self.status = if e.is_empty() { None } else { Some((e, false)) }; }
                Err(_) => {}
            }
        }
        if let Some(rx) = &self.importing {
            match rx.try_recv() {
                Ok(Ok(m)) => { self.importing = None; if pd_filter::setup::find_library(&self.root).is_some() { self.done = true; } else { self.status = Some((format!("{m} No library came with it."), false)); } }
                Ok(Err(e)) => { self.importing = None; self.status = if e.is_empty() { None } else { Some((e, false)) }; }
                Err(_) => {}
            }
        }
        let mut raw = st.take_egui_input(&window);
        // the controller walks the controls in order (down/right = next, up/left = previous) and A presses the focused one
        if let Some(q) = &self.pad {
            use crate::gamepad::Nav;
            for (n, pressed) in q.lock().unwrap().drain(..) {
                self.pad_used = true;
                let (key, shift) = match n { Nav::Down | Nav::Right => (egui::Key::Tab, false), Nav::Up | Nav::Left => (egui::Key::Tab, true), Nav::Confirm => (egui::Key::Enter, false), _ => continue };
                raw.events.push(egui::Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers: egui::Modifiers { shift, ..Default::default() } });
            }
        }
        let accent = ui_kit::accent();
        let mut create = false; let mut restore = false; let mut open: Option<&str> = None;
        let busy = self.restoring.is_some() || self.importing.is_some();
        let mut import: Option<Option<PathBuf>> = None;
        let found = self.playdate.clone();
        let import_status = self.importing.as_ref().map(|_| { let (d, t) = (self.ip.1.load(Ordering::Relaxed), self.ip.2.load(Ordering::Relaxed));
            if !self.ip.0.load(Ordering::Relaxed) { "Choose PlayDate's folder...".to_string() } else if t > 0 { format!("Importing your PlayDate library: {d} of {t} files. Please keep this window open.") } else { "Importing your PlayDate library...".to_string() } });
        let mut form = std::mem::take(&mut self.form);
        form.poll(&[]);
        let mut new_status: Option<(String, bool)> = None;
        let picked = self.rp.0.load(Ordering::Relaxed) || self.ip.0.load(Ordering::Relaxed);
        let opening = self.announced;
        let busy = busy || opening;
        let status = if opening { Some(("Opening your library. The first time this prepares the cover images, which can take a little while.".to_string(), true)) } else if busy {
            Some((if picked { let (d, t) = (self.rp.1.load(Ordering::Relaxed), self.rp.2.load(Ordering::Relaxed)); if t > 0 { format!("Restoring your backup: {d} of {t}. This can take a minute with many covers. Please keep this window open.") } else { "Restoring your backup. Please keep this window open.".to_string() } } else { "Choose a backup file in the file window...".to_string() }, true))
        } else { self.status.clone() };
        let status = match import_status { Some(m) if !opening => Some((m, true)), _ => status };
        let mut closed = false;
        let pad_used = self.pad_used;
        // the right stick scrolls, as in the main window's dialogs (not while typing in a text box); about 1,200 px a second at full tilt
        let now = Instant::now(); let dt = now.duration_since(self.stick_at).as_secs_f32().min(0.1); self.stick_at = now;
        let up = self.pad_scroll.as_ref().map_or(0.0, |s| s.load(Ordering::Relaxed) as f32 / 1000.0);
        let stick_dy = if up.abs() >= crate::gamepad::dead_zone() && !self.ctx.text_edit_focused() { up * 20.0 * 60.0 * dt } else { 0.0 };
        let out = self.ctx.run_ui(raw, |ui| {
            egui::CentralPanel::default().frame(egui::Frame::new().fill(egui::Color32::from_rgb(0x0d, 0x11, 0x17))).show_inside(ui, |_| {});
            let res = ui_kit::modal(ui.ctx(), "setup", "Welcome to Zest", 36.0, |ui, r| {
                ui_kit::style_fields(ui);
                if stick_dy != 0.0 { ui.scroll_with_delta(egui::vec2(0.0, stick_dy)); ui.ctx().request_repaint(); }
                // coming from PlayDate: its library is copied (PlayDate keeps its own, so it still works)
                ui.label(egui::RichText::new("Coming from PlayDate?").strong().color(crate::ui_kit::heading()).size(r));
                ui.label(ui_kit::muted(if found.is_empty() { "Zest did not find a PlayDate library on this computer. If you have one, choose its folder (the one with games_....db in it)." } else { "Zest can copy your PlayDate library: games, statuses, settings, sign-ins and covers. PlayDate keeps its own copy and still works." }, 0.82 * r));
                let home = pd_filter::installdir::home();
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
                for (dir, libs) in &found {
                    ui.horizontal(|ui| {
                        if ui_kit::button(ui, "IMPORT", Kind::Primary, accent, r).on_hover_text(dir.display().to_string()).clicked() && !busy { import = Some(Some(dir.clone())); }
                        let games: i64 = libs.iter().map(|(_, n)| n).sum();
                        let used = pd_filter::import_playdate::last_used(dir).map(|t| format!(", last used {}", pd_filter::import_playdate::ago(t, now))).unwrap_or_default();
                        ui.label(egui::RichText::new(pd_filter::import_playdate::label(dir, &home)).color(crate::ui_kit::heading()).size(0.85 * r));
                        ui.label(ui_kit::muted(format!("{games} game{}{used}", if games == 1 { "" } else { "s" }), 0.82 * r));
                    });
                }
                if ui_kit::button(ui, if found.is_empty() { "IMPORT FROM A FOLDER..." } else { "IMPORT FROM ANOTHER FOLDER..." }, Kind::Plain, accent, r).clicked() && !busy { import = Some(None); }
                ui.add_space(0.8 * r);
                ui.label(egui::RichText::new("Set up your library").strong().color(crate::ui_kit::heading()).size(r));
                ui.label(ui_kit::muted("Zest keeps your games in a library on this computer. Sign in with Steam, or find the account Steam already has on this computer, or type your Steam ID. Other stores are added later from the Plugins dialog.", 0.82 * r));
                ui.add_space(0.4 * r);
                if let Some(u) = form.ui(ui, r, accent) { open = Some(u); }
                if let Some(m) = form.status.take() { new_status = Some(m); }
                ui.add_space(0.6 * r);
                ui.horizontal(|ui| {
                    if ui_kit::button(ui, "CREATE LIBRARY", Kind::Primary, accent, r).clicked() && !busy { create = true; }
                    if ui_kit::button(ui, "RESTORE A BACKUP...", Kind::Plain, accent, r).clicked() && !busy { restore = true; }
                });
                if let Some((m, ok)) = new_status.as_ref().or(status.as_ref()) { ui.add_space(0.3 * r); ui.label(egui::RichText::new(m).size(0.85 * r).color(if *ok { egui::Color32::from_rgb(0x8b, 0xb4, 0x2c) } else { crate::ui_kit::text_danger() })); }
            });
            closed = res.closed && !(busy && picked);
            // the focus ring, as the main window draws it (pad_focus_ring in main.rs)
            if pad_used {
                if let Some(resp) = ui.ctx().memory(|m| m.focused()).and_then(|id| ui.ctx().read_response(id)) {
                    let p = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("pad_focus_ring")));
                    let rr = resp.rect.expand(2.0);
                    p.rect_stroke(rr.expand(1.0), 4, egui::Stroke::new(3.0_f32, egui::Color32::BLACK), egui::StrokeKind::Outside);
                    p.rect_stroke(rr, 4, egui::Stroke::new(2.0_f32, ui_kit::focus_ring()), egui::StrokeKind::Outside);
                }
            }
            if busy { ui.ctx().request_repaint_after(Duration::from_millis(100)); }
        });
        self.form = form; if new_status.is_some() { self.status = new_status; }
        st.handle_platform_output(&window, out.platform_output);
        if let Some(u) = open { crate::open_url(u); }
        if create { self.create(); }
        if restore { self.restore(); }
        if let Some(from) = import { self.import(from); }
        let ppp = out.pixels_per_point;
        let prims = self.ctx.tessellate(out.shapes, ppp);
        if let Some(surface) = self.surface.as_mut() {
            if let Ok(mut buf) = surface.buffer_mut() {
                buf.fill(0xff0d1117);
                let mut bref = BufferMutRef::new(bytemuck::cast_slice_mut(&mut buf[..]), w, h);
                self.renderer.render(&mut bref, &prims, &out.textures_delta, ppp);
                if let Some((path, n)) = &mut self.shot {
                    *n += 1;
                    if *n >= 12 { let rgb: Vec<u8> = buf.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8]).collect(); if let Some(img) = image::RgbImage::from_raw(w as u32, h as u32, rgb) { let _ = img.save(&path); } self.done = true; }
                }
                let _ = buf.present();
            }
        }
        if closed { self.finish(); }
        if self.done {
            if self.shot.is_some() { self.finish(); }
            // the hand-over blocks this thread for a while (the library and its cover packs load): say so first, and let that frame reach the screen
            else if !self.announced { self.announced = true; window.request_redraw(); return; }
            else { self.finished = true; return; }
        }
        let delay = out.viewport_output.get(&egui::ViewportId::ROOT).map(|v| v.repaint_delay).unwrap_or(Duration::MAX);
        if delay.is_zero() || self.shot.is_some() { window.request_redraw(); }
        self.next_wake = if !delay.is_zero() && delay < Duration::from_secs(3600) { Some(Instant::now() + delay) } else { None };
    }
}

impl App {
    /// The gamepad thread has something: draw, which reads it.
    pub fn wake(&self) { if let Some(w) = &self.window { w.request_redraw(); } }
}

impl ApplicationHandler<()> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_some() { return; }
        let mut attrs = Window::default_attributes().with_title("Zest").with_inner_size(winit::dpi::LogicalSize::new(760.0, 640.0)).with_visible(self.shot.is_none());
        if let Ok(img) = image::load_from_memory(include_bytes!("../../assets/favicon.png")) {
            let img = img.resize(128, 128, image::imageops::FilterType::Lanczos3).to_rgba8();
            if let Ok(icon) = winit::window::Icon::from_rgba(img.to_vec(), img.width(), img.height()) { attrs = attrs.with_window_icon(Some(icon)); }
        }
        #[cfg(target_os = "linux")]
        { use winit::platform::wayland::WindowAttributesExtWayland; let id = std::env::var("FLATPAK_ID").unwrap_or_else(|_| "zest-dev".into()); attrs = attrs.with_name(id.clone(), id); }
        let window = Rc::new(el.create_window(attrs).expect("window"));
        // Steam Deck Game Mode (gamescope): full screen, as the main window opens there (it keeps this window after the questions)
        if std::env::var_os("GAMESCOPE_WAYLAND_DISPLAY").is_some() || std::env::var("XDG_SESSION_DESKTOP").map_or(false, |d| d == "gamescope") { window.set_fullscreen(Some(winit::window::Fullscreen::Borderless(None))); }
        let context = softbuffer::Context::new(el.owned_display_handle()).expect("softbuffer context");
        self.surface = Some(softbuffer::Surface::new(&context, window.clone()).expect("surface"));
        self.egui_winit = Some(egui_winit::State::new(self.ctx.clone(), egui::ViewportId::ROOT, &window, Some(window.scale_factor() as f32), None, None));
        window.request_redraw();
        self.window = Some(window);
    }
    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        use winit::event_loop::ControlFlow;
        match self.next_wake {
            Some(t) if Instant::now() >= t => { self.next_wake = None; if let Some(w) = &self.window { w.request_redraw(); } el.set_control_flow(ControlFlow::Wait); }
            Some(t) => el.set_control_flow(ControlFlow::WaitUntil(t)),
            None => el.set_control_flow(ControlFlow::Wait),
        }
    }
    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, ev: WindowEvent) {
        match ev {
            // closing while files are being extracted would leave a half-restored library
            WindowEvent::CloseRequested => { if !(self.restoring.is_some() && self.rp.0.load(Ordering::Relaxed)) { self.finish() } }
            WindowEvent::RedrawRequested => self.frame(el),
            other => { if let (Some(w), Some(st)) = (self.window.clone(), self.egui_winit.as_mut()) { if st.on_window_event(&w, &other).repaint { w.request_redraw(); } } }
        }
    }
}
