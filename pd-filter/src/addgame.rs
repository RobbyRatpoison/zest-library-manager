//! Add Game (PlayDate's `/api/steam-search` and `/api/games/add`): a Steam game found by name and added as a normal Steam row, or a game with no store behind it added by name
//! under the Manual (`custom`) platform with a negative id. The row is only a placeholder: the caller then runs the usual re-scrape, art and HLTB jobs on it.
use crate::net::{enc, Http};
use rusqlite::{params, Connection};
use serde_json::Value;
use std::collections::HashSet;
use std::time::Duration;

/// One Steam store search result.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit { pub appid: i64, pub name: String, pub in_library: bool }

/// The store search answer as hits: only real apps (not soundtracks' bundles or packages), at most 12, each flagged when the library already has it.
pub fn parse_search(body: &str, existing: &HashSet<i64>) -> Vec<Hit> {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    v["items"].as_array().map(|a| a.as_slice()).unwrap_or(&[]).iter().filter(|i| i["type"].as_str() == Some("app")).filter_map(|i| {
        let id = i["id"].as_i64().filter(|id| *id != 0)?;
        Some(Hit { appid: id, name: i["name"].as_str().unwrap_or("").to_string(), in_library: existing.contains(&id) })
    }).take(12).collect()
}

/// The Steam appid a search box entry names: a store/community link (`.../app/461040/...`), a `steam://store/461040` or `steam://nav/games/details/461040` link, or just the number.
/// The second value says whether it was only a number, which could equally be a game's name ("1942", "2048"), so the name search runs too.
pub fn parse_appid(term: &str) -> Option<(i64, bool)> {
    let t = term.trim();
    let id = |s: &str| -> Option<i64> { let d: String = s.chars().take_while(|c| c.is_ascii_digit()).collect(); (!d.is_empty() && d.len() <= 10).then(|| d.parse().ok()).flatten().filter(|n| *n > 0) };
    if t.chars().all(|c| c.is_ascii_digit()) { return id(t).map(|n| (n, true)); }
    let low = t.to_ascii_lowercase();
    if !(low.contains("steampowered.com") || low.contains("steamcommunity.com") || low.starts_with("steam://")) { return None; }
    for key in ["/app/", "steam://store/", "/games/details/", "steam://run/", "steam://install/"] {
        if let Some(p) = low.find(key) { if let Some(n) = id(&t[p + key.len()..]) { return Some((n, false)); } }
    }
    None
}

/// An appid looked up by id: the store's own name for it, else the placeholder name when the store does not know it or cannot be reached (the re-scrape fills it in later).
/// The store answers `success: false` for a delisted or region-locked app that the library may still own, so that is no reason to refuse it.
pub fn lookup_appid(http: &dyn Http, appid: i64, existing: &HashSet<i64>) -> Hit {
    let name = http.get(&format!("https://store.steampowered.com/api/appdetails?appids={appid}&l=english"), &[], Duration::from_secs(10)).ok()
        .filter(|r| r.status < 400).and_then(|r| crate::steamweb::parse_appdetails(&r.body, appid)).map(|d| d.name).filter(|n| !n.trim().is_empty());
    Hit { appid, name: name.unwrap_or_else(|| format!("AppID {appid}")), in_library: existing.contains(&appid) }
}

/// `steam_search`: fewer than two letters asks nothing. Err = Steam could not be reached.
/// A Steam appid or store link is looked up by id and comes first (a bare number also runs the name search, after it).
pub fn steam_search(http: &dyn Http, term: &str, existing: &HashSet<i64>) -> Result<Vec<Hit>, String> {
    let q = term.trim();
    if let Some((appid, number_only)) = parse_appid(q) {
        let mut hits = vec![lookup_appid(http, appid, existing)];
        if number_only && q.chars().count() >= 2 { if let Ok(more) = search_by_name(http, q, existing) { hits.extend(more.into_iter().filter(|h| h.appid != appid)); } }
        hits.truncate(12);
        return Ok(hits);
    }
    if q.chars().count() < 2 { return Ok(vec![]); }
    search_by_name(http, q, existing)
}

