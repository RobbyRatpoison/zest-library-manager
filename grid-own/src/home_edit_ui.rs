//! Edit Home Page: a scale model of the page in a dialog. The map is drawn in the shape of a screen (your window, or a preset such as the Steam Deck's), every shelf at its real
//! row height and width with placeholder cards of the real size, so how many cards fit, and whether the layout runs past the bottom of the screen (the page then scrolls), is
//! visible at once. Boxes drag by grabbing them: onto a sibling to swap places in the row, onto another row to move the row there (below it if it came from above, above it if it
//! came from below, as PlayDate does), onto the left or right edge of another row's box to put it beside that shelf. Rows resize by their bottom edge and side-by-side shelves by
//! the bar between them. Everything else about the selected shelf is edited in the inspector under the map: title, number of games, height, width, split, hide, delete.
//! Splitting follows PlayDate's picker (which shelf, then where in the row). Nothing reaches the page until SAVE LAYOUT.
use crate::home::{card_metrics, row_widths};
use crate::ui_kit::{self, Kind};
use egui::{Color32, Pos2, Rect, Sense, Shape, Stroke, Ui, Vec2};
use pd_filter::home_edit as he;
use serde_json::{json, Value};

enum Drag {
    Move(String),
    Height { id: String, vh0: f32, y0: f32 },
    Cols { a: String, b: String, pa: f32, pb: f32, x0: f32, wa0: f32, wb0: f32 },
}
struct SplitPick { a: String, /// Move mode: `a` is the shelf being moved and `b` (once chosen) any shelf of the row it goes to.
    mover: bool, b: Option<String>, /// A shelf made by the picker's NEW SHELF: removed again if the choice is backed out of.
    new_id: Option<String> }
/// What the split and move pickers preview (display only): the Home map as it would look if the highlighted choice were taken.
#[derive(Clone, PartialEq)]
enum Plan {
    /// `moved` joins `target`'s row; `before` is the member it goes before (None = after the row's last).
    Join { moved: String, target: String, before: Option<String> },
    /// `moved` gets a row of its own before `before`'s row (None = the end of the page).
    Alone { moved: String, before: Option<String> },
    /// Nothing changes yet: `row` is outlined (the row being considered) and `moved`, if any, is the shelf that would go there.
    Row { moved: Option<String>, row: String },
}
/// The choice under the pad's focus and the one under the mouse, seen in one pass over a picker's buttons.
#[derive(Default)]
struct Track { foc: Option<(Plan, String)>, hov: Option<(Plan, String)> }
impl Track {
    fn see(&mut self, r: &egui::Response, plan: Plan, cap: String) {
        if r.has_focus() { self.foc = Some((plan.clone(), cap.clone())); }
        if r.hovered() { self.hov = Some((plan, cap)); }
    }
}
/// Resizing with the pad: A on a bar starts it, the arrows move it, A or B ends it.
enum PadDrag { Height { id: String, vh: f32 }, Cols { a: String, b: String, share: f32, total: f64 } }


pub struct HomeEditor {
    /// What the shelf settings offer: filters (key, label) and the platforms in the library (key, label).
    filters: Vec<(String, String)>, platforms: Vec<(String, String)>,
    work: Vec<Value>, sel: Option<String>, drag: Option<Drag>, split: Option<SplitPick>, delete_ask: Option<String>, reset_ask: bool,
    /// The shelf priority panel while it is open: shelf ids in priority order (top claims a duplicate game first) and whether each takes part in de-duplication.
    prio: Option<Vec<(String, bool)>>, /// The shelf under the pointer in the priority list: its box is lit on the map.
    prio_hover: Option<String>,
    pad_drag: Option<PadDrag>,
    /// The picker's preview: the choice shown now and its caption, the focused choice of the last pass, and whether the pad moved last (it then beats a resting mouse).
    hl: Option<(Plan, String)>, hl_foc: Option<Plan>, pad_wins: bool,
    window: (i32, i32),
    /// The wedges of the completion and achievement charts, to draw the widgets as they look.
    pies: (Vec<(f32, u32)>, Vec<(f32, u32)>),
}
pub enum EditorOut { None, Close, /// The shelves to save and show.
    Save(Vec<Value>) }

enum Edit {
    Title(String, String), Games(String, i64), Height(String, i64), Width(String, f64),
    /// Games (None) or a widget (Some(clock | completion_pie | achievement_pie)) on a shelf.
    Kind(String, Option<&'static str>),
    OpenPrio, ApplyPrio(Vec<(String, bool)>),
    Hide(String), Show(String), Fill(String, bool), Set(String, &'static str, Value), Filter(String, String), Platform(String, String, bool), Delete(String), Unsplit(String), OpenSplit(String), OpenMove(String), NewRow { id: String, before: Option<String> }, Select(String), Add,
    Reorder(String, String), MoveRow(String, String), DropSplit { dragged: String, target: String, before: Option<String> },
}

fn s_of<'a>(v: &'a Value, k: &str) -> &'a str { v.get(k).and_then(|x| x.as_str()).unwrap_or("") }
fn widget_kind(v: &Value) -> Option<&'static str> { ["preset", "filter_key"].iter().find_map(|k| ["clock", "completion_pie", "achievement_pie"].into_iter().find(|w| s_of(v, k) == *w)) }
fn title_of(v: &Value) -> String { let l = s_of(v, "label"); if l.is_empty() { s_of(v, "id").replace('_', " ").to_uppercase() } else { l.to_string() } }
/// The shelves that take part in de-duplication choices (not widgets), in priority order: lower `dedup_priority` claims a duplicate game first. (id, included).
fn priority_list(work: &[Value]) -> Vec<(String, bool)> {
    let mut v: Vec<&Value> = work.iter().filter(|s| widget_kind(s).is_none()).collect();
    v.sort_by(|a, b| num(a, "dedup_priority", 0.0).partial_cmp(&num(b, "dedup_priority", 0.0)).unwrap_or(std::cmp::Ordering::Equal));
    v.into_iter().map(|s| (s_of(s, "id").to_string(), s.get("dedup").map_or(true, |x| x.as_bool().unwrap_or(true)))).collect()
}
/// The most vh a row can have without the page running past the window: what the other rows leave (they keep their own heights; a row that fills never goes under its saved height).
fn height_room(work: &[Value], id: &str) -> f32 {
    let mine = he::row_of(work, id);
    let rows = he::rows(work);
    let others: f32 = rows.iter().filter(|r| **r != mine).map(|r| num(&work[r[0]], "row_height", 30.0) as f32).sum();
    100.0 - 5.5 - 0.5 * (rows.len() as f32 - 1.0) - others
}
/// A shelf's place (1 = first) among the shelves that de-duplicate, for the map's badge.
fn priority_rank(work: &[Value], id: &str) -> Option<usize> {
    priority_list(work).into_iter().filter(|(_, on)| *on).position(|(i, _)| i == id).map(|p| p + 1)
}
fn visible(v: &Value) -> bool { v.get("visible").map_or(true, |x| x.as_bool().unwrap_or(true)) }
fn num(v: &Value, k: &str, d: f64) -> f64 { v.get(k).and_then(|x| x.as_f64()).unwrap_or(d) }

/// A wedge of a circle as filled polygons (each at most a quarter turn, so each is convex).
fn wedge(painter: &egui::Painter, c: Pos2, rad: f32, from: f32, to: f32, fill: Color32) {
    let mut a = from;
    while a < to - 1e-4 {
        let b = (a + std::f32::consts::FRAC_PI_2).min(to);
        let steps = (((b - a) / 0.15).ceil() as usize).max(1);
        let mut pts = vec![c];
        for k in 0..=steps { let t = a + (b - a) * k as f32 / steps as f32; pts.push(Pos2::new(c.x + rad * t.sin(), c.y - rad * t.cos())); }
        painter.add(Shape::convex_polygon(pts, fill, Stroke::NONE));
        a = b;
    }
}

impl HomeEditor {
    pub fn new(shelves: Vec<Value>, window: (i32, i32), pies: (Vec<(f32, u32)>, Vec<(f32, u32)>), filters: Vec<(String, String)>, platforms: Vec<(String, String)>) -> HomeEditor {
        let mut shelves = shelves;
        he::normalize_all(&mut shelves);   // a row whose shelves disagree about fill or height (an earlier join could leave that) is made to agree with its first shelf, which is what the page reads
        HomeEditor { filters, platforms, work: shelves, sel: std::env::var("ZEST_EDIT_SELECT").ok(), drag: None, split: None, delete_ask: None, reset_ask: false, prio: None, prio_hover: None, pad_drag: None, hl: None, hl_foc: None, pad_wins: false, window: (window.0.max(400), window.1.max(300)), pies }
    }
    /// The pad is resizing a bar: the page hands every direction and A/B here instead of moving focus.
    pub fn pad_dragging(&self) -> bool { self.pad_drag.is_some() }
    /// One pad press while resizing. Up/Down change a row's height a vh at a time, Left/Right move the bar between two shelves; A and B end it.
    pub fn pad_key(&mut self, n: crate::gamepad::Nav) {
        use crate::gamepad::Nav;
        let mut edits: Vec<Edit> = vec![];
        match (&mut self.pad_drag, n) {
            (_, Nav::Confirm | Nav::Back) => self.pad_drag = None,
            (Some(PadDrag::Height { id, vh }), Nav::Up | Nav::Down) => {
                *vh = (*vh + if n == Nav::Up { -1.0 } else { 1.0 }).round().clamp(he::MIN_ROW_VH as f32, he::MAX_ROW_VH as f32);
                let cap = height_room(&self.work, id).max(*vh - if n == Nav::Down { 1.0 } else { 0.0 }).floor();
                *vh = vh.min(cap);
                edits.push(Edit::Height(id.clone(), *vh as i64));
            }
            (Some(PadDrag::Cols { a, b, share, total }), Nav::Left | Nav::Right) => {
                *share = (*share + if n == Nav::Left { -0.03 } else { 0.03 }).clamp(0.1, 0.9);
                edits.push(Edit::Width(a.clone(), *total * *share as f64)); edits.push(Edit::Width(b.clone(), *total * (1.0 - *share) as f64));
            }
            _ => {}
        }
        self.apply(edits);
    }
    /// B with the pad: closes whatever sits on top of the map (a delete or reset question, the priority panel) or takes the picker back one step. True when it did one of these,
    /// so the page does not also treat B as "close the dialog".
    pub fn pad_back_open(&self) -> bool { self.delete_ask.is_some() || self.reset_ask || self.prio.is_some() || self.split.is_some() }
    pub fn pad_back(&mut self) -> bool {
        if self.delete_ask.take().is_some() { return true; }
        if self.reset_ask { self.reset_ask = false; return true; }
        if self.prio.is_some() { self.prio = None; self.prio_hover = None; return true; }
        if self.split.is_some() { self.back_step(); return true; }
        false
    }
    /// The pickers' BACK: from the position step to the choice before it (a shelf made by + NEW SHELF is taken away again), from the first step out of the picker.
    fn back_step(&mut self) {
        self.hl = None; self.hl_foc = None;
        let Some(sp) = self.split.as_mut() else { return };
        if sp.b.is_some() {
            if let Some(id) = sp.new_id.take() { he::delete(&mut self.work, &id); }
            sp.b = None;
        } else { self.split = None; }
    }
    fn idx(&self, id: &str) -> Option<usize> { self.work.iter().position(|s| s_of(s, "id") == id) }
    fn screen(&self) -> (i32, i32) { self.window }

