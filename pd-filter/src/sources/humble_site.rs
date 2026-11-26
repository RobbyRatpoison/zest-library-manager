//! Reading the library of Humble Bundle's rebuilt website (November 2026). The old `/api/v1/user/*` API is gone; the new library is a Next.js application, and what is known
//! about it was found by recording the real browser's requests with a real session (see Test Log item n_humble_new_site):
//!  * the library list is a server action named `filterUserLibrary`: 5 entries a page, newest first, each `{id, title, subTitle, type, contentId, productEntitlementId, ...}`;
//!    `type` is `steam` (a key) or `download_drm_free` (a download); `contentId` is the old API's machine name (`bastion`); `productEntitlementId` is the bundle (the old API's order);
//!  * the action's id changes with every deploy, so it is read out of the site's own script files each time;
//!  * a bundle's page (`/home/library/<productEntitlementId>`) carries, in its Next.js "flight" payload, every item of the bundle with its downloads per platform.
//! Everything here is pure (text in, values out) except the few functions that take an `Http`.
use crate::net::Http;
use regex::Regex;
use serde_json::{json, Value};
use std::sync::OnceLock;
use std::time::Duration;

pub const LIBRARY_URL: &str = "https://www.humblebundle.com/home/library";
const UA: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:130.0) Gecko/20100101 Firefox/130.0";

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex { cell.get_or_init(|| Regex::new(pat).unwrap()) }

/// The page's flight payload: every `self.__next_f.push([1,"..."])` string, unescaped and joined.
pub fn flight_payload(html: &str) -> String {
    static R: OnceLock<Regex> = OnceLock::new();
    let mut out = String::new();
    for c in re(&R, r#"(?s)self\.__next_f\.push\(\[1,"(.*?)"\]\)"#).captures_iter(html) {
        if let Ok(s) = serde_json::from_str::<String>(&format!("\"{}\"", &c[1])) { out.push_str(&s); }
    }
    out
}

/// Every JSON object that starts `{"id":` before an occurrence of `needle`, decoded (an object nested in a bigger one is found through its own occurrence).
pub fn objects_with(payload: &str, needle: &str) -> Vec<Value> {
    let mut out: Vec<Value> = vec![];
    for (at, _) in payload.match_indices(needle) {
        let Some(start) = payload[..at].rfind("{\"id\":") else { continue };
        let mut it = serde_json::Deserializer::from_str(&payload[start..]).into_iter::<Value>();
        if let Some(Ok(v)) = it.next() { if !out.contains(&v) { out.push(v); } }
    }
    out
}

