//! The Steam / ProtonDB fetchers (`scrapers.fetch_*`): build the request, call the `Http` trait, hand the body to the parsers in `steamweb`.
//! Same contract as Python: a 429 is `Err(RateLimited)`; any other failure is logged (without a URL) and gives `Ok(None)`.
use crate::net::{enc, Http, Response};
use crate::steamweb::{self as sw, Cheevos, OwnedGame, Reviews, StoreData};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub struct RateLimited(pub Option<f64>);
pub type Fetch<T> = Result<T, RateLimited>;

/// What the store page says about an app.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Listing { /// The page loads.
    Live, /// The store has removed the app.
    Gone, /// Could not tell (an error, an unexpected answer).
    Unknown }

/// The pure part of `Steam::store_listing`: a redirect to the store's front page (or a 404) is a removed app; a 200 is a live one.
pub fn classify_listing(status: u16, location: &str) -> Listing {
    match status {
        200 => Listing::Live,
        404 | 410 => Listing::Gone,
        301 | 302 | 303 | 307 | 308 => {
            let path = location.trim().trim_start_matches("https://store.steampowered.com").trim_start_matches("http://store.steampowered.com").split(['?', '#']).next().unwrap_or("");
            if path.is_empty() || path == "/" { Listing::Gone } else if path.starts_with("/app/") { Listing::Live } else { Listing::Unknown }   // a redirect to the app's own page is a live app (a language or region hop)
        }
        _ => Listing::Unknown,
    }
}

/// The account's key and SteamID. `Debug` never shows them.
#[derive(Clone)]
pub struct Creds { pub api_key: String, pub steam_id: String }
impl std::fmt::Debug for Creds { fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("Creds(..)") } }

/// What the local appinfo file knows of a game's release dates (`steamlocal::AppInfo`).
#[derive(Debug, Clone, Copy, Default)]
pub struct AppinfoDates { pub original: Option<i64>, pub steam: Option<i64> }

/// `fetch_store_data`'s result: the parsed store entry with the release date already decided.
#[derive(Debug, Clone, PartialEq)]
pub struct Store { pub data: StoreData, pub release_date: Option<i64> }

pub struct Steam { http: Arc<dyn Http>, creds: Option<Creds>, half_trust: f64 }

const T: fn(u64) -> Duration = Duration::from_secs;
const AGE_COOKIE: &str = "birthtime=283993201; lastagecheckage=1-0-1979";

impl Steam {
    pub fn new(http: Arc<dyn Http>, creds: Option<Creds>, half_trust: f64) -> Self { Steam { http, creds, half_trust } }
    pub fn has_key(&self) -> bool { self.creds.as_ref().map_or(false, |c| !c.api_key.trim().is_empty() && !c.steam_id.trim().is_empty()) }

    /// One GET with the shared handling: a transport error is logged and None; 429 is `RateLimited`.
    fn get(&self, what: &str, url: &str, headers: &[(&str, &str)], timeout: u64) -> Fetch<Option<Response>> {
        match self.http.get(url, headers, T(timeout)) {
            Ok(r) if r.status == 429 => Err(RateLimited(r.retry_after)),
            Ok(r) => Ok(Some(r)),
            Err(e) => { crate::net::log(&format!("{what}: {e}")); Ok(None) }
        }
    }
    fn ok_body(&self, what: &str, url: &str, headers: &[(&str, &str)], timeout: u64) -> Fetch<Option<String>> {
        Ok(self.get(what, url, headers, timeout)?.and_then(|r| if (200..300).contains(&r.status) { Some(r.body) } else { crate::net::log(&format!("{what}: HTTP {}", r.status)); None }))
    }

    /// `_scrape_store_page_date`
    fn store_page_date(&self, appid: i64) -> Fetch<Option<i64>> {
        let url = format!("https://store.steampowered.com/app/{appid}/?l=english");
        Ok(self.ok_body("store page date", &url, &[("Cookie", "birthtime=0; mature_content=1")], 15)?.and_then(|b| sw::parse_store_page_date(&b)))
    }

