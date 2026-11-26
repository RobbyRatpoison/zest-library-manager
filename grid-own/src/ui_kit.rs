//! Shared pieces for dialogs and menus: the modal frame (scrim, title bar, Esc/outside click to close) and the page's button styles.
//! Colors are the web CSS's (`#1a2332` panels, `#2a475e` borders, the theme accent).

use egui::{Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, Ui, Vec2};

/// Every color a theme controls (PlayDate's `theme.json` variables), read by the app instead of literals. `set_from_theme` fills it when the theme loads or changes.
#[derive(Clone, Copy)]
pub struct Palette {
    pub accent: Color32, pub surface: Color32, pub raised: Color32, pub input: Color32, pub card: Color32, pub text: Color32, pub heading: Color32,
    pub muted: Color32, pub text_input: Color32, pub bright: Color32, pub on_accent: Color32, pub positive: Color32, pub border: Color32, pub danger: Color32, pub text_danger: Color32, pub warning: Color32,
    pub hover: Color32, pub divider: Color32, pub focus_ring: Color32, pub button: Color32, pub selection: Color32, pub backdrop: Color32,
}
const fn c(r: u8, g: u8, b: u8) -> Color32 { Color32::from_rgb(r, g, b) }
static PALETTE: std::sync::RwLock<Palette> = std::sync::RwLock::new(Palette {
    accent: c(0x66, 0xc0, 0xf4), surface: c(0x1b, 0x28, 0x38), raised: c(0x1a, 0x23, 0x32), input: c(0x10, 0x18, 0x22), card: c(0x10, 0x18, 0x22),
    text: c(0xc7, 0xd5, 0xe0), heading: c(0xe2, 0xea, 0xf0), muted: c(0x8f, 0x98, 0xa0), text_input: c(0xff, 0xff, 0xff), bright: c(0xff, 0xff, 0xff), on_accent: c(0x0e, 0x16, 0x21),
    positive: c(0x5c, 0x7e, 0x10), border: c(0x2a, 0x47, 0x5e), danger: c(0xa3, 0x2a, 0x2a), text_danger: c(0xff, 0x80, 0x80), warning: c(0xc9, 0x7c, 0x00),
    hover: c(0x24, 0x34, 0x47), divider: c(0x2a, 0x3f, 0x55), focus_ring: c(0x66, 0xc0, 0xf4), button: c(0x1b, 0x28, 0x38), selection: c(0x1f, 0x4e, 0x73), backdrop: c(0, 0, 0),
});
pub fn pal() -> Palette { *PALETTE.read().unwrap() }
/// Loads the palette from a theme (at start, and again when Appearance applies one).
pub fn set_from_theme(t: &crate::theme::Theme) {
    let k = |v: crate::theme::Rgb| Color32::from_rgb(v.0, v.1, v.2);
    *PALETTE.write().unwrap() = Palette { accent: k(t.accent), surface: k(t.bg_surface), raised: k(t.bg_raised), input: k(t.bg_input), card: k(t.bg_card), text: k(t.text_primary),
        heading: k(t.text_heading), muted: k(t.text_secondary), text_input: k(t.text_input), bright: k(t.text_bright), on_accent: k(t.on_accent), positive: k(t.accent_positive), border: k(t.border),
        danger: k(t.color_danger), text_danger: k(t.text_danger), warning: k(t.color_warning),
        hover: k(t.hover()), divider: k(t.divider_c()), focus_ring: k(t.focus()), button: k(t.button()), selection: k(t.selection), backdrop: k(t.backdrop) };
}
// Theme colors under the names the dialogs already use (`--bg-raised`, `--border`, `--text-secondary`, `--text-primary`, `--bg-input`, `--on-accent`, `--color-danger`).
#[allow(non_snake_case)] pub fn PANEL() -> Color32 { pal().raised }
#[allow(non_snake_case)] pub fn EDGE() -> Color32 { pal().border }
#[allow(non_snake_case)] pub fn RULE() -> Color32 { pal().divider }
#[allow(non_snake_case)] pub fn MUTED() -> Color32 { pal().muted }
#[allow(non_snake_case)] pub fn TEXT() -> Color32 { pal().text }
#[allow(non_snake_case)] pub fn HOVER() -> Color32 { pal().hover }
#[allow(non_snake_case)] pub fn INPUT() -> Color32 { pal().input }
#[allow(non_snake_case)] pub fn ON_ACCENT() -> Color32 { pal().on_accent }
#[allow(non_snake_case)] pub fn DANGER() -> Color32 { pal().danger }
/// `--text-heading`, `--text-bright`, `--accent-positive`, `--text-danger`, `--color-warning`, `--text-input`, `--bg-card`.
pub fn heading() -> Color32 { pal().heading }
pub fn bright() -> Color32 { pal().bright }
pub fn positive() -> Color32 { pal().positive }
pub fn text_danger() -> Color32 { pal().text_danger }
pub fn warning() -> Color32 { pal().warning }
pub fn text_input() -> Color32 { pal().text_input }
pub fn card() -> Color32 { pal().card }
/// The dimmed backdrop behind a dialog (`--backdrop`, always at the same strength), the keyboard / pad focus ring, and the toolbar buttons' fill.
pub fn backdrop() -> Color32 { let b = pal().backdrop; Color32::from_rgba_unmultiplied(b.r(), b.g(), b.b(), 150) }
pub fn focus_ring() -> Color32 { pal().focus_ring }
pub fn button_fill() -> Color32 { pal().button }
/// The theme's accent and surface colors, used by every dialog frame.
pub fn accent() -> Color32 { pal().accent }
pub fn surface() -> Color32 { pal().surface }

static UI_SCALE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(100);
/// The UI scale in percent (Settings > Appearance), 75 to 150.
pub fn set_scale(pct: u32) { UI_SCALE.store(pct.clamp(75, 150), std::sync::atomic::Ordering::Relaxed); }
pub fn rem(ctx: &egui::Context) -> f32 { (ctx.content_rect().width() * 0.00625).clamp(11.0, 20.0) * UI_SCALE.load(std::sync::atomic::Ordering::Relaxed) as f32 / 100.0 }

