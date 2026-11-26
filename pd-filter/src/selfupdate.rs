//! Zest replacing itself (Linux and macOS tarballs, the Windows zip; a Flatpak updates through `flatpak install`). The running program is
//! swapped for the downloaded one and a small supervisor, the *old* copy of the program started in watch mode, makes sure the new one comes up:
//!  1. the new program is taken out of the download, put next to the running one and asked its version (`--version`) before anything is touched;
//!  2. the running program is copied to `<name>-old`, replaced by the new one (the old file is renamed aside first, which Windows allows for a running image),
//!     `.rollback/pending.json` is written (the same handshake `rollback_marker` answers: the new program deletes it once its first page is drawn);
//!  3. the old copy is started as `--zest-watch` and the app quits. The watchdog starts the new program; if it exits with an error before it has deleted
//!     the marker, twice, the old program is put back, the version goes to `.rollback/failed.json` (never offered again) and the old program is started.
//!     A program that exits cleanly, or is still running three minutes later without having cleared the marker, is left alone (a slow start is not a failure).
//! Not covered (PlayDate also snapshots the data folder): the data is not rolled back. Zest only adds columns, so an old program reads what a new one wrote.
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const ATTEMPTS: usize = 2;
pub const HANG_LIMIT: Duration = Duration::from_secs(180);

/// `<stem>-<tag><ext>` beside `exe`: `zest` -> `zest-old`, `Zest.exe` -> `Zest-old.exe`.
pub fn sibling(exe: &Path, tag: &str) -> PathBuf {
    let stem = exe.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "zest".into());
    let ext = exe.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    exe.with_file_name(format!("{stem}-{tag}{ext}"))
}
fn marker(root: &Path) -> PathBuf { root.join(".rollback/pending.json") }
fn now() -> f64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64()) }

/// Removes the leftovers of an earlier swap (`-old`, `-replaced`, `-failed`) once no update is in flight. Windows refuses while the watchdog still runs: tried again next start.
pub fn cleanup(root: &Path, exe: &Path) {
    if marker(root).exists() { return; }
    for t in ["old", "replaced", "failed"] { let _ = std::fs::remove_file(sibling(exe, t)); let _ = std::fs::remove_file(sibling(&helper_beside(exe), t)); }
}

/// The sign-in helper (`zest-login`) sits next to the program and is updated with it. Name of its file.
pub fn helper_name() -> &'static str { if cfg!(windows) { "zest-login.exe" } else { "zest-login" } }
/// Where the helper lives for a program at `exe`.
pub fn helper_beside(exe: &Path) -> PathBuf { exe.with_file_name(helper_name()) }

fn find_named(dir: &Path, names: &[&str]) -> Option<PathBuf> {
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() { if let Some(f) = find_named(&p, names) { return Some(f); } }
        else if let Some(n) = p.file_name().map(|n| n.to_string_lossy().to_lowercase()) { if names.contains(&n.as_str()) { return Some(p); } }
    }
    None
}
fn find_program(dir: &Path) -> Option<PathBuf> { find_named(dir, &["zest", "zest.exe"]) }

/// Puts the download's sign-in helper in place of the installed one (the old one is kept as `-old` for a rollback). The helper only runs while a sign-in
/// window is open, so it is normally free to replace; a failure here is reported to the caller but never stops the update (the old helper stays).
fn swap_helper(exe: &Path, found: &Path) -> Result<(), String> {
    let (target, new) = (helper_beside(exe), sibling(&helper_beside(exe), "new"));
    std::fs::copy(found, &new).map_err(|e| format!("sign-in helper: {e}"))?;
    #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; let _ = std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755)); }
    let (old, replaced) = (sibling(&target, "old"), sibling(&target, "replaced"));
    let _ = std::fs::remove_file(&old);
    if target.exists() { std::fs::copy(&target, &old).map_err(|e| { let _ = std::fs::remove_file(&new); format!("sign-in helper: {e}") })?; }
    let _ = std::fs::remove_file(&replaced);
    if target.exists() { std::fs::rename(&target, &replaced).map_err(|e| { let _ = std::fs::remove_file(&new); format!("sign-in helper: {e}") })?; }
    if let Err(e) = std::fs::rename(&new, &target) { let _ = std::fs::rename(&replaced, &target); return Err(format!("sign-in helper: {e}")); }
    #[cfg(unix)] { let _ = std::fs::remove_file(&replaced); }
    Ok(())
}
/// A rollback puts the old helper back too (if this update replaced one), so program and helper stay a pair.
fn restore_helper(exe: &Path) {
    let target = helper_beside(exe); let old = sibling(&target, "old");
    if !old.exists() { return; }
    let _ = std::fs::remove_file(&target);
    if std::fs::copy(&old, &target).is_ok() {
        #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)); }
    }
}

