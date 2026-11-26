//! Per-tag taste affinity (port of `database.compute_tag_affinity`, and the lists `/api/tag-affinity` builds from it): `liked_rate - library_rate` for every tag a Steam game
//! carries, with the user's pins applied. Shared by the cached `tag_similarity` column and the Tuning panel's liked / disliked lists, so the two cannot disagree.
//! Checked against the real Python by `tests/gen_tagaffinity_golden.py`.
use rusqlite::Connection;
use serde_json::Value;
use std::collections::HashMap;

fn split(s: &str) -> Vec<String> { s.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect() }

/// `compute_tag_affinity`. The liked pool is the Beaten/Completed Steam games (the 50 most played when there are none), weighted by playtime (`log1p(hours)/log1p(cap)`, flat when no liked game
/// has playtime); tags in less than `min_rate_pct` percent of the Steam library are left out (too small a sample), and `overrides` (`{tag: like|dislike|ignore}`) are applied last.
pub fn compute(conn: &Connection, min_rate_pct: f64, cap_hours: f64, overrides: Option<&Value>) -> rusqlite::Result<HashMap<String, f64>> {
    let rows = |sql: &str| -> rusqlite::Result<Vec<(Vec<String>, f64)>> {
        let mut st = conn.prepare(sql)?;
        let it = st.query_map([], |r| Ok((r.get::<_, Option<String>>(0)?.unwrap_or_default(), r.get::<_, Option<f64>>(1).unwrap_or(None).unwrap_or(0.0))))?;
        Ok(it.flatten().map(|(t, p)| (split(&t), p)).collect())
    };
    // Steam only: other stores' `tags` are not always a real tag vocabulary
    let mut liked = rows("SELECT tags, playtime_forever FROM games WHERE completion_status IN ('Beaten', 'Completed') AND platform = 'steam' AND tags IS NOT NULL AND tags != ''")?;
    if liked.is_empty() { liked = rows("SELECT tags, playtime_forever FROM games WHERE platform = 'steam' AND tags IS NOT NULL AND tags != '' ORDER BY playtime_forever DESC LIMIT 50")?; }
    let library = rows("SELECT tags, 0 FROM games WHERE platform = 'steam' AND tags IS NOT NULL AND tags != ''")?;
    let weight = |minutes: f64| -> f64 { let h = minutes / 60.0; if h <= 0.0 { 0.0 } else { (h.ln_1p() / cap_hours.ln_1p()).min(1.0) } };
    let weights: Vec<f64> = liked.iter().map(|(_, p)| weight(*p)).collect();
    let weighted = weights.iter().any(|w| *w > 0.0);
    let pool = |rows: &[(Vec<String>, f64)], w: Option<&[f64]>| -> HashMap<String, f64> {
        let n = match w { None => rows.len() as f64, Some(w) => w.iter().sum() };
        let mut counts: HashMap<String, f64> = HashMap::new();
        if n == 0.0 { return counts; }
        for (i, (tags, _)) in rows.iter().enumerate() {
            let wi = w.map_or(1.0, |w| w[i]);
            if wi <= 0.0 { continue; }
            for t in tags { *counts.entry(t.clone()).or_insert(0.0) += wi; }
        }
        counts.into_iter().map(|(t, c)| (t, c / n)).collect()
    };
    let liked_rate = pool(&liked, if weighted { Some(&weights) } else { None });
    let library_rate = pool(&library, None);
    let floor = min_rate_pct / 100.0;
    let affinity: HashMap<String, f64> = library_rate.iter().filter(|(_, r)| **r >= floor).map(|(t, r)| (t.clone(), liked_rate.get(t).copied().unwrap_or(0.0) - r)).collect();
    Ok(crate::pick::apply_tag_overrides(affinity, overrides))
}

#[derive(Clone, Debug, PartialEq)]
pub struct Ranked { pub tag: String, pub affinity: f64, pub pinned: bool }
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Lists { pub liked: Vec<Ranked>, pub disliked: Vec<Ranked>, pub ignored: Vec<String> }

/// The route's lists: the `limit` strongest liked tags (affinity > 0, best first), the `limit` strongest disliked ones (affinity < 0, most negative first), and the tags the user set to ignore.
pub fn lists(aff: &HashMap<String, f64>, overrides: Option<&Value>, limit: usize) -> Lists {
    let ov = overrides.and_then(|v| v.as_object());
    let mode = |t: &str| ov.and_then(|o| o.get(t)).and_then(|m| m.as_str());
    let mut ranked: Vec<(&String, f64)> = aff.iter().map(|(t, a)| (t, *a)).collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.0.cmp(b.0)));
    let entry = |t: &String, a: f64| Ranked { tag: t.clone(), affinity: a, pinned: matches!(mode(t), Some("like") | Some("dislike")) };
    let liked = ranked.iter().filter(|(_, a)| *a > 0.0).take(limit).map(|(t, a)| entry(t, *a)).collect();
    let disliked = ranked.iter().rev().filter(|(_, a)| *a < 0.0).take(limit).map(|(t, a)| entry(t, *a)).collect();
    let mut ignored: Vec<String> = ov.map(|o| o.iter().filter(|(_, m)| m.as_str() == Some("ignore")).map(|(t, _)| t.clone()).collect()).unwrap_or_default();
    ignored.sort();
    Lists { liked, disliked, ignored }
}

