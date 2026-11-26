//! The Pick 6 page: controls (egui), then six result cards whose covers are painted by our own renderer.
//! Layout rules follow `templates/pick.html` (controls column max 68.75rem with clamp(1rem, 5vw, 8rem) side padding,
//! cards sized from the width at 2:3 and clamped to the height left under the controls).

use crate::home::{self, GameInfo, HomeData};
use crate::theme::Theme;
use egui::{Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, Ui, Vec2};
use pd_filter::pick::PickParams;
use std::collections::{BTreeMap, HashMap, HashSet};

const STATUSES: [&str; 5] = ["Never Played", "Unfinished", "Beaten", "Completed", "Won't Play"];
const KEYS: [&str; 5] = ["tags", "review", "staleness", "recency", "hltb"];
const NAMES: [&str; 5] = ["Tag Similarity", "Review Score", "Staleness", "Release Date", "Length (HLTB)"];
#[allow(non_snake_case)] fn PANEL() -> Color32 { crate::ui_kit::PANEL() }
#[allow(non_snake_case)] fn EDGE() -> Color32 { crate::ui_kit::RULE() }
#[allow(non_snake_case)] fn MUTED() -> Color32 { crate::ui_kit::MUTED() }
#[allow(non_snake_case)] fn TEXT() -> Color32 { crate::ui_kit::TEXT() }
#[allow(non_snake_case)] fn ON_ACCENT() -> Color32 { crate::ui_kit::ON_ACCENT() }

#[derive(Clone, Copy, PartialEq)]
enum Mode { Random, Smart, Weighted }

pub struct Card { pub appid: i64, pub reason: Option<String>, pub relaxed: bool }

pub struct PickState {
    mode: Mode,
    w: [i32; 5],
    bound: [String; 4],   // review, staleness, recency, hltb
    use_filtered: bool,
    statuses: [bool; 5],
    weights_open: bool,
    pub results: Vec<Card>,
    pool: usize,
    relaxed: bool,
    error: Option<String>,
    counts: Option<(BTreeMap<String, i64>, Option<BTreeMap<String, i64>>)>,
    filter_name: String,
    has_filters: bool,
    /// Where the cards were drawn last frame: the whole card (for clicks) and its art (for the covers).
    pub rects: Vec<(i64, Rect)>,
    /// The page's controls, in the order they are drawn: where the gamepad cursor can stop besides the cards.
    pub pad_items: Vec<Rect>,
    pub art: Vec<(i64, Rect)>,
    pub art_w: u32,
    pub toast: Option<String>,
    pub want_filters: bool,   // the filter link was clicked: the app opens the Filters dialog
    /// The six cards stay hidden after a pick until their covers are decoded (or a short timeout), so no card ever appears as an empty box.
    revealed: bool,
    /// A re-roll waits here, with its covers decoding, while the previous six stay on screen; it replaces them in one step.
    pending: Option<(Vec<Card>, usize, bool, std::time::Instant)>,
    hold_until: Option<std::time::Instant>,
    wants_redraw: bool,
}

