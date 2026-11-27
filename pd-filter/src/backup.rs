//! Backup, restore and CSV export (PlayDate's `backup.py`), in PlayDate's own file layout so a backup from either program restores in the other: `config.json`, `state.json`,
//! `theme.json`, `emulators.json`, `santa_gifts.json`, every `games_*.db` (a consistent copy made with `VACUUM INTO`), the per-account `group_sources_*.json` and
//! `steamgifts_wins_*.json`, the badge icons and background image, and with art every cover under `static/img/library`. Covers are stored uncompressed (JPEG does not shrink), the
//! rest deflated. A restored database is integrity-checked first and written through SQLite's backup API, so a library that is open is replaced cleanly, write-ahead log included.
use crate::net::Cancel;
use crate::zipwrite::ZipOut;
use rusqlite::types::Value as Sql;
use rusqlite::Connection;
use std::path::{Path, PathBuf};

const CORE: [&str; 5] = ["config.json", "state.json", "theme.json", "emulators.json", "santa_gifts.json"];

fn files_like(dir: &Path, prefix: &str, suffix: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path())
        .filter(|p| p.is_file() && p.file_name().map_or(false, |n| { let n = n.to_string_lossy(); n.starts_with(prefix) && n.ends_with(suffix) })).collect();
    v.sort(); v
}
fn walk_jpg(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() { walk_jpg(&p, out); } else if p.extension().map_or(false, |x| x.to_string_lossy().eq_ignore_ascii_case("jpg")) { out.push(p); }
    }
}
fn arc(root: &Path, p: &Path) -> String { p.strip_prefix(root).unwrap_or(p).components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect::<Vec<_>>().join("/") }

/// Writes a backup of the data folder to `dest` (written beside it and renamed, so a half-written file never has the final name). `progress(done, total)` counts entries.
/// Returns how many entries went in.
/// `sign_ins`: store sign-ins go into the backup's `config.json` (unencrypted); without it they are left out. Typed API keys are always included.
pub fn create(root: &Path, dest: &Path, include_art: bool, sign_ins: bool, progress: &mut dyn FnMut(usize, usize), cancel: &Cancel) -> Result<usize, String> {
    let tmp = dest.with_extension("zip.partial");
    let result = (|| -> Result<usize, String> {
        let f = std::io::BufWriter::new(std::fs::File::create(&tmp).map_err(|e| format!("Could not write {}: {e}", tmp.display()))?);
        let mut z = ZipOut::new(f);
        let mut plain: Vec<(String, PathBuf, bool)> = vec![];
        for n in CORE { let p = root.join(n); if p.is_file() { plain.push((n.to_string(), p, true)); } }
        let dbs = files_like(root, "games_", ".db");
        for p in files_like(root, "group_sources_", ".json").into_iter().chain(files_like(root, "steamgifts_wins_", ".json")) { plain.push((arc(root, &p), p, true)); }
        let badges = root.join("static/img/badges");
        for p in files_like(&badges, "", "").into_iter().filter(|p| p.extension().map_or(false, |x| x.to_string_lossy().eq_ignore_ascii_case("png"))) { plain.push((arc(root, &p), p, false)); }
        let bg = root.join("static/img/backgrounds/background.jpg"); if bg.is_file() { plain.push(("static/img/backgrounds/background.jpg".into(), bg, false)); }
        let mut art = vec![];
        if include_art { walk_jpg(&root.join("static/img/library"), &mut art); art.sort(); }
        let total = plain.len() + dbs.len() + art.len();
        let mut done = 0usize;
        let check = |cancel: &Cancel| if cancel.is_set() { Err("Cancelled".to_string()) } else { Ok(()) };
        for (name, p, deflate) in &plain {
            check(cancel)?;
            // config.json is written as a backup should hold it: keys from the keyring put back, sign-ins only when asked for
            let cfg = (name == "config.json").then(|| crate::account::config_for_backup(root, sign_ins)).flatten().and_then(|v| serde_json::to_vec_pretty(&v).ok());
            match cfg { Some(b) => z.add_bytes(name, &b, *deflate), None => z.add_file(name, p, *deflate) }.map_err(|e| format!("{name}: {e}"))?;
            done += 1; progress(done, total);
        }
        for db in &dbs {
            check(cancel)?;
            let name = db.file_name().unwrap_or_default().to_string_lossy().to_string();
            // a consistent copy made inside its own read transaction; VACUUM INTO wants a target that does not exist yet
            let dir = std::env::temp_dir().join(format!("zest-backup-{}-{}", std::process::id(), done));
            let _ = std::fs::remove_dir_all(&dir); std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let copy = dir.join("vacuum.db");
            let r = (|| -> Result<(), String> {
                let c = Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE).map_err(|e| e.to_string())?;
                c.busy_timeout(std::time::Duration::from_secs(30)).ok();
                c.execute("VACUUM INTO ?1", [copy.to_string_lossy().to_string()]).map_err(|e| e.to_string())?;
                z.add_file(&name, &copy, true).map_err(|e| format!("{name}: {e}"))
            })();
            let _ = std::fs::remove_dir_all(&dir);
            r?; done += 1; progress(done, total);
        }
        for p in &art { check(cancel)?; z.add_file(&arc(root, p), p, false).map_err(|e| format!("{}: {e}", p.display()))?; done += 1; if done % 50 == 0 { progress(done, total); } }
        let n = z.count();
        z.finish().and_then(|mut w| { use std::io::Write; w.flush()?; w.into_inner().map_err(|e| e.into_error())?.sync_all() }).map_err(|e| e.to_string())?;
        progress(total, total);
        Ok(n)
    })();
    match result {
        Ok(n) => { std::fs::rename(&tmp, dest).map_err(|e| format!("Could not finish the backup: {e}"))?; Ok(n) }
        Err(e) => { let _ = std::fs::remove_file(&tmp); Err(e) }
    }
}

