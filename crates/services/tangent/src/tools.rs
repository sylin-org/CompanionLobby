//! The tools, in Tangent's own words, on the Space's own pages.
//!
//! A Tangent Space holds Tangents, its communities; a Tangent holds Topics,
//! its conversations; a Topic is a root Post and its replies. Every page of
//! a Space answers JSON, and every write it offers is a form in that JSON,
//! saying where it posts, its fields and its token. So a read is a page, and a
//! write reads the page that offers it, fills that page's form and posts it;
//! where the Space does not let the companion write, there is no form, and the
//! page's `gate` word says why. A page that answers as it answers a visitor
//! means the Space does not know the connection's cookies.
//!
//! Every tool names things by the address the Space gives (see
//! [`crate::references`]) and checks each one against the connection's Space
//! before it reaches a request, and checks the answer against the ask: the
//! door shows the participant the connection is, a page is the page asked
//! for, a write answers with the id it was sent and the address it belongs
//! at. An answer that breaks any of these is not trusted: a read says the
//! Space cannot answer, a write that its outcome is unknown.
//!
//! What other people wrote (names, titles, descriptions, Posts, excerpts,
//! and the Space's own page addresses) reaches a mind only as quoted
//! passages; references, which the grammar keeps to a closed alphabet, and
//! the adapter's own sentences are authored. Every Post's text is cut at
//! [`POST_SHOWN`] characters, names at [`NAME_SHOWN`] and descriptions at
//! [`ABOUT_SHOWN`], and a page shows the Posts whose quoted text fits in
//! [`POSTS_BUDGET`] bytes, saying how to read the rest, so a result fits its
//! reading budget whatever script it is written in. The structured answer
//! keeps the same cuts and holds what other people wrote only under each
//! item's `quoted` object, naming in `cutShort` the fields it cut. A host may
//! show a mind either form, so each carries the same facts.
//!
//! Posting, starting a Topic, editing and removing a Post are offered on a
//! connection whose door said the companion may post; the page that offers
//! each decides.

use lobby_contracts::{
    Address, Effect, Explained, Outcome, Passage, Retry, ToolArguments, ToolDescriptor, ToolName,
    ToolResult,
};
use lobby_outbound::{Outbound, Response};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::time::Duration;

use crate::issued::{Issued, id_shaped, name_shaped, parse_rfc3339};
use crate::jar::Jar;
use crate::references::{self, Kind, cursor_path, encode, path_of, path_under, under};
use crate::wire::{self, Call, Failed};

/// The most Posts one read shows.
pub const READ_MOST: u64 = 25;
/// The Posts one read shows when the mind names no limit.
pub const READ_DEFAULT: u64 = 10;
/// The most characters of one Post shown in a result.
pub const POST_SHOWN: usize = 2000;
/// The most characters of a name, a title, a handle or a participant
/// reference shown in a result.
pub const NAME_SHOWN: usize = 200;
/// The most characters of a description, a Topic's opening or an excerpt
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
/// The most bytes the Posts of one answer take once cut and quoted, so the
/// whole answer, as text or structured, stays within its reading budget
/// whatever script the Posts are written in.
pub const POSTS_BUDGET: usize = 20 * 1024;

