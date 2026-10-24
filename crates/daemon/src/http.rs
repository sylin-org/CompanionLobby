//! The loopback HTTP listener and the daemon's main loop.
//!
//! The daemon listens on 127.0.0.1 only. Every request passes the same
//! envelope, in this order, before any body is read:
//!
//! 1. the `Host` header must name 127.0.0.1, with no port or the bound one;
//!    `localhost` is redirected to 127.0.0.1 on the same port, and any other
//!    name or port is refused, so a page that points its own hostname at the
//!    loopback address reaches nothing;
//! 2. a declared body larger than 1 MiB is refused, and a body that grows past
//!    it while arriving is refused too; headers are capped at 8 KiB; a head
//!    must arrive within 30 seconds and a body within 10 seconds after it;
//! 3. operator mutations under `/api/` need JSON and same-origin browser
//!    evidence: an `Origin` equal to the page's own origin (built from the
//!    bound port, never from the request), and a
//!    `Sec-Fetch-Site` that is absent or `same-origin`;
//! 4. `/mcp` refuses any browser evidence at all (`Origin`, `Referer` or a
//!    `Sec-Fetch-*` header). Attaching requires the per-install attach secret
//!    as a bearer token; every later frame and the detach require the token
//!    the daemon gave that one attachment, so the install secret crosses the
//!    port once per attachment, only after the shim verified the listener.
//!
//! Every response is sent with `Cache-Control: no-store`,
//! `Referrer-Policy: no-referrer` and `X-Content-Type-Options: nosniff`. Only
//! the discovery document carries cross-origin headers, and what it says to a
//! browser leaves out everything that identifies the install.
//!
//! `POST /api/stop` stops accepting connections, lets every accepted request
//! finish, and only then lets the process exit.

use std::convert::Infallible;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::header::{self, HeaderMap, HeaderValue};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::server::graceful::GracefulShutdown;
use lobby_contracts::{AttachmentId, Companion, Outcome};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::{Notify, Semaphore};

use crate::discovery::{DISCOVERY_PATH, Discovery};
use crate::domain::{Lobby, Refusal};
use crate::home;
use crate::keyring::{AttachSecret, AttachmentToken};
use crate::lock::{DaemonLock, DaemonRecord, LockProblem};
use crate::{discovery, mcp, trace};

/// Request head limit: the request line plus every header.
pub const HEADER_LIMIT: usize = 8 * 1024;
/// Request body limit.
pub const BODY_LIMIT: usize = 1024 * 1024;
/// How long a peer may take to send its request head.
pub const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a peer may take to send a body once its head has arrived, so a
/// trickled body cannot hold an admission slot or a stop's drain.
pub const BODY_READ_TIMEOUT: Duration = Duration::from_secs(10);
/// Requests served at once; one more is answered 503 at once.
pub const MAX_ACTIVE_REQUESTS: usize = 128;
/// Settling pause after a failed accept, so a persistent socket failure cannot spin the loop hot.
pub const ACCEPT_ERROR_PAUSE: Duration = Duration::from_millis(100);
/// The longest a stop waits for accepted requests to finish.
pub const DRAIN_LIMIT: Duration = Duration::from_secs(10);
/// The fixed default port.
pub const DEFAULT_PORT: u16 = 5219;
/// The environment variable that names another fixed port.
pub const PORT_ENV: &str = "LOBBY_PORT";
/// The header that names the attachment an MCP request acts for.
pub const ATTACHMENT_HEADER: &str = "lobby-attachment";

/// The port to use: `LOBBY_PORT` when set, the fixed default otherwise.
pub fn resolve_port() -> Result<u16, String> {
    port_from(std::env::var(PORT_ENV).ok().as_deref())
}

/// The pure decision behind [`resolve_port`]. Port 0 is refused: the port is
/// the contract hosts and pages rely on, so it is never random.
pub fn port_from(environment: Option<&str>) -> Result<u16, String> {
    let port = match environment.map(str::trim) {
        None | Some("") => DEFAULT_PORT,
        Some(value) => value
            .parse::<u16>()
            .map_err(|_| format!("{PORT_ENV} must be a port number, not '{value}'"))?,
    };
    if port == 0 {
        return Err(format!(
            "Companion Lobby needs a fixed port; 0 would pick a random one. Name one from 1 to 65535 with {PORT_ENV}."
        ));
    }
    Ok(port)
}

