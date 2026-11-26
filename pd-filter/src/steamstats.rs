//! Achievement counts from Steam's own cache, no web call and no API key. Steam keeps, per game, `appcache/stats/UserGameStatsSchema_<app>.bin` (every
//! achievement, as bits of bit-field stats) and `UserGameStats_<account>_<app>.bin` (which bits you hold). Both are Valve binary KeyValues. The files exist
//! for the games Steam has loaded stats for and are refreshed when you play, which is exactly when Zest asks. Checked against 2,389 games on a real library:
//! 2,367 matched the web API counts exactly; the rest were games whose developer changed the achievements after the file was cached, or had no file.
use std::collections::BTreeMap;
use std::path::Path;

/// A Valve binary KeyValues value (the kinds these files use).
#[derive(Debug, Clone, PartialEq)]
pub enum Kv { Obj(BTreeMap<String, Kv>), Str(String), Int(i64) }

impl Kv {
    pub fn get(&self, k: &str) -> Option<&Kv> { if let Kv::Obj(m) = self { m.get(k) } else { None } }
    pub fn int(&self) -> Option<i64> { if let Kv::Int(i) = self { Some(*i) } else { None } }
}

fn parse_obj(b: &[u8], i: &mut usize) -> Option<BTreeMap<String, Kv>> {
    let mut out = BTreeMap::new();
    loop {
        let t = *b.get(*i)?; *i += 1;
        if t == 8 { return Some(out); }
        let end = b[*i..].iter().position(|c| *c == 0)? + *i;
        let key = String::from_utf8_lossy(&b[*i..end]).into_owned();
        *i = end + 1;
        let v = match t {
            0 => Kv::Obj(parse_obj(b, i)?),
            1 => { let e = b[*i..].iter().position(|c| *c == 0)? + *i; let s = String::from_utf8_lossy(&b[*i..e]).into_owned(); *i = e + 1; Kv::Str(s) }
            2 | 3 | 4 | 6 => { let a: [u8; 4] = b.get(*i..*i + 4)?.try_into().ok()?; *i += 4; Kv::Int(i32::from_le_bytes(a) as i64) }
            7 => { let a: [u8; 8] = b.get(*i..*i + 8)?.try_into().ok()?; *i += 8; Kv::Int(u64::from_le_bytes(a) as i64) }
            _ => return None,
        };
        out.insert(key, v);
    }
}

/// Parses a binary KeyValues file; `None` when it is not one (a truncated or foreign file).
pub fn parse_kv(b: &[u8]) -> Option<Kv> { let mut i = 0; parse_obj(b, &mut i).map(Kv::Obj) }

/// (unlocked, total) from a schema file and a user-stats file's contents. `None` when the schema lists no achievements or either file is unreadable.
pub fn count_achievements(schema: &[u8], user: &[u8]) -> Option<(i64, i64)> {
    let schema = parse_kv(schema)?;
    let Kv::Obj(top) = &schema else { return None };
    let stats = top.values().next()?.get("stats")?;
    let Kv::Obj(stats) = stats else { return None };
    let user = parse_kv(user)?;
    let cache = user.get("cache");
    let (mut unlocked, mut total) = (0i64, 0i64);
    for (id, def) in stats {
        let Some(Kv::Obj(bits)) = def.get("bits") else { continue };
        let mask = cache.and_then(|c| c.get(id)).and_then(|s| s.get("data")).and_then(|d| d.int()).unwrap_or(0) as u32;
        for b in bits.keys() {
            total += 1;
            if let Ok(n) = b.parse::<u32>() { if n < 32 && (mask >> n) & 1 == 1 { unlocked += 1; } }
        }
    }
    (total > 0).then_some((unlocked, total))
}

/// The counts for one game from `<steam root>/appcache/stats`, for the account with this 32-bit id.
pub fn local_counts(steam_root: &Path, account32: u64, appid: i64) -> Option<(i64, i64)> {
    let d = steam_root.join("appcache/stats");
    let schema = std::fs::read(d.join(format!("UserGameStatsSchema_{appid}.bin"))).ok()?;
    let user = std::fs::read(d.join(format!("UserGameStats_{account32}_{appid}.bin"))).ok()?;
    count_achievements(&schema, &user)
}

/// The 32-bit account id Steam names these files by, from a 64-bit Steam id.
pub fn account32(steam_id64: &str) -> Option<u64> { steam_id64.trim().parse::<u64>().ok()?.checked_sub(76561197960265728) }

