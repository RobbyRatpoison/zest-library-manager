//! Legacy Games from the Legacy Games Launcher's own files (PlayDate's `plugins/legacy_games`): no account, no network. The "sign-in" is the Wine prefix (on Linux) the launcher
//! is installed in. The launcher's electron-store file (`app-state.json`) holds the signed-in account's owned downloads and the whole site catalog; install state is the
//! per-game `HKCU\Software\Legacy Games\<name>` key in the prefix's `user.reg`. The launcher has no install command line, so an uninstalled game's Play opens the launcher.
use super::rockstar::{find_file, parse_prefix_input, prefix_setting, wine_user_dir};
use super::{Action, AuthBegin, AuthStatus, Caps, GameRef, InstalledRow, Launch, LibrarySource, SourceCtx};
use crate::importer::GameRecord;
use crate::net::Cancel;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct LegacyGamesSource { config_dir: PathBuf, db: PathBuf }

/// One installed game's registry key.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RegEntry { pub name: String, pub inst_dir: String, pub game_exe: String }

// ---------------------------------------------------------------- pure parts

/// `{installer uuid: entry}` from the text of a Wine `user.reg`: every `[Software\\Legacy Games\\<name>]` key that has an `InstallerUUID`.
pub fn parse_registry(text: &str) -> HashMap<String, RegEntry> {
    const HEAD: &str = "[Software\\\\Legacy Games\\\\";
    let lines: Vec<&str> = text.split('\n').collect();
    let unesc = |s: &str| s.replace("\\\\", "\\");
    let mut out = HashMap::new();
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i];
        let name = match l.strip_prefix(HEAD).and_then(|r| r.find(']').filter(|&e| e > 0).map(|e| r[..e].to_string())) { Some(n) => unesc(&n), None => { i += 1; continue } };
        let mut values: HashMap<String, String> = HashMap::new();
        i += 1;
        while i < lines.len() && !lines[i].trim().is_empty() && !lines[i].starts_with('[') {
            let l = lines[i];
            if let Some(rest) = l.strip_prefix('"') {
                if let Some(q) = rest.find("\"=\"") {
                    let (k, v) = (&rest[..q], &rest[q + 3..]);
                    if !k.is_empty() && k.chars().all(|c| c.is_ascii_alphabetic()) && v.ends_with('"') { values.insert(k.to_string(), unesc(&v[..v.len() - 1])); }
                }
            }
            i += 1;
        }
        if let Some(u) = values.get("InstallerUUID").filter(|u| !u.is_empty()) {
            out.insert(u.clone(), RegEntry { name, inst_dir: values.get("InstDir").cloned().unwrap_or_default(), game_exe: values.get("GameExe").cloned().unwrap_or_default() });
        }
    }
    out
}

/// A registry path (`C:\Program Files\Legacy Games\Foo`) as the folder inside the prefix. Other drives are not in the prefix's `drive_c`: empty (PlayDate built a
/// path under a folder literally named `D:` for them, which does not exist).
pub fn win_path_to_host(prefix: &Path, win_path: &str) -> String {
    if win_path.is_empty() { return String::new(); }
    let rel = match win_path.split_once("C:\\") { Some((_, r)) => r, None if win_path.as_bytes().get(1) == Some(&b':') => return String::new(), None => win_path };
    let p = prefix.join("drive_c").join(rel.replace('\\', "/"));
    p.to_string_lossy().to_string()
}

pub fn app_state_path(prefix: &Path) -> Option<PathBuf> {
    let p = wine_user_dir(prefix, &std::env::var("USER").unwrap_or_default())?.join("AppData/Roaming/legacy-games-launcher/app-state.json");
    p.is_file().then_some(p)
}

fn load_state(prefix: &Path) -> Option<Value> { serde_json::from_str(&std::fs::read_to_string(app_state_path(prefix)?).ok()?).ok() }

/// The site catalog by product id (an entry without one is skipped).
pub fn catalog(state: &Value) -> HashMap<i64, &Value> {
    state["siteData"]["catalog"].as_array().map(|a| a.iter().filter_map(|i| Some((i.get("product_id")?.as_i64()?, i))).collect()).unwrap_or_default()
}

fn first_game(product: &Value) -> &Value { product["games"].as_array().and_then(|g| g.first()).unwrap_or(&Value::Null) }
fn text<'a>(v: &'a Value, k: &str) -> &'a str { v.get(k).and_then(|x| x.as_str()).unwrap_or("") }