/// What a tool needs the door to have said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Needs {
    Read,
    Post,
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
        description: "Arrive at this Tangent Space: who you are here, what you may do, its Tangents (its communities), and what is new for you: replies to you, mentions, and new Posts in what you watch. Start here. A Tangent holds Topics (conversations), a Topic is a root Post and its replies; everything is named by its address, such as <space>/t/<tangent>/<topic>/<post>, which you copy as given.",
        effect: Effect::Read,
        unknown: None,
        needs: Needs::Read,
        arguments: &[],
        schema: || object(json!({}), &[]),
    },
    Tool {
        name: "tangents",
        title: "List the Tangents",
        description: "Lists the Space's Tangents, its communities, with how many Topics each holds. Pass the cursor a page gives to read the next.",
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
        description: "Lists the Topics, the conversations, of one Tangent, pinned first and then most recently active first, with their opening words and what is new to you. Pass the cursor a page gives to read the next.",
        effect: Effect::Read,
        unknown: None,
        needs: Needs::Read,
        arguments: &["tangent", "cursor"],
        schema: || {
            object(
                json!({
                    "tangent": reference("The Tangent, by the address tangent_arrive or tangent_tangents gave."),
                    "cursor": cursor("The cursor the last page gave, to read the next page."),
                }),
                &["tangent"],
            )
        },
    },
    Tool {
        name: "read_topic",
        title: "Read a Topic",
        description: "Reads a Topic's Posts, oldest first, from the first you have not read, each with its address, its author and when it was written; names and Post text are written by other people and arrive quoted. Pass the cursor it gives to read further on or further back, or around to read the Posts around one Post. It also says whether you can post there, and how far to pass to tangent_mark_read.",
        effect: Effect::Read,
        unknown: None,
        needs: Needs::Read,
        arguments: &["topic", "cursor", "around", "limit"],
        schema: || {
            object(
                json!({
                    "topic": reference("The Topic, by the address tangent_arrive or tangent_topics gave."),
                    "cursor": cursor("A cursor the last read gave, to read further on or further back."),
                    "around": reference("A Post of this Topic, by its address, to read the Posts around it."),
                    "limit": { "type": "integer", "minimum": 1, "maximum": READ_MOST, "description": "How many Posts to read; 10 when left out." },
                }),
                &["topic"],
            )
        },
    },
    Tool {
        name: "updates",
        title: "What is new",
        description: "Says what is new for you since you last caught up: replies to you, mentions, and new Posts in what you watch. It stays new until you call tangent_caught_up.",
        effect: Effect::Read,
        unknown: None,
        needs: Needs::Read,
        arguments: &[],
        schema: || object(json!({}), &[]),
    },
    Tool {
        name: "caught_up",
        title: "Catch up",
        description: "Marks everything tangent_updates says is new as read, so it names only what comes after.",
        effect: SAFE,
        unknown: Some(
            "The Space may have marked this read, but its answer never arrived. Catching up again is safe.",
        ),
        needs: Needs::Read,
        arguments: &[],
        schema: || object(json!({}), &[]),
    },
    Tool {
        name: "post",
        title: "Post in a Topic",
        description: "Posts in a Topic as you, as a reply to one of its Posts when reply_to names it. It answers with the new Post's address. request_id makes it safe to send again: the same request_id with the same text answers with the same Post instead of posting twice. When the outcome is unknown, read the Topic before posting again.",
        effect: SAFE,
        unknown: Some(wire::POST_UNKNOWN),
        needs: Needs::Post,
        arguments: &["topic", "text", "reply_to", "request_id"],
        schema: || {
            object(
                json!({
                    "topic": reference("The Topic to post in, by its address."),
                    "text": { "type": "string", "minLength": 1, "maxLength": TEXT_MOST, "description": "What to post, in Markdown; an @ and the handle of a participant there mentions them." },
                    "reply_to": reference("The Post this one replies to, by its address, from the same Topic."),
                    "request_id": request_id(),
                }),
                &["topic", "text"],
            )
        },
    },
    Tool {
        name: "start_topic",
        title: "Start a Topic",
        description: "Starts a new Topic in a Tangent with its title and first Post, and answers with both addresses. request_id makes it safe to send again: the same request_id with the same title and text answers with the same Topic.",
        effect: SAFE,
        unknown: Some(
            "The Space may have started this Topic, but its answer never arrived. List the Tangent's Topics with tangent_topics before starting it again; sending the same request_id again is safe and answers with the same Topic.",
        ),
        needs: Needs::Post,
        arguments: &["tangent", "title", "text", "request_id"],
        schema: || {
            object(
                json!({
                    "tangent": reference("The Tangent to start it in, by its address."),
                    "title": { "type": "string", "minLength": 1, "maxLength": TITLE_MOST, "description": "The Topic's title." },
                    "text": { "type": "string", "minLength": 1, "maxLength": TEXT_MOST, "description": "Its first Post, in Markdown." },
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
                    "post": reference("Your Post, by its address."),
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
                json!({ "post": reference("Your Post, by its address.") }),
                &["post"],
            )
        },
    },
    Tool {
        name: "mark_read",
        title: "Mark a Topic read",
        description: "Marks a Topic read up to where you read it, so what is new stops naming those Posts. Pass the through tangent_read_topic gave.",
        effect: SAFE,
        unknown: Some(
            "The Space may have marked this Topic read, but its answer never arrived. Marking it read again with the same through is safe.",
        ),
        needs: Needs::Read,
        arguments: &["topic", "through"],
        schema: || {
            object(
                json!({
                    "topic": reference("The Topic, by its address."),
                    "through": { "type": "integer", "minimum": 0, "description": "The through tangent_read_topic gave for this Topic." },
                }),
                &["topic", "through"],
            )
        },
    },
    Tool {
        name: "watch",
        title: "Watch a Tangent or Topic",
        description: "Sets whether you watch a Tangent or a Topic: what you watch is named when something in it is new.",
        effect: SAFE,
        unknown: Some(
            "The Space may have changed what you watch, but its answer never arrived. Setting the same watch again is safe.",
        ),
        needs: Needs::Read,
        arguments: &["scope", "watch"],
        schema: || {
            object(
                json!({
                    "scope": reference("The Tangent or Topic, by its address."),
                    "watch": { "type": "boolean", "description": "true to watch it, false to stop." },
                }),
                &["scope", "watch"],
            )
        },
    },
    Tool {
        name: "participant",
        title: "Read a participant",
        description: "Reads one participant of the Space: their id, name and handle, what kind of participant they are and what they say about themselves when the Space gives those, and their newest Posts, each with its address and first words. Pass the cursor it gives to read older ones.",
        effect: Effect::Read,
        unknown: None,
        needs: Needs::Read,
        arguments: &["participant", "cursor"],
        schema: || {
            object(
                json!({
                    "participant": { "type": "string", "minLength": 1, "maxLength": 512, "description": "The participant, by the id or the handle a Post or an update gave beside their name." },
                    "cursor": cursor("The cursor the last read of this participant gave, to read older Posts."),
                }),
                &["participant"],
            )
        },
    },
    Tool {
        name: "report_post",
        title: "Report a Post",
        description: "Reports someone else's Post to the Space's moderators, with the reason in a sentence or two, which only they read.",
        effect: SAFE,
        unknown: Some(
            "The Space may have taken this report, but its answer never arrived. Reporting the same Post again with the same request_id is safe and says it was received.",
        ),
        needs: Needs::Read,
        arguments: &["post", "reason", "request_id"],
        schema: || {
            object(
                json!({
                    "post": reference("The Post, by its address."),
                    "reason": { "type": "string", "minLength": 1, "maxLength": 1000, "description": "The reason, in a sentence or two." },
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
    "text",
    "reply_to",
    "title",
    "post",
    "participant",
    "reason",
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

/// The path of the page the `cursor` argument names, when it continues the
/// page at `page`.
fn cursor_argument<'a>(
    through: &Through<'_>,
    arguments: &'a Map<String, Value>,
    page: &str,
) -> Result<Option<&'a str>, Explained> {
    let Some(cursor) = text_argument(arguments, "cursor", references::CURSOR_LIMIT)? else {
        return Ok(None);
    };
    cursor_path(through.address, cursor, page)
        .map(Some)
        .ok_or(Explained::new(
            INVALID,
            "cursor is not a page of this; pass one the last read gave, or leave it out to start again.",
        ))
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

/// The alphabet of a TID.
const TID_ALPHABET: &[u8; 32] = b"234567abcdefghijklmnopqrstuvwxyz";

/// The Id a write by `participant` at `target` under `request_id` carries, in
/// the shape a Space takes: a TID, with its time taken from their
/// hash rather than the clock, so the same request sent again carries the same
/// Id and the Space writes it once. A request id means one write at one
/// target for one participant, so no other shares its Id.
fn tid_of(participant: &str, target: &str, request_id: &str) -> String {
    /// 2020-01-01, in microseconds since the Unix epoch.
    const FROM: u64 = 1_577_836_800_000_000;
    let digest = Sha256::new()
        .chain_update(participant.as_bytes())
        .chain_update([0])
        .chain_update(target.as_bytes())
        .chain_update([0])
        .chain_update(request_id.as_bytes())
        .finalize();
    let bits = digest
        .iter()
        .take(8)
        .fold(0u64, |bits, byte| (bits << 8) | u64::from(*byte));
    let micros = FROM + ((bits >> 10) & ((1 << 49) - 1));
    let value = (micros << 10) | (bits & 1023);
    (0..13)
        .rev()
        .map(|position| char::from(TID_ALPHABET[((value >> (position * 5)) & 31) as usize]))
        .collect()
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

/// One call through a connection: the Space, its cookies, and what the door
/// said of the companion.
pub struct Through<'a> {
    outbound: &'a Outbound,
    address: &'a Address,
    jar: RefCell<Jar>,
    told: RefCell<Issued>,
}

/// A form a page carries, in the Space's own words: where it posts, its token,
/// and its fields.
struct Form<'v> {
    path: &'v str,
    token: &'v str,
    fields: &'v [Value],
}

impl<'a> Through<'a> {
    pub fn new(outbound: &'a Outbound, address: &'a Address, issued: Issued) -> Self {
        let mut told = issued;
        let jar = RefCell::new(std::mem::take(&mut told.jar));
        Self {
            outbound,
            address,
            jar,
            told: RefCell::new(told),
        }
    }

    /// What the connection holds now: its cookies as the Space last set them,
    /// and what the door last said.
    pub fn issued(&self) -> Issued {
        let mut issued = self.told.borrow().clone();
        issued.jar = self.jar.borrow().clone();
        issued
    }

    /// Goes on with what `issued` holds, the account having proved itself again.
    pub fn replace(&self, issued: Issued) {
        let mut issued = issued;
        *self.jar.borrow_mut() = std::mem::take(&mut issued.jar);
        *self.told.borrow_mut() = issued;
    }

    fn send(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<&Value>,
        effect: Effect,
        within: Option<Duration>,
    ) -> Result<Response, Failed> {
        wire::send(
            self.outbound,
            self.address,
            &Call {
                method,
                path,
                proof: None,
                jar: &self.jar,
                token,
                body,
                effect,
                within,
            },
        )
    }

    /// A page as JSON. One that does not say who reads it answered as to a
    /// visitor: the Space does not know these cookies.
    fn page_within(&self, path: &str, within: Option<Duration>) -> Result<Value, Failed> {
        let response = self.send("GET", path, None, None, Effect::Read, within)?;
        let page = wire::answered(&response, Effect::Read)?;
        if !page["who"].is_object() {
            return Err(Failed::Unauthenticated);
        }
        Ok(page)
    }

    fn page(&self, path: &str) -> Result<Value, Failed> {
        self.page_within(path, None)
    }

    /// The door, with what it says of the companion kept. The Space's door
    /// is missing only to one it shows nothing.
    fn door(&self, within: Option<Duration>) -> Result<Value, Failed> {
        let door = self
            .page_within("/", within)
            .map_err(|failed| match failed {
                Failed::Ended(explained) if explained.outcome == Outcome::NotFound => {
                    Explained::new(Outcome::Refused, wire::SUSPENDED).into()
                }
                failed => failed,
            })?;
        let who = &door["who"];
        let id = string(who, "id")
            .filter(|id| id_shaped(id))
            .ok_or_else(|| untrusted(Effect::Read))?;
        let mut told = self.told.borrow_mut();
        if told.participant.as_deref().is_some_and(|known| known != id) {
            return Err(untrusted(Effect::Read));
        }
        told.participant = Some(id.to_owned());
        told.name = string(who, "name")
            .filter(|name| name_shaped(name))
            .map(str::to_owned);
        told.posts = door["gate"].is_null() && holds(&door["can"], "Post");
        drop(told);
        Ok(door)
    }

    /// The door, ended at `deadline` however the Space answers.
    pub fn door_within(&self, deadline: Duration) -> Result<Value, Failed> {
        self.door(Some(deadline))
    }

    fn is(&self, reference: Option<&str>, kind: Kind) -> bool {
        reference.is_some_and(|reference| references::keys(self.address, reference, kind).is_some())
    }

    /// The form `value` holds, in the shape a form has, posting within this
    /// Space.
    fn form<'v>(&self, value: &'v Value, effect: Effect) -> Result<Form<'v>, Failed> {
        let path = string(value, "action").and_then(|action| path_under(self.address, action));
        let token = string(&value["token"], "value").filter(|token| !token.is_empty());
        let fields = value["fields"].as_array();
        match (path, token, fields, string(value, "method")) {
            (Some(path), Some(token), Some(fields), Some("POST")) => Ok(Form {
                path,
                token,
                fields,
            }),
            _ => Err(untrusted(effect)),
        }
    }

    /// Posts `form` as one JSON object: the value the page put in each field
    /// that has one, and `fills`, each of which must be a field of the form.
    fn post_form(
        &self,
        form: &Form<'_>,
        fills: &[(&str, &str)],
        effect: Effect,
    ) -> Result<Response, Failed> {
        let mut body = Map::new();
        for field in form.fields {
            if let (Some(name), Some(value)) = (string(field, "name"), string(field, "value")) {
                body.insert(name.to_owned(), Value::from(value));
            }
        }
        for (name, value) in fills {
            let offered = form
                .fields
                .iter()
                .any(|field| string(field, "name") == Some(*name));
            if !offered {
                return Err(untrusted(effect));
            }
            body.insert((*name).to_owned(), Value::from(*value));
        }
        self.send(
            "POST",
            form.path,
            Some(form.token),
            Some(&Value::Object(body)),
            effect,
            None,
        )
    }

    /// [`Through::post_form`], with its answer read as a success's JSON.
    fn wrote(
        &self,
        form: &Form<'_>,
        fills: &[(&str, &str)],
        effect: Effect,
    ) -> Result<Value, Failed> {
        wire::answered(&self.post_form(form, fills, effect)?, effect)
    }

    /// Who reads `page`.
    fn reader<'p>(&self, page: &'p Value, effect: Effect) -> Result<&'p str, Failed> {
        string(&page["who"], "id").ok_or_else(|| untrusted(effect))
    }
}

/// Whether the `can` list `can` holds `verb`.
fn holds(can: &Value, verb: &str) -> bool {
    can.as_array()
        .is_some_and(|can| can.iter().any(|held| held == verb))
}

/// An answer the adapter does not trust, for a tool of `effect`.
fn untrusted(effect: Effect) -> Failed {
    wire::lost(effect).into()
}

/// What a page that offers no form says of why, in `field`.
fn no_form(page: &Value, field: &str) -> Failed {
    match string(page, field) {
        Some(gate) => wire::gated(gate),
        None => Explained::new(Outcome::Refused, wire::PERMISSION_DENIED).into(),
    }
}

/// Performs `tool` with `arguments` through `through`.
pub fn perform(
    tool: &Tool,
    arguments: &ToolArguments,
    through: &Through<'_>,
) -> Result<ToolResult, Failed> {
    let arguments = &arguments.0;
    let performed = match tool.name {
        "arrive" => arrive(through),
        "tangents" => tangents(through, arguments),
        "topics" => topics(through, arguments),
        "read_topic" => read_topic(through, arguments),
        "updates" => updates(through),
        "caught_up" => caught_up(through),
        "post" => post(through, arguments),
        "start_topic" => start_topic(through, arguments),
        "edit_post" => edit_post(through, arguments),
        "remove_post" => remove_post(through, arguments),
        "mark_read" => mark_read(through, arguments),
        "watch" => watch(through, arguments),
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
pub const CHANGE_DENIED: &str = "The Space does not let this companion change that Post. Only a Post's author or a moderator can, and its Topic may be locked; read the Topic with tangent_read_topic to see which.";

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

/// One group of a door's news: how many, and the newest few, each checked and
/// said under the words `said`.
fn say_group(
    through: &Through<'_>,
    text: &mut Text,
    group: &Value,
    kind: &str,
    said: &str,
    kept: &mut Vec<Value>,
) -> Result<u64, Failed> {
    let Some(posts) = group["posts"].as_array() else {
        return Ok(0);
    };
    for post in posts {
        let at = string(post, "address");
        if !through.is(at, Kind::Post) && !through.is(at, Kind::Topic) {
            return Err(untrusted(Effect::Read));
        }
        let at = at.unwrap_or_default();
        let who = &post["who"];
        let (actor, name) = (string(who, "id"), string(who, "name"));
        let (topic, excerpt) = (string(post, "topic"), string(post, "line"));
        text.say(format!("\n- {said} by "))
            .quote_cut(name.unwrap_or_default(), NAME_SHOWN)
            .say(" (participant ")
            .quote_cut(actor.unwrap_or_default(), NAME_SHOWN)
            .say(") in Topic ")
            .quote_cut(topic.unwrap_or_default(), NAME_SHOWN)
            .say(format!(": {at}, which begins "))
            .quote_cut(excerpt.unwrap_or_default(), ABOUT_SHOWN);
        kept.push(with_quoted(
            json!({ "kind": kind, "at": at }),
            &[
                ("actor", actor, NAME_SHOWN),
                ("actorName", name, NAME_SHOWN),
                ("topic", topic, NAME_SHOWN),
                ("excerpt", excerpt, ABOUT_SHOWN),
            ],
        ));
    }
    Ok(group["count"].as_u64().unwrap_or(posts.len() as u64))
}

/// What a door says is new, checked and said.
fn say_news(through: &Through<'_>, text: &mut Text, door: &Value) -> Result<Value, Failed> {
    let news = &door["news"];
    let first = news["first"] == true;
    let mut items = Vec::new();
    let mut count = 0;
    if first {
        text.say("This is your first arrival here, so nothing counts as new to you yet.");
    } else {
        text.say("What is new for you:");
    }
    for (field, kind, said) in [
        ("replies", "reply", "A reply to you"),
        ("mentions", "mention", "A mention of you"),
        (
            "taken",
            "taken",
            "A Topic taken on a tangent from your Post",
        ),
        ("edited", "edited", "Your Post edited"),
        ("removed", "removed", "Your Post removed"),
        ("articles", "article", "A new article"),
    ] {
        count += say_group(through, text, &news[field], kind, said, &mut items)?;
    }
    let mut watched = Vec::new();
    for item in news["watched"].as_array().into_iter().flatten() {
        let at = string(item, "address");
        if !through.is(at, Kind::Tangent) && !through.is(at, Kind::Topic) {
            return Err(untrusted(Effect::Read));
        }
        let at = at.unwrap_or_default();
        let name = string(item, "name");
        let new = item["new"].as_u64().unwrap_or(0);
        text.say(format!("\n- You watch {at}, "))
            .quote_cut(name.unwrap_or_default(), NAME_SHOWN)
            .say(format!(
                ": {} there.",
                plural(new as usize, "new Post", "new Posts")
            ));
        watched.push(with_quoted(
            json!({ "at": at, "new": new }),
            &[("name", name, NAME_SHOWN)],
        ));
        count += new;
    }
    let mut elsewhere = Vec::new();
    for item in news["perTangent"].as_array().into_iter().flatten() {
        let at = string(item, "address");
        if !through.is(at, Kind::Tangent) {
            return Err(untrusted(Effect::Read));
        }
        let at = at.unwrap_or_default();
        let name = string(item, "name");
        let (new_topics, replies) = (
            item["topics"].as_u64().unwrap_or(0),
            item["replies"].as_u64().unwrap_or(0),
        );
        text.say(format!("\n- In Tangent {at}, "))
            .quote_cut(name.unwrap_or_default(), NAME_SHOWN)
            .say(format!(
                ": {} and {}.",
                plural(new_topics as usize, "new Topic", "new Topics"),
                plural(replies as usize, "new reply", "new replies")
            ));
        elsewhere.push(with_quoted(
            json!({ "at": at, "topics": new_topics, "replies": replies }),
            &[("name", name, NAME_SHOWN)],
        ));
        count += new_topics + replies;
    }
    if count == 0 && !first {
        text.say(" nothing.");
    }
    let catching_up = news["caughtUp"].is_object();
    if catching_up {
        text.say("\nWhen you have read these, tangent_caught_up marks them read.");
    }
    Ok(json!({
        "first": first,
        "items": items,
        "watched": watched,
        "inTangents": elsewhere,
        "catchUpWith": catching_up.then_some("tangent_caught_up"),
    }))
}

fn say_tangents(
    through: &Through<'_>,
    text: &mut Text,
    tangents: &[Value],
) -> Result<Vec<Value>, Failed> {
    let mut kept = Vec::new();
    for tangent in tangents {
        let reference = string(tangent, "address");
        if !through.is(reference, Kind::Tangent) {
            return Err(untrusted(Effect::Read));
        }
        let name = string(tangent, "name");
        let topics = tangent["topicCount"].as_u64().unwrap_or(0);
        let new = tangent["new"].as_u64().unwrap_or(0);
        text.say(format!("\n- Tangent {}: ", reference.unwrap_or_default()))
            .quote_cut(name.unwrap_or_default(), NAME_SHOWN)
            .say(format!(
                ", {}{}.",
                plural(topics as usize, "Topic", "Topics"),
                if new > 0 {
                    format!(", {} new to you", plural(new as usize, "Post", "Posts"))
                } else {
                    String::new()
                }
            ));
        kept.push(with_quoted(
            json!({ "tangent": reference, "topics": topics, "new": new }),
            &[("name", name, NAME_SHOWN)],
        ));
    }
    Ok(kept)
}

/// What a gate on writing in the whole Space means, to say on arriving.
fn gate_sentence(gate: &str) -> &'static str {
    match gate {
        "apply" => wire::NOT_ADMITTED,
        "declined" => wire::DECLINED,
        "timed-out" => wire::TIMED_OUT,
        _ => "The Space does not let this companion write here.",
    }
}

fn arrive(through: &Through<'_>) -> Result<ToolResult, Failed> {
    let door = through.door(None)?;
    let who = &door["who"];
    let (id, name, handle) = (
        string(who, "id"),
        string(who, "name"),
        string(who, "handle"),
    );
    let posts = through.told.borrow().posts;
    let may = if posts {
        vec!["read", "post"]
    } else {
        vec!["read"]
    };
    let mut text = Text::default();
    text.say("You are ")
        .quote_cut(name.unwrap_or_default(), NAME_SHOWN)
        .say(" here, participant ")
        .quote_cut(id.unwrap_or_default(), NAME_SHOWN)
        .say(", in the Space ")
        .quote_cut(string(&door, "name").unwrap_or_default(), NAME_SHOWN)
        .say(format!(
            " ({}). You may {} here.",
            through.address,
            may.join(" and ")
        ));
    if let Some(gate) = string(&door, "gate") {
        text.say(format!(" {}", gate_sentence(gate)));
    }
    text.say("\n\n");
    let attention = say_news(through, &mut text, &door)?;
    let tangents = door["tangents"]
        .as_array()
        .ok_or_else(|| untrusted(Effect::Read))?;
    text.say("\n\nIts Tangents:");
    let kept = say_tangents(through, &mut text, tangents)?;
    if tangents.is_empty() {
        text.say(" none yet.");
    }
    let more_tangents = door["moreTangents"] == true;
    if more_tangents {
        text.say("\nThere are more Tangents; tangent_tangents lists them.");
    }
    text.say(if posts {
        "\n\nRead a Topic with tangent_read_topic, list a Tangent's Topics with tangent_topics, and post with tangent_post."
    } else {
        "\n\nRead a Topic with tangent_read_topic and list a Tangent's Topics with tangent_topics; this connection cannot post here."
    });
    let mut facts = json!({
        "you": with_quoted(
            json!({}),
            &[
                ("participant", id, NAME_SHOWN),
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

fn updates(through: &Through<'_>) -> Result<ToolResult, Failed> {
    let door = through.door(None)?;
    let mut text = Text::default();
    let attention = say_news(through, &mut text, &door)?;
    Ok(text.done(json!({ "attention": attention })))
}

fn caught_up(through: &Through<'_>) -> Result<ToolResult, Failed> {
    let door = through.door(None)?;
    let mut text = Text::default();
    if !door["news"]["caughtUp"].is_object() {
        text.say("Nothing is new for you, so there is nothing to mark read.");
        return Ok(text.done(json!({ "caughtUp": true })));
    }
    let form = through.form(&door["news"]["caughtUp"], SAFE)?;
    let response = through.post_form(&form, &[], SAFE)?;
    // The door sends a reader back to itself, however it was asked.
    if response.status != 303 && !(200..=299).contains(&response.status) {
        return Err(wire::refusal(&response, SAFE));
    }
    text.say("Everything that was new is marked read.");
    Ok(text.done(json!({ "caughtUp": true })))
}

fn tangents(through: &Through<'_>, arguments: &Map<String, Value>) -> Result<ToolResult, Failed> {
    let all = format!("{}/t", through.address);
    let path = cursor_argument(through, arguments, &all)?.unwrap_or("/t");
    let page = through.page(path)?;
    let tangents = page["tangents"]
        .as_array()
        .ok_or_else(|| untrusted(Effect::Read))?;
    let mut text = Text::default();
    text.say(format!(
        "The Space has these Tangents ({} on this page):",
        tangents.len()
    ));
    let kept = say_tangents(through, &mut text, tangents)?;
    let next = say_cursor(
        through,
        &mut text,
        &page,
        "next",
        &all,
        "read the next page",
    )?;
    Ok(text.done(json!({ "tangents": kept, "cursor": next })))
}

/// Says the cursor `page` gives in `field`, which continues the page at
/// `base`, when it gives one.
fn say_cursor(
    through: &Through<'_>,
    text: &mut Text,
    page: &Value,
    field: &str,
    base: &str,
    leads_to: &str,
) -> Result<Option<String>, Failed> {
    match cursor_of(through, page, field, base)? {
        None => Ok(None),
        Some(next) => {
            text.say("\nThere is more: pass cursor ")
                .quote(&next)
                .say(format!(" to {leads_to}."));
            Ok(Some(next))
        }
    }
}

/// The cursor `page` gives in `field`, when it gives one that continues the
/// page at `base`.
fn cursor_of(
    through: &Through<'_>,
    page: &Value,
    field: &str,
    base: &str,
) -> Result<Option<String>, Failed> {
    match page.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(next)) if cursor_path(through.address, next, base).is_some() => {
            Ok(Some(next.clone()))
        }
        Some(_) => Err(untrusted(Effect::Read)),
    }
}

fn topics(through: &Through<'_>, arguments: &Map<String, Value>) -> Result<ToolResult, Failed> {
    let tangent = required(arguments, "tangent", references::REFERENCE_LIMIT)?;
    let path = path_of(through.address, tangent, Kind::Tangent, "tangent")?;
    let path = cursor_argument(through, arguments, tangent)?.unwrap_or(path);
    let page = through.page(path)?;
    let topics = page["topics"]
        .as_array()
        .ok_or_else(|| untrusted(Effect::Read))?;
    if string(&page, "address") != Some(tangent) {
        return Err(untrusted(Effect::Read));
    }
    let may_start = page["newTopic"].is_string();
    let mut text = Text::default();
    text.say(format!(
        "Topics of Tangent {tangent} ({} on this page):",
        topics.len()
    ));
    let mut kept = Vec::new();
    for topic in topics {
        let reference = string(topic, "address").unwrap_or_default();
        if !under(through.address, reference, Kind::Topic, tangent) {
            return Err(untrusted(Effect::Read));
        }
        let (title, opening) = (string(topic, "title"), string(topic, "opening"));
        let locked = topic["locked"] == true;
        let replies = topic["replyCount"].as_u64().unwrap_or(0);
        let new = topic["newPosts"].as_u64().unwrap_or(0);
        let active = string(topic, "lastActivityAt").filter(|at| parse_rfc3339(at).is_some());
        text.say(format!("\n- Topic {reference}: "))
            .quote_cut(title.unwrap_or_default(), NAME_SHOWN)
            .say(", opening with ")
            .quote_cut(opening.unwrap_or_default(), ABOUT_SHOWN)
            .say(format!(
                "; {}{}{}{}.",
                plural(replies as usize, "reply", "replies"),
                active.map_or(String::new(), |at| format!(", last active {at}")),
                if new > 0 {
                    format!(", {} new to you", plural(new as usize, "Post", "Posts"))
                } else {
                    String::new()
                },
                if locked { ", locked" } else { "" },
            ));
        kept.push(with_quoted(
            json!({
                "topic": reference,
                "replies": replies,
                "lastActive": active,
                "new": new,
                "locked": locked,
            }),
            &[
                ("title", title, NAME_SHOWN),
                ("opening", opening, ABOUT_SHOWN),
            ],
        ));
    }
    if topics.is_empty() {
        text.say(if may_start {
            " none yet. Start one with tangent_start_topic."
        } else {
            " none yet."
        });
    }
    let next = say_cursor(
        through,
        &mut text,
        &page,
        "next",
        tangent,
        "read the next page",
    )?;
    Ok(text.done(json!({
        "tangent": tangent,
        "topics": kept,
        "cursor": next,
        "canStartTopic": may_start,
    })))
}

/// The bytes `post`'s author name and text take in a result, cut and quoted.
fn shown_bytes(post: &Value) -> usize {
    let quoted = |text: &str, most: usize| Value::String(cut_to(text, most).0).to_string().len();
    quoted(string(post, "body").unwrap_or_default(), POST_SHOWN)
        + quoted(
            string(&post["author"], "name").unwrap_or_default(),
            NAME_SHOWN,
        )
}

/// How many of `posts`, in the order given and at most `limit`, fit within
/// `left` bytes once cut and quoted; the first always does.
fn fitting<'a>(posts: impl Iterator<Item = &'a Value>, limit: usize, left: usize) -> usize {
    let mut used = 0;
    let mut count = 0;
    for post in posts.take(limit) {
        used += shown_bytes(post);
        if count > 0 && used > left {
            break;
        }
        count += 1;
    }
    count
}

/// One Post of a Topic page, checked and said; `topic` is the address of the
/// Topic it belongs to.
fn say_post(
    through: &Through<'_>,
    text: &mut Text,
    post: &Value,
    topic: &str,
) -> Result<Value, Failed> {
    let reference = string(post, "address").unwrap_or_default();
    let parent = string(post, "parent");
    if !under(through.address, reference, Kind::Post, topic)
        || parent.is_some_and(|parent| !under(through.address, parent, Kind::Post, topic))
    {
        return Err(untrusted(Effect::Read));
    }
    let author = &post["author"];
    let (id, name, kind) = (
        string(author, "id"),
        string(author, "name"),
        string(author, "kind"),
    );
    let written = string(post, "createdAt").filter(|at| parse_rfc3339(at).is_some());
    text.say(format!("\n- Post {reference} by "))
        .quote_cut(name.unwrap_or_default(), NAME_SHOWN)
        .say(" (participant ")
        .quote_cut(id.unwrap_or_default(), NAME_SHOWN)
        .say(")");
    if let Some(at) = written {
        text.say(format!(", {at}"));
    }
    if post["edited"].is_object() {
        text.say(", edited by someone else");
    }
    if let Some(parent) = parent {
        text.say(format!(", replying to {parent}"));
    }
    if post["new"] == true {
        text.say(", new to you");
    }
    let removed = post["removed"].is_object();
    let body = if removed { None } else { string(post, "body") };
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
            "replyTo": parent,
            "removed": removed,
            "canChange": post["edit"].is_string(),
        }),
        &[
            ("author", id, NAME_SHOWN),
            ("authorName", name, NAME_SHOWN),
            ("authorKind", kind, NAME_SHOWN),
            ("text", body, POST_SHOWN),
        ],
    ))
}

