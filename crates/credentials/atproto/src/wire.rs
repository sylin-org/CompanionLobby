//! Where the adapter reaches, and how it reads what comes back.
//!
//! Every request goes through the one outbound agent, under the world's
//! address policy. An answer is read strictly: a document is a JSON object
//! under a JSON media type, and a status other than the one the step expects
//! becomes an outcome, never provider text.

use lobby_contracts::{Address, AddressPolicy, Outcome};
use lobby_outbound::{Failure, Outbound, Request, Response};
use serde_json::Value;

use crate::codec::is_token;
use crate::dpop::DpopKey;

/// The longest DPoP nonce accepted.
const NONCE_LIMIT: usize = 1024;

/// What the adapter resolves and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct World {
    pub(crate) policy: AddressPolicy,
    pub(crate) directory: Address,
    pub(crate) resolver: Address,
    pub(crate) authserver: Address,
    /// Where did:web documents are read: `None` reads each from its own
    /// host; a fake world serves them from one origin.
    pub(crate) web_documents: Option<Address>,
}

impl World {
    /// The public DID directory, for did:plc documents.
    pub const DIRECTORY: &str = "https://plc.directory";
    /// The public handle resolver.
    pub const RESOLVER: &str = "https://public.api.bsky.app";
    /// The authorization server a sign-in without a handle goes to.
    pub const AUTHSERVER: &str = "https://bsky.social";

    /// The public network, reached over HTTPS to public addresses only.
    /// `authserver`, when given, is the authorization server a sign-in
    /// without a handle goes to instead of [`Self::AUTHSERVER`]; it must be
    /// one public HTTPS origin.
    pub fn public(authserver: Option<&str>) -> Result<Self, String> {
        let policy = AddressPolicy::PublicHttps;
        let origin = |text: &str| {
            Address::parse(text, policy)
                .ok_or_else(|| format!("{text} is not one public HTTPS origin"))
        };
        Ok(Self {
            policy,
            directory: origin(Self::DIRECTORY)?,
            resolver: origin(Self::RESOLVER)?,
            authserver: origin(authserver.map_or(Self::AUTHSERVER, str::trim))?,
            web_documents: None,
        })
    }

    /// A fake world on this machine: the directory, the handle resolver, the
    /// authorization server and every did:web document at `origin`, and the
    /// loopback address reachable.
    pub fn loopback(origin: &str) -> Option<Self> {
        let address = Address::parse(origin, AddressPolicy::LoopbackAllowed)?;
        Some(Self {
            policy: AddressPolicy::LoopbackAllowed,
            directory: address.clone(),
            resolver: address.clone(),
            authserver: address.clone(),
            web_documents: Some(address),
        })
    }

    /// `text` as an origin this world may reach.
    pub(crate) fn origin(&self, text: &str) -> Option<Address> {
        Address::parse(text, self.policy)
    }

    /// Whether a handle may be resolved from its own host's well-known
    /// document; a fake world resolves every handle through its resolver.
    pub(crate) fn reads_handle_hosts(&self) -> bool {
        self.web_documents.is_none()
    }
}

/// One URL split into the origin the agent reaches and the path it asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub(crate) origin: Address,
    pub(crate) path: String,
}

impl Endpoint {
    /// `url` as an endpoint on `origin`: it must start with the origin and a
    /// `/`, and carry no fragment.
    pub fn on(origin: &Address, url: &str) -> Option<Self> {
        let path = url.strip_prefix(origin.as_str())?;
        (path.starts_with('/') && !path.contains('#') && is_token(path, 2048)).then(|| Self {
            origin: origin.clone(),
            path: path.to_owned(),
        })
    }

    /// The whole URL.
    pub fn url(&self) -> String {
        self.origin.url(&self.path)
    }
}

/// The outcome of an answer whose status is not the one expected.
pub fn status_outcome(status: u16) -> Outcome {
    match status {
        404 => Outcome::NotFound,
        429 => Outcome::RateLimited,
        500..=599 => Outcome::TemporarilyUnavailable,
        _ => Outcome::Refused,
    }
}

/// The outcome of a request that got no answer.
pub fn failure_outcome(failure: Failure) -> Outcome {
    match failure {
        Failure::Refused => Outcome::Refused,
        Failure::Unreachable | Failure::Interrupted => Outcome::TemporarilyUnavailable,
    }
}

