//! One Zest per data folder. Two copies writing the same database and `state.json` would corrupt them, so the first takes an exclusive lock on
//! `zest.lock` in the data folder and a second one is refused. The lock belongs to the open file, so a crash releases it: there is no stale lock to clean up.
use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::Path;

/// Holds the lock until it is dropped (keep it alive for the whole run).
#[derive(Debug)]
pub struct Instance { _file: File }

/// Takes the lock for `dir`. `Err` carries a sentence for the user when another Zest already runs there (or the folder cannot be written).
pub fn acquire(dir: &Path) -> Result<Instance, String> {
    let path = dir.join("zest.lock");
    let mut f = OpenOptions::new().create(true).read(true).write(true).truncate(false).open(&path).map_err(|e| format!("Cannot open {}: {e}", path.display()))?;
    match f.try_lock() {
        Ok(()) => { let _ = f.set_len(0); let _ = write!(f, "{}", std::process::id()); let _ = f.flush(); Ok(Instance { _file: f }) }
        Err(TryLockError::WouldBlock) => {
            let pid = std::fs::read_to_string(&path).ok().map(|t| t.trim().to_string()).filter(|t| !t.is_empty() && t.chars().all(|c| c.is_ascii_digit()));
            Err(format!("Zest is already running{} with the data folder {}. Close it first (it may be minimised or on another desktop), or start this one with a different folder.", pid.map(|p| format!(" (process {p})")).unwrap_or_default(), dir.display()))
        }
        Err(TryLockError::Error(e)) => Err(format!("Cannot lock {}: {e}", path.display())),
    }
}

/// [`acquire`], but a lock still held is tried again for up to `wait`: a Zest that has just been closed can take a few seconds to let go (a core dump being
/// written, files flushing), and a quick relaunch should not be told "already running" because of that.
pub fn acquire_waiting(dir: &Path, wait: std::time::Duration) -> Result<Instance, String> {
    let start = std::time::Instant::now();
    loop {
        match acquire(dir) {
            Ok(i) => return Ok(i),
            Err(e) if start.elapsed() >= wait || !e.contains("already running") => return Err(e),
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(200)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn a_relaunch_waits_for_a_closing_instance_but_not_for_a_running_one() {
        let d = std::env::temp_dir().join(format!("zest_inst_wait_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        let first = acquire(&d).unwrap();
        let t = std::thread::spawn(move || { std::thread::sleep(std::time::Duration::from_millis(500)); drop(first); });
        let started = std::time::Instant::now();
        let second = acquire_waiting(&d, std::time::Duration::from_secs(5)).expect("gets it once the first lets go");
        assert!(started.elapsed() >= std::time::Duration::from_millis(400) && started.elapsed() < std::time::Duration::from_secs(3));
        t.join().unwrap();
        // one that never lets go: refused after the wait
        let started = std::time::Instant::now();
        assert!(acquire_waiting(&d, std::time::Duration::from_millis(600)).unwrap_err().contains("already running"));
        assert!(started.elapsed() >= std::time::Duration::from_millis(600));
        drop(second);
    }
    #[test] fn a_second_instance_is_refused_until_the_first_ends() {
        let d = std::env::temp_dir().join(format!("zest_inst_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        let a = acquire(&d).expect("first");
        let e = acquire(&d).unwrap_err();
        assert!(e.contains("already running"), "{e}");
        assert_eq!(std::fs::read_to_string(d.join("zest.lock")).unwrap(), std::process::id().to_string());
        drop(a);
        let again = acquire(&d).expect("released with the first");
        // another folder is independent
        let d2 = d.join("other"); std::fs::create_dir_all(&d2).unwrap();
        assert!(acquire(&d2).is_ok());
        assert!(acquire(&d).is_err(), "still held by the second");
        drop(again);
    }
}
