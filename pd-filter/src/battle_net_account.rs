//! Battle.net's signed-in account sync (PlayDate's `plugins/battle_net/account.py`). Blizzard has no owned-games API, so this replays the account.battle.net session
//! cookies of a real login against the same JSON endpoints the account website calls: `/api/games-and-subs` (title ids), `/api/transactions` (purchase dates by title),
//! `/api/` (still signed in?) and `/api/details` (battletag). The cookies come from the sign-in window's whole-jar mode (`zest-login`, `names: ["*"]`) and are kept as a
//! JSON object under the `battle_net` secrets (`web_cookies`, `battletag`).
//!
//! Failures never mean "owns nothing": an unreadable or expired answer gives an empty result and the caller keeps what the local files found.
//! The jar is only ever sent to battle.net hosts, including across redirects.
use crate::battle_net_local as bn;
use crate::net::{Http, Raw};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

pub const BASE: &str = "https://account.battle.net";
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";
const HOPS: usize = 8;

/// What the sign-in window waits for: the address back at the bare account root (the login form lives on `us.account.battle.net`, the start page `/api/logout` is on the root
/// too, hence the exclusion) and the session cookie (`BA-...`), not the anonymous ones every visitor has.
pub fn login_done() -> serde_json::Value {
    serde_json::json!({ "kind": "cookies", "url": format!("{BASE}/"), "names": ["*"], "url_contains": "://account.battle.net/", "url_not_contains": "/api/|/callback", "probe_contains": "\"authenticated\":true" })
}
/// The address the sign-in window opens: it clears any stale session so the next page is Blizzard's real login.
pub fn login_url() -> String { format!("{BASE}/api/logout") }

// ------------------------------------------------------------------ the saved login

pub fn saved_cookies(dir: &Path) -> Option<BTreeMap<String, String>> {
    let raw = crate::account::plugin_secret_get(dir, "battle_net", "web_cookies")?;
    let m: BTreeMap<String, String> = serde_json::from_str(&raw).ok()?;
    (!m.is_empty()).then_some(m)
}
pub fn saved_battletag(dir: &Path) -> Option<String> { crate::account::plugin_secret_get(dir, "battle_net", "battletag").filter(|t| !t.is_empty()) }
pub fn clear(dir: &Path) -> Result<(), String> {
    for k in ["web_cookies", "battletag"] { crate::account::plugin_secret_set(dir, "battle_net", k, None).map_err(|_| "Could not save".to_string())?; }
    Ok(())
}
/// Stores what the sign-in window returned (a JSON `{cookie: value}` object) and asks for the battletag. Ok(display name).
pub fn save_login(http: &dyn Http, dir: &Path, raw_json: &str) -> Result<String, String> {
    let cookies: BTreeMap<String, String> = serde_json::from_str(raw_json).map_err(|_| "Could not read the login session".to_string())?;
    let cookies: BTreeMap<String, String> = cookies.into_iter().filter(|(_, v)| !v.is_empty()).collect();
    if cookies.is_empty() { return Err("No cookies were captured. Try signing in again.".into()); }
    crate::account::plugin_secret_set(dir, "battle_net", "web_cookies", Some(&serde_json::to_string(&cookies).unwrap_or_default())).map_err(|_| "Could not save the sign-in".to_string())?;
    crate::net::log(&format!("Battle.net account: sign-in received ({} cookies: {})", cookies.len(), cookies.keys().cloned().collect::<Vec<_>>().join(", ")));
    let mut s = Session::new(http, cookies);
    if s.check_connection() != Some(true) { s.warm_up(); }
    // the website must accept what the window captured, or nothing later will work
    if s.check_connection() == Some(false) {
        let _ = clear(dir);
        return Err("Battle.net did not accept the sign-in (it still treats you as signed out). Try again, and tell me if it keeps happening.".into());
    }
    let tag = s.battletag();
    // the warm-up renews the session cookie: keep the renewed jar
    let _ = crate::account::plugin_secret_set(dir, "battle_net", "web_cookies", Some(&serde_json::to_string(&s.jar).unwrap_or_default()));
    if let Some(t) = &tag { let _ = crate::account::plugin_secret_set(dir, "battle_net", "battletag", Some(t)); }
    Ok(tag.unwrap_or_else(|| "Battle.net Account".into()))
}

// ------------------------------------------------------------------ the session