/// Everything a request handler can see.
pub struct App {
    lobby: Lobby,
    secret: AttachSecret,
    discovery: Discovery,
    port: u16,
    stop_requested: AtomicBool,
    stopping: Notify,
    admission: Semaphore,
}

impl App {
    /// The state behind one listener.
    pub fn new(port: u16, secret: AttachSecret, build_id: &str) -> Self {
        let discovery = Discovery::new(port, build_id, &secret.fingerprint());
        Self {
            lobby: Lobby::new(),
            secret,
            discovery,
            port,
            stop_requested: AtomicBool::new(false),
            stopping: Notify::new(),
            admission: Semaphore::new(MAX_ACTIVE_REQUESTS),
        }
    }

    fn request_stop(&self) {
        if !self.stop_requested.swap(true, Ordering::SeqCst) {
            self.stopping.notify_one();
        }
    }
}

/// Runs the daemon in the foreground until it is asked to stop.
pub fn run() -> Result<(), String> {
    let dir = home::resolve().map_err(|problem| problem.to_string())?;
    home::prepare(&dir).map_err(|problem| problem.to_string())?;
    let shown = home::display_path(&dir);
    trace::install(&dir)
        .map_err(|error| format!("cannot open the trace log in {shown}: {error}"))?;
    let lock = match DaemonLock::acquire(&dir) {
        Ok(lock) => lock,
        Err(LockProblem::Held) => {
            trace::line("start refused: another daemon holds the lock");
            return Err(format!("a Companion Lobby daemon already runs for {shown}"));
        }
        Err(LockProblem::Io(error)) => return Err(format!("cannot lock {shown}: {error}")),
    };
    let (secret, renewal) = AttachSecret::load_or_renew(&dir)
        .map_err(|error| format!("cannot read the attach secret in {shown}: {error}"))?;
    if let Some(renewal) = renewal {
        trace::line(renewal.describe());
    }
    let port = resolve_port()?;
    let build_id =
        discovery::build_id().map_err(|error| format!("cannot read this executable: {error}"))?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("cannot start the runtime: {error}"))?;
    let result = runtime.block_on(async {
        let listener = bind(port).await?;
        DaemonRecord {
            pid: std::process::id(),
            port,
            build_id: build_id.clone(),
        }
        .write(&dir)
        .map_err(|error| format!("cannot write the daemon record in {shown}: {error}"))?;
        trace::line(&format!("listening on 127.0.0.1:{port} build {build_id}"));
        let app = Arc::new(App::new(port, secret, &build_id));
        serve(listener, app).await;
        Ok::<(), String>(())
    });
    finish(&dir, lock);
    result
}

fn finish(dir: &Path, lock: DaemonLock) {
    DaemonRecord::remove_own(dir);
    trace::line("stopped");
    drop(lock);
}

/// Listens on 127.0.0.1:`port`. On Windows the socket is bound for exclusive
/// use, so no other socket can share the port while the daemon runs.
fn listen(port: u16) -> io::Result<TcpListener> {
    let socket = tokio::net::TcpSocket::new_v4()?;
    #[cfg(windows)]
    crate::win32::use_address_exclusively(&socket)?;
    #[cfg(unix)]
    socket.set_reuseaddr(true)?;
    socket.bind(std::net::SocketAddr::from((
        std::net::Ipv4Addr::LOCALHOST,
        port,
    )))?;
    socket.listen(1024)
}

async fn bind(port: u16) -> Result<TcpListener, String> {
    match listen(port) {
        Ok(listener) => Ok(listener),
        Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
            trace::line(&format!("start refused: port {port} is in use"));
            Err(format!(
                "another process already listens on 127.0.0.1:{port}; stop it, or choose another port with {PORT_ENV}"
            ))
        }
        Err(error) => Err(format!("cannot listen on 127.0.0.1:{port}: {error}")),
    }
}

