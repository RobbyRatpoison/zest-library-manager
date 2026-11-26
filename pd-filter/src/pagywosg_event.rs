//! PAGYWOSG's "auto" step: one event (categories plus the mod-verified entries) and the library become the pools, tags, conditions and appid
//! lists the builder and the quals panel work from. A port of the `/api/pagywosg-auto` route in `pagywosg.py` (golden: `gen_pagywosg_auto_golden.py`).
//! The event itself is fetched live from pagywosg.xyz ([`fetch_event`]); the supplement and personal-default files are read by [`Supplements::load`].
use crate::net::Http;
use std::time::Duration;
use crate::pagywosg::{classify, event_id, redundant_where, AppidSource, Class, Param, Val};
use regex::Regex;
use rusqlite::{types::Value as Sql, Connection};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap, HashSet};

/// Insertion-ordered map keyed by appid (Python dicts keep first-insertion order, and several outputs depend on it).
#[derive(Debug, Clone, Default)]
pub struct OMap<V> { items: Vec<(i64, V)>, index: HashMap<i64, usize> }
impl<V> OMap<V> {
    pub fn new() -> Self { OMap { items: Vec::new(), index: HashMap::new() } }
    pub fn get(&self, k: i64) -> Option<&V> { self.index.get(&k).map(|&i| &self.items[i].1) }
    pub fn get_mut(&mut self, k: i64) -> Option<&mut V> { match self.index.get(&k) { Some(&i) => Some(&mut self.items[i].1), None => None } }
    /// Replaces the value but keeps the original position, like assigning to an existing dict key.
    pub fn set(&mut self, k: i64, v: V) { match self.index.get(&k) { Some(&i) => self.items[i].1 = v, None => { self.index.insert(k, self.items.len()); self.items.push((k, v)); } } }
    pub fn contains(&self, k: i64) -> bool { self.index.contains_key(&k) }
    pub fn iter(&self) -> impl Iterator<Item = &(i64, V)> { self.items.iter() }
    pub fn len(&self) -> usize { self.items.len() }
    pub fn is_empty(&self) -> bool { self.items.is_empty() }
}
pub type Names = OMap<String>;

/// One category of an event.
#[derive(Debug, Clone)]
pub struct Category { pub id: i64, pub name: String }
/// One mod-verified entry: the game, the category it was verified for, and the SteamGifts user it was verified for.
#[derive(Debug, Clone)]
pub struct Entry { pub category: i64, pub appid: i64, pub game: String, pub verifier: Option<String> }
#[derive(Debug, Clone)]
pub struct Event { pub id: i64, pub name: String, pub started: String, pub ended: String, pub categories: Vec<Category>, pub entries: Vec<Entry> }

impl Event {
    /// Reads the `event` object of `https://pagywosg.xyz/api/events/N` (only verified entries are kept).
    pub fn from_json(v: &Value) -> Event {
        let s = |x: &Value| x.as_str().unwrap_or("").to_string();
        Event {
            id: v["id"].as_i64().unwrap_or(0), name: s(&v["name"]), started: s(&v["startedAt"]), ended: s(&v["endedAt"]),
            categories: v["gameCategories"].as_array().map(|a| a.iter().map(|c| Category { id: c["id"].as_i64().unwrap_or(0), name: s(&c["name"]) }).collect()).unwrap_or_default(),
            entries: v["entries"].as_array().map(|a| a.iter().filter(|e| e["verified"].as_bool().unwrap_or(false)).map(|e| {
                let p = &e["player"];
                let verifier = ["sgProfileName", "profileName"].iter().filter_map(|k| p[k].as_str()).find(|x| !x.is_empty()).map(String::from);
                Entry { category: e["category"]["id"].as_i64().unwrap_or(0), appid: e["game"]["id"].as_i64().unwrap_or(0), game: s(&e["game"]["name"]), verifier }
            }).collect()).unwrap_or_default(),
        }
    }
}

