//! What the account owns, from the store's own list (`store.steampowered.com/dynamicstore/userdata/`, `rgOwnedApps`) instead of the Web API. The Web API's `GetOwnedGames` leaves out
//! some games the account owns (19 on one library: free-to-keep and promotional licenses mostly), and it needs an API key; this list needs only the store session of the Steam sign-in.
//! It names every owned app, DLC, videos, music and tools included (4,748 apps for a library of 2,944 games), so `store_only` keeps just what `appinfo.vdf` says is a game.
use crate::net::Http;
use crate::populate::Candidate;
use crate::steamlocal::AppInfo;
use std::collections::{HashMap, HashSet};
use std::time::Duration;

/// The appids of `rgOwnedApps`. None when the answer is not the list (a signed-out visitor gets an empty one, which must not read as "owns nothing").
pub fn parse_owned_apps(body: &str) -> Option<Vec<i64>> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let ids: Vec<i64> = v.get("rgOwnedApps")?.as_array()?.iter().filter_map(|x| x.as_i64()).filter(|n| *n > 0).collect();
    (!ids.is_empty()).then_some(ids)
}

/// One request with the store session (`steamLoginSecure` value). Err = a line for the user.
pub fn fetch_owned_apps(http: &dyn Http, store_cookie: &str) -> Result<Vec<i64>, String> {
    let header = format!("steamLoginSecure={}", store_cookie.trim());
    let r = http.get("https://store.steampowered.com/dynamicstore/userdata/", &[("Cookie", header.as_str())], Duration::from_secs(30)).map_err(|_| "Could not reach the Steam store.".to_string())?;
    if r.status >= 400 { return Err(format!("The Steam store answered {}.", r.status)); }
    parse_owned_apps(&r.body).ok_or_else(|| "The Steam store did not give the owned-apps list (the sign-in may have expired).".to_string())
}

/// Owned apps the Web API list did not have, as candidates: only those `appinfo.vdf` knows as a game and whose title is not a beta, test server, tool and so on. An app with no appinfo
/// entry (or no type) is not taken here, since DLC outnumbers games 20 to 1; those ids are the second value, for `resolve_unknown`.
pub fn store_only(owned: &[i64], have: &[Candidate], appinfo: &HashMap<i64, AppInfo>) -> (Vec<Candidate>, Vec<i64>) {
    let known: HashSet<i64> = have.iter().map(|c| c.appid).collect();
    let (mut out, mut unknown) = (Vec::new(), Vec::new());
    for id in owned.iter().filter(|id| !known.contains(id)) {
        match appinfo.get(id) {
            None => unknown.push(*id),
            Some(a) if a.kind.is_empty() => unknown.push(*id),
            Some(a) if a.kind.eq_ignore_ascii_case("game") && !a.name.trim().is_empty() && !crate::playnite::looks_like_junk(&a.name) =>
                out.push(Candidate { appid: *id, name: a.name.clone(), playtime_forever: 0, last_played: None, icon_hash: String::new() }),
            Some(_) => {}
        }
    }
    (out, unknown)
}

/// At most this many untyped apps are looked up in the store (one request each); more means a machine with no Steam client, where they would all be untyped.
pub const MAX_UNKNOWN_LOOKUPS: usize = 100;

/// The untyped apps, one store request each (a second apart, stopping at the first 429): kept when the store calls it a game and the title is not junk. Apps the store has no page for
/// (delisted, internal) are left out.
pub fn resolve_unknown(http: &dyn Http, ids: &[i64]) -> (Vec<Candidate>, Vec<i64>) {
    if ids.len() > MAX_UNKNOWN_LOOKUPS { return (vec![], vec![]); }
    let (mut out, mut settled) = (Vec::new(), Vec::new());
    for (i, id) in ids.iter().enumerate() {
        if i > 0 { std::thread::sleep(Duration::from_millis(1000)); }
        let Ok(r) = http.get(&format!("https://store.steampowered.com/api/appdetails?appids={id}&l=english"), &[], Duration::from_secs(15)) else { continue };
        if r.status == 429 { crate::net::log("owned apps: the store asked to slow down; the rest of the untyped apps were not checked"); break; }
        if r.status >= 400 { continue; }
        settled.push(*id);   // the store answered: a game, not a game, or no page; either way asking again will not change it for a while
        // the store redirects some old ids to the app that replaced them (977941 answers with HITMAN 2's own page, 863550): that is a copy of a game the library has, not a new game
        let own_page = serde_json::from_str::<serde_json::Value>(&r.body).ok().and_then(|j| crate::steamweb::appdetails_entry(&j, *id).and_then(|e| e["data"]["steam_appid"].as_i64())).map_or(true, |a| a == *id);
        if !own_page { continue; }
        if let Some(d) = crate::steamweb::parse_appdetails(&r.body, *id) {
            if d.kind.eq_ignore_ascii_case("game") && !d.name.trim().is_empty() && !crate::playnite::looks_like_junk(&d.name) {
                out.push(Candidate { appid: *id, name: d.name, playtime_forever: 0, last_played: None, icon_hash: String::new() });
            }
        }
    }
    (out, settled)
}

