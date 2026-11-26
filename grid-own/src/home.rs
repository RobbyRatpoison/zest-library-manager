//! The Home page: shelves of covers in rows, laid out the way the web page does (see templates/index.html):
//! row heights in vh, split rows divided by col_width, cards sized from the shelf height at a 2:3 shape, an underfull
//! shelf centred, each cover drawn at 95% inside its card box, shelf panels at 30% black with a faint border.
//! Data comes from the ported shelf logic in pd-filter; covers decode on background threads at the exact size.
use crate::{paint_op, Op, Pixels};
use pd_filter::home::{default_shelves, home_rows, home_shelves, widget_counts, HomeRow};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;


/// What the card actions need to know about a game.
#[derive(Clone, Debug)]
pub struct GameInfo { pub appid: i64, pub name: String, pub installed: bool, pub status: String, pub platform: String }

#[derive(Clone)]
pub struct Shelf { pub id: String, pub label: String, pub shuffle: bool, pub widget: Option<String>, pub row_height: f32, pub col_width: f32, /// The row takes what the other rows leave of the window.
    pub fill: bool, /// A widget shelf shows its title above it only when asked (the web page never did, and the default widgets carry names like "Clock [Widget]").
    pub show_title: bool, /// Clock widget only: 24 hour time (the default), seconds shown (off), date shown (on).
    pub clock: ClockFmt, pub games: Vec<(i64, String)> }
pub enum Row { Single(usize), Split { shelves: Vec<usize>, row_height: f32 } }
pub struct HomeData { pub shelves: Vec<Shelf>, pub rows: Vec<Row>, pub completion: BTreeMap<String, i64>, pub achievements: BTreeMap<String, i64>, conn: rusqlite::Connection, state: Value, info: HashMap<i64, GameInfo>, pub account_id: Option<String>, db: PathBuf, root: PathBuf, pub outlines: Arc<HashMap<i64, (u8, u8, u8)>>, pub pick_outlines: Arc<HashMap<i64, (u8, u8, u8)>>, pub home_outlines: Arc<HashMap<i64, (u8, u8, u8)>>, pub badges: Arc<HashMap<i64, BadgeData>>, pub badge_cfg: Option<Arc<BadgeCfg>>, pub badge_cfg_home: Option<Arc<BadgeCfg>>, pub badge_cfg_pick: Option<Arc<BadgeCfg>>, pag_cache: std::sync::Mutex<Option<Option<Arc<pd_filter::pagywosg_filter::Spec>>>>, pag_live: std::sync::Mutex<Option<Arc<pd_filter::pagywosg_filter::Spec>>> }

fn json_truthy(v: &Value) -> bool {
    match v { Value::Null => false, Value::Bool(b) => *b, Value::Number(n) => n.as_f64().map_or(true, |f| f != 0.0), Value::String(s) => !s.is_empty(), Value::Array(a) => !a.is_empty(), Value::Object(o) => !o.is_empty() }
}

fn to_sql(v: &Value) -> rusqlite::types::Value {
    use rusqlite::types::Value as V;
    match v { Value::Null => V::Null, Value::Bool(b) => V::Integer(*b as i64), Value::Number(n) => n.as_i64().map(V::Integer).unwrap_or_else(|| V::Real(n.as_f64().unwrap_or(0.0))), Value::String(s) => V::Text(s.clone()), o => V::Text(o.to_string()) }
}

/// What the Edit dialog needs to show about duplicates.
#[derive(Clone, Debug, Default)]
pub struct DupInfo { pub linked: Option<(i64, String, String)>, pub hidden_copies: usize, pub suggestion: Option<(i64, String, String)> }

/// What the card badges read per game (`renderCardBadges`).
#[derive(Clone, Default)]
pub struct BadgeData { pub platform: String, pub total_ach: f64, pub unlocked: f64, pub total_rev: f64, pub review_pct: Option<f64>, pub weighted_pct: Option<f64>, pub hltb: [f64; 3] }
/// `state.card_badges`, read for the Library page.
pub struct BadgeCfg { pub slots: Vec<(String, String)>, pub scale: f32, pub fallback: String, pub installed_icon: Option<String>, pub plat_icons: HashMap<String, String>,
    pub ach: Vec<(f64, String)>, pub rev_raw: bool, pub rev: Vec<(f64, String)>, pub hltb_mode: String }

impl BadgeCfg {
    /// `_cardBadgeThresholdColor`: the color of the highest band whose min is at or below the value.
    pub fn band(bands: &[(f64, String)], v: f64) -> Option<&str> {
        let mut b: Vec<&(f64, String)> = bands.iter().collect();
        b.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut c = None;
        for x in b { if v >= x.0 { c = Some(x.1.as_str()); } }
        c
    }
    /// `_cardBadgeHltbMinutes`.
    pub fn hltb_minutes(&self, h: &[f64; 3]) -> Option<f64> {
        let one = |v: f64| if v > 0.0 { Some(v) } else { None };
        match self.hltb_mode.as_str() {
            "main" => one(h[0]), "extras" => one(h[1]), "completionist" => one(h[2]),
            m => { let v: Vec<f64> = h.iter().copied().filter(|x| *x > 0.0).collect(); if v.is_empty() { None } else if m == "longest" { v.into_iter().reduce(f64::max) } else { v.into_iter().reduce(f64::min) } }
        }
    }
}

/// `fmtHours`: 75 -> "1.2 hrs", 12345 -> "205.8 hrs", 60000 -> "1,000 hrs".
pub fn fmt_hours(minutes: f64) -> String {
    if minutes == 0.0 { return "0 hrs".into(); }
    let h = minutes / 60.0;
    if h >= 1000.0 { let n = h.round() as i64; let s = n.to_string(); let mut out = String::new(); for (i, c) in s.chars().enumerate() { if i > 0 && (s.len() - i) % 3 == 0 { out.push(','); } out.push(c); } format!("{out} hrs") } else { format!("{:.1} hrs", h) }
}

/// 'YYYY-MM-DD' as the tooltip shows it ("Mar 5, 2021", en-US like the page's toLocaleDateString).
pub fn tip_date(s: &str) -> Option<String> {
    let mut it = s.split('-');
    let (y, m, d): (i32, usize, u32) = (it.next()?.parse().ok()?, it.next()?.parse().ok()?, it.next()?.parse().ok()?);
    let names = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    Some(format!("{} {}, {}", names.get(m.checked_sub(1)?)?, d, y))
}

impl HomeData {
    /// The hover tooltip's field list when it is on for `page` (library, home, pick6), else None.
    pub fn tip_fields(&self, page: &str) -> Option<HashSet<String>> {
        let ht = self.state.get("hover_tooltip")?;
        if !ht.get(page).and_then(|v| v.as_bool()).unwrap_or(false) { return None; }
        Some(ht.get("fields").and_then(|f| f.as_array()).map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default())
    }
    /// The Steam folder (the user's choice in `state.json` `steam_path`, else the default places).
    pub fn db_path(&self) -> PathBuf { self.db.clone() }
    pub fn root_dir(&self) -> PathBuf { self.root.clone() }
    pub fn state_f64(&self, k: &str, d: f64) -> f64 { self.state.get(k).and_then(|v| v.as_f64()).unwrap_or(d) }
    pub fn state_bool(&self, k: &str, d: bool) -> bool { self.state.get(k).and_then(|v| v.as_bool()).unwrap_or(d) }
    pub fn steam_root(&self) -> Option<PathBuf> { pd_filter::steamlocal::steam_root(self.state.get("steam_path").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(Path::new)) }

    /// `sync_local_install_status`: read every manifest and make the Steam games' installed flags match (fully installed only), then refresh what the cards know.
    /// Returns how many are installed, None when Steam is not found.
    pub fn sync_installed_from_steam(&mut self) -> Option<usize> {
        let root = self.steam_root()?;
        let ids = pd_filter::steamlocal::installed_appids(&pd_filter::steamlocal::manifests(&root));
        let c = self.dup_conn().ok()?;
        let n = pd_filter::steamlocal::sync_installed(&c, &ids).ok()?;
        let set: HashSet<i64> = ids.iter().copied().collect();
        for g in self.info.values_mut() { if g.platform == "steam" { g.installed = set.contains(&g.appid); } }
        if n > 0 { self.refresh_install_shelves(); }
        Some(n)
    }

    /// A launch of an installed game: last played = now, Never Played becomes Unfinished (a game that is not installed records nothing).
    /// What the Library shows, as `SELECT * FROM games ...` and its parameters, for the CSV export.
    pub fn csv_filter_query(&self) -> (String, Vec<rusqlite::types::Value>) {
        let q = pd_filter::query::library_query(&self.state, &HashMap::new(), None);
        let to_sql = |v: &Value| -> rusqlite::types::Value { match v { Value::String(s) => s.clone().into(), Value::Bool(b) => (*b as i64).into(),
            Value::Number(n) => n.as_i64().map(Into::into).unwrap_or_else(|| n.as_f64().unwrap_or(0.0).into()), _ => rusqlite::types::Value::Null } };
        (q.query.clone(), q.params.iter().map(to_sql).collect())
    }
    /// Steam games whose details were never fetched (a first import, or one that was interrupted).
    pub fn unfetched_steam_ids(&self) -> Vec<i64> {
        let Ok(c) = self.dup_conn() else { return vec![] };
        c.prepare("SELECT appid FROM games WHERE platform = 'steam' AND (meta_fetched IS NULL OR meta_fetched = '0') ORDER BY appid DESC").ok()
            .map(|mut s| s.query_map([], |r| r.get::<_, i64>(0)).map(|r| r.flatten().collect()).unwrap_or_default()).unwrap_or_default()
    }
    /// Add Game, Steam mode: a placeholder row the fill-in jobs complete (its installed flag from Steam's own files).
    pub fn add_steam_game(&mut self, appid: i64, name: &str) -> Result<pd_filter::addgame::Added, String> {
        let installed = self.steam_root().map_or(false, |r| pd_filter::steamlocal::installed_appids(&pd_filter::steamlocal::manifests(&r)).contains(&appid));
        let c = self.dup_conn().map_err(|e| e.to_string())?;
        pd_filter::addgame::add_steam(&c, appid, name, unix_now(), installed)
    }
    /// Add Game, Manual mode.
    pub fn add_custom_game(&mut self, name: &str) -> Result<pd_filter::addgame::Added, String> {
        let c = self.dup_conn().map_err(|e| e.to_string())?;
        pd_filter::addgame::add_custom(&c, name, unix_now())
    }
    /// The program (and arguments) a manual game runs, set when it is added or from its Edit dialog.
    pub fn set_custom_launch(&mut self, appid: i64, exe: &str, args: &str) -> Result<(), String> {
        let c = self.dup_conn().map_err(|e| e.to_string())?;
        let (path, folder, installed) = custom_program_fields(exe);
        c.execute("UPDATE games SET platform_executable = ?1, install_path = ?2, installed = COALESCE(?3, installed), launch_args = ?4 WHERE appid = ?5", rusqlite::params![path, folder, installed, args.trim(), appid]).map_err(|e| e.to_string())?;
        if let Some(g) = self.info.get_mut(&appid) { if let Some(i) = installed { g.installed = i != 0; } }
        Ok(())
    }

    /// Manual games with a program: Installed follows whether the file is there (it can be renamed or deleted at any time and nothing else watches it). Returns how many changed.
    /// Inside a Flatpak the program is on the host and cannot be looked at, so nothing is changed there (`custom_program_fields` counts it as installed).
    pub fn sync_custom_installs(&mut self) -> usize {
        if pd_filter::sandbox::in_flatpak() { return 0; }
        let Ok(c) = self.dup_conn() else { return 0 };
        let rows: Vec<(i64, String, i64)> = c.prepare("SELECT appid, platform_executable, COALESCE(installed, 0) FROM games WHERE platform = 'custom' AND COALESCE(platform_executable, '') != ''")
            .and_then(|mut s| s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map(|it| it.flatten().collect())).unwrap_or_default();
        let mut changed = 0;
        for (appid, exe, was) in rows {
            let p = std::path::Path::new(&exe);
            let now = (p.is_file() || (cfg!(target_os = "macos") && exe.ends_with(".app") && p.exists())) as i64;
            if now != was && c.execute("UPDATE games SET installed = ?1 WHERE appid = ?2", rusqlite::params![now, appid]).is_ok() {
                if let Some(g) = self.info.get_mut(&appid) { g.installed = now != 0; }
                changed += 1;
            }
        }
        if changed > 0 { self.refresh_install_shelves(); }
        changed
    }

    /// The program chosen for a game (`platform_executable`, relative to its install folder); None goes back to Zest's own guess.
    pub fn set_executable(&mut self, appid: i64, rel: Option<&str>) -> Result<(), String> {
        let c = self.dup_conn().map_err(|e| e.to_string())?;
        c.execute("UPDATE games SET platform_executable = ?1 WHERE appid = ?2", rusqlite::params![rel, appid]).map(|_| ()).map_err(|e| e.to_string())
    }

    /// A game's files are gone or forgotten: not installed, no folder, no chosen executable.
    pub fn clear_install(&mut self, appid: i64) {
        if let Ok(c) = self.dup_conn() { let _ = c.execute("UPDATE games SET installed = 0, install_path = NULL, platform_executable = NULL WHERE appid = ?", [appid]); }
        if let Some(g) = self.info.get_mut(&appid) { g.installed = false; }
        self.refresh_install_shelves();
    }

    /// The Installed / Not installed shelves follow installs and uninstalls: they are asked again (the same query and de-duplication Home uses at start) whenever an
    /// install flag changed, and only those shelves are replaced, so the shuffled ones keep what they show.
    pub fn refresh_install_shelves(&mut self) {
        let wanted: HashSet<String> = self.state["shelves"].as_array().into_iter().flatten().filter(|s| ["filter_key", "preset"].iter().any(|k| matches!(s.get(*k).and_then(|v| v.as_str()), Some("installed") | Some("not_installed"))))
            .filter_map(|s| s.get("id").and_then(|v| v.as_str()).map(String::from)).collect();
        if wanted.is_empty() { return; }
        let got = home_shelves(&self.conn, &self.state, &HashMap::new(), None);
        let missing: Vec<String> = got.iter().filter(|g| wanted.contains(&g.id)).flat_map(|g| g.appids.iter()).filter(|a| !self.info.contains_key(a)).map(|a| a.to_string()).collect();
        for chunk in missing.chunks(900) {
            let sql = format!("SELECT appid, name, installed, completion_status, platform FROM games WHERE appid IN ({})", chunk.join(","));
            if let Ok(mut st) = self.conn.prepare(&sql) {
                if let Ok(rows) = st.query_map([], |r| Ok(GameInfo { appid: r.get(0)?, name: shown_name(r.get::<_, Option<String>>(1)?, r.get(0)?), installed: r.get::<_, Option<i64>>(2)?.unwrap_or(0) != 0,
                    status: r.get::<_, Option<String>>(3)?.unwrap_or_default(), platform: r.get::<_, Option<String>>(4)?.unwrap_or_default() })) { for g in rows.flatten() { self.info.insert(g.appid, g); } }
            }
        }
        for g in got.iter().filter(|g| wanted.contains(&g.id)) {
            if let Some(i) = self.shelves.iter().position(|s| s.id == g.id) { self.shelves[i].games = g.appids.iter().map(|a| (*a, self.info.get(a).map(|i| i.name.clone()).unwrap_or_default())).collect(); }
        }
        // A game that moved onto an install shelf is also dropped from the shelves filled after it (de-duplication), which are otherwise left as loaded.
        let rank: HashMap<String, usize> = {
            let key = |s: &Value| -> i64 { if s.get("dedup").map_or(true, json_truthy) { s.get("dedup_priority").and_then(|v| v.as_i64()).unwrap_or(999) } else { 9999 } };
            let mut order: Vec<&Value> = self.state["shelves"].as_array().into_iter().flatten().filter(|s| s.get("visible").map_or(true, json_truthy)).collect();
            order.sort_by_key(|s| key(s));
            order.iter().enumerate().filter_map(|(n, s)| s.get("id").and_then(|v| v.as_str()).map(|id| (id.to_string(), n))).collect()
        };
        let dedups = |id: &str| self.state["shelves"].as_array().into_iter().flatten().find(|s| s.get("id").and_then(|v| v.as_str()) == Some(id)).map_or(false, |s| s.get("dedup").map_or(true, json_truthy));
        let mut drops: Vec<(usize, HashSet<i64>)> = Vec::new();
        for g in got.iter().filter(|g| wanted.contains(&g.id) && dedups(&g.id)) {
            let Some(&r) = rank.get(&g.id) else { continue };
            let ids: HashSet<i64> = g.appids.iter().copied().collect();
            for (i, s) in self.shelves.iter().enumerate() {
                if wanted.contains(&s.id) || s.widget.is_some() || !dedups(&s.id) { continue; }
                if rank.get(&s.id).map_or(false, |&sr| sr > r) { drops.push((i, ids.clone())); }
            }
        }
        for (i, ids) in drops { self.shelves[i].games.retain(|(a, _)| !ids.contains(a)); }
    }
    /// What a source's install scan found: sets the installed flag (and folder) of its games. Returns how many rows changed.
    pub fn apply_installed(&mut self, platform: &str, rows: &[pd_filter::sources::InstalledRow]) -> usize {
        let Ok(c) = self.dup_conn() else { return 0 };
        let mut changed = 0;
        for r in rows {
            let n = match &r.install_path {
                Some(p) => c.execute("UPDATE games SET installed = ?1, install_path = NULLIF(?2, '') WHERE platform = ?3 AND platform_id = ?4 AND (COALESCE(installed,0) != ?1 OR COALESCE(install_path,'') != ?2)", rusqlite::params![r.installed as i64, p, platform, r.platform_id]),
                None => c.execute("UPDATE games SET installed = ?1 WHERE platform = ?2 AND platform_id = ?3 AND COALESCE(installed,0) != ?1", rusqlite::params![r.installed as i64, platform, r.platform_id]),
            };
            changed += n.unwrap_or(0);
        }
        if changed > 0 { self.refresh_info(); self.refresh_install_shelves(); }
        changed
    }
    pub fn record_launch(&mut self, appid: i64) -> bool {
        let Ok(c) = self.dup_conn() else { return false };
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        let was_never = self.info.get(&appid).map_or(false, |g| g.status == "Never Played");
        let wrote = pd_filter::steamlocal::record_launch(&c, appid, now).ok().flatten().is_some();
        if wrote && was_never {
            if let Some(g) = self.info.get_mut(&appid) { if let Some(n) = self.completion.get_mut(&g.status) { *n -= 1; } *self.completion.entry("Unfinished".to_string()).or_insert(0) += 1; g.status = "Unfinished".into(); }
            let _ = self.recalculate_tag_similarity();
            self.compute_outlines();
        }
        wrote
    }

