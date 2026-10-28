//! The tools, in Bluesky's own words.
//!
//! A mind reads its timeline, posts, replies to a post, likes a post and
//! reposts one. A post is named by its `at://` URI exactly as the timeline
//! gave it; the adapter reads the post through the app view for the
//! content identifier a reply, a like or a repost must carry with it, so a
//! mind never has to copy two values that must agree. Everything other
//! people wrote (names, handles, post text) and every reference and cursor
//! Bluesky gave reaches the mind as a quoted passage, cut to a fixed size,
//! and the structured answer keeps what other people wrote only under each
//! post's `quoted` object.
//!
//! Writes are records in the account's own repository: `app.bsky.feed.post`
//! for a post or a reply, `app.bsky.feed.like` and `app.bsky.feed.repost`.
//! A post's links become link facets, so they open where the post is read.
//! Images are uploaded as blobs first and embedded with their alt text.

use lobby_contracts::{
    Address, Authority, Effect, Explained, Outcome, Passage, Retry, ToolArguments, ToolDescriptor,
    ToolName, ToolResult,
};
use lobby_outbound::Outbound;
use serde_json::{Map, Value, json};

use crate::NAME;
use crate::issued::{Issued, valid_did, valid_handle};
use crate::wire::{self, Call, Nonces};

/// The most posts one timeline read gives.
pub const PAGE_LIMIT: u64 = 25;
/// How many posts a timeline read gives unless asked for another number.
pub const PAGE_DEFAULT: u64 = 10;
/// The most characters a post's text holds.
pub const TEXT_LIMIT: usize = 300;
/// The most bytes a post's text holds.
pub const TEXT_BYTES: usize = 3000;
/// The most images one post holds.
pub const IMAGES_LIMIT: usize = 4;
/// The largest image accepted, in bytes.
pub const IMAGE_BYTES: usize = 1_000_000;
/// The longest alt text accepted, in characters.
pub const ALT_LIMIT: usize = 2000;
/// How many characters of a name or handle other people chose a mind reads.
pub const NAME_CUT: usize = 64;
/// How many characters of a post's text a mind reads.
pub const POST_CUT: usize = 1000;
/// The longest post URI accepted, in bytes.
pub const URI_LIMIT: usize = 8192;
/// The longest cursor accepted, in bytes.
pub const CURSOR_LIMIT: usize = 1024;
/// The collection a post is kept in.
pub const POSTS: &str = "app.bsky.feed.post";
/// The collection a like is kept in.
pub const LIKES: &str = "app.bsky.feed.like";
/// The collection a repost is kept in.
pub const REPOSTS: &str = "app.bsky.feed.repost";

/// A post URI that is not one.
pub const NOT_A_POST: &str = "That is not a post's uri. Pass the uri exactly as the timeline gave it, such as at://did:plc:.../app.bsky.feed.post/....";
/// A text that is too long or empty.
pub const TEXT_RULE: &str =
    "A post's text is 1 to 300 characters. Shorten it, or give it words, and post again.";
/// A limit that is not one.
pub const LIMIT_RULE: &str = "limit is a whole number from 1 to 25.";
/// A cursor that is not one.
pub const CURSOR_RULE: &str =
    "cursor is the text a timeline read gave as its cursor, passed exactly as given.";
/// Images that are not.
pub const IMAGES_RULE: &str = "images is a list of 1 to 4 images, each an object with data (the image file in base64: PNG, JPEG, WebP or GIF, at most 1,000,000 bytes) and alt (its description for people who cannot see it, at most 2,000 characters), and nothing else.";
/// An answer Bluesky gave that this adapter does not trust.
pub const BROKE: &str = "Bluesky answered in a shape the lobby does not read. Try again later.";
/// A write whose answer broke its promise.
pub const BROKE_AFTER_WRITE: &str = "Bluesky's answer to this did not keep its shape, so it may or may not have landed. Read the timeline before trying again.";

