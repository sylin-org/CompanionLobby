//! The Credential noun and the credential adapter contract.
//!
//! A credential record names its companion, provider kind, account key, a safe
//! label, its state (ready, needs sign-in, quarantined, revoked), a lifecycle
//! epoch, a material revision and the permissions granted. It never holds the
//! material itself: material is a sealed blob that only the adapter of its
//! provider kind can decode, reached through a [`MaterialSlot`].

use crate::{Authorization, AuthorizationAsk, MaterialSlot, Outcome};

/// Identifier of a credential: a UUIDv7 rendered as 32 lowercase hex digits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub struct CredentialId {}

/// A credential record without its material.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Credential {}

/// The provider kind a credential adapter serves, such as one sign-in protocol.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct ProviderKind {}

/// What a credential adapter tells the manager page about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CredentialDescription {}

/// The sponsor's request to sign a companion in to an account.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SignInRequest {}

/// Where the sponsor's browser goes to complete a sign-in.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SignInStart {}

/// What the provider sent back to the lobby's sign-in return route.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SignInReturn {}

/// A completed sign-in: the account the provider authenticated and its sealed material.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SignedIn {}

/// A companion portrait read from the provider, bounded in size.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Portrait {}

/// The contract every credential adapter implements.
///
/// Calls are synchronous and may block on the network; the daemon runs them on
/// a blocking thread with no lock held. An adapter refuses an ask it does not
/// support before it reads material or touches the network, never widens
/// permissions on refresh, and never returns provider text in an outcome.
pub trait CredentialAdapter: Send + Sync {
    /// The provider kind this adapter serves.
    fn kind(&self) -> ProviderKind;

    /// What the manager page shows about this provider kind.
    fn describe(&self) -> CredentialDescription;

    /// Starts a sign-in and returns where the sponsor's browser goes next.
    fn begin_sign_in(&self, request: SignInRequest) -> Result<SignInStart, Outcome>;

    /// Completes a sign-in from what the provider returned, recording the account it authenticated.
    fn complete_sign_in(&self, returned: SignInReturn) -> Result<SignedIn, Outcome>;

    /// Turns the material in `slot` into an authorization for one ask.
    fn authorize(
        &self,
        slot: &dyn MaterialSlot,
        ask: AuthorizationAsk,
    ) -> Result<Authorization, Outcome>;

    /// Revokes the credential at the provider, as far as the provider allows.
    fn revoke(&self, slot: &dyn MaterialSlot) -> Result<(), Outcome>;

    /// Reads the account's portrait, when the sponsor opted in.
    fn portrait(&self, slot: &dyn MaterialSlot) -> Result<Portrait, Outcome>;
}
