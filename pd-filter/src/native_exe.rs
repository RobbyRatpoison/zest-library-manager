//! Port of PlayDate's `runners/native_exe.py`: which file to launch in a game folder that ships no manifest (itch.io, IndieGala, Humble, GOG's fallback). Paths returned are
//! relative to the folder, `/`-separated, as `games.platform_executable` holds them. The 32-bit/64-bit ranking matters: Unity ships `Foo.x86` next to `Foo.x86_64`, and the
//! 32-bit one cannot run on a 64-bit-only system.
use std::path::Path;

const SKIP_EXE_PREFIXES: &[&str] = &["setup", "install", "unins", "uninst", "redist"];
const HELPER_EXE_NAMES: &[&str] = &["unitycrashhandler64", "unitycrashhandler32", "unitycrashhandler", "unityplayer", "dxsetup", "dxwebsetup", "vcredist_x64", "vcredist_x86", "vc_redist.x64", "vc_redist.x86", "dotnetfx", "dotnet"];

fn head(p: &Path, n: usize) -> Vec<u8> { use std::io::Read; let mut b = vec![0u8; n]; match std::fs::File::open(p).and_then(|mut f| f.read(&mut b)) { Ok(k) => { b.truncate(k); b } Err(_) => vec![] } }
pub(crate) fn is_elf(p: &Path) -> bool { head(p, 4) == b"\x7fELF" }
fn is_macho(p: &Path) -> bool { matches!(head(p, 4).as_slice(), [0xfe, 0xed, 0xfa, 0xce] | [0xfe, 0xed, 0xfa, 0xcf] | [0xce, 0xfa, 0xed, 0xfe] | [0xcf, 0xfa, 0xed, 0xfe] | [0xca, 0xfe, 0xba, 0xbe]) }
fn elf_bits(p: &Path) -> Option<u8> { let h = head(p, 5); if h.len() < 5 || &h[..4] != b"\x7fELF" { return None; } match h[4] { 1 => Some(32), 2 => Some(64), _ => None } }

/// Sort priority of a native candidate (lower = preferred): 64-bit ELF first, 32-bit last, the extension convention when the file is not a readable ELF.
pub(crate) fn native_rank(p: &Path) -> u8 {
    match elf_bits(p) { Some(64) => return 0, Some(32) => return 2, _ => {} }
    let ext = p.extension().and_then(|e| e.to_str()).map(|e| format!(".{}", e.to_lowercase())).unwrap_or_default();
    match ext.as_str() { ".x86_64" | ".amd64" | ".arm64" => 0, ".x86" => 2, _ => 1 }
}

#[derive(Debug, Default, PartialEq)]
pub struct Candidates { pub appimages: Vec<String>, pub natives: Vec<(u8, usize, String)>, pub scripts: Vec<String>, pub winexes: Vec<(usize, String)> }

#[derive(Clone, Copy, PartialEq)]
pub enum Os { Linux, Windows, Mac }
impl Os { pub fn current() -> Os { if cfg!(windows) { Os::Windows } else if cfg!(target_os = "macos") { Os::Mac } else { Os::Linux } } }

fn walk(dir: &Path, base: &Path, out: &mut Vec<(std::path::PathBuf, String)>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let Ok(ft) = e.file_type() else { continue };
        let path = e.path();
        // a symlink to a directory is walked like Python's os.walk does not (followlinks=False): it is listed as a directory entry, not entered
        if ft.is_dir() { if !name.starts_with('.') { walk(&path, base, out); } }
        else if ft.is_file() || (ft.is_symlink() && !path.is_dir()) { if !name.starts_with('.') { let rel = path.strip_prefix(base).map(|r| r.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect::<Vec<_>>().join("/")).unwrap_or_default(); out.push((path.clone(), rel)); } }
    }
}

/// `scan_candidates`: every plausible launch candidate under `install`.
pub fn scan_candidates(install: &Path, os: Os) -> Candidates {
    let mut files = vec![]; walk(install, install, &mut files);
    let mut c = Candidates::default();
    for (path, rel) in files {
        let fname = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy().to_lowercase())).unwrap_or_default();
        let stem = path.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
        let depth = rel.matches('/').count();
        match ext.as_str() {
            ".appimage" => c.appimages.push(rel),
            ".x86_64" | ".x86" | ".amd64" | ".arm64" | ".linux" => c.natives.push((native_rank(&path), depth, rel)),
            ".sh" if os != Os::Windows => c.scripts.push(rel),
            "" if os != Os::Windows && (is_elf(&path) || (os == Os::Mac && is_macho(&path))) => c.natives.push((native_rank(&path), depth, rel)),
            ".exe" => { if SKIP_EXE_PREFIXES.iter().any(|p| stem.starts_with(p)) || HELPER_EXE_NAMES.contains(&stem.as_str()) { continue; } c.winexes.push((depth, rel)); }
            _ => {}
        }
        let _ = fname;
    }
    c
}

