//! Sign in with Steam: Steam's own login page in the sign-in window (`zest-login`), so the password is typed on Steam's page and never reaches Zest.
//! What comes back is the `steamLoginSecure` session cookie, whose value starts with the account's SteamID64 (`7656...%7C%7C<token>`). Zest uses it for two things and
//! keeps neither: the SteamID, and (when the account already has one) the Web API key shown on steamcommunity.com/dev/apikey. The cookie lives in memory for those
//! two steps only. It is never written to disk and never logged.
use std::time::Duration;
use crate::net::Http;

/// What the sign-in is for. Steam's two login pages set different sessions: the community login (where the API key page lives) gives the community session, the Help site's login gives
/// the Help session that Date Added reads. Each purpose starts on the page that gives it what it needs, and keeps the other session when it happens to come along.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Purpose { /// SteamID and API key (Settings > Account, the first-run window).
    Account, /// The Help pages' purchase dates (Bulk Ops, Edit Game).
    Dates }

pub const LOGIN_URL_COMMUNITY: &str = "https://steamcommunity.com/login/home/?goto=%2Fdev%2Fapikey";
pub const LOGIN_URL_HELP: &str = "https://help.steampowered.com/en/wizard/Login?redirect=%2Fen%2Fwizard%2FHelpWithGame%2F";
pub const HELP_URL: &str = "https://help.steampowered.com/";
pub const COMMUNITY_URL: &str = "https://steamcommunity.com/";
pub const API_KEY_URL: &str = "https://steamcommunity.com/dev/apikey";
pub const LOGIN_DOMAIN_URL: &str = "https://login.steampowered.com/";   // where the long-lived refresh cookie (`steamRefresh_steam`) is set

/// The `done` spec for `weblogin::run`. Both purposes sign in on the Help site's login: it is the one that hands back the long-lived refresh token (the community login does not), and the
/// refresh token can be traded for a community session afterwards (`steamsession::renew`), which is what the API key page needs. The Help session is required, the rest is given a second.
pub fn done_spec() -> serde_json::Value {
    serde_json::json!({ "kind": "jars", "url_not_contains": "/Login|/login", "grace_secs": 1, "jars": [
        { "url": HELP_URL, "names": ["steamLoginSecure"], "optional": false },
        { "url": COMMUNITY_URL, "names": ["steamLoginSecure"], "optional": true },
        { "url": LOGIN_DOMAIN_URL, "names": ["steamRefresh_steam"], "optional": true, "key": "refresh_login" },
        { "url": COMMUNITY_URL, "names": ["steamRefresh_steam"], "optional": true, "key": "refresh_community" },
        { "url": HELP_URL, "names": ["steamRefresh_steam"], "optional": true, "key": "refresh_help" } ] })
}

/// The Help site's session, kept in memory for this run of Zest only (never written anywhere): the Date Added reader uses it to read Steam's Help pages as the signed-in player.
static HELP_SESSION: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
pub fn help_session() -> Option<String> { HELP_SESSION.lock().ok().and_then(|g| g.clone()) }
pub fn set_help_session(cookie: &str) { if let Ok(mut g) = HELP_SESSION.lock() { *g = Some(cookie.trim().to_string()); } }
pub fn clear_help_session() { if let Ok(mut g) = HELP_SESSION.lock() { *g = None; } }

/// The community session of the last sign-in, in memory only, so Date Added can try it on the Help site without a second sign-in.
static COMMUNITY_SESSION: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
pub fn community_session() -> Option<String> { COMMUNITY_SESSION.lock().ok().and_then(|g| g.clone()) }
pub fn set_community_session(cookie: &str) { if let Ok(mut g) = COMMUNITY_SESSION.lock() { *g = Some(cookie.trim().to_string()); } }

/// The store's session (`store.steampowered.com`: the owned-apps list, the licenses page), in memory only like the others; `steamsession::renew` is what brings it.
static STORE_SESSION: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
pub fn store_session() -> Option<String> { STORE_SESSION.lock().ok().and_then(|g| g.clone()) }
pub fn set_store_session(cookie: &str) { if let Ok(mut g) = STORE_SESSION.lock() { *g = Some(cookie.trim().to_string()); } }
pub fn clear_store_session() { if let Ok(mut g) = STORE_SESSION.lock() { *g = None; } }