/// Untyped apps already looked up (`owned_checked.json` next to the library): `{appid: unix time}`; an entry counts for `CHECKED_DAYS` days, then the store is asked again.
pub const CHECKED_DAYS: i64 = 30;
pub fn load_checked(path: &std::path::Path, now: i64) -> HashSet<i64> {
    let v: serde_json::Value = std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(serde_json::Value::Null);
    v.as_object().map(|o| o.iter().filter(|(_, t)| t.as_i64().map_or(false, |t| now - t < CHECKED_DAYS * 86400)).filter_map(|(k, _)| k.parse().ok()).collect()).unwrap_or_default()
}
/// Adds `ids` (settled just now) to the file, dropping entries that have expired. A failure to write only means asking again next time.
pub fn save_checked(path: &std::path::Path, ids: &[i64], now: i64) {
    let mut m: serde_json::Map<String, serde_json::Value> = std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).and_then(|v: serde_json::Value| v.as_object().cloned()).unwrap_or_default();
    m.retain(|_, t| t.as_i64().map_or(false, |t| now - t < CHECKED_DAYS * 86400));
    for id in ids { m.insert(id.to_string(), serde_json::json!(now)); }
    let _ = std::fs::write(path, serde_json::Value::Object(m).to_string());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{NetError, Response};
    fn info(name: &str, kind: &str) -> AppInfo { AppInfo { name: name.into(), kind: kind.into(), ..Default::default() } }
    struct Fake(u16, &'static str); impl Http for Fake { fn get(&self, u: &str, h: &[(&str, &str)], _t: Duration) -> Result<Response, NetError> {
        assert!(u.contains("dynamicstore/userdata") && h.iter().any(|(k, v)| *k == "Cookie" && v.starts_with("steamLoginSecure=")), "{u}");
        Ok(Response { status: self.0, retry_after: None, body: self.1.into() }) } }
    #[test] fn the_list_is_read_and_a_signed_out_answer_is_not_an_empty_library() {
        assert_eq!(parse_owned_apps(r#"{"rgOwnedApps":[10,0,-3,620],"rgWishlist":[1]}"#), Some(vec![10, 620]));
        assert_eq!(parse_owned_apps(r#"{"rgOwnedApps":[]}"#), None); assert_eq!(parse_owned_apps("{}"), None); assert_eq!(parse_owned_apps("<html>"), None);
        assert_eq!(fetch_owned_apps(&Fake(200, r#"{"rgOwnedApps":[7]}"#), "1%7C%7Cx").unwrap(), vec![7]);
        assert!(fetch_owned_apps(&Fake(200, r#"{"rgOwnedApps":[]}"#), "x").unwrap_err().contains("expired")); assert!(fetch_owned_apps(&Fake(503, ""), "x").unwrap_err().contains("503"));
    }
    struct Store; impl Http for Store { fn get(&self, u: &str, _h: &[(&str, &str)], _t: Duration) -> Result<Response, NetError> {
        let body = if u.contains("appids=1&") { r#"{"1":{"success":true,"data":{"steam_appid":1,"type":"game","name":"Real Game"}}}"# } else if u.contains("appids=2&") { r#"{"2":{"success":true,"data":{"steam_appid":2,"type":"dlc","name":"Pack"}}}"# }
            else if u.contains("appids=3&") { r#"{"3":{"success":true,"data":{"steam_appid":3,"type":"game","name":"Thing - Beta"}}}"# } else if u.contains("appids=5&") { r#"{"5":{"success":true,"data":{"steam_appid":99,"type":"game","name":"Redirected Copy"}}}"# } else { r#"{"4":{"success":false}}"# };
        Ok(Response { status: 200, retry_after: None, body: body.into() }) } }
    #[test] fn untyped_apps_are_kept_only_when_the_store_calls_them_a_game() {
        let (got, settled) = resolve_unknown(&Store, &[1, 2, 3, 4, 5]);
        assert_eq!(settled, vec![1, 2, 3, 4, 5], "every answer counts as settled; id 5 answers with another app's page (a redirect) and is not taken");
        assert_eq!(got.iter().map(|c| (c.appid, c.name.as_str())).collect::<Vec<_>>(), vec![(1, "Real Game")]);
        assert!(resolve_unknown(&Store, &(0..=MAX_UNKNOWN_LOOKUPS as i64).collect::<Vec<_>>()).0.is_empty(), "too many: none are looked up");
    }
    #[test] fn settled_apps_are_remembered_for_a_month() {
        let d = std::env::temp_dir().join(format!("zest_checked_{}.json", std::process::id())); let _ = std::fs::remove_file(&d);
        assert!(load_checked(&d, 1_000_000).is_empty());
        save_checked(&d, &[5, 6], 1_000_000); save_checked(&d, &[7], 1_000_100);
        assert_eq!(load_checked(&d, 1_000_200), [5, 6, 7].into_iter().collect());
        assert_eq!(load_checked(&d, 1_000_100 + CHECKED_DAYS * 86400 + 1), HashSet::new(), "all expired");
        save_checked(&d, &[8], 1_000_100 + CHECKED_DAYS * 86400 + 5);   // the old ones are dropped when the file is written again
        assert_eq!(load_checked(&d, 1_000_100 + CHECKED_DAYS * 86400 + 6), [8].into_iter().collect()); let _ = std::fs::remove_file(&d);
    }
    #[test] fn only_games_the_web_api_missed_are_taken() {
        let have = vec![Candidate { appid: 1, name: "Have".into(), playtime_forever: 0, last_played: None, icon_hash: String::new() }];
        let appinfo: HashMap<i64, AppInfo> = [(1, info("Have", "Game")), (2, info("Missed Game", "Game")), (3, info("Some DLC", "DLC")), (4, info("Paladins - Public Test", "Game")), (5, info("Trailer", "Video")), (6, info("Tool", ""))].into_iter().collect();
        let (out, unknown) = store_only(&[1, 2, 3, 4, 5, 6, 7], &have, &appinfo);
        assert_eq!(out.iter().map(|c| (c.appid, c.name.as_str())).collect::<Vec<_>>(), vec![(2, "Missed Game")]);
        assert_eq!(unknown, vec![6, 7], "no type (6) and no entry (7)");
    }
}
