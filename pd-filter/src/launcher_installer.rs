//! Installing a store's launcher into a Wine prefix (PlayDate's `runners/launcher_installer.py`), for the stores whose games need their own launcher (Rockstar, Legacy Games,
//! EA App, Ubisoft Connect, Battle.net, Epic). Steps: make the prefix (`wineboot --init`) and set the Windows version, install winetricks packages, write config files the
//! launcher should find on first run, download the installer, run it (or extract it), check the launcher's program is there, copy a few DLLs from Proton, and remember the
//! prefix in `config.json["launchers"][id]` (where every launcher-based source looks for it). Linux only: elsewhere the launcher installs normally.
//!
//! Nothing here spawns by itself: every program goes through the `Runner` (the real one runs processes, tests give a fake), and the download through `Http`.
use crate::net::{Cancel, Http};
use crate::wine::{Runtime, spawn_action};
use crate::sources::Action;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// What `plugin.json`'s `launcher.installer` held.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InstallerCfg {
    pub name: &'static str, pub url: &'static str, /// `exe`, `msi` or `extract`.
    pub kind: &'static str, pub winearch: &'static str, pub win_version: &'static str, pub winetricks: &'static [&'static str],
    pub env: &'static [(&'static str, &'static str)], /// The launcher's program, to check the install worked.
    pub exe_name: &'static str, pub post_install_dlls: &'static [&'static str],
    /// (path inside the user profile, content).
    pub post_install_files: &'static [(&'static str, &'static str)], /// `extract` only: where in `drive_c`.
    pub install_path: &'static str, /// The installer never exits (it turns into the launcher): finish when the program shows up.
    pub detached: bool,
}

const VKD3D: &[&str] = &["lib/vkd3d/x86_64-windows/libvkd3d-1.dll", "lib/vkd3d/x86_64-windows/libvkd3d-shader-1.dll"];
const UBISOFT_SETTINGS: &str = "overlay:\n  enabled: false\n  forceunhookgame: false\n  fps_enabled: false\n  warning_enabled: false\nuser:\n  closebehavior: CloseBehavior_Close";

/// The launcher installer for a store, from the stores' `plugin.json` files.
pub fn installer_for(platform: &str) -> Option<InstallerCfg> {
    let base = InstallerCfg { kind: "exe", winearch: "win64", ..Default::default() };
    Some(match platform {
        "battle_net" => InstallerCfg { name: "Battle.net", url: "https://www.battle.net/download/getInstallerForGame?os=win&locale=enUS&version=LIVE&gameProgram=BATTLENET_APP", win_version: "win10", winetricks: &["vcrun2022", "corefonts"], exe_name: "Battle.net.exe", detached: true, ..base },
        "ea_app" => InstallerCfg { name: "EA App", url: "https://origin-a.akamaihd.net/EA-Desktop-Client-Download/installer-releases/EAappInstaller.exe", win_version: "win10", winetricks: &["vcrun2022", "d3dcompiler_47", "corefonts"], env: &[("EAX_LAUNCH_CLIENT", "0")], exe_name: "EALauncher.exe", post_install_dlls: VKD3D, ..base },
        "epic_games" => InstallerCfg { name: "Epic Games Launcher", url: "https://launcher-public-service-prod06.ol.epicgames.com/launcher/api/installer/download/EpicGamesLauncherInstaller.msi", kind: "msi", exe_name: "EpicGamesLauncher.exe", post_install_dlls: VKD3D, ..base },
        "legacy_games" => InstallerCfg { name: "Legacy Games Launcher", url: "https://cdn.legacygames.com/LegacyGamesLauncher/legacy-games-launcher-setup-1.16.7-x64-full.exe", win_version: "win10", exe_name: "Legacy Games Launcher.exe", detached: true, ..base },
        "rockstar" => InstallerCfg { name: "Rockstar Games Launcher", url: "https://gamedownloads-rockstargames-com.akamaized.net/public/installer/Rockstar-Games-Launcher.exe", win_version: "win10", winetricks: &["vcrun2019", "corefonts"], exe_name: "Launcher.exe", detached: true, ..base },
        "ubisoft" => InstallerCfg { name: "Ubisoft Connect", url: "https://ubistatic3-a.akamaihd.net/orbit/launcher_installer/UbisoftConnectInstaller.exe", win_version: "win10", winetricks: &["d3dcompiler_43", "corefonts"], exe_name: "UbisoftConnect.exe",
            post_install_files: &[("AppData/Local/Ubisoft Game Launcher/settings.yaml", UBISOFT_SETTINGS)], install_path: "Program Files (x86)\\Ubisoft\\Ubisoft Game Launcher", detached: true, ..base },
        _ => return None,
    })
}

// ---------------------------------------------------------------- running programs

#[derive(Clone, Debug, PartialEq)]
pub struct Cmd { pub program: String, pub args: Vec<String>, pub env: Vec<(String, Option<String>)>, pub dir: Option<String> }

impl Cmd {
    fn from_action(a: Action) -> Result<Cmd, String> { match a { Action::Spawn { program, args, dir, env } => Ok(Cmd { program, args, env, dir }), _ => Err("not a program".into()) } }
}

/// How a program ended.
#[derive(Clone, Debug, PartialEq)]
pub enum Ended { Exit(i32), /// Still running when `stop` said enough, or the time ran out.
    Left, Failed(String) }

pub trait Runner: Send + Sync {
    /// Runs `cmd` with its output in the two files and waits; `stop` is polled every `poll` and ends the wait early (the program is left running) when it says true.
    fn run(&self, cmd: &Cmd, out: &Path, err: &Path, limit: Duration, poll: Duration, stop: &mut dyn FnMut() -> bool) -> Ended;
}

pub struct RealRunner;
impl Runner for RealRunner {
    fn run(&self, cmd: &Cmd, out: &Path, err: &Path, limit: Duration, poll: Duration, stop: &mut dyn FnMut() -> bool) -> Ended {
        // output goes to files, not pipes: an installer that hands off to a background service leaves that service holding a pipe open, and reading it would never end
        let (Ok(o), Ok(e)) = (std::fs::File::create(out), std::fs::File::create(err)) else { return Ended::Failed("could not make the log files".into()) };
        let (program, args) = crate::sandbox::wrap(&cmd.program, &cmd.args, cmd.dir.as_deref(), &cmd.env, crate::sandbox::in_flatpak());
        let mut c = std::process::Command::new(&program);
        c.args(&args).stdout(o).stderr(e).stdin(std::process::Stdio::null());
        // in a sandbox the environment and folder travel in the wrapped arguments; the same settings on flatpak-spawn itself would not reach the host program
        if !crate::sandbox::in_flatpak() {
            for (k, v) in &cmd.env { match v { Some(v) => { c.env(k, v); } None => { c.env_remove(k); } } }
            if let Some(d) = &cmd.dir { c.current_dir(d); }
        }
        let mut child = match c.spawn() { Ok(c) => c, Err(x) => return Ended::Failed(format!("{}: {x}", cmd.program)) };
        let start = Instant::now();
        loop {
            match child.try_wait() { Ok(Some(s)) => return Ended::Exit(s.code().unwrap_or(-1)), Ok(None) => {}, Err(x) => return Ended::Failed(x.to_string()) }
            // left running: something still collects it when it ends, so it never lingers as a finished-but-uncollected process
            if start.elapsed() >= limit || stop() { std::thread::spawn(move || { let _ = child.wait(); }); return Ended::Left; }
            std::thread::sleep(poll.min(Duration::from_millis(500)));   // the exit check runs at least this often
        }
    }
}

// ---------------------------------------------------------------- the install

/// Microsoft's Visual C++ 2015-2022 runtime installer (the one every vcrun package of winetricks fetches), always the current build.
pub const VCREDIST_URL: &str = "https://aka.ms/vs/17/release/vc_redist.x64.exe";

/// Downloads `url` into `file`; the cancel flag ends it.
fn download_to(http: &dyn Http, url: &str, file: &Path, cancel: &Cancel) -> Result<(), String> {
    use std::io::Write;
    let mut f = std::fs::File::create(file).map_err(|e| e.to_string())?;
    let (mut ok, mut wrote_err) = (true, None::<String>);
    let r = http.get_stream(url, &[], &mut |status, _| { if status >= 400 { ok = false; } ok }, &mut |chunk| { if cancel.is_set() { return false; } if let Err(e) = f.write_all(chunk) { wrote_err = Some(e.to_string()); return false; } true });
    if cancel.is_set() { return Err("Cancelled".into()); }
    if let Err(e) = r { return Err(format!("Download failed: {}", e.0)); }
    if let Some(e) = wrote_err { return Err(format!("Download failed: {e}")); }
    if !ok { return Err("Download failed: the server refused the request.".into()); }
    Ok(())
}

/// Installs the Visual C++ runtime into the prefix without winetricks: download Microsoft's installer and run it quietly under Wine. Not fatal for the launcher install (the caller logs a failure).
fn install_vcredist(env: &Env, prefix: &Path, extra: &[(&str, &str)], tmp: &Path, cancel: &Cancel, so: &Path, se: &Path) -> Result<(), String> {
    let file = tmp.join("vc_redist.x64.exe");
    download_to(env.http, VCREDIST_URL, &file, cancel)?;
    let cmd = prog(env, prefix, &file.to_string_lossy(), &["/install", "/quiet", "/norestart"], extra)?;
    match env.runner.run(&cmd, so, se, Duration::from_secs(900), env.poll, &mut || cancel.is_set()) {
        // 1638 = a newer or the same runtime is already there, 3010 = installed, a restart would be asked for
        Ended::Exit(0) | Ended::Exit(1638) | Ended::Exit(3010) => Ok(()),
        Ended::Exit(c) => Err(format!("the installer exited with {c}")),
        Ended::Left => Err(if cancel.is_set() { "Cancelled".to_string() } else { "it took too long".to_string() }),
        Ended::Failed(e) => Err(e),
    }
}

/// What the caller supplies: the runtime, the places, the network and the process runner.
pub struct Env<'a> { pub rt: &'a Runtime, pub home: &'a Path, pub wayland: bool, pub http: &'a dyn Http, pub runner: &'a dyn Runner, pub config_dir: &'a Path, pub tmp: &'a Path,
    /// The system `winetricks` and `wine` (for packages under a Proton build that cannot run on its own), and `7z`.
    pub winetricks: Option<PathBuf>, pub system_wine: Option<PathBuf>, pub sevenzip: Option<PathBuf>,
    /// How long the installer may run (600 s in PlayDate) and how often the finished check runs.
    pub installer_limit: Duration, pub poll: Duration, /// A detached launcher counts as installed once its program has been there this long.
    pub settle: Duration }

