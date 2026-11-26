//! A plugin as a `LibrarySource`: the same interface the built-in sources have, answered over the plugin protocol. The plugin process starts on the first
//! request and is stopped after `idle` without work; one that dies or hangs is started again by the next request. Everything a plugin returns is validated
//! here (records through `GameRecord::from_json`, `game.rescrape` columns through a whitelist), so a plugin cannot write anything the protocol does not allow.
use super::manifest::{Manifest, CORE_API};
use super::rpc::{Host, PluginProc, RpcError};
use crate::importer::GameRecord;
use crate::jobs::Update;
use crate::metadata::PageMeta;
use crate::net::{log, Cancel};
use crate::sources::{Action, Caps, GameRef, Launch, LibrarySource, SourceCtx};
use crate::steamclient::{Fetch, RateLimited};
use rusqlite::types::Value as Sql;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The columns `game.rescrape` may set, with their kind: 0 text, 1 integer, 2 list (an array or a comma string), 3 boolean.
const META: &[(&str, u8)] = &[("name", 0), ("developers", 2), ("publishers", 2), ("genres", 2), ("categories", 2), ("tags", 2), ("release_date", 1), ("short_description", 0), ("review_score", 0),
    ("review_percentage", 1), ("weighted_percentage", 1), ("total_reviews", 1), ("positive_reviews", 1), ("total_achievements", 1), ("unlocked_achievements", 1), ("playtime_forever", 1),
    ("last_played", 1), ("is_free", 3), ("metacritic_score", 1), ("platform_slug", 0), ("platform_appname", 0), ("platform_ns", 0), ("installed", 3), ("install_path", 0), ("meta_fetched", 0), ("cheevos_fetched", 0)];

/// `game.rescrape`'s `meta` to column updates; a column that is not on the list, or a value of the wrong type, is dropped (and logged).
pub fn meta_to_update(plugin: &str, meta: &Value) -> Update {
    let mut out: Update = vec![];
    let Some(o) = meta.as_object() else { return out };
    for (k, v) in o {
        let Some((col, kind)) = META.iter().find(|(c, _)| c == k) else { log(&format!("[plugin {plugin}] game.rescrape: column {k:?} is not allowed, dropped")); continue };
        let val = match (kind, v) {
            (_, Value::Null) if *col != "name" => Sql::Null,
            (0, Value::String(t)) if !(*col == "name" && t.trim().is_empty()) => Sql::Text(t.chars().take(20000).collect()),
            (1, x) if x.is_number() => Sql::Integer(x.as_i64().or_else(|| x.as_f64().map(|f| f as i64)).unwrap_or(0)),
            (2, x) => match crate::importer::list(&json!({"x": x}), "x") { Some(l) => Sql::Text(l), None => Sql::Null },
            (3, Value::Bool(b)) => Sql::Integer(*b as i64),
            (3, Value::Number(n)) => Sql::Integer((n.as_i64() == Some(1)) as i64),
            _ => { log(&format!("[plugin {plugin}] game.rescrape: wrong type for {col}, dropped")); continue }
        };
        out.push((col, val));
    }
    out
}

fn game_json(g: &GameRef) -> Value { json!({"appid": g.appid, "name": g.name, "platform_id": g.platform_id, "slug": g.slug, "appname": g.appname, "installed": g.installed, "install_path": g.install_path}) }

