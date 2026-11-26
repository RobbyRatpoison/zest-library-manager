//! Battle.net's library from the client's own files in its Wine prefix (port of PlayDate's `plugins/battle_net/{catalog,product_db,local_sync}.py`): the installed products from the
//! Agent's `product.db` (a protobuf file, read by wire type), plus the account's licence ids from `CachedData.db` matched against the catalog fragments the client caches.
//! Checked against the real Python on a fake prefix (`tests/gen_bnet_golden.py`). Not wired into a source yet: this is the pure reading half.
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

type Row = (&'static str, &'static str, Option<i64>, &'static str, &'static str, bool, bool);
/// code, uid, title id, name, genres, free-to-play, licence trustworthy (false: every modern Call of Duty, whose free-access licences look like purchases).
pub const ROWS: &[Row] = &[
    ("WoW", "wow", Some(5730135), "World of Warcraft", "MMO,RPG", false, true),
    ("WoWC", "wow_classic", None, "World of Warcraft Classic", "MMO,RPG", false, true),
    ("D3", "diablo3", Some(17459), "Diablo III", "Action,RPG", false, true),
    ("OSI", "osi", Some(5198665), "Diablo II: Resurrected", "Action,RPG", false, true),
    ("D1", "diablo_i", Some(1146246220), "Diablo", "Action,RPG", false, true),
    ("Fen", "fenris", Some(4613486), "Diablo IV", "Action,RPG", false, true),
    ("ANBS", "anbs", Some(1095647827), "Diablo Immortal", "Action,RPG", true, true),
    ("S2", "s2", Some(21298), "StarCraft II", "Strategy,RTS", true, true),
    ("S1", "s1", Some(21297), "StarCraft: Remastered", "Strategy,RTS", false, true),
    ("SC", "starcraft", None, "StarCraft Anthology", "Strategy,RTS", false, true),
    ("W3", "w3", Some(22323), "Warcraft III: Reforged", "Strategy,RTS", false, true),
    ("W1", "warcraft_i", Some(1463898673), "Warcraft: Orcs & Humans", "Strategy,RTS", false, true),
    ("W2", "warcraft_ii", Some(1462911566), "Warcraft II: Battle.net Edition", "Strategy,RTS", false, true),
    ("W1R", "warcraft_i_remastered", Some(5714258), "Warcraft: Remastered", "Strategy,RTS", false, true),
    ("W2R", "warcraft_ii_remastered", Some(5714514), "Warcraft II: Remastered", "Strategy,RTS", false, true),
    ("GRY", "gryphon", Some(4674137), "Warcraft Rumble", "Strategy,Mobile", true, true),
    ("WTCG", "hs_beta", Some(1465140039), "Hearthstone", "Card Game,Strategy", true, true),
    ("Hero", "heroes", Some(1214607983), "Heroes of the Storm", "MOBA,Action", true, true),
    ("Pro", "prometheus", Some(5272175), "Overwatch", "Action,FPS,Multiplayer", true, true),
    ("VIPR", "viper", Some(1447645266), "Call of Duty: Black Ops 4", "Action,FPS,Multiplayer", false, false),
    ("ODIN", "odin", Some(1329875278), "Call of Duty: Modern Warfare", "Action,FPS,Multiplayer", false, false),
    ("LAZR", "lazarus", Some(1279351378), "Call of Duty: Modern Warfare 2 Campaign Remastered", "Action,FPS", false, false),
    ("FORE", "fore", Some(1179603525), "Call of Duty: Vanguard", "Action,FPS,Multiplayer", false, false),
    ("ZEUS", "zeus", Some(1514493267), "Call of Duty: Black Ops Cold War", "Action,FPS,Multiplayer", false, false),
    ("AUKS", "auks", Some(1096108883), "Call of Duty: Modern Warfare II", "Action,FPS,Multiplayer", false, false),
    ("PNTA", "pinta", None, "Call of Duty: Modern Warfare III", "Action,FPS,Multiplayer", false, false),
    ("WLBY", "wlby", Some(1464615513), "Crash Bandicoot 4: It's About Time", "Platformer", false, true),
    ("RTRO", "rtro", Some(1381257807), "Blizzard Arcade Collection", "Arcade", false, true),
    ("ARIS", "aris", Some(1095911763), "Doom: The Dark Ages", "Action,FPS", false, true),
    ("SCOR", "scorpio", Some(1396920146), "Sea of Thieves", "Action,Adventure", false, true),
    ("ARK", "arkansas", Some(4280907), "The Outer Worlds 2", "RPG,Action", false, true),
    ("LBRA", "libra", Some(1279414849), "Tony Hawk's Pro Skater 3 + 4", "Sports", false, true),
    ("AQUA", "aqua", Some(1095849281), "Avowed", "RPG,Action", false, true),
];
pub const PUBLISHER: &str = "Blizzard Entertainment";