/// The Wine whose `wineserver` ends a session in a prefix this runtime made (Proton's own Wine, or the Wine itself).
fn session_wine(rt: &Runtime) -> Option<PathBuf> {
    proton_of(rt).and_then(|p| p.path.parent().map(|d| d.join("files/bin/wine64"))).or_else(|| match rt { Runtime::Wine { wine } => Some(wine.clone()), _ => None })
}

fn proton_of(rt: &Runtime) -> Option<&crate::wine::Proton> { match rt { Runtime::Umu { proton, .. } | Runtime::ProtonScript { proton } => Some(proton), Runtime::Wine { .. } => None } }

fn prog(env: &Env, prefix: &Path, exe: &str, args: &[&str], extra: &[(&str, &str)]) -> Result<Cmd, String> {
    let a = spawn_action(env.rt, env.home, Path::new(exe), &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(), Some(prefix), prefix, env.wayland)?;
    let mut c = Cmd::from_action(a)?;
    for (k, v) in extra { c.env.push((k.to_string(), Some(v.to_string()))); }
    Ok(c)
}

/// The program names `walk` finds under `prefix` whose path holds `name` and ends in `.exe`.
fn exe_in_prefix(prefix: &Path, name: &str) -> bool {
    fn walk(d: &Path, name: &str, depth: u32) -> bool {
        let Ok(rd) = std::fs::read_dir(d) else { return false };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() { if depth > 0 && !p.is_symlink() && walk(&p, name, depth - 1) { return true; } }
            else if p.to_string_lossy().ends_with(".exe") && p.to_string_lossy().contains(name) { return true; }
        }
        false
    }
    !name.is_empty() && walk(prefix, name, 12)
}
/// Only the Program Files folders (cheap enough for a poll).
fn exe_in_program_files(prefix: &Path, name: &str) -> bool {
    !name.is_empty() && ["Program Files", "Program Files (x86)"].iter().any(|pf| { fn find(d: &Path, n: &str, depth: u32) -> bool { std::fs::read_dir(d).map(|rd| rd.flatten().any(|e| { let p = e.path(); if p.is_dir() { depth > 0 && find(&p, n, depth - 1) } else { e.file_name().to_string_lossy() == n } })).unwrap_or(false) } find(&prefix.join("drive_c").join(pf), name, 10) })
}

