//! Amazon Games as a library source (port of PlayDate's `amazon_games` plugin: `amazon_games.py`, `__init__.py` and the sign-in route in `routes.py`; launch, uninstall and the
//! SDS manifest reader are built; install is in `install`, the manifest reader in `sds.rs`). Sign-in is the Amazon Games launcher's own device registration: a PKCE sign-in page whose landing address (carrying
//! `openid.oa2.authorization_code=`) the user pastes, exchanged at `api.amazon.com/auth/register` for a bearer and refresh token. The library is the launcher's `GetEntitlements` call,
//! or, when Heroic's Nile client has a `library.json`, that file (no login needed). Tokens live in `config.json["amazon_games"]` as PlayDate keeps them and are never logged.
use super::{Action, AuthBegin, AuthStatus, Caps, GameRef, Installed, InstalledRow, Launch, LibrarySource, SourceCtx};
use crate::importer::GameRecord;
use crate::jobs::Update;
use crate::net::{enc, log, set_activity, Cancel, Http};
use crate::steamclient::Fetch;
use rusqlite::types::Value as Sql;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const API: &str = "https://api.amazon.com";
const ENTITLEMENTS: &str = "https://gaming.amazon.com/api/distribution/entitlements";
const MARKETPLACE_ID: &str = "ATVPDKIKX0DER";
const APP_NAME: &str = "AGSLauncher for Windows";
const APP_VERSION: &str = "1.0.0";
const CLIENT_UA: &str = "com.amazon.agslauncher.win/3.0.9202.1";
const DEVICE_TYPE: &str = "A2UMVHOX7UP4V7";
const STORE: &str = "https://gaming.amazon.com/home";
const DISTRIBUTION: &str = "https://gaming.amazon.com/api/distribution/v2/public";
const SDK_CHANNEL: &str = "https://gaming.amazon.com/api/distribution/v2/public/download/channel/87d38116-4cbf-4af0-a371-a5b498975346";
const NOT_CONNECTED: &str = "Not connected \u{2014} please connect your Amazon account first.";

fn pause(ms: u64) { if !cfg!(test) { std::thread::sleep(Duration::from_millis(ms)); } }
fn now() -> i64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0) }
fn hex(b: &[u8], upper: bool) -> String { b.iter().map(|x| if upper { format!("{x:02X}") } else { format!("{x:02x}") }).collect() }
fn b64url(b: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut o = String::new();
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..=c.len() { o.push(T[(n >> (18 - 6 * i) & 63) as usize] as char); }
    }
    o
}
fn sha256(b: &[u8]) -> Vec<u8> { ring::digest::digest(&ring::digest::SHA256, b).as_ref().to_vec() }
fn random(n: usize) -> Result<Vec<u8>, String> { use ring::rand::SecureRandom; let mut v = vec![0u8; n]; ring::rand::SystemRandom::new().fill(&mut v).map_err(|_| "No source of randomness".to_string())?; Ok(v) }
fn to_i64(v: &Value) -> Option<i64> { match v { Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f.trunc() as i64)), Value::String(s) => s.trim().parse().ok(), _ => None } }

// ---------------------------------------------------------------- pure helpers (checked against the Python)

/// `_generate_pkce`: (verifier, challenge) for 32 random bytes.
pub fn pkce(random32: &[u8]) -> (String, String) { let v = b64url(random32); let c = b64url(&sha256(v.as_bytes())); (v, c) }
/// The launcher's client id: the device serial and type, hex encoded.
pub fn client_id(serial: &str) -> String { hex(format!("{serial}#{DEVICE_TYPE}").as_bytes(), false) }
/// `get_auth_url`: the sign-in page for one attempt.
pub fn auth_url(client_id: &str, challenge: &str) -> String {
    let ns = "http://specs.openid.net/auth/2.0";
    let params: [(&str, String); 16] = [("openid.ns", ns.into()), ("openid.claimed_id", format!("{ns}/identifier_select")), ("openid.identity", format!("{ns}/identifier_select")), ("openid.mode", "checkid_setup".into()),
        ("openid.oa2.scope", "device_auth_access".into()), ("openid.ns.oa2", "http://www.amazon.com/ap/ext/oauth/2".into()), ("openid.oa2.response_type", "code".into()), ("openid.oa2.code_challenge_method", "S256".into()),
        ("openid.oa2.client_id", format!("device:{client_id}")), ("language", "en_US".into()), ("marketPlaceId", MARKETPLACE_ID.into()), ("openid.return_to", "https://www.amazon.com".into()),
        ("openid.pape.max_auth_age", "0".into()), ("openid.assoc_handle", "amzn_sonic_games_launcher".into()), ("pageId", "amzn_sonic_games_launcher".into()), ("openid.oa2.code_challenge", challenge.into())];
    format!("https://amazon.com/ap/signin?{}", params.iter().map(|(k, v)| format!("{}={}", enc(k), enc(v))).collect::<Vec<_>>().join("&"))
}
/// `exchange_code`'s first step: the authorization code from the pasted landing address, or the bare code. None = no code in it.
pub fn code_from(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let code = if raw.contains("openid.oa2.authorization_code") {
        let q = raw.split_once('?').map(|(_, q)| q.split('#').next().unwrap_or("")).unwrap_or("");
        q.split('&').filter_map(|kv| kv.split_once('=')).find(|(k, _)| super::gog::unquote(k) == "openid.oa2.authorization_code").map(|(_, v)| super::gog::unquote(v)).unwrap_or_default()
    } else if raw.starts_with("http") { String::new() } else { raw.to_string() };   // an address without the code is not a code (PlayDate sent it on and Amazon refused it)
    (!code.is_empty()).then_some(code)
}
/// The body of the device registration.
pub fn register_body(code: &str, client_id: &str, verifier: &str, serial: &str) -> Value {
    json!({"auth_data": {"authorization_code": code, "client_domain": "DeviceLegacy", "client_id": client_id, "code_algorithm": "SHA-256", "code_verifier": verifier, "use_global_authentication": false},
        "registration_data": {"app_name": APP_NAME, "app_version": APP_VERSION, "device_model": "Windows", "device_name": null, "device_serial": serial, "device_type": DEVICE_TYPE, "domain": "Device", "os_version": "10.0.19044.0"},
        "requested_extensions": ["customer_info", "device_info"], "requested_token_type": ["bearer", "mac_dms"], "user_context_map": {}})
}
pub struct Registered { pub access_token: String, pub refresh_token: String, pub expires_in: i64, pub device_serial: String, pub username: String }
/// The tokens in a registration answer; None = an unexpected shape.
pub fn parse_registration(v: &Value) -> Option<Registered> {
    let ok = v.get("response")?.get("success")?;
    let bearer = ok.get("tokens")?.get("bearer")?;
    let s = |x: &Value| x.as_str().map(String::from);
    Some(Registered { access_token: s(bearer.get("access_token")?)?, refresh_token: s(bearer.get("refresh_token")?)?, expires_in: bearer.get("expires_in").map_or(Some(3600), to_i64)?,
        device_serial: s(ok.get("extensions")?.get("device_info")?.get("device_serial_number")?)?,
        username: ok.get("extensions")?.get("customer_info")?.get("given_name").and_then(|g| g.as_str()).filter(|g| !g.is_empty()).unwrap_or("Connected").to_string() })
}
/// `_entitlements_request`'s body.
pub fn entitlements_body(next_token: Option<&str>, device_serial: &str) -> Value {
    json!({"Operation": "GetEntitlements", "clientId": "Sonic", "syncPoint": null, "nextToken": next_token, "maxResults": 50, "productIdFilter": null,
        "keyId": "d5dc8b8b-86c8-4fc4-ae93-18c0def5314d", "hardwareHash": hex(&sha256(device_serial.as_bytes()), true)})
}
fn nonblank(v: Option<&Value>) -> String { v.and_then(|x| x.as_str()).unwrap_or("").trim().to_string() }
fn product_id(v: &Value) -> String { match v { Value::String(s) => s.trim().to_string(), Value::Number(n) => n.to_string(), _ => String::new() } }

