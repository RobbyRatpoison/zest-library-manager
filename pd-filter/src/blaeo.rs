//! BLAEO (backlog-assassins.net) sync: your BLAEO statuses and list names into Zest's completion statuses and groups. A port of the BLAEO part of
//! PlayDate's `scrapers.py`, reshaped: it is only ever a preview you apply by choice (never part of populate), the preview lives in the dialog (no hidden
//! cache), and achievement counts are not taken from BLAEO (the startup sync reads them from Steam's own files).
//!
//! Who owns a group on a game is kept in `group_sources_<account>.json` (the file PlayDate uses): a group a BLAEO list added is removed when the list
//! drops the game, unless something else (a Steam collection, you by hand) also put it there.
use crate::net::Http;
use regex::Regex;
use rusqlite::Connection;
use scraper::{Html, Selector};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::LazyLock;
use std::time::Duration;

pub const STATUSES: [&str; 5] = ["Never Played", "Unfinished", "Beaten", "Completed", "Won't Play"];

/// One game row on BLAEO.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub appid: i64,
    /// Zest's status for BLAEO's class (`game-wont-play` -> "Won't Play"); anything BLAEO has that Zest does not (uncategorized) is "Unknown" and changes nothing.
    pub status: String,
    /// The lists the game is on, in page order: (id from the list's address, name).
    pub lists: Vec<(String, String)>,
    /// The paging cursor this row carries.
    pub cursor: Option<String>,
}

fn clean_status(class_suffix: &str) -> String {
    match class_suffix { "never-played" => "Never Played", "wont-play" => "Won't Play", "unfinished" => "Unfinished", "beaten" => "Beaten", "completed" => "Completed", _ => "Unknown" }.to_string()
}

/// Rows of one page; `None` when the page has no games table at all (not a BLAEO account page).
pub fn parse_page(html: &str) -> Option<Vec<Row>> {
    static TABLE: LazyLock<Selector> = LazyLock::new(|| Selector::parse("table.game-table").unwrap());
    static ROWS: LazyLock<Selector> = LazyLock::new(|| Selector::parse("table.game-table tbody tr.game").unwrap());
    static STEAM: LazyLock<Selector> = LazyLock::new(|| Selector::parse("a.steam").unwrap());
    static TAGS: LazyLock<Selector> = LazyLock::new(|| Selector::parse("a.list-tag").unwrap());
    static APP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"/app/(\d+)").unwrap());
    static LIST: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"/lists/(\w+)").unwrap());
    let doc = Html::parse_document(html);
    doc.select(&TABLE).next()?;
    let mut out = Vec::new();
    for tr in doc.select(&ROWS) {
        let cursor = tr.value().attr("data-item").map(String::from);
        let Some(appid) = tr.select(&STEAM).next().and_then(|a| a.value().attr("href")).and_then(|h| APP.captures(h)).and_then(|m| m[1].parse::<i64>().ok()) else { continue };
        let mut status = "Unknown".to_string();
        for c in tr.value().classes() { if let Some(s) = c.strip_prefix("game-") { status = clean_status(s); } }
        let lists = tr.select(&TAGS).map(|t| {
            let id = t.value().attr("href").and_then(|h| LIST.captures(h)).map(|m| m[1].to_string()).unwrap_or_default();
            (id, t.text().collect::<String>().trim().to_string())
        }).collect();
        out.push(Row { appid, status, lists, cursor });
    }
    Some(out)
}

pub fn profile_url(steam_id: &str) -> String { format!("https://www.backlog-assassins.net/users/+{steam_id}/games") }

/// Every game row of the profile: pages follow each other by `?start_at=<last row's cursor>` until one comes back empty. `pause` is the courtesy wait between pages.
pub fn fetch_all(http: &dyn Http, base_url: &str, pause: Duration) -> Result<Vec<Row>, String> {
    let base = base_url.trim_end_matches('/');
    let ua = [("User-Agent", "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36")];
    let mut all: Vec<Row> = Vec::new();
    let mut url = base.to_string();
    for page in 1.. {
        let r = http.get(&url, &ua, Duration::from_secs(15)).map_err(|e| format!("BLAEO: {e}"))?;
        if r.status == 404 { return Err("BLAEO profile not found. You may not have a BLAEO account.".into()); }
        if r.status != 200 { return Err(format!("BLAEO may be down (HTTP {}).", r.status)); }
        let rows = match parse_page(&r.body) { Some(r) => r, None if page == 1 => return Err("No BLAEO game list found. You may not have a BLAEO account.".into()), None => break };
        if rows.is_empty() { break; }
        let next = rows.last().and_then(|r| r.cursor.clone());
        all.extend(rows);
        let Some(c) = next else { break };
        url = format!("{base}?start_at={c}");
        std::thread::sleep(pause);
    }
    Ok(all)
}

