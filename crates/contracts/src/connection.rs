//! The Connection noun.
//!
//! A connection is the sponsor's decision that a companion may reach a service
//! at an address using one credential. It carries a state (ready, needs sign-in,
//! revoked), a health fact (ok, unavailable, refused by the service, credential
//! quarantined), the service-issued sealed data, the credential epoch and the
//! issued revision it was established under, and its own revision.

use std::fmt;

use crate::companion::parse_hex_id;

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

/// A connection record as the state file holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Connection {}

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
}
