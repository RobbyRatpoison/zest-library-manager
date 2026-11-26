//! Derived values that have to be rewritten when a setting changes.
use rusqlite::Connection;

/// `database.recalculate_weighted_scores`: rewrite `games.weighted_percentage` from the raw `review_percentage` / `total_reviews` stored by the scrape, with the current half-trust
/// count (Settings -> Tuning -> Review Scores). Steam rows only: stores that report reviews (Epic Games) use their own formula, which this must not overwrite. Games with no review
/// count are left alone; the `typeof` filter skips legacy rows holding `''` (`'' > 0` is true in SQLite). Returns the number of rows rescored. A recompute at unchanged settings changes nothing.
pub fn weighted_scores(conn: &mut Connection, half_trust: f64) -> rusqlite::Result<usize> {
    let rows: Vec<(i64, f64, i64)> = {
        let mut st = conn.prepare("SELECT appid, review_percentage, total_reviews FROM games WHERE platform = 'steam' AND typeof(total_reviews) IN ('integer', 'real') AND total_reviews > 0 AND typeof(review_percentage) IN ('integer', 'real')")?;
        let it = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?, r.get::<_, f64>(2)? as i64)))?;
        it.flatten().collect()
    };
    let tx = conn.transaction()?;
    {
        let mut up = tx.prepare("UPDATE games SET weighted_percentage = ?1 WHERE appid = ?2")?;
        for (appid, pct, n) in &rows { up.execute(rusqlite::params![crate::steamweb::weighted_review_score(*pct, *n, half_trust), appid])?; }
    }
    tx.commit()?;
    Ok(rows.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn only_steam_rows_with_a_real_review_count_are_rescored() {
        let mut c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE games (appid INTEGER PRIMARY KEY, platform TEXT, review_percentage, total_reviews, weighted_percentage);
            INSERT INTO games VALUES (1,'steam',95,10,0),(2,'steam',95,100000,0),(3,'steam',NULL,50,7),(4,'steam',80,0,7),(5,'epic_games',90,500,42),(6,'steam',90,'',7),(7,'steam',60,25.0,0);").unwrap();
        // half-trust 10 (the default): the same numbers the scraper stores
        assert_eq!(weighted_scores(&mut c, 10.0).unwrap(), 3);
        fn w(c: &Connection, id: i64) -> i64 { c.query_row("SELECT weighted_percentage FROM games WHERE appid = ?", [id], |r| r.get::<_, i64>(0)).unwrap() }
        assert_eq!(w(&c, 1), crate::steamweb::weighted_review_score(95.0, 10, 10.0));
        assert_eq!(w(&c, 2), crate::steamweb::weighted_review_score(95.0, 100000, 10.0));
        assert_eq!(w(&c, 7), crate::steamweb::weighted_review_score(60.0, 25, 10.0));
        assert_eq!((w(&c, 3), w(&c, 4), w(&c, 5), w(&c, 6)), (7, 7, 42, 7), "no percentage, no count, another store, a legacy empty string: untouched");
        // a higher half-trust count pulls the same small sample further toward 50
        weighted_scores(&mut c, 300.0).unwrap();
        assert!(w(&c, 1) < crate::steamweb::weighted_review_score(95.0, 10, 10.0) && w(&c, 1) > 50);
        // unchanged settings: unchanged values
        let before: Vec<i64> = (1..=7).map(|i| w(&c, i)).collect(); weighted_scores(&mut c, 300.0).unwrap();
        assert_eq!((1..=7).map(|i| w(&c, i)).collect::<Vec<_>>(), before);
    }
}
