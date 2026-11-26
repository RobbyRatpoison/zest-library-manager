//! Steam's local files (a port of the Steam parts of `utils.py`): finding Steam, accounts, `localconfig.vdf` playtime, collections, `appmanifest_*.acf`
//! (installed games and names), the binary `appinfo.vdf` cache, the install sync and recording a launch. Decisions in NATIVE_UI_PLAN.md section 14:
//! a game is installed when its manifest says it is fully installed (`StateFlags` bit 4), tools are kept out where games are *added* (appinfo type), not
//! at the installed check, and a launch is recorded only for an installed game.
use rusqlite::{params, Connection, Result};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------- text VDF

/// A text VDF/ACF value: a string or an object (keys keep their order; a repeated key keeps the last value, as the Python `vdf` package does).
#[derive(Debug, Clone, PartialEq)]
pub enum Vdf { Str(String), Obj(Vec<(String, Vdf)>) }

impl Vdf {
    /// A key's value, ignoring case (Steam writes `UserLocalConfigStore` but older files used lower case).
    pub fn get(&self, key: &str) -> Option<&Vdf> {
        match self { Vdf::Obj(v) => v.iter().rev().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v), _ => None }
    }
    pub fn path(&self, keys: &[&str]) -> Option<&Vdf> { keys.iter().try_fold(self, |v, k| v.get(k)) }
    pub fn as_str(&self) -> Option<&str> { match self { Vdf::Str(s) => Some(s), _ => None } }
    pub fn entries(&self) -> &[(String, Vdf)] { match self { Vdf::Obj(v) => v, _ => &[] } }
}

/// Parse text VDF: quoted strings (with `\\ \" \n \t` escapes), `{ }`, `//` comments. Returns None on a structural error.
pub fn parse_vdf(text: &str) -> Option<Vdf> {
    let b = text.as_bytes();
    let mut i = 0;
    let top = parse_obj(b, &mut i, true)?;
    Some(Vdf::Obj(top))
}

fn skip_ws(b: &[u8], i: &mut usize) {
    loop {
        while *i < b.len() && (b[*i] as char).is_ascii_whitespace() { *i += 1; }
        if *i + 1 < b.len() && b[*i] == b'/' && b[*i + 1] == b'/' { while *i < b.len() && b[*i] != b'\n' { *i += 1; } } else { break; }
    }
}

fn parse_str(b: &[u8], i: &mut usize) -> Option<String> {
    if b.get(*i) != Some(&b'"') { return None; }
    *i += 1;
    let mut out = Vec::new();
    while *i < b.len() {
        match b[*i] {
            b'"' => { *i += 1; return Some(String::from_utf8_lossy(&out).into_owned()); }
            b'\\' if *i + 1 < b.len() => { *i += 1; out.push(match b[*i] { b'n' => b'\n', b't' => b'\t', c => c }); *i += 1; }
            c => { out.push(c); *i += 1; }
        }
    }
    None
}

fn parse_obj(b: &[u8], i: &mut usize, top: bool) -> Option<Vec<(String, Vdf)>> {
    let mut out: Vec<(String, Vdf)> = Vec::new();
    loop {
        skip_ws(b, i);
        match b.get(*i) {
            None => return if top { Some(out) } else { None },
            Some(b'}') => { *i += 1; return if top { None } else { Some(out) }; }
            Some(b'"') => {
                let key = parse_str(b, i)?;
                skip_ws(b, i);
                match b.get(*i) {
                    Some(b'{') => { *i += 1; let o = parse_obj(b, i, false)?; out.push((key, Vdf::Obj(o))); }
                    Some(b'"') => { let v = parse_str(b, i)?; out.push((key, Vdf::Str(v))); }
                    _ => return None,
                }
            }
            _ => return None,
        }
    }
}

// ---------------------------------------------------------------- finding Steam

/// The default `steamapps` folders, in the order `find_steam_path` tries them.
pub fn default_steamapps_candidates() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_default();
    vec![home.join(".steam/steam/steamapps"), home.join(".local/share/Steam/steamapps"), PathBuf::from("C:/Program Files (x86)/Steam/steamapps"),
         PathBuf::from("C:/Program Files/Steam/steamapps"), home.join("Library/Application Support/Steam/steamapps")]
}

