//! The stdio peer: a test standing where an MCP host stands.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

/// How long a peer waits for any one answer.
pub const ANSWER_TIMEOUT: Duration = Duration::from_secs(20);

/// A spawned stdio MCP server driven line by line.
///
/// Every stdout line is kept, so a test can assert that stdout carried
/// nothing but JSON-RPC. The child is killed and reaped on drop.
pub struct Peer {
    child: Child,
    lines: Receiver<String>,
    stderr: Receiver<String>,
    seen: Vec<String>,
    stdout_closed: bool,
}

impl Peer {
    /// Spawns `program` with `arguments` and the given extra environment.
    pub fn spawn(program: &Path, arguments: &[&str], environment: &[(&str, &str)]) -> Self {
        let mut command = Command::new(program);
        command
            .args(arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (name, value) in environment {
            command.env(name, value);
        }
        let mut child = command.spawn().expect("spawn the stdio server");
        let lines = forward_lines(child.stdout.take().expect("stdout"));
        let stderr = forward_lines(child.stderr.take().expect("stderr"));
        Self {
            child,
            lines,
            stderr,
            seen: Vec::new(),
            stdout_closed: false,
        }
    }

    /// The child's process id.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// The child's process handle, for placing it in a job object.
    #[cfg(windows)]
    pub fn child_handle(&self) -> std::os::windows::io::RawHandle {
        use std::os::windows::io::AsRawHandle;
        self.child.as_raw_handle()
    }

    /// Sends one frame.
    pub fn send(&mut self, frame: &Value) {
        let mut line = serde_json::to_string(frame).expect("encode a frame");
        line.push('\n');
        self.send_raw(&line);
    }

    /// Sends raw text, as a misbehaving host would.
    pub fn send_raw(&mut self, text: &str) {
        let stdin = self.child.stdin.as_mut().expect("stdin is open");
        stdin.write_all(text.as_bytes()).expect("write to stdin");
        stdin.flush().expect("flush stdin");
    }

    /// Closes stdin, as a host does when it is done.
    pub fn close_stdin(&mut self) {
        drop(self.child.stdin.take());
    }

    /// The next stdout frame, whatever it is.
    pub fn next_frame(&mut self) -> Value {
        let line = self
            .lines
            .recv_timeout(ANSWER_TIMEOUT)
            .expect("a frame on stdout");
        self.seen.push(line.clone());
        serde_json::from_str(&line)
            .unwrap_or_else(|_| panic!("stdout carried a non-JSON line: {line}"))
    }

    /// The answer whose id equals `id`; other frames are kept but skipped.
    pub fn answer(&mut self, id: &Value) -> Value {
        loop {
            let frame = self.next_frame();
            if frame.get("id") == Some(id) {
                return frame;
            }
        }
    }

    /// Sends a request and waits for its answer.
    pub fn ask(&mut self, frame: &Value) -> Value {
        self.send(frame);
        let id = frame.get("id").cloned().expect("a request has an id");
        self.answer(&id)
    }

    /// Whether stdout reaches its end within `limit`. Lines that arrive first are kept.
    pub fn stdout_ends_within(&mut self, limit: Duration) -> bool {
        if self.stdout_closed {
            return true;
        }
        let deadline = Instant::now() + limit;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(line) => self.seen.push(line),
                Err(RecvTimeoutError::Timeout) => return false,
                // The reader thread ended: stdout reached its end.
                Err(_) => {
                    self.stdout_closed = true;
                    return true;
                }
            }
        }
    }

    /// Waits for the child to exit.
    pub fn wait_exit(&mut self, limit: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + limit;
        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                return Some(status);
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Every stdout line seen so far.
    pub fn stdout_lines(&self) -> &[String] {
        &self.seen
    }

    /// Everything the child has written to stderr so far.
    pub fn stderr_text(&self) -> String {
        let mut text = String::new();
        while let Ok(line) = self.stderr.recv_timeout(Duration::from_millis(300)) {
            text.push_str(&line);
            text.push('\n');
        }
        text
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn forward_lines(stream: impl std::io::Read + Send + 'static) -> Receiver<String> {
    let (sender, receiver) = channel();
    thread::spawn(move || {
        for line in BufReader::new(stream).lines() {
            match line {
                Ok(line) => {
                    if sender.send(line).is_err() {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
    });
    receiver
}