/// A Help page as Steam shows it to someone who is not signed in: the same address answers 200 with an inline sign-in form (a "wizard/Login" link) and no account content. Checked by
/// content, because there is no redirect to go by.
pub fn help_page_signed_out(html: &str) -> bool { html.contains("wizard/Login") && !html.contains("LineItemRow") && !html.contains("account_details") }

/// Does the Help site accept this `steamLoginSecure` value? One request for a Help page: Steam sends a signed-out visitor to its sign-in page, so landing anywhere else means yes.
/// (One sign-in gives one token; whether the Help site takes the community site's is decided by Steam, so this asks instead of assuming.)
pub fn help_accepts(http: &dyn Http, cookie: &str) -> bool {
    let header = format!("steamLoginSecure={}", cookie.trim());
    match http.get_final("https://help.steampowered.com/en/wizard/HelpWithGame/?appid=440", &[("Cookie", header.as_str())], Duration::from_secs(20)) {
        Ok((r, end)) => { let ok = r.status == 200 && !end.contains("/Login") && !end.contains("/login") && !help_page_signed_out(&r.body); crate::net::log(&format!("steam sign-in: the Help site {} the community session (status {}, {} bytes)", if ok { "accepted" } else { "did not accept" }, r.status, r.body.len())); ok }
        Err(_) => false,
    }
}

/// Tries the community session of the last sign-in on the Help site; when it is accepted it becomes the Help session (`help_session`) and this says so.
pub fn adopt_community_for_help(http: &dyn Http) -> bool {
    let Some(c) = community_session() else { return false };
    if help_accepts(http, &c) { set_help_session(&c); true } else { false }
}

/// The refresh token the window found (the login domain's cookie, else the one on the community or Help site).
pub fn refresh_of(answer: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(answer).ok()?;
    ["refresh_login", "refresh_community", "refresh_help"].iter().find_map(|k| v.get(*k).and_then(|x| x.as_str()).filter(|x| !x.is_empty()).map(String::from))
}

/// The `exp` claim (seconds since 1970) of the JWT inside a Steam cookie value (`<steamid>||<jwt>`, the bars percent-encoded or not), or None. Only the expiry is read, nothing else is kept.
pub fn jwt_exp(cookie: &str) -> Option<i64> {
    let decoded = cookie.replace("%7C", "|").replace("%7c", "|");
    let payload = decoded.rsplit("||").next()?.split('.').nth(1)?;
    let bytes = b64url_decode(payload)?;
    serde_json::from_slice::<serde_json::Value>(&bytes).ok()?.get("exp")?.as_i64()
}
fn b64url_decode(s: &str) -> Option<Vec<u8>> {
    let (mut out, mut buf, mut bits) = (Vec::new(), 0u32, 0u32);
    for c in s.bytes() {
        let v = match c { b'A'..=b'Z' => c - b'A', b'a'..=b'z' => c - b'a' + 26, b'0'..=b'9' => c - b'0' + 52, b'-' | b'+' => 62, b'_' | b'/' => 63, b'=' => continue, _ => return None };
        buf = (buf << 6) | v as u32; bits += 6;
        if bits >= 8 { bits -= 8; out.push((buf >> bits) as u8); buf &= (1 << bits) - 1; }
    }
    Some(out)
}
/// "23 hours" or "198 days": how long until `exp`, for the log.
pub fn lifetime_text(exp: i64, now: i64) -> String {
    let secs = exp - now;
    if secs <= 0 { "already expired".into() } else if secs < 2 * 86400 { format!("{} hours", secs / 3600) } else { format!("{} days", secs / 86400) }
}

/// The window's answer (`{"<url>": "<cookie>"}`): the Help and community sessions, whichever came.
pub fn parse_jars(answer: &str) -> Result<(Option<String>, Option<String>), String> {
    let v: serde_json::Value = serde_json::from_str(answer).map_err(|_| "The sign-in window gave an answer Zest could not read.".to_string())?;
    let get = |u: &str| v.get(u).and_then(|x| x.as_str()).filter(|x| !x.is_empty()).map(String::from);
    Ok((get(HELP_URL), get(COMMUNITY_URL)))
}