/// `find_steam_path`: the first default folder that exists, then (Windows) the registry. `over` = a folder the user chose (the Steam folder or its steamapps).
pub fn find_steamapps(over: Option<&Path>) -> Option<PathBuf> {
    if let Some(o) = over {
        if o.join("steamapps").is_dir() { return Some(o.join("steamapps")); }
        if o.is_dir() { return Some(o.to_path_buf()); }
    }
    if let Some(p) = default_steamapps_candidates().into_iter().find(|p| p.exists()) { return Some(p); }
    #[cfg(windows)]
    {
        use winreg::{enums::HKEY_LOCAL_MACHINE, RegKey};
        let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
        for key in [r"SOFTWARE\WOW6432Node\Valve\Steam", r"SOFTWARE\Valve\Steam"] {
            if let Ok(k) = hklm.open_subkey(key) {
                if let Ok(path) = k.get_value::<String, _>("InstallPath") { if !path.is_empty() { return Some(PathBuf::from(path).join("steamapps")); } }
            }
        }
    }
    None
}

/// `find_steam_root`: the folder that holds `steamapps`, `userdata`, `config` and `appcache`.
pub fn steam_root(over: Option<&Path>) -> Option<PathBuf> { find_steamapps(over).and_then(|p| p.parent().map(Path::to_path_buf)) }

/// `get_all_steam_library_paths`: the default `steamapps` plus every extra library folder from `config/libraryfolders.vdf` (old and new file layouts).
pub fn library_paths(root: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    let default = root.join("steamapps");
    if default.is_dir() { seen.push(std::fs::canonicalize(&default).unwrap_or(default.clone())); out.push(default); }
    let Ok(text) = std::fs::read_to_string(root.join("config/libraryfolders.vdf")) else { return out };
    let Some(v) = parse_vdf(&text) else { return out };
    let Some(folders) = v.get("libraryfolders") else { return out };
    for (k, e) in folders.entries() {
        if !k.chars().all(|c| c.is_ascii_digit()) || k.is_empty() { continue; }
        let path = match e { Vdf::Obj(_) => e.get("path").and_then(|p| p.as_str()).unwrap_or("").to_string(), Vdf::Str(s) => s.clone() };
        if path.is_empty() { continue; }
        let steamapps = Path::new(&path).join("steamapps");
        let real = std::fs::canonicalize(&steamapps).unwrap_or(steamapps.clone());
        if steamapps.is_dir() && !seen.contains(&real) { seen.push(real); out.push(steamapps); }
    }
    out
}

// ---------------------------------------------------------------- accounts

const ID64_BASE: u64 = 76561197960265728;

#[derive(Debug, Clone, PartialEq)]
pub struct Account { pub steam_id: String, pub name: String }

/// `detect_steam_id`: accounts found in `userdata/` (newest `localconfig.vdf` first), named from `loginusers.vdf`.
pub fn accounts(root: &Path) -> Vec<Account> {
    let mut names: HashMap<String, String> = HashMap::new();
    if let Some(v) = std::fs::read_to_string(root.join("config/loginusers.vdf")).ok().and_then(|t| parse_vdf(&t)) {
        if let Some(users) = v.get("users") {
            for (id, info) in users.entries() {
                let n = info.get("PersonaName").and_then(|x| x.as_str()).filter(|s| !s.is_empty()).or_else(|| info.get("AccountName").and_then(|x| x.as_str()).filter(|s| !s.is_empty())).unwrap_or(id);
                names.insert(id.clone(), n.to_string());
            }
        }
    }
    let Ok(rd) = std::fs::read_dir(root.join("userdata")) else { return Vec::new() };
    let mut found: Vec<(std::time::SystemTime, Account)> = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let Ok(n) = name.parse::<u64>() else { continue };
        if !name.chars().all(|c| c.is_ascii_digit()) { continue; }
        let lc = e.path().join("config/localconfig.vdf");
        if let Ok(m) = std::fs::metadata(&lc).and_then(|m| m.modified()) {
            let sid = (n + ID64_BASE).to_string();
            found.push((m, Account { name: names.get(&sid).cloned().unwrap_or_else(|| sid.clone()), steam_id: sid }));
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0));
    found.into_iter().map(|f| f.1).collect()
}

