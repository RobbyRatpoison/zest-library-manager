//! Replays the art golden cases generated from the real Python (`gen_art_golden.py`).
use grid_core::art::*;
use serde_json::Value;

fn load() -> Value { serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/art_golden.json")).expect("run gen_art_golden.py first")).unwrap() }

/// Colours of two edge values: the largest per-channel difference, or None when their shape (kind letter, stop counts) differs.
fn colour_gap(a: &str, b: &str) -> Option<i32> {
    if a == "-" || b == "-" { return if a == b { Some(0) } else { None }; }
    let (pa, pb) = (parse_edges(a)?, parse_edges(b)?);
    if pa.0 != pb.0 { return None; }
    let mut gap = 0;
    for (x, y) in pa.1.iter().zip(pb.1.iter()).chain(pa.2.iter().zip(pb.2.iter())) { for k in 0..3 { gap = gap.max((x[k] as i32 - y[k] as i32).abs()); } }
    Some(gap)
}

#[test]
fn fit_rule_and_edge_colours_match_python() {
    let g = load();
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/art_data/");
    let (mut worst, mut n, mut bad) = (0, 0, Vec::new());
    for c in g["images"].as_array().unwrap() {
        let img = image::open(format!("{dir}{}", c["file"].as_str().unwrap())).unwrap().to_rgb8();
        let horizontal = c["kind"] == "horizontal";
        assert_eq!(fits(img.width(), img.height(), horizontal), c["fits"].as_bool().unwrap(), "fit rule {}x{} {}", img.width(), img.height(), c["kind"]);
        let mine = compute_edges(&img, horizontal);
        let py = c["edges"].as_str().map(String::from);
        n += 1;
        match (&mine, &py) {
            (None, None) => {}
            (Some(a), Some(b)) => match colour_gap(a, b) { Some(gap) => { worst = worst.max(gap); if gap > 3 { bad.push(format!("{} {}: gap {gap}\n rust   {a}\n python {b}", c["file"], c["kind"])); } } None => bad.push(format!("{} {}: shape differs\n rust   {a}\n python {b}", c["file"], c["kind"])) },
            _ => bad.push(format!("{} {}: rust {:?} python {:?}", c["file"], c["kind"], mine, py)),
        }
    }
    println!("{n} edge cases, largest per-channel colour difference {worst}");
    assert!(bad.is_empty(), "{} of {n} differ:\n{}", bad.len(), bad.iter().take(6).cloned().collect::<Vec<_>>().join("\n"));
}

#[test]
fn crop_boxes_match_python() {
    for c in load()["crops"].as_array().unwrap() {
        let ratio = match c["kind"].as_str().unwrap() { "vertical" => VERTICAL_RATIO, "horizontal" => HORIZONTAL_RATIO, _ => 1.0 };
        let (w, h) = (c["W"].as_u64().unwrap() as u32, c["H"].as_u64().unwrap() as u32);
        let (l, t, cw, ch) = crop_box(w, h, c["x"].as_f64().unwrap(), c["y"].as_f64().unwrap(), c["w"].as_f64().unwrap(), ratio);
        let want = (c["size"][0].as_u64().unwrap() as u32, c["size"][1].as_u64().unwrap() as u32);
        assert_eq!((cw, ch), want, "crop size for {c}");
        assert!(l + cw <= w && t + ch <= h, "box inside the image for {c}");
    }
}

#[test]
fn source_order_matches_python() {
    for c in load()["orders"].as_array().unwrap() {
        let strs = |v: &Value| v.as_array().map(|a| a.iter().map(|s| s.as_str().unwrap().to_string()).collect::<Vec<_>>());
        let (saved, default) = (strs(&c["saved"]), strs(&c["default"]));
        let got = source_order(c["kind"].as_str().unwrap(), c["has_store"].as_bool().unwrap(), saved.as_deref(), default.as_deref());
        assert_eq!(got, strs(&c["out"]), "case {c}");
    }
}

#[test]
fn a_cover_that_does_not_fit_is_contained_on_bars_and_one_that_fits_fills_the_card() {
    // wide cover (2:1) on a vertical card: bars above and below, the cover in the middle at 95%
    let wide = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(400, 200, image::Rgb([200, 40, 40])));
    let card = compose_card(&wide, 200, 300, false);
    assert_eq!(card.dimensions(), (200, 300));
    let mid = card.get_pixel(100, 150).0;
    assert!(mid[0] > 180 && mid[1] < 60, "the cover is in the middle: {mid:?}");
    let bar = card.get_pixel(100, 5).0;   // bars take the cover's edge colour, darkened to 70%
    assert!(bar[0] > 100 && bar[0] < 160 && bar[1] < 40, "bar colour {bar:?}");
    // a cover of the card's own shape fills it
    let fit = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(200, 300, image::Rgb([10, 200, 10])));
    let card = compose_card(&fit, 100, 150, false);
    assert_eq!(card.get_pixel(0, 0).0, [10, 200, 10]);
    assert_eq!(card.get_pixel(99, 149).0, [10, 200, 10]);
}

