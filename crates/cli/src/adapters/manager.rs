//! The operator spoke: a loopback-only web page for companion and enrollment stewardship,
//! on a fixed port (5219 — a stable URL; `TANGENT_CONNECTOR_PORT` or `--port` names
//! another fixed port). Hand-rolled minimal HTTP/1.1 in the house
//! style — request line, headers and a Content-Length body under an 8 KiB header cap
//! and a 1 MiB body cap, GET/POST only, `Connection: close`, a 30 s read timeout,
//! JSON-only `/api/*` bodies (any other encoding is refused on every surface). The
//! one deliberately cross-origin route is `GET/OPTIONS /api/discovery`: it discloses
//! only the connector product/version and this loopback origin, so a Tangent page can
//! decide whether to offer its local companion manager without exposing companions or
//! credentials. The pages are inert embedded strings; every other `/api/*` JSON call
//! crosses
//! the SAME hub as the CLI and MCP intakes (attribution channel `Operator`). The
//! `/bind/{companionId}/{provider}` route IS the atproto OAuth bind, with no
//! interstitial: the GET immediately starts the flow — the default authorization
//! server, or `?handle=` discovery for self-hosted PDSes — and answers the 302 to the
//! provider's authorize page, whose own UI handles account selection and sign-in. The
//! provider's loopback redirect (root path only, per the public local-client profile)
//! lands on `/` with code+state+iss and renders the result page. No state-changing
//! `/bind` POST route exists at all; a cross-origin GET only ever starts a flow the
//! operator sees at the provider. Two rules keep the rest of the surface local: every
//! request must name a loopback `Host`, which refuses a rebound public hostname before
//! it reaches a route, and every `/api/*` POST must carry `application/json` with an
//! `Origin` matching that host and, when the browser sends one, `Sec-Fetch-Site:
//! same-origin` — so a `text/plain` write from another site, which crosses origins
//! without a preflight, is refused. Those rules and the structural guarantees (loopback
//! bind, method/caps discipline) carry the trust: the operator is the trust root and a
//! local process can read state.json directly anyway, so the page carries no
//! interactive token. Nothing but the startup banner is ever printed to stdout.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::adapters::lockfile::DataDirLock;
use crate::adapters::tray;
use crate::application::hub::ConnectorHub;
use companion_core::domain::events::DomainEvent;
use companion_core::domain::companion::CallerId;
use crate::{build_hub, data_directory};

const HEADER_LIMIT: usize = 8 * 1024;
const BODY_LIMIT: usize = 1024 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// Settling pause after a failed accept, so a persistent socket-level failure cannot
/// spin the loop hot.
const ACCEPT_ERROR_PAUSE: Duration = Duration::from_millis(100);
/// Concurrent SSE feed clients this server will hold open. A small bound: each pins a
/// connection thread, and one operator needs at most a couple of tabs.
const SSE_CLIENT_LIMIT: usize = 4;
/// SSE keepalive cadence: a comment frame that also proves the peer is still there.
const SSE_KEEPALIVE: Duration = Duration::from_secs(15);
const INDEX_HTML: &str = include_str!("manager.html");
const MANAGER_STYLE: &str = include_str!("manager.css");

// Both products draw the same bounded ASCII atmosphere. Embed its assets so the local
// manager remains one self-contained executable.
fn atmosphere_assets() -> String {
    format!("<style>{}</style><script>{}</script><script>{}</script>",
        include_str!("../../assets/atmosphere.css"),
        include_str!("../../assets/ascii-scenes.js"),
        include_str!("../../assets/atmosphere.js"))
}

fn manager_index() -> String {
    INDEX_HTML.replace("/* TANGENT_MANAGER_STYLE */", MANAGER_STYLE)
        .replace("<!-- TANGENT_ATMOSPHERE -->", &atmosphere_assets())
}
/// The companion manager's fixed default port: a stable URL any Connect
/// can name. `--port` or `TANGENT_CONNECTOR_PORT` names another fixed port; there is no
/// random port.
pub const DEFAULT_PORT: u16 = 5219;
/// The deterministic page URL that goes with [`DEFAULT_PORT`].
pub const DEFAULT_PAGE_URL: &str = "http://127.0.0.1:5219/";

/// How the companion page names itself in its discovery document. A probe matches this
/// before treating a listener as the page, so an unrelated local service on the same
/// port is never mistaken for it.
pub const CONNECTOR_PRODUCT: &str = "tangent-space-connector";
/// The one bind provider this connector serves today.
const BIND_PROVIDER_ATPROTO: &str = "atproto";

/// The port the companion manager serves on: the `--port` flag wins, then
/// `TANGENT_CONNECTOR_PORT`, then the fixed default. Port 0 is refused.
pub fn resolve_manager_port(flag: Option<u16>) -> Result<u16, String> {
    port_from(flag, std::env::var("TANGENT_CONNECTOR_PORT").ok().as_deref())
}

