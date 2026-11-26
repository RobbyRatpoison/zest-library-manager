//! The update-rollback handshake, Zest's half. When PlayDate's updater installs a new version it writes `.rollback/pending.json` and starts a watchdog that
//! restores the old version unless the new one proves it works by deleting that file. Proof = "the window showed its first page". The old process that
//! wrote the marker can still be alive for a moment, so it must not count itself: each process has its own random token and the marker names the writer's.
//! A port of `rollback.mark_healthy` / `housekeeping` (the watchdog and the snapshots stay Python's; Zest only has to answer).
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// A marker nothing resolved for this long (installer cancelled, watchdog killed) is dropped at start.
pub const PENDING_MAX_AGE_SECS: f64 = 24.0 * 3600.0;
const SNAPSHOTS: [&str; 5] = ["code", "data", "old.flatpak", "repo", "repo.ref"];

/// This process's identity for the handshake (a pid cannot do it: every Flatpak sandbox has its own pid namespace).
pub fn token() -> &'static str {
    static T: OnceLock<String> = OnceLock::new();
    T.get_or_init(|| {
        let ns = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(1);
        let a = (ns as u64) ^ ((std::process::id() as u64) << 32).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let b = a.rotate_left(29).wrapping_mul(0xBF58_476D_1CE4_E5B9) ^ (ns >> 64) as u64;
        format!("{a:016x}{b:016x}")
    })
}

fn dir(root: &Path) -> PathBuf { root.join(".rollback") }
fn read_pending(root: &Path) -> Option<Value> { serde_json::from_str(&std::fs::read_to_string(dir(root).join("pending.json")).ok()?).ok() }
fn remove_all(p: &Path) { if p.is_dir() { let _ = std::fs::remove_dir_all(p); } else { let _ = std::fs::remove_file(p); } }
fn discard_snapshot(root: &Path) { for n in SNAPSHOTS { remove_all(&dir(root).join(n)); } }

/// Call once the first page has been drawn. Returns true when a marker written by another process was cleared (the watchdog then stops supervising).
pub fn mark_healthy(root: &Path) -> bool { mark_healthy_as(root, token()) }

fn mark_healthy_as(root: &Path, me: &str) -> bool {
    let Some(p) = read_pending(root) else { return false };
    if p.get("old_token").and_then(|v| v.as_str()) == Some(me) { return false; }
    remove_all(&dir(root).join("pending.json"));
    discard_snapshot(root);
    true
}

/// At start: drop a marker that is too old to be an update still in flight.
pub fn housekeeping(root: &Path) {
    let Some(p) = read_pending(root) else { return };
    let started = p.get("started").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
    if now - started > PENDING_MAX_AGE_SECS { remove_all(&dir(root).join("pending.json")); discard_snapshot(root); }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup(n: &str, pending: Value) -> PathBuf {
        let d = std::env::temp_dir().join(format!("zest_rb_{n}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join(".rollback/code")).unwrap();
        std::fs::write(d.join(".rollback/code/f"), "x").unwrap();
        std::fs::write(d.join(".rollback/old.flatpak"), "x").unwrap();
        std::fs::write(d.join(".rollback/pending.json"), pending.to_string()).unwrap();
        d
    }
    fn now() -> f64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64() }

    #[test] fn a_marker_from_another_process_is_cleared_with_its_snapshot() {
        let d = setup("other", serde_json::json!({"old_token": "someone-else", "started": now()}));
        assert!(mark_healthy(&d));
        assert!(!d.join(".rollback/pending.json").exists() && !d.join(".rollback/code").exists() && !d.join(".rollback/old.flatpak").exists());
        assert!(!mark_healthy(&d), "nothing left to clear");
    }
    #[test] fn the_process_that_wrote_the_marker_does_not_count_itself() {
        let d = setup("self", serde_json::json!({"old_token": token(), "started": now()}));
        assert!(!mark_healthy(&d));
        assert!(d.join(".rollback/pending.json").exists() && d.join(".rollback/code/f").exists());
    }
    #[test] fn a_stale_marker_is_dropped_at_start_a_fresh_one_kept() {
        let d = setup("stale", serde_json::json!({"old_token": "x", "started": now() - 2.0 * 24.0 * 3600.0}));
        housekeeping(&d);
        assert!(!d.join(".rollback/pending.json").exists() && !d.join(".rollback/code").exists());
        let d = setup("fresh", serde_json::json!({"old_token": "x", "started": now() - 60.0}));
        housekeeping(&d);
        assert!(d.join(".rollback/pending.json").exists());
        let d = std::env::temp_dir().join(format!("zest_rb_none_{}", std::process::id()));
        assert!(!mark_healthy(&d));   // no .rollback folder at all: nothing happens
    }
}
