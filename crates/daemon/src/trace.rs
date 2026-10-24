//! The daemon's own operational log.
//!
//! A bounded log of what the daemon itself did (start, stop, refusals, faults)
//! in `trace.log` in the data directory, rotated to `trace.1.log` at 1 MiB so
//! at most two generations exist. A panic hook records the panic before the
//! process ends. The log never holds secrets or content, and it is separate
//! from the journal, which attributes what companions did.
//!
//! A daemon started by the attach shim has no console and null standard
//! streams, so this file is where it speaks.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// The live log file's name.
pub const TRACE_FILE: &str = "trace.log";
/// The previous generation's name.
pub const TRACE_PREVIOUS: &str = "trace.1.log";
/// The size at which the live file rotates.
pub const TRACE_LIMIT: u64 = 1024 * 1024;

struct Sink {
    dir: PathBuf,
    file: File,
    written: u64,
}

static SINK: OnceLock<Mutex<Sink>> = OnceLock::new();

/// Opens the log in `dir` and installs the panic hook. Later calls do nothing.
pub fn install(dir: &Path) -> std::io::Result<()> {
    let file = open(dir)?;
    let written = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let sink = Sink {
        dir: dir.to_path_buf(),
        file,
        written,
    };
    if SINK.set(Mutex::new(sink)).is_ok() {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            line(&format!("panic: {info}"));
            previous(info);
        }));
    }
    Ok(())
}

fn open(dir: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(TRACE_FILE))
}

/// Appends one line, prefixed with the time and the process id. Failures are
/// swallowed: the log must never take the daemon down.
pub fn line(message: &str) {
    let Some(sink) = SINK.get() else {
        return;
    };
    let Ok(mut sink) = sink.lock() else {
        return;
    };
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or(0);
    let text = format!(
        "{millis} pid={} {}\n",
        std::process::id(),
        message.replace(['\r', '\n'], " ")
    );
    if sink.written + text.len() as u64 > TRACE_LIMIT {
        let dir = sink.dir.clone();
        let _ = std::fs::rename(dir.join(TRACE_FILE), dir.join(TRACE_PREVIOUS));
        if let Ok(file) = open(&dir) {
            sink.file = file;
            sink.written = 0;
        }
    }
    if sink.file.write_all(text.as_bytes()).is_ok() {
        sink.written += text.len() as u64;
    }
}
