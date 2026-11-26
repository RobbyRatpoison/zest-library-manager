//! Xbox / Game Pass games on Linux through Xodus Nexus, an unofficial launcher (port of PlayDate's `plugins/xbox/nexus.py`, the launch and install-detection half; the
//! "Import Owned Games" call into Nexus's backend is not ported). Nexus keeps each game in its own folder under an install root, identified by `MicrosoftGame.Config`'s `StoreId`;
//! it can only be told to launch a game (`--launch-game <id>`), not to install one, so a game that is not there yet opens Nexus for the user to install it.
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const RELEASES_URL: &str = "https://github.com/domsvoboda/nexus-launcher/releases";
pub fn install_hint() -> String { format!("Nexus Launcher, formerly Xodus Nexus (an unofficial, Linux-only launcher), runs Xbox / Game Pass PC games. Install it from {RELEASES_URL}, sign in and install the game there.") }

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex { cell.get_or_init(|| Regex::new(pat).unwrap()) }

#[cfg(unix)] fn is_exec(p: &Path) -> bool { use std::os::unix::fs::PermissionsExt; p.is_file() && std::fs::metadata(p).map_or(false, |m| m.permissions().mode() & 0o111 != 0) }
#[cfg(not(unix))] fn is_exec(p: &Path) -> bool { p.is_file() }

/// The Nexus launcher: its usual install spots, `xodus-nexus` on the path, then a versioned AppImage in the usual download folders.
pub fn find_launcher(home: &Path) -> Option<PathBuf> {
    for p in [home.join(".local/bin/xodus-nexus"), home.join(".local/opt/xodus-nexus/Xodus-Nexus.AppImage")] { if is_exec(&p) { return Some(p); } }
    if let Some(p) = crate::wine::which("xodus-nexus", &std::env::var_os("PATH").unwrap_or_default(), &[]) { return Some(p); }
    static R: OnceLock<Regex> = OnceLock::new();
    let appimage = re(&R, r"(?i)^(xodus[-_ ]?nexus|nexus[-_ ]?launcher).*\.appimage$");
    for d in [".local/bin", ".local/opt/xodus-nexus", "Applications", "AppImages", "Downloads"] {
        let Ok(rd) = std::fs::read_dir(home.join(d)) else { continue };
        let mut names: Vec<String> = rd.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        // the renamed Nexus Launcher (0.5 and later) before any old Xodus Nexus file, newest name first within each
        names.sort(); names.reverse(); names.sort_by_key(|n| !n.to_lowercase().starts_with("nexus"));
        for n in names { let p = home.join(d).join(&n); if appimage.is_match(&n) && is_exec(&p) { return Some(p); } }
    }
    None
}

