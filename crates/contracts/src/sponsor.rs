//! The Sponsor noun.
//!
//! The sponsor is the person at the keyboard who owns the companions: they sign
//! in once per account, turn connections on and off, revoke in one click and
//! read the journal. The lobby trusts the sponsor through same-machine trust on
//! the manager page; a sponsor never attaches over MCP.

/// The sponsor who owns a companion and initiates operator commands.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Sponsor {}