fn search_by_name(http: &dyn Http, q: &str, existing: &HashSet<i64>) -> Result<Vec<Hit>, String> {
    let r = http.get(&format!("https://store.steampowered.com/api/storesearch/?term={}&l=english&cc=us", enc(q)), &[], Duration::from_secs(10)).map_err(|_| "Could not search Steam. Check your connection and try again.".to_string())?;
    if r.status >= 400 { return Ok(vec![]); }
    Ok(parse_search(&r.body, existing))
}

/// What adding a game came to.
#[derive(Debug, PartialEq)]
pub enum Added { New(i64), /// Already there (the message names it).
    Exists(String) }

/// `add_game` mode `steam`: a normal Steam row with a placeholder name (the re-scrape fills it in); an earlier removal is forgiven since this was asked for by hand.
pub fn add_steam(conn: &Connection, appid: i64, name: &str, now: i64, installed: bool) -> Result<Added, String> {
    if appid <= 0 { return Err("Invalid Steam AppID.".into()); }
    if let Ok(n) = conn.query_row("SELECT COALESCE(name, '') FROM games WHERE appid = ?", [appid], |r| r.get::<_, String>(0)) {
        return Ok(Added::Exists(format!("{} is already in your library.", if n.is_empty() { appid.to_string() } else { n })));
    }
    let _ = conn.execute("DELETE FROM blacklist WHERE appid = ?", [appid]);
    let name: String = name.trim().chars().take(200).collect();
    conn.execute("INSERT OR IGNORE INTO games (appid, name, playtime_forever, last_played, date_added, completion_status, installed, icon_hash, platform, art_fetched, meta_fetched, cheevos_fetched, protondb_fetched, hltb_fetched)
        VALUES (?1, ?2, 0, NULL, ?3, 'Never Played', ?4, '', 'steam', '0', '0', '0', '0', '0')",
        params![appid, if name.is_empty() { format!("AppID {appid}") } else { name }, now, installed as i64]).map_err(|e| e.to_string())?;
    Ok(Added::New(appid))
}