/// The pure decision behind [`resolve_manager_port`], so the discipline is assertable
/// without touching the process environment.
pub fn port_from(flag: Option<u16>, environment: Option<&str>) -> Result<u16, String> {
    let port = match (flag, environment.map(str::trim)) {
        (Some(port), _) => port,
        (None, None | Some("")) => DEFAULT_PORT,
        (None, Some(value)) => value
            .parse::<u16>()
            .map_err(|_| format!("TANGENT_CONNECTOR_PORT must be a port number, not '{value}'"))?,
    };
    if port == 0 {
        return Err("the companion manager needs a fixed port; 0 would pick a random one. \
            Name one from 1 to 65535 with --port or TANGENT_CONNECTOR_PORT."
            .to_string());
    }
    Ok(port)
}

/// Binds the manager listener on loopback. An in-use fixed port is an honest refusal
/// naming what is known about the holder: the data-directory lock's record and the
/// page URL the durable state last recorded (the lockfile covers one connector
/// process; the bind conflict may be any listener on that port).
pub fn bind_listener(data_dir: &std::path::Path, port: u16) -> Result<TcpListener, String> {
    match TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => Ok(listener),
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => Err(format!(
            "cannot host the companion manager on 127.0.0.1:{port}: another process is already listening there \n             (state lock: {}; last recorded companion manager: {}). \n             Stop whatever holds the port, or choose another with --port or TANGENT_CONNECTOR_PORT.",
            crate::adapters::lockfile::holder_of(data_dir),
            recorded_page_url(data_dir).unwrap_or_else(|| "none recorded".to_string()),
        )),
        Err(error) => Err(format!("cannot bind the manager listener on 127.0.0.1:{port}: {error}")),
    }
}

/// The companion manager URL durable state last recorded, when state is readable at all.
fn recorded_page_url(data_dir: &std::path::Path) -> Option<String> {
    crate::adapters::store::StateStore::open(data_dir).ok().and_then(|store| store.manager_page_url())
}

/// Entry point of the `operator` verb. Owns stdout for its banner; the MCP edge is a
/// separate process and never runs here.
pub fn manager(rest: &[String]) -> i32 {
    let mut port: Option<u16> = None;
    let mut open_browser = true;
    let mut force = false;
    let mut remaining = rest.iter();
    while let Some(argument) = remaining.next() {
        match argument.as_str() {
            "--port" => match remaining.next().and_then(|value| value.parse().ok()) {
                Some(value) => port = Some(value),
                None => {
                    eprintln!("--port needs a number");
                    return 1;
                }
            },
            "--no-open" => open_browser = false,
            "--force" => force = true,
            other => {
                eprintln!("unknown operator option {other}");
                return 1;
            }
        }
    }
    let data_dir = data_directory();
    // Long-running verbs are mutually exclusive per data directory: whole-file state
    // saves from two processes would clobber each other.
    let lock = match DataDirLock::acquire(&data_dir, force) {
        Ok(lock) => lock,
        Err(error) => {
            eprintln!("{error}");
            return 4;
        }
    };
    let hub = match build_hub(CallerId("manager".into()), data_dir.clone()) {
        Ok(hub) => hub,
        Err(error) => {
            eprintln!("cannot open connector state: {error}");
            return 4;
        }
    };
    let port = match resolve_manager_port(port) {
        Ok(port) => port,
        Err(error) => {
            eprintln!("{error}");
            return 1;
        }
    };
    // Loopback only: the listener binds 127.0.0.1, never anything reachable off-machine.
    // The default port is fixed (stable URL); an in-use port is a refusal naming the holder.
    let listener = match bind_listener(&data_dir, port) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("{error}");
            return 4;
        }
    };
    let url = format!("http://127.0.0.1:{port}/");
    println!("Tangent connector companion manager: {url}");
    println!("The connector records this address in its state so any Connect can pop this page.");
    // The page URL is recorded in memory AND durable state: a Connect in any
    // process — the CLI one-shots included — pops this page at the sign-in anchor.
    hub.announce_manager_page(&url);
    if open_browser {
        hub.open_page(&url);
    }
    // The tray's Quit releases the data-directory lock and clears the recorded page
    // URL before exiting the process.
    let quit_lock = lock.clone();
    let quit_hub = hub.clone();
    tray::spawn(
        hub.clone(),
        url,
        Box::new(move || {
            quit_hub.clear_persisted_manager_page();
            quit_lock.release();
            std::process::exit(0);
        }),
    );
    let serving = listener.try_clone().expect("clone listener");
    let serve_hub = hub.clone();
    let server = std::thread::Builder::new()
        .name("tangent-operator".into())
        .spawn(move || serve(serving, serve_hub))
        .expect("manager server thread");
    // The server thread owns the listener; the tray's Quit exits the process. Either
    // path releases the lock (Drop here, release() in the quit hook) and clears the
    // recorded page URL.
    let _ = server.join();
    hub.clear_persisted_manager_page();
    0
}

/// The accept loop: one short-lived connection thread per request (`Connection: close`);
/// an SSE feed connection is the one deliberate exception and stays open. A failed
/// accept pauses briefly and continues — a transient socket-level error must not end the
/// verb. Shared with the in-process serve mode and tests, which pass their own listener.
pub fn serve(listener: TcpListener, hub: Arc<ConnectorHub>) {
    let sse_clients = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            Err(_) => {
                std::thread::sleep(ACCEPT_ERROR_PAUSE);
                continue;
            }
        };
        let hub = hub.clone();
        let sse_clients = sse_clients.clone();
        std::thread::spawn(move || serve_connection(stream, hub, sse_clients));
    }
}