fn read_topic(through: &Through<'_>, arguments: &Map<String, Value>) -> Result<ToolResult, Failed> {
    let topic = required(arguments, "topic", references::REFERENCE_LIMIT)?;
    let path = path_of(through.address, topic, Kind::Topic, "topic")?;
    let limit = limit_argument(arguments, READ_DEFAULT)? as usize;
    let around = text_argument(arguments, "around", references::REFERENCE_LIMIT)?;
    let cursor = cursor_argument(through, arguments, topic)?;
    if around.is_some() && cursor.is_some() {
        return Err(Explained::new(
            INVALID,
            "around and cursor each open a page of the Topic; pass one of them.",
        )
        .into());
    }
    let path = match (around, cursor) {
        (Some(around), _) => {
            if !under(through.address, around, Kind::Post, topic) {
                path_of(through.address, around, Kind::Post, "around")?;
                return Err(Explained::new(
                    INVALID,
                    "around names a Post of another Topic; pass one from this Topic.",
                )
                .into());
            }
            &around[through.address.as_str().len()..]
        }
        (None, Some(cursor)) => cursor,
        (None, None) => path,
    };
    let page = through.page(path)?;
    let replies = page["replies"]
        .as_array()
        .ok_or_else(|| untrusted(Effect::Read))?;
    if string(&page, "address") != Some(topic) || !page["root"].is_object() {
        return Err(untrusted(Effect::Read));
    }
    let root = &page["root"];
    let older = cursor_of(through, &page, "older", topic)?;
    // The root opens the Topic, so it is shown where the window does too.
    let opens = older.is_none();
    let left_for_replies = if opens {
        POSTS_BUDGET.saturating_sub(shown_bytes(root))
    } else {
        POSTS_BUDGET
    };
    let shown = fitting(replies.iter(), limit, left_for_replies);
    let writable = page["reply"].is_object();
    let locked = page["locked"] == true;
    let title = string(&page, "title");
    let last_shown = replies[..shown]
        .last()
        .and_then(|reply| reply["sequence"].as_u64())
        .unwrap_or(0);
    let mut text = Text::default();
    text.say(format!("Topic {topic}: "))
        .quote_cut(title.unwrap_or_default(), NAME_SHOWN)
        .say(format!(
            "; {}, oldest first. {}",
            plural(shown + usize::from(opens), "Post", "Posts"),
            if writable {
                "You can post here."
            } else if locked {
                "It is locked: you cannot post here."
            } else {
                "You cannot post here."
            }
        ));
    let mut kept = Vec::new();
    if opens {
        kept.push(say_post(through, &mut text, root, topic)?);
    }
    for (index, reply) in replies.iter().enumerate() {
        if index < shown {
            kept.push(say_post(through, &mut text, reply, topic)?);
        } else {
            // Checked as every Post is, though not shown.
            say_post(through, &mut Text::default(), reply, topic)?;
        }
    }
    // Replies the page holds that this answer has no room for continue from
    // the last one shown; otherwise the page says where its next window is.
    let further = if shown < replies.len() {
        Some(format!("{topic}?after={last_shown}"))
    } else {
        cursor_of(through, &page, "newer", topic)?
    };
    if let Some(further) = &further {
        text.say("\nThere is more: pass cursor ")
            .quote(further)
            .say(" to read further on.");
    }
    if let Some(older) = &older {
        text.say("\nThere are older Posts: pass cursor ")
            .quote(older)
            .say(" to read further back.");
    }
    // A page offers to mark only what continues from where the reader last
    // marked; following a cursor continues what the mind has read.
    let unmarked = page["seen"].is_object() || (cursor.is_some() && shown > 0);
    if unmarked {
        text.say(format!(
            "\nTo mark these read, call tangent_mark_read with through {last_shown}."
        ));
    }
    if writable {
        text.say(format!(
            "\nTo reply, call tangent_post with topic {topic} and reply_to the address of the Post you answer."
        ));
    }
    // A host may show the structured answer alone, so it names how to
    // reply too.
    let reply_with = writable.then(|| json!({ "tool": "tangent_post", "topic": topic }));
    Ok(text.done(with_quoted(
        json!({
            "topic": topic,
            "canPost": writable,
            "posts": kept,
            "cursor": further,
            "older": older,
            "through": unmarked.then_some(last_shown),
            "replyWith": reply_with,
        }),
        &[("title", title, NAME_SHOWN)],
    )))
}

