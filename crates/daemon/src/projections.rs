//! Read models derived from the state.
//!
//! Each attachment's catalog is projected, never declared: `reach`, plus the
//! native tools of every service the companion holds a ready connection to,
//! as `<service>_<tool>`. Each of those tools takes one more argument, `at`,
//! whose values are the sponsor's labels for the companion's connections to
//! that service: it may be left out while there is one, and is required once
//! there are more. A companion with no connections sees `reach` alone.
//! `reach` always reports the live tool set, so a mind can trust it even when
//! its host ignores tool list changes. Nothing a service wrote reaches a tool
//! name, description or schema: the names and words are the adapter's, and
//! the labels and addresses are the sponsor's.
//!
//! The manager page's projections are built the same way: each companion as
//! the page shows it, with the host configuration that names it, each
//! credential with the label the sponsor knows its account by, each journal
//! entry as one sentence in the sponsor's words, and the live feed, which
//! turns every event the lobby emits into one item a page applies to what it
//! last loaded. A sentence names companions, services and connections the
//! way the sponsor named them, never by an id, and says how the entry ended
//! in plain words, never by an outcome's wire name.
//!
//! Projections hold no truth and can always be rebuilt.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

use lobby_contracts::{
    Address, Companion, Connection, ConnectionState, Credential, CredentialState, Effect, Label,
    Passage, ServiceName, ToolDescriptor, ToolName, ToolResult,
};
use serde_json::{Map, Value, json};
use tokio::sync::broadcast;

use crate::domain::{CompanionView, Event, SignInView};
use crate::journal;
use crate::registry::capitalized;
use crate::store::State;

/// The name of the one tool every attachment has.
pub const REACH: &str = "reach";
/// The argument every service tool takes to say which connection it acts through.
pub const AT: &str = "at";

/// What a companion reaches through one service: the service's tools and
/// the sponsor's label and address of each ready connection to it.
#[derive(Debug, Clone, PartialEq)]
pub struct Reached {
    /// The service.
    pub service: ServiceName,
    /// Its tools, as its adapter declares them.
    pub tools: Vec<ToolDescriptor>,
    /// Each ready connection's label and address, oldest first.
    pub connections: Vec<(Label, Address)>,
}

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

/// The tools an attachment sees, in order, each with the name a mind calls
/// it by: `reach`, then each reached service's tools with `at` added.
pub fn catalog(reached: &[Reached]) -> Vec<(String, ToolDescriptor)> {
    let mut tools = vec![(REACH.to_owned(), reach_descriptor())];
    for place in reached {
        let labels: Vec<&str> = place
            .connections
            .iter()
            .map(|(label, _)| label.as_str())
            .collect();
        for tool in &place.tools {
            let mut descriptor = tool.clone();
            descriptor.input_schema = with_at(&tool.input_schema, &place.service, &labels);
            tools.push((tool.name.projected(&place.service), descriptor));
        }
    }
    tools
}

/// `schema` with the `at` property: the labels as its only values, required
/// when there is more than one.
fn with_at(schema: &Value, service: &ServiceName, labels: &[&str]) -> Value {
    let mut schema = schema.clone();
    if !schema.is_object() {
        schema = json!({ "type": "object" });
    }
    let description = if labels.len() == 1 {
        format!("Which {service} to act at, by the name the sponsor gave it; it may be left out.")
    } else {
        format!("Which {service} to act at, by the name the sponsor gave it.")
    };
    let properties = schema
        .as_object_mut()
        .map(|object| object.entry("properties").or_insert_with(|| json!({})));
    if let Some(Value::Object(properties)) = properties {
        properties.insert(
            AT.to_owned(),
            json!({ "type": "string", "enum": labels, "description": description }),
        );
    }
    if labels.len() > 1
        && let Some(object) = schema.as_object_mut()
    {
        let required = object.entry("required").or_insert_with(|| json!([]));
        if let Value::Array(required) = required {
            required.push(json!(AT));
        }
    }
    schema
}

