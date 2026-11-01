//! The Service noun and the service adapter contract.
//!
//! A service adapter speaks one service in that service's own words. It
//! declares which provider kinds it accepts and which addresses it may reach,
//! establishes a connection by asking the daemon's [`Authority`] for the
//! authorization the service wants, declares a catalog of typed tools, and
//! performs calls with what the service issued for the connection. It never
//! holds credential material: whatever it needs to prove, the credential
//! adapter mints for it on demand.

use crate::{
    Address, AddressPolicy, Authorization, AuthorizationAsk, Explained, IssuedSlot, ProviderKind,
    SealedIssued, ToolArguments, ToolDescriptor, ToolName, ToolResult,
};

/// Whether `text` is a short lowercase name: `[a-z][a-z0-9]{1,15}`.
pub(crate) fn is_short_name(text: &str) -> bool {
    let bytes = text.as_bytes();
    (2..=ServiceName::MAX_BYTES).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

/// A service name: `[a-z][a-z0-9]{1,15}`, so that `<service>_<tool>` fits in 64 characters.
///
/// It is the one rule for service names and for the prefix of every tool a
/// mind sees; it holds no underscore, so the prefix always splits cleanly.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ServiceName(String);

impl ServiceName {
    /// The longest service name, in bytes.
    pub const MAX_BYTES: usize = 16;

    /// Checks `text` against the service name rule.
    pub fn new(text: &str) -> Option<Self> {
        is_short_name(text).then(|| Self(text.to_owned()))
    }

    /// The name as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ServiceName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a service adapter accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceDescription {
    /// The provider kinds whose credentials can establish a connection.
    pub accepts: Vec<ProviderKind>,
    /// Which addresses a connection may name.
    pub addresses: AddressPolicy,
    /// Whether the service lives where each account lives, on the
    /// account's own server: a connection to it names no address of its
    /// own, reaches the one the credential's adapter names as the
    /// account's [`home`](crate::CredentialAdapter::home), which
    /// [`Self::addresses`] must admit, and a credential holds at most one.
    pub home: bool,
    /// What a credential of an accepted kind must have been granted to
    /// establish a connection, in that provider's own words. The lobby
    /// asks the provider for these whenever it signs an account of that
    /// kind in.
    pub permissions: Vec<String>,
}

/// How a service knows the companion through one connection, as what the
/// service issued for it says, so a mind meets there the name it was told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownAs {
    /// The name the service shows for the companion there, as the service
    /// wrote it; a mind reads it only quoted.
    pub name: Option<String>,
    /// The reference the service gave the companion there, which its own
    /// answers name the companion by, as the service wrote it; a mind
    /// reads it only quoted.
    pub reference: Option<String>,
    /// What the service calls such a reference, in its own words, such as
    /// `member`.
    pub reference_is: &'static str,
    /// What the connection's grants allow there, in the service's words,
    /// as a clause that follows "lets it", such as `read and post`. A
    /// connection that cannot write what others read never says it can.
    pub allows: &'static str,
}

/// The daemon's broker for one connection being established: it mints
/// authorizations from the connection's credential, through that
/// credential's own adapter.
pub trait Authority: Send + Sync {
    /// Asks the credential adapter for one authorization in the shape `ask`
    /// names, and hands back exactly what it minted.
    fn authorize(&self, ask: &AuthorizationAsk) -> Result<Authorization, Explained>;
}

/// The contract every service adapter implements.
///
/// Calls are synchronous and may block on the network; the daemon runs them on
/// a blocking thread with no lock held. A read that fails answers an outcome
/// that says so; a write whose effect is uncertain answers
/// [`Outcome::Unknown`](crate::Outcome::Unknown), never a success and never a
/// plain failure. A definite refusal is explained in the service's words:
/// the fact the service stated and the next step.
pub trait ServiceAdapter: Send + Sync {
    /// The service's name, which prefixes every tool it declares.
    fn name(&self) -> ServiceName;

    /// What the service accepts.
    fn describe(&self) -> ServiceDescription;

    /// The address of the place the server reached at `address` says it is;
    /// `address` when it says nothing. A mind may name a place however it was
    /// told, and a connection is to the place itself, so an adapter sends what
    /// it establishes with only there. It may block on the network, and never
    /// fails: what cannot be read is left for establishing to say.
    fn goes_by(&self, address: &Address) -> Address {
        address.clone()
    }

    /// Establishes a connection at `address`, asking `authority` for what
    /// the service wants, and returns what the service issued for it.
    fn establish(
        &self,
        address: &Address,
        authority: &dyn Authority,
    ) -> Result<SealedIssued, Explained>;

    /// Every tool this adapter offers, named without the service prefix.
    /// The first is where a mind starts.
    fn catalog(&self) -> Vec<ToolDescriptor>;

    /// The tools of the catalog that what the service issued for one
    /// connection allows; every one unless the adapter says otherwise.
    fn available(&self, issued: &SealedIssued) -> Vec<ToolName> {
        let _ = issued;
        self.catalog().into_iter().map(|tool| tool.name).collect()
    }

    /// How the service knows the companion through the connection whose
    /// issued data is `issued`, and what that connection's grants allow
    /// there; none when the adapter cannot read what was issued.
    fn known_as(&self, issued: &SealedIssued) -> Option<KnownAs>;

    /// Performs one tool call through the connection at `address`, whose
    /// issued data `slot` guards.
    fn call(
        &self,
        tool: &ToolName,
        arguments: &ToolArguments,
        address: &Address,
        slot: &dyn IssuedSlot,
    ) -> Result<ToolResult, Explained>;
}

#[cfg(test)]
mod tests {
    use super::ServiceName;

    #[test]
    fn service_names_are_short_lowercase_words() {
        for good in ["board", "ab", "x9", "abcdefghijklmnop"] {
            assert!(ServiceName::new(good).is_some(), "{good}");
        }
        for bad in [
            "",
            "a",
            "Board",
            "9board",
            "bo_ard",
            "bo-ard",
            "abcdefghijklmnopq",
            "bóard",
        ] {
            assert!(ServiceName::new(bad).is_none(), "{bad:?}");
        }
    }
}