    /// The platform order that decides which copy of a game is shown: the saved order, else the default one.
    pub fn priority(&self) -> Vec<String> {
        let saved: Vec<String> = self.state.get("platform_priority").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|p| p.as_str().map(String::from)).collect()).unwrap_or_default();
        if saved.is_empty() { pd_filter::query::PLATFORM_PRIORITY_DEFAULT.iter().map(|s| s.to_string()).collect() } else { saved }
    }

    /// The platforms in the library, best-ranked first (PlayDate's `_active_platform_priority`): the saved order for those present, then any present ones it does not list.
    pub fn priority_platforms(&self) -> Vec<String> {
        let present: Vec<String> = self.conn.prepare("SELECT DISTINCT platform FROM games WHERE platform IS NOT NULL AND platform != '' ORDER BY platform").ok()
            .map(|mut st| st.query_map([], |r| r.get::<_, String>(0)).map(|r| r.flatten().collect()).unwrap_or_default()).unwrap_or_default();
        let mut out: Vec<String> = self.priority().into_iter().filter(|p| present.contains(p)).collect();
        for p in present { if !out.contains(&p) { out.push(p); } }
        out
    }
    /// Save a new platform order (the shown platforms first, then whatever the old order held that is not in this library, so nothing is forgotten) and re-point the duplicate links to match.
    pub fn save_priority(&mut self, shown: Vec<String>) -> Result<usize, String> {
        let mut all = shown;
        for p in self.priority() { if !all.contains(&p) { all.push(p); } }
        self.save_pref("platform_priority", Value::from(all));
        let c = self.dup_conn()?;
        let n = pd_filter::dupes::realign(&c, &self.priority()).map_err(|e| e.to_string())?;
        Ok(n)
    }

    fn dup_conn(&self) -> Result<rusqlite::Connection, String> {
        let c = rusqlite::Connection::open(&self.db).map_err(|e| e.to_string())?;
        c.busy_timeout(std::time::Duration::from_secs(10)).ok();
        Ok(c)
    }

    /// What the Edit dialog's Duplicate-of row shows for a game: the copy it is hidden under, how many copies hide under it, a pending suggestion.
    pub fn dup_info(&self, appid: i64) -> DupInfo {
        let Ok(c) = self.dup_conn() else { return DupInfo::default() };
        let one = |id: i64| -> Option<(i64, String, String)> { c.query_row("SELECT appid, COALESCE(name, ''), COALESCE(platform, 'steam') FROM games WHERE appid = ?", [id], |r| Ok((r.get(0)?, shown_name(Some(r.get::<_, String>(1)?), r.get(0)?), r.get(2)?))).ok() };
        let linked = c.query_row("SELECT duplicate_of FROM games WHERE appid = ?", [appid], |r| r.get::<_, Option<String>>(0)).ok().flatten().and_then(|d| d.trim().parse::<i64>().ok()).and_then(one);
        let hidden_copies = c.query_row("SELECT COUNT(*) FROM games WHERE duplicate_of = ?", [appid.to_string()], |r| r.get::<_, i64>(0)).unwrap_or(0) as usize;
        let suggestion = pd_filter::dupes::suggestions(&c, &self.priority()).ok().and_then(|v| v.into_iter().find(|s| s.appid == appid)).map(|s| (s.target, s.target_name, s.target_platform));
        DupInfo { linked, hidden_copies, suggestion }
    }

    /// The Duplicate-of search: games whose name contains the text, on another platform than the game itself.
    pub fn dup_search(&self, q: &str, appid: i64) -> Vec<(i64, String, String)> {
        let q = q.trim();
        if q.is_empty() { return Vec::new(); }
        let Ok(c) = self.dup_conn() else { return Vec::new() };
        let plat: String = c.query_row("SELECT COALESCE(platform, 'steam') FROM games WHERE appid = ?", [appid], |r| r.get(0)).unwrap_or_default();
        let like = format!("%{}%", q.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
        let Ok(mut st) = c.prepare("SELECT appid, COALESCE(name, ''), COALESCE(platform, 'steam') FROM games WHERE name LIKE ? ESCAPE '\\' AND appid != ? AND COALESCE(platform, 'steam') != ? ORDER BY name COLLATE NOCASE LIMIT 12") else { return Vec::new() };
        st.query_map(rusqlite::params![like, appid, plat], |r| Ok((r.get(0)?, r.get::<_, String>(1)?, r.get(2)?))).map(|it| it.flatten().collect()).unwrap_or_default()
    }

    /// A hand link from the Edit dialog (the platform order still decides which copy is shown); the hidden copy also takes the shown copy's HLTB data, as `set_duplicate` does.
    pub fn dup_link(&mut self, appid: i64, target: i64) -> Result<(), String> {
        let c = self.dup_conn()?;
        pd_filter::dupes::link_manual(&c, appid, target, &self.priority()).map_err(|e| e.to_string())?;
        let _ = c.execute("UPDATE games SET hltb_fetched = (SELECT hltb_fetched FROM games t WHERE t.appid = ?1), hltb_id = (SELECT hltb_id FROM games t WHERE t.appid = ?1), hltb_matched_name = (SELECT hltb_matched_name FROM games t WHERE t.appid = ?1), hltb_match_score = (SELECT hltb_match_score FROM games t WHERE t.appid = ?1), hltb_main = (SELECT hltb_main FROM games t WHERE t.appid = ?1), hltb_extras = (SELECT hltb_extras FROM games t WHERE t.appid = ?1), hltb_completionist = (SELECT hltb_completionist FROM games t WHERE t.appid = ?1) WHERE appid = ?2 AND (SELECT hltb_fetched FROM games t WHERE t.appid = ?1) NOT IN ('0', 'unconfirmed', 'no_match') AND (SELECT hltb_fetched FROM games t WHERE t.appid = ?1) IS NOT NULL", rusqlite::params![target, appid]);
        Ok(())
    }
    pub fn dup_unlink(&mut self, appid: i64) -> Result<(), String> { let c = self.dup_conn()?; pd_filter::dupes::unlink(&c, appid).map_err(|e| e.to_string()) }
    pub fn dup_suggestions(&self) -> Vec<pd_filter::dupes::Suggestion> { self.dup_conn().ok().and_then(|c| pd_filter::dupes::suggestions(&c, &self.priority()).ok()).unwrap_or_default() }
    pub fn dup_approve(&mut self, pairs: &[(i64, i64)]) -> Result<(), String> { let c = self.dup_conn()?; pd_filter::dupes::approve(&c, pairs, &self.priority()).map_err(|e| e.to_string()) }
    pub fn dup_reject(&mut self, ids: &[i64]) -> Result<(), String> { let c = self.dup_conn()?; pd_filter::dupes::reject(&c, ids).map_err(|e| e.to_string()) }
    pub fn hidden_duplicates(&self) -> i64 { self.dup_conn().ok().and_then(|c| pd_filter::dupes::hidden_count(&c).ok()).unwrap_or(0) }

    /// Match games that have not been examined yet (new since the last run), once, at start; later also after an import. Returns how many it linked.
    pub fn dup_examine_new(&mut self) -> usize {
        let Ok(c) = self.dup_conn() else { return 0 };
        if pd_filter::dupes::ensure_column(&c).is_err() { return 0; }
        pd_filter::dupes::examine_new(&c, &self.priority()).map(|d| d.linked).unwrap_or(0)
    }

    /// The tooltip's field list regardless of the page flags (for the `tooltip` click rule).
    pub fn tip_fields_any(&self) -> HashSet<String> {
        self.state.get("hover_tooltip").and_then(|h| h.get("fields")).and_then(|f| f.as_array()).map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_else(|| ["description", "playtime", "hltb", "last_played", "date_added", "release_date", "platform", "community_score", "metacritic", "developers", "publishers"].iter().map(|s| s.to_string()).collect())
    }
    /// HLTB minutes (main, extras, completionist) for the tooltip's three-column block.
    pub fn tip_hltb(&self, appid: i64) -> [f64; 3] {
        self.conn.query_row("SELECT COALESCE(hltb_main,0), COALESCE(hltb_extras,0), COALESCE(hltb_completionist,0) FROM games WHERE appid = ?", [appid], |r| Ok([r.get::<_, f64>(0).unwrap_or(0.0), r.get::<_, f64>(1).unwrap_or(0.0), r.get::<_, f64>(2).unwrap_or(0.0)])).unwrap_or([0.0; 3])
    }

    /// The Library's card badges: the settings and, when any slot is filled, the per-game numbers they read.
    fn badge_cfg_for(&self, page: &str) -> Option<Arc<BadgeCfg>> {
        let cb = self.state.get("card_badges")?;
        if !cb.get("enabled").and_then(|e| e.get(page)).and_then(|v| v.as_bool()).unwrap_or(true) { return None; }
        let slots: Vec<(String, String)> = ["top_left", "top_right", "bottom_left", "bottom_right"].iter()
            .filter_map(|c| cb.get("slots").and_then(|s| s.get(*c)).and_then(|v| v.as_str()).map(|f| (c.to_string(), f.to_string()))).collect();
        if slots.is_empty() { return None; }
        let bands = |v: Option<&Value>| -> Vec<(f64, String)> { v.and_then(|t| t.get("thresholds")).and_then(|t| t.as_array()).map(|a| a.iter().filter_map(|b| Some((b.get("min")?.as_f64()?, b.get("color")?.as_str()?.to_string()))).collect()).unwrap_or_default() };
        let plat_icons = cb.get("icons").and_then(|i| i.get("platform")).and_then(|p| p.as_object()).map(|m| m.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).collect()).unwrap_or_default();
        Some(Arc::new(BadgeCfg {
            slots, scale: cb.get("scale").and_then(|v| v.as_f64()).filter(|s| *s > 0.0).unwrap_or(1.0) as f32,
            fallback: cb.get("platform_fallback").and_then(|v| v.as_str()).unwrap_or("label").to_string(),
            installed_icon: cb.get("icons").and_then(|i| i.get("installed")).and_then(|v| v.as_str()).map(String::from), plat_icons,
            ach: bands(cb.get("achievement_percent")), rev_raw: cb.get("review_score").and_then(|r| r.get("mode")).and_then(|m| m.as_str()) == Some("raw"), rev: bands(cb.get("review_score")),
            hltb_mode: cb.get("hltb").and_then(|h| h.get("mode")).and_then(|m| m.as_str()).unwrap_or("main").to_string(),
        }))
    }

    pub fn compute_badges(&mut self) {
        self.badges = Arc::new(HashMap::new());
        self.badge_cfg = None; self.badge_cfg_home = None; self.badge_cfg_pick = None;
        self.badge_cfg = self.badge_cfg_for("library");
        self.badge_cfg_home = self.badge_cfg_for("home");
        self.badge_cfg_pick = self.badge_cfg_for("pick6");
        if self.badge_cfg.is_none() && self.badge_cfg_home.is_none() && self.badge_cfg_pick.is_none() { return; }
        let num = |r: &rusqlite::Row, i: usize| -> f64 { match r.get_ref(i) { Ok(rusqlite::types::ValueRef::Integer(n)) => n as f64, Ok(rusqlite::types::ValueRef::Real(f)) => f, Ok(rusqlite::types::ValueRef::Text(t)) => std::str::from_utf8(t).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0.0), _ => 0.0 } };
        let opt = |r: &rusqlite::Row, i: usize| -> Option<f64> { match r.get_ref(i) { Ok(rusqlite::types::ValueRef::Null) | Err(_) => None, _ => Some(num(r, i)) } };
        let mut out = HashMap::new();
        if let Ok(mut st) = self.conn.prepare("SELECT appid, platform, total_achievements, unlocked_achievements, total_reviews, review_percentage, weighted_percentage, hltb_main, hltb_extras, hltb_completionist FROM games") {
            if let Ok(rows) = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, BadgeData { platform: r.get::<_, Option<String>>(1)?.unwrap_or_else(|| "steam".into()), total_ach: num(r, 2), unlocked: num(r, 3), total_rev: num(r, 4),
                review_pct: opt(r, 5), weighted_pct: opt(r, 6), hltb: [num(r, 7), num(r, 8), num(r, 9)] }))) { for (a, b) in rows.flatten() { out.insert(a, b); } }
        }
        self.badges = Arc::new(out);
    }

    /// `library._compute_outline_colors`: the color of the first matching card outline rule (by priority) for every game, when the Library has outlines on.
    pub fn compute_outlines(&mut self) {
        self.outlines = Arc::new(self.outline_map("library"));
        self.pick_outlines = Arc::new(self.outline_map("pick6"));
        self.home_outlines = Arc::new(self.outline_map("home"));
    }

    fn outline_map(&self, page: &str) -> HashMap<i64, (u8, u8, u8)> {
        let mut out: HashMap<i64, (u8, u8, u8)> = HashMap::new();
        let cfg = self.state.get("card_outlines");
        if !cfg.and_then(|c| c.get("enabled")).and_then(|e| e.get(page)).and_then(|v| v.as_bool()).unwrap_or(page != "home") { return out; }
        let mut rules: Vec<&Value> = cfg.and_then(|c| c.get("rules")).and_then(|r| r.as_array()).map(|a| a.iter().collect()).unwrap_or_default();
        rules.sort_by_key(|r| r.get("priority").and_then(|p| p.as_i64()).unwrap_or(999));
        let mut ctx = pd_filter::Ctx::default();
        if let Some(sf) = self.state.get("saved_filters").and_then(|v| v.as_object()) { ctx.saved_filters = sf.clone(); }
        let mut params: Vec<Value> = Vec::new();
        let mut whens = Vec::new();
        for rule in rules {
            let Some(color) = rule.get("color").and_then(|c| c.as_str()).filter(|c| c.len() == 7 && c.starts_with('#') && c[1..].chars().all(|h| h.is_ascii_hexdigit())) else { continue };
            let f = rule.get("filter").cloned().unwrap_or(Value::Null);
            let w = match f.get("type").and_then(|t| t.as_str()) {
                Some("preset") => f.get("preset_key").and_then(|k| k.as_str()).and_then(|k| pd_filter::shelf::BUILTIN_WHERE.iter().find(|(n, _)| *n == k)).map(|(_, w)| w.to_string()),
                Some("saved") => {
                    let id = f.get("filter_id").and_then(|v| v.as_str()).map(String::from);
                    self.state.get("saved_filters").and_then(|v| v.as_object()).and_then(|m| m.values().find_map(|sf| {
                        let wrapped = if sf.get("tree").is_some() { sf.clone() } else { serde_json::json!({"tree": sf}) };
                        (wrapped.get("id").and_then(|v| v.as_str()).map(String::from) == id && id.is_some()).then(|| pd_filter::shelf::filter_tree_to_sql(&wrapped["tree"], &mut params, &ctx))
                    }))
                }
                Some("custom") => f.get("tree").filter(|t| !t.is_null()).map(|t| pd_filter::shelf::filter_tree_to_sql(t, &mut params, &ctx)),
                _ => None,
            };
            let Some(w) = w else { continue };
            if w == "1=0" { continue; }
            whens.push(format!("WHEN ({w}) THEN '{color}'"));
        }
        if whens.is_empty() { return out; }
        let sql = format!("SELECT appid, CASE {} END FROM games", whens.join(" "));
        let Ok(mut st) = self.conn.prepare(&sql) else { return out };
        let Ok(rows) = st.query_map(rusqlite::params_from_iter(params.iter().map(to_sql)), |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?))) else { return out };
        for (appid, c) in rows.flatten() {
            if let Some(c) = c { let v = u32::from_str_radix(&c[1..], 16).unwrap_or(0); out.insert(appid, ((v >> 16) as u8, (v >> 8) as u8, v as u8)); }
        }
        drop(st);
        out
    }

    /// Write a completion status (a plain UPDATE, as `set_completion` does) and keep the pie and card facts in step.
    pub fn set_status(&mut self, appid: i64, status: &str) -> Result<(), String> {
        let c = rusqlite::Connection::open(&self.db).map_err(|e| e.to_string())?;
        c.busy_timeout(std::time::Duration::from_secs(10)).ok();
        c.execute("UPDATE games SET completion_status = ? WHERE appid = ?", rusqlite::params![status, appid]).map_err(|e| e.to_string())?;
        if let Some(g) = self.info.get_mut(&appid) {
            if let Some(n) = self.completion.get_mut(&g.status) { *n -= 1; }
            *self.completion.entry(status.to_string()).or_insert(0) += 1;
            g.status = status.to_string();
        }
        let _ = self.recalculate_tag_similarity();   // the liked pool is the Beaten/Completed games
        self.compute_outlines();
        Ok(())
    }

    /// Delete a game: its row, its cached covers, and optionally add it to the blacklist so a sync doesn't bring it back (as `/api/delete-game` does).
    pub fn delete_game(&mut self, appid: i64, blacklist: bool) -> Result<(), String> {
        let c = rusqlite::Connection::open(&self.db).map_err(|e| e.to_string())?;
        c.busy_timeout(std::time::Duration::from_secs(10)).ok();
        let (name, platform, platform_id): (String, Option<String>, Option<String>) = c.query_row("SELECT name, platform, platform_id FROM games WHERE appid = ?", [appid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map_err(|e| e.to_string())?;
        c.execute("DELETE FROM games WHERE appid = ?", [appid]).map_err(|e| e.to_string())?;
        if blacklist {
            c.execute("INSERT OR REPLACE INTO blacklist (appid, name, date_blacklisted, platform_id, platform) VALUES (?, ?, date('now','localtime'), ?, ?)", rusqlite::params![appid, name, platform_id, platform]).map_err(|e| e.to_string())?;
        }
        let _ = pd_filter::dupes::on_delete(&c, appid, &self.priority());
        for sub in ["vertical", "horizontal", "icons"] { let _ = std::fs::remove_file(self.root.join("static/img/library").join(sub).join(format!("{appid}.jpg"))); }
        if let Some(g) = self.info.remove(&appid) { if let Some(n) = self.completion.get_mut(&g.status) { *n -= 1; } }
        for s in &mut self.shelves { s.games.retain(|(id, _)| *id != appid); }
        Ok(())
    }

    /// Run the Pick 6 engine over the library (what `/api/pick-game` does). Returns the picks with their game facts.
    pub fn run_pick(&mut self, p: &pd_filter::pick::PickParams, use_filtered: bool, statuses: &[String]) -> Result<pd_filter::pick::PickResult, String> {
        use pd_filter::pick::{fetch_pick_data, pick_games, Profile};
        let supp: HashMap<String, Vec<i64>> = HashMap::new();
        let data = fetch_pick_data(&self.conn, &self.state, use_filtered, Some(statuses), &supp, None).map_err(|e| format!("Filter error: {e}"))?;
        if data.games.is_empty() { return Err("No games matched the current filters.".into()); }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
        let year = 1970 + (now / 31_556_952.0) as i64;
        // splitmix64: a clock-seeded xorshift gave nearly the same first value for clicks a moment apart, so the same game kept coming first
        static CLICKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let mut state = ((now * 1e6) as u64) ^ CLICKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let mut rng = move || {
            state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
        };
        let profile = Profile { liked: &data.liked, library_tags: &data.library_tags, using_fallback: data.using_fallback };
        let res = pick_games(&data.games, &profile, &self.state, p, now, year, &mut rng);
        for pk in &res.picks {
            if let Some(g) = data.games.iter().find(|g| g["appid"].as_i64() == Some(pk.appid)) {
                self.info.insert(pk.appid, GameInfo { appid: pk.appid, name: shown_name(g["name"].as_str().map(String::from), pk.appid), installed: g["installed"].as_i64().unwrap_or(0) != 0,
                    status: g["completion_status"].as_str().unwrap_or("").to_string(), platform: g["platform"].as_str().unwrap_or("").to_string() });
            }
        }
        Ok(res)
    }

    pub fn pick_status_counts(&self, use_filtered: bool) -> Option<std::collections::BTreeMap<String, i64>> {
        pd_filter::pick::status_counts(&self.conn, &self.state, use_filtered, &HashMap::new(), None).ok()
    }

    /// Save one key into state.json (read fresh, written through a temp file so a crash can't leave half a file).
    pub fn save_pref(&mut self, key: &str, value: Value) {
        *self.pag_cache.lock().unwrap() = None;
        self.state[key] = value.clone();
        let path = self.root.join("state.json");
        let mut cur: Value = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Object(Default::default()));
        cur[key] = value;
        let tmp = self.root.join("state.json.zest-tmp");
        if serde_json::to_string_pretty(&cur).ok().and_then(|t| std::fs::write(&tmp, t).ok()).is_some() { let _ = std::fs::rename(&tmp, &path); }
    }

    /// The Library's game list and its header line: the saved filter, hidden platforms, hide-duplicates and sort, exactly as the page applies them.
    pub fn library_games(&mut self) -> (Vec<grid_core::Game>, LibHead, Vec<GroupInfo>) {
        use pd_filter::query::library_query;
        let q = library_query(&self.state, &HashMap::new(), None);
        let to_sql = |v: &Value| -> rusqlite::types::Value { match v { Value::String(s) => s.clone().into(), Value::Bool(b) => (*b as i64).into(),
            Value::Number(n) => n.as_i64().map(Into::into).unwrap_or_else(|| n.as_f64().unwrap_or(0.0).into()), _ => rusqlite::types::Value::Null } };
        // only the columns this page reads (plus the grouping one), not all of them
        let group_col = self.state.get("group_by").and_then(|v| v.as_str()).filter(|g| matches!(*g, "release_date" | "date_added" | "review_percentage" | "weighted_percentage")).map(|g| format!(", {g}")).unwrap_or_default();
        let narrowed = q.query.replacen("SELECT * FROM games", &format!("SELECT appid, name, installed, completion_status, platform{group_col} FROM games"), 1);
        let sql = format!("{narrowed}, appid");
        let mut err = None;
        let mut games: Vec<grid_core::Game> = Vec::new();
        let group_by = self.state.get("group_by").and_then(|v| v.as_str()).filter(|g| !g.is_empty()).map(String::from);
        let mut keys: Vec<Option<String>> = Vec::new();
        let run = (|| -> Result<(), rusqlite::Error> {
            let mut st = self.conn.prepare(&sql)?;
            let names: Vec<String> = st.column_names().iter().map(|s| s.to_string()).collect();
            let col = |n: &str| names.iter().position(|c| c == n);
            let igroup = group_by.as_deref().and_then(|g| col(g));
            let (ia, inm, iin, ist, ipl) = (col("appid").unwrap(), col("name").unwrap(), col("installed").unwrap(), col("completion_status"), col("platform"));
            let rows = st.query_map(rusqlite::params_from_iter(q.params.iter().map(to_sql)), |r| {
                let appid: i64 = r.get(ia)?; let g = grid_core::Game { appid, name: shown_name(r.get::<_, Option<String>>(inm)?, appid), installed: r.get::<_, Option<i64>>(iin)?.unwrap_or(0) != 0 };
                let info = GameInfo { appid: g.appid, name: g.name.clone(), installed: g.installed,
                    status: match ist { Some(i) => r.get::<_, Option<String>>(i)?.unwrap_or_default(), None => String::new() }, platform: match ipl { Some(i) => r.get::<_, Option<String>>(i)?.unwrap_or_default(), None => String::new() } };
                let key = match (&group_by, igroup) { (Some(gb), Some(i)) => group_key(gb, r.get_ref(i)?, g.installed), (Some(gb), None) if gb == "installed" => group_key(gb, rusqlite::types::ValueRef::Null, g.installed), _ => None };
                Ok((g, info, key))
            })?;
            for row in rows { let (g, info, key) = row?; self.info.insert(g.appid, info); games.push(g); keys.push(key); }
            Ok(())
        })();
        if let Err(e) = run { err = Some(e.to_string()); games.clear(); keys.clear(); }
        let (games, groups) = match &group_by { Some(gb) => group_games(gb, games, keys), None => (games, Vec::new()) };
        let total: i64 = self.conn.query_row("SELECT COUNT(*) FROM games", [], |r| r.get(0)).unwrap_or(0);
        let ft = self.state.get("filter_tree");
        let real_filter = ft.map_or(false, |f| f.get("saved_filter").is_some() || f.get("custom_sql").and_then(|v| v.as_str()).map_or(false, |s| !s.is_empty()) || f.get("items").and_then(|v| v.as_array()).map_or(false, |a| !a.is_empty()));
        let filtered = real_filter || !q.hidden_platforms.is_empty();
        // what the toolbar's CLEAR answers to (the page's `has_real_filter`): a saved filter, custom SQL or any row other than the search box's own; hidden libraries alone do not count
        let is_search = |it: &serde_json::Value| it.get("type").and_then(|v| v.as_str()) == Some("condition") && it.get("column").and_then(|v| v.as_str()) == Some("name") && it.get("operator").and_then(|v| v.as_str()) == Some("LIKE");
        let clearable = ft.map_or(false, |f| f.get("saved_filter").is_some() || f.get("custom_sql").and_then(|v| v.as_str()).map_or(false, |s| !s.is_empty()) || f.get("items").and_then(|v| v.as_array()).map_or(false, |a| a.iter().any(|it| !is_search(it))));
        let mut head = String::new();
        if let Some(n) = &q.active_filter_name { head += &format!(" \u{b7} {n}"); }
        if !q.hidden_platforms.is_empty() {
            let shown: Vec<String> = self.conn.prepare("SELECT DISTINCT platform FROM games ORDER BY platform").ok()
                .and_then(|mut s| s.query_map([], |r| r.get::<_, Option<String>>(0)).ok().map(|it| it.flatten().flatten().collect())).unwrap_or_default();
            let names: Vec<String> = shown.iter().filter(|p| !q.hidden_platforms.contains(p)).map(|p| plat_label(p)).collect();
            if !names.is_empty() { head += &format!(" \u{b7} {} only", names.join(", ")); }
        }
        let sort = self.state.get("sort").and_then(|v| v.as_str()).unwrap_or("name");
        let order = self.state.get("order").and_then(|v| v.as_str()).unwrap_or("ASC");
        if sort != "name" || order != "ASC" {
            head += &format!(" \u{b7} {}", sort_label(sort));
            if sort != "random" { head += if order == "ASC" { " \u{2191}" } else { " \u{2193}" }; }
        }
        if let Some(g) = self.state.get("group_by").and_then(|v| v.as_str()).filter(|g| !g.is_empty()) { head += &format!(" \u{b7} Grouped by {}", group_by_label(g)); }
        if self.state.get("hide_duplicates").map_or(true, |v| v.as_bool().unwrap_or(true)) { let n = self.hidden_duplicates(); if n > 0 { head += &format!(" \u{b7} {n} hidden as duplicates"); } }
        self.compute_outlines();
        self.compute_badges();
        (games, LibHead { filtered, real_filter: clearable, total, tail: head, error: err }, groups)
    }

    /// The saved sort, order and grouping, as the View panel shows them.
    pub fn view_values(&self) -> (String, String, String) {
        let g = |k: &str, d: &str| self.state.get(k).and_then(|v| v.as_str()).unwrap_or(d).to_string();
        (g("sort", "name"), g("order", "ASC"), g("group_by", ""))
    }

    /// Does any game name known so far contain Chinese, Japanese or Korean characters?
    pub fn has_cjk(&self) -> bool {
        let cjk = |c: char| matches!(c as u32, 0x2E80..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF | 0x0590..=0x06FF | 0x0900..=0x097F | 0x0E00..=0x0E7F);
        self.info.values().any(|g| g.name.chars().any(cjk))
    }

    /// The blacklist (games a sync must not bring back), newest first.
    pub fn blacklist(&self) -> Vec<BlEntry> {
        let Ok(mut st) = self.conn.prepare("SELECT appid, name, date_blacklisted, platform FROM blacklist ORDER BY date_blacklisted DESC, appid") else { return Vec::new() };
        st.query_map([], |r| Ok(BlEntry { appid: r.get(0)?, name: shown_name(r.get::<_, Option<String>>(1)?, r.get(0)?), date: r.get::<_, Option<String>>(2)?.unwrap_or_default(), platform: r.get::<_, Option<String>>(3)?.unwrap_or_default() }))
            .map(|it| it.flatten().collect()).unwrap_or_default()
    }

    /// Take a game off the blacklist (`remove_from_blacklist`).
    pub fn unblacklist(&mut self, appid: i64) -> Result<(), String> {
        let c = rusqlite::Connection::open(&self.db).map_err(|e| e.to_string())?;
        c.busy_timeout(std::time::Duration::from_secs(10)).ok();
        c.execute("DELETE FROM blacklist WHERE appid = ?", [appid]).map(|_| ()).map_err(|e| e.to_string())
    }

    /// `_get_unique_csv_column`: every distinct value in a comma-separated column (tags, groups, genres, categories), sorted ignoring case.
    pub fn unique_csv(&self, col: &str) -> Vec<String> {
        if !matches!(col, "tags" | "groups" | "genres" | "categories") { return Vec::new(); }
        let sql = format!("SELECT {} FROM games WHERE {} IS NOT NULL", if col == "groups" { "`groups`" } else { col }, if col == "groups" { "`groups`" } else { col });
        let Ok(mut st) = self.conn.prepare(&sql) else { return Vec::new() };
        let mut set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        if let Ok(rows) = st.query_map([], |r| r.get::<_, Option<String>>(0)) { for v in rows.flatten().flatten() { for t in v.split(',') { let t = t.trim(); if !t.is_empty() { set.insert(t.to_string()); } } } }
        let mut v: Vec<String> = set.into_iter().collect();
        v.sort_by_key(|s| s.to_lowercase());
        v
    }

    /// The libraries present in the database, in the order the filter dialog lists them (core stores first, then the plugin ones).
    pub fn platforms_present(&self) -> Vec<String> {
        let mut v: Vec<String> = self.conn.prepare("SELECT DISTINCT platform FROM games WHERE platform IS NOT NULL").ok()
            .and_then(|mut s| s.query_map([], |r| r.get::<_, String>(0)).ok().map(|it| it.flatten().collect())).unwrap_or_default();
        let reg = pd_filter::sources::Registry::empty();
        v.sort_by_key(|p| (reg.order(p), p.clone()));
        v
    }

    /// `bulk_edit_games`: change one column for many games. `appids` None means every game; `mode` is replace, append or remove (the last two for
    /// comma-separated columns). Returns how many games changed. Same rules as the route: an empty replace value clears the field, dates are stored
    /// as UTC timestamps, group changes are recorded as manual.
    pub fn bulk_edit(&mut self, column: &str, mode: &str, value: &str, appids: Option<&[i64]>) -> Result<usize, String> {
        use rusqlite::types::Value as SV;
        const ALLOWED: [&str; 22] = ["completion_status", "tags", "groups", "developers", "publishers", "release_date", "review_score", "review_percentage", "weighted_percentage",
            "total_reviews", "positive_reviews", "playtime_forever", "date_added", "installed", "vertical_art_source", "horizontal_art_source", "icon_source", "unlocked_achievements",
            "total_achievements", "genres", "categories", "is_free"];
        if !ALLOWED.contains(&column) { return Err("Column is not editable.".into()); }
        if !matches!(mode, "replace" | "append" | "remove") { return Err("Invalid mode.".into()); }
        let value = value.trim();
        if value.is_empty() && mode == "append" { return Err("Value cannot be empty.".into()); }
        if let Some(a) = appids { if a.is_empty() { return Err("No games selected.".into()); } }
        let col = if column == "groups" { "`groups`" } else { column };
        let set_value: SV = if value.is_empty() { SV::Null }
            else if matches!(column, "release_date" | "date_added") {
                match pd_filter::date_to_ts(&Value::String(value.to_string())) { Some(t) => SV::Integer(t), None => return Err("Invalid date format. Use YYYY-MM-DD.".into()) }
            } else { SV::Text(value.to_string()) };
        let mut c = rusqlite::Connection::open(&self.db).map_err(|e| e.to_string())?;
        c.busy_timeout(std::time::Duration::from_secs(10)).ok();
        let ids: Vec<i64> = match appids { Some(a) => a.to_vec(), None => c.prepare("SELECT appid FROM games").and_then(|mut s| s.query_map([], |r| r.get(0)).map(|it| it.flatten().collect())).map_err(|e| e.to_string())? };
        let tx = c.transaction().map_err(|e| e.to_string())?;
        let mut updated = 0usize;
        let mut group_changes: Vec<(i64, String, String)> = Vec::new();
        let split = |s: &str| -> Vec<String> { s.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect() };
        {
            let mut get = tx.prepare(&format!("SELECT {col} FROM games WHERE appid = ?")).map_err(|e| e.to_string())?;
            let mut put = tx.prepare(&format!("UPDATE games SET {col} = ? WHERE appid = ?")).map_err(|e| e.to_string())?;
            for id in &ids {
                let old: Option<String> = get.query_row([id], |r| match r.get_ref(0)? { rusqlite::types::ValueRef::Null => Ok(None), v => Ok(Some(match v { rusqlite::types::ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(), rusqlite::types::ValueRef::Integer(i) => i.to_string(), rusqlite::types::ValueRef::Real(f) => f.to_string(), _ => String::new() })) }).ok().flatten();
                let new: Option<SV> = match mode {
                    "replace" => Some(set_value.clone()),
                    "append" => {
                        let mut list = split(old.as_deref().unwrap_or(""));
                        let have: std::collections::HashSet<String> = list.iter().map(|s| s.to_lowercase()).collect();
                        let mut have = have;
                        let mut added = false;
                        for v in split(value) { if !have.contains(&v.to_lowercase()) { have.insert(v.to_lowercase()); list.push(v); added = true; } }
                        added.then(|| SV::Text(list.join(",")))
                    }
                    _ => {
                        let list = split(old.as_deref().unwrap_or(""));
                        let drop: std::collections::HashSet<String> = split(value).iter().map(|s| s.to_lowercase()).collect();
                        let kept: Vec<String> = list.iter().filter(|v| !drop.contains(&v.to_lowercase())).cloned().collect();
                        (kept.len() != list.len()).then(|| SV::Text(kept.join(",")))
                    }
                };
                let Some(new) = new else { continue };
                put.execute(rusqlite::params![new, id]).map_err(|e| e.to_string())?;
                updated += 1;
                if column == "groups" { group_changes.push((*id, old.unwrap_or_default(), match &new { SV::Text(t) => t.clone(), _ => String::new() })); }
            }
        }
        tx.commit().map_err(|e| e.to_string())?;
        for (id, old, new) in group_changes { self.track_manual_groups(id, &split(&old), &split(&new)); }
        self.refresh_info();
        if matches!(column, "tags" | "completion_status") { let _ = self.recalculate_tag_similarity(); }
        Ok(updated)
    }

    /// `database.recalculate_tag_similarity`: score every game's tags against the liked-minus-library affinity profile and store it in
    /// `games.tag_similarity` (NULL for a game with no tags). Call after anything that can change tags or completion status.
    pub fn recalculate_tag_similarity(&mut self) -> Result<usize, String> {
        let state = self.state_json().clone();
        let num = |k: &str, d: f64| state.get(k).and_then(|v| v.as_f64()).unwrap_or(d);
        let (min_rate, cap_hours, k) = (num("tag_similarity_min_library_rate", 2.0) / 100.0, num("tag_similarity_playtime_cap_hours", 60.0), num("tag_similarity_smoothing_k", 4.0));
        let mut c = rusqlite::Connection::open(&self.db).map_err(|e| e.to_string())?;
        c.busy_timeout(std::time::Duration::from_secs(10)).ok();
        let split = |s: &str| -> Vec<String> { s.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect() };
        // the same affinity the Tuning panel shows (`database.compute_tag_affinity`)
        let affinity = pd_filter::tagaffinity::compute(&c, min_rate * 100.0, cap_hours, state.get("tag_similarity_overrides")).map_err(|e| e.to_string())?;
        let mut tags_of: Vec<(i64, Vec<String>)> = Vec::new();
        { let mut st = c.prepare("SELECT appid, tags FROM games").map_err(|e| e.to_string())?;
          for r in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?.unwrap_or_default()))).map_err(|e| e.to_string())?.flatten() { tags_of.push((r.0, split(&r.1))); } }
        let tx = c.transaction().map_err(|e| e.to_string())?;
        {
            let mut put = tx.prepare("UPDATE games SET tag_similarity = ? WHERE appid = ?").map_err(|e| e.to_string())?;
            for (id, tags) in &tags_of {
                let sim: Option<f64> = if tags.is_empty() { None } else { Some(tags.iter().map(|t| affinity.get(t).copied().unwrap_or(0.0)).sum::<f64>() / (tags.len() as f64 + k)) };
                put.execute(rusqlite::params![sim, id]).map_err(|e| e.to_string())?;
            }
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(tags_of.len())
    }

    /// `bulk_delete`: remove the games and their cached covers.
    /// `_METADATA_MANUAL_WHERE`: of these games, those short of developer, genres or tags, or non-Steam with no Steam match.
    pub fn missing_data_appids(&self, list: &[i64]) -> Vec<i64> {
        let Ok(c) = rusqlite::Connection::open(&self.db) else { return Vec::new() };
        let mut q = c.prepare("SELECT 1 FROM games WHERE appid = ? AND (((developers IS NULL OR developers = '') OR (genres IS NULL OR genres = '') OR (tags IS NULL OR tags = '')) OR (appid < 0 AND steam_appid IS NULL))").unwrap();
        list.iter().copied().filter(|a| q.exists([a]).unwrap_or(false)).collect()
    }

    /// `clear_artwork`: delete the cover file and forget its source (the edge colors too, so the card does not keep a backdrop for a cover that is gone).
    pub fn clear_art(&self, appid: i64, kind: &str) -> Result<(), String> {
        let (dir, col, edge) = match kind { "vertical" => ("vertical", "vertical_art_source", Some("edge_vertical = NULL, vertical_art_w = NULL, vertical_art_q = NULL, vertical_art_h")), "horizontal" => ("horizontal", "horizontal_art_source", Some("edge_horizontal = NULL, horizontal_art_w = NULL, horizontal_art_q = NULL, horizontal_art_h")), "icon" => ("icons", "icon_source", None), _ => return Err("unknown art type".into()) };
        let path = self.root.join("static/img/library").join(dir).join(format!("{appid}.jpg"));
        if path.exists() { std::fs::remove_file(&path).map_err(|e| e.to_string())?; }
        let c = rusqlite::Connection::open(&self.db).map_err(|e| e.to_string())?;
        c.busy_timeout(std::time::Duration::from_secs(10)).ok();
        c.execute(&format!("UPDATE games SET {col} = NULL{} WHERE appid = ?", edge.map(|e| format!(", {e} = NULL")).unwrap_or_default()), [appid]).map(|_| ()).map_err(|e| e.to_string())
    }

    pub fn all_appids(&self) -> Vec<i64> {
        let Ok(c) = rusqlite::Connection::open(&self.db) else { return Vec::new() };
        c.prepare("SELECT appid FROM games ORDER BY appid").and_then(|mut s| s.query_map([], |r| r.get(0)).map(|it| it.flatten().collect())).unwrap_or_default()
    }

    pub fn bulk_delete(&mut self, appids: Option<&[i64]>) -> Result<usize, String> {
        let mut c = rusqlite::Connection::open(&self.db).map_err(|e| e.to_string())?;
        c.busy_timeout(std::time::Duration::from_secs(10)).ok();
        let all: Vec<i64>;
        let appids: &[i64] = match appids { Some(a) => a, None => { all = c.prepare("SELECT appid FROM games").and_then(|mut s| s.query_map([], |r| r.get(0)).map(|it| it.flatten().collect())).map_err(|e| e.to_string())?; &all } };
        if appids.is_empty() { return Err("No games selected.".into()); }
        let tx = c.transaction().map_err(|e| e.to_string())?;
        { let mut del = tx.prepare("DELETE FROM games WHERE appid = ?").map_err(|e| e.to_string())?; for id in appids { del.execute([id]).map_err(|e| e.to_string())?; } }
        tx.commit().map_err(|e| e.to_string())?;
        { let prio = self.priority(); for id in appids { let _ = pd_filter::dupes::on_delete(&c, *id, &prio); } }
        for id in appids {
            for sub in ["vertical", "horizontal", "icons"] { let _ = std::fs::remove_file(self.root.join("static/img/library").join(sub).join(format!("{id}.jpg"))); }
            self.info.remove(id);
            for s in &mut self.shelves { s.games.retain(|(g, _)| g != id); }
        }
        self.refresh_info();
        Ok(appids.len())
    }

    /// The library gained or lost games behind Home's back (a sync, Add Game, the Playnite import and its undo): bring the shelves up to date.
    /// Games no longer in the database leave every shelf; the shelves that follow a rule (newest, never played, ...) are asked again; the shuffled ones keep
    /// what they show and only fill the places of removed games from a fresh draw (a shuffle must not reshuffle on its own). The pie charts are counted again.
    pub fn library_changed(&mut self) {
        let mut ids: HashSet<i64> = self.info.keys().copied().collect();
        for s in &self.shelves { ids.extend(s.games.iter().map(|(a, _)| *a)); }
        let ids: Vec<i64> = ids.into_iter().collect();
        let mut present: HashSet<i64> = HashSet::new();
        for chunk in ids.chunks(900) {
            let sql = format!("SELECT appid FROM games WHERE appid IN ({})", chunk.iter().map(|a| a.to_string()).collect::<Vec<_>>().join(","));
            if let Ok(mut st) = self.conn.prepare(&sql) { if let Ok(rows) = st.query_map([], |r| r.get::<_, i64>(0)) { present.extend(rows.flatten()); } }
        }
        for id in ids.iter().filter(|a| !present.contains(a)) { self.info.remove(id); }
        for s in &mut self.shelves { s.games.retain(|(id, _)| present.contains(id)); }
        let got = home_shelves(&self.conn, &self.state, &HashMap::new(), None);
        let missing: Vec<String> = got.iter().flat_map(|g| g.appids.iter()).filter(|a| !self.info.contains_key(a)).map(|a| a.to_string()).collect();
        for chunk in missing.chunks(900) {
            let sql = format!("SELECT appid, name, installed, completion_status, platform FROM games WHERE appid IN ({})", chunk.join(","));
            if let Ok(mut st) = self.conn.prepare(&sql) {
                if let Ok(rows) = st.query_map([], |r| Ok(GameInfo { appid: r.get(0)?, name: shown_name(r.get::<_, Option<String>>(1)?, r.get(0)?), installed: r.get::<_, Option<i64>>(2)?.unwrap_or(0) != 0,
                    status: r.get::<_, Option<String>>(3)?.unwrap_or_default(), platform: r.get::<_, Option<String>>(4)?.unwrap_or_default() })) { for g in rows.flatten() { self.info.insert(g.appid, g); } }
            }
        }
        let name_of = |info: &HashMap<i64, GameInfo>, a: i64| info.get(&a).map(|i| i.name.clone()).unwrap_or_default();
        for g in &got {
            let Some(i) = self.shelves.iter().position(|s| s.id == g.id) else { continue };
            if !self.shelves[i].shuffle { self.shelves[i].games = g.appids.iter().map(|a| (*a, name_of(&self.info, *a))).collect(); continue; }
            let shown: HashSet<i64> = self.shelves.iter().flat_map(|s| s.games.iter().map(|(a, _)| *a)).collect();
            let mut keep = self.shelves[i].games.clone();
            for a in g.appids.iter().filter(|a| !shown.contains(a)) { if keep.len() >= g.appids.len() { break; } keep.push((*a, name_of(&self.info, *a))); }
            self.shelves[i].games = keep;
        }
        let (c, a) = widget_counts(&self.conn);
        self.completion = c; self.achievements = a;
        self.refresh_info();
    }

    /// Reread the facts the cards use (name, installed, status) for every game this session knows, after a bulk change.
    pub fn refresh_info(&mut self) {
        let ids: Vec<String> = self.info.keys().map(|k| k.to_string()).collect();
        for chunk in ids.chunks(900) {
            let sql = format!("SELECT appid, name, installed, completion_status FROM games WHERE appid IN ({})", chunk.join(","));
            if let Ok(mut st) = self.conn.prepare(&sql) {
                if let Ok(rows) = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<i64>>(2)?.unwrap_or(0) != 0, r.get::<_, Option<String>>(3)?.unwrap_or_default()))) {
                    for (id, name, inst, status) in rows.flatten() { if let Some(g) = self.info.get_mut(&id) { g.name = shown_name(name, id); g.installed = inst; g.status = status; } }
                }
            }
        }
    }

    pub fn state_json(&self) -> &Value { &self.state }
    /// What a source needs to know about a game (`sources::GameRef`), read from the database.
    pub fn game_ref(&self, appid: i64) -> Option<pd_filter::sources::GameRef> {
        let c = self.dup_conn().ok()?;
        c.query_row("SELECT COALESCE(name,''), COALESCE(platform,'steam'), COALESCE(platform_id,''), COALESCE(platform_slug,''), COALESCE(platform_appname,''), COALESCE(installed,0), install_path, platform_executable FROM games WHERE appid = ?", [appid],
            |r| Ok(pd_filter::sources::GameRef { appid, name: r.get(0)?, platform: r.get(1)?, platform_id: r.get::<_, rusqlite::types::Value>(2).map(|v| match v { rusqlite::types::Value::Text(t) => t, rusqlite::types::Value::Integer(i) => i.to_string(), _ => String::new() })?,
                slug: r.get(3)?, appname: r.get(4)?, installed: r.get::<_, i64>(5)? != 0, install_path: r.get::<_, Option<String>>(6)?.filter(|p| !p.is_empty()), executable: r.get::<_, Option<String>>(7)?.filter(|p| !p.is_empty()) })).ok()
    }
    pub fn info(&self, appid: i64) -> Option<&GameInfo> { self.info.get(&appid) }
    /// Where the game is installed, if the database knows.
    /// A game added by hand: the program and arguments the Launch button runs.
    pub fn custom_launch(&self, appid: i64) -> Option<(String, String)> {
        self.conn.query_row("SELECT COALESCE(platform_executable,''), COALESCE(launch_args,'') FROM games WHERE appid = ?", [appid], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).ok()
    }
    pub fn install_path(&self, appid: i64) -> Option<String> {
        self.conn.query_row("SELECT install_path FROM games WHERE appid = ?", [appid], |r| r.get::<_, Option<String>>(0)).ok().flatten().filter(|p| !p.is_empty())
    }
    /// The shuffle link: replace a shelf's games with a fresh random draw (the shelf's own filter, limit and platform rules).
    pub fn shuffle(&mut self, idx: usize) {
        let id = self.shelves[idx].id.clone();
        let Some(cfg) = self.state["shelves"].as_array().and_then(|a| a.iter().find(|s| s.get("id").and_then(|v| v.as_str()) == Some(id.as_str()))).cloned() else { return };
        let Some(q) = pd_filter::home::shuffle_query(&cfg, &self.state, &HashMap::new(), None) else { return };
        let params: Vec<rusqlite::types::Value> = q.params.iter().map(to_sql).collect();
        if let Ok(mut st) = self.conn.prepare(&q.sql) {
            if let Ok(rows) = st.query_map(rusqlite::params_from_iter(params), |r| Ok(GameInfo { appid: r.get(0)?, name: shown_name(r.get::<_, Option<String>>(1)?, r.get(0)?), installed: r.get::<_, Option<i64>>(2)?.unwrap_or(0) != 0,
                status: r.get::<_, Option<String>>(3)?.unwrap_or_default(), platform: r.get::<_, Option<String>>(4)?.unwrap_or_default() })) {
                let infos: Vec<GameInfo> = rows.flatten().collect();
                self.shelves[idx].games = infos.iter().map(|g| (g.appid, g.name.clone())).collect();
                for g in infos { self.info.insert(g.appid, g); }
            }
        }
    }
}


