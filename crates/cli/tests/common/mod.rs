//! Shared test support: a scripted fake Tangent experience server speaking just enough HTTP
//! for ureq, with per-request recording for assertions. All data is synthetic and labelled so.
//! The placeholder `ORIGIN` in scripted payloads is replaced with the listener's real origin,
//! because the connector validates references against the exact canonical origin. The same
//! listener also plays the whole atproto OAuth cast: the handle resolver, the PLC-style DID
//! directory, the PDS's protected-resource metadata, and a fake authorization server (PAR +
//! authorize redirect + token/refresh with real DPoP-proof validation).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use companion_lobby::application::hub::ConnectorHub;
use companion_core::domain::companion::{AccountSession, CallerId, Enrollment};

#[allow(dead_code)]
pub const LUMEN_CREDENTIAL: &str = "ts_lumen_test_credential_000000000000000000";
#[allow(dead_code)]
pub const STEWARD_CREDENTIAL: &str = "ts_steward_test_credential_000000000000000";
#[allow(dead_code)]
pub const ACTION_ONLY_CREDENTIAL: &str = "ts_action_only_test_credential_00000000000";
#[allow(dead_code)]
pub const STALE_STEWARD_CREDENTIAL: &str = "ts_stale_steward_test_credential_000000000";
#[allow(dead_code)]
pub const SILENT_STEWARD_CREDENTIAL: &str = "ts_silent_steward_test_credential_00000000";
#[allow(dead_code)]
pub const REVOKED_CREDENTIAL: &str = "ts_revoked_test_credential_0000000000000";

pub fn origin(listener: &TcpListener) -> String {
    format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port())
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Received {
    pub method: String,
    pub path: String,
    pub bearer: String,
    /// The DPoP proof header the request carried, when it carried one (empty string
    /// otherwise) — assertions prove OAuth resource requests are DPoP-proved.
    pub dpop: String,
    pub body: Value,
}

/// The fake server's proof audience (a synthetic fixture service DID).
pub const AUDIENCE: &str = "did:plc:fixture-tangent-server";

/// One synthetic atproto account the fake PDS knows.
#[derive(Debug, Clone)]
pub struct FakeAccount {
    pub handle: String,
    pub password: String,
    pub did: String,
    /// When set, createSession still succeeds but getServiceAuth rejects the session —
    /// the "PDS session expired, re-bind" scenario.
    pub expiring: bool,
}

/// Script modes of the bound-exchange surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(dead_code)]
pub enum BoundMode {
    /// The full recipe works: discovery → getServiceAuth → /mcp/token.
    #[default]
    Ready,
    /// Discovery answers 503 `exchange_unconfigured` (no proof audience).
    AudienceUnconfigured,
    /// Discovery advertises a different exchange method.
    WrongMethod,
    /// Discovery's audience carries characters that would inject query structure if
    /// the client failed to percent-encode.
    TamperedAudience,
    /// `/mcp/token` rejects every proof with 401 `invalid_service_proof`.
    RejectProofs,
    /// `/mcp/token` answers 403 `participant_suspended`.
    Suspended,
}

/// Shared mutable state of the bound-exchange surface.
#[derive(Clone)]
pub struct BoundScript {
    pub mode: BoundMode,
    accounts: Arc<Mutex<Vec<FakeAccount>>>,
    proofs: Arc<Mutex<std::collections::HashMap<String, bool>>>,
    counter: Arc<Mutex<u32>>,
}

impl BoundScript {
    fn new(mode: BoundMode) -> Self {
        Self {
            mode,
            accounts: Arc::new(Mutex::new(Vec::new())),
            proofs: Arc::new(Mutex::new(std::collections::HashMap::new())),
            counter: Arc::new(Mutex::new(0)),
        }
    }
}

/// One pushed authorization request the fake authorization server parked.
#[derive(Clone)]
struct ParRecord {
    #[allow(dead_code)]
    client_id: String,
    redirect_uri: String,
    state: String,
    code_challenge: String,
    dpop: p256::ecdsa::VerifyingKey,
}

/// The fake authorization server's live state: parked pushed requests, issued codes and
/// refresh tokens, the DPoP jti replay guard, and the initial token lifetime knob.
#[derive(Clone)]
struct OauthScript {
    pars: Arc<Mutex<std::collections::HashMap<String, ParRecord>>>,
    codes: Arc<Mutex<std::collections::HashMap<String, (ParRecord, String)>>>,
    refresh_tokens: Arc<Mutex<std::collections::HashMap<String, String>>>,
    jtis: Arc<Mutex<std::collections::HashSet<String>>>,
    counter: Arc<Mutex<u32>>,
    /// Lifetime baked into newly issued (not refreshed) access tokens — the
    /// refresh-before-use scenario shrinks it.
    initial_lifetime_secs: Arc<Mutex<u64>>,
    /// The DID whose document the resolver last served — the account the auto-approving
    /// authorize endpoint signs in as.
    last_resolved_did: Arc<Mutex<Option<String>>>,
}

impl OauthScript {
    fn new() -> Self {
        Self {
            pars: Arc::new(Mutex::new(std::collections::HashMap::new())),
            codes: Arc::new(Mutex::new(std::collections::HashMap::new())),
            refresh_tokens: Arc::new(Mutex::new(std::collections::HashMap::new())),
            jtis: Arc::new(Mutex::new(std::collections::HashSet::new())),
            counter: Arc::new(Mutex::new(0)),
            initial_lifetime_secs: Arc::new(Mutex::new(7200)),
            last_resolved_did: Arc::new(Mutex::new(None)),
        }
    }

    fn next(&self, prefix: &str) -> String {
        let mut counter = self.counter.lock().unwrap();
        *counter += 1;
        format!("{prefix}{}", *counter)
    }
}

/// The DPoP nonce the fake authorization server demands on the token endpoint — its
/// OWN context, deliberately distinct from the PDS's below (the bug this suite pins:
/// an AS nonce never satisfies a resource server).
const FAKE_DPOP_NONCE: &str = "fake-dpop-nonce-1";
/// The DPoP nonce the fake PDS (a separate resource server) demands — issued via 401
/// challenge, never accepted from the AS's context.
const PDS_DPOP_NONCE: &str = "fake-pds-dpop-nonce-1";

/// The scoped localhost client id the connector binds under — the fake's single
/// source mirrors the crate's own construction, so the two can never drift.
fn scoped_client_id() -> String {
    adapter_auth_atproto::atproto_oauth::bind_client_id()
}

/// A scripted experience API. Responses are synthetic and the post registry gives the
/// crash-retry scenario its statefulness. The same listener also plays the bound-exchange
/// roles: the Tangent server's discovery document and `/mcp/token`, and the account's PDS
/// (`getServiceAuth`).
#[allow(dead_code)]
pub struct FakeServer {
    pub requests: Arc<Mutex<Vec<Received>>>,
    posts: Arc<Mutex<HashMap<String, u32>>>,
    bound: BoundScript,
    oauth: OauthScript,
    origin: String,
}

impl FakeServer {
    pub fn start() -> Self {
        Self::start_with(BoundMode::Ready)
    }

    /// A server whose bound-exchange surface is in the given mode.
    #[allow(dead_code)]
    pub fn start_bound_with(mode: BoundMode) -> Self {
        Self::start_with(mode)
    }