/// Why a capped read stopped.
#[derive(Debug)]
enum CappedError {
    /// The line exceeded the byte budget while still arriving.
    OverLimit,
    /// The stream failed or the peer vanished.
    Io,
}

/// Reads one line incrementally, refusing to buffer beyond `cap`: the limit bites while
/// bytes arrive, not after, and only the line itself (newline included) is consumed —
/// following request bytes stay queued for the next read. Returns the raw byte count
/// consumed; `Ok(None)` is a clean end of stream before any byte.
fn read_line_capped(reader: &mut impl BufRead, buffer: &mut String, cap: usize) -> Result<Option<usize>, CappedError> {
    buffer.clear();
    let mut total = 0usize;
    loop {
        let (consumed, hit_newline) = {
            let available = match reader.fill_buf() {
                Ok(available) => available,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(CappedError::Io),
            };
            if available.is_empty() {
                return if total == 0 { Ok(None) } else { Ok(Some(total)) };
            }
            match available.iter().position(|byte| *byte == b'\n') {
                Some(index) => {
                    total += index + 1;
                    if total > cap {
                        return Err(CappedError::OverLimit);
                    }
                    buffer.push_str(&String::from_utf8_lossy(&available[..index]));
                    (index + 1, true)
                }
                None => {
                    total += available.len();
                    if total > cap {
                        return Err(CappedError::OverLimit);
                    }
                    buffer.push_str(&String::from_utf8_lossy(available));
                    (available.len(), false)
                }
            }
        };
        reader.consume(consumed);
        if hit_newline {
            return Ok(Some(total));
        }
    }
}

/// One request per connection (`Connection: close`), parsed under the caps. The request
/// line and every header are read incrementally against the 8 KiB header budget, so an
/// unterminated peer cannot grow memory first and fail later; a mid-request IO error
/// just drops that connection — the verb keeps serving. `GET /api/events` is the one
/// exception: it becomes a held-open SSE feed.
fn serve_connection(stream: TcpStream, hub: Arc<ConnectorHub>, sse_clients: Arc<AtomicUsize>) {
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    // The loopback root this server answers on — the OAuth bind's redirect target.
    let root_url = stream
        .local_addr()
        .map(|address| format!("http://127.0.0.1:{}/", address.port()))
        .unwrap_or_else(|_| DEFAULT_PAGE_URL.to_string());
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(clone) => clone,
        Err(_) => return,
    });
    let mut writer = stream;
    let mut request_line = String::new();
    match read_line_capped(&mut reader, &mut request_line, HEADER_LIMIT) {
        Ok(Some(_)) => {}
        Ok(None) | Err(CappedError::Io) => return,
        Err(CappedError::OverLimit) => {
            let _ = respond(&mut writer, 400, problem_json("headers_too_large", "request line exceeds 8 KiB"), None);
            return;
        }
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();
    if method.is_empty() || target.is_empty() {
        let _ = respond(&mut writer, 400, problem_json("bad_request", "malformed request line"), None);
        return;
    }
    let request_path = target.split('?').next().unwrap_or_default();
    let discovery_preflight = method == "OPTIONS" && request_path == "/api/discovery";
    if method != "GET" && method != "POST" && !discovery_preflight {
        let _ = respond(&mut writer, 405, problem_json("method_not_allowed", "GET and POST only"), None);
        return;
    }
    let mut content_length: usize = 0;
    let mut content_type = String::new();
    let mut host = String::new();
    let mut origin = String::new();
    let mut fetch_site = String::new();
    let mut budget = HEADER_LIMIT;
    loop {
        let mut header = String::new();
        match read_line_capped(&mut reader, &mut header, budget) {
            Ok(Some(raw)) => {
                budget = budget.saturating_sub(raw);
            }
            Ok(None) | Err(CappedError::Io) => return,
            Err(CappedError::OverLimit) => {
                let _ = respond(&mut writer, 400, problem_json("headers_too_large", "headers exceed 8 KiB"), None);
                return;
            }
        }
        if header.trim().is_empty() {
            break;
        }
        // Header names are case-insensitive; compare one lowercased view.
        let lowered = header.to_ascii_lowercase();
        if let Some(value) = lowered.strip_prefix("content-length:") {
            content_length = value.trim().parse().unwrap_or(0);
        }
        if let Some(value) = lowered.strip_prefix("content-type:") {
            content_type = value.trim().to_string();
        }
        // The three headers the write guard reads. Hosts, schemes and the fetch
        // metadata are all case-insensitive, so the lowercased view compares directly.
        if let Some(value) = lowered.strip_prefix("host:") {
            host = value.trim().to_string();
        }
        if let Some(value) = lowered.strip_prefix("origin:") {
            origin = value.trim().to_string();
        }
        if let Some(value) = lowered.strip_prefix("sec-fetch-site:") {
            fetch_site = value.trim().to_string();
        }
    }
    if content_length > BODY_LIMIT {
        let _ = respond(&mut writer, 413, problem_json("body_too_large", "body exceeds 1 MiB"), None);
        return;
    }
    let mut body_bytes = vec![0u8; content_length];
    if content_length > 0 && reader.read_exact(&mut body_bytes).is_err() {
        return;
    }
    // First rule: this page answers loopback names only. A site that rebinds its
    // own hostname to 127.0.0.1 reaches this port with that hostname in `Host`, so the
    // check keeps every surface — reads included — off the open web. It costs the
    // legitimate page nothing: the browser fills `Host` from the address it opened.
    if !is_loopback_host(&host) {
        let _ = respond(&mut writer, 403, problem_json("forbidden_host", "this page answers loopback hosts only"), None);
        return;
    }
    // Cross-origin discovery is intentionally tiny and inert. It proves only that a
    // compatible connector is listening on this browser's loopback interface. Every
    // companion, enrollment and mutation route remains same-origin and receives no
    // CORS headers.
    if request_path == "/api/discovery" && (method == "GET" || method == "OPTIONS") {
        let _ = respond_discovery(&mut writer, method == "OPTIONS", root_url.trim_end_matches('/'));
        return;
    }
    // The live activity feed (A1): the connection is handed to the streaming handler
    // and never returns here.
    if method == "GET" && target.split('?').next() == Some("/api/events") {
        stream_events(writer, hub, sse_clients);
        return;
    }
    // Second rule: a write comes from the page itself. `text/plain` is a CORS
    // simple request — any site can send one to this port without a preflight — so the
    // JSON surface accepts `application/json` and nothing else, and the same-origin
    // evidence the browser attaches must agree with the host it reached. The server's
    // credential endpoint applies these same three checks, so both sides read alike.
    // `Sec-Fetch-Site` is absent on older browsers and on non-browser callers; when it
    // is present it must say the request never left the page.
    if method == "POST"
        && request_path.starts_with("/api/")
        && (!is_json(&content_type) || origin != format!("http://{host}") || (!fetch_site.is_empty() && fetch_site != "same-origin"))
    {
        let _ = respond(&mut writer, 403, problem_json("cross_site_write", "writes come from the companion page itself"), None);
        return;
    }
    // Bodies are JSON or nothing. No form surface remains anywhere — the bind route is
    // a navigation — so any other encoding classifies as neither and the routes refuse
    // it explicitly, rather than through a misleading JSON parse error.
    let body = if content_length == 0 || is_json(&content_type) {
        RequestBody::Json(serde_json::from_slice(&body_bytes).unwrap_or(Value::Null))
    } else {
        RequestBody::Other
    };
    let response = route(&hub, &method, &target, &body, &root_url);
    let _ = respond(&mut writer, response.0, response.1, response.2.as_deref());
}

