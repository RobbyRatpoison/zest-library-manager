//! Keeping the Steam sign-in across restarts. The sign-in window leaves two things: short-lived session cookies (about 24 hours) and a refresh token (`steamRefresh_steam`, about 200 days).
//! The refresh token is what a browser uses to stay signed in: Steam trades it for fresh session cookies for each of its sites. Zest saves only the refresh token, in the system keyring
//! (`secret-tool` on Linux, the DPAPI vault on Windows, the Keychain on macOS), never in `config.json` and never in the log, and trades it for new sessions when one is needed.
use crate::net::Http;
use std::time::Duration;

/// Where the refresh token is kept. The app uses the keyring; tests use a plain map.
pub trait SecretStore: Send + Sync {
    fn get(&self, steam_id: &str) -> Option<String>;
    fn put(&self, steam_id: &str, token: &str) -> bool;
    fn forget(&self, steam_id: &str);
}

/// Keeps nothing (tests, and systems without a keyring).
pub struct NoSecrets;
impl SecretStore for NoSecrets { fn get(&self, _: &str) -> Option<String> { None } fn put(&self, _: &str, _: &str) -> bool { false } fn forget(&self, _: &str) {} }

/// The system keyring through `secret-tool` (libsecret), the same store KDE Wallet and GNOME Keyring both answer. The token travels over the program's stdin, never its arguments.
pub struct SecretTool;
impl SecretTool {
    /// Windows and macOS keep the token in the shared vault (DPAPI file, Keychain) under this name; the id must be digits so it can not make a odd file name.
    fn vault_name(steam_id: &str) -> Option<String> { (!steam_id.is_empty() && steam_id.bytes().all(|b| b.is_ascii_digit())).then(|| format!("steam-refresh/{steam_id}")) }
    fn have() -> bool { std::process::Command::new("secret-tool").arg("--version").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().is_ok() }
}
impl SecretStore for SecretTool {
    fn get(&self, steam_id: &str) -> Option<String> {
        let vault = || Self::vault_name(steam_id).and_then(|n| crate::secrets::current()?.get(&n));
        if !cfg!(target_os = "linux") { return vault(); }
        let direct = Self::have().then(|| std::process::Command::new("secret-tool").args(["lookup", "service", "zest", "kind", "steam-refresh", "steamid", steam_id]).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).output().ok()).flatten()
            .and_then(|out| { let t = String::from_utf8_lossy(&out.stdout).trim().to_string(); (out.status.success() && !t.is_empty()).then_some(t) });
        // no keyring: the vault's fallback (a file or memory, as the user chose) may hold it
        direct.or_else(vault)
    }
    fn put(&self, steam_id: &str, token: &str) -> bool {
        use std::io::Write;
        if !cfg!(target_os = "linux") { return match (Self::vault_name(steam_id), crate::secrets::current()) { (Some(n), Some(v)) => v.put(&n, token), _ => false }; }
        let fallback = || match (Self::vault_name(steam_id), crate::secrets::current()) { (Some(n), Some(v)) => v.put(&n, token), _ => false };
        if !Self::have() { return fallback(); }
        let Ok(mut child) = std::process::Command::new("secret-tool").args(["store", "--label", "Zest: Steam sign-in", "service", "zest", "kind", "steam-refresh", "steamid", steam_id])
            .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn() else { return false };
        if let Some(mut si) = child.stdin.take() { let _ = si.write_all(token.as_bytes()); }
        if child.wait().map(|s| s.success()).unwrap_or(false) { return true; }
        fallback()
    }
    fn forget(&self, steam_id: &str) {
        if let (Some(n), Some(v)) = (Self::vault_name(steam_id), crate::secrets::current()) { v.forget(&n); }
        if !cfg!(target_os = "linux") || !Self::have() { return; }
        let _ = std::process::Command::new("secret-tool").args(["clear", "service", "zest", "kind", "steam-refresh", "steamid", steam_id]).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
    }
}

/// The refresh token's JWT part from a cookie value (`<steamid>||<jwt>`, the bars percent-encoded or not).
pub fn jwt_of(cookie: &str) -> String { cookie.replace("%7C", "|").replace("%7c", "|").rsplit("||").next().unwrap_or("").trim().to_string() }

/// What a renewal brought back: the session cookie values for the sites Zest reads.
#[derive(Debug, PartialEq, Default)]
pub struct Renewed { pub steam_id: String, pub help: Option<String>, pub community: Option<String>, /// The store's session (the owned-apps list and the licenses page).
    pub store: Option<String> }

fn sessionid() -> String {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(1) ^ ((std::process::id() as u128) << 64);
    format!("{:024x}", n & ((1u128 << 96) - 1))
}

