//! Bringing a PlayDate (Python) library into Zest on its first start. Zest is a separate app: it COPIES PlayDate's data into its own folder (PlayDate keeps its own, sign-ins
//! included, so going back stays possible and the two never write one database). What is copied is what a backup holds (`backup.rs`): the settings files, every
//! `games_*.db` (a consistent copy made with `VACUUM INTO`, also while PlayDate is open), the per-account group-source and SteamGifts files, badges, the background image and
//! every cover, plus the Humble cache. Logs, rollback snapshots, Python plugins and program files are not.
//!
//! Where PlayDate keeps its data: the Flatpaks' data folders (the GTK and Qt app IDs); on Windows the installer's folder (the data lives next to `PlayDate.exe`); a source install
//! anywhere, found through the shortcuts PlayDate's installer made (`.desktop` entries on Linux, the wrapper in `PlayDate.app` on a Mac).
use crate::net::Cancel;
use rusqlite::Connection;
use std::path::{Path, PathBuf};

/// Settings files copied as they are (the same list as a backup's, plus the Humble cache).
const FILES: [&str; 6] = ["config.json", "state.json", "theme.json", "emulators.json", "santa_gifts.json", "humble_cache.json"];

fn named(dir: &Path, prefix: &str, suffix: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path())
        .filter(|p| p.is_file() && p.file_name().map_or(false, |n| { let n = n.to_string_lossy(); n.starts_with(prefix) && n.ends_with(suffix) })).collect();
    v.sort(); v
}

/// Does `dir` hold a PlayDate library (a `games_*.db` beside its `config.json` or `state.json`)?
pub fn is_library(dir: &Path) -> bool {
    !named(dir, "games_", ".db").is_empty() && (dir.join("config.json").is_file() || dir.join("state.json").is_file())
}

/// Absolute paths in a shortcut or launcher script that point at PlayDate's own start files; their folder is the install.
fn install_dirs_in(text: &str) -> Vec<PathBuf> {
    let mut out = vec![];
    for line in text.lines() {
        for start in ["launch.sh", "playdate-launch.sh", "main.py"] {
            let mut from = 0;
            while let Some(i) = line[from..].find(start) {
                let end = from + i + start.len();
                // back to the start of the path: an opening quote, '=', or the line's start
                let head = &line[..from + i];
                let begin = head.rfind(['"', '\'', '=']).map(|j| j + 1).unwrap_or(0);
                let p = PathBuf::from(line[begin..end].trim());
                if p.is_absolute() { if let Some(d) = p.parent() { if !out.iter().any(|o: &PathBuf| o == d) { out.push(d.to_path_buf()); } } }
                from = end;
            }
        }
    }
    out
}

/// The folders PlayDate may keep a library in on this machine (only those that hold one), each once, most likely first. `home` is the user's home folder.
pub fn find(home: &Path) -> Vec<PathBuf> {
    let mut c: Vec<PathBuf> = vec![];
    for id in ["io.github.robbyratpoison.PlayDate", "io.github.robbyratpoison.PlayDate.Qt"] { c.push(home.join(".var/app").join(id).join("data/playdate")); }
    if cfg!(windows) {
        c.push(home.join("PlayDate"));
        if let Some(l) = std::env::var_os("LOCALAPPDATA") { c.push(PathBuf::from(&l).join("Programs/PlayDate")); c.push(PathBuf::from(l).join("PlayDate")); }
    }
    // a source install: the shortcuts its installer made say where it is
    let mut shortcuts: Vec<PathBuf> = vec![home.join("Desktop/PlayDate.desktop")];
    for d in [home.join(".local/share/applications")] { shortcuts.extend(named(&d, "", ".desktop").into_iter().filter(|p| p.file_name().map_or(false, |n| n.to_string_lossy().to_lowercase().contains("playdate")))); }
    for app in [home.join("Applications/PlayDate.app"), home.join("Desktop/PlayDate.app")] { shortcuts.extend(named(&app.join("Contents/MacOS"), "", "")); }
    for s in shortcuts { if let Ok(t) = std::fs::read_to_string(&s) { c.extend(install_dirs_in(&t)); } }
    // the usual places someone unpacks it
    for d in [home.join("PlayDate"), home.join("Applications/PlayDate"), home.join("Games/PlayDate")] { c.push(d); }
    let mut seen = std::collections::HashSet::new();
    c.into_iter().filter(|d| is_library(d)).filter(|d| seen.insert(std::fs::canonicalize(d).unwrap_or_else(|_| d.clone()))).collect()
}

