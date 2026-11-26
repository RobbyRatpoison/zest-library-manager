//! The Python tests for the Steam local files (`tests/test_parse_appinfo.py`, `tests/test_steam_userdata.py`) as the spec, on synthetic files,
//! plus the decisions of NATIVE_UI_PLAN.md section 14 (fully installed, tools, launch recording).
use pd_filter::steamlocal::*;
use rusqlite::Connection;
use std::path::PathBuf;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("zest-steam-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

// ---- appinfo.vdf builder (mirrors the Python test helpers)
const KEYS: [&str; 5] = ["appinfo", "common", "name", "type", "steam_release_date"];
fn kid(k: &str) -> u32 { KEYS.iter().position(|x| *x == k).unwrap() as u32 }
fn vdf_bytes(name: &str, kind: &str, release: Option<i32>) -> Vec<u8> {
    let mut b = vec![0u8]; b.extend(kid("appinfo").to_le_bytes());
    b.push(0); b.extend(kid("common").to_le_bytes());
    b.push(1); b.extend(kid("name").to_le_bytes()); b.extend(name.as_bytes()); b.push(0);
    b.push(1); b.extend(kid("type").to_le_bytes()); b.extend(kind.as_bytes()); b.push(0);
    if let Some(r) = release { b.push(2); b.extend(kid("steam_release_date").to_le_bytes()); b.extend(r.to_le_bytes()); }
    b.extend([8, 8, 8]);
    b
}
fn record(appid: u32, vdf: &[u8]) -> Vec<u8> {
    let mut r = appid.to_le_bytes().to_vec();
    r.extend(((60 + vdf.len()) as u32).to_le_bytes());
    r.extend(vec![1u8; 60]);
    r.extend(vdf);
    r
}
fn file(records: &[Vec<u8>]) -> Vec<u8> {
    let mut body: Vec<u8> = records.concat();
    body.extend([0u8; 8]);
    let kt = 16 + body.len() as u32;
    let mut out = Vec::new();
    for v in [0x0756_4429u32, 1, kt, 0] { out.extend(v.to_le_bytes()); }
    out.extend(body);
    out.extend(b"BT\0\0");
    for k in KEYS { out.extend(k.as_bytes()); out.push(0); }
    out
}

#[test]
fn appinfo_names_types_and_release_dates() {
    let r = parse_appinfo(&file(&[record(440, &vdf_bytes("Half-Life", "game", Some(1191988800))), record(1000, &vdf_bytes("Some Soundtrack", "music", None))]));
    assert_eq!((r[&440].name.as_str(), r[&440].kind.as_str(), r[&440].steam_release_date), ("Half-Life", "game", Some(1191988800)));
    assert_eq!((r[&1000].name.as_str(), r[&1000].kind.as_str(), r[&1000].steam_release_date), ("Some Soundtrack", "music", None));
}

#[test]
fn appinfo_bad_files_give_nothing_and_a_corrupt_record_does_not_break_the_rest() {
    let good = record(440, &vdf_bytes("Half-Life", "game", None));
    let mut wrong = file(&[good.clone()]); wrong[..4].copy_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
    assert!(parse_appinfo(&wrong).is_empty());
    assert!(parse_appinfo(&0x0756_4429u32.to_le_bytes()).is_empty());
    assert!(parse_appinfo(&[]).is_empty());
    let r = parse_appinfo(&file(&[record(666, &[0xff; 30]), good]));
    assert!(r.contains_key(&440) && !r.contains_key(&666));
}

#[test]
fn appinfo_reads_past_value_types_it_does_not_keep() {
    // a float, a wide string and a 64-bit number before `name` must not stop the block (the Python gave up at the first unknown type)
    let mut keys: Vec<&str> = KEYS.to_vec(); keys.extend(["f", "w", "big"]);
    let k = |n: &str| keys.iter().position(|x| *x == n).unwrap() as u32;
    let mut v = vec![0u8]; v.extend(k("appinfo").to_le_bytes());
    v.push(0); v.extend(k("common").to_le_bytes());
    v.push(3); v.extend(k("f").to_le_bytes()); v.extend(1.5f32.to_le_bytes());
    v.push(5); v.extend(k("w").to_le_bytes()); v.extend([b'h', 0, b'i', 0, 0, 0]);
    v.push(7); v.extend(k("big").to_le_bytes()); v.extend(7u64.to_le_bytes());
    v.push(1); v.extend(k("name").to_le_bytes()); v.extend(b"After Others\0");
    v.extend([8, 8, 8]);
    let mut out = Vec::new();
    let body = [record(7, &v), vec![0u8; 8]].concat();
    for x in [0x0756_4429u32, 1, 16 + body.len() as u32, 0] { out.extend(x.to_le_bytes()); }
    out.extend(body); out.extend(b"BT\0\0"); for kk in &keys { out.extend(kk.as_bytes()); out.push(0); }
    assert_eq!(parse_appinfo(&out)[&7].name, "After Others");
}

// ---- accounts
const ID64: u64 = 76561198020831227;
const ID3: &str = "60565499";
const OTHER3: &str = "216730178";

fn write_account(root: &PathBuf, id3: &str, apps: Option<&str>, collections: Option<&str>) {
    let cfg = root.join("userdata").join(id3).join("config");
    std::fs::create_dir_all(&cfg).unwrap();
    if let Some(a) = apps { std::fs::write(cfg.join("localconfig.vdf"), format!("\"UserLocalConfigStore\"\n{{\n \"Software\" {{ \"Valve\" {{ \"Steam\" {{ \"apps\" {{ {a} }} }} }} }}\n}}\n")).unwrap(); }
    if let Some(c) = collections { std::fs::create_dir_all(cfg.join("cloudstorage")).unwrap(); std::fs::write(cfg.join("cloudstorage/cloud-storage-namespace-1.json"), c).unwrap(); }
}
fn coll(id: &str, name: &str, added: &[i64]) -> String {
    let inner = serde_json::json!({"id": id, "name": name, "added": added}).to_string();
    serde_json::json!([format!("user-collections.{id}"), {"is_deleted": false, "value": inner}]).to_string()
}

#[test]
fn the_configured_account_wins_and_a_missing_folder_never_falls_back_to_another_account() {
    let root = tmp("acct");
    write_account(&root, "0", Some(""), None);
    write_account(&root, ID3, Some("\"440\" { \"Playtime\" \"120\" \"LastPlayed\" \"1700000000\" }"), Some(&format!("[{}]", coll("fav", "Favorites", &[440, 10]))));
    write_account(&root, OTHER3, Some("\"570\" { \"Playtime\" \"999\" }"), None);
    let id = ID64.to_string();
    assert_eq!(localconfig_path(&root, Some(&id)).unwrap(), root.join("userdata").join(ID3).join("config/localconfig.vdf"));
    assert_eq!(local_library(&root, Some(&id)), vec![LocalApp { appid: 440, playtime_forever: 120, last_played: Some(1700000000) }]);
    let c = collections(&root, Some(&id));
    assert_eq!(c["fav"], Collection { name: "Favorites".into(), added: vec![440, 10] });
    // the configured account's folder is missing: nothing, not another account's data
    let root2 = tmp("acct2");
    write_account(&root2, "0", Some(""), None);
    write_account(&root2, OTHER3, Some("\"570\" { \"Playtime\" \"999\" }"), Some(&format!("[{}]", coll("wl", "Wishlist", &[570]))));
    assert!(localconfig_path(&root2, Some(&id)).is_none());
    assert!(local_library(&root2, Some(&id)).is_empty());
    assert!(collections(&root2, Some(&id)).is_empty());
    // no id: the first account folder that has the file
    assert!(local_library(&root2, None).iter().any(|a| a.appid == 570));
}

#[test]
fn collections_skip_deleted_hidden_and_unnamed_ones() {
    let root = tmp("coll");
    let del = serde_json::json!(["user-collections.x", {"is_deleted": true, "value": serde_json::json!({"id": "x", "name": "Gone", "added": [1]}).to_string()}]).to_string();
    write_account(&root, ID3, None, Some(&format!("[{}, {}, {}, {}]", coll("hidden", "Hidden", &[1]), del, coll("a", "  ", &[1]), coll("ok", " Keep ", &[5]))));
    let c = collections(&root, Some(&ID64.to_string()));
    assert_eq!(c.keys().collect::<Vec<_>>(), vec!["ok"]);
    assert_eq!(c["ok"].name, "Keep");
}

#[test]
fn accounts_come_from_userdata_named_by_loginusers_newest_first() {
    let root = tmp("names");
    write_account(&root, ID3, Some(""), None);
    std::fs::create_dir_all(root.join("config")).unwrap();
    std::fs::write(root.join("config/loginusers.vdf"), format!("\"users\" {{ \"{ID64}\" {{ \"AccountName\" \"acct\" \"PersonaName\" \"Rob\" }} }}")).unwrap();
    let a = accounts(&root);
    assert_eq!(a, vec![Account { steam_id: ID64.to_string(), name: "Rob".into() }]);
}

/// A path as Steam writes it into a .vdf: backslashes doubled (matters on Windows only).
fn vdf_path(p: &std::path::Path) -> String { p.display().to_string().replace('\\', "\\\\") }

// ---- manifests, installed, tools
fn acf(root: &PathBuf, dir: &str, id: i64, name: &str, flags: u32, installdir: &str, user_config: bool) {
    let d = root.join(dir); std::fs::create_dir_all(&d).unwrap();
    let uc = if user_config { "\"UserConfig\" { \"language\" \"english\" }" } else { "" };
    std::fs::write(d.join(format!("appmanifest_{id}.acf")), format!("\"AppState\"\n{{\n\t\"appid\"\t\t\"{id}\"\n\t\"name\"\t\t\"{name}\"\n\t\"StateFlags\"\t\t\"{flags}\"\n\t\"installdir\"\t\t\"{installdir}\"\n\t{uc}\n}}\n")).unwrap();
}

#[test]
fn only_fully_installed_manifests_count_as_installed_in_every_library_folder() {
    let root = tmp("acf");
    acf(&root, "steamapps", 1, "Done", 4, "Done", true);
    acf(&root, "steamapps", 2, "Updating", 6, "Updating", true);          // installed, update pending: still installed
    acf(&root, "steamapps", 3, "Downloading", 1026, "Downloading", true); // not yet
    let extra = root.join("elsewhere");
    acf(&extra, "steamapps", 4, "On another drive", 4, "Other", true);
    std::fs::create_dir_all(root.join("config")).unwrap();
    std::fs::write(root.join("config/libraryfolders.vdf"), format!("\"libraryfolders\" {{ \"0\" {{ \"path\" \"{}\" }} \"1\" {{ \"path\" \"{}\" }} }}", vdf_path(&root), vdf_path(&extra))).unwrap();
    let m = manifests(&root);
    let mut ids = installed_appids(&m); ids.sort();
    assert_eq!(ids, vec![1, 2, 4]);
    assert_eq!(m.iter().find(|x| x.appid == 3).unwrap().name, "Downloading");
}

#[test]
fn tools_are_kept_out_when_adding_games_not_when_checking_installed() {
    let root = tmp("tools");
    acf(&root, "steamapps", 10, "Proton 9.0", 4, "Proton 9.0", true);
    acf(&root, "steamapps", 11, "Proton Pulse", 4, "ProtonPulse", true);          // a real game whose name contains the word
    acf(&root, "steamapps", 12, "Mystery Tool", 4, "tool", false);
    let m = manifests(&root);
    assert_eq!(installed_appids(&m).len(), 3, "the installed check has no exclusion words");
    let by = |id: i64| m.iter().find(|x| x.appid == id).unwrap();
    // with appinfo's type the answer is exact
    assert!(!is_game_to_add(by(10), Some("tool")));
    assert!(is_game_to_add(by(11), Some("game")));
    // without it: the fallback looks at name and installdir only and needs a UserConfig
    assert!(!is_game_to_add(by(10), None));
    assert!(!is_game_to_add(by(12), None));
}

// ---- database
fn games_db() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    c.execute("CREATE TABLE games (appid INTEGER PRIMARY KEY, name TEXT, platform TEXT, installed INTEGER, completion_status TEXT, last_played INTEGER)", []).unwrap();
    c.execute("INSERT INTO games VALUES (1,'A','steam',1,'Never Played',NULL),(2,'B','steam',1,'Beaten',100),(3,'C','steam',0,'Never Played',NULL),(-5,'D','gog',1,'Never Played',NULL)", []).unwrap();
    c
}