/// Installs the launcher. `progress(phase, detail)` feeds the status line. Ok(()) when the launcher is in the prefix and remembered.
pub fn install(platform: &str, cfg: &InstallerCfg, prefix: &Path, env: &Env, cancel: &Cancel, progress: &mut dyn FnMut(&str, &str)) -> Result<(), String> {
    let step = |cancel: &Cancel| if cancel.is_set() { Err("Cancelled".to_string()) } else { Ok(()) };
    if cfg.url.is_empty() { return Err("No installer address is configured for this store.".into()); }
    let mut extra: Vec<(&str, &str)> = cfg.env.to_vec();
    // Proton builds take these two for speed
    if proton_of(env.rt).is_some() { extra.splice(0..0, [("WINEFSYNC", "1"), ("WINEESYNC", "1")]); }
    let tmp = env.tmp.join(format!("zest_{platform}_{}", std::process::id()));
    let result = (|| -> Result<(), String> {
        std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
        let (so, se) = (tmp.join("stdout.log"), tmp.join("stderr.log"));
        let quick = Duration::from_secs(900);
        let tail = |p: &Path| output_tail(&std::fs::read_to_string(p).unwrap_or_default(), 1000);

        // 1. the prefix: a leftover Wine session in it would make the first command run against a half-live prefix
        progress("creating_prefix", "Creating the Wine prefix...");
        if prefix.is_dir() { let _ = crate::wine::end_prefix_session(prefix, session_wine(env.rt).as_deref()); }
        std::fs::create_dir_all(prefix).map_err(|e| format!("Could not make {}: {e}", prefix.display()))?;
        let mut boot = prog(env, prefix, "wineboot", &["--init"], &extra)?;
        boot.env.push(("WINEARCH".into(), Some(cfg.winearch.into())));
        match env.runner.run(&boot, &so, &se, quick, env.poll, &mut || false) {
            Ended::Exit(0) => {}
            Ended::Exit(c) => return Err(format!("Failed to create the Wine prefix (wineboot exit {c}): {}", tail(&se))),
            Ended::Left => return Err("Creating the Wine prefix took too long.".into()),
            Ended::Failed(e) => return Err(format!("Failed to create the Wine prefix: {e}")),
        }
        step(cancel)?;
        if !cfg.win_version.is_empty() {
            let mut reg = prog(env, prefix, "reg", &["add", "HKCU\\Software\\Wine", "/v", "Version", "/d", cfg.win_version, "/f"], &extra)?;
            reg.env.push(("WINEARCH".into(), Some(cfg.winearch.into())));
            let _ = env.runner.run(&reg, &so, &se, Duration::from_secs(120), env.poll, &mut || false);
        }

        // 2. winetricks. Under umu-run: its own `winetricks` (bare name: the container does not see the host's path); otherwise the system winetricks with a system Wine
        let mut vc_done = false;
        for verb in cfg.winetricks {
            step(cancel)?;
            progress("winetricks", &format!("Installing {verb} via winetricks..."));
            let cmd = if let (Runtime::Umu { umu, proton }, _) = (env.rt, ()) {
                let root = proton.path.parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
                Cmd { program: umu.to_string_lossy().to_string(), args: vec!["winetricks".into(), "--unattended".into(), (*verb).into()], dir: None,
                    env: [("WINEPREFIX", prefix.to_string_lossy().to_string()), ("WINEARCH", cfg.winearch.into()), ("WINEDEBUG", "-all".into()), ("GAMEID", "umu-default".into()), ("PROTONPATH", root)].into_iter().map(|(k, v)| (k.to_string(), Some(v))).collect() }
            } else {
                // macOS has no winetricks. Its main job here is the Visual C++ runtime, which Zest installs itself (Microsoft's own installer, run in the prefix); the other packages
                // (fonts, DirectX shader library) are skipped and a launcher that needs them shows it in the log
                let Some(wt) = env.winetricks.clone() else {
                    if cfg!(target_os = "macos") {
                        if verb.starts_with("vcrun") {
                            if !vc_done {
                                vc_done = true;
                                progress("winetricks", "Installing the Visual C++ runtime...");
                                match install_vcredist(env, prefix, &extra, &tmp, cancel, &so, &se) { Ok(()) => crate::net::log(&format!("Launcher install {platform}: Visual C++ runtime installed")), Err(e) => { if e == "Cancelled" { return Err(e); } crate::net::log(&format!("Launcher install {platform}: Visual C++ runtime: {e}")); } }
                            }
                        } else { crate::net::log(&format!("Launcher install {platform}: no winetricks on this Mac, skipping {verb}")); }
                        continue;
                    }
                    return Err("winetricks is required but was not found: install it with your package manager".into());
                };
                let wine = match env.rt { Runtime::Wine { wine } => Some(wine.clone()), _ => env.system_wine.clone() };
                let mut e: Vec<(String, Option<String>)> = vec![("WINEPREFIX".into(), Some(prefix.to_string_lossy().to_string())), ("WINEARCH".into(), Some(cfg.winearch.into())), ("WINEDEBUG".into(), Some("-all".into()))];
                if let Some(w) = wine { e.push(("WINE".into(), Some(w.to_string_lossy().to_string()))); }
                Cmd { program: wt.to_string_lossy().to_string(), args: vec!["--unattended".into(), (*verb).into()], env: e, dir: None }
            };
            // a failed package is logged and the install goes on, as PlayDate does
            match env.runner.run(&cmd, &so, &se, Duration::from_secs(1800), env.poll, &mut || false) {
                Ended::Exit(0) => {}
                Ended::Exit(c) => crate::net::log(&format!("Launcher install {platform}: winetricks {verb} exit {c}: {}", tail(&se))),
                Ended::Left => crate::net::log(&format!("Launcher install {platform}: winetricks {verb} took too long")),
                Ended::Failed(e) => return Err(format!("winetricks {verb} failed: {e}")),
            }
        }

        // config files go in before the installer runs: some installers start the launcher once at the end
        for (rel, content) in cfg.post_install_files {
            if let Some(user) = crate::sources::rockstar::wine_user_dir(prefix, &std::env::var("USER").unwrap_or_default()) {
                let dst = user.join(rel);
                if let Some(d) = dst.parent() { let _ = std::fs::create_dir_all(d); }
                if let Err(e) = std::fs::write(&dst, content) { crate::net::log(&format!("Launcher install {platform}: could not write {}: {e}", dst.display())); }
            } else { crate::net::log(&format!("Launcher install {platform}: no Windows user folder for the config files")); }
        }

        // 3. download
        step(cancel)?;
        progress("downloading", "Downloading the installer...");
        let ext = if cfg.kind == "msi" { "msi" } else { "exe" };
        let file = tmp.join(format!("installer.{ext}"));
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&file).map_err(|e| e.to_string())?;
            let (mut ok, mut wrote_err) = (true, None::<String>);
            let r = env.http.get_stream(cfg.url, &[], &mut |status, _| { if status >= 400 { ok = false; } ok }, &mut |chunk| { if cancel.is_set() { return false; } if let Err(e) = f.write_all(chunk) { wrote_err = Some(e.to_string()); return false; } true });
            if let Err(e) = r { return Err(format!("Download failed: {}", e.0)); }
            if let Some(e) = wrote_err { return Err(format!("Download failed: {e}")); }
            if !ok { return Err("Download failed: the server refused the request.".into()); }
            step(cancel)?;
        }

        // 4. install
        if cfg.kind == "extract" {
            if cfg.install_path.is_empty() { return Err("An extract-type installer needs an install folder.".into()); }
            let dest = prefix.join("drive_c").join(cfg.install_path.replace('\\', "/"));
            std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
            progress("installing", "Extracting files...");
            let z = env.sevenzip.clone().ok_or("7z was not found: install p7zip with your package manager")?;
            let cmd = Cmd { program: z.to_string_lossy().to_string(), args: vec!["e".into(), file.to_string_lossy().to_string(), format!("-o{}", dest.display()), "-y".into()], env: vec![], dir: None };
            match env.runner.run(&cmd, &so, &se, Duration::from_secs(600), env.poll, &mut || false) { Ended::Exit(0) | Ended::Exit(1) => {}, Ended::Exit(c) => return Err(format!("Extraction failed (7z exit {c}): {}", tail(&se))), Ended::Left => return Err("Extraction took too long.".into()), Ended::Failed(e) => return Err(format!("Extraction error: {e}")) }
        } else {
            progress("installing", "Running the installer: complete its setup window...");
            let fstr = file.to_string_lossy().to_string();
            let cmd = if cfg.kind == "msi" { prog(env, prefix, "msiexec", &["/i", &fstr], &extra)? }
                // PROTON_VERB=run: the default under umu-run waits for the whole session to empty, which an installer that leaves the launcher open never lets happen
                else { let mut e2 = extra.clone(); e2.push(("PROTON_VERB", "run")); prog(env, prefix, &fstr, &[], &e2)? };
            let mut seen: Option<Instant> = None;
            let detached = cfg.detached && cfg.kind != "msi";
            let mut stop = || {
                if !detached { return false; }
                if exe_in_program_files(prefix, cfg.exe_name) { let t = *seen.get_or_insert_with(Instant::now); t.elapsed() >= env.settle } else { seen = None; false }
            };
            match env.runner.run(&cmd, &so, &se, env.installer_limit, env.poll, &mut stop) {
                Ended::Exit(c) if c != 0 => crate::net::log(&format!("Launcher install {platform}: installer exit {c}: {}", tail(&se))),
                Ended::Failed(e) => return Err(format!("Installer error: {e}")),
                _ => {}
            }
        }
        step(cancel)?;

        // 5. check, DLLs, remember
        progress("verifying", "Verifying the installation...");
        if !cfg.exe_name.is_empty() && !exe_in_prefix(prefix, cfg.exe_name) { return Err(format!("{} not found after installation. Did the setup complete?", cfg.exe_name)); }
        if let Some(p) = proton_of(env.rt) {
            if let Some(root) = p.path.parent().map(|d| d.join("files")) {
                let sys32 = prefix.join("drive_c/windows/system32");
                for rel in cfg.post_install_dlls {
                    let src = root.join(rel);
                    if !src.is_file() { crate::net::log(&format!("Launcher install {platform}: {} not found, skipped", src.display())); continue; }
                    if let Err(e) = std::fs::copy(&src, sys32.join(src.file_name().unwrap_or_default())) { crate::net::log(&format!("Launcher install {platform}: could not copy {}: {e}", src.display())); }
                }
            }
        }
        remember(env.config_dir, platform, prefix, match env.rt { Runtime::Wine { wine } => Some(wine.as_path()), _ => None }).map_err(|e| format!("Could not save the launcher setting: {e}"))?;
        progress("done", "Launcher installed successfully.");
        Ok(())
    })();
    // what the install left running in the prefix (the Wine server and its helpers, an installer still busy: on a Mac five servers and a 41-minute installer were
    // found after a few installs) is ended, except a launcher the installer opens to stay open (a detached one, when the install worked)
    if result.is_err() || !cfg.detached {
        if let Err(e) = crate::wine::end_prefix_session(prefix, session_wine(env.rt).as_deref()) { crate::net::log(&format!("Launcher install {platform}: {e}")); }
    }
    // a failed install keeps what the programs printed, in the data folder, so the real error can be read afterwards (the temp folder is removed below)
    if let Err(e) = &result {
        let mut text = format!("launcher install {platform} failed: {e}\n");
        for (name, file) in [("stdout", "stdout.log"), ("stderr", "stderr.log")] { text.push_str(&format!("\n--- {name}, without Wine's graphics banner ---\n{}\n", output_tail(&std::fs::read_to_string(tmp.join(file)).unwrap_or_default(), 6000))); }
        let _ = std::fs::write(env.config_dir.join(format!("launcher-install-{platform}.log")), text);
    }
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