/// Runs `program --version` (ten seconds at most) and returns what it printed.
pub fn ask_version(program: &Path) -> Result<String, String> {
    let mut child = { use crate::proc::Hide; Command::new(program).arg("--version").hidden().stdin(Stdio::null()) }.stdout(Stdio::piped()).stderr(Stdio::null()).spawn().map_err(|e| format!("The new program would not start: {e}"))?;
    let t0 = Instant::now();
    loop {
        match child.try_wait() { Ok(Some(_)) => break, Ok(None) if t0.elapsed() < Duration::from_secs(10) => std::thread::sleep(Duration::from_millis(50)), _ => { let _ = child.kill(); return Err("The new program did not answer.".into()); } }
    }
    let mut s = String::new(); use std::io::Read; if let Some(mut o) = child.stdout.take() { let _ = o.read_to_string(&mut s); }
    Ok(s.trim().to_string())
}

/// Swaps the running program for the one in `archive` (a tar.gz or zip from a release) and starts the watchdog. On Ok the caller must quit at once.
pub fn install(root: &Path, archive: &Path, to_version: &str) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = exe.canonicalize().unwrap_or(exe);
    // Windows hands back `\\?\C:\...`: fine for the system, ugly in a process list and in the paths the supervisor is given
    let exe = exe.to_string_lossy().strip_prefix(r"\\?\").map(PathBuf::from).unwrap_or(exe);
    install_at(&exe, root, archive, to_version, true)
}

/// `install` for an explicit program path; `start_watchdog` false is for tests (the watchdog is the program itself).
pub fn install_at(exe: &Path, root: &Path, archive: &Path, to_version: &str, start_watchdog: bool) -> Result<(), String> {
    let exe = exe.to_path_buf();
    let dir = exe.parent().ok_or("Zest does not know where it is installed.")?;
    let stage = root.join(".rollback/stage"); let _ = std::fs::remove_dir_all(&stage); std::fs::create_dir_all(&stage).map_err(|e| e.to_string())?;
    crate::archive::extract(archive, &stage, &mut |_, _| {}, &|| false)?;
    let found = find_program(&stage).ok_or("The download has no Zest program in it.")?;
    let new = sibling(&exe, "new");
    std::fs::copy(&found, &new).map_err(|e| format!("Zest cannot write next to itself ({}): {e}. Install the download yourself.", dir.display()))?;
    #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; let _ = std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755)); }
    let helper = find_named(&stage, &[helper_name()]).map(|h| { let keep = root.join(".rollback/helper.new"); (std::fs::copy(&h, &keep).is_ok()).then_some(keep) }).flatten();
    let _ = std::fs::remove_dir_all(&stage);
    let said = ask_version(&new).inspect_err(|_| { let _ = std::fs::remove_file(&new); })?;
    if !said.contains(to_version) { let _ = std::fs::remove_file(&new); return Err(format!("The download says it is \"{said}\", not {to_version}; nothing was changed.")); }
    let (old, replaced) = (sibling(&exe, "old"), sibling(&exe, "replaced"));
    std::fs::copy(&exe, &old).map_err(|e| format!("Could not keep a copy of this version: {e}"))?;
    let _ = std::fs::remove_file(&replaced);
    std::fs::rename(&exe, &replaced).map_err(|e| { let _ = std::fs::remove_file(&new); format!("Could not replace the running program: {e}") })?;
    if let Err(e) = std::fs::rename(&new, &exe) { let _ = std::fs::rename(&replaced, &exe); return Err(format!("Could not put the new program in place: {e}")); }
    #[cfg(unix)] { let _ = std::fs::remove_file(&replaced); }   // a running program's file can be deleted here; Windows leaves it for the next start (`cleanup`)
    if let Some(h) = helper { if let Err(e) = swap_helper(&exe, &h) { eprintln!("zest update: {e} (the old helper stays)"); } let _ = std::fs::remove_file(&h); }
    resign_app_bundle(&exe);
    let _ = std::fs::create_dir_all(root.join(".rollback"));
    let pending = json!({ "old_token": crate::rollback_marker::token(), "started": now(), "to": to_version, "kind": "zest-binary" });
    std::fs::write(marker(root), pending.to_string()).map_err(|e| format!("Could not write the update marker: {e}"))?;
    if !start_watchdog { return Ok(()); }
    let mut w = Command::new(&old);
    w.arg("--zest-watch").arg(&exe).arg(&old).arg(root).arg(std::process::id().to_string()).arg(to_version).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(unix)] { use std::os::unix::process::CommandExt; w.process_group(0); }
    #[cfg(windows)] { use std::os::windows::process::CommandExt; w.creation_flags(0x0000_0008 | 0x0800_0000); }
    w.spawn().map_err(|e| format!("Could not start the update supervisor: {e}"))?;
    Ok(())
}