// ───────────────────────── who owns which group ─────────────────────────

/// `group_sources_<account>.json`: `sources` (`blaeo:<list id>` -> {type, name, members}) and `assignments` (appid -> group -> [owner source ids, "manual"]).
#[derive(Debug, Clone, PartialEq)]
pub struct GroupSources(pub Value);

impl GroupSources {
    pub fn empty() -> Self { GroupSources(json!({"version": 1, "sources": {}, "assignments": {}})) }
    pub fn load(path: &Path) -> Self { std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).map(GroupSources).unwrap_or_else(Self::empty) }
    pub fn save(&self, path: &Path) -> std::io::Result<()> { crate::account::write_json(path, &self.0) }
    fn assignments(&mut self) -> &mut Map<String, Value> {
        if !self.0.is_object() { self.0 = json!({}); }
        let o = self.0.as_object_mut().unwrap();
        if !o.get("assignments").map_or(false, |a| a.is_object()) { o.insert("assignments".into(), json!({})); }
        o.get_mut("assignments").unwrap().as_object_mut().unwrap()
    }
    /// Does a source other than `excluding` own this group on this game?
    pub fn is_protected(&self, appid: i64, group: &str, excluding: &str) -> bool {
        self.0["assignments"][appid.to_string()][group].as_array().map_or(false, |o| o.iter().any(|s| s.as_str() != Some(excluding)))
    }
    pub fn add_owner(&mut self, appid: i64, group: &str, source: &str) {
        let g = self.assignments().entry(appid.to_string()).or_insert_with(|| json!({}));
        if !g.is_object() { *g = json!({}); }
        let owners = g.as_object_mut().unwrap().entry(group.to_string()).or_insert_with(|| json!([]));
        if let Some(a) = owners.as_array_mut() { if !a.iter().any(|s| s.as_str() == Some(source)) { a.push(json!(source)); } }
    }
    /// `gs_remove_owner`: drop one owner of a group on a game, tidying up empty entries.
    pub fn remove_owner(&mut self, appid: i64, group: &str, source: &str) {
        let key = appid.to_string();
        let Some(game) = self.assignments().get_mut(&key).and_then(|g| g.as_object_mut()) else { return };
        if let Some(a) = game.get_mut(group).and_then(|v| v.as_array_mut()) { a.retain(|s| s.as_str() != Some(source)); }
        if game.get(group).and_then(|v| v.as_array()).map_or(false, |a| a.is_empty()) { game.remove(group); }
        if game.is_empty() { self.assignments().remove(&key); }
    }
    /// The BLAEO lists seen at the last apply: id -> name, and (appid, list id) pairs.
    pub fn stored_lists(&self) -> BTreeMap<String, String> {
        self.0["sources"].as_object().into_iter().flatten().filter(|(_, s)| s["type"] == "blaeo")
            .filter_map(|(k, s)| Some((k.strip_prefix("blaeo:")?.to_string(), s["name"].as_str()?.to_string()))).collect()
    }
    pub fn stored_members(&self) -> BTreeSet<(i64, String)> {
        let mut out = BTreeSet::new();
        for (k, s) in self.0["sources"].as_object().into_iter().flatten().filter(|(_, s)| s["type"] == "blaeo") {
            let Some(id) = k.strip_prefix("blaeo:") else { continue };
            for a in s["members"].as_array().into_iter().flatten().filter_map(|a| a.as_i64()) { out.insert((a, id.to_string())); }
        }
        out
    }
    /// `_rebuild_blaeo_sources`: forget every BLAEO ownership, record the lists and members as they are now, and mark any group on a game that nothing owns as manual.
    pub fn rebuild_blaeo(&mut self, lists: &BTreeMap<String, String>, members: &BTreeSet<(i64, String)>, db_groups: &[(i64, Vec<String>)]) {
        let old: Vec<String> = self.0["sources"].as_object().into_iter().flatten().filter(|(k, _)| k.starts_with("blaeo:")).map(|(k, _)| k.clone()).collect();
        for sid in &old {
            for game in self.assignments().values_mut().filter_map(|g| g.as_object_mut()) {
                let groups: Vec<String> = game.keys().cloned().collect();
                for g in groups {
                    if let Some(a) = game.get_mut(&g).and_then(|v| v.as_array_mut()) { a.retain(|s| s.as_str() != Some(sid)); }
                    if game.get(&g).and_then(|v| v.as_array()).map_or(false, |a| a.is_empty()) { game.remove(&g); }
                }
            }
            if let Some(s) = self.0["sources"].as_object_mut() { s.remove(sid); }
        }
        self.assignments().retain(|_, v| v.as_object().map_or(false, |m| !m.is_empty()));
        if !self.0["sources"].is_object() { self.0["sources"] = json!({}); }
        for (id, name) in lists {
            let sid = format!("blaeo:{id}");
            let mem: Vec<i64> = members.iter().filter(|(_, l)| l == id).map(|(a, _)| *a).collect::<BTreeSet<_>>().into_iter().collect();
            self.0["sources"][&sid] = json!({"type": "blaeo", "name": name, "members": mem});
            for a in &mem { self.add_owner(*a, name, &sid); }
        }
        for (appid, groups) in db_groups {
            for g in groups { if self.0["assignments"][appid.to_string()].get(g).is_none() { self.add_owner(*appid, g, "manual"); } }
        }
    }
}

