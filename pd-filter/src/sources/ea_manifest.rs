//! Finds an installed EA game inside the Wine prefix the EA App runs in (port of PlayDate's `plugins/ea_app/manifest.py`). Used only to tell whether a game is installed
//! and where: install, launch and uninstall just open the EA App. Each install writes `Software\<Company>\<Title>\Install Dir` (or a Wow6432Node variant) into the prefix's
//! registry; the folder it names holds `__Installer/installerdata.xml`, which lists the game's content ids and its title. The company is not always "EA Games"
//! (SteamWorld Dig registers under its developer), so the scan accepts any company and the manifest check is the real filter.
//! Checked against the real Python by `tests/gen_ea_manifest_golden.py` on a fake prefix.
use regex::Regex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The prefix's registry as Python's dict saw it: lower-cased `section\value` keys in first-seen order, a later definition replacing the value in place.
#[derive(Default, Debug)]
pub struct Registry { items: Vec<(String, String)>, index: HashMap<String, usize> }

impl Registry {
    fn put(&mut self, k: String, v: String) {
        match self.index.get(&k) { Some(&i) => self.items[i].1 = v, None => { self.index.insert(k.clone(), self.items.len()); self.items.push((k, v)); } }
    }
    pub fn get(&self, k: &str) -> Option<&str> { self.index.get(k).map(|&i| self.items[i].1.as_str()) }
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> { self.items.iter().map(|(k, v)| (k.as_str(), v.as_str())) }
    pub fn len(&self) -> usize { self.items.len() }
    pub fn is_empty(&self) -> bool { self.items.is_empty() }
}

/// `_unescape_reg`: `\"` is a quote, `\xHHHH` a character (the (tm) sign in "Battlefield(tm) V"), `\\` one backslash.
pub fn unescape_reg(s: &str) -> String {
    static U: OnceLock<Regex> = OnceLock::new();
    let s = s.replace("\\\"", "\"");
    let s = U.get_or_init(|| Regex::new(r"\\x([0-9a-fA-F]{4})").unwrap()).replace_all(&s, |c: &regex::Captures| {
        char::from_u32(u32::from_str_radix(&c[1], 16).unwrap_or(0xFFFD)).unwrap_or('\u{FFFD}').to_string()
    }).into_owned();
    s.replace("\\\\", "\\")
}