/// The answer to `reach` for `companion`, which reaches `reached`.
pub fn reach(companion: &Companion, reached: &[Reached]) -> ToolResult {
    let tools: Vec<String> = catalog(reached).into_iter().map(|(name, _)| name).collect();
    let display = &companion.display_name;
    let mut text = format!(
        "You are attached as {display} (moniker {moniker}). ",
        moniker = companion.moniker
    );
    if reached.is_empty() {
        text.push_str(&format!(
            "{display} reaches nothing yet: the sponsor has not connected a service for this companion."
        ));
    }
    for place in reached {
        let places: Vec<String> = place
            .connections
            .iter()
            .map(|(label, address)| format!("{label} ({address})"))
            .collect();
        let names: Vec<String> = place
            .tools
            .iter()
            .map(|tool| tool.name.projected(&place.service))
            .collect();
        text.push_str(&format!(
            "{display} reaches {service} at {places}, with {names}; pass {AT} to say which. ",
            service = place.service,
            places = places.join(" and "),
            names = names.join(", "),
        ));
    }
    text.push_str(&format!(" Your tools: {}.", tools.join(", ")));
    let connections: Vec<Value> = reached
        .iter()
        .flat_map(|place| {
            place.connections.iter().map(|(label, address)| {
                json!({
                    "service": place.service.as_str(),
                    "at": label.as_str(),
                    "address": address.as_str(),
                })
            })
        })
        .collect();
    ToolResult {
        text: vec![Passage::Authored(text.replace("  ", " ").trim().to_owned())],
        structured: Some(json!({
            "companion": {
                "id": companion.id.to_string(),
                "moniker": companion.moniker.as_str(),
                "displayName": companion.display_name.as_str(),
            },
            "connections": connections,
            "tools": tools,
        })),
    }
}

/// A credential as the manager page shows it. Its material never is.
pub fn credential_json(credential: &Credential) -> Value {
    json!({
        "id": credential.id.to_string(),
        "companion": credential.companion.to_string(),
        "kind": credential.kind.as_str(),
        "account": credential.account.as_str(),
        "label": credential.label.as_str(),
        "permissions": credential.permissions,
        "state": match credential.state {
            CredentialState::Ready => "ready",
            CredentialState::Revoked => "revoked",
        },
    })
}

/// A sign-in as the manager page follows it. Its `state` never shows.
pub fn sign_in_json(sign_in: &SignInView) -> Value {
    json!({
        "id": sign_in.id,
        "companion": sign_in.companion.to_string(),
        "kind": sign_in.kind.as_str(),
        "account": sign_in.account,
        "status": sign_in.status.code(),
        "expiresAt": sign_in.expires_at,
    })
}

/// A connection as the manager page shows it. What the service issued for
/// it never is.
pub fn connection_json(connection: &Connection) -> Value {
    json!({
        "id": connection.id.to_string(),
        "companion": connection.companion.to_string(),
        "credential": connection.credential.to_string(),
        "service": connection.service.as_str(),
        "address": connection.address.as_str(),
        "label": connection.label.as_str(),
        "state": match connection.state {
            ConnectionState::Ready => "ready",
            ConnectionState::Revoked => "revoked",
        },
        "revision": connection.revision,
    })
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
        "portrait": view.record.portrait.map(|_| {
            format!("/api/companions/{}/portrait?v={}", companion.id, view.record.revision)
        }),
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

/// What the live feed says about one event, with the names `state` gives
/// companions and connections.
pub fn event_json(event: &Event, state: &State, command: &str) -> Value {
    match event {
        Event::CompanionChanged(view) => {
            json!({ "kind": "companion", "companion": companion_json(view, command) })
        }
        Event::CompanionRemoved(id) => json!({ "kind": "companion_removed", "id": id.to_string() }),
        Event::CredentialChanged(credential) => {
            json!({ "kind": "credential", "credential": credential_json(credential) })
        }
        Event::SignInChanged(sign_in) => {
            json!({ "kind": "sign_in", "signIn": sign_in_json(sign_in) })
        }
        Event::ConnectionChanged(connection) => {
            json!({ "kind": "connection", "connection": connection_json(connection) })
        }
        Event::Journaled(entry) => {
            let mut entry = journal::encode(&state.store_id, entry);
            told(&mut entry, &Names::of(state));
            json!({ "kind": "journal", "entry": entry })
        }
        Event::JournalHealth(healthy) => json!({ "kind": "journal_health", "healthy": healthy }),
    }
}

/// The names the sponsor gave what journal entries name by id.
#[derive(Debug, Default)]
pub struct Names {
    companions: Vec<(String, String)>,
    connections: Vec<(String, String)>,
}

impl Names {
    /// The display name of every companion and the label of every
    /// connection in `state`.
    pub fn of(state: &State) -> Self {
        Self {
            companions: state
                .companions
                .iter()
                .map(|record| {
                    (
                        record.companion.id.to_string(),
                        record.companion.display_name.as_str().to_owned(),
                    )
                })
                .collect(),
            connections: state
                .connections
                .iter()
                .map(|connection| {
                    (
                        connection.id.to_string(),
                        connection.label.as_str().to_owned(),
                    )
                })
                .collect(),
        }
    }

    fn find<'a>(list: &'a [(String, String)], id: &str) -> Option<&'a str> {
        list.iter()
            .find(|(known, _)| known == id)
            .map(|(_, name)| name.as_str())
    }
}