impl PickState {
    pub fn new(state: &serde_json::Value) -> PickState {
        let saved: Option<Vec<String>> = state.get("pick6_statuses").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect());
        let statuses = match saved.filter(|s| !s.is_empty()) {
            Some(s) => STATUSES.map(|n| s.iter().any(|x| x == n)),
            None => [true, true, true, true, false],
        };
        let ft = state.get("filter_tree");
        let filter_name = ft.and_then(|f| f.get("saved_filter")).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let has_filters = ft.map_or(false, |f| f.get("saved_filter").is_some() || f.get("custom_sql").and_then(|v| v.as_str()).map_or(false, |s| !s.is_empty())
            || f.get("items").and_then(|v| v.as_array()).map_or(false, |a| !a.is_empty()));
        PickState { mode: Mode::Random, w: [65, 35, 0, 0, 0], bound: ["70".into(), "365".into(), "2020".into(), "3".into()], use_filtered: has_filters, statuses, weights_open: true,
            results: Vec::new(), pool: 0, relaxed: false, error: None, counts: None, filter_name, has_filters, rects: Vec::new(), pad_items: Vec::new(), art: Vec::new(), art_w: 0, toast: None, want_filters: false, revealed: true, pending: None, hold_until: None, wants_redraw: false }
    }

    fn active_statuses(&self) -> Vec<String> { STATUSES.iter().zip(self.statuses).filter(|(_, on)| *on).map(|(n, _)| n.to_string()).collect() }

    fn refresh_counts(&mut self, data: &HomeData) {
        let lib = data.pick_status_counts(false);
        let fil = if self.use_filtered { data.pick_status_counts(true) } else { None };
        self.counts = lib.map(|l| (l, fil));
        self.apply_availability();
    }

    fn unavailable(&self, name: &str) -> bool {
        let Some((lib, fil)) = &self.counts else { return false };
        lib.get(name).copied().unwrap_or(0) == 0 || (self.use_filtered && fil.as_ref().map_or(false, |f| f.get(name).copied().unwrap_or(0) == 0))
    }

    /// Greyed-out statuses can't be on (not saved, so a passing filter never overwrites the user's choice).
    fn apply_availability(&mut self) {
        for (i, n) in STATUSES.iter().enumerate() { if self.unavailable(n) { self.statuses[i] = false; } }
        if !self.statuses.iter().any(|s| *s) { if let Some(i) = STATUSES.iter().position(|n| !self.unavailable(n)) { self.statuses[i] = true; } }
    }

    /// True once after the cards were revealed, so the caller draws one more frame (the chrome appears with the covers).
    pub fn is_revealed(&self) -> bool { (self.revealed && self.pending.is_none()) || self.art.is_empty() }
    pub fn take_redraw(&mut self) -> bool { std::mem::take(&mut self.wants_redraw) }

    pub fn take_filters_request(&mut self) -> bool { std::mem::take(&mut self.want_filters) }

    /// The filter changed (the Filters dialog): the link under the pool toggle and the pool setting follow it.
    pub fn refresh_filter(&mut self, state: &serde_json::Value) {
        let ft = state.get("filter_tree");
        self.filter_name = ft.and_then(|f| f.get("saved_filter")).and_then(|v| v.as_str()).unwrap_or("").to_string();
        self.has_filters = ft.map_or(false, |f| f.get("saved_filter").is_some() || f.get("custom_sql").and_then(|v| v.as_str()).map_or(false, |s| !s.is_empty())
            || f.get("items").and_then(|v| v.as_array()).map_or(false, |a| !a.is_empty()));
        self.use_filtered = self.has_filters;
        self.counts = None;
    }

    pub fn set_mode(&mut self, m: &str) { self.mode = match m { "smart" => Mode::Smart, "weighted" => Mode::Weighted, _ => Mode::Random }; self.weights_open = true; }

    pub fn pick(&mut self, data: &mut HomeData) {
        self.error = None;
        let bound = |i: usize, key: usize| -> Option<f64> {
            if self.mode != Mode::Weighted || self.w[key] == 0 { return None; }
            self.bound[i].trim().parse::<f64>().ok()
        };
        let mut p = PickParams {
            mode: match self.mode { Mode::Random => "random", Mode::Smart => "smart", Mode::Weighted => "weighted" }.to_string(),
            w_tags: self.w[0] as f64, w_review: self.w[1] as f64, w_staleness: self.w[2] as f64, w_recency: self.w[3] as f64, w_hltb: self.w[4] as f64,
            b_review: bound(0, 1), b_staleness: bound(1, 2), b_recency: bound(2, 3), b_hltb: bound(3, 4),
        };
        if self.mode != Mode::Weighted { (p.w_tags, p.w_review, p.w_staleness, p.w_recency, p.w_hltb) = (65.0, 35.0, 0.0, 0.0, 0.0); }
        let statuses = self.active_statuses();
        match data.run_pick(&p, self.use_filtered, &statuses) {
            Ok(res) => {
                let cards: Vec<Card> = res.picks.iter().map(|k| Card { appid: k.appid, reason: k.reason.clone(), relaxed: k.bounds_relaxed }).collect();
                if self.mode == Mode::Weighted { self.weights_open = false; }
                if self.results.is_empty() || !self.revealed {
                    // the first pick: show nothing until its covers are ready
                    self.results = cards;
                    self.pool = res.bounded_pool_size;
                    self.relaxed = res.bounds_relaxed;
                    self.revealed = false;
                    self.hold_until = Some(std::time::Instant::now() + std::time::Duration::from_millis(700));
                } else {
                    // a re-roll: the cards on screen stay until the new ones are ready
                    self.pending = Some((cards, res.bounded_pool_size, res.bounds_relaxed, std::time::Instant::now()));
                }
            }
            Err(e) => { self.error = Some(e); self.results.clear(); }
        }
    }
}

