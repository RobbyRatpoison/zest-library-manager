//! What the stores that hand out plain game folders (itch.io, IndieGala, Humble) share: starting the file chosen or guessed in a folder, and turning "install" into download-and-unpack.
use super::{Action, Launch};
use std::path::Path;

/// Runs `exe`: a macOS `.app` through `open`; a Windows `.exe` directly on Windows, elsewhere through Proton or Wine in `prefix` (see `wine`); anything else is made executable and
/// started in `cwd`.
pub fn run_file(exe: &Path, cwd: &Path, prefix: Option<&Path>) -> Result<Launch, String> {
    let e = exe.to_string_lossy().to_string();
    if e.ends_with(".app") && exe.is_dir() { return Ok(Launch { action: Action::Spawn { program: "open".into(), args: vec![e], dir: None, env: vec![] }, record: true }); }
    if e.to_lowercase().ends_with(".exe") && !cfg!(windows) {
        let prefix = prefix.ok_or("This is a Windows game and has no Wine prefix to run in")?;
        return crate::wine::launch_action(exe, prefix).map(|action| Launch { action, record: true });
    }
    #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; if let Ok(m) = std::fs::metadata(exe) { let mut p = m.permissions(); p.set_mode(p.mode() | 0o111); let _ = std::fs::set_permissions(exe, p); } }
    Ok(Launch { action: Action::Spawn { program: e, args: vec![], dir: Some(cwd.to_string_lossy().to_string()), env: vec![] }, record: true })
}

/// The file to run in an install folder: the chosen one if it is still there, else the best guess. None = nothing to run.
pub fn executable_in(dir: &Path, chosen: Option<&str>) -> Option<String> {
    chosen.filter(|r| { let a = dir.join(r); a.is_file() || (r.ends_with(".app") && a.is_dir()) }).map(String::from)
        .or_else(|| launcher_script(dir))
        .or_else(|| crate::native_exe::pick_executable(dir, crate::native_exe::Os::current()).path)
}

/// What to start in a game's folder.
#[derive(Debug, PartialEq)]
pub enum Choice { /// The file, relative to the folder.
    File(String), /// Several files tie for the best and nothing chose between them: ask.
    Ask(Vec<String>), /// Nothing that could start it.
    Nothing }

/// The chosen file if it is still there, else the game's own launcher script, else the best guess; when the best guess is a tie the caller must ask the user.
pub fn choose_executable(dir: &Path, chosen: Option<&str>) -> Choice {
    if let Some(r) = chosen.filter(|r| { let a = dir.join(r); a.is_file() || (r.ends_with(".app") && a.is_dir()) }) { return Choice::File(r.to_string()); }
    if let Some(s) = launcher_script(dir) { return Choice::File(s); }
    let p = crate::native_exe::pick_executable(dir, crate::native_exe::Os::current());
    match p.path { None => Choice::Nothing, Some(_) if p.ambiguous && p.candidates.len() > 1 => { let mut c = p.candidates; c.sort(); Choice::Ask(c) } Some(path) => Choice::File(path) }
}

/// The launch result for a folder's choice: run the file, ask which one, or open the folder when there is nothing to run (a zip not unpacked yet, say).
pub fn launch_choice(dir: &Path, choice: Choice, run: &dyn Fn(&Path, &Path) -> Result<Launch, String>) -> Result<Launch, String> {
    match choice {
        Choice::File(rel) => { let abs = dir.join(rel); let cwd = abs.parent().unwrap_or(dir).to_path_buf(); run(&abs, &cwd) }
        Choice::Ask(candidates) => Ok(Launch { action: Action::PickExecutable { dir: dir.to_string_lossy().to_string(), candidates }, record: false }),
        Choice::Nothing => Ok(match html_game(dir) { Some(page) => open_html(&page), None => Launch { action: Action::OpenUrl(dir.to_string_lossy().to_string()), record: false } }),
    }
}

/// A shell script in the game's own folder that sets `LD_LIBRARY_PATH` before starting the binary (the usual way a game ships its own libraries): that script is the way to start the
/// game, since the binary alone cannot find the libraries. PlayDate's picker went straight to the binary.
pub fn launcher_script(dir: &Path) -> Option<String> {
    if cfg!(windows) { return None; }
    let mut names: Vec<String> = std::fs::read_dir(dir).ok()?.flatten().filter(|e| e.path().is_file()).map(|e| e.file_name().to_string_lossy().to_string()).collect();
    names.sort();
    names.into_iter().find(|n| {
        let p = dir.join(n);
        let mut head = vec![0u8; 8192];
        let Ok(k) = std::fs::File::open(&p).and_then(|mut f| { use std::io::Read; f.read(&mut head) }) else { return false };
        head.truncate(k);
        head.starts_with(b"#!") && String::from_utf8_lossy(&head).contains("LD_LIBRARY_PATH")
    })
}

