//! The programs installed on this computer, for adding a game that has no store behind it: Linux `.desktop` launchers (including Flatpak and Snap apps, which start
//! through `flatpak run ...`), macOS `.app` folders, and the Windows Start Menu shortcuts. Each is a name and what to run; the user picks one instead of typing a path.
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub struct App { pub name: String, /// The program to run: a full path (on a Mac the `.app` folder).
    pub program: String, /// Arguments to run it with (what a Flatpak needs: `run com.example.App`).
    pub args: String }

/// `shlex.split`: spaces separate, single or double quotes group, a backslash escapes (outside quotes, and before a quote or backslash inside double ones).
pub fn split_args(s: &str) -> Vec<String> {
    let (mut out, mut cur, mut have) = (vec![], String::new(), false);
    let mut it = s.chars().peekable(); let mut q: Option<char> = None;
    while let Some(c) = it.next() {
        match (q, c) {
            (None, ' ' | '\t' | '\n') => { if have { out.push(std::mem::take(&mut cur)); have = false; } }
            (None, '\'' | '"') => { q = Some(c); have = true; }
            (Some(x), c) if c == x => q = None,
            (None, '\\') | (Some('"'), '\\') => { if let Some(&n) = it.peek() { if q.is_none() || n == '"' || n == '\\' { cur.push(n); it.next(); have = true; } else { cur.push('\\'); have = true; } } }
            (_, c) => { cur.push(c); have = true; }
        }
    }
    if have { out.push(cur); }
    out
}
/// The reverse for showing arguments back in a box: a part with a space or quote gets quoted.
pub fn join_args(parts: &[String]) -> String {
    parts.iter().map(|p| if p.is_empty() || p.contains([' ', '"', '\'', '\\']) { format!("\"{}\"", p.replace('\\', "\\\\").replace('"', "\\\"")) } else { p.clone() }).collect::<Vec<_>>().join(" ")
}

/// A program name found on the PATH (a full path stays as it is).
pub fn which(program: &str) -> Option<PathBuf> {
    let p = Path::new(program);
    if p.is_absolute() { return p.is_file().then(|| p.to_path_buf()); }
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(program)).find(|f| f.is_file())
}

/// One `.desktop` file: its `[Desktop Entry]` name and command, without the field codes (`%U`, `%f`...) a launcher fills in. None for anything that is not a visible application.
pub fn parse_desktop(text: &str) -> Option<App> {
    let (mut name, mut exec, mut ty, mut hidden) = (None::<String>, None::<String>, String::new(), false);
    let mut inside = false;
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('[') { inside = l == "[Desktop Entry]"; continue; }
        if !inside { continue; }
        if let Some((k, v)) = l.split_once('=') {
            match k.trim() { "Name" => name = Some(v.trim().to_string()), "Exec" => exec = Some(v.trim().to_string()), "Type" => ty = v.trim().to_string(),
                "NoDisplay" | "Hidden" => if v.trim().eq_ignore_ascii_case("true") { hidden = true }, _ => {} }
        }
    }
    if hidden || (!ty.is_empty() && ty != "Application") { return None; }
    let (name, exec) = (name.filter(|n| !n.is_empty())?, exec?);
    if exec.contains("steam://") { return None; }   // a Steam game's launcher: the game is already in the library
    let all = split_args(&exec);
    let is_code = |t: &str| matches!(t, "%f" | "%F" | "%u" | "%U" | "%d" | "%D" | "%n" | "%N" | "%i" | "%c" | "%k" | "%v" | "%m" | "@@u" | "@@" | "@@f");
    let had_code = all.iter().any(|t| is_code(t));
    let mut parts: Vec<String> = all.into_iter().filter(|t| !is_code(t)).map(|t| t.replace("%%", "%")).collect();
    // `discord --url -- %u`: once the address is gone, the switches that only carried it are no use either
    if had_code { while parts.len() > 1 && matches!(parts.last().map(String::as_str), Some("--" | "--url" | "--file" | "--uri" | "--open-url" | "-u" | "--new-tab" | "--new-window")) { parts.pop(); } }
    if parts.first().map(String::as_str) == Some("env") { parts.remove(0); while parts.first().map_or(false, |t| t.contains('=') && !t.starts_with('/')) { parts.remove(0); } }
    let program = which(parts.first()?)?;
    Some(App { name, program: program.to_string_lossy().to_string(), args: join_args(&parts[1..]) })
}