#[test]
fn the_install_sync_resets_steam_rows_only_and_sets_the_found_ones() {
    let c = games_db();
    assert_eq!(sync_installed(&c, &[3]).unwrap(), 1);
    let st: Vec<(i64, i64)> = c.prepare("SELECT appid, installed FROM games ORDER BY appid").unwrap().query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().flatten().collect();
    assert_eq!(st, vec![(-5, 1), (1, 0), (2, 0), (3, 1)]);   // the GOG row is not touched
}

#[test]
fn a_launch_is_recorded_for_an_installed_game_only() {
    let c = games_db();
    assert_eq!(record_launch(&c, 1, 5000).unwrap(), Some(5000));
    let row = |id: i64| c.query_row("SELECT last_played, completion_status FROM games WHERE appid = ?", [id], |r| Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, String>(1)?))).unwrap();
    assert_eq!(row(1), (Some(5000), "Unfinished".into()));                  // Never Played becomes Unfinished
    assert_eq!(record_launch(&c, 2, 6000).unwrap(), Some(6000));
    assert_eq!(row(2), (Some(6000), "Beaten".into()));                      // any other status stays
    assert_eq!(record_launch(&c, 3, 7000).unwrap(), None);                  // not installed: starting the install prompt is not a launch
    assert_eq!(row(3), (None, "Never Played".into()));
    assert_eq!(record_launch(&c, 999, 1).unwrap(), None);
}