    fn start_with(bound_mode: BoundMode) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = origin(&listener);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let posts = Arc::new(Mutex::new(HashMap::new()));
        let bound = BoundScript::new(bound_mode);
        let oauth = OauthScript::new();
        let server = Self {
            requests: requests.clone(),
            posts: posts.clone(),
            bound: bound.clone(),
            oauth: oauth.clone(),
            origin: url.clone(),
        };
        std::thread::Builder::new()
            .name("fake-experience".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { break };
                    let requests = requests.clone();
                    let posts = posts.clone();
                    let bound = bound.clone();
                    let oauth = oauth.clone();
                    let origin = url.clone();
                    std::thread::spawn(move || {
                        serve_connection(stream, requests, posts, bound, oauth, origin)
                    });
                }
            })
            .expect("server thread");
        server
    }

    /// Registers a synthetic atproto account the fake PDS will accept at createSession.
    #[allow(dead_code)]
    pub fn add_account(&self, handle: &str, password: &str, did: &str) {
        self.bound.accounts.lock().unwrap().push(FakeAccount {
            handle: handle.to_string(),
            password: password.to_string(),
            did: did.to_string(),
            expiring: false,
        });
    }

    /// Like [`FakeServer::add_account`], but the minted PDS session is already expired.
    #[allow(dead_code)]
    pub fn add_expiring_account(&self, handle: &str, password: &str, did: &str) {
        self.bound.accounts.lock().unwrap().push(FakeAccount {
            handle: handle.to_string(),
            password: password.to_string(),
            did: did.to_string(),
            expiring: true,
        });
    }

    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// Shrinks the lifetime baked into newly issued access tokens (seconds), for the
    /// silent-refresh scenario. Refreshed tokens always come back long-lived.
    #[allow(dead_code)]
    pub fn set_oauth_token_lifetime(&self, seconds: u64) {
        *self.oauth.initial_lifetime_secs.lock().unwrap() = seconds;
    }

    pub fn requests(&self) -> Vec<Received> {
        self.requests.lock().unwrap().clone()
    }

    /// How many times the scripted Topic accepted a write for this request id.
    #[allow(dead_code)]
    pub fn dispatches(&self, request_id: &str) -> u32 {
        self.posts.lock().unwrap().get(request_id).copied().unwrap_or(0)
    }

}

fn serve_connection(
    stream: TcpStream,
    requests: Arc<Mutex<Vec<Received>>>,
    posts: Arc<Mutex<HashMap<String, u32>>>,
    bound: BoundScript,
    oauth: OauthScript,
    origin: String,
) {
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(clone) => clone,
        Err(_) => return,
    });
    let mut writer = stream;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let mut parts = line.split_whitespace();
        let method = parts.next().unwrap_or_default().to_string();
        let path = parts.next().unwrap_or_default().to_string();
        let mut content_length = 0usize;
        let mut bearer = String::new();
        let mut dpop = String::new();
        let mut dpop_nonce = String::new();
        let mut content_type = String::new();
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header).unwrap_or(0) == 0 {
                return;
            }
            let header = header.trim();
            if header.is_empty() {
                break;
            }
            if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                content_length = value.trim().parse().unwrap_or(0);
            }
            if let Some(value) = header.strip_prefix("Authorization:") {
                bearer = value.trim().to_string();
            }
            if let Some(value) = header.strip_prefix("DPoP:") {
                dpop = value.trim().to_string();
            }
            if let Some(value) = header.strip_prefix("DPoP-Nonce:") {
                dpop_nonce = value.trim().to_string();
            }
            if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-type:") {
                content_type = value.trim().to_string();
            }
        }
        let mut body_bytes = vec![0u8; content_length];
        if content_length > 0 && reader.read_exact(&mut body_bytes).is_err() {
            return;
        }
        // Form posts (the OAuth surface) record as a JSON object of their pairs.
        // Form-urlencoded semantics: '+' is a space, '%' is an escape.
        let body = if content_type.starts_with("application/x-www-form-urlencoded") {
            let text = String::from_utf8_lossy(&body_bytes);
            let mut object = serde_json::Map::new();
            for pair in text.split('&').filter(|pair| !pair.is_empty()) {
                let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
                object.insert(percent_decode(name), Value::String(percent_decode(&value.replace('+', " "))));
            }
            Value::Object(object)
        } else {
            serde_json::from_slice(&body_bytes).unwrap_or(Value::Null)
        };
        requests.lock().unwrap().push(Received {
            method: method.clone(),
            path: path.clone(),
            bearer: bearer.clone(),
            dpop: dpop.clone(),
            body: body.clone(),
        });
        if bearer.ends_with(REVOKED_CREDENTIAL) {
            let _ = writer.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: 47\r\nConnection: keep-alive\r\n\r\n{\"error\":\"participant_authentication_required\"}");
            let _ = writer.flush();
            continue;
        }
        let extras = RequestExtras { dpop, dpop_nonce };
        match respond(&method, &path, &body, &bearer, &posts, &bound, &oauth, &origin, &extras) {
            Script::Body(status, payload) => {
                let text = serde_json::to_string(&payload).unwrap().replace("ORIGIN", &origin);
                let reason = match status {
                    200 => "OK",
                    201 => "Created",
                    401 => "Unauthorized",
                    409 => "Conflict",
                    _ => "Status",
                };
                let _ = writer.write_all(
                    format!(
                        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
                        status,
                        reason,
                        text.len()
                    )
                    .as_bytes(),
                );
                let _ = writer.write_all(text.as_bytes());
                let _ = writer.flush();
            }
            // The authorize endpoint's answer: a redirect to the loopback callback.
            Script::Moved(location) => {
                let head = format!(
                    "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = writer.write_all(head.as_bytes());
                let _ = writer.flush();
                return;
            }
            // The DPoP nonce challenge (RFC 9449): the error body plus the nonce header.
            Script::NonceChallenge(status, payload, nonce) => {
                let text = serde_json::to_string(&payload).unwrap();
                let head = format!(
                    "HTTP/1.1 {status} Status\r\nContent-Type: application/json\r\nDPoP-Nonce: {nonce}\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
                    text.len()
                );
                let _ = writer.write_all(head.as_bytes());
                let _ = writer.write_all(text.as_bytes());
                let _ = writer.flush();
            }
            // A lost response: the connection dies without an answer.
            Script::Lost => return,
        }
    }
}

/// The OAuth-surface headers one request carried (proof + nonce).
struct RequestExtras {
    dpop: String,
    dpop_nonce: String,
}

/// What the scripted server answers: a status plus JSON body, a redirect, a DPoP nonce
/// challenge, or a dropped connection.
enum Script {
    Body(u16, Value),
    Moved(String),
    NonceChallenge(u16, Value, String),
    Lost,
}

