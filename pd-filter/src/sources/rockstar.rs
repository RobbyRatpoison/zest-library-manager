//! Rockstar Games from the Rockstar Games Launcher's own files (PlayDate's `plugins/rockstar`, local sync): no account, no network for the library. The "sign-in" is the
//! Wine prefix (on Linux) the launcher is installed in. A sync reads the launcher's rotated logs for `[mergedmanifest] Using ... for <titleId>` lines (the launcher only
//! does that for a title the signed-in account owns), names the titles from Rockstar's public catalog and records them. Install, launch and uninstall go through the
//! launcher's own command line, as in PlayDate. Not built: the Social Club token sync (PlayDate's guessed endpoint, never confirmed), art (SteamGridDB covers it).
use super::{Action, AuthBegin, AuthStatus, Caps, GameRef, InstalledRow, Launch, LibrarySource, SourceCtx};
use crate::importer::GameRecord;
use crate::net::{Cancel, Http};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const TITLE_METADATA_URL: &str = "https://gamedownloads-rockstargames-com.akamaized.net/public/title_metadata.json";

/// Names for a title id the public catalog does not list (PlayDate's last-resort table).
const FALLBACK_NAMES: &[(&str, &str)] = &[
    ("3", "Grand Theft Auto III \u{2013} The Definitive Edition"), ("7", "Grand Theft Auto: Vice City \u{2013} The Definitive Edition"),
    ("11", "Grand Theft Auto: San Andreas \u{2013} The Definitive Edition"), ("13", "Grand Theft Auto IV"), ("16", "Max Payne"),
    ("17", "Max Payne 2: The Fall of Max Payne"), ("18", "Grand Theft Auto V"), ("24", "Max Payne 3"), ("26", "Midnight Club: Los Angeles"),
    ("29", "GTA Online"), ("31", "Grand Theft Auto: San Andreas"), ("35", "L.A. Noire"), ("44", "Bully: Scholarship Edition"), ("45", "Manhunt"),
    ("51", "Grand Theft Auto: The Trilogy \u{2013} The Definitive Edition"), ("56", "Red Dead Online"), ("57", "Red Dead Redemption 2"), ("58", "Grand Theft Auto VI"),
];

/// What the catalog says about one title.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Title { pub name: String, pub exe: String }

pub struct RockstarSource { http: Arc<dyn Http>, config_dir: PathBuf, db: PathBuf, catalog: Mutex<Option<HashMap<String, Title>>> }

// ---------------------------------------------------------------- pure parts

/// The title ids in one launcher log's text: `[mergedmanifest] Using build collection|regular build for <id>`, the launcher itself left out.
pub fn owned_in_log(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in text.lines() {
        let mut from = 0;
        while let Some(at) = line[from..].find("[mergedmanifest]") {
            let after = &line[from + at + "[mergedmanifest]".len()..];
            from += at + "[mergedmanifest]".len();
            let rest = after.trim_start();
            if rest.len() == after.len() { continue; }   // the pattern needs whitespace after the tag
            let Some(rest) = rest.strip_prefix("Using ") else { continue };
            let Some(rest) = rest.strip_prefix("build collection for ").or_else(|| rest.strip_prefix("regular build for ")) else { continue };
            let id: String = rest.chars().take_while(|c| !c.is_whitespace()).collect();
            if !id.is_empty() && id != "launcher" { out.insert(id); }
            break;   // a regex search takes the first match on the line
        }
    }
    out
}

/// Every `launcher*.log` in the folder (the launcher rotates `launcher.log`, `launcher.01.log`, ...).
pub fn owned_in_dir(dir: &Path) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !name.starts_with("launcher") || !name.ends_with(".log") { continue; }
        if let Ok(bytes) = std::fs::read(e.path()) { out.extend(owned_in_log(&String::from_utf8_lossy(&bytes))); }
    }
    out
}

