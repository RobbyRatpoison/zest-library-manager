//! The art Crop dialog (the web's `#art-crop-modal`): a frame fixed at the card's shape with the picture moving under it. Drag to move, wheel or slider to zoom
//! about a point, arrow keys nudge. `zoom` 1 shows the whole picture's shorter side; the result is sent as the kept area's top-left corner and width, all as
//! fractions of the stored picture (the height follows from the card's ratio, `grid_core::art::crop_box`).
use crate::ui_kit;
use egui::{Color32, Pos2, Rect, Sense, Vec2};

pub struct CropDialog {
    pub appid: i64,
    pub kind: usize,   // 0 vertical, 1 horizontal, 2 icon
    tex: egui::TextureHandle,
    iw: f32, ih: f32,
    zoom: f32,
    x0: f32, y0: f32,   // top-left of the visible part, as fractions of the picture
}

pub enum CropOut { None, Cancel, Apply { x: f64, y: f64, w: f64 } }


impl CropDialog {
    /// None when the game has no picture of that type.
    pub fn open(ctx: &egui::Context, root: &std::path::Path, appid: i64, kind: usize) -> Option<CropDialog> {
        // the picture the Edit dialog shows (a staged one when there is one), so a second crop continues from the first
        let path = crate::artstore::current_file(root, crate::artstore::KINDS[kind], appid)?;
        let (iw, ih) = image::image_dimensions(&path).ok()?;
        let im = image::open(&path).ok()?.thumbnail(1600, 1600).to_rgba8();
        let tex = ctx.load_texture("crop-image", egui::ColorImage::from_rgba_unmultiplied([im.width() as usize, im.height() as usize], im.as_raw()), egui::TextureOptions::LINEAR);
        let mut d = CropDialog { appid, kind, tex, iw: iw as f32, ih: ih as f32, zoom: 1.0, x0: 0.0, y0: 0.0 };
        let (w, h) = d.visible_fracs(d.frame_for(20.0));
        d.x0 = (1.0 - w) / 2.0; d.y0 = (1.0 - h) / 2.0;   // centred
        Some(d)
    }

    fn frame_for(&self, r: f32) -> Vec2 {
        match self.kind { 0 => Vec2::new(26.0 * r * 2.0 / 3.0, 26.0 * r), 1 => Vec2::new(40.0 * r, 40.0 * r * 353.0 / 616.0), _ => Vec2::splat(18.0 * r) }
    }
    /// scale (screen px per picture px) at which the picture just covers a frame of this size
    fn base(&self, f: Vec2) -> f32 { (f.x / self.iw).max(f.y / self.ih) }
    /// the visible part of the picture as fractions (width, height)
    fn visible_fracs(&self, f: Vec2) -> (f32, f32) { let s = self.base(f) * self.zoom; ((f.x / (self.iw * s)).min(1.0), (f.y / (self.ih * s)).min(1.0)) }
    fn clamp(&mut self, f: Vec2) { let (w, h) = self.visible_fracs(f); self.x0 = self.x0.clamp(0.0, 1.0 - w); self.y0 = self.y0.clamp(0.0, 1.0 - h); }
    /// zoom to `z` keeping the picture point under `p` (frame coordinates) where it is
    fn zoom_about(&mut self, f: Vec2, z: f32, p: Vec2) {
        let s = self.base(f) * self.zoom;
        let (fx, fy) = (self.x0 + p.x / (self.iw * s), self.y0 + p.y / (self.ih * s));
        self.zoom = z.clamp(1.0, 4.0);
        let s2 = self.base(f) * self.zoom;
        self.x0 = fx - p.x / (self.iw * s2); self.y0 = fy - p.y / (self.ih * s2);
        self.clamp(f);
    }

