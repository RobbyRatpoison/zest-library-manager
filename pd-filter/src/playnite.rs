//! Import from Playnite (`imports.py`'s Playnite half, plus what its overhaul note asked for): read a Playnite backup zip (a LiteDB v4 reader for `library/games.db`,
//! `completionstatuses.db` and `categories.db`), match its games to this library the way PlayDate does, then apply what the user chose: dates and playtime (fill or
//! overwrite), completion statuses through an editable mapping, categories as groups. Everything an apply changes is written to `playnite_undo.json` first.
//! The Playnite field names for status and categories (`CompletionStatusId` / an embedded reference, `CategoryIds`) are read from Playnite's data model, not yet checked against a
//! real backup (PlayDate never read them), so both the plain Guid and the reference-document shapes are accepted.
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

pub const STEAM_PLUGIN_ID: &str = "cb91dfc9-b977-43bf-8e70-55f46e410fab";
const NO_PLUGIN: &str = "00000000-0000-0000-0000-000000000000";
/// Playnite's own library plugins and the PlayDate platform each one is (ids from their source, as in `KNOWN_PLAYNITE_PLUGINS`).
pub const KNOWN_PLUGINS: &[(&str, &str)] = &[
    (STEAM_PLUGIN_ID, "steam"), ("aebe8b7c-6dc3-4a66-af31-e7375c6b5e9e", "gog"), ("00000002-dbd1-46c6-b5d0-b1ba559d10e4", "epic_games"), ("402674cd-4af6-4886-b6ec-0e695bfa0688", "amazon_games"),
    ("7e4fbb5e-2ae3-48d4-8ba0-6b30e7a4e287", "xbox"), ("c2f038e5-8b92-4877-91f1-da9094155fc5", "ubisoft"), ("85dd7072-2f20-4e76-a007-41035e390724", "ea_app"), ("00000001-ebb2-4eec-abcb-7c89937a42bb", "itch_io"),
    ("96e8c4bc-ec5c-4c8b-87e7-18ee5a690626", "humble"), ("e3c26a3d-d695-4cb7-a769-5ff7612c7edd", "battle_net"), ("88409022-088a-4de8-805a-fdbac291f00a", "rockstar"),
];
fn known(plugin: &str) -> Option<&'static str> { KNOWN_PLUGINS.iter().find(|(p, _)| *p == plugin).map(|(_, v)| *v) }

// ───────────────────────── LiteDB v4 and BSON ─────────────────────────

const PAGE: usize = 4096;
const HDR: usize = 25;
const NO_PAGE: u32 = 0xFFFF_FFFF;
fn u16le(d: &[u8], p: usize) -> Option<u16> { Some(u16::from_le_bytes(d.get(p..p + 2)?.try_into().ok()?)) }
fn u32le(d: &[u8], p: usize) -> Option<u32> { Some(u32::from_le_bytes(d.get(p..p + 4)?.try_into().ok()?)) }
fn i32le(d: &[u8], p: usize) -> Option<i32> { Some(i32::from_le_bytes(d.get(p..p + 4)?.try_into().ok()?)) }
fn i64le(d: &[u8], p: usize) -> Option<i64> { Some(i64::from_le_bytes(d.get(p..p + 8)?.try_into().ok()?)) }

/// `_litedb4_documents`: the whole BSON documents of every collection in a LiteDB v4 file (file version 7, 4096-byte pages); empty for any other format.
pub fn litedb4_documents(data: &[u8]) -> Vec<Vec<u8>> {
    if data.len() < PAGE * 2 || data.get(25..52) != Some(b"** This is a LiteDB file **") || data.get(52) != Some(&7) { return vec![]; }
    let npages = data.len() / PAGE;
    let mut extends: HashMap<u32, (u32, &[u8])> = HashMap::new();
    for pid in 0..npages {
        let base = pid * PAGE;
        if data[base + 4] == 5 {
            let (Some(nxt), Some(count)) = (u32le(data, base + 9), u16le(data, base + 13)) else { continue };
            let count = count as usize;
            if count <= PAGE - HDR { extends.insert(pid as u32, (nxt, &data[base + HDR..base + HDR + count])); }
        }
    }
    let mut docs = vec![];
    for pid in 0..npages {
        let base = pid * PAGE;
        if data[base + 4] != 4 { continue; }
        let Some(count) = u16le(data, base + 13) else { continue };
        let mut off = base + HDR;
        for _ in 0..count {
            if off + 8 > base + PAGE { break; }
            let (Some(mut ext), Some(size)) = (u32le(data, off + 2), u16le(data, off + 6)) else { break };
            off += 8;
            let size = size as usize;
            if ext != NO_PAGE {
                let (mut parts, mut seen) = (Vec::new(), HashSet::new());
                while let Some((nxt, chunk)) = extends.get(&ext) { if !seen.insert(ext) { break; } parts.extend_from_slice(chunk); ext = *nxt; }
                docs.push(parts);
            } else { docs.push(data.get(off..off + size).unwrap_or(&[]).to_vec()); }
            off += size;
        }
    }
    docs
}

#[derive(Clone, Debug, PartialEq)]
pub enum B { Str(String), Bool(bool), Int(i64), Bin(Vec<u8>), Doc(Vec<(String, B)>), Arr(Vec<B>) }

/// `_bson_fields`: the top-level fields of one BSON document named in `want` (all when empty): strings, bools, ints and datetimes, binary, embedded documents and arrays.
/// Other types are skipped; a malformed document yields whatever was read before the problem.
pub fn bson_fields(doc: &[u8], want: &[&str]) -> Vec<(String, B)> {
    let mut out = vec![];
    if doc.len() < 5 { return out; }
    let end = (i32le(doc, 0).unwrap_or(0).max(0) as usize).min(doc.len());
    let mut pos = 4;
    let wanted = |k: &str| want.is_empty() || want.contains(&k);
    while pos + 1 < end {
        let t = doc[pos];
        let Some(k_end) = doc[pos + 1..].iter().position(|b| *b == 0).map(|i| pos + 1 + i) else { break };
        let key = String::from_utf8_lossy(&doc[pos + 1..k_end]).to_string();
        pos = k_end + 1;
        match t {
            0x02 => {
                let Some(n) = i32le(doc, pos) else { break };
                if n < 1 || pos + 4 + n as usize > doc.len() { break; }
                if wanted(&key) { out.push((key, B::Str(String::from_utf8_lossy(&doc[pos + 4..pos + 3 + n as usize]).to_string()))); }
                pos += 4 + n as usize;
            }
            0x03 | 0x04 => {
                let Some(n) = i32le(doc, pos) else { break };
                if n < 5 || pos + n as usize > doc.len() { break; }
                if wanted(&key) {
                    let sub = bson_fields(&doc[pos..pos + n as usize], &[]);
                    out.push((key, if t == 0x03 { B::Doc(sub) } else { B::Arr(sub.into_iter().map(|(_, v)| v).collect()) }));
                }
                pos += n as usize;
            }
            0x05 => {
                let Some(n) = i32le(doc, pos) else { break };
                if n < 0 || pos + 5 + n as usize > doc.len() { break; }
                if wanted(&key) { out.push((key, B::Bin(doc[pos + 5..pos + 5 + n as usize].to_vec()))); }
                pos += 5 + n as usize;
            }
            0x01 | 0x07 | 0x08 | 0x09 | 0x0A | 0x10 | 0x12 | 0x13 | 0xFF | 0x7F => {
                let size = match t { 0x01 | 0x09 | 0x12 => 8, 0x07 => 12, 0x08 => 1, 0x10 => 4, 0x13 => 16, _ => 0 };
                if wanted(&key) {
                    match t {
                        0x08 => if let Some(b) = doc.get(pos) { out.push((key, B::Bool(*b != 0))); },
                        0x09 | 0x12 => if let Some(v) = i64le(doc, pos) { out.push((key, B::Int(v))); },
                        0x10 => if let Some(v) = i32le(doc, pos) { out.push((key, B::Int(v as i64))); },
                        _ => {}
                    }
                }
                pos += size;
            }
            _ => break,
        }
    }
    out
}
fn get<'a>(f: &'a [(String, B)], k: &str) -> Option<&'a B> { f.iter().find(|(n, _)| n == k).map(|(_, v)| v) }
fn get_str(f: &[(String, B)], k: &str) -> Option<String> { match get(f, k)? { B::Str(s) => Some(s.clone()), _ => None } }
fn get_bool(f: &[(String, B)], k: &str) -> bool { matches!(get(f, k), Some(B::Bool(true))) }