/// Set just before a dialog that has no footer (PlayDate's Settings dialogs): the rule and padding of the empty footer strip are left out. Taken by the next dialog drawn.
pub static NO_FOOTER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A dialog body that scrolls when it would be taller than `max_h` (the window less the dialog's header) and is drawn exactly as it always was otherwise. It does not size the body from
/// last frame's height: a list that fills in later (HLTB's) or a body that grows must be free to. It only notices that the drawn body is too tall and, from the next frame, puts it in a
/// scroll area of `max_h`; when the content fits again the scroll area goes away.
pub fn fit_scroll<R>(ui: &mut Ui, id: egui::Id, max_h: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    let key = id.with("wrapped");
    let wrapped: bool = ui.ctx().data(|d| d.get_temp(key)).unwrap_or(false);
    if !wrapped {
        let top = ui.cursor().min.y;
        // The body is drawn with its height limited to `max_h`: a scroll area inside it (Settings > Library, the lists in many dialogs) shrinks to the room that is left, so the page has ONE
        // scroll area. Only a body with nothing that scrolls itself overflows, and only then does the automatic scroll below take over (two nested ones fought over the wheel and their bars).
        let out = ui.scope(|ui| { ui.set_max_height(max_h); add(ui) }).inner;
        if ui.min_rect().max.y - top > max_h + 1.0 { ui.ctx().data_mut(|d| d.insert_temp(key, true)); ui.ctx().request_repaint(); }
        out
    } else {
        let w = ui.available_width();
        let res = ui.allocate_ui_with_layout(Vec2::new(w, max_h), egui::Layout::top_down(egui::Align::Min), |ui| {
            egui::ScrollArea::vertical().id_salt(id).max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
                // a body with a scroll area of its own (Settings > Library, the lists in several dialogs) would have its bar under this one's at the same edge: leave a gutter for ours
                let gutter = ui.spacing().scroll.bar_width + 6.0;
                ui.set_max_width((ui.available_width() - gutter).max(1.0));
                add(ui)
            })
        }).inner;
        if res.content_size.y <= max_h - 2.0 { ui.ctx().data_mut(|d| d.insert_temp(key, false)); ui.ctx().request_repaint(); }
        res.inner
    }
}

/// `body` in a vertical scroll area of at most `max_h` when `scroll`, else drawn straight into `ui`. A page that already scrolls as a whole (the Settings > Library dialog, which lists
/// several tabs one under another) passes `false`, so its sections are part of that one scroll area instead of small scroll areas inside it, whose bars sat under the page's own.
pub fn maybe_scroll<R>(ui: &mut Ui, scroll: bool, max_h: f32, salt: &str, body: impl FnOnce(&mut Ui) -> R) -> R {
    if scroll { egui::ScrollArea::vertical().id_salt(salt).max_height(max_h).auto_shrink([false, true]).show(ui, body).inner } else { body(ui) }
}

/// A dialog opens invisible and is shown once its size has stopped changing. An Area centres itself from its size on the PREVIOUS pass, and on the first frames a scroll area inside has not
/// measured its content yet, so the dialog was drawn off-centre and too short for a frame or two, then jumped to its real place (a brief flash on every open). The layout still runs while it
/// is hidden (opacity 0), and the dialog is never hidden again after it first settles (a body that grows later is not a reason to flash).
fn settle_hidden(ctx: &egui::Context, id: &str) -> bool {
    // "still open" means it was drawn in the previous FRAME, however long ago that was (nothing is drawn while the window is idle, so a clock would call a rested dialog a new one)
    let frame = ctx.cumulative_frame_nr();
    let st: Option<(Vec2, u8, u64, bool)> = ctx.data(|d| d.get_temp(egui::Id::new((id, "settle"))));
    !matches!(st, Some((_, _, f, true)) if frame.saturating_sub(f) <= 1)
}
fn settle_update(ctx: &egui::Context, id: &str, size: Vec2) {
    let key = egui::Id::new((id, "settle"));
    let frame = ctx.cumulative_frame_nr();
    let prev: Option<(Vec2, u8, u64, bool)> = ctx.data(|d| d.get_temp(key));
    let (stable, settled) = match prev {
        Some((s, n, f, done)) if frame.saturating_sub(f) <= 1 => { let n = if (s - size).abs().max_elem() < 0.5 { (n + 1).min(10) } else { 0 }; (n, done || n >= 2) }
        _ => (0, false),
    };
    ctx.data_mut(|d| d.insert_temp(key, (size, stable, frame, settled)));
    if !settled { ctx.request_repaint(); }
}

/// The last settled picture of a dialog, so that going from one page of it to another (Settings > Appearance) keeps the old page on screen until the new one is ready, and the two swap in
/// one frame instead of leaving nothing in between. Only a page change of the SAME dialog id uses it: a dialog opened fresh never shows an old picture.
type Snap = (String, u64, Vec<egui::epaint::ClippedShape>);
fn remember_snapshot(ctx: &egui::Context, id: &str, title: &str) {
    let layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::new((id, "dialog")));
    let shapes: Vec<egui::epaint::ClippedShape> = ctx.graphics(|g| g.get(layer).map(|l| l.all_entries().cloned().collect()).unwrap_or_default());
    if shapes.is_empty() { return; }
    let frame = ctx.cumulative_frame_nr();
    ctx.data_mut(|d| d.insert_temp::<Snap>(egui::Id::new((id, "snap")), (title.to_string(), frame, shapes)));
}
fn show_snapshot(ctx: &egui::Context, id: &str, title: &str) {
    let key = egui::Id::new((id, "snap"));
    let frame = ctx.cumulative_frame_nr();
    let Some((t, f, shapes)) = ctx.data(|d| d.get_temp::<Snap>(key)) else { return };
    if t == title || frame.saturating_sub(f) > 1 { return; }
    let layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::new((id, "snap_layer")));
    ctx.graphics_mut(|g| { let list = g.entry(layer); for cs in &shapes { list.add(cs.clip_rect, cs.shape.clone()); } });
    ctx.data_mut(|d| d.insert_temp::<Snap>(key, (t, frame, shapes)));   // keep it "just shown" for as long as the new page is settling
    ctx.request_repaint();
}

/// What a dialog frame reports back.
pub struct ModalOut<R> { pub closed: bool, pub inner: Option<R> }

