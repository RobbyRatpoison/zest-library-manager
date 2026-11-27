//! Running Windows programs on Linux (port of PlayDate's `runners/proton.py` discovery and the launch half of `runners/wine.py`). Three ways, best first: `umu-run` with an installed
//! Proton (GE-Proton preferred; umu supplies the Steam Runtime container Proton needs, which is what made PlayDate's launches reliable), Proton's own `proton run` script (what
//! PlayDate's game plugins used before umu), a system Wine. The program is started by the app (`Action::Spawn`); this module only decides the command, the environment and the prefix.
//! Launcher installers, DXVK setup and running-session handling are not part of this port.
use crate::sources::Action;
use std::path::{Path, PathBuf};

pub const NO_WINE_MSG: &str = "No Wine or Proton found. Install a system Wine package, or GE-Proton (ProtonUp-Qt or Steam) together with umu-launcher.";

/// Folders holding community Proton builds, one directory per build with a `proton` script inside (relative to the home folder, or absolute).
const COMPAT_TOOL_DIRS: &[&str] = &[".local/share/Steam/compatibilitytools.d", ".steam/steam/compatibilitytools.d", ".steam/root/compatibilitytools.d",
    ".var/app/com.valvesoftware.Steam/.local/share/Steam/compatibilitytools.d", ".var/app/com.valvesoftware.Steam/data/Steam/compatibilitytools.d",
    "snap/steam/common/.local/share/Steam/compatibilitytools.d"];
/// System-wide folders (a distro may ship a Proton build there: CachyOS and Arch put `proton-cachyos` under `/usr/share/steam`).
pub const SYSTEM_COMPAT_DIRS: &[&str] = &["/usr/share/steam/compatibilitytools.d", "/usr/local/share/steam/compatibilitytools.d"];
/// Steam roots, whose `steamapps/common/Proton */` hold the official builds (also in extra libraries named by `libraryfolders.vdf`).
const STEAM_ROOTS: &[&str] = &[".local/share/Steam", ".steam/steam", ".var/app/com.valvesoftware.Steam/.local/share/Steam", ".var/app/com.valvesoftware.Steam/data/Steam", "snap/steam/common/.local/share/Steam"];
pub const WINE_DIRS: &[&str] = &["/usr/bin", "/usr/local/bin", "/opt/wine/bin", "/opt/wine-stable/bin", "/opt/wine-staging/bin", "/opt/wine-devel/bin", "/opt/homebrew/bin"];

#[derive(Clone, Debug, PartialEq)]
pub struct Proton { pub name: String, /// The `proton` script.
    pub path: PathBuf }

fn under(home: &Path, rel: &str) -> PathBuf { if rel.starts_with('/') { PathBuf::from(rel) } else { home.join(rel) } }

