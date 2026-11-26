//! The Steam account in PlayDate's `config.json` (`accounts[active_account]`: `steam_id`, `api_key`, `label`). Same file and shape as the Python app, so a
//! user keeps working in place. Everything else in the file (plugin tokens, launcher paths, ...) is left exactly as found, key order included, and a write is
//! atomic (temp file + rename, indent 4 like `json.dump(..., indent=4)`).
use serde_json::{json, Map, Value};
use std::path::Path;

#[derive(Clone, PartialEq, Default)]
pub struct Account { pub steam_id: String, pub api_key: String, pub label: String }
impl std::fmt::Debug for Account { fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "Account({}, key {})", self.steam_id, if self.api_key.is_empty() { "unset" } else { "set" }) } }

static CONFIG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
/// Held while `config.json` is read, changed and written back. `config.json` was once corrupted by two writers (PlayDate, commit d2eaaf35); in Zest the
/// Companion listener, a sync and the UI all edit it from different threads. Not re-entrant: do not call another locked function while holding it.
pub fn config_lock() -> std::sync::MutexGuard<'static, ()> { CONFIG_LOCK.lock().unwrap_or_else(|e| e.into_inner()) }

fn read(dir: &Path) -> Option<Value> { serde_json::from_str(&std::fs::read_to_string(dir.join("config.json")).ok()?).ok() }

// ───────────────────────── secrets: the vault first, `config.json` as the fallback and for what is not yet moved ─────────────────────────

use crate::secrets::{self, Vault};

/// Where one secret lives: its blob and field in the vault, and the place it had (or has, with no vault) in `config.json`.
struct Loc { blob: String, field: String, path: Vec<String> }
fn loc_plugin(plugin: &str, key: &str) -> Loc { Loc { blob: format!("plugin/{plugin}"), field: key.into(), path: vec![plugin.into(), key.into()] } }
fn loc_api_key(id: &str) -> Loc { Loc { blob: format!("account/{id}"), field: "api_key".into(), path: vec!["accounts".into(), id.into(), "api_key".into()] } }
fn loc_sgdb() -> Loc { Loc { blob: "sgdb".into(), field: "key".into(), path: vec!["sgdb_key".into()] } }

fn plaintext_at(cfg: &Value, path: &[String]) -> Option<String> { let mut v = cfg; for p in path { v = v.get(p)?; } v.as_str().map(String::from) }
/// Takes the plaintext copy out of `config.json` (the empty section it leaves is kept: a plugin's `username` may still live there).
fn remove_plaintext(dir: &Path, path: &[String]) -> std::io::Result<()> {
    let _g = config_lock();
    let Some(mut cfg) = read(dir) else { return Ok(()) };
    let mut v = &mut cfg; for p in &path[..path.len() - 1] { match v.get_mut(p) { Some(n) => v = n, None => return Ok(()) } }
    if v.as_object_mut().map_or(false, |o| o.remove(path.last().unwrap()).is_some()) { write_json(&dir.join("config.json"), &cfg) } else { Ok(()) }
}
fn set_plaintext(dir: &Path, path: &[String], value: &str) -> std::io::Result<()> {
    let _g = config_lock();
    let mut cfg = read(dir).unwrap_or_else(|| json!({}));
    let mut v = &mut cfg;
    for p in &path[..path.len() - 1] { let o = v.as_object_mut().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "config.json holds something that is not an object"))?; v = o.entry(p.clone()).or_insert_with(|| Value::Object(Map::new())); }
    v.as_object_mut().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "config.json holds something that is not an object"))?.insert(path.last().unwrap().clone(), json!(value));
    write_json(&dir.join("config.json"), &cfg)
}

