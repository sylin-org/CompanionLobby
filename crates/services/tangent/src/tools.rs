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
//! characters, names at [`NAME_SHOWN`] and descriptions at
//! [`ABOUT_SHOWN`], and a page shows the newest Posts whose quoted text fits
//! in [`POSTS_BUDGET`] bytes, saying how many it left out, so a result fits
//! its reading budget whatever script it is written in. The structured
//! answer keeps the same cuts and holds what other people wrote only under
//! each item's `quoted` object, naming in `cutShort` the fields it cut. A
//! host may show a mind either form, so each carries the same facts: what
//! is new names each person by their card's name beside their reference.
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
use std::time::{Duration, Instant};

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
/// The most characters of a name, a title, a handle or a participant
/// reference shown in a result.
pub const NAME_SHOWN: usize = 200;
/// The most characters of a description, a Topic's brief or an excerpt
/// shown in a result.
pub const ABOUT_SHOWN: usize = 300;
/// The most characters of what a participant says about themselves.
pub const PROFILE_SHOWN: usize = 500;
/// The longest Post or Topic text a mind may send, in characters.
pub const TEXT_MOST: usize = 10_000;
/// The longest Topic title, in characters.
pub const TITLE_MOST: usize = 200;
/// The longest request id, in bytes.
pub const REQUEST_ID_MOST: usize = 128;
/// The most items one page holds.
const PAGE_MOST: usize = 10;
/// The most bytes the Posts of one answer take once cut and quoted, so the
/// whole answer, as text or structured, stays within its reading budget
/// whatever script the Posts are written in. A page whose Posts take more
/// shows its newest Posts that fit and says how many it left out.
pub const POSTS_BUDGET: usize = 20 * 1024;

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
    /// What a mind reads when the answer to this write never arrived, in
    /// this tool's own nouns; a read has none.
    pub unknown: Option<&'static str>,
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
        unknown: None,
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
        unknown: None,
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
        unknown: None,
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
        unknown: None,
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
        unknown: None,
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
        unknown: Some(wire::POST_UNKNOWN),
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
        unknown: Some(
            "The Space may have started this Topic, but its answer never arrived. List the Tangent's Topics with tangent_topics before starting it again; sending the same request_id again is safe and answers with the same Topic.",
        ),
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
        unknown: Some(
            "The Space may have edited this Post, but its answer never arrived. Read its Topic before editing it again.",
        ),
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
        unknown: Some(
            "The Space may have removed this Post, but its answer never arrived. Read its Topic before removing it again.",
        ),
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
        unknown: Some(
            "The Space may have marked this Topic read, but its answer never arrived. Marking it read again with the same read_cursor is safe.",
        ),
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
        unknown: Some(
            "The Space may have taken your request to join this Tangent, but its answer never arrived. Call tangent_tangents to see whether you are a member before joining again.",
        ),
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
        unknown: Some(
            "The Space may have taken your leaving this Tangent, but its answer never arrived. Call tangent_tangents to see whether you are still a member before leaving again.",
        ),
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
        unknown: Some(
            "The Space may have changed how you follow this, but its answer never arrived. Setting the same mode again is safe.",
        ),
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
        unknown: None,
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
        description: "Reads one participant of the Space: their reference, name and handle, what kind of participant they are and what they say about themselves when the Space gives those, and their newest Posts, up to 10; when some do not fit the reading budget, it says how many it left out.",
        effect: Effect::Read,
        unknown: None,
        needs: Needs::Read,
        arguments: &["participant"],
        schema: || {
            object(
                json!({
                    "participant": { "type": "string", "minLength": 1, "maxLength": 512, "description": "The participant, by the reference a Post or an update gave beside their name (author or actor), or by their handle." },
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
        unknown: Some(
            "The Space may have taken this report, but its answer never arrived. Reporting the same Post again with the same request_id is safe and says whether it was reported already.",
        ),
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

/// The refusals that name the argument they refuse, one fixed sentence per
/// argument a tool takes, so a mind reads which one to change.
macro_rules! argument_sentences {
    ($($name:literal),* $(,)?) => {
        /// Every argument a refusal names.
        #[cfg(test)]
        const NAMED: &[&str] = &[$($name),*];

        /// What a tool answers when `name`, which it needs, is missing.
        fn missing(name: &str) -> &'static str {
            match name {
                $($name => concat!($name, " is missing; this tool needs it, as its description says."),)*
                _ => "A required argument is missing; the tool's description names each one.",
            }
        }

        /// What a tool answers when `name` is not text of a length it takes.
        fn not_text(name: &str) -> &'static str {
            match name {
                $($name => concat!($name, " must be text, not empty, within the length the tool's description gives."),)*
                _ => "An argument is not text of the length the tool's description gives.",
            }
        }

        /// What a tool answers when `name` holds a reference of another Space.
        pub(crate) fn foreign(name: &str) -> &'static str {
            match name {
                $($name => concat!($name, " holds a reference of another Space. Use a reference this Space gave, or pass at to act at the Space it came from."),)*
                _ => references::FOREIGN,
            }
        }
    };
}

argument_sentences!(
    "scope",
    "cursor",
    "tangent",
    "topic",
    "around",
    "checkpoint",
    "text",
    "reply_to",
    "title",
    "post",
    "read_cursor",
    "mode",
    "prefix",
    "participant",
    "reason",
    "statement",
);

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
        Some(_) => Err(Explained::new(INVALID, not_text(name))),
    }
}

