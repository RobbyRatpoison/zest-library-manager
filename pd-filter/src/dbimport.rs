//! Import Data from Another Database (`imports.py`: `inspect_database`, `execute_import`): copy chosen columns of any table of another SQLite file into the open library's
//! `games` rows, matching games by AppID. Only games present in both are touched.
use rusqlite::{types::Value as Sql, Connection, OpenFlags};
use std::path::Path;

pub struct Inspect {
    pub tables: Vec<String>,
    /// For each table (same order) its columns as (name, declared type, upper case).
    pub columns: Vec<Vec<(String, String)>>,
    /// The library's own `games` columns, with their types.
    pub local: Vec<(String, String)>,
}
#[derive(Debug)]
pub struct Outcome { pub updated: usize, pub skipped: usize, pub warnings: Vec<String> }

fn table_info(c: &Connection, table: &str) -> Result<Vec<(String, String)>, String> {
    let mut st = c.prepare(&format!("PRAGMA table_info(\"{}\")", table.replace('"', "\"\""))).map_err(|e| e.to_string())?;
    let v = st.query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, String>(2)?.to_uppercase()))).map_err(|e| e.to_string())?.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
    Ok(v)
}

/// Step 1: the other file's tables and columns, and the library's.
pub fn inspect(ext: &Path, local: &Connection) -> Result<Inspect, String> {
    if !ext.is_file() { return Err("File not found.".into()); }
    let e = Connection::open_with_flags(ext, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|_| "Could not read that database.".to_string())?;
    let tables: Vec<String> = e.prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name").and_then(|mut s| s.query_map([], |r| r.get(0))?.collect()).map_err(|_| "Could not read that database.".to_string())?;
    if tables.is_empty() { return Err("No tables found in this database.".into()); }
    let columns = tables.iter().map(|t| table_info(&e, t)).collect::<Result<Vec<_>, _>>()?;
    Ok(Inspect { tables, columns, local: table_info(local, "games")? })
}

/// `_types_compatible`: only a numeric/text mismatch is worth a warning.
pub fn type_warning(src: &str, tgt: &str) -> Option<String> {
    let bucket = |t: &str| { let t = t.to_uppercase(); if ["INT", "NUM", "REAL", "FLOAT"].iter().any(|x| t.contains(x)) { 'n' } else { 't' } };
    (bucket(src) != bucket(tgt)).then(|| format!("Type mismatch: source is {src}, target is {tgt}. Values will be imported as-is; numeric columns may receive text data."))
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468; let era = if z >= 0 { z } else { z - 146096 } / 146097; let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; let y = yoe + era * 400; let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153; let d = doy - (153 * mp + 2) / 5 + 1; let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}
const MONTHS: [&str; 12] = ["january", "february", "march", "april", "may", "june", "july", "august", "september", "october", "november", "december"];

/// A small `strptime`: %d %m %Y %b %B and literal characters, one or two digits for day and month, the whole text must match.
fn parse(fmt: &str, s: &str) -> Option<(i64, i64, i64)> {
    let (f, t): (Vec<char>, Vec<char>) = (fmt.chars().collect(), s.chars().collect());
    let (mut i, mut j) = (0, 0);
    let (mut y, mut m, mut d) = (None, None, None);
    let num = |t: &[char], j: &mut usize, min: usize, max: usize| -> Option<i64> { let st = *j; while *j < t.len() && *j - st < max && t[*j].is_ascii_digit() { *j += 1; } (*j - st >= min).then(|| t[st..*j].iter().collect::<String>().parse().ok()).flatten() };
    while i < f.len() {
        if f[i] == '%' && i + 1 < f.len() {
            match f[i + 1] {
                'd' => d = Some(num(&t, &mut j, 1, 2)?), 'm' => m = Some(num(&t, &mut j, 1, 2)?),
                'Y' => y = Some(num(&t, &mut j, 4, 4)?),
                'b' | 'B' => {
                    let rest: String = t[j..].iter().collect::<String>().to_lowercase();
                    let full = f[i + 1] == 'B';
                    let (k, len) = MONTHS.iter().enumerate().find_map(|(k, n)| { let w = if full { *n } else { &n[..3] }; rest.starts_with(w).then(|| (k, w.len())) })?;
                    m = Some(k as i64 + 1); j += len;
                }
                _ => return None,
            }
            i += 2;
        } else { if t.get(j) != Some(&f[i]) { return None; } i += 1; j += 1; }
    }
    if j != t.len() { return None; }
    let (y, m, d) = (y?, m?, d?);
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let dim = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31].get((m - 1) as usize).copied()?;
    (y >= 1 && (1..=dim).contains(&d)).then_some((y, m, d))
}