/// One tool: what it is called, what it does and what it takes.
#[derive(Debug)]
pub struct Tool {
    /// Its name, without the service prefix.
    pub name: &'static str,
    /// Its title.
    pub title: &'static str,
    /// What it does.
    pub description: &'static str,
    /// Its arguments, by name.
    pub arguments: &'static [&'static str],
    /// Whether it reads or writes.
    pub effect: Effect,
}

const WRITE: Effect = Effect::Write {
    retry: Retry::ReadFirst,
};

/// Every tool; the first is where a mind starts.
pub const TOOLS: [Tool; 5] = [
    Tool {
        name: "timeline",
        title: "Read the timeline",
        description: "Reads the companion's Bluesky timeline: the newest posts from the accounts it follows, newest first, each with its author, its text, its uri and how many replies, reposts and likes it has. limit says how many posts to read, 1 to 25 (10 when left out); cursor, from an earlier read, reads the posts after that page. Pass a post's uri as post to bluesky_reply, bluesky_like or bluesky_repost.",
        arguments: &["limit", "cursor"],
        effect: Effect::Read,
    },
    Tool {
        name: "post",
        title: "Post",
        description: "Posts to Bluesky as the companion: text is the post, 1 to 300 characters, and its links open where the post is read. images, when given, is a list of 1 to 4 images, each with data (the image file in base64: PNG, JPEG, WebP or GIF, at most 1,000,000 bytes, and the whole call within about 700 KB) and alt (what the image shows, for people who cannot see it). Answers the new post's uri.",
        arguments: &["text", "images"],
        effect: WRITE,
    },
    Tool {
        name: "reply",
        title: "Reply",
        description: "Replies on Bluesky, as the companion, to the post whose uri is post, exactly as the timeline gave it: text is the reply, 1 to 300 characters. The reply joins the post's thread. Answers the reply's uri.",
        arguments: &["post", "text"],
        effect: WRITE,
    },
    Tool {
        name: "like",
        title: "Like",
        description: "Likes, as the companion, the Bluesky post whose uri is post, exactly as the timeline gave it.",
        arguments: &["post"],
        effect: WRITE,
    },
    Tool {
        name: "repost",
        title: "Repost",
        description: "Reposts, as the companion, the Bluesky post whose uri is post, exactly as the timeline gave it, so the companion's followers see it.",
        arguments: &["post"],
        effect: WRITE,
    },
];

impl Tool {
    /// The tool as the contract declares it.
    pub fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: ToolName::new(self.name).unwrap_or_else(|| unreachable!("a tool name")),
            title: self.title.to_owned(),
            description: self.description.to_owned(),
            input_schema: schema(self.name),
            effect: self.effect,
        }
    }
}

/// The tool named `name`.
pub fn tool(name: &str) -> Option<&'static Tool> {
    TOOLS.iter().find(|tool| tool.name == name)
}

fn schema(name: &str) -> Value {
    let post = json!({ "type": "string", "description": "A post's uri, exactly as the timeline gave it." });
    let text = json!({ "type": "string", "minLength": 1, "maxLength": TEXT_LIMIT });
    match name {
        "timeline" => json!({
            "type": "object",
            "properties": {
                "limit": { "type": "integer", "minimum": 1, "maximum": PAGE_LIMIT },
                "cursor": { "type": "string" },
            },
            "additionalProperties": false,
        }),
        "post" => json!({
            "type": "object",
            "properties": {
                "text": { "type": "string", "maxLength": TEXT_LIMIT },
                "images": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": IMAGES_LIMIT,
                    "items": {
                        "type": "object",
                        "properties": {
                            "data": { "type": "string", "contentEncoding": "base64" },
                            "alt": { "type": "string", "maxLength": ALT_LIMIT },
                        },
                        "required": ["data", "alt"],
                        "additionalProperties": false,
                    },
                },
            },
            "required": ["text"],
            "additionalProperties": false,
        }),
        "reply" => json!({
            "type": "object",
            "properties": { "post": post, "text": text },
            "required": ["post", "text"],
            "additionalProperties": false,
        }),
        _ => json!({
            "type": "object",
            "properties": { "post": post },
            "required": ["post"],
            "additionalProperties": false,
        }),
    }
}

