//! Read models derived from the state.
//!
//! Each attachment's catalog is projected, never declared: `reach`, plus the
//! native tools of the companion's ready connections as `<service>_<tool>`.
//! A companion with no connections sees `reach` alone. `reach` always reports
//! the live tool set, so a mind can trust it even when its host ignores tool
//! list changes.
//!
//! The manager page's projections are built the same way: each companion as
//! the page shows it, with the host configuration that names it, and the live
//! feed, which turns every event the lobby emits into one item a page applies
//! to what it last loaded.
//!
//! Projections hold no truth and can always be rebuilt.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

use lobby_contracts::{Companion, Effect, Passage, ToolDescriptor, ToolName, ToolResult};
use serde_json::{Map, Value, json};
use tokio::sync::broadcast;

use crate::domain::{CompanionView, Event};
use crate::journal;

/// The name of the one tool every attachment has.
pub const REACH: &str = "reach";

/// The descriptor of `reach`.
pub fn reach_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: ToolName::new(REACH).unwrap_or_else(|| unreachable!("reach is a valid tool name")),
        title: "Reach".to_owned(),
        description: "Says which companion you are attached as and what that companion can reach \
                      right now: each connected service, the addresses it reaches there, and the \
                      tools you can call for it. Call it first, and again whenever a tool you \
                      expected is missing."
            .to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
        effect: Effect::Read,
    }
}

/// The tools an attachment of `companion` sees, in order.
pub fn catalog(_companion: &Companion) -> Vec<ToolDescriptor> {
    vec![reach_descriptor()]
}

/// The answer to `reach` for `companion`.
pub fn reach(companion: &Companion) -> ToolResult {
    let tools: Vec<String> = catalog(companion)
        .iter()
        .map(|tool| tool.name.as_str().to_owned())
        .collect();
    let text = format!(
        "You are attached as {display} (moniker {moniker}). {display} reaches nothing yet: \
         the sponsor has not connected a service for this companion. Your tools: {tools}.",
        display = companion.display_name,
        moniker = companion.moniker,
        tools = tools.join(", "),
    );
    ToolResult {
        text: vec![Passage::Authored(text)],
        structured: Some(json!({
            "companion": {
                "id": companion.id.to_string(),
                "moniker": companion.moniker.as_str(),
                "displayName": companion.display_name.as_str(),
            },
            "connections": [],
            "tools": tools,
        })),
    }
}

/// A companion as the manager page shows it: the record, how many minds are
/// attached as it right now, and the host configuration that names it.
pub fn companion_json(view: &CompanionView, command: &str) -> Value {
    let companion = &view.record.companion;
    json!({
        "id": companion.id.to_string(),
        "moniker": companion.moniker.as_str(),
        "displayName": companion.display_name.as_str(),
        "revision": view.record.revision,
        "attachments": view.attachments,
        "hostConfig": host_config(companion, command),
    })
}

/// The `mcpServers` block an MCP host configuration needs for `companion`:
/// the server is named after the moniker, and the shim is told the immutable
/// id, so renaming the companion never breaks the configuration. `command`
/// is the path of this executable; JSON escaping takes care of the
/// backslashes of a Windows path.
pub fn host_config(companion: &Companion, command: &str) -> String {
    let mut servers = Map::new();
    servers.insert(
        companion.moniker.as_str().to_owned(),
        json!({ "command": command, "args": ["attach", companion.id.to_string()] }),
    );
    serde_json::to_string_pretty(&json!({ "mcpServers": servers })).unwrap_or_default()
}

/// What the live feed says about one event.
pub fn event_json(event: &Event, store_id: &str, command: &str) -> Value {
    match event {
        Event::CompanionChanged(view) => {
            json!({ "kind": "companion", "companion": companion_json(view, command) })
        }
        Event::CompanionRemoved(id) => json!({ "kind": "companion_removed", "id": id.to_string() }),
        Event::Journaled(entry) => {
            json!({ "kind": "journal", "entry": journal::encode(store_id, entry) })
        }
        Event::JournalHealth(healthy) => json!({ "kind": "journal_health", "healthy": healthy }),
    }
}

/// How many items the feed keeps for a client that reconnects.
pub const FEED_REPLAY_ITEMS: usize = 512;
/// How many bytes of items the feed keeps for a client that reconnects.
pub const FEED_REPLAY_BYTES: usize = 512 * 1024;
/// How many items a slow client may fall behind before it is reset.
pub const FEED_BACKLOG: usize = 256;

/// One item of the live feed: its position in this process's feed and its
/// JSON text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedItem {
    /// The position, counted from 1 since this daemon started.
    pub seq: u64,
    /// The JSON text of the item.
    pub data: Arc<str>,
}

