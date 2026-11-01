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
//! The PDS mints a service's proof at
//! `GET /xrpc/com.atproto.server.getServiceAuth?aud=..&lxm=..&exp=..` the
//! way the reference PDS does for an OAuth grant: the access token under
//! the `DPoP` scheme (a DPoP-bound token under `Bearer` is the reference
//! PDS's "Malformed token" 400), a proof of the request made with the key
//! the grant is bound to, carrying `ath` for that token and the PDS's own
//! nonce (one of its own, never the authorization server's), an `exp` at
//! most 130 seconds out, and a granted `rpc:<lxm>?aud=*` (or this very
//! audience) permission, without which it answers 403 `ScopeMissingError`.
//! A proof without the PDS's nonce is answered 401 with a fresh
//! `DPoP-Nonce` and `use_dpop_nonce`, in the body by default or in the
//! `WWW-Authenticate` header after [`AtprotoWorld::challenge_in_header`].
//! Each proof it mints is a JWT-shaped token kept in the world's
//! [`Proofs`], which the fake services that trust this world consult
//! instead of resolving the account's signing key.
//!
//! The PDS also keeps each account's repository and answers for the app
//! view, the way the reference PDS does for an OAuth grant. Every such
//! request presents the access token under `DPoP` with a proof of that very
//! request (the same checks as above: a DPoP-bound token under `Bearer` is
//! the "Malformed token" 400, and a proof without the PDS's nonce is
//! challenged), and what the grant was given decides what it may do,
//! answered 403 `ScopeMissingError` otherwise:
//!
//! * `GET /xrpc/com.atproto.server.getSession` names the account, its DID
//!   and handle, to any grant;
//! * `POST /xrpc/com.atproto.repo.createRecord` writes a record into the
//!   grant's own repository only, and needs `repo:<collection>` (or
//!   `repo:*`, or a `repo:<collection>?action=create` form); a record's
//!   `$type` must be its collection, a post's text at most 300 characters,
//!   and every strong reference it holds (a reply's root and parent, a like
//!   or repost's subject, an image's blob) must name what exists, uri and
//!   cid alike, or it is a 400 `InvalidRequest`;
//! * `POST /xrpc/com.atproto.repo.uploadBlob` keeps the body as a blob of
//!   the declared image type, at most [`BLOB_LIMIT`] bytes, and needs a
//!   `blob:` permission whose pattern covers that type;
//! * `GET /xrpc/app.bsky.feed.getTimeline` and `getPosts`, proxied with
//!   `atproto-proxy:` [`APP_VIEW`], answer from the posts the world holds,
//!   newest first, and need `rpc:<method>?aud=` that service, or `aud=*`;
//!   without the proxy header the PDS does not implement them.
//!
//! Each answer carries the PDS's current nonce in `DPoP-Nonce`.
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

use p256::ecdsa::signature::{Signer as _, Verifier as _};
use p256::ecdsa::{Signature, SigningKey, VerifyingKey};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::listener::{Listener, Received, Reply};
use crate::{percent_decode, percent_encode, unique_token};

/// The loopback client id's origin, before its query.
pub const CLIENT_ORIGIN: &str = "http://localhost";
/// The app view the PDS proxies reads to, as a request names it in its
/// `atproto-proxy` header.
pub const APP_VIEW: &str = "did:web:api.bsky.app#bsky_appview";
/// The largest blob the PDS keeps, in bytes.
pub const BLOB_LIMIT: usize = 1_000_000;

/// A record in an account's repository.
#[derive(Debug, Clone, PartialEq)]
pub struct RepoRecord {
    /// Its `at://` URI.
    pub uri: String,
    /// Its content identifier.
    pub cid: String,
    /// The account whose repository holds it.
    pub did: String,
    /// Its collection.
    pub collection: String,
    /// The record itself.
    pub value: Value,
}

/// A blob an account uploaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoBlob {
    /// Its content identifier.
    pub cid: String,
    /// The account that uploaded it.
    pub did: String,
    /// Its declared media type.
    pub mime: String,
    /// Its size in bytes.
    pub size: usize,
}

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
    /// What the current access token was granted.
    granted: String,
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
    /// Every refresh token the token endpoint took up past its DPoP checks.
    refreshes: Vec<String>,
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
    /// The PDS's own nonce, which rotates on every challenge.
    pds_nonce: String,
    pds_jtis: HashSet<String>,
    challenge_in_header: bool,
    records: Vec<RepoRecord>,
    blobs: Vec<RepoBlob>,
    display_names: HashMap<String, String>,
    sequence: u64,
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