/// A secret: from the vault; else from the plaintext in `config.json`, which is then moved into the vault (and only removed once the vault reads it back). No vault: the plaintext.
fn secret_get_with(vault: Option<&dyn Vault>, dir: &Path, loc: &Loc) -> Option<String> {
    let plain = read(dir).and_then(|c| plaintext_at(&c, &loc.path));
    let Some(v) = vault else { return plain };
    let mut blob = secrets::blob_get(v, &loc.blob);
    if let Some(x) = blob.get(&loc.field).and_then(|x| x.as_str()) { return Some(x.to_string()); }
    let p = plain?;
    if p.is_empty() { return Some(p); }
    blob.insert(loc.field.clone(), json!(p));
    if secrets::blob_put(v, &loc.blob, &blob) { let _ = remove_plaintext(dir, &loc.path); } else { crate::net::log("secrets: the keyring did not take a value; it stays in config.json for now"); }
    Some(p)
}
/// Stores (Some) or removes (None) a secret: the vault when there is one and it takes it, else `config.json`.
fn secret_set_with(vault: Option<&dyn Vault>, dir: &Path, loc: &Loc, value: Option<&str>) -> std::io::Result<()> {
    if let Some(v) = vault {
        let mut blob = secrets::blob_get(v, &loc.blob);
        match value { Some(x) => { blob.insert(loc.field.clone(), json!(x)); } None => { blob.remove(&loc.field); } }
        if secrets::blob_put(v, &loc.blob, &blob) { return remove_plaintext(dir, &loc.path); }
        crate::net::log("secrets: the keyring did not take a value; it goes to config.json");
    }
    match value { Some(x) => set_plaintext(dir, &loc.path, x), None => remove_plaintext(dir, &loc.path) }
}
fn vault_ref() -> Option<std::sync::Arc<dyn Vault>> { secrets::current() }

static SECTION_EDIT: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn section_plain(dir: &Path, plugin: &str) -> Map<String, Value> { read(dir).and_then(|v| v.get(plugin).and_then(|g| g.as_object().cloned())).unwrap_or_default() }
/// A plugin's whole section as its source sees it: the file's values with the secrets from the vault put back (a secret still in the file is moved first).
fn section_with(v: Option<&dyn Vault>, dir: &Path, plugin: &str) -> Map<String, Value> {
    let mut m = section_plain(dir, plugin);
    let Some(v) = v else { return m };
    let name = format!("plugin/{plugin}");
    let mut blob = secrets::blob_get(v, &name);
    let left: Vec<String> = m.iter().filter(|(k, x)| x.is_string() && secrets::is_secret_key(k) && !blob.contains_key(*k)).map(|(k, _)| k.clone()).collect();
    if !left.is_empty() {
        for k in &left { blob.insert(k.clone(), m[k].clone()); }
        if secrets::blob_put(v, &name, &blob) { for k in &left { let _ = remove_plaintext(dir, &[plugin.to_string(), k.clone()]); } }
    }
    for (k, x) in &blob { m.insert(k.clone(), x.clone()); }
    m
}
/// Changes a plugin's section (`f` sees every value, secrets included) and stores the result: secret strings in the vault when it takes them, everything else in `config.json`.
fn section_edit_with(v: Option<&dyn Vault>, dir: &Path, plugin: &str, f: impl FnOnce(&mut Map<String, Value>)) {
    let _one_at_a_time = SECTION_EDIT.lock().unwrap_or_else(|e| e.into_inner());
    let mut m = section_with(v, dir, plugin);
    f(&mut m);
    let secret = |k: &String, x: &Value| x.is_string() && secrets::is_secret_key(k);
    let mut plain = m.clone();
    if let Some(v) = v {
        let sec: Map<String, Value> = m.iter().filter(|(k, x)| secret(k, x)).map(|(k, x)| (k.clone(), x.clone())).collect();
        if secrets::blob_put(v, &format!("plugin/{plugin}"), &sec) { plain.retain(|k, x| !secret(k, x)); } else { crate::net::log("secrets: the keyring did not take a value; it goes to config.json"); }
    }
    let _g = config_lock();
    let mut cfg = read(dir).unwrap_or_else(|| json!({}));
    let Some(o) = cfg.as_object_mut() else { return };
    o.insert(plugin.to_string(), Value::Object(plain));
    let _ = write_json(&dir.join("config.json"), &cfg);
}
/// For the sources that keep their whole login in one section (GOG, Amazon, Xbox): read it ...
pub fn plugin_section(dir: &Path, plugin: &str) -> Map<String, Value> { section_with(vault_ref().as_deref(), dir, plugin) }
/// ... and change it (the section is created when missing, as their own code did).
pub fn plugin_section_edit(dir: &Path, plugin: &str, f: impl FnOnce(&mut Map<String, Value>)) { section_edit_with(vault_ref().as_deref(), dir, plugin, f) }

