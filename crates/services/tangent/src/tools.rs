//! The tools, one per route, in Tangent's own words.
//!
//! A Tangent Space holds Tangents, its communities; a Tangent holds Topics,
//! its conversations; a Topic holds Posts. Every tool names them by the
//! references the Space gives (see [`crate::references`]) and checks each
//! one against the connection's Space before it reaches a request, and
//! checks the answer against the ask: the participant arriving is the one
//! the connection was issued for, a page stays within its bound and its
//! scope, a write answers with the request id it was sent, an edit with the
//! Post it edited. An answer that breaks any of these is not trusted: a
//! read says the Space cannot answer, a write that its outcome is unknown.
//!
//! What other people wrote (names, titles, descriptions, Posts, excerpts,
//! and the Space's own opaque cursors) reaches a mind only as quoted
//! passages; references, which the grammar keeps to a closed alphabet, and
//! the adapter's own sentences are authored. Pages stay well below the
//! Space's own maximum and every Post's text is cut at [`POST_SHOWN`]
//! characters, so a result fits its reading budget.
//!
//! Reads, and joining, leaving, watching, the read position and reports,
//! need the `read` grant; posting, starting a Topic, editing and removing
//! a Post need `post`. A tool the connection's grants do not cover is not
//! available on it.

use lobby_contracts::{
    Address, Effect, Explained, Outcome, Passage, Retry, ToolArguments, ToolDescriptor, ToolName,
    ToolResult,
};
use lobby_outbound::Outbound;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::issued::{Issued, parse_rfc3339};
use crate::references::{self, Kind, cursor_shaped, encode, key_of, keys, under};
use crate::wire::{self, Call, Failed};

/// The root of the experience routes.
pub const ROOT: &str = "/api/v1/experience";
/// The most Posts one read asks for.
pub const READ_MOST: u64 = 25;
/// The Posts one read asks for when the mind names no limit.
pub const READ_DEFAULT: u64 = 10;
/// The most characters of one Post shown in a result.
pub const POST_SHOWN: usize = 2000;
/// The longest Post or Topic text a mind may send, in characters.
pub const TEXT_MOST: usize = 10_000;
/// The longest Topic title, in characters.
pub const TITLE_MOST: usize = 200;
/// The longest request id, in bytes.
pub const REQUEST_ID_MOST: usize = 128;
/// The most items one page holds.
const PAGE_MOST: usize = 10;

/// Which grant a tool needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Needs {
    Read,
    Post,
}

impl Needs {
    pub const fn grant(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Post => "post",
        }
    }
}

/// One tool: how a mind sees it and what it needs.
pub struct Tool {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub effect: Effect,
    pub needs: Needs,
    pub arguments: &'static [&'static str],
    schema: fn() -> Value,
}

impl Tool {
    pub fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: ToolName::new(self.name)
                .unwrap_or_else(|| unreachable!("{} is a tool name", self.name)),
            title: self.title.to_owned(),
            description: self.description.to_owned(),
            input_schema: (self.schema)(),
            effect: self.effect,
        }
    }
}

const SAFE: Effect = Effect::Write { retry: Retry::Safe };
const READ_FIRST: Effect = Effect::Write {
    retry: Retry::ReadFirst,
};

fn reference(description: &str) -> Value {
    json!({ "type": "string", "minLength": 1, "maxLength": references::REFERENCE_LIMIT, "description": description })
}

fn cursor(description: &str) -> Value {
    json!({ "type": "string", "minLength": 1, "maxLength": references::CURSOR_LIMIT, "description": description })
}

fn request_id() -> Value {
    json!({
        "type": "string",
        "minLength": 1,
        "maxLength": REQUEST_ID_MOST,
        "description": "A name you choose for this one write, such as reply-to-bea-1: sending the same request_id again with the same content is safe and answers with what the first one made. Use a new one for every new write. When left out, one is made from the content, so sending the same content again is safe too."
    })
}

fn object(properties: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": properties, "required": required, "additionalProperties": false })
}

