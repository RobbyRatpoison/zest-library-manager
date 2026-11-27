//! The app's look, read from the same files as the web UI: theme.json (CSS variables), the background image, and the font stack.
use std::path::Path;

pub type Rgb = (u8, u8, u8);
pub fn pack(c: Rgb) -> u32 { ((c.0 as u32) << 16) | ((c.1 as u32) << 8) | c.2 as u32 }
fn hex(s: &str) -> Option<Rgb> {
    let s = s.trim().trim_start_matches('#');
    if s.len() == 6 { let v = u32::from_str_radix(s, 16).ok()?; Some(((v >> 16) as u8, (v >> 8) as u8, v as u8)) } else { None }
}

/// Every `theme.json` variable PlayDate has (the same 18 colors and the background opacity), with the shipped defaults for anything missing.
#[derive(Clone)]
pub struct Theme { pub bg_surface: Rgb, pub color_danger: Rgb, pub bg_page: Rgb, pub bg_nav: Rgb, pub border: Rgb, pub accent: Rgb, pub text_secondary: Rgb, pub text_bright: Rgb, pub text_primary: Rgb, pub bg_image_opacity: f32,
    pub bg_raised: Rgb, pub bg_input: Rgb, pub bg_card: Rgb, pub text_heading: Rgb, pub text_input: Rgb, pub on_accent: Rgb, pub accent_positive: Rgb, pub text_danger: Rgb, pub color_warning: Rgb,
    /// Zest's own extras. `None` = automatic: derived from the base colors (so a theme that does not set them keeps the look it always had).
    pub bg_hover: Option<Rgb>, pub divider: Option<Rgb>, pub focus_ring: Option<Rgb>, pub bg_button: Option<Rgb>, pub selection: Rgb, pub backdrop: Rgb }
fn shift(a: Rgb, d: (i16, i16, i16)) -> Rgb { let f = |v: u8, d: i16| (v as i16 + d).clamp(0, 255) as u8; (f(a.0, d.0), f(a.1, d.1), f(a.2, d.2)) }
impl Theme {
    // The automatic extras: shades of the base colors, the amounts the app always used, so the default theme looks as before.
    pub fn hover(&self) -> Rgb { self.bg_hover.unwrap_or_else(|| shift(self.bg_surface, (9, 12, 15))) }
    pub fn divider_c(&self) -> Rgb { self.divider.unwrap_or_else(|| shift(self.border, (0, -8, -9))) }
    pub fn focus(&self) -> Rgb { self.focus_ring.unwrap_or(self.accent) }
    pub fn button(&self) -> Rgb { self.bg_button.unwrap_or(self.bg_surface) }
    /// What a variable shows as when it is automatic (the Theme Editor's swatch and hex box).
    pub fn effective(&self, key: &str) -> Option<Rgb> { match key { "--bg-hover" => Some(self.hover()), "--divider" => Some(self.divider_c()), "--focus-ring" => Some(self.focus()), "--bg-button" => Some(self.button()), _ => None } }
    pub fn load(root: &Path) -> Theme {
        let v = std::fs::read_to_string(root.join("theme.json")).ok().and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()).unwrap_or(serde_json::Value::Null);
        Theme::from_value(&v)
    }
    /// A theme from (variable, value) pairs: the Theme Editor's draft, shown live without being saved.
    pub fn from_pairs(vars: &[(String, String)]) -> Theme {
        Theme::from_value(&serde_json::Value::Object(vars.iter().map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone()))).collect()))
    }
    fn from_value(v: &serde_json::Value) -> Theme {
        let mut t = Theme { bg_surface: (0x1b, 0x28, 0x38), color_danger: (0xa3, 0x2a, 0x2a), bg_page: (0x0e, 0x14, 0x19), bg_nav: (0x17, 0x1a, 0x21), border: (0x2a, 0x47, 0x5e), accent: (0x66, 0xc0, 0xf4),
                            text_secondary: (0x8f, 0x98, 0xa0), text_bright: (0xff, 0xff, 0xff), text_primary: (0xc7, 0xd5, 0xe0), bg_image_opacity: 1.0,
                            bg_raised: (0x1a, 0x23, 0x32), bg_input: (0x10, 0x18, 0x22), bg_card: (0x10, 0x18, 0x22), text_heading: (0xe2, 0xea, 0xf0), text_input: (0xff, 0xff, 0xff), on_accent: (0x0e, 0x16, 0x21),
                            accent_positive: (0x5c, 0x7e, 0x10), text_danger: (0xff, 0x80, 0x80), color_warning: (0xc9, 0x7c, 0x00),
                            bg_hover: None, divider: None, focus_ring: None, bg_button: None, selection: (0x1f, 0x4e, 0x73), backdrop: (0, 0, 0) };
        let get = |k: &str| v.get(k).and_then(|x| x.as_str()).and_then(hex);
        if let Some(c) = get("--bg-page") { t.bg_page = c; }
        if let Some(c) = get("--bg-nav") { t.bg_nav = c; }
        if let Some(c) = get("--bg-surface") { t.bg_surface = c; }
        if let Some(c) = get("--color-danger") { t.color_danger = c; }
        if let Some(c) = get("--border") { t.border = c; }
        if let Some(c) = get("--accent") { t.accent = c; }
        if let Some(c) = get("--text-secondary") { t.text_secondary = c; }
        if let Some(c) = get("--text-bright") { t.text_bright = c; }
        if let Some(c) = get("--text-primary") { t.text_primary = c; }
        if let Some(c) = get("--bg-raised") { t.bg_raised = c; }
        if let Some(c) = get("--bg-input") { t.bg_input = c; }
        if let Some(c) = get("--bg-card") { t.bg_card = c; }
        if let Some(c) = get("--text-heading") { t.text_heading = c; }
        if let Some(c) = get("--text-input") { t.text_input = c; }
        if let Some(c) = get("--on-accent") { t.on_accent = c; }
        if let Some(c) = get("--accent-positive") { t.accent_positive = c; }
        if let Some(c) = get("--text-danger") { t.text_danger = c; }
        if let Some(c) = get("--color-warning") { t.color_warning = c; }
        t.bg_hover = get("--bg-hover"); t.divider = get("--divider"); t.focus_ring = get("--focus-ring"); t.bg_button = get("--bg-button");
        if let Some(c) = get("--selection") { t.selection = c; }
        if let Some(c) = get("--backdrop") { t.backdrop = c; }
        if let Some(o) = v.get("--bg-image-opacity").and_then(|x| x.as_str().and_then(|s| s.parse::<f32>().ok()).or_else(|| x.as_f64().map(|f| f as f32))) { t.bg_image_opacity = o.clamp(0.0, 1.0); }
        t
    }
}

