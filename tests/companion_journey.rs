//! Companion journeys: companion CRUD and handle uniqueness, behavior-based companion
//! resolution (exactly one companion → auto-resolve for every intake; several → honest
//! selection question; zero → creation instruction), the account-bound enrollment
//! exchange against the fake server, and the companion manager's local-only discipline.
//! All data is synthetic.

mod common;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;

use serde_json::{json, Value};

use common::FakeServer;
use tangent_connector::adapters::experience::UreqExperience;
use tangent_connector::adapters::store::StateStore;
use tangent_connector::application::bus::EventBus;
use tangent_connector::application::hub::ConnectorHub;
use tangent_connector::application::ports::ExperiencePort;
use tangent_connector::domain::companion::CallerId;
use tangent_connector::domain::intake::IntakeChannel;

fn workspace(label: &str, caller: CallerId) -> Arc<ConnectorHub> {
    // Test sessions are synthetic.
    let dir = std::env::temp_dir().join(format!("tangent-connector-companion-{}-{}", label, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let events = Arc::new(EventBus::new());
    let port: Arc<dyn ExperiencePort> = Arc::new(UreqExperience::new());
    let store = StateStore::open(&dir).expect("store");
    Arc::new(ConnectorHub::new(port, store, events, caller))
}

fn mcp_workspace(label: &str, client: &str) -> Arc<ConnectorHub> {
    workspace(label, CallerId(format!("mcp:{client}")))
}

fn select(hub: &ConnectorHub, moniker: Option<&str>) -> tangent_connector::application::hub::ToolOutcome {
    let mut arguments = json!({});
    if let Some(moniker) = moniker {
        arguments["moniker"] = json!(moniker);
    }
    hub.invoke(IntakeChannel::Mcp, "SelectCompanion", &arguments)
}

/// An companion enrolled the only way a companion can be: its atproto account bound
/// (seeded, since binding is [`bind_oauth_journey`]'s subject) and then the
/// account-bound proof exchange.
fn enrolled(hub: &ConnectorHub, server: &FakeServer, handle: &str) -> (String, String) {
    let account = format!("{handle}.bsky.example");
    let did = format!("did:plc:{handle}");
    server.add_account(&account, "unused", &did);
    let local_id = common::seed_bound_companion(hub, &account, &did, server.origin());
    let entry = hub.enroll_bound(&local_id, server.origin()).expect("bound enrollment");
    (local_id, entry.enrollment_id)
}

// ---------- companion CRUD ----------

#[test]
fn companion_crud_enforces_handle_uniqueness() {
    let hub = workspace("crud", CallerId("cli".into()));
    let lumen = hub.create_companion("lumen", Some("Lumen")).expect("create");
    assert_eq!(lumen.handle, "lumen");
    assert_eq!(lumen.local_id.len(), 32, "local ids are guid-v7 hex");

    // Uniqueness is case-insensitive within the connector.
    assert!(hub.create_companion("LUMEN", None).is_err());
    let other = hub.create_companion("ada", None).expect("second companion");
    assert!(hub.update_companion(&other.local_id, Some("lumen"), None).is_err(), "an update may not steal a handle");
    assert!(hub.create_companion("x", None).is_err(), "handles are at least 2 characters");
    assert!(hub.create_companion(&"h".repeat(254), None).is_err(), "handles are at most 253 characters");

    let renamed = hub.update_companion(&lumen.local_id, Some("lumen-primary"), Some(Some("Lumen E."))).expect("update");
    assert_eq!(renamed.handle, "lumen-primary");
    assert_eq!(renamed.display_name.as_deref(), Some("Lumen E."));
    let cleared = hub.update_companion(&lumen.local_id, None, Some(None)).expect("clear display name");
    assert!(cleared.display_name.is_none());

    hub.delete_companion(&other.local_id, false).expect("delete without enrollments");
    assert!(hub.delete_companion(&other.local_id, false).is_err(), "the companion is gone");
    assert!(hub.companion(&lumen.local_id).is_some());
}

#[test]
fn delete_refuses_while_enrollments_exist_and_cascades_when_confirmed() {
    let server = FakeServer::start();
    let hub = workspace("cascade", CallerId("cli".into()));
    let (local_id, enrollment_id) = enrolled(&hub, &server, "lumen");

    let refused = hub.delete_companion(&local_id, false).expect_err("must refuse");
    assert!(refused.contains("enrollment"), "error was: {refused}");

    hub.delete_companion(&local_id, true).expect("cascade delete");
    assert!(hub.companion(&local_id).is_none());
    assert!(hub.enrollments_of(&local_id).is_empty());
    assert!(hub.store().lock().unwrap().enrollment(&enrollment_id).is_none(), "the enrollment cascades");
}

// ---------- behavior-based resolution (every intake alike) ----------

#[test]
fn the_one_companion_resolves_automatically_for_every_intake() {
    let server = FakeServer::start();
    let hub = workspace("one-companion", CallerId("cli".into()));
    let (local_id, enrollment_id) = enrolled(&hub, &server, "lumen");

    // The CLI intake resolves exactly like the MCP intake: no moniker, one companion.
    let outcome = hub.invoke(IntakeChannel::Cli, "SelectCompanion", &json!({}));
    assert!(!outcome.is_error, "text: {}", outcome.text);
    assert_eq!(
        outcome.structured.pointer("/connector/enrollmentId").and_then(Value::as_str),
        Some(enrollment_id.as_str())
    );
    // An explicit moniker still works alongside the automatic resolution.
    let by_moniker = hub.invoke(IntakeChannel::Mcp, "SelectCompanion", &json!({ "moniker": "lumen" }));
    assert!(!by_moniker.is_error, "text: {}", by_moniker.text);
    let _ = local_id;
}

#[test]
fn several_companions_resolve_nothing_without_an_explicit_choice() {
    let server = FakeServer::start();
    let hub = mcp_workspace("two-companions", "codex-host");
    let _ = enrolled(&hub, &server, "alpha");
    let beta = hub.create_companion("beta", None).expect("companion");
    let _ = beta;

    let outcome = select(&hub, None);
    assert!(outcome.is_error);
    assert_eq!(
        outcome.structured.pointer("/problem/code").and_then(Value::as_str),
        Some("companion_selection_required")
    );
    assert!(outcome.text.contains("alpha") && outcome.text.contains("beta"), "both handles are listed: {}", outcome.text);
    assert!(outcome.text.contains("moniker"), "the instruction names the explicit path: {}", outcome.text);
}

#[test]
fn no_companions_point_at_creation() {
    let hub = mcp_workspace("zero-companions", "codex-host");
    let outcome = select(&hub, None);
    assert!(outcome.is_error);
    assert_eq!(
        outcome.structured.pointer("/problem/code").and_then(Value::as_str),
        Some("companion_selection_required")
    );
    assert!(outcome.text.contains("No local companion exists yet"), "text: {}", outcome.text);
    assert!(outcome.text.contains("operator"), "the instruction names the operator path: {}", outcome.text);
}

// ---------- the W2-contract enrollment exchange ----------

#[test]
fn already_enrolled_is_an_honest_error_locally_and_from_the_server() {
    let server = FakeServer::start();
    let hub = workspace("already", CallerId("cli".into()));
    server.add_account("jeff.bsky.example", "unused", "did:plc:jeff");
    let local_id = common::seed_bound_companion(&hub, "jeff.bsky.example", "did:plc:jeff", server.origin());
    hub.enroll_bound(&local_id, server.origin()).expect("first enrollment");

    // Local guard: the companion already holds a session for this origin.
    let local = hub.enroll_bound(&local_id, server.origin()).expect_err("must refuse");
    assert!(local.contains("already_enrolled"), "error was: {local}");

    // Server-side guard: forgetting locally leaves the server's mapping, which reports
    // already_enrolled without a new credential.
    let enrollment_id = hub.enrollments_of(&local_id)[0].enrollment_id.clone();
    hub.forget_enrollment(&enrollment_id).expect("forget");
    assert!(hub.enrollments_of(&local_id).is_empty(), "the local enrollment is gone");
}

// ---------- two origins, two sessions ----------

#[test]
fn one_companion_at_two_servers_keeps_distinct_working_sessions() {
    let server_a = FakeServer::start();
    let server_b = FakeServer::start();
    let hub = workspace("two-origins", CallerId("cli".into()));
    for server in [&server_a, &server_b] {
        server.add_account("jeff.bsky.example", "unused", "did:plc:jeff");
    }
    let local_id = common::seed_bound_companion(&hub, "jeff.bsky.example", "did:plc:jeff", server_a.origin());

    let at_a = hub.enroll_bound(&local_id, server_a.origin()).expect("enroll at a");
    // Each fake listener plays both the Tangent server and the account's PDS, so it only
    // honours proofs it minted itself. Pointing the binding at the second listener's PDS
    // role is what lets the same companion enrol there; the subject under test is the
    // session map, not where the account is hosted.
    common::seed_atproto_session(&hub, &local_id, "jeff.bsky.example", "did:plc:jeff", server_b.origin());
    let at_b = hub.enroll_bound(&local_id, server_b.origin()).expect("enroll at b");
    assert_ne!(at_a.enrollment_id, at_b.enrollment_id, "each enrollment is its own session key");
    let (token_a, token_b) = {
        let store = hub.store().lock().unwrap();
        (
            store.session(&at_a.enrollment_id).expect("a session at a"),
            store.session(&at_b.enrollment_id).expect("a session at b"),
        )
    };
    assert_ne!(token_a, token_b, "each origin issued its own session");
    {
        // The session map holds one distinct entry per enrollment.
        let store = hub.store().lock().unwrap();
        assert_eq!(store.session(&at_a.enrollment_id).as_deref(), Some(token_a.as_str()));
        assert_eq!(store.session(&at_b.enrollment_id).as_deref(), Some(token_b.as_str()));
    }

    // Both enrollments hold distinct working sessions: arrival at each origin uses that
    // origin's own bearer, and neither session ever reaches the other server.
    for (entry, server, token) in [(&at_a, &server_a, &token_a), (&at_b, &server_b, &token_b)] {
        let outcome = hub.invoke(
            IntakeChannel::Cli,
            "Arrive",
            &json!({ "enrollmentId": entry.enrollment_id, "serverUrl": server.origin() }),
        );
        assert!(!outcome.is_error, "arrival failed: {}", outcome.text);
        let seen = server
            .requests()
            .iter()
            .any(|request| request.path == "/api/v1/experience" && request.bearer == format!("Bearer {token}"));
        assert!(seen, "the origin did not see its own bearer");
    }
    assert!(server_a.requests().iter().all(|request| !request.bearer.contains(&token_b)), "session b never reaches server a");
    assert!(server_b.requests().iter().all(|request| !request.bearer.contains(&token_a)), "session a never reaches server b");

    // Forgetting one enrollment removes exactly its session; the other stays intact and
    // working.
    hub.forget_enrollment(&at_a.enrollment_id).expect("forget a");
    let inventory = hub.enrollment_inventory();
    assert!(inventory.iter().all(|(entry, _)| entry.enrollment_id != at_a.enrollment_id));
    let (_, available) = inventory
        .iter()
        .find(|(entry, _)| entry.enrollment_id == at_b.enrollment_id)
        .expect("enrollment b remains");
    assert!(available, "b's session is intact under its own key");
    {
        let store = hub.store().lock().unwrap();
        assert_eq!(store.session(&at_b.enrollment_id).as_deref(), Some(token_b.as_str()), "b's session value survives");
        assert_eq!(store.session(&at_a.enrollment_id), None, "a's session is gone with its enrollment");
    }
    let again = hub.invoke(
        IntakeChannel::Cli,
        "Arrive",
        &json!({ "enrollmentId": at_b.enrollment_id, "serverUrl": server_b.origin() }),
    );
    assert!(!again.is_error, "b still participates after forgetting a: {}", again.text);
}

// ---------- the manager listener ----------

fn http_round_trip(stream: &mut TcpStream, request: &str) -> (String, String) {
    stream.write_all(request.as_bytes()).expect("write request");
    stream.flush().expect("flush");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read response");
    let text = String::from_utf8_lossy(&raw).to_string();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((text.as_str(), ""));
    (head.to_string(), body.to_string())
}

/// The page carries no token: the operator is the trust root, a local
/// process can read state.json anyway, and the browser drive-by class is blocked
/// structurally (loopback bind, GET/POST only, caps, JSON bodies, no CORS). The page and
/// its API answer plainly on loopback; the ceremony routes are gone.
#[test]
fn the_operator_api_answers_plainly_and_the_ceremony_routes_are_gone() {
    let hub = workspace("operator-plain", CallerId("manager".into()));
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = listener.local_addr().unwrap();
    {
        let hub = hub.clone();
        let serving = listener.try_clone().expect("clone listener");
        std::thread::Builder::new()
            .name("operator-under-test".into())
            .spawn(move || tangent_connector::adapters::manager::serve(serving, hub))
            .expect("server thread");
    }

    // The page itself: inert HTML, served plainly.
    let mut page = TcpStream::connect(address).expect("connect");
    let (head, body) = http_round_trip(&mut page, "GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    assert!(head.starts_with("HTTP/1.1 200"), "head was: {head}");
    assert!(body.contains("Atmosphere handle"), "the Atmosphere-handle column is live on the page");
    // The enroll buttons are gone (R2): the page never enrolls, and the per-companion
    // bind pages exist for the Connect handshake to open.
    assert!(!body.contains("enroll-bound") && !body.contains("Enroll unbound") && !body.contains("Enroll with bound"), "no enroll buttons remain: {body}");
    assert!(body.contains("/bind/"), "the per-companion bind pages are wired");
    // The sign-in form is gone: binding is an OAuth page, never a
    // password form on the companion manager.
    assert!(!body.contains("type=\"password\"") && !body.contains("appPassword"), "no password form remains: {body}");
    // The allowlist section is gone too: resolution is behavior, not configuration.
    assert!(!body.contains("allowlist") && !body.contains("Allowlist"), "no allowlist UI remains: {body}");

    // The API reads plainly — no token anywhere (structural absence: the generator is
    // gone from the codebase, so there is nothing to present).
    let mut bare = TcpStream::connect(address).expect("connect");
    let (head, body) = http_round_trip(&mut bare, "GET /api/companions HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    assert!(head.starts_with("HTTP/1.1 200"), "head was: {head}");
    assert!(body.contains("\"status\":\"ok\""), "body was: {body}");

    // A mutation works plainly: creating an companion through the API.
    let mut via_post = TcpStream::connect(address).expect("connect");
    let payload = json!({ "handle": "lumen", "displayName": "Lumen" }).to_string();
    let (head, body) = http_round_trip(
        &mut via_post,
        &format!(
            "POST /api/companions HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: http://127.0.0.1\r\nSec-Fetch-Site: same-origin\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
            payload.len()
        ),
    );
    assert!(head.starts_with("HTTP/1.1 200"), "head was: {head}");
    assert!(body.contains("\"handle\":\"lumen\""), "body was: {body}");
    assert_eq!(hub.companions().len(), 1, "the mutation crossed the same hub");

    // The enroll API routes are gone (R2), and the allowlist routes are gone (owner
    // correction): both answer an honest 404.
    let mut enroll_attempt = TcpStream::connect(address).expect("connect");
    let (head, _) = http_round_trip(
        &mut enroll_attempt,
        "POST /api/companions/00000000000000000000000000000000/enroll HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: http://127.0.0.1\r\nSec-Fetch-Site: same-origin\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
    );
    assert!(head.starts_with("HTTP/1.1 404"), "the enroll route is gone: {head}");
    let mut allowlist_read = TcpStream::connect(address).expect("connect");
    let (head, _) = http_round_trip(
        &mut allowlist_read,
        "GET /api/allowlist HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
    );
    assert!(head.starts_with("HTTP/1.1 404"), "the allowlist read route is gone: {head}");
    let mut allowlist_write = TcpStream::connect(address).expect("connect");
    let (head, _) = http_round_trip(
        &mut allowlist_write,
        "POST /api/allowlist HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: http://127.0.0.1\r\nSec-Fetch-Site: same-origin\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
    );
    assert!(head.starts_with("HTTP/1.1 404"), "the allowlist write route is gone: {head}");
}

/// The companion manager answers its own page and nothing else. A site that reaches
/// this port — by rebinding its hostname to the loopback address, or by sending the one
/// content type that crosses origins without a preflight — is refused before it reaches
/// a route, while the page's own write and the inert discovery document still work.
#[test]
fn the_companion_manager_refuses_foreign_hosts_and_cross_site_writes() {
    let hub = workspace("operator-hardened", CallerId("manager".into()));
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = listener.local_addr().unwrap();
    {
        let hub = hub.clone();
        let serving = listener.try_clone().expect("clone listener");
        std::thread::Builder::new()
            .name("operator-hardened-under-test".into())
            .spawn(move || tangent_connector::adapters::manager::serve(serving, hub))
            .expect("server thread");
    }
    let payload = json!({ "handle": "intruder" }).to_string();
    let write = |headers: &str| {
        format!(
            "POST /api/companions HTTP/1.1\r\nHost: 127.0.0.1\r\n{headers}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
            payload.len()
        )
    };

    // A rebound hostname reaches the loopback socket, but the browser names the
    // attacker's host — so even a read is refused.
    let mut rebound = TcpStream::connect(address).expect("connect");
    let (head, _) = http_round_trip(
        &mut rebound,
        "GET /api/companions HTTP/1.1\r\nHost: rebound.example\r\nConnection: close\r\n\r\n",
    );
    assert!(head.starts_with("HTTP/1.1 403"), "a foreign host is refused, reads included: {head}");

    // `text/plain` is a CORS simple request: no preflight stands between a website and
    // this port, so the JSON surface refuses the content type itself.
    let mut plain = TcpStream::connect(address).expect("connect");
    let (head, body) = http_round_trip(
        &mut plain,
        &format!(
            "POST /api/companions HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: http://attacker.example\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
            payload.len()
        ),
    );
    assert!(head.starts_with("HTTP/1.1 403"), "a cross-site text/plain write is refused: {head}");
    assert!(body.contains("cross_site_write"), "the refusal names itself: {body}");

    // Each piece of same-origin evidence stands on its own.
    let mut foreign_origin = TcpStream::connect(address).expect("connect");
    let (head, _) = http_round_trip(&mut foreign_origin, &write("Origin: http://attacker.example\r\n"));
    assert!(head.starts_with("HTTP/1.1 403"), "a foreign origin is refused: {head}");
    let mut foreign_site = TcpStream::connect(address).expect("connect");
    let (head, _) = http_round_trip(&mut foreign_site, &write("Origin: http://127.0.0.1\r\nSec-Fetch-Site: cross-site\r\n"));
    assert!(head.starts_with("HTTP/1.1 403"), "a cross-site fetch is refused: {head}");
    let mut anonymous = TcpStream::connect(address).expect("connect");
    let (head, _) = http_round_trip(&mut anonymous, &write(""));
    assert!(head.starts_with("HTTP/1.1 403"), "a write with no origin is refused: {head}");

    assert!(hub.companions().is_empty(), "no refused write reached the hub");

    // The page's own write still works.
    let mut page = TcpStream::connect(address).expect("connect");
    let (head, _) = http_round_trip(&mut page, &write("Origin: http://127.0.0.1\r\nSec-Fetch-Site: same-origin\r\n"));
    assert!(head.starts_with("HTTP/1.1 200"), "the page's own write is served: {head}");
    assert_eq!(hub.companions().len(), 1, "exactly one companion was created");

    // Discovery stays readable across origins: it is the inert document another process
    // reads to recognise this page, and it discloses nothing else.
    let mut discovery = TcpStream::connect(address).expect("connect");
    let (head, body) = http_round_trip(
        &mut discovery,
        "GET /api/discovery HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: https://tangent.example\r\nConnection: close\r\n\r\n",
    );
    assert!(head.starts_with("HTTP/1.1 200"), "discovery still answers: {head}");
    assert!(body.contains("tangent-space-connector"), "discovery names the product: {body}");
    assert!(!body.contains("intruder"), "discovery discloses no companion: {body}");
}