fn slider_desc(key: &str, v: i32) -> String {
    if v == 0 { return "No preference".into(); }
    let a = v.abs();
    let level = if a <= 33 { "Slight" } else if a <= 66 { "Moderate" } else if a <= 89 { "Strong" } else { "Very strong" };
    let pref = format!("{level} preference for ");
    match key {
        "tags" => if v > 0 { format!("{level} match to your taste profile") } else { format!("{level} preference for games outside your taste profile") },
        "review" => pref + if v > 0 { "highly-reviewed games" } else { "low-reviewed / hidden gems" },
        "staleness" => pref + if v > 0 { "long-untouched games" } else { "recently played games" },
        "recency" => pref + if v > 0 { "newer releases" } else { "older releases" },
        _ => pref + if v > 0 { "longer games" } else { "shorter games" },
    }
}

/// The page's pill button, painted by hand so hovering never changes its size (egui's own buttons grow on hover).
fn accent_button(ui: &mut Ui, text: &str, on: bool, accent: Color32, enabled: bool, size: f32) -> egui::Response {
    let (padx, pady) = (size * 1.15, size * 0.45);
    let galley = ui.painter().layout_no_wrap(text.to_string(), crate::theme::bold(size), Color32::PLACEHOLDER);
    let (rect, resp) = ui.allocate_exact_size(galley.size() + Vec2::new(2.0 * padx, 2.0 * pady), if enabled { Sense::click() } else { Sense::hover() });
    let hot = resp.hovered() && enabled && !on;
    let (fill, ink, edge) = if on { (accent, ON_ACCENT(), accent) }
        else if hot { (Color32::from_rgb(0x24, 0x34, 0x47), TEXT(), crate::ui_kit::EDGE()) }
        else { (Color32::TRANSPARENT, if enabled { MUTED() } else { Color32::from_rgba_unmultiplied(0x8f, 0x98, 0xa0, 90) }, crate::ui_kit::EDGE()) };
    ui.painter().rect_filled(rect, 5, fill);
    ui.painter().rect_stroke(rect, 5, Stroke::new(1.0_f32, edge), egui::StrokeKind::Inside);
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, ink);
    if resp.hovered() && enabled { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
    resp
}

