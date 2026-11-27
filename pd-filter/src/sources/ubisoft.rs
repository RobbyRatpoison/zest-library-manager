//! Ubisoft Connect from the client's own cache (PlayDate's `plugins/ubisoft` local sync, `config_parser.py` and `watcher.py`): no account, no network. Ubisoft Connect keeps a
//! descriptor of every game on the signed-in account in one binary file (`AppData/Local/Ubisoft Game Launcher/cache/configuration/configurations`): a flat protobuf byte stream
//! of records (install id, launch id, YAML text). The "sign-in" is the Wine prefix the client is installed in (on Linux); Windows reads `%LOCALAPPDATA%`. Install state comes
//! from each game's registry install-dir key in the prefix's `.reg` files; launch, install and uninstall are `uplay://` links, which need the numeric launch id (a GUID is ignored).
//! Not built: the account API sync (its login is blocked by the vendor's bot detection) and with it name repair through the products API and richer metadata.
use super::rockstar::{parse_prefix_input, prefix_setting, wine_user_dir};
use super::{Action, AuthBegin, AuthStatus, Caps, GameRef, InstalledRow, Launch, LibrarySource, SourceCtx};
use crate::importer::GameRecord;
use crate::net::Cancel;
use regex::Regex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const THUMB_CDN: &str = "https://static3.cdn.ubi.com/orbit/uplay_launcher_3_0/assets/";
const CONFIG_SUBPATH: &str = "AppData/Local/Ubisoft Game Launcher/cache/configuration/configurations";
/// Every game Ubisoft Connect installs lives under its own `games` folder: a process running from there is a game, not the launcher.
const GAME_PATH_MARKER: &str = "/ubisoft game launcher/games/";

pub struct UbisoftSource { config_dir: PathBuf, db: PathBuf }

/// One game's descriptor from the cache.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Entry { pub install_id: String, pub launch_id: String, pub space_id: String, pub name: String, pub thumb_image: String,
    pub exe_relative: Option<String>, pub registry_key: Option<String>, pub third_party_platform: Option<String> }

// ---------------------------------------------------------------- names

/// `is_placeholder_name`: an untranslated token (`l1`, `l22`) or an upper-case echo of a field name (`GAMENAME`, `THUMBIMAGE`) rather than a real name.
pub fn is_placeholder_name(v: &str) -> bool {
    let v = v.trim();
    if v.is_empty() || ["ubisoft game", "name"].contains(&v.to_lowercase().as_str()) { return true; }
    let b = v.as_bytes();
    if b.len() > 1 && (b[0] == b'l' || b[0] == b'L') && b[1..].iter().all(|c| c.is_ascii_digit()) { return true; }
    v.chars().any(char::is_uppercase) && !v.chars().any(char::is_lowercase) && v.chars().all(char::is_alphabetic) && v.chars().count() > 3
}

/// `slugify`: the store-page slug for a name.
pub fn slugify(name: &str) -> String {
    let s: String = name.replace('\u{2019}', "").replace('\'', "").chars().filter(|c| c.is_alphanumeric() || *c == '_' || c.is_whitespace() || *c == '-').collect();
    let mut out = String::new();
    let mut gap = false;
    for c in s.trim().chars() {
        if c.is_whitespace() || c == '_' || c == '-' { gap = true; continue; }
        if gap && !out.is_empty() { out.push('-'); }
        gap = false;
        out.push(c);
    }
    out.to_lowercase()
}

// ---------------------------------------------------------------- the binary cache

fn varint(d: &[u8], mut pos: usize) -> Option<(u64, usize)> {
    let (mut r, mut shift) = (0u64, 0u32);
    loop {
        let b = *d.get(pos)?; pos += 1;
        if shift < 64 { r |= ((b & 0x7F) as u64) << shift; }
        if b & 0x80 == 0 { return Some((r, pos)); }
        shift += 7;
    }
}

/// `_iter_raw_entries`: (install id, launch id, YAML bytes) of each top-level record. A malformed byte resyncs one byte on: the file has no index.
pub fn raw_entries(d: &[u8]) -> Vec<(u64, u64, Vec<u8>)> {
    let (mut out, mut pos, n) = (vec![], 0usize, d.len());
    while pos < n {
        let start = pos;
        let attempt = (|| -> Option<(usize, Option<(u64, u64, Vec<u8>)>)> {
            let (tag, p) = varint(d, pos)?;
            if tag >> 3 != 1 || tag & 7 != 2 { return None; }
            let (len, p) = varint(d, p)?;
            let end = p.checked_add(len as usize)?;
            if len == 0 || end > n { return None; }
            let (mut inst, mut launch, mut yaml) = (0u64, 0u64, vec![]);
            let (mut ip, mut valid) = (p, true);
            while ip < end {
                let (t, q) = varint(d, ip)?; ip = q;
                match t & 7 {
                    0 => { let (v, q) = varint(d, ip)?; ip = q; if t >> 3 == 1 { inst = v } else if t >> 3 == 2 { launch = v } }
                    2 => { let (l, q) = varint(d, ip)?; let from = q.min(n); let to = q.saturating_add(l as usize).min(n); if t >> 3 == 3 { yaml = d[from..to].to_vec(); } ip = q.saturating_add(l as usize); }
                    1 => ip += 8,
                    5 => ip += 4,
                    _ => { valid = false; break; }
                }
            }
            Some((end, (valid && !yaml.is_empty()).then(|| (inst, if launch != 0 { launch } else { inst }, yaml))))
        })();
        match attempt { Some((end, rec)) => { out.extend(rec); pos = end; } None => pos = start + 1 }
    }
    out
}