/// The end of a program's output (the part that says what went wrong), without the graphics banner Wine prints on macOS ([mvk-info] and the list of Vulkan extensions) and without blank lines;
/// at most `max` characters.
pub fn output_tail(text: &str, max: usize) -> String {
    let kept: Vec<&str> = text.lines().filter(|l| { let t = l.trim(); !t.is_empty() && !t.starts_with("[mvk-") && !t.starts_with("VK_") && !t.starts_with("The following") && !t.starts_with("Created VkInstance") && !t.starts_with("MoltenVK") }).collect();
    let joined = kept.join("\n");
    let n = joined.chars().count();
    if n <= max { joined } else { joined.chars().skip(n - max).collect() }
}

/// The prefix a store's launcher is set up in: the one already saved (`config.json["launchers"][id]`), else the default folder.
pub fn prefix_for(config_dir: &Path, platform: &str) -> PathBuf {
    std::fs::read_to_string(config_dir.join("config.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v["launchers"][platform]["prefix"].as_str().map(|s| s.trim().to_string())).filter(|p| !p.is_empty()).map(PathBuf::from).unwrap_or_else(|| default_prefix(platform))
}

/// Installs a store's launcher on this machine with its real runtime and programs; the status line follows `progress`. Windows and macOS install launchers the normal way, so this refuses there.
pub fn install_here(platform: &str, config_dir: &Path, cancel: &Cancel, progress: &mut dyn FnMut(&str, &str)) -> Result<(), String> {
    if cfg!(windows) { let cfg = installer_for(platform).ok_or("This store has no launcher to install.")?; return install_windows(platform, &cfg, cancel, progress); }
    if !cfg!(any(target_os = "linux", target_os = "macos")) { return Err("This system installs launchers the usual way.".into()); }
    let cfg = installer_for(platform).ok_or("This store has no launcher to install.")?;
    // macOS: no Wine yet means Zest downloads one into its own folder first (the person was asked before this started)
    if cfg!(target_os = "macos") && crate::wine::resolve_system().is_err() { crate::macwine::install(&crate::net::UreqHttp::new(), cancel, progress)?; }
    let rt = crate::wine::resolve_system()?;
    let path = std::env::var_os("PATH").unwrap_or_default();
    let http = crate::net::UreqHttp::new();
    let home = crate::installdir::home();
    let env = Env { rt: &rt, home: &home, wayland: std::env::var_os("WAYLAND_DISPLAY").is_some(), http: &http, runner: &RealRunner, config_dir, tmp: &std::env::temp_dir(),
        winetricks: crate::wine::which("winetricks", &path, &[]), system_wine: crate::wine::find_wine(&path, &[]), sevenzip: ["7z", "7zz", "7za"].iter().find_map(|n| crate::wine::which(n, &path, &[])),
        installer_limit: Duration::from_secs(600), poll: Duration::from_secs(5), settle: Duration::from_secs(20) };
    install(platform, &cfg, &prefix_for(config_dir, platform), &env, cancel, progress)
}

/// `save_launcher_config`: `config.json["launchers"][platform] = {prefix, mode: "wine", wine_bin?}`; the rest of the file stays.
pub fn remember(config_dir: &Path, platform: &str, prefix: &Path, wine_bin: Option<&Path>) -> std::io::Result<()> {
    let _g = crate::account::config_lock();
    let path = config_dir.join("config.json");
    let mut cfg: Value = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_else(|| json!({}));
    let o = cfg.as_object_mut().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "config.json is not an object"))?;
    let l = o.entry("launchers").or_insert_with(|| json!({}));
    let mut entry = json!({"prefix": prefix.to_string_lossy(), "mode": "wine"});
    if let Some(w) = wine_bin { entry["wine_bin"] = json!(w.to_string_lossy()); }
    if let Some(m) = l.as_object_mut() { m.insert(platform.to_string(), entry); }
    crate::account::write_json(&path, &cfg)
}

