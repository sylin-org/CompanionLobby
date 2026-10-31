//! A scripted loopback HTTP listener.
//!
//! Every fake in the test kit stands on one: a thread per connection, one
//! request per connection, a handler the fake injects to answer each request,
//! and a record of everything received, so a test can count what reached a
//! service. A test can also hold the gate for a route: requests to it are
//! recorded as they arrive and then wait, unanswered, until the hold is
//! dropped, which freezes a call mid-flight.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde_json::Value;

/// How long a test waits for a request to arrive.
pub const ARRIVAL_TIMEOUT: Duration = Duration::from_secs(20);

/// A request as the listener received it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Received {
    /// The method.
    pub method: String,
    /// The path and query, as sent.
    pub target: String,
    /// The headers, names lowercased.
    pub headers: BTreeMap<String, String>,
    /// The body bytes.
    pub body: Vec<u8>,
}

impl Received {
    /// The path without its query.
    pub fn route(&self) -> &str {
        self.target.split('?').next().unwrap_or_default()
    }

    /// The header `name`, given in any case.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }

    /// The query parameter `name`, percent-decoded.
    pub fn query(&self, name: &str) -> Option<String> {
        let (_, query) = self.target.split_once('?')?;
        query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (crate::percent_decode(key) == name).then(|| crate::percent_decode(value))
        })
    }

    /// The body as JSON, or `null` when it is not JSON.
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    /// The fields of a form body, percent-decoded, in the order sent.
    pub fn form(&self) -> Vec<(String, String)> {
        String::from_utf8_lossy(&self.body)
            .split('&')
            .filter(|pair| !pair.is_empty())
            .map(|pair| {
                let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
                (crate::percent_decode(name), crate::percent_decode(value))
            })
            .collect()
    }

    /// The form field `name`, when the body is a form that has it.
    pub fn field(&self, name: &str) -> Option<String> {
        self.form()
            .into_iter()
            .find(|(field, _)| field == name)
            .map(|(_, value)| value)
    }
}

/// What the listener answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    /// The status.
    pub status: u16,
    /// Headers beyond the length and the closing of the connection.
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: Vec<u8>,
}

impl Reply {
    /// A JSON body with `status`.
    pub fn json(status: u16, body: &Value) -> Self {
        Self {
            status,
            headers: vec![("Content-Type".to_owned(), "application/json".to_owned())],
            body: body.to_string().into_bytes(),
        }
    }

    /// A redirect to `location`.
    pub fn redirect(location: &str) -> Self {
        Self {
            status: 302,
            headers: vec![("Location".to_owned(), location.to_owned())],
            body: Vec::new(),
        }
    }

    /// The same reply with one more header.
    #[must_use]
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }
}

type Handler = dyn Fn(&Received) -> Reply + Send + Sync;

struct Shared {
    handler: Box<Handler>,
    requests: Mutex<Vec<Received>>,
    arrived: Condvar,
    held: Mutex<Vec<String>>,
    held_answers: Mutex<Vec<AnswerHold>>,
    handled: Mutex<Vec<String>>,
    released: Condvar,
    stopped: AtomicBool,
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A listener on 127.0.0.1 answering with an injected handler, stopped on drop.
pub struct Listener {
    port: u16,
    shared: Arc<Shared>,
}

/// Which answers an answer hold keeps back: those to a route starting with
/// `prefix`, and, when `field` names one, whose form field or JSON member has
/// that value.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AnswerHold {
    prefix: String,
    field: Option<(String, String)>,
}

impl AnswerHold {
    fn holds(&self, request: &Received) -> bool {
        request.route().starts_with(self.prefix.as_str())
            && self.field.as_ref().is_none_or(|(name, value)| {
                request.field(name).as_deref() == Some(value.as_str())
                    || request.json()[name.as_str()] == value.as_str()
            })
    }
}

/// A hold on one route: requests to it, or their answers, wait until this
/// is dropped.
pub struct Hold<'a> {
    listener: &'a Listener,
    prefix: String,
    answers: Option<AnswerHold>,
}

