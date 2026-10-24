//! The Mind noun, as the lobby sees it: an attachment.
//!
//! A mind is whatever model or agent an MCP host runs. The lobby knows a mind
//! only through its attachment: one attachment is one companion, exists only
//! while the daemon runs, and is proven by the per-install attach secret. The
//! identity of an attachment is minted by the daemon when the attach shim
//! presents the secret; it is independent of the MCP transport's own
//! bookkeeping, and whatever a host says about itself (its client name and
//! version) is attribution, never authority.

use std::fmt;

use crate::CompanionId;
use crate::companion::parse_hex_id;

/// Identifier of a live attachment: a UUIDv7 rendered as 32 lowercase hex digits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AttachmentId(u128);

impl AttachmentId {
    /// Wraps the 128 bits of a UUID the daemon minted.
    pub const fn from_u128(bits: u128) -> Self {
        Self(bits)
    }

    /// Parses exactly 32 lowercase hex digits.
    pub fn parse(text: &str) -> Option<Self> {
        parse_hex_id(text).map(Self)
    }
}

impl fmt::Display for AttachmentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:032x}", self.0)
    }
}

/// A mind attached as one companion. Runtime only; never persisted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    /// The identifier the daemon minted for this attachment.
    pub id: AttachmentId,
    /// The one companion this attachment acts as.
    pub companion: CompanionId,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachment_identifiers_round_trip_as_32_hex_digits() {
        let id = AttachmentId::from_u128(42);
        assert_eq!(id.to_string(), format!("{:032x}", 42));
        assert_eq!(AttachmentId::parse(&id.to_string()), Some(id));
        assert_eq!(AttachmentId::parse("42"), None);
    }
}
