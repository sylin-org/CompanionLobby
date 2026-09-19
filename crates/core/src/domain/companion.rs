//! Local companion, enrollment and context bindings. Handles are routing state, never
//! credentials: an companion is a locally minted participant persona, an enrollment is the
//! credential + server binding for one companion at one origin, and a context binds one
//! enrollment to one caller for one canonical origin.

use serde::{Deserialize, Serialize};

/// The local caller companion. stdio v1 admits exactly one operator-approved caller per
/// process; the field exists so a later daemon does not silently merge principals. For the
/// MCP intake the caller is `mcp:{clientInfo.name}`; the CLI and operator intakes are
/// `cli` and `operator`. Attribution and feed labeling only — companion resolution is the
/// same behavior for every caller.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CallerId(pub String);

impl CallerId {
    /// The `clientInfo.name` this caller was admitted under, when it is an MCP caller.
    /// Used for feed attribution ("model (via {client})"), never for a domain decision.
    pub fn mcp_client_name(&self) -> Option<&str> {
        self.0.strip_prefix("mcp:")
    }
}


/// One local companion: the persona the connector acts as. Minted locally (GUIDv7,
/// immutable); binding it to an atproto DID is a later wave, so `bound_did` is normally
/// `None`. The handle is unique within the connector.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Companion {
    /// Connector-minted GUIDv7 (32 hex chars). Never formatted as a DID.
    pub local_id: String,
    /// Operator-chosen handle, 2..=253 characters, unique (case-insensitive) within the
    /// connector. Used for moniker selection.
    pub handle: String,
    pub display_name: Option<String>,
    /// The atproto DID once a binding is verified; `None` until the account is bound.
    #[serde(default)]
    pub bound_did: Option<String>,
    pub created_at: i64,
}

/// Companion handle discipline: 2..=253 characters, no whitespace, no separators.
pub fn valid_handle(handle: &str) -> bool {
    let trimmed = handle.trim();
    (2..=253).contains(&trimmed.chars().count())
        && trimmed == handle
        && !handle.chars().any(|c| c.is_whitespace() || c == ':')
}

/// One enrollment: the session + server binding for one companion at one origin, made
/// by the account-bound proof exchange — the only way a companion enrolls. Verified at
/// enrollment time against the server's own companion response. The bearer session itself lives in the store's per-enrollment
/// session map — deliberately NOT on this struct, so cloned entries never carry it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Enrollment {
    /// Stable local handle for this enrollment, e.g. `cmp_lumen`; also its session key.
    pub enrollment_id: String,
    /// The owning companion's `local_id`.
    #[serde(default)]
    pub local_id: String,
    /// Operator-chosen short name used for selection matching.
    pub name: String,
    /// Canonical server origin this enrollment is bound to.
    pub origin: String,
    /// The server's canonical participant reference (its GUIDv7 id).
    #[serde(default)]
    pub participant_ref: String,
    /// The participant's atproto DID when the server reports one; optional since W2.
    #[serde(default)]
    pub did: Option<String>,
    /// Display name and handle as the server reports them (presentation only).
    pub display_name: Option<String>,
    pub handle: Option<String>,
    pub enrolled_at: i64,
    /// Whether the background checker runs for this enrollment while the process lives.
    pub auto_check: bool,
}

impl Enrollment {
    /// Moniker matching mirrors the server's companion selection: exact participant
    /// reference or DID, exact handle (case-insensitive, one optional leading @), or the
    /// local name. Never a display-name guess.
    pub fn matches(&self, moniker: &str) -> bool {
        let trimmed = moniker.trim();
        if trimmed.is_empty() || trimmed.len() > 253 {
            return false;
        }
        if trimmed == self.name || trimmed == self.enrollment_id || trimmed == self.participant_ref {
            return true;
        }
        if let Some(did) = &self.did {
            if trimmed == did {
                return true;
            }
        }
        let Some(handle) = &self.handle else { return false };
        let supplied = trimmed.strip_prefix('@').unwrap_or(trimmed);
        supplied.eq_ignore_ascii_case(handle)
    }
}

