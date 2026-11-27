//! Library sources: what Zest asks of a store (Steam, GOG, Epic, ...) behind one interface. Built-in sources implement `LibrarySource` directly; a third-party
//! plugin is an adapter that implements the same trait over the plugin protocol (design: `NATIVE_UI_PLAN.md` sections 18 and 18b). Core code looks a game's
//! source up in the `Registry` by its `platform` and never names a store itself, so adding a store means adding one implementation and registering it.
use crate::jobs::Update;
use crate::metadata::{Hooks, PageMeta};
use crate::net::Http;
use crate::steamclient::{AppinfoDates, Fetch, Steam};
use std::path::PathBuf;
use std::sync::Arc;

pub mod amazon_games;
pub mod battle_net;
pub mod ea_app;
pub mod ea_manifest;
pub mod epic;
pub mod folder;
pub mod gog;
pub mod gog_install;
pub mod gog_launch;
pub mod humble;
pub mod humble_site;
pub mod indiegala;
pub mod itch_io;
pub mod legacy_games;
pub mod nintendo;
pub mod playstation;
pub mod rockstar;
pub mod steam;
pub mod ubisoft;
pub mod xbox;
pub mod xbox_nexus;

/// The part of a `games` row a source needs to know about one game.
#[derive(Clone, Debug, Default)]
pub struct GameRef { pub appid: i64, pub name: String, pub platform: String, pub platform_id: String, pub slug: String, pub appname: String, pub installed: bool, pub install_path: Option<String>,
    /// The file to run inside the install folder (`platform_executable`), relative to it, when one was chosen.
    pub executable: Option<String> }

/// What a source can do. The UI shows only the buttons and menu entries a source supports.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Caps {
    pub auth: bool, pub sync: bool, pub rescrape: bool, pub launch: bool, pub install: bool, pub uninstall: bool, pub installed_scan: bool,
    pub description: bool, pub art: bool, pub store_page_metadata: bool, pub store_page: bool, pub achievements_page: bool, pub community_page: bool,
    /// `purchase_dates` works (the store's account says when each game was bought).
    pub purchase_dates: bool,
}

/// A finished install: where the game is, and the file to run if the source knows it.
/// One way a game can be downloaded (a platform's build): what the install is told to take (`key`) and what the person is shown (`label`).
#[derive(Clone, Debug, PartialEq)]
pub struct InstallOption { pub key: String, pub label: String }
/// The choice the person made for a game's next install (set by the app just before the install starts, taken by the install).
/// The program picked for one launch of a game that holds several (a pack of games): used once.
static LAUNCH_CHOICE: std::sync::Mutex<Vec<(i64, String)>> = std::sync::Mutex::new(Vec::new());
pub fn set_launch_choice(appid: i64, rel: &str) { if let Ok(mut v) = LAUNCH_CHOICE.lock() { v.retain(|(a, _)| *a != appid); v.push((appid, rel.to_string())); } }
pub fn take_launch_choice(appid: i64) -> Option<String> { LAUNCH_CHOICE.lock().ok().and_then(|mut v| v.iter().position(|(a, _)| *a == appid).map(|i| v.remove(i).1)) }
static INSTALL_CHOICE: std::sync::Mutex<Vec<(i64, String)>> = std::sync::Mutex::new(Vec::new());
pub fn set_install_choice(appid: i64, key: &str) { if let Ok(mut v) = INSTALL_CHOICE.lock() { v.retain(|(a, _)| *a != appid); v.push((appid, key.to_string())); } }
pub fn take_install_choice(appid: i64) -> Option<String> { INSTALL_CHOICE.lock().ok().and_then(|mut v| v.iter().position(|(a, _)| *a == appid).map(|i| v.remove(i).1)) }

#[derive(Clone, Debug, PartialEq)]
pub struct Installed { pub install_path: String, pub executable: Option<String>, /// Something to tell the user (the files are there but Zest could not unpack them).
    pub note: Option<String> }

/// One game's install state found by `installed_scan`: `install_path` Some(p) sets the folder (an empty p clears it), None leaves it.
#[derive(Clone, Debug, PartialEq)]
pub struct InstalledRow { pub platform_id: String, pub installed: bool, pub install_path: Option<String> }

#[derive(Clone, Debug, PartialEq)]
pub struct AuthStatus { pub connected: bool, pub username: Option<String> }
/// A second, optional sign-in on top of the first (Battle.net: the account website, beside the local client files). Only the sign-in window can do it.
#[derive(Clone, Debug, PartialEq)]
pub struct AccountLogin { /// For example "Battle.net account".
    pub label: String, pub url: String, /// What the sign-in window waits for (the helper's `done`).
    pub done: serde_json::Value }
