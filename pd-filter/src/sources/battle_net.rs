//! Battle.net from the client's own files (PlayDate's `plugins/battle_net` local sync): no account, no network. The "sign-in" is the folder of the Wine prefix the Battle.net
//! client is installed in (on Linux), checked for the Agent's `product.db` and kept in `config.json`; a sync reads the installed products and the account's cached licences
//! (`battle_net_local`). Windows reads `%PROGRAMDATA%` directly. An optional account sign-in (`battle_net_account`, the sign-in window's cookies) adds the owned games that are not
//! installed and the real purchase dates.
use super::{AccountLogin, Action, AuthBegin, AuthStatus, Caps, GameRef, InstalledRow, Launch, LibrarySource, SourceCtx};
use crate::battle_net_account as acct;
use crate::battle_net_local as bn;
use crate::importer::GameRecord;
use crate::net::Cancel;
use std::path::{Path, PathBuf};

pub struct BattleNetSource { config_dir: PathBuf, db: PathBuf, http: std::sync::Arc<dyn crate::net::Http> }

impl BattleNetSource {
    pub fn new(config_dir: PathBuf, db: PathBuf, http: std::sync::Arc<dyn crate::net::Http>) -> Self { BattleNetSource { config_dir, db, http } }
    /// The prefix pasted into the card, else the one INSTALL LAUNCHER set up (`launchers.battle_net.prefix`), the way the other launcher-based sources find theirs.
    fn prefix(&self) -> Option<PathBuf> { super::rockstar::prefix_setting(&self.config_dir, "battle_net") }
    /// What the client keeps: the prefix's Agent database, or on Windows the ProgramData one.
    fn db_path(prefix: Option<&Path>) -> PathBuf {
        match prefix { Some(p) => bn::agent_db_path(p), None => PathBuf::from(std::env::var_os("PROGRAMDATA").unwrap_or_else(|| "C:\\ProgramData".into())).join("Battle.net/Agent/product.db") }
    }
}

/// A scan result as a library record: the catalog code is the store id, as in PlayDate; Blizzard is developer and publisher of its own titles.
pub fn record(f: &bn::Found) -> GameRecord {
    let own = bn::by_code(&f.code).is_some();
    GameRecord { platform_id: f.code.clone(), name: f.name.clone(), genres: Some(f.genres.clone()).filter(|g| !g.is_empty()),
        developers: own.then(|| bn::PUBLISHER.to_string()), publishers: own.then(|| bn::PUBLISHER.to_string()),
        installed: Some(f.installed), install_path: Some(f.install_path.clone()).filter(|p| !p.is_empty()), ..Default::default() }
}

/// The account the Battle.net client last signed in with: `Client.SavedAccountNames` in its own `Battle.net.config` (the login name as typed, usually the email; the BattleTag itself is only
/// known to the web account sign-in). The first one when several are saved.
pub fn saved_login(prefix: Option<&Path>) -> Option<String> {
    let file = match prefix {
        Some(p) => std::fs::read_dir(p.join("drive_c/users")).ok()?.flatten().map(|u| u.path().join("AppData/Roaming/Battle.net/Battle.net.config")).find(|f| f.is_file())?,
        None => PathBuf::from(std::env::var_os("APPDATA")?).join("Battle.net/Battle.net.config"),
    };
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(file).ok()?).ok()?;
    v["Client"]["SavedAccountNames"].as_str()?.split(',').map(str::trim).find(|n| !n.is_empty()).map(mask_login)
}

/// A login shown partly hidden: `ro*****@gmail.com` (the first two letters and the mail domain stay).
pub fn mask_login(login: &str) -> String {
    let (name, domain) = match login.split_once('@') { Some((n, d)) => (n, format!("@{d}")), None => (login, String::new()) };
    let keep: String = name.chars().take(if name.chars().count() > 3 { 2 } else { 1 }).collect();
    format!("{keep}*****{domain}")
}

impl LibrarySource for BattleNetSource {
    fn id(&self) -> &str { "battle_net" }
    fn label(&self) -> &str { "Battle.net" }
    fn caps(&self) -> Caps { Caps { auth: true, sync: true, launch: true, uninstall: true, installed_scan: true, install: false, ..Default::default() } }
    fn auth_status(&self) -> Result<AuthStatus, String> {
        let ok = if cfg!(windows) { Self::db_path(None).is_file() } else { self.prefix().map_or(false, |p| p.is_dir()) };
        Ok(AuthStatus { connected: ok, username: if ok { let pre = if cfg!(windows) { None } else { self.prefix() }; saved_login(pre.as_deref()) } else { None } })   // the client's saved login; no name when it has none (never the folder's path)
    }
    fn sign_in_hint(&self) -> Option<&'static str> { Some("Sign in inside the Battle.net app (install it here with INSTALL LAUNCHER and press START LAUNCHER). SYNC LIBRARY then reads the games it has installed.") }
    fn auth_begin(&self) -> Result<AuthBegin, String> {
        Ok(AuthBegin { url: String::new(), placeholder: "Full path of the Wine prefix Battle.net is installed in".into(), instructions: vec![
            "Zest reads the Battle.net client's own files: it needs the folder (the Wine prefix) the client is installed in.".into(),
            "The folder contains drive_c. Typical places: ~/.local/share/Zest/battle_net, ~/.local/share/PlayDate/battle_net (from PlayDate) or a Lutris/Bottles prefix.".into(),
            "Log in once in the Battle.net client so it has written its product list, then paste the full path below.".into()] })
    }
    fn auth_complete(&self, value: &str) -> Result<AuthStatus, String> {
        let raw = value.trim();
        let path = if let Some(rest) = raw.strip_prefix("~/") { crate::installdir::home().join(rest) } else { PathBuf::from(raw) };
        if raw.is_empty() || !path.is_absolute() { return Err("Give the full path of the prefix folder (starting with / or ~/).".into()); }
        if !path.join("drive_c").is_dir() { return Err("That folder has no drive_c inside, so it is not a Wine prefix.".into()); }
        if !bn::agent_db_path(&path).is_file() { return Err("No Battle.net product list was found in it. Install the Battle.net client in this prefix and log in once.".into()); }
        crate::account::plugin_secret_set(&self.config_dir, "battle_net", "prefix", Some(&path.to_string_lossy())).map_err(|_| "Could not save the folder".to_string())?;
        self.auth_status()
    }
    fn account_login(&self) -> Option<AccountLogin> { Some(AccountLogin { label: "Battle.net account".into(), url: acct::login_url(), done: acct::login_done() }) }
    fn account_status(&self) -> Option<AuthStatus> {
        let Some(jar) = acct::saved_cookies(&self.config_dir) else { return Some(AuthStatus { connected: false, username: None }) };
        // offline reads as still connected: only a real "signed out" answer disconnects
        let ok = acct::Session::new(self.http.as_ref(), jar).check_connection().unwrap_or(true);
        Some(AuthStatus { connected: ok, username: ok.then(|| acct::saved_battletag(&self.config_dir).unwrap_or_else(|| "Battle.net Account".into())) })
    }
    fn account_complete(&self, value: &str) -> Result<Option<String>, String> { acct::save_login(self.http.as_ref(), &self.config_dir, value).map(Some) }
    fn account_disconnect(&self) -> Result<(), String> { acct::clear(&self.config_dir) }
    fn auth_disconnect(&self) -> Result<(), String> {
        crate::account::plugin_secret_set(&self.config_dir, "battle_net", "prefix", None).map_err(|_| "Could not save".to_string())?;
        crate::launcher_installer::forget_prefix(&self.config_dir, "battle_net");   // the older saved place too, or the folder would still be found
        Ok(())
    }
    fn launcher_folder(&self) -> Option<PathBuf> { self.prefix() }