/// `config.normalize_tag_overrides`: only `like` / `dislike` / `ignore`, tags trimmed, no comma (tags are comma separated in the library), at most 100 characters, at most 500 pins.
pub fn normalize_overrides(raw: &Value) -> Value {
    let mut out = serde_json::Map::new();
    if let Some(o) = raw.as_object() {
        for (tag, mode) in o {
            let Some(m) = mode.as_str().filter(|m| matches!(*m, "like" | "dislike" | "ignore")) else { continue };
            let t = tag.trim();
            if !t.is_empty() && !t.contains(',') && t.chars().count() <= 100 { out.insert(t.to_string(), Value::from(m)); }
            if out.len() >= 500 { break; }
        }
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn golden() -> Value { serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/tagaffinity_golden.json")).unwrap()).unwrap() }
    /// The same fake library the generator builds.
    fn library(kind: &str) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute("CREATE TABLE games (appid INTEGER, platform TEXT, tags TEXT, completion_status TEXT, playtime_forever INTEGER)", []).unwrap();
        let tagsets: [Option<&str>; 16] = [Some("Action,RPG,Singleplayer"), Some("Action,Shooter,Multiplayer"), Some("RPG,Story Rich,Singleplayer"), Some("Puzzle,Casual,Singleplayer"), Some("Strategy,Turn-Based,Singleplayer"), Some("Action,Roguelike,Singleplayer"),
            Some("Horror,Singleplayer"), Some("Racing,Multiplayer"), Some("Puzzle,Singleplayer,Relaxing"), Some("RPG,Story Rich"), Some("Rare Tag,Singleplayer"), Some("Action,RPG"), Some(""), None, Some("Casual,Relaxing,Puzzle"), Some("Strategy,Singleplayer")];
        let status = ["Beaten", "Completed", "Unfinished", "Never Played", "Won't Play"];
        let n = if kind == "tiny" { 6 } else { 80 };
        for i in 0..n {
            let st = if kind == "none_liked" { "Never Played" } else { status[(i * 7) % 5] };
            let base: [Option<i64>; 6] = [Some(0), Some(30), Some(600), Some(5000), Some(12000), None];
            let play = if kind == "no_playtime" { Some(0) } else { base[i % 6].map(|b| b + i as i64) };
            let plat = if i % 9 != 0 { "steam" } else { "epic_games" };
            c.execute("INSERT INTO games VALUES (?1, ?2, ?3, ?4, ?5)", rusqlite::params![i as i64 + 1, plat, tagsets[i % 16], st, play]).unwrap();
        }
        c
    }
    fn ranked(v: &Value) -> Vec<(String, f64, bool)> { v.as_array().unwrap().iter().map(|e| (e["tag"].as_str().unwrap().to_string(), e["affinity"].as_f64().unwrap(), e["pinned"].as_bool().unwrap())).collect() }
    fn got(r: &[Ranked]) -> Vec<(String, f64, bool)> { r.iter().map(|e| (e.tag.clone(), e.affinity, e.pinned)).collect() }
    fn close(a: &[(String, f64, bool)], b: &[(String, f64, bool)], what: &str) {
        assert_eq!(a.len(), b.len(), "{what}: length");
        for (x, y) in a.iter().zip(b) { assert!(x.0 == y.0 && x.2 == y.2 && (x.1 - y.1).abs() < 1e-12, "{what}: {x:?} vs {y:?}"); }
    }
    #[test] fn affinities_and_lists_agree_with_the_real_python() {
        let g = golden();
        for case in g["cases"].as_array().unwrap() {
            let (kind, min, cap) = (case["kind"].as_str().unwrap(), case["min_rate"].as_f64().unwrap(), case["cap"].as_f64().unwrap());
            let ov = case["overrides"].clone();
            let ovr = ov.as_object().filter(|o| !o.is_empty()).map(|_| &ov);
            let aff = compute(&library(kind), min, cap, ovr).unwrap();
            let want = case["affinity"].as_object().unwrap();
            let label = format!("{kind} {min} {cap} {ov}");
            assert_eq!(aff.len(), want.len(), "{label}: tag count");
            for (t, a) in want { assert!((aff[t] - a.as_f64().unwrap()).abs() < 1e-12, "{label}: {t}"); }
            for (key, limit) in [("lists", 10usize), ("lists3", 3)] {
                let l = lists(&aff, Some(&ov), limit);
                close(&got(&l.liked), &ranked(&case[key]["liked"]), &format!("{label} {key} liked"));
                close(&got(&l.disliked), &ranked(&case[key]["disliked"]), &format!("{label} {key} disliked"));
                assert_eq!(l.ignored, case[key]["ignored"].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect::<Vec<_>>(), "{label} ignored");
            }
        }
    }
    #[test] fn pins_are_cleaned_like_python_cleans_them() {
        let raw = json!({" Action ": "like", "Bad,Tag": "like", "": "dislike", "Puzzle": "maybe", "Horror": "dislike", "X": 5, "Long": "ignore"});
        assert_eq!(normalize_overrides(&raw), json!({"Action": "like", "Horror": "dislike", "Long": "ignore"}));
        assert_eq!(normalize_overrides(&json!("nope")), json!({}));
        assert_eq!(normalize_overrides(&json!({"a".repeat(101): "like"})), json!({}));
    }
}