/// How to sign in: open `url` in the browser, show the steps, take a pasted value (a code, an address, a cookie).
#[derive(Clone, Debug, PartialEq)]
pub struct AuthBegin { pub url: String, pub instructions: Vec<String>, pub placeholder: String }

/// Something for the app to do on a source's behalf (the source never opens windows or spawns processes itself, so it stays testable).
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    OpenUrl(String),
    /// Start a program (in `dir` when given); the app reports it if the program dies within a second.
    /// `env` sets variables (`Some`) or removes them (`None`) for the program only.
    Spawn { program: String, args: Vec<String>, dir: Option<String>, env: Vec<(String, Option<String>)> },
    Message(String),
    /// Several steps in order (open a folder and a web page, say).
    Many(Vec<Action>),
    /// The game's files are removed (`delete`) or only forgotten (the folder is outside the games folder, so it is left alone), then the game is marked not installed.
    RemoveInstall { dir: String, delete: bool },
    /// Several files in the game's folder could start it and nothing says which: the app asks, saves the answer as the game's executable and launches again. `candidates` are relative to `dir`.
    PickExecutable { dir: String, candidates: Vec<String> },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Launch { pub action: Action, /// Record a launch (last played = now, Never Played becomes Unfinished) when the game was installed.
    pub record: bool }

/// What a source may use while it works. Built once per job.
pub struct SourceCtx {
    pub http: Arc<dyn Http>, pub steam: Arc<Steam>, pub root: PathBuf, pub db: PathBuf, pub today: String,
    pub appinfo: Arc<dyn Fn(i64) -> AppinfoDates + Send + Sync>,
    /// The Steam account id (the data folder's `games_<id>.db`), for links into a profile.
    pub account_id: Option<String>,
}

/// One game the deep scan flagged: the library row and why.
#[derive(Clone, Debug, PartialEq)]
pub struct JunkHit { pub appid: i64, pub name: String, pub reason: String }

pub trait LibrarySource: Send + Sync {
    /// The value of the `platform` column.
    fn id(&self) -> &str;
    fn label(&self) -> &str;
    fn caps(&self) -> Caps;

    /// Sign-in (only for a source whose `caps().auth` is true). A built-in source keeps its token where PlayDate's plugin did (`config.json[platform]`).
    fn auth_status(&self) -> Result<AuthStatus, String> { Err(format!("{} has no sign-in", self.label())) }
    /// When this store's login page can be shown in `zest-login`'s window: what finishes the sign-in (the helper's `done`). What the window returns is what `auth_complete` takes from the
    /// paste box. None = the paste flow only (the store needs steps a window cannot follow, or its bot check refuses an embedded browser).
    fn web_login_done(&self) -> Option<serde_json::Value> { None }
    /// Does the plugin row still offer the paste box (SIGN IN IN BROWSER) beside the sign-in window? A store whose window has been shown to work says false; the paste flow is
    /// still what SIGN IN falls back to when the helper program is not installed (a Flatpak), and for every store whose window is not yet confirmed.
    fn paste_login_kept(&self) -> bool { true }
    /// Where the sign-in window starts when that is not `auth_begin`'s address (the paste flow's).
    fn web_login_url(&self) -> Option<String> { None }
    /// The folder (a Wine prefix on Linux) of the launcher this store reads its games from, when it has one set.
    fn launcher_folder(&self) -> Option<std::path::PathBuf> { None }
    /// A store with no sign-in of its own to press (it is done inside its launcher): the SIGN IN button is shown but cannot be pressed, and this is what it says.
    fn sign_in_hint(&self) -> Option<&'static str> { None }
    /// The browser name the sign-in window gives the store, when the store's bot check refuses the embedded browser's own (None = the default).
    fn web_login_agent(&self) -> Option<&'static str> { None }
    /// A second sign-in this source can use besides `auth_*` (None = none). Cheap: no file or network access.
    fn account_login(&self) -> Option<AccountLogin> { None }
    /// Whether that second sign-in is connected (may ask the network; run it on a thread) and who it is.
    fn account_status(&self) -> Option<AuthStatus> { None }
    /// Takes what the sign-in window returned; the name to show.
    fn account_complete(&self, _value: &str) -> Result<Option<String>, String> { Err(format!("{} has no account sign-in", self.label())) }
    fn account_disconnect(&self) -> Result<(), String> { Err(format!("{} has no account sign-in", self.label())) }
    fn auth_begin(&self) -> Result<AuthBegin, String> { Err(format!("{} has no sign-in", self.label())) }
    fn auth_complete(&self, _value: &str) -> Result<AuthStatus, String> { Err(format!("{} has no sign-in", self.label())) }
    fn auth_disconnect(&self) -> Result<(), String> { Err(format!("{} has no sign-in", self.label())) }

