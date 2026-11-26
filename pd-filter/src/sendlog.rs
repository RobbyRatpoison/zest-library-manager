//! Send Log to Developer (`diagnostics.py`, `redact.py`): the log, a version/OS line and a snapshot of the settings go to the support relay, which turns them into a Discord
//! message. `config.json` is never sent (it holds the keys); the log is scrubbed of credentials first. The same relay and request shape as PlayDate, marked as Zest.
use crate::net::Http;
use regex::{Captures, Regex};
use serde_json::{json, Value};
use std::sync::OnceLock;
use std::time::Duration;

pub const RELAY_URL: &str = "https://playdate-log-relay.robbyratpoison.workers.dev";
pub const MAX_LOG_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_DIAG_BYTES: usize = 256 * 1024;
pub const MAX_MESSAGE_CHARS: usize = 1000;
pub const COOLDOWN_SECS: u64 = 5 * 60;

const PLACEHOLDER: &str = "[redacted]";

/// `redact.redact`: credential values in URL queries, JSON-looking text and Authorization / Cookie headers become `[redacted]`.
pub fn redact(text: &str) -> String {
    static Q: OnceLock<Regex> = OnceLock::new(); static J: OnceLock<Regex> = OnceLock::new(); static H: OnceLock<Regex> = OnceLock::new();
    let q = Q.get_or_init(|| Regex::new(r#"(?i)(?P<pre>[?&;\s,(]|^)(?P<name>key|api_key|apikey|access_token|refresh_token|id_token|auth_token|token|password|passwd|secret|client_secret|steamid|sessionid)=(?P<val>[^&\s'"<>)]+)"#).unwrap());
    let j = J.get_or_init(|| Regex::new(r#"(?i)(?P<pre>"(?:api_key|apikey|access_token|refresh_token|id_token|auth_token|password|secret|client_secret|sessionid)"\s*:\s*")(?P<val>[^"]+)"#).unwrap());
    let h = H.get_or_init(|| Regex::new(r#"(?i)(?P<pre>(?:authorization|cookie|x-api-key)\s*[:=]\s*(?:bearer\s+|basic\s+)?)(?P<val>[^\s'",;]+)"#).unwrap());
    let t = q.replace_all(text, |c: &Captures| format!("{}{}={PLACEHOLDER}", &c["pre"], &c["name"]));
    let t = j.replace_all(&t, |c: &Captures| format!("{}{PLACEHOLDER}", &c["pre"]));
    h.replace_all(&t, |c: &Captures| format!("{}{PLACEHOLDER}", &c["pre"])).into_owned()
}

fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719468; let era = if z >= 0 { z } else { z - 146096 } / 146097; let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; let y = yoe + era * 400; let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153; let d = doy - (153 * mp + 2) / 5 + 1; let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}
/// "2026-11-07 18:04:09" (UTC) for a Unix time.
pub fn stamp(secs: i64) -> String { let (y, m, d) = civil(secs.div_euclid(86400)); let t = secs.rem_euclid(86400); format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}", t / 3600, t % 3600 / 60, t % 60) }

/// Makes the app's log lines (`net::log`) go to `<data folder>/zest.log` as well as stderr: one timestamped, credential-scrubbed line each. The file is kept to about 4 MB:
/// past that, the older half is dropped.
pub fn install_file_logger(root: &std::path::Path) {
    let path = root.join("zest.log");
    let lock = std::sync::Mutex::new(());
    crate::net::set_logger(move |msg| {
        let line = redact(msg);
        eprintln!("{line}");
        let _g = lock.lock().unwrap_or_else(|e| e.into_inner());
        if std::fs::metadata(&path).map_or(false, |m| m.len() as usize > MAX_LOG_BYTES) {
            if let Ok(t) = std::fs::read(&path) { let keep = &t[t.len() / 2..]; let at = keep.iter().position(|b| *b == b'\n').map_or(0, |i| i + 1); let _ = std::fs::write(&path, &keep[at..]); }
        }
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
            let _ = writeln!(f, "{} {}", stamp(now), line);
        }
    });
}

/// What the support message names the install: `zest-flatpak`, `zest-windows`, ...
pub fn channel() -> &'static str { use crate::updater::Install::*; match crate::updater::Install::current() { Flatpak => "zest-flatpak", Windows => "zest-windows", Mac => "zest-mac", Linux => "zest-linux" } }

/// The settings and library counts that help reproduce a bug (no secrets live in state.json).
pub fn snapshot(state: &Value, conn: Option<&rusqlite::Connection>, plugins: &[(String, String)]) -> Value {
    let mut s = json!({ "state": state });
    s["library"] = match conn.map(|c| -> rusqlite::Result<Value> {
        let total: i64 = c.query_row("SELECT COUNT(*) FROM games", [], |r| r.get(0))?;
        let hidden: i64 = c.query_row("SELECT COUNT(*) FROM games WHERE duplicate_of IS NOT NULL AND duplicate_of != ''", [], |r| r.get(0))?;
        let mut counts = serde_json::Map::new();
        let mut st = c.prepare("SELECT platform, COUNT(*) FROM games GROUP BY platform")?;
        for r in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? { let (p, n) = r?; counts.insert(p, json!(n)); }
        Ok(json!({ "total_games": total, "hidden_dupes": hidden, "platform_counts": counts }))
    }) { Some(Ok(v)) => v, Some(Err(e)) => json!({ "error": e.to_string() }), None => json!({ "error": "no library" }) };
    s["plugins"] = Value::Object(plugins.iter().map(|(k, v)| (k.clone(), json!(v))).collect());
    s["renderer"] = json!({ "active": "zest native (egui, CPU)" });
    s
}

/// A multipart/form-data body with the three parts the relay reads (`meta`, `file`, `diagnostics`). Returns (content type, body).
pub fn multipart(meta: &Value, log: &str, diag: &str) -> (String, String) {
    let b = format!("----zestlog{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
    let mut body = String::new();
    body.push_str(&format!("--{b}\r\nContent-Disposition: form-data; name=\"meta\"\r\n\r\n{}\r\n", meta));
    body.push_str(&format!("--{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"zest.log\"\r\nContent-Type: text/plain\r\n\r\n{log}\r\n"));
    body.push_str(&format!("--{b}\r\nContent-Disposition: form-data; name=\"diagnostics\"; filename=\"diagnostics.json\"\r\nContent-Type: application/json\r\n\r\n{diag}\r\n"));
    body.push_str(&format!("--{b}--\r\n"));
    (format!("multipart/form-data; boundary={b}"), body)
}

/// Builds and posts a report to `relay`. `log` is the raw log text (scrubbed here, cut to its last 4 MB).
pub fn send(http: &dyn Http, relay: &str, message: &str, version: &str, os: &str, log: &str, snapshot: &Value) -> Result<(), String> {
    let mut log = redact(log);
    if log.len() > MAX_LOG_BYTES { let mut cut = log.len() - MAX_LOG_BYTES; while !log.is_char_boundary(cut) { cut += 1; } log = log[cut..].to_string(); }
    let mut diag = serde_json::to_string_pretty(snapshot).unwrap_or_default();
    if diag.len() > MAX_DIAG_BYTES { diag = json!({ "error": "diagnostics snapshot too large, omitted" }).to_string(); }
    let meta = json!({ "message": message.trim().chars().take(MAX_MESSAGE_CHARS).collect::<String>(), "version": format!("Zest {version}"), "install_channel": channel(), "os": os });
    let (ct, body) = multipart(&meta, &log, &diag);
    match http.post(relay, &[("Content-Type", ct.as_str())], &body, Duration::from_secs(30)) {
        Ok(r) if (200..300).contains(&r.status) => Ok(()),
        Ok(_) => Err("The report server refused it. Try again later.".into()),
        Err(_) => Err("Could not reach the report server. Try again later.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{NetError, Response};
    use std::sync::Mutex;
    const KEY: &str = "ABCDEF0123456789ABCDEF0123456789";
    #[test] fn credentials_are_scrubbed_like_playdates_redact_and_ordinary_text_is_not() {
        for t in [format!("fetch failed https://x/y?key={KEY}&steamid=76561198000000000&format=json"), format!("Authorization: Bearer {KEY}"), format!("{{\"api_key\": \"{KEY}\", \"x\": 1}}"),
                  format!("POST https://x/token?access_token={KEY}&scope=a"), format!("Cookie: sessionid={KEY}; other=1")] {
            let c = redact(&t); assert!(!c.contains(KEY) && !c.contains("76561198000000000"), "{c}");
        }
        assert!(redact("https://x/y?key=K&format=json").contains("format=json"));
        let plain = "ordinary line with no secrets, key figures and exit code=1"; assert_eq!(redact(plain), plain);
    }
    struct Fake(Mutex<Option<(String, String)>>, u16);
    impl Http for Fake {
        fn get(&self, _: &str, _: &[(&str, &str)], _: Duration) -> Result<Response, NetError> { Err(NetError("no".into())) }
        fn post(&self, url: &str, h: &[(&str, &str)], body: &str, _: Duration) -> Result<Response, NetError> {
            *self.0.lock().unwrap() = Some((format!("{url} {}", h[0].1), body.to_string()));
            Ok(Response { status: self.1, body: String::new(), retry_after: None })
        }
    }
    #[test] fn the_report_has_the_three_parts_the_relay_reads_with_the_log_scrubbed_and_marked_zest() {
        let f = Fake(Mutex::new(None), 200);
        send(&f, "http://relay.test", "it broke", "2026.11.0", "Linux 7", &format!("line key={KEY}\nnext"), &json!({"state": {"a": 1}})).unwrap();
        let (head, body) = f.0.lock().unwrap().clone().unwrap();
        assert!(head.starts_with("http://relay.test multipart/form-data; boundary="));
        assert!(body.contains("name=\"meta\"") && body.contains("name=\"file\"; filename=\"zest.log\"") && body.contains("name=\"diagnostics\""));
        assert!(body.contains("\"version\":\"Zest 2026.11.0\"") && body.contains("it broke") && !body.contains(KEY) && body.contains("key=[redacted]"));
        assert!(send(&Fake(Mutex::new(None), 502), "u", "", "v", "o", "", &json!({})).is_err());
    }
    #[test] fn stamps_read_as_dates() { assert_eq!(stamp(1579046400), "2020-01-15 00:00:00"); assert_eq!(stamp(0), "1970-01-01 00:00:00"); }
    #[test] fn a_snapshot_counts_games_per_platform() {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE games (appid INTEGER, platform TEXT, duplicate_of TEXT); INSERT INTO games VALUES (1,'steam',NULL),(2,'gog',NULL),(3,'gog','1');").unwrap();
        let s = snapshot(&json!({}), Some(&c), &[("x".into(), "1".into())]);
        assert_eq!(s["library"]["total_games"], 3); assert_eq!(s["library"]["platform_counts"]["gog"], 2); assert_eq!(s["library"]["hidden_dupes"], 1); assert_eq!(s["plugins"]["x"], "1");
    }
}
