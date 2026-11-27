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
    /// Would a value put here be kept as the user wants? False only while the user has not said where sign-ins go without a keyring: then a sign-in still in `config.json` is
    /// left there rather than moved somewhere closing Zest would lose it.
    fn keeps(&self, _name: &str) -> bool { true }
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
    fn keeps(&self, name: &str) -> bool { self.inner.keeps(name) }
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

// ───────────────────────── no working keyring: a file, or memory only, as the user chose ─────────────────────────

/// Where sign-ins go when the keyring does not work (a Steam Deck in Game Mode, a desktop without one). Asked once, at the first sign-in (`choice_pending`), and changeable in
/// Settings > Account. Typed API keys (`account/<id>`, `sgdb`) are not sign-ins: without a keyring they stay in `config.json` as before, whatever the choice.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Fallback { Ask, File, Session }
impl Fallback {
    pub fn key(self) -> &'static str { match self { Fallback::Ask => "ask", Fallback::File => "file", Fallback::Session => "session" } }
    pub fn from_key(k: &str) -> Fallback { match k { "file" => Fallback::File, "session" => Fallback::Session, _ => Fallback::Ask } }
}

/// Is this vault entry a sign-in (a store's tokens and cookies, the Steam sign-in) rather than a key the user typed?
pub fn is_sign_in(name: &str) -> bool { name.starts_with("plugin/") || name.starts_with("steam-refresh/") }

/// The sign-ins kept in a plain file in the data folder, readable by this user only (`zest_secrets.json`): one JSON object of name -> value.
pub struct FileVault { path: std::path::PathBuf, lock: Mutex<()> }
impl FileVault {
    pub fn new(path: std::path::PathBuf) -> Self { FileVault { path, lock: Mutex::new(()) } }
    fn load(&self) -> Map<String, Value> { std::fs::read_to_string(&self.path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v.as_object().cloned()).unwrap_or_default() }
    fn save(&self, m: &Map<String, Value>) -> bool {
        if m.is_empty() { let _ = std::fs::remove_file(&self.path); return true; }
        let tmp = self.path.with_extension("json.tmp");
        let ok = (|| -> std::io::Result<()> {
            use std::io::Write;
            let mut o = std::fs::OpenOptions::new(); o.write(true).create(true).truncate(true);
            #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; o.mode(0o600); }
            let mut f = o.open(&tmp)?; f.write_all(Value::Object(m.clone()).to_string().as_bytes())?; f.sync_all()?;
            std::fs::rename(&tmp, &self.path)
        })().is_ok();
        if !ok { let _ = std::fs::remove_file(&tmp); }
        ok
    }
    pub fn names(&self) -> Vec<String> { let _g = self.lock.lock(); self.load().keys().cloned().collect() }
}
impl Vault for FileVault {
    fn get(&self, name: &str) -> Option<String> { let _g = self.lock.lock(); self.load().get(name).and_then(|v| v.as_str()).map(String::from) }
    fn put(&self, name: &str, value: &str) -> bool { let _g = self.lock.lock(); let mut m = self.load(); m.insert(name.into(), Value::String(value.into())); self.save(&m) }
    fn forget(&self, name: &str) { let _g = self.lock.lock(); let mut m = self.load(); if m.remove(name).is_some() { self.save(&m); } }
}

/// Kept in memory only: gone when Zest closes.
#[derive(Default)] pub struct MemVault { map: Mutex<HashMap<String, String>> }
impl Vault for MemVault {
    fn get(&self, n: &str) -> Option<String> { self.map.lock().ok()?.get(n).cloned() }
    fn put(&self, n: &str, v: &str) -> bool { if let Ok(mut m) = self.map.lock() { m.insert(n.into(), v.into()); } true }
    fn forget(&self, n: &str) { if let Ok(mut m) = self.map.lock() { m.remove(n); } }
}

