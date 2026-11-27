//! Settings > Appearance: the theme colors (the same `theme.json` as PlayDate, with its saved themes), the background picture and its opacity, and the UI scale.
//! This only edits files; the app reloads what changed when it gets `AppearanceOut`.
use crate::theme;
use crate::ui_kit::{self, Kind};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};

#[derive(Debug, PartialEq)]
pub enum AppearanceOut { None, /// theme.json changed (colors or opacity)
    Theme, /// the Theme Editor's draft, to show live (nothing is saved)
    Live(Vec<(String, String)>), /// the Background dialog opened: get the picture ready for dragging the opacity
    Prewarm, /// the background picture changed
    Background, /// the scale in percent (75 to 150), to save in state.json
    Scale(u32) }

pub struct Appearance {
    root: PathBuf,
    vars: Vec<(String, String)>,
    saved: Vec<String>,
    /// Each saved theme's surface, accent and text colors, for its row in Choose Theme.
    saved_cols: Vec<Option<(theme::Rgb, theme::Rgb, theme::Rgb)>>,
    name: String,
    scale: f64,
    status: Option<(String, bool)>,
    /// A theme was applied from the Choose Theme list: the dialog closes, as PlayDate's picker does.
    pub done: bool,
    /// The Theme Editor: the active theme's values (what a reset returns to), the preview tab, and the hex box being typed in.
    applied: Vec<(String, String)>,
    tab: usize,
    hex_buf: std::collections::HashMap<String, String>,
    /// The Theme Editor's Advanced group is open.
    adv_open: bool,
    /// The opacity slider while it is dragged: the value is shown about five times a second (the thumb itself follows the mouse every frame), and when it was last shown.
    opacity_pending: bool,
    prewarmed: bool,
    last_live: Option<std::time::Instant>,
    last_change: Option<std::time::Instant>,
    /// A saved theme whose X was pressed once: pressing it again within 3 s deletes it.
    confirm_del: Option<(String, std::time::Instant)>,
    rx: Option<Receiver<(&'static str, Option<PathBuf>)>>,
}

/// The Theme Editor's rows, as PlayDate's `THEME_VAR_META`: variable, group, label, what it colors.
const META: &[(&str, &str, &str, &str)] = &[
    ("--bg-page", "Backgrounds", "Page Background", "Window body behind all surfaces"),
    ("--bg-surface", "Backgrounds", "Surface", "Primary UI surface \u{2014} toolbars, panels"),
    ("--bg-raised", "Backgrounds", "Raised Surface", "Modals, dropdowns, context menus"),
    ("--bg-input", "Backgrounds", "Input Background", "Text fields and inset areas"),
    ("--bg-card", "Backgrounds", "Card Background", "Game cards in grids and shelves"),
    ("--bg-nav", "Backgrounds", "Nav Background", "Top navigation bar"),
    ("--text-primary", "Text", "Body Text", "Main readable text throughout the app"),
    ("--text-heading", "Text", "Heading Text", "Page headings and shelf titles"),
    ("--text-secondary", "Text", "Secondary Text", "Captions, labels, subdued text"),
    ("--text-input", "Text", "Input Text", "Text typed inside fields and selects"),
    ("--text-bright", "Text", "Bright Text", "High-contrast text on colored surfaces"),
    ("--accent", "Accent", "Accent", "Links, focus rings, active highlights"),
    ("--on-accent", "Accent", "On Accent", "Text on accent-colored surfaces"),
    ("--accent-positive", "Accent", "Positive", "Save buttons and success indicators"),
    ("--border", "Borders", "Border", "All panel and input borders"),
    ("--color-danger", "Status", "Danger", "Exit, delete, and destructive action surfaces"),
    ("--text-danger", "Status", "Danger Text", "Red warning and error text"),
    ("--color-warning", "Status", "Warning", "Amber caution indicators"),
    // Zest's extras (PlayDate has no such variables): leave them automatic and they follow the colors above
    ("--bg-hover", "Advanced", "Hover", "Highlight under the pointer or controller in lists and menus"),
    ("--bg-button", "Advanced", "Button", "Toolbar buttons such as Filters and Bulk Edit"),
    ("--divider", "Advanced", "Divider", "Thin lines between sections"),
    ("--selection", "Advanced", "Selection", "Highlight behind selected text"),
    ("--focus-ring", "Advanced", "Focus Ring", "The ring around what the controller or keyboard is on"),
    ("--backdrop", "Advanced", "Backdrop", "The dimming behind dialogs"),
];
fn rgb(s: &str) -> Option<egui::Color32> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 { return None; }
    let v = u32::from_str_radix(s, 16).ok()?;
    Some(egui::Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

/// Which piece of Appearance a dialog shows: saved themes, the color editor, the background picture and its opacity, or the UI scale.
#[derive(Clone, Copy, PartialEq)]
pub enum Part { Theme, Editor, Background, Scale }

impl Appearance {
    pub fn new(root: PathBuf, scale: u32) -> Appearance {
        let vars = theme::read_vars(&root);
        Appearance { vars: vars.clone(), applied: vars, saved_cols: theme::saved_names(&root).iter().map(|n| theme::saved_vars(&root, n).and_then(|v| theme::swatch(&v))).collect(), saved: theme::saved_names(&root), root, name: String::new(), scale: scale.clamp(75, 150) as f64, status: None, done: false, confirm_del: None, adv_open: std::env::var_os("ZEST_ADV_OPEN").is_some(), opacity_pending: false, prewarmed: false, last_live: None, last_change: None, tab: std::env::var("ZEST_PREVIEW_TAB").ok().and_then(|v| v.parse().ok()).unwrap_or(0), hex_buf: Default::default(), rx: None }
    }
    fn set(&mut self, key: &str, v: String) { if let Some(e) = self.vars.iter_mut().find(|(k, _)| k == key) { e.1 = v; } }
    fn get(&self, key: &str) -> &str { self.vars.iter().find(|(k, _)| k == key).map_or("", |(_, v)| v.as_str()) }
    /// A picture was chosen or reset while CLEAR had the opacity at 0: bring it back to full, or the new picture would not show. Saved with the theme.
    fn unclear(&mut self) { if self.get("--bg-image-opacity").parse::<f64>().map_or(false, |o| o <= 0.0) { self.set("--bg-image-opacity", "1".into()); let _ = theme::save_active(&self.root, &self.vars); } }
    fn pick(&mut self, what: &'static str, save_name: Option<String>) {
        let (tx, rx) = channel(); self.rx = Some(rx);
        std::thread::spawn(move || {
            let p = match what {
                "bg" => rfd::FileDialog::new().set_title("Choose a background picture").add_filter("Pictures", &["jpg", "jpeg", "png", "webp"]).pick_file(),
                "import" => rfd::FileDialog::new().set_title("Choose a theme file").add_filter("JSON", &["json"]).pick_file(),
                _ => rfd::FileDialog::new().set_title("Export theme").set_file_name(save_name.unwrap_or_else(|| "theme.json".into())).add_filter("JSON", &["json"]).save_file(),
            };
            let _ = tx.send((what, p));
        });
    }

    fn reload_saved(&mut self) { self.saved = theme::saved_names(&self.root); self.saved_cols = self.saved.iter().map(|n| theme::saved_vars(&self.root, n).and_then(|v| theme::swatch(&v))).collect(); }
    fn applied_val(&self, key: &str) -> String { self.applied.iter().find(|(k, _)| k == key).map_or_else(String::new, |(_, v)| v.clone()) }

    /// The Theme Editor, laid out as PlayDate's: the colors in groups on the left, a live preview on the right. Every change is shown at once (the app takes the draft through
    /// `AppearanceOut::Live`); nothing is saved until SAVE AS..., and closing the dialog drops the draft (`discard`).
    fn editor_ui(&mut self, ui: &mut egui::Ui, r: f32, accent: egui::Color32) -> AppearanceOut {
        let mut changed = false;
        let mut apply = false;
        let mut tab = self.tab;
        ui.columns(2, |cols| {
            // ── left: the colors ──
            {
                let ui = &mut cols[0];
                let mut group = "";
                let now = theme::Theme::from_pairs(&self.vars);
                for (key, grp, label, desc) in META {
                    if *grp != group {
                        group = grp;
                        ui.add_space(if group == "Backgrounds" { 0.0 } else { 0.6 * r });
                        if group == "Advanced" {
                            let h = ui.add(egui::Label::new(egui::RichText::new(format!("{} ADVANCED", if self.adv_open { "\u{25BE}" } else { "\u{25B8}" })).size(0.7 * r).color(ui_kit::MUTED())).sense(egui::Sense::click()));
                            if h.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
                            if h.clicked() { self.adv_open = !self.adv_open; }
                        } else {
                            ui.label(egui::RichText::new(group.to_uppercase()).size(0.7 * r).color(ui_kit::MUTED()));
                        }
                        ui.painter().hline(ui.max_rect().x_range(), ui.cursor().min.y, egui::Stroke::new(1.0_f32, ui_kit::RULE()));
                        ui.add_space(0.3 * r);
                    }
                    if group == "Advanced" && !self.adv_open { continue; }
                    let raw = self.get(key).to_string();
                    // an automatic color shows what it comes out as
                    let cur = if raw.is_empty() { now.effective(key).map(|c| format!("#{:02x}{:02x}{:02x}", c.0, c.1, c.2)).unwrap_or_default() } else { raw.clone() };
                    let applied = self.applied_val(key);
                    let mut new_hex: Option<String> = None;
                    ui.horizontal(|ui| {
                        let c = rgb(&cur).unwrap_or(egui::Color32::from_rgb(0x80, 0, 0x80));
                        let mut arr = [c.r(), c.g(), c.b()];
                        if ui.color_edit_button_srgb(&mut arr).changed() { new_hex = Some(format!("#{:02x}{:02x}{:02x}", arr[0], arr[1], arr[2])); }
                        let label_w = (ui.available_width() - 9.5 * r).max(6.0 * r);
                        ui.allocate_ui_with_layout(egui::vec2(label_w, 2.6 * r), egui::Layout::top_down(egui::Align::Min), |ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            ui.add(egui::Label::new(egui::RichText::new(*label).size(0.88 * r).strong().color(ui_kit::TEXT())).truncate());
                            ui.add(egui::Label::new(egui::RichText::new(*desc).size(0.68 * r).color(ui_kit::MUTED())).truncate());
                        });
                        let mut buf = self.hex_buf.remove(*key).unwrap_or_else(|| cur.clone());
                        let resp = ui.add(egui::TextEdit::singleline(&mut buf).desired_width(5.2 * r).font(egui::FontId::monospace(0.85 * r)));
                        if resp.has_focus() {
                            let t = buf.trim();
                            let ok = t.starts_with('#') && (t.len() == 7 || t.len() == 4) && t[1..].chars().all(|h| h.is_ascii_hexdigit());
                            if ok && !t.eq_ignore_ascii_case(&cur) {
                                new_hex = Some(if t.len() == 4 { let b: Vec<char> = t[1..].chars().collect(); format!("#{0}{0}{1}{1}{2}{2}", b[0], b[1], b[2]) } else { t.to_lowercase() });
                            }
                            self.hex_buf.insert(key.to_string(), buf);
                        }
                        let dim = raw.eq_ignore_ascii_case(&applied);
                        if raw.is_empty() { ui.label(egui::RichText::new("auto").size(0.62 * r).color(ui_kit::MUTED())); }
                        let reset = ui.add(egui::Label::new(egui::RichText::new("\u{21BA}").size(0.95 * r).color(if dim { ui_kit::MUTED().gamma_multiply(0.35) } else { ui_kit::TEXT() })).sense(egui::Sense::click()));
                        if reset.clicked() && !dim { new_hex = Some(applied.clone()); self.hex_buf.remove(*key); }
                    });
                    if let Some(h) = new_hex { self.set(key, h); changed = true; }
                }
            }
            // ── right: preview, export / import, save as ──
            {
                let ui = &mut cols[1];
                ui.label(egui::RichText::new("LIVE PREVIEW").size(0.7 * r).color(ui_kit::MUTED()));
                ui.horizontal_wrapped(|ui| { for (i, name) in ["HOME", "LIBRARY", "PICK 6", "MODALS"].iter().enumerate() { if ui_kit::pill(ui, name, tab == i, accent, 0.75 * r).clicked() { tab = i; } } });
                ui.add_space(0.3 * r);
                preview(ui, r, tab, &self.vars);
                ui.add_space(0.6 * r);
                ui.horizontal(|ui| {
                    if ui_kit::button(ui, "EXPORT", Kind::Plain, accent, r).clicked() { self.pick("export", Some("zest-theme.json".into())); }
                    if ui_kit::button(ui, "IMPORT", Kind::Plain, accent, r).clicked() { self.pick("import", None); }
                    if let Some((m, ok)) = &self.status { ui.label(egui::RichText::new(m).size(0.78 * r).color(if *ok { ui_kit::positive() } else { ui_kit::text_danger() })); }
                });
                ui.add_space(0.5 * r);
                ui.label(egui::RichText::new("SAVE THEME").size(0.7 * r).color(ui_kit::MUTED()));
                ui.add(egui::TextEdit::singleline(&mut self.name).hint_text("Name this theme\u{2026}").desired_width(ui.available_width()).font(egui::FontId::proportional(0.9 * r)));
                ui.add_space(0.3 * r);
                ui.horizontal(|ui| {
                    if ui_kit::button(ui, "SAVE AS\u{2026}", Kind::Plain, accent, r).clicked() {
                        let name = self.name.clone();
                        match theme::save_named(&self.root, &name, &self.vars) { Ok(()) => { self.name.clear(); self.reload_saved(); self.status = Some((format!("Saved as \"{}\". Choose it in Choose Theme to use it.", name.trim()), true)); } Err(e) => self.status = Some((e, false)) }
                    }
                    if ui_kit::button(ui, "SAVE & APPLY", Kind::Primary, accent, r).clicked() { apply = true; }
                });
            }
        });
        self.tab = tab;
        if apply {
            // SAVE & APPLY: under a name too when one is typed, and this becomes the active theme
            let name = self.name.trim().to_string();
            let named = if name.is_empty() { Ok(()) } else { theme::save_named(&self.root, &name, &self.vars) };
            match named.and_then(|_| theme::save_active(&self.root, &self.vars)) {
                Ok(()) => { self.applied = self.vars.clone(); self.reload_saved(); if !name.is_empty() { self.name.clear(); } self.status = Some((if name.is_empty() { "Applied.".to_string() } else { format!("Saved as \"{name}\" and applied.") }, true)); return AppearanceOut::Theme; }
                Err(e) => self.status = Some((e, false)),
            }
        }
        if changed { self.status = None; AppearanceOut::Live(self.vars.clone()) } else { AppearanceOut::None }
    }

    /// Closing the Theme Editor without saving: the draft goes back to the active theme.
    pub fn discard(&mut self) { self.vars = theme::read_vars(&self.root); self.applied = self.vars.clone(); self.hex_buf.clear(); self.status = None; }

    /// Draws the tab. Returns what the app has to reload.
    pub fn show(&mut self, ui: &mut egui::Ui, r: f32, accent: egui::Color32, part: Part) -> AppearanceOut {
        let mut out = AppearanceOut::None;
        if let Some((what, p)) = self.rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.rx = None;
            if let Some(p) = p {
                let res: Result<AppearanceOut, String> = match what {
                    "bg" => theme::set_background(&self.root, &p).map(|_| { self.unclear(); AppearanceOut::Background }),
                    "import" => std::fs::read_to_string(&p).map_err(|e| e.to_string()).and_then(|t| serde_json::from_str::<serde_json::Value>(&t).map_err(|_| "That is not a theme file.".to_string())).and_then(|v| {
                        let src = v.get("playdate_theme").or_else(|| v.get("theme")).unwrap_or(&v);   // PlayDate's export wraps the variables in "playdate_theme"
                        let mut n = 0;
                        for (k, _) in theme::DEFAULTS { if let Some(x) = src.get(*k).and_then(|x| x.as_str()) { self.set(k, x.to_string()); n += 1; } }
                        if n == 0 { Err("That file has no theme colors.".into()) } else { Ok(AppearanceOut::None) }
                    }),
                    _ => { let m: serde_json::Map<String, serde_json::Value> = self.vars.iter().map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone()))).collect(); std::fs::write(&p, serde_json::to_string_pretty(&serde_json::json!({ "playdate_theme": m })).unwrap_or_default()).map_err(|e| e.to_string()).map(|_| AppearanceOut::None) }
                };
                match res { Ok(o) => { if o != AppearanceOut::None { out = o; } self.status = Some((match what { "bg" => "Background set.", "import" => "Imported. Press APPLY COLORS to use it.", _ => "Exported." }.to_string(), true)); } Err(e) => self.status = Some((e, false)) }
            }
        }
        let head = |ui: &mut egui::Ui, t: &str| { ui.label(egui::RichText::new(t).size(1.0 * r).strong().color(crate::ui_kit::heading())); };
        let note = |ui: &mut egui::Ui, t: &str| { ui.label(ui_kit::muted(t, 0.78 * r)); };

        if part == Part::Theme {
        let mut act: Option<(&'static str, String)> = None;
        // a name on a full-width row, a hover fill and (saved themes) an X on the right, as PlayDate's picker draws them; the row is clicked to apply
        let row = |ui: &mut egui::Ui, text: &str, x: Option<(&str, bool)>, cols: Option<(theme::Rgb, theme::Rgb, theme::Rgb)>| -> (bool, bool) {
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 2.3 * r), egui::Sense::click());
            let x_rect = egui::Rect::from_center_size(egui::pos2(rect.max.x - 1.7 * r, rect.center().y), egui::vec2(3.2 * r, 1.9 * r));
            let xr = x.map(|_| ui.interact(x_rect, resp.id.with("x"), egui::Sense::click()));
            let k = |c: theme::Rgb| egui::Color32::from_rgb(c.0, c.1, c.2);
            // a theme is drawn in its own colors: its surface as the row, its accent as the bar at the left, its body text as the name
            let ink = if let Some((surf, acc, txt)) = cols {
                let rr = rect.shrink2(egui::vec2(0.0, 0.15 * r));
                ui.painter().rect_filled(rr, 4.0, k(surf));
                ui.painter().rect_filled(egui::Rect::from_min_size(rr.min, egui::vec2(0.45 * r, rr.height())), egui::CornerRadius { nw: 4, sw: 4, ne: 0, se: 0 }, k(acc));
                if resp.hovered() { ui.painter().rect_stroke(rr, 4.0, egui::Stroke::new(1.5_f32, k(acc)), egui::StrokeKind::Inside); }
                k(txt)
            } else {
                if resp.hovered() { ui.painter().rect_filled(rect, 4.0, ui_kit::INPUT()); }
                ui_kit::TEXT()
            };
            if resp.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
            ui.painter().text(egui::pos2(rect.min.x + 1.2 * r, rect.center().y), egui::Align2::LEFT_CENTER, text, egui::FontId::proportional(0.95 * r), ink);
            if let (Some((label, danger)), Some(xr)) = (x, &xr) {
                ui.painter().text(x_rect.center(), egui::Align2::CENTER_CENTER, label, egui::FontId::proportional(0.85 * r), if danger { ui_kit::text_danger() } else { ui_kit::MUTED() });
                if xr.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
            }
            let x_clicked = xr.as_ref().map_or(false, |x| x.clicked());
            (resp.clicked() && !x_clicked, x_clicked)
        };
        head(ui, "Presets");
        for (name, _) in theme::PRESETS { if row(ui, name, None, theme::preset_swatch(name)).0 { act = Some(("preset", name.to_string())); } }
        if !self.saved.is_empty() {
            ui.add_space(0.5 * r);
            ui.painter().hline(ui.max_rect().x_range(), ui.cursor().min.y, egui::Stroke::new(1.0_f32, ui_kit::EDGE()));
            ui.add_space(0.7 * r);
            head(ui, "Saved themes");
            for (si, n) in self.saved.clone().into_iter().enumerate() {
                let confirming = self.confirm_del.as_ref().map_or(false, |(c, t)| *c == n && t.elapsed().as_secs_f32() < 3.0);
                let (go, x) = row(ui, &n, Some((if confirming { "Delete?" } else { "\u{2715}" }, confirming)), self.saved_cols.get(si).copied().flatten());
                if go { act = Some(("apply", n.clone())); }
                if x { if confirming { act = Some(("delete", n.clone())); self.confirm_del = None; } else { self.confirm_del = Some((n.clone(), std::time::Instant::now())); ui.ctx().request_repaint_after(std::time::Duration::from_millis(3100)); } }
            }
        }
        if let Some((what, n)) = act {
            let res: Result<Option<AppearanceOut>, String> = match what {
                "preset" => match theme::preset_vars(&n, &self.vars) { Some(v) => { self.vars = v; theme::save_active(&self.root, &self.vars).map(|_| Some(AppearanceOut::Theme)) } None => Err("That preset is gone.".into()) },
                "apply" => match theme::saved_vars(&self.root, &n) { Some(v) => { self.vars = v; theme::save_active(&self.root, &self.vars).map(|_| Some(AppearanceOut::Theme)) } None => Err("That theme is gone.".into()) },
                "delete" => theme::delete_named(&self.root, &n).map(|_| None),
                _ => theme::save_named(&self.root, &n, &self.vars).map(|_| { self.name.clear(); None }),
            };
            self.reload_saved();
            match res { Ok(o) => { if let Some(o) = o { out = o; } self.status = Some((match what { "apply" | "preset" => { self.done = true; "Theme applied." } "delete" => "Deleted.", _ => "Saved." }.to_string(), true)); } Err(e) => self.status = Some((e, false)) }
        }
        ui.add_space(0.8 * r);

        }
        if part == Part::Editor {
            let o = self.editor_ui(ui, r, accent);
            if o != AppearanceOut::None { out = o; }
        }
        if part != Part::Background { self.prewarmed = false; }
        if part == Part::Background {
        if !self.prewarmed { self.prewarmed = true; out = AppearanceOut::Prewarm; }
        head(ui, "Background");
        note(ui, "A picture behind every page, faded over the page color.");
        ui.horizontal(|ui| {
            if ui_kit::button(ui, "CHOOSE PICTURE...", Kind::Plain, accent, r).clicked() { self.pick("bg", None); }
            if ui_kit::button(ui, "RESET TO DEFAULT", Kind::Plain, accent, r).clicked() {
                match theme::reset_background(&self.root) { Ok(()) => { self.unclear(); out = AppearanceOut::Background; self.status = Some(("Background reset.".into(), true)); } Err(e) => self.status = Some((e, false)) }
            }
            // just the page color: the picture fades out completely (opacity 0, the same setting PlayDate uses, so a backup keeps it in either program)
            if ui_kit::button(ui, "CLEAR", Kind::Plain, accent, r).clicked() {
                self.set("--bg-image-opacity", "0".into());
                match theme::save_active(&self.root, &self.vars) { Ok(()) => { out = AppearanceOut::Theme; self.status = Some(("Background cleared: only the page color shows. Choose a picture or raise the opacity to bring one back.".into(), true)); } Err(e) => self.status = Some((e, false)) }
            }
        });
        let mut o = self.get("--bg-image-opacity").parse::<f64>().unwrap_or(1.0).clamp(0.0, 1.0);
        ui.horizontal(|ui| {
            ui.label(ui_kit::muted("Opacity", 0.85 * r));
            let resp = ui_kit::add_slider(ui, egui::Slider::new(&mut o, 0.0..=1.0).step_by(0.05).show_value(false));
            ui_kit::slider_value(ui, format!("{}%", (o * 100.0).round() as i64), 0.88 * r);
            if resp.changed() {
                self.set("--bg-image-opacity", format!("{:.2}", o).trim_end_matches('0').trim_end_matches('.').to_string());
                // shown while the slider moves (below), saved when it is let go (or at once for a click or a key)
                if resp.dragged() { self.opacity_pending = true; self.last_change = Some(std::time::Instant::now()); }
                else { match theme::save_active(&self.root, &self.vars) { Ok(()) => out = AppearanceOut::Theme, Err(e) => self.status = Some((e, false)) } }
            }
            // Showing a step redraws the page behind the dialog (about 25 ms at 2560x1400 since the pie charts were made cheap, 100+ ms before), and while that runs the thumb cannot move. So a step
            // is shown when the slider has been still for a moment, or at the latest every 100 ms of a long drag: the thumb and the percentage stay smooth while the hand moves.
            if self.opacity_pending && resp.dragged() {
                const PAUSE: std::time::Duration = std::time::Duration::from_millis(50);
                const LONGEST: std::time::Duration = std::time::Duration::from_millis(100);
                let still = self.last_change.map_or(PAUSE, |t| t.elapsed());
                let since = self.last_live.map_or(LONGEST, |t| t.elapsed());
                if still >= PAUSE || since >= LONGEST { out = AppearanceOut::Live(self.vars.clone()); self.last_live = Some(std::time::Instant::now()); self.opacity_pending = false; }
                else { ui.ctx().request_repaint_after((PAUSE - still).min(LONGEST - since)); }
            }
            if resp.drag_stopped() { self.opacity_pending = false; self.last_live = None; match theme::save_active(&self.root, &self.vars) { Ok(()) => out = AppearanceOut::Theme, Err(e) => self.status = Some((e, false)) } }
        });
        ui.add_space(0.8 * r);

        }
        if part == Part::Scale {
        ui.horizontal(|ui| {
            let resp = ui_kit::add_slider(ui, egui::Slider::new(&mut self.scale, 75.0..=150.0).step_by(5.0).show_value(false));
            ui_kit::slider_value(ui, format!("{}%", self.scale.round() as i64), 0.88 * r);
            if resp.changed() && !resp.dragged() || resp.drag_stopped() { out = AppearanceOut::Scale(self.scale.round() as u32); }
            if ui_kit::pill(ui, "RESET", false, accent, 0.75 * r).clicked() { self.scale = 100.0; out = AppearanceOut::Scale(100); }   // the size of the Home / Library / Pick 6 buttons above it
        });
        }
        if let Some((m, ok)) = &self.status { ui.add_space(0.4 * r); ui.label(egui::RichText::new(m).size(0.85 * r).color(if *ok { egui::Color32::from_rgb(0x8b, 0xb4, 0x2c) } else { crate::ui_kit::text_danger() })); }
        if self.rx.is_some() { ui.ctx().request_repaint_after(std::time::Duration::from_millis(100)); }
        out
    }
}