fn read_reg_file(path: &Path, out: &mut Registry) {
    static H: OnceLock<Regex> = OnceLock::new(); static V: OnceLock<Regex> = OnceLock::new();
    let Ok(bytes) = std::fs::read(path) else { return };
    let text = String::from_utf8_lossy(&bytes);
    let (h, v) = (H.get_or_init(|| Regex::new(r"^\[(.+?)\]").unwrap()), V.get_or_init(|| Regex::new(r#"^"((?:[^"\\]|\\.)*)"=(.*)$"#).unwrap()));
    let mut section = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') { continue; }
        if let Some(m) = h.captures(line) { section = unescape_reg(&m[1]); continue; }
        if let Some(m) = v.captures(line) {
            if section.is_empty() { continue; }
            let key = unescape_reg(&m[1]);
            let mut raw = m[2].trim().to_string();
            if raw.len() >= 2 && raw.starts_with('"') && raw.ends_with('"') { raw = unescape_reg(&raw[1..raw.len() - 1]); }
            out.put(format!("{section}\\{key}").replace("\\\\", "\\").to_lowercase(), raw);
        }
    }
}

/// `_load_prefix_registry`: `system.reg`, then `user.reg`.
pub fn load_registry(prefix: &Path) -> Registry {
    let mut r = Registry::default();
    read_reg_file(&prefix.join("system.reg"), &mut r);
    read_reg_file(&prefix.join("user.reg"), &mut r);
    r
}

fn normalize_reg_key(key: &str) -> String {
    let k = key.to_lowercase();
    for p in ["hkey_local_machine\\", "hkey_current_user\\"] { if let Some(rest) = k.strip_prefix(p) { return rest.to_string(); } }
    k
}

/// `_read_reg_value`: `Sub\Key\ValueName` (HKLM / HKCU prefix optional), also looked up under Wow6432Node.
pub fn read_reg_value(reg: &Registry, key: &str) -> Option<String> {
    let n = normalize_reg_key(key);
    if let Some(v) = reg.get(&n) { return Some(v.to_string()); }
    reg.get(&n.replacen("software\\", "software\\wow6432node\\", 1)).map(String::from)
}

/// `_case_insensitive_join`: Wine's disk is case-sensitive but manifests use Windows casing.
pub fn case_insensitive_join(base: &Path, rel: &str) -> PathBuf {
    let mut cur = base.to_path_buf();
    for part in rel.split('/').filter(|p| !p.is_empty()) {
        let cand = cur.join(part);
        if cand.exists() { cur = cand; continue; }
        let found = std::fs::read_dir(&cur).ok().and_then(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().to_string()).find(|n| n.to_lowercase() == part.to_lowercase()));
        cur = cur.join(found.as_deref().unwrap_or(part));
    }
    cur
}

/// `_resolve_windows_path`: a drive letter goes through the prefix's `dosdevices` link (a game installed outside the prefix lives on Z:, the host's root), without one the path is under `drive_c`.
pub fn resolve_windows_path(prefix: &Path, windows_path: &str) -> String {
    static D: OnceLock<Regex> = OnceLock::new();
    let Some(m) = D.get_or_init(|| Regex::new(r"^([A-Za-z]):/(.*)$").unwrap()).captures(windows_path) else {
        return case_insensitive_join(&prefix.join("drive_c"), windows_path).to_string_lossy().to_string();
    };
    let drive = m[1].to_lowercase();
    let root = std::fs::canonicalize(prefix.join("dosdevices").join(format!("{drive}:"))).ok().filter(|p| p.is_dir())
        .unwrap_or_else(|| if drive == "c" { prefix.join("drive_c") } else { PathBuf::from("/") });
    case_insensitive_join(&root, &m[2]).to_string_lossy().to_string()
}

/// `_resolve_registry_path`: a literal Windows path, or `[Sub\Key\ValueName]suffix` whose bracketed registry value is looked up first. `None` when that value does not exist.
pub fn resolve_registry_path(prefix: &Path, reg: &Registry, expr: &str) -> Option<String> {
    let win = if expr.contains('[') && expr.contains(']') {
        let (key_part, suffix) = expr.split_once(']').unwrap_or((expr, ""));
        let value = read_reg_value(reg, key_part.trim_start_matches('['))?;
        let base = value.replace('\\', "/").trim_end_matches('/').to_string();
        let suffix = suffix.replace('\\', "/").trim_start_matches('/').to_string();
        if suffix.is_empty() { base } else { format!("{base}/{suffix}") }
    } else { expr.replace('\\', "/") };
    Some(resolve_windows_path(prefix, &win))
}

/// `_iter_ea_games_install_dirs`: every `Software\<Company>\<Title>\Install Dir` (or Wow6432Node) that names a folder that exists, in registry order.
pub fn install_dirs(prefix: &Path, reg: &Registry) -> Vec<String> {
    static K: OnceLock<Regex> = OnceLock::new();
    let k = K.get_or_init(|| Regex::new(r"^software\\(?:wow6432node\\)?[^\\]+\\[^\\]+\\install dir$").unwrap());
    reg.iter().filter(|(key, value)| k.is_match(key) && !value.is_empty())
        .map(|(_, value)| resolve_windows_path(prefix, &value.replace('\\', "/"))).filter(|p| Path::new(p).is_dir()).collect()
}

/// The text of the elements called `name` (no namespace) in an `installerdata.xml`, each stripped, like Python's `{el.text.strip() for el in root.iter(name) if el.text}`
/// (an element's text is what comes before its first child). `None` when the file is missing or not well-formed XML.
fn manifest_texts(path: &Path, names: &[&str]) -> Option<Vec<String>> {
    let bytes = std::fs::read(path).ok()?;
    let text: String = if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        let le = bytes[0] == 0xFF;
        let u: Vec<u16> = bytes[2..].chunks(2).filter(|c| c.len() == 2).map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) }).collect();
        String::from_utf16_lossy(&u)
    } else { String::from_utf8_lossy(bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes)).into_owned() };
    let doc = roxmltree::Document::parse(&text).ok()?;
    Some(doc.descendants().filter(|n| n.is_element() && n.tag_name().namespace().is_none() && names.contains(&n.tag_name().name()))
        .filter_map(|n| n.text()).filter(|t| !t.is_empty()).map(|t| t.trim().to_string()).collect())
}

fn manifest_path(install_dir: &str) -> PathBuf { case_insensitive_join(Path::new(install_dir), "__Installer/installerdata.xml") }

/// `find_manifest_path_by_content_id`: (install folder, manifest) of the install whose manifest lists this content id (exact match, never by title).
pub fn find_by_content_id(prefix: &Path, reg: &Registry, content_id: &str) -> Option<(String, String)> {
    if content_id.is_empty() { return None; }
    for dir in install_dirs(prefix, reg) {
        let mp = manifest_path(&dir);
        if !mp.is_file() { continue; }
        let Some(ids) = manifest_texts(&mp, &["contentID"]) else { continue };
        if ids.iter().any(|i| i == content_id) { return Some((dir, mp.to_string_lossy().to_string())); }
    }
    None
}

/// `_normalize_title`: punctuation removed, white space folded, lower case ("Bejeweled(R) 3" and "Bejeweled 3" meet).
pub fn normalize_title(name: &str) -> String {
    static T: OnceLock<Regex> = OnceLock::new(); static W: OnceLock<Regex> = OnceLock::new();
    let s = T.get_or_init(|| Regex::new(r"[^\w\s]").unwrap()).replace_all(name, "");
    W.get_or_init(|| Regex::new(r"\s+").unwrap()).replace_all(&s, " ").trim().to_lowercase()
}