/// Adds the entry's sentence to an encoded journal entry.
pub fn told(entry: &mut Value, names: &Names) {
    let said = sentence(entry, names);
    if let Some(object) = entry.as_object_mut() {
        object.insert("sentence".to_owned(), Value::String(said));
    }
}

/// One encoded journal entry as a sentence: who, as which companion, did
/// what, through which connection, and how it ended.
pub fn sentence(entry: &Value, names: &Names) -> String {
    let text = |field: &str| entry[field].as_str().unwrap_or_default();
    let companion = match entry["companion"].as_str() {
        None => "a companion".to_owned(),
        Some(id) => Names::find(&names.companions, id)
            .map_or_else(|| "a companion since removed".to_owned(), str::to_owned),
    };
    let service = capitalized(text("service"));
    let at = entry["connection"]
        .as_str()
        .and_then(|id| Names::find(&names.connections, id))
        .map_or_else(String::new, |label| format!(" at {label}"));
    let command = text("command");
    let mind = entry["initiator"] == "mind";
    let who = match text("initiator") {
        "sponsor" => "You".to_owned(),
        "lobby" => "The lobby".to_owned(),
        _ if matches!(command, "attach" | "detach") => "A mind".to_owned(),
        _ => format!("A mind as {companion}"),
    };
    // What happened, said once as done and once as attempted.
    let (done, attempt) = match (text("kind"), command) {
        ("invoked" | "settled", _) => {
            let tool = text("tool");
            (
                format!("called {tool} on {service}{at}"),
                format!("call {tool} on {service}{at}"),
            )
        }
        (_, "create_companion") => (
            format!("created {companion}"),
            format!("create {companion}"),
        ),
        (_, "rename_companion") => (
            format!("renamed {companion}"),
            format!("rename {companion}"),
        ),
        (_, "remove_companion") => (
            format!("removed {companion}"),
            format!("remove {companion}"),
        ),
        (_, "set_portrait") => (
            format!("changed the portrait of {companion}"),
            format!("change the portrait of {companion}"),
        ),
        (_, "attach") if mind => (
            format!("attached as {companion}"),
            format!("attach as {companion}"),
        ),
        (_, "attach") => (
            format!("let a mind attach as {companion}"),
            format!("let a mind attach as {companion}"),
        ),
        (_, "detach") if mind => (
            format!("detached from {companion}"),
            format!("detach from {companion}"),
        ),
        (_, "detach") => (
            format!("ended an attachment as {companion}"),
            format!("end an attachment as {companion}"),
        ),
        (_, "renew_attach_secret") => (
            "replaced the attach secret".to_owned(),
            "replace the attach secret".to_owned(),
        ),
        (_, "begin_sign_in") => (
            format!("started signing {companion} in"),
            format!("start signing {companion} in"),
        ),
        (_, "complete_sign_in") => (
            format!("signed {companion} in"),
            format!("sign {companion} in"),
        ),
        (_, "cancel_sign_in") => (
            format!("cancelled a sign-in for {companion}"),
            format!("cancel a sign-in for {companion}"),
        ),
        (_, "revoke_credential") => (
            format!("revoked the account {companion} held"),
            format!("revoke the account {companion} held"),
        ),
        (_, "grant_connection") => (
            format!("opened a connection for {companion} to {service}{at}"),
            format!("open a connection for {companion} to {service}{at}"),
        ),
        (_, "revoke_connection") => (
            format!("revoked the connection of {companion} to {service}{at}"),
            format!("revoke the connection of {companion} to {service}{at}"),
        ),
        (_, "sweep_keyring") => (
            "tidied its stored files".to_owned(),
            "tidy its stored files".to_owned(),
        ),
        _ => (
            "changed the lobby".to_owned(),
            "change the lobby".to_owned(),
        ),
    };
    let ended = match text("outcome") {
        "done" => return stopped(format!("{who} {done}")),
        "accepted" => return format!("{who} {done}; the service finishes it later."),
        "pending" => return stopped(format!("{who} set out to {attempt}")),
        "refused" => "it was refused",
        "not_found" => "what it named was not found",
        "conflict" => "it clashed with a change made first",
        "invalid_input" => "what was asked for was refused",
        "needs_sign_in" => "the account needs signing in again",
        "needs_permission" => "the account lacks the permission",
        "revoked" => "it had been revoked",
        "temporarily_unavailable" => "it could not be reached",
        "rate_limited" => "it was asked to slow down",
        "outcome_unknown" => "whether it landed is unknown",
        "unsupported" => "it is not offered",
        _ => "something went wrong inside the lobby",
    };
    format!("{who} tried to {attempt}, but {ended}.")
}

