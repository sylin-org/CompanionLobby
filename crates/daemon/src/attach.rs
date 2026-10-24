//! The attach shim behind `companion-lobby attach <companion>`.
//!
//! An MCP host launches the shim as a stdio server. The shim:
//!
//! * finds the daemon for this data directory, starting it windowless and
//!   detached when none runs and restarting it when it runs another build, and
//!   verifies that the process on the port is the daemon lock holder before it
//!   sends the attach secret (see [`crate::contact`]);
//! * attaches as the companion the host configuration names, by id or moniker,
//!   and relays every frame from the host to the daemon on behalf of that one
//!   attachment, writing each answer back as one line;
//! * keeps stdout for protocol frames only: everything it has to say to a
//!   person goes to stderr;
//! * reads newline-delimited frames of at most 8 MiB, answers an unparseable
//!   frame locally with a parse error, and stops after an oversized one,
//!   because the stream cannot be resynchronized after it;
//! * detaches when the host closes stdin, and leaves the daemon running.
//!
//! With `--log <file>` it appends every frame it relays, in both directions,
//! to that file as JSON lines, so a host's conversation can be recorded.

use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::contact::{self, Daemon};
use crate::home;
use crate::http::{self, ATTACHMENT_HEADER};
use crate::loopback::{self, Call};
use crate::mcp::{INTERNAL_ERROR, PARSE_ERROR, rpc_error};

/// The largest frame a host may send.
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
/// How long the shim may spend finding or starting the daemon.
pub const STARTUP_BUDGET: Duration = Duration::from_secs(10);
/// How long one relayed request may take.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(120);

/// Why a frame could not be read.
#[derive(Debug, PartialEq, Eq)]
pub enum FrameError {
    /// The frame is larger than [`MAX_FRAME_BYTES`].
    Oversized,
    /// The stream failed.
    Read,
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Oversized => "frame exceeds 8 MiB",
            Self::Read => "stream read failed",
        })
    }
}

/// Reads one newline-terminated frame into `buffer`, without its newline.
/// Returns the bytes consumed; 0 means the stream ended.
pub fn read_frame(reader: &mut impl BufRead, buffer: &mut String) -> Result<usize, FrameError> {
    buffer.clear();
    let mut bytes = Vec::new();
    let mut had_newline = false;
    loop {
        let available = match reader.fill_buf() {
            Ok(available) => available,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(FrameError::Read),
        };
        if available.is_empty() {
            if bytes.is_empty() {
                return Ok(0);
            }
            break;
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |index| index + 1);
        if bytes.len().saturating_add(consumed) > MAX_FRAME_BYTES + 1 {
            return Err(FrameError::Oversized);
        }
        let content = newline.unwrap_or(consumed);
        bytes.extend_from_slice(&available[..content]);
        reader.consume(consumed);
        if newline.is_some() {
            had_newline = true;
            break;
        }
    }
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(FrameError::Oversized);
    }
    let consumed = bytes.len() + usize::from(had_newline);
    buffer.push_str(&String::from_utf8_lossy(&bytes));
    Ok(consumed)
}

/// The optional record of a host's conversation.
struct Transcript {
    file: Mutex<File>,
    started: Instant,
}

impl Transcript {
    fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            file: Mutex::new(file),
            started: Instant::now(),
        })
    }

    fn record(&self, direction: &str, frame: Value) {
        let line = json!({ "ms": self.started.elapsed().as_millis() as u64, "dir": direction, "frame": frame });
        if let Ok(mut file) = self.file.lock() {
            let _ = writeln!(file, "{line}");
        }
    }
}

/// The relay's link to its attachment in the daemon.
struct Link {
    companion: String,
    daemon: Mutex<Daemon>,
    attachment: Mutex<String>,
}

impl Link {
    fn attach(daemon: Daemon, companion: &str) -> Result<Self, String> {
        let attachment = attach_once(&daemon, companion)?;
        Ok(Self {
            companion: companion.to_owned(),
            daemon: Mutex::new(daemon),
            attachment: Mutex::new(attachment),
        })
    }

