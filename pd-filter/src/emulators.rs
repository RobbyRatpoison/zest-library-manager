//! Emulator support: port of PlayDate's `emulators.py` (config in `emulators.json`, built-in definitions from `known_emulators.py`, exported to
//! `data/known_emulators.json` by a script so the two cannot drift by hand-copying).
//!
//! An emulated game is a row whose `platform` is the system id (`snes`, `ps2`, ...) and whose `platform_id` is the ROM file (or, for PS3 folders and Vita3K
//! apps, the game folder / title id). The config file keeps PlayDate's exact shape, so both programs read the same `emulators.json`; entries are handled as
//! JSON values and only the keys this module knows are touched.
use crate::sources::Action;
use regex::Regex;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Debug, Clone)]
pub struct Known {
    pub id: String, pub name: String, pub binary_names: Vec<String>, pub appimage_names: Vec<String>, pub platforms: Vec<String>,
    pub extensions: BTreeMap<String, Vec<String>>, pub args: Vec<String>, pub scan_mode: Option<String>, pub cores_search: BTreeMap<String, Vec<String>>,
}
struct KnownFile { platform_names: Vec<(String, String)>, emulators: Vec<Known> }

fn strs(v: Option<&Value>) -> Vec<String> { v.and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default() }
fn str_map(v: Option<&Value>) -> BTreeMap<String, Vec<String>> { v.and_then(Value::as_object).map(|o| o.iter().map(|(k, x)| (k.clone(), strs(Some(x)))).collect()).unwrap_or_default() }
fn data() -> &'static KnownFile {
    static D: OnceLock<KnownFile> = OnceLock::new();
    D.get_or_init(|| {
        let v: Value = serde_json::from_str(include_str!("../data/known_emulators.json")).expect("known_emulators.json");
        let platform_names = v["platform_names"].as_array().unwrap().iter().map(|p| (p[0].as_str().unwrap().to_string(), p[1].as_str().unwrap().to_string())).collect();
        let emulators = v["emulators"].as_array().unwrap().iter().map(|e| Known {
            id: e["id"].as_str().unwrap().into(), name: e["name"].as_str().unwrap().into(), binary_names: strs(e.get("binary_names")), appimage_names: strs(e.get("appimage_names")),
            platforms: strs(e.get("platforms")), extensions: str_map(e.get("extensions")), args: strs(e.get("args")), scan_mode: e.get("scan_mode").and_then(Value::as_str).map(String::from),
            cores_search: str_map(e.get("cores_search")) }).collect();
        KnownFile { platform_names, emulators }
    })
}
pub fn known_list() -> &'static [Known] { &data().emulators }
pub fn known(id: &str) -> Option<&'static Known> { known_list().iter().find(|k| k.id == id) }
/// Every emulated system with its display name, in PlayDate's order.
pub fn platform_names() -> &'static [(String, String)] { &data().platform_names }
pub fn platform_label(id: &str) -> String { platform_names().iter().find(|(p, _)| p == id).map(|(_, l)| l.clone()).unwrap_or_else(|| id.to_string()) }

fn home() -> PathBuf { crate::installdir::home() }
fn exists_exec(p: &Path) -> bool {
    #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; p.is_file() && p.metadata().map(|m| m.permissions().mode() & 0o111 != 0).unwrap_or(false) }
    #[cfg(not(unix))] { p.is_file() }
}
fn which(name: &str) -> Option<PathBuf> {
    if name.is_empty() { return None; }
    if name.contains(std::path::MAIN_SEPARATOR) { let p = PathBuf::from(name); return exists_exec(&p).then_some(p); }
    std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join(name)).find(|f| exists_exec(f)))
}

// ---------------------------------------------------------------- config file
pub fn config_path(root: &Path) -> PathBuf { root.join("emulators.json") }
pub fn load(root: &Path) -> Vec<Value> {
    std::fs::read_to_string(config_path(root)).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()).and_then(|v| v.as_array().cloned()).unwrap_or_default()
}
pub fn save(root: &Path, list: &[Value]) -> Result<(), String> {
    let p = config_path(root); let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(list).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &p).map_err(|e| e.to_string())
}
pub fn entry_id(e: &Value) -> &str { e.get("id").and_then(Value::as_str).unwrap_or("") }
pub fn get_entry(root: &Path, id: &str) -> Option<Value> { load(root).into_iter().find(|e| entry_id(e) == id) }
pub fn enabled(e: &Value) -> bool { e.get("enabled").and_then(Value::as_bool).unwrap_or(true) }
pub fn binary(e: &Value) -> String { e.get("binary").and_then(Value::as_str).unwrap_or("").to_string() }
pub fn args(e: &Value) -> Vec<String> {
    e.get("args").and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_else(|| vec!["{rom}".into()])
}
pub fn display_name(e: &Value) -> String {
    known(entry_id(e)).map(|k| k.name.clone()).or_else(|| e.get("name").and_then(Value::as_str).map(String::from)).unwrap_or_else(|| entry_id(e).to_string())
}
/// A platform's folders: the config holds one string or a list; empty strings are dropped.
pub fn platform_dirs(e: &Value, platform: &str) -> Vec<String> {
    match e.get("platforms").and_then(|p| p.get(platform)) {
        Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_str()).filter(|s| !s.is_empty()).map(String::from).collect(),
        Some(Value::String(s)) if !s.is_empty() => vec![s.clone()],
        _ => vec![],
    }
}
/// The systems an entry covers, in the order shown: a known emulator's list, else the entry's own keys.
pub fn entry_platforms(e: &Value) -> Vec<String> {
    match known(entry_id(e)) { Some(k) => k.platforms.clone(), None => e.get("platforms").and_then(Value::as_object).map(|o| o.keys().cloned().collect()).unwrap_or_default() }
}
pub fn stored_core(e: &Value, platform: &str) -> String { e.get("cores").and_then(|c| c.get(platform)).and_then(Value::as_str).unwrap_or("").to_string() }

