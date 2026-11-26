//! The Library Filters dialog (simple mode), laid out from the web one (`templates/modal_filters.html`, captured in headless Chromium).
//! Not in this version: the Advanced tab (nested groups, expressions, custom SQL), comparing one column with another, and the Saved Filter field.
//! A filter that needs those is left untouched unless the user changes something.

use crate::filter_model::{self as m, FromTree, Kind, Opts, Row};
use crate::ui_kit::{self, MUTED, TEXT};
use egui::{Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, Ui, Vec2};
use serde_json::{json, Value};

const SEGMENT_BG: Color32 = Color32::from_rgb(0x0e, 0x19, 0x23);
const SEGMENT_EDGE: Color32 = Color32::from_rgb(0x2a, 0x3f, 0x55);

/// What the dialog asks its owner to do.
pub enum FilterOut {
    None,
    Close,
    /// Apply: `tree` is None when an advanced filter was left alone; the hidden libraries are always saved.
    Apply { tree: Option<Value>, hidden: Vec<String> },
    /// A quick filter: apply this tree now (and the libraries as set).
    Quick { tree: Value, hidden: Vec<String> },
    /// The saved filters changed (the whole map, as it goes into state.json), and maybe the active filter pointer with them.
    SavedFilters { map: Value, filter_tree: Option<Value> },
    Toast(String),
    /// The header link: close this dialog and open the PAGYWOSG filter builder.
    OpenPag,
}

#[derive(Clone)]
enum Sub { Save, Rename, Delete }

pub struct FilterDialog {
    or: bool,
    rows: Vec<Row>,
    hidden: Vec<String>,
    platforms: Vec<String>,
    saved: serde_json::Map<String, Value>,
    selected: String,
    loaded_name: Option<String>,
    loaded_tree: Option<Value>,
    /// The tree this dialog opened with could not be shown as rows: Apply keeps it as it is until something is edited.
    unsupported: Option<&'static str>,
    want_pag: bool,
    show_sql: bool,
    sub: Option<(Sub, String)>,
    uniques: [Vec<String>; 4],
    adv_mode: bool,
    adv: m::Group,
    custom_sql: String,
    sql_open: bool,
    /// CLEAR ALL was pressed since the last look: the page empties its search box too (it is only a row here until Apply).
    cleared: bool,
    salt: u64,   // distinguishes the widget ids of rows drawn one after another through `rows[0]` in the advanced builder
}

fn unique_index(col: &str) -> usize { match col { "tags" => 0, "groups" => 1, "genres" => 2, _ => 3 } }

impl FilterDialog {
    pub fn new(tree: Option<&Value>, saved: Value, hidden: Vec<String>, platforms: Vec<String>, uniques: [Vec<String>; 4], search: &str) -> FilterDialog {
        let saved = saved.as_object().cloned().unwrap_or_default();
        let mut d = FilterDialog { or: false, rows: Vec::new(), hidden, platforms, saved, selected: String::new(), loaded_name: None, loaded_tree: None, unsupported: None, want_pag: false, show_sql: false, sub: None, uniques, salt: 0, adv_mode: false, adv: m::Group::default(), custom_sql: String::new(), sql_open: false, cleared: false };
        // the search box's words are a "Game Title contains" row here, as on the web page (a saved filter or nothing at all still gets one)
        let none = json!({"type": "group", "logic": "AND", "items": []});
        let Some(tree) = tree.or_else(|| (!search.trim().is_empty()).then_some(&none)) else { return d };
        // a pointer to a saved filter shows that filter's tree
        let resolved: Option<Value> = match tree.get("saved_filter").and_then(|v| v.as_str()) {
            Some(name) => { d.loaded_name = Some(name.to_string()); d.selected = name.to_string(); d.saved.get(name).map(|e| e.get("tree").cloned().unwrap_or_else(|| e.clone())) }
            None => Some(tree.clone()),
        };
        if let Some(t) = resolved { d.show_tree(&m::with_search(&t, search), true); }
        // a filter the simple builder cannot show (nested groups, an expression, custom SQL, a game list...) opens on the Advanced tab
        if d.unsupported.is_some() { d.set_mode(true); }
        d
    }

    pub fn take_cleared(&mut self) -> bool { std::mem::take(&mut self.cleared) }

    /// Three example rows (for headless screenshots: `--filters --filter-demo`).
    pub fn demo(&mut self) {
        let t = json!({"type": "group", "logic": "AND", "items": [
            {"type": "condition", "column": "tags", "operator": "LIKE", "value": "Indie"},
            {"type": "condition", "column": "playtime_forever", "operator": ">", "value": "60"},
            {"type": "condition", "column": "completion_status", "operator": "=", "value": "Never Played"},
            {"type": "condition", "column": "unlocked_achievements", "operator": ">=", "value_col": "total_achievements"},
            {"type": "saved_filter_ref", "id": "x", "name": "Backlog", "negate": true}]});
        self.show_tree(&t, false);
        if std::env::var_os("ZEST_DEMO_ADV").is_some() {
            let t2 = json!({"type": "group", "logic": "AND", "items": [{"type": "condition", "column": "installed", "operator": "=", "value": "1"},
                {"type": "group", "logic": "OR", "items": [{"type": "condition", "column": "tags", "operator": "LIKE", "value": "RPG"}, {"type": "condition", "column": "playtime_forever", "operator": ">", "value": "600"}]},
                {"type": "custom_expr", "sql": "name LIKE 'a%'"}], "custom_sql": "playtime_forever > 60"});
            self.show_tree(&t2, true);
            self.set_mode(true);
        }
    }

    fn show_tree(&mut self, tree: &Value, keep_loaded: bool) {
        if keep_loaded { self.loaded_tree = Some(tree.clone()); }
        match m::rows_from_tree(tree) {
            FromTree::Rows { or, rows } => { self.or = or; self.rows = rows; self.unsupported = None; }
            FromTree::Unsupported(why) => { self.unsupported = Some(why); self.rows.clear(); }
        }
    }

    fn touched(&mut self) { self.unsupported = None; }

    fn current_tree(&self) -> Value { m::tree_from_rows(self.or, &self.rows) }
    /// What Save Current stores: the advanced builder's tree (with its custom SQL and PAGYWOSG markers) in advanced mode, an advanced filter left untouched as it was loaded,
    /// else the simple rows. (It used to take the simple rows always, so an advanced filter looked empty and was never saved.)
    fn save_tree(&self) -> Value {
        if self.adv_mode { self.adv_tree() } else if self.unsupported.is_some() { self.loaded_tree.clone().unwrap_or_else(|| self.current_tree()) } else { self.current_tree() }
    }

    pub(crate) fn input(ui: &mut Ui, s: &mut String, w: f32, r: f32, hint: &str) -> egui::Response {
        ui.scope(|ui| {
            ui_kit::style_fields(ui);
            ui.visuals_mut().extreme_bg_color = crate::ui_kit::INPUT();
            { let wd = &mut ui.visuals_mut().widgets; for x in [&mut wd.inactive, &mut wd.hovered, &mut wd.active] { x.bg_stroke = Stroke::new(1.0_f32, crate::ui_kit::EDGE()); } }
            ui.add(egui::TextEdit::singleline(s).desired_width(w).hint_text(RichText::new(hint).size(0.85 * r).color(MUTED())).font(FontId::proportional(0.95 * r)).margin(egui::Margin::symmetric((0.6 * r) as i8, (0.55 * r) as i8)))
        }).inner
    }