    fn sync(&self, _cx: &SourceCtx, sink: &mut dyn FnMut(Vec<GameRecord>), cancel: &Cancel) -> Result<usize, String> {
        if cancel.is_set() { return Ok(0); }
        let found = if cfg!(windows) {
            // no prefix: the installed products only (the licence cache lives inside the client's own data and is not read)
            bn::parse_product_db(&Self::db_path(None)).into_iter().filter(|r| r.product_code != "agent" && r.product_code != "bna").map(|r| {
                let g = bn::by_uid(&r.uid).or_else(|| bn::by_code(&r.product_code));
                let code = g.as_ref().map(|g| g.code.to_string()).unwrap_or_else(|| r.product_code.clone());
                bn::Found { name: g.as_ref().map(|g| g.name.to_string()).unwrap_or_else(|| code.clone()), genres: g.as_ref().map(|g| g.genres.to_string()).unwrap_or_default(), uid: r.uid, code, installed: r.installed || r.playable, install_path: r.install_path, source: "installed" }
            }).collect()
        } else {
            match self.prefix().filter(|p| p.is_dir()) {
                Some(prefix) => bn::scan(&prefix),
                // no Wine prefix: a signed-in account alone still lists what you own (nothing is marked installed)
                None if acct::saved_cookies(&self.config_dir).is_some() => Vec::new(),
                None => return Err("Battle.net is not set up: sign in to your account (Plugins > Battle.net) or give its Wine prefix".into()),
            }
        };
        crate::net::log(&format!("Battle.net sync: {} games from local files", found.len()));
        // the signed-in account, when connected, is the authoritative list: it adds what is not installed and brings the real purchase dates
        let merged: Vec<(bn::Found, Option<i64>)> = match acct::saved_cookies(&self.config_dir) {
            Some(jar) if !cancel.is_set() => {
                let mut s = acct::Session::new(self.http.as_ref(), jar);
                s.warm_up();
                let owned = acct::scan_owned(&mut s);
                let _ = crate::account::plugin_secret_set(&self.config_dir, "battle_net", "web_cookies", Some(&serde_json::to_string(&s.jar).unwrap_or_default()));
                crate::net::log(&format!("Battle.net sync: account lists {} games", owned.len()));
                acct::merge(found, owned)
            }
            _ => found.into_iter().map(|f| (f, None)).collect(),
        };
        let records: Vec<GameRecord> = merged.iter().map(|(f, d)| GameRecord { date_added: *d, ..record(f) }).collect();
        let n = records.len();
        sink(records);
        Ok(n)
    }
    /// The Agent rewrites `product.db` in place under its data folder on every install or uninstall, so the whole subtree is watched.
    fn watch_paths(&self) -> Vec<(PathBuf, bool)> { self.prefix().map(|p| vec![(p.join("drive_c/ProgramData/Battle.net/Agent"), true)]).unwrap_or_default() }
    fn installed_scan(&self) -> Option<Vec<InstalledRow>> {
        let c = rusqlite::Connection::open_with_flags(&self.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
        let mut st = c.prepare("SELECT platform_id FROM games WHERE platform = 'battle_net' AND platform_id IS NOT NULL").ok()?;
        let ids: Vec<String> = st.query_map([], |r| r.get::<_, String>(0)).ok()?.flatten().collect();
        let found: Vec<bn::Found> = if cfg!(windows) { return None } else { bn::scan(&self.prefix().filter(|p| p.is_dir())?) };
        Some(ids.into_iter().map(|id| match found.iter().find(|f| f.code.eq_ignore_ascii_case(&id)) {
            Some(f) if f.installed => InstalledRow { installed: true, install_path: Some(f.install_path.clone()).filter(|p| !p.is_empty()), platform_id: id },
            _ => InstalledRow { installed: false, install_path: Some(String::new()), platform_id: id },
        }).collect())
    }
    fn launch(&self, g: &GameRef) -> Result<Launch, String> {
        let raw = g.platform_id.trim();
        if raw.is_empty() { return Err("Game has no product code: try syncing again".into()); }
        let code = bn::by_code(raw).map(|x| x.code.to_string()).unwrap_or_else(|| raw.to_string());
        let play = format!("--exec=launch {code}");
        if cfg!(windows) {
            // the installed client given the launch command, else the link, which only selects the game
            let native = ["PROGRAMFILES(X86)", "PROGRAMFILES", "PROGRAMW6432"].iter().filter_map(|e| std::env::var_os(e)).flat_map(|b| { let b = PathBuf::from(b); [b.join("Battle.net/Battle.net.exe"), b.join("Blizzard Entertainment/Battle.net/Battle.net.exe")] }).find(|p| p.is_file());
            return Ok(match (g.installed, native) {
                (true, Some(n)) => Launch { action: Action::Spawn { program: n.to_string_lossy().to_string(), args: vec![play], dir: None, env: vec![] }, record: true },
                _ => Launch { action: Action::OpenUrl(format!("battlenet://{code}")), record: g.installed },
            });
        }
        if cfg!(target_os = "macos") { return Ok(Launch { action: Action::OpenUrl(format!("battlenet://{code}")), record: g.installed }); }
        let prefix = self.prefix().filter(|p| p.is_dir()).ok_or("Battle.net is not set up. Install it from the Plugins dialog (INSTALL LAUNCHER) or give its Wine prefix.")?;
        let exe = super::rockstar::find_file(&prefix.join("drive_c"), "Battle.net.exe", 8).ok_or("Battle.net.exe not found in the prefix: reinstall the launcher.")?;
        let running = crate::wine::prefix_has_running_process(&prefix);
        if !g.installed {
            // `battlenet://<code>` only selects the game in the client: the user installs it there
            return Ok(Launch { record: false, action: if running {
                Action::Many(vec![Action::Message("Opening the game in Battle.net. Press Install there.".into()), crate::wine::session_action(Path::new("start"), &prefix, &[format!("battlenet://{code}")])?])
            } else {
                {
                let start = crate::wine::launch_action_args(&exe, &prefix, &[])?;
                let page = crate::wine::session_action(Path::new("start"), &prefix, &[format!("battlenet://{code}")])?;
                Action::Many(vec![Action::Message("Opening Battle.net. Sign in if it asks; the game's page opens once the client has loaded, then press Install.".into()), after_login(start, page, &prefix)])
            }
            } });
        }
        if running {
            // the client's window closes to the tray and the session stays alive: it is handed the launch command directly (a game running in it is left alone)
            if let Some(why) = game_running(&crate::wine::prefix_processes(&prefix)) { return Err(why); }
            return Ok(Launch { action: crate::wine::session_action(&exe, &prefix, &[play])?, record: true });
        }
        // `--exec` only works on a client that is already running and loaded: start it, wait for it, then send the command (PlayDate does the same)
        let start = crate::wine::launch_action_args(&exe, &prefix, &[])?;
        let go = crate::wine::session_action(&exe, &prefix, &[play])?;
        Ok(Launch { action: Action::Many(vec![Action::Message("Starting Battle.net. Sign in if it asks; the game starts once the client has loaded.".into()), after_login(start, go, &prefix)]), record: true })
    }
    /// NOT OFFERED (`Caps.install` is false, Rob 2026-11-16): the download starts but the client never tracks it, whatever order the page is opened in (builds 73 to 75). Zest opens the game's page instead (see `launch`) and the user presses Install there. Kept for reference. Installs a game through the running client's own helper (the Agent): the same two requests the client sends. The helper's address and key are read from the
    /// prefix; a client that is not running is started first. Linux only (elsewhere the client installs it). The game's page is then opened in the client: a client only
    /// notices an install it did not start when it looks at that page, after which it shows the download itself (tried with Heroes of the Storm).
    fn install(&self, _cx: &SourceCtx, g: &GameRef, progress: &mut dyn FnMut(&str, u64, u64), cancel: &Cancel) -> Result<super::Installed, String> {
        use std::time::{Duration, Instant};
        let prefix = self.prefix().filter(|p| p.is_dir()).ok_or("Battle.net is not set up. Install it from the Plugins dialog (INSTALL LAUNCHER) or give its Wine prefix.")?;
        let code = bn::by_code(g.platform_id.trim()).ok_or_else(|| format!("{} is not a game Zest can install from Battle.net yet", g.name))?;
        if !code.lic_ok && !code.free { return Err(format!("{} cannot be installed from here", g.name)); }
        let name = g.name.clone();
        // the helper only exists while the client runs
        let mut agent = find_agent(self.http.as_ref(), &prefix);
        if agent.is_none() {
            progress(&format!("Starting Battle.net to install {name}"), 0, 0);
            let exe = super::rockstar::find_file(&prefix.join("drive_c"), "Battle.net.exe", 8).ok_or("Battle.net.exe not found in the prefix: reinstall the launcher.")?;
            let start = crate::wine::launch_action_args(&exe, &prefix, &[])?;
            run_spawn(&start)?;
            let t = Instant::now();
            while agent.is_none() {
                if cancel.is_set() { return Err("Cancelled".into()); }
                if t.elapsed() > Duration::from_secs(120) { return Err("Battle.net did not start in time. Open it, sign in, and try again.".into()); }
                std::thread::sleep(Duration::from_secs(3));
                agent = find_agent(self.http.as_ref(), &prefix);
            }
        }
        let agent = agent.ok_or("Could not reach Battle.net's helper")?;
        let h = |a: &Agent| -> Vec<(&str, String)> { vec![("Authorization", a.token.clone()), ("Content-Type", "application/json".into())] };
        let call = |method_post: bool, path: &str, body: &str| -> Result<serde_json::Value, String> {
            let hd = h(&agent); let hr: Vec<(&str, &str)> = hd.iter().map(|(k, v)| (*k, v.as_str())).collect();
            let url = format!("http://127.0.0.1:{}{}", agent.port, path);
            let r = if method_post { self.http.post(&url, &hr, body, Duration::from_secs(30)) } else { self.http.get(&url, &hr, Duration::from_secs(15)) }.map_err(|e| format!("Battle.net's helper did not answer: {}", e.0))?;
            if r.status == 401 { return Err("Battle.net's helper refused the key (the client restarted?). Try again.".into()); }
            if r.status >= 400 { return Err(format!("Battle.net's helper answered HTTP {} for {path}", r.status)); }
            serde_json::from_str(&r.body).map_err(|_| "Battle.net's helper sent something unreadable".to_string())
        };
        // the client has to be fully up first: it starts at a login window when its saved sign-in is gone, and nothing it is told then is picked up properly
        let t = Instant::now();
        while !client_logged_in(&prefix) {
            if cancel.is_set() { return Err("Cancelled".into()); }
            if t.elapsed() > Duration::from_secs(600) { return Err("Battle.net did not finish loading. Sign in to it, then try again.".into()); }
            progress(&format!("Waiting for Battle.net to load (sign in to it if it asks) to install {name}"), 0, 0);
            std::thread::sleep(Duration::from_secs(2));
        }
        std::thread::sleep(Duration::from_secs(3));   // the window finishes drawing after the login
        // the page is opened right AFTER the install request (build 73/74 opened it first and waited: the client never took the download over, in either
        // Hearthstone run; the one run where it did, Heroes of the Storm, had the install land under a second before the page)
        let show = || { if let Ok(a) = crate::wine::session_action(Path::new("start"), &prefix, &[format!("battlenet://{}", code.code)]) { let _ = run_spawn(&a); } };
        progress(&format!("Asking Battle.net to install {name}"), 0, 0);
        let uid = code.uid;
        let first = call(true, "/install", &serde_json::json!({ "instructions_patch_url": patch_url(code.code), "instructions_product": "NGDP", "priority": { "insert_at_head": true, "value": 699 }, "uid": uid }).to_string())?;
        let form = &first["form"];
        let dir = form["game_dir"]["default"].as_str().zip(form["game_dir"]["dirname"].as_str()).map(|(d, n)| format!("{d}{n}\\")).ok_or("Battle.net did not say where to install the game")?;
        let need = form["game_dir"]["required_space"].as_u64().unwrap_or(0);
        crate::installdir::check_disk_space(&prefix.join("drive_c"), need)?;
        let langs: Vec<&str> = form["language"]["list"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect()).unwrap_or_default();
        let lang = if langs.contains(&"enUS") || langs.is_empty() { "enUS" } else { langs[0] };
        let choice = |finalized: bool| { let mut b = serde_json::json!({ "account_country": "USA", "additional_tags": [], "game_dir": dir, "geo_ip_country": "US", "language": [lang], "selected_asset_locale": lang, "selected_locale": lang }); if finalized { b["finalized"] = true.into(); } b.to_string() };
        call(true, &format!("/install/{uid}"), &choice(false))?;
        call(true, &format!("/install/{uid}"), &choice(true))?;
        let pickup_mark = client_log_mark(&prefix);
        show();
        let pickup_line = format!("Install started: {uid}");
        let (mut seen_by_client, mut nudged) = (false, Instant::now());
        let mut first_nudge = true;
        let other = bn::ROWS.iter().map(|r| r.0).find(|c| !c.eq_ignore_ascii_case(code.code)).unwrap_or("Pro").to_string();
        let show_other = || { if let Ok(a) = crate::wine::session_action(Path::new("start"), &prefix, &[format!("battlenet://{other}")]) { let _ = run_spawn(&a); } };
        // the download runs in the client; follow it
        let mut last_seen = Instant::now();
        loop {
            if cancel.is_set() { return Err("Cancelled here; Battle.net may keep downloading (cancel it in the client)".into()); }
            match call(false, &format!("/game/{uid}"), "") {
                Ok(v) => {
                    last_seen = Instant::now();
                    if v["installed"].as_bool() == Some(true) && v["playable"].as_bool() != Some(false) && v["download_complete"].as_bool() == Some(true) {
                        let dir = v["install_dir"].as_str().unwrap_or("");
                        let path = if dir.is_empty() { String::new() } else { win_to_prefix_path(dir, &prefix).to_string_lossy().to_string() };
                        crate::net::log(&format!("Battle.net: installed {name:?} -> {path}"));
                        show();   // once more at the end, so the client looks at the finished game too
                        return Ok(super::Installed { install_path: path, executable: None, note: None });
                    }
                    let p = v["update_progress"].as_f64().unwrap_or(0.0).clamp(0.0, 1.0);
                    if !seen_by_client { seen_by_client = client_log_saw(&pickup_mark, &pickup_line); }
                    // the client has not noticed the download yet: show the page again (once every 30 s)
                    // showing the page that is already showing does nothing (seen with Hearthstone: four repeats, no pickup), so go to another game's page and back
                    if !seen_by_client && nudged.elapsed() > Duration::from_secs(if first_nudge { 8 } else { 30 }) { first_nudge = false; show_other(); std::thread::sleep(Duration::from_secs(4)); show(); nudged = Instant::now(); }
                    progress(&format!("Downloading {name}: {}%{}", (p * 100.0).round() as u32, if seen_by_client { " (also shown in Battle.net)" } else { "" }), (p * 1000.0) as u64, 1000);
                }
                Err(e) => { if last_seen.elapsed() > Duration::from_secs(90) { return Err(e); } }
            }
            std::thread::sleep(Duration::from_secs(3));
        }
    }
    fn store_url(&self, _g: &GameRef) -> Option<String> { None }
    /// Battle.net has no uninstall link of its own: each installed game registers a normal Windows uninstall entry, whose command (Blizzard's uninstaller, which shows its own
    /// confirmation window) is run in the prefix. Found by install folder, then by name.
    fn uninstall(&self, g: &GameRef) -> Result<Action, String> {
        const NOT_FOUND: &str = "Could not find the uninstaller. Remove it from the Battle.net client instead.";
        if !g.installed { return Err("That game is not installed.".into()); }
        if cfg!(windows) { return crate::winuninstall::action(g).map_err(|_| NOT_FOUND.to_string()); }
        let prefix = self.prefix().filter(|p| p.is_dir()).ok_or("Battle.net is not set up.")?;
        let install = g.install_path.clone().unwrap_or_default();
        let (exe, args) = find_uninstall_command(&prefix, &install, &g.name).ok_or(NOT_FOUND)?;
        crate::wine::launch_action_restarting(&exe, &prefix, &args, &game_running)
    }
}

// ---------------------------------------------------------------- uninstall (port of the plugin's `uninstall.py`)

type Section = std::collections::HashMap<String, String>;
fn unescape(s: &str) -> String { s.replace("\\\\", "\u{0}").replace("\\\"", "\"").replace('\u{0}', "\\") }

/// The `Uninstall\...` sections of a Wine registry file (`system.reg`, `user.reg`): value name to value, quotes removed.
pub fn read_reg(text: &str) -> Vec<Section> {
    let mut out: Vec<Section> = vec![];
    let mut cur: Option<usize> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') { continue; }
        if line.starts_with('[') {
            if let Some(end) = line.find(']') {
                let key = unescape(&line[1..end]);
                cur = if key.contains("Uninstall\\") { out.push(Section::new()); Some(out.len() - 1) } else { None };
            }
            continue;
        }
        let Some(i) = cur else { continue };
        if !line.starts_with('"') { continue; }
        let rest = &line[1..];
        let Some(q) = rest.find("\"") else { continue };
        let (name, after) = (&rest[..q], rest[q + 1..].trim_start());
        let Some(val) = after.strip_prefix('=') else { continue };
        let val = val.trim();
        let val = if val.len() >= 2 && val.starts_with('"') && val.ends_with('"') { unescape(&val[1..val.len() - 1]) } else { val.to_string() };
        out[i].insert(unescape(name), val);
    }
    out
}
/// Blizzard's entries: Publisher "Blizzard Entertainment", or an uninstall command through Blizzard Uninstaller or Battle.net.exe.
pub fn blizzard_entries(sections: Vec<Section>) -> Vec<Section> {
    sections.into_iter().filter(|v| {
        let us = v.get("UninstallString").map(String::as_str).unwrap_or("");
        !us.is_empty() && (v.get("Publisher").map(String::as_str) == Some("Blizzard Entertainment") || us.contains("Blizzard Uninstaller") || us.contains("Battle.net.exe"))
    }).collect()
}
/// The entry for a game: by install location (either may be the tail of the other), else by display name.
pub fn pick_entry<'a>(entries: &'a [Section], install_path: &str, display_name: &str) -> Option<&'a Section> {
    let norm = |p: &str| p.replace('\\', "/").trim_end_matches('/').to_lowercase();
    let want = norm(install_path);
    for e in entries {
        let loc = norm(e.get("InstallLocation").map(String::as_str).unwrap_or(""));
        if !want.is_empty() && !loc.is_empty() && (loc == want || loc.ends_with(&want) || want.ends_with(&loc)) { return Some(e); }
    }
    let name = display_name.trim().to_lowercase();
    if name.is_empty() { return None; }
    entries.iter().find(|e| e.get("DisplayName").map_or(false, |d| d.trim().to_lowercase() == name))
}
/// `"C:\\...\\Blizzard Uninstaller.exe" --lang=enUS --uid=prometheus --displayname="Overwatch"` -> the program and its `--key=value` arguments (quotes around a value removed).
pub fn split_uninstall_string(us: &str) -> (String, Vec<String>) {
    let t = us.trim_start();
    let (exe, rest) = match t.strip_prefix('"').and_then(|r| r.find('"').map(|q| (r[..q].to_string(), r[q + 1..].to_string()))) {
        Some(x) => x,
        None => { let mut it = t.splitn(2, char::is_whitespace); (it.next().unwrap_or("").to_string(), it.next().unwrap_or("").to_string()) }
    };
    static R: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = R.get_or_init(|| regex::Regex::new(r#"--[\w-]+(?:=(?:"[^"]*"|[^\s]+))?"#).unwrap());
    let args = re.find_iter(&rest).map(|m| { let tok = m.as_str(); match tok.split_once('=') { Some((k, v)) => format!("{k}={}", v.trim_matches('"')), None => tok.to_string() } }).collect();
    (exe, args)
}
/// `C:\foo\bar.exe` -> `<prefix>/drive_c/foo/bar.exe`; anything that is not a drive path is returned as it is.
pub fn win_to_prefix_path(win: &str, prefix: &Path) -> PathBuf {
    let p = win.trim().trim_matches('"');
    let b = p.as_bytes();
    if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/') { prefix.join("drive_c").join(p[3..].replace('\\', "/")) } else { PathBuf::from(p) }
}
/// The uninstaller (program inside the prefix, arguments) of an installed game, from the prefix's registry files.
pub fn find_uninstall_command(prefix: &Path, install_path: &str, name: &str) -> Option<(PathBuf, Vec<String>)> {
    let mut sections = vec![];
    for f in ["system.reg", "user.reg"] { if let Ok(t) = std::fs::read_to_string(prefix.join(f)) { sections.extend(read_reg(&t)); } }
    let entries = blizzard_entries(sections);
    let e = pick_entry(&entries, install_path, name)?;
    let (exe, args) = split_uninstall_string(e.get("UninstallString")?);
    Some((win_to_prefix_path(&exe, prefix), args))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tmpdir(tag: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("zest_bnsrc_{tag}_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); std::fs::write(d.join("config.json"), "{\"other\": 1}").unwrap(); d }
    fn cx(d: &PathBuf) -> SourceCtx { let h: std::sync::Arc<dyn crate::net::Http> = std::sync::Arc::new(crate::net::UreqHttp::new()); SourceCtx { http: h.clone(), steam: std::sync::Arc::new(crate::steamclient::Steam::new(h, None, 10.0)), root: d.clone(), db: d.join("games.db"), today: "2026-11-03".into(), appinfo: std::sync::Arc::new(|_| Default::default()), account_id: None } }
    #[test] fn a_login_is_shown_partly_hidden() {
        assert_eq!(mask_login("robby@example.com"), "ro*****@example.com");
        assert_eq!(mask_login("ab@x.io"), "a*****@x.io");
        assert_eq!(mask_login("playername"), "pl*****");
    }
    #[cfg(not(windows))]
    #[test] fn the_prefix_is_checked_kept_and_synced_into_records() {
        let d = tmpdir("flow"); let s = BattleNetSource::new(d.clone(), d.join("games_1.db"), std::sync::Arc::new(crate::net::UreqHttp::new()));
        let fixture = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/bnet_prefix"));
        assert!(!s.auth_status().unwrap().connected && s.sync(&cx(&d), &mut |_| {}, &Cancel::new()).unwrap_err().contains("not set up"));
        assert!(s.auth_complete("relative/path").is_err() && s.auth_complete("").is_err());
        assert!(s.auth_complete(&d.to_string_lossy()).unwrap_err().contains("no drive_c"));
        std::fs::create_dir_all(d.join("drive_c")).unwrap();
        assert!(s.auth_complete(&d.to_string_lossy()).unwrap_err().contains("No Battle.net product list"));
        let st = s.auth_complete(&fixture.to_string_lossy()).unwrap();
        assert_eq!((st.connected, st.username), (true, None), "the fixture's client has no saved login");
        let cfg: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(d.join("config.json")).unwrap()).unwrap();
        assert_eq!((cfg["battle_net"]["prefix"].as_str(), cfg["other"].as_i64()), (Some(&*fixture.to_string_lossy()), Some(1)));
        let mut got = vec![]; let n = s.sync(&cx(&d), &mut |b| got.extend(b), &Cancel::new()).unwrap();
        assert_eq!(n, got.len()); got.sort_by(|a, b| a.platform_id.cmp(&b.platform_id));
        let ow = got.iter().find(|r| r.platform_id == "Pro").unwrap();
        assert_eq!((ow.name.as_str(), ow.installed, ow.install_path.as_deref(), ow.developers.as_deref(), ow.genres.as_deref()), ("Overwatch", Some(true), Some("C:/Games/Overwatch"), Some("Blizzard Entertainment"), Some("Action,FPS,Multiplayer")));
        let unknown = got.iter().find(|r| r.platform_id == "MYST").unwrap();
        assert_eq!((unknown.developers.as_deref(), unknown.genres.as_deref()), (None, None), "a product the catalog does not know is kept, without invented details");
        assert!(got.iter().all(|r| r.platform_id != "ODIN"), "Call of Duty is only imported when installed");
        s.auth_disconnect().unwrap(); assert!(!s.auth_status().unwrap().connected);
    }

    /// account.battle.net as a fake: Diablo IV (not in the fixture prefix) owned and bought once, Overwatch (installed in it) owned and free.
    struct Acct;
    impl crate::net::Http for Acct {
        fn get(&self, _: &str, _: &[(&str, &str)], _: std::time::Duration) -> Result<crate::net::Response, crate::net::NetError> { Err(crate::net::NetError("unused".into())) }
        fn get_raw(&self, url: &str, _: &[(&str, &str)], _: std::time::Duration) -> Result<crate::net::Raw, crate::net::NetError> {
            let body = match url.strip_prefix("https://account.battle.net").unwrap_or("") {
                "/api/games-and-subs" => r#"{"gameAccounts":[{"titleId":5272175},{"titleId":4613486}]}"#,
                "/api/transactions" => r#"{"purchases":[{"productTitle":"Diablo IV","date":1690000000000}]}"#,
                "/api/" => r#"{"authenticated":true}"#, "/api/details" => r#"{"battleTag":"Rob#1"}"#, _ => "{}" };
            Ok(crate::net::Raw { status: 200, headers: vec![], body: body.into() })
        }
    }
    #[cfg(not(windows))]
    #[cfg(not(windows))]
    #[test] fn an_account_alone_syncs_without_a_wine_prefix() {
        let d = tmpdir("noprefix"); let s = BattleNetSource::new(d.clone(), d.join("games_1.db"), std::sync::Arc::new(Acct));
        assert!(s.sync(&cx(&d), &mut |_| {}, &Cancel::new()).unwrap_err().contains("not set up"));
        s.account_complete(r#"{"BA-tassadar":"s"}"#).unwrap();
        let mut got = vec![]; s.sync(&cx(&d), &mut |b| got.extend(b), &Cancel::new()).unwrap();
        let fen = got.iter().find(|r| r.platform_id == "Fen").expect("owned game added with no prefix");
        assert_eq!((fen.installed, fen.date_added), (Some(false), Some(1_690_000_000)));
    }
    // install state comes from the fixture Wine prefix here; on Windows from the real Battle.net install
    #[cfg(not(windows))]
    #[test] fn a_connected_account_adds_owned_games_and_their_purchase_dates_to_the_sync() {
        let d = tmpdir("acct"); let s = BattleNetSource::new(d.clone(), d.join("games_1.db"), std::sync::Arc::new(Acct));
        let fixture = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/bnet_prefix"));
        s.auth_complete(&fixture.to_string_lossy()).unwrap();
        assert_eq!(s.account_status(), Some(AuthStatus { connected: false, username: None }));
        let mut before = vec![]; s.sync(&cx(&d), &mut |b| before.extend(b), &Cancel::new()).unwrap();
        assert!(before.iter().all(|r| r.platform_id != "Fen" && r.date_added.is_none()), "no account: local files only");
        assert_eq!(s.account_complete(r#"{"BA-tassadar":"s"}"#).unwrap().as_deref(), Some("Rob#1"));
        assert_eq!(s.account_status(), Some(AuthStatus { connected: true, username: Some("Rob#1".into()) }));
        let mut got = vec![]; s.sync(&cx(&d), &mut |b| got.extend(b), &Cancel::new()).unwrap();
        let fen = got.iter().find(|r| r.platform_id == "Fen").expect("the account-only game is added");
        assert_eq!((fen.name.as_str(), fen.installed, fen.date_added), ("Diablo IV", Some(false), Some(1_690_000_000)));
        let pro = got.iter().find(|r| r.platform_id == "Pro").unwrap();
        assert_eq!((pro.installed, pro.date_added), (Some(true), None), "install state stays local; a free title has no purchase date");
        s.account_disconnect().unwrap(); assert_eq!(s.account_status(), Some(AuthStatus { connected: false, username: None }));
        assert!(s.auth_status().unwrap().connected, "the local setup is untouched");
    }

    // Linux only: on a Mac the native Battle.net app is opened through its link instead
    #[cfg(target_os = "linux")]
    #[test] fn launch_and_the_scan_follow_the_prefix() {
        let d = tmpdir("launch"); let db = d.join("games_1.db");
        let s = BattleNetSource::new(d.clone(), db.clone(), std::sync::Arc::new(crate::net::UreqHttp::new()));
        let fixture = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/bnet_prefix"));
        let pfx = d.join("pfx"); fn copy(a: &Path, b: &Path) { std::fs::create_dir_all(b).unwrap(); for e in std::fs::read_dir(a).unwrap().flatten() { let t = b.join(e.file_name()); if e.path().is_dir() { copy(&e.path(), &t) } else { std::fs::copy(e.path(), t).unwrap(); } } }
        copy(fixture, &pfx);
        let g = GameRef { appid: -1, name: "Overwatch".into(), platform: "battle_net".into(), platform_id: "Pro".into(), installed: true, ..Default::default() };
        assert!(s.launch(&g).unwrap_err().contains("not set up"));
        s.auth_complete(&pfx.to_string_lossy()).unwrap();
        crate::wine::TEST_RUNTIME.with(|t| *t.borrow_mut() = Some(Ok(crate::wine::Runtime::Wine { wine: "/usr/bin/wine".into() })));
        assert!(s.launch(&g).unwrap_err().contains("Battle.net.exe not found"));
        let ex = pfx.join("drive_c/Program Files (x86)/Battle.net"); std::fs::create_dir_all(&ex).unwrap(); std::fs::write(ex.join("Battle.net.exe"), "x").unwrap();
        // no client running: it is started, then (a shell waits for it to load) given the launch command
        let l = s.launch(&g).unwrap(); assert!(l.record);
        match &l.action { Action::Many(v) => { assert!(matches!(&v[0], Action::Message(m) if m.contains("Starting Battle.net"))); match &v[1] { Action::Spawn { program, args, .. } => { assert_eq!(program, "sh"); assert!(args[1].contains("--exec=launch Pro") && args[1].contains("Logged into Battle.net successfully") && !args[1].contains("sleep 15"), "{}", args[1]); } a => panic!("{a:?}") } } a => panic!("{a:?}") }
        // not installed and no client running: the client is started and, once it is signed in, shown the game's page
        let l = s.launch(&GameRef { installed: false, ..g.clone() }).unwrap(); assert!(!l.record);
        match &l.action { Action::Many(v) => { assert!(matches!(&v[0], Action::Message(m) if m.contains("Press Install") || m.contains("press Install"))); match &v[1] { Action::Spawn { args, .. } => assert!(args[1].contains("battlenet://Pro") && args[1].contains("Logged into Battle.net successfully"), "{}", args[1]), a => panic!("{a:?}") } } a => panic!("{a:?}") }
        assert!(s.launch(&GameRef::default()).is_err());
        // the scan: the product database says what is installed
        rusqlite::Connection::open(&db).unwrap().execute_batch("CREATE TABLE games (platform TEXT, platform_id TEXT); INSERT INTO games VALUES ('battle_net','Pro'),('battle_net','MYST'),('battle_net','ZZZ');").unwrap();
        let rows = s.installed_scan().unwrap();
        assert_eq!(rows.iter().map(|r| (r.platform_id.as_str(), r.installed)).collect::<Vec<_>>(), vec![("Pro", true), ("MYST", true), ("ZZZ", false)]);
        assert_eq!(rows[0].install_path.as_deref(), Some("C:/Games/Overwatch"));
    }
}

#[cfg(test)]
mod uninstall_tests {
    use super::*;
    const REG: &str = r#"WINE REGISTRY Version 2
[Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Overwatch] 1700000000
"DisplayName"="Overwatch"
"InstallLocation"="C:\\Program Files (x86)\\Overwatch"
"Publisher"="Blizzard Entertainment"
"UninstallString"="\"C:\\ProgramData\\Battle.net\\Agent\\Blizzard Uninstaller.exe\" --lang=enUS --uid=prometheus --displayname=\"Overwatch\""

[Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Other] 1700000001
"DisplayName"="Some Other App"
"Publisher"="Someone Else"
"UninstallString"="C:\\other\\unins.exe /S"

[Software\\Wine\\Unrelated]
"DisplayName"="Not an uninstall key"
"#;
    #[test] fn the_blizzard_entry_is_found_by_folder_then_by_name() {
        let entries = blizzard_entries(read_reg(REG));
        assert_eq!(entries.len(), 1, "only Blizzard's entry, not other apps or non-Uninstall keys");
        assert_eq!(pick_entry(&entries, "C:/Program Files (x86)/Overwatch", "").and_then(|e| e.get("DisplayName")).map(String::as_str), Some("Overwatch"));
        assert!(pick_entry(&entries, "/somewhere/else", "").is_none());
        assert!(pick_entry(&entries, "", "overwatch ").is_some(), "by name when there is no folder");
    }
    #[test] fn the_uninstall_command_is_split_into_program_and_arguments() {
        let (exe, args) = split_uninstall_string(r#""C:\ProgramData\Battle.net\Agent\Blizzard Uninstaller.exe" --lang=enUS --uid=prometheus --displayname="Overwatch""#);
        assert_eq!(exe, r"C:\ProgramData\Battle.net\Agent\Blizzard Uninstaller.exe");
        assert_eq!(args, vec!["--lang=enUS", "--uid=prometheus", "--displayname=Overwatch"]);
        assert_eq!(split_uninstall_string("tool.exe --x").0, "tool.exe");
    }
    #[test] fn a_windows_path_lands_inside_the_prefix() {
        assert_eq!(win_to_prefix_path(r"C:\ProgramData\x\u.exe", Path::new("/p")), PathBuf::from("/p/drive_c/ProgramData/x/u.exe"));
        assert_eq!(win_to_prefix_path("/usr/bin/x", Path::new("/p")), PathBuf::from("/usr/bin/x"));
    }
    #[test] fn the_command_comes_from_the_prefixs_registry_files() {
        let d = std::env::temp_dir().join(format!("zest_bn_unreg_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("system.reg"), REG).unwrap();
        let (exe, args) = find_uninstall_command(&d, "C:\\Program Files (x86)\\Overwatch", "Overwatch").unwrap();
        assert_eq!(exe, d.join("drive_c/ProgramData/Battle.net/Agent/Blizzard Uninstaller.exe"));
        assert_eq!(args.len(), 3);
        assert!(find_uninstall_command(&d, "/nope", "Nothing").is_none());
        let _ = std::fs::remove_dir_all(&d);
    }
}