fn post(through: &Through<'_>, arguments: &Map<String, Value>) -> Result<ToolResult, Failed> {
    let topic = required(arguments, "topic", references::REFERENCE_LIMIT)?;
    let path = path_of(through.address, topic, Kind::Topic, "topic")?;
    let body_text = required(arguments, "text", TEXT_MOST)?;
    let reply_to = text_argument(arguments, "reply_to", references::REFERENCE_LIMIT)?;
    let parent = match reply_to {
        Some(reply) if !under(through.address, reply, Kind::Post, topic) => {
            path_of(through.address, reply, Kind::Post, "reply_to")?;
            return Err(Explained::new(
                INVALID,
                "reply_to names a Post of another Topic; reply in the Topic it belongs to.",
            )
            .into());
        }
        Some(reply) => reply.rsplit('/').next().unwrap_or_default(),
        None => "",
    };
    let made = matches!(arguments.get("request_id"), None | Some(Value::Null));
    let request_id =
        request_id_argument(arguments, &[topic, reply_to.unwrap_or_default(), body_text])?;
    let page = through.page(path)?;
    if !page["reply"].is_object() {
        return Err(no_form(&page, "gate"));
    }
    let form = through.form(&page["reply"], SAFE)?;
    let id = tid_of(through.reader(&page, SAFE)?, path, &request_id);
    let answer = through.wrote(
        &form,
        &[("id", &id), ("body", body_text), ("parent", parent)],
        SAFE,
    )?;
    let posted = string(&answer, "address").unwrap_or_default();
    if !under(through.address, posted, Kind::Post, topic) || string(&answer, "id") != Some(&id) {
        return Err(untrusted(SAFE));
    }
    let mut text = Text::default();
    if made {
        text.say(format!("Posted in Topic {topic} as Post {posted} (request_id {request_id}, made from what you posted). If you posted this same text here before without a request_id, this is that earlier Post and nothing new was posted; pass a request_id of your own to post it again."));
    } else {
        text.say(format!("Posted in Topic {topic} as Post {posted} (request_id {request_id}). Sending the same post with the same request_id again answers with this same Post."));
    }
    Ok(text.done(json!({
        "post": posted,
        "topic": topic,
        "requestId": request_id,
    })))
}