    pub(crate) fn combo(ui: &mut Ui, id: egui::Id, text: &str, options: &[(String, String)], w: f32, r: f32) -> Option<String> {
        let mut pick = None;
        ui.scope(|ui| {
            ui_kit::style_dropdown(ui, r);
            ui.spacing_mut().interact_size.y = 2.6 * r;
            { let v = ui.visuals_mut(); for x in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] { x.weak_bg_fill = crate::ui_kit::INPUT(); x.bg_fill = crate::ui_kit::INPUT(); x.bg_stroke = Stroke::new(1.0_f32, crate::ui_kit::EDGE()); } }
            egui::ComboBox::from_id_salt(id).width(w - 0.3 * r).height(22.0 * r).selected_text(RichText::new(text).size(0.95 * r).color(TEXT())).show_ui(ui, |ui| {
                let s = ui_kit::combo_search(ui, id, options.len(), r);
                let mut n = 0;
                for (v, l) in options.iter().filter(|(_, l)| s.matches(l)) {
                    let hit = ui.selectable_label(l == text, l).clicked() || (s.enter && n == 0);
                    n += 1;
                    if hit { pick = Some(v.clone()); if s.enter { ui.close(); } }
                }
                if n == 0 { ui.label(ui_kit::muted("Nothing matches", 0.85 * r)); }
            });
        });
        pick
    }

    fn segment(ui: &mut Ui, labels: [&str; 2], on: usize, r: f32, accent: Color32) -> Option<usize> {
        let mut hit = None;
        ui.spacing_mut().item_spacing.x = 0.0;
        let sizes: Vec<egui::Vec2> = labels.iter().map(|l| ui.painter().layout_no_wrap(l.to_string(), crate::theme::bold(0.8 * r), Color32::WHITE).size() + Vec2::new(1.6 * r, 0.8 * r)).collect();
        let total = Vec2::new(sizes.iter().map(|s| s.x).sum::<f32>(), sizes.iter().map(|s| s.y).fold(0.0, f32::max));
        let (rect, _) = ui.allocate_exact_size(total, Sense::hover());
        ui.painter().rect_filled(rect, (0.3 * r) as u8, SEGMENT_BG);
        let mut x = rect.min.x;
        for (i, l) in labels.iter().enumerate() {
            let cell = Rect::from_min_size(Pos2::new(x, rect.min.y), Vec2::new(sizes[i].x, total.y));
            let resp = ui.interact(cell, ui.id().with(("seg", *l)), Sense::click());
            if on == i { ui.painter().rect_filled(cell, 0.0, accent); }
            let ink = if on == i { Color32::from_rgb(0x0e, 0x19, 0x23) } else if resp.hovered() { TEXT() } else { MUTED() };
            let g = ui.painter().layout_no_wrap(l.to_string(), crate::theme::bold(0.8 * r), ink);
            ui.painter().galley(cell.center() - g.size() / 2.0, g, ink);
            if resp.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
            if resp.clicked() { hit = Some(i); }
            x += sizes[i].x;
        }
        ui.painter().rect_stroke(rect, (0.3 * r) as u8, Stroke::new(1.0_f32, SEGMENT_EDGE), egui::StrokeKind::Inside);
        hit
    }

    /// A rounded quick-filter pill (`#0e1923`, hover accent).
    fn pill(ui: &mut Ui, text: &str, r: f32, accent: Color32) -> egui::Response {
        let g = ui.painter().layout_no_wrap(text.to_string(), FontId::proportional(0.78 * r), Color32::PLACEHOLDER);
        let (rect, resp) = ui.allocate_exact_size(g.size() + Vec2::new(1.5 * r, 0.5 * r), Sense::click());
        let hot = resp.hovered();
        ui.painter().rect_filled(rect, rect.height() / 2.0, SEGMENT_BG);
        ui.painter().rect_stroke(rect, rect.height() / 2.0, Stroke::new(1.0_f32, if hot { accent } else { SEGMENT_EDGE }), egui::StrokeKind::Inside);
        ui.painter().galley(rect.center() - g.size() / 2.0, g, if hot { accent } else { MUTED() });
        if hot { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
        resp
    }

    fn small_label(ui: &mut Ui, text: &str, r: f32) { ui.label(RichText::new(text.to_uppercase()).size(0.75 * r).color(Color32::from_rgb(0x4a, 0x6a, 0x8a))); }

    fn option_list(&self, fd: &m::Field) -> Vec<(String, String)> {
        match fd.opts {
            Opts::Static(list) => list.iter().map(|(v, l)| (v.to_string(), l.to_string())).collect(),
            Opts::Platforms => self.platforms.iter().map(|p| (p.clone(), crate::home::platform_label(p))).collect(),
            Opts::Unique(c) => self.uniques[unique_index(c)].iter().map(|s| (s.clone(), s.clone())).collect(),
            Opts::Saved => {
                let mut v: Vec<(String, String)> = self.saved.iter().filter(|(n, _)| Some(n.as_str()) != self.loaded_name.as_deref())
                    .filter_map(|(n, e)| e.get("id").and_then(|i| i.as_str()).map(|i| (i.to_string(), n.clone()))).collect();
                v.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));
                v
            }
            Opts::None => Vec::new(),
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, sql_preview: &dyn Fn(&Value) -> String) -> FilterOut {
        let accent = ui_kit::accent();
        let mut out = FilterOut::None;
        let mut out_body = FilterOut::None;   // what the scrolling part (the quick filters) chose
        let r = ui_kit::rem(ctx);
        let screen_h = ctx.content_rect().height();
        let mut touched = false;
        let mut want_sub: Option<Sub> = None;
        let cell = std::cell::RefCell::new(&mut *self);
        let res = ui_kit::modal_full(ctx, "filters", "Library Filters", 48.75,
            |ui, r| {
                // header end: Simple / Advanced
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    let g = ui.painter().layout_no_wrap("Advanced".into(), FontId::proportional(0.8 * r), Color32::WHITE).size();
                    let adv_now = cell.borrow().adv_mode;
                    for (label, is_adv) in [("Advanced", true), ("Simple", false)] {
                        let w = if is_adv { g.x + 1.75 * r } else { g.x + 1.4 * r };
                        let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 1.9 * r), Sense::click());
                        let on = adv_now == is_adv;
                        if on { ui.painter().rect_filled(rect, (0.375 * r) as u8, accent); }
                        ui.painter().text(rect.center(), Align2::CENTER_CENTER, label, if on { crate::theme::bold(0.8 * r) } else { FontId::proportional(0.8 * r) }, if on { Color32::from_rgb(0x0e, 0x19, 0x23) } else { MUTED() });
                        if resp.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
                        if resp.clicked() && !on { cell.borrow_mut().set_mode(is_adv); }
                    }
                });
                // the PAGYWOSG link sits beside the title, as in PlayDate
                let tw = ui.painter().layout_no_wrap("Library Filters".into(), crate::theme::bold(1.28 * r), Color32::WHITE).size().x;
                let area = ui.max_rect();
                let link_rect = egui::Rect::from_min_size(egui::Pos2::new(area.min.x + tw + 1.2 * r, area.center().y - 0.7 * r), Vec2::new(14.0 * r, 1.4 * r));
                let link = ui.put(link_rect, egui::Label::new(RichText::new("PAGYWOSG Filter Builder \u{2192}").size(0.78 * r).color(accent)).sense(Sense::click()));
                if link.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
                if link.clicked() { cell.borrow_mut().want_pag = true; }
            },
            |ui, r| {
                let me = &mut **cell.borrow_mut();
                ui.spacing_mut().item_spacing = Vec2::new(0.6 * r, 0.5 * r);
                if let Some(why) = me.unsupported {
                    egui::Frame::new().fill(Color32::from_rgb(0x2d, 0x24, 0x10)).stroke(Stroke::new(1.0_f32, Color32::from_rgb(0x7a, 0x65, 0x30))).corner_radius((0.3 * r) as u8).inner_margin(egui::Margin::symmetric((0.8 * r) as i8, (0.6 * r) as i8)).show(ui, |ui| {
                        ui.label(RichText::new(format!("The current filter uses {why}, which this builder cannot show yet. It stays as it is until you change something here or pick a quick filter.")).size(0.82 * r).color(Color32::from_rgb(0xc8, 0xa0, 0x50)));
                    });
                    ui.add_space(0.3 * r);
                }
                if me.adv_mode { me.advanced_ui(ui, r, accent, screen_h, &mut touched); } else {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Combine filters with").size(0.82 * r).color(MUTED()));
                    if let Some(i) = Self::segment(ui, ["AND", "OR"], me.or as usize, r, accent) { me.or = i == 1; touched = true; }
                    ui.label(RichText::new(if me.or { "Any condition can match" } else { "All conditions must match" }).size(0.78 * r).italics().color(Color32::from_rgb(0x66, 0x66, 0x66)));
                });
                ui.add_space(0.4 * r);
                let max_h = (screen_h * 0.82 - 28.0 * r).max(8.0 * r);
                egui::ScrollArea::vertical().max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
                    let w = ui.available_width();
                    let (cw, ow) = (17.1 * r, 8.1 * r);
                    let vw = w - cw - ow - 2.4 * r - 3.0 * 0.6 * r;
                    let mut remove = None;
                    let mut changed = false;
                    for i in 0..me.rows.len() {
                        let id = ui.id().with(("fm_row", i));
                        let (ch, rm) = me.row_line(ui, id, i, cw, ow, vw, r, accent);
                        if ch { changed = true; }
                        if rm { remove = Some(i); }
                    }
                    if let Some(i) = remove { me.rows.remove(i); changed = true; }
                    if changed { touched = true; }
                    // + Filter
                    let g = ui.painter().layout_no_wrap("+ Filter".into(), FontId::proportional(0.85 * r), Color32::PLACEHOLDER);
                    let (rect, resp) = ui.allocate_exact_size(g.size() + Vec2::new(2.0 * r, 0.8 * r), Sense::click());
                    let hot = resp.hovered();
                    let c = if hot { accent } else { Color32::from_rgb(0x3a, 0x55, 0x70) };
                    let pts = [rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom(), rect.left_top()];
                    for sh in egui::Shape::dashed_line(&pts, Stroke::new(1.0_f32, c), 3.0, 3.0) { ui.painter().add(sh); }
                    ui.painter().galley(rect.center() - g.size() / 2.0, g, if hot { accent } else { MUTED() });
                    if hot { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
                    if resp.clicked() { me.rows.push(Row::new("tags")); touched = true; }
                });
                // SQL preview
                ui.add_space(0.2 * r);
                let head = ui.add(egui::Label::new(RichText::new(format!("{} Show SQL Preview", if me.show_sql { "\u{25BE}" } else { "\u{25B8}" })).size(0.78 * r).color(MUTED())).sense(Sense::click()));
                if head.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
                if head.clicked() { me.show_sql = !me.show_sql; }
                if me.show_sql {
                    egui::Frame::new().fill(Color32::from_rgb(0x0b, 0x15, 0x20)).stroke(Stroke::new(1.0_f32, Color32::from_rgb(0x1e, 0x33, 0x48))).corner_radius((0.3 * r) as u8).inner_margin(egui::Margin::symmetric((0.9 * r) as i8, (0.6 * r) as i8)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new(sql_preview(&me.current_tree())).size(0.78 * r).monospace().color(accent));
                    });
                }
                }
                // everything below the conditions scrolls with them; only the Apply row stays put
                ui.add_space(0.3 * r);
                ui.separator();
                let (general, status) = m::quick_filters();
                ui.horizontal_wrapped(|ui| {
                    Self::small_label(ui, "Quick:", r);
                    for (label, tree) in &general { if Self::pill(ui, label, r, accent).clicked() { out_body = FilterOut::Quick { tree: tree.clone(), hidden: me.hidden.clone() }; } }
                });
                ui.horizontal_wrapped(|ui| {
                    Self::small_label(ui, "Status:", r);
                    for (label, tree) in &status { if Self::pill(ui, label, r, accent).clicked() { out_body = FilterOut::Quick { tree: tree.clone(), hidden: me.hidden.clone() }; } }
                });
                // saved filters
                ui.horizontal(|ui| {
                    let names: Vec<(String, String)> = me.saved.keys().map(|k| (k.clone(), k.clone())).collect();
                    let show = if me.selected.is_empty() { "Saved filters...".to_string() } else { me.selected.clone() };
                    let btn_w = 3.0 * (6.5 * r) + 2.0 * 0.5 * r;
                    if let Some(p) = Self::combo(ui, egui::Id::new("fm_saved"), &show, &names, ui.available_width() - btn_w - 1.0 * r, r) {
                        me.selected = p.clone();
                        if let Some(e) = me.saved.get(&p) {
                            let tree = e.get("tree").cloned().unwrap_or_else(|| e.clone());
                            me.loaded_name = Some(p);
                            me.show_tree(&tree, true);
                            me.unsupported = if let FromTree::Unsupported(w) = m::rows_from_tree(&tree) { Some(w) } else { None };
                            // a filter the simple builder cannot show opens the advanced one; in advanced mode the picked filter replaces what is there
                            if me.unsupported.is_some() { me.set_mode(true); }
                            else if me.adv_mode { me.adv = m::Group::from_tree(&tree); me.custom_sql = tree.get("custom_sql").and_then(|v| v.as_str()).unwrap_or("").to_string(); }
                        }
                    }
                    if ui_kit::nav_button_sized(ui, "\u{270E} RENAME", 2.25 * r, r * 0.8, None).clicked() && !me.selected.is_empty() { want_sub = Some(Sub::Rename); }
                    if ui_kit::nav_button_sized(ui, "\u{2715} DELETE", 2.25 * r, r * 0.8, None).clicked() && !me.selected.is_empty() { want_sub = Some(Sub::Delete); }
                    if ui_kit::nav_button_sized(ui, "\u{2605} SAVE CURRENT", 2.25 * r, r * 0.8, None).clicked() { want_sub = Some(Sub::Save); }
                });
                // libraries
                let (rule, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
                ui.painter().rect_filled(rule, 0.0, ui_kit::RULE());
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Libraries").size(0.82 * r).color(MUTED()));
                    let all_on = me.hidden.iter().all(|h| !me.platforms.contains(h));
                    if ui_kit::nav_button_sized(ui, if all_on { "None" } else { "All" }, 2.0 * r, r * 0.78, None).clicked() { me.hidden = if all_on { me.platforms.clone() } else { Vec::new() }; }
                    for p in me.platforms.clone() {
                        let on = !me.hidden.contains(&p);
                        let label = crate::home::platform_label(&p);
                        let g = ui.painter().layout_no_wrap(label.clone(), FontId::proportional(0.82 * r), Color32::PLACEHOLDER);
                        let (rect, resp) = ui.allocate_exact_size(Vec2::new(g.size().x + 4.2 * r, 2.25 * r), Sense::click());
                        let hot = resp.hovered();
                        ui.painter().rect_filled(rect, (0.375 * r) as u8, if hot { Color32::from_rgb(0x44, 0x44, 0x44) } else { Color32::from_rgb(0x33, 0x33, 0x33) });
                        ui.painter().rect_stroke(rect, (0.375 * r) as u8, Stroke::new(1.0_f32, Color32::from_rgb(0x44, 0x44, 0x44)), egui::StrokeKind::Inside);
                        if on { ui.painter().text(Pos2::new(rect.min.x + 1.1 * r, rect.center().y), Align2::CENTER_CENTER, "\u{2713}", FontId::proportional(0.9 * r), accent); }
                        ui.painter().galley(Pos2::new(rect.min.x + 2.0 * r, rect.center().y - g.size().y / 2.0), g, if on { Color32::WHITE } else { Color32::from_white_alpha(115) });
                        if hot { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
                        if resp.clicked() { if on { me.hidden.push(p.clone()); } else { me.hidden.retain(|h| *h != p); } }
                    }
                });
            },
            |ui, r| {
                let me = &mut **cell.borrow_mut();
                ui.spacing_mut().item_spacing = Vec2::new(0.5 * r, 0.5 * r);
                // action row
                ui.horizontal(|ui| {
                    let gap = ui.spacing().item_spacing.x;
                    let total = ui.available_width() - 2.0 * gap;
                    let (rect, resp) = ui.allocate_exact_size(Vec2::new(total * 0.5, 3.0 * r), Sense::click());
                    let fill = crate::ui_kit::positive();
                    ui.painter().rect_filled(rect, (0.25 * r) as u8, if resp.hovered() { fill.gamma_multiply(1.2) } else { fill });
                    ui.painter().text(rect.center(), Align2::CENTER_CENTER, "APPLY", crate::theme::bold(1.0 * r), Color32::WHITE);
                    if resp.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
                    if resp.clicked() {
                        let tree = if me.adv_mode { Some(me.adv_tree()) } else if me.unsupported.is_some() { None } else { Some(me.apply_tree()) };
                        out = FilterOut::Apply { tree, hidden: me.hidden.clone() };
                    }
                    if ui_kit::nav_button_sized_w(ui, "CLEAR ALL", total * 0.25, 3.0 * r, r).clicked() { me.rows.clear(); me.loaded_name = None; me.loaded_tree = None; me.selected.clear(); me.unsupported = None; me.adv_mode = false; me.adv = m::Group::default(); me.custom_sql.clear(); me.sql_open = false; me.salt += 1; me.cleared = true; }
                    if ui_kit::nav_button_sized_w(ui, "CLOSE", total * 0.25, 3.0 * r, r).clicked() { out = FilterOut::Close; }
                });
            });
        drop(cell);
        if matches!(out, FilterOut::None) { out = out_body; }
        if touched { self.touched(); }
        if std::mem::take(&mut self.want_pag) && matches!(out, FilterOut::None) { out = FilterOut::OpenPag; }
        if let Some(s) = want_sub { self.sub = Some((s.clone(), if matches!(s, Sub::Rename) { self.selected.clone() } else { String::new() })); }
        if res.closed && self.sub.is_none() && matches!(out, FilterOut::None) { out = FilterOut::Close; }
        self.sub_dialog(ctx, r, &mut out);
        out
    }

    /// The tree Apply writes: the rows, or a pointer to the saved filter they came from when nothing was changed.
    fn apply_tree(&self) -> Value {
        let tree = self.current_tree();
        if let (Some(name), Some(loaded)) = (&self.loaded_name, &self.loaded_tree) {
            if m::without_search(loaded) == tree || tree_eq(loaded, &tree) { return json!({"saved_filter": name}); }
        }
        tree
    }

    /// Tag-like values as chips, with the matching known values offered while you type.

    /// Simple <-> Advanced: carry the filter over (what simple cannot show stays on the advanced side with a note).
    fn set_mode(&mut self, adv: bool) {
        if adv {
            let mut tree = if self.unsupported.is_some() { self.loaded_tree.clone().unwrap_or_else(|| self.current_tree()) } else { self.current_tree() };
            // a PAGYWOSG filter is shown the way PlayDate shows it: ordinary groups, conditions and appid lists the user can change (its markers stay with the filter)
            if let Some(spec) = pd_filter::pagywosg_filter::Spec::from_tree(&tree) { tree = pd_filter::pagywosg_filter::python_tree(&spec); self.loaded_tree = Some(tree.clone()); }
            if self.custom_sql.is_empty() { if let Some(cs) = tree.get("custom_sql").and_then(|v| v.as_str()) { self.custom_sql = cs.to_string(); self.sql_open = true; } }
            self.adv = m::Group::from_tree(&tree);
            self.adv_mode = true;
            self.unsupported = None;
        } else {
            let tree = self.adv.to_value();
            match m::rows_from_tree(&tree) { FromTree::Rows { or, rows } => { self.or = or; self.rows = rows; self.unsupported = None; }, FromTree::Unsupported(why) => { self.unsupported = Some(why); self.loaded_tree = Some(tree); self.rows.clear(); } }
            self.adv_mode = false;
        }
    }

    /// The tree Apply writes in advanced mode: the group tree, plus the custom WHERE clause (which overrides it) when there is one.
    fn adv_tree(&self) -> Value {
        let mut t = self.adv.to_value();
        if !self.custom_sql.trim().is_empty() { t["custom_sql"] = json!(self.custom_sql.trim()); }
        // a PAGYWOSG filter keeps its markers (the tooltip's qualifications and the Edit dialog read them), as PlayDate's applyFilters does
        if let Some(l) = &self.loaded_tree { for k in ["pagywosg", "pagywosg_event", "pagywosg_verified", "pagywosg_personal_cats"] { if let Some(x) = l.get(k) { t[k] = x.clone(); } } }
        t
    }

    fn advanced_ui(&mut self, ui: &mut Ui, r: f32, accent: Color32, screen_h: f32, touched: &mut bool) {
        let max_h = (screen_h * 0.82 - 28.0 * r).max(8.0 * r);
        let mut g = std::mem::take(&mut self.adv);
        egui::ScrollArea::vertical().max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
            let w = ui.available_width();
            let mut counter = 0u64;
            if self.group_ui(ui, &mut g, 0, w, r, accent, &mut counter) { *touched = true; }
            ui.add_space(0.5 * r);
            let head = ui.add(egui::Label::new(RichText::new(format!("\u{2328} Custom SQL WHERE Clause {}", if self.sql_open { "\u{25BE}" } else { "\u{25B8}" })).size(0.82 * r).color(if self.custom_sql.trim().is_empty() { MUTED() } else { accent })).sense(Sense::click()));
            if head.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
            if head.clicked() { self.sql_open = !self.sql_open; }
            if self.sql_open {
                egui::Frame::new().fill(Color32::from_rgb(0x0b, 0x15, 0x20)).stroke(Stroke::new(1.0_f32, Color32::from_rgb(0x2a, 0x3f, 0x55))).corner_radius((0.375 * r) as u8).inner_margin(egui::Margin::same((0.75 * r) as i8)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new("Custom WHERE clause").size(0.85 * r).strong().color(Color32::from_rgb(0xc2, 0xc8, 0xcc)));
                    ui.label(RichText::new("overrides the group builder above when not empty").size(0.75 * r).color(Color32::from_rgb(0x66, 0x66, 0x66)));
                    ui.label(RichText::new("SELECT * FROM games WHERE \u{25BC} your expression here ORDER BY \u{2026}").size(0.78 * r).monospace().color(MUTED()));
                    let te = ui_kit::multiline_mark(ui.add(egui::TextEdit::multiline(&mut self.custom_sql).desired_width(f32::INFINITY).desired_rows(3).font(egui::FontId::monospace(0.82 * r))
                        .hint_text("e.g. (completion_status = 'Never Played' OR completion_status = 'Unfinished') AND playtime_forever > 60")));
                    if te.changed() { *touched = true; }
                    let sql = self.custom_sql.trim();
                    if !sql.is_empty() {
                        if !pd_filter::is_safe_sql(sql) { ui.label(RichText::new("\u{26A0} This SQL is not allowed (only WHERE expressions over the known columns); it will be ignored.").size(0.78 * r).color(crate::ui_kit::text_danger())); }
                        else { ui.label(RichText::new("\u{26A0} Custom SQL is active \u{2014} group builder is ignored.").size(0.78 * r).color(crate::ui_kit::warning())); }
                    }
                });
            }
            ui.add_space(0.3 * r);
            ui.label(RichText::new("Not built here yet: \"Parse into builder\" and collapsing groups.").size(0.72 * r).color(Color32::from_rgb(0x66, 0x66, 0x66)));
        });
        self.adv = g;
    }

    /// A node the builder has no row for. A PAGYWOSG filter is editable here (completion statuses, the SteamGifts group, tags, conditions and categories of the event);
    /// a plain list of games is only counted. Returns (changed, remove).
    fn opaque_ui(ui: &mut Ui, v: &mut Value, r: f32, accent: Color32) -> (bool, bool) {
        let x_btn = |ui: &mut Ui| ui.add(egui::Label::new(RichText::new("\u{2715}").size(1.0 * r).color(Color32::from_rgb(0xe0, 0x50, 0x50))).sense(Sense::click())).clicked();
        let (mut changed, mut remove) = (false, false);
        if v.get("type").and_then(|t| t.as_str()) == Some("pag_filter") {
            let mut s = pd_filter::pagywosg_filter::Spec::from_json(&v["spec"]);
            egui::Frame::new().fill(Color32::from_rgb(0x10, 0x1c, 0x28)).stroke(Stroke::new(1.0_f32, Color32::from_rgb(0x3a, 0x5a, 0x7a))).corner_radius((0.4 * r) as u8).inner_margin(egui::Margin::same((0.6 * r) as i8)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("PAGYWOSG filter: {}", if s.event_name.is_empty() { "(no event)" } else { &s.event_name })).size(0.9 * r).strong().color(crate::ui_kit::heading()));
                    if x_btn(ui) { remove = true; }
                });
                let games: usize = s.lists.iter().map(|l| l.all_ids().len()).sum();
                ui.label(RichText::new(format!("{} categories with {} named games. Rebuild it in Community Tools to refresh the event's lists.", s.lists.len(), games)).size(0.75 * r).color(MUTED()));
                Self::small_label(ui, "Keep games that are", r);
                ui.horizontal_wrapped(|ui| {
                    const ALL: [&str; 5] = ["Never Played", "Unfinished", "Beaten", "Completed", "Won't Play"];
                    for st in ALL {
                        let on = s.statuses.is_empty() || s.statuses.iter().any(|x| x == st);
                        if ui_kit::pill(ui, st, on, accent, 0.75 * r).clicked() {
                            let mut cur: Vec<String> = if s.statuses.is_empty() { ALL.iter().map(|x| x.to_string()).collect() } else { s.statuses.clone() };
                            if on { cur.retain(|x| x != st); } else { cur.push(st.to_string()); }
                            s.statuses = if cur.len() == 5 || cur.is_empty() { Vec::new() } else { ALL.iter().filter(|a| cur.iter().any(|c| c == *a)).map(|a| a.to_string()).collect() };
                            changed = true;
                        }
                    }
                });
                Self::small_label(ui, "Group holding your SteamGifts wins (empty = none)", r);
                let mut grp = s.sg_group.clone().unwrap_or_default();
                if ui.add(egui::TextEdit::singleline(&mut grp).desired_width(16.0 * r)).changed() { s.sg_group = Some(grp.trim().to_string()).filter(|g| !g.is_empty()); changed = true; }
                for (title, pool) in [("Wins pool (a SteamGifts win that also matches one of these)", &mut s.wins), ("All-games pool (any game matching one of these)", &mut s.all)] {
                    if pool.tags.is_empty() && pool.conds.is_empty() { continue; }
                    Self::small_label(ui, title, r);
                    let mut drop_tag = None; let mut drop_cond = None;
                    for (i, t) in pool.tags.iter().enumerate() { ui.horizontal(|ui| { ui.label(RichText::new(format!("tag: {t}")).size(0.82 * r).color(TEXT())); if x_btn(ui) { drop_tag = Some(i); } }); }
                    for (i, (c, o, val)) in pool.conds.iter().enumerate() { ui.horizontal(|ui| { ui.label(RichText::new(format!("{c} {o} {val}")).size(0.82 * r).color(TEXT())); if x_btn(ui) { drop_cond = Some(i); } }); }
                    if let Some(i) = drop_tag { pool.tags.remove(i); changed = true; }
                    if let Some(i) = drop_cond { pool.conds.remove(i); changed = true; }
                }
                if !s.lists.is_empty() {
                    Self::small_label(ui, "Categories with named games", r);
                    let mut drop = None;
                    for (i, l) in s.lists.iter().enumerate() { ui.horizontal(|ui| { ui.label(RichText::new(format!("{} ({} games)", l.label, l.all_ids().len())).size(0.82 * r).color(TEXT())); if x_btn(ui) { drop = Some(i); } }); }
                    if let Some(i) = drop { s.lists.remove(i); changed = true; }
                }
            });
            if changed { v["spec"] = s.to_json(); }
        } else {
            let n = v.get("appids").and_then(|a| a.as_array()).map_or(0, |a| a.len());
            let label = v.get("label").and_then(|l| l.as_str()).filter(|l| !l.is_empty());
            let auto = v.get("auto").and_then(|a| a.as_bool()).unwrap_or(false);
            ui.horizontal(|ui| {
                egui::Frame::new().fill(Color32::from_rgb(0x1a, 0x3a, 0x5a)).stroke(Stroke::new(1.0_f32, Color32::from_rgb(0x3a, 0x6a, 0x9a))).corner_radius((0.3 * r) as u8).inner_margin(egui::Margin::symmetric((0.5 * r) as i8, (0.2 * r) as i8))
                    .show(ui, |ui| { ui.label(RichText::new("APPIDS").size(0.72 * r).strong().color(Color32::from_rgb(0x7a, 0xc0, 0xf0))); });
                let ids = format!("{n} {}game ID{}", if auto { "" } else { "verified " }, if n == 1 { "" } else { "s" });
                ui.label(RichText::new(match label { Some(l) => format!("{l} ({ids})"), None => format!("A list of {ids}") }).size(0.8 * r).monospace().color(MUTED()));
                if x_btn(ui) { remove = true; }
            });
        }
        (changed, remove)
    }

    /// One group: items (conditions through `row_line`, expressions, nested groups), the add buttons and the AND/OR toggle. Returns whether anything changed.
    fn group_ui(&mut self, ui: &mut Ui, g: &mut m::Group, depth: usize, w: f32, r: f32, accent: Color32, counter: &mut u64) -> bool {
        let mut changed = false;
        let (edge, fill) = if g.or { (Color32::from_rgb(0x6a, 0x4a, 0x10), Color32::from_rgb(0x1c, 0x17, 0x0d)) } else { (Color32::from_rgb(0x2a, 0x5a, 0x7a), Color32::from_rgb(0x0f, 0x1a, 0x26)) };
        let mut remove = None;
        let mut ungroup = None;
        egui::Frame::new().fill(fill).stroke(Stroke::new(1.0_f32, edge)).corner_radius((0.4 * r) as u8).inner_margin(egui::Margin::same((0.6 * r) as i8)).show(ui, |ui| {
            ui.set_width(w - 1.2 * r - 2.0);
            ui.spacing_mut().item_spacing = Vec2::new(0.6 * r, 0.5 * r);
            let inner = ui.available_width();
            let (cw, ow) = (14.0 * r, 7.5 * r);
            let vw = (inner - cw - ow - 2.4 * r - 3.0 * 0.6 * r).max(6.0 * r);
            let n = g.items.len();
            for k in 0..n {
                if k > 0 {
                    let (txt, col) = if g.or { ("OR", Color32::from_rgb(0xc0, 0x80, 0x30)) } else { ("AND", Color32::from_rgb(0x5a, 0x9a, 0xba)) };
                    ui.label(RichText::new(txt).size(0.7 * r).strong().color(col));
                }
                *counter += 1;
                self.salt = *counter;
                let id = ui.id().with(("adv_row", depth, k, *counter));
                match &mut g.items[k] {
                    m::Node::Cond(row) => {
                        let taken = std::mem::take(row);
                        let saved_rows = std::mem::replace(&mut self.rows, vec![taken]);
                        let mut rm = false;
                        ui.horizontal_top(|ui| { ui.spacing_mut().item_spacing.x = 0.6 * r; let (ch, x) = self.row_line(ui, id, 0, cw, ow, vw, r, accent); if ch { changed = true; } rm = x; });
                        *row = self.rows.pop().unwrap_or_default();
                        self.rows = saved_rows;
                        if rm { remove = Some(k); }
                    }
                    m::Node::Expr(e) => {
                        ui.horizontal_top(|ui| {
                            ui.label(RichText::new("SQL").size(0.75 * r).color(Color32::from_rgb(0x66, 0x66, 0x66)));
                            if ui.add(egui::TextEdit::singleline(e).desired_width(inner - 5.0 * r).font(egui::FontId::monospace(0.85 * r)).hint_text("SQL expression, e.g. playtime_forever > 60")).changed() { changed = true; }
                            let x = ui.add(egui::Label::new(RichText::new("\u{2715}").size(1.1 * r).color(Color32::from_rgb(0xe0, 0x50, 0x50))).sense(Sense::click()));
                            if x.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
                            if x.clicked() { remove = Some(k); }
                        });
                    }
                    m::Node::Group(sub) => {
                        let mut sub_taken = std::mem::take(sub);
                        ui.horizontal(|ui| {
                            let u = ui.add(egui::Label::new(RichText::new("\u{2715} Ungroup").size(0.75 * r).color(MUTED())).sense(Sense::click()));
                            if u.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
                            if u.clicked() { ungroup = Some(k); }
                        });
                        if self.group_ui(ui, &mut sub_taken, depth + 1, inner, r, accent, counter) { changed = true; }
                        if let m::Node::Group(s) = &mut g.items[k] { *s = sub_taken; }
                    }
                    m::Node::Opaque(v) => {
                        let (ch, rm) = Self::opaque_ui(ui, v, r, accent);
                        if ch { changed = true; }
                        if rm { remove = Some(k); }
                    }
                }
            }
            ui.horizontal(|ui| {
                for (label, kind) in [("+ Filter", 0), ("+ Group", 1), ("+ Expression", 2)] {
                    let gl = ui.painter().layout_no_wrap(label.into(), FontId::proportional(0.8 * r), Color32::PLACEHOLDER);
                    let (rect, resp) = ui.allocate_exact_size(gl.size() + Vec2::new(1.6 * r, 0.8 * r), Sense::click());
                    let hot = resp.hovered();
                    ui.painter().rect_filled(rect, (0.25 * r) as u8, Color32::from_rgb(0x1b, 0x3a, 0x52));
                    ui.painter().rect_stroke(rect, (0.25 * r) as u8, Stroke::new(1.0_f32, if hot { accent } else { Color32::from_rgb(0x2a, 0x5a, 0x7a) }), egui::StrokeKind::Inside);
                    ui.painter().galley(rect.center() - gl.size() / 2.0, gl, if hot { Color32::WHITE } else { accent });
                    if hot { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
                    if resp.clicked() {
                        g.items.push(match kind { 0 => m::Node::Cond(Row::new("tags")), 1 => m::Node::Group(m::Group::default()), _ => m::Node::Expr(String::new()) });
                        changed = true;
                    }
                }
                let (txt, col) = if g.or { ("OR", Color32::from_rgb(0xc0, 0x80, 0x30)) } else { ("AND", Color32::from_rgb(0x5a, 0x9a, 0xba)) };
                let gl = ui.painter().layout_no_wrap(txt.into(), crate::theme::bold(0.75 * r), Color32::PLACEHOLDER);
                let (rect, resp) = ui.allocate_exact_size(gl.size() + Vec2::new(1.4 * r, 0.8 * r), Sense::click());
                ui.painter().rect_filled(rect, (0.25 * r) as u8, if g.or { Color32::from_rgb(0x3a, 0x2a, 0x0a) } else { Color32::from_rgb(0x1b, 0x3a, 0x52) });
                ui.painter().galley(rect.center() - gl.size() / 2.0, gl, col);
                if resp.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
                let resp = resp.on_hover_text("Toggle AND / OR");
                if resp.clicked() { g.or = !g.or; changed = true; }
            });
        });
        if let Some(k) = remove { g.items.remove(k); changed = true; }
        if let Some(k) = ungroup { if let m::Node::Group(sub) = g.items.remove(k) { for (j, it) in sub.items.into_iter().enumerate() { g.items.insert(k + j, it); } } changed = true; }
        changed
    }

    /// One condition line (column, operator, value, remove cross) for `self.rows[i]`. Returns (something changed, the cross was clicked).
    fn row_line(&mut self, ui: &mut Ui, id: egui::Id, i: usize, cw: f32, ow: f32, vw: f32, r: f32, accent: Color32) -> (bool, bool) {
        let mut changed = false;
        let mut remove = false;
ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = 0.6 * r;
                            let cols: Vec<(String, String)> = m::FIELDS.iter().map(|f| (f.key.to_string(), f.label.to_string())).collect();
                            let fd = m::field(&self.rows[i].col).unwrap_or(&m::FIELDS[0]);
                            if let Some(p) = Self::combo(ui, id.with("col"), fd.label, &cols, cw, r) { self.rows[i].set_col(&p); changed = true; }
                            let fd = m::field(&self.rows[i].col).unwrap_or(&m::FIELDS[0]);
                            let ops: Vec<(String, String)> = fd.ops.iter().map(|(o, l)| (o.to_string(), l.to_string())).collect();
                            let op_label = fd.ops.iter().find(|o| o.0 == self.rows[i].op).map_or("", |o| o.1);
                            if let Some(p) = Self::combo(ui, id.with("op"), op_label, &ops, ow, r) { self.rows[i].op = p; if !m::can_compare(&self.rows[i].col, &self.rows[i].op) { self.rows[i].val_col.clear(); } changed = true; }
                            // the value
                            let row_op = self.rows[i].op.clone();
                            if m::needs_no_value(&row_op) { ui.label(RichText::new("(no value needed)").size(0.8 * r).color(Color32::from_rgb(0x66, 0x66, 0x66))); ui.add_space(vw - 8.5 * r); }
                            else if self.rows[i].uses_pills() { if self.pills(ui, i, vw, r, accent) { changed = true; } }
                            else if m::can_compare(&self.rows[i].col, &row_op) || !self.rows[i].val_col.is_empty() && m::can_compare(&self.rows[i].col, &row_op) {
                                // numeric and date columns can be compared with another column of their kind (the web's ⇄ toggle)
                                let on = !self.rows[i].val_col.is_empty();
                                let (fill, edge, ink) = if on { (Color32::from_rgb(0x2a, 0x5a, 0x2a), Color32::from_rgb(0x3a, 0x7a, 0x3a), Color32::from_rgb(0x80, 0xc0, 0x80)) } else { (Color32::from_rgb(0x1b, 0x3a, 0x52), Color32::from_rgb(0x2a, 0x5a, 0x7a), accent) };
                                let g = ui.painter().layout_no_wrap("\u{21C4}".into(), FontId::proportional(0.85 * r), ink);
                                let (rect, resp) = ui.allocate_exact_size(egui::vec2(g.size().x + 0.875 * r + 2.0, 2.6 * r), Sense::click());
                                ui.painter().rect(rect, (0.25 * r) as u8, fill, Stroke::new(1.0_f32, edge), egui::StrokeKind::Inside);
                                ui.painter().galley(rect.center() - g.size() / 2.0, g, ink);
                                if resp.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
                                let resp = resp.on_hover_text("Toggle column comparison");
                                if resp.clicked() {
                                    if on { self.rows[i].val_col.clear(); } else { self.rows[i].val_col = m::compare_cols(&self.rows[i].col)[0].to_string(); }
                                    self.rows[i].text.clear(); changed = true;
                                }
                                let vw2 = vw - rect.width() - 0.6 * r;
                                if !self.rows[i].val_col.is_empty() {
                                    let cols: Vec<(String, String)> = m::compare_cols(&self.rows[i].col).iter().map(|c| (c.to_string(), m::field(c).map_or(c.to_string(), |f| f.label.to_string()))).collect();
                                    let shown = cols.iter().find(|c| c.0 == self.rows[i].val_col).map_or(String::new(), |c| c.1.clone());
                                    if let Some(p) = Self::combo(ui, id.with("valcol"), &shown, &cols, vw2, r) { self.rows[i].val_col = p; changed = true; }
                                } else {
                                    let (hint, numeric) = match fd.kind { Kind::Date => ("YYYY-MM-DD", false), _ => ("Enter value...", true) };
                                    if Self::input(ui, &mut self.rows[i].text, vw2, r, hint).changed() { if numeric { self.rows[i].text.retain(|c| c.is_ascii_digit() || c == '.'); } changed = true; }
                                }
                            }
                            else if fd.kind == Kind::Select {
                                let opts = self.option_list(fd);
                                if self.rows[i].text.is_empty() && !fd.default.is_empty() { self.rows[i].text = fd.default.to_string(); }
                                let shown = match opts.iter().find(|o| o.0 == self.rows[i].text) {
                                    Some(o) => o.1.clone(),
                                    None if fd.key == "saved_filter" && !self.rows[i].text.is_empty() => format!("{} (not available)", if self.rows[i].name.is_empty() { "Unknown" } else { &self.rows[i].name }),
                                    None => self.rows[i].text.clone(),
                                };
                                if let Some(p) = Self::combo(ui, id.with("val"), &shown, &opts, vw, r) {
                                    if fd.key == "saved_filter" { self.rows[i].name = opts.iter().find(|o| o.0 == p).map(|o| o.1.clone()).unwrap_or_default(); }
                                    self.rows[i].text = p; changed = true;
                                }
                            } else {
                                let (hint, numeric) = match (fd.kind, row_op.as_str()) {
                                    (Kind::Date, "STRFTIME_MONTH") => ("1-12", true), (Kind::Date, "STRFTIME_DAY") => ("1-31", true), (Kind::Date, "STRFTIME_YEAR") => ("YYYY", true),
                                    (Kind::Date, "STRFTIME_WEEKDAY") => ("0=Sun 1=Mon ... 6=Sat", true), (Kind::Date, "LIKE") => ("e.g. %-03-% (March) or 2023-%", false),
                                    (Kind::Date, _) => ("YYYY-MM-DD", false), (Kind::Number, _) => ("Enter value...", true), _ => ("Enter value...", false),
                                };
                                if Self::input(ui, &mut self.rows[i].text, vw, r, hint).changed() { if numeric { self.rows[i].text.retain(|c| c.is_ascii_digit() || c == '.'); } changed = true; }
                            }
                            let x = ui.add(egui::Label::new(RichText::new("\u{2715}").size(1.1 * r).color(Color32::from_rgb(0xe0, 0x50, 0x50))).sense(Sense::click()));
                            if x.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
                            if x.clicked() { remove = true; }
                        });
        (changed, remove)
    }

    fn pills(&mut self, ui: &mut Ui, i: usize, w: f32, r: f32, accent: Color32) -> bool {
        let mut changed = false;
        let col = self.rows[i].col.clone();
        let known = self.uniques[unique_index(&col)].clone();
        egui::Frame::new().fill(crate::ui_kit::INPUT()).stroke(Stroke::new(1.0_f32, crate::ui_kit::EDGE())).corner_radius((0.25 * r) as u8).inner_margin(egui::Margin::symmetric((0.5 * r) as i8, (0.4 * r) as i8)).show(ui, |ui| {
            ui.set_width(w - 1.0 * r - 2.0);
            ui.spacing_mut().item_spacing = Vec2::new(0.4 * r, 0.3 * r);
            ui.horizontal_wrapped(|ui| {
                let mut remove = None;
                for (k, t) in self.rows[i].vals.iter().enumerate() {
                    let g = ui.painter().layout_no_wrap(t.clone(), FontId::proportional(0.8 * r), accent);
                    let (rect, _) = ui.allocate_exact_size(Vec2::new(g.size().x + 2.1 * r, 1.5 * r), Sense::hover());
                    ui.painter().rect_filled(rect, (0.25 * r) as u8, Color32::from_rgb(0x1e, 0x36, 0x4e));
                    ui.painter().rect_stroke(rect, (0.25 * r) as u8, Stroke::new(1.0_f32, crate::ui_kit::EDGE()), egui::StrokeKind::Inside);
                    ui.painter().galley(Pos2::new(rect.min.x + 0.5 * r, rect.center().y - g.size().y / 2.0), g, accent);
                    let x = Rect::from_center_size(Pos2::new(rect.max.x - 0.8 * r, rect.center().y), Vec2::splat(1.2 * r));
                    let xr = ui.interact(x, ui.id().with(("pillx", self.salt, i, k)), Sense::click());
                    ui.painter().text(x.center(), Align2::CENTER_CENTER, "\u{00D7}", FontId::proportional(0.85 * r), if xr.hovered() { Color32::WHITE } else { MUTED() });
                    if xr.clicked() { remove = Some(k); }
                }
                if let Some(k) = remove { self.rows[i].vals.remove(k); changed = true; }
                let resp = ui.add(egui::TextEdit::singleline(&mut self.rows[i].pill_in).id(egui::Id::new(("fm_pill_in", self.salt, i))).frame(egui::Frame::NONE).desired_width(8.0 * r).font(FontId::proportional(0.9 * r)));
                if resp.lost_focus() && ui.input(|k| k.key_pressed(egui::Key::Enter)) {
                    let v = self.rows[i].pill_in.trim().trim_matches(',').to_string();
                    if !v.is_empty() && !self.rows[i].vals.contains(&v) { self.rows[i].vals.push(v); changed = true; }
                    self.rows[i].pill_in.clear();
                    resp.request_focus();
                }
                // suggestions under the box
                let typed = self.rows[i].pill_in.trim().to_lowercase();
                if !typed.is_empty() {
                    let hits: Vec<String> = known.iter().filter(|k| k.to_lowercase().contains(&typed) && !self.rows[i].vals.contains(k)).take(8).cloned().collect();
                    if !hits.is_empty() {
                        let mut chosen = None;
                        egui::Area::new(egui::Id::new(("fm_suggest", self.salt, i))).order(egui::Order::Foreground).fixed_pos(Pos2::new(resp.rect.min.x, resp.rect.max.y + 4.0)).show(ui.ctx(), |ui| {
                            ui_kit::menu_frame(r).show(ui, |ui| { ui.set_min_width(10.0 * r); for h in &hits { if ui_kit::menu_row(ui, "", h, ui_kit::TEXT(), false, r).clicked() { chosen = Some(h.clone()); } } });
                        });
                        if let Some(c) = chosen { self.rows[i].vals.push(c); self.rows[i].pill_in.clear(); changed = true; }
                    }
                }
            });
        });
        changed
    }

    fn sub_dialog(&mut self, ctx: &egui::Context, _r: f32, out: &mut FilterOut) {
        let Some((kind, mut name)) = self.sub.take() else { return };
        let accent = ui_kit::accent();
        let title = match kind { Sub::Save => "Save Filter", Sub::Rename => "Rename Filter", Sub::Delete => "Delete Filter" };
        let (ok, mut cancel) = (std::cell::Cell::new(false), false);
        let res = ui_kit::modal_footer(ctx, "fm_sub", title, 21.25, |ui, r| {
            match kind {
                Sub::Delete => { ui.label(RichText::new(format!("Delete the saved filter \"{}\"?", self.selected)).size(0.95 * r).color(TEXT())); }
                _ => {
                    ui.label(RichText::new("Name").size(0.85 * r).color(MUTED()));
                    let resp = Self::input(ui, &mut name, ui.available_width(), r, "Filter name");
                    if resp.lost_focus() && ui.input(|k| k.key_pressed(egui::Key::Enter)) { ok.set(true); }
                }
            }
        }, |ui, r| {
            ui.horizontal(|ui| {
                let total = ui.available_width() - ui.spacing().item_spacing.x;
                let danger = matches!(kind, Sub::Delete);
                let fill = if danger { Some((crate::ui_kit::DANGER(), crate::ui_kit::DANGER())) } else { Some((Color32::from_rgb(0x1a, 0x5c, 0x2e), Color32::from_rgb(0x2a, 0x7c, 0x3e))) };
                let _ = accent;
                if ui_kit::nav_button(ui, "Cancel", total / 2.0, r, None).clicked() { cancel = true; }
                if ui_kit::nav_button(ui, if danger { "Delete" } else { "OK" }, total / 2.0, r, fill).clicked() { ok.set(true); }
            });
        });
        if res.closed { cancel = true; }
        if cancel { return; }
        if !ok.get() { self.sub = Some((kind, name)); return; }
        let name = name.trim().to_string();
        match kind {
            Sub::Save => {
                if name.is_empty() { *out = FilterOut::Toast("Name cannot be empty.".into()); self.sub = Some((kind, name)); return; }
                let tree = self.save_tree();
                if m::is_empty_tree(&tree) { *out = FilterOut::Toast("No filter to save.".into()); return; }
                let id = self.saved.get(&name).and_then(|e| e.get("id")).and_then(|v| v.as_str()).map(String::from).unwrap_or_else(m::new_id);
                self.saved.insert(name.clone(), json!({"id": id, "tree": tree.clone()}));
                self.selected = name.clone();
                self.loaded_name = Some(name);
                self.loaded_tree = Some(tree);
                *out = FilterOut::SavedFilters { map: Value::Object(self.saved.clone()), filter_tree: None };
            }
            Sub::Rename => {
                let old = self.selected.clone();
                if name.is_empty() || name == old { return; }
                if self.saved.contains_key(&name) { *out = FilterOut::Toast(format!("A filter named \"{name}\" already exists.")); return; }
                // keep the dictionary's order: rebuild it with the new name in place
                let rebuilt: serde_json::Map<String, Value> = std::mem::take(&mut self.saved).into_iter().map(|(k, v)| if k == old { (name.clone(), v) } else { (k, v) }).collect();
                self.saved = rebuilt;
                if self.loaded_name.as_deref() == Some(&old) { self.loaded_name = Some(name.clone()); }
                self.selected = name.clone();
                *out = FilterOut::SavedFilters { map: Value::Object(self.saved.clone()), filter_tree: Some(json!({"__rename": [old, name]})) };
            }
            Sub::Delete => {
                let old = self.selected.clone();
                self.saved.remove(&old);
                if self.loaded_name.as_deref() == Some(&old) { self.loaded_name = None; self.loaded_tree = None; }
                self.selected.clear();
                *out = FilterOut::SavedFilters { map: Value::Object(self.saved.clone()), filter_tree: Some(json!({"__delete": old})) };
            }
        }
    }
}