/// Serves `listener` until a stop is requested, then drains accepted requests.
/// The listener stays bound through the drain, accepting nothing, so no other
/// process can take the port while this daemon still holds the lock and its
/// record names it; it closes only once the drain is over.
pub async fn serve(listener: TcpListener, app: Arc<App>) {
    let graceful = GracefulShutdown::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    let app = app.clone();
                    let service = service_fn(move |request| handle(app.clone(), request));
                    let mut builder = http1::Builder::new();
                    builder
                        .max_buf_size(HEADER_LIMIT)
                        .timer(TokioTimer::new())
                        .header_read_timeout(READ_TIMEOUT)
                        .keep_alive(false);
                    let connection = graceful.watch(builder.serve_connection(TokioIo::new(stream), service));
                    tokio::spawn(async move {
                        let _ = connection.await;
                    });
                }
                // Transient accept failures never end the loop.
                Err(_) => tokio::time::sleep(ACCEPT_ERROR_PAUSE).await,
            },
            () = app.stopping.notified() => break,
        }
    }
    trace::line("stop requested; draining accepted requests");
    if tokio::time::timeout(DRAIN_LIMIT, graceful.shutdown())
        .await
        .is_err()
    {
        trace::line("drain limit reached; exiting with requests still open");
    }
    drop(listener);
}

type Reply = Response<Full<Bytes>>;

async fn handle(app: Arc<App>, request: Request<Incoming>) -> Result<Reply, Infallible> {
    let Ok(_admitted) = app.admission.try_acquire() else {
        return Ok(problem(
            StatusCode::SERVICE_UNAVAILABLE,
            Outcome::TemporarilyUnavailable,
            "The lobby is busy; try again.",
        ));
    };
    Ok(route(&app, request).await)
}

/// Where a request's `Host` header puts it.
#[derive(Debug, PartialEq, Eq)]
enum HostRule {
    Loopback,
    Redirect,
    Foreign,
}

/// The rule for a `Host` header on a daemon bound to `port`. A port other than
/// the bound one is foreign: the daemon knows where it listens.
fn host_rule(host: Option<&str>, port: u16) -> HostRule {
    let Some(host) = host else {
        return HostRule::Foreign;
    };
    if host.starts_with('[') {
        // IPv6 literals, [::1] included: the daemon listens on 127.0.0.1 only.
        return HostRule::Foreign;
    }
    let (name, named_port) = match host.split_once(':') {
        Some((name, named_port)) => (name, Some(named_port)),
        None => (host, None),
    };
    if named_port.is_some_and(|named_port| named_port != port.to_string()) {
        return HostRule::Foreign;
    }
    if name == "127.0.0.1" {
        HostRule::Loopback
    } else if name.eq_ignore_ascii_case("localhost") {
        HostRule::Redirect
    } else {
        HostRule::Foreign
    }
}

fn header_text(headers: &HeaderMap, name: impl header::AsHeaderName) -> Option<&str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

