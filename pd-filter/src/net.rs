//! The HTTP side of the scrapers: a tiny blocking client behind a trait (so tests fake the network), the pool backoff gate and the two-level work
//! queue, ported from `scrapers.py` (`_PoolBackoff`, `_next`, `_parse_retry_after`, `BACKOFF_DELAYS`). No async runtime: callers run these on threads.
//!
//! **Credentials never reach a message.** The Steam API key travels in the request URL, so every error is built from the transport error's kind and
//! message only, never from its Display (ureq's includes the URL). `Creds` has a redacting `Debug`.
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------- the client

#[derive(Debug, Clone)]
pub struct Response { pub status: u16, /// Seconds from a `Retry-After` header (number or HTTP date), when there was one.
    pub retry_after: Option<f64>, pub body: String }

/// A failure to get any answer (DNS, connect, TLS, timeout). The text is safe to log: it never holds a URL.
#[derive(Debug, Clone, PartialEq)]
pub struct NetError(pub String);
impl std::fmt::Display for NetError { fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.0) } }

/// An answer with its headers, for the few places that need them (a renewed session cookie, a redirect to a sign-in page).
#[derive(Debug, Clone)]
pub struct Raw { pub status: u16, pub headers: Vec<(String, String)>, pub body: String }
impl Raw { pub fn header_all(&self, name: &str) -> Vec<&str> { self.headers.iter().filter(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str()).collect() } }

pub trait Http: Send + Sync {
    /// GET without following redirects (a 3xx is the answer), with the response headers. Default: a plain `get` with no headers.
    fn get_raw(&self, url: &str, headers: &[(&str, &str)], timeout: Duration) -> Result<Raw, NetError> { self.get(url, headers, timeout).map(|r| Raw { status: r.status, headers: vec![], body: r.body }) }
    /// GET `url`. A 4xx/5xx is a normal `Ok(Response)`; only "no answer at all" is an `Err`.
    fn get(&self, url: &str, headers: &[(&str, &str)], timeout: Duration) -> Result<Response, NetError>;
    /// POST a form (`application/x-www-form-urlencoded`) without following redirects, with the answer's headers (a sign-in step that answers with Set-Cookie). Default: not supported.
    fn post_form_raw(&self, _url: &str, _headers: &[(&str, &str)], _form: &[(&str, &str)], _timeout: Duration) -> Result<Raw, NetError> { Err(NetError("form POST not supported".into())) }
    /// GET that also reports the address the answer came from after redirects (a sign-in check needs it). Default: the address asked for.
    fn get_final(&self, url: &str, headers: &[(&str, &str)], timeout: Duration) -> Result<(Response, String), NetError> { self.get(url, headers, timeout).map(|r| (r, url.to_string())) }
    /// GET a binary body (images): (status, bytes), at most 40 MB. Default: not supported.
    fn get_bytes(&self, _url: &str, _headers: &[(&str, &str)], _timeout: Duration) -> Result<(u16, Vec<u8>), NetError> { Err(NetError("binary GET not supported".into())) }
    /// GET a large body piece by piece (a game download): `on_head(status, length)` decides whether to go on (false stops cleanly), `on_chunk` gets each piece (false stops, e.g. cancelled).
    /// Redirects are followed; there is no overall time limit, only a connect and a read-stall limit. Default: not supported.
    fn get_stream(&self, _url: &str, _headers: &[(&str, &str)], _on_head: &mut dyn FnMut(u16, Option<u64>) -> bool, _on_chunk: &mut dyn FnMut(&[u8]) -> bool) -> Result<(), NetError> { Err(NetError("streaming GET not supported".into())) }
    /// POST a JSON body. Default: not supported (fakes that only GET need not implement it).
    fn post(&self, _url: &str, _headers: &[(&str, &str)], _body: &str, _timeout: Duration) -> Result<Response, NetError> { Err(NetError("POST not supported".into())) }
}

pub struct UreqHttp { agent: ureq::Agent, agent_nr: ureq::Agent, agent_dl: ureq::Agent }
impl UreqHttp {
    pub fn new() -> Self {
        let ua = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36";
        UreqHttp { agent: ureq::AgentBuilder::new().user_agent(ua).build(), agent_nr: ureq::AgentBuilder::new().user_agent(ua).redirects(0).build(),
            agent_dl: ureq::AgentBuilder::new().user_agent(ua).timeout_connect(Duration::from_secs(30)).timeout_read(Duration::from_secs(60)).build() }
    }
}
impl Default for UreqHttp { fn default() -> Self { Self::new() } }