// ───────────────────────── what would change ─────────────────────────

/// A game as the library has it.
#[derive(Debug, Clone, PartialEq)]
pub struct Local { pub name: String, pub groups: Vec<String>, pub status: String }

pub fn split_groups(csv: &str) -> Vec<String> { csv.split(',').map(str::trim).filter(|g| !g.is_empty()).map(String::from).collect() }

/// Games the library has among these appids.
pub fn load_local(conn: &Connection, appids: &[i64]) -> HashMap<i64, Local> {
    let mut out = HashMap::new();
    let Ok(mut st) = conn.prepare("SELECT name, `groups`, completion_status FROM games WHERE appid = ?") else { return out };
    for a in appids {
        if let Ok(l) = st.query_row([a], |r| Ok(Local { name: r.get::<_, Option<String>>(0)?.unwrap_or_default(), groups: split_groups(&r.get::<_, Option<String>>(1)?.unwrap_or_default()), status: r.get::<_, Option<String>>(2)?.filter(|s| !s.is_empty()).unwrap_or_else(|| "Never Played".into()) })) { out.insert(*a, l); }
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
pub struct StatusChange { pub appid: i64, pub name: String, pub from: String, pub to: String, /// Moves the game back (Completed -> Beaten, Beaten -> Never Played ...): offered unticked.
    pub lowers: bool }
#[derive(Debug, Clone, PartialEq)]
pub struct Addition { pub appid: i64, pub name: String, pub lists: Vec<String> }
#[derive(Debug, Clone, PartialEq)]
pub struct Removal { pub appid: i64, pub name: String, pub list_name: String, pub list_id: String }
#[derive(Debug, Clone, PartialEq)]
pub struct Rename { pub id: String, pub from: String, pub to: String }

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Plan {
    pub status_changes: Vec<StatusChange>, pub additions: Vec<Addition>, pub removals: Vec<Removal>, pub renames: Vec<Rename>,
    /// What BLAEO shows now: the lists, and which game is on which.
    pub lists: BTreeMap<String, String>, pub members: BTreeSet<(i64, String)>, pub rows: Vec<Row>,
}
impl Plan { pub fn is_empty(&self) -> bool { self.status_changes.is_empty() && self.additions.is_empty() && self.removals.is_empty() && self.renames.is_empty() } }

fn rank(s: &str) -> Option<i32> { match s { "Never Played" => Some(0), "Unfinished" => Some(1), "Beaten" => Some(2), "Completed" => Some(3), _ => None } }
/// A change from a higher to a lower status (Won't Play has no rank: moving to or from it is a decision, not a step back).
pub fn lowers(from: &str, to: &str) -> bool { matches!((rank(from), rank(to)), (Some(a), Some(b)) if b < a) }

/// `_blaeo_scrape_and_compute`'s comparison, without touching anything. `gs` is what the last apply recorded.
pub fn plan(rows: Vec<Row>, local: &HashMap<i64, Local>, gs: &GroupSources) -> Plan {
    let mut lists: BTreeMap<String, String> = BTreeMap::new();
    let mut members: BTreeSet<(i64, String)> = BTreeSet::new();
    for r in &rows { for (id, name) in &r.lists { if !id.is_empty() { lists.insert(id.clone(), name.clone()); members.insert((r.appid, id.clone())); } } }
    let stored = gs.stored_lists();
    let renames: Vec<Rename> = lists.iter().filter_map(|(id, new)| stored.get(id).filter(|old| *old != new).map(|old| Rename { id: id.clone(), from: old.clone(), to: new.clone() })).collect();
    let rename_map: HashMap<&str, &str> = renames.iter().map(|r| (r.from.as_str(), r.to.as_str())).collect();
    let renamed = |g: &str| rename_map.get(g).map_or(g.to_string(), |t| t.to_string());

    let mut removals = Vec::new();
    for (appid, list_id) in gs.stored_members().difference(&members) {
        let Some(list_name) = stored.get(list_id).or_else(|| lists.get(list_id)) else { continue };
        let Some(l) = local.get(appid) else { continue };
        let existing: HashSet<String> = l.groups.iter().map(|g| renamed(g)).collect();
        let eff = renamed(list_name);
        if existing.contains(&eff) { removals.push(Removal { appid: *appid, name: l.name.clone(), list_name: eff, list_id: list_id.clone() }); }
    }
    removals.sort_by(|a, b| (a.appid, &a.list_id).cmp(&(b.appid, &b.list_id)));

    let (mut additions, mut status_changes) = (Vec::new(), Vec::new());
    for r in &rows {
        let Some(l) = local.get(&r.appid) else { continue };
        let existing: HashSet<String> = l.groups.iter().map(|g| renamed(g)).collect();
        let new: BTreeSet<String> = r.lists.iter().map(|(_, n)| n.clone()).filter(|n| !existing.contains(n)).collect();
        if !new.is_empty() { additions.push(Addition { appid: r.appid, name: l.name.clone(), lists: new.into_iter().collect() }); }
        let to = if STATUSES.contains(&r.status.as_str()) { r.status.clone() } else { l.status.clone() };
        if to != l.status { status_changes.push(StatusChange { appid: r.appid, name: l.name.clone(), from: l.status.clone(), lowers: lowers(&l.status, &to), to }); }
    }
    Plan { status_changes, additions, removals, renames, lists, members, rows }
}

/// What the user ticked.
#[derive(Debug, Clone, Default)]
pub struct Selection { pub status: HashSet<i64>, pub additions: HashSet<i64>, pub removals: HashSet<(i64, String)>, pub renames: HashSet<(String, String)> }
impl Selection {
    /// Everything proposed, except status changes that lower a status (the dialog's starting point).
    pub fn default_for(p: &Plan) -> Selection {
        Selection { status: p.status_changes.iter().filter(|c| !c.lowers).map(|c| c.appid).collect(), additions: p.additions.iter().map(|a| a.appid).collect(),
                    removals: p.removals.iter().map(|r| (r.appid, r.list_name.clone())).collect(), renames: p.renames.iter().map(|r| (r.from.clone(), r.to.clone())).collect() }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Applied { pub status: usize, pub additions: usize, pub removals: usize, pub renames: usize, /// Removals left alone because another source also owns the group.
    pub protected: usize }

fn write_groups(conn: &Connection, appid: i64, groups: &BTreeSet<String>) -> rusqlite::Result<()> {
    conn.execute("UPDATE games SET `groups` = ? WHERE appid = ?", rusqlite::params![groups.iter().cloned().collect::<Vec<_>>().join(","), appid]).map(|_| ())
}
fn groups_of(conn: &Connection, appid: i64) -> Option<BTreeSet<String>> {
    conn.query_row("SELECT `groups` FROM games WHERE appid = ?", [appid], |r| r.get::<_, Option<String>>(0)).ok().map(|g| split_groups(&g.unwrap_or_default()).into_iter().collect())
}

/// `apply_blaeo_changes`: carries out the ticked part of the plan, then records BLAEO's current lists and members in the ownership file's data `gs`
/// (the caller saves it). Renames go first (on the games that are on the list), then removals, then each game's status and additions.
pub fn apply(conn: &Connection, gs: &mut GroupSources, p: &Plan, sel: &Selection) -> rusqlite::Result<Applied> {
    let mut done = Applied::default();
    for r in p.renames.iter().filter(|r| sel.renames.contains(&(r.from.clone(), r.to.clone()))) {
        let source = format!("blaeo:{}", r.id);
        for (appid, lid) in &p.members {
            if *lid != r.id { continue; }
            let Some(mut g) = groups_of(conn, *appid) else { continue };
            if !g.contains(&r.from) { continue; }
            if !gs.is_protected(*appid, &r.from, &source) { g.remove(&r.from); }
            g.insert(r.to.clone());
            write_groups(conn, *appid, &g)?;
        }
        done.renames += 1;
    }
    for r in p.removals.iter().filter(|r| sel.removals.contains(&(r.appid, r.list_name.clone()))) {
        // the group may still carry the list's old name (a game that left the list is not renamed with its members; PlayDate then removed nothing)
        let source = format!("blaeo:{}", r.list_id);
        let mut names = vec![r.list_name.clone()];
        names.extend(p.renames.iter().filter(|x| x.id == r.list_id && x.to == r.list_name).map(|x| x.from.clone()));
        let Some(mut g) = groups_of(conn, r.appid) else { continue };
        let (mut removed, mut kept) = (false, false);
        for n in names.iter().filter(|n| g.contains(*n)).cloned().collect::<Vec<_>>() {
            if gs.is_protected(r.appid, &n, &source) { kept = true; } else { g.remove(&n); removed = true; }
        }
        if removed { write_groups(conn, r.appid, &g)?; done.removals += 1; }
        if kept { done.protected += 1; }
    }
    let local = load_local(conn, &p.rows.iter().map(|r| r.appid).collect::<Vec<_>>());
    for row in &p.rows {
        let Some(l) = local.get(&row.appid) else { continue };
        let Some(mut g) = groups_of(conn, row.appid) else { continue };
        let to = if STATUSES.contains(&row.status.as_str()) { row.status.clone() } else { l.status.clone() };
        let apply_status = sel.status.contains(&row.appid) && to != l.status;
        let names: BTreeSet<String> = row.lists.iter().map(|(_, n)| n.clone()).collect();
        let apply_add = sel.additions.contains(&row.appid) && names.iter().any(|n| !g.contains(n));
        if !apply_status && !apply_add { continue; }
        if apply_add { g.extend(names); }
        conn.execute("UPDATE games SET completion_status = ?, `groups` = ? WHERE appid = ?", rusqlite::params![if apply_status { &to } else { &l.status }, g.iter().cloned().collect::<Vec<_>>().join(","), row.appid])?;
        if apply_status { done.status += 1; }
        if apply_add { done.additions += 1; }
    }
    let mut st = conn.prepare("SELECT appid, `groups` FROM games WHERE `groups` IS NOT NULL AND `groups` != ''")?;
    let db_groups: Vec<(i64, Vec<String>)> = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, split_groups(&r.get::<_, String>(1)?)))).map(|it| it.flatten().collect())?;
    gs.rebuild_blaeo(&p.lists, &p.members, &db_groups);
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{NetError, Response};
    use std::sync::Mutex;

    const PAGE: &str = include_str!("../tests/blaeo_page.html");

    #[test] fn a_real_page_is_read_row_by_row() {
        let rows = parse_page(PAGE).unwrap();
        assert_eq!(rows.len(), 19);
        let by = |a: i64| rows.iter().find(|r| r.appid == a).unwrap();
        assert_eq!(by(729370).status, "Beaten");
        assert_eq!(by(525480).status, "Never Played");
        assert_eq!(by(410110).status, "Won't Play");
        assert_eq!(by(205690).status, "Unfinished");
        assert_eq!(by(227580).status, "Completed");
        assert_eq!(by(4126040).status, "Unknown", "uncategorized changes nothing");
        assert_eq!(by(717610).lists, vec![("6drzgzl".to_string(), "Won on SteamGifts".to_string())]);
        assert!(by(729370).lists.is_empty());
        assert_eq!(rows.last().unwrap().cursor.as_deref(), Some("a24kz45"));
        assert!(parse_page("<html><body>no table</body></html>").is_none());
    }

    struct Fake { pages: Mutex<Vec<(u16, String)>>, urls: Mutex<Vec<String>> }
    impl Http for Fake {
        fn get(&self, url: &str, _: &[(&str, &str)], _: Duration) -> Result<Response, NetError> {
            self.urls.lock().unwrap().push(url.to_string());
            let mut p = self.pages.lock().unwrap();
            let (status, body) = if p.is_empty() { (200, "<table class='game-table'><tbody></tbody></table>".to_string()) } else { p.remove(0) };
            Ok(Response { status, retry_after: None, body })
        }
    }
    fn fake(pages: Vec<(u16, String)>) -> Fake { Fake { pages: Mutex::new(pages), urls: Mutex::new(vec![]) } }

    #[test] fn pages_follow_the_cursor_until_one_is_empty() {
        let h = fake(vec![(200, PAGE.to_string())]);
        let rows = fetch_all(&h, &profile_url("765"), Duration::ZERO).unwrap();
        assert_eq!(rows.len(), 19);
        assert_eq!(*h.urls.lock().unwrap(), vec!["https://www.backlog-assassins.net/users/+765/games".to_string(), "https://www.backlog-assassins.net/users/+765/games?start_at=a24kz45".to_string()]);
        assert!(fetch_all(&fake(vec![(404, String::new())]), "x", Duration::ZERO).unwrap_err().contains("not found"));
        assert!(fetch_all(&fake(vec![(503, String::new())]), "x", Duration::ZERO).unwrap_err().contains("HTTP 503"));
        assert!(fetch_all(&fake(vec![(200, "<html></html>".into())]), "x", Duration::ZERO).unwrap_err().contains("No BLAEO game list"));
    }

    fn lib() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute("CREATE TABLE games (appid INTEGER PRIMARY KEY, name TEXT, `groups` TEXT, completion_status TEXT)", []).unwrap();
        for (a, g, s) in [(1, "", "Never Played"), (2, "SG,Mine", "Completed"), (3, "Old Name", "Unfinished"), (4, "Old Name,Keep", "Beaten"), (5, "Hand", "Never Played")] {
            c.execute("INSERT INTO games VALUES (?, ?, ?, ?)", rusqlite::params![a, format!("G{a}"), g, s]).unwrap();
        }
        c
    }
    fn row(a: i64, st: &str, lists: &[(&str, &str)]) -> Row { Row { appid: a, status: st.into(), lists: lists.iter().map(|(i, n)| (i.to_string(), n.to_string())).collect(), cursor: None } }
    fn gs_with(lists: &[(&str, &str, &[i64])]) -> GroupSources {
        let mut g = GroupSources::empty();
        for (id, name, mem) in lists { g.0["sources"][format!("blaeo:{id}")] = json!({"type": "blaeo", "name": name, "members": mem}); for a in *mem { g.add_owner(*a, name, &format!("blaeo:{id}")); } }
        g
    }

    #[test] fn the_plan_has_the_four_kinds_of_change_and_marks_status_steps_back() {
        let c = lib();
        // BLAEO now: game 1 beaten and on list "SG"; game 2 never played (a step back) and still on "SG"; list L1 was called "Old Name" and game 3 is still on it;
        // game 4 left the list; game 99 is not in the library
        let rows = vec![row(1, "Beaten", &[("s", "SG")]), row(2, "Never Played", &[("s", "SG")]), row(3, "Unfinished", &[("l1", "New Name")]), row(4, "Beaten", &[]), row(99, "Beaten", &[]), row(5, "Unknown", &[])];
        let gs = gs_with(&[("l1", "Old Name", &[3, 4])]);
        let local = load_local(&c, &[1, 2, 3, 4, 5, 99]);
        let p = plan(rows, &local, &gs);
        assert_eq!(p.renames, vec![Rename { id: "l1".into(), from: "Old Name".into(), to: "New Name".into() }]);
        assert_eq!(p.removals, vec![Removal { appid: 4, name: "G4".into(), list_name: "New Name".into(), list_id: "l1".into() }].into_iter().map(|mut r| { r.list_name = "New Name".into(); r }).collect::<Vec<_>>(), "the group is checked under its new name");
        assert_eq!(p.additions.iter().map(|a| (a.appid, a.lists.clone())).collect::<Vec<_>>(), vec![(1, vec!["SG".to_string()])], "game 2 already has SG; game 3's group is being renamed, not added");
        assert_eq!(p.status_changes.iter().map(|s| (s.appid, s.from.as_str(), s.to.as_str(), s.lowers)).collect::<Vec<_>>(), vec![(1, "Never Played", "Beaten", false), (2, "Completed", "Never Played", true)]);
        let sel = Selection::default_for(&p);
        assert!(sel.status.contains(&1) && !sel.status.contains(&2), "stepping back starts unticked");
    }
    #[test] fn winning_or_dropping_wont_play_is_not_a_step_back() {
        assert!(lowers("Completed", "Beaten") && lowers("Beaten", "Never Played") && lowers("Unfinished", "Never Played"));
        assert!(!lowers("Never Played", "Beaten") && !lowers("Completed", "Won't Play") && !lowers("Won't Play", "Never Played") && !lowers("Beaten", "Beaten"));
    }

    #[test] fn applying_changes_only_what_was_ticked_and_respects_other_owners() {
        let c = lib();
        let rows = vec![row(1, "Beaten", &[("s", "SG")]), row(2, "Never Played", &[("s", "SG")]), row(3, "Unfinished", &[("l1", "New Name")]), row(4, "Beaten", &[])];
        let mut gs = gs_with(&[("l1", "Old Name", &[3, 4])]);
        gs.add_owner(4, "Old Name", "manual");   // game 4 also got that group by hand
        let local = load_local(&c, &[1, 2, 3, 4]);
        let p = plan(rows, &local, &gs);
        let mut sel = Selection::default_for(&p);
        sel.status.insert(2);    // the user does want the step back for game 2
        sel.additions.remove(&1);
        let d = apply(&c, &mut gs, &p, &sel).unwrap();
        assert_eq!(d, Applied { status: 2, additions: 0, removals: 0, renames: 1, protected: 1 });
        let g = |a| load_local(&c, &[a]).remove(&a).unwrap();
        assert_eq!((g(1).status.as_str(), g(1).groups), ("Beaten", vec![]), "status taken, the unticked addition not");
        assert_eq!(g(2).status, "Never Played");
        assert_eq!(g(3).groups, vec!["New Name".to_string()], "renamed on the game that is on the list");
        assert_eq!(g(4).groups, vec!["Old Name".to_string(), "Keep".to_string()], "game 4 left the list but you added the group by hand: kept");
        // the ownership file now describes BLAEO as it is, and untracked groups are marked manual
        assert_eq!(gs.stored_lists().get("s").map(String::as_str), Some("SG"));
        assert!(gs.stored_members().contains(&(3, "l1".to_string())) && !gs.stored_members().contains(&(4, "l1".to_string())));
        assert!(gs.is_protected(5, "Hand", "blaeo:x"), "a group nothing owns became manual");
        // and with nobody else owning it, a game that left a renamed list loses the group under its old name
        let c2 = lib();
        let mut gs2 = gs_with(&[("l1", "Old Name", &[3, 4])]);
        let p2 = plan(vec![row(3, "Unfinished", &[("l1", "New Name")]), row(4, "Beaten", &[])], &load_local(&c2, &[3, 4]), &gs2);
        let d2 = apply(&c2, &mut gs2, &p2, &Selection::default_for(&p2)).unwrap();
        assert_eq!((d2.removals, d2.protected), (1, 0));
        assert_eq!(load_local(&c2, &[4])[&4].groups, vec!["Keep".to_string()]);
        assert!(gs.0["assignments"]["3"]["New Name"].as_array().unwrap().iter().any(|s| s == "blaeo:l1"));
    }
    #[test] fn a_removal_takes_the_group_away_when_nobody_else_owns_it() {
        let c = lib();
        let mut gs = gs_with(&[("o", "Old Name", &[3])]);
        let p = plan(vec![row(3, "Unfinished", &[])], &load_local(&c, &[3]), &gs);
        assert_eq!(p.removals.len(), 1);
        let d = apply(&c, &mut gs, &p, &Selection::default_for(&p)).unwrap();
        assert_eq!((d.removals, d.protected), (1, 0));
        assert!(load_local(&c, &[3])[&3].groups.is_empty());
        assert!(gs.stored_members().is_empty());
        // the file round-trips
        let f = std::env::temp_dir().join(format!("zest_gs_{}.json", std::process::id()));
        gs.save(&f).unwrap();
        assert_eq!(GroupSources::load(&f), gs);
    }
}