async fn route(app: &App, request: Request<Incoming>) -> Reply {
    let headers = request.headers();
    match host_rule(header_text(headers, header::HOST), app.port) {
        HostRule::Loopback => {}
        HostRule::Redirect => {
            let target = request
                .uri()
                .path_and_query()
                .map_or("/", |path| path.as_str());
            return redirect(&format!("{}{target}", discovery::manager_origin(app.port)));
        }
        HostRule::Foreign => {
            return problem(
                StatusCode::FORBIDDEN,
                Outcome::Refused,
                "This port answers requests for 127.0.0.1 only.",
            );
        }
    }
    let declared =
        header_text(headers, header::CONTENT_LENGTH).and_then(|value| value.parse::<u64>().ok());
    if declared.is_some_and(|length| length > BODY_LIMIT as u64) {
        return problem(
            StatusCode::PAYLOAD_TOO_LARGE,
            Outcome::InvalidInput,
            "The body exceeds 1 MiB.",
        );
    }
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    match (&method, path.as_str()) {
        (&Method::GET | &Method::OPTIONS, DISCOVERY_PATH) => {
            discovery_reply(app, method == Method::OPTIONS, browser_evidence(headers))
        }
        (&Method::GET, "/") => page(app),
        (&Method::GET, "/api/companions") => json_reply(
            StatusCode::OK,
            json!({ "companions": app.lobby.companions().iter().map(companion_json).collect::<Vec<_>>() }),
        ),
        (&Method::POST, "/api/companions") => match operator_body(app, request).await {
            Ok(body) => create_companion(app, &body),
            Err(refused) => refused.reply(),
        },
        (&Method::POST, "/api/stop") => match operator_body(app, request).await {
            Ok(_) => {
                trace::line("stop requested over HTTP");
                app.request_stop();
                json_reply(
                    StatusCode::ACCEPTED,
                    json!({ "outcome": Outcome::Accepted.code() }),
                )
            }
            Err(refused) => refused.reply(),
        },
        (&Method::POST, "/mcp/attach") => match attach_body(app, request).await {
            Ok(body) => attach(app, &body),
            Err(refused) => refused.reply(),
        },
        (&Method::DELETE, detach_path) if detach_path.starts_with("/mcp/attach/") => {
            detach(app, request.headers(), &detach_path["/mcp/attach/".len()..])
        }
        (&Method::POST, "/mcp") => {
            let attachment =
                header_text(request.headers(), ATTACHMENT_HEADER).and_then(AttachmentId::parse);
            let companion = match attachment_guard(app, request.headers(), attachment) {
                Ok(companion) => companion,
                Err(refused) => return *refused,
            };
            match json_body(request).await {
                Ok(body) => mcp_reply(&companion, &body),
                Err(refused) => refused.reply(),
            }
        }
        (_, DISCOVERY_PATH | "/" | "/api/companions" | "/api/stop" | "/mcp/attach" | "/mcp") => {
            problem(
                StatusCode::METHOD_NOT_ALLOWED,
                Outcome::Unsupported,
                "That method is not served here.",
            )
        }
        _ => problem(
            StatusCode::NOT_FOUND,
            Outcome::NotFound,
            "Nothing is served at that path.",
        ),
    }
}

/// Whether the media type is JSON, whatever its case or parameters.
fn is_json(headers: &HeaderMap) -> bool {
    header_text(headers, header::CONTENT_TYPE)
        .and_then(|value| value.split(';').next())
        .is_some_and(|media| media.trim().eq_ignore_ascii_case("application/json"))
}

