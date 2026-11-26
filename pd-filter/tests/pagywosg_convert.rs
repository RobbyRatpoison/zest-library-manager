//! Reading filters that PlayDate (Python) saved: every condition the builder can write is read back from the node PlayDate's own JS produced, and a whole
//! tree becomes a spec with the same event, group, tags, conditions, statuses and lists.
use pd_filter::pagywosg_filter::*;
use serde_json::{json, Value};

#[test]
fn every_condition_playdates_builder_writes_is_read_back() {
    let g: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/pagywosg_convert_golden.json")).expect("run gen_pagywosg_convert_golden.py")).unwrap();
    let mut bad = vec![];
    for c in &g {
        let (col, op, val) = (c["col"].as_str().unwrap(), c["op"].as_str().unwrap(), c["val"].as_str().unwrap());
        // PlayDate lowercases a title word before writing it
        // (the builder also trims what is typed and drops a blank value)
        if val.trim().is_empty() { assert!(c["node"].is_null()); continue; }
        let want_val = if op == "title_word" { val.trim().to_lowercase() } else { val.trim().to_string() };
        let got = read_cond(&c["node"]);
        if got != Some((col.to_string(), op.to_string(), want_val.clone())) { bad.push(format!("{col} {op} {val:?} -> node {} -> {got:?}", c["node"])); }
    }
    assert!(bad.is_empty(), "{} of {} not read back:\n{}", bad.len(), g.len(), bad.iter().take(8).cloned().collect::<Vec<_>>().join("\n"));
}

fn python_tree() -> Value {
    json!({"type": "group", "logic": "AND", "pagywosg": true, "pagywosg_event": {"id": 90, "name": "November 2026 - Test"}, "pagywosg_personal_cats": ["Cat A"],
        "items": [
            {"type": "condition", "column": "platform", "operator": "=", "value": "steam"},
            {"type": "group", "logic": "OR", "items": [
                {"type": "group", "logic": "AND", "items": [
                    {"type": "group", "logic": "OR", "items": [{"type": "condition", "column": "groups", "operator": "LIKE", "value": "Won on SteamGifts"}, {"type": "appid_list", "appids": [50], "label": "Secret Santa / Snowballs", "auto": true}]},
                    {"type": "group", "logic": "OR", "items": [
                        {"type": "condition", "column": "tags", "operator": "LIKE", "value": ["Adventure", "RPG"]},
                        {"type": "custom_expr", "sql": "name LIKE 'Q%'"},
                        {"type": "condition", "column": "release_date", "operator": "STRFTIME_MONTH", "value": "3"},
                        {"type": "appid_list", "appids": [5, 6], "label": "Cat A", "auto": false}]}]},
                {"type": "group", "logic": "OR", "items": [
                    {"type": "condition", "column": "tags", "operator": "LIKE", "value": "Puzzle"},
                    {"type": "custom_expr", "sql": "totally unknown sql"},
                    {"type": "appid_list_ref", "label": "icaio", "source": "icaio_ga"}]}]},
            {"type": "group", "logic": "OR", "items": [{"type": "condition", "column": "completion_status", "operator": "=", "value": "Never Played"}, {"type": "condition", "column": "completion_status", "operator": "=", "value": "Unfinished"}]}],
        "pagywosg_verified": {"7": [{"cat": "Cat B", "pool": "all", "verifiers": ["bob"]}],
            "_c": [{"e": [{"cat": "Cat A", "pool": "wins", "verifiers": ["alice"]}], "ids": [5, 6]},
                   {"e": [{"cat": "Secret Santa / Snowballs", "pool": "wins", "auto": true, "year": 2021}], "ids": [50]},
                   {"e": [{"cat": "Any GA", "pool": "all", "auto": true}], "ids": [1, 2, 3]}]}})
}

#[test]
fn a_whole_playdate_tree_becomes_a_spec_that_answers_like_the_original() {
    let c = spec_from_python_tree(&python_tree()).unwrap();
    let s = &c.spec;
    assert_eq!((s.event_id, s.sg_group.as_deref(), s.statuses.clone(), c.skipped), (Some(90), Some("Won on SteamGifts"), vec!["Never Played".to_string(), "Unfinished".to_string()], 1));
    assert_eq!(s.wins.tags, vec!["Adventure".to_string(), "RPG".to_string()]);
    assert_eq!(s.wins.conds, vec![("name".into(), "starts_with".into(), "Q".into()), ("release_date".into(), "month_is".into(), "3".into())]);
    assert_eq!(s.all.tags, vec!["Puzzle".to_string()]);
    assert_eq!(s.personal, vec!["Cat A".to_string()]);
    let l: Vec<(&str, Pool, bool, bool)> = s.lists.iter().map(|l| (l.label.as_str(), l.pool, l.auto, l.santa)).collect();
    assert_eq!(l, vec![("Cat A", Pool::Wins, false, false), ("Secret Santa / Snowballs", Pool::Wins, true, true), ("Cat B", Pool::All, false, false), ("Any GA", Pool::All, true, false)]);
    assert_eq!(s.lists[0].groups[0].verifiers, vec!["alice".to_string()]);
    assert_eq!(s.lists[1].groups[0].year, Some(2021));
    // the generated tree runs, and a game on the Cat A list that was won gets its line
    let g = PagGame { appid: 5, name: "Plain".into(), groups: "Won on SteamGifts".into(), ..Default::default() };
    assert!(verdict(s, &g).iter().any(|q| q.desc == "Cat A" && q.verified && q.label == "(win)"));
    // not a PlayDate PAGYWOSG tree
    assert!(spec_from_python_tree(&json!({"type": "group", "logic": "AND", "items": []})).is_none());
}

/// Your real PlayDate-built filter, when the dev data folder exists on this machine.
#[test]
fn the_real_november_filter_converts_when_present() {
    let p = std::path::Path::new(&std::env::var("HOME").unwrap_or_default()).join(".local/share/zest-dev/data/state.json");
    let Ok(t) = std::fs::read_to_string(p) else { return };
    let st: Value = serde_json::from_str(&t).unwrap();
    let Some(tree) = st["saved_filters"]["PAGYWOSG November 2026"].get("tree").filter(|t| t["pagywosg"] == true && !t.to_string().contains("pag_filter")) else { return };
    let c = spec_from_python_tree(tree).expect("converts");
    assert_eq!(c.spec.event_id, Some(90));
    assert!(c.spec.sg_group.is_some() && !c.spec.wins.tags.is_empty() && !c.spec.lists.is_empty(), "{:?}", c.spec.wins);
    assert_eq!(c.skipped, 0);
}
