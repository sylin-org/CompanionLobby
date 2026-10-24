//! The client side of the loopback port.
//!
//! `attach`, `open` and `stop` talk to the daemon over 127.0.0.1 with this
//! small blocking HTTP/1.1 client: one request per connection, no redirects
//! followed, no proxy, a bounded response. [`open_stream`] reads a response
//! that never ends, such as an attachment's notice stream, line by line. It
//! never leaves the loopback address; adapters that reach the network use the
//! outbound agent instead.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

/// The largest response read.
pub const RESPONSE_LIMIT: usize = 16 * 1024 * 1024;

/// A response from the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    /// The HTTP status.
    pub status: u16,
    /// The body bytes.
    pub body: Vec<u8>,
}

/// One request to 127.0.0.1:`port`.
#[derive(Debug, Clone)]
pub struct Call<'a> {
    /// The method, such as `GET`.
    pub method: &'a str,
    /// The path and query.
    pub path: &'a str,
    /// Extra headers.
    pub headers: &'a [(&'a str, &'a str)],
    /// The body; a non-empty body is sent as JSON.
    pub body: &'a [u8],
    /// How long to wait for the connection and for each read.
    pub timeout: Duration,
}

impl<'a> Call<'a> {
    /// A body-less GET.
    pub fn get(path: &'a str, timeout: Duration) -> Self {
        Self {
            method: "GET",
            path,
            headers: &[],
            body: &[],
            timeout,
        }
    }
}

/// The error behind a call that never reached a listener: nothing was sent,
/// so the call can be made again without doing anything twice.
#[derive(Debug)]
pub struct Unreachable(io::Error);

impl std::fmt::Display for Unreachable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "nothing accepted the connection: {}", self.0)
    }
}

impl std::error::Error for Unreachable {}

/// Whether `error` came from a call that never connected.
pub fn unreachable(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|inner| inner.is::<Unreachable>())
}

/// Sends `call` to the daemon on `port` and reads the whole answer.
pub fn send(port: u16, call: &Call<'_>) -> io::Result<Answer> {
    send_checked(port, call, |_| Ok(()))
}

/// The longest line a stream may send.
pub const STREAM_LINE_LIMIT: usize = 64 * 1024;
/// How long a stream may stay silent; the daemon sends a keepalive well within it.
pub const STREAM_SILENCE: Duration = Duration::from_secs(60);

/// The lines of a response body that arrives over time.
pub struct Lines {
    reader: BufReader<TcpStream>,
    chunked: bool,
    /// Bytes left in the current chunk of a chunked body.
    left: usize,
    ended: bool,
    pending: Vec<u8>,
}

impl Lines {
    /// The next line, without its line ending; `None` once the body ended.
    pub fn next_line(&mut self) -> io::Result<Option<String>> {
        loop {
            if let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
                let mut line: Vec<u8> = self.pending.drain(..=end).collect();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Ok(Some(String::from_utf8_lossy(&line).into_owned()));
            }
            if self.pending.len() > STREAM_LINE_LIMIT {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "a line is too long",
                ));
            }
            if self.ended {
                return Ok(None);
            }
            self.fill()?;
        }
    }

    /// Reads more of the body into `pending`.
    fn fill(&mut self) -> io::Result<()> {
        if !self.chunked {
            let mut buffer = [0u8; 4096];
            let read = self.reader.read(&mut buffer)?;
            if read == 0 {
                self.ended = true;
            }
            self.pending.extend_from_slice(&buffer[..read]);
            return Ok(());
        }
        if self.left == 0 {
            let mut size = String::new();
            if self.reader.read_line(&mut size)? == 0 {
                self.ended = true;
                return Ok(());
            }
            let size = size.trim();
            if size.is_empty() {
                // The line ending after the previous chunk.
                return Ok(());
            }
            let digits = size.split(';').next().unwrap_or_default();
            self.left = usize::from_str_radix(digits, 16)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "a bad chunk size"))?;
            if self.left == 0 {
                self.ended = true;
            }
            return Ok(());
        }
        let mut buffer = vec![0u8; self.left.min(4096)];
        let read = self.reader.read(&mut buffer)?;
        if read == 0 {
            self.ended = true;
        }
        self.left -= read;
        self.pending.extend_from_slice(&buffer[..read]);
        Ok(())
    }
}