// ───────────────────────── editing theme.json (the same file and shape as PlayDate) ─────────────────────────

/// PlayDate's `DEFAULT_THEME`: every variable the Theme Editor lists, with its shipped value. Zest paints with the ones `Theme` reads; the rest are kept so the file
/// stays whole for PlayDate.
pub const DEFAULTS: &[(&str, &str)] = &[
    ("--bg-page", "#0e1419"), ("--bg-surface", "#1b2838"), ("--bg-raised", "#1a2332"), ("--bg-input", "#101822"), ("--bg-card", "#101822"), ("--bg-nav", "#171a21"),
    ("--text-primary", "#c7d5e0"), ("--text-heading", "#e2eaf0"), ("--text-secondary", "#8f98a0"), ("--text-input", "#ffffff"), ("--text-bright", "#ffffff"),
    ("--accent", "#66c0f4"), ("--on-accent", "#0e1621"), ("--accent-positive", "#5c7e10"), ("--border", "#2a475e"),
    ("--color-danger", "#a32a2a"), ("--text-danger", "#ff8080"), ("--color-warning", "#c97c00"),
    // Zest's extras: an empty value means automatic (derived from the colors above)
    ("--bg-hover", ""), ("--divider", ""), ("--focus-ring", ""), ("--bg-button", ""), ("--selection", "#1f4e73"), ("--backdrop", "#000000"),
    ("--bg-image-opacity", "1"),
];