/// Every file that could start a game in `install`, best first (AppImages, native binaries, scripts, Windows programs; a mac bundle alone), for the "Change Executable" list.
pub fn all_candidates(install: &Path, os: Os) -> Vec<String> {
    if let Some(app) = find_mac_app_bundle(install, os) { return vec![app]; }
    let c = scan_candidates(install, os);
    let mut out = c.appimages; out.sort();
    let mut n = c.natives; n.sort(); out.extend(n.into_iter().map(|(_, _, r)| r));
    let mut sc = c.scripts; sc.sort(); out.extend(sc);
    let mut w = c.winexes; w.sort(); out.extend(w.into_iter().map(|(_, r)| r));
    out
}

/// A macOS `.app` bundle's inner executable (or the bundle), relative to `install`; None elsewhere or when there is none.
pub fn find_mac_app_bundle(install: &Path, os: Os) -> Option<String> {
    if os != Os::Mac { return None; }
    for e in std::fs::read_dir(install).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !name.ends_with(".app") || !e.path().is_dir() { continue; }
        let macos = e.path().join("Contents").join("MacOS");
        if macos.is_dir() {
            for inner in std::fs::read_dir(&macos).ok()?.flatten() {
                #[cfg(unix)] let exec = { use std::os::unix::fs::PermissionsExt; inner.metadata().map_or(false, |m| m.permissions().mode() & 0o111 != 0) };
                #[cfg(not(unix))] let exec = true;
                if inner.path().is_file() && exec { return Some(format!("{name}/Contents/MacOS/{}", inner.file_name().to_string_lossy())); }
            }
        }
        return Some(name);
    }
    None
}

#[derive(Debug, PartialEq)]
pub struct Picked { pub path: Option<String>, pub is_windows: bool, pub ambiguous: bool, pub candidates: Vec<String> }

fn tied_by_depth(mut group: Vec<String>) -> (String, Vec<String>) {
    group.sort();
    let best = group[0].matches('/').count();
    let tied = group.iter().filter(|p| p.matches('/').count() == best).cloned().collect();
    (group[0].clone(), tied)
}

/// `pick_executable`: a mac bundle first, then AppImages > native binaries > scripts > Windows executables; `ambiguous` when the winning group has several tied for the best.
pub fn pick_executable(install: &Path, os: Os) -> Picked {
    if let Some(app) = find_mac_app_bundle(install, os) { return Picked { path: Some(app.clone()), is_windows: false, ambiguous: false, candidates: vec![app] }; }
    let c = scan_candidates(install, os);
    if !c.appimages.is_empty() { let (p, tied) = tied_by_depth(c.appimages); return Picked { path: Some(p), is_windows: false, ambiguous: tied.len() > 1, candidates: tied }; }
    if !c.natives.is_empty() {
        let mut g = c.natives; g.sort();
        let (r, d) = (g[0].0, g[0].1);
        let tied: Vec<String> = g.iter().filter(|(rank, depth, _)| *rank == r && *depth == d).map(|(_, _, rel)| rel.clone()).collect();
        return Picked { path: Some(g[0].2.clone()), is_windows: false, ambiguous: tied.len() > 1, candidates: tied };
    }
    if !c.scripts.is_empty() { let (p, tied) = tied_by_depth(c.scripts); return Picked { path: Some(p), is_windows: false, ambiguous: tied.len() > 1, candidates: tied }; }
    if !c.winexes.is_empty() {
        let mut g = c.winexes; g.sort();
        let d = g[0].0;
        let tied: Vec<String> = g.iter().filter(|(depth, _)| *depth == d).map(|(_, rel)| rel.clone()).collect();
        return Picked { path: Some(g[0].1.clone()), is_windows: true, ambiguous: tied.len() > 1, candidates: tied };
    }
    Picked { path: None, is_windows: false, ambiguous: false, candidates: vec![] }
}