/// Moves every secret still in `config.json` into the vault (the first start after the vault arrived); does nothing without one. Safe to repeat.
pub fn migrate_secrets(dir: &Path) { if let Some(v) = vault_ref() { migrate_secrets_with(&*v, dir); } }
fn migrate_secrets_with(v: &dyn Vault, dir: &Path) {
    let Some(cfg) = read(dir) else { return };
    let mut moved = 0;
    let mut touch = |loc: Loc| { if plaintext_at(&cfg, &loc.path).map_or(false, |p| !p.is_empty()) { secret_get_with(Some(v), dir, &loc); moved += 1; } };
    touch(loc_sgdb());
    if let Some(a) = cfg.get("accounts").and_then(|a| a.as_object()) { for id in a.keys() { touch(loc_api_key(id)); } }
    if let Some(o) = cfg.as_object() { for (plugin, sec) in o { if matches!(plugin.as_str(), "accounts" | "launchers") { continue; }
        if let Some(m) = sec.as_object() { for (k, x) in m { if x.is_string() && secrets::is_secret_key(k) { touch(loc_plugin(plugin, k)); } } } } }
    if moved > 0 { crate::net::log(&format!("secrets: {moved} secret(s) moved from config.json into the keyring")); }
}

/// `get_active_account`
pub fn load_account(dir: &Path) -> Option<Account> {
    let cfg = read(dir)?;
    let id = cfg.get("active_account")?.as_str()?;
    let a = cfg.get("accounts")?.get(id)?;
    let g = |k: &str| a.get(k).and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let sid = if g("steam_id").is_empty() { id.to_string() } else { g("steam_id") };
    let key = secret_get_with(vault_ref().as_deref(), dir, &loc_api_key(id)).unwrap_or_default().trim().to_string();
    Some(Account { steam_id: sid, api_key: key, label: g("label") })
}

/// The SteamGridDB key (`config.json` top level `sgdb_key`), if any.
/// The SteamGifts name of the active account (PAGYWOSG's personal categories key off it); falls back to the legacy top-level value.
pub fn sg_username(dir: &Path) -> String {
    let Some(v) = read(dir) else { return String::new() };
    let active = v.get("active_account").and_then(|a| a.as_str()).unwrap_or("");
    v.pointer(&format!("/accounts/{active}/sg_username")).and_then(|x| x.as_str()).filter(|x| !x.is_empty())
        .or_else(|| v.get("sg_username").and_then(|x| x.as_str())).unwrap_or("").trim().to_string()
}

/// Remembers the SteamGifts name on the active account (what PlayDate does the first time the browser is logged in): `accounts[active].sg_username`.
pub fn set_sg_username(dir: &Path, name: &str) -> std::io::Result<()> {
    let _g = config_lock();
    let mut cfg = read(dir).unwrap_or_else(|| json!({}));
    let active = cfg.get("active_account").and_then(|a| a.as_str()).unwrap_or("").to_string();
    let Some(acct) = cfg.get_mut("accounts").and_then(|a| a.get_mut(&active)).and_then(|a| a.as_object_mut()) else { return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "no active account")) };
    acct.insert("sg_username".into(), json!(name.trim()));
    if let Some(o) = cfg.as_object_mut() { o.remove("sg_username"); }
    write_json(&dir.join("config.json"), &cfg)
}

pub fn sgdb_key(dir: &Path) -> Option<String> { secret_get_with(vault_ref().as_deref(), dir, &loc_sgdb()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) }

