//! Cover art, the local part of `images.py`: the shape rule, edge colors for covers that do not fit their card, composing a card with the web's
//! edge-color bars, crop boxes, the per-platform source order and saving a cover. No network. Decisions in NATIVE_UI_PLAN.md section 13.
use image::{imageops::FilterType, DynamicImage, RgbImage};
use std::path::Path;

/// Width / height of a card (`ART_CARD_RATIO`).
pub const VERTICAL_RATIO: f64 = 2.0 / 3.0;
pub const HORIZONTAL_RATIO: f64 = 616.0 / 353.0;
/// A cover within this fraction of the card's shape fits (the web's 5% rule).
pub const FIT_TOLERANCE: f64 = 0.05;
const EDGE_STOPS: usize = 6;
const EDGE_DIM: f64 = 0.7;
const EDGE_SAMPLE: u32 = 240;

/// Python's `round()`: halves go to the even neighbour (190.5 -> 190), unlike `f64::round`.
fn round_even(v: f64) -> f64 { let f = v.floor(); let d = v - f; if (d - 0.5).abs() < 1e-9 { if f % 2.0 == 0.0 { f } else { f + 1.0 } } else { v.round() } }

pub fn card_ratio(horizontal: bool) -> f64 { if horizontal { HORIZONTAL_RATIO } else { VERTICAL_RATIO } }

/// `_art_fits` / the check in `compute_art_edges`.
pub fn fits(w: u32, h: u32, horizontal: bool) -> bool {
    let t = card_ratio(horizontal);
    w > 0 && h > 0 && ((w as f64 / h as f64 - t).abs() / t) < FIT_TOLERANCE
}

