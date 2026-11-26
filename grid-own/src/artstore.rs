//! The app's side of the art downloader: saving a cover (JPEG, edge colors in the database), the shape check, and the Steam copy of a non-Steam game.
use grid_core::art;
pub use pd_filter::artclient::Kind;
use pd_filter::artclient::{ArtStore, Hooks};
use std::path::PathBuf;

/// The JPEG quality for a cover Zest has to convert (PNG, WebP: SteamGridDB serves these). PlayDate saved every cover at 95, so a stored quality of 95 means "saved by PlayDate"
/// (the Cover Quality filter's "Saved at JPEG quality 95"); a JPEG that arrives as one is written unchanged and keeps its own quality.
pub const CONVERT_QUALITY: u8 = 100;

/// `stage`: the Edit dialog's art operations write into the staging folder (`staging`) instead of the library, so a new picture can be looked at in the dialog and only
/// reaches the grid when the dialog is saved (`commit`); closing it throws the staged pictures away (`discard`).
pub struct FsStore { pub root: PathBuf, pub db: PathBuf, pub stage: bool }
impl FsStore {
    fn path(&self, kind: Kind, appid: i64) -> PathBuf { if self.stage { staged_file(&self.root, kind, appid) } else { library_file(&self.root, kind, appid) } }
}

fn kind_dir(kind: Kind) -> &'static str { match kind { Kind::Vertical => "vertical", Kind::Horizontal => "horizontal", Kind::Icon => "icons" } }
/// A cover's place in the library.
pub fn library_file(root: &std::path::Path, kind: Kind, appid: i64) -> PathBuf { root.join("static/img/library").join(kind_dir(kind)).join(format!("{appid}.jpg")) }
/// The folder staged pictures live in (emptied at every start: nothing staged survives a restart).
pub fn staging_root(root: &std::path::Path) -> PathBuf { root.join("art_staging") }
pub fn staged_file(root: &std::path::Path, kind: Kind, appid: i64) -> PathBuf { staging_root(root).join(kind_dir(kind)).join(format!("{appid}.jpg")) }
fn staged_note(root: &std::path::Path, kind: Kind, appid: i64) -> PathBuf { staging_root(root).join(kind_dir(kind)).join(format!("{appid}.note")) }
fn staged_cleared(root: &std::path::Path, kind: Kind, appid: i64) -> PathBuf { staging_root(root).join(kind_dir(kind)).join(format!("{appid}.cleared")) }
/// The picture the Edit dialog shows for a type: the staged one when there is one, else the library's. None when it was cleared or there is none.
pub fn current_file(root: &std::path::Path, kind: Kind, appid: i64) -> Option<PathBuf> {
    if staged_cleared(root, kind, appid).exists() { return None; }
    let s = staged_file(root, kind, appid); if s.is_file() { return Some(s); }
    let l = library_file(root, kind, appid); l.is_file().then_some(l)
}

/// What a staged picture will become when saved.
#[derive(Clone, Debug, PartialEq)]
pub struct Staged { pub kind: Kind, pub source: String, /// Came from the automatic order (a re-scrape): the game's `art_fetched` date is set too.
    pub order: bool, /// The picture is to be removed instead.
    pub cleared: bool }