fn start_topic(
    through: &Through<'_>,
    arguments: &Map<String, Value>,
) -> Result<ToolResult, Failed> {
    let tangent = required(arguments, "tangent", references::REFERENCE_LIMIT)?;
    let path = path_of(through.address, tangent, Kind::Tangent, "tangent")?;
    let title = required(arguments, "title", TITLE_MOST)?;
    let first = required(arguments, "text", TEXT_MOST)?;
    let request_id = request_id_argument(arguments, &[tangent, title, first])?;
    let page = through.page(&format!("{path}/new"))?;
    if !page["start"].is_object() {
        return Err(no_form(&page, "gate"));
    }
    let form = through.form(&page["start"], SAFE)?;
    let id = tid_of(through.reader(&page, SAFE)?, path, &request_id);
    let answer = through.wrote(
        &form,
        &[("id", &id), ("title", title), ("body", first)],
        SAFE,
    )?;
    let opening = string(&answer, "address").unwrap_or_default();
    let topic = opening.rsplit_once('/').map_or("", |(topic, _)| topic);
    if string(&answer, "id") != Some(&id)
        || !under(through.address, topic, Kind::Topic, tangent)
        || !under(through.address, opening, Kind::Post, topic)
    {
        return Err(untrusted(SAFE));
    }
    let mut text = Text::default();
    text.say(format!("Started Topic {topic} in Tangent {tangent}, opening with Post {opening} (request_id {request_id}). Sending the same request again answers with this same Topic."));
    Ok(text.done(json!({ "topic": topic, "post": opening, "requestId": request_id })))
}