/// Trades a refresh token for new session cookies, the way Steam's own pages do: `jwt/finalizelogin` takes the token and answers with one transfer per Steam site, and posting each
/// transfer to its site's `settoken` makes that site set `steamLoginSecure`. Err carries a message for the log; the token is never part of it.
pub fn renew(http: &dyn Http, refresh_token: &str) -> Result<Renewed, String> {
    let jwt = jwt_of(refresh_token);
    if jwt.is_empty() { return Err("no refresh token".into()); }
    let sid = sessionid();
    let origin = [("Origin", "https://steamcommunity.com"), ("Referer", "https://steamcommunity.com/")];
    let cookie = format!("sessionid={sid}");
    let fin = http.post_form_raw("https://login.steampowered.com/jwt/finalizelogin", &origin, &[("nonce", jwt.as_str()), ("sessionid", sid.as_str()), ("redirect_uri", "https://steamcommunity.com/login/home/?goto=")], Duration::from_secs(25))
        .map_err(|e| format!("finalizelogin: {e}"))?;
    if fin.status != 200 { return Err(format!("finalizelogin answered {}", fin.status)); }
    let v: serde_json::Value = serde_json::from_str(&fin.body).map_err(|_| "finalizelogin gave no JSON".to_string())?;
    if let Some(e) = v.get("error").and_then(|e| e.as_str()) { return Err(format!("finalizelogin refused: {e}")); }
    let steam_id = v.get("steamID").and_then(|x| x.as_str()).map(String::from).or_else(|| v.get("steamID").and_then(|x| x.as_u64()).map(|n| n.to_string())).ok_or("finalizelogin gave no SteamID")?;
    let transfers = v.get("transfer_info").and_then(|t| t.as_array()).ok_or("finalizelogin gave no transfers")?;
    let mut out = Renewed { steam_id: steam_id.clone(), ..Default::default() };
    for t in transfers {
        let Some(url) = t.get("url").and_then(|u| u.as_str()) else { continue };
        let site = if url.contains("help.steampowered.com") { 1 } else if url.contains("steamcommunity.com") { 2 } else if url.contains("store.steampowered.com") { 3 } else { continue };
        let p = |k: &str| t.get("params").and_then(|p| p.get(k)).and_then(|x| x.as_str()).unwrap_or("").to_string();
        let (nonce, auth) = (p("nonce"), p("auth"));
        let headers = [("Origin", "https://steamcommunity.com"), ("Referer", "https://steamcommunity.com/"), ("Cookie", cookie.as_str())];
        let r = match http.post_form_raw(url, &headers, &[("steamID", steam_id.as_str()), ("nonce", nonce.as_str()), ("auth", auth.as_str())], Duration::from_secs(25)) { Ok(r) => r, Err(e) => { crate::net::log(&format!("steam session: settoken for site {site}: {e}")); continue } };
        let value = r.header_all("set-cookie").into_iter().find_map(|c| c.strip_prefix("steamLoginSecure=").map(|rest| rest.split(';').next().unwrap_or("").to_string())).filter(|c| !c.is_empty());
        crate::net::log(&format!("steam session: settoken for {} answered {}{}", match site { 1 => "the Help site", 2 => "the community site", _ => "the store" }, r.status, if value.is_some() { ", cookie set" } else { ", no cookie" }));
        match site { 1 => out.help = value, 2 => out.community = value, _ => out.store = value }
    }
    if out.help.is_none() && out.community.is_none() && out.store.is_none() { return Err("no site gave a session".into()); }
    Ok(out)
}

/// Saves the refresh token (keyring) for this account.
pub fn remember(store: &dyn SecretStore, steam_id: &str, refresh_cookie: &str) {
    let ok = store.put(steam_id, refresh_cookie);
    crate::net::log(&format!("steam session: the refresh token was {}", if !ok { "not saved; the sign-in lasts until Zest closes" } else if crate::secrets::no_keyring() { "kept without a keyring (a file or memory, as chosen in Settings > Account)" } else { "saved in the keyring" }));
}

