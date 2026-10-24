//! The Sponsor noun.
//!
//! The sponsor is the person at the keyboard who owns the companions: they sign
//! in once per account, turn connections on and off, revoke in one click and
//! read the journal. The lobby trusts the sponsor through same-machine trust on
//! the manager page; a sponsor never attaches over MCP.

/// The sponsor who owns a companion and initiates operator commands.
///
/// One install serves one sponsor: the local user who runs the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub struct Sponsor {}

impl Sponsor {
    /// The local user who runs this install.
    pub const LOCAL: Self = Self {};
}