pub const KINDS: [Kind; 3] = [Kind::Vertical, Kind::Horizontal, Kind::Icon];
/// Records where a staged picture came from (`source` is the tag the database keeps: 'custom', 'sgdb_grid', 'capsule_2x' ...).
pub fn stage_note(root: &std::path::Path, kind: Kind, appid: i64, source: &str, order: bool) {
    let _ = std::fs::remove_file(staged_cleared(root, kind, appid));
    let p = staged_note(root, kind, appid); if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); }
    let _ = std::fs::write(p, format!("{source}\n{}", if order { "order" } else { "" }));
}
/// "Clear" in the dialog: the picture goes when the dialog is saved, not before.
pub fn stage_clear(root: &std::path::Path, kind: Kind, appid: i64) {
    let _ = std::fs::remove_file(staged_file(root, kind, appid)); let _ = std::fs::remove_file(staged_note(root, kind, appid));
    let p = staged_cleared(root, kind, appid); if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); } let _ = std::fs::write(p, "");
}
/// Everything staged for a game.
pub fn staged(root: &std::path::Path, appid: i64) -> Vec<Staged> {
    KINDS.iter().filter_map(|&kind| {
        if staged_cleared(root, kind, appid).exists() { return Some(Staged { kind, source: String::new(), order: false, cleared: true }); }
        if !staged_file(root, kind, appid).is_file() { return None; }
        let note = std::fs::read_to_string(staged_note(root, kind, appid)).unwrap_or_default();
        let mut lines = note.lines();
        let source = lines.next().filter(|s| !s.is_empty()).unwrap_or("custom").to_string();
        Some(Staged { kind, source, order: lines.next() == Some("order"), cleared: false })
    }).collect()
}
/// Throws the game's staged pictures away.
pub fn discard(root: &std::path::Path, appid: i64) {
    for kind in KINDS { for p in [staged_file(root, kind, appid), staged_note(root, kind, appid), staged_cleared(root, kind, appid)] { let _ = std::fs::remove_file(p); } }
}
/// Puts the staged pictures into the library (the edges, sizes and sources in the database follow, as for any saved cover) and clears the staging. Returns what was staged;
/// the caller removes the pictures marked `cleared` (that is the library's own clear, which knows every column).
pub fn commit(root: &std::path::Path, db: &std::path::Path, appid: i64, today: &str) -> Result<Vec<Staged>, String> {
    let all = staged(root, appid);
    let real = FsStore { root: root.to_path_buf(), db: db.to_path_buf(), stage: false };
    for s in all.iter().filter(|s| !s.cleared) {
        let bytes = std::fs::read(staged_file(root, s.kind, appid)).map_err(|e| e.to_string())?;
        if !real.save_q(s.kind, appid, &bytes, CONVERT_QUALITY) { return Err("Could not save the new picture.".into()); }
        if let Ok(c) = rusqlite::Connection::open(db) {
            let _ = c.busy_timeout(std::time::Duration::from_secs(10));
            let col = match s.kind { Kind::Vertical => "vertical_art_source", Kind::Horizontal => "horizontal_art_source", Kind::Icon => "icon_source" };
            let _ = c.execute(&format!("UPDATE games SET {col} = ? WHERE appid = ?"), rusqlite::params![s.source, appid]);
            if s.order { let _ = c.execute("UPDATE games SET art_fetched = ? WHERE appid = ?", rusqlite::params![today, appid]); }
        }
    }
    discard(root, appid);
    Ok(all)
}
impl FsStore {
    /// `save_as_jpg` at `quality`: the JPEG, then the card-edge colors into the database (never fatal, like Python's `_store_art_edges`).
    pub fn save_q(&self, kind: Kind, appid: i64, bytes: &[u8], quality: u8) -> bool {
        let path = self.path(kind, appid);
        let Ok((w, h)) = art::save_cover(bytes, &path, quality) else { return false };
        if self.stage { return true; }   // a staged picture touches nothing but its own file
        // the file's pixel size, for the Cover Quality filter (not for icons)
        if kind != Kind::Icon {
            if let Ok(c) = rusqlite::Connection::open(&self.db) {
                let _ = c.busy_timeout(std::time::Duration::from_secs(10));
                let (cw, ch) = if kind == Kind::Horizontal { ("horizontal_art_w", "horizontal_art_h") } else { ("vertical_art_w", "vertical_art_h") };
                let cq = if kind == Kind::Horizontal { "horizontal_art_q" } else { "vertical_art_q" };
                let q = file_quality(&path).map(|q| q as i64);   // what the file on disk says (a JPEG kept as it arrived has its own quality, not `quality`)
                let _ = c.execute(&format!("UPDATE games SET {cw} = ?, {ch} = ?, {cq} = ? WHERE appid = ?"), rusqlite::params![w as i64, h as i64, q, appid]);
            }
        }
        // edge colors for the card backdrop (not for icons); never fatal, like Python's `_store_art_edges`
        if kind != Kind::Icon {
            if let Ok(img) = image::open(&path) {
                if let Some(v) = art::compute_edges(&img.to_rgb8(), kind == Kind::Horizontal) {
                    if let Ok(c) = rusqlite::Connection::open(&self.db) {
                        let _ = c.busy_timeout(std::time::Duration::from_secs(10));
                        let col = if kind == Kind::Horizontal { "edge_horizontal" } else { "edge_vertical" };
                        let _ = c.execute(&format!("UPDATE games SET {col} = ? WHERE appid = ?"), rusqlite::params![v, appid]);
                    }
                }
            }
        }
        true
    }