/// Extra Steam library folders named in a readable `libraryfolders.vdf`.
fn library_roots(home: &Path) -> Vec<PathBuf> {
    let re = regex::Regex::new(r#""path"\s+"([^"]+)""#).expect("fixed pattern");
    let mut out = vec![];
    for r in STEAM_ROOTS {
        if let Ok(t) = std::fs::read_to_string(under(home, r).join("config/libraryfolders.vdf")) { for m in re.captures_iter(&t) { out.push(PathBuf::from(m[1].replace("\\\\", "\\"))); } }
    }
    out
}

/// A name's parts for ordering: runs of digits compare as numbers, the rest as text, so `GE-Proton11-11` is newer than `GE-Proton11-5`.
fn natural(name: &str) -> Vec<(u8, u64, String)> {
    let (mut out, mut cur, mut digits) = (vec![], String::new(), false);
    let flush = |out: &mut Vec<(u8, u64, String)>, cur: &mut String, digits: bool| { if !cur.is_empty() { out.push(if digits { (0, cur.parse().unwrap_or(0), String::new()) } else { (1, 0, cur.clone()) }); cur.clear(); } };
    for c in name.to_lowercase().chars() {
        let d = c.is_ascii_digit();
        if d != digits { flush(&mut out, &mut cur, digits); digits = d; }
        cur.push(c);
    }
    flush(&mut out, &mut cur, digits);
    out
}

fn rank(p: &Proton) -> u8 {
    if p.name.to_lowercase().starts_with("ge-proton") { 0 } else if p.path.to_string_lossy().contains("/compatibilitytools.d/") { 1 } else { 2 }
}

/// `find_proton_versions`: every installed Proton, GE-Proton first, then other community builds, then official ones; newest first within each.
pub fn find_proton_versions(home: &Path, system: &[&str]) -> Vec<Proton> {
    let mut paths: Vec<PathBuf> = vec![];
    for d in COMPAT_TOOL_DIRS.iter().chain(system.iter()) { if let Ok(rd) = std::fs::read_dir(under(home, d)) { paths.extend(rd.flatten().map(|e| e.path().join("proton"))); } }
    let roots: Vec<PathBuf> = STEAM_ROOTS.iter().map(|r| under(home, r)).chain(library_roots(home)).collect();
    for root in roots {
        if let Ok(rd) = std::fs::read_dir(root.join("steamapps/common")) { paths.extend(rd.flatten().filter(|e| e.file_name().to_string_lossy().starts_with("Proton")).map(|e| e.path().join("proton"))); }
    }
    paths.retain(|p| p.is_file());
    paths.sort(); paths.dedup();
    let (mut seen, mut found) = (std::collections::HashSet::new(), vec![]);
    for p in paths {
        let real = std::fs::canonicalize(&p).unwrap_or_else(|_| p.clone());
        if !seen.insert(real) { continue; }
        found.push(Proton { name: p.parent().and_then(|d| d.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_default(), path: p });
    }
    found.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| natural(&b.name).cmp(&natural(&a.name))));
    found
}

/// A program by name on `PATH` (or an extra folder list), as an absolute path.
pub fn which(name: &str, path_env: &std::ffi::OsStr, extra: &[&str]) -> Option<PathBuf> {
    // inside a Flatpak the programs are on the host, not in the sandbox's own path
    #[cfg(not(test))] if crate::sandbox::in_flatpak() { return crate::sandbox::host_which(name); }
    std::env::split_paths(path_env).chain(extra.iter().map(PathBuf::from)).map(|d| d.join(name)).find(|p| p.is_file() && is_exec(p))
}
#[cfg(unix)] fn is_exec(p: &Path) -> bool { use std::os::unix::fs::PermissionsExt; std::fs::metadata(p).map_or(false, |m| m.permissions().mode() & 0o111 != 0) }
#[cfg(not(unix))] fn is_exec(_: &Path) -> bool { true }

/// `find_wine_binary`: `wine64`, else `wine`, on the path or in the usual distro folders.
pub fn find_wine(path_env: &std::ffi::OsStr, dirs: &[&str]) -> Option<PathBuf> {
    ["wine64", "wine"].iter().find_map(|n| which(n, path_env, dirs)).or_else(|| if cfg!(target_os = "macos") { crate::macwine::installed() } else { None })
}
pub fn find_umu(path_env: &std::ffi::OsStr) -> Option<PathBuf> { which("umu-run", path_env, &[]) }

#[derive(Clone, Debug, PartialEq)]
pub enum Runtime {
    /// `umu-run` with this Proton build (PROTONPATH is its folder).
    Umu { umu: PathBuf, proton: Proton },
    /// Proton's own `proton run`.
    ProtonScript { proton: Proton },
    Wine { wine: PathBuf },
}