/// The `userdata/<id3>` folder for a SteamID64. With an id, never another account's folder (the bug the Python tests pin down); without one, the first numeric folder.
fn account_dir(root: &Path, steam_id: Option<&str>, need: &str) -> Option<PathBuf> {
    let userdata = root.join("userdata");
    if !userdata.is_dir() { return None; }
    if let Some(id) = steam_id.filter(|s| !s.is_empty()) {
        let id3 = id.parse::<u64>().ok()?.checked_sub(ID64_BASE)?.to_string();
        let d = userdata.join(&id3);
        return if need.is_empty() || d.join(need).exists() { Some(d) } else { None };
    }
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&userdata).ok()?.flatten().filter(|e| e.file_name().to_string_lossy().chars().all(|c| c.is_ascii_digit())).map(|e| e.path()).collect();
    // Steam's anonymous placeholder folder "0" never holds a real account: try it last
    dirs.sort_by_key(|d| (d.file_name().map_or(false, |n| n == "0"), d.clone()));
    dirs.into_iter().find(|d| need.is_empty() || d.join(need).exists())
}

pub fn localconfig_path(root: &Path, steam_id: Option<&str>) -> Option<PathBuf> {
    account_dir(root, steam_id, "config/localconfig.vdf").map(|d| d.join("config/localconfig.vdf"))
}

#[derive(Debug, Clone, PartialEq)]
pub struct LocalApp { pub appid: i64, pub playtime_forever: i64, pub last_played: Option<i64> }

/// `fetch_local_library`: every app in `localconfig.vdf` with its playtime (minutes) and last played (Unix time).
pub fn local_library(root: &Path, steam_id: Option<&str>) -> Vec<LocalApp> {
    let Some(path) = localconfig_path(root, steam_id) else { return Vec::new() };
    let Some(v) = std::fs::read(&path).ok().and_then(|b| parse_vdf(&String::from_utf8_lossy(&b))) else { return Vec::new() };
    let Some(apps) = v.path(&["UserLocalConfigStore", "Software", "Valve", "Steam"]).and_then(|s| s.get("apps")) else { return Vec::new() };
    let int = |x: Option<&Vdf>| x.and_then(|v| v.as_str()).and_then(|s| s.trim().parse::<i64>().ok());
    apps.entries().iter().filter(|(k, v)| !k.is_empty() && k.chars().all(|c| c.is_ascii_digit()) && matches!(v, Vdf::Obj(_)))
        .filter_map(|(k, v)| Some(LocalApp { appid: k.parse().ok()?, playtime_forever: int(v.get("Playtime")).unwrap_or(0), last_played: int(v.get("LastPlayed")).filter(|t| *t > 0) }))
        // a non-Steam shortcut added to Steam (its id has the top bit of 32 set, e.g. 4232373341 for a program added by hand) is not a game the account owns
        .filter(|a| !is_shortcut_id(a.appid)).collect()
}

/// Steam gives a non-Steam shortcut (shortcuts.vdf) an id with the top bit of 32 set; real app ids are far below that.
pub fn is_shortcut_id(appid: i64) -> bool { appid >= 0x8000_0000 }

#[derive(Debug, Clone, PartialEq)]
pub struct Collection { pub name: String, pub added: Vec<i64> }

/// `read_steam_collections`: the user's own collections (not deleted, not `hidden`) from the cloud storage file.
pub fn collections(root: &Path, steam_id: Option<&str>) -> BTreeMap<String, Collection> {
    let mut out = BTreeMap::new();
    let Some(dir) = account_dir(root, steam_id, "") else { return out };
    let Some(data) = std::fs::read_to_string(dir.join("config/cloudstorage/cloud-storage-namespace-1.json")).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()) else { return out };
    for entry in data.as_array().map(|a| a.as_slice()).unwrap_or(&[]) {
        let Some(pair) = entry.as_array().filter(|p| p.len() >= 2) else { continue };
        let (Some(key), val) = (pair[0].as_str(), &pair[1]) else { continue };
        if !key.starts_with("user-collections.") || val.get("is_deleted").and_then(|d| d.as_bool()).unwrap_or(false) { continue; }
        let Some(raw) = val.get("value").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) else { continue };
        let Ok(col) = serde_json::from_str::<serde_json::Value>(raw) else { continue };
        let id = col.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let name = col.get("name").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        if id.is_empty() || id == "hidden" || name.is_empty() { continue; }
        let added = col.get("added").and_then(|a| a.as_array()).map(|a| a.iter().filter_map(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64))).collect()).unwrap_or_default();
        out.insert(id.to_string(), Collection { name, added });
    }
    out
}