/// The launcher's program inside the prefix, when it is installed (found by name, whatever the letter case).
pub fn launcher_exe(config_dir: &Path, platform: &str) -> Option<PathBuf> {
    fn find(dir: &Path, name: &str, depth: u32) -> Option<PathBuf> {
        let mut subdirs = vec![];
        for e in std::fs::read_dir(dir).ok()?.flatten() {
            let p = e.path();
            if p.is_dir() { subdirs.push(p); } else if e.file_name().to_string_lossy().to_lowercase() == name { return Some(p); }
        }
        if depth == 0 { return None; }
        subdirs.into_iter().find_map(|d| find(&d, name, depth - 1))
    }
    let cfg = installer_for(platform)?;
    if cfg!(windows) { return windows_launcher_exe(platform, &cfg); }
    find(&prefix_for(config_dir, platform).join("drive_c"), &cfg.exe_name.to_lowercase(), 8)
}

/// Starts the installed launcher in its prefix.
pub fn start_launcher(config_dir: &Path, platform: &str) -> Result<crate::sources::Action, String> {
    let exe = launcher_exe(config_dir, platform).ok_or("The launcher is not installed.")?;
    if cfg!(windows) { return Ok(crate::sources::Action::Spawn { program: exe.to_string_lossy().to_string(), args: vec![], dir: exe.parent().map(|d| d.to_string_lossy().to_string()), env: vec![] }); }
    let act = crate::wine::launch_action(&exe, &prefix_for(config_dir, platform))?;
    Ok(logged(act, &config_dir.join(format!("launcher-start-{platform}.log"))))
}

/// The same program, started through `sh` so that what it prints (Wine's error messages included, `WINEDEBUG=err+all`) goes to `log`, which is rewritten at each start. A launcher that
/// opens a black window or closes at once otherwise leaves nothing to read.
/// The most a launcher's start log keeps (20 MB).
pub const LAUNCH_LOG_MAX: u64 = 20_000_000;
/// Runs "$0" "$@" with its output, both streams, written to $ZEST_LAUNCH_LOG up to LAUNCH_LOG_MAX bytes and a note when the rest is dropped.
const LOG_CAP_SH: &str = "\"$0\" \"$@\" 2>&1 | awk -v max=20000000 'BEGIN { f = ENVIRON[\"ZEST_LAUNCH_LOG\"]; printf \"\" > f } { n += length($0) + 1; if (n <= max) print >> f; else if (!cut) { print \"[the log stops here: over 20 MB; the program kept running]\" >> f; close(f); cut = 1 } }'";
fn logged(act: crate::sources::Action, log: &Path) -> crate::sources::Action {
    match act {
        crate::sources::Action::Spawn { program, args, dir, mut env } => {
            env.retain(|(k, _)| k != "WINEDEBUG");
            env.push(("WINEDEBUG".into(), Some("err+all,fixme-all".into())));
            env.push(("ZEST_LAUNCH_LOG".into(), Some(log.to_string_lossy().to_string())));
            // at most LAUNCH_LOG_MAX bytes: a launcher stuck in a loop (Ubisoft Connect's window on a Mac repeated one OpenGL error) wrote 4.2 GB in minutes. awk keeps reading
            // after the limit and only stops writing, so the launcher never meets a closed pipe (which could kill it)
            let mut a = vec!["-c".to_string(), LOG_CAP_SH.to_string(), program];
            a.extend(args);
            crate::sources::Action::Spawn { program: "/bin/sh".into(), args: a, dir, env }
        }
        other => other,
    }
}