/// What a restore wrote and what it left out.
#[derive(Debug, Default, PartialEq)]
pub struct Restored { pub restored: Vec<String>, pub skipped: Vec<String> }

/// `_write_database`: the backed-up database is checked, then put at `dest` (an open library is replaced through SQLite's backup API).
pub fn write_database(from: &Path, dest: &Path) -> Result<(), String> {
    let src = Connection::open(from).map_err(|e| e.to_string())?;
    let verdict: String = src.query_row("PRAGMA integrity_check", [], |r| r.get(0)).map_err(|e| e.to_string())?;
    if verdict != "ok" { return Err(format!("the backed-up database failed its integrity check ({verdict})")); }
    if !dest.exists() { drop(src); return std::fs::rename(from, dest).or_else(|_| std::fs::copy(from, dest).map(|_| ())).map_err(|e| e.to_string()); }
    let mut dst = Connection::open(dest).map_err(|e| e.to_string())?;
    dst.busy_timeout(std::time::Duration::from_secs(30)).ok();
    {
        let b = rusqlite::backup::Backup::new(&src, &mut dst).map_err(|e| e.to_string())?;
        b.run_to_completion(256, std::time::Duration::from_millis(5), None).map_err(|e| format!("the library is in use and could not be replaced ({e}); close Zest and PlayDate and restore again"))?;
    }
    let _ = dst.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
    Ok(())
}

/// `_sanitize_restored_state`: window geometry, renderer choice and cached update links belong to the machine the backup came from.
pub fn sanitize_state(root: &Path) -> Vec<String> {
    let mut dropped = vec![];
    for (file, keys) in [("state.json", &["window_state", "fullscreen", "renderer", "flatpak_url_other_variant"][..]), ("config.json", &["qt_renderer_hint_seen"][..])] {
        let p = root.join(file);
        let Some(mut v) = std::fs::read_to_string(&p).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()) else { continue };
        let Some(o) = v.as_object_mut() else { continue };
        let before = o.len();
        for k in keys { if o.remove(*k).is_some() { dropped.push(format!("{file}:{k}")); } }
        if o.len() != before { let _ = crate::account::write_json(&p, &v); }
    }
    dropped
}

