//! Play or Pay: tags each game you picked in a running Play or Pay cycle with a group named after the event, and keeps one evergreen "Play or Pay"
//! saved filter over every group it tracks. A port of `pop_sync.py`. The network part ([`fetch_plan`]) runs in a thread; applying it
//! ([`apply`]) changes groups and needs the library, so it runs where the library lives, through the [`Groups`] trait.
use crate::net::Http;
use scraper::{Html, Selector};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The site moved from playorpaysg.com (a JSON API, no longer updated) to playorpay.games (server-rendered HTML, Oct 2026): cycles and picks are read from the pages.
pub const SITE: &str = "https://www.playorpay.games";
pub const FILTER_NAME: &str = "Play or Pay";

/// What one event says about the user's picks.
#[derive(Debug, Clone, PartialEq)]
pub struct EventPicks { /// The cycle number on playorpay.games (an old API uuid in state saved before the move).
    pub uuid: String, pub name: String, pub appids: Vec<i64>, pub names: BTreeMap<i64, String> }

/// What the network said: the events running today that the user takes part in. `None` = nothing running at all.
#[derive(Debug, Clone, PartialEq)]
pub enum Plan { NoActiveEvent, NotSignedUp, Events(Vec<EventPicks>) }

/// One cycle from the list page. `start`/`end` are the page's UTC timestamps (`2026-09-25T00:00:00Z`).
#[derive(Debug, Clone, PartialEq)]
pub struct Cycle { pub id: String, pub name: String, pub start: String, pub end: String }

fn sel(s: &str) -> Selector { Selector::parse(s).expect("valid selector") }

fn get_html(http: &dyn Http, path: &str) -> Result<String, String> {
    let r = http.get(&format!("{SITE}{path}"), &[("User-Agent", "PlayDate")], Duration::from_secs(30)).map_err(|e| format!("Play or Pay: {e}"))?;
    if r.status != 200 { return Err(format!("Play or Pay answered {}", r.status)); }
    Ok(r.body)
}

/// The `/cycles` page -> its cycles (number, name, dates).
pub fn parse_cycles(html: &str) -> Vec<Cycle> {
    let doc = Html::parse_document(html);
    let (entry, link, name, dates) = (sel(".cycle-entry"), sel("a[href^=\"/cycles/\"]"), sel(".cycle-name"), sel(".cycle-dates [data-utc]"));
    let mut out = Vec::new();
    for e in doc.select(&entry) {
        let Some(href) = e.select(&link).next().and_then(|a| a.value().attr("href")) else { continue };
        let id = href.trim_end_matches('/').rsplit('/').next().unwrap_or("");
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) { continue; }
        let Some(n) = e.select(&name).next() else { continue };
        let d: Vec<&str> = e.select(&dates).filter_map(|x| x.value().attr("data-utc")).collect();
        if d.len() < 2 { continue; }
        out.push(Cycle { id: id.to_string(), name: n.text().collect::<String>().trim().to_string(), start: d[0].to_string(), end: d[1].to_string() });
    }
    out
}

/// Cycles whose dates contain `today` (`YYYY-MM-DD`, UTC; the dates are compared as text, which orders correctly; the end day counts).
pub fn active_events(cycles: &[Cycle], today: &str) -> Vec<Cycle> {
    cycles.iter().filter(|c| c.start.len() >= 10 && c.end.len() >= 10 && &c.start[..10] <= today && today <= &c.end[..10]).cloned().collect()
}

/// "Brazil, 2026!" -> "Brazil 2026!": commas out (groups are comma-separated), whitespace collapsed.
pub fn sanitize_tag(name: &str) -> String { name.replace(',', "").split_whitespace().collect::<Vec<_>>().join(" ") }