/// The atproto session one companion holds after the operator binds an account through
/// atproto OAuth (the `/bind` route): the PDS-issued bearer (`accessJwt`, cookie-jar
/// posture — the same exposure class as the per-enrollment sessions) plus where to
/// reach the PDS again, and the refresh token and DPoP key that let the connector renew
/// it silently. Session state, never a vault secret.
#[derive(Clone, Serialize, Deserialize)]
pub struct AccountSession {
    /// The bound account's DID (`did:plc:…`); mirrors the companion's `bound_did`.
    pub did: String,
    /// The handle the PDS confirmed (canonical form, no leading `@`).
    pub handle: String,
    /// The PDS access token used for `getServiceAuth`. Session state, not a vault secret.
    pub access_jwt: String,
    /// The OAuth refresh token.
    #[serde(default)]
    pub refresh_jwt: Option<String>,
    /// Canonical PDS origin for follow-up service-auth requests.
    pub pds: String,
    /// The authorization server origin that issued the OAuth tokens, for silent refresh.
    #[serde(default)]
    pub authserver: Option<String>,
    /// The OAuth client id this session was authorized under (new binds: the
    /// scope-declaring localhost form; absent for pre-scope-era sessions, which
    /// refresh under the bare localhost origin). Routing state, not a secret.
    #[serde(default)]
    pub client_id: Option<String>,
    /// The session's DPoP ES256 private key, base64url SEC1 bytes — required material
    /// for the refresh request.
    #[serde(default)]
    pub dpop_key: Option<String>,
    /// The service classes this credential may establish sessions for (the operator's
    /// per-credential grant). Agents never see credentials — only session identifiers —
    /// and discover this boundary only when a connect is honestly refused. Grants gate
    /// establishment only, never mid-session use. Older state without the field grants
    /// the Tangent service.
    #[serde(default = "default_services")]
    pub services: Vec<String>,
    /// Epoch milliseconds of the sign-in that produced this session.
    pub obtained_at: i64,
}

/// The grant a credential carries when the operator has not chosen one: the Tangent
/// service, which is the connector's reason to exist.
fn default_services() -> Vec<String> {
    vec!["tangent".to_string()]
}

/// Redacted by hand: a derived Debug would print the access token, refresh token
/// and DPoP key into any debug log. Only routing facts render; the secrets are named,
/// never shown.
impl std::fmt::Debug for AccountSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AccountSession")
            .field("did", &self.did)
            .field("handle", &self.handle)
            .field("access_jwt", &"[redacted]")
            .field("refresh_jwt", &self.refresh_jwt.as_ref().map(|_| "[redacted]"))
            .field("pds", &self.pds)
            .field("authserver", &self.authserver)
            .field("client_id", &self.client_id)
            .field("dpop_key", &self.dpop_key.as_ref().map(|_| "[redacted]"))
            .field("services", &self.services)
            .field("obtained_at", &self.obtained_at)
            .finish()
    }
}

/// A participation context: caller + enrollment + canonical origin + credential binding.
/// The acting companion rides the enrollment; it is never chosen here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Context {
    /// The labeled session handle, e.g. `tangent_9f01ab` — the service moniker for
    /// the eyes, a unique short id underneath. Dispatch is exact-token lookup; the
    /// label is never parsed.
    pub context_id: String,
    /// The service moniker this session was minted through ("tangent", "bluesky").
    #[serde(default)]
    pub service: String,
    pub caller: CallerId,
    pub enrollment_id: String,
    pub origin: String,
    /// The enrollment's participant reference at enrollment time (presentation only).
    #[serde(default)]
    pub participant_ref: String,
    pub created_at: i64,
    pub last_used_at: i64,
}

impl Context {
    /// A context is usable only for the exact caller and enrollment it was issued to.
    pub fn belongs_to(&self, caller: &CallerId, enrollment_id: &str) -> bool {
        &self.caller == caller && self.enrollment_id == enrollment_id
    }
}