/// Fetches an event live (no cache, as decided). The id for the current month comes from [`event_id`].
pub fn fetch_event(http: &dyn Http, id: i64) -> Result<Event, String> {
    let r = http.get(&format!("https://pagywosg.xyz/api/events/{id}"), &[("User-Agent", "Zest/1.0")], Duration::from_secs(10)).map_err(|e| format!("pagywosg.xyz: {e}"))?;
    if r.status != 200 { return Err(format!("pagywosg.xyz answered {} for event {id}", r.status)); }
    let v: Value = serde_json::from_str(&r.body).map_err(|e| format!("pagywosg.xyz sent something unreadable: {e}"))?;
    let mut ev = Event::from_json(&v["event"]);
    if ev.id == 0 { ev.id = id; }
    Ok(ev)
}

/// The event to work from: this month's, or on a boundary day the neighbour whose dates contain `today` (`YYYY-MM-DD`); `next` asks for the upcoming one.
pub fn current_event(http: &dyn Http, year: i32, month: i32, today: &str, next: bool) -> Result<Event, String> {
    let mut id = event_id(year, month) as i64 + if next { 1 } else { 0 };
    let mut ev = fetch_event(http, id)?;
    if !next && !ev.started.is_empty() && !ev.ended.is_empty() && !(ev.started.as_str() <= today && today < ev.ended.as_str()) {
        let adj = if today >= ev.ended.as_str() { id + 1 } else { id - 1 };
        if let Ok(a) = fetch_event(http, adj) { if a.started.as_str() <= today && today < a.ended.as_str() { ev = a; id = adj; } }
    }
    ev.id = id;
    Ok(ev)
}

/// Appid lists from the bundled/local supplement files, plus the maintainer's "always personal" category names per event.
#[derive(Debug, Clone, Default)]
pub struct Supplements { pub icaio_giveaways: Names, pub icaio_wishlist: Names, pub santa: Names }

impl Supplements {
    /// `pagywosg_supplement.json`: `icaio_giveaways: [{appid, name}]`, `icaio_wishlist: {appid: name}`.
    pub fn add_supplement(&mut self, v: &Value) {
        for g in v["icaio_giveaways"].as_array().into_iter().flatten() { if let (Some(a), Some(n)) = (g["appid"].as_i64(), g["name"].as_str()) { self.icaio_giveaways.set(a, n.into()); } }
        for (k, n) in v["icaio_wishlist"].as_object().into_iter().flatten() { if let (Ok(a), Some(n)) = (k.parse::<i64>(), n.as_str()) { self.icaio_wishlist.set(a, n.into()); } }
    }
    /// `santa_gifts.json`: `[{appid, name, year?}]`.
    pub fn add_santa(&mut self, v: &Value) {
        for g in v.as_array().into_iter().flatten() { if let (Some(a), Some(n)) = (g["appid"].as_i64(), g["name"].as_str()) { self.santa.set(a, n.into()); } }
    }
    /// The files in `dir`, with `bundled_supplement` (the copy that ships with the app) used when the user has none.
    pub fn load(dir: &std::path::Path, bundled_supplement: &str) -> Supplements {
        let read = |f: &str| std::fs::read_to_string(dir.join(f)).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok());
        let mut s = Supplements::default();
        if let Some(v) = read("pagywosg_supplement.json").or_else(|| serde_json::from_str(bundled_supplement).ok()) { s.add_supplement(&v); }
        if let Some(v) = read("santa_gifts.json") { s.add_santa(&v); }
        s
    }
}

/// A Secret Santa / Snowball gift: counted as a win by PAGYWOSG. The year is the evidence a moderator looks for.
#[derive(Debug, Clone, PartialEq)]
pub struct Gift { pub appid: i64, pub name: String, pub year: Option<i64> }

pub fn load_gifts(dir: &std::path::Path) -> Vec<Gift> {
    std::fs::read_to_string(dir.join("santa_gifts.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v.as_array().map(|a| a.iter().filter_map(|g|
        Some(Gift { appid: g["appid"].as_i64()?, name: g["name"].as_str()?.to_string(), year: g["year"].as_i64() })).collect())).unwrap_or_default()
}

