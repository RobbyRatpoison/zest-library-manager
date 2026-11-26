//! "Import Owned Games" for Xbox through Xodus Nexus (port of the second half of PlayDate's `plugins/xbox/nexus.py`). Xbox's title history lists everything ever played, not what the
//! account owns, so this asks Nexus's own backend (`xodus-cli library --json --market XX`, which keeps the credentials to itself) for the account's Store licences, keeps the ones that are
//! valid now (held directly, or through a Game Pass plan), looks them up in Microsoft's public display catalog, adds the games the library lacks and tags every owned game with the
//! "Owned (Microsoft Store)" or "Game Pass" group. Safe to repeat; never removes a game (a game the account no longer holds only loses the groups added here).
use crate::net::Http;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const GROUP_OWNED: &str = "Owned (Microsoft Store)";
pub const GROUP_GAMEPASS: &str = "Game Pass";
/// A game in the library that the account does not hold now (it was only played, or the licence has ended).
pub const GROUP_PLAYED: &str = "Played, not owned";
const PLANS: [&str; 6] = ["CFQ7TTC0KGQ8", "CFQ7TTC0KHS0", "CFQ7TTC0P85B", "CFQ7TTC0K5DJ", "CFQ7TTC0K6L8", "CFQ7TTC10QFD"];
const CATALOG_URL: &str = "https://displaycatalog.mp.microsoft.com/v7.0/products";
const PROFILES: [&str; 2] = [".config/Xodus Nexus", ".config/xodus-nexus"];

fn state(home: &Path) -> Value {
    for d in PROFILES { if let Some(v) = std::fs::read_to_string(home.join(d).join("launcher-state.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).filter(|v| v.is_object()) { return v; } }
    Value::Null
}
/// The Store market from Nexus's settings (two capital letters), else US.
pub fn market(home: &Path) -> String {
    let m = state(home).pointer("/settings/market").and_then(|v| v.as_str()).unwrap_or("US").to_uppercase();
    if m.len() == 2 && m.chars().all(|c| c.is_ascii_uppercase()) { m } else { "US".into() }
}
/// Nexus's current backend program, from the `current.json` Nexus keeps; it must lie inside the profile folder and be executable.
pub fn backend_binary(home: &Path) -> Option<PathBuf> {
    for d in PROFILES {
        let dir = home.join(d);
        let Some(p) = std::fs::read_to_string(dir.join("xodus-backends/current.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v.get("binaryPath")?.as_str().map(PathBuf::from)) else { continue };
        let (Ok(real), Ok(base)) = (std::fs::canonicalize(&p), std::fs::canonicalize(&dir)) else { continue };
        #[cfg(unix)] let exec = { use std::os::unix::fs::PermissionsExt; std::fs::metadata(&real).map_or(false, |m| m.is_file() && m.permissions().mode() & 0o111 != 0) };
        #[cfg(not(unix))] let exec = real.is_file();
        if real.starts_with(&base) && real != base && exec { return Some(p); }
    }
    None
}

fn store_id_only(s: &str) -> bool { s.len() == 12 && s.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()) }

/// The backend's answer: the last line that starts with `{` must be schema 1 for this market, with an `entitlements` list; rows keep their Store ID as key (only 12-character ids).
pub fn parse_library(stdout: &str, exit_ok: bool, market: &str) -> Result<HashMap<String, Value>, String> {
    if !exit_ok { return Err("Xodus Nexus could not load your account licences. Open Nexus, make sure you are signed in, then try again.".into()); }
    let line = stdout.lines().rev().find(|l| l.trim_start().starts_with('{')).unwrap_or("");
    let v: Value = serde_json::from_str(line).map_err(|_| "Xodus Nexus returned something unreadable.".to_string())?;
    let ok = v.is_object() && v.get("schema").and_then(|s| s.as_i64()) == Some(1) && v.get("market").and_then(|m| m.as_str()) == Some(market) && v.get("entitlements").map_or(false, |e| e.is_array());
    if !ok { return Err("Xodus Nexus returned an unexpected format (its backend may have changed).".into()); }
    Ok(v["entitlements"].as_array().unwrap().iter().filter_map(|r| { let id = r.get("productId")?.as_str()?; if r.is_object() && store_id_only(id) { Some((id.to_string(), r.clone())) } else { None } }).collect())
}

/// Runs the backend (on the host under Flatpak), 60 seconds at most: (exited cleanly, standard output).
pub fn run_backend(binary: &Path, market: &str) -> Result<(bool, String), String> {
    use std::io::Read;
    let args: Vec<String> = vec!["library".into(), "--json".into(), "--market".into(), market.to_string()];
    let (prog, a) = crate::sandbox::wrap(&binary.to_string_lossy(), &args, None, &[], crate::sandbox::in_flatpak());
    let mut child = std::process::Command::new(prog).args(a).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).spawn().map_err(|e| format!("Could not run the Xodus Nexus backend: {e}"))?;
    let mut out = child.stdout.take().ok_or("no output")?;
    let reader = std::thread::spawn(move || { let mut s = String::new(); let _ = out.read_to_string(&mut s); s });
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(st)) => return Ok((st.success(), reader.join().unwrap_or_default())),
            Ok(None) if start.elapsed() > Duration::from_secs(60) => { let _ = child.kill(); let _ = child.wait(); return Err("Xodus Nexus took too long to answer. Try again in a moment.".into()); }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(format!("Could not run the Xodus Nexus backend: {e}")),
        }
    }
}