/// Sets (or, with an empty key, removes) the SteamGridDB key at the top level of `config.json`.
pub fn set_sgdb_key(dir: &Path, key: &str) -> std::io::Result<()> {
    let key = key.trim();
    secret_set_with(vault_ref().as_deref(), dir, &loc_sgdb(), (!key.is_empty()).then_some(key))?;
    // the file keeps its shape for a fresh folder (the key itself lives in the vault when there is one)
    let _g = config_lock();
    let mut cfg = read(dir).unwrap_or_else(|| json!({"active_account": null, "sgdb_key": "", "accounts": {}}));
    let o = cfg.as_object_mut().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "config.json is not an object"))?;
    if key.is_empty() || vault_ref().is_none() { o.insert("sgdb_key".into(), json!(key)); } else { o.remove("sgdb_key"); }
    write_json(&dir.join("config.json"), &cfg)
}

/// Sets (or adds) an account and makes it the active one when asked; every other key stays untouched. A fresh dir gets the Python app's starting shape.
pub fn save_account(dir: &Path, acct: &Account, make_active: bool) -> std::io::Result<()> {
    // the API key goes to the vault first (outside the file lock); only when it is there does the file stop holding it
    let in_vault = match vault_ref() { Some(v) if !acct.api_key.is_empty() => { let loc = loc_api_key(&acct.steam_id); let mut b = secrets::blob_get(&*v, &loc.blob); b.insert(loc.field.clone(), json!(acct.api_key)); secrets::blob_put(&*v, &loc.blob, &b) } _ => false };
    let _g = config_lock();
    let mut cfg = read(dir).filter(|v| v.get("accounts").map_or(false, |a| a.is_object())).unwrap_or_else(|| json!({"active_account": null, "sgdb_key": "", "accounts": {}}));
    let o = cfg.as_object_mut().unwrap();
    let accounts = o.get_mut("accounts").and_then(|a| a.as_object_mut()).unwrap();
    let e = accounts.entry(acct.steam_id.clone()).or_insert_with(|| Value::Object(Map::new()));
    let e = e.as_object_mut().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "account entry is not an object"))?;
    e.insert("steam_id".into(), json!(acct.steam_id)); e.insert("label".into(), json!(acct.label));
    if in_vault { e.remove("api_key"); } else { e.insert("api_key".into(), json!(acct.api_key)); }
    if make_active || o.get("active_account").map_or(true, |v| v.as_str().map_or(true, str::is_empty)) { o.insert("active_account".into(), json!(acct.steam_id)); }
    write_json(&dir.join("config.json"), &cfg)
}

/// Every account in `config.json`, in file order, with whether it is the active one (`GET /api/accounts`).
pub fn list_accounts(dir: &Path) -> Vec<(Account, bool)> {
    let Some(cfg) = read(dir) else { return vec![] };
    let active = cfg.get("active_account").and_then(|a| a.as_str()).unwrap_or("").to_string();
    let Some(m) = cfg.get("accounts").and_then(|a| a.as_object()) else { return vec![] };
    m.iter().map(|(id, a)| { let g = |k: &str| a.get(k).and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        let key = secret_get_with(vault_ref().as_deref(), dir, &loc_api_key(id)).unwrap_or_default().trim().to_string();
        (Account { steam_id: if g("steam_id").is_empty() { id.clone() } else { g("steam_id") }, api_key: key, label: g("label") }, *id == active) }).collect()
}

/// `POST /api/account/switch`: the library follows (`games_<id>.db`), which a restart picks up.
pub fn switch_account(dir: &Path, steam_id: &str) -> Result<(), String> {
    let _g = config_lock();
    let mut cfg = read(dir).ok_or("Not configured")?;
    if cfg.get("accounts").and_then(|a| a.get(steam_id)).is_none() { return Err("Account not found".into()); }
    cfg.as_object_mut().ok_or("config.json is not an object")?.insert("active_account".into(), json!(steam_id));
    write_json(&dir.join("config.json"), &cfg).map_err(|e| e.to_string())
}