/// The user's Steam picks on one cycle page (`None` when they are not a participant) and the names the page gives them.
pub fn extract_picks(html: &str, steam_id: &str) -> Option<(Vec<i64>, BTreeMap<i64, String>)> {
    let doc = Html::parse_document(html);
    if !doc.select(&sel(".cycle-card")).any(|c| c.value().attr("data-participant") == Some(steam_id)) { return None; }
    let (slots, pick, img) = (sel(".cycle-picker-slot"), sel(".pick-display"), sel("img"));
    let (mut appids, mut names) = (Vec::new(), BTreeMap::new());
    for slot in doc.select(&slots) {
        if slot.value().attr("data-participant") != Some(steam_id) { continue; }
        let Some(p) = slot.select(&pick).next() else { continue };
        let Some(id) = p.value().attr("data-appid").and_then(|s| s.parse::<i64>().ok()) else { continue };
        if names.contains_key(&id) { continue; }
        names.insert(id, p.select(&img).next().and_then(|i| i.value().attr("alt")).unwrap_or("").to_string());
        appids.push(id);
    }
    Some((appids, names))
}

pub fn fetch_plan(http: &dyn Http, steam_id: &str, today: &str) -> Result<Plan, String> {
    let cycles = parse_cycles(&get_html(http, "/cycles")?);
    let active = active_events(&cycles, today);
    if active.is_empty() { return Ok(Plan::NoActiveEvent); }
    let mut out = Vec::new();
    for c in active {
        let html = get_html(http, &format!("/cycles/{}", c.id))?;
        if let Some((appids, names)) = extract_picks(&html, steam_id) {
            out.push(EventPicks { uuid: c.id.clone(), name: c.name.clone(), appids, names });
        }
    }
    Ok(if out.is_empty() { Plan::NotSignedUp } else { Plan::Events(out) })
}

// ───────────────────────── applying ─────────────────────────

/// What applying needs from the library.
pub trait Groups {
    fn append(&mut self, tag: &str, appids: &[i64]) -> Result<(), String>;
    fn remove(&mut self, tag: &str, appids: &[i64]) -> Result<(), String>;
    /// Which of these appids are in the library.
    fn existing(&self, appids: &[i64]) -> HashSet<i64>;
}

/// `pop_sync_<steam_id>.json`: per event uuid, the tag and the appids last tagged (the same file PlayDate uses).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SyncState { pub events: BTreeMap<String, (String, Vec<i64>)> }

pub fn state_path(root: &Path, steam_id: &str) -> PathBuf { root.join(format!("pop_sync_{steam_id}.json")) }
pub fn load_state(root: &Path, steam_id: &str) -> SyncState {
    let v: Value = std::fs::read_to_string(state_path(root, steam_id)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
    let mut s = SyncState::default();
    for (u, e) in v["events"].as_object().into_iter().flatten() {
        s.events.insert(u.clone(), (e["tag"].as_str().unwrap_or("").to_string(), e["appids"].as_array().map(|a| a.iter().filter_map(|x| x.as_i64()).collect()).unwrap_or_default()));
    }
    s
}
pub fn save_state(root: &Path, steam_id: &str, s: &SyncState) -> std::io::Result<()> {
    let mut ev = Map::new();
    for (u, (tag, ids)) in &s.events { ev.insert(u.clone(), json!({"tag": tag, "appids": ids})); }
    crate::account::write_json(&state_path(root, steam_id), &json!({"events": ev}))
}

#[derive(Debug, Clone, PartialEq)]
pub struct Summary { pub event: String, pub tag: String, pub total: usize, pub matched: usize, pub missing: Vec<String> }

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Nothing to do, with a sentence for the user.
    Info(String),
    /// A new cycle began while groups of an old one exist: ask whether to remove them (call again with `Some(answer)`). Nothing changed yet.
    ConfirmCleanup { stale_tags: Vec<String> },
    Done { message: String, events: Vec<Summary>, /// Every tag now tracked: the evergreen filter lists these groups (none = remove the filter).
        tags: BTreeSet<String> },
}

