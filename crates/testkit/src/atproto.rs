//! A fake atproto world: an authorization server and a PDS.
//!
//! The authorization server stands on one loopback listener and also answers
//! as the DID directory (`GET /did:plc:...`), the handle resolver
//! (`com.atproto.identity.resolveHandle`) and the host of the latest did:web
//! account's document (`/.well-known/did.json`), so the credential adapter
//! can be pointed at one origin for everything it would resolve publicly.
//! The PDS stands on a listener of its own and publishes its
//! protected-resource metadata, naming the authorization server.
//!
//! The authorization server keeps to the atproto OAuth profile the way the
//! real one does, and refuses what the real one refuses:
//!
//! * the pushed authorization request must come from the loopback client id
//!   `http://localhost?...`, whose own query declares the scopes it may ask
//!   for and the redirect URIs it may name; the requested scope must lie
//!   inside the declared one and include `atproto`, the redirect URI must be
//!   one the client id declares, on the loopback address, and neither a
//!   `nonce` nor a `login_hint` may be sent;
//! * PKCE is S256 only, the code is one use, and the verifier must hash to
//!   the pushed challenge;
//! * every request to the PAR and token endpoints carries a DPoP proof:
//!   ES256 over the header's own JWK, `typ` `dpop+jwt`, `htm` and `htu` of
//!   the request, `iat` within the window, a `jti` this server never saw
//!   (one guard per server) and the server's current nonce, which rotates on
//!   every answer; a proof without the current or the previous nonce is
//!   answered `use_dpop_nonce` with a fresh one, and the token request must
//!   be signed by the key the PAR was signed by;
//! * the authorize endpoint approves as whichever account the test scripted
//!   with [`AtprotoWorld::sign_in_as`], whatever the request names, and sends
//!   the browser back with `code`, `state` and `iss`; with no account
//!   scripted it sends back `error=access_denied`;
//! * a refresh (`grant_type=refresh_token`) must present the grant's
//!   current refresh token, the client id it was granted under and a proof
//!   made with the key its PAR was signed by; it rotates both tokens, so a
//!   refresh token works once, and one presented again is `invalid_grant`;
//! * the revocation endpoint ends the grant a refresh or access token
//!   belongs to: after it, the grant's refresh token refreshes nothing.
//!
//! Tokens are DPoP-bound and live [`AtprotoWorld::access_lifetime`] seconds,
//! an hour unless a test says otherwise. An access token is JWT-shaped unless
//! [`AtprotoWorld::opaque_tokens`] says otherwise; the adapter must not care.
//! Knobs break the metadata the adapter checks, flaw the token answer (its
//! type, subject, refresh token or scope), refuse or slow refreshes, widen
//! what a refresh grants, let a handle resolve to an account whose document
//! does not claim it, and make the PDS name another authorization server.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use p256::ecdsa::signature::Verifier as _;
use p256::ecdsa::{Signature, VerifyingKey};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::listener::{Listener, Received, Reply};
use crate::{percent_decode, percent_encode, unique_token};

/// The loopback client id's origin, before its query.
pub const CLIENT_ORIGIN: &str = "http://localhost";

/// A metadata promise the authorization server can be made to break.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flaw {
    /// No pushed authorization request endpoint, and PAR not required.
    NoPushedRequests,
    /// PKCE without S256.
    NoS256,
    /// No DPoP with ES256.
    NoDpop,
    /// No `atproto` scope.
    NoAtprotoScope,
    /// The metadata names another issuer.
    AnotherIssuer,
    /// The metadata document redirects elsewhere instead of answering.
    Redirects,
    /// The metadata names no revocation endpoint.
    NoRevocation,
}

/// An account the world knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// Its handle.
    pub handle: String,
    /// Its DID.
    pub did: String,
}

#[derive(Debug, Clone)]
struct Pushed {
    client_id: String,
    redirect_uri: String,
    scope: String,
    state: String,
    challenge: String,
    key: VerifyingKey,
}