/// The vault Zest installs: the keyring when it works, else the file or memory for sign-ins as chosen. Typed keys without a keyring are refused (`put` false), so `account.rs`
/// keeps them in `config.json` as before.
pub struct WithFallback { keyring: Option<Arc<dyn Vault>>, keyring_works: bool, file: FileVault, mem: MemVault, mode: RwLock<Fallback> }
static PENDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static ACTIVE: RwLock<Option<Arc<WithFallback>>> = RwLock::new(None);
impl WithFallback {
    /// `keyring` is the platform's store (None = none here); whether it really works is tried once with a throwaway entry. Sign-ins left in the file from a run without a keyring
    /// move into the keyring when it works now (a Steam Deck switched to Desktop Mode).
    pub fn new(keyring: Option<Arc<dyn Vault>>, file: std::path::PathBuf, mode: Fallback) -> Self {
        let keyring_works = keyring.as_ref().map_or(false, |k| { let ok = k.put("zest-probe", "1"); k.forget("zest-probe"); ok });
        let me = WithFallback { keyring, keyring_works, file: FileVault::new(file), mem: MemVault::default(), mode: RwLock::new(mode) };
        if let (true, Some(k)) = (me.keyring_works, &me.keyring) {
            for n in me.file.names() { if let Some(v) = me.file.get(&n) { if k.put(&n, &v) { me.file.forget(&n); } } }
        }
        me
    }
    pub fn keyring_works(&self) -> bool { self.keyring_works }
    pub fn mode(&self) -> Fallback { self.mode.read().map(|m| *m).unwrap_or(Fallback::Ask) }
    /// The user's answer: sign-ins held in memory while asking move to the file when they chose it.
    pub fn set_mode(&self, m: Fallback) {
        if let Ok(mut g) = self.mode.write() { *g = m; }
        PENDING.store(false, std::sync::atomic::Ordering::SeqCst);
        match m {
            Fallback::File => { let held: Vec<(String, String)> = self.mem.map.lock().map(|x| x.iter().map(|(a, b)| (a.clone(), b.clone())).collect()).unwrap_or_default();
                for (n, v) in held { if self.file.put(&n, &v) { self.mem.forget(&n); } } }
            // chose not to keep them: what an earlier choice saved in the file is now kept in memory only
            Fallback::Session => { for n in self.file.names() { if let Some(v) = self.file.get(&n) { self.mem.put(&n, &v); } self.file.forget(&n); } }
            Fallback::Ask => {}
        }
    }
    fn holder(&self) -> &dyn Vault { if self.mode() == Fallback::File { &self.file } else { &self.mem } }
}
impl Vault for WithFallback {
    fn get(&self, name: &str) -> Option<String> {
        if self.keyring_works { if let Some(v) = self.keyring.as_ref().and_then(|k| k.get(name)) { return Some(v); } }
        self.mem.get(name).or_else(|| self.file.get(name))
    }
    fn put(&self, name: &str, value: &str) -> bool {
        if self.keyring_works { if let Some(k) = &self.keyring { if k.put(name, value) { self.file.forget(name); self.mem.forget(name); return true; } } }
        if !is_sign_in(name) { return false; }
        if self.mode() == Fallback::Ask { PENDING.store(true, std::sync::atomic::Ordering::SeqCst); }
        self.holder().put(name, value)
    }
    fn forget(&self, name: &str) { if let Some(k) = &self.keyring { if self.keyring_works { k.forget(name); } } self.file.forget(name); self.mem.forget(name); }
    /// While the user has not answered, a sign-in already in `config.json` stays there (it is not moved into memory, where closing Zest would lose it).
    /// Typed keys without a keyring are not offered at all (they stay in `config.json`, quietly).
    fn keeps(&self, name: &str) -> bool { self.keyring_works || (is_sign_in(name) && self.mode() != Fallback::Ask) }
}