/// The SteamID64 inside a `steamLoginSecure` value (percent-encoded `||` after it), or None when it does not start with 17 digits.
pub fn steam_id_from_cookie(value: &str) -> Option<String> {
    let id: String = value.trim().chars().take_while(|c| c.is_ascii_digit()).collect();
    (id.len() == 17 && id.starts_with("7656")).then_some(id)
}

#[derive(Debug, PartialEq)]
pub enum ApiKeyPage {
    /// The account's key.
    Key(String),
    /// Signed in, but no key is registered yet (Steam asks for a domain name to register one).
    NoKey,
    /// Steam refuses keys to this account (a limited account: it has not spent $5 yet).
    Limited,
    /// The page was not the key page (the session was not accepted, or Steam changed the page).
    Unknown,
}

/// What `steamcommunity.com/dev/apikey` says. The page lists `Key: <32 hex digits>` for an account that has one.
pub fn parse_api_key_page(html: &str) -> ApiKeyPage {
    let text = html.replace("&nbsp;", " ");
    if let Some(i) = text.find("Key:") {
        let tail = &text[i + 4..];
        let key: String = tail.trim_start().chars().take_while(|c| c.is_ascii_hexdigit()).collect();
        if key.len() == 32 { return ApiKeyPage::Key(key.to_uppercase()); }
    }
    let low = text.to_lowercase();
    if low.contains("limited user account") || low.contains("access denied") || low.contains("must have purchased") { return ApiKeyPage::Limited; }
    if low.contains("register for a new steam web api key") || low.contains("domain name") { return ApiKeyPage::NoKey; }
    ApiKeyPage::Unknown
}

/// Asks Steam for the account's key with the session cookie from the sign-in. Err carries a message for the player.
pub fn fetch_api_key(http: &dyn Http, cookie: &str) -> Result<ApiKeyPage, String> {
    let header = format!("steamLoginSecure={}", cookie.trim());
    let r = http.get(API_KEY_URL, &[("Cookie", header.as_str())], Duration::from_secs(20)).map_err(|e| format!("Could not reach Steam: {e}"))?;
    if r.status != 200 { return Err(format!("Steam answered {} for the key page.", r.status)); }
    Ok(parse_api_key_page(&r.body))
}

/// The display name in a public profile's XML (`steamcommunity.com/profiles/<id>/?xml=1`): `<steamID><![CDATA[Name]]></steamID>`. None for a private or missing profile (the page then has no name).
pub fn parse_profile_name(xml: &str) -> Option<String> {
    let i = xml.find("<steamID>")? + "<steamID>".len();
    let j = xml[i..].find("</steamID>")? + i;
    let raw = xml[i..j].trim();
    let name = raw.strip_prefix("<![CDATA[").and_then(|r| r.strip_suffix("]]>")).unwrap_or(raw).trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// The account's display name from its public profile, or None when it cannot be read (private profile, no answer): the caller then leaves the label as it is.
pub fn fetch_profile_name(http: &dyn Http, steam_id: &str) -> Option<String> {
    if steam_id.is_empty() || !steam_id.chars().all(|c| c.is_ascii_digit()) { return None; }
    let url = format!("https://steamcommunity.com/profiles/{steam_id}/?xml=1");
    // two tries: an answer of 429 or 5xx straight after the sign-in is tried once more a moment later (what Steam said is logged either way, since the cause was never certain)
    for attempt in 0..2 {
        let r = match http.get_raw(&url, &[], Duration::from_secs(15)) { Ok(r) => r, Err(e) => { crate::net::log(&format!("steam sign-in: the profile name could not be read: {e}")); return None; } };
        if r.status == 200 {
            let name = parse_profile_name(&r.body);
            crate::net::log(&match &name { Some(_) => "steam sign-in: read the display name from the public profile".to_string(), None => format!("steam sign-in: the profile page had no name ({} bytes: {})", r.body.len(), r.body.chars().take(120).collect::<String>().replace('\n', " ")) });
            return name;
        }
        let hdr = |k: &str| r.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(k)).map(|(_, v)| v.clone()).unwrap_or_default();
        crate::net::log(&format!("steam sign-in: the profile page for {steam_id} answered {} (retry-after {:?}, location {:?}, attempt {})", r.status, hdr("retry-after"), hdr("location"), attempt + 1));
        if attempt == 0 && (r.status == 429 || r.status >= 500) { std::thread::sleep(Duration::from_secs(3)); continue; }
        return None;
    }
    None
}

