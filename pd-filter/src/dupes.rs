//! Duplicate copies of one game across libraries (a port of `database.auto_detect_duplicates` and its helpers, redesigned for Zest).
//!
//! What differs from the Python on purpose (decisions in NATIVE_UI_PLAN.md section 12):
//! * detection is **incremental**: links persist and only games not yet examined (`dup_checked IS NULL`) are matched; nothing is cleared and
//!   recomputed, so a pair the user unlinked (`duplicate_auto = 2`) can never come back;
//! * automatic links need an **exact** match of the normalised name (lowercase, punctuation to spaces); a match that needs the edition suffix
//!   stripped ("Game - Remastered" vs "Game") is only ever a *suggestion*;
//! * a rename never creates a link (it may drop one that no longer matches); same-platform siblings are each linked; a name that normalises
//!   to nothing is never matched; platforms outside the saved order rank after the listed ones (two of them tie, so nothing is linked between them).
//! Automatic steps only ever err toward showing a game.
use regex::Regex;
use rusqlite::{params, Connection, Result};
use std::collections::HashMap;
use std::sync::OnceLock;

fn re_edition() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)\s*[-\u{2013}:]\s*(goty|game of the year|complete edition|deluxe edition|gold edition|definitive edition|remastered|remaster|enhanced edition|anniversary edition|director'?s cut|ultimate edition|premium edition)\s*$").unwrap())
}
fn re_punct() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"[^\p{L}\p{N}_\s']").unwrap()) }
fn re_space() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\s+").unwrap()) }

fn squash(s: &str) -> String {
    let s = re_punct().replace_all(s, " ");
    re_space().replace_all(&s, " ").trim().to_string()
}
/// The name as automatic matching sees it: lowercase, punctuation to spaces, spaces collapsed. No edition suffix is stripped.
pub fn norm_exact(name: &str) -> String { squash(&name.to_lowercase()) }
/// `database._normalize_name_for_dup`: like `norm_exact` but a trailing "- GOTY" / "- Remastered" / "- Deluxe Edition"... is removed first.
pub fn norm_edition(name: &str) -> String { squash(&re_edition().replace(&name.to_lowercase(), "")) }

/// Add the `dup_checked` column. The first time (column missing) every existing game counts as examined: Python already ran its detection
/// on that library and its links are kept as they are. Returns true when the column was added.
pub fn ensure_column(conn: &Connection) -> Result<bool> {
    let has = conn.prepare("PRAGMA table_info(games)")?.query_map([], |r| r.get::<_, String>(1))?.flatten().any(|c| c == "dup_checked");
    if has { return Ok(false); }
    conn.execute("ALTER TABLE games ADD COLUMN dup_checked INTEGER", [])?;
    conn.execute("UPDATE games SET dup_checked = 1", [])?;
    Ok(true)
}

#[derive(Clone, Debug)]
struct Row { appid: i64, name: String, platform: String, dup: Option<i64>, auto: i64, checked: bool }

fn rank(priority: &[String], plat: &str) -> usize { priority.iter().position(|p| p == plat).unwrap_or(priority.len()) }

fn load(conn: &Connection) -> Result<Vec<Row>> {
    let mut st = conn.prepare("SELECT appid, COALESCE(name, ''), COALESCE(platform, 'steam'), duplicate_of, COALESCE(duplicate_auto, 0), dup_checked FROM games ORDER BY appid")?;
    let rows = st.query_map([], |r| {
        let dup: Option<String> = r.get::<_, Option<String>>(3).or_else(|_| r.get::<_, Option<i64>>(3).map(|v| v.map(|n| n.to_string())))?;
        Ok(Row { appid: r.get(0)?, name: r.get(1)?, platform: r.get(2)?, dup: dup.and_then(|d| d.trim().parse().ok()), auto: r.get(4)?, checked: r.get::<_, Option<i64>>(5)?.is_some() })
    })?.flatten().collect();
    Ok(rows)
}

fn set_link(conn: &Connection, appid: i64, to: Option<i64>, auto: i64) -> Result<()> {
    conn.execute("UPDATE games SET duplicate_of = ?, duplicate_auto = ? WHERE appid = ?", params![to.map(|t| t.to_string()), auto, appid])?;
    Ok(())
}

/// What `examine_new` did: links it made and the games it looked at.
#[derive(Debug, Default, PartialEq)]
pub struct Detected { pub linked: usize, pub examined: usize }