/// Streams `url` into `dest` (a file), reporting `progress(done, total)`; refuses a non-200 answer or a disk too full for the download. Err("Cancelled") leaves nothing behind.
pub fn download(http: &dyn crate::net::Http, url: &str, dest: &Path, cancel: &crate::net::Cancel, progress: &mut dyn FnMut(u64, u64)) -> Result<u64, String> {
    use std::cell::RefCell;
    use std::io::Write;
    struct St { file: Option<std::fs::File>, problem: Option<String>, total: u64, done: u64, last: std::time::Instant }
    let dir = dest.parent().ok_or("bad destination")?;
    let st = RefCell::new(St { file: None, problem: None, total: 0, done: 0, last: std::time::Instant::now() });
    let prog = RefCell::new(progress);
    let r = http.get_stream(url, &[],
        &mut |status, len| {
            let mut s = st.borrow_mut();
            if status != 200 { s.problem = Some(format!("The download was refused (HTTP {status})")); return false; }
            s.total = len.unwrap_or(0);
            if s.total > 0 { if let Err(e) = crate::installdir::check_disk_space(dir, s.total) { s.problem = Some(e); return false; } }
            match std::fs::File::create(dest) { Ok(f) => { s.file = Some(f); true } Err(e) => { s.problem = Some(format!("Could not write {}: {e}", dest.display())); false } }
        },
        &mut |chunk| {
            let mut s = st.borrow_mut();
            if cancel.is_set() { s.problem = Some("Cancelled".into()); return false; }
            if let Some(f) = s.file.as_mut() { if let Err(e) = f.write_all(chunk) { s.problem = Some(format!("Could not write the download: {e}")); return false; } }
            s.done += chunk.len() as u64;
            if s.last.elapsed() >= std::time::Duration::from_millis(250) { s.last = std::time::Instant::now(); (prog.borrow_mut())(s.done, s.total); }
            true
        });
    let s = st.into_inner();
    drop(s.file);
    if let Some(p) = s.problem { let _ = std::fs::remove_file(dest); return Err(p); }
    if let Err(e) = r { let _ = std::fs::remove_file(dest); return Err(format!("The download failed: {e}")); }
    if s.total > 0 && s.done < s.total { let _ = std::fs::remove_file(dest); return Err("The download ended early".into()); }
    (prog.borrow_mut())(s.done, s.total.max(s.done));
    Ok(s.done)
}

/// Unpacks a downloaded archive into its folder and removes the archive; for a file Zest cannot unpack it is left in place and the note says so.
pub fn unpack(archive: &Path, dir: &Path, cancel: &crate::net::Cancel, progress: &mut dyn FnMut(u64, u64)) -> Result<Option<String>, String> {
    let name = archive.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    if crate::archive::kind_of(&name).is_none() && crate::archive::is_zip_payload(archive) { unpack_installer(archive, dir, cancel, progress)?; return Ok(None); }
    if crate::archive::kind_of(&name).is_none() { return Ok(Some(format!("{name} was downloaded to {} but Zest cannot unpack that kind of file; open it yourself.", dir.display()))); }
    crate::archive::extract(archive, dir, progress, &|| cancel.is_set())?;
    let _ = std::fs::remove_file(archive);
    Ok(None)
}

/// A self-extracting installer (a program with a zip appended; MojoSetup keeps the game under `data/`): its files are unpacked into the game's folder directly, without running the installer,
/// and the installer is removed.
fn unpack_installer(installer: &Path, dir: &Path, cancel: &crate::net::Cancel, progress: &mut dyn FnMut(u64, u64)) -> Result<(), String> {
    let staging = dir.join(".zest_unpack");
    let _ = std::fs::remove_dir_all(&staging);
    let r = crate::archive::extract_zip(installer, &staging, progress, &|| cancel.is_set());
    if let Err(e) = r { let _ = std::fs::remove_dir_all(&staging); return Err(e); }
    let from = if staging.join("data").is_dir() { staging.join("data") } else { staging.clone() };
    for e in std::fs::read_dir(&from).map_err(|e| e.to_string())?.flatten() {
        let target = dir.join(e.file_name());
        if target.is_dir() { let _ = std::fs::remove_dir_all(&target); } else { let _ = std::fs::remove_file(&target); }
        std::fs::rename(e.path(), &target).map_err(|x| format!("Could not move {}: {x}", e.file_name().to_string_lossy()))?;
    }
    let _ = std::fs::remove_dir_all(&staging);
    let _ = std::fs::remove_file(installer);
    Ok(())
}