/// A -100..100 slider whose track fills from the middle to the value, as the page's does.
fn zero_slider(ui: &mut Ui, v: &mut i32, width: f32, accent: Color32) -> egui::Response {
    let (rect, mut resp) = ui.allocate_exact_size(Vec2::new(width, 16.0), Sense::click_and_drag());
    let (pad, cy) = (7.0, rect.center().y);
    let (x0, x1) = (rect.min.x + pad, rect.max.x - pad);
    if let (true, Some(p)) = (resp.dragged() || resp.clicked(), resp.interact_pointer_pos()) {
        let nv = ((p.x - x0) / (x1 - x0) * 200.0 - 100.0).round().clamp(-100.0, 100.0) as i32;
        if nv != *v { *v = nv; resp.mark_changed(); }
    }
    let at = |val: i32| x0 + (val as f32 + 100.0) / 200.0 * (x1 - x0);
    let track = Rect::from_min_max(Pos2::new(x0, cy - 2.0), Pos2::new(x1, cy + 2.0));
    ui.painter().rect_filled(track, 2, Color32::from_rgb(0x2a, 0x3f, 0x55));
    let (a, b) = (at(0).min(at(*v)), at(0).max(at(*v)));
    if b > a { ui.painter().rect_filled(Rect::from_min_max(Pos2::new(a, cy - 2.0), Pos2::new(b, cy + 2.0)), 2, accent); }
    ui.painter().circle_filled(Pos2::new(at(*v), cy), 7.0, accent);
    if resp.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
    resp
}

/// The page's toggle switch.
fn toggle_switch(ui: &mut Ui, on: &mut bool, label: &str, accent: Color32, rem: f32) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(label.to_string(), FontId::proportional(0.88 * rem), TEXT());
    let (tw, th) = (2.375 * rem, 1.3125 * rem);
    let (rect, mut resp) = ui.allocate_exact_size(Vec2::new(tw + 0.625 * rem + galley.size().x, th.max(galley.size().y)), Sense::click());
    if resp.clicked() { *on = !*on; resp.mark_changed(); }
    let track = Rect::from_min_size(Pos2::new(rect.min.x, rect.center().y - th / 2.0), Vec2::new(tw, th));
    ui.painter().rect_filled(track, (th / 2.0) as u8, if *on { accent } else { Color32::from_rgb(0x2a, 0x3f, 0x55) });
    let k = 0.9375 * rem;
    let kx = track.min.x + 0.1875 * rem + if *on { 1.0625 * rem } else { 0.0 };
    ui.painter().circle_filled(Pos2::new(kx + k / 2.0, track.center().y), k / 2.0, if *on { ON_ACCENT() } else { MUTED() });
    ui.painter().galley(Pos2::new(track.max.x + 0.625 * rem, rect.center().y - galley.size().y / 2.0), galley, TEXT());
    if resp.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
    resp
}

