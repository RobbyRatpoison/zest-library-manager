//! Toolkit-independent cover pipeline for the Library grid prototypes (Slint / iced / egui).
//! Workers decode covers off the UI thread (from a card-size tier on disk when there is one, else the
//! original JPEG with DCT scaling) into raw RGBA; the UI thread turns that into its toolkit's image
//! type through a closure and keeps it in a byte-budgeted cache. A view tracker prefetches ahead in
//! the scroll direction and workers drop jobs that scrolled out of the window, nearest-first. A tiny
//! always-resident tier (64x96 JPEG per cover) is upscaled smoothly on a worker as the placeholder.
pub mod art;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant};

pub const BUDGET_BYTES: usize = 48 * 1024 * 1024;        // floors: each grows to fit the whole prefetch window
pub const TINY_BUDGET_BYTES: usize = 24 * 1024 * 1024;
pub const AHEAD_ROWS: usize = 3;
pub const BEHIND_ROWS: usize = 1;
pub const MAX_EXTRA_AHEAD_ROWS: usize = 8;
const INSTANT_PREVIEWS_PER_FRAME: u32 = 256;   // effectively every visible card: a plain card looks worse than a slightly slower frame
const TINY_W: u32 = 128;
const PREVIEW_PACK: &str = "tiny128";

#[derive(Clone, Debug)]
pub struct Game { pub appid: i64, pub name: String, pub installed: bool }

/// Reading a file at an offset without moving its cursor: the platform's own on Unix, a loop over `seek_read` on Windows.
mod fileext {
    #[cfg(unix)] pub use std::os::unix::fs::FileExt;
    #[cfg(windows)] pub trait FileExt { fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> std::io::Result<()>; }
    #[cfg(windows)] impl FileExt for std::fs::File {
        fn read_exact_at(&self, mut buf: &mut [u8], mut offset: u64) -> std::io::Result<()> {
            use std::os::windows::fs::FileExt as W;
            while !buf.is_empty() {
                match self.seek_read(buf, offset) {
                    Ok(0) => return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "failed to fill whole buffer")),
                    Ok(n) => { buf = &mut buf[n..]; offset += n as u64; }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(e),
                }
            }
            Ok(())
        }
    }
}

pub fn rss_mb() -> u64 {
    std::fs::read_to_string("/proc/self/status").ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("VmRSS")).map(|l| l.to_string()))
        .and_then(|l| l.split_whitespace().nth(1).and_then(|v| v.parse::<u64>().ok()))
        .map(|kb| kb / 1024).unwrap_or(0)
}

/// Decode width for a card: rounded up to 16 px so a slider drag does not create endless variants.
pub fn bucket(card_w: u32) -> u32 { let st = BUCKET_STEP.load(Relaxed).max(1); (card_w + st - 1) / st * st }
/// Decode-width granularity (default 16). 1 = exact card width, so a renderer can blit 1:1.
pub static BUCKET_STEP: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(16);
pub fn set_bucket_step(step: u32) { BUCKET_STEP.store(step, Relaxed); }
/// One visual row of the grid: a group header (`count == 0`) or a row of cards `first .. first + count` of `games`.
#[derive(Clone, Debug)]
pub struct Row { pub y: f32, pub h: f32, pub first: usize, pub count: usize, pub section: Option<usize> }
/// A group: `len` games from `start`, drawn under a header; a collapsed one shows only the header.
#[derive(Clone, Debug)]
pub struct Section { pub start: usize, pub len: usize, pub collapsed: bool }
pub const HEADER_H: f32 = 34.0;
pub const SECTION_GAP: f32 = 10.0;

/// Horizontal covers (the Library's wide view): card shape 616:353 instead of 2:3, own cover folder and own packs. Chosen once at start.
pub static HORIZONTAL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub fn is_horizontal() -> bool { HORIZONTAL.load(Relaxed) }
/// Card height for a card width in the current shape.
pub fn card_h(w: u32) -> u32 { if is_horizontal() { (w as f32 * 353.0 / 616.0).round() as u32 } else { w * 3 / 2 } }
fn covers_dir(root: &std::path::Path) -> PathBuf { root.join(if is_horizontal() { "static/img/library/horizontal" } else { "static/img/library/vertical" }) }
/// A command for the heavy pack and tier builds: run through `nice -n 19` where it exists, so building covers never competes with a game for the CPU.
fn background(exe: PathBuf) -> std::process::Command {
    #[cfg(unix)]
    { let nice = std::path::Path::new("/usr/bin/nice"); if nice.exists() { let mut c = std::process::Command::new(nice); c.args(["-n", "19"]).arg(exe); return c; } }
    std::process::Command::new(exe)
}

/// Bumped when what a pack stores changes (v2: covers that do not fit their card are contained on edge-color bars instead of cropped), so old packs are rebuilt once.
const PACK_FORMAT: &str = "compose-v2";
fn packs_dir(root: &std::path::Path) -> PathBuf {
    let d = if is_horizontal() { root.join("static/img/library/h") } else { root.join("static/img/library") };
    let _ = std::fs::create_dir_all(&d);
    let marker = d.join("pack-format");
    if std::fs::read_to_string(&marker).map_or(true, |t| t.trim() != PACK_FORMAT) {
        if let Ok(rd) = std::fs::read_dir(&d) { for e in rd.flatten() { if e.path().extension().map_or(false, |x| x == "zpack") { let _ = std::fs::remove_file(e.path()); } } }
        let _ = std::fs::write(&marker, PACK_FORMAT);
    }
    d
}
/// The space between cards, across and down (PlayDate's `.game-grid { gap: 25px }`), and how far a row's cards sit below the row's top (half of it, rounded down).
pub const CARD_GAP: f32 = 25.0;
pub const ROW_TOP: f32 = 12.0;
pub fn row_height(card_w: u32) -> f32 { card_h(card_w) as f32 + CARD_GAP }

struct Job { idx: usize, appid: i64, width: u32, blur: bool, epoch: u64 }
struct Done { appid: i64, width: u32, blur: bool, epoch: u64, w: u32, h: u32, rgba: Vec<u8> }

// pending-set key: a blurry placeholder and the sharp cover of the same size are different jobs
fn pkey(appid: i64, width: u32, blur: bool) -> (i64, u32) { (appid, if blur { width | 0x8000_0000 } else { width }) }

struct Shared {
    queue: Mutex<Vec<Job>>,
    pending: Mutex<HashSet<(i64, u32)>>,
    cv: Condvar,
    epoch: AtomicU64,
    keep_lo: AtomicUsize,
    keep_hi: AtomicUsize,
    center: AtomicUsize,
    decode_us: AtomicU64,
    decodes: AtomicU64,
    /// The preview pack (128x192 JPEGs): only its index is in RAM, the bytes are read from disk when a preview is needed.
    preview: RwLock<Option<Tier>>,
    tiers: RwLock<Vec<Tier>>,
    /// Decodes that failed (no cover file, unreadable): not asked for again, or a card with no cover would be retried every frame forever.
    failed: Mutex<HashSet<(i64, u32, bool)>>,
}