impl Http for UreqHttp {
    fn get_raw(&self, url: &str, headers: &[(&str, &str)], timeout: Duration) -> Result<Raw, NetError> {
        let mut req = self.agent_nr.get(url).timeout(timeout);
        for (k, v) in headers { req = req.set(k, v); }
        let resp = match req.call() {
            Ok(r) => r, Err(ureq::Error::Status(_, r)) => r,
            Err(ureq::Error::Transport(t)) => return Err(NetError(format!("{:?}{}", t.kind(), t.message().map(|m| format!(": {}", strip_urls(m))).unwrap_or_default()))),
        };
        let status = resp.status();
        let mut hs = vec![];
        for n in resp.headers_names() { for v in resp.all(&n) { hs.push((n.clone(), v.to_string())); } }
        let body = resp.into_string().map_err(|e| NetError(format!("reading the answer: {}", strip_urls(&e.to_string()))))?;
        Ok(Raw { status, headers: hs, body })
    }
    fn post_form_raw(&self, url: &str, headers: &[(&str, &str)], form: &[(&str, &str)], timeout: Duration) -> Result<Raw, NetError> {
        let mut req = self.agent_nr.post(url).timeout(timeout);
        for (k, v) in headers { req = req.set(k, v); }
        let resp = match req.send_form(form) {
            Ok(r) => r, Err(ureq::Error::Status(_, r)) => r,
            Err(ureq::Error::Transport(t)) => return Err(NetError(format!("{:?}{}", t.kind(), t.message().map(|m| format!(": {}", strip_urls(m))).unwrap_or_default()))),
        };
        let status = resp.status();
        let mut hs = vec![];
        for n in resp.headers_names() { for v in resp.all(&n) { hs.push((n.clone(), v.to_string())); } }
        let body = resp.into_string().map_err(|e| NetError(format!("reading the answer: {}", strip_urls(&e.to_string()))))?;
        Ok(Raw { status, headers: hs, body })
    }
    fn get(&self, url: &str, headers: &[(&str, &str)], timeout: Duration) -> Result<Response, NetError> {
        let mut req = self.agent.get(url).timeout(timeout);
        for (k, v) in headers { req = req.set(k, v); }
        Self::finish(req.call())
    }
    fn get_final(&self, url: &str, headers: &[(&str, &str)], timeout: Duration) -> Result<(Response, String), NetError> {
        let mut req = self.agent.get(url).timeout(timeout);
        for (k, v) in headers { req = req.set(k, v); }
        let resp = match req.call() {
            Ok(r) => r, Err(ureq::Error::Status(_, r)) => r,
            Err(ureq::Error::Transport(t)) => return Err(NetError(format!("{:?}{}", t.kind(), t.message().map(|m| format!(": {}", strip_urls(m))).unwrap_or_default()))),
        };
        let last = resp.get_url().to_string();
        let status = resp.status();
        let retry_after = resp.header("Retry-After").and_then(parse_retry_after);
        let body = resp.into_string().map_err(|e| NetError(format!("reading the answer: {}", strip_urls(&e.to_string()))))?;
        Ok((Response { status, retry_after, body }, last))
    }
    fn get_bytes(&self, url: &str, headers: &[(&str, &str)], timeout: Duration) -> Result<(u16, Vec<u8>), NetError> {
        use std::io::Read;
        let mut req = self.agent.get(url).timeout(timeout);
        for (k, v) in headers { req = req.set(k, v); }
        let resp = match req.call() {
            Ok(r) => r, Err(ureq::Error::Status(_, r)) => r,
            Err(ureq::Error::Transport(t)) => return Err(NetError(format!("{:?}{}", t.kind(), t.message().map(|m| format!(": {}", strip_urls(m))).unwrap_or_default()))),
        };
        let status = resp.status();
        let mut buf = Vec::new();
        resp.into_reader().take(40 * 1024 * 1024).read_to_end(&mut buf).map_err(|e| NetError(format!("reading the image: {}", strip_urls(&e.to_string()))))?;
        Ok((status, buf))
    }
    fn get_stream(&self, url: &str, headers: &[(&str, &str)], on_head: &mut dyn FnMut(u16, Option<u64>) -> bool, on_chunk: &mut dyn FnMut(&[u8]) -> bool) -> Result<(), NetError> {
        let mut req = self.agent_dl.get(url);
        for (k, v) in headers { req = req.set(k, v); }
        let resp = match req.call() { Ok(r) => r, Err(ureq::Error::Status(_, r)) => r, Err(ureq::Error::Transport(t)) => return Err(NetError(format!("{:?}{}", t.kind(), t.message().map(|m| format!(": {}", strip_urls(m))).unwrap_or_default()))) };
        let len = resp.header("content-length").and_then(|l| l.parse::<u64>().ok());
        if !on_head(resp.status(), len) { return Ok(()); }
        let mut r = resp.into_reader();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = r.read(&mut buf).map_err(|e| NetError(format!("the download stopped: {}", strip_urls(&e.to_string()))))?;
            if n == 0 { return Ok(()); }
            if !on_chunk(&buf[..n]) { return Ok(()); }
        }
    }
    fn post(&self, url: &str, headers: &[(&str, &str)], body: &str, timeout: Duration) -> Result<Response, NetError> {
        let mut req = self.agent.post(url).timeout(timeout);
        for (k, v) in headers { req = req.set(k, v); }
        Self::finish(req.send_string(body))
    }
}
impl UreqHttp {
    fn finish(res: Result<ureq::Response, ureq::Error>) -> Result<Response, NetError> {
        let resp = match res {
            Ok(r) => r,
            Err(ureq::Error::Status(_, r)) => r,
            // `Transport`'s Display starts with the URL; use its parts instead.
            Err(ureq::Error::Transport(t)) => return Err(NetError(format!("{:?}{}", t.kind(), t.message().map(|m| format!(": {}", strip_urls(m))).unwrap_or_default()))),
        };
        let status = resp.status();
        let retry_after = resp.header("Retry-After").and_then(parse_retry_after);
        let body = resp.into_string().map_err(|e| NetError(format!("reading the answer: {}", strip_urls(&e.to_string()))))?;
        Ok(Response { status, retry_after, body })
    }
}