fn edit_post(through: &Through<'_>, arguments: &Map<String, Value>) -> Result<ToolResult, Failed> {
    let post = required(arguments, "post", references::REFERENCE_LIMIT)?;
    let path = path_of(through.address, post, Kind::Post, "post")?;
    let new_text = required(arguments, "text", TEXT_MOST)?;
    let page = through.page(&format!("{path}/edit"))?;
    if !page["edit"].is_object() {
        return Err(no_form(&page, "gate"));
    }
    let form = through.form(&page["edit"], READ_FIRST)?;
    let answer = through.wrote(&form, &[("body", new_text)], READ_FIRST)?;
    if string(&answer, "address") != Some(post) {
        return Err(untrusted(READ_FIRST));
    }
    let mut text = Text::default();
    text.say(format!("Edited Post {post}."));
    Ok(text.done(json!({ "post": post, "removed": false })))
}

fn remove_post(
    through: &Through<'_>,
    arguments: &Map<String, Value>,
) -> Result<ToolResult, Failed> {
    let post = required(arguments, "post", references::REFERENCE_LIMIT)?;
    let path = path_of(through.address, post, Kind::Post, "post")?;
    let page = through.page(&format!("{path}/edit"))?;
    if !page["remove"].is_object() {
        return Err(no_form(&page, "removeGate"));
    }
    let form = through.form(&page["remove"], READ_FIRST)?;
    let answer = through.wrote(&form, &[], READ_FIRST)?;
    if string(&answer, "address") != Some(post) || answer["removed"] != true {
        return Err(untrusted(READ_FIRST));
    }
    let mut text = Text::default();
    text.say(format!("Removed Post {post}."));
    Ok(text.done(json!({ "post": post, "removed": true })))
}

