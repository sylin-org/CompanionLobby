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
//! 3. operator changes under `/api/` (every method but `GET`) need JSON and
//!    same-origin browser evidence: an `Origin` equal to the page's own
//!    origin (built from the bound port, never from the request), and a
//!    `Sec-Fetch-Site` that is absent or `same-origin`;
//! 4. reads under `/api/` carry no browser evidence of another origin: an
//!    `Origin`, when present, is the page's own, and a `Sec-Fetch-Site`, when
//!    present, is `same-origin` or `none`, so a page elsewhere cannot hold the
//!    live feed's places or read the lobby through a browser that lets it;
//! 5. `/mcp` refuses any browser evidence at all (`Origin`, `Referer` or a
//!    `Sec-Fetch-*` header). Attaching requires the per-install attach secret
//!    as a bearer token; every later frame, the notice stream and the detach
//!    require the token the daemon gave that one attachment, so the install
//!    secret crosses the port once per attachment, only after the shim
//!    verified the listener;
//! 6. `/signin/<kind>/return` is where a provider sends the sponsor's browser
//!    back after a sign-in; it completes only a sign-in the lobby started and
//!    waits for, once, and sends the browser on to `/signin/<kind>/done`,
//!    which says how it ended from the outcome alone, so the code and the
//!    `state` leave the address bar at once and no page echoes anything the
//!    provider sent.
//!
//! Every response is sent with `Cache-Control: no-store`,
//! `Referrer-Policy: no-referrer` and `X-Content-Type-Options: nosniff`. Only
//! the discovery document carries cross-origin headers, and what it says to a
//! browser leaves out everything that identifies the install.
//!
//! Commands run on a blocking thread, since they write the state file and the
//! journal before they answer, and some call an adapter. `GET /api/events` is
//! the live feed the manager page follows, as server-sent events. `GET /mcp`
//! is an attachment's notice stream, also server-sent events: one
//! `notifications/tools/list_changed` frame each time the companion's tools
//! change, which the shim relays to the host.
//!
//! `POST /api/stop` stops accepting connections, ends the live feeds, lets
//! every accepted request finish, and only then lets the process exit.

use std::convert::Infallible;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::header::{self, HeaderMap, HeaderValue};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::server::graceful::GracefulShutdown;
use lobby_contracts::{
    Attachment, AttachmentId, Companion, CompanionId, ConnectionId, CredentialId, Outcome,
    ProviderKind, ToolResult,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::{Notify, Semaphore, broadcast, mpsc, watch};

use crate::discovery::{DISCOVERY_PATH, Discovery};
use crate::domain::{CallRefusal, Grant, Lobby, Refusal, Rename, Snapshot};
use crate::home;
use crate::journal::{PAGE_DEFAULT, PageProblem};
use crate::keyring::{AttachSecret, AttachmentToken};
use crate::lock::{DaemonLock, DaemonRecord, LockProblem};
use crate::manager;
use crate::projections::{Reached, companion_json, connection_json, credential_json, sign_in_json};
use crate::registry::Registry;
use crate::{discovery, mcp, trace};

mod feed_body;
use feed_body::FeedBody;

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
/// Live feeds served at once; one more is answered 503.
pub const FEED_CLIENT_LIMIT: usize = 16;
/// The live feed's path.
pub const FEED_PATH: &str = "/api/events";
/// The MCP path: frames are posted to it and the notice stream is read from it.
pub const MCP_PATH: &str = "/mcp";
/// How often a quiet live feed sends a comment, so a vanished page is noticed.
pub const FEED_KEEPALIVE: Duration = Duration::from_secs(15);

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
    stopped: watch::Sender<bool>,
    admission: Semaphore,
    feed_clients: AtomicUsize,
}

impl App {
    /// The state behind one listener.
    pub fn new(port: u16, secret: AttachSecret, build_id: &str, lobby: Lobby) -> Self {
        let discovery = Discovery::new(port, build_id, &secret.fingerprint());
        Self {
            lobby,
            secret,
            discovery,
            port,
            stop_requested: AtomicBool::new(false),
            stopping: Notify::new(),
            stopped: watch::channel(false).0,
            admission: Semaphore::new(MAX_ACTIVE_REQUESTS),
            feed_clients: AtomicUsize::new(0),
        }
    }

    fn request_stop(&self) {
        if !self.stop_requested.swap(true, Ordering::SeqCst) {
            self.stopping.notify_one();
            self.stopped.send_replace(true);
        }
    }
}

/// Runs the daemon in the foreground until it is asked to stop.
pub fn run(registry: Registry) -> Result<(), String> {
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
    let command = std::env::current_exe()
        .map(|exe| home::display_path(&exe))
        .map_err(|error| format!("cannot read this executable's path: {error}"))?;
    let lobby = Lobby::open(&dir, &command, registry).map_err(|problem| {
        trace::line(&format!("start refused: {problem}"));
        format!("{problem} (in {shown})")
    })?;
    lobby
        .sweep_keyring()
        .map_err(|refusal| format!("{} (in {shown})", refusal.message))?;
    let secret = attach_secret(&dir, &lobby)
        .map_err(|problem| format!("cannot use the attach secret in {shown}: {problem}"))?;
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
        let app = Arc::new(App::new(port, secret, &build_id, lobby));
        serve(listener, app).await;
        Ok::<(), String>(())
    });
    finish(&dir, lock);
    result
}

