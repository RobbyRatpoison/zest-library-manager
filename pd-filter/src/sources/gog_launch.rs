//! What to run for an installed GOG game (port of `_get_info_candidates`, `_get_dosbox_launch` and `_find_gog_executable` in PlayDate's `plugins/gog/gog.py`).
//! A GOG install carries `goggame-<id>.info` (JSON) whose `playTasks` name the file to start; DOSBox games name the bundled `dosbox.exe`, which is replaced by the
//! system DOSBox with the game's own `.conf` files. Pure file reading: the system's DOSBox binaries are looked up through a closure so tests control them.
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const DOSBOX_NAMES: [&str; 4] = ["dosbox", "dosbox-staging", "dosbox-x", "dosbox-svn"];

/// The `.info` files to read, the game's own first.
pub fn info_candidates(install: &Path, product_id: &str) -> Vec<PathBuf> {
    let own = format!("goggame-{product_id}.info");
    let mut out = vec![install.join(&own)];
    if let Ok(rd) = std::fs::read_dir(install) {
        out.extend(rd.flatten().map(|e| e.file_name().to_string_lossy().to_string()).filter(|f| f.starts_with("goggame-") && f.ends_with(".info") && *f != own).map(|f| install.join(f)));
    }
    out
}

fn tasks(info: &Path) -> Vec<Value> {
    let Some(v) = std::fs::read(info).ok().and_then(|b| serde_json::from_str::<Value>(&String::from_utf8_lossy(&b)).ok()) else { return vec![] };
    let mut t: Vec<Value> = v.get("playTasks").and_then(|t| t.as_array()).cloned().unwrap_or_default();
    t.sort_by_key(|x| !x.get("isPrimary").and_then(|p| p.as_bool()).unwrap_or(false));   // the primary task first, order kept among equals
    t
}
fn s<'a>(v: &'a Value, k: &str) -> &'a str { v.get(k).and_then(|x| x.as_str()).unwrap_or("") }
fn is_file_task(t: &Value) -> bool { s(t, "type") == "FileTask" }
fn dosbox_exe(path_fwd: &str) -> bool { let l = path_fwd.to_lowercase(); l.ends_with(".exe") && l.rsplit('/').next().unwrap_or("").contains("dosbox") }

/// Shell-style splitting of a task's `arguments` (quotes and backslash escapes); on a malformed string, plain whitespace splitting as PlayDate did.
fn split_args(raw: &str) -> Vec<String> {
    let (mut out, mut cur, mut have, mut q) = (vec![], String::new(), false, None::<char>);
    let mut it = raw.chars();
    while let Some(c) = it.next() {
        match (q, c) {
            (Some(qc), c) if c == qc => q = None,
            (Some('"'), '\\') => match it.next() { Some(n @ ('"' | '\\' | '$' | '`')) => cur.push(n), Some(n) => { cur.push('\\'); cur.push(n) } None => return raw.split_whitespace().map(String::from).collect() },
            (Some(_), c) => cur.push(c),
            (None, '\'' | '"') => { q = Some(c); have = true }
            (None, '\\') => match it.next() { Some(n) => { cur.push(n); have = true } None => return raw.split_whitespace().map(String::from).collect() },
            (None, c) if c.is_whitespace() => { if have || !cur.is_empty() { out.push(std::mem::take(&mut cur)); have = false } }
            (None, c) => cur.push(c),
        }
    }
    if q.is_some() { return raw.split_whitespace().map(String::from).collect(); }
    if have || !cur.is_empty() { out.push(cur); }
    out
}

fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() { match c { std::path::Component::ParentDir => { if !out.pop() { out.push(".."); } } std::path::Component::CurDir => {} c => out.push(c) } }
    out
}

#[derive(Debug, PartialEq)]
pub enum Dosbox { NotDosbox, Missing, Run { bin: PathBuf, args: Vec<String>, cwd: PathBuf } }

