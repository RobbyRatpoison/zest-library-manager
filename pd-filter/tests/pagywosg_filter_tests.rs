//! The PAGYWOSG filter: SQL and per-game answer agree on random games, the generated tree lists only games the verdict can explain, spec round trips,
//! and the scenarios the panel and tooltip rely on (SG group, Santa gifts, DLC star, personal lists).
use pd_filter::pagywosg::cond_sql;
use pd_filter::pagywosg_filter::*;
use pd_filter::{build_tree_sql, Ctx};
use rusqlite::{types::Value as Sql, Connection};
use serde_json::{json, Value};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 { self.0 ^= self.0 << 13; self.0 ^= self.0 >> 7; self.0 ^= self.0 << 17; self.0 }
    fn pick<'a, T>(&mut self, v: &'a [T]) -> &'a T { &v[(self.next() % v.len() as u64) as usize] }
    fn chance(&mut self, n: u64) -> bool { self.next() % n == 0 }
}

const WORDS: [&str; 22] = ["Red", "Book", "Dark", "Quest", "Q", "The", "Zero", "Star", "Night", "Fire", "Word", "Mr.", "O'Neil", "Game!", "Is it?", "UNDER", "ROGUE", "Ünï", "RPG", "Hack", "Indie", "2"];
const TAGS: [&str; 8] = ["Roguelike", "Indie", "RPG", "Strategy", "Turn-Based Strategy", "Co-op", "Hack and Slash", "Puzzle"];
const TIERS: [&str; 4] = ["Very Positive", "Mixed", "Overwhelmingly Positive", "Negative"];
const DEVS: [&str; 5] = ["Ubi Soft", "Valve", "Hidden Door", "Devolver", "RUOK Games"];

fn random_game(r: &mut Rng, appid: i64) -> PagGame {
    let name = if r.chance(9) { (*r.pick(&WORDS)).to_string() } else { (0..1 + r.next() % 4).map(|_| *r.pick(&WORDS)).collect::<Vec<_>>().join(" ") };
    let csv = |r: &mut Rng, pool: &[&str], max: u64| -> String { let n = r.next() % (max + 1); let mut v: Vec<&str> = Vec::new(); for _ in 0..n { let x = *r.pick(pool); if !v.contains(&x) { v.push(x); } } v.join(",") };
    let some = |r: &mut Rng, v: &[i64]| -> Option<i64> { if r.chance(5) { None } else { Some(*r.pick(v)) } };
    PagGame {
        appid, name, tags: csv(r, &TAGS, 5), groups: csv(r, &["Won on SteamGifts", "Favs", "Backlog"], 2), developers: csv(r, &DEVS, 2), publishers: csv(r, &DEVS, 2),
        genres: csv(r, &["Action", "RPG", "Indie"], 2), categories: csv(r, &["Single-player", "Co-op"], 2),
        release_date: if r.chance(6) { *r.pick(&[None, Some(0)]) } else { Some(1_167_609_600 + (r.next() % 1500) as i64 * 86400) },
        total_achievements: some(r, &[0, 5, 12, 40]), hltb_main: some(r, &[0, 60, 120, 300, 600]), hltb_extras: some(r, &[0, 180, 720]), hltb_completionist: some(r, &[0, 1200]),
        review_score: if r.chance(4) { None } else { Some((*r.pick(&TIERS)).to_string()) }, sg_dlc_win: r.chance(10),
    }
}