    /// Purchases a browser read (ISO time, store name, sku) matched to the library and set as `date_added`, earliest wins. (read, matched, set). Default: not supported.
    /// When the account bought each of these games (PlayDate's `fetch_purchase_dates`): (appid, Unix time or None when the store has no date for it). The sources read what they need
    /// (platform id, namespace) from `cx.db`. An Err is a problem for the whole read (not connected, the store refused), reported once.
    fn purchase_dates(&self, _cx: &SourceCtx, _appids: &[i64]) -> Result<Vec<(i64, Option<i64>)>, String> { Err(format!("{} has no purchase dates to read", self.label())) }
    fn apply_purchase_dates(&self, _online_id: &str, _purchases: &[(String, String, String)]) -> Result<(usize, usize, usize), String> { Err(format!("{} has no purchase dates to import", self.label())) }

    /// The store's own artwork for a game, for the Artwork Sources "Store" option: ("vertical" | "horizontal" | "icon", address), any subset; empty = none. May ask the store.
    fn art_urls(&self, _g: &GameRef) -> Vec<(String, String)> { vec![] }

    /// Download and unpack a game the user does not have on disk yet. `progress(text, done, total)` feeds the status line; `cancel` is polled. On success the game is installed in the returned folder
    /// (`executable` is the file to run when the source knows it). Default: not supported.
    /// A download from this store can hold several games (a pack): launching one that has several programs and none pinned asks which one, every time (nothing is remembered).
    fn pick_program_each_time(&self) -> bool { false }
    /// Does this store offer several builds of a game (Windows, Linux, Mac) to choose between?
    fn platform_choice(&self) -> bool { false }
    /// The builds on offer for one game, so the person can pick one when there are several (needs the network).
    fn install_options(&self, _cx: &SourceCtx, _g: &GameRef) -> Result<Vec<InstallOption>, String> { Ok(vec![]) }
    fn install(&self, _cx: &SourceCtx, g: &GameRef, _progress: &mut dyn FnMut(&str, u64, u64), _cancel: &crate::net::Cancel) -> Result<Installed, String> { Err(format!("Installing {} games is not supported yet ({})", self.label(), g.name)) }
    /// The Blacklist Manager's Deep Plugin Scan (PlayDate's `scan_junk`): re-check this store's imported games that were never touched (no playtime, not installed, never played)
    /// against the store's current import rules and return the ones that now look like non-games. Asks the store over the network; `Ok(vec![])` when not connected.
    fn scan_junk(&self) -> Result<Vec<JunkHit>, String> { Ok(vec![]) }
    /// Which of this source's games are installed, from what is on disk (a launcher's records, the games folder). `None` = this source cannot tell.
    fn installed_scan(&self) -> Option<Vec<InstalledRow>> { None }
    /// The folder this source's downloaded games live in, if it has one.
    fn games_dir(&self) -> Option<PathBuf> { None }
    /// Folders whose changes can mean a game was installed or removed: the app watches them and runs `installed_scan` once things settle. `true` = the whole subtree.
    /// Default: the games folder, top level only. A folder that does not exist yet is picked up when it appears.
    fn watch_paths(&self) -> Vec<(PathBuf, bool)> { self.games_dir().into_iter().map(|d| (d, false)).collect() }

    /// Links: None = this game has none.
    fn store_url(&self, _g: &GameRef) -> Option<String> { None }
    fn achievements_url(&self, _g: &GameRef, _account_id: Option<&str>) -> Option<String> { None }
    fn community_url(&self, _g: &GameRef) -> Option<String> { None }

    /// Play. The returned action is carried out by the app.
    fn launch(&self, g: &GameRef) -> Result<Launch, String> { { let _ = g; Err(format!("Launching {} games is not supported yet", self.label())) } }
    /// Windows: the game's own uninstall entry in the registry; elsewhere a store that does not say how is not supported yet.
    fn uninstall(&self, g: &GameRef) -> Result<Action, String> { if cfg!(windows) { crate::winuninstall::action(g) } else { Err(format!("Uninstalling {} games is not supported yet", self.label())) } }

    /// List the user's games: batches of records go to `sink` as they arrive (the caller imports them); `cancel` is polled. `Ok(count)` when finished.
    fn sync(&self, _cx: &SourceCtx, _sink: &mut dyn FnMut(Vec<crate::importer::GameRecord>), _cancel: &crate::net::Cancel) -> Result<usize, String> { Err(format!("Syncing {} is not supported yet", self.label())) }
    /// True when a first import should be followed by a re-scrape of the new games (the record format cannot carry ratings or achievements).
    fn enrich_new(&self) -> bool { false }
    /// Fresh metadata for one game (`rescrape`), as the columns to write; `Ok(None)` = nothing came back. A 429 is `Err`.
    fn rescrape(&self, _cx: &SourceCtx, _g: &GameRef) -> Fetch<Option<Update>> { Ok(None) }
    /// The store's own description (plain text), if the source has one.
    fn description(&self, _cx: &SourceCtx, _g: &GameRef) -> Option<String> { None }
    /// Genre / tags / release date scraped from the store's own page (`platform_slug`), for a game no Steam or PCGamingWiki page knows.
    fn store_page_metadata(&self, _slug: &str) -> PageMeta { PageMeta::default() }
}

