//! Syncing a store's library once when Zest starts (PlayDate's `startup_sync_plugins`, commits 8c7dd065 and 60535698). Opt-in per store, off by default; the stores
//! enabled sync one after another, never two at once, and a store that is not signed in is skipped quietly. The outcome of each sync is recorded by `status`, which is
//! what the Plugins dialog already shows as the last-sync line (failures in amber), so no separate badge state is kept.
use serde_json::Value;

pub const KEY: &str = "startup_sync_plugins";
/// Seconds after the window opens before the first startup sync starts (nothing the first page waits for).
pub const DELAY_SECS: u64 = 10;

fn valid(name: &str) -> bool { let mut c = name.chars(); c.next().map_or(false, |f| f.is_ascii_lowercase()) && c.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') }

/// The stores that sync at start, from `state.json`: sorted, without repeats or names that are not store ids.
pub fn enabled(state: &Value) -> Vec<String> {
    let mut v: Vec<String> = state.get(KEY).and_then(|a| a.as_array()).map(|a| a.iter().filter_map(|x| x.as_str()).filter(|n| valid(n)).map(String::from).collect()).unwrap_or_default();
    v.sort(); v.dedup(); v
}

/// The list to save after turning `platform` on or off. A name that is not a store id changes nothing.
pub fn toggled(state: &Value, platform: &str, on: bool) -> Value {
    let mut v = enabled(state);
    if valid(platform) { if on { v.push(platform.to_string()); v.sort(); v.dedup(); } else { v.retain(|p| p != platform); } }
    Value::from(v)
}

/// The queue to run: the enabled stores that exist, can sync and are signed in, in the saved (alphabetical) order. `usable` answers for one store id.
pub fn queue(state: &Value, usable: &dyn Fn(&str) -> bool) -> Vec<String> { enabled(state).into_iter().filter(|p| usable(p)).collect() }

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test] fn the_setting_is_cleaned_toggled_and_only_store_ids_are_kept() {
        let st = json!({"startup_sync_plugins": ["itch_io", "gog", "gog", "../x", "", "Bad", 5, "epic_games"]});
        assert_eq!(enabled(&st), ["epic_games", "gog", "itch_io"]);
        assert!(enabled(&json!({})).is_empty() && enabled(&json!({"startup_sync_plugins": "gog"})).is_empty());
        assert_eq!(toggled(&st, "humble", true), json!(["epic_games", "gog", "humble", "itch_io"]));
        assert_eq!(toggled(&st, "gog", false), json!(["epic_games", "itch_io"]));
        assert_eq!(toggled(&st, "gog", true), json!(["epic_games", "gog", "itch_io"]), "no repeats");
        assert_eq!(toggled(&st, "a/b", true), json!(["epic_games", "gog", "itch_io"]), "not an id");
    }
    #[test] fn the_queue_skips_stores_that_cannot_sync_or_are_not_signed_in_and_keeps_order() {
        let st = json!({"startup_sync_plugins": ["itch_io", "gog", "humble"]});
        assert_eq!(queue(&st, &|p| p != "gog"), ["humble", "itch_io"]);
        assert!(queue(&json!({}), &|_| true).is_empty());
    }
}
