//! The Edit Game dialog, laid out from the web one (`templates/modal_edit.html`, measured in headless Chromium: 71.4rem wide, a 44.4rem form column and
//! a 20rem art column). Stage 1: everything that only needs the database. The art buttons, Sync Steam Data, Fill Missing Data, HLTB matching and the
//! duplicate link need the backend and say so.

use crate::home::{self, EditFields, EditInfo};
use crate::ui_kit::{self, MUTED, TEXT};
use egui::{Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, Ui, Vec2};


#[derive(Clone, Copy, PartialEq)]
enum Tab { Stats, Info }

pub enum EditOut { None, Close, Save, Delete, OpenFolder, ChangeExecutable, Store, Toast(&'static str), Open(String), DupLink(i64), DupUnlink, DupApprove(i64), DupReject, SyncSteam, FillMissing, ProtondbRefresh, HltbRescrape, HltbNone, HltbApplied,
    /// Art tab actions; the number is the art type: 0 vertical, 1 horizontal, 2 icon.
    ArtRescrape(usize), ArtFromSteam(usize), ArtUrl(usize, String), ArtClear(usize), ArtCrop(usize), ArtBrowse(usize) }

enum PickMsg { Results(Vec<pd_filter::hltbclient::HltbData>), Applied(Result<(), String>) }
/// The inline "Other match" panel under the HLTB row (PlayDate's: a search box, the found pages, a pasted address).
struct HltbPick { query: String, results: Option<Vec<pd_filter::hltbclient::HltbData>>, manual: String, msg: String, busy: bool, tx: std::sync::mpsc::Sender<PickMsg>, rx: std::sync::mpsc::Receiver<PickMsg> }

pub struct EditDialog {
    pub appid: i64,
    pub old: EditFields,
    pub f: EditFields,
    info: EditInfo,
    exe_rx: Option<std::sync::mpsc::Receiver<Option<String>>>,
    tab: Tab,
    hrs: String,
    pill_in: [String; 4],
    texture: Option<egui::TextureHandle>,
    /// Pictures the art tabs have staged for this game (shown here; they reach the grid when the dialog is saved, see `artstore::commit`).
    staged: Vec<crate::artstore::Staged>,
    pub status: Option<String>,
    pub dup: home::DupInfo,
    pub dup_query: String,
    pub dup_results: Vec<(i64, String, String)>,
    pub dup_query_done: String,
    pub account_id: Option<String>,   // the Steam id, for the Achievements link
    hltb_act: Option<EditOut>,       // a ProtonDB / HLTB button the user pressed, returned from show
    pending_open: Option<String>,     // a link the user clicked, returned as EditOut::Open
    hltb_pick: Option<HltbPick>,
    /// The library database, for the inline HLTB match (set by the app).
    pub db: Option<std::path::PathBuf>,
    pub root: std::path::PathBuf,   // the data folder, for the horizontal and icon pictures
    art_kind: usize,
    art_tex: [Option<egui::TextureHandle>; 3],
    art_url: String,
    art_url_open: bool,
    hide_desc: bool,   // the list view's pane shows the description above the form
    /// What the active PAGYWOSG filter says about this game (set by the app).
    pub quals: Option<crate::pagywosg_dialog::PagPanel>,
    /// The header's store link for a game from another store ("View on GOG Store"): None when its source has no store page for it.
    pub store_text: Option<String>,
}

/// Minutes as the web's HLTB lines write them: `4h 44m`, `2h`, `0h`.
fn hrs_text(m: Option<i64>) -> String { let m = m.unwrap_or(0).max(0); if m % 60 == 0 { format!("{}h", m / 60) } else { format!("{}h {}m", m / 60, m % 60) } }

fn numeric(s: &mut String, decimals: bool) {
    let mut dot = false;
    s.retain(|c| if c.is_ascii_digit() { true } else if decimals && c == '.' && !dot { dot = true; true } else { false });
}

impl EditDialog {
    pub fn new(appid: i64, info: EditInfo, start_on_info: bool) -> EditDialog {
        let hrs = info.fields.playtime_mins.parse::<f64>().map(|m| format!("{:.1}", m / 60.0)).unwrap_or_default();
        EditDialog { staged: Vec::new(), hltb_act: None, exe_rx: None, appid, old: info.fields.clone(), f: info.fields.clone(), info, tab: if start_on_info { Tab::Info } else { Tab::Stats }, hrs, pill_in: Default::default(), texture: None, root: Default::default(), art_kind: 0, art_tex: [None, None, None], art_url: String::new(), art_url_open: false, status: None, dup: Default::default(), dup_query: String::new(), dup_results: Vec::new(), dup_query_done: String::new(), account_id: None, pending_open: None, hltb_pick: None, db: None, hide_desc: false, quals: None, store_text: None }
    }

    /// The game's title as typed in the form (the SteamGridDB picker searches by it).
    pub fn title_for_search(&self) -> String { self.f.name.clone() }
    /// The HLTB line again, after a match was saved (the other fields, and anything typed, stay as they are).
    pub fn refresh_hltb(&mut self, i: &EditInfo) { self.info.hltb = i.hltb.clone(); self.info.hltb_id = i.hltb_id; self.info.hltb_name = i.hltb_name.clone(); }
    fn pick_search(&mut self, ctx: &egui::Context, q: String) {
        let Some(p) = self.hltb_pick.as_mut() else { return };
        p.results = None; p.msg.clear();
        let (tx, ctx) = (p.tx.clone(), ctx.clone());
        std::thread::spawn(move || { let h = pd_filter::hltbclient::Hltb::new(std::sync::Arc::new(pd_filter::net::UreqHttp::new())); let _ = tx.send(PickMsg::Results(pd_filter::hltb_ops::search(&h, &q))); ctx.request_repaint(); });
    }
    fn pick_select(&mut self, ctx: &egui::Context, hid: i64, score: Option<i64>) {
        let (Some(p), Some(db)) = (self.hltb_pick.as_mut(), self.db.clone()) else { return };
        p.busy = true; p.msg = "Saving...".into();
        let (tx, ctx, appid) = (p.tx.clone(), ctx.clone(), self.appid);
        std::thread::spawn(move || {
            let day = pd_filter::backup::ts_to_date(&rusqlite::types::Value::Integer(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)));
            let r = rusqlite::Connection::open(&db).map_err(|e| e.to_string()).and_then(|c| { c.busy_timeout(std::time::Duration::from_secs(10)).ok(); let h = pd_filter::hltbclient::Hltb::new(std::sync::Arc::new(pd_filter::net::UreqHttp::new())); pd_filter::hltb_ops::select(&c, &h, appid, hid, score, &day) });
            let _ = tx.send(PickMsg::Applied(r)); ctx.request_repaint();
        });
    }
    pub fn has_cover(&self) -> bool { self.texture.is_some() }
    pub fn is_steam(&self) -> bool { self.info.platform == "steam" }
    /// The art tab shown: 0 vertical, 1 horizontal, 2 icon.
    pub fn art_kind(&self) -> usize { self.art_kind }
    pub fn set_art_kind(&mut self, k: usize) { self.art_kind = k.min(2); }
    /// SteamGridDB's page for this game with no API key: a Steam game by its id, anything else by a search for its name (a store game has no Steam id to look up).
    pub fn sgdb_page(&self) -> String {
        if self.is_steam() { format!("https://www.steamgriddb.com/steam/{}", self.appid) }
        else { format!("https://www.steamgriddb.com/search/grids?term={}", pd_filter::net::enc(self.f.name.trim())) }
    }
    pub fn install_path(&self) -> Option<&str> { self.info.install_path.as_deref() }
    pub fn name_for_toast(&self) -> String { self.info.fields.name.clone() }

