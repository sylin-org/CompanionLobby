//! The single outcome vocabulary.
//!
//! Every adapter, the daemon and the MCP projection answer with one of these
//! outcomes. No variant can carry provider text: messages a mind or a sponsor
//! reads are authored by the lobby.

/// How a call ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Outcome {
    /// The call completed.
    Done,
    /// The service accepted the request and will complete it later.
    Accepted,
    /// The request is waiting on the service.
    Pending,
    /// The service or the lobby refused the request definitively.
    Refused,
    /// The target does not exist or is not visible to this companion.
    NotFound,
    /// The request conflicts with the target's state.
    Conflict,
    /// The arguments do not satisfy the tool's schema or invariants.
    InvalidInput,
    /// The credential needs the sponsor to sign in again.
    NeedsSignIn,
    /// The credential lacks a permission the call needs.
    NeedsPermission,
    /// The connection or credential was revoked.
    Revoked,
    /// The service is unreachable or failing; retrying later may help.
    TemporarilyUnavailable,
    /// The service asked the lobby to slow down.
    RateLimited,
    /// A write may or may not have landed; read before retrying. Its wire name is `outcome_unknown`.
    Unknown,
    /// The adapter does not offer this operation.
    Unsupported,
    /// The lobby itself failed.
    Internal,
}

/// Whether a write may be repeated after an unknown outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Retry {
    /// Repeating the call cannot duplicate its effect.
    Safe,
    /// Read the target first; repeat only if the effect is absent.
    ReadFirst,
    /// Never repeat automatically.
    Never,
}
