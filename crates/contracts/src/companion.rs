//! The Companion noun.
//!
//! A companion is a named identity with a moniker (an explicit charset and a
//! byte bound), a display name, a portrait reference and its sponsor. A mind
//! attaches as a companion and sees exactly what that companion may reach.

/// Identifier of a companion: a UUIDv7 rendered as 32 lowercase hex digits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub struct CompanionId {}

/// A companion record as the state file holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Companion {}
