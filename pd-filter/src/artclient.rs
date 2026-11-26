//! Cover art downloads (`images.py`: `_get_steam_assets`, `_download_vertical/_horizontal/_icon`, `download_from_steam`, `_sgdb_download_best`,
//! `_download_art`, `_download_preferring_fit`). The picture work (decode, JPEG save, edge colors, the shape check) stays with the app behind `ArtStore`,
//! so this crate has no image code. The SteamGridDB key goes in a request header only and never in a message.
use crate::net::{enc, Http};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind { Vertical, Horizontal, Icon }
impl Kind {
    pub fn name(&self) -> &'static str { match self { Kind::Vertical => "vertical", Kind::Horizontal => "horizontal", Kind::Icon => "icon" } }
    pub fn parse(s: &str) -> Option<Kind> { match s { "vertical" => Some(Kind::Vertical), "horizontal" => Some(Kind::Horizontal), "icon" => Some(Kind::Icon), _ => None } }
    fn target(&self) -> f64 { match self { Kind::Vertical => 2.0 / 3.0, Kind::Horizontal => 616.0 / 353.0, Kind::Icon => 1.0 } }
}

/// The app's side: writing a cover (JPEG, edge colors, database) and looking at what is stored.
pub trait ArtStore: Send + Sync {
    /// `save_as_jpg`: true when the bytes were an image and the cover is now saved for `appid`.
    fn save(&self, kind: Kind, appid: i64, bytes: &[u8]) -> bool;
    /// `_art_fits`: the stored cover has the card's shape (5% rule).
    fn fits(&self, kind: Kind, appid: i64) -> bool;
    /// The stored cover's file bytes, if any.
    fn read(&self, kind: Kind, appid: i64) -> Option<Vec<u8>>;
}

/// Where else art can come from, supplied by the app: the Steam copy of a non-Steam game (`images.steam_copy_of`) and a plugin's own art URL.
pub trait Hooks: Send + Sync {
    fn steam_copy_of(&self, _appid: i64) -> Option<(i64, String)> { None }
    fn store_url(&self, _appid: i64, _kind: Kind) -> Option<String> { None }
}
pub struct NoHooks;
impl Hooks for NoHooks {}

#[derive(Debug, Clone, PartialEq)]
pub struct SgdbOption { pub url: String, pub thumb: String, pub width: i64, pub height: i64, pub fits: bool }
#[derive(Debug, Clone, PartialEq)]
pub struct SgdbGame { pub id: i64, pub name: String, pub verified: bool }

pub struct Art { http: Arc<dyn Http>, store: Arc<dyn ArtStore>, hooks: Arc<dyn Hooks>, sgdb_key: Option<String> }

const SGDB_TRIES: usize = 5;
const CDN: &str = "https://cdn.cloudflare.steamstatic.com/steam/apps";

/// `urllib.parse.quote(name)`: everything but letters, digits, `_.-~` and `/` is percent-encoded.
fn quote(s: &str) -> String { s.split('/').map(enc).collect::<Vec<_>>().join("/") }

impl Art {
    pub fn new(http: Arc<dyn Http>, store: Arc<dyn ArtStore>, hooks: Arc<dyn Hooks>, sgdb_key: Option<String>) -> Self { Art { http, store, hooks, sgdb_key: sgdb_key.filter(|k| !k.trim().is_empty()) } }

    fn json(&self, url: &str, headers: &[(&str, &str)], secs: u64) -> Option<Value> {
        match self.http.get(url, headers, Duration::from_secs(secs)) { Ok(r) if r.status == 200 => serde_json::from_str(&r.body).ok(), Ok(_) => None, Err(e) => { crate::net::log(&format!("art: {e}")); None } }
    }
    /// GET an image and save it; true when it was a 200 and an image.
    fn fetch_save(&self, url: &str, kind: Kind, appid: i64, secs: u64) -> bool {
        match self.http.get_bytes(url, &[], Duration::from_secs(secs)) {
            Ok((200, b)) => self.store.save(kind, appid, &b),
            Ok(_) => false,
            Err(e) => { crate::net::log(&format!("download_{}: {e}", kind.name())); false }
        }
    }