/// Installs `v` as the vault and remembers it for `choice_pending` / `set_fallback`.
pub fn install_with_fallback(v: Arc<WithFallback>) { if let Ok(mut g) = ACTIVE.write() { *g = Some(v.clone()); } install(v); }
fn active() -> Option<Arc<WithFallback>> { ACTIVE.read().ok().and_then(|g| g.clone()) }
/// A sign-in has just been held in memory with no keyring and no answer yet: the app asks where to keep sign-ins.
pub fn choice_pending() -> bool { PENDING.load(std::sync::atomic::Ordering::SeqCst) && active().map_or(false, |a| !a.keyring_works() && a.mode() == Fallback::Ask) }
/// Is the keyring missing (so the choice matters here)?
pub fn no_keyring() -> bool { active().map_or(true, |a| !a.keyring_works()) }
/// Sign-ins already on disk (imported from PlayDate, or from a run before the question existed) with no keyring and no answer: ask about them too.
pub fn request_choice() { if active().map_or(false, |a| !a.keyring_works() && a.mode() == Fallback::Ask) { PENDING.store(true, std::sync::atomic::Ordering::SeqCst); } }
/// The question was closed without an answer: not asked again this run (the sign-in stays in memory meanwhile).
pub fn dismiss_pending() { PENDING.store(false, std::sync::atomic::Ordering::SeqCst); }
pub fn set_fallback(m: Fallback) { if let Some(a) = active() { a.set_mode(m); } }

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
    fn tmpfile(tag: &str) -> std::path::PathBuf { let d = std::env::temp_dir().join(format!("zest_secrets_{tag}_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d.join("zest_secrets.json") }
    #[test] fn without_a_keyring_sign_ins_wait_in_memory_until_the_user_chooses() {
        let f = tmpfile("ask"); let v = WithFallback::new(None, f.clone(), Fallback::Ask);
        assert!(!v.keeps("plugin/gog"), "a sign-in in config.json is left there while asking");
        assert!(!v.keeps("account/1"), "typed keys are not offered without a keyring: they stay in config.json");
        assert!(v.put("plugin/gog", "tok")); assert_eq!(v.get("plugin/gog").as_deref(), Some("tok")); assert!(!f.exists(), "nothing written to disk before the answer");
        assert!(!v.put("account/1", "{}"), "a typed key is refused, so it stays in config.json");
        v.set_mode(Fallback::File);
        assert!(f.exists()); assert_eq!(FileVault::new(f.clone()).get("plugin/gog").as_deref(), Some("tok"), "the held sign-in moved to the file");
        #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o600); }
        v.set_mode(Fallback::Session);
        assert!(!f.exists(), "choosing not to keep sign-ins empties the file"); assert_eq!(v.get("plugin/gog").as_deref(), Some("tok"), "but this run still has it");
    }
    #[test] fn a_working_keyring_wins_and_takes_over_what_the_file_held() {
        let f = tmpfile("kr"); FileVault::new(f.clone()).put("plugin/gog", "old");
        let k = Arc::new(Mem::default());
        let v = WithFallback::new(Some(k.clone()), f.clone(), Fallback::File);
        assert!(v.keyring_works()); assert_eq!(k.get("plugin/gog").as_deref(), Some("old"), "moved into the keyring"); assert!(!f.exists());
        assert!(v.put("account/1", "{}"), "typed keys go to a working keyring");
        let broken = Arc::new(Mem::default()); broken.broken.store(true, std::sync::atomic::Ordering::SeqCst);
        let v2 = WithFallback::new(Some(broken), tmpfile("broken"), Fallback::Ask); assert!(!v2.keyring_works(), "a keyring that does not take a value counts as none");
    }
    #[test] fn the_windows_file_name_is_hex_of_the_name() { #[cfg(target_os = "windows")] { assert!(WindowsVault::file("plugin/x").is_some()); } assert_eq!(format!("{}", "plugin/x".bytes().map(|b| format!("{b:02x}")).collect::<String>()), "706c7567696e2f78"); }
}