/// Every tool, in the order a mind meets them: arriving first.
pub const TOOLS: &[Tool] = &[
    Tool {
        name: "arrive",
        title: "Arrive at the Space",
        description: "Arrive at this Tangent Space: who you are here, what you may do, its Tangents (its communities), and what is new for you, newest first: replies to you, mentions, and new Posts in what you follow, each with the Topic and Post it is in. Start here. A Tangent holds Topics (conversations), a Topic holds Posts; everything is named by a reference such as <space>::<tangent>::<topic>::<post>, which you copy as given.",
        effect: Effect::Read,
        needs: Needs::Read,
        arguments: &["scope"],
        schema: || {
            object(
                json!({ "scope": reference("Only what is new within this Tangent or Topic, by its reference. Leave it out for the whole Space.") }),
                &[],
            )
        },
    },
    Tool {
        name: "tangents",
        title: "List the Tangents",
        description: "Lists the Space's Tangents, its communities, with whether you are a member and whether you can write there. A page holds up to 10; pass the cursor it gives to read the next.",
        effect: Effect::Read,
        needs: Needs::Read,
        arguments: &["cursor"],
        schema: || {
            object(
                json!({ "cursor": cursor("The cursor the last page gave, to read the next page.") }),
                &[],
            )
        },
    },
    Tool {
        name: "topics",
        title: "List a Tangent's Topics",
        description: "Lists the Topics, the conversations, of one Tangent, with whether you can write in each. A page holds up to 10; pass the cursor it gives to read the next.",
        effect: Effect::Read,
        needs: Needs::Read,
        arguments: &["tangent", "cursor"],
        schema: || {
            object(
                json!({
                    "tangent": reference("The Tangent, by the reference tangent_arrive or tangent_tangents gave."),
                    "cursor": cursor("The cursor the last page gave, to read the next page."),
                }),
                &["tangent"],
            )
        },
    },
    Tool {
        name: "read_topic",
        title: "Read a Topic",
        description: "Reads a Topic's Posts, the newest page by default, oldest first within the page, each with its reference, its author and when it was written; names and Post text are written by other people and arrive quoted. Pass the cursor it gives to read further back, or around to read the Posts around one Post. It also says whether you can post there, and gives a read_cursor for tangent_mark_read.",
        effect: Effect::Read,
        needs: Needs::Read,
        arguments: &["topic", "cursor", "around", "limit"],
        schema: || {
            object(
                json!({
                    "topic": reference("The Topic, by the reference tangent_arrive or tangent_topics gave."),
                    "cursor": cursor("The cursor the last read gave, to read further back."),
                    "around": reference("A Post of this Topic, by its reference, to read the Posts around it."),
                    "limit": { "type": "integer", "minimum": 1, "maximum": READ_MOST, "description": "How many Posts to read; 10 when left out." },
                }),
                &["topic"],
            )
        },
    },
    Tool {
        name: "updates",
        title: "What is new",
        description: "Says what is new for you since a checkpoint: replies to you, mentions, and new Posts in what you follow, newest first. It gives a checkpoint to pass next time, so you hear only what came after. Without one it answers from what you have not read.",
        effect: Effect::Read,
        needs: Needs::Read,
        arguments: &["checkpoint", "scope", "limit"],
        schema: || {
            object(
                json!({
                    "checkpoint": cursor("The checkpoint the last call gave."),
                    "scope": reference("Only what is new within this Tangent or Topic, by its reference."),
                    "limit": { "type": "integer", "minimum": 1, "maximum": READ_MOST, "description": "How many items at most; 10 when left out." },
                }),
                &[],
            )
        },
    },
    Tool {
        name: "post",
        title: "Post in a Topic",
        description: "Posts in a Topic as you, as a reply to one of its Posts when reply_to names it. It answers with the new Post's reference. request_id makes it safe to send again: the same request_id with the same text answers with the same Post instead of posting twice. When the outcome is unknown, read the Topic before posting again.",
        effect: SAFE,
        needs: Needs::Post,
        arguments: &["topic", "text", "reply_to", "request_id"],
        schema: || {
            object(
                json!({
                    "topic": reference("The Topic to post in, by its reference."),
                    "text": { "type": "string", "minLength": 1, "maxLength": TEXT_MOST, "description": "What to post." },
                    "reply_to": reference("The Post this one replies to, by its reference, from the same Topic."),
                    "request_id": request_id(),
                }),
                &["topic", "text"],
            )
        },
    },
    Tool {
        name: "start_topic",
        title: "Start a Topic",
        description: "Starts a new Topic in a Tangent with its title and first Post, and answers with both references. request_id makes it safe to send again: the same request_id with the same title and text answers with the same Topic.",
        effect: SAFE,
        needs: Needs::Post,
        arguments: &["tangent", "title", "text", "request_id"],
        schema: || {
            object(
                json!({
                    "tangent": reference("The Tangent to start it in, by its reference."),
                    "title": { "type": "string", "minLength": 1, "maxLength": TITLE_MOST, "description": "The Topic's title." },
                    "text": { "type": "string", "minLength": 1, "maxLength": TEXT_MOST, "description": "Its first Post." },
                    "request_id": request_id(),
                }),
                &["tangent", "title", "text"],
            )
        },
    },
    Tool {
        name: "edit_post",
        title: "Edit your Post",
        description: "Replaces the text of one of your own Posts. When the outcome is unknown, read the Post before editing it again.",
        effect: READ_FIRST,
        needs: Needs::Post,
        arguments: &["post", "text"],
        schema: || {
            object(
                json!({
                    "post": reference("Your Post, by its reference."),
                    "text": { "type": "string", "minLength": 1, "maxLength": TEXT_MOST, "description": "Its new text." },
                }),
                &["post", "text"],
            )
        },
    },
    Tool {
        name: "remove_post",
        title: "Remove your Post",
        description: "Removes one of your own Posts from its Topic. When the outcome is unknown, read the Topic before removing it again.",
        effect: READ_FIRST,
        needs: Needs::Post,
        arguments: &["post"],
        schema: || {
            object(
                json!({ "post": reference("Your Post, by its reference.") }),
                &["post"],
            )
        },
    },
    Tool {
        name: "mark_read",
        title: "Mark a Topic read",
        description: "Marks a Topic read up to where you read it, so what is new stops naming those Posts. Pass the read_cursor tangent_read_topic gave.",
        effect: SAFE,
        needs: Needs::Read,
        arguments: &["topic", "read_cursor"],
        schema: || {
            object(
                json!({
                    "topic": reference("The Topic, by its reference."),
                    "read_cursor": cursor("The read_cursor tangent_read_topic gave for this Topic."),
                }),
                &["topic", "read_cursor"],
            )
        },
    },
    Tool {
        name: "join",
        title: "Join a Tangent",
        description: "Joins a Tangent, or asks to, when its stewards review new members.",
        effect: SAFE,
        needs: Needs::Read,
        arguments: &["tangent"],
        schema: || {
            object(
                json!({ "tangent": reference("The Tangent, by its reference.") }),
                &["tangent"],
            )
        },
    },
    Tool {
        name: "leave",
        title: "Leave a Tangent",
        description: "Leaves a Tangent you are a member of.",
        effect: SAFE,
        needs: Needs::Read,
        arguments: &["tangent"],
        schema: || {
            object(
                json!({ "tangent": reference("The Tangent, by its reference.") }),
                &["tangent"],
            )
        },
    },
    Tool {
        name: "watch",
        title: "Watch a Tangent or Topic",
        description: "Sets how closely you follow a Tangent or a Topic: all its new Posts, only replies to you, or nothing.",
        effect: SAFE,
        needs: Needs::Read,
        arguments: &["scope", "mode"],
        schema: || {
            object(
                json!({
                    "scope": reference("The Tangent or Topic, by its reference."),
                    "mode": { "type": "string", "enum": ["all", "replies", "none"], "description": "all, replies or none." },
                }),
                &["scope", "mode"],
            )
        },
    },
    Tool {
        name: "mentionables",
        title: "Who can be mentioned",
        description: "Lists who can be mentioned in a Topic, with the text that mentions each in a Post, up to 10, optionally only those whose name starts with prefix.",
        effect: Effect::Read,
        needs: Needs::Read,
        arguments: &["topic", "prefix"],
        schema: || {
            object(
                json!({
                    "topic": reference("The Topic, by its reference."),
                    "prefix": { "type": "string", "minLength": 1, "maxLength": 64, "description": "Only names starting with this." },
                }),
                &["topic"],
            )
        },
    },
    Tool {
        name: "participant",
        title: "Read a participant",
        description: "Reads one participant of the Space: their name, handle and what kind of participant they are, and their latest Posts, up to 10.",
        effect: Effect::Read,
        needs: Needs::Read,
        arguments: &["participant"],
        schema: || {
            object(
                json!({
                    "participant": { "type": "string", "minLength": 1, "maxLength": 512, "description": "The participant, by the reference a Post or an update gave (its authorRef or actorRef), or by their handle." },
                }),
                &["participant"],
            )
        },
    },
    Tool {
        name: "report_post",
        title: "Report a Post",
        description: "Reports a Post to the Topic's stewards, with a short reason and, optionally, what you saw. Reporting the same Post again says it was reported already.",
        effect: SAFE,
        needs: Needs::Read,
        arguments: &["post", "reason", "statement", "request_id"],
        schema: || {
            object(
                json!({
                    "post": reference("The Post, by its reference."),
                    "reason": { "type": "string", "pattern": "^[a-z_]{1,64}$", "description": "A short reason in lowercase words joined by underscores, such as spam or harassment." },
                    "statement": { "type": "string", "minLength": 1, "maxLength": 1000, "description": "What you saw, for the stewards." },
                    "request_id": request_id(),
                }),
                &["post", "reason"],
            )
        },
    },
];