fn parse_action(v: &Value) -> Result<Action, String> {
    let a = v.get("action").unwrap_or(v);
    match a.get("type").and_then(|t| t.as_str()) {
        Some("open_url") => { let u = a.get("url").and_then(|x| x.as_str()).unwrap_or(""); if u.starts_with("https://") || u.starts_with("http://") || u.contains("://") && !u.starts_with("file:") { Ok(Action::OpenUrl(u.to_string())) } else { Err("the plugin gave an address Zest will not open".into()) } }
        Some("spawn") => Ok(Action::Spawn { dir: None, env: vec![], program: a.get("program").and_then(|x| x.as_str()).ok_or("spawn without a program")?.to_string(), args: a.get("args").and_then(|x| x.as_array()).map(|l| l.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default() }),
        Some("message") => Ok(Action::Message(a.get("text").and_then(|x| x.as_str()).unwrap_or("").to_string())),
        _ => Err("the plugin gave no action".into()),
    }
}

struct Inner {
    m: Manifest, config_dir: PathBuf, db: PathBuf, idle: Duration,
    proc: Mutex<Option<Arc<PluginProc>>>, last: Mutex<Instant>, busy: AtomicUsize,
    sink: Mutex<Option<Box<dyn FnMut(Vec<GameRecord>) + Send>>>, rejected: AtomicUsize, progress: Mutex<Option<(u64, Option<u64>, String)>>,
}

/// The plugin's side of a request: its secrets, the known ids, its games and progress.
struct HostImpl(Arc<Inner>);
impl Host for HostImpl {
    fn request(&self, method: &str, p: &Value) -> Result<Value, String> {
        let i = &self.0;
        let key = || p.get("key").and_then(|k| k.as_str()).filter(|k| !k.is_empty() && k.len() <= 100).ok_or_else(|| "key is missing".to_string());
        match method {
            "secrets.get" => Ok(json!({"value": crate::account::plugin_secret_get(&i.config_dir, &i.m.id, key()?)})),
            "secrets.set" => { let v = p.get("value").and_then(|v| v.as_str()).ok_or("value must be a string")?; crate::account::plugin_secret_set(&i.config_dir, &i.m.id, key()?, Some(v)).map_err(|e| format!("could not save: {}", e.kind() as u8))?; Ok(json!({})) }
            "secrets.delete" => { crate::account::plugin_secret_set(&i.config_dir, &i.m.id, key()?, None).map_err(|_| "could not save".to_string())?; Ok(json!({})) }
            "core.known_ids" => {
                let c = rusqlite::Connection::open_with_flags(&i.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|_| "the library is not available".to_string())?;
                let mut st = c.prepare("SELECT platform_id FROM games WHERE platform = ?").map_err(|e| e.to_string())?;
                // not the emulated games that share a platform value with a store (see `sources::origin`)
                let ids: Vec<String> = st.query_map([&i.m.platform], |r| r.get::<_, rusqlite::types::Value>(0)).map_err(|e| e.to_string())?.flatten().filter_map(|v| match v { Sql::Text(t) => Some(t), Sql::Integer(n) => Some(n.to_string()), _ => None })
                    .filter(|id| crate::sources::origin(&i.m.platform, id) == crate::sources::Origin::Store).collect();
                Ok(json!({"ids": ids}))
            }
            m => Err(format!("unknown method {m}")),
        }
    }
    fn notify(&self, method: &str, p: &Value) {
        let i = &self.0;
        match method {
            "library.games" => {
                let mut ok = vec![];
                for g in p.get("games").and_then(|g| g.as_array()).into_iter().flatten() {
                    match GameRecord::from_json(g) { Ok(r) => ok.push(r), Err(e) => { i.rejected.fetch_add(1, Ordering::SeqCst); log(&format!("[plugin {}] a game was rejected: {e}", i.m.id)); } }
                }
                if let Some(f) = i.sink.lock().unwrap().as_mut() { f(ok); }
            }
            "progress" => *i.progress.lock().unwrap() = Some((p.get("done").and_then(|d| d.as_u64()).unwrap_or(0), p.get("total").and_then(|t| t.as_u64()), p.get("message").and_then(|m| m.as_str()).unwrap_or("").to_string())),
            "log" => log(&format!("[plugin {}] {}", i.m.id, p.get("message").and_then(|m| m.as_str()).unwrap_or(""))),
            _ => {}
        }
    }
}

pub struct PluginSource { inner: Arc<Inner> }

struct BusyGuard<'a>(&'a Inner);
impl<'a> BusyGuard<'a> { fn new(i: &'a Inner) -> Self { i.busy.fetch_add(1, Ordering::SeqCst); BusyGuard(i) } }
impl Drop for BusyGuard<'_> { fn drop(&mut self) { *self.0.last.lock().unwrap() = Instant::now(); self.0.busy.fetch_sub(1, Ordering::SeqCst); } }

impl PluginSource {
    /// `config_dir` holds `config.json` (secrets), `db` is the library database (for `core.known_ids`).
    pub fn new(m: Manifest, config_dir: PathBuf, db: PathBuf) -> Self { Self::with_idle(m, config_dir, db, Duration::from_secs(30)) }
    pub fn with_idle(m: Manifest, config_dir: PathBuf, db: PathBuf, idle: Duration) -> Self {
        PluginSource { inner: Arc::new(Inner { m, config_dir, db, idle, proc: Mutex::new(None), last: Mutex::new(Instant::now()), busy: AtomicUsize::new(0), sink: Mutex::new(None), rejected: AtomicUsize::new(0), progress: Mutex::new(None) }) }
    }
    pub fn manifest(&self) -> &Manifest { &self.inner.m }
    /// Is the process running right now (for tests and the Plugins screen).
    pub fn running(&self) -> bool { self.inner.proc.lock().unwrap().as_ref().map_or(false, |p| p.is_alive()) }
    /// The plugin's latest progress report: (done, total, message).
    pub fn progress(&self) -> Option<(u64, Option<u64>, String)> { self.inner.progress.lock().unwrap().clone() }
    /// Games a plugin sent that failed validation, since this source was created.
    pub fn rejected(&self) -> usize { self.inner.rejected.load(Ordering::SeqCst) }
    pub fn stop(&self) { if let Some(p) = self.inner.proc.lock().unwrap().take() { p.stop(); } }

    fn proc(&self) -> Result<Arc<PluginProc>, RpcError> {
        let i = &self.inner;
        let mut g = i.proc.lock().unwrap();
        if let Some(p) = g.as_ref().filter(|p| p.is_alive()) { return Ok(p.clone()); }
        let p = PluginProc::start(&i.m, Arc::new(HostImpl(i.clone()))).map_err(|e| RpcError { code: -32001, message: e })?;
        let d = p.call("describe", json!({"core_api": CORE_API}), Some(Duration::from_secs(20)), None)?;
        let api = d.get("api").and_then(|a| a.as_u64()).unwrap_or(0) as u32;
        if api == 0 || api > CORE_API { p.stop(); return Err(RpcError { code: -32004, message: format!("the plugin speaks protocol {api}, this Zest speaks {CORE_API}") }); }
        *g = Some(p.clone());
        // stop it when it has been idle for a while
        let weak = Arc::downgrade(i);
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(200));
            let Some(i) = weak.upgrade() else { return };
            let alive = i.proc.lock().unwrap().as_ref().map_or(false, |p| p.is_alive());
            if !alive { return; }
            if i.busy.load(Ordering::SeqCst) == 0 && i.last.lock().unwrap().elapsed() > i.idle { if let Some(p) = i.proc.lock().unwrap().take() { p.stop(); } return; }
        });
        Ok(p)
    }

    /// A request to the plugin (starting it first when needed).
    pub fn call(&self, method: &str, params: Value, timeout: Option<Duration>, cancel: Option<&Cancel>) -> Result<Value, RpcError> {
        let _busy = BusyGuard::new(&self.inner);
        let p = self.proc()?;
        let r = p.call(method, params, timeout, cancel);
        if matches!(&r, Err(e) if e.code == -32001 || e.code == -32002) { *self.inner.proc.lock().unwrap() = None; }
        r
    }
    fn quick(&self, method: &str, params: Value) -> Result<Value, RpcError> { self.call(method, params, Some(Duration::from_secs(60)), None) }

    // ---- not part of the shared interface yet: sign-in and install watching
    pub fn auth_status(&self) -> Result<Value, RpcError> { self.quick("auth.status", json!({})) }
    pub fn auth_begin(&self) -> Result<Value, RpcError> { self.quick("auth.begin", json!({})) }
    pub fn auth_complete(&self, value: &str) -> Result<Value, RpcError> { self.quick("auth.complete", json!({"value": value})) }
    pub fn auth_disconnect(&self) -> Result<Value, RpcError> { self.quick("auth.disconnect", json!({})) }
    pub fn installed_paths(&self) -> Vec<String> { self.quick("installed.paths", json!({})).ok().and_then(|v| v.get("paths").and_then(|p| p.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())).unwrap_or_default() }
    pub fn installed_scan(&self) -> Result<Vec<(String, Option<String>)>, RpcError> {
        let v = self.quick("installed.scan", json!({}))?;
        Ok(v.get("installed").and_then(|a| a.as_array()).map(|a| a.iter().filter_map(|x| Some((x.get("platform_id").and_then(|p| p.as_str().map(String::from).or_else(|| p.as_i64().map(|n| n.to_string())))?, x.get("install_path").and_then(|p| p.as_str()).map(String::from)))).collect()).unwrap_or_default())
    }
    pub fn art_urls(&self, g: &GameRef) -> Vec<(String, String)> {
        self.quick("game.art_urls", json!({"game": game_json(g)})).ok().and_then(|v| v.as_object().map(|o| o.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).filter(|(k, _)| ["vertical", "horizontal", "icon"].contains(&k.as_str())).collect())).unwrap_or_default()
    }
}