impl Drop for Hold<'_> {
    fn drop(&mut self) {
        let shared = &self.listener.shared;
        match &self.answers {
            Some(answers) => {
                let mut held = locked(&shared.held_answers);
                if let Some(index) = held.iter().position(|hold| hold == answers) {
                    held.remove(index);
                }
            }
            None => {
                let mut held = locked(&shared.held);
                if let Some(index) = held.iter().position(|prefix| *prefix == self.prefix) {
                    held.remove(index);
                }
            }
        }
        shared.released.notify_all();
    }
}

impl Listener {
    /// Listens on a fresh loopback port and answers every request with `handler`.
    pub fn start(handler: impl Fn(&Received) -> Reply + Send + Sync + 'static) -> Self {
        let socket = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
        let port = socket.local_addr().expect("local address").port();
        let shared = Arc::new(Shared {
            handler: Box::new(handler),
            requests: Mutex::new(Vec::new()),
            arrived: Condvar::new(),
            held: Mutex::new(Vec::new()),
            held_answers: Mutex::new(Vec::new()),
            handled: Mutex::new(Vec::new()),
            released: Condvar::new(),
            stopped: AtomicBool::new(false),
        });
        let accepting = shared.clone();
        std::thread::spawn(move || {
            for stream in socket.incoming() {
                if accepting.stopped.load(Ordering::Acquire) {
                    return;
                }
                let Ok(stream) = stream else { continue };
                let shared = accepting.clone();
                std::thread::spawn(move || serve(stream, &shared));
            }
        });
        Self { port, shared }
    }

    /// The port.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The origin, `http://127.0.0.1:<port>`.
    pub fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Everything received so far, oldest first.
    pub fn requests(&self) -> Vec<Received> {
        locked(&self.shared.requests).clone()
    }

    /// How many requests to `route` were received.
    pub fn count(&self, route: &str) -> usize {
        locked(&self.shared.requests)
            .iter()
            .filter(|request| request.route() == route)
            .count()
    }