/// What a new subscriber starts from.
#[derive(Debug)]
pub struct Subscription {
    /// Why the subscriber must reload the lobby before it applies anything,
    /// if it must: it named no position, or one this feed cannot continue.
    pub reset: Option<&'static str>,
    /// The items after the position it named, when it can continue.
    pub replay: Vec<FeedItem>,
    /// Every item published from now on.
    pub receiver: broadcast::Receiver<FeedItem>,
}

/// The live feed: every event the lobby emits, as projections, in order.
///
/// The feed belongs to one process: its epoch is fresh at every start, and a
/// position is `<epoch>:<seq>`. A subscriber that names a position this feed
/// still holds gets what came after it; any other subscriber is told to
/// reset, that is to reload the lobby's state and apply what follows. A
/// subscriber that falls behind is reset too, never silently skipped.
#[derive(Debug)]
pub struct Feed {
    epoch: String,
    state: Mutex<FeedState>,
    sender: broadcast::Sender<FeedItem>,
}

#[derive(Debug, Default)]
struct FeedState {
    seq: u64,
    history: VecDeque<FeedItem>,
    bytes: usize,
}

impl Default for Feed {
    fn default() -> Self {
        Self::new()
    }
}

impl Feed {
    /// A feed with a fresh epoch and nothing in it.
    pub fn new() -> Self {
        Self {
            epoch: uuid::Uuid::now_v7().simple().to_string(),
            state: Mutex::new(FeedState::default()),
            sender: broadcast::channel(FEED_BACKLOG).0,
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, FeedState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// This process's epoch.
    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    /// The position of the last item published, 0 before the first.
    pub fn position(&self) -> u64 {
        self.state().seq
    }

    /// Publishes `data` as the next item.
    pub fn publish(&self, data: &Value) {
        let mut state = self.state();
        state.seq += 1;
        let item = FeedItem {
            seq: state.seq,
            data: Arc::from(data.to_string()),
        };
        state.bytes += item.data.len();
        state.history.push_back(item.clone());
        while state.history.len() > FEED_REPLAY_ITEMS || state.bytes > FEED_REPLAY_BYTES {
            if let Some(dropped) = state.history.pop_front() {
                state.bytes -= dropped.data.len();
            }
        }
        // No subscriber is not a failure.
        let _ = self.sender.send(item);
    }

    /// Subscribes after the position `last`, as an `EventSource` sends it in
    /// `Last-Event-ID`.
    pub fn subscribe(&self, last: Option<&str>) -> Subscription {
        let state = self.state();
        let receiver = self.sender.subscribe();
        let position = last.and_then(|last| {
            let (epoch, seq) = last.rsplit_once(':')?;
            (epoch == self.epoch).then(|| seq.parse::<u64>().ok())?
        });
        let oldest = state.history.front().map_or(state.seq + 1, |item| item.seq);
        let (reset, replay) = match (last, position) {
            (None, _) => (Some("fresh"), Vec::new()),
            (Some(_), None) => (Some("another_feed"), Vec::new()),
            (Some(_), Some(seq)) if seq > state.seq => (Some("ahead"), Vec::new()),
            (Some(_), Some(seq)) if seq + 1 < oldest => (Some("gone"), Vec::new()),
            (Some(_), Some(seq)) => (
                None,
                state
                    .history
                    .iter()
                    .filter(|item| item.seq > seq)
                    .cloned()
                    .collect(),
            ),
        };
        Subscription {
            reset,
            replay,
            receiver,
        }
    }

    /// The `id:` of an item, as the feed hands it out.
    pub fn id_of(&self, item: &FeedItem) -> String {
        format!("{}:{}", self.epoch, item.seq)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lobby_contracts::{CompanionId, DisplayName, Moniker, Sponsor};

    fn ada() -> Companion {
        Companion {
            id: CompanionId::from_u128(1),
            moniker: Moniker::new("ada").expect("moniker"),
            display_name: DisplayName::new("Ada").expect("name"),
            sponsor: Sponsor::LOCAL,
        }
    }

    #[test]
    fn a_companion_without_connections_sees_reach_alone() {
        let catalog = catalog(&ada());
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].name.as_str(), REACH);
        assert!(catalog[0].read_only_hint());
    }

    #[test]
    fn reach_reports_the_companion_and_the_live_tool_set() {
        let answer = reach(&ada());
        let text = answer.rendered();
        assert!(text.contains("Ada") && text.contains("reach"));
        let structured = answer.structured.expect("structured");
        assert_eq!(structured["companion"]["moniker"], "ada");
        assert_eq!(structured["tools"], json!(["reach"]));
        assert_eq!(structured["connections"], json!([]));
    }
}
