//! The single-instance guard.
//!
//! The daemon holds an exclusive OS lock on `daemon.lock` in the data directory
//! for the life of the process; the OS releases it when the process dies, so a
//! killed daemon never blocks the next start. Because an exclusive lock on
//! Windows also blocks other processes from reading the locked file, the
//! holder writes its pid, port and build id to `daemon.json` beside it. The
//! attach shim trusts that record only while the lock is held, and checks that
//! the process listening on the port is the one the record names.

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The file the daemon locks.
pub const LOCK_FILE: &str = "daemon.lock";
/// The record the lock holder writes beside it.
pub const RECORD_FILE: &str = "daemon.json";

/// The held lock. Dropping it releases the lock.
#[derive(Debug)]
pub struct DaemonLock {
    _file: File,
}

/// Why the lock could not be taken.
#[derive(Debug)]
pub enum LockProblem {
    /// Another process holds it.
    Held,
    /// The file could not be opened or locked.
    Io(io::Error),
}

impl DaemonLock {
    /// Takes the lock without waiting.
    pub fn acquire(dir: &Path) -> Result<Self, LockProblem> {
        let file = open_lock(dir).map_err(LockProblem::Io)?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(TryLockError::WouldBlock) => Err(LockProblem::Held),
            Err(TryLockError::Error(error)) => Err(LockProblem::Io(error)),
        }
    }
}

/// Whether some process holds the lock for `dir` right now.
pub fn is_held(dir: &Path) -> io::Result<bool> {
    if !dir.join(LOCK_FILE).exists() {
        return Ok(false);
    }
    let file = open_lock(dir)?;
    match file.try_lock() {
        Ok(()) => {
            file.unlock()?;
            Ok(false)
        }
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(error)) => Err(error),
    }
}

fn open_lock(dir: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(LOCK_FILE))
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
    /// Writes the record atomically: a temporary file, then a rename.
    pub fn write(&self, dir: &Path) -> io::Result<()> {
        let temporary = dir.join(format!("{RECORD_FILE}.{}.tmp", std::process::id()));
        let bytes = serde_json::to_vec(self).map_err(io::Error::other)?;
        std::fs::write(&temporary, bytes)?;
        std::fs::rename(&temporary, record_path(dir)).inspect_err(|_| {
            let _ = std::fs::remove_file(&temporary);
        })
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