/// PlayDate's five built-in themes (`THEME_PRESETS` in modal_tools.js), variable for variable, then Zest's own.
pub const PRESETS: &[(&str, &[(&str, &str)])] = &[
    ("Steam Dark", &[
        ("--bg-page", "#0e1419"), ("--bg-surface", "#1b2838"), ("--bg-raised", "#1a2332"), ("--bg-input", "#101822"),
        ("--bg-card", "#101822"), ("--bg-nav", "#171a21"), ("--text-primary", "#c7d5e0"), ("--text-heading", "#e2eaf0"),
        ("--text-secondary", "#8f98a0"), ("--text-input", "#ffffff"), ("--text-bright", "#ffffff"), ("--accent", "#66c0f4"),
        ("--on-accent", "#0e1621"), ("--accent-positive", "#5c7e10"), ("--border", "#2a475e"), ("--color-danger", "#a32a2a"),
        ("--text-danger", "#ff8080"), ("--color-warning", "#c97c00"),
    ]),
    ("OLED Black", &[
        ("--bg-page", "#000000"), ("--bg-surface", "#0d0d0d"), ("--bg-raised", "#141414"), ("--bg-input", "#050505"),
        ("--bg-card", "#111111"), ("--bg-nav", "#000000"), ("--text-primary", "#d0d8e0"), ("--text-heading", "#eef2f5"),
        ("--text-secondary", "#7a8590"), ("--text-input", "#ffffff"), ("--text-bright", "#ffffff"), ("--accent", "#58b4f0"),
        ("--on-accent", "#000000"), ("--accent-positive", "#4a7a08"), ("--border", "#222222"), ("--color-danger", "#8a1a1a"),
        ("--text-danger", "#ff6060"), ("--color-warning", "#a06000"),
    ]),
    ("Slate", &[
        ("--bg-page", "#0d1117"), ("--bg-surface", "#161b22"), ("--bg-raised", "#1c2128"), ("--bg-input", "#0d1117"),
        ("--bg-card", "#21262d"), ("--bg-nav", "#0d1117"), ("--text-primary", "#c9d1d9"), ("--text-heading", "#f0f6fc"),
        ("--text-secondary", "#8b949e"), ("--text-input", "#f0f6fc"), ("--text-bright", "#ffffff"), ("--accent", "#58a6ff"),
        ("--on-accent", "#0d1117"), ("--accent-positive", "#3fb950"), ("--border", "#30363d"), ("--color-danger", "#8b1a1a"),
        ("--text-danger", "#ff7b72"), ("--color-warning", "#d29922"),
    ]),
    ("Warm", &[
        ("--bg-page", "#120c08"), ("--bg-surface", "#1e1510"), ("--bg-raised", "#261a12"), ("--bg-input", "#120c08"),
        ("--bg-card", "#2a1e16"), ("--bg-nav", "#0e0a06"), ("--text-primary", "#e0cfc0"), ("--text-heading", "#f0e5d8"),
        ("--text-secondary", "#9a8070"), ("--text-input", "#f5ede0"), ("--text-bright", "#ffffff"), ("--accent", "#e8925a"),
        ("--on-accent", "#120c08"), ("--accent-positive", "#6a9a20"), ("--border", "#3e2a1a"), ("--color-danger", "#a03030"),
        ("--text-danger", "#ff8060"), ("--color-warning", "#c08020"),
    ]),
    ("Midnight", &[
        ("--bg-page", "#080810"), ("--bg-surface", "#0f0f1e"), ("--bg-raised", "#131322"), ("--bg-input", "#080812"),
        ("--bg-card", "#16162a"), ("--bg-nav", "#06060e"), ("--text-primary", "#c8cce8"), ("--text-heading", "#e8eaf8"),
        ("--text-secondary", "#8888aa"), ("--text-input", "#f0f0ff"), ("--text-bright", "#ffffff"), ("--accent", "#8888ff"),
        ("--on-accent", "#080810"), ("--accent-positive", "#44aa44"), ("--border", "#2a2a50"), ("--color-danger", "#882222"),
        ("--text-danger", "#ff8888"), ("--color-warning", "#cc8800"),
    ]),
    // Zest's own (PlayDate does not ship it): the olive green of the original Steam client, with its gold highlight
    ("Steam Classic", &[
        ("--bg-page", "#2a3022"), ("--bg-surface", "#4c5844"), ("--bg-raised", "#3e4637"), ("--bg-input", "#2f3628"),
        ("--bg-card", "#394132"), ("--bg-nav", "#3e4637"), ("--text-primary", "#c8d1bf"), ("--text-heading", "#e8eddf"),
        ("--text-secondary", "#929d88"), ("--text-input", "#e8eddf"), ("--text-bright", "#ffffff"), ("--accent", "#c4b550"),
        ("--on-accent", "#282e22"), ("--accent-positive", "#5c7e10"), ("--border", "#282e22"), ("--color-danger", "#8a2a2a"),
        ("--text-danger", "#ff9a8a"), ("--color-warning", "#d9a441"),
    ]),
    // Zest's own, after Steam Classic: a light theme, a high-contrast one, a cool blue-gray, a violet and a red
    ("Daylight", &[
        ("--bg-page", "#eef1f5"), ("--bg-surface", "#ffffff"), ("--bg-raised", "#f7f9fb"), ("--bg-input", "#ffffff"),
        ("--bg-card", "#e3e8ee"), ("--bg-nav", "#ffffff"), ("--text-primary", "#1f2933"), ("--text-heading", "#0b1520"),
        ("--text-secondary", "#5b6876"), ("--text-input", "#0b1520"), ("--text-bright", "#ffffff"), ("--accent", "#1a73e8"),
        ("--on-accent", "#ffffff"), ("--accent-positive", "#2e7d32"), ("--border", "#c5cdd6"), ("--color-danger", "#c62828"),
        ("--text-danger", "#c62828"), ("--color-warning", "#b26a00"), ("--selection", "#b3d4fc"),
    ]),
    ("High Contrast", &[
        ("--bg-page", "#000000"), ("--bg-surface", "#0a0a0a"), ("--bg-raised", "#141414"), ("--bg-input", "#000000"),
        ("--bg-card", "#0a0a0a"), ("--bg-nav", "#000000"), ("--text-primary", "#ffffff"), ("--text-heading", "#ffffff"),
        ("--text-secondary", "#d0d0d0"), ("--text-input", "#ffffff"), ("--text-bright", "#ffffff"), ("--accent", "#ffe600"),
        ("--on-accent", "#000000"), ("--accent-positive", "#008a2e"), ("--border", "#ffffff"), ("--color-danger", "#d50000"),
        ("--text-danger", "#ff7777"), ("--color-warning", "#ffb300"), ("--selection", "#5a5000"),
    ]),
    ("Frost", &[
        ("--bg-page", "#2e3440"), ("--bg-surface", "#3b4252"), ("--bg-raised", "#434c5e"), ("--bg-input", "#2e3440"),
        ("--bg-card", "#3b4252"), ("--bg-nav", "#2e3440"), ("--text-primary", "#d8dee9"), ("--text-heading", "#eceff4"),
        ("--text-secondary", "#9aa5b8"), ("--text-input", "#eceff4"), ("--text-bright", "#ffffff"), ("--accent", "#88c0d0"),
        ("--on-accent", "#2e3440"), ("--accent-positive", "#5e8a54"), ("--border", "#4c566a"), ("--color-danger", "#bf616a"),
        ("--text-danger", "#e58a93"), ("--color-warning", "#ebcb8b"),
    ]),
    ("Grape", &[
        ("--bg-page", "#130b1f"), ("--bg-surface", "#1f1433"), ("--bg-raised", "#271a3f"), ("--bg-input", "#130b1f"),
        ("--bg-card", "#2a1c45"), ("--bg-nav", "#0f0818"), ("--text-primary", "#dccff0"), ("--text-heading", "#f2eafc"),
        ("--text-secondary", "#9b8bb8"), ("--text-input", "#f5eefe"), ("--text-bright", "#ffffff"), ("--accent", "#b57bff"),
        ("--on-accent", "#130b1f"), ("--accent-positive", "#4f9a3c"), ("--border", "#3d2a62"), ("--color-danger", "#9a2a5a"),
        ("--text-danger", "#ff8fb8"), ("--color-warning", "#e0a030"),
    ]),
    ("Crimson", &[
        ("--bg-page", "#120808"), ("--bg-surface", "#1e1010"), ("--bg-raised", "#281515"), ("--bg-input", "#120808"),
        ("--bg-card", "#2c1818"), ("--bg-nav", "#0d0505"), ("--text-primary", "#e8d5d5"), ("--text-heading", "#f5e6e6"),
        ("--text-secondary", "#a08080"), ("--text-input", "#f8eded"), ("--text-bright", "#ffffff"), ("--accent", "#e63950"),
        ("--on-accent", "#120808"), ("--accent-positive", "#5c8a2a"), ("--border", "#46222a"), ("--color-danger", "#b3202f"),
        ("--text-danger", "#ff8a8a"), ("--color-warning", "#e0902a"),
    ]),
];

