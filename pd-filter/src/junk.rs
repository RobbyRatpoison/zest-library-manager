//! The Library Junk Finder (port of `library.py`'s `/api/steam-junk-scan`): entries that are probably not real games. Two lists, both only suggestions for the user to review:
//! Steam games that are no longer on the account (checked against the Web API), and games of any store whose title looks like a beta, demo, soundtrack, dev kit and so on.
//! Checked against the real Python by `tests/gen_junk_golden.py`.
use regex::Regex;
use serde_json::Value;
use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;

/// `_JUNK_PATTERNS`, in Python's order (a title can match several; all reasons are listed).
const PATTERNS: &[(&str, &str)] = &[
    (r"(?i)\bbeta\b", "beta"), (r"(?i)\balpha\b", "alpha"), (r"(?i)\b(public|closed|open)\s+test(ing)?\b", "public/closed test"), (r"(?i)\bunstable\b", "unstable/test branch"),
    (r"(?i)\bplayable\s+teaser\b", "playable teaser"), (r"(?i)\bfriend'?s\s+pass\b", "friend's pass"), (r"(?i)\bkey\s+only\b", "key-only bundle placeholder"),
    (r"(?i)-\s*multiplayer\s*$", "multiplayer-only companion app"), (r"(?i)\bthe\s+movie\b", "documentary/movie"), (r"(?i)\bdemo\b", "demo"), (r"(?i)\btrial\b", "trial"),
    (r"(?i)\bprototype\b", "prototype"), (r"(?i)\bsneak\s?peek\b", "sneak peek"), (r"(?i)\bsource\s?code\b", "source code"), (r"(?i)\b(dev|mod)\s?kit\b", "dev/mod kit"),
    (r"\bSDK\b", "SDK"), (r"(?i)\bsoundtrack\b", "soundtrack"), (r"(?i)\b(art\s?book|artbook)\b", "artbook"), (r"\bOST\b", "soundtrack (OST)"),
];

/// `_junk_match_reasons`.
pub fn reasons(name: &str) -> Vec<&'static str> {
    static R: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    R.get_or_init(|| PATTERNS.iter().map(|(p, l)| (Regex::new(p).unwrap(), *l)).collect()).iter().filter(|(re, _)| re.is_match(name)).map(|(_, l)| *l).collect()
}

/// A library row as the scan needs it.
#[derive(Clone, Debug, PartialEq)]
pub struct Row { pub appid: i64, pub name: String, pub platform: String, pub is_free: bool, pub playtime: i64 }

#[derive(Clone, Debug, PartialEq)]
pub struct Found { pub appid: i64, pub name: String, pub platform: String, pub reasons: Vec<&'static str> }

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Selection { pub owned: Vec<(i64, String)>, pub patterns: Vec<Found> }

/// The scan's choice. `owned` = the account's appids when they could be fetched (None: no key, or the request failed). A game already flagged as not owned is not also listed by title.
pub fn select(rows: &[Row], blacklisted: &HashSet<String>, dismissed: &HashSet<i64>, owned: Option<&HashSet<i64>>) -> Selection {
    let mut not_owned: HashSet<i64> = HashSet::new();
    if let Some(owned) = owned {
        for r in rows.iter().filter(|r| r.platform == "steam") {
            if blacklisted.contains(&r.appid.to_string()) || dismissed.contains(&r.appid) || owned.contains(&r.appid) { continue; }
            // the bulk call hides lightly played free games even when owned: only trust absence once there is some playtime
            if r.is_free && r.playtime <= 5 { continue; }
            not_owned.insert(r.appid);
        }
    }
    let mut sel = Selection::default();
    sel.owned = rows.iter().filter(|r| r.platform == "steam" && not_owned.contains(&r.appid)).map(|r| (r.appid, r.name.clone())).collect();
    for r in rows {
        if blacklisted.contains(&r.appid.to_string()) || dismissed.contains(&r.appid) || not_owned.contains(&r.appid) { continue; }
        let why = reasons(&r.name);
        if !why.is_empty() { sel.patterns.push(Found { appid: r.appid, name: r.name.clone(), platform: r.platform.clone(), reasons: why }); }
    }
    sel
}

/// The library (by name, case-insensitively) and the blacklist's platform ids, read-only.
pub fn load(db: &Path) -> Result<(Vec<Row>, HashSet<String>), String> {
    let c = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| e.to_string())?;
    let black: HashSet<String> = c.prepare("SELECT platform_id FROM blacklist WHERE platform_id IS NOT NULL").map_err(|e| e.to_string())?
        .query_map([], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())?.flatten().collect();
    let mut st = c.prepare("SELECT appid, COALESCE(name,''), platform, COALESCE(is_free,0), COALESCE(playtime_forever,0) FROM games ORDER BY name COLLATE NOCASE").map_err(|e| e.to_string())?;
    let rows = st.query_map([], |r| Ok(Row { appid: r.get(0)?, name: r.get(1)?, platform: r.get::<_, Option<String>>(2)?.filter(|p| !p.is_empty()).unwrap_or_else(|| "steam".into()), is_free: r.get::<_, i64>(3)? != 0, playtime: r.get(4)? }))
        .map_err(|e| e.to_string())?.flatten().collect();
    Ok((rows, black))
}

