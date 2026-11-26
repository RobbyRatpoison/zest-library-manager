//! `native_exe` against the real `runners/native_exe.py` (`gen_native_exe_golden.py` records what it finds in each folder tree; this rebuilds the same trees).
use pd_filter::native_exe::*;
use serde_json::{json, Value};

#[test] fn scan_and_pick_match_python_on_every_tree_and_os() {
    let g: Value = serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/native_exe_golden.json")).expect("run gen_native_exe_golden.py")).unwrap();
    for c in g.as_array().unwrap() {
        let dir = std::env::temp_dir().join(format!("zest_ne_{}_{}", c["name"].as_str().unwrap(), std::process::id())); let _ = std::fs::remove_dir_all(&dir); std::fs::create_dir_all(&dir).unwrap();
        for (rel, bytes) in c["files"].as_object().unwrap() {
            let p = dir.join(rel); std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, bytes.as_array().unwrap().iter().map(|b| b.as_u64().unwrap() as u8).collect::<Vec<u8>>()).unwrap();
        }
        for (name, os) in [("linux", Os::Linux), ("windows", Os::Windows), ("mac", Os::Mac)] {
            let want = &c["out"][name];
            let s = scan_candidates(&dir, os);
            let sorted = |mut v: Vec<String>| { v.sort(); json!(v) };
            let mut nat: Vec<_> = s.natives.iter().map(|(r, d, p)| json!([r, d, p])).collect(); nat.sort_by_key(|v| v.to_string());
            let mut wantn = want["scan"]["natives"].as_array().unwrap().clone(); wantn.sort_by_key(|v| v.to_string());
            let mut win: Vec<_> = s.winexes.iter().map(|(d, p)| json!([d, p])).collect(); win.sort_by_key(|v| v.to_string());
            let mut wantw = want["scan"]["winexes"].as_array().unwrap().clone(); wantw.sort_by_key(|v| v.to_string());
            assert_eq!(sorted(s.appimages.clone()), want["scan"]["appimages"], "{} {name} appimages", c["name"]);
            assert_eq!(sorted(s.scripts.clone()), want["scan"]["scripts"], "{} {name} scripts", c["name"]);
            assert_eq!(nat, wantn, "{} {name} natives", c["name"]);
            assert_eq!(win, wantw, "{} {name} winexes", c["name"]);
            let p = pick_executable(&dir, os);
            let mut cands = p.candidates.clone(); cands.sort();
            let mut wantc: Vec<String> = want["pick"]["candidates"].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect(); wantc.sort();
            assert_eq!(p.path.as_deref(), want["pick"]["path"].as_str(), "{} {name} path", c["name"]);
            assert_eq!((p.is_windows, p.ambiguous, cands), (want["pick"]["is_windows"].as_bool().unwrap(), want["pick"]["ambiguous"].as_bool().unwrap(), wantc), "{} {name} flags", c["name"]);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