/// The libraries in a PlayDate folder, for showing before the import: (Steam account id from the file name, games in it).
pub fn summary(dir: &Path) -> Vec<(String, i64)> {
    named(dir, "games_", ".db").into_iter().map(|p| {
        let id = p.file_stem().map(|s| s.to_string_lossy().trim_start_matches("games_").to_string()).unwrap_or_default();
        let n = Connection::open_with_flags(&p, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok().and_then(|c| c.query_row("SELECT COUNT(*) FROM games", [], |r| r.get(0)).ok()).unwrap_or(0);
        (id, n)
    }).collect()
}

/// A short name for a found PlayDate folder: "PlayDate Flatpak", "PlayDate Flatpak (Qt)", else the folder with the home folder as `~`.
pub fn label(dir: &Path, home: &Path) -> String {
    let s = dir.to_string_lossy();
    if s.contains("io.github.robbyratpoison.PlayDate.Qt/") { return "PlayDate Flatpak (Qt)".into(); }
    if s.contains("io.github.robbyratpoison.PlayDate/") { return "PlayDate Flatpak".into(); }
    match dir.strip_prefix(home) { Ok(r) => format!("~/{}", r.display()), Err(_) => dir.display().to_string() }
}

/// When the library in `dir` was last used: the newest change to its databases (their write-ahead logs included) or its settings, as Unix time.
pub fn last_used(dir: &Path) -> Option<i64> {
    let mut files = named(dir, "games_", ".db"); files.extend(named(dir, "games_", ".db-wal")); files.push(dir.join("state.json"));
    files.iter().filter_map(|p| std::fs::metadata(p).ok()?.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs() as i64)).max()
}

/// "today", "yesterday", "5 days ago", "3 months ago" for a time `t` seen at `now`.
pub fn ago(t: i64, now: i64) -> String {
    let d = (now - t).max(0) / 86400;
    match d { 0 => "today".into(), 1 => "yesterday".into(), 2..=59 => format!("{d} days ago"), 60..=729 => format!("{} months ago", d / 30), _ => format!("{} years ago", d / 365) }
}

fn covers(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() { covers(&p, out); } else if p.extension().map_or(false, |x| x.to_string_lossy().eq_ignore_ascii_case("jpg")) { out.push(p); }
    }
}

/// What an import copied.
#[derive(Debug, Default, PartialEq)]
pub struct Imported { pub libraries: usize, pub files: usize, pub covers: usize }

/// Copies PlayDate's library from `from` into the Zest data folder `to`. The databases go last, so a folder with a library in it always has its settings and covers too
/// (an import stopped half-way leaves no library, and the first-run window simply shows again). Refuses a folder that already holds a library, and `from` = `to`.
/// `progress(done, total)` counts files.
/// The `state.json` key the first start reads: a library copied from PlayDate is shown What's New, a fresh one the tutorial.
pub const FROM_PLAYDATE_KEY: &str = "zest_from_playdate";

fn mark_from_playdate(to: &Path) {
    let p = to.join("state.json");
    let mut v = std::fs::read_to_string(&p).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()).filter(|v| v.is_object()).unwrap_or_else(|| serde_json::json!({}));
    v[FROM_PLAYDATE_KEY] = serde_json::Value::Bool(true);
    let _ = crate::account::write_json(&p, &v);
}