/// `_sync_from_api`'s records: one per entitlement with a product id. The SKU rides in `appname` (the launcher needs it to start the game), genres come from the product detail.
pub fn records_from_entitlements(entries: &[Value], seen: &mut HashSet<String>, now: i64) -> Vec<GameRecord> {
    let mut out = vec![];
    for e in entries {
        let Some(p) = e.get("product").filter(|p| p.as_object().map_or(false, |o| !o.is_empty())) else { continue };
        let id = p.get("id").map(product_id).unwrap_or_default();
        if id.is_empty() || !seen.insert(id.clone()) { continue; }
        let title = nonblank(p.get("title"));
        let genres = p.get("productDetail").and_then(|d| d.get("details")).and_then(|d| d.get("genres")).and_then(|g| g.as_array())
            .and_then(|a| a.iter().map(|x| x.as_str()).collect::<Option<Vec<_>>>()).map(|g| g.join(",")).filter(|g| !g.is_empty());
        out.push(GameRecord { platform_id: id.clone(), name: if title.is_empty() { id } else { title }, appname: Some(nonblank(p.get("sku"))).filter(|s| !s.is_empty()), genres, date_added: Some(now), ..Default::default() });
    }
    out
}
/// `_sync_from_nile`'s records from Nile's `library.json` (a list, or an object holding `library` / `games`) and the ids in its `installed.json` (a list of objects or an object keyed by id).
pub fn records_from_nile(library: &Value, installed: &HashSet<String>, seen: &mut HashSet<String>, now: i64) -> Vec<GameRecord> {
    let games: Vec<Value> = match library { Value::Object(o) => o.get("library").or_else(|| o.get("games")).and_then(|g| g.as_array()).cloned().unwrap_or_default(), Value::Array(a) => a.clone(), _ => vec![] };
    let mut out = vec![];
    for g in &games {
        let product = g.get("product").filter(|p| p.as_object().map_or(false, |o| !o.is_empty())).unwrap_or(g);
        let id = { let a = product.get("id").map(product_id).unwrap_or_default(); if a.is_empty() { g.get("id").map(product_id).unwrap_or_default() } else { a } };
        if id.is_empty() || !seen.insert(id.clone()) { continue; }
        let title = nonblank(product.get("title"));
        out.push(GameRecord { platform_id: id.clone(), name: if title.is_empty() { id.clone() } else { title }, appname: Some(nonblank(product.get("sku"))).filter(|s| !s.is_empty()),
            installed: Some(installed.contains(&id)), date_added: Some(now), ..Default::default() });
    }
    out
}
/// The ids in Nile's `installed.json`.
pub fn nile_installed_ids(v: &Value) -> HashSet<String> {
    match v { Value::Array(a) => a.iter().map(|g| g.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string()).collect(), Value::Object(o) => o.keys().cloned().collect(), _ => HashSet::new() }
}

// ---------------------------------------------------------------- the source

/// A game's launch descriptor, `fuel.json` at the root of its install: the program, its arguments and a working-folder override (all relative to the install folder).
#[derive(Clone, Debug, PartialEq)]
pub struct Fuel { pub command: String, pub args: Vec<String>, pub cwd_override: Option<String> }
pub fn read_fuel(dir: &std::path::Path) -> Option<Fuel> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("fuel.json")).ok()?).ok()?;
    let main = &v["Main"];
    let sep = |s: &str| s.replace('\\', "/");
    let command = sep(main["Command"].as_str().unwrap_or(""));
    if command.is_empty() { return None; }
    Some(Fuel { command, args: main["Args"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default(),
        cwd_override: Some(sep(main["WorkingSubdirOverride"].as_str().unwrap_or(""))).filter(|c| !c.is_empty()) })
}

/// `url` with `suffix` added to its path (a query or fragment stays at the end).
fn path_url(url: &str, suffix: &str) -> String {
    let cut = url.find(['?', '#']).unwrap_or(url.len());
    format!("{}{}{}", &url[..cut], suffix, &url[cut..])
}
/// `rel` (a manifest path, either slash) below `base`; None for anything that would leave it.
fn safe_join(base: &std::path::Path, rel: &str) -> Option<PathBuf> {
    let parts: Vec<&str> = rel.split(['/', '\\']).filter(|p| !p.is_empty() && *p != ".").collect();
    if parts.is_empty() || parts.iter().any(|p| *p == ".." || p.contains(':')) { return None; }
    Some(parts.iter().fold(base.to_path_buf(), |b, p| b.join(p)))
}
fn sha256_file(path: &std::path::Path) -> Option<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buf = vec![0u8; 1 << 20];
    loop { let n = f.read(&mut buf).ok()?; if n == 0 { break; } ctx.update(&buf[..n]); }
    Some(hex(ctx.finish().as_ref(), false))
}
fn folder_name(name: &str, fallback: &str) -> String {
    let s: String = name.chars().filter(|c| c.is_alphanumeric() || c.is_whitespace() || matches!(c, '_' | '.' | '-')).collect();
    let s = s.trim().to_string();
    if s.is_empty() { fallback.to_string() } else { s }
}

pub struct AmazonSource { http: Arc<dyn Http>, config_dir: PathBuf, db: PathBuf, home: PathBuf, pending: Mutex<Option<(String, String)>>, /** manifest signing key override (tests); None = Amazon's */ verify_key: Option<Vec<u8>> }

