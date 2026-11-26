//! IndieGala as a library source (port of PlayDate's `indiegala` plugin: `indiegala.py`, `__init__.py` and the sign-in route in `routes.py`; install, launch and the games folder wait
//! for the shared runners layer). IndieGala has no API: the library is the logged-in `/library` page, paged as `/library/showcase/N/`, read with the user's `sessionid` cookie
//! (pasted; kept in `config.json["indiegala"]` as PlayDate keeps it, never logged). A game's cover, store page and download page all come from that listing.
use super::itch_io::utc_ts;
use super::{Action, AuthBegin, AuthStatus, Caps, GameRef, Installed, InstalledRow, Launch, LibrarySource, SourceCtx};
use crate::importer::GameRecord;
use crate::jobs::Update;
use crate::net::{log, set_activity, Cancel, Http};
use crate::steamclient::Fetch;
use regex::Regex;
use rusqlite::types::Value as Sql;
use scraper::{ElementRef, Html, Selector};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const LIBRARY_URL: &str = "https://www.indiegala.com/library";
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";
const MAX_PAGES: u32 = 200;
const CACHE_TTL: Duration = Duration::from_secs(120);
const EXPIRED: &str = "Session expired \u{2014} please reconnect IndieGala in the Plugins settings.";

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex { cell.get_or_init(|| Regex::new(pat).unwrap()) }
fn pause(ms: u64) { if !cfg!(test) { std::thread::sleep(Duration::from_millis(ms)); } }
fn sel(s: &str) -> Selector { Selector::parse(s).unwrap() }
/// `get_text(strip=True)`: the pieces of text under a node, each trimmed, joined with nothing.
fn stripped(el: ElementRef) -> String { el.text().map(str::trim).filter(|t| !t.is_empty()).collect() }
fn has_class(el: ElementRef, c: &str) -> bool { el.value().classes().any(|x| x == c) }
fn descendant_with_class<'a>(el: ElementRef<'a>, tag: Option<&str>, class: &str) -> Option<ElementRef<'a>> {
    el.descendants().filter_map(ElementRef::wrap).find(|e| tag.map_or(true, |t| e.value().name() == t) && has_class(*e, class))
}

/// One game on the library page.
#[derive(Clone, Debug, PartialEq)]
pub struct Listed { pub prod_id: String, pub prod_name: String, pub date_str: String, pub img_url: String, pub showcase_url: String, pub store_url: String }

/// `_parse_page`: the games on one library page.
pub fn parse_page(html: &str) -> Vec<Listed> {
    static PROD: OnceLock<Regex> = OnceLock::new(); static SHOW: OnceLock<Regex> = OnceLock::new();
    let prod = re(&PROD, r"/products/([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})/");
    let show = re(&SHOW, r"showShowcaseContents\('(\d+)'");
    let doc = Html::parse_document(html);
    let mut games = vec![];
    for card in doc.select(&sel("figcaption")).filter(|c| has_class(*c, "profile-private-page-library-title-padding-showcase")) {
        let Some(title_el) = descendant_with_class(card, None, "profile-private-page-library-title") else { continue };
        let title = stripped(title_el);
        let dates: Vec<ElementRef> = card.descendants().filter_map(ElementRef::wrap).filter(|e| has_class(*e, "profile-private-page-library-date-showcase")).collect();
        let date_str = dates.get(1).map(|d| stripped(*d)).unwrap_or_default();
        let fig = card.prev_siblings().filter_map(ElementRef::wrap).find(|e| e.value().name() == "figure");
        let (mut img_url, mut prod_id, mut showcase_url, mut store_url) = (String::new(), String::new(), String::new(), String::new());
        if let Some(fig) = fig {
            // the figure or a parent link may point straight at the showcase page
            let wrap = fig.ancestors().filter_map(ElementRef::wrap).find(|a| a.value().name() == "a" && a.value().attr("href").is_some())
                .or_else(|| fig.descendants().filter_map(ElementRef::wrap).find(|a| a.value().name() == "a" && a.value().attr("href").is_some()));
            if let Some(href) = wrap.and_then(|w| w.value().attr("href")) {
                if href.contains("/library/showcase/") { showcase_url = if href.starts_with("http") { href.to_string() } else { format!("https://www.indiegala.com{href}") }; }
            }
            if let Some(img) = fig.descendants().filter_map(ElementRef::wrap).find(|e| e.value().name() == "img") {
                // the raw HTML uses data-src (the page's script swaps it into src)
                let src = img.value().attr("data-src").filter(|s| !s.is_empty()).or(img.value().attr("src")).unwrap_or("");
                img_url = src.to_string();
                if let Some(m) = prod.captures(src) {
                    prod_id = m[1].to_string();
                    if showcase_url.is_empty() { showcase_url = format!("https://www.indiegala.com/library/showcase/{prod_id}"); }
                }
            }
            // the public store page lives in a separate panel, tied to this card by the numeric id in its click handler; its domain varies with how the game was
            // obtained, so any link that is not back into IndieGala's own library counts
            let item = card.ancestors().filter_map(ElementRef::wrap).find(|a| a.value().name() == "li");
            let click = item.and_then(|li| descendant_with_class(li, Some("a"), "fit-click"));
            if let Some(id) = click.and_then(|a| show.captures(a.value().attr("onclick").unwrap_or("")).map(|m| m[1].to_string())) {
                let want = format!("showcase-{id}");
                let panel = doc.root_element().descendants().filter_map(ElementRef::wrap).find(|e| e.value().attr("id") == Some(want.as_str()));
                let title_a = panel.and_then(|p| p.descendants().filter_map(ElementRef::wrap).find(|a| a.value().name() == "a" && has_class(*a, "library-showcase-title") && a.value().attr("href").is_some()));
                if let Some(href) = title_a.and_then(|a| a.value().attr("href")) { if href.starts_with("http") && !href.contains("/library/showcase/") { store_url = href.to_string(); } }
            }
        }
        if prod_id.is_empty() { continue; }
        games.push(Listed { prod_id, prod_name: title, date_str, img_url, showcase_url, store_url });
    }
    games
}