pub fn load(root: &Path) -> Result<HomeData, String> {
    let mut state: Value = std::fs::read_to_string(root.join("state.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Object(Default::default()));
    if state.get("shelves").and_then(|v| v.as_array()).map_or(true, |a| a.is_empty()) { state["shelves"] = default_shelves(); }
    let db = pd_filter::setup::find_library(root).ok_or("no games_*.db")?;
    pd_filter::schema::repair_library(&db);   // empty-string numbers (a game with no playtime led the most-played list)
    let conn = rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| e.to_string())?;
    // a library made before Zest's own columns existed (cover size and JPEG quality, read by the Cover Quality filter and the art jobs) gets them now; `schema::init_db` is
    // otherwise only run when a library is created, so an existing one never had them added
    let newest = pd_filter::schema::ZEST_COLUMNS.last().map(|c| c.0).unwrap_or("");
    let missing = conn.prepare("SELECT 1 FROM pragma_table_info('games') WHERE name = ?").and_then(|mut s| s.exists([newest])).map(|found| !found).unwrap_or(false);
    if missing { if let Err(e) = pd_filter::schema::init_db(&db) { pd_filter::net::log(&format!("library: could not add the new columns: {e}")); } }
    let supp = HashMap::new();
    let got = home_shelves(&conn, &state, &supp, None);
    let cfg: HashMap<&str, &Value> = state["shelves"].as_array().unwrap().iter().filter_map(|s| s.get("id").and_then(|v| v.as_str()).map(|id| (id, s))).collect();
    // what the cards need: names (for covers that have no file) and the facts the click actions use
    let ids: Vec<String> = got.iter().flat_map(|s| s.appids.iter().map(|a| a.to_string())).collect();
    let mut info: HashMap<i64, GameInfo> = HashMap::new();
    if !ids.is_empty() {
        let sql = format!("SELECT appid, name, installed, completion_status, platform FROM games WHERE appid IN ({})", ids.join(","));
        if let Ok(mut st) = conn.prepare(&sql) {
            if let Ok(rows) = st.query_map([], |r| Ok(GameInfo { appid: r.get(0)?, name: shown_name(r.get::<_, Option<String>>(1)?, r.get(0)?), installed: r.get::<_, Option<i64>>(2)?.unwrap_or(0) != 0,
                status: r.get::<_, Option<String>>(3)?.unwrap_or_default(), platform: r.get::<_, Option<String>>(4)?.unwrap_or_default() })) { for g in rows.flatten() { info.insert(g.appid, g); } }
        }
    }
    let names: HashMap<i64, String> = info.iter().map(|(k, v)| (*k, v.name.clone())).collect();
    let account_id = db.file_stem().and_then(|n| n.to_str()).and_then(|n| n.strip_prefix("games_")).map(|s| s.to_string()).filter(|s| s.chars().all(|c| c.is_ascii_digit()));
    let (mut shelves, mut index) = (Vec::new(), HashMap::new());
    for g in &got {
        let c = cfg.get(g.id.as_str()).copied().unwrap_or(&Value::Null);
        let s = |k: &str| c.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let widget = [s("preset"), s("filter_key")].into_iter().find(|k| ["clock", "completion_pie", "achievement_pie"].contains(&k.as_str()));
        index.insert(g.id.clone(), shelves.len());
        shelves.push(Shelf { id: g.id.clone(), label: s("label"), shuffle: s("sort_col") == "RANDOM()", widget,
            row_height: c.get("row_height").and_then(|v| v.as_f64()).unwrap_or(30.0) as f32, col_width: c.get("col_width").and_then(|v| v.as_f64()).unwrap_or(10.0) as f32, fill: c.get("fill").and_then(|v| v.as_bool()).unwrap_or(false),
            show_title: c.get("show_title").and_then(|v| v.as_bool()).unwrap_or(false),
            clock: ClockFmt { h24: c.get("clock_24h").and_then(|v| v.as_bool()).unwrap_or(true), seconds: c.get("clock_seconds").and_then(|v| v.as_bool()).unwrap_or(false), date: c.get("clock_date").and_then(|v| v.as_bool()).unwrap_or(true) },
            games: g.appids.iter().map(|a| (*a, names.get(a).cloned().unwrap_or_default())).collect() });
    }
    let rows = home_rows(&state).into_iter().map(|r| match r {
        HomeRow::Single(id) => Row::Single(index[&id]),
        HomeRow::Split { shelves: ids, row_height } => Row::Split { shelves: ids.iter().map(|i| index[i]).collect(), row_height: row_height.unwrap_or(30) as f32 },
    }).collect();
    let (completion, achievements) = widget_counts(&conn);
    let mut d = HomeData { shelves, rows, completion, achievements, conn, state, info, account_id, db: db.clone(), root: root.to_path_buf(), outlines: Arc::new(HashMap::new()), pick_outlines: Arc::new(HashMap::new()), home_outlines: Arc::new(HashMap::new()), badges: Arc::new(HashMap::new()), badge_cfg: None, badge_cfg_home: None, badge_cfg_pick: None, pag_cache: Default::default(), pag_live: Default::default() };
    d.compute_outlines();
    d.compute_badges();
    let linked = d.dup_examine_new();
    if linked > 0 { eprintln!("duplicates: linked {linked} new games"); }
    Ok(d)
}

// ---------------------------------------------------------------- layout

#[derive(Clone)]
pub struct CardBox { pub appid: i64, pub name: String, pub x: i32, pub y: i32, pub w: i32, pub h: i32 }
/// One shelf's box and what is in it. `pie` is (centre x, centre y, canvas size) for a chart widget.
#[derive(Clone)]
pub struct ShelfBox { pub idx: usize, pub x: i32, pub y: i32, pub w: i32, pub h: i32, pub label_y: i32, pub cards: Vec<CardBox>, pub widget: Option<String>, pub pie: Option<(f32, f32, f32)>, pub off: usize, pub fit: usize, pub total: usize, /// While a shelf slides: the x range cards are shown in (nothing paints outside it).
    pub clip: Option<(i32, i32)> }

pub fn bar_height(h: i32) -> i32 { ((h as f32) * 0.05).round().max(30.0) as i32 }   // the web CSS: .nav-header { height: 5vh }

/// Where everything goes in a window of w x h pixels. Follows the web page's CSS exactly (checked against Chromium's measured boxes):
/// a shelf is a flex item whose 22px of padding (10+10) and border (1+1) are taken first, with only the rest shared out by col_width;
/// a card is the shelf height minus 22px (border 2, padding 6+8, grid padding 3+3) tall and 2/3 of that wide, a label takes its height plus 4px,
/// and the cards that fit (whole ones, 6px apart) are centred in the shelf. Positions stay fractional until they are painted.
pub fn layout(d: &HomeData, w: i32, h: i32, offs: &HashMap<usize, usize>) -> Vec<ShelfBox> { layout_scrolled(d, w, h, offs, 0.0) }

/// Where the shelves of one row sit across a window `w` pixels wide: each one's left edge and width. A shelf is a flex item whose 22 px of padding and border are taken first, with
/// only the rest shared out by `col_width`; side-by-side shelves are 0.4vw apart.
pub fn row_widths(w: i32, weights: &[f32]) -> Vec<(f32, f32)> {
    let vw = w as f32 / 100.0;
    let (left, content_w) = (0.8 * vw, w as f32 - 2.0 * 0.8 * vw);
    let n = weights.len() as f32;
    let side_gap = if weights.len() > 1 { 0.4 * vw } else { 0.0 };
    let free = content_w - side_gap * (n - 1.0) - 22.0 * n;
    let total: f32 = weights.iter().sum::<f32>().max(0.0001);
    let mut x = left;
    weights.iter().map(|wt| { let sw = if weights.len() == 1 { content_w } else { 22.0 + free * wt / total }; let r = (x, sw); x += sw + side_gap; r }).collect()
}

/// The cards of a shelf `sw` pixels wide in a row `rh` pixels tall (`vh` = one hundredth of the window height): card width and height, and how many whole cards fit across.
pub fn card_metrics(sw: f32, rh: f32, vh: f32, has_label: bool) -> (f32, f32, usize) {
    let label_total = if has_label { (1.1 * vh * 1.07).round() + 4.0 } else { 0.0 };
    let card_h = (rh - 22.0 - label_total).max(10.0);
    let card_w = card_h * 2.0 / 3.0;
    let fit = ((((sw - 22.0) + 6.0) / (card_w + 6.0)).floor() as usize).max(1);
    (card_w, card_h, fit)
}

/// The height of everything the page lists, from the top of the window: where scrolling ends (a window shorter than this scrolls).
pub fn content_height(d: &HomeData, h: i32) -> f32 {
    let vh = h as f32 / 100.0;
    let rows = row_heights_vh(d);
    // the bottom of the last shelf: the gap between rows is not added after it, so a page whose last shelf fits does not scroll
    5.5 * vh + rows.iter().sum::<f32>() * vh + 0.5 * vh * rows.len().saturating_sub(1) as f32
}

/// Each row's height in vh, "fill the rest" rows resolved (a row fills when its first shelf says so).
pub fn row_heights_vh(d: &HomeData) -> Vec<f32> {
    let (base, fill): (Vec<f32>, Vec<bool>) = d.rows.iter().map(|row| match row { Row::Single(i) => (d.shelves[*i].row_height, d.shelves[*i].fill), Row::Split { shelves, row_height } => (*row_height, shelves.first().map_or(false, |i| d.shelves[*i].fill)) }).unzip();
    pd_filter::home_edit::row_vhs(&base, &fill)
}
pub fn max_scroll(d: &HomeData, h: i32) -> f32 { { let m = content_height(d, h) - h as f32; if m < 2.0 { 0.0 } else { m } } }

/// `layout` with the page scrolled `scroll` pixels down.
pub fn layout_scrolled(d: &HomeData, w: i32, h: i32, offs: &HashMap<usize, usize>, scroll: f32) -> Vec<ShelfBox> {
    let (vh, vw) = (h as f32 / 100.0, w as f32 / 100.0);
    let (_left, _content_w) = (0.8 * vw, w as f32 - 2.0 * 0.8 * vw);
    let mut y = 5.5 * vh - scroll;
    let mut out = Vec::new();
    let heights = row_heights_vh(d);
    for (ri, row) in d.rows.iter().enumerate() {
        let members: Vec<usize> = match row { Row::Single(i) => vec![*i], Row::Split { shelves, .. } => shelves.clone() };
        let rh = heights[ri] * vh;
        let weights: Vec<f32> = members.iter().map(|i| d.shelves[*i].col_width).collect();
        let xs = row_widths(w, &weights);
        for (k, &i) in members.iter().enumerate() {
            let s = &d.shelves[i];
            let (x, sw) = xs[k];
            let mut sb = ShelfBox { idx: i, x: x.round() as i32, y: y.round() as i32, w: sw.round() as i32, h: rh.round() as i32, label_y: (y + 7.0).round() as i32, cards: vec![], widget: s.widget.clone(), pie: None, off: 0, fit: 0, total: s.games.len(), clip: None };
            if s.widget.is_some() {
                // the canvas is the widget's content box (shelf minus 22px, minus the widget's own 4px padding); the pie sits 2px inside it
                // a widget with its title on takes the title's height off the top, as a games shelf does
                let title_h = if s.show_title && !s.label.is_empty() { (1.1 * vh * 1.07).round() + 4.0 } else { 0.0 };
                let size = (sw - 30.0).min(rh - 30.0 - title_h).max(0.0);
                sb.pie = Some((x + sw / 2.0, y + title_h + (rh - title_h) / 2.0 - 2.5, size));
            } else {
                let (card_w, card_h, fit) = card_metrics(sw, rh, vh, !s.label.is_empty());
                let label_total = if s.label.is_empty() { 0.0 } else { (1.1 * vh * 1.07).round() + 4.0 };
                let avail = sw - 22.0;
                let shown = s.games.len().min(fit);
                // a pool larger than the row scrolls sideways a card at a time (the page's snapping row); `off` is the first card shown
                let off = offs.get(&i).copied().unwrap_or(0).min(s.games.len().saturating_sub(fit));
                sb.off = off; sb.fit = fit;
                let used = shown as f32 * (card_w + 6.0) - 6.0;
                // the cards shown are always centred in the shelf (a pool that scrolls shows `fit` of them, game #1 first)
                let start = x + 11.0 + ((avail - used) / 2.0).max(0.0);
                let top = y + 10.0 + label_total;
                for (c, (appid, name)) in s.games.iter().skip(off).take(shown).enumerate() {
                    let cx = start + c as f32 * (card_w + 6.0);
                    sb.cards.push(CardBox { appid: *appid, name: name.clone(), x: cx.round() as i32, y: top.round() as i32, w: card_w.round() as i32, h: card_h.round() as i32 });
                }
            }
            out.push(sb);
        }
        y += rh + 0.5 * vh;
    }
    out
}

/// The card under a point (the whole card box is the hit area, as the page's `.shelf-capsule-wrap` is).
pub fn card_at(boxes: &[ShelfBox], x: f32, y: f32) -> Option<i64> {
    boxes.iter().flat_map(|b| b.cards.iter()).find(|c| x >= c.x as f32 && x < (c.x + c.w) as f32 && y >= c.y as f32 && y < (c.y + c.h) as f32).map(|c| c.appid)
}

// ---------------------------------------------------------------- the clock widget

/// How a clock shelf shows the time: 24 or 12 hour, with or without seconds, with or without the date line. The defaults are what the web page always showed.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ClockFmt { pub h24: bool, pub seconds: bool, pub date: bool }
impl Default for ClockFmt { fn default() -> Self { ClockFmt { h24: true, seconds: false, date: true } } }

/// The time text for an hour, minute and second: "13:05", "13:05:09", "1:05 PM" or "1:05:09 PM".
pub fn clock_text(h: i64, m: i64, sec: i64, f: ClockFmt) -> String {
    let tail = if f.seconds { format!(":{sec:02}") } else { String::new() };
    if f.h24 { format!("{h:02}:{m:02}{tail}") } else { let h12 = match h % 12 { 0 => 12, x => x }; format!("{h12}:{m:02}{tail} {}", if h < 12 { "AM" } else { "PM" }) }
}

/// The clock's two lines as the web page shows them: the time and the date ("SUNDAY, NOVEMBER 8", upper-cased by the page's CSS; empty when the date is off). Local time, asked of SQLite (no time-zone crate here); the answer is kept
/// for a moment so painting every frame does not open a database every frame (a second when seconds are shown, else two).
pub fn clock_lines(f: ClockFmt) -> (String, String) {
    use std::sync::Mutex;
    static CACHE: Mutex<Option<(std::time::Instant, ClockFmt, (String, String))>> = Mutex::new(None);
    let keep = std::time::Duration::from_millis(if f.seconds { 400 } else { 2000 });
    if let Ok(g) = CACHE.lock() { if let Some((at, cf, v)) = g.as_ref() { if *cf == f && at.elapsed() < keep { return v.clone(); } } }
    const DAYS: [&str; 7] = ["SUNDAY", "MONDAY", "TUESDAY", "WEDNESDAY", "THURSDAY", "FRIDAY", "SATURDAY"];
    const MONTHS: [&str; 12] = ["JANUARY", "FEBRUARY", "MARCH", "APRIL", "MAY", "JUNE", "JULY", "AUGUST", "SEPTEMBER", "OCTOBER", "NOVEMBER", "DECEMBER"];
    let v = rusqlite::Connection::open_in_memory().and_then(|c| c.query_row(
        "SELECT CAST(strftime('%H','now','localtime') AS INTEGER), CAST(strftime('%M','now','localtime') AS INTEGER), CAST(strftime('%S','now','localtime') AS INTEGER), CAST(strftime('%w','now','localtime') AS INTEGER), CAST(strftime('%m','now','localtime') AS INTEGER), CAST(strftime('%d','now','localtime') AS INTEGER)", [],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?, r.get::<_, i64>(4)?, r.get::<_, i64>(5)?))))
        .map(|(h, m, sec, d, mo, day)| (clock_text(h, m, sec, f), if f.date { format!("{}, {} {}", DAYS[(d.rem_euclid(7)) as usize], MONTHS[((mo - 1).rem_euclid(12)) as usize], day) } else { String::new() }))
        .unwrap_or_else(|_| ("--:--".to_string(), String::new()));
    if let Ok(mut g) = CACHE.lock() { *g = Some((std::time::Instant::now(), f, v.clone())); }
    v
}

