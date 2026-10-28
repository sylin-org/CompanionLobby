//! The one vetted outbound HTTP agent.
//!
//! Every adapter that talks to the network sends its requests through an
//! [`Outbound`], so the properties that stop server-side request forgery
//! cannot be dropped by accident:
//!
//! * the target is an [`Address`] that already passed the one origin parser,
//!   and every address its host resolves to is checked against the adapter's
//!   [`AddressPolicy`] inside the resolver, once, so the connection goes to an
//!   address that was checked and nothing can point the name elsewhere in between;
//! * no redirect is followed and no proxy is taken from the environment;
//! * a connection has 10 seconds to open and every read and write 30, and
//!   the whole request, from opening the connection to the last byte of the
//!   body, has [`DEADLINE`] unless the request names a shorter one; resolving
//!   a name is left to the system resolver and is not counted against it;
//! * a body is read up to a limit the request names
//!   ([`DEFAULT_LIMIT`] unless a route needs more), and a longer one is
//!   refused rather than cut;
//! * the status, the headers and the body come back as they are, whatever the
//!   status, so each adapter maps them to outcomes in its own words.
//!
//! It is synchronous: the daemon calls adapters on blocking threads. HTTPS is
//! spoken with rustls and verified against the Mozilla root certificates the
//! agent carries, never the machine's own store, so nothing installed locally
//! widens what it trusts; plain HTTP is spoken only to this machine, the one
//! place [`AddressPolicy`] lets it reach. A body is sent as JSON, as a form,
//! or as bytes of a declared type.

use std::io::{self, Read};
use std::net::{SocketAddr, ToSocketAddrs};
use std::time::Duration;

use lobby_contracts::{Address, AddressPolicy};

/// How long a connection may take to open.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long any one read or write may take.
pub const IO_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a whole request may take, from opening the connection to reading
/// the last byte of the body, when the request names no shorter deadline.
pub const DEADLINE: Duration = Duration::from_secs(60);
/// The longest body read when a request names no other limit.
pub const DEFAULT_LIMIT: usize = 256 * 1024;

/// One request to an address.
#[derive(Debug, Clone)]
pub struct Request<'a> {
    /// The method, such as `GET`.
    pub method: &'a str,
    /// The path and query, starting with `/`.
    pub path: &'a str,
    /// Extra headers.
    pub headers: Vec<(&'a str, String)>,
    /// A body and its content type.
    pub body: Option<(&'static str, Vec<u8>)>,
    /// The longest response body accepted.
    pub limit: usize,
    /// How long the whole request may take.
    pub deadline: Duration,
}

impl<'a> Request<'a> {
    /// A body-less request with the default limit.
    pub fn new(method: &'a str, path: &'a str) -> Self {
        Self {
            method,
            path,
            headers: Vec::new(),
            body: None,
            limit: DEFAULT_LIMIT,
            deadline: DEADLINE,
        }
    }

    /// Adds a header.
    #[must_use]
    pub fn header(mut self, name: &'a str, value: impl Into<String>) -> Self {
        self.headers.push((name, value.into()));
        self
    }

    /// Sends `body`, already encoded, as JSON.
    #[must_use]
    pub fn json(mut self, body: Vec<u8>) -> Self {
        self.body = Some(("application/json", body));
        self
    }

    /// Sends `body` as it is, declared as `content_type`, such as an image
    /// a service keeps as a file.
    #[must_use]
    pub fn bytes(mut self, content_type: &'static str, body: Vec<u8>) -> Self {
        self.body = Some((content_type, body));
        self
    }

    /// Sends `fields` as an `application/x-www-form-urlencoded` body.
    #[must_use]
    pub fn form(mut self, fields: &[(&str, &str)]) -> Self {
        let encoded = fields
            .iter()
            .map(|(name, value)| format!("{}={}", form_encode(name), form_encode(value)))
            .collect::<Vec<_>>()
            .join("&");
        self.body = Some(("application/x-www-form-urlencoded", encoded.into_bytes()));
        self
    }

    /// Accepts a response body of up to `limit` bytes.
    #[must_use]
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    /// Gives the whole request `deadline`, never more than [`DEADLINE`].
    #[must_use]
    pub fn within(mut self, deadline: Duration) -> Self {
        self.deadline = deadline.min(DEADLINE);
        self
    }
}

/// A response, whatever its status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// The HTTP status.
    pub status: u16,
    /// The headers, names lowercased, in the order they arrived.
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: Vec<u8>,
}

