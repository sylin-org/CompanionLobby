//! The OAuth bind journeys against the fake authorization server (synthetic data): the
//! connector-served /bind route runs the atproto local-client profile end to end with
//! NO interstitial — the GET itself starts the flow at the default
//! authorization server (PAR: loopback client, PKCE S256, DPoP-bound, no login_hint) →
//! authorize redirect (the provider's own UI picks the account) → loopback callback
//! with code+state+iss → tokens stored as the companion's atproto session (refresh
//! token and DPoP key included; the mandatory `sub` claim IS the bound DID) → the
//! waiting connect auto-resumes, its getServiceAuth DPoP-proved against the
//! proof-demanding fake PDS. Refusals are honest: state mismatch and a missing or
//! foreign issuer bind nothing, an aged-out flight refuses, and the companion manager
//! carries no password form anywhere. The `?handle=` escape hatch still discovers
//! self-hosted PDSes; the silent refresh runs before the PDS session is used.

mod common;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::sync::Arc;

use serde_json::Value;

use common::FakeServer;
use adapter_auth_atproto::atproto_oauth::AtprotoOauth;
use adapter_service_tangent::experience::UreqExperience;
use companion_lobby::adapters::manager;
use companion_lobby::adapters::store::StateStore;
use companion_lobby::application::bus::EventBus;
use companion_lobby::application::hub::ConnectorHub;
use companion_core::domain::companion::CallerId;
use companion_core::domain::intake::IntakeChannel;