    /// `crop_artwork`: keep the box (top-left x, y and width w, as fractions of the stored picture) at the card's ratio; one lossless hand-off and one JPEG
    /// encode at quality 100. The source becomes 'custom'. Returns the new size.
    pub fn crop(&self, kind: Kind, appid: i64, x: f64, y: f64, w: f64) -> Result<(u32, u32), String> {
        if !(w > 0.0 && w <= 1.0 && (0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y)) { return Err("invalid crop".into()); }
        let path = if self.stage { current_file(&self.root, kind, appid).unwrap_or_else(|| self.path(kind, appid)) } else { self.path(kind, appid) };
        if !path.is_file() { return Err("This game has no image to crop.".into()); }
        let img = image::open(&path).map_err(|e| e.to_string())?.to_rgb8();
        let ratio = match kind { Kind::Vertical => art::VERTICAL_RATIO, Kind::Horizontal => art::HORIZONTAL_RATIO, Kind::Icon => 1.0 };
        let (l, t, cw, ch) = art::crop_box(img.width(), img.height(), x, y, w, ratio);
        let cropped = image::imageops::crop_imm(&img, l, t, cw, ch).to_image();
        let mut png = Vec::new();
        cropped.write_with_encoder(image::codecs::png::PngEncoder::new(&mut png)).map_err(|e| e.to_string())?;
        if !self.save_q(kind, appid, &png, 100) { return Err("Could not save the cropped image.".into()); }
        if self.stage { stage_note(&self.root, kind, appid, "custom", false); return Ok((cw, ch)); }
        let col = match kind { Kind::Vertical => "vertical_art_source", Kind::Horizontal => "horizontal_art_source", Kind::Icon => "icon_source" };
        if let Ok(c) = rusqlite::Connection::open(&self.db) { let _ = c.busy_timeout(std::time::Duration::from_secs(10)); let _ = c.execute(&format!("UPDATE games SET {col} = 'custom' WHERE appid = ?"), [appid]); }
        Ok((cw, ch))
    }
}

impl ArtStore for FsStore {
    fn save(&self, kind: Kind, appid: i64, bytes: &[u8]) -> bool { self.save_q(kind, appid, bytes, CONVERT_QUALITY) }
    fn fits(&self, kind: Kind, appid: i64) -> bool {
        if kind == Kind::Icon { return true; }
        image::image_dimensions(self.path(kind, appid)).map_or(false, |(w, h)| art::fits(w, h, kind == Kind::Horizontal))
    }
    fn read(&self, kind: Kind, appid: i64) -> Option<Vec<u8>> { std::fs::read(self.path(kind, appid)).ok() }
}

/// `images.steam_copy_of`: the duplicate link first, else the Steam match the metadata backfill saved.
pub struct DbHooks { pub db: PathBuf, /// For the "store" art source: the source that owns a game asks its store for the cover (one answer per game is kept for the three art types).
    pub sources: Option<std::sync::Arc<pd_filter::sources::Registry>>, pub cache: std::sync::Mutex<std::collections::HashMap<i64, Vec<(String, String)>>> }
