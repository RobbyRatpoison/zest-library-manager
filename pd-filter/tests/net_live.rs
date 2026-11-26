//! Live smoke test (network, keyless): `cargo test -p pd-filter --test net_live -- --ignored --nocapture`
use pd_filter::net::UreqHttp;
use pd_filter::steamclient::{AppinfoDates, Steam};
use std::sync::Arc;

#[test] #[ignore]
fn keyless_endpoints_answer() {
    let s = Steam::new(Arc::new(UreqHttp::new()), None, 10.0);
    let st = s.store_data(620, AppinfoDates::default()).unwrap().expect("store data");
    println!("store: {} / {} / {:?}", st.data.name, st.data.developers, st.release_date);
    assert_eq!(st.data.name, "Portal 2");
    let r = s.reviews(620).unwrap().expect("reviews");
    println!("reviews: {} {}% of {}", r.review_score, r.review_percentage, r.total_reviews);
    assert!(r.total_reviews > 100_000);
    let t = s.tags(620).unwrap().expect("tags"); println!("tags: {:?}", &t[..t.len().min(5)]); assert!(t.iter().any(|x| x == "Puzzle"));
    println!("protondb: {:?}", s.protondb(620).unwrap());
    // an unreachable host must give an error text with no URL in it
    let e = UreqHttp::new(); use pd_filter::net::Http;
    let err = e.get("https://127.0.0.1:9/secret?key=SECRETKEY", &[], std::time::Duration::from_secs(2)).unwrap_err();
    println!("error text: {err}"); assert!(!err.0.contains("SECRETKEY") && !err.0.contains("127.0.0.1:9/"));
}

#[test] #[ignore]
fn bulk_rescrape_on_a_throwaway_db() {
    use pd_filter::jobs::{bulk_rescrape, Rescrape};
    let db = std::env::temp_dir().join(format!("zest_rescrape_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&db);
    let c = rusqlite::Connection::open(&db).unwrap();
    c.execute_batch("CREATE TABLE games (appid INTEGER PRIMARY KEY, name TEXT, platform TEXT, completion_status TEXT, total_achievements INTEGER, unlocked_achievements INTEGER, tags TEXT, meta_fetched TEXT, review_score TEXT, review_percentage INTEGER, weighted_percentage INTEGER, total_reviews INTEGER, positive_reviews INTEGER, developers TEXT, publishers TEXT, release_date INTEGER, genres TEXT, categories TEXT, is_free INTEGER, metacritic_score INTEGER, short_description TEXT, name_from_store INTEGER);
        INSERT INTO games (appid, name, platform) VALUES (620,'x','steam'),(-5,'custom','custom');").unwrap();
    let job = Rescrape { local_play: Default::default(), db: db.clone(), root: std::env::temp_dir(), steam: Arc::new(Steam::new(Arc::new(UreqHttp::new()), None, 10.0)), http: Arc::new(UreqHttp::new()), sources: Arc::new(pd_filter::sources::Registry::builtin()), appinfo: Arc::new(|_| AppinfoDates::default()), today: "2026-11-01".into(), account_id: None, meta: true, cheevo: true, phase: pd_filter::jobs::Phase::All };
    let counts = bulk_rescrape(job, vec![620, -5], pd_filter::net::Cancel::new(), Arc::new(|e| println!("{e:?}")));
    println!("{counts:?}");
    let r: (String, String, i64, String) = c.query_row("SELECT name, review_score, total_reviews, tags FROM games WHERE appid=620", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap();
    println!("{r:?}");
    assert_eq!((counts.done, counts.failed), (1, 1)); assert_eq!(r.0, "Portal 2"); assert!(r.2 > 100_000 && r.3.contains("Puzzle"));
    let _ = std::fs::remove_file(&db);
}

#[test] #[ignore]
fn hltb_answers() {
    let h = pd_filter::hltbclient::Hltb::new(Arc::new(UreqHttp::new()));
    let a = h.fetch_data("Portal 2", 75, "2026-11-01").expect("reachable").expect("found");
    println!("{a:?}");
    assert_eq!(a.matched_name, "Portal 2"); assert!(a.main.unwrap() > 300);
    let t0 = std::time::Instant::now();
    let b = h.fetch_data("Half-Life 2", 75, "d").unwrap().unwrap(); println!("second lookup {:?}: {b:?}", t0.elapsed());
    let by = h.fetch_by_id("Portal 2", a.id).unwrap(); println!("{by:?}"); assert!(by.times_available);
}

#[test] #[ignore]
fn resolver_finds_steam_ids() {
    use pd_filter::metadata::{resolve, Confidence, Pcgw};
    let http: Arc<dyn pd_filter::net::Http> = Arc::new(UreqHttp::new());
    let pcgw = Pcgw::new(http.clone());
    let (id, c, b) = resolve(http.as_ref(), &pcgw, "Hades");
    println!("Hades -> {id:?} {c:?} {:?}", b.map(|b| (b.title, b.developers)));
    assert_eq!((id, c), (Some(1145360), Confidence::Pcgw));
    let (id, c, b) = resolve(http.as_ref(), &pcgw, "Syndicate (1993)");
    println!("Syndicate (1993) -> {id:?} {c:?} {:?}", b.map(|b| (b.title, b.developers, b.release_date)));
}

/// `ZEST_LIVE_DB=<copy of a library> ZEST_LIVE_APPID=<id> cargo test ... hltb_job -- --ignored --nocapture`: the real HLTB job on a COPY of a library, timed.
#[test] #[ignore]
fn hltb_job_on_a_library_copy() {
    use pd_filter::jobs::bulk_hltb;
    let (Ok(db), Ok(id)) = (std::env::var("ZEST_LIVE_DB"), std::env::var("ZEST_LIVE_APPID")) else { return };
    let id: i64 = id.parse().unwrap();
    let http: Arc<dyn pd_filter::net::Http> = Arc::new(UreqHttp::new());
    let dir = std::env::temp_dir();
    let t0 = std::time::Instant::now();
    let map = Arc::new(pd_filter::hltbclient::steam_hltb_map(&dir, http.as_ref()));
    println!("map: {} entries in {:?}", map.len(), t0.elapsed());
    let hltb = Arc::new(pd_filter::hltbclient::Hltb::new(http));
    let c = bulk_hltb(std::path::PathBuf::from(db), hltb, map, 99, "2026-11-01".into(), vec![id], pd_filter::net::Cancel::new(), { let t1 = t0; Arc::new(move |e| println!("{:?} at {:?}", e, t1.elapsed())) });
    println!("{c:?} in {:?}", t0.elapsed());
}

/// `ZEST_LIVE_DATA=<data folder> ZEST_LIVE_APPID=<id>`: the achievement request with the account's own key from that folder's `config.json`. Prints counts only.
#[test] #[ignore]
fn achievements_with_the_account_key() {
    use pd_filter::steamclient::{Creds, Steam};
    let (Ok(dir), Ok(id)) = (std::env::var("ZEST_LIVE_DATA"), std::env::var("ZEST_LIVE_APPID")) else { return };
    let a = pd_filter::account::load_account(std::path::Path::new(&dir)).expect("an account");
    let s = Steam::new(Arc::new(UreqHttp::new()), Some(Creds { api_key: a.api_key, steam_id: a.steam_id }), 10.0);
    assert!(s.has_key());
    let c = s.cheevos(id.parse().unwrap()).unwrap();
    println!("achievements: {c:?}");
    assert!(c.is_some());
}
