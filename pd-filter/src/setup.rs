//! First run: a data folder with no library gets one. The account is the one `config.json` already names, else the Steam account Steam's own files say was used last, else an id
//! the user gave (`--steam-id`, `ZEST_STEAM_ID`; `--steam-key` / `ZEST_STEAM_KEY` for the Web API key). The library database is made with PlayDate's schema, named `games_<steam id>.db`.
use crate::account::{self, Account};
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq)]
pub struct Made { pub steam_id: String, pub label: String, /// The account came from Steam's files, not from the user: say so.
    pub detected: bool, pub db: PathBuf }

pub fn find_library(root: &Path) -> Option<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(root).ok()?.flatten().map(|e| e.path()).filter(|p| p.file_name().map_or(false, |n| { let n = n.to_string_lossy(); n.starts_with("games_") && n.ends_with(".db") })).collect();
    v.sort();
    let active = account::load_account(root).map(|a| root.join(format!("games_{}.db", a.steam_id)));
    if let Some(a) = active.filter(|a| v.contains(a)) { return Some(a); }
    v.into_iter().next()
}

fn valid_id(id: &str) -> bool { id.len() >= 15 && id.chars().all(|c| c.is_ascii_digit()) }

/// Makes the library for a folder that has none. `given` is an account the user supplied (id, optional key); `steam_root` is where Steam's files were found. Err = nothing to
/// make an account from (the message says what to do).
pub fn first_run(root: &Path, given: Option<(String, Option<String>)>, steam_root: Option<&Path>) -> Result<Made, String> {
    std::fs::create_dir_all(root).map_err(|e| format!("Could not make the data folder {}: {e}", root.display()))?;
    let (acct, detected) = if let Some((id, key)) = given {
        if !valid_id(id.trim()) { return Err("The Steam ID is the long number (17 digits) of your profile, not the profile name.".into()); }
        (Account { steam_id: id.trim().into(), api_key: key.unwrap_or_default().trim().into(), label: String::new() }, false)
    } else if let Some(a) = account::load_account(root).filter(|a| valid_id(&a.steam_id)) { (a, false) }
    else if let Some(a) = steam_root.map(crate::steamlocal::accounts).and_then(|v| v.into_iter().find(|a| valid_id(&a.steam_id))) { (Account { steam_id: a.steam_id, api_key: String::new(), label: a.name }, true) }
    else { return Err("Zest could not tell which Steam account to use. Start Steam once and sign in, or start Zest with --steam-id <your 17 digit Steam ID> (and --steam-key <Web API key> for achievements).".into()); };
    let db = root.join(format!("games_{}.db", acct.steam_id));
    crate::schema::init_db(&db)?;
    account::save_account(root, &acct, true).map_err(|e| format!("Could not save the account: {e}"))?;
    Ok(Made { steam_id: acct.steam_id, label: acct.label, detected, db })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tmp(tag: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("zest_setup_{tag}_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); d }
    #[test] fn a_given_account_makes_the_library_and_is_saved_like_playdates() {
        let d = tmp("given");
        let m = first_run(&d, Some(("76561198000000001".into(), Some(" KEY ".into()))), None).unwrap();
        assert_eq!((m.steam_id.as_str(), m.detected), ("76561198000000001", false)); assert!(m.db.ends_with("games_76561198000000001.db") && m.db.is_file());
        assert_eq!(find_library(&d), Some(m.db.clone()));
        let a = account::load_account(&d).unwrap(); assert_eq!((a.steam_id.as_str(), a.api_key.as_str()), ("76561198000000001", "KEY"));
        let c = rusqlite::Connection::open(&m.db).unwrap(); assert!(c.prepare("SELECT hltb_fetched, platform_slug FROM games").is_ok(), "the schema is complete");
    }
    #[test] fn the_account_in_config_json_wins_over_steams_files_and_nothing_known_is_an_error() {
        let d = tmp("cfg"); std::fs::create_dir_all(&d).unwrap();
        account::save_account(&d, &Account { steam_id: "76561198000000002".into(), api_key: "k".into(), label: "L".into() }, true).unwrap();
        let m = first_run(&d, None, None).unwrap(); assert_eq!((m.steam_id.as_str(), m.detected), ("76561198000000002", false));
        let e = tmp("none"); assert!(first_run(&e, None, None).unwrap_err().contains("--steam-id"));
        assert!(first_run(&e, Some(("shortname".into(), None)), None).unwrap_err().contains("17 digits"));
    }
    #[test] fn steams_own_files_name_the_account_when_nothing_else_does() {
        let root = tmp("steam"); let ud = root.join("userdata/39735275/config"); std::fs::create_dir_all(&ud).unwrap(); std::fs::write(ud.join("localconfig.vdf"), "\"UserLocalConfigStore\" {}").unwrap();
        std::fs::create_dir_all(root.join("config")).unwrap();
        std::fs::write(root.join("config/loginusers.vdf"), "\"users\" { \"76561198000001003\" { \"PersonaName\" \"Robby\" \"Timestamp\" \"1\" } }").unwrap();
        let d = tmp("data");
        let m = first_run(&d, None, Some(&root)).unwrap();
        assert_eq!((m.steam_id.as_str(), m.detected), ("76561198000001003", true)); assert_eq!(m.label, "Robby");
    }
}
