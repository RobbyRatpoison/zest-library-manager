//! The live controller picture in Settings > Gamepad > Controller test: PlayDate's own drawing (its Gamepad Setup's `gamepad_wizard.js`, the Xbox-style and PlayStation-style
//! bodies), redrawn with the painter in the same 400 x 244 box with the same outline, part positions, colors and rules: a control lights while pressed (a trigger above 0.15, any
//! other button above 0.5), a thumb moves 9 units per unit of axis, a stick's ring lights while it is clicked, and the face buttons carry the label style (PlayStation ones as shapes).
use crate::gamepad::{LabelStyle, Live, PadLabels};
use egui::{epaint::Mesh, Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, Ui, Vec2};

const BODY: &str = "M60,70 Q60,40 100,40 L300,40 Q340,40 340,70 L362,188 Q366,232 330,232 Q300,232 285,198 L262,172 L138,172 L115,198 Q100,232 70,232 Q34,232 38,188 Z";
const THUMB_TRAVEL: f32 = 9.0;

// PlayDate's colors (`.gpw-*` in modal_tools.html)
const BODY_FILL: Color32 = Color32::from_rgb(0x1f, 0x2a, 0x3a);
const EDGE: Color32 = Color32::from_rgb(0x56, 0x66, 0x7f);
const CTL: Color32 = Color32::from_rgb(0x2a, 0x35, 0x47);
const THUMB: Color32 = Color32::from_rgb(0x3a, 0x4a, 0x63);
const LABEL: Color32 = Color32::from_rgb(0xcf, 0xd8, 0xe3);
const DARK: Color32 = Color32::from_rgb(0x0e, 0x16, 0x21);

/// Which body: PlayStation pads get the PlayStation one (d-pad on the left, sticks side by side), everything else the Xbox one.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Body { Xbox, Ps }
pub fn body_for(style: LabelStyle) -> Body { if style == LabelStyle::Ps { Body::Ps } else { Body::Xbox } }

/// Where each part sits (standard button numbers: 0 A, 1 B, 2 X, 3 Y, 4 LB, 5 RB, 6 LT, 7 RT, 8 Back, 9 Start, 10 L3, 11 R3, 12 to 15 d-pad up/down/left/right, 16 Guide).
pub struct Layout { pub face: [(u8, f32, f32); 4], pub shoulders: [(u8, f32, f32); 2], pub triggers: [(u8, f32, f32); 2], pub sticks: [(u8, f32, f32, usize, usize); 2], pub dpad: (f32, f32), pub centers: [(u8, f32, f32); 3] }
pub fn layout(body: Body) -> Layout {
    let face = [(0, 288.0, 118.0), (1, 308.0, 98.0), (2, 268.0, 98.0), (3, 288.0, 78.0)];
    let (shoulders, triggers) = ([(4, 105.0, 28.0), (5, 295.0, 28.0)], [(6, 105.0, 11.0), (7, 295.0, 11.0)]);
    match body {
        Body::Xbox => Layout { face, shoulders, triggers, sticks: [(10, 112.0, 96.0, 0, 1), (11, 252.0, 152.0, 2, 3)], dpad: (148.0, 152.0), centers: [(8, 172.0, 100.0), (16, 200.0, 74.0), (9, 228.0, 100.0)] },
        Body::Ps => Layout { face, shoulders, triggers, sticks: [(10, 152.0, 158.0, 0, 1), (11, 248.0, 158.0, 2, 3)], dpad: (112.0, 100.0), centers: [(8, 168.0, 92.0), (16, 200.0, 176.0), (9, 232.0, 92.0)] },
    }
}

/// How far a thumb sits from its stick's centre for axis values (screen y grows down, the pad's up is negative in the web version and positive in gilrs).
pub fn thumb_offset(ax: f32, ay: f32) -> (f32, f32) { (ax.clamp(-1.0, 1.0) * THUMB_TRAVEL, -ay.clamp(-1.0, 1.0) * THUMB_TRAVEL) }
/// Is control `i` lit: a trigger from 0.15 of its travel (or its button down), anything else when the button is down.
pub fn lit(i: u8, live: &Live) -> bool {
    let down = live.pressed & (1 << i) != 0;
    match i { 6 => down || live.axes[4] > 0.15, 7 => down || live.axes[5] > 0.15, _ => down }
}

