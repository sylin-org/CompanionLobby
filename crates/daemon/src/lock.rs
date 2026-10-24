//! The single-instance guard.
//!
//! The daemon holds exclusive OS locks on two files in the data directory for
//! the life of the process; the OS releases them when the process dies, so a
//! killed daemon never blocks the next start.
//!
//! * `daemon.instance` decides which daemon runs. A starting daemon tries it
//!   once and, when another daemon holds it, refuses to start at once. Nothing
//!   else ever touches it.
//! * `daemon.lock` says that a daemon runs. [`is_held`] learns that by taking
//!   it for an instant, so the instance holder waits out such a probe when it
//!   takes this lock; a probe can never make a start fail.
//!
//! Because an exclusive lock on Windows also blocks other processes from
//! reading the locked file, the holder writes its pid, port and build id to
//! `daemon.json` beside them. The attach shim trusts that record only while
//! the lock is held, and checks that the process listening on the port is the
//! one the record names.

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The file whose lock says that a daemon runs; probed by [`is_held`].
pub const LOCK_FILE: &str = "daemon.lock";
/// The file whose lock decides which daemon runs; never probed.
pub const INSTANCE_FILE: &str = "daemon.instance";
/// The record the lock holder writes beside it.
pub const RECORD_FILE: &str = "daemon.json";
/// How many times the instance holder tries [`LOCK_FILE`] while probes hold it.
pub const PROBE_WAIT_ATTEMPTS: u32 = 40;
/// The pause between those tries: together far longer than any probe holds it.
pub const PROBE_WAIT_PAUSE: Duration = Duration::from_millis(25);

/// The held locks. Dropping it releases the instance first, then the lock,
/// so a daemon that starts in between waits for the lock rather than refusing.
#[derive(Debug)]
pub struct DaemonLock {
    _instance: File,
    _running: File,
}

/// Why the lock could not be taken.
#[derive(Debug)]
pub enum LockProblem {
    /// Another daemon holds it.
    Held,
    /// The file could not be opened or locked.
    Io(io::Error),
}

impl DaemonLock {
    /// Takes the instance at once or reports [`LockProblem::Held`], then takes
    /// the lock, waiting out any probe that holds it for an instant.
    pub fn acquire(dir: &Path) -> Result<Self, LockProblem> {
        let instance = open(dir, INSTANCE_FILE).map_err(LockProblem::Io)?;
        match instance.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Err(LockProblem::Held),
            Err(TryLockError::Error(error)) => return Err(LockProblem::Io(error)),
        }
        // Only the instance holder holds this lock for longer than a probe.
        let running = open(dir, LOCK_FILE).map_err(LockProblem::Io)?;
        let mut attempt = 1;
        loop {
            match running.try_lock() {
                Ok(()) => {
                    return Ok(Self {
                        _instance: instance,
                        _running: running,
                    });
                }
                Err(TryLockError::WouldBlock) if attempt < PROBE_WAIT_ATTEMPTS => {
                    attempt += 1;
                    std::thread::sleep(PROBE_WAIT_PAUSE);
                }
                Err(TryLockError::WouldBlock) => {
                    return Err(LockProblem::Io(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "daemon.lock stayed taken by a process that is not a daemon",
                    )));
                }
                Err(TryLockError::Error(error)) => return Err(LockProblem::Io(error)),
            }
        }
    }
}

/// Whether some process holds the lock for `dir` right now.
pub fn is_held(dir: &Path) -> io::Result<bool> {
    if !dir.join(LOCK_FILE).exists() {
        return Ok(false);
    }
    let file = open(dir, LOCK_FILE)?;
    match file.try_lock() {
        Ok(()) => {
            file.unlock()?;
            Ok(false)
        }
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(error)) => Err(error),
    }
}

fn open(dir: &Path, name: &str) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(name))
}

/// What the lock holder says about itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DaemonRecord {
    /// The holder's process id.
    pub pid: u32,
    /// The loopback port it listens on.
    pub port: u16,
    /// The build it runs.
    pub build_id: String,
}

impl DaemonRecord {
    /// Writes the record atomically.
    pub fn write(&self, dir: &Path) -> io::Result<()> {
        let bytes = serde_json::to_vec(self).map_err(io::Error::other)?;
        crate::store::atomic_write(&record_path(dir), &bytes)
    }

    /// Reads the record, if one exists and parses.
    pub fn read(dir: &Path) -> io::Result<Self> {
        let bytes = std::fs::read(record_path(dir))?;
        serde_json::from_slice(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

    /// Removes the record if it is still this process's own.
    pub fn remove_own(dir: &Path) {
        if Self::read(dir).is_ok_and(|record| record.pid == std::process::id()) {
            let _ = std::fs::remove_file(record_path(dir));
        }
    }
}

fn record_path(dir: &Path) -> PathBuf {
    dir.join(RECORD_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lobby-lock-{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7().simple()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn one_holder_at_a_time_and_release_on_drop() {
        let dir = temp_dir("holder");
        assert!(!is_held(&dir).expect("probe"));
        let held = DaemonLock::acquire(&dir).expect("first");
        assert!(is_held(&dir).expect("probe"));
        assert!(matches!(DaemonLock::acquire(&dir), Err(LockProblem::Held)));
        drop(held);
        assert!(!is_held(&dir).expect("probe"));
        let _again = DaemonLock::acquire(&dir).expect("after release");
        drop(_again);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_start_waits_out_a_probe_and_refuses_at_once_when_a_daemon_runs() {
        let dir = temp_dir("probe");
        // A probe takes daemon.lock for a moment, as is_held does.
        let probe = open(&dir, LOCK_FILE).expect("open");
        probe.try_lock().expect("probe");
        let release = std::thread::spawn(move || {
            std::thread::sleep(PROBE_WAIT_PAUSE * 4);
            drop(probe);
        });
        let running = DaemonLock::acquire(&dir).expect("the start outlasts the probe");
        release.join().expect("release");
        assert!(is_held(&dir).expect("probe"));
        // A second daemon is refused without waiting.
        let started = std::time::Instant::now();
        assert!(matches!(DaemonLock::acquire(&dir), Err(LockProblem::Held)));
        assert!(started.elapsed() < PROBE_WAIT_PAUSE * (PROBE_WAIT_ATTEMPTS / 2));
        drop(running);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_record_round_trips_and_refuses_unknown_fields() {
        let dir = temp_dir("record");
        let record = DaemonRecord {
            pid: std::process::id(),
            port: 5219,
            build_id: "abc".to_owned(),
        };
        record.write(&dir).expect("write");
        assert_eq!(DaemonRecord::read(&dir).expect("read"), record);
        std::fs::write(
            dir.join(RECORD_FILE),
            br#"{"pid":1,"port":2,"buildId":"x","extra":true}"#,
        )
        .expect("write");
        assert!(DaemonRecord::read(&dir).is_err());
        record.write(&dir).expect("write");
        DaemonRecord::remove_own(&dir);
        assert!(DaemonRecord::read(&dir).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
