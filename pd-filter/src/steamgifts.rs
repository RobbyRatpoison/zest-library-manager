//! SteamGifts won-giveaway import: parsing the won pages, the per-account wins file, and writing the "Won on SteamGifts" group. A port of PlayDate's
//! `steamgifts.py` (golden: `gen_steamgifts_golden.py`). Cloudflare blocks fetching steamgifts.com from outside a browser, so the pages are read in the user's
//! own logged-in browser (the Companion userscript) and their HTML handed to [`parse_won_public`] / [`parse_won_private`].
//!
//! Two passes: the **public** page `/user/<name>/giveaways/won` (25 a page, every field) and the **private** `/giveaways/won` (50 a page, the authoritative
//! "received" state; opt-in). The private page embeds live Steam keys: parsing never selects the key node and [`scrub`] drops any value that looks like one.
use crate::blaeo::{split_groups, GroupSources};
use crate::metadata::similar;
use crate::net::Http;
use regex::Regex;
use rusqlite::Connection;
use scraper::{ElementRef, Html, Selector};
use serde_json::{json, Map, Value};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::LazyLock;
use std::time::Duration;

pub const GROUP_NAME: &str = "Won on SteamGifts";
pub const SOURCE_ID: &str = "steamgifts:won";
pub const PUBLIC_PAGE_SIZE: i64 = 25;
pub const PRIVATE_PAGE_SIZE: i64 = 50;

macro_rules! rx { ($p:expr) => {{ static R: LazyLock<Regex> = LazyLock::new(|| Regex::new($p).unwrap()); &*R }}; }
macro_rules! sel { ($p:expr) => {{ static S: LazyLock<Selector> = LazyLock::new(|| Selector::parse($p).unwrap()); &*S }}; }

/// BeautifulSoup's `get_text(strip=True)`: every text piece trimmed, empty ones dropped, the rest joined with nothing between.
fn text(el: ElementRef) -> String { el.text().map(str::trim).filter(|t| !t.is_empty()).collect() }

/// Never let a Steam key survive into a stored record.
pub fn scrub(v: String) -> Option<String> { if rx!(r"[A-Za-z0-9]{5}-[A-Za-z0-9]{5}-[A-Za-z0-9]{5}").is_match(&v) { None } else { Some(v) } }