/// `classify_entitlements`: `owned` or `gamepass` for each licence that is valid at `now` (a timestamp). A licence counts when Active, not a trial, not special, not shared and not expired;
/// one satisfied by another licence takes that one's kind (a Game Pass plan makes `gamepass`; a loop or a chain longer than 16 counts for nothing); a plan itself is never a game.
pub fn classify(entries: &HashMap<String, Value>, now: i64) -> HashMap<String, &'static str> {
    fn kind(entries: &HashMap<String, Value>, pid: &str, seen: &mut Vec<String>, now: i64) -> Option<&'static str> {
        let row = entries.get(pid)?;
        if seen.iter().any(|s| s == pid) || seen.len() >= 16 { return None; }
        let end = super::sources::xbox::parse_iso_ts(row.get("endDate").and_then(|e| e.as_str()).unwrap_or(""))?;
        let truthy = |k: &str| match row.get(k) { None | Some(Value::Null) => false, Some(Value::Bool(b)) => *b, Some(Value::String(s)) => !s.is_empty(), Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0), Some(Value::Array(a)) => !a.is_empty(), Some(Value::Object(o)) => !o.is_empty() };
        if row.get("status").and_then(|s| s.as_str()) != Some("Active") || truthy("isTrial") || truthy("special") || row.get("sharingSource").and_then(|s| s.as_str()) != Some("None") || end <= now { return None; }
        match row.get("satisfiedByProductId").and_then(|p| p.as_str()).filter(|p| !p.is_empty()) {
            None => if PLANS.contains(&pid) { None } else { Some("owned") },
            Some(parent) if PLANS.contains(&parent) => Some("gamepass"),
            Some(parent) => { seen.push(pid.to_string()); kind(entries, parent, seen, now) }
        }
    }
    entries.keys().filter_map(|pid| kind(entries, pid, &mut Vec::new(), now).map(|k| (pid.clone(), k))).collect()
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CatInfo { pub kind: Option<String>, pub title: Option<String>, pub publisher: Option<String>, pub developer: Option<String>, pub description: Option<String>, pub pfn: Option<String> }