/// A centred dialog over a dimmed screen, as the web's `.modal-content`: surface fill, accent border with a soft glow, a header with the title
/// and a close cross, a body, and an optional footer row. The scrim takes every click, Esc or a click outside the dialog closes it.
/// `width` is in rem. Returns whether it was closed this frame plus whatever the body returned.
pub fn modal<R>(ctx: &egui::Context, id: &str, title: &str, width_rem: f32, body: impl FnOnce(&mut Ui, f32) -> R) -> ModalOut<R> {
    modal_footer(ctx, id, title, width_rem, body, |_, _| {})
}

pub fn modal_footer<R>(ctx: &egui::Context, id: &str, title: &str, width_rem: f32, body: impl FnOnce(&mut Ui, f32) -> R, footer: impl FnOnce(&mut Ui, f32)) -> ModalOut<R> {
    modal_full(ctx, id, title, width_rem, |_, _| {}, body, footer)
}

/// `modal_footer` plus a closure for the right end of the header (a mode toggle, say), drawn left of the close cross.
pub fn modal_full<R>(ctx: &egui::Context, id: &str, title: &str, width_rem: f32, header_right: impl FnOnce(&mut Ui, f32), body: impl FnOnce(&mut Ui, f32) -> R, footer: impl FnOnce(&mut Ui, f32)) -> ModalOut<R> {
    let screen = ctx.content_rect();
    let r = rem(ctx);
    let accent = accent();
    let no_footer = NO_FOOTER.swap(false, std::sync::atomic::Ordering::Relaxed);
    let mut closed = ctx.input(|i| i.key_pressed(egui::Key::Escape)) && !egui::Popup::is_any_open(ctx);   // B with a dropdown open closes the dropdown, not the dialog
    let scrim_order = if STACKED.swap(false, std::sync::atomic::Ordering::Relaxed) { egui::Order::Foreground } else { egui::Order::Middle };
    egui::Area::new(egui::Id::new((id, "scrim"))).order(scrim_order).fixed_pos(Pos2::ZERO).show(ctx, |ui| {
        let (rect, resp) = ui.allocate_exact_size(screen.size(), Sense::CLICK | Sense::DRAG);   // not FOCUSABLE: the pad must never land on the scrim
        ui.painter().rect_filled(rect, 0.0, backdrop());
        if resp.clicked() { closed = true; }
    });
    // a dialog stacked over another one: its scrim above the dialog beneath, its own window above the scrim, every frame (egui remembers a layer's place between openings, and a scrim
    // that had been clicked once could end up over its own window: the dialog then showed dimmed and every click closed it)
    if scrim_order == egui::Order::Foreground { ctx.move_to_top(egui::LayerId::new(scrim_order, egui::Id::new((id, "scrim")))); }
    // the scrim is the modal layer: egui keeps keyboard / gamepad focus and clicks off everything below it (the page behind), while the dialog above it stays live
    ctx.memory_mut(|m| m.set_modal_layer(egui::LayerId::new(scrim_order, egui::Id::new((id, "scrim")))));
    let width = (width_rem * r).min(screen.width() - 2.0 * r);
    let mut inner = None;
    let hidden = settle_hidden(ctx, &format!("{id}|{title}"));
    if hidden { show_snapshot(ctx, id, title); }
    let area = egui::Area::new(egui::Id::new((id, "dialog"))).order(egui::Order::Foreground).anchor(Align2::CENTER_CENTER, Vec2::ZERO).show(ctx, |ui| {
        if hidden { ui.set_opacity(0.0); }
        // An Area hands its content the size it had LAST frame as the room to grow into (a small default on the first), so a scroll area inside it could never grow past the height the dialog had
        // before its content arrived: dialogs opened short and stayed short. Every dialog gets the whole window to grow into.
        ui.set_max_height(ctx.content_rect().height());
        egui::Frame::new().fill(surface()).stroke(Stroke::new(1.0_f32, accent)).corner_radius((0.5 * r) as u8)
            .shadow(egui::Shadow { offset: [0, 0], blur: 20, spread: 0, color: accent.gamma_multiply(0.2) })
            .show(ui, |ui| {
                ui.set_width(width);
                ui.spacing_mut().item_spacing = Vec2::new(0.5 * r, 0.65 * r);
                // header: title and close cross, a rule underneath
                let bar_h = 1.25 * r + 1.4 * r + 0.875 * r;
                let (bar, _) = ui.allocate_exact_size(Vec2::new(width, bar_h), Sense::hover());
                let mid = bar.min.y + 1.25 * r + 0.7 * r;
                // a title longer than the room before the close cross is cut with an ellipsis instead of running out of the dialog
                let mut job = egui::text::LayoutJob::simple(title.to_string(), crate::theme::bold(1.28 * r), crate::ui_kit::heading(), (width - 1.5 * r - 4.2 * r).max(40.0));
                job.wrap.max_rows = 1; job.wrap.break_anywhere = true; job.wrap.overflow_character = Some('\u{2026}');
                let galley = ui.painter().layout_job(job);
                ui.painter().galley(Pos2::new(bar.min.x + 1.5 * r, mid - galley.size().y / 2.0), galley, crate::ui_kit::heading());
                let x_rect = Rect::from_center_size(Pos2::new(bar.max.x - 1.5 * r - 0.2 * r, mid), Vec2::splat(1.8 * r));
                let x_resp = ui.interact(x_rect, ui.id().with((id, "x")), Sense::click());
                let hot = x_resp.hovered();
                ui.painter().text(x_rect.center(), Align2::CENTER_CENTER, "\u{2715}", FontId::proportional(1.3 * r), if hot { Color32::WHITE } else { MUTED() });
                if hot { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
                if x_resp.clicked() { closed = true; }
                let hr = Rect::from_min_max(Pos2::new(bar.min.x + 1.5 * r, mid - 1.4 * r), Pos2::new(x_rect.min.x - 0.6 * r, mid + 1.4 * r));
                ui.scope_builder(egui::UiBuilder::new().max_rect(hr).layout(egui::Layout::right_to_left(egui::Align::Center)), |ui| header_right(ui, r));
                ui.painter().line_segment([Pos2::new(bar.min.x, bar.max.y), Pos2::new(bar.max.x, bar.max.y)], Stroke::new(1.0_f32, RULE()));
                // body
                egui::Frame::new().inner_margin(egui::Margin::symmetric((1.9 * r) as i8, (1.25 * r) as i8)).show(ui, |ui| {
                    ui.set_width(width - 3.8 * r);
                    inner = Some(fit_scroll(ui, egui::Id::new((id, "fit", title)), (screen.height() - 14.0 * r).max(15.0 * r), |ui| body(ui, r)));
                });
                if !no_footer {
                // footer, with a rule above it
                let rule_y = ui.cursor().min.y;
                ui.painter().line_segment([Pos2::new(ui.min_rect().min.x, rule_y), Pos2::new(ui.min_rect().min.x + width, rule_y)], Stroke::new(1.0_f32, RULE()));
                let before = ui.cursor().min.y;
                egui::Frame::new().inner_margin(egui::Margin::symmetric((1.9 * r) as i8, (0.875 * r) as i8)).show(ui, |ui| {
                    ui.set_width(width - 3.8 * r);
                    footer(ui, r);
                });
                let _ = before;
                }
            });
    });
    settle_update(ctx, &format!("{id}|{title}"), area.response.rect.size());
    if !hidden { remember_snapshot(ctx, id, title); }
    if scrim_order == egui::Order::Foreground { ctx.move_to_top(egui::LayerId::new(egui::Order::Foreground, egui::Id::new((id, "dialog")))); }
    ModalOut { closed, inner }
}

/// A footer or toolbar button as the web's `.nav-btn`: surface fill, faint white border, bold uppercase text; `fill` overrides for Apply (green).
pub fn nav_button(ui: &mut Ui, text: &str, width: f32, r: f32, fill: Option<(Color32, Color32)>) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 2.5 * r), Sense::click());
    let hot = resp.hovered();
    let (bg, edge, ink) = match fill {
        Some((f, e)) => (if hot { f.gamma_multiply(1.2) } else { f }, e, bright()),
        None => if hot { (accent(), accent(), surface()) } else { (button_fill(), EDGE(), heading()) },
    };
    ui.painter().rect_filled(rect, (0.375 * r) as u8, bg);
    ui.painter().rect_stroke(rect, (0.375 * r) as u8, Stroke::new(1.0_f32, edge), egui::StrokeKind::Inside);
    let galley = ui.painter().layout_no_wrap(text.to_uppercase(), crate::theme::bold(0.9 * r), ink);
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, ink);
    if hot { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
    focus_mark(ui, &resp, rect);
    resp
}