/// The owned products as records: product id as the store id, the launcher's installer uuid as `appname` (the key into the registry).
pub fn records(state: &Value, reg: &HashMap<String, RegEntry>, prefix: &Path) -> Vec<GameRecord> {
    let cat = catalog(state);
    let mut out = vec![];
    for d in state["user"]["profile"]["downloads"].as_array().map(|a| a.as_slice()).unwrap_or(&[]) {
        let Some(pid) = d.get("product_id").and_then(|p| p.as_i64()) else { continue };
        let product = cat.get(&pid).copied().unwrap_or(&Value::Null);
        let game = first_game(product);
        let name = [text(game, "game_name"), text(product, "product_name"), text(product, "name")].into_iter().find(|n| !n.is_empty()).map(String::from).unwrap_or_else(|| format!("Legacy Games {pid}"));
        let uuid = text(game, "installer_uuid");
        let entry = (!uuid.is_empty()).then(|| reg.get(uuid)).flatten();
        let path = entry.map(|e| win_path_to_host(prefix, &e.inst_dir)).filter(|p| !p.is_empty());
        out.push(GameRecord { platform_id: pid.to_string(), name, slug: Some(text(product, "slug").to_string()).filter(|s| !s.is_empty()), appname: Some(uuid.to_string()).filter(|s| !s.is_empty()),
            installed: Some(entry.is_some()), install_path: path, ..Default::default() });
    }
    out
}

/// The launcher's own art for a product: the cover as vertical, the first wide image as horizontal (only when the catalog has one: a cover stretched wide is worse than another source's).
pub fn art(product: &Value) -> Vec<(String, String)> {
    let mut v = vec![];
    let cover = text(first_game(product), "game_coverart");
    if !cover.is_empty() { v.push(("vertical".to_string(), cover.to_string())); }
    let wide = product["images"].as_array().and_then(|a| a.first()).map(|i| text(i, "src")).unwrap_or("");
    if !wide.is_empty() { v.push(("horizontal".to_string(), wide.to_string())); }
    v
}

// ---------------------------------------------------------------- the source

impl LegacyGamesSource {
    pub fn new(config_dir: PathBuf, db: PathBuf) -> Self { LegacyGamesSource { config_dir, db } }
    fn prefix(&self) -> Option<PathBuf> { prefix_setting(&self.config_dir, "legacy_games") }
    fn registry(prefix: &Path) -> HashMap<String, RegEntry> { std::fs::read_to_string(prefix.join("user.reg")).map(|t| parse_registry(&t)).unwrap_or_default() }
    fn launcher(prefix: &Path) -> Option<PathBuf> { find_file(&prefix.join("drive_c"), "Legacy Games Launcher.exe", 8) }
    /// The registry entry of an installed game, and the host folder it points at.
    fn install_of(prefix: &Path, uuid: &str) -> Option<(RegEntry, String)> {
        let e = Self::registry(prefix).remove(uuid).filter(|_| !uuid.is_empty())?;
        let dir = win_path_to_host(prefix, &e.inst_dir);
        Some((e, dir))
    }
}

impl LibrarySource for LegacyGamesSource {
    fn id(&self) -> &str { "legacy_games" }
    fn label(&self) -> &str { "Legacy Games" }
    fn caps(&self) -> Caps { Caps { auth: true, sync: true, launch: true, uninstall: true, installed_scan: true, art: true, ..Default::default() } }