/// Match every game that has not been examined yet against the rest of the library, then mark it examined.
pub fn examine_new(conn: &Connection, priority: &[String]) -> Result<Detected> {
    let mut rows = load(conn)?;
    let mut by_norm: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, r) in rows.iter().enumerate() { let n = norm_exact(&r.name); if !n.is_empty() { by_norm.entry(n).or_default().push(i); } }
    let index: HashMap<i64, usize> = rows.iter().enumerate().map(|(i, r)| (r.appid, i)).collect();
    let mut out = Detected::default();
    let todo: Vec<usize> = (0..rows.len()).filter(|i| !rows[*i].checked).collect();
    let tx = conn.unchecked_transaction()?;
    for ni in todo {
        out.examined += 1;
        rows[ni].checked = true;
        tx.execute("UPDATE games SET dup_checked = 1 WHERE appid = ?", [rows[ni].appid])?;
        let n = rows[ni].clone();
        let norm = norm_exact(&n.name);
        if norm.is_empty() || n.auto == 2 || n.dup.is_some() { continue; }
        // the copies of this game other than N: the shown copy of each candidate on another platform
        let mut tops: Vec<usize> = Vec::new();
        for &ci in by_norm.get(&norm).map(|v| v.as_slice()).unwrap_or(&[]) {
            if ci == ni { continue; }
            let top = match rows[ci].dup.and_then(|d| index.get(&d).copied()) { Some(t) => t, None => ci };
            if rows[top].platform != n.platform && !tops.contains(&top) { tops.push(top); }
        }
        if tops.is_empty() { continue; }
        // the copy that should be shown: the best-ranked platform; equal ranks (two unlisted platforms) are ambiguous, so nothing is linked
        let best_rank = tops.iter().map(|t| rank(priority, &rows[*t].platform)).min().unwrap();
        let best: Vec<usize> = tops.iter().copied().filter(|t| rank(priority, &rows[*t].platform) == best_rank).collect();
        let my = rank(priority, &n.platform);
        if best.len() != 1 || my == best_rank { continue; }
        let b = best[0];
        if my > best_rank {
            set_link(&tx, n.appid, Some(rows[b].appid), 1)?;
            rows[ni].dup = Some(rows[b].appid); rows[ni].auto = 1;
            out.linked += 1;
        } else {
            // N outranks every copy: it becomes the shown copy and the group's links move to it
            let hide: Vec<usize> = tops.iter().copied().filter(|t| rows[*t].auto != 2).collect();   // a copy the user marked "not a duplicate" stays shown
            for t in hide {
                let tid = rows[t].appid;
                set_link(&tx, tid, Some(n.appid), 1)?;
                rows[t].dup = Some(n.appid); rows[t].auto = 1;
                for j in 0..rows.len() {
                    if rows[j].dup == Some(tid) && j != ni { let a = rows[j].auto; set_link(&tx, rows[j].appid, Some(n.appid), a)?; rows[j].dup = Some(n.appid); }
                }
                out.linked += 1;
            }
        }
    }
    tx.commit()?;
    Ok(out)
}

/// Re-point existing links after a priority change or a hand link: every connected group of linked copies shows its best-ranked copy (on a tie
/// the one that is already shown), everything else points straight at it, loops (A -> B -> A) are resolved the same way, and a link to a game
/// that no longer exists is dropped. Creates no link between unconnected games. Port of `_align_manual_duplicate_links` +
/// `_flatten_duplicate_links`, generalised to automatic links and to groups where several copies outrank the target.
pub fn realign(conn: &Connection, priority: &[String]) -> Result<usize> {
    let rows = load(conn)?;
    let n = rows.len();
    let index: HashMap<i64, usize> = rows.iter().enumerate().map(|(i, r)| (r.appid, i)).collect();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut Vec<usize>, mut x: usize) -> usize { while p[x] != x { p[x] = p[p[x]]; x = p[x]; } x }
    let tx = conn.unchecked_transaction()?;
    let mut changed = 0;
    for i in 0..n {
        let Some(d) = rows[i].dup else { continue };
        match index.get(&d) {
            Some(&j) => { let (a, b) = (find(&mut parent, i), find(&mut parent, j)); if a != b { parent[a] = b; } }
            None => { set_link(&tx, rows[i].appid, None, 0)?; changed += 1; }   // the target is gone: show this copy again
        }
    }
    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..n { let r = find(&mut parent, i); groups.entry(r).or_default().push(i); }
    for (_, members) in groups {
        if members.len() < 2 { continue; }
        let top = members.iter().copied().min_by_key(|i| (rank(priority, &rows[*i].platform), if rows[*i].dup.is_none() { 0 } else { 1 }, rows[*i].appid)).unwrap();
        let tid = rows[top].appid;
        for &m in members.iter().filter(|m| **m != top) {
            if rows[m].dup != Some(tid) {
                // a copy that was shown and now hides keeps the kind of link the group had (automatic stays automatic)
                let auto = if rows[m].dup.is_some() { rows[m].auto } else if rows[top].dup.is_some() { rows[top].auto } else { 0 };
                set_link(&tx, rows[m].appid, Some(tid), auto)?; changed += 1;
            }
        }
        if rows[top].dup.is_some() { set_link(&tx, tid, None, 0)?; changed += 1; }
    }
    tx.commit()?;
    Ok(changed)
}

/// A link made by hand in the Edit dialog (`duplicate_auto = 0`); the platform order still decides which copy is shown.
pub fn link_manual(conn: &Connection, appid: i64, target: i64, priority: &[String]) -> Result<()> {
    set_link(conn, appid, Some(target), 0)?;
    realign(conn, priority)?;
    Ok(())
}

