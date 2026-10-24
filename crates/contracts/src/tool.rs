//! Tool descriptors and results.
//!
//! A service adapter declares its tools; the daemon projects them to a mind as
//! `<service>_<tool>` and derives the MCP hints from each tool's [`Effect`]. A
//! result carries text plus optional structured data.

use std::fmt;

use serde_json::{Map, Value};

use crate::{Retry, ServiceName};

/// A tool name as a service adapter declares it, before the service prefix.
///
/// A lowercase ASCII letter, then lowercase ASCII letters, digits or
/// underscores, at most 47 bytes, so `<service>_<tool>` fits in 64.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ToolName(String);

impl ToolName {
    /// The longest tool name, in bytes.
    pub const MAX_BYTES: usize = 64 - ServiceName::MAX_BYTES - 1;

    /// Checks `text` against the tool name rule.
    pub fn new(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        let valid = bytes.first().is_some_and(u8::is_ascii_lowercase)
            && bytes.len() <= Self::MAX_BYTES
            && bytes
                .iter()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_');
        valid.then(|| Self(text.to_owned()))
    }

    /// The name as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The name a mind sees for this tool of `service`: `<service>_<tool>`.
    pub fn projected(&self, service: &ServiceName) -> String {
        format!("{service}_{}", self.0)
    }
}

impl fmt::Display for ToolName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A tool's name, title, description, input schema and effect.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDescriptor {
    /// The tool's name.
    pub name: ToolName,
    /// A short human title.
    pub title: String,
    /// What the tool does, written by the adapter's author; never third-party text.
    pub description: String,
    /// The JSON Schema of the arguments object.
    pub input_schema: Value,
    /// Whether it reads or writes, and how a write may be retried.
    pub effect: Effect,
}

impl ToolDescriptor {
    /// The MCP `readOnlyHint`: the tool changes nothing.
    pub const fn read_only_hint(&self) -> bool {
        matches!(self.effect, Effect::Read)
    }

    /// The MCP `destructiveHint`: a write may change or remove what exists.
    pub const fn destructive_hint(&self) -> bool {
        matches!(self.effect, Effect::Write { .. })
    }

    /// The MCP `idempotentHint`: repeating the call with the same arguments has no further effect.
    pub const fn idempotent_hint(&self) -> bool {
        matches!(
            self.effect,
            Effect::Read | Effect::Write { retry: Retry::Safe }
        )
    }
}

/// The arguments a mind passed to a tool, already parsed as a JSON object.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ToolArguments(pub Map<String, Value>);

/// What a tool returns to the mind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    /// The text the mind reads.
    pub text: String,
    /// The same answer as structured data, when the tool has one.
    pub structured: Option<Value>,
}

/// Whether a tool only reads or also writes, and how a write may be retried.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Effect {
    /// The tool changes nothing at the service.
    Read,
    /// The tool changes something at the service.
    Write {
        /// How the write may be repeated after an unknown outcome.
        retry: Retry,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(effect: Effect) -> ToolDescriptor {
        ToolDescriptor {
            name: ToolName::new("post").expect("tool name"),
            title: "Post".to_owned(),
            description: "Posts.".to_owned(),
            input_schema: serde_json::json!({ "type": "object" }),
            effect,
        }
    }

    #[test]
    fn tool_names_fit_the_projected_bound() {
        let service = ServiceName::new("abcdefghijklmnop").expect("16-byte service");
        let longest = ToolName::new(&"t".repeat(ToolName::MAX_BYTES)).expect("47-byte tool");
        assert_eq!(longest.projected(&service).len(), 64);
        assert!(ToolName::new(&"t".repeat(ToolName::MAX_BYTES + 1)).is_none());
        for bad in ["", "Post", "1post", "_post", "po-st", "po st"] {
            assert!(ToolName::new(bad).is_none(), "{bad:?}");
        }
        assert_eq!(
            ToolName::new("read_topic")
                .expect("tool")
                .projected(&ServiceName::new("board").expect("service")),
            "board_read_topic"
        );
    }

    #[test]
    fn hints_derive_from_the_effect() {
        let read = descriptor(Effect::Read);
        assert!(read.read_only_hint() && read.idempotent_hint() && !read.destructive_hint());
        let safe = descriptor(Effect::Write { retry: Retry::Safe });
        assert!(!safe.read_only_hint() && safe.idempotent_hint() && safe.destructive_hint());
        for retry in [Retry::ReadFirst, Retry::Never] {
            let write = descriptor(Effect::Write { retry });
            assert!(!write.read_only_hint() && !write.idempotent_hint());
        }
    }
}
