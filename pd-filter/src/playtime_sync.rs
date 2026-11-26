//! The startup sync of playtime and achievements for Steam games, with no web calls where Steam's own files can answer. A port of PlayDate's
//! `sync_recent_playtime`: playtime and last played come from `localconfig.vdf`; for a game whose playtime changed (you just played it, so Steam has just
//! refreshed its stats) the achievement counts are read from `appcache/stats`; a game with no such file is returned for the caller to ask the web API about
//! (PlayDate asked the API for every changed game). Never Played with playtime becomes Unfinished when the setting is on.
use crate::steamlocal::LocalApp;
use rusqlite::{params, Connection};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Outcome {
    /// Games whose playtime was written.
    pub updated: usize,
    /// Of those, the ones whose playtime changed.
    pub changed: usize,
    /// Changed games whose counts came from Steam's local files.
    pub from_files: usize,
    /// Changed games with no usable local file: ask the web API (needs a key).
    pub need_web: Vec<i64>,
    pub promoted: usize,
}

/// `account32`: the Steam account number the stats files are named by (`steamstats::account32`); `None` skips the file step.
pub fn sync(conn: &Connection, local: &[LocalApp], steam_root: &Path, account32: Option<u64>, promote_unfinished: bool) -> rusqlite::Result<Outcome> {
    let mut out = Outcome::default();
    // IMMEDIATE: take the write lock now. A deferred transaction that reads first and writes later fails at once with "database is locked" (no waiting) when another
    // connection wrote in between, which is what happened when the app was busy writing at start.
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    {
        let mut get = tx.prepare("SELECT playtime_forever, completion_status FROM games WHERE appid = ?")?;
        let mut put_both = tx.prepare("UPDATE games SET playtime_forever = ?, last_played = ? WHERE appid = ?")?;
        let mut put_time = tx.prepare("UPDATE games SET playtime_forever = ? WHERE appid = ?")?;
        let mut put_ach = tx.prepare("UPDATE games SET unlocked_achievements = ?, total_achievements = ? WHERE appid = ?")?;
        let mut put_status = tx.prepare("UPDATE games SET completion_status = 'Unfinished' WHERE appid = ?")?;
        for g in local {
            let Ok((old, status)) = get.query_row([g.appid], |r| Ok((r.get::<_, Option<i64>>(0)?.unwrap_or(0), r.get::<_, Option<String>>(1)?))) else { continue };
            match g.last_played { Some(t) => put_both.execute(params![g.playtime_forever, t, g.appid])?, None => put_time.execute(params![g.playtime_forever, g.appid])? };
            out.updated += 1;
            if g.playtime_forever == old { continue; }
            out.changed += 1;
            match account32.and_then(|a| crate::steamstats::local_counts(steam_root, a, g.appid)) {
                Some((unlocked, total)) => { put_ach.execute(params![unlocked, total, g.appid])?; out.from_files += 1; }
                None => out.need_web.push(g.appid),
            }
            if promote_unfinished && g.playtime_forever > 0 && status.as_deref().map_or(true, |s| s == "Never Played") { put_status.execute([g.appid])?; out.promoted += 1; }
        }
    }
    tx.commit()?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::steamstats::tests::{schema, user};

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute("CREATE TABLE games (appid INTEGER PRIMARY KEY, name TEXT, playtime_forever INTEGER, last_played INTEGER, completion_status TEXT, unlocked_achievements INTEGER, total_achievements INTEGER)", []).unwrap();
        for (a, pt, st) in [(1, 0, "Never Played"), (2, 100, "Unfinished"), (3, 50, "Never Played"), (4, 10, "Beaten"), (5, 0, "Never Played")] {
            c.execute("INSERT INTO games VALUES (?, ?, ?, NULL, ?, 0, 5)", params![a, format!("G{a}"), pt, st]).unwrap();
        }
        c
    }
    fn row(c: &Connection, a: i64) -> (i64, Option<i64>, String, i64, i64) {
        c.query_row("SELECT playtime_forever, last_played, completion_status, unlocked_achievements, total_achievements FROM games WHERE appid = ?", [a], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).unwrap()
    }
    fn app(appid: i64, pt: i64, lp: Option<i64>) -> LocalApp { LocalApp { appid, playtime_forever: pt, last_played: lp } }

    #[test] fn changed_playtime_reads_the_local_files_and_unchanged_games_are_left_alone() {
        let root = std::env::temp_dir().join(format!("zest_pts_{}", std::process::id())); let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("appcache/stats")).unwrap();
        let put = |a: i64, n, m, d| { std::fs::write(root.join(format!("appcache/stats/UserGameStatsSchema_{a}.bin")), schema(&a.to_string(), n, m)).unwrap(); std::fs::write(root.join(format!("appcache/stats/UserGameStats_9_{a}.bin")), user(d, None)).unwrap(); };
        put(1, 10, 0, 0b111); put(2, 4, 0, 0b1111);
        let c = db();
        let local = [app(1, 90, Some(1_700_000_000)), app(2, 100, Some(1_700_000_500)), app(3, 80, None), app(4, 10, None), app(99, 5, None)];
        let o = sync(&c, &local, &root, Some(9), true).unwrap();
        assert_eq!((o.updated, o.changed, o.from_files, o.need_web.clone(), o.promoted), (4, 2, 1, vec![3], 2));   // 99 is not in the library
        assert_eq!(row(&c, 1), (90, Some(1_700_000_000), "Unfinished".into(), 3, 10), "playtime, last played, counts from the file, promoted");
        assert_eq!(row(&c, 2), (100, Some(1_700_000_500), "Unfinished".into(), 0, 5), "same playtime: last played refreshed, counts untouched");
        assert_eq!(row(&c, 3), (80, None, "Unfinished".into(), 0, 5), "no file: counts untouched, left for the web, still promoted");
        assert_eq!(row(&c, 4).2, "Beaten");
        assert_eq!(row(&c, 5).0, 0, "not mentioned by Steam: untouched");
    }
    #[test] fn promotion_can_be_switched_off_and_a_missing_account_skips_the_files() {
        let c = db();
        let o = sync(&c, &[app(1, 30, None)], Path::new("/nonexistent"), None, false).unwrap();
        assert_eq!((o.promoted, o.need_web), (0, vec![1]));
        assert_eq!(row(&c, 1).2, "Never Played");
        // played-for-nothing never promotes
        let o = sync(&c, &[app(5, 0, None)], Path::new("/nonexistent"), None, true).unwrap();
        assert_eq!((o.changed, o.promoted), (0, 0));
    }
}