/// A DOSBox game's launch: the system DOSBox (first of `DOSBOX_NAMES` that `which` finds), the task's arguments with each `-conf` name resolved to a real file (searched one level
/// above the working directory, then `app/`, then the install root; a name found nowhere is dropped), started in `app/` when it exists so `mount c ..` reaches the install root.
pub fn dosbox_launch(install: &Path, product_id: &str, which: &dyn Fn(&str) -> Option<PathBuf>) -> Dosbox {
    let task = info_candidates(install, product_id).iter().filter(|p| p.is_file())
        .find_map(|p| tasks(p).into_iter().find(|t| is_file_task(t) && dosbox_exe(&s(t, "path").replace('\\', "/"))));
    let Some(task) = task else { return Dosbox::NotDosbox };
    let Some(bin) = DOSBOX_NAMES.iter().find_map(|n| which(n)) else { return Dosbox::Missing };
    let work = s(&task, "workingDirectory").replace('\\', "/");
    let mut dirs = vec![];
    if !work.is_empty() { dirs.push(normalize(&install.join(&work).join(".."))); }
    dirs.push(install.join("app")); dirs.push(install.to_path_buf());
    let parts = split_args(s(&task, "arguments"));
    let (mut args, mut i) = (vec![], 0);
    while i < parts.len() {
        if parts[i].eq_ignore_ascii_case("-conf") && i + 1 < parts.len() {
            let name = parts[i + 1].replace('\\', "/"); let name = name.rsplit('/').next().unwrap_or("").to_string();
            if let Some(f) = dirs.iter().map(|d| d.join(&name)).find(|c| c.is_file()) { args.push("-conf".into()); args.push(f.to_string_lossy().to_string()); }
            i += 2;
        } else { args.push(parts[i].clone()); i += 1; }
    }
    let app = install.join("app");
    Dosbox::Run { bin, args, cwd: if app.is_dir() { app } else { install.to_path_buf() } }
}

#[derive(Debug, PartialEq)]
pub enum GogExe { /// Relative to the install folder, `/`-separated.
    File(String), /// A DOSBox game: the system's DOSBox stands in for the bundled `dosbox.exe` (no Wine/Proton needed); see `dosbox_launch` for the arguments.
    Dosbox(PathBuf), /// A DOSBox game, and the system has no DOSBox.
    NeedsDosbox, None }

/// `_find_gog_executable`: the primary play task's file (at its literal path, else under its working directory), else DOSBox; failing that a known Linux script, else the
/// shared scan of the folder's top level for something executable (scripts first, then 64-bit before 32-bit).
pub fn find_executable(install: &Path, product_id: &str, which: &dyn Fn(&str) -> Option<PathBuf>) -> GogExe {
    for info in info_candidates(install, product_id).iter().filter(|p| p.is_file()) {
        for t in tasks(info) {
            if !is_file_task(&t) || s(&t, "path").is_empty() { continue; }
            let path = s(&t, "path").replace('\\', "/");
            if install.join(&path).is_file() { return GogExe::File(path); }
            let work = s(&t, "workingDirectory");
            if !work.is_empty() {
                let joined = format!("{}/{}", work.replace('\\', "/").trim_end_matches('/'), path);
                let joined = joined.trim_start_matches('/').to_string();
                if install.join(&joined).is_file() { return GogExe::File(joined); }
            }
            if dosbox_exe(&path) { return DOSBOX_NAMES.iter().find_map(|n| which(n)).map_or(GogExe::NeedsDosbox, GogExe::Dosbox); }
        }
    }
    for n in ["start.sh", "game.sh", "launch.sh", "run.sh"] { if install.join(n).is_file() { return GogExe::File(n.into()); } }
    const SKIP: &[&str] = &["txt", "md", "cfg", "ini", "conf", "json", "log", "bak", "orig", "py", "xml", "html", "htm", "css", "js", "desktop", "png", "jpg", "svg", "ico"];
    let mut found: Vec<String> = vec![];
    if let Ok(rd) = std::fs::read_dir(install) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let p = e.path();
            if !p.is_file() || name.starts_with('.') { continue; }
            let ext = p.extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default();
            if SKIP.contains(&ext.as_str()) || name.to_lowercase().ends_with(".sh.bak") { continue; }
            if is_executable(&p) || crate::native_exe::is_elf(&p) { found.push(name); }
        }
    }
    if found.is_empty() { return GogExe::None; }
    let scripts: Vec<&String> = { let mut v: Vec<&String> = found.iter().filter(|f| matches!(Path::new(f.as_str()).extension().map(|x| x.to_string_lossy().to_lowercase()).as_deref(), Some("sh" | "bash" | "run" | "bin"))).collect(); v.sort(); v };
    if let Some(sc) = scripts.first() { return GogExe::File((*sc).clone()); }
    found.sort_by_key(|f| (crate::native_exe::native_rank(&install.join(f)), f.clone()));
    GogExe::File(found.remove(0))
}

#[cfg(unix)] fn is_executable(p: &Path) -> bool { use std::os::unix::fs::PermissionsExt; std::fs::metadata(p).map_or(false, |m| m.permissions().mode() & 0o111 != 0) }
#[cfg(not(unix))] fn is_executable(_p: &Path) -> bool { false }

