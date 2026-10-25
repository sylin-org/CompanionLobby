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
    AccountLabel, Address, Companion, Connection, ConnectionState, Credential, CredentialState,
    Effect, KnownAs, Label, Passage, ProviderKind, ServiceName, ToolDescriptor, ToolName,
    ToolResult,
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
    /// The name its makers give it, as the sponsor and the mind read it.
    pub title: String,
    /// Its tools, as its adapter declares them, those its connections allow.
    pub tools: Vec<ToolDescriptor>,
    /// Each ready connection, oldest first.
    pub connections: Vec<ReachedAt>,
}

/// One ready connection a companion reaches a service through.
#[derive(Debug, Clone, PartialEq)]
pub struct ReachedAt {
    /// The sponsor's label for it.
    pub label: Label,
    /// Where the service is.
    pub address: Address,
    /// The tools that what the service issued for it allows.
    pub allows: Vec<ToolName>,
    /// The label of the account it was established with.
    pub account: Option<AccountLabel>,
    /// How the service knows the companion through it, and what its grants
    /// allow there, as its adapter reads what the service issued.
    pub known: Option<KnownAs>,
}

impl ReachedAt {
    /// The tools of `place` this connection allows, in catalog order.
    fn tools<'a>(&'a self, place: &'a Reached) -> impl Iterator<Item = &'a ToolDescriptor> {
        place
            .tools
            .iter()
            .filter(|tool| self.allows.contains(&tool.name))
    }
}

/// An account a companion holds, as `reach` names it: by the label the
/// sponsor knows it by and its kind's title, and whether it waits for the
/// sponsor to sign it in again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldAccount {
    /// What the sponsor knows the account by.
    pub label: AccountLabel,
    /// Its provider kind.
    pub kind: ProviderKind,
    /// The title the sponsor reads for that kind.
    pub title: String,
    /// Whether it waits for the sponsor to sign it in again.
    pub waits: bool,
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
            .map(|connection| connection.label.as_str())
            .collect();
        for tool in &place.tools {
            let mut descriptor = tool.clone();
            descriptor.input_schema = with_at(&tool.input_schema, &place.title, &labels);
            tools.push((tool.name.projected(&place.service), descriptor));
        }
    }
    tools
}