pub fn import(from: &Path, to: &Path, progress: &mut dyn FnMut(usize, usize), cancel: &Cancel) -> Result<Imported, String> {
    if !is_library(from) { return Err(format!("{} does not hold a PlayDate library.", from.display())); }
    let same = std::fs::canonicalize(from).ok().zip(std::fs::canonicalize(to).ok()).map_or(false, |(a, b)| a == b);
    if same { return Err("That is Zest's own folder.".into()); }
    if !named(to, "games_", ".db").is_empty() { return Err("Zest already has a library here.".into()); }
    std::fs::create_dir_all(to).map_err(|e| format!("Could not make {}: {e}", to.display()))?;
    let rel = |p: &Path| p.strip_prefix(from).unwrap_or(p).to_path_buf();
    let mut plain: Vec<PathBuf> = FILES.iter().map(|n| from.join(n)).filter(|p| p.is_file()).collect();
    plain.extend(named(from, "group_sources_", ".json")); plain.extend(named(from, "steamgifts_wins_", ".json"));
    plain.extend(named(&from.join("static/img/badges"), "", ".png"));
    let bg = from.join("static/img/backgrounds/background.jpg"); if bg.is_file() { plain.push(bg); }
    let mut art = vec![]; covers(&from.join("static/img/library"), &mut art); art.sort();
    let dbs = named(from, "games_", ".db");
    let total = plain.len() + art.len() + dbs.len();
    let mut done = 0usize;
    let mut out = Imported::default();
    for p in plain.iter().chain(art.iter()) {
        if cancel.is_set() { return Err("Cancelled".into()); }
        let d = to.join(rel(p));
        if let Some(parent) = d.parent() { std::fs::create_dir_all(parent).map_err(|e| format!("Could not make {}: {e}", parent.display()))?; }
        std::fs::copy(p, &d).map_err(|e| format!("Could not copy {}: {e}", p.display()))?;
        if art.contains(p) { out.covers += 1 } else { out.files += 1 }
        done += 1; if done % 50 == 0 || done == plain.len() { progress(done, total); }
    }
    for db in &dbs {
        if cancel.is_set() { return Err("Cancelled".into()); }
        let name = db.file_name().unwrap_or_default();
        let (dest, tmp) = (to.join(name), to.join(format!("{}.importing", name.to_string_lossy())));
        let _ = std::fs::remove_file(&tmp);
        // a consistent copy, made inside its own read transaction even while PlayDate has the library open; it appears under its real name only when complete
        let c = Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| format!("Could not open {}: {e}", db.display()))?;
        c.busy_timeout(std::time::Duration::from_secs(30)).ok();
        c.execute("VACUUM INTO ?1", [tmp.to_string_lossy().to_string()]).map_err(|e| { let _ = std::fs::remove_file(&tmp); format!("Could not copy {}: {e}", db.display()) })?;
        drop(c);
        std::fs::rename(&tmp, &dest).map_err(|e| format!("Could not finish {}: {e}", dest.display()))?;
        out.libraries += 1; done += 1; progress(done, total);
    }
    // window placement, renderer and update links belong to the old program
    let dropped = crate::backup::sanitize_state(to);
    // tells the first start this library came from PlayDate, so it shows what changed instead of the tutorial
    mark_from_playdate(to);
    crate::net::log(&format!("PlayDate import from {}: {} librar{}, {} settings file(s), {} cover(s){}", from.display(), out.libraries, if out.libraries == 1 { "y" } else { "ies" }, out.files, out.covers,
        if dropped.is_empty() { String::new() } else { format!("; dropped {dropped:?}") }));
    progress(total, total);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tmp(tag: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("zest_pdimport_{tag}_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d }
    fn playdate(dir: &Path) {
        std::fs::create_dir_all(dir.join("static/img/library/vertical")).unwrap();
        std::fs::create_dir_all(dir.join("static/img/badges")).unwrap();
        std::fs::create_dir_all(dir.join("plugins/gog")).unwrap();
        let c = Connection::open(dir.join("games_76561198000000001.db")).unwrap();
        c.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE games (appid INTEGER PRIMARY KEY, name TEXT); INSERT INTO games VALUES (10, 'Ten'), (20, 'Twenty');").unwrap();
        // a row still only in the write-ahead log (PlayDate open): the copy must have it
        c.execute("INSERT INTO games VALUES (30, 'Thirty')", []).unwrap();
        std::mem::forget(c);
        std::fs::write(dir.join("config.json"), r#"{"active_account": "76561198000000001", "accounts": {"76561198000000001": {"api_key": "K"}}}"#).unwrap();
        std::fs::write(dir.join("state.json"), r#"{"sort": "name", "window_state": {"x": 1}}"#).unwrap();
        std::fs::write(dir.join("humble_cache.json"), "{}").unwrap();
        std::fs::write(dir.join("group_sources_76561198000000001.json"), "{}").unwrap();
        std::fs::write(dir.join("static/img/library/vertical/10.jpg"), "jpg").unwrap();
        std::fs::write(dir.join("static/img/badges/b.png"), "png").unwrap();
        for f in ["playdate.log", "main.py", "plugins/gog/plugin.json"] { std::fs::write(dir.join(f), "x").unwrap(); }
    }
    #[test] fn a_library_is_copied_with_what_a_backup_holds_and_nothing_else() {
        let (from, to) = (tmp("from"), tmp("to").join("Zest data"));
        playdate(&from);
        assert!(is_library(&from) && !is_library(&to));
        assert_eq!(summary(&from), vec![("76561198000000001".to_string(), 3)]);
        let mut steps = vec![];
        let r = import(&from, &to, &mut |d, t| steps.push((d, t)), &Cancel::new()).unwrap();
        assert_eq!(r, Imported { libraries: 1, files: 5, covers: 1 }, "config, state, Humble cache, group sources and the badge; one cover");
        let c = Connection::open(to.join("games_76561198000000001.db")).unwrap();
        assert_eq!(c.query_row("SELECT COUNT(*) FROM games", [], |r| r.get::<_, i64>(0)).unwrap(), 3, "the row still in PlayDate's write-ahead log came along");
        for f in ["config.json", "humble_cache.json", "group_sources_76561198000000001.json", "static/img/library/vertical/10.jpg", "static/img/badges/b.png"] { assert!(to.join(f).is_file(), "{f}"); }
        for f in ["playdate.log", "main.py", "plugins"] { assert!(!to.join(f).exists(), "{f} is not data"); }
        assert!(!std::fs::read_to_string(to.join("state.json")).unwrap().contains("window_state"), "the old window placement is dropped");
        let st: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(to.join("state.json")).unwrap()).unwrap();
        assert_eq!(st[FROM_PLAYDATE_KEY], serde_json::json!(true), "the library is marked as copied from PlayDate");
        assert!(!to.join("games_76561198000000001.db.importing").exists());
        assert_eq!(steps.last(), Some(&(7, 7)));
        assert!(import(&from, &to, &mut |_, _| {}, &Cancel::new()).unwrap_err().contains("already has a library"));
        assert!(import(&from, &from, &mut |_, _| {}, &Cancel::new()).is_err());
        assert!(import(&to.join("nothing"), &tmp("to2"), &mut |_, _| {}, &Cancel::new()).unwrap_err().contains("does not hold"));
    }
    #[test] fn playdate_is_found_through_its_flatpak_folder_and_the_shortcuts_its_installer_made() {
        let home = tmp("home");
        let flat = home.join(".var/app/io.github.robbyratpoison.PlayDate.Qt/data/playdate"); playdate(&flat);
        let src = home.join("Stuff/PlayDate-2026.10.9"); playdate(&src);
        std::fs::create_dir_all(home.join(".local/share/applications")).unwrap();
        std::fs::write(home.join(".local/share/applications/playdate.desktop"), format!("[Desktop Entry]\nName=PlayDate\nExec={}/launch.sh\n", src.display())).unwrap();
        let mac = home.join("Mac Games/PlayDate"); playdate(&mac);
        std::fs::create_dir_all(home.join("Applications/PlayDate.app/Contents/MacOS")).unwrap();
        std::fs::write(home.join("Applications/PlayDate.app/Contents/MacOS/PlayDate"), format!("#!/bin/sh\nexec \"{}/launch.sh\" \"$@\"\n", mac.display())).unwrap();
        let empty = home.join("PlayDate"); std::fs::create_dir_all(&empty).unwrap();
        let found = find(&home);
        assert_eq!(found, vec![flat, src, mac], "an empty PlayDate folder is not offered");
    }
    #[test] fn found_folders_get_short_names_and_how_long_ago_they_were_used() {
        let home = Path::new("/home/r");
        assert_eq!(label(&home.join(".var/app/io.github.robbyratpoison.PlayDate/data/playdate"), home), "PlayDate Flatpak");
        assert_eq!(label(&home.join(".var/app/io.github.robbyratpoison.PlayDate.Qt/data/playdate"), home), "PlayDate Flatpak (Qt)");
        assert_eq!(label(&home.join("Projects/PlayDate"), home), "~/Projects/PlayDate");
        assert_eq!(label(Path::new("/opt/PlayDate"), home), "/opt/PlayDate");
        let now = 1_000_000_000;
        assert_eq!([ago(now, now), ago(now - 86400, now), ago(now - 5 * 86400, now), ago(now - 90 * 86400, now), ago(now - 800 * 86400, now)], ["today", "yesterday", "5 days ago", "3 months ago", "2 years ago"]);
        let d = tmp("used"); playdate(&d); assert!(last_used(&d).map_or(false, |t| (now..now * 3).contains(&t)));
    }
    #[test] fn launcher_lines_give_the_install_folder() {
        assert_eq!(install_dirs_in("Exec=/home/a/My Apps/PlayDate/launch.sh"), vec![PathBuf::from("/home/a/My Apps/PlayDate")]);
        assert_eq!(install_dirs_in("exec \"/Users/r/PlayDate/playdate-launch.sh\" \"$@\""), vec![PathBuf::from("/Users/r/PlayDate")]);
        assert!(install_dirs_in("Exec=launch.sh").is_empty(), "a relative path says nothing");
    }
}

/// A real import, read-only on the source: `ZEST_IMPORT_FROM=<PlayDate folder> ZEST_IMPORT_TO=<empty folder> cargo test -p pd-filter real_playdate_import -- --ignored --nocapture`.
#[cfg(test)]
mod real_import {
    #[test] #[ignore] fn real_playdate_import() {
        let (Some(from), Some(to)) = (std::env::var_os("ZEST_IMPORT_FROM"), std::env::var_os("ZEST_IMPORT_TO")) else { return };
        let t = std::time::Instant::now();
        let r = super::import(std::path::Path::new(&from), std::path::Path::new(&to), &mut |_, _| {}, &crate::net::Cancel::new());
        println!("{r:?} in {:.1} s", t.elapsed().as_secs_f32());
        assert!(r.is_ok());
    }
}