    /// Holds every request whose route starts with `prefix` until the
    /// returned hold is dropped.
    pub fn hold(&self, prefix: &str) -> Hold<'_> {
        locked(&self.shared.held).push(prefix.to_owned());
        Hold {
            listener: self,
            prefix: prefix.to_owned(),
            answers: None,
        }
    }

    /// Lets every request whose route starts with `prefix` be handled, and
    /// holds its answer until the returned hold is dropped: what the
    /// handler did has happened, and the client has not heard of it.
    pub fn hold_answers(&self, prefix: &str) -> Hold<'_> {
        self.hold_answers_like(AnswerHold {
            prefix: prefix.to_owned(),
            field: None,
        })
    }

    /// [`Listener::hold_answers`], for the requests to `prefix` whose form
    /// field or JSON member `field` is `value` only.
    pub fn hold_answers_to(&self, prefix: &str, field: &str, value: &str) -> Hold<'_> {
        self.hold_answers_like(AnswerHold {
            prefix: prefix.to_owned(),
            field: Some((field.to_owned(), value.to_owned())),
        })
    }

    fn hold_answers_like(&self, hold: AnswerHold) -> Hold<'_> {
        locked(&self.shared.held_answers).push(hold.clone());
        Hold {
            listener: self,
            prefix: hold.prefix.clone(),
            answers: Some(hold),
        }
    }

    /// Waits until `count` requests to `route` were received in all.
    pub fn wait_for(&self, route: &str, count: usize) {
        let deadline = Instant::now() + ARRIVAL_TIMEOUT;
        let mut requests = locked(&self.shared.requests);
        while requests
            .iter()
            .filter(|request| request.route() == route)
            .count()
            < count
        {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "no request {count} to {route} arrived");
            requests = self
                .shared
                .arrived
                .wait_timeout(requests, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Waits until the handler answered `count` requests to `route` in
    /// all, whether or not an answer hold keeps those answers back: what
    /// the requests did at the fake is done.
    pub fn wait_for_handled(&self, route: &str, count: usize) {
        let deadline = Instant::now() + ARRIVAL_TIMEOUT;
        let mut handled = locked(&self.shared.handled);
        while handled.iter().filter(|handled| *handled == route).count() < count {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "no request {count} to {route} was handled");
            handled = self
                .shared
                .arrived
                .wait_timeout(handled, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.shared.stopped.store(true, Ordering::Release);
        locked(&self.shared.held).clear();
        locked(&self.shared.held_answers).clear();
        self.shared.released.notify_all();
        // Wakes the accepting thread so it sees the stop.
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

fn serve(stream: TcpStream, shared: &Shared) {
    let _ = stream.set_read_timeout(Some(ARRIVAL_TIMEOUT));
    let Some(request) = read_request(&stream) else {
        return;
    };
    {
        locked(&shared.requests).push(request.clone());
        shared.arrived.notify_all();
    }
    {
        let mut held = locked(&shared.held);
        while held
            .iter()
            .any(|prefix| request.route().starts_with(prefix.as_str()))
        {
            held = shared
                .released
                .wait(held)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
    let reply = (shared.handler)(&request);
    {
        locked(&shared.handled).push(request.route().to_owned());
        shared.arrived.notify_all();
    }
    {
        let mut held = locked(&shared.held_answers);
        while held.iter().any(|hold| hold.holds(&request)) {
            held = shared
                .released
                .wait(held)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
    let mut head = format!("HTTP/1.1 {} Scripted\r\n", reply.status);
    for (name, value) in &reply.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        reply.body.len()
    ));
    let mut stream = stream;
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&reply.body);
    let _ = stream.flush();
}

fn read_request(stream: &TcpStream) -> Option<Received> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_owned();
    let target = parts.next()?.to_owned();
    let mut headers = BTreeMap::new();
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).ok()? == 0 {
            return None;
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        let (name, value) = header.split_once(':')?;
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
    }
    let length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).ok()?;
    Some(Received {
        method,
        target,
        headers,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn get(port: u16, target: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        write!(
            stream,
            "GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Probe: Yes\r\n\r\n"
        )
        .expect("write");
        let mut answer = String::new();
        stream.read_to_string(&mut answer).expect("read");
        answer
    }

    #[test]
    fn requests_are_recorded_and_answered_by_the_handler() {
        let listener = Listener::start(|request| {
            Reply::json(
                201,
                &json!({ "route": request.route(), "q": request.query("q") }),
            )
        });
        let answer = get(listener.port(), "/one?q=a%20b");
        assert!(answer.starts_with("HTTP/1.1 201"), "{answer}");
        assert!(
            answer.ends_with(r#"{"q":"a b","route":"/one"}"#),
            "{answer}"
        );
        let requests = listener.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].header("x-probe"), Some("Yes"));
        assert_eq!(listener.count("/one"), 1);
        assert_eq!(listener.count("/two"), 0);
    }

    #[test]
    fn a_held_route_is_recorded_then_waits_for_the_release() {
        let listener = Listener::start(|_| Reply::json(200, &json!({})));
        let port = listener.port();
        let hold = listener.hold("/slow");
        let call = std::thread::spawn(move || get(port, "/slow"));
        listener.wait_for("/slow", 1);
        std::thread::sleep(Duration::from_millis(100));
        assert!(!call.is_finished(), "held");
        assert!(get(port, "/fast").starts_with("HTTP/1.1 200"));
        drop(hold);
        assert!(call.join().expect("call").starts_with("HTTP/1.1 200"));
    }

    #[test]
    fn a_held_answer_comes_after_the_handler_ran() {
        let handled = Arc::new(AtomicBool::new(false));
        let marking = handled.clone();
        let listener = Listener::start(move |_| {
            marking.store(true, Ordering::SeqCst);
            Reply::json(200, &json!({}))
        });
        let port = listener.port();
        let hold = listener.hold_answers("/slow");
        let call = std::thread::spawn(move || get(port, "/slow"));
        listener.wait_for("/slow", 1);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !handled.load(Ordering::SeqCst) {
            assert!(Instant::now() < deadline, "the handler ran");
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(100));
        assert!(!call.is_finished(), "the answer waits");
        drop(hold);
        assert!(call.join().expect("call").starts_with("HTTP/1.1 200"));
    }
}
