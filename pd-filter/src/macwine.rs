//! Wine on macOS. Zest looks for one the person already has (on the path, Homebrew, a Wine app in Applications) and otherwise offers to download a ready-made build (the Gcenx macOS
//! builds of Wine Stable, about 185 MB) into its own folder, so nothing needs Homebrew or the Command Line Tools. Used by the launcher installs (Battle.net, Epic, ...), which run the
//! store's Windows launcher in a Wine prefix as on Linux. On an Apple Silicon Mac the build is an Intel one and needs Rosetta.
use crate::net::{Cancel, Http};
use std::path::{Path, PathBuf};

/// The build offered: Wine Stable for Intel Macs (Apple Silicon runs it through Rosetta).
pub const WINE_VERSION: &str = "11.0_1";
pub const WINE_URL: &str = "https://github.com/Gcenx/macOS_Wine_builds/releases/download/11.0_1/wine-stable-11.0_1-osx64.tar.xz";
pub const WINE_MB: u64 = 185;

/// The folder Zest keeps its own Wine in.
pub fn install_dir() -> PathBuf { crate::installdir::default_dir("wine", "wine") }

/// A `wine64` or `wine` program inside a Wine app folder (`Wine Stable.app`): `<app>/Contents/Resources/wine/bin/`.
fn in_app(app: &Path) -> Option<PathBuf> {
    let bin = app.join("Contents/Resources/wine/bin");
    ["wine64", "wine"].iter().map(|n| bin.join(n)).find(|p| p.is_file())
}

/// Wine apps directly inside `dir` (any folder whose name starts with "Wine" and ends in .app).
fn apps_in(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path())
        .filter(|p| p.extension().map_or(false, |x| x == "app") && p.file_name().map_or(false, |n| n.to_string_lossy().starts_with("Wine"))).collect();
    v.sort();
    v
}

/// The Wine a Mac already has: the one Zest downloaded, a Wine app in Applications (system or per-user), or None.
pub fn installed() -> Option<PathBuf> { installed_in(&[install_dir(), PathBuf::from("/Applications"), crate::installdir::home().join("Applications")]) }
pub fn installed_in(dirs: &[PathBuf]) -> Option<PathBuf> { dirs.iter().flat_map(|d| apps_in(d)).find_map(|a| in_app(&a)) }

/// Rosetta is what runs Intel programs on Apple Silicon; on an Intel Mac it is not needed. True when Wine can run.
pub fn can_run_intel() -> bool {
    if !cfg!(target_os = "macos") { return false; }
    let arm = std::process::Command::new("uname").arg("-m").output().map_or(false, |o| String::from_utf8_lossy(&o.stdout).trim() == "arm64");
    !arm || std::process::Command::new("pgrep").args(["-q", "oahd"]).status().map_or(false, |s| s.success())
}

/// Downloads and unpacks Wine into `install_dir()` and returns the program. `progress(phase, detail)` feeds the status line. Does nothing when one is already there.
pub fn install(http: &dyn Http, cancel: &Cancel, progress: &mut dyn FnMut(&str, &str)) -> Result<PathBuf, String> {
    if let Some(w) = installed() { return Ok(w); }
    if !can_run_intel() { return Err("This Mac needs Rosetta to run Wine. Open Terminal and run: softwareupdate --install-rosetta --agree-to-license, then try again.".into()); }
    let dest = install_dir();
    std::fs::create_dir_all(&dest).map_err(|e| format!("Could not make {}: {e}", dest.display()))?;
    let file = dest.join("wine-download.tar.xz");
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&file).map_err(|e| e.to_string())?;
        let (mut ok, total, mut got, mut shown, mut wrote_err) = (true, std::cell::Cell::new(0u64), 0u64, 0u64, None::<String>);
        progress("downloading", &format!("Downloading Wine (about {WINE_MB} MB)..."));
        let r = http.get_stream(WINE_URL, &[], &mut |status, len| { if status >= 400 { ok = false; } total.set(len.unwrap_or(0)); ok }, &mut |chunk| {
            if cancel.is_set() { return false; }
            if let Err(e) = f.write_all(chunk) { wrote_err = Some(e.to_string()); return false; }
            got += chunk.len() as u64;
            if got / 8_000_000 != shown { shown = got / 8_000_000; progress("downloading", &if total.get() > 0 { format!("Downloading Wine: {} of {} MB", got / 1_000_000, total.get() / 1_000_000) } else { format!("Downloading Wine: {} MB", got / 1_000_000) }); }
            true });
        drop(f);
        if cancel.is_set() { let _ = std::fs::remove_file(&file); return Err("Cancelled".into()); }
        if let Err(e) = r { let _ = std::fs::remove_file(&file); return Err(format!("Download failed: {}", e.0)); }
        if let Some(e) = wrote_err { let _ = std::fs::remove_file(&file); return Err(format!("Download failed: {e}")); }
        if !ok { let _ = std::fs::remove_file(&file); return Err("Download failed: the server refused the request.".into()); }
    }
    progress("installing", "Unpacking Wine...");
    let out = std::process::Command::new("tar").arg("-xJf").arg(&file).arg("-C").arg(&dest).output().map_err(|e| format!("Could not unpack Wine: {e}"))?;
    let _ = std::fs::remove_file(&file);
    if !out.status.success() { return Err(format!("Could not unpack Wine: {}", String::from_utf8_lossy(&out.stderr).trim())); }
    let wine = installed().ok_or("Wine was unpacked, but its program was not found in it.")?;
    crate::net::log(&format!("Wine {WINE_VERSION} installed at {}", wine.display()));
    Ok(wine)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn a_wine_app_is_found_by_its_program_and_other_apps_are_ignored() {
        let d = std::env::temp_dir().join(format!("zest_macwine_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d);
        let bin = d.join("Wine Stable.app/Contents/Resources/wine/bin"); std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(d.join("Safari.app/Contents/Resources/wine/bin")).unwrap();
        assert!(installed_in(&[d.clone()]).is_none(), "an app without the program does not count");
        std::fs::write(bin.join("wine64"), b"x").unwrap();
        assert_eq!(installed_in(&[d.join("nowhere"), d.clone()]).unwrap(), bin.join("wine64"));
        let _ = std::fs::remove_dir_all(&d);
    }
    #[test] fn the_download_is_one_pinned_build() { assert!(WINE_URL.contains(WINE_VERSION) && WINE_URL.ends_with("osx64.tar.xz") && install_dir().ends_with("wine")); }
}