/// A folder with archives in it but nothing to run (a download that could not be unpacked then, or a zip the user dropped in): unpack them in place and remove them. True when something was unpacked.
/// Archives over 400 MB are left (this runs while a launch waits).
pub fn unpack_leftovers(dir: &Path) -> bool {
    // something to run, unless that something is itself a self-extracting installer
    if let Some(rel) = executable_in(dir, None) { if !crate::archive::is_zip_payload(&dir.join(rel)) { return false; } }
    let mut any = false;
    let Ok(rd) = std::fs::read_dir(dir) else { return false };
    let mut files: Vec<_> = rd.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect(); files.sort();
    for p in files {
        let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if std::fs::metadata(&p).map_or(true, |m| m.len() > 400 * 1024 * 1024) { continue; }
        let installer = crate::archive::kind_of(&name).is_none() && crate::archive::is_zip_payload(&p);
        if crate::archive::kind_of(&name).is_none() && !installer { continue; }
        let done = if installer { unpack_installer(&p, dir, &crate::net::Cancel::new(), &mut |_, _| {}).map(|_| 0) } else { crate::archive::extract(&p, dir, &mut |_, _| {}, &|| false) };
        match done { Ok(_) => { let _ = std::fs::remove_file(&p); crate::net::log(&format!("unpacked {name} in {}", dir.display())); any = true; } Err(e) => crate::net::log(&format!("could not unpack {name}: {e}")) }
    }
    any
}

// ---------------------------------------------------------------- browser games

/// A game made of web pages (an itch.io HTML game downloaded as files): its `index.html`, else the only `.html` file, at the top of the folder or one folder down.
pub fn html_game(dir: &Path) -> Option<std::path::PathBuf> {
    let pick = |d: &Path| -> Option<std::path::PathBuf> {
        let mut html: Vec<std::path::PathBuf> = std::fs::read_dir(d).ok()?.flatten().map(|e| e.path()).filter(|p| p.is_file() && p.extension().map_or(false, |x| x.eq_ignore_ascii_case("html") || x.eq_ignore_ascii_case("htm"))).collect();
        html.sort();
        html.iter().find(|p| p.file_name().map_or(false, |n| n.to_string_lossy().eq_ignore_ascii_case("index.html"))).cloned().or_else(|| (html.len() == 1).then(|| html[0].clone()))
    };
    pick(dir).or_else(|| {
        let subs: Vec<std::path::PathBuf> = std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).filter(|p| p.is_dir() && !p.file_name().map_or(true, |n| n.to_string_lossy().starts_with('.') || n == "__MACOSX")).collect();
        if subs.len() == 1 { pick(&subs[0]) } else { None }
    })
}
/// Opens a browser game in the default web browser (counts as a play).
pub fn open_html(page: &Path) -> Launch { Launch { action: Action::OpenUrl(page.to_string_lossy().to_string()), record: true } }

// ---------------------------------------------------------------- a game shipped as a Windows setup program

/// The setup programs Zest can run without anyone at the keyboard. Judged by the file's contents, not its name: an installer can be called anything,
/// and the program picker already skips files named setup/install.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Setup { Inno, Nsis, Msi }

/// Is this file a setup program (and which kind)? An `.msi` package by its name; a Windows program by the marks Inno Setup and NSIS leave in it
/// (they sit in the program's first part, so only the first 8 MB is read).
pub fn setup_kind(path: &Path) -> Option<Setup> {
    let name = path.file_name()?.to_string_lossy().to_lowercase();
    if name.ends_with(".msi") { return Some(Setup::Msi); }
    if !name.ends_with(".exe") { return None; }
    use std::io::Read;
    let mut buf = Vec::new();
    std::fs::File::open(path).ok()?.take(8 << 20).read_to_end(&mut buf).ok()?;
    if !buf.starts_with(b"MZ") { return None; }
    let has = |needle: &[u8]| buf.windows(needle.len()).any(|w| w == needle);
    if has(b"Inno Setup Setup Data") || has(b"JR.Inno.Setup") { return Some(Setup::Inno); }
    if has(b"Nullsoft Install System") || has(b"NullsoftInst") { return Some(Setup::Nsis); }
    None
}