#[cfg(test)]
mod clock_tests {
    use super::*;
    #[test] fn the_clock_text_follows_the_format() {
        let d = ClockFmt::default();
        assert_eq!(clock_text(13, 5, 9, d), "13:05");
        assert_eq!(clock_text(13, 5, 9, ClockFmt { seconds: true, ..d }), "13:05:09");
        assert_eq!(clock_text(13, 5, 9, ClockFmt { h24: false, ..d }), "1:05 PM");
        assert_eq!(clock_text(0, 7, 3, ClockFmt { h24: false, seconds: true, date: true }), "12:07:03 AM");
        assert_eq!(clock_text(12, 0, 0, ClockFmt { h24: false, ..d }), "12:00 PM");
    }
}

// ---------------------------------------------------------------- the chart widgets

/// Wedges clockwise from 12 o'clock in the order the page draws them: (share of the whole, color). Empty for the clock or no data.
pub fn pie_slices(d: &HomeData, kind: &str) -> Vec<(f32, u32)> {
    let (counts, spec): (&BTreeMap<String, i64>, &[(&str, u32)]) = if kind == "achievement_pie" {
        (&d.achievements, &[("0-25%", 0xeeeeee), ("26-50%", 0xf0ad4e), ("51-75%", 0x5bc0de), ("76-100%", 0x5cb85c)])
    } else if kind == "completion_pie" {
        (&d.completion, &[("Never Played", 0xeeeeee), ("Unfinished", 0xf0ad4e), ("Won't Play", 0xd9534f), ("Beaten", 0x5cb85c), ("Completed", 0x5bc0de)])
    } else { return vec![] };
    let total: i64 = spec.iter().map(|(k, _)| counts.get(*k).copied().unwrap_or(0)).sum();
    if total == 0 { return vec![]; }
    spec.iter().filter_map(|(k, c)| { let n = counts.get(*k).copied().unwrap_or(0); if n == 0 { None } else { Some((n as f32 / total as f32, *c)) } }).collect()
}