/// Restores a backup into the data folder. Only the files a backup holds are written, each to its own place; a name that would leave the folder is skipped.
/// The caller restarts the app afterwards (an open library has been replaced under it).
pub fn restore(root: &Path, zip: &Path, progress: &mut dyn FnMut(usize, usize), cancel: &Cancel) -> Result<Restored, String> {
    let names = crate::archive::zip_names(zip).map_err(|e| if e.contains("not a zip") { "That is not a backup file (not a zip archive).".to_string() } else { e })?;
    let has = |n: &str| names.iter().any(|x| x == n);
    if !CORE.iter().any(|c| has(c)) && !names.iter().any(|n| n.starts_with("games_") && n.ends_with(".db")) { return Err("That zip does not look like a PlayDate or Zest backup.".into()); }
    let safe = |n: &str| crate::archive::safe_rel(n).map(|r| root.join(r));
    let mut out = Restored::default();
    let mut zr = crate::archive::ZipReader::open(zip)?;
    let total = names.len().max(1);
    let mut done = 0usize;
    for n in CORE { if has(n) { if let Some(d) = safe(n) { zr.extract(n, &d)?; out.restored.push(n.into()); } } else { out.skipped.push(n.into()); } done += 1; progress(done, total); }
    for n in names.iter().filter(|n| n.starts_with("games_") && n.ends_with(".db") && !n.contains('/')) {
        if cancel.is_set() { return Err("Cancelled".into()); }
        let Some(d) = safe(n) else { continue };
        let tmp = d.with_extension("db.restore");
        zr.extract(n, &tmp)?;
        match write_database(&tmp, &d) { Ok(()) => out.restored.push(n.clone()), Err(e) => { crate::net::log(&format!("restore: could not restore {n}: {e}")); out.skipped.push(n.clone()); } }
        let _ = std::fs::remove_file(&tmp); done += 1; progress(done, total);
    }
    for n in names.iter().filter(|n| !n.contains('/') && n.ends_with(".json") && (n.starts_with("group_sources_") || n.starts_with("steamgifts_wins_"))) {
        if let Some(d) = safe(n) { zr.extract(n, &d)?; out.restored.push(n.clone()); }
    }
    let (mut art, mut imgs) = (0usize, 0usize);
    for n in &names {
        if cancel.is_set() { return Err("Cancelled".into()); }
        let cover = n.starts_with("static/img/library/") && n.ends_with(".jpg");
        let img = (n.starts_with("static/img/badges/") && n.ends_with(".png")) || n == "static/img/backgrounds/background.jpg";
        if !cover && !img { continue; }
        let Some(d) = safe(n) else { continue };
        zr.extract(n, &d)?;
        if cover { art += 1 } else { imgs += 1 }
        done += 1; if done % 50 == 0 { progress(done.min(total), total); }
    }
    if art > 0 { out.restored.push(format!("{art} cover image(s)")); }
    if imgs > 0 { out.restored.push(format!("{imgs} badge/background image(s)")); }
    let dropped = sanitize_state(root);
    if !dropped.is_empty() { crate::net::log(&format!("restore: dropped machine-specific keys {dropped:?}")); }
    progress(total, total);
    Ok(out)
}

// ---------------------------------------------------------------- CSV export

fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719468; let era = z.div_euclid(146097); let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; let y = yoe + era * 400; let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153; let d = doy - (153 * mp + 2) / 5 + 1; let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}
/// `ts_to_date`: a unix time as `YYYY-MM-DD` (UTC); a stored `YYYY-MM-DD...` string as it is; nothing for 0 or NULL.
pub fn ts_to_date(v: &Sql) -> String {
    match v {
        Sql::Integer(0) | Sql::Null => String::new(),
        Sql::Integer(t) => { let (y, m, d) = civil(t.div_euclid(86400)); format!("{y:04}-{m:02}-{d:02}") }
        Sql::Real(t) if *t != 0.0 => { let (y, m, d) = civil((*t as i64).div_euclid(86400)); format!("{y:04}-{m:02}-{d:02}") }
        Sql::Text(s) if s.len() >= 10 && s.as_bytes()[4] == b'-' => s[..10].to_string(),
        Sql::Text(s) => s.parse::<i64>().map(|t| ts_to_date(&Sql::Integer(t))).unwrap_or_default(),
        _ => String::new(),
    }
}
fn field(s: &str) -> String { if s.contains([',', '"', '\r', '\n']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() } }
fn txt(v: &Sql) -> String { match v { Sql::Text(s) => s.clone(), Sql::Integer(i) => i.to_string(), Sql::Real(r) => format!("{r:?}"), _ => String::new() } }
fn py_round1(x: f64) -> String { format!("{:?}", (x * 10.0).round() / 10.0) }

