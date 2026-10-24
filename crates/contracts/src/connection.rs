//! The Connection noun.
//!
//! A connection is the sponsor's decision that a companion may reach a service
//! at an address using one credential, under a label the sponsor chose. It
//! exists only because the service adapter established it, and the only data
//! it holds is what the service issued, kept sealed beside the record. Its
//! revision advances on every change, so a call pinned to one revision can
//! tell that the connection changed while it ran.

use std::fmt;

use crate::companion::parse_hex_id;
use crate::{Address, CompanionId, CredentialId, Moniker, NameProblem, ServiceName};

/// Identifier of a connection: a UUIDv7 rendered as 32 lowercase hex digits.
///
/// The daemon mints identifiers; this type only carries and checks them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConnectionId(u128);

impl ConnectionId {
    /// Wraps the 128 bits of a UUID the daemon minted.
    pub const fn from_u128(bits: u128) -> Self {
        Self(bits)
    }

    /// Parses exactly 32 lowercase hex digits.
    pub fn parse(text: &str) -> Option<Self> {
        parse_hex_id(text).map(Self)
    }
}

impl fmt::Display for ConnectionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:032x}", self.0)
    }
}

/// The sponsor's name for one connection, which a mind passes as `at` to say
/// where a tool acts. It follows the moniker rule, so it is never text a
/// service wrote.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Label(String);

impl Label {
    /// Checks `text` against the label rule, which is the moniker rule.
    pub fn new(text: &str) -> Result<Self, NameProblem> {
        Moniker::new(text).map(|moniker| Self(moniker.as_str().to_owned()))
    }

    /// The label as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Label {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Whether a connection can be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConnectionState {
    /// Established; its tools are in its companion's catalog.
    Ready,
    /// The sponsor revoked it, or its credential: its tools are gone and what
    /// the service issued for it is deleted.
    Revoked,
}

/// A connection record as the state file holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    /// The immutable identifier.
    pub id: ConnectionId,
    /// The companion that reaches the service through it.
    pub companion: CompanionId,
    /// The credential it was established with.
    pub credential: CredentialId,
    /// The service it reaches.
    pub service: ServiceName,
    /// Where the service is.
    pub address: Address,
    /// The sponsor's name for it.
    pub label: Label,
    /// Whether it can be used.
    pub state: ConnectionState,
    /// Advances on every change to this connection.
    pub revision: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_identifiers_round_trip_as_32_hex_digits() {
        let id = ConnectionId::from_u128(42);
        assert_eq!(id.to_string(), format!("{:032x}", 42));
        assert_eq!(ConnectionId::parse(&id.to_string()), Some(id));
        assert_eq!(ConnectionId::parse("42"), None);
    }

    #[test]
    fn labels_follow_the_moniker_rule() {
        assert_eq!(Label::new("front").expect("label").as_str(), "front");
        assert_eq!(Label::new("Front"), Err(NameProblem::Character));
        assert_eq!(Label::new(""), Err(NameProblem::Empty));
    }
}