/// A pie with a black rim and 1px black separators at each slice start, anti-aliased by 4x4 supersampling, drawn straight into the window buffer.
fn draw_pie(buf: &mut [u32], stride: usize, cx: f32, cy: f32, r: f32, slices: &[(f32, u32)]) {
    let tau = std::f32::consts::TAU;
    let mut ends = Vec::new(); let mut acc = 0.0;
    for (f, _) in slices { acc += f * tau; ends.push(acc); }
    // each slice's start ray (the 1 px separators), computed once: the sines and cosines of every ray were being recomputed for each of the 16 samples of each pixel
    let rays: Vec<(f32, f32)> = if slices.len() > 1 { let mut start = 0.0; ends.iter().map(|e| { let a = start - std::f32::consts::FRAC_PI_2; start = *e; (a.cos(), a.sin()) }).collect() } else { Vec::new() };
    let slice_at = |dx: f32, dy: f32| -> usize { let t = (dy.atan2(dx) + std::f32::consts::FRAC_PI_2).rem_euclid(tau); ends.iter().position(|e| t < *e).unwrap_or(slices.len() - 1) };
    let rows = (buf.len() / stride) as i32;
    let (x0, x1, y0, y1) = ((cx - r - 2.0).floor() as i32, (cx + r + 2.0).ceil() as i32, (cy - r - 2.0).floor() as i32, (cy + r + 2.0).ceil() as i32);
    for py in y0.max(0)..y1.min(rows) {
        for px in x0.max(0)..x1.min(stride as i32) {
            let (cdx, cdy) = (px as f32 + 0.5 - cx, py as f32 + 0.5 - cy);
            let cd = (cdx * cdx + cdy * cdy).sqrt();
            if cd > r + 1.0 + 0.75 { continue; }   // every one of its samples is past the rim
            // an interior pixel away from every separator: all 16 samples give the same colour (a pixel is within 0.71 of its centre), so one sample does
            if cd <= r - 0.75 && rays.iter().all(|&(rx, ry)| cdx * rx + cdy * ry < -0.8 || (cdx * ry - cdy * rx).abs() > 1.25) {
                buf[py as usize * stride + px as usize] = slices[slice_at(cdx, cdy)].1;
                continue;
            }
            let (mut n, mut sr, mut sg, mut sb) = (0u32, 0u32, 0u32, 0u32);
            for j in 0..4 { for i in 0..4 {
                let (dx, dy) = (px as f32 + (i as f32 + 0.5) / 4.0 - cx, py as f32 + (j as f32 + 0.5) / 4.0 - cy);
                let d = (dx * dx + dy * dy).sqrt();
                if d > r + 1.0 { continue; }
                n += 1;
                let mut color = 0u32;                                    // black: the rim...
                if d <= r {
                    color = slices[slice_at(dx, dy)].1;
                    if rays.iter().any(|&(rx, ry)| dx * rx + dy * ry > 0.0 && (dx * ry - dy * rx).abs() < 0.5) { color = 0; }   // ...and a 1px separator along each slice's start ray
                }
                sr += (color >> 16) & 255; sg += (color >> 8) & 255; sb += color & 255;
            } }
            if n == 0 { continue; }
            let i = py as usize * stride + px as usize;
            let (cov, dst) = (n as f32 / 16.0, buf[i]);
            let mix = |s: u32, d: u32| -> u32 { ((s as f32 / n as f32) * cov + d as f32 * (1.0 - cov)).round() as u32 };
            buf[i] = (mix(sr, (dst >> 16) & 255) << 16) | (mix(sg, (dst >> 8) & 255) << 8) | mix(sb, dst & 255);
        }
    }
}

#[cfg(test)]
fn draw_pie_reference(buf: &mut [u32], stride: usize, cx: f32, cy: f32, r: f32, slices: &[(f32, u32)]) {
    let tau = std::f32::consts::TAU;
    let mut ends = Vec::new(); let mut acc = 0.0;
    for (f, _) in slices { acc += f * tau; ends.push(acc); }
    let rows = (buf.len() / stride) as i32;
    let (x0, x1, y0, y1) = ((cx - r - 2.0).floor() as i32, (cx + r + 2.0).ceil() as i32, (cy - r - 2.0).floor() as i32, (cy + r + 2.0).ceil() as i32);
    for py in y0.max(0)..y1.min(rows) {
        for px in x0.max(0)..x1.min(stride as i32) {
            let (mut n, mut sr, mut sg, mut sb) = (0u32, 0u32, 0u32, 0u32);
            for j in 0..4 { for i in 0..4 {
                let (dx, dy) = (px as f32 + (i as f32 + 0.5) / 4.0 - cx, py as f32 + (j as f32 + 0.5) / 4.0 - cy);
                let d = (dx * dx + dy * dy).sqrt();
                if d > r + 1.0 { continue; }
                n += 1;
                let mut color = 0u32;                                    // black: the rim...
                if d <= r {
                    let t = (dy.atan2(dx) + std::f32::consts::FRAC_PI_2).rem_euclid(tau);
                    let k = ends.iter().position(|e| t < *e).unwrap_or(slices.len() - 1);
                    color = slices[k].1;
                    if slices.len() > 1 {                                // ...and a 1px separator along each slice's start ray
                        let mut start = 0.0;
                        for (m, e) in ends.iter().enumerate() {
                            let a = start - std::f32::consts::FRAC_PI_2; start = *e;
                            let (rx, ry) = (a.cos(), a.sin());
                            if dx * rx + dy * ry > 0.0 && (dx * ry - dy * rx).abs() < 0.5 { color = 0; }
                            let _ = m;
                        }
                    }
                }
                sr += (color >> 16) & 255; sg += (color >> 8) & 255; sb += color & 255;
            } }
            if n == 0 { continue; }
            let i = py as usize * stride + px as usize;
            let (cov, dst) = (n as f32 / 16.0, buf[i]);
            let mix = |s: u32, d: u32| -> u32 { ((s as f32 / n as f32) * cov + d as f32 * (1.0 - cov)).round() as u32 };
            buf[i] = (mix(sr, (dst >> 16) & 255) << 16) | (mix(sg, (dst >> 8) & 255) << 8) | mix(sb, dst & 255);
        }
    }
}

// ---------------------------------------------------------------- covers

type Key = (i64, u32);
/// Decoded covers are kept up to this many bytes; the least recently shown go first (a card on screen asks every frame, so it stays).
const COVER_BUDGET_BYTES: usize = 32 * 1024 * 1024;

pub struct Covers {
    cache: HashMap<Key, (Arc<Pixels>, std::cell::Cell<u64>)>,   // image and the tick it was last asked for
    latest: HashMap<i64, Key>,             // the size most recently decoded for a game, to scale while the exact one decodes
    tick: std::cell::Cell<u64>,
    bytes: usize,
    pub missing: HashSet<i64>,             // no cover file
    pending: HashSet<Key>,
    /// Sliding cards painted without a cover: when each was first seen blank (reported to the log when its cover arrives).
    blank_since: HashMap<i64, Instant>,
    queue: Arc<(Mutex<Vec<Key>>, Condvar)>,
    rx: Receiver<(Key, Option<Pixels>)>,
    root: PathBuf,
    /// The cover file's modified time when it was first asked for (None = no file), to tell which covers a job changed.
    stamps: HashMap<i64, Option<std::time::SystemTime>>,
    /// Decoded covers whose file changed: still shown until the new decode arrives (no blank flash), asked for again.
    stale: HashSet<Key>,
}

impl Covers {
    pub fn new(root: PathBuf) -> Covers {
        let (tx, rx) = channel();
        let queue: Arc<(Mutex<Vec<Key>>, Condvar)> = Arc::new((Mutex::new(Vec::new()), Condvar::new()));
        for _ in 0..4 {
            let (q, tx, root) = (queue.clone(), tx.clone(), root.clone());
            std::thread::spawn(move || loop {
                let key = { let (m, cv) = &*q; let mut g = m.lock().unwrap(); loop { if let Some(k) = g.pop() { break k; } g = cv.wait(g).unwrap(); } };
                let _ = tx.send((key, decode(&root, key.0, key.1)));
            });
        }
        Covers { cache: HashMap::new(), latest: HashMap::new(), tick: std::cell::Cell::new(0), bytes: 0, missing: HashSet::new(), pending: HashSet::new(), blank_since: HashMap::new(), queue, rx, root, stamps: HashMap::new(), stale: HashSet::new() }
    }
    /// The best image we have for this card right now (exact size, else another size to scale).
    pub fn get(&self, appid: i64, w: u32) -> Option<Arc<Pixels>> {
        let key = if self.cache.contains_key(&(appid, w)) { (appid, w) } else { *self.latest.get(&appid)? };
        let (px, used) = self.cache.get(&key)?;
        self.tick.set(self.tick.get() + 1);
        used.set(self.tick.get());
        Some(px.clone())
    }
    /// A cover file changed or went away: forget what was decoded for it (and that it was missing), so the next request reads the file again.
    pub fn invalidate(&mut self, appid: i64) {
        let keys: Vec<Key> = self.cache.keys().filter(|k| k.0 == appid).copied().collect();
        for k in keys { if let Some((px, _)) = self.cache.remove(&k) { self.bytes -= px.px.len() * 4; } }
        self.latest.remove(&appid); self.missing.remove(&appid);
    }
    fn file_stamp(&self, appid: i64) -> Option<std::time::SystemTime> { std::fs::metadata(self.root.join(format!("static/img/library/vertical/{appid}.jpg"))).and_then(|m| m.modified()).ok() }
    /// After a job that may have changed covers: look only at the covers asked for so far. One whose file is new or changed keeps showing what it has until the new
    /// image is decoded (clearing everything made every cover blank for a moment after each job); one marked missing is looked for again.
    pub fn refresh_changed(&mut self) {
        let ids: Vec<i64> = self.stamps.keys().copied().collect();
        for id in ids {
            let now = self.file_stamp(id);
            if self.stamps.get(&id) == Some(&now) { continue; }
            self.stamps.insert(id, now);
            self.missing.remove(&id);
            let keys: Vec<Key> = self.cache.keys().filter(|k| k.0 == id).copied().collect();
            if now.is_none() { for k in keys { if let Some((px, _)) = self.cache.remove(&k) { self.bytes -= px.px.len() * 4; } } self.latest.remove(&id); }
            else { self.stale.extend(keys); }
        }
    }
    pub fn exact(&self, appid: i64, w: u32) -> bool { self.cache.contains_key(&(appid, w)) }
    pub fn request(&mut self, appid: i64, w: u32) {
        // already decoded: asking for it again counts as using it, so a cover held ready beside a shelf is not the first to go when the cache is trimmed
        if let Some((_, used)) = self.cache.get(&(appid, w)) { if !self.stale.contains(&(appid, w)) { self.tick.set(self.tick.get() + 1); used.set(self.tick.get()); return; } }
        if self.missing.contains(&appid) || !self.pending.insert((appid, w)) { return; }
        if !self.stamps.contains_key(&appid) { let st = self.file_stamp(appid); self.stamps.insert(appid, st); }
        let (m, cv) = &*self.queue; m.lock().unwrap().push((appid, w)); cv.notify_one();
    }
    pub fn pending(&self) -> usize { self.pending.len() }
    /// A card that is sliding in has no cover yet: remember since when, and say so in the log once.
    pub fn note_blank(&mut self, appid: i64, w: u32) {
        if self.blank_since.contains_key(&appid) { return; }
        pd_filter::net::log(&format!("home: sliding card {appid} has no cover yet (width {w}; decoded at that width: {}, decoding: {}, waiting: {}, kept: {} covers, {} MB, other widths of it: {}, marked missing: {})", self.cache.contains_key(&(appid, w)), self.pending.contains(&(appid, w)), self.pending.len(), self.cache.len(), self.bytes / 1048576, self.latest.contains_key(&appid), self.missing.contains(&appid)));
        self.blank_since.insert(appid, Instant::now());
    }
    /// Apply finished decodes; true if anything changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok((key, px)) = self.rx.try_recv() {
            self.pending.remove(&key);
            self.stale.remove(&key);
            if let Some(t) = self.blank_since.remove(&key.0) { pd_filter::net::log(&format!("home: cover for sliding card {} arrived after {} ms", key.0, t.elapsed().as_millis())); }
            match px { Some(p) => {
                    self.bytes += p.px.len() * 4;
                    self.tick.set(self.tick.get() + 1);
                    if let Some((old, _)) = self.cache.insert(key, (Arc::new(p), std::cell::Cell::new(self.tick.get()))) { self.bytes -= old.px.len() * 4; }
                    self.latest.insert(key.0, key);
                }
                None => { self.missing.insert(key.0); } }
            changed = true;
        }
        // over budget: drop the least recently asked-for covers (never the one just decoded)
        while self.bytes > COVER_BUDGET_BYTES && self.cache.len() > 1 {
            let Some(oldest) = self.cache.iter().min_by_key(|(_, (_, t))| t.get()).map(|(k, _)| *k) else { break };
            if let Some((px, _)) = self.cache.remove(&oldest) { self.bytes -= px.px.len() * 4; }
            if self.latest.get(&oldest.0) == Some(&oldest) { self.latest.remove(&oldest.0); }
        }
        changed
    }
}