/// What one call goes through.
pub struct Through<'a> {
    /// The outbound agent.
    pub outbound: &'a Outbound,
    /// The PDS nonces learned so far.
    pub nonces: &'a Nonces,
    /// The account's PDS.
    pub address: &'a Address,
    /// Who proves each request.
    pub authority: &'a dyn Authority,
    /// The account the connection reaches Bluesky as.
    pub issued: &'a Issued,
}

impl Through<'_> {
    fn send(&self, call: &Call) -> Result<Value, Explained> {
        wire::send(
            self.outbound,
            self.nonces,
            self.address,
            self.authority,
            call,
        )
    }

    fn read(&self, path: String, proxied: bool) -> Result<Value, Explained> {
        self.send(&Call {
            method: "GET",
            path,
            body: None,
            proxied,
            effect: Effect::Read,
        })
    }

    /// Writes `record` into the account's own `collection`, and answers the
    /// new record's uri and content identifier.
    fn create(&self, collection: &str, record: Value) -> Result<(String, String), Explained> {
        let body = json!({ "repo": self.issued.did, "collection": collection, "record": record });
        let answer = self.send(&Call {
            method: "POST",
            path: "/xrpc/com.atproto.repo.createRecord".to_owned(),
            body: Some(("application/json", body.to_string().into_bytes())),
            proxied: false,
            effect: WRITE,
        })?;
        let uri = answer["uri"]
            .as_str()
            .filter(|uri| record_uri(uri, collection))
            .ok_or(Explained::new(Outcome::Unknown, BROKE_AFTER_WRITE))?;
        let cid = answer["cid"]
            .as_str()
            .filter(|cid| is_cid(cid))
            .ok_or(Explained::new(Outcome::Unknown, BROKE_AFTER_WRITE))?;
        Ok((uri.to_owned(), cid.to_owned()))
    }

    /// The post `uri`, as the app view shows it.
    fn post(&self, uri: &str) -> Result<Value, Explained> {
        let answer = self.read(
            format!("/xrpc/app.bsky.feed.getPosts?uris={}", percent_encode(uri)),
            true,
        )?;
        let posts = answer["posts"]
            .as_array()
            .ok_or(Explained::new(Outcome::TemporarilyUnavailable, BROKE))?;
        posts
            .iter()
            .find(|post| post["uri"] == uri && post["cid"].as_str().is_some_and(is_cid))
            .cloned()
            .ok_or(Explained::new(Outcome::NotFound, wire::NOT_FOUND))
    }
}

/// Performs `tool` with `arguments` through `through`.
pub fn perform(
    tool: &Tool,
    arguments: &ToolArguments,
    through: &Through<'_>,
) -> Result<ToolResult, Explained> {
    match tool.name {
        "timeline" => timeline(arguments, through),
        "post" => post(arguments, through),
        "reply" => reply(arguments, through),
        _ => mark(tool.name, arguments, through),
    }
}

/// A result's text, built from the adapter's words and quoted passages.
#[derive(Default)]
struct Text(Vec<Passage>);

impl Text {
    fn say(&mut self, words: &str) -> &mut Self {
        match self.0.last_mut() {
            Some(Passage::Authored(last)) => last.push_str(words),
            _ => self.0.push(Passage::Authored(words.to_owned())),
        }
        self
    }

    fn quote(&mut self, text: &str) -> &mut Self {
        self.0.push(Passage::Quoted(text.to_owned()));
        self
    }
}

/// `text` within `limit` characters, and whether it was cut.
fn cut(text: &str, limit: usize) -> (String, bool) {
    match text.char_indices().nth(limit) {
        Some((end, _)) => (text[..end].to_owned(), true),
        None => (text.to_owned(), false),
    }
}