/// Picks how to run Windows programs here; Err carries install guidance.
pub fn resolve(home: &Path, system: &[&str], wine_dirs: &[&str], path_env: &std::ffi::OsStr) -> Result<Runtime, String> {
    let proton = find_proton_versions(home, system).into_iter().find(|p| p.path.parent().map_or(false, |d| d.join("toolmanifest.vdf").is_file()));
    if let (Some(umu), Some(p)) = (find_umu(path_env), proton.clone()) { return Ok(Runtime::Umu { umu, proton: p }); }
    if let Some(p) = proton { return Ok(Runtime::ProtonScript { proton: p }); }
    if let Some(w) = find_wine(path_env, wine_dirs) { return Ok(Runtime::Wine { wine: w }); }
    Err(NO_WINE_MSG.into())
}

/// One line saying what Windows games will run with (for the Plugins dialog).
pub fn describe(home: &Path, system: &[&str], wine_dirs: &[&str], path_env: &std::ffi::OsStr) -> String {
    match resolve(home, system, wine_dirs, path_env) {
        Ok(Runtime::Umu { proton, .. }) => format!("Windows games run with umu-run and {}", proton.name),
        Ok(Runtime::ProtonScript { proton }) => format!("Windows games run with {} (install umu-launcher for a more reliable setup)", proton.name),
        Ok(Runtime::Wine { wine }) => format!("Windows games run with the system Wine ({})", wine.display()),
        Err(e) => e,
    }
}

/// The Steam folder above a Proton build (the one with `steamapps`), else the usual default.
fn steam_root(home: &Path, proton: &Path) -> PathBuf {
    let mut d = proton.parent();
    while let Some(p) = d { if p.join("steamapps").is_dir() { return p.to_path_buf(); } d = p.parent(); }
    [".steam/steam", ".local/share/Steam"].iter().map(|r| home.join(r)).find(|p| p.is_dir()).unwrap_or_else(|| home.join(".steam/steam"))
}

/// The command that runs `exe` (with `args`) in `prefix`, started in `cwd` (default: the program's own folder: games look for their data relative to it). The prefix folder is made.
pub fn spawn_action(rt: &Runtime, home: &Path, exe: &Path, args: &[String], cwd: Option<&Path>, prefix: &Path, wayland: bool) -> Result<Action, String> {
    std::fs::create_dir_all(prefix).map_err(|e| format!("Could not make the Wine prefix {}: {e}", prefix.display()))?;
    let dir = cwd.map(Path::to_path_buf).or_else(|| exe.parent().map(Path::to_path_buf)).map(|d| d.to_string_lossy().to_string());
    let s = |p: &Path| p.to_string_lossy().to_string();
    let set = |k: &str, v: String| (k.to_string(), Some(v));
    Ok(match rt {
        Runtime::Umu { umu, proton } => {
            let root = proton.path.parent().ok_or("bad Proton path")?;
            Action::Spawn { program: s(umu), args: std::iter::once(s(exe)).chain(args.iter().cloned()).collect(), dir,
                env: vec![set("WINEPREFIX", s(prefix)), set("WINEDEBUG", "-all".into()), set("GAMEID", "umu-default".into()), set("PROTONPATH", s(root))] }
        }
        Runtime::ProtonScript { proton } => Action::Spawn { program: s(&proton.path), args: ["run".to_string(), s(exe)].into_iter().chain(args.iter().cloned()).collect(), dir,
            env: vec![set("STEAM_COMPAT_DATA_PATH", s(prefix)), set("STEAM_COMPAT_CLIENT_INSTALL_PATH", s(&steam_root(home, &proton.path)))] },
        Runtime::Wine { wine } => {
            let mut env = vec![set("WINEPREFIX", s(prefix)), set("WINEDEBUG", "-all".into())];
            // on a Wayland session Wine chooses its native Wayland driver when DISPLAY is not set (under XWayland a window can be slow and miss clicks)
            if wayland { env.push(("DISPLAY".to_string(), None)); }
            Action::Spawn { program: s(wine), args: std::iter::once(s(exe)).chain(args.iter().cloned()).collect(), dir, env }
        }
    })
}