/// A refusal decided before a handler runs.
struct Problem(StatusCode, Outcome, &'static str);

impl Problem {
    fn reply(&self) -> Reply {
        problem(self.0, self.1, self.2)
    }
}

async fn read_body(request: Request<Incoming>) -> Result<Bytes, Problem> {
    let collecting = Limited::new(request.into_body(), BODY_LIMIT).collect();
    match tokio::time::timeout(BODY_READ_TIMEOUT, collecting).await {
        Ok(Ok(collected)) => Ok(collected.to_bytes()),
        Ok(Err(_)) => Err(Problem(
            StatusCode::PAYLOAD_TOO_LARGE,
            Outcome::InvalidInput,
            "The body exceeds 1 MiB or did not arrive.",
        )),
        Err(_) => Err(Problem(
            StatusCode::REQUEST_TIMEOUT,
            Outcome::InvalidInput,
            "The body did not arrive in time.",
        )),
    }
}

/// The operator write guard: JSON, and browser evidence that the request
/// came from the page's own origin, which the daemon knows from its own port.
async fn operator_body(app: &App, request: Request<Incoming>) -> Result<Bytes, Problem> {
    let headers = request.headers();
    let same_origin =
        header_text(headers, header::ORIGIN) == Some(discovery::manager_origin(app.port).as_str());
    let fetch_site_ok =
        header_text(headers, "sec-fetch-site").is_none_or(|site| site == "same-origin");
    if !is_json(headers) || !same_origin || !fetch_site_ok {
        return Err(Problem(
            StatusCode::FORBIDDEN,
            Outcome::Refused,
            "Operator changes come from the manager page itself, as JSON.",
        ));
    }
    read_body(request).await
}

/// Whether a request carries any sign of having come from a browser.
fn browser_evidence(headers: &HeaderMap) -> bool {
    [
        header::ORIGIN.as_str(),
        header::REFERER.as_str(),
        "sec-fetch-site",
        "sec-fetch-mode",
        "sec-fetch-dest",
    ]
    .iter()
    .any(|name| headers.contains_key(*name))
}

/// Refuses a request to `/mcp...` that came from a browser.
fn refuse_browser(headers: &HeaderMap) -> Result<(), Problem> {
    if browser_evidence(headers) {
        return Err(Problem(
            StatusCode::FORBIDDEN,
            Outcome::Refused,
            "Minds attach through companion-lobby attach, never from a browser.",
        ));
    }
    Ok(())
}

/// The bearer token a request presents, if any.
fn bearer(headers: &HeaderMap) -> Option<&str> {
    header_text(headers, header::AUTHORIZATION).and_then(|value| value.strip_prefix("Bearer "))
}

/// The attach guard: no browser evidence, and the install's attach secret.
fn install_guard(app: &App, headers: &HeaderMap) -> Result<(), Problem> {
    refuse_browser(headers)?;
    if !bearer(headers).is_some_and(|secret| app.secret.matches(secret)) {
        return Err(Problem(
            StatusCode::UNAUTHORIZED,
            Outcome::Refused,
            "Attaching needs this install's attach secret.",
        ));
    }
    Ok(())
}

/// The guard for frames: no browser evidence, a live attachment, and that
/// attachment's own token.
fn attachment_guard(
    app: &App,
    headers: &HeaderMap,
    attachment: Option<AttachmentId>,
) -> Result<Companion, Box<Reply>> {
    refuse_browser(headers).map_err(|refused| Box::new(refused.reply()))?;
    let Some(presented) = bearer(headers) else {
        return Err(Box::new(problem(
            StatusCode::UNAUTHORIZED,
            Outcome::Refused,
            "Every frame needs its attachment's token.",
        )));
    };
    let Some(attachment) = attachment else {
        return Err(Box::new(problem(
            StatusCode::NOT_FOUND,
            Outcome::NotFound,
            "No such attachment is live; attach again.",
        )));
    };
    app.lobby
        .attachment(attachment, presented)
        .map(|(_, companion)| companion)
        .map_err(|refusal| Box::new(refusal_reply(&refusal)))
}

async fn json_body(request: Request<Incoming>) -> Result<Bytes, Problem> {
    if !is_json(request.headers()) {
        return Err(Problem(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Outcome::InvalidInput,
            "The body must be JSON.",
        ));
    }
    read_body(request).await
}

async fn attach_body(app: &App, request: Request<Incoming>) -> Result<Bytes, Problem> {
    install_guard(app, request.headers())?;
    json_body(request).await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateCompanion {
    moniker: String,
    display_name: Option<String>,
}

fn create_companion(app: &App, body: &[u8]) -> Reply {
    let Ok(request) = serde_json::from_slice::<CreateCompanion>(body) else {
        return problem(
            StatusCode::BAD_REQUEST,
            Outcome::InvalidInput,
            "Send {\"moniker\": ..., \"displayName\": ...} and nothing else.",
        );
    };
    let display_name = request
        .display_name
        .unwrap_or_else(|| request.moniker.clone());
    match app.lobby.create_companion(&request.moniker, &display_name) {
        Ok(companion) => {
            trace::line(&format!("companion {} created", companion.id));
            json_reply(
                StatusCode::CREATED,
                json!({ "companion": companion_json(&companion) }),
            )
        }
        Err(refusal) => refusal_reply(&refusal),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AttachRequest {
    companion: String,
}

fn attach(app: &App, body: &[u8]) -> Reply {
    let Ok(request) = serde_json::from_slice::<AttachRequest>(body) else {
        return problem(
            StatusCode::BAD_REQUEST,
            Outcome::InvalidInput,
            "Send {\"companion\": ...} and nothing else.",
        );
    };
    let Ok(token) = AttachmentToken::generate() else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            Outcome::TemporarilyUnavailable,
            "The lobby could not draw a random token; try again.",
        );
    };
    match app.lobby.attach(&request.companion, token.clone()) {
        Ok((attachment, companion)) => {
            trace::line(&format!(
                "attachment {} attached as companion {}; {} live",
                attachment.id,
                companion.id,
                app.lobby.attachment_count()
            ));
            json_reply(
                StatusCode::CREATED,
                json!({
                    "attachment": attachment.id.to_string(),
                    "token": token.expose(),
                    "companion": companion_json(&companion),
                }),
            )
        }
        Err(refusal) => refusal_reply(&refusal),
    }
}

fn detach(app: &App, headers: &HeaderMap, id: &str) -> Reply {
    if let Err(refused) = refuse_browser(headers) {
        return refused.reply();
    }
    let Some(presented) = bearer(headers) else {
        return problem(
            StatusCode::UNAUTHORIZED,
            Outcome::Refused,
            "Detaching needs the attachment's token.",
        );
    };
    let Some(id) = AttachmentId::parse(id) else {
        return problem(
            StatusCode::NOT_FOUND,
            Outcome::NotFound,
            "No such attachment is live.",
        );
    };
    match app.lobby.detach(id, presented) {
        Ok(()) => {
            trace::line(&format!(
                "attachment {id} detached; {} live",
                app.lobby.attachment_count()
            ));
            empty_reply(StatusCode::NO_CONTENT)
        }
        Err(refusal) => refusal_reply(&refusal),
    }
}

fn mcp_reply(companion: &Companion, body: &[u8]) -> Reply {
    let Ok(message) = serde_json::from_slice::<Value>(body) else {
        return json_reply(
            StatusCode::OK,
            mcp::rpc_error(&Value::Null, mcp::PARSE_ERROR, "Parse error"),
        );
    };
    match mcp::handle(companion, &message) {
        Some(answer) => json_reply(StatusCode::OK, answer),
        None => empty_reply(StatusCode::ACCEPTED),
    }
}

fn companion_json(companion: &Companion) -> Value {
    json!({
        "id": companion.id.to_string(),
        "moniker": companion.moniker.as_str(),
        "displayName": companion.display_name.as_str(),
    })
}

fn refusal_reply(refusal: &Refusal) -> Reply {
    let status = match refusal.outcome {
        Outcome::NotFound => StatusCode::NOT_FOUND,
        Outcome::Conflict => StatusCode::CONFLICT,
        Outcome::InvalidInput => StatusCode::BAD_REQUEST,
        // A token that does not match is a failed proof, as a missing one is.
        Outcome::Refused => StatusCode::UNAUTHORIZED,
        _ => StatusCode::FORBIDDEN,
    };
    problem(status, refusal.outcome, &refusal.message)
}

/// The discovery document. A request with browser evidence gets the fields a
/// page needs to find the daemon and nothing that identifies this install or
/// its process; the shim, which sends none, gets the whole document.
fn discovery_reply(app: &App, preflight: bool, from_browser: bool) -> Reply {
    let mut reply = if preflight {
        empty_reply(StatusCode::NO_CONTENT)
    } else if from_browser {
        json_reply(StatusCode::OK, app.discovery.for_pages())
    } else {
        json_reply(
            StatusCode::OK,
            serde_json::to_value(&app.discovery).unwrap_or(Value::Null),
        )
    };
    let headers = reply.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, OPTIONS"),
    );
    headers.insert(
        "access-control-allow-private-network",
        HeaderValue::from_static("true"),
    );
    headers.insert(
        header::VARY,
        HeaderValue::from_static("Origin, Access-Control-Request-Private-Network"),
    );
    reply
}

fn page(app: &App) -> Reply {
    let companions = app.lobby.companions();
    let items = if companions.is_empty() {
        "<li>No companions yet.</li>".to_owned()
    } else {
        companions
            .iter()
            .map(|companion| {
                format!(
                    "<li><b>{}</b> <code>{}</code></li>",
                    html_escape(companion.display_name.as_str()),
                    html_escape(companion.moniker.as_str())
                )
            })
            .collect()
    };
    let body = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"referrer\" content=\"no-referrer\">\
         <title>Companion Lobby</title></head><body><h1>Companion Lobby</h1><h2>Companions</h2><ul>{items}</ul></body></html>"
    );
    let mut reply = with_hygiene(Response::new(Full::new(Bytes::from(body))), StatusCode::OK);
    let headers = reply.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
        ),
    );
    reply
}