// ---------------------------------------------------------------- binary and folder detection
fn flatpak_export_dirs() -> Vec<PathBuf> {
    if cfg!(target_os = "linux") { vec![home().join(".local/share/flatpak/exports/bin"), PathBuf::from("/var/lib/flatpak/exports/bin")] } else { vec![] }
}
pub fn flatpak_app_id(binary: &str) -> Option<String> {
    let p = Path::new(binary); let dir = p.parent()?;
    flatpak_export_dirs().iter().any(|d| d == dir).then(|| p.file_name().map(|n| n.to_string_lossy().to_string())).flatten()
}
fn appimage_dirs() -> Vec<PathBuf> { if cfg!(target_os = "linux") { vec![home().join("Applications"), home().join("Downloads"), home().join(".local/bin")] } else { vec![] } }
fn windows_dirs() -> Vec<PathBuf> {
    if !cfg!(windows) { return vec![]; }
    vec![home().join("Downloads"), home().join("Desktop"), PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default()).join("Programs")]
}
fn mac_app_dirs() -> Vec<PathBuf> { if cfg!(target_os = "macos") { vec![PathBuf::from("/Applications"), home().join("Applications")] } else { vec![] } }
fn mac_brew_dirs() -> Vec<PathBuf> { if cfg!(target_os = "macos") { vec![PathBuf::from("/opt/homebrew/bin"), PathBuf::from("/usr/local/bin")] } else { vec![] } }

/// Where a known emulator's program is: PATH, Flatpak exports, AppImages in the usual folders (made executable), Homebrew, `.app` bundles, Windows folders.
/// Empty when nothing is found.
pub fn auto_detect_binary(id: &str) -> String {
    let Some(k) = known(id) else { return String::new() };
    for n in &k.binary_names { if let Some(p) = which(n) { return p.to_string_lossy().into(); } }
    for n in k.binary_names.iter().filter(|n| n.contains('.')) {
        for d in flatpak_export_dirs() { let c = d.join(n); if exists_exec(&c) { return c.to_string_lossy().into(); } }
    }
    let pats: Vec<String> = k.appimage_names.iter().map(|p| p.to_lowercase()).collect();
    if !pats.is_empty() {
        for d in appimage_dirs() {
            let Ok(rd) = std::fs::read_dir(&d) else { continue };
            let mut names: Vec<_> = rd.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect(); names.sort();
            for f in names {
                let fl = f.to_lowercase();
                if fl.ends_with(".appimage") && pats.iter().any(|p| fl.contains(p)) {
                    let full = d.join(&f);
                    #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; if let Ok(m) = full.metadata() { let mut perm = m.permissions(); if perm.mode() & 0o111 == 0 { perm.set_mode(perm.mode() | 0o111); let _ = std::fs::set_permissions(&full, perm); } } }
                    return full.to_string_lossy().into();
                }
            }
        }
    }
    for d in mac_brew_dirs() { for n in &k.binary_names { let c = d.join(n); if exists_exec(&c) { return c.to_string_lossy().into(); } } }
    for d in mac_app_dirs() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for b in rd.flatten().filter(|b| b.file_name().to_string_lossy().ends_with(".app")) {
            for n in &k.binary_names { let c = b.path().join("Contents/MacOS").join(n); if exists_exec(&c) { return c.to_string_lossy().into(); } }
        }
    }
    if cfg!(windows) {
        let mut names: HashSet<String> = k.binary_names.iter().map(|n| n.to_lowercase()).collect();
        for n in &k.binary_names { if !n.to_lowercase().ends_with(".exe") { names.insert(format!("{}.exe", n.to_lowercase())); } }
        for d in windows_dirs() {
            let Ok(rd) = std::fs::read_dir(&d) else { continue };
            for e in rd.flatten() {
                let f = e.file_name().to_string_lossy().to_lowercase();
                if names.contains(&f) { return e.path().to_string_lossy().into(); }
                if e.path().is_dir() { if let Ok(inner) = std::fs::read_dir(e.path()) { for i in inner.flatten() { if names.contains(&i.file_name().to_string_lossy().to_lowercase()) { return i.path().to_string_lossy().into(); } } } }
            }
        }
    }
    String::new()
}