fn is_battle_net(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else { return false };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("").split(':').next().unwrap_or("");
    host == "battle.net" || host.ends_with(".battle.net")
}
fn join(base: &str, loc: &str) -> String {
    if loc.starts_with("http://") || loc.starts_with("https://") { return loc.to_string(); }
    let origin = base.splitn(4, '/').take(3).collect::<Vec<_>>().join("/");
    if loc.starts_with('/') { format!("{origin}{loc}") } else { format!("{origin}/{loc}") }
}

pub struct Session<'a> { http: &'a dyn Http, pub jar: BTreeMap<String, String> }
impl<'a> Session<'a> {
    pub fn new(http: &'a dyn Http, jar: BTreeMap<String, String>) -> Self { Session { http, jar } }
    fn cookie_header(&self) -> String { self.jar.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; ") }
    /// GET with the jar, following redirects by hand so every hop's `Set-Cookie` lands in the jar (a requests.Session does the same). Only battle.net addresses are followed.
    fn get(&mut self, url: &str, timeout: Duration) -> Result<Raw, String> {
        let mut url = url.to_string();
        for _ in 0..HOPS {
            if !is_battle_net(&url) { return Err("The answer pointed away from battle.net".into()); }
            let cookie = self.cookie_header();
            let r = self.http.get_raw(&url, &[("Cookie", cookie.as_str()), ("User-Agent", UA), ("Accept", "application/json, text/html;q=0.8, */*;q=0.5")], timeout).map_err(|e| e.0)?;
            let own_host = url.strip_prefix("https://").unwrap_or("").split(['/', '?', '#']).next().unwrap_or("") == "account.battle.net";
            crate::net::log(&format!("Battle.net account: GET {} -> {}", url.split('?').next().unwrap_or(""), r.status));
            for c in r.header_all("set-cookie") {
                // a flat jar: another host's own-host cookies (SESSIONID, JSESSIONID...) would overwrite the account's, so only the account's host and Domain=battle.net cookies are taken
                let shared = c.to_ascii_lowercase().contains("domain=.battle.net") || c.to_ascii_lowercase().contains("domain=battle.net");
                if !own_host && !shared { continue; }
                if let Some((n, v)) = c.split(';').next().and_then(|p| p.split_once('=')) {
                    let (n, v) = (n.trim(), v.trim());
                    if n.is_empty() { continue; }
                    // an expired cookie arrives with an empty value: drop it
                    if v.is_empty() || v == "deleted" { self.jar.remove(n); } else { self.jar.insert(n.to_string(), v.to_string()); }
                }
            }
            if (300..400).contains(&r.status) {
                let Some(loc) = r.header_all("location").first().map(|l| l.to_string()) else { return Ok(r) };
                url = join(&url, &loc);
                continue;
            }
            return Ok(r);
        }
        Err("Too many redirects".into())
    }
    /// Renews the auth cookie before the real calls (Playnite and the GOG Galaxy community plugin both do this). A failure is only logged.
    pub fn warm_up(&mut self) {
        if let Err(e) = self.get(&format!("{BASE}/oauth2/authorization/account-settings"), Duration::from_secs(15)) { crate::net::log(&format!("Battle.net account: warm-up failed: {e}")); }
    }
    fn json(&mut self, path: &str, secs: u64) -> Result<serde_json::Value, String> {
        let r = self.get(&format!("{BASE}{path}"), Duration::from_secs(secs))?;
        if r.status != 200 { return Err(format!("HTTP {}", r.status)); }
        serde_json::from_str(&r.body).map_err(|_| "unreadable answer".to_string())
    }
    /// Some(true/false) = the website still treats the login as signed in or not; None = no answer (offline), which must not read as "signed out".
    pub fn check_connection(&mut self) -> Option<bool> {
        match self.get(&format!("{BASE}/api/"), Duration::from_secs(10)) {
            Err(_) => None,
            Ok(r) if r.status == 401 || r.status == 403 => Some(false),
            Ok(r) => { crate::net::log(&format!("Battle.net account: /api/ answered {} {}", r.status, r.body.chars().take(120).collect::<String>())); serde_json::from_str::<serde_json::Value>(&r.body).ok().map(|v| v["authenticated"].as_bool().unwrap_or(false)) }
        }
    }
    pub fn battletag(&mut self) -> Option<String> {
        let d = self.json("/api/details", 10).ok()?;
        ["battleTag", "battletag", "battleTagName"].iter().find_map(|k| d.get(*k).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(String::from))
    }
    /// The numeric title ids the account owns. Empty on any failure (expired session, network, a changed answer): "nothing to add", never "owns nothing".
    pub fn owned_title_ids(&mut self) -> BTreeSet<i64> {
        match self.json("/api/games-and-subs", 20) {
            Ok(d) => d["gameAccounts"].as_array().map(|a| a.iter().filter_map(|g| g["titleId"].as_i64()).collect()).unwrap_or_default(),
            Err(e) => { crate::net::log(&format!("Battle.net account: games-and-subs failed: {e}")); BTreeSet::new() }
        }
    }
    /// {catalog code: unix time} of every real purchase (free-access grants and free-to-play titles have no transaction). The endpoint carries only a free-text title,
    /// matched through the catalog's normalised names; the earliest purchase of a title wins.
    pub fn purchase_dates(&mut self) -> BTreeMap<String, i64> {
        let d = match self.json("/api/transactions", 20) { Ok(d) => d, Err(e) => { crate::net::log(&format!("Battle.net account: transactions failed: {e}")); return BTreeMap::new() } };
        let mut out: BTreeMap<String, i64> = BTreeMap::new();
        let mut unmatched: Vec<String> = vec![];
        // what the answer's other sections hold (counts and field names only, no values), to see whether claimed gifts carry dates
        for k in ["giftClaims", "metadata", "vcViewModel"] {
            let v = &d[k];
            let shape = match v { serde_json::Value::Array(a) => format!("{} items, first has: {}", a.len(), a.first().and_then(|x| x.as_object()).map(|o| o.keys().cloned().collect::<Vec<_>>().join(",")).unwrap_or_default()), serde_json::Value::Object(o) => format!("object with: {}", o.keys().cloned().collect::<Vec<_>>().join(",")), other => format!("{}", if other.is_null() { "missing" } else { "a plain value" }) };
            crate::net::log(&format!("Battle.net account: transactions.{k}: {shape}"));
        }
        crate::net::log(&format!("Battle.net account: transactions answered with {} purchases (keys: {})", d["purchases"].as_array().map(|a| a.len()).unwrap_or(0), d.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>().join(",")).unwrap_or_default()));
        for p in d["purchases"].as_array().map(|a| a.as_slice()).unwrap_or(&[]) {
            let title = p["productTitle"].as_str().unwrap_or("").trim();
            let Some(ms) = p["date"].as_i64() else { continue };
            if title.is_empty() { continue; }
            let Some(g) = bn::by_name(title) else { unmatched.push(title.to_string()); continue };
            let ts = ms.div_euclid(1000);
            let e = out.entry(g.code.to_string()).or_insert(ts);
            if ts < *e { *e = ts; }
        }
        crate::net::log(&format!("Battle.net account: {} purchase dates matched; titles with no catalog match: {}", out.len(), unmatched.join(" | ")));
        out
    }
}

// ------------------------------------------------------------------ what the account owns

/// A game the account owns, with its purchase date when a real purchase is on record.
#[derive(Debug, Clone, PartialEq)]
pub struct Owned { pub found: bn::Found, pub date_added: Option<i64> }

/// `scan_owned`: every title id the account owns that the catalog knows (an unknown id is logged so it can be added).
pub fn scan_owned(s: &mut Session) -> Vec<Owned> {
    let dates = s.purchase_dates();
    let mut out = vec![];
    for tid in s.owned_title_ids() {
        let Some(g) = bn::by_title_id(tid) else { crate::net::log(&format!("Battle.net account: unrecognized title id {tid}: add it to the catalog")); continue };
        out.push(Owned { date_added: dates.get(g.code).copied(), found: bn::Found { code: g.code.into(), uid: g.uid.into(), name: g.name.into(), genres: g.genres.into(), installed: false, install_path: String::new(), source: "account" } });
    }
    out
}

/// `_merge_account`: union by code. What the local files found (install state and path) wins; an account-only game is added as not installed. A date only ever comes from the
/// account. The merged source reads "account" unless the game is installed.
pub fn merge(local: Vec<bn::Found>, account: Vec<Owned>) -> Vec<(bn::Found, Option<i64>)> {
    let mut by_code: BTreeMap<String, (bn::Found, Option<i64>)> = local.into_iter().map(|f| (f.code.clone(), (f, None))).collect();
    for o in account {
        match by_code.get_mut(&o.found.code) {
            Some((f, d)) => { if f.source != "installed" { f.source = "account"; } if o.date_added.is_some() { *d = o.date_added; } }
            None => { let d = o.date_added; by_code.insert(o.found.code.clone(), (o.found, d)); }
        }
    }
    by_code.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{NetError, Response};
    use std::sync::Mutex;

    /// A fake battle.net: answers by path, records the Cookie header of every call, optionally redirects and sets cookies.
    struct Fake { seen: Mutex<Vec<(String, String)>>, authed: bool }
    impl Http for Fake {
        fn get(&self, _u: &str, _h: &[(&str, &str)], _t: Duration) -> Result<Response, NetError> { Err(NetError("plain get not used".into())) }
        fn get_raw(&self, url: &str, headers: &[(&str, &str)], _t: Duration) -> Result<Raw, NetError> {
            let cookie = headers.iter().find(|(k, _)| *k == "Cookie").map(|(_, v)| v.to_string()).unwrap_or_default();
            self.seen.lock().unwrap().push((url.to_string(), cookie.clone()));
            let ok = |body: &str| Ok(Raw { status: 200, headers: vec![], body: body.into() });
            match url.strip_prefix(BASE).unwrap_or(url) {
                "/oauth2/authorization/account-settings" => Ok(Raw { status: 302, headers: vec![("Location".into(), "/callback/renew".into()), ("Set-Cookie".into(), "BA-tassadar=old-renewed; Path=/; HttpOnly".into())], body: String::new() }),
                "/callback/renew" => Ok(Raw { status: 200, headers: vec![("Set-Cookie".into(), "BA-tassadar=renewed; Path=/".into())], body: "ok".into() }),
                "/api/" => if self.authed { ok(r#"{"authenticated":true}"#) } else { Ok(Raw { status: 401, headers: vec![], body: String::new() }) },
                "/api/details" => ok(r#"{"battleTag":"Rob#1234"}"#),
                "/api/games-and-subs" => ok(r#"{"gameAccounts":[{"titleId":5272175},{"titleId":4613486},{"titleId":999},{"titleId":"x"}]}"#),
                "/api/transactions" => ok(r#"{"purchases":[{"productTitle":"Diablo® IV","date":1700000000000},{"productTitle":"Diablo IV","date":1690000000999},{"productTitle":"Unknown Thing","date":1}]}"#),
                "/away" => Ok(Raw { status: 302, headers: vec![("Location".into(), "https://evil.example/steal".into())], body: String::new() }),
                _ => Ok(Raw { status: 404, headers: vec![], body: String::new() }),
            }
        }
    }
    fn fake(authed: bool) -> Fake { Fake { seen: Mutex::new(vec![]), authed } }
    fn jar() -> BTreeMap<String, String> { [("BA-tassadar".to_string(), "s1".to_string()), ("other".to_string(), "o".to_string())].into() }
    fn found(code: &str, source: &'static str, installed: bool) -> bn::Found { bn::Found { code: code.into(), uid: code.to_lowercase(), name: code.into(), genres: String::new(), installed, install_path: if installed { "/x".into() } else { String::new() }, source } }

    #[test] fn the_jar_travels_as_a_cookie_header_and_every_hop_can_renew_it() {
        let f = fake(true); let mut s = Session::new(&f, jar());
        s.warm_up();
        assert_eq!(s.jar.get("BA-tassadar").map(String::as_str), Some("renewed"), "the redirect target's cookie wins");
        let seen = f.seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "the warm-up followed its redirect by hand");
        assert!(seen[0].1.contains("BA-tassadar=s1") && seen[0].1.contains("other=o"));
        assert!(seen[1].1.contains("BA-tassadar=old-renewed"), "the second hop already sends the first hop's cookie");
    }
    #[test] fn the_jar_is_never_sent_to_another_site() {
        let f = fake(true); let mut s = Session::new(&f, jar());
        assert!(s.get(&format!("{BASE}/away"), Duration::from_secs(1)).is_err());
        assert!(f.seen.lock().unwrap().iter().all(|(u, _)| !u.contains("evil.example")), "no request went to the other host");
        assert!(is_battle_net("https://us.account.battle.net/login") && !is_battle_net("https://battle.net.evil.example/") && !is_battle_net("http://account.battle.net/") && !is_battle_net("https://notbattle.net/"));
    }
    #[test] fn a_signed_in_check_tells_signed_out_from_offline() {
        assert_eq!(Session::new(&fake(true), jar()).check_connection(), Some(true));
        assert_eq!(Session::new(&fake(false), jar()).check_connection(), Some(false));
        struct Down; impl Http for Down { fn get(&self, _: &str, _: &[(&str, &str)], _: Duration) -> Result<Response, NetError> { Err(NetError("x".into())) } fn get_raw(&self, _: &str, _: &[(&str, &str)], _: Duration) -> Result<Raw, NetError> { Err(NetError("offline".into())) } }
        assert_eq!(Session::new(&Down, jar()).check_connection(), None, "no answer is not 'signed out'");
        assert!(Session::new(&Down, jar()).owned_title_ids().is_empty() && Session::new(&Down, jar()).purchase_dates().is_empty());
    }
    #[test] fn owned_games_come_from_title_ids_with_the_earliest_purchase_date() {
        let f = fake(true); let mut s = Session::new(&f, jar());
        let mut got = scan_owned(&mut s); got.sort_by(|a, b| a.found.code.cmp(&b.found.code));
        let codes: Vec<&str> = got.iter().map(|o| o.found.code.as_str()).collect();
        assert_eq!(codes, vec!["Fen", "Pro"], "an unknown id and a non-number are skipped");
        let fen = &got[0]; assert_eq!(fen.found.name, "Diablo IV"); assert_eq!(fen.date_added, Some(1_690_000_000), "the earlier of two purchases, in seconds");
        assert_eq!(got[1].date_added, None, "no purchase on record (a free-to-play title)");
        assert!(got.iter().all(|o| !o.found.installed && o.found.source == "account"));
        assert_eq!(s.battletag().as_deref(), Some("Rob#1234"));
    }
    #[test] fn merging_keeps_local_install_state_and_takes_dates_and_new_games_from_the_account() {
        let local = vec![found("Pro", "installed", true), found("D3", "license", false)];
        let acct = vec![Owned { found: found("Pro", "account", false), date_added: Some(5) }, Owned { found: found("D3", "account", false), date_added: None }, Owned { found: found("Fen", "account", false), date_added: Some(7) }];
        let m = merge(local, acct);
        let get = |c: &str| m.iter().find(|(f, _)| f.code == c).unwrap();
        assert!(get("Pro").0.installed && get("Pro").0.source == "installed" && get("Pro").1 == Some(5), "installed stays installed, gains the date");
        assert!(get("D3").0.source == "account" && get("D3").1.is_none(), "a licence-only game is confirmed by the account");
        assert!(!get("Fen").0.installed && get("Fen").1 == Some(7), "an account-only game is added as not installed");
        assert_eq!(m.len(), 3);
    }
    #[test] fn a_login_is_saved_with_its_battletag_and_the_renewed_cookies_and_can_be_cleared() {
        let d = std::env::temp_dir().join(format!("zest_bnacct_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        assert!(save_login(&fake(true), &d, "not json").is_err() && save_login(&fake(true), &d, "{}").is_err() && save_login(&fake(true), &d, r#"{"a":""}"#).is_err());
        let who = save_login(&fake(true), &d, r#"{"BA-tassadar":"s1","x":"y"}"#).unwrap();
        assert_eq!(who, "Rob#1234");
        let c = saved_cookies(&d).unwrap(); assert_eq!(c["BA-tassadar"], "s1", "a login the site already accepts is kept as it came"); assert_eq!(c["x"], "y");
        assert_eq!(saved_battletag(&d).as_deref(), Some("Rob#1234"));
        assert!(save_login(&fake(false), &d, r#"{"x":"y"}"#).is_err() && saved_cookies(&d).is_none(), "a rejected login is not kept");
        clear(&d).unwrap(); assert!(saved_cookies(&d).is_none() && saved_battletag(&d).is_none());
    }
    #[test] fn the_sign_in_window_waits_for_the_session_cookie_on_the_account_root() {
        let d = login_done(); assert_eq!(d["names"][0], "*"); assert!(d.get("need_prefix").is_none()); assert_eq!(d["url_not_contains"], "/api/|/callback");
        assert_eq!(login_url(), "https://account.battle.net/api/logout");
    }
}