/// `POST /api/account/remove`: drops the entry only (the library file stays); removing the active one activates the first left, or none.
pub fn remove_account(dir: &Path, steam_id: &str) -> Result<Option<String>, String> {
    let _g = config_lock();
    let mut cfg = read(dir).ok_or("Not configured")?;
    let o = cfg.as_object_mut().ok_or("config.json is not an object")?;
    if let Some(a) = o.get_mut("accounts").and_then(|a| a.as_object_mut()) { a.shift_remove(steam_id); }
    if let Some(v) = vault_ref() { v.forget(&loc_api_key(steam_id).blob); }
    let remaining: Vec<String> = o.get("accounts").and_then(|a| a.as_object()).map(|m| m.keys().cloned().collect()).unwrap_or_default();
    if o.get("active_account").and_then(|a| a.as_str()) == Some(steam_id) { o.insert("active_account".into(), remaining.first().map_or(Value::Null, |s| json!(s))); }
    let now = o.get("active_account").and_then(|a| a.as_str()).map(String::from);
    write_json(&dir.join("config.json"), &cfg).map_err(|e| e.to_string())?;
    Ok(now)
}

/// A plugin's stored value (`config.json[plugin_id][key]`), the place PlayDate's plugins keep their tokens.
pub fn plugin_secret_get(dir: &Path, plugin: &str, key: &str) -> Option<String> {
    // a harmless value (a user name, an id) stays in config.json; only secret-looking keys use the vault
    if secrets::is_secret_key(key) { secret_get_with(vault_ref().as_deref(), dir, &loc_plugin(plugin, key)) } else { read(dir)?.get(plugin)?.get(key)?.as_str().map(String::from) }
}
/// Sets (value Some) or removes (None) one stored value; the rest of the file stays as it is.
pub fn plugin_secret_set(dir: &Path, plugin: &str, key: &str, value: Option<&str>) -> std::io::Result<()> {
    if secrets::is_secret_key(key) && vault_ref().is_some() { return secret_set_with(vault_ref().as_deref(), dir, &loc_plugin(plugin, key), value); }
    let _g = config_lock();
    let mut cfg = read(dir).unwrap_or_else(|| json!({}));
    let o = cfg.as_object_mut().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "config.json is not an object"))?;
    let sec = o.entry(plugin.to_string()).or_insert_with(|| Value::Object(Map::new()));
    let sec = sec.as_object_mut().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "plugin section is not an object"))?;
    match value { Some(v) => { sec.insert(key.to_string(), json!(v)); } None => { sec.remove(key); } }
    write_json(&dir.join("config.json"), &cfg)
}

/// Sets one numeric value in a plugin's section (an expiry time stays a number, as PlayDate wrote it).
pub fn plugin_number_set(dir: &Path, plugin: &str, key: &str, value: i64) -> std::io::Result<()> {
    let _g = config_lock();
    let mut cfg = read(dir).unwrap_or_else(|| json!({}));
    let o = cfg.as_object_mut().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "config.json is not an object"))?;
    let sec = o.entry(plugin.to_string()).or_insert_with(|| Value::Object(Map::new()));
    sec.as_object_mut().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "plugin section is not an object"))?.insert(key.to_string(), json!(value));
    write_json(&dir.join("config.json"), &cfg)
}
/// Removes a plugin's whole section (sign out), leaving the rest of the file.
pub fn plugin_remove(dir: &Path, plugin: &str) -> std::io::Result<()> {
    if let Some(v) = vault_ref() { v.forget(&loc_plugin(plugin, "").blob); }
    let _g = config_lock();
    let Some(mut cfg) = read(dir) else { return Ok(()) };
    if let Some(o) = cfg.as_object_mut() { o.remove(plugin); }
    write_json(&dir.join("config.json"), &cfg)
}