/// What the display catalog's `Products` say about each product id.
pub fn parse_catalog(v: &Value) -> HashMap<String, CatInfo> {
    let mut out = HashMap::new();
    for p in v.get("Products").and_then(|p| p.as_array()).into_iter().flatten() {
        let Some(id) = p.get("ProductId").and_then(|i| i.as_str()) else { continue };
        let props = p.get("LocalizedProperties").and_then(|l| l.as_array()).and_then(|l| l.first()).cloned().unwrap_or(Value::Null);
        let text = |k: &str| props.get(k).and_then(|x| x.as_str()).map(String::from);
        let mut pfn = p.pointer("/Properties/PackageFamilyName").and_then(|x| x.as_str()).filter(|s| !s.is_empty()).map(String::from);
        if pfn.is_none() {
            'o: for dsa in p.get("DisplaySkuAvailabilities").and_then(|d| d.as_array()).into_iter().flatten() {
                for pkg in dsa.pointer("/Sku/Properties/Packages").and_then(|x| x.as_array()).into_iter().flatten() { if let Some(f) = pkg.get("PackageFamilyName").and_then(|x| x.as_str()).filter(|s| !s.is_empty()) { pfn = Some(f.to_string()); break 'o; } }
            }
        }
        out.insert(id.to_string(), CatInfo { kind: p.get("ProductType").and_then(|x| x.as_str()).map(String::from), title: text("ProductTitle"), publisher: text("PublisherName"), developer: text("DeveloperName"), description: text("ShortDescription"), pfn });
    }
    out
}

/// Microsoft's public display catalog, twenty ids at a time (no sign-in); a batch that fails is skipped.
pub fn lookup_catalog(http: &dyn Http, ids: &[String], market: &str) -> HashMap<String, CatInfo> {
    let mut out = HashMap::new();
    for chunk in ids.chunks(20) {
        let url = format!("{CATALOG_URL}?bigIds={}&market={}&languages=en-US", chunk.iter().map(|i| crate::net::enc(i)).collect::<Vec<_>>().join(","), crate::net::enc(market));
        match http.get(&url, &[], Duration::from_secs(30)) {
            Ok(r) if r.status < 400 => match serde_json::from_str::<Value>(&r.body) { Ok(v) => out.extend(parse_catalog(&v)), Err(_) => crate::net::log("Xbox: catalog lookup returned something unreadable") },
            Ok(r) => crate::net::log(&format!("Xbox: catalog lookup failed (HTTP {})", r.status)),
            Err(e) => crate::net::log(&format!("Xbox: catalog lookup failed: {e}")),
        }
    }
    out
}

/// A comma-separated group list with this module's two groups removed and `add` appended.
pub fn with_groups(current: &str, add: &[&str]) -> String {
    let mut g: Vec<String> = current.split(',').filter(|x| !x.is_empty() && *x != GROUP_OWNED && *x != GROUP_GAMEPASS && *x != GROUP_PLAYED).map(String::from).collect();
    for a in add { if !g.iter().any(|x| x == a) { g.push(a.to_string()); } }
    g.join(",")
}

/// What the NEXUS button does with Nexus's list (chosen each time it is pressed): `Group` only sets the groups, `Add` also adds the owned games the library lacks (the default,
/// and what the sync always did), `Replace` also deletes the account's games Nexus does not report (after the user approved the list once).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode { Group, Add, Replace }

pub fn read_state(dir: &Path) -> Value { std::fs::read_to_string(dir.join("state.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_else(|| serde_json::json!({})) }
/// The choice made when the NEXUS button was pressed, handed to the import that button starts (asked every time, never saved): the mode, and whether the list of games to delete was approved.
static NEXT: std::sync::Mutex<Option<(Mode, bool)>> = std::sync::Mutex::new(None);
pub fn request(mode: Mode, approved: bool) { *NEXT.lock().unwrap_or_else(|e| e.into_inner()) = Some((mode, approved)); }
fn take_request() -> (Mode, bool) { NEXT.lock().unwrap_or_else(|e| e.into_inner()).take().unwrap_or((Mode::Add, false)) }
/// The games a Replace sync would delete, waiting for the user's approval: `[{"name", "minutes"}]` (the Plugins dialog shows it; absent = nothing waiting).
pub fn pending_path(dir: &Path) -> PathBuf { dir.join("xbox_nexus_pending.json") }
pub fn read_pending(dir: &Path) -> Vec<(String, i64)> {
    std::fs::read_to_string(pending_path(dir)).ok().and_then(|t| serde_json::from_str::<Vec<Value>>(&t).ok()).unwrap_or_default().iter()
        .filter_map(|v| Some((v.get("name")?.as_str()?.to_string(), v.get("minutes").and_then(|m| m.as_i64()).unwrap_or(0)))).collect()
}

#[derive(Debug, Default, PartialEq)]
pub struct Outcome { pub added: usize, pub marked: usize, /// Deleted now.
    pub removed: usize, /// Games a Replace sync would delete but may not yet (not approved).
    pub pending: usize }

/// Adds the owned games the library lacks (real games with a title and a package only: bundles and umbrella entries can never launch) and sets the groups. Returns (added, marked).
pub fn apply(conn: &mut rusqlite::Connection, kinds: &HashMap<String, &'static str>, catalog: &HashMap<String, CatInfo>, now: i64) -> rusqlite::Result<(usize, usize)> {
    apply_mode(conn, kinds, catalog, now, Mode::Add, None).map(|o| (o.added, o.marked))
}

/// `apply` for a chosen mode. `remove` is `Some((data dir, approved))` for the deleting mode: with `approved` the games Nexus does not report are deleted (row and cached covers, never
/// blacklisted, so one Nexus lists later comes back); without it nothing is deleted and the list is written for the user to approve. Nothing is ever deleted from an empty Nexus answer.
pub fn apply_mode(conn: &mut rusqlite::Connection, kinds: &HashMap<String, &'static str>, catalog: &HashMap<String, CatInfo>, now: i64, mode: Mode, remove: Option<(&Path, bool)>) -> rusqlite::Result<Outcome> {
    let have: std::collections::HashSet<String> = { let mut q = conn.prepare("SELECT COALESCE(platform_slug, '') FROM games WHERE platform = 'xbox'")?; let v: std::collections::HashSet<String> = q.query_map([], |r| r.get::<_, String>(0))?.flatten().map(|s| s.to_uppercase()).collect(); v };
    let mut recs = vec![];
    let mut ids: Vec<&String> = kinds.keys().collect(); ids.sort();
    for pid in ids {
        if mode == Mode::Group || have.contains(pid.as_str()) { continue; }
        let Some(i) = catalog.get(pid) else { continue };
        let (Some("Game"), Some(title), Some(pfn)) = (i.kind.as_deref(), i.title.clone().filter(|t| !t.is_empty()), i.pfn.clone()) else { continue };
        recs.push(crate::importer::GameRecord { platform_id: format!("store:{pid}"), name: title, slug: Some(pid.clone()), appname: Some(pfn), date_added: Some(now),
            developers: i.developer.clone().or_else(|| i.publisher.clone()), publishers: i.publisher.clone(), description: i.description.clone(), ..Default::default() });
    }
    let added = if recs.is_empty() { 0 } else { crate::importer::import(conn, "xbox", &recs, now)?.added };
    let rows: Vec<(i64, String, String, String, i64)> = { let mut q = conn.prepare("SELECT appid, COALESCE(platform_slug, ''), COALESCE(groups, ''), COALESCE(name, ''), COALESCE(playtime_forever, 0) FROM games WHERE platform = 'xbox' AND COALESCE(platform_slug, '') != ''")?; let v: Vec<(i64, String, String, String, i64)> = q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?.flatten().collect(); v };
    let tx = conn.transaction()?;
    let mut marked = 0;
    let mut gone: Vec<(i64, String, i64)> = vec![];
    for (appid, slug, groups, name, minutes) in rows {
        let new = match kinds.get(&slug.to_uppercase()) {
            Some(k) => { marked += 1; with_groups(&groups, &[if *k == "owned" { GROUP_OWNED } else { GROUP_GAMEPASS }]) }
            None => { if mode == Mode::Replace && !kinds.is_empty() { gone.push((appid, name, minutes)); } with_groups(&groups, &[GROUP_PLAYED]) }
        };
        if new != groups { tx.execute("UPDATE games SET groups = ?1 WHERE appid = ?2", rusqlite::params![new, appid])?; }
    }
    let (mut removed, mut pending) = (0, 0);
    if let Some((dir, approved)) = remove {
        if gone.is_empty() { let _ = std::fs::remove_file(pending_path(dir)); }
        else if approved {
            let state = read_state(dir);
            let priority: Vec<String> = state.get("platform_priority").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|p| p.as_str().map(String::from)).collect()).filter(|v: &Vec<String>| !v.is_empty())
                .unwrap_or_else(|| crate::query::PLATFORM_PRIORITY_DEFAULT.iter().map(|s| s.to_string()).collect());
            for (appid, name, minutes) in &gone {
                tx.execute("DELETE FROM games WHERE appid = ?1", [appid])?;
                crate::dupes::on_delete(&tx, *appid, &priority)?;
                for sub in ["vertical", "horizontal", "icons"] { let _ = std::fs::remove_file(dir.join("static/img/library").join(sub).join(format!("{appid}.jpg"))); }
                crate::net::log(&format!("Xbox sync: removed {name:?} (appid {appid}, {minutes} min played): Nexus does not report it"));
                removed += 1;
            }
            let _ = std::fs::remove_file(pending_path(dir));
        } else {
            pending = gone.len();
            let list: Vec<Value> = gone.iter().map(|(_, n, m)| serde_json::json!({"name": n, "minutes": m})).collect();
            let _ = std::fs::write(pending_path(dir), serde_json::to_string(&list).unwrap_or_default());
        }
    }
    tx.commit()?;
    Ok(Outcome { added, marked, removed, pending })
}

/// Asks Nexus what the account holds now and looks each product up: (product id -> `owned` or `gamepass`, product id -> catalog facts).
pub fn fetch(home: &Path, http: &dyn Http, now: i64) -> Result<(HashMap<String, &'static str>, HashMap<String, CatInfo>), String> {
    let binary = backend_binary(home).ok_or("Nexus Launcher (Xodus Nexus) was not found. Install it from https://github.com/domsvoboda/nexus-launcher, open it once, sign in and let it finish setting up, then try again.")?;
    let market = market(home);
    let (ok, out) = run_backend(&binary, &market)?;
    let kinds = classify(&parse_library(&out, ok, &market)?, now);
    crate::net::set_activity(&format!("Found {} owned or Game Pass products. Looking them up...", kinds.len()));
    let mut ids: Vec<String> = kinds.keys().cloned().collect(); ids.sort();
    let catalog = lookup_catalog(http, &ids, &market);
    Ok((kinds, catalog))
}

/// The whole job: ask Nexus, classify, look up, apply. Returns the line to show.
/// Returns the message for the person, what was done, and how many owned or Game Pass games Nexus listed.
pub fn import(db: &Path, home: &Path, http: &dyn Http, now: i64) -> Result<(String, Outcome, usize), String> {
    let (mode, approved) = take_request();
    let (kinds, catalog) = fetch(home, http, now)?;
    let mut conn = rusqlite::Connection::open(db).map_err(|e| e.to_string())?;
    conn.busy_timeout(Duration::from_secs(10)).ok();
    let dir = db.parent().unwrap_or(Path::new("."));
    let o = apply_mode(&mut conn, &kinds, &catalog, now, mode, (mode == Mode::Replace).then_some((dir, approved))).map_err(|e| e.to_string())?;
    let msg = if o.pending > 0 { format!("{} Xbox games are not on Nexus's list. Nothing was deleted yet: look at the list in the Xbox card and approve it.", o.pending) }
       else { message(kinds.len(), &o) };
    Ok((msg, o, kinds.len()))
}

/// What a finished Nexus sync says: the games Nexus listed, then only what changed ("nothing new" when nothing did).
pub fn message(listed: usize, o: &Outcome) -> String {
    let mut parts = vec![];
    if o.added > 0 { parts.push(format!("{} added", o.added)); }
    if o.removed > 0 { parts.push(format!("{} removed", o.removed)); }
    format!("Nexus sync: {listed} owned or Game Pass game{}, {}", if listed == 1 { "" } else { "s" }, if parts.is_empty() { "nothing new".to_string() } else { parts.join(", ") })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn golden() -> Value { serde_json::from_str(include_str!("../tests/golden/xbox_owned.json")).unwrap() }
    #[test] fn the_finished_message_says_what_nexus_listed_and_only_what_changed() {
        let o = |added, removed| Outcome { added, marked: 9, removed, pending: 0 };
        assert_eq!(message(9, &o(0, 0)), "Nexus sync: 9 owned or Game Pass games, nothing new");
        assert_eq!(message(1, &o(1, 0)), "Nexus sync: 1 owned or Game Pass game, 1 added");
        assert_eq!(message(9, &o(2, 26)), "Nexus sync: 9 owned or Game Pass games, 2 added, 26 removed");
    }
    #[test] fn classification_matches_the_python_for_trials_expiry_sharing_plans_chains_and_loops() {
        let g = golden();
        let entries: HashMap<String, Value> = g["entries"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        let now = super::super::sources::xbox::parse_iso_ts("2026-11-07T12:00:00").unwrap();
        let got = classify(&entries, now);
        let want: HashMap<String, String> = g["kinds"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string())).collect();
        assert_eq!(got.iter().map(|(k, v)| (k.clone(), v.to_string())).collect::<HashMap<_, _>>(), want);
        assert_eq!(want.len(), 5);
    }
    #[test] fn the_backend_answer_is_checked_the_way_the_python_checks_it() {
        let g = golden();
        for (name, c) in g["fetch"].as_object().unwrap() {
            let (want, stdout, rc, market) = (&c[0], c[1].as_str().unwrap(), c[2].as_i64().unwrap(), c[3].as_str().unwrap());
            let got = parse_library(stdout, rc == 0, market);
            match (want.get("ok"), want.get("err")) {
                (Some(ids), _) => { let mut k: Vec<String> = got.unwrap_or_else(|e| panic!("{name}: {e}")).into_keys().collect(); k.sort(); assert_eq!(serde_json::json!(k), *ids, "{name}"); }
                (_, Some(e)) => assert_eq!(got.unwrap_err(), e.as_str().unwrap(), "{name}"),
                _ => unreachable!(),
            }
        }
    }
    #[test] fn catalog_products_give_title_publisher_and_a_package_from_the_product_or_its_skus() {
        let g = golden();
        let got = parse_catalog(&serde_json::json!({"Products": g["products"]}));
        for (id, w) in g["catalog"].as_object().unwrap() {
            let i = got.get(id).unwrap_or_else(|| panic!("{id}"));
            let s = |k: &str| w[k].as_str().map(String::from);
            assert_eq!((i.kind.clone(), i.title.clone(), i.publisher.clone(), i.developer.clone(), i.description.clone(), i.pfn.clone()), (s("type"), s("title"), s("publisher"), s("developer"), s("description"), s("pfn")), "{id}");
        }
    }
    #[test] fn group_lists_drop_the_two_import_groups_and_add_without_repeats() {
        for r in golden()["groups"].as_array().unwrap() { let add: Vec<&str> = r[1].as_array().unwrap().iter().map(|x| x.as_str().unwrap()).collect(); assert_eq!(with_groups(r[0].as_str().unwrap(), &add), r[2].as_str().unwrap()); }
    }
    #[test] fn applying_adds_only_real_games_missing_from_the_library_tags_owned_ones_and_strips_the_groups_from_the_rest() {
        let mut c = rusqlite::Connection::open_in_memory().unwrap();
        crate::schema::ensure(&c).ok();
        c.execute_batch("CREATE TABLE IF NOT EXISTS games (appid INTEGER PRIMARY KEY, name TEXT, platform TEXT, platform_id TEXT, platform_slug TEXT, platform_appname TEXT, groups TEXT, date_added INTEGER, developers TEXT, publishers TEXT, short_description TEXT, completion_status TEXT, installed INTEGER);
            CREATE TABLE IF NOT EXISTS blacklist (appid INTEGER, platform_id TEXT, platform TEXT);
            INSERT INTO games (appid, name, platform, platform_id, platform_slug, groups) VALUES (-1,'Have','xbox','111','AAAAAAAAAAAA','Mine'),(-2,'Lost','xbox','222','ZZZZZZZZZZZZ','Mine,Game Pass'),(-3,'Rom','xbox','rom',NULL,'');").ok();
        let kinds: HashMap<String, &'static str> = [("AAAAAAAAAAAA".to_string(), "owned"), ("BBBBBBBBBBBB".to_string(), "gamepass"), ("CCCCCCCCCCCC".to_string(), "owned"), ("DDDDDDDDDDDD".to_string(), "owned")].into();
        let cat = |t: &str, ty: &str, pfn: Option<&str>| CatInfo { kind: Some(ty.into()), title: Some(t.into()), publisher: Some("Pub".into()), developer: None, description: Some("d".into()), pfn: pfn.map(String::from) };
        let catalog: HashMap<String, CatInfo> = [("BBBBBBBBBBBB".to_string(), cat("Game B", "Game", Some("P.B"))), ("CCCCCCCCCCCC".to_string(), cat("A DLC", "Durable", Some("P.C")))].into();
        let (added, marked) = apply(&mut c, &kinds, &catalog, 1000).unwrap();
        assert_eq!(added, 1);   // B only: C is not a game, D is unknown to the catalog
        let groups = |slug: &str| c.query_row("SELECT groups FROM games WHERE platform_slug = ?1", [slug], |r| r.get::<_, String>(0)).unwrap();
        assert_eq!(groups("AAAAAAAAAAAA"), "Mine,Owned (Microsoft Store)"); assert_eq!(groups("BBBBBBBBBBBB"), "Game Pass"); assert_eq!(groups("ZZZZZZZZZZZZ"), "Mine,Played, not owned");   // held no longer (or never): kept, and marked so it can be found
        assert_eq!(marked, 2);
        let rom: Option<String> = c.query_row("SELECT groups FROM games WHERE appid = -3", [], |r| r.get(0)).unwrap(); assert_eq!(rom.as_deref(), Some(""));
        let b: (String, String, String) = c.query_row("SELECT platform_id, platform_appname, name FROM games WHERE platform_slug = 'BBBBBBBBBBBB'", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!(b, ("store:BBBBBBBBBBBB".into(), "P.B".into(), "Game B".into()));
        // a second run changes nothing
        assert_eq!(apply(&mut c, &kinds, &catalog, 2000).unwrap(), (0, 2));
    }
}

