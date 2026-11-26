//! Where Zest keeps secrets (API keys, sign-in tokens, cookies): the operating system's own store instead of the plain `config.json`. One small JSON blob per owner
//! (`account/<steamid>`, `sgdb`, `plugin/<id>`) in a `Vault`: Linux `secret-tool` (libsecret: GNOME Keyring, KDE Wallet), macOS Keychain (`security`), Windows a DPAPI-protected
//! file per user (Credential Manager caps an entry at 2.5 KB; a Nintendo session cookie is 3.5 KB). `account.rs` is the only caller: it moves a secret in on first read or write
//! (verified by reading it back before the plaintext copy is removed), and without a vault (no keyring on this machine, a Deck in game mode) keeps today's plaintext behaviour.
//! UNVERIFIED on Windows and macOS: only the Linux backend has been run on a real system; the others are written from the platforms' documentation.
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

/// A place that holds named strings. `put` returning true means the value was written AND reads back.
pub trait Vault: Send + Sync {
    fn get(&self, name: &str) -> Option<String>;
    fn put(&self, name: &str, value: &str) -> bool;
    fn forget(&self, name: &str);
}

static VAULT: RwLock<Option<Arc<dyn Vault>>> = RwLock::new(None);
/// Makes `v` the vault for this run. Nothing is installed by default, so tests and tools never touch a real keyring.
pub fn install(v: Arc<dyn Vault>) { if let Ok(mut g) = VAULT.write() { *g = Some(v); } }
pub fn current() -> Option<Arc<dyn Vault>> { VAULT.read().ok().and_then(|g| g.clone()) }

/// Is this stored key a secret (kept in the vault) rather than a harmless value (a user name, an id, an expiry)? Decided by the key's name, so a plugin's own key names work without a list.
pub fn is_secret_key(key: &str) -> bool {
    let k = key.to_lowercase();
    ["token", "key", "cookie", "secret", "password", "ticket", "session", "auth", "uhs", "serial"].iter().any(|w| k.contains(w))
}

/// Remembers what was read or written so a token read many times does not start a program each time.
pub struct Cached { inner: Arc<dyn Vault>, seen: Mutex<HashMap<String, Option<String>>> }
impl Cached { pub fn new(inner: Arc<dyn Vault>) -> Self { Cached { inner, seen: Mutex::new(HashMap::new()) } } }
impl Vault for Cached {
    fn get(&self, name: &str) -> Option<String> {
        if let Some(v) = self.seen.lock().ok().and_then(|m| m.get(name).cloned()) { return v; }
        let v = self.inner.get(name); if let Ok(mut m) = self.seen.lock() { m.insert(name.to_string(), v.clone()); } v
    }
    fn put(&self, name: &str, value: &str) -> bool {
        // read back from the real store, not the cache: a store that swallows writes must not look like it worked
        let ok = self.inner.put(name, value) && self.inner.get(name).as_deref() == Some(value);
        if let Ok(mut m) = self.seen.lock() { if ok { m.insert(name.to_string(), Some(value.to_string())); } else { m.remove(name); } }
        ok
    }
    fn forget(&self, name: &str) { self.inner.forget(name); if let Ok(mut m) = self.seen.lock() { m.insert(name.to_string(), None); } }
}

/// The platform's store, when its tool is there; None = no vault on this machine.
pub fn platform_vault() -> Option<Arc<dyn Vault>> {
    #[cfg(target_os = "linux")] { if LinuxVault::have() { return Some(Arc::new(Cached::new(Arc::new(LinuxVault)))); } }
    #[cfg(target_os = "macos")] { return Some(Arc::new(Cached::new(Arc::new(MacVault)))); }
    #[cfg(target_os = "windows")] { return Some(Arc::new(Cached::new(Arc::new(WindowsVault)))); }
    #[allow(unreachable_code)] None
}