/// Wine prefixes found one level below each of `dirs` (a folder holding `drive_c` is a prefix), sorted by path.
pub fn list_prefixes(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = dirs.iter().filter_map(|d| std::fs::read_dir(d).ok()).flatten().flatten()
        .map(|e| e.path()).filter(|p| p.is_dir() && p.join("drive_c").is_dir()).collect();
    out.sort(); out
}

/// Running processes whose `WINEPREFIX` is this prefix or its `pfx` folder (Proton and umu keep the real prefix in `<prefix>/pfx`, so a game started that way is
/// only found by matching both). Walks a `/proc` tree; `(pid, first argv entry)`, which for a Wine-hosted process is the Windows-style path of its program.
pub fn prefix_processes_in(proc_root: &Path, prefix: &Path) -> Vec<(u32, String)> {
    let norm = |p: &str| p.trim_end_matches('/').to_string();
    let (a, b) = (norm(&prefix.to_string_lossy()), norm(&prefix.join("pfx").to_string_lossy()));
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(proc_root) else { return out };
    for e in rd.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else { continue };
        let Ok(env) = std::fs::read(e.path().join("environ")) else { continue };
        let Some(wp) = env.split(|&c| c == 0).find_map(|kv| kv.strip_prefix(b"WINEPREFIX=")) else { continue };
        let wp = norm(&String::from_utf8_lossy(wp));
        if wp != a && wp != b { continue; }
        let argv0 = std::fs::read(e.path().join("cmdline")).ok().and_then(|c| c.split(|&x| x == 0).next().map(|s| String::from_utf8_lossy(s).to_string())).unwrap_or_default();
        out.push((pid, argv0));
    }
    out.sort(); out
}

pub fn prefix_processes(prefix: &Path) -> Vec<(u32, String)> { prefix_processes_in(Path::new("/proc"), prefix) }
pub fn prefix_has_running_process(prefix: &Path) -> bool { !prefix_processes(prefix).is_empty() }

/// One lock per prefix path: the check-then-launch (or kill-then-relaunch) steps must not race, or three quick clicks start three containers on one prefix.
pub fn prefix_lock(prefix: &Path) -> std::sync::Arc<std::sync::Mutex<()>> {
    use std::sync::{Arc, Mutex, OnceLock};
    static LOCKS: OnceLock<Mutex<std::collections::HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    let key = std::path::absolute(prefix).unwrap_or_else(|_| prefix.to_path_buf());
    LOCKS.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner()).entry(key).or_default().clone()
}