fn yaml_str(v: &serde_yaml::Value) -> String { match v { serde_yaml::Value::String(s) => s.clone(), serde_yaml::Value::Number(n) => n.to_string(), serde_yaml::Value::Bool(b) => b.to_string(), _ => String::new() } }
fn field(root: &serde_yaml::Value, name: &str) -> String {
    let v = root.get(name).map(|x| match x { serde_yaml::Value::String(s) => s.trim().to_string(), _ => String::new() }).unwrap_or_default();
    if !is_placeholder_name(&v) { return v; }
    if name == "name" {
        let fb = root.get("installer").and_then(|i| i.get("game_identifier")).and_then(|g| g.as_str()).unwrap_or("").trim().to_string();
        if !fb.is_empty() && !is_placeholder_name(&fb) { return fb; }
    }
    v
}

/// `_parse_entry`: a record's YAML as an `Entry`; None for a record that is not a game (no `start_game`, unreadable YAML).
pub fn parse_entry(install: u64, launch: u64, yaml: &[u8]) -> Option<Entry> {
    static GUID: OnceLock<Regex> = OnceLock::new();
    let text = String::from_utf8_lossy(yaml).replace('\u{FFFD}', "");
    if !text.contains("start_game") { return None; }
    let doc: serde_yaml::Value = serde_yaml::from_str(&text.replace('\t', " ")).ok()?;
    let root = doc.get("root").filter(|r| r.is_mapping())?;
    let exe = root.get("start_game").and_then(|s| s.get("online")).and_then(|o| o.get("executables")).and_then(|e| e.get(0));
    let rel = exe.and_then(|e| e.get("path")).and_then(|p| p.get("relative")).map(yaml_str).filter(|s| !s.is_empty());
    let reg = exe.and_then(|e| e.get("working_directory")).and_then(|w| w.get("register")).map(yaml_str).filter(|s| !s.is_empty());
    // all or nothing, as the Python's single try block: a record missing either part has neither
    let (rel, reg) = if rel.is_some() && reg.is_some() { (rel, reg) } else { (None, None) };
    let sid_raw = root.get("space_id").map(yaml_str).unwrap_or_default();
    let space_id = GUID.get_or_init(|| Regex::new(r"(?i)\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b").unwrap()).find(&sid_raw).map(|m| m.as_str().to_string()).unwrap_or(sid_raw);
    let tpp = root.get("third_party_platform").and_then(|t| t.get("name")).map(yaml_str).filter(|s| !s.is_empty());
    Some(Entry { install_id: install.to_string(), launch_id: launch.to_string(), space_id, name: field(root, "name"), thumb_image: field(root, "thumb_image"), exe_relative: rel, registry_key: reg, third_party_platform: tpp })
}

pub fn parse_configurations(data: &[u8]) -> Vec<Entry> { raw_entries(data).into_iter().filter_map(|(i, l, y)| parse_entry(i, l, &y)).collect() }

// ---------------------------------------------------------------- the prefix registry

fn unescape_reg(s: &str) -> String {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\\x([0-9a-fA-F]{4})").unwrap()).replace_all(s, |c: &regex::Captures| char::from_u32(u32::from_str_radix(&c[1], 16).unwrap_or(0xFFFD)).unwrap_or('\u{FFFD}').to_string()).replace("\\\\", "\\")
}

/// `{lower-cased "section\value name": raw value}` from a Wine `.reg` text file, added to `out`.
fn read_reg_file(path: &Path, out: &mut HashMap<String, String>) {
    let Ok(bytes) = std::fs::read(path) else { return };
    let text = String::from_utf8_lossy(&bytes);
    let mut section = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') { continue; }
        if line.starts_with('[') {
            if let Some(end) = line.rfind(']') {
                let tail = line[end + 1..].trim_start();
                if end > 1 && !line[end + 1..].is_empty() && line[end + 1..].starts_with(char::is_whitespace) && !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) { section = unescape_reg(&line[1..end]); continue; }
            }
        }
        if let Some(rest) = line.strip_prefix('"') {
            if let Some(q) = rest.find("\"=") {
                if q > 0 && !section.is_empty() {
                    let key = unescape_reg(&rest[..q]);
                    let mut raw = rest[q + 2..].trim().to_string();
                    if raw.len() >= 2 && raw.starts_with('"') && raw.ends_with('"') { raw = unescape_reg(&raw[1..raw.len() - 1]); }
                    out.insert(format!("{section}\\{key}").to_lowercase(), raw);
                }
            }
        }
    }
}