/// Two trees mean the same when their items match (a saved filter's own metadata does not matter).
fn tree_eq(a: &Value, b: &Value) -> bool { a.get("logic") == b.get("logic") && a.get("items") == b.get("items") }

#[cfg(test)]
mod dialog_tests {
    use super::*;
    fn open(tree: Value) -> FilterDialog { FilterDialog::new(Some(&tree), json!({}), vec![], vec![], Default::default(), "") }
    #[test] fn the_search_box_words_open_as_a_title_row_and_clear_all_reports_itself() {
        let tree = json!({"type": "group", "logic": "AND", "items": [{"type": "condition", "column": "tags", "operator": "LIKE", "value": "Indie"}]});
        let d = FilterDialog::new(Some(&tree), json!({}), vec![], vec![], Default::default(), "zel");
        assert_eq!(d.rows.len(), 2);
        assert_eq!(d.rows[0].col, "name");
        let d = FilterDialog::new(None, json!({}), vec![], vec![], Default::default(), "zel");   // no filter at all: still a row
        assert_eq!(d.rows.len(), 1);
        let mut d = d;
        assert!(!d.take_cleared());
    }
    #[test] fn a_filter_simple_mode_can_show_opens_in_simple_mode() {
        let d = open(json!({"type": "group", "logic": "AND", "items": [{"type": "condition", "column": "tags", "operator": "LIKE", "value": "Indie"}]}));
        assert!(!d.adv_mode);
        assert!(!FilterDialog::new(None, json!({}), vec![], vec![], Default::default(), "").adv_mode, "no filter: simple");
    }
    #[test] fn an_advanced_filter_opens_on_the_advanced_tab() {
        let nested = json!({"type": "group", "logic": "AND", "items": [{"type": "condition", "column": "installed", "operator": "=", "value": "1"},
            {"type": "group", "logic": "OR", "items": [{"type": "condition", "column": "tags", "operator": "LIKE", "value": "RPG"}, {"type": "condition", "column": "playtime_forever", "operator": ">", "value": "600"}]}]});
        assert!(open(nested).adv_mode, "nested groups");
        assert!(open(json!({"type": "group", "logic": "AND", "items": [{"type": "custom_expr", "sql": "name LIKE 'a%'"}]})).adv_mode, "an expression");
        let d = open(json!({"type": "group", "logic": "AND", "items": [], "custom_sql": "playtime_forever > 60"}));
        assert!(d.adv_mode && d.custom_sql == "playtime_forever > 60", "custom SQL, carried over");
        // a pointer to a saved filter shows that filter: advanced when the saved tree is
        let saved = json!({"Big": {"id": "1", "tree": {"type": "group", "logic": "AND", "items": [{"type": "condition", "column": "installed", "operator": "=", "value": "1"}, {"type": "group", "logic": "OR", "items": [{"type": "condition", "column": "tags", "operator": "LIKE", "value": "RPG"}, {"type": "condition", "column": "tags", "operator": "LIKE", "value": "Indie"}]}]}}});
        let d = FilterDialog::new(Some(&json!({"saved_filter": "Big"})), saved, vec![], vec![], Default::default(), "");
        assert!(d.adv_mode, "adv_mode (unsupported: {:?})", d.unsupported); assert_eq!(d.loaded_name.as_deref(), Some("Big"));
    }
    /// Save Current in advanced mode saves the advanced tree (it used to save the empty simple rows, so the new name never appeared).
    #[test] fn save_current_in_advanced_mode_saves_the_advanced_tree_with_its_pagywosg_markers() {
        let spec = pd_filter::pagywosg_filter::Spec { event_id: Some(9), event_name: "E".into(), sg_group: Some("Won".into()), wins: pd_filter::pagywosg_filter::PoolSpec { tags: vec!["RPG".into()], conds: vec![] }, ..Default::default() };
        let d = open(spec.to_tree());
        assert!(d.adv_mode);
        let t = d.save_tree();
        assert!(!m::is_empty_tree(&t), "something to save: {t}");
        assert_eq!(t["pagywosg"], true, "the PAGYWOSG marker stays");
        assert!(t.to_string().contains("\"RPG\""), "the edited criteria are in it");
        // a plain advanced filter too
        let d = open(json!({"type": "group", "logic": "AND", "items": [{"type": "custom_expr", "sql": "name LIKE 'a%'"}]}));
        assert!(!m::is_empty_tree(&d.save_tree()));
    }
}