// ---------------------------------------------------------------- loading and packs

pub fn load_games(root: &PathBuf) -> Vec<Game> {
    let db = std::fs::read_dir(root).unwrap().filter_map(|e| e.ok()).map(|e| e.path())
        .find(|p| p.file_name().and_then(|n| n.to_str()).map_or(false, |n| n.starts_with("games_") && n.ends_with(".db")))
        .expect("no games_*.db in project copy");
    // opened for writing, not read-only: a program that was killed in the middle of a write leaves a rollback journal, and only a connection that may write can roll it back
    // (a read-only one fails with "attempt to write a readonly database" and Zest could never start again). It never creates the file.
    let Ok(con) = rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE).or_else(|_| rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)) else { return Vec::new() };
    let games = match con.prepare("SELECT appid, name, installed FROM games WHERE duplicate_of IS NULL ORDER BY name COLLATE NOCASE") {
        Ok(mut st) => st.query_map([], |r| Ok(Game { appid: r.get(0)?, name: r.get(1)?, installed: r.get::<_, i64>(2)? != 0 })).map(|rows| rows.filter_map(|r| r.ok()).collect()).unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    games
}

impl Shared {
    fn has_preview(&self, appid: i64) -> bool { self.preview.read().unwrap().as_ref().map_or(false, |t| t.index.contains_key(&appid)) }
    fn preview_bytes(&self, appid: i64) -> Option<Vec<u8>> {
        use crate::fileext::FileExt;
        let guard = self.preview.read().unwrap();
        let t = guard.as_ref()?;
        let &(off, len) = t.index.get(&appid)?;
        let mut b = vec![0u8; len];
        t.file.read_exact_at(&mut b, off).ok()?;
        Some(b)
    }
}

/// JPEG bytes straight to RGBA (zune-jpeg, what `image` uses underneath, without the intermediate copies).
fn jpeg_rgba(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    use zune_jpeg::zune_core::{bytestream::ZCursor, colorspace::ColorSpace, options::DecoderOptions};
    let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
    let mut d = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(bytes), opts);
    let px = d.decode().ok()?;
    let (w, h) = d.dimensions()?;
    (px.len() == w * h * 4).then(|| (w as u32, h as u32, px))
}

fn fit_rgba((sw, sh, px): (u32, u32, Vec<u8>), w: u32, h: u32) -> (u32, u32, Vec<u8>) {
    if sw == w && sh == h { return (w, h, px); }
    let img = image::RgbaImage::from_raw(sw, sh, px).unwrap();
    (w, h, image::DynamicImage::ImageRgba8(img).resize_to_fill(w, h, image::imageops::FilterType::Triangle).to_rgba8().into_raw())   // an already composed card at a slightly different size: same shape, so a plain resize
}

/// Vertical box blur (radius `r` pixels, edges repeat): previews that look like motion blur while scrolling instead of an unloaded image.
/// One pass of running sums per row, so it costs a few multiplications per pixel.
fn blur_vertical(rgba: &mut [u8], w: usize, h: usize, r: usize) {
    if h < 2 || w == 0 { return; }
    let (stride, win) = (w * 4, (2 * r + 1) as u32);
    let src = rgba.to_vec();
    let row = |y: usize| &src[y * stride..(y + 1) * stride];
    let mut sum = vec![0u32; stride];
    for (s, &v) in sum.iter_mut().zip(row(0)) { *s = v as u32 * r as u32; }
    for y in 0..=r.min(h - 1) { for (s, &v) in sum.iter_mut().zip(row(y)) { *s += v as u32; } }
    for y in 0..h {
        for (o, s) in rgba[y * stride..(y + 1) * stride].iter_mut().zip(&sum) { *o = (*s / win) as u8; }
        let (add, sub) = (row((y + r + 1).min(h - 1)), row(y.saturating_sub(r)));
        for i in 0..stride { sum[i] = sum[i] + add[i] as u32 - sub[i] as u32; }
    }
}

/// The preview pack's JPEG for a game, decoded and given its streak.
fn preview_rgba(shared: &Shared, appid: i64) -> Option<(u32, u32, Vec<u8>)> {
    let bytes = shared.preview_bytes(appid)?;
    let (w, h, mut rgba) = jpeg_rgba(&bytes)?;
    blur_vertical(&mut rgba, w as usize, h as usize, 3);
    Some((w, h, rgba))
}

/// A card-sized tier: only the index is in RAM, the JPEG bytes are read from disk on demand.
struct Tier { file: std::fs::File, index: HashMap<i64, (u64, usize)>, width: u32 }

// ---------------------------------------------------------------- packs (streamed, incremental)
//
// A pack file holds JPEGs back to back, then an index, then a trailer:
//   [blobs...][n x (appid i64, offset u64, len u32, cover mtime u64)][n u32][magic u32]
// so it can be written as a stream (memory stays small however big the library is) and updated by appending: a cover that is new or
// whose file changed (art replaced or cropped) is encoded again, the rest is kept as it is.

const PACK_MAGIC: u32 = 0x324B_505A;
const ENTRY_BYTES: usize = 28;

#[derive(Clone, Copy)]
struct PackEntry { appid: i64, off: u64, len: u32, mtime: u64 }

fn pack_path(root: &PathBuf, name: &str) -> PathBuf { packs_dir(root).join(format!("{name}.zpack")) }

fn read_pack_index(path: &std::path::Path) -> Option<(Vec<PackEntry>, u64)> {
    use crate::fileext::FileExt;
    let f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    if len < 8 { return None; }
    let mut tr = [0u8; 8];
    f.read_exact_at(&mut tr, len - 8).ok()?;
    let n = u32::from_le_bytes(tr[0..4].try_into().unwrap()) as usize;
    if u32::from_le_bytes(tr[4..8].try_into().unwrap()) != PACK_MAGIC { return None; }
    let idx_len = (n * ENTRY_BYTES) as u64;
    if len < 8 + idx_len { return None; }
    let data_end = len - 8 - idx_len;
    let mut raw = vec![0u8; idx_len as usize];
    f.read_exact_at(&mut raw, data_end).ok()?;
    let entries = (0..n).map(|i| { let e = &raw[i * ENTRY_BYTES..(i + 1) * ENTRY_BYTES]; PackEntry {
        appid: i64::from_le_bytes(e[0..8].try_into().unwrap()), off: u64::from_le_bytes(e[8..16].try_into().unwrap()),
        len: u32::from_le_bytes(e[16..20].try_into().unwrap()), mtime: u64::from_le_bytes(e[20..28].try_into().unwrap()) } }).collect();
    Some((entries, data_end))
}