pub fn load_registry(prefix: &Path) -> HashMap<String, String> {
    let mut reg = HashMap::new();
    read_reg_file(&prefix.join("system.reg"), &mut reg);
    read_reg_file(&prefix.join("user.reg"), &mut reg);
    reg
}

/// `read_registry_value`: `Sub\Key\Path\ValueName` (a hive prefix is optional); the 32-bit view (`Wow6432Node`) is tried when the plain key is missing.
pub fn registry_value(reg: &HashMap<String, String>, key: &str) -> Option<String> {
    if key.is_empty() { return None; }
    let mut k = key.to_lowercase();
    for hive in ["hkey_local_machine\\", "hkey_current_user\\"] { if let Some(r) = k.strip_prefix(hive) { k = r.to_string(); break; } }
    reg.get(&k).cloned().or_else(|| reg.get(&k.replacen("software\\", "software\\wow6432node\\", 1)).cloned())
}

fn resolve_drive_path(prefix: &Path, win: &str) -> PathBuf {
    static R: OnceLock<Regex> = OnceLock::new();
    let Some(c) = R.get_or_init(|| Regex::new(r"^([A-Za-z]):[\\/](.*)$").unwrap()).captures(win) else { return prefix.join("drive_c").join(win.replace('\\', "/")) };
    let (drive, rel) = (c[1].to_lowercase(), c[2].replace('\\', "/"));
    let link = prefix.join("dosdevices").join(format!("{drive}:"));
    let root = std::fs::canonicalize(&link).ok().filter(|r| r.is_dir()).unwrap_or_else(|| if drive == "c" { prefix.join("drive_c") } else { PathBuf::from("/") });
    root.join(rel)
}

/// `resolve_install_dir`: the real folder a game's registry key points at, None when the key is unset (not installed) or the folder is gone.
pub fn resolve_install_dir(prefix: &Path, reg: &HashMap<String, String>, key: &str) -> Option<PathBuf> {
    let v = registry_value(reg, key).filter(|v| !v.is_empty())?;
    Some(resolve_drive_path(prefix, &v)).filter(|p| p.is_dir())
}

// ---------------------------------------------------------------- the source

/// A protocol-buffers field value (only what the envelope uses).
enum Pb<'a> { Num(u64), Bytes(&'a [u8]) }
fn pb_fields(mut b: &[u8]) -> Option<Vec<(u64, Pb<'_>)>> {
    fn varint(b: &mut &[u8]) -> Option<u64> { let mut v = 0u64; for i in 0..10 { let c = *b.first()?; *b = &b[1..]; v |= u64::from(c & 0x7f) << (7 * i); if c & 0x80 == 0 { return Some(v); } } None }
    let mut out = vec![];
    while !b.is_empty() {
        let key = varint(&mut b)?;
        match key & 7 {
            0 => out.push((key >> 3, Pb::Num(varint(&mut b)?))),
            2 => { let n = usize::try_from(varint(&mut b)?).ok()?; if n > b.len() { return None; } out.push((key >> 3, Pb::Bytes(&b[..n]))); b = &b[n..]; }
            _ => return None,
        }
    }
    Some(out)
}
fn pb_bytes<'a>(f: &[(u64, Pb<'a>)], n: u64) -> Option<&'a [u8]> { f.iter().find_map(|(k, v)| match v { Pb::Bytes(b) if *k == n => Some(*b), _ => None }) }

/// What a current client wraps its game list in (`configurations.vdat`): which key closes it (its id names a file in the client's `store` folder), and how. One field holding the envelope: a header
/// (whose field 2 is the encryption: algorithm 1 = AES-256-GCM, the key's id, a 12-byte nonce, a 16-byte tag) and the sealed data.
pub struct Envelope<'a> { pub key_id: String, nonce: [u8; 12], tag: &'a [u8], sealed: &'a [u8] }
pub fn read_envelope(bytes: &[u8]) -> Option<Envelope<'_>> {
    let outer = pb_fields(bytes)?;
    let env = pb_fields(pb_bytes(&outer, 1)?)?;
    let header = pb_fields(pb_bytes(&env, 1)?)?;
    let sealed = pb_bytes(&env, 2)?;
    let enc = pb_fields(pb_bytes(&header, 2)?)?;
    if !enc.iter().any(|(k, v)| *k == 1 && matches!(v, Pb::Num(1))) { return None; }
    let key_id = std::str::from_utf8(pb_bytes(&enc, 2)?).ok()?.to_string();
    if key_id.len() != 32 || !key_id.bytes().all(|b| b.is_ascii_alphanumeric()) { return None; }
    let nonce: [u8; 12] = pb_bytes(&enc, 3)?.try_into().ok()?;
    let tag = pb_bytes(&enc, 4)?;
    (tag.len() == 16).then_some(Envelope { key_id, nonce, tag, sealed })
}
/// The envelope's contents, when `key` (32 bytes) is the one that closed it.
pub fn open_envelope(env: &Envelope<'_>, key: &[u8]) -> Option<Vec<u8>> {
    use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
    let k = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, key).ok()?);
    let mut data = env.sealed.to_vec(); data.extend_from_slice(env.tag);
    let plain = k.open_in_place(Nonce::assume_unique_for_key(env.nonce), Aad::empty(), &mut data).ok()?;
    Some(plain.to_vec())
}