pub const CSV_HEADERS: [&str; 17] = ["Name", "AppID", "Completion Status", "Tags", "Groups", "Playtime (hrs)", "Last Played", "Date Added", "Installed", "Review Score", "Review %", "Developers", "Publishers", "Release Date",
    "Achievements Unlocked", "Achievements Total", "Achievement %"];

/// `_build_csv_rows` + the file writer: every game (or those the `inner` query selects: a `SELECT * FROM games ...` and its parameters) as CSV, with a byte order mark so Excel reads it as UTF-8.
pub fn csv_text(conn: &Connection, inner: Option<(&str, &[Sql])>) -> Result<String, String> { csv_text_cols(conn, inner, &[true; 17]) }
/// The same with only the columns `keep` marks (in `CSV_HEADERS` order); none marked means all, as in PlayDate.
pub fn csv_text_cols(conn: &Connection, inner: Option<(&str, &[Sql])>, keep: &[bool; 17]) -> Result<String, String> {
    let keep: [bool; 17] = if keep.iter().any(|k| *k) { *keep } else { [true; 17] };
    let pick = |v: Vec<String>| -> Vec<String> { v.into_iter().zip(keep.iter()).filter(|(_, k)| **k).map(|(c, _)| c).collect() };
    let cols = "name, appid, completion_status, tags, groups, playtime_forever, last_played, date_added, installed, review_score, review_percentage, developers, publishers, release_date, unlocked_achievements, total_achievements";
    let (sql, params): (String, Vec<Sql>) = match inner { Some((q, p)) => (format!("SELECT {cols} FROM ({q}) ORDER BY name COLLATE NOCASE ASC"), p.to_vec()), None => (format!("SELECT {cols} FROM games ORDER BY name COLLATE NOCASE ASC"), vec![]) };
    let mut st = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let mut out = String::from("\u{feff}");
    out.push_str(&pick(CSV_HEADERS.iter().map(|s| s.to_string()).collect()).join(",")); out.push_str("\r\n");
    let rows = st.query_map(rusqlite::params_from_iter(params.iter()), |r| (0..16).map(|i| r.get::<_, Sql>(i)).collect::<Result<Vec<Sql>, _>>()).map_err(|e| e.to_string())?;
    for row in rows {
        let r = row.map_err(|e| e.to_string())?;
        let num = |v: &Sql| match v { Sql::Integer(i) => *i as f64, Sql::Real(f) => *f, Sql::Text(s) => s.parse().unwrap_or(0.0), _ => 0.0 };
        let mins = num(&r[5]);
        let hrs = if mins == 0.0 { "0".to_string() } else { py_round1(mins / 60.0) };
        let (unlocked, total) = (num(&r[14]), num(&r[15]));
        let pct = if total != 0.0 { format!("{}%", py_round1(unlocked / total * 100.0)) } else { String::new() };
        let cells = vec![txt(&r[0]), txt(&r[1]), txt(&r[2]), txt(&r[3]), txt(&r[4]), hrs, ts_to_date(&r[6]), ts_to_date(&r[7]), (if num(&r[8]) != 0.0 { "Yes" } else { "No" }).to_string(), txt(&r[9]),
            if matches!(r[10], Sql::Null) { String::new() } else { txt(&r[10]) }, txt(&r[11]), txt(&r[12]), ts_to_date(&r[13]), (unlocked as i64).to_string(), (total as i64).to_string(), pct];
        out.push_str(&pick(cells).iter().map(|c| field(c)).collect::<Vec<_>>().join(",")); out.push_str("\r\n");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tmp(tag: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("zest_bk_{tag}_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d }
    fn library(root: &Path, name: &str) {
        let c = Connection::open(root.join(name)).unwrap();
        c.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE games (appid INTEGER PRIMARY KEY, name TEXT, completion_status TEXT, tags TEXT, groups TEXT, playtime_forever INTEGER, last_played INTEGER, date_added INTEGER, installed INTEGER,
            review_score TEXT, review_percentage INTEGER, developers TEXT, publishers TEXT, release_date INTEGER, unlocked_achievements INTEGER, total_achievements INTEGER);
            INSERT INTO games VALUES (1, 'Zed, the \"Game\"', 'Beaten', 'A,B', 'g', 150, 1700000000, 0, 1, 'Very Positive', 91, 'Dev', 'Pub', 1577923200, 5, 10),
                                     (2, 'alpha', NULL, NULL, NULL, 0, NULL, 1600000000, 0, NULL, NULL, NULL, NULL, NULL, 0, 0);").unwrap();
    }
    #[test] fn csv_matches_what_the_python_export_writes() {
        let d = tmp("csv"); library(&d, "games_1.db");
        let c = Connection::open(d.join("games_1.db")).unwrap();
        let t = csv_text(&c, None).unwrap();
        assert!(t.starts_with('\u{feff}'));
        let lines: Vec<&str> = t[3..].split("\r\n").collect();
        assert_eq!(lines[0], CSV_HEADERS.join(","));
        // sorted by name without case, text quoted and doubled, hours rounded to one place, dates in UTC, empty cells for nothing
        assert_eq!(lines[1], "alpha,2,,,,0,,2020-09-13,No,,,,,,0,0,");
        assert_eq!(lines[2], "\"Zed, the \"\"Game\"\"\",1,Beaten,\"A,B\",g,2.5,2023-11-14,1970-01-01,Yes,Very Positive,91,Dev,Pub,2020-01-02,5,10,50.0%".replace("1970-01-01", ""));
        let some = csv_text(&c, Some(("SELECT * FROM games WHERE appid = ?", &[Sql::Integer(2)]))).unwrap();
        assert_eq!(some[3..].split("\r\n").filter(|l| !l.is_empty()).count(), 2);
    }
    #[test] fn dates_follow_python_ts_to_date() {
        assert_eq!(ts_to_date(&Sql::Integer(0)), ""); assert_eq!(ts_to_date(&Sql::Null), ""); assert_eq!(ts_to_date(&Sql::Integer(86399)), "1970-01-01"); assert_eq!(ts_to_date(&Sql::Integer(951782400)), "2000-02-29");
        assert_eq!(ts_to_date(&Sql::Text("2024-05-06 hello".into())), "2024-05-06"); assert_eq!(ts_to_date(&Sql::Text("1700000000".into())), "2023-11-14");
    }
    #[test] fn a_backup_round_trips_through_restore_and_keeps_the_library_consistent() {
        let a = tmp("a"); library(&a, "games_1.db");
        std::fs::write(a.join("config.json"), r#"{"k": 1}"#).unwrap(); std::fs::write(a.join("state.json"), r#"{"sort": "name", "window_state": {"w": 1}, "fullscreen": true}"#).unwrap();
        std::fs::write(a.join("steamgifts_wins_1.json"), "{}").unwrap(); std::fs::write(a.join("junk.txt"), "x").unwrap();
        std::fs::create_dir_all(a.join("static/img/library/vertical")).unwrap(); std::fs::write(a.join("static/img/library/vertical/1.jpg"), [0xffu8, 0xd8, 1, 2, 3]).unwrap();
        std::fs::create_dir_all(a.join("static/img/badges")).unwrap(); std::fs::write(a.join("static/img/badges/b.png"), [1u8, 2]).unwrap();
        let z = a.join("backup.zip");
        let n = create(&a, &z, true, false, &mut |_, _| {}, &Cancel::new()).unwrap();
        assert_eq!(n, 6, "config, state, db, wins, badge and one cover; the junk file stays out");
        assert!(!a.join("backup.zip.partial").exists() && !z.with_extension("zip.partial").exists());
        // without art the covers are left out
        let z2 = a.join("small.zip"); assert_eq!(create(&a, &z2, false, false, &mut |_, _| {}, &Cancel::new()).unwrap(), 5);
        // restore into another folder that already has a different library open
        let b = tmp("b"); library(&b, "games_1.db");
        let live = Connection::open(b.join("games_1.db")).unwrap(); live.execute("DELETE FROM games", []).unwrap();
        let r = restore(&b, &z, &mut |_, _| {}, &Cancel::new()).unwrap();
        assert!(r.restored.contains(&"games_1.db".to_string()) && r.restored.contains(&"1 cover image(s)".to_string()) && r.skipped.contains(&"theme.json".to_string()), "{r:?}");
        let c = Connection::open(b.join("games_1.db")).unwrap();
        assert_eq!(c.query_row("SELECT COUNT(*) FROM games", [], |r| r.get::<_, i64>(0)).unwrap(), 2, "the live database now holds the backed-up library");
        assert_eq!(live.query_row("SELECT COUNT(*) FROM games", [], |r| r.get::<_, i64>(0)).unwrap(), 2, "an open connection sees it too");
        assert_eq!(std::fs::read(b.join("static/img/library/vertical/1.jpg")).unwrap(), vec![0xffu8, 0xd8, 1, 2, 3]);
        let st: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(b.join("state.json")).unwrap()).unwrap();
        assert_eq!(st, serde_json::json!({"sort": "name"}), "machine-bound keys are dropped");
        assert!(!b.join("junk.txt").exists());
        // not a backup
        std::fs::write(b.join("x.zip"), b"nope").unwrap(); assert!(restore(&b, &b.join("x.zip"), &mut |_, _| {}, &Cancel::new()).is_err());
        let only = b.join("only.zip"); let mut zz = ZipOut::new(std::fs::File::create(&only).unwrap()); zz.add_bytes("readme.txt", b"hi", true).unwrap(); zz.finish().unwrap();
        assert!(restore(&b, &only, &mut |_, _| {}, &Cancel::new()).unwrap_err().contains("does not look like"));
    }
    #[test] fn a_zip_with_more_than_65534_entries_uses_zip64_and_reads_back() {
        let d = tmp("z64"); let z = d.join("big.zip");
        let mut w = ZipOut::new(std::io::BufWriter::new(std::fs::File::create(&z).unwrap()));
        for i in 0..70_000u32 { w.add_bytes(&format!("f/{i}.txt"), b"x", false).unwrap(); }
        w.finish().unwrap();
        let names = crate::archive::zip_names(&z).unwrap();
        assert_eq!(names.len(), 70_000); assert_eq!(names[69_999], "f/69999.txt");
        let out = d.join("one.txt"); assert_eq!(crate::archive::extract_entry(&z, "f/12345.txt", &out).unwrap(), 1); assert_eq!(std::fs::read(&out).unwrap(), b"x");
    }
    #[test] fn chosen_columns_only_and_none_means_all() {
        let d = tmp("cols"); library(&d, "games_1.db");
        let c = Connection::open(d.join("games_1.db")).unwrap();
        let mut keep = [false; 17]; keep[0] = true; keep[5] = true;
        let t = csv_text_cols(&c, None, &keep).unwrap();
        assert_eq!(t.lines().next().unwrap().trim_start_matches('\u{feff}'), "Name,Playtime (hrs)");
        assert_eq!(csv_text_cols(&c, None, &[false; 17]).unwrap(), csv_text(&c, None).unwrap());
    }
    #[test] fn a_backup_python_wrote_restores() {
        let d = tmp("py"); let z = d.join("py.zip");
        let py = format!("import zipfile; z = zipfile.ZipFile(r'{}', 'w'); z.writestr('config.json', '{{\"a\": 2}}'); z.writestr('static/img/library/vertical/9.jpg', b'JPG', compress_type=zipfile.ZIP_STORED); z.writestr('../evil.txt', 'x'); z.close()", z.display());
        if std::process::Command::new("python3").args(["-c", &py]).status().map_or(true, |s| !s.success()) { return; }   // no python here: nothing to check
        let root = d.join("data"); std::fs::create_dir_all(&root).unwrap();
        let r = restore(&root, &z, &mut |_, _| {}, &Cancel::new()).unwrap();
        assert!(r.restored.contains(&"config.json".to_string()));
        assert_eq!(std::fs::read(root.join("static/img/library/vertical/9.jpg")).unwrap(), b"JPG");
        assert!(!d.join("evil.txt").exists());
    }
}