/// The download buttons on a library page: (game id, system, address). Each showcase game's panel has one button per system, a direct link to IndieGala's file
/// server whose path carries the game's id (`.../dev_fold_freebies/<id>/win/<name>_win.zip`); the button's icon names the system. Only https links to
/// IndieGala's own servers count.
pub fn parse_downloads(html: &str) -> Vec<(String, &'static str, String)> {
    static B: OnceLock<Regex> = OnceLock::new(); static ID: OnceLock<Regex> = OnceLock::new();
    let btn = re(&B, r#"library-showcase-download-btn[^>]*?onclick="location\.href='([^']+)'"[^>]*>\s*<i class="fa fa-([a-z]+)""#);
    let id = re(&ID, r"/([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})/");
    btn.captures_iter(html).filter_map(|c| {
        let url = c[1].replace("&amp;", "&");
        let host = url.strip_prefix("https://")?.split('/').next()?.to_lowercase();
        if !(host == "indiegala.com" || host.ends_with(".indiegala.com") || host.ends_with(".indiegalacdn.com")) { return None; }
        let os = match &c[2] { "windows" => "windows", "linux" => "linux", "apple" => "mac", _ => return None };
        Some((id.captures(&url)?[1].to_lowercase(), os, url))
    }).collect()
}

/// `_parse_dt`: the listing's date text/// `_parse_dt`: the listing's date text (`2020-05-01 12:30:00`), read as UTC.
pub fn parse_dt(s: &str) -> Option<i64> {
    static R: OnceLock<Regex> = OnceLock::new();
    let head: String = s.chars().take(19).collect();
    let c = re(&R, r"^(\d{4})-(\d{1,2})-(\d{1,2})[ T](\d{1,2}):(\d{1,2}):(\d{1,2})$").captures(&head)?;
    let n = |i: usize| c[i].parse::<i64>().unwrap_or(0);
    utc_ts(n(1), n(2), n(3), n(4), n(5), n(6))
}

/// The library records for a page's games (`seen` keeps a game that appears twice to its first appearance).
pub fn records_from(games: &[Listed], seen: &mut std::collections::HashSet<String>, now: i64) -> Vec<GameRecord> {
    games.iter().filter(|g| seen.insert(g.prod_id.clone())).map(|g| GameRecord {
        platform_id: g.prod_id.clone(), name: g.prod_name.clone(), slug: Some(g.store_url.clone()).filter(|u| !u.is_empty()), appname: Some(g.showcase_url.clone()).filter(|u| !u.is_empty()),
        date_added: Some(parse_dt(&g.date_str).unwrap_or(now)), ..Default::default() }).collect()
}

// ---------------------------------------------------------------- the source

enum Walk { Expired, Down(String) }
pub struct IndiegalaSource { http: Arc<dyn Http>, config_dir: PathBuf, db: PathBuf, cache: Mutex<Option<(Instant, HashMap<String, Listed>)>> }

impl IndiegalaSource {
    pub fn new(http: Arc<dyn Http>, config_dir: PathBuf, db: PathBuf) -> Self { IndiegalaSource { http, config_dir, db, cache: Mutex::new(None) } }
    fn cookie(&self) -> Option<String> { crate::account::plugin_secret_get(&self.config_dir, "indiegala", "session_cookie").filter(|v| !v.is_empty()) }
    fn name(&self) -> Option<String> { crate::account::plugin_secret_get(&self.config_dir, "indiegala", "username").filter(|u| !u.is_empty()) }
    /// The account name the library page shows (`<span class="username-text">Name</span>`).
    fn username_in(body: &str) -> Option<String> {
        static R: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let n = R.get_or_init(|| regex::Regex::new(r#"class="username-text">\s*([^<]{1,60}?)\s*<"#).unwrap()).captures(body)?[1].replace("&amp;", "&");
        (!n.is_empty()).then_some(n)
    }
    fn page_url(page: u32) -> String { if page == 1 { LIBRARY_URL.into() } else { format!("{LIBRARY_URL}/showcase/{page}/") } }
    /// The page and the address it came from after redirects.
    fn fetch(&self, url: &str, cookie: &str, secs: u64) -> Result<(crate::net::Response, String), String> {
        let c = format!("sessionid={cookie}");
        self.http.get_final(url, &[("Cookie", &c), ("User-Agent", UA), ("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8"), ("Accept-Language", "en-US,en;q=0.5")], Duration::from_secs(secs)).map_err(|e| e.to_string())
    }
    /// True = the library page loaded for this session; False = it was refused (a login page); None = could not tell.
    fn check(&self, cookie: &str) -> Option<bool> {
        let (r, url) = self.fetch(LIBRARY_URL, cookie, 15).ok()?;
        if r.status != 200 { return None; }
        let ok = !(url.contains("/login") || !r.body.contains("profile-private-page-library"));
        // a connection made before the name was read (it said "Connected") learns it now
        if ok { if let Some(n) = Self::username_in(&r.body) { if self.name().as_deref() != Some(n.as_str()) { let _ = crate::account::plugin_secret_set(&self.config_dir, "indiegala", "username", Some(&n)); } } }
        Some(ok)
    }
    /// Every library page until an empty one comes back (the pagination widget only shows nearby page numbers, so it cannot give the total).
    fn walk(&self, cookie: &str, cancel: Option<&Cancel>, label: &str) -> Result<Vec<Listed>, Walk> {
        let mut all = vec![];
        for page in 1..=MAX_PAGES {
            if cancel.map_or(false, |c| c.is_set()) { break; }
            set_activity(&format!("{label}: page {page}"));
            let (r, url) = match self.fetch(&Self::page_url(page), cookie, 20) { Ok(x) => x, Err(e) => { log(&format!("IndieGala: page {page} fetch failed: {e}")); if page == 1 { return Err(Walk::Down(format!("Connection failed: {e}"))); } break } };
            if url.contains("/login") { return Err(Walk::Expired); }
            if r.status != 200 { log(&format!("IndieGala: page {page} returned {}", r.status)); if page == 1 { return Err(Walk::Down(format!("Could not load IndieGala library (HTTP {}).", r.status))); } break; }
            let games = parse_page(&r.body);
            log(&format!("IndieGala: page {page}, {} games parsed", games.len()));
            if games.is_empty() { break; }
            all.extend(games);
            pause(500);
        }
        Ok(all)
    }
    /// One game's listing entry, from a walk shared for two minutes so a bulk re-scrape reads the library once.
    fn listed(&self, prod_id: &str) -> Option<Listed> {
        let mut c = self.cache.lock().ok()?;
        if c.as_ref().map_or(true, |(t, _)| t.elapsed() > CACHE_TTL) {
            let cookie = self.cookie()?;
            match self.walk(&cookie, None, "IndieGala") {
                Ok(all) => *c = Some((Instant::now(), all.into_iter().map(|g| (g.prod_id.clone(), g)).collect())),
                Err(Walk::Expired) => { log("IndieGala session expired"); return None; }
                Err(Walk::Down(m)) => { log(&format!("IndieGala: {m}")); *c = Some((Instant::now(), HashMap::new())); }
            }
        }
        c.as_ref().and_then(|(_, m)| m.get(prod_id).cloned())
    }
    /// The game's download buttons, (system, address), read from the library pages (until the game is found or the pages run out).
    fn downloads_of(&self, prod_id: &str) -> Result<Vec<(String, String)>, String> {
        let cookie = self.cookie().ok_or("Not connected to IndieGala \u{2014} please sign in")?;
        let want = prod_id.to_lowercase();
        for page in 1..=MAX_PAGES {
            let (r, url) = self.fetch(&Self::page_url(page), &cookie, 20).map_err(|e| format!("Could not reach IndieGala: {e}"))?;
            if url.contains("/login") { return Err("Your IndieGala sign-in has expired \u{2014} please sign in again".into()); }
            if r.status != 200 { return Err(format!("IndieGala answered HTTP {}", r.status)); }
            let found: Vec<(String, String)> = parse_downloads(&r.body).into_iter().filter(|(id, _, _)| *id == want).map(|(_, os, u)| (os.to_string(), u)).collect();
            if !found.is_empty() { return Ok(found); }
            if parse_page(&r.body).is_empty() { break; }
            pause(500);
        }
        Err("IndieGala's library shows no download for this game (a Steam key, or a game with no build to download)".into())
    }
    /// The cover the library page shows (`art_urls`)    /// The cover the library page shows (`art_urls`): one image, used as the vertical art.
    pub fn art_urls(&self, prod_id: &str) -> Vec<(&'static str, String)> {
        self.listed(prod_id).filter(|g| !g.img_url.is_empty()).map(|g| vec![("vertical", g.img_url)]).unwrap_or_default()
    }
}

impl LibrarySource for IndiegalaSource {
    fn id(&self) -> &str { "indiegala" }
    fn label(&self) -> &str { "IndieGala" }
    fn caps(&self) -> Caps { Caps { auth: true, sync: true, rescrape: true, art: true, store_page: true, purchase_dates: true, launch: true, install: true, uninstall: true, installed_scan: true, ..Default::default() } }
    fn platform_choice(&self) -> bool { true }
    fn install_options(&self, _cx: &SourceCtx, g: &GameRef) -> Result<Vec<super::InstallOption>, String> {
        let links = self.downloads_of(&g.platform_id)?;
        Ok(["windows", "linux", "mac"].iter().filter_map(|p| links.iter().find(|(os, _)| os == p).map(|(os, url)| super::InstallOption {
            key: os.to_string(), label: format!("{}: {}", match os.as_str() { "windows" => "Windows (run through Wine)", "linux" => "Linux (native)", _ => "Mac" }, url.rsplit('/').next().unwrap_or("")) })).collect())
    }
    /// Downloads the game's build from IndieGala's file server (the library page's download button) into its folder and unpacks it.
    fn install(&self, _cx: &SourceCtx, g: &GameRef, progress: &mut dyn FnMut(&str, u64, u64), cancel: &Cancel) -> Result<Installed, String> {
        progress(&format!("{}: looking up the download on IndieGala", g.name), 0, 0);
        let links = self.downloads_of(&g.platform_id)?;
        let order = super::humble::download_order("auto", super::take_install_choice(g.appid).as_deref(), crate::native_exe::Os::current());
        let (os, url) = order.iter().find_map(|p| links.iter().find(|(os, _)| os == p)).cloned().ok_or("IndieGala offers no download of this game for Windows, Linux or Mac")?;
        let filename = url.rsplit('/').next().filter(|f| !f.is_empty() && !f.contains("..")).unwrap_or("download.zip").to_string();
        log(&format!("IndieGala: downloading {filename:?} for {:?} ({os})", g.name));
        let dir = self.dir().join(safe_dirname(&g.name));
        crate::installdir::check_writable(&dir)?;
        let archive = dir.join(&filename);
        let name = g.name.clone();
        super::folder::download(self.http.as_ref(), &url, &archive, cancel, &mut |d, t| progress(&format!("Downloading {name}: {} of {} MB", super::folder::mb(d), super::folder::mb(t)), d, t))?;
        progress(&format!("Unpacking {name}"), 0, 0);
        let mut note = super::folder::unpack(&archive, &dir, cancel, &mut |d, t| progress(&format!("Unpacking {name}: {} of {} MB", super::folder::mb(d), super::folder::mb(t)), d, t))?;
        // a game shipped as a Windows setup program: run it once into the game's folder
        match super::folder::run_setup(&dir, &self.prefix(g.appid), cancel, &mut |m| progress(&format!("{name}: {m}"), 0, 0)) {
            Ok(Some(n)) => { log(&format!("indiegala: {name:?} {n}")); note = None; }   // the "cannot unpack" note was about the setup file
            Ok(None) => {}
            Err(e) if e == "Cancelled" => return Err(e),
            Err(e) => { log(&format!("indiegala: {name:?}: {e}")); note = Some(e); }
        }
        Ok(Installed { install_path: dir.to_string_lossy().to_string(), executable: None, note })
    }
    fn purchase_dates(&self, cx: &SourceCtx, appids: &[i64]) -> Result<Vec<(i64, Option<i64>)>, String> {
        let cookie = self.cookie().ok_or("Not connected")?;
        let all = self.walk(&cookie, None, "IndieGala: reading your library for purchase dates").map_err(|w| match w { Walk::Expired => EXPIRED.to_string(), Walk::Down(m) => m })?;
        crate::net::set_activity("");
        let by_id: HashMap<&str, i64> = all.iter().filter_map(|g| parse_dt(&g.date_str).map(|t| (g.prod_id.as_str(), t))).collect();
        Ok(super::platform_rows(&cx.db, appids).into_iter().map(|(id, pid, _)| (id, by_id.get(pid.as_str()).copied())).collect())
    }

    fn auth_status(&self) -> Result<AuthStatus, String> {
        let Some(cookie) = self.cookie() else { return Ok(AuthStatus { connected: false, username: None }) };
        // asked each time the dialog opens, as PlayDate's status route did: a refused cookie shows as signed out
        if self.check(&cookie) == Some(false) { return Ok(AuthStatus { connected: false, username: None }); }
        Ok(AuthStatus { connected: true, username: Some(crate::account::plugin_secret_get(&self.config_dir, "indiegala", "username").filter(|u| !u.is_empty()).unwrap_or_else(|| "Connected".into())) })
    }
    /// The sign-in window opens the library page (it sends a signed-out visitor to the login) and is done once the session cookie exists (the login lands on the home page, not the library).
    fn web_login_done(&self) -> Option<serde_json::Value> { Some(serde_json::json!({"kind": "cookies", "url": "https://www.indiegala.com/", "names": ["sessionid"], "url_not_contains": "/login"})) }
    fn paste_login_kept(&self) -> bool { false }   // the sign-in window works (confirmed live 2026-11-22)
    fn auth_begin(&self) -> Result<AuthBegin, String> {
        Ok(AuthBegin { url: LIBRARY_URL.into(), placeholder: "Paste your sessionid cookie value here".into(), instructions: vec![
            "Sign in to your IndieGala account in your browser.".into(),
            "Open the developer tools (F12), go to Application (Chrome) or Storage (Firefox), then Cookies, then indiegala.com.".into(),
            "Copy the value of the cookie named sessionid and paste it below.".into()] })
    }
    fn auth_complete(&self, value: &str) -> Result<AuthStatus, String> {
        let cookie = value.trim().trim_matches('"').to_string();
        if cookie.is_empty() { return Err("No cookie provided".into()); }
        let (r, url) = self.fetch(LIBRARY_URL, &cookie, 15).map_err(|e| { log(&format!("IndieGala connect: {e}")); format!("Connection failed: {e}") })?;
        log(&format!("IndieGala connect: HTTP {}", r.status));
        if r.status != 200 { return Err(format!("Could not load IndieGala library (HTTP {}).", r.status)); }
        if url.contains("/login") || !r.body.contains("profile-private-page-library") { return Err("Session cookie appears invalid \u{2014} log in to IndieGala and try again.".into()); }
        let who = Self::username_in(&r.body).unwrap_or_else(|| "Connected".into());
        for (k, v) in [("session_cookie", cookie.as_str()), ("username", who.as_str())] { crate::account::plugin_secret_set(&self.config_dir, "indiegala", k, Some(v)).map_err(|_| "Could not save the session".to_string())?; }
        log("IndieGala connected");
        Ok(AuthStatus { connected: true, username: Some(who) })
    }
    fn auth_disconnect(&self) -> Result<(), String> {
        for k in ["session_cookie", "username"] { crate::account::plugin_secret_set(&self.config_dir, "indiegala", k, None).map_err(|_| "Could not save".to_string())?; }
        *self.cache.lock().map_err(|_| "busy".to_string())? = None;
        Ok(())
    }

    fn art_urls(&self, g: &GameRef) -> Vec<(String, String)> { Self::art_urls(self, &g.platform_id).into_iter().map(|(k, v)| (k.to_string(), v)).collect() }
    fn games_dir(&self) -> Option<PathBuf> { Some(self.dir()) }
    /// A game with a folder is installed while the folder exists; a missing folder clears it; folders in the games folder that match no linked game are matched to unlinked games by name.
    fn installed_scan(&self) -> Option<Vec<InstalledRow>> {
        let c = rusqlite::Connection::open_with_flags(&self.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
        let mut st = c.prepare("SELECT appid, COALESCE(name,''), platform_id, COALESCE(install_path,'') FROM games WHERE platform = 'indiegala' AND platform_id IS NOT NULL").ok()?;
        let games: Vec<(i64, String, String, String)> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).ok()?.flatten().collect();
        let (mut out, mut linked) = (vec![], std::collections::HashSet::new());
        for (_, _, pid, path) in &games {
            if path.is_empty() { continue; }
            if super::folder::has_content(std::path::Path::new(path)) { out.push(InstalledRow { platform_id: pid.clone(), installed: true, install_path: None }); linked.insert(path.clone()); }
            else { out.push(InstalledRow { platform_id: pid.clone(), installed: false, install_path: Some(String::new()) }); }
        }
        let mut unlinked: Vec<(i64, String)> = games.iter().filter(|g| g.3.is_empty()).map(|g| (g.0, g.1.clone())).collect();
        if let Ok(rd) = std::fs::read_dir(self.dir()) {
            let mut dirs: Vec<(String, PathBuf)> = rd.flatten().filter(|e| e.path().is_dir() && super::folder::has_content(&e.path())).map(|e| (e.file_name().to_string_lossy().to_string(), e.path())).collect();
            dirs.sort();   // the order is the folder names', not the file system's, so a tie is always settled the same way
            for (name, path) in dirs {
                let ps = path.to_string_lossy().to_string();
                if linked.contains(&ps) || name.starts_with('.') { continue; }
                if let Some(id) = find_best_match(&name, &unlinked) {
                    if let Some(g) = games.iter().find(|g| g.0 == id) { out.push(InstalledRow { platform_id: g.2.clone(), installed: true, install_path: Some(ps.clone()) }); }
                    unlinked.retain(|(a, _)| *a != id); linked.insert(ps);
                }
            }
        }
        Some(out)
    }
    fn launch(&self, g: &GameRef) -> Result<Launch, String> {
        if g.installed {
            let dir = g.install_path.as_deref().map(std::path::Path::new).filter(|d| d.is_dir()).ok_or("Game folder not found \u{2014} it may have been moved or deleted.")?;
            super::folder::unpack_leftovers(dir);   // PlayDate's `_maybe_unzip`: archives dropped in the folder are unpacked when there is nothing to run
            return super::folder::launch_choice(dir, super::folder::choose_executable(dir, g.executable.as_deref()), &|abs, cwd| self.run_file(abs, cwd, g.appid));
        }
        // not installed: make the game's folder, open it and the page to download from
        let dir = self.dir().join(safe_dirname(&g.name));
        std::fs::create_dir_all(&dir).map_err(|e| format!("Could not make {}: {e}", dir.display()))?;
        let page = Some(g.appname.clone()).filter(|u| !u.is_empty()).or_else(|| Some(g.platform_id.clone()).filter(|p| !p.is_empty()).map(|p| format!("https://www.indiegala.com/library/showcase/{p}")));
        let mut steps = vec![Action::OpenUrl(dir.to_string_lossy().to_string())];
        if let Some(u) = page { steps.push(Action::OpenUrl(u)); }
        steps.push(Action::Message("Game folder opened \u{2014} drop the downloaded files in, then launch again.".into()));
        Ok(Launch { action: Action::Many(steps), record: false })
    }
    fn uninstall(&self, g: &GameRef) -> Result<Action, String> {
        let dir = g.install_path.clone().filter(|p| !p.is_empty()).ok_or_else(|| format!("{} has no folder to remove.", g.name))?;
        Ok(Action::RemoveInstall { delete: crate::installdir::is_inside(std::path::Path::new(&dir), &self.dir()), dir })
    }
    fn store_url(&self, g: &GameRef) -> Option<String> { Some(g.slug.clone()).filter(|s| s.starts_with("http")) }
    /// The library pages the Zest Companion userscript read in the signed-in browser: `{"pages": ["<html>", ...]}`, in order. Nothing is stored as a session.
    fn import_pushed(&self, data: &serde_json::Value) -> Result<Vec<GameRecord>, String> {
        let pages = data.get("pages").and_then(|p| p.as_array()).ok_or("No pages were sent")?;
        let (mut seen, mut out) = (std::collections::HashSet::new(), vec![]);
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        for html in pages.iter().filter_map(|p| p.as_str()) { out.extend(records_from(&parse_page(html), &mut seen, now)); }
        if out.is_empty() { return Err("No games were found on the pages the browser sent".into()); }
        Ok(out)
    }

    fn sync(&self, _cx: &SourceCtx, sink: &mut dyn FnMut(Vec<GameRecord>), cancel: &Cancel) -> Result<usize, String> {
        let cookie = self.cookie().ok_or("Not connected")?;
        let all = self.walk(&cookie, Some(cancel), "IndieGala: reading your library").map_err(|w| match w { Walk::Expired => EXPIRED.to_string(), Walk::Down(m) => m })?;
        if cancel.is_set() { return Ok(0); }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        let records = records_from(&all, &mut Default::default(), now);
        let n = records.len();
        log(&format!("IndieGala sync: {n} games"));
        if n > 0 { sink(records); }
        Ok(n)
    }

    fn rescrape(&self, cx: &SourceCtx, g: &GameRef) -> Fetch<Option<Update>> {
        if g.platform_id.is_empty() { return Ok(None); }
        let Some(game) = self.listed(&g.platform_id) else { return Ok(None) };
        let mut out: Update = vec![("meta_fetched", Sql::Text(cx.today.clone()))];
        if !game.store_url.is_empty() { out.push(("platform_slug", Sql::Text(game.store_url))); }
        if !game.showcase_url.is_empty() { out.push(("platform_appname", Sql::Text(game.showcase_url))); }
        Ok(Some(out))
    }
}


// ---------------------------------------------------------------- install detection and launch (a games folder with one sub-folder per game)

const FUZZY_THRESHOLD: f64 = 0.65;
/// `_normalize_name`: lower case, punctuation as spaces, runs of spaces as one.
pub fn normalize_name(name: &str) -> String {
    static P: OnceLock<Regex> = OnceLock::new(); static W: OnceLock<Regex> = OnceLock::new();
    let n = re(&P, r"[^\w\s]").replace_all(&name.to_lowercase(), " ").to_string();
    re(&W, r"\s+").replace_all(&n, " ").trim().to_string()
}
/// `_find_best_match`: the game (appid) whose name is closest to a folder name, at least 0.65 alike; the first of equals wins.
pub fn find_best_match(folder: &str, candidates: &[(i64, String)]) -> Option<i64> {
    let f = normalize_name(folder);
    let (mut best, mut score) = (None, 0.0);
    for (id, name) in candidates { let s = crate::hltb::sequence_ratio(&f, &normalize_name(name)); if s > score { score = s; best = Some(*id); } }
    if score >= FUZZY_THRESHOLD { best } else { None }
}
/// `_safe_dirname`: a game's folder name: no punctuation but `-` and `.`, at most 80 characters.
pub fn safe_dirname(name: &str) -> String {
    static R: OnceLock<Regex> = OnceLock::new();
    let n: String = re(&R, r"[^\w\s\-.]").replace_all(name, "").trim().chars().take(80).collect();
    if n.is_empty() { "game".into() } else { n }
}

impl IndiegalaSource {
    fn prefix(&self, appid: i64) -> PathBuf { self.dir().join(".prefixes").join(appid.to_string()) }
    fn dir(&self) -> PathBuf { crate::installdir::games_dir(&self.config_dir, "indiegala", crate::installdir::default_dir("IndieGala", "IndieGala")) }
    fn run_file(&self, exe: &std::path::Path, cwd: &std::path::Path, appid: i64) -> Result<Launch, String> { super::folder::run_file(exe, cwd, Some(&self.prefix(appid))) }
}

#[cfg(test)]
mod behaviour {
    use super::*;
    use crate::net::{NetError, Response};
    use serde_json::json;

    const PID1: &str = "0123abcd-0123-abcd-0123-0123456789ab";
    const PID2: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    fn card(pid: &str, name: &str, date: &str, showcase: u32, linked: bool) -> String {
        format!(r#"<li><a href="{}"><figure><img data-src="https://img.indiegala.com/products/{pid}/cover.jpg"></figure><figcaption class="profile-private-page-library-title-padding-showcase"><div class="profile-private-page-library-title">{name}</div>
            <span class="profile-private-page-library-date-showcase">Added</span><span class="profile-private-page-library-date-showcase">{date}</span></figcaption></a><a class="fit-click" onclick="showShowcaseContents('{showcase}', 1)"></a></li>
            <div id="showcase-{showcase}"><a class="library-showcase-title" href="{}">Store</a></div>"#,
            if linked { format!("/library/showcase/{pid}") } else { "#".into() }, if showcase == 7 { "https://store.example.com/game-one".to_string() } else { "https://www.indiegala.com/library/showcase/x".to_string() })
    }
    fn page(cards: &[String]) -> String { format!("<html><body><div class=\"profile-private-page-library\"><ul>{}</ul></div></body></html>", cards.join("")) }

    /// A fake IndieGala: pages 1 and 2 hold games, page 3 is empty; an unknown cookie is sent to the login page.
    struct Site { good: &'static str, fail_page: Option<u32>, asked: Mutex<Vec<String>> }
    impl Http for Site {
        fn get(&self, u: &str, h: &[(&str, &str)], t: Duration) -> Result<Response, NetError> { self.get_final(u, h, t).map(|x| x.0) }
        fn get_final(&self, u: &str, h: &[(&str, &str)], _t: Duration) -> Result<(Response, String), NetError> {
            self.asked.lock().unwrap().push(u.to_string());
            let ok = h.iter().any(|(k, v)| *k == "Cookie" && *v == format!("sessionid={}", self.good));
            let r = |b: String| Ok((Response { status: 200, retry_after: None, body: b }, u.to_string()));
            if !ok { return Ok((Response { status: 200, retry_after: None, body: "<html>login</html>".into() }, "https://www.indiegala.com/login".into())); }
            let n: u32 = if u == LIBRARY_URL { 1 } else { u.trim_end_matches('/').rsplit('/').next().unwrap().parse().unwrap() };
            if self.fail_page == Some(n) { return Err(NetError("timed out".into())); }
            match n { 1 => r(page(&[card(PID1, "Game One", "2020-05-01 12:30:00", 7, true), card(PID2, "Game Two", "garbage", 8, false)])), 2 => r(page(&[card(PID1, "Game One again", "2021-01-01 00:00:00", 7, true)])), _ => r(page(&[])) }
        }
    }
    fn setup(tag: &str, fail_page: Option<u32>, cookie: bool) -> (IndiegalaSource, Arc<Site>, PathBuf) {
        let d = std::env::temp_dir().join(format!("zest_ig_{tag}_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        if cookie { std::fs::write(d.join("config.json"), json!({"indiegala": {"session_cookie": "GOOD", "username": "Connected"}, "other": 1}).to_string()).unwrap(); }
        let site = Arc::new(Site { good: "GOOD", fail_page, asked: Mutex::new(vec![]) });
        (IndiegalaSource::new(site.clone(), d.clone(), d.join("games.db")), site, d)
    }
    fn cx(d: &PathBuf, site: Arc<Site>) -> SourceCtx {
        SourceCtx { http: site, steam: Arc::new(crate::steamclient::Steam::new(Arc::new(crate::net::UreqHttp::new()), None, 10.0)), root: d.clone(), db: d.join("games.db"), today: "2026-11-01".into(), appinfo: Arc::new(|_| Default::default()), account_id: None }
    }
    fn g(id: &str) -> GameRef { GameRef { appid: -5, name: "x".into(), platform: "indiegala".into(), platform_id: id.into(), ..Default::default() } }

    #[test] fn the_listing_page_gives_ids_covers_dates_showcase_and_store_links() {
        let games = parse_page(&page(&[card(PID1, "Game One", "2020-05-01 12:30:00", 7, true), card(PID2, "Game Two", "x", 8, false)]));
        assert_eq!(games.len(), 2);
        assert_eq!(games[0], Listed { prod_id: PID1.into(), prod_name: "Game One".into(), date_str: "2020-05-01 12:30:00".into(), img_url: format!("https://img.indiegala.com/products/{PID1}/cover.jpg"),
            showcase_url: format!("/library/showcase/{PID1}").replace("/library", "https://www.indiegala.com/library"), store_url: "https://store.example.com/game-one".into() });
        // no link to the showcase page: built from the product id; a store link back into IndieGala's library is not a store page
        assert_eq!(games[1].showcase_url, format!("https://www.indiegala.com/library/showcase/{PID2}")); assert_eq!(games[1].store_url, "");
        assert_eq!(parse_dt("2020-05-01 12:30:00"), Some(1588336200)); assert_eq!(parse_dt("garbage"), None);
    }
    #[test] fn pages_read_in_the_browser_become_the_same_records_as_a_sync() {
        let (s, _, _) = setup("push", None, false);
        let pages = json!({"pages": [page(&[card(PID1, "Game One", "2020-05-01 12:30:00", 7, true)]), page(&[card(PID1, "Game One again", "2021-01-01 00:00:00", 7, true), card(PID2, "Game Two", "2020-06-01 00:00:00", 8, false)]), "not html"]});
        let r = s.import_pushed(&pages).unwrap();
        assert_eq!(r.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(), ["Game One", "Game Two"]);
        assert_eq!(r[0].slug.as_deref(), Some("https://store.example.com/game-one"));
        assert!(s.import_pushed(&json!({"pages": ["<html></html>"]})).is_err() && s.import_pushed(&json!({})).is_err());
    }
    /// Read-only look at the real library page (for working out the download links): `ZEST_DATA=<data dir> ZEST_OUT=<file> cargo test -p pd-filter real_indiegala_page -- --ignored`.
    #[test] #[ignore] fn real_indiegala_page() {
        let (Some(dir), Some(out)) = (std::env::var_os("ZEST_DATA"), std::env::var_os("ZEST_OUT")) else { return };
        let dir = PathBuf::from(dir);
        if let Some(v) = crate::secrets::platform_vault() { crate::secrets::install(v); }   // the saved sign-in lives in the system's keyring, as in the app
        let s = IndiegalaSource::new(Arc::new(crate::net::UreqHttp::new()), dir.clone(), dir.join("games.db"));
        let cookie = s.cookie().expect("no IndieGala sign-in saved");
        if let Ok(id) = std::env::var("ZEST_GAME") { println!("downloads of {id}: {:?}", s.downloads_of(&id)); return; }
        let url = std::env::var("ZEST_URL").unwrap_or_else(|_| LIBRARY_URL.to_string());
        let (r, fin) = s.fetch(&url, &cookie, 30).unwrap();
        println!("HTTP {} from {}", r.status, fin);
        std::fs::write(out, r.body).unwrap();
    }
    #[test] fn download_buttons_are_read_per_game_and_system_and_only_from_indiegalas_servers() {
        let btn = |url: &str, icon: &str| format!(r##"<div class="profile-private-sub-section-col-inner"><a class="library-showcase-download-btn bg-gradient-red" href="#" onclick="location.href='{url}'" target="_blank"><i class="fa fa-{icon}"></i></a></div>"##);
        let html = [btn(&format!("https://content.indiegalacdn.com/DevShowcaseBuildsVolume/dev_fold_freebies/{PID1}/win/one_win.zip"), "windows"),
            btn(&format!("https://content.indiegalacdn.com/DevShowcaseBuildsVolume/dev_fold_freebies/{PID1}/mac/one_mac.zip"), "apple"),
            btn(&format!("https://content.indiegalacdn.com/x/{}/lin/two.tar.gz", PID2.to_uppercase()), "linux"),
            btn(&format!("https://evil.example/{PID2}/win/x.zip"), "windows"), btn(&format!("http://content.indiegalacdn.com/{PID2}/w.zip"), "windows"),
            btn(&format!("https://content.indiegalacdn.com/{PID2}/a.apk"), "android"), btn("https://content.indiegalacdn.com/common/galaK_win32.zip", "windows")].join("");
        let got = parse_downloads(&html);
        assert_eq!(got.iter().map(|(id, os, u)| (id.as_str(), *os, u.rsplit('/').next().unwrap())).collect::<Vec<_>>(),
            [(PID1, "windows", "one_win.zip"), (PID1, "mac", "one_mac.zip"), (PID2, "linux", "two.tar.gz")], "another host, plain http, an unknown system and a link with no game id are left out");
    }
    #[test] fn launch_runs_the_games_best_file_or_opens_its_folder_and_the_download_page() {
        let (s, _, d) = setup("launch", None, true);
        std::fs::write(d.join("config.json"), json!({"indiegala": {"session_cookie": "GOOD", "games_dir": d.join("games").to_string_lossy()}}).to_string()).unwrap();
        let elf = [0x7fu8, b'E', b'L', b'F', 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let gdir = d.join("games/Game One"); std::fs::create_dir_all(&gdir).unwrap(); std::fs::write(gdir.join("run.x86_64"), elf).unwrap();
        let gr = GameRef { appid: -5, name: "Game One".into(), platform: "indiegala".into(), platform_id: PID1.into(), installed: true, install_path: Some(gdir.to_string_lossy().into()), ..Default::default() };
        let l = s.launch(&gr).unwrap(); assert!(l.record);
        assert_eq!(l.action, Action::Spawn { program: gdir.join("run.x86_64").to_string_lossy().into(), args: vec![], dir: Some(gdir.to_string_lossy().into()), env: vec![] });
        // nothing to run: the folder is opened, no launch recorded
        let empty = d.join("games/Empty"); std::fs::create_dir_all(&empty).unwrap();
        let l = s.launch(&GameRef { install_path: Some(empty.to_string_lossy().into()), ..gr.clone() }).unwrap();
        assert_eq!((l.action, l.record), (Action::OpenUrl(empty.to_string_lossy().into()), false));
        assert!(s.launch(&GameRef { install_path: Some("/gone".into()), ..gr.clone() }).unwrap_err().contains("not found"));
        // not installed: a folder named for the game is made and opened with the download page
        let l = s.launch(&GameRef { installed: false, install_path: None, name: "Brand: New/Game!".into(), appname: "https://www.indiegala.com/library/showcase/x".into(), ..gr.clone() }).unwrap();
        let made = d.join("games/Brand NewGame"); assert!(made.is_dir());
        match l.action { Action::Many(v) => assert_eq!(v[..2], [Action::OpenUrl(made.to_string_lossy().into()), Action::OpenUrl("https://www.indiegala.com/library/showcase/x".into())]), a => panic!("{a:?}") }
        // uninstall removes files only inside the games folder
        assert_eq!(s.uninstall(&GameRef { install_path: Some(gdir.to_string_lossy().into()), ..gr.clone() }).unwrap(), Action::RemoveInstall { dir: gdir.to_string_lossy().into(), delete: true });
        assert_eq!(s.uninstall(&GameRef { install_path: Some("/elsewhere/Game".into()), ..gr.clone() }).unwrap(), Action::RemoveInstall { dir: "/elsewhere/Game".into(), delete: false });
    }
    #[test] fn the_games_folder_scan_links_folders_to_games_by_name_and_clears_missing_ones() {
        let (s, _, d) = setup("scan", None, true);
        std::fs::write(d.join("config.json"), json!({"indiegala": {"session_cookie": "GOOD", "games_dir": d.join("games").to_string_lossy()}}).to_string()).unwrap();
        for n in ["Game One", "Stuff That Matches Nothing", ".hidden", "game two!", "Empty Five"] { std::fs::create_dir_all(d.join("games").join(n)).unwrap(); }
        for n in ["Game One", "Stuff That Matches Nothing", ".hidden", "game two!"] { std::fs::write(d.join("games").join(n).join("game.exe"), "x").unwrap(); }
        // an empty folder (the "drop the files in here" folder an older Zest made) or one holding only dot files is not an install
        let empty6 = d.join("games/Empty Six"); std::fs::create_dir_all(&empty6).unwrap(); std::fs::write(empty6.join(".directory"), "x").unwrap();
        let c = rusqlite::Connection::open(d.join("games.db")).unwrap();
        c.execute_batch("CREATE TABLE games (appid INTEGER PRIMARY KEY, name TEXT, platform TEXT, platform_id TEXT, install_path TEXT);").unwrap();
        let linked = d.join("games/Game One");
        c.execute("INSERT INTO games VALUES (-1,'Game One','indiegala','p1',?), (-2,'Game Two','indiegala','p2',NULL), (-3,'Lost','indiegala','p3','/gone'), (-4,'Unmatched','indiegala','p4',NULL), (-5,'Empty Five','indiegala','p5',NULL), (-6,'Empty Six','indiegala','p6',?)", [linked.to_string_lossy(), empty6.to_string_lossy()]).unwrap();
        let rows = s.installed_scan().unwrap();
        let by = |id: &str| rows.iter().find(|r| r.platform_id == id).cloned();
        assert_eq!(by("p1").unwrap(), InstalledRow { platform_id: "p1".into(), installed: true, install_path: None });
        assert_eq!(by("p2").unwrap(), InstalledRow { platform_id: "p2".into(), installed: true, install_path: Some(d.join("games/game two!").to_string_lossy().into()) });
        assert_eq!(by("p3").unwrap(), InstalledRow { platform_id: "p3".into(), installed: false, install_path: Some(String::new()) });
        assert!(by("p4").is_none());
        assert!(by("p5").is_none(), "an empty folder is not linked to a game");
        assert_eq!(by("p6").unwrap(), InstalledRow { platform_id: "p6".into(), installed: false, install_path: Some(String::new()) });
    }
    #[test] fn the_account_name_is_read_from_the_library_page() {
        assert_eq!(IndiegalaSource::username_in(r#"<span class=\"username-text\">Some Name</span>"#.replace("\\\"", "\"").as_str()).as_deref(), Some("Some Name"));
        assert_eq!(IndiegalaSource::username_in("<html>nothing</html>"), None);
    }
    #[test] fn sign_in_needs_the_library_page_and_keeps_only_the_cookie() {
        let (s, _, d) = setup("auth", None, false);
        assert!(!s.auth_status().unwrap().connected);
        assert!(s.auth_complete(" ").is_err());
        assert!(s.auth_complete("BAD").unwrap_err().contains("appears invalid"));
        assert!(s.auth_complete("\"GOOD\"").unwrap().connected);
        let cfg: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(d.join("config.json")).unwrap()).unwrap();
        assert_eq!(cfg["indiegala"], json!({"session_cookie": "GOOD", "username": "Connected"}));
        assert!(s.auth_status().unwrap().connected);
        // the cookie stops working: the dialog shows signed out
        std::fs::write(d.join("config.json"), json!({"indiegala": {"session_cookie": "STALE"}}).to_string()).unwrap();
        assert!(!s.auth_status().unwrap().connected);
        s.auth_disconnect().unwrap(); assert!(s.cookie().is_none());
    }
    #[test] fn sync_reads_pages_until_an_empty_one_and_lists_each_game_once() {
        let (s, site, d) = setup("sync", None, true);
        let mut got = vec![];
        assert_eq!(s.sync(&cx(&d, site.clone()), &mut |b| got.extend(b), &Cancel::new()).unwrap(), 2);
        assert_eq!(got.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["Game One", "Game Two"]);
        assert_eq!((got[0].date_added, got[0].slug.as_deref()), (Some(1588336200), Some("https://store.example.com/game-one")));
        assert!(got[1].date_added.unwrap() > 1_700_000_000);   // no readable date: now
        assert_eq!(site.asked.lock().unwrap().len(), 3);
        assert_eq!(site.asked.lock().unwrap()[1], "https://www.indiegala.com/library/showcase/2/");
    }
    #[test] fn sync_failures_are_reported_and_a_later_page_failing_keeps_what_was_read() {
        let (s, site, d) = setup("nocookie", None, false);
        assert_eq!(s.sync(&cx(&d, site), &mut |_| {}, &Cancel::new()).unwrap_err(), "Not connected");
        let (s, site, d) = setup("expired", None, true);
        std::fs::write(d.join("config.json"), json!({"indiegala": {"session_cookie": "STALE"}}).to_string()).unwrap();
        assert_eq!(s.sync(&cx(&d, site), &mut |_| {}, &Cancel::new()).unwrap_err(), EXPIRED);
        let (s, site, d) = setup("down1", Some(1), true);
        assert!(s.sync(&cx(&d, site), &mut |_| {}, &Cancel::new()).unwrap_err().starts_with("Connection failed"));
        let (s, site, d) = setup("down2", Some(2), true);
        let mut got = vec![]; assert_eq!(s.sync(&cx(&d, site), &mut |b| got.extend(b), &Cancel::new()).unwrap(), 2);
        let (s, site, d) = setup("cancel", None, true);
        let c = Cancel::new(); c.cancel(); let mut called = false;
        assert_eq!(s.sync(&cx(&d, site), &mut |_| called = true, &c).unwrap(), 0); assert!(!called);
    }
    #[test] fn rescrape_and_cover_share_one_walk_and_stamp_links() {
        let (s, site, d) = setup("rescrape", None, true);
        let u = s.rescrape(&cx(&d, site.clone()), &g(PID1)).unwrap().unwrap();
        let get = |k: &str| u.iter().find(|(c, _)| *c == k).map(|(_, v)| v.clone());
        assert_eq!(get("platform_slug"), Some(Sql::Text("https://store.example.com/game-one".into()))); assert_eq!(get("meta_fetched"), Some(Sql::Text("2026-11-01".into())));
        assert!(get("platform_appname").is_some());
        assert_eq!(s.art_urls(PID2), vec![("vertical", format!("https://img.indiegala.com/products/{PID2}/cover.jpg"))]);
        assert_eq!(LibrarySource::art_urls(&s, &g(PID2)), vec![("vertical".to_string(), format!("https://img.indiegala.com/products/{PID2}/cover.jpg"))]);
        assert_eq!(site.asked.lock().unwrap().len(), 3);   // one walk served both
        assert!(s.rescrape(&cx(&d, site), &g("unknown")).unwrap().is_none());
        assert!(s.caps().sync && s.caps().auth && !s.enrich_new());
    }
}