impl UbisoftSource {
    pub fn new(config_dir: PathBuf, db: PathBuf) -> Self { UbisoftSource { config_dir, db } }
    fn prefix(&self) -> Option<PathBuf> { prefix_setting(&self.config_dir, "ubisoft") }
    /// The cache file: `%LOCALAPPDATA%` on Windows, else inside the prefix's user folder.
    fn config_path(&self) -> Option<PathBuf> {
        if cfg!(windows) { return std::env::var_os("LOCALAPPDATA").map(|l| PathBuf::from(l).join("Ubisoft Game Launcher/cache/configuration/configurations")); }
        Some(wine_user_dir(&self.prefix()?, &std::env::var("USER").unwrap_or_default())?.join(CONFIG_SUBPATH))
    }
    /// The game list as bytes: the plain `configurations` file, or what a current client's encrypted `configurations.vdat` holds (its key is a protected file next to it: opened for the Wine
    /// user under Wine, with Windows' own DPAPI on Windows).
    pub fn cache_bytes(&self) -> Option<Vec<u8>> {
        let plain = self.config_path()?;
        if let Ok(d) = std::fs::read(&plain) { return Some(d); }
        let vdat = std::fs::read(plain.with_extension("vdat")).ok()?;
        if cfg!(windows) {
            // real Windows: the key file next to the cache is opened with Windows' own DPAPI (as the user who signed in to Ubisoft Connect); the client writes it without extra entropy
            let env = read_envelope(&vdat)?;
            let store = plain.parent()?.parent()?.parent()?.join("store");   // ...\Ubisoft Game Launcher\cache\configuration\configurations -> ...\Ubisoft Game Launcher\store
            let blob = std::fs::read(store.join(format!("{}.bin", env.key_id))).ok().filter(|b| b.len() <= 16 * 1024)?;
            let key = crate::windpapi::unprotect(&blob, None).filter(|k| k.len() == 32).or_else(|| { crate::net::log("Ubisoft: the cache key could not be opened with Windows' DPAPI"); None })?;
            return open_envelope(&env, &key);
        }
        let env = read_envelope(&vdat)?;
        let user_dir = wine_user_dir(&self.prefix()?, &std::env::var("USER").unwrap_or_default())?;
        let store = user_dir.join("AppData/Local/Ubisoft Game Launcher/store");
        let blob = std::fs::read(store.join(format!("{}.bin", env.key_id))).ok().filter(|b| b.len() <= 16 * 1024)?;
        // Wine's DPAPI names the Wine user the program ran as: the folder's name (Proton prefixes use steamuser), else the login
        let mut names: Vec<String> = vec![user_dir.file_name()?.to_string_lossy().to_string(), "steamuser".into(), std::env::var("USER").unwrap_or_default()];
        names.retain(|n| !n.is_empty()); names.dedup();
        let key = names.iter().find_map(|n| crate::wine_dpapi::unprotect(&blob, n).filter(|k| k.len() == 32))?;
        open_envelope(&env, &key)
    }
    fn has_cache(&self) -> bool { self.config_path().map_or(false, |p| p.is_file() || p.with_extension("vdat").is_file()) }
    fn entries(&self) -> Vec<Entry> { self.cache_bytes().map(|d| parse_configurations(&d)).unwrap_or_default() }
    fn key(e: &Entry) -> &str { if e.space_id.is_empty() { &e.launch_id } else { &e.space_id } }
    /// The numeric id the `uplay://` handler wants for a game stored under its space id (a game with no space id is stored under it already).
    fn launch_id(&self, platform_id: &str) -> String { self.entries().into_iter().find(|e| e.space_id == platform_id).map(|e| e.launch_id).filter(|l| !l.is_empty() && l != "0").unwrap_or_else(|| platform_id.to_string()) }
    fn send(&self, url: String) -> Result<Action, String> {
        if !cfg!(unix) || cfg!(target_os = "macos") { return Ok(Action::OpenUrl(url)); }
        let prefix = self.prefix().ok_or("Ubisoft Connect not set up. Give its Wine prefix in the Plugins dialog.")?;
        // Ubisoft Connect's window will not take a link while it runs under Wine: end the session and start fresh (it re-forwards the link on startup), unless a game is running
        crate::wine::protocol_action(&url, &prefix, true, &[], &|procs| procs.iter().any(|(_, a)| a.replace('\\', "/").to_lowercase().contains(GAME_PATH_MARKER))
            .then(|| "A Ubisoft game is currently running. Close it before launching, installing or uninstalling a different game.".to_string()))
    }
}