fn required<'a>(
    arguments: &'a Map<String, Value>,
    name: &str,
    most: usize,
) -> Result<&'a str, Explained> {
    text_argument(arguments, name, most)?.ok_or(Explained::new(INVALID, missing(name)))
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
        match cut_to(text, most) {
            (kept, false) => self.quote(&kept),
            (kept, true) => self.quote(&kept).say(" (cut short here)"),
        }
    }

    fn done(self, structured: Value) -> ToolResult {
        ToolResult {
            text: self.0,
            structured: Some(structured),
        }
    }
}

/// `text` cut to its first `most` characters, and whether it was cut.
fn cut_to(text: &str, most: usize) -> (String, bool) {
    if text.chars().count() <= most {
        (text.to_owned(), false)
    } else {
        (text.chars().take(most).collect(), true)
    }
}

/// `item` with what other people wrote under `quoted`, each field cut to
/// its most characters, and the fields cut named in `cutShort`. A field the
/// Space left out is left out.
fn with_quoted(mut item: Value, fields: &[(&str, Option<&str>, usize)]) -> Value {
    let mut quoted = Map::new();
    let mut cut = Vec::new();
    for &(name, text, most) in fields {
        let Some(text) = text else {
            continue;
        };
        let (kept, short) = cut_to(text, most);
        if short {
            cut.push(Value::from(name));
        }
        quoted.insert(name.to_owned(), Value::String(kept));
    }
    item["quoted"] = Value::Object(quoted);
    if !cut.is_empty() {
        item["cutShort"] = Value::Array(cut);
    }
    item
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
        self.send_within(method, path, body, effect, None)
    }

    fn send_within(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        effect: Effect,
        within: Option<Duration>,
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
                within,
            },
        )?;
        let status = response.status;
        wire::answered(&response, effect).map(|body| (status, body))
    }

    fn get(&self, path: &str) -> Result<Value, Failed> {
        self.send("GET", path, None, Effect::Read)
            .map(|(_, body)| body)
    }

    /// [`Through::get`], ended at `deadline` however the Space answers.
    fn get_within(&self, path: &str, deadline: Duration) -> Result<Value, Failed> {
        self.send_within("GET", path, None, Effect::Read, Some(deadline))
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
    let performed = match tool.name {
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
    };
    performed.map_err(|failed| in_its_words(tool, failed))
}