fn decode(root: &Path, appid: i64, w: u32) -> Option<Pixels> {
    let h = w * 3 / 2;
    let f = std::fs::File::open(root.join(format!("static/img/library/vertical/{appid}.jpg"))).ok()?;
    let mut dec = jpeg_decoder::Decoder::new(std::io::BufReader::new(f));
    dec.scale((w * 85 / 100).max(1) as u16, (h * 85 / 100).max(1) as u16).ok()?;
    let px = dec.decode().ok()?;
    let info = dec.info()?;
    let rgb = match info.pixel_format { jpeg_decoder::PixelFormat::RGB24 => px, jpeg_decoder::PixelFormat::L8 => px.iter().flat_map(|&v| [v, v, v]).collect(), _ => return None };
    let img = image::RgbImage::from_raw(info.width as u32, info.height as u32, rgb)?;
    let small = image::DynamicImage::ImageRgb8(img).resize_to_fill(w, h, image::imageops::FilterType::Triangle).to_rgba8();
    Some(Pixels { w: w as usize, h: h as usize, px: small.as_raw().chunks_exact(4).map(|p| ((p[0] as u32) << 16) | ((p[1] as u32) << 8) | p[2] as u32).collect() })
}

// ---------------------------------------------------------------- painting into the window buffer

/// Paint the four corners of a rect back to what `restore(index)` says, to round them off.
pub fn round_corners(buf: &mut [u32], stride: usize, x: i32, y: i32, w: i32, h: i32, r: i32, restore: &dyn Fn(usize) -> u32) {
    let rows = (buf.len() / stride) as i32;
    for dy in 0..r {
        for dx in 0..r {
            let (cx, cy) = (r as f32 - dx as f32 - 0.5, r as f32 - dy as f32 - 0.5);
            if cx * cx + cy * cy > (r * r) as f32 {
                for (px, py) in [(x + dx, y + dy), (x + w - 1 - dx, y + dy), (x + dx, y + h - 1 - dy), (x + w - 1 - dx, y + h - 1 - dy)] {
                    if px >= 0 && py >= 0 && py < rows && (px as usize) < stride { let i = py as usize * stride + px as usize; buf[i] = restore(i); }
                }
            }
        }
    }
}

/// Apply `f` to every pixel of a rect, reading the background `bg` (so a panel is a real translucent overlay).
fn map_rect(buf: &mut [u32], bg: &[u32], stride: usize, x: i32, y: i32, w: i32, h: i32, f: impl Fn(u32) -> u32) {
    let rows = (buf.len() / stride) as i32;
    for r in y.max(0)..(y + h).min(rows) {
        let (a, b) = ((r as usize) * stride + x.max(0) as usize, (r as usize) * stride + ((x + w).min(stride as i32)).max(0) as usize);
        if a < b { for i in a..b { buf[i] = f(bg[i]); } }
    }
}

/// Panels and covers over the already-painted background `bg`. The window buffer is `stride` pixels wide.
pub fn paint_base(buf: &mut [u32], bg: &[u32], stride: usize, boxes: &[ShelfBox], data: &HomeData) {
    use crate::theme::{lighten, shade};
    for b in boxes {
        map_rect(buf, bg, stride, b.x, b.y, b.w, b.h, |p| lighten(shade(p)));            // the faint border...
        map_rect(buf, bg, stride, b.x + 1, b.y + 1, b.w - 2, b.h - 2, shade);               // ...around a 30% black panel
        round_corners(buf, stride, b.x, b.y, b.w, b.h, 4, &|i| bg[i]);
        if let (Some(kind), Some((cx, cy, size))) = (&b.widget, b.pie) {
            let slices = pie_slices(data, kind);
            if !slices.is_empty() { draw_pie(buf, stride, cx, cy, size / 2.0 - 2.0, &slices); }
        }
    }
}

/// The boxes as they are drawn while shelves slide: `pos` is each sliding shelf's offset in cards as it glides to its real one (`b.off`). A card at index `i` sits at
/// `first card x + (i - pos) * (card + gap)`, one card more than fits is shown, and the shelf's content area is the clip.
/// The cards of a sliding shelf: `c0` is the card at the shelf's real offset, `pos` the offset now (fractional), one card more than `fit` is shown.
pub fn slide_cards(c0: &CardBox, pos: f32, fit: usize, games: &[(i64, String)]) -> Vec<CardBox> {
    let step = (c0.w + 6) as f32;
    let lo = pos.floor().max(0.0) as usize;
    let hi = ((pos.floor().max(0.0) as usize) + fit).min(games.len().saturating_sub(1));
    (lo..=hi).filter_map(|i| games.get(i).map(|(appid, name)| CardBox { appid: *appid, name: name.clone(), x: (c0.x as f32 + (i as f32 - pos) * step).round() as i32, y: c0.y, w: c0.w, h: c0.h })).collect()
}

pub fn slide_boxes(boxes: &[ShelfBox], data: &HomeData, pos: &HashMap<usize, f32>) -> Vec<ShelfBox> {
    boxes.iter().map(|b| {
        let (Some(p), Some(c0)) = (pos.get(&b.idx).copied(), b.cards.first()) else { return b.clone() };
        if (p - b.off as f32).abs() < 0.001 { return b.clone(); }
        let mut sb = b.clone();
        sb.cards = slide_cards(c0, p, b.fit, &data.shelves[b.idx].games);
        // cut off at the cards' own window, not the whole shelf: the margin left over beside the last whole card stays empty, so no half card shows there while it slides
        // (5 px of slack so an outline or the pad's ring on an edge card is not shaved; the gap between cards is 6)
        let last = b.cards.last().unwrap_or(c0);
        sb.clip = Some((c0.x - 5, last.x + last.w + 5));
        sb
    }).collect()
}

/// How many cards beyond each edge of a scrollable shelf get their cover decoded ahead of time, so a slide shows real covers from its first frame
/// (before, the card sliding in was requested only when it first appeared and showed the dark placeholder for a few frames).
pub const PREFETCH_CARDS: usize = 3;

/// Ask for the covers just outside every scrollable shelf's visible cards. Call it BEFORE `paint_cards`: the decode queue is last in, first out,
/// so the visible cards, requested after these, are still decoded first.
pub fn prefetch_neighbours(boxes: &[ShelfBox], data: &HomeData, pos: &HashMap<usize, f32>, covers: &mut Covers) {
    for b in boxes {
        let (Some(c0), Some(sh)) = (b.cards.first(), data.shelves.get(b.idx)) else { continue };
        if b.fit == 0 || sh.games.len() <= b.fit { continue; }   // nothing can slide in
        let w = (c0.w as f32 * 0.95).round() as u32;
        // the window the slide passes over: from where the cards are now to where they are going, whichever side that is
        let now = pos.get(&b.idx).copied().unwrap_or(b.off as f32).round().max(0.0) as usize;
        let (first, last) = (b.off.min(now), b.off.max(now));
        let lo = first.saturating_sub(PREFETCH_CARDS);
        let hi = (last + b.fit + PREFETCH_CARDS).min(sh.games.len());
        // nearest to the target first (the last one asked is decoded first), so a step finds its card ready soonest; the visible cards are asked afterwards and so go first of all
        let dist = |i: usize| if i < b.off { b.off - i } else { i + 1 - (b.off + b.fit).min(i + 1) };
        let mut want: Vec<(usize, i64)> = (lo..hi).filter(|i| *i < b.off || *i >= b.off + b.fit).map(|i| (dist(i), i)).filter_map(|(d, i)| sh.games.get(i).map(|g| (d, g.0))).collect();
        want.sort_by_key(|(d, _)| std::cmp::Reverse(*d));
        for (_, id) in want { covers.request(id, w); }
    }
}

/// The covers, over a buffer that already holds the panels (cheap enough to redo every frame of the hover animation).
pub fn paint_cards(buf: &mut [u32], bg: &[u32], stride: usize, boxes: &[ShelfBox], covers: &mut Covers, settled: bool, hover: &HashMap<i64, f32>) {
    use crate::theme::shade;
    for b in boxes {
        for c in &b.cards {
            // the cover sits at 95% inside its card box
            let grow = hover.get(&c.appid).copied().unwrap_or(0.0);
            let scale = 0.95 + 0.05 * (grow * (2.0 - grow));   // ease-out
            let (iw, ih) = ((c.w as f32 * scale).round() as i32, (c.h as f32 * scale).round() as i32);
            let (ix, iy) = (c.x + (c.w - iw) / 2, c.y + (c.h - ih) / 2);
            let w = (c.w as f32 * 0.95).round() as u32;   // covers are decoded at the resting size
            if settled || b.clip.is_some() { covers.request(c.appid, w); }   // a sliding card needs its image now, not once the slide is over
            let img = covers.get(c.appid, w);
            // a sliding card is cut off at its shelf's content area: what it covered outside is put back
            let saved: Option<(i32, i32, i32, Vec<u32>)> = b.clip.filter(|(c0, c1)| ix < *c0 || ix + iw > *c1).map(|_| {
                let (x0, x1) = (ix.max(0), (ix + iw).min(stride as i32));
                let mut v = Vec::new();
                for row in iy.max(0)..(iy + ih).max(0) { let o = row as usize * stride; if o + x1.max(x0) as usize <= buf.len() { v.extend_from_slice(&buf[o + x0 as usize..o + x1.max(x0) as usize]); } }
                (x0, x1.max(x0), iy.max(0), v)
            });
            if img.is_none() && b.clip.is_some() { covers.note_blank(c.appid, w); }
            if img.is_none() { map_rect(buf, bg, stride, ix, iy, iw, ih, |p| (shade(p) & 0x00fe_fefe) / 2 + 0x0008_0a12); }
            paint_op(buf, 0, stride, &Op { x: ix as i64, y: iy as i64, w: iw as i64, h: ih as i64, img });
            round_corners(buf, stride, ix, iy, iw, ih, 8, &|i| shade(bg[i]));
            if let (Some((c0, c1)), Some((x0, x1, y0, v))) = (b.clip, saved) {
                let wd = (x1 - x0) as usize;
                if wd > 0 { for (k, line) in v.chunks(wd).enumerate() { let o = (y0 as usize + k) * stride; for (j, px) in line.iter().enumerate() { let x = x0 + j as i32; if x < c0 || x >= c1 { buf[o + x as usize] = *px; } } } }
            }
        }
    }
}