/// The `/_next/static/chunks/*.js` files a page loads.
pub fn chunk_urls(html: &str) -> Vec<String> {
    static R: OnceLock<Regex> = OnceLock::new();
    let mut v: Vec<String> = re(&R, r#"/_next/static/[^"'\\]+\.js"#).find_iter(html).map(|m| m.as_str().to_string()).collect();
    v.sort(); v.dedup(); v
}

/// The id of the `filterUserLibrary` server action in a script file, if that file defines it.
pub fn find_action_id(js: &str) -> Option<String> { find_named_action(js, "filterUserLibrary") }
/// The id of the server action `name` in a script file, if that file defines it (the id changes with every deploy of the site).
pub fn find_named_action(js: &str, name: &str) -> Option<String> {
    let r = Regex::new(&format!(r#"createServerReference\)?\(\s*"([0-9a-f]{{40,}})"[^;()]{{0,300}}?"{}""#, regex::escape(name))).ok()?;
    r.captures(js).map(|c| c[1].to_string())
}

/// Reads the library page and the scripts it loads until one names the action. Err = what to tell the user.
pub fn discover_action(http: &dyn Http, cookie: &str) -> Result<String, String> {
    let h = [("Cookie", cookie), ("User-Agent", UA), ("Accept", "text/html")];
    let page = http.get_raw(LIBRARY_URL, &h, Duration::from_secs(30)).map_err(|_| "Could not reach Humble Bundle".to_string())?;
    if page.status >= 300 { return Err(format!("Humble's library page answered {}", page.status)); }
    for u in chunk_urls(&page.body) {
        let Ok(js) = http.get(&format!("https://www.humblebundle.com{u}"), &[("User-Agent", UA)], Duration::from_secs(30)) else { continue };
        if js.status != 200 || !js.body.contains("filterUserLibrary") { continue; }
        if let Some(id) = find_action_id(&js.body) { return Ok(id); }
    }
    Err("Humble's library script was not found (the site may have changed again)".into())
}

/// The id of the server action `name` as the scripts of the page at `page_url` define it (a bundle page carries the download actions, the library page does not).
pub fn discover_page_action(http: &dyn Http, cookie: &str, page_url: &str, name: &str) -> Result<String, String> {
    let h = [("Cookie", cookie), ("User-Agent", UA), ("Accept", "text/html")];
    let page = http.get_raw(page_url, &h, Duration::from_secs(30)).map_err(|_| "Could not reach Humble Bundle".to_string())?;
    if page.status >= 300 { return Err(format!("Humble's page answered {}", page.status)); }
    for u in chunk_urls(&page.body) {
        let Ok(js) = http.get(&format!("https://www.humblebundle.com{u}"), &[("User-Agent", UA)], Duration::from_secs(30)) else { continue };
        if js.status != 200 || !js.body.contains(name) { continue; }
        if let Some(id) = find_named_action(&js.body, name) { return Ok(id); }
    }
    Err(format!("Humble's download script ({name}) was not found (the site may have changed again)"))
}

/// The signed download addresses of one item (`getSignedUrlsForEntitlements([[item id]])`): (download key such as `knightclub_win`, file name, address). The addresses are short-lived and
/// must never be logged. The answer is `1:{"ok":true,"data":{"signedUrls":{<item machine name>:{<download key>:{<file name>:<address>}}}}}`; a file's `.torrent` is listed beside it.
pub fn signed_urls(http: &dyn Http, cookie: &str, page_url: &str, action: &str, item_id: &str) -> Result<Vec<(String, String, String)>, String> {
    let h = [("Cookie", cookie), ("User-Agent", UA), ("Accept", "text/x-component"), ("Next-Action", action), ("Content-Type", "text/plain;charset=UTF-8"), ("Origin", "https://www.humblebundle.com"), ("Referer", page_url)];
    let r = http.post(page_url, &h, &json!([[item_id]]).to_string(), Duration::from_secs(40)).map_err(|_| "Could not reach Humble Bundle".to_string())?;
    if r.status == 401 || r.status == 403 { return Err("EXPIRED".into()); }
    if r.status >= 400 { return Err(format!("Humble answered {} when asked for the download", r.status)); }
    let v: Value = r.body.lines().find_map(|l| l.strip_prefix("1:")).and_then(|l| serde_json::from_str(l).ok()).ok_or("Humble sent an unexpected answer for the download")?;
    if v.get("ok").and_then(Value::as_bool) != Some(true) { return Err("Humble would not give a download address for this item".into()); }
    let mut out = vec![];
    for (_, keys) in v.pointer("/data/signedUrls").and_then(Value::as_object).into_iter().flatten() {
        for (key, files) in keys.as_object().into_iter().flatten() {
            for (file, url) in files.as_object().into_iter().flatten() { if let Some(u) = url.as_str() { out.push((key.clone(), file.clone(), u.to_string())); } }
        }
    }
    Ok(out)
}

/// The form the action takes: the fields in the order the browser sends them, then the root part `0` last (it must come last, or the fields are not seen). `sort` has to be a real
/// value (`mostRecent`): an empty one silently gives the default page. `hide_revealed` leaves out keys already redeemed (a third of the library here); downloads are never hidden.
pub fn multipart_body(boundary: &str, page: usize, query: &str, hide_revealed: bool) -> String {
    let fields = [("requestId", "1".to_string()), ("query", query.to_string()), ("sort", "mostRecent".to_string()), ("cursor", (5 * page).to_string()), ("page", page.to_string()), ("limit", "5".to_string()),
        ("hideRevealed", hide_revealed.to_string()), ("hideExpired", "false".to_string()), ("dateExpires", String::new())];
    let mut b = String::new();
    for (k, v) in &fields { b.push_str(&format!("--{boundary}\r\nContent-Disposition: form-data; name=\"_1_{k}\"\r\n\r\n{v}\r\n")); }
    let root = json!([{"results": [], "pagination": {"page": 1, "totalPages": 1, "totalDocs": 0}, "success": true}, "$K1"]);
    b.push_str(&format!("--{boundary}\r\nContent-Disposition: form-data; name=\"0\"\r\n\r\n{root}\r\n--{boundary}--\r\n"));
    b
}

/// One entry of the library list.
#[derive(Clone, Debug, PartialEq)]
pub struct Item { pub id: String, pub title: String, pub sub_title: String, pub kind: String, pub content_id: Option<String>, pub bundle_id: String }
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ListPage { pub items: Vec<Item>, pub page: usize, pub total_pages: usize, pub total_docs: usize }

fn s(v: &Value, k: &str) -> String { v.get(k).and_then(|x| x.as_str()).filter(|x| !x.starts_with('$')).unwrap_or("").to_string() }

/// The action's answer (`text/x-component`): the line `1:{...}` is the page.
pub fn parse_list_response(body: &str) -> Result<ListPage, String> {
    let line = body.lines().find_map(|l| l.strip_prefix("1:")).ok_or("Humble's answer had no library page")?;
    let v: Value = serde_json::from_str(line).map_err(|_| "Humble's library page could not be read".to_string())?;
    if v.get("success").and_then(|x| x.as_bool()) != Some(true) { return Err("Humble refused the library request".into()); }
    let p = v.get("pagination").ok_or("Humble's answer had no page count")?;
    let n = |k: &str| p.get(k).and_then(|x| x.as_u64()).unwrap_or(0) as usize;
    let items = v.get("results").and_then(|r| r.as_array()).into_iter().flatten().map(|r| Item {
        id: s(r, "id"), title: s(r, "title"), sub_title: s(r, "subTitle"), kind: s(r, "type"),
        content_id: Some(s(r, "contentId")).filter(|c| !c.is_empty()), bundle_id: s(r, "productEntitlementId") }).collect();
    Ok(ListPage { items, page: n("page"), total_pages: n("totalPages"), total_docs: n("totalDocs") })
}

/// Page `page` of the list.
pub fn fetch_list_page(http: &dyn Http, cookie: &str, action: &str, page: usize, query: &str, hide_revealed: bool) -> Result<ListPage, String> {
    let boundary = "----ZestHumbleBoundary7c1d";
    let ct = format!("multipart/form-data; boundary={boundary}");
    let h = [("Cookie", cookie), ("User-Agent", UA), ("Accept", "text/x-component"), ("Next-Action", action), ("Content-Type", ct.as_str()), ("Origin", "https://www.humblebundle.com"), ("Referer", LIBRARY_URL)];
    let r = http.post(LIBRARY_URL, &h, &multipart_body(boundary, page, query, hide_revealed), Duration::from_secs(40)).map_err(|_| "Could not reach Humble Bundle".to_string())?;
    if r.status == 401 || r.status == 403 { return Err("EXPIRED".into()); }
    if r.status >= 400 { return Err(format!("Humble's library answered {} (the site may have changed)", r.status)); }
    parse_list_response(&r.body)
}

/// One bundle's DRM-free items, from its page, shaped like the old API's order so the old rules (`is_game`, `records_from_order`) apply unchanged:
/// `{"created": null, "product": {"human_name": <bundle name>, "machine_name": <bundle id>}, "subproducts": [{"machine_name", "human_name", "downloads": [{"platform", "download_struct": [{"name"}]}]}]}`.
/// `content_ids` maps an item's id to its machine name (the page does not carry it; the list does). An item the map does not know is left out; an empty map means "no map" and the title is used.
pub fn bundle_order(html: &str, bundle_id: &str, bundle_name: &str, content_ids: &std::collections::HashMap<String, String>) -> Value {
    let payload = flight_payload(html);
    let mut subs: Vec<Value> = vec![];
    for it in objects_with(&payload, "\"type\":\"download_drm_free\"") {
        if it.get("type").and_then(|t| t.as_str()) != Some("download_drm_free") { continue; }
        let id = s(&it, "id");
        // with no map at all (a check that only reads downloads) the title stands in for the machine name; with one, an item it does not know is left out
        let machine = match content_ids.get(&id) { Some(m) => m.clone(), None if content_ids.is_empty() => s(&it, "title"), None => continue };
        let downloads: Vec<Value> = it.get("downloads").and_then(|d| d.as_array()).into_iter().flatten().map(|d| json!({
            "platform": s(d, "platform"), "key": s(d, "key"),
            "download_struct": d.get("downloadStruct").and_then(|x| x.as_array()).into_iter().flatten().map(|x| json!({"name": s(x, "name"), "human_size": s(x, "size")})).collect::<Vec<_>>() })).collect();
        if subs.iter().any(|x| x["machine_name"] == json!(&machine)) { continue; }
        subs.push(json!({"machine_name": machine, "human_name": s(&it, "title"), "item_id": id, "downloads": downloads}));
    }
    json!({"created": null, "product": {"human_name": bundle_name, "machine_name": bundle_id}, "subproducts": subs})
}

/// `GET /api/auth/session-status`: Some(true) signed in, Some(false) signed out, None when Humble did not say.
pub fn session_status(http: &dyn Http, cookie: &str) -> Option<bool> {
    let r = http.get("https://www.humblebundle.com/api/auth/session-status", &[("Cookie", cookie), ("User-Agent", UA), ("Accept", "application/json")], Duration::from_secs(20)).ok()?;
    if r.status == 401 || r.status == 403 { return Some(false); }
    if r.status != 200 { return None; }
    let v: Value = serde_json::from_str(&r.body).ok()?;
    let st = v.get("status").and_then(|x| x.as_str()).unwrap_or("").to_lowercase();
    if v.get("authenticated").and_then(|x| x.as_bool()) == Some(true) { return Some(true); }
    Some(!st.is_empty() && !["anonymous", "unauthenticated", "signed_out", "none"].contains(&st.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{NetError, Raw, Response};
    use std::collections::HashMap;

    struct SignSite;
    impl Http for SignSite {
        fn get(&self, _u: &str, _h: &[(&str, &str)], _t: Duration) -> Result<Response, NetError> { Ok(Response { status: 200, retry_after: None, body: String::new() }) }
        fn post(&self, _u: &str, h: &[(&str, &str)], body: &str, _t: Duration) -> Result<Response, NetError> {
            assert!(h.iter().any(|(k, v)| *k == "Next-Action" && *v == "ACTION"), "the page's own action id is used");
            assert_eq!(body, r#"[["item-1"]]"#, "the item's id goes in as the one argument");
            Ok(Response { status: 200, retry_after: None, body: "0:{\"a\":\"$@1\"}\n1:{\"ok\":true,\"data\":{\"signedUrls\":{\"game\":{\"game_win\":{\"Game.zip\":\"https://cdn.humble.com/Game.zip?t=1\",\"Game.zip.torrent\":\"https://cdn.humble.com/Game.zip.torrent?t=1\"}}},\"expiresAt\":\"x\"}}\n".into() })
        }
    }
    #[test] fn signed_addresses_come_back_per_download_key_and_file() {
        let got = signed_urls(&SignSite, "c", "https://www.humblebundle.com/home/library/B", "ACTION", "item-1").unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!((got[0].0.as_str(), got[0].1.as_str()), ("game_win", "Game.zip"));
        assert!(got[0].2.starts_with("https://cdn.humble.com/"));
        let js = r#"x=createServerReference)("70ecfc5fcadcce725d6ea74231b7c4789611a9fb21",c,void 0,f,"getSignedUrlsForEntitlements"),y=createServerReference)("402926cca843cd6f792d586d17f97bd7ff116a4149",c,void 0,f,"recordDownload")"#;
        assert_eq!(find_named_action(js, "getSignedUrlsForEntitlements").as_deref(), Some("70ecfc5fcadcce725d6ea74231b7c4789611a9fb21"));
        assert_eq!(find_named_action(js, "recordDownload").as_deref(), Some("402926cca843cd6f792d586d17f97bd7ff116a4149"));
        assert_eq!(find_named_action(js, "nothing"), None);
    }
    /// A bundle page as Humble sends it: the flight payload is a list of JS strings; this one holds a Steam key item (whose key text must never matter) and two downloads.
    fn page() -> String {
        let items = r#"[{"id":"i-key","title":"Some Steam Game","type":"steam","redemptionData":{"keyFormatted":"AAAAA-BBBBB-CCCCC"},"downloads":[]},{"id":"i-bastion","title":"Bastion","type":"download_drm_free","redemptionData":{},"downloads":[{"id":"d1","key":"bastion_windows","platform":"windows","downloadStruct":[{"name":"ZIP","size":"1 GB","url":{"web":"Bastion.zip","bittorrent":"Bastion.zip.torrent"}}]},{"id":"d2","key":"bastion_linux","platform":"linux","downloadStruct":[{"name":"OTHER","size":"1 GB"}]}],"signedUrls":"$undefined"},{"id":"i-ost","title":"Bastion Soundtrack","type":"download_drm_free","redemptionData":{},"downloads":[{"id":"d3","key":"bastion_audio","platform":"audio","downloadStruct":[{"name":"FLAC","size":"300 MB"}]}]}]"#;
        let esc = serde_json::to_string(items).unwrap();   // a JS string literal, quotes escaped
        format!(r#"<html><script>self.__next_f.push([1,{esc}])</script><script src="/_next/static/chunks/a1.js"></script></html>"#)
    }
    fn ids() -> HashMap<String, String> { [("i-bastion", "bastion"), ("i-ost", "bastion_soundtrack")].into_iter().map(|(a, b)| (a.to_string(), b.to_string())).collect() }

    #[test] fn the_flight_payload_is_unescaped_and_its_drm_free_items_are_found() {
        let p = flight_payload(&page());
        assert!(p.contains("\"type\":\"download_drm_free\"") && p.contains("\"title\":\"Bastion\""));
        let found = objects_with(&p, "\"type\":\"download_drm_free\"");
        assert_eq!(found.iter().map(|o| o["title"].as_str().unwrap()).collect::<Vec<_>>(), vec!["Bastion", "Bastion Soundtrack"], "the Steam key item is not one of them");
    }
    #[test] fn a_bundle_becomes_an_old_shaped_order_the_old_rules_can_read() {
        let o = bundle_order(&page(), "b1", "Yogscast Jingle Jam 2019", &ids());
        assert_eq!(o["product"]["human_name"], "Yogscast Jingle Jam 2019");
        let subs = o["subproducts"].as_array().unwrap();
        assert_eq!(subs.len(), 2);
        assert_eq!(subs[0]["machine_name"], "bastion"); assert_eq!(subs[0]["human_name"], "Bastion");
        assert_eq!(subs[0]["downloads"][0]["platform"], "windows"); assert_eq!(subs[0]["downloads"][0]["download_struct"][0]["name"], "ZIP");
        assert!(super::super::humble::is_game(&subs[0], Some(&o)), "a Windows download is a game");
        assert!(!super::super::humble::is_game(&subs[1], Some(&o)), "a soundtrack (audio only, 'soundtrack' in the name) is not");
        let one: HashMap<String, String> = [("i-bastion".to_string(), "bastion".to_string())].into_iter().collect();
        assert_eq!(bundle_order(&page(), "b1", "x", &one)["subproducts"].as_array().unwrap().len(), 1, "an item the map does not know is left out");
        assert_eq!(bundle_order(&page(), "b1", "x", &HashMap::new())["subproducts"][0]["machine_name"], "Bastion", "no map: the title stands in");
    }
    #[test] fn the_action_id_is_read_from_the_script_that_names_it() {
        let js = r#"let rl=(0,a0.createServerReference)("607622c68ed0fa8a71acd7ff343f9f3366703af38e",a0.callServer,void 0,a0.findSourceMapURL,"filterUserLibrary");let rm=(0,a0.createServerReference)("4046053fd4197d81e52124dfddf4759167feeffd36",a0.callServer,void 0,a0.findSourceMapURL,"saveLibraryShowRedeemed");"#;
        assert_eq!(find_action_id(js).as_deref(), Some("607622c68ed0fa8a71acd7ff343f9f3366703af38e"));
        assert_eq!(find_action_id(r#"createServerReference)("4046053fd4197d81e52124dfddf4759167feeffd36",x,void 0,y,"saveLibraryShowRedeemed")"#), None);
        assert_eq!(chunk_urls(&page()), vec!["/_next/static/chunks/a1.js".to_string()]);
    }
    #[test] fn the_form_has_the_fields_first_and_the_root_part_last() {
        let b = multipart_body("BB", 3, "Bas tion", true);
        let names: Vec<&str> = b.split("name=\"").skip(1).map(|p| p.split('"').next().unwrap()).collect();
        assert_eq!(names, vec!["_1_requestId", "_1_query", "_1_sort", "_1_cursor", "_1_page", "_1_limit", "_1_hideRevealed", "_1_hideExpired", "_1_dateExpires", "0"]);
        assert!(b.contains("name=\"_1_sort\"\r\n\r\nmostRecent\r\n") && b.contains("name=\"_1_cursor\"\r\n\r\n15\r\n") && b.contains("name=\"_1_hideRevealed\"\r\n\r\ntrue\r\n") && b.ends_with("--BB--\r\n"));
        assert!(b.contains("[{\"pagination\"") || b.contains("\"$K1\""), "the root names the form as its second argument");
    }
    #[test] fn a_list_answer_gives_its_entries_and_page_counts() {
        let body = "0:{\"a\":\"$@1\"}\n1:{\"results\":[{\"id\":\"i1\",\"title\":\"Bastion\",\"subTitle\":\"Jam\",\"type\":\"download_drm_free\",\"contentId\":\"bastion\",\"productEntitlementId\":\"b1\"},{\"id\":\"i2\",\"title\":\"K\",\"subTitle\":\"$undefined\",\"type\":\"steam\",\"contentId\":\"k_steam\",\"productEntitlementId\":\"b2\"}],\"pagination\":{\"page\":2,\"totalPages\":704,\"totalDocs\":3519},\"success\":true}\n";
        let p = parse_list_response(body).unwrap();
        assert_eq!((p.page, p.total_pages, p.total_docs, p.items.len()), (2, 704, 3519, 2));
        assert_eq!(p.items[0], Item { id: "i1".into(), title: "Bastion".into(), sub_title: "Jam".into(), kind: "download_drm_free".into(), content_id: Some("bastion".into()), bundle_id: "b1".into() });
        assert_eq!(p.items[1].sub_title, "", "an undefined value is empty, not the text '$undefined'");
        assert!(parse_list_response("0:{}\n").is_err() && parse_list_response("1:{\"success\":false}\n").is_err());
    }
    struct Site;
    impl Http for Site {
        fn get(&self, u: &str, _h: &[(&str, &str)], _t: Duration) -> Result<Response, NetError> {
            if u.ends_with("session-status") { return Ok(Response { status: 200, retry_after: None, body: r#"{"status":"authenticated"}"#.into() }); }
            Ok(Response { status: 200, retry_after: None, body: if u.ends_with("a1.js") { r#"x=createServerReference)("607622c68ed0fa8a71acd7ff343f9f3366703af38e",c,void 0,f,"filterUserLibrary")"#.into() } else { "nothing here".into() } })
        }
        fn get_raw(&self, _u: &str, _h: &[(&str, &str)], _t: Duration) -> Result<Raw, NetError> { Ok(Raw { status: 200, headers: vec![], body: page() }) }
    }
    #[test] fn the_action_is_discovered_and_a_session_is_checked() {
        assert_eq!(discover_action(&Site, "c").unwrap(), "607622c68ed0fa8a71acd7ff343f9f3366703af38e");
        assert_eq!(session_status(&Site, "c"), Some(true));
    }
}