/// .NET's Guid bytes (the first three groups little-endian) as the usual text, as `uuid.UUID(bytes_le=...)`.
pub fn guid_str(b: &[u8]) -> Option<String> {
    if b.len() != 16 { return None; }
    Some(format!("{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}", b[3], b[2], b[1], b[0], b[5], b[4], b[7], b[6], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]))
}
/// A Guid as the game's field holds it: a binary value, or a reference document (`{"$id": <guid>, "$ref": ...}`).
fn guid_of(v: &B) -> Option<String> {
    match v { B::Bin(b) => guid_str(b), B::Doc(d) => get(d, "$id").and_then(guid_of), B::Str(s) if s.len() == 36 => Some(s.to_lowercase()), _ => None }
}

fn ms_to_secs(ms: i64) -> Option<i64> { (ms > 0 && ms < 4_102_444_800_000).then_some(ms / 1000) }
fn civil_date(secs: i64) -> String {
    let z = secs.div_euclid(86400) + 719468; let era = if z >= 0 { z } else { z - 146096 } / 146097; let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; let y = yoe + era * 400; let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153; let d = doy - (153 * mp + 2) / 5 + 1; let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{:02}-{:02}", if m <= 2 { y + 1 } else { y }, m, d)
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PGame {
    pub plugin: Option<String>, pub game_id: String, pub name: Option<String>,
    pub date_added: Option<String>, pub last_played: Option<i64>, pub playtime: Option<i64>,
    pub status_id: Option<String>, pub category_ids: Vec<String>, pub favorite: bool, pub hidden: bool,
}
fn game_from_fields(f: &[(String, B)]) -> Option<PGame> {
    let game_id = get_str(f, "GameId")?;
    let mut g = PGame { game_id, name: get_str(f, "Name"), favorite: get_bool(f, "Favorite"), hidden: get_bool(f, "Hidden"), ..Default::default() };
    if let Some(B::Bin(b)) = get(f, "PluginId") { g.plugin = guid_str(b); }
    if let Some(B::Int(ms)) = get(f, "Added") { g.date_added = ms_to_secs(*ms).map(civil_date); }
    if let Some(B::Int(ms)) = get(f, "LastActivity") { g.last_played = ms_to_secs(*ms); }
    if let Some(B::Int(p)) = get(f, "Playtime") { if *p >= 60 { g.playtime = Some(p / 60); } }
    g.status_id = get(f, "CompletionStatusId").and_then(guid_of).or_else(|| get(f, "CompletionStatus").and_then(guid_of)).filter(|s| s != NO_PLUGIN);
    if let Some(B::Arr(a)) = get(f, "CategoryIds").or_else(|| get(f, "Categories")) { g.category_ids = a.iter().filter_map(guid_of).collect(); }
    Some(g)
}

pub struct Backup { pub games: Vec<PGame>, /// id -> name
    pub statuses: HashMap<String, String>, pub categories: HashMap<String, String> }

fn entry_bytes(zip: &Path, names: &[String], suffix: &str) -> Result<Option<Vec<u8>>, String> {
    let Some(n) = names.iter().find(|n| n.replace('\\', "/").to_lowercase().ends_with(suffix)) else { return Ok(None) };
    let tmp = std::env::temp_dir().join(format!("zest_playnite_{}_{:?}_{}", std::process::id(), std::thread::current().id(), suffix.replace('/', "_")));
    crate::archive::extract_entry(zip, n, &tmp)?;
    let b = std::fs::read(&tmp).map_err(|e| e.to_string()); let _ = std::fs::remove_file(&tmp);
    Ok(Some(b?))
}
fn names_of(data: &[u8]) -> HashMap<String, String> {
    litedb4_documents(data).iter().filter_map(|d| { let f = bson_fields(d, &["_id", "Name"]); Some((guid_of(get(&f, "_id")?)?, get_str(&f, "Name")?)) }).collect()
}

/// Every game of a Playnite backup (from any library plugin), with the names of its statuses and categories.
pub fn read_backup(zip: &Path) -> Result<Backup, String> {
    let names = crate::archive::zip_names(zip)?;
    let data = entry_bytes(zip, &names, "library/games.db")?.ok_or("That zip has no Playnite library (library/games.db).")?;
    let games: Vec<PGame> = litedb4_documents(&data).iter().filter_map(|d| game_from_fields(&bson_fields(d, &["GameId", "Name", "PluginId", "Added", "LastActivity", "Playtime", "CompletionStatusId", "CompletionStatus", "CategoryIds", "Categories", "Favorite", "Hidden"]))).collect();
    if games.is_empty() { return Err("No games found in the backup (it may be an older Playnite format).".into()); }
    let statuses = entry_bytes(zip, &names, "library/completionstatuses.db")?.map(|d| names_of(&d)).unwrap_or_default();
    let categories = entry_bytes(zip, &names, "library/categories.db")?.map(|d| names_of(&d)).unwrap_or_default();
    Ok(Backup { games, statuses, categories })
}

// ───────────────────────── matching ─────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub struct Match { pub game: usize, pub appid: i64, pub platform: String, /// found by the plugin's own id (else by title)
    pub by_id: bool }
pub struct Matched { pub matches: Vec<Match>, pub unmatched: Vec<usize> }

