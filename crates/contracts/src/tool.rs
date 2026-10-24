//! Tool descriptors and results.
//!
//! A service adapter declares its tools; the daemon projects them to a mind as
//! `<service>_<tool>` and derives the MCP hints from each tool's [`Effect`]. A
//! result carries text within a rendered-output budget plus structured data,
//! with third-party fields marked as quoted.

use crate::Retry;

/// A tool name as a service adapter declares it, before the service prefix.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct ToolName {}

/// A tool's name, title, description, input schema and effect.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ToolDescriptor {}

/// The arguments a mind passed to a tool, already parsed as JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ToolArguments {}

/// What a tool returns to the mind.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ToolResult {}

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
