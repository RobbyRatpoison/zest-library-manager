//! Looking for a newer Zest (PlayDate's `updater.py`, the check and the choice of download; not the in-place install). Zest's releases are in its own GitHub repository
//! (`zest-library-manager`; PlayDate's stays separate so an installed PlayDate never picks up a Zest download): tags `v<version>`, versions like `2026.11.1` or `2026.11.1-beta.2`,
//! a prerelease ranking below the final release of the same number. A stable install is only
//! offered final releases; a beta install (or one that asked for betas) also the prereleases. A version an earlier update rolled back from is never offered again. The download
//! that fits this install is chosen by its file name: the `.flatpak`, the Windows `.exe` or zip, `*-linux-<arch>.tar.gz`, `*-darwin-<arch>.tar.gz`.
use crate::net::{Cancel, Http};
use serde_json::Value;
use std::time::Duration;

/// This build's version. CI stamps it from the tag (`ZEST_BUILD_VERSION`); a local build says it is a beta of the first native release.
pub const VERSION: &str = match option_env!("ZEST_BUILD_VERSION") { Some(v) => v, None => "2026.11.0-beta.1" };
/// The repositories the check asks, first answer wins.
pub const REPOS: &[&str] = &["RobbyRatpoison/zest-library-manager"];
pub const TAG_PREFIX: &str = "v";

/// `_parse_build_version`: the numbers and the prerelease number (None = a final release; `-beta` with no number is 0). Unreadable numbers are 0.0.0.
pub fn parse_version(v: &str) -> (Vec<u64>, Option<u64>) {
    let v = v.trim().trim_start_matches('v');
    let (base, suffix) = match v.split_once('-') { Some((b, s)) => (b, Some(s)), None => (v, None) };
    let nums: Option<Vec<u64>> = base.split('.').map(|x| x.parse::<u64>().ok()).collect();
    let nums = nums.unwrap_or_else(|| vec![0, 0, 0]);
    let pre = suffix.map(|s| { let d: String = s.chars().rev().take_while(|c| c.is_ascii_digit()).collect::<Vec<_>>().into_iter().rev().collect(); d.parse::<u64>().unwrap_or(0) });
    (nums, pre)
}