#[test]
fn text_vdf_handles_escapes_comments_and_repeated_keys() {
    let v = parse_vdf("// c\n\"a\" { \"k\" \"x\\\"y\\\\z\" \"k\" \"last\" \"n\" { \"i\" \"1\" } }\n").unwrap();
    assert_eq!(v.path(&["a", "k"]).and_then(|x| x.as_str()), Some("last"));
    assert_eq!(v.path(&["A", "N", "I"]).and_then(|x| x.as_str()), Some("1"));
    assert!(parse_vdf("\"a\" { \"k\" ").is_none());
}

/// Real Steam folder against the Python (needs ZEST_STEAM_REAL = the json from `gen_steam_real.py`, read-only on both sides): `--ignored --nocapture`.
#[test]
#[ignore]
fn real_steam_folder_against_python() {
    let Ok(p) = std::env::var("ZEST_STEAM_REAL") else { return };
    let g: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
    let root = PathBuf::from(g["root"].as_str().unwrap());
    let sid = g["steam_id"].as_str();
    // appinfo
    let mine = read_appinfo(&root);
    let py = g["appinfo"].as_object().unwrap();
    let (mut same, mut diff, mut only_py) = (0, Vec::new(), 0);
    for (k, v) in py {
        match mine.get(&k.parse::<i64>().unwrap()) {
            None => only_py += 1,
            Some(m) => {
                let ok = m.name == v["name"].as_str().unwrap_or("") && m.kind == v["type"].as_str().unwrap_or("")
                    && m.steam_release_date == v["steam_release_date"].as_i64() && m.original_release_date == v["original_release_date"].as_i64();
                if ok { same += 1 } else if diff.len() < 5 { diff.push(format!("{k}: rust {m:?} python {v}")); } else { diff.push(String::new()); }
            }
        }
    }
    println!("appinfo: python {} rust {} identical {same} different {} only python {only_py} only rust {}", py.len(), mine.len(), diff.len(), mine.len().saturating_sub(py.len() - only_py));
    for d in diff.iter().filter(|d| !d.is_empty()) { println!("   {d}"); }
    // playtime / last played
    let local = local_library(&root, sid);
    let pl: Vec<(i64, i64, Option<i64>)> = g["local"].as_array().unwrap().iter().map(|a| (a["appid"].as_i64().unwrap(), a["playtime_forever"].as_i64().unwrap(), a["last_played"].as_i64())).collect();
    let mut a: Vec<_> = local.iter().map(|l| (l.appid, l.playtime_forever, l.last_played)).collect(); a.sort();
    let mut b = pl.clone(); b.sort();
    println!("localconfig: python {} rust {} identical {}", b.len(), a.len(), a == b);
    // collections
    let c = collections(&root, sid);
    let pc = g["collections"].as_object().unwrap();
    let same_c = c.len() == pc.len() && c.iter().all(|(k, v)| pc.get(k).map_or(false, |p| p["name"] == v.name.as_str() && p["added"].as_array().map(|x| x.iter().filter_map(|n| n.as_i64()).collect::<Vec<_>>()) == Some(v.added.clone())));
    println!("collections: python {} rust {} identical {same_c}", pc.len(), c.len());
    // manifests: Python's old rule (UserConfig, no tool word anywhere) against the new rules
    let m = manifests(&root);
    let old_rule = |x: &Manifest| x.has_user_config;   // the word list is applied by name/installdir in the new fallback; compare both views
    let py_installed: std::collections::BTreeSet<i64> = g["installed"].as_array().unwrap().iter().map(|v| v.as_i64().unwrap()).collect();
    let fully: std::collections::BTreeSet<i64> = installed_appids(&m).into_iter().collect();
    let games: std::collections::BTreeSet<i64> = m.iter().filter(|x| x.fully_installed() && is_game_to_add(x, mine.get(&x.appid).map(|i| i.kind.as_str()))).map(|x| x.appid).collect();
    println!("manifests {} | python installed (real game rule) {} | rust fully installed {} | rust fully installed and a game to add {} | with UserConfig {}", m.len(), py_installed.len(), fully.len(), games.len(), m.iter().filter(|x| old_rule(x)).count());
    println!("  python-only {:?} | rust game set differs from python by: only rust {:?}", py_installed.difference(&games).collect::<Vec<_>>(), games.difference(&py_installed).collect::<Vec<_>>());
    for x in m.iter().filter(|x| fully.contains(&x.appid) && !games.contains(&x.appid)) { println!("  fully installed but not a game to add: {} {:?} type {:?}", x.appid, x.name, mine.get(&x.appid).map(|i| i.kind.clone())); }
    let names: std::collections::BTreeMap<String, String> = g["acf_names"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string())).collect();
    let mine_names: std::collections::BTreeMap<String, String> = m.iter().filter(|x| games.contains(&x.appid)).map(|x| (x.appid.to_string(), x.name.clone())).collect();
    println!("names of games: python {} rust {} identical {}", names.len(), mine_names.len(), names == mine_names);
}