    fn snapshot(&self) -> (Daemon, String) {
        let daemon = self
            .daemon
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let attachment = self
            .attachment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        (daemon, attachment)
    }

    /// Relays one message; `None` when there is nothing to send back.
    fn forward(&self, message: &Value) -> Option<Value> {
        let id = message.get("id").cloned();
        let body = serde_json::to_vec(message).unwrap_or_default();
        for attempt in 0..2 {
            let (daemon, attachment) = self.snapshot();
            let bearer = format!("Bearer {}", daemon.secret.expose());
            let call = Call {
                method: "POST",
                path: "/mcp",
                headers: &[("Authorization", &bearer), (ATTACHMENT_HEADER, &attachment)],
                body: &body,
                timeout: CALL_TIMEOUT,
            };
            match loopback::send(daemon.port, &call) {
                Ok(answer) if answer.status == 200 => {
                    return serde_json::from_slice(&answer.body).ok().or_else(|| {
                        id.as_ref().map(|id| {
                            rpc_error(id, INTERNAL_ERROR, "The lobby sent an unreadable answer.")
                        })
                    });
                }
                Ok(answer) if answer.status == 202 => return None,
                // The daemon has forgotten this attachment, or is gone:
                // it was restarted. Attach again once, then give up.
                _ if attempt == 0 && self.reattach().is_ok() => continue,
                _ => break,
            }
        }
        id.map(|id| {
            rpc_error(
                &id,
                INTERNAL_ERROR,
                "The Companion Lobby daemon did not answer. Restart this MCP server in the host.",
            )
        })
    }

    fn reattach(&self) -> Result<(), String> {
        let (daemon, _) = self.snapshot();
        let daemon = contact::ensure(&daemon.dir, daemon.port, STARTUP_BUDGET)?;
        let attachment = attach_once(&daemon, &self.companion)?;
        *self
            .daemon
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = daemon;
        *self
            .attachment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = attachment;
        Ok(())
    }

    fn detach(&self) {
        let (daemon, attachment) = self.snapshot();
        let bearer = format!("Bearer {}", daemon.secret.expose());
        let path = format!("/mcp/attach/{attachment}");
        let call = Call {
            method: "DELETE",
            path: &path,
            headers: &[("Authorization", &bearer)],
            body: &[],
            timeout: contact::PROBE_TIMEOUT,
        };
        let _ = loopback::send(daemon.port, &call);
    }
}

fn attach_once(daemon: &Daemon, companion: &str) -> Result<String, String> {
    let bearer = format!("Bearer {}", daemon.secret.expose());
    let body = serde_json::to_vec(&json!({ "companion": companion })).unwrap_or_default();
    let call = Call {
        method: "POST",
        path: "/mcp/attach",
        headers: &[("Authorization", &bearer)],
        body: &body,
        timeout: contact::PROBE_TIMEOUT,
    };
    let answer = loopback::send(daemon.port, &call)
        .map_err(|error| format!("cannot reach the daemon to attach: {error}"))?;
    let value: Value = serde_json::from_slice(&answer.body).unwrap_or(Value::Null);
    if answer.status != 201 {
        let message = value
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("the daemon refused the attachment");
        return Err(format!("cannot attach as {companion}: {message}"));
    }
    value
        .get("attachment")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "the daemon's attach answer named no attachment".to_owned())
}

/// Runs the shim for `companion`, returning the process exit code.
pub fn run(companion: &str, log: Option<&Path>) -> u8 {
    let transcript = match log.map(Transcript::open).transpose() {
        Ok(transcript) => transcript.map(Arc::new),
        Err(error) => {
            eprintln!("companion-lobby attach: cannot open the log file: {error}");
            return 1;
        }
    };
    if let Some(transcript) = &transcript {
        transcript.record(
            "shim",
            json!({
                "event": "start",
                "version": env!("CARGO_PKG_VERSION"),
                "dataDirFromEnvironment": std::env::var_os(home::DATA_DIR_ENV).is_some(),
                "portFromEnvironment": std::env::var_os(http::PORT_ENV).is_some(),
            }),
        );
    }
    let started = Instant::now();
    let link = match connect(companion) {
        Ok(link) => link,
        Err(message) => {
            if let Some(transcript) = &transcript {
                transcript.record("shim", json!({ "event": "refused", "message": message }));
            }
            eprintln!("companion-lobby attach: {message}");
            return 1;
        }
    };
    if let Some(transcript) = &transcript {
        transcript.record(
            "shim",
            json!({ "event": "attached", "startupMs": started.elapsed().as_millis() as u64 }),
        );
    }
    relay(Arc::new(link), transcript);
    0
}