/// Whether a process of the prefix is a game rather than the client or Wine itself: ending the session would kill it.
fn game_running(procs: &[(u32, String)]) -> Option<String> {
    const OWN: [&str; 21] = ["battle.net", "agent", "blizzard", "services.exe", "winedevice", "plugplay", "svchost", "rpcss", "explorer", "conhost", "start.exe", "wineboot", "crashreport", "winedbg", "tabtip", "wineserver", "umu.exe", "umu-run", "\\windows\\", "proton", "pressure-vessel"];
    procs.iter().find(|(_, a)| { let l = a.to_lowercase(); (l.ends_with(".exe") || l.contains('\\')) && !OWN.iter().any(|o| l.contains(o)) })
        .map(|(_, a)| format!("{} is running in the Battle.net prefix. Close it first.", a.rsplit(['\\', '/']).next().unwrap_or(a)))
}

fn sq(s: &str) -> String { format!("'{}'", s.replace('\'', "'\\''")) }
/// One shell line that runs a `Spawn` action (its folder, environment and program).
fn spawn_line(a: &Action) -> Option<String> {
    let Action::Spawn { program, args, dir, env } = a else { return None };
    let mut l = String::new();
    if let Some(d) = dir { l.push_str(&format!("cd {} && ", sq(d))); }
    l.push_str("env");
    // `env` takes its options (-u) before the NAME=VALUE pairs: after one, `-u` would be read as the program
    for (k, v) in env { if v.is_none() { l.push_str(&format!(" -u {k}")); } }
    for (k, v) in env { if let Some(v) = v { l.push_str(&format!(" {}={}", k, sq(v))); } }
    l.push_str(&format!(" {}", sq(program)));
    for x in args { l.push_str(&format!(" {}", sq(x))); }
    Some(l)
}
/// `first` now, `second` after `secs` seconds, as one background shell (the client needs time to load before it takes a command).
/// Starts `first` (the client) and sends `second` as soon as the client's own log says it is signed in and showing its home page (the log has no separate line for the page:
/// `Logged into Battle.net successfully` is the last thing before it draws). Only a log written after this started counts, so a leftover from an earlier run cannot fire it.
/// Gives up waiting after 5 minutes (nobody signed in) and sends anyway, which the client then ignores or queues.
fn after_login(first: Action, second: Action, prefix: &Path) -> Action {
    match (spawn_line(&first), spawn_line(&second)) {
        (Some(a), Some(b)) => {
            let logs = format!("{}/drive_c/users/*/AppData/Local/Battle.net/Logs/battle.net-*.log", sq(&prefix.to_string_lossy()));
            let wait = format!("t0=$(date +%s); ({a}) & i=0; while [ $i -lt {LOGIN_WAIT_SECS} ]; do f=$(ls -t {logs} 2>/dev/null | head -1); if [ -n \"$f\" ] && [ \"$(stat -c %Y \"$f\" 2>/dev/null || echo 0)\" -ge \"$t0\" ] && grep -q '{LOGIN_LINE}' \"$f\"; then break; fi; sleep 1; i=$((i+1)); done; {b}");
            Action::Spawn { program: "sh".into(), args: vec!["-c".into(), wait], dir: None, env: vec![] }
        }
        _ => Action::Many(vec![first]),
    }
}
const LOGIN_LINE: &str = "Logged into Battle.net successfully";
const LOGIN_WAIT_SECS: u32 = 300;