/// `state.json` `steam_junk_whitelist`: games the user dismissed from the lists.
pub fn dismissed(state: &Value) -> HashSet<i64> { state["steam_junk_whitelist"].as_array().map(|a| a.iter().filter_map(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok())).collect()).unwrap_or_default() }

/// The whitelist with `add` merged in, sorted, as it is saved.
pub fn merged_whitelist(state: &Value, add: &[i64]) -> Value {
    let mut all = dismissed(state); all.extend(add.iter().copied());
    let mut v: Vec<i64> = all.into_iter().collect(); v.sort();
    Value::from(v)
}

/// The account's appids from Steam, or why there are none. `Ok(None)` = no API key, so the check is skipped (not an error).
pub fn owned_appids(steam: &crate::steamclient::Steam) -> Result<Option<HashSet<i64>>, String> {
    if !steam.has_key() { return Ok(None); }
    steam.owned_games().map(|g| Some(g.into_iter().map(|o| o.appid).collect()))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn golden() -> Value { serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/junk_golden.json")).unwrap()).unwrap() }
    fn set_i(v: &Value) -> HashSet<i64> { v.as_array().unwrap().iter().map(|x| x.as_i64().unwrap()).collect() }
    fn rows(g: &Value) -> Vec<Row> {
        g["rows"].as_array().unwrap().iter().map(|r| Row { appid: r[0].as_i64().unwrap(), name: r[1].as_str().unwrap().into(), platform: r[2].as_str().unwrap_or("steam").into(), is_free: r[3].as_i64().unwrap() != 0, playtime: r[4].as_i64().unwrap() }).collect()
    }
    fn render(s: &Selection) -> Value {
        serde_json::json!({"owned": s.owned.iter().map(|(a, n)| serde_json::json!({"appid": a, "name": n})).collect::<Vec<_>>(),
            "patterns": s.patterns.iter().map(|f| serde_json::json!({"appid": f.appid, "name": f.name, "platform": f.platform, "reasons": f.reasons})).collect::<Vec<_>>()})
    }
    #[test] fn title_patterns_agree_with_the_real_python() {
        let g = golden();
        for p in g["names"].as_array().unwrap() {
            let (name, want) = (p[0].as_str().unwrap(), p[1].as_array().unwrap().iter().map(|x| x.as_str().unwrap()).collect::<Vec<_>>());
            assert_eq!(reasons(name), want, "{name:?}");
        }
    }
    #[test] fn the_selection_rules_agree_with_the_real_python() {
        let g = golden(); let r = rows(&g);
        let black: HashSet<String> = g["blacklisted"].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect();
        let owned = set_i(&g["owned"]); let dis = set_i(&g["dismissed"]);
        assert_eq!(render(&select(&r, &black, &dis, Some(&owned))), g["with_ownership"]);
        assert_eq!(render(&select(&r, &black, &dis, None)), g["without_ownership"]);
    }
    #[test] fn the_library_is_read_by_name_and_the_whitelist_merges() {
        let d = std::env::temp_dir().join(format!("zest_junk_{}.db", std::process::id())); let _ = std::fs::remove_file(&d);
        let c = rusqlite::Connection::open(&d).unwrap();
        c.execute_batch("CREATE TABLE games (appid INTEGER, name TEXT, platform TEXT, is_free INTEGER, playtime_forever INTEGER); CREATE TABLE blacklist (appid INTEGER, platform_id TEXT);
            INSERT INTO games VALUES (2,'banana','gog',NULL,NULL),(1,'Apple',NULL,1,9),(3,NULL,'steam',0,0); INSERT INTO blacklist VALUES (5,'77'),(6,NULL);").unwrap();
        drop(c);
        let (rows, black) = load(&d).unwrap();
        assert_eq!(rows.iter().map(|r| (r.appid, r.name.as_str(), r.platform.as_str(), r.is_free, r.playtime)).collect::<Vec<_>>(), vec![(3, "", "steam", false, 0), (1, "Apple", "steam", true, 9), (2, "banana", "gog", false, 0)]);
        assert_eq!(black, ["77".to_string()].into_iter().collect());
        let _ = std::fs::remove_file(&d);
        let st = serde_json::json!({"steam_junk_whitelist": [5, "3", 9]});
        assert_eq!(merged_whitelist(&st, &[4, 5]), serde_json::json!([3, 4, 5, 9]));
        assert_eq!(merged_whitelist(&serde_json::json!({}), &[2, 1]), serde_json::json!([1, 2]));
    }
}
