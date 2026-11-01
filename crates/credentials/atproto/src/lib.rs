//! The atproto credential adapter.
//!
//! It signs a companion's account in with atproto OAuth, the profile for a
//! client on the sponsor's own machine, and is the only reader of the
//! material it hands the daemon. Every request goes through the outbound
//! agent under the [`World`]'s address policy: HTTPS to public addresses
//! only, with names resolved once and checked, no redirect followed and no
//! proxy taken from the environment.
//!
//! Beginning a sign-in:
//!
//! 1. With nothing typed, the sign-in goes to the default authorization
//!    server, which lets the sponsor choose the account in its own pages; no
//!    account is named in advance, so no `login_hint` is sent. With a handle
//!    or a DID typed, the adapter resolves it first: handle to DID (the
//!    document must claim the handle back), DID to document, document to
//!    PDS, and the PDS's protected-resource metadata to its authorization
//!    server.
//! 2. The authorization server's metadata must keep every promise the
//!    sign-in relies on (see [`server`]); a server that does not is refused
//!    before anything is pushed.
//! 3. A pushed authorization request (PAR) carries the loopback client id,
//!    which declares the lobby's return route and the scope (see
//!    [`client`]), that redirect URI, `response_type=code`, the scope
//!    `atproto`, the lobby's `state`, and a PKCE S256 challenge, with a DPoP
//!    proof made with a key fresh for this sign-in. No `nonce` parameter is
//!    sent: the pushed request URI takes its place. The server's DPoP nonce
//!    is learned from its first answer and the request made once more.
//! 4. The flight is parked in memory under the `state` (see [`flight`]) and
//!    the browser is sent to the authorize endpoint with the client id and
//!    the request URI.
//!
//! Completing it, on the lobby's return route:
//!
//! 1. The `state` must be the one this sign-in carried and its flight still
//!    parked and within its time; the flight is used once. A return that
//!    says the sponsor declined, or carries no `iss` naming the very
//!    authorization server the flight went to, or no usable code, signs
//!    nothing in.
//! 2. The code is exchanged at the token endpoint with the PKCE verifier, the
//!    same redirect URI and client id, and a proof made with the flight's
//!    key. The answer must be a DPoP-bound token pair whose `sub` is a DID
//!    and whose granted scope includes `atproto` and lies inside what was
//!    asked. When the sponsor named an account, `sub` must be that account:
//!    another account the provider signed in never becomes the credential.
//! 3. Only then is `sub` resolved: its document's PDS must be the one the
//!    sponsor's account named, when it named one, and that PDS must name the
//!    flight's authorization server.
//! 4. The account is `sub`; the permissions are the granted scope; the
//!    material is the session (see [`material`]). The label the sponsor
//!    sees is the first handle the document claims that resolves back to
//!    `sub`; an account with no such handle is labelled by `sub` itself.
//!
//! Signing an account in again, when its credential needs it, starts from
//! the credential's own material: the account is its DID, resolved as if the
//! sponsor had typed it, so only that account can complete the sign-in.
//!
//! A session is kept alive by refreshing it a minute before its access
//! token expires, and revoked at the authorization server when the sponsor
//! revokes the credential and the server offers revocation; see
//! [`renewal`] for the order a refresh follows and what each answer means.
//!
//! A service that asks for a proof of the account gets one minted at the
//! account's PDS, for exactly the audience and method it declared, when
//! the account was granted that method; see [`proofs`]. A service that
//! lives on the account's own PDS asks for a proof of each request it
//! sends there instead, and presents the access token under `DPoP` beside
//! it, never as a bearer token; see [`requests`]. The account's home is
//! its PDS. Every sign-in asks
//! for `atproto` and for what the lobby's services need an account of this
//! kind granted.
//!
//! The word session here means the OAuth session the authorization server
//! grants, and nothing else.

pub mod client;
pub mod codec;
pub mod dpop;
pub mod flight;
pub mod identity;
pub mod material;
pub mod proofs;
pub mod renewal;
pub mod requests;
pub mod server;
pub mod tokens;
pub mod wire;

use std::time::{Duration, Instant};

use lobby_contracts::{
    AccountKey, AccountLabel, AccountProfile, Address, Authorization, AuthorizationAsk,
    CredentialAdapter, Explained, Freshness, MaterialSlot, Outcome, ProviderKind, Revocation,
    Secret, SignInRequest, SignInReturn, SignInStart, SignedIn,
};
use lobby_outbound::Outbound;
use serde_json::Value;

use crate::codec::{base64url, is_token, now_secs, percent_encode, random_token, sha256};
use crate::dpop::DpopKey;
use crate::flight::{Flight, Flights};
use crate::identity::Named;
use crate::material::Material;
use crate::renewal::{Locks, Nonces, Renewer};
use crate::tokens::{TOKEN_LIMIT, Tokens};
use crate::wire::{json_object, proved_post, status_outcome};

pub use crate::wire::World;

/// The provider kind of this adapter.
pub const KIND: &str = "atproto";

/// What the sponsor reads for this kind of account on the manager page: the
/// name people know these accounts by.
pub const TITLE: &str = "Bluesky";