    pub fn show(&mut self, ctx: &egui::Context) -> CropOut {
        let r = ui_kit::rem(ctx);
        let accent = ui_kit::accent();
        let f = self.frame_for(r);
        let mut out = CropOut::None;
        let title = ["Crop Vertical Art", "Crop Horizontal Art", "Crop Icon"][self.kind];
        let res = ui_kit::modal(ctx, "art_crop", title, (f.x / r + 3.0).max(30.0), |ui, r| {
            ui.vertical_centered(|ui| {
                ui.label(ui_kit::muted("Drag the picture to move it, scroll or use the slider to zoom. The frame is the shape of the card.", 0.8 * r));
                ui.add_space(0.4 * r);
                let (frame, resp) = ui.allocate_exact_size(f, Sense::click_and_drag());
                let s = self.base(f) * self.zoom;
                if resp.dragged() { let d = resp.drag_delta(); self.x0 -= d.x / (self.iw * s); self.y0 -= d.y / (self.ih * s); self.clamp(f); }
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                    let wheel = ui.input(|i| i.smooth_scroll_delta.y);
                    if wheel != 0.0 { if let Some(p) = resp.hover_pos() { self.zoom_about(f, self.zoom * 1.0015f32.powf(wheel), p - frame.min); } }
                }
                // arrow keys nudge by 2% of the visible part
                let (w, h) = self.visible_fracs(f);
                let (dx, dy) = ui.input(|i| ((i.key_pressed(egui::Key::ArrowRight) as i32 - i.key_pressed(egui::Key::ArrowLeft) as i32) as f32, (i.key_pressed(egui::Key::ArrowDown) as i32 - i.key_pressed(egui::Key::ArrowUp) as i32) as f32));
                if dx != 0.0 || dy != 0.0 { self.x0 += dx * 0.02 * w; self.y0 += dy * 0.02 * h; self.clamp(f); }
                let s = self.base(f) * self.zoom;
                let img = Rect::from_min_size(frame.min - Vec2::new(self.x0 * self.iw * s, self.y0 * self.ih * s), Vec2::new(self.iw * s, self.ih * s));
                let p = ui.painter().with_clip_rect(frame);
                p.rect_filled(frame, 0, Color32::BLACK);
                p.image(self.tex.id(), img, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
                ui.painter().rect_stroke(frame, 0, egui::Stroke::new(2.0_f32, accent), egui::StrokeKind::Outside);
                ui.add_space(0.6 * r);
                ui.horizontal(|ui| {
                    ui.add_space(((ui.available_width() - 28.0 * r) / 2.0).max(0.0));
                    ui.label(ui_kit::muted("Zoom", 0.8 * r));
                    let mut z = self.zoom;
                    if ui_kit::slider(ui, &mut z, 1.0, 4.0, 0.01, 20.0 * r, accent).changed() { self.zoom_about(f, z, f / 2.0); }
                });
                let (w, h) = self.visible_fracs(f);
                ui.label(ui_kit::muted(format!("Keeps {} x {} px", (w * self.iw).round() as i64, (h * self.ih).round() as i64), 0.75 * r));
                ui.add_space(0.6 * r);
                ui.horizontal(|ui| {
                    ui.add_space(((ui.available_width() - 14.0 * r) / 2.0).max(0.0));
                    if ui_kit::button(ui, "APPLY", ui_kit::Kind::Plain, accent, r).clicked() { out = CropOut::Apply { x: self.x0 as f64, y: self.y0 as f64, w: w as f64 }; }
                    if ui_kit::button(ui, "CANCEL", ui_kit::Kind::Plain, accent, r).clicked() { out = CropOut::Cancel; }
                });
            });
        });
        if res.closed { return CropOut::Cancel; }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn dlg(iw: f32, ih: f32, kind: usize) -> CropDialog {
        let ctx = egui::Context::default();
        let tex = ctx.load_texture("t", egui::ColorImage::new([1, 1], vec![Color32::WHITE]), egui::TextureOptions::LINEAR);
        CropDialog { appid: 1, kind, tex, iw, ih, zoom: 1.0, x0: 0.0, y0: 0.0 }
    }
    #[test] fn zoom_one_shows_the_whole_short_side_and_stays_inside() {
        let mut d = dlg(600.0, 900.0, 0);            // exactly the card's shape: everything is visible
        let f = d.frame_for(10.0);
        let (w, h) = d.visible_fracs(f); assert!((w - 1.0).abs() < 1e-4 && (h - 1.0).abs() < 1e-4);
        let mut wide = dlg(1000.0, 500.0, 0);         // a landscape picture in a portrait frame: full height visible, part of the width
        let f = wide.frame_for(10.0);
        let (w, h) = wide.visible_fracs(f); assert!((h - 1.0).abs() < 1e-4 && w < 0.5);
        wide.x0 = 5.0; wide.clamp(f); assert!((wide.x0 - (1.0 - w)).abs() < 1e-5);
        d.zoom_about(f, 9.0, Vec2::ZERO); assert_eq!(d.zoom, 4.0);
    }
    #[test] fn zoom_keeps_the_point_under_the_pointer() {
        let mut d = dlg(2000.0, 3000.0, 0); let f = d.frame_for(10.0);
        d.x0 = 0.1; d.y0 = 0.2; d.clamp(f);
        let p = Vec2::new(f.x * 0.3, f.y * 0.6);
        let before = { let s = d.base(f) * d.zoom; (d.x0 + p.x / (d.iw * s), d.y0 + p.y / (d.ih * s)) };
        d.zoom_about(f, 2.5, p);
        let after = { let s = d.base(f) * d.zoom; (d.x0 + p.x / (d.iw * s), d.y0 + p.y / (d.ih * s)) };
        assert!((before.0 - after.0).abs() < 1e-4 && (before.1 - after.1).abs() < 1e-4, "{before:?} {after:?}");
    }
}