#[allow(dead_code)]
fn run(cmd: &mut std::process::Command, stdin: Option<&str>) -> Option<(bool, String)> {
    use std::io::Write; use std::process::Stdio;
    let mut child = cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    if let (Some(mut si), Some(text)) = (child.stdin.take(), stdin) { let _ = si.write_all(text.as_bytes()); }
    let out = child.wait_with_output().ok()?;
    Some((out.status.success(), String::from_utf8_lossy(&out.stdout).to_string()))
}

/// libsecret through the `secret-tool` program; the value travels over its stdin, never its arguments.
#[allow(dead_code)]
pub struct LinuxVault;
#[allow(dead_code)]
impl LinuxVault {
    fn have() -> bool { std::process::Command::new("secret-tool").arg("--version").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().is_ok() }
}
impl Vault for LinuxVault {
    fn get(&self, name: &str) -> Option<String> {
        let (ok, out) = run(std::process::Command::new("secret-tool").args(["lookup", "service", "zest", "kind", "vault", "name", name]), None)?;
        let t = out.trim_end_matches('\n').to_string(); (ok && !t.is_empty()).then_some(t)
    }
    fn put(&self, name: &str, value: &str) -> bool {
        run(std::process::Command::new("secret-tool").args(["store", "--label", "Zest", "service", "zest", "kind", "vault", "name", name]), Some(value)).map_or(false, |(ok, _)| ok)
    }
    fn forget(&self, name: &str) { let _ = run(std::process::Command::new("secret-tool").args(["clear", "service", "zest", "kind", "vault", "name", name]), None); }
}

/// The macOS Keychain through `security`. The value is an argument (the tool offers no stdin form), visible to the same user's `ps` for an instant. UNVERIFIED on a Mac.
#[allow(dead_code)]
pub struct MacVault;
impl Vault for MacVault {
    fn get(&self, name: &str) -> Option<String> {
        let (ok, out) = run(std::process::Command::new("security").args(["find-generic-password", "-a", name, "-s", "zest-vault", "-w"]), None)?;
        let t = out.trim_end_matches('\n').to_string(); (ok && !t.is_empty()).then_some(t)
    }
    fn put(&self, name: &str, value: &str) -> bool { run(std::process::Command::new("security").args(["add-generic-password", "-U", "-a", name, "-s", "zest-vault", "-w", value]), None).map_or(false, |(ok, _)| ok) }
    fn forget(&self, name: &str) { let _ = run(std::process::Command::new("security").args(["delete-generic-password", "-a", name, "-s", "zest-vault"]), None); }
}

/// Windows: one file per name under `%LOCALAPPDATA%\Zest\vault`, encrypted with the user's DPAPI key (only this Windows user can read it), through PowerShell. UNVERIFIED on Windows.
#[allow(dead_code)]
pub struct WindowsVault;
#[allow(dead_code)]
impl WindowsVault {
    fn file(name: &str) -> Option<std::path::PathBuf> {
        let dir = std::path::PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("Zest").join("vault");
        Some(dir.join(format!("{}.bin", name.bytes().map(|b| format!("{b:02x}")).collect::<String>())))
    }
    fn ps(script: &str, file: &std::path::Path, stdin: Option<&str>) -> Option<(bool, String)> {
        { use crate::proc::Hide; run(std::process::Command::new("powershell").args(["-NoProfile", "-NonInteractive", "-Command", script]).env("ZEST_VAULT_FILE", file).hidden(), stdin) }
    }
}
impl Vault for WindowsVault {
    fn get(&self, name: &str) -> Option<String> {
        let f = Self::file(name)?; if !f.exists() { return None; }
        let (ok, out) = Self::ps("Add-Type -AssemblyName System.Security; $b=[IO.File]::ReadAllBytes($env:ZEST_VAULT_FILE); $p=[Security.Cryptography.ProtectedData]::Unprotect($b,$null,'CurrentUser'); [Console]::Out.Write([Text.Encoding]::UTF8.GetString($p))", &f, None)?;
        (ok && !out.is_empty()).then_some(out)
    }
    fn put(&self, name: &str, value: &str) -> bool {
        let Some(f) = Self::file(name) else { return false };
        if let Some(d) = f.parent() { let _ = std::fs::create_dir_all(d); }
        Self::ps("Add-Type -AssemblyName System.Security; $v=[Console]::In.ReadToEnd(); $b=[Text.Encoding]::UTF8.GetBytes($v); $p=[Security.Cryptography.ProtectedData]::Protect($b,$null,'CurrentUser'); [IO.File]::WriteAllBytes($env:ZEST_VAULT_FILE,$p)", &f, Some(value)).map_or(false, |(ok, _)| ok)
    }
    fn forget(&self, name: &str) { if let Some(f) = Self::file(name) { let _ = std::fs::remove_file(f); } }
}

