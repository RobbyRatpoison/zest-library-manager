//! Proton discovery against the real `runners/proton.py` (`gen_wine_golden.py` builds each fake home folder and records what the Python finds there).
#![cfg(unix)]   // Proton and Wine prefixes only exist on Linux; the expected paths use '/'
use pd_filter::wine::{find_proton_versions, SYSTEM_COMPAT_DIRS};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::PathBuf;

#[test] fn the_same_proton_builds_are_found_in_the_same_groups() {
    let g: Value = serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/wine_golden.json")).expect("run gen_wine_golden.py")).unwrap();
    for c in g.as_array().unwrap() {
        let home = std::env::temp_dir().join(format!("zest_wg_{}_{}", c["name"].as_str().unwrap(), std::process::id())); let _ = std::fs::remove_dir_all(&home); std::fs::create_dir_all(&home).unwrap();
        let t = &c["tree"];
        let names = |k: &str| -> Vec<String> { t.get(k).and_then(|v| v.as_array()).map(|a| a.iter().map(|x| x.as_str().unwrap().to_string()).collect()).unwrap_or_default() };
        let mk = |base: PathBuf, list: Vec<String>, skip: &[String]| for n in list { let d = base.join(&n); std::fs::create_dir_all(&d).unwrap(); if !skip.contains(&n) { std::fs::write(d.join("proton"), "#!/bin/sh\n").unwrap(); } };
        let steam = home.join(".local/share/Steam");
        mk(steam.join("compatibilitytools.d"), names("compat"), &names("noscript")); mk(steam.join("steamapps/common"), names("official"), &[]);
        if t.get("symlink_steam").is_some() { std::fs::create_dir_all(home.join(".steam")).unwrap(); #[cfg(unix)] std::os::unix::fs::symlink(&steam, home.join(".steam/steam")).unwrap(); }
        if t.get("library").is_some() { let lib = home.join("games-lib"); mk(lib.join("steamapps/common"), names("library"), &[]); std::fs::create_dir_all(steam.join("config")).unwrap();
            std::fs::write(steam.join("config/libraryfolders.vdf"), format!("\"libraryfolders\"\n{{\n \"1\"\n {{\n  \"path\"\t\t\"{}\"\n }}\n}}\n", lib.display())).unwrap(); }
        mk(home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam/compatibilitytools.d"), names("flatpak"), &[]);
        let mine = find_proton_versions(&home, SYSTEM_COMPAT_DIRS);
        let want: Vec<(String, String)> = c["found"].as_array().unwrap().iter().map(|f| (f["name"].as_str().unwrap().to_string(), f["rel"].as_str().unwrap().to_string())).collect();
        // the same builds at the same places, inside the fake home only: the machine's own /usr/share builds differ between the machine the golden was made on and this one
        let set = |v: Vec<(String, String)>| v.into_iter().filter(|(_, r)| !r.contains("usr/")).collect::<BTreeSet<_>>();
        let got: Vec<(String, String)> = mine.iter().map(|p| (p.name.clone(), p.path.strip_prefix(&home).map(|r| r.to_string_lossy().to_string()).unwrap_or_else(|_| format!("..{}", p.path.display())))).collect();
        // a build outside the home folder shows as `../../usr/share/...` in the Python's relative path and as the absolute path here: compare from `usr/`
        let norm = |v: Vec<(String, String)>| set(v.into_iter().map(|(n, r)| (n, r.find("usr/").map_or(r.clone(), |i| r[i..].to_string()))).collect());
        assert_eq!(norm(got.clone()), norm(want.clone()), "{}", c["name"]);
        // GE-Proton first, then other community builds, then official: the Python's group order; inside a group the newest comes first (by number, which the Python's letter order got wrong)
        let group = |n: &str, p: &str| if n.to_lowercase().starts_with("ge-proton") { 0 } else if p.contains("compatibilitytools.d") { 1 } else { 2 };
        let groups: Vec<i32> = got.iter().map(|(n, p)| group(n, p)).collect();
        assert!(groups.windows(2).all(|w| w[0] <= w[1]), "{} {:?}", c["name"], got);
        let ge: Vec<&str> = got.iter().filter(|(n, _)| n.starts_with("GE-Proton")).map(|(n, _)| n.as_str()).collect();
        if c["name"] == "typical" { assert_eq!(ge, ["GE-Proton10-34", "GE-Proton10-9", "GE-Proton10-4", "GE-Proton9-10"]); }
        let _ = std::fs::remove_dir_all(&home);
    }
}