fn write_pack_index(f: &mut std::fs::File, entries: &[PackEntry]) -> std::io::Result<()> {
    use std::io::Write;
    let mut buf = Vec::with_capacity(entries.len() * ENTRY_BYTES + 8);
    for e in entries { buf.extend_from_slice(&e.appid.to_le_bytes()); buf.extend_from_slice(&e.off.to_le_bytes()); buf.extend_from_slice(&e.len.to_le_bytes()); buf.extend_from_slice(&e.mtime.to_le_bytes()); }
    buf.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    buf.extend_from_slice(&PACK_MAGIC.to_le_bytes());
    f.write_all(&buf)
}

fn open_tier(root: &PathBuf, name: &str, width: u32) -> Option<Tier> {
    let path = pack_path(root, name);
    let (entries, _) = read_pack_index(&path)?;
    let index = entries.iter().filter(|e| e.len > 0).map(|e| (e.appid, (e.off, e.len as usize))).collect();
    Some(Tier { file: std::fs::File::open(path).ok()?, index, width })
}

/// The applied-size tier is `active<width>.zpack`; at most one is kept.
fn find_active_tier(root: &PathBuf) -> Option<Tier> {
    let mut best: Option<u32> = None;
    for e in std::fs::read_dir(packs_dir(root)).ok()?.filter_map(|e| e.ok()) {
        let n = e.file_name().to_string_lossy().to_string();
        if let Some(w) = n.strip_prefix("active").and_then(|r| r.strip_suffix(".zpack")).and_then(|w| w.parse::<u32>().ok()) { if best.map_or(true, |b| w > b) { best = Some(w); } }
    }
    best.and_then(|w| open_tier(root, &format!("active{w}"), w))
}

fn all_appids(root: &PathBuf) -> Vec<i64> {
    let Some(db) = std::fs::read_dir(root).ok().and_then(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path())
        .find(|p| p.file_name().and_then(|n| n.to_str()).map_or(false, |n| n.starts_with("games_") && n.ends_with(".db")))) else { return Vec::new() };
    let Ok(con) = rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { return Vec::new() };
    con.prepare("SELECT appid FROM games").ok().and_then(|mut s| s.query_map([], |r| r.get::<_, i64>(0)).ok().map(|it| it.flatten().collect())).unwrap_or_default()
}

/// CMYK (jpeg-decoder hands over plain ink amounts, 0 = none) to RGB, for the few covers saved that way.
fn cmyk_to_rgb(px: &[u8]) -> Vec<u8> {
    px.chunks_exact(4).flat_map(|p| { let k = p[3] as u32; [(((255 - p[0] as u32) * (255 - k)) / 255) as u8, (((255 - p[1] as u32) * (255 - k)) / 255) as u8, (((255 - p[2] as u32) * (255 - k)) / 255) as u8] }).collect()
}

fn encode_cover(path: &std::path::Path, w: u32, h: u32, quality: u8) -> Option<Vec<u8>> {
    let f = std::fs::File::open(path).ok()?;
    let mut dec = jpeg_decoder::Decoder::new(std::io::BufReader::new(f));
    dec.scale(w as u16, h as u16).ok()?;
    let px = dec.decode().ok()?;
    let info = dec.info()?;
    let rgb = match info.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => px,
        jpeg_decoder::PixelFormat::L8 => px.iter().flat_map(|&v| [v, v, v]).collect(),
        jpeg_decoder::PixelFormat::CMYK32 => cmyk_to_rgb(&px),
        _ => return None,
    };
    let img = image::RgbImage::from_raw(info.width as u32, info.height as u32, rgb)?;
    let small = art::compose_card(&image::DynamicImage::ImageRgb8(img), w, h, is_horizontal());
    let mut buf = Vec::new();
    let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality);
    image::DynamicImage::ImageRgb8(small).write_with_encoder(enc).ok()?;
    Some(buf)
}

fn cover_mtime(path: &std::path::Path) -> Option<u64> {
    let m = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(m.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs())
}

/// Bring a pack up to date with the cover files: encode what is new or changed, drop what is gone, leave the rest. Returns the appids whose
/// entry was (re)written. Memory stays bounded: covers are encoded a batch at a time and appended as they finish.
pub fn update_pack(root: &PathBuf, name: &str, w: u32, h: u32, quality: u8) -> Vec<i64> {
    use std::io::{Seek, SeekFrom, Write};
    let path = pack_path(root, name);
    let covers = covers_dir(root);
    let existing = read_pack_index(&path);
    let old: HashMap<i64, PackEntry> = existing.as_ref().map(|(e, _)| e.iter().map(|x| (x.appid, *x)).collect()).unwrap_or_default();
    let data_end = existing.as_ref().map_or(0, |(_, d)| *d);
    let (mut keep, mut todo): (Vec<PackEntry>, Vec<(i64, u64)>) = (Vec::new(), Vec::new());
    for appid in all_appids(root) {
        let Some(mt) = cover_mtime(&covers.join(format!("{appid}.jpg"))) else { continue };
        match old.get(&appid) { Some(e) if e.mtime == mt => keep.push(*e), _ => todo.push((appid, mt)) }
    }
    let kept_bytes: u64 = keep.iter().map(|e| e.len as u64).sum();
    let dropped = old.len() - keep.len().min(old.len());
    if todo.is_empty() && dropped == 0 && existing.is_some() { return Vec::new(); }
    // too much dead space (replaced or removed covers): start over into a new file, swapped in atomically (a running app keeps the old one open)
    let rebuild = existing.is_none() || (data_end > 0 && kept_bytes * 10 < data_end * 7);
    let (target, mut entries, mut pos) = if rebuild {
        for (appid, _) in keep.drain(..).map(|e| (e.appid, ())) { if let Some(mt) = cover_mtime(&covers.join(format!("{appid}.jpg"))) { todo.push((appid, mt)); } }
        (path.with_extension("zpack.tmp"), Vec::new(), 0u64)
    } else { (path.clone(), keep, data_end) };
    let Ok(mut f) = std::fs::OpenOptions::new().read(true).write(true).create(true).open(&target) else { return Vec::new() };
    if rebuild { let _ = f.set_len(0); } else { let _ = f.set_len(data_end); }
    let _ = f.seek(SeekFrom::Start(pos));
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let todo_ids: std::collections::HashSet<i64> = todo.iter().map(|t| t.0).collect();
    for batch in todo.chunks(threads * 16) {
        let results: Vec<Option<(i64, u64, Vec<u8>)>> = std::thread::scope(|sc| {
            let hs: Vec<_> = batch.chunks((batch.len() + threads - 1) / threads).map(|part| {
                let covers = &covers;
                sc.spawn(move || part.iter().map(|&(appid, mt)| encode_cover(&covers.join(format!("{appid}.jpg")), w, h, quality).map(|b| (appid, mt, b))).collect::<Vec<_>>())
            }).collect();
            hs.into_iter().flat_map(|h| h.join().unwrap()).collect()
        });
        for ((appid, mt), res) in batch.iter().zip(results) {
            match res {
                Some((_, _, b)) => {
                    if f.write_all(&b).is_err() { return Vec::new(); }
                    entries.push(PackEntry { appid: *appid, off: pos, len: b.len() as u32, mtime: *mt });
                    pos += b.len() as u64;
                }
                // a cover that cannot be encoded is remembered (length 0) so it is not tried again until its file changes
                None => entries.push(PackEntry { appid: *appid, off: pos, len: 0, mtime: *mt }),
            }
        }
    }
    if write_pack_index(&mut f, &entries).is_err() { return Vec::new(); }
    drop(f);
    if rebuild {
        // Windows can refuse to replace a pack another handle still has open (a virus scanner, the running app): retry briefly. If the swap
        // never happens the old pack stays valid, so drop the temp file and report nothing changed (the next run tries again).
        let mut swapped = false;
        for attempt in 0..6 {
            if std::fs::rename(&target, &path).is_ok() { swapped = true; break; }
            if attempt < 5 { std::thread::sleep(std::time::Duration::from_millis(100)); }
        }
        if !swapped { let _ = std::fs::remove_file(&target); return Vec::new(); }
    }
    // what the app must forget: covers written now, and covers that are gone (their old picture would otherwise stay in its caches)
    let now_ids: std::collections::HashSet<i64> = entries.iter().map(|e| e.appid).collect();
    let mut out: Vec<i64> = entries.iter().filter(|e| e.len > 0 && todo_ids.contains(&e.appid)).map(|e| e.appid).collect();
    out.extend(old.keys().filter(|id| !now_ids.contains(id)));
    out
}