/// One grant the authorization server issued: the account, what it was
/// granted under, and its current tokens.
#[derive(Debug, Clone)]
struct Grant {
    did: String,
    scope: String,
    client_id: String,
    key: VerifyingKey,
    refresh: String,
    access: String,
    ended: bool,
}

#[derive(Debug, Default)]
struct Book {
    accounts: Vec<Account>,
    /// Handles the resolver answers for, which no document claims.
    aliases: Vec<(String, String)>,
    approving: Option<String>,
    subject: Option<String>,
    token_type: Option<String>,
    without_refresh: bool,
    granted_scope: Option<String>,
    access_lifetime: Option<u64>,
    refusing_refreshes: bool,
    limiting_refreshes: bool,
    widened_refresh: Option<String>,
    grants: Vec<Grant>,
    revocations: Vec<String>,
    flaws: Vec<Flaw>,
    pds_names: Option<String>,
    opaque: bool,
    resolver_down: bool,
    pushed: HashMap<String, Pushed>,
    codes: HashMap<String, (Pushed, String)>,
    jtis: HashSet<String>,
    nonces: VecDeque<String>,
    issued: Vec<String>,
}

impl Book {
    /// Rotates the nonce: the fresh one is current, the one before it still
    /// accepted.
    fn rotate(&mut self) -> String {
        let nonce = unique_token("nonce");
        self.nonces.push_front(nonce.clone());
        self.nonces.truncate(2);
        nonce
    }

    fn account(&self, did: &str) -> Option<&Account> {
        self.accounts
            .iter()
            .rev()
            .find(|account| account.did == did)
    }
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A fake authorization server and PDS on their own loopback listeners.
pub struct AtprotoWorld {
    server: Listener,
    pds: Listener,
    book: Arc<Mutex<Book>>,
}

impl Default for AtprotoWorld {
    fn default() -> Self {
        Self::start()
    }
}

impl AtprotoWorld {
    /// Starts a world with no accounts.
    pub fn start() -> Self {
        let book = Arc::new(Mutex::new(Book::default()));
        let origins = Arc::new(Mutex::new((String::new(), String::new())));
        let (serving, server_origins) = (book.clone(), origins.clone());
        let server = Listener::start(move |request| {
            let (server, pds) = locked(&server_origins).clone();
            answer_server(&serving, &server, &pds, request)
        });
        let (resourcing, pds_origins) = (book.clone(), origins.clone());
        let pds = Listener::start(move |request| {
            let (server, pds) = locked(&pds_origins).clone();
            answer_pds(&resourcing, &server, &pds, request)
        });
        *locked(&origins) = (server.origin(), pds.origin());
        Self { server, pds, book }
    }

    /// The authorization server's origin, which also answers as the DID
    /// directory and the handle resolver.
    pub fn origin(&self) -> String {
        self.server.origin()
    }

    /// The PDS's origin.
    pub fn pds_origin(&self) -> String {
        self.pds.origin()
    }

    /// The authorization server's listener.
    pub fn server(&self) -> &Listener {
        &self.server
    }

    /// The PDS's listener.
    pub fn pds(&self) -> &Listener {
        &self.pds
    }

    /// Adds an account whose PDS is this world's.
    pub fn add_account(&self, handle: &str, did: &str) {
        locked(&self.book).accounts.push(Account {
            handle: handle.to_owned(),
            did: did.to_owned(),
        });
    }

    /// Approves every sign-in from now on as the account `did`.
    pub fn sign_in_as(&self, did: &str) {
        locked(&self.book).approving = Some(did.to_owned());
    }

    /// Makes the token response name `did` as its subject, whoever signed in.
    pub fn token_names(&self, did: &str) {
        locked(&self.book).subject = Some(did.to_owned());
    }

    /// Makes the token response say `kind` as its token type.
    pub fn token_type_is(&self, kind: &str) {
        locked(&self.book).token_type = Some(kind.to_owned());
    }

