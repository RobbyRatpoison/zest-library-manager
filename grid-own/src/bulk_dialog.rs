//! The Library Bulk Operations dialog, laid out from the web one (`bulk-edit-modal` in `templates/library.html`, captured in headless Chromium).
//! Edit tab works on its own; Re-scrape and Date Importer start sync jobs, and "Pick games" needs selection mode.

use crate::edit_dialog::EditDialog;
use crate::ui_kit::{self, Kind, MUTED, TEXT};
use egui::{Color32, RichText, Sense, Stroke, Vec2};

const COLUMNS: [(&str, &str); 22] = [
    ("completion_status", "completion_status"), ("tags", "tags"), ("groups", "groups"), ("developers", "developers"), ("publishers", "publishers"),
    ("release_date", "release_date"), ("review_score", "review_score"), ("review_percentage", "review_percentage"), ("weighted_percentage", "weighted_percentage"),
    ("total_reviews", "total_reviews"), ("positive_reviews", "positive_reviews"), ("unlocked_achievements", "unlocked_achievements"), ("total_achievements", "total_achievements"),
    ("genres", "genres"), ("categories", "categories"), ("is_free", "is_free"), ("playtime_forever", "playtime_forever"), ("date_added", "date_added"),
    ("installed", "installed"), ("vertical_art_source", "vertical_art_source"), ("horizontal_art_source", "horizontal_art_source"), ("icon_source", "icon_source"),
];
const LIST_COLUMNS: [&str; 4] = ["tags", "groups", "genres", "categories"];
const STATUSES: [&str; 5] = ["Never Played", "Unfinished", "Beaten", "Completed", "Won't Play"];
const WARN: Color32 = Color32::from_rgb(0xf0, 0xad, 0x4e);
const WARN_FILL: Color32 = Color32::from_rgb(0x2a, 0x21, 0x12);

#[derive(Clone, Copy, PartialEq)]
pub enum Scope { All, Filtered, Selected }

pub enum BulkOut {
    None,
    Close,
    /// Apply the edit to these games (None = every game in the library).
    Apply { scope: Scope, column: String, mode: String, value: String },
    Delete { scope: Scope },
    /// Re-fetch Steam data (store, reviews, tags, achievements) for these games.
    Rescrape { scope: Scope, meta: bool, cheevo: bool },
    /// Fetch ProtonDB tiers for these games.
    ProtonDb { scope: Scope },
    /// Match HowLongToBeat times for these games.
    Hltb { scope: Scope },
    /// The Date Importer tab's one button: store purchase dates for the other stores' games and Steam activation dates (read in the browser) for the Steam ones.
    FetchDates { scope: Scope },
    /// Fill blank fields from Steam and PCGamingWiki.
    Backfill { scope: Scope },
    /// Re-fetch cover art (vertical, horizontal, icon) by the Artwork Sources order.
    Art { scope: Scope, types: [bool; 3], source: &'static str, keep_custom: bool },
    /// Stop the running job.
    Stop,
    /// "Pick games": close this and let the user click the games in the Library.
    PickGames,
}

pub struct BulkDialog {
    scope: Scope,
    column: usize,
    mode: &'static str,
    value: String,
    status: String,
    items: Vec<String>,
    pill_in: String,
    /// Games the Filtered scope covers (what the Library shows now), and the whole library.
    pub shown: usize,
    pub total: usize,
    pub selected: usize,
    pub error: Option<String>,
    confirm_delete: bool,
    tab: usize,
    do_meta: bool, do_cheevo: bool, art_types: [bool; 3], art_source: usize, art_keep_custom: bool,
    /// The running job, set by the app before each frame: (what it is doing, total, finished, a rate-limit wait if any).
    pub job: Option<(String, usize, usize, Option<String>)>,
    want_pick: bool,
    suggestions: [Vec<String>; 4],
}

/// "completion_status" as the web's dropdown writes it: "Completion Status".
fn label(col: &str) -> String {
    col.split('_').map(|w| { let mut c = w.chars(); c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default() }).collect::<Vec<_>>().join(" ")
}

impl BulkDialog {
    pub fn new(shown: usize, total: usize, selected: usize, suggestions: [Vec<String>; 4]) -> BulkDialog {
        BulkDialog { scope: if selected > 0 { Scope::Selected } else { Scope::Filtered }, selected, column: 0, mode: "replace", value: String::new(), status: STATUSES[0].to_string(), items: Vec::new(), pill_in: String::new(), shown, total, error: None, confirm_delete: false, tab: 0, do_meta: true, do_cheevo: true, art_types: [true; 3], art_source: 0, art_keep_custom: false, job: None, want_pick: false, suggestions }
    }