    fn apply(&mut self, edits: Vec<Edit>) {
        for e in edits {
            match e {
                Edit::Title(id, t) => if let Some(i) = self.idx(&id) { self.work[i]["label"] = json!(t); },
                Edit::Games(id, n) => if let Some(i) = self.idx(&id) { self.work[i]["limit"] = json!(n.clamp(0, 200)); },
                Edit::Height(id, vh) => he::set_row_height(&mut self.work, &id, vh),
                Edit::Width(id, w) => if let Some(i) = self.idx(&id) { self.work[i]["col_width"] = json!(w.clamp(0.5, 20.0)); },
                Edit::Hide(id) => { he::unsplit(&mut self.work, &id); if let Some(i) = self.idx(&id) { self.work[i]["visible"] = json!(false); } }
                Edit::Set(id, key, v) => if let Some(i) = self.idx(&id) { self.work[i][key] = v; },
                // nothing stays selected while the priority list is open: the map lights a box when it is selected OR hovered in the list, so a selected shelf would stay outlined
                Edit::OpenPrio => { self.sel = None; self.prio = Some(priority_list(&self.work)); }
                Edit::ApplyPrio(list) => for (n, (id, on)) in list.iter().enumerate() { if let Some(i) = self.idx(id) { self.work[i]["dedup_priority"] = json!(n as i64); self.work[i]["dedup"] = json!(*on); } },
                Edit::Kind(id, w) => if let Some(i) = self.idx(&id) {
                    let cur = widget_kind(&self.work[i]);
                    match w {
                        Some(w) => {
                            // remember the games filter so switching back restores it
                            if cur.is_none() { let k = { let k = s_of(&self.work[i], "filter_key"); if k.is_empty() { s_of(&self.work[i], "preset") } else { k } }.to_string(); self.work[i]["games_filter"] = json!(k); }
                            self.work[i]["filter_key"] = json!(w); self.work[i]["preset"] = json!(w); self.work[i]["custom_sql"] = Value::Null;
                        }
                        None => if cur.is_some() {
                            let back = { let k = s_of(&self.work[i], "games_filter"); if k.is_empty() || widget_kind(&json!({"filter_key": k})).is_some() { "all_games" } else { k } }.to_string();
                            self.work[i]["filter_key"] = json!(back.clone()); self.work[i]["preset"] = json!(back); self.work[i]["custom_sql"] = Value::Null;
                        },
                    }
                },
                Edit::Filter(id, k) => if let Some(i) = self.idx(&id) { self.work[i]["filter_key"] = json!(k); self.work[i]["preset"] = json!(k); self.work[i]["custom_sql"] = Value::Null; },
                Edit::Platform(id, p, shown) => if let Some(i) = self.idx(&id) {
                    let mut hid: Vec<String> = self.work[i].get("hidden_platforms").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
                    hid.retain(|x| *x != p); if !shown { hid.push(p); }
                    self.work[i]["hidden_platforms"] = json!(hid);
                },
                Edit::Fill(id, on) => for k in he::row_of(&self.work, &id) {
                    // a row that fills keeps only a small minimum (10 vh) so it can give way; the height it had is kept and comes back when Fill is turned off
                    if on {
                        if self.work[k].get("height_before_fill").is_none() { let h = num(&self.work[k], "row_height", 30.0); self.work[k]["height_before_fill"] = json!(h); }
                        if num(&self.work[k], "row_height", 30.0) > 10.0 { self.work[k]["row_height"] = json!(10); }
                    } else if let Some(h) = self.work[k].get("height_before_fill").and_then(|v| v.as_f64()) {
                        self.work[k]["row_height"] = json!(h);
                        if let Some(o) = self.work[k].as_object_mut() { o.remove("height_before_fill"); }
                    }
                    self.work[k]["fill"] = json!(on);
                },
                Edit::Show(id) => if let Some(i) = self.idx(&id) { self.work[i]["visible"] = json!(true); },
                Edit::Delete(id) => { he::delete(&mut self.work, &id); if self.sel.as_deref() == Some(id.as_str()) { self.sel = None; } }
                Edit::Unsplit(id) => he::unsplit(&mut self.work, &id),
                Edit::OpenSplit(id) => { self.hl = None; self.hl_foc = None; self.split = Some(SplitPick { a: id, mover: false, b: None, new_id: None }); }
                Edit::OpenMove(id) => { self.hl = None; self.hl_foc = None; self.split = Some(SplitPick { a: id, mover: true, b: None, new_id: None }); }
                Edit::NewRow { id, before } => { he::move_to_new_row(&mut self.work, &id, before.as_deref()); }
                Edit::Select(id) => self.sel = Some(id),
                Edit::Add => { let id = he::add_shelf(&mut self.work); self.sel = Some(id); }
                Edit::Reorder(id, t) => { he::reorder_in_row(&mut self.work, &id, &t); }
                Edit::MoveRow(id, t) => { if let Some(b) = he::drop_before(&self.work, &id, &t) { he::move_row(&mut self.work, &id, b.as_deref()); } }
                Edit::DropSplit { dragged, target, before } => { he::split_with(&mut self.work, &target, &dragged, before.as_deref()); }
            }
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> EditorOut {
        let accent = ui_kit::accent();
        let mut out = EditorOut::None;
        let mut edits: Vec<Edit> = vec![];
        let res = ui_kit::modal_full(ctx, "home_edit", "Edit Home Page", 66.0, |_, _| {}, |ui, r| {
            // the split picker (which shelf, then where in the row) takes the place of the map
            if self.split.is_some() { self.split_picker(ui, r, accent, &mut edits); return; }

            ui.label(ui_kit::muted("A scale model of the page. Drag a shelf to move it, drag a row's bottom edge or the bar between two shelves to resize, and edit the selected shelf below. Nothing changes on Home until SAVE.", 0.82 * r));
            ui.add_space(0.3 * r);
            let (sw_px, sh_px) = self.screen();
            let (pointer, down) = ui.input(|i| (i.pointer.hover_pos(), i.pointer.primary_down()));
            let rows = he::rows(&self.work);
            let vh_px = sh_px as f32 / 100.0;
            // the page's height (what the rows add up to) and how far past the screen that is
            let row_vh: Vec<f32> = he::row_vhs(&rows.iter().map(|row| num(&self.work[row[0]], "row_height", 30.0) as f32).collect::<Vec<_>>(), &rows.iter().map(|row| self.work[row[0]].get("fill").and_then(|v| v.as_bool()).unwrap_or(false)).collect::<Vec<_>>());
            let content_vh = 5.5 + row_vh.iter().sum::<f32>() + 0.5 * row_vh.len().saturating_sub(1) as f32;
            // under 2 px past the bottom is not a scroll on the page (`max_scroll`), so it is not one here either
            let over = (content_vh - 100.0) * vh_px >= 2.0;
            let avail_w = ui.available_width() - 14.0;
            let max_h = (ctx.content_rect().height() * 0.42).max(20.0 * r);
            let s = (avail_w / sw_px as f32).min(max_h / sh_px as f32);
            let (map_w, frame_h) = (sw_px as f32 * s, sh_px as f32 * s);
            let canvas_h = if over { frame_h.max(content_vh * vh_px * s) } else { frame_h };
            let mut rects: Vec<(String, usize, Rect)> = vec![];
            let mut pad_start: Option<PadDrag> = None;
            egui::ScrollArea::vertical().id_salt("home_map").max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
                let (canvas, _) = ui.allocate_exact_size(Vec2::new(avail_w + 14.0, canvas_h + 4.0), Sense::hover());
                let origin = Pos2::new(canvas.min.x + (avail_w + 14.0 - map_w) / 2.0, canvas.min.y + 2.0);
                let painter = ui.painter().clone();
                let frame = Rect::from_min_size(origin, Vec2::new(map_w, frame_h));
                painter.rect_filled(Rect::from_min_size(origin, Vec2::new(map_w, canvas_h)), 3.0, Color32::from_rgb(8, 12, 16));
                if canvas_h > frame_h + 1.0 { painter.rect_filled(Rect::from_min_max(Pos2::new(origin.x, frame.max.y), Pos2::new(origin.x + map_w, origin.y + canvas_h)), 0.0, Color32::from_rgba_unmultiplied(160, 60, 40, 28)); }
                // the top bar
                painter.rect_filled(Rect::from_min_size(origin, Vec2::new(map_w, 5.0 * vh_px * s)), 0.0, Color32::from_rgb(23, 26, 33));
                painter.text(origin + Vec2::new(8.0, 2.5 * vh_px * s), egui::Align2::LEFT_CENTER, "top bar", egui::FontId::proportional(0.7 * r), Color32::from_gray(0x70));
                let mut y_px = 5.5 * vh_px;
                for (ri, row) in rows.iter().enumerate() {
                    let n = row.len();
                    let rh_px = row_vh[ri] * vh_px;
                    let weights: Vec<f32> = row.iter().map(|i| num(&self.work[*i], "col_width", 10.0).max(0.5) as f32).collect();
                    let xs = row_widths(sw_px, &weights);
                    for (k, &i) in row.iter().enumerate() {
                        let id = s_of(&self.work[i], "id").to_string();
                        let (bx, bw) = xs[k];
                        let rect = Rect::from_min_size(origin + Vec2::new(bx * s, y_px * s), Vec2::new(bw * s, rh_px * s));
                        rects.push((id.clone(), ri, rect));
                        let sel = self.sel.as_deref() == Some(id.as_str()) || self.prio_hover.as_deref() == Some(id.as_str());
                        painter.rect_filled(rect, 3.0, if sel { Color32::from_rgb(22, 42, 62) } else { Color32::from_rgb(20, 28, 38) });
                        let sh = &self.work[i];
                        let wk = widget_kind(sh);
                        let label = s_of(sh, "label");
                        let limit = num(sh, "limit", 10.0) as i64;
                        // what is inside: placeholder cards at the real size, or the chart
                        if let Some(kind) = wk {
                            let size = (bw - 30.0).min(rh_px - 30.0).max(0.0) * s;
                            let c = rect.center();
                            if kind == "clock" {
                                painter.circle_filled(c, size / 2.0, Color32::from_rgb(30, 40, 52)); painter.circle_stroke(c, size / 2.0, Stroke::new(1.5_f32, Color32::from_gray(0xb0)));
                                painter.line_segment([c, c + Vec2::new(0.0, -size * 0.36)], Stroke::new(2.0_f32, Color32::WHITE)); painter.line_segment([c, c + Vec2::new(size * 0.26, size * 0.1)], Stroke::new(2.0_f32, Color32::WHITE));
                            } else {
                                let slices = if kind == "achievement_pie" { &self.pies.1 } else { &self.pies.0 };
                                if slices.is_empty() { painter.circle_filled(c, size / 2.0, Color32::from_rgb(40, 50, 62)); }
                                let mut a = 0.0f32;
                                for (frac, col) in slices { let to = a + frac * std::f32::consts::TAU; wedge(&painter, c, size / 2.0, a, to, Color32::from_rgb((*col >> 16) as u8, (*col >> 8) as u8, *col as u8)); a = to; }
                            }
                        } else {
                            let (cw, ch, fit) = card_metrics(bw, rh_px, vh_px, !label.is_empty());
                            let label_total = if label.is_empty() { 0.0 } else { (1.1 * vh_px * 1.07).round() + 4.0 };
                            let shown = if limit == 0 { fit } else { (limit as usize).min(fit) };
                            let avail = bw - 22.0;
                            let used = shown as f32 * (cw + 6.0) - 6.0;
                            let overflow = limit == 0 || limit as usize > fit;
                            let start = bx + 11.0 + ((avail - used) / 2.0).max(0.0);
                            for c in 0..shown {
                                let cr = Rect::from_min_size(origin + Vec2::new((start + c as f32 * (cw + 6.0)) * s, (y_px + 10.0 + label_total) * s), Vec2::new(cw * s, ch * s));
                                painter.rect_filled(cr, (4.0 * s).max(1.0), Color32::from_rgb(38, 46, 58));
                            }
                            if !label.is_empty() { painter.text(Pos2::new(rect.center().x, rect.min.y + (7.0 + 5.0) * s), egui::Align2::CENTER_CENTER, label.to_uppercase(), egui::FontId::proportional((1.1 * vh_px * s).clamp(7.0, 14.0)), accent); }
                            // how many fit
                            if rect.width() > 7.0 * r && rect.height() > 2.2 * r {
                                let note = if overflow { format!("{fit} fit \u{b7} scrolls") } else if (limit as usize) < fit { format!("{fit} fit \u{b7} room for {} more", fit - limit as usize) } else { format!("{fit} fit") };
                                let g = painter.layout_no_wrap(note, egui::FontId::proportional(0.75 * r), Color32::WHITE);
                                let pr = Rect::from_min_size(Pos2::new(rect.max.x - g.size().x - 10.0, rect.max.y - g.size().y - 8.0), g.size() + Vec2::new(8.0, 4.0));
                                painter.rect_filled(pr, 3.0, Color32::from_black_alpha(190)); painter.galley(pr.min + Vec2::new(4.0, 2.0), g, Color32::WHITE);
                            }
                        }
                        painter.rect_stroke(rect, 3.0, Stroke::new(if sel { 2.5 } else { 1.0_f32 }, if sel { accent } else { Color32::from_rgb(60, 95, 125) }), egui::StrokeKind::Inside);
                        if wk.is_some() || label.is_empty() { painter.text(rect.min + Vec2::new(5.0, 4.0), egui::Align2::LEFT_TOP, title_of(sh), egui::FontId::proportional(0.7 * r), Color32::from_gray(0x90)); }
                        // the priority of a shelf that leaves out games shown above it: 1 claims a duplicate game first
                        if let (None, Some(rank)) = (wk, priority_rank(&self.work, &id)) { if rect.width() > 5.0 * r {
                            let g = painter.layout_no_wrap(format!("priority {rank}"), egui::FontId::proportional(0.7 * r), Color32::WHITE);
                            let pr = Rect::from_min_size(Pos2::new(rect.max.x - g.size().x - 10.0, rect.min.y + 4.0), g.size() + Vec2::new(8.0, 4.0));
                            painter.rect_filled(pr, 3.0, Color32::from_black_alpha(190)); painter.galley(pr.min + Vec2::new(4.0, 2.0), g, Color32::WHITE);
                        } }
                        // grab a box anywhere: a click selects it, a drag moves it
                        let resp = ui.interact(rect, ui.id().with(("box", &id)), Sense::click_and_drag());
                        if resp.clicked() { edits.push(Edit::Select(id.clone())); }
                        if resp.hovered() && self.drag.is_none() { ui.ctx().set_cursor_icon(egui::CursorIcon::Grab); }
                        if resp.drag_started() { self.drag = Some(Drag::Move(id.clone())); edits.push(Edit::Select(id.clone())); }
                        // the bar between this box and the next: drag to share the row's width differently
                        if let Some(&j) = row.get(k + 1) {
                            let gx = rect.max.x + ((xs[k + 1].0 - bx - bw) * s) / 2.0;
                            let grip = Rect::from_min_max(Pos2::new(gx - 5.0, rect.min.y), Pos2::new(gx + 5.0, rect.max.y));
                            let resp = ui.interact(grip, ui.id().with(("cols", &id)), Sense::click_and_drag());
                            let pad_on = matches!(&self.pad_drag, Some(PadDrag::Cols { a, .. }) if *a == id);
                            if resp.clicked() && !ui.input(|i| i.pointer.any_click()) {   // A on the focused bar (a mouse click does nothing here)
                                pad_start = Some(PadDrag::Cols { a: id.clone(), b: s_of(&self.work[j], "id").to_string(), share: weights[k] / (weights[k] + weights[k + 1]), total: (weights[k] + weights[k + 1]) as f64 });
                            }
                            if resp.hovered() || resp.dragged() { ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal); }
                            painter.rect_filled(Rect::from_center_size(grip.center(), Vec2::new(4.0, (rect.height() * 0.4).clamp(8.0, 60.0))), 2.0, if resp.hovered() || resp.dragged() || pad_on { accent } else { accent.gamma_multiply(0.5) });
                            if resp.drag_started() {
                                let b = s_of(&self.work[j], "id").to_string();
                                self.drag = Some(Drag::Cols { a: id.clone(), b, pa: bw * s, pb: xs[k + 1].1 * s, x0: pointer.map_or(gx, |p| p.x), wa0: weights[k], wb0: weights[k + 1] });
                            }
                        }
                    }
                    // the bottom edge of the row: drag for its height
                    let row_rect = Rect::from_min_size(origin + Vec2::new(xs[0].0 * s, y_px * s), Vec2::new((xs[n - 1].0 + xs[n - 1].1 - xs[0].0) * s, rh_px * s));
                    let grip = Rect::from_min_max(Pos2::new(row_rect.min.x, row_rect.max.y - 4.0), Pos2::new(row_rect.max.x, row_rect.max.y + 5.0));
                    let first_id = s_of(&self.work[row[0]], "id").to_string();
                    let resp = ui.interact(grip, ui.id().with(("rowh", ri)), Sense::click_and_drag());
                    if resp.clicked() && !ui.input(|i| i.pointer.any_click()) { pad_start = Some(PadDrag::Height { id: first_id.clone(), vh: row_vh[ri] }); }   // A on the focused bar
                    let hot = resp.hovered() || matches!(&self.drag, Some(Drag::Height { id, .. }) if *id == first_id) || matches!(&self.pad_drag, Some(PadDrag::Height { id, .. }) if *id == first_id);
                    if hot { ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical); }
                    painter.rect_filled(Rect::from_center_size(Pos2::new(row_rect.center().x, row_rect.max.y + 0.5), Vec2::new(50.0, 4.0)), 2.0, if hot { accent } else { accent.gamma_multiply(0.45) });
                    if resp.drag_started() {
                        // grabbing the edge of a row that fills sets its height by hand: it stops filling and the drag starts from the height shown
                        let fills = self.work[row[0]].get("fill").and_then(|v| v.as_bool()).unwrap_or(false);
                        if fills { edits.push(Edit::Fill(first_id.clone(), false)); }
                        self.drag = Some(Drag::Height { vh0: if fills { row_vh[ri] } else { num(&self.work[row[0]], "row_height", 30.0) as f32 }, y0: pointer.map_or(0.0, |p| p.y), id: first_id });
                    }
                    y_px += rh_px + 0.5 * vh_px;
                }
                // the bottom of the screen
                painter.line_segment([Pos2::new(origin.x, frame.max.y), Pos2::new(origin.x + map_w, frame.max.y)], Stroke::new(1.5_f32, if over { Color32::from_rgb(0xe0, 0x7a, 0x3a) } else { Color32::from_gray(0x50) }));
                painter.rect_stroke(frame, 0.0, Stroke::new(1.0_f32, Color32::from_gray(0x50)), egui::StrokeKind::Outside);
                // a drag in progress: where it will land
                let ptr = pointer.unwrap_or_default();
                if let Some(Drag::Move(id)) = &self.drag {
                    painter.text(ptr + Vec2::new(14.0, -6.0), egui::Align2::LEFT_BOTTOM, self.work.iter().find(|x| s_of(x, "id") == id).map(title_of).unwrap_or_default(), egui::FontId::proportional(1.0 * r), Color32::WHITE);
                    let my_row = rects.iter().find(|(i, _, _)| i == id).map(|t| t.1);
                    if let Some((_, tri, trect)) = rects.iter().find(|(i, _, rc)| i != id && rc.contains(ptr)) {
                        if Some(*tri) == my_row { painter.rect_stroke(*trect, 3.0, Stroke::new(3.0_f32, accent), egui::StrokeKind::Inside); }
                        else {
                            let f = (ptr.x - trect.min.x) / trect.width().max(1.0);
                            if f < 0.22 { painter.rect_filled(Rect::from_min_max(trect.min, Pos2::new(trect.min.x + 6.0, trect.max.y)), 2.0, accent); }
                            else if f > 0.78 { painter.rect_filled(Rect::from_min_max(Pos2::new(trect.max.x - 6.0, trect.min.y), trect.max), 2.0, accent); }
                            else { let rr = rects.iter().filter(|(_, ri, _)| ri == tri).fold(*trect, |a, (_, _, b)| a.union(*b)); painter.rect_stroke(rr, 3.0, Stroke::new(3.0_f32, accent), egui::StrokeKind::Inside); }
                        }
                    }
                }
            });
            // A on a bar with the pad: start resizing it (a row that fills stops filling, as grabbing it with the mouse does)
            if let Some(p) = pad_start.take() {
                if let PadDrag::Height { id, .. } = &p {
                    if self.work.iter().any(|sh| s_of(sh, "id") == id && sh.get("fill").and_then(|v| v.as_bool()).unwrap_or(false)) { edits.push(Edit::Fill(id.clone(), false)); }
                }
                self.pad_drag = Some(p);
            }
            if self.pad_drag.is_some() { ui.label(egui::RichText::new("Resizing with the pad: the arrows move the bar, A or B finishes.").size(0.85 * r).color(accent)); }
            // the note under the map
            ui.label(egui::RichText::new(if over { format!("The layout is {:.0} vh tall: the page scrolls {:.0} vh past the bottom of this screen.", content_vh, content_vh - 100.0) } else { "The layout fits this screen.".to_string() })
                .size(0.8 * r).color(if over { Color32::from_rgb(0xe0, 0x9a, 0x5a) } else { ui_kit::MUTED() }));

            // a drag in progress
            let ptr = pointer.unwrap_or_default();
            match &self.drag {
                Some(Drag::Height { id, vh0, y0 }) => {
                    let mut vh = (*vh0 + (ptr.y - *y0) / (s * vh_px)).round();
                    // a row cannot be dragged taller than the window leaves room for: the rows below keep their own heights (a row that fills never goes under its saved height),
                    // so past that point the page would run off the screen. A layout that already overflows may still shrink.
                    vh = vh.min(height_room(&self.work, id).max(*vh0).floor());
                    edits.push(Edit::Height(id.clone(), vh as i64));
                }
                Some(Drag::Cols { a, b, pa, pb, x0, wa0, wb0 }) => {
                    let share = ((*pa + (ptr.x - *x0)) / (*pa + *pb)).clamp(0.1, 0.9);
                    let total = wa0 + wb0; edits.push(Edit::Width(a.clone(), (total * share) as f64)); edits.push(Edit::Width(b.clone(), (total * (1.0 - share)) as f64));
                }
                _ => {}
            }
            if self.drag.is_some() && !down {
                if let Some(Drag::Move(id)) = self.drag.take() {
                    let my_row = rects.iter().find(|(i, _, _)| *i == id).map(|t| t.1);
                    if let Some((tid, tri, trect)) = rects.iter().find(|(i, _, rc)| *i != id && rc.contains(ptr)) {
                        if Some(*tri) == my_row { edits.push(Edit::Reorder(id.clone(), tid.clone())); }
                        else {
                            let f = (ptr.x - trect.min.x) / trect.width().max(1.0);
                            if f < 0.22 { edits.push(Edit::DropSplit { dragged: id.clone(), target: tid.clone(), before: Some(tid.clone()) }); }
                            else if f > 0.78 {
                                let members: Vec<String> = rects.iter().filter(|(_, ri, _)| ri == tri).map(|(i, _, _)| i.clone()).collect();
                                let at = members.iter().position(|m| m == tid).unwrap_or(0);
                                edits.push(Edit::DropSplit { dragged: id.clone(), target: tid.clone(), before: members.get(at + 1).cloned() });
                            } else { edits.push(Edit::MoveRow(id.clone(), tid.clone())); }
                        }
                    }
                }
                self.drag = None;
            }

            // the inspector: everything about the selected shelf
            ui.add_space(0.4 * r);
            if self.prio.is_some() { self.prio_panel(ui, r, accent, &mut edits); } else { egui::Frame::new().fill(Color32::from_rgb(18, 28, 40)).stroke(Stroke::new(1.0_f32, Color32::from_rgb(60, 95, 125))).corner_radius(5.0).inner_margin(egui::Margin::same((0.6 * r) as i8)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                let Some(sid) = self.sel.clone() else { ui.label(ui_kit::muted("Click a shelf in the map to edit it.", 0.88 * r)); return };
                let Some(i) = self.idx(&sid) else { return };
                let sh = self.work[i].clone();
                let row = he::row_of(&self.work, &sid);
                let n = row.len();
                let widget = widget_kind(&sh).is_some();
                ui_kit::style_fields(ui);
                ui.horizontal_wrapped(|ui| {
                    ui.label(ui_kit::muted("Title", 0.8 * r));
                    let mut t = s_of(&sh, "label").to_string();
                    if ui.add(egui::TextEdit::singleline(&mut t).hint_text(title_of(&sh)).desired_width(14.0 * r).font(egui::FontId::proportional(0.9 * r))).changed() { edits.push(Edit::Title(sid.clone(), t)); }
                    if !widget {
                        ui.label(ui_kit::muted("Covers", 0.8 * r));
                        let mut g = num(&sh, "limit", 10.0) as i64;
                        if ui.add(egui::DragValue::new(&mut g).range(0..=200).speed(0.2).custom_formatter(|v, _| if v == 0.0 { "all".to_string() } else { format!("{}", v as i64) })).changed() { edits.push(Edit::Games(sid.clone(), g)); }
                    }
                    ui.label(ui_kit::muted(if sh.get("fill").and_then(|v| v.as_bool()).unwrap_or(false) { "Minimum height" } else { "Height" }, 0.8 * r));
                    let mut h = num(&self.work[row[0]], "row_height", 30.0).round() as i64;
                    if ui.add(egui::DragValue::new(&mut h).range(he::MIN_ROW_VH..=he::MAX_ROW_VH).speed(0.2).suffix(" vh")).changed() { edits.push(Edit::Height(sid.clone(), h)); }
                    let mut fill = sh.get("fill").and_then(|v| v.as_bool()).unwrap_or(false);
                    if ui.checkbox(&mut fill, egui::RichText::new("Fill the rest").color(ui_kit::TEXT()).size(0.85 * r)).on_hover_text("This row takes whatever height the other rows leave of the window (never less than its Height).").changed() { edits.push(Edit::Fill(sid.clone(), fill)); }
                    if n > 1 {
                        ui.label(ui_kit::muted("Width", 0.8 * r));
                        let mut w = num(&sh, "col_width", 10.0);
                        if ui.add(egui::DragValue::new(&mut w).range(0.5..=20.0).speed(0.05).fixed_decimals(1)).changed() { edits.push(Edit::Width(sid.clone(), w)); }
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    if ui_kit::button(ui, "ADD A SHELF TO THIS ROW...", Kind::Plain, accent, 0.9 * r).clicked() { edits.push(Edit::OpenSplit(sid.clone())); }
                    if he::rows(&self.work).len() > 1 && ui_kit::button(ui, "MOVE THIS SHELF TO ANOTHER ROW...", Kind::Plain, accent, 0.9 * r).on_hover_text("Put this shelf in another row, then choose its place in that row.").clicked() { edits.push(Edit::OpenMove(sid.clone())); }
                    if n > 1 && ui_kit::button(ui, "UNSPLIT", Kind::Plain, accent, 0.9 * r).clicked() { edits.push(Edit::Unsplit(sid.clone())); }
                    if ui_kit::button(ui, "HIDE", Kind::Plain, accent, 0.9 * r).clicked() { edits.push(Edit::Hide(sid.clone())); }
                    let asking = self.delete_ask.as_deref() == Some(sid.as_str());
                    if ui_kit::button(ui, if asking { "DELETE FOR GOOD? CLICK AGAIN" } else { "DELETE" }, if asking { Kind::Danger } else { Kind::Plain }, accent, 0.9 * r).clicked() {
                        if asking { edits.push(Edit::Delete(sid.clone())); self.delete_ask = None; } else { self.delete_ask = Some(sid.clone()); }
                    }
                });
                // what the shelf holds: games, or one of the widgets
                ui.horizontal_wrapped(|ui| {
                    ui.label(ui_kit::muted("Shows", 0.8 * r));
                    if ui_kit::pill(ui, "GAMES", !widget, accent, 0.7 * r).clicked() && widget { edits.push(Edit::Kind(sid.clone(), None)); }
                    if ui_kit::pill(ui, "WIDGET", widget, accent, 0.7 * r).clicked() && !widget { edits.push(Edit::Kind(sid.clone(), Some("clock"))); }
                    if let Some(cur) = widget_kind(&sh) {
                        let name = |k: &str| match k { "clock" => "Clock", "completion_pie" => "Completion chart", "achievement_pie" => "Achievement chart", _ => k }.to_string();
                        egui::ComboBox::from_id_salt(("he_widget", sid.as_str())).selected_text(name(cur)).width(9.0 * r).show_ui(ui, |ui| {
                            for k in ["clock", "completion_pie", "achievement_pie"] { if ui.selectable_label(cur == k, name(k)).clicked() && cur != k { edits.push(Edit::Kind(sid.clone(), Some(k))); } }
                        });
                        if cur == "clock" {
                            // the clock's own settings, saved on the shelf like its other keys (clock_24h defaults on, clock_seconds off, clock_date on)
                            for (key, label, def, tip) in [("clock_24h", "24 hour", true, "Off shows 12 hour time with AM or PM."), ("clock_seconds", "Seconds", false, "Shows the seconds in the time."), ("clock_date", "Date", true, "Shows the day and date under the time.")] {
                                let on = sh.get(key).and_then(|v| v.as_bool()).unwrap_or(def);
                                if ui.checkbox(&mut { on }, egui::RichText::new(label).color(ui_kit::TEXT()).size(0.85 * r)).on_hover_text(tip).clicked() { edits.push(Edit::Set(sid.clone(), key, json!(!on))); }
                            }
                        }
                        let show = sh.get("show_title").and_then(|v| v.as_bool()).unwrap_or(false);
                        if ui.checkbox(&mut { show }, egui::RichText::new("Show the title above it").color(ui_kit::TEXT()).size(0.85 * r)).on_hover_text("Off by default, as on the web page. The title is the Title box above.").clicked() { edits.push(Edit::Set(sid.clone(), "show_title", json!(!show))); }
                    }
                });
                if !widget {
                    // what the shelf shows: its filter, sorting, platforms and whether a game on it is left off the shelves after it
                    let key = { let k = s_of(&sh, "filter_key"); if k.is_empty() { s_of(&sh, "preset") } else { k } }.to_string();
                    let key = if key.is_empty() { "all_games".to_string() } else { key };
                    let col = { let c = s_of(&sh, "sort_col"); if c.is_empty() { "date_added" } else { c } }.to_string();
                    let dir = { let d = s_of(&sh, "sort_dir"); if d.is_empty() { "DESC" } else { d } }.to_string();
                    ui.horizontal_wrapped(|ui| {
                        ui.label(ui_kit::muted("Filter", 0.8 * r));
                        let cur = self.filters.iter().find(|(k, _)| *k == key).map_or(key.clone(), |(_, l)| l.clone());
                        egui::ComboBox::from_id_salt(("he_filter", sid.as_str())).selected_text(cur).width(11.0 * r).show_ui(ui, |ui| {
                            let s = ui_kit::combo_search(ui, egui::Id::new(("he_filter", sid.as_str())), self.filters.len(), r);
                            let mut n = 0;
                            for (k, l) in self.filters.iter().filter(|(_, l)| s.matches(l)) {
                                let hit = ui.selectable_label(*k == key, l).clicked() || (s.enter && n == 0);
                                n += 1;
                                if hit { if *k != key { edits.push(Edit::Filter(sid.clone(), k.clone())); } if s.enter { ui.close(); } }
                            }
                            if n == 0 { ui.label(ui_kit::muted("Nothing matches", 0.85 * r)); }
                        });
                        ui.label(ui_kit::muted("Sorted by", 0.8 * r));
                        let cur = if col == "RANDOM()" { "Shuffle".to_string() } else { crate::settings_dialog::SHELF_SORTS.iter().find(|(k, _)| *k == col).map_or(col.clone(), |(_, l)| l.to_string()) };
                        egui::ComboBox::from_id_salt(("he_sort", sid.as_str())).selected_text(cur).width(11.0 * r).show_ui(ui, |ui| {
                            let s = ui_kit::combo_search(ui, egui::Id::new(("he_sort", sid.as_str())), crate::settings_dialog::SHELF_SORTS.len(), r);
                            let mut n = 0;
                            for (k, l) in crate::settings_dialog::SHELF_SORTS.iter().filter(|(_, l)| s.matches(l)) {
                                let hit = ui.selectable_label(col == *k, *l).clicked() || (s.enter && n == 0);
                                n += 1;
                                if hit { if col != *k { edits.push(Edit::Set(sid.clone(), "sort_col", json!(k))); } if s.enter { ui.close(); } }
                            }
                            if n == 0 { ui.label(ui_kit::muted("Nothing matches", 0.85 * r)); }
                        });
                        if col != "RANDOM()" { for (d, name) in [("DESC", "HIGH FIRST"), ("ASC", "LOW FIRST")] { if ui_kit::pill(ui, name, dir == d, accent, 0.7 * r).clicked() && dir != d { edits.push(Edit::Set(sid.clone(), "sort_dir", json!(d))); } } }
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.label(ui_kit::muted("Platforms", 0.8 * r));
                        let hidden: Vec<String> = sh.get("hidden_platforms").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
                        for (k, l) in &self.platforms { let shown = !hidden.contains(k); if ui_kit::pill(ui, l, shown, accent, 0.7 * r).clicked() { edits.push(Edit::Platform(sid.clone(), k.clone(), !shown)); } }
                        ui.add_space(0.6 * r);
                        let dedup = sh.get("dedup").map_or(true, |v| v.as_bool().unwrap_or(true));
                        if ui.checkbox(&mut { dedup }, egui::RichText::new("Skip games claimed by higher-priority shelves").color(ui_kit::TEXT()).size(0.85 * r)).on_hover_text("Takes part in shelf priority: a game that also qualifies for a higher-priority shelf is left off this one. Untick to show every game this shelf qualifies for. Change the order with SHELF PRIORITY...").clicked() { edits.push(Edit::Set(sid.clone(), "dedup", json!(!dedup))); }
                        if dedup {
                            let rank = priority_rank(&self.work, &sid).unwrap_or(0);
                            ui.label(ui_kit::muted(format!("Priority {rank}"), 0.8 * r)).on_hover_text("1 claims a duplicate game first; later shelves leave it out.");
                        }
                    });
                }
                if !widget {
                    let ri = rows.iter().position(|rw| rw.contains(&i)).unwrap_or(0);
                    let rh_px = row_vh.get(ri).copied().unwrap_or(30.0) * vh_px;
                    let weights: Vec<f32> = row.iter().map(|k| num(&self.work[*k], "col_width", 10.0).max(0.5) as f32).collect();
                    let xs = row_widths(sw_px, &weights);
                    let pos = row.iter().position(|k| *k == i).unwrap_or(0);
                    let (cw, ch, fit) = card_metrics(xs[pos].1, rh_px, vh_px, !s_of(&sh, "label").is_empty());
                    let limit = num(&sh, "limit", 10.0) as i64;
                    let how = if limit == 0 || limit as usize > fit { format!("{fit} fit across; the rest scroll sideways") } else if (limit as usize) < fit { format!("{fit} fit across; {limit} shown, room for {} more", fit - limit as usize) } else { format!("{fit} fit across, all shown") };
                    ui.label(ui_kit::muted(format!("{how}. Cards are {:.0} x {:.0} px on this screen.", cw, ch), 0.8 * r));
                }
            }); }

            // shelves that are hidden from the page
            let hidden: Vec<(String, String)> = self.work.iter().filter(|s| !visible(s)).map(|s| (s_of(s, "id").to_string(), title_of(s))).collect();
            if !hidden.is_empty() {
                ui.add_space(0.4 * r);
                ui.label(egui::RichText::new("Hidden shelves").color(ui_kit::TEXT()).strong().size(0.95 * r));
                ui.horizontal_wrapped(|ui| {
                    for (id, t) in hidden {
                        ui.label(egui::RichText::new(&t).color(ui_kit::MUTED()).size(0.88 * r));
                        if ui_kit::button(ui, "SHOW", Kind::Plain, accent, 0.9 * r).clicked() { edits.push(Edit::Show(id.clone())); }
                        if ui_kit::button(ui, "\u{2715}", Kind::Plain, accent, 0.9 * r).clicked() { edits.push(Edit::Delete(id)); }
                        ui.add_space(0.6 * r);
                    }
                });
            }
            ui.add_space(0.6 * r);
            ui.horizontal(|ui| {
                if ui_kit::button(ui, "+ ADD SHELF", Kind::Plain, accent, r).clicked() { edits.push(Edit::Add); }
                if ui_kit::button(ui, "SHELF PRIORITY...", Kind::Plain, accent, r).clicked() { edits.push(Edit::OpenPrio); }
                if ui_kit::button(ui, if self.reset_ask { "CLICK AGAIN TO RESET" } else { "RESET SHELVES" }, if self.reset_ask { Kind::Danger } else { Kind::Plain }, accent, r).clicked() {
                    if self.reset_ask { self.work = pd_filter::home::default_shelves().as_array().cloned().unwrap_or_default(); self.sel = None; self.reset_ask = false; } else { self.reset_ask = true; }
                }
                ui.add_space(2.0 * r);
                if ui_kit::button(ui, "CANCEL", Kind::Plain, accent, r).clicked() { out = EditorOut::Close; }
                if ui_kit::button(ui, "SAVE", Kind::Primary, accent, r).clicked() { out = EditorOut::Save(Vec::new()); }
            });
        }, |_, _| {});
        self.apply(edits);
        if let EditorOut::Save(..) = &out {
            out = EditorOut::Save(self.work.clone());
        }
        if res.closed { return EditorOut::Close; }
        out
    }

    /// PlayDate's Shelf Priority panel: which shelves claim a game first when it qualifies for several, and which take no part in that.
    fn prio_panel(&mut self, ui: &mut Ui, r: f32, accent: Color32, edits: &mut Vec<Edit>) {
        let titles: std::collections::HashMap<String, String> = self.work.iter().map(|s| (s_of(s, "id").to_string(), title_of(s))).collect();
        let Some(list) = self.prio.as_mut() else { return };
        ui.label(egui::RichText::new("Shelf priority").color(ui_kit::TEXT()).strong().size(1.1 * r));
        ui.label(ui_kit::muted("When the same game qualifies for several shelves, the shelf nearest the top claims it and the shelves below leave it out. Untick a shelf to take it out of this logic: it then shows every game it qualifies for, and its games do not hide from other shelves.", 0.85 * r));
        ui.add_space(0.4 * r);
        let (mut up, mut down): (Option<usize>, Option<usize>) = (None, None);
        let mut hover: Option<String> = None;
        let n = list.len();
        egui::ScrollArea::vertical().max_height(14.0 * r).show(ui, |ui| {
            for (k, (id, on)) in list.iter_mut().enumerate() {
                let row = ui.horizontal(|ui| {
                    ui.label(ui_kit::muted(format!("{}", k + 1), 0.85 * r));
                    ui.add_enabled_ui(k > 0, |ui| { if ui_kit::button(ui, "\u{25B2}", Kind::Plain, accent, 0.85 * r).clicked() { up = Some(k); } });
                    ui.add_enabled_ui(k + 1 < n, |ui| { if ui_kit::button(ui, "\u{25BC}", Kind::Plain, accent, 0.85 * r).clicked() { down = Some(k); } });
                    let t = titles.get(id).cloned().unwrap_or_default();
                    ui.checkbox(on, egui::RichText::new(t).color(if *on { ui_kit::TEXT() } else { ui_kit::MUTED() }).size(0.9 * r));
                });
                // the row's own extent counts (number, arrows, tick box and name, and the gaps between them: a widget under the pointer takes the hover from the row itself),
                // not the empty space to the right of the name, where clicking does nothing
                if ui.rect_contains_pointer(row.response.rect) { hover = Some(id.clone()); }
            }
        });
        self.prio_hover = hover;
        if let Some(k) = up { list.swap(k, k - 1); }
        if let Some(k) = down { list.swap(k, k + 1); }
        ui.add_space(0.6 * r);
        let (mut apply, mut cancel) = (false, false);
        ui.horizontal(|ui| {
            apply = ui_kit::button(ui, "APPLY", Kind::Primary, accent, r).clicked();
            cancel = ui_kit::button(ui, "CANCEL", Kind::Plain, accent, r).clicked();
        });
        if apply { edits.push(Edit::ApplyPrio(list.clone())); self.prio = None; self.prio_hover = None; } else if cancel { self.prio = None; self.prio_hover = None; }
    }

    /// MOVE TO ROW: which row, then where in it. The shelf being moved leaves its own row.
    /// The layout a previewed choice would give, the shelf to highlight and a shelf whose row to outline.
    fn simulate(&self, plan: &Plan) -> (Vec<Value>, Option<String>, Option<String>) {
        let mut w = self.work.clone();
        match plan {
            Plan::Join { moved, target, before } => { he::split_with(&mut w, target, moved, before.as_deref()); (w, Some(moved.clone()), Some(moved.clone())) }
            Plan::Alone { moved, before } => { he::move_to_new_row(&mut w, moved, before.as_deref()); (w, Some(moved.clone()), Some(moved.clone())) }
            Plan::Row { moved, row } => (w, moved.clone(), Some(row.clone())),
        }
    }

    /// A small copy of the Home map for the pickers: display only, showing the page as the highlighted choice would leave it (the shelf that moves is lit, its row outlined).
    /// `idle` is what it shows while no choice has the focus or the pointer.
    fn draw_preview(&self, ui: &mut Ui, r: f32, accent: Color32, idle: Plan, grow: f32) {
        let (plan, cap) = self.hl.clone().unwrap_or((idle, String::new()));
        let (w, mark, outline) = self.simulate(&plan);
        let (sw, sh) = self.screen();
        let heights = |w: &[Value], rows: &[Vec<usize>]| he::row_vhs(&rows.iter().map(|row| num(&w[row[0]], "row_height", 30.0) as f32).collect::<Vec<_>>(), &rows.iter().map(|row| w[row[0]].get("fill").and_then(|v| v.as_bool()).unwrap_or(false)).collect::<Vec<_>>());
        let content = |hs: &[f32]| 5.5 + hs.iter().sum::<f32>() + 0.5 * hs.len().saturating_sub(1) as f32;
        let rows = he::rows(&w);
        if rows.is_empty() { return; }
        let row_vh = heights(&w, &rows);
        let content_vh = content(&row_vh);
        // The area's size comes from the layout as it is now, never from the previewed result, so hovering between choices cannot resize it (the dialog is centred, so it
        // would jump about under the pointer). Joining a row never makes the page taller (the shelf takes that row's height and its old row can only vanish, leaving
        // some empty space below); only a new row can, so `grow` is the room to keep for one, and is 0 whenever that choice is not on offer.
        let now_rows = he::rows(&self.work);
        let now_vh = heights(&self.work, &now_rows);
        let reserved_vh = content(&now_vh) + grow;
        let vh_px = sh as f32 / 100.0;
        let avail_w = (ui.available_width() - 14.0).max(40.0);
        let max_h = (ui.ctx().content_rect().height() * 0.26).max(10.0 * r);
        let s = (avail_w / sw as f32).min(max_h / (reserved_vh * vh_px)).min(max_h / (content_vh * vh_px));
        let (map_w, _map_h) = (sw as f32 * s, content_vh * vh_px * s);
        // one caption size for both texts (and a fixed line), so nothing below moves when the text changes
        ui.label(if cap.is_empty() { ui_kit::muted("Preview of the Home page", 0.88 * r) } else { egui::RichText::new(cap).size(0.88 * r).color(accent) });
        let (canvas, _) = ui.allocate_exact_size(Vec2::new(avail_w + 14.0, reserved_vh * vh_px * s + 8.0), Sense::hover());
        let origin = Pos2::new(canvas.min.x + (avail_w + 14.0 - map_w) / 2.0, canvas.min.y + 4.0);
        let painter = ui.painter_at(canvas);
        painter.rect_filled(Rect::from_min_size(Pos2::new(origin.x, origin.y), Vec2::new(map_w, reserved_vh * vh_px * s)), 3.0, Color32::from_rgb(8, 12, 16));
        painter.rect_filled(Rect::from_min_size(origin, Vec2::new(map_w, 5.0 * vh_px * s)), 0.0, Color32::from_rgb(23, 26, 33));
        let outlined = outline.as_deref().map(|id| he::row_of(&w, id));
        let mut y = 5.5 * vh_px * s;
        let mut outlines: Vec<Rect> = vec![];   // drawn after every row, or the next row's box would cover an outline's lower edge
        for (ri, row) in rows.iter().enumerate() {
            let rh = row_vh[ri] * vh_px * s;
            let weights: Vec<f32> = row.iter().map(|k| num(&w[*k], "col_width", 10.0).max(0.5) as f32).collect();
            let gap = 0.4 * (sw as f32 / 100.0) * s;
            let total: f32 = weights.iter().sum();
            let usable = map_w - gap * row.len().saturating_sub(1) as f32;
            let (mut x, top) = (origin.x, origin.y + y);
            for (n, k) in row.iter().enumerate() {
                let wd = if row.len() == 1 { map_w } else { usable * weights[n] / total };
                let rect = Rect::from_min_size(Pos2::new(x, top), Vec2::new(wd, rh));
                let marked = mark.as_deref() == Some(s_of(&w[*k], "id"));
                painter.rect_filled(rect, 3.0, if marked { accent.linear_multiply(0.4) } else { Color32::from_rgb(22, 28, 38) });
                painter.rect_stroke(rect, 3.0, Stroke::new(if marked { 2.0 } else { 1.0_f32 }, if marked { accent } else { Color32::from_gray(0x44) }), egui::StrokeKind::Inside);
                let g = painter.layout_no_wrap(title_of(&w[*k]), egui::FontId::proportional((0.8 * r).min(rh * 0.5).max(6.0)), if marked { Color32::WHITE } else { ui_kit::TEXT() });
                painter.with_clip_rect(rect.shrink(2.0)).galley(rect.center() - g.size() / 2.0, g, ui_kit::TEXT());
                x += wd + gap;
            }
            if outlined.as_ref().map_or(false, |o| row.iter().any(|k| o.contains(k))) {
                outlines.push(Rect::from_min_size(Pos2::new(origin.x - 1.0, top - 1.0), Vec2::new(map_w + 2.0, rh + 2.0)));
            }
            y += rh + 0.5 * vh_px * s;
        }
        for o in outlines { painter.rect_stroke(o, 4.0, Stroke::new(2.0_f32, accent), egui::StrokeKind::Outside); }
    }

    /// Keep the choice to preview: the mouse's, unless the pad moved the focus since the mouse last moved.
    fn settle(&mut self, ui: &Ui, t: Track) {
        let moving = ui.input(|i| i.pointer.is_moving());
        let foc_plan = t.foc.as_ref().map(|f| f.0.clone());
        if foc_plan.is_some() && foc_plan != self.hl_foc { self.pad_wins = true; }
        if moving { self.pad_wins = false; }
        self.hl_foc = foc_plan;
        let next = if self.pad_wins { t.foc.or(t.hov) } else if self.hl.is_some() { t.hov } else { t.hov.or(t.foc) };   // the mouse's last choice stays; the pad's focus (which starts on the first button) only counts before anything was chosen
        if next.as_ref().map(|n| &n.0) != self.hl.as_ref().map(|n| &n.0) { ui.ctx().request_repaint(); }
        // between two buttons nothing is hovered: keep the last choice until another one is pointed at, so the map does not jump back
        if next.is_some() { self.hl = next; }
    }

    fn move_picker(&mut self, ui: &mut Ui, r: f32, accent: Color32, edits: &mut Vec<Edit>) {
        let Some(sp) = self.split.as_ref() else { return };
        let (a, b) = (sp.a.clone(), sp.b.clone());
        let Some(ia) = self.work.iter().position(|s| s_of(s, "id") == a) else { self.split = None; return };
        let mine = title_of(&self.work[ia]);
        let a_row = he::row_of(&self.work, &a);
        let mut back = false;
        let mut track = Track::default();
        // new rows are offered in the first step; one made from a shelf that shares its row is a whole extra row (a lone shelf just changes places)
        let grow = if b.is_none() && a_row.len() > 1 { num(&self.work[ia], "row_height", 30.0) as f32 + 0.5 } else { 0.0 };
        self.draw_preview(ui, r, accent, match &b { None => Plan::Row { moved: Some(a.clone()), row: a.clone() }, Some(bid) => Plan::Row { moved: Some(a.clone()), row: bid.clone() } }, grow);
        match b {
            None => {
                ui.label(egui::RichText::new(format!("Move \"{mine}\" to a row")).color(ui_kit::TEXT()).strong().size(1.1 * r));
                ui.label(ui_kit::muted("Choose a row for it to join (rows are listed top to bottom), or give it a row of its own. The map above shows the result. You will then choose its place in a row you join.", 0.82 * r));
                ui.add_space(0.4 * r);
                let mut chosen: Option<String> = None;
                let mut alone: Option<Option<String>> = None;
                egui::ScrollArea::vertical().max_height(22.0 * r).show(ui, |ui| {
                    let all = he::rows(&self.work);
                    for (n, row) in all.iter().enumerate() {
                        if row.iter().any(|i| a_row.contains(i)) { continue; }
                        let names: Vec<String> = row.iter().map(|i| title_of(&self.work[*i])).collect();
                        let first = s_of(&self.work[row[0]], "id").to_string();
                        let label = format!("Row {}:  {}", n + 1, names.join("  +  "));
                        let resp = ui_kit::button(ui, &label, Kind::Plain, accent, r);
                        track.see(&resp, Plan::Row { moved: Some(a.clone()), row: first.clone() }, format!("Join row {}", n + 1));
                        if resp.clicked() { chosen = Some(first); }
                    }
                    ui.add_space(0.3 * r);
                    ui.label(ui_kit::muted("Or a new row:", 0.82 * r));
                    for k in 0..=all.len() {
                        let before = all.get(k).map(|row| s_of(&self.work[row[0]], "id").to_string());
                        let mut w = self.work.clone();
                        if !he::move_to_new_row(&mut w, &a, before.as_deref()) { continue; }   // where it already is
                        let label = if k == 0 { "New row at the top".to_string() } else if k == all.len() { format!("New row at the bottom (below row {})", k) } else { format!("New row between rows {} and {}", k, k + 1) };
                        let resp = ui_kit::button(ui, &label, Kind::Plain, accent, r);
                        track.see(&resp, Plan::Alone { moved: a.clone(), before: before.clone() }, label.clone());
                        if resp.clicked() { alone = Some(before); }
                    }
                });
                if let Some(before) = alone { edits.push(Edit::NewRow { id: a.clone(), before }); self.split = None; return; }
                if let Some(c) = chosen { self.hl = None; self.hl_foc = None; if let Some(sp) = self.split.as_mut() { sp.b = Some(c); } }
            }
            Some(bid) => {
                let members: Vec<(String, String)> = he::row_of(&self.work, &bid).iter().map(|i| (s_of(&self.work[*i], "id").to_string(), title_of(&self.work[*i]))).collect();
                if members.is_empty() { self.split = None; return; }
                ui.label(egui::RichText::new(format!("Where should \"{mine}\" go in the row?")).color(ui_kit::TEXT()).strong().size(1.1 * r));
                ui.add_space(0.4 * r);
                let mut go: Option<Option<String>> = None;
                let first = ui_kit::button(ui, &format!("\u{27F5} Before {}", members[0].1), Kind::Plain, accent, r);
                if ui.memory(|m| m.focused().is_none()) { first.request_focus(); }
                track.see(&first, Plan::Join { moved: a.clone(), target: bid.clone(), before: Some(members[0].0.clone()) }, format!("Before {}", members[0].1));
                if first.clicked() { go = Some(Some(members[0].0.clone())); }
                for (k, m) in members.iter().enumerate() {
                    let next = members.get(k + 1);
                    let label = match next { Some(n) => format!("Between {} and {}", m.1, n.1), None => format!("After {} \u{27F6}", m.1) };
                    let resp = ui_kit::button(ui, &label, Kind::Plain, accent, r);
                    track.see(&resp, Plan::Join { moved: a.clone(), target: bid.clone(), before: next.map(|n| n.0.clone()) }, label.replace(" \u{27F6}", ""));
                    if resp.clicked() { go = Some(next.map(|n| n.0.clone())); }
                }
                if let Some(before) = go { edits.push(Edit::DropSplit { dragged: a.clone(), target: bid.clone(), before }); self.split = None; return; }
            }
        }
        self.settle(ui, track);
        ui.add_space(0.6 * r);
        if ui_kit::button(ui, "BACK", Kind::Plain, accent, r).clicked() { back = true; }
        if back { self.back_step(); }
    }

    /// PlayDate's split picker: which shelf, then where in the row.
    fn split_picker(&mut self, ui: &mut Ui, r: f32, accent: Color32, edits: &mut Vec<Edit>) {
        if self.split.as_ref().map_or(false, |sp| sp.mover) { return self.move_picker(ui, r, accent, edits); }
        let Some(sp0) = self.split.as_ref() else { return };
        let (a0, b0) = (sp0.a.clone(), sp0.b.clone());
        self.draw_preview(ui, r, accent, match &b0 { None => Plan::Row { moved: None, row: a0.clone() }, Some(b) => Plan::Row { moved: Some(b.clone()), row: a0.clone() } }, 0.0);
        let mut track = Track::default();
        let Some(sp) = self.split.as_mut() else { return };
        let a = sp.a.clone();
        let Some(ia) = self.work.iter().position(|s| s_of(s, "id") == a) else { self.split = None; return };
        let my_row = he::row_of(&self.work, &a);
        let adding = my_row.len() > 1;
        let mut make_new = false;
        match sp.b.clone() {
            None => {
                ui.label(egui::RichText::new("Add a shelf to this row").color(ui_kit::TEXT()).strong().size(1.1 * r));
                ui.label(ui_kit::muted(if adding { "Choose a shelf to add to this row, or make a new one. You will then choose where it goes.".to_string() } else { format!("Choose a shelf to put beside {}, or make a new one. You will then choose its position.", title_of(&self.work[ia])) }, 0.85 * r));
                ui.add_space(0.4 * r);
                let mut chosen = None;
                // the pad needs somewhere to stand: the button that opened this step is gone, so nothing has the focus
                let nb = ui_kit::button(ui, "+ NEW SHELF", Kind::Plain, accent, r);
                if ui.memory(|m| m.focused().is_none()) { nb.request_focus(); }
                track.see(&nb, Plan::Row { moved: None, row: a.clone() }, "A new shelf joins this row".to_string());
                if nb.clicked() { make_new = true; }
                ui.add_space(0.3 * r);
                egui::ScrollArea::vertical().max_height(26.0 * r).show(ui, |ui| {
                    for (i, s) in self.work.iter().enumerate() {
                        if i == ia || my_row.contains(&i) { continue; }
                        let in_other = he::row_of(&self.work, s_of(s, "id")).len() > 1;
                        ui.horizontal(|ui| {
                            let resp = ui_kit::button(ui, &title_of(s), Kind::Plain, accent, r);
                            track.see(&resp, Plan::Row { moved: Some(s_of(s, "id").to_string()), row: a.clone() }, format!("{} joins this row", title_of(s)));
                            if resp.clicked() { chosen = Some(s_of(s, "id").to_string()); }
                            if in_other { ui.label(egui::RichText::new("will leave its current row").size(0.78 * r).color(crate::ui_kit::warning())); }
                            if !visible(s) { ui.label(ui_kit::muted("hidden", 0.78 * r)); }
                        });
                    }
                });
                if let Some(b) = chosen { sp.b = Some(b); self.hl = None; self.hl_foc = None; }
            }
            Some(b) => {
                let bt = self.work.iter().find(|s| s_of(s, "id") == b).map(title_of).unwrap_or_default();
                ui.label(egui::RichText::new(format!("Where should \"{bt}\" go in the row?")).color(ui_kit::TEXT()).strong().size(1.1 * r));
                ui.add_space(0.4 * r);
                let members: Vec<(String, String)> = my_row.iter().map(|i| (s_of(&self.work[*i], "id").to_string(), title_of(&self.work[*i]))).collect();
                let mut go: Option<Option<String>> = None;
                let fb = ui_kit::button(ui, &format!("\u{27F5} Before {}", members[0].1), Kind::Plain, accent, r);
                if ui.memory(|m| m.focused().is_none()) { fb.request_focus(); }
                track.see(&fb, Plan::Join { moved: b.clone(), target: a.clone(), before: Some(members[0].0.clone()) }, format!("{bt} goes before {}", members[0].1));
                if fb.clicked() { go = Some(Some(members[0].0.clone())); }
                for (k, m) in members.iter().enumerate() {
                    let next = members.get(k + 1);
                    let label = match next { Some(n) => format!("Between {} and {}", m.1, n.1), None => format!("After {} \u{27F6}", m.1) };
                    let resp = ui_kit::button(ui, &label, Kind::Plain, accent, r);
                    let cap = match next { Some(n) => format!("{bt} goes between {} and {}", m.1, n.1), None => format!("{bt} goes after {}", m.1) };
                    track.see(&resp, Plan::Join { moved: b.clone(), target: a.clone(), before: next.map(|n| n.0.clone()) }, cap);
                    if resp.clicked() { go = Some(next.map(|n| n.0.clone())); }
                }
                if let Some(before) = go { edits.push(Edit::DropSplit { dragged: b, target: a.clone(), before }); self.split = None; return; }
            }
        }
        self.settle(ui, track);
        if make_new {
            // an empty shelf of its own to put beside: it is made now and goes on to the position step like any other choice
            let id = he::add_shelf(&mut self.work);
            if let Some(sp) = self.split.as_mut() { sp.b = Some(id.clone()); sp.new_id = Some(id); self.hl = None; self.hl_foc = None; }
        }
        ui.add_space(0.6 * r);
        if ui_kit::button(ui, "BACK", Kind::Plain, accent, r).clicked() { self.back_step(); }
    }
}