/// A built-in theme's variables, completed with `current` for what it does not set (the background opacity).
pub fn preset_vars(name: &str, current: &[(String, String)]) -> Option<Vec<(String, String)>> {
    let (_, v) = PRESETS.iter().find(|(n, _)| *n == name)?;
    Some(DEFAULTS.iter().map(|(k, d)| (k.to_string(), v.iter().find(|(pk, _)| pk == k).map(|(_, pv)| pv.to_string())
        // the background opacity is not part of a theme; everything else a preset does not set goes back to its default (automatic), so one preset's extras never carry into the next
        .or_else(|| if *k == "--bg-image-opacity" { current.iter().find(|(ck, _)| ck == k).map(|(_, cv)| cv.clone()) } else { None }).unwrap_or_else(|| d.to_string()))).collect())
}

/// The colors a theme is shown with in the Choose Theme list: its surface, accent and body text.
pub fn swatch(vars: &[(String, String)]) -> Option<(Rgb, Rgb, Rgb)> {
    let g = |k: &str| vars.iter().find(|(kk, _)| kk == k).and_then(|(_, v)| hex(v));
    Some((g("--bg-surface")?, g("--accent")?, g("--text-primary")?))
}
pub fn preset_swatch(name: &str) -> Option<(Rgb, Rgb, Rgb)> {
    let (_, v) = PRESETS.iter().find(|(n, _)| *n == name)?;
    let g = |k: &str| v.iter().find(|(kk, _)| *kk == k).and_then(|(_, x)| hex(x));
    Some((g("--bg-surface")?, g("--accent")?, g("--text-primary")?))
}
fn read_json(root: &Path) -> serde_json::Map<String, serde_json::Value> {
    std::fs::read_to_string(root.join("theme.json")).ok().and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()).and_then(|v| v.as_object().cloned()).unwrap_or_default()
}
fn write_json(root: &Path, m: &serde_json::Map<String, serde_json::Value>) -> Result<(), String> {
    let tmp = root.join("theme.json.tmp");
    // serde_json indents by 2; PlayDate's json.dump(indent=4) by 4
    let pretty = serde_json::to_string_pretty(m).map_err(|e| e.to_string())?;
    let buf: String = pretty.lines().map(|l| { let n = l.len() - l.trim_start().len(); format!("{}{}\n", " ".repeat(n * 2), l.trim_start()) }).collect();
    std::fs::write(&tmp, buf).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, root.join("theme.json")).map_err(|e| e.to_string())
}
/// The active theme as (variable, value) pairs in `DEFAULTS` order: the file's value where it has one, else the default (`load_theme`).
pub fn read_vars(root: &Path) -> Vec<(String, String)> {
    let m = read_json(root);
    DEFAULTS.iter().map(|(k, d)| (k.to_string(), m.get(*k).and_then(|v| v.as_str().map(String::from).or_else(|| v.as_f64().map(|f| f.to_string()))).unwrap_or_else(|| d.to_string()))).collect()
}
/// A theme's variables kept only if they are known ones with a plausible value (`save_theme` keeps known keys only; a bad color would break parsing).
fn clean(vars: &[(String, String)]) -> serde_json::Map<String, serde_json::Value> {
    let mut m = serde_json::Map::new();
    for (k, v) in vars {
        if !DEFAULTS.iter().any(|(d, _)| d == k) { continue; }
        let ok = if k == "--bg-image-opacity" { v.trim().parse::<f32>().is_ok() } else { hex(v).is_some() };
        if ok { m.insert(k.clone(), serde_json::Value::String(v.trim().to_string())); }
    }
    m
}
/// `save_theme`: replaces the active variables, keeps the saved themes (`_saved`).
pub fn save_active(root: &Path, vars: &[(String, String)]) -> Result<(), String> {
    let old = read_json(root);
    let mut m = clean(vars);
    if let Some(sv) = old.get("_saved").filter(|v| v.as_object().map_or(false, |o| !o.is_empty())) { m.insert("_saved".into(), sv.clone()); }
    write_json(root, &m)
}
/// Names of the saved themes, in file order.
pub fn saved_names(root: &Path) -> Vec<String> { read_json(root).get("_saved").and_then(|v| v.as_object()).map(|o| o.keys().cloned().collect()).unwrap_or_default() }
/// A saved theme's variables, completed with the defaults.
pub fn saved_vars(root: &Path, name: &str) -> Option<Vec<(String, String)>> {
    let m = read_json(root); let t = m.get("_saved")?.get(name)?.as_object()?;
    Some(DEFAULTS.iter().map(|(k, d)| (k.to_string(), t.get(*k).and_then(|v| v.as_str()).unwrap_or(d).to_string())).collect())
}
/// `save_named_theme`: the active theme in the file stays as it is, `_saved` gains (or replaces) the entry.
pub fn save_named(root: &Path, name: &str, vars: &[(String, String)]) -> Result<(), String> {
    let name = name.trim(); if name.is_empty() { return Err("Give the theme a name.".into()); }
    let mut m = read_json(root);
    let mut saved = m.get("_saved").and_then(|v| v.as_object().cloned()).unwrap_or_default();
    saved.insert(name.to_string(), serde_json::Value::Object(clean(vars)));
    m.insert("_saved".into(), serde_json::Value::Object(saved));
    write_json(root, &m)
}
pub fn delete_named(root: &Path, name: &str) -> Result<(), String> {
    let mut m = read_json(root);
    if let Some(o) = m.get_mut("_saved").and_then(|v| v.as_object_mut()) { o.shift_remove(name); }
    write_json(root, &m)
}
/// `set_background_from_path`: any jpg, png or webp becomes `static/img/backgrounds/background.jpg` (JPEG, quality 92).
pub fn set_background(root: &Path, from: &Path) -> Result<(), String> {
    let ext = from.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    if !["jpg", "jpeg", "png", "webp"].contains(&ext.as_str()) { return Err("Choose a jpg, png or webp picture.".into()); }
    let img = image::open(from).map_err(|e| format!("Could not read that picture: {e}"))?.into_rgb8();
    let dir = root.join("static/img/backgrounds"); std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let f = std::fs::File::create(dir.join("background.jpg")).map_err(|e| e.to_string())?;
    image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::BufWriter::new(f), 92).encode_image(&img).map_err(|e| e.to_string())
}
pub fn reset_background(root: &Path) -> Result<(), String> {
    match std::fs::remove_file(root.join("static/img/backgrounds/background.jpg")) { Ok(()) => Ok(()), Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()), Err(e) => Err(e.to_string()) }
}