impl LibrarySource for UbisoftSource {
    fn id(&self) -> &str { "ubisoft" }
    fn label(&self) -> &str { "Ubisoft Connect" }
    fn caps(&self) -> Caps { Caps { auth: true, sync: true, launch: true, uninstall: true, installed_scan: true, art: true, ..Default::default() } }

    fn auth_status(&self) -> Result<AuthStatus, String> {
        // signed in = Ubisoft Connect has saved a game list (it does so only once an account is signed in there)
        let ok = self.has_cache();
        Ok(AuthStatus { connected: ok, username: None })
    }
    fn sign_in_hint(&self) -> Option<&'static str> { Some("Ubisoft's website login cannot be used from an app: sign in inside Ubisoft Connect, then SYNC LIBRARY will find your games.") }
    fn auth_begin(&self) -> Result<AuthBegin, String> {
        Ok(AuthBegin { url: String::new(), placeholder: "Full path of the Wine prefix Ubisoft Connect is installed in".into(), instructions: vec![
            "Zest reads Ubisoft Connect's own game list from its cache: it needs the folder (the Wine prefix) the client is installed in.".into(),
            "The folder contains drive_c. Typical place: ~/.local/share/Zest/ubisoft (or ~/.local/share/PlayDate/ubisoft from PlayDate).".into(),
            "Sign in once in Ubisoft Connect so it has saved your games, then paste the full path below. (Ubisoft's website login cannot be used from an app.)".into()] })
    }
    fn auth_complete(&self, value: &str) -> Result<AuthStatus, String> {
        let path = parse_prefix_input(value)?;
        let has = wine_user_dir(&path, &std::env::var("USER").unwrap_or_default()).map_or(false, |u| u.join(CONFIG_SUBPATH).is_file() || u.join(format!("{CONFIG_SUBPATH}.vdat")).is_file());
        if !has { return Err("No Ubisoft Connect game list was found in that prefix. Install Ubisoft Connect there and sign in once.".into()); }
        crate::account::plugin_secret_set(&self.config_dir, "ubisoft", "prefix", Some(&path.to_string_lossy())).map_err(|_| "Could not save the folder".to_string())?;
        self.auth_status()
    }
    fn auth_disconnect(&self) -> Result<(), String> {
        crate::account::plugin_secret_set(&self.config_dir, "ubisoft", "prefix", None).map_err(|_| "Could not save".to_string())?;
        crate::launcher_installer::forget_prefix(&self.config_dir, "ubisoft");   // the older saved place too, or the folder would still be found
        Ok(())
    }
    fn launcher_folder(&self) -> Option<PathBuf> { self.prefix() }

    fn sync(&self, _cx: &SourceCtx, sink: &mut dyn FnMut(Vec<GameRecord>), cancel: &Cancel) -> Result<usize, String> {
        if cancel.is_set() { return Ok(0); }
        if !self.has_cache() { return Err("Ubisoft Connect launcher not installed, or never signed in".into()); }
        let all = parse_configurations(&self.cache_bytes().ok_or("Could not read Ubisoft Connect's game list (its newer encrypted cache could not be opened)")?);
        // a game cross-linked from another store (Steam) cannot be installed or launched by Ubisoft Connect and duplicates a game owned there
        let recs: Vec<GameRecord> = all.iter().filter(|e| !e.name.is_empty() && !(e.space_id.is_empty() && e.launch_id.is_empty()) && e.third_party_platform.is_none()).map(|e| GameRecord {
            platform_id: Self::key(e).to_string(), name: e.name.clone(), slug: Some(slugify(&e.name)).filter(|s| !s.is_empty()), ..Default::default() }).collect();
        let n = recs.len();
        crate::net::log(&format!("Ubisoft sync: {n} games from the client's cache"));
        sink(recs);
        Ok(n)
    }