#[cfg(test)]
mod launch_script_tests {
    use super::*;
    #[test] fn a_login_launch_is_one_shell_that_starts_the_client_then_waits_for_its_login_line_then_the_command_with_each_programs_own_environment() {
        let a = Action::Spawn { program: "/u/umu-run".into(), args: vec!["/p/Battle.net.exe".into()], dir: Some("/p".into()), env: vec![("WINEPREFIX".into(), Some("/p x".into())), ("DISPLAY".into(), None)] };
        let b = Action::Spawn { program: "/usr/bin/wine".into(), args: vec!["/p/Battle.net.exe".into(), "--exec=launch Pro".into()], dir: None, env: vec![("WINEPREFIX".into(), Some("/p x".into()))] };
        match after_login(a, b, Path::new("/p x")) {
            Action::Spawn { program, args, .. } => {
                assert_eq!(program, "sh");
                assert_eq!(args[0], "-c");
                let sc = &args[1];
                assert!(sc.contains("(cd '/p' && env -u DISPLAY WINEPREFIX='/p x' '/u/umu-run' '/p/Battle.net.exe') &"), "{sc}");
                assert!(sc.contains("'/p x'/drive_c/users/*/AppData/Local/Battle.net/Logs/battle.net-*.log"), "{sc}");
                assert!(sc.contains("grep -q 'Logged into Battle.net successfully'"), "{sc}");
                assert!(sc.ends_with("done; env WINEPREFIX='/p x' '/usr/bin/wine' '/p/Battle.net.exe' '--exec=launch Pro'"), "{sc}");
                assert!(!sc.contains("sleep 15"), "no fixed wait");
            }
            o => panic!("{o:?}"),
        }
        assert_eq!(sq("it's"), "'it'\\''s'");
    }
    #[test] fn only_a_real_game_stops_the_client_session_from_being_ended() {
        let own = vec![(1, "C:\\windows\\system32\\services.exe".to_string()), (2, "C:\\Program Files (x86)\\Battle.net\\Battle.net.exe".to_string()), (3, "C:\\ProgramData\\Battle.net\\Agent\\Agent.exe".to_string())];
        let mut own = own; own.push((4, "c:\\windows\\system32\\umu.exe".to_string()));
        assert_eq!(game_running(&own), None);
        let with_game = vec![(1, "C:\\windows\\system32\\services.exe".to_string()), (9, "C:\\Program Files (x86)\\Hearthstone\\Hearthstone.exe".to_string())];
        assert!(game_running(&with_game).unwrap().contains("Hearthstone.exe"));
    }
}