/// A JPEG as RGB, decoded with jpeg-decoder (the `image` crate's decoder needed ~84 MB of working memory for a 4K file); other formats and odd JPEGs go through `image`.
fn load_rgb(path: &Path) -> Option<image::RgbImage> {
    let fast = || -> Option<image::RgbImage> {
        let mut dec = jpeg_decoder::Decoder::new(std::io::BufReader::new(std::fs::File::open(path).ok()?));
        dec.scale(1920, 1080).ok()?;   // the decoder halves a 4K file while decoding, no separate resize
        let px = dec.decode().ok()?;
        let info = dec.info()?;
        let rgb = match info.pixel_format { jpeg_decoder::PixelFormat::RGB24 => px, jpeg_decoder::PixelFormat::L8 => px.iter().flat_map(|&v| [v, v, v]).collect(), _ => return None };
        image::RgbImage::from_raw(info.width as u32, info.height as u32, rgb)
    };
    fast().or_else(|| image::open(path).ok().map(|i| i.into_rgb8()))
}

/// The page background: `background.jpg` (else the shipped default) scaled "cover", centred, faded over the page color by the theme's opacity.
pub struct Background { src: Option<image::RgbImage>, raw: Option<(usize, usize, Vec<u32>)>, tmp: Vec<u32>, pub buf: Vec<u32>, pub size: (usize, usize), page: u32, opacity: f32, want: (usize, usize), want_since: std::time::Instant }
impl Background {
    pub fn load(root: &Path, t: &Theme) -> Background {
        let dir = root.join("static/img/backgrounds");
        // the user's own picture, else Zest's default built into the program (PlayDate's old default is not used: Zest has its own; drawn with PlayDate's
        // tools/gen_background.py, seed 458590, navy palette, density 0.99, 2560x1440, picked with ~/Projects/tools/BackgroundPicker)
        let src = load_rgb(&dir.join("background.jpg"))
            .or_else(|| image::load_from_memory(include_bytes!("../../assets/default_background.jpg")).ok().map(|i| i.to_rgb8())).map(|i| {
            // a decorative, faded image: a 4K source only costs RAM, so keep at most 2560 px on the long side
            let long = i.width().max(i.height());
            if long <= 2560 { return i; }
            let s = 2560.0 / long as f32;
            image::imageops::resize(&i, ((i.width() as f32 * s).round() as u32).max(1), ((i.height() as f32 * s).round() as u32).max(1), image::imageops::FilterType::Triangle)
        });
        let (page, opacity) = (pack(t.bg_page), t.bg_image_opacity);
        Background { src, raw: None, tmp: Vec::new(), buf: Vec::new(), size: (0, 0), page, opacity, want: (0, 0), want_since: std::time::Instant::now() }
    }
    /// The scaled picture without any fade, kept for the length of a drag: a step is then one cheap pass over it instead of scaling the source again.
    fn ensure_raw(&mut self, w: usize, h: usize) {
        let Some(src) = &self.src else { return };
        if self.raw.as_ref().map_or(false, |(rw, rh, _)| (*rw, *rh) == (w, h)) { return; }
        let (sw, sh) = (src.width() as f32, src.height() as f32);
        let scale = (w as f32 / sw).max(h as f32 / sh);
        let (cw, ch) = (((w as f32 / scale).round() as u32).min(src.width()), ((h as f32 / scale).round() as u32).min(src.height()));
        let (cx, cy) = ((src.width() - cw) / 2, (src.height() - ch) / 2);
        let cropped = image::imageops::crop_imm(src, cx, cy, cw, ch);
        let scaled = image::imageops::resize(&*cropped, w as u32, h as u32, image::imageops::FilterType::Triangle);
        self.raw = Some((w, h, scaled.pixels().map(|p| ((p[0] as u32) << 16) | ((p[1] as u32) << 8) | p[2] as u32).collect()));
    }
    /// Done when the Background dialog opens, so the first step of a drag does not pay for scaling the picture.
    pub fn prewarm(&mut self) { let (w, h) = if self.size != (0, 0) { self.size } else { self.want }; if w > 0 && h > 0 { self.ensure_raw(w, h); } }
    /// A new page color or opacity (the Appearance slider while it moves): the pre-faded copy is stale from now on, so the picture is painted straight from its source until the size and look
    /// have been quiet for a moment, then the copy is rebuilt (once, not on every frame of a drag).
    pub fn restyle(&mut self, page: u32, opacity: f32) {
        if page == self.page && (opacity - self.opacity).abs() < f32::EPSILON { return; }
        self.page = page; self.opacity = opacity;
        let (w, h) = if self.size != (0, 0) { self.size } else { self.want };
        if w == 0 || h == 0 { return; }
        let o = (opacity * 256.0) as u32;
        let (pr, pg, pb) = ((page >> 16) & 255, (page >> 8) & 255, page & 255);
        let fade = |p: u32| -> u32 { let (r, g, b) = (((p >> 16) & 255) * o + pr * (256 - o), ((p >> 8) & 255) * o + pg * (256 - o), (p & 255) * o + pb * (256 - o)); ((r >> 8) << 16) | ((g >> 8) << 8) | (b >> 8) };
        let Some(_src) = &self.src else { self.buf = vec![page; w * h]; self.size = (w, h); return };
        self.ensure_raw(w, h);
        let raw = &self.raw.as_ref().unwrap().2;
        self.buf.clear(); self.buf.extend(raw.iter().map(|&p| fade(p)));
        self.size = (w, h);
    }
    /// Rebuild for a new window size once the size has been stable for a moment (scaling the image is not free, so not on every frame of a drag).
    pub fn update(&mut self, w: usize, h: usize) -> bool {
        if (w, h) != self.want { self.want = (w, h); self.want_since = std::time::Instant::now(); }
        if self.size != (w, h) && (self.buf.is_empty() || self.want_since.elapsed().as_millis() >= 100) { self.rebuild(w, h); return true; }
        false
    }
    fn rebuild(&mut self, w: usize, h: usize) {
        let page = self.page;
        self.buf = match &self.src {
            None => vec![page; w * h],
            Some(src) => {
                let (sw, sh) = (src.width() as f32, src.height() as f32);
                let scale = (w as f32 / sw).max(h as f32 / sh);
                let (cw, ch) = (((w as f32 / scale).round() as u32).min(src.width()), ((h as f32 / scale).round() as u32).min(src.height()));
                let (cx, cy) = ((src.width() - cw) / 2, (src.height() - ch) / 2);
                let cropped = image::imageops::crop_imm(src, cx, cy, cw, ch);   // a view, not a copy
                let scaled = image::imageops::resize(&*cropped, w as u32, h as u32, image::imageops::FilterType::Triangle);
                let o = (self.opacity * 256.0) as u32;
                let (pr, pg, pb) = ((page >> 16) & 255, (page >> 8) & 255, page & 255);
                scaled.pixels().map(|p| {
                    let (r, g, b) = ((p[0] as u32 * o + pr * (256 - o)) >> 8, (p[1] as u32 * o + pg * (256 - o)) >> 8, (p[2] as u32 * o + pb * (256 - o)) >> 8);
                    (r << 16) | (g << 8) | b
                }).collect()
            }
        };
        self.size = (w, h);
    }
    /// The background at exactly window size (while a drag is in progress the scaled image may be for an older size).
    pub fn window_sized(&mut self, w: usize, h: usize) -> &[u32] {
        if self.size == (w, h) { return &self.buf; }
        let mut tmp = std::mem::take(&mut self.tmp);
        tmp.resize(w * h, 0);
        self.paint_into(&mut tmp, w, h);
        self.tmp = tmp;
        &self.tmp
    }
    /// Fill `buf` (a w x h window buffer) with the background. While a drag is in progress the exact scaled image does not exist yet, so sample
    /// the source directly with the same "cover, centred" geometry (nearest pixel: fast, and right at every size, not a stretched old image).
    pub fn paint_into(&self, buf: &mut [u32], w: usize, h: usize) {
        if self.size == (w, h) && self.buf.len() == buf.len() { buf.copy_from_slice(&self.buf); return; }
        let Some(src) = &self.src else { buf.fill(self.page); return };
        let (sw, sh) = (src.width() as usize, src.height() as usize);
        if w == 0 || h == 0 { buf.fill(self.page); return; }
        let scale = (w as f32 / sw as f32).max(h as f32 / sh as f32);
        let (ox, oy) = ((sw as f32 - w as f32 / scale) / 2.0, (sh as f32 - h as f32 / scale) / 2.0);
        let xs: Vec<usize> = (0..w).map(|x| (((x as f32 + 0.5) / scale + ox) as usize).min(sw - 1)).collect();
        // fade each sampled pixel over the page color here (a full-size pre-faded copy was 33 MB of RAM for a 4K image)
        let o = (self.opacity * 256.0) as u32;
        let (pr, pg, pb) = ((self.page >> 16) & 255, (self.page >> 8) & 255, self.page & 255);
        let raw = src.as_raw();
        for y in 0..h {
            let sy = (((y as f32 + 0.5) / scale + oy) as usize).min(sh - 1);
            let row = &raw[sy * sw * 3..(sy + 1) * sw * 3];
            for (d, &sx) in buf[y * w..(y + 1) * w].iter_mut().zip(&xs) {
                let p = &row[sx * 3..sx * 3 + 3];
                *d = (((p[0] as u32 * o + pr * (256 - o)) >> 8) << 16) | (((p[1] as u32 * o + pg * (256 - o)) >> 8) << 8) | ((p[2] as u32 * o + pb * (256 - o)) >> 8);
            }
        }
    }
}