/// A hub plus a real manager server on an ephemeral loopback port, with the OAuth
/// resolution origins and the default authorization server pointed at the fake.
/// Answers the server's page URL.
fn operator_workspace(label: &str, server: &FakeServer) -> (Arc<ConnectorHub>, std::path::PathBuf, String, std::net::SocketAddr) {
    let dir = std::env::temp_dir().join(format!("companion-lobby-bind-{}-{}", label, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let events = Arc::new(EventBus::new());
    let store = StateStore::open(&dir).expect("store");
    let hub = Arc::new(ConnectorHub::new(store, events, CallerId("manager".into()))
        .with_service(Arc::new(UreqExperience::new()))
        .with_auth(Arc::new(AtprotoOauth::new())));
    hub.set_atproto_oauth(AtprotoOauth::with_origins(server.origin(), server.origin(), server.origin()));
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = listener.local_addr().expect("address");
    {
        let hub = hub.clone();
        let serving = listener.try_clone().expect("clone listener");
        std::thread::Builder::new()
            .name("operator-under-test".into())
            .spawn(move || manager::serve(serving, hub))
            .expect("server thread");
    }
    let page = format!("http://{address}/");
    hub.set_manager_page_url(&page);
    (hub, dir, page, address)
}

fn server_addr(origin: &str) -> std::net::SocketAddr {
    origin.trim_start_matches("http://").to_socket_addrs().expect("resolve").next().expect("an address")
}

fn http_round_trip(stream: &mut TcpStream, request: &str) -> String {
    stream.write_all(request.as_bytes()).expect("write request");
    stream.flush().expect("flush");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read response");
    String::from_utf8_lossy(&raw).to_string()
}

fn get(stream_addr: std::net::SocketAddr, target: &str) -> String {
    let mut stream = TcpStream::connect(stream_addr).expect("connect");
    http_round_trip(&mut stream, &format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"))
}

/// Starts one bind the way the operator's browser does — GETs the bind route (with the
/// optional `?handle=` escape hatch), follows the fake authorization server's
/// auto-approving authorize redirect — and answers the loopback callback URL without
/// visiting it, so scenarios can tamper with (or age out) the flight first.
fn start_bind(address: std::net::SocketAddr, server: &FakeServer, local_id: &str, handle: Option<&str>) -> String {
    let target = match handle {
        Some(handle) => format!("/bind/{local_id}/atproto?handle={handle}"),
        None => format!("/bind/{local_id}/atproto"),
    };
    let started = get(address, &target);
    assert!(
        started.starts_with("HTTP/1.1 302"),
        "the bind route immediately redirects to authorize (no interstitial): {started}"
    );
    let authorize = started
        .lines()
        .find_map(|line| line.strip_prefix("Location: "))
        .expect("authorize location")
        .trim()
        .to_string();
    assert!(authorize.starts_with(&format!("{}/oauth/authorize", server.origin())), "authorize on the fake AS: {authorize}");
    let authorize_path = authorize.strip_prefix(server.origin()).unwrap_or(&authorize);
    let redirected = get(server_addr(server.origin()), authorize_path);
    assert!(redirected.starts_with("HTTP/1.1 302"), "the AS redirects back: {redirected}");
    redirected
        .lines()
        .find_map(|line| line.strip_prefix("Location: "))
        .expect("callback location")
        .trim()
        .to_string()
}

/// Runs the operator-driven OAuth dance against the fake: GET bind → 302 authorize →
/// 302 callback → the callback page's response. Answers the callback response.
fn drive_bind(address: std::net::SocketAddr, server: &FakeServer, local_id: &str, handle: Option<&str>) -> String {
    let callback = start_bind(address, server, local_id, handle);
    assert!(callback.starts_with(&format!("http://{address}/?")), "the callback lands on the operator root: {callback}");
    let callback_path = callback.strip_prefix(&format!("http://{address}")).unwrap_or(&callback);
    get(address, callback_path)
}

// ---------- the full journey ----------

#[test]
fn the_bind_route_starts_the_flow_immediately_and_binds_the_authenticated_account() {
    let server = FakeServer::start();
    // A decoy account registered FIRST: the provider signs the default flow in as its
    // LAST registered account, and nothing the connector sends names either — the
    // binding is exactly what the provider's token says it is.
    server.add_account("decoy.bsky.example", "unused-password", "did:plc:decoy");
    server.add_account("lumen.bsky.example", "unused-password", "did:plc:lumen");
    let (hub, dir, _page, address) = operator_workspace("journey", &server);
    let companion = hub.create_companion("lumen", None).expect("companion");

    // The companion manager links the bind route and carries no password form anywhere.
    let operator_page = get(address, "/");
    assert!(operator_page.contains("signIn.href = \"/bind/\" + companion.localId + \"/atproto\""), "Sign In links the bind route: {operator_page}");
    assert!(!operator_page.contains("type=\"password\"") && !operator_page.contains("appPassword"), "the companion manager has no password form: {operator_page}");

    // A connect waits for the operator (no binding yet) — the honest popped-page answer.
    let waiting = hub.invoke(IntakeChannel::Mcp, "Connect", &serde_json::json!({ "service": "tangent", "persona": "lumen", "address": server.origin() }));
    assert!(waiting.is_error);
    assert_eq!(
        waiting.structured.pointer("/problem/code").and_then(Value::as_str),
        Some("operator_action_needed"),
        "text: {}",
        waiting.text
    );
    assert!(hub.enrollments_of(&companion.local_id).is_empty(), "nothing enrolled while waiting");

    // The operator drives the bind through the fake authorization server — no handle,
    // no interstitial: the GET is the start.
    let callback_page = drive_bind(address, &server, &companion.local_id, None);
    assert!(callback_page.starts_with("HTTP/1.1 200"), "the callback answers: {callback_page}");
    assert!(callback_page.contains("Signed in as <strong>lumen.bsky.example</strong>"), "the success page names the handle: {callback_page}");
    assert!(callback_page.contains("close this tab"), "the tab is freed honestly: {callback_page}");

    // The waiting connect finished by itself: one bound enrollment with a stored session.
    let enrollments = hub.enrollments_of(&companion.local_id);
    assert_eq!(enrollments.len(), 1, "the resume enrolled exactly once");
    assert_eq!(enrollments[0].did.as_deref(), Some("did:plc:lumen"));
    assert!(hub.store().lock().unwrap().has_session(&enrollments[0].enrollment_id));

    // The binding IS the token's subject: no pre-declared DID existed to
    // mismatch, and the authenticated account — not the decoy — is what got bound.
    assert_eq!(hub.companion(&companion.local_id).unwrap().bound_did.as_deref(), Some("did:plc:lumen"));
    let session = hub.store().lock().unwrap().atproto_session(&companion.local_id).expect("oauth session");
    assert_eq!(session.did, "did:plc:lumen");
    assert_eq!(session.handle, "lumen.bsky.example");
    assert_eq!(session.pds, server.origin(), "the PDS comes from the post-exchange DID document");
    assert_eq!(session.authserver.as_deref(), Some(server.origin()));
    assert!(session.access_jwt.split('.').count() == 3, "the access token is JWT-shaped");
    assert!(session.refresh_jwt.as_deref().is_some_and(|token| token.starts_with("rt_oauth")), "the refresh token is stored");
    assert!(session.dpop_key.as_deref().is_some_and(|key| key.len() >= 43), "the DPoP key is stored");
    let state_text = std::fs::read_to_string(dir.join("state.json")).expect("state file");
    assert!(state_text.contains("rt_oauth") && state_text.contains("atproto_sessions"), "the refresh token lives in the state map: {state_text}");
    assert!(!state_text.contains("did:plc:decoy"), "the decoy account's DID appears nowhere: {state_text}");
    assert!(!state_text.contains("unused-password"), "the password never reaches state");

    // The wire discipline the local-client profile demands (all against the fake AS).
    let requests = server.requests();
    let par_position = requests.iter().position(|request| request.path == "/oauth/par").expect("the PAR request");
    let par = &requests[par_position];
    assert_eq!(
        par.body.get("client_id").and_then(Value::as_str),
        Some(adapter_auth_atproto::atproto_oauth::bind_client_id().as_str()),
        "the loopback client id declaring its scope set"
    );
    assert_eq!(par.body.get("response_type").and_then(Value::as_str), Some("code"));
    assert_eq!(
        par.body.get("scope").and_then(Value::as_str),
        Some("atproto rpc:local.tangent.mcp.exchange?aud=*"),
        "the profile base plus the granular getServiceAuth permission"
    );
    assert!(par.body.get("nonce").is_none(), "the atproto profile sends no nonce (the request_uri binding replaces it)");
    assert!(par.body.get("login_hint").is_none(), "no interstitial means no pre-declared account (no login_hint)");
    assert_eq!(par.body.get("code_challenge_method").and_then(Value::as_str), Some("S256"));
    let redirect = par.body.get("redirect_uri").and_then(Value::as_str).unwrap_or_default();
    assert!(redirect.starts_with("http://127.0.0.1:") && redirect.ends_with('/'), "the redirect is the loopback root: {redirect}");
    // Nothing was resolved before the PAR: the default flow declares no account.
    assert!(
        !requests[..par_position].iter().any(|request| request.path.contains("resolveHandle")),
        "no handle resolution happens before the PAR"
    );
    let token_position = requests.iter().position(|request| request.path == "/oauth/token").expect("the token request");
    let token = &requests[token_position];
    assert_eq!(token.body.get("grant_type").and_then(Value::as_str), Some("authorization_code"));
    assert!(token.body.get("code_verifier").and_then(Value::as_str).is_some_and(|verifier| verifier.len() >= 43), "the PKCE verifier rides the exchange");
    assert!(token.body.get("refresh_token").is_none(), "the initial grant is not a refresh");
    // The DID document is resolved AFTER the exchange named the account, not before.
    let did_doc_position = requests
        .iter()
        .position(|request| request.path.starts_with("/did:plc:lumen"))
        .expect("the DID document fetch");
    assert!(did_doc_position > token_position, "the bound DID's document is resolved after the token exchange");
    // The service-auth step of the auto-resume used the OAuth session on the
    // proof-demanding PDS: the DPoP auth scheme (RFC 9449 §7.1), one 401 nonce
    // challenge from the PDS's OWN context, and the retried proof embedding it —
    // never the authorization server's nonce.
    let service_auths: Vec<_> = requests
        .iter()
        .filter(|request| request.path.starts_with("/xrpc/com.atproto.server.getServiceAuth"))
        .collect();
    assert_eq!(service_auths.len(), 2, "exactly one challenge round: {service_auths:?}");
    assert!(service_auths[0].bearer.starts_with("DPoP ey"), "the bound token rides the DPoP auth scheme");
    assert!(!service_auths[0].dpop.is_empty(), "even the challenged first ask carries a proof");
    let first_proof_payload = proof_payload_of(&service_auths[0].dpop);
    assert!(!first_proof_payload.contains("\"nonce\""), "the first ask carries no nonce (the PDS had issued none): {first_proof_payload}");
    assert!(service_auths[1].bearer.starts_with("DPoP ey"), "the retry stays on the DPoP scheme");
    let retry_proof_payload = proof_payload_of(&service_auths[1].dpop);
    assert!(retry_proof_payload.contains("\"nonce\""), "the retried proof embeds the PDS's challenge nonce: {retry_proof_payload}");
}

/// The decoded payload segment of one DPoP proof header (assertable claim shapes).
fn proof_payload_of(proof: &str) -> String {
    let segment = proof.split('.').nth(1).expect("payload segment");
    let decoded = common::decode_b64url(segment).expect("payload decodes");
    String::from_utf8(decoded).expect("payload is UTF-8")
}

#[test]
fn a_state_mismatch_or_missing_issuer_binds_nothing() {
    let server = FakeServer::start();
    server.add_account("keeper.bsky.example", "unused-password", "did:plc:keeper");
    let (hub, _dir, _page, address) = operator_workspace("mismatch", &server);
    let companion = hub.create_companion("keeper", None).expect("companion");

    // Start a real bind, then hand the callback a forged state and a foreign issuer.
    let _real_callback = start_bind(address, &server, &companion.local_id, None);
    let forged = get(address, "/?code=code-999&state=forged-state&iss=http://127.0.0.1:1");
    assert!(forged.starts_with("HTTP/1.1 200"), "the failure still renders a page: {forged}");
    assert!(forged.contains("state_mismatch") && forged.contains("Sign-in didn’t finish"), "the failure names the code: {forged}");
    assert!(hub.companion(&companion.local_id).unwrap().bound_did.is_none(), "nothing was bound");
    assert!(hub.store().lock().unwrap().atproto_session(&companion.local_id).is_none(), "no session was stored");

    // The forged state matched no flight, so the REAL bind is still parked — but its
    // state is a secret the forger never held, and any other guess is refused the same
    // way. A callback without an issuer (iss) is refused outright: RFC 9207's
    // issuer identification is mandatory on this surface.
    let no_issuer = get(address, "/?code=code-1&state=whatever");
    assert!(no_issuer.contains("invalid_callback") && no_issuer.contains("no issuer"), "a missing iss is an honest refusal: {no_issuer}");
}

#[test]
fn a_real_callback_without_an_issuer_is_refused() {
    let server = FakeServer::start();
    server.add_account("honest.bsky.example", "unused-password", "did:plc:honest");
    let (hub, _dir, _page, address) = operator_workspace("no-iss", &server);
    let companion = hub.create_companion("honest", None).expect("companion");

    // A perfectly valid dance, except the issuer is stripped from the callback.
    let callback = start_bind(address, &server, &companion.local_id, None);
    let stripped = match callback.find("&iss=") {
        Some(at) => callback[..at].to_string(),
        None => panic!("the fake AS always sends iss: {callback}"),
    };
    let path = stripped.strip_prefix(&format!("http://{address}")).unwrap_or(&stripped);
    let refused = get(address, path);
    assert!(refused.contains("invalid_callback") && refused.contains("no issuer"), "the refusal is honest: {refused}");
    assert!(hub.companion(&companion.local_id).unwrap().bound_did.is_none(), "nothing was bound without the issuer");
    assert!(hub.store().lock().unwrap().atproto_session(&companion.local_id).is_none(), "no session was stored");
}

#[test]
fn an_aged_out_bind_flight_is_refused_honestly() {
    let server = FakeServer::start();
    server.add_account("late.bsky.example", "unused-password", "did:plc:late");
    let (hub, _dir, _page, address) = operator_workspace("aged", &server);
    let companion = hub.create_companion("late", None).expect("companion");
    // The test seam: the ten-minute park window shrinks to gone.
    hub.set_bind_flight_ttl_ms(1);

    let callback = start_bind(address, &server, &companion.local_id, None);
    std::thread::sleep(std::time::Duration::from_millis(25));
    let path = callback.strip_prefix(&format!("http://{address}")).unwrap_or(&callback);
    let late = get(address, path);
    assert!(late.contains("bind_expired") && late.contains("Sign-in didn’t finish"), "the aged-out flight refuses: {late}");
    assert!(hub.companion(&companion.local_id).unwrap().bound_did.is_none(), "nothing was bound");
    assert!(hub.store().lock().unwrap().atproto_session(&companion.local_id).is_none(), "no session was stored");
}

#[test]
fn the_handle_escape_hatch_still_discovers_the_self_hosted_path() {
    let server = FakeServer::start();
    server.add_account("selfhosted.example", "unused-password", "did:plc:selfhosted");
    let (hub, _dir, _page, address) = operator_workspace("escape", &server);
    let companion = hub.create_companion("selfhosted", None).expect("companion");

    // `?handle=` runs the discovery path BEFORE the flow: resolver, DID document,
    // the PDS's protected-resource metadata, the AS metadata — then the PAR.
    let callback_page = drive_bind(address, &server, &companion.local_id, Some("selfhosted.example"));
    assert!(callback_page.contains("Signed in as <strong>selfhosted.example</strong>"), "the discovery path completes: {callback_page}");

    let requests = server.requests();
    let par_position = requests.iter().position(|request| request.path == "/oauth/par").expect("the PAR request");
    for expected in [
        "/xrpc/com.atproto.identity.resolveHandle",
        "/did:plc:selfhosted",
        "/.well-known/oauth-protected-resource",
        "/.well-known/oauth-authorization-server",
    ] {
        let found = requests[..par_position].iter().any(|request| request.path.starts_with(expected));
        assert!(found, "the discovery step {expected} ran before the PAR");
    }
    assert_eq!(hub.companion(&companion.local_id).unwrap().bound_did.as_deref(), Some("did:plc:selfhosted"));
    let session = hub.store().lock().unwrap().atproto_session(&companion.local_id).expect("session");
    assert_eq!(session.did, "did:plc:selfhosted", "the discovery path agrees with the token's subject");
    assert_eq!(session.pds, server.origin());
}

#[test]
fn an_unknown_provider_or_companion_is_an_honest_404_page() {
    let server = FakeServer::start();
    let (hub, _dir, _page, address) = operator_workspace("forty-four", &server);
    let companion = hub.create_companion("plain", None).expect("companion");

    let provider = get(address, &format!("/bind/{}/github", companion.local_id));
    assert!(provider.starts_with("HTTP/1.1 404"), "the unknown provider is a 404: {provider}");
    assert!(provider.contains("Unknown bind provider 'github'") && provider.contains("atproto"), "the page names what lives there: {provider}");

    let unknown = get(address, "/bind/ox_missing/atproto");
    assert!(unknown.starts_with("HTTP/1.1 404"), "the unknown companion is a 404: {unknown}");
    assert!(unknown.contains("No local companion"), "the page says so honestly: {unknown}");

    // No POST surface exists on the bind route at all: the bind is a navigation.
    let mut post = TcpStream::connect(address).expect("connect");
    let posted = http_round_trip(
        &mut post,
        &format!(
            "POST /bind/{}/atproto HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: 7\r\nConnection: close\r\n\r\nhandle=x",
            companion.local_id
        ),
    );
    assert!(posted.starts_with("HTTP/1.1 404"), "no POST bind route remains: {posted}");
    assert!(hub.companion(&companion.local_id).unwrap().bound_did.is_none());
}

// ---------- the silent refresh ----------

/// The per-PDS nonce cache's payoff, pinned on the wire: the FIRST mint at a PDS pays
/// the RFC 9449 challenge round trip (ask without nonce → 401 → retry with it); the
/// SECOND mint at the SAME PDS already carries the cached nonce — one ask, no
/// challenge. The authorization server's nonce context never enters either ask.
#[test]
fn the_pds_nonce_cache_saves_the_second_mint_one_round_trip() {
    let server = FakeServer::start();
    server.add_account("first.bsky.example", "unused-password", "did:plc:first");
    let (hub, _dir, _page, address) = operator_workspace("nonce-cache", &server);
    let one = hub.create_companion("first", None).expect("companion");
    let two = hub.create_companion("second", None).expect("companion");

    // The default flow signs in as the provider's LAST registered account.
    let first_bind = drive_bind(address, &server, &one.local_id, None);
    assert!(first_bind.contains("Signed in as <strong>first.bsky.example</strong>"), "the first companion binds: {first_bind}");
    // The `?handle=` discovery path re-points the provider's session at the second
    // account (the default flow would re-sign the last resolved DID).
    server.add_account("second.bsky.example", "unused-password", "did:plc:second");
    let second_bind = drive_bind(address, &server, &two.local_id, Some("second.bsky.example"));
    assert!(second_bind.contains("Signed in as <strong>second.bsky.example</strong>"), "the second companion binds: {second_bind}");

    hub.enroll_bound(&one.local_id, server.origin()).expect("the first bound enrollment");
    hub.enroll_bound(&two.local_id, server.origin()).expect("the second bound enrollment");

    let requests = server.requests();
    let mints = requests
        .iter()
        .filter(|request| request.path.starts_with("/xrpc/com.atproto.server.getServiceAuth"))
        .count();
    assert_eq!(mints, 3, "challenge + retry on the first mint, ONE ask on the second (the cached PDS nonce)");
    let third = requests
        .iter()
        .filter(|request| request.path.starts_with("/xrpc/com.atproto.server.getServiceAuth"))
        .nth(2)
        .expect("the cached-nonce ask");
    let payload = proof_payload_of(&third.dpop);
    assert!(payload.contains("\"nonce\""), "the cached-nonce ask embeds the PDS's challenge nonce: {payload}");
    // The fake PDS validates that claim against its OWN constant (never the AS's), so
    // this ask succeeding is itself the pin: no authorization-server nonce rode it.
}

#[test]
fn an_expired_oauth_session_refreshes_silently_before_use() {
    let server = FakeServer::start();
    server.add_account("wanderer.bsky.example", "unused-password", "did:plc:wanderer");
    // Tokens issued from here expire inside the refresh margin.
    server.set_oauth_token_lifetime(30);
    let (hub, _dir, _page, address) = operator_workspace("refresh", &server);
    let companion = hub.create_companion("wanderer", None).expect("companion");

    let callback_page = drive_bind(address, &server, &companion.local_id, None);
    assert!(callback_page.contains("Signed in as <strong>wanderer.bsky.example</strong>"), "the bind completed: {callback_page}");
    let stale = hub.store().lock().unwrap().atproto_session(&companion.local_id).expect("session").access_jwt;

    // The enrollment uses the PDS session: the stale token must refresh first (silently
    // — the enrollment simply succeeds and the stored token changes).
    let entry = hub.enroll_bound(&companion.local_id, server.origin()).expect("bound enrollment with a silent refresh");
    assert_eq!(entry.did.as_deref(), Some("did:plc:wanderer"));

    let requests = server.requests();
    let refresh = requests
        .iter()
        .find(|request| request.path == "/oauth/token" && request.body.get("grant_type").and_then(Value::as_str) == Some("refresh_token"))
        .expect("a refresh grant ran before use");
    assert_eq!(
        refresh.body.get("client_id").and_then(Value::as_str),
        Some(adapter_auth_atproto::atproto_oauth::bind_client_id().as_str()),
        "the refresh presents the client id the grant lives under"
    );

    let renewed = hub.store().lock().unwrap().atproto_session(&companion.local_id).expect("renewed session");
    assert_ne!(renewed.access_jwt, stale, "the access token was replaced");
    assert!(renewed.access_jwt.split('.').count() == 3, "still JWT-shaped");
    assert!(renewed.refresh_jwt.as_deref().is_some_and(|token| token.starts_with("rt_oauth")), "the rotated refresh token is stored");

    // The proof mint rode the renewed token — the DPoP scheme with a proof binding it
    //, on the proof-demanding PDS, after the PDS's own nonce challenge settled.
    let service_auth = requests
        .iter()
        .find(|request| request.path.starts_with("/xrpc/com.atproto.server.getServiceAuth"))
        .expect("service-auth request");
    assert_eq!(service_auth.bearer, format!("DPoP {}", renewed.access_jwt), "the refreshed token minted the proof");
    assert!(!service_auth.dpop.is_empty(), "the PDS call carried a DPoP proof for the renewed token");
}

// ---------- the fixed port and its honest conflicts ----------

#[test]
fn the_port_discipline_is_default_then_environment_then_flag() {
    use companion_lobby::adapters::manager::{port_from, DEFAULT_PORT};
    assert_eq!(port_from(None, None).unwrap(), DEFAULT_PORT);
    assert_eq!(port_from(None, Some("")).unwrap(), DEFAULT_PORT, "an empty environment value falls back");
    assert_eq!(port_from(None, Some("5321")).unwrap(), 5321);
    assert_eq!(port_from(Some(5322), Some("5321")).unwrap(), 5322, "the flag wins");
    assert!(port_from(Some(0), None).is_err(), "0 would pick a random port");
    assert!(port_from(None, Some("0")).is_err(), "0 would pick a random port");
    let refused = port_from(None, Some("not-a-port")).expect_err("garbage is an honest refusal");
    assert!(refused.contains("COMPANION_LOBBY_PORT"), "the message names the knob: {refused}");
}

#[test]
fn an_in_use_port_is_a_refusal_naming_the_holder() {
    let dir = std::env::temp_dir().join(format!("companion-lobby-bind-conflict-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    // The lockfile names the one-process holder (us, in this test).
    let _lock = companion_lobby::adapters::lockfile::DataDirLock::acquire(&dir, false).expect("lock");
    // Another listener holds the fixed port. The operator's real manager may already
    // be serving there — anything answering on the fixed port makes this test's
    // conflict indistinguishable, so it honestly stands down instead of squatting.
    let Ok(squatter) = TcpListener::bind(("127.0.0.1", manager::DEFAULT_PORT)) else {
        eprintln!("the fixed port {} is already in use; skipping the bind-conflict check", manager::DEFAULT_PORT);
        return;
    };
    let refused = manager::bind_listener(&dir, manager::DEFAULT_PORT).expect_err("the bind conflict refuses");
    drop(squatter);
    assert!(refused.contains("already listening"), "names the conflict: {refused}");
    assert!(refused.contains(&format!("pid={}", std::process::id())), "names the lockfile holder: {refused}");
    assert!(refused.contains("COMPANION_LOBBY_PORT"), "offers the escape: {refused}");
    // With the port free again, the same call binds.
    assert!(manager::bind_listener(&dir, manager::DEFAULT_PORT).is_ok(), "the fixed port binds once free");
}

// ---------- the 1:1 model: sign-in as creation, duplicates, re-binds ----------

/// The add screen's one flow: a companion is BORN from the sign-in, named after the
/// account, granted the Tangent service by default. No bare creation exists anywhere.
#[test]
fn a_new_signin_creates_the_companion_named_after_the_account() {
    let server = FakeServer::start();
    server.add_account("nova.bsky.example", "unused-password", "did:plc:nova");
    let (hub, _dir, _page, address) = operator_workspace("creation", &server);
    assert!(hub.companions().is_empty(), "nothing exists before any sign-in");

    // The add route starts the same dance with no companion to name.
    let callback = start_bind(address, &server, "new", None);
    let callback_path = callback.strip_prefix(&format!("http://{address}")).unwrap_or(&callback);
    let page = get(address, callback_path);
    assert!(page.starts_with("HTTP/1.1 200"), "the callback answers: {page}");
    assert!(page.contains("nova.bsky.example is here."), "creation is named: {page}");

    let companions = hub.companions();
    assert_eq!(companions.len(), 1, "exactly one companion was born");
    assert_eq!(companions[0].handle, "nova.bsky.example", "the account's handle is the companion's name");
    assert_eq!(companions[0].bound_did.as_deref(), Some("did:plc:nova"));
    let session = hub.store().lock().unwrap().atproto_session(&companions[0].local_id).expect("the credential rode in with the companion");
    assert_eq!(session.did, "did:plc:nova");
    assert_eq!(session.services, vec!["tangent".to_string()], "a fresh credential grants the Tangent service");
}

/// One account, one companion: a repeat sign-in creates nothing and routes the
/// operator to the companion that already holds the account.
#[test]
fn a_duplicate_signin_routes_to_the_existing_companion() {
    let server = FakeServer::start();
    server.add_account("lumen.bsky.example", "unused-password", "did:plc:lumen");
    let (hub, _dir, _page, address) = operator_workspace("duplicate", &server);
    let companion = hub.create_companion("lumen", None).expect("companion");
    drive_bind(address, &server, &companion.local_id, None);

    // The same account walks the add flow again.
    let callback = start_bind(address, &server, "new", None);
    let callback_path = callback.strip_prefix(&format!("http://{address}")).unwrap_or(&callback);
    let page = get(address, callback_path);
    assert!(page.contains("This account already has a companion."), "the duplicate is friendly: {page}");
    assert!(page.contains(&format!("href=\"/companion/{}\"", companion.local_id)), "the page routes to the companion: {page}");
    assert_eq!(hub.companions().len(), 1, "no second companion appeared");
    assert_eq!(hub.companions()[0].handle, "lumen", "the existing companion is untouched");
}

/// A re-bind replaces the credential and carries the operator's service grants
/// forward — renewing access never silently widens or narrows it.
#[test]
fn a_rebind_replaces_the_session_and_carries_the_grants() {
    let server = FakeServer::start();
    server.add_account("lumen.bsky.example", "unused-password", "did:plc:lumen");
    let (hub, _dir, _page, address) = operator_workspace("rebind", &server);
    let companion = hub.create_companion("lumen", None).expect("companion");
    drive_bind(address, &server, &companion.local_id, None);
    hub.set_companion_services(&companion.local_id, &[]).expect("withdraw the Tangent grant");
    // The fake mints deterministic tokens, so replacement is proven by poisoning the
    // stored session: a re-bind that kept it would leave the poison in place.
    {
        let mut store = hub.store().lock().unwrap();
        let mut stale = store.atproto_session(&companion.local_id).expect("first session");
        stale.access_jwt = "stale".to_string();
        store.set_atproto_session(&companion.local_id, stale);
    }

    drive_bind(address, &server, &companion.local_id, None);
    let second = hub.store().lock().unwrap().atproto_session(&companion.local_id).expect("re-bound session");
    assert_ne!(second.access_jwt, "stale", "the re-bind replaced the poisoned session");
    assert!(second.services.is_empty(), "the operator's grants carried forward");
    assert_eq!(hub.companions().len(), 1, "a re-bind creates nothing");
}

/// Sign-in pendings are per companion: two companions can wait for their sign-ins
/// at once, and completing one bind clears and resumes only its own.
#[test]
fn signin_pendings_are_per_companion_and_cleared_by_completion() {
    let server = FakeServer::start();
    // The fake signs its default flow in as the LAST registered account, so the
    // second account is registered only when the second sign-in is about to run.
    server.add_account("lumen.bsky.example", "unused-password", "did:plc:lumen");
    let (hub, _dir, _page, address) = operator_workspace("pendings", &server);
    let first = hub.create_companion("lumen", None).expect("first");
    let second = hub.create_companion("ox_omega", None).expect("second");

    let waiting = |persona: &str| {
        hub.invoke(IntakeChannel::Mcp, "Connect", &serde_json::json!({
            "service": "tangent", "persona": persona, "address": server.origin()
        }))
    };
    assert_eq!(waiting("lumen").structured.pointer("/problem/code").and_then(Value::as_str), Some("operator_action_needed"));
    assert_eq!(waiting("ox_omega").structured.pointer("/problem/code").and_then(Value::as_str), Some("operator_action_needed"));

    // Completing the FIRST companion's sign-in enrolls exactly it; the second stays
    // waiting, its own pending untouched.
    drive_bind(address, &server, &first.local_id, None);
    assert_eq!(hub.enrollments_of(&first.local_id).len(), 1, "the first companion enrolled");
    assert!(hub.enrollments_of(&second.local_id).is_empty(), "the second still waits");
    assert_eq!(waiting("ox_omega").structured.pointer("/problem/code").and_then(Value::as_str), Some("operator_action_needed"));

    server.add_account("oxomega.bsky.example", "unused-password", "did:plc:oxomega");
    // The waiting connect must be FRESH relative to the flight: park the flight, then
    // let the model ask again, then complete the sign-in — the real world's order.
    // The fake signs its default flow in as the DID its resolver last served — the
    // first bind left lumen there — so the second sign-in names its account through
    // the ?handle= discovery path, exactly as a self-hosted operator would.
    let callback = start_bind(address, &server, &second.local_id, Some("oxomega.bsky.example"));
    assert_eq!(waiting("ox_omega").structured.pointer("/problem/code").and_then(Value::as_str), Some("operator_action_needed"));
    let callback_path = callback.strip_prefix(&format!("http://{address}")).unwrap_or(&callback);
    get(address, callback_path);
    assert_eq!(hub.enrollments_of(&second.local_id).len(), 1, "and then the second enrolls on its own sign-in");
}