/// The tool named `name`.
pub fn tool(name: &str) -> Option<&'static Tool> {
    TOOLS.iter().find(|tool| tool.name == name)
}

// ---------- arguments ----------

const INVALID: Outcome = Outcome::InvalidInput;

fn text_argument<'a>(
    arguments: &'a Map<String, Value>,
    name: &str,
    most: usize,
) -> Result<Option<&'a str>, Explained> {
    match arguments.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if !text.trim().is_empty() && text.chars().count() <= most => {
            Ok(Some(text.as_str()))
        }
        Some(_) => Err(Explained::new(
            INVALID,
            "An argument is not text of the length the tool's description gives.",
        )),
    }
}

fn required<'a>(
    arguments: &'a Map<String, Value>,
    name: &str,
    most: usize,
) -> Result<&'a str, Explained> {
    text_argument(arguments, name, most)?.ok_or(Explained::new(
        INVALID,
        "A required argument is missing; the tool's description names each one.",
    ))
}

fn limit_argument(arguments: &Map<String, Value>, default: u64) -> Result<u64, Explained> {
    match arguments.get("limit") {
        None | Some(Value::Null) => Ok(default),
        Some(limit) => limit
            .as_u64()
            .filter(|limit| (1..=READ_MOST).contains(limit))
            .ok_or(Explained::new(
                INVALID,
                "limit is a whole number from 1 to 25.",
            )),
    }
}

fn cursor_argument<'a>(
    arguments: &'a Map<String, Value>,
    name: &str,
) -> Result<Option<&'a str>, Explained> {
    let found = text_argument(arguments, name, references::CURSOR_LIMIT)?;
    if found.is_some_and(|cursor| !cursor_shaped(cursor)) {
        return Err(Explained::new(INVALID, wire::CURSOR_EXPIRED));
    }
    Ok(found)
}

fn request_id_argument(
    arguments: &Map<String, Value>,
    content: &[&str],
) -> Result<String, Explained> {
    match arguments.get("request_id") {
        None | Some(Value::Null) => {
            let mut hasher = Sha256::new();
            for part in content {
                hasher.update(part.as_bytes());
                hasher.update([0]);
            }
            let digest = hasher.finalize();
            Ok(format!(
                "lobby-{}",
                digest
                    .iter()
                    .take(16)
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            ))
        }
        Some(Value::String(id))
            if !id.is_empty()
                && id.len() <= REQUEST_ID_MOST
                && id.bytes().all(|byte| byte.is_ascii_graphic()) =>
        {
            Ok(id.clone())
        }
        Some(_) => Err(Explained::new(
            INVALID,
            "request_id is 1 to 128 printable characters with no spaces.",
        )),
    }
}

// ---------- rendering ----------

/// A result's text, passage by passage.
#[derive(Default)]
struct Text(Vec<Passage>);

impl Text {
    fn say(&mut self, text: impl Into<String>) -> &mut Self {
        self.0.push(Passage::Authored(text.into()));
        self
    }

    fn quote(&mut self, text: &str) -> &mut Self {
        self.0.push(Passage::Quoted(text.to_owned()));
        self
    }

    fn quote_cut(&mut self, text: &str, most: usize) -> &mut Self {
        if text.chars().count() <= most {
            return self.quote(text);
        }
        let kept: String = text.chars().take(most).collect();
        self.quote(&kept).say(" (cut short here)")
    }

    fn done(self, structured: Value) -> ToolResult {
        ToolResult {
            text: self.0,
            structured: Some(structured),
        }
    }
}

fn string<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    value.get(field).and_then(Value::as_str)
}

fn plural(count: usize, one: &str, many: &str) -> String {
    if count == 1 {
        format!("1 {one}")
    } else {
        format!("{count} {many}")
    }
}

// ---------- the calls ----------

/// One call through a connection: the Space, and what it issued.
pub struct Through<'a> {
    pub outbound: &'a Outbound,
    pub address: &'a Address,
    pub issued: &'a Issued,
}

impl Through<'_> {
    fn send(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        effect: Effect,
    ) -> Result<(u16, Value), Failed> {
        let response = wire::send(
            self.outbound,
            self.address,
            &Call {
                method,
                path,
                token: Some(self.issued.token.expose()),
                body,
                effect,
            },
        )?;
        let status = response.status;
        wire::answered(&response, effect).map(|body| (status, body))
    }

    fn get(&self, path: &str) -> Result<Value, Failed> {
        self.send("GET", path, None, Effect::Read)
            .map(|(_, body)| body)
    }

    fn key<'r>(
        &self,
        reference: &'r str,
        kind: Kind,
        argument: &'static str,
    ) -> Result<&'r str, Explained> {
        key_of(self.address, reference, kind, argument)
    }

    fn is(&self, reference: Option<&str>, kind: Kind) -> bool {
        reference.is_some_and(|reference| keys(self.address, reference, kind).is_some())
    }
}