/// `normalize_date`: text in many common layouts (or a Unix timestamp) to YYYY-MM-DD; anything unrecognised comes back unchanged.
pub fn normalize_date(value: &str) -> String {
    let v = value.trim();
    if v.is_empty() { return value.to_string(); }
    let b = v.as_bytes();
    if b.len() == 10 && b[4] == b'-' && b[7] == b'-' && v.chars().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit()) { return v.to_string(); }
    if (9..=11).contains(&v.len()) && v.bytes().all(|c| c.is_ascii_digit()) { if let Ok(ts) = v.parse::<i64>() { let (y, m, d) = civil_from_days(ts.div_euclid(86400)); return format!("{y:04}-{m:02}-{d:02}"); } }
    for f in ["%d %b %Y", "%b %d, %Y", "%B %d, %Y", "%d/%m/%Y", "%m/%d/%Y", "%d-%m-%Y", "%Y/%m/%d", "%d.%m.%Y", "%Y%m%d"] {
        if let Some((y, m, d)) = parse(f, v) { return format!("{y:04}-{m:02}-{d:02}"); }
    }
    value.to_string()
}

fn text_of(v: &Sql) -> Option<String> {
    match v { Sql::Null => None, Sql::Integer(i) => Some(i.to_string()), Sql::Real(f) => Some(f.to_string()), Sql::Text(s) => { let t = s.trim(); (!t.is_empty()).then(|| s.clone()) } Sql::Blob(_) => None }
}