/// The client's newest log file and how long it is now: what is written after this is "since".
fn client_log_mark(prefix: &Path) -> (Option<PathBuf>, u64) {
    let logs = prefix.join("drive_c/users");
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    if let Ok(users) = std::fs::read_dir(&logs) { for u in users.flatten() { if let Ok(rd) = std::fs::read_dir(u.path().join("AppData/Local/Battle.net/Logs")) { for f in rd.flatten() {
        let n = f.file_name().to_string_lossy().to_string();
        if n.starts_with("battle.net-") && n.ends_with(".log") { if let Ok(m) = f.metadata().and_then(|m| m.modified()) { if best.as_ref().map_or(true, |b| m > b.0) { best = Some((m, f.path())); } } }
    } } } }
    match best { Some((_, p)) => { let len = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0); (Some(p), len) } None => (None, 0) }
}
/// Whether the client's log has gained a line holding `needle` since `mark` (a client restarted meanwhile is read from its start).
fn client_log_saw(mark: &(Option<PathBuf>, u64), needle: &str) -> bool {
    let prefix_guess = mark.0.as_ref().and_then(|p| p.ancestors().nth(6).map(|a| a.to_path_buf()));   // .../drive_c/users/<u>/AppData/Local/Battle.net/Logs/<file> -> .../drive_c
    let _ = prefix_guess;
    let Some(path) = mark.0.as_ref() else { return false };
    let Ok(bytes) = std::fs::read(path) else { return false };
    let from = (mark.1 as usize).min(bytes.len());
    String::from_utf8_lossy(&bytes[from..]).contains(needle)
}