impl AmazonSource {
    pub fn new(http: Arc<dyn Http>, config_dir: PathBuf, db: PathBuf) -> Self {
        let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_default();
        AmazonSource { http, config_dir, db, home, pending: Mutex::new(None), verify_key: None }
    }
    /// `GetGameDownload`: the base address of an owned game's download (its manifest and files hang off it).
    fn game_download(&self, game_id: &str, token: &str) -> Result<String, String> {
        let h = [("X-Amz-Target", "com.amazon.animusdistributionservice.external.AnimusDistributionService.GetGameDownload"), ("x-amzn-token", token), ("UserAgent", CLIENT_UA), ("Content-Type", "application/json"), ("Content-Encoding", "amz-1.0")];
        let r = self.post(DISTRIBUTION, &h, &json!({"entitlementId": game_id, "Operation": "GetGameDownload"}), 30)?;
        if r.status >= 400 { return Err(format!("GetGameDownload failed (HTTP {})", r.status)); }
        let j: Value = serde_json::from_str(&r.body).map_err(|_| "Unexpected download answer".to_string())?;
        j.get("downloadUrl").and_then(|v| v.as_str()).filter(|u| !u.is_empty()).map(String::from).ok_or_else(|| "Amazon did not return a download URL for this game".into())
    }
    fn manifest(&self, download_url: &str) -> Result<crate::sds::Manifest, String> {
        let (status, body) = self.http.get_bytes(&path_url(download_url, "/manifest.proto"), &[], Duration::from_secs(30)).map_err(|e| e.to_string())?;
        if status >= 400 { return Err(format!("HTTP {status}")); }
        crate::sds::parse_manifest_with(&body, Some(self.verify_key.clone().unwrap_or_else(crate::sds::amazon_key))).map_err(|e| e.to_string())
    }
    /// Amazon's own SDK files (the Fuel SDK DLLs every native Amazon game reads), fetched once into `amazon_games_sdk/`. Best effort: a game may still run without them.
    fn ensure_sdk(&self, cancel: &Cancel) {
        let dir = self.config_dir.join("amazon_games_sdk");
        if dir.join(".done").is_file() { return; }
        let run = || -> Result<usize, String> {
            let r = self.http.get(SDK_CHANNEL, &[], Duration::from_secs(20)).map_err(|e| e.to_string())?;
            let j: Value = serde_json::from_str(&r.body).map_err(|_| "bad channel answer".to_string())?;
            let base = j.get("downloadUrl").and_then(|v| v.as_str()).ok_or("no download url")?.to_string();
            let mut wrote = 0;
            for pkg in self.manifest(&base)?.packages { for f in pkg.files {
                if !f.path.contains("FuelSDK_x64.dll") && !f.path.contains("AmazonGamesSDK_") { continue; }
                let Some(dest) = safe_join(&dir, &f.path) else { continue };
                if let Some(d) = dest.parent() { let _ = std::fs::create_dir_all(d); }
                super::folder::download(self.http.as_ref(), &path_url(&base, &format!("/files/{}", f.hash.value)), &dest, cancel, &mut |_, _| {})?;
                wrote += 1;
            } }
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            std::fs::write(dir.join(".done"), "1").map_err(|e| e.to_string())?;
            Ok(wrote)
        };
        match run() { Ok(n) => log(&format!("Amazon: Fuel SDK downloaded ({n} files) to {}", dir.display())), Err(e) => log(&format!("Amazon: Fuel SDK download failed (some games may fail to start): {e}")) }
    }

    fn section(&self) -> Map<String, Value> { crate::account::plugin_section(&self.config_dir, "amazon_games") }
    /// Edits `config.json["amazon_games"]` keeping every other key (`expires_at` stays a number).
    fn edit(&self, f: impl FnOnce(&mut Map<String, Value>)) {
        crate::account::plugin_section_edit(&self.config_dir, "amazon_games", f);   // secrets go to the keyring, the rest stays in config.json
    }
    fn str_of(&self, k: &str) -> Option<String> { self.section().get(k).and_then(|v| v.as_str()).filter(|v| !v.is_empty()).map(String::from) }
    /// The Nile library file, when Heroic's Nile client has written one.
    fn nile_paths(&self, file: &str) -> Vec<PathBuf> {
        ["", ".config/heroic/nile_config/"].iter().map(|p| self.home.join(".config").join(if p.is_empty() { "nile" } else { "heroic/nile_config/nile" }).join(file)).collect::<Vec<_>>().into_iter()
            .chain(std::iter::once(self.home.join(".var/app/com.heroicgameslauncher.hgl/config/heroic/nile_config/nile").join(file))).collect()
    }
    fn nile_file(&self, file: &str) -> Option<PathBuf> { self.nile_paths(file).into_iter().find(|p| p.is_file()) }

    fn post(&self, url: &str, headers: &[(&str, &str)], body: &Value, secs: u64) -> Result<crate::net::Response, String> {
        self.http.post(url, headers, &body.to_string(), Duration::from_secs(secs)).map_err(|e| e.to_string())
    }
    /// `/auth/token`: a new access token from the refresh token. Ok(None) = Amazon refused it; Err = no answer.
    fn refresh(&self, refresh_token: &str) -> Result<Result<(String, i64), u16>, String> {
        let body = json!({"source_token": refresh_token, "source_token_type": "refresh_token", "requested_token_type": "access_token", "app_name": APP_NAME, "app_version": APP_VERSION});
        let r = self.post(&format!("{API}/auth/token"), &[("Content-Type", "application/json")], &body, 20).map_err(|e| { log(&format!("Amazon: token refresh failed: {e}")); e })?;
        if r.status >= 400 { log(&format!("Amazon: token refresh HTTP {}", r.status)); return Ok(Err(r.status)); }
        let j: Value = serde_json::from_str(&r.body).map_err(|_| "Unexpected token answer".to_string())?;
        let at = j.get("access_token").and_then(|t| t.as_str()).ok_or("Unexpected token answer")?.to_string();
        Ok(Ok((at, j.get("expires_in").and_then(to_i64).unwrap_or(3600))))
    }
    /// A valid access token, refreshed when it expires within a minute.
    fn token(&self) -> Option<String> {
        let at = self.str_of("access_token")?;
        if now() <= self.section().get("expires_at").and_then(to_i64).unwrap_or(0) - 60 { return Some(at); }
        let rt = self.str_of("refresh_token")?;
        match self.refresh(&rt) {
            Ok(Ok((t, exp))) => { self.edit(|m| { m.insert("access_token".into(), json!(t)); m.insert("expires_at".into(), json!(now() + exp)); }); Some(t) }
            _ => None,
        }
    }
    /// `check_connection`: true = still accepted, false = refused, None = could not tell.
    fn check(&self) -> Option<bool> {
        if self.str_of("access_token").is_none() { return Some(self.nile_file("library.json").is_some()); }
        if now() <= self.section().get("expires_at").and_then(to_i64).unwrap_or(0) - 60 { return Some(true); }
        let Some(rt) = self.str_of("refresh_token") else { return Some(false) };
        match self.refresh(&rt) {
            Ok(Ok((t, exp))) => { self.edit(|m| { m.insert("access_token".into(), json!(t)); m.insert("expires_at".into(), json!(now() + exp)); }); Some(true) }
            Ok(Err(s)) => if matches!(s, 400 | 401 | 403) { Some(false) } else { None },
            Err(_) => None,
        }
    }
    fn sync_nile(&self, path: &PathBuf, sink: &mut dyn FnMut(Vec<GameRecord>)) -> Result<usize, String> {
        set_activity("Amazon Games: reading the Nile library");
        log("Amazon: syncing from the Nile library file");
        let raw: Value = std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).ok_or("Could not read Nile library file")?;
        let installed = self.nile_file("installed.json").and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str::<Value>(&t).ok()).map(|v| nile_installed_ids(&v)).unwrap_or_default();
        let records = records_from_nile(&raw, &installed, &mut HashSet::new(), now());
        let n = records.len();
        if n > 0 { sink(records); }
        Ok(n)
    }
    fn sync_api(&self, sink: &mut dyn FnMut(Vec<GameRecord>), cancel: &Cancel) -> Result<usize, String> {
        let token = self.token().ok_or(NOT_CONNECTED)?;
        let serial = self.str_of("device_serial").unwrap_or_default();
        let (mut seen, mut count, mut next, mut page) = (HashSet::new(), 0usize, None::<String>, 1u32);
        loop {
            if cancel.is_set() { break; }
            set_activity(&format!("Amazon Games: reading your library (page {page})"));
            let h = [("X-Amz-Target", "com.amazon.animusdistributionservice.entitlement.AnimusEntitlementsService.GetEntitlements"), ("x-amzn-token", token.as_str()), ("UserAgent", CLIENT_UA), ("Content-Type", "application/json"), ("Content-Encoding", "amz-1.0")];
            let r = self.post(ENTITLEMENTS, &h, &entitlements_body(next.as_deref(), &serial), 20).map_err(|e| format!("Network error contacting Amazon: {e}"))?;
            if r.status >= 400 { return Err(format!("GetEntitlements failed (HTTP {}) \u{2014} try reconnecting.", r.status)); }
            let data: Value = serde_json::from_str(&r.body).map_err(|_| "Unexpected answer from Amazon".to_string())?;
            let entries = data.get("entitlements").and_then(|e| e.as_array()).cloned().unwrap_or_default();
            log(&format!("Amazon GetEntitlements page {page}: {} items", entries.len()));
            let records = records_from_entitlements(&entries, &mut seen, now());
            count += records.len();
            if !records.is_empty() { sink(records); }
            next = data.get("nextToken").and_then(|t| t.as_str()).filter(|t| !t.is_empty()).map(String::from);
            if next.is_none() || entries.is_empty() { break; }
            page += 1; pause(500);
        }
        Ok(count)
    }
}

