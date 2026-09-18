//! Durable mutation intents. A mutation's request id, actor, origin, operation, target and
//! exact payload are persisted before the request is sent; retries reuse the same tuple, and a
//! changed payload under the same key is a conflict, never a silent second action.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Receipt {
    pub request_id: String,
    pub context_id: String,
    pub enrollment_id: String,
    pub origin: String,
    pub operation: String,
    pub target_ref: String,
    pub payload: serde_json::Value,
    pub registered_at: i64,
    #[serde(default)]
    pub settled: bool,
    #[serde(default)]
    pub state: Option<String>,
}

/// Connector-side request-id discipline, matching the server registry: 1–128 ASCII letters,
/// digits, hyphens or underscores.
pub fn valid_request_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_ids_are_bounded_ascii_identifiers() {
        assert!(valid_request_id("reply-84"));
        assert!(valid_request_id("a"));
        assert!(!valid_request_id(""));
        assert!(!valid_request_id("has space"));
        assert!(!valid_request_id("über"));
        assert!(!valid_request_id(&"x".repeat(129)));
    }
}