#[cfg(test)]
mod tests {
    use super::*;
    fn tmp(tag: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("zest_goglaunch_{tag}_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d }
    fn put(d: &Path, rel: &str, body: &str) { let p = d.join(rel); std::fs::create_dir_all(p.parent().unwrap()).unwrap(); std::fs::write(p, body).unwrap(); }
    fn none(_: &str) -> Option<PathBuf> { None }
    fn info(tasks: &str) -> String { format!(r#"{{"gameId":"1","playTasks":{tasks}}}"#) }

    #[test] fn the_primary_task_wins_and_a_working_directory_is_tried_when_the_literal_path_is_missing() {
        let d = tmp("exe");
        put(&d, "goggame-1.info", &info(r#"[{"type":"FileTask","path":"Other.exe"},{"type":"FileTask","isPrimary":true,"path":"bin\\Game.exe"},{"type":"URLTask","path":"x"}]"#));
        put(&d, "bin/Game.exe", ""); put(&d, "Other.exe", "");
        assert_eq!(find_executable(&d, "1", &none), GogExe::File("bin/Game.exe".into()));
        let d = tmp("work");
        put(&d, "goggame-1.info", &info(r#"[{"type":"FileTask","isPrimary":true,"path":"Game.exe","workingDirectory":"sub\\dir\\"}]"#));
        put(&d, "sub/dir/Game.exe", "");
        assert_eq!(find_executable(&d, "1", &none), GogExe::File("sub/dir/Game.exe".into()));
        // another goggame-*.info is read when the game's own is absent (an add-on, a collection)
        let d = tmp("other"); put(&d, "goggame-2.info", &info(r#"[{"type":"FileTask","path":"B.exe"}]"#)); put(&d, "B.exe", "");
        assert_eq!(find_executable(&d, "1", &none), GogExe::File("B.exe".into()));
    }
    #[test] fn without_a_usable_task_a_known_script_then_the_executable_files_are_used_scripts_first() {
        let d = tmp("fallback");
        put(&d, "readme.txt", ""); put(&d, ".hidden", ""); put(&d, "Zed.x86", ""); put(&d, "Zed.x86_64", ""); put(&d, "tool.sh.bak", "");
        for f in ["Zed.x86", "Zed.x86_64"] { std::fs::write(d.join(f), "x").unwrap(); #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(d.join(f), std::fs::Permissions::from_mode(0o755)).unwrap(); } }
        assert_eq!(find_executable(&d, "1", &none), GogExe::File("Zed.x86_64".into()), "no ELF header here, so the extension convention ranks 64-bit first");
        put(&d, "zz.sh", "#!/bin/sh"); #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(d.join("zz.sh"), std::fs::Permissions::from_mode(0o755)).unwrap(); }
        assert_eq!(find_executable(&d, "1", &none), GogExe::File("zz.sh".into()));
        put(&d, "start.sh", ""); assert_eq!(find_executable(&d, "1", &none), GogExe::File("start.sh".into()), "the named launcher beats the scan");
        assert_eq!(find_executable(&tmp("empty"), "1", &none), GogExe::None);
    }
    #[test] fn dosbox_games_use_the_system_dosbox_or_ask_for_one() {
        let d = tmp("dos");
        put(&d, "goggame-1.info", &info(r#"[{"type":"FileTask","isPrimary":true,"path":"DOSBOX\\dosbox.exe","arguments":"-conf \"..\\game.conf\" -conf missing.conf -noconsole","workingDirectory":"DOSBOX"}]"#));
        put(&d, "game.conf", ""); put(&d, "app/other.conf", "");
        assert_eq!(find_executable(&d, "1", &none), GogExe::NeedsDosbox);
        let which = |n: &str| (n == "dosbox-staging").then(|| PathBuf::from("/usr/bin/dosbox-staging"));
        assert_eq!(find_executable(&d, "1", &which), GogExe::Dosbox("/usr/bin/dosbox-staging".into()));
        assert_eq!(dosbox_launch(&d, "1", &none), Dosbox::Missing);
        // -conf resolved beside the working directory's parent (the install root here), a name found nowhere dropped, cwd = app/ when it exists
        assert_eq!(dosbox_launch(&d, "1", &which), Dosbox::Run { bin: "/usr/bin/dosbox-staging".into(), args: vec!["-conf".into(), d.join("game.conf").to_string_lossy().to_string(), "-noconsole".into()], cwd: d.join("app") });
        let plain = tmp("plain"); put(&plain, "goggame-1.info", &info(r#"[{"type":"FileTask","path":"Game.exe"}]"#));
        assert_eq!(dosbox_launch(&plain, "1", &which), Dosbox::NotDosbox);
    }
    #[test] fn arguments_split_like_a_shell() {
        assert_eq!(split_args(r#"-conf "a b.conf" 'c d' e\ f"#), vec!["-conf", "a b.conf", "c d", "e f"]);
        assert_eq!(split_args(r#"unterminated "quote here"#), vec!["unterminated", "\"quote", "here"]);
        assert_eq!(split_args(r#"-x "" y"#), vec!["-x", "", "y"]);
    }
}