/// Platforms that exist in databases (and in the filter lists) whether or not a source for them is built yet: id, label. The order is the order the filter
/// dialog lists them, core stores first.
pub const KNOWN_PLATFORMS: &[(&str, &str)] = &[
    ("steam", "Steam"), ("epic_games", "Epic Games"), ("ea_app", "EA App"), ("ubisoft", "Ubisoft Connect"), ("xbox", "Xbox"), ("battle_net", "Battle.net"), ("gog", "GOG"),
    ("humble", "Humble Bundle"), ("indiegala", "IndieGala"), ("itch_io", "itch.io"), ("nintendo", "Nintendo eShop"), ("amazon_games", "Amazon Games"),
    ("legacy_games", "Legacy Games"), ("playstation", "PlayStation"), ("rockstar", "Rockstar Games"), ("custom", "Manual"),
];

/// The display label of a platform id without a registry (known table, else the id with spaces).
pub fn platform_label(platform: &str) -> String { Registry::empty().label(platform) }

// ---------------------------------------------------------------- stores versus emulated systems
//
// Both kinds of game live in the same `platform` column: a store's id (`gog`, `xbox`, ...) or, for a game run in an emulator, the id of the emulated system
// (`snes`, `ps2`, `xbox`, ...; PlayDate's `known_emulators.PLATFORM_NAMES`). One id is in both lists today: `xbox`, the Xbox / Game Pass store and the original
// Xbox's emulator systems (a ROM's `platform_id` is its file path, a store game's is `store:<id>` or a product id). Nothing may ever treat the two as the same game:
// a source is looked up by `Registry::for_game`, which refuses a game whose id is a ROM path, no source may claim an emulated system's id except the one listed in
// `SHARED_WITH_EMULATION`, and the importer rejects a record that looks like a ROM. A test fails when a new store id collides.

/// The emulated systems PlayDate's emulator support knows (`known_emulators.PLATFORM_NAMES`).
pub const EMULATED_PLATFORMS: &[&str] = &["32x", "3ds", "arcade", "atari2600", "c64", "dos", "dreamcast", "gamecube", "gb", "gba", "gbc", "genesis", "gg", "lynx", "n64", "nds", "nes", "pce", "ps1", "ps2",
    "ps3", "psp", "psvita", "saturn", "scummvm", "segacd", "sms", "snes", "switch", "virtualboy", "wii", "wiiu", "xbox", "xbox360"];
/// Store platform ids that are also an emulated system's id, on purpose and for good (existing databases hold both under this value).
pub const SHARED_WITH_EMULATION: &[&str] = &["xbox"];

pub fn is_emulated_system(platform: &str) -> bool { EMULATED_PLATFORMS.contains(&platform) }

/// Does a `platform_id` look like a file on disk (an emulated game's ROM, folder or app) rather than a store's id? Store ids never contain a path separator, a
/// drive letter or a ROM extension; `store:` ids are the Xbox plugin's.
pub fn looks_like_rom_path(platform_id: &str) -> bool {
    if platform_id.starts_with("store:") { return false; }
    let b = platform_id.as_bytes();
    if platform_id.contains('/') || platform_id.contains('\\') { return true; }
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' { return true; }
    let l = platform_id.to_ascii_lowercase();
    ["iso", "xbe", "xex", "zar", "bin", "cue", "chd", "rom", "gba", "gbc", "gb", "nes", "sfc", "smc", "n64", "z64", "nds", "3ds", "cia", "nsp", "xci", "wbfs", "rvz", "gcm", "pbp", "cso", "pkg", "zip", "7z"].iter().any(|e| l.ends_with(&format!(".{e}")))
}

#[derive(Debug, Clone, PartialEq)]
pub enum Origin { Store, Emulated }

/// What kind of game a row is. A platform that is only a store, or only an emulated system, answers by itself; the shared one is decided by the `platform_id`.
pub fn origin(platform: &str, platform_id: &str) -> Origin {
    match (is_emulated_system(platform), SHARED_WITH_EMULATION.contains(&platform)) {
        (false, _) => Origin::Store,
        (true, false) => Origin::Emulated,
        (true, true) => if looks_like_rom_path(platform_id) { Origin::Emulated } else { Origin::Store },
    }
}

