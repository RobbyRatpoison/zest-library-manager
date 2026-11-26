//! Importing a source's games into the library: the one place that writes rows for a store other than Steam. A source (built-in or plugin) hands over
//! `GameRecord`s and never touches the database; this assigns the negative appid, skips games the user blacklisted, matches a game already in the library by
//! `(platform, platform_id)` and applies the rules of `PLUGINS.md` ("Inserting a game", "Skipping blacklisted games").
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

/// One game as a source reports it. Only `platform_id` and `name` are required.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GameRecord {
    pub platform_id: String, pub name: String, pub slug: Option<String>, pub appname: Option<String>, pub namespace: Option<String>,
    pub playtime_forever: Option<i64>, pub last_played: Option<i64>, pub date_added: Option<i64>, pub release_date: Option<i64>,
    pub developers: Option<String>, pub publishers: Option<String>, pub genres: Option<String>, pub categories: Option<String>, pub tags: Option<String>,
    pub description: Option<String>, pub installed: Option<bool>, pub install_path: Option<String>,
}

fn s(v: &Value, k: &str, max: usize) -> Option<String> { v.get(k).and_then(|x| x.as_str()).map(|x| x.trim().chars().take(max).collect::<String>()).filter(|x| !x.trim().is_empty()) }
fn n(v: &Value, k: &str) -> Option<i64> { v.get(k).and_then(|x| x.as_i64().or_else(|| x.as_f64().map(|f| f as i64))) }
/// The database's list convention: items joined by "," with no space after, an item never holding a comma.
pub(crate) fn list(v: &Value, k: &str) -> Option<String> {
    let items: Vec<String> = match v.get(k)? {
        Value::Array(a) => a.iter().filter_map(|x| x.as_str()).map(|x| x.replace(',', " ").split_whitespace().collect::<Vec<_>>().join(" ")).filter(|x| !x.is_empty()).collect(),
        Value::String(t) => t.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect(),
        _ => return None,
    };
    let mut seen = Vec::new(); for i in items { if !seen.contains(&i) { seen.push(i); } }
    (!seen.is_empty()).then(|| seen.join(","))
}