    /// Makes the code exchange answer without a refresh token.
    pub fn without_refresh_token(&self) {
        locked(&self.book).without_refresh = true;
    }

    /// Makes the code exchange say it granted `scope`, whatever was asked.
    pub fn grants_scope(&self, scope: &str) {
        locked(&self.book).granted_scope = Some(scope.to_owned());
    }

    /// Makes the resolver answer `did` for `handle`, a handle the account's
    /// document does not claim.
    pub fn alias_handle(&self, handle: &str, did: &str) {
        locked(&self.book)
            .aliases
            .push((handle.to_owned(), did.to_owned()));
    }

    /// Issues tokens that live `secs` seconds from now on.
    pub fn access_lifetime(&self, secs: u64) {
        locked(&self.book).access_lifetime = Some(secs);
    }

    /// Refuses every refresh from now on, as a grant the provider ended.
    pub fn refuse_refreshes(&self) {
        locked(&self.book).refusing_refreshes = true;
    }

    /// Answers every refresh with 429 while `limiting`, rotating nothing.
    pub fn rate_limit_refreshes(&self, limiting: bool) {
        locked(&self.book).limiting_refreshes = limiting;
    }

    /// Makes every refresh from now on say it granted `scope`.
    pub fn widen_refreshes(&self, scope: &str) {
        locked(&self.book).widened_refresh = Some(scope.to_owned());
    }

    /// Every refresh token presented to the token endpoint, oldest first,
    /// whether or not it refreshed anything.
    pub fn refresh_tokens_presented(&self) -> Vec<String> {
        self.server
            .requests()
            .iter()
            .filter(|request| {
                request.route() == "/oauth/token"
                    && request.field("grant_type").as_deref() == Some("refresh_token")
            })
            .filter_map(|request| request.field("refresh_token"))
            .collect()
    }

    /// Every token the revocation endpoint was asked to revoke, oldest
    /// first.
    pub fn revocations(&self) -> Vec<String> {
        locked(&self.book).revocations.clone()
    }

    /// The current refresh token of the grant made to `did` last, while it
    /// is live.
    pub fn live_refresh_token(&self, did: &str) -> Option<String> {
        locked(&self.book)
            .grants
            .iter()
            .rev()
            .find(|grant| grant.did == did && !grant.ended)
            .map(|grant| grant.refresh.clone())
    }

    /// Breaks one metadata promise.
    pub fn break_metadata(&self, flaw: Flaw) {
        locked(&self.book).flaws.push(flaw);
    }

    /// Makes the PDS name `origin` as its authorization server.
    pub fn pds_names(&self, origin: &str) {
        locked(&self.book).pds_names = Some(origin.to_owned());
    }

    /// Issues access tokens that are not JWT-shaped.
    pub fn opaque_tokens(&self) {
        locked(&self.book).opaque = true;
    }

    /// Makes the handle resolver answer 503 from now on, as one that cannot
    /// be reached for a while.
    pub fn resolver_unavailable(&self) {
        locked(&self.book).resolver_down = true;
    }

    /// Every token issued so far, for tests that look for secrets where none
    /// may be.
    pub fn issued(&self) -> Vec<String> {
        locked(&self.book).issued.clone()
    }