#[cfg(unix)] fn alive(pid: u32) -> bool { unsafe { libc::kill(pid as i32, 0) == 0 } }
#[cfg(not(unix))] fn alive(_pid: u32) -> bool { false }

/// The `.app` folder a macOS program file sits in (`.../Zest.app/Contents/MacOS/Zest` -> `.../Zest.app`), if it is in one.
pub fn app_bundle_of(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?; let contents = macos.parent()?; let app = contents.parent()?;
    let named = |p: &Path, n: &str| p.file_name().map_or(false, |f| f == n);
    (named(macos, "MacOS") && named(contents, "Contents") && app.extension().map_or(false, |e| e.eq_ignore_ascii_case("app"))).then(|| app.to_path_buf())
}

/// macOS: Zest.app carries an ad-hoc signature that also seals the other files in the app (zest-login, Info.plist). Swapping the program files in place
/// leaves that seal out of date, which Apple Silicon may refuse to start, so the app is signed again (ad hoc, as package/make-macos-app.sh does) after a
/// swap and after a rollback. `codesign` is part of macOS. A failure is only logged: the new program file still carries its own signature.
fn resign_app_bundle(exe: &Path) {
    if !cfg!(target_os = "macos") { return; }
    let Some(app) = app_bundle_of(exe) else { return };
    match Command::new("/usr/bin/codesign").args(["--force", "--deep", "--sign", "-"]).arg(&app).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).output() {
        Ok(o) if o.status.success() => {}
        Ok(o) => eprintln!("zest update: could not re-sign {}: {}", app.display(), String::from_utf8_lossy(&o.stderr).trim()),
        Err(e) => eprintln!("zest update: could not run codesign: {e}"),
    }
}

fn record_failure(root: &Path, version: &str) {
    let p = root.join(".rollback/failed.json");
    let mut v: Vec<String> = std::fs::read_to_string(&p).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    if !v.iter().any(|x| x == version) { v.push(version.to_string()); }
    let _ = std::fs::write(&p, serde_json::to_string(&v).unwrap_or_default());
}