/// A Linux or Mac path as Wine sees it (`Z:\home\...`); on Windows the path itself.
pub fn windows_path(p: &Path) -> String {
    let s = p.to_string_lossy().to_string();
    if cfg!(windows) { s } else { format!("Z:{}", s.replace('/', "\\")) }
}

/// The arguments that run a setup program silently into `target` (`silent` false: the setup window shows, starting in `target`, for the person to finish).
/// NSIS wants `/D=` last and unquoted; Inno takes `/DIR=`; an MSI package goes through `msiexec`.
pub fn setup_args(kind: Setup, installer: &Path, target: &Path, silent: bool) -> (String, Vec<String>) {
    let t = windows_path(target);
    match kind {
        Setup::Inno => (installer.to_string_lossy().to_string(), [if silent { vec!["/VERYSILENT".to_string(), "/SUPPRESSMSGBOXES".into(), "/NORESTART".into(), "/SP-".into()] } else { vec!["/SP-".to_string()] }, vec![format!("/DIR={t}")]].concat()),
        Setup::Nsis => (installer.to_string_lossy().to_string(), [if silent { vec!["/S".to_string()] } else { vec![] }, vec![format!("/D={t}")]].concat()),
        Setup::Msi => ("msiexec".into(), [vec!["/i".to_string(), windows_path(installer)], if silent { vec!["/qn".to_string()] } else { vec![] }, vec![format!("TARGETDIR={t}"), format!("INSTALLDIR={t}"), format!("INSTALLLOCATION={t}"), "/norestart".into()]].concat()),
    }
}

/// The setup program a freshly unpacked game folder is: the one file at its top that is a setup program, when no other program there could be the game.
/// A game that merely ships a runtime installer beside itself (DirectX, Visual C++) is left alone.
pub fn setup_in(dir: &Path) -> Option<(std::path::PathBuf, Setup)> {
    let files: Vec<std::path::PathBuf> = std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
    let progs: Vec<&std::path::PathBuf> = files.iter().filter(|p| p.extension().map_or(false, |x| x.eq_ignore_ascii_case("exe") || x.eq_ignore_ascii_case("msi"))).collect();
    let found: Vec<(std::path::PathBuf, Setup)> = progs.iter().filter_map(|p| setup_kind(p).map(|k| ((*p).clone(), k))).collect();
    let [(path, kind)] = found.as_slice() else { return None };
    let name = path.file_name()?.to_string_lossy().to_lowercase();
    if ["redist", "vcredist", "vc_redist", "directx", "dxsetup", "dotnet", "physx", "oalinst"].iter().any(|r| name.contains(r)) { return None; }
    // another program beside it is the game (the installer is something else it ships)
    if progs.len() > 1 { return None; }
    Some((path.clone(), *kind))
}

