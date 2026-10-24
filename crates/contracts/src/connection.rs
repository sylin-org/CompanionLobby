//! The Connection noun.
//!
//! A connection is the sponsor's decision that a companion may reach a service
//! at an address using one credential. It carries a state (ready, needs sign-in,
//! revoked), a health fact (ok, unavailable, refused by the service, credential
//! quarantined), the service-issued sealed data, the credential epoch and the
//! issued revision it was established under, and its own revision.

/// Identifier of a connection: a UUIDv7 rendered as 32 lowercase hex digits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub struct ConnectionId {}

/// A connection record as the state file holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Connection {}