/// `_build_is_newer`: is `a` newer than `b` (a final release beats any prerelease of the same numbers).
pub fn is_newer(a: &str, b: &str) -> bool {
    let ((an, ap), (bn, bp)) = (parse_version(a), parse_version(b));
    if an != bn { return an > bn; }
    match (ap, bp) { (None, bp) => bp.is_some(), (Some(_), None) => false, (Some(x), Some(y)) => x > y }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Install { Flatpak, Windows, Linux, Mac }
impl Install { pub fn current() -> Install { if cfg!(windows) { Install::Windows } else if cfg!(target_os = "macos") { Install::Mac } else if crate::sandbox::in_flatpak() { Install::Flatpak } else { Install::Linux } } }

#[derive(Clone, Debug, PartialEq)]
pub struct Asset { pub name: String, pub url: String, pub size: u64 }

#[derive(Clone, Debug, PartialEq)]
pub struct Update { pub version: String, pub notes: String, pub page: String, pub prerelease: bool, pub asset: Option<Asset> }

/// The download for this kind of install among a release's files.
pub fn choose_asset(assets: &[Asset], kind: Install, arch: &str) -> Option<Asset> {
    let l = |a: &Asset| a.name.to_lowercase();
    let pick = |f: &dyn Fn(&str) -> bool| assets.iter().find(|a| f(&l(a))).cloned();
    match kind {
        Install::Flatpak => pick(&|n| n.ends_with(".flatpak")),
        Install::Windows => pick(&|n| n.ends_with(".exe") && n.contains("zest")).or_else(|| pick(&|n| n.ends_with(".zip") && n.contains("windows"))),
        Install::Linux => pick(&|n| n.ends_with(".tar.gz") && n.contains("-linux-") && n.contains(arch)),
        Install::Mac => pick(&|n| n.ends_with(".tar.gz") && n.contains("-darwin-") && n.contains(arch)),
    }
}

/// The newest release of Zest that this install may be offered: tags `v...`, not a draft, with files attached, a prerelease only when `beta`, newer than `current`, not a
/// version in `failed`, and with a download for this install attached (a release whose build is still uploading is not offered yet).
pub fn pick_release(releases: &Value, beta: bool, current: &str, failed: &[String], kind: Install, arch: &str) -> Option<Update> {
    let mut best: Option<(String, &Value)> = None;
    for r in releases.as_array()? {
        let tag = r["tag_name"].as_str().unwrap_or("");
        let Some(ver) = tag.strip_prefix(TAG_PREFIX) else { continue };
        if r["draft"].as_bool().unwrap_or(false) || r["assets"].as_array().map_or(true, |a| a.is_empty()) { continue; }
        if r["prerelease"].as_bool().unwrap_or(false) && !beta { continue; }
        if best.as_ref().map_or(true, |(b, _)| is_newer(ver, b)) { best = Some((ver.to_string(), r)); }
    }
    let (ver, r) = best?;
    if !is_newer(&ver, current) || failed.iter().any(|f| f == &ver) { return None; }
    let assets: Vec<Asset> = r["assets"].as_array()?.iter().filter_map(|a| Some(Asset { name: a["name"].as_str()?.to_string(), url: a["browser_download_url"].as_str()?.to_string(), size: a["size"].as_u64().unwrap_or(0) })).collect();
    let asset = choose_asset(&assets, kind, arch);
    if asset.is_none() { return None; }
    Some(Update { version: ver, notes: r["body"].as_str().unwrap_or("").trim().to_string(), page: r["html_url"].as_str().unwrap_or("").to_string(), prerelease: r["prerelease"].as_bool().unwrap_or(false), asset })
}

/// GitHub's API address. `ZEST_UPDATE_API` points the check at a test server, but only a local one (loopback, or the address a VM sees its host at): the update
/// source is not something the environment of a real run should be able to move.
fn api_base() -> String {
    match std::env::var("ZEST_UPDATE_API") { Ok(v) if ["http://127.0.0.1", "http://localhost", "http://10.0.2.2"].iter().any(|p| v.starts_with(p)) => v.trim_end_matches('/').to_string(), _ => "https://api.github.com".into() }
}

/// Asks GitHub (the first repository that answers). Ok(None) = up to date. Err = could not ask.
pub fn check(http: &dyn Http, repos: &[&str], beta: bool, failed: &[String]) -> Result<Option<Update>, String> {
    let mut last = String::from("Could not reach GitHub.");
    for repo in repos {
        match http.get(&format!("{}/repos/{repo}/releases?per_page=30", api_base()), &[("Accept", "application/vnd.github+json"), ("User-Agent", "Zest-App")], Duration::from_secs(10)) {
            Ok(r) if r.status == 200 => { let v: Value = serde_json::from_str(&r.body).map_err(|_| "GitHub sent an answer Zest could not read.".to_string())?; return Ok(pick_release(&v, beta, VERSION, failed, Install::current(), std::env::consts::ARCH)); }
            Ok(r) => last = format!("GitHub answered HTTP {}.", r.status),
            Err(e) => last = format!("Could not reach GitHub: {e}"),
        }
    }
    Err(last)
}

/// Versions an earlier update rolled back from (`.rollback/failed.json`, the file PlayDate's rollback keeps): never offered again.
pub fn failed_versions(root: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(root.join(".rollback/failed.json")).ok().and_then(|t| serde_json::from_str::<Vec<String>>(&t).ok()).unwrap_or_default()
}

/// The folder a download goes to: `~/Downloads` (else the home folder).
pub fn download_dir() -> std::path::PathBuf { let h = crate::installdir::home(); let d = h.join("Downloads"); if d.is_dir() { d } else { h } }

/// The command that installs a downloaded Flatpak bundle for this user (run on the host through `sandbox::wrap` inside a sandbox).
pub fn flatpak_install_args(bundle: &std::path::Path) -> Vec<String> { ["install", "--user", "-y", "--reinstall", &bundle.to_string_lossy()].iter().map(|s| s.to_string()).collect() }

/// Downloads the update's file to `dir`; returns where it is. Err("Cancelled") leaves nothing behind.
pub fn download(http: &dyn Http, a: &Asset, dir: &std::path::Path, cancel: &Cancel, progress: &mut dyn FnMut(u64, u64)) -> Result<std::path::PathBuf, String> {
    let name: String = a.name.chars().filter(|c| c.is_ascii_alphanumeric() || "._-".contains(*c)).collect();
    if name.is_empty() { return Err("The download has no usable name.".into()); }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let dest = dir.join(&name);
    crate::sources::folder::download(http, &a.url, &dest, cancel, progress)?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{NetError, Response};
    #[test] fn versions_compare_exactly_as_the_python_updater_does() {
        let g: Value = serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/update_golden.json")).unwrap()).unwrap();
        for p in g["pairs"].as_array().unwrap() { let (a, b, want) = (p[0].as_str().unwrap(), p[1].as_str().unwrap(), p[2].as_bool().unwrap()); assert_eq!(is_newer(a, b), want, "{a} vs {b}"); }
        for (v, w) in g["parsed"].as_object().unwrap() {
            let (n, pre) = parse_version(v);
            assert_eq!((n, pre), (w[0].as_array().unwrap().iter().map(|x| x.as_u64().unwrap()).collect::<Vec<_>>(), w[1].as_u64()), "{v}");
        }
    }
    fn rel(tag: &str, pre: bool, files: &[&str]) -> Value { serde_json::json!({"tag_name": tag, "prerelease": pre, "draft": false, "body": " notes ", "html_url": format!("https://github.com/x/y/releases/{tag}"),
        "assets": files.iter().map(|f| serde_json::json!({"name": f, "browser_download_url": format!("https://dl/{f}"), "size": 10})).collect::<Vec<_>>()}) }
    #[test] fn assets_are_chosen_by_install_type_and_architecture() {
        let a = |n: &str| Asset { name: n.into(), url: format!("https://dl/{n}"), size: 1 };
        let all = [a("Zest-2026.11.1-Linux.flatpak"), a("Zest-windows.zip"), a("zest-2026.11.1-linux-x86_64.tar.gz"), a("zest-2026.11.1-linux-aarch64.tar.gz"), a("zest-2026.11.1-darwin-arm64.tar.gz"), a("PlayDate-Setup-1.exe")];
        assert_eq!(choose_asset(&all, Install::Flatpak, "x86_64").unwrap().name, "Zest-2026.11.1-Linux.flatpak");
        assert_eq!(choose_asset(&all, Install::Windows, "x86_64").unwrap().name, "Zest-windows.zip", "PlayDate's installer is not Zest's");
        assert_eq!(choose_asset(&all, Install::Linux, "aarch64").unwrap().name, "zest-2026.11.1-linux-aarch64.tar.gz");
        assert_eq!(choose_asset(&all, Install::Mac, "arm64").unwrap().name, "zest-2026.11.1-darwin-arm64.tar.gz");
        assert!(choose_asset(&all, Install::Mac, "x86_64").is_none());
    }
    #[test] fn the_right_release_is_offered_to_stable_and_beta_installs() {
        let files = ["zest-1-linux-x86_64.tar.gz"];
        let rs = serde_json::json!([rel("v1.12.0", false, &files), rel("v2026.11.0", false, &files), rel("v2026.11.1-beta.2", true, &files), rel("v2026.11.1-beta.1", true, &files),
            rel("v2026.12.0", false, &[]), serde_json::json!({"tag_name": "v2027.1.0", "prerelease": false, "draft": true, "assets": [{"name": "zest-x-linux-x86_64.tar.gz", "browser_download_url": "u"}]})]);
        let pick = |beta, cur: &str, failed: &[&str]| pick_release(&rs, beta, cur, &failed.iter().map(|s| s.to_string()).collect::<Vec<_>>(), Install::Linux, "x86_64").map(|u| u.version);
        assert_eq!(pick(false, "2026.10.9", &[]), Some("2026.11.0".into()), "an older release, a beta, a draft and a release with no files are not offered to a stable install");
        assert_eq!(pick(true, "2026.11.0-beta.1", &[]), Some("2026.11.1-beta.2".into()));
        assert_eq!(pick(true, "2026.11.1-beta.2", &[]), None); assert_eq!(pick(false, "2026.11.0", &[]), None);
        assert_eq!(pick(true, "2026.11.0", &["2026.11.1-beta.2"]), None, "the newest is a version rolled back from: nothing is offered (as in Python; a newer one would be)");
        let u = pick_release(&rs, false, "2026.10.9", &[], Install::Linux, "x86_64").unwrap();
        assert_eq!((u.notes.as_str(), u.page.as_str(), u.prerelease, u.asset.unwrap().name.as_str()), ("notes", "https://github.com/x/y/releases/v2026.11.0", false, "zest-1-linux-x86_64.tar.gz"));
        assert!(pick_release(&rs, true, "2026.10.9", &[], Install::Mac, "arm64").is_none(), "no download for this install yet: not offered");
    }
    #[test] fn the_check_asks_github_and_tells_a_failure_from_being_up_to_date() {
        struct Gh(u16, String);
        impl Http for Gh { fn get(&self, u: &str, h: &[(&str, &str)], _t: Duration) -> Result<Response, NetError> {
            assert!(u.starts_with("https://api.github.com/repos/o/r/releases") && h.iter().any(|(k, _)| *k == "User-Agent"), "{u}");
            if self.0 == 0 { return Err(NetError("Io".into())); } Ok(Response { status: self.0, retry_after: None, body: self.1.clone() }) } }
        assert_eq!(check(&Gh(200, "[]".into()), &["o/r"], true, &[]), Ok(None));
        assert!(check(&Gh(403, String::new()), &["o/r"], true, &[]).unwrap_err().contains("403"));
        assert!(check(&Gh(0, String::new()), &["o/r"], true, &[]).unwrap_err().contains("Could not reach"));
        assert!(check(&Gh(200, "nope".into()), &["o/r"], true, &[]).is_err());
        assert!(flatpak_install_args(std::path::Path::new("/d/Zest.flatpak")).ends_with(&["/d/Zest.flatpak".to_string()]));
    }
}