/// RetroArch's core folders per OS.
fn retroarch_core_dirs() -> Vec<PathBuf> {
    let h = home();
    if cfg!(windows) { let a = |k: &str| PathBuf::from(std::env::var_os(k).unwrap_or_default()).join("RetroArch/cores"); vec![a("APPDATA"), a("LOCALAPPDATA")] }
    else if cfg!(target_os = "macos") { vec![h.join("Library/Application Support/RetroArch/cores"), h.join(".config/retroarch/cores"), PathBuf::from("/Applications/RetroArch.app/Contents/Resources/cores")] }
    else { vec![h.join(".config/retroarch/cores"), h.join(".var/app/org.libretro.RetroArch/config/retroarch/cores"), PathBuf::from("/usr/lib/retroarch/cores"),
        PathBuf::from("/usr/lib/x86_64-linux-gnu/retroarch/cores"), PathBuf::from("/usr/share/libretro/cores")] }
}
/// Cores found on disk for a system, in the order of preference `known_emulators` lists them: (name, path).
pub fn list_cores_for_platform(platform: &str) -> Vec<(String, String)> { list_cores_in(platform, &retroarch_core_dirs()) }
fn list_cores_in(platform: &str, dirs: &[PathBuf]) -> Vec<(String, String)> {
    let Some(k) = known("retroarch") else { return vec![] };
    let Some(cands) = k.cores_search.get(platform) else { return vec![] };
    let (mut found, mut seen): (Vec<(String, String)>, HashSet<&String>) = (vec![], HashSet::new());
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(d) else { continue };
        let entries: Vec<String> = rd.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        for c in cands {
            if seen.contains(c) { continue }
            for f in &entries {
                let lower = f.to_lowercase();
                let stem = [".so", ".dll", ".dylib"].iter().find(|e| lower.ends_with(**e)).map(|e| &f[..f.len() - e.len()]).unwrap_or(f);
                if stem == c { found.push((c.clone(), d.join(f).to_string_lossy().into())); seen.insert(c); break; }
            }
        }
    }
    found
}

fn dir_candidates(scan_mode: &str, binary: &str) -> Vec<PathBuf> {
    let h = home(); let appdata = std::env::var_os("APPDATA").map(PathBuf::from); let bin_dir = Path::new(binary).parent().filter(|_| !binary.is_empty()).map(PathBuf::from);
    let mut v: Vec<PathBuf> = vec![];
    match scan_mode {
        "vita3k_app" => {
            v.push(h.join(".local/share/Vita3K/Vita3K/ux0")); v.push(h.join(".var/app/org.vita3k.Vita3K/data/Vita3K/ux0")); v.push(h.join("Library/Application Support/Vita3K/Vita3K/ux0"));
            if let Some(a) = &appdata { v.push(a.join("Vita3K/Vita3K/ux0")); }
            if let Some(b) = &bin_dir { v.push(b.join("ux0")); }
        }
        "ps3_folder" => {
            v.push(h.join(".config/rpcs3/dev_hdd0/game")); v.push(h.join(".var/app/net.rpcs3.RPCS3/config/rpcs3/dev_hdd0/game")); v.push(h.join("Library/Application Support/rpcs3/dev_hdd0/game"));
            if let Some(a) = &appdata { v.push(a.join("rpcs3/dev_hdd0/game")); }
            if let Some(b) = &bin_dir { v.push(b.join("dev_hdd0/game")); }
        }
        _ => {}
    }
    v
}
/// Folders that exist where this emulator keeps its own game library (PS3 and Vita3K only).
pub fn auto_detect_platform_dir(id: &str, binary: &str) -> Vec<String> {
    let mode = known(id).and_then(|k| k.scan_mode.clone()).unwrap_or_default();
    dir_candidates(&mode, binary).into_iter().filter(|p| p.is_dir()).map(|p| p.to_string_lossy().into()).collect()
}

// ---------------------------------------------------------------- editing the list
/// Add a known emulator, detecting its program and folders. `Err` when unknown or already added.
pub fn add_emulator(root: &Path, id: &str) -> Result<Value, String> {
    let k = known(id).ok_or("Unknown emulator.")?;
    let mut list = load(root);
    if list.iter().any(|e| entry_id(e) == id) { return Err("Already added.".into()); }
    let bin = auto_detect_binary(id);
    let mut plats = serde_json::Map::new();
    for p in &k.platforms { let d = auto_detect_platform_dir(id, &bin); plats.insert(p.clone(), if d.is_empty() { json!([""]) } else { json!(d) }); }
    let mut e = json!({"id": id, "binary": bin, "platforms": plats, "args": k.args, "enabled": true});
    if !k.cores_search.is_empty() { e["cores"] = json!({}); }
    list.push(e.clone()); save(root, &list)?; Ok(e)
}
pub fn remove_emulator(root: &Path, id: &str) -> Result<(), String> { let l: Vec<Value> = load(root).into_iter().filter(|e| entry_id(e) != id).collect(); save(root, &l) }

#[derive(Default, Clone)]
pub struct Update { pub binary: Option<String>, pub platform_dirs: Option<BTreeMap<String, Vec<String>>>, pub args: Option<Vec<String>>, pub enabled: Option<bool>, pub cores: Option<BTreeMap<String, String>> }
pub fn update_emulator(root: &Path, id: &str, u: &Update) -> Result<(), String> {
    let mut list = load(root);
    if let Some(e) = list.iter_mut().find(|e| entry_id(e) == id) {
        if let Some(b) = &u.binary { e["binary"] = json!(b); }
        if let Some(d) = &u.platform_dirs { for (p, dirs) in d {
            // one folder stays a plain string, as the web page saves it; several are a list
            e["platforms"][p] = if dirs.len() == 1 { json!(dirs[0]) } else if dirs.is_empty() { json!("") } else { json!(dirs) };
        } }
        if let Some(a) = &u.args { e["args"] = json!(a); }
        if let Some(en) = u.enabled { e["enabled"] = json!(en); }
        if let Some(c) = &u.cores { for (p, path) in c { e["cores"][p] = json!(path); } }
    }
    save(root, &list)
}
/// A user-defined emulator for one system, borrowing the file extensions of a known emulator that covers it. Returns the new entry.
pub fn add_custom_emulator(root: &Path, name: &str, platform: &str, binary: &str, args: &[String]) -> Result<Value, String> {
    if name.trim().is_empty() { return Err("A name is required.".into()); }
    if platform.trim().is_empty() { return Err("Choose a system.".into()); }
    let mut list = load(root);
    let ids: HashSet<String> = list.iter().map(|e| entry_id(e).to_string()).collect();
    let mut i = 1; while ids.contains(&format!("custom_{i}")) { i += 1; }
    let mut e = json!({"id": format!("custom_{i}"), "name": name.trim(), "binary": binary.trim(), "platforms": {platform: ""},
        "args": if args.is_empty() { vec!["{rom}".to_string()] } else { args.to_vec() }, "enabled": true, "custom": true});
    if let Some(ex) = known_list().iter().find_map(|k| k.extensions.get(platform)) { e["extensions"] = json!({platform: ex}); }
    list.push(e.clone()); save(root, &list)?; Ok(e)
}
/// After a restore or a move: any known emulator whose stored program is gone is looked for again. Returns whether anything changed.
pub fn resync_binaries(root: &Path) -> bool {
    let mut list = load(root); let mut changed = false;
    for e in list.iter_mut() {
        let b = binary(e);
        if !b.is_empty() && exists_exec(Path::new(&b)) { continue; }
        if e.get("custom").and_then(Value::as_bool).unwrap_or(false) { continue; }
        let found = auto_detect_binary(entry_id(e));
        if !found.is_empty() && found != b { e["binary"] = json!(found); changed = true; }
    }
    if changed { let _ = save(root, &list); }
    changed
}

