//! Port of PlayDate's `runners/installdir.py` (the games folder a store's downloads live in) plus the default folders its plugins used.
//! The setting is `config.json[plugin]["games_dir"]`, as before.
use std::path::{Path, PathBuf};

pub fn home() -> PathBuf { std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_default() }

/// The default games folder of a store: `~/Games/<linux>` on Linux, `<other>` in Zest's per-user data folder elsewhere (see `user_data`).
pub fn default_dir(linux: &str, other: &str) -> PathBuf {
    if cfg!(windows) || cfg!(target_os = "macos") { user_data(other) } else { home().join("Games").join(linux) }
}

/// The per-user data folder named `name`: `%LOCALAPPDATA%` on Windows, `~/Library/Application Support` on a Mac, `~/.local/share` on Linux.
fn data_root(name: &str) -> PathBuf {
    if cfg!(windows) { PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_else(|| home().into_os_string())).join(name) }
    else if cfg!(target_os = "macos") { home().join("Library/Application Support").join(name) }
    else { home().join(".local/share").join(name) }
}

/// Where Zest keeps `sub` (a store's Wine prefix, its games on Windows and Mac): in the `Zest` data folder, except when PlayDate already made `sub` in its
/// `PlayDate` folder, which keeps being used (its launchers and games stay where they are; nothing is moved).
pub fn user_data(sub: &str) -> PathBuf { user_data_in(&data_root("PlayDate"), &data_root("Zest"), sub) }
pub fn user_data_in(old: &Path, new: &Path, sub: &str) -> PathBuf { let o = old.join(sub); if o.exists() { o } else { new.join(sub) } }

/// `get_install_dir`: the configured folder, else `default`.
pub fn games_dir(config_dir: &Path, plugin: &str, default: PathBuf) -> PathBuf {
    std::fs::read_to_string(config_dir.join("config.json")).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|c| c.get(plugin)?.get("games_dir")?.as_str().filter(|s| !s.is_empty()).map(PathBuf::from)).unwrap_or(default)
}

/// Sets (or, with an empty `path`, resets to the default) the games folder of `plugin` in `config.json`. The folder is created and must be writable; `~` means the
/// home folder; a relative path is refused (it would mean something different next time Zest starts from another place). Returns the folder now in use, or `None` for the default.
pub fn set_games_dir(config_dir: &Path, plugin: &str, path: &str) -> Result<Option<PathBuf>, String> {
    let path = path.trim();
    let chosen = if path.is_empty() { None } else {
        let p = if let Some(rest) = path.strip_prefix("~/").or_else(|| if path == "~" { Some("") } else { None }) { home().join(rest) } else { PathBuf::from(path) };
        if !p.is_absolute() { return Err("Give the full path of the folder (starting with / or a drive letter, or ~/).".into()); }
        std::fs::create_dir_all(&p).map_err(|e| format!("Cannot create {}: {e}", p.display()))?;
        let probe = p.join(format!(".zest-write-test-{}", std::process::id()));
        std::fs::write(&probe, b"x").map_err(|e| format!("Cannot write in {}: {e}", p.display()))?;
        let _ = std::fs::remove_file(&probe);
        Some(p)
    };
    let _g = crate::account::config_lock();
    let file = config_dir.join("config.json");
    let mut cfg: serde_json::Value = std::fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_else(|| serde_json::json!({}));
    let o = cfg.as_object_mut().ok_or("config.json is not an object")?;
    let sec = o.entry(plugin.to_string()).or_insert_with(|| serde_json::json!({}));
    let sec = sec.as_object_mut().ok_or("that plugin's settings are not an object")?;
    match &chosen { Some(p) => { sec.insert("games_dir".into(), serde_json::json!(p.to_string_lossy())); } None => { sec.remove("games_dir"); } }
    crate::account::write_json(&file, &cfg).map_err(|e| format!("Could not save the setting: {e}"))?;
    Ok(chosen)
}

/// Is `path` inside `dir` (compared as written, after resolving `.` and `..` lexically; no symlink following, so a link cannot make a folder look inside)?
pub fn is_inside(path: &Path, dir: &Path) -> bool {
    fn norm(p: &Path) -> Vec<std::ffi::OsString> { let mut out: Vec<std::ffi::OsString> = vec![]; for c in p.components() { use std::path::Component as C; match c { C::ParentDir => { out.pop(); } C::CurDir => {} o => out.push(o.as_os_str().to_os_string()) } } out }
    let (p, d) = (norm(path), norm(dir));
    !d.is_empty() && p.len() > d.len() && p[..d.len()] == d[..]
}