/// The whole page's egui part. Fills `ps.rects` / `ps.art` for the painter and the click engine.
pub fn show(ui: &mut Ui, ps: &mut PickState, data: &mut HomeData, t: &Theme, hover: &HashMap<i64, f32>, missing: &HashSet<i64>) {
    let screen = ui.ctx().content_rect();
    let (w, h) = (screen.width(), screen.height());
    let rem = (w * 0.00625).clamp(11.0, 20.0);
    let accent = Color32::from_rgb(t.accent.0, t.accent.1, t.accent.2);
    if ps.counts.is_none() { ps.refresh_counts(data); }
    ps.pad_items.clear();

    let pad = (w * 0.05).clamp(rem, 8.0 * rem);
    let col_w = (68.75 * rem).min(w) - 2.0 * pad;
    let x0 = (w - (68.75 * rem).min(w)) / 2.0 + pad;
    let top = home::bar_height(h as i32) as f32 + 0.015 * h + 0.06 * h * 0.0;
    let mut col = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(Pos2::new(x0, top), Vec2::new(col_w, h))).layout(egui::Layout::top_down(egui::Align::Min)));
    col.set_width(col_w);
    let bsz = 0.83 * rem;

    egui::Frame::new().fill(PANEL()).stroke(Stroke::new(1.0_f32, EDGE())).corner_radius((0.625 * rem) as u8).inner_margin(egui::Margin::symmetric((1.5 * rem) as i8, rem as i8)).show(&mut col, |ui| {
        ui.visuals_mut().extreme_bg_color = Color32::from_rgb(0x11, 0x1c, 0x29);
        { let wd = &mut ui.visuals_mut().widgets; for w in [&mut wd.inactive, &mut wd.hovered, &mut wd.active] { w.bg_stroke = Stroke::new(1.0_f32, EDGE()); w.expansion = 0.0; } }
        ui.set_width(col_w - 3.0 * rem - 2.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("PICK 6").size((w * 0.025).clamp(1.4 * rem, 2.0 * rem)).strong().color(crate::ui_kit::heading()));
            ui.label(RichText::new("Let Zest choose your next six games.").size(0.9 * rem).color(MUTED()));
        });
        ui.add_space(0.4 * rem);
        ui.separator();
        ui.add_space(0.4 * rem);

        // mode
        ui.horizontal_wrapped(|ui| {
            ui.add_sized([6.875 * rem, 0.0], egui::Label::new(RichText::new("MODE").size(0.82 * rem).color(MUTED())));
            for (m, label) in [(Mode::Random, "RANDOM"), (Mode::Smart, "SMART"), (Mode::Weighted, "WEIGHTED")] {
                let b = accent_button(ui, label, ps.mode == m, accent, true, bsz); ps.pad_items.push(b.rect);
                if b.clicked() { ps.mode = m; ps.results.clear(); ps.pending = None; ps.revealed = true; ps.relaxed = false; if m == Mode::Weighted { ps.weights_open = true; } }
            }
        });
        let desc = match ps.mode {
            Mode::Random => "Picks any game with equal probability.",
            Mode::Smart => "Uses your completion history and tags to surface games you'll likely enjoy.",
            Mode::Weighted => "You control how much each factor influences the picks.",
        };
        ui.horizontal(|ui| {
            ui.add_space(6.875 * rem + ui.spacing().item_spacing.x);
            ui.label(RichText::new(desc).size(0.8 * rem).italics().color(MUTED()));
            if ps.mode == Mode::Smart { ui.label(RichText::new("Auto-filter: review \u{2265} 70%").size(0.78 * rem).italics().color(MUTED())); }
        });

        if ps.mode == Mode::Weighted {
            ui.add_space(0.5 * rem);
            egui::Frame::new().fill(Color32::from_black_alpha(51)).stroke(Stroke::new(1.0_f32, EDGE())).corner_radius((0.5 * rem) as u8).inner_margin(egui::Margin::symmetric(rem as i8, (0.4 * rem) as i8)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                let head = ui.add(egui::Label::new(RichText::new(format!("WEIGHTS  {}", if ps.weights_open { "\u{25BE}" } else { "\u{25B8}" })).size(0.78 * rem).strong().color(MUTED())).sense(Sense::click()));
                ps.pad_items.push(head.rect);
                if head.clicked() { ps.weights_open = !ps.weights_open; }
                if ps.weights_open {
                    for i in 0..5 {
                        ui.horizontal(|ui| {
                            ui.allocate_ui(Vec2::new(10.0 * rem, 2.2 * rem), |ui| {
                                ui.set_width(10.0 * rem);
                                ui.vertical(|ui| {
                                    ui.set_width(10.0 * rem);
                                    ui.label(RichText::new(NAMES[i]).size(0.82 * rem).color(TEXT()));
                                    ui.label(RichText::new(slider_desc(KEYS[i], ps.w[i])).size(0.7 * rem).color(MUTED()));
                                });
                            });
                            let r = zero_slider(ui, &mut ps.w[i], (ui.available_width() - 4.0 * rem).max(60.0), accent);
                            if r.changed() && ps.w[i] != 0 && ps.w[i].abs() <= 5 { ps.w[i] = 0; }
                            ui.label(RichText::new(ps.w[i].to_string()).size(0.82 * rem).monospace().strong().color(accent));
                        });
                        if i >= 1 && ps.w[i] != 0 {
                            let (units, hint) = [("%", "0-100"), ("days", "days"), ("", "year"), ("hrs", "hours")][i - 1];
                            ui.horizontal(|ui| {
                                ui.add_space(10.9 * rem);
                                ui.label(RichText::new(if ps.w[i] > 0 { "MIN" } else { "MAX" }).size(0.72 * rem).strong().color(MUTED()));
                                ui.add(egui::TextEdit::singleline(&mut ps.bound[i - 1]).desired_width(4.5 * rem).char_limit(6).hint_text(hint).font(FontId::monospace(0.82 * rem)).text_color(accent));
                                // numbers only (digits and one decimal point)
                                let mut dot = false;
                                ps.bound[i - 1].retain(|c| if c.is_ascii_digit() { true } else if c == '.' && !dot { dot = true; true } else { false });
                                ui.label(RichText::new(units).size(0.78 * rem).color(MUTED()));
                            });
                        }
                    }
                }
            });
        }

        // pool
        ui.add_space(0.8 * rem);
        ui.horizontal_wrapped(|ui| {
            ui.add_sized([6.875 * rem, 0.0], egui::Label::new(RichText::new("POOL").size(0.82 * rem).color(MUTED())));
            let before = ps.use_filtered;
            let tg = toggle_switch(ui, &mut ps.use_filtered, "Use currently filtered games only", accent, rem); ps.pad_items.push(tg.rect);
            if before != ps.use_filtered { ps.refresh_counts(data); }
            let label = if !ps.filter_name.is_empty() { format!("{} \u{2192}", ps.filter_name) } else if ps.has_filters { "Active filter \u{2192}".into() } else { "No filter \u{2192}".into() };
            let link = ui.add(egui::Label::new(RichText::new(label).size(0.75 * rem).color(if ps.has_filters { accent } else { MUTED() })).sense(Sense::click()));
            ps.pad_items.push(link.rect);
            if link.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
            if link.clicked() { ps.want_filters = true; }
        });

        // status
        ui.add_space(0.4 * rem);
        ui.horizontal_wrapped(|ui| {
            ui.add_sized([6.875 * rem, 0.0], egui::Label::new(RichText::new("STATUS").size(0.82 * rem).color(MUTED())));
            for (i, n) in STATUSES.iter().enumerate() {
                let unavailable = ps.unavailable(n);
                let r = accent_button(ui, n, ps.statuses[i], accent, !unavailable, 0.78 * rem);
                ps.pad_items.push(r.rect);   // greyed ones too: the cursor stops on them (a click does nothing), as the web's gamepad focus did
                if unavailable { r.clone().on_hover_text(if ps.counts.as_ref().map_or(false, |c| c.0.get(*n).copied().unwrap_or(0) == 0) { "No games in your library have this status." } else { "Excluded by the active filter." }); }
                if r.clicked() && !unavailable {
                    ps.statuses[i] = !ps.statuses[i];
                    if !ps.statuses.iter().any(|s| *s) { ps.statuses[i] = true; }
                    let list = ps.active_statuses();
                    data.save_pref("pick6_statuses", serde_json::json!(list));
                }
            }
        });
    });

    col.add_space(0.75 * rem);
    let (r, resp) = col.allocate_exact_size(Vec2::new(col_w, 2.6 * rem), Sense::click());
    ps.pad_items.push(r);
    col.painter().rect_filled(r, (0.5 * rem) as u8, if resp.hovered() { accent.gamma_multiply(0.88) } else { accent });
    col.painter().text(r.center(), Align2::CENTER_CENTER, "PICK 6", FontId::proportional(1.05 * rem), ON_ACCENT());
    if resp.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
    if resp.clicked() { ps.pick(data); }
    if let Some(e) = &ps.error {
        col.add_space(0.6 * rem);
        egui::Frame::new().fill(Color32::from_rgb(0x3d, 0x1a, 0x1a)).stroke(Stroke::new(1.0_f32, crate::ui_kit::DANGER())).corner_radius((0.5 * rem) as u8).inner_margin(egui::Margin::symmetric((0.9 * rem) as i8, (0.6 * rem) as i8))
            .show(&mut col, |ui| { ui.set_width(col_w - 2.0 * rem); ui.label(RichText::new(format!("\u{2718} {e}")).size(0.85 * rem).color(crate::ui_kit::text_danger())); });
    }
    if !ps.results.is_empty() {
        let all_relaxed = ps.results.iter().all(|c| c.relaxed);
        let msg = if ps.relaxed { Some((if all_relaxed { "No games matched your bounds." } else { "Some picks fell outside your bounds." }, all_relaxed)) }
                  else if ps.pool < 12 { None } else { None };
        let small = (!ps.relaxed && ps.pool < 12).then(|| format!("Small pool: only {} game{} available.", ps.pool, if ps.pool != 1 { "s" } else { "" }));
        let (txt, bad) = match (msg, small) { (Some((m, b)), _) => (Some(m.to_string()), b), (None, Some(s)) => (Some(s), false), _ => (None, false) };
        if let Some(txt) = txt {
            col.add_space(0.6 * rem);
            let (fill, edge, ink) = if bad { (Color32::from_rgb(0x3d, 0x1a, 0x1a), crate::ui_kit::DANGER(), crate::ui_kit::text_danger()) } else { (Color32::from_rgb(0x2d, 0x24, 0x10), Color32::from_rgb(0x7a, 0x65, 0x30), Color32::from_rgb(0xc8, 0xa0, 0x50)) };
            egui::Frame::new().fill(fill).stroke(Stroke::new(1.0_f32, edge)).corner_radius((0.5 * rem) as u8).inner_margin(egui::Margin::symmetric((0.9 * rem) as i8, (0.6 * rem) as i8))
                .show(&mut col, |ui| { ui.set_width(col_w - 2.0 * rem); ui.label(RichText::new(txt).size(0.85 * rem).color(ink)); });
        }
    }

    // the cards
    ps.rects.clear();
    ps.art.clear();
    let n = ps.results.len();
    if n == 0 { return; }
    let cards_top = col.min_rect().bottom() + 0.875 * rem;
    let gap = 14.0;
    let info_h = 6.375 * rem;
    let avail_w = w - 2.0 * 0.015 * w;
    let avail_h = (h - cards_top - h * 0.05 - 8.0 - info_h).max(120.0);
    let mut card_w = ((avail_w - gap * (n as f32 - 1.0)) / n as f32).floor().max(1.0);
    let mut art_h = (card_w * 1.5).floor();
    if art_h > avail_h { art_h = avail_h; card_w = (art_h * 2.0 / 3.0).floor().max(1.0); }
    ps.art_w = (card_w * 0.95).round() as u32;
    let row_w = card_w * n as f32 + gap * (n as f32 - 1.0);
    let x_start = (w - row_w) / 2.0;
    let painter = ui.painter().clone();
    for (i, c) in ps.results.iter().enumerate() {
        let t = hover.get(&c.appid).copied().unwrap_or(0.0);
        let s = 0.95 + 0.05 * (t * (2.0 - t));
        let full = Rect::from_min_size(Pos2::new(x_start + i as f32 * (card_w + gap), cards_top), Vec2::new(card_w, art_h + info_h + 2.0));
        let shown = ps.revealed;
        let r = Rect::from_center_size(full.center(), full.size() * s);
        if shown { ps.rects.push((c.appid, full)); }
        let art = Rect::from_min_size(r.min + Vec2::splat(1.0), Vec2::new(r.width() - 2.0, art_h * s));
        ps.art.push((c.appid, art));
        if !shown { continue; }
        let outline = data.pick_outlines.get(&c.appid).copied();
        if let Some((r8, g8, b8)) = outline { painter.rect_stroke(r, (0.625 * rem * s) as u8, Stroke::new(3.0_f32, Color32::from_rgb(r8, g8, b8)), egui::StrokeKind::Outside); }   // `.result-card[--outline-color]`
        let info = Rect::from_min_max(Pos2::new(art.min.x, art.max.y), r.max - Vec2::splat(1.0));
        let radius = (0.625 * rem * s) as u8;
        painter.rect_filled(info, egui::CornerRadius { nw: 0, ne: 0, sw: radius.saturating_sub(1), se: radius.saturating_sub(1) }, PANEL());
        let edge = if let Some((r8, g8, b8)) = outline { Color32::from_rgb(r8, g8, b8) } else if t > 0.5 { accent } else if c.relaxed { Color32::from_rgb(0x3a, 0x4a, 0x5e) } else { EDGE() };
        painter.rect_stroke(r, radius, Stroke::new(1.0_f32, edge), egui::StrokeKind::Inside);
        let name = data.info(c.appid).map(|g: &GameInfo| g.name.clone()).unwrap_or_default();
        let text_w = info.width() - 1.5 * rem;
        let mut y = info.min.y + 0.75 * rem * s;
        let mut job = egui::text::LayoutJob::single_section(name.clone(), egui::text::TextFormat { font_id: crate::theme::bold(0.9 * rem), color: Color32::WHITE, ..Default::default() });
        job.wrap = egui::text::TextWrapping { max_width: text_w, max_rows: 1, break_anywhere: true, overflow_character: Some('\u{2026}') };
        let g = painter.layout_job(job);
        painter.galley(Pos2::new(info.min.x + 0.75 * rem, y), g.clone(), Color32::WHITE);
        y += g.size().y + 0.5 * rem;
        if let Some(reason) = &c.reason {
            let mut job = egui::text::LayoutJob::single_section(reason.clone(), egui::text::TextFormat { font_id: FontId::proportional(0.82 * rem), color: MUTED(), italics: true, ..Default::default() });
            job.wrap = egui::text::TextWrapping { max_width: text_w, max_rows: 3, break_anywhere: false, overflow_character: Some('\u{2026}') };
            painter.galley(Pos2::new(info.min.x + 0.75 * rem, y), painter.layout_job(job), MUTED());
        }
        if missing.contains(&c.appid) {
            let g = painter.layout(name, FontId::proportional(0.8 * rem), MUTED(), (art.width() - 20.0).max(20.0));
            painter.galley(art.center() - g.size() / 2.0, g, MUTED());
        }
    }
}