impl Response {
    /// The first value of the header `name`, compared without case.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// Whether the body is declared as JSON.
    pub fn is_json(&self) -> bool {
        self.header("content-type")
            .and_then(|value| value.split(';').next())
            .is_some_and(|media| media.trim().eq_ignore_ascii_case("application/json"))
    }
}

/// Why a request got no response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// The address, or what its name resolved to, is not one the policy
    /// allows, or the agent does not speak its scheme. Nothing was sent.
    Refused,
    /// No connection opened. Nothing was sent, so the request may be made
    /// again without doing anything twice.
    Unreachable,
    /// The connection opened and then failed, or the answer could not be
    /// read or was larger than the limit. The service may have acted on the
    /// request.
    Interrupted,
}

/// The agent one adapter uses, bound to that adapter's address policy.
#[derive(Debug)]
pub struct Outbound {
    agent: ureq::Agent,
}

impl Outbound {
    /// An agent that reaches only what `policy` allows.
    pub fn new(policy: AddressPolicy) -> Self {
        let agent = ureq::AgentBuilder::new()
            .redirects(0)
            .try_proxy_from_env(false)
            .timeout_connect(CONNECT_TIMEOUT)
            .timeout_read(IO_TIMEOUT)
            .timeout_write(IO_TIMEOUT)
            .timeout(DEADLINE)
            .resolver(PolicyResolver { policy })
            .build();
        Self { agent }
    }

    /// Sends `request` to `address` and reads the whole response.
    pub fn send(&self, address: &Address, request: Request<'_>) -> Result<Response, Failure> {
        if !request.path.starts_with('/') {
            return Err(Failure::Refused);
        }
        let mut call = self
            .agent
            .request(request.method, &address.url(request.path))
            .timeout(request.deadline);
        for (name, value) in &request.headers {
            call = call.set(name, value);
        }
        let answered = match &request.body {
            Some((content_type, body)) => call.set("Content-Type", content_type).send_bytes(body),
            None => call.call(),
        };
        let response = match answered {
            Ok(response) => response,
            Err(ureq::Error::Status(_, response)) => response,
            Err(ureq::Error::Transport(transport)) => {
                return Err(match transport.kind() {
                    ureq::ErrorKind::InvalidUrl | ureq::ErrorKind::UnknownScheme => {
                        Failure::Refused
                    }
                    ureq::ErrorKind::Dns => {
                        // The policy resolver refuses with PermissionDenied.
                        if refused_by_policy(&transport) {
                            Failure::Refused
                        } else {
                            Failure::Unreachable
                        }
                    }
                    ureq::ErrorKind::ConnectionFailed => Failure::Unreachable,
                    _ => Failure::Interrupted,
                });
            }
        };
        let status = response.status();
        let headers = response
            .headers_names()
            .into_iter()
            .filter_map(|name| {
                let value = response.header(&name)?.to_owned();
                Some((name.to_ascii_lowercase(), value))
            })
            .collect();
        let mut body = Vec::new();
        response
            .into_reader()
            .take(request.limit as u64 + 1)
            .read_to_end(&mut body)
            .map_err(|_| Failure::Interrupted)?;
        if body.len() > request.limit {
            return Err(Failure::Interrupted);
        }
        Ok(Response {
            status,
            headers,
            body,
        })
    }
}

