//! A server-sent events client, standing where the manager page's
//! `EventSource` stands.
//!
//! It opens one stream to a loopback port, reads the response whether it is
//! chunked or not, and hands over each event (its `id` and its `data`) as it
//! arrives. Comments and `retry` lines are read and dropped, as a browser
//! does.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::mpsc::{Receiver, channel};
use std::thread;
use std::time::Duration;

/// How long a stream waits for any one event.
pub const EVENT_TIMEOUT: Duration = Duration::from_secs(20);

/// One server-sent event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// Its `id:` line, if it had one.
    pub id: Option<String>,
    /// Its `data:` lines, joined by newlines.
    pub data: String,
}

/// An open event stream.
pub struct EventStream {
    events: Receiver<Event>,
    /// Everything the stream carried, head and body, for tests that search it.
    raw: Receiver<Vec<u8>>,
    seen: Vec<u8>,
}

impl EventStream {
    /// Opens `path` on 127.0.0.1:`port`, sending `last_event_id` when given.
    /// Panics unless the answer is 200 with an event stream.
    pub fn open(port: u16, path: &str, last_event_id: Option<&str>) -> Self {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        let resume = last_event_id
            .map(|id| format!("Last-Event-ID: {id}\r\n"))
            .unwrap_or_default();
        stream
            .write_all(
                format!(
                    "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: text/event-stream\r\n{resume}\r\n"
                )
                .as_bytes(),
            )
            .expect("send the request");
        let mut reader = BufReader::new(stream);
        let mut head = String::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).expect("read the head") == 0 {
                panic!("the stream ended before its head: {head}");
            }
            head.push_str(&line);
            if line == "\r\n" {
                break;
            }
        }
        let lowered = head.to_ascii_lowercase();
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        assert!(
            lowered.contains("content-type: text/event-stream"),
            "{head}"
        );
        let chunked = lowered.contains("transfer-encoding: chunked");
        let (events_sender, events) = channel();
        let (raw_sender, raw) = channel();
        let _ = raw_sender.send(head.into_bytes());
        thread::spawn(move || {
            let mut parser = Parser::default();
            loop {
                let bytes = if chunked {
                    read_chunk(&mut reader)
                } else {
                    let mut buffer = vec![0u8; 4096];
                    match reader.read(&mut buffer) {
                        Ok(0) | Err(_) => None,
                        Ok(read) => {
                            buffer.truncate(read);
                            Some(buffer)
                        }
                    }
                };
                let Some(bytes) = bytes else {
                    return;
                };
                let _ = raw_sender.send(bytes.clone());
                for event in parser.feed(&bytes) {
                    if events_sender.send(event).is_err() {
                        return;
                    }
                }
            }
        });
        Self {
            events,
            raw,
            seen: Vec::new(),
        }
    }

    /// The next event, or `None` when none arrives in time or the stream ended.
    pub fn next_event(&mut self) -> Option<Event> {
        self.next_within(EVENT_TIMEOUT)
    }

    /// The next event within `timeout`.
    pub fn next_within(&mut self, timeout: Duration) -> Option<Event> {
        self.events.recv_timeout(timeout).ok()
    }

    /// Every byte the stream carried so far.
    pub fn bytes(&mut self) -> &[u8] {
        self.seen.extend(self.raw.try_iter().flatten());
        &self.seen
    }
}

/// One chunk of a chunked body; `None` at its end.
fn read_chunk(reader: &mut impl BufRead) -> Option<Vec<u8>> {
    let mut size = String::new();
    if reader.read_line(&mut size).ok()? == 0 {
        return None;
    }
    let size = usize::from_str_radix(size.trim().split(';').next()?.trim(), 16).ok()?;
    if size == 0 {
        return None;
    }
    let mut chunk = vec![0u8; size + 2];
    reader.read_exact(&mut chunk).ok()?;
    chunk.truncate(size);
    Some(chunk)
}

#[derive(Default)]
struct Parser {
    pending: Vec<u8>,
    id: Option<String>,
    data: Vec<String>,
}

impl Parser {
    fn feed(&mut self, bytes: &[u8]) -> Vec<Event> {
        self.pending.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                if !self.data.is_empty() {
                    events.push(Event {
                        id: self.id.take(),
                        data: self.data.join("\n"),
                    });
                }
                self.id = None;
                self.data.clear();
            } else if line.starts_with(':') {
                // A comment, such as a keepalive.
            } else {
                let (field, value) = line.split_once(':').unwrap_or((line, ""));
                let value = value.strip_prefix(' ').unwrap_or(value);
                match field {
                    "id" => self.id = Some(value.to_owned()),
                    "data" => self.data.push(value.to_owned()),
                    _ => {}
                }
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_are_split_on_blank_lines_and_comments_are_dropped() {
        let mut parser = Parser::default();
        let mut events = parser.feed(b"retry: 2000\n\n: keepalive\n\nid: e:1\ndata: {\"a\":1}\n");
        assert!(events.is_empty(), "the event is not over yet");
        events.extend(parser.feed(b"\ndata: one\ndata: two\n\n"));
        assert_eq!(
            events,
            vec![
                Event {
                    id: Some("e:1".to_owned()),
                    data: "{\"a\":1}".to_owned()
                },
                Event {
                    id: None,
                    data: "one\ntwo".to_owned()
                },
            ]
        );
    }

    #[test]
    fn a_chunked_body_is_read_chunk_by_chunk() {
        let mut body = std::io::Cursor::new(b"5\r\nhello\r\n3;x=y\r\nabc\r\n0\r\n\r\n".to_vec());
        assert_eq!(read_chunk(&mut body), Some(b"hello".to_vec()));
        assert_eq!(read_chunk(&mut body), Some(b"abc".to_vec()));
        assert_eq!(read_chunk(&mut body), None);
    }
}