    fn installed_scan(&self) -> Option<Vec<InstalledRow>> {
        let c = rusqlite::Connection::open_with_flags(&self.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
        let mut st = c.prepare("SELECT platform_id FROM games WHERE platform = 'ubisoft' AND platform_id IS NOT NULL").ok()?;
        let ids: Vec<String> = st.query_map([], |r| r.get::<_, String>(0)).ok()?.flatten().collect();
        // no prefix or no cache: nothing can be installed, so stale flags are cleared (as PlayDate)
        let mut dirs: HashMap<String, PathBuf> = HashMap::new();
        if let (Some(prefix), true) = (self.prefix(), self.has_cache()) {
            let reg = load_registry(&prefix);
            for e in self.entries() { if let Some(d) = e.registry_key.as_deref().and_then(|k| resolve_install_dir(&prefix, &reg, k)) { dirs.insert(Self::key(&e).to_string(), d); } }
        }
        Some(ids.into_iter().map(|id| match dirs.get(&id) { Some(d) => InstalledRow { installed: true, install_path: Some(d.to_string_lossy().to_string()), platform_id: id }, None => InstalledRow { installed: false, install_path: Some(String::new()), platform_id: id } }).collect())
    }

    fn art_urls(&self, g: &GameRef) -> Vec<(String, String)> {
        self.entries().into_iter().find(|e| Self::key(e) == g.platform_id && !e.thumb_image.is_empty()).map(|e| vec![("vertical".to_string(), format!("{THUMB_CDN}{}", e.thumb_image))]).unwrap_or_default()
    }
    fn store_url(&self, g: &GameRef) -> Option<String> { (!g.slug.is_empty()).then(|| format!("https://store.ubisoft.com/us/{}", g.slug)) }

    fn launch(&self, g: &GameRef) -> Result<Launch, String> {
        let pid = g.platform_id.trim();
        if pid.is_empty() { return Err("Game has no space ID: try syncing again".into()); }
        let id = self.launch_id(pid);
        let url = if g.installed { format!("uplay://launch/{id}/0") } else { format!("uplay://install/{id}") };
        Ok(Launch { action: self.send(url)?, record: g.installed })
    }
    fn uninstall(&self, g: &GameRef) -> Result<Action, String> {
        let pid = g.platform_id.trim();
        if pid.is_empty() { return Err("Game has no space ID: try syncing again".into()); }
        self.send(format!("uplay://uninstall/{}", self.launch_id(pid)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    fn here() -> PathBuf { Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests")).to_path_buf() }
    fn golden() -> Value { serde_json::from_str(&std::fs::read_to_string(here().join("ubisoft_golden.json")).unwrap()).unwrap() }
    fn tmpdir(tag: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("zest_ubi_{tag}_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); std::fs::write(d.join("config.json"), "{\"other\": 1}").unwrap(); d }
    fn cx(d: &PathBuf) -> SourceCtx { let h: std::sync::Arc<dyn crate::net::Http> = std::sync::Arc::new(crate::net::UreqHttp::new()); SourceCtx { http: h.clone(), steam: std::sync::Arc::new(crate::steamclient::Steam::new(h, None, 10.0)), root: d.clone(), db: d.join("games.db"), today: "2026-11-04".into(), appinfo: std::sync::Arc::new(|_| Default::default()), account_id: None } }
    fn opt(v: &Value) -> Option<String> { v.as_str().map(String::from) }

    /// Reads the real cache in the PlayDate prefix on this PC (read-only; prints a count and a few titles): `cargo test -p pd-filter real_ubisoft -- --ignored --nocapture`.
    #[test] #[ignore] fn real_ubisoft_cache_opens() {
        let d = tmpdir("real"); let pfx = crate::installdir::home().join(".local/share/PlayDate/ubisoft");
        std::fs::write(d.join("config.json"), serde_json::json!({"launchers": {"ubisoft": {"prefix": pfx.to_string_lossy()}}}).to_string()).unwrap();
        let s = UbisoftSource::new(d.clone(), d.join("games_1.db"));
        let e = s.entries();
        let named: Vec<&Entry> = e.iter().filter(|x| !x.name.is_empty() && x.third_party_platform.is_none()).collect();
        println!("entries {} named-and-not-cross-linked {}; first: {:?}", e.len(), named.len(), named.iter().take(6).map(|x| x.name.as_str()).collect::<Vec<_>>());
        assert!(!e.is_empty());
    }
    // the fixture cache is encrypted with the Wine prefix's key; on Windows the real DPAPI key of the user is used (windpapi)
    #[cfg(not(windows))]
    #[test] fn a_current_clients_encrypted_cache_gives_the_same_games_as_the_plain_one() {
        use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
        fn varint(mut v: u64) -> Vec<u8> { let mut o = vec![]; loop { let b = (v & 0x7f) as u8; v >>= 7; if v == 0 { o.push(b); return o; } o.push(b | 0x80); } }
        fn field(n: u64, data: &[u8]) -> Vec<u8> { let mut o = varint(n << 3 | 2); o.extend(varint(data.len() as u64)); o.extend_from_slice(data); o }
        let plain = std::fs::read(here().join("data/ubisoft_configurations")).unwrap();
        let key: Vec<u8> = (1u8..=32).collect(); let nonce = [7u8; 12]; let key_id = "51bd3a919cf546c1b3c40b2013a4c83f";
        let mut sealed = plain.clone();
        let tag = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, &key).unwrap()).seal_in_place_separate_tag(Nonce::assume_unique_for_key(nonce), Aad::empty(), &mut sealed).unwrap();
        let mut enc = varint(1 << 3); enc.extend(varint(1)); enc.extend(field(2, key_id.as_bytes())); enc.extend(field(3, &nonce)); enc.extend(field(4, tag.as_ref()));
        let mut header = field(2, &enc); header.splice(0..0, field(1, &[]));
        let mut envelope = field(1, &header); envelope.extend(field(2, &sealed));
        let vdat = field(1, &envelope);
        let d = tmpdir("vdat"); let pfx = d.join("pfx");
        let user = pfx.join("drive_c/users/steamuser/AppData/Local/Ubisoft Game Launcher");
        std::fs::create_dir_all(user.join("cache/configuration")).unwrap(); std::fs::create_dir_all(user.join("store")).unwrap();
        std::fs::write(user.join("cache/configuration/configurations.vdat"), &vdat).unwrap();
        std::fs::write(user.join(format!("store/{key_id}.bin")), crate::wine_dpapi::tests::hex(crate::wine_dpapi::tests::BLOB)).unwrap();
        std::fs::write(d.join("config.json"), serde_json::json!({"launchers": {"ubisoft": {"prefix": pfx.to_string_lossy()}}}).to_string()).unwrap();
        let s = UbisoftSource::new(d.clone(), d.join("games_1.db"));
        assert!(s.has_cache() && s.auth_status().unwrap().connected, "an encrypted cache counts as signed in");
        let got = s.entries(); let want = parse_configurations(&plain);
        assert!(!want.is_empty() && got.len() == want.len() && got.iter().zip(&want).all(|(a, b)| a.name == b.name && a.space_id == b.space_id));
        std::fs::write(user.join(format!("store/{key_id}.bin")), b"not the key").unwrap();
        assert!(s.entries().is_empty(), "without the key nothing is read");
    }
    #[test] fn the_real_cache_parses_to_exactly_what_the_python_parser_found() {
        let want = golden()["entries"].as_array().unwrap().clone();
        let got = parse_configurations(&std::fs::read(here().join("data/ubisoft_configurations")).unwrap());
        assert_eq!(got.len(), want.len());
        for (g, w) in got.iter().zip(&want) {
            assert_eq!(g, &Entry { install_id: w["install_id"].as_str().unwrap().into(), launch_id: w["launch_id"].as_str().unwrap().into(), space_id: w["space_id"].as_str().unwrap().into(), name: w["name"].as_str().unwrap().into(),
                thumb_image: w["thumb_image"].as_str().unwrap().into(), exe_relative: opt(&w["exe_relative"]), registry_key: opt(&w["registry_key"]), third_party_platform: opt(&w["third_party_platform"]) }, "{}", w["name"]);
        }
    }
    // the registry here is the fixture prefix's user.reg; on Windows it is the real registry
    #[cfg(not(windows))]
    #[test] fn slugs_placeholders_and_the_registry_agree_with_the_python() {
        let g = golden();
        for (n, s) in g["slugs"].as_object().unwrap() { assert_eq!(slugify(n), s.as_str().unwrap(), "{n:?}"); }
        for (p, b) in g["placeholders"].as_object().unwrap() { assert_eq!(is_placeholder_name(p), b.as_bool().unwrap(), "{p:?}"); }
        let root = here().join("data/ubisoft_prefix"); let reg = load_registry(&root);
        for (k, w) in g["registry"].as_object().unwrap() {
            assert_eq!(registry_value(&reg, k), opt(&w["value"]), "{k:?}");
            let dir = resolve_install_dir(&root, &reg, k).map(|d| d.to_string_lossy().replace(&*root.to_string_lossy(), "<ROOT>")).unwrap_or_default();
            assert_eq!(dir, w["dir"].as_str().unwrap(), "{k:?}");
        }
    }
    #[test] fn a_broken_record_is_skipped_and_the_scan_resyncs() {
        let good = raw_entries(&std::fs::read(here().join("data/ubisoft_configurations")).unwrap());
        let mut data = vec![0xFFu8, 0xFF, 0x08, 0x01];   // junk before the first record
        for (i, l, y) in good.iter().take(2) { let mut inner = vec![0x08]; let mut v = *i; while v > 0x7F { inner.push((v & 0x7F) as u8 | 0x80); v >>= 7; } inner.push(v as u8); inner.push(0x10); let mut v = *l; while v > 0x7F { inner.push((v & 0x7F) as u8 | 0x80); v >>= 7; } inner.push(v as u8);
            inner.push(0x1A); let mut v = y.len() as u64; while v > 0x7F { inner.push((v & 0x7F) as u8 | 0x80); v >>= 7; } inner.push(v as u8); inner.extend(y);
            data.push(0x0A); let mut v = inner.len() as u64; while v > 0x7F { data.push((v & 0x7F) as u8 | 0x80); v >>= 7; } data.push(v as u8); data.extend(inner); }
        assert_eq!(raw_entries(&data).len(), 2);
        assert!(raw_entries(&[]).is_empty() && raw_entries(&[0x0A, 0xFF]).is_empty());
    }
    // Linux only: on a Mac a game's link goes straight to Ubisoft Connect (no prefix to start it in)
    #[cfg(target_os = "linux")]
    #[test] fn the_prefix_is_kept_and_sync_scan_art_and_links_work_on_it() {
        let d = tmpdir("flow"); let pfx = d.join("pfx");
        let cache = pfx.join("drive_c/users/steamuser").join(CONFIG_SUBPATH); std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
        std::fs::copy(here().join("data/ubisoft_configurations"), &cache).unwrap();
        let want = golden()["entries"].as_array().unwrap().clone();
        let db = d.join("games_1.db");
        let s = UbisoftSource::new(d.clone(), db.clone());
        assert!(!s.auth_status().unwrap().connected && s.sync(&cx(&d), &mut |_| {}, &Cancel::new()).unwrap_err().contains("never signed in"));
        std::fs::create_dir_all(d.join("empty/drive_c")).unwrap();
        assert!(s.auth_complete(&d.join("empty").to_string_lossy()).unwrap_err().contains("No Ubisoft Connect") && s.auth_complete("x").is_err());
        assert!(s.auth_complete(&pfx.to_string_lossy()).unwrap().connected);
        let mut got = vec![]; let n = s.sync(&cx(&d), &mut |b| got.extend(b), &Cancel::new()).unwrap();
        let expect = want.iter().filter(|e| !e["name"].as_str().unwrap().is_empty() && e["third_party_platform"].is_null()).count();
        assert_eq!((n, got.len()), (expect, expect));
        let first = want.iter().find(|e| e["third_party_platform"].is_null()).unwrap();
        assert_eq!((got[0].platform_id.as_str(), got[0].name.as_str()), (first["space_id"].as_str().unwrap(), first["name"].as_str().unwrap()));
        // the scan: only a game whose registry key resolves to a real folder is installed
        let conv = want.iter().find(|e| e["registry_key"].is_string()).unwrap();
        let (cid, key) = (conv["space_id"].as_str().unwrap().to_string(), conv["registry_key"].as_str().unwrap().to_string());
        let sect = key.rsplit_once('\\').unwrap().0.replace('\\', "\\\\"); let sect = sect.trim_start_matches("HKEY_LOCAL_MACHINE\\\\");
        std::fs::write(pfx.join("system.reg"), format!("WINE REGISTRY Version 2\n\n[{sect}] 1700000000\n\"InstallDir\"=\"C:\\\\Games\\\\Conv\"\n")).unwrap();
        std::fs::create_dir_all(pfx.join("drive_c/Games/Conv")).unwrap();
        rusqlite::Connection::open(&db).unwrap().execute_batch(&format!("CREATE TABLE games (platform TEXT, platform_id TEXT); INSERT INTO games VALUES ('ubisoft','{cid}'),('ubisoft','nope');")).unwrap();
        let rows = s.installed_scan().unwrap();
        assert_eq!(rows.iter().map(|r| (r.platform_id.as_str(), r.installed)).collect::<Vec<_>>(), vec![(cid.as_str(), true), ("nope", false)]);
        assert_eq!(rows[0].install_path.as_deref(), Some(&*pfx.join("drive_c/Games/Conv").to_string_lossy()));
        // art, store link, launch id lookup and the links
        let g = GameRef { platform_id: cid.clone(), slug: "tom-clancys-splinter-cell-conviction".into(), name: "x".into(), ..Default::default() };
        assert_eq!(s.art_urls(&g)[0].1, format!("{THUMB_CDN}{}", conv["thumb_image"].as_str().unwrap()));
        assert_eq!(s.store_url(&g).as_deref(), Some("https://store.ubisoft.com/us/tom-clancys-splinter-cell-conviction"));
        assert_eq!(s.launch_id(&cid), conv["launch_id"].as_str().unwrap());
        assert_eq!(s.launch_id("9999"), "9999", "a game with no cache entry keeps its stored id");
        crate::wine::TEST_RUNTIME.with(|t| *t.borrow_mut() = Some(Ok(crate::wine::Runtime::Wine { wine: "/usr/bin/wine".into() })));
        let l = s.launch(&GameRef { installed: true, ..g.clone() }).unwrap();
        assert!(l.record); match &l.action { Action::Spawn { args, .. } => assert_eq!(args, &vec!["start".to_string(), format!("uplay://launch/{}/0", conv["launch_id"].as_str().unwrap())]), a => panic!("{a:?}") }
        let l = s.launch(&g).unwrap(); assert!(!l.record); match &l.action { Action::Spawn { args, .. } => assert!(args[1].starts_with("uplay://install/")), a => panic!("{a:?}") }
        match s.uninstall(&g).unwrap() { Action::Spawn { args, .. } => assert!(args[1].starts_with("uplay://uninstall/")), a => panic!("{a:?}") }
        s.auth_disconnect().unwrap();
        assert!(s.installed_scan().unwrap().iter().all(|r| !r.installed && r.install_path.as_deref() == Some("")));
    }
}