// ───────────────────────── blobs: the owner's secrets as one JSON object ─────────────────────────

pub fn blob_get(v: &dyn Vault, name: &str) -> Map<String, Value> { v.get(name).and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|j| j.as_object().cloned()).unwrap_or_default() }
/// Writes the blob (an empty one removes the entry). True = stored and read back.
pub fn blob_put(v: &dyn Vault, name: &str, m: &Map<String, Value>) -> bool {
    if m.is_empty() { v.forget(name); return true; }
    v.put(name, &Value::Object(m.clone()).to_string())
}

#[cfg(test)]
pub mod testing {
    use super::*;
    /// A vault in memory; `broken` makes every write fail, like a locked keyring.
    #[derive(Default)] pub struct Mem { pub map: Mutex<HashMap<String, String>>, pub broken: std::sync::atomic::AtomicBool }
    impl Vault for Mem {
        fn get(&self, n: &str) -> Option<String> { self.map.lock().unwrap().get(n).cloned() }
        fn put(&self, n: &str, v: &str) -> bool { if self.broken.load(std::sync::atomic::Ordering::SeqCst) { return false; } self.map.lock().unwrap().insert(n.into(), v.into()); true }
        fn forget(&self, n: &str) { self.map.lock().unwrap().remove(n); }
    }
}

#[cfg(test)]
mod tests {
    use super::*; use super::testing::Mem;
    #[test] fn secrets_are_told_from_harmless_keys_by_name() {
        for k in ["access_token", "refresh_token", "api_key", "session_cookie", "web_cookies", "cookie", "xsts_token", "uhs", "device_serial", "password", "X-Auth", "client_secret", "ticket"] { assert!(is_secret_key(k), "{k}"); }
        for k in ["username", "display_name", "xuid", "gamertag", "online_id", "account_id", "galaxy_user_id", "expires_at", "refresh_expires", "nickname", "battletag", "label"] { assert!(!is_secret_key(k), "{k}"); }
    }
    #[test] fn a_write_counts_only_when_it_reads_back_and_a_failed_one_is_not_remembered() {
        let inner = Arc::new(Mem::default()); let c = Cached::new(inner.clone());
        assert!(c.put("a", "1")); assert_eq!(c.get("a").as_deref(), Some("1"));
        inner.broken.store(true, std::sync::atomic::Ordering::SeqCst); assert!(!c.put("a", "2"), "a locked store fails");
        assert_eq!(c.get("a").as_deref(), Some("1"), "what the store really holds is served, not the value that failed to write");
        inner.broken.store(false, std::sync::atomic::Ordering::SeqCst); c.forget("a"); assert_eq!(c.get("a"), None);
    }
    #[test] fn blobs_roundtrip_and_an_empty_one_removes_the_entry() {
        let m = Mem::default(); let mut b = Map::new(); b.insert("k".into(), Value::String("v".into()));
        assert!(blob_put(&m, "plugin/x", &b)); assert_eq!(blob_get(&m, "plugin/x").get("k").and_then(|v| v.as_str()), Some("v"));
        assert!(blob_put(&m, "plugin/x", &Map::new())); assert!(m.get("plugin/x").is_none());
    }
    #[test] fn the_windows_file_name_is_hex_of_the_name() { #[cfg(target_os = "windows")] { assert!(WindowsVault::file("plugin/x").is_some()); } assert_eq!(format!("{}", "plugin/x".bytes().map(|b| format!("{b:02x}")).collect::<String>()), "706c7567696e2f78"); }
}
