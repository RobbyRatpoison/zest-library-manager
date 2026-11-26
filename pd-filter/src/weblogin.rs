//! Sign in through `zest-login`, a small helper program that shows the store's own login page in the system web view (see `zest-login/src/main.rs`).
//! Zest starts it only when the player presses Sign in; it is a separate process, so the web engine's memory exists only while the window is open and the page where the
//! password is typed never runs inside Zest. What it prints is the same value the store's paste box takes, so `auth_complete` handles both the same way.
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// The helper next to the running program (`ZEST_LOGIN_HELPER` overrides, for tests); None when it is not installed, in which case the paste flow is the only way.
pub fn helper_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("ZEST_LOGIN_HELPER") { let p = PathBuf::from(p); return p.is_file().then_some(p); }
    let exe = std::env::current_exe().ok()?;
    let name = if cfg!(windows) { "zest-login.exe" } else { "zest-login" };
    let p = exe.parent()?.join(name);
    p.is_file().then_some(p)
}

/// Show `url` in a sign-in window until `done` (see the helper's spec) is met: the value, or why there is none. Blocks until the window finishes; call it off the UI thread.
pub fn run(helper: &std::path::Path, title: &str, url: &str, done: &serde_json::Value, timeout_secs: u64) -> Result<String, String> { run_as(helper, title, url, done, timeout_secs, None) }
/// `run`, presenting the window as the given browser (a store whose bot check refuses the embedded browser's own name).
pub fn run_as(helper: &std::path::Path, title: &str, url: &str, done: &serde_json::Value, timeout_secs: u64, user_agent: Option<&str>) -> Result<String, String> {
    let mut spec = serde_json::json!({ "title": title, "url": url, "timeout_secs": timeout_secs, "done": done });
    if let Some(ua) = user_agent { spec["user_agent"] = serde_json::json!(ua); }
    let spec = spec.to_string();
    // the helper writes where its window went and which cookie NAMES it saw (never a value) to a file in the temp folder; whatever a sign-in does, the trail ends up in Zest's own log
    let trace_file = std::env::temp_dir().join("zest-login.log"); let _ = std::fs::remove_file(&trace_file);
    let mut cmd = Command::new(helper);
    cmd.arg(spec).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(windows)] { use std::os::windows::process::CommandExt; cmd.creation_flags(0x0800_0000); }   // CREATE_NO_WINDOW: the helper is a console program that only prints one line; no black window beside its own
    let out = cmd.output().map_err(|e| format!("The sign-in window could not start: {e}"))?;
    let result = parse_output(&String::from_utf8_lossy(&out.stdout));
    if let Ok(t) = std::fs::read_to_string(&trace_file) { let mut last = ""; for l in t.lines().filter(|l| { let new = *l != last; last = *l; new }).take(80) { crate::net::log(&format!("sign-in window ({title}): {l}")); } let _ = std::fs::remove_file(&trace_file); }
    crate::net::log(&format!("sign-in window ({title}): {}", match &result { Ok(v) => format!("gave an answer of {} characters", v.chars().count()), Err(e) => format!("no answer: {e}") }));
    result
}
/// The helper's one line of JSON: the value, or a message for the player.
pub fn parse_output(stdout: &str) -> Result<String, String> {
    let line = stdout.lines().rev().find(|l| l.trim_start().starts_with('{')).ok_or("The sign-in window could not open here (it printed nothing). Use Sign in in browser instead.")?;
    let v: serde_json::Value = serde_json::from_str(line).map_err(|_| "The sign-in window gave an answer Zest could not read.".to_string())?;
    if v["ok"].as_bool() == Some(true) {
        return v["value"].as_str().filter(|s| !s.is_empty()).map(String::from).ok_or_else(|| "The sign-in finished but gave nothing back.".to_string());
    }
    Err(match v["error"].as_str().unwrap_or("") {
        "closed" => "The sign-in window was closed before signing in finished.".to_string(),
        "timeout" => "The sign-in window waited too long and closed.".to_string(),
        e if e.starts_with("the web view could not start") || e.starts_with("no window") => format!("The sign-in window could not open here ({e}). Use Sign in in browser instead."),
        e => format!("The sign-in window failed: {e}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn the_value_or_a_plain_message_comes_back() {
        assert_eq!(parse_output("noise\n{\"ok\":true,\"value\":\"https://x/?code=1\"}\n").unwrap(), "https://x/?code=1");
        assert!(parse_output("{\"ok\":false,\"error\":\"closed\"}").unwrap_err().contains("closed"));
        assert!(parse_output("{\"ok\":false,\"error\":\"timeout\"}").unwrap_err().contains("too long"));
        assert!(parse_output("{\"ok\":false,\"error\":\"the web view could not start: x\"}").unwrap_err().contains("in browser"));
        assert!(parse_output("").is_err());
        assert!(parse_output("{\"ok\":true,\"value\":\"\"}").is_err(), "an empty value is not a sign-in");
    }
    #[cfg(unix)]
    #[test] fn a_helper_is_started_with_the_spec_and_its_line_is_read() {
        use std::os::unix::fs::PermissionsExt;
        let d = std::env::temp_dir().join(format!("zest_weblogin_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        let h = d.join("fake-login");
        // the fake prints the title it was given inside the value, so the test sees the spec arrive
        std::fs::write(&h, "#!/bin/sh\ncase \"$1\" in *'\"title\":\"Sign in to X\"'*) echo '{\"ok\":true,\"value\":\"got-the-spec\"}';; *) echo '{\"ok\":false,\"error\":\"no spec\"}';; esac\n").unwrap();
        std::fs::set_permissions(&h, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(run(&h, "Sign in to X", "https://x/", &serde_json::json!({"kind":"url","contains":"code="}), 60).unwrap(), "got-the-spec");
        assert!(run(&d.join("missing"), "t", "https://x/", &serde_json::json!({}), 60).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
}