/// Whether the running client has finished signing in: its newest log's last login line is a success (a failure after it, a saved sign-in that expired, undoes it).
fn client_logged_in(prefix: &Path) -> bool {
    let Some(path) = client_log_mark(prefix).0 else { return false };
    let Ok(bytes) = std::fs::read(&path) else { return false };
    logged_in_in(&String::from_utf8_lossy(&bytes))
}
fn logged_in_in(log: &str) -> bool {
    let ok = log.rfind("Logged into Battle.net successfully");
    let bad = ["Login failed.", "Handling error from service"].iter().filter_map(|b| log.rfind(b)).max();
    match (ok, bad) { (Some(o), Some(b)) => o > b, (Some(_), None) => true, _ => false }
}

/// The client's helper (the Agent): its port and its key.
struct Agent { port: u16, token: String }
/// Product segment of the version server's address (`https://us.version.battle.net/<segment>`): the lower-case code, Hearthstone's being `hsb`.
fn patch_url(code: &str) -> String { let seg = if code.eq_ignore_ascii_case("WTCG") { "hsb".to_string() } else { code.to_lowercase() }; format!("https://us.version.battle.net/{seg}") }
/// The key the helper wrote into its newest log (`"authorization": "<digits>"`).
fn agent_token(log: &str) -> Option<String> {
    let at = log.find("\"authorization\"")?;
    let rest = &log[at + "\"authorization\"".len()..];
    let q1 = rest.find('"')? + 1; let q2 = rest[q1..].find('"')? + q1;
    Some(rest[q1..q2].to_string()).filter(|t| !t.is_empty() && t.chars().all(|c| c.is_ascii_digit()))
}
/// Ports something listens on at 127.0.0.1 (from a `/proc/net/tcp` listing).
fn local_listening_ports(tcp: &str) -> Vec<u16> {
    tcp.lines().skip(1).filter_map(|l| { let f: Vec<&str> = l.split_whitespace().collect(); if f.get(3) != Some(&"0A") { return None; } let (ip, port) = f.get(1)?.split_once(':')?; if ip != "0100007F" { return None; } u16::from_str_radix(port, 16).ok() }).collect()
}
fn find_agent(http: &dyn crate::net::Http, prefix: &Path) -> Option<Agent> {
    let base = prefix.join("drive_c/ProgramData/Battle.net/Agent");
    // the newest helper log (each start writes a new one, with a new key)
    let mut logs: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for d in std::fs::read_dir(&base).ok()?.flatten() { if let Ok(rd) = std::fs::read_dir(d.path().join("Logs")) { for f in rd.flatten() { let n = f.file_name().to_string_lossy().to_string(); if n.starts_with("Agent-") && n.ends_with(".log") { if let Ok(m) = f.metadata().and_then(|m| m.modified()) { logs.push((m, f.path())); } } } } }
    logs.sort();
    let token = agent_token(&std::fs::read_to_string(&logs.last()?.1).ok()?)?;
    for port in local_listening_ports(&std::fs::read_to_string("/proc/net/tcp").ok()?) {
        let hd = [("Authorization", token.as_str())];
        if let Ok(r) = http.get(&format!("http://127.0.0.1:{port}/game"), &hd, std::time::Duration::from_secs(3)) { if r.status == 200 && r.body.contains("battle.net") { return Some(Agent { port, token }); } }
    }
    None
}
/// Starts a `Spawn` action and leaves it running (its output is ignored; a thread reaps it).
fn run_spawn(a: &Action) -> Result<(), String> {
    let Action::Spawn { program, args, dir, env } = a else { return Err("nothing to start".into()) };
    let mut c = std::process::Command::new(program);
    c.args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    if let Some(d) = dir { c.current_dir(d); }
    for (k, v) in env { match v { Some(v) => { c.env(k, v); } None => { c.env_remove(k); } } }
    let mut child = c.spawn().map_err(|e| format!("Could not start {program}: {e}"))?;
    std::thread::spawn(move || { let _ = child.wait(); });
    Ok(())
}