/// Darken a pixel to 70% (a 30% black overlay) / lighten by 5% (a white 5% border).
#[inline] pub fn shade(p: u32) -> u32 { (((p >> 16) & 255) * 7 / 10) << 16 | (((p >> 8) & 255) * 7 / 10) << 8 | ((p & 255) * 7 / 10) }
#[inline] pub fn lighten(p: u32) -> u32 { let f = |c: u32| c + (255 - c) * 5 / 100; f((p >> 16) & 255) << 16 | f((p >> 8) & 255) << 8 | f(p & 255) }

/// Install the same font the web UI ends up with. Its stack is "Segoe UI, Tahoma, Geneva, Verdana, sans-serif"; on a machine with none of the
/// first four that is the system's default sans-serif. Linux asks fontconfig; elsewhere egui's built-in font stays.
/// A font file mapped into memory for the life of the program: egui copies owned font bytes (twice), but a static slice it keeps as is,
/// and the OS only loads the pages whose glyphs are used (a CJK font is ~20 MB).
fn map_font(path: &str) -> Option<&'static [u8]> {
    let f = std::fs::File::open(path).ok()?;
    let m = unsafe { memmap2::Mmap::map(&f).ok()? };
    let leaked: &'static memmap2::Mmap = Box::leak(Box::new(m));
    Some(&leaked[..])
}