/// Build the card-size tier for `width` in a child process (its worker threads' memory never lands in the app). `true` once it has finished.
pub fn build_tier_async(root: &PathBuf, width: u32) -> Arc<std::sync::atomic::AtomicBool> {
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (d2, root) = (done.clone(), root.clone());
    std::thread::spawn(move || {
        let _ = background(self_exe()).arg(&root).arg("--build-tier").arg(width.to_string()).status();
        d2.store(true, Relaxed);
    });
    done
}

fn self_exe() -> std::path::PathBuf {
    // The real path of this executable. NOT the string "/proc/self/exe": the helper is started through `nice`, which would resolve that link to ITSELF.
    // When the binary was replaced on disk while running (an update, a rebuild), Linux reports "<path> (deleted)": the file at <path> is then the new build.
    if cfg!(target_os = "linux") {
        if let Ok(p) = std::fs::read_link("/proc/self/exe") {
            let s = p.to_string_lossy();
            return match s.strip_suffix(" (deleted)") { Some(t) => std::path::PathBuf::from(t), None => p.clone() };
        }
    }
    std::env::current_exe().unwrap_or_default()
}

/// A background refresh of the preview pack and the applied tier against the cover files (new games, replaced art).
pub struct PackJob { pub done: Arc<std::sync::atomic::AtomicBool>, pub changed: Arc<Mutex<Vec<i64>>> }

pub fn update_packs_async(root: &PathBuf) -> PackJob {
    let job = PackJob { done: Arc::new(std::sync::atomic::AtomicBool::new(false)), changed: Arc::new(Mutex::new(Vec::new())) };
    let (done, changed, root) = (job.done.clone(), job.changed.clone(), root.clone());
    std::thread::spawn(move || {
        use std::io::{BufRead, BufReader};
        let child = background(self_exe()).arg(&root).arg("--update-packs").stdout(std::process::Stdio::piped()).spawn();
        if let Ok(mut c) = child {
            if let Some(out) = c.stdout.take() {
                for line in BufReader::new(out).lines().flatten() { if let Some(id) = line.strip_prefix("changed ").and_then(|v| v.parse::<i64>().ok()) { changed.lock().unwrap().push(id); } }
            }
            let _ = c.wait();
        }
        done.store(true, Relaxed);
    });
    job
}

/// Call first thing in `main`: handles the one-shot builder modes (run as child processes so their
/// worker threads' memory never lands in the app). Returns true if it did a build and main should exit.
pub fn run_builder_mode(root: &PathBuf) -> bool {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--build-pack") { update_pack(root, PREVIEW_PACK, TINY_W, card_h(TINY_W), 80); return true; }
    if let Some(i) = args.iter().position(|a| a == "--build-tier") {
        let w: u32 = args.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(208);
        update_pack(root, &format!("active{w}"), w, card_h(w), 85);
        return true;
    }
    if args.iter().any(|a| a == "--update-packs") {
        let mut ids = update_pack(root, PREVIEW_PACK, TINY_W, card_h(TINY_W), 80);
        if let Some(t) = find_active_tier(root) { ids.extend(update_pack(root, &format!("active{}", t.width), t.width, card_h(t.width), 85)); }
        ids.sort_unstable(); ids.dedup();
        for id in ids { println!("changed {id}"); }
        return true;
    }
    false
}

fn ensure_packs(root: &PathBuf) {
    let exe = self_exe();
    if !pack_path(root, PREVIEW_PACK).exists() {
        let t = Instant::now();
        let _ = background(exe.clone()).arg(root).arg("--build-pack").status();
        println!("{:<34} {:.1}s (one-time)", "preview pack built", t.elapsed().as_secs_f32());
    }
    if find_active_tier(root).is_none() {
        let t = Instant::now();
        let _ = background(exe.clone()).arg(root).arg("--build-tier").arg("208").status();
        println!("{:<34} {:.1}s (one-time)", "default-size tier built", t.elapsed().as_secs_f32());
    }
}

// ---------------------------------------------------------------- decoding (worker threads)

fn rgba_of(img: &image::DynamicImage, w: u32, h: u32) -> (u32, u32, Vec<u8>) {
    let rgba = if img.width() == w && img.height() == h { img.to_rgba8() }
        else { image::DynamicImage::ImageRgb8(art::compose_card(img, w, h, is_horizontal())).to_rgba8() };
    (w, h, rgba.into_raw())
}