/// `add_game` mode `custom`: a name only, under `custom` with the next negative id; the same name (any case) twice is refused.
pub fn add_custom(conn: &Connection, name: &str, now: i64) -> Result<Added, String> {
    let name: String = name.trim().chars().take(200).collect();
    if name.is_empty() { return Err("Enter a name for the game.".into()); }
    if conn.query_row("SELECT 1 FROM games WHERE platform = 'custom' AND name = ? COLLATE NOCASE", [&name], |_| Ok(())).is_ok() {
        return Ok(Added::Exists(format!("\"{name}\" is already in your library as a manual game.")));
    }
    let min: Option<i64> = conn.query_row("SELECT MIN(appid) FROM games WHERE appid < 0", [], |r| r.get(0)).map_err(|e| e.to_string())?;
    let appid = min.map_or(-1, |m| m - 1);
    conn.execute("INSERT INTO games (appid, name, platform, date_added, completion_status, installed, art_fetched, meta_fetched, cheevos_fetched, protondb_fetched, hltb_fetched)
        VALUES (?1, ?2, 'custom', ?3, 'Never Played', 0, '0', '0', '0', '0', '0')", params![appid, name, now]).map_err(|e| e.to_string())?;
    Ok(Added::New(appid))
}

/// A game of a store with its own id (GOG, say), as a placeholder row under that platform with the next negative id; the same store id twice is refused. The caller fills it in.
pub fn add_store_game(conn: &Connection, platform: &str, platform_id: &str, name: &str, now: i64) -> Result<Added, String> {
    let name: String = name.trim().chars().take(200).collect();
    if name.is_empty() || platform_id.trim().is_empty() { return Err("A store game needs a name and an id.".into()); }
    if let Ok(n) = conn.query_row("SELECT COALESCE(name, '') FROM games WHERE platform = ?1 AND platform_id = ?2", params![platform, platform_id], |r| r.get::<_, String>(0)) {
        return Ok(Added::Exists(format!("{} is already in your library.", if n.is_empty() { platform_id.to_string() } else { n })));
    }
    let min: Option<i64> = conn.query_row("SELECT MIN(appid) FROM games WHERE appid < 0", [], |r| r.get(0)).map_err(|e| e.to_string())?;
    let appid = min.map_or(-1, |m| m - 1);
    conn.execute("INSERT INTO games (appid, name, platform, platform_id, date_added, completion_status, installed, art_fetched, meta_fetched, cheevos_fetched, protondb_fetched, hltb_fetched)
        VALUES (?1, ?2, ?3, ?4, ?5, 'Never Played', 0, '0', '0', '0', '0', '0')", params![appid, name, platform, platform_id, now]).map_err(|e| e.to_string())?;
    Ok(Added::New(appid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{NetError, Response};
    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE games (appid INTEGER PRIMARY KEY, name TEXT, platform TEXT, playtime_forever INTEGER, last_played INTEGER, date_added INTEGER, completion_status TEXT, installed INTEGER, icon_hash TEXT,
            art_fetched TEXT, meta_fetched TEXT, cheevos_fetched TEXT, protondb_fetched TEXT, hltb_fetched TEXT); CREATE TABLE blacklist (appid INTEGER);").unwrap();
        c
    }
    struct Site(String); impl Http for Site { fn get(&self, u: &str, _h: &[(&str, &str)], _t: Duration) -> Result<Response, NetError> { assert!(u.contains("term=half%20life") && u.contains("storesearch"), "{u}"); Ok(Response { status: 200, retry_after: None, body: self.0.clone() }) } }

    #[test] fn search_keeps_apps_flags_owned_ones_and_stops_at_twelve() {
        let items: Vec<Value> = (1..=15).map(|i| serde_json::json!({"id": i, "name": format!("G{i}"), "type": "app"})).chain([serde_json::json!({"id": 99, "name": "OST", "type": "sub"}), serde_json::json!({"name": "no id", "type": "app"})]).collect();
        let body = serde_json::json!({"items": items}).to_string();
        let hits = steam_search(&Site(body), "half life", &[2].into_iter().collect()).unwrap();
        assert_eq!(hits.len(), 12); assert_eq!(hits[1], Hit { appid: 2, name: "G2".into(), in_library: true }); assert!(!hits[0].in_library);
        assert!(steam_search(&Site(String::new()), "a", &HashSet::new()).unwrap().is_empty(), "one letter asks nothing");
        assert!(parse_search("not json", &HashSet::new()).is_empty());
    }
    #[test] fn an_appid_or_store_link_is_recognised() {
        assert_eq!(parse_appid(" 461040 "), Some((461040, true)));
        for l in ["https://store.steampowered.com/app/461040/PICO_PARKClassic_Edition/", "store.steampowered.com/app/461040", "https://steamcommunity.com/app/461040/discussions/", "steam://store/461040", "steam://nav/games/details/461040", "steam://run/461040"] {
            assert_eq!(parse_appid(l), Some((461040, false)), "{l}");
        }
        for l in ["half life", "0", "", "https://example.com/app/461040", "https://store.steampowered.com/", "https://store.steampowered.com/app/abc", "12345678901"] { assert_eq!(parse_appid(l), None, "{l}"); }
    }
    struct Routes(Vec<(&'static str, u16, String)>);
    impl Http for Routes { fn get(&self, u: &str, _h: &[(&str, &str)], _t: Duration) -> Result<Response, NetError> {
        self.0.iter().find(|(k, _, _)| u.contains(k)).map(|(_, s, b)| Response { status: *s, retry_after: None, body: b.clone() }).ok_or(NetError("no route".into())) } }
    const PICO: &str = r#"{"461040": {"success": true, "data": {"steam_appid": 461040, "type": "game", "name": "PICO PARK:Classic Edition"}}}"#;
    #[test] fn a_link_is_looked_up_by_id_and_falls_back_to_a_placeholder_name() {
        let own: HashSet<i64> = [461040].into_iter().collect();
        let h = steam_search(&Routes(vec![("appdetails", 200, PICO.into())]), "https://store.steampowered.com/app/461040/", &HashSet::new()).unwrap();
        assert_eq!(h, vec![Hit { appid: 461040, name: "PICO PARK:Classic Edition".into(), in_library: false }]);
        assert!(steam_search(&Routes(vec![("appdetails", 200, PICO.into()), ("storesearch", 200, "{}".into())]), "461040", &own).unwrap()[0].in_library);
        // delisted (success false), a server error and no connection all give the placeholder, never an error
        for r in [Routes(vec![("appdetails", 200, r#"{"461040": {"success": false}}"#.into())]), Routes(vec![("appdetails", 503, String::new())]), Routes(vec![])] {
            let h = steam_search(&r, "steam://store/461040", &own).unwrap();
            assert_eq!(h, vec![Hit { appid: 461040, name: "AppID 461040".into(), in_library: true }]);
        }
    }
    #[test] fn a_bare_number_is_also_searched_as_a_name() {
        let search = r#"{"items": [{"id": 4, "name": "1942: The Game", "type": "app"}, {"id": 1942, "name": "dup of the id", "type": "app"}]}"#;
        let r = Routes(vec![("appdetails", 200, r#"{"1942": {"success": true, "data": {"steam_appid": 1942, "name": "Some App"}}}"#.into()), ("storesearch", 200, search.into())]);
        let h = steam_search(&r, "1942", &HashSet::new()).unwrap();
        assert_eq!(h.iter().map(|h| (h.appid, h.name.as_str())).collect::<Vec<_>>(), vec![(1942, "Some App"), (4, "1942: The Game")]);
        // a link never runs the name search
        let h = steam_search(&Routes(vec![("appdetails", 200, PICO.into())]), "store.steampowered.com/app/461040", &HashSet::new()).unwrap();
        assert_eq!(h.len(), 1);
    }
    #[test] fn a_steam_game_is_added_once_and_forgiven_a_blacklisting() {
        let c = db(); c.execute("INSERT INTO blacklist VALUES (620)", []).unwrap();
        assert_eq!(add_steam(&c, 620, " Portal 2 ", 1000, true).unwrap(), Added::New(620));
        let (n, p, i, s, bl): (String, String, i64, String, i64) = c.query_row("SELECT name, platform, installed, completion_status, (SELECT COUNT(*) FROM blacklist) FROM games WHERE appid = 620", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).unwrap();
        assert_eq!((n.as_str(), p.as_str(), i, s.as_str(), bl), ("Portal 2", "steam", 1, "Never Played", 0));
        assert!(matches!(add_steam(&c, 620, "x", 1, false).unwrap(), Added::Exists(m) if m.contains("Portal 2")));
        assert_eq!(add_steam(&c, 7, "", 1, false).unwrap(), Added::New(7)); assert_eq!(c.query_row("SELECT name FROM games WHERE appid = 7", [], |r| r.get::<_, String>(0)).unwrap(), "AppID 7");
        assert!(add_steam(&c, 0, "x", 1, false).is_err() && add_steam(&c, -3, "x", 1, false).is_err());
    }
    #[test] fn a_manual_game_gets_the_next_negative_id_and_a_name_only_once() {
        let c = db();
        assert_eq!(add_custom(&c, "  My Game ", 5).unwrap(), Added::New(-1));
        assert_eq!(add_custom(&c, "Other", 5).unwrap(), Added::New(-2));
        assert!(matches!(add_custom(&c, "my game", 5).unwrap(), Added::Exists(_)));
        assert!(add_custom(&c, "  ", 5).is_err());
        assert_eq!(c.query_row("SELECT platform FROM games WHERE appid = -2", [], |r| r.get::<_, String>(0)).unwrap(), "custom");
    }
}