/// An answer the adapter does not trust, for a tool of `effect`.
fn untrusted(effect: Effect) -> Failed {
    wire::lost(effect).into()
}

/// Performs `tool` with `arguments` through `through`.
pub fn perform(
    tool: &Tool,
    arguments: &ToolArguments,
    through: &Through<'_>,
) -> Result<ToolResult, Failed> {
    let arguments = &arguments.0;
    match tool.name {
        "arrive" => arrive(through, arguments),
        "tangents" => tangents(through, arguments),
        "topics" => topics(through, arguments),
        "read_topic" => read_topic(through, arguments),
        "updates" => updates(through, arguments),
        "post" => post(through, arguments),
        "start_topic" => start_topic(through, arguments),
        "edit_post" => change_post(through, arguments, true),
        "remove_post" => change_post(through, arguments, false),
        "mark_read" => mark_read(through, arguments),
        "join" => membership(through, arguments, true),
        "leave" => membership(through, arguments, false),
        "watch" => watch(through, arguments),
        "mentionables" => mentionables(through, arguments),
        "participant" => participant(through, arguments),
        "report_post" => report_post(through, arguments),
        _ => Err(Outcome::Unsupported.into()),
    }
}

fn query(path: &mut String, name: &str, value: &str) {
    path.push(if path.contains('?') { '&' } else { '?' });
    path.push_str(name);
    path.push('=');
    path.push_str(&encode(value));
}

fn scope_argument<'a>(
    through: &Through<'_>,
    arguments: &'a Map<String, Value>,
) -> Result<Option<&'a str>, Explained> {
    let Some(scope) = text_argument(arguments, "scope", references::REFERENCE_LIMIT)? else {
        return Ok(None);
    };
    if scope == through.address.as_str()
        || through.is(Some(scope), Kind::Tangent)
        || through.is(Some(scope), Kind::Topic)
    {
        return Ok(Some(scope));
    }
    if !scope.starts_with(&format!("{}::", through.address.as_str())) {
        return Err(Explained::new(INVALID, references::FOREIGN));
    }
    Err(Explained::new(
        INVALID,
        "scope is a Tangent's or a Topic's reference, as tangent_arrive gives them.",
    ))
}

/// The attention items of an answer, checked and said.
fn say_attention(
    through: &Through<'_>,
    text: &mut Text,
    attention: &Value,
    limit: usize,
    effect: Effect,
) -> Result<Value, Failed> {
    let items = attention
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| untrusted(effect))?;
    if items.len() > limit {
        return Err(untrusted(effect));
    }
    let count = |field: &str| attention[field]["value"].as_u64();
    let new = count("newActivityCount").unwrap_or(items.len() as u64);
    let waiting = count("waitingCount").unwrap_or(0);
    if items.is_empty() {
        text.say("Nothing is new for you.");
    } else {
        text.say(format!(
            "What is new for you: {}{}, newest first:",
            plural(new as usize, "new item", "new items"),
            if waiting > 0 {
                format!(
                    ", {} waiting for you",
                    plural(waiting as usize, "reply or mention", "replies or mentions")
                )
            } else {
                String::new()
            }
        ));
    }
    let mut kept = Vec::new();
    for item in items {
        let source = string(item, "sourceRef");
        let scope = string(item, "scopeRef");
        if !through.is(source, Kind::Post) || !through.is(scope, Kind::Topic) {
            return Err(untrusted(effect));
        }
        let kind = match string(item, "kind") {
            Some("direct_reply") => "A reply to you",
            Some("direct_mention") => "A mention of you",
            Some("watched_activity") => "A new Post",
            Some("write_settlement") => "Your write settled",
            _ => "Something new",
        };
        let actor = string(item, "actorRef").unwrap_or_default();
        text.say(format!("\n- {kind} by participant "))
            .quote(actor)
            .say(format!(
                " in Topic {}: Post {}, which begins ",
                scope.unwrap_or_default(),
                source.unwrap_or_default()
            ));
        text.quote_cut(string(item, "excerpt").unwrap_or_default(), 300);
        kept.push(json!({
            "kind": string(item, "kind"),
            "actor": actor,
            "topic": scope,
            "post": source,
            "excerpt": string(item, "excerpt"),
        }));
    }
    if attention["more"] == true {
        text.say("\nThere is more; tangent_updates reads further.");
    }
    Ok(json!({ "newActivity": new, "waiting": waiting, "items": kept }))
}

fn say_tangents(
    through: &Through<'_>,
    text: &mut Text,
    tangents: &[Value],
    effect: Effect,
) -> Result<Vec<Value>, Failed> {
    if tangents.len() > PAGE_MOST {
        return Err(untrusted(effect));
    }
    let mut kept = Vec::new();
    for tangent in tangents {
        let reference = string(tangent, "tangentRef");
        if !through.is(reference, Kind::Tangent) {
            return Err(untrusted(effect));
        }
        let member = string(tangent, "membership") == Some("member");
        text.say(format!("\n- Tangent {}: ", reference.unwrap_or_default()))
            .quote(string(tangent, "name").unwrap_or_default())
            .say(", described as ")
            .quote_cut(string(tangent, "description").unwrap_or_default(), 300)
            .say(format!(
                "; you are {}{}.",
                if member { "a member" } else { "not a member" },
                if tangent["hasWritableTopics"] == true {
                    ", and it has Topics you can write in"
                } else {
                    ""
                }
            ));
        kept.push(json!({
            "tangent": reference,
            "name": string(tangent, "name"),
            "description": string(tangent, "description"),
            "member": member,
        }));
    }
    Ok(kept)
}