/// The web's `.vm-view-btn`: a dark grey button, accent when it is the current choice. `enabled = false` dims it.
pub fn view_button(ui: &mut Ui, text: &str, on: bool, enabled: bool, r: f32) -> egui::Response {
    let size = 0.82 * r;
    let galley = ui.painter().layout_no_wrap(text.to_string(), FontId::proportional(size), Color32::PLACEHOLDER);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(galley.size().x + 1.75 * r, 2.25 * r), if enabled { Sense::click() } else { Sense::hover() });
    let hot = resp.hovered() && enabled && !on;
    let (bg, edge, ink) = if on { (accent(), accent(), surface()) } else if hot { (Color32::from_rgb(0x44, 0x44, 0x44), Color32::from_rgb(0x44, 0x44, 0x44), Color32::WHITE) } else { (Color32::from_rgb(0x33, 0x33, 0x33), Color32::from_rgb(0x44, 0x44, 0x44), Color32::WHITE) };
    let ink = if enabled { ink } else { ink.gamma_multiply(0.45) };
    ui.painter().rect_filled(rect, (0.375 * r) as u8, if enabled { bg } else { bg.gamma_multiply(0.7) });
    ui.painter().rect_stroke(rect, (0.375 * r) as u8, Stroke::new(1.0_f32, edge), egui::StrokeKind::Inside);
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, ink);
    if resp.hovered() && enabled { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
    focus_mark(ui, &resp, rect);
    resp
}

/// Dropdown look of `.nav-dropdown` inside a dialog: tall, dark grey, bold text.
pub fn style_dropdown(ui: &mut Ui, r: f32) {
    ui.spacing_mut().interact_size.y = 2.5 * r;
    ui.spacing_mut().button_padding = Vec2::new(0.8 * r, 0.4 * r);
    let v = ui.visuals_mut();
    let (fill, hover, edge) = (Color32::from_rgb(0x33, 0x33, 0x33), Color32::from_rgb(0x44, 0x44, 0x44), Color32::from_rgb(0x44, 0x44, 0x44));
    for (w, f) in [(&mut v.widgets.inactive, fill), (&mut v.widgets.hovered, hover), (&mut v.widgets.active, hover), (&mut v.widgets.open, fill)] {
        w.weak_bg_fill = f; w.bg_fill = f; w.bg_stroke = Stroke::new(1.0_f32, edge); w.fg_stroke = Stroke::new(1.0_f32, Color32::WHITE);
        w.corner_radius = egui::CornerRadius::same((0.375 * r) as u8); w.expansion = 0.0;
    }
}

/// The page's pill button, painted by hand so hovering never changes its size. `primary` = accent fill, `danger` = red.
pub fn button(ui: &mut Ui, text: &str, kind: Kind, accent: Color32, r: f32) -> egui::Response { button_h(ui, text, kind, accent, r, if kind == Kind::Primary { 3.3 } else { 3.1 }) }

/// A small square toggle with a triangle (collapsible cards): about as tall as the line of text beside it.
pub fn chevron(ui: &mut Ui, open: bool, r: f32) -> egui::Response {
    let side = 1.5 * r;
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(side), Sense::click());
    let hot = resp.hovered();
    ui.painter().rect_filled(rect, 4, if hot { HOVER() } else { Color32::TRANSPARENT });
    ui.painter().rect_stroke(rect, 4, Stroke::new(1.0_f32, EDGE()), egui::StrokeKind::Inside);
    let g = ui.painter().layout_no_wrap(if open { "\u{25BE}" } else { "\u{25B8}" }.to_string(), crate::theme::bold(0.8 * r), Color32::PLACEHOLDER);
    ui.painter().galley(rect.center() - g.size() / 2.0, g, if hot { TEXT() } else { MUTED() });
    if hot { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
    focus_mark(ui, &resp, rect);
    resp
}