/// What a mind reads when the Space refuses a post, giving no reason the
/// adapter knows.
pub const POST_DENIED: &str = "The Space does not let this companion post in this Topic. Read it with tangent_read_topic to see whether it is locked, or post in another Topic.";
/// What a mind reads when the Space refuses a new Topic.
pub const START_DENIED: &str = "The Space does not let this companion start a Topic in this Tangent. List its Topics with tangent_topics to see where it can post.";
/// What a mind reads when the Space refuses an edit or a removal.
pub const CHANGE_DENIED: &str = "The Space does not let this companion change that Post. Only a Post's author can, and its Topic may be locked; read the Topic with tangent_read_topic to see which.";

/// `failed`, with an unknown outcome and a refusal that gives no reason
/// the adapter knows said in `tool`'s own nouns.
fn in_its_words(tool: &Tool, failed: Failed) -> Failed {
    match (failed, tool.unknown) {
        (Failed::Ended(explained), Some(sentence)) if explained.outcome == Outcome::Unknown => {
            Explained::new(Outcome::Unknown, sentence).into()
        }
        (Failed::Ended(explained), _)
            if explained.outcome == Outcome::Refused
                && explained.sentence == wire::PERMISSION_DENIED =>
        {
            let sentence = match tool.name {
                "post" => POST_DENIED,
                "start_topic" => START_DENIED,
                "edit_post" | "remove_post" => CHANGE_DENIED,
                _ => wire::PERMISSION_DENIED,
            };
            Explained::new(Outcome::Refused, sentence).into()
        }
        (failed, _) => failed,
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
        return Err(Explained::new(INVALID, foreign("scope")));
    }
    Err(Explained::new(
        INVALID,
        "scope is a Tangent's or a Topic's reference, as tangent_arrive gives them.",
    ))
}

/// How long the participant cards that name what is new may take in all.
const NAMES_WITHIN: Duration = Duration::from_secs(3);