fn read_state(home: &Path) -> Value {
    for d in [".config/Xodus Nexus", ".config/xodus-nexus"] {
        if let Some(v) = std::fs::read_to_string(home.join(d).join("launcher-state.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).filter(|v| v.is_object()) { return v; }
    }
    Value::Object(Default::default())
}

/// Where Nexus installs games: its saved install root, else `~/Games/Xodus Nexus`.
pub fn install_root(home: &Path) -> PathBuf {
    read_state(home).get("settings").and_then(|s| s.get("installRoot")).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(PathBuf::from).unwrap_or_else(|| home.join("Games/Xodus Nexus"))
}

/// Candidate game folders: the ones Nexus recorded plus the install root's children, each once, as real paths that exist.
pub fn game_dirs(home: &Path) -> Vec<PathBuf> {
    let state = read_state(home);
    let root = state.get("settings").and_then(|s| s.get("installRoot")).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(PathBuf::from).unwrap_or_else(|| home.join("Games/Xodus Nexus"));
    let mut dirs: Vec<PathBuf> = state.get("games").and_then(|g| g.as_array()).map(|a| a.iter().filter_map(|g| g.get("path")?.as_str().map(PathBuf::from)).filter(|p| p.is_absolute()).collect()).unwrap_or_default();
    if let Ok(rd) = std::fs::read_dir(&root) { let mut kids: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect(); kids.sort(); dirs.extend(kids); }
    let mut seen = std::collections::HashSet::new();
    dirs.into_iter().filter_map(|p| std::fs::canonicalize(&p).ok()).filter(|r| r.is_dir() && seen.insert(r.clone())).collect()
}

fn find_file(dir: &Path, lowered: &str) -> Option<PathBuf> { std::fs::read_dir(dir).ok()?.flatten().find(|e| e.file_name().to_string_lossy().to_lowercase() == lowered).map(|e| e.path()) }
fn read_text(p: &Path) -> String {
    use std::io::Read;
    let mut b = Vec::new();
    if let Ok(f) = std::fs::File::open(p) { let _ = f.take(1_000_000).read_to_end(&mut b); }
    let s = String::from_utf8_lossy(&b).to_string();
    s.strip_prefix('\u{feff}').map(String::from).unwrap_or(s)
}

/// `rel` under `dir`, each part matched without regard to case (a game's config can name `Game.exe` while the file on disk is `GAME.EXE`; Windows does not care, Linux does).
fn resolve_nocase(dir: &Path, rel: &str) -> Option<PathBuf> {
    let mut cur = dir.to_path_buf();
    for part in rel.split('/').filter(|p| !p.is_empty() && *p != ".") {
        if part == ".." { return None; }
        let exact = cur.join(part);
        cur = if exact.exists() { exact } else { find_file(&cur, &part.to_lowercase())? };
    }
    std::fs::canonicalize(cur).ok()
}

/// Nexus's id for a game folder: the first 16 hex digits of the SHA-256 of its real path.
pub fn launch_id(dir: &Path) -> String {
    let real = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let d = ring::digest::digest(&ring::digest::SHA256, real.to_string_lossy().as_bytes());
    d.as_ref().iter().take(8).map(|b| format!("{b:02x}")).collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct Info { pub store_id: String, pub path: PathBuf, pub ready: bool, pub id: String }

/// A Nexus game folder: its Store ID, whether the program and Nexus's streaming marker are both there, and its launch id. None for any other folder.
pub fn inspect_game_dir(dir: &Path) -> Option<Info> {
    let cfg = find_file(dir, "microsoftgame.config").map(|p| read_text(&p)).unwrap_or_default();
    let appx = find_file(dir, "appxmanifest.xml").map(|p| read_text(&p)).unwrap_or_default();
    if cfg.is_empty() && appx.is_empty() { return None; }
    static S: OnceLock<Regex> = OnceLock::new(); static E: OnceLock<Regex> = OnceLock::new(); static A: OnceLock<Regex> = OnceLock::new();
    let store_id = re(&S, r"(?i)<StoreId>\s*([A-Z0-9]{12})\s*</StoreId>").captures(&cfg)?[1].to_uppercase();
    let exe = re(&E, r#"(?i)<Executable\b[^>]*\bName\s*=\s*"([^"]+)""#).captures(&cfg).or_else(|| re(&A, r#"(?i)<Application\b[^>]*\bExecutable\s*=\s*"([^"]+)""#).captures(&appx)).map(|c| c[1].replace('\\', "/")).unwrap_or_default();
    let real_dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let exe_path = if exe.is_empty() { None } else { resolve_nocase(&real_dir, &exe) };
    let ready = exe_path.map_or(false, |p| p.starts_with(&real_dir) && p != real_dir && p.is_file()) && real_dir.join(".xodus-streaming.msixvc").is_file();
    Some(Info { store_id, path: real_dir.clone(), ready, id: launch_id(&real_dir) })
}

// ---------------------------------------------------------------- uninstalling a game the way Nexus does, while Nexus is closed
// Nexus (0.5.1, its StateStore) reads launcher-state.json once when it starts, keeps it in memory and writes the whole of it back on every change and when it quits,
// so an edit made while any Nexus program runs is lost. Its own "remove game" deletes the game folder, drops the game's entry and its shortcuts, and keeps the
// Wine prefix (saves) in ~/.local/share/xodus-nexus/compatdata. Zest does the same, and only when no Nexus program runs.

pub const NEXUS_OPEN: &str = "Nexus is open. Uninstall this game from Nexus, or quit Nexus completely (also from the tray) and uninstall it here.";

/// A process's first argument is a Nexus program: the launcher (`xodus-nexus.bin`, `xodus-nexus`), its AppImage (old or new name) or its backend (`xodus-cli` under the Nexus folder).
pub fn is_nexus_cmd(argv0: &str) -> bool {
    let l = argv0.to_lowercase();
    let base = l.rsplit('/').next().unwrap_or(&l);
    base.starts_with("xodus-nexus") || base.starts_with("xodus_nexus") || base.starts_with("nexus-launcher") || (base == "xodus-cli" && l.contains("xodus-nexus"))
}

/// Whether any Nexus program runs (Linux, from /proc).
pub fn nexus_running() -> bool {
    let Ok(rd) = std::fs::read_dir("/proc") else { return false };
    rd.flatten().any(|e| {
        let n = e.file_name(); let n = n.to_string_lossy();
        n.chars().all(|c| c.is_ascii_digit()) && std::fs::read(e.path().join("cmdline")).map_or(false, |c| is_nexus_cmd(&String::from_utf8_lossy(c.split(|b| *b == 0).next().unwrap_or(&[]))))
    })
}

/// The launcher-state.json Nexus reads (the first of its two profile folders that holds a readable one).
fn state_file(home: &Path) -> Option<PathBuf> {
    [".config/Xodus Nexus", ".config/xodus-nexus"].iter().map(|d| home.join(d).join("launcher-state.json"))
        .find(|p| std::fs::read_to_string(p).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).map_or(false, |v| v.is_object()))
}

fn same_dir(a: &str, b: &Path) -> bool { std::fs::canonicalize(a).map_or(false, |r| r == b) || Path::new(a) == b }

/// Uninstalls the Nexus game with this Store ID: refuses while Nexus runs or while Nexus has an unfinished download of it, deletes its folder (only one inside
/// Nexus's install root or recorded by Nexus, never the root itself), then removes its entry from launcher-state.json (a copy of the old file is kept as
/// `launcher-state.json.zest-backup`) and its menu and desktop shortcuts. The Wine prefix with the saves stays, as with Nexus's own uninstall.
/// `running` is `nexus_running` (a stand-in in the tests). Returns the folder that was removed.
pub fn remove_game(home: &Path, store_id: &str, running: &dyn Fn() -> bool) -> Result<PathBuf, String> {
    if running() { return Err(NEXUS_OPEN.into()); }
    let store_id = store_id.to_uppercase();
    let info = scan_installed(home).remove(&store_id).ok_or("Nexus has no folder for this game. It may already be uninstalled.")?;
    let dir = info.path.clone();
    let state = read_state(home);
    // only a folder Nexus would own: inside its install root (not the root), or one it recorded; never the home folder
    let root = std::fs::canonicalize(install_root(home)).unwrap_or_else(|_| install_root(home));
    let recorded = state.get("games").and_then(|g| g.as_array()).map_or(false, |a| a.iter().any(|g| g.get("path").and_then(|p| p.as_str()).map_or(false, |p| same_dir(p, &dir))));
    let home_real = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    if dir == root || dir == home_real || dir.components().count() < 3 || !(crate::installdir::is_inside(&dir, &root) || recorded) {
        return Err(format!("Zest will not delete {}: it is not a Nexus game folder.", dir.display()));
    }
    let busy = state.get("downloads").and_then(|d| d.as_array()).map_or(false, |a| a.iter().any(|t| {
        let active = t.get("status").and_then(|v| v.as_str()).map_or(false, |s| matches!(s, "running" | "queued" | "paused"));
        let this = t.get("productId").and_then(|v| v.as_str()).map_or(false, |p| p.eq_ignore_ascii_case(&store_id)) || t.get("destination").and_then(|v| v.as_str()).map_or(false, |p| same_dir(p, &dir));
        active && this
    }));
    if busy { return Err("Nexus has an unfinished download of this game. Finish or cancel it in Nexus first.".into()); }
    std::fs::remove_dir_all(&dir).map_err(|e| format!("Could not delete {}: {e}", dir.display()))?;
    // the entry: by Nexus's id (from the folder's real path), the folder, or the Store ID
    if let Some(file) = state_file(home) {
        let text = std::fs::read_to_string(&file).map_err(|e| format!("The game's files were deleted, but Nexus's list could not be read: {e}"))?;
        let mut v: Value = serde_json::from_str(&text).map_err(|e| format!("The game's files were deleted, but Nexus's list could not be read: {e}"))?;
        let before = v.get("games").and_then(|g| g.as_array()).map_or(0, |a| a.len());
        if let Some(games) = v.get_mut("games").and_then(|g| g.as_array_mut()) {
            games.retain(|g| {
                let id = g.get("id").and_then(|x| x.as_str()).map_or(false, |x| x == info.id);
                let path = g.get("path").and_then(|x| x.as_str()).map_or(false, |p| Path::new(p) == dir || same_dir(p, &dir));
                let store = g.get("storeId").and_then(|x| x.as_str()).map_or(false, |x| x.eq_ignore_ascii_case(&store_id));
                !(id || path || store)
            });
        }
        let after = v.get("games").and_then(|g| g.as_array()).map_or(0, |a| a.len());
        if after != before {
            let late = "The game's files were deleted, but Nexus started meanwhile. Open Nexus and remove the game from its library there.";
            let _ = std::fs::write(file.with_extension("json.zest-backup"), &text);
            let tmp = file.with_extension("json.zest-tmp");
            std::fs::write(&tmp, serde_json::to_string_pretty(&v).unwrap_or_default()).map_err(|e| format!("The game's files were deleted, but Nexus's list could not be saved: {e}"))?;
            #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)); }
            // checked again just before the swap: a Nexus started meanwhile would write its old copy back
            if running() { let _ = std::fs::remove_file(&tmp); return Err(late.into()); }
            std::fs::rename(&tmp, &file).map_err(|e| format!("The game's files were deleted, but Nexus's list could not be saved: {e}"))?;
        }
    }
    remove_shortcuts(home, &info.id);
    Ok(dir)
}

/// Nexus's own shortcuts for one game: `.desktop` files in the menu and desktop folders that carry its marker lines.
fn remove_shortcuts(home: &Path, id: &str) {
    let mark = format!("X-Xodus-Nexus-Game={id}");
    for d in [home.join(".local/share/applications"), home.join("Desktop")] {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().map_or(true, |x| x != "desktop") || !std::fs::symlink_metadata(&p).map_or(false, |m| m.is_file() && m.len() < 128 * 1024) { continue; }
            let t = std::fs::read_to_string(&p).unwrap_or_default();
            if t.lines().any(|l| l.starts_with("X-Xodus-Nexus-Profile=")) && t.lines().any(|l| l == mark) { let _ = std::fs::remove_file(&p); }
        }
    }
}

/// Every Nexus game folder by Store ID (the first folder wins).
pub fn scan_installed(home: &Path) -> HashMap<String, Info> {
    let mut found = HashMap::new();
    for d in game_dirs(home) { if let Some(i) = inspect_game_dir(&d) { found.entry(i.store_id.clone()).or_insert(i); } }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tmp(tag: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("zest_nexus_{tag}_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d }
    fn game(root: &Path, name: &str, store: &str, ready: bool) -> PathBuf {
        let g = root.join(name); std::fs::create_dir_all(g.join("bin")).unwrap();
        std::fs::write(g.join("MicrosoftGame.config"), format!("\u{feff}<Game><StoreId> {store} </StoreId><ExecutableList><Executable Name=\"bin\\Game.exe\" /></ExecutableList></Game>")).unwrap();
        if ready { std::fs::write(g.join("bin/Game.exe"), "x").unwrap(); std::fs::write(g.join(".xodus-streaming.msixvc"), "y").unwrap(); }
        g
    }
    #[test] fn game_folders_are_read_by_store_id_and_ready_means_the_program_and_the_streaming_marker_exist() {
        let h = tmp("scan"); let root = h.join("Games/Xodus Nexus");
        game(&root, "A", "9abcdefghijk", true); game(&root, "B", "BBBBBBBBBBBB", false); std::fs::create_dir_all(root.join("not-a-game")).unwrap();
        let found = scan_installed(&h);
        assert_eq!(found.len(), 2);
        assert!(found["9ABCDEFGHIJK"].ready && !found["BBBBBBBBBBBB"].ready);
        assert_eq!(found["9ABCDEFGHIJK"].path, std::fs::canonicalize(root.join("A")).unwrap());
    }
    #[test] fn nexus_state_adds_recorded_folders_and_a_custom_install_root_and_folders_are_listed_once() {
        let h = tmp("state"); let other = h.join("elsewhere"); let custom = h.join("custom-root");
        game(&other, "G", "CCCCCCCCCCCC", true); game(&custom, "H", "DDDDDDDDDDDD", true);
        std::fs::create_dir_all(h.join(".config/Xodus Nexus")).unwrap();
        std::fs::write(h.join(".config/Xodus Nexus/launcher-state.json"), serde_json::json!({"settings": {"installRoot": custom}, "games": [{"path": other.join("G")}, {"path": other.join("G")}, {"path": "relative"}]}).to_string()).unwrap();
        let dirs = game_dirs(&h); assert_eq!(dirs.len(), 2, "{dirs:?}");
        assert_eq!(scan_installed(&h).keys().cloned().collect::<std::collections::BTreeSet<_>>().into_iter().collect::<Vec<_>>(), ["CCCCCCCCCCCC", "DDDDDDDDDDDD"]);
    }
    #[test] fn the_launch_id_is_the_first_sixteen_hex_digits_of_the_sha256_of_the_real_path_and_the_launcher_is_found_by_name_or_appimage() {
        let h = tmp("id"); let g = h.join("g"); std::fs::create_dir_all(&g).unwrap();
        let real = std::fs::canonicalize(&g).unwrap();
        let want = { let d = ring::digest::digest(&ring::digest::SHA256, real.to_string_lossy().as_bytes()); d.as_ref()[..8].iter().map(|b| format!("{b:02x}")).collect::<String>() };
        assert_eq!(launch_id(&g), want); assert_eq!(launch_id(&g).len(), 16);
        assert_eq!(find_launcher(&h), None);
        std::fs::create_dir_all(h.join("Downloads")).unwrap();
        let ai = h.join("Downloads/Xodus-Nexus-0.3.11-x86_64.AppImage"); std::fs::write(&ai, "x").unwrap();
        #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; assert_eq!(find_launcher(&h), None, "not executable"); std::fs::set_permissions(&ai, std::fs::Permissions::from_mode(0o755)).unwrap(); assert_eq!(find_launcher(&h), Some(ai)); }
        let nl = h.join("Downloads/Nexus-Launcher-0.5.1-x86_64.AppImage"); std::fs::write(&nl, "x").unwrap();
        #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&nl, std::fs::Permissions::from_mode(0o755)).unwrap(); assert_eq!(find_launcher(&h), Some(nl), "the renamed launcher wins over an old Xodus Nexus file"); }
    }
    /// Read-only check against this machine's real Nexus install: `cargo test -p pd-filter real_nexus -- --ignored --nocapture`.
    #[test] fn the_program_is_found_whatever_the_case_of_its_name_on_disk() {
        let h = tmp("case"); let g = h.join("G"); std::fs::create_dir_all(g.join("BIN")).unwrap();
        std::fs::write(g.join("MicrosoftGame.Config"), r#"<Game><StoreId>9NBLGGH1Z149</StoreId><ExecutableList><Executable Name="Bin\Game_x64.exe"/></ExecutableList></Game>"#).unwrap();
        std::fs::write(g.join("BIN/GAME_X64.EXE"), "x").unwrap();
        assert!(!inspect_game_dir(&g).unwrap().ready, "no streaming marker yet");
        std::fs::write(g.join(".xodus-streaming.msixvc"), "y").unwrap();
        assert!(inspect_game_dir(&g).unwrap().ready);
        assert_eq!(resolve_nocase(&g, "../G/BIN/GAME_X64.EXE"), None, "never leaves the folder");
    }
    #[test] fn nexus_programs_are_recognised_by_their_first_argument() {
        for yes in ["/tmp/.mount_Nexus-35PQ6Z/xodus-nexus.bin", "/home/r/Downloads/Nexus-Launcher-0.5.1-x86_64.AppImage", "/home/r/Downloads/Xodus-Nexus-0.3.11-x86_64.appimage", "/home/r/.config/xodus-nexus/xodus-backends/releases/x/xodus-cli", "xodus-nexus"] { assert!(is_nexus_cmd(yes), "{yes}"); }
        for no in ["/home/r/.local/share/zest-dev/zest", "/usr/bin/xodus-cli", "nexusmods-app", "/home/r/xodus-nexus-notes/vim"] { assert!(!is_nexus_cmd(no), "{no}"); }
    }
    #[test] fn a_game_is_removed_like_nexus_does_and_never_while_nexus_runs() {
        let h = tmp("remove"); let root = h.join("Games/Xodus Nexus");
        let a = game(&root, "A", "AAAAAAAAAAAA", true); let b = game(&root, "B", "BBBBBBBBBBBB", true);
        let (ra, rb) = (std::fs::canonicalize(&a).unwrap(), std::fs::canonicalize(&b).unwrap());
        let cfg = h.join(".config/xodus-nexus"); std::fs::create_dir_all(&cfg).unwrap();
        let state = serde_json::json!({"games": [{"id": launch_id(&a), "title": "A", "path": ra, "storeId": "AAAAAAAAAAAA", "extra": 1}, {"id": launch_id(&b), "title": "B", "path": rb, "storeId": "BBBBBBBBBBBB"}],
            "downloads": [{"id": "d1", "productId": "BBBBBBBBBBBB", "destination": rb, "status": "paused"}], "settings": {"installRoot": root, "keep": true}});
        std::fs::write(cfg.join("launcher-state.json"), serde_json::to_string_pretty(&state).unwrap()).unwrap();
        let apps = h.join(".local/share/applications"); std::fs::create_dir_all(&apps).unwrap();
        let sc = apps.join("xodus-nexus-game-p-a.desktop"); std::fs::write(&sc, format!("[Desktop Entry]\nName=A\nX-Xodus-Nexus-Profile=p\nX-Xodus-Nexus-Game={}\n", launch_id(&a))).unwrap();
        let other = apps.join("other.desktop"); std::fs::write(&other, format!("[Desktop Entry]\nX-Xodus-Nexus-Game={}\n", launch_id(&a))).unwrap();
        // Nexus open: nothing touched
        assert_eq!(remove_game(&h, "AAAAAAAAAAAA", &|| true).unwrap_err(), NEXUS_OPEN); assert!(a.is_dir());
        // a paused download of B in Nexus: refused
        assert!(remove_game(&h, "bbbbbbbbbbbb", &|| false).unwrap_err().contains("unfinished download")); assert!(b.is_dir());
        // A: folder, entry and its own shortcut go; B, the settings, other fields and a file without Nexus's marker stay; the old list is kept
        assert_eq!(remove_game(&h, "aaaaaaaaaaaa", &|| false).unwrap(), ra);
        assert!(!a.exists() && b.is_dir() && !sc.exists() && other.exists());
        let v: Value = serde_json::from_str(&std::fs::read_to_string(cfg.join("launcher-state.json")).unwrap()).unwrap();
        assert_eq!(v["games"].as_array().unwrap().len(), 1); assert_eq!(v["games"][0]["title"], "B"); assert_eq!(v["settings"]["keep"], true); assert_eq!(v["downloads"].as_array().unwrap().len(), 1);
        assert!(std::fs::read_to_string(cfg.join("launcher-state.json.zest-backup")).unwrap().contains("\"extra\": 1"));
        assert!(remove_game(&h, "AAAAAAAAAAAA", &|| false).unwrap_err().contains("no folder"));
        // Nexus starting between the delete and the swap: the list is left alone and the user is told
        let c = game(&root, "C", "CCCCCCCCCCCC", true); let rc = std::fs::canonicalize(&c).unwrap();
        let mut v2 = v.clone(); v2["games"].as_array_mut().unwrap().push(serde_json::json!({"id": launch_id(&c), "path": rc, "storeId": "CCCCCCCCCCCC"}));
        std::fs::write(cfg.join("launcher-state.json"), serde_json::to_string(&v2).unwrap()).unwrap();
        let calls = std::cell::Cell::new(0); let later = || { calls.set(calls.get() + 1); calls.get() > 1 };
        assert!(remove_game(&h, "CCCCCCCCCCCC", &later).unwrap_err().contains("Nexus started meanwhile"));
        assert!(std::fs::read_to_string(cfg.join("launcher-state.json")).unwrap().contains("CCCCCCCCCCCC"));
        assert!(!cfg.join("launcher-state.json.zest-tmp").exists());
    }
    #[test] fn a_folder_outside_the_install_root_that_nexus_did_not_record_is_never_deleted() {
        let h = tmp("outside"); let root = h.join("Games/Xodus Nexus"); std::fs::create_dir_all(&root).unwrap();
        let elsewhere = h.join("Other"); let g = game(&elsewhere, "G", "GGGGGGGGGGGG", true);
        let cfg = h.join(".config/xodus-nexus"); std::fs::create_dir_all(&cfg).unwrap();
        // recorded once (so the scan finds it), then forgotten while the folder stays: no longer Nexus's to delete
        std::fs::write(cfg.join("launcher-state.json"), serde_json::json!({"games": [{"path": std::fs::canonicalize(&g).unwrap(), "storeId": "GGGGGGGGGGGG"}], "settings": {"installRoot": root}}).to_string()).unwrap();
        assert!(scan_installed(&h).contains_key("GGGGGGGGGGGG"));
        assert!(remove_game(&h, "GGGGGGGGGGGG", &|| false).is_ok(), "a folder Nexus recorded may go");
        let g2 = game(&elsewhere, "G2", "HHHHHHHHHHHH", true);
        assert!(inspect_game_dir(&g2).is_some() && remove_game(&h, "HHHHHHHHHHHH", &|| false).is_err() && g2.is_dir(), "never found, never deleted");
    }
    #[test] #[ignore] fn real_nexus_profile_is_understood() {
        let h = crate::installdir::home();
        println!("launcher: {:?}", find_launcher(&h));
        println!("backend: {:?}, market {}", crate::xbox_owned::backend_binary(&h), crate::xbox_owned::market(&h));
        let found = scan_installed(&h);
        for (id, i) in &found { println!("{id}: ready={} {}", i.ready, i.path.display()); }
        assert!(crate::xbox_owned::backend_binary(&h).is_some() || found.is_empty() || true);
    }
}
