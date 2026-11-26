//! Test harness for the Zest Companion userscript. Starts the real listener against a THROWAWAY config folder (never the real
//! one) with the real Humble / Nintendo / PlayStation sign-ins, prints the four addresses to open, and reports what arrives.
//!   cargo run -p pd-filter --bin companion_dev [config-dir]      (default: $TMPDIR/zest-companion-test)
//! The sign-ins talk to the real services, so a captured cookie is genuinely tested; it is saved only in the throwaway folder.
use pd_filter::companion::{Event, Listener, SERVICES};
use pd_filter::sources::Registry;
use std::sync::Arc;

fn main() {
    let dir = std::env::args().nth(1).map(std::path::PathBuf::from).unwrap_or_else(|| std::env::temp_dir().join("zest-companion-test"));
    std::fs::create_dir_all(&dir).expect("could not create the test folder");
    // A small library in the throwaway folder so imports are really saved (and a second run shows what gets skipped).
    let db_path = dir.join("games.db");
    {
        let c = rusqlite::Connection::open(&db_path).expect("could not open the test library");
        c.execute_batch("CREATE TABLE IF NOT EXISTS games (appid INTEGER PRIMARY KEY, name TEXT, platform TEXT, platform_id TEXT, platform_slug TEXT, platform_appname TEXT, platform_ns TEXT, date_added INTEGER, completion_status TEXT, installed INTEGER, install_path TEXT, playtime_forever INTEGER, last_played INTEGER,
            release_date INTEGER, developers TEXT, publishers TEXT, genres TEXT, categories TEXT, tags TEXT, short_description TEXT, art_fetched TEXT, meta_fetched TEXT, cheevos_fetched TEXT, protondb_fetched TEXT, hltb_fetched TEXT);
            CREATE TABLE IF NOT EXISTS blacklist (appid INTEGER PRIMARY KEY, name TEXT, date_blacklisted TEXT, platform_id TEXT, platform TEXT);").expect("could not prepare the test library");
    }
    let mut reg = Registry::builtin();
    reg.register_builtins(&pd_filter::sources::SourceEnv { http: Arc::new(pd_filter::net::UreqHttp::new()), config_dir: dir.clone(), db: dir.join("games.db") });
    let save_to = db_path.clone();
    let l = Listener::start(Arc::new(reg), Arc::new(move |e| match e {
        Event::Connected { platform, username } => println!("CONNECTED  {platform} as {}", username.as_deref().unwrap_or("(no name)")),
        Event::Imported { platform, records } => {
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
            let saved = rusqlite::Connection::open(&save_to).ok().and_then(|mut c| pd_filter::importer::import(&mut c, &platform, &records, now).ok());
            println!("IMPORTED   {} {platform} games; earliest date {:?}; saved to the test library: {}", records.len(), records.iter().filter_map(|r| r.date_added).min(), if saved.is_some() { "yes" } else { "NO" });
            for r in records.iter().take(5) { println!("           {}  {}", r.platform_id, r.name); }
        }
        Event::SteamGifts { message, .. } => println!("STEAMGIFTS {message}"),
        Event::Refused { platform } => println!("REFUSED    {platform} did not accept the captured value"),
        Event::PurchaseDates { online_id, read, matched, set, .. } => println!("PURCHASES  {read} read from {online_id:?}, {matched} games matched, {set} dates set"),
    })).expect("could not start the listener");
    println!("Config folder (throwaway): {}\nListening on 127.0.0.1:{}. Open one of these in the browser that has the userscript:\n", dir.display(), l.port);
    for (s, _) in SERVICES { println!("  {s:10} {}", l.arm_url(s).unwrap()); }
    println!("\nCtrl-C to stop.");
    loop { std::thread::sleep(std::time::Duration::from_secs(3600)); }
}