/// The names the Space shows for `actors`, each read once from its
/// participant card, for at most [`PAGE_MOST`] of them and within
/// [`NAMES_WITHIN`] in all. An actor whose card cannot be read, or names
/// another participant, goes unnamed, and once the time runs out the rest
/// go unnamed too: a name is a courtesy, and its absence never fails or
/// stalls the answer.
fn names_of<'a>(
    through: &Through<'_>,
    actors: impl Iterator<Item = &'a str>,
) -> Vec<(String, String)> {
    let mut distinct: Vec<&str> = Vec::new();
    for actor in actors {
        if participant_shaped(actor) && actor.len() <= 512 && !distinct.contains(&actor) {
            distinct.push(actor);
        }
    }
    let until = Instant::now() + NAMES_WITHIN;
    let mut names = Vec::new();
    for actor in distinct.into_iter().take(PAGE_MOST) {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let Ok(card) = through.get_within(&format!("{ROOT}/participants/{}", encode(actor)), left)
        else {
            continue;
        };
        let profile = &card["profile"];
        if let Some(name) = string(profile, "displayName").filter(|name| !name.trim().is_empty())
            && string(profile, "participantRef") == Some(actor)
        {
            names.push((actor.to_owned(), name.to_owned()));
        }
    }
    names
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
    for item in items {
        if !through.is(string(item, "sourceRef"), Kind::Post)
            || !through.is(string(item, "scopeRef"), Kind::Topic)
        {
            return Err(untrusted(effect));
        }
    }
    let names = names_of(
        through,
        items.iter().filter_map(|item| string(item, "actorRef")),
    );
    let mut kept = Vec::new();
    for item in items {
        let source = string(item, "sourceRef").unwrap_or_default();
        let scope = string(item, "scopeRef").unwrap_or_default();
        let excerpt = string(item, "excerpt");
        let actor = string(item, "actorRef").unwrap_or_default();
        let name = names
            .iter()
            .find(|(reference, _)| reference == actor)
            .map(|(_, name)| name.as_str());
        let (kind, said) = match string(item, "kind") {
            Some(kind @ "direct_reply") => (kind, "A reply to you"),
            Some(kind @ "direct_mention") => (kind, "A mention of you"),
            Some(kind @ "watched_activity") => (kind, "A new Post"),
            Some(kind @ "write_settlement") => (kind, ""),
            _ => ("other", "Something new"),
        };
        if kind == "write_settlement" {
            text.say(format!(
                "\n- Your Post {source} is now shown in Topic {scope}; it begins "
            ));
        } else {
            text.say(format!("\n- {said} by "));
            match name {
                Some(name) => text
                    .quote_cut(name, NAME_SHOWN)
                    .say(" (participant ")
                    .quote_cut(actor, NAME_SHOWN)
                    .say(")"),
                None => text
                    .say("participant ")
                    .quote_cut(actor, NAME_SHOWN)
                    .say(" (tangent_participant names them)"),
            };
            text.say(format!(" in Topic {scope}: Post {source}, which begins "));
        }
        text.quote_cut(excerpt.unwrap_or_default(), ABOUT_SHOWN);
        kept.push(with_quoted(
            json!({ "kind": kind, "topic": scope, "post": source }),
            &[
                ("actor", Some(actor), NAME_SHOWN),
                ("actorName", name, NAME_SHOWN),
                ("excerpt", excerpt, ABOUT_SHOWN),
            ],
        ));
    }
    let more = attention["more"] == true;
    if more {
        text.say("\nThere is more; tangent_updates reads further.");
    }
    let mut said = json!({
        "newActivity": new,
        "waiting": waiting,
        "order": "newest first",
        "items": kept,
    });
    // A host may show the structured answer alone, so it says there is
    // more as the text does.
    if more {
        said["more"] = json!({ "readWith": "tangent_updates" });
    }
    Ok(said)
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
        let writable = tangent["hasWritableTopics"] == true;
        let (name, description) = (string(tangent, "name"), string(tangent, "description"));
        text.say(format!("\n- Tangent {}: ", reference.unwrap_or_default()))
            .quote_cut(name.unwrap_or_default(), NAME_SHOWN)
            .say(", described as ")
            .quote_cut(description.unwrap_or_default(), ABOUT_SHOWN)
            .say(format!(
                "; you are {}{}.",
                if member { "a member" } else { "not a member" },
                if writable {
                    ", and it has Topics you can write in"
                } else {
                    ""
                }
            ));
        kept.push(with_quoted(
            json!({
                "tangent": reference,
                "member": member,
                "hasTopicsYouCanWriteIn": writable,
            }),
            &[
                ("name", name, NAME_SHOWN),
                ("description", description, ABOUT_SHOWN),
            ],
        ));
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
    // What the companion may do here through this connection's tools;
    // managing is not among them, whatever the Space allows.
    let mut may = vec!["read"];
    if authority["canPost"] == true {
        may.push("post");
    }
    let (name, handle) = (string(identity, "displayName"), string(identity, "handle"));
    let mut text = Text::default();
    text.say("You are ")
        .quote_cut(name.unwrap_or_default(), NAME_SHOWN)
        .say(" here, participant ")
        .quote_cut(&through.issued.participant, NAME_SHOWN)
        .say(", in the Space ")
        .quote_cut(
            string(&body["place"], "label").unwrap_or_default(),
            NAME_SHOWN,
        )
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
    let more_tangents = body["continuation"].is_string();
    if more_tangents {
        text.say("\nThere are more Tangents; tangent_tangents lists them.");
    }
    text.say("\n\nRead a Topic with tangent_read_topic, list a Tangent's Topics with tangent_topics, and post with tangent_post.");
    let mut facts = json!({
        "you": with_quoted(
            json!({}),
            &[
                ("participant", Some(through.issued.participant.as_str()), NAME_SHOWN),
                ("displayName", name, NAME_SHOWN),
                ("handle", handle, NAME_SHOWN),
            ],
        ),
        "may": may,
        "attention": attention,
        "tangents": kept,
    });
    // A host may show the structured answer alone, so it says there are
    // more Tangents as the text does.
    if more_tangents {
        facts["moreTangents"] = json!({ "readWith": "tangent_tangents" });
    }
    Ok(text.done(facts))
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
        let (title, about) = (string(topic, "title"), string(topic, "topic"));
        text.say(format!("\n- Topic {reference}: "))
            .quote_cut(title.unwrap_or_default(), NAME_SHOWN)
            .say(", about ")
            .quote_cut(about.unwrap_or_default(), ABOUT_SHOWN)
            .say(if writable {
                "; you can post there."
            } else if topic["isLocked"] == true {
                "; it is locked."
            } else {
                "; you cannot post there."
            });
        kept.push(with_quoted(
            json!({ "topic": reference, "canPost": writable }),
            &[("title", title, NAME_SHOWN), ("about", about, ABOUT_SHOWN)],
        ));
    }
    if topics.is_empty() {
        text.say(" none yet. Start one with tangent_start_topic.");
    }
    let next = say_next(&mut text, &body, "continuation", "cursor", Effect::Read)?;
    Ok(text.done(json!({ "tangent": tangent, "topics": kept, "cursor": next })))
}