/// Percent-encodes everything but the RFC 3986 unreserved characters.
fn percent_encode(text: &str) -> String {
    text.bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

/// Whether `cid` looks like a content identifier.
fn is_cid(cid: &str) -> bool {
    (8..=128).contains(&cid.len()) && cid.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

/// Whether `uri` names a record of `collection`: `at://`, the account's DID
/// or handle, the collection and a record key.
pub fn record_uri(uri: &str, collection: &str) -> bool {
    let Some(rest) = uri.strip_prefix("at://") else {
        return false;
    };
    let parts: Vec<&str> = rest.split('/').collect();
    let [authority, named, key] = parts[..] else {
        return false;
    };
    uri.len() <= URI_LIMIT
        && (valid_did(authority) || valid_handle(authority))
        && named == collection
        && !key.is_empty()
        && key.len() <= 512
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:~-".contains(&byte))
}

/// The post a call names in `post`.
fn named_post(arguments: &ToolArguments) -> Result<String, Explained> {
    arguments
        .0
        .get("post")
        .and_then(Value::as_str)
        .filter(|uri| record_uri(uri, POSTS))
        .map(str::to_owned)
        .ok_or(Explained::new(Outcome::InvalidInput, NOT_A_POST))
}

/// The text a call names in `text`, checked against a post's rule;
/// `optional` lets it be empty, for a post that holds images.
fn post_text(arguments: &ToolArguments, optional: bool) -> Result<String, Explained> {
    let text = arguments
        .0
        .get("text")
        .and_then(Value::as_str)
        .ok_or(Explained::new(Outcome::InvalidInput, TEXT_RULE))?;
    let empty = text.trim().is_empty();
    if (empty && !optional) || text.chars().count() > TEXT_LIMIT || text.len() > TEXT_BYTES {
        return Err(Explained::new(Outcome::InvalidInput, TEXT_RULE));
    }
    Ok(if empty {
        String::new()
    } else {
        text.to_owned()
    })
}

/// The time now, as Bluesky writes it: `YYYY-MM-DDTHH:MM:SS.000Z`.
pub fn now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    timestamp(secs)
}

/// `secs` since the Unix epoch, as Bluesky writes a time.
pub fn timestamp(secs: u64) -> String {
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rest = secs % 86_400;
    // Howard Hinnant's days-to-civil.
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.000Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// The link facets of `text`: every `http://` or `https://` run up to
/// whitespace, without the punctuation that ends a sentence, by its UTF-8
/// byte range.
pub fn link_facets(text: &str) -> Vec<Value> {
    let mut facets = Vec::new();
    let mut from = 0;
    while let Some(found) = ["https://", "http://"]
        .iter()
        .filter_map(|scheme| text[from..].find(scheme).map(|at| from + at))
        .min()
    {
        let starts_word = found == 0
            || text[..found]
                .chars()
                .next_back()
                .is_some_and(|before| before.is_whitespace() || "([\"'".contains(before));
        let end = text[found..]
            .find(char::is_whitespace)
            .map_or(text.len(), |at| found + at);
        let link =
            text[found..end].trim_end_matches(['.', ',', ';', ':', '!', '?', ')', '"', '\'']);
        let host_follows = link
            .split_once("://")
            .is_some_and(|(_, rest)| !rest.is_empty());
        if starts_word && host_follows {
            facets.push(json!({
                "index": { "byteStart": found, "byteEnd": found + link.len() },
                "features": [{ "$type": "app.bsky.richtext.facet#link", "uri": link }],
            }));
        }
        from = end.max(found + 1);
        if from >= text.len() {
            break;
        }
    }
    facets
}

/// A post record of `text`.
fn post_record(text: &str) -> Value {
    let mut record = json!({ "$type": POSTS, "text": text, "createdAt": now() });
    let facets = link_facets(text);
    if !facets.is_empty() {
        record["facets"] = Value::Array(facets);
    }
    record
}

fn timeline(arguments: &ToolArguments, through: &Through<'_>) -> Result<ToolResult, Explained> {
    let limit = match arguments.0.get("limit") {
        None => PAGE_DEFAULT,
        Some(value) => value
            .as_u64()
            .filter(|limit| (1..=PAGE_LIMIT).contains(limit))
            .ok_or(Explained::new(Outcome::InvalidInput, LIMIT_RULE))?,
    };
    let mut path = format!("/xrpc/app.bsky.feed.getTimeline?limit={limit}");
    if let Some(cursor) = arguments.0.get("cursor") {
        let cursor = cursor
            .as_str()
            .filter(|cursor| !cursor.is_empty() && cursor.len() <= CURSOR_LIMIT)
            .ok_or(Explained::new(Outcome::InvalidInput, CURSOR_RULE))?;
        path.push_str("&cursor=");
        path.push_str(&percent_encode(cursor));
    }
    let answer = through.read(path, true)?;
    let feed = answer["feed"]
        .as_array()
        .ok_or(Explained::new(Outcome::TemporarilyUnavailable, BROKE))?;
    let cursor = answer["cursor"]
        .as_str()
        .filter(|cursor| !cursor.is_empty() && cursor.len() <= CURSOR_LIMIT);
    let mut text = Text::default();
    let mut posts = Vec::new();
    if feed.is_empty() {
        text.say("The timeline has no posts here: nobody the account follows has posted, or this page is past the last.");
    } else {
        text.say(&format!(
            "The timeline, newest first: {} post{}. Pass a post's uri as post to {NAME}_reply, {NAME}_like or {NAME}_repost.",
            feed.len(),
            if feed.len() == 1 { "" } else { "s" }
        ));
    }
    for item in feed {
        let Some(shown) = shown_post(&item["post"]) else {
            continue;
        };
        text.say("\n- ");
        match &shown.name {
            Some(name) => {
                text.quote(name).say(" (");
                text.quote(&shown.handle).say(")");
            }
            None => {
                text.quote(&shown.handle);
            }
        }
        if let Some(by) = item["reason"]["by"]["handle"]
            .as_str()
            .filter(|_| item["reason"]["$type"] == "app.bsky.feed.defs#reasonRepost")
        {
            text.say(", reposted by ").quote(&cut(by, NAME_CUT).0);
        }
        text.say(" wrote ").quote(&shown.text);
        if shown.cut {
            text.say(" (cut short)");
        }
        text.say(", post ").quote(&shown.uri);
        if let Some(parent) = &shown.reply_to {
            text.say(", replying to post ").quote(parent);
        }
        text.say(&format!(
            "; {} repl{}, {} repost{}, {} like{}.",
            shown.replies,
            if shown.replies == 1 { "y" } else { "ies" },
            shown.reposts,
            if shown.reposts == 1 { "" } else { "s" },
            shown.likes,
            if shown.likes == 1 { "" } else { "s" },
        ));
        posts.push(shown.structured());
    }
    let mut structured = json!({ "posts": posts });
    match cursor {
        Some(cursor) => {
            text.say(&format!(
                "\nThere is more: call {NAME}_timeline with cursor "
            ))
            .quote(cursor)
            .say(" to read on.");
            structured["more"] = json!({ "tool": format!("{NAME}_timeline"), "cursor": cursor });
        }
        None if !feed.is_empty() => {
            text.say("\nThat is all the timeline holds.");
        }
        None => {}
    }
    Ok(ToolResult {
        text: text.0,
        structured: Some(structured),
    })
}

/// One post as a mind reads it.
struct Shown {
    uri: String,
    name: Option<String>,
    handle: String,
    text: String,
    cut: bool,
    created_at: Option<String>,
    reply_to: Option<String>,
    replies: u64,
    reposts: u64,
    likes: u64,
}

impl Shown {
    fn structured(&self) -> Value {
        let mut quoted = Map::new();
        if let Some(name) = &self.name {
            quoted.insert("author".to_owned(), json!(name));
        }
        quoted.insert("handle".to_owned(), json!(self.handle));
        quoted.insert("text".to_owned(), json!(self.text));
        let mut post = json!({
            "post": self.uri,
            "quoted": quoted,
            "replies": self.replies,
            "reposts": self.reposts,
            "likes": self.likes,
        });
        if self.cut {
            post["cutShort"] = json!(["text"]);
        }
        if let Some(created_at) = &self.created_at {
            post["createdAt"] = json!(created_at);
        }
        if let Some(parent) = &self.reply_to {
            post["replyTo"] = json!(parent);
        }
        post
    }
}

/// A post view the app view answered, when it keeps its shape.
fn shown_post(view: &Value) -> Option<Shown> {
    let uri = view["uri"].as_str().filter(|uri| record_uri(uri, POSTS))?;
    let handle = view["author"]["handle"].as_str()?;
    let name = view["author"]["displayName"]
        .as_str()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| cut(name, NAME_CUT).0);
    let (text, was_cut) = cut(
        view["record"]["text"].as_str().unwrap_or_default(),
        POST_CUT,
    );
    let count = |field: &str| view[field].as_u64().unwrap_or(0);
    Some(Shown {
        uri: uri.to_owned(),
        name,
        handle: cut(handle, NAME_CUT).0,
        text,
        cut: was_cut,
        created_at: view["record"]["createdAt"]
            .as_str()
            .filter(|at| at.len() <= 64 && at.bytes().all(|byte| byte.is_ascii_graphic()))
            .map(str::to_owned),
        reply_to: view["record"]["reply"]["parent"]["uri"]
            .as_str()
            .filter(|parent| record_uri(parent, POSTS))
            .map(str::to_owned),
        replies: count("replyCount"),
        reposts: count("repostCount"),
        likes: count("likeCount"),
    })
}

/// A strong reference, when `value` is one this adapter trusts.
fn strong_ref(value: &Value) -> Option<Value> {
    let uri = value["uri"].as_str().filter(|uri| record_uri(uri, POSTS))?;
    let cid = value["cid"].as_str().filter(|cid| is_cid(cid))?;
    Some(json!({ "uri": uri, "cid": cid }))
}

/// Decodes standard or URL-safe base64, padded or not, ignoring whitespace.
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut buffer, mut bits) = (0u32, 0u32);
    let trimmed = text
        .trim_end_matches(|character: char| character == '=' || character.is_ascii_whitespace());
    for byte in trimmed.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return None,
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    (bits < 6).then_some(out)
}