    fn auth_status(&self) -> Result<AuthStatus, String> {
        // signed in = the launcher itself has an account: its state file holds the account's profile only after someone signed in there (a fresh install has just a token)
        let signed_in = self.prefix().and_then(|p| load_state(&p)).map_or(false, |s| s["user"]["profile"].is_object());
        Ok(AuthStatus { connected: signed_in, username: None })
    }
    fn sign_in_hint(&self) -> Option<&'static str> { Some("Sign in inside the Legacy Games Launcher. Once you have signed in there, SYNC LIBRARY will find your games.") }
    fn auth_begin(&self) -> Result<AuthBegin, String> {
        Ok(AuthBegin { url: String::new(), placeholder: "Full path of the Wine prefix the Legacy Games Launcher is installed in".into(), instructions: vec![
            "Zest reads the Legacy Games Launcher's own data files: it needs the folder (the Wine prefix) the launcher is installed in.".into(),
            "The folder contains drive_c. Typical place: ~/.local/share/Zest/legacy_games (or ~/.local/share/PlayDate/legacy_games from PlayDate).".into(),
            "Sign in once in the launcher so it has saved your games, then paste the full path below.".into()] })
    }
    fn auth_complete(&self, value: &str) -> Result<AuthStatus, String> {
        let path = parse_prefix_input(value)?;
        if app_state_path(&path).is_none() && Self::launcher(&path).is_none() { return Err("The Legacy Games Launcher was not found in that prefix. Install it there and sign in once.".into()); }
        crate::account::plugin_secret_set(&self.config_dir, "legacy_games", "prefix", Some(&path.to_string_lossy())).map_err(|_| "Could not save the folder".to_string())?;
        self.auth_status()
    }
    fn auth_disconnect(&self) -> Result<(), String> {
        crate::account::plugin_secret_set(&self.config_dir, "legacy_games", "prefix", None).map_err(|_| "Could not save".to_string())?;
        crate::launcher_installer::forget_prefix(&self.config_dir, "legacy_games");   // the older saved place too, or the folder would still be found
        Ok(())
    }
    fn launcher_folder(&self) -> Option<PathBuf> { self.prefix() }

    fn sync(&self, _cx: &SourceCtx, sink: &mut dyn FnMut(Vec<GameRecord>), cancel: &Cancel) -> Result<usize, String> {
        if cancel.is_set() { return Ok(0); }
        let prefix = self.prefix().ok_or("Legacy Games Launcher not set up: sign in with the prefix folder first")?;
        let state = load_state(&prefix).ok_or("Legacy Games Launcher data not found. Install it and sign in first.")?;
        let recs = records(&state, &Self::registry(&prefix), &prefix);
        if recs.is_empty() { return Err("No owned games found. Make sure you are signed in to the Legacy Games Launcher and have bought or claimed at least one game, then try again.".into()); }
        let n = recs.len();
        crate::net::log(&format!("Legacy Games sync: {n} games from the launcher's data"));
        sink(recs);
        Ok(n)
    }

    /// Install state is in the prefix's `user.reg`, a file in the prefix folder itself.
    fn watch_paths(&self) -> Vec<(PathBuf, bool)> { self.prefix().map(|p| vec![(p, false)]).unwrap_or_default() }
    fn installed_scan(&self) -> Option<Vec<InstalledRow>> {
        let c = rusqlite::Connection::open_with_flags(&self.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
        let mut st = c.prepare("SELECT platform_id, COALESCE(platform_appname, '') FROM games WHERE platform = 'legacy_games' AND platform_id IS NOT NULL").ok()?;
        let rows: Vec<(String, String)> = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).ok()?.flatten().collect();
        // no prefix: nothing can be installed, so stale flags are cleared (as PlayDate)
        let prefix = self.prefix();
        let reg = prefix.as_deref().map(Self::registry).unwrap_or_default();
        Some(rows.into_iter().map(|(id, uuid)| match (&prefix, reg.get(&uuid).filter(|_| !uuid.is_empty())) {
            (Some(p), Some(e)) => InstalledRow { platform_id: id, installed: true, install_path: Some(win_path_to_host(p, &e.inst_dir)) },
            _ => InstalledRow { platform_id: id, installed: false, install_path: Some(String::new()) },
        }).collect())
    }

    fn art_urls(&self, g: &GameRef) -> Vec<(String, String)> {
        let (Some(prefix), Ok(pid)) = (self.prefix(), g.platform_id.parse::<i64>()) else { return vec![] };
        let Some(state) = load_state(&prefix) else { return vec![] };
        catalog(&state).get(&pid).map(|p| art(p)).unwrap_or_default()
    }

    fn launch(&self, g: &GameRef) -> Result<Launch, String> {
        let prefix = self.prefix().ok_or("Legacy Games Launcher not set up. Give its Wine prefix in the Plugins dialog.")?;
        if let Some((e, dir)) = Self::install_of(&prefix, g.appname.trim()) {
            let exe = Path::new(&dir).join(&e.game_exe);
            if !e.game_exe.is_empty() && exe.is_file() { return Ok(Launch { action: crate::wine::launch_action(&exe, &prefix)?, record: true }); }
        }
        // not installed (or the registry or program is missing): the launcher has no install command, so open it and let the user install there
        let launcher = Self::launcher(&prefix).ok_or("Legacy Games Launcher not found in the prefix.")?;
        let spawn = crate::wine::launch_action(&launcher, &prefix)?;
        Ok(Launch { action: Action::Many(vec![Action::Message(format!("Opening Legacy Games Launcher: install {} there, then press Play again.", g.name)), spawn]), record: false })
    }