/// The bytes `post`'s author name and text take in a result, cut and quoted.
fn shown_bytes(post: &Value) -> usize {
    let text = if post["removed"] == true {
        ""
    } else {
        string(post, "text").unwrap_or_default()
    };
    let quoted = |text: &str, most: usize| Value::String(cut_to(text, most).0).to_string().len();
    quoted(text, POST_SHOWN) + quoted(string(post, "authorName").unwrap_or_default(), NAME_SHOWN)
}

/// How many of `posts`, in the order given, fit within [`POSTS_BUDGET`]
/// once cut and quoted; the first always does.
fn fitting<'a>(posts: impl Iterator<Item = &'a Value>) -> usize {
    let mut used = 0;
    let mut count = 0;
    for post in posts {
        used += shown_bytes(post);
        if count > 0 && used > POSTS_BUDGET {
            break;
        }
        count += 1;
    }
    count
}

/// The cursor `body` gives in `field`, when it gives one the Space may.
fn cursor_of(body: &Value, field: &str) -> Result<Option<String>, Failed> {
    match body.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(cursor)) if cursor_shaped(cursor) => Ok(Some(cursor.clone())),
        Some(_) => Err(untrusted(Effect::Read)),
    }
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
    let author_name = string(post, "authorName");
    let written = string(post, "createdAt").filter(|at| parse_rfc3339(at).is_some());
    text.say(format!("\n- Post {reference} by "))
        .quote_cut(author_name.unwrap_or_default(), NAME_SHOWN)
        .say(" (participant ")
        .quote_cut(author, NAME_SHOWN)
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
    let removed = post["removed"] == true;
    let body = if removed { None } else { string(post, "text") };
    if removed {
        text.say(": removed.");
    } else {
        text.say(": ");
        text.quote_cut(body.unwrap_or_default(), POST_SHOWN);
    }
    Ok(with_quoted(
        json!({
            "post": reference,
            "at": written,
            "replyTo": reply,
            "removed": removed,
        }),
        &[
            ("author", Some(author), NAME_SHOWN),
            ("authorName", author_name, NAME_SHOWN),
            ("text", body, POST_SHOWN),
        ],
    ))
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
    let (title, brief) = (string(&body, "title"), string(&body, "brief"));
    let shown = fitting(posts.iter().rev());
    let left = posts.len() - shown;
    let older = cursor_of(&body, "historyOlderCursor")?;
    let read_cursor = cursor_of(&body, "readCursor")?;
    let mut text = Text::default();
    text.say(format!("Topic {topic}: "))
        .quote_cut(title.unwrap_or_default(), NAME_SHOWN)
        .say(", about ")
        .quote_cut(brief.unwrap_or_default(), ABOUT_SHOWN)
        .say(format!(
            "; {}, oldest first. {}",
            plural(shown, "Post", "Posts"),
            if writable {
                "You can post here."
            } else if body["isLocked"] == true {
                "It is locked: you cannot post here."
            } else {
                "You cannot post here."
            }
        ));
    // The cursor of a page whose oldest Posts were left out would skip
    // them, so a smaller page is offered instead.
    let older = if left > 0 {
        text.say(format!(
            "\nThe {} of this page {} left out to keep this answer within its reading budget; read with limit {shown} and follow its cursor to read further back.",
            plural(left, "oldest Post", "oldest Posts"),
            if left == 1 { "is" } else { "are" },
        ));
        None
    } else {
        if let Some(next) = &older {
            text.say("\nThere is more: pass cursor ")
                .quote(next)
                .say(" to read further back.");
        }
        older
    };
    if let Some(cursor) = &read_cursor {
        text.say("\nTo mark these read, call tangent_mark_read with read_cursor ")
            .quote(cursor)
            .say(".");
    }
    let mut kept = Vec::new();
    for (index, post) in posts.iter().enumerate() {
        if index < left {
            // Checked as every Post is, though not shown.
            say_post(
                through,
                &mut Text::default(),
                post,
                Some(topic),
                Effect::Read,
            )?;
            continue;
        }
        kept.push(say_post(
            through,
            &mut text,
            post,
            Some(topic),
            Effect::Read,
        )?);
    }
    if writable {
        text.say(format!(
            "\nTo reply, call tangent_post with topic {topic} and reply_to the reference of the Post you answer."
        ));
    }
    // A host may show the structured answer alone, so it names how to
    // reply too.
    let reply_with = writable.then(|| json!({ "tool": "tangent_post", "topic": topic }));
    let mut facts = json!({
        "topic": topic,
        "canPost": writable,
        "posts": kept,
        "cursor": older,
        "readCursor": read_cursor,
        "replyWith": reply_with,
    });
    // The oldest Posts left out are named in the structured answer too,
    // with the limit that reads them, since its cursor is withheld.
    if left > 0 {
        facts["leftOut"] = json!(left);
        facts["readWithLimit"] = json!(shown);
    }
    Ok(text.done(with_quoted(
        facts,
        &[("title", title, NAME_SHOWN), ("about", brief, ABOUT_SHOWN)],
    )))
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
    let made = matches!(arguments.get("request_id"), None | Some(Value::Null));
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
    } else if made {
        text.say(format!("Posted in Topic {topic} as Post {posted} (request_id {request_id}, made from what you posted). If you posted this same text here before without a request_id, this is that earlier Post and nothing new was posted; pass a request_id of your own to post it again."));
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
        let (label, insert) = (string(target, "label"), string(target, "insert"));
        text.say("\n- ")
            .quote_cut(label.unwrap_or_default(), NAME_SHOWN)
            .say(", mentioned by writing ")
            .quote_cut(insert.unwrap_or_default(), NAME_SHOWN);
        kept.push(with_quoted(
            json!({}),
            &[("label", label, NAME_SHOWN), ("insert", insert, NAME_SHOWN)],
        ));
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
    if !participant_shaped(identifier) {
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
    let reference = string(profile, "participantRef");
    let name = string(profile, "displayName");
    let handle = string(profile, "handle");
    let description = string(profile, "description");
    let mut text = Text::default();
    text.say("Participant ")
        .quote_cut(reference.unwrap_or_default(), NAME_SHOWN)
        .say(": ")
        .quote_cut(name.unwrap_or_default(), NAME_SHOWN);
    if let Some(handle) = handle {
        text.say(", handle ").quote_cut(handle, NAME_SHOWN);
    }
    let kind = string(profile, "classification");
    if let Some(kind) = kind {
        text.say(", a ")
            .quote_cut(kind, NAME_SHOWN)
            .say(" participant");
    }
    if let Some(description) = description {
        text.say(", who says ")
            .quote_cut(description, PROFILE_SHOWN);
    }
    let shown = fitting(posts.iter());
    text.say(format!(
        ". {}:",
        plural(shown, "recent Post", "recent Posts")
    ));
    let mut kept = Vec::new();
    for (index, post) in posts.iter().enumerate() {
        if index < shown {
            kept.push(say_post(through, &mut text, post, None, Effect::Read)?);
        } else {
            say_post(through, &mut Text::default(), post, None, Effect::Read)?;
        }
    }
    let left = posts.len() - shown;
    if left > 0 {
        text.say(format!(
            "\n{} left out to keep this answer within its reading budget; read them in their Topics.",
            plural(left, "more recent Post is", "more recent Posts are")
        ));
    }
    let mut facts = json!({ "posts": kept });
    // A host may show the structured answer alone, so it says how many
    // Posts were left out, as a Topic read does.
    if left > 0 {
        facts["leftOut"] = json!(left);
        facts["readLeftOutIn"] = json!("their Topics, with tangent_read_topic");
    }
    Ok(text.done(with_quoted(
        facts,
        &[
            ("participant", reference, NAME_SHOWN),
            ("displayName", name, NAME_SHOWN),
            ("handle", handle, NAME_SHOWN),
            ("kind", kind, NAME_SHOWN),
            ("description", description, PROFILE_SHOWN),
        ],
    )))
}

/// Whether `identifier` names one participant and nothing else: one path
/// segment, never only dots, with no control characters.
fn participant_shaped(identifier: &str) -> bool {
    !identifier.contains(['/', '\\'])
        && !identifier.chars().any(char::is_control)
        && !identifier.chars().all(|character| character == '.')
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
    fn every_write_says_its_own_unknown_outcome_and_a_read_has_none() {
        let mut said = Vec::new();
        for tool in TOOLS {
            match (tool.effect, tool.unknown) {
                (Effect::Read, None) => {}
                (Effect::Write { .. }, Some(sentence)) => {
                    assert!(sentence.contains("never arrived"), "{}", tool.name);
                    assert!(!said.contains(&sentence), "{} repeats another", tool.name);
                    said.push(sentence);
                }
                _ => panic!("{} says the wrong unknown outcome", tool.name),
            }
        }
        let join = tool("join").expect("join");
        let lost = in_its_words(join, wire::lost(join.effect).into());
        let Failed::Ended(explained) = lost else {
            panic!("ended");
        };
        assert_eq!(explained.outcome, Outcome::Unknown);
        assert!(explained.sentence.contains("join this Tangent"));
        assert!(!explained.sentence.contains("post"));
        let refused = Explained::new(Outcome::Refused, wire::PERMISSION_DENIED);
        assert_eq!(in_its_words(join, refused.into()), Failed::Ended(refused));
        for (name, sentence) in [
            ("post", POST_DENIED),
            ("start_topic", START_DENIED),
            ("edit_post", CHANGE_DENIED),
            ("remove_post", CHANGE_DENIED),
        ] {
            assert_eq!(
                in_its_words(tool(name).expect("tool"), refused.into()),
                Failed::Ended(Explained::new(Outcome::Refused, sentence)),
                "{name} says a refusal in its own nouns"
            );
        }
        let suspended = Explained::new(Outcome::Refused, wire::SUSPENDED);
        assert_eq!(
            in_its_words(tool("post").expect("post"), suspended.into()),
            Failed::Ended(suspended),
            "a refusal with its own reason keeps it"
        );
    }

    #[test]
    fn a_refused_argument_is_named_in_its_refusal() {
        for tool in TOOLS {
            for name in tool.arguments {
                // limit and request_id have sentences of their own that name them.
                if matches!(*name, "limit" | "request_id") {
                    continue;
                }
                assert!(
                    NAMED.contains(name),
                    "{} takes {name}, which no refusal names",
                    tool.name
                );
            }
        }
        let missing = required(&Map::new(), "topic", 10).expect_err("missing");
        assert_eq!(
            missing.sentence,
            "topic is missing; this tool needs it, as its description says."
        );
        let mut given = Map::new();
        given.insert("text".to_owned(), json!(42));
        let not_text = text_argument(&given, "text", 10).expect_err("not text");
        assert!(
            not_text.sentence.starts_with("text must be text"),
            "{}",
            not_text.sentence
        );
    }

    #[test]
    fn a_participant_is_one_segment_and_never_only_dots() {
        assert!(participant_shaped("prt_2"));
        assert!(participant_shaped("bea.example.test"));
        for climbing in [".", "..", "...", "a/b", "a\\b", "a\nb"] {
            assert!(!participant_shaped(climbing), "{climbing:?}");
        }
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