    /// `_get_steam_assets`: asset name -> URL from the store's item manifest; empty on any failure.
    pub fn steam_assets(&self, appid: i64) -> HashMap<String, String> {
        let input = format!(r#"{{"ids":[{{"appid":{appid}}}],"context":{{"language":"english","country_code":"US","steam_realm":1}},"data_request":{{"include_assets":true}}}}"#);
        let url = format!("https://api.steampowered.com/IStoreBrowseService/GetItems/v1/?input_json={}", enc(&input));
        let Some(j) = self.json(&url, &[], 8) else { return HashMap::new() };
        let Some(assets) = j.pointer("/response/store_items/0/assets").and_then(|a| a.as_object()) else { return HashMap::new() };
        let fmt = assets.get("asset_url_format").and_then(|v| v.as_str()).unwrap_or("");
        if fmt.is_empty() { return HashMap::new(); }
        let base = format!("https://shared.fastly.steamstatic.com/store_item_assets/{fmt}");
        assets.iter().filter(|(k, _)| k.as_str() != "asset_url_format").filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), base.replace("${FILENAME}", v)))).collect()
    }

    fn sgdb(&self, endpoint: &str) -> Option<Value> {
        let key = self.sgdb_key.as_ref()?;
        let auth = format!("Bearer {key}");
        self.json(&format!("https://www.steamgriddb.com/api/v2/{endpoint}"), &[("Authorization", &auth)], 5)
    }
    fn sgdb_data(&self, endpoint: &str) -> Option<Value> {
        let j = self.sgdb(endpoint)?;
        (j.get("success").and_then(|v| v.as_bool()).unwrap_or(false) && j.get("data").and_then(|d| d.as_array()).map_or(false, |d| !d.is_empty())).then_some(j)
    }

    /// `_sgdb_search_game_id`
    pub fn sgdb_search_game_id(&self, name: &str) -> Option<i64> {
        if self.sgdb_key.is_none() || name.is_empty() { return None; }
        self.sgdb_data(&format!("search/autocomplete/{}", quote(name)))?.pointer("/data/0/id").and_then(|v| v.as_i64())
    }

    /// `_steam_search_appid`: the store search's first hit, whatever its type.
    pub fn steam_search_appid(&self, name: &str) -> Option<i64> {
        let url = format!("https://store.steampowered.com/api/storesearch/?term={}&l=english&cc=US", enc(name));
        self.json(&url, &[], 5)?.get("items")?.as_array()?.first()?.get("id")?.as_i64()
    }

    pub fn steam_copy_of(&self, appid: i64) -> Option<(i64, String)> { self.hooks.steam_copy_of(appid) }
    pub fn has_cover(&self, kind: Kind, appid: i64) -> bool { self.store.read(kind, appid).is_some() }

    /// `_sgdb_options_from`: the static images, closest to the card's shape first (SteamGridDB's order among equals); `fits` is the 5% rule (icons always fit).
    pub fn options_from(data: &Value, kind: Kind) -> Vec<SgdbOption> {
        let mut opts: Vec<(f64, SgdbOption)> = data["data"].as_array().into_iter().flatten().filter(|i| !i.get("animated").map_or(false, |a| a.as_bool().unwrap_or(false) || a.as_i64().map_or(false, |n| n != 0)))
            .map(|i| {
                let (w, h) = (i.get("width").and_then(|v| v.as_i64()).unwrap_or(0), i.get("height").and_then(|v| v.as_i64()).unwrap_or(0));
                let d = if w != 0 && h != 0 { ((w as f64 / h as f64 - kind.target()).abs() / kind.target() * 1000.0).round_ties_even() / 1000.0 } else { f64::INFINITY };
                let raw = if w != 0 && h != 0 { (w as f64 / h as f64 - kind.target()).abs() / kind.target() } else { f64::INFINITY };
                let s = |k: &str| i.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
                (d, SgdbOption { url: s("url"), thumb: s("thumb"), width: w, height: h, fits: kind == Kind::Icon || raw < 0.05 })
            }).collect();
        opts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));   // stable, like Python's
        opts.into_iter().map(|(_, o)| o).collect()
    }

    /// `fetch_sgdb_options` / `fetch_sgdb_options_by_id`: what the picker lists. `by_game` = a SteamGridDB game id, else the Steam appid.
    pub fn sgdb_options(&self, id: i64, by_game: bool, kind: Kind) -> Vec<SgdbOption> {
        let (grid_or_icon, q) = match kind { Kind::Vertical => ("grids", ""), Kind::Horizontal => ("grids", "?dimensions=460x215,920x430"), Kind::Icon => ("icons", "") };
        let ep = format!("{grid_or_icon}/{}/{id}{q}", if by_game { "game" } else { "steam" });
        self.sgdb_data(&ep).map(|d| Self::options_from(&d, kind)).unwrap_or_default()
    }

    /// `search_sgdb_games`: games matching a name.
    pub fn sgdb_search(&self, term: &str) -> Vec<SgdbGame> {
        if self.sgdb_key.is_none() || term.is_empty() { return vec![]; }
        self.sgdb_data(&format!("search/autocomplete/{}", quote(term))).map(|d| d["data"].as_array().into_iter().flatten().filter_map(|g| Some(SgdbGame { id: g.get("id")?.as_i64()?, name: g.get("name")?.as_str()?.to_string(), verified: g.get("verified").and_then(|v| v.as_bool()).unwrap_or(false) })).collect()).unwrap_or_default()
    }

    /// A thumbnail or any picture's bytes (the picker previews).
    pub fn picture_bytes(&self, url: &str) -> Option<Vec<u8>> { match self.http.get_bytes(url, &[], Duration::from_secs(8)) { Ok((200, b)) => Some(b), _ => None } }
    pub fn has_sgdb_key(&self) -> bool { self.sgdb_key.is_some() }

    /// `_sgdb_download_best`: tries the best-fitting images, up to 5; a vertical grid is never landscape or square.
    fn sgdb_download_best(&self, data: &Value, kind: Kind, appid: i64) -> bool {
        let mut tried = 0;
        for o in Self::options_from(data, kind) {
            if o.url.is_empty() || (kind == Kind::Vertical && o.width != 0 && o.height != 0 && o.width >= o.height) { continue; }
            tried += 1;
            if self.fetch_save(&o.url, kind, appid, 5) { return true; }
            if tried >= SGDB_TRIES { break; }
        }
        false
    }

    /// The strict runners `_download_vertical/_horizontal/_icon`: `source` is "auto", "steam" or "sgdb"; the tag of what was saved, or "missing".
    pub fn download(&self, kind: Kind, appid: i64, assets: Option<HashMap<String, String>>, source: &str, sgdb_id: Option<i64>, game_name: Option<&str>, icon_hash: &str) -> String {
        let name = game_name.filter(|n| !n.is_empty());
        match kind {
            Kind::Vertical => {
                if source != "sgdb" && sgdb_id.is_none() {
                    let assets = assets.unwrap_or_else(|| self.steam_assets(appid));
                    for key in ["library_capsule_2x", "library_capsule"] { if let Some(u) = assets.get(key) { if self.fetch_save(u, kind, appid, 5) { return if key.contains("2x") { "capsule_2x" } else { "capsule" }.into(); } } }
                    for (u, tag) in [(format!("{CDN}/{appid}/library_600x900_2x.jpg"), "capsule_2x"), (format!("{CDN}/{appid}/library_600x900.jpg"), "capsule")] { if self.fetch_save(&u, kind, appid, 5) { return tag.into(); } }
                }
                if source != "steam" && self.sgdb_key.is_some() {
                    let ep = match sgdb_id { Some(id) => format!("grids/game/{id}"), None => format!("grids/steam/{appid}") };
                    if let Some(d) = self.sgdb_data(&ep) { if self.sgdb_download_best(&d, kind, appid) { return "sgdb_grid".into(); } }
                }
                if let (Some(n), true) = (name, source != "sgdb") {
                    if let Some(sid) = self.steam_search_appid(n) {
                        let a = self.steam_assets(sid);
                        for key in ["library_capsule_2x", "library_capsule"] { if let Some(u) = a.get(key) { if self.fetch_save(u, kind, appid, 5) { return if key.contains("2x") { "capsule_2x_steam" } else { "capsule_steam" }.into(); } } }
                        for (u, tag) in [(format!("{CDN}/{sid}/library_600x900_2x.jpg"), "capsule_2x_steam"), (format!("{CDN}/{sid}/library_600x900.jpg"), "capsule_steam")] { if self.fetch_save(&u, kind, appid, 5) { return tag.into(); } }
                    }
                }
            }
            Kind::Horizontal => {
                if source != "sgdb" && sgdb_id.is_none() {
                    let assets = assets.unwrap_or_else(|| self.steam_assets(appid));
                    if let Some(u) = assets.get("header_image").or_else(|| assets.get("main_capsule")) { if self.fetch_save(u, kind, appid, 5) { return "header".into(); } }
                    if self.fetch_save(&format!("{CDN}/{appid}/header.jpg"), kind, appid, 5) { return "header".into(); }
                }
                if source != "steam" && self.sgdb_key.is_some() {
                    let q = "?dimensions=460x215,920x430";
                    let ep = match sgdb_id { Some(id) => format!("grids/game/{id}{q}"), None => format!("grids/steam/{appid}{q}") };
                    if let Some(d) = self.sgdb_data(&ep) { if self.sgdb_download_best(&d, kind, appid) { return "sgdb_grid_wide".into(); } }
                }
                if let (Some(n), true) = (name, source != "sgdb") {
                    if let Some(sid) = self.steam_search_appid(n) {
                        let a = self.steam_assets(sid);
                        if let Some(u) = a.get("header_image").or_else(|| a.get("main_capsule")) { if self.fetch_save(u, kind, appid, 5) { return "header_steam".into(); } }
                        if self.fetch_save(&format!("{CDN}/{sid}/header.jpg"), kind, appid, 5) { return "header_steam".into(); }
                    }
                }
            }
            Kind::Icon => {
                if source != "steam" && self.sgdb_key.is_some() {
                    let ep = match sgdb_id { Some(id) => format!("icons/game/{id}"), None => format!("icons/steam/{appid}") };
                    if let Some(d) = self.sgdb_data(&ep) { if self.sgdb_download_best(&d, kind, appid) { return "sgdb_icon".into(); } }
                }
                if source != "sgdb" && !icon_hash.is_empty() {
                    let base = format!("https://media.steampowered.com/steamcommunity/public/images/apps/{appid}");
                    for u in [format!("{base}/{icon_hash}_2x.jpg"), format!("{base}/{icon_hash}.jpg")] {
                        match self.http.get_bytes(&u, &[], Duration::from_secs(5)) { Ok((200, b)) if self.store.save(kind, appid, &b) => return "steam".into(), Ok(_) => {}, Err(e) => { crate::net::log(&format!("download_icon: {e}")); break; } }
                    }
                }
                if let (Some(n), true, true) = (name, self.sgdb_key.is_some(), source != "steam") {
                    if let Some(sid) = self.steam_search_appid(n) {
                        if let Some(d) = self.sgdb_data(&format!("icons/steam/{sid}")) {
                            // Python tries the first static icon only (the loop breaks after one attempt)
                            if let Some(item) = d["data"].as_array().into_iter().flatten().find(|i| !i.get("animated").map_or(false, |a| a.as_bool().unwrap_or(false))) {
                                if let Some(u) = item.get("url").and_then(|v| v.as_str()) { if self.fetch_save(u, kind, appid, 5) { return "sgdb_icon_steam".into(); } }
                            }
                        }
                    }
                }
            }
        }
        "missing".into()
    }

    /// `download_from_steam`: Steam game `steam_appid`'s art of `kind`, saved as `appid`'s own. "steam" or "missing".
    pub fn download_from_steam(&self, appid: i64, steam_appid: i64, kind: Kind, icon_hash: &str) -> String {
        let urls: Vec<Option<String>> = match kind {
            Kind::Icon => { if icon_hash.is_empty() { return "missing".into(); } let b = format!("https://media.steampowered.com/steamcommunity/public/images/apps/{steam_appid}"); vec![Some(format!("{b}/{icon_hash}_2x.jpg")), Some(format!("{b}/{icon_hash}.jpg"))] }
            Kind::Vertical => { let a = self.steam_assets(steam_appid); vec![a.get("library_capsule_2x").cloned(), a.get("library_capsule").cloned(), Some(format!("{CDN}/{steam_appid}/library_600x900_2x.jpg")), Some(format!("{CDN}/{steam_appid}/library_600x900.jpg"))] }
            Kind::Horizontal => { let a = self.steam_assets(steam_appid); vec![a.get("header_image").or_else(|| a.get("main_capsule")).cloned(), Some(format!("{CDN}/{steam_appid}/header.jpg"))] }
        };
        for u in urls.into_iter().flatten() { if self.fetch_save(&u, kind, appid, 8) { return "steam".into(); } }
        "missing".into()
    }

    /// `download_from_url` for an http(s) address ("custom" or "missing"); local paths are the app's job.
    pub fn download_from_url(&self, appid: i64, url: &str, kind: Kind) -> String { if self.fetch_save(url, kind, appid, 10) { "custom".into() } else { "missing".into() } }

    fn run_source(&self, kind: Kind, appid: i64, assets: &Option<HashMap<String, String>>, src: &str, sgdb_id: Option<i64>, name: Option<&str>, icon_hash: &str) -> String {
        if src == "steam" && appid < 0 && kind != Kind::Icon {
            if let Some((sid, ih)) = self.hooks.steam_copy_of(appid) { if self.download_from_steam(appid, sid, kind, &ih) == "steam" { return "steam".into(); } }
        }
        self.download(kind, appid, assets.clone(), src, sgdb_id, name, icon_hash)
    }

    /// `_download_art`: an explicit `source` bypasses the order; otherwise `order` (None = the original "auto" chain) is walked, and with `prefer_fit` the first
    /// result that fits the card wins, else the first result there was is put back.
    pub fn download_art(&self, kind: Kind, appid: i64, assets: Option<HashMap<String, String>>, source: &str, order: Option<&[String]>, prefer_fit: bool,
                        sgdb_id: Option<i64>, name: Option<&str>, icon_hash: &str) -> String {
        if source != "auto" { return self.run_source(kind, appid, &assets, source, sgdb_id, name, icon_hash); }
        let Some(order) = order else { return self.run_source(kind, appid, &assets, "auto", sgdb_id, name, icon_hash) };
        let attempt = |src: &str| -> String {
            if src == "store" {
                return match self.hooks.store_url(appid, kind) { Some(u) if self.download_from_url(appid, &u, kind) != "missing" => "store".into(), _ => "missing".into() };
            }
            self.run_source(kind, appid, &assets, src, sgdb_id, name, icon_hash)
        };
        if prefer_fit && kind != Kind::Icon {
            let mut fallback: Option<(String, Vec<u8>)> = None;
            for src in order {
                let tag = attempt(src);
                if tag == "missing" { continue; }
                if self.store.fits(kind, appid) { return tag; }
                if fallback.is_none() { if let Some(b) = self.store.read(kind, appid) { fallback = Some((tag, b)); } }
            }
            if let Some((tag, bytes)) = fallback { if self.store.save(kind, appid, &bytes) { return tag; } }
            return "missing".into();
        }
        for src in order { let r = attempt(src); if r != "missing" { return r; } }
        "missing".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{NetError, Response};
    use std::sync::Mutex;

    /// A site: JSON routes by URL substring, image routes by substring giving (status, bytes); records every URL asked.
    struct Site { json: Vec<(&'static str, &'static str)>, img: Vec<(&'static str, u16, Vec<u8>)>, seen: Mutex<Vec<String>>, auth: Mutex<Vec<String>> }
    impl Http for Site {
        fn get(&self, u: &str, h: &[(&str, &str)], _t: Duration) -> Result<Response, NetError> {
            self.seen.lock().unwrap().push(u.to_string());
            for (k, v) in h { if *k == "Authorization" { self.auth.lock().unwrap().push(v.to_string()); } }
            self.json.iter().find(|(k, _)| u.contains(k)).map(|(_, b)| Response { status: 200, retry_after: None, body: b.to_string() }).ok_or(NetError("no rule".into()))
        }
        fn get_bytes(&self, u: &str, _h: &[(&str, &str)], _t: Duration) -> Result<(u16, Vec<u8>), NetError> {
            self.seen.lock().unwrap().push(u.to_string());
            self.img.iter().find(|(k, _, _)| u.contains(k)).map(|(_, s, b)| (*s, b.clone())).ok_or(NetError("no rule".into()))
        }
    }
    /// Images are 2 bytes: [shape, id]; shape 1 = fits. Saved covers are kept per (kind, appid).
    #[derive(Default)] struct Mem(Mutex<HashMap<(String, i64), Vec<u8>>>);
    impl ArtStore for Mem {
        fn save(&self, k: Kind, a: i64, b: &[u8]) -> bool { if b.len() != 2 { return false; } self.0.lock().unwrap().insert((k.name().into(), a), b.to_vec()); true }
        fn fits(&self, k: Kind, a: i64) -> bool { self.0.lock().unwrap().get(&(k.name().to_string(), a)).map_or(false, |b| b[0] == 1) }
        fn read(&self, k: Kind, a: i64) -> Option<Vec<u8>> { self.0.lock().unwrap().get(&(k.name().to_string(), a)).cloned() }
    }
    fn art(site: Site, key: Option<&str>) -> (Art, Arc<Site>, Arc<Mem>) {
        let s = Arc::new(site); let m = Arc::new(Mem::default());
        (Art::new(s.clone(), m.clone(), Arc::new(NoHooks), key.map(String::from)), s, m)
    }
    const ASSETS: &str = r#"{"response":{"store_items":[{"assets":{"asset_url_format":"store_item_assets/steam/apps/620/${FILENAME}?t=1","library_capsule_2x":"cap2x.jpg","header_image":"hdr.jpg","community_icon":7}}]}}"#;

    #[test] fn assets_manifest_expands_filenames() {
        let (a, _, _) = art(Site { json: vec![("GetItems", ASSETS)], img: vec![], seen: Default::default(), auth: Default::default() }, None);
        let m = a.steam_assets(620);
        assert_eq!(m["library_capsule_2x"], "https://shared.fastly.steamstatic.com/store_item_assets/store_item_assets/steam/apps/620/cap2x.jpg?t=1");
        assert!(!m.contains_key("asset_url_format") && !m.contains_key("community_icon"));
    }
    #[test] fn vertical_prefers_the_manifest_then_the_cdn() {
        let (a, s, m) = art(Site { json: vec![("GetItems", ASSETS)], img: vec![("cap2x.jpg", 404, vec![]), ("library_600x900_2x.jpg", 200, vec![1, 9])], seen: Default::default(), auth: Default::default() }, None);
        assert_eq!(a.download(Kind::Vertical, 620, None, "auto", None, None, ""), "capsule_2x");
        assert_eq!(m.read(Kind::Vertical, 620), Some(vec![1, 9]));
        assert!(s.seen.lock().unwrap().iter().any(|u| u.contains("cap2x.jpg")));
    }
    #[test] fn steam_only_never_asks_sgdb_and_sgdb_only_never_asks_steam() {
        let (a, s, _) = art(Site { json: vec![], img: vec![], seen: Default::default(), auth: Default::default() }, Some("KEY"));
        assert_eq!(a.download(Kind::Horizontal, 5, None, "steam", None, None, ""), "missing");
        assert!(!s.seen.lock().unwrap().iter().any(|u| u.contains("steamgriddb")));
        s.seen.lock().unwrap().clear();
        assert_eq!(a.download(Kind::Horizontal, 5, None, "sgdb", None, Some("Name"), ""), "missing");
        let seen = s.seen.lock().unwrap().clone();
        assert!(seen.iter().all(|u| u.contains("steamgriddb")), "{seen:?}");
    }
    #[test] fn sgdb_picks_the_best_shape_skips_landscape_for_vertical_and_hides_the_key() {
        let grids = r#"{"success":true,"data":[{"url":"https://x/land.png","width":920,"height":430},{"url":"https://x/anim.png","width":600,"height":900,"animated":true},{"url":"https://x/far.png","width":600,"height":1000},{"url":"https://x/best.png","width":600,"height":900}]}"#;
        let (a, s, m) = art(Site { json: vec![("grids/steam/7", grids)], img: vec![("best.png", 200, vec![1, 3])], seen: Default::default(), auth: Default::default() }, Some("SECRETKEY"));
        assert_eq!(a.download(Kind::Vertical, 7, Some(HashMap::new()), "sgdb", None, None, ""), "sgdb_grid");
        assert_eq!(m.read(Kind::Vertical, 7), Some(vec![1, 3]));
        assert_eq!(s.auth.lock().unwrap()[0], "Bearer SECRETKEY");
        assert!(s.seen.lock().unwrap().iter().all(|u| !u.contains("SECRETKEY")), "the key is a header, never in a URL");
        assert!(!s.seen.lock().unwrap().iter().any(|u| u.contains("land.png") || u.contains("anim.png")));
    }
    #[test] fn sgdb_tries_at_most_five_images() {
        let items: Vec<String> = (0..9).map(|i| format!(r#"{{"url":"https://x/i{i}.png","width":600,"height":900}}"#)).collect();
        let body: &'static str = Box::leak(format!(r#"{{"success":true,"data":[{}]}}"#, items.join(",")).into_boxed_str());
        let (a, s, _) = art(Site { json: vec![("grids/steam/7", body)], img: vec![], seen: Default::default(), auth: Default::default() }, Some("K"));
        assert_eq!(a.download(Kind::Vertical, 7, Some(HashMap::new()), "sgdb", None, None, ""), "missing");
        assert_eq!(s.seen.lock().unwrap().iter().filter(|u| u.contains("/i")).count(), 5);
    }
    #[test] fn icon_chain_sgdb_then_steam_hash() {
        let (a, _, m) = art(Site { json: vec![], img: vec![("abc_2x.jpg", 200, vec![1, 1])], seen: Default::default(), auth: Default::default() }, None);
        assert_eq!(a.download(Kind::Icon, 620, None, "auto", None, None, "abc"), "steam");
        assert!(m.read(Kind::Icon, 620).is_some());
        assert_eq!(a.download(Kind::Icon, 620, None, "auto", None, None, ""), "missing");
    }
    #[test] fn name_fallback_for_a_non_steam_game() {
        let (a, _, m) = art(Site { json: vec![("storesearch", r#"{"items":[{"id":620}]}"#), ("%22appid%22%3A620", ASSETS)], img: vec![("hdr.jpg", 200, vec![1, 2])], seen: Default::default(), auth: Default::default() }, None);
        assert_eq!(a.download(Kind::Horizontal, -4, None, "auto", None, Some("Portal 2"), ""), "header_steam");
        assert_eq!(m.read(Kind::Horizontal, -4), Some(vec![1, 2]));
    }
    #[test] fn prefer_fit_walks_on_and_restores_the_first_when_none_fit() {
        let site = || Site { json: vec![("GetItems", ASSETS), ("grids/steam/9", r#"{"success":true,"data":[{"url":"https://x/g.png","width":600,"height":900}]}"#)],
            img: vec![("cap2x.jpg", 200, vec![0, 1]), ("g.png", 200, vec![0, 2])], seen: Default::default(), auth: Default::default() };
        let order: Vec<String> = ["steam", "sgdb"].iter().map(|s| s.to_string()).collect();
        // neither fits: the first result (the Steam capsule) is put back
        let (a, _, m) = art(site(), Some("K"));
        assert_eq!(a.download_art(Kind::Vertical, 9, None, "auto", Some(&order), true, None, None, ""), "capsule_2x");
        assert_eq!(m.read(Kind::Vertical, 9), Some(vec![0, 1]));
        // without prefer_fit: first source with any art
        let (a, _, _) = art(site(), Some("K"));
        assert_eq!(a.download_art(Kind::Vertical, 9, None, "auto", Some(&order), false, None, None, ""), "capsule_2x");
        // the second source fits: it wins
        let mut s2 = site(); s2.img[1].2 = vec![1, 2];
        let (a, _, m) = art(s2, Some("K"));
        assert_eq!(a.download_art(Kind::Vertical, 9, None, "auto", Some(&order), true, None, None, ""), "sgdb_grid");
        assert_eq!(m.read(Kind::Vertical, 9), Some(vec![1, 2]));
    }
    #[test] fn download_from_steam_uses_the_given_steam_id() {
        let (a, s, _) = art(Site { json: vec![("GetItems", ASSETS)], img: vec![("hdr.jpg", 200, vec![1, 5])], seen: Default::default(), auth: Default::default() }, None);
        assert_eq!(a.download_from_steam(-3, 620, Kind::Horizontal, ""), "steam");
        assert_eq!(a.download_from_steam(-3, 620, Kind::Icon, ""), "missing");
        assert!(s.seen.lock().unwrap()[0].contains("620"));
    }
    #[test] fn picker_lists_fits_first_and_searches_by_name() {
        let grids = r#"{"success":true,"data":[{"url":"u1","thumb":"t1","width":920,"height":430},{"url":"u2","thumb":"t2","width":600,"height":900},{"url":"u3","thumb":"t3","width":600,"height":900,"animated":true}]}"#;
        let search = r#"{"success":true,"data":[{"id":5,"name":"Portal 2","verified":true},{"id":6,"name":"Portal"}]}"#;
        let (a, s, _) = art(Site { json: vec![("grids/game/5", grids), ("search/autocomplete/Portal%202", search)], img: vec![("t2", 200, vec![1, 1])], seen: Default::default(), auth: Default::default() }, Some("K"));
        let o = a.sgdb_options(5, true, Kind::Vertical);
        assert_eq!(o.iter().map(|x| (x.url.as_str(), x.fits)).collect::<Vec<_>>(), [("u2", true), ("u1", false)]);
        assert_eq!(a.sgdb_search("Portal 2"), vec![SgdbGame { id: 5, name: "Portal 2".into(), verified: true }, SgdbGame { id: 6, name: "Portal".into(), verified: false }]);
        assert!(a.sgdb_options(9, false, Kind::Horizontal).is_empty());
        assert!(s.seen.lock().unwrap().iter().any(|u| u.contains("grids/steam/9?dimensions=460x215,920x430")));
        assert_eq!(a.picture_bytes("https://x/t2"), Some(vec![1, 1]));
    }
    #[test] fn quoting_keeps_slashes() { assert_eq!(quote("a b/c&d"), "a%20b/c%26d"); }
}