/// The Theme Editor's preview: a small Home, Library, Pick 6 and dialog drawn with the draft colors (`vars`), as PlayDate's `.tp-*` mock-ups.
fn preview(ui: &mut egui::Ui, r: f32, tab: usize, vars: &[(String, String)]) {
    use egui::{Align2, Color32, FontId, Rect, Stroke, StrokeKind, pos2, vec2};
    let col = |k: &str| -> Color32 { vars.iter().find(|(kk, _)| kk == k).and_then(|(_, v)| rgb(v)).unwrap_or(Color32::from_rgb(0xff, 0, 0xff)) };
    let (page, nav, surface, raised, input, card) = (col("--bg-page"), col("--bg-nav"), col("--bg-surface"), col("--bg-raised"), col("--bg-input"), col("--bg-card"));
    let (text, heading, dim, text_in, bright) = (col("--text-primary"), col("--text-heading"), col("--text-secondary"), col("--text-input"), col("--text-bright"));
    let (accent, on_accent, positive, border, danger, text_danger, warning) = (col("--accent"), col("--on-accent"), col("--accent-positive"), col("--border"), col("--color-danger"), col("--text-danger"), col("--color-warning"));
    let t = theme::Theme::from_pairs(vars);
    let rgbc = |c: theme::Rgb| Color32::from_rgb(c.0, c.1, c.2);
    let (hover, divider, focus, buttonc, selection) = (rgbc(t.hover()), rgbc(t.divider_c()), rgbc(t.focus()), rgbc(t.button()), rgbc(t.selection));
    let backdrop = Color32::from_rgba_unmultiplied(t.backdrop.0, t.backdrop.1, t.backdrop.2, 150);
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(w, 24.0 * r), egui::Sense::hover());
    let p = ui.painter().with_clip_rect(rect);
    p.rect_filled(rect, 4.0, page);
    let f = |s: f32| FontId::proportional(s * r);
    // nav
    let navr = Rect::from_min_size(rect.min, vec2(w, 1.9 * r));
    p.rect_filled(navr, 0.0, nav);
    let active = [0usize, 1, 2, 0][tab.min(3)];
    let mut x = rect.min.x + 0.8 * r;
    for (i, name) in ["Home", "Library", "Pick"].iter().enumerate() {
        let on = i == active;
        let rr = p.text(pos2(x, navr.center().y), Align2::LEFT_CENTER, name, if on { crate::theme::bold(0.62 * r) } else { f(0.62) }, if on { bright } else { dim });
        x = rr.max.x + 0.7 * r;
    }
    let btn = Rect::from_center_size(pos2(navr.max.x - if tab == 0 { 4.6 } else { 1.2 } * r, navr.center().y), vec2(1.5 * r, 1.1 * r));
    p.rect_stroke(btn, 3.0, Stroke::new(1.0_f32, accent), StrokeKind::Inside);
    p.text(btn.center(), Align2::CENTER_CENTER, "\u{2630}", f(0.6), accent);
    if tab == 0 {
        let ex = Rect::from_center_size(pos2(navr.max.x - 1.7 * r, navr.center().y), vec2(2.4 * r, 1.1 * r));
        p.rect_filled(ex, 3.0, danger);
        p.text(ex.center(), Align2::CENTER_CENTER, "Exit", crate::theme::bold(0.58 * r), bright);
    }
    let left = rect.min.x + 0.9 * r;
    let mut y = navr.max.y + 0.9 * r;
    let label = |y: &mut f32, t: &str| { p.text(pos2(left, *y), Align2::LEFT_TOP, t, f(0.55), dim); *y += 1.2 * r; };
    let cards = |y: &mut f32, names: &[&str], accent_second: bool| {
        let mut cx = left;
        for (i, n) in names.iter().enumerate() {
            let c = Rect::from_min_size(pos2(cx, *y), vec2(3.4 * r, 4.4 * r));
            p.rect_filled(c, 3.0, card);
            p.rect_stroke(c, 3.0, Stroke::new(1.0_f32, if accent_second && i == 1 { accent } else { border }), StrokeKind::Inside);
            p.text(pos2(c.min.x + 0.3 * r, c.max.y - 0.3 * r), Align2::LEFT_BOTTOM, n, f(0.48), dim);
            cx += 3.8 * r;
        }
        *y += 5.0 * r;
    };
    let button = |at: egui::Pos2, t: &str, fill: Color32, ink: Color32, edge: Option<Color32>| -> f32 {
        let g = p.layout_no_wrap(t.to_string(), crate::theme::bold(0.55 * r), ink);
        let b = Rect::from_min_size(at, vec2(g.size().x + 1.4 * r, 1.4 * r));
        p.rect_filled(b, 3.0, fill);
        if let Some(e) = edge { p.rect_stroke(b, 3.0, Stroke::new(1.0_f32, e), StrokeKind::Inside); }
        p.galley(pos2(b.min.x + 0.7 * r, b.center().y - g.size().y / 2.0), g, ink);
        b.width()
    };
    match tab {
        0 => {
            p.text(pos2(left, y), Align2::LEFT_TOP, "Your library \u{2014} 1,234 games tracked", f(0.85), text); y += 1.6 * r;
            label(&mut y, "RECENTLY ADDED"); cards(&mut y, &["Game 1", "Game 2", "Game 3", "Game 4"], false);
            label(&mut y, "INSTALLED"); cards(&mut y, &["Game 5", "Game 6", "Game 7"], false);
        }
        1 => {
            let strip = Rect::from_min_size(pos2(rect.min.x, y - 0.6 * r), vec2(w, 5.0 * r));
            p.rect_filled(strip, 0.0, surface);
            p.hline(strip.x_range(), strip.max.y, Stroke::new(1.0_f32, divider));
            let inp = Rect::from_min_size(pos2(left, y), vec2(w * 0.45, 1.5 * r));
            p.rect_filled(inp, 3.0, input); p.rect_stroke(inp, 3.0, Stroke::new(1.0_f32, border), StrokeKind::Inside);
            p.text(pos2(inp.min.x + 0.5 * r, inp.center().y), Align2::LEFT_CENTER, "Search games\u{2026}", f(0.6), dim);
            let w1 = button(pos2(inp.max.x + 0.4 * r, y + 0.05 * r), "Search", accent, on_accent, None);
            button(pos2(inp.max.x + 0.8 * r + w1, y + 0.05 * r), "\u{2715} Clear", Color32::TRANSPARENT, dim, Some(border));
            y += 2.2 * r;
            let a = button(pos2(left, y), "Filters", buttonc, text, Some(border));
            let b = button(pos2(left + a + 0.4 * r, y), "Bulk Edit", buttonc, text, Some(border));
            button(pos2(left + a + b + 0.8 * r, y), "Select (3)", accent, on_accent, None);
            y += 3.2 * r;
            label(&mut y, "SHOWING 847 GAMES"); cards(&mut y, &["Game A", "Game B", "Game C", "Game D"], false);
            let nb = Rect::from_min_size(pos2(left, y - 0.4 * r), vec2(w - 1.8 * r, 1.5 * r));
            p.rect_filled(nb, 3.0, danger.gamma_multiply(0.3)); p.rect_stroke(nb, 3.0, Stroke::new(1.0_f32, danger), StrokeKind::Inside);
            p.text(pos2(nb.min.x + 0.5 * r, nb.center().y), Align2::LEFT_CENTER, "\u{2718} Filter error: unknown column", f(0.6), text_danger);
        }
        2 => {
            p.text(pos2(left, y), Align2::LEFT_TOP, "Pick 6", crate::theme::bold(1.0 * r), heading); y += 1.7 * r;
            label(&mut y, "YOUR PICKS FOR TODAY"); cards(&mut y, &["Game A", "Game B", "Game C", "Game D"], true);
            let a = button(pos2(left, y - 0.3 * r), "Pick Again", accent, on_accent, None);
            p.text(pos2(left + a + 0.6 * r, y + 0.4 * r), Align2::LEFT_CENTER, "or lock picks", f(0.55), dim);
            y += 2.0 * r;
            let bx = Rect::from_min_size(pos2(left, y), vec2(w - 1.8 * r, 4.0 * r));
            p.rect_filled(bx, 3.0, raised); p.rect_stroke(bx, 3.0, Stroke::new(1.0_f32, border), StrokeKind::Inside);
            p.text(pos2(bx.min.x + 0.6 * r, bx.min.y + 0.5 * r), Align2::LEFT_TOP, "Score breakdown", f(0.55), dim);
            for (i, (name, frac)) in [("Taste match", 0.8f32), ("Review score", 0.65)].iter().enumerate() {
                let by = bx.min.y + (1.9 + i as f32 * 1.0) * r;
                let track = Rect::from_min_size(pos2(bx.min.x + 0.6 * r, by), vec2(5.0 * r, 0.4 * r));
                p.rect_filled(track, 2.0, input);
                p.rect_filled(Rect::from_min_size(track.min, vec2(track.width() * frac, track.height())), 2.0, accent);
                p.text(pos2(track.max.x + 0.5 * r, by + 0.2 * r), Align2::LEFT_CENTER, name, f(0.5), dim);
            }
        }
        _ => {
            let body = Rect::from_min_max(pos2(rect.min.x, navr.max.y), rect.max);
            p.rect_filled(body, 0.0, backdrop);
            let bx = Rect::from_center_size(body.center(), vec2((w * 0.7).min(18.0 * r), 18.5 * r));
            p.rect_filled(bx, 4.0, raised); p.rect_stroke(bx, 4.0, Stroke::new(1.0_f32, border), StrokeKind::Inside);
            let l = bx.min.x + 0.9 * r; let fw = bx.width() - 1.8 * r; let mut by = bx.min.y + 0.8 * r;
            p.text(pos2(l, by), Align2::LEFT_TOP, "Edit Game", crate::theme::bold(0.95 * r), heading); by += 1.9 * r;
            p.hline(bx.x_range(), by - 0.3 * r, Stroke::new(1.0_f32, divider)); by += 0.4 * r;
            let field = |by: f32, t: &str, ink: Color32| -> Rect { let fr = Rect::from_min_size(pos2(l, by), vec2(fw, 1.5 * r)); p.rect_filled(fr, 3.0, input); p.rect_stroke(fr, 3.0, Stroke::new(1.0_f32, border), StrokeKind::Inside); p.text(pos2(fr.min.x + 0.5 * r, fr.center().y), Align2::LEFT_CENTER, t, f(0.6), ink); fr };
            field(by, "Completion Status", text_in); by += 1.7 * r;
            // a list under the field: the first row is the one under the pointer
            let lr = Rect::from_min_size(pos2(l, by), vec2(fw, 3.0 * r));
            p.rect_filled(lr, 3.0, raised); p.rect_stroke(lr, 3.0, Stroke::new(1.0_f32, border), StrokeKind::Inside);
            let r1 = Rect::from_min_size(lr.min + vec2(0.1 * r, 0.1 * r), vec2(fw - 0.2 * r, 1.35 * r));
            p.rect_filled(r1, 2.0, hover);
            p.text(pos2(r1.min.x + 0.5 * r, r1.center().y), Align2::LEFT_CENTER, "Beaten", f(0.6), text);
            p.text(pos2(r1.min.x + 0.5 * r, r1.center().y + 1.4 * r), Align2::LEFT_CENTER, "Never Played", f(0.6), text);
            by += 3.5 * r;
            // a field with some of its text selected
            let fr = field(by, "Action, ", text_in);
            let g1 = p.layout_no_wrap("Action, ".to_string(), f(0.6), text_in);
            let g2 = p.layout_no_wrap("Indie".to_string(), f(0.6), text_in);
            let sx = fr.min.x + 0.5 * r + g1.size().x;
            p.rect_filled(Rect::from_min_size(pos2(sx, fr.center().y - g2.size().y / 2.0 - 1.0), vec2(g2.size().x, g2.size().y + 2.0)), 0.0, selection);
            p.galley(pos2(sx, fr.center().y - g2.size().y / 2.0), g2, text_in);
            by += 2.4 * r;
            // Save has the controller's focus ring; Cancel is a plain button
            let s = button(pos2(l, by), "Save", positive, bright, None);
            p.rect_stroke(Rect::from_min_size(pos2(l, by), vec2(s, 1.4 * r)).expand(2.0), 4.0, Stroke::new(2.0_f32, focus), StrokeKind::Outside);
            button(pos2(l + s + 0.9 * r, by), "Cancel", buttonc, dim, Some(border)); by += 2.3 * r;
            p.text(pos2(l, by), Align2::LEFT_TOP, "\u{2718} Error example", f(0.55), text_danger); by += 1.1 * r;
            p.text(pos2(l, by), Align2::LEFT_TOP, "\u{26A0} Warning example", f(0.55), warning);
        }
    }
    p.rect_stroke(rect, 4.0, Stroke::new(1.0_f32, border), StrokeKind::Inside);
}