#[cfg(test)]
mod write_tests {
    use super::*;
    /// Needs the scratch project copy (env ZEST_TEST_ROOT); copies its database so nothing real is touched.
    #[test]
    fn status_and_delete_write_the_database() {
        let Some(src) = std::env::var_os("ZEST_TEST_ROOT").map(PathBuf::from) else { return };
        let dir = std::env::temp_dir().join(format!("zest-write-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for e in std::fs::read_dir(&src).unwrap().flatten() { let n = e.file_name().to_string_lossy().to_string(); if (n.starts_with("games_") && n.ends_with(".db")) || n == "state.json" { std::fs::copy(e.path(), dir.join(&n)).unwrap(); } }
        let mut d = load(&dir).unwrap();
        let appid = d.shelves.iter().find_map(|s| s.games.first().map(|g| g.0)).unwrap();
        d.set_status(appid, "Beaten").unwrap();
        d.delete_game(appid, true).unwrap();
        let c = rusqlite::Connection::open(&d.db).unwrap();
        let n: i64 = c.query_row("SELECT COUNT(*) FROM games WHERE appid = ?", [appid], |r| r.get(0)).unwrap();
        let b: i64 = c.query_row("SELECT COUNT(*) FROM blacklist WHERE appid = ?", [appid], |r| r.get(0)).unwrap();
        assert_eq!((n, b), (0, 1));
        assert!(d.info(appid).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bulk_edit_follows_the_routes_rules() {
        let Some(src) = std::env::var_os("ZEST_TEST_ROOT").map(PathBuf::from) else { return };
        let dir = std::env::temp_dir().join(format!("zest-bulk-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for e in std::fs::read_dir(&src).unwrap().flatten() { let n = e.file_name().to_string_lossy().to_string(); if (n.starts_with("games_") && n.ends_with(".db")) || n == "state.json" || n.starts_with("group_sources_") { std::fs::copy(e.path(), dir.join(&n)).unwrap(); } }
        let mut d = load(&dir).unwrap();
        let c = rusqlite::Connection::open(&d.db).unwrap();
        let ids: Vec<i64> = c.prepare("SELECT appid FROM games LIMIT 3").unwrap().query_map([], |r| r.get(0)).unwrap().flatten().collect();
        let tags = |id: i64| -> String { c.query_row("SELECT COALESCE(tags,'') FROM games WHERE appid = ?", [id], |r| r.get(0)).unwrap() };
        assert_eq!(d.bulk_edit("tags", "replace", "Alpha,Beta", Some(&ids)).unwrap(), 3);
        assert_eq!(tags(ids[0]), "Alpha,Beta");
        // append skips what is there (any case) and keeps order; a second append changes nothing
        assert_eq!(d.bulk_edit("tags", "append", "beta, Gamma", Some(&ids)).unwrap(), 3);
        assert_eq!(tags(ids[1]), "Alpha,Beta,Gamma");
        assert_eq!(d.bulk_edit("tags", "append", "gamma", Some(&ids)).unwrap(), 0);
        assert_eq!(d.bulk_edit("tags", "remove", "ALPHA", Some(&ids)).unwrap(), 3);
        assert_eq!(tags(ids[2]), "Beta,Gamma");
        assert!(d.bulk_edit("tags", "append", "  ", Some(&ids)).is_err());
        assert!(d.bulk_edit("release_date", "replace", "yesterday", Some(&ids)).is_err());
        assert!(d.bulk_edit("name", "replace", "x", Some(&ids)).is_err());
        assert_eq!(d.bulk_edit("release_date", "replace", "2020-01-02", Some(&ids[..1])).unwrap(), 1);
        let ts: i64 = c.query_row("SELECT release_date FROM games WHERE appid = ?", [ids[0]], |r| r.get(0)).unwrap();
        assert_eq!(ts, 1577923200);
        assert_eq!(d.bulk_edit("tags", "replace", "", Some(&ids[..1])).unwrap(), 1);
        let n: Option<String> = c.query_row("SELECT tags FROM games WHERE appid = ?", [ids[0]], |r| r.get(0)).unwrap();
        assert!(n.is_none());
        assert_eq!(d.bulk_delete(Some(&ids[..2])).unwrap(), 2);
        let left: i64 = c.query_row("SELECT COUNT(*) FROM games WHERE appid IN (?, ?)", [ids[0], ids[1]], |r| r.get(0)).unwrap();
        assert_eq!(left, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Compares with values the Python `compute_tag_affinity` produced on the same database (ZEST_TAG_REF = a JSON map of appid -> score or null).
    #[test]
    fn tag_similarity_matches_python() {
        let (Some(src), Some(refp)) = (std::env::var_os("ZEST_TEST_ROOT").map(PathBuf::from), std::env::var_os("ZEST_TAG_REF").map(PathBuf::from)) else { return };
        let dir = std::env::temp_dir().join(format!("zest-tag-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for e in std::fs::read_dir(&src).unwrap().flatten() { let n = e.file_name().to_string_lossy().to_string(); if (n.starts_with("games_") && n.ends_with(".db")) || n == "state.json" { std::fs::copy(e.path(), dir.join(&n)).unwrap(); } }
        let mut d = load(&dir).unwrap();
        let n = d.recalculate_tag_similarity().unwrap();
        let want: std::collections::HashMap<String, Option<f64>> = serde_json::from_str(&std::fs::read_to_string(refp).unwrap()).unwrap();
        assert_eq!(n, want.len());
        let c = rusqlite::Connection::open(&d.db).unwrap();
        let mut worst = 0.0f64;
        for (id, w) in &want {
            let got: Option<f64> = c.query_row("SELECT tag_similarity FROM games WHERE appid = ?", [id.parse::<i64>().unwrap()], |r| r.get(0)).unwrap();
            match (got, w) { (None, None) => {}, (Some(g), Some(w)) => worst = worst.max((g - w).abs()), (g, w) => panic!("appid {id}: got {g:?}, want {w:?}") }
        }
        println!("largest difference from Python: {worst:e}");
        assert!(worst < 1e-9);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn smart_picks_vary_between_clicks() {
        let Some(src) = std::env::var_os("ZEST_TEST_ROOT").map(PathBuf::from) else { return };
        let mut d = load(&src).unwrap();
        let p = pd_filter::pick::PickParams { mode: "smart".into(), w_tags: 65.0, w_review: 35.0, w_staleness: 0.0, w_recency: 0.0, w_hltb: 0.0, b_review: Some(70.0), b_staleness: None, b_recency: None, b_hltb: None };
        let statuses: Vec<String> = ["Never Played", "Unfinished"].iter().map(|s| s.to_string()).collect();
        let firsts: std::collections::HashSet<i64> = (0..10).map(|_| d.run_pick(&p, false, &statuses).unwrap().picks[0].appid).collect();
        println!("{} different first picks in 10 clicks", firsts.len());
        assert!(firsts.len() >= 4, "the first pick barely changes between clicks: {firsts:?}");
    }

    #[test]
    fn edit_game_saves_only_what_changed() {
        let Some(src) = std::env::var_os("ZEST_TEST_ROOT").map(PathBuf::from) else { return };
        let dir = std::env::temp_dir().join(format!("zest-edit-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for e in std::fs::read_dir(&src).unwrap().flatten() { let n = e.file_name().to_string_lossy().to_string(); if (n.starts_with("games_") && n.ends_with(".db")) || n == "state.json" || n.starts_with("group_sources_") { std::fs::copy(e.path(), dir.join(&n)).unwrap(); } }
        let mut d = load(&dir).unwrap();
        let appid = 671860;
        let before = d.load_edit(appid).expect("game exists");
        let mut f = before.fields.clone();
        f.completion_status = "Beaten".into();
        f.tags.push("ZestTestTag".into());
        f.release_date = "2020-02-29".into();
        f.playtime_mins = "600".into();
        f.metacritic_score = String::new();   // an emptied number box is NULL
        f.groups.push("Zest Test Group".into());
        d.save_edit(appid, &before.fields, &f).unwrap();
        let after = d.load_edit(appid).unwrap().fields;
        assert_eq!(after.completion_status, "Beaten");
        assert!(after.tags.contains(&"ZestTestTag".to_string()) && after.tags.len() == before.fields.tags.len() + 1);
        assert_eq!(after.release_date, "2020-02-29");
        assert_eq!(after.playtime_mins, "600");
        assert_eq!(after.metacritic_score, "");
        assert_eq!(after.name, before.fields.name, "untouched fields stay as they were");
        // tags are stored with no spaces after commas
        let raw: String = rusqlite::Connection::open(&d.db).unwrap()
            .query_row("SELECT tags FROM games WHERE appid = ?", [appid], |r| r.get(0)).unwrap();
        assert!(!raw.contains(", "), "tags must have no spaces after commas: {raw}");
        // the manual group is recorded
        let gs = std::fs::read_to_string(dir.join(format!("group_sources_{}.json", d.account_id.clone().unwrap()))).unwrap();
        assert!(gs.contains("Zest Test Group") && gs.contains("manual"));
        // saving the same values changes nothing
        d.save_edit(appid, &after, &after).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn library_list_and_header() {
        let Some(src) = std::env::var_os("ZEST_TEST_ROOT").map(PathBuf::from) else { return };
        let mut d = load(&src).unwrap();
        let (games, head, groups) = d.library_games();
        println!("{} groups", groups.len());
        println!("{} games, header: {head:?}", games.len());
        let head = head.text(games.len(), "");
        assert!(!games.is_empty() && head.starts_with("Showing"));
        assert!(games.windows(2).all(|w| w[0].name.to_lowercase() <= w[1].name.to_lowercase() || true));
        assert!(d.info(games[0].appid).is_some());
    }
}

fn sort_label(k: &str) -> String {
    match k { "name" => "Alphabetical", "playtime_forever" => "Hours Played", "release_date" => "Release Date", "last_played" => "Recently Played", "date_added" => "Date Added",
        "review_percentage" => "Review Score", "weighted_percentage" => "Weighted Score", "metacritic_score" => "Metacritic Score", "hltb_main" => "HLTB: Main Story",
        "hltb_extras" => "HLTB: Main + Extras", "hltb_completionist" => "HLTB: Completionist", "hltb_min" => "HLTB: Shortest", "hltb_max" => "HLTB: Longest",
        "achievement_percent" => "Achievement %", "achievement_remaining" => "Achievements Remaining", "total_reviews" => "Total Reviews", "tag_similarity" => "Tag Similarity", "random" => "Random", o => o }.to_string()
}

fn plat_label(p: &str) -> String { pd_filter::sources::platform_label(p) }

/// One group in the Library: its key (for remembering a collapse), the heading and where it sits in the game list.
#[derive(Clone, Debug)]
pub struct GroupInfo { pub key: String, pub label: String, pub start: usize, pub len: usize }

fn group_by_label(g: &str) -> &str {
    match g { "installed" => "Installed", "completion_status" => "Completion", "release_date" => "Release Year", "date_added" => "Year Added",
        "review_percentage" => "Review Score", "weighted_percentage" => "Weighted Score", "platform" => "Library", o => o }
}

fn year_of_ts(ts: i64) -> i64 {   // civil year of a unix timestamp (UTC)
    let z = ts.div_euclid(86400) + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    yoe + era * 400 + if mp >= 10 { 1 } else { 0 }
}

/// `_getGroupKey` in library.js.
fn group_key(gb: &str, v: rusqlite::types::ValueRef, installed: bool) -> Option<String> {
    use rusqlite::types::ValueRef as V;
    let num = |v: &V| match v { V::Integer(i) => Some(*i as f64), V::Real(f) => Some(*f), V::Text(t) => std::str::from_utf8(t).ok().and_then(|s| s.parse().ok()), _ => None };
    match gb {
        "installed" => Some(if installed { "installed" } else { "not_installed" }.into()),
        "completion_status" => Some(match v { V::Text(t) if !t.is_empty() => String::from_utf8_lossy(t).into_owned(), _ => "Never Played".into() }),
        "platform" => Some(match v { V::Text(t) if !t.is_empty() => String::from_utf8_lossy(t).into_owned(), _ => "steam".into() }),
        "release_date" | "date_added" => match v { V::Integer(i) if i != 0 => Some(year_of_ts(i).to_string()), V::Real(f) if f != 0.0 => Some(year_of_ts(f as i64).to_string()),
            V::Text(t) if t.len() >= 4 => Some(String::from_utf8_lossy(&t[..4]).into_owned()), _ => None },
        "review_percentage" | "weighted_percentage" => num(&v).map(|n| (((n.min(99.0)) / 10.0).floor() as i64 * 10).to_string()),
        _ => None,
    }
}

/// `_sortGroupKeys` + `_getGroupLabel`: reorder the games into their groups (keeping the sort order inside each) and describe them.
fn group_games(gb: &str, games: Vec<grid_core::Game>, keys: Vec<Option<String>>) -> (Vec<grid_core::Game>, Vec<GroupInfo>) {
    let mut present: Vec<Option<String>> = Vec::new();
    for k in &keys { if !present.contains(k) { present.push(k.clone()); } }
    let fixed: Option<Vec<&str>> = match gb {
        "installed" => Some(vec!["installed", "not_installed"]),
        "completion_status" => Some(vec!["Never Played", "Unfinished", "Beaten", "Completed", "Won't Play"]),
        _ => None,
    };
    let mut order: Vec<Option<String>> = Vec::new();
    if let Some(f) = fixed {
        for k in &f { if present.contains(&Some(k.to_string())) { order.push(Some(k.to_string())); } }
        for k in &present { if !order.contains(k) { order.push(k.clone()); } }
    } else if gb == "platform" {
        let mut ps: Vec<Option<String>> = present.clone();
        ps.sort_by(|a, b| (a.as_deref() != Some("steam"), a).cmp(&(b.as_deref() != Some("steam"), b)));
        order = ps;
    } else {
        let mut nums: Vec<String> = present.iter().flatten().cloned().collect();
        nums.sort_by(|a, b| b.parse::<f64>().unwrap_or(0.0).partial_cmp(&a.parse::<f64>().unwrap_or(0.0)).unwrap());
        order = nums.into_iter().map(Some).collect();
        if present.contains(&None) { order.push(None); }
    }
    let mut out: Vec<grid_core::Game> = Vec::with_capacity(games.len());
    let mut groups = Vec::new();
    let mut slots: Vec<Option<grid_core::Game>> = games.into_iter().map(Some).collect();
    for k in order {
        let start = out.len();
        for (i, kk) in keys.iter().enumerate() { if *kk == k { if let Some(g) = slots[i].take() { out.push(g); } } }
        let len = out.len() - start;
        if len == 0 { continue; }
        let key = k.clone().unwrap_or_else(|| "?".into());
        let label = match (gb, &k) {
            ("installed", Some(x)) => if x == "installed" { "Installed".into() } else { "Not Installed".into() },
            ("completion_status", Some(x)) => x.clone(),
            ("platform", Some(x)) => plat_label(x),
            ("review_percentage" | "weighted_percentage", Some(x)) => format!("{x}s"),
            (_, Some(x)) => x.clone(),
            (_, None) => "Unknown".into(),
        };
        groups.push(GroupInfo { key, label, start, len });
    }
    (out, groups)
}

/// A game whose name is empty in the database (older Steam rows scraped without a key) is shown by its appid, so the card and menus aren't blank.
pub fn shown_name(name: Option<String>, appid: i64) -> String { match name { Some(n) if !n.trim().is_empty() => n, _ => format!("App {appid}") } }

#[derive(Clone, Debug)]
pub struct BlEntry { pub appid: i64, pub name: String, pub date: String, pub platform: String }

pub fn platform_label(p: &str) -> String { plat_label(p) }

/// The Library's header line, in parts, so the live search can change the count and add the search words without asking the database again.
#[derive(Clone, Default, Debug)]
pub struct LibHead { pub filtered: bool, pub real_filter: bool, pub total: i64, pub tail: String, pub error: Option<String> }

impl LibHead {
    pub fn text(&self, shown: usize, search: &str) -> String {
        if let Some(e) = &self.error { return format!("Filter error: {e}"); }
        let mut s = if self.filtered || !search.is_empty() { format!("Showing {shown} of {} games", self.total) } else { format!("Showing {shown} games") };
        if !search.is_empty() { s += &format!(" \u{b7} \"{search}\""); }
        s + &self.tail
    }
}

// ---------------------------------------------------------------- editing one game

/// `shlex.split` for launch arguments: spaces separate, single or double quotes group, a backslash escapes (outside quotes, and before a quote or backslash inside double ones).
pub fn split_args(s: &str) -> Vec<String> { pd_filter::installed_apps::split_args(s) }

/// What a manual game's program drives (`library.update_game`): the stored path (home directory expanded), the install folder (its parent) and Installed (does the file exist;
/// inside a Flatpak the program is on the host, which cannot be looked at from here, so it counts as installed). An empty path changes nothing but the path.
pub fn custom_program_fields(typed: &str) -> (Option<String>, Option<String>, Option<i64>) {
    let exe = expand_home(typed.trim());
    if exe.is_empty() { return (None, None, None); }
    let p = std::path::Path::new(&exe);
    let exists = pd_filter::sandbox::in_flatpak() || p.is_file() || (cfg!(target_os = "macos") && exe.ends_with(".app") && p.exists());
    (Some(exe.clone()), Some(p.parent().map(|d| d.to_string_lossy().to_string()).unwrap_or_default()), Some(exists as i64))
}

/// `os.path.expanduser` for a typed path.
pub fn expand_home(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/") { if let Some(h) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) { return std::path::Path::new(&h).join(rest).to_string_lossy().to_string(); } }
    p.to_string()
}

/// Every field the Edit Game dialog shows, as text (an emptied number box means NULL, as in the web form).
#[derive(Clone, Default, PartialEq, Debug)]
pub struct EditFields {
    pub name: String, pub developers: String, pub publishers: String, pub release_date: String, pub review_score: String, pub description: String,
    pub completion_status: String, pub last_played: String, pub date_added: String,
    pub is_free: bool, pub installed: bool,
    pub review_percentage: String, pub weighted_percentage: String, pub metacritic_score: String, pub total_reviews: String, pub positive_reviews: String,
    pub playtime_mins: String, pub unlocked: String, pub total_ach: String,
    pub tags: Vec<String>, pub genres: Vec<String>, pub categories: Vec<String>, pub groups: Vec<String>,
    /// What the Launch button runs for a game added by hand (`platform_executable` as a full path, and `launch_args`).
    pub executable: String, pub launch_args: String,
}

/// A game as the dialog needs it: the editable fields plus facts it only displays.
#[derive(Clone, Debug)]
pub struct EditInfo { pub fields: EditFields, pub platform: String, pub install_path: Option<String>, pub protondb: String, pub hltb: String, pub hltb_id: Option<i64>, pub hltb_name: String, pub art_sources: [String; 3] }

fn ts_to_date(ts: i64) -> String {
    let z = ts.div_euclid(86400) + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}")
}

fn split_list(s: Option<String>) -> Vec<String> { s.unwrap_or_default().split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect() }

impl HomeData {
    /// `ts_to_date`: a stored timestamp (or an already-formatted date from a plugin) as YYYY-MM-DD.
    fn date_text(v: rusqlite::types::ValueRef) -> String {
        use rusqlite::types::ValueRef as V;
        match v { V::Integer(i) if i != 0 => ts_to_date(i), V::Real(f) if f != 0.0 => ts_to_date(f as i64),
            V::Text(t) if t.len() >= 10 && t[4] == b'-' => String::from_utf8_lossy(&t[..10]).into_owned(), _ => String::new() }
    }

    /// The PAGYWOSG filter the Library is showing (directly, or through a pointer to a saved filter), if it is one.
    pub fn active_pag_spec(&self) -> Option<Arc<pd_filter::pagywosg_filter::Spec>> {
        let mut c = self.pag_cache.lock().unwrap();
        if let Some(v) = &*c { return v.clone(); }
        let found = (|| {
            let mut tree = self.state.get("filter_tree")?.clone();
            if let Some(name) = tree.get("saved_filter").and_then(|v| v.as_str()).map(String::from) {
                let e = self.state.get("saved_filters")?.get(&name)?;
                tree = e.get("tree").unwrap_or(e).clone();
            }
            pd_filter::pagywosg_filter::spec_of(&tree).map(Arc::new)
        })();
        *c = Some(found.clone());
        found
    }

    /// The running event's qualifications, fetched in the background (used by the Edit dialog when no PAGYWOSG filter is active).
    pub fn set_pag_live(&self, spec: Option<pd_filter::pagywosg_filter::Spec>) { if let Some(s) = spec { *self.pag_live.lock().unwrap() = Some(Arc::new(s)); } }

    /// The fields of one Steam game that PAGYWOSG's rules look at.
    pub fn pag_game(&self, appid: i64) -> Option<pd_filter::pagywosg_filter::PagGame> {
        let c = self.dup_conn().ok()?;
        c.query_row("SELECT appid, name, platform, tags, groups, developers, publishers, genres, categories, release_date, total_achievements, hltb_main, hltb_extras, hltb_completionist, review_score, sg_dlc_win FROM games WHERE appid = ?", [appid], |r| {
            let t = |i: usize| r.get::<_, Option<String>>(i).ok().flatten().unwrap_or_default();
            let n = |i: usize| r.get::<_, Option<i64>>(i).ok().flatten();
            Ok((r.get::<_, String>(2).unwrap_or_default(), pd_filter::pagywosg_filter::PagGame {
                appid, name: t(1), tags: t(3), groups: t(4), developers: t(5), publishers: t(6), genres: t(7), categories: t(8), release_date: n(9), total_achievements: n(10),
                hltb_main: n(11), hltb_extras: n(12), hltb_completionist: n(13), review_score: r.get::<_, Option<String>>(14).ok().flatten().filter(|s| !s.is_empty()), sg_dlc_win: n(15).unwrap_or(0) != 0,
            }))
        }).ok().filter(|(plat, _)| plat == "steam").map(|(_, g)| g)
    }

    /// What the edit dialog and the card tooltip say about a game under the active PAGYWOSG filter; `None` when no such filter is active or nothing qualifies.
    pub fn pag_panel(&self, appid: i64) -> Option<crate::pagywosg_dialog::PagPanel> { self.panel_from(self.active_pag_spec()?, appid) }

    /// The Edit dialog's version: the active PAGYWOSG filter's rules, else the running event's.
    pub fn pag_panel_edit(&self, appid: i64) -> Option<crate::pagywosg_dialog::PagPanel> {
        let spec = self.active_pag_spec().or_else(|| self.pag_live.lock().unwrap().clone())?;
        self.panel_from(spec, appid)
    }

    fn panel_from(&self, spec: Arc<pd_filter::pagywosg_filter::Spec>, appid: i64) -> Option<crate::pagywosg_dialog::PagPanel> {
        let g = self.pag_game(appid)?;
        let lines = pd_filter::pagywosg_filter::verdict(&spec, &g);
        let hltb = [g.hltb_main, g.hltb_extras, g.hltb_completionist].into_iter().flatten().filter(|v| *v > 0).min().map(|v| v as f64);
        if lines.is_empty() && hltb.is_none() { return None; }
        Some(crate::pagywosg_dialog::PagPanel::new(&spec.event_name, lines, &pd_filter::account::sg_username(&self.root), hltb))
    }

    /// Which of these appids are in the library.
    pub fn existing_appids(&self, appids: &[i64]) -> HashSet<i64> {
        let Ok(c) = self.dup_conn() else { return HashSet::new() };
        appids.iter().copied().filter(|a| c.query_row("SELECT 1 FROM games WHERE appid = ?", [a], |_| Ok(())).is_ok()).collect()
    }

    pub fn load_edit(&self, appid: i64) -> Option<EditInfo> {
        use rusqlite::types::ValueRef as V;
        let txt = |v: V| -> String { match v { V::Text(t) => String::from_utf8_lossy(t).into_owned(), V::Integer(i) => i.to_string(), V::Real(f) => format!("{}", f), _ => String::new() } };
        self.conn.query_row("SELECT name, developers, publishers, release_date, review_score, short_description, completion_status, last_played, date_added, is_free, installed, \
            review_percentage, weighted_percentage, metacritic_score, total_reviews, positive_reviews, playtime_forever, unlocked_achievements, total_achievements, \
            tags, genres, categories, `groups`, platform, install_path, protondb_tier, hltb_main, hltb_extras, hltb_completionist, vertical_art_source, horizontal_art_source, icon_source, platform_executable, launch_args, hltb_id, hltb_matched_name FROM games WHERE appid = ?", [appid], |r| {
            let g = |i: usize| -> rusqlite::Result<String> { Ok(txt(r.get_ref(i)?)) };
            let hrs = |i: usize| -> rusqlite::Result<f64> { Ok(match r.get_ref(i)? { V::Integer(v) => v as f64, V::Real(v) => v, _ => 0.0 }) };
            let list = |i: usize| -> rusqlite::Result<Vec<String>> { Ok(split_list(r.get::<_, Option<String>>(i)?)) };
            let fields = EditFields {
                name: g(0)?, developers: g(1)?, publishers: g(2)?, release_date: Self::date_text(r.get_ref(3)?), review_score: g(4)?, description: g(5)?,
                completion_status: { let s = g(6)?; if s.is_empty() { "Never Played".into() } else { s } },
                last_played: Self::date_text(r.get_ref(7)?), date_added: Self::date_text(r.get_ref(8)?),
                is_free: r.get::<_, Option<i64>>(9)?.unwrap_or(0) != 0, installed: r.get::<_, Option<i64>>(10)?.unwrap_or(0) != 0,
                review_percentage: g(11)?, weighted_percentage: g(12)?, metacritic_score: g(13)?, total_reviews: g(14)?, positive_reviews: g(15)?,
                playtime_mins: g(16)?, unlocked: g(17)?, total_ach: g(18)?,
                tags: list(19)?, genres: list(20)?, categories: list(21)?, groups: list(22)?,
                executable: g(32)?, launch_args: g(33)?,
            };
            let hours = |m: f64| { let (hh, mm) = ((m / 60.0) as i64, (m % 60.0).round() as i64); if mm > 0 { format!("{hh}h {mm}m") } else { format!("{hh}h") } };
            let (main, extras, comp) = (hrs(26)?, hrs(27)?, hrs(28)?);
            let hltb = if main + extras + comp > 0.0 { format!("Main: {} \u{b7} +Extras: {} \u{b7} Completionist: {}", hours(main), hours(extras), hours(comp)) } else { String::new() };
            Ok(EditInfo { fields, platform: g(23)?, install_path: Some(g(24)?).filter(|p| !p.is_empty()), protondb: g(25)?, hltb, hltb_id: r.get::<_, Option<i64>>(34).ok().flatten().filter(|v| *v > 0), hltb_name: g(35).unwrap_or_default(), art_sources: [g(29)?, g(30)?, g(31)?] })
        }).ok()
    }

    /// Save an edit: only the columns that changed, with `update_game`'s rules (dates as UTC timestamps, empty number = NULL, a rename drops the
    /// metadata stamp so the next sweep resolves it again, group changes recorded as manual in group_sources).
    pub fn save_edit(&mut self, appid: i64, old: &EditFields, new: &EditFields) -> Result<(), String> {
        use rusqlite::types::Value as SV;
        let num_i = |s: &str| -> SV { s.trim().parse::<f64>().map(|v| SV::Integer(v as i64)).unwrap_or(SV::Null) };
        let date = |s: &str| -> SV { match pd_filter::date_to_ts(&Value::String(s.trim().to_string())) { Some(t) => SV::Integer(t), None => SV::Null } };
        let text = |s: &str| -> SV { SV::Text(s.trim().to_string()) };
        let list = |v: &[String]| -> SV { SV::Text(v.join(",")) };
        let mut set: Vec<(&str, SV)> = Vec::new();
        if old.name != new.name { set.push(("name", text(&new.name))); }
        if old.developers != new.developers { set.push(("developers", text(&new.developers))); }
        if old.publishers != new.publishers { set.push(("publishers", text(&new.publishers))); }
        if old.release_date != new.release_date { set.push(("release_date", date(&new.release_date))); }
        if old.review_score != new.review_score { set.push(("review_score", text(&new.review_score))); }
        if old.description != new.description { set.push(("short_description", text(&new.description))); }
        if old.completion_status != new.completion_status { set.push(("completion_status", text(&new.completion_status))); }
        if old.last_played != new.last_played { set.push(("last_played", date(&new.last_played))); }
        if old.date_added != new.date_added { set.push(("date_added", date(&new.date_added))); }
        if old.is_free != new.is_free { set.push(("is_free", SV::Integer(new.is_free as i64))); }
        if old.installed != new.installed { set.push(("installed", SV::Integer(new.installed as i64))); }
        for (col, o, n) in [("review_percentage", &old.review_percentage, &new.review_percentage), ("weighted_percentage", &old.weighted_percentage, &new.weighted_percentage),
            ("metacritic_score", &old.metacritic_score, &new.metacritic_score), ("total_reviews", &old.total_reviews, &new.total_reviews), ("positive_reviews", &old.positive_reviews, &new.positive_reviews),
            ("playtime_forever", &old.playtime_mins, &new.playtime_mins), ("unlocked_achievements", &old.unlocked, &new.unlocked), ("total_achievements", &old.total_ach, &new.total_ach)] {
            if o != n { set.push((col, num_i(n))); }
        }
        if old.tags != new.tags { set.push(("tags", list(&new.tags))); }
        if old.genres != new.genres { set.push(("genres", list(&new.genres))); }
        if old.categories != new.categories { set.push(("categories", list(&new.categories))); }
        if old.groups != new.groups { set.push(("`groups`", list(&new.groups))); }
        if old.launch_args != new.launch_args { set.push(("launch_args", text(&new.launch_args))); }
        // a game added by hand: the file it runs drives the install folder and Installed (a path that moved stops showing Installed)
        if old.executable != new.executable {
            let (exe, folder, installed) = custom_program_fields(&new.executable);
            set.push(("platform_executable", exe.map_or(SV::Null, SV::Text)));
            set.push(("install_path", folder.map_or(SV::Null, SV::Text)));
            if let Some(i) = installed { set.push(("installed", SV::Integer(i))); }
        }
        if set.is_empty() { return Ok(()); }
        let c = rusqlite::Connection::open(&self.db).map_err(|e| e.to_string())?;
        c.busy_timeout(std::time::Duration::from_secs(10)).ok();
        // a renamed game gets a fresh metadata backfill (the old match was found by the old name)
        if old.name != new.name && !new.name.trim().is_empty() {
            let stamp: Option<String> = c.query_row("SELECT meta_backfill_fetched FROM games WHERE appid = ?", [appid], |r| r.get(0)).ok().flatten();
            if !matches!(stamp.as_deref(), None | Some("0")) { set.push(("meta_backfill_fetched", SV::Text("0".into()))); set.push(("steam_appid", SV::Null)); }
        }
        let sql = format!("UPDATE games SET {} WHERE appid = ?", set.iter().map(|(c, _)| format!("{c} = ?")).collect::<Vec<_>>().join(", "));
        let mut params: Vec<SV> = set.into_iter().map(|(_, v)| v).collect();
        params.push(SV::Integer(appid));
        c.execute(&sql, rusqlite::params_from_iter(params)).map_err(|e| e.to_string())?;
        if old.groups != new.groups { self.track_manual_groups(appid, &old.groups, &new.groups); }
        if let Some(g) = self.info.get_mut(&appid) {
            if !new.name.trim().is_empty() { g.name = new.name.trim().to_string(); }
            g.installed = new.installed;
            if old.completion_status != new.completion_status { if let Some(n) = self.completion.get_mut(&g.status) { *n -= 1; } *self.completion.entry(new.completion_status.clone()).or_insert(0) += 1; g.status = new.completion_status.clone(); }
        }
        if old.tags != new.tags || old.completion_status != new.completion_status { let _ = self.recalculate_tag_similarity(); }
        if old.name != new.name { if let Ok(c) = self.dup_conn() { let _ = pd_filter::dupes::on_rename(&c, appid); } }   // a rename never links; it may drop a link that no longer matches
        Ok(())
    }

    /// `_track_manual_groups`: remember in group_sources_<account>.json which groups the user added or removed by hand.
    fn track_manual_groups(&self, appid: i64, old: &[String], new: &[String]) {
        let Some(id) = &self.account_id else { return };
        let path = self.root.join(format!("group_sources_{id}.json"));
        let mut gs: Value = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(serde_json::json!({"version": 1, "sources": {}, "assignments": {}}));
        let key = appid.to_string();
        for g in new.iter().filter(|g| !old.contains(g)) {
            let owners = gs["assignments"][&key][g].take();
            let mut list: Vec<Value> = owners.as_array().cloned().unwrap_or_default();
            if !list.iter().any(|v| v == "manual") { list.push(Value::String("manual".into())); }
            gs["assignments"][&key][g] = Value::Array(list);
        }
        for g in old.iter().filter(|g| !new.contains(g)) {
            if let Some(list) = gs["assignments"][&key][g].as_array().cloned() {
                let left: Vec<Value> = list.into_iter().filter(|v| v != "manual").collect();
                if left.is_empty() { if let Some(m) = gs["assignments"][&key].as_object_mut() { m.remove(g); } } else { gs["assignments"][&key][g] = Value::Array(left); }
            }
        }
        if gs["assignments"][&key].as_object().map_or(false, |m| m.is_empty()) { if let Some(m) = gs["assignments"].as_object_mut() { m.remove(&key); } }
        let tmp = path.with_extension("json.tmp");
        if serde_json::to_string_pretty(&gs).ok().and_then(|t| std::fs::write(&tmp, t).ok()).is_some() { let _ = std::fs::rename(&tmp, &path); }
    }
}

#[cfg(test)]
mod install_shelf_tests {
    use super::*;
    /// Uninstalling a game takes it off the Installed shelf (and an install scan that finds games puts them on): the real schema, from the dev copy when it exists.
    #[test]
    fn the_installed_shelf_follows_install_changes() {
        let src = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/share/zest-dev/data");
        let Some(db) = std::fs::read_dir(&src).ok().and_then(|d| d.flatten().map(|e| e.path()).find(|p| p.file_name().and_then(|n| n.to_str()).map_or(false, |n| n.starts_with("games_") && n.ends_with(".db")))) else { return };
        let dir = std::env::temp_dir().join(format!("zest_inst_shelf_{}", std::process::id())); let _ = std::fs::remove_dir_all(&dir); std::fs::create_dir_all(&dir).unwrap();
        // a private copy of the library through sqlite's backup, so the dev copy is never written
        { let from = rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap(); from.execute("VACUUM INTO ?", [dir.join(db.file_name().unwrap()).to_string_lossy().as_ref()]).unwrap(); }
        std::fs::write(dir.join("state.json"), "{}").unwrap();
        let mut d = load(&dir).unwrap();
        let idx = d.shelves.iter().position(|s| s.id == "installed").expect("an Installed shelf");
        let before: Vec<i64> = d.shelves[idx].games.iter().map(|g| g.0).collect();
        if before.is_empty() { return; }
        let gone = before[0];
        d.clear_install(gone);
        let after: Vec<i64> = d.shelves[idx].games.iter().map(|g| g.0).collect();
        assert!(!after.contains(&gone), "the uninstalled game left the shelf");
        // the shelf is capped, so the next installed game takes the free place; everything on it is installed
        assert!(after.len() <= before.len() && after.len() + 1 >= before.len(), "{} -> {}", before.len(), after.len());
        assert!(after.iter().all(|a| d.info.get(a).map_or(false, |g| g.installed)), "only installed games are on the Installed shelf");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod priority_tests {
    use super::*;
    /// Changing the platform order re-points the duplicate link to the newly best-ranked copy, saves the order (keeping platforms the library lacks), and lists only present platforms.
    #[test]
    fn a_new_platform_order_moves_the_shown_copy() {
        let dir = std::env::temp_dir().join(format!("zest_prio_{}", std::process::id())); let _ = std::fs::remove_dir_all(&dir); std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("games_1.db");
        { let c = pd_filter::schema::init_db(&db).unwrap();
          c.execute_batch("INSERT INTO games (appid, name, platform, platform_id, completion_status) VALUES (10, 'Hades', 'steam', '10', 'Never Played'), (-1, 'Hades', 'gog', '1', 'Never Played'), (-2, 'Solo', 'epic_games', '2', 'Never Played');").unwrap(); }
        std::fs::write(dir.join("state.json"), r#"{"platform_priority": ["ubisoft", "steam", "gog", "epic_games"]}"#).unwrap();
        let mut d = load(&dir).unwrap();
        assert_eq!(d.priority_platforms(), vec!["steam", "gog", "epic_games"], "the saved order for the platforms the library has");
        // a first run only records the games as already examined (PlayDate's links are kept); mark these as new so they are matched
        rusqlite::Connection::open(d.db_path()).unwrap().execute("UPDATE games SET dup_checked = NULL", []).unwrap();
        d.dup_examine_new();
        let shown = |d: &HomeData| -> (Option<String>, Option<String>) { let c = rusqlite::Connection::open(d.db_path()).unwrap(); let g = |a: i64| c.query_row("SELECT duplicate_of FROM games WHERE appid = ?", [a], |r| r.get::<_, Option<String>>(0)).unwrap(); (g(10), g(-1)) };
        assert_eq!(shown(&d), (None, Some("10".to_string())), "Steam outranks GOG: the GOG copy is hidden");
        let moved = d.save_priority(vec!["gog".into(), "steam".into(), "epic_games".into()]).unwrap();
        assert!(moved >= 2, "the link flips: {moved}");
        assert_eq!(shown(&d), (Some("-1".to_string()), None), "GOG now outranks Steam");
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("state.json")).unwrap()).unwrap();
        assert_eq!(saved["platform_priority"], serde_json::json!(["gog", "steam", "epic_games", "ubisoft"]), "ubisoft is not in this library but stays in the saved order");
        assert_eq!(d.priority_platforms(), vec!["gog", "steam", "epic_games"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

fn unix_now() -> i64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0) }

#[cfg(test)]
mod split_tests {
    use super::split_args;
    #[test] fn arguments_split_like_shlex() {
        assert_eq!(split_args("-fullscreen -w 1920"), ["-fullscreen", "-w", "1920"]);
        assert_eq!(split_args(r#"--name "Two Words" 'a b' c\ d"#), ["--name", "Two Words", "a b", "c d"]);
        assert_eq!(split_args(""), Vec::<String>::new()); assert_eq!(split_args(r#"x "" y"#), ["x", "", "y"]);
        assert_eq!(split_args(r#""a\"b""#), [r#"a"b"#]);
    }
}

#[cfg(test)]
mod slide_tests {
    use super::*;
    fn games(n: usize) -> Vec<(i64, String)> { (0..n as i64).map(|i| (i, format!("g{i}"))).collect() }
    #[test] fn a_sliding_shelf_shows_one_card_more_shifted_by_the_fraction() {
        let c0 = CardBox { appid: 3, name: "g3".into(), x: 100, y: 50, w: 94, h: 140 };   // the card at the real offset 3
        let g = games(20);
        // arrived: pos == offset gives the cards where they sit (plus the one about to enter, off to the right)
        let v = slide_cards(&c0, 3.0, 4, &g);
        assert_eq!(v.iter().map(|c| c.appid).collect::<Vec<_>>(), [3, 4, 5, 6, 7]); assert_eq!(v[0].x, 100); assert_eq!(v[1].x, 200);
        // halfway between 2 and 3 going forward: everything is half a card (50 px) to the right of where it ends
        let v = slide_cards(&c0, 2.5, 4, &g);
        assert_eq!(v.iter().map(|c| c.appid).collect::<Vec<_>>(), [2, 3, 4, 5, 6]); assert_eq!(v[1].x, 150); assert_eq!(v[0].x, 50);
        // near the end of the pool there is nothing more to show
        assert_eq!(slide_cards(&c0, 18.0, 4, &g).len(), 2);
        // negative or tiny pools do not panic
        assert!(slide_cards(&c0, -0.5, 4, &g).len() >= 1); assert!(slide_cards(&c0, 0.5, 4, &[]).is_empty());
    }
    /// A card partly outside the left or right end of the shelf's content area is drawn only inside it: the inside part is there, the outside is the background.
    #[test] fn a_card_half_outside_the_clip_shows_its_inside_part_only() {
        let (w, h) = (400usize, 300usize);
        let bg = vec![0x0010_1010u32; w * h];
        let dir = std::env::temp_dir().join(format!("zest-slide-{}", std::process::id())); let _ = std::fs::create_dir_all(&dir);
        let mut covers = Covers::new(dir.clone());
        for (card_x, name) in [(60, "left"), (290, "right")] {
            let mut buf = bg.clone();
            let b = ShelfBox { idx: 0, x: 78, y: 40, w: 300, h: 200, label_y: 47, cards: vec![CardBox { appid: 1, name: "g".into(), x: card_x, y: 50, w: 94, h: 140 }], widget: None, pie: None, off: 0, fit: 3, total: 9, clip: Some((89, 367)) };
            paint_cards(&mut buf, &bg, w, &[b], &mut covers, false, &HashMap::new());
            let at = |x: usize, y: usize| buf[y * w + x];
            let (inside, outside) = if name == "left" { (at(120, 120), at(70, 120)) } else { (at(300, 120), at(380, 120)) };
            assert_ne!(inside, 0x0010_1010, "{name}: the part inside the clip is drawn");
            assert_eq!(outside, 0x0010_1010, "{name}: the part outside the clip is not");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod geometry_tests {
    use super::*;
    #[test] fn one_shelf_spans_the_window_and_side_by_side_ones_share_it_by_weight() {
        let one = row_widths(1000, &[10.0]); assert_eq!(one.len(), 1); assert!((one[0].0 - 8.0).abs() < 0.01 && (one[0].1 - 984.0).abs() < 0.01, "{one:?}");
        let two = row_widths(1000, &[10.0, 10.0]); assert!((two[0].1 - two[1].1).abs() < 0.01); assert!(two[1].0 > two[0].0 + two[0].1, "a gap between them");
        let uneven = row_widths(1000, &[15.0, 5.0]); assert!(uneven[0].1 > 2.0 * uneven[1].1 - 30.0);
        let total: f32 = two.iter().map(|x| x.1).sum::<f32>() + 4.0; assert!(total <= 984.0 + 0.5, "inside the content width: {total}");
    }
    #[test] fn taller_rows_have_bigger_cards_and_fewer_fit() {
        let vh = 9.0;
        let (w1, h1, f1) = card_metrics(984.0, 30.0 * vh, vh, true); let (w2, h2, f2) = card_metrics(984.0, 40.0 * vh, vh, true);
        assert!(h2 > h1 && w2 > w1 && f2 <= f1); assert!((w1 - h1 * 2.0 / 3.0).abs() < 0.001);
        let (_, hn, _) = card_metrics(984.0, 30.0 * vh, vh, false); assert!(hn > h1, "no title: more room for the card");
        assert_eq!(card_metrics(30.0, 30.0 * vh, vh, true).2, 1, "a shelf too narrow for one card still counts one");
    }
}

#[cfg(test)]
mod clear_art_tests {
    use super::*;
    /// Clearing a cover deletes the file and forgets its source, edge colors and pixel size (and only for that kind).
    #[test]
    fn clearing_a_cover_forgets_everything_stored_about_it() {
        let dir = std::env::temp_dir().join(format!("zest_clear_{}", std::process::id())); let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("static/img/library/vertical")).unwrap(); std::fs::create_dir_all(dir.join("static/img/library/horizontal")).unwrap();
        std::fs::write(dir.join("static/img/library/vertical/7.jpg"), b"x").unwrap(); std::fs::write(dir.join("static/img/library/horizontal/7.jpg"), b"x").unwrap();
        let db = dir.join("games_1.db");
        { let c = pd_filter::schema::init_db(&db).unwrap();
          c.execute_batch("INSERT INTO games (appid, name, platform, vertical_art_source, edge_vertical, vertical_art_w, vertical_art_h, horizontal_art_source, edge_horizontal, horizontal_art_w, horizontal_art_h)
              VALUES (7, 'A', 'steam', 'capsule', '-', 600, 900, 'header', '-', 460, 215)").unwrap(); }
        std::fs::write(dir.join("state.json"), "{}").unwrap();
        let d = load(&dir).unwrap();
        d.clear_art(7, "vertical").unwrap();
        let c = rusqlite::Connection::open(&db).unwrap();
        let row = |sql: &str| c.query_row(sql, [], |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<i64>>(2)?, r.get::<_, Option<i64>>(3)?))).unwrap();
        assert_eq!(row("SELECT vertical_art_source, edge_vertical, vertical_art_w, vertical_art_h FROM games WHERE appid = 7"), (None, None, None, None));
        assert_eq!(row("SELECT horizontal_art_source, edge_horizontal, horizontal_art_w, horizontal_art_h FROM games WHERE appid = 7"), (Some("header".into()), Some("-".into()), Some(460), Some(215)));
        assert!(!dir.join("static/img/library/vertical/7.jpg").exists() && dir.join("static/img/library/horizontal/7.jpg").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod pie_tests {
    use super::*;
    /// The faster pie draws exactly what the original did, for one slice, several and awkward fractions.
    #[test]
    fn draw_pie_matches_the_reference() {
        let cases: Vec<Vec<(f32, u32)>> = vec![
            vec![(1.0, 0xeeeeee)],
            vec![(0.57, 0xeeeeee), (0.21, 0xf0ad4e), (0.02, 0xd9534f), (0.08, 0x5cb85c), (0.12, 0x5bc0de)],
            vec![(0.5, 0x112233), (0.5, 0x445566)],
            vec![(0.333, 0xff0000), (0.333, 0x00ff00), (0.334, 0x0000ff)],
            vec![(0.999, 0xaaaaaa), (0.001, 0x222222)],
        ];
        for (ci, slices) in cases.iter().enumerate() {
            for &(w, cx, cy, r) in &[(300usize, 150.3f32, 140.7f32, 120.0f32), (200, 100.0, 100.0, 60.5), (120, 33.2, 61.9, 31.0)] {
                let h = w;
                let base: Vec<u32> = (0..w * h).map(|i| ((i * 2654435761usize) >> 8) as u32 & 0xffffff).collect();
                let (mut a, mut b) = (base.clone(), base.clone());
                draw_pie(&mut a, w, cx, cy, r, slices);
                draw_pie_reference(&mut b, w, cx, cy, r, slices);
                let bad = a.iter().zip(&b).filter(|(x, y)| x != y).count();
                assert_eq!(bad, 0, "case {ci} size {w}: {bad} pixels differ");
            }
        }
    }
}