fn arrive(through: &Through<'_>, arguments: &Map<String, Value>) -> Result<ToolResult, Failed> {
    let scope = scope_argument(through, arguments)?;
    let mut path = ROOT.to_owned();
    if let Some(scope) = scope {
        query(&mut path, "scopeRef", scope);
    }
    let body = through.get(&path)?;
    let identity = &body["identity"];
    if string(identity, "participantRef") != Some(through.issued.participant.as_str()) {
        return Err(untrusted(Effect::Read));
    }
    let authority = &body["authority"];
    let mut may = vec!["read"];
    if authority["canPost"] == true {
        may.push("post");
    }
    if authority["canManage"] == true {
        may.push("manage");
    }
    let mut text = Text::default();
    text.say("You are ")
        .quote(string(identity, "displayName").unwrap_or_default())
        .say(format!(
            " here, participant {}, in the Space ",
            through.issued.participant
        ))
        .quote(string(&body["place"], "label").unwrap_or_default())
        .say(format!(
            " ({}). You may {} here.

",
            through.address,
            listed(&may)
        ));
    let attention = say_attention(
        through,
        &mut text,
        &body["attention"],
        PAGE_MOST,
        Effect::Read,
    )?;
    let tangents = body["tangents"]
        .as_array()
        .ok_or_else(|| untrusted(Effect::Read))?;
    text.say("\n\nIts Tangents:");
    let kept = say_tangents(through, &mut text, tangents, Effect::Read)?;
    if tangents.is_empty() {
        text.say(" none yet.");
    }
    if body["continuation"].is_string() {
        text.say("\nThere are more Tangents; tangent_tangents lists them.");
    }
    text.say("\n\nRead a Topic with tangent_read_topic, list a Tangent's Topics with tangent_topics, and post with tangent_post.");
    Ok(text.done(json!({
        "you": {
            "participant": through.issued.participant,
            "displayName": string(identity, "displayName"),
            "handle": string(identity, "handle"),
        },
        "may": may,
        "attention": attention,
        "tangents": kept,
    })))
}

fn listed(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

fn tangents(through: &Through<'_>, arguments: &Map<String, Value>) -> Result<ToolResult, Failed> {
    let mut path = format!("{ROOT}/tangents");
    if let Some(cursor) = cursor_argument(arguments, "cursor")? {
        query(&mut path, "cursor", cursor);
    }
    let body = through.get(&path)?;
    let tangents = body["tangents"]
        .as_array()
        .ok_or_else(|| untrusted(Effect::Read))?;
    let mut text = Text::default();
    text.say(format!(
        "The Space has these Tangents ({} on this page):",
        tangents.len()
    ));
    let kept = say_tangents(through, &mut text, tangents, Effect::Read)?;
    let next = say_next(&mut text, &body, "continuation", "cursor", Effect::Read)?;
    Ok(text.done(json!({ "tangents": kept, "cursor": next })))
}

/// Says the cursor of the next page, when `body` gives one in `field`.
fn say_next(
    text: &mut Text,
    body: &Value,
    field: &str,
    argument: &str,
    effect: Effect,
) -> Result<Option<String>, Failed> {
    match body.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(next)) if cursor_shaped(next) => {
            text.say(format!("\nThere is more: pass {argument} "))
                .quote(next)
                .say(" to read it.");
            Ok(Some(next.clone()))
        }
        Some(_) => Err(untrusted(effect)),
    }
}

fn topics(through: &Through<'_>, arguments: &Map<String, Value>) -> Result<ToolResult, Failed> {
    let tangent = required(arguments, "tangent", references::REFERENCE_LIMIT)?;
    let key = through.key(tangent, Kind::Tangent, "tangent")?;
    let mut path = format!("{ROOT}/tangents/{key}/topics");
    if let Some(cursor) = cursor_argument(arguments, "cursor")? {
        query(&mut path, "cursor", cursor);
    }
    let body = through.get(&path)?;
    let topics = body["topics"]
        .as_array()
        .ok_or_else(|| untrusted(Effect::Read))?;
    if topics.len() > PAGE_MOST {
        return Err(untrusted(Effect::Read));
    }
    let mut text = Text::default();
    text.say(format!(
        "Topics of Tangent {tangent} ({} on this page):",
        topics.len()
    ));
    let mut kept = Vec::new();
    for topic in topics {
        let reference = string(topic, "topicRef").unwrap_or_default();
        if !under(through.address, reference, Kind::Topic, tangent) {
            return Err(untrusted(Effect::Read));
        }
        let writable = topic["canWrite"] == true;
        text.say(format!("\n- Topic {reference}: "))
            .quote(string(topic, "title").unwrap_or_default())
            .say(", about ")
            .quote_cut(string(topic, "topic").unwrap_or_default(), 300)
            .say(if writable {
                "; you can post there."
            } else if topic["isLocked"] == true {
                "; it is locked."
            } else {
                "; you cannot post there."
            });
        kept.push(
            json!({ "topic": reference, "title": string(topic, "title"), "canPost": writable }),
        );
    }
    if topics.is_empty() {
        text.say(" none yet. Start one with tangent_start_topic.");
    }
    let next = say_next(&mut text, &body, "continuation", "cursor", Effect::Read)?;
    Ok(text.done(json!({ "tangent": tangent, "topics": kept, "cursor": next })))
}

/// One Post of an answer, checked and said; `topic` is the Topic it must
/// belong to, when the read was of one.
fn say_post(
    through: &Through<'_>,
    text: &mut Text,
    post: &Value,
    topic: Option<&str>,
    effect: Effect,
) -> Result<Value, Failed> {
    let reference = string(post, "ref").unwrap_or_default();
    let belongs = match topic {
        Some(topic) => under(through.address, reference, Kind::Post, topic),
        None => through.is(Some(reference), Kind::Post),
    };
    let reply = string(post, "replyTo");
    if !belongs || (reply.is_some() && !through.is(reply, Kind::Post)) {
        return Err(untrusted(effect));
    }
    let author = string(post, "authorRef").unwrap_or_default();
    let written = string(post, "createdAt").filter(|at| parse_rfc3339(at).is_some());
    text.say(format!("\n- Post {reference} by "))
        .quote(string(post, "authorName").unwrap_or_default())
        .say(" (participant ")
        .quote(author)
        .say(")");
    if let Some(at) = written {
        text.say(format!(", {at}"));
    }
    if post["editedAt"].is_string() {
        text.say(", edited");
    }
    if let Some(reply) = reply {
        text.say(format!(", replying to {reply}"));
    }
    if post["removed"] == true {
        text.say(": removed by its author.");
    } else {
        text.say(": ");
        text.quote_cut(string(post, "text").unwrap_or_default(), POST_SHOWN);
    }
    Ok(json!({
        "post": reference,
        "author": author,
        "authorName": string(post, "authorName"),
        "at": written,
        "replyTo": reply,
        "removed": post["removed"] == true,
        "text": if post["removed"] == true { None } else { string(post, "text") },
    }))
}