// ---------------------------------------------------------------- manifests

#[derive(Debug, Clone, PartialEq)]
pub struct Manifest { pub appid: i64, pub name: String, pub state_flags: u32, pub installdir: String, pub has_user_config: bool }

impl Manifest {
    /// Fully installed: Steam sets bit 4 (`StateFlags` 4), and keeps it while an update is pending (6). A download in progress (1026 and the like) has none.
    pub fn fully_installed(&self) -> bool { self.state_flags & 4 != 0 }
}

/// One scan of every `appmanifest_*.acf` in every library folder (the Python scanned twice, once for ids and once for names).
pub fn manifests(root: &Path) -> Vec<Manifest> {
    let mut out = Vec::new();
    for dir in library_paths(root) {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let Some(id) = name.strip_prefix("appmanifest_").and_then(|n| n.strip_suffix(".acf")).and_then(|n| n.parse::<i64>().ok()) else { continue };
            let Ok(text) = std::fs::read(e.path()).map(|b| String::from_utf8_lossy(&b).into_owned()) else { continue };
            let Some(v) = parse_vdf(&text) else { continue };
            let Some(st) = v.get("AppState") else { continue };
            let s = |k: &str| st.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
            out.push(Manifest { appid: id, name: s("name"), state_flags: s("StateFlags").trim().parse().unwrap_or(0), installdir: s("installdir"), has_user_config: st.get("UserConfig").is_some() });
        }
    }
    out
}

/// The appids of fully installed games (no exclusion words here: the sync only flips rows that already exist in the library).
pub fn installed_appids(m: &[Manifest]) -> Vec<i64> { m.iter().filter(|m| m.fully_installed()).map(|m| m.appid).collect() }

const TOOL_WORDS: [&str; 5] = ["steamworks shared", "proton", "steamlinuxruntime", "easyanticheat", "redistributable"];

/// Should this installed manifest become a library entry when games are added from local files? The `appinfo.vdf` type is the signal when there is one
/// (`game`); without it, a manifest with a `UserConfig` whose name and install folder do not look like a tool or runtime.
pub fn is_game_to_add(m: &Manifest, appinfo_type: Option<&str>) -> bool {
    if let Some(t) = appinfo_type.filter(|t| !t.is_empty()) { return t.eq_ignore_ascii_case("game"); }
    if !m.has_user_config { return false; }
    let hay = format!("{} {}", m.name, m.installdir).to_lowercase();
    !TOOL_WORDS.iter().any(|w| hay.contains(w))
}

// ---------------------------------------------------------------- appinfo.vdf

#[derive(Debug, Clone, PartialEq, Default)]
pub struct AppInfo { pub name: String, pub kind: String, pub steam_release_date: Option<i64>, pub original_release_date: Option<i64> }

const APPINFO_MAGIC: u32 = 0x0756_4429;

fn rd_u32(d: &[u8], o: usize) -> Option<u32> { d.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap())) }

/// A value of Steam's binary VDF, only as far as it matters here.
enum Bin { Str(String), Int(i64), Other }