pub fn save_gifts(dir: &std::path::Path, gifts: &[Gift]) -> std::io::Result<()> {
    let v = Value::Array(gifts.iter().map(|g| { let mut o = json!({"appid": g.appid, "name": g.name}); if let Some(y) = g.year { o["year"] = json!(y); } o }).collect());
    crate::account::write_json(&dir.join("santa_gifts.json"), &v)
}

/// The categories the maintainer marked "always personal" for an event: the user's own file in `dir`, else the copy that ships with the app.
pub fn load_personal_defaults(dir: &std::path::Path, bundled: &str, event: i64) -> Vec<String> {
    std::fs::read_to_string(dir.join("pagywosg_personal_defaults.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).or_else(|| serde_json::from_str(bundled).ok())
        .and_then(|v| v[event.to_string()].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())).unwrap_or_default()
}

/// One category a game counts for. `verifiers`: the SteamGifts users it was verified for (only on mod-verified lists).
#[derive(Debug, Clone, PartialEq)]
pub struct CatEntry { pub cat: String, pub auto: bool, pub verifiers: Option<Vec<String>> }
#[derive(Debug, Clone)]
pub struct PoolGame { pub name: String, pub categories: Vec<CatEntry> }

/// A pool's appids for one category label, only the owned ones.
#[derive(Debug, Clone, PartialEq)]
pub struct AppidSource_ { pub label: String, pub appids: Vec<i64>, pub auto: bool, pub personal_appids: Vec<i64> }

#[derive(Debug, Clone, Default)]
pub struct Pool { pub appids: Vec<i64>, pub appid_sources: Vec<AppidSource_>, pub games: Vec<GameRow> }
#[derive(Debug, Clone)]
pub struct GameRow { pub appid: i64, pub name: String, pub categories: Vec<CatEntry>, pub in_library: bool, pub redundant: bool }

pub type Cond = (String, String, String);

#[derive(Debug, Clone, Default)]
pub struct Auto {
    pub event_id: i64, pub event_name: String,
    pub tags_wins: Vec<String>, pub tags_all: Vec<String>, pub conds_wins: Vec<Cond>, pub conds_all: Vec<Cond>,
    /// Category base names in the order the classifier met them (the order a game's categories are listed in).
    pub cat_order: Vec<String>,
    pub wins: Pool, pub all: Pool, pub skipped: Vec<String>, pub personal_candidates: Vec<String>, pub default_personal: Vec<String>,
}

fn cat_json(c: &CatEntry) -> Value {
    let mut o = json!({"cat": c.cat});
    if c.auto { o["auto"] = json!(true); }
    if let Some(v) = &c.verifiers { o["verifiers"] = json!(v); }
    o
}
fn conds_json(c: &[Cond]) -> Value { Value::Array(c.iter().map(|(col, op, val)| json!({"col": col, "op": op, "val": val})).collect()) }
fn pool_json(p: &Pool) -> Value {
    json!({
        "appids": p.appids,
        "appid_sources": p.appid_sources.iter().map(|s| json!({"label": s.label, "appids": s.appids, "auto": s.auto, "personal_appids": s.personal_appids})).collect::<Vec<_>>(),
        "games": p.games.iter().map(|g| json!({"appid": g.appid, "name": g.name, "categories": g.categories.iter().map(cat_json).collect::<Vec<_>>(), "in_library": g.in_library, "redundant": g.redundant})).collect::<Vec<_>>(),
    })
}
impl Auto {
    /// The same shape the Python route returned (used by the golden test and anything that wants the data as JSON).
    pub fn to_json(&self) -> Value {
        json!({"status": "success", "event": {"id": self.event_id, "name": self.event_name},
               "tags": {"wins": self.tags_wins, "all": self.tags_all}, "conds": {"wins": conds_json(&self.conds_wins), "all": conds_json(&self.conds_all)},
               "wins": pool_json(&self.wins), "all": pool_json(&self.all), "skipped": self.skipped, "personal_candidates": self.personal_candidates, "default_personal": self.default_personal})
    }
}

fn to_sql(p: &[Param]) -> Vec<Sql> { p.iter().map(|x| match x { Param::Text(s) => Sql::Text(s.clone()), Param::Int(i) => Sql::Integer(*i) }).collect() }

/// Library games that the pool's tag/condition criteria already match without the explicit appid list, restricted to `owned`.
fn redundant_set(db: &Connection, owned: &HashSet<i64>, conds: &[Cond], tags: &[String]) -> HashSet<i64> {
    if owned.is_empty() { return HashSet::new(); }
    let Some((where_, params)) = redundant_where(conds, tags) else { return HashSet::new() };
    let Ok(mut st) = db.prepare(&format!("SELECT appid FROM games WHERE ({where_})")) else { return HashSet::new() };
    let Ok(rows) = st.query_map(rusqlite::params_from_iter(to_sql(&params)), |r| r.get::<_, i64>(0)) else { return HashSet::new() };
    rows.flatten().filter(|a| owned.contains(a)).collect()
}

fn serialise_pool(db: &Connection, pool: &OMap<PoolGame>, tags: &[String], conds: &[Cond], in_library: &HashSet<i64>, sg_username: &str) -> Pool {
    let owned: HashSet<i64> = pool.iter().map(|(a, _)| *a).filter(|a| in_library.contains(a)).collect();
    let redundant = redundant_set(db, &owned, conds, tags);
    let sg = sg_username.to_lowercase();
    let mut sources: Vec<AppidSource_> = Vec::new();
    for (aid, g) in pool.iter() {
        if !in_library.contains(aid) { continue; }
        for ce in &g.categories {
            if ce.cat.is_empty() { continue; }
            let i = match sources.iter().position(|s| s.label == ce.cat) { Some(i) => i, None => { sources.push(AppidSource_ { label: ce.cat.clone(), appids: vec![], auto: ce.auto, personal_appids: vec![] }); sources.len() - 1 } };
            sources[i].appids.push(*aid);
            if !sg.is_empty() && ce.verifiers.iter().flatten().any(|v| v.to_lowercase() == sg) { sources[i].personal_appids.push(*aid); }
        }
    }
    for s in &mut sources { s.appids.sort(); s.personal_appids.sort(); }
    sources.retain(|s| !s.appids.is_empty());
    let mut appids: Vec<i64> = owned.iter().copied().collect();
    appids.sort();
    let mut games: Vec<GameRow> = pool.iter().map(|(a, g)| GameRow { appid: *a, name: g.name.clone(), categories: g.categories.clone(), in_library: in_library.contains(a), redundant: redundant.contains(a) }).collect();
    games.sort_by_key(|g| g.name.to_lowercase());
    Pool { appids, appid_sources: sources, games }
}

/// Builds everything the builder needs from one event. `personal_defaults`: the maintainer's always-personal categories for this event.
/// `db` is the library database (`games` table); `sg_username` the active account's SteamGifts name (decides each category's personal appids).
pub fn build_auto(event: &Event, supp: &Supplements, personal_defaults: &[String], db: &Connection, sg_username: &str) -> Result<Auto, String> {
    let suffix = Regex::new(r"(?i)\s*\((win|backlog)\)\s*$").unwrap();
    let mut base_to_cats: Vec<(String, Vec<&Category>)> = Vec::new();
    for c in &event.categories {
        let base = suffix.replace(&c.name, "").trim().to_string();
        match base_to_cats.iter_mut().find(|(b, _)| *b == base) { Some((_, v)) => v.push(c), None => base_to_cats.push((base, vec![c])) }
    }
    let all_pool: HashSet<&str> = base_to_cats.iter().filter(|(_, cats)| {
        let l: Vec<String> = cats.iter().map(|c| c.name.to_lowercase()).collect();
        l.iter().any(|n| n.contains("(win)")) && l.iter().any(|n| n.contains("(backlog)"))
    }).map(|(b, _)| b.as_str()).collect();

    // verified appids per category id, with who each was verified for
    let mut verified: HashMap<i64, OMap<(String, BTreeSet<String>)>> = HashMap::new();
    for e in &event.entries {
        let slot = verified.entry(e.category).or_default();
        if !slot.contains(e.appid) { slot.set(e.appid, (e.game.clone(), BTreeSet::new())); }
        if let Some(v) = &e.verifier { slot.get_mut(e.appid).unwrap().1.insert(v.clone()); }
    }

    let mut out = Auto { event_id: event.id, event_name: event.name.clone(), default_personal: personal_defaults.to_vec(), ..Default::default() };
    let (mut wins, mut all): (OMap<PoolGame>, OMap<PoolGame>) = (OMap::new(), OMap::new());
    let mut personal_candidates: BTreeSet<String> = BTreeSet::new();

    out.cat_order = base_to_cats.iter().map(|(b, _)| b.clone()).collect();
    for (base, cats) in &base_to_cats {
        let is_all = all_pool.contains(base.as_str());
        let mut base_appids: Names = OMap::new();
        let mut base_verifiers: OMap<BTreeSet<String>> = OMap::new();
        for c in cats {
            if let Some(m) = verified.get(&c.id) {
                for (aid, (name, vs)) in m.iter() {
                    base_appids.set(*aid, name.clone());
                    if !base_verifiers.contains(*aid) { base_verifiers.set(*aid, BTreeSet::new()); }
                    base_verifiers.get_mut(*aid).unwrap().extend(vs.iter().cloned());
                }
            }
        }
        let results = classify(base, !base_appids.is_empty(), !supp.icaio_giveaways.is_empty(), !supp.icaio_wishlist.is_empty(), !supp.santa.is_empty());
        for r in results {
            match r {
                Class::Tag { tag, .. } => { if is_all { out.tags_all.push(tag) } else { out.tags_wins.push(tag) } }
                Class::Cond { col, op, val, .. } => {
                    let target = if is_all { &mut out.conds_all } else { &mut out.conds_wins };
                    match (op, val) {
                        ("starts_with_any", Val::Many(letters)) => for l in letters { target.push((col.to_string(), "starts_with".into(), l)); },
                        (_, Val::One(v)) => target.push((col.to_string(), op.to_string(), v)),
                        (_, Val::Many(vs)) => for v in vs { target.push((col.to_string(), op.to_string(), v)); },
                    }
                }
                Class::Appids { source, auto, force_wins, .. } => {
                    let list = match source { AppidSource::IcaioGiveaways => &supp.icaio_giveaways, AppidSource::IcaioWishlist => &supp.icaio_wishlist, AppidSource::Santa => &supp.santa, AppidSource::Verified => &base_appids };
                    let target = if force_wins || !is_all { &mut wins } else { &mut all };
                    for (aid, name) in list.iter() {
                        if !target.contains(*aid) { target.set(*aid, PoolGame { name: name.clone(), categories: vec![] }); }
                        let g = target.get_mut(*aid).unwrap();
                        if !g.categories.iter().any(|c| c.cat == *base) {
                            // only a genuinely mod-verified list has verifier attribution (the supplements come from bundled files)
                            let verifiers = if auto || base_verifiers.is_empty() { None } else { base_verifiers.get(*aid).map(|s| s.iter().cloned().collect()) };
                            g.categories.push(CatEntry { cat: base.clone(), auto, verifiers });
                        }
                    }
                }
                Class::Skip { reason } => { out.skipped.push(base.clone()); if reason == "unhandled" { personal_candidates.insert(base.clone()); } }
            }
        }
    }

    let mut lib: HashSet<i64> = HashSet::new();
    { let mut st = db.prepare("SELECT appid FROM games").map_err(|e| e.to_string())?; for a in st.query_map([], |r| r.get::<_, i64>(0)).map_err(|e| e.to_string())?.flatten() { lib.insert(a); } }
    out.wins = serialise_pool(db, &wins, &out.tags_wins, &out.conds_wins, &lib, sg_username);
    out.all = serialise_pool(db, &all, &out.tags_all, &out.conds_all, &lib, sg_username);
    out.personal_candidates = personal_candidates.into_iter().collect();
    Ok(out)
}
