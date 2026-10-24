//! The Service noun and the service adapter contract.
//!
//! A service adapter speaks one service in that service's own words. It
//! declares which provider kinds and permissions it accepts and whether a
//! companion holds one connection or many (one per address), establishes a
//! connection by asking the daemon's [`Authority`] for authorizations, declares
//! a catalog of typed tools, says which tools the connection's authority makes
//! available, and performs calls. A service adapter may also push events into
//! the daemon through [`Ingress`].

use crate::{
    Address, Authorization, AuthorizationAsk, Outcome, SealedIssued, ToolArguments, ToolDescriptor,
    ToolName, ToolResult,
};

/// A service name: `[a-z][a-z0-9]{1,15}`, so that `<service>_<tool>` fits in 64 characters.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct ServiceName {}

/// The accepted credential kinds with their permissions, the addressing mode and the push flag.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ServiceDescription {}

/// Facts a connection learned about what its authority allows.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AuthorityFacts {}

/// What a call is pinned to: the attachment, the connection, its sealed issued data and the ticket.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CallContext {}

/// One event a service pushed for a connection.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct IngressItem {}

/// The daemon's broker: mints authorizations from the connection's credential.
pub trait Authority: Send + Sync {
    /// Asks the connection's credential adapter for one authorization.
    fn authorize(&self, ask: AuthorizationAsk) -> Result<Authorization, Outcome>;
}

/// The daemon's intake for events a service pushes.
pub trait Ingress: Send + Sync {
    /// Hands one pushed event to the daemon's event pipeline.
    fn push(&self, item: IngressItem) -> Result<(), Outcome>;
}

/// The contract every service adapter implements.
///
/// Calls are synchronous and may block on the network; the daemon runs them on
/// a blocking thread with no lock held.
pub trait ServiceAdapter: Send + Sync {
    /// The service's name, which prefixes every tool it declares.
    fn name(&self) -> ServiceName;

    /// What the service accepts and how it is addressed.
    fn describe(&self) -> ServiceDescription;

    /// Establishes a connection at `address`, minting what it needs through `authority`.
    fn establish(
        &self,
        address: &Address,
        authority: &dyn Authority,
    ) -> Result<SealedIssued, Outcome>;

    /// Every tool this adapter can offer.
    fn catalog(&self) -> Vec<ToolDescriptor>;

    /// The tools a connection with these authority facts may call.
    fn available(&self, facts: &AuthorityFacts) -> Vec<ToolName>;

    /// Performs one tool call.
    fn call(
        &self,
        tool: &ToolName,
        arguments: ToolArguments,
        pinned: CallContext,
    ) -> Result<ToolResult, Outcome>;

    /// Opens a push stream for a connection, for services that push events.
    fn open_stream(&self, _pinned: CallContext, _ingress: &dyn Ingress) -> Result<(), Outcome> {
        Err(Outcome::Unsupported)
    }
}
