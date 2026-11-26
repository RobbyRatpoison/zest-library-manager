//! The plugin host against the real toy plugin (`plugin-sdk/examples/toy`, a Python process speaking the protocol). Skipped when `python3` is missing.
use pd_filter::importer::{import, GameRecord};
use pd_filter::net::Cancel;
use pd_filter::plugin::adapter::PluginSource;
use pd_filter::plugin::manifest::{discover, Manifest};
use pd_filter::sources::{Action, GameRef, LibrarySource, SourceCtx};
use rusqlite::{types::Value as Sql, Connection};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn toy_dir() -> PathBuf { Path::new(env!("CARGO_MANIFEST_DIR")).join("../plugin-sdk/examples/toy") }
fn have_python() -> bool { std::process::Command::new("python3").arg("--version").output().is_ok() }

struct Env { dir: PathBuf, db: PathBuf }
fn env(tag: &str) -> Env {
    let dir = std::env::temp_dir().join(format!("zest_toy_{tag}_{}", std::process::id())); let _ = std::fs::remove_dir_all(&dir); std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.json"), r#"{"active_account":"1","accounts":{}}"#).unwrap();
    let db = dir.join("games_1.db");
    Connection::open(&db).unwrap().execute_batch("CREATE TABLE games (appid INTEGER PRIMARY KEY, name TEXT, platform TEXT, platform_id TEXT, platform_slug TEXT, platform_appname TEXT, platform_ns TEXT, date_added INTEGER, completion_status TEXT, installed INTEGER, install_path TEXT, playtime_forever INTEGER, last_played INTEGER,
        release_date INTEGER, developers TEXT, publishers TEXT, genres TEXT, categories TEXT, tags TEXT, short_description TEXT, art_fetched TEXT, meta_fetched TEXT, cheevos_fetched TEXT, protondb_fetched TEXT, hltb_fetched TEXT);
        CREATE TABLE blacklist (appid INTEGER PRIMARY KEY, name TEXT, date_blacklisted TEXT, platform_id TEXT, platform TEXT);").unwrap();
    Env { dir, db }
}
fn source(e: &Env, idle: Option<Duration>) -> PluginSource {
    let m = Manifest::load(&toy_dir()).unwrap();
    match idle { Some(i) => PluginSource::with_idle(m, e.dir.clone(), e.db.clone(), i), None => PluginSource::new(m, e.dir.clone(), e.db.clone()) }
}
fn cx(e: &Env) -> SourceCtx {
    let http: Arc<dyn pd_filter::net::Http> = Arc::new(pd_filter::net::UreqHttp::new());
    SourceCtx { http: http.clone(), steam: Arc::new(pd_filter::steamclient::Steam::new(http, None, 10.0)), root: e.dir.clone(), db: e.db.clone(), today: "2026-11-01".into(), appinfo: Arc::new(|_| Default::default()), account_id: None }
}
fn sign_in(s: &PluginSource) { s.auth_complete("letmein").unwrap(); }
fn gref(id: &str, installed: bool) -> GameRef { GameRef { appid: -1, name: "Toy Tanks".into(), platform: "toy".into(), platform_id: id.into(), slug: "toy-tanks".into(), installed, install_path: installed.then(|| "/games/block-builder".to_string()), ..Default::default() } }

#[test] fn discovery_finds_the_toy_and_skips_plugins_for_playdate_only() {
    let (ok, bad) = discover(&toy_dir().parent().unwrap().to_path_buf());
    assert_eq!(ok.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["toy"]); assert!(bad.is_empty());
}

#[test] fn sign_in_sync_import_and_resync() {
    if !have_python() { return; }
    let e = env("flow"); let s = source(&e, None);
    // not signed in: the plugin's own message comes through
    let mut got: Vec<GameRecord> = vec![];
    let err = s.sync(&cx(&e), &mut |b| got.extend(b), &Cancel::new()).unwrap_err(); assert_eq!(err, "Sign in to Toy Store first.");
    // sign in: wrong code, then the right one; the token lands in config.json under the plugin's id
    assert!(s.auth_complete("nope").unwrap_err().message.contains("not right"));
    assert_eq!(s.auth_begin().unwrap()["url"], "https://toy.example/login");
    sign_in(&s);
    assert_eq!(s.auth_status().unwrap()["connected"], true);
    let cfg: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(e.dir.join("config.json")).unwrap()).unwrap();
    assert_eq!(cfg["toy"]["token"], "tok-letmein"); assert_eq!(cfg["active_account"], "1");
    // sync: records stream in, Zest imports them
    let n = s.sync(&cx(&e), &mut |b| got.extend(b), &Cancel::new()).unwrap();
    assert_eq!((n, got.len(), s.rejected()), (3, 3, 0));
    assert_eq!(got[2].platform_id, "3", "a numeric id arrives as text");
    let mut c = Connection::open(&e.db).unwrap();
    let st = import(&mut c, "toy", &got, 1000).unwrap(); assert_eq!((st.added, st.updated), (3, 0));
    let row: (String, i64, String) = c.query_row("SELECT genres, playtime_forever, name FROM games WHERE platform_id='t1'", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
    assert_eq!(row, ("Action,Arcade".into(), 125, "Toy Tanks".into()));
    // the second sync: the plugin sees its games as already known
    let mut again = vec![]; s.sync(&cx(&e), &mut |b| again.extend(b), &Cancel::new()).unwrap();
    assert!(s.progress().unwrap().2.contains("already known"), "{:?}", s.progress());
    let st = import(&mut c, "toy", &again, 2000).unwrap(); assert_eq!((st.added, st.updated), (0, 3));
    // sign out
    s.auth_disconnect().unwrap(); assert_eq!(s.auth_status().unwrap()["connected"], false);
}