/// What a built-in source is built from.
#[derive(Clone)]
pub struct SourceEnv { pub http: Arc<dyn Http>, pub config_dir: PathBuf, pub db: PathBuf }

pub struct Registry { sources: Vec<Arc<dyn LibrarySource>> }

impl Registry {
    pub fn empty() -> Self { Registry { sources: vec![] } }
    /// The built-in sources that need the data folder, the network or the library (everything except Steam): add them to a registry.
    pub fn register_builtins(&mut self, env: &SourceEnv) {
        for s in [Arc::new(nintendo::NintendoSource::new(env.http.clone(), env.config_dir.clone(), env.db.clone())) as Arc<dyn LibrarySource>, Arc::new(humble::HumbleSource::new(env.http.clone(), env.config_dir.clone(), env.db.clone())), Arc::new(playstation::PlaystationSource::new(env.http.clone(), env.config_dir.clone(), env.db.clone())), Arc::new(epic::EpicSource::new(env.http.clone(), env.config_dir.clone(), env.db.clone())), Arc::new(gog::GogSource::new(env.http.clone(), env.config_dir.clone(), env.db.clone())), Arc::new(itch_io::ItchSource::new(env.http.clone(), env.config_dir.clone(), env.db.clone())), Arc::new(indiegala::IndiegalaSource::new(env.http.clone(), env.config_dir.clone(), env.db.clone())), Arc::new(amazon_games::AmazonSource::new(env.http.clone(), env.config_dir.clone(), env.db.clone())), Arc::new(xbox::XboxSource::new(env.http.clone(), env.config_dir.clone(), env.db.clone())), Arc::new(battle_net::BattleNetSource::new(env.config_dir.clone(), env.db.clone(), env.http.clone())), Arc::new(rockstar::RockstarSource::new(env.http.clone(), env.config_dir.clone(), env.db.clone())), Arc::new(legacy_games::LegacyGamesSource::new(env.config_dir.clone(), env.db.clone())), Arc::new(ea_app::EaSource::new(env.http.clone(), env.config_dir.clone(), env.db.clone())), Arc::new(ubisoft::UbisoftSource::new(env.config_dir.clone(), env.db.clone()))] {
            self.register(s).expect("built-in sources do not collide");
        }
    }
    /// Steam plus the other built-ins, for a job that needs the registry on its own thread.
    pub fn builtin_with(config_dir: &std::path::Path, db: &std::path::Path) -> Self { let mut r = Registry::builtin(); r.register_builtins(&SourceEnv { http: Arc::new(crate::net::UreqHttp::new()), config_dir: config_dir.to_path_buf(), db: db.to_path_buf() }); r }
    /// The sources compiled into Zest.
    pub fn builtin() -> Self { let mut r = Registry::empty(); r.register(Arc::new(steam::SteamSource)).expect("built-in sources do not collide"); r }
    /// Adds a source. Refused (Err) when its id is already taken, or is an emulated system's id that is not on the `SHARED_WITH_EMULATION` list: two owners of one
    /// `platform` value is the kind of mistake that corrupts a library, so it is an error, never a silent replace.
    pub fn register(&mut self, s: Arc<dyn LibrarySource>) -> Result<(), String> {
        let id = s.id().to_string();
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') { return Err(format!("{id:?} is not a valid platform id (lower case letters, digits and underscores)")); }
        if self.sources.iter().any(|x| x.id() == id) { return Err(format!("the platform {id} already has a source")); }
        if is_emulated_system(&id) && !SHARED_WITH_EMULATION.contains(&id.as_str()) { return Err(format!("{id} is an emulated system; a store must not use that platform id")); }
        self.sources.push(s); Ok(())
    }
    /// The source for a platform. Prefer `for_game` when the game's `platform_id` is known: it cannot be fooled by an emulated game under a shared platform id.
    pub fn get(&self, platform: &str) -> Option<&Arc<dyn LibrarySource>> { self.sources.iter().find(|s| s.id() == platform) }
    /// The source that owns this game, or None for a game that is not a store's (an emulated one, or a platform with no source yet).
    pub fn for_game(&self, platform: &str, platform_id: &str) -> Option<&Arc<dyn LibrarySource>> {
        if origin(platform, platform_id) == Origin::Emulated { return None; }
        self.get(platform)
    }
    pub fn all(&self) -> &[Arc<dyn LibrarySource>] { &self.sources }
    /// Display label: a registered source's, else the known one, else the id with spaces.
    pub fn label(&self, platform: &str) -> String {
        if let Some(s) = self.get(platform) { return s.label().to_string(); }
        KNOWN_PLATFORMS.iter().find(|(id, _)| *id == platform).map(|(_, l)| l.to_string()).or_else(|| crate::emulators::platform_names().iter().find(|(p, _)| p == platform).map(|(_, l)| l.clone())).unwrap_or_else(|| platform.split('_').filter(|w| !w.is_empty()).map(|w| { let mut c = w.chars(); c.next().map_or(String::new(), |f| f.to_uppercase().collect::<String>() + c.as_str()) }).collect::<Vec<_>>().join(" "))
    }
    /// Position in the filter list (known platforms first, in order, then the rest alphabetically by the caller).
    pub fn order(&self, platform: &str) -> usize { KNOWN_PLATFORMS.iter().position(|(id, _)| *id == platform).unwrap_or(usize::MAX) }
    pub fn caps(&self, platform: &str) -> Caps { self.get(platform).map(|s| s.caps()).unwrap_or_default() }
}

