//! The diagnostics spoke: subscribes to the event bus and appends a bounded JSONL journal of
//! connector activity. Events carry no secrets by construction; this is operational evidence,
//! not a model transcript.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use crate::application::bus::EventBus;
use companion_core::domain::events::DomainEvent;

const ROTATE_BYTES: u64 = 1024 * 1024;

/// Spawns the journal thread. It exits when the bus (and its publishers) go away.
pub fn spawn(events: &EventBus, data_dir: std::path::PathBuf) {
    let receiver = events.subscribe();
    std::thread::Builder::new()
        .name("companion-diagnostics".into())
        .spawn(move || journal_loop(receiver, data_dir))
        .expect("diagnostics thread");
}

fn journal_loop(receiver: Receiver<DomainEvent>, data_dir: PathBuf) {
    let path = data_dir.join("connector.log");
    while let Ok(event) = receiver.recv() {
        let line = serde_json::to_string(&event).unwrap_or_else(|_| "{\"kind\":\"unserializable\"}".to_string());
        if let Err(error) = append(&path, &line) {
            eprintln!("companion-lobby: diagnostics journal failed: {error}");
            return;
        }
        rotate_if_large(&path);
    }
}

fn append(path: &PathBuf, line: &str) -> std::io::Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(line.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()
}

fn rotate_if_large(path: &PathBuf) {
    if let Ok(metadata) = std::fs::metadata(path) {
        if metadata.len() > ROTATE_BYTES {
            let rotated = path.with_extension("log.old");
            let _ = std::fs::remove_file(&rotated);
            let _ = std::fs::rename(path, rotated);
        }
    }
}