/// The image type of `bytes`, read from their first bytes.
pub fn image_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// One image a post holds: its type, its bytes and its alt text.
type Image = (&'static str, Vec<u8>, String);

/// The images a post call names, decoded and checked, with their alt text.
fn images(arguments: &ToolArguments) -> Result<Vec<Image>, Explained> {
    let Some(given) = arguments.0.get("images") else {
        return Ok(Vec::new());
    };
    let refused = || Explained::new(Outcome::InvalidInput, IMAGES_RULE);
    let list = given.as_array().ok_or_else(refused)?;
    if list.is_empty() || list.len() > IMAGES_LIMIT {
        return Err(refused());
    }
    list.iter()
        .map(|image| {
            let fields = image.as_object().ok_or_else(refused)?;
            if fields.keys().any(|key| key != "data" && key != "alt") {
                return Err(refused());
            }
            let alt = fields
                .get("alt")
                .and_then(Value::as_str)
                .filter(|alt| alt.chars().count() <= ALT_LIMIT)
                .ok_or_else(refused)?;
            let bytes = fields
                .get("data")
                .and_then(Value::as_str)
                .and_then(base64_decode)
                .filter(|bytes| !bytes.is_empty() && bytes.len() <= IMAGE_BYTES)
                .ok_or_else(refused)?;
            let kind = image_type(&bytes).ok_or_else(refused)?;
            Ok((kind, bytes, alt.to_owned()))
        })
        .collect()
}

fn post(arguments: &ToolArguments, through: &Through<'_>) -> Result<ToolResult, Explained> {
    let images = images(arguments)?;
    let text = post_text(arguments, !images.is_empty())?;
    let mut record = post_record(&text);
    if !images.is_empty() {
        let mut embedded = Vec::new();
        for (kind, bytes, alt) in images {
            let answer = through.send(&Call {
                method: "POST",
                path: "/xrpc/com.atproto.repo.uploadBlob".to_owned(),
                body: Some((kind, bytes)),
                proxied: false,
                // A blob no record names is let go by the PDS, so an upload
                // whose answer was lost changed nothing anyone sees.
                effect: Effect::Read,
            })?;
            let blob = &answer["blob"];
            if blob["$type"] != "blob" || !blob["ref"]["$link"].as_str().is_some_and(is_cid) {
                return Err(Explained::new(Outcome::TemporarilyUnavailable, BROKE));
            }
            embedded.push(json!({ "alt": alt, "image": blob }));
        }
        record["embed"] = json!({ "$type": "app.bsky.embed.images", "images": embedded });
    }
    let (uri, cid) = through.create(POSTS, record)?;
    let mut said = Text::default();
    said.say("Posted to Bluesky as ")
        .quote(&format!("@{}", through.issued.handle))
        .say(". The new post is ")
        .quote(&uri)
        .say(".");
    Ok(ToolResult {
        text: said.0,
        structured: Some(json!({ "post": uri, "cid": cid })),
    })
}

fn reply(arguments: &ToolArguments, through: &Through<'_>) -> Result<ToolResult, Explained> {
    let parent_uri = named_post(arguments)?;
    let text = post_text(arguments, false)?;
    let parent_view = through.post(&parent_uri)?;
    let parent =
        strong_ref(&parent_view).ok_or(Explained::new(Outcome::NotFound, wire::NOT_FOUND))?;
    let root =
        strong_ref(&parent_view["record"]["reply"]["root"]).unwrap_or_else(|| parent.clone());
    let mut record = post_record(&text);
    record["reply"] = json!({ "root": root, "parent": parent });
    let (uri, cid) = through.create(POSTS, record)?;
    let mut said = Text::default();
    said.say("Replied on Bluesky as ")
        .quote(&format!("@{}", through.issued.handle))
        .say(" to post ")
        .quote(&parent_uri)
        .say(". The reply is post ")
        .quote(&uri)
        .say(".");
    Ok(ToolResult {
        text: said.0,
        structured: Some(json!({ "post": uri, "cid": cid, "replyTo": parent_uri })),
    })
}

/// Likes or reposts the post a call names.
fn mark(
    name: &str,
    arguments: &ToolArguments,
    through: &Through<'_>,
) -> Result<ToolResult, Explained> {
    let (collection, done) = match name {
        "like" => (LIKES, "Liked"),
        "repost" => (REPOSTS, "Reposted"),
        _ => return Err(Outcome::Unsupported.into()),
    };
    let subject_uri = named_post(arguments)?;
    let view = through.post(&subject_uri)?;
    let subject = strong_ref(&view).ok_or(Explained::new(Outcome::NotFound, wire::NOT_FOUND))?;
    let record = json!({ "$type": collection, "subject": subject, "createdAt": now() });
    let (uri, _) = through.create(collection, record)?;
    let mut said = Text::default();
    said.say(&format!("{done} post "))
        .quote(&subject_uri)
        .say(" on Bluesky as ")
        .quote(&format!("@{}", through.issued.handle))
        .say(".");
    Ok(ToolResult {
        text: said.0,
        structured: Some(json!({ name: uri, "post": subject_uri })),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalog_starts_with_the_timeline_and_names_valid_tools() {
        assert_eq!(TOOLS[0].name, "timeline");
        for tool in &TOOLS {
            let descriptor = tool.descriptor();
            assert_eq!(descriptor.input_schema["type"], "object");
            let declared: Vec<&str> = descriptor.input_schema["properties"]
                .as_object()
                .expect("properties")
                .keys()
                .map(String::as_str)
                .collect();
            let mut taken: Vec<&str> = tool.arguments.to_vec();
            taken.sort_unstable();
            let mut declared = declared;
            declared.sort_unstable();
            assert_eq!(declared, taken, "{}", tool.name);
        }
    }

    #[test]
    fn a_post_uri_names_a_post_and_nothing_else() {
        assert!(record_uri(
            "at://did:plc:aaaaaaaaaaaaaaaaaaaaaaaa/app.bsky.feed.post/3l2abc",
            POSTS
        ));
        assert!(record_uri(
            "at://ada.example.test/app.bsky.feed.post/3l2abc",
            POSTS
        ));
        for bad in [
            "https://bsky.app/profile/ada/post/3l2abc",
            "at://did:plc:a/app.bsky.feed.like/3l2abc",
            "at://did:plc:a/app.bsky.feed.post/",
            "at://did:plc:a/app.bsky.feed.post/a/b",
            "at://did:plc:a/app.bsky.feed.post/a b",
            "at://did:key:z/app.bsky.feed.post/3l2abc",
        ] {
            assert!(!record_uri(bad, POSTS), "{bad}");
        }
    }

    #[test]
    fn times_are_written_the_way_bluesky_writes_them() {
        assert_eq!(timestamp(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(timestamp(951_782_400), "2000-02-29T00:00:00.000Z");
        assert_eq!(timestamp(1_793_212_496), "2026-10-28T18:34:56.000Z");
    }

    #[test]
    fn links_become_facets_by_their_byte_range() {
        let text =
            "Café notes: https://example.test/a?b=1. And (http://x.test) too, not xhttps://y.";
        let facets = link_facets(text);
        assert_eq!(facets.len(), 2, "{facets:?}");
        let start = facets[0]["index"]["byteStart"].as_u64().expect("start") as usize;
        let end = facets[0]["index"]["byteEnd"].as_u64().expect("end") as usize;
        assert_eq!(&text[start..end], "https://example.test/a?b=1");
        assert_eq!(facets[1]["features"][0]["uri"], "http://x.test");
        assert!(link_facets("no links here").is_empty());
        assert!(link_facets("https://").is_empty());
    }

    #[test]
    fn images_are_read_from_base64_and_known_by_their_first_bytes() {
        assert_eq!(base64_decode("aGk=").as_deref(), Some(&b"hi"[..]));
        assert_eq!(base64_decode("aGk").as_deref(), Some(&b"hi"[..]));
        assert_eq!(base64_decode("a G\nk=").as_deref(), Some(&b"hi"[..]));
        assert_eq!(base64_decode("a$Gk"), None);
        assert_eq!(image_type(b"\x89PNG\r\n\x1a\nrest"), Some("image/png"));
        assert_eq!(image_type(&[0xff, 0xd8, 0xff, 0xe0]), Some("image/jpeg"));
        assert_eq!(image_type(b"GIF89a.."), Some("image/gif"));
        assert_eq!(image_type(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(image_type(b"%PDF-1.7"), None);
    }

    #[test]
    fn a_post_text_keeps_to_the_networks_length() {
        let args = |text: &str| {
            ToolArguments(
                json!({ "text": text })
                    .as_object()
                    .cloned()
                    .expect("object"),
            )
        };
        assert!(post_text(&args("Hello"), false).is_ok());
        assert!(post_text(&args(&"é".repeat(300)), false).is_ok());
        assert!(post_text(&args(&"é".repeat(301)), false).is_err());
        assert!(post_text(&args("  "), false).is_err());
        assert_eq!(post_text(&args("  "), true).as_deref(), Ok(""));
    }
}