/// `check_disk_space`: Err when the disk holding `path` lacks room for `needed` bytes plus headroom (5% or 2 GB, whichever is more): a full disk misbehaves system-wide.
pub fn check_disk_space(path: &Path, needed: u64) -> Result<(), String> {
    let Some(free) = free_bytes(path) else { return Ok(()) };   // cannot tell: do not block
    let margin = (needed / 20).max(2 * 1024 * 1024 * 1024);
    if free < needed + margin { return Err(format!("Not enough disk space: need {:.1} GB, only {:.1} GB free", (needed + margin) as f64 / 1e9, free as f64 / 1e9)); }
    Ok(())
}
#[cfg(unix)] fn free_bytes(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let mut p = path; while !p.exists() { p = p.parent()?; }
    let c = std::ffi::CString::new(p.as_os_str().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    (unsafe { libc::statvfs(c.as_ptr(), &mut st) } == 0).then(|| st.f_bavail as u64 * st.f_frsize as u64)
}
#[cfg(windows)] fn free_bytes(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" { fn GetDiskFreeSpaceExW(dir: *const u16, available_to_caller: *mut u64, total: *mut u64, total_free: *mut u64) -> i32; }
    let mut p = path; while !p.exists() { p = p.parent()?; }
    let wide: Vec<u16> = p.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut avail = 0u64;
    (unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut avail, std::ptr::null_mut(), std::ptr::null_mut()) } != 0).then_some(avail)
}
#[cfg(not(any(unix, windows)))] fn free_bytes(_: &Path) -> Option<u64> { None }

/// `check_writable`: makes the folder and proves a file can be written in it (a permission bit alone can mislead on network drives).
pub fn check_writable(path: &Path) -> Result<(), String> {
    std::fs::create_dir_all(path).map_err(|_| "That folder could not be created.".to_string())?;
    let probe = path.join(".zest_write_test");
    std::fs::write(&probe, b"x").map_err(|_| "That folder is not writable.".to_string())?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn the_games_folder_can_be_set_and_reset() {
        let d = std::env::temp_dir().join(format!("zest_gd_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("config.json"), r#"{"itch_io":{"api_key":"K"},"other":1}"#).unwrap();
        let target = d.join("my games/itch");
        assert_eq!(set_games_dir(&d, "itch_io", &target.to_string_lossy()).unwrap(), Some(target.clone()));
        assert!(target.is_dir() && std::fs::read_dir(&target).unwrap().next().is_none(), "created, probe removed");
        assert_eq!(games_dir(&d, "itch_io", "/def".into()), target);
        let cfg: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(d.join("config.json")).unwrap()).unwrap();
        assert_eq!((cfg["itch_io"]["api_key"].as_str(), cfg["other"].as_i64()), (Some("K"), Some(1)));
        assert!(set_games_dir(&d, "itch_io", "relative/folder").is_err());
        assert!(set_games_dir(&d, "itch_io", "/proc/definitely/not/writable").is_err());
        assert_eq!(set_games_dir(&d, "itch_io", "  ").unwrap(), None);
        assert_eq!(games_dir(&d, "itch_io", "/def".into()), PathBuf::from("/def"));
        // a plugin with no section yet gets one
        set_games_dir(&d, "indiegala", &d.join("ig").to_string_lossy()).unwrap();
        assert_eq!(games_dir(&d, "indiegala", "/def".into()), d.join("ig"));
    }
    #[test] fn writable_and_space_checks() {
        let d = std::env::temp_dir().join(format!("zest_idir_w_{}", std::process::id()));
        assert!(check_writable(&d.join("new/sub")).is_ok() && !d.join("new/sub/.zest_write_test").exists());
        assert!(check_disk_space(&d, 1000).is_ok());
        assert!(check_disk_space(&d, u64::MAX / 4).unwrap_err().contains("Not enough disk space"));
        #[cfg(unix)] assert!(check_writable(Path::new("/proc/zest/x")).is_err());   // /proc does not exist (and the path would resolve under the drive root) elsewhere
    }
    #[test] fn inside_means_below_and_not_equal_and_not_through_dotdot() {
        let d = Path::new("/home/r/Games/itch");
        assert!(is_inside(Path::new("/home/r/Games/itch/Game_1"), d));
        assert!(!is_inside(Path::new("/home/r/Games/itch"), d));
        assert!(!is_inside(Path::new("/home/r/Games/itchy/x"), d));
        assert!(!is_inside(Path::new("/home/r/Games/itch/../Other"), d));
        assert!(is_inside(Path::new("/home/r/Games/itch/a/../b"), d));
    }
    #[test] fn the_configured_folder_wins_and_an_empty_one_does_not() {
        let d = std::env::temp_dir().join(format!("zest_idir_{}", std::process::id())); std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("config.json"), r#"{"itch_io":{"games_dir":"/data/itch"},"indiegala":{"games_dir":""}}"#).unwrap();
        assert_eq!(games_dir(&d, "itch_io", "/def".into()), PathBuf::from("/data/itch"));
        assert_eq!(games_dir(&d, "indiegala", "/def".into()), PathBuf::from("/def"));
        assert_eq!(games_dir(&d, "other", "/def".into()), PathBuf::from("/def"));
    }
}

#[cfg(test)]
mod user_data_tests {
    #[test] fn a_folder_playdate_already_made_keeps_being_used_and_anything_new_goes_to_zest() {
        let d = std::env::temp_dir().join(format!("zest_userdata_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d);
        let (old, new) = (d.join("PlayDate"), d.join("Zest"));
        std::fs::create_dir_all(old.join("battle_net/drive_c")).unwrap();
        assert_eq!(super::user_data_in(&old, &new, "battle_net"), old.join("battle_net"));
        assert_eq!(super::user_data_in(&old, &new, "ubisoft"), new.join("ubisoft"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