fn connect(companion: &str) -> Result<Link, String> {
    let dir = home::resolve().map_err(|problem| problem.to_string())?;
    let port = http::resolve_port()?;
    let daemon = contact::ensure(&dir, port, STARTUP_BUDGET)?;
    Link::attach(daemon, companion)
}

fn relay(link: Arc<Link>, transcript: Option<Arc<Transcript>>) {
    let stdout = Arc::new(Mutex::new(io::stdout()));
    let write = {
        let stdout = stdout.clone();
        let transcript = transcript.clone();
        move |frame: &Value| -> bool {
            if let Some(transcript) = &transcript {
                transcript.record("lobby", frame.clone());
            }
            let mut line = serde_json::to_string(frame).unwrap_or_default();
            line.push('\n');
            let mut stdout = stdout
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            stdout
                .write_all(line.as_bytes())
                .and_then(|()| stdout.flush())
                .is_ok()
        }
    };
    let write = Arc::new(write);
    let mut reader = BufReader::new(io::stdin().lock());
    let mut buffer = String::new();
    let mut workers: Vec<std::thread::JoinHandle<()>> = Vec::new();
    loop {
        match read_frame(&mut reader, &mut buffer) {
            Ok(0) | Err(FrameError::Read) => break,
            Ok(_) => {}
            Err(FrameError::Oversized) => {
                write(&rpc_error(&Value::Null, PARSE_ERROR, "frame exceeds 8 MiB"));
                break;
            }
        }
        let line = buffer.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(line) else {
            if let Some(transcript) = &transcript {
                transcript.record("host", Value::String(line.chars().take(4096).collect()));
            }
            if !write(&rpc_error(&Value::Null, PARSE_ERROR, "Parse error")) {
                break;
            }
            continue;
        };
        if let Some(transcript) = &transcript {
            transcript.record("host", message.clone());
        }
        workers.retain(|worker| !worker.is_finished());
        let link = link.clone();
        let write = write.clone();
        workers.push(std::thread::spawn(move || {
            if let Some(answer) = link.forward(&message) {
                write(&answer);
            }
        }));
    }
    for worker in workers {
        let _ = worker.join();
    }
    link.detach();
    if let Some(transcript) = &transcript {
        transcript.record("shim", json!({ "event": "stdin closed; detached" }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn frames_are_one_line_each_and_bounded() {
        let mut batched = Cursor::new(b"{\"id\":1}\n{\"id\":2}\r\n".to_vec());
        let mut frame = String::new();
        assert_eq!(read_frame(&mut batched, &mut frame), Ok(9));
        assert_eq!(frame, "{\"id\":1}");
        assert_eq!(read_frame(&mut batched, &mut frame), Ok(10));
        assert_eq!(frame.trim(), "{\"id\":2}");
        assert_eq!(read_frame(&mut batched, &mut frame), Ok(0));

        let mut unterminated = Cursor::new(b"{\"id\":3}".to_vec());
        assert_eq!(read_frame(&mut unterminated, &mut frame), Ok(8));
        assert_eq!(frame, "{\"id\":3}");

        let mut oversized = Cursor::new(vec![b'x'; MAX_FRAME_BYTES + 1]);
        assert_eq!(
            read_frame(&mut oversized, &mut frame),
            Err(FrameError::Oversized)
        );

        let mut exactly = vec![b'x'; MAX_FRAME_BYTES];
        exactly.push(b'\n');
        assert_eq!(
            read_frame(&mut Cursor::new(exactly), &mut frame),
            Ok(MAX_FRAME_BYTES + 1)
        );
    }
}