#[cfg(test)]
pub mod tests {
    use super::*;
    /// Builders for the binary form, used here and by the sync tests.
    pub fn obj(key: &str, body: Vec<u8>) -> Vec<u8> { let mut v = vec![0]; v.extend(key.as_bytes()); v.push(0); v.extend(body); v.push(8); v }
    pub fn int(key: &str, n: i32) -> Vec<u8> { let mut v = vec![2]; v.extend(key.as_bytes()); v.push(0); v.extend(n.to_le_bytes()); v }
    pub fn text(key: &str, s: &str) -> Vec<u8> { let mut v = vec![1]; v.extend(key.as_bytes()); v.push(0); v.extend(s.as_bytes()); v.push(0); v }
    /// A schema with `n` achievements in one bit-field stat (id "1"), a second bit-field "2" with `m`, and a plain counter stat that must not count.
    pub fn schema(app: &str, n: u32, m: u32) -> Vec<u8> {
        let bits = |k: u32| (0..k).flat_map(|b| obj(&b.to_string(), text("name", &format!("ACH_{b}")))).collect::<Vec<u8>>();
        let mut stats = obj("1", obj("bits", bits(n)));
        if m > 0 { stats.extend(obj("2", obj("bits", bits(m)))); }
        stats.extend(obj("3", [int("type", 1), text("name", "kills")].concat()));
        let mut f = obj(app, obj("stats", stats)); f.push(8); f   // a file ends with the root's terminator
    }
    pub fn user(data1: i32, data2: Option<i32>) -> Vec<u8> {
        let mut c = int("crc", 7); c.extend(int("PendingChanges", 0)); c.extend(obj("1", int("data", data1)));
        if let Some(d) = data2 { c.extend(obj("2", int("data", d))); }
        let mut f = obj("cache", c); f.push(8); f
    }

    #[test] fn bits_of_every_bit_field_stat_are_counted_and_plain_stats_are_not() {
        assert_eq!(count_achievements(&schema("10", 9, 0), &user(0b1_1110_1111, None)), Some((8, 9)));
        assert_eq!(count_achievements(&schema("10", 32, 5), &user(-1, Some(0b10101))), Some((32 + 3, 37)), "bit 31 set: a negative int is a full mask");
        assert_eq!(count_achievements(&schema("10", 4, 0), &user(0, None)), Some((0, 4)));
        // a user file with no entry for the stat means nothing unlocked
        let mut u = int("crc", 1); u.extend(obj("9", int("data", 5)));
        assert_eq!(count_achievements(&schema("10", 3, 0), &[obj("cache", u), vec![8]].concat()), Some((0, 3)));
    }
    #[test] fn nothing_to_count_or_unreadable_files_give_none() {
        assert_eq!(count_achievements(&[obj("10", obj("stats", obj("3", int("type", 1)))), vec![8]].concat(), &user(1, None)), None);
        assert_eq!(count_achievements(&[0, b'x'], &user(1, None)), None);
        assert_eq!(count_achievements(&schema("10", 3, 0), b""), None);
        let s = schema("10", 3, 0);
        assert_eq!(count_achievements(&s[..s.len() - 4], &user(1, None)), None, "truncated");
        assert_eq!(account32("76561198054814426"), Some(94548698));
        assert_eq!(account32("nope"), None);
    }
    #[test] fn files_are_read_from_the_stats_folder() {
        let d = std::env::temp_dir().join(format!("zest_stats_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("appcache/stats")).unwrap();
        std::fs::write(d.join("appcache/stats/UserGameStatsSchema_77.bin"), schema("77", 6, 0)).unwrap();
        std::fs::write(d.join("appcache/stats/UserGameStats_5_77.bin"), user(0b111, None)).unwrap();
        assert_eq!(local_counts(&d, 5, 77), Some((3, 6)));
        assert_eq!(local_counts(&d, 6, 77), None, "another account's file does not exist");
        assert_eq!(local_counts(&d, 5, 78), None);
    }
    /// The one real file known to be in a stranger-proof state: skipped when this machine has no such Steam folder.
    #[test] fn a_real_steam_cache_matches_the_known_counts_when_present() {
        let root = std::path::Path::new(&std::env::var("HOME").unwrap_or_default()).join(".local/share/Steam");
        if let Some(c) = local_counts(&root, 94548698, 1135040) { assert_eq!(c, (8, 8)); }
    }
}

#[cfg(test)]
mod real_library {
    /// Every game on this machine's real Steam folder against the counts the dev database holds (run with `--ignored`; the numbers were 2,367 of 2,389 on 2026-11-02).
    #[test] #[ignore]
    fn local_counts_agree_with_the_database() {
        let home = std::env::var("HOME").unwrap();
        let root = std::path::Path::new(&home).join(".local/share/Steam");
        let db = rusqlite::Connection::open_with_flags(format!("{home}/.local/share/zest-dev/data/games_76561198054814426.db"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let mut st = db.prepare("SELECT appid, unlocked_achievements, total_achievements FROM games WHERE platform = 'steam' AND total_achievements > 0").unwrap();
        let rows: Vec<(i64, i64, i64)> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().flatten().collect();
        let (mut same, mut diff, mut none) = (0, 0, 0);
        for (a, u, t) in &rows { match super::local_counts(&root, 94548698, *a) { Some(c) if c == (*u, *t) => same += 1, Some(_) => diff += 1, None => none += 1 } }
        eprintln!("same {same}, different {diff}, no file {none} of {}", rows.len());
        assert!(same as f64 / (same + diff) as f64 > 0.98);
    }
}