impl GameRecord {
    /// From the JSON a plugin sent. Unknown keys are ignored; a bad or missing `platform_id` / `name` rejects the record.
    pub fn from_json(v: &Value) -> Result<GameRecord, String> {
        let pid = match v.get("platform_id") { Some(Value::String(t)) => t.trim().to_string(), Some(Value::Number(x)) => x.to_string(), _ => String::new() };
        if pid.is_empty() || pid.len() > 200 { return Err("platform_id is missing or too long".into()); }
        let name = s(v, "name", 500).ok_or("name is missing")?;
        Ok(GameRecord { platform_id: pid, name, slug: s(v, "slug", 300), appname: s(v, "appname", 300), namespace: s(v, "namespace", 300),
            playtime_forever: n(v, "playtime_forever").filter(|x| *x >= 0), last_played: n(v, "last_played").filter(|x| *x > 0), date_added: n(v, "date_added").filter(|x| *x > 0), release_date: n(v, "release_date"),
            developers: list(v, "developers"), publishers: list(v, "publishers"), genres: list(v, "genres"), categories: list(v, "categories"), tags: list(v, "tags"),
            description: s(v, "description", 20000), installed: v.get("installed").and_then(|x| x.as_bool()), install_path: s(v, "install_path", 1000) })
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImportStats { pub new_appids: Vec<i64>, pub added: usize, pub updated: usize, pub skipped_blacklisted: usize, pub rejected: usize,
    /// Records that named a game the store already had under another id (a second purchase of it): folded into the existing row instead of added.
    pub merged: usize }

/// `_next_negative_appid`
fn next_appid(c: &Connection) -> rusqlite::Result<i64> { Ok(c.query_row("SELECT MIN(appid) FROM games WHERE appid < 0", [], |r| r.get::<_, Option<i64>>(0))?.map_or(-1, |m| m - 1)) }

/// Imports `records` for `platform` in one transaction. A game already in the library (same platform and id) keeps its name, status, groups and everything
/// the user or a scrape set; a source refreshes only what it owns (playtime, last played, install state, links) and fills blank metadata. `now` stamps
/// `date_added` when the source gives none. When `full` is true (a complete listing, not a partial batch) nothing is removed: removal is the user's choice.
pub fn import(conn: &mut Connection, platform: &str, records: &[GameRecord], now: i64) -> rusqlite::Result<ImportStats> {
    let mut st = ImportStats::default();
    // a store never imports into an emulated system's platform (the one shared id is allowed, see `sources::SHARED_WITH_EMULATION`)
    if crate::sources::is_emulated_system(platform) && !crate::sources::SHARED_WITH_EMULATION.contains(&platform) { st.rejected = records.len(); return Ok(st); }
    let tx = conn.transaction()?;
    // this store's games by name, so a second purchase of one (another id, the same name) is recognised instead of added
    let mut by_name: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    { let mut q = tx.prepare("SELECT appid, COALESCE(name,'') FROM games WHERE platform = ?1 ORDER BY COALESCE(date_added, 0) DESC, appid ASC")?;
      for row in q.query_map([platform], |x| Ok((x.get::<_, i64>(0)?, x.get::<_, String>(1)?)))?.flatten() { let k = crate::dupes::norm_exact(&row.1); if !k.is_empty() { by_name.insert(k, row.0); } } }
    for r in records {
        // ... and a record that looks like a ROM on disk is never a store game, so it can neither add to nor overwrite an emulated game
        if crate::sources::origin(platform, &r.platform_id) == crate::sources::Origin::Emulated { st.rejected += 1; continue; }
        let blocked: bool = tx.query_row("SELECT 1 FROM blacklist WHERE platform_id = ?1 AND (platform IS NULL OR platform = '' OR platform = ?2)", params![r.platform_id, platform], |_| Ok(true)).optional()?.unwrap_or(false);
        if blocked { st.skipped_blacklisted += 1; continue; }
        let existing: Option<i64> = tx.query_row("SELECT appid FROM games WHERE platform = ?1 AND platform_id = ?2", params![platform, r.platform_id], |x| x.get(0)).optional()?;
        let twin = if existing.is_none() { by_name.get(&crate::dupes::norm_exact(&r.name)).copied() } else { None };
        if let Some(appid) = twin {
            // the same game bought again: the earliest purchase date wins, playtime and last played only grow, blank metadata is filled; the existing row keeps its id and links
            tx.execute("UPDATE games SET date_added = CASE WHEN ?1 IS NOT NULL AND ?1 > 0 AND (COALESCE(date_added,0) = 0 OR ?1 < date_added) THEN ?1 ELSE date_added END,
                    playtime_forever = MAX(COALESCE(playtime_forever,0), COALESCE(?2,0)), last_played = COALESCE(MAX(COALESCE(last_played,0), COALESCE(?3,0)), last_played) WHERE appid = ?4", params![r.date_added, r.playtime_forever, r.last_played, appid])?;
            tx.execute("UPDATE games SET developers = CASE WHEN COALESCE(developers,'') = '' THEN ?1 ELSE developers END, publishers = CASE WHEN COALESCE(publishers,'') = '' THEN ?2 ELSE publishers END,
                    genres = CASE WHEN COALESCE(genres,'') = '' THEN ?3 ELSE genres END, release_date = COALESCE(release_date, ?4) WHERE appid = ?5", params![r.developers, r.publishers, r.genres, r.release_date, appid])?;
            st.merged += 1;
            continue;
        }
        match existing {
            None => {
                let appid = next_appid(&tx)?;
                tx.execute("INSERT INTO games (appid, name, platform, platform_id, platform_slug, platform_appname, platform_ns, date_added, completion_status, installed, install_path, playtime_forever, last_played,
                        release_date, developers, publishers, genres, categories, tags, short_description, art_fetched, meta_fetched, cheevos_fetched, protondb_fetched, hltb_fetched)
                    VALUES (?1,?2,?3,?4,?5,?6,?7,?8,CASE WHEN COALESCE(?11, 0) > 0 THEN 'Unfinished' ELSE 'Never Played' END,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,'0','0','0','0','0')",
                    params![appid, r.name, platform, r.platform_id, r.slug, r.appname, r.namespace, r.date_added.unwrap_or(now), r.installed.unwrap_or(false) as i64, r.install_path, r.playtime_forever.unwrap_or(0), r.last_played,
                        r.release_date, r.developers, r.publishers, r.genres, r.categories, r.tags, r.description])?;
                let k = crate::dupes::norm_exact(&r.name); if !k.is_empty() { by_name.entry(k).or_insert(appid); }
                st.added += 1; st.new_appids.push(appid);
            }
            Some(appid) => {
                // owned by the source: always refreshed when given
                tx.execute("UPDATE games SET playtime_forever = COALESCE(?1, playtime_forever), last_played = COALESCE(MAX(COALESCE(last_played, 0), ?2), last_played),
                        installed = COALESCE(?3, installed), install_path = CASE WHEN ?3 = 0 THEN NULL ELSE COALESCE(?4, install_path) END,
                        platform_slug = COALESCE(?5, platform_slug), platform_appname = COALESCE(?6, platform_appname), platform_ns = COALESCE(?7, platform_ns),
                        completion_status = CASE WHEN COALESCE(completion_status, 'Never Played') = 'Never Played' AND COALESCE(?1, 0) > 0 THEN 'Unfinished' ELSE completion_status END WHERE appid = ?8",
                    params![r.playtime_forever, r.last_played, r.installed.map(|b| b as i64), r.install_path, r.slug, r.appname, r.namespace, appid])?;
                // blank metadata is filled, never overwritten
                tx.execute("UPDATE games SET developers = CASE WHEN COALESCE(developers,'') = '' THEN ?1 ELSE developers END, publishers = CASE WHEN COALESCE(publishers,'') = '' THEN ?2 ELSE publishers END,
                        genres = CASE WHEN COALESCE(genres,'') = '' THEN ?3 ELSE genres END, categories = CASE WHEN COALESCE(categories,'') = '' THEN ?4 ELSE categories END,
                        tags = CASE WHEN COALESCE(tags,'') = '' THEN ?5 ELSE tags END, release_date = COALESCE(release_date, ?6),
                        short_description = CASE WHEN COALESCE(short_description,'') = '' THEN ?7 ELSE short_description END WHERE appid = ?8",
                    params![r.developers, r.publishers, r.genres, r.categories, r.tags, r.release_date, r.description, appid])?;
                st.updated += 1;
            }
        }
    }
    tx.commit()?;
    Ok(st)
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MergeStats { pub merged: usize, pub removed: Vec<i64> }

/// Folds this store's rows that carry the same game name into the earliest one (by `date_added`, then the first created). The kept row takes the best of the copies: the earliest
/// date, the most playtime and latest play, the furthest completion status (Completed, Beaten, Unfinished, Won't Play, Never Played), an install if only another copy had one, and
/// every group any copy was in; hidden-duplicate links that pointed at a removed copy point at the kept one. The other rows are deleted (the caller removes their cached covers).
pub fn merge_same_store(conn: &mut Connection, platform: &str) -> rusqlite::Result<MergeStats> {
    struct R { appid: i64, date: i64, status: String, installed: i64, path: Option<String>, exe: Option<String>, play: i64, last: Option<i64>, groups: String, dup: Option<String>, dup_auto: Option<i64> }
    let tx = conn.transaction()?;
    let mut groups: std::collections::HashMap<String, Vec<R>> = std::collections::HashMap::new();
    { let mut q = tx.prepare("SELECT appid, COALESCE(name,''), COALESCE(date_added,0), COALESCE(completion_status,'Never Played'), COALESCE(installed,0), install_path, platform_executable, COALESCE(playtime_forever,0), last_played, COALESCE(groups,''), duplicate_of, duplicate_auto FROM games WHERE platform = ?1")?;
      for row in q.query_map([platform], |x| Ok((x.get::<_, String>(1)?, R { appid: x.get(0)?, date: x.get(2)?, status: x.get(3)?, installed: x.get(4)?, path: x.get(5)?, exe: x.get(6)?, play: x.get(7)?, last: x.get(8)?, groups: x.get(9)?, dup: x.get(10)?, dup_auto: x.get(11)? })))?.flatten() {
          let k = crate::dupes::norm_exact(&row.0); if !k.is_empty() { groups.entry(k).or_default().push(row.1); } } }
    let rank = |s: &str| match s { "Completed" => 4, "Beaten" => 3, "Unfinished" => 2, "Won't Play" => 1, _ => 0 };
    let mut out = MergeStats::default();
    let mut keys: Vec<String> = groups.iter().filter(|(_, v)| v.len() > 1).map(|(k, _)| k.clone()).collect(); keys.sort();
    for k in keys {
        let mut g = groups.remove(&k).unwrap_or_default();
        // earliest first (a missing date counts as the latest); among equals the first created (appids count down, so the larger one)
        g.sort_by_key(|r| (if r.date > 0 { r.date } else { i64::MAX }, std::cmp::Reverse(r.appid)));
        let (win, losers) = g.split_first().expect("a group has a member");
        let date = g.iter().map(|r| r.date).filter(|d| *d > 0).min().unwrap_or(0);
        let play = g.iter().map(|r| r.play).max().unwrap_or(0);
        let last = g.iter().filter_map(|r| r.last).max();
        let status = g.iter().max_by_key(|r| (rank(&r.status), std::cmp::Reverse(r.appid != win.appid))).map(|r| r.status.clone()).unwrap_or_else(|| win.status.clone());
        let (mut inst, mut path, mut exe) = (win.installed, win.path.clone(), win.exe.clone());
        if inst == 0 { if let Some(l) = losers.iter().find(|l| l.installed != 0) { inst = 1; path = l.path.clone(); exe = l.exe.clone(); } }
        let mut grp: Vec<String> = vec![];
        for r in &g { for x in r.groups.split(',').map(str::trim).filter(|x| !x.is_empty()) { if !grp.iter().any(|y| y == x) { grp.push(x.to_string()); } } }
        let (dup, dup_auto) = if win.dup.is_some() { (win.dup.clone(), win.dup_auto) } else { losers.iter().find(|l| l.dup.is_some()).map(|l| (l.dup.clone(), l.dup_auto)).unwrap_or((None, None)) };
        tx.execute("UPDATE games SET date_added = NULLIF(?1,0), playtime_forever = ?2, last_played = ?3, completion_status = ?4, installed = ?5, install_path = ?6, platform_executable = ?7, groups = NULLIF(?8,''), duplicate_of = ?9, duplicate_auto = ?10 WHERE appid = ?11",
            params![date, play, last, status, inst, path, exe, grp.join(","), dup, dup_auto, win.appid])?;
        for l in losers {
            tx.execute("UPDATE games SET duplicate_of = ?1 WHERE duplicate_of = ?2", params![win.appid.to_string(), l.appid.to_string()])?;
            tx.execute("DELETE FROM games WHERE appid = ?1", [l.appid])?;
            out.removed.push(l.appid);
        }
        out.merged += losers.len();
    }
    tx.commit()?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE games (appid INTEGER PRIMARY KEY, name TEXT, platform TEXT, platform_id TEXT, platform_slug TEXT, platform_appname TEXT, platform_ns TEXT, date_added INTEGER, completion_status TEXT, installed INTEGER, install_path TEXT, playtime_forever INTEGER, last_played INTEGER,
            release_date INTEGER, developers TEXT, publishers TEXT, genres TEXT, categories TEXT, tags TEXT, short_description TEXT, art_fetched TEXT, meta_fetched TEXT, cheevos_fetched TEXT, protondb_fetched TEXT, hltb_fetched TEXT, groups TEXT, duplicate_of TEXT, duplicate_auto INTEGER, platform_executable TEXT);
            CREATE TABLE blacklist (appid INTEGER PRIMARY KEY, name TEXT, date_blacklisted TEXT, platform_id TEXT, platform TEXT);
            INSERT INTO games (appid, name, platform, platform_id) VALUES (10, 'Steam Game', 'steam', '10');").unwrap(); c
    }
    fn rec(id: &str, name: &str) -> GameRecord { GameRecord { platform_id: id.into(), name: name.into(), ..Default::default() } }

    #[test] fn json_records_are_validated_and_lists_normalised() {
        let r = GameRecord::from_json(&json!({"platform_id": 1207, "name": " Doom ", "tags": ["FPS", "Gore, Violence", "FPS", ""], "genres": "Action, Shooter,", "playtime_forever": 90.7, "last_played": 0, "bogus": 1})).unwrap();
        assert_eq!((r.platform_id.as_str(), r.name.as_str()), ("1207", "Doom")); assert_eq!(r.tags.as_deref(), Some("FPS,Gore Violence")); assert_eq!(r.genres.as_deref(), Some("Action,Shooter"));
        assert_eq!((r.playtime_forever, r.last_played), (Some(90), None));
        assert!(GameRecord::from_json(&json!({"name": "x"})).is_err()); assert!(GameRecord::from_json(&json!({"platform_id": "a", "name": "  "})).is_err()); assert!(GameRecord::from_json(&json!({"platform_id": "", "name": "x"})).is_err());
    }
    #[test] fn new_games_get_the_next_negative_appid_and_the_starting_state() {
        let mut c = db();
        let st = import(&mut c, "gog", &[rec("a", "A"), rec("b", "B")], 1000).unwrap();
        assert_eq!((st.added, st.updated), (2, 0));
        let ids: Vec<i64> = c.prepare("SELECT appid FROM games WHERE platform='gog' ORDER BY appid DESC").unwrap().query_map([], |r| r.get(0)).unwrap().map(|x| x.unwrap()).collect();
        assert_eq!(ids, [-1, -2]);
        let row: (String, i64, String, String) = c.query_row("SELECT completion_status, date_added, art_fetched, meta_fetched FROM games WHERE platform_id='a'", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap();
        assert_eq!(row, ("Never Played".into(), 1000, "0".into(), "0".into()));
        assert_eq!(import(&mut c, "epic_games", &[rec("c", "C")], 1).unwrap().added, 1);
        assert_eq!(c.query_row("SELECT appid FROM games WHERE platform_id='c'", [], |r| r.get::<_, i64>(0)).unwrap(), -3);
    }
    #[test] fn playtime_makes_a_new_game_unfinished_and_promotes_a_never_played_one_but_nothing_else() {
        let mut c = db();
        let mut played = rec("p", "P"); played.playtime_forever = Some(30);
        import(&mut c, "playstation", &[played.clone(), rec("q", "Q")], 1).unwrap();
        let st = |c: &Connection, id: &str| c.query_row("SELECT completion_status FROM games WHERE platform_id = ?", [id], |r| r.get::<_, String>(0)).unwrap();
        assert_eq!((st(&c, "p").as_str(), st(&c, "q").as_str()), ("Unfinished", "Never Played"));
        let mut later = rec("q", "Q"); later.playtime_forever = Some(5);
        c.execute("UPDATE games SET completion_status = 'Beaten' WHERE platform_id = 'p'", []).unwrap();
        import(&mut c, "playstation", &[played, later], 2).unwrap();
        assert_eq!((st(&c, "p").as_str(), st(&c, "q").as_str()), ("Beaten", "Unfinished"), "a chosen status is never overwritten");
    }
    #[test] fn blacklisted_ids_are_skipped_but_only_for_their_platform() {
        let mut c = db();
        c.execute("INSERT INTO blacklist (appid, name, platform_id, platform) VALUES (-50, 'x', 'bad', 'gog')", []).unwrap();
        c.execute("INSERT INTO blacklist (appid, name, platform_id, platform) VALUES (-51, 'y', 'anyplat', NULL)", []).unwrap();
        let st = import(&mut c, "gog", &[rec("bad", "Bad"), rec("anyplat", "Any"), rec("ok", "Ok")], 1).unwrap();
        assert_eq!((st.added, st.skipped_blacklisted), (1, 2));
        assert_eq!(import(&mut c, "epic_games", &[rec("bad", "Same id, other store")], 1).unwrap().added, 1);
    }
    #[test] fn a_known_game_is_refreshed_not_replaced() {
        let mut c = db();
        let mut r = rec("a", "Original"); r.playtime_forever = Some(10); r.genres = Some("Action".into()); r.installed = Some(true); r.install_path = Some("/g/a".into());
        import(&mut c, "gog", &[r.clone()], 1).unwrap();
        c.execute("UPDATE games SET name = 'My Rename', completion_status = 'Beaten', tags = 'Mine' WHERE platform_id = 'a'", []).unwrap();
        let mut r2 = rec("a", "Source Name"); r2.playtime_forever = Some(99); r2.genres = Some("RPG".into()); r2.tags = Some("Theirs".into()); r2.developers = Some("Dev".into()); r2.installed = Some(false);
        let st = import(&mut c, "gog", &[r2], 5).unwrap();
        assert_eq!((st.added, st.updated), (0, 1));
        let row: (String, String, i64, String, String, String, i64, Option<String>) = c.query_row("SELECT name, completion_status, playtime_forever, genres, tags, developers, installed, install_path FROM games WHERE platform_id='a'", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?))).unwrap();
        assert_eq!(row, ("My Rename".into(), "Beaten".into(), 99, "Action".into(), "Mine".into(), "Dev".into(), 0, None), "name, status, existing genres/tags kept; playtime and install state refreshed; blank developers filled");
        assert_eq!(c.query_row("SELECT COUNT(*) FROM games WHERE platform='gog'", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    }
    #[test] fn a_store_cannot_write_into_emulated_games() {
        let mut c = db();
        c.execute("INSERT INTO games (appid, name, platform, platform_id) VALUES (-1, 'Halo (ROM)', 'xbox', '/roms/xbox/halo.iso')", []).unwrap();
        let st = import(&mut c, "xbox", &[rec("/roms/xbox/halo.iso", "Hijack"), rec("store:ABC", "Sea of Thieves")], 5).unwrap();
        assert_eq!((st.added, st.rejected), (1, 1));
        assert_eq!(c.query_row("SELECT name FROM games WHERE platform_id = '/roms/xbox/halo.iso'", [], |r| r.get::<_, String>(0)).unwrap(), "Halo (ROM)", "the emulated game is untouched");
        assert_eq!(import(&mut c, "snes", &[rec("a", "A")], 1).unwrap().rejected, 1, "an emulated-only system id is refused outright");
        assert_eq!(c.query_row("SELECT COUNT(*) FROM games WHERE platform = 'snes'", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    }
    #[test] fn last_played_never_moves_backwards() {
        let mut c = db();
        let mut r = rec("a", "A"); r.last_played = Some(500); import(&mut c, "gog", &[r.clone()], 1).unwrap();
        r.last_played = Some(100); import(&mut c, "gog", &[r], 1).unwrap();
        assert_eq!(c.query_row("SELECT last_played FROM games WHERE platform_id='a'", [], |r| r.get::<_, i64>(0)).unwrap(), 500);
    }

    fn dated(id: &str, name: &str, date: i64) -> GameRecord { GameRecord { platform_id: id.into(), name: name.into(), date_added: Some(date), ..Default::default() } }
    fn rows(c: &Connection, platform: &str) -> Vec<(i64, String, i64)> { let mut q = c.prepare("SELECT appid, platform_id, COALESCE(date_added,0) FROM games WHERE platform = ?1 ORDER BY appid DESC").unwrap(); q.query_map([platform], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().flatten().collect() }

    #[test] fn a_second_purchase_of_a_game_is_folded_into_the_existing_row_with_the_earliest_date() {
        let mut c = db();
        // the same game under three ids in one batch (newest purchase listed first), then again in a later import
        let st = import(&mut c, "humble", &[dated("sh_2020", "Super Hexagon", 3000), dated("sh_store", "Super  Hexagon!", 1000), dated("sh_android", "super hexagon", 2000), dated("other", "Other Game", 500)], 99).unwrap();
        assert_eq!((st.added, st.merged), (2, 2));
        let r = rows(&c, "humble"); assert_eq!(r.len(), 2);
        assert_eq!(r.iter().find(|x| x.1 == "sh_2020").unwrap().2, 1000, "the earliest purchase date");
        let st = import(&mut c, "humble", &[dated("sh_store", "Super Hexagon", 1000), dated("sh_2020", "Super Hexagon", 3000), dated("sh_new", "Super Hexagon", 4000)], 99).unwrap();
        assert_eq!((st.added, st.updated, st.merged), (0, 1, 2));   // the known id is refreshed ("sh_2020"), the others are folded
        assert_eq!(rows(&c, "humble").len(), 2);
        // another store with the same name is a different library entry
        assert_eq!(import(&mut c, "gog", &[dated("g1", "Super Hexagon", 10)], 99).unwrap().added, 1);
    }
    #[test] fn merging_existing_copies_keeps_the_earliest_and_the_best_of_each() {
        let mut c = db();
        c.execute_batch("INSERT INTO games (appid, name, platform, platform_id, date_added, completion_status, installed, install_path, platform_executable, playtime_forever, last_played, groups, duplicate_of, duplicate_auto) VALUES
            (-1531, 'Super Hexagon', 'humble', 'c', 1585742532, 'Never Played', 0, NULL, NULL, 0, NULL, 'Arcade', '221640', 1),
            (-1564, 'Super Hexagon', 'humble', 'b', 1396586015, 'Beaten', 0, NULL, NULL, 90, 500, 'Favourites,Arcade', NULL, NULL),
            (-2286, 'Super Hexagon', 'humble', 'a', 1394570479, 'Unfinished', 1, '/g/SH', 'x/run', 30, 900, NULL, NULL, NULL),
            (-1600, 'Hidden Link', 'humble', 'd', 5, 'Never Played', 0, NULL, NULL, 0, NULL, NULL, '-1531', 1),
            (-1700, 'Lone', 'humble', 'e', 7, 'Never Played', 0, NULL, NULL, 0, NULL, NULL, NULL, NULL);").unwrap();
        let m = merge_same_store(&mut c, "humble").unwrap();
        assert_eq!((m.merged, { let mut r = m.removed.clone(); r.sort(); r }), (2, vec![-1564, -1531]));
        let g = |col: &str| -> Option<String> { c.query_row(&format!("SELECT CAST({col} AS TEXT) FROM games WHERE appid = -2286"), [], |r| r.get(0)).unwrap() };
        assert_eq!((g("date_added").as_deref(), g("completion_status").as_deref(), g("playtime_forever").as_deref(), g("last_played").as_deref()), (Some("1394570479"), Some("Beaten"), Some("90"), Some("900")));
        assert_eq!((g("installed").as_deref(), g("install_path").as_deref(), g("platform_executable").as_deref(), g("groups").as_deref()), (Some("1"), Some("/g/SH"), Some("x/run"), Some("Favourites,Arcade")));
        assert_eq!(g("duplicate_of").as_deref(), Some("221640"), "the hidden-as-a-Steam-copy link is kept");
        let pointer: Option<String> = c.query_row("SELECT duplicate_of FROM games WHERE appid = -1600", [], |r| r.get(0)).unwrap();
        assert_eq!(pointer.as_deref(), Some("-2286"), "a link to a removed copy follows to the kept one");
        assert_eq!(c.query_row("SELECT COUNT(*) FROM games WHERE platform = 'humble'", [], |r| r.get::<_, i64>(0)).unwrap(), 3);
        assert_eq!(merge_same_store(&mut c, "humble").unwrap(), MergeStats::default());   // nothing left to merge
    }

    /// `ZEST_REAL_DB=/path/games.db cargo test real_library_merge -- --ignored --nocapture`: runs the merge on a COPY of a real library and says what it would remove.
    #[test] #[ignore] fn real_library_merge() {
        let Ok(path) = std::env::var("ZEST_REAL_DB") else { return };
        let copy = std::env::temp_dir().join(format!("zest_real_merge_{}.db", std::process::id()));
        std::fs::copy(&path, &copy).unwrap();
        let mut c = Connection::open(&copy).unwrap();
        let platforms: Vec<String> = c.prepare("SELECT DISTINCT platform FROM games WHERE platform != 'steam'").unwrap().query_map([], |r| r.get(0)).unwrap().flatten().collect();
        for p in platforms {
            let names: std::collections::HashMap<i64, String> = c.prepare("SELECT appid, name FROM games WHERE platform = ?1").unwrap().query_map([&p], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().flatten().collect();
            let m = merge_same_store(&mut c, &p).unwrap();
            if m.merged > 0 { println!("{p}: {} removed: {:?}", m.merged, m.removed.iter().map(|a| format!("{a} {}", names[a])).collect::<Vec<_>>()); }
        }
        let _ = std::fs::remove_file(&copy);
    }
}