/// `compute_art_edges`: `-` when the cover fits, else `y` + top + `/` + bottom (wider than the card: bars above and below) or `x` + left + `/` + right
/// (taller: bars at the sides), each side 6 comma separated hex colors without `#`, darkened to 70%. None when the image is too small.
pub fn compute_edges(img: &RgbImage, horizontal: bool) -> Option<String> {
    let (w, h) = img.dimensions();
    if w < 2 || h < 2 { return None; }
    if fits(w, h, horizontal) { return Some("-".into()); }
    let target = card_ratio(horizontal);
    let small;
    let im: &RgbImage = if w.max(h) > EDGE_SAMPLE {
        let f = EDGE_SAMPLE as f64 / w.max(h) as f64;
        let (nw, nh) = ((round_even(w as f64 * f) as u32).max(2), (round_even(h as f64 * f) as u32).max(2));
        small = image::imageops::resize(img, nw, nh, FilterType::Triangle);
        &small
    } else { img };
    let (w, h) = im.dimensions();
    let wide = w as f64 / h as f64 > target;
    let band = ((if wide { h } else { w }) / 12).max(1);
    // one side: the band shrunk to one pixel per stop with a box filter (fractional coverage along the long axis, plain average across the band)
    let side = |x0: u32, y0: u32, x1: u32, y1: u32| -> String {
        let (bw, bh) = (x1 - x0, y1 - y0);
        let len = if wide { bw } else { bh } as f64;
        let across = if wide { bh } else { bw };
        let mut out = Vec::new();
        for i in 0..EDGE_STOPS {
            let (s0, s1) = (i as f64 * len / EDGE_STOPS as f64, (i + 1) as f64 * len / EDGE_STOPS as f64);
            let mut acc = [0f64; 3];
            let mut wsum = 0f64;
            let (first, last) = (s0.floor() as u32, (s1.ceil() as u32).min(len as u32));
            // fewer source pixels than stops (a tiny image): PIL's box filter takes the pixel under the stop's centre, it does not blend
            let (first, last) = if len < EDGE_STOPS as f64 { let c = (((i as f64 + 0.5) * len / EDGE_STOPS as f64).floor() as u32).min(len as u32 - 1); (c, c + 1) } else { (first, last) };
            for p in first..last {
                let wt = if len < EDGE_STOPS as f64 { 1.0 } else { (s1.min(p as f64 + 1.0) - s0.max(p as f64)).max(0.0) };
                for q in 0..across {
                    let (x, y) = if wide { (x0 + p, y0 + q) } else { (x0 + q, y0 + p) };
                    let px = im.get_pixel(x, y).0;
                    for c in 0..3 { acc[c] += px[c] as f64 * wt; }
                    wsum += wt;
                }
            }
            let rgb: Vec<u8> = (0..3).map(|c| { let v = (acc[c] / wsum).round().clamp(0.0, 255.0); (v * EDGE_DIM) as u8 }).collect();   // int(c * 0.7) on the rounded channel
            out.push(format!("{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]));
        }
        out.join(",")
    };
    Some(if wide { format!("y{}/{}", side(0, 0, w, band), side(0, h - band, w, h)) } else { format!("x{}/{}", side(0, 0, band, h), side(w - band, 0, w, h)) })
}

/// A parsed edge value: (the cover is wider than its card, first side, second side).
pub fn parse_edges(s: &str) -> Option<(bool, [[u8; 3]; EDGE_STOPS], [[u8; 3]; EDGE_STOPS])> {
    let (wide, rest) = match s.as_bytes().first()? { b'y' => (true, &s[1..]), b'x' => (false, &s[1..]), _ => return None };
    let (a, b) = rest.split_once('/')?;
    let side = |t: &str| -> Option<[[u8; 3]; EDGE_STOPS]> {
        let parts: Vec<&str> = t.split(',').collect();
        if parts.len() != EDGE_STOPS { return None; }
        let mut out = [[0u8; 3]; EDGE_STOPS];
        for (i, p) in parts.iter().enumerate() {
            if p.len() != 6 || !p.chars().all(|c| c.is_ascii_hexdigit()) { return None; }
            let v = u32::from_str_radix(p, 16).ok()?;
            out[i] = [(v >> 16) as u8, (v >> 8) as u8, v as u8];
        }
        Some(out)
    };
    Some((wide, side(a)?, side(b)?))
}

fn gradient(stops: &[[u8; 3]; EDGE_STOPS], u: f64) -> [f64; 3] {
    // CSS linear-gradient: evenly spaced stops, linear in between
    let pos = (u.clamp(0.0, 1.0)) * (EDGE_STOPS - 1) as f64;
    let i = (pos.floor() as usize).min(EDGE_STOPS - 2);
    let f = pos - i as f64;
    let mut c = [0f64; 3];
    for k in 0..3 { c[k] = stops[i][k] as f64 * (1.0 - f) + stops[i + 1][k] as f64 * f; }
    c
}

/// The card for a cover: a cover that fits its card fills it (cropped by the few percent it differs); one that does not is contained at 95% on bars painted
/// from its own edge colors, as the web's `.needs-edge` does (two stacked gradients, the first fading into the second across the card).
pub fn compose_card(img: &DynamicImage, w: u32, h: u32, horizontal: bool) -> RgbImage {
    let (iw, ih) = (img.width(), img.height());
    if fits(iw, ih, horizontal) || iw < 2 || ih < 2 { return img.resize_to_fill(w, h, FilterType::Triangle).to_rgb8(); }
    let rgb = img.to_rgb8();
    let Some((wide, first, second)) = compute_edges(&rgb, horizontal).and_then(|e| parse_edges(&e)) else { return img.resize_to_fill(w, h, FilterType::Triangle).to_rgb8() };
    let mut card = RgbImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let (xu, yu) = ((x as f64 + 0.5) / w as f64, (y as f64 + 0.5) / h as f64);
            // wide covers: stops run left to right, the top side fades into the bottom side downwards; tall covers: stops run downwards, left fades into right
            let (along, across) = if wide { (xu, yu) } else { (yu, xu) };
            let (over, under) = (gradient(&first, along), gradient(&second, along));
            let a = 1.0 - across;
            let px = [0, 1, 2].map(|k| (over[k] * a + under[k] * (1.0 - a)).round().clamp(0.0, 255.0) as u8);
            card.put_pixel(x, y, image::Rgb(px));
        }
    }
    let s = (w as f64 / iw as f64).min(h as f64 / ih as f64) * 0.95;
    let (cw, ch) = (((iw as f64 * s).round() as u32).max(1), ((ih as f64 * s).round() as u32).max(1));
    let small = image::imageops::resize(&rgb, cw, ch, FilterType::Triangle);
    image::imageops::overlay(&mut card, &small, ((w.saturating_sub(cw)) / 2) as i64, ((h.saturating_sub(ch)) / 2) as i64);
    card
}

