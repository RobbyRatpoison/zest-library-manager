//! Follows installs and uninstalls of non-Steam games while Zest is open (PlayDate's `PluginInstallWatcher`, one for all sources). Each source names the folders whose
//! changes matter (`LibrarySource::watch_paths`); a change there marks the source as pending, and once it has been quiet for a moment the app runs that source's
//! `installed_scan`. A game install or an EA clean-up produces thousands of events, so only the latest time per source is kept and the scan runs once.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

/// How long a source has to be quiet before it is scanned (PlayDate used 1.5 s for EA and Battle.net, 3 s for Legacy Games: Wine flushes `user.reg` a few seconds apart).
pub const QUIET: Duration = Duration::from_millis(2500);
/// A folder that did not exist yet (EA's "EA Games" before the first install) is looked for again this often.
pub const RETRY: Duration = Duration::from_secs(30);

/// One watched folder: the source it belongs to, the folder, and whether its subtree counts.
pub type Spec = (String, PathBuf, bool);

pub struct InstallWatch {
    watcher: notify::RecommendedWatcher,
    rx: mpsc::Receiver<String>,
    roots: Arc<Mutex<Vec<(PathBuf, String)>>>,
    watched: HashMap<PathBuf, bool>,
    pending: HashMap<String, Instant>,
    last_refresh: Option<Instant>,
}

/// The sources whose folders contain any of `paths` (an event on a file inside a watched folder, or on the folder itself).
pub fn sources_for(roots: &[(PathBuf, String)], paths: &[PathBuf]) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for (root, id) in roots { if paths.iter().any(|p| p.starts_with(root)) && !out.contains(id) { out.push(id.clone()); } }
    out
}

/// The sources that have been quiet for `quiet` as of `now`; they leave `pending`. The rest stay.
pub fn settled(pending: &mut HashMap<String, Instant>, now: Instant, quiet: Duration) -> Vec<String> {
    let done: Vec<String> = pending.iter().filter(|(_, t)| now.duration_since(**t) >= quiet).map(|(id, _)| id.clone()).collect();
    for id in &done { pending.remove(id); }
    let mut done = done; done.sort();
    done
}

impl InstallWatch {
    pub fn new(ctx: egui::Context) -> Option<Self> {
        let (tx, rx) = mpsc::channel();
        let roots: Arc<Mutex<Vec<(PathBuf, String)>>> = Arc::new(Mutex::new(vec![]));
        let r2 = roots.clone();
        let last = Arc::new(Mutex::new(Instant::now() - Duration::from_secs(10)));
        let watcher = notify::recommended_watcher(move |ev: notify::Result<notify::Event>| {
            let Ok(ev) = ev else { return };
            if matches!(ev.kind, notify::EventKind::Access(_)) { return; }
            let ids = sources_for(&r2.lock().map(|g| g.clone()).unwrap_or_default(), &ev.paths);
            if ids.is_empty() { return; }
            for id in ids { let _ = tx.send(id); }
            // an install is thousands of events: wake the UI at most a few times a second
            let mut l = last.lock().unwrap_or_else(|e| e.into_inner());
            if l.elapsed() >= Duration::from_millis(250) { *l = Instant::now(); ctx.request_repaint(); }
        }).ok()?;
        Some(InstallWatch { watcher, rx, roots, watched: HashMap::new(), pending: HashMap::new(), last_refresh: None })
    }

    /// Starts watching every folder in `specs` that exists and is not watched yet. Cheap; called now and then so a folder that appears later is picked up.
    pub fn refresh(&mut self, specs: &[Spec]) -> usize {
        use notify::Watcher;
        self.last_refresh = Some(Instant::now());
        let mut missing = 0;
        for (id, path, deep) in specs {
            if self.watched.contains_key(path) { self.add_root(path, id); continue; }
            if !path.is_dir() { missing += 1; continue; }
            let mode = if *deep { notify::RecursiveMode::Recursive } else { notify::RecursiveMode::NonRecursive };
            if self.watcher.watch(path, mode).is_ok() { self.watched.insert(path.clone(), *deep); self.add_root(path, id); pd_filter::net::log(&format!("watching {} for installs ({id})", path.display())); }
        }
        missing
    }
    /// Events carry the folder's real path (symlinks resolved: macOS reports /private/var for /var, and a games folder can be a link to another drive),
    /// so a root is matched both as given and as its real path.
    fn add_root(&self, path: &Path, id: &str) {
        let real = std::fs::canonicalize(path).ok().filter(|r| r != path);
        if let Ok(mut r) = self.roots.lock() {
            for p in std::iter::once(path.to_path_buf()).chain(real) { if !r.iter().any(|(q, i)| *q == p && i == id) { r.push((p, id.to_string())); } }
        }
    }
    pub fn due_for_refresh(&self) -> bool { self.last_refresh.map_or(true, |t| t.elapsed() >= RETRY) }