/// Escapes text for HTML element content and attribute values.
pub fn html_escape(text: &str) -> String {
    text.chars().fold(
        String::with_capacity(text.len()),
        |mut escaped, character| {
            match character {
                '&' => escaped.push_str("&amp;"),
                '<' => escaped.push_str("&lt;"),
                '>' => escaped.push_str("&gt;"),
                '"' => escaped.push_str("&quot;"),
                '\'' => escaped.push_str("&#39;"),
                other => escaped.push(other),
            }
            escaped
        },
    )
}

fn with_hygiene(mut reply: Reply, status: StatusCode) -> Reply {
    *reply.status_mut() = status;
    let headers = reply.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    reply
}

fn json_reply(status: StatusCode, body: Value) -> Reply {
    let bytes = serde_json::to_vec(&body).unwrap_or_default();
    let mut reply = with_hygiene(Response::new(Full::new(Bytes::from(bytes))), status);
    reply.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    reply
}

fn empty_reply(status: StatusCode) -> Reply {
    with_hygiene(Response::new(Full::new(Bytes::new())), status)
}

fn redirect(location: &str) -> Reply {
    let mut reply = empty_reply(StatusCode::PERMANENT_REDIRECT);
    if let Ok(value) = HeaderValue::from_str(location) {
        reply.headers_mut().insert(header::LOCATION, value);
    }
    reply
}