    /// Is the app's store page still there? A removed app (delisted by its developer, or taken down by Valve; Steam does not say which) sends the page to the store's front page or
    /// answers 404; a live one answers 200. Anything else, or a request that failed, is `Unknown`, so a hiccup is never taken for a removal. Rate limits are the usual `Err`.
    pub fn store_listing(&self, appid: i64) -> Fetch<Listing> {
        let url = format!("https://store.steampowered.com/app/{appid}/?l=english");
        match self.http.get_raw(&url, &[("Cookie", AGE_COOKIE), ("User-Agent", "Mozilla/5.0")], T(10)) {
            Ok(r) => {
                if r.status == 429 { return Err(RateLimited(r.header_all("retry-after").first().and_then(|v| crate::net::parse_retry_after(v)))); }
                let location = r.header_all("location").first().copied().unwrap_or("");
                Ok(classify_listing(r.status, location))
            }
            Err(_) => Ok(Listing::Unknown),
        }
    }

    /// `fetch_store_data`: None when the store does not know the app or the request failed.
    pub fn store_data(&self, appid: i64, dates: AppinfoDates) -> Fetch<Option<Store>> {
        let url = format!("https://store.steampowered.com/api/appdetails?appids={appid}&l=english");
        let Some(body) = self.ok_body("store data", &url, &[], 15)? else { return Ok(None) };
        let Some(data) = sw::parse_appdetails(&body, appid) else { return Ok(None) };
        let mut limited = None;
        let release = sw::release_date(dates.original, dates.steam, &data.api_date, || match self.store_page_date(appid) { Ok(d) => d, Err(e) => { limited = Some(e); None } });
        if let Some(e) = limited { return Err(e); }
        Ok(Some(Store { data, release_date: release }))
    }

    /// `fetch_review_data`
    pub fn reviews(&self, appid: i64) -> Fetch<Option<Reviews>> {
        let url = format!("{}?input_json={}", sw::REVIEWS_URL, enc(&sw::reviews_input_json(appid)));
        Ok(self.ok_body("review data", &url, &[], 20)?.and_then(|b| sw::parse_reviews(&b, self.half_trust)))
    }

    /// `fetch_tag_data`: only a 200 counts (like Python), tags come from `a.app_tag`.
    pub fn tags(&self, appid: i64) -> Fetch<Option<Vec<String>>> {
        let url = format!("https://store.steampowered.com/app/{appid}/?l=english");
        let Some(r) = self.get("tags", &url, &[("Cookie", AGE_COOKIE), ("User-Agent", "Mozilla/5.0")], 10)? else { return Ok(None) };
        Ok(if r.status == 200 { sw::parse_tags(&r.body) } else { None })
    }

    /// `fetch_cheevo_data`: needs a key; 403 (not owned / private) is an expected None.
    pub fn cheevos(&self, appid: i64) -> Fetch<Option<Cheevos>> {
        let Some(c) = self.creds.as_ref().filter(|_| self.has_key()) else { return Ok(None) };
        let url = format!("https://api.steampowered.com/ISteamUserStats/GetPlayerAchievements/v0001/?appid={appid}&key={}&steamid={}&include_played_free_games=1&skip_unvetted_apps=false", enc(c.api_key.trim()), enc(c.steam_id.trim()));
        let Some(r) = self.get("achievements", &url, &[], 15)? else { return Ok(None) };
        Ok(sw::parse_cheevos(r.status, &r.body))
    }

    /// `fetch_achievement_schema_total`: needs a key but not ownership.
    pub fn schema_total(&self, appid: i64) -> Fetch<Option<i64>> {
        let Some(c) = self.creds.as_ref().filter(|c| !c.api_key.trim().is_empty()) else { return Ok(None) };
        let url = format!("https://api.steampowered.com/ISteamUserStats/GetSchemaForGame/v2/?appid={appid}&key={}", enc(c.api_key.trim()));
        Ok(self.ok_body("achievement schema", &url, &[], 15)?.and_then(|b| sw::parse_schema_total(&b)))
    }