// ---------------------------------------------------------------- Flatpak access
fn flatpak_grants(app: &str) -> Vec<String> {
    let Ok(out) = std::process::Command::new("flatpak").args(["info", "--show-permissions", app]).output() else { return vec![] };
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Some(rest) = line.trim().strip_prefix("filesystems=") {
            return rest.split(';').map(|g| { let g = g.trim(); for s in [":ro", ":rw", ":create"] { if let Some(x) = g.strip_suffix(s) { return x.to_string(); } } g.to_string() }).filter(|g| !g.is_empty()).collect();
        }
    }
    vec![]
}
pub fn flatpak_path_covered(grants: &[String], path: &str) -> bool {
    let h = home(); let path = std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
    grants.iter().any(|g| {
        if g == "host" || g == "host-os" { return true; }
        if g == "home" { return path.starts_with(&h); }
        let gp = if let Some(r) = g.strip_prefix("~/") { h.join(r) } else { PathBuf::from(g) };
        path.starts_with(std::fs::canonicalize(&gp).unwrap_or(gp))
    })
}
/// (system, message, fix command) for each folder a Flatpak emulator cannot reach.
pub fn flatpak_warnings(e: &Value) -> Vec<(String, String, String)> {
    let Some(app) = flatpak_app_id(&binary(e)) else { return vec![] };
    let grants = flatpak_grants(&app); let mut out = vec![];
    for p in entry_platforms(e) { for d in platform_dirs(e, &p) {
        if Path::new(&d).is_dir() && !flatpak_path_covered(&grants, &d) {
            out.push((p.clone(), format!("{app} (Flatpak) cannot access this folder."), format!("flatpak override --user --filesystem={d} {app}")));
        }
    } }
    out
}

// ---------------------------------------------------------------- names
/// Python's `os.path.splitext(name)[0]`: the last dot splits, but a leading dot is part of the name.
fn stem(name: &str) -> &str {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let off = name.len() - base.len();
    match base.rfind('.') { Some(i) if i > 0 && !base[..i].chars().all(|c| c == '.') => &name[..off + i], _ => name }
}
fn ext_lower(name: &str) -> String { let s = stem(name); name[s.len()..].to_lowercase() }
/// A game name from a ROM file name: region codes, revisions, disc markers and anything else in brackets are dropped.
pub fn clean_name(filename: &str) -> String {
    static A: OnceLock<Regex> = OnceLock::new(); static B: OnceLock<Regex> = OnceLock::new();
    let a = A.get_or_init(|| Regex::new(r"(?i)\s*[\(\[](USA|EUR|JAP|JPN|PAL|NTSC|World|En|Fr|De|Es|It|Rev\s*\w+|v[\d.]+|Disc\s*\d+|!|b\d+|\d{4})[^\)\]]*[\)\]]").unwrap());
    let b = B.get_or_init(|| Regex::new(r"\s*[\(\[][^\)\]]*[\)\]]").unwrap());
    let n = a.replace_all(stem(filename), ""); b.replace_all(&n, "").trim().to_string()
}
/// The `TITLE` of a PS Vita / PSP `param.sfo`.
pub fn read_sfo_title(path: &Path) -> Option<String> {
    let d = std::fs::read(path).ok()?; sfo_title(&d)
}
pub fn sfo_title(d: &[u8]) -> Option<String> {
    let u32at = |o: usize| d.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize);
    let u16at = |o: usize| d.get(o..o + 2).map(|b| u16::from_le_bytes(b.try_into().unwrap()) as usize);
    if d.get(..4)? != b"\0PSF" { return None; }
    let (lab, dat, n) = (u32at(8)?, u32at(12)?, u32at(16)?);
    for i in 0..n {
        let base = 20 + i * 16; let (ko, dl, dof) = (u16at(base)?, u32at(base + 4)?, u32at(base + 12)?);
        let key = d.get(lab + ko..)?; let key = &key[..key.iter().position(|b| *b == 0).unwrap_or(key.len())];
        if key == b"TITLE" {
            let v = d.get(dat + dof..dat + dof + dl)?; let end = v.iter().rposition(|b| *b != 0).map_or(0, |p| p + 1);
            let t = String::from_utf8_lossy(&v[..end]).trim().to_string();
            return if t.is_empty() { None } else { Some(t) };
        }
    }
    None
}