/// `find_install_dir_by_name`: the install whose manifest `<title>` or `<gameTitle>` equals the game's name once normalised. Purely local; used to mark a game installed, never to pick what runs.
pub fn find_by_name(prefix: &Path, reg: &Registry, game_name: &str) -> Option<String> {
    let target = normalize_title(game_name);
    if target.is_empty() { return None; }
    for dir in install_dirs(prefix, reg) {
        let mp = manifest_path(&dir);
        if !mp.is_file() { continue; }
        let Some(titles) = manifest_texts(&mp, &["title", "gameTitle"]) else { continue };
        if titles.iter().any(|t| normalize_title(t) == target) { return Some(dir); }
    }
    None
}

/// `_install_present` (watcher.py): whether a game is really installed at `install_path`. EA's Cleanup.exe leaves the top folder behind as an empty shell, so the folder alone proves nothing:
/// the recorded program still existing, or `__Installer/installerdata.xml` still existing, does.
pub fn install_present(install_path: &str, exe_rel: Option<&str>) -> bool {
    let dir = Path::new(install_path);
    if install_path.is_empty() || !dir.is_dir() { return false; }
    if let Some(e) = exe_rel.filter(|e| !e.is_empty()) { if dir.join(e).is_file() { return true; } }
    manifest_path(install_path).is_file()
}

/// The EA test prefix (`tests/data/ea_prefix`). Its `dosdevices/c:` and `e:` links are made here, not kept in git: a name with a colon cannot be checked out on Windows.
/// Every test that uses the fixture must get it through this.
#[cfg(all(test, unix))]
pub(crate) fn fixture_prefix() -> PathBuf {
    let p = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/ea_prefix")).to_path_buf();
    for d in ["c", "e"] { let l = p.join("dosdevices").join(format!("{d}:")); if std::fs::symlink_metadata(&l).is_err() { let _ = std::fs::create_dir_all(p.join("dosdevices")); let _ = std::os::unix::fs::symlink(format!("../drive_{d}"), &l); } }
    p
}

#[cfg(all(test, unix))]   // Wine prefixes are a Linux thing, and the fixture's drive links are Unix symlinks
mod tests {
    use super::*;
    use serde_json::Value;
    fn here() -> PathBuf { Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests")).to_path_buf() }
    fn golden() -> Value { serde_json::from_str(&std::fs::read_to_string(here().join("ea_manifest_golden.json")).unwrap()).unwrap() }
    fn root() -> PathBuf { std::fs::canonicalize(fixture_prefix()).unwrap() }
    fn put(s: &str) -> String { s.replace("<ROOT>", &root().to_string_lossy()) }
    fn want(v: &Value) -> Option<String> { v.as_str().map(put) }

    #[test] fn the_registry_and_install_folders_agree_with_the_real_python() {
        let g = golden(); let reg = load_registry(&root());
        let w: Vec<(String, String)> = g["registry"].as_array().unwrap().iter().map(|p| (p[0].as_str().unwrap().to_string(), p[1].as_str().unwrap().to_string())).collect();
        assert_eq!(reg.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<Vec<_>>(), w, "keys, values and order");
        assert_eq!(install_dirs(&root(), &reg), g["dirs"].as_array().unwrap().iter().map(|v| put(v.as_str().unwrap())).collect::<Vec<_>>());
    }
    #[test] fn content_ids_and_titles_find_the_same_installs_as_python() {
        let g = golden(); let reg = load_registry(&root());
        for (id, w) in g["by_content_id"].as_object().unwrap() {
            let got = find_by_content_id(&root(), &reg, id);
            assert_eq!(got, w.as_array().map(|a| (put(a[0].as_str().unwrap()), put(a[1].as_str().unwrap()))), "content id {id:?}");
        }
        for (name, w) in g["by_name"].as_object().unwrap() { assert_eq!(find_by_name(&root(), &reg, name), want(w), "name {name:?}"); }
    }
    #[test] fn paths_and_expressions_resolve_like_python() {
        let g = golden(); let reg = load_registry(&root());
        for (expr, w) in g["expr"].as_object().unwrap() { assert_eq!(resolve_registry_path(&root(), &reg, expr), want(w), "{expr}"); }
        for (t, w) in g["norm"].as_object().unwrap() { if t != "<none>" { assert_eq!(normalize_title(t), w.as_str().unwrap(), "{t:?}"); } }
    }
    #[test] fn a_gutted_install_is_not_installed() {
        let reg = load_registry(&root()); let dirs = install_dirs(&root(), &reg);
        let bej = dirs.iter().find(|d| d.ends_with("Bejeweled 3")).unwrap();
        assert!(install_present(bej, None) && install_present(bej, Some("nope.exe")) && install_present(bej, Some("")), "a missing program falls back to the manifest");
        let hospital = dirs.iter().find(|d| d.ends_with("Theme Hospital")).unwrap();
        assert!(!install_present(hospital, None), "the shell Cleanup.exe leaves has no installerdata.xml");
        assert!(!install_present("", None) && !install_present("/no/such/folder", None));
        assert!(install_present(hospital, Some("game.ico")), "a recorded program that still exists counts");
    }
}