/// `button` with the height (in rem) the caller wants: a row of buttons of different kinds can be made the same height.
pub fn button_h(ui: &mut Ui, text: &str, kind: Kind, accent: Color32, r: f32, height_rem: f32) -> egui::Response {
    let size = 0.85 * r;
    let galley = ui.painter().layout_no_wrap(text.to_string(), crate::theme::bold(size), Color32::PLACEHOLDER);
    // PlayDate's buttons are about 3.1rem tall (3.3rem for the main action of a dialog); the text is the same size as before
    let want_h = height_rem * r;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(galley.size().x + 2.0 * size * 1.2, (galley.size().y + 2.0 * size * 0.5).max(want_h)), Sense::click());
    let hot = resp.hovered();
    let (fill, ink, edge) = match kind {
        Kind::Primary => (if hot { accent.gamma_multiply(0.88) } else { accent }, ON_ACCENT(), accent),
        Kind::Danger => (if hot { DANGER().gamma_multiply(1.15) } else { DANGER() }, bright(), DANGER()),
        Kind::Plain => (if hot { HOVER() } else { Color32::TRANSPARENT }, if hot { TEXT() } else { MUTED() }, EDGE()),
    };
    ui.painter().rect_filled(rect, 5, fill);
    ui.painter().rect_stroke(rect, 5, Stroke::new(1.0_f32, edge), egui::StrokeKind::Inside);
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, ink);
    if hot { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
    focus_mark(ui, &resp, rect);
    resp
}

#[derive(Clone, Copy, PartialEq)]
pub enum Kind { Primary, Danger, Plain }

pub fn muted(text: impl Into<String>, size: f32) -> RichText { RichText::new(text.into()).size(size).color(MUTED()) }

/// A menu row as the web's `.hamburger-item`: full width, hover fill, left-aligned text. Returns true when clicked.
pub fn menu_item(ui: &mut Ui, text: &str, color: Color32, bold: bool, r: f32) -> bool { menu_item_f(ui, text, color, bold, r, false) }

/// `focus` takes the keyboard / gamepad focus (a menu opened with the pad starts on its first item). The focused item looks hovered.
pub fn menu_item_f(ui: &mut Ui, text: &str, color: Color32, bold: bool, r: f32, focus: bool) -> bool {
    let size = 0.85 * r;
    let font = if bold { crate::theme::bold(size) } else { FontId::proportional(size) };
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, Color32::PLACEHOLDER);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), galley.size().y + 1.2 * r), Sense::click());
    if focus { resp.request_focus(); }
    if resp.hovered() { ui.painter().rect_filled(rect, 0.0, HOVER()); ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
    else if resp.has_focus() { ui.painter().rect_filled(rect, 0.0, HOVER()); }
    ui.painter().galley(Pos2::new(rect.min.x + 1.0 * r, rect.center().y - galley.size().y / 2.0), galley, color);
    resp.clicked()
}

/// A row of the Settings list as the web's `.settings-item`: full width, input-colored with a border, left-aligned text. `focus` takes the pad focus.
pub fn settings_row(ui: &mut Ui, text: &str, r: f32, focus: bool) -> egui::Response {
    let size = 0.88 * r;
    let galley = ui.painter().layout_no_wrap(text.to_string(), FontId::proportional(size), Color32::PLACEHOLDER);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), galley.size().y + 1.4 * r), Sense::click());
    if focus { resp.request_focus(); }
    let hot = resp.hovered() || resp.has_focus();
    if resp.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
    ui.painter().rect_filled(rect, 4, if hot { HOVER() } else { INPUT() });
    ui.painter().rect_stroke(rect, 4, Stroke::new(1.0_f32, if hot { accent() } else { EDGE() }), egui::StrokeKind::Inside);
    ui.painter().galley(Pos2::new(rect.min.x + 0.9 * r, rect.center().y - galley.size().y / 2.0), galley, TEXT());
    resp
}

pub fn menu_sep(ui: &mut Ui, r: f32) {
    ui.add_space(0.25 * r);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, RULE());
    ui.add_space(0.25 * r);
}

/// A context-menu row as the web's `.ctx-item`: an icon column, the label, an optional arrow (submenu) and a tick color for the current choice.
pub fn menu_row(ui: &mut Ui, icon: &str, text: &str, color: Color32, arrow: bool, r: f32) -> egui::Response {
    let size = 0.85 * r;
    let galley = ui.painter().layout_no_wrap(text.to_string(), FontId::proportional(size), Color32::PLACEHOLDER);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), galley.size().y + 1.0 * r), Sense::click());
    if resp.hovered() { ui.painter().rect_filled(rect, 0.0, HOVER()); ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
    let icon_g = ui.painter().layout_no_wrap(icon.to_string(), FontId::proportional(size), Color32::PLACEHOLDER);
    ui.painter().galley(Pos2::new(rect.min.x + 1.0 * r + (1.0 * r - icon_g.size().x) / 2.0, rect.center().y - icon_g.size().y / 2.0), icon_g, color);
    ui.painter().galley(Pos2::new(rect.min.x + 1.0 * r + 1.625 * r, rect.center().y - galley.size().y / 2.0), galley, color);
    if arrow { ui.painter().text(Pos2::new(rect.max.x - 0.9 * r, rect.center().y), Align2::RIGHT_CENTER, "\u{25B8}", FontId::proportional(0.75 * r), Color32::from_rgb(0x4a, 0x6a, 0x8a)); }
    resp
}

/// The frame of a popup menu: panel color, border, radius and shadow of `#ctx-menu`.
pub fn menu_frame(r: f32) -> egui::Frame {
    egui::Frame::new().fill(PANEL()).stroke(Stroke::new(1.0_f32, EDGE())).corner_radius((0.5 * r) as u8)
        .shadow(egui::Shadow { offset: [0, 8], blur: 32, spread: 0, color: Color32::from_black_alpha(140) }).inner_margin(egui::Margin::symmetric(0, (0.25 * r) as i8))
}

