//! Windows: uninstalling a game through the entry it registered under `...\CurrentVersion\Uninstall` (what Settings > Apps lists). Most installer-based games have one (GOG's offline
//! installers, Battle.net's games, plain setup programs). The entry is found by the game's install folder, then by its name, and its uninstall command is started: the uninstaller shows its
//! own confirmation window, so nothing is deleted without the person seeing it.
use crate::sources::{Action, GameRef};
use std::collections::HashMap;

pub type Entry = HashMap<String, String>;

/// The entries in the output of `reg query <key> /s`: one per subkey, its string values by name (`    Name    REG_SZ    data`; numbers and other types are skipped).
pub fn parse_reg_query(text: &str) -> Vec<Entry> {
    let mut out: Vec<Entry> = vec![];
    let mut cur: Option<usize> = None;
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with("HKEY_") { out.push(Entry::new()); cur = Some(out.len() - 1); continue; }
        let Some(i) = cur else { continue };
        let Some(t) = line.strip_prefix("    ") else { continue };
        let Some(p) = t.find("    REG_") else { continue };
        let (name, rest) = (&t[..p], &t[p + 4..]);
        let Some((kind, data)) = rest.split_once("    ") else { continue };
        if kind == "REG_SZ" || kind == "REG_EXPAND_SZ" { out[i].insert(name.to_string(), data.to_string()); }
    }
    out.retain(|e| !e.is_empty());
    out
}

/// A command line such as `"C:\Games\Foo\unins000.exe" /SILENT` or `MsiExec.exe /X{GUID}` -> the program and its arguments (quotes around an argument removed).
pub fn split_command(cmd: &str) -> (String, Vec<String>) {
    // an unquoted program path may contain spaces (Steam registers `C:\Program Files (x86)\Steam\uninstall.exe`): the program ends at the first ".exe"
    let t = cmd.trim();
    if !t.starts_with('"') {
        if let Some(i) = t.to_lowercase().find(".exe").map(|i| i + 4) {
            if t[..i].contains(' ') && t.is_char_boundary(i) && t[i..].chars().next().map_or(true, char::is_whitespace) { return (t[..i].to_string(), split_command(&format!("x {}", &t[i..])).1); }
        }
    }
    let mut parts: Vec<String> = vec![];
    let (mut cur, mut quoted, mut any) = (String::new(), false, false);
    for c in cmd.trim().chars() {
        match c {
            '"' => { quoted = !quoted; any = true; }
            c if c.is_whitespace() && !quoted => { if any { parts.push(std::mem::take(&mut cur)); any = false; } }
            c => { cur.push(c); any = true; }
        }
    }
    if any { parts.push(cur); }
    let mut it = parts.into_iter();
    (it.next().unwrap_or_default(), it.collect())
}

/// The uninstall command of the game's entry: by install folder (either may be the tail of the other), else by display name. Entries without a command are not considered.
pub fn command_for(entries: &[Entry], install_path: &str, name: &str) -> Option<String> {
    let usable: Vec<Entry> = entries.iter().filter(|e| e.get("UninstallString").map_or(false, |s| !s.trim().is_empty())).cloned().collect();
    let e = crate::sources::battle_net::pick_entry(&usable, install_path, name)?;
    e.get("UninstallString").cloned()
}

/// The uninstall command of a program by its display name: the exact name (any letter case), else the first entry whose name contains it.
pub fn command_by_display_name(entries: &[Entry], name: &str) -> Option<String> {
    let want = name.trim().to_lowercase();
    if want.is_empty() { return None; }
    let usable: Vec<&Entry> = entries.iter().filter(|e| e.get("UninstallString").map_or(false, |s| !s.trim().is_empty())).collect();
    let by = |f: &dyn Fn(&str) -> bool| usable.iter().find(|e| e.get("DisplayName").map_or(false, |d| f(&d.trim().to_lowercase()))).and_then(|e| e.get("UninstallString").cloned());
    by(&|d| d == want).or_else(|| by(&|d| d.contains(&want)))
}

/// Every uninstall entry on this machine (both 64 and 32 bit programs, machine and user).
#[cfg(windows)]
pub fn entries() -> Vec<Entry> {
    use crate::proc::Hide;
    let mut out = vec![];
    for key in [r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall", r"HKLM\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall", r"HKCU\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall"] {
        if let Ok(o) = std::process::Command::new("reg").args(["query", key, "/s"]).hidden().output() { out.extend(parse_reg_query(&String::from_utf8_lossy(&o.stdout))); }
    }
    out
}
#[cfg(not(windows))]
pub fn entries() -> Vec<Entry> { vec![] }

/// Start the game's uninstaller. Err says why it could not be found.
pub fn action(g: &GameRef) -> Result<Action, String> {
    if !cfg!(windows) { return Err(format!("Uninstalling {} games is not supported yet", g.platform)); }
    if !g.installed { return Err(format!("{} is not installed.", g.name)); }
    let cmd = command_for(&entries(), g.install_path.as_deref().unwrap_or(""), &g.name).ok_or_else(|| format!("Could not find an uninstaller for {}. Remove it from Settings > Apps instead.", g.name))?;
    let (program, args) = split_command(&cmd);
    if program.is_empty() { return Err(format!("{}'s uninstall entry is empty.", g.name)); }
    crate::net::log(&format!("uninstall {}: starting {program}", g.name));
    Ok(Action::Spawn { program, args, dir: None, env: vec![] })
}