#[cfg(test)]
mod mode_tests {
    use super::*;
    fn db() -> rusqlite::Connection {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE games (appid INTEGER PRIMARY KEY, name TEXT, platform TEXT, platform_id TEXT, platform_slug TEXT, groups TEXT, playtime_forever INTEGER, duplicate_of TEXT, duplicate_auto INTEGER);
            INSERT INTO games VALUES (-1,'Owned One','xbox','1','AAAAAAAAAAAA','',0,NULL,0), (-2,'Played Only','xbox','2','BBBBBBBBBBBB','',600,NULL,0), (-3,'Emulated','xbox','/roms/x.iso','', '',0,NULL,0), (-4,'Elsewhere','gog','9','BBBBBBBBBBBB','',5,NULL,0);").unwrap();
        c
    }
    fn kinds() -> HashMap<String, &'static str> { HashMap::from([("AAAAAAAAAAAA".to_string(), "owned")]) }
    fn dir(tag: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("zest-xbm-{tag}-{}", std::process::id())); let _ = std::fs::create_dir_all(&d); d }
    fn count(c: &rusqlite::Connection) -> i64 { c.query_row("SELECT COUNT(*) FROM games", [], |r| r.get(0)).unwrap() }
    #[test] fn replace_without_approval_deletes_nothing_and_lists_the_games() {
        let (mut c, d) = (db(), dir("pend"));
        let o = apply_mode(&mut c, &kinds(), &HashMap::new(), 1, Mode::Replace, Some((&d, false))).unwrap();
        assert_eq!((o.removed, o.pending, count(&c)), (0, 1, 4));
        assert_eq!(read_pending(&d), vec![("Played Only".to_string(), 600)]);
    }
    #[test] fn approved_replace_deletes_only_the_unreported_account_games() {
        let (mut c, d) = (db(), dir("ok"));
        let o = apply_mode(&mut c, &kinds(), &HashMap::new(), 1, Mode::Replace, Some((&d, true))).unwrap();
        assert_eq!((o.removed, count(&c)), (1, 3), "the emulated xbox game and the other store's game stay");
        let left: Vec<String> = c.prepare("SELECT name FROM games ORDER BY appid DESC").unwrap().query_map([], |r| r.get(0)).unwrap().flatten().collect();
        assert!(!left.contains(&"Played Only".to_string()) && left.contains(&"Emulated".to_string()) && left.contains(&"Elsewhere".to_string()));
        assert!(read_pending(&d).is_empty());
    }
    #[test] fn an_empty_nexus_answer_never_deletes() {
        let (mut c, d) = (db(), dir("empty"));
        let o = apply_mode(&mut c, &HashMap::new(), &HashMap::new(), 1, Mode::Replace, Some((&d, true))).unwrap();
        assert_eq!((o.removed, o.pending, count(&c)), (0, 0, 4));
    }
    #[test] fn group_and_add_modes_never_delete() {
        for mode in [Mode::Group, Mode::Add] { let mut c = db(); let o = apply_mode(&mut c, &kinds(), &HashMap::new(), 1, mode, None).unwrap(); assert_eq!((o.removed, count(&c)), (0, 4)); }
    }
}