/// A request body as these surfaces classify it: JSON (the /api/* surface, and an
/// empty body) or anything else, kept distinct so every route's refusal is explicit
/// rather than a misleading JSON parse error.
enum RequestBody {
    Json(Value),
    Other,
}

/// The loopback names this page answers on, with or without a port. The port is not
/// the rule — reaching us by a loopback name is, and a rebound public hostname never
/// is one.
fn is_loopback_host(host: &str) -> bool {
    let name = match host.strip_prefix('[').and_then(|rest| rest.find(']')) {
        // An IPv6 literal keeps its brackets; only the `:port` after them splits off.
        Some(end) => &host[..end + 2],
        None => host.split(':').next().unwrap_or_default(),
    };
    matches!(name, "127.0.0.1" | "localhost" | "[::1]")
}

/// JSON, whatever parameters follow it (`application/json; charset=utf-8`).
fn is_json(content_type: &str) -> bool {
    content_type.split(';').next().map(str::trim) == Some("application/json")
}

/// The SSE feed (owner addendum): the one deliberate exception to this server's
/// one-response-per-connection shape — the response is held open and written as
/// events arrive. Frames are the existing `DomainEvent` vocabulary serialized as
/// `data:` JSON (the page reads `kind` from the payload). The `ManagerPageReady`
/// event is deliberately skipped: it is journal material, not feed material. A
/// keepalive comment every [`SSE_KEEPALIVE`] keeps intermediaries honest and surfaces
/// a vanished peer as a write error; past [`SSE_CLIENT_LIMIT`] concurrent clients the
/// refusal is an honest 503 JSON problem.
fn stream_events(mut writer: TcpStream, hub: Arc<ConnectorHub>, sse_clients: Arc<AtomicUsize>) {
    if sse_clients.fetch_add(1, Ordering::AcqRel) >= SSE_CLIENT_LIMIT {
        sse_clients.fetch_sub(1, Ordering::AcqRel);
        let _ = respond(&mut writer, 503, problem_json("sse_clients_busy", "too many live activity feeds are open; close one and reload"), None);
        return;
    }
    let _guard = SseSlot { count: sse_clients };
    // Subscribe BEFORE the response head is written: a client that has seen the head
    // can then never miss a later event (the channel buffers in between).
    let receiver = hub.events().subscribe();
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nCache-Control: no-store\r\n\r\n";
    if writer.write_all(head.as_bytes()).is_err() || writer.flush().is_err() {
        return;
    }
    loop {
        match receiver.recv_timeout(SSE_KEEPALIVE) {
            Ok(event) => {
                if matches!(event, DomainEvent::ManagerPageReady { .. }) {
                    continue;
                }
                let data = serde_json::to_string(&event)
                    .unwrap_or_else(|_| "{\"kind\":\"unserializable\"}".to_string());
                let frame = format!("data: {data}\n\n");
                if writer.write_all(frame.as_bytes()).is_err() || writer.flush().is_err() {
                    return;
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if writer.write_all(b": keepalive\n\n").is_err() || writer.flush().is_err() {
                    return;
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Decrements the live SSE client count when the feed ends, however it ends.
struct SseSlot {
    count: Arc<AtomicUsize>,
}

impl Drop for SseSlot {
    fn drop(&mut self) {
        self.count.fetch_sub(1, Ordering::AcqRel);
    }
}

struct ApiResponse(u16, Value, Option<String>);

/// The route table. The embedded page is inert HTML+JS; the `/bind/{companionId}/{provider}`
/// route immediately starts the atproto OAuth flow (any other provider is an honest
/// 404); everything under /api/ is the same local-only trust boundary (loopback bind,
/// GET/POST, caps, JSON bodies).
fn route(hub: &ConnectorHub, method: &str, target: &str, body: &RequestBody, root_url: &str) -> ApiResponse {
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path, query),
        None => (target, ""),
    };
    if !path.starts_with("/api/") {
        return html_routes(hub, method, path, query, body, root_url);
    }
    let RequestBody::Json(body) = body else {
        return ApiResponse(400, problem_json("bad_request", "the API surface speaks JSON only"), None);
    };
    let segments: Vec<&str> = path.trim_start_matches("/api/").split('/').filter(|segment| !segment.is_empty()).collect();
    match (method, segments.as_slice()) {
        ("GET", ["companions"]) => ApiResponse(200, ok_json(json!({ "companions": companion_list(hub) })), None),
        ("POST", ["companions"]) => {
            let handle = body.get("handle").and_then(Value::as_str).unwrap_or_default();
            let display = body.get("displayName").and_then(Value::as_str);
            finish(hub.create_companion(handle, display), |companion| ok_json(json!({ "companion": companion_json(&companion) })))
        }
        ("POST", ["companions", local_id]) => {
            let handle = body.get("handle").and_then(Value::as_str);
            let display = match body.get("displayName") {
                None | Some(Value::Null) => None,
                Some(Value::String(value)) => Some(Some(value.as_str())),
                Some(_) => return ApiResponse(400, problem_json("bad_request", "displayName must be a string or null"), None),
            };
            finish(hub.update_companion(local_id, handle, display), |companion| ok_json(json!({ "companion": companion_json(&companion) })))
        }
        ("POST", ["companions", local_id, "delete"]) => {
            let cascade = body.get("cascade").and_then(Value::as_bool).unwrap_or(false);
            finish(hub.delete_companion(local_id, cascade), |_| ok_json(json!({ "deleted": local_id })))
        }
        ("GET", ["companions", local_id, "enrollments"]) => {
            if hub.companion(local_id).is_none() {
                return ApiResponse(200, blocked_json("unknown_companion", "no companion matches that id"), None);
            }
            let enrollments: Vec<Value> = hub
                .enrollment_inventory()
                .into_iter()
                .filter(|(entry, _)| entry.local_id == *local_id)
                .map(|(entry, available)| enrollment_json(&entry, available))
                .collect();
            ApiResponse(200, ok_json(json!({ "enrollments": enrollments })), None)
        }
        // Enrollment deliberately has no route here (R2): it is a consequence of
        // connecting (the Connect handshake) or an explicit hub/CLI action — the disarm
        // tier — never an operator-page ceremony. The old enroll routes are gone.
        //
        // Binding happens on the /bind route over OAuth, and nowhere else: there is no
        // password path on the page, in the hub or in the CLI.
        ("POST", ["companions", local_id, "atproto", "unbind"]) => {
            finish(hub.unbind_atproto(local_id), |companion| {
                ok_json(json!({ "companion": companion_with_atproto(&companion, hub.atproto_binding(&companion.local_id)) }))
            })
        }
        ("POST", ["enrollments", enrollment_id, "forget"]) => {
            finish(hub.forget_enrollment(enrollment_id), |_| ok_json(json!({ "forgotten": enrollment_id })))
        }
        ("GET", ["server-cards"]) => ApiResponse(200, ok_json(json!({ "servers": hub.refresh_server_cards() })), None),
        ("GET", ["status"]) => {
            let enrollments: Vec<Value> = hub
                .enrollment_statuses()
                .into_iter()
                .map(|status| {
                    json!({
                        "enrollmentId": status.enrollment_id,
                        "companionId": status.companion_local_id,
                        "origin": status.origin,
                        "waiting": status.waiting,
                        "pendingAttention": status.pending_attention,
                        "unresolvedWrites": status.unresolved_writes,
                    })
                })
                .collect();
            ApiResponse(200, ok_json(json!({ "enrollments": enrollments })), None)
        }
        _ => ApiResponse(404, problem_json("not_found", "no such operator API route"), None),
    }
}

/// The human surfaces: the companion manager, the bind route, and the loopback callback
/// that lands on `/` (the atproto local-client redirect rule — the public authorization
/// server only ever redirects to `http://127.0.0.1[:port]/`).
fn html_routes(hub: &ConnectorHub, method: &str, path: &str, query: &str, _body: &RequestBody, root_url: &str) -> ApiResponse {
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').filter(|segment| !segment.is_empty()).collect();
    match (method, segments.as_slice()) {
        ("GET", []) => {
            // The OAuth callback: `state` in the query means an authorization server
            // answered us. Anything else is a plain page load.
            let parameters = parse_query(query);
            if parameters.iter().any(|(name, _)| name == "state") {
                return bind_callback(hub, &parameters, root_url);
            }
            ApiResponse(200, Value::String(manager_index()), None)
        }
        ("GET", ["index.html"]) => ApiResponse(200, Value::String(manager_index()), None),
        ("GET", ["bind", local_id, provider]) => {
            if *provider != BIND_PROVIDER_ATPROTO {
                return not_found_page(&format!(
                    "Unknown bind provider '{provider}' — only '{}' lives here.",
                    BIND_PROVIDER_ATPROTO
                ));
            }
            if hub.companion(local_id).is_none() {
                return not_found_page("No local companion matches that id — open the companion manager and pick one.");
            }
            // No interstitial: this GET is the bind's start. The
            // default authorization server handles account selection and sign-in in
            // its own UI; the answer is the 302 to its authorize page. `?handle=` is
            // the self-hosted escape hatch — it runs the handle→DID→PDS→AS discovery
            // path first, then the same redirect.
            let handle = parse_query(query)
                .iter()
                .find(|(name, _)| name == "handle")
                .map(|(_, value)| value.trim().to_string())
                .filter(|value| !value.is_empty());
            match hub.begin_atproto_bind(local_id, handle.as_deref(), root_url) {
                Ok(authorize_url) => ApiResponse(302, Value::String(String::new()), Some(authorize_url)),
                Err(problem) => bind_problem_page(&problem, local_id),
            }
        }
        // No POST bind route exists (R4): the bind is a navigation, and a cross-origin
        // GET only ever starts a flow the operator sees at the provider. Form posts
        // anywhere outside /api/'s JSON surface fall through to the honest 404.
        _ => not_found_page("only the companion manager, its bind route and /api/* live here"),
    }
}

/// The loopback callback (redirect target of the bind flow): a provider error renders
/// an honest failure naming its code; a code+state+iss triple completes the bind and
/// renders the close-your-tab page. The issuer is mandatory (the hub refuses its
/// absence and any mismatch with the flight's authorization server). The waiting
/// connect (if any) finished by itself inside the hub — this page only reports.
fn bind_callback(hub: &ConnectorHub, parameters: &[(String, String)], root_url: &str) -> ApiResponse {
    let parameter = |name: &str| parameters.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str());
    if let Some(error) = parameter("error") {
        let description = parameter("error_description").unwrap_or_default();
        return bind_result_page(false, &format!("provider_refused: {error}: {description}"));
    }
    let state = parameter("state").unwrap_or_default();
    let code = parameter("code").unwrap_or_default();
    if state.is_empty() {
        return bind_result_page(false, "invalid_callback: the callback carried no state");
    }
    match hub.complete_atproto_bind(state, code, parameter("iss"), root_url) {
        Ok(handle) => bind_result_page(true, &handle),
        Err(problem) => bind_result_page(false, &problem),
    }
}

// ---------- the small HTML surfaces (hand-rolled, escaped, inert) ----------

/// Minimal HTML escaping for the few values interpolated into pages.
fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\"', "&quot;")
}

/// The shared skeleton of the bind flow's small pages.
fn bind_skeleton(title: &str, body: &str) -> String {
    let style = MANAGER_STYLE;
    let atmosphere = atmosphere_assets();
    format!(r##"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="theme-color" content="#111016"><title>{title} · Tangent</title><style>{style}</style></head>
<body class="auth-page"><header class="masthead"><a class="brand" href="/"><span aria-hidden="true">✦</span> Tangent <small>Companions</small></a><span class="local-badge">ON YOUR COMPUTER</span></header>
<main>{body}</main>{atmosphere}</body></html>"##)
}

/// The callback's result page: success names the bound handle and frees the tab; a
/// failure names the failure code honestly.
fn bind_result_page(success: bool, message: &str) -> ApiResponse {
    let body = if success {
        format!(
            "<p class=\"eyebrow\">A familiar face, ready to return</p><h1>You’re connected.</h1>\n\
             <p class=\"account\">Signed in as <strong>{}</strong></p>\n\
             <p class=\"muted\">Your companion’s account is ready. The connector remembers this sign-in for future visits and continues any waiting connection.</p>\n\
             <a class=\"button\" href=\"/\">Back to your companions</a>\n\
             <p class=\"muted\">You can also close this tab and return to your agent app.</p>\n",
            html_escape(message)
        )
    } else {
        format!(
            "<p class=\"eyebrow\">Let’s try that again</p><h1>Sign-in didn’t finish.</h1>\n\
             <p class=\"muted\">We couldn’t connect your companion’s account. Return to the manager and choose Sign in to start again.</p>\n\
             <a class=\"button\" href=\"/\">Back to your companions</a>\n\
             <details><summary>What happened</summary><p class=\"error\">{}</p></details>\n",
            html_escape(message)
        )
    };
    ApiResponse(200, Value::String(bind_skeleton(if success { "Account connected" } else { "Sign-in didn’t finish" }, &body)), None)
}

/// A bind that could not even start (default authorization server unreachable,
/// discovery failure on the `?handle=` path, PAR refusal): the operator sees the
/// honest reason with a way back to retry.
fn bind_problem_page(problem: &str, local_id: &str) -> ApiResponse {
    let body = format!(
        "<p class=\"eyebrow\">A little interruption</p><h1>We couldn’t open sign-in.</h1>\n\
         <p class=\"muted\">The account connection couldn’t be started. Try again, or return to your companions.</p>\n\
         <details><summary>What happened</summary><p class=\"error\">{}</p></details>\n\
         <p><a class=\"button\" href=\"/bind/{}/{}\">Try again</a></p><p class=\"muted\"><a href=\"/\">Back to your companions</a></p>\n\
         <details><summary>Using another account provider?</summary><p class=\"muted\">Add <code>?handle=your.handle</code> to the sign-in address. The connector will use that handle to find your provider.</p></details>\n",
        html_escape(problem),
        html_escape(local_id),
        BIND_PROVIDER_ATPROTO
    );
    ApiResponse(200, Value::String(bind_skeleton("Sign-in unavailable", &body)), None)
}

fn not_found_page(message: &str) -> ApiResponse {
    let body = format!("<p class=\"eyebrow\">Let’s find your way back</p><h1>This page isn’t here.</h1>\n<p class=\"muted\">Your companion manager is just one step away.</p>\n<a class=\"button\" href=\"/\">Back to your companions</a>\n<details><summary>Page details</summary><p class=\"error\">{}</p></details>\n", html_escape(message));
    ApiResponse(404, Value::String(bind_skeleton("Page not found", &body)), None)
}

// ---------- tiny query/form parsing (bounded, strict enough for loopback forms) ----------

/// Splits one `a=1&b=2` string into decoded pairs; `+` reads as space.
fn parse_pairs(raw: &str) -> Vec<(String, String)> {
    raw.split('&')
        .filter(|pair| !pair.is_empty())
        .filter_map(|pair| {
            let (name, value) = pair.split_once('=')?;
            Some((percent_decode_form(name), percent_decode_form(value)))
        })
        .collect()
}

fn parse_query(query: &str) -> Vec<(String, String)> {
    parse_pairs(query)
}

/// Minimal percent-decoding plus `+`-as-space (the form convention).
fn percent_decode_form(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            b'%' if index + 3 <= bytes.len() => {
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok();
                match hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        index += 3;
                    }
                    None => {
                        out.push(bytes[index]);
                        index += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

fn companion_list(hub: &ConnectorHub) -> Vec<Value> {
    // One batched hub read (one store guard inside it), then pure JSON assembly. This
    // route once walked the store under its own guard and asked the hub per companion —
    // `atproto_binding` re-locked the same non-reentrant mutex on the same thread and
    // froze the entire hub (store held forever): every Connect, every operator
    // mutation, the page itself. Never re-enter the store from under a store guard.
    let inventory = hub.companion_inventory();
    let availability: std::collections::HashMap<String, bool> = hub
        .enrollment_inventory()
        .into_iter()
        .map(|(entry, available)| (entry.local_id.clone(), available))
        .collect();
    inventory
        .into_iter()
        .map(|(companion, atproto, count)| {
            let local_id = companion.local_id.clone();
            let mut value = companion_with_atproto(&companion, atproto);
            value["enrollmentCount"] = json!(count);
            value["sessionsAvailable"] = json!(availability.get(&local_id).copied().unwrap_or(true));
            value
        })
        .collect()
}

fn companion_json(companion: &companion_core::domain::companion::Companion) -> Value {
    json!({
        "localId": companion.local_id,
        "handle": companion.handle,
        "displayName": companion.display_name,
        "boundDid": companion.bound_did,
        "createdAt": companion.created_at,
    })
}

/// The companion view plus its atproto binding status: what is bound, where, and how old
/// the session is — never the access token. Pure rendering: the
/// binding is fetched by the caller, so no store guard is ever held here.
fn companion_with_atproto(
    companion: &companion_core::domain::companion::Companion,
    atproto: Option<crate::application::hub::AtprotoBinding>,
) -> Value {
    let mut value = companion_json(companion);
    value["atproto"] = match atproto {
        Some(binding) => json!({
            "did": binding.did,
            "handle": binding.handle,
            "pds": binding.pds,
            "obtainedAt": binding.obtained_at,
        }),
        None => Value::Null,
    };
    value
}

/// Session STATUS only: whether the enrollment holds one — never the token value.
fn enrollment_json(entry: &companion_core::domain::companion::Enrollment, available: bool) -> Value {
    json!({
        "enrollmentId": entry.enrollment_id,
        "companionId": entry.local_id,
        "origin": entry.origin,
        "participantRef": entry.participant_ref,
        "did": entry.did,
        "handle": entry.handle,
        "displayName": entry.display_name,
        "autoCheck": entry.auto_check,
        "sessionStatus": if available { "stored" } else { "missing" },
    })
}

fn finish<T>(result: Result<T, String>, render: impl FnOnce(T) -> Value) -> ApiResponse {
    match result {
        Ok(value) => ApiResponse(200, render(value), None),
        Err(message) => {
            let (code, text) = match message.split_once(": ") {
                Some((code, text)) => (code.to_string(), text.to_string()),
                None => ("blocked".to_string(), message),
            };
            ApiResponse(200, blocked_json(&code, &text), None)
        }
    }
}

fn ok_json(data: Value) -> Value {
    let mut value = json!({ "status": "ok" });
    if let (Some(object), Some(payload)) = (value.as_object_mut(), data.as_object()) {
        for (key, item) in payload {
            if item.is_null() {
                continue;
            }
            object.insert(key.clone(), item.clone());
        }
    }
    value
}

fn blocked_json(code: &str, message: &str) -> Value {
    json!({ "status": "blocked", "problem": { "code": code, "message": message } })
}

fn problem_json(code: &str, message: &str) -> Value {
    blocked_json(code, message)
}

fn respond_discovery(writer: &mut impl Write, preflight: bool, manager_origin: &str) -> std::io::Result<()> {
    let bytes = if preflight {
        Vec::new()
    } else {
        serde_json::to_vec(&json!({
            "product": CONNECTOR_PRODUCT,
            "discoveryVersion": 1,
            "managerOrigin": manager_origin,
        }))
        .unwrap_or_default()
    };
    let (status, reason) = if preflight { (204, "No Content") } else { (200, "OK") };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, OPTIONS\r\nAccess-Control-Allow-Private-Network: true\r\nVary: Origin, Access-Control-Request-Private-Network\r\nX-Content-Type-Options: nosniff\r\n\r\n",
        bytes.len()
    );
    writer.write_all(head.as_bytes())?;
    writer.write_all(&bytes)?;
    writer.flush()
}

fn respond(writer: &mut TcpStream, status: u16, body: Value, location: Option<&str>) -> std::io::Result<()> {
    let (content_type, bytes) = match body {
        Value::String(html) => {
            // An empty HTML string is an empty body (the redirect case).
            let content_type = if html.is_empty() { "text/plain; charset=utf-8" } else { "text/html; charset=utf-8" };
            (content_type, html.into_bytes())
        }
        other => ("application/json", serde_json::to_vec(&other).unwrap_or_default()),
    };
    let reason = match status {
        200 => "OK",
        201 => "Created",
        302 => "Found",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        _ => "Error",
    };
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n",
        bytes.len()
    );
    if let Some(location) = location {
        head.push_str(&format!("Location: {location}\r\n"));
    }
    head.push_str("\r\n");
    writer.write_all(head.as_bytes())?;
    writer.write_all(&bytes)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn the_page_opens_no_dialog() {
        let page = manager_index();
        for blocking in ["alert(", "confirm(", "prompt(", "showModal(", "<dialog", "createElement('dialog')"] {
            assert!(!page.contains(blocking), "the companion page must not use {blocking}");
        }
    }

    #[test]
    fn capped_reads_consume_one_line_at_a_time_and_refuse_oversize() {
        let mut reader = Cursor::new(b"first line\r\nsecond\r\n".to_vec());
        let mut line = String::new();
        assert_eq!(read_line_capped(&mut reader, &mut line, 64).unwrap(), Some(12));
        assert_eq!(line.trim_end(), "first line", "the CR of a CRLF line is trimming territory");
        assert_eq!(read_line_capped(&mut reader, &mut line, 64).unwrap(), Some(8));
        assert_eq!(line.trim_end(), "second");
        assert_eq!(read_line_capped(&mut reader, &mut line, 64).unwrap(), None, "clean end of stream");
        // An unterminated run longer than the cap is refused while arriving.
        let mut endless = Cursor::new(vec![b'x'; 4096]);
        assert!(matches!(read_line_capped(&mut endless, &mut line, 128), Err(CappedError::OverLimit)));
        // A final unterminated fragment still yields its bytes.
        let mut partial = Cursor::new(b"tail".to_vec());
        assert_eq!(read_line_capped(&mut partial, &mut line, 64).unwrap(), Some(4));
        assert_eq!(line, "tail");
    }

    #[test]
    fn discovery_is_minimal_cors_enabled_and_has_an_inert_preflight() {
        let mut response = Vec::new();
        respond_discovery(&mut response, false, "http://127.0.0.1:5219").expect("discovery response");
        let response = String::from_utf8(response).expect("utf-8 response");
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("Access-Control-Allow-Origin: *"));
        assert!(response.contains("Access-Control-Allow-Private-Network: true"));
        assert!(response.contains("\"product\":\"tangent-space-connector\""));
        assert!(response.contains("\"managerOrigin\":\"http://127.0.0.1:5219\""));
        assert!(!response.contains("companions") && !response.contains("enrollment") && !response.contains("token"));

        let mut preflight = Vec::new();
        respond_discovery(&mut preflight, true, "http://127.0.0.1:5219").expect("preflight response");
        let preflight = String::from_utf8(preflight).expect("utf-8 preflight");
        assert!(preflight.starts_with("HTTP/1.1 204 No Content"));
        assert!(preflight.ends_with("\r\n\r\n"), "preflight has no response body");
    }
}