#[test] fn rescrape_description_launch_links_and_install_scan() {
    if !have_python() { return; }
    let e = env("game"); let s = source(&e, None);
    let u = s.rescrape(&cx(&e), &gref("t1", false)).unwrap().unwrap();
    let cols: Vec<&str> = u.iter().map(|(c, _)| *c).collect();
    assert!(cols.contains(&"developers") && cols.contains(&"review_percentage") && cols.contains(&"installed") && !cols.contains(&"evil_column"), "{cols:?}");
    assert_eq!(u.iter().find(|(c, _)| *c == "developers").unwrap().1, Sql::Text("Toybox,Tank Works".into()));
    assert!(s.rescrape(&cx(&e), &gref("zzz", false)).unwrap().is_none());
    assert_eq!(s.description(&cx(&e), &gref("t1", false)).as_deref(), Some("A toy game called Toy Tanks."));
    let l = s.launch(&gref("t2", true)).unwrap(); assert!(l.record); assert_eq!(l.action, Action::Spawn { program: "/games/block-builder/run".into(), args: vec!["--toy".into()], dir: None, env: vec![] });
    let l = s.launch(&gref("t1", false)).unwrap(); assert!(!l.record); assert_eq!(l.action, Action::OpenUrl("toystore://install/t1".into()));
    assert_eq!(s.store_url(&gref("t1", false)).as_deref(), Some("https://toy.example/game/toy-tanks"));
    assert_eq!(s.installed_paths(), ["/games"]); assert_eq!(s.installed_scan().unwrap(), [("t2".to_string(), Some("/games/block-builder".to_string()))]);
    let c = s.caps(); assert!(c.sync && c.rescrape && c.launch && c.store_page && !c.uninstall);
    assert!(s.uninstall(&gref("t1", false)).is_err(), "not a declared capability");
}

#[test] fn stray_output_unknown_methods_hangs_and_crashes_do_not_break_the_host() {
    if !have_python() { return; }
    let e = env("bad"); let s = source(&e, None);
    assert_eq!(s.call("debug.noise", serde_json::json!({}), Some(Duration::from_secs(10)), None).unwrap()["ok"], true, "stray lines are ignored");
    assert!(s.call("nope.nothing", serde_json::json!({}), Some(Duration::from_secs(10)), None).unwrap_err().message.contains("method not found"));
    // a hung plugin: the request fails after its timeout and the next one works
    let t = Instant::now();
    let err = s.call("debug.sleep", serde_json::json!({"seconds": 30}), Some(Duration::from_millis(700)), None).unwrap_err();
    assert_eq!(err.code, -32002); assert!(t.elapsed() < Duration::from_secs(5)); assert!(!s.running());
    assert_eq!(s.auth_status().unwrap()["connected"], false, "restarted by the next request");
    // a crashing plugin
    assert_eq!(s.call("debug.crash", serde_json::json!({}), Some(Duration::from_secs(10)), None).unwrap_err().code, -32001);
    assert_eq!(s.auth_status().unwrap()["connected"], false, "restarted again");
}

#[test] fn cancel_stops_a_slow_sync() {
    if !have_python() { return; }
    let e = env("cancel"); let s = source(&e, None);
    sign_in(&s); pd_filter::account::plugin_secret_set(&e.dir, "toy", "slow", Some("1")).unwrap();
    let cancel = Cancel::new(); let c2 = cancel.clone();
    std::thread::spawn(move || { std::thread::sleep(Duration::from_millis(600)); c2.cancel(); });
    let t = Instant::now(); let mut got = vec![];
    let n = s.sync(&cx(&e), &mut |b| got.extend(b), &cancel).unwrap();
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed()); assert!(n < 3 && got.len() < 3);
}

#[test] fn an_idle_plugin_is_stopped_and_started_again() {
    if !have_python() { return; }
    let e = env("idle"); let s = source(&e, Some(Duration::from_millis(500)));
    s.auth_status().unwrap(); assert!(s.running());
    std::thread::sleep(Duration::from_millis(1800)); assert!(!s.running(), "stopped after being idle");
    s.auth_status().unwrap(); assert!(s.running(), "started again on demand");
}