/// `crop_to_ratio`: the box (left, top, width, height) a crop keeps. x, y are the top-left corner and w the width, all as fractions of the image; the
/// height follows from the ratio (`ratio` = width / height of the target: the card's, or 1.0 for an icon); the box is moved inside the image.
pub fn crop_box(width: u32, height: u32, x: f64, y: f64, w: f64, ratio: f64) -> (u32, u32, u32, u32) {
    let (fw, fh) = (width as f64, height as f64);
    let mut cw = (round_even(w * fw) as i64).clamp(1, width as i64) as f64;
    let mut ch = round_even(cw / ratio);
    if ch > fh { ch = fh; cw = round_even(fh * ratio).max(1.0); }
    let left = round_even(x * fw).max(0.0).min(fw - cw);
    let top = round_even(y * fh).max(0.0).min(fh - ch);
    (left as u32, top as u32, cw as u32, ch as u32)
}

pub const ART_SOURCES: [&str; 3] = ["store", "steam", "sgdb"];
pub const ART_DEFAULT_ORDER: [&str; 3] = ["steam", "store", "sgdb"];

/// `art_source_order`: the ordered sources to try for one art type on one platform, or None for the original chain (only icons, untouched).
/// `saved` = the user's list (None = never customised); a saved list is honoured exactly, minus unknown names and `store` where the platform has none.
pub fn source_order(kind: &str, has_store: bool, saved: Option<&[String]>, default: Option<&[String]>) -> Option<Vec<String>> {
    let list: Vec<String> = match saved {
        Some(s) => s.to_vec(),
        None if kind == "vertical" || kind == "horizontal" => ART_DEFAULT_ORDER.iter().map(|s| s.to_string()).collect(),
        None if !has_store => return None,
        None => default.map(|d| d.to_vec()).unwrap_or_else(|| ["store", "sgdb", "steam"].iter().map(|s| s.to_string()).collect()),
    };
    Some(list.into_iter().filter(|s| ART_SOURCES.contains(&s.as_str()) && (s != "store" || has_store)).collect())
}

/// Save a cover as a JPEG (atomic: temp file then rename). A JPEG that is already RGB or grey is written exactly as it came (no second lossy encode);
/// anything else is converted to RGB and encoded at `quality` (100 from Zest; PlayDate used 95). Returns the saved image's size.
pub fn save_cover(bytes: &[u8], path: &Path, quality: u8) -> std::io::Result<(u32, u32)> {
    let bad = |e: String| std::io::Error::new(std::io::ErrorKind::InvalidData, e);
    if let Some(dir) = path.parent() { std::fs::create_dir_all(dir)?; }
    let tmp = path.with_extension("jpg.tmp");
    let plain_jpeg = bytes.starts_with(&[0xFF, 0xD8]) && {
        let mut d = jpeg_decoder::Decoder::new(std::io::Cursor::new(bytes));
        d.read_info().is_ok() && matches!(d.info().map(|i| i.pixel_format), Some(jpeg_decoder::PixelFormat::RGB24) | Some(jpeg_decoder::PixelFormat::L8))
    };
    let dims;
    if plain_jpeg {
        let mut d = jpeg_decoder::Decoder::new(std::io::Cursor::new(bytes));
        d.read_info().map_err(|e| bad(e.to_string()))?;
        let i = d.info().unwrap();
        dims = (i.width as u32, i.height as u32);
        std::fs::write(&tmp, bytes)?;
    } else {
        let img = image::load_from_memory(bytes).map_err(|e| bad(e.to_string()))?;
        let rgb = DynamicImage::ImageRgb8(img.to_rgb8());
        dims = (rgb.width(), rgb.height());
        let mut buf = Vec::new();
        let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality);
        rgb.write_with_encoder(enc).map_err(|e| bad(e.to_string()))?;
        std::fs::write(&tmp, buf)?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(dims)
}