/// A name as a store-page slug: lower case, runs of anything else become one dash.
pub fn slugify(name: &str) -> String {
    let mut s = String::new();
    let mut dash = false;
    for c in name.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() { s.push(c); dash = false; } else if !dash && !s.is_empty() { s.push('-'); dash = true; }
    }
    s.trim_matches('-').to_string()
}

/// The prefix's Windows user folder: Proton's `steamuser`, else the Unix user name, else the one real profile.
pub fn wine_user_dir(prefix: &Path, unix_user: &str) -> Option<PathBuf> {
    let users = prefix.join("drive_c/users");
    for c in ["steamuser", unix_user] { if !c.is_empty() && users.join(c).is_dir() { return Some(users.join(c)); } }
    let mut names: Vec<String> = std::fs::read_dir(&users).ok()?.flatten().filter(|e| e.path().is_dir()).map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| !["Public", "Default User", "Default", "All Users"].contains(&n.as_str())).collect();
    names.sort();
    names.first().map(|n| users.join(n))
}

pub fn log_dir(prefix: &Path) -> Option<PathBuf> {
    let d = wine_user_dir(prefix, &std::env::var("USER").unwrap_or_default())?.join("Documents/Rockstar Games/Launcher");
    d.is_dir().then_some(d)
}

/// The catalog file's titles by title id.
pub fn parse_catalog(json: &str) -> HashMap<String, Title> {
    let v: Value = serde_json::from_str(json).unwrap_or(Value::Null);
    let mut out = HashMap::new();
    if let Some(t) = v.get("titles").and_then(|t| t.as_object()) {
        for x in t.values() {
            let Some(id) = x.get("titleId").and_then(|i| i.as_str()).filter(|i| !i.is_empty()) else { continue };
            out.insert(id.to_string(), Title { name: x.get("friendlyName").and_then(|n| n.as_str()).unwrap_or("").to_string(), exe: x.get("principalFileName").and_then(|n| n.as_str()).unwrap_or("").to_string() });
        }
    }
    out
}

/// A game's program inside the prefix's Program Files folders. The catalog's file name may be a sub-path (`Gameface\Binaries\Win64\X.exe`), so only its last part is matched.
pub fn find_game_exe(prefix: &Path, exe_name: &str) -> Option<PathBuf> {
    let base = exe_name.replace('\\', "/").rsplit('/').next().unwrap_or("").to_string();
    if base.is_empty() { return None; }
    ["Program Files", "Program Files (x86)"].iter().find_map(|pf| find_file(&prefix.join("drive_c").join(pf), &base, 12))
}

pub(super) fn find_file(dir: &Path, name: &str, depth: u32) -> Option<PathBuf> {
    let mut subdirs = vec![];
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() { subdirs.push(p); } else if e.file_name().to_string_lossy() == name { return Some(p); }
    }
    if depth == 0 { return None; }
    subdirs.sort();
    subdirs.iter().find_map(|d| find_file(d, name, depth - 1))
}

pub fn find_launcher(prefix: &Path) -> Option<PathBuf> { find_file(&prefix.join("drive_c"), "Launcher.exe", 8) }

/// A scan result as a library record.
pub fn record(id: &str, t: Option<&Title>) -> GameRecord {
    let name = t.map(|t| t.name.clone()).filter(|n| !n.is_empty()).or_else(|| FALLBACK_NAMES.iter().find(|(i, _)| *i == id).map(|(_, n)| n.to_string())).unwrap_or_else(|| id.to_string());
    GameRecord { platform_id: id.to_string(), slug: Some(slugify(&name)), name, ..Default::default() }
}