/// Sends `call` and, when the daemon answers 200, hands back the body's
/// lines as they arrive; any other status is returned as it is.
pub fn open_stream(port: u16, call: &Call<'_>) -> io::Result<Result<Lines, u16>> {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&address, call.timeout)
        .map_err(|error| io::Error::new(error.kind(), Unreachable(error)))?;
    stream.set_write_timeout(Some(call.timeout))?;
    stream.set_read_timeout(Some(STREAM_SILENCE))?;
    stream.set_nodelay(true)?;
    let mut head = format!(
        "{} {} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n",
        call.method, call.path
    );
    for (name, value) in call.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.flush()?;
    let mut reader = BufReader::new(stream);
    let invalid = |what: &str| io::Error::new(io::ErrorKind::InvalidData, what.to_owned());
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let status = line
        .strip_prefix("HTTP/1.1 ")
        .and_then(|rest| rest.get(..3))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| invalid("the answer has no status"))?;
    let mut chunked = false;
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Err(invalid("the answer has no complete head"));
        }
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.trim().eq_ignore_ascii_case("transfer-encoding")
        {
            chunked = value.trim().eq_ignore_ascii_case("chunked");
        }
    }
    if status != 200 {
        return Ok(Err(status));
    }
    Ok(Ok(Lines {
        reader,
        chunked,
        left: 0,
        ended: false,
        pending: Vec::new(),
    }))
}

/// Sends `call` like [`send`], but only after `check` accepted the connection
/// it is about to travel on; when `check` refuses, nothing is written.
pub fn send_checked(
    port: u16,
    call: &Call<'_>,
    check: impl FnOnce(&TcpStream) -> io::Result<()>,
) -> io::Result<Answer> {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&address, call.timeout)
        .map_err(|error| io::Error::new(error.kind(), Unreachable(error)))?;
    check(&stream)?;
    stream.set_read_timeout(Some(call.timeout))?;
    stream.set_write_timeout(Some(call.timeout))?;
    stream.set_nodelay(true)?;
    let mut head = format!(
        "{} {} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nContent-Length: {}\r\n",
        call.method,
        call.path,
        call.body.len()
    );
    if !call.body.is_empty() {
        head.push_str("Content-Type: application/json\r\n");
    }
    for (name, value) in call.headers {
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(call.body)?;
    stream.flush()?;
    let mut raw = Vec::new();
    stream
        .take(RESPONSE_LIMIT as u64 + 1)
        .read_to_end(&mut raw)?;
    if raw.len() > RESPONSE_LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the daemon's answer is too large",
        ));
    }
    parse(&raw)
}

/// Parses a complete HTTP/1.1 response with a `Content-Length` or no body.
pub fn parse(raw: &[u8]) -> io::Result<Answer> {
    let invalid = |what: &str| io::Error::new(io::ErrorKind::InvalidData, what.to_owned());
    let end = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| invalid("the answer has no complete head"))?;
    let head =
        std::str::from_utf8(&raw[..end]).map_err(|_| invalid("the answer's head is not text"))?;
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.strip_prefix("HTTP/1.1 "))
        .and_then(|rest| rest.get(..3))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| invalid("the answer has no status"))?;
    let mut length = None;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                length = value.trim().parse::<usize>().ok();
            }
            if name.trim().eq_ignore_ascii_case("transfer-encoding") {
                return Err(invalid(
                    "the answer uses a transfer encoding this client does not read",
                ));
            }
        }
    }
    let rest = &raw[end + 4..];
    let body = match length {
        Some(length) if length <= rest.len() => rest[..length].to_vec(),
        Some(_) => return Err(invalid("the answer ended early")),
        None => rest.to_vec(),
    };
    Ok(Answer { status, body })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_response_with_a_length_is_read_exactly() {
        let answer = parse(b"HTTP/1.1 201 Created\r\nContent-Length: 2\r\nX: y\r\n\r\n{}trailing")
            .expect("parse");
        assert_eq!(
            answer,
            Answer {
                status: 201,
                body: b"{}".to_vec()
            }
        );
        let empty = parse(b"HTTP/1.1 204 No Content\r\n\r\n").expect("parse");
        assert_eq!(empty.status, 204);
        assert!(empty.body.is_empty());
    }

    #[test]
    fn a_call_to_a_closed_port_is_unreachable_and_other_failures_are_not() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("address").port();
        drop(listener);
        let error = send(port, &Call::get("/", Duration::from_secs(5))).expect_err("closed");
        assert!(unreachable(&error), "{error}");
        assert!(!unreachable(&io::Error::other("read failed")));
    }

    #[test]
    fn a_chunked_stream_is_read_line_by_line() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("address").port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut head = [0u8; 1024];
            let _ = stream.read(&mut head).expect("read");
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n8\r\n: open\n\n\r\n",
                )
                .expect("write");
            stream
                .write_all(b"9\r\ndata: {}\n\r\n1\r\n\n\r\n0\r\n\r\n")
                .expect("write");
        });
        let mut lines = open_stream(port, &Call::get("/mcp", Duration::from_secs(5)))
            .expect("open")
            .expect("200");
        let mut seen = Vec::new();
        while let Some(line) = lines.next_line().expect("line") {
            seen.push(line);
        }
        assert_eq!(seen, [": open", "", "data: {}", ""]);
        server.join().expect("server");
    }

    #[test]
    fn malformed_or_truncated_answers_are_refused() {
        assert!(parse(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nshort").is_err());
        assert!(parse(b"garbage").is_err());
        assert!(parse(b"HTTP/1.0 200 OK\r\n\r\n").is_err());
        assert!(parse(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n").is_err());
    }
}
