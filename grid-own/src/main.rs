//! Option 3 prototype: egui for the chrome (header, slider, scroll input) and our own CPU renderer for the grid.
//! The grid lives in a persistent pixel layer. Scrolling moves the existing pixels (a memory copy) and draws only
//! the newly exposed strip plus any cover that just changed; each frame the layer is copied to the window buffer
//! and egui's overlay (header text, scrollbar) is rasterized on top by the stock egui_software_backend renderer.
//! Covers are plain 0x00RRGGBB pixels (no egui textures, no GPU).
// a program with a window, not a console one: without this Windows opens a black console window behind it
#![cfg_attr(windows, windows_subsystem = "windows")]
use std::collections::HashSet;

mod card_actions;
mod pick_page;
mod ui_kit;
mod edit_dialog;
mod filter_model;
mod filter_dialog;
mod bulk_dialog;
mod pagywosg_dialog;
mod pad_drawing;
mod community_dialogs;
mod sync;
mod install_watch;
mod junk_dialog;
mod settings_dialog;
mod account_form;
mod plugins_dialog;
mod artstore;
mod crop_dialog;
mod add_game_dialog;
mod emulators_dialog;
mod data_dialog;
mod sendlog_dialog;
mod hltb_dialog;
mod update_dialog;
mod tutorial_text;
mod tutorial_dialog;
mod setup_window;
mod sgdb_picker;
mod badge_defaults;
mod gamepad;
mod padnav;

#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;
#[cfg(feature = "dhat-heap")]
static PROFILER: std::sync::Mutex<Option<dhat::Profiler>> = std::sync::Mutex::new(None);
#[cfg(test)]
mod select_test;
mod home;
mod theme;
mod appearance;
mod dbimport_ui;
mod playnite_dialog;
mod home_edit_ui;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use egui_software_backend::{BufferMutRef, ColorFieldOrder, EguiSoftwareRender};
use grid_core::{rss_mb, row_height, Core, Cover, HEADER_H, ROW_TOP};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

const BG: u32 = 0xFF00_0000;   // 'transparent': the page background shows through (real pixels never have the top byte set)
/// The empty card behind a cover that is not drawn yet: the theme's Card Background (`--bg-card`), packed as the pixel buffers expect.
fn card_bg() -> u32 { let c = ui_kit::card(); ((c.r() as u32) << 16) | ((c.g() as u32) << 8) | c.b() as u32 }
const GAP: usize = grid_core::CARD_GAP as usize;   // PlayDate's `.game-grid { gap: 25px }`
/// Pick 6 gamepad cursor: the page's controls get ids from here up (real appids are never this small).
const PAD_CTRL: i64 = i64::MIN / 2;
const MARGIN: usize = 20;   // left/right margin (PlayDate's grid padding: 20px); the (floating) scrollbar sits inside it

/// The grid layout: how many cards across, how wide each is, and the left offset that centres the block.
#[derive(Clone, Copy, PartialEq)]
struct Lay { cols: usize, cw: u32, x_off: usize }

/// The slider is a minimum card width: fit as many cards as possible at that size, then stretch them a
/// little so each row exactly fills the width (equal margins left and right).
fn layout(avail_w: f32, target: u32) -> Lay {
    let (g, w) = (GAP as i64, (avail_w as i64 - 2 * MARGIN as i64).max(60));
    let cols = ((w + g) / (target as i64 + g)).max(1);
    let cw = ((w - (cols - 1) * g) / cols).max(40);
    let used = cols * cw + (cols - 1) * g;
    Lay { cols: cols as usize, cw: cw as u32, x_off: (MARGIN as i64 + (w - used) / 2) as usize }
}
const PASSES: [(u32, f32); 5] = [(140, 12.0), (200, 12.0), (200, 30.0), (320, 12.0), (320, 30.0)];

struct Pixels { w: usize, h: usize, px: Vec<u32> }

/// One card to paint: its rectangle in layer coordinates and the cover (None = a plain placeholder rectangle).
struct Op { x: i64, y: i64, w: i64, h: i64, img: Option<Arc<Pixels>> }

/// Linear interpolation of two packed 0x00RRGGBB pixels, t in 0..=256 (red and blue are done together, green apart).
#[inline(always)]
fn lerp(a: u32, b: u32, t: u32) -> u32 {
    let rb = (((a & 0x00FF_00FF) * (256 - t) + (b & 0x00FF_00FF) * t) >> 8) & 0x00FF_00FF;
    let g = (((a & 0x0000_FF00) * (256 - t) + (b & 0x0000_FF00) * t) >> 8) & 0x0000_FF00;
    rb | g
}

/// Paint one card into `band` (rows band_y0.. of a layer `stride` pixels wide), only the rows that fall inside the band.
fn paint_op(band: &mut [u32], band_y0: i64, stride: usize, op: &Op) {
    let rows = (band.len() / stride) as i64;
    let (top, bot) = (op.y.max(band_y0), (op.y + op.h).min(band_y0 + rows));
    if top >= bot || op.x >= stride as i64 || op.x + op.w <= 0 { return; }
    // a card sliding in can start left of the layer (a shelf that begins near the window's edge): only the part inside it is painted. Skipping the whole card
    // there (as this once did for any negative x) left the background showing where the cover should be.
    let x0 = op.x.max(0);
    let skip = (x0 - op.x) as usize;
    let right = (op.x + op.w).min(stride as i64);
    let n = (right - x0) as usize;
    let Some(p) = &op.img else {
        for row in top..bot { let d = (row - band_y0) as usize * stride + x0 as usize; band[d..d + n].fill(card_bg()); }
        return;
    };
    if p.w as i64 == op.w && p.h as i64 == op.h {   // exact size: a plain copy
        for row in top..bot {
            let (sy, d) = ((row - op.y) as usize, (row - band_y0) as usize * stride + x0 as usize);
            band[d..d + n].copy_from_slice(&p.px[sy * p.w + skip..sy * p.w + skip + n]);
        }
        return;
    }
    // another size: bilinear. Source position of destination pixel centre i, in 1/256 source pixels.
    let (pw, ph) = (p.w as i64, p.h as i64);
    let xs: Vec<(usize, usize, u32)> = (0..n as i64).map(|i| {
        let i = i + skip as i64;   // the card's own column, counted from its left edge
        let pos = (((2 * i + 1) * pw * 256) / (2 * op.w) - 128).max(0);
        let x0 = ((pos >> 8) as usize).min(p.w - 1);
        (x0, (x0 + 1).min(p.w - 1), (pos & 255) as u32)
    }).collect();
    for row in top..bot {
        let dy = row - op.y;
        let pos = (((2 * dy + 1) * ph * 256) / (2 * op.h) - 128).max(0);
        let y0 = ((pos >> 8) as usize).min(p.h - 1);
        let (y1, fy) = ((y0 + 1).min(p.h - 1), (pos & 255) as u32);
        let (r0, r1) = (&p.px[y0 * p.w..(y0 + 1) * p.w], &p.px[y1 * p.w..(y1 + 1) * p.w]);
        let d = (row - band_y0) as usize * stride + x0 as usize;
        for (i, &(sx0, sx1, fx)) in xs.iter().enumerate() {
            band[d + i] = lerp(lerp(r0[sx0], r0[sx1], fx), lerp(r1[sx0], r1[sx1], fx), fy);
        }
    }
}

/// The persistent grid layer plus what it currently shows.
struct Layer {
    w: usize, h: usize,
    buf: Vec<u32>,
    drawn_y: i64,
    drawn: (usize, u32, usize),   // (cols, card width, x offset) the content was drawn for
    valid: bool,
    dirty: HashSet<usize>,
    stats: (u64, u64),     // (full redraws, rows exposed by scrolling)
}

impl Layer {
    fn new() -> Self { Layer { w: 0, h: 0, buf: Vec::new(), drawn_y: 0, drawn: (0, 0, 0), valid: false, dirty: HashSet::new(), stats: (0, 0) } }

    fn fill_rows(&mut self, y0: usize, y1: usize) {
        let (w, y1) = (self.w, y1.min(self.h));
        if y0 < y1 { self.buf[y0 * w..y1 * w].fill(BG); }
    }

    /// What to draw for game `idx` (in column `col` of a row whose top is `row_y`): its card rectangle in layer coordinates and the image (or placeholder).
    fn plan_card(core: &mut Core<Arc<Pixels>>, idx: usize, col: usize, row_y: f32, lay: Lay, y: i64) -> Op {
        let cw = lay.cw;
        Op { x: (lay.x_off + col * (cw as usize + GAP)) as i64, y: row_y.round() as i64 + ROW_TOP as i64 - y, w: cw as i64, h: grid_core::card_h(cw) as i64,
             img: match core.cover(idx) { Cover::Sharp(p) | Cover::Blurry(p) => Some(p), Cover::None => None } }
    }

    /// Cards whose rows intersect [clip0, clip1) at scroll offset y.
    fn draw_rows(&mut self, core: &mut Core<Arc<Pixels>>, lay: Lay, y: i64, clip0: i64, clip1: i64) {
        let rows: Vec<(f32, usize, usize)> = core.rows_in((y + clip0) as f32, (y + clip1) as f32).map(|i| { let r = &core.rows[i]; (r.y, r.first, r.count) }).collect();
        let mut ops = Vec::new();
        for (ry, first, count) in rows { for k in 0..count { ops.push(Self::plan_card(core, first + k, k, ry, lay, y)); } }
        self.paint(&ops, clip0, clip1);
    }

    /// Paint cards into rows [clip0, clip1). A cover whose size differs from its card (the window or slider just changed and the
    /// new size is still decoding) is drawn with bilinear filtering; large areas are split into bands painted on several threads.
    fn paint(&mut self, ops: &[Op], clip0: i64, clip1: i64) {
        let (w, h) = (self.w, self.h as i64);
        let (c0, c1) = (clip0.max(0), clip1.min(h));
        if c0 >= c1 || w == 0 { return; }
        let scaled = ops.iter().any(|o| o.img.as_ref().map_or(false, |p| p.w as i64 != o.w || p.h as i64 != o.h));
        let area = (c1 - c0) as usize * w;
        let region = &mut self.buf[c0 as usize * w..c1 as usize * w];
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(8);
        if scaled && area > 300_000 && threads > 1 {
            let rows_per = (((c1 - c0) as usize + threads - 1) / threads).max(1);
            std::thread::scope(|sc| {
                for (i, band) in region.chunks_mut(rows_per * w).enumerate() {
                    let band_y0 = c0 + (i * rows_per) as i64;
                    sc.spawn(move || for op in ops { paint_op(band, band_y0, w, op); });
                }
            });
        } else {
            for op in ops { paint_op(region, c0, w, op); }
        }
    }

    fn update(&mut self, core: &mut Core<Arc<Pixels>>, w: usize, h: usize, y: i64, lay: Lay, changed: &[usize]) {
        for &i in changed { self.dirty.insert(i); }
        let dy = y - self.drawn_y;
        if !self.valid || (w, h) != (self.w, self.h) || self.drawn != (lay.cols, lay.cw, lay.x_off) || dy.unsigned_abs() as usize >= h {
            if (w, h) != (self.w, self.h) { self.w = w; self.h = h; self.buf = vec![BG; w * h]; }
            self.fill_rows(0, h);
            self.draw_rows(core, lay, y, 0, h as i64);
            self.valid = true; self.drawn = (lay.cols, lay.cw, lay.x_off); self.drawn_y = y;
            self.stats.0 += 1;
            self.dirty.clear();
            return;
        }
        let (wu, hu) = (w, h);
        if dy > 0 {
            let d = dy as usize;
            self.buf.copy_within(d * wu..hu * wu, 0);                      // content moves up
            self.fill_rows(hu - d, hu);
            self.draw_rows(core, lay, y, (hu - d) as i64, hu as i64);  // exposed strip at the bottom
            self.stats.1 += d as u64;
        } else if dy < 0 {
            let d = (-dy) as usize;
            self.buf.copy_within(0..(hu - d) * wu, d * wu);                // content moves down
            self.fill_rows(0, d);
            self.draw_rows(core, lay, y, 0, d as i64);                // exposed strip at the top
            self.stats.1 += d as u64;
        }
        self.drawn_y = y;
        // covers that just arrived (or changed): redraw them in place; the exposed strip is already fresh
        let dirty: Vec<usize> = self.dirty.drain().collect();
        for idx in dirty { if let Some((ry, col)) = core.pos_of(idx) { let op = Self::plan_card(core, idx, col, ry, lay, y); self.paint(&[op], 0, h as i64); } }
    }
}

/// --resize-test: simulate dragging the window edge (a width that changes every frame), then hold, and count what the user would see.
struct ResizeTest { start: Instant, base: f32, frames: u32, blank: u32, nonsharp: u32, settled_at: Option<f32>, prev: Vec<u8>, regress: u32, w_min: u32, w_max: u32, real: bool }

/// Interactive resize logging: whenever the window width changes, record each frame; a second after the last change print a summary.
struct FrameRec { t: f32, w: usize, cols: usize, cw: u32, st: [u32; 4], regress: u32, ms: f32, gap_ms: f32 }
#[derive(Default)]
struct ResizeLog { recs: Vec<FrameRec>, last_w: usize, last_change: Option<Instant>, prev: Vec<u8>, events: u32, full_redraws0: u64, started: Option<Instant>, last_frame: Option<Instant> }

#[derive(Clone, Copy, PartialEq)]
enum Page { Home, Library, Pick }

/// The Home page's data, cover cache and what the last frame looked like.
/// Watches the Steam library folders for manifest changes (a game installed, finished downloading, updated or removed) and wakes the UI.
struct SteamWatch { _watcher: notify::RecommendedWatcher, rx: std::sync::mpsc::Receiver<()> }

fn start_steam_watch(paths: Vec<PathBuf>, ctx: egui::Context) -> Option<SteamWatch> {
    use notify::Watcher;
    let (tx, rx) = std::sync::mpsc::channel();
    let mut w = notify::recommended_watcher(move |ev: notify::Result<notify::Event>| {
        if let Ok(ev) = ev {
            let manifest = ev.paths.iter().any(|p| p.file_name().and_then(|n| n.to_str()).map_or(false, |n| n.starts_with("appmanifest_") && n.ends_with(".acf")));
            if manifest { let _ = tx.send(()); ctx.request_repaint(); }
        }
    }).ok()?;
    let mut any = false;
    for p in &paths { if w.watch(p, notify::RecursiveMode::NonRecursive).is_ok() { any = true; } }
    any.then_some(SteamWatch { _watcher: w, rx })
}

struct HomeState {
    steam_watch: Option<SteamWatch>,
    install_watch: Option<install_watch::InstallWatch>,   // installs and uninstalls of non-Steam games (EA, Legacy Games, Battle.net and the folder stores)
    steam_pending: Option<Instant>,   // a manifest changed: sync once it has been quiet for a moment (Steam writes a manifest several times)
    data: home::HomeData, covers: home::Covers, size: (usize, usize), changed_at: Instant,
    engine: card_actions::Engine,
    pointer: Option<egui::Pos2>,
    press: Option<(card_actions::Button, i64)>,
    hover: std::collections::HashMap<i64, f32>,   // 0..1 grow animation per card
    anim_at: Instant,
    /// Each shelf's shown offset (in cards) while it slides to its real one, and when it was last moved.
    shelf_pos: std::collections::HashMap<usize, f32>, slide_at: Instant,
    /// Home scrolls when its shelves are taller than the window: where it is, where it is going, and when it last moved.
    home_scroll: f32, home_scroll_target: f32, scroll_at: Instant,
    /// Messages from what the user did or started. They stay until clicked away: a result that vanishes after a moment is a result nobody can check.
    notices: Vec<(String, Instant)>,
    sync_status: std::collections::HashMap<String, pd_filter::status::SyncStatus>,
    menu: Option<(i64, egui::Pos2)>,
    menu_rect: Option<egui::Rect>,
    sub_rect: Option<egui::Rect>,
    open_edit: Option<i64>,
    tip_pin: Option<i64>,   // the `tooltip` click rule: shown until the next press or Esc
    shelf_off: std::collections::HashMap<usize, usize>,   // per shelf: the first card shown when its pool scrolls
    wheel_acc: f32,
    entering: bool,                  // the Home page was just opened (launch or page change)
    hold_until: Option<Instant>,     // until then Home shows only the background and bar, so no card ever appears as an empty box
    accent: egui::Color32,
    dirty: bool,   // the game list changed under the Library (a delete)
    confirm: Option<i64>,   // a delete waiting for the user's answer
    update_rx: Option<mpsc::Receiver<Result<Option<pd_filter::updater::Update>, String>>>,   // the quiet daily check for a newer Zest
    update_started: bool,
    populate_pending: bool,   // add the account's new games as soon as the window is up (every start)
    job_queue: Vec<(egui::Context, sync::Kind, Vec<i64>, String)>,   // jobs waiting for the running one to end (the fill-in of a game just added)
    /// The game whose install is running: when it ends with several programs in its folder and none chosen, the user is asked which one starts it.
    install_target: Option<i64>,
    /// "Ask each time": the builds a store offered for a game (filled by a worker thread), and the question on screen.
    install_opts_slot: Arc<std::sync::Mutex<Option<(i64, String, Result<Vec<pd_filter::sources::InstallOption>, String>)>>>,
    install_pick: Option<(i64, String, Vec<pd_filter::sources::InstallOption>)>,
    exe_pick: Option<(i64, String, bool, Vec<String>)>,   // the user picks the program for a game: (game, name, launch it afterwards, candidates)
    pick: pick_page::PickState,
    sync: Option<sync::SyncJob>,
    /// A one-game art change from the Edit dialog runs on its own beside any other job, so it never waits behind a long sync
    art_job: Option<sync::SyncJob>,
    /// The covers of games just added: fetched in this second slot while the details are filled in (`sync`), so a new card gets its cover without waiting for the metadata.
    fill_art: Option<sync::SyncJob>,
    /// Set for the one `start_job` call that puts a cover job into `fill_art`.
    art_to_fill: bool,
    /// The Zest Companion userscript's local listener (started the first time a browser step is asked for), what it reported, and records waiting for the running job to end.
    companion: Option<pd_filter::companion::Listener>,
    /// The Steam activation-date run the browser script reads (made with the listener, once the account is known).
    steam_dates: Option<Arc<pd_filter::steam_dates::SteamDates>>,
    /// Which games' art is wanted soonest: the pages say what they show, the art job ranks its queue by it.
    art_focus: Arc<pd_filter::artfocus::ArtFocus>,
    focus_sig: (usize, usize, usize, i64),
    /// New Steam games waiting for their purchase dates (read when signed in to Steam and no other date run is going).
    dates_pending: Vec<i64>,
    /// The art tab the Edit dialog was on (game, tab): a job that finishes reopens the dialog, which must come back to the same tab.
    edit_art: Option<(i64, usize)>,
    /// An art operation for this game has just staged a new picture: its open Edit dialog shows it (see `artstore`).
    art_staged_for: Option<i64>,
    companion_events: Arc<std::sync::Mutex<Vec<pd_filter::companion::Event>>>,
    companion_ctx: Option<egui::Context>,
    pushes: Vec<(String, Vec<pd_filter::importer::GameRecord>)>,
    companion_gen: u64,
    /// Messages from threads that watch a started game (it died at once); shown by the next frame.
    late_notices: Arc<std::sync::Mutex<Vec<String>>>,
    /// A Fetch Dates run in progress: (games from other stores, Steam games), so the bar counts all of them from the start and keeps the finished store part in its total.
    dates_run: Option<(usize, usize)>,
    installed_scanned: bool,
    custom_checked: Option<Instant>,   // when the manual games' programs were last looked for (Installed follows the file)
    /// The startup playtime and achievements sync: running (its answer arrives here as the line to log) or done.
    playtime_rx: Option<std::sync::mpsc::Receiver<(String, bool)>>,
    playtime_started: bool,
    /// Stores set to sync when Zest starts: when the window opened, whether the signed-in check ran, and the stores found signed in, waiting their turn (one job at a time).
    startup_at: Option<Instant>, startup_checked: bool, startup_ready: Arc<std::sync::Mutex<Vec<String>>>,
    /// The running PAGYWOSG event for the Edit dialog (fetched at start and again after an hour).
    live_rx: Option<std::sync::mpsc::Receiver<Result<pd_filter::pagywosg_filter::Spec, String>>>,
    live_at: Option<Instant>,
    /// The running event just arrived: an Edit dialog that is already open gets its panel.
    live_arrived: bool,
    opt_meta: bool, opt_cheevo: bool, opt_art_kinds: Vec<pd_filter::artclient::Kind>, opt_art_source: &'static str,
    sources: std::sync::Arc<pd_filter::sources::Registry>,
    plugins: Vec<std::sync::Arc<pd_filter::plugin::adapter::PluginSource>>,
    plugin_errors: Vec<(String, String)>,
    packs_stale: bool,
    lib_gen_seen: u64, lib_gen_at: Option<Instant>,   // a store sync's writes last shown (`sync::library_gen`), so its games appear while it still runs
    art_seen: usize, art_seen_at: Option<Instant>,   // an Art job's progress last acted on, so covers show up one by one as they are downloaded   // a job changed cover files: have the packs re-read them
    base: Option<((usize, usize, (usize, usize)), Vec<u32>)>,   // background + panels + charts, reused while only covers change
}

/// The built-in sources plus every Zest plugin in `<data>/plugins/` (PlayDate-only plugins there are skipped without a word; a plugin that cannot be read is listed
/// in the Plugins dialog). A plugin cannot take over `steam` or `custom`.
fn load_sources(root: &std::path::Path) -> (std::sync::Arc<pd_filter::sources::Registry>, Vec<std::sync::Arc<pd_filter::plugin::adapter::PluginSource>>, Vec<(String, String)>) {
    let mut reg = pd_filter::sources::Registry::builtin();
    let db = pd_filter::setup::find_library(root).unwrap_or_else(|| root.join("games.db"));
    reg.register_builtins(&pd_filter::sources::SourceEnv { http: std::sync::Arc::new(pd_filter::net::UreqHttp::new()), config_dir: root.to_path_buf(), db: db.clone() });
    let (manifests, mut errors) = pd_filter::plugin::manifest::discover(&root.join("plugins"));
    let mut plugins = vec![];
    for m in manifests {
        let (id, platform) = (m.id.clone(), m.platform.clone());
        let p = std::sync::Arc::new(pd_filter::plugin::adapter::PluginSource::new(m, root.to_path_buf(), db.clone()));
        if platform == "custom" { errors.push((id, "the platform custom is Zest's own".into())); continue; }
        match reg.register(p.clone()) { Ok(()) => plugins.push(p), Err(e) => errors.push((id, e)) }
    }
    (std::sync::Arc::new(reg), plugins, errors)
}

/// Two random symbols, cycled while a random-order library scrolls (random order has no position to show).
fn scroll_glyphs() -> String {
    const G: &[char] = &['!', '@', '#', '$', '%', '^', '&', '*', '?', '<', '>', '{', '}', '[', ']', '~', '+', '=', '0', '1', 'X', 'Y', 'Z'];
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos() as usize);
    format!("{}{}", G[t % G.len()], G[(t / 7) % G.len()])
}

/// What the Library's fast-scroll preview says for the game at the top, by the saved sort (`_scrollPreviewText` in library.js).
fn scroll_preview_text(db: Option<&std::path::Path>, sort: &str, appid: i64, name: &str) -> String {
    if sort == "name" { let ch = name.trim().chars().next().map(|c| c.to_uppercase().next().unwrap_or(c)).unwrap_or('#'); return if ch.is_ascii_alphanumeric() { ch.to_string() } else { "#".into() }; }
    let cols: &[&str] = match sort {
        "release_date" | "last_played" | "date_added" | "playtime_forever" | "review_percentage" | "weighted_percentage" | "total_reviews" => &[match sort { "release_date" => "release_date", "last_played" => "last_played", "date_added" => "date_added", "playtime_forever" => "playtime_forever", "review_percentage" => "review_percentage", "weighted_percentage" => "weighted_percentage", _ => "total_reviews" }],
        "hltb_main" | "hltb_extras" | "hltb_completionist" => &[match sort { "hltb_main" => "hltb_main", "hltb_extras" => "hltb_extras", _ => "hltb_completionist" }],
        "hltb_min" | "hltb_max" => &["hltb_main", "hltb_extras", "hltb_completionist"],
        "achievement_percent" | "achievement_remaining" => &["unlocked_achievements", "total_achievements"],
        _ => return String::new(),
    };
    let Some(db) = db else { return String::new() };
    let Ok(c) = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { return String::new() };
    let _ = c.busy_timeout(std::time::Duration::from_millis(200));
    let vals: Vec<f64> = c.query_row(&format!("SELECT {} FROM games WHERE appid = ?", cols.iter().map(|k| format!("COALESCE({k}, 0)")).collect::<Vec<_>>().join(", ")), [appid], |r| (0..cols.len()).map(|i| r.get::<_, f64>(i)).collect()).unwrap_or_default();
    if vals.is_empty() { return String::new(); }
    scroll_preview_value(sort, &vals)
}

/// The label for a sort and the game's raw column values (the columns `scroll_preview_text` selects, a missing one as 0).
fn scroll_preview_value(sort: &str, v: &[f64]) -> String {
    const MON: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    match sort {
        "release_date" | "last_played" | "date_added" => if v[0] <= 0.0 { "\u{2014}".into() } else { let (y, m, _, _) = pd_filter::pagywosg_filter::civil(v[0] as i64); format!("{} {y}", MON[(m.clamp(1, 12) - 1) as usize]) },
        "playtime_forever" => home::fmt_hours(v[0]),
        "hltb_main" | "hltb_extras" | "hltb_completionist" => if v[0] > 0.0 { home::fmt_hours(v[0]) } else { "\u{2014}".into() },
        "hltb_min" | "hltb_max" => { let real: Vec<f64> = v.iter().copied().filter(|x| *x > 0.0).collect(); if real.is_empty() { "\u{2014}".into() } else { home::fmt_hours(if sort == "hltb_max" { real.iter().cloned().fold(f64::MIN, f64::max) } else { real.iter().cloned().fold(f64::MAX, f64::min) }) } }
        "review_percentage" | "weighted_percentage" => if v[0] > 0.0 { format!("{}%", v[0].round()) } else { "\u{2014}".into() },
        "achievement_percent" | "achievement_remaining" => if v[1] > 0.0 { if sort == "achievement_percent" { format!("{}%", (100.0 * v[0] / v[1]).round()) } else { format!("{} left", (v[1] - v[0]) as i64) } } else { "\u{2014}".into() },
        "total_reviews" => if v[0] >= 1000.0 { format!("{:.1}k", v[0] / 1000.0) } else { format!("{}", v[0] as i64) },
        _ => String::new(),
    }
}

fn plat_label_of(p: &str) -> String { pd_filter::sources::platform_label(p) }

enum ArtAct { Order, FromSteam, Url(String) }


/// Open the file manager with `path` selected (PlayDate's `reveal_in_file_manager`): Windows `explorer /select,`, macOS `open -R`, Linux the
/// FileManager1 `ShowItems` D-Bus call (run in a thread, 5 s at most), and the containing folder when no file manager answers it.
fn reveal_file(path: &std::path::Path) {
    let path = path.to_path_buf();
    std::thread::spawn(move || {
        use std::process::{Command, Stdio};
        let quiet = |c: &mut Command| { c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()); };
        #[cfg(target_os = "windows")] { let mut c = Command::new("explorer"); c.arg(format!("/select,{}", path.display())); quiet(&mut c); let _ = c.spawn(); return; }
        #[cfg(target_os = "macos")] { let mut c = Command::new("open"); c.arg("-R").arg(&path); quiet(&mut c); let _ = c.spawn(); return; }
        #[cfg(all(unix, not(target_os = "macos")))] {
            let uri = format!("file://{}", path.to_string_lossy().split('/').map(pd_filter::net::enc).collect::<Vec<_>>().join("/"));
            let mut c = Command::new("dbus-send");
            c.args(["--session", "--print-reply", "--dest=org.freedesktop.FileManager1", "/org/freedesktop/FileManager1", "org.freedesktop.FileManager1.ShowItems"]).arg(format!("array:string:{uri}")).arg("string:");
            quiet(&mut c);
            if let Ok(mut child) = c.spawn() {
                let t = Instant::now();
                while t.elapsed() < Duration::from_secs(5) {
                    match child.try_wait() { Ok(Some(st)) if st.success() => return, Ok(Some(_)) => break, Ok(None) => std::thread::sleep(Duration::from_millis(50)), Err(_) => break }
                }
                let _ = child.kill();
            }
            if let Some(dir) = path.parent() { open_url(&dir.to_string_lossy()); }
        }
    });
}

fn open_url(url: &str) {
    #[cfg(target_os = "linux")] let (prog, args): (&str, Vec<&str>) = ("xdg-open", vec![url]);
    #[cfg(target_os = "macos")] let (prog, args): (&str, Vec<&str>) = ("open", vec![url]);
    #[cfg(target_os = "windows")] let (prog, args): (&str, Vec<&str>) = ("cmd", vec!["/c", "start", "", url]);
    use pd_filter::proc::Hide;
    let _ = std::process::Command::new(prog).args(args).hidden().stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();   // `cmd /c start` on Windows: no console window of its own
}

/// Where a card's tooltip goes (its top-left): below or above the card, else beside it, in the first place where it fits on the screen without covering the card. Above is tried
/// first for a card in the lower half. When nowhere fits, the old way: the preferred side, squeezed onto the screen.
/// A drop-down list opened while the pad was in use: the control it came from (focus goes back there when the list closes) and the list's layer once its first entry has the focus.
struct PadPopup { from: egui::Id, from_layer: egui::LayerId, layer: Option<egui::LayerId>, wait: u8 }

fn tip_place(card: egui::Rect, size: egui::Vec2, screen: egui::Rect) -> egui::Pos2 {
    let m = 8.0;
    // the Library grows a hovered card by 5% and draws a 3 px outline outside it, so the card on screen is bigger than the cell the tip was told about: keep clear of that one
    let card = if card.is_finite() { egui::Rect::from_center_size(card.center(), card.size() * 1.05).expand(3.0) } else { card };
    let inner = screen.shrink(4.0);
    let clamp = |p: egui::Pos2| egui::pos2(p.x.clamp(inner.min.x, (inner.max.x - size.x).max(inner.min.x)), p.y.clamp(inner.min.y, (inner.max.y - size.y).max(inner.min.y)));
    let below = egui::pos2(card.center().x - size.x / 2.0, card.max.y + m);
    let above = egui::pos2(card.center().x - size.x / 2.0, card.min.y - m - size.y);
    let right = egui::pos2(card.max.x + m, card.center().y - size.y / 2.0);
    let left = egui::pos2(card.min.x - m - size.x, card.center().y - size.y / 2.0);
    let lower_half = card.min.y > screen.center().y;
    let order = if lower_half { [above, below, right, left] } else { [below, above, right, left] };
    for c in order {
        let p = clamp(c);
        if !egui::Rect::from_min_size(p, size).intersects(card) { return p; }
    }
    clamp(order[0])
}

/// How long a top-bar message stays before it clears itself (a click still clears it at once).
const NOTICE_SHOWN: Duration = Duration::from_secs(8);

impl HomeState {
    /// What the top bar's status line shows: a running job (click cancels it), else the latest message (click clears it).
    fn status_line(&self) -> Option<(String, bool, String)> {
        match self.sync.as_ref().or(self.fill_art.as_ref()) {
            Some(j) => Some((j.text(), true, "Click to cancel".to_string())),
            // the browser run that reads Steam activation dates reports through the activity line, with no job behind it
            None if !pd_filter::net::activity().is_empty() => Some((pd_filter::net::activity(), false, "Reading Steam activation dates in your browser".to_string())),
            None => self.notices.last().filter(|(_, t)| t.elapsed() < NOTICE_SHOWN).map(|(m, _)| (m.clone(), false, format!("Recent messages (click to clear the latest):\n{}", self.notices.iter().rev().take(6).map(|(m, _)| m.as_str()).collect::<Vec<_>>().join("\n")))),
        }
    }

    /// Follow Steam's manifests: after the changes settle, re-read the installed games and refresh the cards.
    fn steam_tick(&mut self, ctx: &egui::Context) {
        let gone: Vec<i64> = UNINSTALLED.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default();
        for id in gone { let name = self.data.info(id).map(|g| g.name.clone()).unwrap_or_default(); self.data.clear_install(id); self.dirty = true; self.toast(format!("{name} is uninstalled")); }
        let mut got = false;
        // Steam installed (or its folder created) while Zest is open: look for it again every 30 s until it is found, then watch it like any other
        if self.steam_watch.is_none() {
            static LAST: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);
            let due = { let mut l = LAST.lock().unwrap_or_else(|e| e.into_inner()); if l.map_or(true, |t| t.elapsed() >= Duration::from_secs(30)) { *l = Some(Instant::now()); true } else { false } };
            if due {
                if let Some(root) = self.data.steam_root() {
                    self.steam_watch = start_steam_watch(pd_filter::steamlocal::library_paths(&root), ctx.clone());
                    if self.steam_watch.is_some() { pd_filter::net::log("steam: found the Steam folder after startup; watching it now"); got = true; }
                }
            }
            ctx.request_repaint_after(Duration::from_secs(30));
        }
        if let Some(w) = &self.steam_watch { while w.rx.try_recv().is_ok() { got = true; } }
        if got { self.steam_pending = Some(Instant::now()); }
        if let Some(t) = self.steam_pending {
            if t.elapsed() >= Duration::from_millis(700) {
                self.steam_pending = None;
                if self.data.sync_installed_from_steam().is_some() { self.dirty = true; }
            } else { ctx.request_repaint_after(Duration::from_millis(200)); }
        }
        self.install_tick(ctx);
    }

    /// Follow the other stores' installs: watch the folders each source names, and scan a source once its folders have been quiet for a moment.
    fn install_tick(&mut self, ctx: &egui::Context) {
        let Some(w) = self.install_watch.as_mut() else { return };
        if w.due_for_refresh() {
            let mut specs = vec![];
            for s in self.sources.all() { if s.id() != "steam" && s.caps().installed_scan { for (p, deep) in s.watch_paths() { specs.push((s.id().to_string(), p, deep)); } } }
            // a folder that does not exist yet (the first game is not installed) is looked for again later
            if w.refresh(&specs) > 0 { ctx.request_repaint_after(install_watch::RETRY); }
        }
        let due = w.poll();
        if w.has_pending() { ctx.request_repaint_after(Duration::from_millis(500)); }
        for id in due {
            let Some(src) = self.sources.all().iter().find(|s| s.id() == id).cloned() else { continue };
            if let Some(rows) = src.installed_scan() { if self.data.apply_installed(src.id(), &rows) > 0 { pd_filter::net::log(&format!("install watch: {id} changed")); self.dirty = true; } }
        }
    }

    /// Pointer events -> the click engine (a click is a press and a release on the same card). `hit` says which card is under a point.
    fn feed(&mut self, events: &[egui::Event], hit: &dyn Fn(f32, f32) -> Option<i64>, now: Instant) -> Vec<card_actions::Fired> {
        let hs = self;
        let mut fired = Vec::new();
        for ev in events {
            match ev {
                egui::Event::PointerMoved(p) => hs.pointer = Some(*p),
                egui::Event::PointerGone => hs.pointer = None,
                egui::Event::Key { key: egui::Key::Escape, pressed: true, .. } => { hs.menu = None; hs.tip_pin = None; }
                egui::Event::PointerButton { pos, button, pressed, .. } => {
                    hs.pointer = Some(*pos);
                    if *pressed { hs.tip_pin = None; }
                    if hs.menu.is_some() {
                        if *pressed && !hs.menu_rect.is_some_and(|r| r.contains(*pos)) { hs.menu = None; }
                        continue;
                    }
                    use card_actions::Button as B;
                    let b = match button { egui::PointerButton::Primary => B::Left, egui::PointerButton::Middle => B::Middle, egui::PointerButton::Secondary => B::Right,
                        egui::PointerButton::Extra1 => B::Back, egui::PointerButton::Extra2 => B::Forward };
                    let over = hit(pos.x, pos.y);
                    if *pressed { hs.press = over.map(|c| (b, c)); }
                    else if let (Some((pb, pc)), Some(oc)) = (hs.press.take(), over) { if pb == b && pc == oc { fired.extend(hs.engine.click(b, oc, (pos.x, pos.y), now)); } }
                }
                _ => {}
            }
        }
        fired.extend(hs.engine.tick(now));
        fired
    }

    /// Carry out what the frame collected: fired rules, a context-menu pick, an answer to the delete dialog.
    fn finish(&mut self, fired: Vec<card_actions::Fired>, pick: Option<(String, i64)>, answer: Option<(i64, bool)>) {
        let hs = self;
        if let Some(m) = hs.pick.toast.take() { hs.toast(m); }
        for f in fired { hs.run_action(&f.action, f.appid, f.pos); }
        if let Some((key, appid)) = pick { hs.menu = None; hs.run_action(&key, appid, (0.0, 0.0)); }
        if let Some((appid, blacklist)) = answer {
            hs.confirm = None;
            let name = hs.data.info(appid).map(|g| g.name.clone()).unwrap_or_default();
            hs.pick.results.retain(|c| c.appid != appid);
            match hs.data.delete_game(appid, blacklist) {
                Ok(()) => { hs.dirty = true; hs.data.library_changed(); hs.toast(format!("Deleted {name}{}", if blacklist { " and blacklisted it" } else { "" })) }
                Err(e) => hs.toast(format!("Could not delete: {e}")),
            }
        }
    }

    /// The Edit dialog was saved: the pictures its art tabs staged go into the library, the grid and the shelves show them, and the database follows.
    fn commit_art(&mut self, appid: i64) {
        let root = self.data.root_dir();
        if artstore::staged(&root, appid).is_empty() { return; }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        let (y, m, d, _) = pd_filter::pagywosg_filter::civil(now);
        match artstore::commit(&root, &self.data.db_path(), appid, &format!("{y:04}-{m:02}-{d:02}")) {
            Ok(done) => {
                for st in done.iter().filter(|s| s.cleared) { let _ = self.data.clear_art(appid, st.kind.name()); }
                self.packs_stale = true; self.covers.invalidate(appid); self.dirty = true;
            }
            Err(e) => self.toast(e),
        }
    }

    /// An art operation for one game from the Edit dialog's art tabs.
    fn art_op(&mut self, ctx: &egui::Context, appid: i64, k: usize, act: ArtAct, label: &str) {
        use pd_filter::artclient::Kind as AK;
        let kind = [AK::Vertical, AK::Horizontal, AK::Icon][k];
        let op = match act { ArtAct::Order => sync::ArtOp::Order(kind), ArtAct::FromSteam => sync::ArtOp::FromSteam(kind), ArtAct::Url(u) => sync::ArtOp::Url(kind, u) };
        self.start_sync_with(ctx, sync::Kind::ArtEdit, Some(op), None, vec![appid], label, Some(appid));
    }

    /// Start a Steam re-scrape job (one at a time).
    /// What the art job asks before each download: on screen first, then just off screen, then the rest of what the current view lists, then everything else, games hidden under a
    /// duplicate last (`pd_filter::artfocus`). The pages keep `art_focus` up to date while they are drawn.
    fn art_ranker(&self) -> std::sync::Arc<dyn Fn(&i64) -> u32 + Send + Sync> {
        let hidden: std::collections::HashSet<i64> = rusqlite::Connection::open_with_flags(self.data.db_path(), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()
            .and_then(|c| c.prepare("SELECT appid FROM games WHERE duplicate_of IS NOT NULL AND duplicate_of != ''").ok().and_then(|mut st| st.query_map([], |r| r.get::<_, i64>(0)).ok().map(|rows| rows.flatten().collect()))).unwrap_or_default();
        self.art_focus.ranker(hidden)
    }
    fn start_sync(&mut self, ctx: &egui::Context, kind: sync::Kind, appids: Vec<i64>, label: &str, target: Option<i64>) { self.start_sync_with(ctx, kind, None, None, appids, label, target) }
    fn start_sync_with(&mut self, ctx: &egui::Context, kind: sync::Kind, art_op: Option<sync::ArtOp>, plugin_platform: Option<String>, appids: Vec<i64>, label: &str, target: Option<i64>) { self.start_job(ctx, kind, art_op, plugin_platform, appids, label, target, Vec::new()); }
    /// The background half of a Steam sync (the store's owned-apps list and the dates from the licenses list) has finished: show what it changed, fill in the games it added, and read the
    /// Help pages for those it could not date.
    fn poll_late(&mut self) {
        let Some(late) = pd_filter::populate::take_late() else { return };
        self.dirty = true; self.data.library_changed();
        if !late.added.is_empty() {
            let n = late.added.len();
            self.toast(format!("Added {n} more game{} from your Steam account; fetching their details and covers", if n == 1 { "" } else { "s" }));
            if let Some(ctx) = self.companion_ctx.clone() {
                for (k, l) in [(sync::Kind::Art, "Art for the extra games"), (sync::Kind::Rescrape, "Filling in the extra games"), (sync::Kind::ProtonDb, "ProtonDB for the extra games"), (sync::Kind::Hltb, "HLTB for the extra games")] { self.job_queue.push((ctx.clone(), k, late.added.clone(), l.to_string())); }
            }
        }
        // one message for the whole dating step: what the licenses list dated, and what is left for the Help pages (their own summary follows when they finish)
        let (total, left) = (late.total, late.undated.len());
        if total > 0 {
            let got = total - left;
            self.toast(match (got, left) {
                (_, 0) => format!("Dated {} new game{} from your Steam licenses", total, if total == 1 { "" } else { "s" }),
                (0, _) => format!("Reading the purchase dates of {left} new game{} from the Steam Help pages", if left == 1 { "" } else { "s" }),
                _ => format!("Dated {got} of {total} new games from your Steam licenses; reading the other {left} from the Steam Help pages"),
            });
        }
        self.auto_dates(late.undated);
    }
    #[allow(clippy::too_many_arguments)]
    fn start_job(&mut self, ctx: &egui::Context, kind: sync::Kind, art_op: Option<sync::ArtOp>, plugin_platform: Option<String>, appids: Vec<i64>, label: &str, target: Option<i64>, pushed: Vec<pd_filter::importer::GameRecord>) {
        if kind == sync::Kind::ArtEdit { if self.art_job.is_some() { self.toast("An art change is already running"); return; } }
        else if kind == sync::Kind::Art && self.art_to_fill { if self.fill_art.is_some() { self.art_to_fill = false; return; } }
        else if self.sync.is_some() { self.toast("A sync is already running"); return; }
        let p = sync::Params { kind, meta: self.opt_meta, cheevo: self.opt_cheevo, art_kinds: self.opt_art_kinds.clone(), art_source: self.opt_art_source.to_string(), sources: self.sources.clone(), plugin_platform, art_op, sgdb_key: pd_filter::account::sgdb_key(&self.data.root_dir()), art_prefs: self.data.state_json().get("art_source_prefs").cloned().unwrap_or(serde_json::Value::Null), prefer_fit: self.data.state_bool("art_prefer_fit", true), hltb_threshold: self.data.state_f64("hltb_match_threshold", 99.0) as i64, db: self.data.db_path(), root: self.data.root_dir(), steam_root: self.data.steam_root(), half_trust: self.data.state_f64("review_half_trust_count", 10.0),
            promote: self.data.state_bool("auto_complete_on_100pct", true), downgrade: self.data.state_bool("auto_downgrade_completed", true), pushed,
            art_rank: if kind == sync::Kind::Art { Some(self.art_ranker()) } else { None } };
        // the games on screen are filled in first, then the ones just off screen, the filtered ones, the rest, and the hidden duplicates last (the order the cover job already uses)
        let mut appids = appids;
        if matches!(kind, sync::Kind::Rescrape | sync::Kind::Backfill) && appids.len() > 1 { let rank = self.art_ranker(); appids.sort_by_key(|a| rank(a)); }
        pd_filter::net::log(&format!("job start: {label} ({} game(s))", appids.len()));   // so the order the jobs ran in can be read from the log
        match sync::start(p, appids, label, target, ctx.clone()) { Ok(j) => if kind == sync::Kind::ArtEdit { self.art_job = Some(j) } else if kind == sync::Kind::Art && std::mem::take(&mut self.art_to_fill) { self.fill_art = Some(j) } else { self.sync = Some(j) }, Err(e) => { self.art_to_fill = false; self.toast(e) } }
    }

    /// Asks the user's browser (the Zest Companion userscript) to do `service`'s step: the listener starts on first use and the service's page opens with its address and token.
    fn companion_open(&mut self, ctx: &egui::Context, service: &str) { self.companion_open_at(ctx, service, None) }
    /// Asks the user's browser to open `base` (when given) with the service's address and token on it.
    fn ensure_steam_dates(&mut self) {
        if self.steam_dates.is_none() { self.steam_dates = pd_filter::account::load_account(&self.data.root_dir()).filter(|a| !a.steam_id.is_empty()).map(|a| Arc::new(pd_filter::steam_dates::SteamDates::new(self.data.db_path(), a.steam_id))); }
    }
    /// Reads the activation dates of these Steam games in the browser (the Zest Companion userscript, on the signed-in Steam Help site) and saves each as its Date Added.
    /// New Steam games: when Steam is already signed in (the sign-in window was used earlier in this run of Zest), read their real purchase dates in the background and set Date Added.
    /// Nothing opens by itself: with no sign-in this does nothing, and a big batch (a first sync) is left to Bulk Ops > Steam activation dates.
    fn auto_dates(&mut self, ids: Vec<i64>) {
        let ids: Vec<i64> = ids.into_iter().filter(|a| *a > 0).collect();   // non-Steam games have negative ids
        if ids.is_empty() { return; }
        let Some(acct) = pd_filter::account::load_account(&self.data.root_dir()).filter(|a| !a.steam_id.is_empty()) else { return };
        self.ensure_steam_dates();
        let in_memory = pd_filter::steamsignin::help_session().map_or(false, |c| pd_filter::steamsignin::steam_id_from_cookie(&c).as_deref() == Some(acct.steam_id.as_str()));
        if !in_memory && !self.steam_dates.as_ref().map_or(false, |sd| sd.can_read()) { return; }   // signed in now, or a saved sign-in to trade for a session
        if ids.len() > 100 { self.toast(format!("{} new games: use Bulk Ops > Steam activation dates to read their purchase dates", ids.len())); return; }
        self.dates_pending.extend(ids);
        self.start_pending_dates();
    }
    /// Starts the waiting purchase-date reads once no other date run is going.
    fn start_pending_dates(&mut self) {
        if self.dates_pending.is_empty() { return; }
        self.ensure_steam_dates();
        let Some(sd) = self.steam_dates.clone() else { self.dates_pending.clear(); return };
        if sd.progress().is_some() { return; }
        let ids: Vec<i64> = std::mem::take(&mut self.dates_pending);
        let games: Vec<(i64, String)> = ids.iter().filter_map(|id| self.data.info(*id).filter(|g| g.platform == "steam").map(|g| (g.appid, g.name.clone()))).collect();
        if games.is_empty() { return; }
        pd_filter::net::log(&format!("steam dates: reading the purchase dates of {} new game(s) with the Steam sign-in", games.len()));
        sd.set_plan(games);
        sd.run_direct(Arc::new(pd_filter::net::UreqHttp::new()), Duration::from_millis(600));
    }
    fn steam_dates_run(&mut self, ctx: &egui::Context, appids: &[i64], to_db: bool) {
        self.ensure_steam_dates();
        let games: Vec<(i64, String)> = appids.iter().filter_map(|id| self.data.info(*id).filter(|g| g.platform == "steam").map(|g| (g.appid, g.name.clone()))).collect();
        let (Some(sd), Some(first)) = (self.steam_dates.clone(), games.first().map(|g| g.0)) else { self.toast("No Steam games to read dates for (or no Steam account is set up)"); return };
        let n = games.len();
        // with the sign-in window installed, Zest reads the Help pages itself using Steam's own sign-in (no browser script); without it, the Companion userscript does the reading
        if let (Some(helper), Some(acct)) = (pd_filter::weblogin::helper_path(), pd_filter::account::load_account(&self.data.root_dir()).filter(|a| !a.steam_id.is_empty())) {
            let (sd2, http) = (sd.clone(), Arc::new(pd_filter::net::UreqHttp::new()));
            std::thread::spawn(move || {
                let mine = |c: &str| pd_filter::steamsignin::steam_id_from_cookie(c).as_deref() == Some(acct.steam_id.as_str());
                if !pd_filter::steamsignin::help_session().map_or(false, |c| mine(&c)) {
                    pd_filter::steamsignin::clear_help_session();
                    // a sign-in saved earlier (keyring) comes first, then the last sign-in's community session; a window only when neither works
                    pd_filter::steamsession::restore(&*http, &pd_filter::steamsession::SecretTool, &acct.steam_id);
                }
                if !pd_filter::steamsignin::help_session().map_or(false, |c| mine(&c)) {
                    // the last sign-in's community session may be good on the Help site too: try that before asking for another sign-in
                    if pd_filter::steamsignin::community_session().map_or(false, |c| mine(&c)) { pd_filter::steamsignin::adopt_community_for_help(&*http); }
                }
                if !pd_filter::steamsignin::help_session().map_or(false, |c| mine(&c)) {
                    match pd_filter::steamsignin::sign_in(&helper, &*http, pd_filter::steamsignin::Purpose::Dates) {
                        Ok((id, _)) if id == acct.steam_id => {}
                        Ok((id, _)) => { pd_filter::steamsignin::clear_help_session(); sd2.fail(&format!("Steam is signed in as {id} but Zest is set up for {}. Nothing was imported.", acct.steam_id)); return }
                        Err(e) => { sd2.fail(&e); return }
                    }
                }
                if to_db { sd2.set_plan(games); } else { sd2.set_plan_for_dialog(games); }
                sd2.run_direct(http, Duration::from_millis(600));
            });
            self.toast(if pd_filter::steamsignin::help_session().is_some() { format!("Reading {n} game{} on Steam's Help site; the dates are saved as they are found", if n == 1 { "" } else { "s" }) } else { "A Steam sign-in window opened. Sign in there and the dates are read afterwards.".to_string() });
            self.dirty = true;
            return;
        }
        if to_db { sd.set_plan(games); } else { sd.set_plan_for_dialog(games); }
        self.companion_open_at(ctx, "steam-dates", Some(&format!("https://help.steampowered.com/en/wizard/HelpWithGame/?appid={first}")));
        self.toast(format!("Reading {n} game{} on Steam's Help site; the dates are saved as the browser finds them", if n == 1 { "" } else { "s" }));
        self.dirty = true;
    }
    fn companion_open_at(&mut self, ctx: &egui::Context, service: &str, base: Option<&str>) {
        self.ensure_steam_dates();
        if self.companion.is_none() {
            let q = self.companion_events.clone(); let c2 = ctx.clone();
            let on_event: Arc<dyn Fn(pd_filter::companion::Event) + Send + Sync> = Arc::new(move |e| { if let Ok(mut v) = q.lock() { v.push(e); } c2.request_repaint(); });
            // the SteamGifts wins import needs the library and the account
            let acct = pd_filter::account::load_account(&self.data.root_dir()).filter(|a| !a.steam_id.is_empty());
            let sg_runner: Option<Arc<dyn pd_filter::companion::SteamGiftsHandler>> = acct.clone().map(|a|
                Arc::new(pd_filter::steamgifts_runner::Runner::new(self.data.root_dir(), self.data.db_path(), a.steam_id, Arc::new(pd_filter::net::UreqHttp::new()), Duration::from_secs(1), on_event.clone())) as Arc<dyn pd_filter::companion::SteamGiftsHandler>);
            let sg: Option<Arc<dyn pd_filter::companion::SteamGiftsHandler>> = Some(Arc::new(CompanionHandlers { sg: sg_runner, steam: self.steam_dates.clone() }));
            match pd_filter::companion::Listener::start_with(self.sources.clone(), on_event, sg) {
                Ok(l) => { self.companion = Some(l); self.companion_ctx = Some(ctx.clone()); }
                Err(e) => { pd_filter::net::log(&format!("companion: could not start: {e}")); self.toast("Could not start the listener the browser script talks to (see the log)"); return; }
            }
        }
        // a sign-in that ends on a page carrying the code starts from the source's own sign-in address (it holds this attempt's secrets)
        let url = self.companion.as_ref().and_then(|l| base.and_then(|b| l.arm_url_for(service, b)).or_else(|| l.arm_url(service)).or_else(|| {
            let src = pd_filter::companion::platform_of(service).and_then(|p| self.sources.get(p))?;
            let begin = src.auth_begin().ok()?;
            l.arm_url_for(service, &begin.url)
        }));
        match url {
            Some(u) => { open_url(&u); self.toast("Opened in your browser: the Zest Companion userscript takes it from there (it needs Tampermonkey)"); }
            None => self.toast("That step is not available"),
        }
    }
    /// What the browser script reported: sign-ins and purchase dates are told in the status line; library data is saved as a job (one at a time, so it waits for a running one).
    fn companion_tick(&mut self) {
        self.poll_install_options();
        let late: Vec<String> = self.late_notices.lock().map(|mut v| std::mem::take(&mut *v)).unwrap_or_default();
        for m in late { self.toast(m); }
        // new dates change what Home's date-sorted shelves show, not just the Library's order: the shelves are asked again too
        if let Some(m) = self.steam_dates.as_ref().and_then(|sd| sd.tick()) { self.toast(m); self.dirty = true; self.data.library_changed(); }
        if !self.dates_pending.is_empty() { self.start_pending_dates(); }
        // once, early: playtime and achievements from Steam's own files, off the UI thread (PlayDate's `sync_recent_playtime`)
        if !self.playtime_started {
            self.playtime_started = true;
            if no_sync() {} else if let (Some(acct), Some(steam_root)) = (pd_filter::account::load_account(&self.data.root_dir()).filter(|a| !a.steam_id.is_empty()), self.data.steam_root()) {
                let (db, state) = (self.data.db_path(), self.data.state_json().clone());
                let flag = |k: &str| state.get(k).and_then(|v| v.as_bool()).unwrap_or(true);
                let (promote, p100, down, half) = (flag("auto_promote_unfinished"), flag("auto_complete_on_100pct"), flag("auto_downgrade_completed"), state.get("review_half_trust_count").and_then(|v| v.as_f64()).unwrap_or(10.0));
                let (tx, rx) = std::sync::mpsc::channel();
                self.playtime_rx = Some(rx);
                let wake = self.companion_ctx.clone();
                std::thread::spawn(move || {
                    let line = (|| -> Result<(String, bool), String> {
                        let local = pd_filter::steamlocal::local_library(&steam_root, Some(&acct.steam_id));
                        if local.is_empty() { return Ok((String::new(), false)); }
                        let conn = rusqlite::Connection::open(&db).map_err(|e| e.to_string())?;
                        conn.busy_timeout(std::time::Duration::from_secs(10)).ok();
                        let o = pd_filter::playtime_sync::sync(&conn, &local, &steam_root, pd_filter::steamstats::account32(&acct.steam_id), promote).map_err(|e| e.to_string())?;
                        // games Steam has no stats file for: ask the web API, a few at a time, only with a key
                        let mut web = 0;
                        if !acct.api_key.trim().is_empty() && !o.need_web.is_empty() {
                            let steam = pd_filter::steamclient::Steam::new(std::sync::Arc::new(pd_filter::net::UreqHttp::new()), Some(pd_filter::steamclient::Creds { api_key: acct.api_key.clone(), steam_id: acct.steam_id.clone() }), half);
                            for a in o.need_web.iter().take(60) {
                                if let Ok(Some(c)) = steam.cheevos(*a) { let _ = conn.execute("UPDATE games SET unlocked_achievements = ?, total_achievements = ? WHERE appid = ?", rusqlite::params![c.unlocked, c.total, a]); web += 1; }
                                std::thread::sleep(std::time::Duration::from_millis(500));
                            }
                        }
                        let (up, dn) = pd_filter::jobs::sweep_achievement_status(&conn, p100, down).unwrap_or((0, 0));
                        Ok((format!("playtime sync: {} games read, {} played since last time ({} counts from Steam's files, {} from the web, {} without a source), {} promoted to Unfinished; achievements set {up} Completed, {dn} back to Beaten",
                            o.updated, o.changed, o.from_files, web, o.need_web.len().saturating_sub(web), o.promoted), o.changed > 0 || up + dn > 0))
                    })();
                    let _ = tx.send(line.unwrap_or_else(|e| (format!("playtime sync failed: {e}"), false)));
                    if let Some(c) = &wake { c.request_repaint(); }
                });
            }
        }
        if let Some(rx) = &self.playtime_rx {
            if let Ok((line, changed)) = rx.try_recv() { self.playtime_rx = None; if !line.is_empty() { pd_filter::net::log(&line); } if changed { self.dirty = true; } }
        }
        // the running PAGYWOSG event, for the Edit dialog's qualifications (once at start, again after an hour; a failure just means no panel)
        if self.live_rx.is_none() && self.live_at.map_or(true, |t| t.elapsed() > Duration::from_secs(3600)) && self.playtime_started && !no_sync() {
            self.live_at = Some(Instant::now());
            let (db, root, user) = (self.data.db_path(), self.data.root_dir(), pd_filter::account::sg_username(&self.data.root_dir()));
            let group = match self.data.state_json().get("pagywosg_sg_group") { None => None, Some(serde_json::Value::Null) => Some(None), Some(v) => Some(v.as_str().map(String::from)) };
            let (tx, rx) = std::sync::mpsc::channel();
            self.live_rx = Some(rx);
            let wake = self.companion_ctx.clone();
            std::thread::spawn(move || { let _ = tx.send(pagywosg_dialog::live_spec(&db, &root, &user, group)); if let Some(c) = &wake { c.request_repaint(); } });
        }
        if let Some(rx) = &self.live_rx {
            if let Ok(res) = rx.try_recv() { self.live_rx = None; match res { Ok(s) => { self.data.set_pag_live(Some(s)); self.live_arrived = true; } Err(e) => pd_filter::net::log(&format!("PAGYWOSG event for the Edit dialog: {e}")) } }
        }
        // once, early: ask every source that can tell which of its games are installed (a launcher's records, a games folder)
        if !self.installed_scanned {
            self.installed_scanned = true;
            let mut changed = 0;
            for src in self.sources.all().to_vec() { if src.id() != "steam" && src.caps().installed_scan { if let Some(rows) = src.installed_scan() { changed += self.data.apply_installed(src.id(), &rows); } } }
            if changed > 0 { self.dirty = true; }
        }
        // a manual game's program can be renamed or deleted at any time and nothing watches for it: look every few seconds (a handful of files)
        if self.custom_checked.map_or(true, |t| t.elapsed() >= Duration::from_secs(5)) {
            self.custom_checked = Some(Instant::now());
            if self.data.sync_custom_installs() > 0 { self.dirty = true; }
            if let Some(c) = &self.companion_ctx { c.request_repaint_after(Duration::from_secs(5)); }
        }
        let events: Vec<pd_filter::companion::Event> = self.companion_events.lock().map(|mut v| std::mem::take(&mut *v)).unwrap_or_default();
        for e in events {
            use pd_filter::companion::Event as E;
            self.companion_gen += 1;   // the Plugins dialog asks every source's sign-in status again
            match e {
                E::Connected { platform, username } => { self.toast(format!("{} connected{}", self.sources.label(&platform), username.map(|u| format!(" as {u}")).unwrap_or_default())); self.dirty = true; }
                E::Refused { platform } => self.toast(format!("{} did not accept what the browser sent (see the browser tab)", self.sources.label(&platform))),
                E::Imported { platform, records } => self.pushes.push((platform, records)),
                E::SteamGifts { message, .. } => { self.toast(message); self.data.refresh_info(); self.dirty = true; }
                E::PurchaseDates { read, matched, set, .. } => { self.toast(format!("PlayStation purchase dates: {read} read, {matched} games matched, {set} dates set")); self.dirty = true; if set > 0 { self.data.library_changed(); } }
            }
        }
        if self.sync.is_none() && !self.pushes.is_empty() {
            if let Some(ctx) = self.companion_ctx.clone() {
                let (platform, records) = self.pushes.remove(0);
                let label = format!("Syncing {} (read in your browser)", self.sources.label(&platform));
                self.start_job(&ctx, sync::Kind::PluginPush, None, Some(platform), vec![0], &label, None, records);
            }
        }
    }

    /// Library syncs the user asked for at start (Plugins -> "Sync this library when Zest starts"): ten seconds after the window opens the enabled stores are checked for a
    /// sign-in off the UI thread (a store that is not signed in is skipped quietly), then they run one after another. The result of each is the store's last-sync line.
    fn startup_sync_tick(&mut self) {
        if no_sync() { return; }
        let t0 = *self.startup_at.get_or_insert_with(Instant::now);
        if !self.startup_checked {
            if t0.elapsed() < Duration::from_secs(pd_filter::startup_sync::DELAY_SECS) { if let Some(c) = &self.companion_ctx { c.request_repaint_after(Duration::from_secs(1)); } return; }
            self.startup_checked = true;
            // emulated games: find a moved emulator again and re-check that each ROM is still on disk (nothing else ever does; a restore from another machine stales every path)
            { let (root, db) = (self.data.root_dir(), self.data.db_path());
              std::thread::spawn(move || {
                  if pd_filter::emulators::resync_binaries(&root) { pd_filter::net::log("emulators: re-detected a program"); }
                  if let Ok(c) = rusqlite::Connection::open(&db) { let _ = c.busy_timeout(Duration::from_secs(5)); if let Ok(n) = pd_filter::emulators::sync_install_status(&c) { if n > 0 { pd_filter::net::log(&format!("emulators: {n} games changed install state")); } } }
              }); }
            // covers saved before their pixel size was recorded (Cover Quality filter)
            { let (root, db) = (self.data.root_dir(), self.data.db_path());
              std::thread::spawn(move || { let n = artstore::backfill_art_dims(&db, &root); if n > 0 { pd_filter::net::log(&format!("covers: recorded the size of {n}")); } let n = artstore::backfill_art_quality(&db, &root); if n > 0 { pd_filter::net::log(&format!("covers: recorded the JPEG quality of {n}")); } }); }
            let enabled = pd_filter::startup_sync::enabled(self.data.state_json());
            if !enabled.is_empty() {
                let (sources, ready, ctx) = (self.sources.clone(), self.startup_ready.clone(), self.companion_ctx.clone());
                pd_filter::net::log(&format!("startup sync: checking {}", enabled.join(", ")));
                std::thread::spawn(move || for p in enabled {
                    let Some(src) = sources.get(&p) else { pd_filter::net::log(&format!("startup sync: {p} skipped (no such source)")); continue };
                    let caps = src.caps();
                    let status = src.auth_status();
                    let connected = status.as_ref().map_or(false, |a| a.connected);
                    if caps.sync && caps.auth && connected { pd_filter::net::log(&format!("startup sync: {p} is signed in, queued (window context {})", ctx.is_some())); if let Ok(mut r) = ready.lock() { r.push(p); } if let Some(c) = &ctx { c.request_repaint(); } }
                    else { pd_filter::net::log(&format!("startup sync: {p} skipped (can sync {}, has sign-in {}, signed in {:?})", caps.sync, caps.auth, status.map(|a| a.connected))); }
                });
            }
        }
        if self.sync.is_some() {
            static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if self.startup_ready.lock().map_or(false, |r| !r.is_empty()) && !SAID.swap(true, std::sync::atomic::Ordering::Relaxed) { pd_filter::net::log("startup sync: waiting for the job that is running"); }
            return;
        }
        // without the window context nothing can be started: keep the queue for the next frame instead of dropping its first store
        if self.companion_ctx.is_none() { return; }
        let next: Option<String> = self.startup_ready.lock().ok().and_then(|mut r| if r.is_empty() { None } else { Some(r.remove(0)) });
        if let Some(p) = &next { pd_filter::net::log(&format!("startup sync: picked {p} (window context {})", self.companion_ctx.is_some())); }
        if let (Some(platform), Some(ctx)) = (next, self.companion_ctx.clone()) {
            pd_filter::net::log(&format!("startup sync: {platform}"));
            let label = format!("Syncing {}", self.sources.label(&platform));
            self.start_sync_with(&ctx, sync::Kind::PluginSync, None, Some(platform), vec![0], &label, None);
        }
    }

    /// Drain a running job; when it ends, refresh what it changed.
    fn sync_tick(&mut self) {
        self.companion_tick();
        self.startup_sync_tick();
        // once a day, quietly: is there a newer Zest? (a toast, nothing more; the menu's Check for Updates does the rest)
        if !self.update_started && self.companion_ctx.is_some() && self.data.state_bool("check_updates", true) {
            self.update_started = true;
            let last = self.data.state_f64("zest_update_checked_at", 0.0);
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
            if now - last >= 24.0 * 3600.0 {
                self.data.save_pref("zest_update_checked_at", serde_json::json!(now));
                let (beta, root, ctx) = (self.data.state_bool("beta_updates", pd_filter::updater::VERSION.contains('-')), self.data.root_dir(), self.companion_ctx.clone().unwrap());
                let (tx, rx) = mpsc::channel(); self.update_rx = Some(rx);
                std::thread::spawn(move || { let r = pd_filter::updater::check(&pd_filter::net::UreqHttp::new(), pd_filter::updater::REPOS, beta, &pd_filter::updater::failed_versions(&root)); let _ = tx.send(r); ctx.request_repaint(); });
            }
        }
        if let Some(rx) = &self.update_rx { if let Ok(r) = rx.try_recv() { self.update_rx = None; if let Ok(Some(u)) = r { self.toast(format!("Zest {} is available. Menu > Check for Updates.", u.version)); } } }
        if self.populate_pending && self.sync.is_none() { if let Some(ctx) = self.companion_ctx.clone() { self.populate_pending = false; self.start_job(&ctx, sync::Kind::Populate, None, None, vec![0], "Adding new games", None, Vec::new()); } }
        self.poll_late();
        // the Edit dialog's art change: done on its own, whatever else is running
        if let Some(j) = self.art_job.as_mut() {
            if let Some(c) = j.poll() {
                let target = j.target; self.art_job = None;
                self.packs_stale = true; self.covers.refresh_changed(); self.dirty = true;
                if let Some(t) = target { self.art_staged_for = Some(t); }
                self.toast(if c.done > 0 { "New art ready: SAVE keeps it, CLOSE drops it" } else { "No art found (the cover you had is kept)" });
            }
        }
        if let Some(j) = self.fill_art.as_mut() {
            let n = j.done + j.failed;
            if let Some(c) = j.poll() { self.fill_art = None; self.packs_stale = true; self.covers.refresh_changed(); self.dirty = true; self.toast(if c.done > 0 { "Art updated" } else { "No art found (the cover you had is kept)" }); }
            else if n != self.art_seen && self.art_seen_at.map_or(true, |t| t.elapsed() >= Duration::from_secs(2)) { self.art_seen = n; self.art_seen_at = Some(Instant::now()); self.packs_stale = true; self.covers.refresh_changed(); }
        }
        // covers of new games run beside the details being filled in: a queued cover job goes to its own slot at once, the job behind it starts in the main one
        if self.fill_art.is_none() && self.job_queue.first().map_or(false, |j| j.1 == sync::Kind::Art) { let (ctx, kind, ids, label) = self.job_queue.remove(0); self.art_to_fill = true; self.start_job(&ctx, kind, None, None, ids, &label, None, Vec::new()); }
        if self.sync.is_none() && !self.job_queue.is_empty() { let (ctx, kind, ids, label) = self.job_queue.remove(0); self.start_job(&ctx, kind, None, None, ids, &label, None, Vec::new()); }
        let Some(j) = self.sync.as_mut() else { return };
        let polled = j.poll();
        let (is_art, n_done) = (j.label.starts_with("Art"), j.done + j.failed);
        let Some(c) = polled else {
            // an Art job: every couple of seconds, show what has arrived so far (the packs and Home's covers are refreshed) instead of waiting for the end
            // a store sync: its games (then their details and covers) show up every couple of seconds as they are written, not only when the whole job ends
            let gen = sync::library_gen();
            if gen != self.lib_gen_seen && self.lib_gen_at.map_or(true, |t| t.elapsed() >= Duration::from_secs(2)) {
                self.lib_gen_seen = gen; self.lib_gen_at = Some(Instant::now());
                self.data.library_changed(); self.packs_stale = true; self.covers.refresh_changed(); self.dirty = true;
            }
            if is_art && n_done != self.art_seen && self.art_seen_at.map_or(true, |t| t.elapsed() >= Duration::from_secs(2)) {
                self.art_seen = n_done; self.art_seen_at = Some(Instant::now());
                self.packs_stale = true; self.covers.refresh_changed();
            }
            return;
        };
        let (target, label) = (j.target, j.label.clone()); self.sync = None;
        // games a store sync just added get an HLTB match too (the sync itself reads their details and covers, not their play times)
        let plugin_new = sync::take_plugin_new();
        if !plugin_new.is_empty() { if let Some(ctx) = self.companion_ctx.clone() { self.job_queue.push((ctx, sync::Kind::Hltb, plugin_new, "HLTB for the added games".to_string())); } }
        self.sync_status = pd_filter::status::load(&self.data.root_dir());
        self.data.library_changed();   // a job may have added or removed games: the shelves and pie charts follow
        if label.starts_with("Adding new games") {
            self.dirty = true; self.packs_stale = true;
            let note = pd_filter::net::take_note();
            if c.failed > 0 { self.toast(if note.is_empty() { "Could not add your games".to_string() } else { note }); return; }
            let ids = self.data.unfetched_steam_ids();   // the jobs below also finish any game whose details were never filled in
            let added = pd_filter::populate::take_added();   // what this sync really added: the message and the purchase dates are about these only
            // with a store session the sync's background half dates the new games from the licenses list itself (see `poll_late`); only without one are the Help pages read here
            if !pd_filter::populate::take_background_started() { self.auto_dates(added.clone()); }
            self.toast(if !added.is_empty() { format!("Added {} game{}; fetching their details, reviews and covers", added.len(), if added.len() == 1 { "" } else { "s" }) } else if ids.is_empty() { "Your library is up to date".to_string() } else { format!("No new games; filling in {} that were never completed", ids.len()) });
            if let Some(ctx) = self.companion_ctx.clone() {
                if !ids.is_empty() {
                    for (k, l) in [(sync::Kind::Art, "Art for new games"), (sync::Kind::Rescrape, "Filling in new games"), (sync::Kind::ProtonDb, "ProtonDB for new games"), (sync::Kind::Hltb, "HLTB for new games")] { self.job_queue.push((ctx.clone(), k, ids.clone(), l.to_string())); }
                }
            }
            return;
        }
        // a Nexus sync says what Nexus listed and what changed (it used to end on a bare "Synced 1 game")
        if label == "Reading your owned Xbox games" { let n = pd_filter::net::take_note(); self.toast(if n.is_empty() { if c.failed > 0 { "Nexus sync failed (see the log)".to_string() } else { "Nexus sync: done".to_string() } } else { n }); return; }
        if label.starts_with("Installing ") {
            self.dirty = true; let n = pd_filter::net::take_note(); self.toast(if n.is_empty() { format!("{label}: done") } else { n });
            self.install_target = None;
            return;
        }
        if label.starts_with("Syncing ") && label != "Syncing" { self.dirty = true; let note = pd_filter::net::take_note(); let tail = if note.is_empty() { String::new() } else { format!("; {note}") }; self.toast(if c.failed > 0 { format!("{label}: it did not finish (see the log)") } else { format!("{label}: {} games added or updated{tail}", c.done) }); return; }
        let _ = self.data.recalculate_tag_similarity();
        self.data.dup_examine_new();
        self.packs_stale = true;
        self.covers.refresh_changed();   // a job may have changed covers: the ones whose file changed are read again (and keep showing the old picture until then)
        self.dirty = true;
        if let Some(t) = target { if label.starts_with("Art") { self.art_staged_for = Some(t); } else { self.open_edit = Some(t); } }   // an art operation keeps the open dialog (and what was typed in it) and only shows the new picture
        if label.starts_with("Art") { self.toast(if c.done > 0 { if target.is_some() { "New art ready: SAVE keeps it, CLOSE drops it" } else { "Art updated" } } else { "No art found (the cover you had is kept)" }); return; }
        self.toast(if c.cancelled { format!("Cancelled: {} done, {} failed", c.done, c.failed) } else if c.aborted { format!("Steam kept rate-limiting; stopped after {} done, {} failed", c.done, c.failed) }
            else if c.failed == 0 { format!("Synced {} game{}", c.done, if c.done == 1 { "" } else { "s" }) } else { format!("Synced {}, {} got no data", c.done, c.failed) });
    }

    /// Starts the install job for a game (one job at a time, as every job).
    fn begin_install(&mut self, appid: i64, platform: &str, name: &str) {
        let label = format!("Installing {name}");
        let Some(ctx) = self.companion_ctx.clone() else { return };
        self.install_target = Some(appid);
        self.start_job(&ctx, sync::Kind::PluginInstall, None, Some(platform.to_string()), vec![appid], &label, None, Vec::new());
    }
    /// The builds a worker found for "ask each time": more than one asks the person, one is taken, none starts the install as usual (the install says why when it cannot).
    fn poll_install_options(&mut self) {
        let got = self.install_opts_slot.lock().ok().and_then(|mut s| s.take());
        let Some((appid, platform, r)) = got else { return };
        // picked up after this frame was drawn: one more frame shows the answer (the picker or the install) without waiting for the mouse to move
        if let Some(c) = &self.companion_ctx { c.request_repaint(); }
        let name = self.data.info(appid).map(|g| g.name.clone()).unwrap_or_default();
        match r {
            Err(e) => self.toast(e),
            Ok(opts) if opts.len() > 1 => self.install_pick = Some((appid, name, opts)),
            Ok(opts) => { if let Some(o) = opts.first() { pd_filter::sources::set_install_choice(appid, &o.key); } self.begin_install(appid, &platform, &name); }
        }
    }

    fn toast(&mut self, msg: impl Into<String>) {
        let m: String = msg.into();
        self.notices.retain(|(t, _)| *t != m);
        self.notices.push((m, Instant::now()));
        if self.notices.len() > 5 { self.notices.remove(0); }
    }

    /// Carry out what a source asked for. `appid` is the game it was for, when there is one (an uninstall clears that game's install state).
    fn do_action(&mut self, a: pd_filter::sources::Action) { self.do_action_for(None, a); }
    fn do_action_for(&mut self, appid: Option<i64>, a: pd_filter::sources::Action) {
        use pd_filter::sources::Action as A;
        match a {
            A::OpenUrl(u) => open_url(&u),
            A::Spawn { program, args, dir, env } => {
                let sandboxed = pd_filter::sandbox::in_flatpak();
                let (prog_run, args_run) = pd_filter::sandbox::wrap(&program, &args, dir.as_deref(), &env, sandboxed);
                let mut cmd = std::process::Command::new(&prog_run);
                cmd.args(&args_run).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped());
                if !sandboxed {
                    if let Some(d) = &dir { cmd.current_dir(d); }
                    for (k, v) in &env { match v { Some(v) => { cmd.env(k, v); } None => { cmd.env_remove(k); } } }
                }
                match cmd.spawn() {
                    // a game that dies within a second with an error is told about (PlayDate's `check_launch`); one still running or finished cleanly is left alone
                    Ok(mut child) => { let q = self.late_notices.clone(); let wake = self.companion_ctx.clone(); let name = std::path::Path::new(&program).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or(program.clone());
                        std::thread::spawn(move || { std::thread::sleep(Duration::from_millis(1000));
                            match child.try_wait() {
                                Ok(Some(st)) => { if !st.success() {
                                    let mut err = String::new(); if let Some(mut e) = child.stderr.take() { use std::io::Read; let mut b = vec![0u8; 400]; let n = e.read(&mut b).unwrap_or(0); err = String::from_utf8_lossy(&b[..n]).trim().to_string(); }
                                    if let Ok(mut v) = q.lock() { v.push(format!("{name} exited at once (code {}){}", st.code().map_or("?".to_string(), |c| c.to_string()), if err.is_empty() { String::new() } else { format!(": {err}") })); } if let Some(c) = &wake { c.request_repaint(); } } }
                                // still running: its error output must keep being read until it ends. Dropping the pipe here made the program's next log line fail (SIGPIPE), and umu-run, which logs a lot, died a moment after Zest started it
                                Ok(None) => { if let Some(mut e) = child.stderr.take() { use std::io::Read; let mut b = [0u8; 4096]; while matches!(e.read(&mut b), Ok(n) if n > 0) {} } let _ = child.wait(); }
                                Err(_) => {}
                            } }); }
                    Err(e) => self.toast(format!("Could not start {program}: {e}")),
                }
            }
            A::Message(m) => self.toast(m),
            A::PickExecutable { dir, candidates } => {
                if let Some(id) = appid { let name = self.data.info(id).map(|g| g.name.clone()).unwrap_or_default(); let _ = dir; self.exe_pick = Some((id, name, true, candidates)); }
            }
            A::Many(list) => { for x in list { self.do_action_for(appid, x); } }
            A::RemoveInstall { dir, delete } => {
                let p = std::path::Path::new(&dir);
                // never a root, a home folder or anything with fewer than three parts; and only a real folder
                let safe = p.is_absolute() && p.components().count() >= 3 && std::env::var_os("HOME").map_or(true, |h| std::path::Path::new(&h) != p);
                if delete && p.is_dir() {
                    if !safe { self.toast("Zest will not delete that folder"); return; }
                    if let Err(e) = std::fs::remove_dir_all(p) { self.toast(format!("Could not delete {dir}: {e}")); return; }
                }
                if let Some(id) = appid { self.data.clear_install(id); self.dirty = true; }
                self.toast(if delete { "Uninstalled".to_string() } else { "Forgotten; the folder is outside the games folder, so its files were left".to_string() });
            }
        }
    }

    /// Do what a fired rule says. Anything that needs the backend (launch, edit, status, delete, uninstall) only shows a toast for now.
    /// The Edit dialog's store link for a game from another store: the source's own store page, when it has one for this game (the label is the store's name).
    fn store_link_text(&self, appid: i64) -> Option<String> {
        let gr = self.data.game_ref(appid)?;
        let src = self.sources.for_game(&gr.platform, &gr.platform_id)?;
        src.store_url(&gr).map(|_| format!("View on {} Store", src.label()))
    }

    fn run_action(&mut self, action: &str, appid: i64, pos: (f32, f32)) {
        let Some(g) = self.data.info(appid).cloned() else { return };
        match action {
            "context_menu" => { self.menu = Some((appid, egui::pos2(pos.0, pos.1))); self.menu_rect = None; }
            "launch" if g.platform == "custom" => {
                // no store or plugin stands behind these: run the program set in the Edit dialog (`system.py`'s custom branch)
                let (exe, args) = self.data.custom_launch(appid).unwrap_or_default();
                let exe = home::expand_home(exe.trim());
                if exe.is_empty() { self.toast("No program set for this game. Edit it to add one."); return; }
                let p = std::path::Path::new(&exe);
                let sandboxed = pd_filter::sandbox::in_flatpak();   // the program is on the host: it cannot be looked at from here, and is started there
                if !sandboxed && !p.is_file() && !(cfg!(target_os = "macos") && exe.ends_with(".app") && p.exists()) { self.toast("That program was not found; it may have moved. Edit the game to update it."); return; }
                let args = home::split_args(&args);
                let win_exe_on_linux = cfg!(target_os = "linux") && exe.to_lowercase().ends_with(".exe");
                let (prog, mut all): (String, Vec<String>) = if win_exe_on_linux { ("wine".into(), vec![exe.clone()]) }
                    else if cfg!(target_os = "macos") && exe.ends_with(".app") { ("open".into(), { let mut v = vec![exe.clone()]; if !args.is_empty() { v.push("--args".into()); } v }) }
                    else { (exe.clone(), vec![]) };
                all.extend(args.iter().cloned());
                let dir = p.parent().map(|d| d.to_string_lossy().to_string()).filter(|d| !d.is_empty());
                let (prog, all) = pd_filter::sandbox::wrap(&prog, &all, dir.as_deref(), &[], sandboxed);
                let mut cmd = std::process::Command::new(prog); cmd.args(all);
                if !sandboxed { if let Some(d) = &dir { cmd.current_dir(d); } }
                match cmd.spawn() {
                    Ok(_) => { pd_filter::net::log(&format!("Launched manual game {} ({})", g.name, exe)); if self.data.record_launch(appid) { self.dirty = true; } }
                    Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => self.toast("That file is not marked executable; check its permissions."),
                    Err(e) if win_exe_on_linux && e.kind() == std::io::ErrorKind::NotFound => self.toast("This is a Windows program: install Wine to run it."),
                    Err(e) => { pd_filter::net::log(&format!("Could not launch {exe}: {e}")); self.toast("Could not launch this game (see the log)"); }
                }
            }
            "store" | "achievements" | "community_hub" | "uninstall" | "launch" if self.data.game_ref(appid).map_or(true, |gr| self.sources.for_game(&gr.platform, &gr.platform_id).is_none() && pd_filter::sources::origin(&gr.platform, &gr.platform_id) != pd_filter::sources::Origin::Emulated) => self.toast(format!("{} games need their source, which is not built yet", plat_label_of(&g.platform))),
            "store" | "achievements" | "community_hub" => {
                let Some(gr) = self.data.game_ref(appid) else { return };
                let Some(src) = self.sources.for_game(&gr.platform, &gr.platform_id).cloned() else { return };
                let url = match action { "store" => src.store_url(&gr), "achievements" => src.achievements_url(&gr, self.data.account_id.as_deref()), _ => src.community_url(&gr) };
                match url { Some(u) => open_url(&u), None => self.toast(match action { "store" => format!("{} has no store page for this game", src.label()), "achievements" => format!("Achievements are not available for {} games", src.label()), _ => format!("Community Hub is not available for {} games", src.label()) }) }
            }
            "open_folder" => match self.data.install_path(appid) {
                Some(p) if std::path::Path::new(&p).exists() => open_url(&p),
                _ => self.toast("This game is not installed") },
            "launch" | "uninstall" => {
                let Some(gr) = self.data.game_ref(appid) else { return };
                // a game run in an emulator has no store source: the Emulators setup says how to start it (uninstalling one is not a thing, the ROM is the user's)
                if pd_filter::sources::origin(&gr.platform, &gr.platform_id) == pd_filter::sources::Origin::Emulated {
                    if action == "launch" {
                        match pd_filter::emulators::launch(&self.data.root_dir(), &gr.platform, &gr.platform_id) {
                            Ok(a) => { pd_filter::net::log(&format!("launch {} ({}): {:?}", gr.name, gr.platform, a)); self.do_action_for(Some(appid), a); if self.data.record_launch(appid) { self.dirty = true; } }
                            Err(e) => { pd_filter::net::log(&format!("launch {} ({}) failed: {e}", gr.name, gr.platform)); self.toast(e) } }
                    } else { self.toast("An emulated game is not installed by Zest; remove its file to uninstall it"); }
                    return;
                }
                let Some(src) = self.sources.for_game(&gr.platform, &gr.platform_id).cloned() else { return };
                // a game that is not on disk yet, from a store that can download it: the install runs as a job (progress in the status line), then Launch plays it
                if action == "launch" && !gr.installed && src.caps().install {
                    // a store with several builds of a game and "ask each time" chosen: find out what is on offer first, then ask (the answer comes back through `install_opts_slot`)
                    if src.platform_choice() {
                        if let Some(ctx) = self.companion_ctx.clone() {
                            self.toast(format!("Looking up the builds of {}...", gr.name));
                            sync::fetch_install_options(src.clone(), gr.clone(), self.data.root_dir(), self.data.db_path(), self.install_opts_slot.clone(), ctx);
                            return;
                        }
                    }
                    self.begin_install(appid, &gr.platform, &gr.name);
                } else if action == "launch" {
                    match src.launch(&gr) { Ok(l) => { self.do_action_for(Some(appid), l.action); if l.record && self.data.record_launch(appid) { self.dirty = true; } } Err(e) => self.toast(e) }
                } else { match src.uninstall(&gr) {
                    Ok(a) => {
                        // an uninstaller was started (Windows): the game leaves the installed shelves once its entry or its folder is gone
                        if matches!(a, pd_filter::sources::Action::Spawn { .. }) { let wake = self.companion_ctx.clone(); pd_filter::winuninstall::watch_gone(&gr, move || { if let Ok(mut q) = UNINSTALLED.lock() { q.push(appid); } if let Some(c) = wake { c.request_repaint(); } }); }
                        self.do_action_for(Some(appid), a)
                    }
                    Err(e) => self.toast(e) } }
            }
            "delete" => self.confirm = Some(appid),
            s if s.starts_with("status:") => match self.data.set_status(appid, &s["status:".len()..]) { Ok(()) => {}, Err(e) => self.toast(format!("Could not save: {e}")) },
            "edit" => self.open_edit = Some(appid),
            "tooltip" => { self.tip_pin = Some(appid); }
            _ => {}
        }
    }
}

/// `--no-sync`: no startup network or sync work (playtime, PAGYWOSG event, store syncs), so headless screenshot runs settle.
fn no_sync() -> bool { static F: std::sync::OnceLock<bool> = std::sync::OnceLock::new(); *F.get_or_init(|| std::env::args().any(|a| a == "--no-sync")) }
static SHOT_PAD_N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static SHOT_PAD: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());
/// Test hook `--script "click 640,300;type abc;key Enter;wait"`: mouse and keyboard actions for screenshot runs, one every fourth frame once `ZEST_PAD_WAIT_MS` has passed.
static SHOT_SCRIPT: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// The words a saved filter tree searches for (its top-level `name LIKE` condition), or nothing.
fn search_in_tree(tree: &serde_json::Value) -> String {
    tree.get("items").and_then(|v| v.as_array()).and_then(|a| a.iter().find(|it| it.get("type").and_then(|v| v.as_str()) == Some("condition")
        && it.get("column").and_then(|v| v.as_str()) == Some("name") && it.get("operator").and_then(|v| v.as_str()) == Some("LIKE")))
        .and_then(|it| it.get("value")).and_then(|v| v.as_str()).unwrap_or("").to_string()
}

/// The tree without its top-level search condition (`_strip_search_condition`): a lone nested group takes its place.
fn tree_without_search(tree: &serde_json::Value) -> serde_json::Value {
    let Some(items) = tree.get("items").and_then(|v| v.as_array()).filter(|a| !a.is_empty()) else { return tree.clone() };
    let kept: Vec<serde_json::Value> = items.iter().filter(|it| !(it.get("type").and_then(|v| v.as_str()) == Some("condition")
        && it.get("column").and_then(|v| v.as_str()) == Some("name") && it.get("operator").and_then(|v| v.as_str()) == Some("LIKE"))).cloned().collect();
    if kept.len() == 1 && kept[0].get("type").and_then(|v| v.as_str()) == Some("group") { return kept[0].clone(); }
    let mut out = tree.clone();
    out["items"] = serde_json::Value::Array(kept);
    out
}

/// The dice's pick: it scrolls to the card (a jump to two screens away, then a 4 s ease-out), and once a third of the card is on screen the card
/// flashes (`dice-flash` in the web page: five pulses over 3.5 s, then a long fade, 7 s in all).
struct Dice { appid: i64, glow_at: Option<Instant>, scroll: Option<(Instant, f32, f32)> }

/// The outline's strength `t` seconds into the flash, or None when it is over. Each of the first five 0.7 s steps fades from full to nothing
/// (the CSS `ease-out` applies per keyframe), then one fade over the last 3.5 s.
/// (bytes the program holds, free bytes malloc keeps, mmapped bytes), in MB: tells real use from allocator retention.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn heap_mb() -> (usize, usize, usize) {
    let i = unsafe { libc::mallinfo2() };
    (i.uordblks / 1048576, i.fordblks / 1048576, i.hblkhd / 1048576)
}
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn heap_mb() -> (usize, usize, usize) { (0, 0, 0) }

fn dice_alpha(t: f32) -> Option<f32> {
    if t >= 7.0 { return None; }
    let p = if t < 3.5 { (t % 0.7) / 0.7 } else { (t - 3.5) / 3.5 };
    Some(1.0 - (1.0 - (1.0 - p).powi(2)))
}

/// Play or Pay's group edits, carried out on the library.
struct PopGroups<'a>(&'a mut home::HomeData);
impl pd_filter::pop_sync::Groups for PopGroups<'_> {
    fn append(&mut self, tag: &str, appids: &[i64]) -> Result<(), String> { self.0.bulk_edit("groups", "append", tag, Some(appids)).map(|_| ()) }
    fn remove(&mut self, tag: &str, appids: &[i64]) -> Result<(), String> { self.0.bulk_edit("groups", "remove", tag, Some(appids)).map(|_| ()) }
    fn existing(&self, appids: &[i64]) -> std::collections::HashSet<i64> { self.0.existing_appids(appids) }
}

/// An open dialog. Only the Blacklist manager has real content so far; the rest say so.
enum Modal { SendLog(Box<sendlog_dialog::SendLogDialog>), Tutorial(Box<tutorial_dialog::TutorialDialog>), Update(Box<update_dialog::UpdateDialog>), Hltb(Box<hltb_dialog::HltbDialog>), Data(Box<data_dialog::DataDialog>), Playnite(Box<playnite_dialog::PlayniteDialog>), HomeEdit(Box<home_edit_ui::HomeEditor>), AddGame(Box<add_game_dialog::AddGameDialog>), Junk(Box<junk_dialog::JunkDialog>), Emulators(Box<emulators_dialog::EmulatorsDialog>), Community, Settings(Box<settings_dialog::SettingsDialog>), Plugins(Box<plugins_dialog::PluginsDialog>), Placeholder(&'static str), WhatsNew { sections: Vec<pd_filter::whats_new::Section> }, KeyringChoice, Blacklist { entries: Vec<home::BlEntry>, query: String }, Duplicates { items: Vec<(pd_filter::dupes::Suggestion, bool)> } }

/// What the Companion listener calls: the SteamGifts import and the Steam date import share its two route prefixes.
struct CompanionHandlers { sg: Option<Arc<dyn pd_filter::companion::SteamGiftsHandler>>, steam: Option<Arc<pd_filter::steam_dates::SteamDates>> }
impl pd_filter::companion::SteamGiftsHandler for CompanionHandlers {
    fn call(&self, route: &str, body: &serde_json::Value) -> Result<serde_json::Value, String> {
        if route.starts_with("steam/") { return self.steam.as_ref().ok_or_else(|| "the Steam date import is not available".to_string())?.call(route, body); }
        self.sg.as_ref().ok_or_else(|| "the SteamGifts import is not available".to_string())?.call(route, body)
    }
}

const MENU: [(&str, &str); 12] = [("add_game", "Add Game"), ("edit_home", "Edit Home Page"), ("duplicates", "Duplicates"), ("community", "Community Tools"), ("plugins", "Plugins"), ("data", "Data"), ("emulators", "Emulators"),
    ("blacklist", "Blacklist / Junk Finder"), ("hltb", "HLTB"), ("settings", "Settings"), ("tutorial", "Tutorial"), ("update", "Check for Updates")];

struct Auto { pass: usize, started: Instant, y: f32, last: Instant, fresh: bool, sharp: f64, any: f64, frames: u32 }

struct Own {
    window: Option<Rc<Window>>,
    surface: Option<softbuffer::Surface<winit::event_loop::OwnedDisplayHandle, Rc<Window>>>,
    ctx: egui::Context,
    egui_winit: Option<egui_winit::State>,
    renderer: EguiSoftwareRender,
    core: Core<Arc<Pixels>>,
    layer: Layer,
    cw: f32,   // the slider: target (minimum) card width
    lay: Lay,
    theme: theme::Theme,
    bg: theme::Background,
    page: Page,
    want_fullscreen: bool,
    shot: Option<(PathBuf, u32)>,
    shot_size: Option<(usize, usize)>,   // --screenshot renders at exactly this size, whatever size the compositor gives the window
    want_page: Option<Page>,
    home: Option<HomeState>,
    cjk_done: bool,
    win_maximized: bool,
    win_normal: (u32, u32),   // inner size when not maximized
    win_max: (u32, u32),      // inner size when maximized (used to prepare Home's covers before the window exists)
    want_exit: bool,
    menu_open: bool,
    edit: Option<edit_dialog::EditDialog>,
    filter: Option<filter_dialog::FilterDialog>,
    bulk: Option<bulk_dialog::BulkDialog>,
    crop: Option<crop_dialog::CropDialog>,
    picker: Option<sgdb_picker::SgdbPicker>,
    bulk_open: bool,
    pag: Option<pagywosg_dialog::PagDialog>,
    pag_open: bool,
    santa: Option<community_dialogs::SantaDialog>,
    miam: Option<community_dialogs::MiamDialog>,
    santa_open: bool,
    blaeo: Option<community_dialogs::BlaeoDialog>,
    whats_new_checked: bool,
    blaeo_parked: Option<community_dialogs::BlaeoDialog>,   // the BLAEO dialog was closed while it was still reading (or after it finished without being used): kept so the menu can say "results ready"
    blaeo_open: bool,
    sg_unmatched: Option<community_dialogs::SgUnmatchedDialog>,
    sg_unmatched_open: bool,
    /// Play or Pay sync: the thread fetching the plan, the plan waiting for the cleanup answer, the line under the button.
    pop_rx: Option<std::sync::mpsc::Receiver<Result<pd_filter::pop_sync::Plan, String>>>,
    pop_plan: Option<(pd_filter::pop_sync::Plan, Vec<String>)>,
    pop_status: Option<(String, bool)>,
    miam_open: bool,
    /// Headless check: save the loaded event as a filter, make it the active one and close the builder (`--pag-save`).
    pag_autosave: bool,
    bulk_tab_arg: Option<usize>,
    filter_open: bool,
    filter_demo: bool,
    crop_arg: Option<(i64, usize)>,   // --crop KIND with --edit: open the Crop dialog at start (for screenshots)
    edit_arg: Option<(i64, bool)>,   // --edit APPID [--edit-tab info]: open the dialog at start (for screenshots)
    menu_action: Option<&'static str>,
    modal: Option<Modal>,
    lib_header: String,
    lib_head: home::LibHead,
    lib_full: Option<(Vec<grid_core::Game>, Vec<home::GroupInfo>)>,   // the whole list before the live search narrows it
    search: String,
    search_applied: String,
    search_init: bool,
    search_edit_at: Option<Instant>,
    dice: Option<Dice>,
    dice_pick: bool,
    scroll_to: Option<f32>,
    want_clear: bool,
    want_show_all: bool,
    groups: Vec<home::GroupInfo>,
    collapsed: HashSet<String>,
    toggle: Option<usize>,
    view_open: bool,
    view_stage: (String, String, String, f32),   // what the View panel shows until Apply
    view_changes: Vec<(&'static str, serde_json::Value)>,
    tier_job: Option<(Arc<std::sync::atomic::AtomicBool>, u32)>,
    tier_pending: bool,
    pack_job: Option<grid_core::PackJob>,
    status: Option<(String, bool, String)>,   // the top bar's status line (text, is a running job, tooltip), prepared each frame from the home state
    status_clicked: bool,
    /// The update-rollback handshake was answered (the first page was drawn).
    healthy_marked: bool,
    selftest: Option<(u8, Instant)>,   // --selftest-clear: clear the first card's cover and report what the grid shows (a regression check, not a feature)
    packs_checked: bool,
    settle_until: Option<Instant>,   // after covers or the list changed behind the window's back: look again every 100 ms until then, so nothing waits for a mouse move

    applied_cw: f32,   // the card size last applied (the slider previews live; closing the panel without Apply goes back to this)
    root: PathBuf,
    view_vals: (String, String, String),   // sort, order, group_by as saved
    lib_loaded: bool,
    lib_warm: bool,
    lib_grid: Option<(egui::Rect, f32)>,
    lib_focus_sig: ((usize, i64, i64), (usize, i64)),   // what was last told to the art job: the list (length, first, last game) and the rows on screen
    lib_mouse_card: Option<i64>,   // the last Library card the mouse was over: where the pad starts when it takes over (PlayDate's `_lastMouseCard`)
    edit_corner: Option<String>,   // where the hover edit button sits (state.edit_button), None = off for this page
    edit_rect: Option<egui::Rect>, // last frame's edit button, so a click on it is not also a click on the card
    edit_click: Option<i64>,
    shot_select: Option<usize>,   // --select N: start in selection mode with the first N games chosen (headless renders)
    badges: std::sync::Arc<std::collections::HashMap<i64, home::BadgeData>>,
    badge_cfg: Option<std::sync::Arc<home::BadgeCfg>>,
    badge_tex: std::collections::HashMap<String, Option<egui::TextureHandle>>,
    tip_pin: Option<i64>,
    tip_target: Option<(i64, egui::Rect)>,   // the card the pointer is on this frame, set while drawing
    tip_cache: Option<(i64, home::EditInfo, [f64; 3])>,
    tip_quals: Option<(i64, Option<pagywosg_dialog::PagPanel>)>,
    tip_fields: Option<std::collections::HashSet<String>>,
    tip_since: Option<(i64, Instant)>,
    list_mode: bool,   // the Library's list view (state artwork_orientation = list), chosen at start
    list_sel: Option<i64>,
    list_saved: Option<i64>,   // the selection last written to state.json (`zest_list_selected`)
    list_menu: Option<(i64, egui::Pos2)>,   // right click on a list row: open the card context menu there
    list_center: bool,   // scroll the list so the selected row is centred (on restore and keyboard moves)
    list_cover: Option<(i64, egui::TextureHandle)>,
    list_icons: std::collections::HashMap<i64, Option<egui::TextureHandle>>,
    list_status: std::collections::HashMap<i64, String>,
    list_edit: Option<edit_dialog::EditDialog>,
    list_out: Option<edit_dialog::EditOut>,
    select_mode: bool,   // Bulk 'Pick games': a click on a card toggles it instead of acting
    selected: HashSet<i64>,
    sel_press: Option<i64>,
    outlines: std::sync::Arc<std::collections::HashMap<i64, (u8, u8, u8)>>,   // card outline colors by appid (state.card_outlines)
    shot_wheel: Option<f32>,   // --wheel N: N wheel notches (down is positive) sent once at the pointer, for headless renders
    shot_switch: Option<String>,   // --switch vertical|horizontal|list: change the view once after a few frames (headless check of the live switch)
    shot_rclick: Option<(f32, f32)>,   // --rclick X,Y: one right click, for headless renders
    shot_hover: Option<(f32, f32)>,   // --hover X,Y: a pointer position for headless renders
    rt: Option<ResizeTest>,
    rlog: ResizeLog,
    sim_w: Option<f32>,
    y: f32,
    last_y: f32,
    size: [f32; 2],
    auto: Option<Auto>,
    pass_limit: usize,
    next_wake: Option<Instant>,
    keep_drawing: bool,
    pad_cur: Option<i64>,   // the gamepad cursor's card
    lib_zoom: std::collections::HashMap<i64, (f32, egui::Rect)>,   // Library hover/gamepad zoom: 0..1 grow animation and the card's rectangle relative to the grid area
    lib_zoom_tex: std::collections::HashMap<i64, (egui::TextureHandle, u64)>,   // the card's pixels cut from the layer (and the cover epoch they were cut at)
    lib_zoom_at: Instant,
    lib_zoom_y: f32,
    pad_ring: Option<egui::Rect>,   // Home / Pick 6: where the ring goes this frame
    pad_nav: Option<usize>,   // the cursor is on the nav bar: 0 menu button, 1 to 3 the page tabs
    pad_bar: Option<usize>,   // the cursor is on the Library toolbar (an index into pad_bar_items)
    pad_nav_rects: Vec<egui::Rect>,   // the nav bar's four controls last frame (menu button, three tabs), so the pad can start on the one the mouse rests on
    pad_bar_items: Vec<egui::Rect>,   // the toolbar's controls last frame, left to right
    pad_menu: bool,   // the menu was opened with the pad: focus its first item
    pad_dir: Option<(egui::Id, gamepad::Nav)>,   // dialog: the focus and direction of the last Up/Down press, to fall back to Tab when egui's geometry finds nothing
    pad_show: bool,   // the ring is drawn from the first pad press until the mouse moves
    pad_move: Option<egui::FocusDirection>,   // a pad direction to hand egui's focus search directly (a slider or text box under the cursor would swallow the arrow key)
    pad_popup: Option<PadPopup>,   // a drop-down list opened with the pad is on screen
    pad_popup_was_open: bool,
    pad_popup_close: bool,   // A was pressed on an entry of a drop-down list: egui closes a popup on a mouse click only, so close it a frame later (after the entry took the press)
    pad_stepping: Option<egui::Id>,   // the control a row-by-row scroll toward the next control started from: its repeated presses are one move, not a new entry into the list
    /// The control and direction of an Up/Down that was answered with a Tab step: where the Tab landed is checked one frame later (entering a scrolling list that way must start at its first or last row).
    /// The control of a dialog or menu the mouse was last over: the pad's first press after the mouse was used starts there.
    pad_hover: Option<egui::Id>,
    pad_tab: Option<(egui::Id, gamepad::Nav, u8)>,   // (the control, the direction, frames still to wait for the Tab to take effect)
    pad_wrap: Option<(egui::LayerId, egui::Id, bool, u8)>,   // a pad press wrapped round inside a scrolling list: (its layer, the list, to the top?, frames to wait while it scrolls) then focus its first or last control
    pad_adjust: bool,   // a slider has the focus AND A was pressed on it: Left/Right change it; otherwise they are ignored there
    pad_typing: bool,   // a text box has the focus AND the pad (or the keyboard) is typing in it; otherwise the pad only outlines it and A starts the typing
    pad_good: Option<egui::Id>,   // the last dialog control the pad focused, to put the focus back on when it strays to the page behind
    pad_scroll: gamepad::Scroll,   // right stick Y in thousandths, written by the pad thread
    rs_since: Option<Instant>, rs_at: Instant,   // when the right stick left its dead zone, and the last frame it was read
    rs_db: Option<PathBuf>, rs_sort: String, rs_preview: (Instant, i64, String),   // the Library's sorted-position preview while the right stick scrolls fast
    rs_px: f32, rs_mult: f32,   // pixels to scroll this frame (down positive) and the ramp multiplier, for the position preview
    pad_queue: gamepad::Queue,   // presses from the gamepad thread, turned into egui key events each frame
    // timing (ms): [egui run, grid update, compose, overlay, present, whole frame] and frame intervals
    parts: Vec<[f32; 6]>,
    intervals: Vec<f32>,
    last_frame: Instant,
    live: Instant,
}

impl Own {
    fn ui(&mut self, ui: &mut egui::Ui, forced_y: Option<f32>, grid: &mut Option<(egui::Rect, f32)>) {
        self.nav(ui);
        self.toolbar(ui);
        if self.search != self.search_applied { self.apply_search(); self.scroll_to = Some(0.0); self.search_edit_at = Some(Instant::now()); }
        let mut forced_y = forced_y.or(self.scroll_to.take());
        if self.rs_px != 0.0 && self.pad_free() && forced_y.is_none() && !self.list_mode {
            // not past the last row: it used to run on beyond the games and snap back when the stick was let go
            let max_y = self.lib_grid.map_or(f32::MAX, |(r, _)| (self.core.rows_height() - r.height()).max(0.0));   // last frame's view: `grid` is still empty at this point
            forced_y = Some((self.y + self.rs_px).clamp(0.0, max_y));
        }
        if let Some(d) = &mut self.dice {
            if let Some((t0, from, to)) = d.scroll {
                let p = (t0.elapsed().as_secs_f32() / 4.0).min(1.0);
                if forced_y.is_none() { forced_y = Some(from + (to - from) * (1.0 - (1.0 - p).powi(3))); }
                if p >= 1.0 { d.scroll = None; }
                ui.ctx().request_repaint();
            }
        }
        ui.horizontal(|ui| {
            ui.add_space(20.0);
            if !self.list_mode { ui.label(egui::RichText::new(self.lib_header.clone()).color(egui::Color32::from_rgb(0xc7, 0xd5, 0xe0))); }
            if self.tier_job.is_some() { ui.label(egui::RichText::new("Preparing covers for this size\u{2026}").weak()); }
        });
        if self.view_open { self.view_panel(ui); }
        if self.list_mode { self.list_ui(ui, grid); return; }
        if self.lib_loaded && self.lib_full.as_ref().map_or(false, |(g, _)| g.is_empty()) {
            // nothing passes the filter and the hidden libraries (the page's `library-empty-state`)
            let r = ui_kit::rem(ui.ctx());
            ui.vertical_centered(|ui| {
                ui.add_space(6.0 * r);
                ui.label(egui::RichText::new("No games to show.").size(1.1 * r));
                if self.lib_head.filtered || !self.search.trim().is_empty() {
                    ui.add_space(1.0 * r);
                    if ui_kit::toolbar_button(ui, "Show All Games", false, r).clicked() { self.want_show_all = true; }
                }
            });
            *grid = Some((egui::Rect::from_min_size(ui.cursor().min, egui::vec2(1.0, 1.0)), 0.0));   // the frame needs a grid rectangle even when there are no cards (as the list view gives one)
            return;
        }
        let avail = ui.available_size();
        let bar = ui.spacing().scroll.allocated_width();
        let lay = layout(self.sim_w.unwrap_or(avail.x) - bar, self.cw.round() as u32);
        self.lay = lay;
        let (cols, cw) = (lay.cols, lay.cw);
        self.core.set_layout(cols, cw);
        let total_h = self.core.rows_height();
        let mut area = egui::ScrollArea::vertical().auto_shrink([false, false]).scroll_source(egui::scroll_area::ScrollSource { drag: false, ..egui::scroll_area::ScrollSource::ALL });
        if let Some(y) = forced_y { area = area.vertical_scroll_offset(y); }
        let out = area.show_viewport(ui, |ui, viewport| {
            ui.allocate_space(egui::vec2(avail.x - bar, total_h));   // the cards themselves are drawn by our renderer
            viewport.min.y
        });
        // titles for cards that have no image yet (the layer draws only the plain rectangle)
        let (rect, y) = (out.inner_rect, out.inner);
        let painter = ui.painter().with_clip_rect(rect);
        let accent = egui::Color32::from_rgb(self.theme.accent.0, self.theme.accent.1, self.theme.accent.2);
        let vis = self.core.rows_in(y, y + rect.height());
        let prev_edit = self.edit_rect.take();
        let mut zoom_hit: Option<(i64, egui::Rect)> = None;
        let zoom_pad = if self.pad_show && self.pad_nav.is_none() && !self.menu_open && self.pad_free() { self.pad_cur } else { None };
        let zoom_ptr = ui.input(|i| i.pointer.hover_pos()).filter(|p| rect.contains(*p) && ui.ctx().layer_id_at(*p).map_or(true, |l| l.order == egui::Order::Background));
        for ri in vis {
            let row = self.core.rows[ri].clone();
            if let (0, Some(si)) = (row.count, row.section) {   // a group header
                let Some(g) = self.groups.get(si) else { continue };
                let top = rect.min.y + row.y - y + (row.h - HEADER_H);
                let hdr = egui::Rect::from_min_size(egui::pos2(rect.min.x + lay.x_off as f32, top), egui::vec2(lay.cols as f32 * (cw as f32 + GAP as f32) - GAP as f32, HEADER_H - 6.0));
                let id = ui.id().with(("group", si));
                let resp = ui.interact(hdr.intersect(rect), id, egui::Sense::click());
                if resp.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
                if resp.clicked() { self.toggle = Some(si); }
                let line = if resp.hovered() { egui::Color32::from_rgb(0x3a, 0x58, 0x70) } else { egui::Color32::from_rgb(0x2a, 0x3f, 0x55) };
                painter.line_segment([egui::pos2(hdr.min.x, hdr.max.y), egui::pos2(hdr.max.x, hdr.max.y)], egui::Stroke::new(1.0_f32, line));
                let collapsed = self.core.sections.get(si).map_or(false, |s| s.collapsed);
                painter.text(egui::pos2(hdr.min.x + 8.0, hdr.center().y), egui::Align2::LEFT_CENTER, if collapsed { "\u{25B8}" } else { "\u{25BE}" }, egui::FontId::proportional(12.0), accent);
                let mut job = egui::text::LayoutJob::default();
                job.append(&g.label.to_uppercase(), 0.0, egui::text::TextFormat { font_id: theme::bold(12.5), color: egui::Color32::from_rgb(0xcd, 0xd6, 0xe0), extra_letter_spacing: 0.9, ..Default::default() });
                let gl = painter.layout_job(job);
                painter.galley(egui::pos2(hdr.min.x + 26.0, hdr.center().y - gl.size().y / 2.0), gl, egui::Color32::WHITE);
                painter.text(egui::pos2(hdr.max.x - 4.0, hdr.center().y), egui::Align2::RIGHT_CENTER, g.len.to_string(), egui::FontId::proportional(12.0), egui::Color32::from_rgb(0x5c, 0x6b, 0x78));
                continue;
            }
            for c in 0..row.count {
                let idx = row.first + c;
                if let Some(g) = self.core.games.get(idx) {
                    let cmin = rect.min + egui::vec2(lay.x_off as f32 + c as f32 * (cw as f32 + GAP as f32), row.y + ROW_TOP - y);
                    let card = egui::Rect::from_min_size(cmin, egui::vec2(cw as f32, grid_core::card_h(cw) as f32));
                    if zoom_ptr.map_or(false, |p| card.contains(p)) { self.lib_mouse_card = Some(g.appid); }
                    // the pad's card wins over a mouse that is merely resting on another one (moving or clicking the mouse ends the pad's focus anyway)
                    if zoom_pad.is_some() { if zoom_pad == Some(g.appid) { zoom_hit = Some((g.appid, card)); } }
                    else if zoom_ptr.map_or(false, |p| card.contains(p)) { zoom_hit = Some((g.appid, card)); }
                }
                if self.tip_fields.is_some() {
                    if let (Some(pin), Some(g)) = (self.tip_pin, self.core.games.get(idx)) { if g.appid == pin {
                        let cmin = rect.min + egui::vec2(lay.x_off as f32 + c as f32 * (cw as f32 + GAP as f32), row.y + ROW_TOP - y);
                        self.tip_target = Some((pin, egui::Rect::from_min_size(cmin, egui::vec2(cw as f32, grid_core::card_h(cw) as f32))));
                    } }
                    else if let (Some(a), Some(g)) = (zoom_pad, self.core.games.get(idx)) {
                        // the pad's card gets the tooltip, whatever the resting mouse is over
                        if g.appid == a { self.tip_target = Some((a, egui::Rect::from_min_size(rect.min + egui::vec2(lay.x_off as f32 + c as f32 * (cw as f32 + GAP as f32), row.y + ROW_TOP - y), egui::vec2(cw as f32, grid_core::card_h(cw) as f32)))); }
                    }
                    else if let (Some(p), Some(g)) = (ui.input(|i| i.pointer.hover_pos()), self.core.games.get(idx)) {
                        let cmin = rect.min + egui::vec2(lay.x_off as f32 + c as f32 * (cw as f32 + GAP as f32), row.y + ROW_TOP - y);
                        let card = egui::Rect::from_min_size(cmin, egui::vec2(cw as f32, grid_core::card_h(cw) as f32));
                        if rect.contains(p) && card.contains(p) && ui.ctx().layer_id_at(p).map_or(true, |l| l.order == egui::Order::Background) { self.tip_target = Some((g.appid, card)); }
                    }
                }
                // always drawn: a zooming card's grown outline (below) covers it, and on the last frame of a hover fade the grown card is already gone, so skipping this one flickered
                if let Some(&(r8, g8, b8)) = self.core.games.get(idx).and_then(|g| self.outlines.get(&g.appid)) {
                    let cmin = rect.min + egui::vec2(lay.x_off as f32 + c as f32 * (cw as f32 + GAP as f32), row.y + ROW_TOP - y);
                    let card = egui::Rect::from_min_size(cmin, egui::vec2(cw as f32, grid_core::card_h(cw) as f32));
                    painter.rect_stroke(card, 4, egui::Stroke::new(3.0_f32, egui::Color32::from_rgb(r8, g8, b8)), egui::StrokeKind::Outside);   // `outline: 3px solid var(--outline-color)`
                }
                if let (Some(cfg), Some(g)) = (self.badge_cfg.clone(), self.core.games.get(idx)) {
                    let cmin = rect.min + egui::vec2(lay.x_off as f32 + c as f32 * (cw as f32 + GAP as f32), row.y + ROW_TOP - y);
                    let card = egui::Rect::from_min_size(cmin, egui::vec2(cw as f32, grid_core::card_h(cw) as f32));
                    let (appid, installed) = (g.appid, g.installed);
                    self.paint_badges(ui, &painter, card, &cfg, appid, installed);
                }
                if self.select_mode && self.core.games.get(idx).map_or(false, |g| self.selected.contains(&g.appid)) {
                    let cmin = rect.min + egui::vec2(lay.x_off as f32 + c as f32 * (cw as f32 + GAP as f32), row.y + ROW_TOP - y);
                    let card = egui::Rect::from_min_size(cmin, egui::vec2(cw as f32, grid_core::card_h(cw) as f32)).shrink(2.0);
                    painter.rect_stroke(card, 4, egui::Stroke::new(3.0_f32, accent), egui::StrokeKind::Outside);   // `.game-card.selected { outline: 3px solid var(--accent); outline-offset: -2px }`
                }
                if let (Some(corner), Some(p)) = (self.edit_corner.clone(), ui.input(|i| i.pointer.hover_pos())) {
                    let cmin = rect.min + egui::vec2(lay.x_off as f32 + c as f32 * (cw as f32 + GAP as f32), row.y + ROW_TOP - y);
                    let card = egui::Rect::from_min_size(cmin, egui::vec2(cw as f32, grid_core::card_h(cw) as f32));
                    let free = ui.ctx().layer_id_at(p).map_or(true, |l| l.order == egui::Order::Background);
                    if free && rect.contains(p) && (card.contains(p) || prev_edit.map_or(false, |r| r.contains(p) && card.intersects(r))) {
                        if let Some(g) = self.core.games.get(idx) { self.edit_button(ui, &painter, card, &corner, g.appid, 1.0); }
                    }
                }
                if self.core.has_image(idx) { continue; }
                let min = rect.min + egui::vec2(lay.x_off as f32 + c as f32 * (cw as f32 + GAP as f32), row.y + ROW_TOP - y);
                let size = egui::vec2(cw as f32, grid_core::card_h(cw) as f32);
                let galley = painter.layout(self.core.games[idx].name.clone(), egui::FontId::proportional(13.0),
                    egui::Color32::from_rgb(0x8a, 0x93, 0xa3), (cw as f32 - 16.0).max(20.0));
                painter.galley(min + (size - galley.size()) / 2.0, galley, egui::Color32::WHITE);
            }
        }
        // zoom: the hovered card (or the gamepad cursor's) grows 5% about its centre, like the other pages' covers; cut from the layer, drawn over the neighbours
        {
            let now = Instant::now();
            let dt = now.duration_since(self.lib_zoom_at).as_secs_f32() / 0.15;
            self.lib_zoom_at = now;
            let scrolled = (y - self.lib_zoom_y).abs() > 0.5 || self.layer.drawn_y != y.round() as i64 || !self.layer.valid;
            self.lib_zoom_y = y;
            let hov = if scrolled || self.select_mode { None } else { zoom_hit };
            if let Some((a, card)) = hov { self.lib_zoom.entry(a).or_insert((0.0, card.translate(-rect.min.to_vec2()))).1 = card.translate(-rect.min.to_vec2()); }
            if scrolled { self.lib_zoom.clear(); self.lib_zoom_tex.clear(); }
            let mut animating = false;
            for (a, (t, _)) in self.lib_zoom.iter_mut() {
                let target = if hov.map(|h| h.0) == Some(*a) { 1.0 } else { 0.0 };
                *t = if *t < target { (*t + dt).min(target) } else { (*t - dt).max(target) };
                if *t != target { animating = true; }
            }
            self.lib_zoom.retain(|_, (t, _)| *t > 0.0);
            let epoch = self.core.epoch();
            let live: Vec<(i64, f32, egui::Rect)> = self.lib_zoom.iter().map(|(a, (t, r))| (*a, *t, *r)).collect();
            self.lib_zoom_tex.retain(|a, _| live.iter().any(|l| l.0 == *a));
            for (a, t, rel) in live {
                {   // painted again every frame (the cover may still be sharpening): the whole card is rendered on its own, not cut from the grid's picture, which has only what is on screen, so a card cut off at the grid's edge grows whole too
                    let (w, h) = (rel.width().round() as usize, rel.height().round() as usize);
                    let mut buf = vec![BG; w * h];
                    let img = match self.core.index_of(a).map(|i| self.core.cover(i)) { Some(Cover::Sharp(p)) | Some(Cover::Blurry(p)) => Some(p), _ => None };
                    paint_op(&mut buf, 0, w, &Op { x: 0, y: 0, w: w as i64, h: h as i64, img });
                    let px: Vec<egui::Color32> = buf.iter().map(|&p| if p >> 24 != 0 { egui::Color32::TRANSPARENT } else { egui::Color32::from_rgb((p >> 16) as u8, (p >> 8) as u8, p as u8) }).collect();
                    let img = egui::ColorImage { size: [w, h], source_size: egui::vec2(w as f32, h as f32), pixels: px };
                    let tex = ui.ctx().load_texture(format!("lib-zoom-{a}"), img, egui::TextureOptions::LINEAR);
                    self.lib_zoom_tex.insert(a, (tex, epoch));
                }
                if let Some((tex, _)) = self.lib_zoom_tex.get(&a) {
                    let k = 1.0 + 0.05 * (t * (2.0 - t));   // ease-out
                    let card = rel.translate(rect.min.to_vec2());
                    let big = egui::Rect::from_center_size(card.center(), card.size() * k);
                    let zp = ui.painter().with_clip_rect(rect.expand2(egui::vec2(12.0, 0.0)));
                    zp.image(tex.id(), big, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
                    // a card with no cover shows its name, drawn over the grid; the grown card is opaque and covers that one, so it carries the name itself
                    if let Some(i) = self.core.index_of(a).filter(|i| !self.core.has_image(*i)) {
                        let g = zp.layout(self.core.games[i].name.clone(), egui::FontId::proportional(13.0 * k), egui::Color32::from_rgb(0x8a, 0x93, 0xa3), ((card.width() - 16.0) * k).max(20.0));
                        zp.galley(big.center() - g.size() / 2.0, g, egui::Color32::WHITE);
                    }
                    if let Some(&(r8, g8, b8)) = self.outlines.get(&a) { zp.rect_stroke(big, (4.0 * k) as u8, egui::Stroke::new(3.0_f32, egui::Color32::from_rgb(r8, g8, b8)), egui::StrokeKind::Outside); }
                    if let (Some(cfg), Some(g)) = (self.badge_cfg.clone(), self.core.index_of(a).and_then(|i| self.core.games.get(i))) { let installed = g.installed; self.paint_badges(ui, &zp, big, &cfg, a, installed); }
                    // the edit pencil was drawn with the plain card, under this grown copy: put it on the grown card's corner, on top
                    if let (Some(corner), Some(p), true) = (self.edit_corner.clone(), zoom_ptr, hov.map_or(false, |h| h.0 == a)) {
                        if card.contains(p) || big.contains(p) || prev_edit.map_or(false, |r| r.contains(p)) { self.edit_button(ui, &zp, big, &corner, a, k); }
                    }
                }
            }
            if hov.is_some() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
            if animating { ui.ctx().request_repaint(); }
        }
        // the sorted-position preview (PlayDate's #scroll-preview): once the right stick has ramped to 3x, a big label at the right says where the scroll is (a letter, a month, a score...)
        {
            let max_y = (total_h - rect.height()).max(0.0);
            let at_edge = (self.rs_px < 0.0 && y <= 2.0) || (self.rs_px > 0.0 && y >= max_y - 2.0);
            if self.rs_mult >= 3.0 && self.rs_px != 0.0 && !at_edge && !self.list_mode && self.pad_free() {
                let row = self.core.rows_in(y, y + 1.0).filter_map(|i| self.core.rows.get(i)).find(|r| r.count > 0).map(|r| r.first);
                if let Some(idx) = row.filter(|i| *i < self.core.games.len()) {
                    let (appid, name) = (self.core.games[idx].appid, self.core.games[idx].name.clone());
                    let stale = self.rs_preview.0.elapsed().as_millis() >= 120 || self.rs_preview.1 != appid;
                    if self.rs_sort == "random" { if self.rs_preview.0.elapsed().as_millis() >= 80 { self.rs_preview = (Instant::now(), 0, scroll_glyphs()); } }
                    else if stale { let t = scroll_preview_text(self.rs_db.as_deref(), &self.rs_sort, appid, &name); if !t.is_empty() { self.rs_preview = (Instant::now(), appid, t); } }
                }
                if !self.rs_preview.2.is_empty() {
                    let p = ui.painter().with_clip_rect(rect);
                    let g = p.layout_no_wrap(self.rs_preview.2.clone(), theme::bold(32.0), egui::Color32::WHITE);
                    let box_ = egui::Rect::from_center_size(egui::pos2(rect.max.x - 40.0 - g.size().x / 2.0 - 20.0, rect.center().y), g.size() + egui::vec2(40.0, 16.0));
                    p.rect_filled(box_, 12, egui::Color32::from_rgba_unmultiplied(20, 24, 30, 217));
                    p.rect_stroke(box_, 12, egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(0x2a, 0x35, 0x42)), egui::StrokeKind::Inside);
                    p.galley(box_.center() - g.size() / 2.0, g, egui::Color32::WHITE);
                }
            }
        }
        if let Some(d) = &mut self.dice {
            let spot = self.core.index_of(d.appid).and_then(|i| self.core.pos_of(i));
            match spot {
                Some((ry, c)) => {
                    let min = rect.min + egui::vec2(lay.x_off as f32 + c as f32 * (cw as f32 + GAP as f32), ry + ROW_TOP - y);
                    let card = egui::Rect::from_min_size(min, egui::vec2(cw as f32, grid_core::card_h(cw) as f32));
                    if d.glow_at.is_none() {
                        // the glow waits until a third of the card is on screen (the web page's IntersectionObserver)
                        let seen = (card.max.y.min(rect.max.y) - card.min.y.max(rect.min.y)).max(0.0) / card.height();
                        if seen >= 0.3 { d.glow_at = Some(Instant::now()); }
                    }
                    if let Some(t0) = d.glow_at {
                        match dice_alpha(t0.elapsed().as_secs_f32()) {
                            Some(a) => {
                                let col = |k: f32| egui::Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), (a * k * 255.0) as u8);
                                painter.rect_stroke(card, 4, egui::Stroke::new(6.0_f32, col(0.5)), egui::StrokeKind::Outside);   // box-shadow: 0 0 0 6px, 50%
                                painter.rect_stroke(card, 4, egui::Stroke::new(3.0_f32, col(1.0)), egui::StrokeKind::Outside);   // outline: 3px
                                ui.ctx().request_repaint_after(Duration::from_millis(16));
                            }
                            None => self.dice = None,
                        }
                    } else { ui.ctx().request_repaint_after(Duration::from_millis(16)); }
                }
                None => self.dice = None,
            }
        }
        *grid = Some((out.inner_rect, out.inner));
    }

    /// Switch the Library between Vertical, Horizontal and List without a restart: the cover shape is a process-wide setting, so a different shape
    /// needs a fresh cover core (its own folder and packs) and the list reloaded; List only swaps the view.
    fn apply_orientation(&mut self, o: &str) {
        let horiz = o == "horizontal";
        self.list_mode = o == "list";
        self.list_center = true;
        if horiz != grid_core::is_horizontal() {
            grid_core::HORIZONTAL.store(horiz, std::sync::atomic::Ordering::Relaxed);
            self.core = Core::new(&self.root, |w, h, rgba: Vec<u8>| {
                let px: Vec<u32> = rgba.chunks_exact(4).map(|p| ((p[0] as u32) << 16) | ((p[1] as u32) << 8) | p[2] as u32).collect();
                Arc::new(Pixels { w: w as usize, h: h as usize, px })
            });
            self.lib_loaded = false;
            self.lib_warm = false;
            self.layer.valid = false;
            self.packs_checked = false;
            self.pack_job = None;
            self.tier_job = None;
            self.tier_pending = true;
            self.edit_rect = None;
            self.tip_cache = None;
        }
    }

    /// Remember whether the window is maximized and how big it is, so the next start looks the same.
    fn track_window(&mut self, window: &Window, w: usize, h: usize) {
        let max = window.is_maximized();
        // a "normal" size that fills the screen is not one: macOS's zoom (title bar double-click, green button) is not reported as maximized, at least while it
        // animates, and saving that size made un-maximizing restore an almost full-screen window
        let fills = window.current_monitor().map_or(false, |m| fills_screen((w as u32, h as u32), (m.size().width, m.size().height)));
        if max { self.win_max = (w as u32, h as u32); } else if window.fullscreen().is_none() && !fills { self.win_normal = (w as u32, h as u32); }
        if max != self.win_maximized { self.win_maximized = max; self.save_window(); }
    }

    fn save_window(&mut self) {
        let v = serde_json::json!({"maximized": self.win_maximized, "w": self.win_normal.0, "h": self.win_normal.1, "mw": self.win_max.0, "mh": self.win_max.1});
        if let Some(hs) = &mut self.home { hs.data.save_pref("zest_window", v); }
    }

    /// Narrow the loaded list by the search words (name contains, any case), keeping the groups that still have games, and show it.
    fn apply_search(&mut self) {
        let Some((games, groups)) = &self.lib_full else { return };
        let q = self.search.trim().to_lowercase();
        let hit = |g: &grid_core::Game| q.is_empty() || g.name.to_lowercase().contains(&q);
        let (list, infos): (Vec<grid_core::Game>, Vec<home::GroupInfo>) = if groups.is_empty() {
            (games.iter().filter(|g| hit(g)).cloned().collect(), Vec::new())
        } else {
            let (mut out, mut gi) = (Vec::new(), Vec::new());
            for gr in groups {
                let start = out.len();
                out.extend(games[gr.start..gr.start + gr.len].iter().filter(|g| hit(g)).cloned());
                if out.len() > start { gi.push(home::GroupInfo { key: gr.key.clone(), label: gr.label.clone(), start, len: out.len() - start }); }
            }
            (out, gi)
        };
        let sections = infos.iter().map(|g| grid_core::Section { start: g.start, len: g.len, collapsed: self.collapsed.contains(&g.key) }).collect();
        self.core.set_games(list, sections);
        self.groups = infos;
        self.lib_header = self.lib_head.text(self.core.games.len(), self.search.trim());
        self.search_applied = self.search.clone();
        self.layer.valid = false;
    }

    /// The Library's toolbar: search box, VIEW, CLEAR (while something narrows the list), FILTERS, BULK OPS and the dice.
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let r = ui_kit::rem(ui.ctx());
        let w = ui.ctx().content_rect().width();
        let pad = (w * 0.05).clamp(r, 8.0 * r);
        let accent = egui::Color32::from_rgb(self.theme.accent.0, self.theme.accent.1, self.theme.accent.2);
        let narrowed = self.lib_head.real_filter || !self.search.trim().is_empty();   // CLEAR: a real filter or a search, never hidden libraries alone (the web page's `_is_filtered`)
        ui.add_space(0.6 * r);
        let mut items: Vec<egui::Rect> = Vec::new();
        ui.horizontal(|ui| {
            ui.add_space(pad);
            ui.spacing_mut().item_spacing.x = 0.9 * r;
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(pad);
                ui.spacing_mut().item_spacing.x = 0.9 * r;
                if self.select_mode {
                    let n = self.selected.len();
                    let b = ui_kit::accent_toolbar_button(ui, &format!("{n} selected \u{2715}"), accent, r).on_hover_text("Cancel selection"); items.push(b.rect);
                    if b.clicked() { self.select_mode = false; self.selected.clear(); }
                }
                let b = ui_kit::dice_button(ui, r).on_hover_text("Pick a random game"); items.push(b.rect);
                if b.clicked() { self.dice_pick = true; }
                let b = ui_kit::toolbar_button(ui, "BULK OPS", false, r); items.push(b.rect);
                if b.clicked() { self.bulk_open = true; }
                let b = ui_kit::toolbar_button(ui, "FILTERS", false, r); items.push(b.rect);
                if b.clicked() { self.filter_open = true; }
                if narrowed { let b = ui_kit::toolbar_button(ui, "\u{2715} CLEAR", true, r); items.push(b.rect); if b.clicked() { self.want_clear = true; } }
                let b = ui_kit::toolbar_button(ui, "VIEW", false, r).on_hover_text("View options"); items.push(b.rect);
                if b.clicked() {
                    self.view_open = !self.view_open;
                    if !self.view_open { self.cw = self.applied_cw; }
                    if self.view_open { self.view_stage = (self.view_vals.0.clone(), self.view_vals.1.clone(), self.view_vals.2.clone(), self.cw); }
                }
                // the search box takes whatever is left
                let resp = ui.scope(|ui| {
                    ui_kit::style_fields(ui);
                    ui.visuals_mut().extreme_bg_color = egui::Color32::from_rgb(0x1a, 0x1a, 0x1a);
                    ui.visuals_mut().selection.stroke = egui::Stroke::new(1.0_f32, accent);
                    { let wd = &mut ui.visuals_mut().widgets; for wgt in [&mut wd.inactive, &mut wd.hovered] { wgt.bg_stroke = egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(0x33, 0x33, 0x33)); } }
                    let vpad = ((3.0 * r - 1.1 * r * 1.25) / 2.0).max(0.0) as i8;
                    ui.add(egui::TextEdit::singleline(&mut self.search).id(egui::Id::new("library_search")).hint_text("Search games...").font(egui::FontId::proportional(1.1 * r))
                        .margin(egui::Margin { left: (1.25 * r) as i8, right: (2.75 * r) as i8, top: vpad, bottom: vpad }).desired_width(ui.available_width()).min_size(egui::vec2(0.0, 3.0 * r)))
                }).inner;
                items.push(resp.rect);
                if !self.search.is_empty() {
                    let x = egui::Rect::from_center_size(egui::pos2(resp.rect.right() - 1.4 * r, resp.rect.center().y), egui::Vec2::splat(1.8 * r));
                    let xr = ui.interact(x, ui.id().with("search_clear"), egui::Sense::click());
                    ui.painter().text(x.center(), egui::Align2::CENTER_CENTER, "\u{2715}", egui::FontId::proportional(1.1 * r), if xr.hovered() { egui::Color32::WHITE } else { ui_kit::MUTED() });
                    if xr.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
                    if xr.clicked() { self.search.clear(); resp.request_focus(); }
                }
            });
        });
        items.sort_by(|a, b| a.min.x.partial_cmp(&b.min.x).unwrap());
        self.pad_bar_items = items;
        if let Some(i) = self.pad_bar { if let Some(rc) = self.pad_bar_items.get(i) { self.pad_ring_draw(ui.ctx(), rc.expand(2.0), ui.ctx().content_rect(), false, 4.0); } }
        ui.add_space(0.6 * r);
    }

    /// Remember the search words in state.json the way the page does: a `name LIKE` condition in front of the saved filter tree.
    fn persist_search(&self, h: &mut HomeState) {
        let q = self.search.trim();
        let cur = h.data.state_json().get("filter_tree").cloned();
        let base = cur.as_ref().map(tree_without_search);
        if base.as_ref().map_or(false, |b| b.get("saved_filter").is_some()) { return; }   // a saved-filter pointer cannot be nested: leave it alone
        let has_base = base.as_ref().map_or(false, |b| b.get("custom_sql").and_then(|v| v.as_str()).map_or(false, |s| !s.is_empty()) || b.get("items").and_then(|v| v.as_array()).map_or(false, |a| !a.is_empty()));
        let tree = if q.is_empty() { base.unwrap_or_else(|| serde_json::json!({"type": "group", "logic": "AND", "items": []})) } else {
            let node = serde_json::json!({"type": "condition", "column": "name", "operator": "LIKE", "value": q});
            if has_base { serde_json::json!({"type": "group", "logic": "AND", "items": [node, base.unwrap()]}) } else { serde_json::json!({"type": "group", "logic": "AND", "items": [node]}) }
        };
        h.data.save_pref("filter_tree", tree);
    }

    /// The hover tooltip (`PDInfoTip.buildInfo`, text rows only: no alternate cover yet), shown after the pointer rests on a card for a moment.
    fn tip_ui(&mut self, ctx: &egui::Context) {
        if std::env::var_os("ZEST_TIP_DEBUG").is_some() { eprintln!("tip: fields {} target {:?} cache {:?}", self.tip_fields.is_some(), self.tip_target.map(|t| t.0), self.tip_cache.as_ref().map(|c| c.0)); }
        let (Some(fields), Some((id, card)), Some((cid, info, hltb))) = (self.tip_fields.clone(), self.tip_target, self.tip_cache.clone()) else { self.tip_since = None; return };
        if cid != id { return; }
        match self.tip_since { Some((i, _)) if i == id => {}, _ => { self.tip_since = Some((id, Instant::now())); } }
        if self.tip_since.map_or(true, |(_, t)| t.elapsed() < Duration::from_millis(if self.shot_size.is_some() { 0 } else { 250 })) { ctx.request_repaint_after(Duration::from_millis(60)); return; }
        let r = ui_kit::rem(ctx);
        let f = &info.fields;
        let dim = egui::Color32::from_rgb(self.theme.text_secondary.0, self.theme.text_secondary.1, self.theme.text_secondary.2);
        let bright = egui::Color32::from_rgb(self.theme.text_heading.0, self.theme.text_heading.1, self.theme.text_heading.2);   // values on the tooltip's panel: the heading color (bright is for text on colored fills)
        let mut rows: Vec<(String, String)> = Vec::new();
        let mins: f64 = f.playtime_mins.parse().unwrap_or(0.0);
        if fields.contains("playtime") { rows.push(("Time played".into(), if mins > 0.0 { home::fmt_hours(mins) } else { "Never played".into() })); }
        if fields.contains("last_played") { rows.push(("Last played".into(), home::tip_date(&f.last_played).unwrap_or_else(|| "Never".into()))); }
        if fields.contains("date_added") { if let Some(d) = home::tip_date(&f.date_added) { rows.push(("Date added".into(), d)); } }
        if fields.contains("release_date") { if let Some(d) = home::tip_date(&f.release_date) { rows.push(("Released".into(), d)); } }
        if fields.contains("platform") { rows.push(("Library".into(), home::platform_label(&info.platform))); }
        if fields.contains("community_score") && !f.review_percentage.is_empty() { rows.push(("Steam community".into(), format!("{}({}%)", if f.review_score.is_empty() { String::new() } else { format!("{} ", f.review_score) }, f.review_percentage))); }
        if fields.contains("metacritic") && !f.metacritic_score.is_empty() { rows.push(("Metacritic".into(), f.metacritic_score.clone())); }
        if fields.contains("developers") && !f.developers.is_empty() { rows.push(("Developer".into(), f.developers.split(',').collect::<Vec<_>>().join(", "))); }
        if fields.contains("publishers") && !f.publishers.is_empty() { rows.push(("Publisher".into(), f.publishers.split(',').collect::<Vec<_>>().join(", "))); }
        let desc = if fields.contains("description") && !f.description.is_empty() { let d = &f.description; Some(if d.chars().count() > 400 { format!("{}...", d.chars().take(397).collect::<String>().trim_end()) } else { d.clone() }) } else { None };
        let quals = self.tip_quals.as_ref().filter(|q| q.0 == id).and_then(|q| q.1.clone());
        if rows.is_empty() && desc.is_none() && quals.is_none() && !(fields.contains("hltb") && hltb.iter().any(|v| *v > 0.0)) { return; }
        let screen = ctx.content_rect();
        // last frame's size of this tooltip (a first guess before it has been drawn)
        let tip_size = ctx.memory(|m| m.area_rect(egui::Id::new(("card_tip", id)))).map_or(egui::vec2(330.0, 220.0), |r| r.size());
        let (pos, pivot) = (tip_place(card, tip_size, screen), egui::Align2::LEFT_TOP);
        let t = self.theme.clone();
        egui::Area::new(egui::Id::new(("card_tip", id))).order(egui::Order::Tooltip).pivot(pivot).fixed_pos(pos).constrain(true).interactable(false).fade_in(false).show(ctx, |ui| {
            egui::Frame::new().fill(egui::Color32::from_rgb(t.bg_surface.0, t.bg_surface.1, t.bg_surface.2)).stroke(egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(t.border.0, t.border.1, t.border.2)))
                .corner_radius(7).inner_margin(egui::Margin::symmetric(12, 9)).shadow(egui::Shadow { offset: [0, 4], blur: 18, spread: 0, color: egui::Color32::from_black_alpha(115) }).show(ui, |ui| {
                ui.set_max_width(330.0);
                ui.spacing_mut().item_spacing.y = 0.0;
                if let Some(d) = &desc { ui.label(egui::RichText::new(d).size(0.95 * r).color(bright)); ui.add_space(6.0); }
                for (k, v) in &rows {
                    let mut job = egui::text::LayoutJob::default();
                    job.append(&format!("{k}: "), 0.0, egui::text::TextFormat { font_id: egui::FontId::proportional(0.95 * r), color: dim, ..Default::default() });
                    job.append(v, 0.0, egui::text::TextFormat { font_id: egui::FontId::proportional(0.95 * r), color: bright, ..Default::default() });
                    ui.label(job);
                }
                if fields.contains("hltb") && hltb.iter().any(|v| *v > 0.0) {
                    egui::Grid::new("tip_hltb").spacing([16.0, 0.0]).show(ui, |ui| {
                        for l in ["Main story", "Extras", "Completionist"] { ui.label(egui::RichText::new(l).size(0.95 * r).color(dim)); }
                        ui.end_row();
                        for m in hltb { ui.label(egui::RichText::new(if m > 0.0 { home::fmt_hours(m).replace(" hrs", "h") } else { "\u{2013}".into() }).size(0.95 * r).color(bright)); }
                        ui.end_row();
                    });
                }
                if let Some(p) = &quals {
                    if !(rows.is_empty() && desc.is_none()) { ui.add_space(6.0); ui.separator(); ui.add_space(4.0); }
                    pagywosg_dialog::panel_ui(ui, r, p, true, dim, bright);
                }
            });
        });
    }

    /// A badge icon from `static/img/badges/` (uploaded in the Card Badges settings), decoded once.
    fn badge_texture(&mut self, ctx: &egui::Context, file: &str) -> Option<egui::TextureHandle> {
        if let Some(t) = self.badge_tex.get(file) { return t.clone(); }
        let path = self.root.join("static/img/badges").join(file);
        let bundled = file.strip_prefix("@default:").and_then(badge_defaults::decode);   // a store's bundled logo, not a file of the user's
        let tex = bundled.or_else(|| (!file.contains(['/', '\\'])).then(|| image::open(&path).ok()).flatten().map(|im| im.to_rgba8())).map(|rgba| {
            ctx.load_texture(format!("badge_{file}"), egui::ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw()), egui::TextureOptions::LINEAR)
        });
        self.badge_tex.insert(file.to_string(), tex.clone());
        tex
    }

    /// `renderCardBadges`: what each of the four corners shows for this game. Platform icons that plugins ship are not loaded (the text label stands in).
    fn paint_badges(&mut self, ui: &egui::Ui, painter: &egui::Painter, card: egui::Rect, cfg: &home::BadgeCfg, appid: i64, installed: bool) {
        let r = ui_kit::rem(ui.ctx());
        let side = 32.0 * cfg.scale;
        let Some(d) = self.badges.get(&appid).cloned() else { return };
        for (corner, feature) in &cfg.slots {
            let mut text: Option<(String, Option<String>)> = None;   // (text, background color)
            let mut icon: Option<(String, bool)> = None;              // (file, round)
            match feature.as_str() {
                "installed" => if installed { icon = cfg.installed_icon.clone().map(|f| (f, false)); },
                "platform" => match cfg.plat_icons.get(&d.platform) {
                    Some(f) => icon = Some((f.clone(), false)),
                    // "bundled badge": the store's own logo, circle-cropped, when the program has one; otherwise (and for "text") the store's name
                    None => if cfg.fallback == "default" && badge_defaults::bytes(&d.platform).is_some() { icon = Some((format!("@default:{}", d.platform), false)); }
                            else if cfg.fallback != "none" { text = Some((home::platform_label(&d.platform), None)); },
                },
                "achievement_percent" => if d.total_ach > 0.0 {
                    let pct = (100.0 * d.unlocked / d.total_ach).round();
                    if let Some(c) = home::BadgeCfg::band(&cfg.ach, pct) { text = Some((format!("{pct}%"), Some(c.to_string()))); }
                },
                "review_score" => if d.total_rev > 0.0 {
                    if let Some(p) = if cfg.rev_raw { d.review_pct } else { d.weighted_pct } {
                        let pct = p.round();
                        if let Some(c) = home::BadgeCfg::band(&cfg.rev, pct) { text = Some((format!("{pct}%"), Some(c.to_string()))); }
                    }
                },
                "hltb" => if let Some(m) = cfg.hltb_minutes(&d.hltb) { text = Some((home::fmt_hours(m), None)); },
                _ => {}
            }
            let anchor = |size: egui::Vec2| -> egui::Rect {
                let x = if corner.ends_with("left") { card.min.x + 6.0 } else { card.max.x - 6.0 - size.x };
                let y = if corner.starts_with("top") { card.min.y + 6.0 } else { card.max.y - 6.0 - size.y };
                egui::Rect::from_min_size(egui::pos2(x, y), size)
            };
            if let Some((file, _)) = icon {
                if let Some(t) = self.badge_texture(ui.ctx(), &file) {
                    let ts = t.size_vec2();
                    let k = (side / ts.x).min(side / ts.y);   // object-fit: contain, centred in the square
                    let sq = anchor(egui::vec2(side, side));
                    let img = egui::Rect::from_center_size(sq.center(), ts * k);
                    painter.image(t.id(), img, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
                }
            } else if let Some((txt, bg)) = text {
                // `.card-badge-text`: 0.65rem x scale, padding 2px 5px, line-height 1.4, radius 3, at most the card minus 12px
                let fill = bg.as_deref().and_then(|h| u32::from_str_radix(h.trim_start_matches('#'), 16).ok().filter(|_| h_ok(bg.as_deref()))).map(|v| egui::Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)).unwrap_or(egui::Color32::from_rgba_unmultiplied(0, 0, 0, 179));
                let lum = (0.299 * fill.r() as f32 + 0.587 * fill.g() as f32 + 0.114 * fill.b() as f32) / 255.0;
                let ink = if bg.is_some() && lum > 0.6 { egui::Color32::from_rgb(0x11, 0x11, 0x11) } else { egui::Color32::WHITE };
                let fs = 0.65 * r * cfg.scale;
                let g = painter.layout(txt, egui::FontId::proportional(fs), ink, (card.width() - 12.0 - 10.0).max(10.0));
                let size = egui::vec2(g.size().x + 10.0, fs * 1.4 + 4.0);
                let b = anchor(size);
                painter.rect_filled(b, 3, fill);
                painter.galley(egui::pos2(b.min.x + 5.0, b.center().y - g.size().y / 2.0), g, ink);
            }
        }
    }

    /// The Library's list view: a scrolling list of rows (name and status) on the left, the chosen game's details on the right.
    fn list_ui(&mut self, ui: &mut egui::Ui, grid: &mut Option<(egui::Rect, f32)>) {
        let r = ui_kit::rem(ui.ctx());
        let avail = ui.available_rect_before_wrap();
        *grid = Some((egui::Rect::from_min_size(avail.min, egui::vec2(1.0, 1.0)), 0.0));
        let accent = egui::Color32::from_rgb(self.theme.accent.0, self.theme.accent.1, self.theme.accent.2);
        let pane_w = (avail.width() * 0.2).max(240.0).min(avail.width() * 0.5);
        let row_h = 2.6 * r;
        let n = self.core.games.len();
        let typing = ui.ctx().egui_wants_keyboard_input();
        if !typing && !self.select_mode {
            let step: i64 = ui.input(|i| (i.key_pressed(egui::Key::ArrowDown) as i64) - (i.key_pressed(egui::Key::ArrowUp) as i64));
            if step != 0 && n > 0 {
                let cur = self.list_sel.and_then(|a| self.core.games.iter().position(|g| g.appid == a)).map(|p| p as i64).unwrap_or(-1);
                let next = (cur + step).clamp(0, n as i64 - 1) as usize;
                self.list_sel = Some(self.core.games[next].appid);
                self.list_center = true;
            }
        }
        let list_rect = egui::Rect::from_min_size(avail.min + egui::vec2(8.0, 0.0), egui::vec2(pane_w, avail.height() - 8.0));
        let mut list_ui = ui.new_child(egui::UiBuilder::new().max_rect(list_rect));
        // rows: a header per group (collapsible) and the games under it
        let mut rows: Vec<(Option<usize>, usize)> = Vec::new();   // (Some(group) header | None game, group index or game index)
        if self.groups.is_empty() { rows.extend((0..n).map(|i| (None, i))); }
        else {
            for (gi, g) in self.groups.iter().enumerate() {
                rows.push((Some(gi), gi));
                if !self.collapsed.contains(&g.key) { rows.extend((g.start..(g.start + g.len).min(n)).map(|i| (None, i))); }
            }
        }
        let mut toggle_group: Option<usize> = None;
        let mut area = egui::ScrollArea::vertical().id_salt("list_pane").auto_shrink([false, false]);
        if std::mem::take(&mut self.list_center) {
            if let Some(ri) = rows.iter().position(|(h, i)| h.is_none() && Some(self.core.games[*i].appid) == self.list_sel) {
                area = area.vertical_scroll_offset((ri as f32 * row_h - (list_rect.height() - row_h) / 2.0).max(0.0));
            }
        }
        area.show_rows(&mut list_ui, row_h, rows.len(), |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for ri in range {
                let (hdr, idx) = rows[ri];
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), egui::Sense::click());
                if let Some(gi) = hdr {
                    let g = &self.groups[gi];
                    let collapsed = self.collapsed.contains(&g.key);
                    ui.painter().text(egui::pos2(rect.min.x + 10.0, rect.center().y), egui::Align2::LEFT_CENTER, if collapsed { "\u{25B8}" } else { "\u{25BE}" }, egui::FontId::proportional(0.8 * r), accent);
                    let mut job = egui::text::LayoutJob::default();
                    job.append(&g.label.to_uppercase(), 0.0, egui::text::TextFormat { font_id: theme::bold(0.75 * r), color: accent, extra_letter_spacing: 0.8, ..Default::default() });
                    let gl = ui.painter().layout_job(job);
                    ui.painter().galley(egui::pos2(rect.min.x + 26.0, rect.center().y - gl.size().y / 2.0), gl, egui::Color32::WHITE);
                    if resp.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
                    if resp.clicked() { toggle_group = Some(gi); }
                    continue;
                }
                let g = &self.core.games[idx];
                let sel = self.list_sel == Some(g.appid);
                if sel { ui.painter().rect_filled(rect, 0, accent.gamma_multiply(0.22)); ui.painter().rect_filled(egui::Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height())), 0, accent); }
                else if resp.hovered() { ui.painter().rect_filled(rect, 0, egui::Color32::from_white_alpha(14)); }
                ui.painter().line_segment([rect.left_bottom(), rect.right_bottom()], egui::Stroke::new(1.0_f32, egui::Color32::from_white_alpha(10)));
                if self.select_mode && self.selected.contains(&g.appid) { ui.painter().rect_stroke(rect.shrink(1.0), 3, egui::Stroke::new(2.0_f32, accent), egui::StrokeKind::Inside); }
                let appid_row = g.appid;
                if self.list_icons.len() > 400 { let keep = self.list_sel; self.list_icons.retain(|k, _| Some(*k) == keep); }   // the selected game's icon stays
                self.list_icon_load(ui.ctx(), appid_row);
                let icon_w = if let Some(Some(t)) = self.list_icons.get(&appid_row) {
                    let side = row_h - 6.0;
                    ui.painter().image(t.id(), egui::Rect::from_min_size(egui::pos2(rect.min.x + 10.0, rect.min.y + 3.0), egui::vec2(side, side)), egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
                    side + 10.0
                } else { 0.0 };
                let g = &self.core.games[idx];
                let status = self.list_status.get(&g.appid).cloned().unwrap_or_default();
                let sg = ui.painter().layout_no_wrap(status, egui::FontId::proportional(0.7 * r), egui::Color32::from_rgb(0x8f, 0x98, 0xa0));
                let sw = sg.size().x;
                ui.painter().galley(egui::pos2(rect.max.x - sw - 12.0, rect.center().y - sg.size().y / 2.0), sg, egui::Color32::WHITE);
                let name = ui.painter().layout(g.name.clone(), egui::FontId::proportional(0.9 * r), egui::Color32::from_rgb(0xc7, 0xd5, 0xe0), (rect.width() - sw - 30.0 - icon_w).max(20.0));
                ui.painter().galley(egui::pos2(rect.min.x + 10.0 + icon_w, rect.center().y - name.size().y.min(row_h) / 2.0), name, egui::Color32::WHITE);
                if resp.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); self.lib_mouse_card = Some(appid_row); }   // where the pad starts when it takes over
                if resp.secondary_clicked() { if let Some(p) = resp.interact_pointer_pos() { self.list_menu = Some((g.appid, p)); } }
                if resp.clicked() {
                    if self.select_mode { if !self.selected.remove(&g.appid) { self.selected.insert(g.appid); } } else { self.list_sel = Some(g.appid); }
                }
            }
        });
        if let Some(gi) = toggle_group { let k = self.groups[gi].key.clone(); if !self.collapsed.remove(&k) { self.collapsed.insert(k); } }
        // the detail pane
        let det = egui::Rect::from_min_max(egui::pos2(list_rect.max.x + 14.0, avail.min.y), egui::pos2(avail.max.x - 8.0, avail.max.y - 8.0));
        let mut dui = ui.new_child(egui::UiBuilder::new().max_rect(det));
        dui.painter().rect(det, 6, egui::Color32::from_rgba_unmultiplied(0x1a, 0x23, 0x32, 220), egui::Stroke::new(1.0_f32, ui_kit::EDGE()), egui::StrokeKind::Inside);
        // load the selected game's cover BEFORE the text width is chosen: deciding first made the first frame after a selection change (holding Down) lay the form out at full width, in front of the cover
        if let Some(id) = self.list_edit.as_ref().map(|d| d.appid).filter(|a| Some(*a) == self.list_sel) {
            if self.list_cover.as_ref().map_or(true, |c| c.0 != id) {
                let dir = if grid_core::is_horizontal() { "horizontal" } else { "vertical" };
                self.list_cover = image::open(self.root.join(format!("static/img/library/{dir}/{id}.jpg"))).ok().map(|im| { let im = im.thumbnail(900, 1200).to_rgba8(); (id, ui.ctx().load_texture(format!("lc{id}"), egui::ColorImage::from_rgba_unmultiplied([im.width() as usize, im.height() as usize], im.as_raw()), egui::TextureOptions::LINEAR)) });
            }
        }
        let has_cover = self.list_cover.as_ref().map_or(false, |c| Some(c.0) == self.list_sel);
        let text_w = if has_cover { (det.width() * 0.55 - 28.0).max(200.0) } else { det.width() - 28.0 };
        let mut cui = dui.new_child(egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(det.min + egui::vec2(14.0, 14.0), egui::vec2(text_w, det.height() - 28.0))));
        (|ui: &mut egui::Ui| {
            let Some(id) = self.list_edit.as_ref().map(|d| d.appid).filter(|a| Some(*a) == self.list_sel) else {
                ui.label(egui::RichText::new("Select a game from the list").size(1.0 * r).color(ui_kit::MUTED()));
                return;
            };
            if let Some((_, t)) = &self.list_cover {
                // `#detail-cover-art`: a column 45% of the pane wide and as tall as the pane, raised background, left border, image contained at the top centre
                let col = egui::Rect::from_min_max(egui::pos2(det.max.x - det.width() * 0.45, det.min.y), det.max);
                ui.painter().rect_filled(col.shrink2(egui::vec2(0.0, 1.0)), 0, egui::Color32::from_rgb(0x14, 0x1c, 0x28));
                ui.painter().line_segment([col.left_top(), col.left_bottom()], egui::Stroke::new(1.0_f32, ui_kit::EDGE()));
                let sz = t.size_vec2();
                let k = ((col.width() - 1.0) / sz.x).min(col.height() / sz.y);
                let img = egui::Vec2::new(sz.x * k, sz.y * k);
                let rc = egui::Rect::from_min_size(egui::pos2(col.center().x + 0.5 - img.x / 2.0, col.min.y), img);
                ui.painter().image(t.id(), rc, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
            }
            if self.list_edit.is_some() { self.list_icon_load(ui.ctx(), id); }   // the row may be scrolled out of view: the pane loads its own icon
            if let Some(d) = self.list_edit.as_mut() {
                let icon = self.list_icons.get(&id).cloned().flatten();
                let out = d.show_embedded(ui, text_w, r, icon.as_ref());
                if !matches!(out, edit_dialog::EditOut::None) { self.list_out = Some(out); }
            }
        })(&mut cui);
    }

    /// The pencil button over a hovered card (web `renderEditButton`: `.status-tag`, 6px from the chosen corner, size from the card-size slider).
    fn edit_button(&mut self, ui: &mut egui::Ui, painter: &egui::Painter, card: egui::Rect, corner: &str, appid: i64, scale: f32) {
        let r = ui_kit::rem(ui.ctx());
        let font = (if self.page == Page::Library { (self.cw * 0.009).clamp(0.8, 3.5) * r } else { 1.8 * r }) * scale;   // Home and Pick 6 have no card-size slider: the CSS default 1.8rem
        let g = painter.layout_no_wrap("\u{270E}".into(), egui::FontId::proportional(font), egui::Color32::WHITE);
        let size = g.size() + egui::vec2(12.0 + 2.0, 4.0 + 2.0) * scale;   // padding 2px 6px plus the 1px border
        let x = if corner.ends_with("left") { card.min.x + 6.0 * scale } else { card.max.x - 6.0 * scale - size.x };
        let y = if corner.starts_with("top") { card.min.y + 6.0 * scale } else { card.max.y - 6.0 * scale - size.y };
        let b = egui::Rect::from_min_size(egui::pos2(x, y), size);
        let resp = ui.interact(b, ui.id().with(("edit_btn", appid)), egui::Sense::click());
        let hot = resp.hovered();
        let t = &self.theme;
        let c = |v: theme::Rgb| egui::Color32::from_rgb(v.0, v.1, v.2);
        let (fill, ink, edge) = if hot { (c(t.accent), egui::Color32::from_rgb(0x1b, 0x28, 0x38), c(t.accent)) } else { (ui_kit::INPUT(), c(t.text_primary), c(t.border)) };
        painter.rect(b, 3, fill, egui::Stroke::new(1.0_f32, edge), egui::StrokeKind::Inside);
        painter.galley(b.center() - g.size() / 2.0, g, ink);
        if hot { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
        if resp.clicked() { self.edit_click = Some(appid); }
        self.edit_rect = Some(b);
    }

    /// The dice: a random game from the list on screen, scrolled to the middle the way the web page does it, then outlined.
    fn dice_roll(&mut self, view_h: f32) {
        let n = self.core.games.len();
        if n == 0 { return; }
        let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1);
        let idx = (seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 33) as usize % n;
        if self.core.pos_of(idx).is_none() {   // it sits in a collapsed group: open the group
            if let Some(si) = self.groups.iter().position(|g| idx >= g.start && idx < g.start + g.len) {
                if let Some(g) = self.groups.get(si) { self.collapsed.remove(&g.key); }
                self.core.toggle_section(si);
                self.layer.valid = false;
            }
        }
        let Some((row_y, _)) = self.core.pos_of(idx) else { return };
        let card_h = grid_core::card_h(self.core.card_w) as f32;
        let card_top = row_y + ROW_TOP;
        let max_y = (self.core.rows_height() - view_h).max(0.0);
        let target = (card_top + card_h / 2.0 - view_h / 2.0).clamp(0.0, max_y);
        let top_in_view = card_top - self.y;
        let appid = self.core.games[idx].appid;
        if top_in_view >= 0.0 && top_in_view + card_h <= view_h {
            self.dice = Some(Dice { appid, glow_at: Some(Instant::now()), scroll: None });   // already fully visible: glow at once
        } else {
            self.dice = Some(Dice { appid, glow_at: None, scroll: Some((Instant::now(), self.y, target)) });   // scroll from where we are, no jump
        }
    }

    /// Load (or reload after a change) the Library's game list into the grid model.
    fn ensure_library(&mut self, h: &mut HomeState) {
        if self.lib_loaded && !h.dirty { return; }
        let (games, head, groups) = h.data.library_games();
        if !self.search_init {
            // a search left in state.json by the last session (the saved filter tree's `name LIKE` condition)
            self.search = h.data.state_json().get("filter_tree").map(search_in_tree).unwrap_or_default();
            self.search_init = true;
        }
        self.lib_full = Some((games, groups));
        self.lib_head = head;
        self.apply_search();
        self.view_vals = h.data.view_values();
        if self.view_stage.0.is_empty() { self.view_stage = (self.view_vals.0.clone(), self.view_vals.1.clone(), self.view_vals.2.clone(), self.cw); }
        self.layer.valid = false;
        self.lib_loaded = true;
        self.lib_warm = false;
        h.dirty = false;
    }

    /// While another page is showing, get the Library's first screen of covers decoded at its card size, so entering it shows sharp covers.
    fn warm_library(&mut self, w: usize, h: usize) {
        let bar = self.ctx.global_style().spacing.scroll.allocated_width();
        let lay = layout(w as f32 - bar, self.cw.round() as u32);
        self.lay = lay;
        self.core.set_layout(lay.cols, lay.cw);
        self.core.update_view(self.y.max(0.0), (h as f32 - 44.0).max(100.0), 1);
        self.lib_warm = true;
    }

    /// Sort, order and grouping: saved to state.json and applied by reloading the list (what the web page's View modal does for these three).
    fn view_panel(&mut self, ui: &mut egui::Ui) {
        const GROUP_COMBO_TRIM: f32 = 0.0;
        const SORTS: [(&str, &str); 18] = [("name", "Alphabetical"), ("playtime_forever", "Hours Played"), ("release_date", "Release Date"), ("last_played", "Recently Played"), ("date_added", "Date Added"),
            ("review_percentage", "Review Score"), ("weighted_percentage", "Weighted Score"), ("metacritic_score", "Metacritic Score"), ("hltb_main", "HLTB: Main Story"), ("hltb_extras", "HLTB: Main + Extras"),
            ("hltb_completionist", "HLTB: Completionist"), ("hltb_min", "HLTB: Shortest"), ("hltb_max", "HLTB: Longest"), ("achievement_percent", "Achievement %"), ("achievement_remaining", "Achievements Remaining"),
            ("total_reviews", "Total Reviews"), ("tag_similarity", "Tag Similarity"), ("random", "Random")];
        const GROUPS: [(&str, &str); 8] = [("", "None"), ("installed", "Installed"), ("completion_status", "Completion Status"), ("release_date", "Release Date"), ("date_added", "Date Added"),
            ("review_percentage", "Review Score"), ("weighted_percentage", "Weighted Score"), ("platform", "Library")];
        let (mut sort, mut order, mut group, _) = self.view_stage.clone();
        let mut cw = self.cw;
        let (mut apply, mut cancel) = (false, false);
        let _cols = self.core.cols;
        let accent = egui::Color32::from_rgb(self.theme.accent.0, self.theme.accent.1, self.theme.accent.2);
        if std::env::var_os("ZEST_MEM").is_some() {
            static LAST: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);
            let mut l = LAST.lock().unwrap();
            if l.map_or(true, |t| t.elapsed().as_secs_f32() >= 1.0) { *l = Some(Instant::now()); eprintln!("mem y={:.0} RSS {} MB, caches {} MB, malloc in use/free/mmapped {:?}, layer {} MB, quick/dirty", self.y, rss_mb(), self.core.cache_mb(), heap_mb(), self.layer.buf.len() * 4 / 1048576); }
        }
        let auto = self.auto.is_some();
        let green = (egui::Color32::from_rgb(0x1a, 0x5c, 0x2e), egui::Color32::from_rgb(0x2a, 0x7c, 0x3e));
        let out = ui_kit::modal_footer(ui.ctx(), "view", "Library View", 32.0, |ui, r| {
            ui_kit::style_dropdown(ui, r);
            let label = |ui: &mut egui::Ui, t: &str| { ui.allocate_ui_with_layout(egui::vec2(5.2 * r, 2.5 * r), egui::Layout::left_to_right(egui::Align::Center), |ui| { ui.set_width(5.2 * r); ui.label(egui::RichText::new(t).size(0.82 * r).color(ui_kit::MUTED())); }); };
            let edge = std::cell::Cell::new(0.0f32);   // right edge of the sort row, so the group row can end at the same place
            let combo_text = |t: &str, r: f32| egui::RichText::new(t).strong().size(0.95 * r).color(crate::ui_kit::heading());
            // sort and order
            ui.horizontal(|ui| {
                label(ui, "Sort by");
                let show_order = sort != "random";
                let order_w = if show_order { 3.0 * r + ui.spacing().item_spacing.x } else { 0.0 };
                egui::ComboBox::from_id_salt("view_sort").width(ui.available_width() - order_w).selected_text(combo_text(SORTS.iter().find(|s| s.0 == sort).map_or(sort.as_str(), |s| s.1), r)).show_ui(ui, |ui| {
                    let s = ui_kit::combo_search(ui, egui::Id::new("view_sort"), SORTS.len(), r);
                    let mut n = 0;
                    for (k, l) in SORTS.iter().filter(|(_, l)| s.matches(l)) {
                        let hit = ui.selectable_label(sort == *k, *l).clicked() || (s.enter && n == 0);
                        n += 1;
                        if hit { if sort != *k { order = pd_filter::query::default_sort_order(k).to_string(); }   // each sort starts in its usual order (names up, hours and dates down), as in PlayDate
                            sort = k.to_string(); if s.enter { ui.close(); } }
                    }
                    if n == 0 { ui.label(ui_kit::muted("Nothing matches", 0.85 * r)); }
                });
                if show_order {
                    let b = ui_kit::nav_button(ui, if order == "ASC" { "\u{25B2}" } else { "\u{25BC}" }, 3.0 * r, r, None).on_hover_text("Toggle ascending/descending");
                    if b.clicked() { order = if order == "ASC" { "DESC".into() } else { "ASC".into() }; }
                    edge.set(b.rect.right());
                } else { edge.set(ui.min_rect().right()); }
            });
            ui.add_space(1.25 * r - ui.spacing().item_spacing.y);
            // grouping, right under the sort: both decide how the list is arranged
            ui.horizontal(|ui| {
                label(ui, "Group by");
                let gw = (edge.get() - ui.cursor().min.x - GROUP_COMBO_TRIM * r).max(8.0 * r);
                egui::ComboBox::from_id_salt("view_group").width(gw).selected_text(combo_text(GROUPS.iter().find(|g| g.0 == group).map_or("None", |g| g.1), r)).show_ui(ui, |ui| {
                    for (k, l) in GROUPS { ui.selectable_value(&mut group, k.to_string(), l); }
                });
            });
            ui.add_space(1.25 * r - ui.spacing().item_spacing.y);
            // display mode, then the card-size slider on a row of its own, as wide as the controls above
            ui.horizontal_wrapped(|ui| {
                label(ui, "Display");
                let horiz = grid_core::is_horizontal();
                if ui_kit::view_button(ui, "\u{25AE} Vertical", !horiz && !self.list_mode, true, r).clicked() && (horiz || self.list_mode) { self.view_changes.push(("artwork_orientation", serde_json::json!("vertical"))); }
                if ui_kit::view_button(ui, "\u{25AC} Horizontal", horiz && !self.list_mode, true, r).clicked() && (!horiz || self.list_mode) { self.view_changes.push(("artwork_orientation", serde_json::json!("horizontal"))); }
                if ui_kit::view_button(ui, "\u{2630} List", self.list_mode, true, r).clicked() && !self.list_mode { self.view_changes.push(("artwork_orientation", serde_json::json!("list"))); }
            });
            ui.add_space(0.5 * r);
            let full_w = edge.get() - ui.cursor().min.x;
            ui.add_enabled_ui(!auto, |ui| { ui_kit::slider(ui, &mut cw, 120.0, 400.0, 10.0, full_w, accent); });
        }, |ui, r| {
            ui.horizontal(|ui| {
                let gap = ui.spacing().item_spacing.x;
                let total = ui.available_width() - gap;
                if ui_kit::nav_button(ui, "Close", total / 3.0, r, None).clicked() { cancel = true; }
                if ui_kit::nav_button(ui, "Apply", total * 2.0 / 3.0, r, Some(green)).clicked() { apply = true; }
            });
        });
        self.cw = cw;   // the cards follow the slider as it moves
        if out.closed || cancel { self.view_open = false; self.cw = self.applied_cw; }
        if apply {
            if sort != self.view_vals.0 { self.view_changes.push(("sort", serde_json::json!(sort))); }
            if order != self.view_vals.1 { self.view_changes.push(("order", serde_json::json!(order))); }
            if group != self.view_vals.2 { self.view_changes.push(("group_by", if group.is_empty() { serde_json::Value::Null } else { serde_json::json!(group) })); }
            if (cw - self.applied_cw).abs() >= 0.5 { self.applied_cw = cw; self.tier_pending = true; self.view_changes.push(("card_height", serde_json::json!(cw.round() as i64))); }
            self.view_vals = (sort.clone(), order.clone(), group.clone());
            self.view_open = false;
        }
        self.view_stage = (sort, order, group, cw);
    }

    /// The hamburger dropdown, with the web menu's entries. Choosing one sets `menu_action` (opened by `modal_ui`, which has the data).
    fn hamburger_menu(&mut self, ui: &mut egui::Ui, button: egui::Rect, bar_bottom: f32, accent: egui::Color32) {
        let r = ui_kit::rem(ui.ctx());
        let mut chosen = None;
        let resp = egui::Area::new(egui::Id::new("hamburger_menu")).order(egui::Order::Foreground).fixed_pos(egui::pos2(button.min.x, bar_bottom + 4.0)).show(ui.ctx(), |ui| {
            egui::Frame::new().fill(ui_kit::PANEL()).stroke(egui::Stroke::new(1.0_f32, ui_kit::EDGE())).corner_radius(8)
                .shadow(egui::Shadow { offset: [0, 8], blur: 32, spread: 0, color: egui::Color32::from_black_alpha(140) }).inner_margin(egui::Margin::symmetric(0, 4)).show(ui, |ui| {
                ui.set_width(230.0 * r / 11.9);
                // a window too short for the whole list scrolls it instead of cutting the bottom off
                let room = (ui.ctx().content_rect().bottom() - (bar_bottom + 4.0) - 16.0).max(6.0 * r);
                ui.set_max_height(room);   // the area hands its content only about 390 points; the scroll area can use no more than that
                egui::ScrollArea::vertical().max_height(room).auto_shrink([false, true]).show(ui, |ui| {
                if ui_kit::menu_item_f(ui, "POPULATE ZEST", accent, true, r, std::mem::take(&mut self.pad_menu)) { chosen = Some("populate"); }
                if let Some(n) = self.blaeo_parked.as_ref().and_then(|p| p.notice()) {
                    if ui_kit::menu_item(ui, n, accent, true, r) { chosen = Some("blaeo_ready"); }
                } else if self.blaeo_parked.is_some() { ui.label(ui_kit::muted("  BLAEO sync running...", 0.8 * r)); }
                ui_kit::menu_sep(ui, r);
                for (i, (key, label)) in MENU.iter().enumerate() {
                    if ui_kit::menu_item(ui, label, ui_kit::TEXT(), false, r) { chosen = Some(*key); }
                    if matches!(i, 0 | 6 | 7 | 8) { ui_kit::menu_sep(ui, r); }
                }
                ui_kit::menu_sep(ui, r);
                if ui_kit::menu_item(ui, "Discord \u{2197}", ui_kit::TEXT(), false, r) { chosen = Some("discord"); }
                });
            });
        });
        let outside = ui.input(|i| i.pointer.any_pressed()) && ui.input(|i| i.pointer.interact_pos()).map_or(false, |p| !resp.response.rect.contains(p) && !button.contains(p));
        if outside || ui.input(|i| i.key_pressed(egui::Key::Escape)) { self.menu_open = false; }
        if let Some(c) = chosen { self.menu_open = false; self.menu_action = Some(c); }
    }

    /// The Filters dialog: open it from the toolbar or the Pick 6 link, draw it, and carry out what it returns.
    fn filter_ui(&mut self, ui: &mut egui::Ui, hs: &mut HomeState) {
        if hs.pick.take_filters_request() { self.filter_open = true; }
        if std::mem::take(&mut self.filter_open) && self.filter.is_none() {
            let st = hs.data.state_json();
            let tree = st.get("filter_tree").cloned().filter(|t| t.is_object());
            let saved = st.get("saved_filters").cloned().unwrap_or_else(|| serde_json::json!({}));
            let hidden: Vec<String> = st.get("hidden_platforms").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
            let uniques = ["tags", "groups", "genres", "categories"].map(|c| hs.data.unique_csv(c));
            // the search box's words (not yet read from state.json when the Library has not been opened: then the saved search is the box's content)
            let words = if self.search_init { self.search.trim().to_string() } else { tree.as_ref().map(search_in_tree).unwrap_or_default() };
            let mut d = filter_dialog::FilterDialog::new(tree.as_ref(), saved, hidden, hs.data.platforms_present(), uniques, &words);
            if self.filter_demo { d.demo(); }
            self.filter = Some(d);
        }
        let Some(mut d) = self.filter.take() else { return };
        let preview = |tree: &serde_json::Value| -> String {
            let mut params = Vec::new();
            let sql = pd_filter::build_tree_sql(&pd_filter::strip_search(tree), &mut params, &pd_filter::Ctx::default());
            let mut it = params.iter();
            let mut out = String::from("WHERE ");
            for ch in if sql.is_empty() || sql == "1=1" { "1=1".to_string() } else { sql }.chars() {
                if ch == '?' { match it.next() { Some(serde_json::Value::String(v)) => { out.push('\''); out.push_str(&v.replace('\'', "''")); out.push('\''); } Some(v) => out.push_str(&v.to_string()), None => out.push('?') } } else { out.push(ch); }
            }
            out
        };
        use filter_dialog::FilterOut as F;
        let shown = d.show(ui.ctx(), &preview);
        if d.take_cleared() && !self.search.is_empty() {
            // CLEAR ALL empties the search box too, but only the box: nothing is saved until Apply (the page sets the input's text, no `input` event)
            self.search.clear();
            self.apply_search();
            self.scroll_to = Some(0.0);
        }
        match shown {
            F::None => self.filter = Some(d),
            F::Close => {}
            F::Toast(t) => { hs.toast(t); self.filter = Some(d); }
            F::OpenPag => { self.pag_open = true; }
            F::Apply { tree, hidden } => { self.apply_filter(hs, tree, hidden); }
            F::Quick { tree, hidden } => { self.apply_filter(hs, Some(tree), hidden); }
            F::SavedFilters { map, filter_tree } => {
                hs.data.save_pref("saved_filters", map);
                // renaming or deleting the saved filter that is active moves or clears the pointer
                if let Some(ft) = filter_tree {
                    let cur = hs.data.state_json().get("filter_tree").and_then(|t| t.get("saved_filter")).and_then(|v| v.as_str()).map(String::from);
                    if let Some(pair) = ft.get("__rename").and_then(|v| v.as_array()) { if cur.as_deref() == pair[0].as_str() { hs.data.save_pref("filter_tree", serde_json::json!({"saved_filter": pair[1]})); } }
                    if let Some(old) = ft.get("__delete").and_then(|v| v.as_str()) { if cur.as_deref() == Some(old) { hs.data.save_pref("filter_tree", serde_json::Value::Null); hs.dirty = true; } }
                }
                self.filter = Some(d);
            }
        }
    }

    /// Play or Pay, step one: ask the site (in a thread) which events run today and what the account picked.
    fn pop_start(&mut self, hs: &mut HomeState, ctx: &egui::Context) {
        let Some(acct) = pd_filter::account::load_account(&hs.data.root_dir()).filter(|a| !a.steam_id.is_empty()) else { self.pop_status = Some(("No active Steam account configured.".into(), false)); return };
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx2 = ctx.clone();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        let (y, m, d, _) = pd_filter::pagywosg_filter::civil(now);
        let today = format!("{y:04}-{m:02}-{d:02}");
        self.pop_rx = Some(rx);
        self.pop_plan = None;
        self.pop_status = Some(("Asking Play or Pay...".into(), true));
        std::thread::spawn(move || { let _ = tx.send(pd_filter::pop_sync::fetch_plan(&pd_filter::net::UreqHttp::new(), &acct.steam_id, &today)); ctx2.request_repaint(); });
    }

    /// Play or Pay, step two: tag the games and keep the evergreen "Play or Pay" filter in step.
    fn pop_apply(&mut self, hs: &mut HomeState, plan: pd_filter::pop_sync::Plan, confirm: Option<bool>) {
        use pd_filter::pop_sync::{apply, filter_entry, Outcome};
        let root = hs.data.root_dir();
        let Some(acct) = pd_filter::account::load_account(&root) else { return };
        match apply(&plan, &root, &acct.steam_id, &mut PopGroups(&mut hs.data), confirm) {
            Err(e) => self.pop_status = Some((e, false)),
            Ok(Outcome::Info(t)) => self.pop_status = Some((t, true)),
            Ok(Outcome::ConfirmCleanup { stale_tags }) => { self.pop_plan = Some((plan, stale_tags)); self.pop_status = None; }
            Ok(Outcome::Done { message, tags, .. }) => {
                let mut saved = hs.data.state_json().get("saved_filters").and_then(|v| v.as_object()).cloned().unwrap_or_default();
                let old_id = saved.get(pd_filter::pop_sync::FILTER_NAME).and_then(|e| e.get("id")).and_then(|v| v.as_str()).map(String::from);
                match filter_entry(&tags, old_id.as_deref(), filter_model::new_id) { Some(e) => { saved.insert(pd_filter::pop_sync::FILTER_NAME.into(), e); } None => { saved.shift_remove(pd_filter::pop_sync::FILTER_NAME); } }
                hs.data.save_pref("saved_filters", serde_json::Value::Object(saved));
                hs.dirty = true;
                self.pop_status = Some((message, true));
            }
        }
    }

    /// The Secret Santa gift list and the Monthly in a Month builder (opened from Community Tools).
    fn community_ui(&mut self, ui: &mut egui::Ui, hs: &mut HomeState) {
        if std::mem::take(&mut self.santa_open) && self.santa.is_none() { self.santa = Some(community_dialogs::SantaDialog::open(hs.data.db_path(), hs.data.root_dir(), hs.data.unique_csv("groups"))); }
        if std::mem::take(&mut self.blaeo_open) && self.blaeo.is_none() {
            if let Some(p) = self.blaeo_parked.take() { self.blaeo = Some(p); }   // the read that kept going after the dialog was closed
            else { match pd_filter::account::load_account(&hs.data.root_dir()).filter(|a| !a.steam_id.is_empty()) {
                Some(a) => self.blaeo = Some(community_dialogs::BlaeoDialog::open(hs.data.db_path(), hs.data.root_dir(), a.steam_id, ui.ctx())),
                None => hs.toast("No active Steam account configured."),
            } }
        }
        if let Some(p) = self.blaeo_parked.as_mut() { p.poll(ui.ctx()); }
        if std::mem::take(&mut self.sg_unmatched_open) && self.sg_unmatched.is_none() {
            match pd_filter::account::load_account(&hs.data.root_dir()).filter(|a| !a.steam_id.is_empty()) {
                Some(a) => self.sg_unmatched = Some(community_dialogs::SgUnmatchedDialog::open(hs.data.db_path(), hs.data.root_dir(), hs.data.steam_root(), a.steam_id)),
                None => hs.toast("No active Steam account configured."),
            }
        }
        if let Some(mut d) = self.sg_unmatched.take() {
            let out = d.show(ui.ctx());
            if out.changed { hs.data.refresh_info(); hs.dirty = true; }
            // a game added from Steam's local files gets its cover art next
            if !out.added.is_empty() { hs.start_sync(ui.ctx(), sync::Kind::Art, out.added.clone(), "Fetching art", None); }
            if !out.close { self.sg_unmatched = Some(d); }
        }
        if let Some(mut d) = self.blaeo.take() {
            let out = d.show(ui.ctx());
            if out.changed { hs.data.refresh_info(); hs.dirty = true; }
            if !out.close { self.blaeo = Some(d); }
            else if d.loading() { self.blaeo_parked = Some(d); }   // closed while reading: keep reading, and say so in the menu when it is done
        }
        if let Some(mut d) = self.santa.take() { if !d.show(ui.ctx()) { self.santa = Some(d); } }
        if std::mem::take(&mut self.miam_open) && self.miam.is_none() {
            let saved = hs.data.state_json().get("saved_filters").and_then(|v| v.as_object()).cloned().unwrap_or_default();
            self.miam = Some(community_dialogs::MiamDialog::open(hs.data.db_path(), saved, ui.ctx()));
        }
        if let Some(mut d) = self.miam.take() {
            let out = d.show(ui.ctx());
            if let Some(map) = out.saved { hs.data.save_pref("saved_filters", serde_json::Value::Object(map)); }
            if !out.close { self.miam = Some(d); }
        }
    }

    /// The PAGYWOSG filter builder (opened from Community Tools).
    fn pag_ui(&mut self, ui: &mut egui::Ui, hs: &mut HomeState) {
        if std::mem::take(&mut self.pag_open) && self.pag.is_none() {
            let st = hs.data.state_json();
            let env = pagywosg_dialog::PagEnv {
                db: hs.data.db_path(), root: hs.data.root_dir(), sg_user: pd_filter::account::sg_username(&hs.data.root_dir()),
                tags: hs.data.unique_csv("tags"), groups: hs.data.unique_csv("groups"),
                saved: st.get("saved_filters").and_then(|v| v.as_object()).cloned().unwrap_or_default(),
                sg_group: match st.get("pagywosg_sg_group") { None => None, Some(serde_json::Value::Null) => Some(None), Some(v) => Some(v.as_str().map(String::from)) },
                comp_defaults: st.get("pagywosg_comp_defaults").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default(),
            };
            self.pag = Some(pagywosg_dialog::PagDialog::open(env, ui.ctx()));
        }
        let Some(mut d) = self.pag.take() else { return };
        let preview = |tree: &serde_json::Value| -> String {
            let mut params = Vec::new();
            let sql = pd_filter::build_tree_sql(tree, &mut params, &pd_filter::Ctx::default());
            let mut it = params.iter();
            let mut out = String::from("WHERE ");
            for ch in if sql.is_empty() || sql == "1=1" { "1=1".to_string() } else { sql }.chars() {
                if ch == '?' { match it.next() { Some(serde_json::Value::String(v)) => { out.push('\''); out.push_str(&v.replace('\'', "''")); out.push('\''); } Some(v) => out.push_str(&v.to_string()), None => out.push('?') } } else { out.push(ch); }
            }
            out
        };
        let mut out = d.show(ui.ctx(), &preview);
        if self.pag_autosave && d.settled() && d.is_ready() { self.pag_autosave = false; d.save_now(&mut out); if let Some(n) = d.saved_name() { hs.data.save_pref("filter_tree", serde_json::json!({"saved_filter": n})); hs.dirty = true; } out.close = true; }
        for (k, v) in out.prefs { hs.data.save_pref(k, v); }
        if let Some(map) = out.saved { hs.data.save_pref("saved_filters", serde_json::Value::Object(map)); }
        if let Some(t) = out.toast { hs.toast(t); }
        if !out.close { self.pag = Some(d); }
    }

    /// The Bulk Operations dialog: open it from the toolbar, draw it, and carry out the edit or delete.
    fn bulk_ui(&mut self, ui: &mut egui::Ui, hs: &mut HomeState) {
        if std::mem::take(&mut self.bulk_open) && self.bulk.is_none() {
            let sugg = ["tags", "groups", "genres", "categories"].map(|c| hs.data.unique_csv(c));
            self.bulk = Some(bulk_dialog::BulkDialog::new(self.core.games.len(), self.lib_head.total.max(0) as usize, if self.select_mode { self.selected.len() } else { 0 }, sugg));
        }
        let Some(mut d) = self.bulk.take() else { return };
        if let Some(t) = self.bulk_tab_arg.take() { d.set_tab(t); }
        use bulk_dialog::{BulkOut as B, Scope};
        let ids = |s: &Self, sc: Scope| -> Option<Vec<i64>> { match sc { Scope::All => None, Scope::Filtered => Some(s.core.games.iter().map(|g| g.appid).collect()), Scope::Selected => Some(s.selected.iter().copied().collect()) } };
        // one bar for a whole Fetch Dates (the store part is a job, the Steam part runs in the browser): all its games count from the start, a finished part stays in the total
        let (store_now, steam_now) = (hs.sync.as_ref().map(|j| j.progress()).filter(|j| j.0 == "Purchase dates"), hs.steam_dates.as_ref().and_then(|s| s.progress()));
        if hs.dates_run.is_some() && store_now.is_none() && steam_now.is_none() { hs.dates_run = None; }
        d.job = match hs.dates_run {
            Some((other, steam)) if other + steam > 0 => {
                let other_done = store_now.as_ref().map_or(other, |j| j.2.min(other));
                let steam_done = steam_now.as_ref().map_or(steam, |j| j.2.min(steam));
                Some(("Dates".to_string(), other + steam, other_done + steam_done, store_now.and_then(|j| j.3)))
            }
            _ => match (hs.sync.as_ref().map(|j| j.progress()), steam_now) {
                (Some(j), _) => Some(j),
                (None, Some((l, t, n))) => Some((l, t, n, None)),
                (None, None) => None,
            },
        };
        match d.show(ui.ctx()) {
            B::None => self.bulk = Some(d),
            B::Close => {}
            B::PickGames => { self.select_mode = true; self.selected.clear(); }
            B::Apply { scope, column, mode, value } => {
                let list = ids(self, scope);
                match hs.data.bulk_edit(&column, &mode, &value, list.as_deref()) {
                    Ok(n) => { hs.toast(format!("Updated {n} games")); hs.dirty = true; d.error = None; }
                    Err(e) => { d.error = Some(e); }
                }
                self.bulk = Some(d);
            }
            // the Re-scrape tab stays open while a job runs, showing its progress; every button starts a job and keeps the dialog
            B::Rescrape { scope, meta, cheevo } => {
                let list = match ids(self, scope) { Some(l) => l, None => hs.data.all_appids() };
                hs.opt_meta = meta; hs.opt_cheevo = cheevo;
                hs.start_sync(ui.ctx(), sync::Kind::Rescrape, list, "Re-scraping", None);
                self.bulk = Some(d);
            }
            B::FetchDates { scope } => {
                let list = match ids(self, scope) { Some(l) => l, None => hs.data.all_appids() };
                let others: Vec<i64> = list.iter().copied().filter(|a| *a < 0).collect();
                let steam: Vec<i64> = list.into_iter().filter(|a| *a > 0).collect();
                if others.is_empty() && steam.is_empty() { hs.toast("No games in this selection"); }
                else {
                    // both at once: they touch different games (the browser part reports through the Companion, the store part is a background job)
                    hs.dates_run = Some((others.len(), steam.len()));
                    if !others.is_empty() { hs.start_sync(ui.ctx(), sync::Kind::PurchaseDates, others, "Purchase dates", None); }
                    if !steam.is_empty() { hs.steam_dates_run(&self.ctx.clone(), &steam, true); }
                }
                self.bulk = Some(d);
            }
            B::ProtonDb { scope } => {
                let list = match ids(self, scope) { Some(l) => l, None => hs.data.all_appids() };
                hs.start_sync(ui.ctx(), sync::Kind::ProtonDb, list.into_iter().filter(|a| *a > 0).collect(), "ProtonDB", None);
                self.bulk = Some(d);
            }
            B::Hltb { scope } => {
                let list = match ids(self, scope) { Some(l) => l, None => hs.data.all_appids() };
                hs.start_sync(ui.ctx(), sync::Kind::Hltb, list, "HowLongToBeat", None);
                self.bulk = Some(d);
            }
            B::Backfill { scope } => {
                let list = match ids(self, scope) { Some(l) => l, None => hs.data.all_appids() };
                let todo = hs.data.missing_data_appids(&list);
                if todo.is_empty() { hs.toast("Nothing is missing in these games"); } else { hs.start_sync(ui.ctx(), sync::Kind::Backfill, todo, "Filling", None); }
                self.bulk = Some(d);
            }
            B::Art { scope, types, source, keep_custom } => {
                let mut list = match ids(self, scope) { Some(l) => l, None => hs.data.all_appids() };
                use pd_filter::artclient::Kind as AK;
                hs.opt_art_kinds = [AK::Vertical, AK::Horizontal, AK::Icon].into_iter().zip(types).filter(|(_, on)| *on).map(|(k, _)| k).collect();
                if keep_custom {
                    let n = list.len();
                    list = artstore::without_custom(&hs.data.db_path(), &list, &hs.opt_art_kinds);
                    if list.len() < n { hs.toast(&format!("Skipping {} games whose cover you set yourself", n - list.len())); }
                }
                hs.opt_art_source = source;
                hs.start_sync(ui.ctx(), sync::Kind::Art, list, "Cover art", None);
                self.bulk = Some(d);
            }
            B::Stop => { if let Some(j) = &hs.sync { j.cancel(); } else if let Some(s) = &hs.steam_dates { s.cancel(); } self.bulk = Some(d); }
            B::Delete { scope } => {
                let list = ids(self, scope);
                match hs.data.bulk_delete(list.as_deref()) {
                    Ok(n) => { hs.toast(format!("Deleted {n} games")); hs.dirty = true; hs.data.library_changed(); if scope == Scope::Selected { self.select_mode = false; self.selected.clear(); } }
                    Err(e) => { d.error = Some(e); self.bulk = Some(d); }
                }
            }
        }
    }

    /// Apply a filter: the tree (when there is one), the libraries to show; the search box is cleared, as the web page does on a reload.
    fn apply_filter(&mut self, hs: &mut HomeState, tree: Option<serde_json::Value>, hidden: Vec<String>) {
        let saved_tree = tree.is_some();
        if let Some(t) = tree { hs.data.save_pref("filter_tree", t); }
        hs.data.save_pref("hidden_platforms", serde_json::json!(hidden));
        // the search is a row of the saved tree (the dialog showed it as "Game Title contains"): what remains of it stays in the box, and a tree
        // that has none (a preset, CLEAR ALL) empties it
        if saved_tree {
            self.search = hs.data.state_json().get("filter_tree").map(search_in_tree).unwrap_or_default();
            self.search_applied = self.search.clone();
        }
        self.search_edit_at = None;
        hs.dirty = true;
        let st = hs.data.state_json().clone();
        hs.pick.refresh_filter(&st);
    }

    /// Open a link from the Edit dialog: a web address in the browser, or `file://cover/<appid>` = the folder holding that game's vertical cover.
    fn open_link(&mut self, u: &str) {
        if let Some(rest) = u.strip_prefix("file://cover/") {
            let (kind, id) = rest.split_once('/').unwrap_or(("vertical", rest));
            let sub = if kind == "icon" { "icons" } else { kind };
            let dir = self.root.join("static/img/library").join(sub);
            let file = dir.join(format!("{id}.jpg"));
            if file.exists() { reveal_file(&file); } else { open_url(&(if dir.exists() { dir } else { self.root.join("static/img/library") }).to_string_lossy()); }
        } else if u.starts_with("https://") { open_url(u); }
    }

    /// The Edit Game dialog: open it when asked (context menu, hover button), draw it, and carry out what it returns.
    fn edit_ui(&mut self, ui: &mut egui::Ui, hs: &mut HomeState, start_on_info: bool) {
        if std::mem::take(&mut hs.live_arrived) { if let Some(d) = self.edit.as_mut() { if d.quals.is_none() { d.quals = hs.data.pag_panel_edit(d.appid); } } if let Some(d) = self.list_edit.as_mut() { if d.quals.is_none() { d.quals = hs.data.pag_panel_edit(d.appid); } } }
        self.edit_ui_inner(ui, hs, start_on_info);
        self.art_tools_ui(ui, hs);
    }

    /// Load a game's list icon into the cache (a missing file is remembered as None, so it is not read again every frame).
    fn list_icon_load(&mut self, ctx: &egui::Context, appid: i64) {
        if self.list_icons.contains_key(&appid) { return; }
        let path = self.root.join(format!("static/img/library/icons/{appid}.jpg"));
        let tex = image::open(&path).ok().map(|im| { let im = im.thumbnail(64, 64).to_rgba8(); ctx.load_texture(format!("li{appid}"), egui::ColorImage::from_rgba_unmultiplied([im.width() as usize, im.height() as usize], im.as_raw()), egui::TextureOptions::LINEAR) });
        self.list_icons.insert(appid, tex);
    }

    /// The Crop dialog and the SteamGridDB picker, which open over the Edit dialog.
    fn art_tools_ui(&mut self, ui: &mut egui::Ui, hs: &mut HomeState) {
        use pd_filter::artclient::Kind as AK;
        if let Some(mut d) = self.crop.take() {
            ui_kit::STACKED.store(true, std::sync::atomic::Ordering::Relaxed);
            match d.show(ui.ctx()) {
                crop_dialog::CropOut::None => self.crop = Some(d),
                crop_dialog::CropOut::Cancel => {}
                crop_dialog::CropOut::Apply { x, y, w } => {
                    let store = artstore::FsStore { root: hs.data.root_dir(), db: hs.data.db_path(), stage: true };   // staged: the cropped picture is shown in the dialog and saved with it
                    match store.crop([AK::Vertical, AK::Horizontal, AK::Icon][d.kind], d.appid, x, y, w) {
                        Ok(_) => { hs.toast("Cropped: SAVE keeps it, CLOSE drops it"); if let Some(e) = self.edit.as_mut() { e.art_changed(); } if let Some(e) = self.list_edit.as_mut() { e.art_changed(); } }
                        Err(e) => hs.toast(e),
                    }
                }
            }
        }
        if let Some(mut p) = self.picker.take() {
            ui_kit::STACKED.store(true, std::sync::atomic::Ordering::Relaxed);
            match p.show(ui.ctx()) {
                sgdb_picker::PickOut::None => self.picker = Some(p),
                sgdb_picker::PickOut::Close => {}
                sgdb_picker::PickOut::Use(url) => { hs.art_op(&self.ctx, p.appid, p.kind, ArtAct::Url(url), "Art"); }
            }
        }
    }

    fn edit_ui_inner(&mut self, ui: &mut egui::Ui, hs: &mut HomeState, start_on_info: bool) {
        if let Some(id) = hs.open_edit.take() {
            if let Some(info) = hs.data.load_edit(id) { let mut d = edit_dialog::EditDialog::new(id, info, start_on_info); d.account_id = hs.data.account_id.clone(); d.db = Some(hs.data.db_path()); d.root = hs.data.root_dir(); d.dup = hs.data.dup_info(id); d.quals = hs.data.pag_panel_edit(id); d.store_text = hs.store_link_text(id);
                // the art tab matching the Library's view (horizontal covers when the Library shows them), else the one this game's dialog was just on
                let by_view = if hs.data.state_json().get("artwork_orientation").and_then(|v| v.as_str()) == Some("horizontal") { 1 } else { 0 };
                d.set_art_kind(match hs.edit_art { Some((a, k)) if a == id => k, _ => by_view });
                d.art_changed();   // a picture staged before this dialog was rebuilt (another button reopens it) is still shown
                self.edit = Some(d); }
            else { hs.toast("That game is not in the library any more"); }
        }
        // a date the browser just read for the game in the open Edit dialog: show it in the field (the dialog holds its own copy, and its SAVE would write the old one back)
        if let Some(sd) = hs.steam_dates.clone() { for (a, dt) in sd.take_written() { if let Some(d) = self.edit.as_mut() { if d.appid == a { d.f.date_added = dt; } } } }
        let Some(mut d) = self.edit.take() else { return };
        if d.dup_query != d.dup_query_done { d.dup_results = hs.data.dup_search(&d.dup_query, d.appid); d.dup_query_done = d.dup_query.clone(); }
        let out = d.show(ui.ctx(), &mut hs.covers);
        if hs.art_staged_for == Some(d.appid) { hs.art_staged_for = None; d.art_changed(); self.ctx.request_repaint(); }   // the next frame loads the new picture: without this nudge it waited for the next mouse move   // a new picture was staged: show it
        hs.edit_art = Some((d.appid, d.art_kind()));
        use edit_dialog::EditOut as O;
        match out {
            O::None => self.edit = Some(d),
            O::Close => { hs.edit_art = None; artstore::discard(&hs.data.root_dir(), d.appid); }   // closing drops any picture the art tabs staged
            O::Open(u) => { match u.strip_prefix("zest://steam-date/").and_then(|i| i.parse::<i64>().ok()) { Some(id) => hs.steam_dates_run(&self.ctx.clone(), &[id], false), None => self.open_link(&u) } self.edit = Some(d); }
            O::DupLink(t) => {
                match hs.data.dup_link(d.appid, t) { Ok(()) => { hs.dirty = true; d.dup_query.clear(); d.dup_query_done.clear(); d.dup = hs.data.dup_info(d.appid); hs.toast("Linked: the platform order decides which copy is shown"); } Err(e) => hs.toast(format!("Could not link: {e}")) }
                self.edit = Some(d);
            }
            O::DupApprove(t) => {
                match hs.data.dup_approve(&[(d.appid, t)]) { Ok(()) => { hs.dirty = true; d.dup = hs.data.dup_info(d.appid); } Err(e) => hs.toast(format!("Could not link: {e}")) }
                self.edit = Some(d);
            }
            O::DupUnlink => {
                match hs.data.dup_unlink(d.appid) { Ok(()) => { hs.dirty = true; d.dup = hs.data.dup_info(d.appid); hs.toast("Unlinked: it will not be linked automatically again"); } Err(e) => hs.toast(format!("Could not unlink: {e}")) }
                self.edit = Some(d);
            }
            O::DupReject => {
                match hs.data.dup_reject(&[d.appid]) { Ok(()) => { d.dup = hs.data.dup_info(d.appid); } Err(e) => hs.toast(format!("Could not save: {e}")) }
                self.edit = Some(d);
            }
            O::Toast(t) => { hs.toast(t); self.edit = Some(d); }
            O::SyncSteam => { hs.start_sync(&self.ctx, sync::Kind::Rescrape, vec![d.appid], "Syncing", Some(d.appid)); self.edit = Some(d); }
            O::ArtRescrape(k) => { hs.art_op(&self.ctx, d.appid, k, ArtAct::Order, "Art"); self.edit = Some(d); }
            O::ArtFromSteam(k) => { hs.art_op(&self.ctx, d.appid, k, ArtAct::FromSteam, "Art from Steam"); self.edit = Some(d); }
            O::ArtUrl(k, u) => { hs.art_op(&self.ctx, d.appid, k, ArtAct::Url(u), "Art"); self.edit = Some(d); }
            O::ArtCrop(k) => {
                match crop_dialog::CropDialog::open(&self.ctx, &hs.data.root_dir(), d.appid, k) { Some(c) => self.crop = Some(c), None => hs.toast("This game has no image to crop") }
                self.edit = Some(d);
            }
            O::ArtBrowse(k) => {
                match pd_filter::account::sgdb_key(&hs.data.root_dir()) {
                    Some(key) => {
                        let art = std::sync::Arc::new(pd_filter::artclient::Art::new(std::sync::Arc::new(pd_filter::net::UreqHttp::new()), std::sync::Arc::new(artstore::FsStore { root: hs.data.root_dir(), db: hs.data.db_path(), stage: false }), std::sync::Arc::new(artstore::DbHooks::new(hs.data.db_path(), None)), Some(key)));
                        self.picker = Some(sgdb_picker::SgdbPicker::open(&self.ctx, art, d.appid, k, d.title_for_search()));
                    }
                    None => open_url(&d.sgdb_page()),
                }
                self.edit = Some(d);
            }
            O::ArtClear(k) => {
                // staged like any other art change: the picture leaves when the dialog is saved, and a CLOSE brings it back
                artstore::stage_clear(&hs.data.root_dir(), artstore::KINDS[k], d.appid); hs.toast("Art cleared: SAVE keeps that, CLOSE brings it back"); d.art_changed();
                self.edit = Some(d);
            }
            O::FillMissing => { hs.start_sync(&self.ctx, sync::Kind::Backfill, vec![d.appid], "Filling", Some(d.appid)); self.edit = Some(d); }
            O::ProtondbRefresh => { hs.start_sync(&self.ctx, sync::Kind::ProtonDb, vec![d.appid], "ProtonDB", Some(d.appid)); self.edit = Some(d); }
            O::HltbApplied => { let mut d = d; if let Some(i) = hs.data.load_edit(d.appid) { d.refresh_hltb(&i); } hs.dirty = true; self.edit = Some(d); }
            O::HltbRescrape => { hs.start_sync(&self.ctx, sync::Kind::Hltb, vec![d.appid], "HLTB", Some(d.appid)); self.edit = Some(d); }
            O::HltbNone => {
                let r = rusqlite::Connection::open(hs.data.db_path()).map_err(|e| e.to_string()).and_then(|c| pd_filter::hltb_ops::clear(&c, d.appid));
                match r { Ok(()) => { hs.toast("HLTB match removed"); hs.open_edit = Some(d.appid); } Err(e) => { hs.toast(format!("Could not clear it: {e}")); self.edit = Some(d); } }
            }
            O::Store => { if d.is_steam() { open_url(&format!("https://store.steampowered.com/app/{}", d.appid)); } else { hs.run_action("store", d.appid, (0.0, 0.0)); } self.edit = Some(d); }
            O::OpenFolder => {
                match d.install_path().filter(|p| std::path::Path::new(p).exists()).map(String::from) { Some(p) => open_url(&p), None => hs.toast("This game is not installed") }
                self.edit = Some(d);
            }
            O::ChangeExecutable => {
                let dir = d.install_path().filter(|p| std::path::Path::new(p).is_dir()).map(String::from);
                match dir {
                    Some(p) => {
                        let c = pd_filter::native_exe::all_candidates(std::path::Path::new(&p), pd_filter::native_exe::Os::current());
                        if c.is_empty() { hs.toast("No program that could start this game was found in its folder"); } else { hs.exe_pick = Some((d.appid, d.name_for_toast(), false, c)); }
                    }
                    None => hs.toast("This game is not installed"),
                }
                self.edit = Some(d);
            }
            O::Delete => { hs.confirm = Some(d.appid); }
            O::Save => match hs.data.save_edit(d.appid, &d.old, &d.f) {
                Ok(()) => { hs.dirty = true; hs.data.library_changed(); hs.commit_art(d.appid); }
                Err(e) => { d.status = Some(format!("Could not save: {e}")); self.edit = Some(d); }
            },
        }
    }

    /// Open what the menu chose, and draw the open dialog.
    fn modal_ui(&mut self, ui: &mut egui::Ui, hs: &mut HomeState) {
        if let Some(a) = self.menu_action.take() {
            match a {
                "discord" => open_url("https://discord.gg/ESqXvFWkFe"),
                "blaeo_ready" => self.blaeo_open = true,
                "tutorial" => self.modal = Some(Modal::Tutorial(Box::new(tutorial_dialog::TutorialDialog::new()))),
                "update" => { let beta = hs.data.state_bool("beta_updates", pd_filter::updater::VERSION.contains('-')); let auto = hs.data.state_bool("check_updates", true); self.modal = Some(Modal::Update(Box::new(update_dialog::UpdateDialog::new(&ui.ctx().clone(), hs.data.root_dir(), beta, auto)))); }
                "hltb" => { let thr = hs.data.state_f64("hltb_match_threshold", 75.0) as i64; self.modal = Some(Modal::Hltb(Box::new(hltb_dialog::HltbDialog::new(&ui.ctx().clone(), hs.data.db_path(), thr)))); }
                "send_log" => self.modal = Some(Modal::SendLog(Box::new(sendlog_dialog::SendLogDialog::new(&ui.ctx().clone(), hs.data.root_dir(), hs.data.db_path(), hs.data.state_f64("last_log_submit_at", 0.0))))),
                "data" => self.modal = Some(Modal::Data(Box::new(data_dialog::DataDialog::new(&ui.ctx().clone(), hs.data.root_dir(), hs.data.db_path(), hs.data.csv_filter_query(), &hs.data.state_json().get("saved_filters").and_then(|v| v.as_object()).cloned().unwrap_or_default())))),
                "add_game" => self.modal = Some(Modal::AddGame(Box::new(add_game_dialog::AddGameDialog::new(&ui.ctx().clone(), hs.data.db_path())))),
                "edit_home" => {
                    let shelves: Vec<serde_json::Value> = hs.data.state_json().get("shelves").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                    let filters: Vec<(String, String)> = settings_dialog::OUTLINE_PRESETS.iter().filter(|(k, _)| *k != "beaten_only").map(|(k, l)| (k.to_string(), l.to_string()))
                        .chain(hs.data.state_json().get("saved_filters").and_then(|v| v.as_object()).map(|m| { let mut n: Vec<String> = m.keys().cloned().collect(); n.sort_by_key(|x| x.to_lowercase()); n }).unwrap_or_default().into_iter().map(|n| (n.clone(), format!("\u{2605} {n}")))).collect();
                    let platforms: Vec<(String, String)> = hs.data.platforms_present().into_iter().map(|p| { let l = plat_label_of(&p); (p, l) }).collect();
                    self.modal = Some(Modal::HomeEdit(Box::new(home_edit_ui::HomeEditor::new(shelves, (hs.size.0 as i32, hs.size.1 as i32), (home::pie_slices(&hs.data, "completion_pie"), home::pie_slices(&hs.data, "achievement_pie")), filters, platforms))));
                }
                "playnite" => self.modal = Some(Modal::Playnite(Box::new(playnite_dialog::PlayniteDialog::new(hs.data.db_path(), hs.data.root_dir())))),
                "emulators" => self.modal = Some(Modal::Emulators(Box::new(emulators_dialog::EmulatorsDialog::new(&ui.ctx().clone(), hs.data.root_dir(), hs.data.db_path())))),
                "community" => self.modal = Some(Modal::Community),
                "junk" => self.modal = Some(Modal::Junk(Box::new(junk_dialog::JunkDialog::new(&ui.ctx().clone(), hs.data.db_path(), hs.data.root_dir())))),
                "blacklist" => self.modal = Some(Modal::Blacklist { entries: hs.data.blacklist(), query: String::new() }),
                "duplicates" => self.modal = Some(Modal::Duplicates { items: hs.data.dup_suggestions().into_iter().map(|s| (s, false)).collect() }),
                "populate" => { let ctx = ui.ctx().clone(); hs.start_sync(&ctx, sync::Kind::Populate, vec![0], "Adding new games", None); }
                "settings" => self.modal = Some(Modal::Settings(Box::new(settings_dialog::SettingsDialog::open(hs.data.root_dir(), hs.data.state_json(), hs.data.priority_platforms()).with_db(hs.data.db_path()).with_art_platforms(hs.data.priority_platforms().into_iter().map(|p| { let l = hs.sources.label(&p); let st = p != "steam" && hs.sources.caps(&p).art; (p, l, st) }).collect())))),
                "plugins" => { let mut d0 = Box::new(plugins_dialog::PluginsDialog::open(&ui.ctx().clone(), hs.sources.all().iter().filter(|s| s.id() != "steam").map(|s| plugins_dialog::Entry { src: s.clone(), plugin: hs.plugins.iter().find(|p| p.manifest().platform == s.id()).cloned() }).collect(), hs.plugin_errors.clone(), pd_filter::plugin::manifest::discover_legacy(&hs.data.root_dir().join("plugins")), hs.data.root_dir().join("plugins").to_string_lossy().into_owned())); d0.startup = pd_filter::startup_sync::enabled(hs.data.state_json()); self.modal = Some(Modal::Plugins(d0)); }
                key => { if let Some((_, label)) = MENU.iter().find(|m| m.0 == key) { self.modal = Some(Modal::Placeholder(label)); } }
            }
        }
        // a sign-in was just made with no keyring and no answer yet: ask where sign-ins go (Settings > Account changes it later)
        if self.modal.is_none() && pd_filter::secrets::choice_pending() { self.modal = Some(Modal::KeyringChoice); }
        // the first start of a new version: the changelog sections newer than the last one shown, once. A first-ever start shows the running version's notes
        // when the library was copied from PlayDate (what changed), else the tutorial (it opens with the welcome)
        if !std::mem::replace(&mut self.whats_new_checked, true) && self.modal.is_none() {
            let running = pd_filter::updater::VERSION;
            let seen = hs.data.state_json().get("whats_new_seen").and_then(|v| v.as_str()).map(String::from);
            if seen.is_none() {
                let from_playdate = hs.data.state_json().get(pd_filter::import_playdate::FROM_PLAYDATE_KEY).and_then(|v| v.as_bool()) == Some(true);
                let run_t = pd_filter::whats_new::version_tuple(running);
                let sections: Vec<_> = pd_filter::whats_new::parse(pd_filter::whats_new::CHANGELOG).into_iter().filter(|s| pd_filter::whats_new::version_tuple(&s.version) == run_t).collect();
                if from_playdate && !sections.is_empty() { self.modal = Some(Modal::WhatsNew { sections }); }
                else { hs.data.save_pref("whats_new_seen", serde_json::json!(running)); self.modal = Some(Modal::Tutorial(Box::new(tutorial_dialog::TutorialDialog::new()))); }
            } else if seen.as_deref() != Some(running) {
                let sections = pd_filter::whats_new::due(&pd_filter::whats_new::parse(pd_filter::whats_new::CHANGELOG), seen.as_deref(), running);
                if sections.is_empty() { hs.data.save_pref("whats_new_seen", serde_json::json!(running)); } else { self.modal = Some(Modal::WhatsNew { sections }); }
            }
        }
        let Some(m) = self.modal.take() else { return };
        let accent = egui::Color32::from_rgb(self.theme.accent.0, self.theme.accent.1, self.theme.accent.2);
        let ctx = ui.ctx().clone();
        self.modal = match m {
            Modal::Settings(mut d) => {
                let (out, url) = d.show(&ctx);
                if let Some(u) = url { open_url(&u); }
                match out {
                    settings_dialog::SettingsOut::None => Some(Modal::Settings(d)),
                    settings_dialog::SettingsOut::Close => None,
                    settings_dialog::SettingsOut::EditHome => { self.menu_action = Some("edit_home"); None }
                    settings_dialog::SettingsOut::Menu(k) => { self.menu_action = Some(k); None }
                    settings_dialog::SettingsOut::Priority(order) => {
                        match hs.data.save_priority(order) {
                            Ok(n) => { hs.dirty = true; hs.data.refresh_info(); d.priority_saved(if n == 0 { "Saved. No duplicate links needed to move.".to_string() } else { format!("Saved. {n} duplicate link{} moved.", if n == 1 { "" } else { "s" }) }); }
                            Err(e) => d.priority_saved(format!("Could not save: {e}")),
                        }
                        Some(Modal::Settings(d))
                    }
                    settings_dialog::SettingsOut::Appearance(a) => {
                        use appearance::AppearanceOut as A;
                        match a {
                            A::Theme | A::Background => {
                                let root = hs.data.root_dir();
                                self.theme = theme::Theme::load(&root);
                                let c = |t: (u8, u8, u8)| egui::Color32::from_rgb(t.0, t.1, t.2);
                                ui_kit::set_from_theme(&self.theme);
                                hs.accent = c(self.theme.accent);
                                self.bg = theme::Background::load(&root, &self.theme);
                                hs.base = None; self.layer = Layer::new();   // the pages keep a finished copy of the background and panels: drop it so the change shows at once
                                hs.dirty = true;
                            }
                            A::Live(vars) => {
                                // the Theme Editor's draft, shown at once and not saved (closing the editor reloads the saved theme)
                                self.theme = theme::Theme::from_pairs(&vars);
                                ui_kit::set_from_theme(&self.theme);
                                hs.accent = egui::Color32::from_rgb(self.theme.accent.0, self.theme.accent.1, self.theme.accent.2);
                                self.bg.restyle(theme::pack(self.theme.bg_page), self.theme.bg_image_opacity);   // the page color and the picture's opacity follow while a slider moves
                                // not `hs.dirty`: that reloads the whole game list from the database, which a color or an opacity step does not need and made the slider crawl
                                hs.base = None; self.layer = Layer::new();
                            }
                            A::Prewarm => { self.bg.prewarm(); }
                            A::Scale(p) => { ui_kit::set_scale(p); hs.data.save_pref("ui_scale", serde_json::json!(p)); }
                            A::None => {}
                        }
                        Some(Modal::Settings(d))
                    }
                    settings_dialog::SettingsOut::Apply { updates, tags, reviews, section } => {
                        let rules_changed = updates.iter().any(|(k, _)| k == "card_click_rules");
                        let list_changed = updates.iter().any(|(k, _)| k == "hide_duplicates");
                        let badges_changed = updates.iter().any(|(k, _)| k == "card_badges");
                        let outlines_changed = updates.iter().any(|(k, _)| k == "card_outlines");
                        let shelves_changed = updates.iter().any(|(k, _)| k == "shelves");
                        for (k, v) in updates { hs.data.save_pref(&k, v); }
                        if rules_changed { hs.engine.set_rules(card_actions::rules_from_state(hs.data.state_json())); }
                        let mut msg = "Saved.".to_string();
                        // a cached derived value follows its setting: the weighted review score and the tag similarity are rewritten for the whole library
                        if reviews {
                            let half = hs.data.state_f64("review_half_trust_count", 10.0);
                            msg = match rusqlite::Connection::open(hs.data.db_path()).and_then(|mut c| { c.busy_timeout(std::time::Duration::from_secs(10)).ok(); pd_filter::recalc::weighted_scores(&mut c, half) }) {
                                Ok(n) => format!("Saved; {n} games rescored."), Err(e) => format!("Saved, but rescoring failed: {e}") };
                        }
                        if tags {
                            msg = match hs.data.recalculate_tag_similarity() { Ok(n) => format!("Saved; {n} games rescored."), Err(e) => format!("Saved, but rescoring failed: {e}") };
                        }
                        if badges_changed { hs.data.compute_badges(); }
                        if outlines_changed { hs.data.compute_outlines(); }
                        if shelves_changed { match home::load(&hs.data.root_dir()) { Ok(d) => { hs.data = d; hs.data.compute_outlines(); } Err(e) => hs.toast(format!("Could not rebuild Home: {e}")) } }
                        if tags || reviews || list_changed || badges_changed || outlines_changed { hs.dirty = true; }
                        if let Some(sec) = section { d.saved(sec, msg); }
                        Some(Modal::Settings(d))
                    }
                }
            }
            Modal::Plugins(mut d) => {
                match d.show(&ctx, &hs.sync_status, hs.sync.as_ref().map(|j| j.progress()), hs.companion_gen) {
                    plugins_dialog::PluginsOut::None => Some(Modal::Plugins(d)),
                    plugins_dialog::PluginsOut::Close => None,
                    plugins_dialog::PluginsOut::OpenUrl(u) => { open_url(&u); Some(Modal::Plugins(d)) }
                    plugins_dialog::PluginsOut::Companion(service) => { hs.companion_open(&self.ctx, &service); Some(Modal::Plugins(d)) }
                    plugins_dialog::PluginsOut::SetGamesDir(plugin, path) => {
                        match pd_filter::installdir::set_games_dir(&hs.data.root_dir(), &plugin, &path) {
                            Ok(Some(p)) => hs.toast(format!("Games folder set to {}", p.display())),
                            Ok(None) => hs.toast("Games folder reset to the default"),
                            Err(e) => hs.toast(e),
                        }
                        d.folder_changed();
                        Some(Modal::Plugins(d))
                    }
                    plugins_dialog::PluginsOut::StartupSync(platform, on) => {
                        let list = pd_filter::startup_sync::toggled(hs.data.state_json(), &platform, on);
                        hs.data.save_pref(pd_filter::startup_sync::KEY, list.clone());
                        d.startup = pd_filter::startup_sync::enabled(hs.data.state_json());
                        Some(Modal::Plugins(d))
                    }
                    plugins_dialog::PluginsOut::XboxMode(mode) => {
                        let _ = std::fs::remove_file(pd_filter::xbox_owned::pending_path(&hs.data.root_dir()));
                        pd_filter::xbox_owned::request(match mode.as_str() { "group" => pd_filter::xbox_owned::Mode::Group, "replace" => pd_filter::xbox_owned::Mode::Replace, _ => pd_filter::xbox_owned::Mode::Add }, false);
                        hs.start_sync_with(&self.ctx, sync::Kind::XboxOwned, None, Some("xbox".to_string()), vec![0], "Reading your owned Xbox games", None);
                        Some(Modal::Plugins(d))
                    }
                    plugins_dialog::PluginsOut::XboxApprove => {
                        // the list was approved just now: run the import again (it asks Nexus afresh) and delete what is still not on it
                        pd_filter::xbox_owned::request(pd_filter::xbox_owned::Mode::Replace, true);
                        hs.start_sync_with(&self.ctx, sync::Kind::XboxOwned, None, Some("xbox".to_string()), vec![0], "Removing the Xbox games Nexus does not report", None);
                        Some(Modal::Plugins(d))
                    }
                    plugins_dialog::PluginsOut::XboxKeep => {
                        let _ = std::fs::remove_file(pd_filter::xbox_owned::pending_path(&hs.data.root_dir()));
                        Some(Modal::Plugins(d))
                    }
                    plugins_dialog::PluginsOut::OpenFolder => { let f = hs.data.root_dir().join("plugins"); let _ = std::fs::create_dir_all(&f); open_url(&f.to_string_lossy()); Some(Modal::Plugins(d)) }
                    plugins_dialog::PluginsOut::InstallLauncher(platform) => { hs.start_sync_with(&self.ctx, sync::Kind::LauncherInstall, None, Some(platform.clone()), vec![0], &format!("Installing the {} launcher", hs.sources.label(&platform)), None); Some(Modal::Plugins(d)) }
                    plugins_dialog::PluginsOut::StartLauncher(platform) => {
                        match pd_filter::launcher_installer::start_launcher(&hs.data.root_dir(), &platform) { Ok(act) => hs.do_action(act), Err(e) => hs.toast(e) }
                        Some(Modal::Plugins(d))
                    }
                    plugins_dialog::PluginsOut::UninstallLauncher(platform) => {
                        // a prefix can hold several gigabytes of games: removed off the window's thread, the result arrives as a message
                        let (root, q, wake, label) = (hs.data.root_dir(), hs.late_notices.clone(), hs.companion_ctx.clone(), hs.sources.label(&platform).to_string());
                        hs.toast(format!("Uninstalling the {label} launcher..."));
                        std::thread::spawn(move || {
                            let msg = match pd_filter::launcher_installer::uninstall_launcher(&root, &platform) { Ok(()) => format!("The {label} launcher is uninstalled"), Err(e) => e };
                            pd_filter::net::log(&format!("launcher uninstall {platform}: {msg}"));
                            if let Ok(mut v) = q.lock() { v.push(msg); } if let Some(c) = &wake { c.request_repaint(); }
                        });
                        Some(Modal::Plugins(d))
                    }
                    plugins_dialog::PluginsOut::Sync(platform) => { hs.start_sync_with(&self.ctx, sync::Kind::PluginSync, None, Some(platform.clone()), vec![0], &format!("Syncing {}", hs.sources.label(&platform)), None); Some(Modal::Plugins(d)) }
                }
            }
            Modal::Community => {
                // Play or Pay: the thread's answer, then the group edits on the library
                if let Some(rx) = &self.pop_rx {
                    match rx.try_recv() {
                        Ok(Ok(plan)) => { self.pop_rx = None; self.pop_apply(hs, plan, None); }
                        Ok(Err(e)) => { self.pop_rx = None; self.pop_status = Some((e, false)); }
                        Err(std::sync::mpsc::TryRecvError::Empty) => { ctx.request_repaint_after(Duration::from_millis(200)); }
                        Err(_) => { self.pop_rx = None; self.pop_status = Some(("the sync stopped unexpectedly".into(), false)); }
                    }
                }
                let mut pick = None;
                let busy = self.pop_rx.is_some();
                let confirm = self.pop_plan.as_ref().map(|p| p.1.clone());
                let status = self.pop_status.clone();
                let out = ui_kit::modal(&ctx, "community", "Community", 26.0, |ui, r| {
                    ui.vertical(|ui| {
                        for (key, label) in [("pag", "PAGYWOSG Filter Builder"), ("miam", "Monthly in a Month Filter Builder"), ("santa", "Secret Santa / Snowballs")] {
                            if ui_kit::button(ui, label, ui_kit::Kind::Plain, accent, r).clicked() { pick = Some(key); }
                        }
                        ui.add_space(0.6 * r);
                        ui.label(egui::RichText::new("BLAEO").size(0.78 * r).color(ui_kit::MUTED()));
                        if ui_kit::button(ui, "Sync BLAEO", ui_kit::Kind::Plain, accent, r).clicked() { pick = Some("blaeo"); }
                        ui.add_space(0.6 * r);
                        ui.label(egui::RichText::new("STEAMGIFTS").size(0.78 * r).color(ui_kit::MUTED()));
                        if ui_kit::button(ui, "Sync SteamGifts Wins", ui_kit::Kind::Plain, accent, r).clicked() { pick = Some("sg"); }
                        if ui_kit::button(ui, "Full refresh (re-check every win, drop stale ones)", ui_kit::Kind::Plain, accent, r).clicked() { pick = Some("sg_full"); }
                        if ui_kit::button(ui, "Wins not in your library...", ui_kit::Kind::Plain, accent, r).clicked() { pick = Some("sg_unmatched"); }
                        ui.label(ui_kit::muted("Reads your won giveaways in your own browser (needs the Zest Companion userscript).", 0.78 * r));
                        ui.add_space(0.6 * r);
                        ui.label(egui::RichText::new("PLAY OR PAY").size(0.78 * r).color(ui_kit::MUTED()));
                        if ui_kit::button(ui, if busy { "Syncing..." } else { "Sync Play or Pay Picks" }, ui_kit::Kind::Plain, accent, r).clicked() && !busy { pick = Some("pop"); }
                        if let Some(tags) = &confirm {
                            ui.label(ui_kit::muted(format!("A new cycle started. Remove the groups of the earlier one ({})?", tags.join(", ")), 0.82 * r));
                            ui.horizontal(|ui| {
                                if ui_kit::button(ui, "Remove them", ui_kit::Kind::Danger, accent, r).clicked() { pick = Some("pop_yes"); }
                                if ui_kit::button(ui, "Keep them", ui_kit::Kind::Plain, accent, r).clicked() { pick = Some("pop_no"); }
                            });
                        }
                        if let Some((t, good)) = &status { ui.label(egui::RichText::new(t).size(0.85 * r).color(if *good { egui::Color32::from_rgb(0x5c, 0xb8, 0x5c) } else { egui::Color32::from_rgb(0xe0, 0x6c, 0x6c) })); }
                        ui.add_space(0.6 * r);
                        
                    });
                });
                match pick {
                    Some("pag") => { self.pag_open = true; None }
                    Some("miam") => { self.miam_open = true; None }
                    Some("santa") => { self.santa_open = true; None }
                    Some("blaeo") => { self.blaeo_open = true; None }
                    Some("sg_unmatched") => { self.sg_unmatched_open = true; None }
                    Some(k @ ("sg" | "sg_full")) => { hs.companion_open(&ctx, if k == "sg" { "steamgifts" } else { "steamgifts-full" }); Some(Modal::Community) }
                    Some("pop") => { self.pop_start(hs, &ctx); Some(Modal::Community) }
                    Some(k @ ("pop_yes" | "pop_no")) => { if let Some((plan, _)) = self.pop_plan.take() { self.pop_apply(hs, plan, Some(k == "pop_yes")); } Some(Modal::Community) }
                    _ => if out.closed { None } else { Some(Modal::Community) },
                }
            }
            Modal::Tutorial(mut d) => {
                let accent = hs.accent;
                match d.show(&ctx, accent) {
                    tutorial_dialog::TutOut::None => Some(Modal::Tutorial(d)),
                    tutorial_dialog::TutOut::Close => None,
                    // a link into a dialog: the tutorial closes and that dialog opens (the same path as choosing it in the menu)
                    tutorial_dialog::TutOut::Open(k) => { self.menu_action = Some(k); None }
                }
            }
            Modal::Update(mut d) => {
                let accent = hs.accent;
                match d.show(&ctx, accent) {
                    update_dialog::UpdateOut::None => Some(Modal::Update(d)),
                    update_dialog::UpdateOut::Close => None,
                    update_dialog::UpdateOut::Pref(k, v) => { hs.data.save_pref(k, serde_json::json!(v)); Some(Modal::Update(d)) }
                    update_dialog::UpdateOut::Open(u) => { open_url(&u); Some(Modal::Update(d)) }
                    update_dialog::UpdateOut::Restart => { self.want_exit = true; None }
                    update_dialog::UpdateOut::RestartFlatpak => { relaunch_flatpak(); self.want_exit = true; None }
                }
            }
            Modal::Hltb(mut d) => {
                let accent = hs.accent;
                match d.show(&ctx, accent, hs.sync.is_some()) {
                    hltb_dialog::HltbOut::None => Some(Modal::Hltb(d)),
                    hltb_dialog::HltbOut::Close => None,
                    hltb_dialog::HltbOut::ConfirmAll(ids) => { let n = ids.len(); hs.start_sync(&ctx, sync::Kind::HltbConfirm, ids, &format!("HLTB: confirming {n} matches"), None); Some(Modal::Hltb(d)) }
                    hltb_dialog::HltbOut::Rescrape(ids) => { let n = ids.len(); hs.start_sync(&ctx, sync::Kind::Hltb, ids, &format!("HLTB: looking up {n} games"), None); Some(Modal::Hltb(d)) }
                }
            }
            Modal::HomeEdit(mut d) => {
                match d.show(&ctx) {
                    home_edit_ui::EditorOut::None => Some(Modal::HomeEdit(d)),
                    home_edit_ui::EditorOut::Close => None,
                    home_edit_ui::EditorOut::Save(v) => {
                        hs.data.save_pref("shelves", serde_json::Value::Array(v));
                        match home::load(&hs.data.root_dir()) { Ok(nd) => { hs.data = nd; hs.data.compute_outlines(); hs.dirty = true; hs.shelf_pos.clear(); hs.toast("Home layout saved"); } Err(e) => hs.toast(format!("Could not rebuild Home: {e}")) }
                        None
                    }
                }
            }
            Modal::Playnite(mut d) => {
                match d.show(&ctx) {
                    playnite_dialog::PlayniteOut::None => Some(Modal::Playnite(d)),
                    playnite_dialog::PlayniteOut::Changed => { hs.dirty = true; hs.packs_stale = true; hs.data.library_changed(); Some(Modal::Playnite(d)) }
                    playnite_dialog::PlayniteOut::Added(games) => {
                        hs.dirty = true; hs.data.library_changed();
                        hs.toast(format!("Added {} game(s) from Playnite; filling in their details", games.len()));
                        // one job of each kind for all the new games (Steam ones from the store, the others from what the matching finds), then art and HLTB for all
                        let steam: Vec<i64> = games.iter().filter(|g| g.2).map(|g| g.0).collect();
                        let other: Vec<i64> = games.iter().filter(|g| !g.2).map(|g| g.0).collect();
                        let all: Vec<i64> = games.iter().map(|g| g.0).collect();
                        hs.job_queue.push((ctx.clone(), sync::Kind::Art, all.clone(), "Art for the added games".into()));   // covers first: a card without one looks broken, one without tags only looks plain
                        if !steam.is_empty() { hs.job_queue.push((ctx.clone(), sync::Kind::Rescrape, steam.clone(), "Filling in the added Steam games".into())); }
                        if !other.is_empty() { hs.job_queue.push((ctx.clone(), sync::Kind::Backfill, other, "Filling in the added games".into())); }
                        if !steam.is_empty() { hs.job_queue.push((ctx.clone(), sync::Kind::ProtonDb, steam, "ProtonDB for the added games".into())); }
                        hs.job_queue.push((ctx.clone(), sync::Kind::Hltb, all, "HLTB for the added games".into()));
                        Some(Modal::Playnite(d))
                    }
                    playnite_dialog::PlayniteOut::Close => None,
                }
            }
            Modal::SendLog(mut d) => {
                let accent = hs.accent;
                match d.show(&ctx, accent) {
                    sendlog_dialog::SendLogOut::None => Some(Modal::SendLog(d)),
                    sendlog_dialog::SendLogOut::Sent => { hs.data.save_pref("last_log_submit_at", serde_json::json!(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64()))); Some(Modal::SendLog(d)) }
                    sendlog_dialog::SendLogOut::Close => None,
                }
            }
            Modal::Data(mut d) => {
                let accent = hs.accent;
                match d.show(&ctx, accent) {
                    data_dialog::DataOut::None | data_dialog::DataOut::Restored => Some(Modal::Data(d)),
                    data_dialog::DataOut::Imported => { hs.dirty = true; Some(Modal::Data(d)) }
                    data_dialog::DataOut::OpenPlaynite => Some(Modal::Playnite(Box::new(playnite_dialog::PlayniteDialog::new(hs.data.db_path(), hs.data.root_dir())))),
                    data_dialog::DataOut::Close => None,
                    data_dialog::DataOut::SaveFilter(name, tree) => {
                        let mut saved = hs.data.state_json().get("saved_filters").and_then(|v| v.as_object()).cloned().unwrap_or_default();
                        saved.insert(name, serde_json::json!({ "id": filter_model::new_id(), "tree": tree }));
                        hs.data.save_pref("saved_filters", serde_json::Value::Object(saved));
                        hs.dirty = true;
                        Some(Modal::Data(d))
                    }
                    data_dialog::DataOut::BackedUp => { hs.data.save_pref("last_backup_at", serde_json::json!(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|t| t.as_secs_f64()).unwrap_or(0.0))); Some(Modal::Data(d)) }
                }
            }
            Modal::AddGame(mut d) => {
                let accent = hs.accent;
                match d.show(&ctx, accent) {
                    add_game_dialog::AddOut::None => Some(Modal::AddGame(d)),
                    add_game_dialog::AddOut::Close => None,
                    out => {
                        let (steam, res, name) = match out { add_game_dialog::AddOut::Steam(id, n) => (true, hs.data.add_steam_game(id, &n), n), add_game_dialog::AddOut::Custom(n, exe, args) => { let r = hs.data.add_custom_game(&n); if let Ok(pd_filter::addgame::Added::New(id)) = &r { if !exe.is_empty() || !args.is_empty() { if let Err(e) = hs.data.set_custom_launch(*id, &exe, &args) { hs.toast(format!("Added, but could not save the program: {e}")); } } } (false, r, n) } _ => unreachable!() };
                        match res {
                            Ok(pd_filter::addgame::Added::New(appid)) => {
                                hs.dirty = true; hs.data.library_changed();
                                hs.toast(format!("Added {name}; fetching its details"));
                                // the same jobs a bulk re-scrape and art fetch run, one after the other
                                let fill = if steam { (sync::Kind::Rescrape, format!("Filling in {name}")) } else { (sync::Kind::Backfill, format!("Filling in {name}")) };
                                let art = (sync::Kind::Art, format!("Art for {name}"));
                                // covers first, except for a game still named "AppID N" (the store lookup failed): its cover search is by name, so the fill-in has to name it first
                                let mut q = if name.starts_with("AppID ") { vec![fill, art] } else { vec![art, fill] };
                                if steam { q.push((sync::Kind::ProtonDb, format!("ProtonDB for {name}"))); }
                                q.push((sync::Kind::Hltb, format!("HLTB for {name}")));
                                for (k, l) in q { hs.job_queue.push((ctx.clone(), k, vec![appid], l)); }
                                if steam { hs.auto_dates(vec![appid]); }
                                None
                            }
                            Ok(pd_filter::addgame::Added::Exists(m)) => { d.status = m; Some(Modal::AddGame(d)) }
                            Err(e) => { d.status = e; Some(Modal::AddGame(d)) }
                        }
                    }
                }
            }
            Modal::Emulators(mut d) => {
                let accent = hs.accent;
                match d.show(&ctx, accent) {
                    emulators_dialog::EmuOut::None => Some(Modal::Emulators(d)),
                    emulators_dialog::EmuOut::Close => None,
                    emulators_dialog::EmuOut::Added(games) => {
                        hs.dirty = true;
                        // cover art for each new game, one job after another (a ROM has no store page to take it from)
                        for (appid, name) in games { hs.job_queue.push((ctx.clone(), sync::Kind::Art, vec![appid], format!("Cover for {name}"))); }
                        Some(Modal::Emulators(d))
                    }
                }
            }
            Modal::KeyringChoice => {
                let accent = ui_kit::accent();
                let mut pick: Option<pd_filter::secrets::Fallback> = None;
                let out = ui_kit::modal(&ctx, "keyring_choice", "Keep your sign-ins?", 30.0, |ui, r| {
                    ui.label(egui::RichText::new("Zest keeps sign-ins in your system's keyring, but none is available here (for example, Steam Deck Game Mode).").size(0.9 * r).color(ui_kit::TEXT()));
                    ui.add_space(0.4 * r);
                    ui.label(egui::RichText::new("Save in a file: you stay signed in. The file is not encrypted, so anyone who can read your files can use these sign-ins.").size(0.85 * r).color(ui_kit::TEXT()));
                    ui.add_space(0.2 * r);
                    ui.label(egui::RichText::new("Forget when Zest closes: you sign in again next time.").size(0.85 * r).color(ui_kit::TEXT()));
                    ui.add_space(0.4 * r);
                    ui.horizontal_wrapped(|ui| {
                        if ui_kit::button(ui, "SAVE IN A FILE", ui_kit::Kind::Primary, accent, r).clicked() { pick = Some(pd_filter::secrets::Fallback::File); }
                        if ui_kit::button(ui, "FORGET WHEN ZEST CLOSES", ui_kit::Kind::Plain, accent, r).clicked() { pick = Some(pd_filter::secrets::Fallback::Session); }
                    });
                    ui.add_space(0.3 * r);
                    ui.label(ui_kit::muted("You can change this in Settings > Account.", 0.8 * r));
                });
                if let Some(m) = pick {
                    hs.data.save_pref("keyring_fallback", serde_json::json!(m.key()));
                    pd_filter::secrets::set_fallback(m);
                    pd_filter::account::migrate_secrets(&hs.data.root_dir());
                    None
                } else if out.closed { pd_filter::secrets::dismiss_pending(); None } else { Some(Modal::KeyringChoice) }
            }
            Modal::WhatsNew { sections } => {
                let out = ui_kit::modal(&ctx, "whats_new", "What's new in Zest", 36.0, |ui, r| {
                    egui::ScrollArea::vertical().max_height(f32::INFINITY).auto_shrink([false, true]).show(ui, |ui| {
                        for s in &sections {
                            ui.add_space(0.4 * r);
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(&s.version).size(1.05 * r).strong().color(crate::ui_kit::heading()));
                                if !s.date.is_empty() { ui.label(ui_kit::muted(s.date.clone(), 0.8 * r)); }
                            });
                            for (group, items) in &s.groups {
                                ui.label(egui::RichText::new(group).size(0.9 * r).strong().color(ui_kit::accent()));
                                for i in items { ui.label(egui::RichText::new(format!("\u{2022} {i}")).size(0.88 * r).color(ui_kit::TEXT())); }
                            }
                        }
                    });
                });
                if out.closed { hs.data.save_pref("whats_new_seen", serde_json::json!(pd_filter::updater::VERSION)); None } else { Some(Modal::WhatsNew { sections }) }
            }
            Modal::Placeholder(title) => {
                let out = ui_kit::modal(&ctx, "placeholder", title, 28.0, |ui, r| {
                    ui.label(ui_kit::muted("This dialog is not built yet.", 0.9 * r));
                });
                if out.closed { None } else { Some(Modal::Placeholder(title)) }
            }
            Modal::Blacklist { mut entries, mut query } => {
                let mut remove = None;
                let mut open_junk = false;
                let out = ui_kit::modal(&ctx, "blacklist", "Blacklist", 40.0, |ui, r| {
                    ui.label(ui_kit::muted("Games a sync must not bring back. Remove one to allow it again.", 0.85 * r));
                    ui.add_space(0.5 * r);
                    if ui_kit::button(ui, "FIND LIBRARY JUNK", ui_kit::Kind::Plain, accent, r).clicked() { open_junk = true; }
                    ui.add_space(0.5 * r);
                    ui.add(egui::TextEdit::singleline(&mut query).hint_text("Search...").desired_width(f32::INFINITY));
                    ui.add_space(0.5 * r);
                    let q = query.to_lowercase();
                    let shown: Vec<&home::BlEntry> = entries.iter().filter(|e| q.is_empty() || e.name.to_lowercase().contains(&q)).collect();
                    ui.label(ui_kit::muted(format!("{} of {} games", shown.len(), entries.len()), 0.8 * r));
                    egui::ScrollArea::vertical().max_height(f32::INFINITY).auto_shrink([false, true]).show(ui, |ui| {
                        for e in shown {
                            ui.horizontal(|ui| {
                                let cell = |ui: &mut egui::Ui, w: f32, text: egui::RichText| { ui.allocate_ui_with_layout(egui::vec2(w, 1.6 * r), egui::Layout::left_to_right(egui::Align::Center), |ui| { ui.add(egui::Label::new(text).truncate()); }); };
                                cell(ui, 17.5 * r, egui::RichText::new(&e.name).color(ui_kit::TEXT()).size(0.88 * r));
                                cell(ui, 7.5 * r, ui_kit::muted(home::platform_label(&e.platform), 0.8 * r));
                                cell(ui, 5.5 * r, ui_kit::muted(e.date.clone(), 0.78 * r));
                                if ui_kit::button(ui, "Remove", ui_kit::Kind::Plain, accent, r).clicked() { remove = Some(e.appid); }
                            });
                            ui.separator();
                        }
                    });
                });
                if let Some(id) = remove {
                    match hs.data.unblacklist(id) { Ok(()) => { entries.retain(|e| e.appid != id); } Err(e) => hs.toast(format!("Could not remove: {e}")) }
                }
                if open_junk { Some(Modal::Junk(Box::new(junk_dialog::JunkDialog::new(&ctx, hs.data.db_path(), hs.data.root_dir())))) }
                else if out.closed { None } else { Some(Modal::Blacklist { entries, query }) }
            }
            Modal::Junk(mut d) => {
                let accent = hs.accent;
                match d.show(&ctx, accent) {
                    junk_dialog::JunkOut::None => Some(Modal::Junk(d)),
                    junk_dialog::JunkOut::Close => None,
                    out @ (junk_dialog::JunkOut::Delete(_) | junk_dialog::JunkOut::Blacklist(_)) => {
                        let (ids, blacklist) = match out { junk_dialog::JunkOut::Delete(i) => (i, false), junk_dialog::JunkOut::Blacklist(i) => (i, true), _ => unreachable!() };
                        let (mut gone, mut first_err) = (Vec::new(), None::<String>);
                        for id in ids { match hs.data.delete_game(id, blacklist) { Ok(()) => gone.push(id), Err(e) => { first_err.get_or_insert(e); } } }
                        // as a delete from a shelf: Home asks its shelves again (the shuffled ones only fill the gaps), or the deleted games just left holes
                        if !gone.is_empty() { hs.dirty = true; hs.data.library_changed(); }
                        let msg = match first_err { None => format!("{} {} game{}.", if blacklist { "Blacklisted" } else { "Deleted" }, gone.len(), if gone.len() == 1 { "" } else { "s" }), Some(e) => format!("{} done, then an error: {e}", gone.len()) };
                        d.done(&gone, msg);
                        Some(Modal::Junk(d))
                    }
                    junk_dialog::JunkOut::Whitelist(ids) => {
                        let v = pd_filter::junk::merged_whitelist(hs.data.state_json(), &ids);
                        hs.data.save_pref("steam_junk_whitelist", v);
                        let n = ids.len();
                        d.done(&ids, format!("Whitelisted {n} game{}.", if n == 1 { "" } else { "s" }));
                        Some(Modal::Junk(d))
                    }
                }
            }
            Modal::Duplicates { mut items } => {
                let (mut approve, mut reject, mut all) = (false, false, None::<bool>);
                let out = ui_kit::modal(&ctx, "duplicates", "Possible duplicates", 56.0, |ui, r| {
                    ui.label(ui_kit::muted("Games that match another library's copy only after an edition name is ignored (\"Game - Remastered\" and \"Game\"). Nothing is hidden until you approve it. Approved games are hidden under the copy that ranks first in your platform order.", 0.85 * r));
                    ui.add_space(0.5 * r);
                    let sel = items.iter().filter(|i| i.1).count();
                    ui.horizontal(|ui| {
                        if ui_kit::button(ui, "Select all", ui_kit::Kind::Plain, accent, r).clicked() { all = Some(true); }
                        if ui_kit::button(ui, "Select none", ui_kit::Kind::Plain, accent, r).clicked() { all = Some(false); }
                        ui.label(ui_kit::muted(format!("{sel} of {} selected", items.len()), 0.8 * r));
                    });
                    ui.add_space(0.3 * r);
                    if items.is_empty() { ui.label(ui_kit::muted("No suggestions.", 0.9 * r)); }
                    egui::ScrollArea::vertical().max_height(f32::INFINITY).auto_shrink([false, true]).show(ui, |ui| {
                        for (s, on) in items.iter_mut() {
                            ui.horizontal(|ui| {
                                ui.checkbox(on, "");
                                ui.label(egui::RichText::new(&s.name).color(ui_kit::TEXT()).size(0.88 * r));
                                ui.label(ui_kit::muted(format!("({})", home::platform_label(&s.platform)), 0.78 * r));
                                ui.label(ui_kit::muted("\u{2192} hidden under", 0.78 * r));
                                ui.label(egui::RichText::new(&s.target_name).color(ui_kit::TEXT()).size(0.88 * r));
                                ui.label(ui_kit::muted(format!("({})", home::platform_label(&s.target_platform)), 0.78 * r));
                            });
                            ui.separator();
                        }
                    });
                    ui.add_space(0.5 * r);
                    ui.horizontal(|ui| {
                        if ui_kit::button(ui, "Approve selected", ui_kit::Kind::Primary, accent, r).clicked() { approve = true; }
                        if ui_kit::button(ui, "Not duplicates", ui_kit::Kind::Plain, accent, r).clicked() { reject = true; }
                    });
                });
                if let Some(v) = all { for i in items.iter_mut() { i.1 = v; } }
                if approve || reject {
                    let chosen: Vec<&(pd_filter::dupes::Suggestion, bool)> = items.iter().filter(|i| i.1).collect();
                    if chosen.is_empty() { hs.toast("Select some games first"); }
                    else if approve {
                        let pairs: Vec<(i64, i64)> = chosen.iter().map(|i| (i.0.appid, i.0.target)).collect();
                        match hs.data.dup_approve(&pairs) { Ok(()) => { hs.dirty = true; hs.toast(format!("Linked {} games", pairs.len())); items = hs.data.dup_suggestions().into_iter().map(|s| (s, false)).collect(); } Err(e) => hs.toast(format!("Could not link: {e}")) }
                    } else {
                        let ids: Vec<i64> = chosen.iter().map(|i| i.0.appid).collect();
                        match hs.data.dup_reject(&ids) { Ok(()) => { hs.toast(format!("Marked {} games as not duplicates", ids.len())); items = hs.data.dup_suggestions().into_iter().map(|s| (s, false)).collect(); } Err(e) => hs.toast(format!("Could not save: {e}")) }
                    }
                }
                if out.closed { None } else { Some(Modal::Duplicates { items }) }
            }
        };
    }

    /// The top bar: menu, page tabs, EXIT. Takes the bar's height out of the layout. Sizes follow the web CSS (rem = clamp(11px, 0.625vw, 20px),
    /// links clamp(0.5rem, 1.5vw, 1.2rem) bold with 1px letter spacing).
    fn nav(&mut self, ui: &mut egui::Ui) {
        use egui::{Color32, RichText};
        let col = |c: theme::Rgb| Color32::from_rgb(c.0, c.1, c.2);
        let t = self.theme.clone();
        let screen = ui.ctx().content_rect();
        let rem = (screen.width() * 0.00625).clamp(11.0, 20.0);
        let font = (screen.width() * 0.015).clamp(0.5 * rem, 1.2 * rem);
        let bar_h = home::bar_height(screen.height() as i32) as f32;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), bar_h), egui::Sense::hover());
        ui.painter().rect_filled(rect, 0.0, col(t.bg_nav));
        ui.painter().line_segment([rect.left_bottom(), rect.right_bottom()], egui::Stroke::new(1.0_f32, col(t.border)));
        let pad = 2.5 * rem;
        // the web button is [12px hidden balance][the ☰ glyph][5px + 7px hidden notification dot]: the glyph sits 12px in and the button is glyph + 24px wide
        let glyph_w = font * 0.78;
        let hb = egui::Rect::from_min_size(egui::pos2(rect.left() + pad, rect.top()), egui::vec2(glyph_w + 24.0, bar_h));
        let hresp = ui.interact(hb, ui.id().with("hamburger"), egui::Sense::click());
        if hresp.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
        self.pad_nav_rects.clear(); self.pad_nav_rects.push(hb);
        if hresp.clicked() { self.menu_open = !self.menu_open; }
        if self.pad_nav == Some(0) { self.pad_ring_draw(ui.ctx(), hb.shrink2(egui::vec2(4.0, bar_h * 0.12)), screen, false, 4.0); }
        let hcol = if hresp.hovered() || self.menu_open { col(t.accent) } else { col(t.text_secondary) };
        for k in 0..3 { let y = rect.center().y - font * 0.34 + k as f32 * font * 0.34; ui.painter().line_segment([egui::pos2(rect.left() + pad + 10.0, y), egui::pos2(rect.left() + pad + 10.0 + glyph_w, y)], egui::Stroke::new(1.3_f32, hcol)); }
        if self.menu_open { self.hamburger_menu(ui, hb, rect.bottom(), col(t.accent)); }
        // The tabs are laid out by hand at positions fitted to PlayDate's real screenshot (its font renders them at 13.5px with 1px letter spacing,
        // origin 100.08px, 38.32px between tabs at a 1904px window; all of it scales with rem). egui's own button padding was part of the drift.
        let sc = rem / 11.9;
        let tab_font = (screen.width() * 0.015).clamp(0.5 * rem, 1.2 * rem) * 0.945;
        let (mut x, cy) = (rect.left() + 99.08 * sc, rect.center().y - 2.0);
        for (k, (label, page)) in [("HOME", Some(Page::Home)), ("LIBRARY", Some(Page::Library)), ("PICK 6", Some(Page::Pick))].into_iter().enumerate() {
            let active = page == Some(self.page);
            let layout = |color: Color32| { let mut job = egui::text::LayoutJob::default();
                job.append(label, 0.0, egui::text::TextFormat { font_id: theme::bold(tab_font), color, extra_letter_spacing: 1.15 * sc, ..Default::default() });
                ui.painter().layout_job(job) };
            let size = layout(Color32::WHITE).size();
            let tab = egui::Rect::from_min_size(egui::pos2(x, cy - size.y / 2.0), size);
            let resp = ui.interact(tab.expand2(egui::vec2(6.0, 8.0)), ui.id().with(("tab", label)), egui::Sense::click());
            if resp.hovered() && page.is_some() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
            self.pad_nav_rects.push(resp.rect);
            if self.pad_nav == Some(k + 1) { self.pad_ring_draw(ui.ctx(), resp.rect, screen, false, 4.0); }
            // the page's link color: secondary text, the accent blue on hover, bright for the current page
            let ink = if active { col(t.text_heading) } else if resp.hovered() && page.is_some() { col(t.accent) } else { col(t.text_secondary) };
            let galley = layout(ink);
            ui.painter().galley(tab.min, galley.clone(), Color32::PLACEHOLDER);
            if resp.clicked() { if let Some(p) = page { self.want_page = Some(p); } }
            x += galley.size().x + 38.7 * sc;
        }
        // .nav-btn: bg-surface fill, 1px 20% white border, 0.375rem radius, height 3.2vh, semi-bold uppercase text with 0.5px letter spacing
        let btn_h = screen.height() * 0.032;
        let radius = egui::CornerRadius::same((0.375 * rem).round() as u8);
        let btn_font = (screen.width() * 0.011).clamp(0.5 * rem, rem);
        // the status line: the running job (click to cancel) or the latest message (click to clear); it lives here, in the bar, never as a popup
        let status: Option<(String, bool, String)> = self.status.clone();
        let tabs_end = x;
        let mut status_clicked = false;
        if status.as_ref().map_or(false, |s| s.1) { ui.ctx().request_repaint_after(Duration::from_millis(400)); }
        let (want_fs, want_exit) = (&mut self.want_fullscreen, &mut false);
        let mut exit = false;
        let (mut fs_rect, mut exit_rect) = (egui::Rect::NOTHING, egui::Rect::NOTHING);
        let mut row = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(egui::vec2(pad, 0.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
        row.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let danger = col(t.color_danger);
            // EXIT
            let mut job = egui::text::LayoutJob::default();
            job.append("EXIT", 0.0, egui::text::TextFormat { font_id: theme::bold(btn_font), color: col(t.text_bright), extra_letter_spacing: 1.0, ..Default::default() });   // PlayDate's text is a little wider than egui lays it out (0.5px CSS spacing + font metrics)
            let galley = ui.painter().layout_job(job);
            let (r, resp) = ui.allocate_exact_size(egui::vec2(galley.size().x + 2.0 * screen.width() * 0.012, btn_h), egui::Sense::click());
            ui.painter().rect_filled(r, radius, danger);
            ui.painter().rect_stroke(r, radius, egui::Stroke::new(1.0_f32, danger), egui::StrokeKind::Inside);
            let tint = if resp.hovered() { col(t.text_bright) } else { Color32::WHITE };
            ui.painter().galley(r.center() - galley.size() / 2.0, galley, tint);
            if resp.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
            if resp.clicked() { exit = true; }
            exit_rect = r;
            ui.add_space(8.0);
            // fullscreen: a small square with the corner-brackets glyph
            let (r, resp) = ui.allocate_exact_size(egui::vec2((2.25 * rem).max(2.0 * 0.625 * rem + rem), btn_h), egui::Sense::click());
            let r = egui::Rect::from_min_max(r.min.round(), r.max.round());   // whole pixels, or the edges anti-alias unevenly
            let fill = if resp.hovered() { col(t.accent) } else { col(t.bg_surface) };
            ui.painter().rect_filled(r, radius, fill);
            ui.painter().rect_stroke(r, radius, egui::Stroke::new(1.0_f32, Color32::from_white_alpha(51)), egui::StrokeKind::Inside);
            // the corner brackets, each arm a filled 2px rectangle on whole pixels (a stroked line at a fractional position blurs one side)
            let c = r.center().round();
            let (s, l, th) = ((rem * 0.45).round().max(4.0), (rem * 0.32).round().max(3.0), 2.0);
            let ink = if resp.hovered() { col(t.bg_surface) } else { Color32::WHITE };
            for (sx, sy) in [(-1.0f32, -1.0f32), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                let (x0, x1) = if sx < 0.0 { (c.x - s, c.x - s + l) } else { (c.x + s - l, c.x + s) };
                let (y0, y1) = if sy < 0.0 { (c.y - s, c.y - s + th) } else { (c.y + s - th, c.y + s) };
                ui.painter().rect_filled(egui::Rect::from_min_max(egui::pos2(x0, y0), egui::pos2(x1, y1)), 0.0, ink);
                let (vx0, vx1) = if sx < 0.0 { (c.x - s, c.x - s + th) } else { (c.x + s - th, c.x + s) };
                let (vy0, vy1) = if sy < 0.0 { (c.y - s, c.y - s + l) } else { (c.y + s - l, c.y + s) };
                ui.painter().rect_filled(egui::Rect::from_min_max(egui::pos2(vx0, vy0), egui::pos2(vx1, vy1)), 0.0, ink);
            }
            if resp.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
            if resp.clicked() { *want_fs = true; }
            fs_rect = r;
            ui.add_space(14.0);
            if let Some((text, job, tip)) = &status {
                let room = (ui.available_width() - (tabs_end - rect.left() - pad) - 24.0).max(0.0);
                if room > 40.0 {
                    ui.allocate_ui_with_layout(egui::vec2(room, btn_h), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let ink = if *job { col(t.accent) } else { col(t.text_secondary) };
                        let l = ui.add(egui::Label::new(RichText::new(text).size(btn_font * 1.05).color(ink)).truncate().sense(egui::Sense::click()));
                        if l.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
                        if l.clone().on_hover_text(tip).clicked() { status_clicked = true; }
                    });
                }
            }
        });
        if status_clicked { self.status_clicked = true; }
        let _ = want_exit;
        if exit { self.want_exit = true; }
        // the pad reaches the fullscreen and EXIT buttons too: nav indices 4 and 5, after the menu button and the three tabs
        self.pad_nav_rects.truncate(4); self.pad_nav_rects.push(fs_rect); self.pad_nav_rects.push(exit_rect);
        for (k, rc) in [(4usize, fs_rect), (5, exit_rect)] { if self.pad_nav == Some(k) && rc.is_positive() { self.pad_ring_draw(ui.ctx(), rc, ui.ctx().content_rect(), false, 4.0); } }
    }

    /// Labels (the shuffle link is clickable), titles of covers that have no image, and the chart widgets: the Home page's text and vector art.
    /// Returns the shelf whose shuffle label was clicked.
    fn home_overlay(ui: &mut egui::Ui, hs: &HomeState, boxes: &[home::ShelfBox], h: i32, t: &theme::Theme) -> Option<usize> {
        use egui::{text::{LayoutJob, TextFormat}, Color32, FontId, Pos2, Rect, Sense};
        let col = |c: theme::Rgb| Color32::from_rgb(c.0, c.1, c.2);
        let (vh, rem) = (h as f32 / 100.0, (ui.ctx().content_rect().width() * 0.00625).clamp(11.0, 20.0));
        let (painter, mut clicked) = (ui.painter().clone(), None);
        for b in boxes {
            let shelf = &hs.data.shelves[b.idx];
            if b.widget.as_deref() == Some("clock") {
                // the page's clock widget: the time big and bold in monospace, the date small and faint under it (index.html `.clock-widget`)
                let vw = ui.ctx().content_rect().width() / 100.0;
                let (tsize, dsize) = ((3.5 * vw).clamp(rem, 4.5 * rem), vw.clamp(0.5 * rem, 1.2 * rem));
                let (time, date) = home::clock_lines(shelf.clock);
                // a wider text (seconds, a long date) shrinks to fit the shelf instead of running past its edges
                let room = (b.w as f32 - 12.0).max(20.0);
                let mut tg = painter.layout_no_wrap(time.clone(), FontId::new(tsize, egui::FontFamily::Monospace), col(t.text_heading));
                if tg.size().x > room { tg = painter.layout_no_wrap(time, FontId::new((tsize * room / tg.size().x).max(6.0), egui::FontFamily::Monospace), col(t.text_heading)); }
                let mut dg = painter.layout_no_wrap(date.clone(), FontId::proportional(dsize), col(t.text_heading).gamma_multiply(0.6));
                if dg.size().x > room { dg = painter.layout_no_wrap(date, FontId::proportional((dsize * room / dg.size().x).max(6.0)), col(t.text_heading).gamma_multiply(0.6)); }
                let title_h = if shelf.show_title && !shelf.label.is_empty() { (1.1 * vh * 1.07).round() + 4.0 } else { 0.0 };
                let total = tg.size().y + if shelf.clock.date { 6.0 + dg.size().y } else { 0.0 };
                let top = b.y as f32 + title_h + (b.h as f32 - title_h - total) / 2.0;
                let cx = b.x as f32 + b.w as f32 / 2.0;
                painter.galley(Pos2::new(cx - tg.size().x / 2.0, top), tg.clone(), Color32::WHITE);
                painter.galley(Pos2::new(cx - dg.size().x / 2.0, top + tg.size().y + 6.0), dg, Color32::WHITE);
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(if shelf.clock.seconds { 500 } else { 5000 }));
            }
            if !shelf.label.is_empty() && (b.widget.is_none() || shelf.show_title) {
                let size = (1.1 * vh).max(9.0);
                let mut job = LayoutJob::default();
                job.append(&shelf.label.to_uppercase(), 0.0, TextFormat { font_id: theme::bold(size), color: col(t.accent), extra_letter_spacing: 1.0, ..Default::default() });
                let galley = painter.layout_job(job);
                let pos = Pos2::new(b.x as f32 + (b.w as f32 - galley.size().x) / 2.0, b.label_y as f32);
                let mut tint = Color32::WHITE;
                if shelf.shuffle {   // the shuffle link
                    let resp = ui.interact(Rect::from_min_size(pos, galley.size()).expand(3.0), ui.id().with(("shuffle", b.idx)), Sense::click());
                    if resp.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); tint = Color32::from_rgb(0xdd, 0xee, 0xff); }
                    if resp.clicked() { clicked = Some(b.idx); }
                }
                painter.galley(pos, galley, tint);
            }
            for c in &b.cards {
                if hs.covers.missing.contains(&c.appid) && !c.name.is_empty() {
                    let rect = Rect::from_min_size(Pos2::new(c.x as f32, c.y as f32), egui::vec2(c.w as f32, c.h as f32));
                    let galley = painter.layout(c.name.clone(), FontId::proportional(0.8 * rem), col(t.text_secondary), (c.w as f32 - 20.0).max(20.0));
                    painter.galley(rect.center() - galley.size() / 2.0, galley, Color32::WHITE);
                }
            }
        }
        clicked
    }

    /// The context menu and the toast. Returns a menu pick as (action, appid); status picks are "status:<name>".
    fn home_popups(ui: &mut egui::Ui, hs: &mut HomeState) -> (Option<(String, i64)>, Option<(i64, bool)>) {
        
        let mut pick = None;
        let mut answer = None;
        if let Some((appid, name, launch_after, cands)) = hs.exe_pick.clone() {
            let accent = hs.accent;
            let mut chosen: Option<String> = None;
            let out = ui_kit::modal(ui.ctx(), "exe_pick", "Which program?", 30.0, |ui, r| {
                ui.label(egui::RichText::new(&name).size(0.95 * r).strong().color(ui_kit::TEXT()));
                ui.label(ui_kit::muted("Several programs could start this game. Pick the one to start.", 0.85 * r));
                ui.add_space(0.4 * r);
                egui::ScrollArea::vertical().max_height(22.0 * r).show(ui, |ui| {
                    // the file's name (Fishbane.exe); its folder too only when two share a name
                    let base = |c: &str| c.rsplit('/').next().unwrap_or(c).to_string();
                    for c in &cands {
                        let label = if cands.iter().filter(|o| base(o) == base(c)).count() > 1 { c.clone() } else { base(c) };
                        if ui_kit::button(ui, &label, ui_kit::Kind::Plain, accent, r).on_hover_text(c).clicked() { chosen = Some(c.clone()); }
                    }
                });
            });
            if out.closed { hs.exe_pick = None; }
            if let Some(rel) = chosen {
                hs.exe_pick = None;
                // a pack of games asked about at a launch is not remembered: the pick starts that program this once; changing the program in the Edit dialog still saves it
                let once = launch_after && hs.data.game_ref(appid).and_then(|g| hs.sources.for_game(&g.platform, &g.platform_id).cloned()).map_or(false, |s| s.pick_program_each_time());
                if once { pd_filter::sources::set_launch_choice(appid, &rel); hs.run_action("launch", appid, (0.0, 0.0)); }
                else { match hs.data.set_executable(appid, Some(&rel)) { Ok(()) => if launch_after { hs.run_action("launch", appid, (0.0, 0.0)) } else { hs.toast(format!("{name} will start with {rel}")) }, Err(e) => hs.toast(format!("Could not save: {e}")) } }
            }
        }
        if let Some((appid, name, opts)) = hs.install_pick.clone() {
            let accent = hs.accent;
            let mut chosen: Option<String> = None;
            let out = ui_kit::modal(ui.ctx(), "install_pick", "Which build?", 30.0, |ui, r| {
                ui.label(egui::RichText::new(&name).size(0.95 * r).strong().color(ui_kit::TEXT()));
                ui.label(ui_kit::muted("The store offers more than one. Windows builds run through Wine. (Change the default in Plugins.)", 0.85 * r));
                ui.add_space(0.4 * r);
                for o in &opts { if ui_kit::button(ui, &o.label, ui_kit::Kind::Plain, accent, r).clicked() { chosen = Some(o.key.clone()); } }
            });
            if out.closed { hs.install_pick = None; }
            if let Some(key) = chosen {
                hs.install_pick = None;
                let platform = hs.data.game_ref(appid).map(|g| g.platform).unwrap_or_default();
                pd_filter::sources::set_install_choice(appid, &key);
                hs.begin_install(appid, &platform, &name);
            }
        }
        if let Some(appid) = hs.confirm {
            let name = hs.data.info(appid).map(|g| g.name.clone()).unwrap_or_default();
            let accent = hs.accent;
            let (mut ans, mut cancel): (Option<bool>, bool) = (None, false);
            // the standard dialog, so the pad can reach its buttons (a bare egui window was never on its focus path)
            let out = ui_kit::modal(ui.ctx(), "delete_confirm", "Delete game", 30.0, |ui, r| {
                ui.label(egui::RichText::new(format!("Delete {name} from your library?")).size(0.95 * r).color(ui_kit::TEXT()));
                ui.label(ui_kit::muted("Blacklisting also stops a sync from adding it back.", 0.8 * r));
                ui.add_space(0.6 * r);
                ui.horizontal(|ui| {
                    if ui_kit::button(ui, "CANCEL", ui_kit::Kind::Plain, accent, r).clicked() { cancel = true; }
                    if ui_kit::button(ui, "DELETE", ui_kit::Kind::Danger, accent, r).clicked() { ans = Some(false); }
                    if ui_kit::button(ui, "DELETE AND BLACKLIST", ui_kit::Kind::Danger, accent, r).clicked() { ans = Some(true); }
                });
            });
            if out.closed || cancel { hs.confirm = None; }
            if let Some(b) = ans { answer = Some((appid, b)); }
        }
        if let Some((appid, pos)) = hs.menu {
            if let Some(g) = hs.data.info(appid).cloned() {
                let caps = hs.data.game_ref(appid).and_then(|gr| hs.sources.for_game(&gr.platform, &gr.platform_id).map(|s| s.caps())).unwrap_or_default();
                let r = ui_kit::rem(ui.ctx());
                let accent = hs.accent;
                let (mut chosen, mut comp): (Option<String>, Option<egui::Rect>) = (None, None);
                let mut comp_hot = false;
                let resp = egui::Area::new(egui::Id::new("card_menu")).order(egui::Order::Foreground).fixed_pos(pos).constrain(true).show(ui.ctx(), |ui| {
                    ui_kit::menu_frame(r).show(ui, |ui| {
                        ui.set_width(13.125 * r);
                        let red = crate::ui_kit::text_danger();
                        let first = if g.installed { ui_kit::menu_row(ui, "\u{25B6}", "Launch Game", ui_kit::TEXT(), false, r) } else { ui_kit::menu_row(ui, "\u{2193}", "Install Game", ui_kit::TEXT(), false, r) };
                        let mut main_ids: Vec<egui::Id> = vec![first.id];
                        if first.clicked() { chosen = Some("launch".into()); }
                        // opened with the pad: nothing has the focus yet, and the first Down would Tab to something behind the menu; start on the first row
                        if ui.memory(|m| m.focused().is_none()) { first.request_focus(); }
                        let rr = ui_kit::menu_row(ui, "\u{2197}", "Open Store Page", ui_kit::TEXT(), false, r); main_ids.push(rr.id);
                        if rr.clicked() { chosen = Some("store".into()); }
                        if caps.achievements_page { let rr = ui_kit::menu_row(ui, "\u{2605}", "Achievements", ui_kit::TEXT(), false, r); main_ids.push(rr.id); if rr.clicked() { chosen = Some("achievements".into()); } }
                        if caps.community_page { let rr = ui_kit::menu_row(ui, "\u{2709}", "Community Hub", ui_kit::TEXT(), false, r); main_ids.push(rr.id); if rr.clicked() { chosen = Some("community_hub".into()); } }
                        let c = ui_kit::menu_row(ui, "\u{2714}", "Completion Status", ui_kit::TEXT(), true, r);
                        comp = Some(c.rect); main_ids.push(c.id);
                        let rr = ui_kit::menu_row(ui, "\u{270E}", "Edit Game", ui_kit::TEXT(), false, r); main_ids.push(rr.id);
                        if rr.clicked() { chosen = Some("edit".into()); }
                        if g.installed { let rr = ui_kit::menu_row(ui, "\u{232B}", "Uninstall Game", red, false, r); main_ids.push(rr.id); if rr.clicked() { chosen = Some("uninstall".into()); } }
                        let rr = ui_kit::menu_row(ui, "\u{2715}", "Delete Game", red, false, r); main_ids.push(rr.id);
                        if rr.clicked() { chosen = Some("delete".into()); }
                        // the status list opens under the pointer or when the pad is on the row (or already inside the list). Focus on something that is not a row of this menu is inside the
                        // list: it must stay open then, or the row the pad just moved to vanishes, the focus is lost and falls back to the first row (Right into the list).
                        let f = ui.memory(|m| m.focused());
                        comp_hot = c.hovered() || f == Some(c.id) || ui.ctx().data(|d| d.get_temp::<bool>(egui::Id::new("card_sub_focus"))).unwrap_or(false) || f.map_or(false, |f| !main_ids.contains(&f));
                    });
                });
                let main_rect = resp.response.rect;
                let mut full = main_rect;
                // the status submenu opens to the right while the pointer is on its row or on the submenu itself
                let over = |rect: Option<egui::Rect>| hs.pointer.zip(rect).map_or(false, |(p, rr)| rr.contains(p));
                if let Some(cr) = comp {
                    if comp_hot || over(hs.sub_rect) {
                        let sub_w = 10.0 * r;
                        let screen = ui.ctx().content_rect();
                        let x = if main_rect.max.x + sub_w + 4.0 < screen.max.x { main_rect.max.x - 1.0 } else { main_rect.min.x - sub_w + 1.0 };
                        let sub = egui::Area::new(egui::Id::new("card_menu_sub")).order(egui::Order::Foreground).fixed_pos(egui::pos2(x, cr.min.y - 4.0)).show(ui.ctx(), |ui| {
                            ui_kit::menu_frame(r).show(ui, |ui| {
                                ui.set_width(sub_w);
                                let mut inside = false;
                                for st in ["Never Played", "Unfinished", "Beaten", "Completed", "Won't Play"] {
                                    let on = g.status == st;
                                    let row = ui_kit::menu_row(ui, if on { "\u{2713}" } else { "" }, st, if on { accent } else { ui_kit::TEXT() }, false, r);
                                    if row.clicked() { chosen = Some(format!("status:{st}")); }
                                    inside |= ui.memory(|m| m.focused()) == Some(row.id);
                                }
                                ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("card_sub_focus"), inside));
                            });
                        });
                        hs.sub_rect = Some(sub.response.rect);
                        full = full.union(sub.response.rect);
                    } else { hs.sub_rect = None; ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("card_sub_focus"), false)); }
                }
                if let Some(k) = chosen { pick = Some((k, appid)); }
                hs.menu_rect = Some(full);
            } else { hs.menu = None; }
        }
        (pick, answer)
    }

    /// True when nothing but the page itself is on screen (no dialog, menu or popup), so the pad drives the card cursor.
    fn pad_free(&self) -> bool {
        let blockers: Vec<_> = self.ctx.memory(|m| m.areas().visible_layer_ids()).into_iter().filter(|l| matches!(l.order, egui::Order::Middle | egui::Order::Foreground)).collect();
        if !blockers.is_empty() && std::env::var_os("ZEST_PAD_DEBUG").is_some() { eprintln!("gamepad: cursor off, layers on screen: {blockers:?}"); }
        blockers.is_empty()
    }

    /// Drain the pad queue. With `cards` and nothing else on screen, directions, A, X, Y and the bumpers go to the card cursor (returned);
    /// everything else becomes egui key events (egui moves focus by geometry and activates with Enter). Only while the window has focus:
    /// a pad used in a game must not drive Zest behind it.
    fn pad_take(&mut self, window: &Window, raw: &mut egui::RawInput, cards: bool) -> Vec<gamepad::Nav> {
        use gamepad::Nav;
        const PAGES: [Page; 3] = [Page::Home, Page::Library, Page::Pick];
        if raw.events.iter().any(|e| matches!(e, egui::Event::PointerMoved(_) | egui::Event::PointerButton { .. })) { self.pad_show = false; self.pad_nav = None; self.pad_bar = None; }
        { let pad_driving = self.pad_show; self.ctx.data_mut(|d| d.insert_temp(egui::Id::new("zest_pad_show"), pad_driving)); }   // read by ui_kit::combo_search: no search box in a drop-down list the pad is stepping through
        // remember the control of a dialog or menu the mouse is over (the smallest one under it, in the layer on top there), for the pad's first press
        if let Some(p) = self.ctx.input(|i| i.pointer.latest_pos()) {
            if let Some(l) = self.ctx.layer_id_at(p).filter(|l| l.order != egui::Order::Background) {
                let best = self.ctx.memory(|m| m.focusable_ids()).into_iter().filter_map(|o| self.ctx.read_response(o)).filter(|r| r.layer_id == l && r.rect.contains(p))
                    .min_by(|x, y| (x.rect.width() * x.rect.height()).partial_cmp(&(y.rect.width() * y.rect.height())).unwrap_or(std::cmp::Ordering::Equal));
                if let Some(r) = best { self.pad_hover = Some(r.id); }
            }
        }
        // the mouse wheel scrolling a dialog away from the control the pad was on lets go of that control, as a click does: otherwise the focus pulls the list back to it
        if raw.events.iter().any(|e| matches!(e, egui::Event::MouseWheel { .. })) && !self.ctx.egui_wants_keyboard_input() && !self.pad_free() {
            self.pad_show = false;
            self.ctx.memory_mut(|m| if let Some(id) = m.focused() { m.surrender_focus(id); });
        }
        let free_any = !self.menu_open && !self.ctx.egui_wants_keyboard_input() && self.pad_free();
        let free = cards && free_any;
        let mut out = Vec::new();
        let navs: Vec<(Nav, bool)> = { let mut q = self.pad_queue.lock().unwrap(); q.drain(..).collect() };
        let navs = if gamepad::enabled() { navs } else { Vec::new() };   // Settings > Gamepad > off: the pad does nothing here
        let navs = if gamepad::testing() { if !navs.is_empty() { eprintln!("gamepad: {} press(es) ignored (Controller test is on)", navs.len()); } Vec::new() } else { navs };   // Settings > Gamepad > TEST BUTTONS: nothing reaches Zest
        if !self.pad_free() || std::env::var_os("ZEST_PAD_ANY").is_some() { let waiting = { static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new(); let wait: u64 = std::env::var("ZEST_PAD_WAIT_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(0); START.get_or_init(Instant::now).elapsed().as_millis() < wait as u128 };   // test hook: hold the keys until a dialog has loaded
        let keys = if waiting { String::new() } else { let mut g = SHOT_PAD.lock().unwrap(); let mut it = g.chars(); let first: String = it.next().into_iter().collect(); *g = it.collect(); first }; if !keys.is_empty() { if let Ok(mut q) = self.pad_queue.lock() { for c in keys.chars() { let n = match c { 'u' => Nav::Up, 'd' => Nav::Down, 'l' => Nav::Left, 'r' => Nav::Right, 'a' => Nav::Confirm, 'b' => Nav::Back, _ => continue }; q.push_back((n, true)); q.push_back((n, false)); } } } }
        if !window.has_focus() && self.shot.is_none() { return out; }
        // test hook (--script): one mouse or keyboard action every fourth frame, after the wait
        if self.shot_size.is_some() {
            static T0: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
            static TICK: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let wait: u64 = std::env::var("ZEST_PAD_WAIT_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
            if T0.get_or_init(Instant::now).elapsed().as_millis() >= wait as u128 && TICK.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 4 == 0 {
                let act = { let mut q = SHOT_SCRIPT.lock().unwrap(); if q.is_empty() { None } else { Some(q.remove(0)) } };
                if let Some(act) = act {
                    let (verb, rest) = act.split_once(' ').map_or((act.as_str(), ""), |(v, r)| (v, r.trim()));
                    match verb {
                        "click" => if let Some((x, y)) = rest.split_once(',').and_then(|(a, b)| Some((a.trim().parse::<f32>().ok()?, b.trim().parse::<f32>().ok()?))) {
                            let pos = egui::pos2(x, y);
                            self.pad_show = false; self.ctx.data_mut(|d| d.insert_temp(egui::Id::new("zest_pad_show"), false));
                            // press now, release on the next action: a click's two halves land in different frames, as with a real mouse (a press and release in one frame also clears the focus a list that opens on it asks for)
                            raw.events.push(egui::Event::PointerMoved(pos));
                            raw.events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
                            SHOT_SCRIPT.lock().unwrap().insert(0, format!("release {x},{y}"));
                        },
                        "release" => if let Some((x, y)) = rest.split_once(',').and_then(|(a, b)| Some((a.trim().parse::<f32>().ok()?, b.trim().parse::<f32>().ok()?))) {
                            raw.events.push(egui::Event::PointerButton { pos: egui::pos2(x, y), button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
                        },
                        "type" => raw.events.push(egui::Event::Text(rest.to_string())),
                        "key" => if let Some(k) = egui::Key::from_name(rest) { for pressed in [true, false] { raw.events.push(egui::Event::Key { key: k, physical_key: Some(k), pressed, repeat: false, modifiers: Default::default() }); } },
                        _ => {}
                    }
                    if std::env::var_os("ZEST_PAD_DEBUG").is_some() { eprintln!("script: {act}"); }
                }
            }
        }
        // right stick: scroll (PlayDate's input.js: 20 px per frame at full deflection, ramping up after a second held)
        {
            let now = Instant::now();
            let dt = now.duration_since(self.rs_at).as_secs_f32().min(0.1);
            self.rs_at = now;
            let up = std::env::var("ZEST_PAD_STICK").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or_else(|| self.pad_scroll.load(std::sync::atomic::Ordering::Relaxed) as f32 / 1000.0);   // ZEST_PAD_STICK: a test hook, a constant stick value (-1 to 1)
            // not while typing in a text box; any other focused control (egui counts every focused widget as 'wanting the keyboard') must not stop the stick: in a dialog the pad always has one
            if up.abs() >= gamepad::dead_zone() && !self.ctx.text_edit_focused() {
                let since = *self.rs_since.get_or_insert(now);
                let held = now.duration_since(since).as_millis() as f32;
                let mult = if held > 1000.0 { 1.8f32.powf((held - 1000.0) / 1000.0).min(12.0) } else { 1.0 };
                self.rs_mult = mult;
                self.rs_px = -up * 20.0 * 60.0 * dt * mult;
                if !self.pad_free() {
                    // a dialog: egui scrolls the area under the pointer, so put it on the focused control (that also hovers it, which the cursor ring already says)
                    // a focused header with a list of its own open under it (PAGYWOSG's 'additional games') gets the stick: its scroll position is moved directly (the wheel only reaches
                    // an area through pointer hit-testing, which did not find this one)
                    let focused = self.ctx.memory(|m| m.focused());
                    let own_list = focused.and_then(|id| self.ctx.data(|d| d.get_temp::<Option<egui::Id>>(egui::Id::new(("pad_header_list", id)))).flatten());
                    if std::env::var_os("ZEST_PAD_DEBUG").is_some() { eprintln!("stick: focused={focused:?} own_list={own_list:?} areas={}", Self::scroll_areas(&self.ctx).len()); }
                    let moved = own_list.map_or(false, |aid| {
                        let Some((view, content, _)) = Self::scroll_areas(&self.ctx).into_iter().find(|(_, _, i)| *i == aid) else { return false };
                        let Some(mut st) = egui::scroll_area::State::load(&self.ctx, aid) else { return false };
                        st.jump_y((st.offset.y + self.rs_px).clamp(0.0, (content.height() - view.height()).max(0.0)));
                        st.store(&self.ctx, aid);
                        true
                    });
                    if !moved {
                        // a dialog: egui scrolls the area under the pointer, so put it on the focused control (that also hovers it, which the cursor ring already says)
                        let at = focused.and_then(|id| self.ctx.read_response(id)).map(|r| r.rect.center());
                        if let Some(p) = at { raw.events.push(egui::Event::PointerMoved(p)); }
                        raw.events.push(egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, -self.rs_px), phase: egui::TouchPhase::Move, modifiers: Default::default() });
                    }
                }
                self.ctx.request_repaint();
            } else { self.rs_since = None; self.rs_px = 0.0; self.rs_mult = 1.0; }
        }
        // a dialog is open but the keyboard focus sits on the page behind it (egui's modal layer is a frame late, so the first presses after a dialog opens, or while the
        // menu closes, can land there: the Home nav tabs, the Shuffle link). Take it back, to the last dialog control that had it if there was one.
        if !self.pad_free() {
            if let Some(f) = self.ctx.memory(|m| m.focused()) {
                let behind = self.ctx.read_response(f).map_or(false, |r| r.layer_id.order == egui::Order::Background);
                if behind { self.ctx.memory_mut(|m| { m.surrender_focus(f); if let Some(g) = self.pad_good { m.request_focus(g); } }); }
                else { self.pad_good = Some(f); }
            }
        } else { self.pad_good = None; }
        let in_multiline = ui_kit::MULTILINE_FOCUS.swap(false, std::sync::atomic::Ordering::Relaxed);
        let _grid = ui_kit::PAD_GEOMETRY.swap(false, std::sync::atomic::Ordering::Relaxed);
        // a drop-down list: opening it with the pad puts the focus on its first entry (egui leaves it on the drop-down itself), and closing it gives the focus back to the drop-down
        {
            let mut open = egui::Popup::is_any_open(&self.ctx);
            if std::mem::take(&mut self.pad_popup_close) && open { egui::Popup::close_all(&self.ctx); open = false; self.ctx.request_repaint(); }
            if open && !self.pad_popup_was_open && self.pad_show {
                if let Some(f) = self.ctx.memory(|m| m.focused()) {
                    if let Some(r) = self.ctx.read_response(f) { self.pad_popup = Some(PadPopup { from: f, from_layer: r.layer_id, layer: None, wait: 2 }); }
                }
            }
            if !open && self.pad_popup_was_open {
                if let Some(p) = self.pad_popup.take() { if self.pad_show { let id = p.from; self.ctx.memory_mut(|m| m.request_focus(id)); } }
            }
            self.pad_popup_was_open = open;
            if !open { self.pad_popup = None; }
            let mut enter: Option<(egui::Id, egui::LayerId)> = None;
            if let Some(p) = self.pad_popup.as_mut() {
                if p.layer.is_none() {
                    if p.wait > 0 { p.wait -= 1; self.ctx.request_repaint(); }
                    else {
                        let first = self.ctx.memory(|m| m.focusable_ids()).into_iter().filter_map(|o| self.ctx.read_response(o))
                            .filter(|r| r.layer_id != p.from_layer && r.layer_id.order == egui::Order::Foreground && r.id != p.from)
                            .min_by(|x, y| x.rect.center().y.partial_cmp(&y.rect.center().y).unwrap_or(std::cmp::Ordering::Equal));
                        if let Some(r) = first { p.layer = Some(r.layer_id); enter = Some((r.id, r.layer_id)); }
                    }
                }
            }
            if let Some((id, _)) = enter { self.ctx.memory_mut(|m| m.request_focus(id)); self.ctx.request_repaint(); }
        }
        if let Some((layer, area, top, left)) = self.pad_wrap {
            if left > 0 { self.pad_wrap = Some((layer, area, top, left - 1)); self.ctx.request_repaint(); }
            else {
                self.pad_wrap = None;
                // the list has scrolled to its start (or end): its controls are registered now, so the first (last) of them is the one that was meant
                if let Some((view, _, _)) = Self::scroll_areas_in(&self.ctx, layer).into_iter().find(|(_, _, i)| *i == area) {
                    let best = self.ctx.memory(|m| m.focusable_ids()).into_iter().filter_map(|o| self.ctx.read_response(o)).filter(|r| r.layer_id == layer && view.contains(r.rect.center()))
                        .min_by(|x, y| { let (a, b) = (x.rect.center().y, y.rect.center().y); (if top { a.partial_cmp(&b) } else { b.partial_cmp(&a) }).unwrap_or(std::cmp::Ordering::Equal) });
                    if std::env::var_os("ZEST_PAD_DEBUG").is_some() { eprintln!("pad_wrap take: top={top} view={view:?} best={:?} offset={:?}", best.as_ref().map(|r| r.rect), egui::scroll_area::State::load(&self.ctx, area).map(|s| s.offset)); }
                    if let Some(r) = best { let rid = r.id; self.ctx.memory_mut(|m| m.request_focus(rid)); self.ctx.request_repaint(); }
                }
            }
        }
        // A Tab step (the fallback when egui's search finds nothing, or leaves a list the wrong way) can land inside a scrolling list from outside it: from the dialog's last control
        // back to the first row, say. The press itself was already judged, so look at where the focus ended up now: if it entered a list that is not scrolled to its start (Down) or end
        // (Up), scroll it there, put the focus back and press again, so the first (last) row is the one reached and not whichever row was on screen.
        if let Some((id, dir, left)) = self.pad_tab.take() {
            if !self.pad_free() {
                let now = self.ctx.memory(|m| m.focused());
                // the Tab takes effect a frame or two after it was pushed: until the focus has moved, keep waiting
                if now == Some(id) && left > 0 { self.pad_tab = Some((id, dir, left - 1)); self.ctx.request_repaint(); }
                if std::env::var_os("ZEST_PAD_DEBUG").is_some() { eprintln!("tab-take {dir:?}: id={id:?} now={now:?} a={:?} b={:?}", self.ctx.read_response(id).map(|r| (r.rect, r.layer_id)), now.and_then(|n| self.ctx.read_response(n)).map(|r| (r.rect, r.layer_id))); }
                if let (Some(a), Some(b)) = (self.ctx.read_response(id), now.and_then(|n| self.ctx.read_response(n))) {
                    if now != Some(id) && a.layer_id == b.layer_id {
                        let areas = Self::scroll_areas_in(&self.ctx, a.layer_id);
                        let inner = areas.iter().filter(|(v, _, _)| v.expand2(egui::vec2(0.0, 12.0)).intersects(b.rect) && !v.intersects(a.rect))
                            .min_by(|x, y| (x.1.width() * x.1.height()).partial_cmp(&(y.1.width() * y.1.height())).unwrap_or(std::cmp::Ordering::Equal));
                        if std::env::var_os("ZEST_PAD_DEBUG").is_some() { eprintln!("tab-check {dir:?}: a={:?} b={:?} inner={:?}", a.rect, b.rect, inner.map(|x| (x.0, x.1))); }
                        if let Some((view, content, area_id)) = inner {
                            if let Some(mut st) = egui::scroll_area::State::load(&self.ctx, *area_id) {
                                let target = if dir == Nav::Down { 0.0 } else { (content.height() - view.height()).max(0.0) };
                                if (st.offset.y - target).abs() > 0.5 {
                                    st.jump_y(target);
                                    st.store(&self.ctx, *area_id);
                                    self.ctx.memory_mut(|m| m.request_focus(id));
                                    self.pad_wrap = Some((a.layer_id, *area_id, dir == Nav::Down, 2));
                                    self.ctx.request_repaint();
                                }
                            }
                        }
                    }
                }
            }
        }
        // Up / Down that egui answered with a control in the SAME ROW (a row of pills: Home, Library, Pick 6) moved sideways, which is neither direction asked for. Go to the nearest
        // control in the next row that way instead (the closest across when the row has several), or stay put when there is none.
        if let Some((id, dir)) = self.pad_dir {
            if matches!(dir, Nav::Up | Nav::Down) && !self.pad_free() {
                let now = self.ctx.memory(|m| m.focused());
                if let (Some(a), Some(b)) = (self.ctx.read_response(id), now.and_then(|n| self.ctx.read_response(n))) {
                    if now != Some(id) && a.layer_id == b.layer_id && (b.rect.center().y - a.rect.center().y).abs() < 6.0 && !egui::Popup::is_any_open(&self.ctx) {
                        let (ay, ax) = (a.rect.center().y, a.rect.center().x);
                        let mut cands: Vec<egui::Response> = self.ctx.memory(|m| m.focusable_ids()).into_iter().filter(|o| *o != id).filter_map(|o| self.ctx.read_response(o))
                            .filter(|r| r.layer_id == a.layer_id && if dir == Nav::Down { r.rect.center().y > ay + 6.0 } else { r.rect.center().y < ay - 6.0 }).collect();
                        cands.sort_by(|x, y| (x.rect.center().y - ay).abs().partial_cmp(&(y.rect.center().y - ay).abs()).unwrap_or(std::cmp::Ordering::Equal));
                        let target = cands.first().map(|f| { let row_y = f.rect.center().y; cands.iter().filter(|r| (r.rect.center().y - row_y).abs() < 6.0).min_by(|x, y| (x.rect.center().x - ax).abs().partial_cmp(&(y.rect.center().x - ax).abs()).unwrap_or(std::cmp::Ordering::Equal)).map(|r| r.id).unwrap_or(f.id) });
                        self.ctx.memory_mut(|m| m.request_focus(target.unwrap_or(id)));
                        self.pad_dir = None;
                        self.ctx.request_repaint();
                    }
                }
            }
        }
        if let Some((id, dir)) = self.pad_dir.take() {
            // the last Up/Down in a dialog left the focus where it was (egui only looks in a cone: from the close cross nothing lies below it): step in creation order instead
            let now = self.ctx.memory(|m| m.focused());
            // egui's geometric search ignores layers: from a dialog it can land on the page behind it (the top bar's EXIT). Put the focus back and step in order instead.
            let popup_open = egui::Popup::is_any_open(&self.ctx);   // an open dropdown is its own layer on purpose: the focus may enter its list
            let escaped = !popup_open && now.map_or(false, |n| n != id && match (self.ctx.read_response(id), self.ctx.read_response(n)) { (Some(a), Some(b)) => a.layer_id != b.layer_id && b.layer_id.order < a.layer_id.order, _ => false });   // only DOWN a layer (a dialog to the page behind it): the menu and its submenu are both Foreground
            if escaped { self.ctx.memory_mut(|m| m.request_focus(id)); }
            // Down from the last visible control of a long, scrolling dialog made egui wrap round to the first one (the controls below are clipped, so it never sees them): that is a
            // move the wrong way. Put the focus back and step in creation order instead, which reaches the clipped ones (and scrolls them into view).
            let wrapped = !escaped && matches!(dir, Nav::Up | Nav::Down) && now.map_or(false, |n| n != id && match (self.ctx.read_response(id), self.ctx.read_response(n)) {
                (Some(a), Some(b)) => if dir == Nav::Down { b.rect.center().y < a.rect.center().y - 1.0 } else { b.rect.center().y > a.rect.center().y + 1.0 },
                _ => false });
            // Down past the last control of a scrolling list that has nothing after it (a drop-down's list, say) made egui wrap to the first control ON SCREEN, not the list's first (and the
            // other way for Up): scroll the list to its start (end) and put the focus on the control that is first (last) once it has scrolled
            // Down on a drop-down's last entry (Up on its first) must not leave the list open and the focus outside it: wrap to the other end of the list
            let mut popup_wrap = false;
            if let Some(pl) = self.pad_popup.as_ref().and_then(|p| p.layer) {
                if popup_open {
                    if let Some(a) = self.ctx.read_response(id) {
                        if a.layer_id == pl {
                            let mut items: Vec<egui::Response> = self.ctx.memory(|m| m.focusable_ids()).into_iter().filter_map(|o| self.ctx.read_response(o)).filter(|r| r.layer_id == pl).collect();
                            items.sort_by(|x, y| x.rect.center().y.partial_cmp(&y.rect.center().y).unwrap_or(std::cmp::Ordering::Equal));
                            // egui moved the focus out of the list (or the wrong way inside it)...
                            let left = now != Some(id) && now.and_then(|n| self.ctx.read_response(n)).map_or(false, |b| b.layer_id != pl || (dir == Nav::Down && b.rect.center().y < a.rect.center().y - 1.0) || (dir == Nav::Up && b.rect.center().y > a.rect.center().y + 1.0));
                            // ...or found nothing to move to because this is the last (first) entry: that wraps too
                            let at_end = now == Some(id) && match dir { Nav::Down => items.last().map_or(false, |l| l.id == id), Nav::Up => items.first().map_or(false, |f| f.id == id), _ => false };
                            if left || at_end {
                                let target = if dir == Nav::Down { items.first() } else { items.last() };
                                if let Some(t) = target {
                                    let tid = t.id;
                                    self.ctx.memory_mut(|m| m.request_focus(tid));
                                    popup_wrap = true;
                                    // the target may be scrolled out of sight: put the list at its start (end) so the entry is on screen
                                    for (view, content, area_id) in Self::scroll_areas_in(&self.ctx, pl) {
                                        if let Some(mut st) = egui::scroll_area::State::load(&self.ctx, area_id) {
                                            st.jump_y(if dir == Nav::Down { 0.0 } else { (content.height() - view.height()).max(0.0) });
                                            st.store(&self.ctx, area_id);
                                        }
                                    }
                                    self.ctx.request_repaint();
                                }
                            }
                        }
                    }
                }
            }
            let mut wrapped_in_list = false;
            if wrapped && !popup_wrap {
                if let (Some(a), Some(b)) = (self.ctx.read_response(id), now.and_then(|n| self.ctx.read_response(n))) {
                    let areas = Self::scroll_areas_in(&self.ctx, a.layer_id);
                    let inner = areas.iter().filter(|(_, c, _)| c.contains(a.rect.center()) && c.contains(b.rect.center()))
                        .min_by(|x, y| (x.1.width() * x.1.height()).partial_cmp(&(y.1.width() * y.1.height())).unwrap_or(std::cmp::Ordering::Equal));
                    if let Some((view, content, area_id)) = inner {
                        if let Some(mut st) = egui::scroll_area::State::load(&self.ctx, *area_id) {
                            let top = dir == Nav::Down;
                            st.jump_y(if top { 0.0 } else { (content.height() - view.height()).max(0.0) });
                            st.store(&self.ctx, *area_id);
                            self.pad_wrap = Some((a.layer_id, *area_id, top, 2));
                            self.ctx.request_repaint();
                            wrapped_in_list = true;
                        }
                    }
                }
            }
            // Up / Down left a scrolling list for a control outside it (a fixed button above the list) while the list still has controls scrolled out of view that way:
            // those are not visible to egui's search, so step in creation order, which reaches them (and scrolls them into view).
            let skipped = !escaped && !wrapped && matches!(dir, Nav::Up | Nav::Down) && now.map_or(false, |n| n != id && match (self.ctx.read_response(id), self.ctx.read_response(n)) {
                (Some(a), Some(b)) => {
                    let areas = Self::scroll_areas_in(&self.ctx, a.layer_id);
                    areas.iter().filter(|(_, c, _)| c.contains(a.rect.center())).min_by(|x, y| (x.1.width() * x.1.height()).partial_cmp(&(y.1.width() * y.1.height())).unwrap_or(std::cmp::Ordering::Equal))
                        .map_or(false, |(view, content, _)| !view.intersects(b.rect) && self.ctx.memory(|m| m.focusable_ids()).into_iter().filter(|o| *o != id).filter_map(|o| self.ctx.read_response(o)).any(|o|
                            o.layer_id == a.layer_id && content.contains(o.rect.center()) && !view.contains(o.rect.center()) && if dir == Nav::Down { o.rect.center().y > a.rect.center().y } else { o.rect.center().y < a.rect.center().y }))
                }
                _ => false });
            // A list that builds only the rows on screen (HowLongToBeat's) has no hidden controls to step to: when Up / Down finds nothing in the list, or leaves it, and the list can
            // still scroll that way, scroll it a row and press the direction again next frame, when the next row exists.
            // Down or Up from outside a scrolling list into it: egui only knows the controls on screen (the others are not registered), so it lands on the first VISIBLE one, which is
            // not the list's first (or last) when the list was scrolled. Scroll it to its start (Down) or end (Up) and press again, so the real first or last control is the one reached.
            let mut entered = wrapped_in_list || popup_wrap;
            if !escaped && matches!(dir, Nav::Up | Nav::Down) && !self.pad_free() {
                if let (Some(a), Some(b)) = (self.ctx.read_response(id), now.and_then(|n| self.ctx.read_response(n))) {
                    // not while a row-by-row scroll from this very control is under way: a tall control (the Controller test's display) scrolls out of view during it and would look like one outside the list
                    if now != Some(id) && a.layer_id == b.layer_id && self.pad_stepping != Some(id) {
                        let areas = Self::scroll_areas_in(&self.ctx, a.layer_id);
                        // the VISIBLE part, not the content: a list scrolled down has a content rectangle that runs up over the dialog's header, so the control we came from looked "inside" it
                        let inner = areas.iter().filter(|(v, _, _)| v.expand2(egui::vec2(0.0, 12.0)).intersects(b.rect) && !v.intersects(a.rect))   // the view grown a little: a row lying just under the dialog's header (hidden, but registered) is the one egui picks first when the list was scrolled, and it does not touch the view itself. Intersects, not centre-inside: a row half hidden at the list's top edge (its centre just above the view) is the one egui picks first when the list is scrolled down
                            .min_by(|x, y| (x.1.width() * x.1.height()).partial_cmp(&(y.1.width() * y.1.height())).unwrap_or(std::cmp::Ordering::Equal));
                        if std::env::var_os("ZEST_PAD_DEBUG").is_some() { eprintln!("enter-check {dir:?}: a={:?} b={:?} areas={} inner={:?} offset={:?}", a.rect, b.rect, areas.len(), inner.map(|x| (x.0, x.1)), inner.and_then(|x| egui::scroll_area::State::load(&self.ctx, x.2)).map(|s| s.offset)); }
                        if let Some((view, content, area_id)) = inner {
                            if let Some(mut st) = egui::scroll_area::State::load(&self.ctx, *area_id) {
                                let target = if dir == Nav::Down { 0.0 } else { (content.height() - view.height()).max(0.0) };
                                if (st.offset.y - target).abs() > 0.5 {
                                    st.jump_y(target);
                                    st.store(&self.ctx, *area_id);
                                    self.pad_wrap = Some((a.layer_id, *area_id, dir == Nav::Down, 2));   // the rows of the scrolled list are registered a frame later: wait, then take its first (last) one
                                    self.ctx.request_repaint();
                                    entered = true;
                                }
                            }
                        }
                    }
                }
            }
            let mut vstep = false;
            if !escaped && !entered && matches!(dir, Nav::Up | Nav::Down) && !self.pad_free() {   // before `skipped`: a list that can still scroll that way scrolls a row (Tab cannot reach rows that were never drawn)
                if let Some(a) = self.ctx.read_response(id) {
                    let moved_in_list = |areas: &[(egui::Rect, egui::Rect, egui::Id)]| now.and_then(|n| self.ctx.read_response(n)).map_or(false, |b| now != Some(id) && !wrapped && areas.iter().any(|(v, c, _)| c.contains(a.rect.center()) && v.contains_rect(b.rect)));   // the VIEW, not the content: the content rectangle runs on below the list, over the buttons under it
                    let areas = Self::scroll_areas_in(&self.ctx, a.layer_id);
                    if !moved_in_list(&areas) {
                        if let Some((view, content, area_id)) = areas.iter().filter(|(_, c, _)| c.contains(a.rect.center())).min_by(|x, y| (x.1.width() * x.1.height()).partial_cmp(&(y.1.width() * y.1.height())).unwrap_or(std::cmp::Ordering::Equal)) {
                            if let Some(mut st) = egui::scroll_area::State::load(&self.ctx, *area_id) {
                                let max = (content.height() - view.height()).max(0.0);
                                let room = if dir == Nav::Down { max - st.offset.y } else { st.offset.y };
                                // a control of this layer already on screen further that way: egui's search missed it (the page behind the dialog has nearer widgets, which the pad refuses), so step to it in
                                // creation order instead of scrolling a row at a time forever (Plugins on the Library page)
                                // a control of this layer already on screen further that way: egui's search missed it (the page behind the dialog has nearer widgets, which the pad refuses), so step to it in
                                // creation order instead of scrolling a row at a time forever (Plugins on the Library page)
                                let next_on_screen = self.ctx.memory(|m| m.focusable_ids()).into_iter().filter(|o| *o != id).filter_map(|o| self.ctx.read_response(o))
                                    .any(|o| o.layer_id == a.layer_id && view.contains_rect(o.rect) && if dir == Nav::Down { o.rect.center().y > a.rect.center().y + 1.0 } else { o.rect.center().y < a.rect.center().y - 1.0 });
                                if room > 0.5 && !next_on_screen {
                                    st.jump_y((st.offset.y + if dir == Nav::Down { 1.0 } else { -1.0 } * (a.rect.height() + 6.0).min(room.max(1.0))).clamp(0.0, max));
                                    st.store(&self.ctx, *area_id);
                                    self.pad_queue.lock().unwrap().push_front((dir, true));
                                    self.ctx.request_repaint();
                                    vstep = true;
                                }
                            }
                        }
                    }
                }
            }
            if std::env::var_os("ZEST_PAD_DEBUG2").is_some() {
                if let Some(a) = self.ctx.read_response(id) {
                    let mut below: Vec<(f32, String)> = self.ctx.memory(|m| m.focusable_ids()).into_iter().filter(|o| *o != id).filter_map(|o| self.ctx.read_response(o)).filter(|o| o.rect.center().y > a.rect.center().y).map(|o| (o.rect.center().y, format!("{:?} {:?} y{:.0} x{:.0} {:?} {:?}", o.layer_id.order, o.id, o.rect.center().y, o.rect.center().x, o.rect.size(), o.sense))).collect();
                    below.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap_or(std::cmp::Ordering::Equal));
                    eprintln!("  from {:?} {:?} y{:.0} x{:.0}; below: {:?}", a.layer_id.order, id, a.rect.center().y, a.rect.center().x, below.iter().take(4).map(|b| b.1.clone()).collect::<Vec<_>>());
                }
            }
            if std::env::var_os("ZEST_PAD_DEBUG").is_some() { eprintln!("pad_dir {dir:?}: id={id:?} now={now:?} escaped={escaped} wrapped={wrapped} skipped={skipped} vstep={vstep} areas={} view/content/a/b={:?}", Self::scroll_areas(&self.ctx).len(), (Self::scroll_areas(&self.ctx).first().map(|x| (x.0, x.1)), self.ctx.read_response(id).map(|r| r.rect), now.and_then(|n| self.ctx.read_response(n)).map(|r| (r.rect, r.sense)))); }
            self.pad_stepping = if vstep { Some(id) } else { None };
            if (wrapped || skipped || vstep || entered) && !popup_wrap { self.ctx.memory_mut(|m| m.request_focus(id)); }
            // egui found nothing in that direction (its search looks in a cone, and a control in the next row can lie outside it): Tab would go to the next control in CREATION order,
            // which is often the one beside this (CONFIRM ALL -> RE-SCRAPE, a tab -> the next tab). Go to the nearest control in the next row that way instead, when there is one close by.
            let mut stepped_by_row = false;
            if !self.pad_free() && !vstep && !entered && now == Some(id) && !wrapped && !skipped && !escaped && !in_multiline && matches!(dir, Nav::Up | Nav::Down) {
                if let Some(a) = self.ctx.read_response(id) {
                    let (ay, ax) = (a.rect.center().y, a.rect.center().x);
                    let mut cands: Vec<egui::Response> = self.ctx.memory(|m| m.focusable_ids()).into_iter().filter(|o| *o != id).filter_map(|o| self.ctx.read_response(o))
                        .filter(|r| r.layer_id == a.layer_id && r.rect.height() > 0.0 && if dir == Nav::Down { r.rect.center().y > ay + 6.0 && r.rect.center().y < ay + 160.0 } else { r.rect.center().y < ay - 6.0 && r.rect.center().y > ay - 160.0 }).collect();
                    cands.sort_by(|x, y| (x.rect.center().y - ay).abs().partial_cmp(&(y.rect.center().y - ay).abs()).unwrap_or(std::cmp::Ordering::Equal));
                    if let Some(f) = cands.first() {
                        let row_y = f.rect.center().y;
                        let t = cands.iter().filter(|r| (r.rect.center().y - row_y).abs() < 6.0).min_by(|x, y| (x.rect.center().x - ax).abs().partial_cmp(&(y.rect.center().x - ax).abs()).unwrap_or(std::cmp::Ordering::Equal)).map(|r| r.id).unwrap_or(f.id);
                        self.ctx.memory_mut(|m| m.request_focus(t));
                        self.ctx.request_repaint();
                        stepped_by_row = true;
                    }
                }
            }
            if !stepped_by_row && !self.pad_free() && !vstep && !entered && (now == Some(id) || wrapped || skipped || escaped) && !in_multiline && matches!(dir, Nav::Up | Nav::Down) {
                let mods = if dir == Nav::Up { egui::Modifiers::SHIFT } else { Default::default() };
                for pr in [true, false] { raw.events.push(egui::Event::Key { key: egui::Key::Tab, physical_key: Some(egui::Key::Tab), pressed: pr, repeat: false, modifiers: mods }); }
                self.pad_tab = Some((id, dir, 3));
            }
        }
        // a text box in a dialog: the pad outlines it (the ring) and A starts the typing; any keyboard typing or a click in it counts as typing too
        let text_id = if self.pad_free() || self.menu_open { None } else { self.ctx.memory(|m| m.focused()).filter(|id| egui::text_edit::TextEditState::load(&self.ctx, *id).is_some()) };
        if text_id.is_none() { self.pad_typing = false; }
        else if raw.events.iter().any(|e| matches!(e, egui::Event::Text(_) | egui::Event::Key { .. } | egui::Event::PointerButton { pressed: true, .. } | egui::Event::Paste(_))) { self.pad_typing = true; }
        let on_slider = ui_kit::SLIDER_FOCUS.swap(false, std::sync::atomic::Ordering::Relaxed) && !self.pad_free() && !self.menu_open && text_id.is_none();
        if !on_slider { self.pad_adjust = false; }
        ui_kit::SLIDER_ADJUSTING.store(self.pad_adjust, std::sync::atomic::Ordering::Relaxed);
        for (n, pressed) in navs {
            // Edit Home Page: while a bar is being resized with the pad, the directions and A/B go to the editor and nothing else sees them
            if let Some(Modal::HomeEdit(ed)) = &mut self.modal {
                if ed.pad_dragging() && matches!(n, Nav::Up | Nav::Down | Nav::Left | Nav::Right | Nav::Confirm | Nav::Back) { if pressed { ed.pad_key(n); } continue; }
                // B backs out of a picker, the priority panel or a question before it can close the dialog
                if n == Nav::Back && ed.pad_back_open() { if pressed { ed.pad_back(); } continue; }
            }
            if on_slider {
                if !self.pad_adjust {
                    match n { Nav::Confirm => { if pressed { self.pad_adjust = true; } continue; } Nav::Left | Nav::Right => { if pressed { self.pad_move = Some(if n == Nav::Left { egui::FocusDirection::Left } else { egui::FocusDirection::Right }); } continue; } _ => {} }
                } else {
                    match n { Nav::Confirm | Nav::Back => { if pressed { self.pad_adjust = false; } continue; } Nav::Up | Nav::Down => { if pressed { self.pad_adjust = false; } } _ => {} }
                }
            }
            // any pad press shows the cursor again (the mouse hides it; a control the mouse clicked still holds egui's focus, so the "first press" branch below never ran)
            if pressed && !self.pad_free() && matches!(n, Nav::Up | Nav::Down | Nav::Left | Nav::Right | Nav::Confirm) {
                let was_hidden = !self.pad_show;
                self.pad_show = true;
                // the first press after the mouse was used starts where the mouse left off, as on the game cards: the control it was last over takes the focus and that press does nothing more
                if was_hidden && text_id.is_none() {
                    let h = self.pad_hover.filter(|h| self.ctx.read_response(*h).map_or(false, |r| r.layer_id.order != egui::Order::Background));
                    if let Some(h) = h { self.ctx.memory_mut(|m| m.request_focus(h)); self.ctx.request_repaint(); continue; }
                }
            }
            if text_id.is_some() && !self.menu_open {
                fn tap(raw: &mut egui::RawInput, k: egui::Key, mods: egui::Modifiers) { for pr in [true, false] { raw.events.push(egui::Event::Key { key: k, physical_key: Some(k), pressed: pr, repeat: false, modifiers: mods }); } }
                if !self.pad_typing {
                    match n {
                        Nav::Confirm => { if pressed { self.pad_typing = true; } continue; }
                        Nav::Up | Nav::Down | Nav::Left | Nav::Right => {
                            if pressed {
                                self.pad_move = Some(match n { Nav::Up => egui::FocusDirection::Up, Nav::Down => egui::FocusDirection::Down, Nav::Left => egui::FocusDirection::Left, _ => egui::FocusDirection::Right });
                                // the same follow-up every other control gets (stepping to controls scrolled out of a list, the wrap correction, the Tab fallback); this branch used to `continue` before it
                                if let Some(tid) = text_id { self.pad_dir = Some((tid, n)); }
                            }
                            continue;
                        }
                        _ => {}
                    }
                } else {
                    match n {
                        Nav::Back => { if pressed { self.pad_typing = false; } continue; }   // B stops the typing; the dialog stays open
                        Nav::Up | Nav::Down if !in_multiline => { if pressed { self.pad_typing = false; tap(raw, egui::Key::Tab, if n == Nav::Up { egui::Modifiers::SHIFT } else { Default::default() }); } continue; }
                        Nav::Confirm if !in_multiline => { if pressed { self.pad_typing = false; tap(raw, egui::Key::Enter, Default::default()); } continue; }
                        _ => {}
                    }
                }
            }
            // Select (PlayDate's Back button) opens and closes the menu, on any page, but never from inside a dialog
            if n == Nav::Select {
                if pressed {
                    if self.menu_open { self.menu_open = false; self.pad_menu = false; }
                    else if free_any { self.pad_nav = None; self.pad_bar = None; self.menu_open = true; self.pad_menu = true; }
                }
                continue;
            }
            if free_any {
                if matches!(n, Nav::PagePrev | Nav::PageNext | Nav::Back) {
                    // LB / RB switch the page, B opens the menu (or leaves the nav bar)
                    if pressed {
                        let here = PAGES.iter().position(|p| *p == self.page).unwrap_or(0);
                        match n {
                            Nav::Back => { if self.pad_bar.is_some() { self.pad_bar = None; } else if self.pad_nav.is_some() { self.pad_nav = None; } else { self.menu_open = true; self.pad_menu = true; } }
                            _ => {
                                let to = if n == Nav::PagePrev { here.saturating_sub(1) } else { (here + 1).min(2) };
                                if to != here { self.want_page = Some(PAGES[to]); self.pad_cur = None; self.pad_nav = None; self.pad_bar = None; }
                            }
                        }
                    }
                    continue;
                }
                // the first press after the mouse was used starts on the nav bar or Library toolbar control the mouse is resting on, and does nothing more (as on the cards)
                if pressed && !self.pad_show && matches!(n, Nav::Up | Nav::Down | Nav::Left | Nav::Right | Nav::Confirm) {
                    if let Some(p) = self.ctx.input(|i| i.pointer.latest_pos()) {
                        let on_nav = self.pad_nav_rects.iter().position(|r| r.contains(p));
                        let on_bar = if self.page == Page::Library { self.pad_bar_items.iter().position(|r| r.expand(2.0).contains(p)) } else { None };
                        if on_nav.is_some() || on_bar.is_some() { self.pad_nav = on_nav; self.pad_bar = if on_nav.is_some() { None } else { on_bar }; self.pad_show = true; self.ctx.request_repaint(); continue; }
                    }
                }
                if let (Some(i), true) = (self.pad_bar, self.pad_show) {
                    // the Library toolbar: sideways walks the controls, A clicks the one under the ring
                    if pressed {
                        let last = self.pad_bar_items.len().saturating_sub(1);
                        match n {
                            Nav::Left => self.pad_bar = Some(i.saturating_sub(1)),
                            Nav::Right => self.pad_bar = Some((i + 1).min(last)),
                            Nav::Up => { self.pad_bar = None; self.pad_nav = Some(self.page_nav_index()); }
                            Nav::Down => self.pad_bar = None,
                            Nav::Confirm => if let Some(rc) = self.pad_bar_items.get(i) {
                                let pos = rc.center();
                                raw.events.push(egui::Event::PointerMoved(pos));
                                for pr in [true, false] { raw.events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: pr, modifiers: Default::default() }); }
                            },
                            _ => {}
                        }
                    }
                    continue;
                }
                if let (Some(i), true) = (self.pad_nav, self.pad_show) {
                    if pressed {
                        match n {
                            Nav::Left => self.pad_nav = Some(i.saturating_sub(1)),
                            Nav::Right => self.pad_nav = Some((i + 1).min(self.pad_nav_rects.len().saturating_sub(1).max(3))),
                            Nav::Down => {
                                self.pad_nav = None;
                                if self.page == Page::Library && !self.pad_bar_items.is_empty() { self.pad_bar = Some(self.pad_bar_items.len() / 2); }
                            }
                            Nav::Confirm => {
                                match i { 0 => { self.menu_open = true; self.pad_menu = true; } 4 => self.want_fullscreen = true, 5 => self.want_exit = true, _ => { self.want_page = Some(PAGES[i - 1]); self.pad_cur = None; } }
                                self.pad_nav = None;
                            }
                            _ => {}
                        }
                    }
                    continue;
                }
            }
            if self.menu_open {
                // the menu is a column: sideways does nothing (it used to walk egui's focus out of the menu, leaving no highlight to get back with),
                // and a press with nothing focused puts the focus on the first item again
                if matches!(n, Nav::Left | Nav::Right) { continue; }
                if matches!(n, Nav::Up | Nav::Down | Nav::Confirm) && self.ctx.memory(|m| m.focused()).is_none() { if pressed { self.pad_menu = true; } continue; }
            }
            // a dialog is open and nothing in it has the focus yet: the first direction or A press puts it on the first control (Tab), as the menu does above
            if !free_any && !self.menu_open && matches!(n, Nav::Up | Nav::Down | Nav::Left | Nav::Right | Nav::Confirm) && self.ctx.memory(|m| m.focused()).is_none() {
                if pressed { self.pad_show = true; for pr in [true, false] { raw.events.push(egui::Event::Key { key: egui::Key::Tab, physical_key: Some(egui::Key::Tab), pressed: pr, repeat: false, modifiers: Default::default() }); } }
                continue;
            }
            if free && !matches!(n, Nav::Back) {
                if pressed { out.push(n); }
                continue;
            }
            if !self.pad_free() && pressed && matches!(n, Nav::Up | Nav::Down | Nav::Left | Nav::Right) { if let Some(id) = self.ctx.memory(|m| m.focused()) { self.pad_dir = Some((id, n)); } }
            // Up/Down and Left/Right all go by position now (the vendored egui looks row by row); see `find_widget_in_direction`
            if n == gamepad::Nav::Confirm && pressed && self.pad_popup.as_ref().map_or(false, |p| p.layer.is_some()) && self.ctx.memory(|m| m.focused()).and_then(|f| self.ctx.read_response(f)).map_or(false, |r| Some(r.layer_id) == self.pad_popup.as_ref().and_then(|p| p.layer)) { self.pad_popup_close = true; }
            if let Some(key) = gamepad::egui_key(n) { raw.events.push(egui::Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers: Default::default() }); }
        }
        out
    }

    fn page_nav_index(&self) -> usize { match self.page { Page::Home => 1, Page::Library => 2, Page::Pick => 3 } }

    /// The cursor ring: the theme accent, drawn above the page but under nothing (it is hidden whenever a dialog is open).
    /// Inside a dialog the pad moves egui's focus; whichever widget has it gets the cursor ring, whatever it is (most dialog controls are painted by hand and draw
    /// nothing for focus themselves). The rectangle is last frame's, so a widget that has just appeared gets its ring one frame later.
    /// The lists that scroll this pass: (what you see of it, everything in it), as the vendored egui records them.
    /// The lists that scroll in one layer: a control in the hamburger menu or a dialog belongs to no list of the page behind it, even where that list's rectangle lies under it.
    fn scroll_areas_in(ctx: &egui::Context, layer: egui::LayerId) -> Vec<(egui::Rect, egui::Rect, egui::Id)> {
        let nr = ctx.cumulative_frame_nr();
        let layers: Vec<(egui::Id, egui::LayerId)> = ctx.data(|d| d.get_temp::<(u64, Vec<(egui::Id, egui::LayerId)>)>(egui::Id::new("zest_scroll_layers"))).filter(|(n, _)| *n == nr || *n + 1 == nr).map(|(_, v)| v).unwrap_or_default();
        // a list that holds nothing to focus (PAGYWOSG's 'additional games' names) is not a list the pad enters, scrolls or wraps in: the press goes to the next control in one step
        Self::scroll_areas(ctx).into_iter().filter(|(_, _, id)| layers.iter().any(|(i, l)| i == id && *l == layer)).filter(|(_, _, id)| !ctx.data(|d| d.get_temp::<bool>(egui::Id::new(("pad_no_controls", *id)))).unwrap_or(false)).collect()
    }
    fn scroll_areas(ctx: &egui::Context) -> Vec<(egui::Rect, egui::Rect, egui::Id)> {
        let nr = ctx.cumulative_frame_nr();
        if std::env::var_os("ZEST_PAD_DEBUG").is_some() { let raw = ctx.data(|d| d.get_temp::<(u64, Vec<(egui::Rect, egui::Rect, egui::Id)>)>(egui::Id::new("zest_scroll_areas"))); eprintln!("scroll_areas: frame {nr}, stored {:?}", raw.map(|(n, v)| (n, v.len()))); }
        ctx.data(|d| d.get_temp::<(u64, Vec<(egui::Rect, egui::Rect, egui::Id)>)>(egui::Id::new("zest_scroll_areas"))).filter(|(n, _)| *n == nr || *n + 1 == nr).map(|(_, v)| v).unwrap_or_default()   // this frame, or the one just before it (the key handler runs between frames)
    }

    /// PlayDate's `_refocusAfterScroll`: when the wheel (or a scroll bar) has taken the card the pad is on out of view, the ring moves to the card in the first row in view, in the same column.
    fn lib_pad_refocus(&mut self) {
        if !self.pad_show || self.list_mode || self.scroll_to.is_some() || self.pad_nav.is_some() || self.pad_bar.is_some() { return; }
        let Some((rect, sy)) = self.lib_grid else { return };
        let Some(cur) = self.pad_cur.and_then(|a| self.core.games.iter().position(|g| g.appid == a)) else { return };
        let rh = row_height(self.core.card_w);
        let live: Vec<&grid_core::Row> = self.core.rows.iter().filter(|r| r.count > 0).collect();
        let Some(ri) = live.iter().position(|r| cur >= r.first && cur < r.first + r.count) else { return };
        let top = live[ri].y - sy;
        let visible = (top + rh).min(rect.height()) - top.max(0.0);
        if visible >= rh * 0.5 { return; }
        let nr = live.partition_point(|r| r.y + rh <= sy).min(live.len() - 1);
        let col = cur - live[ri].first;
        let to = live[nr].first + col.min(live[nr].count - 1);
        self.pad_cur = self.core.games.get(to).map(|g| g.appid);
    }

    fn pad_focus_ring(&self, ctx: &egui::Context) {
        if std::env::var_os("ZEST_PAD_DEBUG").is_some() { eprintln!("focus ring: show={} free={} focused={:?} rect={:?}", self.pad_show, self.pad_free(), ctx.memory(|m| m.focused()), ctx.memory(|m| m.focused()).and_then(|id| ctx.read_response(id)).map(|r| r.rect)); }
        if !self.pad_show || self.pad_free() || self.menu_open { return; }
        let Some(id) = ctx.memory(|m| m.focused()) else { return };
        let Some(resp) = ctx.read_response(id) else { return };
        if resp.layer_id.order == egui::Order::Background { return; }   // a page widget behind the dialog: no ring
        // a widget scrolled out of its list (a button far down the Blacklist): bring it into view and draw nothing meanwhile, or the ring floats outside the dialog
        let seen = resp.interact_rect;
        if !(seen.is_positive() && seen.height() >= resp.rect.height() - 1.0 && seen.width() >= resp.rect.width() - 1.0) {
            resp.scroll_to_me(None);
            // `scroll_to_me` only reaches a list while it is being built, so a control the pad arrived at in some other way (the step-in-order fallback) stayed out of view: move the list itself.
            // Not during a row-by-row scroll from another control, which is moving the list on purpose.
            if self.pad_stepping.is_none() {
                let areas = Self::scroll_areas_in(ctx, resp.layer_id);
                if let Some((view, content, area_id)) = areas.iter().filter(|(_, c, _)| c.contains(resp.rect.center())).min_by(|x, y| (x.1.width() * x.1.height()).partial_cmp(&(y.1.width() * y.1.height())).unwrap_or(std::cmp::Ordering::Equal)) {
                    if let Some(mut st) = egui::scroll_area::State::load(ctx, *area_id) {
                        let margin = 10.0;
                        let dy = if resp.rect.height() + 2.0 * margin >= view.height() || resp.rect.min.y < view.min.y { resp.rect.min.y - view.min.y - margin } else if resp.rect.max.y > view.max.y { resp.rect.max.y - view.max.y + margin } else { 0.0 };
                        let max = (content.height() - view.height()).max(0.0);
                        let to = (st.offset.y + dy).clamp(0.0, max);
                        if (to - st.offset.y).abs() > 0.5 { st.jump_y(to); st.store(ctx, *area_id); }
                    }
                }
            }
            ctx.request_repaint(); return;
        }
        // the pad just arrived on the first or last control of a scrolling list: scroll that list to its start or end, so the text around the control shows too
        static LAST_RING: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        if LAST_RING.swap(id.value(), std::sync::atomic::Ordering::Relaxed) != id.value() {
            let areas = Self::scroll_areas_in(ctx, resp.layer_id);
            let here = resp.rect.center();
            // the innermost list (smallest content) that holds this control
            if let Some((_, content, _)) = areas.iter().filter(|(_, c, _)| c.contains(here)).min_by(|a, b| (a.1.width() * a.1.height()).partial_cmp(&(b.1.width() * b.1.height())).unwrap_or(std::cmp::Ordering::Equal)) {
                let (mut first, mut last) = (true, true);
                for other in ctx.memory(|m| m.focusable_ids()) {
                    if other == id { continue; }
                    let Some(o) = ctx.read_response(other) else { continue };
                    if o.layer_id != resp.layer_id || !o.rect.is_positive() || !content.contains(o.rect.center()) { continue; }
                    if o.rect.center().y < here.y - 1.0 { first = false; }
                    if o.rect.center().y > here.y + 1.0 { last = false; }
                }
                // only when the control really sits at that end of the list: a list that builds just the rows on screen shows its top visible row as "first" wherever it is scrolled to
                let near = 12.0 * ui_kit::rem(ctx);
                let at_start = resp.rect.min.y - content.min.y < near;
                let at_end = content.max.y - resp.rect.max.y < near;
                if first != last && if first { at_start } else { at_end } { resp.scroll_to_edge(first); ctx.request_repaint(); }
            }
        }
        let accent = egui::Color32::from_rgb(self.theme.accent.0, self.theme.accent.1, self.theme.accent.2);
        let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("pad_focus_ring")));
        let r = resp.rect.expand(2.0);
        p.rect_stroke(r.expand(1.0), 4, egui::Stroke::new(3.0_f32, egui::Color32::BLACK), egui::StrokeKind::Outside);
        p.rect_stroke(r, 4, egui::Stroke::new(2.0_f32, accent), egui::StrokeKind::Outside);
    }

    /// The cursor ring around a game card: tight against the card, or outside its completion-status outline (3 px) when it has one.
    /// `outline_radius` is the corner radius of the card's completion outline (None = no outline): the ring wraps it, so its corners are that radius plus the outline's 3 px.
    fn pad_ring_card(&self, ctx: &egui::Context, card: egui::Rect, clip: egui::Rect, outline_radius: Option<f32>) { self.pad_ring_draw(ctx, if outline_radius.is_some() { card.expand(3.0) } else { card }, clip, true, outline_radius.map_or(4.0, |r| r + 3.0)); }

    fn pad_ring_draw(&self, ctx: &egui::Context, r: egui::Rect, clip: egui::Rect, card: bool, radius: f32) {
        if !self.pad_show || !self.pad_free() || (card && (self.pad_nav.is_some() || self.pad_bar.is_some())) { return; }
        let _accent = egui::Color32::from_rgb(self.theme.accent.0, self.theme.accent.1, self.theme.accent.2);
        let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("pad_ring"))).with_clip_rect(clip);
        p.rect_stroke(r.expand(1.0), (radius + 1.0) as u8, egui::Stroke::new(4.0_f32, egui::Color32::BLACK), egui::StrokeKind::Outside);
        p.rect_stroke(r, radius as u8, egui::Stroke::new(3.0_f32, ui_kit::focus_ring()), egui::StrokeKind::Outside);
    }

    fn home_frame(&mut self, window: &Rc<Window>, w: usize, h: usize) {
        let mut hs = self.home.take().unwrap();
        if std::mem::take(&mut self.status_clicked) { match hs.sync.as_ref().or(hs.fill_art.as_ref()) { Some(j) => { j.cancel(); if let Some(f) = &hs.fill_art { f.cancel(); } } None => { hs.notices.pop(); } } }
        self.status = hs.status_line();
        if !pd_filter::net::activity().is_empty() { self.ctx.request_repaint_after(Duration::from_millis(500)); }
        if hs.notices.last().map_or(false, |(_, t)| t.elapsed() < NOTICE_SHOWN) { self.ctx.request_repaint_after(Duration::from_millis(1000)); }   // so the message goes away by itself
        if hs.companion_ctx.is_none() { hs.companion_ctx = Some(self.ctx.clone()); }   // the start-up sync and the update check need it, and a start on Home never reached the line that set it
        hs.steam_tick(&self.ctx);
        hs.sync_tick();
        if std::mem::take(&mut hs.packs_stale) { self.packs_checked = false; self.list_icons.clear(); self.settle_until = Some(Instant::now() + Duration::from_secs(8)); }   // art changed: read the icons again
        let now = Instant::now();
        let t_frame = Instant::now();
        if hs.entering {
            // opening the page: the size is not 'just changed' (ask for covers at once) and nothing shows until they are decoded
            hs.entering = false;
            hs.size = (w, h);
            hs.changed_at = now - Duration::from_secs(1);
            hs.hold_until = Some(now + Duration::from_millis(700));
        }
        if hs.size != (w, h) { hs.size = (w, h); hs.changed_at = now; self.lib_warm = false; }
        let settled = now.duration_since(hs.changed_at).as_millis() >= 120;
        self.badges = hs.data.badges.clone();
        let covers_changed = hs.covers.poll();
        if settled && (!self.lib_warm || hs.dirty) && self.shot_size.is_none() { self.ensure_library(&mut hs); self.warm_library(w, h); }
        let on_pick = self.page == Page::Pick;
        let raw = if self.shot_size.is_some() { egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w as f32, h as f32))), ..Default::default() } }
                  else { self.egui_winit.as_mut().unwrap().take_egui_input(window) };
        let mut raw = raw;
        let pad_navs = self.pad_take(window, &mut raw, true);
        if let Some((x, y)) = self.shot_hover { raw.events.push(egui::Event::PointerMoved(egui::pos2(x, y))); hs.pointer = Some(egui::pos2(x, y)); }
        if self.shot_hover.is_some() && hs.size.0 > 0 && settled { if let Some(n) = self.shot_wheel.take() { raw.events.push(egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, -40.0 * n), modifiers: Default::default(), phase: egui::TouchPhase::Move }); } }
        if !on_pick {
            // the page scrolls when its shelves are taller than the window: the right stick moves it directly, everything else eases (PlayDate's wheel feel: a quarter of the way every 60th of a second)
            let max_scroll = home::max_scroll(&hs.data, h as i32);
            let sdt = now.duration_since(hs.scroll_at).as_secs_f32().min(0.05); hs.scroll_at = now;
            hs.home_scroll_target = hs.home_scroll_target.clamp(0.0, max_scroll);
            if self.rs_px != 0.0 && self.pad_free() && self.shot_size.is_none() { hs.home_scroll_target = (hs.home_scroll_target + self.rs_px).clamp(0.0, max_scroll); hs.home_scroll = hs.home_scroll_target; }
            let diff = hs.home_scroll_target - hs.home_scroll;
            if diff.abs() < 0.5 { hs.home_scroll = hs.home_scroll_target; } else { hs.home_scroll += diff * (1.0 - 0.75f32.powf(sdt * 60.0)); self.ctx.request_repaint(); }
            hs.home_scroll = hs.home_scroll.clamp(0.0, max_scroll);
        }
        let mut boxes = if on_pick { Vec::new() } else { home::layout_scrolled(&hs.data, w as i32, h as i32, &hs.shelf_off, hs.home_scroll) };
        if !on_pick && self.ctx.layer_id_at(hs.pointer.unwrap_or_default()).map_or(true, |l| l.order == egui::Order::Background) {
            // the wheel over a shelf whose pool is bigger than the row moves it a card at a time (vertical or sideways wheel, touchpads accumulate)
            // over a shelf with more games than fit the wheel moves its cards; anywhere else it scrolls the page
            let over_pool = hs.pointer.map_or(false, |p| boxes.iter().any(|b| b.widget.is_none() && b.total > b.fit && p.x >= b.x as f32 && p.x < (b.x + b.w) as f32 && p.y >= b.y as f32 && p.y < (b.y + b.h) as f32));
            let max_scroll = home::max_scroll(&hs.data, h as i32);
            for ev in &raw.events {
                if let egui::Event::MouseWheel { delta, unit, .. } = ev {
                    if over_pool {
                        let d = if delta.x.abs() > delta.y.abs() { delta.x } else { delta.y };
                        // a wheel notch is one card whatever its size; a touchpad's points add up to a card every 40
                        match unit { egui::MouseWheelUnit::Point => hs.wheel_acc += d, _ => if d != 0.0 { hs.wheel_acc += d.signum() * 40.0 } }
                    } else if delta.y != 0.0 && max_scroll > 0.0 {
                        let px = match unit { egui::MouseWheelUnit::Point => delta.y, egui::MouseWheelUnit::Line => delta.y * 0.1 * h as f32, egui::MouseWheelUnit::Page => delta.y * 0.9 * h as f32 };
                        hs.home_scroll_target = (hs.home_scroll_target - px).clamp(0.0, max_scroll);
                    }
                }
            }
            let step = 40.0;
            if hs.wheel_acc.abs() >= step {
                let n = (hs.wheel_acc / step).trunc();
                hs.wheel_acc -= n * step;
                if let Some(p) = hs.pointer {
                    if let Some(b) = boxes.iter().find(|b| b.widget.is_none() && p.x >= b.x as f32 && p.x < (b.x + b.w) as f32 && p.y >= b.y as f32 && p.y < (b.y + b.h) as f32) {
                        let max = b.total.saturating_sub(b.fit);
                        if max > 0 {
                            let cur = b.off as i64;
                            let next = (cur - n as i64).clamp(0, max as i64) as usize;   // wheel down (negative) = further along
                            if next != b.off { hs.shelf_off.insert(b.idx, next); boxes = home::layout_scrolled(&hs.data, w as i32, h as i32, &hs.shelf_off, hs.home_scroll); }
                        }
                    }
                }
            }
        }
        // the gamepad cursor on a card that is off the window: scroll the page to it (worked out from where the card is with no scroll, so it settles instead of chasing itself)
        if !on_pick && self.pad_show {
            if let Some(c) = self.pad_cur.and_then(|cur| boxes.iter().flat_map(|b| b.cards.iter()).find(|c| c.appid == cur)) {
                let (top, bottom) = (home::bar_height(h as i32) as f32 + 8.0, h as f32 - 8.0);
                let (abs_y, ch) = (c.y as f32 + hs.home_scroll, c.h as f32);
                let max = home::max_scroll(&hs.data, h as i32);
                if abs_y - hs.home_scroll_target < top { hs.home_scroll_target = (abs_y - top).clamp(0.0, max); }
                else if abs_y + ch - hs.home_scroll_target > bottom { hs.home_scroll_target = (abs_y + ch - bottom).clamp(0.0, max); }
            }
        }
        // tell the art job what is on screen, what is just off screen and what the page lists (the cover job fetches in that order, see pd_filter::artfocus)
        {
            let vis: std::collections::HashSet<i64> = if on_pick { hs.pick.rects.iter().map(|(a, _)| *a).collect() } else { boxes.iter().flat_map(|b| b.cards.iter().map(|c| c.appid)).collect() };
            let mut near: std::collections::HashSet<i64> = Default::default();
            if !on_pick {
                for b in boxes.iter().filter(|b| b.widget.is_none() && b.total > b.fit) {
                    if let Some(sh) = hs.data.shelves.get(b.idx) { let (lo, hi) = (b.off.saturating_sub(b.fit), (b.off + 2 * b.fit).min(sh.games.len())); for g in sh.games.get(lo..hi).unwrap_or(&[]) { if !vis.contains(&g.0) { near.insert(g.0); } } }
                }
            }
            let sig = (vis.len(), near.len(), if on_pick { 0 } else { hs.data.shelves.iter().map(|s| s.games.len()).sum() }, vis.iter().chain(near.iter()).fold(0i64, |a, x| a.wrapping_mul(31).wrapping_add(*x) ^ x));
            if hs.focus_sig != sig {
                let filtered: std::collections::HashSet<i64> = if on_pick { Default::default() } else { hs.data.shelves.iter().flat_map(|s| s.games.iter().map(|g| g.0)).collect() };
                hs.art_focus.set(vis, near, filtered); hs.focus_sig = sig;
            }
        }
        // a shelf that scrolls decodes the card just off each end ahead of time, so the next notch slides real covers in instead of plain placeholders
        if !on_pick {
            for b in boxes.iter().filter(|b| b.widget.is_none() && b.total > b.fit) {
                let Some(c0) = b.cards.first() else { continue };
                let cw = (c0.w as f32 * 0.95).round() as u32;
                for gi in [b.off.checked_sub(1), Some(b.off + b.fit)].into_iter().flatten() {
                    if let Some((appid, _)) = hs.data.shelves[b.idx].games.get(gi) { hs.covers.request(*appid, cw); }
                }
            }
        }
        let mut hold = false;
        if !on_pick && hs.hold_until.is_some() {
            let mut ready = true;
            for c in boxes.iter().flat_map(|b| b.cards.iter()) {
                let cw = (c.w as f32 * 0.95).round() as u32;
                hs.covers.request(c.appid, cw);
                if !hs.covers.exact(c.appid, cw) && !hs.covers.missing.contains(&c.appid) { ready = false; }
            }
            if ready || hs.hold_until.map_or(true, |t| now >= t) { hs.hold_until = None; } else { hold = true; }
        }
        // gamepad cursor over Home's shelves or Pick 6's cards: the nearest card in the direction pressed
        let mut pad_fired: Vec<card_actions::Fired> = Vec::new();
        self.pad_ring = None;
        if !hold && (self.pad_show || !pad_navs.is_empty()) {
            use gamepad::Nav;
            let items_of = hs.pick.pad_items.clone();
            let cards_of = |boxes: &[home::ShelfBox], pick: &[(i64, egui::Rect)]| -> Vec<(i64, [f32; 4])> {
                if on_pick {
                    // the page's controls first (ids below PAD_CTRL), then the picked cards
                    items_of.iter().enumerate().map(|(k, r)| (PAD_CTRL + k as i64, [r.min.x, r.min.y, r.width(), r.height()]))
                        .chain(pick.iter().map(|(a, r)| (*a, [r.min.x, r.min.y, r.width(), r.height()]))).collect()
                }
                else { boxes.iter().flat_map(|b| b.cards.iter()).map(|c| (c.appid, [c.x as f32, c.y as f32, c.w as f32, c.h as f32])).collect() }
            };
            for n in pad_navs.iter().copied() {
                let cards = cards_of(&boxes, &hs.pick.rects);
                let cur = self.pad_cur.filter(|a| cards.iter().any(|c| c.0 == *a));
                let Some(cur) = cur.filter(|_| self.pad_show) else {
                    self.pad_show = true;
                    if on_pick {
                        // the card under the mouse if there is one (the page does not scroll, so there is no "first in view"), else where the pad was, else the first control
                        let under = hs.pointer.and_then(|p| hs.pick.rects.iter().find(|(_, r)| r.contains(p)).map(|(a, _)| *a)
                            // a page control under the mouse (the smallest one, so a button inside a bigger panel wins) starts the pad there too
                            .or_else(|| items_of.iter().enumerate().filter(|(_, r)| r.contains(p)).min_by(|x, y| (x.1.width() * x.1.height()).partial_cmp(&(y.1.width() * y.1.height())).unwrap_or(std::cmp::Ordering::Equal)).map(|(k, _)| PAD_CTRL + k as i64)));
                        self.pad_cur = under.or(cur).or(cards.first().map(|c| c.0));
                    }
                    else {
                        // PlayDate: the card under the mouse, else the first card in view (the shelves may be scrolled), never a stale position
                        let under = hs.pointer.and_then(|p| cards.iter().find(|c| p.x >= c.1[0] && p.x < c.1[0] + c.1[2] && p.y >= c.1[1] && p.y < c.1[1] + c.1[3]).map(|c| c.0));
                        let in_view = cards.iter().find(|c| c.1[1] + c.1[3] * 0.5 > 40.0 && c.1[1] + c.1[3] * 0.5 < h as f32).map(|c| c.0);
                        self.pad_cur = under.or(in_view).or(cards.first().map(|c| c.0));
                    }
                    continue;
                };
                let r = cards.iter().find(|c| c.0 == cur).unwrap().1;
                let pos = (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
                match n {
                    Nav::Confirm if cur < PAD_CTRL + 10_000 && cur >= PAD_CTRL => {
                        // a control on the Pick 6 page: click it where it is
                        let at = egui::pos2(pos.0, pos.1);
                        raw.events.push(egui::Event::PointerMoved(at));
                        for pr in [true, false] { raw.events.push(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: pr, modifiers: Default::default() }); }
                    }
                    Nav::Menu | Nav::Edit if cur < PAD_CTRL + 10_000 && cur >= PAD_CTRL => {}
                    Nav::Confirm => pad_fired.push(card_actions::Fired { action: "launch".into(), appid: cur, pos }),
                    Nav::Menu => pad_fired.push(card_actions::Fired { action: "context_menu".into(), appid: cur, pos }),
                    Nav::Edit => pad_fired.push(card_actions::Fired { action: "edit".into(), appid: cur, pos }),
                    Nav::Start | Nav::Store | Nav::Folder | Nav::Achievements | Nav::Hub if cur >= PAD_CTRL && cur < PAD_CTRL + 10_000 => {}
                    Nav::Start => pad_fired.push(card_actions::Fired { action: "launch".into(), appid: cur, pos }),
                    Nav::Store | Nav::Folder | Nav::Achievements | Nav::Hub => pad_fired.push(card_actions::Fired { action: n.shortcut().unwrap().into(), appid: cur, pos }),
                    Nav::Up | Nav::Down | Nav::Left | Nav::Right => {
                        // at the end of a shelf that holds more games than fit, the row moves along instead of leaving the shelf
                        let mut shifted = false;
                        if !on_pick && matches!(n, Nav::Left | Nav::Right) {
                            if let Some(b) = boxes.iter().find(|b| b.cards.iter().any(|c| c.appid == cur)) {
                                let max = b.total.saturating_sub(b.fit);
                                let edge = if n == Nav::Right { b.cards.last().map(|c| c.appid) == Some(cur) && b.off < max } else { b.cards.first().map(|c| c.appid) == Some(cur) && b.off > 0 };
                                if edge {
                                    let (idx, next) = (b.idx, if n == Nav::Right { b.off + 1 } else { b.off - 1 });
                                    hs.shelf_off.insert(idx, next);
                                    boxes = home::layout_scrolled(&hs.data, w as i32, h as i32, &hs.shelf_off, hs.home_scroll);
                                    if let Some(b) = boxes.iter().find(|b| b.idx == idx) { self.pad_cur = if n == Nav::Right { b.cards.last() } else { b.cards.first() }.map(|c| c.appid); }
                                    shifted = true;
                                }
                            }
                        }
                        if !shifted {
                            match padnav::nearest(&cards, cur, n) {
                                Some(a) => self.pad_cur = Some(a),
                                None if n == Nav::Up => self.pad_nav = Some(self.page_nav_index()),
                                None => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            let cards = cards_of(&boxes, &hs.pick.rects);
            self.pad_ring = self.pad_cur.and_then(|a| cards.iter().find(|c| c.0 == a)).map(|c| egui::Rect::from_min_size(egui::pos2(c.1[0], c.1[1]), egui::vec2(c.1[2], c.1[3])));
        }
        let rects = hs.pick.rects.clone();
        let edit_rect0 = self.edit_rect.take();
        let hit0 = |x: f32, y: f32| -> Option<i64> { if edit_rect0.map_or(false, |r| r.contains(egui::pos2(x, y))) { return None; } if on_pick { rects.iter().find(|(_, r)| r.contains(egui::pos2(x, y))).map(|(a, _)| *a) } else { home::card_at(&boxes, x, y) } };
        let ctx_h = self.ctx.clone();
        let hit = |x: f32, y: f32| -> Option<i64> {
            // a window or menu over the point owns the click: nothing behind it may react
            if ctx_h.layer_id_at(egui::pos2(x, y)).map_or(false, |l| l.order != egui::Order::Background) { None } else { hit0(x, y) }
        };
        let mut fired = hs.feed(&raw.events, &hit, now);
        fired.extend(pad_fired);
        // hover grow: 95% -> 100% over 150 ms
        let hovered = if hs.menu.is_none() { (if self.pad_show && self.pad_nav.is_none() && !self.menu_open && self.pad_free() { self.pad_cur.filter(|a| *a < PAD_CTRL || *a >= PAD_CTRL + 10_000) } else { None }).or_else(|| hs.pointer.and_then(|p| hit(p.x, p.y))) } else { None };   // the pad's card wins over a resting mouse   // the gamepad cursor's card grows like a hovered one
        let dt = now.duration_since(hs.anim_at).as_secs_f32() / 0.15;
        hs.anim_at = now;
        let mut animating = false;
        let ids: Vec<i64> = hs.hover.keys().copied().chain(hovered).collect();
        for id in ids {
            let t = hs.hover.entry(id).or_insert(0.0);
            let target = if Some(id) == hovered { 1.0 } else { 0.0 };
            *t = if *t < target { (*t + dt).min(target) } else { (*t - dt).max(target) };
            if *t != target { animating = true; }
        }
        hs.hover.retain(|_, t| *t > 0.0);
        if hovered.is_some() { self.ctx.set_cursor_icon(egui::CursorIcon::PointingHand); }
        // text in labels is not selectable: a selectable label is a target for the pad's arrow-key focus search (only Tab skips it), so Down from a settings tab landed on the heading
        // under it and the focus was lost there
        if self.ctx.global_style().interaction.selectable_labels { self.ctx.global_style_mut(|s| s.interaction.selectable_labels = false); }
        let ctx = self.ctx.clone();
        let theme = self.theme.clone();
        let mut clicked = None;
        let mut pick = None;
        let mut answer = None;
        let wake = hs.engine.deadline().map(|d| d.saturating_duration_since(now));
        self.tip_fields = hs.data.tip_fields(if on_pick { "pick6" } else { "home" });
        self.tip_quals = None;
        if hs.tip_pin.is_some() && self.tip_fields.is_none() { self.tip_fields = Some(hs.data.tip_fields_any()); }
        if let Some(id) = hs.tip_pin { if self.tip_target.map_or(true, |t| t.0 != id) { self.tip_target = Some((id, egui::Rect::NOTHING)); } }
        if let Some((id, _)) = self.tip_target { if self.tip_fields.is_some() && self.tip_cache.as_ref().map_or(true, |c| c.0 != id) { self.tip_cache = hs.data.load_edit(id).map(|i| (id, i, hs.data.tip_hltb(id))); } }
        // a shelf that was scrolled eases to its new offset the way PlayDate's wheel scroll does: every 60th of a second a quarter of the distance left
        // (index.html: `pos += diff * 0.25`), until it is within half a pixel. Done before the UI pass so the outlines and badges drawn there ride with the covers painted after it.
        let mut sliding = false;
        {
            let sdt = now.duration_since(hs.slide_at).as_secs_f32().min(0.05); hs.slide_at = now;
            if !on_pick && !hold {
                for b in boxes.iter().filter(|b| b.widget.is_none()) {
                    let target = b.off as f32;
                    let cur = hs.shelf_pos.get(&b.idx).copied().unwrap_or(target);
                    let step_px = b.cards.first().map_or(100.0, |c| (c.w + 6) as f32);
                    let k = 1.0 - 0.75f32.powf(sdt * 60.0);
                    let eased = cur + (target - cur) * k;
                    let next = if (cur - target).abs() > 6.0 || (eased - target).abs() * step_px < 0.5 { target } else { eased };
                    hs.shelf_pos.insert(b.idx, next);   // always kept: the next scroll starts from where the cards are now
                    if (next - target).abs() >= 0.001 { sliding = true; }
                }
            }
        }
        let out = ctx.run_ui(raw, |ui| {
            if let Some(d) = self.pad_move.take() { ui.ctx().memory_mut(|m| m.move_focus(d)); }
            self.nav(ui);
            if let Some(r) = self.pad_ring {
                // while the shelf slides the ring rides with its card (and is cut off at the shelf's edges), so it leaves and enters the view with it
                let (mut r, mut clip) = (r, ui.ctx().content_rect());
                if !on_pick && !hold && sliding { if let Some(a) = self.pad_cur {
                    let sb = home::slide_boxes(&boxes, &hs.data, &hs.shelf_pos);
                    if let Some((b, c)) = sb.iter().find_map(|b| b.cards.iter().find(|c| c.appid == a).map(|c| (b, c))) {
                        r = r.translate(egui::vec2(c.x as f32 - r.min.x, 0.0));
                        if let Some((c0, c1)) = b.clip { clip = clip.intersect(egui::Rect::from_min_max(egui::pos2(c0 as f32, clip.min.y), egui::pos2(c1 as f32, clip.max.y))); }
                    }
                } }
                let outlined = self.pad_cur.map_or(false, |a| if on_pick { hs.data.pick_outlines.contains_key(&a) } else { hs.data.home_outlines.contains_key(&a) }); let rad = if on_pick { 0.625 * ui_kit::rem(ui.ctx()) } else { 8.0 }; self.pad_ring_card(ui.ctx(), r, clip, if outlined { Some(rad) } else { None }); }
            if on_pick { pick_page::show(ui, &mut hs.pick, &mut hs.data, &theme, &hs.hover, &hs.covers.missing); } else if !hold { clicked = Self::home_overlay(ui, &hs, &boxes, h as i32, &theme); }
            {
                let painter = ui.painter().clone();
                // while a shelf slides, its outlines and badges ride with the covers (the final layout's boxes would show them at their resting places at once), cut off at the shelf's edges like the covers
                let slid = if on_pick || hold { None } else { Some(home::slide_boxes(&boxes, &hs.data, &hs.shelf_pos)) };
                let clips: std::collections::HashMap<i64, egui::Rect> = slid.iter().flatten().filter(|b| b.clip.is_some()).flat_map(|b| { let (c0, c1) = b.clip.unwrap(); b.cards.iter().map(move |c| (c.appid, egui::Rect::from_min_max(egui::pos2(c0 as f32, 0.0), egui::pos2(c1 as f32, 100000.0)))) }).collect();
                let spots: Vec<(i64, egui::Rect)> = if on_pick { hs.pick.rects.clone() } else if hold { Vec::new() } else { slid.iter().flatten().flat_map(|b| b.cards.iter()).map(|c| (c.appid, egui::Rect::from_min_size(egui::pos2(c.x as f32, c.y as f32), egui::vec2(c.w as f32, c.h as f32)))).collect() };
                let page = if on_pick { "pick6" } else { "home" };
                if !on_pick { for (a, rc) in &spots { if let Some(&(r8, g8, b8)) = hs.data.home_outlines.get(a) { clips.get(a).map_or(painter.clone(), |c| painter.with_clip_rect(*c)).rect_stroke(egui::Rect::from_center_size(rc.center(), rc.size() * { let g = hs.hover.get(a).copied().unwrap_or(0.0); 0.95 + 0.05 * (g * (2.0 - g)) }), 8, egui::Stroke::new(3.0_f32, egui::Color32::from_rgb(r8, g8, b8)), egui::StrokeKind::Outside); } } }
                if hs.tip_pin.is_none() { self.tip_target = None; }
                if let (Some(id), true) = (hs.tip_pin, self.tip_fields.is_some()) { if let Some((a, r)) = spots.iter().find(|(a, _)| *a == id) { self.tip_target = Some((*a, *r)); } }
                else if let (Some(_), true, Some((a, r))) = (self.tip_fields.as_ref(), self.pad_show && self.pad_nav.is_none() && !self.menu_open && self.pad_free(), self.pad_cur.and_then(|a| spots.iter().find(|(s, _)| *s == a).copied())) { self.tip_target = Some((a, r)); }
                else if let (Some(_), Some(p)) = (self.tip_fields.as_ref(), ui.input(|i| i.pointer.hover_pos())) {
                    let free = ui.ctx().layer_id_at(p).map_or(true, |l| l.order == egui::Order::Background);
                    if free && hs.menu.is_none() { if let Some((a, r)) = spots.iter().find(|(_, r)| r.contains(p)) { self.tip_target = Some((*a, *r)); } }
                }
                self.tip_ui(ui.ctx());
                let eb = hs.data.state_json().get("edit_button");
                if eb.and_then(|e| e.get("pages")).and_then(|p| p.get(page)).and_then(|v| v.as_bool()).unwrap_or(false) {
                    let corner = eb.and_then(|e| e.get("corner")).and_then(|c| c.as_str()).unwrap_or("top_right").to_string();
                    if let Some(p) = ui.input(|i| i.pointer.hover_pos()) {
                        let free = ui.ctx().layer_id_at(p).map_or(true, |l| l.order == egui::Order::Background);
                        let prev = edit_rect0;
                        let on_card = prev.map_or(false, |r| r.contains(p));
                        if free && hs.menu.is_none() {
                            if let Some((appid, rect)) = spots.iter().find(|(_, r)| r.contains(p) || (on_card && prev.map_or(false, |pr| r.intersects(pr)))).copied() { self.edit_button(ui, &painter, rect, &corner, appid, 1.0); }
                        }
                    }
                }
                if let Some(cfg) = if on_pick { hs.data.badge_cfg_pick.clone() } else { hs.data.badge_cfg_home.clone() } {
                    for (appid, rect) in spots {
                        // the badges ride on the cover, which sits at 95% and grows to 100% under the pointer (the outline above does the same)
                        let g = hs.hover.get(&appid).copied().unwrap_or(0.0);
                        let rect = egui::Rect::from_center_size(rect.center(), rect.size() * (0.95 + 0.05 * (g * (2.0 - g))));
                        let inst = hs.data.info(appid).map_or(false, |g| g.installed); let pc = clips.get(&appid).map_or(painter.clone(), |c| painter.with_clip_rect(*c)); self.paint_badges(ui, &pc, rect, &cfg, appid, inst);
                    }
                }
            }
            let (p, a) = Self::home_popups(ui, &mut hs); pick = p; answer = a;
            self.modal_ui(ui, &mut hs);
            self.edit_ui(ui, &mut hs, false);
            self.filter_ui(ui, &mut hs);
            self.bulk_ui(ui, &mut hs);
            self.pag_ui(ui, &mut hs);
            self.community_ui(ui, &mut hs);
            self.pad_focus_ring(&ui.ctx().clone());   // last, after the dialogs: it reads this pass's rectangles, so the ring stays on a control that moved this frame
            if let Some(d) = wake { ui.ctx().request_repaint_after(d); }
            // the browser's answers (Steam dates) arrive on another thread and nothing else wakes the window: look again every 250 ms while a run is going. It has to be asked inside the pass; a request made before the pass starts is dropped.
            if hs.steam_dates.as_ref().map_or(false, |sd| sd.progress().is_some()) { ui.ctx().request_repaint_after(Duration::from_millis(250)); }
        });
        if let Some(i) = clicked { hs.data.shuffle(i); }
        hs.finish(fired, pick, answer);
        if let Some(id) = self.edit_click.take() { hs.open_edit = Some(id); }
        self.egui_winit.as_mut().unwrap().handle_platform_output(window, out.platform_output);
        let ppp = out.pixels_per_point;
        let prims = ctx.tessellate(out.shapes, ppp);
        {
            let mut local: Vec<u32> = Vec::new();
            let mut surf = None;
            if self.shot_size.is_some() { local = vec![0u32; w * h]; } else { surf = Some(self.surface.as_mut().unwrap().buffer_mut().unwrap()); }
            let buffer: &mut [u32] = match surf.as_mut() { Some(b) => &mut b[..], None => &mut local[..] };
            let bg = self.bg.window_sized(w, h);
            // the cached panels also depend on where the shelves are (saving a layout or a re-sort changes them without changing the window), so the boxes are part of the key
            let geom = boxes.iter().fold(0usize, |a, b| a.wrapping_mul(1_000_003).wrapping_add((b.x as usize) ^ ((b.y as usize) << 12) ^ ((b.w as usize) << 24) ^ ((b.h as usize) << 36) ^ b.widget.as_deref().map_or(0usize, |w| w.bytes().fold(7usize, |h, c| h.wrapping_mul(31).wrapping_add(c as usize)))));   // the kind too: a clock changed to a chart in the same box must repaint its panel
            let key = (w, h, (bg.len() ^ ((hs.home_scroll.round() as i64 as usize) << 20) ^ geom, usize::from(settled)));
            if on_pick { buffer.copy_from_slice(bg); pick_page::paint(buffer, bg, w, &mut hs.pick, &mut hs.covers, settled); }
            else if hold { buffer.copy_from_slice(bg); }
            else {
                match &hs.base {
                    Some((k, px)) if *k == key => buffer.copy_from_slice(px),
                    _ => { buffer.copy_from_slice(bg); home::paint_base(buffer, bg, w, &boxes, &hs.data); hs.base = Some((key, buffer.to_vec())); }
                }
                if sliding { self.ctx.request_repaint(); }
                let drawn = if sliding { Some(home::slide_boxes(&boxes, &hs.data, &hs.shelf_pos)) } else { None };
                home::prefetch_neighbours(&boxes, &hs.data, &hs.shelf_pos, &mut hs.covers);   // before the visible ones: the decode queue is last in, first out
                home::paint_cards(buffer, bg, w, drawn.as_deref().unwrap_or(&boxes), &mut hs.covers, settled, &hs.hover);
            }
            let mut bref = BufferMutRef::new(bytemuck::cast_slice_mut(buffer), w, h);
            self.renderer.render(&mut bref, &prims, &out.textures_delta, ppp);
            if let Some((path, n)) = &mut self.shot {
                if settled && hs.covers.pending() == 0 && !covers_changed && hs.live_rx.is_none() && self.pag.as_ref().map_or(true, |p| p.settled()) && self.miam.as_ref().map_or(true, |m| m.settled()) && self.blaeo.as_ref().map_or(true, |b| b.settled()) { *n += 1; }
                if *n >= 8 + 2 * SHOT_PAD_N.load(std::sync::atomic::Ordering::Relaxed) as u32 && SHOT_PAD.lock().unwrap().is_empty() && SHOT_SCRIPT.lock().unwrap().is_empty() {
                    let rgb: Vec<u8> = buffer.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8]).collect();
                    image::RgbImage::from_raw(w as u32, h as u32, rgb).unwrap().save(&path).unwrap();
                    for b in &boxes { println!("  shelf {:<18} {:>2} cards shown (of {}), card box {}x{} px at ({},{}), shelf box y {}..{}", hs.data.shelves[b.idx].id, b.cards.len(), hs.data.shelves[b.idx].games.len(), b.cards.first().map_or(0, |c| c.w), b.cards.first().map_or(0, |c| c.h), b.cards.first().map_or(0, |c| c.x), b.cards.first().map_or(0, |c| c.y), b.y, b.y + b.h); }
                    println!("screenshot {}x{} -> {}", w, h, path.display());
                    #[cfg(feature = "dhat-heap")]
                    { let p = PROFILER.lock().unwrap().take(); drop(p); }
                    std::process::exit(0);
                }
            }
            let stale = self.shot.is_none() && self.auto.is_none() && !WINDOW_SHOWN.swap(true, std::sync::atomic::Ordering::Relaxed) && reveal_window(&window);   // shown first: a hidden window throws away what is drawn to it
            if let Some(b) = surf { if !stale { b.present().ok(); finish_reveal(&window); } }
        }
        if std::env::var_os("HOME_TIMING").is_some() && (animating || !hs.hover.is_empty()) { eprintln!("frame {:.1} ms", t_frame.elapsed().as_secs_f32() * 1000.0); }
        let delay = out.viewport_output.get(&egui::ViewportId::ROOT).map(|v| v.repaint_delay).unwrap_or(Duration::MAX);
        if !self.healthy_marked && self.shot.is_none() { self.healthy_marked = true; let r = self.root.clone(); std::thread::spawn(move || { pd_filter::rollback_marker::housekeeping(&r); if let Ok(exe) = std::env::current_exe() { pd_filter::selfupdate::cleanup(&r, &exe); } if pd_filter::rollback_marker::mark_healthy(&r) { eprintln!("update marker cleared: this version started"); } }); }
        let keep = hold || hs.covers.pending() > 0 || hs.pick.take_redraw() || !hs.pick.is_revealed() || self.shot.is_some() || covers_changed || clicked.is_some() || animating || delay == Duration::ZERO;
        self.keep_drawing = keep;
        self.next_wake = if keep { None } else if !settled || self.bg.size != (w, h) { Some(Instant::now() + Duration::from_millis(130)) } else if delay < Duration::from_secs(3600) { Some(Instant::now() + delay) } else { None };
        if self.settle_until.map_or(false, |t| Instant::now() < t) && !keep { let t = Instant::now() + Duration::from_millis(100); self.next_wake = Some(self.next_wake.map_or(t, |w| w.min(t))); }
        if keep { window.request_redraw(); }
        self.home = Some(hs);
        if let Some(p) = self.want_page.take() { self.page = p; if let Some(hs) = &mut self.home { hs.hover.clear(); hs.base = None; hs.menu = None; if p == Page::Home { hs.entering = true; } } window.request_redraw(); }
    }

    fn auto_step(&mut self, view_h: f32) -> Option<f32> {
        let mut a = self.auto.take()?;
        if a.pass >= PASSES.len().min(self.pass_limit) { std::process::exit(0); }
        let (cw, secs) = PASSES[a.pass];
        let now = Instant::now();
        let dt = now.duration_since(a.last).as_secs_f32();
        a.last = now;
        if a.fresh {
            self.cw = cw as f32;
            a.y = 0.0; a.started = now; a.sharp = 0.0; a.any = 0.0; a.frames = 0; a.fresh = false;
            self.parts.clear(); self.intervals.clear(); self.core.reset_stats();
            self.auto = Some(a);
            return Some(0.0);
        }
        let max = (self.core.row_count() as f32 * row_height(self.core.card_w) - view_h).max(1.0);
        if now.duration_since(a.started).as_secs_f32() > 1.5 {
            a.y = (a.y + max * dt / secs).min(max);
            let (ok, any, tot) = self.core.ready(a.y, view_h);
            a.sharp += ok as f64 / tot.max(1) as f64; a.any += any as f64 / tot.max(1) as f64; a.frames += 1;
            if a.y >= max {
                let n = a.frames.max(1) as f64;
                let (decodes, avg) = self.core.decode_stats();
                println!("card {:3}px {:>2.0}s scroll ({} cols): {:5.1}% sharp, {:5.1}% sharp-or-blurry | RSS {:3} MB (cover caches {} MB; malloc in use/free/mmapped {:?}) | {} decodes, avg {:.1} ms",
                    cw, secs, self.core.cols, a.sharp / n * 100.0, a.any / n * 100.0, rss_mb(), self.core.cache_mb(), heap_mb(), decodes, avg);
                #[cfg(feature = "dhat-heap")]
                { let p = PROFILER.lock().unwrap().take(); drop(p); std::process::exit(0); }
                self.report();
                a.pass += 1; a.fresh = true;
            }
        }
        let y = a.y;
        self.auto = Some(a);
        Some(y)
    }

    fn report(&mut self) {
        let q = |v: &mut Vec<f32>, p: usize| { v.sort_by(|a, b| a.partial_cmp(b).unwrap()); if v.is_empty() { 0.0 } else { v[(v.len() * p / 100).min(v.len() - 1)] } };
        let mut iv = std::mem::take(&mut self.intervals);
        if iv.is_empty() { return; }
        let avg = iv.iter().sum::<f32>() / iv.len() as f32;
        println!("      frame interval: avg {:.1} ms ({:.0} fps), p50 {:.1}, p95 {:.1}, max {:.1} ms over {} frames",
            avg, 1000.0 / avg, q(&mut iv, 50), q(&mut iv, 95), q(&mut iv, 100), iv.len());
        let names = ["egui run", "grid update", "compose", "overlay raster", "present", "whole frame"];
        let mut line = String::from("      p50 ms per stage:");
        for (k, n) in names.iter().enumerate() {
            let mut v: Vec<f32> = self.parts.iter().map(|p| p[k]).collect();
            line += &format!(" {} {:.1} |", n, q(&mut v, 50));
        }
        println!("{}", line);
        self.parts.clear();
    }

    /// Leaves for good. Closing the window the normal way ends in a SIGSEGV (it has on every close since Zest first ran under Wayland): the clipboard helper thread
    /// (`smithay-clipboard`, via egui-winit) destroys its Wayland objects while the main thread tears the connection down. Everything worth keeping is already written
    /// (window size, settings, the library), so the process ends here instead of running the destructors; the lock and the sockets close with it.
    fn leave(&mut self, el: &ActiveEventLoop) {
        self.save_window();
        #[cfg(not(feature = "dhat-heap"))]
        { let _ = el; std::process::exit(0); }
        #[cfg(feature = "dhat-heap")]
        el.exit();
    }

    fn frame(&mut self, el: &ActiveEventLoop) {
        if self.want_exit { self.leave(el); return; }
        let t_frame = Instant::now();
        self.intervals.push(t_frame.duration_since(self.last_frame).as_secs_f32() * 1000.0);
        self.last_frame = t_frame;
        let window = self.window.clone().unwrap();
        let sz = window.inner_size();
        let (w, h) = self.shot_size.unwrap_or((sz.width.max(1) as usize, sz.height.max(1) as usize));
        if self.shot_size.is_none() { self.surface.as_mut().unwrap().resize(NonZeroU32::new(w as u32).unwrap(), NonZeroU32::new(h as u32).unwrap()).ok(); }

        if self.shot_size.is_none() { self.track_window(&window, w, h); }
        if std::mem::take(&mut self.want_fullscreen) { window.set_fullscreen(if window.fullscreen().is_some() { None } else { Some(winit::window::Fullscreen::Borderless(None)) }); }
        self.bg.update(w, h);
        if !self.cjk_done && self.home.as_ref().map_or(false, |h| h.data.has_cjk()) { theme::install_fonts(&self.ctx, true); self.cjk_done = true; }
        if matches!(self.page, Page::Home | Page::Pick) && self.home.is_some() { self.home_frame(&window, w, h); return; }
        let changed = self.core.poll();
        if !self.core.games.is_empty() && self.shot.as_ref().map_or(true, |s| s.1 >= 2) { if let Some(n) = self.shot_select.take() { self.select_mode = true; self.selected = self.core.games.iter().take(n).map(|g| g.appid).collect(); } }
        if let Some(rt) = &mut self.rt {
            let t = rt.start.elapsed().as_secs_f32();
            if rt.base == 0.0 { rt.base = w as f32; }
            let target = if t < 2.0 { rt.base } else if t < 6.0 {
                rt.base - 420.0 * (1.0 - (std::f32::consts::TAU * (t - 2.0) / 2.0).cos()) / 2.0   // two slow in-and-out drags
            } else { rt.base - 123.0 };   // hold at a fixed width
            if rt.real { let _ = window.request_inner_size(winit::dpi::PhysicalSize::new(target.round() as u32, h as u32)); }
            else { self.sim_w = if t < 2.0 { None } else { Some(target) }; }
            if t > 2.0 { rt.w_min = rt.w_min.min(w as u32); rt.w_max = rt.w_max.max(w as u32); }
        }
        let mut hs = self.home.take();
        if let Some(h) = hs.as_mut() { if std::mem::take(&mut self.status_clicked) { match h.sync.as_ref().or(h.fill_art.as_ref()) { Some(j) => { j.cancel(); if let Some(f) = &h.fill_art { f.cancel(); } } None => { h.notices.pop(); } } } self.status = h.status_line(); }
        if let Some(h) = hs.as_mut() { if h.companion_ctx.is_none() { h.companion_ctx = Some(self.ctx.clone()); } h.steam_tick(&self.ctx); h.sync_tick(); if std::mem::take(&mut h.packs_stale) { self.settle_until = Some(Instant::now() + Duration::from_secs(8)); self.packs_checked = false; self.list_icons.clear(); } }
        if let Some(h) = hs.as_mut() { self.ensure_library(h); self.outlines = h.data.outlines.clone(); self.badges = h.data.badges.clone(); self.badge_cfg = h.data.badge_cfg.clone(); }
        if let Some(h) = hs.as_ref() { gamepad::apply_state(h.data.state_json()); }
        self.rs_db = hs.as_ref().map(|h| h.data.db_path()); self.rs_sort = hs.as_ref().map(|h| h.data.view_values().0).unwrap_or_default();
        self.edit_corner = hs.as_ref().and_then(|h| {
            let eb = h.data.state_json().get("edit_button");
            let on = eb.and_then(|e| e.get("pages")).and_then(|p| p.get("library")).and_then(|v| v.as_bool()).unwrap_or(true);
            on.then(|| eb.and_then(|e| e.get("corner")).and_then(|c| c.as_str()).unwrap_or("top_right").to_string())
        });
        let covers_changed_lib = hs.as_mut().map_or(false, |h| h.covers.poll());
        let edit_info_tab = self.edit_arg.map_or(false, |a| a.1);
        if let (Some(h), Some((id, _))) = (hs.as_mut(), self.edit_arg.take()) { h.open_edit = Some(id); if let Some((_, k)) = self.crop_arg.take() { self.crop = crop_dialog::CropDialog::open(&self.ctx, &h.data.root_dir(), id, k); } }
        let mut raw = self.egui_winit.as_mut().unwrap().take_egui_input(&window);
        let pad_navs = self.pad_take(&window, &mut raw, true);
        self.lib_pad_refocus();
        if self.shot_size.is_some() { raw.screen_rect = Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w as f32, h as f32))); }
        if self.shot.as_ref().map_or(false, |s| s.1 >= 2) { if let Some(o) = self.shot_switch.take() { self.view_changes.push(("artwork_orientation", serde_json::json!(o))); if let Some((p, _)) = self.shot.as_mut().map(|s| (s.0.clone(), 0)) { let _ = p; } if let Some(s) = self.shot.as_mut() { s.1 = 0; } } }
        if let Some((x, y)) = self.shot_hover { raw.events.push(egui::Event::PointerMoved(egui::pos2(x, y))); }
        if self.shot.as_ref().map_or(false, |s| s.1 >= 3) { if let Some((x, y)) = self.shot_rclick.take() {
            let pos = egui::pos2(x, y);
            raw.events.push(egui::Event::PointerMoved(pos));
            for pressed in [true, false] { raw.events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Secondary, pressed, modifiers: Default::default() }); }
        } }
        let mut fired = Vec::new();
        if let Some(h) = hs.as_mut() {
            let (lay, grid) = (self.lay, self.lib_grid);
            let cw = self.core.card_w;
            let rows: Vec<(f32, usize, usize)> = self.core.rows.iter().filter(|r| r.count > 0).map(|r| (r.y, r.first, r.count)).collect();
            let ids: Vec<i64> = self.core.games.iter().map(|g| g.appid).collect();
            let edit_rect = self.edit_rect;
            let hit0 = move |x: f32, y: f32| -> Option<i64> {
                if edit_rect.map_or(false, |r| r.contains(egui::pos2(x, y))) { return None; }
                let (rect, sy) = grid?;
                if !rect.contains(egui::pos2(x, y)) { return None; }
                let (lx, ly) = (x - rect.min.x - lay.x_off as f32, y - rect.min.y + sy);
                if lx < 0.0 { return None; }
                let ri = rows.partition_point(|r| r.0 + row_height(cw) <= ly);
                let &(ry, first, count) = rows.get(ri)?;
                let ly = ly - ry - 4.0;
                if ly < 0.0 || ly >= grid_core::card_h(cw) as f32 { return None; }
                let c = (lx / (cw as f32 + GAP as f32)) as usize;
                if c >= count || lx - c as f32 * (cw as f32 + GAP as f32) >= cw as f32 { return None; }
                ids.get(first + c).copied()
            };
        let ctx_h = self.ctx.clone();
        let hit = |x: f32, y: f32| -> Option<i64> {
            // a window or menu over the point owns the click: nothing behind it may react
            if ctx_h.layer_id_at(egui::pos2(x, y)).map_or(false, |l| l.order != egui::Order::Background) { None } else { hit0(x, y) }
        };
            fired = h.feed(&raw.events, &hit, Instant::now());
            if self.select_mode {
                // a click on a card selects or deselects it; only the context menu keeps its rule
                fired.retain(|f| f.action == "context_menu");
                if h.menu.is_none() {
                    for ev in &raw.events {
                        if let egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, .. } = ev {
                            let over = hit(pos.x, pos.y);
                            if *pressed { self.sel_press = over; }
                            else if let (Some(p), Some(o)) = (self.sel_press.take(), over) { if p == o && !self.selected.remove(&o) { self.selected.insert(o); } }
                        }
                    }
                }
            }
            if h.pointer.and_then(|p| hit(p.x, p.y)).is_some() && h.menu.is_none() { self.ctx.set_cursor_icon(egui::CursorIcon::PointingHand); }
            // gamepad cursor over the cards: rows are (first, count) over the shown games
            if !pad_navs.is_empty() && self.list_mode {
                use gamepad::Nav;
                let n_games = self.core.games.len();
                // the first press after the mouse moved starts where the mouse was: the row it was last over is selected, and that press does nothing more (as in the grid)
                let mut skip_first = false;
                if !self.pad_show {
                    if let Some(a) = self.lib_mouse_card.filter(|a| self.core.games.iter().any(|g| g.appid == *a)) {
                        if self.list_sel != Some(a) { self.list_sel = Some(a); self.list_center = true; }
                        skip_first = true;
                    }
                }
                self.pad_show = true;
                for n in pad_navs.iter().copied() {
                    if skip_first && matches!(n, gamepad::Nav::Up | gamepad::Nav::Down) { skip_first = false; continue; }
                    let cur = self.list_sel.and_then(|a| self.core.games.iter().position(|g| g.appid == a));
                    let centre = (self.size[0] as f32 / 2.0, self.size[1] as f32 / 2.0);
                    match (n, cur) {
                        (Nav::Up, Some(0)) | (Nav::Up, None) => { if self.pad_bar_items.is_empty() { self.pad_nav = Some(self.page_nav_index()); } else { self.pad_bar = Some(self.pad_bar_items.len() / 2); } }
                        (Nav::Up, Some(i)) => { self.list_sel = Some(self.core.games[i - 1].appid); self.list_center = true; }
                        (Nav::Down, Some(i)) if i + 1 < n_games => { self.list_sel = Some(self.core.games[i + 1].appid); self.list_center = true; }
                        (Nav::Down, None) if n_games > 0 => { self.list_sel = Some(self.core.games[0].appid); self.list_center = true; }
                        (Nav::Confirm, Some(i)) => fired.push(card_actions::Fired { action: "launch".into(), appid: self.core.games[i].appid, pos: centre }),
                        (Nav::Menu, Some(i)) => fired.push(card_actions::Fired { action: "context_menu".into(), appid: self.core.games[i].appid, pos: centre }),
                        (Nav::Start, Some(i)) => fired.push(card_actions::Fired { action: "launch".into(), appid: self.core.games[i].appid, pos: centre }),
                        (Nav::Store | Nav::Folder | Nav::Achievements | Nav::Hub, Some(i)) => fired.push(card_actions::Fired { action: n.shortcut().unwrap().into(), appid: self.core.games[i].appid, pos: centre }),
                        _ => {}
                    }
                }
            } else if !pad_navs.is_empty() {
                use gamepad::Nav;
                let live: Vec<&grid_core::Row> = self.core.rows.iter().filter(|r| r.count > 0).collect();
                let rows: Vec<(usize, usize)> = live.iter().map(|r| (r.first, r.count)).collect();
                if let (false, Some((rect, mut sy))) = (rows.is_empty(), self.lib_grid) {
                    let cw = self.core.card_w;
                    let page_rows = ((rect.height() / row_height(cw)) as usize).saturating_sub(1).max(1);
                    for n in pad_navs.iter().copied() {
                        let mut cur = self.pad_cur.and_then(|a| self.core.games.iter().position(|g| g.appid == a));
                        if !self.pad_show || cur.is_none() {
                            // the first press only shows the ring: on the card the mouse was last over if it is on screen (what PlayDate does), else the first one in view, never a
                            // position left over from before the page was scrolled
                            if !self.pad_show {
                                let rh = row_height(cw);
                                cur = self.lib_mouse_card.and_then(|a| self.core.games.iter().position(|g| g.appid == a))
                                    .filter(|i| live.iter().any(|r| *i >= r.first && *i < r.first + r.count && r.y + rh * 0.5 > sy && r.y + rh * 0.5 < sy + rect.height()));
                            }
                            if cur.is_none() { let ri = live.partition_point(|r| r.y + row_height(cw) <= sy).min(rows.len() - 1); cur = Some(rows[ri].0); }
                            self.pad_show = true;
                            self.pad_cur = cur.and_then(|i| self.core.games.get(i)).map(|g| g.appid);
                            continue;
                        }
                        let i = cur.unwrap();
                        let ri = rows.iter().position(|&(f, c)| i >= f && i < f + c).unwrap_or(0);
                        let (x, y) = (rect.min.x + lay.x_off as f32 + (i - rows[ri].0) as f32 * (cw as f32 + GAP as f32) + cw as f32 / 2.0, rect.min.y + live[ri].y - sy + 4.0 + grid_core::card_h(cw) as f32 / 2.0);
                        match n {
                            Nav::Confirm if self.select_mode => { let a = self.core.games[i].appid; if !self.selected.remove(&a) { self.selected.insert(a); } }
                            Nav::Confirm => fired.push(card_actions::Fired { action: "launch".into(), appid: self.core.games[i].appid, pos: (x, y) }),
                            Nav::Menu => fired.push(card_actions::Fired { action: "context_menu".into(), appid: self.core.games[i].appid, pos: (x, y) }),
                            Nav::Start if !self.select_mode => fired.push(card_actions::Fired { action: "launch".into(), appid: self.core.games[i].appid, pos: (x, y) }),
                            Nav::Store | Nav::Folder | Nav::Achievements | Nav::Hub if !self.select_mode => fired.push(card_actions::Fired { action: n.shortcut().unwrap().into(), appid: self.core.games[i].appid, pos: (x, y) }),
                            Nav::Edit if !self.select_mode => fired.push(card_actions::Fired { action: "edit".into(), appid: self.core.games[i].appid, pos: (x, y) }),
                            _ => {
                                let to = padnav::grid_move(&rows, i, n, page_rows);
                                if n == Nav::Up && to == i { if self.pad_bar_items.is_empty() { self.pad_nav = Some(self.page_nav_index()); } else { self.pad_bar = Some(self.pad_bar_items.len() / 2); } continue; }
                                self.pad_cur = self.core.games.get(to).map(|g| g.appid);
                                if let Some(r2) = rows.iter().position(|&(f, c)| to >= f && to < f + c) {
                                    // never past the end of the list: the last row's target used to lie a margin beyond it, and the page pulled back every frame
                                    let max_sy = (self.core.rows_height() - rect.height()).max(0.0);
                                    let ns = padnav::scroll_into_view(sy, rect.height(), live[r2].y, row_height(cw), 8.0).min(max_sy);
                                    if (ns - sy).abs() > 0.5 { self.scroll_to = Some(ns); sy = ns; }
                                }
                            }
                        }
                    }
                }
            }
        }
        let forced = if self.auto.is_some() { self.auto_step((h as f32 - 44.0).max(100.0)) } else { None };
        let t0 = Instant::now();
        let ctx = self.ctx.clone();
        let mut grid: Option<(egui::Rect, f32)> = None;
        let (mut pick, mut answer) = (None, None);
        let wake = hs.as_ref().and_then(|h| h.engine.deadline()).map(|d| d.saturating_duration_since(Instant::now()));
        self.tip_fields = hs.as_ref().and_then(|h| h.data.tip_fields("library"));
        if self.tip_fields.is_none() && hs.as_ref().map_or(false, |h| h.data.active_pag_spec().is_some()) { self.tip_fields = Some(HashSet::new()); }
        if self.list_mode {
            // the last selected game (kept for the session, like the web's sessionStorage), else the first game in the list
            if self.list_sel.is_none() && self.list_saved.is_none() {
                if let Some(h) = hs.as_ref() { self.list_sel = h.data.state_json().get("zest_list_selected").and_then(|v| v.as_i64()); self.list_saved = self.list_sel; self.list_center = true; }
            }
            let still_there = self.list_sel.map_or(false, |a| self.core.games.iter().any(|g| g.appid == a));
            if !still_there && !self.core.games.is_empty() {
                self.list_sel = self.core.games.first().map(|g| g.appid);
                self.list_center = true;
            }
            if let Some(h) = hs.as_ref() {
                if self.list_status.len() != self.core.games.len() { self.list_status = self.core.games.iter().filter_map(|g| h.data.info(g.appid).map(|i| (g.appid, i.status.clone()))).collect(); }
                if let Some(id) = self.list_sel { if self.list_edit.as_ref().map_or(true, |d| d.appid != id) { if let Some(old) = self.list_edit.as_ref() { artstore::discard(&h.data.root_dir(), old.appid); } self.list_edit = h.data.load_edit(id).map(|i| { let mut d = edit_dialog::EditDialog::new(id, i, true); d.account_id = h.data.account_id.clone(); d.db = Some(h.data.db_path()); d.quals = h.data.pag_panel_edit(id); d.store_text = h.store_link_text(id); if h.data.state_json().get("artwork_orientation").and_then(|v| v.as_str()) == Some("horizontal") { d.set_art_kind(1); } d }); } }
            }
        }
        let lib_pin = hs.as_ref().and_then(|h| h.tip_pin);
        if lib_pin.is_some() && self.tip_fields.is_none() { self.tip_fields = hs.as_ref().map(|h| h.data.tip_fields_any()); }
        self.tip_pin = lib_pin;
        if let Some(id) = lib_pin { self.tip_target = Some((id, egui::Rect::NOTHING)); }
        if let (Some(h), Some((id, _))) = (hs.as_ref(), self.tip_target) { if self.tip_cache.as_ref().map_or(true, |c| c.0 != id) { self.tip_cache = h.data.load_edit(id).map(|i| (id, i, h.data.tip_hltb(id))); } if self.tip_quals.as_ref().map_or(true, |c| c.0 != id) { self.tip_quals = Some((id, h.data.pag_panel(id))); } }
        let hovering_modal = hs.as_ref().map_or(false, |h| h.menu.is_some());
        if hovering_modal { self.tip_target = None; }
        let prev_target = self.tip_target.take();
        self.tip_target = prev_target;
        if ctx.global_style().interaction.selectable_labels { ctx.global_style_mut(|s| s.interaction.selectable_labels = false); }   // see the Home path: a selectable label is a target for the pad's Down
        let out = ctx.run_ui(raw, |ui| {
            if let Some(d) = self.pad_move.take() { ui.ctx().memory_mut(|m| m.move_focus(d)); }
            self.tip_target = None;
            self.ui(ui, forced, &mut grid);
            if let (true, Some((rect, sy)), Some(a)) = (self.pad_show, grid, self.pad_cur) {
                let rows: Vec<&grid_core::Row> = self.core.rows.iter().filter(|r| r.count > 0).collect();
                if let Some(i) = self.core.games.iter().position(|g| g.appid == a) {
                    if let Some(r) = rows.iter().find(|r| i >= r.first && i < r.first + r.count) {
                        let cw = self.core.card_w as f32;
                        let (x, y) = (rect.min.x + self.lay.x_off as f32 + (i - r.first) as f32 * (cw + GAP as f32), rect.min.y + r.y - sy + ROW_TOP);
                        let card = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(cw, grid_core::card_h(self.core.card_w) as f32));
                        // the hovered / cursor card grows (see the zoom above): the ring goes round the grown card
                        let k = self.lib_zoom.get(&a).map_or(1.0, |(t, _)| 1.0 + 0.05 * (t * (2.0 - t)));
                        self.pad_ring_card(ui.ctx(), egui::Rect::from_center_size(card.center(), card.size() * k), rect, if self.outlines.contains_key(&a) { Some(4.0 * k) } else { None });
                    }
                }
            }
            self.tip_ui(ui.ctx());
            if let Some(h) = hs.as_mut() { let (p, a) = Self::home_popups(ui, h); pick = p; answer = a; self.modal_ui(ui, h); self.edit_ui(ui, h, edit_info_tab); self.filter_ui(ui, h); self.bulk_ui(ui, h); self.pag_ui(ui, h); self.community_ui(ui, h); }
            self.pad_focus_ring(&ui.ctx().clone());   // last, after the dialogs: it reads this pass's rectangles, so the ring stays on a control that moved this frame
            if let Some(d) = wake { ui.ctx().request_repaint_after(d); }
            // the browser's answers (Steam dates) arrive on another thread and nothing else wakes the window: look again every 250 ms while a run is going. It has to be asked inside the pass; a request made before the pass starts is dropped.
            if hs.as_ref().map_or(false, |h| h.steam_dates.as_ref().map_or(false, |sd| sd.progress().is_some())) { ui.ctx().request_repaint_after(Duration::from_millis(250)); }
        });
        if let Some(h) = hs.as_mut() {
            h.finish(fired, pick, answer);
            if self.list_mode && self.list_sel.is_some() && self.list_sel != self.list_saved {
                self.list_saved = self.list_sel;
                h.data.save_pref("zest_list_selected", serde_json::json!(self.list_sel));
            }
            if let Some((id, p)) = self.list_menu.take() { h.menu = Some((id, p)); h.menu_rect = None; }
            if let (Some(id), Some(d)) = (h.art_staged_for, self.list_edit.as_mut()) { if d.appid == id { h.art_staged_for = None; d.art_changed(); self.ctx.request_repaint(); } }   // a new picture was staged: show it
            if let Some(out) = self.list_out.take() {
                use edit_dialog::EditOut as O;
                match (out, self.list_edit.as_mut()) {
                    (O::Save, Some(d)) => match h.data.save_edit(d.appid, &d.old, &d.f) {
                        Ok(()) => { d.old = d.f.clone(); d.status = None; self.list_status.clear(); h.dirty = true; h.data.library_changed(); h.commit_art(d.appid); d.art_changed(); h.toast("Saved"); }
                        Err(e) => { d.status = Some(format!("Could not save: {e}")); }
                    },
                    (O::Close, _) => { if let Some(d) = self.list_edit.as_ref() { artstore::discard(&h.data.root_dir(), d.appid); } self.list_edit = None; }   // Cancel: reload the saved values, and drop any picture the art tabs staged
                    (O::Delete, Some(d)) => { h.confirm = Some(d.appid); }
                    (O::Toast(t), _) => { h.toast(t); }
                    (O::SyncSteam, Some(d)) => { h.start_sync(&self.ctx, sync::Kind::Rescrape, vec![d.appid], "Syncing", None); }
                    (O::ArtRescrape(k), Some(d)) => { h.art_op(&self.ctx, d.appid, k, ArtAct::Order, "Art"); }
                    (O::ArtFromSteam(k), Some(d)) => { h.art_op(&self.ctx, d.appid, k, ArtAct::FromSteam, "Art from Steam"); }
                    (O::ArtUrl(k, u), Some(d)) => { h.art_op(&self.ctx, d.appid, k, ArtAct::Url(u), "Art"); }
                    (O::ArtCrop(k), Some(d)) => { match crop_dialog::CropDialog::open(&self.ctx, &h.data.root_dir(), d.appid, k) { Some(c) => self.crop = Some(c), None => h.toast("This game has no image to crop") } }
                    (O::ArtBrowse(_), Some(d)) => { open_url(&d.sgdb_page()); }
                    (O::ArtClear(k), Some(d)) => { artstore::stage_clear(&h.data.root_dir(), artstore::KINDS[k], d.appid); d.art_changed(); h.toast("Art cleared: SAVE keeps that, CANCEL brings it back"); }
                    (O::FillMissing, Some(d)) => { h.start_sync(&self.ctx, sync::Kind::Backfill, vec![d.appid], "Filling", None); }
                    (O::ProtondbRefresh, Some(d)) => { h.start_sync(&self.ctx, sync::Kind::ProtonDb, vec![d.appid], "ProtonDB", None); }
                    (O::HltbApplied, Some(d)) => { if let Some(i) = h.data.load_edit(d.appid) { d.refresh_hltb(&i); } h.dirty = true; }
                    (O::HltbRescrape, Some(d)) => { h.start_sync(&self.ctx, sync::Kind::Hltb, vec![d.appid], "HLTB", None); }
                    (O::HltbNone, Some(d)) => {
                        let r = rusqlite::Connection::open(h.data.db_path()).map_err(|e| e.to_string()).and_then(|c| pd_filter::hltb_ops::clear(&c, d.appid));
                        match r { Ok(()) => h.toast("HLTB match removed"), Err(e) => h.toast(format!("Could not clear it: {e}")) }
                    }
                    (O::Open(u), _) => { match u.strip_prefix("zest://steam-date/").and_then(|i| i.parse::<i64>().ok()) { Some(id) => h.steam_dates_run(&self.ctx.clone(), &[id], false), None => self.open_link(&u) } }
                    (O::DupLink(_) | O::DupUnlink | O::DupApprove(_) | O::DupReject, _) => {}
                    (O::Store, Some(d)) => { if d.is_steam() { open_url(&format!("https://store.steampowered.com/app/{}", d.appid)); } else { h.run_action("store", d.appid, (0.0, 0.0)); } }
                    _ => {}
                }
            }
            if let Some(id) = self.edit_click.take() { h.open_edit = Some(id); }
            let mut new_orientation: Option<String> = None;
            for (k, v) in std::mem::take(&mut self.view_changes) {
                if k == "artwork_orientation" { new_orientation = v.as_str().map(String::from); }
                h.data.save_pref(k, v);
                if k != "card_height" { h.dirty = true; }
            }
            if let Some(o) = new_orientation { self.apply_orientation(&o); }
            if std::mem::take(&mut self.want_clear) {
                // CLEAR (`clearAndReload`): no search, no saved filter; the hidden libraries stay as they are
                self.search.clear();
                self.search_edit_at = None;
                h.data.save_pref("filter_tree", serde_json::json!({"type": "group", "logic": "AND", "items": []}));
                h.dirty = true;
            }
            if std::mem::take(&mut self.want_show_all) {
                // "Show All Games" on the empty page (`_showAllGames`): the filter, the search and the hidden libraries
                self.search.clear();
                self.search_edit_at = None;
                h.data.save_pref("filter_tree", serde_json::json!({"type": "group", "logic": "AND", "items": []}));
                h.data.save_pref("hidden_platforms", serde_json::json!([]));
                h.dirty = true;
            }
            if std::mem::take(&mut self.dice_pick) { let vh = self.lib_grid.map_or(600.0, |(r, _)| r.height()); self.dice_roll(vh); }
            if self.search_edit_at.map_or(false, |t| t.elapsed() >= Duration::from_millis(400)) { self.persist_search(h); self.search_edit_at = None; }
        }
        self.home = hs;
        if let Some(si) = self.toggle.take() {
            if let Some(g) = self.groups.get(si) { if !self.collapsed.remove(&g.key) { self.collapsed.insert(g.key.clone()); } }
            self.core.toggle_section(si);
            self.layer.valid = false;
        }
        let t_run = t0.elapsed().as_secs_f32() * 1000.0;
        if !self.packs_checked && self.pack_job.is_none() && self.shot_size.is_none() { self.packs_checked = true; self.pack_job = Some(grid_core::update_packs_async(&self.root)); }
        if let Some(job) = &self.pack_job {
            if job.done.load(std::sync::atomic::Ordering::Relaxed) {
                let changed = job.changed.lock().unwrap().clone();
                self.core.reload_packs(&self.root, &changed);
                self.layer.valid = false;
                self.pack_job = None;
                self.settle_until = Some(Instant::now() + Duration::from_secs(3));
            }
        }
        if let (Some(st), Some(h)) = (self.selftest.as_mut(), self.home.as_mut()) {
            let n = self.core.games.len();
            match st.0 {
                0 if n > 0 && self.core.card_state(0) == 3 => {
                    let id = self.core.games[0].appid;
                    println!("selftest: card 0 (appid {id}) is sharp; clearing its cover");
                    println!("selftest: clear_art -> {:?}", h.data.clear_art(id, "vertical"));
                    h.packs_stale = true; h.covers.invalidate(id);
                    *st = (1, Instant::now());
                }
                1 if st.1.elapsed() > Duration::from_secs(4) => {
                    println!("selftest: after 4 s the card shows state {} (0 nothing, 1 blurry, 2 sharp other size, 3 sharp); has_image {}", self.core.card_state(0), self.core.has_image(0));
                    std::process::exit(0);
                }
                _ => {}
            }
            self.keep_drawing = true;
        }
        if self.tier_pending && self.tier_job.is_none() {
            let width = grid_core::bucket(self.lay.cw);
            self.tier_job = Some((grid_core::build_tier_async(&self.root, width), width));
            self.tier_pending = false;
        }
        if let Some((done, width)) = &self.tier_job {
            if done.load(std::sync::atomic::Ordering::Relaxed) { let w = *width; self.core.adopt_tier(&self.root, w); self.tier_job = None; }
        }
        self.egui_winit.as_mut().unwrap().handle_platform_output(&window, out.platform_output);
        let ppp = out.pixels_per_point;
        let prims = ctx.tessellate(out.shapes, ppp);

        // --- our grid layer
        let t1 = Instant::now();
        let (rect, vy) = grid.unwrap();
        self.lib_grid = Some((rect, vy));
        // the same for the Library: the rows on screen, a couple of screens either side, and the whole filtered list
        if let (Some(hs), false) = (self.home.as_ref(), self.list_mode) {
            let vh = rect.height();
            let ids = |range: std::ops::Range<usize>| -> std::collections::HashSet<i64> {
                range.flat_map(|i| { let r = &self.core.rows[i]; (r.first..r.first + r.count).filter_map(|g| self.core.games.get(g).map(|x| x.appid)).collect::<Vec<_>>() }).collect()
            };
            let vis = ids(self.core.rows_in(vy, vy + vh));
            let games = &self.core.games;
            let sig = ((games.len(), games.first().map_or(0, |g| g.appid), games.last().map_or(0, |g| g.appid)), (vis.len(), vis.iter().fold(0i64, |a, x| a.wrapping_mul(31).wrapping_add(*x) ^ x)));
            let home_last = hs.focus_sig.0 != usize::MAX;   // the Home page published last: its sets are not the Library's
            let full = home_last || self.lib_focus_sig.0 != sig.0;
            if full || self.lib_focus_sig != sig {
                let mut near = ids(self.core.rows_in(vy - 2.0 * vh, vy + 3.0 * vh)); near.retain(|a| !vis.contains(a));
                if full { hs.art_focus.set(vis, near, games.iter().map(|g| g.appid).collect()); } else { hs.art_focus.set_view(vis, near); }
                self.lib_focus_sig = sig;
                if home_last { if let Some(h) = self.home.as_mut() { h.focus_sig = (usize::MAX, 0, 0, 0); } }
            }
        }
        let (gx, gy) = (rect.min.x.round().max(0.0) as usize, rect.min.y.round().max(0.0) as usize);
        let (gw, gh) = (((rect.width().round() as usize).min(w.saturating_sub(gx))).max(1), ((rect.height().round() as usize).min(h.saturating_sub(gy))).max(1));
        let y = if self.auto.is_some() { forced.unwrap_or(self.y) } else { vy }.round();
        self.y = y;
        let dir = if y >= self.last_y { 1 } else { -1 };
        self.last_y = y;
        let lay = self.lay;
        self.core.update_view(y, gh as f32, dir);
        self.layer.update(&mut self.core, gw, gh, y as i64, lay, &changed);
        let t_grid = t1.elapsed().as_secs_f32() * 1000.0;
        {
            // --- resize logging (interactive): states of the visible cards this frame vs the last
            let rl = &mut self.rlog;
            let now = Instant::now();
            if w != rl.last_w { if rl.last_change.is_none() { rl.started = Some(now); rl.full_redraws0 = self.layer.stats.0; rl.events = 0; } rl.last_change = Some(now); rl.events += 1; }
            rl.last_w = w;
            if rl.last_change.is_some() {
                let (cols, rowh) = (self.core.cols, row_height(self.core.card_w));
                let (first, last) = ((y / rowh).floor().max(0.0) as usize, ((y + gh as f32) / rowh).ceil().max(0.0) as usize);
                let n = self.core.games.len();
                let mut st = [0u32; 4];
                let mut regress = 0;
                rl.prev.resize(n, 0);
                for i in (first * cols)..(((last + 1) * cols).min(n)) {
                    let s = self.core.card_state(i);
                    st[s as usize] += 1;
                    if rl.prev[i] >= 2 && s < 2 { regress += 1; }
                    rl.prev[i] = s;
                }
                let gap = rl.last_frame.map_or(0.0, |t| now.duration_since(t).as_secs_f32() * 1000.0);
                rl.recs.push(FrameRec { t: rl.started.unwrap().elapsed().as_secs_f32(), w, cols: self.core.cols, cw: self.core.card_w, st, regress, ms: t_frame.elapsed().as_secs_f32() * 1000.0, gap_ms: gap });
            }
            rl.last_frame = Some(now);
            if rl.last_change.map_or(false, |t| now.duration_since(t).as_secs_f32() > 1.0) && !rl.recs.is_empty() {
                let r = &rl.recs;
                let (w0, w1) = (r[0].w, r.iter().rev().find(|x| x.w > 0).unwrap().w);
                let missing = r.iter().filter(|x| x.st[0] > 0).count();
                let blurry = r.iter().filter(|x| x.st[1] > 0).count();
                let regress: u32 = r.iter().map(|x| x.regress).sum();
                let mut ms: Vec<f32> = r.iter().map(|x| x.ms).collect(); ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
                let gaps = r.iter().filter(|x| x.gap_ms > 40.0).count();
                let worst_gap = r.iter().map(|x| x.gap_ms).fold(0.0f32, f32::max);
                println!("[resize] {} frames over {:.1} s, {} resize events, width {} -> {} px, cols {} -> {}, card {} -> {} px, {} full redraws",
                    r.len(), r.last().unwrap().t, rl.events, w0, w1, r[0].cols, r.last().unwrap().cols, r[0].cw, r.last().unwrap().cw, self.layer.stats.0.saturating_sub(rl.full_redraws0));
                println!("         frames with a card showing NOTHING: {} | with a blurry-only card: {} | cards that lost a full cover between frames: {}", missing, blurry, regress);
                println!("         frame time p50 {:.1} p95 {:.1} max {:.1} ms | gaps over 40 ms between frames: {} (worst {:.0} ms)", ms[ms.len() / 2], ms[ms.len() * 95 / 100], ms[ms.len() - 1], gaps, worst_gap);
                let odd: Vec<&FrameRec> = r.iter().filter(|x| x.st[0] > 0 || x.st[1] > 0 || x.regress > 0 || x.gap_ms > 40.0).take(10).collect();
                for x in odd { println!("           t={:.2}s w={} cols={} cw={} | cards: none {} blurry {} other-size {} exact {} | lost {} | frame {:.1} ms, gap {:.0} ms", x.t, x.w, x.cols, x.cw, x.st[0], x.st[1], x.st[2], x.st[3], x.regress, x.ms, x.gap_ms); }
                rl.recs.clear(); rl.last_change = None; rl.started = None;
            }
        }
        if let Some(rt) = &mut self.rt {
            let t = rt.start.elapsed().as_secs_f32();
            let (ok, any, tot) = self.core.ready(y, gh as f32);
            if t > 2.0 && t < 6.0 { rt.frames += 1; if any < tot { rt.blank += 1; } if ok < tot { rt.nonsharp += 1; } }
            // flicker proper: a visible card that had a full cover last frame and has only a placeholder or nothing now
            let (cols, rowh) = (self.core.cols, row_height(self.core.card_w));
            let (first, last) = ((y / rowh).floor().max(0.0) as usize, ((y + gh as f32) / rowh).ceil().max(0.0) as usize);
            let n = self.core.games.len();
            let states: Vec<u8> = ((first * cols)..(((last + 1) * cols).min(n))).map(|i| self.core.card_state(i)).collect();
            if t > 2.0 && t < 6.0 && first * cols == rt.prev.len().saturating_sub(0) * 0 + first * cols {
                for (k, st) in states.iter().enumerate() {
                    let idx = first * cols + k;
                    if let Some(pst) = rt.prev.get(idx) { if *pst >= 2 && *st < 2 { rt.regress += 1; } }
                }
            }
            rt.prev.resize(n, 0);
            for (k, st) in states.iter().enumerate() { rt.prev[first * cols + k] = *st; }
            if t >= 6.0 && rt.settled_at.is_none() && ok == tot { rt.settled_at = Some(t - 6.0); }
            if t > 8.0 {
                println!("resize test (target ~{} px, drag 1300 -> ~{} px wide and back, then hold):", self.cw.round() as u32, rt.base as i32 - 420);
                println!("  window width during the test: {} .. {} px ({})", rt.w_min, rt.w_max, if rt.real { "real window resize" } else { "simulated" });
                println!("  cards that LOST a full cover between two frames (the flicker): {}  ({:.2} per frame)", rt.regress, rt.regress as f64 / rt.frames.max(1) as f64);
                println!("  during the drag: {} frames, {} with a blank card ({:.0}%), {} with any card not yet sharp at its exact size ({:.0}%)",
                    rt.frames, rt.blank, rt.blank as f64 * 100.0 / rt.frames.max(1) as f64, rt.nonsharp, rt.nonsharp as f64 * 100.0 / rt.frames.max(1) as f64);
                match rt.settled_at { Some(s) => println!("  after the drag stops: every visible card sharp at the new size after {:.2} s", s),
                                      None => println!("  after the drag stops: NOT all sharp within 2 s") }
                std::process::exit(0);
            }
        }

        // --- compose into the window buffer, then the egui overlay on top
        let t2 = Instant::now();
        let mut local: Vec<u32> = Vec::new();
        let mut surf = None;
        if self.shot_size.is_some() { local = vec![0u32; w * h]; } else { surf = Some(self.surface.as_mut().unwrap().buffer_mut().unwrap()); }
        let buffer: &mut [u32] = match surf.as_mut() { Some(b) => &mut b[..], None => &mut local[..] };
        self.bg.paint_into(buffer, w, h);
        for r in 0..gh {   // the layer is transparent between cards: the background shows through
            let dst = &mut buffer[(gy + r) * w + gx..(gy + r) * w + gx + gw];
            for (d, s) in dst.iter_mut().zip(&self.layer.buf[r * gw..(r + 1) * gw]) { if *s >> 24 == 0 { *d = *s; } }
        }
        let t_compose = t2.elapsed().as_secs_f32() * 1000.0;
        let t3 = Instant::now();
        let mut bref = BufferMutRef::new(bytemuck::cast_slice_mut(buffer), w, h);
        self.renderer.render(&mut bref, &prims, &out.textures_delta, ppp);
        let hs_live_done = self.home.as_ref().map_or(true, |h| h.live_rx.is_none());
        if let Some((path, n)) = &mut self.shot {
            if self.core.pending() == 0 && changed.is_empty() && self.edit.as_ref().map_or(true, |d| d.has_cover()) && hs_live_done && self.pag.as_ref().map_or(true, |p| p.settled()) && self.miam.as_ref().map_or(true, |m| m.settled()) && self.blaeo.as_ref().map_or(true, |b| b.settled()) { *n += 1; }
            if *n >= 8 + 2 * SHOT_PAD_N.load(std::sync::atomic::Ordering::Relaxed) as u32 && SHOT_PAD.lock().unwrap().is_empty() && SHOT_SCRIPT.lock().unwrap().is_empty() {
                let rgb: Vec<u8> = buffer.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8]).collect();
                image::RgbImage::from_raw(w as u32, h as u32, rgb).unwrap().save(&path).unwrap();
                println!("screenshot {}x{} -> {}", w, h, path.display());
                #[cfg(feature = "dhat-heap")]
                { let p = PROFILER.lock().unwrap().take(); drop(p); }
                std::process::exit(0);
            }
        }
        let t_over = t3.elapsed().as_secs_f32() * 1000.0;
        let t4 = Instant::now();
        let stale = self.shot.is_none() && self.auto.is_none() && !WINDOW_SHOWN.swap(true, std::sync::atomic::Ordering::Relaxed) && reveal_window(&window);
        if let Some(b) = surf.take() { if !stale { b.present().ok(); finish_reveal(&window); } }
        drop(surf);
        let t_present = t4.elapsed().as_secs_f32() * 1000.0;

        if self.auto.is_some() && self.auto.as_ref().map_or(false, |a| !a.fresh && a.started.elapsed().as_secs_f32() > 1.5) {
            self.parts.push([t_run, t_grid, t_compose, t_over, t_present, t_frame.elapsed().as_secs_f32() * 1000.0]);
        } else if self.auto.is_none() {
            self.parts.push([t_run, t_grid, t_compose, t_over, t_present, t_frame.elapsed().as_secs_f32() * 1000.0]);
            if self.live.elapsed().as_secs_f32() >= 2.0 {
                if self.intervals.len() > 8 && (self.layer.stats.1 > 0) {
                    let mut iv = self.intervals.clone(); iv.sort_by(|a, b| a.partial_cmp(b).unwrap());
                    println!("[scroll] {} frames in 2 s, interval p50 {:.1} p95 {:.1} max {:.1} ms | {} px exposed, {} full redraws | RSS {} MB",
                        iv.len(), iv[iv.len() / 2], iv[iv.len() * 95 / 100], iv[iv.len() - 1], self.layer.stats.1, self.layer.stats.0, rss_mb());
                    self.report();
                }
                self.intervals.clear(); self.parts.clear(); self.layer.stats = (0, 0);
                self.live = Instant::now();
            }
        }

        // --- repaint scheduling
        let delay = out.viewport_output.get(&egui::ViewportId::ROOT).map(|v| v.repaint_delay).unwrap_or(Duration::MAX);
        self.keep_drawing = self.selftest.is_some() || covers_changed_lib || self.edit.as_ref().map_or(false, |d| !d.has_cover()) || self.auto.is_some() || self.rt.is_some() || self.shot.is_some() || !changed.is_empty() || self.core.pending() > 0 || delay == Duration::ZERO;
        self.next_wake = if !self.keep_drawing && delay < Duration::from_secs(3600) { Some(Instant::now() + delay) } else { None };
        if self.settle_until.map_or(false, |t| Instant::now() < t) && !self.keep_drawing { let t = Instant::now() + Duration::from_millis(100); self.next_wake = Some(self.next_wake.map_or(t, |w| w.min(t))); }
        if (self.tier_job.is_some() || self.pack_job.is_some() || self.search_edit_at.is_some()) && !self.keep_drawing { let t = Instant::now() + Duration::from_millis(500); self.next_wake = Some(self.next_wake.map_or(t, |w| w.min(t))); }
        if self.bg.size != (w, h) && !self.keep_drawing { self.next_wake = Some(Instant::now() + Duration::from_millis(130)); }
        if self.keep_drawing { window.request_redraw(); }
        if let Some(p) = self.want_page.take() {
            self.page = p;
            if p == Page::Library { self.lib_loaded = false; }
            else { self.core.release_caches(); self.layer = Layer::new(); }   // leaving the Library: its decoded covers and scroll layer are not kept
            window.request_redraw();
        }
        let _ = el;
    }
}

impl ApplicationHandler for Own {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_some() { return; }
        let saved = self.home.as_ref().and_then(|h| h.data.state_json().get("zest_window").cloned());
        let get = |k: &str| saved.as_ref().and_then(|v| v.get(k)).and_then(|v| v.as_u64()).map(|v| v as f64);
        // the saved size is in physical pixels (what the window measured itself at): restored as such, and a value no screen has (a leftover of the old points-for-pixels doubling, which reached 41600 x 28800 on a
        // Retina Mac after four starts and made the window crawl open) is ignored for the default size
        let screen = el.primary_monitor().or_else(|| el.available_monitors().next()).map(|m| (m.size().width, m.size().height));
        let saved_size = get("w").zip(get("h")).filter(|(w, h)| (400.0..=7680.0).contains(w) && (300.0..=4320.0).contains(h))
            // a saved normal size that fills the screen (see `track_window`) gives way to the default size
            .filter(|(w, h)| screen.map_or(true, |s| !fills_screen((*w as u32, *h as u32), s)));
        // never bigger than 90% of the screen: the default (1300 x 900 points) is larger than a 13-inch Mac's 1280 x 800, and a window opened larger than the screen
        // zoomed to a size wider than it (seen on the Retina MacBook)
        let scale = el.primary_monitor().or_else(|| el.available_monitors().next()).map(|m| m.scale_factor()).unwrap_or(1.0);
        let want = match saved_size { Some((w, h)) => (w, h), None => (self.size[0] as f64 * scale, self.size[1] as f64 * scale) };
        let (sw, sh) = screen.map(|(w, h)| fit_on_screen(want, (w as f64, h as f64))).unwrap_or(want);
        let start_size: winit::dpi::Size = winit::dpi::PhysicalSize::new(sw.round() as u32, sh.round() as u32).into();
        let maximized = saved.as_ref().and_then(|v| v.get("maximized")).and_then(|v| v.as_bool()).unwrap_or(false);
        { let (r, g, b) = self.theme.bg_page; WINDOW_BG.store(((b as u32) << 16) | ((g as u32) << 8) | r as u32, std::sync::atomic::Ordering::Relaxed); }
        WANT_MAX.store(maximized, std::sync::atomic::Ordering::Relaxed);   // applied when the window is shown: a window created hidden but maximized flashed a strip on Windows
        let mut attrs = Window::default_attributes().with_title("Zest")
            .with_inner_size(start_size)
            .with_visible(false);   // hidden until the first frame is drawn (see WINDOW_SHOWN); test runs (screenshots, --auto) never show a window
        // The window icon: X11 and Windows take it from here; Wayland takes it from the desktop entry whose name matches the app id.
        if let Ok(img) = image::load_from_memory(include_bytes!("../../assets/favicon.png")) {
            let img = img.resize(128, 128, image::imageops::FilterType::Lanczos3).to_rgba8();
            if let Ok(icon) = winit::window::Icon::from_rgba(img.to_vec(), img.width(), img.height()) {
                // Windows draws the taskbar button from the big icon, the title bar from the small one: both are set (with only the small one the taskbar showed a generic window)
                #[cfg(windows)] { use winit::platform::windows::WindowAttributesExtWindows; attrs = attrs.with_taskbar_icon(Some(icon.clone())); }
                attrs = attrs.with_window_icon(Some(icon));
            }
        }
        #[cfg(target_os = "linux")]
        {
            use winit::platform::wayland::WindowAttributesExtWayland;
            // the desktop entry whose name matches this gives the window its icon: the Flatpak's own id, "zest" for a release build (install.sh's zest.desktop), "zest-dev" for a dev build
            let id = std::env::var("FLATPAK_ID").unwrap_or_else(|_| if option_env!("ZEST_BUILD_VERSION").is_some() { "zest".into() } else { "zest-dev".into() });
            attrs = attrs.with_name(id.clone(), id);
        }
        let window = Rc::new(el.create_window(attrs).expect("window"));
        // Steam Deck Game Mode (gamescope) shows one app filling the screen: a window of any other shape was shrunk to fit with bars beside it, so it opens full screen there
        if std::env::var_os("GAMESCOPE_WAYLAND_DISPLAY").is_some() || std::env::var("XDG_SESSION_DESKTOP").map_or(false, |d| d == "gamescope") { window.set_fullscreen(Some(winit::window::Fullscreen::Borderless(None))); }
        // the covers are laid out in physical pixels and egui draws in points, so a screen scale above 1 (a Retina Mac, Windows at 125% or 150%) put every label at the wrong place: egui is told to treat one point as one physical pixel
        self.ctx.set_zoom_factor(1.0 / window.scale_factor() as f32);
        let context = softbuffer::Context::new(el.owned_display_handle()).expect("softbuffer context");
        self.surface = Some(softbuffer::Surface::new(&context, window.clone()).expect("surface"));
        self.egui_winit = Some(egui_winit::State::new(self.ctx.clone(), egui::ViewportId::ROOT, &window, Some(window.scale_factor() as f32), None, None));
        window.request_redraw();
        self.window = Some(window);
        // a hidden window gets no redraw request on Windows, so the first picture is drawn here; frame() then shows the window
        if cfg!(windows) && self.shot.is_none() && self.auto.is_none() { self.frame(el); }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, ev: WindowEvent) {
        match ev {
            WindowEvent::CloseRequested => { self.leave(el) }
            WindowEvent::RedrawRequested => { LAST_FRAME_MS.store(now_ms(), std::sync::atomic::Ordering::Relaxed); let t0 = Instant::now(); self.frame(el); if std::env::var_os("ZEST_FRAME_MS").is_some() { eprintln!("frame {:.1} ms", t0.elapsed().as_secs_f64() * 1000.0); } }
            other => {
                if let winit::event::WindowEvent::ScaleFactorChanged { scale_factor, .. } = &other { self.ctx.set_zoom_factor(1.0 / *scale_factor as f32); }   // moved to another screen: still one point per physical pixel
                if let (Some(w), Some(st)) = (self.window.clone(), self.egui_winit.as_mut()) {
                    let r = st.on_window_event(&w, &other);
                    if std::env::var_os("ZEST_KEYLOG").is_some() {
                        match &other {
                            winit::event::WindowEvent::KeyboardInput { event, .. } => pd_filter::net::log(&format!("keylog: {:?} {:?} text={:?} repeat={} consumed={} wants_kb={}", event.state, event.logical_key, event.text, event.repeat, r.consumed, self.ctx.egui_wants_keyboard_input())),
                            winit::event::WindowEvent::Ime(i) => pd_filter::net::log(&format!("keylog: ime {i:?} consumed={}", r.consumed)),
                            winit::event::WindowEvent::ModifiersChanged(m) => pd_filter::net::log(&format!("keylog: modifiers {:?}", m.state())),
                            _ => {}
                        }
                    }
                    if r.repaint { w.request_redraw(); }
                }
            }
        }
    }

    fn user_event(&mut self, _el: &ActiveEventLoop, _: ()) {   // the gamepad thread queued something
        if let Some(w) = &self.window { w.request_redraw(); }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        // safety net: if no first frame ever shows the hidden window (a page that draws another way), it appears after 3 s anyway; a cloaked one is let out after 5 s
        if self.shot.is_none() && self.auto.is_none() && (!WINDOW_SHOWN.load(std::sync::atomic::Ordering::Relaxed) || UNCLOAK.load(std::sync::atomic::Ordering::Relaxed)) {
            static T0: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
            let up = T0.get_or_init(Instant::now).elapsed();
            if !WINDOW_SHOWN.load(std::sync::atomic::Ordering::Relaxed) && up > Duration::from_secs(3) { if let Some(w) = &self.window { WINDOW_SHOWN.store(true, std::sync::atomic::Ordering::Relaxed); reveal_window(w); } }
            if UNCLOAK.load(std::sync::atomic::Ordering::Relaxed) && up > Duration::from_secs(5) { if let Some(w) = &self.window { finish_reveal(w); } }
            el.set_control_flow(ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(500)));
        }
        match self.next_wake {
            Some(t) if Instant::now() >= t => { self.next_wake = None; if let Some(w) = &self.window { w.request_redraw(); } el.set_control_flow(ControlFlow::Wait); }
            Some(t) => el.set_control_flow(ControlFlow::WaitUntil(t)),
            None => el.set_control_flow(ControlFlow::Wait),
        }
    }
}

/// Shows the window that was created hidden. Windows would fade it in (an empty window for a moment) and maximizing it afterwards redrew nothing at the new size, so the open animation
/// is switched off for this one show and, when the window is to start maximized, that is done first. True = the picture just drawn has the old size, so it is not presented: the
/// next frame (asked for here) is drawn at the right size.
/// After a Flatpak update: starts the new version once this one has quit. It runs on the host, detached (`setsid` in a shell that returns at once, or the `flatpak-spawn`
/// client keeps this old sandbox alive), after a pause so this copy has let go of its lock. Started from Steam (Game Mode, a Steam shortcut) it goes through Steam
/// (`steam://rungameid/<SteamGameId>`), so Steam and gamescope show it as the same game; otherwise `flatpak run`.
fn relaunch_flatpak() {
    let id = std::env::var("FLATPAK_ID").unwrap_or_else(|_| "io.github.robbyratpoison.Zest".into());
    let game = std::env::var("SteamGameId").ok().filter(|g| !g.is_empty() && g != "0" && g.bytes().all(|b| b.is_ascii_digit()));
    let start = match &game { Some(g) => format!("steam steam://rungameid/{g}"), None => format!("flatpak run {id}") };
    let script = format!("setsid sh -c 'sleep 3; {start}' >/dev/null 2>&1 </dev/null &");
    pd_filter::net::log(&format!("update: restarting with: {start}"));
    let _ = std::process::Command::new("flatpak-spawn").args(["--host", "sh", "-c", &script]).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
}

fn reveal_window(window: &Window) -> bool {
    #[cfg(windows)] { dark_background(window); dwm_transitions(window, false); dwm_cloak(window, true); UNCLOAK.store(true, std::sync::atomic::Ordering::Relaxed); }
    let max = WANT_MAX.load(std::sync::atomic::Ordering::Relaxed);
    if max { window.set_maximized(true); }
    window.set_visible(true); window.focus_window(); window.request_redraw();
    #[cfg(windows)] dwm_transitions(window, true);
    max
}
#[cfg(windows)]
fn dark_background(window: &Window) {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    #[link(name = "gdi32")] extern "system" { fn CreateSolidBrush(color: u32) -> isize; }
    #[link(name = "user32")] extern "system" { fn SetClassLongPtrW(hwnd: isize, index: i32, value: isize) -> usize; }
    if let Ok(h) = window.window_handle() { if let RawWindowHandle::Win32(w) = h.as_raw() {
        // GCLP_HBRUSH: what Windows paints a window with before the first picture arrives (it was white): the page color of the user's theme (WINDOW_BG, COLORREF 0x00BBGGRR)
        unsafe { let brush = CreateSolidBrush(WINDOW_BG.load(std::sync::atomic::Ordering::Relaxed)); SetClassLongPtrW(w.hwnd.get(), -10, brush); }
    } }
}
/// While cloaked a window is shown to Windows but not to the screen: the first picture is drawn into it and only then is it uncloaked, so no unpainted frame (white) is ever seen.
#[cfg(windows)]
fn dwm_cloak(window: &Window, on: bool) {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    #[link(name = "dwmapi")] extern "system" { fn DwmSetWindowAttribute(hwnd: isize, attr: u32, value: *const std::ffi::c_void, size: u32) -> i32; }
    if let Ok(h) = window.window_handle() { if let RawWindowHandle::Win32(w) = h.as_raw() {
        let v: i32 = on as i32;   // DWMWA_CLOAK
        unsafe { DwmSetWindowAttribute(w.hwnd.get(), 13, &v as *const i32 as *const std::ffi::c_void, 4); }
    } }
}
/// After the first picture of the revealed window has been presented: let it be seen.
fn finish_reveal(window: &Window) {
    #[cfg(windows)] { if UNCLOAK.swap(false, std::sync::atomic::Ordering::Relaxed) { dwm_cloak(window, false); } }
    #[cfg(not(windows))] { let _ = window; }
}
static UNCLOAK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
#[cfg(windows)]
fn dwm_transitions(window: &Window, on: bool) {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    #[link(name = "dwmapi")] extern "system" { fn DwmSetWindowAttribute(hwnd: isize, attr: u32, value: *const std::ffi::c_void, size: u32) -> i32; }
    if let Ok(h) = window.window_handle() { if let RawWindowHandle::Win32(w) = h.as_raw() {
        let disabled: i32 = if on { 0 } else { 1 };   // DWMWA_TRANSITIONS_FORCEDISABLED
        unsafe { DwmSetWindowAttribute(w.hwnd.get(), 3, &disabled as *const i32 as *const std::ffi::c_void, 4); }
    } }
}
static WINDOW_BG: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x0012_0C08);
/// Games whose uninstaller finished (see `winuninstall::watch_gone`), waiting for the UI thread to clear their install state.
static UNINSTALLED: std::sync::Mutex<Vec<i64>> = std::sync::Mutex::new(Vec::new());
static WANT_MAX: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static WINDOW_SHOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static LAST_FRAME_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
fn now_ms() -> u64 { static T0: std::sync::OnceLock<Instant> = std::sync::OnceLock::new(); T0.get_or_init(Instant::now).elapsed().as_millis() as u64 }

/// glibc keeps freed memory in its heap (50 to 80 MB after a Library scroll). Hand it back whenever the window has been idle for a moment,
/// so it never runs while frames are being drawn.
fn start_idle_trimmer() {
    std::thread::spawn(|| {
        let mut trimmed_at = 0u64;
        loop {
            std::thread::sleep(Duration::from_millis(700));
            let last = LAST_FRAME_MS.load(std::sync::atomic::Ordering::Relaxed);
            if last > trimmed_at && now_ms().saturating_sub(last) > 700 { #[cfg(all(target_os = "linux", target_env = "gnu"))] unsafe { libc::malloc_trim(0); } trimmed_at = now_ms(); }
        }
    });
}

/// Where a user's data lives when no folder is given: `ZEST_DATA`; Windows next to the program; macOS Application Support; Linux `$XDG_DATA_HOME/zest` (default
/// `~/.local/share/zest`; in the Flatpak `~/.var/app/io.github.robbyratpoison.Zest/data/zest`). Never PlayDate's folder: Zest is a separate app and imports a copy of PlayDate's library.
pub fn default_data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("ZEST_DATA").filter(|d| !d.is_empty()) { return PathBuf::from(d); }
    let home = || std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_default();
    let xdg = || std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()).map(PathBuf::from).unwrap_or_else(|| home().join(".local/share"));
    if cfg!(windows) { return std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.to_path_buf())).unwrap_or_else(|| PathBuf::from(".")); }
    if cfg!(target_os = "macos") { return home().join("Library/Application Support/Zest"); }
    xdg().join("zest")
}

/// Everything the window needs once a library exists: the model, Home, theme, the page to open on. Used at start, and again when the first-run setup hands over its window.
fn build_own(root: &PathBuf, args: &[String], first_note: Option<String>, ctx: egui::Context) -> Own {
    let root = root.clone();
    // the Library's cover shape (Display: Vertical / Horizontal) is read once at start; a change takes effect after a restart
    if let Ok(t) = std::fs::read_to_string(root.join("state.json")) {
        if serde_json::from_str::<serde_json::Value>(&t).ok().and_then(|v| v.get("artwork_orientation").and_then(|o| o.as_str()).map(|o| o == "horizontal")).unwrap_or(false) { grid_core::HORIZONTAL.store(true, std::sync::atomic::Ordering::Relaxed); }
    }
    let list_start = std::fs::read_to_string(root.join("state.json")).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()).and_then(|v| v.get("artwork_orientation").and_then(|o| o.as_str()).map(|o| o == "list")).unwrap_or(false);
    grid_core::set_bucket_step(1);
    let arg = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let size: [f32; 2] = arg("--size").and_then(|v| { let (w, h) = v.split_once('x')?; Some([w.parse().ok()?, h.parse().ok()?]) }).unwrap_or([1300.0, 900.0]);
    let cw: f32 = arg("--cw").and_then(|v| v.parse().ok()).unwrap_or(200.0);
    let auto = args.iter().any(|a| a == "--auto");
    let resize_test = args.iter().any(|a| a == "--resize-test");
    let core = Core::new(&root, |w, h, rgba: Vec<u8>| {
        let px: Vec<u32> = rgba.chunks_exact(4).map(|p| ((p[0] as u32) << 16) | ((p[1] as u32) << 8) | p[2] as u32).collect();
        Arc::new(Pixels { w: w as usize, h: h as usize, px })
    });
    let wheel: f32 = arg("--wheel").and_then(|v| v.parse().ok()).unwrap_or(150.0);   // pixels per wheel click (egui default: 40)
    ctx.options_mut(|o| o.input_options.line_scroll_speed = wheel);
    ctx.all_styles_mut(|s| s.interaction.select_word_on_press = true);
    ctx.all_styles_mut(|s| s.spacing.scroll.fade.strength = 0.0);   // egui's gradient at the edge of every scroll area faded the buttons that sat there
    let loaded = load_sources(&root);
    let home = match home::load(&root) { Ok(d) => { let rules = card_actions::rules_from_state(d.state_json()); let state_for_pick = d.state_json().clone(); Some(HomeState { art_staged_for: None, lib_gen_seen: sync::library_gen(), lib_gen_at: None, art_seen: 0, art_seen_at: None, late_notices: Default::default(), dates_run: None, installed_scanned: false, custom_checked: None, playtime_rx: None, playtime_started: false, startup_at: None, startup_checked: false, startup_ready: Default::default(), live_rx: None, live_at: None, live_arrived: false, companion_gen: 0, companion: None, steam_dates: None, edit_art: None, companion_events: Default::default(), companion_ctx: None, pushes: Vec::new(), steam_watch: None, install_watch: None, steam_pending: None, data: d, covers: home::Covers::new(root.clone()), size: (0, 0), changed_at: Instant::now(), engine: card_actions::Engine::new(rules), pointer: None, press: None, hover: Default::default(), anim_at: Instant::now(), shelf_pos: Default::default(), slide_at: Instant::now(), home_scroll: 0.0, home_scroll_target: 0.0, scroll_at: Instant::now(), notices: Vec::new(), sync_status: pd_filter::status::load(&root), menu: None, menu_rect: None, confirm: None, update_rx: None, update_started: args.iter().any(|a| a == "--screenshot" || a == "--auto" || a.starts_with("--selftest") || a == "--update-packs" || a == "--build-tier" || a == "--build-pack"), populate_pending: false, job_queue: Vec::new(), install_target: None, install_opts_slot: Default::default(), install_pick: None, exe_pick: None, sub_rect: None, open_edit: None, tip_pin: None, shelf_off: Default::default(), wheel_acc: 0.0, entering: true, hold_until: None, accent: egui::Color32::from_rgb(0x66, 0xc0, 0xf4), dirty: false, pick: pick_page::PickState::new(&state_for_pick), sync: None, art_job: None, fill_art: None, art_to_fill: false, opt_meta: true, opt_cheevo: true, opt_art_kinds: vec![pd_filter::artclient::Kind::Vertical, pd_filter::artclient::Kind::Horizontal, pd_filter::artclient::Kind::Icon], opt_art_source: "auto", sources: loaded.0, plugins: loaded.1, plugin_errors: loaded.2, packs_stale: false, art_focus: pd_filter::artfocus::ArtFocus::new(), focus_sig: (usize::MAX, 0, 0, 0), dates_pending: Vec::new(), base: None }) }, Err(e) => { eprintln!("Home page not available: {e}"); None } };
    let mut home = home;
    if let Some(h) = home.as_mut() {
        // what Steam says is installed right now, and a watcher so it stays true while the app is open
        if let Some(n) = h.data.sync_installed_from_steam() { eprintln!("steam: {n} installed games"); }
        if args.iter().all(|a| a != "--screenshot") {
            if let Some(root) = h.data.steam_root() { h.steam_watch = start_steam_watch(pd_filter::steamlocal::library_paths(&root), ctx.clone()); }
            h.install_watch = install_watch::InstallWatch::new(ctx.clone());
        }
    }
    // the page to open on: the command line wins, else Settings -> Library -> Open on
    let saved_page = home.as_ref().and_then(|h| h.data.state_json().get("startup_page").and_then(|v| v.as_str()).map(String::from));
    let start_page = match arg("--page").or(saved_page).as_deref() { Some("library") => Page::Library, Some("pick") => Page::Pick, _ => Page::Home };
    let mut home = home;
    // Prepare Home's covers before the window exists, at the size it will probably open at (last session's), so the page is ready when it appears.
    if let Some(hs) = &mut home {
        let saved = hs.data.state_json().get("zest_window").cloned();
        let num = |k: &str| saved.as_ref().and_then(|v| v.get(k)).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
        let (pw, ph) = if saved.as_ref().and_then(|v| v.get("maximized")).and_then(|v| v.as_bool()).unwrap_or(false) && num("mw") > 0 { (num("mw"), num("mh")) }
                       else if num("w") > 0 { (num("w"), num("h")) } else { (size[0] as i32, size[1] as i32) };
        for c in home::layout(&hs.data, pw, ph, &Default::default()).iter().flat_map(|b| b.cards.iter()) { hs.covers.request(c.appid, (c.w as f32 * 0.95).round() as u32); }
    }
    let open_view = args.iter().any(|a| a == "--view-open");
    if let (Some(hs), Some(m)) = (&mut home, arg("--pick-mode")) { hs.pick.set_mode(&m); }
    if let (Some(hs), Some(v)) = (&mut home, arg("--home-scroll").and_then(|v| v.parse::<f32>().ok())) { hs.home_scroll = v; hs.home_scroll_target = v; }
    if let (Some(hs), Some(n)) = (&mut home, first_note) { hs.toast(n); hs.populate_pending = true; }
    // every start adds the games bought since the last one, as PlayDate did once per session (it only adds what is missing; headless runs skip it)
    else if let Some(hs) = &mut home {
        if !no_sync() && pd_filter::account::load_account(&hs.data.root_dir()).map_or(false, |a| !a.steam_id.is_empty()) { hs.populate_pending = true; }
    }
    if args.iter().any(|a| a == "--auto-pick") { if let Some(hs) = &mut home { hs.pick.pick(&mut hs.data); } }
    let cw = if args.iter().any(|a| a == "--cw") { cw } else { home.as_ref().and_then(|h| h.data.state_json().get("card_height").or_else(|| h.data.state_json().get("zest_card_size"))).and_then(|v| v.as_f64()).map(|v| v as f32).unwrap_or(cw) };
    let theme = theme::Theme::load(&root);
    if let Some(h) = &home { ui_kit::set_scale(h.data.state_json().get("ui_scale").and_then(|v| v.as_u64()).unwrap_or(100) as u32); }
    ui_kit::set_from_theme(&theme);
    if let Some(h) = &mut home { h.accent = egui::Color32::from_rgb(theme.accent.0, theme.accent.1, theme.accent.2); }
    let bg = theme::Background::load(&root, &theme);
    // egui paints an orange "Unaligned" over widgets that are not pixel aligned in debug builds; Zest lays out in fractions of a pixel on purpose
    #[cfg(debug_assertions)]   // the field only exists in debug builds, which are also the only ones that draw the overlay
    ctx.all_styles_mut(|s| s.debug.show_unaligned = false);
    theme::install_fonts(&ctx, false);
    let now = Instant::now();
    let mut app = Own {
        window: None, surface: None, ctx, egui_winit: None,
        renderer: EguiSoftwareRender::new(ColorFieldOrder::Bgra), core, layer: Layer::new(),
        cw, lay: Lay { cols: 1, cw: 200, x_off: 0 }, theme, bg, page: start_page, want_fullscreen: false, shot: arg("--screenshot").map(|p| (PathBuf::from(p), 0)), shot_size: arg("--screenshot").map(|_| (size[0] as usize, size[1] as usize)), want_page: None, home, cjk_done: false, win_maximized: false, win_normal: (size[0] as u32, size[1] as u32), win_max: (0, 0), want_exit: false, menu_open: false, filter: None, bulk: None, pag: None, pag_open: false, santa: None, miam: None, santa_open: false, miam_open: false, blaeo: None, blaeo_parked: None, whats_new_checked: false, blaeo_open: false, sg_unmatched: None, sg_unmatched_open: false, pop_rx: None, pop_plan: None, pop_status: None, pag_autosave: false, crop: None, crop_arg: None, picker: None, bulk_open: false, bulk_tab_arg: None, filter_open: false, filter_demo: false, edit: None, edit_arg: None, menu_action: None, modal: None, lib_header: String::new(), lib_head: Default::default(), lib_full: None, search: String::new(), search_applied: String::new(), search_init: false, search_edit_at: None, dice: None, dice_pick: false, scroll_to: None, want_clear: false, want_show_all: false, groups: Vec::new(), collapsed: HashSet::new(), toggle: None, view_open: false, view_vals: ("name".into(), "ASC".into(), String::new()), view_stage: (String::new(), String::new(), String::new(), 200.0), view_changes: Vec::new(), tier_job: None, tier_pending: false, pack_job: None, packs_checked: false, settle_until: None, applied_cw: cw, root: root.clone(), lib_loaded: false, lib_warm: false, lib_grid: None, lib_focus_sig: ((usize::MAX, 0, 0), (0, 0)), lib_mouse_card: None, edit_corner: None, edit_rect: None, edit_click: None, list_mode: list_start, list_sel: None, list_saved: None, list_menu: None, list_center: true, list_cover: None, list_icons: Default::default(), list_status: Default::default(), list_edit: None, list_out: None, tip_pin: None, tip_target: None, tip_cache: None, tip_quals: None, tip_fields: None, tip_since: None, badges: Default::default(), badge_cfg: None, badge_tex: Default::default(), shot_select: arg("--select").and_then(|v| v.parse().ok()), select_mode: false, selected: HashSet::new(), sel_press: None, shot_wheel: arg("--wheel").and_then(|v| v.parse().ok()), outlines: Default::default(), shot_switch: arg("--switch"), shot_rclick: arg("--rclick").and_then(|v| { let mut it = v.split(','); Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?)) }), shot_hover: arg("--hover").and_then(|v| { let mut it = v.split(','); Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?)) }), rt: None, rlog: ResizeLog::default(), sim_w: None, y: 0.0, last_y: 0.0, size,
        auto: auto.then(|| Auto { pass: 0, started: now, y: 0.0, last: now, fresh: true, sharp: 0.0, any: 0.0, frames: 0 }),
        pass_limit: std::env::var("PASS_LIMIT").ok().and_then(|v| v.parse().ok()).unwrap_or(PASSES.len()),
        next_wake: None, keep_drawing: false, pad_move: None, pad_adjust: false, pad_wrap: None, pad_hover: None, pad_nav_rects: Vec::new(), pad_tab: None, pad_stepping: None, pad_popup: None, pad_popup_was_open: false, pad_popup_close: false, pad_typing: false, pad_good: None, pad_scroll: Default::default(), rs_db: None, rs_sort: String::new(), rs_preview: (Instant::now(), 0, String::new()), rs_since: None, rs_at: Instant::now(), rs_px: 0.0, rs_mult: 1.0, pad_queue: Default::default(), pad_cur: None, lib_zoom: Default::default(), lib_zoom_tex: Default::default(), lib_zoom_at: Instant::now(), lib_zoom_y: 0.0, pad_nav: None, pad_bar: None, pad_bar_items: Vec::new(), pad_menu: false, pad_dir: None, pad_show: false, pad_ring: None, status: None, status_clicked: false, selftest: None, healthy_marked: false, parts: Vec::new(), intervals: Vec::new(), last_frame: now, live: now,
    };
    if open_view { app.view_open = true; }
    if args.iter().any(|a| a == "--menu-open") { app.menu_open = true; app.pad_menu = true; app.pad_show = true; }   // test hook: as if opened with the pad
    if args.iter().any(|a| a == "--filters") { app.filter_open = true; app.filter_demo = args.iter().any(|a| a == "--filter-demo"); }
    if args.iter().any(|a| a == "--bulk") { app.bulk_open = true; }
    if args.iter().any(|a| a == "--pag") { app.pag_open = true; }
    if args.iter().any(|a| a == "--miam") { app.miam_open = true; }
    if args.iter().any(|a| a == "--santa") { app.santa_open = true; }
    if args.iter().any(|a| a == "--blaeo") { app.blaeo_open = true; }
    if args.iter().any(|a| a == "--pag-save") { app.pag_open = true; app.pag_autosave = true; }
    app.bulk_tab_arg = arg("--bulk-tab").and_then(|v| v.parse().ok());
    if args.iter().any(|a| a == "--selftest-clear") { app.selftest = Some((0, Instant::now())); }
    if let (Some(n), Some(h)) = (arg("--notice"), app.home.as_mut()) { h.toast(n); }   // for screenshots of the status line
    if args.iter().any(|a| a == "--duplicates") { app.menu_action = Some("duplicates"); }
    if let (Some(id), Some(k)) = (arg("--edit").and_then(|v| v.parse::<i64>().ok()), arg("--crop").and_then(|v| v.parse::<usize>().ok())) { app.crop_arg = Some((id, k)); }
    if let Some(id) = arg("--card-menu").and_then(|v| v.parse::<i64>().ok()) { if let Some(h) = app.home.as_mut() { h.menu = Some((id, egui::pos2(300.0, 260.0))); } app.pad_show = true; }   // test hook: the card context menu, as if opened by the pad
    if let Some(id) = arg("--edit").and_then(|v| v.parse::<i64>().ok()) { app.edit_arg = Some((id, arg("--edit-tab").as_deref() == Some("info"))); }
    if arg("--modal").as_deref() == Some("pagywosg") { app.pag_open = true; }   // test hook: the PAGYWOSG builder has no menu entry of its own
    if let Some(m) = arg("--modal") { app.menu_action = ["blacklist", "junk", "settings", "plugins", "data", "send_log", "add_game", "emulators", "hltb", "update", "tutorial", "playnite", "edit_home"].into_iter().find(|k| *k == m); }
    if resize_test { app.rt = Some(ResizeTest { start: Instant::now(), base: 0.0, frames: 0, blank: 0, nonsharp: 0, settled_at: None, prev: Vec::new(), regress: 0, w_min: u32::MAX, w_max: 0, real: !args.iter().any(|a| a == "--sim") }); }
    app
}

/// The pad thread: its presses wake the event loop through `proxy`.
/// `size` shrunk (keeping its shape) to at most 90% of `screen` in both directions; a size that fits is kept as it is.
fn fit_on_screen(size: (f64, f64), screen: (f64, f64)) -> (f64, f64) {
    if screen.0 <= 0.0 || screen.1 <= 0.0 { return size; }
    let k = (screen.0 * 0.9 / size.0).min(screen.1 * 0.9 / size.1).min(1.0);
    (size.0 * k, size.1 * k)
}

/// Does a window of `size` all but fill a screen of `screen` (both in physical pixels)? Then it is not a deliberate "normal" size.
fn fills_screen(size: (u32, u32), screen: (u32, u32)) -> bool { screen.0 > 0 && screen.1 > 0 && size.0 as u64 * 100 >= screen.0 as u64 * 95 && size.1 as u64 * 100 >= screen.1 as u64 * 80 }

/// A repaint asked for from another thread (a background lookup or job that just finished) wakes the window: the event loop sleeps until an event, and
/// egui alone has no way to wake it, so such a request did nothing until the mouse moved. Requests made while drawing are left as they were: each frame
/// already reads its own repaint delay, and waking for those too would change how often an idle window draws.
fn wake_on_repaint(ctx: &egui::Context, proxy: winit::event_loop::EventLoopProxy<()>) {
    let ui = std::thread::current().id();
    ctx.set_request_repaint_callback(move |_info| { if std::thread::current().id() != ui { let _ = proxy.send_event(()); } });
}

fn start_gamepad(app: &Own, proxy: winit::event_loop::EventLoopProxy<()>) {
    if let Some(st) = std::fs::read_to_string(app.root.join("state.json")).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()) { gamepad::set_sdl_mappings(&st); }
    gamepad::spawn(app.pad_queue.clone(), app.pad_scroll.clone(), move || { let _ = proxy.send_event(()); });
}

/// First run with no library: one window that starts as the setup questions and becomes the main window when they are answered (no second window opens).
struct Shell { setup: Option<setup_window::App>, own: Option<Own>, ctx: egui::Context, root: PathBuf, args: Vec<String>, proxy: winit::event_loop::EventLoopProxy<()>,
    /// The gamepad thread, started with the setup window so a controller works there too; the main window takes these over instead of starting a second one.
    pad: (gamepad::Queue, gamepad::Scroll) }
impl Shell {
    fn hand_over(&mut self) {
        let Some(mut s) = self.setup.take() else { return };
        let Some((window, surface, st)) = s.take_handles() else { std::process::exit(1) };
        let mut own = build_own(&self.root, &self.args, Some("Welcome to Zest. Library created; adding your games...".to_string()), self.ctx.clone());
        // the size a restored backup remembers (a created library has none, and keeps the setup window's size)
        if let Some(saved) = own.home.as_ref().and_then(|h| h.data.state_json().get("zest_window").cloned()) {
            let get = |k: &str| saved.get(k).and_then(|v| v.as_u64()).map(|v| v as f64);
            if saved.get("maximized").and_then(|v| v.as_bool()).unwrap_or(false) { window.set_maximized(true); }
            else if let (Some(w), Some(h)) = (get("w").filter(|v| *v >= 400.0), get("h").filter(|v| *v >= 300.0)) { let _ = window.request_inner_size(winit::dpi::LogicalSize::new(w, h)); }
        }
        own.window = Some(window.clone()); own.surface = Some(surface); own.egui_winit = Some(st);
        // as Own::resumed does: egui works in physical pixels, like the cards it draws over (without this a display scale other than 1, a Steam Deck in Game Mode, drew the
        // focus ring and other overlays at the wrong size and place until the next start)
        self.ctx.set_zoom_factor(1.0 / window.scale_factor() as f32);
        // a PlayDate import or a restored backup may have brought sign-ins in config.json: with no keyring, ask where they go
        if pd_filter::account::has_plaintext_sign_ins(&self.root) { pd_filter::secrets::request_choice(); }
        own.pad_queue = self.pad.0.clone(); own.pad_scroll = self.pad.1.clone();
        if let Some(st) = std::fs::read_to_string(own.root.join("state.json")).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()) { gamepad::apply_state(&st); gamepad::set_sdl_mappings(&st); gamepad::reload(); }   // an imported or restored library may bring its own controller mappings
        window.request_redraw();
        self.own = Some(own);
    }
}
impl ApplicationHandler for Shell {
    fn resumed(&mut self, el: &ActiveEventLoop) { if let Some(o) = self.own.as_mut() { o.resumed(el) } else if let Some(s) = self.setup.as_mut() { s.resumed(el) } }
    fn window_event(&mut self, el: &ActiveEventLoop, id: WindowId, ev: WindowEvent) {
        if let Some(o) = self.own.as_mut() { o.window_event(el, id, ev); return; }
        let Some(s) = self.setup.as_mut() else { return };
        s.window_event(el, id, ev);
        if s.finished { self.hand_over(); }
    }
    fn user_event(&mut self, el: &ActiveEventLoop, e: ()) { if let Some(o) = self.own.as_mut() { o.user_event(el, e) } else if let Some(s) = self.setup.as_ref() { s.wake() } }
    fn about_to_wait(&mut self, el: &ActiveEventLoop) { if let Some(o) = self.own.as_mut() { o.about_to_wait(el) } else if let Some(s) = self.setup.as_mut() { s.about_to_wait(el) } }
}

fn main() {
    // `zest --version`: the self-updater asks a downloaded program this before it swaps anything
    if std::env::args().nth(1).as_deref() == Some("--version") { println!("Zest {}", pd_filter::updater::VERSION); return; }
    // `zest --install-update <archive> <version>`: what the Update dialog does after a download, for testing the swap and the rollback without the dialog
    {
        let a: Vec<String> = std::env::args().collect();
        if a.get(1).map(String::as_str) == Some("--install-update") && a.len() >= 4 {
            let root = a.get(4).map(PathBuf::from).unwrap_or_else(default_data_dir);
            match pd_filter::selfupdate::install(&root, std::path::Path::new(&a[2]), &a[3]) { Ok(()) => { println!("swapped"); return; } Err(e) => { eprintln!("{e}"); std::process::exit(1); } }
        }
    }
    // `zest --zest-watch <exe> <old copy> <data folder> <old pid> <version>`: the update supervisor (pd_filter::selfupdate), started by the old program
    {
        let a: Vec<String> = std::env::args().collect();
        if a.get(1).map(String::as_str) == Some("--zest-watch") && a.len() >= 7 {
            let code = pd_filter::selfupdate::watch(std::path::Path::new(&a[2]), std::path::Path::new(&a[3]), std::path::Path::new(&a[4]), a[5].parse().unwrap_or(0), &a[6], std::time::Duration::from_secs(20), pd_filter::selfupdate::HANG_LIMIT);
            std::process::exit(code);
        }
    }
    // every decode thread otherwise gets its own 64 MB malloc pool that keeps what it freed; two pools cost a little contention and ~100 MB less
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe { libc::mallopt(libc::M_ARENA_MAX, 2); }
    start_idle_trimmer();
    #[cfg(feature = "dhat-heap")]
    { *PROFILER.lock().unwrap() = Some(dhat::Profiler::builder().file_name("dhat-heap.json").build()); }
    let args: Vec<String> = std::env::args().collect();
    // the first-run window runs as a child of the program itself (see setup_window.rs)
    if args.get(1).map(String::as_str) == Some("--setup") { let ok = setup_window::run(PathBuf::from(args.get(2).expect("usage: zest --setup <data folder>"))); std::process::exit(if ok { 0 } else { 1 }); }
    // the data folder: the first argument (development, tests), else ZEST_DATA, else the place a user install keeps it
    let root = match args.get(1).filter(|a| !a.starts_with("--")) { Some(a) => PathBuf::from(a), None => { let r = default_data_dir(); let _ = std::fs::create_dir_all(&r); r } };
    // the helper runs (cover packs rebuilt after an art change) and headless runs are not the program the user is using: they must not touch the keyring or the staged pictures
    let helper_run = args.iter().any(|a| a == "--screenshot" || a.starts_with("--selftest") || a == "--update-packs" || a == "--build-tier" || a == "--build-pack");
    // pictures staged by an Edit dialog never outlive the run (anything left is from a crash)
    if !helper_run { let _ = std::fs::remove_dir_all(artstore::staging_root(&root)); }
    // the app's own log lines go to <data folder>/zest.log (what Send Log to Developer sends), not only to the terminal
    if args.iter().all(|a| a != "--screenshot") { pd_filter::sendlog::install_file_logger(&root); }
    // secrets (API keys, sign-in tokens, cookies) live in the system keyring where there is one (pd_filter::secrets): a secret still in config.json is moved there now. Headless runs
    // (screenshots, self-tests) and ZEST_NO_KEYRING=1 leave config.json as it is, so test data never reaches the real keyring.
    if std::env::var_os("ZEST_NO_KEYRING").is_none() && !helper_run {
        // without a working keyring, sign-ins go to a file or stay in memory as the user chose (asked at the first sign-in; Settings > Account); typed API keys stay in config.json
        let mode = std::fs::read_to_string(root.join("state.json")).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|st| st.get("keyring_fallback").and_then(|v| v.as_str()).map(pd_filter::secrets::Fallback::from_key)).unwrap_or(pd_filter::secrets::Fallback::Ask);
        let v = std::sync::Arc::new(pd_filter::secrets::WithFallback::new(pd_filter::secrets::platform_vault(), root.join("zest_secrets.json"), mode));
        pd_filter::net::log(&if v.keyring_works() { "secrets: using the system keyring".to_string() } else { format!("secrets: no working keyring; sign-ins: {}", mode.key()) });
        pd_filter::secrets::install_with_fallback(v);
        pd_filter::account::migrate_secrets(&root);
        // sign-ins already in config.json (a PlayDate import, an earlier run) with no keyring: ask about them as about a new sign-in
        if pd_filter::account::has_plaintext_sign_ins(&root) { pd_filter::secrets::request_choice(); }
    }
    // one Zest per data folder; headless shots, self-tests and the cover-preview helpers (Zest starts itself with --update-packs / --build-tier, see grid-core) run beside the real one
    let _instance = if args.iter().any(|a| a == "--screenshot" || a.starts_with("--selftest") || a == "--update-packs" || a == "--build-tier" || a == "--build-pack") { None } else {
        match pd_filter::instance::acquire_waiting(&root, std::time::Duration::from_secs(8)) {
            Ok(i) => Some(i),
            Err(e) => {
                // started from a shortcut there is no terminal: say so in a dialog, or the window simply never appears
                eprintln!("{e}");
                // who started this second copy: the next report of the popup then says where it came from (parent process, its command line, the systemd unit)
                #[cfg(unix)]
                {
                    let cmd = |pid: &str| std::fs::read(format!("/proc/{pid}/cmdline")).map(|b| String::from_utf8_lossy(&b).replace('\0', " ").trim().to_string()).unwrap_or_default();
                    let ppid = std::fs::read_to_string("/proc/self/stat").ok().and_then(|t| t.rsplit(')').next().map(|r| r.split_whitespace().nth(1).unwrap_or("").to_string())).unwrap_or_default();
                    let unit = std::fs::read_to_string("/proc/self/cgroup").ok().and_then(|t| t.lines().last().map(|l| l.rsplit('/').next().unwrap_or("").to_string())).unwrap_or_default();
                    eprintln!("second launch: pid {}, parent {ppid} ({}), grandparent command: {}, unit {unit}, activation token set: {}", std::process::id(), cmd(&ppid),
                        std::fs::read_to_string(format!("/proc/{ppid}/stat")).ok().and_then(|t| t.rsplit(')').next().map(|r| r.split_whitespace().nth(1).unwrap_or("").to_string())).map(|g| cmd(&g)).unwrap_or_default(),
                        std::env::var_os("XDG_ACTIVATION_TOKEN").is_some() || std::env::var_os("DESKTOP_STARTUP_ID").is_some());
                }
                rfd::MessageDialog::new().set_level(rfd::MessageLevel::Warning).set_title("Zest").set_description(e.as_str()).show();
                std::process::exit(1);
            }
        }
    };
    // no library here: make one (PlayDate's schema) for the account config.json names, else the Steam account Steam's own files name, else the id given with --steam-id / ZEST_STEAM_ID
    let mut first_note: Option<String> = None;
    // the first-run questions are asked in the main window itself (see `Shell`), so nothing closes and reopens
    let mut need_setup = false;
    if pd_filter::setup::find_library(&root).is_none() {
        let flag = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
        let given = flag("--steam-id").or_else(|| std::env::var("ZEST_STEAM_ID").ok().filter(|v| !v.is_empty())).map(|id| (id, flag("--steam-key").or_else(|| std::env::var("ZEST_STEAM_KEY").ok().filter(|v| !v.is_empty()))));
        // a person at the keyboard with nothing configured gets the setup window (sign in with Steam, detect the account Steam has here, or type the ID and key) rather than a silent pick
        let headless_run = args.iter().any(|a| a == "--screenshot" || a.starts_with("--selftest") || a == "--update-packs" || a == "--build-tier" || a == "--build-pack");
        let unconfigured = pd_filter::account::load_account(&root).map_or(true, |a| a.steam_id.trim().is_empty());
        let asked = if !headless_run && given.is_none() && unconfigured {
            need_setup = true;
            true
        } else { false };
        if !asked {
        match pd_filter::setup::first_run(&root, given, pd_filter::steamlocal::steam_root(None).as_deref()) {
            Ok(m) => {
                first_note = Some(if m.detected { format!("Welcome to Zest. Library set up for Steam account {}; adding your games...", if m.label.is_empty() { m.steam_id.clone() } else { m.label.clone() }) } else { "Welcome to Zest. Library created; adding your games...".to_string() });
                eprintln!("first run: library {} made", m.db.display());
            }
            Err(msg) => {
                eprintln!("{msg}");
                let headless = args.iter().any(|a| a == "--screenshot" || a.starts_with("--selftest") || a == "--update-packs" || a == "--build-tier" || a == "--build-pack");
                // a person at the keyboard: ask in a window, then carry on with what it made
                if !headless { need_setup = true; } else {
                if !headless {
                    rfd::MessageDialog::new().set_level(rfd::MessageLevel::Info).set_title("Zest").set_description(format!("Zest found no game library in\n{}\n\n{msg}", root.display()).as_str()).show();
                }
                std::process::exit(1);
                }
            }
        }
        }
    }
    if need_setup {
        let event_loop = EventLoop::new().expect("event loop");
        let ctx = egui::Context::default();
        let proxy = event_loop.create_proxy();
        wake_on_repaint(&ctx, proxy.clone());
        let pad: (gamepad::Queue, gamepad::Scroll) = Default::default();
        { let p = proxy.clone(); gamepad::spawn(pad.0.clone(), pad.1.clone(), move || { let _ = p.send_event(()); }); }
        let mut setup = setup_window::App::new(root.clone(), ctx.clone()); setup.pad = Some(pad.0.clone()); setup.pad_scroll = Some(pad.1.clone());
        let mut shell = Shell { setup: Some(setup), own: None, ctx, root: root.clone(), args: args.clone(), proxy, pad };
        event_loop.run_app(&mut shell).expect("run");
        return;
    }
    if grid_core::run_builder_mode(&root) { return; }
    let mut app = build_own(&root, &args, first_note, egui::Context::default());
    let arg = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let event_loop = EventLoop::new().expect("event loop");
    // test hook for screenshots: --pad udlra holds those presses (up, down, left, right, A) until a dialog is open, then pushes them as if from a pad
    if let Some(script) = arg("--script") { let acts: Vec<String> = script.split(';').map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).collect(); SHOT_PAD_N.fetch_add(acts.len() * 2, std::sync::atomic::Ordering::Relaxed); *SHOT_SCRIPT.lock().unwrap() = acts; }
    if let Some(keys) = arg("--pad") { SHOT_PAD_N.store(keys.chars().count(), std::sync::atomic::Ordering::Relaxed); *SHOT_PAD.lock().unwrap() = keys; }
    let headless_run = args.iter().any(|a| a == "--screenshot" || a == "--auto" || a.starts_with("--selftest"));
    if !headless_run {
        let proxy = event_loop.create_proxy();
        wake_on_repaint(&app.ctx, proxy.clone());
        start_gamepad(&app, proxy);
    }
    event_loop.run_app(&mut app).expect("run");
}

#[cfg(test)]
mod window_size_tests {
    #[test] fn a_size_that_fills_the_screen_is_not_a_normal_size() {
        assert!(super::fills_screen((2592, 1348), (2560, 1600)), "the Mac's saved zoom size");
        assert!(super::fills_screen((2560, 1348), (2560, 1600)));
        assert!(!super::fills_screen((1600, 1000), (2560, 1600)));
        assert!(!super::fills_screen((2560, 900), (2560, 1600)), "a full-width but short window is a choice");
        assert!(!super::fills_screen((800, 600), (0, 0)));
        // the default 1300 x 900 points on a 1280 x 800 Retina screen (2600 x 1800 pixels on 2560 x 1600): shrunk to fit, same shape
        let (w, h) = super::fit_on_screen((2600.0, 1800.0), (2560.0, 1600.0));
        assert!(w <= 2304.0 + 0.5 && h <= 1440.0 + 0.5 && ((w / h) - 2600.0 / 1800.0).abs() < 1e-6);
        assert_eq!(super::fit_on_screen((1600.0, 1000.0), (2560.0, 1600.0)), (1600.0, 1000.0), "a size that fits is kept");
    }
}

#[cfg(test)]
mod dice_tests {
    use super::dice_alpha;
    #[test]
    fn the_pulse_matches_the_web_keyframes() {
        let a = |t| dice_alpha(t).unwrap();
        assert!((a(0.0) - 1.0).abs() < 1e-4);
        assert!(a(0.69) < 0.01);
        assert!((a(0.7) - 1.0).abs() < 1e-4);
        assert!((a(3.5) - 1.0).abs() < 1e-4);
        assert!(a(6.99) < 0.01);
        assert!(dice_alpha(7.0).is_none());
    }
}

/// A badge background color is `#rrggbb`; anything else is ignored (the web writes it into a style attribute).
fn h_ok(c: Option<&str>) -> bool { c.map_or(false, |c| c.len() == 7 && c.starts_with('#') && c[1..].chars().all(|h| h.is_ascii_hexdigit())) }

#[cfg(test)]
mod scroll_preview_tests {
    use super::*;
    #[test]
    fn the_preview_says_what_the_sort_is_sorting_by() {
        assert_eq!(scroll_preview_text(None, "name", 1, "  zelda"), "Z");
        assert_eq!(scroll_preview_text(None, "name", 1, "!Hidden"), "#");
        assert_eq!(scroll_preview_value("release_date", &[1_700_000_000.0]), "Nov 2023");
        assert_eq!(scroll_preview_value("last_played", &[0.0]), "\u{2014}");
        assert_eq!(scroll_preview_value("weighted_percentage", &[87.4]), "87%");
        assert_eq!(scroll_preview_value("hltb_max", &[0.0, 120.0, 600.0]), home::fmt_hours(600.0));
        assert_eq!(scroll_preview_value("hltb_min", &[0.0, 120.0, 600.0]), home::fmt_hours(120.0));
        assert_eq!(scroll_preview_value("achievement_percent", &[5.0, 20.0]), "25%");
        assert_eq!(scroll_preview_value("achievement_remaining", &[5.0, 20.0]), "15 left");
        assert_eq!(scroll_preview_value("achievement_percent", &[0.0, 0.0]), "\u{2014}");
        assert_eq!(scroll_preview_value("total_reviews", &[12500.0]), "12.5k");
    }
}

#[cfg(test)]
mod tip_place_tests {
    use super::*;
    fn r(x: f32, y: f32, w: f32, h: f32) -> egui::Rect { egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, h)) }
    #[test]
    fn a_tall_tooltip_goes_beside_a_card_that_has_no_room_above_or_below() {
        let screen = r(0.0, 0.0, 1300.0, 800.0);
        let card = r(500.0, 100.0, 200.0, 600.0);   // nearly the whole height: no room above or below for a 400 px tip
        let size = egui::vec2(330.0, 400.0);
        let p = tip_place(card, size, screen);
        assert!(!r(p.x, p.y, size.x, size.y).intersects(card), "{p:?}");
        assert!(screen.contains_rect(r(p.x, p.y, size.x, size.y)));
    }
    #[test]
    fn usually_below_for_a_card_in_the_top_half_and_above_for_the_bottom_half() {
        let screen = r(0.0, 0.0, 1300.0, 800.0);
        let size = egui::vec2(300.0, 150.0);
        let top = r(100.0, 100.0, 120.0, 180.0);
        assert!(tip_place(top, size, screen).y > top.max.y);
        let bottom = r(100.0, 500.0, 120.0, 180.0);
        assert!(tip_place(bottom, size, screen).y + size.y < bottom.min.y);
    }
    #[test]
    fn it_never_leaves_the_screen_even_when_it_must_overlap() {
        let screen = r(0.0, 0.0, 400.0, 300.0);
        let card = r(0.0, 0.0, 400.0, 300.0);   // the card is the whole screen
        let size = egui::vec2(330.0, 200.0);
        let p = tip_place(card, size, screen);
        assert!(screen.contains_rect(r(p.x, p.y, size.x, size.y)));
    }
}

#[cfg(test)]
mod paint_op_tests {
    use super::*;
    fn cover(w: usize, h: usize) -> Arc<Pixels> { Arc::new(Pixels { w, h, px: (0..w * h).map(|i| (i as u32).wrapping_mul(2654435761u32) & 0x00ff_ffff | 0x0001_0101).collect() }) }
    /// A card that starts left of the layer is painted for the part inside it, the same pixels it would have had if the layer were wider (it used to be skipped whole).
    #[test]
    fn a_card_starting_left_of_the_layer_still_paints_its_visible_part() {
        for (cw, ch, ow, oh) in [(8usize, 6usize, 8i64, 6i64), (8, 6, 13, 9), (30, 45, 12, 18)] {
            let img = cover(cw, ch);
            let (stride, rows) = (40usize, oh as usize + 2);
            // the reference: the whole card, 10 pixels in
            let mut wide = vec![0u32; stride * rows];
            paint_op(&mut wide, 0, stride, &Op { x: 10, y: 1, w: ow, h: oh, img: Some(img.clone()) });
            for cut in [0i64, 1, 3, ow - 1] {
                let mut part = vec![0u32; stride * rows];
                paint_op(&mut part, 0, stride, &Op { x: -cut, y: 1, w: ow, h: oh, img: Some(img.clone()) });
                for r in 1..1 + oh as usize { for c in 0..(ow - cut) as usize { assert_eq!(part[r * stride + c], wide[r * stride + 10 + cut as usize + c], "{cw}x{ch} as {ow}x{oh}, cut {cut}, row {r}, col {c}"); } }
                // and nothing is painted left of the card's visible part
                assert!(part[stride + (ow - cut) as usize..2 * stride].iter().all(|p| *p == 0));
            }
        }
        // wholly outside: nothing
        let mut none = vec![0u32; 40 * 8];
        paint_op(&mut none, 0, 40, &Op { x: -8, y: 1, w: 8, h: 6, img: Some(cover(8, 6)) });
        assert!(none.iter().all(|p| *p == 0));
    }
}
