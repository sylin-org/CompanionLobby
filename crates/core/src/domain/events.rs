//! The closed in-process event vocabulary. Adapters and the poller publish; delivery and the
//! diagnostics journal subscribe. Fan-out is best-effort in-process messaging, not a broker.

use serde::Serialize;
use serde_json::Value;

use crate::domain::intake::IntakeChannel;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum DomainEvent {
    CompanionSelected { enrollment_id: String },
    ContextArrived { context_id: String, origin: String },
    PollCompleted { enrollment_id: String, revision: String, waiting: i64, activity: i64 },
    PollFailed { enrollment_id: String, attempt: u32, reason: String },
    BackoffScheduled { enrollment_id: String, seconds: u64 },
    AttentionObserved { enrollment_id: String, new_items: Vec<String>, coalesced: usize },
    AttentionDelivered { enrollment_id: String, item_count: usize, delivery_mode: String },
    WriteRegistered { request_id: String, operation: String },
    WriteSettled { request_id: String, state: String },
    ToolInvoked { channel: IntakeChannel, tool: String },
    ToolCompleted { channel: IntakeChannel, tool: String, status: String, text_bytes: usize },
    /// The serve-mode companion manager became ready. The loopback URL is journal material
    /// (the operator's recovery path once stderr has scrolled away) and state material
    /// (any process's Connect pops this page); it is never model-visible output. The
    /// SSE feed skips it.
    ManagerPageReady { url: String },
    /// The on-the-fly handshake's live progress: narration for the
    /// companion manager's activity feed. Presentation-only — never a token, password,
    /// proof or session value. `companion` is the companion handle; `code` on failure is
    /// the honest problem code the model saw; `initiator` names who started this
    /// connect ("model (via {client})" for MCP tool calls, "operator (CLI)" for the
    /// command line, "companion manager" for page-driven actions and the auto-resume).
    ConnectStarted { origin: String, initiator: String },
    ConnectResolved { origin: String, companion: String, initiator: String },
    ConnectWaitingForOperator { origin: String, companion: String, needed: String, initiator: String },
    ConnectOperatorCompleted { origin: String, companion: String, initiator: String },
    ConnectEnrolled { origin: String, companion: String, initiator: String },
    ConnectArrived { origin: String, companion: String, initiator: String },
    ConnectFailed { origin: String, companion: String, code: String, initiator: String },
    Shutdown { reason: String },
    /// A companion node changed in the manager's graph. `node` IS the update — the
    /// manager's own projection of that companion (account status, service grants,
    /// places with session status, never a token value) — so every open copy of the
    /// page converges by patching its graph, with no follow-up fetch.
    CompanionChanged { local_id: String, node: Value },
    /// A companion node left the manager's graph, places and all.
    CompanionRemoved { local_id: String },
    /// One server card's presentation changed; `card` is the projection the page's
    /// graph should now hold for that origin.
    ServerCardChanged { origin: String, card: Value },
}