/// `confirm_cleanup`: `None` = ask if a cleanup decision is needed; `Some(true)` = remove the groups of cycles the user is no longer in; `Some(false)` = leave them.
pub fn apply(plan: &Plan, root: &Path, steam_id: &str, groups: &mut dyn Groups, confirm_cleanup: Option<bool>) -> Result<Outcome, String> {
    let events = match plan {
        Plan::NoActiveEvent => return Ok(Outcome::Info("No active PoP event found.".into())),
        Plan::NotSignedUp => return Ok(Outcome::Info("You're not signed up for the current PoP cycle.".into())),
        Plan::Events(e) => e,
    };
    let mut state = load_state(root, steam_id);
    // state saved against the old API is keyed by a uuid; carry it over to the cycle number by tag so the user isn't asked to clean up groups about to be tagged again
    for e in events {
        if state.events.contains_key(&e.uuid) { continue; }
        let tag = sanitize_tag(&e.name);
        if let Some(old) = state.events.iter().find(|(k, v)| k.contains('-') && v.0 == tag).map(|(k, _)| k.clone()) {
            if let Some(v) = state.events.remove(&old) { state.events.insert(e.uuid.clone(), v); }
        }
    }
    let known: BTreeSet<String> = state.events.keys().cloned().collect();
    let current: BTreeSet<String> = events.iter().map(|e| e.uuid.clone()).collect();
    let newly_seen = current.difference(&known).count() > 0;
    let stale: Vec<String> = known.difference(&current).cloned().collect();
    if newly_seen && !stale.is_empty() && confirm_cleanup.is_none() {
        return Ok(Outcome::ConfirmCleanup { stale_tags: stale.iter().filter_map(|u| state.events.get(u)).map(|e| e.0.clone()).filter(|t| !t.is_empty()).collect() });
    }
    if confirm_cleanup == Some(true) {
        for u in &stale {
            if let Some((tag, ids)) = state.events.remove(u) { if !tag.is_empty() && !ids.is_empty() { groups.remove(&tag, &ids)?; } }
        }
    }
    let mut summaries = Vec::new();
    for e in events {
        let tag = sanitize_tag(&e.name);
        let (prior_tag, prior_ids) = state.events.get(&e.uuid).cloned().unwrap_or_default();
        let new: HashSet<i64> = e.appids.iter().copied().collect();
        // a renamed event drops everything under the old name; the same name drops only picks that were taken back
        let drop: Vec<i64> = if prior_tag == tag { prior_ids.iter().copied().filter(|a| !new.contains(a)).collect() } else { prior_ids.clone() };
        if !drop.is_empty() { groups.remove(if prior_tag.is_empty() { &tag } else { &prior_tag }, &drop)?; }
        if !e.appids.is_empty() { groups.append(&tag, &e.appids)?; }
        state.events.insert(e.uuid.clone(), (tag.clone(), e.appids.clone()));
        let have = groups.existing(&e.appids);
        summaries.push(Summary { event: e.name.clone(), tag, total: e.appids.len(), matched: have.len(), missing: e.appids.iter().filter(|a| !have.contains(a)).map(|a| e.names.get(a).cloned().unwrap_or_else(|| a.to_string())).collect() });
    }
    save_state(root, steam_id, &state).map_err(|e| format!("could not save the sync state: {e}"))?;
    let tags: BTreeSet<String> = state.events.values().map(|e| e.0.clone()).filter(|t| !t.is_empty()).collect();
    let message = summaries.iter().map(|s| {
        let mut p = format!("{}: tagged {}/{} game(s)", s.tag, s.matched, s.total);
        if !s.missing.is_empty() { p += &format!(" ({} not in library: {})", s.missing.len(), s.missing.join(", ")); }
        p
    }).collect::<Vec<_>>().join("; ");
    Ok(Outcome::Done { message, events: summaries, tags })
}