fn decode(shared: &Shared, covers: &PathBuf, job: &Job) -> Option<(u32, u32, Vec<u8>)> {
    use crate::fileext::FileExt;
    let (w, h) = (job.width, card_h(job.width));
    // Smallest pre-sized tier that is at least ~85% of the wanted width: ~1 ms instead of ~10 ms.
    {
        let tiers = shared.tiers.read().unwrap();
        if let Some(t) = tiers.iter().find(|t| t.width * 100 >= w * 85) {
            if let Some(&(off, len)) = t.index.get(&job.appid) {
                let mut bytes = vec![0u8; len];
                if t.file.read_exact_at(&mut bytes, off).is_ok() {
                    if let Some(px) = jpeg_rgba(&bytes) { return Some(fit_rgba(px, w, h)); }
                }
            }
        }
    }
    let f = std::fs::File::open(covers.join(format!("{}.jpg", job.appid))).ok()?;
    let mut dec = jpeg_decoder::Decoder::new(std::io::BufReader::new(f));
    // Accept up to ~15% upscaling from the nearest DCT scale: far cheaper than the next scale up.
    dec.scale((w * 85 / 100).max(1) as u16, (h * 85 / 100).max(1) as u16).ok()?;
    let px = dec.decode().ok()?;
    let info = dec.info()?;
    let rgb = match info.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => px,
        jpeg_decoder::PixelFormat::L8 => px.iter().flat_map(|&v| [v, v, v]).collect(),
        jpeg_decoder::PixelFormat::CMYK32 => cmyk_to_rgb(&px),
        _ => return None,
    };
    let img = image::RgbImage::from_raw(info.width as u32, info.height as u32, rgb)?;
    Some(rgba_of(&image::DynamicImage::ImageRgb8(img), w, h))
}

/// Placeholder: the 64x96 pack entry smoothly upscaled to the card's size. Cheap, so it runs first.
fn decode_blur(shared: &Shared, job: &Job) -> Option<(u32, u32, Vec<u8>)> {
    let (sw, sh, px) = preview_rgba(shared, job.appid)?;
    let (w, h) = (job.width, card_h(job.width));
    let small = image::RgbaImage::from_raw(sw, sh, px)?;
    Some((w, h, image::imageops::resize(&small, w, h, image::imageops::FilterType::Triangle).into_raw()))
}

fn spawn_workers(shared: Arc<Shared>, covers: PathBuf, tx: Sender<Done>) {
    let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 8);
    for _ in 0..n {
        let (shared, covers, tx) = (shared.clone(), covers.clone(), tx.clone());
        std::thread::spawn(move || loop {
            let job = {
                let mut q = shared.queue.lock().unwrap();
                loop {
                    let (lo, hi, c) = (shared.keep_lo.load(Relaxed), shared.keep_hi.load(Relaxed), shared.center.load(Relaxed));
                    let epoch = shared.epoch.load(Relaxed);
                    let mut dropped: Vec<(i64, u32)> = Vec::new();
                    q.retain(|j| {
                        let keep = j.epoch == epoch && j.idx >= lo && j.idx <= hi;
                        if !keep { dropped.push(pkey(j.appid, j.width, j.blur)); }
                        keep
                    });
                    if !dropped.is_empty() {
                        let mut p = shared.pending.lock().unwrap();
                        for d in dropped { p.remove(&d); }
                    }
                    if let Some(pos) = (0..q.len()).min_by_key(|&i| (!q[i].blur, q[i].idx.abs_diff(c))) {
                        break q.swap_remove(pos);
                    }
                    q = shared.cv.wait_timeout(q, Duration::from_millis(40)).unwrap().0;
                }
            };
            let t = Instant::now();
            let out = if job.blur { decode_blur(&shared, &job) } else { decode(&shared, &covers, &job) };
            if let Some((w, h, rgba)) = out {
                shared.decode_us.fetch_add(t.elapsed().as_micros() as u64, Relaxed);
                shared.decodes.fetch_add(1, Relaxed);
                let _ = tx.send(Done { appid: job.appid, width: job.width, blur: job.blur, epoch: job.epoch, w, h, rgba });
            } else {
                shared.failed.lock().unwrap().insert((job.appid, job.width, job.blur));
                shared.pending.lock().unwrap().remove(&pkey(job.appid, job.width, job.blur));
            }
        });
    }
}

// ---------------------------------------------------------------- the UI-thread side

struct Entry<T> { width: u32, img: T, bytes: usize, tick: u64 }

/// What a card should draw right now.
pub enum Cover<T> { Sharp(T), Blurry(T), None }

/// Evict least-recently-used entries until under budget, but never one inside the keep window:
/// evicting a visible or about-to-be-visible cover is what made cards flicker.
fn evict_outside<T>(map: &mut HashMap<i64, Entry<T>>, total: &mut usize, budget: usize, lo: usize, hi: usize,
                    index_of: &HashMap<i64, usize>) {
    while *total > budget {
        let victim = map.iter().filter(|(k, _)| index_of.get(k).map_or(true, |&i| i < lo || i > hi))
            .min_by_key(|(_, e)| e.tick).map(|(k, _)| *k);
        match victim.and_then(|k| map.remove(&k)) {
            Some(e) => *total -= e.bytes,
            None => break,
        }
    }
}

pub struct Core<T: Clone> {
    pub games: Vec<Game>,
    index_of: HashMap<i64, usize>,
    pub cols: usize,
    pub card_w: u32,
    pub sections: Vec<Section>,
    pub rows: Vec<Row>,
    idx_row: Vec<u32>,   // game index -> row (u32::MAX: hidden in a collapsed group)
    shared: Arc<Shared>,
    rx: Receiver<Done>,
    make: Box<dyn Fn(u32, u32, Vec<u8>) -> T>,
    cache: HashMap<i64, Entry<T>>,
    bytes: usize,
    tiny: HashMap<i64, Entry<T>>,
    tiny_bytes: usize,
    tick: u64,
    quick: HashMap<i64, T>,        // 64x96 previews decoded on the spot (bounded), so a card never waits for a worker to show something
    last_view: Option<(f32, Instant)>,
    instant_left: u32,             // on-the-spot previews still allowed this frame (they cost main-thread time)
    speed: f32,                    // scroll speed in px/s, smoothed
}