    fn uninstall(&self, g: &GameRef) -> Result<Action, String> {
        let prefix = self.prefix().ok_or("Legacy Games Launcher not set up.")?;
        let (_, dir) = Self::install_of(&prefix, g.appname.trim()).filter(|(e, _)| !e.inst_dir.is_empty()).ok_or("Could not find this game's install record. Uninstall it from the Legacy Games Launcher instead.")?;
        let un = Path::new(&dir).join("Uninstall.exe");
        if !un.is_file() { return Err("Uninstaller not found for this game. Uninstall it from the Legacy Games Launcher instead.".into()); }
        // the uninstaller re-runs itself from Temp and its exit code means nothing: the install scan (run after a game closes or at start) sees the folder go
        crate::wine::launch_action(&un, &prefix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn here() -> PathBuf { Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests")).to_path_buf() }
    fn golden() -> Value { serde_json::from_str(&std::fs::read_to_string(here().join("legacy_golden.json")).unwrap()).unwrap() }
    fn fixture() -> PathBuf { here().join("data/legacy_prefix") }
    fn tmpdir(tag: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("zest_lg_{tag}_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); std::fs::write(d.join("config.json"), "{\"other\": 1}").unwrap(); d }
    fn cx(d: &PathBuf) -> SourceCtx { let h: std::sync::Arc<dyn crate::net::Http> = std::sync::Arc::new(crate::net::UreqHttp::new()); SourceCtx { http: h.clone(), steam: std::sync::Arc::new(crate::steamclient::Steam::new(h, None, 10.0)), root: d.clone(), db: d.join("games.db"), today: "2026-11-04".into(), appinfo: std::sync::Arc::new(|_| Default::default()), account_id: None } }

    #[test] fn the_registry_catalog_and_paths_agree_with_the_real_python() {
        let g = golden();
        let reg = parse_registry(&std::fs::read_to_string(fixture().join("user.reg")).unwrap());
        let want = g["registry"].as_object().unwrap();
        assert_eq!(reg.len(), want.len());
        for (uuid, e) in want { assert_eq!(reg[uuid], RegEntry { name: e["name"].as_str().unwrap().into(), inst_dir: e["inst_dir"].as_str().unwrap().into(), game_exe: e["game_exe"].as_str().unwrap().into() }, "{uuid}"); }
        let state = load_state(&fixture()).unwrap();
        let mut ids: Vec<i64> = catalog(&state).keys().copied().collect(); ids.sort();
        assert_eq!(ids, g["catalog_ids"].as_array().unwrap().iter().map(|v| v.as_i64().unwrap()).collect::<Vec<_>>());
        for (win, host) in g["host_paths"].as_object().unwrap() {
            let got = win_path_to_host(Path::new("<ROOT>"), win);
            // the one deliberate difference: a path on another drive is empty here (PlayDate built a bogus folder name)
            let want = host.as_str().unwrap().replace("<ROOT>/drive_c/D:/Other/Place", "");
            assert_eq!(got.replace('\\', "/"), want.replace('\\', "/"), "{win}");
        }
    }
    #[test] fn owned_downloads_become_records_with_install_state_and_art() {
        let state = load_state(&fixture()).unwrap();
        let reg = parse_registry(&std::fs::read_to_string(fixture().join("user.reg")).unwrap());
        let recs = records(&state, &reg, &fixture());
        assert_eq!(recs.iter().map(|r| (r.platform_id.as_str(), r.name.as_str())).collect::<Vec<_>>(), vec![("101", "Hidden Foo"), ("102", "Dir Only Product"), ("999", "Legacy Games 999")]);
        assert_eq!((recs[0].slug.as_deref(), recs[0].appname.as_deref(), recs[0].installed), (Some("hidden-foo"), Some("uuid-foo"), Some(true)));
        assert_eq!(recs[0].install_path.as_deref(), Some(&*fixture().join("drive_c/Program Files/Legacy Games/Hidden Foo").to_string_lossy()));
        assert_eq!((recs[1].installed, recs[1].appname.as_deref()), (Some(false), None), "a product with no game entry has no installer uuid");
        assert_eq!(recs[2].installed, Some(false));
        let cat = catalog(&state);
        assert_eq!(art(cat[&101]), vec![("vertical".to_string(), "https://img.example/foo.jpg".to_string()), ("horizontal".to_string(), "https://img.example/foo-wide.jpg".to_string())]);
        assert!(art(cat[&102]).is_empty());
    }
    #[cfg(not(windows))]
    #[test] fn the_prefix_is_kept_and_sync_scan_launch_and_uninstall_work_on_it() {
        let d = tmpdir("flow"); let pfx = d.join("pfx");
        // a copy of the fixture prefix with real files for the installed game
        fn copy(a: &Path, b: &Path) { std::fs::create_dir_all(b).unwrap(); for e in std::fs::read_dir(a).unwrap().flatten() { let t = b.join(e.file_name()); if e.path().is_dir() { copy(&e.path(), &t) } else { std::fs::copy(e.path(), t).unwrap(); } } }
        copy(&fixture(), &pfx);
        let inst = pfx.join("drive_c/Program Files/Legacy Games/Hidden Foo"); std::fs::create_dir_all(&inst).unwrap();
        std::fs::write(inst.join("foo.exe"), "x").unwrap(); std::fs::write(inst.join("Uninstall.exe"), "x").unwrap();
        let db = d.join("games_1.db");
        let s = LegacyGamesSource::new(d.clone(), db.clone());
        assert!(!s.auth_status().unwrap().connected && s.sync(&cx(&d), &mut |_| {}, &Cancel::new()).unwrap_err().contains("not set up"));
        std::fs::create_dir_all(d.join("empty/drive_c")).unwrap();
        assert!(s.auth_complete(&d.join("empty").to_string_lossy()).unwrap_err().contains("not found") && s.auth_complete("nope").is_err());
        assert!(s.auth_complete(&pfx.to_string_lossy()).unwrap().connected);
        let mut got = vec![]; assert_eq!(s.sync(&cx(&d), &mut |b| got.extend(b), &Cancel::new()).unwrap(), 3);
        assert_eq!(got[0].installed, Some(true));
        rusqlite::Connection::open(&db).unwrap().execute_batch("CREATE TABLE games (appid INTEGER, platform TEXT, platform_id TEXT, platform_appname TEXT); INSERT INTO games VALUES (-1,'legacy_games','101','uuid-foo'),(-2,'legacy_games','102','uuid-nope');").unwrap();
        let rows = s.installed_scan().unwrap();
        assert_eq!(rows.iter().map(|r| (r.platform_id.as_str(), r.installed)).collect::<Vec<_>>(), vec![("101", true), ("102", false)]);
        crate::wine::TEST_RUNTIME.with(|t| *t.borrow_mut() = Some(Ok(crate::wine::Runtime::Wine { wine: "/usr/bin/wine".into() })));
        let g = GameRef { appid: -1, name: "Hidden Foo".into(), platform: "legacy_games".into(), platform_id: "101".into(), appname: "uuid-foo".into(), ..Default::default() };
        let l = s.launch(&g).unwrap();
        assert!(l.record); match &l.action { Action::Spawn { args, .. } => assert_eq!(args, &vec![inst.join("foo.exe").to_string_lossy().to_string()]), a => panic!("{a:?}") }
        match s.uninstall(&g).unwrap() { Action::Spawn { args, .. } => assert_eq!(args, vec![inst.join("Uninstall.exe").to_string_lossy().to_string()]), a => panic!("{a:?}") }
        // not installed: the launcher is opened, no launch is recorded; with no launcher exe in the prefix that is an error
        let other = GameRef { appname: "uuid-nope".into(), ..g.clone() };
        assert!(s.launch(&other).unwrap_err().contains("not found"));
        let lx = pfx.join("drive_c/Program Files/Legacy Games Launcher"); std::fs::create_dir_all(&lx).unwrap(); std::fs::write(lx.join("Legacy Games Launcher.exe"), "x").unwrap();
        let l = s.launch(&other).unwrap(); assert!(!l.record); assert!(matches!(l.action, Action::Many(_)));
        assert!(s.uninstall(&other).unwrap_err().contains("install record"));
        std::fs::remove_file(inst.join("Uninstall.exe")).unwrap();
        assert!(s.uninstall(&g).unwrap_err().contains("Uninstaller not found"));
        assert_eq!(s.art_urls(&g).len(), 2);
        s.auth_disconnect().unwrap();
        assert!(s.installed_scan().unwrap().iter().all(|r| !r.installed && r.install_path.as_deref() == Some("")));
    }
}