#[cfg(target_os = "linux")]
fn scan_platform() -> Vec<App> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut dirs: Vec<PathBuf> = vec![];
    let data_home = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).or_else(|| home.as_ref().map(|h| h.join(".local/share")));
    if let Some(d) = data_home { dirs.push(d.join("applications")); }
    let sys = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    for d in sys.split(':').filter(|d| !d.is_empty()) { dirs.push(Path::new(d).join("applications")); }
    dirs.push("/var/lib/flatpak/exports/share/applications".into());
    if let Some(h) = &home { dirs.push(h.join(".local/share/flatpak/exports/share/applications")); }
    dirs.push("/var/lib/snapd/desktop/applications".into());
    let mut out = vec![];
    for d in dirs { if let Ok(rd) = std::fs::read_dir(d) { for e in rd.flatten() { let p = e.path(); if p.extension().map_or(false, |x| x == "desktop") { if let Some(a) = std::fs::read_to_string(&p).ok().and_then(|t| parse_desktop(&t)) { out.push(a); } } } } }
    out
}
#[cfg(target_os = "macos")]
fn scan_platform() -> Vec<App> {
    let mut dirs: Vec<PathBuf> = vec!["/Applications".into(), "/System/Applications".into()];
    if let Some(h) = std::env::var_os("HOME") { dirs.push(Path::new(&h).join("Applications")); }
    let mut out = vec![];
    for d in dirs { if let Ok(rd) = std::fs::read_dir(d) { for e in rd.flatten() { let p = e.path(); if p.extension().map_or(false, |x| x == "app") { out.push(App { name: p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(), program: p.to_string_lossy().to_string(), args: String::new() }); } } } }
    out
}
#[cfg(windows)]
fn scan_platform() -> Vec<App> {
    // one PowerShell run lists every Start Menu shortcut with its target
    let script = r#"$sh = New-Object -ComObject WScript.Shell; foreach ($root in @("$env:ProgramData\Microsoft\Windows\Start Menu\Programs", "$env:APPDATA\Microsoft\Windows\Start Menu\Programs")) { if (Test-Path $root) { Get-ChildItem $root -Recurse -Filter *.lnk -ErrorAction SilentlyContinue | ForEach-Object { try { $s = $sh.CreateShortcut($_.FullName); if ($s.TargetPath) { $_.BaseName + '|' + $s.TargetPath + '|' + $s.Arguments } } catch {} } } }"#;
    let Ok(o) = ({ use crate::proc::Hide; std::process::Command::new("powershell").args(["-NoProfile", "-NonInteractive", "-Command", script]).stdin(std::process::Stdio::null()).hidden().output() }) else { return vec![] };
    parse_shortcut_lines(&String::from_utf8_lossy(&o.stdout))
}
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn scan_platform() -> Vec<App> { vec![] }

/// `name|target|arguments` lines from the Windows shortcut listing: programs only, uninstallers left out.
pub fn parse_shortcut_lines(text: &str) -> Vec<App> {
    text.lines().filter_map(|l| {
        let mut p = l.trim().splitn(3, '|');
        let (name, target, args) = (p.next()?.trim(), p.next()?.trim(), p.next().unwrap_or("").trim());
        let t = target.to_lowercase();
        (!name.is_empty() && t.ends_with(".exe") && !name.to_lowercase().contains("uninstall") && !t.contains("unins") && !t.ends_with("\\msiexec.exe")).then(|| App { name: name.to_string(), program: target.to_string(), args: args.to_string() })
    }).collect()
}