    /// `fetch_player_data`: one owned game's name / playtime / last played.
    pub fn player_data(&self, appid: i64) -> Fetch<Option<OwnedGame>> {
        let Some(c) = self.creds.as_ref().filter(|_| self.has_key()) else { return Ok(None) };
        let url = format!("https://api.steampowered.com/IPlayerService/GetOwnedGames/v0001/?key={}&steamid={}&format=json&include_appinfo=true&include_played_free_games=1&skip_unvetted_apps=false&appids_filter%5B0%5D={appid}", enc(&c.api_key), enc(&c.steam_id));
        Ok(self.ok_body("player data", &url, &[], 10)?.and_then(|b| sw::parse_owned_games(&b).ok()).and_then(|g| g.into_iter().next()))
    }

    /// `fetch_owned_appids`' request: the whole library. Err carries the message to show.
    pub fn owned_games(&self) -> Result<Vec<OwnedGame>, String> {
        let Some(c) = self.creds.as_ref().filter(|_| self.has_key()) else { return Err("No Steam API key or SteamID is set.".into()) };
        let url = format!("https://api.steampowered.com/IPlayerService/GetOwnedGames/v1/?key={}&steamid={}&format=json&include_appinfo=true&include_played_free_games=1&skip_unvetted_apps=false", enc(&c.api_key), enc(&c.steam_id));
        match self.http.get(&url, &[], T(30)) {
            Ok(r) if (200..300).contains(&r.status) => sw::parse_owned_games(&r.body).map_err(String::from),
            Ok(r) if r.status == 403 => Err("Steam rejected the API key (HTTP 403). Check the key.".into()),
            Ok(r) => Err(format!("Steam answered HTTP {}.", r.status)),
            Err(e) => Err(format!("Could not reach Steam: {e}")),
        }
    }

    /// `fetch_protondb_data`: `Ok(Some(None))`-style distinction matters here, so it is its own type: a missing page / `pending` CLEARS the stored tier,
    /// a failed request must not (Python wiped the tier on a network error too; this is a deliberate difference).
    pub fn protondb(&self, appid: i64) -> Result<Option<(String, Option<String>)>, NetFail> {
        let url = format!("https://www.protondb.com/api/v1/reports/summaries/{appid}.json");
        match self.http.get(&url, &[("User-Agent", "Mozilla/5.0")], T(10)) {
            Ok(r) if r.status == 404 => Ok(None),
            Ok(r) if (200..300).contains(&r.status) => Ok(sw::parse_protondb(&r.body)),
            Ok(r) => Err(NetFail(format!("HTTP {}", r.status))),
            Err(e) => Err(NetFail(e.0)),
        }
    }
}