/// The backfill's plugin hooks, answered by whichever source owns the game's platform.
impl Hooks for Registry {
    fn store_page_metadata(&self, platform: &str, slug: &str) -> PageMeta { self.get(platform).map(|s| s.store_page_metadata(slug)).unwrap_or_default() }
    fn description(&self, platform: &str, appid: i64, platform_id: &str) -> String {
        // description() needs a context the backfill does not carry; the sources that have one are asked through `rescrape` for now
        let _ = (platform, appid, platform_id); String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fake;
    impl LibrarySource for Fake { fn id(&self) -> &str { "fake" } fn label(&self) -> &str { "Fake Store" } fn caps(&self) -> Caps { Caps { launch: true, ..Default::default() } } }
    #[test] fn every_builtin_has_a_known_label_a_valid_id_and_no_emulated_clash() {
        let http: Arc<dyn Http> = Arc::new(crate::net::UreqHttp::new());
        let mut r = Registry::builtin(); r.register_builtins(&SourceEnv { http, config_dir: PathBuf::from("/nonexistent"), db: PathBuf::from("/nonexistent/g.db") });
        for s in r.all() {
            assert!(KNOWN_PLATFORMS.iter().any(|(id, l)| *id == s.id() && *l == s.label()), "{} is missing from KNOWN_PLATFORMS or its label differs", s.id());
            assert!(!is_emulated_system(s.id()) || SHARED_WITH_EMULATION.contains(&s.id()));
        }
        assert!(r.get("nintendo").is_some());
    }
    /// Read-only run against a real account: `ZEST_REAL_DIR` = a COPY of the data folder (config.json and a games db), never the live one (a token refresh writes config.json).
    /// `cargo test -p pd-filter real_purchase_dates -- --ignored --nocapture`
    #[test] #[ignore] fn real_purchase_dates() {
        let dir = std::path::PathBuf::from(std::env::var("ZEST_REAL_DIR").expect("ZEST_REAL_DIR"));
        let db = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).find(|p| p.file_name().and_then(|n| n.to_str()).map_or(false, |n| n.starts_with("games_") && n.ends_with(".db"))).expect("a games_*.db");
        let reg = Registry::builtin_with(&dir, &db);
        let conn = rusqlite::Connection::open(&db).unwrap();
        let steam = Arc::new(crate::steamclient::Steam::new(Arc::new(crate::net::UreqHttp::new()), None, 10.0));
        let cx = SourceCtx { http: Arc::new(crate::net::UreqHttp::new()), steam, root: dir.clone(), db: db.clone(), today: "2026-11-09".into(), appinfo: Arc::new(|_| Default::default()), account_id: None };
        for plat in ["indiegala", "gog", "epic_games", "ea_app"] {
            let ids: Vec<i64> = conn.prepare("SELECT appid FROM games WHERE platform = ? LIMIT 40").unwrap().query_map([plat], |r| r.get(0)).unwrap().filter_map(|x| x.ok()).collect();
            let Some(src) = reg.get(plat) else { println!("{plat}: no source"); continue };
            if ids.is_empty() { println!("{plat}: no games in this library"); continue; }
            match src.purchase_dates(&cx, &ids) {
                Ok(r) => { let some = r.iter().filter(|(_, t)| t.is_some()).count(); let (lo, hi) = r.iter().filter_map(|(_, t)| *t).fold((i64::MAX, 0), |a, t| (a.0.min(t), a.1.max(t))); println!("{plat}: {} games asked, {some} dated (earliest {lo}, latest {hi})", ids.len()); }
                Err(e) => println!("{plat}: {e}"),
            }
        }
    }
    /// Humble's real sync against a COPY of the data folder (`ZEST_REAL_DIR`; the session is read from the keyring): how many games it finds, how many the library lacks, how long it took.
    /// Prints counts and a few titles, never a key. `cargo test -p pd-filter real_humble_sync -- --ignored --nocapture`
    #[test] #[ignore] fn real_humble_sync() {
        let dir = std::path::PathBuf::from(std::env::var("ZEST_REAL_DIR").expect("ZEST_REAL_DIR"));
        let db = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).find(|p| p.file_name().and_then(|n| n.to_str()).map_or(false, |n| n.starts_with("games_") && n.ends_with(".db"))).expect("a games_*.db");
        if let Some(v) = crate::secrets::platform_vault() { crate::secrets::install(v); }
        let reg = Registry::builtin_with(&dir, &db);
        let steam = Arc::new(crate::steamclient::Steam::new(Arc::new(crate::net::UreqHttp::new()), None, 10.0));
        let cx = SourceCtx { http: Arc::new(crate::net::UreqHttp::new()), steam, root: dir.clone(), db: db.clone(), today: "2026-11-22".into(), appinfo: Arc::new(|_| Default::default()), account_id: None };
        let src = reg.get("humble").expect("humble");
        let have: std::collections::HashSet<String> = rusqlite::Connection::open(&db).unwrap().prepare("SELECT platform_id FROM games WHERE platform = 'humble'").unwrap().query_map([], |r| r.get::<_, String>(0)).unwrap().flatten().collect();
        let mut recs: Vec<crate::importer::GameRecord> = vec![];
        let t0 = std::time::Instant::now();
        let r = src.sync(&cx, &mut |b| recs.extend(b), &crate::net::Cancel::new());
        let new: Vec<&crate::importer::GameRecord> = recs.iter().filter(|g| !have.contains(&g.platform_id)).collect();
        println!("humble sync: {:?} in {:?}; library has {} humble games; the sync produced {} records, {} not in the library", r, t0.elapsed(), have.len(), recs.len(), new.len());
        for g in new.iter().take(15) { println!("  new: {} ({})", g.name, g.platform_id); }
        let missing: Vec<&String> = have.iter().filter(|h| !recs.iter().any(|g| &g.platform_id == *h)).collect();
        println!("in the library but not produced by this sync (expected: they were skipped as already known, or are gone): {}", missing.len());
    }
    /// Epic's real sync against a COPY of the data folder (`ZEST_REAL_DIR`), then the status line saved the way the app saves it: does the file change?
    /// `cargo test -p pd-filter real_epic_sync_status -- --ignored --nocapture`
    #[test] #[ignore] fn real_epic_sync_status() {
        let dir = std::path::PathBuf::from(std::env::var("ZEST_REAL_DIR").expect("ZEST_REAL_DIR"));
        let db = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).find(|p| p.file_name().and_then(|n| n.to_str()).map_or(false, |n| n.starts_with("games_") && n.ends_with(".db"))).expect("a games_*.db");
        let reg = Registry::builtin_with(&dir, &db);
        let steam = Arc::new(crate::steamclient::Steam::new(Arc::new(crate::net::UreqHttp::new()), None, 10.0));
        let cx = SourceCtx { http: Arc::new(crate::net::UreqHttp::new()), steam, root: dir.clone(), db: db.clone(), today: "2026-11-09".into(), appinfo: Arc::new(|_| Default::default()), account_id: None };
        let src = reg.get("epic_games").expect("epic");
        let mut n = 0usize;
        let t0 = std::time::Instant::now();
        let r = src.sync(&cx, &mut |b| n += b.len(), &crate::net::Cancel::new());
        println!("epic sync: {:?} after {:?}, {n} records", r.as_ref().map(|x| *x), t0.elapsed());
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        let st = crate::status::SyncStatus { at: now, found: *r.as_ref().unwrap_or(&0), added: 0, updated: n, blocked: 0, error: r.err() };
        println!("record: {:?}", crate::status::record(&dir, "epic_games", &st));
        let back = crate::status::load(&dir);
        println!("saved at: {:?}  (now {now})", back.get("epic_games").map(|s| s.at));
    }
    /// What each store says about being signed in, on a COPY of the data folder (`ZEST_REAL_DIR`): the start-up sync only queues a store that says connected.
    #[test] #[ignore] fn real_auth_statuses() {
        let dir = std::path::PathBuf::from(std::env::var("ZEST_REAL_DIR").expect("ZEST_REAL_DIR"));
        let db = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).find(|p| p.file_name().and_then(|n| n.to_str()).map_or(false, |n| n.starts_with("games_") && n.ends_with(".db"))).expect("a games_*.db");
        let reg = Registry::builtin_with(&dir, &db);
        for id in ["epic_games", "gog", "humble", "itch_io", "xbox", "nintendo"] {
            match reg.get(id) { Some(s) => { let c = s.caps(); println!("{id}: sync={} auth={} status={:?}", c.sync, c.auth, s.auth_status().map(|a| (a.connected, a.username.is_some()))); } None => println!("{id}: no source") }
        }
    }
    #[test] fn an_emulated_system_is_labelled_with_its_full_name_and_a_store_keeps_its_own() {
        assert_eq!(platform_label("n64"), "Nintendo 64"); assert_eq!(platform_label("snes"), "SNES"); assert_eq!(platform_label("xbox"), "Xbox");
    }
    #[test] fn registry_lookup_labels_and_order() {
        let mut r = Registry::builtin();
        assert!(r.get("steam").is_some() && r.get("gog").is_none());
        assert_eq!(r.label("steam"), "Steam"); assert_eq!(r.label("gog"), "GOG"); assert_eq!(r.label("custom"), "Manual"); assert_eq!(r.label("some_new_store"), "Some New Store");
        assert!(r.order("steam") < r.order("gog") && r.order("gog") < r.order("custom") && r.order("zzz") == usize::MAX);
        r.register(Arc::new(Fake)).unwrap();
        assert_eq!(r.label("fake"), "Fake Store"); assert!(r.caps("fake").launch && !r.caps("fake").rescrape && !r.caps("gog").launch);
        assert!(r.register(Arc::new(Fake)).unwrap_err().contains("already"), "a second owner of a platform is an error");
    }
    #[test] fn defaults_refuse_politely() {
        let g = GameRef::default();
        // on Windows the default uninstall runs the game's own uninstaller (found in the registry), so its refusal names that instead
        if !cfg!(windows) { assert!(Fake.uninstall(&g).unwrap_err().contains("Fake Store")); }
        assert!(Fake.store_url(&g).is_none());
    }

    struct Id(&'static str);
    impl LibrarySource for Id { fn id(&self) -> &str { self.0 } fn label(&self) -> &str { "x" } fn caps(&self) -> Caps { Caps::default() } }

    #[test] fn no_store_id_collides_with_an_emulated_system_except_the_listed_one() {
        let clash: Vec<&str> = KNOWN_PLATFORMS.iter().map(|(id, _)| *id).filter(|id| is_emulated_system(id)).collect();
        assert_eq!(clash, SHARED_WITH_EMULATION, "a new store id must not be an emulated system's id (or the shared list must say so on purpose)");
        assert_eq!(EMULATED_PLATFORMS.len(), 34);
        let mut sorted = EMULATED_PLATFORMS.to_vec(); sorted.sort(); sorted.dedup(); assert_eq!(sorted.len(), 34);
    }
    #[test] fn a_source_cannot_claim_an_emulated_system() {
        let mut r = Registry::empty();
        assert!(r.register(Arc::new(Id("snes"))).unwrap_err().contains("emulated system"));
        assert!(r.register(Arc::new(Id("ps2"))).is_err());
        assert!(r.register(Arc::new(Id("xbox"))).is_ok(), "the one listed collision");
        assert!(r.register(Arc::new(Id("Bad Id"))).is_err() && r.register(Arc::new(Id(""))).is_err());
    }
    #[test] fn the_shared_xbox_value_tells_a_store_game_from_a_rom_by_its_id() {
        assert_eq!(origin("xbox", "store:9PJ8266BHFWN"), Origin::Store);
        assert_eq!(origin("xbox", "791051466"), Origin::Store);
        assert_eq!(origin("xbox", "/roms/xbox/Halo.iso"), Origin::Emulated);
        assert_eq!(origin("xbox", "D:\\Games\\halo.xbe"), Origin::Emulated);
        assert_eq!(origin("xbox", "halo.iso"), Origin::Emulated);
        assert_eq!(origin("snes", "store:whatever"), Origin::Emulated, "an emulated-only system is always emulated");
        assert_eq!(origin("gog", "/weird/but/a/store"), Origin::Store, "a store-only platform is always a store");
        let mut r = Registry::empty(); r.register(Arc::new(Id("xbox"))).unwrap();
        assert!(r.for_game("xbox", "store:9PJ8266BHFWN").is_some());
        assert!(r.for_game("xbox", "/roms/xbox/Halo.iso").is_none(), "the Xbox store source never gets an emulated game");
    }
}

/// (appid, platform_id, platform_ns) of these games, for a source that looks dates up by its own ids.
pub fn platform_rows(db: &std::path::Path, appids: &[i64]) -> Vec<(i64, String, String)> {
    let Ok(c) = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { return vec![] };
    let _ = c.busy_timeout(std::time::Duration::from_secs(5));
    let Ok(mut st) = c.prepare("SELECT COALESCE(platform_id,''), COALESCE(platform_ns,'') FROM games WHERE appid = ?") else { return vec![] };
    appids.iter().map(|id| { let (p, n) = st.query_row([id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).unwrap_or_default(); (*id, p, n) }).collect()
}