impl LibrarySource for PluginSource {
    fn id(&self) -> &str { &self.inner.m.platform }
    fn label(&self) -> &str { &self.inner.m.name }
    fn caps(&self) -> Caps {
        let m = &self.inner.m;
        Caps { auth: m.can("auth"), sync: m.can("sync"), rescrape: m.can("rescrape"), launch: m.can("launch"), uninstall: m.can("uninstall"), installed_scan: m.can("installed_scan"), description: m.can("description"), art: m.can("art"),
            store_page_metadata: m.can("store_page_metadata"), store_page: m.store_url.is_some(), ..Default::default() }
    }
    fn auth_status(&self) -> Result<crate::sources::AuthStatus, String> {
        let v = PluginSource::auth_status(self).map_err(|e| e.message)?;
        Ok(crate::sources::AuthStatus { connected: v.get("connected").and_then(|c| c.as_bool()).unwrap_or(false), username: v.get("username").and_then(|u| u.as_str()).map(String::from) })
    }
    fn auth_begin(&self) -> Result<crate::sources::AuthBegin, String> {
        let v = PluginSource::auth_begin(self).map_err(|e| e.message)?;
        let url = v.get("url").and_then(|u| u.as_str()).filter(|u| u.starts_with("https://") || u.starts_with("http://")).ok_or("the plugin gave no sign-in address")?.to_string();
        Ok(crate::sources::AuthBegin { url, instructions: v.get("instructions").and_then(|a| a.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default(), placeholder: v.get("placeholder").and_then(|p| p.as_str()).unwrap_or("paste here").to_string() })
    }
    fn auth_complete(&self, value: &str) -> Result<crate::sources::AuthStatus, String> {
        let v = PluginSource::auth_complete(self, value).map_err(|e| e.message)?;
        Ok(crate::sources::AuthStatus { connected: v.get("connected").and_then(|c| c.as_bool()).unwrap_or(false), username: v.get("username").and_then(|u| u.as_str()).map(String::from) })
    }
    fn auth_disconnect(&self) -> Result<(), String> { PluginSource::auth_disconnect(self).map(|_| ()).map_err(|e| e.message) }
    fn store_url(&self, g: &GameRef) -> Option<String> {
        let t = self.inner.m.store_url.as_ref()?;
        if g.slug.is_empty() && t.contains("{slug}") { return None; }
        Some(t.replace("{slug}", &g.slug).replace("{platform_id}", &g.platform_id).replace("{appid}", &g.appid.to_string()))
    }
    fn launch(&self, g: &GameRef) -> Result<Launch, String> {
        if !self.inner.m.can("launch") { return Err(format!("{} cannot launch games", self.label())); }
        let v = self.quick("game.launch", json!({"game": game_json(g)})).map_err(|e| e.message)?;
        Ok(Launch { action: parse_action(&v)?, record: v.get("record").and_then(|r| r.as_bool()).unwrap_or(false) })
    }
    fn uninstall(&self, g: &GameRef) -> Result<Action, String> {
        if !self.inner.m.can("uninstall") { return Err(format!("{} cannot uninstall games", self.label())); }
        parse_action(&self.quick("game.uninstall", json!({"game": game_json(g)})).map_err(|e| e.message)?)
    }
    fn sync(&self, _cx: &SourceCtx, sink: &mut dyn FnMut(Vec<GameRecord>), cancel: &Cancel) -> Result<usize, String> {
        if !self.inner.m.can("sync") { return Err(format!("{} cannot list a library", self.label())); }
        // the notifications arrive on the reader thread: hand them over through a channel and deliver on this thread
        let (tx, rx) = std::sync::mpsc::channel::<Vec<GameRecord>>();
        *self.inner.sink.lock().unwrap() = Some(Box::new(move |g| { let _ = tx.send(g); }));
        let me = PluginSource { inner: self.inner.clone() };
        let c2 = cancel.clone();
        let h = std::thread::spawn(move || me.call("library.sync", json!({}), None, Some(&c2)));
        let mut total = 0usize;
        loop {
            match rx.recv_timeout(Duration::from_millis(50)) { Ok(b) => { total += b.len(); sink(b); } Err(_) => { if h.is_finished() { break; } } }
        }
        while let Ok(b) = rx.try_recv() { total += b.len(); sink(b); }
        *self.inner.sink.lock().unwrap() = None;
        let r = h.join().map_err(|_| "the sync stopped unexpectedly".to_string())?.map_err(|e| e.message)?;
        Ok(r.get("count").and_then(|c| c.as_u64()).map(|c| c as usize).unwrap_or(total))
    }
    fn rescrape(&self, _cx: &SourceCtx, g: &GameRef) -> Fetch<Option<Update>> {
        if !self.inner.m.can("rescrape") { return Ok(None); }
        match self.quick("game.rescrape", json!({"game": game_json(g)})) {
            Ok(v) => { let u = meta_to_update(&self.inner.m.id, v.get("meta").unwrap_or(&v)); Ok((!u.is_empty()).then_some(u)) }
            // a plugin that reports rate limiting says so with this code; nothing else is a retry
            Err(e) if e.code == 429 => Err(RateLimited(None)),
            Err(e) => { log(&format!("[plugin {}] rescrape failed: {}", self.inner.m.id, e.message)); Ok(None) }
        }
    }
    fn description(&self, _cx: &SourceCtx, g: &GameRef) -> Option<String> {
        if !self.inner.m.can("description") { return None; }
        self.quick("game.description", json!({"game": game_json(g)})).ok().and_then(|v| v.get("text").and_then(|t| t.as_str()).map(String::from).or_else(|| v.as_str().map(String::from))).filter(|t| !t.trim().is_empty())
    }
    fn store_page_metadata(&self, slug: &str) -> PageMeta {
        if !self.inner.m.can("store_page_metadata") { return PageMeta::default(); }
        let Ok(v) = self.quick("game.store_page_metadata", json!({"slug": slug})) else { return PageMeta::default() };
        let l = |k: &str| crate::importer::list(&v, k);
        PageMeta { genres: l("genres"), categories: l("categories"), tags: l("tags"), developers: l("developers"), publishers: l("publishers"), release_date: v.get("release_date").and_then(|d| d.as_i64()) }
    }
}

impl Drop for PluginSource { fn drop(&mut self) { if Arc::strong_count(&self.inner) == 1 { self.stop(); } } }

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn only_allowed_columns_with_the_right_types_get_through() {
        let u = meta_to_update("t", &json!({"developers": ["A", "B, C"], "release_date": 5, "installed": true, "name": "  ", "review_percentage": "x", "appid": 3, "tags": null, "short_description": "d"}));
        let cols: Vec<&str> = u.iter().map(|(c, _)| *c).collect();
        assert!(cols.contains(&"developers") && cols.contains(&"release_date") && cols.contains(&"installed") && cols.contains(&"tags") && cols.contains(&"short_description"));
        assert!(!cols.contains(&"name") && !cols.contains(&"review_percentage") && !cols.contains(&"appid"));
        assert_eq!(u.iter().find(|(c, _)| *c == "developers").unwrap().1, Sql::Text("A,B C".into()));
        assert_eq!(u.iter().find(|(c, _)| *c == "installed").unwrap().1, Sql::Integer(1));
    }
    #[test] fn actions_are_checked() {
        assert_eq!(parse_action(&json!({"action": {"type": "open_url", "url": "https://x/y"}})).unwrap(), Action::OpenUrl("https://x/y".into()));
        assert!(parse_action(&json!({"action": {"type": "open_url", "url": "file:///etc/passwd"}})).is_err());
        assert!(parse_action(&json!({"action": {"type": "open_url", "url": "javascript:alert(1)"}})).is_err());
        assert_eq!(parse_action(&json!({"type": "spawn", "program": "/g/run", "args": ["-x"]})).unwrap(), Action::Spawn { program: "/g/run".into(), args: vec!["-x".into()], dir: None, env: vec![] });
        assert!(parse_action(&json!({})).is_err());
    }
}