/// A proof the PDS minted for a service: who it proves, for which
/// audience and method, until when, under which id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceProof {
    /// The token the service is presented.
    pub token: String,
    /// The account it proves, its `iss`.
    pub issuer: String,
    /// The audience it is for, its `aud`.
    pub audience: String,
    /// The method it allows, its `lxm`.
    pub method: String,
    /// When it expires, in seconds since the Unix epoch.
    pub expires_at: i64,
    /// Its `jti`.
    pub id: String,
}

/// Every proof one world's PDS minted, shared with the fake services that
/// trust that world.
#[derive(Debug, Clone, Default)]
pub struct Proofs(Arc<Mutex<Vec<ServiceProof>>>);

impl Proofs {
    /// The proof `token`, when this world's PDS minted it.
    pub fn verify(&self, token: &str) -> Option<ServiceProof> {
        locked(&self.0)
            .iter()
            .find(|proof| proof.token == token)
            .cloned()
    }

    /// Every proof minted so far, oldest first.
    pub fn minted(&self) -> Vec<ServiceProof> {
        locked(&self.0).clone()
    }
}

/// The knobs of one world, kept apart from its listeners.
#[derive(Debug, Clone)]
pub struct Knobs(Arc<Mutex<Book>>);

impl Knobs {
    /// Makes the code exchange say it granted `scope`, whatever was asked.
    pub fn grants_scope(&self, scope: &str) {
        locked(&self.0).granted_scope = Some(scope.to_owned());
    }

    /// Makes the code exchange grant what was asked again.
    pub fn grants_what_is_asked(&self) {
        locked(&self.0).granted_scope = None;
    }
}

/// A fake authorization server and PDS on their own loopback listeners.
pub struct AtprotoWorld {
    server: Listener,
    pds: Listener,
    book: Arc<Mutex<Book>>,
    proofs: Proofs,
}

impl Default for AtprotoWorld {
    fn default() -> Self {
        Self::start()
    }
}

impl AtprotoWorld {
    /// Starts a world with no accounts.
    pub fn start() -> Self {
        let book = Arc::new(Mutex::new(Book {
            pds_nonce: unique_token("pds-nonce"),
            ..Book::default()
        }));
        let proofs = Proofs::default();
        let origins = Arc::new(Mutex::new((String::new(), String::new())));
        let (serving, server_origins) = (book.clone(), origins.clone());
        let server = Listener::start(move |request| {
            let (server, pds) = locked(&server_origins).clone();
            answer_server(&serving, &server, &pds, request)
        });
        let (resourcing, pds_origins, minting) = (book.clone(), origins.clone(), proofs.clone());
        let pds = Listener::start(move |request| {
            let (server, pds) = locked(&pds_origins).clone();
            answer_pds(&resourcing, &minting, &server, &pds, request)
        });
        *locked(&origins) = (server.origin(), pds.origin());
        Self {
            server,
            pds,
            book,
            proofs,
        }
    }

    /// A handle on this world's knobs that another thread may keep, such as
    /// a control page.
    pub fn knobs(&self) -> Knobs {
        Knobs(self.book.clone())
    }

    /// Every proof this world's PDS minted, shared with whoever checks them.
    pub fn proofs(&self) -> Proofs {
        self.proofs.clone()
    }