fn respond(
    method: &str,
    path: &str,
    body: &Value,
    bearer: &str,
    posts: &Arc<Mutex<HashMap<String, u32>>>,
    bound: &BoundScript,
    oauth: &OauthScript,
    origin: &str,
    extras: &RequestExtras,
) -> Script {
    let clean = path.split('?').next().unwrap_or(path);
    match (method, clean) {
        // ---- the atproto OAuth cast (resolver, DID directory, AS metadata, PAR, authorize, token) ----
        ("GET", "/xrpc/com.atproto.identity.resolveHandle") => {
            let handle = query_param(path, "handle").unwrap_or_default().to_ascii_lowercase();
            let account = bound
                .accounts
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find(|account| account.handle.to_ascii_lowercase() == handle)
                .cloned();
            match account {
                Some(account) => Script::Body(200, json!({ "did": account.did })),
                None => Script::Body(400, json!({ "error": "InvalidRequest", "message": "unable to resolve handle" })),
            }
        }
        _ if clean.starts_with("/did:") => {
            let did = clean.trim_start_matches('/');
            let account = bound
                .accounts
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find(|account| account.did == did)
                .cloned();
            match account {
                Some(account) => {
                    *oauth.last_resolved_did.lock().unwrap() = Some(account.did.clone());
                    Script::Body(
                        200,
                        json!({
                            "@context": ["https://www.w3.org/ns/did/v1"],
                            "id": account.did,
                            "alsoKnownAs": [format!("at://{}", account.handle)],
                            "verificationMethod": [],
                            "service": [
                                { "id": "#atproto_pds", "type": "AtprotoPds", "serviceEndpoint": origin }
                            ],
                        }),
                    )
                }
                None => Script::Body(404, json!({ "error": "NotFound", "message": "did not found" })),
            }
        }
        ("GET", "/.well-known/oauth-protected-resource") => Script::Body(
            200,
            json!({
                "authorization_servers": [origin],
                "bearer_methods_supported": ["header"],
                "resource": origin,
                "resource_documentation": "https://atproto.com",
                "scopes_supported": [],
            }),
        ),
        ("GET", "/.well-known/oauth-authorization-server") => Script::Body(
            200,
            json!({
                "issuer": origin,
                "authorization_endpoint": format!("{origin}/oauth/authorize"),
                "token_endpoint": format!("{origin}/oauth/token"),
                "pushed_authorization_request_endpoint": format!("{origin}/oauth/par"),
                "require_pushed_authorization_requests": true,
                "token_endpoint_auth_methods_supported": ["none", "private_key_jwt"],
                "dpop_signing_alg_values_supported": ["ES256"],
                "scopes_supported": ["atproto"],
                "response_types_supported": ["code"],
                "grant_types_supported": ["authorization_code", "refresh_token"],
                "code_challenge_methods_supported": ["S256"],
                "redirect_uris": ["http://127.0.0.1/", "http://[::1]/"],
            }),
        ),
        ("POST", "/oauth/par") => {
            // The local-client profile, strictly: loopback IP-literal client id,
            // loopback IP-literal redirect with an empty path, atproto scope, PKCE
            // S256, NO nonce (the pushed request_uri replaces it — the public server
            // refuses a nonce; so do we) and NO login_hint (no interstitial to
            // pre-declare an account: the provider's own UI selects the account).
            if body.get("nonce").is_some_and(|value| !value.is_null()) {
                return Script::Body(400, json!({ "error": "invalid_request", "error_description": "Unsupported \"nonce\" parameter" }));
            }
            if body.get("login_hint").is_some_and(|value| !value.is_null()) {
                return Script::Body(400, json!({ "error": "invalid_request", "error_description": "Unsupported \"login_hint\" parameter" }));
            }
            // The localhost virtual client's requestable scopes live in its client_id
            // query parameter; the scoped form declares the bind scope set, the bare
            // bare origin declares the profile base only.
            let client_id = body.get("client_id").and_then(Value::as_str).unwrap_or_default().to_string();
            let declared: &[&str] = if client_id == scoped_client_id() {
                &["atproto", "rpc:local.tangent.mcp.exchange?aud=*"]
            } else if client_id == "http://localhost" {
                &["atproto"]
            } else {
                return Script::Body(400, json!({ "error": "invalid_request", "error_description": "Unsupported client_id" }));
            };
            let requested = body.get("scope").and_then(Value::as_str).unwrap_or_default();
            if !requested.split(' ').filter(|scope| !scope.is_empty()).all(|scope| declared.contains(&scope)) {
                return Script::Body(400, json!({ "error": "invalid_scope", "error_description": format!("scope outside the client's declared set: {requested}") }));
            }
            let redirect = body.get("redirect_uri").and_then(Value::as_str).unwrap_or_default().to_string();
            let redirect_ok = redirect.starts_with("http://127.0.0.1") || redirect.starts_with("http://[::1]");
            // Loopback root only: exactly the scheme slashes plus the trailing slash.
            let path_ok = redirect.matches('/').count() == 3 && redirect.ends_with('/');
            if !redirect_ok || !path_ok {
                return Script::Body(400, json!({ "error": "invalid_request", "error_description": format!("Invalid redirect_uri {redirect}") }));
            }
            if body.get("response_type").and_then(Value::as_str) != Some("code") {
                return Script::Body(400, json!({ "error": "invalid_request", "error_description": "response_type must be code" }));
            }
            if !body
                .get("scope")
                .and_then(Value::as_str)
                .is_some_and(|scope| scope.split(' ').any(|part| part == "atproto"))
            {
                return Script::Body(400, json!({ "error": "invalid_scope", "error_description": "the atproto scope is required" }));
            }
            let state_ok = body
                .get("state")
                .and_then(Value::as_str)
                .is_some_and(|state| !state.is_empty());
            let challenge = body.get("code_challenge").and_then(Value::as_str).unwrap_or_default().to_string();
            let method = body.get("code_challenge_method").and_then(Value::as_str).unwrap_or_default().to_string();
            if !state_ok || challenge.len() < 40 || method != "S256" {
                return Script::Body(400, json!({ "error": "invalid_request", "error_description": "state and an S256 code_challenge are required" }));
            }
            let record = match validate_dpop(extras, "POST", &format!("{origin}/oauth/par"), None, None, oauth) {
                Ok(key) => ParRecord {
                    client_id: "http://localhost".into(),
                    redirect_uri: redirect,
                    state: body.get("state").and_then(Value::as_str).unwrap_or_default().to_string(),
                    code_challenge: challenge,
                    dpop: key,
                },
                Err(problem) => return Script::Body(401, json!({ "error": "invalid_dpop_proof", "error_description": problem })),
            };
            let request_uri = oauth.next("urn:ietf:params:oauth:request_uri:req-");
            oauth.pars.lock().unwrap().insert(request_uri.clone(), record);
            Script::Body(201, json!({ "request_uri": request_uri, "expires_in": 299 }))
        }
        ("GET", "/oauth/authorize") => {
            let client_id = query_param(path, "client_id").unwrap_or_default();
            let request_uri = query_param(path, "request_uri").unwrap_or_default();
            if client_id != adapter_auth_atproto::atproto_oauth::bind_client_id() {
                return Script::Body(400, json!({ "error": "invalid_request", "error_description": "Unsupported client_id" }));
            }
            // Auto-approval as the provider's signed-in account: the DID document the
            // discovery path resolved last (`?handle=` binds), or — the no-interstitial
            // default flow, where nothing was resolved — the most recently registered
            // account. Either way the account comes from the PROVIDER's session, and
            // the token's `sub` names it: the connector binds what it is handed.
            let did = oauth
                .last_resolved_did
                .lock()
                .unwrap()
                .clone()
                .or_else(|| bound.accounts.lock().unwrap().last().map(|account| account.did.clone()));
            let record = oauth.pars.lock().unwrap().remove(&request_uri);
            match (record, did) {
                (Some(record), Some(did)) => {
                    let code = oauth.next("code-");
                    oauth.codes.lock().unwrap().insert(code.clone(), (record.clone(), did));
                    // RFC 9207 issuer identification rides the redirect.
                    Script::Moved(format!(
                        "{}?code={}&state={}&iss={}",
                        record.redirect_uri, code, record.state, origin
                    ))
                }
                _ => Script::Body(400, json!({ "error": "invalid_request", "error_description": "unknown or consumed request_uri" })),
            }
        }
        ("POST", "/oauth/token") => {
            // The DPoP nonce dance first: a request without the nonce is challenged once.
            if extras.dpop_nonce != FAKE_DPOP_NONCE {
                return Script::NonceChallenge(
                    401,
                    json!({ "error": "use_dpop_nonce", "error_description": "DPoP nonce missing" }),
                    FAKE_DPOP_NONCE.into(),
                );
            }
            let grant = body.get("grant_type").and_then(Value::as_str).unwrap_or_default();
            match grant {
                "authorization_code" => {
                    let code = body.get("code").and_then(Value::as_str).unwrap_or_default().to_string();
                    let issued = { oauth.codes.lock().unwrap().remove(&code) };
                    let Some((record, did)) = issued else {
                        return Script::Body(400, json!({ "error": "invalid_grant", "error_description": "unknown or used code" }));
                    };
                    if body.get("client_id").and_then(Value::as_str) != Some(scoped_client_id().as_str()) {
                        return Script::Body(400, json!({ "error": "invalid_request", "error_description": "Unsupported client_id" }));
                    }
                    if body.get("redirect_uri").and_then(Value::as_str) != Some(record.redirect_uri.as_str()) {
                        return Script::Body(400, json!({ "error": "invalid_request", "error_description": "redirect_uri mismatch" }));
                    }
                    // PKCE: the verifier must hash to the pushed challenge.
                    let verifier = body.get("code_verifier").and_then(Value::as_str).unwrap_or_default();
                    let computed = b64url(&sha256(verifier.as_bytes()));
                    if verifier.len() < 43 || computed != record.code_challenge {
                        return Script::Body(400, json!({ "error": "invalid_grant", "error_description": "PKCE verification failed" }));
                    }
                    // DPoP: valid proof, signed by the key the PAR bound.
                    let key = match validate_dpop(extras, "POST", &format!("{origin}/oauth/token"), Some(FAKE_DPOP_NONCE), None, oauth) {
                        Ok(key) => key,
                        Err(problem) => return Script::Body(401, json!({ "error": "invalid_dpop_proof", "error_description": problem })),
                    };
                    if key.to_encoded_point(false) != record.dpop.to_encoded_point(false) {
                        return Script::Body(401, json!({ "error": "invalid_dpop_proof", "error_description": "the token key differs from the PAR key" }));
                    }
                    let lifetime = *oauth.initial_lifetime_secs.lock().unwrap();
                    issue_tokens(oauth, &did, lifetime as i64)
                }
                "refresh_token" => {
                    let refresh = body.get("refresh_token").and_then(Value::as_str).unwrap_or_default().to_string();
                    let did = oauth.refresh_tokens.lock().unwrap().remove(&refresh);
                    let Some(did) = did else {
                        return Script::Body(400, json!({ "error": "invalid_grant", "error_description": "unknown or used refresh token" }));
                    };
                    // The refresh must present the client id the grant lives under —
                    // the scoped form for every session this fake issues.
                    if body.get("client_id").and_then(Value::as_str) != Some(scoped_client_id().as_str()) {
                        return Script::Body(400, json!({ "error": "invalid_request", "error_description": "Unsupported client_id" }));
                    }
                    if let Err(problem) = validate_dpop(extras, "POST", &format!("{origin}/oauth/token"), Some(FAKE_DPOP_NONCE), None, oauth) {
                        return Script::Body(401, json!({ "error": "invalid_dpop_proof", "error_description": problem }));
                    }
                    // Refreshed tokens are always long-lived (the knob shapes only the
                    // initial issuance the scenario wants stale).
                    issue_tokens(oauth, &did, 7200)
                }
                _ => Script::Body(400, json!({ "error": "unsupported_grant_type", "error_description": format!("unsupported grant {grant:?}") })),
            }
        }
        // ---- the bound-exchange surface (discovery + PDS + /mcp/token) ----
        ("GET", "/.well-known/tangent-mcp") => match bound.mode {
            BoundMode::AudienceUnconfigured => Script::Body(503, json!({ "error": "exchange_unconfigured" })),
            BoundMode::WrongMethod => Script::Body(200, json!({
                "serviceProof": { "method": "com.atproto.simplespace.checkUserAccess", "audience": AUDIENCE },
            })),
            BoundMode::TamperedAudience => Script::Body(200, json!({
                "serviceProof": { "method": "local.tangent.mcp.exchange", "audience": "did:plc:fixture&lxm=evil&exp=99999999999" },
            })),
            _ => Script::Body(200, json!({
                "protocolVersion": "2026-07-28",
                "authenticationProfile": "atproto_service_proof_exchange",
                "standardMcpOAuthAuthorizationSupport": false,
                "serviceProof": {
                    "method": "local.tangent.mcp.exchange",
                    "audience": AUDIENCE,
                    "algorithms": ["ES256", "ES256K"],
                    "transport": "authorization_header",
                    "endpoint": format!("{origin}/mcp/token"),
                },
                "endpoints": { "mcp": format!("{origin}/mcp"), "token": format!("{origin}/mcp/token") },
            })),
        },
        // The fake PDS sign-in: identifier + app password → PDS session.
        ("POST", "/xrpc/com.atproto.server.createSession") => {
            let identifier = body
                .get("identifier")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .trim_start_matches('@')
                .to_ascii_lowercase();
            let password = body.get("password").and_then(Value::as_str).unwrap_or_default();
            let account = bound
                .accounts
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find(|account| account.handle.to_ascii_lowercase() == identifier)
                .cloned();
            match account {
                Some(account) if account.password == password => {
                    let port = origin.rsplit(':').next().unwrap_or("0");
                    let access = if account.expiring {
                        format!("sat_expired_{port}_{}", account.handle)
                    } else {
                        format!("sat_{port}_{}", account.handle)
                    };
                    Script::Body(200, json!({
                        "did": account.did,
                        "handle": account.handle,
                        "accessJwt": access,
                        "refreshJwt": format!("rt_{port}"),
                        "didDoc": {
                            "id": account.did,
                            "service": [ { "id": "#atproto_pds", "type": "AtprotoPds", "serviceEndpoint": origin } ]
                        }
                    }))
                }
                _ => Script::Body(401, json!({ "error": "AuthenticationRequired", "message": "Invalid identifier or password" })),
            }
        }
        // The fake PDS service-auth mint: session bearer, aud/lxm/exp query discipline.
        // OAuth sessions (JWT-shaped) are DPoP-bound under the DPoP auth scheme: this
        // resource server has its OWN nonce context (RFC 9449 §8) — the first ask
        // without it draws a 401 challenge carrying this PDS's nonce (distinct from
        // the AS's, which never satisfies this check), and the retried proof must
        // embed it. `ath` pins the exact access token; htm/htu of exactly this
        // request (no query). The reference PDS's granular-scope check follows: the
        // token's scope claim must cover (lxm, aud). App-password sessions (sat_-
        // prefixed) carry no key, no proof and no scope check — Bearer as ever.
        ("GET", "/xrpc/com.atproto.server.getServiceAuth") => {
            let scheme = bearer.split(' ').next().unwrap_or_default();
            let token = bearer.strip_prefix("Bearer ").or_else(|| bearer.strip_prefix("DPoP ")).unwrap_or_default();
            // Seeded sessions are sat_-prefixed; a real OAuth bind's are JWT-shaped.
            let is_session = token.starts_with("sat_") || (token.split('.').count() == 3 && token.starts_with("ey"));
            if !is_session {
                return Script::Body(401, json!({ "error": "InvalidToken", "message": "authentication required" }));
            }
            if token.starts_with("sat_expired") {
                return Script::Body(401, json!({ "error": "ExpiredToken", "message": "token has expired" }));
            }
            if token.split('.').count() == 3 && token.starts_with("ey") {
                // A DPoP-bound token under the Bearer scheme is refused the way the
                // reference PDS refuses it — the misleading "Malformed token" 400.
                if scheme != "DPoP" {
                    return Script::Body(400, json!({ "error": "InvalidToken", "message": "Malformed token" }));
                }
                let htu = format!("{origin}/xrpc/com.atproto.server.getServiceAuth");
                let expected_ath = b64url(&sha256(token.as_bytes()));
                match validate_dpop(extras, "GET", &htu, Some(PDS_DPOP_NONCE), Some(&expected_ath), oauth) {
                    Ok(_) => {}
                    // A proof without OUR nonce (none, or a foreign one — the AS's
                    // included) gets exactly one challenge; the retry must embed it.
                    Err(problem) if proof_nonce(extras).as_deref() != Some(PDS_DPOP_NONCE) => {
                        return Script::NonceChallenge(
                            401,
                            json!({ "error": "use_dpop_nonce", "error_description": problem }),
                            PDS_DPOP_NONCE.into(),
                        );
                    }
                    // Our nonce and still refused: retrying cannot settle it.
                    Err(problem) => return Script::Body(401, json!({ "error": "InvalidDpopProof", "message": problem })),
                }
            }
            let query = path.split_once('?').map(|(_, query)| query.to_string()).unwrap_or_default();
            let param = |name: &str| -> Option<String> {
                query.split('&').find_map(|pair| pair.strip_prefix(&format!("{name}=")).map(percent_decode))
            };
            let lxm = param("lxm");
            let aud = param("aud");
            match lxm.as_deref() {
                Some("local.tangent.mcp.exchange") => {}
                other => {
                    return Script::Body(
                        400,
                        json!({ "error": "InvalidRequest", "message": format!("unsupported lxm {other:?}") }),
                    )
                }
            }
            match aud.as_deref() {
                Some(audience) if audience.starts_with("did:") => {}
                _ => return Script::Body(400, json!({ "error": "InvalidRequest", "message": "aud must be a DID" })),
            }
            match param("exp").and_then(|value| value.parse::<i64>().ok()) {
                // The server's accepted window: a near-future expiry around now+120 s.
                Some(exp) if (now_secs()..=now_secs() + 130).contains(&exp) => {}
                _ => return Script::Body(400, json!({ "error": "InvalidRequest", "message": "exp outside the accepted window" })),
            }
            // The granular permission the reference PDS demands of OAuth sessions:
            // the token's scope claim must carry an rpc permission covering this
            // exact (lxm, aud) — the atproto base alone no longer mints proofs.
            if token.split('.').count() == 3 && token.starts_with("ey") {
                let (lxm, aud) = (lxm.unwrap_or_default(), aud.unwrap_or_default());
                if !scope_covers(token, &lxm, &aud) {
                    return Script::Body(
                        403,
                        json!({ "error": "ScopeMissingError", "message": format!("Missing required scope \"rpc:{lxm}?aud={aud}\"") }),
                    );
                }
            }
            let port = origin.rsplit(':').next().unwrap_or("0");
            let mut counter = bound.counter.lock().unwrap();
            *counter += 1;
            let proof = format!("proof_{port}_{counter}");
            bound.proofs.lock().unwrap().insert(proof.clone(), false);
            Script::Body(200, json!({ "token": proof }))
        }
        // The proof exchange: one-time proof bearer → Tangent session.
        ("POST", "/mcp/token") => {
            let provided = bearer.strip_prefix("Bearer ").unwrap_or_default().to_string();
            if !provided.starts_with("proof_") {
                return Script::Body(401, json!({ "error": "invalid_service_proof" }));
            }
            if bound.mode == BoundMode::Suspended {
                return Script::Body(403, json!({ "error": "participant_suspended" }));
            }
            if bound.mode == BoundMode::RejectProofs {
                return Script::Body(401, json!({ "error": "invalid_service_proof" }));
            }
            let mut proofs = bound.proofs.lock().unwrap();
            match proofs.get_mut(&provided) {
                Some(spent) if !*spent => {
                    *spent = true;
                    let port = origin.rsplit(':').next().unwrap_or("0");
                    let suffix = provided.rsplit('_').next().unwrap_or("0");
                    let name = body.get("name").and_then(Value::as_str).unwrap_or("mcp");
                    let grants = body
                        .get("grants")
                        .cloned()
                        .unwrap_or_else(|| json!(["welcome", "read", "post"]));
                    Script::Body(200, json!({
                        "profile": "atproto_service_proof_exchange",
                        "credential": {
                            "participantId": format!("prt_bound_{port}_{suffix}"),
                            "name": name,
                            "grants": grants,
                            "createdAt": "2026-09-11T00:00:00Z",
                            "expiresAt": "2026-09-18T00:00:00Z",
                            "revokedAt": null,
                        },
                        "token": format!("ts_bound_{port}_{suffix}"),
                    }))
                }
                // A consumed or unknown proof: the replay denial.
                _ => Script::Body(401, json!({ "error": "invalid_service_proof" })),
            }
        }
        // ---- the experience API ----
        ("GET", "/api/v1/experience") => Script::Body(200, arrival()),
        ("GET", "/api/v1/experience/tangents") => Script::Body(200, tangents()),
        ("GET", "/api/v1/experience/tangents/home/topics") => Script::Body(200, topics()),
        ("GET", "/api/v1/experience/topics/lounge") if bearer.ends_with(STEWARD_CREDENTIAL) => Script::Body(200, steward_topic_window()),
        ("GET", "/api/v1/experience/topics/lounge") if bearer.ends_with(STALE_STEWARD_CREDENTIAL) => Script::Body(200, steward_topic_window()),
        ("GET", "/api/v1/experience/topics/lounge") if bearer.ends_with(SILENT_STEWARD_CREDENTIAL) => Script::Body(200, steward_topic_window()),
        ("GET", "/api/v1/experience/topics/lounge") if bearer.ends_with(ACTION_ONLY_CREDENTIAL) => Script::Body(200, action_only_topic_window()),
        ("GET", "/api/v1/experience/topics/lounge") => Script::Body(200, topic_window()),
        ("GET", "/api/v1/experience/topics/lounge/moderation/cases")
            if bearer.ends_with(STEWARD_CREDENTIAL) || bearer.ends_with(STALE_STEWARD_CREDENTIAL)
            => Script::Body(200, moderation_cases()),
        ("GET", "/api/v1/experience/moderation/cases/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            if bearer.ends_with(STEWARD_CREDENTIAL) || bearer.ends_with(STALE_STEWARD_CREDENTIAL)
            => Script::Body(200, moderation_case()),
        ("POST", "/api/v1/experience/moderation/cases/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/previews")
            if bearer.ends_with(STEWARD_CREDENTIAL) => Script::Body(200, moderation_preview(body)),
        ("POST", "/api/v1/experience/moderation/cases/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/actions")
            if bearer.ends_with(STEWARD_CREDENTIAL) => Script::Body(200, moderation_apply(body)),
        (_, path) if path.contains("/moderation/") => Script::Body(403,
            json!({ "code": "permission_denied", "message": "current Topic authority is required" })),
        ("GET", "/api/v1/experience/updates") if bearer.ends_with(SILENT_STEWARD_CREDENTIAL)
            => Script::Body(200, updates_without_capabilities()),
        ("GET", "/api/v1/experience/updates") => Script::Body(200, updates()),
        ("POST", "/api/v1/experience/topics/lounge/posts") => {
            let request_id = body.get("requestId").and_then(Value::as_str).unwrap_or_default().to_string();
            let mut registry = posts.lock().unwrap();
            let count = registry.entry(request_id.clone()).or_insert(0);
            *count += 1;
            // The first dispatch of the crash scenario is lost in transit.
            if request_id == "crash-1" && *count == 1 {
                return Script::Lost;
            }
            if request_id == "conflict-1" {
                return Script::Body(409, json!({ "code": "request_conflict", "message": "conflict-1 already identifies a different action." }));
            }
            Script::Body(200, create_post(&request_id))
        }
        ("POST", "/api/v1/experience/topics/lounge/read-position") => Script::Body(200, read_position()),
        ("PUT", "/api/v1/experience/tangents/home/membership") => Script::Body(200, join()),
        ("DELETE", "/api/v1/experience/tangents/home/membership") => Script::Body(200, leave()),
        ("PUT", "/api/v1/experience/watches") => Script::Body(200, watch(body)),
        _ if clean.starts_with("/api/v1/experience/operations/") => {
            Script::Body(200, operation(clean.rsplit('/').next().unwrap_or("")))
        }
        _ => Script::Body(200, json!({
            "experienceVersion": "1.0", "operation": "unknown", "status": "error",
            "result": { "problem": { "code": "invalid_arguments", "message": "unscripted route" } }
        })),
    }
}

/// The W2-contract arrival companion segment: `participantRef` is the server's GUIDv7
/// participant id, `did` is nullable, and the companion collection is ordered best-first.
fn envelope(operation: &str, status: &str, data: Value) -> Value {
    json!({
        "experienceVersion": "1.0",
        "operation": operation,
        "status": status,
        "snapshot": { "revision": "att:41", "asOf": "2026-09-10T20:00:00Z", "coverage": "current" },
        "identity": {
            "participantRef": "prt_7b3e10a2c4d5",
            "did": "did:plc:lumen",
            "displayName": "Lumen",
            "handle": "lumen.example.test"
        },
        "place": { "serverRef": "ORIGIN", "tangentRef": null, "topicRef": null, "label": "Kintsugi Architecture", "role": "participant", "allowedActions": ["list_tangents", "get_updates"] },
        "result": { "data": data, "receipt": null, "problem": null },
        "attention": { "revision": "att:41", "waitingCount": { "value": 0, "atLeast": false }, "newActivityCount": { "value": 0, "atLeast": false }, "items": [], "more": false, "detailsIncluded": true },
        "continuation": { "activityCheckpoint": "checkpoint:41", "activityPageCursor": null, "historyOlderCursor": null, "historyNewerCursor": null, "readCursor": null, "directoryCursor": null },
        "actions": [],
        "orientation": { "purpose": "A shared conversation space for people and agents.", "rules": [], "brief": null },
        "capabilities": { "attention": true, "coordination": false },
    })
}

/// The enrollment endpoint's participant view for one client localId. The connector-client
/// companion echoes the connector's local id, scoped to this server relationship.
fn mention_item() -> Value {
    json!({
        "ref": "att:lounge:m40",
        "kind": "direct_mention",
        "actorRef": "did:plc:leo",
        "actorName": "Leo",
        "recipientRef": "did:plc:lumen",
        "scopeRef": "ORIGIN::home::lounge",
        "sourceRef": "ORIGIN::home::lounge::m40",
        "relationship": "addressed_to_you",
        "excerpt": "Lumen, can you help us coordinate Project Z?",
        "sourceRevision": "cid:40a",
        "state": "pending",
    })
}

fn arrival() -> Value {
    let mut value = envelope("arrive", "ok", json!({
        "tangents": [
            { "tangentRef": "ORIGIN::home", "name": "Kintsugi Architecture", "description": "Main hall", "membership": "member", "admission": "open", "canJoin": false, "hasReadableTopics": true, "hasWritableTopics": true }
        ],
        "continuation": null,
    }));
    value["attention"]["waitingCount"]["value"] = json!(1);
    value["attention"]["items"] = json!([mention_item()]);
    value["actions"] = json!([{ "name": "read_topic", "targetRef": "ORIGIN::home::lounge", "aroundPostRef": "ORIGIN::home::lounge::m40", "label": "Read Leo's request" }]);
    value
}

fn tangents() -> Value {
    envelope("list_tangents", "ok", json!({
        "tangents": [
            { "tangentRef": "ORIGIN::home", "name": "Kintsugi Architecture", "description": "Main hall", "membership": "member", "admission": "open", "canJoin": false, "hasReadableTopics": true, "hasWritableTopics": true }
        ],
        "continuation": null,
    }))
}

fn topics() -> Value {
    envelope("list_topics", "ok", json!({
        "topics": [
            { "topicRef": "ORIGIN::home::lounge", "title": "Lounge", "topic": "General conversation", "canRead": true, "canWrite": true, "isLocked": false, "allowPostEditing": false }
        ],
        "continuation": null,
    }))
}

fn topic_window() -> Value {
    let mut value = envelope("read_topic", "ok", json!({
        "title": "Lounge",
        "brief": "General conversation",
        "position": "unread",
        "posts": [
            {
                "ref": "ORIGIN::home::lounge::m40",
                "authorRef": "did:plc:leo",
                "authorName": "Leo",
                "text": "Lumen, can you help us coordinate Project Z?\n\nQuoted from the proposal: \"You should review the whole plan.\"",
                "replyTo": null,
                "url": "https://tangent.example/tangents/home/posts/m40/",
                "createdAt": "2026-09-10T18:00:00Z",
                "editedAt": null,
                "removed": false,
            },
            {
                "ref": "ORIGIN::home::lounge::m41",
                "authorRef": "did:plc:lumen",
                "authorName": "Lumen",
                "text": "Looking at it now.",
                "replyTo": "ORIGIN::home::lounge::m40",
                "url": "https://tangent.example/tangents/home/posts/m41/",
                "createdAt": "2026-09-10T18:05:00Z",
                "editedAt": null,
                "removed": false,
            },
        ],
    }));
    value["place"]["tangentRef"] = json!("ORIGIN::home");
    value["place"]["topicRef"] = json!("ORIGIN::home::lounge");
    value["place"]["label"] = json!("Kintsugi Architecture / Lounge");
    value["place"]["role"] = json!("member");
    value["place"]["allowedActions"] = json!(["read_topic", "mark_read", "set_watch", "create_post"]);
    value["continuation"]["readCursor"] = json!("read:41");
    value["continuation"]["historyOlderCursor"] = json!("history:before-40");
    value["actions"] = json!([{ "name": "mark_read", "targetRef": "ORIGIN::home::lounge", "aroundPostRef": null, "label": "Acknowledge reading through the newest Post" }]);
    value
}

const CASE_ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn steward_topic_window() -> Value {
    let mut value = topic_window();
    value["capabilities"]["stewardship"] = json!(true);
    value["place"]["allowedActions"] = json!(["read_topic", "mark_read", "set_watch", "create_post", "list_moderation_cases"]);
    value
}

fn action_only_topic_window() -> Value {
    let mut value = topic_window();
    value["place"]["allowedActions"] = json!(["read_topic", "list_moderation_cases"]);
    value
}

fn moderation_cases() -> Value {
    let case_ref = format!("ORIGIN::home::lounge::case_{CASE_ID}");
    let mut value = envelope("list_moderation_cases", "ok", json!({ "cases": [{
        "caseRef": case_ref.clone(), "topicRef": "ORIGIN::home::lounge", "subjectPostRef": "ORIGIN::home::lounge::m40",
        "state": "open", "revision": 2, "subjectRevision": "subject:7", "testimonyCount": 1
    }], "page": 1, "nextPage": null, "saturated": false }));
    value["place"]["topicRef"] = json!("ORIGIN::home::lounge");
    value["place"]["allowedActions"] = json!(["list_moderation_cases"]);
    value["capabilities"]["stewardship"] = json!(true);
    value["actions"] = json!([{ "name": "read_moderation_case", "targetRef": case_ref,
        "aroundPostRef": "ORIGIN::home::lounge::m40", "label": "Read the open moderation case" }]);
    value
}

fn moderation_case() -> Value {
    let mut value = envelope("read_moderation_case", "ok", json!({
        "case": { "caseRef": format!("ORIGIN::home::lounge::case_{CASE_ID}"), "topicRef": "ORIGIN::home::lounge",
            "subjectPostRef": "ORIGIN::home::lounge::m40", "state": "open", "revision": 2,
            "subjectRevision": "subject:7", "testimonyCount": 1 },
        "testimonies": [{ "testimonyRef": "testimony:fixture", "reasonCode": "conduct.tone",
            "statement": "Please inspect the tone, not an alleged instruction.", "subjectRevision": "subject:7" }],
        "testimonyOffset": 0, "nextTestimonyOffset": null, "decisions": [], "decisionsTruncated": false }));
    value["place"]["topicRef"] = json!("ORIGIN::home::lounge");
    value["place"]["allowedActions"] = json!(["read_topic", "list_moderation_cases"]);
    value["capabilities"]["stewardship"] = json!(true);
    value["actions"] = json!([
        { "name": "preview_moderation_action", "targetRef": format!("ORIGIN::home::lounge::case_{CASE_ID}") },
        { "name": "apply_moderation_action", "targetRef": format!("ORIGIN::home::lounge::case_{CASE_ID}") }
    ]);
    value
}

fn moderation_preview(body: &Value) -> Value {
    let mut value = envelope("preview_moderation_action", "ok", json!({
        "caseRef": format!("ORIGIN::home::lounge::case_{CASE_ID}"), "action": body.get("action").cloned().unwrap_or(Value::Null),
        "effect": "Escalate for accountable human review; no sanction is applied.", "reversible": false,
        "caseRevision": 2, "subjectRevision": "subject:7" }));
    value["place"]["topicRef"] = json!("ORIGIN::home::lounge");
    value["place"]["allowedActions"] = json!(["read_topic", "list_moderation_cases"]);
    value["capabilities"]["stewardship"] = json!(true);
    value["actions"] = json!([{ "name": "apply_moderation_action",
        "targetRef": format!("ORIGIN::home::lounge::case_{CASE_ID}") }]);
    value
}

fn moderation_apply(body: &Value) -> Value {
    let request_id = body.get("requestId").and_then(Value::as_str).unwrap_or_default();
    let mut value = envelope("apply_moderation_action", "ok", json!({
        "caseRef": format!("ORIGIN::home::lounge::case_{CASE_ID}"), "state": "escalated",
        "caseRevision": 3, "subjectRevision": "subject:7" }))
        .with_receipt(request_id, "completed", &format!("ORIGIN::home::lounge::case_{CASE_ID}"));
    value["place"]["topicRef"] = json!("ORIGIN::home::lounge");
    value["place"]["allowedActions"] = json!(["read_topic", "list_moderation_cases"]);
    value["capabilities"]["stewardship"] = json!(true);
    value
}

fn updates() -> Value {
    let mut value = envelope("get_updates", "ok", json!({ "scopeRef": "ORIGIN" }));
    value["attention"]["waitingCount"]["value"] = json!(1);
    value["attention"]["newActivityCount"]["value"] = json!(2);
    value["attention"]["items"] = json!([mention_item()]);
    value["actions"] = json!([{ "name": "read_topic", "targetRef": "ORIGIN::home::lounge", "aroundPostRef": "ORIGIN::home::lounge::m40", "label": "Read Leo's request" }]);
    value
}

fn updates_without_capabilities() -> Value {
    let mut value = updates();
    value.as_object_mut().unwrap().remove("capabilities");
    value
}

fn create_post(request_id: &str) -> Value {
    envelope("create_post", "ok", json!({
        "postRef": "ORIGIN::home::lounge::m84",
        "source": { "uri": "at://did:plc:lumen/example.test.space/op-84/post-84", "cid": "bafyreiexample84" },
        "url": "https://tangent.example/tangents/home/posts/m84/",
    }))
    .with_receipt(request_id, "completed", "ORIGIN::home::lounge::m84")
}

fn read_position() -> Value {
    envelope("read_position", "ok", json!({
        "throughPostRef": "ORIGIN::home::lounge::m41",
        "topicRef": "ORIGIN::home::lounge",
    }))
}

fn join() -> Value {
    envelope("join", "ok", json!({ "membership": "member", "message": "Welcome to your Tangent." }))
}

fn leave() -> Value {
    envelope("leave", "ok", json!({ "membership": "visitor", "message": "You left this Tangent." }))
}

fn watch(body: &Value) -> Value {
    envelope("set_watch", "ok", json!({
        "scopeRef": body.get("scopeRef").cloned().unwrap_or(Value::Null),
        "mode": body.get("mode").cloned().unwrap_or(Value::Null),
    }))
}

fn operation(request_id: &str) -> Value {
    envelope("get_operation", "ok", json!({ "operation": "CreatePost" }))
        .with_receipt(request_id, "completed", "ORIGIN::home::lounge::m84")
}

trait WithReceipt {
    fn with_receipt(self, request_id: &str, state: &str, result_ref: &str) -> Value;
}

impl WithReceipt for Value {
    fn with_receipt(mut self, request_id: &str, state: &str, result_ref: &str) -> Value {
        self["result"]["receipt"] = json!({ "requestId": request_id, "state": state, "resultRef": result_ref, "retryAfterSeconds": null });
        self
    }
}

/// Collects published events for assertions.
#[allow(dead_code)]
pub struct EventRecorder(pub Receiver<companion_core::domain::events::DomainEvent>);

impl EventRecorder {
    #[allow(dead_code)]
    pub fn drain(&self) -> Vec<companion_core::domain::events::DomainEvent> {
        let mut observed = Vec::new();
        while let Ok(event) = self.0.try_recv() {
            observed.push(event);
        }
        observed
    }
}

// ---------- the fake authorization server's helpers ----------

/// One decoded query parameter of a request path.
fn query_param(path: &str, name: &str) -> Option<String> {
    let query = path.split_once('?')?.1;
    query.split('&').find_map(|pair| {
        pair.strip_prefix(&format!("{name}=")).map(percent_decode)
    })
}

/// The scope every OAuth token this fake AS issues carries — the grant the connector
/// binds under, echoed in the token response and the access token's own claim.
const GRANTED_SCOPE: &str = "atproto rpc:local.tangent.mcp.exchange?aud=*";

/// Issues one access+refresh pair for a DID: the access token is JWT-shaped (the
/// connector parses `exp` from it, the fake PDS reads its `scope` claim), the refresh
/// token rotates and is remembered.
fn issue_tokens(oauth: &OauthScript, did: &str, lifetime_secs: i64) -> Script {
    let now = now_secs();
    let access = format!(
        "{}.{}.{}",
        b64url(br#"{"alg":"ES256","typ":"atproto+jwt"}"#),
        b64url(
            format!(
                "{{\"exp\":{},\"scope\":\"{GRANTED_SCOPE}\",\"sub\":\"{did}\"}}",
                now + lifetime_secs
            )
            .as_bytes()
        ),
        b64url(&[7u8; 64])
    );
    let refresh = oauth.next("rt_oauth");
    oauth.refresh_tokens.lock().unwrap().insert(refresh.clone(), did.to_string());
    Script::Body(
        200,
        json!({
            "access_token": access,
            "refresh_token": refresh,
            "token_type": "DPoP",
            "expires_in": lifetime_secs,
            "scope": GRANTED_SCOPE,
            "sub": did,
        }),
    )
}

/// The `nonce` claim inside a request's DPoP proof (None when the proof carries no
/// nonce) — the challenge decision: only a proof already embedding OUR nonce has
/// exhausted the challenge path.
fn proof_nonce(extras: &RequestExtras) -> Option<String> {
    let payload = extras.dpop.split('.').nth(1)?;
    let claims: Value = serde_json::from_slice(&b64url_decode(payload)?).ok()?;
    claims.get("nonce").and_then(Value::as_str).map(str::to_string)
}

/// Whether an OAuth access token's `scope` claim carries an rpc permission covering
/// one exact (lxm, aud) request — the reference PDS's granular check. Either axis may
/// be wildcarded in the granted permission, never both.
fn scope_covers(token: &str, lxm: &str, aud: &str) -> bool {
    let Some(payload) = token.split('.').nth(1).and_then(b64url_decode) else { return false };
    let Ok(claims) = serde_json::from_slice::<Value>(&payload) else { return false };
    let Some(scope) = claims.get("scope").and_then(Value::as_str) else { return false };
    scope.split(' ').any(|value| {
        let Some(rest) = value.strip_prefix("rpc:") else { return false };
        let (granted_lxm, granted_aud) = rest.split_once("?aud=").unwrap_or((rest, "*"));
        (granted_lxm == "*" || granted_lxm == lxm) && (granted_aud == "*" || granted_aud == aud)
    })
}

/// Validates one DPoP proof the way the real servers would: ES256
/// signature over the compact signing input by the header's own JWK, the profile
/// claims (typ/alg, htm/htu, iat window, exp), the nonce when one is expected, `ath`
/// pinning the access token on resource requests, and a jti replay guard. Answers
/// the verified public key.
fn validate_dpop(
    extras: &RequestExtras,
    htm: &str,
    htu: &str,
    expected_nonce: Option<&str>,
    expected_ath: Option<&str>,
    oauth: &OauthScript,
) -> Result<p256::ecdsa::VerifyingKey, String> {
    use p256::ecdsa::signature::Verifier;

    let proof = if extras.dpop.is_empty() { return Err("no DPoP header".into()) } else { extras.dpop.clone() };
    let segments: Vec<&str> = proof.split('.').collect();
    if segments.len() != 3 {
        return Err("not a compact JWS".into());
    }
    let header: Value = serde_json::from_slice(&b64url_decode(segments[0]).ok_or("bad header b64")?).map_err(|_| "header is not JSON")?;
    let payload: Value = serde_json::from_slice(&b64url_decode(segments[1]).ok_or("bad payload b64")?).map_err(|_| "payload is not JSON")?;
    let signature = b64url_decode(segments[2]).ok_or("bad signature b64")?;
    if signature.len() != 64 {
        return Err("the ES256 signature is not 64 raw bytes".into());
    }
    if header.get("typ").and_then(Value::as_str) != Some("dpop+jwt") || header.get("alg").and_then(Value::as_str) != Some("ES256") {
        return Err("wrong typ/alg".into());
    }
    let jwk = header.get("jwk").ok_or("no jwk in the header")?;
    if jwk.get("kty").and_then(Value::as_str) != Some("EC") || jwk.get("crv").and_then(Value::as_str) != Some("P-256") {
        return Err("wrong jwk curve".into());
    }
    let x = b64url_decode(jwk.get("x").and_then(Value::as_str).ok_or("no x")?).ok_or("bad x")?;
    let y = b64url_decode(jwk.get("y").and_then(Value::as_str).ok_or("no y")?).ok_or("bad y")?;
    if x.len() != 32 || y.len() != 32 {
        return Err("wrong coordinate size".into());
    }
    let mut sec1 = Vec::with_capacity(65);
    sec1.push(0x04);
    sec1.extend_from_slice(&x);
    sec1.extend_from_slice(&y);
    let public = p256::PublicKey::from_sec1_bytes(&sec1).map_err(|_| "the jwk is not a curve point")?;
    let verifying = p256::ecdsa::VerifyingKey::from(public);
    let signature = p256::ecdsa::Signature::from_slice(&signature).map_err(|_| "bad signature")?;
    verifying
        .verify(format!("{}.{}", segments[0], segments[1]).as_bytes(), &signature)
        .map_err(|_| "the signature does not verify".to_string())?;
    if payload.get("htm").and_then(Value::as_str) != Some(htm) {
        return Err("wrong htm".into());
    }
    if payload.get("htu").and_then(Value::as_str) != Some(htu) {
        return Err("wrong htu".into());
    }
    let now = now_secs();
    let iat = payload.get("iat").and_then(Value::as_i64).ok_or("no iat")?;
    if !(now - 60..=now + 300).contains(&iat) {
        return Err("iat outside the accepted window".into());
    }
    if payload.get("exp").and_then(Value::as_i64).is_some_and(|exp| exp < now) {
        return Err("the proof expired".into());
    }
    let jti = payload.get("jti").and_then(Value::as_str).unwrap_or_default().to_string();
    if jti.is_empty() {
        return Err("no jti".into());
    }
    if !oauth.jtis.lock().unwrap().insert(jti) {
        return Err("jti replayed".into());
    }
    if let Some(expected) = expected_nonce {
        if payload.get("nonce").and_then(Value::as_str) != Some(expected) {
            return Err("wrong nonce".into());
        }
    }
    if let Some(expected) = expected_ath {
        if payload.get("ath").and_then(Value::as_str) != Some(expected) {
            return Err("wrong ath (the proof does not bind this access token)".into());
        }
    }
    Ok(verifying)
}

/// Strict base64url decode, shared with journey assertions that read proof payloads.
#[allow(dead_code)]
pub fn decode_b64url(text: &str) -> Option<Vec<u8>> {
    b64url_decode(text)
}

/// Strict base64url decode (unpadded alphabet only).
fn b64url_decode(text: &str) -> Option<Vec<u8>> {
    let table: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut buffer: u32 = 0;
    let mut bits = 0u32;
    for byte in text.bytes() {
        let value = table.iter().position(|candidate| *candidate == byte)? as u32;
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    if bits >= 6 {
        return None;
    }
    Some(out)
}

/// Standard base64url (unpadded).
fn b64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(triple >> 18) as usize & 63] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[(triple >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[triple as usize & 63] as char);
        }
    }
    out
}

