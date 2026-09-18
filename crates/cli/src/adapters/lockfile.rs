//! The data-directory lock: a minimal exclusive lockfile guarding long-running verbs
//! (`serve`, `operator`) against cross-process whole-file state clobbering. The file
//! (`lock`) is created with `create_new` and carries the holder's pid and an epoch-ms
//! timestamp; it is removed on clean exit (Drop) and by the tray's Quit. `--force`
//! overrides a lock the operator judges stale — that is the only staleness escape; no
//! liveness probing is attempted. One-shot CLI verbs do not lock.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

const LOCK_FILE: &str = "lock";
/// Settling delay after a contended `create_new` before reading the holder back, so the
/// holder's content is visible.
const CONTENT_SETTLE: Duration = Duration::from_millis(50);

/// A held data-directory lock. Clone-cheap; the file is removed exactly once, on the
/// first of `release` or the final `Drop`.
#[derive(Clone, Debug)]
pub struct DataDirLock {
    inner: Arc<LockInner>,
}

#[derive(Debug)]
struct LockInner {
    path: PathBuf,
    released: AtomicBool,
}

impl DataDirLock {
    /// Acquires the lock, refusing when another process holds it (naming the holder).
    /// `force` removes an existing lock first — the documented stale-lock escape.
    pub fn acquire(data_dir: &Path, force: bool) -> Result<Self, String> {
        fs::create_dir_all(data_dir).map_err(|error| format!("cannot create data directory: {error}"))?;
        let path = data_dir.join(LOCK_FILE);
        if force {
            let _ = fs::remove_file(&path);
        }
        let created = fs::OpenOptions::new().write(true).create_new(true).open(&path);
        match created {
            Ok(mut file) => {
                use std::io::Write as _;
                let content = format!("pid={}\nacquired={}\n", std::process::id(), now_millis());
                let _ = file.write_all(content.as_bytes());
                let _ = file.sync_all();
                Ok(Self { inner: Arc::new(LockInner { path, released: AtomicBool::new(false) }) })
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(format!(
                    "another tangent-connector process holds this state directory ({});
                    stop it first, or pass --force if that lock is stale",
                    describe_holder(&path)
                ))
            }
            Err(error) => Err(format!("cannot create the state directory lock: {error}")),
        }
    }

    /// Removes the lock file; idempotent.
    pub fn release(&self) {
        if !self.inner.released.swap(true, Ordering::SeqCst) {
            let _ = fs::remove_file(&self.inner.path);
        }
    }
}

impl Drop for LockInner {
    fn drop(&mut self) {
        if !self.released.swap(true, Ordering::SeqCst) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn describe_holder(path: &Path) -> String {
    // Settle briefly so a just-written lock is readable, then report what it says.
    std::thread::sleep(CONTENT_SETTLE);
    match fs::read_to_string(path) {
        Ok(content) if !content.trim().is_empty() => content.trim().replace('\n', " · "),
        _ => "pid and start time unknown".to_string(),
    }
}

/// What the state directory's lock file says about its current holder — the honest
/// naming used by bind refusals and other cross-process diagnostics. Reads only.
pub fn holder_of(data_dir: &Path) -> String {
    describe_holder(&data_dir.join(LOCK_FILE))
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tangent-connector-lock-{}-{}", label, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn a_second_acquire_is_refused_and_names_the_holder() {
        let dir = dir("refuse");
        let first = DataDirLock::acquire(&dir, false).expect("first acquire");
        let refusal = DataDirLock::acquire(&dir, false).expect_err("must refuse");
        assert!(refusal.contains("holds this state directory"), "error was: {refusal}");
        assert!(refusal.contains(&format!("pid={}", std::process::id())), "the holder is named: {refusal}");
        // Releasing (or dropping) the first lock re-admits a second.
        first.release();
        assert!(DataDirLock::acquire(&dir, false).is_ok(), "release re-admits");
    }

    #[test]
    fn drop_removes_the_lock_and_force_overrides_a_stale_one() {
        let dir = dir("force");
        let first = DataDirLock::acquire(&dir, false).expect("acquire");
        // Still held: force is the documented escape for a stale lock.
        let second = DataDirLock::acquire(&dir, true).expect("force overrides");
        // The forced lock is now the live one; a plain acquire is refused again.
        assert!(DataDirLock::acquire(&dir, false).is_err(), "the forced lock holds");
        drop(first);
        drop(second);
        assert!(DataDirLock::acquire(&dir, false).is_ok(), "all handles dropped re-admits");
    }
}