/// The outline as points in the 400 x 244 box: absolute `M`, `L`, `Q` (flattened) and `Z`, with repeated points dropped.
pub fn flatten(path: &str) -> Vec<(f32, f32)> {
    let toks: Vec<&str> = path.split(|c: char| c == ' ' || c == ',').filter(|t| !t.is_empty()).collect();
    let mut pts: Vec<(f32, f32)> = vec![];
    let (mut i, mut cur) = (0usize, (0.0f32, 0.0f32));
    let num = |s: &str| s.parse::<f32>().unwrap_or(0.0);
    let mut cmd = ' ';
    while i < toks.len() {
        let t = toks[i];
        let first = t.chars().next().unwrap_or(' ');
        let (c, head) = if first.is_ascii_alphabetic() { (first, &t[1..]) } else { (cmd, t) };
        cmd = c; i += 1;
        // numbers may be glued to the command letter (`M60`)
        let take = |first: Option<&str>, i: &mut usize| -> f32 { if let Some(f) = first.filter(|f| !f.is_empty()) { return num(f); } let v = num(toks[*i]); *i += 1; v };
        match c {
            'M' | 'L' => { let x = take(Some(head), &mut i); let y = take(None, &mut i); cur = (x, y); pts.push(cur); cmd = 'L'; }
            'Q' => {
                let cx = take(Some(head), &mut i); let cy = take(None, &mut i); let x = take(None, &mut i); let y = take(None, &mut i);
                for s in 1..=8 { let t = s as f32 / 8.0; let u = 1.0 - t; pts.push((u * u * cur.0 + 2.0 * u * t * cx + t * t * x, u * u * cur.1 + 2.0 * u * t * cy + t * t * y)); }
                cur = (x, y);
            }
            _ => {}
        }
    }
    pts.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3);
    if pts.len() > 1 && (pts[0].0 - pts[pts.len() - 1].0).abs() < 1e-3 && (pts[0].1 - pts[pts.len() - 1].1).abs() < 1e-3 { pts.pop(); }
    pts
}
fn signed_area(p: &[(f32, f32)]) -> f32 { (0..p.len()).map(|i| { let (a, b) = (p[i], p[(i + 1) % p.len()]); a.0 * b.1 - b.0 * a.1 }).sum::<f32>() / 2.0 }
fn cross(a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> f32 { (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0) }
/// Ear clipping: triangles (as index triples) that fill a simple polygon, concave ones included.
pub fn triangulate(poly: &[(f32, f32)]) -> Vec<[usize; 3]> {
    let mut idx: Vec<usize> = (0..poly.len()).collect();
    if signed_area(poly) < 0.0 { idx.reverse(); }
    let mut tris = vec![];
    while idx.len() > 3 {
        let m = idx.len();
        let ear = (0..m).find(|&i| {
            let (a, b, c) = (idx[(i + m - 1) % m], idx[i], idx[(i + 1) % m]);
            if cross(poly[a], poly[b], poly[c]) <= 1e-6 { return false; }
            !idx.iter().any(|&p| p != a && p != b && p != c && { let q = poly[p]; cross(poly[a], poly[b], q) >= 0.0 && cross(poly[b], poly[c], q) >= 0.0 && cross(poly[c], poly[a], q) >= 0.0 })
        });
        let Some(i) = ear else { break };
        tris.push([idx[(i + m - 1) % m], idx[i], idx[(i + 1) % m]]);
        idx.remove(i);
    }
    if idx.len() == 3 { tris.push([idx[0], idx[1], idx[2]]); }
    tris
}

fn face_color(style: LabelStyle, i: u8) -> Option<Color32> {
    let c = |h: u32| Color32::from_rgb((h >> 16) as u8, (h >> 8) as u8, h as u8);
    match (style, i) {
        (LabelStyle::Xbox, 0) => Some(c(0x3bb143)), (LabelStyle::Xbox, 1) => Some(c(0xe0393e)), (LabelStyle::Xbox, 2) => Some(c(0x3a7bd5)), (LabelStyle::Xbox, 3) => Some(c(0xf4c20d)),
        (LabelStyle::Ps, 0) => Some(c(0x3a7bd5)), (LabelStyle::Ps, 1) => Some(c(0xe0393e)), (LabelStyle::Ps, 2) => Some(c(0xe05fa0)), (LabelStyle::Ps, 3) => Some(c(0x3bb143)),
        _ => None,
    }
}

/// Draws the picture (the caller makes the whole live display one stop for the pad).
pub fn draw(ui: &mut Ui, r: f32, live: &Live, style: LabelStyle, labels: &PadLabels, accent: Color32) -> egui::Response {
    let w = 22.0 * r; let h = w * 244.0 / 400.0;
    let (rect, response) = ui.allocate_exact_size(Vec2::new(w, h), Sense::hover());
    let k = w / 400.0;
    let at = |x: f32, y: f32| Pos2::new(rect.min.x + x * k, rect.min.y + y * k);
    let p = ui.painter();
    let body = body_for(style);
    let lay = layout(body);
    let is_lit = |i: u8| lit(i, live);
    let fill = |on: bool| if on { accent } else { CTL };
    let edge = |on: bool| if on { accent } else { EDGE };
    let sw = (1.5 * k).max(1.0);

    // body: triangulated fill, then the outline
    let pts = flatten(BODY);
    let mut mesh = Mesh::default();
    for q in &pts { mesh.colored_vertex(at(q.0, q.1), BODY_FILL); }
    for t in triangulate(&pts) { mesh.add_triangle(t[0] as u32, t[1] as u32, t[2] as u32); }
    p.add(Shape::mesh(mesh));
    p.add(Shape::closed_line(pts.iter().map(|q| at(q.0, q.1)).collect(), Stroke::new((2.0 * k).max(1.0), EDGE)));

    let rrect = |x: f32, y: f32, ww: f32, hh: f32, rad: f32, on: bool| { p.rect_filled(Rect::from_min_size(at(x, y), Vec2::new(ww * k, hh * k)), rad * k, fill(on)); p.rect_stroke(Rect::from_min_size(at(x, y), Vec2::new(ww * k, hh * k)), rad * k, Stroke::new(sw, edge(on)), egui::StrokeKind::Inside); };
    let circle = |x: f32, y: f32, rad: f32, fillc: Color32, edgec: Color32| { p.circle_filled(at(x, y), rad * k, fillc); p.circle_stroke(at(x, y), rad * k, Stroke::new(sw, edgec)); };
    let name = |i: u8| crate::gamepad::label_name(i, style, labels);

    // shoulders and triggers (a trigger shows its button name; a lit one is filled with the accent)
    for (i, x, y) in lay.shoulders { rrect(x - 39.0, y - 6.0, 78.0, 13.0, 4.0, is_lit(i)); p.text(at(x, y + 0.5), Align2::CENTER_CENTER, name(i), FontId::proportional(8.0 * k), if is_lit(i) { DARK } else { LABEL }); }
    for (i, x, y) in lay.triggers { rrect(x - 27.0, y - 7.0, 54.0, 14.0, 6.0, is_lit(i)); p.text(at(x, y), Align2::CENTER_CENTER, name(i), FontId::proportional(8.0 * k), if is_lit(i) { DARK } else { LABEL }); }
    // sticks: ring (lit while clicked), thumb following the axes
    for (i, x, y, ax, ay) in lay.sticks {
        circle(x, y, 22.0, fill(is_lit(i)), edge(is_lit(i)));
        let (dx, dy) = thumb_offset(live.axes[ax], live.axes[ay]);
        circle(x + dx, y + dy, 11.0, if is_lit(i) { DARK } else { THUMB }, edge(is_lit(i)));
    }
    // d-pad
    let (dx, dy) = lay.dpad;
    rrect(dx - 6.0, dy - 22.0, 12.0, 16.0, 3.0, is_lit(12)); rrect(dx - 6.0, dy + 6.0, 12.0, 16.0, 3.0, is_lit(13));
    rrect(dx - 22.0, dy - 6.0, 16.0, 12.0, 3.0, is_lit(14)); rrect(dx + 6.0, dy - 6.0, 16.0, 12.0, 3.0, is_lit(15));
    // centre buttons
    for (i, x, y) in lay.centers { circle(x, y, 6.0, fill(is_lit(i)), edge(is_lit(i))); }
    // face buttons: the style's letters or PlayStation shapes, in the brand color unless lit
    for (i, x, y) in lay.face {
        let on = is_lit(i);
        circle(x, y, 10.0, fill(on), edge(on));
        let col = if on { DARK } else { face_color(style, i).unwrap_or(LABEL) };
        let custom = labels.names.get(&i);
        if style == LabelStyle::Ps && custom.is_none() {
            let st = Stroke::new((1.9 * k).max(1.0), col);
            let c = at(x, y);
            match i {
                0 => { p.line_segment([at(x - 4.0, y - 4.0), at(x + 4.0, y + 4.0)], st); p.line_segment([at(x + 4.0, y - 4.0), at(x - 4.0, y + 4.0)], st); }
                1 => { p.circle_stroke(c, 5.0 * k, st); }
                2 => { p.rect_stroke(Rect::from_center_size(c, Vec2::splat(8.0 * k)), 0.0, st, egui::StrokeKind::Middle); }
                _ => { p.add(Shape::closed_line(vec![at(x, y - 5.0), at(x + 5.0, y + 3.5), at(x - 5.0, y + 3.5)], st)); }
            }
        } else if style != LabelStyle::Other || custom.is_some() {
            p.text(at(x, y + 0.5), Align2::CENTER_CENTER, name(i), FontId::proportional(9.5 * k), col);
        }
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    fn area(p: &[(f32, f32)], t: &[[usize; 3]]) -> f32 { t.iter().map(|t| cross(p[t[0]], p[t[1]], p[t[2]]).abs() / 2.0).sum() }
    #[test] fn the_outline_is_flattened_and_filled_without_gaps_or_overlap() {
        let pts = flatten(BODY);
        assert!(pts.len() > 30, "the quadratic curves are flattened into points, got {}", pts.len());
        assert_eq!(pts[0], (60.0, 70.0));
        let tris = triangulate(&pts);
        assert_eq!(tris.len(), pts.len() - 2, "a simple polygon of n points is n - 2 triangles");
        assert!((area(&pts, &tris) - signed_area(&pts).abs()).abs() < 0.5, "the triangles cover exactly the polygon (the body is concave)");
    }
    #[test] fn a_concave_polygon_is_filled_exactly() {
        let l = [(0.0, 0.0), (4.0, 0.0), (4.0, 1.0), (1.0, 1.0), (1.0, 4.0), (0.0, 4.0)];   // an L, area 7
        let t = triangulate(&l);
        assert_eq!(t.len(), 4); assert!((area(&l, &t) - 7.0).abs() < 1e-4);
        let rev: Vec<_> = l.iter().rev().cloned().collect();
        assert!((area(&rev, &triangulate(&rev)) - 7.0).abs() < 1e-4, "either winding");
    }
    #[test] fn the_thumb_moves_nine_units_and_up_is_up() {
        assert_eq!(thumb_offset(0.0, 0.0), (0.0, 0.0));
        assert_eq!(thumb_offset(1.0, 0.0), (9.0, 0.0));
        assert!(thumb_offset(0.0, 1.0).1 < 0.0, "up on the pad is up on the screen");
        assert_eq!(thumb_offset(5.0, 0.0).0, 9.0);
    }
    #[test] fn a_trigger_lights_from_a_slight_pull_and_a_button_when_down() {
        let mut l = Live::default();
        assert!(!lit(6, &l));
        l.axes[4] = 0.2; assert!(lit(6, &l) && !lit(7, &l), "the left trigger's travel lights it, not the right one");
        l.axes[4] = 0.1; assert!(!lit(6, &l));
        l.pressed = 1 << 7; assert!(lit(7, &l));
        l.pressed = 1 << 0; assert!(lit(0, &l) && !lit(1, &l));
    }
    #[test] fn a_playstation_pad_gets_the_playstation_body() {
        assert_eq!(body_for(LabelStyle::Ps), Body::Ps);
        assert_eq!(body_for(LabelStyle::Nintendo), Body::Xbox);
        let (x, p) = (layout(Body::Xbox), layout(Body::Ps));
        assert_eq!(x.dpad, (148.0, 152.0)); assert_eq!(p.dpad, (112.0, 100.0));
        assert_eq!(x.face, p.face, "the face diamond is the same on both");
    }
}