/// DejaVu Sans (Bitstream Vera licence, assets/fonts/DejaVu-LICENSE.txt): the fallback UI font where the system has none to offer.
const EMBEDDED_REGULAR: &[u8] = include_bytes!("../assets/fonts/DejaVuSans.ttf");
const EMBEDDED_BOLD: &[u8] = include_bytes!("../assets/fonts/DejaVuSans-Bold.ttf");

/// Fonts for Chinese, Japanese, Korean, Thai, Indic, Hebrew and Arabic names on a system without fontconfig: (file, face index in a .ttc).
#[cfg(windows)]
fn platform_cjk_fonts() -> Vec<(std::path::PathBuf, u32)> {
    let dir = std::path::PathBuf::from(std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into())).join("Fonts");
    [("msyh.ttc", 0), ("YuGothR.ttc", 0), ("meiryo.ttc", 0), ("malgun.ttf", 0), ("simsun.ttc", 0), ("nirmala.ttf", 0), ("leelawui.ttf", 0), ("segoeui.ttf", 0)].into_iter().map(|(f, i)| (dir.join(f), i)).collect()
}
#[cfg(target_os = "macos")]
fn platform_cjk_fonts() -> Vec<(std::path::PathBuf, u32)> {
    ["/System/Library/Fonts/PingFang.ttc", "/System/Library/Fonts/Hiragino Sans GB.ttc", "/System/Library/Fonts/AppleSDGothicNeo.ttc", "/System/Library/Fonts/Supplemental/Arial Unicode.ttf", "/System/Library/Fonts/Thonburi.ttc", "/System/Library/Fonts/KohinoorDevanagari.ttc"].iter().map(|f| (std::path::PathBuf::from(f), 0)).collect()
}
#[cfg(not(any(windows, target_os = "macos")))]
fn platform_cjk_fonts() -> Vec<(std::path::PathBuf, u32)> { Vec::new() }