/// A slider that fills from the left edge to the thumb (card size, and the like).
pub fn slider(ui: &mut Ui, v: &mut f32, min: f32, max: f32, step: f32, width: f32, accent: Color32) -> egui::Response {
    let (rect, mut resp) = ui.allocate_exact_size(Vec2::new(width, 16.0), Sense::click_and_drag());
    let (pad, cy) = (7.0, rect.center().y);
    let (x0, x1) = (rect.min.x + pad, rect.max.x - pad);
    if let (true, Some(p)) = (resp.dragged() || resp.clicked(), resp.interact_pointer_pos()) {
        let nv = (min + ((p.x - x0) / (x1 - x0) * (max - min) / step).round() * step).clamp(min, max);
        if nv != *v { *v = nv; resp.mark_changed(); }
    }
    let x = x0 + (*v - min) / (max - min) * (x1 - x0);
    ui.painter().rect_filled(Rect::from_min_max(Pos2::new(x0, cy - 2.0), Pos2::new(x1, cy + 2.0)), 2, RULE());
    ui.painter().rect_filled(Rect::from_min_max(Pos2::new(x0, cy - 2.0), Pos2::new(x, cy + 2.0)), 2, accent);
    ui.painter().circle_filled(Pos2::new(x, cy), 7.0, accent);
    if resp.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
    resp
}

/// Dropdown and field colors for the dialog bodies.
pub fn style_fields(ui: &mut Ui) {
    let v = ui.visuals_mut();
    v.extreme_bg_color = INPUT();
    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] { w.bg_stroke = Stroke::new(1.0_f32, RULE()); w.expansion = 0.0; }
    v.widgets.inactive.weak_bg_fill = INPUT(); v.widgets.hovered.weak_bg_fill = HOVER(); v.widgets.open.weak_bg_fill = INPUT();
    v.widgets.inactive.bg_fill = INPUT(); v.widgets.hovered.bg_fill = HOVER(); v.widgets.active.bg_fill = HOVER();   // the box of an unticked checkbox, the track of a slider
    v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, text_input()); v.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, heading());   // `--text-input`: what is typed in fields and picked in selects
    v.selection.bg_fill = pal().selection;
    v.window_fill = PANEL(); v.window_stroke = Stroke::new(1.0_f32, EDGE()); v.popup_shadow = egui::Shadow::NONE;
}

/// Keyboard / gamepad focus on a hand-painted button: scrolled into view when focus arrives. The ring itself is drawn by the app (`pad_focus_ring`)
/// for every focused widget, since most dialog controls are painted by hand.
pub fn focus_mark(ui: &Ui, resp: &egui::Response, _rect: Rect) {
    // while it has the focus and any part is clipped (it may have been drawn below the visible part of a list), keep asking the enclosing scroll area to bring it in
    if resp.gained_focus() || (resp.has_focus() && !ui.clip_rect().contains_rect(resp.rect)) { resp.scroll_to_me(None); }
}

/// A small toggle pill (Ascending / Descending, status filters): accent fill when on.
pub fn pill(ui: &mut Ui, text: &str, on: bool, accent: Color32, size: f32) -> egui::Response { pill_w(ui, text, on, accent, size, 0.0) }
/// `pill` at least `min_w` wide (the text stays centered): a grid of pills whose columns line up.
pub fn pill_w(ui: &mut Ui, text: &str, on: bool, accent: Color32, size: f32, min_w: f32) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(text.to_string(), crate::theme::bold(size), Color32::PLACEHOLDER);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new((galley.size().x + 2.0 * size * 1.15).max(min_w), galley.size().y + 2.0 * size * 0.45), Sense::click());
    let hot = resp.hovered() && !on;
    let (fill, ink, edge) = if on { (accent, ON_ACCENT(), accent) } else if hot { (HOVER(), TEXT(), EDGE()) } else { (Color32::TRANSPARENT, MUTED(), EDGE()) };
    ui.painter().rect_filled(rect, 5, fill);
    ui.painter().rect_stroke(rect, 5, Stroke::new(1.0_f32, edge), egui::StrokeKind::Inside);
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, ink);
    if resp.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
    focus_mark(ui, &resp, rect);
    resp
}

/// A button of the Library toolbar (`.search-nav-bar button`): 3rem tall, dark grey, bold; `danger` is the red CLEAR.
/// The selection badge ("3 selected x"): the toolbar button in the accent color (`#select-badge`).
pub fn accent_toolbar_button(ui: &mut Ui, text: &str, accent: Color32, r: f32) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(text.to_string(), crate::theme::bold(0.78 * r), Color32::PLACEHOLDER);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(galley.size().x + 1.6 * r, 3.0 * r), Sense::click());
    ui.painter().rect_filled(rect, (0.5 * r) as u8, accent);
    ui.painter().rect_stroke(rect, (0.5 * r) as u8, Stroke::new(1.0_f32, accent), egui::StrokeKind::Inside);
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, Color32::from_rgb(0x1b, 0x28, 0x38));
    if resp.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
    focus_mark(ui, &resp, rect);
    resp
}

pub fn toolbar_button(ui: &mut Ui, text: &str, danger: bool, r: f32) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(text.to_string(), crate::theme::bold(1.0 * r), Color32::PLACEHOLDER);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(galley.size().x + 3.0 * r, 3.0 * r), Sense::click());
    let hot = resp.hovered();
    let (bg, edge, ink) = if danger { (Color32::from_rgb(0x3d, 0x1a, 0x1a), crate::ui_kit::DANGER(), crate::ui_kit::text_danger()) }
        else if hot { (Color32::from_rgb(0x44, 0x44, 0x44), Color32::from_rgb(0x44, 0x44, 0x44), Color32::WHITE) } else { (Color32::from_rgb(0x33, 0x33, 0x33), Color32::from_rgb(0x44, 0x44, 0x44), Color32::WHITE) };
    ui.painter().rect_filled(rect, (0.5 * r) as u8, bg);
    ui.painter().rect_stroke(rect, (0.5 * r) as u8, Stroke::new(1.0_f32, edge), egui::StrokeKind::Inside);
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, ink);
    if hot { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
    focus_mark(ui, &resp, rect);
    resp
}