impl<T: Clone> Core<T> {
    /// `make` builds the toolkit's image from (width, height, RGBA8 bytes).
    pub fn new(root: &PathBuf, make: impl Fn(u32, u32, Vec<u8>) -> T + 'static) -> Self {
        println!("{:<34} RSS {:4} MB", "start", rss_mb());
        ensure_packs(root);
        let games = load_games(root);
        println!("{:<34} RSS {:4} MB", format!("model loaded ({} games)", games.len()), rss_mb());
        let preview = open_tier(root, PREVIEW_PACK, TINY_W);
        let preview_n = preview.as_ref().map_or(0, |t| t.index.len());
        let tiers: Vec<Tier> = find_active_tier(root).into_iter().collect();
        println!("{:<34} RSS {:4} MB ({} tiny covers; applied tier {} px)", "packs loaded", rss_mb(), preview_n,
            tiers.first().map_or(0, |t| t.width));
        let shared = Arc::new(Shared { queue: Mutex::new(Vec::new()), pending: Mutex::new(HashSet::new()),
            cv: Condvar::new(), epoch: AtomicU64::new(0), keep_lo: AtomicUsize::new(0), keep_hi: AtomicUsize::new(60),
            center: AtomicUsize::new(0), decode_us: AtomicU64::new(0), decodes: AtomicU64::new(0),
            preview: RwLock::new(preview), tiers: RwLock::new(tiers), failed: Mutex::new(HashSet::new()) });
        let (tx, rx) = channel::<Done>();
        spawn_workers(shared.clone(), covers_dir(root), tx);
        let index_of = games.iter().enumerate().map(|(i, g)| (g.appid, i)).collect();
        let mut core = Core { games, index_of, cols: 6, card_w: 200, sections: Vec::new(), rows: Vec::new(), idx_row: Vec::new(), shared, rx, make: Box::new(make), cache: HashMap::new(), bytes: 0,
               tiny: HashMap::new(), tiny_bytes: 0, tick: 0, quick: HashMap::new(), last_view: None, instant_left: 0, speed: 0.0 };
        core.rebuild_rows();
        core
    }

    /// Swap in a different game list (a new filter or sort); covers already decoded stay cached by appid.
    /// `sections` groups it (empty = one ungrouped list); sections must tile `games` in order.
    pub fn set_games(&mut self, games: Vec<Game>, sections: Vec<Section>) {
        self.index_of = games.iter().enumerate().map(|(i, g)| (g.appid, i)).collect();
        self.games = games;
        self.sections = sections;
        self.shared.failed.lock().unwrap().clear();
        self.rebuild_rows();
        self.shared.epoch.fetch_add(1, Relaxed);
        self.shared.queue.lock().unwrap().clear();
        self.shared.pending.lock().unwrap().clear();
    }

    /// Start using the tier just built for `width`; older tiers are deleted (at most one is kept).
    pub fn adopt_tier(&mut self, root: &PathBuf, width: u32) {
        if let Ok(rd) = std::fs::read_dir(packs_dir(root)) {
            for e in rd.filter_map(|e| e.ok()) {
                let n = e.file_name().to_string_lossy().to_string();
                if let Some(w) = n.strip_prefix("active").and_then(|r| r.strip_suffix(".zpack")).and_then(|w| w.parse::<u32>().ok()) { if w != width { let _ = std::fs::remove_file(e.path()); } }
            }
        }
        if let Some(t) = open_tier(root, &format!("active{width}"), width) { *self.shared.tiers.write().unwrap() = vec![t]; }
    }

    /// Forget everything decoded (the Library page was left): the covers are decoded again from the packs when it is opened.
    pub fn release_caches(&mut self) {
        self.cache = HashMap::new(); self.bytes = 0;
        self.tiny = HashMap::new(); self.tiny_bytes = 0;
        self.quick = HashMap::new();
        self.shared.epoch.fetch_add(1, Relaxed);
        self.shared.queue.lock().unwrap().clear();
        self.shared.pending.lock().unwrap().clear();
    }

    /// After the packs were refreshed on disk: reopen them and forget what was decoded for the covers that changed.
    pub fn reload_packs(&mut self, root: &PathBuf, changed: &[i64]) {
        *self.shared.preview.write().unwrap() = open_tier(root, PREVIEW_PACK, TINY_W);
        if let Some(t) = find_active_tier(root) { *self.shared.tiers.write().unwrap() = vec![t]; }
        for id in changed {
            if let Some(e) = self.cache.remove(id) { self.bytes -= e.bytes; }
            if let Some(e) = self.tiny.remove(id) { self.tiny_bytes -= e.bytes; }
            self.quick.remove(id);
        }
        self.shared.failed.lock().unwrap().clear();
        self.shared.epoch.fetch_add(1, Relaxed);
        self.shared.queue.lock().unwrap().clear();
        self.shared.pending.lock().unwrap().clear();
    }

    pub fn toggle_section(&mut self, i: usize) {
        if let Some(s) = self.sections.get_mut(i) { s.collapsed = !s.collapsed; self.rebuild_rows(); }
    }

    fn rebuild_rows(&mut self) {
        let (cols, rowh) = (self.cols.max(1), row_height(self.card_w));
        self.rows.clear();
        self.idx_row = vec![u32::MAX; self.games.len()];
        let mut y = 0.0f32;
        if self.sections.is_empty() {
            let mut i = 0;
            while i < self.games.len() {
                let count = cols.min(self.games.len() - i);
                for k in 0..count { self.idx_row[i + k] = self.rows.len() as u32; }
                self.rows.push(Row { y, h: rowh, first: i, count, section: None });
                y += rowh; i += count;
            }
            return;
        }
        for (si, s) in self.sections.iter().enumerate() {
            let h = HEADER_H + if si > 0 { SECTION_GAP } else { 0.0 };
            self.rows.push(Row { y, h, first: s.start, count: 0, section: Some(si) });
            y += h;
            if s.collapsed { continue; }
            let mut i = s.start;
            while i < s.start + s.len {
                let count = cols.min(s.start + s.len - i);
                for k in 0..count { self.idx_row[i + k] = self.rows.len() as u32; }
                self.rows.push(Row { y, h: rowh, first: i, count, section: Some(si) });
                y += rowh; i += count;
            }
        }
    }

    pub fn index_of(&self, appid: i64) -> Option<usize> { self.index_of.get(&appid).copied() }

    pub fn row_count(&self) -> usize { self.rows.len() }
    pub fn rows_height(&self) -> f32 { self.rows.last().map_or(0.0, |r| r.y + r.h) }

    /// Rows touching the vertical span [y0, y1), as an index range (empty when there are none).
    pub fn rows_in(&self, y0: f32, y1: f32) -> std::ops::Range<usize> {
        let a = self.rows.partition_point(|r| r.y + r.h <= y0);
        let b = self.rows.partition_point(|r| r.y < y1);
        a..b.max(a)
    }

    /// Where game `idx` sits: (row top y, column), or None when its group is collapsed.
    pub fn pos_of(&self, idx: usize) -> Option<(f32, usize)> {
        let r = *self.idx_row.get(idx)?;
        if r == u32::MAX { return None; }
        let row = &self.rows[r as usize];
        Some((row.y, idx - row.first))
    }

    fn request(&self, idx: usize, appid: i64, want: u32, blur: bool) {
        if self.shared.failed.lock().unwrap().contains(&(appid, want, blur)) { return; }
        if self.shared.pending.lock().unwrap().insert(pkey(appid, want, blur)) {
            let job = Job { idx, appid, width: want, blur, epoch: self.shared.epoch.load(Relaxed) };
            self.shared.queue.lock().unwrap().push(job);
        }
    }