impl LibrarySource for AmazonSource {
    fn id(&self) -> &str { "amazon_games" }
    fn label(&self) -> &str { "Amazon Games" }
    fn caps(&self) -> Caps { Caps { auth: true, sync: true, rescrape: true, launch: true, install: true, uninstall: true, installed_scan: true, store_page: true, ..Default::default() } }

    fn games_dir(&self) -> Option<PathBuf> { Some(crate::installdir::games_dir(&self.config_dir, "amazon_games", crate::installdir::default_dir("Amazon Games", "Amazon Games"))) }
    fn install(&self, _cx: &SourceCtx, g: &GameRef, progress: &mut dyn FnMut(&str, u64, u64), cancel: &Cancel) -> Result<Installed, String> {
        let token = self.token().ok_or(NOT_CONNECTED)?;
        progress(&format!("{}: asking Amazon for the download", g.name), 0, 0);
        let base = self.game_download(&g.platform_id, &token).map_err(|e| format!("Could not get download info: {e}"))?;
        let manifest = self.manifest(&base).map_err(|e| if e.contains("signature") { format!("Manifest signature check failed: {e}") } else { format!("Could not fetch manifest: {e}") })?;
        let files = match manifest.packages.first() { Some(p) if !p.files.is_empty() => p.files.clone(), _ => return Err(format!("{:?} has no downloadable files.", g.name)) };
        let total: u64 = files.iter().map(|f| f.size).sum();
        let games = self.games_dir().ok_or("No games folder")?;
        let dir = games.join(folder_name(&g.name, &g.platform_id));
        std::fs::create_dir_all(&dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
        crate::installdir::check_writable(&dir)?;
        crate::installdir::check_disk_space(&dir, total)?;
        self.ensure_sdk(cancel);
        let name = g.name.clone();
        let mut done = 0u64;
        for f in &files {
            if cancel.is_set() { return Err("Cancelled".into()); }
            let dest = safe_join(&dir, &f.path).ok_or_else(|| format!("Refusing a file outside the game folder: {}", f.path))?;
            if let Some(d) = dest.parent() { std::fs::create_dir_all(d).map_err(|e| format!("Could not create {}: {e}", d.display()))?; }
            let matches = |p: &std::path::Path| f.hash.algorithm == "sha256" && sha256_file(p).as_deref() == Some(f.hash.value.as_str());
            if dest.is_file() && matches(&dest) { done += f.size; progress(&format!("Downloading {name}: {} of {} MB", super::folder::mb(done), super::folder::mb(total)), done, total); continue; }
            let before = done;
            super::folder::download(self.http.as_ref(), &path_url(&base, &format!("/files/{}", f.hash.value)), &dest, cancel, &mut |d, _| progress(&format!("Downloading {name}: {} of {} MB", super::folder::mb(before + d), super::folder::mb(total)), before + d, total))
                .map_err(|e| if e == "Cancelled" { e } else { format!("Download failed for {}: {e}", f.path) })?;
            if !matches(&dest) { return Err(format!("Checksum mismatch for {} (got {:?}, want {} {})", f.path, sha256_file(&dest), f.hash.algorithm, f.hash.value)); }
            done += f.size;
        }
        let fuel = read_fuel(&dir).ok_or_else(|| format!("Downloaded {name:?} but could not find its fuel.json launch descriptor."))?;
        if !cfg!(windows) { let _ = std::fs::create_dir_all(dir.join("prefix")); }
        log(&format!("Amazon: installed {name:?} -> {}", dir.display()));
        Ok(Installed { install_path: dir.to_string_lossy().to_string(), executable: Some(fuel.command), note: None })
    }
    fn installed_scan(&self) -> Option<Vec<InstalledRow>> {
        let c = rusqlite::Connection::open_with_flags(&self.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
        let mut st = c.prepare("SELECT platform_id, COALESCE(install_path, '') FROM games WHERE platform = 'amazon_games' AND platform_id IS NOT NULL").ok()?;
        let rows: Vec<(String, String)> = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).ok()?.flatten().collect();
        // games installed through Heroic's Nile client are installed too
        let nile: HashSet<String> = self.nile_file("installed.json").and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str::<Value>(&t).ok()).map(|v| nile_installed_ids(&v)).unwrap_or_default();
        Some(rows.into_iter().map(|(id, path)| {
            let here = !path.is_empty() && std::path::Path::new(&path).join("fuel.json").is_file();
            match (here, !path.is_empty(), nile.contains(&id)) {
                (true, _, _) => InstalledRow { platform_id: id, installed: true, install_path: None },
                // a folder Zest installed into that is gone now: clear the path too
                (false, true, _) => InstalledRow { platform_id: id, installed: false, install_path: Some(String::new()) },
                (false, false, true) => InstalledRow { platform_id: id, installed: true, install_path: None },
                _ => InstalledRow { platform_id: id, installed: false, install_path: None },
            }
        }).collect())
    }
    fn launch(&self, g: &GameRef) -> Result<Launch, String> {
        let dir = g.install_path.as_deref().filter(|p| g.installed && !p.is_empty()).map(std::path::Path::new)
            .ok_or_else(|| format!("{} is not installed. Install it from Zest (or with Heroic's Nile and sync again).", g.name))?;
        let fuel = read_fuel(dir).ok_or_else(|| format!("Could not find {}'s launch descriptor (fuel.json).", g.name))?;
        let exe = dir.join(&fuel.command);
        if !exe.is_file() { return Err(format!("Executable not found: {}", exe.display())); }
        let cwd = fuel.cwd_override.as_ref().map(|c| dir.join(c)).or_else(|| exe.parent().map(|p| p.to_path_buf())).unwrap_or_else(|| dir.to_path_buf());
        // what the launcher passes a game so it can ask Amazon who owns it; the Fuel SDK folder is added when one was downloaded
        let mut env: Vec<(String, String)> = vec![("AMAZON_GAMES_FUEL_ENTITLEMENT_ID".into(), g.platform_id.clone()), ("AMAZON_GAMES_FUEL_DISPLAY_NAME".into(), self.str_of("username").unwrap_or_default())];
        if !g.appname.is_empty() { env.push(("AMAZON_GAMES_FUEL_PRODUCT_SKU".into(), g.appname.clone())); }
        let sdk = self.config_dir.join("amazon_games_sdk");
        if sdk.join(".done").is_file() { env.push(("FUEL_DIR".into(), sdk.join("Legacy").to_string_lossy().to_string())); env.push(("AMAZON_GAMES_SDK_PATH".into(), sdk.join("AmazonGamesSDK").to_string_lossy().to_string())); }
        if cfg!(windows) {
            return Ok(Launch { action: Action::Spawn { program: exe.to_string_lossy().to_string(), args: fuel.args, dir: Some(cwd.to_string_lossy().to_string()), env: env.into_iter().map(|(k, v)| (k, Some(v))).collect() }, record: true });
        }
        // a Windows game on Linux runs in a prefix inside its install folder
        let prefix = dir.join("prefix");
        Ok(Launch { action: crate::wine::launch_action_full(&exe, &prefix, &fuel.args, Some(&cwd), &env)?, record: true })
    }
    fn uninstall(&self, g: &GameRef) -> Result<Action, String> {
        let dir = g.install_path.as_deref().filter(|p| !p.is_empty()).ok_or_else(|| format!("{} is not installed.", g.name))?;
        // files are deleted only inside the games folder; a game installed elsewhere (Heroic) is only forgotten
        Ok(Action::RemoveInstall { delete: self.games_dir().map_or(false, |d| crate::installdir::is_inside(std::path::Path::new(dir), &d)), dir: dir.to_string() })
    }