/// Removes anything URL-shaped from a message (a second belt for messages not built by us).
pub fn strip_urls(s: &str) -> String {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"[a-zA-Z][a-zA-Z0-9+.-]*://\S+").unwrap()).replace_all(s, "<url>").into_owned()
}

/// `_parse_retry_after`: a plain number of seconds, or an HTTP date (`Sun, 06 Nov 1994 08:49:37 GMT`); None when absent or unparseable.
pub fn parse_retry_after(val: &str) -> Option<f64> {
    let v = val.trim();
    if v.is_empty() { return None; }
    if let Ok(n) = v.parse::<f64>() { return Some(n); }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
    parse_http_date(v).map(|t| ((t - now) as f64).max(0.0))
}

fn parse_http_date(v: &str) -> Option<i64> {
    // "Sun, 06 Nov 1994 08:49:37 GMT"
    const M: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    let parts: Vec<&str> = v.split(|c: char| c == ' ' || c == ',').filter(|p| !p.is_empty()).collect();
    let (d, mon, y, hms) = match parts.as_slice() { [_, d, m, y, t, ..] => (*d, *m, *y, *t), _ => return None };
    let d: i64 = d.parse().ok()?; let y: i64 = y.parse().ok()?;
    let m = M.iter().position(|n| n.eq_ignore_ascii_case(mon))? as i64 + 1;
    let hms: Vec<i64> = hms.split(':').map(|x| x.parse().ok()).collect::<Option<_>>()?;
    if hms.len() != 3 { return None; }
    let (yy, mm) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = yy.div_euclid(400); let yoe = yy - era * 400; let doy = (153 * mm + 2) / 5 + d - 1; let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146097 + doe - 719468) * 86400 + hms[0] * 3600 + hms[1] * 60 + hms[2])
}

/// Percent-encodes a query value (everything but unreserved characters).
pub fn enc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for b in s.bytes() { if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { o.push(b as char) } else { o.push_str(&format!("%{b:02X}")) } }
    o
}

// ---------------------------------------------------------------- logging