    /// Source ids to scan now. Drains the events, then returns the sources that have been quiet long enough.
    pub fn poll(&mut self) -> Vec<String> {
        let now = Instant::now();
        while let Ok(id) = self.rx.try_recv() { self.pending.insert(id, now); }
        settled(&mut self.pending, now, QUIET)
    }
    pub fn has_pending(&self) -> bool { !self.pending.is_empty() }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn events_map_to_the_sources_whose_folders_contain_them() {
        let roots = vec![(PathBuf::from("/p/ea/EA Games"), "ea_app".to_string()), (PathBuf::from("/p/legacy"), "legacy_games".to_string()), (PathBuf::from("/p/legacy"), "other".to_string())];
        assert_eq!(sources_for(&roots, &[PathBuf::from("/p/ea/EA Games/Game/x.dll")]), vec!["ea_app"]);
        assert_eq!(sources_for(&roots, &[PathBuf::from("/p/legacy/user.reg")]), vec!["legacy_games", "other"]);
        assert!(sources_for(&roots, &[PathBuf::from("/p/ea/EA Gamesx/y")]).is_empty(), "a sibling folder with the same start is not inside");
        assert!(sources_for(&roots, &[PathBuf::from("/elsewhere")]).is_empty());
    }
    #[test] fn a_source_is_scanned_once_after_it_goes_quiet() {
        let t0 = Instant::now();
        let mut p: HashMap<String, Instant> = HashMap::new();
        p.insert("a".into(), t0); p.insert("b".into(), t0 + Duration::from_secs(2));
        assert!(settled(&mut p, t0 + Duration::from_secs(1), QUIET).is_empty(), "still busy");
        assert_eq!(settled(&mut p, t0 + Duration::from_millis(2600), QUIET), vec!["a"]);
        assert_eq!(p.len(), 1, "b keeps waiting");
        assert_eq!(settled(&mut p, t0 + Duration::from_secs(5), QUIET), vec!["b"]);
        assert!(p.is_empty());
    }
    #[test] fn a_real_folder_change_reaches_poll() {
        let d = std::env::temp_dir().join(format!("zest_iw_{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(d.join("games")).unwrap();
        let mut w = InstallWatch::new(egui::Context::default()).unwrap();
        // the second folder does not exist yet: nothing watches it until it appears
        w.refresh(&[("x".into(), d.join("games"), true), ("y".into(), d.join("later"), false)]);
        assert!(w.watched.contains_key(&d.join("games")) && !w.watched.contains_key(&d.join("later")));
        std::fs::create_dir_all(d.join("games/Game/__Installer")).unwrap(); std::fs::write(d.join("games/Game/__Installer/a.xml"), "x").unwrap();
        std::fs::create_dir_all(d.join("later")).unwrap();
        w.refresh(&[("x".into(), d.join("games"), true), ("y".into(), d.join("later"), false)]);
        assert!(w.watched.contains_key(&d.join("later")));
        // events arrive on another thread: wait for them
        let mut got = false;
        for _ in 0..100 { std::thread::sleep(Duration::from_millis(50)); while let Ok(id) = w.rx.try_recv() { w.pending.insert(id, Instant::now() - QUIET); got = true; } if got { break; } }
        assert!(got, "the install touched a watched folder");
        // macOS (FSEvents) may also report the creation of the folder "y" watches, which only costs a harmless extra scan of y
        let due = w.poll(); assert!(due.contains(&"x".to_string()) && due.iter().all(|i| i == "x" || i == "y"), "{due:?}");
        assert!(!w.has_pending());
        let _ = std::fs::remove_dir_all(&d);
    }
}