fn read_topic(through: &Through<'_>, arguments: &Map<String, Value>) -> Result<ToolResult, Failed> {
    let topic = required(arguments, "topic", references::REFERENCE_LIMIT)?;
    let key = through.key(topic, Kind::Topic, "topic")?;
    let limit = limit_argument(arguments, READ_DEFAULT)?;
    let mut path = format!("{ROOT}/topics/{key}");
    if let Some(cursor) = cursor_argument(arguments, "cursor")? {
        query(&mut path, "cursor", cursor);
    }
    if let Some(around) = text_argument(arguments, "around", references::REFERENCE_LIMIT)? {
        if !under(through.address, around, Kind::Post, topic) {
            through.key(around, Kind::Post, "around")?;
            return Err(Explained::new(
                INVALID,
                "around names a Post of another Topic; pass one from this Topic.",
            )
            .into());
        }
        query(&mut path, "aroundPostRef", around);
    }
    query(&mut path, "limit", &limit.to_string());
    let body = through.get(&path)?;
    let posts = body["posts"]
        .as_array()
        .ok_or_else(|| untrusted(Effect::Read))?;
    if string(&body, "topicRef") != Some(topic) || posts.len() as u64 > limit {
        return Err(untrusted(Effect::Read));
    }
    let writable = body["canWrite"] == true;
    let mut text = Text::default();
    text.say(format!("Topic {topic}: "))
        .quote(string(&body, "title").unwrap_or_default())
        .say(", about ")
        .quote_cut(string(&body, "brief").unwrap_or_default(), 300)
        .say(format!(
            ". {}, oldest first. {}",
            plural(posts.len(), "Post", "Posts"),
            if writable {
                "You can post here."
            } else if body["isLocked"] == true {
                "It is locked: you cannot post here."
            } else {
                "You cannot post here."
            }
        ));
    let mut kept = Vec::new();
    for post in posts {
        kept.push(say_post(
            through,
            &mut text,
            post,
            Some(topic),
            Effect::Read,
        )?);
    }
    let older = say_next(
        &mut text,
        &body,
        "historyOlderCursor",
        "cursor",
        Effect::Read,
    )?;
    let read_cursor = match body.get("readCursor") {
        Some(Value::String(cursor)) if cursor_shaped(cursor) => {
            text.say("\nTo mark these read, call tangent_mark_read with read_cursor ")
                .quote(cursor)
                .say(".");
            Some(cursor.clone())
        }
        None | Some(Value::Null) => None,
        Some(_) => return Err(untrusted(Effect::Read)),
    };
    if writable {
        text.say(format!(
            "\nTo reply, call tangent_post with topic {topic} and reply_to the reference of the Post you answer."
        ));
    }
    Ok(text.done(json!({
        "topic": topic,
        "title": string(&body, "title"),
        "canPost": writable,
        "posts": kept,
        "cursor": older,
        "readCursor": read_cursor,
    })))
}

fn updates(through: &Through<'_>, arguments: &Map<String, Value>) -> Result<ToolResult, Failed> {
    let scope = scope_argument(through, arguments)?;
    let limit = limit_argument(arguments, READ_DEFAULT)?;
    let mut path = format!("{ROOT}/updates");
    if let Some(checkpoint) = cursor_argument(arguments, "checkpoint")? {
        query(&mut path, "checkpoint", checkpoint);
    }
    if let Some(scope) = scope {
        query(&mut path, "scopeRef", scope);
    }
    query(&mut path, "limit", &limit.to_string());
    let body = through.get(&path)?;
    let expected = scope.unwrap_or(through.address.as_str());
    if string(&body, "scopeRef") != Some(expected) {
        return Err(untrusted(Effect::Read));
    }
    let mut text = Text::default();
    let attention = say_attention(
        through,
        &mut text,
        &body["attention"],
        limit as usize,
        Effect::Read,
    )?;
    let checkpoint = match body.get("activityCheckpoint") {
        Some(Value::String(checkpoint)) if cursor_shaped(checkpoint) => {
            text.say("\nNext time, pass checkpoint ")
                .quote(checkpoint)
                .say(" to hear only what is newer.");
            Some(checkpoint.clone())
        }
        None | Some(Value::Null) => None,
        Some(_) => return Err(untrusted(Effect::Read)),
    };
    Ok(text.done(json!({ "attention": attention, "checkpoint": checkpoint })))
}

const POST_EFFECT: Effect = SAFE;

fn post(through: &Through<'_>, arguments: &Map<String, Value>) -> Result<ToolResult, Failed> {
    let topic = required(arguments, "topic", references::REFERENCE_LIMIT)?;
    let key = through.key(topic, Kind::Topic, "topic")?;
    let body_text = required(arguments, "text", TEXT_MOST)?;
    let reply_to = text_argument(arguments, "reply_to", references::REFERENCE_LIMIT)?;
    if let Some(reply) = reply_to
        && !under(through.address, reply, Kind::Post, topic)
    {
        through.key(reply, Kind::Post, "reply_to")?;
        return Err(Explained::new(
            INVALID,
            "reply_to names a Post of another Topic; reply in the Topic it belongs to.",
        )
        .into());
    }
    let request_id =
        request_id_argument(arguments, &[topic, reply_to.unwrap_or_default(), body_text])?;
    let sent =
        json!({ "requestId": request_id, "text": body_text, "replyTo": reply_to, "facets": [] });
    let (status, answer) = through.send(
        "POST",
        &format!("{ROOT}/topics/{key}/posts"),
        Some(&sent),
        POST_EFFECT,
    )?;
    let posted = string(&answer, "postRef").unwrap_or_default();
    if !under(through.address, posted, Kind::Post, topic)
        || string(&answer, "requestId") != Some(request_id.as_str())
    {
        return Err(untrusted(POST_EFFECT));
    }
    let mut text = Text::default();
    if status == 202 {
        text.say(format!("The Space took the Post {posted} in Topic {topic} and will show it once its stewards approve it (request_id {request_id})."));
    } else {
        text.say(format!("Posted in Topic {topic} as Post {posted} (request_id {request_id}). Sending the same post with the same request_id again answers with this same Post."));
    }
    let result = text.done(json!({
        "post": posted,
        "topic": topic,
        "requestId": request_id,
        "pending": status == 202,
    }));
    Ok(result)
}