// ---------------------------------------------------------------- scanning
fn mame_binary(root: &Path) -> String {
    if let Some(e) = get_entry(root, "mame") { let b = binary(&e); if !b.is_empty() && (Path::new(&b).is_file() || which(&b).is_some()) { return b; } }
    for n in ["mame", "mame64"] { if let Some(p) = which(n) { return p.to_string_lossy().into(); } }
    if let Some(k) = known("mame") { for n in k.binary_names.iter().filter(|n| n.contains('.')) { for d in flatpak_export_dirs() { let c = d.join(n); if exists_exec(&c) { return c.to_string_lossy().into(); } } } }
    String::new()
}
/// MAME's own title for a machine id (`mame -listfull id`), cut off after ten seconds.
fn mame_lookup_name(binary: &str, id: &str) -> Option<String> {
    let (b, i) = (binary.to_string(), id.to_string()); let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || { let _ = { use crate::proc::Hide; tx.send(std::process::Command::new(&b).args(["-listfull", &i]).hidden().output()) }; });
    let out = rx.recv_timeout(std::time::Duration::from_secs(10)).ok()?.ok()?;
    for line in String::from_utf8_lossy(&out.stdout).lines().skip(1) {
        let line = line.trim(); if line.is_empty() { continue; }
        let mut parts = line.splitn(2, char::is_whitespace);
        if let (Some(a), Some(rest)) = (parts.next(), parts.next()) { if a == id { let n = rest.trim().trim_matches('"').to_string(); return if n.is_empty() { None } else { Some(n) }; } }
    }
    None
}
fn extensions_for(root: &Path, id: &str, platform: &str) -> HashSet<String> {
    let from = |v: Option<&Value>| -> HashSet<String> {
        match v { Some(Value::Object(o)) => o.get(platform).and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default(),
            Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(), _ => HashSet::new() }
    };
    if let Some(k) = known(id) { return k.extensions.get(platform).map(|v| v.iter().cloned().collect()).unwrap_or_default(); }
    from(get_entry(root, id).as_ref().and_then(|e| e.get("extensions")))
}
fn next_appid(c: &Connection) -> Result<i64, String> {
    let min: Option<i64> = c.query_row("SELECT MIN(appid) FROM games WHERE appid < 0", [], |r| r.get(0)).map_err(|e| e.to_string())?;
    Ok(min.map_or(-1, |m| m - 1))
}
fn known_ids(c: &Connection, platform: &str) -> Result<(HashSet<String>, HashSet<String>), String> {
    let existing = c.prepare("SELECT platform_id FROM games WHERE platform = ?").and_then(|mut s| s.query_map([platform], |r| r.get::<_, Option<String>>(0)).map(|r| r.flatten().flatten().collect())).map_err(|e| e.to_string())?;
    let black = c.prepare("SELECT platform_id FROM blacklist WHERE platform_id IS NOT NULL").and_then(|mut s| s.query_map([], |r| r.get::<_, String>(0)).map(|r| r.flatten().collect())).unwrap_or_default();
    Ok((existing, black))
}
fn insert(c: &Connection, platform: &str, name: &str, pid: &str, now: i64) -> Result<i64, String> {
    let appid = next_appid(c)?;
    c.execute("INSERT OR IGNORE INTO games (appid, name, platform, platform_id, date_added, completion_status, installed, art_fetched, meta_fetched, cheevos_fetched, protondb_fetched, hltb_fetched)
        VALUES (?, ?, ?, ?, ?, 'Never Played', 1, '0', '0', '0', '0', '0')", params![appid, name, platform, pid, now]).map_err(|e| e.to_string())?;
    Ok(appid)
}
fn is_ps3_dir(p: &Path) -> bool { p.join("PS3_GAME").is_dir() || p.join("PS3_DISC.SFB").is_file() }
fn sorted_entries(dir: &Path) -> Vec<std::fs::DirEntry> { let mut v: Vec<_> = std::fs::read_dir(dir).map(|r| r.flatten().collect()).unwrap_or_default(); v.sort_by_key(|e| e.file_name()); v }

fn scan_ps3(c: &Connection, platform: &str, dir: &Path, now: i64) -> Result<Vec<(i64, String)>, String> {
    let (mut existing, black) = known_ids(c, platform)?; let mut added = vec![];
    for outer in sorted_entries(dir) {
        if !outer.path().is_dir() { continue; }
        // the dump itself, or one folder deeper (Vimm's Lair wraps it in an extra folder)
        let game = if is_ps3_dir(&outer.path()) { Some(outer.path()) } else { sorted_entries(&outer.path()).into_iter().map(|i| i.path()).find(|p| p.is_dir() && is_ps3_dir(p)) };
        let Some(game) = game else { continue };
        let gs = game.to_string_lossy().to_string();
        if existing.contains(&gs) || black.contains(&gs) { continue; }
        let name = clean_name(&outer.file_name().to_string_lossy());
        let appid = insert(c, platform, &name, &gs, now)?; existing.insert(gs); added.push((appid, name));
    }
    Ok(added)
}
fn scan_vita3k(c: &Connection, platform: &str, dir: &Path, now: i64) -> Result<Vec<(i64, String)>, String> {
    let app = if dir.join("app").is_dir() { dir.join("app") } else { dir.to_path_buf() };
    let (mut existing, black) = known_ids(c, platform)?; let re = Regex::new(r"^[A-Z]{4}\d{5}$").unwrap(); let mut added = vec![];
    for e in sorted_entries(&app) {
        let tid = e.file_name().to_string_lossy().to_string();
        if !e.path().is_dir() || !re.is_match(&tid) || existing.contains(&tid) || black.contains(&tid) { continue; }
        let name = read_sfo_title(&e.path().join("sce_sys/param.sfo")).unwrap_or_else(|| tid.clone());
        let appid = insert(c, platform, &name, &tid, now)?; existing.insert(tid); added.push((appid, name));
    }
    Ok(added)
}
fn scan_files(root: &Path, c: &Connection, id: &str, platform: &str, dir: &Path, now: i64) -> Result<Vec<(i64, String)>, String> {
    let exts = extensions_for(root, id, platform);
    if exts.is_empty() { return Ok(vec![]); }
    let (mut existing, black) = known_ids(c, platform)?;
    let mame = match id { "mame" => binary(&get_entry(root, "mame").unwrap_or(Value::Null)), "fbneo" => mame_binary(root), _ => String::new() };
    let mut added = vec![];
    for e in sorted_entries(dir) {
        let fname = e.file_name().to_string_lossy().to_string();
        if !exts.contains(&ext_lower(&fname)) { continue; }
        let path = dir.join(&fname).to_string_lossy().to_string();
        if existing.contains(&path) || black.contains(&path) { continue; }
        let machine = stem(&fname).to_string();
        let name = if matches!(id, "mame" | "fbneo") && !mame.is_empty() { mame_lookup_name(&mame, &machine).unwrap_or(machine) } else { clean_name(&fname) };
        let appid = insert(c, platform, &name, &path, now)?; existing.insert(path); added.push((appid, name));
    }
    Ok(added)
}
/// Scan one folder with this emulator's rules; returns the games added as (appid, name).
pub fn scan_dir(root: &Path, c: &Connection, id: &str, platform: &str, dir: &Path, now: i64) -> Result<Vec<(i64, String)>, String> {
    match known(id).and_then(|k| k.scan_mode.as_deref()) {
        Some("ps3_folder") => scan_ps3(c, platform, dir, now),
        Some("vita3k_app") => scan_vita3k(c, platform, dir, now),
        _ => scan_files(root, c, id, platform, dir, now),
    }
}
/// Scan one system of one emulator (`platform` Some) or everything enabled (None). `progress` gets a line per folder.
pub fn scan(root: &Path, c: &Connection, only: Option<(&str, &str)>, now: i64, progress: &dyn Fn(&str)) -> Result<Vec<(i64, String, String)>, String> {
    let mut added = vec![];
    let list = load(root);
    let jobs: Vec<(String, String)> = match only {
        Some((id, p)) => {
            let e = list.iter().find(|e| entry_id(e) == id).ok_or(format!("Emulator {id} is not set up."))?;
            if !platform_dirs(e, p).iter().any(|d| Path::new(d).is_dir()) { return Err(format!("No valid ROM folder is set for {}.", platform_label(p))); }
            vec![(id.to_string(), p.to_string())]
        }
        None => list.iter().filter(|e| enabled(e)).flat_map(|e| entry_platforms_all(e).into_iter().map(move |p| (entry_id(e).to_string(), p))).collect(),
    };
    for (id, p) in jobs {
        let Some(e) = list.iter().find(|e| entry_id(e) == id) else { continue };
        for d in platform_dirs(e, &p) {
            let dir = PathBuf::from(&d); if !dir.is_dir() { continue; }
            progress(&format!("Scanning {}...", platform_label(&p)));
            for (a, n) in scan_dir(root, c, &id, &p, &dir, now)? { added.push((a, n, p.clone())); }
        }
    }
    Ok(added)
}
/// Every key of the entry's own `platforms` map (the scan-all pass walks what is configured, not what the emulator could cover).
fn entry_platforms_all(e: &Value) -> Vec<String> { e.get("platforms").and_then(Value::as_object).map(|o| o.keys().cloned().collect()).unwrap_or_default() }

/// Re-check `installed` for every emulated game: the ROM file must still be there. Returns how many rows changed.
pub fn sync_install_status(c: &Connection) -> Result<usize, String> {
    let ph = vec!["?"; platform_names().len()].join(",");
    let ids: Vec<&str> = platform_names().iter().map(|(p, _)| p.as_str()).collect();
    let rows: Vec<(i64, String, i64)> = c.prepare(&format!("SELECT appid, COALESCE(platform_id, ''), COALESCE(installed, 0) FROM games WHERE platform IN ({ph})"))
        .and_then(|mut s| s.query_map(rusqlite::params_from_iter(ids.iter()), |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map(|r| r.flatten().collect())).map_err(|e| e.to_string())?;
    let mut changed = 0;
    for (appid, pid, inst) in rows {
        // PlayDate checks a file; a PS3 dump or a Vita3K title is not one, so those would always read "not installed" there. They are kept as they are
        // unless they are a path (a PS3 folder) that is gone.
        let now = if pid.is_empty() { 0 } else if Path::new(&pid).is_file() { 1 } else if Path::new(&pid).is_dir() { 1 } else if pid.contains(std::path::MAIN_SEPARATOR) { 0 } else { inst };
        if now != inst { c.execute("UPDATE games SET installed = ? WHERE appid = ?", params![now, appid]).map_err(|e| e.to_string())?; changed += 1; }
    }
    Ok(changed)
}

// ---------------------------------------------------------------- launching
/// How to start an emulated game: the first enabled emulator that covers the system, its program and arguments with `{rom}`, `{core}`, `{rom_dir}` and
/// `{rom_name}` filled in. `Err` carries what to tell the user.
pub fn launch(root: &Path, platform: &str, rom: &str) -> Result<Action, String> {
    let list = load(root);
    let e = list.iter().find(|e| enabled(e) && e.get("platforms").and_then(|p| p.get(platform)).is_some())
        .ok_or_else(|| format!("No emulator is set up for {}.", platform_label(platform)))?;
    let mode = known(entry_id(e)).and_then(|k| k.scan_mode.as_deref());
    match mode {
        Some("ps3_folder") => if rom.is_empty() || !Path::new(rom).is_dir() { return Err("The game folder was not found.".into()); },
        Some("vita3k_app") => if rom.is_empty() { return Err("The title id is not set.".into()); },
        _ => if rom.is_empty() || !Path::new(rom).is_file() { return Err("The ROM file was not found.".into()); },
    }
    let b = binary(e);
    let program = if Path::new(&b).is_file() { b.clone() } else { which(&b).map(|p| p.to_string_lossy().into()).unwrap_or_default() };
    if program.is_empty() { return Err(format!("The emulator program was not found: {b}")); }
    let raw = args(e);
    let core = if raw.iter().any(|a| a.contains("{core}")) {
        let c = stored_core(e, platform);
        if c.is_empty() || !Path::new(&c).is_file() { return Err(format!("No core is chosen for {}. Pick one in Emulators.", platform_label(platform))); }
        c
    } else { String::new() };
    let has_sep = rom.contains(std::path::MAIN_SEPARATOR);
    let rom_dir = if has_sep { Path::new(rom).parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default() } else { String::new() };
    let rom_name = if has_sep { stem(Path::new(rom).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default().as_str()).to_string() } else { rom.to_string() };
    let argv: Vec<String> = raw.iter().map(|a| a.replace("{rom}", rom).replace("{core}", &core).replace("{rom_dir}", &rom_dir).replace("{rom_name}", &rom_name)).collect();
    if let Some(app) = flatpak_app_id(&program) {
        if mode != Some("vita3k_app") && !flatpak_path_covered(&flatpak_grants(&app), rom) {
            let dir = Path::new(rom).parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
            return Err(format!("{app} cannot reach this file. Run: flatpak override --user --filesystem={dir} {app}"));
        }
    }
    Ok(Action::Spawn { program, args: argv, dir: None, env: vec![] })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tmp(name: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("zest-emu-{name}-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d }
    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE games (appid INTEGER PRIMARY KEY, name TEXT, platform TEXT, platform_id TEXT, date_added INTEGER, completion_status TEXT, installed INTEGER, art_fetched TEXT,
            meta_fetched TEXT, cheevos_fetched TEXT, protondb_fetched TEXT, hltb_fetched TEXT); CREATE TABLE blacklist (appid INTEGER, platform_id TEXT);").unwrap(); c
    }
    #[test] fn the_exported_data_matches_python() {
        assert_eq!(platform_names().len(), 34); assert_eq!(known_list().len(), 50);
        assert_eq!(known("retroarch").unwrap().args, vec!["-L", "{core}", "{rom}"]); assert_eq!(known("rpcs3").unwrap().scan_mode.as_deref(), Some("ps3_folder"));
        assert!(platform_names().iter().all(|(p, _)| crate::sources::is_emulated_system(p)) && crate::sources::EMULATED_PLATFORMS.len() == 34, "the sources list and the exported list name the same systems");
    }
    #[test] fn names_are_cleaned_like_python() {
        // expected values produced by emulators._clean_name
        let g: Vec<(String, String)> = serde_json::from_str(include_str!("../tests/golden/emulator_names.json")).unwrap();
        assert!(g.len() > 20);
        for (i, o) in g { assert_eq!(clean_name(&i), o, "{i}"); }
    }
    #[test] fn stem_follows_splitext() { assert_eq!(stem("a.b.iso"), "a.b"); assert_eq!(stem(".hidden"), ".hidden"); assert_eq!(stem("noext"), "noext"); assert_eq!(stem("/x/y.z/f"), "/x/y.z/f"); assert_eq!(ext_lower("Game.SFC"), ".sfc"); }
    #[test] fn sfo_title_is_read() {
        let mut d = b"\0PSF".to_vec(); d.extend([1, 1, 0, 0]);
        let (lab, dat) = (20u32 + 16, 20u32 + 16 + 6); d.extend(lab.to_le_bytes()); d.extend(dat.to_le_bytes()); d.extend(1u32.to_le_bytes());
        d.extend(0u16.to_le_bytes()); d.extend([2, 4]); d.extend(8u32.to_le_bytes()); d.extend(8u32.to_le_bytes()); d.extend(0u32.to_le_bytes());
        d.extend(b"TITLE\0"); d.extend(b"My Game\0"); assert_eq!(sfo_title(&d).as_deref(), Some("My Game")); assert_eq!(sfo_title(b"junk"), None);
    }
    #[test] fn add_edit_and_remove_keep_the_python_file_shape() {
        let r = tmp("cfg"); let e = add_emulator(&r, "dolphin").unwrap();
        assert_eq!(e["platforms"]["gamecube"], json!([""])); assert_eq!(e["args"], json!(["{rom}"])); assert!(add_emulator(&r, "dolphin").is_err()); assert!(add_emulator(&r, "nope").is_err());
        update_emulator(&r, "dolphin", &Update { binary: Some("/bin/true".into()), platform_dirs: Some([("wii".to_string(), vec!["/a".to_string()])].into()), enabled: Some(false), ..Default::default() }).unwrap();
        let e = get_entry(&r, "dolphin").unwrap(); assert_eq!(e["platforms"]["wii"], json!("/a")); assert_eq!(binary(&e), "/bin/true"); assert!(!enabled(&e));
        let c = add_custom_emulator(&r, "My Snes", "snes", "", &[]).unwrap(); assert_eq!(c["id"], "custom_1"); assert!(c["extensions"]["snes"].as_array().unwrap().len() > 0);
        assert_eq!(add_custom_emulator(&r, "Other", "snes", "", &[]).unwrap()["id"], "custom_2");
        remove_emulator(&r, "dolphin").unwrap(); assert!(get_entry(&r, "dolphin").is_none()); let _ = std::fs::remove_dir_all(&r);
    }
    #[test] fn a_scan_adds_roms_once_skips_the_blacklist_and_other_extensions() {
        let r = tmp("scan"); let roms = r.join("roms"); std::fs::create_dir_all(&roms).unwrap();
        for f in ["Super Game (USA) [!].sfc", "Other (Rev 1).smc", "readme.txt", "Black.sfc"] { std::fs::write(roms.join(f), b"x").unwrap(); }
        add_custom_emulator(&r, "Snes9x", "snes", "/bin/true", &[]).unwrap();
        update_emulator(&r, "custom_1", &Update { platform_dirs: Some([("snes".to_string(), vec![roms.to_string_lossy().to_string()])].into()), ..Default::default() }).unwrap();
        let c = db(); c.execute("INSERT INTO blacklist VALUES (NULL, ?)", [roms.join("Black.sfc").to_string_lossy().to_string()]).unwrap();
        let a = scan(&r, &c, Some(("custom_1", "snes")), 5, &|_| {}).unwrap();
        let mut names: Vec<_> = a.iter().map(|x| x.1.clone()).collect(); names.sort(); assert_eq!(names, vec!["Other", "Super Game"]);
        assert!(a.iter().all(|x| x.0 < 0)); assert!(scan(&r, &c, None, 6, &|_| {}).unwrap().is_empty(), "a second pass adds nothing");
        assert_eq!(c.query_row("SELECT COUNT(*) FROM games WHERE platform='snes' AND installed=1", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
        std::fs::remove_file(roms.join("Other (Rev 1).smc")).unwrap();
        assert_eq!(sync_install_status(&c).unwrap(), 1); let _ = std::fs::remove_dir_all(&r);
    }
    #[test] fn ps3_dumps_are_found_one_level_down_and_vita_titles_are_named_from_the_sfo() {
        let r = tmp("ps3"); let g = r.join("Some Game [BLUS12345]/inner"); std::fs::create_dir_all(g.join("PS3_GAME")).unwrap(); std::fs::create_dir_all(r.join("empty")).unwrap();
        let c = db(); let a = scan_ps3(&c, "ps3", &r, 1).unwrap(); assert_eq!(a.len(), 1); assert_eq!(a[0].1, "Some Game"); assert_eq!(scan_ps3(&c, "ps3", &r, 1).unwrap().len(), 0);
        let v = tmp("vita"); std::fs::create_dir_all(v.join("app/PCSA00001/sce_sys")).unwrap(); std::fs::create_dir_all(v.join("app/notatitle")).unwrap();
        let a = scan_vita3k(&c, "psvita", &v, 1).unwrap(); assert_eq!(a, vec![(a[0].0, "PCSA00001".to_string())]); let _ = std::fs::remove_dir_all(&r); let _ = std::fs::remove_dir_all(&v);
    }
    #[test] fn launch_fills_the_placeholders_and_explains_what_is_missing() {
        let r = tmp("launch"); let rom = r.join("g.sfc"); std::fs::write(&rom, b"x").unwrap(); let rs = rom.to_string_lossy().to_string();
        assert!(launch(&r, "snes", &rs).unwrap_err().contains("No emulator"));
        add_custom_emulator(&r, "Mine", "snes", "/bin/true", &["--r".into(), "{rom}".into(), "{rom_dir}".into(), "{rom_name}".into()]).unwrap();
        match launch(&r, "snes", &rs).unwrap() { Action::Spawn { program, args, .. } => { assert_eq!(program, "/bin/true"); assert_eq!(args[1], rs); assert_eq!(args[2], r.to_string_lossy()); assert_eq!(args[3], "g"); } _ => panic!() }
        assert!(launch(&r, "snes", "/nope.sfc").unwrap_err().contains("ROM"));
        update_emulator(&r, "custom_1", &Update { args: Some(vec!["-L".into(), "{core}".into(), "{rom}".into()]), ..Default::default() }).unwrap();
        assert!(launch(&r, "snes", &rs).unwrap_err().contains("core")); let _ = std::fs::remove_dir_all(&r);
    }
    #[test] fn cores_are_listed_in_preference_order() {
        let d = tmp("cores"); let want = known("retroarch").unwrap().cores_search.get("snes").unwrap().clone();
        for n in want.iter().rev().take(2) { std::fs::write(d.join(format!("{n}.so")), b"").unwrap(); }
        let f = list_cores_in("snes", &[d.clone()]); assert_eq!(f.len(), 2); assert_eq!(f[0].0, want[want.len() - 2], "the more preferred core comes first"); let _ = std::fs::remove_dir_all(&d);
    }
}