    pub fn set_layout(&mut self, cols: usize, card_w: u32) {
        let cols = cols.max(1);
        if cols == self.cols && card_w == self.card_w { return; }
        self.cols = cols;
        self.card_w = card_w;
        self.rebuild_rows();
        self.shared.epoch.fetch_add(1, Relaxed);
        self.shared.queue.lock().unwrap().clear();
        self.shared.pending.lock().unwrap().clear();
    }

    /// The 64x96 pack entry decoded right now (about 0.2 ms); the grid scales it up smoothly. None for a game with no pack entry.
    fn instant_tiny(&mut self, appid: i64) -> Option<T> {
        if let Some(t) = self.quick.get(&appid) { return Some(t.clone()); }
        let (w, h, rgba) = preview_rgba(&self.shared, appid)?;
        let t = (self.make)(w, h, rgba);
        if self.quick.len() >= 400 { self.quick.clear(); }
        self.quick.insert(appid, t.clone());
        Some(t)
    }

    /// What to draw for game `idx`: the sharp cover if it is there at the right size, else the previous
    /// size, else the blurry placeholder. Requests whatever is missing.
    pub fn cover(&mut self, idx: usize) -> Cover<T> {
        let want = bucket(self.card_w);
        let appid = self.games[idx].appid;
        self.tick += 1;
        let tick = self.tick;
        let have = self.cache.get_mut(&appid).map(|e| { e.tick = tick; (e.width, e.img.clone()) });
        if let Some((w, img)) = &have {
            if *w == want { return Cover::Sharp(img.clone()); }
        }
        self.request(idx, appid, want, false);
        if let Some((_, img)) = have { self.shared.cv.notify_all(); return Cover::Sharp(img); }
        let tiny = self.tiny.get_mut(&appid).map(|e| { e.tick = tick; (e.width, e.img.clone()) });
        if let Some((tw, img)) = tiny {
            if tw != want { self.request(idx, appid, want, true); }
            self.shared.cv.notify_all();
            return Cover::Blurry(img);
        }
        self.request(idx, appid, want, true);   // a worker makes the card-sized preview (first in the queue)
        self.shared.cv.notify_all();
        if let Some(img) = self.quick.get(&appid) { return Cover::Blurry(img.clone()); }
        if self.instant_left > 0 {
            if let Some(img) = self.instant_tiny(appid) { self.instant_left -= 1; return Cover::Blurry(img); }
        }
        Cover::None
    }

    /// Would `cover(idx)` draw an image (sharp, previous size, or the blurry placeholder) right now?
    pub fn has_image(&self, idx: usize) -> bool {
        let appid = self.games[idx].appid;
        self.cache.contains_key(&appid) || self.tiny.contains_key(&appid) || self.shared.has_preview(appid)
    }

    /// What `cover(idx)` would draw right now: 0 nothing, 1 blurry placeholder, 2 sharp cover of another size, 3 sharp at the exact size.
    pub fn card_state(&self, idx: usize) -> u8 {
        let appid = self.games[idx].appid;
        let want = bucket(self.card_w);
        match self.cache.get(&appid) {
            Some(e) => if e.width == want { 3 } else { 2 },
            None => if self.tiny.contains_key(&appid) || self.shared.has_preview(appid) { 1 } else { 0 },
        }
    }

    /// Call every few ms with the scroll offset (px from the top of the grid) and the grid's viewport
    /// height: protects visible covers from eviction and prefetches ahead in the scroll direction.
    pub fn update_view(&mut self, y: f32, view_h: f32, dir: i32) {
        let (cols, n) = (self.cols, self.games.len());
        if n == 0 || self.rows.is_empty() { return; }
        self.instant_left = INSTANT_PREVIEWS_PER_FRAME;
        let now = Instant::now();
        if let Some((py, pt)) = self.last_view {
            let dt = now.duration_since(pt).as_secs_f32();
            if dt > 0.0005 { let v = (y - py).abs() / dt; self.speed = if v > self.speed { v } else { self.speed * 0.9 + v * 0.1 }; }
        }
        self.last_view = Some((y, now));
        let vis = self.rows_in(y, y + view_h);
        let (first, last) = (vis.start.min(self.rows.len() - 1), vis.end.saturating_sub(1).min(self.rows.len() - 1));
        // look further ahead the faster the scroll (about 0.35 s of travel), within a memory cap
        let extra = ((self.speed * 0.35 / row_height(self.card_w)).ceil() as usize).min(MAX_EXTRA_AHEAD_ROWS);
        let (ahead_rows, behind_rows) = (AHEAD_ROWS + extra, BEHIND_ROWS);
        let (back, ahead) = if dir >= 0 { (behind_rows, ahead_rows) } else { (ahead_rows, behind_rows) };
        let span = first.saturating_sub(back)..=(last + ahead).min(self.rows.len() - 1);
        let (mut lo, mut hi) = (usize::MAX, 0usize);
        for r in &self.rows[span] { if r.count > 0 { lo = lo.min(r.first); hi = hi.max(r.first + r.count - 1); } }
        if lo == usize::MAX { return; }
        self.shared.keep_lo.store(lo, Relaxed);
        self.shared.keep_hi.store(hi, Relaxed);
        let mid = &self.rows[(first + last) / 2];
        self.shared.center.store(mid.first + cols / 2, Relaxed);
        let want = bucket(self.card_w);
        self.tick += 1;
        let tick = self.tick;
        let mut any = false;
        for idx in lo..=hi {
            let appid = self.games[idx].appid;
            let ok = match self.cache.get_mut(&appid) { Some(e) => { e.tick = tick; e.width == want } None => false };
            if !ok {
                self.request(idx, appid, want, false);
                if !self.cache.contains_key(&appid) && !self.tiny.get(&appid).map_or(false, |v| v.width == want) { self.request(idx, appid, want, true); }
                any = true;
            }
        }
        if any { self.shared.cv.notify_all(); }
    }

