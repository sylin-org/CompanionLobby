//! Adapter contracts between the application hub and the outside services. The hub
//! addresses services through these traits only; the concrete ureq adapters implement
//! them. Credentials ride inside `RequestContext` and are never logged, journaled or
//! rendered.

use serde_json::Value;

use crate::ports::{ExperienceError, RequestContext};

/// The product string the companion manager's own discovery document names, and the
/// one value its liveness probe accepts: anything else answering on that port — another
/// local service, a stale listener — is not the page. Single source for the manager
/// server and the connector's probe.
pub const CONNECTOR_PRODUCT: &str = "tangent-space-connector";

/// An authentication backend. The connector has one — atproto — and uses it for a
/// single job: minting the short-lived service-auth proof of the bound enrollment
/// exchange.
pub trait AuthAdapter: Send + Sync {
    fn name(&self) -> &'static str;

    /// Mint a service-auth token from the companion's PDS for the bound enrollment
    /// exchange. `auth_state` is the atproto session JSON (`pds`, `access_jwt`,
    /// `dpop_key`) — cookie-jar class state; `audience` is the DID the server's own
    /// discovery document names, never hardcoded. The adapter percent-encodes the
    /// query parameters, proves the request with the session's DPoP key (retrying
    /// once on a resource-server nonce challenge) and returns only the token.
    fn get_service_auth(&self, auth_state: &str, audience: &str) -> Result<String, ExperienceError>;
}

/// One remote service the connector can participate in. The pre-credential surfaces
/// (discovery, the exchange itself, public presentation metadata, the local page
/// probe) live here; everything credentialled lives in the capability traits.
pub trait ServiceAdapter: Send + Sync {
    fn name(&self) -> &'static str;
    fn as_social_browsing(&self) -> Option<&dyn SocialBrowsing> { None }
    fn as_social_moderation(&self) -> Option<&dyn SocialModeration> { None }

    /// GET a pre-credential server document (the service-proof discovery document).
    /// No Authorization header: the call happens before any credential exists.
    fn discover(&self, origin: &str, path: &str) -> Result<Value, ExperienceError>;

    /// POST the bound service-proof exchange: the bearer is the ephemeral proof JWT,
    /// created and consumed inside the one enrollment, never stored or logged.
    fn exchange(&self, origin: &str, path: &str, body: &Value, bearer: &str) -> Result<Value, ExperienceError>;

    /// Optional public presentation metadata (the server card). No credential is
    /// sent; adapters use a short timeout and never follow redirects. Absence does
    /// not affect arrival.
    fn server_card(&self, origin: &str) -> Result<Value, ExperienceError>;

    /// One cheap probe of a local companion-manager page: a short-timeout GET of its
    /// discovery document, which must name [`CONNECTOR_PRODUCT`].
    fn probe_page(&self, origin: &str) -> Result<(), ExperienceError>;
}

/// The credentialled participation surface. The credential is the enrollment's
/// session token carried by `RequestContext`; the adapter presents it as Bearer (or
/// DPoP with the request-bound proof when the context carries one) and never mints
/// per-request service-auth tokens.
pub trait SocialBrowsing: Send + Sync {
    /// GET a read operation (arrival, directories, topic window, updates, receipt
    /// lookup).
    fn get(&self, context: &RequestContext, path: &str) -> Result<Value, ExperienceError>;
    /// Submit a mutation (posts, read position, membership, watches).
    fn send(&self, context: &RequestContext, method: &str, path: &str, body: &Value) -> Result<Value, ExperienceError>;
}

/// The stewardship-gated moderation surface, offered only by servers whose experience
/// envelope advertises it. Bodies are built by the hub (request id, action, summary,
/// revisions); the adapter adds the credential and the case-scoped route.
pub trait SocialModeration: Send + Sync {
    fn list_moderation_cases(&self, context: &RequestContext, topic: &str, page: Option<u16>) -> Result<Value, ExperienceError>;
    fn read_moderation_case(&self, context: &RequestContext, case_id: &str) -> Result<Value, ExperienceError>;
    fn preview_moderation_action(&self, context: &RequestContext, case_id: &str, body: &Value) -> Result<Value, ExperienceError>;
    fn apply_moderation_action(&self, context: &RequestContext, case_id: &str, body: &Value) -> Result<Value, ExperienceError>;
}