/// What the sponsor reads beside the field naming the account to sign in
/// with.
pub const HINT: &str = "Your handle, such as yourname.bsky.social. Leave it empty to choose the account on the sign-in page.";

/// The scope every sign-in asks for: the account's identity.
pub const SCOPE: &str = "atproto";

/// How many of the handles a document claims are tried as its label.
const LABEL_HANDLES: usize = 3;

/// The scope a sign-in asks for: `atproto`, then each permission the
/// lobby's services need that is one word of the scope's shape, once.
pub fn scope_for(permissions: &[String]) -> String {
    let mut words = vec![SCOPE.to_owned()];
    for permission in permissions {
        if is_token(permission, 256) && !words.contains(permission) {
            words.push(permission.clone());
        }
    }
    words.join(" ")
}

/// The atproto credential adapter.
pub struct Atproto {
    world: World,
    outbound: Outbound,
    flights: Flights,
    locks: Locks,
    nonces: Nonces,
}

impl std::fmt::Debug for Atproto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Atproto")
            .field("world", &self.world)
            .finish()
    }
}

impl Atproto {
    /// An adapter that resolves and signs in within `world`.
    pub fn new(world: World) -> Self {
        Self::with_time_limit(world, flight::TIME_LIMIT)
    }

    /// An adapter whose sign-ins may take `time_limit` to come back.
    pub fn with_time_limit(world: World, time_limit: Duration) -> Self {
        let outbound = Outbound::new(world.policy);
        Self {
            world,
            outbound,
            flights: Flights::new(time_limit),
            locks: Locks::default(),
            nonces: Nonces::default(),
        }
    }

    fn renewer(&self) -> Renewer<'_> {
        Renewer {
            outbound: &self.outbound,
            policy: self.world.policy,
            locks: &self.locks,
            nonces: &self.nonces,
        }
    }

    /// Where a sign-in for what the sponsor typed goes: the authorization
    /// server, and the account named with its PDS, when one was.
    fn destination(&self, hint: &str) -> Result<(Address, Option<(String, Address)>), Outcome> {
        if hint.trim().is_empty() {
            return Ok((self.world.authserver.clone(), None));
        }
        let (did, handle) = match identity::named(hint).ok_or(Outcome::InvalidInput)? {
            Named::Did(did) => (did, None),
            Named::Handle(handle) => (
                identity::resolve_handle(&self.world, &self.outbound, &handle)?,
                Some(handle),
            ),
        };
        let document = identity::document(&self.world, &self.outbound, &did)?;
        if handle.is_some_and(|handle| !document.handles.contains(&handle)) {
            return Err(Outcome::NotFound);
        }
        let authserver =
            identity::authorization_server(&self.world, &self.outbound, &document.pds)?;
        Ok((authserver, Some((did, document.pds))))
    }

    /// What the sponsor knows the account `did` by: the first of the first
    /// [`LABEL_HANDLES`] `handles`, claimed by its document, that resolves
    /// back to `did`, or `did` when none does. The label only names the
    /// account, so a handle whose host cannot be reached is passed over and
    /// never fails a sign-in the provider already completed.
    fn label(&self, did: &str, handles: &[String]) -> Result<AccountLabel, Outcome> {
        let named = handles
            .iter()
            .take(LABEL_HANDLES)
            .find(|handle| {
                identity::resolve_handle(&self.world, &self.outbound, handle)
                    .is_ok_and(|resolved| resolved == did)
            })
            .and_then(|handle| AccountLabel::new(handle));
        named
            .or_else(|| AccountLabel::new(did))
            .ok_or(Outcome::Refused)
    }

    /// Exchanges `code` for the session `flight` started, and checks what
    /// came back: the token pair, its subject and its scope.
    fn exchange(&self, flight: &Flight, code: &str) -> Result<Tokens, Outcome> {
        let fields = [
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", flight.redirect_uri.as_str()),
            ("client_id", flight.client_id.as_str()),
            ("code_verifier", flight.verifier.expose()),
        ];
        let answered = proved_post(
            &self.outbound,
            &flight.token,
            &fields,
            &flight.key,
            flight.nonce.clone(),
        )?;
        // The session's first refresh starts from the nonce the server
        // issued last to its key.
        if let Some(nonce) = &answered.nonce {
            self.nonces.keep(
                flight.token.origin.as_str(),
                &flight.key.thumbprint(),
                nonce,
            );
        }
        let answer = json_object(&answered.response).map_err(|outcome| match outcome {
            Outcome::NotFound => Outcome::Refused,
            other => other,
        })?;
        tokens::checked(&answer, &flight.scope)
    }
}

impl CredentialAdapter for Atproto {
    fn kind(&self) -> ProviderKind {
        ProviderKind::new(KIND).unwrap_or_else(|| unreachable!("atproto is a provider kind"))
    }