/// Removes the launcher: its whole prefix folder (the games installed in it go too) and the saved prefix setting. Refuses anything that is not a folder inside the home folder.
pub fn uninstall_launcher(config_dir: &Path, platform: &str) -> Result<(), String> {
    if cfg!(windows) { return uninstall_windows(platform); }
    let prefix = prefix_for(config_dir, platform);
    let home = crate::installdir::home();
    if !prefix.is_absolute() || !prefix.starts_with(&home) || prefix == home || prefix.components().count() < 4 { return Err("Zest will not delete that folder.".into()); }
    if prefix.is_dir() {
        // a Wine session still running in it keeps writing files ("Directory not empty" on the Mac): it is ended first, with the launcher's saved Wine if there is one
        let saved = std::fs::read_to_string(config_dir.join("config.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v["launchers"][platform]["wine_bin"].as_str().map(PathBuf::from)).filter(|p| p.is_file());
        let wine = saved.or_else(|| crate::wine::resolve_system().ok().and_then(|rt| session_wine(&rt)));
        if let Err(e) = crate::wine::end_prefix_session(&prefix, wine.as_deref()) { crate::net::log(&format!("Uninstall launcher {platform}: {e}")); }
        std::fs::remove_dir_all(&prefix).map_err(|e| format!("Could not remove {}: {e}", prefix.display()))?;
    }
    forget_prefix(config_dir, platform);
    Ok(())
}

// ---------------------------------------------------------------- Windows: the launcher's own installer and uninstaller, run the normal way

/// Where the launcher's program is on Windows: the Program Files folders of its vendor (a few levels down) and the per-user Programs folder; None when it is not installed.
fn windows_launcher_exe(platform: &str, cfg: &InstallerCfg) -> Option<PathBuf> {
    fn find(dir: &Path, name: &str, depth: u32) -> Option<PathBuf> {
        let mut subdirs = vec![];
        for e in std::fs::read_dir(dir).ok()?.flatten() {
            let p = e.path();
            if p.is_dir() { subdirs.push(p); } else if e.file_name().to_string_lossy().to_lowercase() == name { return Some(p); }
        }
        if depth == 0 { return None; }
        subdirs.into_iter().find_map(|d| find(&d, name, depth - 1))
    }
    let (name, vendors): (String, &[&str]) = (cfg.exe_name.to_lowercase(), match platform {
        "battle_net" => &["battle.net", "blizzard"], "ea_app" => &["electronic arts", "ea"], "epic_games" => &["epic games"], "legacy_games" => &["legacy"],
        "rockstar" => &["rockstar games"], "ubisoft" => &["ubisoft"], _ => &[] });
    for var in ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"] {
        let Some(root) = std::env::var_os(var).map(PathBuf::from) else { continue };
        for e in std::fs::read_dir(&root).into_iter().flatten().flatten() {
            let fname = e.file_name().to_string_lossy().to_lowercase();
            if e.path().is_dir() && vendors.iter().any(|v| fname == *v || fname.starts_with(&format!("{v} "))) { if let Some(p) = find(&e.path(), &name, 6) { return Some(p); } }
        }
    }
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from)?;
    find(&local.join("Programs"), &name, 3)
}

/// Downloads the store's installer and runs it the normal way (Windows shows its own window and asks for permission itself); done when the launcher's program is there.
pub fn install_windows(platform: &str, cfg: &InstallerCfg, cancel: &Cancel, progress: &mut dyn FnMut(&str, &str)) -> Result<(), String> {
    use crate::proc::Hide;
    if windows_launcher_exe(platform, cfg).is_some() { return Ok(()); }
    progress("downloading", "Downloading the installer...");
    let ext = if cfg.kind == "msi" { "msi" } else { "exe" };
    let file = std::env::temp_dir().join(format!("zest-launcher-{platform}.{ext}"));
    {
        use std::io::Write;
        let http = crate::net::UreqHttp::new();
        let mut f = std::fs::File::create(&file).map_err(|e| e.to_string())?;
        let (mut ok, mut wrote_err) = (true, None::<String>);
        let r = http.get_stream(cfg.url, &[], &mut |status, _| { if status >= 400 { ok = false; } ok }, &mut |chunk| { if cancel.is_set() { return false; } if let Err(e) = f.write_all(chunk) { wrote_err = Some(e.to_string()); return false; } true });
        if let Err(e) = r { return Err(format!("Download failed: {}", e.0)); }
        if let Some(e) = wrote_err { return Err(format!("Download failed: {e}")); }
        if !ok { return Err("Download failed: the server refused the request.".into()); }
        if cancel.is_set() { return Err("Cancelled".into()); }
    }
    progress("installing", &format!("The {} installer is open: follow its window.", cfg.name));
    // Start-Process asks Windows to run it the way a double click does, including the permission prompt an installer needs (a plain process start cannot show one)
    let script = if cfg.kind == "msi" { format!("Start-Process msiexec -ArgumentList '/i','{}'", file.display()) } else { format!("Start-Process -FilePath '{}'", file.display()) };
    let out = std::process::Command::new("powershell").args(["-NoProfile", "-NonInteractive", "-Command", &script]).hidden().output().map_err(|e| format!("Could not start the installer: {e}"))?;
    if !out.status.success() { return Err(format!("Could not start the installer: {}", String::from_utf8_lossy(&out.stderr).trim())); }
    let start = Instant::now();
    loop {
        if cancel.is_set() { return Err("Cancelled".into()); }
        if windows_launcher_exe(platform, cfg).is_some() { std::thread::sleep(Duration::from_secs(5)); return Ok(()); }
        if start.elapsed() > Duration::from_secs(30 * 60) { return Err("The launcher did not show up within 30 minutes. If the installer finished, press INSTALL LAUNCHER again to check.".into()); }
        std::thread::sleep(Duration::from_secs(3));
    }
}

/// Opens the launcher's own uninstaller (from its entry in Windows' installed programs), which shows its own confirmation.
fn uninstall_windows(platform: &str) -> Result<(), String> {
    use crate::proc::Hide;
    let cfg = installer_for(platform).ok_or("This store has no launcher.")?;
    let cmd = crate::winuninstall::command_by_display_name(&crate::winuninstall::entries(), cfg.name).ok_or_else(|| format!("Could not find the {} uninstaller. Remove it from Settings > Apps instead.", cfg.name))?;
    let (program, args) = crate::winuninstall::split_command(&cmd);
    let arglist = if args.is_empty() { String::new() } else { format!(" -ArgumentList {}", args.iter().map(|a| format!("'{}'", a.replace('\'', "''"))).collect::<Vec<_>>().join(",")) };
    let script = format!("Start-Process -FilePath '{}'{arglist}", program.replace('\'', "''"));
    let out = std::process::Command::new("powershell").args(["-NoProfile", "-NonInteractive", "-Command", &script]).hidden().output().map_err(|e| e.to_string())?;
    if out.status.success() { Ok(()) } else { Err(format!("Could not start the uninstaller: {}", String::from_utf8_lossy(&out.stderr).trim())) }
}

/// Forgets the saved prefix of a store's launcher (`config.json["launchers"][platform]`); the folder itself is not touched.
pub fn forget_prefix(config_dir: &Path, platform: &str) {
    let _g = crate::account::config_lock();
    let path = config_dir.join("config.json");
    if let Some(mut cfg) = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) {
        if let Some(l) = cfg.get_mut("launchers").and_then(Value::as_object_mut) { if l.remove(platform).is_some() { let _ = crate::account::write_json(&path, &cfg); } }
    }
}