/// Brings the session back after a restart (or a day's expiry): the saved refresh token is traded for new cookies, which become this run's sessions (`steamsignin`). True when a Help session exists after.
pub fn restore(http: &dyn Http, store: &dyn SecretStore, steam_id: &str) -> bool {
    let Some(token) = store.get(steam_id) else { return false };
    match renew(http, &token) {
        Ok(r) if r.steam_id == steam_id => {
            if let Some(h) = &r.help { crate::steamsignin::set_help_session(h); }
            if let Some(c) = &r.community { crate::steamsignin::set_community_session(c); }
            if let Some(c) = &r.store { crate::steamsignin::set_store_session(c); }
            crate::net::log(&format!("steam session: restored from the saved sign-in (Help {}, community {}, store {})", r.help.is_some(), r.community.is_some(), r.store.is_some()));
            r.help.is_some()
        }
        Ok(_) => { crate::net::log("steam session: the saved sign-in belongs to another account; forgotten"); store.forget(steam_id); false }
        Err(e) => { crate::net::log(&format!("steam session: could not restore ({e})")); false }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{NetError, Raw, Response};
    use std::sync::Mutex;
    #[derive(Default)] struct Mem(Mutex<std::collections::HashMap<String, String>>);
    impl SecretStore for Mem { fn get(&self, id: &str) -> Option<String> { self.0.lock().unwrap().get(id).cloned() } fn put(&self, id: &str, t: &str) -> bool { self.0.lock().unwrap().insert(id.into(), t.into()); true } fn forget(&self, id: &str) { self.0.lock().unwrap().remove(id); } }
    /// Steam, played by a script: finalizelogin answers with two transfers, each settoken sets a cookie named after its site.
    struct Steam { seen: Mutex<Vec<String>>, refuse: bool }
    impl Http for Steam {
        fn get(&self, _u: &str, _h: &[(&str, &str)], _t: Duration) -> Result<Response, NetError> { Err(NetError("no GET".into())) }
        fn post_form_raw(&self, url: &str, headers: &[(&str, &str)], form: &[(&str, &str)], _t: Duration) -> Result<Raw, NetError> {
            self.seen.lock().unwrap().push(url.to_string());
            let f = |k: &str| form.iter().find(|(a, _)| *a == k).map(|(_, v)| v.to_string()).unwrap_or_default();
            if url.ends_with("/jwt/finalizelogin") {
                assert_eq!(f("nonce"), "REFRESH.JWT.PART", "only the JWT goes as the nonce"); assert!(headers.iter().any(|(k, v)| *k == "Origin" && v.contains("steamcommunity")));
                if self.refuse { return Ok(Raw { status: 200, headers: vec![], body: r#"{"error":"AccessDenied"}"#.into() }); }
                return Ok(Raw { status: 200, headers: vec![], body: r#"{"steamID":"76561198000000001","transfer_info":[{"url":"https://steamcommunity.com/login/settoken","params":{"nonce":"n1","auth":"a1"}},{"url":"https://help.steampowered.com/login/settoken","params":{"nonce":"n2","auth":"a2"}},{"url":"https://store.steampowered.com/login/settoken","params":{"nonce":"n3","auth":"a3"}}]}"#.into() });
            }
            assert_eq!(f("steamID"), "76561198000000001"); assert!(headers.iter().any(|(k, v)| *k == "Cookie" && v.starts_with("sessionid=")), "the same sessionid goes to every site");
            let which = if url.contains("help.") { "HELP" } else if url.contains("store.") { "STORE" } else { "COMMUNITY" };
            Ok(Raw { status: 200, headers: vec![("set-cookie".into(), format!("steamLoginSecure=76561198000000001%7C%7C{which}-{}; Path=/; Secure; HttpOnly", f("auth"))), ("set-cookie".into(), "sessionid=x; Path=/".into())], body: "{}".into() })
        }
    }
    #[test] fn the_jwt_is_cut_out_of_the_cookie_value() { assert_eq!(jwt_of("76561198000000001%7C%7Ceyabc.def.ghi"), "eyabc.def.ghi"); assert_eq!(jwt_of("1||x.y.z"), "x.y.z"); assert_eq!(jwt_of(""), ""); }
    #[test] fn a_refresh_token_is_traded_for_a_session_on_each_site_and_the_other_sites_are_left_alone() {
        let s = Steam { seen: Mutex::new(vec![]), refuse: false };
        let r = renew(&s, "76561198000000001%7C%7CREFRESH.JWT.PART").unwrap();
        assert_eq!(r.steam_id, "76561198000000001"); assert_eq!(r.help.as_deref(), Some("76561198000000001%7C%7CHELP-a2")); assert_eq!(r.community.as_deref(), Some("76561198000000001%7C%7CCOMMUNITY-a1"));
        assert_eq!(r.store.as_deref(), Some("76561198000000001%7C%7CSTORE-a3"));
        assert!(!s.seen.lock().unwrap().iter().any(|u| !(u.contains("help.") || u.contains("store.") || u.contains("steamcommunity.com") || u.contains("login.steampowered.com"))), "only the three sites Zest reads are posted to");
        assert!(renew(&Steam { seen: Mutex::new(vec![]), refuse: true }, "1||REFRESH.JWT.PART").unwrap_err().contains("refused"));
        assert!(renew(&s, "").is_err());
    }
    #[test] fn restore_puts_the_new_sessions_in_place_only_for_the_same_account() {
        let store = Mem::default(); crate::steamsignin::clear_help_session();
        assert!(!restore(&Steam { seen: Mutex::new(vec![]), refuse: false }, &store, "76561198000000001"), "nothing saved: nothing restored");
        remember(&store, "76561198000000001", "76561198000000001%7C%7CREFRESH.JWT.PART");
        assert!(restore(&Steam { seen: Mutex::new(vec![]), refuse: false }, &store, "76561198000000001"));
        assert_eq!(crate::steamsignin::help_session().as_deref(), Some("76561198000000001%7C%7CHELP-a2"));
        assert_eq!(crate::steamsignin::store_session().as_deref(), Some("76561198000000001%7C%7CSTORE-a3"));
        // a token that answers for another account is dropped
        crate::steamsignin::clear_help_session(); store.put("76561198000000002", "1||REFRESH.JWT.PART");
        assert!(!restore(&Steam { seen: Mutex::new(vec![]), refuse: false }, &store, "76561198000000002")); assert!(store.get("76561198000000002").is_none());
    }
}