/// Step 2: copy the mapped columns. `maps` are (source column, library column). Unlike PlayDate the library column must be a real `games` column other than `appid`
/// (PlayDate pasted the name into the SQL as given).
pub fn execute(ext: &Path, local: &mut Connection, table: &str, appid_col: &str, maps: &[(String, String)], normalize_dates: bool) -> Result<Outcome, String> {
    if table.is_empty() || maps.is_empty() { return Err("Table and at least one column mapping are required.".into()); }
    let e = Connection::open_with_flags(ext, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|_| "Could not read that database.".to_string())?;
    let tables: Vec<String> = e.prepare("SELECT name FROM sqlite_master WHERE type='table'").and_then(|mut s| s.query_map([], |r| r.get(0))?.collect()).map_err(|e| e.to_string())?;
    if !tables.iter().any(|t| t == table) { return Err("Table not found in the chosen database.".into()); }
    let src = table_info(&e, table)?; let tgt = table_info(local, "games")?;
    let src_has = |c: &str| src.iter().any(|(n, _)| n == c);
    let mut missing: Vec<&str> = std::iter::once(appid_col).chain(maps.iter().map(|m| m.0.as_str())).filter(|c| !src_has(c)).collect();
    missing.dedup();
    if !missing.is_empty() { return Err(format!("Column(s) not found in table: {}", missing.join(", "))); }
    for (_, t) in maps { if t == "appid" || !tgt.iter().any(|(n, _)| n == t) { return Err(format!("{t} is not a column that can be filled in the library.")); } }
    let mut warnings = vec![];
    for (s, t) in maps {
        let st = src.iter().find(|(n, _)| n == s).map_or("TEXT", |x| x.1.as_str()); let tt = tgt.iter().find(|(n, _)| n == t).map_or("TEXT", |x| x.1.as_str());
        if let Some(w) = type_warning(st, tt) { warnings.push(format!("{s} -> {t}: {w}")); }
    }
    let mut cols: Vec<&str> = vec![appid_col]; for (s, _) in maps { if s != appid_col { cols.push(s); } }
    let q = |c: &str| format!("\"{}\"", c.replace('"', "\"\""));
    let sql = format!("SELECT {} FROM {}", cols.iter().map(|c| q(c)).collect::<Vec<_>>().join(", "), q(table));
    let mut st = e.prepare(&sql).map_err(|e| e.to_string())?;
    let rows: Vec<Vec<Sql>> = st.query_map([], |r| (0..cols.len()).map(|i| r.get::<_, Sql>(i)).collect::<Result<Vec<_>, _>>()).map_err(|e| e.to_string())?.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
    let (mut updated, mut skipped) = (0, 0);
    let tx = local.transaction().map_err(|e| e.to_string())?;
    for row in rows {
        let appid = &row[0];
        if matches!(appid, Sql::Null) || matches!(appid, Sql::Integer(0)) || text_of(appid).is_none() { skipped += 1; continue; }
        let mut set: Vec<(&str, Sql)> = vec![];
        for (s, t) in maps {
            let idx = cols.iter().position(|c| c == s).unwrap();
            let Some(txt) = text_of(&row[idx]) else { continue };
            let mut val = row[idx].clone();
            if normalize_dates {
                let n = normalize_date(&txt);
                if tgt.iter().find(|(c, _)| c == t).map_or(false, |x| x.1.contains("INT")) {
                    match crate::date_to_ts(&serde_json::Value::String(n)) { Some(ts) => val = Sql::Integer(ts), None => continue }
                } else { val = Sql::Text(n); }
            }
            set.push((t, val));
        }
        if set.is_empty() { skipped += 1; continue; }
        let clause = set.iter().map(|(k, _)| format!("{} = ?", q(k))).collect::<Vec<_>>().join(", ");
        let mut params: Vec<Sql> = set.into_iter().map(|(_, v)| v).collect(); params.push(appid.clone());
        let n = tx.execute(&format!("UPDATE games SET {clause} WHERE appid = ?"), rusqlite::params_from_iter(params.iter())).map_err(|e| e.to_string())?;
        if n > 0 { updated += 1; } else { skipped += 1; }
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(Outcome { updated, skipped, warnings })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn dates_are_normalised_like_playdates_list_of_formats() {
        for (i, o) in [("2020-01-15", "2020-01-15"), ("15 Jan 2020", "2020-01-15"), ("Jan 15, 2020", "2020-01-15"), ("January 15, 2020", "2020-01-15"), ("15/01/2020", "2020-01-15"), ("01/15/2020", "2020-01-15"),
            ("05/06/2020", "2020-06-05"), ("15-01-2020", "2020-01-15"), ("2020/01/15", "2020-01-15"), ("15.01.2020", "2020-01-15"), ("20200115", "2020-01-15"), ("1579046400", "2020-01-15"), ("whenever", "whenever"), ("31/02/2020", "31/02/2020")] {
            assert_eq!(normalize_date(i), o, "{i}");
        }
    }
    fn tmp(tag: &str) -> std::path::PathBuf { let d = std::env::temp_dir().join(format!("zest_dbi_{tag}_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d }
    #[test] fn columns_are_copied_for_games_in_both_and_a_bad_target_is_refused() {
        let d = tmp("x"); let lp = d.join("l.db"); crate::schema::init_db(&lp).unwrap();
        let mut l = Connection::open(&lp).unwrap();
        l.execute("INSERT INTO games (appid, name, completion_status) VALUES (1, 'One', 'Never Played'), (2, 'Two', 'Never Played')", []).unwrap();
        let ep = d.join("e.db"); let e = Connection::open(&ep).unwrap();
        e.execute_batch("CREATE TABLE g (id INTEGER, st TEXT, added TEXT, junk TEXT); INSERT INTO g VALUES (1, 'Beaten', '15 Jan 2020', NULL), (3, 'Completed', '', NULL), (0, 'x', '', NULL), (2, '', '', '');").unwrap();
        drop(e);
        let ins = inspect(&ep, &l).unwrap(); assert_eq!(ins.tables, ["g"]); assert!(ins.local.iter().any(|(n, _)| n == "completion_status"));
        let maps = vec![("st".to_string(), "completion_status".to_string()), ("added".to_string(), "date_added".to_string())];
        let o = execute(&ep, &mut l, "g", "id", &maps, true).unwrap();
        assert_eq!((o.updated, o.skipped), (1, 3));
        let (s, a): (String, i64) = l.query_row("SELECT completion_status, date_added FROM games WHERE appid = 1", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((s.as_str(), a), ("Beaten", 1579046400));
        assert_eq!(l.query_row::<String, _, _>("SELECT completion_status FROM games WHERE appid = 2", [], |r| r.get(0)).unwrap(), "Never Played");
        assert!(execute(&ep, &mut l, "g", "id", &[("st".into(), "appid".into())], false).is_err());
        assert!(execute(&ep, &mut l, "g", "id", &[("st".into(), "name = 'x'; --".into())], false).is_err());
        assert!(execute(&ep, &mut l, "g", "nope", &maps, false).unwrap_err().contains("not found"));
        assert!(type_warning("TEXT", "INTEGER").is_some() && type_warning("INT", "REAL").is_none());
    }
}