/// The prefix folder a launcher is installed into by default: `<data root>/<platform>` (`datadirs.data_root()` in PlayDate).
/// A launcher's Wine prefix: `<Zest data folder>/<platform>`, or PlayDate's when it already made one there (on a Mac also its older `~/.local/share/PlayDate`).
pub fn default_prefix(platform: &str) -> PathBuf {
    let legacy = crate::installdir::home().join(".local/share/PlayDate").join(platform);
    if legacy.exists() { legacy } else { crate::installdir::user_data(platform) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{NetError, Response};
    #[test] fn the_end_of_the_output_is_kept_and_the_vulkan_banner_is_dropped() {
        let banner = "[mvk-info] MoltenVK version 1.4.1, supporting Vulkan version 1.4.334.\n\tThe following 150 Vulkan extensions are supported:\n\tVK_KHR_16bit_storage v1\n\tVK_KHR_8bit_storage v1\n";
        let text = format!("{banner}\n0024:err:module:import_dll Library X.dll not found\n\n0024:err:module:LdrInitializeThunk failed\n");
        assert_eq!(output_tail(&text, 1000), "0024:err:module:import_dll Library X.dll not found\n0024:err:module:LdrInitializeThunk failed");
        let long = "x".repeat(50) + "\nEND";
        assert_eq!(output_tail(&long, 10), "xxxxxx\nEND");
        assert_eq!(output_tail("", 10), "");
    }
    use std::sync::Mutex;
    fn tmp(tag: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("zest_li_{tag}_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d }

    /// Records each command and plays a script: what each program "does" to the prefix.
    struct Fake { log: Mutex<Vec<Cmd>>, prefix: PathBuf, fail_installer: bool, install_exe: bool }
    impl Runner for Fake {
        fn run(&self, cmd: &Cmd, _o: &Path, e: &Path, _l: Duration, _p: Duration, stop: &mut dyn FnMut() -> bool) -> Ended {
            self.log.lock().unwrap().push(cmd.clone());
            let is = |s: &str| cmd.args.iter().any(|a| a == s) || cmd.program.ends_with(s);
            if is("wineboot") { std::fs::create_dir_all(self.prefix.join("drive_c/users/steamuser")).unwrap(); return Ended::Exit(0); }
            if cmd.args.iter().any(|a| a.ends_with("installer.exe") || a.ends_with("installer.msi")) {
                if self.fail_installer { std::fs::write(e, "boom").unwrap(); return Ended::Exit(3); }
                if self.install_exe { let d = self.prefix.join("drive_c/Program Files/Store"); std::fs::create_dir_all(&d).unwrap(); std::fs::write(d.join("Launcher.exe"), "x").unwrap(); }
                // a detached installer never exits: it only ends when the caller says it is installed
                if cmd.env.iter().any(|(k, _)| k == "PROTON_VERB") { for _ in 0..3 { if stop() { return Ended::Left; } } return Ended::Left; }
                return Ended::Exit(0);
            }
            if cmd.program.ends_with("7z") && self.install_exe { let d = self.prefix.join("drive_c/Program Files/Store"); std::fs::create_dir_all(&d).unwrap(); std::fs::write(d.join("Launcher.exe"), "x").unwrap(); }
            Ended::Exit(0)
        }
    }
    struct Site; impl Http for Site {
        fn get(&self, _u: &str, _h: &[(&str, &str)], _t: Duration) -> Result<Response, NetError> { Err(NetError("no".into())) }
        fn get_stream(&self, u: &str, _h: &[(&str, &str)], on_head: &mut dyn FnMut(u16, Option<u64>) -> bool, on_chunk: &mut dyn FnMut(&[u8]) -> bool) -> Result<(), NetError> {
            if u.contains("missing") { on_head(404, None); return Ok(()); }
            if on_head(200, Some(4)) { on_chunk(b"MZ.."); } Ok(())
        }
    }
    fn cfg(url: &'static str) -> InstallerCfg { InstallerCfg { name: "Store", url, kind: "exe", winearch: "win64", win_version: "win10", winetricks: &["corefonts"], exe_name: "Launcher.exe", post_install_dlls: &["lib/a.dll"],
        post_install_files: &[("AppData/Local/S/settings.yaml", "x: 1")], detached: true, ..Default::default() } }
    fn env<'a>(rt: &'a Runtime, fake: &'a Fake, d: &'a Path, http: &'a dyn Http) -> Env<'a> { Env { rt, home: d, wayland: false, http, runner: fake, config_dir: d, tmp: d, winetricks: Some("/usr/bin/winetricks".into()), system_wine: Some("/usr/bin/wine".into()), sevenzip: Some("/usr/bin/7z".into()),
        installer_limit: Duration::from_secs(5), poll: Duration::from_millis(1), settle: Duration::ZERO } }

    #[test] fn every_store_with_an_installer_in_plugin_json_has_one_here() {
        for p in ["battle_net", "ea_app", "epic_games", "legacy_games", "rockstar", "ubisoft"] { let c = installer_for(p).unwrap(); assert!(c.url.starts_with("https://") && !c.exe_name.is_empty() && !c.name.is_empty(), "{p}"); }
        assert!(installer_for("gog").is_none() && installer_for("steam").is_none());
        assert_eq!(installer_for("epic_games").unwrap().kind, "msi");
        assert_eq!(installer_for("ubisoft").unwrap().post_install_files.len(), 1);
    }
    #[test] fn a_full_install_with_proton_makes_the_prefix_runs_the_steps_in_order_and_remembers_it() {
        let d = tmp("full"); let prefix = d.join("pfx"); let proton_dir = d.join("GE-Proton10-4"); std::fs::create_dir_all(proton_dir.join("files/lib")).unwrap(); std::fs::write(proton_dir.join("files/lib/a.dll"), "dll").unwrap();
        let rt = Runtime::Umu { umu: "/usr/bin/umu-run".into(), proton: crate::wine::Proton { name: "GE-Proton10-4".into(), path: proton_dir.join("proton") } };
        std::fs::create_dir_all(prefix.join("drive_c/windows/system32")).unwrap();
        std::fs::write(d.join("config.json"), "{\"other\": 1}").unwrap();
        let fake = Fake { log: Mutex::new(vec![]), prefix: prefix.clone(), fail_installer: false, install_exe: true };
        let mut phases = vec![];
        install("rockstar", &cfg("https://x/installer"), &prefix, &env(&rt, &fake, &d, &Site), &Cancel::new(), &mut |p, _| phases.push(p.to_string())).unwrap();
        assert_eq!(phases, ["creating_prefix", "winetricks", "downloading", "installing", "verifying", "done"]);
        let log = fake.log.lock().unwrap();
        assert!(log[0].program == "/usr/bin/umu-run" && log[0].args == ["wineboot", "--init"] && log[0].env.contains(&("WINEARCH".into(), Some("win64".into()))), "{:?}", log[0]);
        assert_eq!(log[1].args[..3], ["reg", "add", "HKCU\\Software\\Wine"]);
        assert_eq!(log[2].args, ["winetricks", "--unattended", "corefonts"]); assert!(log[2].env.iter().any(|(k, v)| k == "PROTONPATH" && v.as_deref() == Some(&*proton_dir.to_string_lossy())));
        assert!(log[3].args[0].ends_with("installer.exe") && log[3].env.contains(&("PROTON_VERB".into(), Some("run".into()))) && log[3].env.contains(&("WINEFSYNC".into(), Some("1".into()))));
        assert_eq!(std::fs::read_to_string(prefix.join("drive_c/users/steamuser/AppData/Local/S/settings.yaml")).unwrap(), "x: 1", "config files are in before the installer runs");
        assert!(prefix.join("drive_c/windows/system32/a.dll").is_file(), "the DLL came over from Proton");
        let c: Value = serde_json::from_str(&std::fs::read_to_string(d.join("config.json")).unwrap()).unwrap();
        assert_eq!((c["launchers"]["rockstar"]["prefix"].as_str(), c["launchers"]["rockstar"]["mode"].as_str(), c["other"].as_i64()), (Some(&*prefix.to_string_lossy()), Some("wine"), Some(1)));
        assert!(!d.join(format!("zest_rockstar_{}", std::process::id())).exists(), "the temp folder is removed");
        // and a source that reads the saved setting finds it
        assert_eq!(crate::sources::rockstar::prefix_setting(&d, "rockstar"), Some(prefix.clone()));
    }
    #[test] fn failures_say_what_went_wrong_and_save_nothing() {
        let d = tmp("fail"); let prefix = d.join("pfx"); let rt = Runtime::Wine { wine: "/usr/bin/wine".into() };
        std::fs::write(d.join("config.json"), "{}").unwrap();
        let mk = |fail, exe| Fake { log: Mutex::new(vec![]), prefix: prefix.clone(), fail_installer: fail, install_exe: exe };
        // the launcher's program never appears
        let f = mk(false, false);
        let e = install("rockstar", &cfg("https://x/i"), &prefix, &env(&rt, &f, &d, &Site), &Cancel::new(), &mut |_, _| {}).unwrap_err();
        assert!(e.contains("Launcher.exe not found after installation"), "{e}");
        // a refused download
        let f = mk(false, true);
        assert!(install("rockstar", &cfg("https://x/missing"), &prefix, &env(&rt, &f, &d, &Site), &Cancel::new(), &mut |_, _| {}).unwrap_err().contains("Download failed"));
        // no winetricks on the machine
        let mut en = env(&rt, &f, &d, &Site); en.winetricks = None;
        assert!(install("rockstar", &cfg("https://x/i"), &prefix, &en, &Cancel::new(), &mut |_, _| {}).unwrap_err().contains("winetricks is required"));
        // cancelled
        let c = Cancel::new(); c.cancel();
        assert_eq!(install("rockstar", &cfg("https://x/i"), &prefix, &env(&rt, &f, &d, &Site), &c, &mut |_, _| {}).unwrap_err(), "Cancelled");
        let cfg_text: Value = serde_json::from_str(&std::fs::read_to_string(d.join("config.json")).unwrap()).unwrap();
        assert!(cfg_text.get("launchers").is_none(), "nothing is remembered after a failure");
        // plain Wine: winetricks runs with the system winetricks and WINE set
        let f = mk(false, true);
        install("rockstar", &cfg("https://x/i"), &prefix, &env(&rt, &f, &d, &Site), &Cancel::new(), &mut |_, _| {}).unwrap();
        let log = f.log.lock().unwrap();
        let wt = log.iter().find(|c| c.program == "/usr/bin/winetricks").unwrap();
        assert!(wt.env.contains(&("WINE".into(), Some("/usr/bin/wine".into()))) && wt.args == ["--unattended", "corefonts"]);
        let c: Value = serde_json::from_str(&std::fs::read_to_string(d.join("config.json")).unwrap()).unwrap();
        assert_eq!(c["launchers"]["rockstar"]["wine_bin"].as_str(), Some("/usr/bin/wine"));
    }
    #[test] fn an_msi_runs_msiexec_and_an_extract_installer_uses_7z() {
        let d = tmp("kinds"); let prefix = d.join("pfx"); let rt = Runtime::Wine { wine: "/usr/bin/wine".into() };
        let f = Fake { log: Mutex::new(vec![]), prefix: prefix.clone(), fail_installer: false, install_exe: true };
        let msi = InstallerCfg { kind: "msi", winetricks: &[], win_version: "", detached: false, post_install_files: &[], post_install_dlls: &[], ..cfg("https://x/i") };
        install("epic_games", &msi, &prefix, &env(&rt, &f, &d, &Site), &Cancel::new(), &mut |_, _| {}).unwrap();
        assert!(f.log.lock().unwrap().iter().any(|c| c.args.first().map(String::as_str) == Some("msiexec") && c.args[1] == "/i" && c.args[2].ends_with("installer.msi")));
        let f = Fake { log: Mutex::new(vec![]), prefix: prefix.clone(), fail_installer: false, install_exe: true };
        let ex = InstallerCfg { kind: "extract", install_path: "Program Files\\Store", winetricks: &[], win_version: "", detached: false, post_install_files: &[], post_install_dlls: &[], ..cfg("https://x/i") };
        install("x", &ex, &prefix, &env(&rt, &f, &d, &Site), &Cancel::new(), &mut |_, _| {}).unwrap();
        let log = f.log.lock().unwrap(); let z = log.iter().find(|c| c.program == "/usr/bin/7z").unwrap();
        assert!(z.args[2].ends_with("drive_c/Program Files/Store") || z.args[2].contains("Program Files/Store"));
    }
}

