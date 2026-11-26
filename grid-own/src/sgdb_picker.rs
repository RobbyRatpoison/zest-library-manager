//! The SteamGridDB picker (the web's `#sgdb-modal-overlay`): the images SteamGridDB has for the game (by Steam id), a name search for another game when there
//! are none, thumbnails loaded on background threads. Choosing one returns its address; the Edit dialog's art code downloads it.
use crate::ui_kit;
use egui::{Color32, Sense, Vec2};
use pd_filter::artclient::{Art, SgdbGame, SgdbOption};
use std::collections::{HashMap, HashSet};
use std::sync::{mpsc, Arc};

enum Msg { Options(Vec<SgdbOption>, bool), Games(Vec<SgdbGame>), Thumb(String, Option<(usize, usize, Vec<u8>)>) }
enum View { Grid(Vec<SgdbOption>), Games(Vec<SgdbGame>), Empty(String) }

pub struct SgdbPicker {
    pub appid: i64,
    pub kind: usize,
    art: Arc<Art>,
    search: String,
    view: View,
    cached: Option<Vec<SgdbOption>>,
    subtitle: String,
    loading: bool,
    thumbs: HashMap<String, Option<egui::TextureHandle>>,
    asked: HashSet<String>,
    tx: mpsc::Sender<Msg>, rx: mpsc::Receiver<Msg>,
    ctx: egui::Context,
}

pub enum PickOut { None, Close, Use(String) }

impl SgdbPicker {
    pub fn open(ctx: &egui::Context, art: Arc<Art>, appid: i64, kind: usize, name: String) -> SgdbPicker {
        let (tx, rx) = mpsc::channel();
        let mut p = SgdbPicker { appid, kind, art, search: name.clone(), view: View::Empty(String::new()), cached: None, subtitle: ["Vertical Capsule", "Horizontal Header", "Icon"][kind].into(), loading: true, thumbs: HashMap::new(), asked: HashSet::new(), tx, rx, ctx: ctx.clone() };
        p.start_options(appid > 0 || false);
        if appid <= 0 { p.search_by_name(); }
        p
    }

    fn akind(&self) -> pd_filter::artclient::Kind { [pd_filter::artclient::Kind::Vertical, pd_filter::artclient::Kind::Horizontal, pd_filter::artclient::Kind::Icon][self.kind] }

    fn start_options(&mut self, go: bool) {
        if !go { return; }
        let (art, tx, ctx, k, id) = (self.art.clone(), self.tx.clone(), self.ctx.clone(), self.akind(), self.appid);
        std::thread::spawn(move || { let o = art.sgdb_options(id, false, k); let _ = tx.send(Msg::Options(o, true)); ctx.request_repaint(); });
    }
    fn search_by_name(&mut self) {
        let term = self.search.trim().to_string();
        if term.is_empty() { return; }
        self.loading = true;
        let (art, tx, ctx) = (self.art.clone(), self.tx.clone(), self.ctx.clone());
        std::thread::spawn(move || { let g = art.sgdb_search(&term); let _ = tx.send(Msg::Games(g)); ctx.request_repaint(); });
    }
    fn load_game(&mut self, id: i64, name: &str) {
        self.loading = true; self.subtitle = name.to_string();
        let (art, tx, ctx, k) = (self.art.clone(), self.tx.clone(), self.ctx.clone(), self.akind());
        std::thread::spawn(move || { let o = art.sgdb_options(id, true, k); let _ = tx.send(Msg::Options(o, false)); ctx.request_repaint(); });
    }
    /// Thumbnails, three downloader threads over the new addresses.
    fn want_thumbs(&mut self, opts: &[SgdbOption]) {
        let urls: Vec<String> = opts.iter().map(|o| o.thumb.clone()).filter(|u| u.starts_with("http") && self.asked.insert(u.clone())).take(90).collect();
        for chunk in urls.chunks((urls.len() / 3).max(1)) {
            let (chunk, art, tx, ctx) = (chunk.to_vec(), self.art.clone(), self.tx.clone(), self.ctx.clone());
            std::thread::spawn(move || for u in chunk {
                let px = art.picture_bytes(&u).and_then(|b| image::load_from_memory(&b).ok()).map(|im| { let im = im.thumbnail(360, 360).to_rgba8(); (im.width() as usize, im.height() as usize, im.into_raw()) });
                let _ = tx.send(Msg::Thumb(u, px)); ctx.request_repaint();
            });
        }
    }