/// The luminance quantisation table of the JPEG standard (Annex K), in the zigzag order a DQT segment stores it in.
const STD_LUMA_ZIGZAG: [u32; 64] = [16, 11, 12, 14, 12, 10, 16, 14, 13, 14, 18, 17, 16, 19, 24, 40, 26, 24, 22, 22, 24, 49, 35, 37, 29, 40, 58, 51, 61, 60, 57, 51,
    56, 55, 64, 72, 92, 78, 64, 68, 87, 69, 55, 56, 80, 109, 81, 87, 95, 98, 103, 104, 103, 62, 77, 113, 121, 112, 100, 120, 92, 101, 103, 99];

/// The IJG quality (1 to 100) a JPEG was saved at, read from its first (luminance) quantisation table: the table libjpeg, Pillow and the `image` crate all
/// produce is the standard one scaled by the quality, so the quality whose table is closest is the answer. Only the start of the file is looked at.
/// `None` for anything that is not a baseline-style JPEG with an 8-bit table.
pub fn jpeg_quality(head: &[u8]) -> Option<u8> {
    if !head.starts_with(&[0xFF, 0xD8]) { return None; }
    let mut i = 2;
    while i + 4 <= head.len() {
        if head[i] != 0xFF { i += 1; continue; }
        let marker = head[i + 1];
        if marker == 0xFF { i += 1; continue; }   // fill byte
        if marker == 0xD8 || (0xD0..=0xD7).contains(&marker) || marker == 0x01 { i += 2; continue; }
        if marker == 0xD9 || marker == 0xDA { return None; }   // end of image / start of scan: no table came first
        let len = ((head[i + 2] as usize) << 8) | head[i + 3] as usize;
        if len < 2 { return None; }
        if marker == 0xDB {
            let seg = head.get(i + 4..(i + 2 + len).min(head.len()))?;
            let mut p = 0;
            while p < seg.len() {
                let (precision, id) = (seg[p] >> 4, seg[p] & 15);
                p += 1;
                if precision != 0 || p + 64 > seg.len() { return None; }
                if id == 0 {
                    let table: Vec<u32> = seg[p..p + 64].iter().map(|b| *b as u32).collect();
                    return Some((1u32..=100).min_by_key(|q| {
                        let scale = if *q < 50 { 5000 / q } else { 200 - 2 * q };
                        STD_LUMA_ZIGZAG.iter().zip(&table).map(|(s, t)| ((s * scale + 50) / 100).clamp(1, 255).abs_diff(*t)).sum::<u32>()
                    })? as u8);
                }
                p += 64;
            }
        }
        i += 2 + len;
    }
    None
}

#[cfg(test)]
mod jpeg_quality_tests {
    use super::*;

    fn encode(q: u8) -> Vec<u8> {
        let img = image::RgbImage::from_fn(64, 64, |x, y| image::Rgb([(x * 4) as u8, (y * 4) as u8, 90]));
        let mut b = Vec::new();
        image::DynamicImage::ImageRgb8(img).write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(&mut b, q)).unwrap();
        b
    }

    #[test]
    fn reads_back_the_quality_a_jpeg_was_saved_at() {
        for q in [30u8, 50, 75, 90, 95, 100] { assert_eq!(jpeg_quality(&encode(q)), Some(q), "quality {q}"); }
    }

    #[test]
    fn not_a_jpeg_has_no_quality() {
        assert_eq!(jpeg_quality(b"\x89PNG\r\n\x1a\n"), None);
        assert_eq!(jpeg_quality(&[]), None);
    }
}