pub struct Game { pub code: &'static str, pub uid: &'static str, pub title_id: Option<i64>, pub name: &'static str, pub genres: &'static str, pub free: bool, pub lic_ok: bool }
fn game(r: &'static Row) -> Game { Game { code: r.0, uid: r.1, title_id: r.2, name: r.3, genres: r.4, free: r.5, lic_ok: r.6 } }
pub fn by_code(code: &str) -> Option<Game> { ROWS.iter().find(|r| r.0.eq_ignore_ascii_case(code)).map(game) }
pub fn by_uid(uid: &str) -> Option<Game> { ROWS.iter().find(|r| r.1.eq_ignore_ascii_case(uid)).map(game) }
pub fn by_title_id(id: i64) -> Option<Game> { ROWS.iter().find(|r| r.2 == Some(id)).map(game) }
/// A display name reduced for loose matching: trademark signs and everything but letters and digits dropped, lower case.
pub fn normalize_name(name: &str) -> String { name.chars().filter(|c| !matches!(c, '\u{ae}' | '\u{2122}' | '\u{a9}')).flat_map(|c| c.to_lowercase()).filter(|c| c.is_ascii_alphanumeric()).collect() }
pub fn by_name(name: &str) -> Option<Game> { let n = normalize_name(name); ROWS.iter().rev().find(|r| normalize_name(r.3) == n).map(game) }

// ---------------------------------------------------------------- product.db (protobuf)

fn varint(b: &[u8], i: &mut usize) -> Option<u64> {
    let (mut r, mut shift) = (0u64, 0u32);
    loop { let byte = *b.get(*i)?; *i += 1; if shift < 64 { r |= ((byte & 0x7f) as u64) << shift; } if byte & 0x80 == 0 { return Some(r); } shift += 7; }
}
enum Val<'a> { Int(u64), Bytes(&'a [u8]) }
/// (field number, wire type, value) for each field of a message; None when the bytes are cut short (a corrupt file).
fn fields(b: &[u8]) -> Option<Vec<(u64, u8, Val<'_>)>> {
    let (mut i, mut out) = (0usize, vec![]);
    while i < b.len() {
        let tag = varint(b, &mut i)?; let (fnum, wt) = (tag >> 3, (tag & 7) as u8);
        match wt {
            0 => out.push((fnum, wt, Val::Int(varint(b, &mut i)?))),
            2 => { let n = varint(b, &mut i)? as usize; let end = i.checked_add(n)?; if end > b.len() { return None; } out.push((fnum, wt, Val::Bytes(&b[i..end]))); i = end; }
            1 => { let s = b.get(i..i + 8)?; out.push((fnum, wt, Val::Int(u64::from_le_bytes(s.try_into().ok()?)))); i += 8; }
            5 => { let s = b.get(i..i + 4)?; out.push((fnum, wt, Val::Int(u32::from_le_bytes(s.try_into().ok()?) as u64))); i += 4; }
            _ => return Some(out),   // groups: obsolete, never present
        }
    }
    Some(out)
}
fn first_bytes(b: &[u8], fnum: u64) -> Option<Vec<u8>> { fields(b)?.into_iter().find_map(|(f, w, v)| match v { Val::Bytes(x) if f == fnum && w == 2 => Some(x.to_vec()), _ => None }) }

#[derive(Debug, Clone, PartialEq)]
pub struct Install { pub uid: String, pub product_code: String, pub install_path: String, pub installed: bool, pub playable: bool }