#[cfg(test)]
mod install_tests {
    use super::*;
    #[test] fn the_version_server_address_follows_the_product_code() {
        assert_eq!(patch_url("WTCG"), "https://us.version.battle.net/hsb");
        assert_eq!(patch_url("Pro"), "https://us.version.battle.net/pro");
        assert_eq!(patch_url("ODIN"), "https://us.version.battle.net/odin");
    }
    #[test] fn the_helpers_key_is_read_from_its_log() {
        assert_eq!(agent_token("[I] Request GET /agent \nResponse 200: {\n  \"authorization\": \"4541012809758235045\",\n  \"pid\": 444\n}").as_deref(), Some("4541012809758235045"));
        assert_eq!(agent_token("no key here"), None);
        assert_eq!(agent_token("\"authorization\": \"abc\""), None, "only digits are a key");
    }
    /// Needs a running Battle.net in the default prefix: `cargo test -p pd-filter live_agent -- --ignored`.
    #[test] #[ignore] fn live_agent_is_found_and_answers() {
        let prefix = crate::installdir::home().join(".local/share/PlayDate/battle_net");
        let a = find_agent(&crate::net::UreqHttp::new(), &prefix).expect("the helper is found");
        assert!(a.port > 0 && !a.token.is_empty());
    }
    #[test] fn the_clients_log_is_read_from_where_it_was_when_the_mark_was_taken() {
        let d = std::env::temp_dir().join(format!("zest_bnlog_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d);
        let logs = d.join("drive_c/users/steamuser/AppData/Local/Battle.net/Logs"); std::fs::create_dir_all(&logs).unwrap();
        let f = logs.join("battle.net-20261116T000000.log"); std::fs::write(&f, "old line: Handling URI: battlenet://Hero\n").unwrap();
        let mark = client_log_mark(&d);
        assert!(!client_log_saw(&mark, "Handling URI: battlenet://Hero"), "what was there before the mark does not count");
        let mut t = std::fs::read_to_string(&f).unwrap(); t.push_str("[UriController] Handling URI: battlenet://Hero\n"); std::fs::write(&f, t).unwrap();
        assert!(client_log_saw(&mark, "Handling URI: battlenet://Hero"));
        assert!(!client_log_saw(&mark, "Install started: heroes"));
    }
    #[test] fn the_client_counts_as_loaded_only_after_its_last_login_line_is_a_success() {
        assert!(!logged_in_in("starting\n"));
        assert!(!logged_in_in("E [BNLogin] Login failed. error=ERROR_TOKEN_NOT_FOUND (49)\n"), "the login window is up");
        assert!(logged_in_in("E Login failed. error=1\nI [BNLogin] Logged into Battle.net successfully. |bnet=1\n"));
        assert!(!logged_in_in("I Logged into Battle.net successfully.\nI Handling error from service. error=ERROR_TOKEN_NOT_FOUND\n"), "signed out again");
    }
    #[test] fn only_loopback_listeners_are_candidates() {
        let tcp = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   0: 0100007F:A0D7 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 1 1 1\n   1: 00000000:1538 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 2 1 1\n   2: 0100007F:5955 0100007F:C0DE 01 00000000:00000000 00:00000000 00000000  1000        0 3 1 1\n";
        assert_eq!(local_listening_ports(tcp), vec![0xA0D7]);
    }
}