/// After an uninstaller was started: say (once, from another thread) when the game is gone, that is when its uninstall entry has left the registry or its install folder no longer
/// exists. Checked every 5 s for up to 30 minutes, starting a few seconds in; an uninstaller the person cancels simply never reports.
pub fn watch_gone(g: &GameRef, on_gone: impl FnOnce() + Send + 'static) {
    if !cfg!(windows) { return; }
    let (path, name) = (g.install_path.clone().unwrap_or_default(), g.name.clone());
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(3));
        for _ in 0..360 {
            let folder_gone = !path.is_empty() && !std::path::Path::new(&path).exists();
            if folder_gone || command_for(&entries(), &path, &name).is_none() { crate::net::log(&format!("uninstall {name}: gone")); on_gone(); return; }
            std::thread::sleep(std::time::Duration::from_secs(5));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    const SAMPLE: &str = "\r\nHKEY_LOCAL_MACHINE\\SOFTWARE\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\AddressBook\r\n\r\nHKEY_LOCAL_MACHINE\\SOFTWARE\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Connection Manager\r\n    SystemComponent    REG_DWORD    0x1\r\n\r\nHKEY_LOCAL_MACHINE\\SOFTWARE\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Microsoft Copilot\r\n    DisplayName    REG_SZ    Copilot\r\n    UninstallString    REG_SZ    \"C:\\Program Files (x86)\\Microsoft\\Copilot\\Application\\158.0\\Installer\\copilot_setup.exe\" --uninstall --channel=stable\r\n    InstallLocation    REG_SZ    C:\\Program Files (x86)\\Microsoft\\Copilot\\Application\r\n    EstimatedSize    REG_DWORD    0x34eea4\r\n\r\nHKEY_LOCAL_MACHINE\\SOFTWARE\\GOG\\Uninstall\\1207658930_is1\r\n    DisplayName    REG_SZ    The Witcher: Enhanced Edition\r\n    InstallLocation    REG_SZ    C:\\GOG Games\\Witcher\\\r\n    UninstallString    REG_SZ    \"C:\\GOG Games\\Witcher\\unins000.exe\"\r\n";
    #[test] fn the_entries_of_reg_query_are_read() {
        let e = parse_reg_query(SAMPLE);
        assert_eq!(e.len(), 2);   // empty subkeys and a key with only a number are dropped
        assert_eq!(e[0]["DisplayName"], "Copilot");
        assert!(e[0]["UninstallString"].ends_with("--channel=stable") && !e[0].contains_key("EstimatedSize"));
        assert_eq!(e[1]["InstallLocation"], "C:\\GOG Games\\Witcher\\");
    }
    #[test] fn a_command_line_is_split_into_program_and_arguments() {
        assert_eq!(split_command("\"C:\\a b\\unins000.exe\" /SILENT \"x y\""), ("C:\\a b\\unins000.exe".to_string(), vec!["/SILENT".to_string(), "x y".to_string()]));
        assert_eq!(split_command("MsiExec.exe /X{1234-ABCD}"), ("MsiExec.exe".to_string(), vec!["/X{1234-ABCD}".to_string()]));
        assert_eq!(split_command("  \"C:\\p.exe\"  "), ("C:\\p.exe".to_string(), vec![]));
        assert_eq!(split_command(""), (String::new(), vec![]));
        assert_eq!(split_command("C:\\Program Files (x86)\\Steam\\uninstall.exe"), ("C:\\Program Files (x86)\\Steam\\uninstall.exe".to_string(), vec![]));
        assert_eq!(split_command("C:\\Program Files\\Foo\\un.exe /S /D=\"a b\""), ("C:\\Program Files\\Foo\\un.exe".to_string(), vec!["/S".to_string(), "/D=a b".to_string()]));
        assert_eq!(split_command("C:\\Windows\\system32\\msiexec.exe /x {A-B}"), ("C:\\Windows\\system32\\msiexec.exe".to_string(), vec!["/x".to_string(), "{A-B}".to_string()]));
    }
    /// Runs on Windows only: the machine's real registry lists programs, and one is found by name and by folder.
    #[cfg(windows)]
    #[test] fn the_real_registry_is_read() {
        let e = entries();
        assert!(e.len() > 3, "found {} entries", e.len());
        let edge = command_for(&e, "", "Microsoft Edge").expect("an uninstall command for Microsoft Edge");
        let (prog, args) = split_command(&edge);
        assert!(prog.to_lowercase().ends_with(".exe") && !args.is_empty(), "{edge}");
        assert!(command_for(&e, r"C:\Program Files (x86)\Microsoft\Edge\Application", "different name").is_some());
    }
    #[test] fn a_program_is_found_by_its_display_name() {
        let e = parse_reg_query(SAMPLE);
        assert!(command_by_display_name(&e, "COPILOT").unwrap().contains("copilot_setup"));
        assert!(command_by_display_name(&e, "witcher").unwrap().contains("unins000"));
        assert!(command_by_display_name(&e, "").is_none() && command_by_display_name(&e, "Nothing here").is_none());
    }
    #[test] fn the_game_is_found_by_folder_then_by_name() {
        let e = parse_reg_query(SAMPLE);
        assert_eq!(command_for(&e, "C:\\GOG Games\\Witcher", "Anything").unwrap(), "\"C:\\GOG Games\\Witcher\\unins000.exe\"");
        assert!(command_for(&e, "", "the witcher: enhanced edition").unwrap().contains("unins000"));
        assert!(command_for(&e, "D:\\Elsewhere", "Not Installed").is_none());
    }
}