#[cfg(all(test, unix))]
mod log_cap_tests {
    #[test] fn the_start_log_stops_at_its_limit_while_the_program_runs_to_the_end() {
        let d = std::env::temp_dir().join(format!("zest_logcap_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        let log = d.join("start.log");
        // a stand-in launcher: 30 MB of output, then a file that shows it was not killed on the way
        let script = d.join("noisy.sh");
        std::fs::write(&script, format!("#!/bin/sh\nyes 'err:d3d:wined3d_check_gl_call GL_INVALID_OPERATION from glClientWaitSync' | head -c 30000000\necho done > '{}'\n", d.join("finished").display())).unwrap();
        let st = std::process::Command::new("/bin/sh").args(["-c", super::LOG_CAP_SH, "/bin/sh", &script.to_string_lossy()]).env("ZEST_LAUNCH_LOG", &log).status().unwrap();
        assert!(st.success());
        let size = std::fs::metadata(&log).unwrap().len();
        assert!(size <= super::LAUNCH_LOG_MAX + 200 && size > super::LAUNCH_LOG_MAX - 200, "{size}");
        assert!(std::fs::read_to_string(&log).unwrap().ends_with("the program kept running]\n"));
        assert!(d.join("finished").exists(), "the program ran to its end");
        let _ = std::fs::remove_dir_all(&d);
    }
}