/// Ends every process of the prefix's Wine session with `wineserver -k -w` (waits for the server to exit), then confirms nothing is left. Needed before deleting or
/// rebuilding a prefix (a live wineserver holding files open leaves it corrupt) and before a fresh umu-run launch against a prefix that has a live session
/// (umu-run cannot safely join one). `wineserver` is looked for beside the given Wine, then on PATH.
pub fn end_prefix_session(prefix: &Path, wine: Option<&Path>) -> Result<(), String> {
    let beside = wine.and_then(|w| w.parent()).map(|d| d.join("wineserver")).filter(|p| p.is_file());
    let server = beside.unwrap_or_else(|| PathBuf::from("wineserver"));
    match std::process::Command::new(&server).args(["-k", "-w"]).env("WINEPREFIX", prefix).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status() {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => { if !prefix_has_running_process(prefix) { return Ok(()); } return Err("wineserver was not found, so the running Wine session could not be ended".into()); }
        Err(e) => return Err(format!("wineserver failed: {e}")),
    }
    for _ in 0..10 {
        if !prefix_has_running_process(prefix) { return Ok(()); }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    Err(format!("the Wine session for {} still shows processes after wineserver -k -w", prefix.display()))
}

// Tests choose the runtime per thread instead of looking at the machine.
#[cfg(test)] thread_local! { pub static TEST_RUNTIME: std::cell::RefCell<Option<Result<Runtime, String>>> = const { std::cell::RefCell::new(None) }; }

/// The Windows runtime this machine would use (umu-run with Proton, `proton run`, or Wine), or why there is none.
pub fn resolve_system() -> Result<Runtime, String> {
    resolve(&crate::installdir::home(), SYSTEM_COMPAT_DIRS, WINE_DIRS, &std::env::var_os("PATH").unwrap_or_default())
}

/// `spawn_action` for the real system: resolves the runtime, reads the environment.
pub fn launch_action(exe: &Path, prefix: &Path) -> Result<Action, String> { launch_action_args(exe, prefix, &[]) }
/// `launch_action` with program arguments (a launcher told what to install, a game told it came from its launcher).
pub fn launch_action_args(exe: &Path, prefix: &Path, args: &[String]) -> Result<Action, String> { launch_action_full(exe, prefix, args, None, &[]) }
/// `launch_action_args` with a working folder and extra environment (a game that reads its own variables).
pub fn launch_action_full(exe: &Path, prefix: &Path, args: &[String], cwd: Option<&Path>, extra_env: &[(String, String)]) -> Result<Action, String> {
    let home = crate::installdir::home();
    let path = std::env::var_os("PATH").unwrap_or_default();
    #[cfg(test)] let rt = TEST_RUNTIME.with(|t| t.borrow().clone()).unwrap_or_else(|| resolve(&home, SYSTEM_COMPAT_DIRS, WINE_DIRS, &path))?;
    #[cfg(not(test))] let rt = resolve(&home, SYSTEM_COMPAT_DIRS, WINE_DIRS, &path)?;
    // a second umu-run against a prefix with a live session waits on its lock forever instead of joining it: say so rather than hang
    let _guard = prefix_lock(prefix); let _g = _guard.lock().unwrap_or_else(|e| e.into_inner());
    if matches!(rt, Runtime::Umu { .. }) && prefix_has_running_process(prefix) {
        return Err("Something is already running in this game's Wine prefix. Close it first (a second launch cannot join a running session).".into());
    }
    let mut a = spawn_action(&rt, &home, exe, args, cwd, prefix, std::env::var_os("WAYLAND_DISPLAY").is_some())?;
    if let Action::Spawn { env, .. } = &mut a { for (k, v) in extra_env { env.push((k.clone(), Some(v.clone()))); } }
    Ok(a)
}

/// Hands a program or link to a Wine session that is already running in the prefix with the session's own Proton Wine run directly (no container: a second `umu-run` cannot
/// join a live session). Checked by hand against a live Battle.net session; the system Wine is refused by it (version mismatch).
pub fn session_action(exe: &Path, prefix: &Path, args: &[String]) -> Result<Action, String> {
    let home = crate::installdir::home();
    let path = std::env::var_os("PATH").unwrap_or_default();
    // the session's own Wine: a client of another version is refused by its wineserver ("version mismatch"), so the Proton build that started it is used when there is one
    let proton_wine = match resolve(&home, SYSTEM_COMPAT_DIRS, WINE_DIRS, &path) {
        Ok(Runtime::Umu { proton, .. }) | Ok(Runtime::ProtonScript { proton }) => proton.path.parent().and_then(|d| ["files/bin", "dist/bin"].iter().map(|b| d.join(b).join("wine")).find(|w| w.is_file())),
        _ => None,
    };
    // tests set the runtime themselves, so they do not depend on a Wine being installed on the machine running them
    #[cfg(test)] let proton_wine = proton_wine.or_else(|| match TEST_RUNTIME.with(|t| t.borrow().clone()) { Some(Ok(Runtime::Wine { wine })) => Some(wine), _ => None });
    let wine = proton_wine.or_else(|| find_wine(&path, WINE_DIRS)).ok_or("Wine is not installed, so Zest cannot hand the launch to the running Battle.net.")?;
    // a bare program name (`start`) has no folder of its own: the prefix is used (an empty folder made the start fail with "No such file or directory")
    let cwd = if exe.is_absolute() { None } else { Some(prefix) };
    spawn_action(&Runtime::Wine { wine }, &home, exe, args, cwd, prefix, std::env::var_os("WAYLAND_DISPLAY").is_some())
}

/// Like `launch_action_args`, but a live session in the prefix is ended first (the client of a store whose window cannot be told anything while it runs), unless `busy` says one of
/// its processes is a game that must not be killed. For the Battle.net client: its window closes to the tray and the session stays alive, so a launch would otherwise be refused.
pub fn launch_action_restarting(exe: &Path, prefix: &Path, args: &[String], busy: &dyn Fn(&[(u32, String)]) -> Option<String>) -> Result<Action, String> {
    let home = crate::installdir::home();
    let path = std::env::var_os("PATH").unwrap_or_default();
    let rt = resolve(&home, SYSTEM_COMPAT_DIRS, WINE_DIRS, &path)?;
    {
        let _guard = prefix_lock(prefix); let _g = _guard.lock().unwrap_or_else(|e| e.into_inner());
        if prefix_has_running_process(prefix) {
            if let Some(why) = busy(&prefix_processes(prefix)) { return Err(why); }
            let wine = match &rt { Runtime::Umu { proton, .. } | Runtime::ProtonScript { proton } => proton.path.parent().and_then(|d| ["files/bin", "dist/bin"].iter().map(|b| d.join(b).join("wine64")).find(|w| w.parent().map_or(false, |b| b.join("wineserver").is_file()))), _ => None };
            end_prefix_session(prefix, wine.as_deref())?;
        }
    }
    launch_action_args(exe, prefix, args)
}

/// Opens a Windows protocol link (`uplay://launch/54/0`) in the prefix with `wine start <url>`. With a session already live: `restart_if_running` (a launcher whose window will not
/// take a link while it runs, Ubisoft Connect) ends the session first and starts fresh, unless `busy` says one of its processes is a game that must not be killed; a plain
/// Wine session is signalled directly; otherwise a second container could not join it, so the caller is told to close it.
pub fn protocol_action(url: &str, prefix: &Path, restart_if_running: bool, extra_env: &[(&str, &str)], busy: &dyn Fn(&[(u32, String)]) -> Option<String>) -> Result<Action, String> {
    let home = crate::installdir::home();
    let path = std::env::var_os("PATH").unwrap_or_default();
    #[cfg(test)] let rt = TEST_RUNTIME.with(|t| t.borrow().clone()).unwrap_or_else(|| resolve(&home, SYSTEM_COMPAT_DIRS, WINE_DIRS, &path))?;
    #[cfg(not(test))] let rt = resolve(&home, SYSTEM_COMPAT_DIRS, WINE_DIRS, &path)?;
    let _guard = prefix_lock(prefix); let _g = _guard.lock().unwrap_or_else(|e| e.into_inner());
    if prefix_has_running_process(prefix) && !matches!(rt, Runtime::Wine { .. }) {
        if !restart_if_running { return Err("Something is already running in this Wine prefix. Close it first (a second launch cannot join a running session).".into()); }
        if let Some(why) = busy(&prefix_processes(prefix)) { return Err(why); }
        let wine = match &rt { Runtime::Umu { proton, .. } | Runtime::ProtonScript { proton } => proton.path.parent().and_then(|d| ["files/bin", "dist/bin"].iter().map(|b| d.join(b).join("wine64")).find(|w| w.parent().map_or(false, |b| b.join("wineserver").is_file()))), _ => None };
        end_prefix_session(prefix, wine.as_deref())?;
    }
    let mut a = spawn_action(&rt, &home, Path::new("start"), &[url.to_string()], Some(prefix), prefix, std::env::var_os("WAYLAND_DISPLAY").is_some())?;
    if let Action::Spawn { env, .. } = &mut a { for (k, v) in extra_env { env.push((k.to_string(), Some(v.to_string()))); } }
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tmp(tag: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("zest_wine_{tag}_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d }
    fn proton(home: &Path, rel: &str, manifest: bool) -> Proton { let d = home.join(rel); std::fs::create_dir_all(&d).unwrap(); std::fs::write(d.join("proton"), "#!/bin/sh\n").unwrap(); if manifest { std::fs::write(d.join("toolmanifest.vdf"), "x").unwrap(); } Proton { name: d.file_name().unwrap().to_string_lossy().to_string(), path: d.join("proton") } }
    fn exe_in(dir: &Path, name: &str) -> PathBuf { std::fs::create_dir_all(dir).unwrap(); let p = dir.join(name); std::fs::write(&p, "#!/bin/sh\n").unwrap(); #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap(); } p }

    #[test] fn handing_a_link_to_a_running_session_runs_in_the_prefix_not_in_an_empty_folder() {
        // `start` has no folder of its own: running "in" its empty parent made the spawn fail with "No such file or directory"
        let p = tmp("sess");
        if let Ok(Action::Spawn { dir, args, .. }) = session_action(Path::new("start"), &p, &["battlenet://Pro".to_string()]) {
            assert_eq!(dir, Some(p.to_string_lossy().to_string()));
            assert_eq!(args[0], "start");
        }   // no Wine on this machine: nothing to check
    }
    #[test] fn newer_versions_sort_first_by_number_not_by_letters() {
        assert!(natural("GE-Proton10-34") > natural("GE-Proton10-9") && natural("GE-Proton10-9") > natural("GE-Proton10-4") && natural("GE-Proton10-4") > natural("GE-Proton9-10"));
        assert!(natural("GE-Proton11-11-x86_64") > natural("GE-Proton11-5-x86_64") && natural("GE-Proton11-5-x86_64") > natural("GE-Proton11-3"));
    }
    #[test] fn runtime_choice_prefers_umu_with_proton_then_the_proton_script_then_wine_and_explains_when_there_is_none() {
        let home = tmp("rt"); let bin = tmp("bin"); let empty = tmp("emptybin");
        assert_eq!(resolve(&home, &[], &[], empty.as_os_str()), Err(NO_WINE_MSG.into()));
        let wine = exe_in(&bin, "wine");
        assert_eq!(resolve(&home, &[], &[], bin.as_os_str()), Ok(Runtime::Wine { wine: wine.clone() }));
        let ge = proton(&home, ".local/share/Steam/compatibilitytools.d/GE-Proton10-4", true);
        let _ = proton(&home, ".local/share/Steam/compatibilitytools.d/GE-Proton10-34", false);   // no toolmanifest: not a usable build
        assert_eq!(resolve(&home, &[], &[], bin.as_os_str()), Ok(Runtime::ProtonScript { proton: ge.clone() }));
        let umu = exe_in(&bin, "umu-run");
        assert_eq!(resolve(&home, &[], &[], bin.as_os_str()), Ok(Runtime::Umu { umu: umu.clone(), proton: ge }));
        assert!(describe(&home, &[], &[], bin.as_os_str()).starts_with("Windows games run with umu-run and GE-Proton10-4"));
    }
    #[test] fn each_runtime_builds_its_own_command_environment_and_prefix() {
        let home = tmp("cmd"); let prefix = home.join("pfx/42"); let exe = home.join("g/Game.exe");
        let p = proton(&home, ".steam/steam/compatibilitytools.d/GE-Proton10-4", true);
        std::fs::create_dir_all(home.join(".steam/steam/steamapps")).unwrap();
        let get = |a: &Action| match a { Action::Spawn { program, args, dir, env } => (program.clone(), args.clone(), dir.clone(), env.clone()), _ => panic!() };
        let (prog, args, dir, env) = get(&spawn_action(&Runtime::Umu { umu: "/usr/bin/umu-run".into(), proton: p.clone() }, &home, &exe, &["-w".into()], None, &prefix, true).unwrap());
        assert_eq!((prog.as_str(), args, dir), ("/usr/bin/umu-run", vec![exe.to_string_lossy().to_string(), "-w".into()], Some(home.join("g").to_string_lossy().to_string())));
        assert!(env.contains(&("WINEPREFIX".into(), Some(prefix.to_string_lossy().to_string()))) && env.contains(&("GAMEID".into(), Some("umu-default".into()))));
        assert!(env.contains(&("PROTONPATH".into(), Some(p.path.parent().unwrap().to_string_lossy().to_string()))));
        assert!(prefix.is_dir(), "the prefix folder is made");
        let (prog, args, _, env) = get(&spawn_action(&Runtime::ProtonScript { proton: p.clone() }, &home, &exe, &[], None, &prefix, true).unwrap());
        assert_eq!((prog, args[0].as_str()), (p.path.to_string_lossy().to_string(), "run"));
        assert!(env.contains(&("STEAM_COMPAT_DATA_PATH".into(), Some(prefix.to_string_lossy().to_string()))));
        assert!(env.iter().any(|(k, v)| k == "STEAM_COMPAT_CLIENT_INSTALL_PATH" && v.as_deref() == Some(home.join(".steam/steam").to_string_lossy().as_ref())));
        let (prog, _, _, env) = get(&spawn_action(&Runtime::Wine { wine: "/usr/bin/wine".into() }, &home, &exe, &[], None, &prefix, true).unwrap());
        assert_eq!(prog, "/usr/bin/wine"); assert!(env.contains(&("DISPLAY".into(), None)), "Wayland: native driver");
        let (_, _, _, env) = get(&spawn_action(&Runtime::Wine { wine: "/usr/bin/wine".into() }, &home, &exe, &[], None, &prefix, false).unwrap());
        assert!(!env.iter().any(|(k, _)| k == "DISPLAY"));
    }
    #[test] fn prefixes_are_folders_with_drive_c_and_processes_match_the_prefix_or_its_pfx() {
        let root = tmp("pfxs"); for d in ["a/drive_c", "b/other", "c/drive_c"] { std::fs::create_dir_all(root.join(d)).unwrap(); }
        assert_eq!(list_prefixes(&[root.clone(), root.join("missing")]), vec![root.join("a"), root.join("c")]);
        let proc = tmp("proc"); let pre = root.join("a");
        let mk = |pid: &str, env: &str, cmd: &str| { let d = proc.join(pid); std::fs::create_dir_all(&d).unwrap(); std::fs::write(d.join("environ"), env).unwrap(); std::fs::write(d.join("cmdline"), cmd).unwrap(); };
        mk("10", &format!("A=1\0WINEPREFIX={}\0", pre.display()), "C:\\launcher.exe\0--x\0");
        mk("11", &format!("WINEPREFIX={}/pfx/\0", pre.display()), "C:\\game.exe\0");
        mk("12", &format!("WINEPREFIX={}\0", root.join("c").display()), "other\0");
        mk("13", "PATH=/bin\0", "sh\0"); std::fs::create_dir_all(proc.join("self")).unwrap();
        assert_eq!(prefix_processes_in(&proc, &pre), vec![(10, "C:\\launcher.exe".to_string()), (11, "C:\\game.exe".to_string())]);
        assert!(prefix_processes_in(&proc, &root.join("b")).is_empty() && prefix_processes_in(&proc.join("nope"), &pre).is_empty());
    }
    #[test] fn one_lock_per_prefix_path() {
        let a = prefix_lock(Path::new("/tmp/zest_lock_a")); let b = prefix_lock(Path::new("/tmp/zest_lock_a")); let c = prefix_lock(Path::new("/tmp/zest_lock_b"));
        assert!(std::sync::Arc::ptr_eq(&a, &b) && !std::sync::Arc::ptr_eq(&a, &c));
    }
}