/// The install's attach secret, written fresh when there is none or the one
/// there cannot be trusted; replacing one that was there is journaled.
fn attach_secret(dir: &Path, lobby: &Lobby) -> Result<AttachSecret, String> {
    let renewal = match AttachSecret::load(dir).map_err(|error| error.to_string())? {
        Ok(secret) => return Ok(secret),
        Err(renewal) => renewal,
    };
    let secret = if renewal.replaces_one() {
        lobby
            .renew_attach_secret(|| AttachSecret::renew(dir))
            .map_err(|refusal| refusal.message)?
    } else {
        AttachSecret::renew(dir).map_err(|error| error.to_string())?
    };
    trace::line(renewal.describe());
    Ok(secret)
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
///
/// A live feed is not drained, nor is an attachment's notice stream. Neither
/// finishes on its own, and a reader that stopped reading leaves frames the
/// connection can never flush, so once a stop is requested such a connection
/// is dropped rather than awaited.
pub async fn serve(listener: TcpListener, app: Arc<App>) {
    let graceful = GracefulShutdown::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    let (marks_feed, mut serves_feed) = watch::channel(false);
                    let service = service_fn({
                        let app = app.clone();
                        move |request: Request<Incoming>| {
                            if is_stream(&request) {
                                marks_feed.send_replace(true);
                            }
                            handle(app.clone(), request)
                        }
                    });
                    let mut builder = http1::Builder::new();
                    builder
                        .max_buf_size(HEADER_LIMIT)
                        .timer(TokioTimer::new())
                        .header_read_timeout(READ_TIMEOUT)
                        .keep_alive(false);
                    let connection = graceful.watch(builder.serve_connection(TokioIo::new(stream), service));
                    let mut stopped = app.stopped.subscribe();
                    tokio::spawn(async move {
                        tokio::select! {
                            _ = connection => {}
                            () = feed_abandoned(&mut serves_feed, &mut stopped) => {}
                        }
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

type Reply = Response<BoxBody<Bytes, Infallible>>;

/// Whether `request` opens a stream that never finishes on its own.
fn is_stream(request: &Request<Incoming>) -> bool {
    let path = request.uri().path();
    path == FEED_PATH || (path == MCP_PATH && request.method() == Method::GET)
}

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

/// The companion an `/api/companions/<id>` path names.
fn companion_in(path: &str) -> Option<CompanionId> {
    path.strip_prefix("/api/companions/")
        .and_then(CompanionId::parse)
}

/// The credential an `/api/credentials/<id>/revoke` path names.
fn credential_revoked_in(path: &str) -> Option<CredentialId> {
    path.strip_prefix("/api/credentials/")
        .and_then(|rest| rest.strip_suffix("/revoke"))
        .and_then(CredentialId::parse)
}

/// The connection an `/api/connections/<id>/revoke` path names.
fn connection_revoked_in(path: &str) -> Option<ConnectionId> {
    path.strip_prefix("/api/connections/")
        .and_then(|rest| rest.strip_suffix("/revoke"))
        .and_then(ConnectionId::parse)
}

/// The sign-in an `/api/sign-ins/<id>/cancel` path names: 32 hex digits.
fn sign_in_cancelled_in(path: &str) -> Option<&str> {
    path.strip_prefix("/api/sign-ins/")
        .and_then(|rest| rest.strip_suffix("/cancel"))
        .filter(|id| id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

/// The provider kind a `/signin/<kind>/done` path names.
fn sign_in_done_in(path: &str) -> Option<&str> {
    path.strip_prefix("/signin/")
        .and_then(|rest| rest.strip_suffix("/done"))
        .filter(|kind| !kind.is_empty() && !kind.contains('/'))
}

/// The provider kind a `/signin/<kind>/return` path names.
fn sign_in_return_in(path: &str) -> Option<&str> {
    path.strip_prefix("/signin/")
        .and_then(|rest| rest.strip_suffix("/return"))
        .filter(|kind| !kind.is_empty() && !kind.contains('/'))
}

async fn route(app: &Arc<App>, request: Request<Incoming>) -> Reply {
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
    let query = request.uri().query().unwrap_or_default().to_owned();
    if method == Method::GET
        && path.starts_with("/api/")
        && path != DISCOVERY_PATH
        && let Err(refused) = same_origin_read(app, headers)
    {
        return refused.reply();
    }
    match (&method, path.as_str()) {
        (&Method::GET | &Method::OPTIONS, DISCOVERY_PATH) => {
            discovery_reply(app, method == Method::OPTIONS, browser_evidence(headers))
        }
        (&Method::GET, page) if manager::is_page_path(page) => asset(manager::PAGE),
        (&Method::GET, file) if manager::asset(file).is_some() => {
            manager::asset(file).map_or_else(not_found, asset)
        }
        (&Method::GET, "/api/lobby") => {
            blocking(app, |lobby| {
                json_reply(StatusCode::OK, snapshot_json(lobby))
            })
            .await
        }
        (&Method::GET, "/api/companions") => {
            blocking(app, |lobby| {
                let companions: Vec<Value> = lobby
                    .companions()
                    .iter()
                    .map(|view| companion_json(view, lobby.command()))
                    .collect();
                json_reply(StatusCode::OK, json!({ "companions": companions }))
            })
            .await
        }
        (&Method::GET, "/api/journal") => {
            blocking(app, move |lobby| journal_reply(lobby, &query)).await
        }
        (&Method::GET, FEED_PATH) => events(app, request.headers()),
        (&Method::POST, "/api/companions") => match operator_body(app, request).await {
            Ok(body) => blocking(app, move |lobby| create_companion(lobby, &body)).await,
            Err(refused) => refused.reply(),
        },
        (&Method::PATCH, companion) if companion_in(companion).is_some() => {
            let id = companion_in(companion).unwrap_or_else(|| unreachable!("checked above"));
            match operator_body(app, request).await {
                Ok(body) => blocking(app, move |lobby| rename_companion(lobby, id, &body)).await,
                Err(refused) => refused.reply(),
            }
        }
        (&Method::DELETE, companion) if companion_in(companion).is_some() => {
            let id = companion_in(companion).unwrap_or_else(|| unreachable!("checked above"));
            match operator_body(app, request).await {
                Ok(body) => blocking(app, move |lobby| remove_companion(lobby, id, &body)).await,
                Err(refused) => refused.reply(),
            }
        }
        (&Method::POST, "/api/sign-ins") => match operator_body(app, request).await {
            Ok(body) => {
                let origin = discovery::manager_origin(app.port);
                blocking(app, move |lobby| begin_sign_in(lobby, &origin, &body)).await
            }
            Err(refused) => refused.reply(),
        },
        (&Method::POST, cancelled) if sign_in_cancelled_in(cancelled).is_some() => {
            let id = sign_in_cancelled_in(cancelled)
                .unwrap_or_else(|| unreachable!("checked above"))
                .to_owned();
            match operator_body(app, request).await {
                Ok(_) => {
                    blocking(app, move |lobby| match lobby.cancel_sign_in(&id) {
                        Ok(sign_in) => {
                            json_reply(StatusCode::OK, json!({ "signIn": sign_in_json(&sign_in) }))
                        }
                        Err(refusal) => refusal_reply(&refusal),
                    })
                    .await
                }
                Err(refused) => refused.reply(),
            }
        }
        (&Method::GET, done) if sign_in_done_in(done).is_some() => sign_in_done(&query),
        (&Method::GET, returned) if sign_in_return_in(returned).is_some() => {
            let kind = sign_in_return_in(returned)
                .unwrap_or_else(|| unreachable!("checked above"))
                .to_owned();
            let pairs = query_pairs(&query);
            blocking(app, move |lobby| complete_sign_in(lobby, &kind, pairs)).await
        }
        (&Method::POST, revoked) if credential_revoked_in(revoked).is_some() => {
            let id =
                credential_revoked_in(revoked).unwrap_or_else(|| unreachable!("checked above"));
            match operator_body(app, request).await {
                Ok(_) => {
                    blocking(app, move |lobby| match lobby.revoke_credential(id) {
                        Ok(credential) => json_reply(
                            StatusCode::OK,
                            json!({ "credential": credential_json(&credential) }),
                        ),
                        Err(refusal) => refusal_reply(&refusal),
                    })
                    .await
                }
                Err(refused) => refused.reply(),
            }
        }
        (&Method::POST, "/api/connections") => match operator_body(app, request).await {
            Ok(body) => blocking(app, move |lobby| grant_connection(lobby, &body)).await,
            Err(refused) => refused.reply(),
        },
        (&Method::POST, revoked) if connection_revoked_in(revoked).is_some() => {
            let id =
                connection_revoked_in(revoked).unwrap_or_else(|| unreachable!("checked above"));
            match operator_body(app, request).await {
                Ok(_) => {
                    blocking(app, move |lobby| match lobby.revoke_connection(id) {
                        Ok(connection) => json_reply(
                            StatusCode::OK,
                            json!({ "connection": connection_json(&connection) }),
                        ),
                        Err(refusal) => refusal_reply(&refusal),
                    })
                    .await
                }
                Err(refused) => refused.reply(),
            }
        }
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
            Ok(body) => blocking(app, move |lobby| attach(lobby, &body)).await,
            Err(refused) => refused.reply(),
        },
        (&Method::DELETE, detach_path) if detach_path.starts_with("/mcp/attach/") => {
            if let Err(refused) = refuse_browser(request.headers()) {
                return refused.reply();
            }
            let presented = bearer(request.headers()).map(str::to_owned);
            let id = detach_path["/mcp/attach/".len()..].to_owned();
            blocking(app, move |lobby| detach(lobby, presented.as_deref(), &id)).await
        }
        (&Method::POST, MCP_PATH) => {
            let (attachment, presented) = match attachment_guard(request.headers()) {
                Ok(proof) => proof,
                Err(refused) => return *refused,
            };
            let checked = presented.clone();
            let found = with_lobby(app, move |lobby| lobby.attachment(attachment, &checked)).await;
            match found {
                Ok(Ok(_)) => {}
                Ok(Err(refusal)) => return refusal_reply(&refusal),
                Err(failed) => return *failed,
            }
            match json_body(request).await {
                Ok(body) => {
                    blocking(app, move |lobby| {
                        match lobby.attachment(attachment, &presented) {
                            Ok((attachment, companion)) => {
                                mcp_reply(lobby, attachment, &companion, &body)
                            }
                            Err(refusal) => refusal_reply(&refusal),
                        }
                    })
                    .await
                }
                Err(refused) => refused.reply(),
            }
        }
        (&Method::GET, MCP_PATH) => {
            let (attachment, presented) = match attachment_guard(request.headers()) {
                Ok(proof) => proof,
                Err(refused) => return *refused,
            };
            let changes = with_lobby(app, move |lobby| {
                lobby.catalog_changes(attachment, &presented)
            })
            .await;
            match changes {
                Ok(Ok(changes)) => notices(app, changes),
                Ok(Err(refusal)) => refusal_reply(&refusal),
                Err(failed) => *failed,
            }
        }
        (
            _,
            DISCOVERY_PATH | "/api/lobby" | "/api/companions" | "/api/journal" | FEED_PATH
            | "/api/stop" | "/mcp/attach" | MCP_PATH | "/api/sign-ins" | "/api/connections",
        ) => method_not_allowed(),
        (_, other)
            if manager::is_page_path(other)
                || companion_in(other).is_some()
                || credential_revoked_in(other).is_some()
                || connection_revoked_in(other).is_some()
                || sign_in_cancelled_in(other).is_some()
                || sign_in_done_in(other).is_some()
                || sign_in_return_in(other).is_some() =>
        {
            method_not_allowed()
        }
        _ => not_found(),
    }
}

fn method_not_allowed() -> Reply {
    problem(
        StatusCode::METHOD_NOT_ALLOWED,
        Outcome::Unsupported,
        "That method is not served here.",
    )
}

fn not_found() -> Reply {
    problem(
        StatusCode::NOT_FOUND,
        Outcome::NotFound,
        "Nothing is served at that path.",
    )
}

/// Runs `work` against the lobby on a blocking thread: commands write the
/// state file and the journal before they answer, and a reader may wait
/// while one does.
async fn with_lobby<T: Send + 'static>(
    app: &Arc<App>,
    work: impl FnOnce(&Lobby) -> T + Send + 'static,
) -> Result<T, Box<Reply>> {
    let app = app.clone();
    tokio::task::spawn_blocking(move || work(&app.lobby))
        .await
        .map_err(|_| {
            Box::new(problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                Outcome::Internal,
                Outcome::Internal.sentence(),
            ))
        })
}

/// [`with_lobby`] for work that answers the request itself.
async fn blocking(app: &Arc<App>, work: impl FnOnce(&Lobby) -> Reply + Send + 'static) -> Reply {
    with_lobby(app, work).await.unwrap_or_else(|failed| *failed)
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

/// The read guard: a read under `/api/` from a browser must come from the
/// page's own origin. A request with no browser evidence passes, since a
/// browser sends it for every request another origin makes.
fn same_origin_read(app: &App, headers: &HeaderMap) -> Result<(), Problem> {
    let origin_ok = header_text(headers, header::ORIGIN)
        .is_none_or(|origin| origin == discovery::manager_origin(app.port));
    let fetch_site_ok = header_text(headers, "sec-fetch-site")
        .is_none_or(|site| site == "same-origin" || site == "none");
    if origin_ok && fetch_site_ok {
        Ok(())
    } else {
        Err(Problem(
            StatusCode::FORBIDDEN,
            Outcome::Refused,
            "The lobby is read by the manager page itself.",
        ))
    }
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

/// The guard for frames: no browser evidence, a named attachment and a token;
/// whether the token is that attachment's own is the lobby's to say.
fn attachment_guard(headers: &HeaderMap) -> Result<(AttachmentId, String), Box<Reply>> {
    refuse_browser(headers).map_err(|refused| Box::new(refused.reply()))?;
    let Some(presented) = bearer(headers) else {
        return Err(Box::new(problem(
            StatusCode::UNAUTHORIZED,
            Outcome::Refused,
            "Every frame needs its attachment's token.",
        )));
    };
    let Some(attachment) = header_text(headers, ATTACHMENT_HEADER).and_then(AttachmentId::parse)
    else {
        return Err(Box::new(problem(
            StatusCode::NOT_FOUND,
            Outcome::NotFound,
            "No such attachment is live; attach again.",
        )));
    };
    Ok((attachment, presented.to_owned()))
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

/// The lobby as the manager page starts from it.
fn snapshot_json(lobby: &Lobby) -> Value {
    let Snapshot {
        companions,
        credentials,
        connections,
        sign_ins,
        store_id,
        epoch,
        position,
        journal_healthy,
        journal_high_water,
    } = lobby.snapshot();
    json!({
        "storeId": store_id,
        "feed": { "epoch": epoch, "position": position },
        "companions": companions
            .iter()
            .map(|view| companion_json(view, lobby.command()))
            .collect::<Vec<_>>(),
        "credentials": credentials.iter().map(credential_json).collect::<Vec<_>>(),
        "connections": connections.iter().map(connection_json).collect::<Vec<_>>(),
        "signIns": sign_ins.iter().map(sign_in_json).collect::<Vec<_>>(),
        "kinds": lobby
            .credential_kinds()
            .iter()
            .map(ProviderKind::as_str)
            .collect::<Vec<_>>(),
        "journal": { "healthy": journal_healthy, "highWater": journal_high_water },
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateCompanion {
    moniker: String,
    display_name: Option<String>,
}

fn create_companion(lobby: &Lobby, body: &[u8]) -> Reply {
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
    match lobby.create_companion(&request.moniker, &display_name) {
        Ok(view) => {
            trace::line(&format!("companion {} created", view.record.companion.id));
            json_reply(
                StatusCode::CREATED,
                json!({ "companion": companion_json(&view, lobby.command()) }),
            )
        }
        Err(refusal) => refusal_reply(&refusal),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RenameCompanion {
    expected_revision: u64,
    moniker: Option<String>,
    display_name: Option<String>,
}

fn rename_companion(lobby: &Lobby, id: CompanionId, body: &[u8]) -> Reply {
    let Ok(request) = serde_json::from_slice::<RenameCompanion>(body) else {
        return problem(
            StatusCode::BAD_REQUEST,
            Outcome::InvalidInput,
            "Send {\"expectedRevision\": ..., \"moniker\": ..., \"displayName\": ...} and nothing else.",
        );
    };
    let rename = Rename {
        moniker: request.moniker.as_deref(),
        display_name: request.display_name.as_deref(),
    };
    match lobby.rename_companion(id, request.expected_revision, rename) {
        Ok(view) => {
            trace::line(&format!("companion {id} renamed"));
            json_reply(
                StatusCode::OK,
                json!({ "companion": companion_json(&view, lobby.command()) }),
            )
        }
        Err(refusal) => refusal_reply(&refusal),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RemoveCompanion {
    expected_revision: u64,
    #[serde(default)]
    cascade: bool,
}

fn remove_companion(lobby: &Lobby, id: CompanionId, body: &[u8]) -> Reply {
    let Ok(request) = serde_json::from_slice::<RemoveCompanion>(body) else {
        return problem(
            StatusCode::BAD_REQUEST,
            Outcome::InvalidInput,
            "Send {\"expectedRevision\": ..., \"cascade\": ...} and nothing else.",
        );
    };
    match lobby.remove_companion(id, request.expected_revision, request.cascade) {
        Ok(ended) => {
            trace::line(&format!(
                "companion {id} removed; {ended} attachment(s) ended"
            ));
            json_reply(
                StatusCode::OK,
                json!({ "removed": id.to_string(), "endedAttachments": ended }),
            )
        }
        Err(refusal) => refusal_reply(&refusal),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BeginSignIn {
    companion: String,
    kind: String,
    hint: String,
}

fn begin_sign_in(lobby: &Lobby, origin: &str, body: &[u8]) -> Reply {
    let Some((request, companion)) = serde_json::from_slice::<BeginSignIn>(body)
        .ok()
        .and_then(|request| CompanionId::parse(&request.companion).map(|id| (request, id)))
    else {
        return problem(
            StatusCode::BAD_REQUEST,
            Outcome::InvalidInput,
            "Send {\"companion\": <id>, \"kind\": ..., \"hint\": ...} and nothing else.",
        );
    };
    match lobby.begin_sign_in(companion, &request.kind, &request.hint, origin) {
        Ok((sign_in, start)) => json_reply(
            StatusCode::CREATED,
            json!({ "signIn": sign_in_json(&sign_in), "url": start.url }),
        ),
        Err(refusal) => refusal_reply(&refusal),
    }
}

/// Completes a sign-in from what the provider sent back, then sends the
/// browser straight on to the page that says how it ended, so the code and
/// the `state` leave the address bar and the history at once. The redirect
/// carries only the outcome's wire name.
fn complete_sign_in(lobby: &Lobby, kind: &str, pairs: Vec<(String, String)>) -> Reply {
    let outcome = match lobby.complete_sign_in(kind, pairs) {
        Ok(credential) => {
            trace::line(&format!("credential {} signed in", credential.id));
            Outcome::Done
        }
        Err(refusal) => refusal.outcome,
    };
    let mut reply = empty_reply(StatusCode::SEE_OTHER);
    if let Ok(value) =
        HeaderValue::from_str(&format!("/signin/{kind}/done?outcome={}", outcome.code()))
    {
        reply.headers_mut().insert(header::LOCATION, value);
    }
    reply
}

/// The page that says how a sign-in ended, from the outcome's wire name
/// alone; the manager page says more. It loads nothing.
fn sign_in_done(query: &str) -> Reply {
    let outcome = query_pairs(query)
        .into_iter()
        .find(|(name, _)| name == "outcome")
        .map(|(_, value)| value);
    let (status, said) = match outcome.as_deref() {
        Some("done") => (
            StatusCode::OK,
            "The account is signed in. You can close this tab and return to the manager page.",
        ),
        Some("not_found") => (
            StatusCode::NOT_FOUND,
            "Nothing was signed in: the lobby was not waiting for this sign-in. It may have been cancelled, expired or already finished; the manager page says which.",
        ),
        Some("conflict") => (
            StatusCode::CONFLICT,
            "Nothing was signed in: that account is signed in for another companion, or this companion already holds an account of this kind.",
        ),
        Some("temporarily_unavailable" | "rate_limited") => (
            StatusCode::SERVICE_UNAVAILABLE,
            "Nothing was signed in: the provider could not be reached, or asked the lobby to slow down. Try again from the manager page.",
        ),
        Some("refused" | "invalid_input") => (
            StatusCode::FORBIDDEN,
            "Nothing was signed in: the provider's answer did not prove the sign-in the lobby started. Start it again from the manager page.",
        ),
        _ => (
            StatusCode::FORBIDDEN,
            "Nothing was signed in. The trace log in the data directory says why.",
        ),
    };
    let page = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Companion Lobby</title></head><body><p>{}</p></body></html>",
        html_escape(said)
    );
    let mut reply = with_hygiene(Response::new(Full::new(Bytes::from(page)).boxed()), status);
    let headers = reply.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'"),
    );
    reply
}

/// The pairs of a query string, percent-decoded, `+` read as a space.
fn query_pairs(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(name), percent_decode(value))
        })
        .collect()
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let hex = |byte: u8| (byte as char).to_digit(16);
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                if let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2])) {
                    out.push((high * 16 + low) as u8);
                    index += 3;
                    continue;
                }
                out.push(b'%');
            }
            b'+' => out.push(b' '),
            byte => out.push(byte),
        }
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GrantConnection {
    companion: String,
    credential: String,
    service: String,
    address: String,
    label: String,
}

fn grant_connection(lobby: &Lobby, body: &[u8]) -> Reply {
    let Some((request, companion, credential)) = serde_json::from_slice::<GrantConnection>(body)
        .ok()
        .and_then(|request| {
            let companion = CompanionId::parse(&request.companion)?;
            let credential = CredentialId::parse(&request.credential)?;
            Some((request, companion, credential))
        })
    else {
        return problem(
            StatusCode::BAD_REQUEST,
            Outcome::InvalidInput,
            "Send {\"companion\", \"credential\", \"service\", \"address\", \"label\"} and nothing else.",
        );
    };
    let grant = Grant {
        companion,
        credential,
        service: &request.service,
        address: &request.address,
        label: &request.label,
    };
    match lobby.grant_connection(grant) {
        Ok(connection) => {
            trace::line(&format!("connection {} established", connection.id));
            json_reply(
                StatusCode::CREATED,
                json!({ "connection": connection_json(&connection) }),
            )
        }
        Err(refusal) => refusal_reply(&refusal),
    }
}

fn journal_reply(lobby: &Lobby, query: &str) -> Reply {
    let mut before = None;
    let mut limit = PAGE_DEFAULT;
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        match pair.split_once('=') {
            Some(("before", cursor)) if !cursor.is_empty() => before = Some(cursor.to_owned()),
            Some(("limit", count)) => match count.parse::<usize>() {
                Ok(count) => limit = count,
                Err(_) => {
                    return problem(
                        StatusCode::BAD_REQUEST,
                        Outcome::InvalidInput,
                        "limit is a number of entries.",
                    );
                }
            },
            _ => {
                return problem(
                    StatusCode::BAD_REQUEST,
                    Outcome::InvalidInput,
                    "The journal takes before and limit, nothing else.",
                );
            }
        }
    }
    match lobby.journal_page(before.as_deref(), limit) {
        Ok(page) => json_reply(
            StatusCode::OK,
            json!({ "entries": page.entries, "upper": page.upper, "next": page.next }),
        ),
        Err(PageProblem::Cursor) => problem(
            StatusCode::CONFLICT,
            Outcome::Conflict,
            "That journal cursor belongs to another journal; reload the journal.",
        ),
        Err(PageProblem::Io(error)) => {
            trace::line(&format!("the journal could not be read: {error}"));
            problem(
                StatusCode::SERVICE_UNAVAILABLE,
                Outcome::TemporarilyUnavailable,
                "The journal could not be read; try again.",
            )
        }
    }
}

/// The live feed as server-sent events. A page that names no position, or
/// one this feed cannot continue, first gets a `reset` item and reloads the
/// lobby; then every item follows as it is published. A page that falls
/// behind gets a `reset` and the stream ends, so it reconnects and reloads.
fn events(app: &Arc<App>, headers: &HeaderMap) -> Reply {
    if app.feed_clients.fetch_add(1, Ordering::AcqRel) >= FEED_CLIENT_LIMIT {
        app.feed_clients.fetch_sub(1, Ordering::AcqRel);
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            Outcome::TemporarilyUnavailable,
            "Too many live feeds are open; close a manager page and reload.",
        );
    }
    let slot = FeedSlot(app.clone());
    let subscription = app.lobby.subscribe(header_text(headers, "last-event-id"));
    let (sender, frames) = mpsc::channel::<Bytes>(64);
    let mut stopped = app.stopped.subscribe();
    let app = app.clone();
    tokio::spawn(async move {
        let _slot = slot;
        let feed = app.lobby.feed();
        let mut first = vec![manager::retry_frame()];
        if let Some(reason) = subscription.reset {
            first.push(manager::reset_frame(reason));
        }
        first.extend(
            subscription
                .replay
                .iter()
                .map(|item| manager::item_frame(&feed.id_of(item), &item.data)),
        );
        for frame in first {
            if !deliver(&sender, &mut stopped, frame).await {
                return;
            }
        }
        let mut receiver = subscription.receiver;
        loop {
            let frame = tokio::select! {
                item = receiver.recv() => match item {
                    Ok(item) => manager::item_frame(&feed.id_of(&item), &item.data),
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        deliver(&sender, &mut stopped, manager::reset_frame("behind")).await;
                        return;
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                },
                () = tokio::time::sleep(FEED_KEEPALIVE) => manager::keepalive_frame(),
                () = stop_requested(&mut stopped) => return,
                // The page went away: its place is free at once, not at the
                // next keepalive.
                () = sender.closed() => return,
            };
            if !deliver(&sender, &mut stopped, frame).await {
                return;
            }
        }
    });
    let mut reply = with_hygiene(Response::new(FeedBody(frames).boxed()), StatusCode::OK);
    reply.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    reply
}

/// Hands one frame to the feed's body, unless the daemon stops first: a page
/// that stops reading fills the channel, and a stop never waits on it.
/// Whether the frame went out.
async fn deliver(
    sender: &mpsc::Sender<Bytes>,
    stopped: &mut watch::Receiver<bool>,
    frame: Bytes,
) -> bool {
    tokio::select! {
        sent = sender.send(frame) => sent.is_ok(),
        () = stop_requested(stopped) => false,
    }
}

/// Resolves once a stop is requested and the connection serves the live
/// feed, in either order; never for any other connection, which the drain
/// awaits.
async fn feed_abandoned(
    serves_feed: &mut watch::Receiver<bool>,
    stopped: &mut watch::Receiver<bool>,
) {
    stop_requested(stopped).await;
    // The connection holds the sender, so it is there while this runs.
    let _ = serves_feed.wait_for(|&feed| feed).await;
}

/// Resolves once a stop is requested, at once when one already was.
async fn stop_requested(stopped: &mut watch::Receiver<bool>) {
    // The sender lives as long as the daemon; were it gone, so is the feed.
    let _ = stopped.wait_for(|&stop| stop).await;
}

/// An attachment's notice stream: a comment when it opens, then one
/// `notifications/tools/list_changed` frame each time the attachment's tools
/// change, with a keepalive comment while nothing does. It ends when the
/// attachment ends, the reader goes away or the daemon stops.
fn notices(app: &Arc<App>, mut changes: watch::Receiver<u64>) -> Reply {
    let (sender, frames) = mpsc::channel::<Bytes>(16);
    let mut stopped = app.stopped.subscribe();
    tokio::spawn(async move {
        if !deliver(&sender, &mut stopped, Bytes::from_static(b": open\n\n")).await {
            return;
        }
        loop {
            let frame = tokio::select! {
                changed = changes.changed() => match changed {
                    Ok(()) => Bytes::from(format!("data: {}\n\n", mcp::list_changed())),
                    // The attachment ended.
                    Err(_) => return,
                },
                () = tokio::time::sleep(FEED_KEEPALIVE) => manager::keepalive_frame(),
                () = stop_requested(&mut stopped) => return,
                () = sender.closed() => return,
            };
            if !deliver(&sender, &mut stopped, frame).await {
                return;
            }
        }
    });
    let mut reply = with_hygiene(Response::new(FeedBody(frames).boxed()), StatusCode::OK);
    reply.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    reply
}

/// Frees a live feed's place when its stream ends, however it ends.
struct FeedSlot(Arc<App>);

impl Drop for FeedSlot {
    fn drop(&mut self) {
        self.0.feed_clients.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AttachRequest {
    companion: String,
}

fn attach(lobby: &Lobby, body: &[u8]) -> Reply {
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
    match lobby.attach(&request.companion, token.clone()) {
        Ok((attachment, companion)) => {
            trace::line(&format!(
                "attachment {} attached as companion {}; {} live",
                attachment.id,
                companion.id,
                lobby.attachment_count()
            ));
            json_reply(
                StatusCode::CREATED,
                json!({
                    "attachment": attachment.id.to_string(),
                    "token": token.expose(),
                    "companion": {
                        "id": companion.id.to_string(),
                        "moniker": companion.moniker.as_str(),
                        "displayName": companion.display_name.as_str(),
                    },
                }),
            )
        }
        Err(refusal) => refusal_reply(&refusal),
    }
}

fn detach(lobby: &Lobby, presented: Option<&str>, id: &str) -> Reply {
    let Some(presented) = presented else {
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
    match lobby.detach(id, presented) {
        Ok(()) => {
            trace::line(&format!(
                "attachment {id} detached; {} live",
                lobby.attachment_count()
            ));
            empty_reply(StatusCode::NO_CONTENT)
        }
        Err(refusal) => refusal_reply(&refusal),
    }
}

/// The lobby, as one attachment's tools.
struct Attached<'a> {
    lobby: &'a Lobby,
    attachment: Attachment,
}

impl mcp::Toolbox for Attached<'_> {
    fn reached(&self) -> Vec<Reached> {
        self.lobby.reached(self.attachment.companion)
    }

    fn call(
        &self,
        name: &str,
        arguments: serde_json::Map<String, Value>,
    ) -> Result<ToolResult, CallRefusal> {
        self.lobby.call_tool(&self.attachment, name, arguments)
    }
}

fn mcp_reply(lobby: &Lobby, attachment: Attachment, companion: &Companion, body: &[u8]) -> Reply {
    let Ok(message) = serde_json::from_slice::<Value>(body) else {
        return json_reply(
            StatusCode::OK,
            mcp::rpc_error(&Value::Null, mcp::PARSE_ERROR, "Parse error"),
        );
    };
    let tools = Attached { lobby, attachment };
    match mcp::handle(companion, &tools, &message) {
        Some(answer) => json_reply(StatusCode::OK, answer),
        None => empty_reply(StatusCode::ACCEPTED),
    }
}

fn refusal_reply(refusal: &Refusal) -> Reply {
    let status = match refusal.outcome {
        Outcome::NotFound => StatusCode::NOT_FOUND,
        Outcome::Conflict => StatusCode::CONFLICT,
        Outcome::InvalidInput => StatusCode::BAD_REQUEST,
        Outcome::TemporarilyUnavailable => StatusCode::SERVICE_UNAVAILABLE,
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

/// One of the manager page's embedded files, with a content security policy
/// that lets the page load only its own script and stylesheet and talk only
/// to its own origin.
fn asset((body, content_type): (&'static str, &'static str)) -> Reply {
    let mut reply = with_hygiene(
        Response::new(Full::new(Bytes::from_static(body.as_bytes())).boxed()),
        StatusCode::OK,
    );
    let headers = reply.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(manager::CONTENT_POLICY),
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
    let mut reply = with_hygiene(Response::new(Full::new(Bytes::from(bytes)).boxed()), status);
    reply.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    reply
}

fn empty_reply(status: StatusCode) -> Reply {
    with_hygiene(Response::new(Full::new(Bytes::new()).boxed()), status)
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
    fn credential_connection_and_return_paths_name_what_they_act_on() {
        let credential = CredentialId::from_u128(7);
        let connection = ConnectionId::from_u128(8);
        assert_eq!(
            credential_revoked_in(&format!("/api/credentials/{credential}/revoke")),
            Some(credential)
        );
        assert_eq!(
            credential_revoked_in(&format!("/api/credentials/{credential}")),
            None
        );
        assert_eq!(
            connection_revoked_in(&format!("/api/connections/{connection}/revoke")),
            Some(connection)
        );
        assert_eq!(connection_revoked_in("/api/connections/x/revoke"), None);
        assert_eq!(
            sign_in_return_in("/signin/passbook/return"),
            Some("passbook")
        );
        assert_eq!(sign_in_return_in("/signin//return"), None);
        let id = "0123456789abcdef0123456789abcdef";
        assert_eq!(
            sign_in_cancelled_in(&format!("/api/sign-ins/{id}/cancel")),
            Some(id)
        );
        assert_eq!(sign_in_cancelled_in("/api/sign-ins/x/cancel"), None);
        assert_eq!(sign_in_cancelled_in(&format!("/api/sign-ins/{id}")), None);
        assert_eq!(sign_in_return_in("/signin/a/b/return"), None);
    }

    #[test]
    fn query_pairs_are_decoded() {
        assert_eq!(
            query_pairs("state=a%2Fb&code=c+d&empty=&bare"),
            [
                ("state".to_owned(), "a/b".to_owned()),
                ("code".to_owned(), "c d".to_owned()),
                ("empty".to_owned(), String::new()),
                ("bare".to_owned(), String::new()),
            ]
        );
        assert_eq!(percent_decode("%zz%4"), "%zz%4");
    }

    #[test]
    fn companion_paths_name_one_identifier() {
        let id = CompanionId::from_u128(7);
        assert_eq!(companion_in(&format!("/api/companions/{id}")), Some(id));
        assert_eq!(companion_in("/api/companions/ada"), None);
        assert_eq!(companion_in(&format!("/api/companions/{id}/x")), None);
        assert_eq!(companion_in("/api/companions"), None);
    }

    #[test]
    fn every_reply_carries_the_hygiene_headers() {
        for reply in [
            json_reply(StatusCode::OK, json!({})),
            empty_reply(StatusCode::NO_CONTENT),
            redirect("http://127.0.0.1:1/"),
            problem(StatusCode::FORBIDDEN, Outcome::Refused, "no"),
            asset(manager::PAGE),
        ] {
            let headers = reply.headers();
            assert_eq!(headers[header::CACHE_CONTROL], "no-store");
            assert_eq!(headers[header::REFERRER_POLICY], "no-referrer");
            assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
            assert!(!headers.contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN));
        }
    }
}