/// `personaname` from `ISteamUser/GetPlayerSummaries/v2` (`{"response": {"players": [{"personaname": "..."}]}}`).
pub fn parse_player_summaries(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let n = v.get("response")?.get("players")?.as_array()?.first()?.get("personaname")?.as_str()?.trim();
    (!n.is_empty()).then(|| n.to_string())
}

/// The display name through the Web API with the key the sign-in just found: the fallback for when the profile page is throttled (HTTP 429) or private. Nothing about the request is logged (the key is in its address).
pub fn fetch_name_with_key(http: &dyn Http, key: &str, steam_id: &str) -> Option<String> {
    if key.is_empty() || steam_id.is_empty() || !steam_id.chars().all(|c| c.is_ascii_digit()) { return None; }
    let r = http.get(&format!("https://api.steampowered.com/ISteamUser/GetPlayerSummaries/v2/?key={key}&steamids={steam_id}"), &[], Duration::from_secs(15)).ok()?;
    if r.status != 200 { crate::net::log(&format!("steam sign-in: the Web API answered {} for the display name", r.status)); return None; }
    parse_player_summaries(&r.body)
}

/// The whole sign-in: the window (Steam's Help login), then the SteamID, the saved refresh token, and (for `Purpose::Account`) the key, read with a community session that comes from the
/// window or, failing that, from trading the refresh token. Blocks until the window is done; call it off the UI thread.
/// Returns (SteamID, key page result) and keeps the sessions in memory (`help_session`, `community_session`). The cookies never leave this module.
pub fn sign_in(helper: &std::path::Path, http: &dyn Http, purpose: Purpose) -> Result<(String, ApiKeyPage), String> {
    let title = match purpose { Purpose::Account => "Sign in with Steam", Purpose::Dates => "Sign in with Steam to read purchase dates" };
    let answer = crate::weblogin::run(helper, title, LOGIN_URL_HELP, &done_spec(), 600)?;
    let (help, mut community) = parse_jars(&answer)?;
    let primary = help.as_ref().ok_or("Steam signed you in but gave no session. Try again, or type the SteamID in.")?;
    let id = steam_id_from_cookie(primary).ok_or("Steam signed you in but the answer did not hold your SteamID. Type it in instead.")?;
    let refresh = refresh_of(&answer);
    // what the sign-in really gave (durations only, never a token): how long the session cookie lasts and whether a long-lived refresh token came back
    {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        let life = |c: &str| jwt_exp(c).map(|e| lifetime_text(e, now)).unwrap_or_else(|| "an unreadable time".into());
        crate::net::log(&format!("steam sign-in: the session cookie is valid for {}", life(primary)));
        match &refresh { Some(r) => crate::net::log(&format!("steam sign-in: a refresh token came back, valid for {}", life(r))), None => crate::net::log("steam sign-in: no refresh token came back") }
    }
    if let Some(r) = &refresh { crate::steamsession::remember(&crate::steamsession::SecretTool, &id, r); }
    set_help_session(primary);
    // the key page wants a community session: use the one the window had, else trade the refresh token for one
    if purpose == Purpose::Account && community.is_none() {
        if let Some(r) = &refresh { match crate::steamsession::renew(http, r) { Ok(n) => { community = n.community; if let Some(c) = &n.store { set_store_session(c); } } Err(e) => crate::net::log(&format!("steam sign-in: no community session from the refresh token ({e})")) } }
    }
    if let Some(c) = &community { set_community_session(c); }
    let page = if purpose == Purpose::Account { community.and_then(|c| fetch_api_key(http, &c).ok()).unwrap_or(ApiKeyPage::Unknown) } else { ApiKeyPage::Unknown };
    Ok((id, page))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn the_steam_id_is_read_from_the_cookie() {
        assert_eq!(steam_id_from_cookie("76561198054814426%7C%7CeyAidHlwIjogIkpXVCIs"), Some("76561198054814426".into()));
        assert_eq!(steam_id_from_cookie("  76561198054814426||abc "), Some("76561198054814426".into()));
        assert_eq!(steam_id_from_cookie("1234%7C%7Cabc"), None);
        assert_eq!(steam_id_from_cookie("garbage"), None);
        assert_eq!(steam_id_from_cookie(""), None);
        assert_eq!(steam_id_from_cookie("12345678901234567%7C%7Cx"), None, "17 digits but not a SteamID64");
    }
    #[test] fn the_key_page_is_understood() {
        assert_eq!(parse_api_key_page("<h2>Your Steam Web API Key</h2><p>Key: 0123456789abcdef0123456789ABCDEF</p><p>Domain Name: localhost</p>"), ApiKeyPage::Key("0123456789ABCDEF0123456789ABCDEF".into()));
        assert_eq!(parse_api_key_page("<p>Key:&nbsp;0123456789ABCDEF0123456789ABCDEF</p>"), ApiKeyPage::Key("0123456789ABCDEF0123456789ABCDEF".into()));
        assert_eq!(parse_api_key_page("<h2>Register for a new Steam Web API Key</h2><input name=domain> Domain Name"), ApiKeyPage::NoKey);
        assert_eq!(parse_api_key_page("<p>Limited user accounts are not allowed to register a key</p> <b>limited user account</b>"), ApiKeyPage::Limited);
        assert_eq!(parse_api_key_page("<html>Sign in</html>"), ApiKeyPage::Unknown);
        assert_eq!(parse_api_key_page("Key: 123"), ApiKeyPage::Unknown, "too short to be a key");
    }
    #[test] fn one_sign_in_on_the_help_site_serves_both_purposes() {
        let d = done_spec(); assert_eq!(d["kind"], "jars");
        assert_eq!((d["jars"][0]["url"].as_str(), d["jars"][0]["optional"].as_bool()), (Some(HELP_URL), Some(false)), "the Help session is required");
        assert_eq!(d["jars"][1]["optional"].as_bool(), Some(true), "the community session is a bonus that may come with it");
        assert!(LOGIN_URL_HELP.starts_with(HELP_URL));
    }
    #[test] fn the_window_answer_is_split_into_the_two_sessions() {
        assert_eq!(parse_jars(r#"{"https://help.steampowered.com/":"H","https://steamcommunity.com/":"C"}"#).unwrap(), (Some("H".to_string()), Some("C".to_string())));
        assert_eq!(parse_jars(r#"{"https://help.steampowered.com/":"H"}"#).unwrap(), (Some("H".to_string()), None));
        assert_eq!(parse_jars(r#"{"https://steamcommunity.com/":"C"}"#).unwrap(), (None, Some("C".to_string())));
        assert!(parse_jars("nonsense").is_err());
    }
    struct Fake { end: &'static str, status: u16, body: &'static str }
    impl Http for Fake {
        fn get(&self, _u: &str, _h: &[(&str, &str)], _t: Duration) -> Result<crate::net::Response, crate::net::NetError> { Ok(crate::net::Response { status: self.status, retry_after: None, body: self.body.to_string() }) }
        fn get_final(&self, u: &str, h: &[(&str, &str)], t: Duration) -> Result<(crate::net::Response, String), crate::net::NetError> {
            assert!(h.iter().any(|(k, v)| *k == "Cookie" && v.starts_with("steamLoginSecure=")), "the session goes along"); let _ = u; self.get(u, h, t).map(|r| (r, self.end.to_string()))
        }
    }
    #[test] fn the_help_site_taking_the_community_session_means_no_second_sign_in() {
        assert!(help_accepts(&Fake { end: "https://help.steampowered.com/en/wizard/HelpWithGame/?appid=440", status: 200, body: "<div class=\"account_details\">x</div>" }, "7656119800000000%7C%7Cx"));
        assert!(!help_accepts(&Fake { end: "https://help.steampowered.com/en/wizard/Login?redirect=x", status: 200, body: "" }, "7656119800000000%7C%7Cx"), "sent to the sign-in page: not accepted");
        assert!(!help_accepts(&Fake { end: "https://help.steampowered.com/en/wizard/HelpWithGame/?appid=440", status: 403, body: "" }, "7656119800000000%7C%7Cx"));
        assert!(!help_accepts(&Fake { end: "https://help.steampowered.com/en/wizard/HelpWithGame/?appid=440", status: 200, body: "<a href=\"/en/wizard/Login\">Sign in</a>" }, "7656119800000000%7C%7Cx"), "the same address with a sign-in form is a signed-out page");
        set_community_session("7656119800000000%7C%7Cx"); clear_help_session();
        assert!(adopt_community_for_help(&Fake { end: "https://help.steampowered.com/en/", status: 200, body: "<div class=\"LineItemRow\"></div>" }));
        assert_eq!(help_session().as_deref(), Some("7656119800000000%7C%7Cx"));
        clear_help_session();
        assert!(!adopt_community_for_help(&Fake { end: "https://help.steampowered.com/en/wizard/Login", status: 200, body: "" })); assert!(help_session().is_none());
    }
    #[test] fn a_signed_out_help_page_is_told_from_one_with_account_content() {
        assert!(help_page_signed_out("<html><a href=\"/en/wizard/Login?redirect=x\">Sign in</a></html>"));
        assert!(!help_page_signed_out("<a href=\"/en/wizard/Login\">x</a><div class=\"LineItemRow\"></div>"), "purchase rows mean signed in");
        assert!(!help_page_signed_out("<div class=\"account_details\">Activated:</div>"));
        assert!(!help_page_signed_out("<html>just a page</html>"));
    }
    #[test] fn a_cookies_expiry_is_read_without_keeping_anything_else() {
        // header {"alg":"EdDSA","typ":"JWT"} . payload {"iss":"r:1","exp":1800000000} . signature
        let payload = "eyJpc3MiOiJyOjEiLCJleHAiOjE4MDAwMDAwMDB9";
        assert_eq!(jwt_exp(&format!("76561198000000001%7C%7CeyJhbGciOiJFZERTQSJ9.{payload}.sig")), Some(1_800_000_000));
        assert_eq!(jwt_exp(&format!("76561198000000001||eyJhbGciOiJFZERTQSJ9.{payload}.sig")), Some(1_800_000_000));
        assert_eq!(jwt_exp("garbage"), None); assert_eq!(jwt_exp("1%7C%7Ca.!!!.c"), None);
        assert_eq!(lifetime_text(1_000_000 + 3 * 3600, 1_000_000), "3 hours"); assert_eq!(lifetime_text(1_000_000 + 200 * 86400, 1_000_000), "200 days"); assert_eq!(lifetime_text(5, 10), "already expired");
    }
    #[test] fn the_refresh_token_is_taken_from_whichever_site_set_it() {
        assert_eq!(refresh_of(r#"{"refresh_community":"C","refresh_help":"H"}"#).as_deref(), Some("C"));
        assert_eq!(refresh_of(r#"{"refresh_login":"L","refresh_help":"H"}"#).as_deref(), Some("L"));
        assert_eq!(refresh_of(r#"{"https://help.steampowered.com/":"x"}"#), None); assert_eq!(refresh_of("nonsense"), None);
        let spec = done_spec(); let keys: Vec<&str> = spec["jars"].as_array().unwrap().iter().filter_map(|j| j["key"].as_str()).collect();
        assert_eq!(keys, vec!["refresh_login", "refresh_community", "refresh_help"], "every refresh jar has its own answer key");
    }
}

#[cfg(test)]
mod profile_name_tests {
    use super::{parse_profile_name, parse_player_summaries};
    #[test] fn the_name_comes_from_the_web_api_answer() {
        assert_eq!(parse_player_summaries(r#"{"response":{"players":[{"steamid":"1","personaname":"Robby"}]}}"#).as_deref(), Some("Robby"));
        assert_eq!(parse_player_summaries(r#"{"response":{"players":[]}}"#), None);
        assert_eq!(parse_player_summaries("not json"), None);
    }
    #[test] fn the_name_comes_from_the_profile_xml() {
        assert_eq!(parse_profile_name("<profile><steamID64>1</steamID64><steamID><![CDATA[Robby <3 & co]]></steamID></profile>").as_deref(), Some("Robby <3 & co"));
        assert_eq!(parse_profile_name("<profile><steamID>Plain</steamID></profile>").as_deref(), Some("Plain"));
        assert_eq!(parse_profile_name("<profile><steamID><![CDATA[  ]]></steamID></profile>"), None);
        assert_eq!(parse_profile_name("<response><error><![CDATA[The specified profile could not be found.]]></error></response>"), None);
    }
}
