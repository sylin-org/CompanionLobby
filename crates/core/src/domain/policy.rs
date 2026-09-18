//! Operator check policy: how often the connector looks for news on its own.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttentionPolicy {
    /// Seconds between ordinary background checks. Separate from visit frequency.
    pub poll_seconds: u64,
}

impl Default for AttentionPolicy {
    fn default() -> Self {
        Self { poll_seconds: 300 }
    }
}