    /// Apply finished decodes. Returns the game indices whose image changed (for repaint/invalidation).
    pub fn poll(&mut self) -> Vec<usize> {
        let mut changed = Vec::new();
        let epoch = self.shared.epoch.load(Relaxed);
        let (lo, hi) = (self.shared.keep_lo.load(Relaxed), self.shared.keep_hi.load(Relaxed));
        while let Ok(d) = self.rx.try_recv() {
            self.shared.pending.lock().unwrap().remove(&pkey(d.appid, d.width, d.blur));
            if d.epoch != epoch { continue; }
            let bytes = d.rgba.len();
            let img = (self.make)(d.w, d.h, d.rgba);
            // the budget always fits the whole window, whatever the card size
            let window = (hi.saturating_sub(lo) + 1) * bytes * 3 / 2;
            let entry = Entry { width: d.width, img, bytes, tick: self.tick };
            if d.blur {
                if let Some(old) = self.tiny.insert(d.appid, entry) { self.tiny_bytes -= old.bytes; }
                self.tiny_bytes += bytes;
                evict_outside(&mut self.tiny, &mut self.tiny_bytes, TINY_BUDGET_BYTES.max(window), lo, hi, &self.index_of);
            } else {
                if let Some(old) = self.cache.insert(d.appid, entry) { self.bytes -= old.bytes; }
                self.bytes += bytes;
                // the sharp cover is here: its blurry placeholder is no longer needed
                if let Some(t) = self.tiny.remove(&d.appid) { self.tiny_bytes -= t.bytes; }
                evict_outside(&mut self.cache, &mut self.bytes, BUDGET_BYTES.max(window), lo, hi, &self.index_of);
            }
            if let Some(&i) = self.index_of.get(&d.appid) { changed.push(i); }
        }
        changed
    }

    /// (covers on screen that are sharp, that show anything, covers on screen), ignoring games with no cover file.
    pub fn ready(&self, y: f32, view_h: f32) -> (usize, usize, usize) {
        let want = bucket(self.card_w);
        let (mut ok, mut any, mut tot) = (0, 0, 0);
        for ri in self.rows_in(y, y + view_h) {
            let r = &self.rows[ri];
            for idx in r.first..r.first + r.count {
                let id = self.games[idx].appid;
                if !self.shared.has_preview(id) { continue; }
                tot += 1;
                let sharp = self.cache.get(&id).map_or(false, |e| e.width == want);
                if sharp { ok += 1; }
                if sharp || self.cache.contains_key(&id) || self.tiny.contains_key(&id) { any += 1; }
            }
        }
        (ok, any, tot)
    }

    pub fn epoch(&self) -> u64 { self.shared.epoch.load(Relaxed) }
    pub fn cache_counts(&self) -> (usize, usize) { (self.cache.len(), self.tiny.len()) }
    /// Decode jobs requested but not yet finished (queued or running).
    pub fn pending(&self) -> usize { self.shared.pending.lock().unwrap().len() }
    pub fn cache_mb(&self) -> usize { (self.bytes + self.tiny_bytes) / 1048576 }
    pub fn decode_stats(&self) -> (u64, f64) {
        let n = self.shared.decodes.load(Relaxed);
        (n, self.shared.decode_us.load(Relaxed) as f64 / n.max(1) as f64 / 1000.0)
    }
    pub fn reset_stats(&self) { self.shared.decodes.store(0, Relaxed); self.shared.decode_us.store(0, Relaxed); }
}

#[cfg(test)]
mod pack_tests {
    use super::*;

    fn jpeg(seed: u8) -> Vec<u8> {
        let img = image::RgbImage::from_fn(60, 90, |x, y| image::Rgb([seed, (x * 4) as u8, (y * 2) as u8]));
        let mut buf = Vec::new();
        image::DynamicImage::ImageRgb8(img).write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 90)).unwrap();
        buf
    }

    fn setup(ids: &[i64]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("zest-pack-test-{}-{}", std::process::id(), ids.len()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("static/img/library/vertical")).unwrap();
        let con = rusqlite::Connection::open(root.join("games_1.db")).unwrap();
        con.execute("CREATE TABLE games (appid INTEGER, name TEXT, installed INTEGER, duplicate_of TEXT)", []).unwrap();
        for id in ids { con.execute("INSERT INTO games VALUES (?, 'x', 0, NULL)", [id]).unwrap(); std::fs::write(root.join(format!("static/img/library/vertical/{id}.jpg")), jpeg(*id as u8)).unwrap(); }
        root
    }

    fn ids_in(root: &PathBuf, name: &str) -> Vec<i64> { let mut v: Vec<i64> = read_pack_index(&pack_path(root, name)).unwrap().0.iter().map(|e| e.appid).collect(); v.sort(); v }

    #[test]
    fn pack_updates_incrementally() {
        let root = setup(&[1, 2, 3, 4]);
        let first = update_pack(&root, "t", 32, 48, 80);
        assert_eq!(first.len(), 4);
        assert_eq!(ids_in(&root, "t"), vec![1, 2, 3, 4]);
        let tier = open_tier(&root, "t", 32).unwrap();
        assert!(tier.index.len() == 4);
        // nothing changed: nothing rewritten
        assert!(update_pack(&root, "t", 32, 48, 80).is_empty());
        // a new game with a cover, and cover 2 replaced (its file time moves)
        let con = rusqlite::Connection::open(root.join("games_1.db")).unwrap();
        con.execute("INSERT INTO games VALUES (5, 'x', 0, NULL)", []).unwrap();
        std::fs::write(root.join("static/img/library/vertical/5.jpg"), jpeg(5)).unwrap();
        let p2 = root.join("static/img/library/vertical/2.jpg");
        std::fs::write(&p2, jpeg(200)).unwrap();
        std::fs::File::options().write(true).open(&p2).unwrap().set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(100)).unwrap();
        let mut changed = update_pack(&root, "t", 32, 48, 80);
        changed.sort();
        assert_eq!(changed, vec![2, 5]);
        assert_eq!(ids_in(&root, "t"), vec![1, 2, 3, 4, 5]);
        // every entry still decodes
        let t = open_tier(&root, "t", 32).unwrap();
        for (_, &(off, len)) in t.index.iter() {
            use crate::fileext::FileExt;
            let mut b = vec![0u8; len]; t.file.read_exact_at(&mut b, off).unwrap();
            assert!(jpeg_rgba(&b).is_some());
        }
        // a cover that disappeared is dropped
        std::fs::remove_file(root.join("static/img/library/vertical/3.jpg")).unwrap();
        let gone = update_pack(&root, "t", 32, 48, 80);
        assert_eq!(ids_in(&root, "t"), vec![1, 2, 4, 5]);
        assert!(gone.contains(&3), "a removed cover is reported so the app drops its picture and its preview: {gone:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_broken_pack_is_rebuilt() {
        let root = setup(&[1, 2]);
        update_pack(&root, "t", 32, 48, 80);
        let p = pack_path(&root, "t");
        let bytes = std::fs::read(&p).unwrap();
        std::fs::write(&p, &bytes[..bytes.len() - 5]).unwrap();   // a write cut short
        assert!(read_pack_index(&p).is_none());
        assert_eq!(update_pack(&root, "t", 32, 48, 80).len(), 2);
        assert_eq!(ids_in(&root, "t"), vec![1, 2]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