/// Runs a game's setup program into `<dir>/game` in `prefix` (through Wine or Proton; directly on Windows) and waits for it: silently first, then, when that left
/// nothing to start, once more with its window showing so the person can finish it. The setup file is removed once the game is there.
/// Ok(None) = there was no setup program; Ok(Some(note)) = installed; Err = it ran but no game came out of it (the setup file is kept).
pub fn run_setup(dir: &Path, prefix: &Path, cancel: &crate::net::Cancel, progress: &mut dyn FnMut(&str)) -> Result<Option<String>, String> {
    use crate::launcher_installer::Runner as _;
    let Some((installer, kind)) = setup_in(dir) else { return Ok(None) };
    let target = dir.join("game");
    std::fs::create_dir_all(&target).map_err(|e| format!("Could not make {}: {e}", target.display()))?;
    // the setup program installs into a folder whose path has no spaces, then the result moves into the game's folder: Wine (like Windows) quotes an
    // argument holding a space, and NSIS reads /D= from the raw command line and does not see a quoted one (it then installs to its own default)
    let staging = setup_staging();
    let into = staging.as_deref().unwrap_or(&target).to_path_buf();
    if let Some(s) = &staging { std::fs::create_dir_all(s).map_err(|e| format!("Could not make {}: {e}", s.display()))?; }
    let found = || crate::native_exe::pick_executable(&into, crate::native_exe::Os::current()).path.is_some();
    let logs = (dir.join(".setup-out.log"), dir.join(".setup-err.log"));
    for silent in [true, false] {
        if cancel.is_set() { return Err("Cancelled".into()); }
        progress(if silent { "Running the game's setup program (no window)" } else { "Running the game's setup program: finish its window" });
        let (program, args) = setup_args(kind, &installer, &into, silent);
        let cmd = if cfg!(windows) { crate::launcher_installer::Cmd { program, args, env: vec![], dir: Some(dir.to_string_lossy().to_string()) } } else {
            // PROTON_VERB=run: the default under umu-run waits for the whole Wine session to end
            match crate::wine::launch_action_full(std::path::Path::new(&program), prefix, &args, Some(dir), &[("PROTON_VERB".into(), "run".into())])? {
                Action::Spawn { program, args, dir, env } => crate::launcher_installer::Cmd { program, args, env, dir },
                _ => return Err("Could not start the setup program".into()),
            }
        };
        crate::net::log(&format!("game setup ({kind:?}, {}): {}", if silent { "silent" } else { "with its window" }, installer.display()));
        let limit = std::time::Duration::from_secs(if silent { 30 * 60 } else { 2 * 60 * 60 });
        let ended = crate::launcher_installer::RealRunner.run(&cmd, &logs.0, &logs.1, limit, std::time::Duration::from_millis(500), &mut || cancel.is_set());
        crate::net::log(&format!("game setup ended: {ended:?}"));
        if cancel.is_set() { return Err("Cancelled".into()); }
        if found() {
            if let Some(s) = &staging { move_contents(s, &target).map_err(|e| format!("The game was installed in {} but could not be moved into {}: {e}", s.display(), target.display()))?; let _ = std::fs::remove_dir_all(s); }
            let _ = std::fs::remove_file(&installer);
            for l in [&logs.0, &logs.1] { let _ = std::fs::remove_file(l); }
            return Ok(Some(format!("installed by its setup program into {}", target.display())));
        }
    }
    if let Some(s) = &staging { let _ = std::fs::remove_dir_all(s); }
    Err(format!("The game's setup program ran, but no game was found in {}. Run it again from the game's folder, or pick the game's program in its Edit dialog.", target.display()))
}

/// A fresh folder under `~/.cache/zest-setup` for a setup program to install into, when that path has no spaces (None: install straight into the game's folder).
/// Under the home folder, not /tmp: Proton's Steam Runtime container has a private /tmp, so a program running in it cannot write there for Zest to see.
fn setup_staging() -> Option<std::path::PathBuf> {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let p = crate::installdir::home().join(".cache/zest-setup").join(format!("{n:x}"));
    (!p.to_string_lossy().contains(' ')).then_some(p)
}

/// Moves everything in `from` into `to` (a rename, or a copy when they are on different disks).
pub fn move_contents(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)?.flatten() {
        let dest = to.join(e.file_name());
        if std::fs::rename(e.path(), &dest).is_err() { copy_tree(&e.path(), &dest)?; if e.path().is_dir() { std::fs::remove_dir_all(e.path())?; } else { std::fs::remove_file(e.path())?; } }
    }
    Ok(())
}
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    if from.is_dir() { std::fs::create_dir_all(to)?; for e in std::fs::read_dir(from)?.flatten() { copy_tree(&e.path(), &to.join(e.file_name()))?; } Ok(()) } else { std::fs::copy(from, to).map(|_| ()) }
}

/// A game folder that holds something/// A game folder that holds something (a file or folder whose name does not start with a dot). An empty folder, such as the one an older Zest made
/// for "drop the downloaded files in here", is not an install.
pub fn has_content(dir: &Path) -> bool { std::fs::read_dir(dir).map_or(false, |rd| rd.flatten().any(|e| !e.file_name().to_string_lossy().starts_with('.'))) }
pub fn mb(n: u64) -> String { format!("{:.1}", n as f64 / 1e6) }