/// Atomic write, 4-space indent.
pub fn write_json(path: &Path, v: &Value) -> std::io::Result<()> {
    use serde::Serialize;
    let mut buf = Vec::new();
    let fmt = serde_json::ser::PrettyFormatter::with_indent(b"    ");
    v.serialize(&mut serde_json::Serializer::with_formatter(&mut buf, fmt)).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    { use std::io::Write; let mut f = std::fs::File::create(&tmp)?; f.write_all(&buf)?; f.sync_all()?; }
    std::fs::rename(&tmp, path).map_err(|e| { let _ = std::fs::remove_file(&tmp); e })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tmp(n: &str) -> std::path::PathBuf { let d = std::env::temp_dir().join(format!("zest_acct_{n}_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d }
    #[test] fn read_write_keeps_the_rest() {
        let d = tmp("keep");
        std::fs::write(d.join("config.json"), r#"{"active_account":"1","sgdb_key":"S","accounts":{"1":{"steam_id":"1","api_key":"A","label":"x","sg_username":"u"}},"gog":{"t":1},"zlast":true}"#).unwrap();
        assert_eq!(load_account(&d).unwrap(), Account { steam_id: "1".into(), api_key: "A".into(), label: "x".into() });
        save_account(&d, &Account { steam_id: "1".into(), api_key: "B".into(), label: "x".into() }, false).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(d.join("config.json")).unwrap()).unwrap();
        assert_eq!(v["accounts"]["1"]["api_key"], "B"); assert_eq!(v["accounts"]["1"]["sg_username"], "u"); assert_eq!(v["gog"]["t"], 1);
        assert_eq!(v.as_object().unwrap().keys().collect::<Vec<_>>(), ["active_account", "sgdb_key", "accounts", "gog", "zlast"]);
        assert!(std::fs::read_to_string(d.join("config.json")).unwrap().contains("\n    \"active_account\""));
    }
    #[test] fn edits_from_many_threads_are_all_kept() {
        let d = tmp("threads");
        std::fs::write(d.join("config.json"), "{}").unwrap();
        let hs: Vec<_> = (0..8).map(|t| { let d = d.clone(); std::thread::spawn(move || for i in 0..40 { plugin_secret_set(&d, &format!("p{t}"), &format!("k{i}"), Some("v")).unwrap(); }) }).collect();
        for h in hs { h.join().unwrap(); }
        let v: Value = serde_json::from_str(&std::fs::read_to_string(d.join("config.json")).unwrap()).unwrap();
        for t in 0..8 { assert_eq!(v[format!("p{t}")].as_object().unwrap().len(), 40, "thread {t} lost edits"); }
    }
    #[test] fn fresh_dir_and_second_account() {
        let d = tmp("fresh");
        assert!(load_account(&d).is_none());
        save_account(&d, &Account { steam_id: "7".into(), api_key: "K".into(), label: "me".into() }, false).unwrap();
        assert_eq!(load_account(&d).unwrap().api_key, "K");
        save_account(&d, &Account { steam_id: "8".into(), api_key: "L".into(), label: "b".into() }, false).unwrap();
        assert_eq!(load_account(&d).unwrap().steam_id, "7", "a second account does not take over unless asked");
        save_account(&d, &Account { steam_id: "8".into(), api_key: "L".into(), label: "b".into() }, true).unwrap();
        assert_eq!(load_account(&d).unwrap().steam_id, "8");
    }
    #[test] fn plugin_secrets_roundtrip_and_leave_the_rest() {
        let d = tmp("sec");
        std::fs::write(d.join("config.json"), r#"{"active_account":"1","gog":{"access_token":"T","username":"u"},"accounts":{}}"#).unwrap();
        assert_eq!(plugin_secret_get(&d, "gog", "access_token").as_deref(), Some("T"));
        plugin_secret_set(&d, "gog", "refresh_token", Some("R")).unwrap(); plugin_secret_set(&d, "toy", "k", Some("v")).unwrap(); plugin_secret_set(&d, "gog", "username", None).unwrap();
        assert_eq!((plugin_secret_get(&d, "gog", "refresh_token").as_deref(), plugin_secret_get(&d, "gog", "access_token").as_deref(), plugin_secret_get(&d, "gog", "username"), plugin_secret_get(&d, "toy", "k").as_deref()), (Some("R"), Some("T"), None, Some("v")));
        let v: Value = serde_json::from_str(&std::fs::read_to_string(d.join("config.json")).unwrap()).unwrap(); assert_eq!(v["active_account"], "1");
    }
    #[test] fn sgdb_key_is_set_at_the_top_level_and_the_account_survives() {
        let d = tmp("sgdb");
        save_account(&d, &Account { steam_id: "7".into(), api_key: "K".into(), label: "me".into() }, true).unwrap();
        set_sgdb_key(&d, "  SG  ").unwrap();
        assert_eq!(sgdb_key(&d).as_deref(), Some("SG")); assert_eq!(load_account(&d).unwrap().api_key, "K");
        set_sgdb_key(&d, "").unwrap(); assert_eq!(sgdb_key(&d), None);
    }
    #[test] fn accounts_list_switch_and_remove_like_playdate() {
        let d = std::env::temp_dir().join(format!("zest_acct_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        let a = |id: &str| Account { steam_id: id.into(), api_key: "k".into(), label: id.into() };
        save_account(&d, &a("1"), true).unwrap(); save_account(&d, &a("2"), false).unwrap();
        let l = list_accounts(&d); assert_eq!(l.len(), 2); assert!(l[0].1 && !l[1].1);
        switch_account(&d, "2").unwrap(); assert_eq!(load_account(&d).unwrap().steam_id, "2");
        assert!(switch_account(&d, "9").is_err());
        assert_eq!(remove_account(&d, "2").unwrap().as_deref(), Some("1"));
        assert_eq!(remove_account(&d, "1").unwrap(), None); assert!(list_accounts(&d).is_empty());
    }
    #[test] fn debug_hides_the_key() { assert!(!format!("{:?}", Account { steam_id: "1".into(), api_key: "SECRET".into(), label: String::new() }).contains("SECRET")); }

    // ── the vault ──
    use crate::secrets::testing::Mem;
    const OLD: &str = r#"{"active_account":"1","sgdb_key":"SG","accounts":{"1":{"steam_id":"1","api_key":"AK","label":"me"}},"gog":{"access_token":"T","refresh_token":"R","username":"u","expires_at":5},"launchers":{"gog":{"prefix":"/p"}}}"#;
    fn cfg(d: &std::path::Path) -> Value { serde_json::from_str(&std::fs::read_to_string(d.join("config.json")).unwrap()).unwrap() }
    #[test] fn existing_secrets_move_into_the_vault_and_the_harmless_values_stay() {
        let d = tmp("mig"); std::fs::write(d.join("config.json"), OLD).unwrap(); let v = Mem::default();
        migrate_secrets_with(&v, &d);
        let c = cfg(&d);
        assert!(c.get("sgdb_key").is_none() && c["accounts"]["1"].get("api_key").is_none() && c["gog"].get("access_token").is_none() && c["gog"].get("refresh_token").is_none(), "no secret is left in the file");
        assert_eq!((c["gog"]["username"].as_str(), c["gog"]["expires_at"].as_i64(), c["launchers"]["gog"]["prefix"].as_str(), c["accounts"]["1"]["label"].as_str()), (Some("u"), Some(5), Some("/p"), Some("me")), "everything else is untouched");
        assert_eq!(secret_get_with(Some(&v), &d, &loc_api_key("1")).as_deref(), Some("AK")); assert_eq!(secret_get_with(Some(&v), &d, &loc_sgdb()).as_deref(), Some("SG"));
        assert_eq!(secret_get_with(Some(&v), &d, &loc_plugin("gog", "refresh_token")).as_deref(), Some("R"));
        migrate_secrets_with(&v, &d); assert_eq!(v.map.lock().unwrap().len(), 3, "a second run changes nothing: account, sgdb and the gog blob");
    }
    #[test] fn a_vault_that_refuses_leaves_everything_in_the_file_and_nothing_is_lost() {
        let d = tmp("brk"); std::fs::write(d.join("config.json"), OLD).unwrap(); let v = Mem::default(); v.broken.store(true, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(secret_get_with(Some(&v), &d, &loc_plugin("gog", "access_token")).as_deref(), Some("T"), "the value is still served");
        assert_eq!(cfg(&d)["gog"]["access_token"], "T", "and still in the file");
        secret_set_with(Some(&v), &d, &loc_plugin("gog", "access_token"), Some("T2")).unwrap(); assert_eq!(cfg(&d)["gog"]["access_token"], "T2", "a refused write goes to the file");
    }
    #[test] fn setting_and_deleting_use_the_vault_and_clear_the_old_plaintext() {
        let d = tmp("set"); std::fs::write(d.join("config.json"), OLD).unwrap(); let v = Mem::default();
        secret_set_with(Some(&v), &d, &loc_plugin("gog", "access_token"), Some("NEW")).unwrap();
        assert!(cfg(&d)["gog"].get("access_token").is_none() && cfg(&d)["gog"]["refresh_token"] == "R", "only that key moved");
        assert_eq!(secret_get_with(Some(&v), &d, &loc_plugin("gog", "access_token")).as_deref(), Some("NEW"));
        secret_set_with(Some(&v), &d, &loc_plugin("gog", "access_token"), None).unwrap(); assert_eq!(secret_get_with(Some(&v), &d, &loc_plugin("gog", "access_token")), None);
        secret_set_with(Some(&v), &d, &loc_plugin("brand_new", "api_key"), Some("K")).unwrap(); assert_eq!(secret_get_with(Some(&v), &d, &loc_plugin("brand_new", "api_key")).as_deref(), Some("K"));
        assert!(cfg(&d).get("brand_new").is_none(), "a new secret never touches the file");
    }
    #[test] fn without_a_vault_everything_works_as_before() {
        let d = tmp("nov"); std::fs::write(d.join("config.json"), OLD).unwrap();
        assert_eq!(secret_get_with(None, &d, &loc_api_key("1")).as_deref(), Some("AK"));
        secret_set_with(None, &d, &loc_plugin("toy", "api_key"), Some("v")).unwrap(); assert_eq!(cfg(&d)["toy"]["api_key"], "v");
        secret_set_with(None, &d, &loc_plugin("toy", "api_key"), None).unwrap(); assert!(cfg(&d)["toy"].get("api_key").is_none());
    }

    #[test] fn a_whole_section_is_read_and_edited_with_its_secrets_in_the_vault() {
        let d = tmp("sec2"); std::fs::write(d.join("config.json"), OLD).unwrap(); let v = Mem::default();
        let m = section_with(Some(&v), &d, "gog");
        assert_eq!((m["access_token"].as_str(), m["refresh_token"].as_str(), m["username"].as_str(), m["expires_at"].as_i64()), (Some("T"), Some("R"), Some("u"), Some(5)));
        assert!(cfg(&d)["gog"].get("access_token").is_none(), "reading moved the secrets out of the file");
        section_edit_with(Some(&v), &d, "gog", |m| { m.insert("access_token".into(), json!("T9")); m.insert("expires_at".into(), json!(77)); m.insert("galaxy_user_id".into(), json!("g")); });
        let c = cfg(&d); assert_eq!((c["gog"]["expires_at"].as_i64(), c["gog"]["galaxy_user_id"].as_str(), c["gog"].get("access_token").is_none()), (Some(77), Some("g"), true));
        assert_eq!(section_with(Some(&v), &d, "gog")["access_token"], "T9");
        section_edit_with(Some(&v), &d, "gog", |m| m.clear()); assert_eq!(section_with(Some(&v), &d, "gog").len(), 0); assert!(v.map.lock().unwrap().get("plugin/gog").is_none(), "an empty section leaves nothing in the vault");
        // a refusing vault keeps the old behaviour
        let d2 = tmp("sec3"); std::fs::write(d2.join("config.json"), "{}").unwrap(); let b = Mem::default(); b.broken.store(true, std::sync::atomic::Ordering::SeqCst);
        section_edit_with(Some(&b), &d2, "xbox", |m| { m.insert("refresh_token".into(), json!("R")); }); assert_eq!(cfg(&d2)["xbox"]["refresh_token"], "R");
        // no vault: exactly as before
        let d3 = tmp("sec4"); section_edit_with(None, &d3, "amazon_games", |m| { m.insert("access_token".into(), json!("A")); }); assert_eq!(cfg(&d3)["amazon_games"]["access_token"], "A");
    }
}