/// Read one value of type `t` at `*o` (a dict is walked by the caller, not read here); None = an unknown type or truncation.
fn read_scalar(d: &[u8], o: &mut usize, t: u8) -> Option<Bin> {
    match t {
        0x01 => { let end = d.get(*o..)?.iter().position(|b| *b == 0)? + *o; let s = String::from_utf8_lossy(&d[*o..end]).into_owned(); *o = end + 1; Some(Bin::Str(s)) }
        0x02 | 0x04 | 0x06 => { let v = i32::from_le_bytes(d.get(*o..*o + 4)?.try_into().ok()?); *o += 4; Some(Bin::Int(v as i64)) }
        0x03 => { *o += 4; d.get(..*o)?; Some(Bin::Other) }
        0x05 => { let mut p = *o; loop { let c = d.get(p..p + 2)?; p += 2; if c == [0, 0] { break; } } *o = p; Some(Bin::Other) }
        0x07 | 0x0A => { let v = i64::from_le_bytes(d.get(*o..*o + 8)?.try_into().ok()?); *o += 8; Some(Bin::Int(v)) }
        _ => None,
    }
}

/// Skip a nested dict whose opening has been read (up to and including its end marker).
fn skip_dict(d: &[u8], o: &mut usize, keys: usize) -> Option<()> {
    loop {
        let t = *d.get(*o)?; *o += 1;
        if t == 0x08 { return Some(()); }
        let kid = rd_u32(d, *o)? as usize; *o += 4;
        if kid >= keys { return None; }
        if t == 0x00 { skip_dict(d, o, keys)?; } else { read_scalar(d, o, t)?; }
    }
}

/// `parse_appinfo` on the file's bytes: `{appid: AppInfo}`. Reads only `appinfo/common` of each record (everything else is skipped without being kept).
/// Empty on a wrong magic or a file too short to hold the header; a damaged record is skipped, the others are kept.
pub fn parse_appinfo(d: &[u8]) -> HashMap<i64, AppInfo> {
    let mut result = HashMap::new();
    if d.len() < 16 || rd_u32(d, 0) != Some(APPINFO_MAGIC) { return result; }
    let kt = rd_u32(d, 8).unwrap_or(u32::MAX) as usize;
    if kt >= d.len() { return result; }
    // the key table: strings after a 4-byte marker; a key id is an index into it
    let mut keys: Vec<&[u8]> = Vec::new();
    let mut p = kt + 4;
    while p < d.len() { let Some(e) = d[p..].iter().position(|b| *b == 0) else { break }; keys.push(&d[p..p + e]); p += e + 1; }
    let key_id = |name: &str| keys.iter().position(|k| *k == name.as_bytes());
    let (Some(k_appinfo), Some(k_common)) = (key_id("appinfo"), key_id("common")) else { return result };
    let (k_name, k_type, k_srd, k_ord) = (key_id("name"), key_id("type"), key_id("steam_release_date"), key_id("original_release_date"));
    let mut opener = vec![0u8]; opener.extend((k_appinfo as u32).to_le_bytes());
    let mut rec = 16;
    while rec + 8 <= kt {
        let (appid, size) = (rd_u32(d, rec).unwrap(), rd_u32(d, rec + 4).unwrap() as usize);
        if appid == 0 && size == 0 { break; }
        let next = rec + 8 + size;
        if next > kt { break; }
        // the VDF follows the fixed 60-byte metadata; if the opener is not there, look for it as the Python did
        let start = if rec + 8 + 60 < next && d.get(rec + 8 + 60..).map_or(false, |t| t.starts_with(&opener)) { Some(rec + 8 + 60) }
            else { (rec + 48..(rec + 128).min(next)).find(|i| d.get(*i..).map_or(false, |t| t.starts_with(&opener))) };
        if let Some(mut o) = start {
            o += 5;   // the appinfo dict's opener: type + key id
            let mut info = AppInfo::default();
            let mut found = false;
            // appinfo's entries: walk until the `common` dict, skipping the rest
            let ok = (|| -> Option<()> {
                loop {
                    let t = *d.get(o)?; o += 1;
                    if t == 0x08 { return Some(()); }
                    let kid = rd_u32(d, o)? as usize; o += 4;
                    if kid >= keys.len() { return None; }
                    if t == 0x00 && kid == k_common {
                        loop {
                            let t2 = *d.get(o)?; o += 1;
                            if t2 == 0x08 { break; }
                            let k2 = rd_u32(d, o)? as usize; o += 4;
                            if k2 >= keys.len() { return None; }
                            if t2 == 0x00 { skip_dict(d, &mut o, keys.len())?; continue; }
                            match read_scalar(d, &mut o, t2)? {
                                Bin::Str(s) if Some(k2) == k_name => { info.name = s; found = true; }
                                Bin::Str(s) if Some(k2) == k_type => { info.kind = s; found = true; }
                                Bin::Int(v) if Some(k2) == k_srd => info.steam_release_date = Some(v),
                                Bin::Int(v) if Some(k2) == k_ord => info.original_release_date = Some(v),
                                Bin::Str(s) if Some(k2) == k_srd => info.steam_release_date = s.trim().parse().ok(),
                                Bin::Str(s) if Some(k2) == k_ord => info.original_release_date = s.trim().parse().ok(),
                                _ => {}
                            }
                        }
                        return Some(());
                    }
                    if t == 0x00 { skip_dict(d, &mut o, keys.len())?; } else { read_scalar(d, &mut o, t)?; }
                }
            })();
            let _ = (ok, found);
            if !info.name.is_empty() || !info.kind.is_empty() { result.insert(appid as i64, info); }
        }
        rec = next;
    }
    result
}