#[cfg(test)]
mod pick_tests {
    use super::*;
    fn elf(p: &Path) { if let Some(d) = p.parent() { std::fs::create_dir_all(d).unwrap(); } std::fs::write(p, [0x7fu8, b'E', b'L', b'F', 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0]).unwrap(); }
    #[cfg(target_os = "linux")]
    #[test] fn a_tie_is_asked_about_a_choice_wins_and_all_candidates_are_listed() {
        let d = std::env::temp_dir().join(format!("zest_pick_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        assert_eq!(choose_executable(&d, None), Choice::Nothing);
        elf(&d.join("Game.x86_64"));
        assert_eq!(choose_executable(&d, None), Choice::File("Game.x86_64".into()), "one best file needs no question");
        elf(&d.join("Editor"));
        match choose_executable(&d, None) { Choice::Ask(c) => assert_eq!(c, vec!["Editor".to_string(), "Game.x86_64".to_string()]), c => panic!("{c:?}") }
        assert_eq!(choose_executable(&d, Some("Editor")), Choice::File("Editor".into()), "a saved choice is never asked about again");
        assert!(matches!(choose_executable(&d, Some("gone")), Choice::Ask(_)), "a saved choice that is gone is asked about again");
        // the launch result: ask, run, or open the folder
        let l = launch_choice(&d, Choice::Ask(vec!["a".into()]), &|_, _| unreachable!()).unwrap();
        assert!(!l.record && matches!(l.action, Action::PickExecutable { ref candidates, .. } if candidates == &vec!["a".to_string()]));
        assert!(matches!(launch_choice(&d, Choice::Nothing, &|_, _| unreachable!()).unwrap().action, Action::OpenUrl(_)));
        assert!(launch_choice(&d, Choice::File("Editor".into()), &|abs, cwd| { assert_eq!((abs, cwd), (d.join("Editor").as_path(), d.as_path())); run_file(abs, cwd, None) }).is_ok());
        let all = crate::native_exe::all_candidates(&d, crate::native_exe::Os::Linux);
        assert_eq!(all.len(), 2); assert!(all.contains(&"Editor".to_string()) && all.contains(&"Game.x86_64".to_string()));
    }
}

#[cfg(test)]
mod setup_and_html_tests {
    use super::*;
    fn tmp(tag: &str) -> std::path::PathBuf { let d = std::env::temp_dir().join(format!("zest_folder_{tag}_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d }
    fn exe(p: &Path, mark: &[u8]) { let mut b = b"MZ\x90\x00".to_vec(); b.extend(vec![0u8; 5000]); b.extend_from_slice(mark); b.extend(vec![0u8; 100]); std::fs::write(p, b).unwrap(); }
    #[test] fn setup_programs_are_known_by_their_contents_not_their_names() {
        let d = tmp("kind");
        exe(&d.join("MyGame-1.0.exe"), b"Inno Setup Setup Data (6.2.0)"); exe(&d.join("x.exe"), b"Nullsoft Install System v3.08"); exe(&d.join("Setup.exe"), b"UnityPlayer");
        std::fs::write(d.join("pkg.msi"), b"x").unwrap(); std::fs::write(d.join("notes.txt"), b"Inno Setup Setup Data").unwrap(); std::fs::write(d.join("fake.exe"), b"Inno Setup Setup Data").unwrap();
        assert_eq!(setup_kind(&d.join("MyGame-1.0.exe")), Some(Setup::Inno));
        assert_eq!(setup_kind(&d.join("x.exe")), Some(Setup::Nsis));
        assert_eq!(setup_kind(&d.join("pkg.msi")), Some(Setup::Msi));
        assert_eq!(setup_kind(&d.join("Setup.exe")), None, "a game merely named Setup is not a setup program");
        assert_eq!(setup_kind(&d.join("notes.txt")), None); assert_eq!(setup_kind(&d.join("fake.exe")), None, "not a Windows program");
    }
    #[test] fn only_a_folder_that_is_just_a_setup_program_is_installed_by_running_it() {
        let one = tmp("one"); exe(&one.join("game_setup.exe"), b"Inno Setup Setup Data"); std::fs::write(one.join("readme.txt"), "x").unwrap();
        assert_eq!(setup_in(&one).map(|(p, k)| (p.file_name().unwrap().to_string_lossy().to_string(), k)), Some(("game_setup.exe".to_string(), Setup::Inno)));
        let game = tmp("game"); exe(&game.join("Game.exe"), b"UnityPlayer"); exe(&game.join("setup.exe"), b"Nullsoft Install System");
        assert_eq!(setup_in(&game), None, "a game that ships an installer beside itself is left alone");
        let redist = tmp("redist"); exe(&redist.join("vcredist_x64.exe"), b"Nullsoft Install System");
        assert_eq!(setup_in(&redist), None);
        let two = tmp("two"); exe(&two.join("a.exe"), b"Inno Setup Setup Data"); exe(&two.join("b.exe"), b"Inno Setup Setup Data");
        assert_eq!(setup_in(&two), None, "two setup programs: Zest cannot tell which");
    }
    #[test] fn silent_arguments_name_the_target_the_way_each_installer_wants() {
        let (i, t) = (Path::new("/g/setup.exe"), Path::new("/home/r/Games/x/game"));
        let tw = if cfg!(windows) { "/home/r/Games/x/game".to_string() } else { "Z:\\home\\r\\Games\\x\\game".to_string() };
        assert_eq!(setup_args(Setup::Inno, i, t, true).1, vec!["/VERYSILENT".to_string(), "/SUPPRESSMSGBOXES".into(), "/NORESTART".into(), "/SP-".into(), format!("/DIR={tw}")]);
        assert_eq!(setup_args(Setup::Nsis, i, t, true).1.last().unwrap(), &format!("/D={tw}"), "NSIS wants /D= last");
        assert_eq!(setup_args(Setup::Nsis, i, t, false).1, vec![format!("/D={tw}")]);
        let (prog, args) = setup_args(Setup::Msi, Path::new("/g/p.msi"), t, true);
        assert_eq!(prog, "msiexec"); assert!(args.contains(&"/qn".to_string()) && args.contains(&format!("TARGETDIR={tw}")));
    }
    #[test] fn a_browser_game_is_its_index_page_or_its_only_page() {
        let a = tmp("html_a"); std::fs::write(a.join("Nothing.html"), "x").unwrap();
        assert_eq!(html_game(&a), Some(a.join("Nothing.html")));
        let b = tmp("html_b"); std::fs::create_dir_all(b.join("build")).unwrap(); std::fs::write(b.join("build/index.html"), "x").unwrap(); std::fs::write(b.join("build/other.html"), "x").unwrap(); std::fs::create_dir_all(b.join("__MACOSX")).unwrap();
        assert_eq!(html_game(&b), Some(b.join("build/index.html")));
        let c = tmp("html_c"); std::fs::write(c.join("a.html"), "x").unwrap(); std::fs::write(c.join("b.html"), "x").unwrap();
        assert_eq!(html_game(&c), None, "two pages and no index: unknown");
        assert_eq!(launch_choice(&a, Choice::Nothing, &|_, _| unreachable!()).unwrap(), Launch { action: Action::OpenUrl(a.join("Nothing.html").to_string_lossy().to_string()), record: true });
    }
}

