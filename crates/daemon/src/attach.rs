//! The attach shim behind `companion-lobby attach <companion>`.
//!
//! An MCP host launches the shim as a stdio server. The shim:
//!
//! * finds the daemon for this data directory, starting it windowless and
//!   detached when none runs and restarting it when it runs another build, and
//!   verifies that the process on the port is the daemon lock holder before it
//!   sends the attach secret (see [`crate::contact`]);
//! * attaches as the companion the host configuration names, by id or moniker,
//!   presenting the install's attach secret once; the daemon answers with a
//!   token for this one attachment, and every relayed frame and the detach
//!   carry that token, never the install secret;
//! * relays every frame from the host to the daemon on behalf of that one
//!   attachment, writing each answer back as one line; notifications go in
//!   the order they arrived, before any later request, and at most
//!   [`MAX_IN_FLIGHT`] requests are relayed at once;
//! * follows the attachment's notice stream, opened before the first frame
//!   is read, and writes each notification it carries to the host, so the
//!   host learns that the companion's tools changed;
//! * keeps stdout for protocol frames only: everything it has to say to a
//!   person goes to stderr;
//! * reads newline-delimited frames of at most 8 MiB, answers an unparseable
//!   frame locally with a parse error, and stops after an oversized one,
//!   because the stream cannot be resynchronized after it; a frame larger than
//!   the daemon accepts is answered locally, without being sent;
//! * attaches again, once, only when the daemon has forgotten the attachment
//!   or nothing listens on the port any more (the daemon was restarted);
//! * detaches when the host closes stdin, and leaves the daemon running.
//!
//! With `--log <file>` it appends every frame it relays, in both directions,
//! to that file as JSON lines, so a host's conversation can be recorded.

use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::contact::{self, Daemon};
use crate::home;
use crate::http::{self, ATTACHMENT_HEADER, MCP_PATH};
use crate::keyring::AttachmentToken;
use crate::loopback::{self, Call};
use crate::mcp::{INTERNAL_ERROR, INVALID_PARAMS, PARSE_ERROR, rpc_error};

/// The largest frame a host may send.
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
/// How long the shim may spend finding or starting the daemon.
pub const STARTUP_BUDGET: Duration = Duration::from_secs(10);
/// How long one relayed request may take.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(120);
/// How many requests the shim relays at once; the next waits for the oldest.
pub const MAX_IN_FLIGHT: usize = 16;
/// The pause before the notice stream is opened again after it ended.
pub const NOTICE_RETRY: Duration = Duration::from_millis(250);

/// What the mind is told when the daemon is gone and attaching again failed.
const GONE: &str =
    "The Companion Lobby daemon did not answer. Restart this MCP server in the host.";

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

/// What the shim does with the daemon's answer to one relayed frame.
#[derive(Debug, PartialEq)]
pub enum Relayed {
    /// Write this frame back to the host.
    Answer(Value),
    /// Nothing goes back: the frame was a notification or a response.
    Nothing,
    /// The attachment or the daemon is gone; attach again and resend.
    Gone,
    /// Tell the mind this, without attaching again.
    Failed(i64, String),
}

