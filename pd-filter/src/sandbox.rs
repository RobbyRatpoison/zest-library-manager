//! Running things on the host when Zest runs inside a Flatpak sandbox (PlayDate's `runners/sandbox.py`). Wine, Proton, umu-run, winetricks and the games themselves live on the
//! host, not in the sandbox, so every program Zest starts goes through `flatpak-spawn --host` there (the app needs `--talk-name=org.freedesktop.Flatpak`). Outside a sandbox nothing
//! changes. Home folders are the same path on both sides (`--filesystem=home`), so paths need no translation.
use std::path::Path;

/// True inside a Flatpak sandbox.
pub fn in_flatpak() -> bool { Path::new("/.flatpak-info").exists() }

/// The program and arguments to start `program args` with `env` set (`Some`) or removed (`None`) and `dir` as the working folder: unchanged outside a sandbox, wrapped in
/// `flatpak-spawn --host` inside one (which has no "unset" switch, so a removed variable goes through `env -u`).
pub fn wrap(program: &str, args: &[String], dir: Option<&str>, env: &[(String, Option<String>)], sandboxed: bool) -> (String, Vec<String>) {
    if !sandboxed { return (program.to_string(), args.to_vec()); }
    let mut a: Vec<String> = vec!["--host".into()];
    if let Some(d) = dir.filter(|d| !d.is_empty()) { a.push(format!("--directory={d}")); }
    for (k, v) in env { if let Some(v) = v { a.push(format!("--env={k}={v}")); } }
    let unset: Vec<&String> = env.iter().filter(|(_, v)| v.is_none()).map(|(k, _)| k).collect();
    if unset.is_empty() { a.push(program.to_string()); } else {
        a.push("env".into());
        for k in unset { a.push("-u".into()); a.push(k.clone()); }
        a.push(program.to_string());
    }
    a.extend(args.iter().cloned());
    ("flatpak-spawn".to_string(), a)
}

/// `command -v name` on the host: the absolute path of a program there, or None.
pub fn host_which(name: &str) -> Option<std::path::PathBuf> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.') { return None; }
    let out = std::process::Command::new("flatpak-spawn").args(["--host", "sh", "-c", &format!("command -v {name}")]).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).output().ok()?;
    let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && p.starts_with('/')).then(|| std::path::PathBuf::from(p))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn outside_a_sandbox_nothing_changes_and_inside_everything_runs_on_the_host() {
        let args = vec!["--exec=launch Pro".to_string()];
        assert_eq!(wrap("/usr/bin/wine", &args, Some("/g"), &[("A".into(), Some("1".into()))], false), ("/usr/bin/wine".to_string(), args.clone()));
        let (p, a) = wrap("/usr/bin/wine", &args, Some("/g"), &[("WINEPREFIX".into(), Some("/p".into())), ("DISPLAY".into(), None), ("WAYLAND_DISPLAY".into(), None)], true);
        assert_eq!(p, "flatpak-spawn");
        assert_eq!(a, ["--host", "--directory=/g", "--env=WINEPREFIX=/p", "env", "-u", "DISPLAY", "-u", "WAYLAND_DISPLAY", "/usr/bin/wine", "--exec=launch Pro"]);
        let (_, a) = wrap("x", &[], None, &[], true); assert_eq!(a, ["--host", "x"]);
    }
    #[test] fn host_which_refuses_names_that_could_inject_a_command() {
        assert_eq!(host_which("wine; rm -rf ~"), None); assert_eq!(host_which(""), None); assert_eq!(host_which("$(id)"), None);
    }
}