fn problem(status: StatusCode, outcome: Outcome, message: &str) -> Reply {
    json_reply(
        status,
        json!({ "outcome": outcome.code(), "message": message }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_port_is_the_default_unless_the_environment_names_one() {
        assert_eq!(port_from(None), Ok(DEFAULT_PORT));
        assert_eq!(port_from(Some("  ")), Ok(DEFAULT_PORT));
        assert_eq!(port_from(Some("5300")), Ok(5300));
        assert!(port_from(Some("0")).is_err());
        assert!(port_from(Some("http")).is_err());
        assert!(port_from(Some("70000")).is_err());
    }

    #[test]
    fn only_the_loopback_address_is_served_and_localhost_is_redirected() {
        let rule = |host: Option<&str>| host_rule(host, 5219);
        assert_eq!(rule(Some("127.0.0.1:5219")), HostRule::Loopback);
        assert_eq!(rule(Some("127.0.0.1")), HostRule::Loopback);
        assert_eq!(rule(Some("localhost:5219")), HostRule::Redirect);
        assert_eq!(rule(Some("LOCALHOST")), HostRule::Redirect);
        for foreign in [
            "[::1]:5219",
            "evil.test",
            "127.0.0.1.evil.test",
            "localhost.evil.test:5219",
            "127.0.0.1:5220",
            "127.0.0.1:05219",
            "localhost:80",
            "",
        ] {
            assert_eq!(rule(Some(foreign)), HostRule::Foreign, "{foreign}");
        }
        assert_eq!(rule(None), HostRule::Foreign);
    }

    #[test]
    fn json_is_recognized_whatever_its_case_or_parameters() {
        let mut headers = HeaderMap::new();
        assert!(!is_json(&headers));
        for value in ["application/json", "Application/JSON; charset=utf-8"] {
            headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(value));
            assert!(is_json(&headers), "{value}");
        }
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));
        assert!(!is_json(&headers));
    }

    #[test]
    fn html_is_escaped() {
        assert_eq!(
            html_escape("<a href='x'>&\""),
            "&lt;a href=&#39;x&#39;&gt;&amp;&quot;"
        );
    }

    #[test]
    fn every_reply_carries_the_hygiene_headers() {
        for reply in [
            json_reply(StatusCode::OK, json!({})),
            empty_reply(StatusCode::NO_CONTENT),
            redirect("http://127.0.0.1:1/"),
            problem(StatusCode::FORBIDDEN, Outcome::Refused, "no"),
        ] {
            let headers = reply.headers();
            assert_eq!(headers[header::CACHE_CONTROL], "no-store");
            assert_eq!(headers[header::REFERRER_POLICY], "no-referrer");
            assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
            assert!(!headers.contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN));
        }
    }
}