static LOGGER: std::sync::OnceLock<Box<dyn Fn(&str) + Send + Sync>> = std::sync::OnceLock::new();
/// Where the scrapers' log lines go (the app installs its log writer once; default is stderr). Messages never hold a URL.
pub fn set_logger(f: impl Fn(&str) + Send + Sync + 'static) { let _ = LOGGER.set(Box::new(f)); }
pub fn log(msg: &str) { match LOGGER.get() { Some(f) => f(&strip_urls(msg)), None => eprintln!("{}", strip_urls(msg)) } }

// ---------------------------------------------------------------- what a long job is doing

static ACTIVITY: Mutex<String> = Mutex::new(String::new());
/// A short note on what the running job is doing right now ("Humble Bundle: order 12 of 40"), for the status line. Set by a source during a long sync, cleared by the job.
static NOTE: Mutex<String> = Mutex::new(String::new());
/// A one-line result a finished job leaves for the app to show (an install that failed, say); taken once.
pub fn set_note(s: &str) { if let Ok(mut n) = NOTE.lock() { *n = s.to_string(); } }
pub fn take_note() -> String { NOTE.lock().map(|mut n| std::mem::take(&mut *n)).unwrap_or_default() }
pub fn set_activity(s: &str) { if let Ok(mut a) = ACTIVITY.lock() { *a = s.to_string(); } }
pub fn activity() -> String { ACTIVITY.lock().map(|a| a.clone()).unwrap_or_default() }

// ---------------------------------------------------------------- cancel

/// `threading.Event` for cancelling a job.
#[derive(Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);
impl Cancel { pub fn new() -> Self { Self::default() } pub fn cancel(&self) { self.0.store(true, Ordering::SeqCst) } pub fn is_set(&self) -> bool { self.0.load(Ordering::SeqCst) } }

// ---------------------------------------------------------------- the backoff gate

/// `BACKOFF_DELAYS`: 15 s, 1 min, 5 min, 1 h 15 s. A server-sent `Retry-After` wins, capped at the last.
pub const BACKOFF_DELAYS: [u64; 4] = [15, 60, 300, 3615];

/// Per-pool rate-limit gate (`_PoolBackoff`). The worker that gets a 429 closes the gate and sleeps; the others block in `wait_ready` until it reopens.
/// After the delays are used up the pool is aborted. `scale` shrinks every wait (tests only).
pub struct PoolBackoff {
    pub name: String,
    delays: Vec<Duration>,
    st: Mutex<GateState>,
    cv: Condvar,
}
struct GateState { open: bool, attempt: usize, aborted: bool }

/// What the UI shows while the gate is closed.
#[derive(Debug, Clone, PartialEq)]
pub struct Wait { pub attempt: usize, pub of: usize, pub delay_secs: f64, pub server_requested: bool }

impl PoolBackoff {
    pub fn new(name: &str) -> Self { Self::with_delays(name, BACKOFF_DELAYS.iter().map(|s| Duration::from_secs(*s)).collect()) }
    pub fn with_delays(name: &str, delays: Vec<Duration>) -> Self { PoolBackoff { name: name.into(), delays, st: Mutex::new(GateState { open: true, attempt: 0, aborted: false }), cv: Condvar::new() } }
    pub fn aborted(&self) -> bool { self.st.lock().unwrap().aborted }
    /// 1-based number of the next attempt (`backoff._attempt + 1`).
    pub fn next_attempt(&self) -> usize { self.st.lock().unwrap().attempt + 1 }
    /// The wait the next 429 would cause, for the progress line (Python computed it before calling `on_rate_limited`).
    pub fn preview(&self, retry_after: Option<f64>) -> Wait {
        let a = self.st.lock().unwrap().attempt;
        let cap = self.delays.last().map_or(0.0, |d| d.as_secs_f64());
        let delay = match retry_after.filter(|r| *r > 0.0) { Some(r) => r.min(cap), None => self.delays[a.min(self.delays.len() - 1)].as_secs_f64() };
        Wait { attempt: a + 1, of: self.delays.len(), delay_secs: delay, server_requested: retry_after.map_or(false, |r| r > 0.0) }
    }

    /// Called by the worker that got a 429. True = keep running; false = retries exhausted or cancelled (the worker should exit).
    pub fn on_rate_limited(&self, cancel: &Cancel, retry_after: Option<f64>) -> bool {
        let delay = {
            let mut g = self.st.lock().unwrap();
            if g.aborted { return false; }
            if !g.open { return true; }                        // another worker is already waiting it out
            if g.attempt >= self.delays.len() { g.aborted = true; self.cv.notify_all(); return false; }
            let cap = *self.delays.last().unwrap();
            let d = match retry_after.filter(|r| *r > 0.0) { Some(r) => Duration::from_secs_f64(r).min(cap), None => self.delays[g.attempt] };
            g.attempt += 1; g.open = false; d
        };
        let end = Instant::now() + delay;
        loop {
            if cancel.is_set() { let mut g = self.st.lock().unwrap(); g.open = true; self.cv.notify_all(); return false; }
            let now = Instant::now(); if now >= end { break; }
            std::thread::sleep((end - now).min(Duration::from_millis(250)));
        }
        let mut g = self.st.lock().unwrap(); if !g.aborted { g.open = true; } self.cv.notify_all();
        true
    }

    /// Blocks while the gate is closed. False if the pool was aborted or the job cancelled.
    pub fn wait_ready(&self, cancel: &Cancel) -> bool {
        let mut g = self.st.lock().unwrap();
        while !g.open {
            if cancel.is_set() { return false; }
            g = self.cv.wait_timeout(g, Duration::from_millis(250)).unwrap().0;
        }
        !g.aborted
    }
}

// ---------------------------------------------------------------- the two-level queue

/// The pools' queue: the on-screen (priority) games come out first, then the rest (`_next`). A priority item pushed while the normal queue is being
/// drained jumps the line; a re-queued rate-limited item goes to the priority side, as in Python.
pub struct WorkQueue<T> { q: Mutex<(VecDeque<T>, VecDeque<T>)>, cv: Condvar, rank: Option<Arc<dyn Fn(&T) -> u32 + Send + Sync>> }
impl<T> WorkQueue<T> {
    pub fn new(items: impl IntoIterator<Item = T>) -> Self { WorkQueue { q: Mutex::new((VecDeque::new(), items.into_iter().collect())), cv: Condvar::new(), rank: None } }
    /// A queue whose normal side is served by `rank` (lowest first, the order the items were given breaking ties) judged afresh at every `next()`, so what the player is looking at
    /// right now can change while the queue is being worked off. A priority item (a re-queued rate-limited one) still goes before any of it.
    pub fn with_ranker(items: impl IntoIterator<Item = T>, rank: Arc<dyn Fn(&T) -> u32 + Send + Sync>) -> Self { let mut q = Self::new(items); q.rank = Some(rank); q }
    pub fn push_priority(&self, t: T) { self.q.lock().unwrap().0.push_back(t); self.cv.notify_one(); }
    pub fn push(&self, t: T) { self.q.lock().unwrap().1.push_back(t); self.cv.notify_one(); }
    pub fn len(&self) -> usize { let g = self.q.lock().unwrap(); g.0.len() + g.1.len() }
    pub fn is_empty(&self) -> bool { self.len() == 0 }
    /// Priority first; None when both are empty (the pools stop on an empty queue, they do not wait for more).
    pub fn next(&self) -> Option<T> {
        let mut g = self.q.lock().unwrap();
        if let Some(t) = g.0.pop_front() { return Some(t); }
        let Some(rank) = &self.rank else { return g.1.pop_front() };
        let mut best: Option<(usize, u32)> = None;
        for (i, t) in g.1.iter().enumerate() { let r = rank(t); if best.map_or(true, |(_, b)| r < b) { best = Some((i, r)); if r == 0 { break; } } }
        best.and_then(|(i, _)| g.1.remove(i))
    }
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test] fn retry_after_forms() {
        assert_eq!(parse_retry_after("30"), Some(30.0));
        assert_eq!(parse_retry_after(" 2.5 "), Some(2.5));
        assert_eq!(parse_retry_after(""), None);
        assert_eq!(parse_retry_after("soon"), None);
        assert_eq!(parse_retry_after("Sun, 06 Nov 1994 08:49:37 GMT"), Some(0.0));      // in the past: 0, like max(0, ...)
        assert_eq!(parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT"), Some(784111777));  // the RFC example
    }
    #[test] fn url_stripping() {
        assert_eq!(strip_urls("failed to connect to https://api.steampowered.com/x?key=SECRET now"), "failed to connect to <url> now");
        assert!(!strip_urls("Get \"http://a/b?key=K\": timeout").contains('K'));
    }
    #[test] fn encoding() { assert_eq!(enc("a b&c=d/é"), "a%20b%26c%3Dd%2F%C3%A9"); assert_eq!(enc("AZaz09-_.~"), "AZaz09-_.~"); }

    #[test] fn a_ranked_queue_serves_the_lowest_rank_first_and_follows_a_rank_that_changes() {
        let focus = Arc::new(std::sync::atomic::AtomicI64::new(5));   // the item that is "on screen"
        let f2 = focus.clone();
        let q = WorkQueue::with_ranker(1..=8i64, Arc::new(move |x: &i64| if *x == f2.load(std::sync::atomic::Ordering::SeqCst) { 0 } else { 1 }));
        assert_eq!(q.next(), Some(5), "the focused item first");
        assert_eq!(q.next(), Some(1), "then the given order among equals");
        focus.store(7, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(q.next(), Some(7), "the focus moved: the new one jumps ahead");
        q.push_priority(99);
        assert_eq!(q.next(), Some(99), "a priority item still goes before any ranked one");
        let rest: Vec<i64> = std::iter::from_fn(|| q.next()).collect();
        assert_eq!(rest, vec![2, 3, 4, 6, 8]);
        assert!(WorkQueue::<i64>::new(vec![]).next().is_none());
    }
    fn fast(n: usize) -> PoolBackoff { PoolBackoff::with_delays("t", (0..n).map(|i| Duration::from_millis(20 * (i as u64 + 1))).collect()) }

    #[test] fn backoff_runs_out_then_aborts() {
        let b = fast(2); let c = Cancel::new();
        assert!(b.on_rate_limited(&c, None)); assert!(b.on_rate_limited(&c, None));
        assert!(!b.on_rate_limited(&c, None)); assert!(b.aborted()); assert!(!b.wait_ready(&c));
    }
    #[test] fn preview_matches_python_choice() {
        let b = PoolBackoff::new("t");
        assert_eq!(b.preview(None), Wait { attempt: 1, of: 4, delay_secs: 15.0, server_requested: false });
        assert_eq!(b.preview(Some(100.0)).delay_secs, 100.0);
        assert_eq!(b.preview(Some(99999.0)).delay_secs, 3615.0);       // capped at the largest delay
        assert!(!b.preview(Some(0.0)).server_requested);               // `if retry_after` is falsy for 0
    }
    #[test] fn others_wait_for_the_gate() {
        let b = Arc::new(fast(1)); let c = Cancel::new();
        let (b2, c2) = (b.clone(), c.clone());
        let t = std::thread::spawn(move || b2.on_rate_limited(&c2, None));
        std::thread::sleep(Duration::from_millis(5));
        assert!(b.on_rate_limited(&c, None), "a second 429 during the wait just joins it");
        let t0 = Instant::now(); assert!(b.wait_ready(&c)); assert!(t0.elapsed() < Duration::from_secs(2));
        assert!(t.join().unwrap());
        assert_eq!(b.next_attempt(), 2, "the joined 429 did not count as an attempt");
    }
    #[test] fn cancel_ends_the_wait() {
        let b = Arc::new(PoolBackoff::with_delays("t", vec![Duration::from_secs(60)])); let c = Cancel::new();
        let (b2, c2) = (b.clone(), c.clone());
        let t = std::thread::spawn(move || b2.on_rate_limited(&c2, None));
        std::thread::sleep(Duration::from_millis(20)); c.cancel();
        assert!(!t.join().unwrap());
        assert!(b.wait_ready(&c), "a cancelled wait reopens the gate (workers then see the cancel flag themselves), as in Python");
    }
    #[test] fn priority_first() {
        let q = WorkQueue::new([1, 2, 3]); q.push_priority(9); q.push_priority(8);
        assert_eq!((q.next(), q.next(), q.next()), (Some(9), Some(8), Some(1)));
        q.push_priority(7); assert_eq!(q.next(), Some(7)); assert_eq!(q.len(), 2);
        assert_eq!((q.next(), q.next(), q.next()), (Some(2), Some(3), None));
    }
}
