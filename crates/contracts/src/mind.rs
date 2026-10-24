//! The Mind noun, as the lobby sees it: an attachment.
//!
//! A mind is whatever model or agent an MCP host runs. The lobby knows a mind
//! only through its attachment: one attachment is one companion, exists only
//! while the daemon runs, and is proven by the per-install attach secret. The
//! identity of an attachment is independent of the MCP transport's own
//! bookkeeping.

/// Identifier of a live attachment: a UUIDv7 rendered as 32 lowercase hex digits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub struct AttachmentId {}

/// A mind attached as one companion. Runtime only; never persisted.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Attachment {}
