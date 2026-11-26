//! The SteamGifts import as the Companion listener sees it: four calls from the userscript (session, page, pass2-plan, finish), each a JSON in and a JSON out,
//! driving one [`Session`]. Owns the files (`steamgifts_wins_<steam id>.json`, `group_sources_<account>.json`) and the library connection; the Steam lookups for
//! wins that did not match go through the `Http` it was given.
use crate::blaeo::GroupSources;
use crate::companion::{Event, SteamGiftsHandler};
use crate::net::Http;
use crate::steamgifts::*;
use crate::steamgifts_session::{Mode, Session};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub struct Runner {
    pub root: PathBuf, pub db: PathBuf, pub steam_id: String,
    pub http: Arc<dyn Http>, /// The wait between Steam store lookups for unmatched wins (a second, like PlayDate).
    pub pause: Duration,
    pub on_event: Arc<dyn Fn(Event) + Send + Sync>,
    session: Mutex<Option<Session>>,
}

impl Runner {
    pub fn new(root: PathBuf, db: PathBuf, steam_id: String, http: Arc<dyn Http>, pause: Duration, on_event: Arc<dyn Fn(Event) + Send + Sync>) -> Runner {
        Runner { root, db, steam_id, http, pause, on_event, session: Mutex::new(None) }
    }
    fn store_path(&self) -> PathBuf { self.root.join(if self.steam_id.is_empty() { "steamgifts_wins.json".to_string() } else { format!("steamgifts_wins_{}.json", self.steam_id) }) }
    fn gs_path(&self) -> PathBuf {
        let id = self.db.file_stem().and_then(|n| n.to_str()).and_then(|n| n.strip_prefix("games_")).map(String::from);
        self.root.join(match id { Some(id) => format!("group_sources_{id}.json"), None => "group_sources.json".into() })
    }
    fn s(b: &Value, k: &str) -> String { b.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string() }
}

impl SteamGiftsHandler for Runner {
    fn call(&self, route: &str, body: &Value) -> Result<Value, String> {
        let mut guard = self.session.lock().map_err(|_| "the import is in an unusable state".to_string())?;
        match route {
            "session" => {
                let configured = crate::account::sg_username(&self.root);
                let (s, b) = Session::begin(load_store(&self.store_path()), &configured, &Self::s(body, "logged_in_as"), &Self::s(body, "viewing"),
                    body["full_refresh"].as_bool().unwrap_or(false), body["pass2"].as_bool().unwrap_or(true))?;
                if let Some(name) = &b.adopted_username { let _ = crate::account::set_sg_username(&self.root, name); }
                *guard = Some(s);
                Ok(json!({"username": b.username, "mode": if b.mode == Mode::Full { "full" } else { "degraded" }, "pass2": b.pass2, "full_refresh": b.full_refresh, "public_url": b.public_url, "public_page_size": b.public_page_size}))
            }
            "page" => {
                let s = guard.as_mut().ok_or("no sync is running: start it from Zest again")?;
                let (phase, page, html) = (Self::s(body, "phase"), body["page"].as_i64().unwrap_or(1), body["html"].as_str().unwrap_or(""));
                if html.is_empty() || (phase != "public" && phase != "private") { return Err("bad request".into()); }
                let reply = if phase == "public" {
                    let r = s.page_public(page, html);
                    json!({"parsed": r.parsed, "new": r.new, "changed": r.changed, "more": r.more, "pages": r.pages, "total": r.total})
                } else {
                    let (parsed, flipped) = s.page_private(page, html);
                    json!({"parsed": parsed, "flipped": flipped})
                };
                save_store(&self.store_path(), &s.store).map_err(|e| format!("could not save the wins file: {e}"))?;
                Ok(reply)
            }
            "pass2-plan" => {
                let s = guard.as_ref().ok_or("no sync is running: start it from Zest again")?;
                Ok(match s.pass2_plan() { Some(p) => json!({"enabled": true, "pages": p, "private_url": "/giveaways/won", "private_page_size": PRIVATE_PAGE_SIZE}), None => json!({"enabled": false, "pages": []}) })
            }
            "finish" => {
                let mut s = guard.take().ok_or("no sync is running: start it from Zest again")?;
                let result = (|| -> Result<Summary, String> {
                    let conn = rusqlite::Connection::open(&self.db).map_err(|e| e.to_string())?;
                    conn.busy_timeout(Duration::from_secs(10)).ok();
                    let mut gs = GroupSources::load(&self.gs_path());
                    let steam = WebSteam::new(&*self.http, self.pause);
                    let sum = s.finish(&conn, &mut gs, &steam).map_err(|e| e.to_string())?;
                    save_store(&self.store_path(), &s.store).map_err(|e| e.to_string())?;
                    gs.save(&self.gs_path()).map_err(|e| e.to_string())?;
                    Ok(sum)
                })();
                match result {
                    Ok(sum) => {
                        let p = &s.progress;
                        let line = format!("SteamGifts: {} game{} in \"{GROUP_NAME}\" ({} added, {} removed), {} new win{}, {} not in the library", sum.games_in_group, if sum.games_in_group == 1 { "" } else { "s" },
                            sum.groups_added, sum.groups_removed, p.new_wins, if p.new_wins == 1 { "" } else { "s" }, sum.unresolved_refs);
                        (self.on_event)(Event::SteamGifts { message: line, ok: true });
                        Ok(json!({"summary": {"games_in_group": sum.games_in_group, "groups_added": sum.groups_added, "groups_removed": sum.groups_removed, "unresolved_refs": sum.unresolved_refs,
                            "wins_total": sum.wins_total, "new_wins": p.new_wins, "changed_wins": p.changed_wins}}))
                    }
                    Err(e) => { (self.on_event)(Event::SteamGifts { message: format!("SteamGifts import failed: {e}"), ok: false }); Err("Apply failed. See the Zest log.".into()) }
                }
            }
            _ => Err("no such step".into()),
        }
    }
}