/// Percent-encodes everything but the RFC 3986 unreserved characters, as one
/// form field name or value.
fn form_encode(text: &str) -> String {
    text.bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

fn refused_by_policy(transport: &ureq::Transport) -> bool {
    std::error::Error::source(transport)
        .and_then(|source| source.downcast_ref::<io::Error>())
        .is_some_and(|error| error.kind() == io::ErrorKind::PermissionDenied)
}

/// Resolves a name once and connects only to addresses the policy
/// allows; a name with any other address is refused whole.
#[derive(Debug, Clone, Copy)]
struct PolicyResolver {
    policy: AddressPolicy,
}

impl ureq::Resolver for PolicyResolver {
    fn resolve(&self, netloc: &str) -> io::Result<Vec<SocketAddr>> {
        let resolved: Vec<SocketAddr> = netloc.to_socket_addrs()?.collect();
        if resolved.is_empty()
            || resolved
                .iter()
                .any(|address| !self.policy.permits(address.ip()))
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the name resolves to an address this adapter may not reach",
            ));
        }
        Ok(resolved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    fn loopback(port: u16) -> Address {
        Address::parse(
            &format!("http://127.0.0.1:{port}"),
            AddressPolicy::LoopbackAllowed,
        )
        .expect("loopback")
    }

    /// Serves one connection on a loopback port with `answer`, returning the
    /// address and the request head it received.
    fn serve_once(answer: &'static [u8]) -> (Address, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("address").port();
        let handle = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut head = String::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                head.push_str(&line);
            }
            let mut stream = stream;
            stream.write_all(answer).expect("answer");
            head
        });
        (loopback(port), handle)
    }

    #[test]
    fn any_status_comes_back_with_its_body_and_headers() {
        let (address, server) = serve_once(
            b"HTTP/1.1 409 Conflict\r\nContent-Type: application/json\r\nX-Board: one\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
        );
        let outbound = Outbound::new(AddressPolicy::LoopbackAllowed);
        let response = outbound
            .send(
                &address,
                Request::new("GET", "/posts").header("Authorization", "Badge b"),
            )
            .expect("a response");
        assert_eq!(response.status, 409);
        assert_eq!(response.body, b"{}");
        assert_eq!(response.header("x-board"), Some("one"));
        assert!(response.is_json());
        let head = server.join().expect("server");
        assert!(head.starts_with("GET /posts HTTP/1.1"), "{head}");
        assert!(head.contains("Authorization: Badge b"), "{head}");
    }

    #[test]
    fn a_redirect_is_returned_never_followed() {
        let (address, server) = serve_once(
            b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/elsewhere\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        let response = Outbound::new(AddressPolicy::LoopbackAllowed)
            .send(&address, Request::new("GET", "/"))
            .expect("a response");
        assert_eq!(response.status, 302);
        server.join().expect("server");
    }

    #[test]
    fn a_body_over_the_limit_is_refused_whole() {
        let (address, server) = serve_once(
            b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\n0123456789",
        );
        let failure = Outbound::new(AddressPolicy::LoopbackAllowed)
            .send(&address, Request::new("GET", "/").limit(9))
            .expect_err("too long");
        assert_eq!(failure, Failure::Interrupted);
        server.join().expect("server");
    }

    /// Reads one request head off `stream`, up to the blank line that ends it.
    fn read_head(stream: &std::net::TcpStream) -> String {
        let mut reader = BufReader::new(stream.try_clone().expect("clone"));
        let mut head = String::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                return head;
            }
            head.push_str(&line);
        }
    }

    #[test]
    fn a_body_that_trickles_past_the_deadline_is_refused() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("address").port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let head = read_head(&stream);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 40\r\nConnection: close\r\n\r\n")
                .expect("the head");
            // Each byte arrives well within the read timeout; the whole body
            // takes far longer than the request's deadline.
            let mut sent = 0;
            for _ in 0..40 {
                if stream
                    .write_all(b"x")
                    .and_then(|()| stream.flush())
                    .is_err()
                {
                    break;
                }
                sent += 1;
                std::thread::sleep(Duration::from_millis(100));
            }
            (head, sent)
        });
        let started = std::time::Instant::now();
        let failure = Outbound::new(AddressPolicy::LoopbackAllowed)
            .send(
                &loopback(port),
                Request::new("GET", "/").within(Duration::from_millis(500)),
            )
            .expect_err("past the deadline");
        let took = started.elapsed();
        assert_eq!(failure, Failure::Interrupted);
        assert!(
            took >= Duration::from_millis(400) && took < Duration::from_secs(3),
            "the deadline, not a malformed answer, ended the request: {took:?}"
        );
        let (head, sent) = server.join().expect("server");
        assert!(head.starts_with("GET / HTTP/1.1\r\n"), "{head}");
        assert!(
            (1..40).contains(&sent),
            "the body had started and was cut short: {sent} bytes"
        );
    }

    #[test]
    fn an_answer_that_never_starts_ends_at_the_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("address").port();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let head = read_head(&stream);
            // The request was read whole; no answer ever comes.
            std::thread::sleep(Duration::from_secs(3));
            drop(stream);
            head
        });
        let started = std::time::Instant::now();
        let failure = Outbound::new(AddressPolicy::LoopbackAllowed)
            .send(
                &loopback(port),
                Request::new("GET", "/slow").within(Duration::from_millis(500)),
            )
            .expect_err("no answer within the deadline");
        let took = started.elapsed();
        assert_eq!(failure, Failure::Interrupted);
        assert!(
            took >= Duration::from_millis(400) && took < Duration::from_secs(2),
            "the deadline ended the wait for the head: {took:?}"
        );
        assert!(
            server
                .join()
                .expect("server")
                .starts_with("GET /slow HTTP/1.1\r\n")
        );
    }

    #[test]
    fn a_deadline_is_never_longer_than_the_agents() {
        let request = Request::new("GET", "/").within(Duration::from_secs(3600));
        assert_eq!(request.deadline, DEADLINE);
        assert_eq!(Request::new("GET", "/").deadline, DEADLINE);
    }

    #[test]
    fn a_closed_port_is_unreachable() {
        let port = {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            listener.local_addr().expect("address").port()
        };
        let failure = Outbound::new(AddressPolicy::LoopbackAllowed)
            .send(&loopback(port), Request::new("GET", "/"))
            .expect_err("closed");
        assert_eq!(failure, Failure::Unreachable);
    }

    #[test]
    fn the_policy_is_applied_to_what_a_name_resolves_to() {
        let address = Address::parse("http://localhost:9", AddressPolicy::LoopbackAllowed)
            .expect("a loopback name");
        let failure = Outbound::new(AddressPolicy::PublicHttps)
            .send(&address, Request::new("GET", "/"))
            .expect_err("loopback is not public");
        assert_eq!(failure, Failure::Refused);
        let public = PolicyResolver {
            policy: AddressPolicy::PublicHttps,
        };
        let refused = ureq::Resolver::resolve(&public, "127.0.0.1:80").expect_err("refused");
        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
        let loopback = PolicyResolver {
            policy: AddressPolicy::LoopbackAllowed,
        };
        assert!(ureq::Resolver::resolve(&loopback, "127.0.0.1:80").is_ok());
    }

    #[test]
    fn an_https_address_is_spoken_to_in_tls_and_never_in_plain_text() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("address").port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut first = [0u8; 1];
            stream.read_exact(&mut first).expect("a first byte");
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
            first[0]
        });
        let address = Address::parse(
            &format!("https://127.0.0.1:{port}"),
            AddressPolicy::LoopbackAllowed,
        )
        .expect("loopback");
        let failure = Outbound::new(AddressPolicy::LoopbackAllowed)
            .send(&address, Request::new("GET", "/secret?token=t"))
            .expect_err("a plain-text answer is no TLS server");
        assert_ne!(failure, Failure::Refused);
        assert_eq!(
            server.join().expect("server"),
            0x16,
            "the first byte is a TLS handshake record, not a request line"
        );
    }

    #[test]
    fn a_form_is_sent_encoded() {
        let (address, server) =
            serve_once(b"HTTP/1.1 201 Created\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        let response = Outbound::new(AddressPolicy::LoopbackAllowed)
            .send(
                &address,
                Request::new("POST", "/form").form(&[("scope", "a b"), ("uri", "http://x/?y=1&z")]),
            )
            .expect("a response");
        assert_eq!(response.status, 201);
        let head = server.join().expect("server");
        assert!(
            head.contains("Content-Type: application/x-www-form-urlencoded"),
            "{head}"
        );
        assert!(head.contains("Content-Length: 45"), "{head}");
        assert_eq!(
            form_encode("http://x/?y=1&z"),
            "http%3A%2F%2Fx%2F%3Fy%3D1%26z"
        );
    }
}
