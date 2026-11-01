//! The seven nouns of Companion Lobby and the two adapter contracts.
//!
//! This crate is pure vocabulary: it performs no I/O, reads no clock and no
//! environment, and knows nothing about async runtimes. Every other crate in
//! the workspace speaks through these types.
//!
//! The seven nouns, and the type that carries each here:
//!
//! * Sponsor, [`Sponsor`]: the person who owns the companions and decides what they may reach.
//! * Companion, [`Companion`]: a named identity a mind attaches as.
//! * Mind, [`Attachment`]: a mind as the lobby sees it; one attachment is one companion.
//! * Credential, [`Credential`]: an account the sponsor signed in to for a companion.
//! * Service, [`ServiceName`]: something the lobby reaches through a service adapter.
//! * Connection, [`Connection`]: a companion's use of a credential to reach a service at an address.
//! * Journal, [`JournalEntry`]: one attributed line of what happened, with no content and no secrets.
//!
//! The two contracts are [`CredentialAdapter`] and [`ServiceAdapter`]. Both are
//! synchronous and `Send + Sync`; the daemon runs them on blocking threads. The
//! daemon implements [`MaterialSlot`], [`IssuedSlot`] and [`Authority`], so
//! adapters never touch the store directly: a service adapter asks the
//! [`Authority`] for an [`Authorization`] in the shape it needs, and the
//! credential adapter mints it from material only it decodes.
//!
//! A type enters this crate only when both synthetic adapters in the test kit
//! need it in the same change.

mod address;
mod authorization;
mod companion;
mod connection;
mod credential;
mod journal;
mod material;
mod mind;
mod outcome;
mod service;
mod sponsor;
mod tool;

pub use address::{Address, AddressPolicy};
pub use authorization::{Authorization, AuthorizationAsk, Secret};
pub use companion::{Companion, CompanionId, DisplayName, Moniker, NameProblem};
pub use connection::{Connection, ConnectionId, ConnectionState, Label};
pub use credential::{
    AccountKey, AccountLabel, AccountProfile, Credential, CredentialAdapter, CredentialId,
    CredentialState, Freshness, ProviderKind, Revocation, SignInRequest, SignInReturn, SignInStart,
    SignedIn,
};
pub use journal::{Initiator, JournalEntry, JournalKind, JournalNames};
pub use material::{IssuedSlot, MaterialSlot, MaterialUpdate, SealedIssued, SealedMaterial};
pub use mind::{Attachment, AttachmentId};
pub use outcome::{Explained, Outcome, Retry};
pub use service::{Authority, KnownAs, ServiceAdapter, ServiceDescription, ServiceName};
pub use sponsor::Sponsor;
pub use tool::{Effect, Passage, ToolArguments, ToolDescriptor, ToolName, ToolResult};

#[cfg(test)]
mod tests {
    use super::*;

    fn shareable<T: ?Sized + Send + Sync>() {}

    #[test]
    fn the_contracts_are_object_safe_and_shareable_across_threads() {
        shareable::<dyn CredentialAdapter>();
        shareable::<dyn ServiceAdapter>();
        shareable::<dyn MaterialSlot>();
        shareable::<dyn IssuedSlot>();
        shareable::<dyn Authority>();
    }
}