/// `product_db.parse`: the products the Agent tracks. Empty on a missing, unreadable or corrupt file.
pub fn parse_product_db(path: &Path) -> Vec<Install> {
    let Ok(data) = std::fs::read(path) else { return vec![] };
    let Some(top) = fields(&data) else { return vec![] };
    let mut out = vec![];
    for (f, w, v) in top {
        let (1, 2, Val::Bytes(entry)) = (f, w, v) else { continue };
        let Some(fs) = fields(entry) else { return vec![] };
        let (mut uid, mut code, mut path, mut installed, mut playable) = (String::new(), String::new(), String::new(), false, false);
        for (ef, ew, ev) in fs {
            match (ef, ew, ev) {
                (1, 2, Val::Bytes(b)) => uid = String::from_utf8_lossy(b).to_string(),
                (2, 2, Val::Bytes(b)) => code = String::from_utf8_lossy(b).to_string(),
                (3, 2, Val::Bytes(b)) => if let Some(p) = first_bytes(b, 1) { path = String::from_utf8_lossy(&p).to_string(); },
                (4, 2, Val::Bytes(b)) => if let Some(base) = first_bytes(b, 1) {
                    let Some(bf) = fields(&base) else { return vec![] };
                    for (bfn, bwt, bv) in bf { if let (0, Val::Int(x)) = (bwt, bv) { if bfn == 1 { installed = x != 0; } else if bfn == 2 { playable = x != 0; } } }
                },
                _ => {}
            }
        }
        if !uid.is_empty() || !code.is_empty() { out.push(Install { uid, product_code: code, install_path: path.replace('\\', "/"), installed, playable }); }
    }
    out
}
pub fn agent_db_path(prefix: &Path) -> PathBuf { prefix.join("drive_c/ProgramData/Battle.net/Agent/product.db") }

// ---------------------------------------------------------------- local_sync

fn local_dir(prefix: &Path) -> PathBuf { prefix.join("drive_c/users/steamuser/AppData/Local/Battle.net") }

/// The licence ids of the signed-in account (`CachedData.db`); empty when the file or the key is missing.
pub fn owned_license_ids(prefix: &Path) -> BTreeSet<i64> {
    let db = local_dir(prefix).join("CachedData.db");
    if !db.is_file() { return BTreeSet::new(); }
    let Ok(c) = rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { return BTreeSet::new() };
    let Ok(text) = c.query_row("SELECT value FROM key_value_store WHERE key = ?", ["features_cached_data_points"], |r| r.get::<_, String>(0)) else { return BTreeSet::new() };
    serde_json::from_str::<Value>(&text).ok().and_then(|v| v.get("licenses").and_then(|l| l.as_array()).map(|a| a.iter().filter_map(|x| x.as_i64()).collect())).unwrap_or_default()
}

fn walk_license_ids(v: &Value, out: &mut BTreeSet<i64>) {
    match v {
        Value::Object(m) => for (k, x) in m {
            if k == "license_id" || k == "requires_licenses" {
                match x { Value::Number(n) => if let Some(i) = n.as_i64() { out.insert(i); }, Value::Array(a) => out.extend(a.iter().filter_map(|y| y.as_i64())), _ => {} }
            } else { walk_license_ids(x, out); }
        },
        Value::Array(a) => for x in a { walk_license_ids(x, out); },
        _ => {}
    }
}

/// Each program id (catalog code) with the licence ids that grant it, from every cached catalog fragment (`Cache/*/*/*`, JSON files only).
pub fn license_index(prefix: &Path) -> BTreeMap<String, BTreeSet<i64>> {
    let mut index: BTreeMap<String, BTreeSet<i64>> = BTreeMap::new();
    let dirs = |p: &Path| -> Vec<PathBuf> { std::fs::read_dir(p).map(|r| r.flatten().map(|e| e.path()).collect()).unwrap_or_default() };
    for a in dirs(&local_dir(prefix).join("Cache")) { for b in dirs(&a) { for f in dirs(&b) {
        let Ok(bytes) = std::fs::read(&f) else { continue };
        if bytes.first() != Some(&b'{') { continue; }
        let Ok(frag) = serde_json::from_slice::<Value>(&bytes) else { continue };
        if frag.get("fragment_id").is_none() { continue; }
        let Some(program) = frag.get("products").and_then(|p| p.as_array()).and_then(|ps| ps.iter().find_map(|p| p.get("base").and_then(|b| b.get("program_id")).and_then(|x| x.as_str()).filter(|s| !s.is_empty()))) else { continue };
        let mut lic = BTreeSet::new();
        for k in ["program_configuration", "installs"] { if let Some(x) = frag.get(k) { walk_license_ids(x, &mut lic); } }
        if !lic.is_empty() { index.entry(program.to_string()).or_default().extend(lic); }
    } } }
    index
}