    fn auth_status(&self) -> Result<AuthStatus, String> {
        let connected = self.str_of("access_token").is_some() || self.nile_file("library.json").is_some();
        // asked each time the dialog opens, as PlayDate's status route did: a refused login shows as signed out
        if !connected || self.check() == Some(false) { return Ok(AuthStatus { connected: false, username: None }); }
        Ok(AuthStatus { connected: true, username: Some(self.str_of("username").unwrap_or_else(|| "Connected".into())) })
    }
    fn web_login_done(&self) -> Option<serde_json::Value> { Some(serde_json::json!({"kind": "url", "contains": "openid.oa2.authorization_code="})) }
    fn paste_login_kept(&self) -> bool { false }   // the sign-in window works (confirmed live 2026-11-22)
    fn auth_begin(&self) -> Result<AuthBegin, String> {
        let verifier_bytes = random(32)?;
        let serial = hex(&random(16)?, true);
        let (verifier, challenge) = pkce(&verifier_bytes);
        let cid = client_id(&serial);
        let url = auth_url(&cid, &challenge);
        *self.pending.lock().map_err(|_| "busy".to_string())? = Some((verifier, serial));
        Ok(AuthBegin { url, placeholder: "Paste the address you land on after signing in".into(), instructions: vec![
            "Sign in to your Amazon account in your browser.".into(),
            "You land on the Amazon home page. Copy the full address from the address bar; it contains openid.oa2.authorization_code=.".into(),
            "Paste that address below.".into()] })
    }
    fn auth_complete(&self, value: &str) -> Result<AuthStatus, String> {
        let Some((verifier, serial)) = self.pending.lock().map_err(|_| "busy".to_string())?.take() else { return Err("Login session expired \u{2014} try connecting again.".into()) };
        let code = code_from(value).ok_or("Could not find an authorization code in the login result \u{2014} try again.")?;
        let body = register_body(&code, &client_id(&serial), &verifier, &serial);
        let r = self.post(&format!("{API}/auth/register"), &[("Content-Type", "application/json")], &body, 20).map_err(|e| { log(&format!("Amazon: {e}")); format!("Could not reach Amazon: {e}") })?;
        if r.status >= 400 { log(&format!("Amazon device registration failed: HTTP {}", r.status)); return Err(format!("Amazon rejected the login (HTTP {}) \u{2014} try again.", r.status)); }
        let reg = serde_json::from_str::<Value>(&r.body).ok().and_then(|v| parse_registration(&v)).ok_or_else(|| { log("Amazon device registration: unexpected response shape"); "Amazon returned an unexpected response \u{2014} see zest.log.".to_string() })?;
        // a new login replaces the whole section, as PlayDate does
        self.edit(|m| { m.clear(); m.insert("access_token".into(), json!(reg.access_token)); m.insert("refresh_token".into(), json!(reg.refresh_token)); m.insert("expires_at".into(), json!(now() + reg.expires_in));
            m.insert("device_serial".into(), json!(reg.device_serial)); m.insert("username".into(), json!(reg.username)); });
        log(&format!("Amazon Games: connected as {:?}", reg.username));
        Ok(AuthStatus { connected: true, username: Some(reg.username) })
    }
    fn auth_disconnect(&self) -> Result<(), String> {
        crate::account::plugin_remove(&self.config_dir, "amazon_games").map_err(|_| "Could not save".to_string())
    }

    fn store_url(&self, _g: &GameRef) -> Option<String> { Some(STORE.into()) }

    fn sync(&self, _cx: &SourceCtx, sink: &mut dyn FnMut(Vec<GameRecord>), cancel: &Cancel) -> Result<usize, String> {
        match self.nile_file("library.json") { Some(p) => self.sync_nile(&p, sink), None => self.sync_api(sink, cancel) }
    }
    /// PlayDate's re-scrape only fetched art and stamped the game; art is Zest's shared step, so this stamps.
    fn rescrape(&self, cx: &SourceCtx, _g: &GameRef) -> Fetch<Option<Update>> { Ok(Some(vec![("meta_fetched", Sql::Text(cx.today.clone()))])) }
}

#[cfg(test)]
mod behaviour {
    use super::*;