fn sha256(data: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs() as i64)
        .unwrap_or(0)
}

/// Minimal percent-decoding for query parameters the fake PDS reads back.
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
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

/// Seeds the state the OAuth bind leaves behind, without driving the flow: an companion
/// whose atproto account is bound. Tests that are not *about* binding start here, so
/// only [`bind_oauth_journey`] exercises the handshake itself.
#[allow(dead_code)]
pub fn seed_bound_companion(hub: &ConnectorHub, handle: &str, did: &str, pds: &str) -> String {
    let companion = hub
        .create_companion(handle.split('.').next().unwrap_or("user"), None)
        .expect("companion");
    seed_atproto_session(hub, &companion.local_id, handle, did, pds);
    companion.local_id
}

/// Seeds (or replaces) one companion's bound atproto session — the write tail of the
/// bind, as the OAuth flow would have left it.
#[allow(dead_code)]
pub fn seed_atproto_session(hub: &ConnectorHub, local_id: &str, handle: &str, did: &str, pds: &str) {
    let mut store = hub.store().lock().expect("state lock");
    let mut companion = store.companion(local_id).expect("companion exists");
    companion.bound_did = Some(did.to_string());
    store.upsert_companion(companion).expect("bind the companion");
    store.set_atproto_session(
        local_id,
        AccountSession {
            did: did.to_string(),
            handle: handle.trim_start_matches('@').to_string(),
            access_jwt: format!("sat_seeded_{}", did.replace(':', "_")),
            refresh_jwt: Some(format!("pds_refresh_{did}")),
            pds: pds.to_string(),
            authserver: Some(pds.to_string()),
            client_id: None,
            dpop_key: None,
            services: vec!["tangent".to_string()],
            obtained_at: 1_757_000_000_000,
        },
    );
    store.save().expect("save seeded binding");
}