/// `schema` with the `at` property: the labels as its only values, required
/// when there is more than one, described by the service's `title`.
fn with_at(schema: &Value, title: &str, labels: &[&str]) -> Value {
    let mut schema = schema.clone();
    if !schema.is_object() {
        schema = json!({ "type": "object" });
    }
    let description = if labels.len() == 1 {
        format!("Which {title} to act at, by the name the sponsor gave it; it may be left out.")
    } else {
        format!("Which {title} to act at, by the name the sponsor gave it.")
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

/// The most characters of an argument's name a refusal repeats.
const ARGUMENT_NAME_SHOWN: usize = 64;

/// The refusal for a call of `projected`, the tool `tool` as a mind calls
/// it, whose `arguments` hold one it does not take: that argument by name,
/// and the ones it takes, `at` among them. None when it takes them all.
pub fn unknown_argument(
    projected: &str,
    tool: &ToolDescriptor,
    arguments: &Map<String, Value>,
) -> Option<String> {
    let mut takes: Vec<&str> = tool
        .input_schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|properties| properties.keys().map(String::as_str).collect())
        .unwrap_or_default();
    takes.push(AT);
    let given = arguments
        .keys()
        .find(|given| !takes.contains(&given.as_str()))?;
    let shown: String = given.chars().take(ARGUMENT_NAME_SHOWN).collect();
    let shown = Value::String(shown).to_string();
    let takes: Vec<String> = takes.iter().map(|name| (*name).to_owned()).collect();
    Some(format!(
        "{projected} takes no argument called {shown}; it takes {}.",
        listed(&takes)
    ))
}

/// The answer to `reach` for `companion`, which holds `accounts`, reaches
/// `reached` and whose connections `waiting` wait for the sponsor to sign
/// their account in again: who the companion is and which accounts it
/// holds, then for each connection where it is, by the sponsor's names,
/// with which account, how the service knows it there, what the
/// connection's grants allow in the service's words, and the tool to start
/// with, in a few sentences. What a service or a provider wrote, the name
/// and reference it gave the companion and an account's label, is quoted.
/// The structured answer holds the same facts, with the whole tool list,
/// which the catalog has too.
pub fn reach(
    companion: &Companion,
    reached: &[Reached],
    waiting: &[(ServiceName, String, Label)],
    accounts: &[HeldAccount],
) -> ToolResult {
    let tools: Vec<String> = catalog(reached).into_iter().map(|(name, _)| name).collect();
    let display = companion.display_name.as_str();
    let mut text: Vec<Passage> = Vec::new();
    let say = |text: &mut Vec<Passage>, said: &str| text.push(Passage::Authored(said.to_owned()));
    say(
        &mut text,
        &format!(
            "You are {}, attached through Companion Lobby.",
            named(companion)
        ),
    );
    if !accounts.is_empty() {
        say(&mut text, &format!(" {display} holds "));
        for (index, account) in accounts.iter().enumerate() {
            if index > 0 {
                say(
                    &mut text,
                    if index + 1 == accounts.len() {
                        " and "
                    } else {
                        ", "
                    },
                );
            }
            say(&mut text, &format!("the {} account ", account.title));
            text.push(Passage::Quoted(account.label.as_str().to_owned()));
            if account.waits {
                say(
                    &mut text,
                    ", which waits for the sponsor to sign it in again",
                );
            }
        }
        say(&mut text, ".");
    }
    if reached.is_empty() && waiting.is_empty() {
        say(
            &mut text,
            &format!(
                " {display} reaches nothing yet: the sponsor has not connected a service for this companion."
            ),
        );
    }
    for place in reached {
        let title = &place.title;
        for connection in &place.connections {
            say(
                &mut text,
                &format!(
                    " {display} reaches {title} at {} ({})",
                    connection.label, connection.address
                ),
            );
            if let Some(account) = &connection.account {
                say(&mut text, " with the account ");
                text.push(Passage::Quoted(account.as_str().to_owned()));
            }
            if let Some(known) = &connection.known {
                let (name, reference) = (known.name.as_deref(), known.reference.as_deref());
                if name.is_some() || reference.is_some() {
                    say(&mut text, &format!("; {title} knows it there as "));
                }
                if let Some(name) = name {
                    text.push(Passage::Quoted(name.to_owned()));
                }
                if let Some(reference) = reference {
                    let opening = if name.is_some() { " (" } else { "" };
                    say(&mut text, &format!("{opening}{} ", known.reference_is));
                    text.push(Passage::Quoted(reference.to_owned()));
                    if name.is_some() {
                        say(&mut text, ")");
                    }
                }
                if name.is_some() || reference.is_some() {
                    say(&mut text, &format!(" and lets it {}", known.allows));
                } else {
                    say(&mut text, &format!("; {title} lets it {}", known.allows));
                }
            }
            match connection.tools(place).next() {
                Some(first) => say(
                    &mut text,
                    &format!(": start with {}.", first.name.projected(&place.service)),
                ),
                None => say(&mut text, ", where nothing is open to it now."),
            }
        }
        if place.connections.len() > 1 {
            say(
                &mut text,
                &format!(" Every {title} tool takes {AT}, the name of the one it acts at."),
            );
        }
    }
    for (_, title, label) in waiting {
        say(
            &mut text,
            &format!(
                " {display} cannot reach {title} at {label} until the sponsor signs its account in again; its tools come back then."
            ),
        );
    }
    // A host may show a mind the structured answer and not the text, so it
    // gives every fact the text does, what a service or provider wrote
    // quoted as in the text.
    let connections: Vec<Value> = reached
        .iter()
        .flat_map(|place| {
            place.connections.iter().map(move |connection| {
                json!({
                    "service": place.service.as_str(),
                    "title": place.title,
                    "at": connection.label.as_str(),
                    "address": connection.address.as_str(),
                    "account": connection.account.as_ref().map(AccountLabel::as_str),
                    "knownAs": connection.known.as_ref().map(|known| json!({
                        "referenceIs": known.reference_is,
                        "quoted": { "name": known.name, "reference": known.reference },
                    })),
                    "allows": connection.known.as_ref().map(|known| known.allows),
                    "startWith": connection
                        .tools(place)
                        .next()
                        .map(|first| first.name.projected(&place.service)),
                })
            })
        })
        .collect();
    ToolResult {
        text,
        structured: Some(json!({
            "companion": {
                "id": companion.id.to_string(),
                "moniker": companion.moniker.as_str(),
                "displayName": companion.display_name.as_str(),
            },
            "accounts": accounts
                .iter()
                .map(|account| json!({
                    "label": account.label.as_str(),
                    "kind": account.kind.as_str(),
                    "title": account.title,
                    "waitsForSignIn": account.waits,
                }))
                .collect::<Vec<_>>(),
            "connections": connections,
            "waiting": waiting
                .iter()
                .map(|(service, title, label)| json!({
                    "service": service.as_str(),
                    "title": title,
                    "at": label.as_str(),
                }))
                .collect::<Vec<_>>(),
            "tools": tools,
        })),
    }
}

/// `companion` as a mind is told who it is: its display name, with its
/// moniker beside it when the two differ.
pub fn named(companion: &Companion) -> String {
    let (display, moniker) = (companion.display_name.as_str(), companion.moniker.as_str());
    if display == moniker {
        display.to_owned()
    } else {
        format!("{display} (moniker {moniker})")
    }
}

/// `items` as prose: `a`, `a and b`, `a, b and c`.
fn listed(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
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
            CredentialState::NeedsSignIn => "needs_sign_in",
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
            ConnectionState::NeedsSignIn => "needs_sign_in",
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
/// what, through which connection, and how it ended, with the reason in the
/// sponsor's words when it did not end well. The names are the ones the
/// entry kept from when it was written; an entry that kept none is told
/// with the names `names` holds now.
pub fn sentence(entry: &Value, names: &Names) -> String {
    let text = |field: &str| entry[field].as_str().unwrap_or_default();
    let kept = |field: &str| entry[field].as_str().filter(|name| !name.is_empty());
    let named = kept("companionName")
        .map(|name| Some(name.to_owned()))
        .or_else(|| {
            entry["companion"]
                .as_str()
                .map(|id| Names::find(&names.companions, id).map(str::to_owned))
        });
    let companion = match &named {
        None => "a companion".to_owned(),
        Some(found) => found
            .clone()
            .unwrap_or_else(|| "a companion since removed".to_owned()),
    };
    // A companion's own removal names it only when its name was kept.
    let removed = named
        .clone()
        .flatten()
        .unwrap_or_else(|| "a companion".to_owned());
    let service = kept("serviceTitle").map_or_else(|| capitalized(text("service")), str::to_owned);
    let at = kept("connectionLabel")
        .or_else(|| {
            entry["connection"]
                .as_str()
                .and_then(|id| Names::find(&names.connections, id))
        })
        .map_or_else(String::new, |label| format!(" at {label}"));
    let kind = kept("kindTitle");
    let with_kind = kind.map_or_else(String::new, |kind| format!(" with {kind}"));
    let provider = kind.unwrap_or("its provider");
    let account = kept("accountLabel");
    let the_account = account.unwrap_or("the account");
    let command = text("command");
    let mind = entry["initiator"] == "mind";
    let who = match text("initiator") {
        "sponsor" => "You".to_owned(),
        "lobby" => "The lobby".to_owned(),
        _ if matches!(command, "attach" | "detach") => "A mind".to_owned(),
        _ => format!("A mind as {companion}"),
    };
    let outcome = text("outcome");
    if command == "end_unfinished_renewal" && outcome == "done" {
        return stopped(format!(
            "The lobby stopped while it renewed {the_account} for {companion}{with_kind} and cannot tell whether {provider} renewed it, so {companion} needs signing in again"
        ));
    }
    // What happened, said once as done and once as attempted.
    let (done, attempt) = match (text("kind"), command) {
        ("invoked" | "settled", _) => match kept("toolTitle") {
            Some(title) => (
                format!("used {title} on {service}{at}"),
                format!("use {title} on {service}{at}"),
            ),
            None => {
                let tool = text("tool");
                (
                    format!("called {tool} on {service}{at}"),
                    format!("call {tool} on {service}{at}"),
                )
            }
        },
        (_, "create_companion") => (
            format!("created {companion}"),
            format!("create {companion}"),
        ),
        (_, "rename_companion") => (
            format!("renamed {companion}"),
            format!("rename {companion}"),
        ),
        (_, "remove_companion") => (format!("removed {removed}"), format!("remove {removed}")),
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
            format!("started signing {companion} in{with_kind}"),
            format!("start signing {companion} in{with_kind}"),
        ),
        (_, "complete_sign_in") => (
            format!(
                "signed {companion} in{with_kind}{}",
                account.map_or_else(String::new, |account| format!(" as {account}"))
            ),
            format!("sign {companion} in{with_kind}"),
        ),
        (_, "cancel_sign_in") => (
            format!("cancelled signing {companion} in{with_kind}"),
            format!("cancel signing {companion} in{with_kind}"),
        ),
        (_, "revoke_credential") => {
            let what = match (account, kind) {
                (Some(account), Some(kind)) => {
                    format!("{account}, the {kind} account {companion} held")
                }
                (Some(account), None) => format!("{account}, the account {companion} held"),
                _ => format!("the account {companion} held"),
            };
            (format!("revoked {what}"), format!("revoke {what}"))
        }
        (_, "revoke_at_provider") => (
            format!("revoked {the_account} at {provider} too"),
            format!("revoke {the_account} at {provider} too"),
        ),
        (_, "renew_credential") => (
            format!("kept {the_account} signed in for {companion}{with_kind}"),
            format!("keep {the_account} signed in for {companion}{with_kind}"),
        ),
        (_, "end_unfinished_renewal") => (
            format!("ended an unfinished renewal of {the_account} for {companion}"),
            format!("end an unfinished renewal of {the_account} for {companion}"),
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
    let sign_in = matches!(command, "begin_sign_in" | "complete_sign_in");
    let again = format!("so {companion} needs signing in again");
    let ended: String = match (command, outcome) {
        (_, "done") => return stopped(format!("{who} {done}")),
        (_, "accepted") => return format!("{who} {done}; the service finishes it later."),
        (_, "pending") => return stopped(format!("{who} set out to {attempt}")),
        ("renew_credential", "needs_sign_in") => {
            format!("{provider} ended that sign-in, {again}")
        }
        ("renew_credential", "outcome_unknown") => {
            format!("the lobby cannot tell whether {provider} renewed it, {again}")
        }
        ("renew_credential", "refused") => {
            format!("{provider} answered with what the lobby cannot keep, {again}")
        }
        ("renew_credential", "rate_limited") => {
            format!("{provider} asked the lobby to slow down; it tries again later")
        }
        ("renew_credential", "temporarily_unavailable") => {
            format!("{provider} could not be reached; it tries again later")
        }
        ("revoke_at_provider", "outcome_unknown") => format!(
            "it waited for a sign-in, so {provider} was sent the last key the lobby held, which a renewal may already have replaced; the lobby cannot tell whether it is revoked there"
        ),
        ("revoke_at_provider", "unsupported") => {
            format!("{provider} offers no way to revoke it there, so it ends there when it expires")
        }
        ("revoke_at_provider", _) => {
            format!("{provider} did not confirm it, so it ends there when it expires")
        }
        ("complete_sign_in", "refused" | "invalid_input") => {
            "the answer from the sign-in page did not prove it".to_owned()
        }
        ("complete_sign_in", "conflict") => {
            "that account belongs to another companion, or another account waits to be signed in again".to_owned()
        }
        ("complete_sign_in", "not_found") => "the companion was removed meanwhile".to_owned(),
        ("begin_sign_in", "not_found") => "no account answers to what was typed".to_owned(),
        ("begin_sign_in", "unsupported") => {
            "the provider does not offer the sign-in the lobby requires".to_owned()
        }
        ("begin_sign_in", "conflict") => {
            "a sign-in for it was already waiting, or it is signed in with that kind already".to_owned()
        }
        ("grant_connection", "unsupported") => {
            format!("the account cannot give {service} the proof it asks for")
        }
        ("grant_connection", "needs_sign_in") => "the account needs signing in again".to_owned(),
        ("grant_connection", "refused") => format!("{service} refused it"),
        ("grant_connection", "conflict") => "that name is taken already".to_owned(),
        ("grant_connection", "revoked") => "the account was revoked meanwhile".to_owned(),
        (_, "refused") => "it was refused".to_owned(),
        (_, "not_found") => "what it named was not found".to_owned(),
        (_, "conflict") => "it clashed with a change made first".to_owned(),
        (_, "invalid_input") => "what was asked for was refused".to_owned(),
        (_, "needs_sign_in") => "the account needs signing in again".to_owned(),
        (_, "needs_permission") => "the account lacks the permission".to_owned(),
        (_, "revoked") => "it had been revoked".to_owned(),
        (_, "temporarily_unavailable") if sign_in => {
            "the sign-in page could not be reached".to_owned()
        }
        (_, "rate_limited") if sign_in => {
            "the sign-in page asked the lobby to slow down".to_owned()
        }
        (_, "temporarily_unavailable") => "it could not be reached".to_owned(),
        (_, "rate_limited") => "it was asked to slow down".to_owned(),
        (_, "outcome_unknown") => "whether it landed is unknown".to_owned(),
        (_, "unsupported") => "it is not offered".to_owned(),
        _ => "something went wrong inside the lobby".to_owned(),
    };
    stopped(format!("{who} tried to {attempt}, but {ended}"))
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

    #[test]
    fn a_companion_whose_name_is_its_moniker_is_named_once() {
        assert_eq!(named(&ada()), "Ada (moniker ada)");
        let mut plain = ada();
        plain.display_name = DisplayName::new("ada").expect("name");
        assert_eq!(named(&plain), "ada");
        assert!(
            reach(&plain, &[], &[], &[])
                .rendered()
                .starts_with("You are ada, attached through Companion Lobby.")
        );
    }

    #[test]
    fn an_argument_a_tool_does_not_take_is_named_with_the_ones_it_takes() {
        let tool = ToolDescriptor {
            name: ToolName::new("post").expect("tool"),
            title: "Post".to_owned(),
            description: "Posts.".to_owned(),
            input_schema: json!({ "type": "object", "properties": { "text": { "type": "string" } } }),
            effect: Effect::Write {
                retry: lobby_contracts::Retry::ReadFirst,
            },
        };
        let mut arguments = Map::new();
        arguments.insert("text".to_owned(), json!("Hello."));
        arguments.insert("at".to_owned(), json!("front"));
        assert_eq!(unknown_argument("board_post", &tool, &arguments), None);
        arguments.insert("txt".to_owned(), json!("Hello."));
        assert_eq!(
            unknown_argument("board_post", &tool, &arguments).as_deref(),
            Some("board_post takes no argument called \"txt\"; it takes text and at.")
        );
        let mut long = Map::new();
        long.insert("x\n".repeat(100), json!(1));
        let said = unknown_argument("board_post", &tool, &long).expect("refused");
        assert!(!said.contains('\n') && said.len() < 300, "{said}");
    }

    fn board(labels: &[&str]) -> Reached {
        Reached {
            service: ServiceName::new("board").expect("service"),
            title: "Board".to_owned(),
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
                .map(|(port, label)| ReachedAt {
                    label: Label::new(label).expect("label"),
                    address: Address::parse(
                        &format!("http://127.0.0.1:{}", 5300 + port),
                        lobby_contracts::AddressPolicy::LoopbackAllowed,
                    )
                    .expect("address"),
                    allows: vec![ToolName::new("read").expect("tool")],
                    account: Some(AccountLabel::new("ada").expect("label")),
                    known: Some(KnownAs {
                        name: Some("ada".to_owned()),
                        reference: Some((port + 1).to_string()),
                        reference_is: "member number",
                        allows: "read the board, but not post on it",
                    }),
                })
                .collect(),
        }
    }

    fn passbook() -> HeldAccount {
        HeldAccount {
            label: AccountLabel::new("ada").expect("label"),
            kind: ProviderKind::new("passbook").expect("kind"),
            title: "Passbook".to_owned(),
            waits: false,
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
        let answer = reach(&ada(), &[], &[], &[]);
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
        let answer = reach(&ada(), &[board(&["front", "back"])], &[], &[passbook()]);
        let waiting = reach(
            &ada(),
            &[],
            &[(
                ServiceName::new("board").expect("service"),
                "Board".to_owned(),
                Label::new("front").expect("label"),
            )],
            &[],
        );
        assert!(
            waiting.rendered().contains(
                "cannot reach Board at front until the sponsor signs its account in again"
            ),
            "{}",
            waiting.rendered()
        );
        assert_eq!(
            waiting.structured.expect("structured")["waiting"],
            json!([{ "service": "board", "title": "Board", "at": "front" }])
        );
        let structured = answer.structured.clone().expect("structured");
        assert_eq!(structured["tools"], json!(["reach", "board_read"]));
        assert_eq!(structured["connections"][1]["at"], "back");
        assert_eq!(
            structured["connections"][1]["startWith"], "board_read",
            "the structured answer names where to start, as the text does"
        );
        assert_eq!(structured["connections"][1]["title"], "Board");
        assert_eq!(
            structured["connections"][1]["allows"],
            "read the board, but not post on it"
        );
        let said = answer.rendered();
        assert!(
            said.contains(
                "Ada reaches Board at front (http://127.0.0.1:5300) with the account \"ada\"; Board knows it there as \"ada\" (member number \"1\") and lets it read the board, but not post on it: start with board_read. Ada reaches Board at back (http://127.0.0.1:5301)"
            ),
            "{said}"
        );
        assert!(said.contains("Every Board tool takes at"), "{said}");
    }

    #[test]
    fn at_is_described_by_the_service_title_never_its_tool_prefix() {
        let mut place = board(&["front"]);
        place.title = "Notice Board".to_owned();
        let tools = catalog(&[place]);
        let described = tools[1].1.input_schema["properties"]["at"]["description"]
            .as_str()
            .expect("described")
            .to_owned();
        assert_eq!(
            described,
            "Which Notice Board to act at, by the name the sponsor gave it; it may be left out."
        );
    }

    #[test]
    fn reach_names_the_accounts_how_the_service_knows_the_companion_and_what_it_allows() {
        let mut place = board(&["front"]);
        let write = ToolDescriptor {
            name: ToolName::new("post").expect("tool"),
            title: "Post".to_owned(),
            description: "Posts.".to_owned(),
            input_schema: json!({ "type": "object" }),
            effect: Effect::Write {
                retry: lobby_contracts::Retry::Safe,
            },
        };
        place.tools.push(write);
        place.connections[0].allows = place.tools.iter().map(|tool| tool.name.clone()).collect();
        place.connections[0].known = Some(KnownAs {
            name: Some("Ada \"the bold\"".to_owned()),
            reference: Some("7".to_owned()),
            reference_is: "member number",
            allows: "read the board and post on it",
        });
        let mut waiting = passbook();
        waiting.label = AccountLabel::new("ada.example.test").expect("label");
        waiting.title = "Atmosphere".to_owned();
        waiting.waits = true;
        let answer = reach(
            &ada(),
            std::slice::from_ref(&place),
            &[],
            &[passbook(), waiting],
        );
        assert_eq!(
            answer.rendered(),
            "You are Ada (moniker ada), attached through Companion Lobby. Ada holds the Passbook account \"ada\" and the Atmosphere account \"ada.example.test\", which waits for the sponsor to sign it in again. Ada reaches Board at front (http://127.0.0.1:5300) with the account \"ada\"; Board knows it there as \"Ada \\\"the bold\\\"\" (member number \"7\") and lets it read the board and post on it: start with board_read."
        );
        let structured = answer.structured.expect("structured");
        assert_eq!(
            structured["accounts"],
            json!([
                { "label": "ada", "kind": "passbook", "title": "Passbook", "waitsForSignIn": false },
                { "label": "ada.example.test", "kind": "passbook", "title": "Atmosphere", "waitsForSignIn": true },
            ])
        );
        let connection = &structured["connections"][0];
        assert_eq!(connection["account"], "ada");
        assert_eq!(
            connection["knownAs"],
            json!({ "referenceIs": "member number", "quoted": { "name": "Ada \"the bold\"", "reference": "7" } })
        );
        assert_eq!(connection["allows"], "read the board and post on it");
        assert!(connection.get("can").is_none());

        // A connection whose adapter could not read what was issued says
        // only where it is and where to start.
        place.connections[0].known = None;
        place.connections[0].account = None;
        assert_eq!(
            reach(&ada(), std::slice::from_ref(&place), &[], &[]).rendered(),
            "You are Ada (moniker ada), attached through Companion Lobby. Ada reaches Board at front (http://127.0.0.1:5300): start with board_read."
        );
        assert_eq!(
            listed(&["a".to_owned(), "b".to_owned(), "c".to_owned()]),
            "a, b and c"
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
                json!({ "initiator": "mind", "attachment": mind, "companion": ada, "connection": front, "service": "board", "serviceTitle": "Board", "tool": "read", "toolTitle": "Read the board", "kind": "settled", "outcome": "done" }),
                "A mind as Ada used Read the board on Board at front.",
            ),
            (
                json!({ "initiator": "sponsor", "companion": ada, "connection": front, "service": "board", "kind": "command", "command": "revoke_connection", "outcome": "done" }),
                "You revoked the connection of Ada to Board at front.",
            ),
            (
                json!({ "initiator": "sponsor", "companion": ada, "kind": "command", "command": "begin_sign_in", "outcome": "conflict" }),
                "You tried to start signing Ada in, but a sign-in for it was already waiting, or it is signed in with that kind already.",
            ),
            (
                json!({ "initiator": "sponsor", "companion": ada, "companionName": "Ada", "kindTitle": "Atmosphere", "accountLabel": "ada.example.test", "kind": "command", "command": "complete_sign_in", "outcome": "done" }),
                "You signed Ada in with Atmosphere as ada.example.test.",
            ),
            (
                json!({ "initiator": "sponsor", "companion": gone, "companionName": "Ada", "kindTitle": "Atmosphere", "accountLabel": "ada.example.test", "kind": "command", "command": "revoke_credential", "outcome": "done" }),
                "You revoked ada.example.test, the Atmosphere account Ada held.",
            ),
            (
                json!({ "initiator": "sponsor", "companion": ada, "companionName": "Ada", "kindTitle": "Atmosphere", "accountLabel": "ada.example.test", "kind": "command", "command": "revoke_at_provider", "outcome": "unsupported" }),
                "You tried to revoke ada.example.test at Atmosphere too, but Atmosphere offers no way to revoke it there, so it ends there when it expires.",
            ),
            (
                json!({ "initiator": "lobby", "companion": ada, "companionName": "Ada", "kindTitle": "Atmosphere", "accountLabel": "ada.example.test", "kind": "command", "command": "renew_credential", "outcome": "done" }),
                "The lobby kept ada.example.test signed in for Ada with Atmosphere.",
            ),
            (
                json!({ "initiator": "lobby", "companion": ada, "companionName": "Ada", "kindTitle": "Atmosphere", "accountLabel": "ada.example.test", "kind": "command", "command": "renew_credential", "outcome": "needs_sign_in" }),
                "The lobby tried to keep ada.example.test signed in for Ada with Atmosphere, but Atmosphere ended that sign-in, so Ada needs signing in again.",
            ),
            (
                json!({ "initiator": "lobby", "companion": ada, "companionName": "Ada", "kindTitle": "Atmosphere", "accountLabel": "ada.example.test", "kind": "command", "command": "renew_credential", "outcome": "outcome_unknown" }),
                "The lobby tried to keep ada.example.test signed in for Ada with Atmosphere, but the lobby cannot tell whether Atmosphere renewed it, so Ada needs signing in again.",
            ),
            (
                json!({ "initiator": "lobby", "companion": ada, "companionName": "Ada", "kindTitle": "Atmosphere", "accountLabel": "ada.example.test", "kind": "command", "command": "end_unfinished_renewal", "outcome": "done" }),
                "The lobby stopped while it renewed ada.example.test for Ada with Atmosphere and cannot tell whether Atmosphere renewed it, so Ada needs signing in again.",
            ),
            (
                json!({ "initiator": "sponsor", "companion": gone, "companionName": "Ada", "kind": "command", "command": "remove_companion", "outcome": "done" }),
                "You removed Ada.",
            ),
            (
                json!({ "initiator": "mind", "attachment": mind, "companion": ada, "companionName": "Ada Lovelace", "connection": front, "connectionLabel": "back", "service": "board", "serviceTitle": "Board", "tool": "read", "kind": "settled", "outcome": "done" }),
                "A mind as Ada Lovelace called read on Board at back.",
            ),
            (
                json!({ "initiator": "lobby", "companion": gone, "kind": "command", "command": "detach", "outcome": "done" }),
                "The lobby ended an attachment as a companion since removed.",
            ),
            (
                json!({ "initiator": "sponsor", "companion": gone, "kind": "command", "command": "remove_companion", "outcome": "done" }),
                "You removed a companion.",
            ),
            (
                json!({ "initiator": "sponsor", "companion": ada, "kind": "command", "command": "begin_sign_in", "outcome": "temporarily_unavailable" }),
                "You tried to start signing Ada in, but the sign-in page could not be reached.",
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