    /// A fake Amazon: device registration, token refresh and two pages of entitlements. Records the requests.
    struct Site { register_status: u16, ent_status: u16, refresh_status: u16, seen: Mutex<Vec<(String, String)>> }
    fn fake() -> Site { Site { register_status: 200, ent_status: 200, refresh_status: 200, seen: Mutex::new(vec![]) } }
    fn ent(id: &str, title: &str, sku: &str) -> Value { json!({"product": {"id": id, "title": title, "sku": sku, "productDetail": {"details": {"genres": ["Action", "RPG"]}}}}) }
    impl Http for Site {
        fn get(&self, _u: &str, _h: &[(&str, &str)], _t: Duration) -> Result<crate::net::Response, crate::net::NetError> { unreachable!() }
        fn post(&self, u: &str, h: &[(&str, &str)], body: &str, _t: Duration) -> Result<crate::net::Response, crate::net::NetError> {
            self.seen.lock().unwrap().push((u.to_string(), body.to_string()));
            let r = |s: u16, b: Value| Ok(crate::net::Response { status: s, retry_after: None, body: b.to_string() });
            let b: Value = serde_json::from_str(body).unwrap();
            if u == format!("{API}/auth/register") {
                assert_eq!(b["auth_data"]["authorization_code"], "THECODE");
                return if self.register_status == 200 { r(200, json!({"response": {"success": {"tokens": {"bearer": {"access_token": "AT1", "refresh_token": "RT1", "expires_in": "3600"}},
                    "extensions": {"device_info": {"device_serial_number": "SER1"}, "customer_info": {"given_name": "Pat"}}}}})) } else { r(self.register_status, json!({})) };
            }
            if u == format!("{API}/auth/token") { assert_eq!(b["source_token"], "RT0"); return if self.refresh_status == 200 { r(200, json!({"access_token": "AT2", "expires_in": 3600})) } else { r(self.refresh_status, json!({})) }; }
            if u == ENTITLEMENTS {
                assert!(h.iter().any(|(k, v)| *k == "x-amzn-token" && (*v == "AT0" || *v == "AT2")) && h.iter().any(|(k, _)| *k == "X-Amz-Target"));
                if self.ent_status != 200 { return r(self.ent_status, json!({})); }
                return if b["nextToken"].is_null() { r(200, json!({"entitlements": [ent("p1", "Game One", "SKU1"), json!({"product": {}}), ent("p2", " Game Two ", "")], "nextToken": "T2"})) }
                       else { r(200, json!({"entitlements": [ent("p1", "Game One again", "SKU1"), ent("p3", "", "SKU3")]})) };
            }
            panic!("unexpected {u}");
        }
    }
    fn setup(tag: &str, site: Site, section: Option<Value>) -> (AmazonSource, Arc<Site>, PathBuf) {
        let d = std::env::temp_dir().join(format!("zest_amz_{tag}_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        if let Some(s) = section { std::fs::write(d.join("config.json"), json!({"amazon_games": s, "other": 1}).to_string()).unwrap(); }
        let site = Arc::new(site);
        let mut s = AmazonSource::new(site.clone(), d.clone(), d.join("games.db")); s.home = d.join("home");
        (s, site, d)
    }
    const FAR: i64 = 4_000_000_000;
    fn tokens(expires: i64) -> Value { json!({"access_token": "AT0", "refresh_token": "RT0", "expires_at": expires, "device_serial": "SER0", "username": "Pat"}) }
    fn cx(d: &PathBuf, site: Arc<Site>) -> SourceCtx {
        SourceCtx { http: site, steam: Arc::new(crate::steamclient::Steam::new(Arc::new(crate::net::UreqHttp::new()), None, 10.0)), root: d.clone(), db: d.join("games.db"), today: "2026-11-01".into(), appinfo: Arc::new(|_| Default::default()), account_id: None }
    }

    #[test] fn sign_in_pastes_the_landing_address_registers_a_device_and_keeps_numbers_as_numbers() {
        let (s, site, d) = setup("auth", fake(), None);
        assert!(s.auth_complete("x").unwrap_err().contains("expired"));   // nothing was begun
        let b = s.auth_begin().unwrap();
        assert!(b.url.starts_with("https://amazon.com/ap/signin?") && b.url.contains("openid.oa2.code_challenge="));
        assert!(s.auth_complete("https://www.amazon.com/?foo=1").unwrap_err().contains("authorization code"));
        s.auth_begin().unwrap();
        let st = s.auth_complete("https://www.amazon.com/?openid.assoc_handle=x&openid.oa2.authorization_code=THECODE&openid.mode=y").unwrap();
        assert_eq!(st, AuthStatus { connected: true, username: Some("Pat".into()) });
        let cfg: Value = serde_json::from_str(&std::fs::read_to_string(d.join("config.json")).unwrap()).unwrap();
        assert_eq!(cfg["amazon_games"]["access_token"], "AT1"); assert!(cfg["amazon_games"]["expires_at"].is_i64() || cfg["amazon_games"]["expires_at"].is_u64());
        assert_eq!(cfg["amazon_games"]["device_serial"], "SER1");
        let sent: Value = serde_json::from_str(&site.seen.lock().unwrap()[0].1).unwrap();
        assert_eq!(sent["registration_data"]["device_type"], DEVICE_TYPE); assert_eq!(sent["auth_data"]["code_algorithm"], "SHA-256");
        s.auth_disconnect().unwrap();
        assert!(serde_json::from_str::<Value>(&std::fs::read_to_string(d.join("config.json")).unwrap()).unwrap().get("amazon_games").is_none());
    }
    #[test] fn a_rejected_registration_or_an_odd_answer_is_an_error_and_the_attempt_is_used_up() {
        let (s, _, _) = setup("rej", Site { register_status: 400, ..fake() }, None);
        s.auth_begin().unwrap();
        assert!(s.auth_complete("THECODE").unwrap_err().contains("HTTP 400"));
        assert!(s.auth_complete("THECODE").unwrap_err().contains("expired"));
    }
    #[test] fn sync_reads_every_entitlement_page_once_per_game_with_sku_and_genres() {
        let (s, site, d) = setup("sync", fake(), Some(tokens(FAR)));
        let mut got = vec![];
        assert_eq!(s.sync(&cx(&d, site.clone()), &mut |b| got.extend(b), &Cancel::new()).unwrap(), 3);
        assert_eq!(got.iter().map(|r| (r.platform_id.as_str(), r.name.as_str(), r.appname.as_deref(), r.genres.as_deref())).collect::<Vec<_>>(),
            [("p1", "Game One", Some("SKU1"), Some("Action,RPG")), ("p2", "Game Two", None, Some("Action,RPG")), ("p3", "p3", Some("SKU3"), Some("Action,RPG"))]);
        let reqs = site.seen.lock().unwrap();
        assert_eq!(reqs.len(), 2);
        let second: Value = serde_json::from_str(&reqs[1].1).unwrap(); assert_eq!(second["nextToken"], "T2"); assert_eq!(second["hardwareHash"].as_str().unwrap().len(), 64);
        assert_eq!(s.store_url(&GameRef::default()).as_deref(), Some(STORE));
    }
    #[test] fn an_expired_token_is_refreshed_and_a_dead_login_or_a_server_error_stops_the_sync() {
        let (s, site, d) = setup("refresh", fake(), Some(tokens(1)));
        assert_eq!(s.sync(&cx(&d, site.clone()), &mut |_| {}, &Cancel::new()).unwrap(), 3);
        assert_eq!(site.seen.lock().unwrap().iter().filter(|(u, _)| u.ends_with("/auth/token")).count(), 1);
        let cfg: Value = serde_json::from_str(&std::fs::read_to_string(d.join("config.json")).unwrap()).unwrap();
        assert_eq!((cfg["amazon_games"]["access_token"].as_str(), cfg["amazon_games"]["refresh_token"].as_str()), (Some("AT2"), Some("RT0")));
        let (s, site, d) = setup("dead", Site { refresh_status: 400, ..fake() }, Some(tokens(1)));
        assert_eq!(s.sync(&cx(&d, site), &mut |_| {}, &Cancel::new()).unwrap_err(), NOT_CONNECTED);
        let (s, site, d) = setup("500", Site { ent_status: 500, ..fake() }, Some(tokens(FAR)));
        assert!(s.sync(&cx(&d, site), &mut |_| {}, &Cancel::new()).unwrap_err().contains("HTTP 500"));
        let (s, site, d) = setup("nologin", fake(), None);
        assert_eq!(s.sync(&cx(&d, site), &mut |_| {}, &Cancel::new()).unwrap_err(), NOT_CONNECTED);
    }
    #[test] fn the_dialog_shows_signed_out_when_the_refresh_token_is_refused_and_keeps_a_login_it_cannot_check() {
        let (s, _, _) = setup("st_ok", fake(), Some(tokens(FAR)));
        assert_eq!(s.auth_status().unwrap(), AuthStatus { connected: true, username: Some("Pat".into()) });
        let (s, _, _) = setup("st_dead", Site { refresh_status: 401, ..fake() }, Some(tokens(1)));
        assert!(!s.auth_status().unwrap().connected);
        let (s, _, _) = setup("st_5xx", Site { refresh_status: 503, ..fake() }, Some(tokens(1)));
        assert!(s.auth_status().unwrap().connected);
        let (s, _, _) = setup("st_none", fake(), None);
        assert!(!s.auth_status().unwrap().connected);
    }
    #[test] fn a_nile_library_file_is_used_without_a_login_and_marks_installed_games() {
        let (s, site, d) = setup("nile", fake(), None);
        let dir = d.join("home/.config/nile"); std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("library.json"), json!([{"id": "e1", "product": {"id": "n1", "title": "Nile One", "sku": "S1"}}, {"id": "n2", "title": "Flat Two"}]).to_string()).unwrap();
        std::fs::write(dir.join("installed.json"), json!([{"id": "n1"}]).to_string()).unwrap();
        assert_eq!(s.auth_status().unwrap(), AuthStatus { connected: true, username: Some("Connected".into()) });
        let mut got = vec![];
        assert_eq!(s.sync(&cx(&d, site.clone()), &mut |b| got.extend(b), &Cancel::new()).unwrap(), 2);
        assert_eq!(got.iter().map(|r| (r.platform_id.as_str(), r.name.as_str(), r.installed)).collect::<Vec<_>>(), [("n1", "Nile One", Some(true)), ("n2", "Flat Two", Some(false))]);
        assert!(site.seen.lock().unwrap().is_empty());
        assert!(s.rescrape(&cx(&d, site), &GameRef::default()).unwrap().is_some());
    }
}