/// Decides what an answer from the daemon means for the host. Only a
/// forgotten attachment (404) or a call that never connected leads to a new
/// attachment; every other failure is reported as it is.
pub fn classify(answer: io::Result<loopback::Answer>) -> Relayed {
    match answer {
        Ok(answer) => match answer.status {
            200 => match serde_json::from_slice(&answer.body) {
                Ok(frame) => Relayed::Answer(frame),
                Err(_) => Relayed::Failed(
                    INTERNAL_ERROR,
                    "The lobby sent an unreadable answer.".to_owned(),
                ),
            },
            202 => Relayed::Nothing,
            404 => Relayed::Gone,
            413 => Relayed::Failed(
                INVALID_PARAMS,
                "The message exceeds the lobby's 1 MiB limit.".to_owned(),
            ),
            503 => Relayed::Failed(
                INTERNAL_ERROR,
                "The lobby is busy; try the call again in a moment.".to_owned(),
            ),
            status => Relayed::Failed(
                INTERNAL_ERROR,
                format!("The lobby refused the message (HTTP {status})."),
            ),
        },
        Err(error) if loopback::unreachable(&error) => Relayed::Gone,
        Err(_) => Relayed::Failed(
            INTERNAL_ERROR,
            "The lobby did not finish answering; the call may or may not have happened.".to_owned(),
        ),
    }
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

/// One attachment in the daemon, as the shim knows it.
#[derive(Debug, Clone)]
struct Attached {
    port: u16,
    id: String,
    token: AttachmentToken,
}

/// The relay's link to its attachment in the daemon.
struct Link {
    companion: String,
    daemon: Mutex<Daemon>,
    attached: Mutex<Attached>,
    /// Set once the host closed stdin and the shim is detaching.
    closing: AtomicBool,
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Link {
    fn attach(daemon: Daemon, companion: &str) -> Result<Self, String> {
        let attached = attach_once(&daemon, companion)?;
        Ok(Self {
            companion: companion.to_owned(),
            daemon: Mutex::new(daemon),
            attached: Mutex::new(attached),
            closing: AtomicBool::new(false),
        })
    }

    fn current(&self) -> Attached {
        locked(&self.attached).clone()
    }

    /// Relays one message; `None` when there is nothing to send back.
    fn forward(&self, message: &Value) -> Option<Value> {
        let id = message.get("id").cloned();
        let body = serde_json::to_vec(message).unwrap_or_default();
        let failed = |code: i64, text: &str| id.as_ref().map(|id| rpc_error(id, code, text));
        if body.len() > http::BODY_LIMIT {
            return failed(
                INVALID_PARAMS,
                "The message exceeds the lobby's 1 MiB limit.",
            );
        }
        let mut attached_again = false;
        loop {
            let attached = self.current();
            let bearer = format!("Bearer {}", attached.token.expose());
            let call = Call {
                method: "POST",
                path: "/mcp",
                headers: &[
                    ("Authorization", &bearer),
                    (ATTACHMENT_HEADER, &attached.id),
                ],
                body: &body,
                timeout: CALL_TIMEOUT,
            };
            match classify(loopback::send(attached.port, &call)) {
                Relayed::Answer(frame) => return Some(frame),
                Relayed::Nothing => return None,
                Relayed::Failed(code, text) => return failed(code, &text),
                Relayed::Gone if attached_again => return failed(INTERNAL_ERROR, GONE),
                Relayed::Gone => {
                    if self.reattach(&attached).is_err() {
                        return failed(INTERNAL_ERROR, GONE);
                    }
                    attached_again = true;
                }
            }
        }
    }

    /// Attaches again after `lost` was found gone. Workers that find the same
    /// attachment gone at once produce one new attachment: the first does the
    /// work under the lock, the others see it replaced and use the new one.
    fn reattach(&self, lost: &Attached) -> Result<(), String> {
        let mut attached = locked(&self.attached);
        if attached.id != lost.id {
            return Ok(());
        }
        let (dir, port) = {
            let daemon = locked(&self.daemon);
            (daemon.dir.clone(), daemon.port)
        };
        let daemon = contact::ensure(&dir, port, STARTUP_BUDGET)?;
        let fresh = attach_once(&daemon, &self.companion)?;
        // Should the daemon still hold the old attachment, end it.
        detach(lost);
        *locked(&self.daemon) = daemon;
        *attached = fresh;
        Ok(())
    }

    fn detach(&self) {
        self.closing.store(true, Ordering::Release);
        detach(&self.current());
    }

    fn closing(&self) -> bool {
        self.closing.load(Ordering::Acquire)
    }
}

/// Opens the notice stream of `attached`, once it said it is open.
fn open_notices(attached: &Attached) -> Option<loopback::Lines> {
    let bearer = format!("Bearer {}", attached.token.expose());
    let call = Call {
        method: "GET",
        path: MCP_PATH,
        headers: &[
            ("Authorization", &bearer),
            (ATTACHMENT_HEADER, &attached.id),
            ("Accept", "text/event-stream"),
        ],
        body: &[],
        timeout: contact::PROBE_TIMEOUT,
    };
    let mut lines = loopback::open_stream(attached.port, &call).ok()?.ok()?;
    let opened = lines.next_line().ok()??;
    opened.starts_with(':').then_some(lines)
}

/// Writes every notification the attachment's notice stream carries to the
/// host, opening the stream again, for the attachment of the moment, when it
/// ends before the shim detaches.
fn relay_notices(
    link: &Link,
    mut stream: Option<loopback::Lines>,
    write: &(dyn Fn(&Value) -> bool + Send + Sync),
) {
    loop {
        if let Some(lines) = stream.as_mut() {
            while let Ok(Some(line)) = lines.next_line() {
                let Some(frame) = line
                    .strip_prefix("data:")
                    .and_then(|data| serde_json::from_str::<Value>(data.trim()).ok())
                else {
                    continue;
                };
                if link.closing() || !write(&frame) {
                    return;
                }
            }
        }
        if link.closing() {
            return;
        }
        std::thread::sleep(NOTICE_RETRY);
        if link.closing() {
            return;
        }
        stream = open_notices(&link.current());
    }
}

fn detach(attached: &Attached) {
    let bearer = format!("Bearer {}", attached.token.expose());
    let path = format!("/mcp/attach/{}", attached.id);
    let call = Call {
        method: "DELETE",
        path: &path,
        headers: &[("Authorization", &bearer)],
        body: &[],
        timeout: contact::PROBE_TIMEOUT,
    };
    let _ = loopback::send(attached.port, &call);
}

/// Attaches once as `companion`. This is the one call that carries the
/// install's attach secret: `daemon` was verified just before it, and the
/// secret is written only on a connection whose far end is the lock holder.
fn attach_once(daemon: &Daemon, companion: &str) -> Result<Attached, String> {
    let bearer = format!("Bearer {}", daemon.secret.expose());
    let body = serde_json::to_vec(&json!({ "companion": companion })).unwrap_or_default();
    let call = Call {
        method: "POST",
        path: "/mcp/attach",
        headers: &[("Authorization", &bearer)],
        body: &body,
        timeout: contact::PROBE_TIMEOUT,
    };
    let holder = daemon.discovery.lock_holder_pid;
    let answer = loopback::send_checked(daemon.port, &call, |stream| {
        contact::held_by(stream, daemon.port, holder)
    })
    .map_err(|error| match error.kind() {
        io::ErrorKind::PermissionDenied => format!(
            "refusing the process on 127.0.0.1:{}: {error}; the attach secret was not sent",
            daemon.port
        ),
        _ => format!("cannot reach the daemon to attach: {error}"),
    })?;
    let value: Value = serde_json::from_slice(&answer.body).unwrap_or(Value::Null);
    if answer.status != 201 {
        let message = value
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("the daemon refused the attachment");
        return Err(format!("cannot attach as {companion}: {message}"));
    }
    let id = value
        .get("attachment")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "the daemon's attach answer named no attachment".to_owned())?;
    let token = value
        .get("token")
        .and_then(Value::as_str)
        .and_then(AttachmentToken::parse)
        .ok_or_else(|| "the daemon's attach answer carried no attachment token".to_owned())?;
    Ok(Attached {
        port: daemon.port,
        id,
        token,
    })
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
            let mut stdout = locked(&stdout);
            stdout
                .write_all(line.as_bytes())
                .and_then(|()| stdout.flush())
                .is_ok()
        }
    };
    let write = Arc::new(write);
    // The notice stream is open before the host's first frame is read, so no
    // change after the host's first answer goes unannounced.
    let first = open_notices(&link.current());
    {
        let link = link.clone();
        let write = write.clone();
        std::thread::spawn(move || relay_notices(&link, first, &*write));
    }
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
        if message.get("method").is_some() && message.get("id").is_none() {
            // A notification reaches the daemon in arrival order, before any
            // request read after it.
            if let Some(answer) = link.forward(&message) {
                write(&answer);
            }
            continue;
        }
        workers.retain(|worker| !worker.is_finished());
        if workers.len() >= MAX_IN_FLIGHT {
            let _ = workers.remove(0).join();
        }
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

    fn answer(status: u16, body: &str) -> io::Result<loopback::Answer> {
        Ok(loopback::Answer {
            status,
            body: body.as_bytes().to_vec(),
        })
    }

    #[test]
    fn only_a_forgotten_attachment_or_an_unreachable_daemon_attaches_again() {
        assert_eq!(
            classify(answer(200, r#"{"id":1}"#)),
            Relayed::Answer(json!({"id": 1}))
        );
        assert_eq!(classify(answer(202, "")), Relayed::Nothing);
        assert_eq!(classify(answer(404, "{}")), Relayed::Gone);
        let unreachable = loopback::send(
            {
                let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
                listener.local_addr().expect("address").port()
            },
            &Call::get("/", Duration::from_secs(5)),
        );
        assert_eq!(classify(unreachable), Relayed::Gone);

        assert!(matches!(
            classify(answer(413, "{}")),
            Relayed::Failed(INVALID_PARAMS, _)
        ));
        for status in [400, 401, 415, 500, 503] {
            assert!(
                matches!(
                    classify(answer(status, "{}")),
                    Relayed::Failed(INTERNAL_ERROR, _)
                ),
                "{status}"
            );
        }
        assert!(matches!(
            classify(answer(200, "not json")),
            Relayed::Failed(INTERNAL_ERROR, _)
        ));
        // A call that connected and then failed may have happened; it is not resent.
        assert!(matches!(
            classify(Err(io::Error::new(io::ErrorKind::TimedOut, "slow"))),
            Relayed::Failed(INTERNAL_ERROR, _)
        ));
    }
}
