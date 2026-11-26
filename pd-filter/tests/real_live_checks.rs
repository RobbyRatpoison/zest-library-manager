//! Live, read-only checks of the Xbox owned-games import and the Amazon library sync, on a COPY of the data folder (ZEST_SCAN_DIR; the games database in it
//! is the only thing written). Run: `ZEST_SCAN_DIR=/path/to/copy cargo test -p pd-filter --test real_live_checks -- --ignored --nocapture --test-threads=1`.
//! Prints counts and a few game names only, never tokens.
use pd_filter::sources::{Registry, SourceCtx};
use pd_filter::net::{Cancel, Http, UreqHttp};
use std::path::PathBuf;
use std::sync::Arc;

fn dir() -> Option<(PathBuf, PathBuf)> {
    let dir = PathBuf::from(std::env::var_os("ZEST_SCAN_DIR")?);
    let db = std::fs::read_dir(&dir).ok()?.flatten().map(|e| e.path()).find(|p| p.file_name().and_then(|n| n.to_str()).map_or(false, |n| n.starts_with("games_") && n.ends_with(".db")))?;
    Some((dir, db))
}

#[test]
#[ignore]
fn amazon_library_sync() {
    let Some((root, db)) = dir() else { eprintln!("ZEST_SCAN_DIR not set"); return };
    let reg = Registry::builtin_with(&root, &db);
    let src = reg.get("amazon_games").expect("amazon registered");
    let http: Arc<dyn Http> = Arc::new(UreqHttp::new());
    let cx = SourceCtx { http: http.clone(), steam: Arc::new(pd_filter::steamclient::Steam::new(http, None, 10.0)), root: root.clone(), db: db.clone(), today: "2026-11-07".into(), appinfo: Arc::new(|_| Default::default()), account_id: None };
    let mut got = Vec::new();
    let r = src.sync(&cx, &mut |b| got.extend(b), &Cancel::new());
    eprintln!("amazon sync: {:?}, {} records", r, got.len());
    for g in got.iter().take(10) { eprintln!("   {}", g.name); }
}

#[test]
#[ignore]
fn xbox_owned_import() {
    let Some((root, db)) = dir() else { eprintln!("ZEST_SCAN_DIR not set"); return };
    let http = UreqHttp::new();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    let before: i64 = rusqlite::Connection::open(&db).unwrap().query_row("SELECT COUNT(*) FROM games WHERE platform='xbox'", [], |r| r.get(0)).unwrap_or(0);
    let r = pd_filter::xbox_owned::import(&db, &home, &http, now);
    let after: i64 = rusqlite::Connection::open(&db).unwrap().query_row("SELECT COUNT(*) FROM games WHERE platform='xbox'", [], |r| r.get(0)).unwrap_or(0);
    eprintln!("xbox import ({}): {:?}; xbox rows {} -> {}", root.display(), r.map(|(m, _, n)| (m, n)), before, after);
}