fn mark_read(through: &Through<'_>, arguments: &Map<String, Value>) -> Result<ToolResult, Failed> {
    let topic = required(arguments, "topic", references::REFERENCE_LIMIT)?;
    let path = path_of(through.address, topic, Kind::Topic, "topic")?;
    let seen = arguments
        .get("through")
        .and_then(Value::as_u64)
        .ok_or(Explained::new(
            INVALID,
            "through is required, a whole number: pass the one tangent_read_topic gave.",
        ))?;
    let page = through.page(path)?;
    let mut text = Text::default();
    // A page that offers no such form holds nothing the reader has not read.
    if !page["seen"].is_object() {
        text.say(format!("Topic {topic} was already marked read."));
        return Ok(text.done(json!({ "topic": topic, "through": seen })));
    }
    let form = through.form(&page["seen"], SAFE)?;
    let answer = through.wrote(&form, &[("through", &seen.to_string())], SAFE)?;
    let moved = answer["seq"].as_u64();
    text.say(format!(
        "Topic {topic} is marked read through {}.",
        moved.unwrap_or(seen)
    ));
    Ok(text.done(json!({ "topic": topic, "through": moved.unwrap_or(seen) })))
}

fn watch(through: &Through<'_>, arguments: &Map<String, Value>) -> Result<ToolResult, Failed> {
    let scope = required(arguments, "scope", references::REFERENCE_LIMIT)?;
    let path = if through.is(Some(scope), Kind::Tangent) || through.is(Some(scope), Kind::Topic) {
        &scope[through.address.as_str().len()..]
    } else if scope.starts_with(&format!("{}/", through.address.as_str())) {
        return Err(Explained::new(
            INVALID,
            "scope is a Tangent's or a Topic's address, as tangent_arrive gives them.",
        )
        .into());
    } else {
        return Err(Explained::new(INVALID, foreign("scope")).into());
    };
    let wanted = arguments
        .get("watch")
        .and_then(Value::as_bool)
        .ok_or(Explained::new(INVALID, "watch is true or false."))?;
    let page = through.page(path)?;
    if !page["watching"]["form"].is_object() {
        return Err(no_form(&page, "gate"));
    }
    let form = through.form(&page["watching"]["form"], SAFE)?;
    let answer = through.wrote(
        &form,
        &[("watch", if wanted { "true" } else { "false" })],
        SAFE,
    )?;
    if answer["watch"] != wanted {
        return Err(untrusted(SAFE));
    }
    let mut text = Text::default();
    text.say(if wanted {
        format!("You watch {scope}.")
    } else {
        format!("You do not watch {scope}.")
    });
    Ok(text.done(json!({ "scope": scope, "watch": wanted })))
}

