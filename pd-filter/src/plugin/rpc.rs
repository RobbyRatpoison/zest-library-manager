//! One running plugin process and its JSON lines. Requests go out with a timeout (and can be cancelled), the plugin's own requests and notifications are
//! answered by a `Host`, and a plugin that exits or hangs only fails the request that was waiting for it: the caller starts it again next time.
use super::manifest::Manifest;
use crate::net::{log, strip_urls, Cancel};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq)]
pub struct RpcError { pub code: i64, pub message: String }
impl RpcError { fn new(code: i64, m: impl Into<String>) -> Self { RpcError { code, message: m.into() } } }
impl std::fmt::Display for RpcError { fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.message) } }

/// What answers the plugin's own messages (secrets, known ids, progress, games).
pub trait Host: Send + Sync {
    fn request(&self, method: &str, params: &Value) -> Result<Value, String>;
    fn notify(&self, method: &str, params: &Value);
}

type Pending = Arc<Mutex<HashMap<u64, Sender<Result<Value, RpcError>>>>>;

pub struct PluginProc {
    pub plugin: String,
    child: Mutex<Child>,
    stdin: Arc<Mutex<ChildStdin>>,
    pending: Pending,
    next: AtomicU64,
    alive: Arc<AtomicBool>,
}

fn write_line(stdin: &Mutex<ChildStdin>, v: &Value) -> std::io::Result<()> {
    let mut line = v.to_string(); line.push('\n');
    let mut g = stdin.lock().unwrap();
    g.write_all(line.as_bytes())?; g.flush()
}

impl PluginProc {
    pub fn start(m: &Manifest, host: Arc<dyn Host>) -> Result<Arc<PluginProc>, String> {
        let mut cmd = Command::new(&m.run[0]);
        cmd.args(&m.run[1..]).current_dir(&m.dir).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
            .env("PYTHONUNBUFFERED", "1").env("PYTHONIOENCODING", "utf-8").env("ZEST_PLUGIN_API", super::manifest::CORE_API.to_string());
        { use crate::proc::Hide; cmd.hidden(); }
        let mut child = cmd.spawn().map_err(|e| format!("could not start the plugin ({:?})", e.kind()))?;
        let stdin = Arc::new(Mutex::new(child.stdin.take().ok_or("no stdin")?));
        let (stdout, stderr) = (child.stdout.take().ok_or("no stdout")?, child.stderr.take().ok_or("no stderr")?);
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let alive = Arc::new(AtomicBool::new(true));
        let name = m.id.clone();
        { // stderr: the plugin's log
            let name = name.clone();
            std::thread::spawn(move || for l in BufReader::new(stderr).lines().map_while(Result::ok) { log(&format!("[plugin {name}] {}", strip_urls(&l))); });
        }
        { // stdout: answers, and the plugin's own requests and notifications
            let (pending, alive, stdin, name) = (pending.clone(), alive.clone(), stdin.clone(), name.clone());
            std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    let Ok(v) = serde_json::from_str::<Value>(&line) else { log(&format!("[plugin {name}] ignored a line that is not JSON: {}", strip_urls(&line.chars().take(200).collect::<String>()))); continue };
                    if let Some(method) = v.get("method").and_then(|m| m.as_str()) {
                        let params = v.get("params").cloned().unwrap_or(Value::Null);
                        match v.get("id").cloned() {
                            Some(id) => {
                                let reply = match host.request(method, &params) { Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}), Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32000, "message": e}}) };
                                let _ = write_line(&stdin, &reply);
                            }
                            None => host.notify(method, &params),
                        }
                    } else if let Some(id) = v.get("id").and_then(|i| i.as_u64()) {
                        let res = match v.get("error") {
                            Some(e) => Err(RpcError::new(e.get("code").and_then(|c| c.as_i64()).unwrap_or(-32000), e.get("message").and_then(|m| m.as_str()).unwrap_or("the plugin reported an error"))),
                            None => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
                        };
                        if let Some(tx) = pending.lock().unwrap().remove(&id) { let _ = tx.send(res); }
                    }
                }
                alive.store(false, Ordering::SeqCst);
                for (_, tx) in pending.lock().unwrap().drain() { let _ = tx.send(Err(RpcError::new(-32001, "the plugin stopped"))); }
            });
        }
        Ok(Arc::new(PluginProc { plugin: name, child: Mutex::new(child), stdin, pending, next: AtomicU64::new(1), alive }))
    }

    pub fn is_alive(&self) -> bool { self.alive.load(Ordering::SeqCst) }

    pub fn notify(&self, method: &str, params: Value) { let _ = write_line(&self.stdin, &json!({"jsonrpc": "2.0", "method": method, "params": params})); }

    /// A request. `timeout` None = wait as long as the plugin runs (use `cancel` to stop it). A timeout kills the plugin: it is hung.
    pub fn call(&self, method: &str, params: Value, timeout: Option<Duration>, cancel: Option<&Cancel>) -> Result<Value, RpcError> {
        if !self.is_alive() { return Err(RpcError::new(-32001, "the plugin is not running")); }
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        self.pending.lock().unwrap().insert(id, tx);
        if write_line(&self.stdin, &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})).is_err() {
            self.pending.lock().unwrap().remove(&id);
            return Err(RpcError::new(-32001, "the plugin is not running"));
        }
        let start = Instant::now();
        let mut cancelled_at: Option<Instant> = None;
        loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(r) => return r,
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(RpcError::new(-32001, "the plugin stopped")),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if timeout.map_or(false, |t| start.elapsed() > t) {
                self.pending.lock().unwrap().remove(&id); self.kill();
                return Err(RpcError::new(-32002, "the plugin did not answer in time and was stopped"));
            }
            if cancel.map_or(false, |c| c.is_set()) {
                match cancelled_at {
                    None => { self.notify("cancel", json!({"id": id})); cancelled_at = Some(Instant::now()); }
                    // it had its chance to stop politely
                    Some(t) if t.elapsed() > Duration::from_secs(5) => { self.pending.lock().unwrap().remove(&id); self.kill(); return Err(RpcError::new(-32003, "cancelled")); }
                    _ => {}
                }
            }
        }
    }

    fn kill(&self) { self.alive.store(false, Ordering::SeqCst); let _ = self.child.lock().unwrap().kill(); let _ = self.child.lock().unwrap().wait(); }

    /// `shutdown`, a moment to exit by itself, then the end of the process.
    pub fn stop(&self) {
        if self.is_alive() { self.notify("shutdown", Value::Null); }
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(2) { if matches!(self.child.lock().unwrap().try_wait(), Ok(Some(_))) { self.alive.store(false, Ordering::SeqCst); return; } std::thread::sleep(Duration::from_millis(30)); }
        self.kill();
    }
}

impl Drop for PluginProc { fn drop(&mut self) { if self.is_alive() { let _ = self.child.lock().unwrap().kill(); } } }