/// The Wine prefix a launcher-based source was set up with: `config.json[id]["prefix"]`, else PlayDate's launcher setting `launchers.<id>.prefix`. It must exist.
pub fn prefix_setting(config_dir: &Path, id: &str) -> Option<PathBuf> {
    let own = crate::account::plugin_secret_get(config_dir, id, "prefix").filter(|p| !p.is_empty());
    let legacy = || std::fs::read_to_string(config_dir.join("config.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v["launchers"][id]["prefix"].as_str().map(|s| s.trim().to_string())).filter(|p| !p.is_empty());
    own.or_else(legacy).map(PathBuf::from).filter(|p| p.is_dir())
}

/// A pasted prefix folder checked: absolute (or `~/`), with `drive_c` inside.
pub(super) fn parse_prefix_input(value: &str) -> Result<PathBuf, String> {
    let raw = value.trim();
    let path = if let Some(rest) = raw.strip_prefix("~/") { crate::installdir::home().join(rest) } else { PathBuf::from(raw) };
    if raw.is_empty() || !path.is_absolute() { return Err("Give the full path of the prefix folder (starting with / or ~/).".into()); }
    if !path.join("drive_c").is_dir() { return Err("That folder has no drive_c inside, so it is not a Wine prefix.".into()); }
    Ok(path)
}

// ---------------------------------------------------------------- the source

impl RockstarSource {
    pub fn new(http: Arc<dyn Http>, config_dir: PathBuf, db: PathBuf) -> Self { RockstarSource { http, config_dir, db, catalog: Mutex::new(None) } }
    fn prefix(&self) -> Option<PathBuf> { prefix_setting(&self.config_dir, "rockstar") }
    /// Rockstar's public catalog, fetched once per run. Empty when it cannot be reached (the callers then fall back or say they cannot tell).
    fn catalog(&self) -> HashMap<String, Title> {
        let mut g = self.catalog.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = g.as_ref() { return c.clone(); }
        let c = match self.http.get(TITLE_METADATA_URL, &[], Duration::from_secs(15)) {
            Ok(r) if r.status < 400 => parse_catalog(&r.body),
            _ => { crate::net::log("Rockstar: could not fetch the title catalog"); return HashMap::new(); }   // not cached: try again next time
        };
        *g = Some(c.clone());
        c
    }
}

const FLAGS: [&str; 1] = ["-enableFullMode"];

impl LibrarySource for RockstarSource {
    fn id(&self) -> &str { "rockstar" }
    fn label(&self) -> &str { "Rockstar Games" }
    fn caps(&self) -> Caps { Caps { auth: true, sync: true, launch: true, uninstall: true, installed_scan: true, store_page: true, ..Default::default() } }

    fn auth_status(&self) -> Result<AuthStatus, String> {
        let p = self.prefix();
        Ok(AuthStatus { connected: p.is_some(), username: None })   // the launcher's folder found; its path is not a name
    }
    fn sign_in_hint(&self) -> Option<&'static str> { Some("Rockstar has no website sign-in Zest can use: sign in inside the Rockstar Games Launcher, open My Library there, then SYNC LIBRARY will find your games.") }
    fn auth_begin(&self) -> Result<AuthBegin, String> {
        Ok(AuthBegin { url: String::new(), placeholder: "Full path of the Wine prefix the Rockstar Games Launcher is installed in".into(), instructions: vec![
            "Zest reads the Rockstar Games Launcher's own log files: it needs the folder (the Wine prefix) the launcher is installed in.".into(),
            "The folder contains drive_c. Typical place: ~/.local/share/Zest/rockstar (or ~/.local/share/PlayDate/rockstar from PlayDate).".into(),
            "Sign in once in the launcher and open My Library so it writes your games into its log, then paste the full path below.".into()] })
    }
    fn auth_complete(&self, value: &str) -> Result<AuthStatus, String> {
        let path = parse_prefix_input(value)?;
        if log_dir(&path).is_none() && find_launcher(&path).is_none() { return Err("The Rockstar Games Launcher was not found in that prefix. Install it there and sign in once.".into()); }
        crate::account::plugin_secret_set(&self.config_dir, "rockstar", "prefix", Some(&path.to_string_lossy())).map_err(|_| "Could not save the folder".to_string())?;
        self.auth_status()
    }
    fn auth_disconnect(&self) -> Result<(), String> {
        crate::account::plugin_secret_set(&self.config_dir, "rockstar", "prefix", None).map_err(|_| "Could not save".to_string())?;
        crate::launcher_installer::forget_prefix(&self.config_dir, "rockstar");   // the older saved place too, or the folder would still be found
        Ok(())
    }
    fn launcher_folder(&self) -> Option<PathBuf> { self.prefix() }

    fn sync(&self, _cx: &SourceCtx, sink: &mut dyn FnMut(Vec<GameRecord>), cancel: &Cancel) -> Result<usize, String> {
        if cancel.is_set() { return Ok(0); }
        let prefix = self.prefix().ok_or("Rockstar Games Launcher not found: sign in with the prefix folder first")?;
        let dir = log_dir(&prefix).ok_or("The launcher has not written its logs yet. Open the Rockstar Games Launcher once and sign in.")?;
        let owned = owned_in_dir(&dir);
        if owned.is_empty() { return Err("No owned titles found in the launcher logs. Make sure you are signed in and have opened My Library in the Rockstar Games Launcher, then try again.".into()); }
        let cat = self.catalog();
        let records: Vec<GameRecord> = owned.iter().map(|id| record(id, cat.get(id))).collect();
        let n = records.len();
        crate::net::log(&format!("Rockstar sync: {n} games from the launcher logs"));
        sink(records);
        Ok(n)
    }

    fn installed_scan(&self) -> Option<Vec<InstalledRow>> {
        let c = rusqlite::Connection::open_with_flags(&self.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
        let mut st = c.prepare("SELECT platform_id FROM games WHERE platform = 'rockstar' AND platform_id IS NOT NULL").ok()?;
        let ids: Vec<String> = st.query_map([], |r| r.get::<_, String>(0)).ok()?.flatten().collect();
        if ids.is_empty() { return Some(vec![]); }
        // no prefix: nothing can be installed (a stale flag is cleared, as PlayDate does)
        let Some(prefix) = self.prefix() else { return Some(ids.into_iter().map(|id| InstalledRow { platform_id: id, installed: false, install_path: Some(String::new()) }).collect()) };
        let cat = self.catalog();
        if cat.is_empty() { return None; }   // without the catalog nobody knows the program names: leave the flags alone rather than clear them
        Some(ids.into_iter().map(|id| {
            let exe = cat.get(&id).and_then(|t| find_game_exe(&prefix, &t.exe));
            InstalledRow { installed: exe.is_some(), install_path: Some(exe.as_ref().and_then(|e| e.parent()).map(|p| p.to_string_lossy().to_string()).unwrap_or_default()), platform_id: id }
        }).collect())
    }

    fn store_url(&self, g: &GameRef) -> Option<String> { (!g.slug.is_empty()).then(|| format!("https://store.rockstargames.com/game/{}", g.slug)) }

    fn launch(&self, g: &GameRef) -> Result<Launch, String> {
        if g.platform_id.trim().is_empty() { return Err("Game has no title ID: try syncing again".into()); }
        let prefix = self.prefix().ok_or("Rockstar Games Launcher not found. Set up its Wine prefix in the Plugins dialog.")?;
        let launcher = find_launcher(&prefix).ok_or("Rockstar Games Launcher not found. Set up its Wine prefix in the Plugins dialog.")?;
        let game_exe = self.catalog().get(g.platform_id.trim()).and_then(|t| find_game_exe(&prefix, &t.exe));
        if let Some(exe) = game_exe {
            // the launcher's own command line for a game (from its log), so the title behaves as when started from the launcher
            let action = crate::wine::launch_action_args(&exe, &prefix, &["-fromRGL".into(), "-rglLanguage=en-US".into()])?;
            return Ok(Launch { action, record: true });
        }
        // not installed: ask the launcher to install it; its own window shows the progress
        let args = vec![FLAGS[0].to_string(), format!("-install={}", g.platform_id.trim())];
        let spawn = crate::wine::launch_action_args(&launcher, &prefix, &args)?;
        Ok(Launch { action: Action::Many(vec![Action::Message(format!("Opening the Rockstar Games Launcher to install {}\u{2026}", g.name)), spawn]), record: false })
    }

    fn uninstall(&self, g: &GameRef) -> Result<Action, String> {
        if g.platform_id.trim().is_empty() { return Err("Game has no title ID".into()); }
        let prefix = self.prefix().ok_or("Rockstar Games Launcher not found.")?;
        let launcher = find_launcher(&prefix).ok_or("Rockstar Games Launcher not found.")?;
        crate::wine::launch_action_args(&launcher, &prefix, &[FLAGS[0].to_string(), format!("-uninstall={}", g.platform_id.trim())])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tmpdir(tag: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("zest_rs_{tag}_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); std::fs::write(d.join("config.json"), "{\"other\": 1}").unwrap(); d }
    fn cx(d: &PathBuf) -> SourceCtx { let h: Arc<dyn Http> = Arc::new(crate::net::UreqHttp::new()); SourceCtx { http: h.clone(), steam: Arc::new(crate::steamclient::Steam::new(h, None, 10.0)), root: d.clone(), db: d.join("games.db"), today: "2026-11-04".into(), appinfo: Arc::new(|_| Default::default()), account_id: None } }

    struct Fake(String);
    impl Http for Fake { fn get(&self, url: &str, _h: &[(&str, &str)], _t: Duration) -> Result<crate::net::Response, crate::net::NetError> { assert_eq!(url, TITLE_METADATA_URL); Ok(crate::net::Response { status: 200, retry_after: None, body: self.0.clone() }) } }
    const CATALOG: &str = r#"{"titles": {"a": {"titleId": "gta5", "friendlyName": "Grand Theft Auto V", "principalFileName": "GTA5.exe"},
        "b": {"titleId": "lcs", "friendlyName": "Grand Theft Auto III – The Definitive Edition", "principalFileName": "Gameface\\Binaries\\Win64\\LibertyCity.exe"}, "c": {"friendlyName": "no id"}}}"#;

    /// A prefix with the launcher, a log folder, and one installed game.
    fn prefix(tag: &str) -> PathBuf {
        let p = tmpdir(tag).join("pfx");
        let user = p.join("drive_c/users/steamuser");
        let logs = user.join("Documents/Rockstar Games/Launcher");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(logs.join("launcher.log"), "[12:00] [mergedmanifest] Using regular build for launcher\n[12:01] [mergedmanifest] Using build collection for gta5\nnoise\n").unwrap();
        std::fs::write(logs.join("launcher.01.log"), "[mergedmanifest]  Using regular build for lcs extra\n[mergedmanifest] Using build collection for 57\n").unwrap();
        std::fs::write(logs.join("other.log"), "[mergedmanifest] Using regular build for ignored\n").unwrap();
        let pf = p.join("drive_c/Program Files/Rockstar Games");
        std::fs::create_dir_all(pf.join("Launcher")).unwrap(); std::fs::write(pf.join("Launcher/Launcher.exe"), "x").unwrap();
        std::fs::create_dir_all(pf.join("GTA V")).unwrap(); std::fs::write(pf.join("GTA V/GTA5.exe"), "x").unwrap();
        p
    }

    #[test] fn log_lines_give_the_owned_titles_and_never_the_launcher_itself() {
        let t = "a [mergedmanifest] Using regular build for launcher\nb [mergedmanifest] Using build collection for gta5 trailing\nc [mergedmanifest]Using regular build for nospace\nd [mergedmanifest] Using the build for x\ne [mergedmanifest] Using regular build for rdr2\n";
        assert_eq!(owned_in_log(t).into_iter().collect::<Vec<_>>(), vec!["gta5", "rdr2"]);
    }
    #[test] fn names_slugs_and_the_user_folder() {
        assert_eq!(slugify("Grand Theft Auto: San Andreas \u{2013} The Definitive Edition"), "grand-theft-auto-san-andreas-the-definitive-edition");
        assert_eq!(slugify("  L.A. Noire "), "l-a-noire");
        let p = prefix("user"); let users = p.join("drive_c/users");
        assert_eq!(wine_user_dir(&p, "rob"), Some(users.join("steamuser")));
        std::fs::remove_dir_all(users.join("steamuser")).unwrap(); std::fs::create_dir_all(users.join("Public")).unwrap(); std::fs::create_dir_all(users.join("zed")).unwrap();
        assert_eq!(wine_user_dir(&p, "rob"), Some(users.join("zed")), "the one real profile");
        assert_eq!(wine_user_dir(&p, "Public"), Some(users.join("Public")), "a folder named after the Unix user wins when it exists");
    }
    #[test] fn the_catalog_and_the_game_program_search() {
        let c = parse_catalog(CATALOG);
        assert_eq!(c.len(), 2); assert_eq!(c["gta5"], Title { name: "Grand Theft Auto V".into(), exe: "GTA5.exe".into() });
        let p = prefix("exe");
        assert_eq!(find_game_exe(&p, &c["gta5"].exe), Some(p.join("drive_c/Program Files/Rockstar Games/GTA V/GTA5.exe")));
        assert_eq!(find_game_exe(&p, &c["lcs"].exe), None, "only the last part of a sub-path is matched, and it is not installed");
        std::fs::create_dir_all(p.join("drive_c/Program Files (x86)/LC/x")).unwrap(); std::fs::write(p.join("drive_c/Program Files (x86)/LC/x/LibertyCity.exe"), "x").unwrap();
        assert!(find_game_exe(&p, &c["lcs"].exe).is_some());
        assert_eq!(find_game_exe(&p, ""), None);
        assert_eq!(find_launcher(&p), Some(p.join("drive_c/Program Files/Rockstar Games/Launcher/Launcher.exe")));
    }
    #[test] fn records_use_the_catalog_then_the_built_in_table_then_the_id() {
        let c = parse_catalog(CATALOG);
        assert_eq!((record("gta5", c.get("gta5")).name.as_str(), record("gta5", c.get("gta5")).slug.as_deref()), ("Grand Theft Auto V", Some("grand-theft-auto-v")));
        assert_eq!(record("57", None).name, "Red Dead Redemption 2");
        assert_eq!(record("zzz", None).name, "zzz");
    }
    #[cfg(not(windows))]
    #[test] fn the_prefix_is_checked_kept_synced_scanned_and_used_to_launch() {
        let p = prefix("flow"); let d = p.parent().unwrap().to_path_buf();
        let db = d.join("games_1.db");
        let s = RockstarSource::new(Arc::new(Fake(CATALOG.into())), d.clone(), db.clone());
        assert!(!s.auth_status().unwrap().connected && s.sync(&cx(&d), &mut |_| {}, &Cancel::new()).unwrap_err().contains("sign in"));
        assert!(s.auth_complete("relative").is_err() && s.auth_complete(&d.to_string_lossy()).unwrap_err().contains("no drive_c"));
        std::fs::create_dir_all(d.join("empty/drive_c")).unwrap();
        assert!(s.auth_complete(&d.join("empty").to_string_lossy()).unwrap_err().contains("not found"));
        assert!(s.auth_complete(&p.to_string_lossy()).unwrap().connected);
        let cfg: Value = serde_json::from_str(&std::fs::read_to_string(d.join("config.json")).unwrap()).unwrap();
        assert_eq!((cfg["rockstar"]["prefix"].as_str(), cfg["other"].as_i64()), (Some(&*p.to_string_lossy()), Some(1)));
        let mut got = vec![]; let n = s.sync(&cx(&d), &mut |b| got.extend(b), &Cancel::new()).unwrap();
        assert_eq!(n, 3); got.sort_by(|a, b| a.platform_id.cmp(&b.platform_id));
        assert_eq!(got.iter().map(|r| (r.platform_id.as_str(), r.name.as_str())).collect::<Vec<_>>(), vec![("57", "Red Dead Redemption 2"), ("gta5", "Grand Theft Auto V"), ("lcs", "Grand Theft Auto III \u{2013} The Definitive Edition")]);
        // the installed scan: GTA V is on disk, the others are not; a game's folder is its program's folder
        rusqlite::Connection::open(&db).unwrap().execute_batch("CREATE TABLE games (appid INTEGER, platform TEXT, platform_id TEXT); INSERT INTO games VALUES (-1,'rockstar','gta5'),(-2,'rockstar','lcs'),(-3,'gog','9');").unwrap();
        let rows = s.installed_scan().unwrap();
        assert_eq!(rows.iter().map(|r| (r.platform_id.as_str(), r.installed)).collect::<Vec<_>>(), vec![("gta5", true), ("lcs", false)]);
        assert_eq!(rows[0].install_path.as_deref(), Some(&*p.join("drive_c/Program Files/Rockstar Games/GTA V").to_string_lossy()));
        // launch: an installed game starts with the launcher's flags; a missing one asks the launcher to install it
        crate::wine::TEST_RUNTIME.with(|t| *t.borrow_mut() = Some(Ok(crate::wine::Runtime::Wine { wine: "/usr/bin/wine".into() })));
        let g = GameRef { appid: -1, name: "Grand Theft Auto V".into(), platform: "rockstar".into(), platform_id: "gta5".into(), slug: "grand-theft-auto-v".into(), ..Default::default() };
        let l = s.launch(&g).unwrap();
        match &l.action { Action::Spawn { args, .. } => assert_eq!(args[1..], ["-fromRGL", "-rglLanguage=en-US"]), a => panic!("{a:?}") }
        assert!(l.record);
        let l = s.launch(&GameRef { platform_id: "lcs".into(), name: "LC".into(), ..g.clone() }).unwrap();
        assert!(!l.record);
        match &l.action { Action::Many(v) => match &v[1] { Action::Spawn { args, .. } => assert_eq!(args[1..], ["-enableFullMode", "-install=lcs"]), a => panic!("{a:?}") }, a => panic!("{a:?}") }
        match s.uninstall(&g).unwrap() { Action::Spawn { args, .. } => assert_eq!(args[1..], ["-enableFullMode", "-uninstall=gta5"]), a => panic!("{a:?}") }
        assert_eq!(s.store_url(&g).as_deref(), Some("https://store.rockstargames.com/game/grand-theft-auto-v"));
        // disconnect clears the flags of every game (nothing can be installed without a prefix)
        s.auth_disconnect().unwrap(); assert!(!s.auth_status().unwrap().connected);
        let rows = s.installed_scan().unwrap(); assert!(rows.iter().all(|r| !r.installed && r.install_path.as_deref() == Some("")));
    }
    #[cfg(not(windows))]
    #[test] fn an_unreachable_catalog_leaves_installed_flags_alone() {
        struct Down; impl Http for Down { fn get(&self, _u: &str, _h: &[(&str, &str)], _t: Duration) -> Result<crate::net::Response, crate::net::NetError> { Err(crate::net::NetError("offline".into())) } }
        let p = prefix("down"); let d = p.parent().unwrap().to_path_buf(); let db = d.join("g.db");
        rusqlite::Connection::open(&db).unwrap().execute_batch("CREATE TABLE games (appid INTEGER, platform TEXT, platform_id TEXT); INSERT INTO games VALUES (-1,'rockstar','gta5');").unwrap();
        let s = RockstarSource::new(Arc::new(Down), d, db);
        s.auth_complete(&p.to_string_lossy()).unwrap();
        assert!(s.installed_scan().is_none());
    }
}
