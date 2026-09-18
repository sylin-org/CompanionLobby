//! Attention records and their lifecycle. The server supplies canonical attention facts; the
//! connector owns delivery state. Item companion is the server's stable item reference, so
//! repeated mention delivery coalesces and unchanged digests never mint new records.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionState {
    /// Seen in a digest, not yet eligible for delivery.
    Pending,
    /// Eligible; waiting for the next supported delivery opportunity.
    Queued,
    /// Included in a delivered tool response or adapter dispatch. Delivery is not evidence a
    /// model read or acted; the record stays inspectable until the server resynchronizes it away.
    Delivered,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttentionRecord {
    /// The server item reference (stable per source post), the deduplication companion.
    pub id: String,
    pub enrollment_id: String,
    pub kind: String,
    pub actor_ref: String,
    pub actor_name: Option<String>,
    pub scope_ref: String,
    pub source_ref: String,
    pub relationship: Option<String>,
    pub excerpt: String,
    pub source_revision: String,
    pub state: AttentionState,
    pub first_seen_at: i64,
    pub last_seen_at: i64,
    pub revision_seen: String,
}

impl AttentionRecord {
    /// Directed attention (mentions and direct replies) is what can request a turn; watched
    /// activity is mere new content and never schedules anything.
    pub fn is_directed(&self) -> bool {
        matches!(self.relationship.as_deref(), Some("addressed_to_you") | Some("replies_to_you"))
    }

    /// Update from a fresh digest occurrence without regressing delivery state.
    pub fn refresh(&mut self, actor_name: Option<String>, excerpt: String, revision: String, now: i64) {
        self.actor_name = actor_name;
        self.excerpt = excerpt;
        self.revision_seen = revision;
        self.last_seen_at = now;
    }
}

/// The maximum retained attention records per companion; oldest delivered items are evicted.
pub const ATTENTION_RECORD_LIMIT: usize = 200;