/// `match_playnite_games`: Steam by appid; every other plugin by `platform_id` / `platform_appname` (confined to the plugin's own platform when it is a built-in Playnite
/// library); then the exact normalised title within that platform, only when one game fits.
pub fn match_games(games: &[PGame], conn: &Connection) -> Result<Matched, String> {
    let norm = crate::dupes::norm_edition;
    let mut st = conn.prepare("SELECT appid, platform, platform_id, platform_appname, name FROM games WHERE platform != 'steam'").map_err(|e| e.to_string())?;
    let rows: Vec<(i64, String, Option<String>, Option<String>, String)> = st.query_map([], |r| Ok((r.get(0)?, r.get::<_, Option<String>>(1)?.unwrap_or_default(), r.get::<_, Option<String>>(2)?, r.get::<_, Option<String>>(3)?, r.get::<_, Option<String>>(4)?.unwrap_or_default())))
        .map_err(|e| e.to_string())?.collect::<Result<_, _>>().map_err(|e| e.to_string())?;
    let steam_owned: HashSet<i64> = conn.prepare("SELECT appid FROM games WHERE platform = 'steam'").and_then(|mut s| s.query_map([], |r| r.get(0))?.collect()).map_err(|e| e.to_string())?;
    let mut by_id: HashMap<String, Vec<(String, i64)>> = HashMap::new();
    let mut by_name: Vec<((String, String), i64)> = vec![];
    for (appid, plat, pid, pname, name) in &rows {
        for k in [pid, pname].into_iter().flatten() { let k = k.trim().to_lowercase(); if !k.is_empty() { by_id.entry(k).or_default().push((plat.clone(), *appid)); } }
        by_name.push(((plat.clone(), norm(name)), *appid));
    }
    let have_plugins = games.iter().any(|g| g.plugin.is_some());
    let mut resolved: BTreeMap<usize, (String, i64, bool)> = BTreeMap::new();
    let mut votes: HashMap<String, HashMap<String, usize>> = HashMap::new();
    for (i, g) in games.iter().enumerate() {
        let plugin = g.plugin.as_deref();
        let digits = !g.game_id.is_empty() && g.game_id.bytes().all(|b| b.is_ascii_digit());
        if plugin == Some(STEAM_PLUGIN_ID) || (!have_plugins && digits) {
            if digits { if let Ok(a) = g.game_id.parse::<i64>() { if steam_owned.contains(&a) { resolved.insert(i, ("steam".into(), a, true)); } } }
            continue;
        }
        let Some(plugin) = plugin.filter(|p| *p != NO_PLUGIN) else { continue };
        let mut hits: Vec<(String, i64)> = by_id.get(&g.game_id.trim().to_lowercase()).cloned().unwrap_or_default();
        if let Some(k) = known(plugin) { hits.retain(|h| h.0 == k); }
        if hits.len() == 1 {
            resolved.insert(i, (hits[0].0.clone(), hits[0].1, true));
            if known(plugin).is_none() { *votes.entry(plugin.to_string()).or_default().entry(hits[0].0.clone()).or_default() += 1; }
        }
    }
    let mut plugin_platform: HashMap<String, String> = KNOWN_PLUGINS.iter().map(|(p, v)| (p.to_string(), v.to_string())).collect();
    for (p, v) in votes { if let Some((plat, _)) = v.into_iter().max_by_key(|(plat, n)| (*n, std::cmp::Reverse(plat.clone()))) { plugin_platform.insert(p, plat); } }
    for (i, g) in games.iter().enumerate() {
        if resolved.contains_key(&i) || g.plugin.as_deref() == Some(STEAM_PLUGIN_ID) { continue; }
        let plugin = g.plugin.as_deref();
        let platform: Option<Option<String>> = match plugin { Some(p) if plugin_platform.contains_key(p) => Some(Some(plugin_platform[p].clone())), Some(p) if p != NO_PLUGIN => Some(None), _ => Some(Some("custom".into())) };
        let platform = platform.unwrap();
        let Some(name) = g.name.as_deref().filter(|n| !n.is_empty()) else { continue };
        let n = norm(name); if n.is_empty() { continue; }
        let cands: Vec<(&String, i64)> = by_name.iter().filter(|((pl, nn), _)| *nn == n && platform.as_ref().map_or(true, |p| p == pl)).map(|((pl, _), a)| (pl, *a)).collect();
        if cands.len() == 1 { resolved.insert(i, (cands[0].0.clone(), cands[0].1, false)); }
    }
    let unmatched = (0..games.len()).filter(|i| !resolved.contains_key(i)).collect();
    Ok(Matched { matches: resolved.into_iter().map(|(game, (platform, appid, by_id))| Match { game, appid, platform, by_id }).collect(), unmatched })
}

// ───────────────────────── applying ─────────────────────────

/// What the user chose.
#[derive(Clone, Debug, Default)]
pub struct Options {
    pub date_added: bool, pub last_played: bool, pub playtime: bool,
    /// Replace PlayDate's values (else only fill blanks; Date Added keeps the earlier one).
    pub overwrite: bool,
    /// Ignore a Date Added shared by this many matched games or more (a bulk import, not a purchase date). 0 = keep all.
    pub skip_bulk_dates: usize,
    /// Playnite status name (lower case) -> the status to give a game still at Never Played. Names not listed are left alone.
    pub status_map: HashMap<String, String>,
    /// Playnite category name -> the group to add matched games to. Names not listed are left alone. "Favorite" and "Hidden" are available as categories.
    pub groups: HashMap<String, String>,
}
/// PlayDate's default for each of Playnite's own status names (users can rename or change them).
pub fn default_status(name: &str) -> Option<&'static str> {
    match name.trim().to_lowercase().as_str() { "beaten" | "completed" => Some("Beaten"), "played" => Some("Unfinished"), "abandoned" => Some("Won't Play"), "plan to play" | "not played" => Some("Never Played"), _ => None }
}
pub const PD_STATUSES: [&str; 5] = ["Never Played", "Unfinished", "Beaten", "Completed", "Won't Play"];

/// One game's values after merging copies (earliest Date Added, else the first seen).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Merged { pub date_added: Option<String>, pub last_played: Option<i64>, pub playtime: Option<i64>, pub status: Option<String>, pub categories: Vec<String> }

pub fn merge(backup: &Backup, matched: &Matched) -> BTreeMap<i64, Merged> {
    let mut out: BTreeMap<i64, Merged> = BTreeMap::new();
    for m in &matched.matches {
        let g = &backup.games[m.game];
        let cur = out.entry(m.appid).or_default();
        if let Some(d) = &g.date_added { match &cur.date_added { Some(c) if c <= d => {} _ => cur.date_added = Some(d.clone()) } }
        if cur.last_played.is_none() { cur.last_played = g.last_played; }
        if cur.playtime.is_none() { cur.playtime = g.playtime; }
        if cur.status.is_none() { cur.status = g.status_id.as_ref().and_then(|id| backup.statuses.get(id)).cloned(); }
        let mut cats: Vec<String> = g.category_ids.iter().filter_map(|id| backup.categories.get(id)).cloned().collect();
        if g.favorite { cats.push("Favorite".into()); } if g.hidden { cats.push("Hidden".into()); }
        for c in cats { if !cur.categories.contains(&c) { cur.categories.push(c); } }
    }
    out
}

#[derive(Debug, Default, PartialEq)]
pub struct Report { pub date_added: usize, pub last_played: usize, pub playtime: usize, pub status: usize, pub grouped: usize, pub skipped_bulk_dates: usize, pub added: usize }

// ───────────────────────── games the library does not have yet ─────────────────────────