    /// Makes the PDS say `use_dpop_nonce` in the `WWW-Authenticate` header
    /// from now on, as the reference PDS does, instead of in the body.
    pub fn challenge_in_header(&self, in_header: bool) {
        locked(&self.book).challenge_in_header = in_header;
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

    /// Makes the code exchange grant what was asked again, as it does
    /// unless told otherwise.
    pub fn grants_what_is_asked(&self) {
        locked(&self.book).granted_scope = None;
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

    /// Stops taking every access token issued so far at the PDS, as a
    /// server that ends access tokens early does; the refresh tokens still
    /// work.
    pub fn end_access_tokens(&self) {
        for grant in &mut locked(&self.book).grants {
            grant.access = unique_token("ended-access");
        }
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

    /// Every refresh token the token endpoint took up, oldest first,
    /// whether or not it refreshed anything: each request past the DPoP
    /// checks, so a request answered only with a fresh nonce is not one.
    pub fn refresh_tokens_presented(&self) -> Vec<String> {
        locked(&self.book).refreshes.clone()
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

    /// Writes a post by the account `did` into its repository, as if it
    /// posted elsewhere, and answers its URI.
    pub fn add_post(&self, did: &str, text: &str) -> String {
        let mut book = locked(&self.book);
        let record = json!({
            "$type": "app.bsky.feed.post",
            "text": text,
            "createdAt": "2026-10-01T12:00:00.000Z",
        });
        keep_record(&mut book, did, "app.bsky.feed.post", record).uri
    }

    /// Makes the app view show `name` as the display name of `did`.
    pub fn display_name(&self, did: &str, name: &str) {
        locked(&self.book)
            .display_names
            .insert(did.to_owned(), name.to_owned());
    }

    /// Every record of `collection` in any repository, oldest first.
    pub fn records(&self, collection: &str) -> Vec<RepoRecord> {
        locked(&self.book)
            .records
            .iter()
            .filter(|record| record.collection == collection)
            .cloned()
            .collect()
    }

    /// Every blob uploaded so far, oldest first.
    pub fn blobs(&self) -> Vec<RepoBlob> {
        locked(&self.book).blobs.clone()
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

fn answer_pds(
    book: &Mutex<Book>,
    proofs: &Proofs,
    server: &str,
    pds: &str,
    request: &Received,
) -> Reply {
    let mut book = locked(book);
    match (request.method.as_str(), request.route()) {
        ("GET", "/.well-known/oauth-protected-resource") => {
            let names = book.pds_names.clone().unwrap_or_else(|| server.to_owned());
            Reply::json(
                200,
                &json!({ "resource": pds, "authorization_servers": [names] }),
            )
        }
        ("GET", "/xrpc/com.atproto.server.getServiceAuth") => {
            service_auth(&mut book, proofs, pds, request)
        }
        ("GET", "/xrpc/com.atproto.server.getSession")
        | ("POST", "/xrpc/com.atproto.repo.createRecord")
        | ("POST", "/xrpc/com.atproto.repo.uploadBlob")
        | ("GET", "/xrpc/app.bsky.feed.getTimeline")
        | ("GET", "/xrpc/app.bsky.feed.getPosts") => {
            let reply = match authenticated(&mut book, pds, request) {
                Ok(grant) => repository(&mut book, &grant, request),
                Err(reply) => reply,
            };
            if reply
                .headers
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("dpop-nonce"))
            {
                reply
            } else {
                reply.with_header("DPoP-Nonce", &book.pds_nonce)
            }
        }
        _ => error(404, "NotFound"),
    }
}

/// An XRPC error the way a PDS answers one.
fn xrpc_error(status: u16, code: &str, message: &str) -> Reply {
    Reply::json(status, &json!({ "error": code, "message": message }))
}

/// `getServiceAuth` for an OAuth grant: see the module's documentation.
fn service_auth(book: &mut Book, proofs: &Proofs, pds: &str, request: &Received) -> Reply {
    let presented = request
        .header("authorization")
        .unwrap_or_default()
        .to_owned();
    let (scheme, token) = presented.split_once(' ').unwrap_or(("", ""));
    let Some(grant) = book
        .grants
        .iter()
        .find(|grant| grant.access == token && !grant.ended)
        .cloned()
    else {
        return xrpc_error(401, "InvalidToken", "Token could not be verified");
    };
    if scheme != "DPoP" {
        return xrpc_error(400, "InvalidToken", "Malformed token");
    }
    let url = format!("{pds}{}", request.route());
    let proof = request.header("dpop").unwrap_or_default().to_owned();
    let nonce = book.pds_nonce.clone();
    let challenge = |book: &mut Book| {
        let fresh = unique_token("pds-nonce");
        book.pds_nonce = fresh.clone();
        let reply = if book.challenge_in_header {
            xrpc_error(401, "InvalidToken", "DPoP nonce mismatch").with_header(
                "WWW-Authenticate",
                r#"DPoP error="use_dpop_nonce", error_description="Resource server requires nonce in DPoP proof""#,
            )
        } else {
            xrpc_error(401, "use_dpop_nonce", "DPoP nonce mismatch")
        };
        reply.with_header("DPoP-Nonce", &fresh)
    };
    let Ok((key, claims)) = check_proof(&proof, "GET", &url, &mut book.pds_jtis) else {
        return xrpc_error(401, "InvalidDpopProof", "DPoP proof invalid");
    };
    if claims["nonce"].as_str() != Some(nonce.as_str()) {
        return challenge(book);
    }
    if key != grant.key
        || claims["ath"].as_str() != Some(base64url(&Sha256::digest(token.as_bytes())).as_str())
    {
        return xrpc_error(
            401,
            "InvalidDpopProof",
            "DPoP proof does not match the token",
        );
    }
    let (Some(audience), Some(method), Some(exp)) = (
        request.query("aud"),
        request.query("lxm"),
        request.query("exp").and_then(|exp| exp.parse::<i64>().ok()),
    ) else {
        return xrpc_error(400, "InvalidRequest", "aud, lxm and exp are required");
    };
    let now = now_secs();
    if !(now..=now + 130).contains(&exp) {
        return xrpc_error(400, "BadExpiration", "exp is outside the accepted window");
    }
    let covers = grant.granted.split(' ').any(|word| {
        word == format!("rpc:{method}?aud=*") || word == format!("rpc:{method}?aud={audience}")
    });
    if !covers {
        return xrpc_error(
            403,
            "ScopeMissingError",
            &format!("Missing required scope \"rpc:{method}?aud={audience}\""),
        );
    }
    let id = unique_token("jti");
    let signed = format!(
        "{}.{}",
        base64url(br##"{"typ":"JWT","alg":"ES256","kid":"#atproto"}"##),
        base64url(
            json!({ "iss": grant.did, "aud": audience, "lxm": method, "exp": exp, "iat": now, "jti": id })
                .to_string()
                .as_bytes()
        ),
    );
    let signature: Signature = account_key(&grant.did).sign(signed.as_bytes());
    let signature = signature.normalize_s().unwrap_or(signature);
    let token = format!("{signed}.{}", base64url(&signature.to_bytes()));
    book.issued.push(token.clone());
    locked(&proofs.0).push(ServiceProof {
        token: token.clone(),
        issuer: grant.did,
        audience,
        method,
        expires_at: exp,
        id,
    });
    Reply::json(200, &json!({ "token": token }))
}

/// The grant whose access token a resource request presents, checked the
/// way the reference PDS checks one: see the module's documentation.
fn authenticated(book: &mut Book, pds: &str, request: &Received) -> Result<Grant, Reply> {
    let presented = request
        .header("authorization")
        .unwrap_or_default()
        .to_owned();
    let (scheme, token) = presented.split_once(' ').unwrap_or(("", ""));
    let Some(grant) = book
        .grants
        .iter()
        .find(|grant| grant.access == token && !grant.ended)
        .cloned()
    else {
        return Err(xrpc_error(
            401,
            "InvalidToken",
            "Token could not be verified",
        ));
    };
    if scheme != "DPoP" {
        return Err(xrpc_error(400, "InvalidToken", "Malformed token"));
    }
    let url = format!("{pds}{}", request.route());
    let proof = request.header("dpop").unwrap_or_default().to_owned();
    let nonce = book.pds_nonce.clone();
    let Ok((key, claims)) = check_proof(&proof, &request.method, &url, &mut book.pds_jtis) else {
        return Err(xrpc_error(401, "InvalidDpopProof", "DPoP proof invalid"));
    };
    if claims["nonce"].as_str() != Some(nonce.as_str()) {
        let fresh = unique_token("pds-nonce");
        book.pds_nonce = fresh.clone();
        let reply = if book.challenge_in_header {
            xrpc_error(401, "InvalidToken", "DPoP nonce mismatch").with_header(
                "WWW-Authenticate",
                r#"DPoP error="use_dpop_nonce", error_description="Resource server requires nonce in DPoP proof""#,
            )
        } else {
            xrpc_error(401, "use_dpop_nonce", "DPoP nonce mismatch")
        };
        return Err(reply.with_header("DPoP-Nonce", &fresh));
    }
    if key != grant.key
        || claims["ath"].as_str() != Some(base64url(&Sha256::digest(token.as_bytes())).as_str())
    {
        return Err(xrpc_error(
            401,
            "InvalidDpopProof",
            "DPoP proof does not match the token",
        ));
    }
    Ok(grant)
}

/// Whether `granted` holds a `repo:` permission to create in `collection`.
fn grants_repo(granted: &str, collection: &str) -> bool {
    granted.split(' ').any(|word| {
        let Some(rest) = word.strip_prefix("repo:") else {
            return false;
        };
        let (target, query) = rest.split_once('?').unwrap_or((rest, ""));
        (target == collection || target == "*")
            && (query.is_empty() || query.split('&').any(|pair| pair == "action=create"))
    })
}

/// Whether `granted` holds a `blob:` permission whose pattern covers `mime`.
fn grants_blob(granted: &str, mime: &str) -> bool {
    let kind = mime.split('/').next().unwrap_or(mime);
    granted.split(' ').any(|word| {
        word.strip_prefix("blob:").is_some_and(|pattern| {
            pattern == mime || pattern == "*/*" || pattern == format!("{kind}/*")
        })
    })
}

/// Whether `granted` lets the grant call `method` at `audience` through
/// the PDS.
fn grants_rpc(granted: &str, method: &str, audience: &str) -> bool {
    granted.split(' ').any(|word| {
        word == format!("rpc:{method}?aud=*")
            || word == format!("rpc:{method}?aud={audience}")
            || word == format!("rpc:{method}?aud={}", percent_encode(audience))
    })
}

fn scope_missing(scope: &str) -> Reply {
    xrpc_error(
        403,
        "ScopeMissingError",
        &format!("Missing required scope {scope:?}"),
    )
}

/// A content identifier for `bytes`, shaped like one.
fn cid_of(prefix: &str, bytes: &[u8]) -> String {
    let digest: String = Sha256::digest(bytes)
        .iter()
        .map(|byte| char::from(b"abcdefghijklmnopqrstuvwxyz234567"[usize::from(byte % 32)]))
        .collect();
    format!("{prefix}{digest}")
}

/// Keeps `value` as a new record of `collection` in `did`'s repository.
fn keep_record(book: &mut Book, did: &str, collection: &str, value: Value) -> RepoRecord {
    const TID: &[u8; 32] = b"234567abcdefghijklmnopqrstuvwxyz";
    book.sequence += 1;
    let mut left = book.sequence;
    let mut digits = Vec::new();
    for _ in 0..11 {
        digits.push(char::from(TID[(left % 32) as usize]));
        left /= 32;
    }
    let rkey: String = "3l".chars().chain(digits.into_iter().rev()).collect();
    let cid = cid_of(
        "bafyrei",
        format!("{did}/{collection}/{rkey}/{value}").as_bytes(),
    );
    let record = RepoRecord {
        uri: format!("at://{did}/{collection}/{rkey}"),
        cid,
        did: did.to_owned(),
        collection: collection.to_owned(),
        value,
    };
    book.records.push(record.clone());
    record
}

/// Whether `reference` is a strong reference to a record that exists.
fn names_a_record(book: &Book, reference: &Value) -> bool {
    book.records.iter().any(|record| {
        reference["uri"].as_str() == Some(record.uri.as_str())
            && reference["cid"].as_str() == Some(record.cid.as_str())
    })
}

/// The app view's picture of one post.
fn post_view(book: &Book, record: &RepoRecord) -> Value {
    let handle = book.account(&record.did).map_or_else(
        || "handle.invalid".to_owned(),
        |account| account.handle.clone(),
    );
    let counted = |collection: &str| {
        book.records
            .iter()
            .filter(|other| {
                other.collection == collection
                    && other.value["subject"]["uri"].as_str() == Some(record.uri.as_str())
            })
            .count()
    };
    let replies = book
        .records
        .iter()
        .filter(|other| {
            other.collection == "app.bsky.feed.post"
                && other.value["reply"]["parent"]["uri"].as_str() == Some(record.uri.as_str())
        })
        .count();
    let mut author = json!({ "did": record.did, "handle": handle });
    if let Some(name) = book.display_names.get(&record.did) {
        author["displayName"] = json!(name);
    }
    json!({
        "uri": record.uri,
        "cid": record.cid,
        "author": author,
        "record": record.value,
        "replyCount": replies,
        "repostCount": counted("app.bsky.feed.repost"),
        "likeCount": counted("app.bsky.feed.like"),
        "indexedAt": record.value["createdAt"],
    })
}

/// What the app view answers to `method`, proxied through the PDS.
fn app_view(book: &Book, grant: &Grant, method: &str, request: &Received) -> Reply {
    if request.header("atproto-proxy") != Some(APP_VIEW) {
        return xrpc_error(501, "MethodNotImplemented", "Method Not Implemented");
    }
    if !grants_rpc(&grant.granted, method, APP_VIEW) {
        return scope_missing(&format!("rpc:{method}?aud={APP_VIEW}"));
    }
    let posts: Vec<&RepoRecord> = book
        .records
        .iter()
        .filter(|record| record.collection == "app.bsky.feed.post")
        .collect();
    if method == "app.bsky.feed.getPosts" {
        let wanted: Vec<String> = request
            .target
            .split_once('?')
            .map(|(_, query)| query)
            .unwrap_or_default()
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .filter(|(name, _)| *name == "uris")
            .map(|(_, value)| percent_decode(value))
            .collect();
        if wanted.is_empty() || wanted.len() > 25 {
            return xrpc_error(400, "InvalidRequest", "uris must name 1 to 25 posts");
        }
        let found: Vec<Value> = wanted
            .iter()
            .filter_map(|uri| posts.iter().find(|record| record.uri == *uri))
            .map(|record| post_view(book, record))
            .collect();
        return Reply::json(200, &json!({ "posts": found }));
    }
    let limit = request
        .query("limit")
        .map_or(Some(50), |limit| limit.parse::<usize>().ok());
    let Some(limit) = limit.filter(|limit| (1..=100).contains(limit)) else {
        return xrpc_error(400, "InvalidRequest", "limit must be 1 to 100");
    };
    let start = request
        .query("cursor")
        .and_then(|cursor| cursor.strip_prefix("page-").map(str::to_owned))
        .and_then(|skip| skip.parse::<usize>().ok())
        .unwrap_or(0);
    let page: Vec<Value> = posts
        .iter()
        .rev()
        .skip(start)
        .take(limit)
        .map(|record| json!({ "post": post_view(book, record) }))
        .collect();
    let mut answer = json!({ "feed": page });
    if start + limit < posts.len() {
        answer["cursor"] = json!(format!("page-{}", start + limit));
    }
    Reply::json(200, &answer)
}

/// A resource request from `grant`, past its DPoP checks.
fn repository(book: &mut Book, grant: &Grant, request: &Received) -> Reply {
    let route = request.route().to_owned();
    if let Some(method) = route
        .strip_prefix("/xrpc/")
        .filter(|method| method.starts_with("app."))
    {
        return app_view(book, grant, method, request);
    }
    match route.as_str() {
        "/xrpc/com.atproto.server.getSession" => {
            let handle = book.account(&grant.did).map_or_else(
                || "handle.invalid".to_owned(),
                |account| account.handle.clone(),
            );
            Reply::json(200, &json!({ "did": grant.did, "handle": handle }))
        }
        "/xrpc/com.atproto.repo.uploadBlob" => upload_blob(book, grant, request),
        _ => create_record(book, grant, request),
    }
}

/// `com.atproto.repo.uploadBlob` for the grant's account.
fn upload_blob(book: &mut Book, grant: &Grant, request: &Received) -> Reply {
    let mime = request
        .header("content-type")
        .unwrap_or_default()
        .to_owned();
    if !matches!(
        mime.as_str(),
        "image/png" | "image/jpeg" | "image/webp" | "image/gif"
    ) {
        return xrpc_error(400, "InvalidRequest", "unsupported blob type");
    }
    if !grants_blob(&grant.granted, &mime) {
        return scope_missing(&format!("blob:{mime}"));
    }
    if request.body.is_empty() || request.body.len() > BLOB_LIMIT {
        return xrpc_error(400, "BlobTooLarge", "blob is empty or too large");
    }
    let cid = cid_of("bafkrei", &request.body);
    book.blobs.push(RepoBlob {
        cid: cid.clone(),
        did: grant.did.clone(),
        mime: mime.clone(),
        size: request.body.len(),
    });
    Reply::json(
        200,
        &json!({ "blob": {
            "$type": "blob",
            "ref": { "$link": cid },
            "mimeType": mime,
            "size": request.body.len(),
        } }),
    )
}

/// `com.atproto.repo.createRecord` into the grant's own repository.
fn create_record(book: &mut Book, grant: &Grant, request: &Received) -> Reply {
    let body = request.json();
    let invalid = |message: &str| xrpc_error(400, "InvalidRequest", message);
    let collection = body["collection"].as_str().unwrap_or_default().to_owned();
    let record = body["record"].clone();
    if body["repo"].as_str() != Some(grant.did.as_str()) {
        return invalid("repo must be the account's own");
    }
    if !grants_repo(&grant.granted, &collection) {
        return scope_missing(&format!("repo:{collection}?action=create"));
    }
    if record["$type"].as_str() != Some(collection.as_str())
        || record["createdAt"].as_str().is_none_or(str::is_empty)
    {
        return invalid("record must be of its collection and say when it was made");
    }
    match collection.as_str() {
        "app.bsky.feed.post" => {
            let text = record["text"].as_str().unwrap_or_default();
            if text.chars().count() > 300 || (text.is_empty() && record["embed"].is_null()) {
                return invalid("text must be 1 to 300 characters");
            }
            if !record["reply"].is_null()
                && !(names_a_record(book, &record["reply"]["root"])
                    && names_a_record(book, &record["reply"]["parent"]))
            {
                return invalid("reply must name its root and parent");
            }
            if !record["embed"].is_null() {
                let images = record["embed"]["images"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                let known = images.iter().all(|image| {
                    let link = image["image"]["ref"]["$link"].as_str().unwrap_or_default();
                    image["alt"].is_string()
                        && book
                            .blobs
                            .iter()
                            .any(|blob| blob.cid == link && blob.did == grant.did)
                });
                if record["embed"]["$type"] != "app.bsky.embed.images"
                    || images.is_empty()
                    || images.len() > 4
                    || !known
                {
                    return invalid("images must name blobs the account uploaded");
                }
            }
        }
        "app.bsky.feed.like" | "app.bsky.feed.repost" => {
            if !names_a_record(book, &record["subject"]) {
                return invalid("subject must name a record");
            }
        }
        _ => return invalid("unknown collection"),
    }
    let kept = keep_record(book, &grant.did, &collection, record);
    Reply::json(200, &json!({ "uri": kept.uri, "cid": kept.cid }))
}

/// The seconds since the Unix epoch.
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

fn document(account: &Account, pds: &str) -> Value {
    json!({
        "id": account.did,
        "alsoKnownAs": [format!("at://{}", account.handle)],
        "verificationMethod": [{
            "id": format!("{}#atproto", account.did),
            "type": "Multikey",
            "controller": account.did,
            "publicKeyMultibase": public_key_multibase(&account.did),
        }],
        "service": [{ "id": "#atproto_pds", "type": "AtprotoPersonalDataServer", "serviceEndpoint": pds }],
    })
}

/// The key an account signs its service proofs with, fixed by its DID so a
/// real verifier can check a proof against the account's document.
fn account_key(did: &str) -> SigningKey {
    let seed = Sha256::digest(format!("lobby-testkit account key {did}").as_bytes());
    SigningKey::from_slice(&seed).expect("a digest is a valid P-256 scalar")
}

/// The account's public key as a `Multikey`: base58btc of the P-256 multicodec
/// prefix and the compressed point.
fn public_key_multibase(did: &str) -> String {
    let point = account_key(did).verifying_key().to_encoded_point(true);
    let mut bytes = vec![0x80, 0x24];
    bytes.extend_from_slice(point.as_bytes());
    format!("z{}", base58(&bytes))
}

fn base58(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    let mut digits: Vec<u8> = Vec::new();
    for &byte in bytes {
        let mut carry = u32::from(byte);
        for digit in &mut digits {
            carry += u32::from(*digit) << 8;
            *digit = (carry % 58) as u8;
            carry /= 58;
        }
        while carry > 0 {
            digits.push((carry % 58) as u8);
            carry /= 58;
        }
    }
    let leading = bytes.iter().take_while(|byte| **byte == 0).count();
    std::iter::repeat_n('1', leading)
        .chain(
            digits
                .iter()
                .rev()
                .map(|digit| ALPHABET[*digit as usize] as char),
        )
        .collect()
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
        granted: scope.clone(),
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
    let presented = request.field("refresh_token").unwrap_or_default();
    book.refreshes.push(presented.clone());
    if book.limiting_refreshes {
        return error(429, "rate_limited").with_header("DPoP-Nonce", nonce);
    }
    if book.refusing_refreshes {
        return refuse("invalid_grant");
    }
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
    book.grants[index].granted = scope.clone();
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