/// Read and parse `appcache/appinfo.vdf` under a Steam root (the file is read once and dropped).
pub fn read_appinfo(root: &Path) -> HashMap<i64, AppInfo> { std::fs::read(root.join("appcache/appinfo.vdf")).map(|d| parse_appinfo(&d)).unwrap_or_default() }

// ---------------------------------------------------------------- database

/// `sync_local_install_status`: every Steam game not installed, then the given ones installed, in one transaction (no moment where everything reads as
/// uninstalled). Returns the number set installed.
pub fn sync_installed(conn: &Connection, ids: &[i64]) -> Result<usize> {
    let tx = conn.unchecked_transaction()?;
    tx.execute("UPDATE games SET installed = 0 WHERE platform = 'steam'", [])?;
    let mut n = 0;
    { let mut st = tx.prepare("UPDATE games SET installed = 1 WHERE appid = ?")?; for id in ids { st.execute([id])?; n += 1; } }
    tx.commit()?;
    Ok(n)
}

/// `record_launch`: for an installed game, last played = now and `Never Played` becomes `Unfinished`. Returns the time written, None when the game is not
/// installed (launching it only starts Steam's install prompt: that is not a launch).
pub fn record_launch(conn: &Connection, appid: i64, now: i64) -> Result<Option<i64>> {
    let row: Option<(Option<i64>, Option<String>)> = conn.query_row("SELECT installed, completion_status FROM games WHERE appid = ?", [appid], |r| Ok((r.get(0)?, r.get(1)?))).ok();
    let Some((installed, status)) = row else { return Ok(None) };
    if installed != Some(1) { return Ok(None); }
    if status.as_deref() == Some("Never Played") { conn.execute("UPDATE games SET last_played = ?, completion_status = 'Unfinished' WHERE appid = ?", params![now, appid])?; }
    else { conn.execute("UPDATE games SET last_played = ? WHERE appid = ?", params![now, appid])?; }
    Ok(Some(now))
}

#[cfg(test)]
mod shortcut_tests {
    #[test] fn non_steam_shortcuts_are_not_owned_games() {
        let d = std::env::temp_dir().join(format!("zest_shortcuts_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d);
        let cfg = d.join("userdata/94548698/config"); std::fs::create_dir_all(&cfg).unwrap();
        std::fs::write(cfg.join("localconfig.vdf"), "\"UserLocalConfigStore\"\n{\n\"Software\"\n{\n\"Valve\"\n{\n\"Steam\"\n{\n\"apps\"\n{\n\"620\"\n{\n\"Playtime\"\t\t\"90\"\n}\n\"4232373341\"\n{\n\"Playtime\"\t\t\"5\"\n}\n}\n}\n}\n}\n}\n").unwrap();
        let ids: Vec<i64> = super::local_library(&d, Some("76561198054814426")).into_iter().map(|a| a.appid).collect();
        assert_eq!(ids, vec![620]);
        assert!(super::is_shortcut_id(4232373341) && !super::is_shortcut_id(3_500_000));
        let _ = std::fs::remove_dir_all(&d);
    }
}