    fn begin_sign_in(&self, request: &SignInRequest) -> Result<SignInStart, Outcome> {
        let redirect_uri = client::return_route(&request.return_to).ok_or(Outcome::Internal)?;
        if !is_token(&request.state, 256) {
            return Err(Outcome::Internal);
        }
        if self.flights.waits(&request.state) {
            return Err(Outcome::Conflict);
        }
        // Signing in again names the account the material holds, and
        // nothing the sponsor typed.
        let named = match &request.again {
            Some(again) => Material::decode(again, self.world.policy)
                .ok_or(Outcome::Refused)?
                .subject
                .clone(),
            None => request.hint.clone(),
        };
        let (authserver, expected) = self.destination(&named)?;
        let endpoints = server::endpoints(&self.outbound, &authserver)?;
        let verifier = Secret::new(random_token()?);
        let challenge = base64url(&sha256(verifier.expose().as_bytes()));
        let key = DpopKey::generate()?;
        let scope = scope_for(&request.permissions);
        let client_id = client::client_id(redirect_uri, &scope);
        let fields = [
            ("client_id", client_id.as_str()),
            ("redirect_uri", redirect_uri),
            ("response_type", "code"),
            ("scope", scope.as_str()),
            ("state", request.state.as_str()),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
        ];
        let pushed = proved_post(&self.outbound, &endpoints.pushed, &fields, &key, None)?;
        if pushed.response.status != 201 {
            return Err(status_outcome(pushed.response.status));
        }
        let parked: Value =
            serde_json::from_slice(&pushed.response.body).map_err(|_| Outcome::Refused)?;
        let request_uri = parked["request_uri"]
            .as_str()
            .filter(|uri| is_token(uri, 1024))
            .ok_or(Outcome::Refused)?
            .to_owned();
        self.flights.park(
            &request.state,
            Flight {
                started: Instant::now(),
                verifier,
                key,
                authserver,
                token: endpoints.token,
                client_id: client_id.clone(),
                redirect_uri: redirect_uri.to_owned(),
                scope: scope.clone(),
                expected,
                nonce: pushed.nonce,
            },
        )?;
        Ok(SignInStart {
            url: format!(
                "{}?client_id={}&request_uri={}",
                endpoints.authorize.url(),
                percent_encode(&client_id),
                percent_encode(&request_uri)
            ),
        })
    }

    fn complete_sign_in(
        &self,
        request: &SignInRequest,
        returned: &SignInReturn,
    ) -> Result<SignedIn, Outcome> {
        if returned.get("state") != Some(request.state.as_str()) {
            return Err(Outcome::Refused);
        }
        let flight = self.flights.take(&request.state).ok_or(Outcome::Refused)?;
        if returned.get("error").is_some() {
            return Err(Outcome::Refused);
        }
        let issuer = returned
            .get("iss")
            .and_then(|iss| self.world.origin(iss))
            .ok_or(Outcome::Refused)?;
        if issuer != flight.authserver {
            return Err(Outcome::Refused);
        }
        let code = returned
            .get("code")
            .filter(|code| is_token(code, TOKEN_LIMIT))
            .ok_or(Outcome::Refused)?;
        let tokens = self.exchange(&flight, code)?;
        let subject = tokens.subject.clone();
        if let Some((did, _)) = &flight.expected
            && *did != subject
        {
            return Err(Outcome::Refused);
        }
        let document = identity::document(&self.world, &self.outbound, &subject)?;
        if let Some((_, pds)) = &flight.expected
            && *pds != document.pds
        {
            return Err(Outcome::Refused);
        }
        let named = identity::authorization_server(&self.world, &self.outbound, &document.pds)?;
        if named != flight.authserver {
            return Err(Outcome::Refused);
        }
        let account = AccountKey::new(&subject).ok_or(Outcome::Refused)?;
        let label = self.label(&subject, &document.handles)?;
        let permissions = tokens.granted.clone();
        let profile = if request.again.is_some() {
            AccountProfile::default()
        } else {
            identity::profile(&self.outbound, &subject, &document.pds)
        };
        let material = Material {
            subject,
            access: tokens.access,
            access_expires_at: now_secs()
                .saturating_add(i64::try_from(tokens.expires_in).unwrap_or(i64::MAX)),
            refresh: tokens.refresh,
            pds: document.pds,
            authserver: flight.authserver,
            client_id: flight.client_id,
            key: flight.key,
            permissions: permissions.clone(),
        };
        Ok(SignedIn {
            account,
            label,
            permissions,
            material: material.encode(),
            profile,
        })
    }

    fn cancel_sign_in(&self, request: &SignInRequest) {
        drop(self.flights.take(&request.state));
    }

    fn authorize(
        &self,
        slot: &dyn MaterialSlot,
        ask: &AuthorizationAsk,
    ) -> Result<Authorization, Explained> {
        proofs::mint(&self.renewer(), &self.outbound, &self.nonces, slot, ask)
    }

    fn home(&self, slot: &dyn MaterialSlot) -> Result<Address, Outcome> {
        let material =
            Material::decode(&slot.read()?, self.world.policy).ok_or(Outcome::Refused)?;
        Ok(material.pds)
    }

    fn renew(&self, slot: &dyn MaterialSlot) -> Result<Freshness, Outcome> {
        self.renewer().renew(slot)
    }

    fn revoke(&self, slot: &dyn MaterialSlot) -> Result<Revocation, Outcome> {
        self.renewer().revoke(slot)
    }
}