/// A request that got no usable answer (safe to show: no URL inside).
#[derive(Debug, Clone, PartialEq)]
pub struct NetFail(pub String);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::NetError;
    use std::sync::Mutex;

    /// Answers by substring of the URL; records every URL asked.
    struct Fake { rules: Vec<(&'static str, Result<Response, NetError>)>, seen: Mutex<Vec<String>> }
    impl Http for Fake {
        fn get(&self, url: &str, _h: &[(&str, &str)], _t: Duration) -> Result<Response, NetError> {
            self.seen.lock().unwrap().push(url.to_string());
            self.rules.iter().find(|(k, _)| url.contains(k)).map(|(_, v)| v.clone()).unwrap_or(Err(NetError("no rule".into())))
        }
    }
    fn resp(status: u16, body: &str) -> Result<Response, NetError> { Ok(Response { status, retry_after: None, body: body.into() }) }
    fn steam(rules: Vec<(&'static str, Result<Response, NetError>)>, creds: bool) -> (Steam, Arc<Fake>) {
        let f = Arc::new(Fake { rules, seen: Mutex::new(vec![]) });
        (Steam::new(f.clone(), creds.then(|| Creds { api_key: "K E+Y".into(), steam_id: "7656".into() }), 10.0), f)
    }

    #[test] fn rate_limit_carries_retry_after() {
        let (s, _) = steam(vec![("appdetails", Ok(Response { status: 429, retry_after: Some(42.0), body: String::new() }))], false);
        assert_eq!(s.store_data(1, AppinfoDates::default()), Err(RateLimited(Some(42.0))));
    }
    #[test] fn transport_failure_is_none_not_error() {
        let (s, _) = steam(vec![("reports", Err(NetError("Dns".into())))], false);
        assert_eq!(s.reviews(1), Ok(None));
    }
    #[test] fn reviews_url_and_parse() {
        let body = r#"{"response":{"query_summary":{"total_reviews":100,"total_positive":90,"review_score_desc":"Very Positive"}}}"#;
        let (s, f) = steam(vec![("GetAppReviews", resp(200, body))], false);
        let r = s.reviews(620).unwrap().unwrap();
        assert_eq!((r.total_reviews, r.review_percentage, r.review_score.as_str()), (100, 90, "Very Positive"));
        let u = f.seen.lock().unwrap()[0].clone();
        assert!(u.contains("input_json=%7B%22appid%22%3A620%2C%22languages%22%3A%5B%22all%22%5D"), "{u}");
    }
    #[test] fn key_is_encoded_and_never_in_debug() {
        let (s, f) = steam(vec![("GetPlayerAchievements", resp(403, ""))], true);
        assert_eq!(s.cheevos(5), Ok(None));
        assert!(f.seen.lock().unwrap()[0].contains("key=K%20E%2BY&steamid=7656"));
        assert!(!format!("{:?}", s.creds).contains("K E"));
    }
    #[test] fn no_key_no_request() {
        let (s, f) = steam(vec![], false);
        assert_eq!((s.cheevos(5), s.schema_total(5), s.player_data(5)), (Ok(None), Ok(None), Ok(None)));
        assert!(f.seen.lock().unwrap().is_empty());
    }
    #[test] fn cheevo_statuses() {
        let (s, _) = steam(vec![("GetPlayerAchievements", resp(400, ""))], true);
        assert_eq!(s.cheevos(5), Ok(Some(Cheevos { total: 0, unlocked: 0, completed: false })));
    }
    #[test] fn protondb_cases() {
        let (s, _) = steam(vec![("/1.json", resp(404, "")), ("/2.json", resp(200, r#"{"tier":"gold","confidence":"strong"}"#)), ("/3.json", resp(200, r#"{"tier":"pending"}"#)), ("/4.json", resp(500, "")), ("/5.json", Err(NetError("Io".into())))], false);
        assert_eq!(s.protondb(1), Ok(None));
        assert_eq!(s.protondb(2), Ok(Some(("gold".into(), Some("strong".into())))));
        assert_eq!(s.protondb(3), Ok(None));
        assert!(s.protondb(4).is_err() && s.protondb(5).is_err());
    }
    #[test] fn store_date_only_fetched_when_needed() {
        let body = r#"{"1":{"success":true,"data":{"name":"G","type":"game","steam_appid":1,"release_date":{"date":"5 Jan, 2020"}}}}"#;
        let (s, f) = steam(vec![("appdetails", resp(200, body)), ("/app/1/", resp(200, "<div class=release_date><div class=date>Jan 6, 2019</div></div>"))], false);
        let st = s.store_data(1, AppinfoDates { original: Some(77), steam: None }).unwrap().unwrap();
        assert_eq!(st.release_date, Some(77)); assert_eq!(f.seen.lock().unwrap().len(), 1);
        let st = s.store_data(1, AppinfoDates::default()).unwrap().unwrap();
        assert_eq!(st.release_date, sw::parse_steam_date("Jan 6, 2019")); assert_eq!(f.seen.lock().unwrap().len(), 3);
    }
}

#[cfg(test)]
mod listing_tests {
    use super::*;
    #[test] fn a_store_page_that_redirects_to_the_front_page_or_is_missing_is_a_removed_app() {
        assert_eq!(classify_listing(200, ""), Listing::Live);
        assert_eq!(classify_listing(302, "https://store.steampowered.com/"), Listing::Gone);
        assert_eq!(classify_listing(302, "/"), Listing::Gone);
        assert_eq!(classify_listing(302, "https://store.steampowered.com/?l=english"), Listing::Gone);
        assert_eq!(classify_listing(404, ""), Listing::Gone);
        assert_eq!(classify_listing(302, "https://store.steampowered.com/app/440/Team_Fortress_2/"), Listing::Live, "a hop to the app's own page");
        assert_eq!(classify_listing(302, "https://store.steampowered.com/agecheck/app/440/"), Listing::Unknown, "an age gate says nothing about removal");
        assert_eq!(classify_listing(503, ""), Listing::Unknown);
    }
}