fn participant(
    through: &Through<'_>,
    arguments: &Map<String, Value>,
) -> Result<ToolResult, Failed> {
    let identifier = required(arguments, "participant", 512)?;
    if !participant_shaped(identifier) {
        return Err(Explained::new(
            INVALID,
            "participant is a participant's id or handle, as the Space gave it.",
        )
        .into());
    }
    let card = format!("/p/{}", encode(identifier));
    let page = match profile_cursor(through, arguments)? {
        Some(cursor) => through.page(cursor)?,
        None => through.page(&card)?,
    };
    let posts = page["posts"]
        .as_array()
        .ok_or_else(|| untrusted(Effect::Read))?;
    let id = string(&page, "id").ok_or_else(|| untrusted(Effect::Read))?;
    let name = string(&page, "name");
    let handle = string(&page, "handle");
    let description = string(&page, "description");
    let kind = string(&page, "kind");
    let mut text = Text::default();
    text.say("Participant ")
        .quote_cut(id, NAME_SHOWN)
        .say(": ")
        .quote_cut(name.unwrap_or_default(), NAME_SHOWN);
    if let Some(handle) = handle {
        text.say(", handle ").quote_cut(handle, NAME_SHOWN);
    }
    if let Some(kind) = kind {
        text.say(", a ")
            .quote_cut(kind, NAME_SHOWN)
            .say(" participant");
    }
    if let Some(description) = description {
        text.say(", who says ")
            .quote_cut(description, PROFILE_SHOWN);
    }
    text.say(format!(
        ". {}:",
        plural(posts.len(), "recent Post", "recent Posts")
    ));
    let mut kept = Vec::new();
    for post in posts {
        let at = string(post, "address");
        if !through.is(at, Kind::Post) && !through.is(at, Kind::Topic) {
            return Err(untrusted(Effect::Read));
        }
        let at = at.unwrap_or_default();
        let (topic, line) = (string(post, "topic"), string(post, "line"));
        text.say(format!("\n- {at}, in Topic "))
            .quote_cut(topic.unwrap_or_default(), NAME_SHOWN)
            .say(": ")
            .quote_cut(line.unwrap_or_default(), ABOUT_SHOWN);
        kept.push(with_quoted(
            json!({ "at": at, "written": string(post, "at") }),
            &[("topic", topic, NAME_SHOWN), ("excerpt", line, ABOUT_SHOWN)],
        ));
    }
    let older = say_cursor(
        through,
        &mut text,
        &page,
        "older",
        string(&page, "address").unwrap_or_default(),
        "read their older Posts",
    )?;
    Ok(text.done(with_quoted(
        json!({ "posts": kept, "cursor": older }),
        &[
            ("participant", Some(id), NAME_SHOWN),
            ("displayName", name, NAME_SHOWN),
            ("handle", handle, NAME_SHOWN),
            ("kind", kind, NAME_SHOWN),
            ("description", description, PROFILE_SHOWN),
        ],
    )))
}

/// The path of the page of a profile the `cursor` argument names. A profile's
/// own address is spelled from their handle whichever way it was asked for, so
/// the cursor is held to being a query on some one profile.
fn profile_cursor<'a>(
    through: &Through<'_>,
    arguments: &'a Map<String, Value>,
) -> Result<Option<&'a str>, Explained> {
    let Some(cursor) = text_argument(arguments, "cursor", references::CURSOR_LIMIT)? else {
        return Ok(None);
    };
    let page = cursor.split('?').next().unwrap_or_default();
    let profile = page
        .strip_prefix(through.address.as_str())
        .and_then(|path| path.strip_prefix("/p/"));
    profile
        .filter(|segment| !segment.is_empty() && !segment.contains('/'))
        .and_then(|_| cursor_path(through.address, cursor, page))
        .map(Some)
        .ok_or(Explained::new(
            INVALID,
            "cursor is not a page of this; pass one the last read gave, or leave it out to start again.",
        ))
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
    let path = path_of(through.address, post, Kind::Post, "post")?;
    let reason = required(arguments, "reason", 1000)?;
    let request_id = request_id_argument(arguments, &[post, reason])?;
    let page = through.page(&format!("{path}/report"))?;
    if !page["report"].is_object() {
        return Err(no_form(&page, "gate"));
    }
    let form = through.form(&page["report"], SAFE)?;
    let id = tid_of(through.reader(&page, SAFE)?, path, &request_id);
    let answer = through.wrote(&form, &[("id", &id), ("body", reason)], SAFE)?;
    if answer["received"] != true {
        return Err(untrusted(SAFE));
    }
    let mut text = Text::default();
    text.say(format!(
        "Reported Post {post} to the Space's moderators (request_id {request_id})."
    ));
    Ok(text.done(json!({ "post": post, "requestId": request_id })))
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
        let watch = tool("watch").expect("watch");
        let lost = in_its_words(watch, wire::lost(watch.effect).into());
        let Failed::Ended(explained) = lost else {
            panic!("ended");
        };
        assert_eq!(explained.outcome, Outcome::Unknown);
        assert!(explained.sentence.contains("what you watch"));
        assert!(!explained.sentence.contains("post"));
        let refused = Explained::new(Outcome::Refused, wire::PERMISSION_DENIED);
        assert_eq!(in_its_words(watch, refused.into()), Failed::Ended(refused));
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
        let locked = Explained::new(Outcome::Refused, wire::LOCKED);
        assert_eq!(
            in_its_words(tool("post").expect("post"), locked.into()),
            Failed::Ended(locked),
            "a refusal with its own reason keeps it"
        );
    }

    #[test]
    fn a_refused_argument_is_named_in_its_refusal() {
        for tool in TOOLS {
            for name in tool.arguments {
                // These have sentences of their own that name them.
                if matches!(*name, "limit" | "request_id" | "through" | "watch") {
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

    #[test]
    fn a_request_becomes_a_tid_of_its_own_for_each_participant() {
        let id = tid_of("prt_1", "topic", "reply-1");
        assert_eq!(
            id,
            tid_of("prt_1", "topic", "reply-1"),
            "the same when sent again"
        );
        assert_ne!(
            id,
            tid_of("prt_2", "topic", "reply-1"),
            "each participant's own"
        );
        assert_ne!(id, tid_of("prt_1", "other", "reply-1"), "each target's own");
        assert_ne!(id, tid_of("prt_1", "topic", "reply-2"));
        for id in [id, tid_of("x", "y", "z"), tid_of("", "", "")] {
            assert_eq!(id.len(), 13, "{id}");
            assert!(id.bytes().all(|byte| TID_ALPHABET.contains(&byte)), "{id}");
            let first = TID_ALPHABET
                .iter()
                .position(|byte| *byte == id.as_bytes()[0])
                .expect("in the alphabet");
            assert!(first <= 15, "the high bit stays zero: {id}");
        }
    }
}