fn db_with(games: &[PagGame]) -> Connection {
    let c = Connection::open_in_memory().unwrap();
    c.execute("CREATE TABLE games (appid INTEGER PRIMARY KEY, name TEXT, platform TEXT, tags TEXT, genres TEXT, categories TEXT, groups TEXT, developers TEXT, publishers TEXT, release_date INTEGER, date_added INTEGER, last_played INTEGER,
        total_achievements INTEGER, hltb_main INTEGER, hltb_extras INTEGER, hltb_completionist INTEGER, review_score TEXT, review_percentage INTEGER, playtime_forever INTEGER, completion_status TEXT)", []).unwrap();
    for g in games {
        c.execute("INSERT INTO games (appid, name, platform, tags, genres, categories, groups, developers, publishers, release_date, total_achievements, hltb_main, hltb_extras, hltb_completionist, review_score, completion_status) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            rusqlite::params![g.appid, g.name, "steam", g.tags, g.genres, g.categories, g.groups, g.developers, g.publishers, g.release_date, g.total_achievements, g.hltb_main, g.hltb_extras, g.hltb_completionist, g.review_score, "Never Played"]).unwrap();
    }
    c
}
fn to_sql(p: &[pd_filter::pagywosg::Param]) -> Vec<Sql> { p.iter().map(|x| match x { pd_filter::pagywosg::Param::Text(s) => Sql::Text(s.clone()), pd_filter::pagywosg::Param::Int(i) => Sql::Integer(*i) }).collect() }

#[test]
fn sql_and_the_per_game_answer_agree() {
    let golden: Value = serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/pagywosg_golden.json")).unwrap()).unwrap();
    let mut r = Rng(0x9e3779b97f4a7c15);
    let games: Vec<PagGame> = (1..=400).map(|i| random_game(&mut r, 10_000 + i * 37 + (i % 7) * 1000)).collect();
    let db = db_with(&games);
    let (mut checked, mut matched, mut bad) = (0, 0, Vec::<String>::new());
    let mut by_op = std::collections::BTreeMap::<String, (u32, u32)>::new();
    // the generated conditions plus ones aimed at the random vocabulary
    // only column/operator pairs the classifier produces or the builder offers (e.g. not "all caps" on a numeric AppID)
    let mut pairs: std::collections::HashSet<(String, String)> = golden["classify"].as_array().unwrap().iter().flat_map(|c| c["py"].as_array().unwrap().iter())
        .filter(|r| r["type"] == "cond").map(|r| (r["col"].as_str().unwrap().to_string(), r["op"].as_str().unwrap().to_string())).collect();
    for (c, o) in [("name", "all_caps"), ("developers", "all_caps"), ("publishers", "contains_all"), ("developers", "contains_all"), ("name", "ends_with"), ("appid", "ends_with"), ("name", "not_contains"), ("tags", "not_contains"),
                   ("release_date", "year_is"), ("release_date", "weekday_is"), ("release_date", "nth_weekday"), ("release_date", "gte"), ("release_date", "lt"), ("total_achievements", "range_incl"), ("hltb_main", "range_incl")] { pairs.insert((c.into(), o.into())); }
    let mut conds: Vec<(String, String, String)> = golden["sql"].as_array().unwrap().iter().map(|c| (c["col"].as_str().unwrap().to_string(), c["op"].as_str().unwrap().to_string(), c["val"].as_str().unwrap().to_string()))
        .filter(|(c, o, _)| pairs.contains(&(c.clone(), o.clone()))).collect();
    for t in TAGS { conds.push(("tags".into(), "contains".into(), t.into())); }
    for d in DEVS { conds.push(("developers".into(), "contains".into(), d.into())); conds.push(("publishers".into(), "contains_all".into(), "RUOK".into())); }
    for w in WORDS { conds.push(("name".into(), "title_word".into(), w.into())); conds.push(("name".into(), "contains".into(), w.into())); conds.push(("name".into(), "starts_with".into(), w.into())); }
    for t in TIERS { conds.push(("review_score".into(), "equals".into(), t.into())); }
    for y in 2007..=2011 { conds.push(("release_date".into(), "year_is".into(), y.to_string())); }
    for (col, o, v) in &conds {
        let Some((sql, ps)) = cond_sql(col, o, v) else { continue };
        let mut st = db.prepare(&format!("SELECT appid FROM games WHERE {sql}")).unwrap();
        let hits: std::collections::HashSet<i64> = st.query_map(rusqlite::params_from_iter(to_sql(&ps)), |r| r.get(0)).unwrap().flatten().collect();
        for g in &games {
            let (sql_says, rust_says) = (hits.contains(&g.appid), cond_matches(col, o, v, g));
            checked += 1;
            let e = by_op.entry(o.clone()).or_default(); e.0 += 1; if sql_says { e.1 += 1; matched += 1; }
            if sql_says != rust_says && bad.len() < 12 { bad.push(format!("{col} {o} {v:?} on {:?} (release {:?}): sql {sql_says}, verdict {rust_says}", g.name, g.release_date)); }
        }
    }
    assert!(bad.is_empty(), "{} checks, {matched} selected; differences:\n{}", checked, bad.join("\n"));
    // coverage: every operator selected something, so the agreement is not "nothing matches either way"
    let silent: Vec<_> = by_op.iter().filter(|(_, v)| v.1 == 0).map(|(k, _)| k.clone()).collect();
    assert!(silent.is_empty(), "operators that never selected a random game: {silent:?}");
}

fn spec_for_tree() -> Spec {
    Spec {
        event_id: Some(90), event_name: "Test".into(), sg_group: Some("Won on SteamGifts".into()), statuses: vec![],
        wins: PoolSpec { tags: vec!["Roguelike".into(), "RPG".into()], conds: vec![("name".into(), "starts_with".into(), "Q".into())] },
        all: PoolSpec { tags: vec!["Puzzle".into()], conds: vec![("hltb_main".into(), "lt".into(), "120".into()), ("release_date".into(), "month_is".into(), "3".into())] },
        personal: vec![],
        lists: vec![
            List { label: "Verified A".into(), pool: Pool::Wins, auto: false, santa: false, groups: vec![Group { verifiers: vec!["alice".into()], year: None, ids: vec![10037, 10074] }] },
            List { label: "Verified B".into(), pool: Pool::All, auto: false, santa: false, groups: vec![Group { verifiers: vec![], year: None, ids: vec![10111] }] },
            List { label: SANTA_LABEL.into(), pool: Pool::Wins, auto: true, santa: true, groups: vec![Group { verifiers: vec![], year: Some(2021), ids: vec![10148, 10185] }] },
        ],
    }
}

#[test]
fn every_game_the_tree_lists_has_a_reason_in_the_verdict() {
    let mut r = Rng(42);
    let games: Vec<PagGame> = (1..=600).map(|i| random_game(&mut r, 10_000 + i * 37)).collect();
    let db = db_with(&games);
    let spec = spec_for_tree();
    let mut params = vec![];
    let sql = build_tree_sql(&expand_tree(&spec), &mut params, &Ctx::default());
    let ps: Vec<Sql> = params.iter().map(|p| match p { Value::String(s) => Sql::Text(s.clone()), Value::Number(n) => Sql::Integer(n.as_i64().unwrap()), o => panic!("{o}") }).collect();
    let mut st = db.prepare(&format!("SELECT appid FROM games WHERE {sql}")).unwrap();
    let hits: std::collections::HashSet<i64> = st.query_map(rusqlite::params_from_iter(ps), |r| r.get(0)).unwrap().flatten().collect();
    assert!(hits.len() > 20, "the filter should list a fair number of games, got {}", hits.len());
    let (mut listed_without_reason, mut reason_without_listing_non_santa) = (vec![], vec![]);
    for g in &games {
        let q = verdict(&spec, g);
        if hits.contains(&g.appid) && q.is_empty() { listed_without_reason.push(g.appid); }
        // a Santa-year line may appear for a gift the criteria do not otherwise list; everything else a verdict names must be listed
        if !hits.contains(&g.appid) && q.iter().any(|x| !x.label.starts_with("(santa/snowball,")) { reason_without_listing_non_santa.push((g.appid, q.iter().map(|x| format!("{} {}", x.desc, x.label)).collect::<Vec<_>>())); }
    }
    assert!(listed_without_reason.is_empty(), "listed with no reason: {listed_without_reason:?}");
    assert!(reason_without_listing_non_santa.is_empty(), "explained but not listed: {:?}", &reason_without_listing_non_santa[..reason_without_listing_non_santa.len().min(5)]);
}

#[test]
fn spec_round_trips_and_sits_inside_a_saved_tree() {
    let s = spec_for_tree();
    let back = Spec::from_json(&s.to_json());
    assert_eq!(back, s);
    let tree = s.to_tree();
    assert_eq!(tree["pagywosg"], json!(true));
    assert_eq!(Spec::from_tree(&tree).unwrap(), s);
    // ids are stored once, in the spec; the saved tree holds no copy
    assert!(!serde_json::to_string(&tree).unwrap().contains("appid_list"));
}

fn game(appid: i64, name: &str, tags: &str, groups: &str) -> PagGame { PagGame { appid, name: name.into(), tags: tags.into(), groups: groups.into(), ..Default::default() } }

#[test]
fn wins_criteria_only_count_for_won_games_or_santa_gifts() {
    let s = spec_for_tree();
    let won = verdict(&s, &game(1, "Quest Game", "Roguelike", "Won on SteamGifts"));
    let descs: Vec<_> = won.iter().map(|q| (q.desc.as_str(), q.label.as_str())).collect();
    assert_eq!(descs, vec![("Tags includes Roguelike", "(win)"), ("Title starts with \"Q\"", "(win)")]);
    // not won: the wins-pool criteria are ignored, the all-games ones show as backlog
    let not_won = verdict(&s, &game(2, "Quest Game", "Roguelike,Puzzle", "Favs"));
    assert_eq!(not_won.iter().map(|q| (q.desc.as_str(), q.label.as_str())).collect::<Vec<_>>(), vec![("Tags includes Puzzle", "(backlog)")]);
    // a Santa gift counts as a win: wins criteria apply, labelled as santa/snowball, plus its own year line
    let gift = verdict(&s, &game(10148, "Quest Game", "Roguelike", ""));
    assert!(gift.iter().any(|q| q.label == "(santa/snowball)" && q.desc == "Tags includes Roguelike"));
    assert!(gift.iter().any(|q| q.desc == SANTA_LABEL && q.label == "(santa/snowball, 2021)" && !q.verified));
}

#[test]
fn verified_lists_show_who_verified_and_the_dlc_star() {
    let s = spec_for_tree();
    let mut g = game(10037, "Plain", "", "Won on SteamGifts");
    g.sg_dlc_win = true;
    let q = verdict(&s, &g);
    assert_eq!(q.len(), 1);
    assert_eq!((q[0].desc.as_str(), q[0].label.as_str(), q[0].verified, q[0].verifiers.clone()), ("Verified A", "(win)*", true, vec!["alice".to_string()]));
    assert!(needs_dlc_note(&q));
    // verified for the wins pool but not won: nothing
    assert!(verdict(&s, &game(10037, "Plain", "", "")).is_empty());
    // all-pool list: (backlog) unless the game is won
    assert_eq!(verdict(&s, &game(10111, "Plain", "", ""))[0].label, "(backlog)");
    assert_eq!(verdict(&s, &game(10111, "Plain", "", "Won on SteamGifts"))[0].label, "(win)");
}

#[test]
fn without_a_group_only_santa_gifts_can_be_wins() {
    let mut s = spec_for_tree();
    s.sg_group = None;
    assert!(verdict(&s, &game(1, "Quest", "Roguelike", "Won on SteamGifts")).is_empty());
    assert!(!verdict(&s, &game(10185, "Quest", "Roguelike", "")).is_empty());
    // the tree still builds: wins block needs the Santa qualifier alone
    let mut p = vec![];
    let sql = build_tree_sql(&expand_tree(&s), &mut p, &Ctx::default());
    assert!(sql.contains("appid IN (10148,10185)"), "{sql}");
}

#[test]
fn building_a_spec_from_an_event_groups_ids_and_applies_personal_lists() {
    use pd_filter::pagywosg_event::{Auto, CatEntry, GameRow};
    let mut auto = Auto { event_id: 90, event_name: "E".into(), cat_order: vec!["Cat B".into(), "Cat A".into()], ..Default::default() };
    let ce = |cat: &str, v: &[&str]| CatEntry { cat: cat.into(), auto: false, verifiers: Some(v.iter().map(|s| s.to_string()).collect()) };
    let row = |a: i64, cats: Vec<CatEntry>| GameRow { appid: a, name: format!("G{a}"), categories: cats, in_library: true, redundant: false };
    auto.wins.games = vec![row(5, vec![ce("Cat B", &["Alice"]), ce("Cat A", &["Bob"])]), row(6, vec![ce("Cat B", &["Alice"])]), row(7, vec![ce("Cat B", &["Bob"])])];
    auto.wins.games.push(GameRow { in_library: false, ..row(8, vec![ce("Cat B", &["Alice"])]) });
    let mut sel = Selection::default();
    sel.personal.insert("Cat B".into());
    let spec = spec_from_auto(&auto, &sel, "alice");
    // lists follow the event's category order; the personal list keeps only entries verified for the user; an unowned game stays in (a game bought or won later matches without rebuilding)
    assert_eq!(spec.lists.iter().map(|l| l.label.as_str()).collect::<Vec<_>>(), vec!["Cat B", "Cat A"]);
    assert_eq!(spec.lists[0].all_ids(), vec![5, 6, 8]);
    assert_eq!(spec.lists[1].all_ids(), vec![5]);
    // identical verifier sets share one group (ids stored once)
    assert_eq!(spec.lists[0].groups.len(), 1);
    sel.santa = Some(vec![(50, Some(2020)), (51, Some(2020)), (52, None)]);
    let spec = spec_from_auto(&auto, &sel, "alice");
    let santa = spec.lists.last().unwrap();
    assert!(santa.santa && santa.pool == Pool::Wins && santa.groups.len() == 2);
}