/// The evergreen saved filter for `tags` (groups LIKE any of them), reusing `existing_id`; `None` when nothing is tracked (the filter is removed).
pub fn filter_entry(tags: &BTreeSet<String>, existing_id: Option<&str>, new_id: impl FnOnce() -> String) -> Option<Value> {
    if tags.is_empty() { return None; }
    let id = existing_id.map(String::from).unwrap_or_else(new_id);
    Some(json!({"id": id, "tree": {"type": "group", "logic": "AND", "items": [{"type": "condition", "column": "groups", "operator": "LIKE", "value": tags.iter().collect::<Vec<_>>()}]}}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{Response, NetError};
    use std::collections::HashMap;

    struct Fake(HashMap<String, String>);
    impl Http for Fake {
        fn get(&self, url: &str, _: &[(&str, &str)], _: Duration) -> Result<Response, NetError> {
            Ok(match self.0.get(url) { Some(b) => Response { status: 200, retry_after: None, body: b.clone() }, None => Response { status: 404, retry_after: None, body: String::new() } })
        }
    }
    const CYCLES: &str = r#"
<div class="cycle-entry"><a href="/cycles/27" class="cycle-entry-link"><span class="cycle-name">Nov 2026</span>
<span class="cycle-dates"><span data-utc="2026-11-01T00:00:00Z">a</span> – <span data-utc="2026-11-30T00:00:00Z">b</span></span></a></div>
<div class="cycle-entry"><a href="/cycles/26" class="cycle-entry-link"><span class="cycle-name">Oct 2026</span>
<span class="cycle-dates"><span data-utc="2026-10-01T00:00:00Z">a</span> – <span data-utc="2026-10-31T00:00:00Z">b</span></span></a></div>
<div class="cycle-entry"><a href="/cycles/x" class="cycle-entry-link"><span class="cycle-name">Broken</span></a></div>"#;
    /// A cycle page: the user "7656" has the given picks (an empty string = a slot with no pick); someone else and a picker slot sit beside them.
    fn page(picks: &[&str]) -> String {
        let mut h = String::from(r#"<div class="cycle-card" data-participant="someone-else"></div><div class="cycle-card" data-participant="7656"></div>"#);
        for (i, p) in picks.iter().enumerate() {
            if p.is_empty() { h += &format!(r#"<div class="cycle-picker-slot" data-participant="7656" data-slot="{i}"></div>"#); }
            else { h += &format!(r#"<div class="cycle-picker-slot" data-participant="7656" data-slot="{i}"><div class="pick-display" data-appid="{p}"><img alt="Game {p}"></div></div>"#); }
        }
        h + r#"<div class="cycle-picker-slot" data-participant="someone-else"><div class="pick-display" data-appid="999"></div></div><div class="cycle-picker-slot"><div class="pick-display" data-appid="998"></div></div>"#
    }
    fn http(pages: &[(&str, String)]) -> Fake {
        let mut m = HashMap::new();
        m.insert(format!("{SITE}/cycles"), CYCLES.to_string());
        for (id, d) in pages { m.insert(format!("{SITE}/cycles/{id}"), d.clone()); }
        Fake(m)
    }
    #[derive(Default)]
    struct Lib { tags: BTreeMap<i64, BTreeSet<String>>, owned: HashSet<i64> }
    impl Groups for Lib {
        fn append(&mut self, tag: &str, ids: &[i64]) -> Result<(), String> { for i in ids { self.tags.entry(*i).or_default().insert(tag.into()); } Ok(()) }
        fn remove(&mut self, tag: &str, ids: &[i64]) -> Result<(), String> { for i in ids { if let Some(s) = self.tags.get_mut(i) { s.remove(tag); } } Ok(()) }
        fn existing(&self, ids: &[i64]) -> HashSet<i64> { ids.iter().copied().filter(|i| self.owned.contains(i)).collect() }
    }
    fn tmp(n: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("zest_pop_{n}_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d }

    #[test] fn only_cycles_running_today_count_and_tags_are_cleaned() {
        let cycles = parse_cycles(CYCLES);
        assert_eq!(cycles.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), vec!["27", "26"]);   // the broken entry is skipped
        assert_eq!(cycles[0], Cycle { id: "27".into(), name: "Nov 2026".into(), start: "2026-11-01T00:00:00Z".into(), end: "2026-11-30T00:00:00Z".into() });
        assert_eq!(active_events(&cycles, "2026-11-02").len(), 1);
        assert_eq!(active_events(&cycles, "2026-11-30").len(), 1);   // the end day counts
        assert_eq!(active_events(&cycles, "2027-01-01").len(), 0);
        assert_eq!(sanitize_tag("  Play or  Pay,  Nov,  2026 "), "Play or Pay Nov 2026");
    }
    #[test] fn picks_are_the_users_games_from_the_cycle_page() {
        let h = http(&[("27", page(&["11", "", "12", "x", "11"]))]);
        match fetch_plan(&h, "7656", "2026-11-02").unwrap() {
            Plan::Events(e) => { assert_eq!(e[0].uuid, "27"); assert_eq!(e[0].name, "Nov 2026"); assert_eq!(e[0].appids, vec![11, 12]); assert_eq!(e[0].names[&12], "Game 12"); assert!(!e[0].names.contains_key(&999)); }
            o => panic!("{o:?}"),
        }
        assert_eq!(fetch_plan(&h, "nobody", "2026-11-02").unwrap(), Plan::NotSignedUp);
        assert_eq!(fetch_plan(&h, "7656", "2030-01-01").unwrap(), Plan::NoActiveEvent);
        assert!(fetch_plan(&Fake(HashMap::new()), "7656", "2026-11-02").unwrap_err().contains("404"));
    }
    #[test] fn state_saved_against_the_old_api_moves_to_the_cycle_number_without_a_prompt() {
        let root = tmp("migrate");
        let mut lib = Lib { owned: [1].into(), ..Default::default() };
        let old = Plan::Events(vec![EventPicks { uuid: "6e883865-f9a2-4042-b3af-344a9f9a8697".into(), name: "Nov 2026".into(), appids: vec![1], names: Default::default() }]);
        apply(&old, &root, "7656", &mut lib, None).unwrap();
        let now = Plan::Events(vec![EventPicks { uuid: "27".into(), name: "Nov 2026".into(), appids: vec![1], names: Default::default() }]);
        assert!(matches!(apply(&now, &root, "7656", &mut lib, None).unwrap(), Outcome::Done { .. }));
        assert_eq!(load_state(&root, "7656").events.keys().collect::<Vec<_>>(), vec!["27"]);
    }
    #[test] fn a_sync_tags_games_reports_the_missing_and_keeps_the_filter_in_step() {
        let root = tmp("sync");
        let plan = Plan::Events(vec![EventPicks { uuid: "a".into(), name: "Nov, 2026".into(), appids: vec![11, 12], names: [(12, "Twelve".to_string())].into() }]);
        let mut lib = Lib { owned: [11].into(), ..Default::default() };
        let Outcome::Done { message, tags, events } = apply(&plan, &root, "7656", &mut lib, None).unwrap() else { panic!() };
        assert_eq!(message, "Nov 2026: tagged 1/2 game(s) (1 not in library: Twelve)");
        assert_eq!(events[0].missing, vec!["Twelve".to_string()]);
        assert!(lib.tags[&11].contains("Nov 2026") && lib.tags[&12].contains("Nov 2026"));
        assert_eq!(tags.iter().collect::<Vec<_>>(), vec!["Nov 2026"]);
        // a pick taken back loses the group on the next sync
        let plan2 = Plan::Events(vec![EventPicks { uuid: "a".into(), name: "Nov, 2026".into(), appids: vec![11], names: Default::default() }]);
        apply(&plan2, &root, "7656", &mut lib, None).unwrap();
        assert!(lib.tags[&12].is_empty() && lib.tags[&11].contains("Nov 2026"));
        assert_eq!(load_state(&root, "7656").events["a"], ("Nov 2026".to_string(), vec![11]));
        // a renamed event moves its games to the new name
        let plan3 = Plan::Events(vec![EventPicks { uuid: "a".into(), name: "Nov 2026 Redux".into(), appids: vec![11], names: Default::default() }]);
        apply(&plan3, &root, "7656", &mut lib, None).unwrap();
        assert_eq!(lib.tags[&11].iter().collect::<Vec<_>>(), vec!["Nov 2026 Redux"]);
    }
    #[test] fn a_new_cycle_asks_before_removing_the_old_groups() {
        let root = tmp("cleanup");
        let mut lib = Lib { owned: [1, 2].into(), ..Default::default() };
        let old = Plan::Events(vec![EventPicks { uuid: "old".into(), name: "Oct 2026".into(), appids: vec![1], names: Default::default() }]);
        apply(&old, &root, "7656", &mut lib, None).unwrap();
        let new = Plan::Events(vec![EventPicks { uuid: "new".into(), name: "Nov 2026".into(), appids: vec![2], names: Default::default() }]);
        assert_eq!(apply(&new, &root, "7656", &mut lib, None).unwrap(), Outcome::ConfirmCleanup { stale_tags: vec!["Oct 2026".into()] });
        assert!(lib.tags[&1].contains("Oct 2026"), "nothing changed yet");
        // declined: the old group stays and stays tracked
        let Outcome::Done { tags, .. } = apply(&new, &root, "7656", &mut lib, Some(false)).unwrap() else { panic!() };
        assert_eq!(tags.len(), 2); assert!(lib.tags[&1].contains("Oct 2026"));
        // accepted (fresh state again)
        let root2 = tmp("cleanup2");
        let mut lib2 = Lib { owned: [1, 2].into(), ..Default::default() };
        apply(&old, &root2, "7656", &mut lib2, None).unwrap();
        let Outcome::Done { tags, .. } = apply(&new, &root2, "7656", &mut lib2, Some(true)).unwrap() else { panic!() };
        assert_eq!(tags.iter().collect::<Vec<_>>(), vec!["Nov 2026"]); assert!(lib2.tags[&1].is_empty());
    }
    #[test] fn the_evergreen_filter_lists_every_tracked_group_or_goes_away() {
        let t: BTreeSet<String> = ["B".to_string(), "A".to_string()].into();
        let e = filter_entry(&t, Some("keep-me"), || "new".into()).unwrap();
        assert_eq!(e["id"], "keep-me");
        assert_eq!(e["tree"]["items"][0]["value"], json!(["A", "B"]));
        assert!(filter_entry(&BTreeSet::new(), None, || "x".into()).is_none());
        assert_eq!(filter_entry(&t, None, || "fresh".into()).unwrap()["id"], "fresh");
    }
}

#[cfg(test)]
mod live {
    use super::*;
    /// Asks the real Play or Pay site what it says about the dev copy's account (run with `--ignored --nocapture`; changes nothing).
    #[test] #[ignore]
    fn the_real_site_for_the_dev_account() {
        let dir = std::path::PathBuf::from(std::env::var("HOME").unwrap()).join(".local/share/zest-dev/data");
        let steam_id = crate::account::load_account(&dir).expect("account").steam_id;
        let today = { let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64; let (y, m, d, _) = crate::pagywosg_filter::civil(now); format!("{y:04}-{m:02}-{d:02}") };
        let http = crate::net::UreqHttp::new();
        let cycles = parse_cycles(&get_html(&http, "/cycles").expect("cycles"));
        eprintln!("{} cycles on the site; running today ({today}): {}", cycles.len(), active_events(&cycles, &today).iter().map(|c| c.name.clone()).collect::<Vec<_>>().join(" | "));
        eprintln!("plan: {:?}", fetch_plan(&http, &steam_id, &today));
    }
}