/// The watchdog (`zest --zest-watch <exe> <old copy> <data folder> <old pid> <version>`): returns the process exit code. `settle` is how long to wait for the old
/// program to be gone, `hang` the point after which a program that never cleared the marker is left alone.
pub fn watch(exe: &Path, old: &Path, root: &Path, old_pid: u32, to_version: &str, settle: Duration, hang: Duration) -> i32 {
    let t0 = Instant::now();
    while alive(old_pid) && t0.elapsed() < settle { std::thread::sleep(Duration::from_millis(100)); }
    std::thread::sleep(Duration::from_millis(300));
    for _ in 0..ATTEMPTS {
        let Ok(mut child) = Command::new(exe).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() else { std::thread::sleep(Duration::from_millis(500)); continue };
        let started = Instant::now();
        loop {
            if !marker(root).exists() { return 0; }                       // it drew its first page: healthy, nothing more to do
            match child.try_wait() {
                Ok(Some(st)) => { if st.success() { return 0; } break; }  // a clean exit is left alone; an error is a failed attempt
                Ok(None) if started.elapsed() >= hang => return 0,        // still alive after three minutes: slow, not broken
                Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                Err(_) => break,
            }
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    // two failures: put the old program back
    let failed = sibling(exe, "failed");
    let _ = std::fs::remove_file(&failed);
    let _ = std::fs::rename(exe, &failed);
    if std::fs::copy(old, exe).is_err() { return 1; }
    restore_helper(exe);
    resign_app_bundle(exe);
    #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; let _ = std::fs::set_permissions(exe, std::fs::Permissions::from_mode(0o755)); }
    record_failure(root, to_version);
    let _ = std::fs::remove_file(marker(root));
    let _ = std::fs::write(root.join(".rollback/rolled_back.json"), json!({ "version": to_version, "at": now() }).to_string());
    let _ = Command::new(exe).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
    0
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    fn tmp(tag: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("zest_su_{tag}_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(d.join("root/.rollback")).unwrap(); d }
    fn script(p: &Path, body: &str) { std::fs::write(p, format!("#!/bin/sh\n{body}\n")).unwrap(); std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap(); }
    fn wait_for(p: &Path) -> bool { for _ in 0..100 { if p.exists() { return true; } std::thread::sleep(Duration::from_millis(50)); } false }

    #[test] fn the_app_bundle_is_found_only_for_a_program_inside_one() {
        assert_eq!(app_bundle_of(Path::new("/Users/r/Applications/Zest.app/Contents/MacOS/Zest")), Some(PathBuf::from("/Users/r/Applications/Zest.app")));
        assert_eq!(app_bundle_of(Path::new("/home/r/.local/share/zest/app/zest")), None);
        assert_eq!(app_bundle_of(Path::new("/x/Contents/MacOS/Zest")), None, "Contents/MacOS outside an .app is not a bundle");
    }
    #[test] fn names_go_before_the_extension() {
        assert_eq!(sibling(Path::new("/a/zest"), "old"), Path::new("/a/zest-old")); assert_eq!(sibling(Path::new("C:/z/Zest.exe"), "new"), Path::new("C:/z/Zest-new.exe"));
    }
    #[test] fn a_new_program_that_fails_twice_is_replaced_by_the_old_one_and_never_offered_again() {
        let d = tmp("fail"); let root = d.join("root");
        let (exe, old) = (d.join("zest"), d.join("zest-old"));
        script(&exe, "exit 3");                                  // the broken new version
        script(&old, &format!("echo old > {}/ran-old", d.display()));
        // the update had replaced the sign-in helper too: its old copy is put back with the program
        script(&helper_beside(&exe), "echo new-helper"); script(&sibling(&helper_beside(&exe), "old"), "echo old-helper");
        std::fs::write(marker(&root), r#"{"old_token":"x"}"#).unwrap();
        assert_eq!(watch(&exe, &old, &root, 0, "9.9.9", Duration::from_millis(10), Duration::from_secs(5)), 0);
        assert!(wait_for(&d.join("ran-old")), "the old program was started again");
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), std::fs::read_to_string(&old).unwrap(), "the old program is back in place");
        assert!(!marker(&root).exists() && sibling(&exe, "failed").exists());
        assert_eq!(std::fs::read_to_string(root.join(".rollback/failed.json")).unwrap(), r#"["9.9.9"]"#);
        assert!(root.join(".rollback/rolled_back.json").exists());
        assert!(std::fs::read_to_string(helper_beside(&exe)).unwrap().contains("old-helper"), "the old sign-in helper is back");
    }
    #[test] fn a_program_that_clears_the_marker_is_left_alone() {
        let d = tmp("ok"); let root = d.join("root");
        let (exe, old) = (d.join("zest"), d.join("zest-old"));
        script(&exe, &format!("rm -f {}; sleep 1", marker(&root).display()));
        script(&old, &format!("touch {}/ran-old", d.display()));
        std::fs::write(marker(&root), "{}").unwrap();
        assert_eq!(watch(&exe, &old, &root, 0, "9.9.9", Duration::from_millis(10), Duration::from_secs(5)), 0);
        std::thread::sleep(Duration::from_millis(300));
        assert!(!d.join("ran-old").exists() && !root.join(".rollback/failed.json").exists());
    }
    #[test] fn a_clean_exit_or_a_slow_start_is_not_a_failure() {
        let d = tmp("clean"); let root = d.join("root");
        let (exe, old) = (d.join("zest"), d.join("zest-old"));
        script(&exe, "exit 0"); script(&old, &format!("touch {}/ran-old", d.display()));
        std::fs::write(marker(&root), "{}").unwrap();
        assert_eq!(watch(&exe, &old, &root, 0, "9.9.9", Duration::from_millis(10), Duration::from_secs(5)), 0);
        assert!(!d.join("ran-old").exists());
        script(&exe, "sleep 3");
        assert_eq!(watch(&exe, &old, &root, 0, "9.9.9", Duration::from_millis(10), Duration::from_millis(400)), 0);
        assert!(!d.join("ran-old").exists(), "alive past the limit with the marker still there: left alone");
    }
    #[test] fn an_update_is_taken_from_a_tarball_checked_swapped_in_and_marked() {
        let d = tmp("inst"); let root = d.join("root");
        let exe = d.join("zest"); script(&exe, "echo Zest 1.0.0");
        let pack = d.join("pack/zest-9.9.9-linux-x86_64"); std::fs::create_dir_all(&pack).unwrap(); script(&pack.join("zest"), "echo Zest 9.9.9"); script(&pack.join("zest-login"), "echo helper 9.9.9"); std::fs::write(pack.join("README.txt"), "hi").unwrap();
        script(&d.join("zest-login"), "echo helper 1.0.0");
        let tgz = d.join("zest-9.9.9-linux-x86_64.tar.gz");
        assert!(Command::new("tar").arg("-C").arg(d.join("pack")).arg("-czf").arg(&tgz).arg("zest-9.9.9-linux-x86_64").status().unwrap().success());
        // a download that is not the version the release says changes nothing
        assert!(install_at(&exe, &root, &tgz, "8.8.8", false).unwrap_err().contains("nothing was changed"));
        assert_eq!(ask_version(&exe).unwrap(), "Zest 1.0.0"); assert!(!marker(&root).exists() && !sibling(&exe, "old").exists());
        install_at(&exe, &root, &tgz, "9.9.9", false).unwrap();
        assert_eq!(ask_version(&exe).unwrap(), "Zest 9.9.9", "the new program is in place");
        assert_eq!(ask_version(&sibling(&exe, "old")).unwrap(), "Zest 1.0.0", "the old one is kept");
        assert!(marker(&root).exists()); assert!(!sibling(&exe, "new").exists());
        assert_eq!(ask_version(&helper_beside(&exe)).unwrap(), "helper 9.9.9", "the sign-in helper is updated with the program");
        assert_eq!(ask_version(&sibling(&helper_beside(&exe), "old")).unwrap(), "helper 1.0.0", "and its old copy is kept for a rollback");
        assert!(!sibling(&helper_beside(&exe), "new").exists());
        cleanup(&root, &exe); assert!(sibling(&helper_beside(&exe), "old").exists(), "an update still in flight keeps its rollback copies");
        std::fs::remove_file(marker(&root)).unwrap(); cleanup(&root, &exe);
        assert!(!sibling(&helper_beside(&exe), "old").exists() && !sibling(&exe, "old").exists(), "once healthy the leftovers go");
        // a download with no program in it
        let empty = d.join("empty/x"); std::fs::create_dir_all(&empty).unwrap(); std::fs::write(empty.join("a.txt"), "x").unwrap();
        let t2 = d.join("e.tar.gz"); assert!(Command::new("tar").arg("-C").arg(d.join("empty")).arg("-czf").arg(&t2).arg("x").status().unwrap().success());
        assert!(install_at(&exe, &root, &t2, "9.9.9", false).unwrap_err().contains("no Zest program"));
    }
    #[test] fn the_version_the_program_prints_is_checked_before_anything_changes() {
        let d = tmp("ver"); let p = d.join("p"); script(&p, "echo Zest 1.2.3");
        assert_eq!(ask_version(&p).unwrap(), "Zest 1.2.3");
        assert!(ask_version(&d.join("missing")).is_err());
    }
}
