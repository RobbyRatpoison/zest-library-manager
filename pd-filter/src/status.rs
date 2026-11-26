//! What the last library sync of each source did, kept in `zest_sync_status.json` next to the data (a Zest-only file) so the Plugins dialog can answer
//! "did it work?" at any time, also after a restart.
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SyncStatus {
    /// Unix time the sync ended.
    pub at: i64,
    /// Games the source listed.
    pub found: usize, pub added: usize, pub updated: usize,
    /// Games skipped because the user removed them (the blacklist).
    pub blocked: usize,
    pub error: Option<String>,
}

const FILE: &str = "zest_sync_status.json";

pub fn load(dir: &Path) -> HashMap<String, SyncStatus> {
    let Some(Value::Object(o)) = std::fs::read_to_string(dir.join(FILE)).ok().and_then(|t| serde_json::from_str(&t).ok()) else { return HashMap::new() };
    o.iter().filter_map(|(k, v)| {
        let n = |f: &str| v.get(f).and_then(|x| x.as_u64()).unwrap_or(0) as usize;
        Some((k.clone(), SyncStatus { at: v.get("at")?.as_i64()?, found: n("found"), added: n("added"), updated: n("updated"), blocked: n("blocked"), error: v.get("error").and_then(|e| e.as_str()).map(String::from) }))
    }).collect()
}

pub fn record(dir: &Path, platform: &str, s: &SyncStatus) -> std::io::Result<()> {
    let mut all: serde_json::Map<String, Value> = std::fs::read_to_string(dir.join(FILE)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    all.insert(platform.to_string(), json!({"at": s.at, "found": s.found, "added": s.added, "updated": s.updated, "blocked": s.blocked, "error": s.error}));
    crate::account::write_json(&dir.join(FILE), &Value::Object(all))
}

/// The line the Plugins dialog shows: what the sync found and did, or why it failed.
/// The short form for a collapsed card: when it last synced (or that it failed), without the game counts.
pub fn describe_short(s: &SyncStatus, now: i64) -> String {
    let full = describe(s, now);
    // "Last sync <when>: N games listed, ..." or "Last sync <when> failed: ..." cut after the time
    let head = full.split(": ").next().unwrap_or(&full);
    head.to_string()
}

pub fn describe(s: &SyncStatus, now: i64) -> String {
    let ago = { let d = (now - s.at).max(0); if d < 90 { "just now".to_string() } else if d < 5400 { format!("{} min ago", d / 60) } else if d < 129_600 { format!("{} h ago", d / 3600) } else { format!("{} days ago", d / 86400) } };
    match &s.error {
        Some(e) => format!("Last sync {ago} failed: {e}"),
        None => format!("Last sync {ago}: {} games listed, {} new, {} refreshed{}", s.found, s.added, s.updated, if s.blocked > 0 { format!(", {} skipped (removed by you)", s.blocked) } else { String::new() }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn roundtrip_and_wording() {
        let d = std::env::temp_dir().join(format!("zest_status_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        assert!(load(&d).is_empty());
        record(&d, "nintendo", &SyncStatus { at: 1000, found: 42, added: 0, updated: 42, blocked: 1, error: None }).unwrap();
        record(&d, "toy", &SyncStatus { at: 900, error: Some("Sign in first".into()), ..Default::default() }).unwrap();
        let m = load(&d);
        assert_eq!(m["nintendo"].found, 42); assert_eq!(m["toy"].error.as_deref(), Some("Sign in first"));
        assert_eq!(describe(&m["nintendo"], 1030), "Last sync just now: 42 games listed, 0 new, 42 refreshed, 1 skipped (removed by you)");
        assert_eq!(describe(&m["toy"], 900 + 7200), "Last sync 2 h ago failed: Sign in first");
    }
    #[test] fn the_short_form_keeps_the_time_and_drops_the_counts() {
        let ok = SyncStatus { at: 1000, found: 42, added: 1, updated: 41, blocked: 0, error: None };
        let short = describe_short(&ok, 1000 + 3300); assert_eq!(short, "Last sync 55 min ago"); assert!(!short.contains("games"));
        let bad = SyncStatus { error: Some("no network".into()), ..ok.clone() };
        assert_eq!(describe_short(&bad, 1000 + 3300), "Last sync 55 min ago failed");
    }
}