    pub fn set_tab(&mut self, t: usize) { self.tab = t.min(1); }
    fn col(&self) -> &'static str { COLUMNS[self.column].0 }
    fn is_list(&self) -> bool { LIST_COLUMNS.contains(&self.col()) }
    fn count(&self) -> usize { match self.scope { Scope::All => self.total, Scope::Filtered => self.shown, Scope::Selected => self.selected } }
    fn value_text(&self) -> String {
        if self.col() == "completion_status" { self.status.clone() } else if self.is_list() { self.items.join(",") } else { self.value.clone() }
    }
    fn hint(&self) -> &'static str {
        match self.col() {
            "release_date" | "date_added" => "Format: YYYY-MM-DD",
            "completion_status" => "",
            "installed" | "is_free" => "Use 1 for yes, 0 for no",
            c if LIST_COLUMNS.contains(&c) => "Type a value and press Enter to add",
            _ => "",
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> BulkOut {
        let mut out = BulkOut::None;
        let _r = ui_kit::rem(ctx);
        let accent = ui_kit::accent();
        if self.confirm_delete {
            let n = self.count();
            let res = ui_kit::modal_footer(ctx, "bulk_delete", "Delete Games", 30.0, |ui, r| {
                ui.label(RichText::new(format!("Permanently delete {n} games? This removes them and their cover files and cannot be undone.")).size(0.95 * r).color(TEXT()));
            }, |ui, r| {
                ui.horizontal(|ui| {
                    if ui_kit::button(ui, "DELETE", Kind::Danger, accent, r).clicked() { out = BulkOut::Delete { scope: self.scope }; self.confirm_delete = false; }
                    if ui_kit::button(ui, "CANCEL", Kind::Plain, accent, r).clicked() { self.confirm_delete = false; }
                });
            });
            if res.closed { self.confirm_delete = false; }
            return out;
        }
        let res = ui_kit::modal(ctx, "bulk_edit", "Bulk Operations", 64.3, |ui, r| {
            let w = ui.available_width();
            ui_kit::style_fields(ui);
            // tab strip: three equal tabs over a rule (Edit, Re-scrape, Date Importer)
            let (strip, _) = ui.allocate_exact_size(Vec2::new(w, 2.4 * r), Sense::hover());
            ui.painter().line_segment([strip.left_bottom(), strip.right_bottom()], Stroke::new(2.0_f32, crate::ui_kit::EDGE()));
            for (i, t) in ["Edit", "Re-scrape", "Date Importer"].iter().enumerate() {
                let cell = egui::Rect::from_min_size(strip.min + Vec2::new(w / 3.0 * i as f32, 0.0), Vec2::new(w / 3.0, 2.4 * r));
                let resp = ui.interact(cell, egui::Id::new(("bulk_tab", i)), Sense::click());
                let on = i == self.tab;
                ui.painter().text(cell.center(), egui::Align2::CENTER_CENTER, *t, crate::theme::bold(0.85 * r), if on { accent } else { MUTED() });
                if on { ui.painter().line_segment([cell.left_bottom(), cell.right_bottom()], Stroke::new(2.0_f32, accent)); }
                if resp.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
                if resp.clicked() { self.tab = i; }
            }
            ui.add_space(0.4 * r);
            // scope: one bordered row of three cells with radio dots
            let (box_rect, _) = ui.allocate_exact_size(Vec2::new(w, 3.4 * r), Sense::hover());
            ui.painter().rect_stroke(box_rect, 5, Stroke::new(1.0_f32, crate::ui_kit::EDGE()), egui::StrokeKind::Inside);
            let cell_w = w / 3.0;
            let third = if self.selected > 0 { (Some(Scope::Selected), "Selected games".to_string(), format!("{} game{} selected", self.selected, if self.selected == 1 { "" } else { "s" })) } else { (None, "Pick games \u{2192}".to_string(), String::new()) };
            let mut pick_games = false;
            for (i, (sc, title, sub)) in [(Some(Scope::All), "All games".to_string(), String::new()),
                (Some(Scope::Filtered), "Filtered games".to_string(), format!("{} games currently shown", self.shown)), third].into_iter().enumerate() {
                let cell = egui::Rect::from_min_size(box_rect.min + Vec2::new(cell_w * i as f32, 0.0), Vec2::new(cell_w, box_rect.height()));
                let resp = ui.interact(cell, egui::Id::new(("bulk_scope", i)), Sense::click());
                if i > 0 { ui.painter().line_segment([cell.left_top(), cell.left_bottom()], Stroke::new(1.0_f32, crate::ui_kit::EDGE())); }
                let on = sc == Some(self.scope);
                let dim = sc.is_none();
                let dot = egui::pos2(cell.min.x + 1.3 * r, cell.center().y);
                ui.painter().circle_stroke(dot, 0.42 * r, Stroke::new(1.5_f32, if on { accent } else { MUTED() }));
                if on { ui.painter().circle_filled(dot, 0.2 * r, accent); }
                let ink = if on { accent } else if dim { MUTED() } else { TEXT() };
                let ty = if sub.is_empty() { cell.center().y } else { cell.center().y - 0.45 * r };
                ui.painter().text(egui::pos2(cell.min.x + 2.4 * r, ty), egui::Align2::LEFT_CENTER, &title, crate::theme::bold(0.8 * r), ink);
                if !sub.is_empty() { ui.painter().text(egui::pos2(cell.min.x + 2.4 * r, cell.center().y + 0.6 * r), egui::Align2::LEFT_CENTER, &sub, egui::FontId::proportional(0.65 * r), MUTED()); }
                if let (Some(s), true) = (sc, resp.clicked()) { self.scope = s; }
                if sc.is_none() && resp.clicked() { pick_games = true; }
                if resp.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
            }
            if pick_games { self.want_pick = true; }
            if self.tab == 1 || self.tab == 2 {
                ui.add_space(0.6 * r);
                let _n = self.count();
                let busy = self.job.is_some();
                // a section box (the web's dark panel with a small bold title)
                fn panel(ui: &mut egui::Ui, title: &str, r: f32, body: impl FnOnce(&mut egui::Ui)) {
                    egui::Frame::new().fill(Color32::from_rgb(0x0e, 0x19, 0x23)).stroke(Stroke::new(1.0_f32, crate::ui_kit::EDGE())).corner_radius(6).inner_margin(egui::Margin::symmetric((1.0 * r) as i8, (0.85 * r) as i8)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new(title).size(0.82 * r).strong().color(Color32::from_rgb(0xc2, 0xc8, 0xcc)));
                        ui.add_space(0.3 * r);
                        body(ui);
                    });
                    ui.add_space(0.7 * r);
                }
                // the accent button across the section
                let wide = |ui: &mut egui::Ui, text: &str, enabled: bool| -> bool {
                    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 2.4 * r), Sense::click());
                    let fill = if !enabled { accent.gamma_multiply(0.35) } else if resp.hovered() { accent.gamma_multiply(1.12) } else { accent };
                    ui.painter().rect_filled(rect, 5, fill);
                    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, text, crate::theme::bold(0.85 * r), Color32::from_rgb(0x0e, 0x16, 0x21));
                    if enabled && resp.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
                    enabled && resp.clicked()
                };
                let note = |ui: &mut egui::Ui, t: &str| { ui.label(RichText::new(t).size(0.75 * r).color(MUTED())); };
                egui::ScrollArea::vertical().max_height(f32::INFINITY).auto_shrink([false, true]).show(ui, |ui| {
                    if self.tab == 2 {
                        panel(ui, "DATES", r, |ui| {
                            note(ui, "Reads when you got each game and sets Date Added, replacing the date Zest first saw it. Games from GOG, Epic Games, EA App and IndieGala ask the store's account in the background (the store must be connected). Steam games read their activation date from Steam's Help site in your browser (needs the Zest Companion userscript and a Steam sign-in there).");
                            ui.add_space(0.3 * r);
                            if wide(ui, "Fetch Dates", !busy) { out = BulkOut::FetchDates { scope: self.scope }; }
                        });
                    } else {
                    panel(ui, "STORE DATA", r, |ui| {
                        ui.horizontal(|ui| { ui.checkbox(&mut self.do_meta, "Store info & reviews"); ui.add_space(1.0 * r); ui.checkbox(&mut self.do_cheevo, "Achievements"); });
                        ui.add_space(0.3 * r);
                        if wide(ui, "Re-scrape Store Data", !busy && (self.do_meta || self.do_cheevo)) { out = BulkOut::Rescrape { scope: self.scope, meta: self.do_meta, cheevo: self.do_cheevo }; }
                    });
                    panel(ui, "ARTWORK", r, |ui| {
                        note(ui, "Replaces each game's existing artwork, including covers you set yourself.");
                        ui.horizontal(|ui| { for (i, t) in ["Vertical", "Horizontal", "Icon"].iter().enumerate() { ui.checkbox(&mut self.art_types[i], *t); ui.add_space(0.8 * r); } });
                        ui.checkbox(&mut self.art_keep_custom, "Skip games whose cover I set myself (Custom source) in these types").on_hover_text("A cover you chose, cropped or loaded from a file is marked Custom; a rescrape would replace it. Off: those games are scraped too.");
                        ui.add_space(0.2 * r);
                        let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 3.0 * r), Sense::hover());
                        ui.painter().rect_stroke(rect, 5, Stroke::new(1.0_f32, crate::ui_kit::EDGE()), egui::StrokeKind::Inside);
                        let cw = rect.width() / 3.0;
                        for (i, (t, sub)) in [("Auto", "Steam + SGDB fallback"), ("Steam only", "No SGDB; other libraries use their Steam copy"), ("SGDB only", "Requires API key")].iter().enumerate() {
                            let cell = egui::Rect::from_min_size(rect.min + Vec2::new(cw * i as f32, 0.0), Vec2::new(cw, rect.height()));
                            let resp = ui.interact(cell, egui::Id::new(("brs_src", i)), Sense::click());
                            if i > 0 { ui.painter().line_segment([cell.left_top(), cell.left_bottom()], Stroke::new(1.0_f32, crate::ui_kit::EDGE())); }
                            let on = self.art_source == i;
                            let dot = egui::pos2(cell.min.x + 0.9 * r, cell.center().y);
                            ui.painter().circle_stroke(dot, 0.35 * r, Stroke::new(1.5_f32, if on { accent } else { MUTED() }));
                            if on { ui.painter().circle_filled(dot, 0.17 * r, accent); }
                            ui.painter().text(egui::pos2(cell.min.x + 1.7 * r, cell.center().y - 0.5 * r), egui::Align2::LEFT_CENTER, *t, crate::theme::bold(0.78 * r), Color32::WHITE);
                            let g = ui.painter().layout(sub.to_string(), egui::FontId::proportional(0.62 * r), MUTED(), cw - 2.0 * r);
                            ui.painter().galley(egui::pos2(cell.min.x + 1.7 * r, cell.center().y - 0.05 * r), g, MUTED());
                            if resp.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
                            if resp.clicked() { self.art_source = i; }
                        }
                        ui.add_space(0.4 * r);
                        if wide(ui, "Scrape Artwork", !busy && self.art_types.iter().any(|t| *t)) { out = BulkOut::Art { scope: self.scope, types: self.art_types, source: ["auto", "steam", "sgdb"][self.art_source], keep_custom: self.art_keep_custom }; }
                    });
                    panel(ui, "HLTB", r, |ui| {
                        note(ui, "Fetches completion times from HowLongToBeat by game name.");
                        ui.add_space(0.3 * r);
                        if wide(ui, "Fetch HLTB Data", !busy) { out = BulkOut::Hltb { scope: self.scope }; }
                    });
                    panel(ui, "STEAM METADATA", r, |ui| {
                        note(ui, "Fills tags, review score, categories, genres, developer / publisher and release date from Steam (or PCGamingWiki for anything Steam is missing). Respects the scope selector; only games whose developer, genres or tags are still blank, or that other libraries haven't matched to a Steam game yet, are processed.");
                        ui.add_space(0.3 * r);
                        if wide(ui, "Fill Missing Data", !busy) { out = BulkOut::Backfill { scope: self.scope }; }
                    });
                    if !cfg!(windows) {
                        panel(ui, "PROTONDB", r, |ui| {
                            note(ui, "Fetches Linux compatibility ratings from ProtonDB.");
                            ui.add_space(0.3 * r);
                            if wide(ui, "Fetch ProtonDB Data", !busy) { out = BulkOut::ProtonDb { scope: self.scope }; }
                        });
                    }
                    }
                });
                if let Some((label, total, done, wait)) = &self.job {
                    let frac = if *total > 0 { (*done as f32 / *total as f32).clamp(0.0, 1.0) } else { 0.0 };
                    let (bar, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 0.6 * r), Sense::hover());
                    ui.painter().rect_filled(bar, 3, Color32::from_rgb(0x0e, 0x19, 0x23));
                    ui.painter().rect_filled(egui::Rect::from_min_size(bar.min, Vec2::new(bar.width() * frac, bar.height())), 3, accent);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| { ui.label(RichText::new(format!("{label}: {done} / {total}")).size(0.78 * r).color(MUTED())); });
                    if let Some(w) = wait { ui.label(RichText::new(w).size(0.8 * r).color(WARN)); }
                }
                ui.add_space(0.4 * r);
                ui.horizontal(|ui| {
                    if self.job.is_some() {
                        let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width() * 0.62, 2.6 * r), Sense::click());
                        let red = crate::ui_kit::text_danger();
                        ui.painter().rect_filled(rect, 6, if resp.hovered() { red.gamma_multiply(1.15) } else { red });
                        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "Stop", crate::theme::bold(0.85 * r), Color32::WHITE);
                        if resp.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
                        if resp.clicked() { out = BulkOut::Stop; }
                    }
                    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 2.6 * r), Sense::click());
                    ui.painter().rect_stroke(rect, 6, Stroke::new(1.0_f32, crate::ui_kit::EDGE()), egui::StrokeKind::Inside);
                    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "CLOSE", crate::theme::bold(0.8 * r), if resp.hovered() { Color32::WHITE } else { TEXT() });
                    if resp.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
                    if resp.clicked() { out = BulkOut::Close; }
                });
                return;
            }
            ui.add_space(0.3 * r);
            ui.label(ui_kit::muted("Column to Edit", 0.8 * r));
            let names: Vec<String> = COLUMNS.iter().map(|c| label(c.0)).collect();
            let name_refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
            if let Some(p) = EditDialog::dropdown(ui, "bulk_col", &names[self.column], &name_refs, w, r) {
                if let Some(i) = names.iter().position(|c| *c == p) { self.column = i; }
                if !self.is_list() { self.mode = "replace"; }
                self.error = None;
            }
            ui.label(ui_kit::muted("Edit Mode", 0.8 * r));
            let cur = match self.mode { "append" => "Append \u{2014} add to the existing values", "remove" => "Remove \u{2014} take out matching values", _ => "Replace \u{2014} overwrite the entire column value" };
            let opts: &[&str] = if self.is_list() { &["Replace \u{2014} overwrite the entire column value", "Append \u{2014} add to the existing values", "Remove \u{2014} take out matching values"] } else { &["Replace \u{2014} overwrite the entire column value"] };
            if let Some(p) = EditDialog::dropdown(ui, "bulk_mode", cur, opts, w, r) { self.mode = if p.starts_with("Append") { "append" } else if p.starts_with("Remove") { "remove" } else { "replace" }; }
            ui.label(ui_kit::muted(match self.mode { "append" => "Adds the values to every matching game, skipping ones it already has.", "remove" => "Removes the values from every matching game.", _ => "Overwrites the entire value for every matching game." }, 0.7 * r));
            ui.label(ui_kit::muted("Value", 0.8 * r));
            if self.col() == "completion_status" {
                let cur = self.status.clone();
                if let Some(p) = EditDialog::dropdown(ui, "bulk_status", &cur, &STATUSES, w, r) { self.status = p; }
            } else if self.is_list() {
                EditDialog::pills(ui, "bulk_pills", &mut self.items, &mut self.pill_in, w, 3.0 * r, r);
                let idx = match self.col() { "tags" => 0, "groups" => 1, "genres" => 2, _ => 3 };
                let q = self.pill_in.trim().to_lowercase();
                if !q.is_empty() {
                    let hits: Vec<String> = self.suggestions[idx].iter().filter(|s| s.to_lowercase().contains(&q) && !self.items.contains(s)).take(6).cloned().collect();
                    ui.horizontal_wrapped(|ui| { for h in hits { if ui_kit::pill(ui, &h, false, accent, 0.8 * r).clicked() { self.items.push(h); self.pill_in.clear(); } } });
                }
            } else {
                ui.add(egui::TextEdit::singleline(&mut self.value).desired_width(w).font(egui::FontId::proportional(0.95 * r)).margin(egui::Margin::symmetric((0.6 * r) as i8, (0.5 * r) as i8)));
            }
            let hint = if !self.hint().is_empty() { self.hint() } else if self.mode == "replace" { "Leave the value empty to clear this field for all matching games." } else { "" };
            if !hint.is_empty() { ui.label(ui_kit::muted(hint, 0.7 * r)); }
            // warning
            let n = self.count();
            let col = label(self.col());
            let msg = match self.mode {
                "append" => format!("\u{26a0} {n} games will have values appended to {col}."),
                "remove" => format!("\u{26a0} {n} games will have values removed from {col}."),
                _ => format!("\u{26a0} {n} games will have their {col} completely overwritten. This cannot be undone."),
            };
            egui::Frame::new().fill(WARN_FILL).stroke(Stroke::new(1.0_f32, WARN.gamma_multiply(0.8))).corner_radius(3).inner_margin(egui::Margin::symmetric((0.8 * r) as i8, (0.45 * r) as i8)).show(ui, |ui| {
                ui.set_width(w - 1.6 * r - 2.0);
                ui.label(RichText::new(msg).size(0.7 * r).color(WARN));
            });
            if let Some(e) = &self.error { ui.label(RichText::new(e.clone()).size(0.85 * r).color(crate::ui_kit::text_danger())); }
            ui.add_space(0.8 * r);
            // Apply (two thirds) and Close (one third), full width
            let gap = 0.5 * r;
            let bw = (w - gap) / 3.0;
            let h = 3.33 * r;
            let (row, _) = ui.allocate_exact_size(Vec2::new(w, h), Sense::hover());
            let apply = egui::Rect::from_min_size(row.min, Vec2::new(bw * 2.0, h));
            let close = egui::Rect::from_min_size(row.min + Vec2::new(bw * 2.0 + gap, 0.0), Vec2::new(bw, h));
            let ar = ui.interact(apply, egui::Id::new("bulk_apply"), Sense::click());
            let olive = Color32::from_rgb(0x5a, 0x7d, 0x14);
            ui.painter().rect_filled(apply, 4, if ar.hovered() { olive.gamma_multiply(1.15) } else { olive });
            ui.painter().text(apply.center(), egui::Align2::CENTER_CENTER, "Apply", crate::theme::bold(0.85 * r), Color32::WHITE);
            if ar.clicked() { out = BulkOut::Apply { scope: self.scope, column: self.col().to_string(), mode: self.mode.to_string(), value: self.value_text() }; }
            let cr = ui.interact(close, egui::Id::new("bulk_close"), Sense::click());
            ui.painter().rect_stroke(close, 4, Stroke::new(1.0_f32, crate::ui_kit::EDGE()), egui::StrokeKind::Inside);
            ui.painter().text(close.center(), egui::Align2::CENTER_CENTER, "CLOSE", crate::theme::bold(0.8 * r), if cr.hovered() { Color32::WHITE } else { TEXT() });
            if cr.clicked() { out = BulkOut::Close; }
            for x in [&ar, &cr] { if x.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); } }
            ui.add_space(0.4 * r);
            let (rule, _) = ui.allocate_exact_size(Vec2::new(w, 1.0), Sense::hover());
            ui.painter().rect_filled(rule, 0.0, crate::ui_kit::EDGE());
            ui.add_space(0.4 * r);
            let (del, dr) = ui.allocate_exact_size(Vec2::new(w, 2.6 * r), Sense::click());
            let red = crate::ui_kit::text_danger();
            ui.painter().rect_stroke(del, 4, Stroke::new(1.0_f32, red), egui::StrokeKind::Inside);
            if dr.hovered() { ui.painter().rect_filled(del, 4, red.gamma_multiply(0.15)); ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
            ui.painter().text(del.center(), egui::Align2::CENTER_CENTER, "BULK DELETE THESE GAMES", crate::theme::bold(0.7 * r), red);
            if dr.clicked() { self.confirm_delete = true; }
        });
        if res.closed { return BulkOut::Close; }
        if self.want_pick { self.want_pick = false; return BulkOut::PickGames; }
        out
    }
}