fn code_from_href(href: &str) -> Option<String> { rx!(r"/giveaway/([0-9A-Za-z]{5})(?:/|$)").captures(href).map(|m| m[1].to_string()) }
fn timestamp(el: Option<ElementRef>) -> Option<i64> { el?.value().attr("data-timestamp")?.trim().parse().ok() }
pub fn ceil_div(a: i64, b: i64) -> i64 { (a + b - 1) / b }
fn has_next(doc: &Html) -> bool { doc.select(sel!(".pagination__navigation a")).any(|a| text(a).to_lowercase().contains("next")) }
fn classes(el: ElementRef<'_>) -> Vec<&str> { el.value().classes().collect() }

#[derive(Debug, Clone, PartialEq)]
pub struct PublicPage { pub wins: Vec<Value>, pub total: Option<i64>, pub pages: Option<i64>, pub has_next: bool, pub logged_in_as: Option<String> }

/// A public `/user/<name>/giveaways/won` page. Win records: `{code, name, steam_ref, won_ts, gifter, points, received}` with `received` true / false / null (awaiting).
pub fn parse_won_public(html: &str) -> PublicPage {
    let doc = Html::parse_document(html);
    let mut wins = Vec::new();
    for row in doc.select(sel!("div.giveaway__row-outer-wrap")) {
        let Some(heading) = row.select(sel!("a.giveaway__heading__name")).next() else { continue };
        let name = scrub(text(heading));
        let code = heading.value().attr("href").and_then(code_from_href);
        let steam_ref = row.select(sel!("a.giveaway__icon")).filter_map(|a| a.value().attr("href")).find(|h| h.contains("store.steampowered.com"))
            .and_then(|h| rx!(r"/(app|sub)/(\d+)").captures(h).map(|m| format!("{}/{}", &m[1], &m[2])));
        let points = row.select(sel!("span.giveaway__heading__thin")).next().and_then(|t| rx!(r"(\d+)").captures(&t.text().collect::<String>()).and_then(|m| m[1].parse::<i64>().ok()));
        let Some(cols) = row.select(sel!("div.giveaway__columns")).next() else { continue };
        let cells: Vec<ElementRef> = cols.children().filter_map(ElementRef::wrap).filter(|e| e.value().name() == "div").collect();
        let won_ts = cells.first().and_then(|c| timestamp(c.select(sel!("span[data-timestamp]")).next()));
        // the cell after "Ended": a positive or negative class, or a plain div reading "Awaiting feedback"
        let received = cells.get(1).and_then(|fb| { let c = classes(*fb); if c.contains(&"giveaway__column--positive") { Some(true) } else if c.contains(&"giveaway__column--negative") { Some(false) } else { None } });
        let gifter = cols.select(sel!("div.giveaway__column--width-fill a.giveaway__username")).next().and_then(|g| scrub(text(g)));
        wins.push(json!({"code": code, "name": name, "steam_ref": steam_ref, "won_ts": won_ts, "gifter": gifter, "points": points, "received": received}));
    }
    let mut total = doc.select(sel!(".pagination__results")).next().and_then(|r| rx!(r"of\s*(?:<strong>)?\s*([\d,]+)").captures(&r.html()).and_then(|m| m[1].replace(',', "").parse::<i64>().ok()));
    if total.is_none() {
        // real pages only say "Displaying 1 to 25": fall back to the "Gifts Won" count, a link to the won page whose text is the number
        total = doc.select(sel!("a[href*=\"/giveaways/won\"]")).map(|a| text(a).replace(',', "")).find(|t| !t.is_empty() && t.chars().all(|c| c.is_ascii_digit())).and_then(|t| t.parse().ok());
    }
    let pages = total.filter(|t| *t != 0).map(|t| ceil_div(t, PUBLIC_PAGE_SIZE));
    let logged_in_as = doc.select(sel!("a.nav__avatar-outer-wrap")).filter_map(|a| a.value().attr("href")).find(|h| h.starts_with("/user/"))
        .and_then(|h| h.split_once("/user/").map(|(_, r)| r.trim_matches('/').to_string())).filter(|s| !s.is_empty());
    PublicPage { wins, total, pages, has_next: has_next(&doc), logged_in_as }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PrivatePage { /// `{code, ended_ts, received}`; `received` is true only for the green check.
    pub rows: Vec<Value>, pub has_next: bool }

/// A private `/giveaways/won` page, for the received state only (the key column is never read).
pub fn parse_won_private(html: &str) -> PrivatePage {
    let doc = Html::parse_document(html);
    let mut rows = Vec::new();
    for row in doc.select(sel!("div.table__row-outer-wrap")) {
        let Some(head) = row.select(sel!("a.table__column__heading")).next() else { continue };
        let code = head.value().attr("href").and_then(code_from_href);
        let ended = timestamp(row.select(sel!(".table__column--width-fill span[data-timestamp]")).next());
        let received = row.select(sel!(".table__column--width-xsmall i")).next().map_or(false, |i| { let c = classes(i); c.contains(&"fa-check-circle") && c.contains(&"icon-green") });
        rows.push(json!({"code": code, "ended_ts": ended, "received": received}));
    }
    PrivatePage { rows, has_next: has_next(&doc) }
}

// ───────────────────────── the wins file ─────────────────────────

pub fn empty_store() -> Value { json!({"version": 1, "username": "", "wins": [], "last_sync_public": null, "last_sync_private": null}) }

/// `steamgifts_wins_<steam id>.json` (the file PlayDate uses).
pub fn load_store(path: &std::path::Path) -> Value {
    let mut v: Value = std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_else(empty_store);
    if let Some(o) = v.as_object_mut() { o.entry("version").or_insert(json!(1)); o.entry("wins").or_insert(json!([])); }
    v
}
pub fn save_store(path: &std::path::Path, v: &Value) -> std::io::Result<()> { crate::account::write_json(path, v) }

fn wins_mut(store: &mut Value) -> &mut Vec<Value> {
    if !store["wins"].is_array() { store["wins"] = json!([]); }
    store["wins"].as_array_mut().unwrap()
}

/// Dedup identity: the giveaway code, or (steam ref, won time) when the code was stripped (an invite-only giveaway seen by a non-winner).
fn win_key(w: &Value) -> String {
    match w["code"].as_str().filter(|c| !c.is_empty()) { Some(c) => format!("code:{c}"), None => format!("anon:{}:{}", w["steam_ref"], w["won_ts"]) }
}

/// Merges one public page into the store: (new, changed). On a full refresh every record the page re-confirms is tagged `_seen_this_run`, which [`apply_wins`] uses
/// to drop what is no longer on the account's won list at all (an incremental sync never sees every page, so it cannot tell "gone" from "not re-scanned").
pub fn merge_public_page(store: &mut Value, parsed: &[Value], full_refresh: bool) -> (usize, usize) {
    let wins = wins_mut(store);
    let mut index: HashMap<String, usize> = wins.iter().enumerate().map(|(i, w)| (win_key(w), i)).collect();
    let (mut new, mut changed) = (0, 0);
    for rec in parsed {
        let k = win_key(rec);
        match index.get(&k) {
            None => {
                let mut r = rec.clone();
                if r.get("appid").is_none() { r["appid"] = Value::Null; }
                if full_refresh { r["_seen_this_run"] = json!(true); }
                wins.push(r);
                index.insert(k, wins.len() - 1);
                new += 1;
            }
            Some(&i) => {
                let cur = &mut wins[i];
                let before = (cur["received"].clone(), cur["gifter"].clone(), cur["points"].clone());
                for f in ["name", "steam_ref", "won_ts", "gifter", "points", "received"] { cur[f] = rec[f].clone(); }
                if full_refresh { cur["_seen_this_run"] = json!(true); }
                if (cur["received"].clone(), cur["gifter"].clone(), cur["points"].clone()) != before { changed += 1; }
            }
        }
    }
    (new, changed)
}

/// Promotes "awaiting" public wins to received from the private page's per-row state; returns how many flipped.
pub fn apply_private_rows(store: &mut Value, rows: &[Value]) -> usize {
    let mut flipped = 0;
    let wins = wins_mut(store);
    let by_code: HashMap<String, usize> = wins.iter().enumerate().filter_map(|(i, w)| w["code"].as_str().filter(|c| !c.is_empty()).map(|c| (c.to_string(), i))).collect();
    for r in rows {
        let Some(&i) = r["code"].as_str().and_then(|c| by_code.get(c)) else { continue };
        if r["received"] == true && wins[i]["received"] != true { wins[i]["received"] = json!(true); flipped += 1; }
    }
    flipped
}

/// Private page numbers that hold a win whose received state is still unknown (and the page after each, for ordering drift).
pub fn pending_pass2_pages(store: &Value) -> Vec<i64> {
    let mut ordered: Vec<&Value> = store["wins"].as_array().into_iter().flatten().filter(|w| w["code"].as_str().map_or(false, |c| !c.is_empty())).collect();
    ordered.sort_by_key(|w| std::cmp::Reverse(w["won_ts"].as_i64().unwrap_or(0)));
    let max_page = ceil_div(ordered.len() as i64, PRIVATE_PAGE_SIZE).max(1);
    let mut pages = BTreeSet::new();
    for (i, w) in ordered.iter().enumerate() {
        if w["received"] != true { let p = i as i64 / PRIVATE_PAGE_SIZE + 1; pages.insert(p); pages.insert((p + 1).min(max_page)); }
    }
    pages.into_iter().collect()
}

// ───────────────────────── Steam lookups ─────────────────────────

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Details { pub ok: bool, pub kind: Option<String>, pub name: Option<String>, pub fullgame_name: Option<String>, pub fullgame_appid: Option<i64>, pub error: bool }

/// What the apply step asks Steam, behind a trait so tests (and a run with no network) can answer from a table.
pub trait SteamLookup {
    fn package_apps(&self, subid: i64) -> Vec<i64>;
    fn appdetails(&self, appid: i64) -> Details;
}

/// The store's public API (packagedetails, appdetails), one second apart for appdetails like PlayDate (it rate-limits; only asked about wins that did not match).
pub struct WebSteam<'a> { pub http: &'a dyn Http, pub pause: Duration, pkg: std::cell::RefCell<HashMap<i64, Vec<i64>>> }
impl<'a> WebSteam<'a> { pub fn new(http: &'a dyn Http, pause: Duration) -> Self { WebSteam { http, pause, pkg: Default::default() } } }
impl SteamLookup for WebSteam<'_> {
    fn package_apps(&self, subid: i64) -> Vec<i64> {
        if let Some(a) = self.pkg.borrow().get(&subid) { return a.clone(); }
        let apps = self.http.get(&format!("https://store.steampowered.com/api/packagedetails?packageids={subid}"), &[], Duration::from_secs(10)).ok()
            .and_then(|r| serde_json::from_str::<Value>(&r.body).ok())
            .and_then(|v| { let e = v.get(subid.to_string())?; if e["success"] != true { return None; } Some(e["data"]["apps"].as_array()?.iter().filter_map(|a| a["id"].as_i64().filter(|i| *i != 0)).collect::<Vec<_>>()) })
            .unwrap_or_default();
        self.pkg.borrow_mut().insert(subid, apps.clone());
        apps
    }
    fn appdetails(&self, appid: i64) -> Details {
        let mut out = Details::default();
        match self.http.get(&format!("https://store.steampowered.com/api/appdetails?appids={appid}&l=english"), &[], Duration::from_secs(10)).ok().and_then(|r| serde_json::from_str::<Value>(&r.body).ok()) {
            None => out.error = true,
            Some(v) => {
                // Steam sometimes answers under another key (a DLC's) with data.steam_appid still the asked one
                let entry = v.get(appid.to_string()).filter(|e| !e.is_null() && e.as_object().map_or(true, |o| !o.is_empty())).cloned()
                    .or_else(|| v.as_object().into_iter().flat_map(|o| o.values()).find(|e| e["data"]["steam_appid"].as_i64() == Some(appid)).cloned()).unwrap_or(Value::Null);
                if entry["success"] == true {
                    let d = &entry["data"];
                    out.ok = true; out.kind = d["type"].as_str().map(String::from); out.name = d["name"].as_str().map(String::from);
                    let fg = &d["fullgame"];
                    out.fullgame_name = fg["name"].as_str().filter(|s| !s.is_empty()).map(String::from);
                    out.fullgame_appid = fg["appid"].as_i64().or_else(|| fg["appid"].as_str().and_then(|s| s.parse().ok()));
                }
            }
        }
        std::thread::sleep(self.pause);
        out
    }
}

fn details_json(d: &Details) -> Value {
    let mut o = json!({"ok": d.ok, "type": d.kind, "name": d.name, "fullgame_name": d.fullgame_name, "fullgame_appid": d.fullgame_appid});
    if d.error { o["error"] = json!(true); }
    o
}
fn details_from(v: &Value) -> Details { Details { ok: v["ok"] == true, kind: v["type"].as_str().map(String::from), name: v["name"].as_str().map(String::from), fullgame_name: v["fullgame_name"].as_str().map(String::from), fullgame_appid: v["fullgame_appid"].as_i64(), error: v["error"] == true } }

/// Cached lookup used only to explain why a win did not match. An entry cached before `fullgame_appid` was tracked is asked again (the cache lives in the file).
fn appdetails_lite(steam: &dyn SteamLookup, cache: &mut Map<String, Value>, appid: i64) -> Details {
    let key = appid.to_string();
    if let Some(e) = cache.get(&key).filter(|e| e.get("fullgame_appid").is_some()) { return details_from(e); }
    let d = steam.appdetails(appid);
    cache.insert(key, details_json(&d));
    d
}

fn kind_label(t: &str) -> String {
    match t { "dlc" => "DLC".into(), "music" => "Soundtrack".into(), "demo" => "Demo".into(), "application" => "Application".into(), o => { let mut c = o.chars(); c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default() } }
}

/// One short reason a win is not in the library, plus the base game's (appid, name) when the win is DLC, a soundtrack and the like for one (Steam's `fullgame`).
pub fn classify_unmatched(rec: &Value, steam: &dyn SteamLookup, cache: &mut Map<String, Value>) -> (String, Option<i64>, Option<String>) {
    let Some(r) = rec["steam_ref"].as_str().filter(|s| !s.is_empty()) else { return ("No Steam store page (gift card or non-Steam reward)".into(), None, None) };
    let (kind, sid) = r.split_once('/').unwrap_or((r, ""));
    let Ok(sid) = sid.parse::<i64>() else { return ("Unrecognised Steam link".into(), None, None) };
    if kind == "sub" {
        let apps = steam.package_apps(sid);
        if apps.is_empty() { return ("Steam package could not be read (likely delisted)".into(), None, None); }
        // a package may bundle only DLC (a season pass): look at each app's `fullgame` like a single-app DLC win
        for a in apps {
            let info = appdetails_lite(steam, cache, a);
            if !info.ok { continue; }
            let t = info.kind.clone().unwrap_or_default().to_lowercase();
            if let (false, Some(base)) = (t.is_empty() || t == "game", info.fullgame_name.clone()) { return (format!("Steam package ({} for {base})", kind_label(&t)), info.fullgame_appid, Some(base)); }
        }
        return ("Steam package \u{2014} none of its games are in your library".into(), None, None);
    }
    let info = appdetails_lite(steam, cache, sid);
    if info.error { return ("Could not check Steam (try again later)".into(), None, None); }
    if !info.ok { return ("Delisted, or removed from your Steam account".into(), None, None); }
    let t = info.kind.clone().unwrap_or_default().to_lowercase();
    if !t.is_empty() && t != "game" {
        let reason = match &info.fullgame_name { Some(b) => format!("{} for {b}", kind_label(&t)), None => format!("{}, not a base game", kind_label(&t)) };
        return (reason, info.fullgame_appid, info.fullgame_name);
    }
    ("Not in your Steam library (delisted, or removed from your account)".into(), None, None)
}

/// A stored `app/N` or `sub/N` ref as one library appid, or `None`. A package: the one owned app, else the best name match among owned apps, else the lowest.
pub fn resolve_ref(r: Option<&str>, name: Option<&str>, lib: &HashSet<i64>, steam: &dyn SteamLookup, app_name: &mut dyn FnMut(i64) -> String) -> Option<i64> {
    let (kind, sid) = r.unwrap_or("").split_once('/').unwrap_or(("", ""));
    if (kind != "app" && kind != "sub") || sid.is_empty() || !sid.chars().all(|c| c.is_ascii_digit()) { return None; }
    let sid: i64 = sid.parse().ok()?;
    if kind == "app" { return lib.contains(&sid).then_some(sid); }
    let owned: Vec<i64> = steam.package_apps(sid).into_iter().filter(|a| lib.contains(a)).collect();
    match owned.len() {
        0 => None,
        1 => Some(owned[0]),
        _ => {
            let name = name.unwrap_or("");
            let mut best = owned[0]; let mut best_s = similar(name, &app_name(best));
            for a in &owned[1..] { let s = similar(name, &app_name(*a)); if s > best_s { best = *a; best_s = s; } }
            if best_s >= 0.5 { Some(best) } else { owned.into_iter().min() }
        }
    }
}

// ───────────────────────── writing the group ─────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct Summary { pub games_in_group: usize, pub groups_added: usize, pub groups_removed: usize, pub unresolved_refs: usize, pub wins_total: usize, pub unmatched: Vec<Value> }

fn set_groups(conn: &Connection, appid: i64, groups: &BTreeSet<String>) -> rusqlite::Result<()> {
    conn.execute("UPDATE games SET `groups` = ? WHERE appid = ?", rusqlite::params![groups.iter().cloned().collect::<Vec<_>>().join(","), appid]).map(|_| ())
}
fn groups_of(conn: &Connection, appid: i64) -> Option<BTreeSet<String>> {
    conn.query_row("SELECT `groups` FROM games WHERE appid = ?", [appid], |r| r.get::<_, Option<String>>(0)).ok().map(|g| split_groups(&g.unwrap_or_default()).into_iter().collect())
}

/// `apply_wins`: resolve each win to a library appid and put "Won on SteamGifts" on every game with at least one *received* win. Additive, except on a full refresh,
/// which also removes the group from games this source added that no longer qualify (a group something else also owns stays) and drops win records the scan did not
/// re-confirm (one scraped under a mistyped username, say). A DLC win the user credited to its base game (`dlc_base_adopted`) counts for that base game and stamps `sg_dlc_win`.
pub fn apply_wins(conn: &Connection, gs: &mut GroupSources, store: &mut Value, full_refresh: bool, steam: &dyn SteamLookup) -> rusqlite::Result<Summary> {
    if full_refresh {
        wins_mut(store).retain(|w| w.get("_seen_this_run").map_or(false, |v| !v.is_null() && *v != false));
        for w in wins_mut(store) { if let Some(o) = w.as_object_mut() { o.remove("_seen_this_run"); } }
    }
    let lib: HashSet<i64> = { let mut st = conn.prepare("SELECT appid FROM games")?; let v: HashSet<i64> = st.query_map([], |r| r.get::<_, i64>(0))?.flatten().collect(); v };
    let mut names: HashMap<i64, String> = HashMap::new();
    let mut app_name = |a: i64| -> String { names.entry(a).or_insert_with(|| conn.query_row("SELECT name FROM games WHERE appid = ?", [a], |r| r.get::<_, Option<String>>(0)).ok().flatten().unwrap_or_default()).clone() };

    let n = wins_mut(store).len();
    for i in 0..n {
        let rec = wins_mut(store)[i].clone();
        if rec["appid"].as_i64().map_or(false, |a| lib.contains(&a)) { continue; }
        let a = resolve_ref(rec["steam_ref"].as_str(), rec["name"].as_str(), &lib, steam, &mut app_name);
        wins_mut(store)[i]["appid"] = a.map_or(Value::Null, Value::from);
    }
    // explain every win that did not land in the library
    if !store["appdetails_cache"].is_object() { store["appdetails_cache"] = json!({}); }
    let mut cache: Map<String, Value> = store["appdetails_cache"].as_object().cloned().unwrap_or_default();
    let mut unmatched: Vec<Value> = Vec::new();
    for i in 0..n {
        let rec = wins_mut(store)[i].clone();
        if rec["appid"].as_i64().map_or(false, |a| lib.contains(&a)) { continue; }
        // credited to its base game already: it stays unmatched by appid for good, so it must not keep reappearing
        if rec["dlc_base_adopted"].as_bool().unwrap_or(false) || rec["dlc_base_adopted"].as_i64().map_or(false, |x| x != 0) {
            if rec["dlc_base_appid"].as_i64().filter(|a| *a != 0).is_none() {
                let (_, base, _) = classify_unmatched(&rec, steam, &mut cache);
                if let Some(b) = base.filter(|b| *b != 0) { wins_mut(store)[i]["dlc_base_appid"] = json!(b); }
            }
            continue;
        }
        let (reason, base_id, base_name) = classify_unmatched(&rec, steam, &mut cache);
        unmatched.push(json!({"code": rec["code"], "name": rec["name"], "steam_ref": rec["steam_ref"], "won_ts": rec["won_ts"], "gifter": rec["gifter"], "received": rec["received"],
            "reason": reason, "dlc_base_appid": base_id, "dlc_base_name": base_name}));
    }
    store["appdetails_cache"] = Value::Object(cache);
    store["unmatched"] = Value::Array(unmatched.clone());
    let unresolved = unmatched.len();

    let mut received: Vec<(i64, bool)> = Vec::new();
    let bump = |v: &mut Vec<(i64, bool)>, a: i64, r: bool| match v.iter_mut().find(|(x, _)| *x == a) { Some(e) => e.1 = e.1 || r, None => v.push((a, r)) };
    for rec in wins_mut(store).iter() { if let Some(a) = rec["appid"].as_i64().filter(|a| *a != 0) { bump(&mut received, a, rec["received"] == true); } }
    // DLC wins credited to a base game count for it, kept apart from direct wins so a later change to the DLC win cannot un-adopt the base
    let mut dlc_derived: HashSet<i64> = HashSet::new();
    for rec in wins_mut(store).iter() {
        if !(rec["dlc_base_adopted"] == true) || rec["received"] != true { continue; }
        let Some(base) = rec["dlc_base_appid"].as_i64().filter(|a| *a != 0) else { continue };
        if !received.iter().any(|(a, r)| *a == base && *r) { dlc_derived.insert(base); }
        match received.iter_mut().find(|(a, _)| *a == base) { Some(e) => e.1 = true, None => received.push((base, true)) }
    }

    let prev: BTreeSet<i64> = gs.0["sources"][SOURCE_ID]["members"].as_array().into_iter().flatten().filter_map(|a| a.as_i64()).collect();
    let mut members: BTreeSet<i64> = if full_refresh { BTreeSet::new() } else { prev.clone() };
    let (mut added, mut removed) = (0usize, 0usize);
    for (aid, has) in &received {
        let Some(mut existing) = groups_of(conn, *aid) else { continue };
        if *has {
            members.insert(*aid);
            if !existing.contains(GROUP_NAME) { existing.insert(GROUP_NAME.into()); set_groups(conn, *aid, &existing)?; added += 1; }
            gs.add_owner(*aid, GROUP_NAME, SOURCE_ID);
            conn.execute("UPDATE games SET sg_dlc_win = ? WHERE appid = ?", rusqlite::params![if dlc_derived.contains(aid) { Some(1) } else { None }, aid])?;
        } else if full_refresh {
            members.remove(aid);
            if existing.contains(GROUP_NAME) && !gs.is_protected(*aid, GROUP_NAME, SOURCE_ID) {
                existing.remove(GROUP_NAME); set_groups(conn, *aid, &existing)?; removed += 1;
                conn.execute("UPDATE games SET sg_dlc_win = NULL WHERE appid = ?", [aid])?;
            }
            gs.remove_owner(*aid, GROUP_NAME, SOURCE_ID);
        }
    }
    if full_refresh {
        for aid in prev.difference(&members) {
            if let Some(mut existing) = groups_of(conn, *aid) {
                if existing.contains(GROUP_NAME) && !gs.is_protected(*aid, GROUP_NAME, SOURCE_ID) {
                    existing.remove(GROUP_NAME); set_groups(conn, *aid, &existing)?; removed += 1;
                    conn.execute("UPDATE games SET sg_dlc_win = NULL WHERE appid = ?", [aid])?;
                }
            }
            gs.remove_owner(*aid, GROUP_NAME, SOURCE_ID);
        }
    }
    if !gs.0["sources"].is_object() { gs.0["sources"] = json!({}); }
    gs.0["sources"][SOURCE_ID] = json!({"type": "steamgifts", "name": GROUP_NAME, "members": members.iter().collect::<Vec<_>>()});
    Ok(Summary { games_in_group: members.len(), groups_added: added, groups_removed: removed, unresolved_refs: unresolved, wins_total: wins_mut(store).len(), unmatched })
}

// ───────────────────────── wins that are not in the library ─────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct UnmatchedRow {
    pub code: String, pub name: String, pub steam_ref: String, pub gifter: String, pub received: Option<bool>, pub reason: String,
    /// Steam's own `fullgame` for a DLC / soundtrack / demo win, whether or not the base game is owned.
    pub dlc_base_appid: Option<i64>, pub dlc_base_name: Option<String>,
    /// A game the user may still own although the store no longer lists it: offer to add it.
    pub adoptable: bool,
    /// The base game is in the library now (checked when the list is built, not at the last sync): offer to credit it.
    pub dlc_base_owned: bool,
}

/// The wins from the last apply that are not in the library, sorted by reason then name.
pub fn unmatched_rows(store: &Value, lib: &HashSet<i64>) -> Vec<UnmatchedRow> {
    let mut rows: Vec<UnmatchedRow> = store["unmatched"].as_array().into_iter().flatten().map(|r| {
        let s = |k: &str| r[k].as_str().unwrap_or("").to_string();
        let (reason, steam_ref) = (s("reason"), s("steam_ref"));
        let base = r["dlc_base_appid"].as_i64().filter(|a| *a != 0);
        UnmatchedRow {
            code: s("code"), name: s("name"), gifter: s("gifter"), received: r["received"].as_bool(), dlc_base_appid: base, dlc_base_name: r["dlc_base_name"].as_str().map(String::from),
            adoptable: (steam_ref.starts_with("app/") || steam_ref.starts_with("sub/")) && (reason.contains("Delisted") || reason.contains("removed") || reason.starts_with("Not in your Steam library")),
            dlc_base_owned: base.map_or(false, |b| lib.contains(&b)), reason, steam_ref,
        }
    }).collect();
    rows.sort_by(|a, b| (&a.reason, a.name.to_lowercase()).cmp(&(&b.reason, b.name.to_lowercase())));
    rows
}

/// `_adopt_into_group`: the group on a game, the source as its owner and a member of the source. True when the group was newly added.
pub fn adopt_into_group(conn: &Connection, gs: &mut GroupSources, appid: i64) -> rusqlite::Result<bool> {
    let mut g = groups_of(conn, appid).unwrap_or_default();
    let added = g.insert(GROUP_NAME.to_string());
    if added { set_groups(conn, appid, &g)?; }
    gs.add_owner(appid, GROUP_NAME, SOURCE_ID);
    if !gs.0["sources"].is_object() { gs.0["sources"] = json!({}); }
    let mut members: BTreeSet<i64> = gs.0["sources"][SOURCE_ID]["members"].as_array().into_iter().flatten().filter_map(|a| a.as_i64()).collect();
    members.insert(appid);
    gs.0["sources"][SOURCE_ID] = json!({"type": "steamgifts", "name": GROUP_NAME, "members": members.iter().collect::<Vec<_>>()});
    Ok(added)
}

/// Credit a DLC / soundtrack win to its base game (which must already be in the library): tag the base game, stamp `sg_dlc_win` when this is what put it in the group,
/// and remember on the win itself (`dlc_base_adopted`, `dlc_base_appid`) so later syncs keep it. The DLC is never given a library row of its own.
pub fn adopt_dlc_base(conn: &Connection, gs: &mut GroupSources, store: &mut Value, code: &str) -> Result<bool, String> {
    if code.is_empty() { return Err("Missing giveaway code".into()); }
    let rec = store["unmatched"].as_array().into_iter().flatten().find(|w| w["code"] == code).cloned().ok_or("Unknown win")?;
    let base = rec["dlc_base_appid"].as_i64().filter(|a| *a != 0).ok_or("This win has no known base game")?;
    if conn.query_row("SELECT 1 FROM games WHERE appid = ?", [base], |_| Ok(())).is_err() { return Err("Base game is not in your library".into()); }
    let added = adopt_into_group(conn, gs, base).map_err(|e| e.to_string())?;
    if added { conn.execute("UPDATE games SET sg_dlc_win = 1 WHERE appid = ?", [base]).map_err(|e| e.to_string())?; }
    if let Some(w) = wins_mut(store).iter_mut().find(|w| w["code"] == code) { w["dlc_base_adopted"] = json!(true); w["dlc_base_appid"] = json!(base); }
    if let Some(u) = store["unmatched"].as_array_mut() { u.retain(|w| w["code"] != code); }
    Ok(added)
}

/// What Steam's own files on this machine know about a game the account owns but the store API no longer returns (a delisted one).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LocalSteam { pub name: Option<String>, pub release_date: Option<i64>, pub playtime: i64, pub last_played: Option<i64>, pub installed: bool,
    /// (unlocked, total) from the stats files or the web API, and whether that is every achievement.
    pub cheevos: Option<(i64, i64)> }