/// Covers into the window buffer, then rounded to the card's corners. Called after `show` each frame.
pub fn paint(buf: &mut [u32], bg: &[u32], stride: usize, ps: &mut PickState, covers: &mut home::Covers, settled: bool) {
    // a re-roll: decode its covers while the old six stay up, then swap them in together
    if let Some((cards, _, _, since)) = &ps.pending {
        let mut ready = true;
        for c in cards {
            covers.request(c.appid, ps.art_w);
            if !covers.exact(c.appid, ps.art_w) && !covers.missing.contains(&c.appid) { ready = false; }
        }
        if ready || since.elapsed() >= std::time::Duration::from_millis(700) {
            if let Some((cards, pool, relaxed, _)) = ps.pending.take() { ps.results = cards; ps.pool = pool; ps.relaxed = relaxed; ps.wants_redraw = true; }
        }
    }
    let mut all_ready = true;
    for (appid, art) in &ps.art {
        if settled || !ps.revealed { covers.request(*appid, ps.art_w); }
        let img = covers.get(*appid, ps.art_w);
        // a cover with no file at all counts as ready (its name is shown instead)
        if img.is_none() && !covers.missing.contains(appid) { all_ready = false; }
        if !ps.revealed { continue; }
        let (x, y, w, h) = (art.min.x.round() as i64, art.min.y.round() as i64, art.width().round() as i64, art.height().round() as i64);
        crate::paint_op(buf, 0, stride, &crate::Op { x, y, w, h, img });
        home::round_corners(buf, stride, x as i32, y as i32, w as i32, h as i32, 9, &|i| bg[i]);
    }
    if !ps.revealed && !ps.art.is_empty() && (all_ready || ps.hold_until.map_or(true, |t| std::time::Instant::now() >= t)) { ps.revealed = true; ps.wants_redraw = true; }
}