/// The real thing, through this machine's Wine or Proton, with a stand-in NSIS setup program (built from source; see NATIVE_UI_PLAN.md):
/// `ZEST_SETUP_EXE=<setup.exe> cargo test -p pd-filter real_setup_program -- --ignored --nocapture`. Uses a throwaway folder and Wine prefix.
#[cfg(test)]
mod real_setup_tests {
    #[test] #[ignore] fn real_setup_program_installs_silently_into_the_game_folder() {
        let Some(exe) = std::env::var_os("ZEST_SETUP_EXE").map(std::path::PathBuf::from) else { return };
        // under the home folder: Proton's Steam Runtime container has a private /tmp, so a game folder there is invisible to the installer
        let root = crate::installdir::home().join(format!(".cache/zest_real_setup_{}", std::process::id())); let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("Zest Setup Test Game"); std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(&exe, dir.join(exe.file_name().unwrap())).unwrap();
        let prefix = root.join(".prefixes/test");
        let mut steps = vec![];
        let r = super::run_setup(&dir, &prefix, &crate::net::Cancel::new(), &mut |m| { println!("progress: {m}"); steps.push(m.to_string()); });
        println!("result: {r:?}");
        for f in std::fs::read_dir(dir.join("game")).into_iter().flatten().flatten() { println!("installed: {}", f.path().display()); }
        assert!(matches!(r, Ok(Some(_))), "{r:?}");
        assert!(dir.join("game/ZestSetupTestGame.exe").is_file() && !dir.join(exe.file_name().unwrap()).exists());
        assert_eq!(steps.len(), 1, "silent was enough");
        assert_eq!(crate::native_exe::pick_executable(&dir, crate::native_exe::Os::current()).path.as_deref(), Some("game/ZestSetupTestGame.exe"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