    /// Follows the sponsor's browser to the authorize `url` and returns where
    /// the authorization server sends it back.
    pub fn approve(&self, url: &str) -> String {
        let target = url
            .strip_prefix(&self.origin())
            .unwrap_or_else(|| panic!("{url} is not on this authorization server"));
        let address = lobby_contracts::Address::parse(
            &self.origin(),
            lobby_contracts::AddressPolicy::LoopbackAllowed,
        )
        .expect("the world's origin");
        let answer = lobby_outbound::Outbound::new(lobby_contracts::AddressPolicy::LoopbackAllowed)
            .send(&address, lobby_outbound::Request::new("GET", target))
            .expect("the authorization server answers");
        assert_eq!(
            answer.status, 302,
            "the authorize endpoint sends the browser back"
        );
        answer.header("location").expect("a location").to_owned()
    }
}

fn error(status: u16, code: &str) -> Reply {
    Reply::json(status, &json!({ "error": code, "error_description": code }))
}

fn answer_server(book: &Mutex<Book>, origin: &str, pds: &str, request: &Received) -> Reply {
    let mut book = locked(book);
    let route = request.route().to_owned();
    match (request.method.as_str(), route.as_str()) {
        ("GET", "/xrpc/com.atproto.identity.resolveHandle") if book.resolver_down => {
            error(503, "TemporarilyUnavailable")
        }
        ("GET", "/xrpc/com.atproto.identity.resolveHandle") => {
            let handle = request.query("handle").unwrap_or_default();
            let named = book
                .accounts
                .iter()
                .rev()
                .find(|account| account.handle == handle)
                .map(|account| account.did.clone())
                .or_else(|| {
                    book.aliases
                        .iter()
                        .find(|(alias, _)| *alias == handle)
                        .map(|(_, did)| did.clone())
                });
            match named {
                Some(did) => Reply::json(200, &json!({ "did": did })),
                None => error(400, "InvalidRequest"),
            }
        }
        ("GET", "/.well-known/did.json") => {
            let account = book
                .accounts
                .iter()
                .rev()
                .find(|account| account.did.starts_with("did:web:"))
                .cloned();
            match account {
                Some(account) => Reply::json(200, &document(&account, pds)),
                None => error(404, "NotFound"),
            }
        }
        ("GET", did) if did.starts_with("/did:plc:") => {
            match book.account(&percent_decode(&did[1..])).cloned() {
                Some(account) => Reply::json(200, &document(&account, pds)),
                None => error(404, "NotFound"),
            }
        }
        ("GET", "/.well-known/oauth-authorization-server") => {
            if book.flaws.contains(&Flaw::Redirects) {
                return Reply::redirect(&format!("{pds}/.well-known/oauth-authorization-server"));
            }
            Reply::json(200, &metadata(&book.flaws, origin))
        }
        ("POST", "/oauth/par") => pushed(&mut book, origin, request),
        ("GET", "/oauth/authorize") => authorize(&mut book, origin, request),
        ("POST", "/oauth/token") => token(&mut book, origin, request),
        ("POST", "/oauth/revoke") if !book.flaws.contains(&Flaw::NoRevocation) => {
            revoke(&mut book, request)
        }
        _ => error(404, "NotFound"),
    }
}

fn answer_pds(book: &Mutex<Book>, server: &str, pds: &str, request: &Received) -> Reply {
    let book = locked(book);
    match (request.method.as_str(), request.route()) {
        ("GET", "/.well-known/oauth-protected-resource") => {
            let names = book.pds_names.clone().unwrap_or_else(|| server.to_owned());
            Reply::json(
                200,
                &json!({ "resource": pds, "authorization_servers": [names] }),
            )
        }
        _ => error(404, "NotFound"),
    }
}

fn document(account: &Account, pds: &str) -> Value {
    json!({
        "id": account.did,
        "alsoKnownAs": [format!("at://{}", account.handle)],
        "verificationMethod": [],
        "service": [{ "id": "#atproto_pds", "type": "AtprotoPersonalDataServer", "serviceEndpoint": pds }],
    })
}

fn metadata(flaws: &[Flaw], origin: &str) -> Value {
    let mut metadata = json!({
        "issuer": origin,
        "authorization_endpoint": format!("{origin}/oauth/authorize"),
        "token_endpoint": format!("{origin}/oauth/token"),
        "pushed_authorization_request_endpoint": format!("{origin}/oauth/par"),
        "revocation_endpoint": format!("{origin}/oauth/revoke"),
        "require_pushed_authorization_requests": true,
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none", "private_key_jwt"],
        "token_endpoint_auth_signing_alg_values_supported": ["ES256"],
        "dpop_signing_alg_values_supported": ["ES256"],
        "scopes_supported": ["atproto", "transition:generic"],
        "authorization_response_iss_parameter_supported": true,
        "client_id_metadata_document_supported": true,
    });
    for flaw in flaws {
        match flaw {
            Flaw::NoPushedRequests => {
                metadata["require_pushed_authorization_requests"] = json!(false);
                if let Some(fields) = metadata.as_object_mut() {
                    fields.remove("pushed_authorization_request_endpoint");
                }
            }
            Flaw::NoS256 => metadata["code_challenge_methods_supported"] = json!(["plain"]),
            Flaw::NoDpop => metadata["dpop_signing_alg_values_supported"] = json!([]),
            Flaw::NoAtprotoScope => metadata["scopes_supported"] = json!(["openid"]),
            Flaw::AnotherIssuer => metadata["issuer"] = json!("https://issuer.test"),
            Flaw::NoRevocation => {
                if let Some(fields) = metadata.as_object_mut() {
                    fields.remove("revocation_endpoint");
                }
            }
            Flaw::Redirects => {}
        }
    }
    metadata
}

/// What a loopback client id declares in its own query: its scopes and its
/// redirect URIs.
fn declared(client_id: &str) -> Option<(Vec<String>, Vec<String>)> {
    let query = client_id.strip_prefix(CLIENT_ORIGIN)?.strip_prefix('?')?;
    let mut scopes = Vec::new();
    let mut redirects = Vec::new();
    for pair in query.split('&') {
        let (name, value) = pair.split_once('=')?;
        match name {
            "scope" => scopes.extend(percent_decode(value).split(' ').map(str::to_owned)),
            "redirect_uri" => redirects.push(percent_decode(value)),
            _ => return None,
        }
    }
    Some((scopes, redirects))
}

/// Checks the request's DPoP proof against this server's nonces and jti
/// guard, and answers with the proof's key; a missing or stale nonce is
/// answered with a fresh one.
fn proven(book: &mut Book, origin: &str, request: &Received) -> Result<VerifyingKey, Reply> {
    let url = format!("{origin}{}", request.route());
    let proof = request.header("dpop").unwrap_or_default().to_owned();
    let claims = check_proof(&proof, &request.method, &url, &mut book.jtis).map_err(|problem| {
        let nonce = book.rotate();
        error(400, problem).with_header("DPoP-Nonce", &nonce)
    })?;
    let nonce = claims.1["nonce"].as_str().unwrap_or_default();
    if !book.nonces.iter().any(|known| known == nonce) {
        let fresh = book.rotate();
        return Err(error(400, "use_dpop_nonce").with_header("DPoP-Nonce", &fresh));
    }
    Ok(claims.0)
}

fn pushed(book: &mut Book, origin: &str, request: &Received) -> Reply {
    let key = match proven(book, origin, request) {
        Ok(key) => key,
        Err(reply) => return reply,
    };
    let field = |name: &str| request.field(name);
    let nonce = book.rotate();
    let refuse = |code: &str| error(400, code).with_header("DPoP-Nonce", &nonce);
    if field("nonce").is_some() || field("login_hint").is_some() {
        return refuse("invalid_request");
    }
    let client_id = field("client_id").unwrap_or_default();
    let Some((scopes, redirects)) = declared(&client_id) else {
        return refuse("invalid_client");
    };
    let scope = field("scope").unwrap_or_default();
    let requested: Vec<&str> = scope.split(' ').filter(|word| !word.is_empty()).collect();
    if !requested.contains(&"atproto")
        || !requested
            .iter()
            .all(|word| scopes.iter().any(|declared| declared == word))
    {
        return refuse("invalid_scope");
    }
    let redirect_uri = field("redirect_uri").unwrap_or_default();
    if !redirects.contains(&redirect_uri)
        || !(redirect_uri.starts_with("http://127.0.0.1:")
            || redirect_uri.starts_with("http://[::1]:"))
    {
        return refuse("invalid_redirect_uri");
    }
    let state = field("state").unwrap_or_default();
    let challenge = field("code_challenge").unwrap_or_default();
    if field("response_type").as_deref() != Some("code")
        || state.is_empty()
        || challenge.len() != 43
        || field("code_challenge_method").as_deref() != Some("S256")
    {
        return refuse("invalid_request");
    }
    let request_uri = format!("urn:ietf:params:oauth:request_uri:{}", unique_token("req"));
    book.pushed.insert(
        request_uri.clone(),
        Pushed {
            client_id,
            redirect_uri,
            scope,
            state,
            challenge,
            key,
        },
    );
    Reply::json(
        201,
        &json!({ "request_uri": request_uri, "expires_in": 299 }),
    )
    .with_header("DPoP-Nonce", &nonce)
}

fn authorize(book: &mut Book, origin: &str, request: &Received) -> Reply {
    let request_uri = request.query("request_uri").unwrap_or_default();
    let Some(pushed) = book.pushed.remove(&request_uri) else {
        return error(400, "invalid_request");
    };
    if request.query("client_id").as_deref() != Some(pushed.client_id.as_str()) {
        return error(400, "invalid_client");
    }
    let back = |query: String| Reply::redirect(&format!("{}?{query}", pushed.redirect_uri));
    let Some(did) = book.approving.clone() else {
        return back(format!(
            "error=access_denied&state={}&iss={}",
            percent_encode(&pushed.state),
            percent_encode(origin)
        ));
    };
    let code = unique_token("code");
    let state = pushed.state.clone();
    let redirect = back(format!(
        "code={}&state={}&iss={}",
        percent_encode(&code),
        percent_encode(&state),
        percent_encode(origin)
    ));
    book.codes.insert(code, (pushed, did));
    redirect
}

fn token(book: &mut Book, origin: &str, request: &Received) -> Reply {
    let key = match proven(book, origin, request) {
        Ok(key) => key,
        Err(reply) => return reply,
    };
    let nonce = book.rotate();
    match request.field("grant_type").as_deref() {
        Some("authorization_code") => exchange(book, origin, request, &key, &nonce),
        Some("refresh_token") => refresh(book, origin, request, &key, &nonce),
        _ => error(400, "unsupported_grant_type").with_header("DPoP-Nonce", &nonce),
    }
}

fn exchange(
    book: &mut Book,
    origin: &str,
    request: &Received,
    key: &VerifyingKey,
    nonce: &str,
) -> Reply {
    let refuse = |code: &str| error(400, code).with_header("DPoP-Nonce", nonce);
    let code = request.field("code").unwrap_or_default();
    let Some((pushed, did)) = book.codes.remove(&code) else {
        return refuse("invalid_grant");
    };
    let verifier = request.field("code_verifier").unwrap_or_default();
    if request.field("client_id").as_deref() != Some(pushed.client_id.as_str())
        || request.field("redirect_uri").as_deref() != Some(pushed.redirect_uri.as_str())
    {
        return refuse("invalid_request");
    }
    if verifier.len() < 43 || base64url(&Sha256::digest(verifier.as_bytes())) != pushed.challenge {
        return refuse("invalid_grant");
    }
    if *key != pushed.key {
        return refuse("invalid_dpop_proof");
    }
    let subject = book.subject.clone().unwrap_or(did);
    let scope = book
        .granted_scope
        .clone()
        .unwrap_or_else(|| pushed.scope.clone());
    let access = access_token(book, origin, &subject, &scope);
    let refresh = unique_token("refresh");
    book.issued.push(refresh.clone());
    book.grants.push(Grant {
        did: subject.clone(),
        scope: pushed.scope.clone(),
        client_id: pushed.client_id,
        key: *key,
        refresh: refresh.clone(),
        access: access.clone(),
        ended: false,
    });
    let mut answer = token_answer(book, &access, &subject, &scope);
    if !book.without_refresh {
        answer["refresh_token"] = json!(refresh);
    }
    Reply::json(200, &answer).with_header("DPoP-Nonce", nonce)
}

fn refresh(
    book: &mut Book,
    origin: &str,
    request: &Received,
    key: &VerifyingKey,
    nonce: &str,
) -> Reply {
    let refuse = |code: &str| error(400, code).with_header("DPoP-Nonce", nonce);
    if book.limiting_refreshes {
        return error(429, "rate_limited").with_header("DPoP-Nonce", nonce);
    }
    if book.refusing_refreshes {
        return refuse("invalid_grant");
    }
    let presented = request.field("refresh_token").unwrap_or_default();
    let Some(index) = book
        .grants
        .iter()
        .position(|grant| grant.refresh == presented && !grant.ended)
    else {
        return refuse("invalid_grant");
    };
    let grant = book.grants[index].clone();
    if request.field("client_id").as_deref() != Some(grant.client_id.as_str()) {
        return refuse("invalid_client");
    }
    if *key != grant.key {
        return refuse("invalid_dpop_proof");
    }
    let scope = book
        .widened_refresh
        .clone()
        .unwrap_or_else(|| grant.scope.clone());
    let access = access_token(book, origin, &grant.did, &scope);
    let rotated = unique_token("refresh");
    book.issued.push(rotated.clone());
    book.grants[index].refresh = rotated.clone();
    book.grants[index].access = access.clone();
    let mut answer = token_answer(book, &access, &grant.did, &scope);
    answer["refresh_token"] = json!(rotated);
    Reply::json(200, &answer).with_header("DPoP-Nonce", nonce)
}

/// A fresh access token for `subject`, JWT-shaped unless the world issues
/// opaque ones.
fn access_token(book: &mut Book, origin: &str, subject: &str, scope: &str) -> String {
    let access = if book.opaque {
        unique_token("access")
    } else {
        format!(
            "{}.{}.{}",
            base64url(br#"{"alg":"ES256","typ":"at+jwt"}"#),
            base64url(
                json!({ "sub": subject, "scope": scope, "aud": origin })
                    .to_string()
                    .as_bytes()
            ),
            base64url(unique_token("signature").as_bytes())
        )
    };
    book.issued.push(access.clone());
    access
}

/// The token answer, without its refresh token.
fn token_answer(book: &Book, access: &str, subject: &str, scope: &str) -> Value {
    json!({
        "access_token": access,
        "token_type": book.token_type.clone().unwrap_or_else(|| "DPoP".to_owned()),
        "expires_in": book.access_lifetime.unwrap_or(3600),
        "scope": scope,
        "sub": subject,
    })
}

/// Ends the grant the presented token belongs to (RFC 7009): the answer is
/// 200 whether or not the token was known.
fn revoke(book: &mut Book, request: &Received) -> Reply {
    let token = request.field("token").unwrap_or_default();
    book.revocations.push(token.clone());
    for grant in &mut book.grants {
        if grant.refresh == token || grant.access == token {
            grant.ended = true;
        }
    }
    Reply::json(200, &json!({}))
}

/// Checks one DPoP proof the way a server does: a compact JWS of three
/// strict base64url segments, `typ` `dpop+jwt` and `alg` `ES256`, a P-256
/// JWK whose key verifies the signature, `htm` and `htu` of the request,
/// `iat` from a minute ago to five minutes ahead, an `exp` not passed, and a
/// `jti` the guard never saw. Answers the key and the claims.
pub fn check_proof(
    proof: &str,
    method: &str,
    url: &str,
    jtis: &mut HashSet<String>,
) -> Result<(VerifyingKey, Value), &'static str> {
    let segments: Vec<&str> = proof.split('.').collect();
    let [header, payload, signature] = segments[..] else {
        return Err("invalid_dpop_proof");
    };
    let decode = |segment: &str| base64url_decode(segment).ok_or("invalid_dpop_proof");
    let header_json: Value =
        serde_json::from_slice(&decode(header)?).map_err(|_| "invalid_dpop_proof")?;
    let claims: Value =
        serde_json::from_slice(&decode(payload)?).map_err(|_| "invalid_dpop_proof")?;
    if header_json["typ"] != "dpop+jwt" || header_json["alg"] != "ES256" {
        return Err("invalid_dpop_proof");
    }
    let jwk = &header_json["jwk"];
    if jwk["kty"] != "EC" || jwk["crv"] != "P-256" {
        return Err("invalid_dpop_proof");
    }
    let coordinate = |name: &str| {
        jwk[name]
            .as_str()
            .and_then(base64url_decode)
            .filter(|bytes| bytes.len() == 32)
            .ok_or("invalid_dpop_proof")
    };
    let mut point = vec![0x04];
    point.extend(coordinate("x")?);
    point.extend(coordinate("y")?);
    let key = VerifyingKey::from_sec1_bytes(&point).map_err(|_| "invalid_dpop_proof")?;
    let signature = Signature::from_slice(&decode(signature)?).map_err(|_| "invalid_dpop_proof")?;
    key.verify(format!("{header}.{payload}").as_bytes(), &signature)
        .map_err(|_| "invalid_dpop_proof")?;
    if claims["htm"] != method || claims["htu"] != url {
        return Err("invalid_dpop_proof");
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0);
    let iat = claims["iat"].as_i64().ok_or("invalid_dpop_proof")?;
    if !(now - 60..=now + 300).contains(&iat) || claims["exp"].as_i64().is_some_and(|exp| exp < now)
    {
        return Err("invalid_dpop_proof");
    }
    let jti = claims["jti"]
        .as_str()
        .filter(|jti| !jti.is_empty())
        .ok_or("invalid_dpop_proof")?;
    if !jtis.insert(jti.to_owned()) {
        return Err("invalid_dpop_proof");
    }
    Ok((key, claims))
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Unpadded base64url.
pub fn base64url(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let triple = chunk
            .iter()
            .enumerate()
            .fold(0u32, |triple, (index, byte)| {
                triple | (u32::from(*byte) << (16 - 8 * index))
            });
        for index in 0..=chunk.len() {
            out.push(char::from(
                ALPHABET[(triple >> (18 - 6 * index)) as usize & 63],
            ));
        }
    }
    out
}

/// Strict unpadded base64url: any other character, or leftover bits that
/// are not zero, is refused.
pub fn base64url_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut buffer, mut bits) = (0u32, 0u32);
    for byte in text.bytes() {
        let value = ALPHABET.iter().position(|candidate| *candidate == byte)? as u32;
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    (bits < 6 && buffer == 0).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_round_trips_strictly() {
        for bytes in [&b""[..], b"f", b"fo", b"foo", b"foob", &[0xff, 0xfe, 0x00]] {
            assert_eq!(base64url_decode(&base64url(bytes)).as_deref(), Some(bytes));
        }
        assert_eq!(base64url(b"foob"), "Zm9vYg");
        for bad in ["Zm9vYg=", "Zm9v+g", "Zh", "A"] {
            assert_eq!(base64url_decode(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_loopback_client_id_declares_its_scopes_and_redirects() {
        let client_id = format!(
            "{CLIENT_ORIGIN}?redirect_uri={}&scope={}",
            percent_encode("http://127.0.0.1:5219/signin/atproto/return"),
            percent_encode("atproto transition:generic")
        );
        let (scopes, redirects) = declared(&client_id).expect("declared");
        assert_eq!(scopes, ["atproto", "transition:generic"]);
        assert_eq!(redirects, ["http://127.0.0.1:5219/signin/atproto/return"]);
        assert_eq!(declared("http://127.0.0.1?scope=atproto"), None);
        assert_eq!(declared(&format!("{CLIENT_ORIGIN}?other=1")), None);
    }

    #[test]
    fn a_proof_is_refused_for_anything_but_a_perfect_shape() {
        let mut jtis = HashSet::new();
        for bad in ["", "a.b", "a.b.c", "....", "eyJ.eyJ.sig"] {
            assert!(
                check_proof(bad, "POST", "http://x/", &mut jtis).is_err(),
                "{bad}"
            );
        }
    }
}
