//! `archive` against archives made by Python's own zipfile / tarfile (`gen_archive_fixtures.py`).
use pd_filter::archive::*;
use std::path::PathBuf;

fn data(n: &str) -> PathBuf { PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data").join(n) }
fn tmp(tag: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("zest_arc_{tag}_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); d }

fn check(dest: &PathBuf, evil_dir: &PathBuf) {
    assert_eq!(std::fs::read(dest.join("game/data/level1.dat")).unwrap(), [b'L', b'1'].repeat(5000));
    assert_eq!(std::fs::read(dest.join("game/readme.txt")).unwrap(), b"read me");
    assert_eq!(std::fs::read(dest.join("empty.txt")).unwrap(), b"");
    assert!(dest.join("game/emptydir").is_dir());
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(dest.join("game/run")).unwrap().permissions().mode() & 0o777, 0o755, "the executable bit is kept");
        assert_eq!(std::fs::metadata(dest.join("game/readme.txt")).unwrap().permissions().mode() & 0o777, 0o644);
        assert_eq!(std::fs::read_link(dest.join("game/link")).unwrap(), PathBuf::from("run"));
    }
    // nothing escaped the folder
    assert!(!evil_dir.join("evil.txt").exists() && !dest.join("abs/evil.txt").exists() && !PathBuf::from("/abs/evil.txt").exists());
}

#[test] fn a_zip_unpacks_with_permissions_backslash_folders_and_no_zip_slip() {
    let parent = tmp("zip"); let dest = parent.join("out");
    let mut last = (0, 0);
    let n = extract_zip(&data("sample.zip"), &dest, &mut |d, t| last = (d, t), &|| false).unwrap();
    assert!(n >= 5, "{n} files");
    check(&dest, &parent);
    assert_eq!(std::fs::read(dest.join("win/style/path.txt")).unwrap(), b"backslashes");
    assert_eq!(last.0, last.1, "progress ends at the total");
    let _ = std::fs::remove_dir_all(&parent);
}
#[test] fn a_tar_gz_unpacks_with_long_names_permissions_links_and_no_escape() {
    let parent = tmp("tgz"); let dest = parent.join("out");
    let n = extract(&data("sample.tar.gz"), &dest, &mut |_, _| {}, &|| false).unwrap();
    assert!(n >= 5, "{n} files");
    check(&dest, &parent);
    assert_eq!(std::fs::read(dest.join(format!("game/{}file.txt", "deep/".repeat(30)))).unwrap(), b"long");
    let _ = std::fs::remove_dir_all(&parent);
}
#[test] fn cancel_stops_and_bad_input_is_explained() {
    let parent = tmp("bad");
    assert_eq!(extract_zip(&data("sample.zip"), &parent.join("o"), &mut |_, _| {}, &|| true).unwrap_err(), "Cancelled");
    let junk = parent.join("x.zip"); std::fs::create_dir_all(&parent).unwrap(); std::fs::write(&junk, b"not a zip at all").unwrap();
    assert!(extract(&junk, &parent.join("o"), &mut |_, _| {}, &|| false).unwrap_err().contains("not a zip"));
    assert!(extract(&parent.join("game.arj"), &parent.join("o"), &mut |_, _| {}, &|| false).unwrap_err().contains("cannot unpack"));
    assert_eq!((kind_of("A.ZIP"), kind_of("a.tar.gz"), kind_of("a.tgz"), kind_of("a.tar"), kind_of("a.sh")), (Some("zip"), Some("tar.gz"), Some("tar.gz"), Some("tar"), None));
    assert_eq!((safe_rel("a/../b"), safe_rel("/etc/x"), safe_rel("C:\\x"), safe_rel("a\\b/./c"), safe_rel("")), (None, None, None, Some(PathBuf::from("a/b/c")), None));
    let _ = std::fs::remove_dir_all(&parent);
}

#[test] fn other_formats_go_to_a_program_on_the_system_or_say_what_is_missing() {
    let have = ["bsdtar"].iter().any(|t| std::env::var_os("PATH").map_or(false, |p| std::env::split_paths(&p).any(|d| d.join(t).is_file())));
    let parent = tmp("xz"); let dest = parent.join("out");
    if have {
        assert_eq!(kind_of("a.tar.xz"), Some("external"));
        let n = extract(&data("sample.tar.xz"), &dest, &mut |_, _| {}, &|| false).unwrap();
        assert!(n >= 4, "{n}");
        assert_eq!(std::fs::read(dest.join("game/readme.txt")).unwrap(), b"read me");
        #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; assert_eq!(std::fs::metadata(dest.join("game/run")).unwrap().permissions().mode() & 0o777, 0o755); }
        assert_eq!(extract(&data("sample.tar.xz"), &dest, &mut |_, _| {}, &|| true).unwrap_err(), "Cancelled");
    }
    let junk = parent.join("x.7z"); std::fs::create_dir_all(&parent).unwrap(); std::fs::write(&junk, b"not an archive").unwrap();
    assert!(extract(&junk, &parent.join("o2"), &mut |_, _| {}, &|| false).is_err());
    let _ = std::fs::remove_dir_all(&parent);
}

#[test] fn a_zip_appended_to_a_program_is_found_by_its_contents_and_unpacked() {
    let p = data("mojo-installer.bin");
    assert!(is_zip_payload(&p) && !is_zip_payload(&data("sample.tar.gz")) && is_zip_payload(&data("sample.zip")));
    let parent = tmp("mojo"); let dest = parent.join("out");
    let n = extract(&p, &dest, &mut |_, _| {}, &|| false).unwrap();
    assert_eq!(n, 6);   // data/Game, x86_64/game.x86_64, libx.so.1, Readme.txt, scripts/config.luac, meta/x
    assert!(dest.join("data/Game").is_file());
    let _ = std::fs::remove_dir_all(&parent);
}