#[test]
fn saving_keeps_a_plain_jpeg_as_it_is_and_converts_other_formats() {
    let dir = std::env::temp_dir().join(format!("zest-art-{}", std::process::id()));
    let img = image::RgbImage::from_fn(64, 96, |x, y| image::Rgb([(x * 4) as u8, (y * 2) as u8, 90]));
    let mut jpg = Vec::new();
    image::DynamicImage::ImageRgb8(img.clone()).write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpg, 88)).unwrap();
    let p = dir.join("a/1.jpg");
    assert_eq!(save_cover(&jpg, &p, 95).unwrap(), (64, 96));
    assert_eq!(std::fs::read(&p).unwrap(), jpg, "a JPEG is not re-encoded");
    let mut png = Vec::new();
    image::DynamicImage::ImageRgb8(img).write_with_encoder(image::codecs::png::PngEncoder::new(&mut png)).unwrap();
    let q = dir.join("a/2.jpg");
    assert_eq!(save_cover(&png, &q, 95).unwrap(), (64, 96));
    assert!(std::fs::read(&q).unwrap().starts_with(&[0xFF, 0xD8]), "a PNG becomes a JPEG");
    assert!(save_cover(b"not an image", &dir.join("a/3.jpg"), 95).is_err());
    assert!(!dir.join("a/3.jpg").exists() && !dir.join("a/3.jpg.tmp").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Real covers (needs ZEST_REAL_ART = a json list of {path, kind, edges} produced by the Python on a library): run with `--ignored --nocapture`.
#[test]
#[ignore]
fn real_covers_against_python() {
    let Ok(path) = std::env::var("ZEST_REAL_ART") else { return };
    let list: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let (mut n, mut kinds, mut worst, mut hist, mut bad) = (0, 0, 0, [0usize; 6], Vec::new());
    for c in list.as_array().unwrap() {
        let Ok(img) = image::open(c["path"].as_str().unwrap()) else { continue };
        let mine = compute_edges(&img.to_rgb8(), c["kind"] == "horizontal");
        let py = c["edges"].as_str();
        n += 1;
        if let (Some(a), Some(b)) = (&mine, py) {
            if a.starts_with(&b[..1]) { kinds += 1; }
            match colour_gap(a, b) { Some(g) => { worst = worst.max(g); hist[(g as usize).min(5)] += 1; } None => bad.push(format!("{}: {a} vs {b}", c["path"])) }
        } else { bad.push(format!("{}: {mine:?} vs {py:?}", c["path"])); }
    }
    println!("{n} covers, same kind {kinds}, largest colour difference {worst}, histogram of the largest gap per cover (0,1,2,3,4,5+): {hist:?}");
    assert!(bad.is_empty(), "{} differ: {}", bad.len(), bad.iter().take(5).cloned().collect::<Vec<_>>().join("\n"));
}

/// Writes the card for one cover to a PNG so it can be looked at: ZEST_DUMP_COVER=<jpg> ZEST_DUMP_OUT=<png> [ZEST_DUMP_HORIZONTAL=1].
#[test]
#[ignore]
fn dump_one_card() {
    let (Ok(src), Ok(out)) = (std::env::var("ZEST_DUMP_COVER"), std::env::var("ZEST_DUMP_OUT")) else { return };
    let horizontal = std::env::var("ZEST_DUMP_HORIZONTAL").is_ok();
    let img = image::open(src).unwrap();
    let (w, h) = if horizontal { (616, 353) } else { (300, 450) };
    compose_card(&img, w, h, horizontal).save(out).unwrap();
}
