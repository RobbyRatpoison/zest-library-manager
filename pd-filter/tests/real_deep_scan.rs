//! Read-only live check of the three stores' `scan_junk` against a COPY of the data folder (ZEST_SCAN_DIR): run with
//! `ZEST_SCAN_DIR=/path/to/copy cargo test -p pd-filter --test real_deep_scan -- --ignored --nocapture`. Prints names and reasons only, never tokens.
use pd_filter::sources::Registry;
use std::path::PathBuf;

#[test]
#[ignore]
fn deep_scan_against_the_real_stores() {
    let Some(dir) = std::env::var_os("ZEST_SCAN_DIR").map(PathBuf::from) else { eprintln!("ZEST_SCAN_DIR not set"); return };
    let db = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).find(|p| p.file_name().and_then(|n| n.to_str()).map_or(false, |n| n.starts_with("games_") && n.ends_with(".db"))).expect("no games_*.db in the folder");
    let reg = Registry::builtin_with(&dir, &db);
    for id in ["epic_games", "humble", "playstation"] {
        let Some(src) = reg.get(id) else { eprintln!("{id}: not registered"); continue };
        let t = std::time::Instant::now();
        match src.scan_junk() {
            Ok(hits) => { eprintln!("{id}: {} hits in {:.1}s", hits.len(), t.elapsed().as_secs_f32()); for h in hits.iter().take(15) { eprintln!("   {} | {}", h.name, h.reason); } }
            Err(e) => eprintln!("{id}: error: {e}"),
        }
    }
}