#[cfg(test)]
mod launch_tests {
    use super::*;
    fn tmpdir(tag: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("zest_amz_{tag}_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); std::fs::write(d.join("config.json"), "{\"amazon_games\": {\"username\": \"Rob\"}}").unwrap(); d }
    fn src(d: &PathBuf) -> AmazonSource { let h: Arc<dyn Http> = Arc::new(crate::net::UreqHttp::new()); AmazonSource::new(h, d.clone(), d.join("games_1.db")) }

    #[test] fn the_launch_descriptor_is_read_with_its_paths_made_portable() {
        let d = tmpdir("fuel");
        assert_eq!(read_fuel(&d), None);
        std::fs::write(d.join("fuel.json"), r#"{"Main": {"Command": "bin\\Game.exe", "Args": ["-a", "b"], "WorkingSubdirOverride": "bin\\data"}}"#).unwrap();
        assert_eq!(read_fuel(&d), Some(Fuel { command: "bin/Game.exe".into(), args: vec!["-a".into(), "b".into()], cwd_override: Some("bin/data".into()) }));
        std::fs::write(d.join("fuel.json"), r#"{"Main": {"Command": ""}}"#).unwrap(); assert_eq!(read_fuel(&d), None);
        std::fs::write(d.join("fuel.json"), "nope").unwrap(); assert_eq!(read_fuel(&d), None);
    }
    // ---- install: a fake Amazon distribution service serving signed manifests (made by tests/gen_sds_golden.py with a throwaway key) and files by hash
    fn unhex(s: &str) -> Vec<u8> { (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect() }
    struct Shop { g: Value, corrupt: Option<String>, no_url: bool, asked: Mutex<Vec<String>> }
    impl Http for Shop {
        fn get(&self, u: &str, _h: &[(&str, &str)], _t: Duration) -> Result<crate::net::Response, crate::net::NetError> {
            self.asked.lock().unwrap().push(u.to_string());
            assert_eq!(u, SDK_CHANNEL);
            Ok(crate::net::Response { status: 200, retry_after: None, body: json!({"downloadUrl": "https://cdn.test/sdk?sig=abc"}).to_string() })
        }
        fn get_bytes(&self, u: &str, _h: &[(&str, &str)], _t: Duration) -> Result<(u16, Vec<u8>), crate::net::NetError> {
            self.asked.lock().unwrap().push(u.to_string());
            let hexm = if u == "https://cdn.test/sdk/manifest.proto?sig=abc" { &self.g["install"]["sdk_manifest_hex"] } else { assert_eq!(u, "https://cdn.test/game/manifest.proto?token=t1"); &self.g["install"]["game_manifest_hex"] };
            Ok((200, unhex(hexm.as_str().unwrap())))
        }
        fn get_stream(&self, u: &str, _h: &[(&str, &str)], on_head: &mut dyn FnMut(u16, Option<u64>) -> bool, on_chunk: &mut dyn FnMut(&[u8]) -> bool) -> Result<(), crate::net::NetError> {
            self.asked.lock().unwrap().push(u.to_string());
            let hash = u.rsplit('/').next().unwrap().split('?').next().unwrap().to_string();
            let mut body = unhex(self.g["install"]["files"][&hash].as_str().unwrap_or_else(|| panic!("unknown file {u}")));
            if self.corrupt.as_deref() == Some(hash.as_str()) { body[0] ^= 0xff; }
            if !on_head(200, Some(body.len() as u64)) { return Ok(()); }
            for c in body.chunks(100) { if !on_chunk(c) { break; } }
            Ok(())
        }
        fn post(&self, u: &str, h: &[(&str, &str)], body: &str, _t: Duration) -> Result<crate::net::Response, crate::net::NetError> {
            assert_eq!(u, DISTRIBUTION);
            assert!(h.iter().any(|(k, v)| *k == "x-amzn-token" && *v == "AT0") && h.iter().any(|(k, v)| *k == "Content-Encoding" && *v == "amz-1.0"));
            let b: Value = serde_json::from_str(body).unwrap();
            assert_eq!((b["entitlementId"].as_str(), b["Operation"].as_str()), (Some("p1"), Some("GetGameDownload")));
            self.asked.lock().unwrap().push("GetGameDownload".into());
            Ok(crate::net::Response { status: 200, retry_after: None, body: if self.no_url { json!({}) } else { json!({"downloadUrl": "https://cdn.test/game?token=t1", "versionId": "v1"}) }.to_string() })
        }
    }
    fn shop_source(tag: &str, corrupt: Option<String>, no_url: bool) -> (AmazonSource, Arc<Shop>, PathBuf) {
        let g: Value = serde_json::from_str(include_str!("../../tests/golden/sds.json")).unwrap();
        let d = std::env::temp_dir().join(format!("zest_amz_inst_{tag}_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("config.json"), json!({"amazon_games": {"access_token": "AT0", "refresh_token": "RT0", "expires_at": 4_000_000_000i64, "device_serial": "SER0", "username": "Pat", "games_dir": d.join("games").to_string_lossy()}}).to_string()).unwrap();
        let shop = Arc::new(Shop { g: g.clone(), corrupt, no_url, asked: Mutex::new(vec![]) });
        let mut s = AmazonSource::new(shop.clone(), d.clone(), d.join("games.db")); s.home = d.join("home");
        s.verify_key = Some(unhex(g["signed"]["pubkey_der_hex"].as_str().unwrap()));
        (s, shop, d)
    }
    fn game_ref() -> GameRef { GameRef { appid: -5, name: "Fun: Game!".into(), platform: "amazon_games".into(), platform_id: "p1".into(), appname: "SKU1".into(), ..Default::default() } }
    fn cx_for(d: &PathBuf) -> SourceCtx { SourceCtx { http: Arc::new(crate::net::UreqHttp::new()), steam: Arc::new(crate::steamclient::Steam::new(Arc::new(crate::net::UreqHttp::new()), None, 10.0)), root: d.clone(), db: d.join("games.db"), today: "2026-11-01".into(), appinfo: Arc::new(|_| Default::default()), account_id: None } }

    #[test] fn install_downloads_the_signed_manifests_files_checks_each_hash_and_fetches_the_sdk_once() {
        let (s, shop, d) = shop_source("ok", None, false);
        let mut says = vec![];
        let got = s.install(&cx_for(&d), &game_ref(), &mut |m, _, _| says.push(m.to_string()), &Cancel::new()).unwrap();
        let dir = s.games_dir().unwrap().join("Fun Game");
        assert_eq!(got.install_path, dir.to_string_lossy());
        assert_eq!(got.executable.as_deref(), Some("bin/Game.exe"));
        assert_eq!(std::fs::read(dir.join("bin/Game.exe")).unwrap(), b"MZ-game-exe");
        assert_eq!(std::fs::read(dir.join("data/level1.pak")).unwrap().len(), 1400);
        assert!(dir.join("fuel.json").is_file() && (cfg!(windows) || dir.join("prefix").is_dir()));
        let sdk = d.join("amazon_games_sdk");
        assert!(sdk.join(".done").is_file() && sdk.join("Legacy/FuelSDK_x64.dll").is_file() && sdk.join("AmazonGamesSDK/AmazonGamesSDK_1.dll").is_file() && !sdk.join("Other/readme.txt").exists());
        assert!(says.iter().any(|m| m.contains("Downloading Fun: Game!")));
        // a second install finds every file already right and does not download them or the SDK again
        let before = shop.asked.lock().unwrap().len();
        s.install(&cx_for(&d), &game_ref(), &mut |_, _, _| {}, &Cancel::new()).unwrap();
        let again: Vec<String> = shop.asked.lock().unwrap()[before..].to_vec();
        assert!(again.iter().all(|u| !u.contains("/files/") && !u.contains("sdk")), "{again:?}");
    }
    #[test] fn a_corrupted_download_is_a_checksum_error_a_missing_url_or_login_is_refused_and_cancelling_stops() {
        let g: Value = serde_json::from_str(include_str!("../../tests/golden/sds.json")).unwrap();
        let exe_hash = g["install"]["files"].as_object().unwrap().iter().find(|(_, v)| v.as_str() == Some(&"MZ-game-exe".bytes().map(|b| format!("{b:02x}")).collect::<String>())).unwrap().0.clone();
        let (s, _, d) = shop_source("bad", Some(exe_hash), false);
        assert!(s.install(&cx_for(&d), &game_ref(), &mut |_, _, _| {}, &Cancel::new()).unwrap_err().contains("Checksum mismatch"));
        let (s, _, d) = shop_source("nourl", None, true);
        assert!(s.install(&cx_for(&d), &game_ref(), &mut |_, _, _| {}, &Cancel::new()).unwrap_err().contains("download URL"));
        let (s, _, d) = shop_source("cancel", None, false);
        let c = Cancel::new(); c.cancel();
        assert_eq!(s.install(&cx_for(&d), &game_ref(), &mut |_, _, _| {}, &c).unwrap_err(), "Cancelled");
        let (s, _, d) = shop_source("nologin", None, false);
        std::fs::write(d.join("config.json"), "{}").unwrap();
        assert_eq!(s.install(&cx_for(&d), &game_ref(), &mut |_, _, _| {}, &Cancel::new()).unwrap_err(), NOT_CONNECTED);
    }
    #[test] fn manifest_paths_cannot_leave_the_game_folder_and_urls_keep_their_query() {
        let b = std::path::Path::new("/g");
        assert_eq!(safe_join(b, "bin\\x/y.dll"), Some(PathBuf::from("/g/bin/x/y.dll")));
        assert!(safe_join(b, "../evil").is_none() && safe_join(b, "a/../../b").is_none() && safe_join(b, "C:\\w").is_none() && safe_join(b, "").is_none());
        assert_eq!(path_url("https://h/p?x=1#f", "/files/ab"), "https://h/p/files/ab?x=1#f");
        assert_eq!(folder_name("Fun: Game!", "id"), "Fun Game"); assert_eq!(folder_name("!!!", "id"), "id");
    }
    #[test] fn launch_runs_the_descriptor_in_a_prefix_and_the_scan_and_uninstall_follow_the_folder() {
        let d = tmpdir("flow"); let s = src(&d);
        let inst = d.join("Games/Amazon Games/Game"); std::fs::create_dir_all(inst.join("bin/data")).unwrap();
        std::fs::write(inst.join("bin/Game.exe"), "x").unwrap();
        std::fs::write(inst.join("fuel.json"), r#"{"Main": {"Command": "bin\\Game.exe", "Args": ["-x"], "WorkingSubdirOverride": "bin\\data"}}"#).unwrap();
        let g = GameRef { appid: -1, name: "Game".into(), platform: "amazon_games".into(), platform_id: "amzn1.adg.product.X".into(), appname: "SKU1".into(), installed: true, install_path: Some(inst.to_string_lossy().into()), ..Default::default() };
        crate::wine::TEST_RUNTIME.with(|t| *t.borrow_mut() = Some(Ok(crate::wine::Runtime::Wine { wine: "/usr/bin/wine".into() })));
        let l = s.launch(&g).unwrap(); assert!(l.record);
        match l.action { Action::Spawn { args, dir, env, .. } => {
            assert_eq!(args, vec![inst.join("bin/Game.exe").to_string_lossy().to_string(), "-x".to_string()]);
            assert_eq!(dir.as_deref(), Some(&*inst.join("bin/data").to_string_lossy()));
            for (k, v) in [("AMAZON_GAMES_FUEL_ENTITLEMENT_ID", "amzn1.adg.product.X"), ("AMAZON_GAMES_FUEL_DISPLAY_NAME", "Rob"), ("AMAZON_GAMES_FUEL_PRODUCT_SKU", "SKU1")] { assert!(env.contains(&(k.to_string(), Some(v.to_string()))), "{k}"); }
            assert!(!env.iter().any(|(k, _)| k == "FUEL_DIR"), "no SDK downloaded"); }
            a => panic!("{a:?}") }
        assert!(inst.join("prefix").is_dir());
        assert!(s.launch(&GameRef { installed: false, ..g.clone() }).unwrap_err().contains("Install it from Zest"));
        assert!(s.launch(&GameRef { install_path: Some(d.join("nowhere").to_string_lossy().into()), ..g.clone() }).unwrap_err().contains("fuel.json"));
        // the scan: a folder with fuel.json is installed; a recorded folder that is gone is cleared; a Nile install counts
        let nile = d.join(".config/nile"); std::fs::create_dir_all(&nile).ok();
        let db = d.join("games_1.db");
        rusqlite::Connection::open(&db).unwrap().execute_batch(&format!("CREATE TABLE games (platform TEXT, platform_id TEXT, install_path TEXT); INSERT INTO games VALUES ('amazon_games','a','{}'),('amazon_games','b','/gone'),('amazon_games','c',NULL);", inst.display())).unwrap();
        let rows = s.installed_scan().unwrap();
        assert_eq!(rows.iter().map(|r| (r.platform_id.as_str(), r.installed, r.install_path.as_deref())).collect::<Vec<_>>(), vec![("a", true, None), ("b", false, Some("")), ("c", false, None)]);
        // uninstall: outside the games folder it only forgets; inside it deletes
        match s.uninstall(&g).unwrap() { Action::RemoveInstall { delete, .. } => assert!(!delete), a => panic!("{a:?}") }
        assert!(s.uninstall(&GameRef { install_path: None, ..g.clone() }).is_err());
    }
}