/// The dice button, drawn as a die (the die-face character it used to show is missing from the fonts and came out as something that did not look like dice).
pub fn dice_button(ui: &mut Ui, r: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(3.0 * r, 3.0 * r), Sense::click());
    let hot = resp.hovered();
    let (bg, edge) = if hot { (Color32::from_rgb(0x44, 0x44, 0x44), Color32::from_rgb(0x44, 0x44, 0x44)) } else { (Color32::from_rgb(0x33, 0x33, 0x33), Color32::from_rgb(0x44, 0x44, 0x44)) };
    let p = ui.painter();
    p.rect_filled(rect, (0.5 * r) as u8, bg);
    p.rect_stroke(rect, (0.5 * r) as u8, Stroke::new(1.0_f32, edge), egui::StrokeKind::Inside);
    // the die: a rounded square with the five-spot pattern
    let side = 1.7 * r;
    let die = Rect::from_center_size(rect.center(), Vec2::splat(side));
    p.rect_stroke(die, 0.3 * r, Stroke::new((0.12 * r).max(1.2), Color32::WHITE), egui::StrokeKind::Inside);
    let d = side * 0.27;
    for (dx, dy) in [(-1.0, -1.0), (1.0, -1.0), (0.0, 0.0), (-1.0, 1.0), (1.0, 1.0)] { p.circle_filled(die.center() + Vec2::new(dx * d, dy * d), (0.11 * r).max(1.3), Color32::WHITE); }
    if hot { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
    focus_mark(ui, &resp, rect);
    resp
}

/// The frame of a dialog with its own header: scrim, surface fill, accent border and glow, 1.875rem padding. Esc always closes it; a click on
/// the scrim closes it only when `close_on_outside` (an edit form should not lose its changes to a stray click).
pub fn dialog_shell<R>(ctx: &egui::Context, id: &str, width_rem: f32, close_on_outside: bool, body: impl FnOnce(&mut Ui, f32) -> R) -> ModalOut<R> {
    let screen = ctx.content_rect();
    let r = rem(ctx);
    let accent = accent();
    let mut closed = ctx.input(|i| i.key_pressed(egui::Key::Escape)) && !egui::Popup::is_any_open(ctx);   // B with a dropdown open closes the dropdown, not the dialog
    let scrim_order = if STACKED.swap(false, std::sync::atomic::Ordering::Relaxed) { egui::Order::Foreground } else { egui::Order::Middle };
    egui::Area::new(egui::Id::new((id, "scrim"))).order(scrim_order).fixed_pos(Pos2::ZERO).show(ctx, |ui| {
        let (rect, resp) = ui.allocate_exact_size(screen.size(), Sense::CLICK | Sense::DRAG);   // not FOCUSABLE: the pad must never land on the scrim
        ui.painter().rect_filled(rect, 0.0, backdrop());
        if close_on_outside && resp.clicked() { closed = true; }
    });
    if scrim_order == egui::Order::Foreground { ctx.move_to_top(egui::LayerId::new(scrim_order, egui::Id::new((id, "scrim")))); }   // see modal_full
    // the scrim is the modal layer: egui keeps keyboard / gamepad focus and clicks off everything below it (the page behind), while the dialog above it stays live
    ctx.memory_mut(|m| m.set_modal_layer(egui::LayerId::new(scrim_order, egui::Id::new((id, "scrim")))));
    let width = (width_rem * r).min(screen.width() - 2.0 * r);
    let mut inner = None;
    let hidden = settle_hidden(ctx, id);
    let area = egui::Area::new(egui::Id::new((id, "dialog"))).order(egui::Order::Foreground).anchor(Align2::CENTER_CENTER, Vec2::ZERO).show(ctx, |ui| {
        if hidden { ui.set_opacity(0.0); }
        // An Area hands its content the size it had LAST frame as the room to grow into (a small default on the first), so a scroll area inside it could never grow past the height the dialog had
        // before its content arrived: dialogs opened short and stayed short. Every dialog gets the whole window to grow into.
        ui.set_max_height(ctx.content_rect().height());
        egui::Frame::new().fill(surface()).stroke(Stroke::new(1.0_f32, accent)).corner_radius((0.5 * r) as u8)
            .shadow(egui::Shadow { offset: [0, 0], blur: 20, spread: 0, color: accent.gamma_multiply(0.2) })
            .inner_margin(egui::Margin::same((1.875 * r) as i8))
            .show(ui, |ui| {
                ui.set_width(width - 3.75 * r);
                ui.spacing_mut().item_spacing = Vec2::new(0.6 * r, 0.4 * r);
                inner = Some(fit_scroll(ui, egui::Id::new((id, "fit")), (screen.height() - 8.0 * r).max(15.0 * r), |ui| body(ui, r)));
            });
    });
    settle_update(ctx, id, area.response.rect.size());
    if scrim_order == egui::Order::Foreground { ctx.move_to_top(egui::LayerId::new(egui::Order::Foreground, egui::Id::new((id, "dialog")))); }
    ModalOut { closed, inner }
}

/// A small `.nav-btn` sized to its text (header buttons): `h` tall, `fill` as in `nav_button`.
pub fn nav_button_sized(ui: &mut Ui, text: &str, h: f32, r: f32, fill: Option<(Color32, Color32)>) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(text.to_uppercase(), crate::theme::bold(1.0 * r), Color32::PLACEHOLDER);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(galley.size().x + 2.0 * r, h), Sense::click());
    let hot = resp.hovered();
    let (bg, edge, ink) = match fill {
        Some((f, e)) => (if hot && f.a() > 0 { f.gamma_multiply(1.2) } else { f }, e, if f.a() == 0 { e } else { bright() }),
        None => if hot { (accent(), accent(), surface()) } else { (button_fill(), EDGE(), heading()) },
    };
    ui.painter().rect_filled(rect, (0.375 * r) as u8, bg);
    ui.painter().rect_stroke(rect, (0.375 * r) as u8, Stroke::new(1.0_f32, edge), egui::StrokeKind::Inside);
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, ink);
    if hot { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
    focus_mark(ui, &resp, rect);
    resp
}