    /// A field label with a small link arrow after it (the web's `Date Added ↗`, `Achievements ↗`).
    fn label_link(&mut self, ui: &mut Ui, text: &str, url: Option<String>, r: f32) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.4 * r;
            Self::label(ui, text, r);
            if let Some(u) = url {
                let l = ui.add(egui::Label::new(RichText::new("\u{2197}").size(0.92 * r).color(ui_kit::accent())).sense(Sense::click()));
                if l.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
                if l.clicked() { self.pending_open = Some(u); }
            }
        });
    }

    fn label(ui: &mut Ui, text: &str, r: f32) { ui.label(RichText::new(text).size(0.92 * r).color(TEXT())); }

    /// A single-line field in the web's colors; `w` in points.
    fn input(ui: &mut Ui, s: &mut String, w: f32, r: f32, size: f32) -> egui::Response { Self::input_hint(ui, s, w, r, size, "") }
    fn input_hint(ui: &mut Ui, s: &mut String, w: f32, r: f32, size: f32, hint: &str) -> egui::Response {
        ui.scope(|ui| {
            ui_kit::style_fields(ui);
            ui.visuals_mut().extreme_bg_color = crate::ui_kit::INPUT();
            { let wd = &mut ui.visuals_mut().widgets; for x in [&mut wd.inactive, &mut wd.hovered, &mut wd.active] { x.bg_stroke = Stroke::new(1.0_f32, crate::ui_kit::EDGE()); } }
            let target = if size > 0.95 { 2.95 } else { 2.7 };   // the web's 32px number boxes and 29px text boxes
            let vpad = ((target - size * 1.25) / 2.0 * r).max(0.0) as i8;
            ui.add(egui::TextEdit::singleline(s).desired_width(w).hint_text(hint).font(FontId::proportional(size * r)).margin(egui::Margin::symmetric((0.6 * r) as i8, vpad)))
        }).inner
    }

    pub(crate) fn dropdown(ui: &mut Ui, id: &str, current: &str, options: &[&str], w: f32, r: f32) -> Option<String> {
        let mut pick = None;
        ui.scope(|ui| {
            ui_kit::style_dropdown(ui, r);
            ui.spacing_mut().interact_size.y = 2.6 * r;
            { let v = ui.visuals_mut(); for x in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] { x.weak_bg_fill = crate::ui_kit::INPUT(); x.bg_fill = crate::ui_kit::INPUT(); x.bg_stroke = Stroke::new(1.0_f32, crate::ui_kit::EDGE()); } }
            egui::ComboBox::from_id_salt(id).width(w - 0.3 * r).selected_text(RichText::new(current).size(0.95 * r).color(TEXT())).show_ui(ui, |ui| {
                let s = ui_kit::combo_search(ui, egui::Id::new(id), options.len(), r);
                let mut n = 0;
                for o in options.iter().filter(|o| s.matches(o)) {
                    let hit = ui.selectable_label(*o == current, *o).clicked() || (s.enter && n == 0);
                    n += 1;
                    if hit { pick = Some(o.to_string()); if s.enter { ui.close(); } }
                }
                if n == 0 { ui.label(ui_kit::muted("Nothing matches", 0.85 * r)); }
            });
        });
        pick
    }

    /// Tags, genres, categories or groups: chips with a cross, and a box to type the next one (Enter adds it).
    pub(crate) fn pills(ui: &mut Ui, id: &str, items: &mut Vec<String>, input: &mut String, w: f32, min_h: f32, r: f32) {
        let accent = ui_kit::accent();
        egui::Frame::new().fill(crate::ui_kit::INPUT()).stroke(Stroke::new(1.0_f32, crate::ui_kit::EDGE())).corner_radius((0.25 * r) as u8).inner_margin(egui::Margin::symmetric((0.6 * r) as i8, (0.5 * r) as i8)).show(ui, |ui| {
            ui.set_width(w - 1.2 * r - 2.0);
            ui.set_min_height(min_h - r);
            ui.spacing_mut().item_spacing = Vec2::new(0.4 * r, 0.3 * r);
            ui.horizontal_wrapped(|ui| {
                let mut remove = None;
                for (i, t) in items.iter().enumerate() {
                    let g = ui.painter().layout_no_wrap(t.clone(), FontId::proportional(0.8 * r), accent);
                    let (rect, _) = ui.allocate_exact_size(Vec2::new(g.size().x + 2.1 * r, 1.5 * r), Sense::hover());
                    ui.painter().rect_filled(rect, (0.25 * r) as u8, Color32::from_rgb(0x1e, 0x36, 0x4e));
                    ui.painter().rect_stroke(rect, (0.25 * r) as u8, Stroke::new(1.0_f32, crate::ui_kit::EDGE()), egui::StrokeKind::Inside);
                    ui.painter().galley(Pos2::new(rect.min.x + 0.5 * r, rect.center().y - g.size().y / 2.0), g, accent);
                    let x = Rect::from_center_size(Pos2::new(rect.max.x - 0.8 * r, rect.center().y), Vec2::splat(1.2 * r));
                    let xr = ui.interact(x, ui.id().with((id, i)), Sense::click());
                    ui.painter().text(x.center(), Align2::CENTER_CENTER, "\u{00D7}", FontId::proportional(0.85 * r), if xr.hovered() { Color32::WHITE } else { MUTED() });
                    if xr.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
                    if xr.clicked() { remove = Some(i); }
                }
                if let Some(i) = remove { items.remove(i); }
                let resp = ui.add(egui::TextEdit::singleline(input).id(egui::Id::new(("pill_input", id))).frame(egui::Frame::NONE).desired_width(8.0 * r).font(FontId::proportional(0.92 * r)));
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    let v = input.trim().trim_matches(',').to_string();
                    if !v.is_empty() && !items.contains(&v) { items.push(v); }
                    input.clear();
                    resp.request_focus();
                }
            });
        });
        ui.label(ui_kit::muted("Type a value and press Enter to add", 0.75 * r));
    }

    pub fn show(&mut self, ctx: &egui::Context, covers: &mut home::Covers) -> EditOut {
        let accent = ui_kit::accent();
        let mut out = EditOut::None;
        let r = ui_kit::rem(ctx);
        // the cover for the art panel, as a texture
        let art_w = (20.0 * r) as u32;
        covers.request(self.appid, art_w);
        let v_cleared = self.staged.iter().any(|s| s.kind == crate::artstore::Kind::Vertical && s.cleared);
        let v_staged = self.staged.iter().any(|s| s.kind == crate::artstore::Kind::Vertical && !s.cleared);
        if self.texture.is_none() && v_staged {
            // a staged cover is read from its file, not from the library's cache
            if let Some(im) = crate::artstore::current_file(&self.root, crate::artstore::Kind::Vertical, self.appid).and_then(|p| image::open(p).ok()) {
                let im = im.thumbnail(art_w.max(64), art_w.max(64) * 2).to_rgba8();
                self.texture = Some(ctx.load_texture("edit-cover", egui::ColorImage::from_rgba_unmultiplied([im.width() as usize, im.height() as usize], im.as_raw()), egui::TextureOptions::LINEAR));
            }
        } else if self.texture.is_none() && !v_cleared {
            if let Some(px) = covers.get(self.appid, art_w) {
                let rgba: Vec<u8> = px.px.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8, 255]).collect();
                self.texture = Some(ctx.load_texture("edit-cover", egui::ColorImage::from_rgba_unmultiplied([px.w, px.h], &rgba), egui::TextureOptions::LINEAR));
            } else { ctx.request_repaint_after(std::time::Duration::from_millis(30)); }
        }
        let res = ui_kit::dialog_shell(ctx, "edit_game", 71.4, true, |ui, r| {
            let w = ui.available_width();
            // ---- header, as PlayDate: the title, the action buttons and the close cross on one row. A non-Steam game that is installed has a fifth button
            // (CHANGE EXECUTABLE) and the five ran over the title, so that one keeps the buttons on a row of their own.
            let red = Some((crate::ui_kit::DANGER(), crate::ui_kit::DANGER()));
            let mut items: Vec<(&'static str, String, Option<(Color32, Color32)>)> = vec![
                ("sync", format!("SYNC {} DATA", pd_filter::sources::platform_label(&self.info.platform).to_uppercase()), Some((Color32::TRANSPARENT, accent))),
                ("fill", "FILL MISSING DATA".to_string(), None)];
            let five = self.info.platform != "steam" && self.info.fields.installed;
            if five { items.push(("exe", "CHANGE EXECUTABLE".to_string(), None)); }
            items.push(("folder", "OPEN FOLDER".to_string(), None)); items.push(("delete", "DELETE GAME".to_string(), red));
            let act = |id: &str| match id { "sync" => EditOut::SyncSteam, "fill" => EditOut::FillMissing, "exe" => EditOut::ChangeExecutable, "folder" => EditOut::OpenFolder, _ => EditOut::Delete };
            ui.horizontal(|ui| {
                ui.label(RichText::new("Edit Game Details").font(crate::theme::bold(1.5 * r)).color(crate::ui_kit::heading()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let x = ui.add(egui::Label::new(RichText::new("\u{2715}").size(1.3 * r).color(MUTED())).sense(Sense::click()));
                    if x.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
                    if x.clicked() { out = EditOut::Close; }
                    if !five { for (id, text, fill) in items.iter().rev() { if ui_kit::nav_button_sized(ui, text, 2.35 * r, r, *fill).clicked() { out = act(id); } } }
                });
            });
            if five { ui.horizontal_wrapped(|ui| { for (id, text, fill) in &items { if ui_kit::nav_button_sized(ui, text, 2.35 * r, r, *fill).clicked() { out = act(id); } } }); }
            // ---- the game's name, id and store link
            ui.horizontal(|ui| {
                ui.label(RichText::new(&self.old.name).font(crate::theme::bold(1.0 * r)).color(crate::ui_kit::heading()));
                // a game from another store says which one ("GOG ID:"), whether or not that store's source is still around (PlayDate's 4088afa9)
                let id_label = if self.is_steam() { "AppID".to_string() } else { format!("{} ID", pd_filter::sources::platform_label(&self.info.platform)) };
                ui.label(ui_kit::muted(format!("{id_label}: {}", self.appid), 0.85 * r));
                let store_text = if self.is_steam() { Some("View on Steam Store".to_string()) } else { self.store_text.clone() };
                if let Some(text) = store_text {
                    let l = ui.add(egui::Label::new(RichText::new(format!("{text} \u{2197}")).size(0.8 * r).color(accent)).sense(Sense::click()));
                    if l.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
                    if l.clicked() { out = EditOut::Store; }
                }
            });
            // ---- duplicates: the copy this one is hidden under (Unlink), or a search to link one, a pending suggestion, and the copies hidden under this one
            ui.horizontal_wrapped(|ui| {
                ui.label(ui_kit::muted("Duplicate of:", 0.85 * r));
                if let Some((id, name, plat)) = self.dup.linked.clone() {
                    ui.label(RichText::new(format!("{name}  \u{b7}  {}", home::platform_label(&plat))).size(0.85 * r).color(crate::ui_kit::heading()));
                    ui.label(ui_kit::muted(format!("({id})"), 0.75 * r));
                    if ui_kit::nav_button_sized(ui, "UNLINK", 2.0 * r, r * 0.8, None).clicked() { out = EditOut::DupUnlink; }
                } else {
                    ui.scope(|ui| {
                        ui_kit::style_fields(ui);
                        ui.visuals_mut().extreme_bg_color = crate::ui_kit::INPUT();
                        { let wd = &mut ui.visuals_mut().widgets; for x in [&mut wd.inactive, &mut wd.hovered, &mut wd.active] { x.bg_stroke = Stroke::new(1.0_f32, crate::ui_kit::EDGE()); } }
                        ui.add(egui::TextEdit::singleline(&mut self.dup_query).hint_text(RichText::new("Search your library...").size(0.7 * r)).desired_width(18.5 * r).font(FontId::proportional(0.8 * r)));
                    });
                }
            });
            if self.dup.linked.is_none() && !self.dup_query.trim().is_empty() {
                // results, as a list under the box (a click links)
                egui::Frame::new().fill(crate::ui_kit::INPUT()).stroke(Stroke::new(1.0_f32, crate::ui_kit::EDGE())).corner_radius((0.25 * r) as u8).inner_margin(egui::Margin::symmetric((0.5 * r) as i8, (0.3 * r) as i8)).show(ui, |ui| {
                    if self.dup_results.is_empty() { ui.label(ui_kit::muted("No matches", 0.8 * r)); }
                    for (id, name, plat) in self.dup_results.clone() {
                        let g = ui.painter().layout_no_wrap(format!("{name}  \u{b7}  {}", home::platform_label(&plat)), FontId::proportional(0.82 * r), Color32::WHITE);
                        let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width().max(g.size().x), g.size().y + 0.4 * r), Sense::click());
                        if resp.hovered() { ui.painter().rect_filled(rect, 3, ui_kit::HOVER()); ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
                        ui.painter().galley(Pos2::new(rect.min.x + 4.0, rect.center().y - g.size().y / 2.0), g, Color32::WHITE);
                        if resp.clicked() { out = EditOut::DupLink(id); }
                    }
                });
            }
            if let Some((tid, tname, tplat)) = self.dup.suggestion.clone() {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(format!("Possible duplicate of {tname} ({})", home::platform_label(&tplat))).size(0.82 * r).color(Color32::from_rgb(0xc8, 0xa0, 0x50)));
                    if ui_kit::nav_button_sized(ui, "LINK", 1.8 * r, r * 0.8, None).clicked() { out = EditOut::DupApprove(tid); }
                    if ui_kit::nav_button_sized(ui, "NOT A DUPLICATE", 6.2 * r, r * 0.8, None).clicked() { out = EditOut::DupReject; }
                });
            }
            if self.dup.hidden_copies > 0 {
                ui.label(ui_kit::muted(format!("{} other cop{} of this game {} hidden under it.", self.dup.hidden_copies, if self.dup.hidden_copies == 1 { "y" } else { "ies" }, if self.dup.hidden_copies == 1 { "is" } else { "are" }), 0.8 * r));
            }
            ui.add_space(0.4 * r);
            let (rule, _) = ui.allocate_exact_size(Vec2::new(w, 1.0), Sense::hover());
            ui.painter().rect_filled(rule, 0.0, ui_kit::RULE());
            ui.add_space(0.8 * r);

            // ---- form column | art column
            let (left_w, art_w_pt) = (44.4 * r, 20.2 * r);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.allocate_ui_with_layout(Vec2::new(left_w, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.set_width(left_w);
                    ui.spacing_mut().item_spacing = Vec2::new(0.9 * r, 0.3 * r);
                    // tabs
                    ui.horizontal(|ui| {
                        for (t, label) in [(Tab::Stats, "Stats"), (Tab::Info, "Info")] {
                            let on = self.tab == t;
                            let g = ui.painter().layout_no_wrap(label.to_string(), crate::theme::bold(0.85 * r), Color32::PLACEHOLDER);
                            let (rect, resp) = ui.allocate_exact_size(Vec2::new(g.size().x + 3.0 * r, 2.3 * r), Sense::click());
                            let ink = if on { accent } else if resp.hovered() { TEXT() } else { MUTED() };
                            ui.painter().galley(Pos2::new(rect.center().x - g.size().x / 2.0, rect.center().y - g.size().y / 2.0), g, ink);
                            if on { ui.painter().line_segment([Pos2::new(rect.min.x, rect.max.y), Pos2::new(rect.max.x, rect.max.y)], Stroke::new(2.0_f32, accent)); }
                            if resp.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
                            if resp.clicked() { self.tab = t; }
                        }
                    });
                    let (rule, _) = ui.allocate_exact_size(Vec2::new(left_w, 1.0), Sense::hover());
                    ui.painter().rect_filled(rule, 0.0, ui_kit::RULE());
                    ui.add_space(0.5 * r);
                    match self.tab { Tab::Stats => self.stats_tab(ui, left_w, r), Tab::Info => self.info_tab(ui, left_w, r) }
                    self.quals_ui(ui, left_w, r);
                });
                ui.add_space(3.0 * r);
                // the divider and the art column
                let top = ui.cursor().min.y;
                ui.allocate_ui_with_layout(Vec2::new(art_w_pt, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                    let x = ui.min_rect().min.x - 1.5 * r;
                    ui.painter().line_segment([Pos2::new(x, top), Pos2::new(x, top + 36.0 * r)], Stroke::new(1.0_f32, ui_kit::RULE()));
                    ui.set_width(art_w_pt);
                    ui.spacing_mut().item_spacing = Vec2::new(0.5 * r, 0.5 * r);
                    self.art_panel(ui, art_w_pt, r, &mut out);
                });
            });
            ui.add_space(0.8 * r);
            let (rule, _) = ui.allocate_exact_size(Vec2::new(w, 1.0), Sense::hover());
            ui.painter().rect_filled(rule, 0.0, ui_kit::RULE());
            ui.add_space(0.5 * r);
            // ---- footer
            if let Some(s) = &self.status { ui.label(RichText::new(s).size(0.85 * r).color(crate::ui_kit::text_danger())); }
            ui.horizontal(|ui| {
                let gap = ui.spacing().item_spacing.x;
                let total = w - gap;
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(total * 0.64, 3.35 * r), Sense::click());
                let fill = crate::ui_kit::positive();
                ui.painter().rect_filled(rect, (0.25 * r) as u8, if resp.hovered() { fill.gamma_multiply(1.2) } else { fill });
                ui.painter().text(rect.center(), Align2::CENTER_CENTER, "SAVE CHANGES", crate::theme::bold(1.12 * r), Color32::WHITE);
                if resp.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
                if resp.clicked() { out = EditOut::Save; }
                if ui_kit::nav_button_sized_w(ui, "CLOSE", total * 0.36, 3.35 * r, r).clicked() { out = EditOut::Close; }
            });
        });
        if res.closed && matches!(out, EditOut::None) { out = EditOut::Close; }
        if let Some(u) = self.pending_open.take() { out = EditOut::Open(u); }
        if let Some(a) = self.hltb_act.take() { out = a; }
        out
    }

    /// The list view's detail pane: the same fields as the dialog (Info with the description, then Stats, stacked as the web's `#detail-main` does),
    /// a header with the title, library and store link, and the footer buttons Save / Cancel / Sync / Delete / Art.
    pub fn show_embedded(&mut self, ui: &mut Ui, w: f32, r: f32, icon: Option<&egui::TextureHandle>) -> EditOut {
        let accent = ui_kit::accent();
        let mut out = EditOut::None;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.9 * r;
            if let Some(t) = icon {
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(3.6 * r), Sense::hover());
                ui.painter().image(t.id(), rect, Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)), Color32::WHITE);
            }
            ui.label(RichText::new(&self.old.name).font(crate::theme::bold(1.25 * r)).color(crate::ui_kit::heading()));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.is_steam() {
                    let l = ui.add(egui::Label::new(RichText::new("Store \u{2197}").size(0.78 * r).color(accent)).sense(Sense::click()));
                    if l.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
                    if l.clicked() { out = EditOut::Store; }
                }
                let plat = home::platform_label(&self.info.platform);
                let g = ui.painter().layout_no_wrap(plat, FontId::proportional(0.72 * r), MUTED());
                let (rect, _) = ui.allocate_exact_size(g.size() + Vec2::new(1.0 * r, 0.5 * r), Sense::hover());
                ui.painter().rect_filled(rect, 3, Color32::from_rgb(0x11, 0x1c, 0x29));
                ui.painter().rect_stroke(rect, 3, Stroke::new(1.0_f32, ui_kit::EDGE()), egui::StrokeKind::Inside);
                ui.painter().galley(rect.center() - g.size() / 2.0, g, MUTED());
            });
        });
        let (rule, _) = ui.allocate_exact_size(Vec2::new(w, 1.0), Sense::hover());
        ui.painter().rect_filled(rule, 0.0, ui_kit::RULE());
        ui.add_space(0.4 * r);
        // the description sits right under the header as small plain text (the web's `#detail-desc-text`), editable
        ui.scope(|ui| {
            ui.visuals_mut().extreme_bg_color = Color32::TRANSPARENT;
            { let wd = &mut ui.visuals_mut().widgets; for x in [&mut wd.inactive, &mut wd.hovered, &mut wd.active] { x.bg_stroke = Stroke::NONE; } }
            ui_kit::multiline_mark(ui.add(egui::TextEdit::multiline(&mut self.f.description).desired_width(w).desired_rows(2).hint_text("No description").font(FontId::proportional(0.85 * r)).text_color(MUTED()).margin(egui::Margin::symmetric((0.5 * r) as i8, (0.4 * r) as i8))));
        });
        let (rule, _) = ui.allocate_exact_size(Vec2::new(w, 1.0), Sense::hover());
        ui.painter().rect_filled(rule, 0.0, ui_kit::RULE());
        ui.add_space(0.4 * r);
        self.hide_desc = true;
        let footer_h = 4.4 * r;
        let body_h = (ui.available_height() - footer_h).max(4.0 * r);
        egui::ScrollArea::vertical().id_salt("detail_form").max_height(body_h).auto_shrink([false, false]).show(ui, |ui| {
            ui.set_width(w - 0.8 * r);
            ui.spacing_mut().item_spacing = Vec2::new(0.9 * r, 0.3 * r);
            let hltb = std::mem::take(&mut self.info.hltb);   // the Stats part shows it; once is enough
            self.info_tab(ui, w - 0.8 * r, r);
            self.info.hltb = hltb;
            ui.add_space(0.6 * r);
            let (rule, _) = ui.allocate_exact_size(Vec2::new(w - 0.8 * r, 1.0), Sense::hover());
            ui.painter().rect_filled(rule, 0.0, ui_kit::RULE());
            ui.add_space(0.6 * r);
            self.stats_tab(ui, w - 0.8 * r, r);
            self.quals_ui(ui, w - 0.8 * r, r);
        });
        let (rule, _) = ui.allocate_exact_size(Vec2::new(w, 1.0), Sense::hover());
        ui.painter().rect_filled(rule, 0.0, ui_kit::RULE());
        ui.add_space(0.3 * r);
        if let Some(st) = &self.status { ui.label(RichText::new(st).size(0.85 * r).color(crate::ui_kit::text_danger())); }
        ui.horizontal(|ui| {
            let gap = ui.spacing().item_spacing.x;
            let total = w - 4.0 * gap;
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(total * 2.0 / 6.0, 2.6 * r), Sense::click());
            let fill = crate::ui_kit::positive();
            ui.painter().rect_filled(rect, (0.25 * r) as u8, if resp.hovered() { fill.gamma_multiply(1.2) } else { fill });
            ui.painter().text(rect.center(), Align2::CENTER_CENTER, "SAVE", crate::theme::bold(0.95 * r), Color32::WHITE);
            if resp.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
            if resp.clicked() { out = EditOut::Save; }
            let unit = total / 6.0;
            if ui_kit::nav_button_sized_w(ui, "CANCEL", unit, 2.6 * r, r).clicked() { out = EditOut::Close; }
            if ui_kit::nav_button_sized_w(ui, "SYNC", unit, 2.6 * r, r).clicked() { out = EditOut::Toast("Sync needs the sync backend, not built yet"); }
            let (dr, dresp) = ui.allocate_exact_size(Vec2::new(unit, 2.6 * r), Sense::click());
            let red = crate::ui_kit::DANGER();
            ui.painter().rect_filled(dr, (0.25 * r) as u8, if dresp.hovered() { red.gamma_multiply(1.2) } else { red });
            ui.painter().text(dr.center(), Align2::CENTER_CENTER, "DELETE", crate::theme::bold(0.8 * r), Color32::WHITE);
            if dresp.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
            if dresp.clicked() { out = EditOut::Delete; }
            if ui_kit::nav_button_sized_w(ui, "ART", unit, 2.6 * r, r).clicked() { out = EditOut::Toast("The art editor is only in the Edit Game dialog for now"); }
        });
        if let Some(u) = self.pending_open.take() { out = EditOut::Open(u); }
        if let Some(a) = self.hltb_act.take() { out = a; }
        out
    }

    /// The PAGYWOSG qualifications of this game, under the active PAGYWOSG filter.
    fn quals_ui(&self, ui: &mut Ui, w: f32, r: f32) {
        let Some(p) = &self.quals else { return };
        if p.lines.is_empty() { return; }
        ui.add_space(0.6 * r);
        let (rule, _) = ui.allocate_exact_size(Vec2::new(w, 1.0), Sense::hover());
        ui.painter().rect_filled(rule, 0.0, ui_kit::RULE());
        ui.add_space(0.4 * r);
        crate::pagywosg_dialog::panel_ui(ui, r, p, false, ui_kit::MUTED(), ui_kit::TEXT());
    }

    fn stats_tab(&mut self, ui: &mut Ui, w: f32, r: f32) {
        let col = (w - 3.0 * 0.9 * r) / 4.0;
        let statuses = ["Never Played", "Unfinished", "Beaten", "Completed", "Won't Play"];
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| { ui.set_width(col); Self::label(ui, "Completion Status", r); if let Some(p) = Self::dropdown(ui, "edit_status", &self.f.completion_status.clone(), &statuses, col, r) { self.f.completion_status = p; } });
            ui.vertical(|ui| { ui.set_width(col); Self::label(ui, "Installed", r); if let Some(p) = Self::dropdown(ui, "edit_installed", if self.f.installed { "Yes" } else { "No" }, &["Yes", "No"], col, r) { self.f.installed = p == "Yes"; } });
            ui.vertical(|ui| {
                ui.set_width(col * 2.0 + 0.9 * r);
                Self::label(ui, "Playtime", r);
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.5 * r;
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.1 * r;
                        let before = self.f.playtime_mins.clone();
                        if Self::input(ui, &mut self.f.playtime_mins, col - 0.5 * r, r, 1.0).changed() { numeric(&mut self.f.playtime_mins, false); if self.f.playtime_mins != before { self.hrs = self.f.playtime_mins.parse::<f64>().map(|m| format!("{:.1}", m / 60.0)).unwrap_or_default(); } }
                        ui.label(ui_kit::muted("mins", 0.7 * r));
                    });
                    ui.add_space(0.3 * r);
                    ui.label(ui_kit::muted("=", 0.9 * r));
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.1 * r;
                        let before = self.hrs.clone();
                        if Self::input(ui, &mut self.hrs, col - 0.5 * r, r, 1.0).changed() { numeric(&mut self.hrs, true); if self.hrs != before { self.f.playtime_mins = self.hrs.parse::<f64>().map(|h| format!("{}", (h * 60.0).round() as i64)).unwrap_or_default(); } }
                        ui.label(ui_kit::muted("hrs", 0.7 * r));
                    });
                });
            });
        });
        if self.info.platform == "custom" {
            ui.add_space(0.3 * r);
            if let Some(p) = self.exe_rx.as_ref().and_then(|rx| rx.try_recv().ok()) { self.exe_rx = None; if let Some(p) = p { self.f.executable = p; } }
            Self::label(ui, "Launch", r);
            ui.horizontal(|ui| {
                Self::input(ui, &mut self.f.executable, w - 14.0 * r, r, 0.92);
                if ui_kit::nav_button_sized(ui, "BROWSE...", 2.7 * r, r * 0.8, None).clicked() && self.exe_rx.is_none() {
                    let (tx, rx) = std::sync::mpsc::channel(); self.exe_rx = Some(rx); let ctx = ui.ctx().clone();
                    std::thread::spawn(move || { let _ = tx.send(rfd::FileDialog::new().set_title("Choose the program to run").pick_file().map(|p| p.to_string_lossy().to_string())); ctx.request_repaint(); });
                }
                if ui_kit::nav_button_sized(ui, "CLEAR", 2.7 * r, r * 0.8, None).clicked() { self.f.executable.clear(); }
            });
            Self::input(ui, &mut self.f.launch_args, w, r, 0.92);
            ui.label(ui_kit::muted("Program to run for this game, then optional arguments. Installed follows whether the file exists.", 0.72 * r));
        }
        ui.add_space(0.3 * r);
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| { ui.set_width(col); Self::label(ui, "Last Played", r); Self::input(ui, &mut self.f.last_played, col, r, 0.92); });
            ui.vertical(|ui| { ui.set_width(col); let u = self.is_steam().then(|| format!("zest://steam-date/{}", self.appid)); self.label_link(ui, "Date Added", u, r); Self::input(ui, &mut self.f.date_added, col, r, 0.92); });
            ui.vertical(|ui| {
                ui.set_width(col * 2.0 + 0.9 * r);
                let ach = if self.is_steam() { self.account_id.as_ref().map(|id| format!("https://steamcommunity.com/{}/stats/{}/achievements/", if id.chars().all(|c| c.is_ascii_digit()) { format!("profiles/{id}") } else { format!("id/{id}") }, self.appid)) } else { None };
                self.label_link(ui, "Achievements", ach, r);
                ui.horizontal(|ui| {
                    if Self::input(ui, &mut self.f.unlocked, col - 0.5 * r, r, 1.0).changed() { numeric(&mut self.f.unlocked, false); }
                    ui.label(ui_kit::muted("/", 0.9 * r));
                    if Self::input(ui, &mut self.f.total_ach, col - 0.5 * r, r, 1.0).changed() { numeric(&mut self.f.total_ach, false); }
                });
            });
        });
        ui.add_space(0.5 * r);
        Self::label(ui, "Groups", r);
        Self::pills(ui, "groups", &mut self.f.groups, &mut self.pill_in[3], w, 3.2 * r, r);
        ui.add_space(0.4 * r);
        // read-only: ProtonDB and HLTB (their refresh and match buttons need the backend)
        let _row = |ui: &mut Ui, text: String| { egui::Frame::new().fill(crate::ui_kit::INPUT()).stroke(Stroke::new(1.0_f32, crate::ui_kit::EDGE())).corner_radius((0.25 * r) as u8).inner_margin(egui::Margin::symmetric((0.8 * r) as i8, (0.6 * r) as i8)).show(ui, |ui| { ui.set_width(w - 1.6 * r - 2.0); ui.label(RichText::new(text).size(0.85 * r).color(Color32::from_rgb(0xc2, 0xc8, 0xcc))); }); };
        // ProtonDB (Steam games always show the row so the tier can be fetched) and HLTB (always), each with its buttons, as PlayDate's edit form has them
        let frame = |ui: &mut Ui, body: &mut dyn FnMut(&mut Ui)| { egui::Frame::new().fill(crate::ui_kit::INPUT()).stroke(Stroke::new(1.0_f32, crate::ui_kit::EDGE())).corner_radius((0.25 * r) as u8).inner_margin(egui::Margin::symmetric((0.8 * r) as i8, (0.6 * r) as i8)).show(ui, |ui| { ui.set_width(w - 1.6 * r - 2.0); body(ui); }); };
        if !self.info.protondb.is_empty() || self.is_steam() {
            let tier = self.info.protondb.clone();
            let shown = if tier.is_empty() { "No rating yet".to_string() } else { let mut c = tier.chars(); c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default() };
            let col = match tier.as_str() { "platinum" => Color32::from_rgb(0xb4, 0xc7, 0xdc), "gold" => Color32::from_rgb(0xff, 0xd7, 0x00), "silver" => Color32::from_rgb(0xc0, 0xc0, 0xc0), "bronze" => Color32::from_rgb(0xcd, 0x7f, 0x32), "borked" => crate::ui_kit::text_danger(), _ => MUTED() };
            let appid = self.appid; let mut act: Option<EditOut> = None; let mut open: Option<String> = None;
            frame(ui, &mut |ui| {
                ui.horizontal(|ui| {
                    ui.label(ui_kit::muted("ProtonDB", 0.75 * r)); ui.label(RichText::new(shown.clone()).font(crate::theme::bold(0.85 * r)).color(col));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui_kit::nav_button_sized(ui, "Refresh", 1.9 * r, r * 0.8, None).clicked() { act = Some(EditOut::ProtondbRefresh); }
                        let l = ui.add(egui::Label::new(RichText::new("ProtonDB \u{2197}").size(0.78 * r).color(ui_kit::accent())).sense(Sense::click()));
                        if l.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
                        if l.clicked() { open = Some(format!("https://www.protondb.com/app/{appid}")); }
                    });
                });
            });
            if let Some(u) = open { self.pending_open = Some(u); }
            if let Some(a) = act { self.hltb_act = Some(a); }
        }
        {
            let text = if self.info.hltb.is_empty() { "No match yet".to_string() } else { self.info.hltb.clone() };
            let (hid, hname) = (self.info.hltb_id, self.info.hltb_name.clone());
            let mut act: Option<EditOut> = None; let mut open: Option<String> = None;
            let (mut toggle_pick, mut search, mut select): (bool, Option<String>, Option<(i64, Option<i64>)>) = (false, None, None);
            let ctx = ui.ctx().clone();
            if let Some(p) = self.hltb_pick.as_mut() {
                while let Ok(m) = p.rx.try_recv() { match m { PickMsg::Results(v) => p.results = Some(v), PickMsg::Applied(Ok(())) => { act = Some(EditOut::HltbApplied); } PickMsg::Applied(Err(e)) => { p.busy = false; p.msg = e; } } }
            }
            frame(ui, &mut |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(ui_kit::muted("HLTB:", 0.75 * r));
                    if let Some(id) = hid {
                        let name = if hname.is_empty() { "match".to_string() } else { hname.clone() };
                        let l = ui.add(egui::Label::new(RichText::new(format!("{name} \u{2197}")).size(0.78 * r).color(ui_kit::accent())).sense(Sense::click()));
                        if l.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
                        if l.clicked() { open = Some(format!("https://howlongtobeat.com/game/{id}")); }
                    }
                    if ui_kit::nav_button_sized(ui, "Other match", 2.0 * r, r * 0.8, None).clicked() { toggle_pick = true; }
                    if ui_kit::nav_button_sized(ui, "No page", 2.0 * r, r * 0.8, None).clicked() { act = Some(EditOut::HltbNone); }
                    if ui_kit::nav_button_sized(ui, "Re-scrape", 2.0 * r, r * 0.8, None).clicked() { act = Some(EditOut::HltbRescrape); }
                });
                ui.label(RichText::new(text.clone()).size(0.8 * r).color(TEXT()));
                if let Some(p) = self.hltb_pick.as_mut() {
                    ui.add_space(0.4 * r);
                    // the button takes its real width first (right to left), the box fills what is left, so nothing pushes the panel wider than its column
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let clicked = ui_kit::nav_button_sized(ui, "Search", 2.2 * r, r * 0.8, None).clicked();
                        let resp = Self::input(ui, &mut p.query, ui.available_width(), r, 0.85);
                        let go = clicked || (resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                        if go && !p.query.trim().is_empty() && !p.busy { search = Some(p.query.trim().to_string()); }
                    });
                    match &p.results {
                        None => { ui.label(ui_kit::muted("Searching...", 0.8 * r)); }
                        Some(v) if v.is_empty() => { ui.label(ui_kit::muted("Nothing found. Try other words, or paste the page's address below.", 0.8 * r)); }
                        Some(v) => { for d in v.iter().take(8) {
                            egui::Frame::new().fill(Color32::from_rgb(0x15, 0x1f, 0x2b)).corner_radius((0.2 * r) as u8).inner_margin(egui::Margin::symmetric((0.6 * r) as i8, (0.4 * r) as i8)).show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui_kit::nav_button_sized(ui, "Select", 2.2 * r, r * 0.8, None).clicked() && !p.busy { select = Some((d.id, Some(d.score))); }
                                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                        ui.label(RichText::new(&d.matched_name).size(0.85 * r).color(TEXT()));
                                        ui.label(ui_kit::muted(format!("{}", d.score), 0.8 * r));
                                        ui.label(ui_kit::muted(format!("Main: {} \u{b7} +Extras: {} \u{b7} Compl: {}", hrs_text(d.main), hrs_text(d.extras), hrs_text(d.completionist)), 0.75 * r));
                                    });
                                });
                            });
                        } }
                    }
                    ui.separator();
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let apply = ui_kit::nav_button_sized(ui, "Apply", 2.2 * r, r * 0.8, None).clicked();
                        Self::input_hint(ui, &mut p.manual, ui.available_width(), r, 0.85, "Paste HLTB URL or game ID\u{2026}");
                        if apply && !p.busy {
                            match pd_filter::hltb_ops::parse_hltb_id(&p.manual) { Some(id) => select = Some((id, None)), None => p.msg = "That is not a HowLongToBeat address or number.".into() }
                        }
                    });
                    if !p.msg.is_empty() { ui.label(ui_kit::muted(p.msg.clone(), 0.78 * r)); }
                }
            });
            if let Some(u) = open { self.pending_open = Some(u); }
            if toggle_pick {
                if self.hltb_pick.is_some() { self.hltb_pick = None; }
                else { let (tx, rx) = std::sync::mpsc::channel(); let q = self.f.name.clone(); self.hltb_pick = Some(HltbPick { query: q.clone(), results: None, manual: String::new(), msg: String::new(), busy: false, tx, rx }); self.pick_search(&ctx, q); }
            }
            if let Some(q) = search { self.pick_search(&ctx, q); }
            if let Some((hid, score)) = select { self.pick_select(&ctx, hid, score); }
            if matches!(act, Some(EditOut::HltbApplied)) { self.hltb_pick = None; }
            if let Some(a) = act { self.hltb_act = Some(a); }
        }
    }

    fn info_tab(&mut self, ui: &mut Ui, w: f32, r: f32) {
        let half = (w - 1.7 * r) / 2.0;
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 1.7 * r;
            ui.vertical(|ui| {
                ui.set_width(half);
                ui.spacing_mut().item_spacing.y = 0.1 * r;
                Self::label(ui, "Game Title", r); Self::input(ui, &mut self.f.name, half, r, 0.92);
                Self::label(ui, "Developers", r); Self::input(ui, &mut self.f.developers, half, r, 0.92);
                Self::label(ui, "Publishers", r); Self::input(ui, &mut self.f.publishers, half, r, 0.92);
                let two = (half - 0.9 * r) / 2.0;
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.9 * r;
                    ui.vertical(|ui| { ui.set_width(two); Self::label(ui, "Release Date", r); Self::input(ui, &mut self.f.release_date, two, r, 0.92); });
                    ui.vertical(|ui| { ui.set_width(two); Self::label(ui, "Free to Play", r); if let Some(p) = Self::dropdown(ui, "edit_free", if self.f.is_free { "Yes" } else { "No" }, &["No", "Yes"], two, r) { self.f.is_free = p == "Yes"; } });
                });
                Self::label(ui, "Review Score", r); Self::input(ui, &mut self.f.review_score, half, r, 0.92);
                let three = (half - 1.8 * r) / 3.0;
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.9 * r;
                    for (label, s) in [("Review %", &mut self.f.review_percentage), ("Weighted %", &mut self.f.weighted_percentage), ("Critic %", &mut self.f.metacritic_score)] {
                        ui.vertical(|ui| { ui.set_width(three); Self::label(ui, label, r); if Self::input(ui, s, three, r, 1.0).changed() { numeric(s, true); } });
                    }
                });
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.9 * r;
                    ui.vertical(|ui| { ui.set_width(two); Self::label(ui, "Total Reviews", r); if Self::input(ui, &mut self.f.total_reviews, two, r, 1.0).changed() { numeric(&mut self.f.total_reviews, false); } });
                    ui.vertical(|ui| { ui.set_width(two); Self::label(ui, "Positive Reviews", r); if Self::input(ui, &mut self.f.positive_reviews, two, r, 1.0).changed() { numeric(&mut self.f.positive_reviews, false); } });
                });
            });
            ui.vertical(|ui| {
                ui.set_width(half);
                Self::label(ui, "Tags", r); Self::pills(ui, "tags", &mut self.f.tags, &mut self.pill_in[0], half, 8.0 * r, r);
                Self::label(ui, "Genres", r); Self::pills(ui, "genres", &mut self.f.genres, &mut self.pill_in[1], half, 3.3 * r, r);
                Self::label(ui, "Categories", r); Self::pills(ui, "cats", &mut self.f.categories, &mut self.pill_in[2], half, 3.3 * r, r);
            });
        });
        ui.add_space(0.4 * r);
        if !self.hide_desc { Self::label(ui, "Description", r);
        ui.scope(|ui| {
            ui_kit::style_fields(ui);
            ui.visuals_mut().extreme_bg_color = crate::ui_kit::INPUT();
            { let wd = &mut ui.visuals_mut().widgets; for x in [&mut wd.inactive, &mut wd.hovered, &mut wd.active] { x.bg_stroke = Stroke::new(1.0_f32, crate::ui_kit::EDGE()); } }
            ui_kit::multiline_mark(ui.add(egui::TextEdit::multiline(&mut self.f.description).desired_width(w).desired_rows(3).margin(egui::Margin::symmetric((0.6 * r) as i8, (0.6 * r) as i8)).font(FontId::monospace(1.12 * r))));
        }); }
        if !self.info.hltb.is_empty() {
            ui.add_space(0.4 * r);
            egui::Frame::new().fill(crate::ui_kit::INPUT()).stroke(Stroke::new(1.0_f32, crate::ui_kit::EDGE())).corner_radius((0.25 * r) as u8).inner_margin(egui::Margin::symmetric((0.8 * r) as i8, (0.6 * r) as i8)).show(ui, |ui| { ui.set_width(w - 1.6 * r - 2.0); ui.label(RichText::new(format!("HLTB   {}", self.info.hltb)).size(0.85 * r).color(Color32::from_rgb(0xc2, 0xc8, 0xcc))); });
        }
    }

    /// The picture for art type `k` (1 horizontal, 2 icon) read from disk, scaled to the panel.
    fn art_texture(&mut self, ctx: &egui::Context, k: usize) {
        if self.art_tex[k].is_some() { return; }
        let Some(path) = crate::artstore::current_file(&self.root, crate::artstore::KINDS[k], self.appid) else { return };   // staged first; none after a staged clear
        if let Ok(im) = image::open(&path) {
            let im = im.thumbnail(640, 640).to_rgba8();
            self.art_tex[k] = Some(ctx.load_texture(format!("edit-art-{k}"), egui::ColorImage::from_rgba_unmultiplied([im.width() as usize, im.height() as usize], im.as_raw()), egui::TextureOptions::LINEAR));
        }
    }

    fn art_panel(&mut self, ui: &mut Ui, w: f32, r: f32, out: &mut EditOut) {
        let accent = ui_kit::accent();
        const KINDS: [&str; 3] = ["Vertical", "Horizontal", "Icon"];
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.4 * r;
            for (i, label) in KINDS.iter().enumerate() {
                let on = i == self.art_kind;
                let g = ui.painter().layout_no_wrap(label.to_string(), FontId::proportional(0.72 * r), Color32::PLACEHOLDER);
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(g.size().x + 1.4 * r, 1.7 * r), Sense::click());
                if on { ui.painter().rect_filled(rect, (0.25 * r) as u8, accent); } else { ui.painter().rect_stroke(rect, (0.25 * r) as u8, Stroke::new(1.0_f32, ui_kit::EDGE()), egui::StrokeKind::Inside); }
                ui.painter().galley(rect.center() - g.size() / 2.0, g, if on { ui_kit::surface() } else { MUTED() });
                if resp.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
                if resp.clicked() { self.art_kind = i; self.art_url_open = false; }
            }
        });
        let k = self.art_kind;
        if k > 0 { self.art_texture(ui.ctx(), k); }
        // the picture, in the card's own shape (icons square)
        let h = match k { 0 => w * 1.5, 1 => w * 353.0 / 616.0, _ => w.min(12.0 * r) };
        let pw = if k == 2 { h } else { w };
        let (rect, _) = ui.allocate_exact_size(Vec2::new(pw, h), Sense::hover());
        ui.painter().rect_filled(rect, (0.25 * r) as u8, crate::ui_kit::INPUT());
        let tex = if k == 0 { self.texture.as_ref() } else { self.art_tex[k].as_ref() };
        if let Some(t) = tex { ui.painter().image(t.id(), rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE); }
        let src = match self.info.art_sources[k].as_str() {
            "capsule_2x" => "Steam 2x Capsule", "capsule" => "Steam Capsule", "capsule_2x_steam" => "Steam 2x Capsule (matched by name)", "capsule_steam" => "Steam Capsule (matched by name)",
            "sgdb_grid" => "SteamGridDB Grid", "header" => "Steam Header", "header_steam" => "Steam Header (matched by name)", "sgdb_grid_wide" => "SteamGridDB Wide",
            "steam" => "Steam", "sgdb_icon" => "SteamGridDB Icon", "sgdb_icon_steam" => "SteamGridDB Icon (matched by name)", "custom" => "Custom", "missing" => "Missing",
            "store" => "Store (the library's own art)", "" => "\u{2014}", o => o,
        }.to_string();
        // the stored file's pixel size (header only, re-read at most once a second so a re-scrape or crop shows up)
        let dims = {
            let key = egui::Id::new(("art_dims", self.appid, k));
            let now = std::time::Instant::now();
            let cached: Option<(Option<(u32, u32)>, std::time::Instant)> = ui.ctx().data(|d| d.get_temp(key));
            match cached { Some((v, at)) if now.duration_since(at) < std::time::Duration::from_secs(1) => v,
                _ => { let v = crate::artstore::current_file(&self.root, crate::artstore::KINDS[k], self.appid).and_then(|p| image::image_dimensions(p).ok()); ui.ctx().data_mut(|d| d.insert_temp(key, (v, now))); v } }
        };
        let staged_here = self.staged.iter().find(|s| s.kind == crate::artstore::KINDS[k]).cloned();
        let src = match &staged_here { Some(st) if st.cleared => "none".to_string(), Some(st) => match st.source.as_str() { "custom" => "Custom".to_string(), o => { let t = o.to_string(); t } }, None => src };
        let unsaved = if staged_here.is_some() { "   (new: SAVE keeps it, CLOSE drops it)" } else { "" };
        ui.label(ui_kit::muted(match dims { Some((w, h)) => format!("Source: {src}   {w} x {h} px{unsaved}"), None => format!("Source: {src}{unsaved}") }, 0.72 * r));
        let kinds = ["vertical", "horizontal", "icon"];
        ui.horizontal_wrapped(|ui| {
            if ui_kit::nav_button_sized(ui, "RE-SCRAPE", 2.7 * r, r * 0.8, None).clicked() { *out = EditOut::ArtRescrape(k); }
            if self.info.platform != "steam" && ui_kit::nav_button_sized(ui, "FROM STEAM", 2.7 * r, r * 0.8, None).clicked() { *out = EditOut::ArtFromSteam(k); }
            if ui_kit::nav_button_sized(ui, "CROP", 2.7 * r, r * 0.8, None).clicked() { *out = EditOut::ArtCrop(k); }
            if ui_kit::nav_button_sized(ui, "SHOW FILE", 2.7 * r, r * 0.8, None).clicked() { *out = EditOut::Open(format!("file://cover/{}/{}", kinds[k], self.appid)); }
            if ui_kit::nav_button_sized(ui, "CUSTOM URL", 2.7 * r, r * 0.8, None).clicked() { self.art_url_open = !self.art_url_open; }
            if ui_kit::nav_button_sized(ui, "BROWSE SGDB \u{2197}", 2.7 * r, r * 0.8, None).clicked() { *out = EditOut::ArtBrowse(k); }
            if ui_kit::nav_button_sized(ui, "CLEAR", 2.7 * r, r * 0.8, None).clicked() { *out = EditOut::ArtClear(k); }
        });
        if self.art_url_open {
            ui_kit::style_fields(ui);
            ui.horizontal(|ui| {
                let resp = ui.add(egui::TextEdit::singleline(&mut self.art_url).hint_text("https://... or a file path").desired_width(w - 6.0 * r).font(FontId::proportional(0.9 * r)));
                let go = ui_kit::nav_button_sized(ui, "APPLY", 3.0 * r, r * 0.8, None).clicked() || (resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                if go && !self.art_url.trim().is_empty() { *out = EditOut::ArtUrl(k, self.art_url.trim().to_string()); }
            });
        }
    }

    /// After an art change: drop the pictures so they are read again.
    pub fn art_changed(&mut self) {
        self.staged = crate::artstore::staged(&self.root, self.appid); self.texture = None; self.art_tex = [None, None, None];
        pd_filter::net::log(&format!("edit dialog {}: art changed, staged now: {}", self.appid, self.staged.iter().map(|s| format!("{} from {}{}", s.kind.name(), s.source, if s.cleared { " (cleared)" } else { "" })).collect::<Vec<_>>().join(", ")));
    }
}
