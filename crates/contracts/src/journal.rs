//! The Journal noun.
//!
//! The journal is attribution, not truth: the state file is truth. Every entry
//! records a sequence number, an id, a time, who initiated it (a mind through
//! its attachment, the sponsor, or the lobby itself), the companion, and when
//! relevant the connection, service and tool, the kind (invoked, settled,
//! command) and the outcome. By construction an entry holds no content and no
//! secrets.

/// One attributed journal line.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct JournalEntry {}