/// Seeds a state directory with one enrolled companion and its Tangent session — the
/// state a bound enrollment leaves behind — for tests that then spawn the binary over
/// that directory instead of driving the handshake in process.
#[allow(dead_code)]
pub fn seed_enrolled_state(home: &std::path::Path, handle: &str, origin: &str, token: &str) -> String {
    let events = Arc::new(companion_lobby::application::bus::EventBus::new());
    let store = companion_lobby::adapters::store::StateStore::open(home).expect("state store");
    let hub = ConnectorHub::new(store, events, CallerId("cli".into()))
        .with_service(Arc::new(adapter_service_tangent::experience::UreqExperience::new()))
        .with_auth(Arc::new(adapter_auth_atproto::atproto_oauth::AtprotoOauth::new()));
    let companion = hub.create_companion(handle, None).expect("companion");
    let enrollment_id = format!("cmp_seeded_{handle}");
    {
        let mut store = hub.store().lock().expect("state lock");
        store.upsert_enrollment(Enrollment {
            enrollment_id: enrollment_id.clone(),
            local_id: companion.local_id.clone(),
            name: handle.to_string(),
            origin: origin.to_string(),
            participant_ref: format!("prt_seeded_{handle}"),
            did: Some(format!("did:plc:{handle}")),
            display_name: None,
            handle: Some(format!("{handle}.bsky.example")),
            enrolled_at: 1_757_000_000_000,
            auto_check: true,
        });
        store.set_session(&enrollment_id, token);
        store.save().expect("save seeded state");
    }
    enrollment_id
}