/// A game in the backup that this library lacks and the import may add (Steam by appid, GOG by product id: the stores whose Playnite id is the store's own).
#[derive(Clone, Debug, PartialEq)]
pub struct NewGame { pub game: usize, pub platform: &'static str, pub platform_id: String, pub name: String }
/// What the import would add, and what it leaves out and why.
#[derive(Debug, Default, PartialEq)]
pub struct NewPlan { pub add: Vec<NewGame>, pub junk: usize, pub blacklisted: usize, pub other_store: usize }
/// A game that was added: its place in the backup, its new appid, its platform and name.
#[derive(Clone, Debug, PartialEq)]
pub struct AddedGame { pub game: usize, pub appid: i64, pub platform: &'static str, pub name: String }

/// Titles of betas, test servers, demos, tools and goodie packs that Steam lists as apps but are not games, on top of the Junk Finder's own patterns.
pub(crate) fn looks_like_junk(name: &str) -> bool {
    use std::sync::OnceLock;
    static R: OnceLock<regex::Regex> = OnceLock::new();
    let re = R.get_or_init(|| regex::Regex::new(r"(?i)\b(test\s+(server|zone)|staging\s+branch|experimental\s+(server|branch)|goodies|goodie\s+pack|playtest|digital\s+comics|jingle\s+jam|megapicker|\(beta\)|ctb)\b|\b(3dmark|pcmark|vrmark|displayfusion|facerig)\b|-\s*test\s*$|\b(rpg\s+maker(\s+\w+)*|clickteam\s+fusion[\d. ]*|gameguru(\s+classic)?|appgamekit(\s+classic)?|marmoset\s+hexels[\d ]*)\s*$").unwrap());
    !crate::junk::reasons(name).is_empty() || re.is_match(name)
}

/// The backup's games that matched nothing, as far as they can be added: a Steam game (its appid) or a GOG game (its product id) that is not blacklisted and does not look like a
/// beta, demo, test server or tool. Other stores' games are only counted.
pub fn plan_new(backup: &Backup, matched: &Matched, conn: &Connection) -> NewPlan {
    let mut plan = NewPlan::default();
    let mut seen: HashSet<(&'static str, String)> = HashSet::new();
    for &i in &matched.unmatched {
        let g = &backup.games[i];
        let plat = g.plugin.as_deref().and_then(known);
        let (platform, id): (&'static str, String) = match plat {
            Some("steam") => match g.game_id.trim().parse::<i64>() { Ok(a) if a > 0 => ("steam", a.to_string()), _ => { plan.other_store += 1; continue } },
            Some("gog") if !g.game_id.trim().is_empty() => ("gog", g.game_id.trim().to_string()),
            _ => { plan.other_store += 1; continue }
        };
        let name = g.name.clone().unwrap_or_default().trim().to_string();
        if name.is_empty() { continue; }
        if !seen.insert((platform, id.clone())) { continue; }
        let blacklisted = if platform == "steam" { conn.query_row("SELECT 1 FROM blacklist WHERE appid = ?", [id.parse::<i64>().unwrap_or(0)], |_| Ok(())).is_ok() }
                          else { conn.query_row("SELECT 1 FROM blacklist WHERE platform = ?1 AND platform_id = ?2", params![platform, &id], |_| Ok(())).is_ok() };
        if blacklisted { plan.blacklisted += 1; continue; }
        if looks_like_junk(&name) { plan.junk += 1; continue; }
        let present = if platform == "steam" { conn.query_row("SELECT 1 FROM games WHERE appid = ?", [id.parse::<i64>().unwrap_or(0)], |_| Ok(())).is_ok() }
                      else { conn.query_row("SELECT 1 FROM games WHERE platform = ?1 AND platform_id = ?2", params![platform, &id], |_| Ok(())).is_ok() };
        if present { continue; }
        plan.add.push(NewGame { game: i, platform, platform_id: id, name });
    }
    plan
}

/// Adds the planned games as placeholder rows (the caller runs the usual fill-in jobs on them) and says what became of each.
pub fn add_new_games(conn: &mut Connection, add: &[NewGame], now: i64) -> Result<Vec<AddedGame>, String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let mut out = vec![];
    for n in add {
        let r = if n.platform == "steam" { crate::addgame::add_steam(&tx, n.platform_id.parse().unwrap_or(0), &n.name, now, false) } else { crate::addgame::add_store_game(&tx, n.platform, &n.platform_id, &n.name, now) }?;
        if let crate::addgame::Added::New(appid) = r { out.push(AddedGame { game: n.game, appid, platform: n.platform, name: n.name.clone() }); }
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(out)
}

/// The whole import: when `add_missing`, the games the library lacks are added first, then the chosen values are written for the matched and the added games alike, and the last import
/// can be undone (the added games are removed again). Returns the report and the games added.
#[allow(clippy::too_many_arguments)]
pub fn import(conn: &mut Connection, root: &Path, backup: &Backup, matched: &Matched, o: &Options, add_missing: bool, now: i64) -> Result<(Report, Vec<AddedGame>), String> {
    let mut merged = merge(backup, matched);
    let mut added = vec![];
    if add_missing {
        let plan = plan_new(backup, matched, conn);
        added = add_new_games(conn, &plan.add, now)?;
        let extra = Matched { matches: added.iter().map(|a| Match { game: a.game, appid: a.appid, platform: a.platform.to_string(), by_id: true }).collect(), unmatched: vec![] };
        for (k, v) in merge(backup, &extra) { merged.insert(k, v); }
    }
    let ids: Vec<i64> = added.iter().map(|a| a.appid).collect();
    let mut rep = apply_added(conn, root, &merged, o, &ids)?;
    rep.added = added.len();
    Ok((rep, added))
}

fn undo_path(root: &Path) -> std::path::PathBuf { root.join("playnite_undo.json") }
pub fn has_undo(root: &Path) -> bool { undo_path(root).is_file() }

/// Writes the chosen values for the matched games. The values every touched game had before go to `playnite_undo.json` first (one level: a second import replaces it).
pub fn apply(conn: &mut Connection, root: &Path, merged: &BTreeMap<i64, Merged>, o: &Options) -> Result<Report, String> { apply_added(conn, root, merged, o, &[]) }

/// `apply`, also recording the games an import added (`added`) so that undoing removes them.
pub fn apply_added(conn: &mut Connection, root: &Path, merged: &BTreeMap<i64, Merged>, o: &Options, added: &[i64]) -> Result<Report, String> {
    // dates shared by many games are a bulk import's, not a purchase date
    let mut per_day: HashMap<&str, usize> = HashMap::new();
    for m in merged.values() { if let Some(d) = &m.date_added { *per_day.entry(d.as_str()).or_default() += 1; } }
    let bulk = |d: &str| o.skip_bulk_dates > 0 && per_day.get(d).copied().unwrap_or(0) >= o.skip_bulk_dates;
    let mut rep = Report::default();
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let mut undo = serde_json::Map::new();
    for (appid, m) in merged {
        // the numbers are read as they are stored: older imports left '' (text) in some playtime cells, which a typed read refuses
        let flex = |r: &rusqlite::Row, i: usize| -> rusqlite::Result<Value> { Ok(match r.get_ref(i)? { rusqlite::types::ValueRef::Integer(n) => json!(n), rusqlite::types::ValueRef::Real(f) => json!(f as i64), rusqlite::types::ValueRef::Text(t) => json!(String::from_utf8_lossy(t)), _ => Value::Null }) };
        let before = tx.query_row("SELECT date_added, last_played, playtime_forever, completion_status, `groups` FROM games WHERE appid = ?", [appid], |r| Ok(json!({
            "date_added": flex(r, 0)?, "last_played": flex(r, 1)?, "playtime_forever": flex(r, 2)?,
            "completion_status": r.get::<_, Option<String>>(3)?, "groups": r.get::<_, Option<String>>(4)? }))).map_err(|e| e.to_string())?;
        let mut touched = false;
        if o.date_added { if let Some(d) = &m.date_added {
            if bulk(d) { rep.skipped_bulk_dates += 1; }
            else if let Some(ts) = crate::date_to_ts(&Value::String(d.clone())) {
                let n = if o.overwrite { tx.execute("UPDATE games SET date_added = ? WHERE appid = ?", params![ts, appid]) }
                        else { tx.execute("UPDATE games SET date_added = ? WHERE appid = ? AND (date_added IS NULL OR date_added = '' OR date_added = 0 OR typeof(date_added) = 'text' OR date_added > ?)", params![ts, appid, ts]) }.map_err(|e| e.to_string())?;
                rep.date_added += n; touched |= n > 0;
            }
        } }
        if o.last_played { if let Some(v) = m.last_played {
            let n = tx.execute(&format!("UPDATE games SET last_played = ? WHERE appid = ?{}", if o.overwrite { "" } else { " AND (last_played IS NULL OR last_played = '' OR last_played = 0)" }), params![v, appid]).map_err(|e| e.to_string())?;
            rep.last_played += n; touched |= n > 0;
        } }
        if o.playtime { if let Some(v) = m.playtime {
            let n = tx.execute(&format!("UPDATE games SET playtime_forever = ? WHERE appid = ?{}", if o.overwrite { "" } else { " AND (playtime_forever IS NULL OR playtime_forever = '' OR playtime_forever = 0)" }), params![v, appid]).map_err(|e| e.to_string())?;
            rep.playtime += n; touched |= n > 0;
        } }
        if let Some(to) = m.status.as_ref().and_then(|s| o.status_map.get(&s.trim().to_lowercase())) {
            if *to != "Never Played" || false {
                let n = tx.execute("UPDATE games SET completion_status = ? WHERE appid = ? AND (completion_status IS NULL OR completion_status = '' OR completion_status = 'Never Played')", params![to, appid]).map_err(|e| e.to_string())?;
                rep.status += n; touched |= n > 0;
            }
        }
        let want: Vec<&String> = m.categories.iter().filter_map(|c| o.groups.get(c)).collect();
        if !want.is_empty() {
            let cur: String = before["groups"].as_str().unwrap_or("").to_string();
            let mut list: Vec<String> = cur.split(',').filter(|s| !s.is_empty()).map(String::from).collect();
            let n0 = list.len();
            for g in want { if !list.contains(g) { list.push(g.clone()); } }
            if list.len() > n0 { tx.execute("UPDATE games SET `groups` = ? WHERE appid = ?", params![list.join(","), appid]).map_err(|e| e.to_string())?; rep.grouped += 1; touched = true; }
        }
        if touched { undo.insert(appid.to_string(), before); }
    }
    if !undo.is_empty() || !added.is_empty() {
        let body = json!({ "rows": undo, "added": added });
        std::fs::write(undo_path(root), serde_json::to_string(&body).map_err(|e| e.to_string())?).map_err(|e| format!("Could not save the undo file: {e}"))?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(rep)
}

/// Puts back what the last apply changed. Returns the number of games restored.
pub fn undo(conn: &mut Connection, root: &Path) -> Result<usize, String> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(undo_path(root)).map_err(|_| "There is nothing to undo.".to_string())?).map_err(|_| "The undo file is damaged.".to_string())?;
    let rows = v.get("rows").and_then(|r| r.as_object()).ok_or("The undo file is damaged.")?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    for (appid, b) in rows {
        let appid: i64 = appid.parse().map_err(|_| "The undo file is damaged.".to_string())?;
        let s = |k: &str| b.get(k).and_then(|x| x.as_str()).map(String::from);
        // a number goes back as a number, text (an old '') as text, null as null
        let i = |k: &str| -> rusqlite::types::Value { match b.get(k) { Some(Value::Number(n)) => rusqlite::types::Value::Integer(n.as_i64().unwrap_or(0)), Some(Value::String(t)) => rusqlite::types::Value::Text(t.clone()), _ => rusqlite::types::Value::Null } };
        tx.execute("UPDATE games SET date_added = ?, last_played = ?, playtime_forever = ?, completion_status = ?, `groups` = ? WHERE appid = ?", params![i("date_added"), i("last_played"), i("playtime_forever"), s("completion_status"), s("groups"), appid]).map_err(|e| e.to_string())?;
    }
    let mut removed = 0;
    let mut gone: Vec<i64> = Vec::new();
    for a in v.get("added").and_then(|a| a.as_array()).map(|a| a.as_slice()).unwrap_or(&[]).iter().filter_map(|a| a.as_i64()) {
        let n = tx.execute("DELETE FROM games WHERE appid = ?", [a]).map_err(|e| e.to_string())?;
        removed += n;
        if n > 0 { gone.push(a); }
    }
    tx.commit().map_err(|e| e.to_string())?;
    // the covers of the games the import added go with them (a normal delete does the same); games that were already there are never in this list
    for a in gone { for sub in ["vertical", "horizontal", "icons"] { let _ = std::fs::remove_file(root.join("static/img/library").join(sub).join(format!("{a}.jpg"))); } }
    let _ = std::fs::remove_file(undo_path(root));
    Ok(rows.len() + removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    const GOG: &str = "aebe8b7c-6dc3-4a66-af31-e7375c6b5e9e";
    fn guid_bytes(s: &str) -> Vec<u8> { let h: Vec<u8> = (0..32).step_by(2).map(|i| u8::from_str_radix(&s.replace('-', "")[i..i + 2], 16).unwrap()).collect(); vec![h[3], h[2], h[1], h[0], h[5], h[4], h[7], h[6], h[8], h[9], h[10], h[11], h[12], h[13], h[14], h[15]] }
    fn el(t: u8, key: &str, payload: &[u8]) -> Vec<u8> { let mut v = vec![t]; v.extend(key.as_bytes()); v.push(0); v.extend(payload); v }
    fn string(key: &str, s: &str) -> Vec<u8> { let mut p = ((s.len() + 1) as i32).to_le_bytes().to_vec(); p.extend(s.as_bytes()); p.push(0); el(0x02, key, &p) }
    fn bin(key: &str, g: &str) -> Vec<u8> { let mut p = 16i32.to_le_bytes().to_vec(); p.push(4); p.extend(guid_bytes(g)); el(0x05, key, &p) }
    fn doc(body: Vec<u8>) -> Vec<u8> { let mut v = ((body.len() + 5) as i32).to_le_bytes().to_vec(); v.extend(body); v.push(0); v }
    fn ms(date: &str) -> i64 { let p: Vec<i64> = date.split('-').map(|x| x.parse().unwrap()).collect(); let (y, m, d) = (p[0], p[1], p[2]); let y2 = if m <= 2 { y - 1 } else { y }; let era = y2.div_euclid(400); let yoe = y2 - era * 400; let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; (era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468) * 86_400_000 }
    #[allow(clippy::too_many_arguments)]
    fn game(id: &str, plugin: &str, name: &str, added: Option<&str>, play: Option<i64>, status: Option<&str>, cats: &[&str], fav: bool, pad: usize) -> Vec<u8> {
        let mut b = el(0x02, "Description", &{ let mut p = ((pad + 1) as i32).to_le_bytes().to_vec(); p.extend(vec![b'x'; pad]); p.push(0); p });
        b.extend(string("Name", name)); b.extend(string("GameId", id)); b.extend(bin("PluginId", plugin));
        if let Some(a) = added { b.extend(el(0x09, "Added", &ms(a).to_le_bytes())); }
        if let Some(p) = play { b.extend(el(0x12, "Playtime", &p.to_le_bytes())); }
        if let Some(s) = status { b.extend(bin("CompletionStatusId", s)); }
        if !cats.is_empty() { let mut inner = vec![]; for (i, c) in cats.iter().enumerate() { inner.extend(bin(&i.to_string(), c)); } b.extend(el(0x04, "CategoryIds", &doc(inner))); }
        if fav { b.extend(el(0x08, "Favorite", &[1])); }
        doc(b)
    }
    fn page(pid: u32, ty: u8, nxt: u32, count: u16, content: &[u8]) -> Vec<u8> {
        let mut h = pid.to_le_bytes().to_vec(); h.push(ty); h.extend(0xFFFF_FFFFu32.to_le_bytes()); h.extend(nxt.to_le_bytes()); h.extend(count.to_le_bytes()); h.extend(0u16.to_le_bytes()); h.extend([0u8; 8]);
        h.extend(content); h.resize(PAGE, 0); h
    }
    fn litedb(docs: &[Vec<u8>]) -> Vec<u8> {
        let mut head = vec![0u8; PAGE]; head[25..52].copy_from_slice(b"** This is a LiteDB file **"); head[52] = 7;
        let mut pages = vec![head];
        for d in docs {
            let pid = pages.len() as u32;
            if d.len() <= 3000 { let mut blk = 0u16.to_le_bytes().to_vec(); blk.extend(0xFFFF_FFFFu32.to_le_bytes()); blk.extend((d.len() as u16).to_le_bytes()); blk.extend(d); pages.push(page(pid, 4, 0xFFFF_FFFF, 1, &blk)); }
            else {
                let chunks: Vec<&[u8]> = d.chunks(3000).collect(); let ids: Vec<u32> = (0..chunks.len() as u32).map(|k| pid + 1 + k).collect();
                let mut blk = 0u16.to_le_bytes().to_vec(); blk.extend(ids[0].to_le_bytes()); blk.extend(0u16.to_le_bytes()); pages.push(page(pid, 4, 0xFFFF_FFFF, 1, &blk));
                for (k, c) in chunks.iter().enumerate() { pages.push(page(ids[k], 5, if k + 1 < chunks.len() { ids[k + 1] } else { 0xFFFF_FFFF }, c.len() as u16, c)); }
            }
        }
        pages.concat()
    }
    const S_BEATEN: &str = "11111111-1111-1111-1111-111111111111";
    const S_PLAYED: &str = "22222222-2222-2222-2222-222222222222";
    const C_RPG: &str = "33333333-3333-3333-3333-333333333333";
    fn name_doc(id: &str, n: &str) -> Vec<u8> { let mut b = bin("_id", id); b.extend(string("Name", n)); doc(b) }
    fn zip_with(dir: &Path, files: &[(&str, Vec<u8>)]) -> std::path::PathBuf {
        let p = dir.join("b.zip"); let mut z = crate::zipwrite::ZipOut::new(std::fs::File::create(&p).unwrap());
        for (n, d) in files { z.add_bytes(n, d, false).unwrap(); }
        z.finish().unwrap(); p
    }
    fn library() -> (Connection, std::path::PathBuf) {
        let d = std::env::temp_dir().join(format!("zest_pl_{}_{:?}", std::process::id(), std::thread::current().id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        let p = d.join("games_1.db"); crate::schema::init_db(&p).unwrap();
        let c = Connection::open(&p).unwrap();
        c.execute("INSERT INTO games (appid, platform, platform_id, name, completion_status) VALUES (730,'steam',NULL,'Counter-Strike 2','Never Played'), (-5,'gog','1207666073','The Witcher','Never Played'), (-6,'gog','999','Hades','Unfinished')", []).unwrap();
        (c, d)
    }
    #[test] fn a_backup_is_read_matched_merged_and_applied_with_a_mapping_and_undone() {
        let (mut c, d) = library();
        let big = game("730", STEAM_PLUGIN_ID, "Counter-Strike 2", Some("2020-01-02"), Some(7200), Some(S_BEATEN), &[C_RPG], true, 9000);
        let wit = game("1207666073", GOG, "Witcher", Some("2021-03-04"), None, Some(S_PLAYED), &[], false, 0);
        let hades = game("abc", GOG, "Hades", None, None, Some(S_BEATEN), &[], false, 0);
        let zip = zip_with(&d, &[("library/games.db", litedb(&[big, wit, hades])), ("library/completionstatuses.db", litedb(&[name_doc(S_BEATEN, "Beaten"), name_doc(S_PLAYED, "Played")])), ("library/categories.db", litedb(&[name_doc(C_RPG, "RPG")]))]);
        let b = read_backup(&zip).unwrap();
        assert_eq!(b.games.len(), 3); assert_eq!(b.games[0].category_ids, vec![C_RPG.to_string()]); assert!(b.games[0].favorite); assert_eq!(b.games[0].plugin.as_deref(), Some(STEAM_PLUGIN_ID));
        assert_eq!(b.statuses[S_BEATEN], "Beaten"); assert_eq!(b.categories[C_RPG], "RPG"); assert_eq!(b.games[0].playtime, Some(120));
        let m = match_games(&b.games, &c).unwrap();
        assert_eq!(m.matches.len(), 3); assert!(m.matches[0].by_id && m.matches[1].by_id && !m.matches[2].by_id, "Hades has no id match here and is found by its title");
        let merged = merge(&b, &m);
        assert_eq!(merged[&730].status.as_deref(), Some("Beaten")); assert_eq!(merged[&730].categories, vec!["RPG".to_string(), "Favorite".to_string()]);
        let mut o = Options { date_added: true, playtime: true, last_played: true, ..Default::default() };
        for n in ["beaten", "played"] { o.status_map.insert(n.into(), default_status(n).unwrap().into()); }
        o.groups.insert("RPG".into(), "From Playnite: RPG".into());
        let r = apply(&mut c, &d, &merged, &o).unwrap();
        assert_eq!((r.date_added, r.playtime, r.status, r.grouped), (2, 1, 2, 1), "{r:?}");
        fn row(c: &Connection, a: i64) -> (Option<i64>, Option<i64>, Option<String>, Option<String>) { c.query_row("SELECT date_added, playtime_forever, completion_status, `groups` FROM games WHERE appid = ?", [a], |r| Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<i64>>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, Option<String>>(3)?))).unwrap() }
        assert_eq!(row(&c, 730), (Some(1577923200), Some(120), Some("Beaten".into()), Some("From Playnite: RPG".into())));
        assert_eq!(row(&c, -5).2.as_deref(), Some("Unfinished")); assert_eq!(row(&c, -6).2.as_deref(), Some("Unfinished"), "a status already set is never replaced");
        assert!(has_undo(&d)); assert_eq!(undo(&mut c, &d).unwrap(), 2);
        assert_eq!(row(&c, 730), (None, None, Some("Never Played".into()), None)); assert!(!has_undo(&d));
    }
    #[test] fn an_empty_text_playtime_from_an_old_import_is_read_filled_and_put_back() {
        let (mut c, d) = library();
        c.execute("UPDATE games SET playtime_forever = '', last_played = '' WHERE appid = 730", []).unwrap();
        let mut merged = BTreeMap::new();
        merged.insert(730, Merged { playtime: Some(120), last_played: Some(1_600_000_000), ..Default::default() });
        let o = Options { playtime: true, last_played: true, ..Default::default() };
        let r = apply(&mut c, &d, &merged, &o).unwrap();
        assert_eq!((r.playtime, r.last_played), (1, 1), "an empty text cell counts as blank: {r:?}");
        let v: (i64, i64) = c.query_row("SELECT playtime_forever, last_played FROM games WHERE appid = 730", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(v, (120, 1_600_000_000));
        undo(&mut c, &d).unwrap();
        let back: (String, String) = c.query_row("SELECT typeof(playtime_forever) || ':' || playtime_forever, typeof(last_played) || ':' || last_played FROM games WHERE appid = 730", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(back, ("text:".to_string(), "text:".to_string()), "put back exactly as it was");
    }
    #[test] fn dates_shared_by_many_games_are_skipped_on_request_and_blanks_only_fill() {
        let (mut c, d) = library();
        c.execute("UPDATE games SET date_added = 5 WHERE appid = 730", []).unwrap();
        let mut merged = BTreeMap::new();
        for a in [730, -5, -6] { merged.insert(a, Merged { date_added: Some("2020-01-02".into()), ..Default::default() }); }
        let o = Options { date_added: true, skip_bulk_dates: 3, ..Default::default() };
        assert_eq!(apply(&mut c, &d, &merged, &o).unwrap().skipped_bulk_dates, 3);
        let o = Options { date_added: true, ..Default::default() };
        let r = apply(&mut c, &d, &merged, &o).unwrap(); assert_eq!(r.date_added, 2, "730 already has an earlier date");
    }
    #[test] fn games_the_library_lacks_are_added_from_steam_and_gog_skipping_junk_and_the_blacklist_and_undone() {
        let (mut c, d) = library();
        c.execute("INSERT INTO blacklist (appid, name, date_blacklisted) VALUES (111, 'Gone', '2026-01-01')", []).unwrap();
        c.execute("INSERT INTO blacklist (appid, name, date_blacklisted, platform_id, platform) VALUES (-50, 'Old GOG', '2026-01-01', 'blk1', 'gog')", []).unwrap();
        let docs = vec![
            game("730", STEAM_PLUGIN_ID, "Counter-Strike 2", Some("2020-01-02"), None, None, &[], false, 0),        // already here
            game("222", STEAM_PLUGIN_ID, "It Takes Two", Some("2021-05-06"), Some(6000), Some(S_PLAYED), &[], false, 0),   // to add
            game("333", STEAM_PLUGIN_ID, "Some Game - Public Test", None, None, None, &[], false, 0),                  // junk
            game("444", STEAM_PLUGIN_ID, "Rust - Staging Branch", None, None, None, &[], false, 0),                    // junk (extra pattern)
            game("111", STEAM_PLUGIN_ID, "Gone", None, None, None, &[], false, 0),                                     // blacklisted
            game("2093619782", GOG, "Cyberpunk 2077", Some("2022-07-08"), None, None, &[], false, 0),                  // to add
            game("blk1", GOG, "Old GOG", None, None, None, &[], false, 0),                                             // blacklisted by id
            game("e1", "85dd7072-2f20-4e76-a007-41035e390724", "The Sims 4", None, None, None, &[], false, 0),        // a store that is not Steam or GOG
        ];
        let zip = zip_with(&d, &[("library/games.db", litedb(&docs)), ("library/completionstatuses.db", litedb(&[name_doc(S_PLAYED, "Played")]))]);
        let b = read_backup(&zip).unwrap();
        let m = match_games(&b.games, &c).unwrap();
        let plan = plan_new(&b, &m, &c);
        assert_eq!(plan.add.iter().map(|n| (n.platform, n.platform_id.as_str(), n.name.as_str())).collect::<Vec<_>>(), vec![("steam", "222", "It Takes Two"), ("gog", "2093619782", "Cyberpunk 2077")]);
        assert_eq!((plan.junk, plan.blacklisted, plan.other_store), (2, 2, 1), "{plan:?}");
        let mut o = Options { date_added: true, playtime: true, ..Default::default() };
        o.status_map.insert("played".into(), "Unfinished".into());
        let (rep, added) = import(&mut c, &d, &b, &m, &o, true, 1_800_000_000).unwrap();
        assert_eq!(rep.added, 2); assert_eq!(added.len(), 2);
        let st = added.iter().find(|a| a.platform == "steam").unwrap(); let gg = added.iter().find(|a| a.platform == "gog").unwrap();
        assert_eq!(st.appid, 222);
        let r: (String, i64, Option<i64>, Option<String>) = c.query_row("SELECT name, date_added, playtime_forever, completion_status FROM games WHERE appid = 222", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap();
        assert_eq!((r.0.as_str(), r.1, r.2, r.3.as_deref()), ("It Takes Two", 1620259200, Some(100), Some("Unfinished")), "the added game takes the backup's date, playtime and status");
        let g: (String, Option<String>, i64) = c.query_row("SELECT platform, platform_id, date_added FROM games WHERE appid = ?", [gg.appid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!((g.0.as_str(), g.1.as_deref(), g.2), ("gog", Some("2093619782"), 1657238400)); assert!(gg.appid < 0);
        // undoing puts the touched rows back and removes the added games
        assert!(has_undo(&d)); assert_eq!(undo(&mut c, &d).unwrap(), 5, "Counter-Strike 2 and the two added games were touched, and the two added games are removed");
        assert_eq!(c.query_row("SELECT COUNT(*) FROM games WHERE appid IN (222, ?)", [gg.appid], |r| r.get::<_, i64>(0)).unwrap(), 0);
        // without the tick nothing is added
        let (rep2, added2) = import(&mut c, &d, &b, &m, &o, false, 1_800_000_000).unwrap();
        assert_eq!((rep2.added, added2.len()), (0, 0));
    }
    /// A real backup against a COPY of a library, end to end, printing what happened: `ZEST_PN_ZIP=<zip> ZEST_PN_DB=<copy of games db> cargo test -p pd-filter real_backup -- --ignored --nocapture`.
    #[test] #[ignore] fn real_backup_on_a_copy() {
        let (Some(zip), Some(db)) = (std::env::var_os("ZEST_PN_ZIP"), std::env::var_os("ZEST_PN_DB")) else { return };
        let db = std::path::PathBuf::from(db); let root = db.parent().unwrap().to_path_buf();
        let mut c = Connection::open(&db).unwrap();
        let snap = |c: &Connection| -> Vec<(i64, String)> { c.prepare("SELECT appid, COALESCE(name,'') || '|' || quote(date_added) || '|' || quote(last_played) || '|' || quote(playtime_forever) || '|' || quote(completion_status) || '|' || quote(`groups`) FROM games ORDER BY appid").unwrap().query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().flatten().collect() };
        let diff = |a: &[(i64, String)], b: &[(i64, String)]| -> Vec<String> { let am: HashMap<i64, &String> = a.iter().map(|(k, v)| (*k, v)).collect(); let bm: HashMap<i64, &String> = b.iter().map(|(k, v)| (*k, v)).collect();
            let mut out = vec![]; for (k, v) in b { match am.get(k) { None => out.push(format!("+ {k} {v}")), Some(o) if *o != v => out.push(format!("~ {k}\n    was {o}\n    now {v}")), _ => {} } } for (k, v) in a { if !bm.contains_key(k) { out.push(format!("- {k} {v}")); } } out };
        let b = read_backup(Path::new(&zip)).unwrap();
        let m = match_games(&b.games, &c).unwrap();
        println!("backup {} games: {} matched ({} by id), {} not in library", b.games.len(), m.matches.len(), m.matches.iter().filter(|x| x.by_id).count(), m.unmatched.len());
        let plan = plan_new(&b, &m, &c);
        println!("would add {} (junk {}, blacklisted {}, other store {})", plan.add.len(), plan.junk, plan.blacklisted, plan.other_store);
        let before = snap(&c);
        { let mut h: HashMap<&str, usize> = HashMap::new(); for m in &m.matches { if let Some(d) = &b.games[m.game].date_added { *h.entry(d.as_str()).or_default() += 1; } }
          let mut v: Vec<_> = h.into_iter().collect(); v.sort_by(|x, y| y.1.cmp(&x.1)); println!("Added dates in the backup (matched games): {} distinct; most shared: {:?}", v.len(), &v[..v.len().min(6)]);
          println!("matched games with an Added date: {}, with Playtime: {}, with LastActivity: {}", m.matches.iter().filter(|x| b.games[x.game].date_added.is_some()).count(), m.matches.iter().filter(|x| b.games[x.game].playtime.is_some()).count(), m.matches.iter().filter(|x| b.games[x.game].last_played.is_some()).count()); }
        let mut o = Options { date_added: true, last_played: true, playtime: true, skip_bulk_dates: 10, ..Default::default() };
        for n in ["not played", "played"] { o.status_map.insert(n.into(), default_status(n).unwrap().into()); }
        // 1. fill blanks
        let (rep, _) = import(&mut c, &root, &b, &m, &o, false, 1_800_000_000).unwrap();
        println!("\n== FILL: {rep:?}"); let after1 = snap(&c); let d1 = diff(&before, &after1);
        println!("{} rows changed; the six test games:", d1.len());
        for id in [405640i64, 241910, 474960, -2841, -2859, 439190] { if let Some(l) = d1.iter().find(|l| l.starts_with(&format!("~ {id}\n"))) { println!("{l}"); } else { println!("~ {id}: unchanged"); } }
        println!("first 12 other changes:"); for l in d1.iter().filter(|l| ![405640i64, 241910, 474960, -2841, -2859, 439190].iter().any(|id| l.starts_with(&format!("~ {id}\n")))).take(12) { println!("{l}"); }
        let n = undo(&mut c, &root).unwrap(); let back1 = snap(&c);
        println!("UNDO put back {n}; library identical to before: {}", back1 == before);
        // 1b. fill without ignoring shared dates
        let o1 = Options { skip_bulk_dates: 0, ..o.clone() };
        let (rep, _) = import(&mut c, &root, &b, &m, &o1, false, 1_800_000_000).unwrap();
        println!("\n== FILL, shared dates kept: {rep:?}"); let d1b = diff(&before, &snap(&c)); println!("{} rows changed; the six test games:", d1b.len());
        for id in [405640i64, 241910, 474960, -2841, -2859, 439190] { if let Some(l) = d1b.iter().find(|l| l.starts_with(&format!("~ {id}\n"))) { println!("{l}"); } else { println!("~ {id}: unchanged"); } }
        undo(&mut c, &root).unwrap(); assert!(snap(&c) == before);
        // 2. overwrite
        let o2 = Options { overwrite: true, ..o.clone() };
        let (rep, _) = import(&mut c, &root, &b, &m, &o2, false, 1_800_000_000).unwrap();
        println!("\n== OVERWRITE: {rep:?}"); let d2 = diff(&before, &snap(&c)); println!("{} rows changed", d2.len());
        let n = undo(&mut c, &root).unwrap(); println!("UNDO put back {n}; identical: {}", snap(&c) == before);
        // 3. add the missing games
        let (rep, added) = import(&mut c, &root, &b, &m, &o, true, 1_800_000_000).unwrap();
        println!("\n== FILL + ADD: {rep:?}, added {}", added.len());
        for a in added.iter().take(60) { println!("  + {} {} {}", a.platform, a.appid, a.name); }
        let n = undo(&mut c, &root).unwrap(); println!("UNDO put back or removed {n}; identical: {}", snap(&c) == before);
    }
    /// Writes a small backup for headless checks of the dialog: `ZEST_FIXTURE_OUT=<zip> ZEST_FIXTURE_APPIDS=730,440 cargo test -p pd-filter write_fixture -- --ignored`.
    #[test] #[ignore] fn write_fixture() {
        let (Some(out), Some(ids)) = (std::env::var_os("ZEST_FIXTURE_OUT"), std::env::var("ZEST_FIXTURE_APPIDS").ok()) else { return };
        let mut docs = vec![];
        for (i, id) in ids.split(',').enumerate() {
            let st = [S_BEATEN, S_PLAYED, S_BEATEN][i % 3];
            docs.push(game(id, STEAM_PLUGIN_ID, &format!("Game {id}"), Some(if i % 2 == 0 { "2019-05-06" } else { "2020-07-08" }), Some(3600 * (i as i64 + 1)), Some(st), &[C_RPG], i == 0, 0));
        }
        docs.push(game("zzz", GOG, "Not in this library", Some("2021-01-01"), None, None, &[], false, 0));
        let dir = std::path::Path::new(&out).parent().unwrap().to_path_buf();
        let z = zip_with(&dir, &[("library/games.db", litedb(&docs)), ("library/completionstatuses.db", litedb(&[name_doc(S_BEATEN, "Beaten"), name_doc(S_PLAYED, "Plan to Play")])), ("library/categories.db", litedb(&[name_doc(C_RPG, "RPG")]))]);
        std::fs::rename(z, out).unwrap();
    }
    #[test] fn guids_and_dates_read_like_dotnet_and_python() {
        assert_eq!(guid_str(&guid_bytes(STEAM_PLUGIN_ID)).unwrap(), STEAM_PLUGIN_ID);
        assert_eq!(civil_date(1579046400), "2020-01-15");
        assert_eq!(default_status("Plan to Play"), Some("Never Played")); assert_eq!(default_status("Custom"), None);
    }
}