fn is_json_media(response: &Response) -> bool {
    response
        .header("content-type")
        .and_then(|value| value.split(';').next())
        .map(|media| media.trim().to_ascii_lowercase())
        .is_some_and(|media| media == "application/json" || media.ends_with("+json"))
}

/// The JSON object a 200 answer carries.
pub fn json_object(response: &Response) -> Result<Value, Outcome> {
    if response.status != 200 {
        return Err(status_outcome(response.status));
    }
    let body: Value = serde_json::from_slice(&response.body).map_err(|_| Outcome::Refused)?;
    (is_json_media(response) && body.is_object())
        .then_some(body)
        .ok_or(Outcome::Refused)
}

/// GETs `path` from `origin` and reads the JSON object it answers.
pub fn get_json(outbound: &Outbound, origin: &Address, path: &str) -> Result<Value, Outcome> {
    let response = outbound
        .send(origin, Request::new("GET", path))
        .map_err(failure_outcome)?;
    json_object(&response)
}

/// What a DPoP-proved form post answered, and the nonce the server issued
/// last.
pub struct Proved {
    pub(crate) response: Response,
    pub(crate) nonce: Option<String>,
}

/// POSTs `fields` to `endpoint` as a form with a DPoP proof made with `key`,
/// carrying `nonce` when the server issued one. A server that answers
/// `use_dpop_nonce` with a fresh nonce is asked once more with it.
pub fn proved_post(
    outbound: &Outbound,
    endpoint: &Endpoint,
    fields: &[(&str, &str)],
    key: &DpopKey,
    nonce: Option<String>,
) -> Result<Proved, Outcome> {
    let mut nonce = nonce;
    let url = endpoint.url();
    for attempt in 0..2 {
        let proof = key.proof("POST", &url, nonce.as_deref())?;
        let response = outbound
            .send(
                &endpoint.origin,
                Request::new("POST", &endpoint.path)
                    .header("DPoP", proof)
                    .form(fields),
            )
            .map_err(failure_outcome)?;
        let fresh = response
            .header("dpop-nonce")
            .filter(|value| is_token(value, NONCE_LIMIT))
            .map(str::to_owned);
        let asks_for_nonce = matches!(response.status, 400 | 401)
            && serde_json::from_slice::<Value>(&response.body)
                .is_ok_and(|body| body["error"] == "use_dpop_nonce");
        if fresh.is_some() {
            nonce = fresh;
        }
        if asks_for_nonce && attempt == 0 && nonce.is_some() {
            continue;
        }
        return Ok(Proved { response, nonce });
    }
    Err(Outcome::Refused)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_public_world_is_https_to_public_origins_only() {
        let world = World::public(None).expect("the public world");
        assert_eq!(world.authserver.as_str(), World::AUTHSERVER);
        assert_eq!(world.policy, AddressPolicy::PublicHttps);
        assert!(world.reads_handle_hosts());
        let chosen = World::public(Some(" https://auth.example.com ")).expect("an override");
        assert_eq!(chosen.authserver.as_str(), "https://auth.example.com");
        for refused in [
            "http://127.0.0.1:5300",
            "https://127.0.0.1",
            "https://10.0.0.1",
            "http://auth.example.com",
            "https://auth.example.com/path",
            "auth.example.com",
            "",
        ] {
            assert!(World::public(Some(refused)).is_err(), "{refused:?}");
        }
        assert!(world.origin("http://127.0.0.1:5300").is_none());
        assert!(world.origin("https://192.168.1.10").is_none());
        assert!(world.origin("https://[fd00::1]").is_none());
    }

    #[test]
    fn a_fake_world_is_one_loopback_origin() {
        let world = World::loopback("http://127.0.0.1:5300").expect("loopback");
        assert_eq!(world.directory, world.authserver);
        assert!(!world.reads_handle_hosts());
        assert!(World::loopback("https://example.com/x").is_none());
    }

    #[test]
    fn endpoints_live_on_their_origin() {
        let origin = Address::parse("https://as.test", AddressPolicy::PublicHttps).expect("origin");
        let endpoint = Endpoint::on(&origin, "https://as.test/oauth/par").expect("on the origin");
        assert_eq!(endpoint.path, "/oauth/par");
        assert_eq!(endpoint.url(), "https://as.test/oauth/par");
        for foreign in [
            "https://as.test.evil/oauth/par",
            "https://other.test/oauth/par",
            "https://as.test",
            "https://as.test/a#b",
            "https://as.test:444/oauth",
        ] {
            assert_eq!(Endpoint::on(&origin, foreign), None, "{foreign}");
        }
    }
}