impl DbHooks { pub fn new(db: PathBuf, sources: Option<std::sync::Arc<pd_filter::sources::Registry>>) -> Self { DbHooks { db, sources, cache: Default::default() } } }
impl Hooks for DbHooks {
    fn store_url(&self, appid: i64, kind: pd_filter::artclient::Kind) -> Option<String> {
        if appid > 0 { return None; }
        let sources = self.sources.as_ref()?;
        if let Some(found) = self.cache.lock().ok().and_then(|c| c.get(&appid).cloned()) { return found.into_iter().find(|(k, _)| k == kind.name()).map(|(_, u)| u); }
        let c = rusqlite::Connection::open_with_flags(&self.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
        let (name, platform, platform_id, slug, appname): (String, String, String, String, String) = c.query_row("SELECT COALESCE(name,''), COALESCE(platform,''), COALESCE(platform_id,''), COALESCE(platform_slug,''), COALESCE(platform_appname,'') FROM games WHERE appid = ?", [appid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).ok()?;
        let src = sources.for_game(&platform, &platform_id)?;
        if !src.caps().art { return None; }
        let urls = src.art_urls(&pd_filter::sources::GameRef { appid, name, platform, platform_id, slug, appname, ..Default::default() });
        if let Ok(mut cache) = self.cache.lock() { cache.insert(appid, urls.clone()); }
        urls.into_iter().find(|(k, _)| k == kind.name()).map(|(_, u)| u)
    }
    fn steam_copy_of(&self, appid: i64) -> Option<(i64, String)> {
        if appid > 0 { return None; }
        let c = rusqlite::Connection::open_with_flags(&self.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
        let (dup, sid): (Option<String>, Option<i64>) = c.query_row("SELECT duplicate_of, steam_appid FROM games WHERE appid = ?", [appid], |r| Ok((r.get(0)?, r.get(1)?))).ok()?;
        let cands: Vec<i64> = [dup.and_then(|d| d.trim().parse::<i64>().ok()), sid].into_iter().flatten().filter(|i| *i > 0).collect();
        for s in &cands {
            if let Ok(ih) = c.query_row("SELECT COALESCE(icon_hash,'') FROM games WHERE appid = ? AND platform = 'steam'", [s], |r| r.get::<_, String>(0)) { return Some((*s, ih)); }
        }
        cands.first().map(|s| (*s, String::new()))
    }
}

/// Fill in the stored pixel size of covers saved before it was recorded (the Cover Quality filter reads it): only the file header is read, so thousands of covers
/// take a second or two. A game whose file is missing or unreadable keeps NULL and is tried again next start. Returns how many were filled in.
pub fn backfill_art_dims(db: &std::path::Path, root: &std::path::Path) -> usize {
    let Ok(mut c) = rusqlite::Connection::open(db) else { return 0 };
    let _ = c.busy_timeout(std::time::Duration::from_secs(10));
    let mut done = 0;
    for (dir, src, w, h) in [("vertical", "vertical_art_source", "vertical_art_w", "vertical_art_h"), ("horizontal", "horizontal_art_source", "horizontal_art_w", "horizontal_art_h")] {
        let ids: Vec<i64> = c.prepare(&format!("SELECT appid FROM games WHERE {w} IS NULL AND {src} IS NOT NULL AND {src} != 'missing'"))
            .and_then(|mut s| s.query_map([], |r| r.get(0)).map(|it| it.flatten().collect())).unwrap_or_default();
        if ids.is_empty() { continue; }
        let Ok(tx) = c.transaction() else { continue };
        for id in ids {
            let path = root.join("static/img/library").join(dir).join(format!("{id}.jpg"));
            if let Ok((pw, ph)) = image::image_dimensions(&path) {
                if tx.execute(&format!("UPDATE games SET {w} = ?, {h} = ? WHERE appid = ?"), rusqlite::params![pw as i64, ph as i64, id]).is_ok() { done += 1; }
            }
        }
        let _ = tx.commit();
    }
    done
}

#[cfg(test)]
mod dims_tests {
    use super::*;

    #[test]
    fn backfill_records_sizes_once_and_save_records_new_ones() {
        let root = std::env::temp_dir().join(format!("zest-dims-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("static/img/library/vertical")).unwrap();
        std::fs::create_dir_all(root.join("static/img/library/horizontal")).unwrap();
        let jpg = |w: u32, h: u32| { let mut b = Vec::new(); image::DynamicImage::ImageRgb8(image::RgbImage::new(w, h)).write_to(&mut std::io::Cursor::new(&mut b), image::ImageFormat::Jpeg).unwrap(); b };
        std::fs::write(root.join("static/img/library/vertical/1.jpg"), jpg(200, 300)).unwrap();
        std::fs::write(root.join("static/img/library/horizontal/1.jpg"), jpg(460, 215)).unwrap();
        let db = root.join("games.db");
        let c = rusqlite::Connection::open(&db).unwrap();
        c.execute_batch("CREATE TABLE games (appid INTEGER, vertical_art_source TEXT, horizontal_art_source TEXT, vertical_art_w INTEGER, vertical_art_h INTEGER, horizontal_art_w INTEGER, horizontal_art_h INTEGER, vertical_art_q INTEGER, horizontal_art_q INTEGER, edge_vertical TEXT, edge_horizontal TEXT);
            INSERT INTO games (appid, vertical_art_source, horizontal_art_source) VALUES (1, 'capsule', 'header');
            INSERT INTO games (appid, vertical_art_source, horizontal_art_source) VALUES (2, 'capsule', 'missing');
            INSERT INTO games (appid, vertical_art_source, horizontal_art_source) VALUES (3, NULL, NULL);").unwrap();
        drop(c);
        assert_eq!(backfill_art_dims(&db, &root), 2);   // game 1, both covers; game 2's file is missing, game 3 has none
        assert_eq!(backfill_art_dims(&db, &root), 0);   // nothing left that can be filled in... except game 2's missing vertical file, which stays NULL
        let c = rusqlite::Connection::open(&db).unwrap();
        let row = |id: i64| c.query_row("SELECT vertical_art_w, vertical_art_h, horizontal_art_w, horizontal_art_h FROM games WHERE appid = ?", [id], |r| Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<i64>>(1)?, r.get::<_, Option<i64>>(2)?, r.get::<_, Option<i64>>(3)?))).unwrap();
        assert_eq!(row(1), (Some(200), Some(300), Some(460), Some(215)));
        assert_eq!(row(2), (None, None, None, None));
        // a cover saved through the store records its size, and clearing it forgets the size
        let store = FsStore { root: root.clone(), db: db.clone(), stage: false };
        assert!(store.save_q(Kind::Vertical, 2, &jpg(640, 960), 95));
        assert_eq!(row(2).0, Some(640));
        assert_eq!(row(2).1, Some(960));
        // the JPEG quality: filled in for the old covers (the test JPEGs are the encoder's default, 75) and recorded when a cover is saved
        let quality = |id: i64| c.query_row("SELECT vertical_art_q, horizontal_art_q FROM games WHERE appid = ?", [id], |r| Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<i64>>(1)?))).unwrap();
        assert_eq!(quality(2), (Some(75), None));   // saved through the store above (a plain JPEG is kept as it arrived)
        // a PNG has to be converted: it is written at CONVERT_QUALITY, never at PlayDate's 95
        let png = { let mut b = Vec::new(); image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(80, 120, |x, y| image::Rgb([x as u8 * 3, y as u8 * 2, 50]))).write_to(&mut std::io::Cursor::new(&mut b), image::ImageFormat::Png).unwrap(); b };
        assert!(<FsStore as pd_filter::artclient::ArtStore>::save(&store, Kind::Horizontal, 2, &png));
        assert_eq!(quality(2).1, Some(100));
        assert_eq!(backfill_art_quality(&db, &root), 2);   // game 1, both covers
        assert_eq!(backfill_art_quality(&db, &root), 0);
        assert_eq!(quality(1), (Some(75), Some(75)));
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// `list` without the games that have a cover the user set themselves (source `custom`) in any of the chosen types: the bulk art job on Auto would replace it.
pub fn without_custom(db: &std::path::Path, list: &[i64], kinds: &[pd_filter::artclient::Kind]) -> Vec<i64> {
    use pd_filter::artclient::Kind;
    let Ok(c) = rusqlite::Connection::open(db) else { return list.to_vec() };
    let cols: Vec<&str> = kinds.iter().map(|k| match k { Kind::Vertical => "vertical_art_source", Kind::Horizontal => "horizontal_art_source", Kind::Icon => "icon_source" }).collect();
    if cols.is_empty() { return list.to_vec(); }
    let cond = cols.iter().map(|c| format!("{c} = 'custom'")).collect::<Vec<_>>().join(" OR ");
    let Ok(mut q) = c.prepare(&format!("SELECT 1 FROM games WHERE appid = ? AND ({cond})")) else { return list.to_vec() };
    list.iter().copied().filter(|a| !q.exists([a]).unwrap_or(false)).collect()
}

#[cfg(test)]
mod keep_custom_tests {
    use super::*;
    #[test]
    fn games_with_a_custom_cover_in_a_chosen_type_are_left_out() {
        let dir = std::env::temp_dir().join(format!("zest-keep-{}", std::process::id())); let _ = std::fs::remove_dir_all(&dir); std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("g.db");
        let c = rusqlite::Connection::open(&db).unwrap();
        c.execute_batch("CREATE TABLE games (appid INTEGER, vertical_art_source TEXT, horizontal_art_source TEXT, icon_source TEXT);
            INSERT INTO games VALUES (1, 'custom', 'header', NULL), (2, 'capsule', 'custom', NULL), (3, 'capsule', 'header', 'steam'), (4, NULL, NULL, NULL);").unwrap();
        drop(c);
        use pd_filter::artclient::Kind;
        assert_eq!(without_custom(&db, &[1, 2, 3, 4], &[Kind::Vertical]), vec![2, 3, 4]);
        assert_eq!(without_custom(&db, &[1, 2, 3, 4], &[Kind::Vertical, Kind::Horizontal]), vec![3, 4]);
        assert_eq!(without_custom(&db, &[1, 2, 3, 4], &[Kind::Icon]), vec![1, 2, 3, 4]);
        assert_eq!(without_custom(&db, &[1, 2, 3, 4], &[]), vec![1, 2, 3, 4]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// The JPEG quality a cover file was saved at (its first quantisation table; only the start of the file is read).
fn file_quality(path: &std::path::Path) -> Option<u8> {
    use std::io::Read;
    let mut head = Vec::with_capacity(65536);
    std::fs::File::open(path).ok()?.take(65536).read_to_end(&mut head).ok()?;
    art::jpeg_quality(&head)
}

/// Fill in the estimated JPEG quality of covers saved before it was recorded (the Cover Quality filter's "quality 95" reads it); a few KB of each file are read.
/// A cover whose file is missing or is not a plain JPEG keeps NULL and is tried again next start. Returns how many were filled in.
pub fn backfill_art_quality(db: &std::path::Path, root: &std::path::Path) -> usize {
    let Ok(mut c) = rusqlite::Connection::open(db) else { return 0 };
    let _ = c.busy_timeout(std::time::Duration::from_secs(10));
    let mut done = 0;
    for (dir, src, q) in [("vertical", "vertical_art_source", "vertical_art_q"), ("horizontal", "horizontal_art_source", "horizontal_art_q")] {
        let ids: Vec<i64> = c.prepare(&format!("SELECT appid FROM games WHERE {q} IS NULL AND {src} IS NOT NULL AND {src} != 'missing'"))
            .and_then(|mut s| s.query_map([], |r| r.get(0)).map(|it| it.flatten().collect())).unwrap_or_default();
        if ids.is_empty() { continue; }
        let Ok(tx) = c.transaction() else { continue };
        for id in ids {
            if let Some(v) = file_quality(&root.join("static/img/library").join(dir).join(format!("{id}.jpg"))) {
                if tx.execute(&format!("UPDATE games SET {q} = ? WHERE appid = ?"), rusqlite::params![v as i64, id]).is_ok() { done += 1; }
            }
        }
        let _ = tx.commit();
    }
    done
}

#[cfg(test)]
mod staging_tests {
    use super::*;
    fn jpg(w: u32, h: u32) -> Vec<u8> { let mut b = Vec::new(); image::DynamicImage::ImageRgb8(image::RgbImage::new(w, h)).write_to(&mut std::io::Cursor::new(&mut b), image::ImageFormat::Jpeg).unwrap(); b }
    fn setup(tag: &str) -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("zest-stage-{tag}-{}", std::process::id())); let _ = std::fs::remove_dir_all(&root); std::fs::create_dir_all(root.join("static/img/library/vertical")).unwrap();
        let db = root.join("games_1.db"); pd_filter::schema::init_db(&db).unwrap();
        rusqlite::Connection::open(&db).unwrap().execute("INSERT INTO games (appid, name, vertical_art_source, art_fetched) VALUES (7, 'Seven', 'capsule', '0')", []).unwrap();
        std::fs::write(library_file(&root, Kind::Vertical, 7), jpg(200, 300)).unwrap();
        (root, db)
    }
    #[test] fn a_staged_picture_touches_nothing_until_it_is_committed() {
        let (root, db) = setup("a");
        let stage = FsStore { root: root.clone(), db: db.clone(), stage: true };
        assert!(stage.save_q(Kind::Vertical, 7, &jpg(400, 600), 100)); stage_note(&root, Kind::Vertical, 7, "sgdb_grid", true);
        assert_eq!(image::image_dimensions(library_file(&root, Kind::Vertical, 7)).unwrap(), (200, 300), "the library still has the old cover");
        assert_eq!(current_file(&root, Kind::Vertical, 7), Some(staged_file(&root, Kind::Vertical, 7)), "the dialog shows the staged one");
        assert_eq!(staged(&root, 7), vec![Staged { kind: Kind::Vertical, source: "sgdb_grid".into(), order: true, cleared: false }]);
        let src = |c: &rusqlite::Connection| c.query_row("SELECT vertical_art_source, art_fetched FROM games WHERE appid = 7", [], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).unwrap();
        assert_eq!(src(&rusqlite::Connection::open(&db).unwrap()), ("capsule".into(), "0".into()), "and so does the database");
        let done = commit(&root, &db, 7, "2026-11-22").unwrap(); assert_eq!(done.len(), 1);
        assert_eq!(image::image_dimensions(library_file(&root, Kind::Vertical, 7)).unwrap(), (400, 600));
        assert_eq!(src(&rusqlite::Connection::open(&db).unwrap()), ("sgdb_grid".into(), "2026-11-22".into()), "the source and the fetch date follow");
        assert!(staged(&root, 7).is_empty() && !staged_file(&root, Kind::Vertical, 7).exists(), "nothing stays staged");
    }
    #[test] fn closing_drops_what_was_staged_and_a_staged_clear_waits_for_the_save() {
        let (root, db) = setup("b");
        let stage = FsStore { root: root.clone(), db: db.clone(), stage: true };
        assert!(stage.save_q(Kind::Vertical, 7, &jpg(400, 600), 100)); stage_note(&root, Kind::Vertical, 7, "custom", false);
        discard(&root, 7); assert_eq!(current_file(&root, Kind::Vertical, 7), Some(library_file(&root, Kind::Vertical, 7)), "the old cover is back");
        stage_clear(&root, Kind::Vertical, 7);
        assert!(library_file(&root, Kind::Vertical, 7).is_file(), "a staged clear removes nothing yet"); assert_eq!(current_file(&root, Kind::Vertical, 7), None, "but the dialog shows none");
        assert_eq!(staged(&root, 7), vec![Staged { kind: Kind::Vertical, source: String::new(), order: false, cleared: true }]);
        let done = commit(&root, &db, 7, "2026-11-22").unwrap(); assert!(done[0].cleared, "the caller does the library's own clear for it");
        assert!(library_file(&root, Kind::Vertical, 7).is_file(), "commit itself leaves the file for that clear");
    }
    #[test] fn a_crop_of_a_staged_picture_continues_from_it_and_a_crop_of_the_library_one_stages() {
        let (root, db) = setup("c");
        let stage = FsStore { root: root.clone(), db: db.clone(), stage: true };
        stage.crop(Kind::Vertical, 7, 0.0, 0.0, 0.5).unwrap();   // from the library's 200x300 (nothing staged yet)
        assert!(staged_file(&root, Kind::Vertical, 7).is_file()); assert_eq!(image::image_dimensions(library_file(&root, Kind::Vertical, 7)).unwrap(), (200, 300));
        let first = image::image_dimensions(staged_file(&root, Kind::Vertical, 7)).unwrap();
        stage.crop(Kind::Vertical, 7, 0.0, 0.0, 0.5).unwrap();   // now from the staged one
        let second = image::image_dimensions(staged_file(&root, Kind::Vertical, 7)).unwrap(); assert!(second.0 < first.0, "{first:?} then {second:?}");
        assert_eq!(staged(&root, 7)[0].source, "custom");
    }
}

#[cfg(test)]
mod staged_download_tests {
    use super::*;
    use pd_filter::net::{Http, NetError, Response};
    struct Fake(Vec<u8>);
    impl Http for Fake {
        fn get(&self, _u: &str, _h: &[(&str, &str)], _t: std::time::Duration) -> Result<Response, NetError> { Ok(Response { status: 404, retry_after: None, body: String::new() }) }
        fn get_bytes(&self, _u: &str, _h: &[(&str, &str)], _t: std::time::Duration) -> Result<(u16, Vec<u8>), NetError> { Ok((200, self.0.clone())) }
    }
    #[test] fn an_automatic_order_download_lands_in_the_staging_folder_and_not_the_library() {
        let root = std::env::temp_dir().join(format!("zest-stagedl-{}", std::process::id())); let _ = std::fs::remove_dir_all(&root); std::fs::create_dir_all(root.join("static/img/library/vertical")).unwrap();
        let db = root.join("games_1.db"); pd_filter::schema::init_db(&db).unwrap();
        let mut b = Vec::new(); image::DynamicImage::ImageRgb8(image::RgbImage::new(600, 900)).write_to(&mut std::io::Cursor::new(&mut b), image::ImageFormat::Jpeg).unwrap();
        let store = std::sync::Arc::new(FsStore { root: root.clone(), db: db.clone(), stage: true });
        let art = pd_filter::artclient::Art::new(std::sync::Arc::new(Fake(b)), store, std::sync::Arc::new(DbHooks::new(db.clone(), None)), None);
        let order = vec!["steam".to_string(), "sgdb".to_string()];
        let tag = art.download_art(Kind::Vertical, 620, None, "auto", Some(&order), true, None, Some("Portal 2"), "");
        assert_ne!(tag, "missing", "the download itself works");
        assert!(staged_file(&root, Kind::Vertical, 620).is_file(), "the picture is in the staging folder");
        assert!(!library_file(&root, Kind::Vertical, 620).exists(), "and not in the library");
    }
}