fn start_topic(
    through: &Through<'_>,
    arguments: &Map<String, Value>,
) -> Result<ToolResult, Failed> {
    let tangent = required(arguments, "tangent", references::REFERENCE_LIMIT)?;
    let key = through.key(tangent, Kind::Tangent, "tangent")?;
    let title = required(arguments, "title", TITLE_MOST)?;
    let first = required(arguments, "text", TEXT_MOST)?;
    let request_id = request_id_argument(arguments, &[tangent, title, first])?;
    let sent = json!({ "requestId": request_id, "title": title, "text": first });
    let (_, answer) = through.send(
        "POST",
        &format!("{ROOT}/tangents/{key}/topics"),
        Some(&sent),
        SAFE,
    )?;
    let topic = string(&answer, "topicRef").unwrap_or_default();
    let opening = string(&answer, "postRef").unwrap_or_default();
    if string(&answer, "tangentRef") != Some(tangent)
        || string(&answer, "requestId") != Some(request_id.as_str())
        || !under(through.address, topic, Kind::Topic, tangent)
        || !under(through.address, opening, Kind::Post, topic)
    {
        return Err(untrusted(SAFE));
    }
    let mut text = Text::default();
    text.say(format!("Started Topic {topic} in Tangent {tangent}, opening with Post {opening} (request_id {request_id}). Sending the same request again answers with this same Topic."));
    Ok(text.done(json!({ "topic": topic, "post": opening, "requestId": request_id })))
}

fn change_post(
    through: &Through<'_>,
    arguments: &Map<String, Value>,
    edit: bool,
) -> Result<ToolResult, Failed> {
    let post = required(arguments, "post", references::REFERENCE_LIMIT)?;
    let id = through.key(post, Kind::Post, "post")?;
    let path = format!("{ROOT}/posts/{id}");
    let (_, answer) = if edit {
        let new_text = required(arguments, "text", TEXT_MOST)?;
        through.send(
            "PATCH",
            &path,
            Some(&json!({ "text": new_text })),
            READ_FIRST,
        )?
    } else {
        through.send("DELETE", &path, None, READ_FIRST)?
    };
    if string(&answer, "postRef") != Some(post) {
        return Err(untrusted(READ_FIRST));
    }
    let mut text = Text::default();
    text.say(if edit {
        format!("Edited Post {post}.")
    } else {
        format!("Removed Post {post}.")
    });
    Ok(text.done(json!({ "post": post, "removed": !edit })))
}

fn mark_read(through: &Through<'_>, arguments: &Map<String, Value>) -> Result<ToolResult, Failed> {
    let topic = required(arguments, "topic", references::REFERENCE_LIMIT)?;
    let key = through.key(topic, Kind::Topic, "topic")?;
    let cursor = cursor_argument(arguments, "read_cursor")?.ok_or(Explained::new(
        INVALID,
        "read_cursor is required: pass the one tangent_read_topic gave.",
    ))?;
    let (_, answer) = through.send(
        "POST",
        &format!("{ROOT}/topics/{key}/read-position"),
        Some(&json!({ "readCursor": cursor })),
        SAFE,
    )?;
    let through_post = string(&answer, "throughPostRef");
    if string(&answer, "topicRef") != Some(topic)
        || through_post.is_some_and(|post| !under(through.address, post, Kind::Post, topic))
    {
        return Err(untrusted(SAFE));
    }
    let mut text = Text::default();
    match through_post {
        Some(post) => text.say(format!("Topic {topic} is marked read through Post {post}.")),
        None => text.say(format!("Topic {topic} is marked read.")),
    };
    Ok(text.done(json!({ "topic": topic, "through": through_post })))
}

fn membership(
    through: &Through<'_>,
    arguments: &Map<String, Value>,
    join: bool,
) -> Result<ToolResult, Failed> {
    let tangent = required(arguments, "tangent", references::REFERENCE_LIMIT)?;
    let key = through.key(tangent, Kind::Tangent, "tangent")?;
    let path = format!("{ROOT}/tangents/{key}/membership");
    let (status, answer) = if join {
        through.send("PUT", &path, Some(&json!({})), SAFE)?
    } else {
        through.send("DELETE", &path, None, SAFE)?
    };
    let state = string(&answer, "membership");
    let mut text = Text::default();
    match (status, state) {
        (202, _) | (_, Some("pending")) => text.say(format!("You asked to join Tangent {tangent}; its stewards review new members, and you are in once they admit you.")),
        (_, Some("member")) => text.say(format!("You are a member of Tangent {tangent}.")),
        (_, Some("none")) => text.say(format!("You are not a member of Tangent {tangent} any more.")),
        _ => return Err(untrusted(SAFE)),
    };
    Ok(text.done(json!({ "tangent": tangent, "membership": state })))
}

fn watch(through: &Through<'_>, arguments: &Map<String, Value>) -> Result<ToolResult, Failed> {
    let scope = scope_argument(through, arguments)?.ok_or(Explained::new(
        INVALID,
        "scope is required: the Tangent or Topic to watch, by its reference.",
    ))?;
    let mode = match text_argument(arguments, "mode", 16)? {
        Some(mode @ ("all" | "replies" | "none")) => mode,
        _ => return Err(Explained::new(INVALID, "mode is all, replies or none.").into()),
    };
    let (_, answer) = through.send(
        "PUT",
        &format!("{ROOT}/watches"),
        Some(&json!({ "scopeRef": scope, "mode": mode })),
        SAFE,
    )?;
    if string(&answer, "scopeRef") != Some(scope) || string(&answer, "mode") != Some(mode) {
        return Err(untrusted(SAFE));
    }
    let mut text = Text::default();
    text.say(match mode {
        "all" => format!("You follow every new Post in {scope}."),
        "replies" => format!("You follow only replies to you in {scope}."),
        _ => format!("You follow nothing in {scope}."),
    });
    Ok(text.done(json!({ "scope": scope, "mode": mode })))
}

