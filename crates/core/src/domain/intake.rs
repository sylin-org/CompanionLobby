//! Input-model companion. An intake is a fact about the shape of the caller, not an open
//! label: it is recorded for attribution and diagnostics, and it is never an input to a
//! domain or authority decision. Both intakes translate their input model into the same
//! closed [`Operation`](crate::application::operations::Operation) vocabulary and cross the
//! same hub, journal and completion path.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntakeChannel {
    /// A model-facing MCP client over stdio.
    #[default]
    Mcp,
    /// A local script or human, through the command line.
    Cli,
    /// The operator web page served by the `operator` verb's loopback listener.
    Manager,
}

impl IntakeChannel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mcp => "mcp",
            Self::Cli => "cli",
            Self::Manager => "manager",
        }
    }
}