    fn pump(&mut self) {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Options(o, from_steam) => {
                    self.loading = false;
                    if o.is_empty() && from_steam && !self.search.trim().is_empty() { self.search_by_name(); continue; }
                    if from_steam { self.cached = Some(o.clone()); }
                    self.want_thumbs(&o);
                    self.view = if o.is_empty() { View::Empty("No results found. Try searching by name above.".into()) } else { View::Grid(o) };
                }
                Msg::Games(g) => { self.loading = false; self.view = if g.is_empty() { View::Empty("No games found for that name.".into()) } else { View::Games(g) }; }
                Msg::Thumb(u, px) => { let t = px.map(|(w, h, d)| self.ctx.load_texture(format!("sgdb-{u}"), egui::ColorImage::from_rgba_unmultiplied([w, h], &d), egui::TextureOptions::LINEAR)); self.thumbs.insert(u, t); }
            }
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> PickOut {
        crate::ui_kit::PAD_GEOMETRY.store(true, std::sync::atomic::Ordering::Relaxed);
        self.pump();
        let accent = ui_kit::accent();
        let mut out = PickOut::None;
        let (mut go_search, mut go_back, mut load): (bool, bool, Option<(i64, String)>) = (false, false, None);
        let raised = crate::ui_kit::INPUT();
        let rule = |ui: &mut egui::Ui| { let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover()); ui.painter().rect_filled(r, 0.0, ui_kit::EDGE()); };
        let res = ui_kit::modal(ctx, "sgdb_picker", "Browse SteamGridDB", 44.0, |ui, r| {
            ui_kit::style_fields(ui);
            ui.label(ui_kit::muted(self.subtitle.clone(), 0.75 * r));
            ui.add_space(0.2 * r);
            rule(ui);
            ui.add_space(0.4 * r);
            // the name search row, always there
            ui.horizontal(|ui| {
                let resp = ui.add(egui::TextEdit::singleline(&mut self.search).hint_text("Search by game name...").desired_width(ui.available_width() - 11.0 * r).font(egui::FontId::proportional(0.85 * r)).margin(egui::Margin::symmetric((0.6 * r) as i8, (0.4 * r) as i8)));
                if ui_kit::nav_button_sized(ui, "SEARCH", 2.0 * r, r * 0.8, None).clicked() || (resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) { go_search = true; }
                let can_back = self.cached.is_some() && (matches!(self.view, View::Games(_)) || self.subtitle != ["Vertical Capsule", "Horizontal Header", "Icon"][self.kind]);
                if can_back && ui_kit::nav_button_sized(ui, "BACK", 2.0 * r, r * 0.8, None).clicked() { go_back = true; }
            });
            ui.add_space(0.4 * r);
            rule(ui);
            if self.loading { ui.add_space(1.5 * r); ui.vertical_centered(|ui| ui.label(ui_kit::muted("Loading...", 0.9 * r))); ui.add_space(1.5 * r); }
            egui::ScrollArea::vertical().max_height(f32::INFINITY).auto_shrink([false, true]).show(ui, |ui| {
                match &self.view {
                    View::Empty(m) => { if !self.loading { ui.add_space(1.0 * r); ui.vertical_centered(|ui| ui.label(ui_kit::muted(m.clone(), 0.9 * r))); ui.add_space(1.0 * r); } }
                    View::Games(g) => for game in g {
                        let (row, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 2.4 * r), Sense::click());
                        if resp.hovered() { ui.painter().rect_filled(row, 0.0, raised); ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
                        ui.painter().text(egui::pos2(row.min.x + 1.2 * r, row.center().y), egui::Align2::LEFT_CENTER, &game.name, egui::FontId::proportional(0.9 * r), ui_kit::TEXT());
                        if game.verified {
                            let g = ui.painter().layout_no_wrap("Verified".into(), egui::FontId::proportional(0.7 * r), crate::ui_kit::positive());
                            let pill = egui::Rect::from_min_size(egui::pos2(row.max.x - g.size().x - 2.2 * r, row.center().y - g.size().y / 2.0 - 0.2 * r), g.size() + Vec2::new(1.2 * r, 0.4 * r));
                            ui.painter().rect_filled(pill, 10, Color32::from_rgba_unmultiplied(0x5c, 0x7e, 0x10, 38));
                            ui.painter().galley(pill.min + Vec2::new(0.6 * r, 0.2 * r), g, crate::ui_kit::positive());
                        }
                        ui.painter().line_segment([row.left_bottom(), row.right_bottom()], egui::Stroke::new(1.0_f32, ui_kit::EDGE()));
                        if resp.clicked() { load = Some((game.id, game.name.clone())); }
                    },
                    View::Grid(opts) => {
                        ui.add_space(0.6 * r);
                        let fit: Vec<&SgdbOption> = opts.iter().filter(|o| o.fits).collect(); let other: Vec<&SgdbOption> = opts.iter().filter(|o| !o.fits).collect();
                        let section = |ui: &mut egui::Ui, text: String, later: bool, list: &[&SgdbOption], out: &mut PickOut| {
                            if later { ui.add_space(0.8 * r); rule(ui); ui.add_space(0.6 * r); }
                            if !text.is_empty() { ui.label(ui_kit::muted(text, 0.78 * r)); ui.add_space(0.3 * r); }
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing = Vec2::splat(0.65 * r);
                                for o in list {
                                    let h = 18.75 * r;   // the web's 300px thumbnails
                                    let loaded = self.thumbs.get(&o.thumb);
                                    if matches!(loaded, Some(None)) { continue; }   // the thumbnail failed to load: left out, as the web does
                                    let tex = loaded.and_then(|t| t.as_ref());
                                    let aspect = tex.map_or(if o.width > 0 && o.height > 0 { o.width as f32 / o.height as f32 } else { 0.7 }, |t| t.size()[0] as f32 / t.size()[1] as f32);
                                    let (rect, resp) = ui.allocate_exact_size(Vec2::new(h * aspect + 4.0, h + 4.0), Sense::click());
                                    ui.painter().rect_filled(rect, 4, raised);
                                    if let Some(t) = tex { ui.painter().image(t.id(), rect.shrink(2.0), egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)), Color32::WHITE); }
                                    ui.painter().rect_stroke(rect, 4, egui::Stroke::new(2.0_f32, if resp.hovered() { accent } else { Color32::TRANSPARENT }), egui::StrokeKind::Inside);
                                    if resp.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
                                    if resp.on_hover_text(format!("{} x {}", o.width, o.height)).clicked() { *out = PickOut::Use(o.url.clone()); }
                                }
                            });
                        };
                        if !fit.is_empty() && !other.is_empty() {
                            section(ui, format!("Fits the card ({})", fit.len()), false, &fit, &mut out);
                            section(ui, format!("Different shape, the card shows a backdrop around these ({})", other.len()), true, &other, &mut out);
                        } else if !other.is_empty() { section(ui, "None of these match the card's shape, so the card shows a backdrop around the image.".into(), false, &other, &mut out); }
                        else { section(ui, String::new(), false, &fit, &mut out); }
                    }
                }
            });
        });
        if go_search { self.search_by_name(); }
        if go_back {
            self.search.clear(); self.subtitle = ["Vertical Capsule", "Horizontal Header", "Icon"][self.kind].into();
            if let Some(c) = self.cached.clone() { self.view = View::Grid(c); }
        }
        if let Some((id, name)) = load { self.load_game(id, &name); }
        if res.closed { return PickOut::Close; }
        out
    }
}