#[derive(Debug, Clone, PartialEq)]
pub struct Found { pub code: String, pub uid: String, pub name: String, pub genres: String, pub installed: bool, pub install_path: String, /// `installed`, `license` or `both`.
    pub source: &'static str }

/// `local_sync.scan`: installed products (an unknown code is kept under its code so nothing vanishes) plus paid games the account's licences grant. Free-to-play titles and
/// the Call of Duty family (`lic_ok` false) only count when installed. Sorted by code here; PlayDate's order is its dict's.
pub fn scan(prefix: &Path) -> Vec<Found> {
    if !prefix.is_dir() { return vec![]; }
    let mut games: BTreeMap<String, Found> = BTreeMap::new();
    for row in parse_product_db(&agent_db_path(prefix)) {
        if row.product_code == "agent" || row.product_code == "bna" { continue; }
        let g = by_uid(&row.uid).or_else(|| by_code(&row.product_code));
        let raw = if row.product_code.is_empty() { row.uid.clone() } else { row.product_code.clone() };
        let code = g.as_ref().map(|g| g.code.to_string()).unwrap_or_else(|| raw.clone());
        games.insert(code.clone(), Found { code, uid: g.as_ref().map(|g| g.uid.to_string()).unwrap_or(row.uid.clone()), name: g.as_ref().map(|g| g.name.to_string()).unwrap_or(raw),
            genres: g.as_ref().map(|g| g.genres.to_string()).unwrap_or_default(), installed: row.installed || row.playable, install_path: row.install_path, source: "installed" });
    }
    let owned = owned_license_ids(prefix);
    if !owned.is_empty() {
        for (code, lic) in license_index(prefix) {
            if owned.is_disjoint(&lic) { continue; }
            let Some(g) = by_code(&code) else { continue };
            let key = g.code.to_string();
            if let Some(f) = games.get_mut(&key) { f.source = "both"; }
            else if !g.lic_ok { continue; }
            else { games.insert(key.clone(), Found { code: key, uid: g.uid.to_string(), name: g.name.to_string(), genres: g.genres.to_string(), installed: false, install_path: String::new(), source: "license" }); }
        }
    }
    games.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn the_scan_equals_the_real_python_on_a_fake_prefix() {
        let g: Value = serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/bnet_golden.json")).expect("run gen_bnet_golden.py")).unwrap();
        let prefix = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/bnet_prefix"));
        let mut want: Vec<Found> = g["scan"].as_array().unwrap().iter().map(|x| Found { code: x["code"].as_str().unwrap().into(), uid: x["uid"].as_str().unwrap().into(), name: x["name"].as_str().unwrap().into(), genres: x["genres"].as_str().unwrap().into(),
            installed: x["installed"].as_bool().unwrap(), install_path: x["install_path"].as_str().unwrap().into(), source: match x["source"].as_str().unwrap() { "installed" => "installed", "license" => "license", _ => "both" } }).collect();
        want.sort_by(|a, b| a.code.cmp(&b.code));
        let mut got = scan(prefix); got.sort_by(|a, b| a.code.cmp(&b.code));
        assert_eq!(got, want);
        assert_eq!(ROWS.len() as i64, g["rows"].as_i64().unwrap());
        for (name, code) in g["names"].as_object().unwrap() { assert_eq!(by_name(name).map(|x| x.code.to_string()), code.as_str().map(String::from), "{name}"); }
        for (name, norm) in g["normalize"].as_object().unwrap().iter().filter(|(n, _)| n.as_str() != "null") { assert_eq!(normalize_name(name), norm.as_str().unwrap()); }
    }
    #[test] fn a_missing_empty_or_cut_off_product_db_gives_nothing() {
        let d = std::env::temp_dir().join(format!("zest_bnet_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        assert!(parse_product_db(&d.join("nope")).is_empty() && scan(&d.join("nope")).is_empty());
        std::fs::write(d.join("p.db"), b"").unwrap(); assert!(parse_product_db(&d.join("p.db")).is_empty());
        std::fs::write(d.join("p.db"), [0x0a, 0x20, 0x0a, 0x03]).unwrap(); assert!(parse_product_db(&d.join("p.db")).is_empty(), "a length past the end of the file");
    }
}