/// A footer button of a given width and height (the dialog's CLOSE).
pub fn nav_button_sized_w(ui: &mut Ui, text: &str, w: f32, h: f32, r: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, h), Sense::click());
    let hot = resp.hovered();
    let (bg, edge, ink) = if hot { (accent(), accent(), surface()) } else { (surface(), Color32::from_white_alpha(51), Color32::WHITE) };
    ui.painter().rect_filled(rect, (0.25 * r) as u8, bg);
    ui.painter().rect_stroke(rect, (0.25 * r) as u8, Stroke::new(1.0_f32, edge), egui::StrokeKind::Inside);
    let g = ui.painter().layout_no_wrap(text.to_uppercase(), crate::theme::bold(1.0 * r), ink);
    ui.painter().galley(rect.center() - g.size() / 2.0, g, ink);
    if hot { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
    focus_mark(ui, &resp, rect);
    resp
}

/// Set while a slider has the focus: the pad must press A to adjust it (Left/Right would otherwise change its value as the cursor passes over it).
pub static SLIDER_FOCUS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// The value written after a slider (plain text, not focusable).
pub fn slider_value(ui: &mut Ui, text: String, size: f32) { ui.label(egui::RichText::new(text).size(size).color(crate::ui_kit::bright())); }
pub fn add_slider(ui: &mut Ui, slider: egui::Slider) -> egui::Response {
    // no built-in value box: it is a second focusable control on the row, and its arrow keys would change the number as the cursor passed over it; callers draw the value with `slider_value`
    // the pad resting on a slider (the ring outlines the whole control) and the pad adjusting it (the handle lights up) must look different: egui draws a focused handle like a dragged
    // one, so while the pad is only resting there the "active" look is the plain one; a mouse drag or the pad's adjust mode gets the accent
    let adjusting = SLIDER_ADJUSTING.load(std::sync::atomic::Ordering::Relaxed) || ui.input(|i| i.pointer.any_down());
    let resp = ui.scope(|ui| {
        let w = &mut ui.visuals_mut().widgets;
        if adjusting { w.active.bg_fill = accent(); w.active.weak_bg_fill = accent(); w.active.fg_stroke = Stroke::new(2.0_f32, accent()); w.active.expansion = 2.0; }
        else { w.active = w.inactive.clone(); }
        ui.add(slider.show_value(false))
    }).inner;
    if resp.has_focus() { SLIDER_FOCUS.store(true, std::sync::atomic::Ordering::Relaxed); }
    resp
}

/// The pad is adjusting the slider it is on (A pressed on it): set from `App::nav` before the dialogs draw.
pub static SLIDER_ADJUSTING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Set while a multi-line text box has the focus: Up and Down belong to it then, so the pad's Up/Down fallback (Tab) must not fire.
pub static MULTILINE_FOCUS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub fn multiline_mark(resp: egui::Response) -> egui::Response {
    if resp.has_focus() { MULTILINE_FOCUS.store(true, std::sync::atomic::Ordering::Relaxed); }
    resp
}

/// Set by a dialog that is a picture grid: the pad's Up/Down then use egui's geometric search (row to row) instead of stepping through the controls in order.
/// Set by a dialog that opens over another dialog (Crop, the SteamGridDB picker) before it is drawn: its scrim goes on the Foreground layer, above the dialog beneath, so egui's modal
/// layer shuts that dialog out of focus too (with both scrims on `Middle` the pad kept walking the dialog behind).
pub static STACKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub static PAD_GEOMETRY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Dropdown lists longer than this get a search box at the top (PlayDate's custom select did the same above 20 entries).
pub const COMBO_SEARCH_MIN: usize = 15;

/// What a dropdown's search box says: the words typed, and whether Enter was pressed in it (which picks the first match).
pub struct ComboSearch { query: String, pub enter: bool }
impl ComboSearch {
    /// True when the entry should be listed.
    pub fn matches(&self, label: &str) -> bool { self.query.is_empty() || label.to_lowercase().contains(&self.query) }
}

/// Call first thing inside a `ComboBox::show_ui` with the number of entries. Over `COMBO_SEARCH_MIN` entries it draws a "Search..." box (focused when the list opens with the mouse)
/// and returns what was typed; otherwise it draws nothing and everything matches. Not drawn while the gamepad is driving (the pad steps through the entries, and a text box in the
/// list would take its first press).
pub fn combo_search(ui: &mut Ui, id: egui::Id, count: usize, r: f32) -> ComboSearch {
    let none = ComboSearch { query: String::new(), enter: false };
    if count <= COMBO_SEARCH_MIN || ui.ctx().data(|d| d.get_temp::<bool>(egui::Id::new("zest_pad_show"))).unwrap_or(false) { return none; }
    let (qid, seen, opened_id) = (id.with("combo_query"), id.with("combo_seen"), id.with("combo_opened"));
    let now = ui.ctx().cumulative_frame_nr();
    // the list is drawn only while it is open: a gap of frames since it was last drawn means it has just been opened. A frame can be drawn in several passes (the first sizes the
    // popup, with its widgets invisible, and a focus asked for there is lost), so "just opened" lasts two frames and every pass in them asks.
    let last = ui.ctx().data(|d| d.get_temp::<u64>(seen));
    if last.map_or(true, |l| now > l + 2) { ui.ctx().data_mut(|d| { d.insert_temp(opened_id, now); d.insert_temp(qid, String::new()); }); }
    ui.ctx().data_mut(|d| d.insert_temp(seen, now));
    let opened = ui.ctx().data(|d| d.get_temp::<u64>(opened_id)).unwrap_or(now);
    let fresh = now <= opened + 1;
    let mut q: String = ui.ctx().data(|d| d.get_temp::<String>(qid)).unwrap_or_default();
    let resp = ui.add(egui::TextEdit::singleline(&mut q).hint_text("Search...").desired_width(ui.available_width().max(8.0 * r)));
    if fresh { resp.request_focus(); }
    if std::env::var_os("ZEST_PAD_DEBUG").is_some() { eprintln!("combo_search: fresh={fresh} focus={} q={q:?} events={}", resp.has_focus(), ui.input(|i| i.events.len())); }
    let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    ui.ctx().data_mut(|d| d.insert_temp(qid, q.clone()));
    ui.separator();
    ComboSearch { query: q.to_lowercase(), enter }
}