fn mentionables(
    through: &Through<'_>,
    arguments: &Map<String, Value>,
) -> Result<ToolResult, Failed> {
    let topic = required(arguments, "topic", references::REFERENCE_LIMIT)?;
    let key = through.key(topic, Kind::Topic, "topic")?;
    let mut path = format!("{ROOT}/topics/{key}/mentionables");
    if let Some(prefix) = text_argument(arguments, "prefix", 64)? {
        query(&mut path, "prefix", prefix);
    }
    let body = through.get(&path)?;
    let targets = body["targets"]
        .as_array()
        .ok_or_else(|| untrusted(Effect::Read))?;
    if targets.len() > PAGE_MOST {
        return Err(untrusted(Effect::Read));
    }
    let mut text = Text::default();
    text.say(format!(
        "Who can be mentioned in Topic {topic} ({}):",
        targets.len()
    ));
    let mut kept = Vec::new();
    for target in targets {
        text.say("\n- ")
            .quote(string(target, "label").unwrap_or_default())
            .say(", mentioned by writing ")
            .quote(string(target, "insert").unwrap_or_default());
        kept.push(json!({ "label": string(target, "label"), "insert": string(target, "insert") }));
    }
    if targets.is_empty() {
        text.say(" nobody yet.");
    }
    Ok(text.done(json!({ "topic": topic, "mentionables": kept })))
}

fn participant(
    through: &Through<'_>,
    arguments: &Map<String, Value>,
) -> Result<ToolResult, Failed> {
    let identifier = required(arguments, "participant", 512)?;
    if identifier.contains(['/', '\\']) || identifier.chars().any(char::is_control) {
        return Err(Explained::new(
            INVALID,
            "participant is a participant's reference or handle, as the Space gave it.",
        )
        .into());
    }
    let body = through.get(&format!("{ROOT}/participants/{}", encode(identifier)))?;
    let profile = &body["profile"];
    let posts = body["recentPosts"]
        .as_array()
        .ok_or_else(|| untrusted(Effect::Read))?;
    if posts.len() > PAGE_MOST || string(profile, "participantRef").is_none() {
        return Err(untrusted(Effect::Read));
    }
    let reference = string(profile, "participantRef").unwrap_or_default();
    let mut text = Text::default();
    text.say("Participant ")
        .quote(reference)
        .say(": ")
        .quote(string(profile, "displayName").unwrap_or_default());
    if let Some(handle) = string(profile, "handle") {
        text.say(", handle ").quote(handle);
    }
    if let Some(kind) = string(profile, "classification") {
        text.say(", a ").quote(kind).say(" participant");
    }
    if let Some(description) = string(profile, "description") {
        text.say(", who says ").quote_cut(description, 500);
    }
    text.say(format!(
        ". {}:",
        plural(posts.len(), "recent Post", "recent Posts")
    ));
    let mut kept = Vec::new();
    for post in posts {
        kept.push(say_post(through, &mut text, post, None, Effect::Read)?);
    }
    Ok(text.done(json!({
        "participant": reference,
        "displayName": string(profile, "displayName"),
        "handle": string(profile, "handle"),
        "posts": kept,
    })))
}

fn report_post(
    through: &Through<'_>,
    arguments: &Map<String, Value>,
) -> Result<ToolResult, Failed> {
    let post = required(arguments, "post", references::REFERENCE_LIMIT)?;
    through.key(post, Kind::Post, "post")?;
    let topic_key = keys(through.address, post, Kind::Post)
        .map(|keys| keys[1])
        .unwrap_or_default();
    let reason = required(arguments, "reason", 64)?;
    if !reason
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
    {
        return Err(Explained::new(
            INVALID,
            "reason is lowercase words joined by underscores, such as spam.",
        )
        .into());
    }
    let statement = text_argument(arguments, "statement", 1000)?;
    let request_id =
        request_id_argument(arguments, &[post, reason, statement.unwrap_or_default()])?;
    let sent = json!({ "requestId": request_id, "postRef": post, "reasonCode": reason, "statement": statement });
    let (_, answer) = through.send(
        "POST",
        &format!("{ROOT}/topics/{topic_key}/reports"),
        Some(&sent),
        SAFE,
    )?;
    if string(&answer, "postRef") != Some(post) {
        return Err(untrusted(SAFE));
    }
    let mut text = Text::default();
    text.say(if answer["alreadyReported"] == true {
        format!("You had reported Post {post} already; the stewards have it.")
    } else {
        format!("Reported Post {post} to its Topic's stewards.")
    });
    Ok(text.done(json!({ "post": post, "alreadyReported": answer["alreadyReported"] == true })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lobby_contracts::ServiceName;

    #[test]
    fn every_tool_fits_its_projected_name_and_says_what_it_takes() {
        let service = ServiceName::new(crate::NAME).expect("a service name");
        for tool in TOOLS {
            let descriptor = tool.descriptor();
            assert!(descriptor.name.projected(&service).len() <= 64);
            let schema = &descriptor.input_schema;
            let declared: Vec<&String> = schema["properties"]
                .as_object()
                .expect("properties")
                .keys()
                .collect();
            let mut expected: Vec<&str> = tool.arguments.to_vec();
            expected.sort_unstable();
            let mut declared: Vec<&str> = declared.iter().map(|name| name.as_str()).collect();
            declared.sort_unstable();
            assert_eq!(declared, expected, "{}", tool.name);
            assert_eq!(schema["additionalProperties"], false);
            assert!(!tool.title.is_empty() && tool.description.ends_with('.'));
        }
        assert_eq!(TOOLS[0].name, "arrive", "a mind starts by arriving");
    }

    #[test]
    fn a_request_id_left_out_is_made_from_the_content() {
        let none = Map::new();
        let one = request_id_argument(&none, &["topic", "", "hello"]).expect("made");
        let again = request_id_argument(&none, &["topic", "", "hello"]).expect("made");
        let other = request_id_argument(&none, &["topic", "", "hello!"]).expect("made");
        assert_eq!(one, again);
        assert_ne!(one, other);
        let mut given = Map::new();
        given.insert("request_id".to_owned(), json!("reply-1"));
        assert_eq!(request_id_argument(&given, &[]).expect("given"), "reply-1");
        given.insert("request_id".to_owned(), json!("has space"));
        assert!(request_id_argument(&given, &[]).is_err());
    }
}