#[derive(Debug, Clone, PartialEq)]
pub struct Adopted { pub appid: i64, pub name: String, pub already_present: bool, pub group_added: bool, pub playtime: i64 }

/// `sg_adopt`: add a win the library does not hold (a game GetOwnedGames no longer returns that the user confirms they still own) from what Steam's local files know,
/// and put it in the group. `local` is asked for the appid once it is known.
pub fn adopt_win(conn: &Connection, gs: &mut GroupSources, store: &mut Value, code: &str, steam: &dyn SteamLookup, local: &dyn Fn(i64) -> LocalSteam, today: &str, now: i64) -> Result<Adopted, String> {
    if code.is_empty() { return Err("Missing giveaway code".into()); }
    let rec = wins_mut(store).iter().find(|w| w["code"] == code).cloned().ok_or("Unknown win")?;
    let (kind, sid) = rec["steam_ref"].as_str().unwrap_or("").split_once('/').unwrap_or(("", ""));
    let appid = match (kind, sid.parse::<i64>()) { ("app", Ok(n)) => Some(n), ("sub", Ok(n)) => steam.package_apps(n).into_iter().min(), _ => None }.ok_or("No usable Steam AppID on this win")?;
    let present = conn.query_row("SELECT 1 FROM games WHERE appid = ?", [appid], |_| Ok(())).is_ok();
    let (name, playtime);
    if present {
        let group_added = adopt_into_group(conn, gs, appid).map_err(|e| e.to_string())?;
        let n: String = conn.query_row("SELECT name FROM games WHERE appid = ?", [appid], |r| r.get::<_, Option<String>>(0)).ok().flatten().unwrap_or_default();
        return Ok(Adopted { appid, name: n, already_present: true, group_added, playtime: 0 });
    }
    let l = local(appid);
    name = l.name.clone().filter(|n| !n.is_empty()).or_else(|| rec["name"].as_str().map(String::from)).unwrap_or_else(|| format!("App {appid}"));
    playtime = l.playtime;
    let status = if playtime > 0 { "Unfinished" } else { "Never Played" };
    conn.execute("INSERT OR IGNORE INTO games (appid, name, playtime_forever, last_played, date_added, completion_status, installed, icon_hash, platform, art_fetched, meta_fetched, cheevos_fetched, protondb_fetched, hltb_fetched)
                  VALUES (?, ?, ?, ?, ?, ?, ?, '', 'steam', '0', '0', '0', '0', '0')", rusqlite::params![appid, name, playtime, l.last_played, now, status, l.installed as i64]).map_err(|e| e.to_string())?;
    if let Some(rd) = l.release_date { conn.execute("UPDATE games SET release_date = ? WHERE appid = ?", rusqlite::params![rd, appid]).map_err(|e| e.to_string())?; }
    if let Some((un, tot)) = l.cheevos.filter(|(_, t)| *t > 0) {
        conn.execute("UPDATE games SET total_achievements = ?, unlocked_achievements = ?, cheevos_fetched = ? WHERE appid = ?", rusqlite::params![tot, un, today, appid]).map_err(|e| e.to_string())?;
        if un == tot && playtime > 0 { conn.execute("UPDATE games SET completion_status = 'Completed' WHERE appid = ?", [appid]).map_err(|e| e.to_string())?; }
    }
    let group_added = adopt_into_group(conn, gs, appid).map_err(|e| e.to_string())?;
    if let Some(w) = wins_mut(store).iter_mut().find(|w| w["code"] == code) { w["appid"] = json!(appid); }
    if let Some(u) = store["unmatched"].as_array_mut() { u.retain(|w| w["code"] != code); }
    Ok(Adopted { appid, name, already_present: false, group_added, playtime })
}

#[cfg(test)]
mod adopt_tests {
    use super::*;
    struct Table;
    impl SteamLookup for Table {
        fn package_apps(&self, s: i64) -> Vec<i64> { if s == 700 { vec![9005, 9004] } else { vec![] } }
        fn appdetails(&self, _: i64) -> Details { Details::default() }
    }
    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute("CREATE TABLE games (appid INTEGER PRIMARY KEY, name TEXT, `groups` TEXT, sg_dlc_win INTEGER, playtime_forever INTEGER, last_played INTEGER, date_added INTEGER, completion_status TEXT,
            installed INTEGER, icon_hash TEXT, platform TEXT, art_fetched TEXT, meta_fetched TEXT, cheevos_fetched TEXT, protondb_fetched TEXT, hltb_fetched TEXT, release_date INTEGER, total_achievements INTEGER, unlocked_achievements INTEGER)", []).unwrap();
        c.execute("INSERT INTO games (appid, name, `groups`) VALUES (100, 'Base Game', ''), (101, 'Owned Already', 'Fav')", []).unwrap();
        c
    }
    fn store() -> Value {
        json!({"wins": [
            {"code": "GONE1", "name": "Delisted Thing", "steam_ref": "app/555", "received": true, "appid": null},
            {"code": "PKG02", "name": "Gone Pack", "steam_ref": "sub/700", "received": true, "appid": null},
            {"code": "HAVE3", "name": "Owned Already", "steam_ref": "app/101", "received": true, "appid": null},
            {"code": "DLC04", "name": "Base DLC", "steam_ref": "app/300", "received": true, "appid": null},
            {"code": "CARD5", "name": "$5 Card", "steam_ref": null, "received": true, "appid": null}],
          "unmatched": [
            {"code": "GONE1", "name": "Delisted Thing", "steam_ref": "app/555", "reason": "Delisted, or removed from your Steam account", "dlc_base_appid": null, "dlc_base_name": null, "received": true, "gifter": "g"},
            {"code": "DLC04", "name": "Base DLC", "steam_ref": "app/300", "reason": "DLC for Base Game", "dlc_base_appid": 100, "dlc_base_name": "Base Game", "received": true, "gifter": "g"},
            {"code": "CARD5", "name": "$5 Card", "steam_ref": null, "reason": "No Steam store page (gift card or non-Steam reward)", "dlc_base_appid": null, "dlc_base_name": null, "received": true, "gifter": "g"},
            {"code": "ZZZ", "name": "Z", "steam_ref": "app/1", "reason": "Not in your Steam library (delisted, or removed from your account)", "dlc_base_appid": 999, "dlc_base_name": "X", "received": null, "gifter": "g"}]})
    }

    #[test] fn the_list_is_sorted_and_says_what_can_be_done_with_each() {
        let rows = unmatched_rows(&store(), &[100].into());
        assert_eq!(rows.iter().map(|r| r.code.as_str()).collect::<Vec<_>>(), vec!["DLC04", "GONE1", "CARD5", "ZZZ"], "by reason, then name");
        let by = |c: &str| rows.iter().find(|r| r.code == c).unwrap().clone();
        assert!(by("GONE1").adoptable && !by("GONE1").dlc_base_owned);
        assert!(!by("DLC04").adoptable && by("DLC04").dlc_base_owned);
        assert!(!by("CARD5").adoptable, "no store page");
        assert!(by("ZZZ").adoptable && !by("ZZZ").dlc_base_owned, "a base game that is not in the library cannot be credited");
    }

    #[test] fn a_dlc_win_is_credited_to_its_owned_base_game_and_remembered() {
        let (c, mut gs, mut st) = (db(), GroupSources::empty(), store());
        assert!(adopt_dlc_base(&c, &mut gs, &mut st, "DLC04").unwrap());
        let (g, f): (String, Option<i64>) = c.query_row("SELECT `groups`, sg_dlc_win FROM games WHERE appid = 100", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((g.as_str(), f), (GROUP_NAME, Some(1)));
        assert_eq!(st["wins"][3]["dlc_base_adopted"], true);
        assert_eq!(st["wins"][3]["dlc_base_appid"], 100);
        assert!(st["unmatched"].as_array().unwrap().iter().all(|w| w["code"] != "DLC04"));
        assert!(gs.is_protected(100, GROUP_NAME, "someone else") && gs.0["sources"][SOURCE_ID]["members"] == json!([100]));
        // a later sync keeps it (the adopted DLC counts for its base game)
        let steam = Table;
        st["wins"][3]["_seen_this_run"] = json!(true);   // a full refresh re-confirmed this win
        let s = apply_wins(&c, &mut gs, &mut st, true, &steam).unwrap();
        assert_eq!(s.games_in_group, 1, "the base game stays in the group through a full refresh");
        // errors
        assert_eq!(adopt_dlc_base(&c, &mut gs, &mut st, "").unwrap_err(), "Missing giveaway code");
        assert_eq!(adopt_dlc_base(&c, &mut gs, &mut st, "NOPE").unwrap_err(), "Unknown win");
        assert_eq!(adopt_dlc_base(&c, &mut gs, &mut store(), "ZZZ").unwrap_err(), "Base game is not in your library");
        assert_eq!(adopt_dlc_base(&c, &mut gs, &mut store(), "GONE1").unwrap_err(), "This win has no known base game");
    }

    #[test] fn a_delisted_game_is_added_from_steams_local_files() {
        let (c, mut gs, mut st) = (db(), GroupSources::empty(), store());
        let local = |a: i64| { assert_eq!(a, 555); LocalSteam { name: Some("Real Name".into()), release_date: Some(1_500_000_000), playtime: 120, last_played: Some(1_600_000_000), installed: true, cheevos: Some((3, 3)) } };
        let a = adopt_win(&c, &mut gs, &mut st, "GONE1", &Table, &local, "2026-11-03", 1_700_000_000).unwrap();
        assert_eq!((a.appid, a.name.as_str(), a.already_present, a.group_added, a.playtime), (555, "Real Name", false, true, 120));
        let r: (String, i64, Option<i64>, String, i64, i64, i64, String) = c.query_row("SELECT name, playtime_forever, last_played, completion_status, installed, total_achievements, unlocked_achievements, `groups` FROM games WHERE appid = 555", [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?))).unwrap();
        assert_eq!(r, ("Real Name".into(), 120, Some(1_600_000_000), "Completed".into(), 1, 3, 3, GROUP_NAME.into()));
        assert_eq!(st["wins"][0]["appid"], 555);
        assert!(st["unmatched"].as_array().unwrap().iter().all(|w| w["code"] != "GONE1"));
        // a package: the lowest app id; with no local info the win's own name and Never Played
        let a = adopt_win(&c, &mut gs, &mut st, "PKG02", &Table, &|_| LocalSteam::default(), "2026-11-03", 1_700_000_000).unwrap();
        assert_eq!((a.appid, a.name.as_str()), (9004, "Gone Pack"));
        assert_eq!(c.query_row("SELECT completion_status FROM games WHERE appid = 9004", [], |r| r.get::<_, String>(0)).unwrap(), "Never Played");
        // a game that is already in the library only gets the group
        let a = adopt_win(&c, &mut gs, &mut st, "HAVE3", &Table, &|_| panic!("not needed"), "2026-11-03", 1).unwrap();
        assert_eq!((a.appid, a.already_present, a.group_added), (101, true, true));
        assert_eq!(c.query_row("SELECT `groups` FROM games WHERE appid = 101", [], |r| r.get::<_, String>(0)).unwrap(), format!("Fav,{GROUP_NAME}"));
        // no usable id
        assert_eq!(adopt_win(&c, &mut gs, &mut st, "CARD5", &Table, &|_| LocalSteam::default(), "d", 1).unwrap_err(), "No usable Steam AppID on this win");
    }
}