/// Every installed program, by name, without repeats.
pub fn scan() -> Vec<App> {
    let mut v = scan_platform();
    v.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.program.cmp(&b.program)));
    v.dedup_by(|a, b| a.name.eq_ignore_ascii_case(&b.name) && a.program == b.program && a.args == b.args);
    v
}
/// The ones whose name contains every word typed (all of them for an empty search).
pub fn filter<'a>(apps: &'a [App], q: &str) -> Vec<&'a App> {
    let words: Vec<String> = q.split_whitespace().map(|w| w.to_lowercase()).collect();
    apps.iter().filter(|a| { let n = a.name.to_lowercase(); words.iter().all(|w| n.contains(w.as_str())) }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn a_desktop_file_gives_a_name_and_a_command_without_field_codes() {
        let a = parse_desktop("[Desktop Entry]\nType=Application\nName=Discord\nExec=/bin/sh %U\nName[de]=Zwietracht\n[Desktop Action x]\nName=Other\nExec=/nope\n").unwrap();
        assert_eq!((a.name.as_str(), a.program.as_str(), a.args.as_str()), ("Discord", which("/bin/sh").unwrap().to_str().unwrap(), ""));
        let f = parse_desktop("[Desktop Entry]\nName=Some App\nType=Application\nExec=/bin/sh run --branch=stable --arch=x86_64 \"--command=a b\" com.example.App @@u %U @@\n").unwrap();
        assert_eq!(f.args, r#"run --branch=stable --arch=x86_64 "--command=a b" com.example.App"#);
        let d = parse_desktop("[Desktop Entry]\nType=Application\nName=Discord\nExec=/bin/sh --url -- %u\n").unwrap();
        assert_eq!(d.args, "", "the switches that only carried the address are dropped with it");
        let k = parse_desktop("[Desktop Entry]\nType=Application\nName=Keep\nExec=/bin/sh --fullscreen --url x\n").unwrap();
        assert_eq!(k.args, "--fullscreen --url x", "without a field code nothing is dropped");
        let e = parse_desktop("[Desktop Entry]\nName=Env\nType=Application\nExec=env FOO=1 BAR=2 /bin/sh -c x\n").unwrap();
        assert_eq!((e.program.ends_with("/sh"), e.args.as_str()), (true, "-c x"));
    }
    #[test] fn hidden_steam_and_missing_programs_are_left_out() {
        assert!(parse_desktop("[Desktop Entry]\nType=Application\nName=X\nExec=/bin/sh\nNoDisplay=true\n").is_none());
        assert!(parse_desktop("[Desktop Entry]\nType=Link\nName=X\nExec=/bin/sh\n").is_none());
        assert!(parse_desktop("[Desktop Entry]\nType=Application\nName=Game\nExec=steam steam://rungameid/10\n").is_none());
        assert!(parse_desktop("[Desktop Entry]\nType=Application\nName=Gone\nExec=/no/such/program-zest\n").is_none());
        assert!(parse_desktop("[Desktop Entry]\nType=Application\nExec=/bin/sh\n").is_none());
    }
    #[test] fn arguments_split_and_join_both_ways() {
        assert_eq!(split_args(r#"--name "Two Words" 'a b' c\ d"#), ["--name", "Two Words", "a b", "c d"]);
        assert_eq!(split_args(r#""a\"b""#), [r#"a"b"#]); assert_eq!(split_args(r#"x "" y"#), ["x", "", "y"]);
        let parts: Vec<String> = ["run", "a b", "q\"x"].iter().map(|s| s.to_string()).collect();
        assert_eq!(split_args(&join_args(&parts)), parts);
    }
    #[test] fn windows_shortcut_lines_keep_programs_only() {
        let a = parse_shortcut_lines("Discord|C:\\Users\\r\\AppData\\Local\\Discord\\Update.exe|--processStart Discord.exe\nUninstall Foo|C:\\x\\u.exe|\nDoc|C:\\d\\readme.txt|\nBlender|C:\\Program Files\\Blender\\blender.exe|\n");
        assert_eq!(a.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), ["Discord", "Blender"]); assert_eq!(a[0].args, "--processStart Discord.exe");
    }
    #[test] fn search_needs_every_word() {
        let v: Vec<App> = ["Discord", "Blender Foundation", "Blender"].iter().map(|n| App { name: n.to_string(), program: "/x".into(), args: String::new() }).collect();
        assert_eq!(filter(&v, "bl").len(), 2); assert_eq!(filter(&v, "blender foun").len(), 1); assert_eq!(filter(&v, "").len(), 3); assert!(filter(&v, "zzz").is_empty());
    }
    #[test] #[cfg(target_os = "linux")] fn this_machine_has_some_programs_and_scanning_never_panics() { let v = scan(); assert!(v.windows(2).all(|w| w[0].name.to_lowercase() <= w[1].name.to_lowercase())); }
}