/// Unlink: the game becomes its own shown copy and is marked `duplicate_auto = 2` so detection never links it again.
pub fn unlink(conn: &Connection, appid: i64) -> Result<()> { set_link(conn, appid, None, 2) }

/// A game was renamed. It is not re-examined (a rename never creates a link). A link that no longer matches is dropped, for the renamed game
/// and for hidden copies that pointed at it; links made by hand are left alone.
pub fn on_rename(conn: &Connection, appid: i64) -> Result<usize> {
    let rows = load(conn)?;
    let index: HashMap<i64, usize> = rows.iter().enumerate().map(|(i, r)| (r.appid, i)).collect();
    let Some(&me) = index.get(&appid) else { return Ok(0) };
    conn.execute("UPDATE games SET dup_checked = 1 WHERE appid = ?", [appid])?;
    let mut dropped = 0;
    let mine = norm_exact(&rows[me].name);
    for r in &rows {
        if r.auto != 1 { continue; }
        let involved = r.appid == appid || r.dup == Some(appid);
        if !involved { continue; }
        let Some(t) = r.dup.and_then(|d| index.get(&d)) else { continue };
        let (a, b) = if r.appid == appid { (mine.clone(), norm_exact(&rows[*t].name)) } else { (mine.clone(), norm_exact(&r.name)) };
        if a != b || a.is_empty() { set_link(conn, r.appid, None, 0)?; dropped += 1; }
    }
    Ok(dropped)
}

/// A game was deleted: the copies that pointed at it get a new shown copy (the best-ranked of them), a lone survivor becomes a normal game.
pub fn on_delete(conn: &Connection, gone: i64, priority: &[String]) -> Result<()> {
    // only the copies that pointed at it are touched (a bulk delete calls this for every game, so no full table read)
    let mut st = conn.prepare("SELECT appid, COALESCE(platform, 'steam'), COALESCE(duplicate_auto, 0) FROM games WHERE duplicate_of = ? ORDER BY appid")?;
    let orphans: Vec<(i64, String, i64)> = st.query_map([gone.to_string()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.flatten().collect();
    drop(st);
    if orphans.is_empty() { return Ok(()); }
    let top = orphans.iter().min_by_key(|o| (rank(priority, &o.1), o.0)).unwrap().0;
    set_link(conn, top, None, 0)?;
    for (a, _, auto) in orphans.iter().filter(|o| o.0 != top) { set_link(conn, *a, Some(top), *auto)?; }
    Ok(())
}

/// A possible duplicate that automatic matching will not act on: needs the edition suffix stripped to match.
#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion { pub appid: i64, pub name: String, pub platform: String, pub target: i64, pub target_name: String, pub target_platform: String }

/// Games with no link, not marked "not a duplicate", whose name matches a copy on another platform only after the edition suffix is stripped.
/// The suggested target is the best-ranked shown copy; the game suggested to hide is the lower-ranked of the two.
pub fn suggestions(conn: &Connection, priority: &[String]) -> Result<Vec<Suggestion>> {
    let rows = load(conn)?;
    let mut by_ed: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, r) in rows.iter().enumerate() { let n = norm_edition(&r.name); if !n.is_empty() { by_ed.entry(n).or_default().push(i); } }
    let mut out = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        if r.dup.is_some() || r.auto == 2 { continue; }
        let mine = norm_exact(&r.name);
        let Some(group) = by_ed.get(&norm_edition(&r.name)) else { continue };
        let mut best: Option<usize> = None;
        for &j in group {
            if j == i || rows[j].dup.is_some() || rows[j].platform == r.platform || norm_exact(&rows[j].name) == mine { continue; }
            if rank(priority, &rows[j].platform) < rank(priority, &r.platform) && best.map_or(true, |b| rank(priority, &rows[j].platform) < rank(priority, &rows[b].platform)) { best = Some(j); }
        }
        if let Some(b) = best { out.push(Suggestion { appid: r.appid, name: r.name.clone(), platform: r.platform.clone(), target: rows[b].appid, target_name: rows[b].name.clone(), target_platform: rows[b].platform.clone() }); }
    }
    Ok(out)
}

/// Approve suggestions (one or many at once): each becomes a hand link, `duplicate_auto = 0`.
pub fn approve(conn: &Connection, pairs: &[(i64, i64)], priority: &[String]) -> Result<()> {
    for (a, t) in pairs { set_link(conn, *a, Some(*t), 0)?; }
    realign(conn, priority)?;
    Ok(())
}

/// "Not a duplicate" for a suggestion: the game is marked so it is never suggested or linked again.
pub fn reject(conn: &Connection, appids: &[i64]) -> Result<()> { for a in appids { conn.execute("UPDATE games SET duplicate_auto = 2 WHERE appid = ? AND (duplicate_of IS NULL OR duplicate_of = '')", [a])?; } Ok(()) }

/// How many games are hidden as a copy of another (for the Library header).
pub fn hidden_count(conn: &Connection) -> Result<i64> { conn.query_row("SELECT COUNT(*) FROM games WHERE duplicate_of IS NOT NULL AND duplicate_of != ''", [], |r| r.get(0)) }