/// `text` closed with a full stop, unless a name it ends with closes it
/// already.
fn stopped(mut text: String) -> String {
    if !text.ends_with(['.', '!', '?']) {
        text.push('.');
    }
    text
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

    fn board(labels: &[&str]) -> Reached {
        Reached {
            service: ServiceName::new("board").expect("service"),
            tools: vec![ToolDescriptor {
                name: ToolName::new("read").expect("tool"),
                title: "Read".to_owned(),
                description: "Reads.".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": { "limit": { "type": "integer" } },
                    "additionalProperties": false
                }),
                effect: Effect::Read,
            }],
            connections: labels
                .iter()
                .enumerate()
                .map(|(port, label)| {
                    (
                        Label::new(label).expect("label"),
                        Address::parse(
                            &format!("http://127.0.0.1:{}", 5300 + port),
                            lobby_contracts::AddressPolicy::LoopbackAllowed,
                        )
                        .expect("address"),
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn a_companion_without_connections_sees_reach_alone() {
        let catalog = catalog(&[]);
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].0, REACH);
        assert!(catalog[0].1.read_only_hint());
    }

    #[test]
    fn reach_reports_the_companion_and_the_live_tool_set() {
        let answer = reach(&ada(), &[]);
        let text = answer.rendered();
        assert!(text.contains("Ada") && text.contains("reach"));
        let structured = answer.structured.expect("structured");
        assert_eq!(structured["companion"]["moniker"], "ada");
        assert_eq!(structured["tools"], json!(["reach"]));
        assert_eq!(structured["connections"], json!([]));
    }

    #[test]
    fn service_tools_take_at_naming_the_sponsors_labels() {
        let one = catalog(&[board(&["front"])]);
        assert_eq!(
            one.iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            ["reach", "board_read"]
        );
        let schema = &one[1].1.input_schema;
        assert_eq!(schema["properties"]["at"]["enum"], json!(["front"]));
        assert!(
            schema.get("required").is_none(),
            "one connection needs no at"
        );
        assert_eq!(schema["properties"]["limit"]["type"], "integer");
        let two = catalog(&[board(&["front", "back"])]);
        let schema = &two[1].1.input_schema;
        assert_eq!(schema["properties"]["at"]["enum"], json!(["front", "back"]));
        assert_eq!(schema["required"], json!(["at"]));
        let answer = reach(&ada(), &[board(&["front", "back"])]);
        let structured = answer.structured.clone().expect("structured");
        assert_eq!(structured["tools"], json!(["reach", "board_read"]));
        assert_eq!(structured["connections"][1]["at"], "back");
        assert!(
            answer
                .rendered()
                .contains("board at front (http://127.0.0.1:5300) and back")
        );
    }

    fn names() -> (Names, String, String) {
        let ada = CompanionId::from_u128(1).to_string();
        let front = lobby_contracts::ConnectionId::from_u128(2).to_string();
        let names = Names {
            companions: vec![(ada.clone(), "Ada".to_owned())],
            connections: vec![(front.clone(), "front".to_owned())],
        };
        (names, ada, front)
    }

    #[test]
    fn a_journal_entry_reads_as_a_sentence_with_names_never_ids_or_codes() {
        let (names, ada, front) = names();
        let gone = CompanionId::from_u128(9).to_string();
        let mind = lobby_contracts::AttachmentId::from_u128(3).to_string();
        let cases = [
            (
                json!({ "initiator": "sponsor", "companion": ada, "kind": "command", "command": "create_companion", "outcome": "done" }),
                "You created Ada.",
            ),
            (
                json!({ "initiator": "mind", "attachment": mind, "companion": ada, "kind": "command", "command": "attach", "outcome": "done" }),
                "A mind attached as Ada.",
            ),
            (
                json!({ "initiator": "mind", "attachment": mind, "companion": ada, "connection": front, "service": "board", "tool": "post", "kind": "settled", "outcome": "outcome_unknown" }),
                "A mind as Ada tried to call post on Board at front, but whether it landed is unknown.",
            ),
            (
                json!({ "initiator": "mind", "attachment": mind, "companion": ada, "connection": front, "service": "board", "tool": "read", "kind": "settled", "outcome": "done" }),
                "A mind as Ada called read on Board at front.",
            ),
            (
                json!({ "initiator": "sponsor", "companion": ada, "connection": front, "service": "board", "kind": "command", "command": "revoke_connection", "outcome": "done" }),
                "You revoked the connection of Ada to Board at front.",
            ),
            (
                json!({ "initiator": "sponsor", "companion": ada, "kind": "command", "command": "begin_sign_in", "outcome": "conflict" }),
                "You tried to start signing Ada in, but it clashed with a change made first.",
            ),
            (
                json!({ "initiator": "lobby", "companion": gone, "kind": "command", "command": "detach", "outcome": "done" }),
                "The lobby ended an attachment as a companion since removed.",
            ),
            (
                json!({ "initiator": "sponsor", "kind": "command", "command": "create_companion", "outcome": "pending" }),
                "You set out to create a companion.",
            ),
        ];
        for (entry, said) in cases {
            let sentence = sentence(&entry, &names);
            assert_eq!(sentence, said);
            for id in [&ada, &front, &gone, &mind] {
                assert!(!sentence.contains(id.as_str()), "{sentence}");
            }
            assert!(!sentence.contains('_'), "no wire name: {sentence}");
        }
        let short = CompanionId::from_u128(4).to_string();
        let names = Names {
            companions: vec![
                (ada.clone(), "Ada".to_owned()),
                (short.clone(), "Ada L.".to_owned()),
            ],
            connections: names.connections,
        };
        assert_eq!(
            sentence(
                &json!({ "initiator": "sponsor", "companion": short, "kind": "command", "command": "rename_companion", "outcome": "done" }),
                &names
            ),
            "You renamed Ada L.",
            "a name that ends with a stop is not stopped twice"
        );
        assert_eq!(
            sentence(
                &json!({ "initiator": "lobby", "kind": "command", "command": "sweep_keyring", "outcome": "internal" }),
                &names
            ),
            "The lobby tried to tidy its stored files, but something went wrong inside the lobby."
        );
        let mut entry = json!({ "initiator": "sponsor", "companion": ada, "kind": "command", "command": "remove_companion", "outcome": "done" });
        told(&mut entry, &names);
        assert_eq!(entry["sentence"], "You removed Ada.");
    }
}