pub fn install_fonts(ctx: &egui::Context, cjk: bool) {
    // The web UI's stack is "Segoe UI, Tahoma, Geneva, Verdana, sans-serif". What it shows depends on the machine: the Flatpak builds
    // end up on DejaVu Sans (the freedesktop runtime's default), a bare host on its own default sans. Prefer DejaVu Sans where it is
    // installed so the native pages look like the app people run today, else the stack's own fontconfig match.
    let ask = |pat: &str| -> Option<Vec<u8>> {
        let out = std::process::Command::new("fc-match").args(["-f", "%{family}|%{file}", pat]).output().ok()?;
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let (family, file) = text.split_once('|')?;
        if pat.starts_with("DejaVu") && !family.contains("DejaVu") { return None; }   // fontconfig substituted something else
        std::fs::read(file.trim()).ok()
    };
    let (regular, bold) = match (ask("DejaVu Sans"), ask("DejaVu Sans:bold")) {
        (Some(r), Some(b)) => (Some(r), Some(b)),
        _ => match (ask("Segoe UI,Tahoma,Geneva,Verdana,sans-serif"), ask("Segoe UI,Tahoma,Geneva,Verdana,sans-serif:bold")) {
            (Some(r), Some(b)) => (Some(r), Some(b)),
            // no fontconfig (Windows, macOS) or no usable match: the copy of DejaVu Sans inside the program. egui's own fonts lack the arrows, stars, envelopes and other symbols the
            // menus use (they showed as empty boxes on Windows), and DejaVu Sans has all of them (checked against every symbol in the source).
            _ => (Some(EMBEDDED_REGULAR.to_vec()), Some(EMBEDDED_BOLD.to_vec())),
        },
    };
    let mut fonts = egui::FontDefinitions::default();
    let mut bold_family: Vec<String> = Vec::new();
    if let (Some(regular), Some(bold)) = (regular, bold) {
        fonts.font_data.insert("ui".into(), std::sync::Arc::new(egui::FontData::from_owned(regular)));
        fonts.font_data.insert("ui-bold".into(), std::sync::Arc::new(egui::FontData::from_owned(bold)));
        fonts.families.entry(egui::FontFamily::Proportional).or_default().insert(0, "ui".into());
        bold_family.push("ui-bold".into());
    }
    // Chinese, Japanese and Korean names: egui's own fonts have none of those glyphs, so borrow the system's CJK font (loaded only
    // when a name needs it, since the file is large). A .ttc holds several regional faces; index 2 of Noto Sans CJK is Simplified Chinese.
    if cjk {
        let mut seen: Vec<String> = Vec::new();
        for pat in ["sans:lang=zh-cn", "sans:lang=ja", "sans:lang=ko", "sans:lang=th", "sans:lang=he", "sans:lang=ar", "sans:lang=hi", "Noto Sans CJK SC", "WenQuanYi Micro Hei", "Microsoft YaHei", "PingFang SC"] {
            let Ok(out) = std::process::Command::new("fc-match").args(["-f", "%{file}", pat]).output() else { continue };
            let file = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let lower = file.to_lowercase();
            if file.is_empty() || seen.contains(&file) { continue; }
            if !(lower.contains("cjk") || lower.contains("noto") || lower.contains("wenquan") || lower.contains("yahei") || lower.contains("pingfang") || lower.contains("hei")) { continue; }
            let Some(bytes) = map_font(&file) else { continue };
            seen.push(file.clone());
            let name = format!("cjk{}", seen.len());
            let mut data = egui::FontData::from_static(bytes);
            if lower.ends_with(".ttc") && lower.contains("cjk") { data.index = 2; }
            fonts.font_data.insert(name.clone(), std::sync::Arc::new(data));
            for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] { fonts.families.entry(fam).or_default().push(name.clone()); }
        }
    }
    if cjk {
        for (n, (path, index)) in platform_cjk_fonts().into_iter().enumerate() {
            let Some(bytes) = map_font(&path.to_string_lossy()) else { continue };
            let name = format!("sys{n}");
            let mut data = egui::FontData::from_static(bytes);
            data.index = index;
            fonts.font_data.insert(name.clone(), std::sync::Arc::new(data));
            for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] { fonts.families.entry(fam).or_default().push(name.clone()); }
        }
    }
    // the "bold" family always exists (without a system font it is just the regular one), so asking for it never fails
    bold_family.extend(fonts.families.get(&egui::FontFamily::Proportional).cloned().unwrap_or_default());
    fonts.families.insert(egui::FontFamily::Name("bold".into()), bold_family);
    ctx.set_fonts(fonts);
}
pub fn bold(size: f32) -> egui::FontId { egui::FontId::new(size, egui::FontFamily::Name("bold".into())) }

#[cfg(test)]
mod edit_tests {
    use super::*;
    fn tmp() -> std::path::PathBuf { let d = std::env::temp_dir().join(format!("zest_theme_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d }
    #[test] fn the_active_theme_and_saved_themes_live_in_one_file_like_playdates() {
        let d = tmp();
        assert_eq!(read_vars(&d).iter().find(|(k, _)| k == "--accent").unwrap().1, "#66c0f4");
        let mut v = read_vars(&d); v.iter_mut().find(|(k, _)| k == "--accent").unwrap().1 = "#ff0000".into(); v.iter_mut().find(|(k, _)| k == "--bg-page").unwrap().1 = "nonsense".into();
        save_named(&d, "Red", &v).unwrap();
        save_active(&d, &v).unwrap();
        assert_eq!(Theme::load(&d).accent, (255, 0, 0)); assert_eq!(Theme::load(&d).bg_page, (0x0e, 0x14, 0x19), "a bad color is not written");
        assert_eq!(saved_names(&d), ["Red"]);
        assert_eq!(saved_vars(&d, "Red").unwrap().iter().find(|(k, _)| k == "--accent").unwrap().1, "#ff0000");
        delete_named(&d, "Red").unwrap(); assert!(saved_names(&d).is_empty());
    }
}
