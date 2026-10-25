//! Tangent's references, checked before any of them reaches a URL.
//!
//! A Space names everything by reference: `<origin>::<tangent>` for a
//! Tangent, `<origin>::<tangent>::<topic>` for a Topic, and
//! `<origin>::<tangent>::<topic>::<post>` for a Post, where the origin is
//! the Space's own. A mind copies them as given. Before a reference becomes
//! part of a request it must name this connection's Space exactly, have
//! exactly its number of segments, each 1 to 64 bytes of `[a-z0-9_-]`, and
//! be at most [`REFERENCE_LIMIT`] bytes; only its last segment ever reaches
//! a path. So a reference from another Space, or one that would climb out
//! of its route, never becomes a request. The same holds for what the
//! Space answers: a reference it sends back that is not of this Space is
//! an answer the adapter does not trust.
//!
//! Cursors are the Space's own opaque text, passed back as given: non-empty,
//! at most [`CURSOR_LIMIT`] bytes, with no control characters, and always
//! percent-encoded into a query.

use lobby_contracts::{Address, Explained, Outcome};

/// The longest reference, in bytes.
pub const REFERENCE_LIMIT: usize = 512;
/// The longest cursor, in bytes.
pub const CURSOR_LIMIT: usize = 4096;

/// What a reference names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A Tangent.
    Tangent,
    /// A Topic.
    Topic,
    /// A Post.
    Post,
}

impl Kind {
    const fn segments(self) -> usize {
        match self {
            Self::Tangent => 1,
            Self::Topic => 2,
            Self::Post => 3,
        }
    }

    const fn noun(self) -> &'static str {
        match self {
            Self::Tangent => "a Tangent",
            Self::Topic => "a Topic",
            Self::Post => "a Post",
        }
    }
}

/// The sentence for a reference of another Space.
pub const FOREIGN: &str = "That reference belongs to another Space. Use a reference this Space gave, or pass at to act at the Space it came from.";

/// The keys of `reference`, after the Space's origin, when it names a
/// thing of `kind` in the Space at `address`.
pub fn keys<'a>(address: &Address, reference: &'a str, kind: Kind) -> Option<Vec<&'a str>> {
    let rest = reference
        .strip_prefix(address.as_str())?
        .strip_prefix("::")?;
    if reference.len() > REFERENCE_LIMIT {
        return None;
    }
    let keys: Vec<&str> = rest.split("::").collect();
    let valid = keys.len() == kind.segments()
        && keys.iter().all(|key| {
            !key.is_empty()
                && key.len() <= 64
                && key.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || byte == b'-'
                        || byte == b'_'
                })
        });
    valid.then_some(keys)
}

/// The last key of the reference a mind passed as `argument`, when it names
/// a thing of `kind` in the Space at `address`; otherwise a refusal that
/// says what was wrong, before anything is sent.
pub fn key_of<'a>(
    address: &Address,
    reference: &'a str,
    kind: Kind,
    argument: &'static str,
) -> Result<&'a str, Explained> {
    if let Some(keys) = keys(address, reference, kind) {
        return Ok(keys[keys.len() - 1]);
    }
    if !reference.starts_with(&format!("{}::", address.as_str())) {
        return Err(Explained::new(
            Outcome::InvalidInput,
            crate::tools::foreign(argument),
        ));
    }
    Err(Explained::new(
        Outcome::InvalidInput,
        wrong_kind(kind, argument),
    ))
}

/// Whether `reference` names a thing of `kind` in the Space at `address`
/// under `parent`, a reference of the thing that holds it.
pub fn under(address: &Address, reference: &str, kind: Kind, parent: &str) -> bool {
    keys(address, reference, kind).is_some()
        && reference
            .strip_prefix(parent)
            .is_some_and(|rest| rest.starts_with("::") && !rest[2..].contains("::"))
}

fn wrong_kind(kind: Kind, argument: &'static str) -> &'static str {
    match (kind, argument) {
        (Kind::Tangent, "tangent") => {
            "tangent is not a Tangent's reference. A Tangent's reference is <space>::<tangent>, as tangent_arrive and tangent_tangents give it."
        }
        (Kind::Tangent, _) => {
            "That is not a Tangent's reference. A Tangent's reference is <space>::<tangent>, as tangent_arrive and tangent_tangents give it."
        }
        (Kind::Topic, "topic") => {
            "topic is not a Topic's reference. A Topic's reference is <space>::<tangent>::<topic>, as tangent_topics and tangent_arrive give it."
        }
        (Kind::Topic, _) => {
            "That is not a Topic's reference. A Topic's reference is <space>::<tangent>::<topic>, as tangent_topics and tangent_arrive give it."
        }
        (Kind::Post, "reply_to") => {
            "reply_to is not a Post's reference. A Post's reference is <space>::<tangent>::<topic>::<post>, as tangent_read_topic gives it."
        }
        (Kind::Post, "around") => {
            "around is not a Post's reference. A Post's reference is <space>::<tangent>::<topic>::<post>, as tangent_read_topic gives it."
        }
        (Kind::Post, "post") => {
            "post is not a Post's reference. A Post's reference is <space>::<tangent>::<topic>::<post>, as tangent_read_topic gives it."
        }
        (Kind::Post, _) => {
            "That is not a Post's reference. A Post's reference is <space>::<tangent>::<topic>::<post>, as tangent_read_topic gives it."
        }
    }
}

/// What a reference of `kind` is called, for descriptions.
pub fn noun(kind: Kind) -> &'static str {
    kind.noun()
}

/// Whether `cursor` is a cursor the adapter passes back.
pub fn cursor_shaped(cursor: &str) -> bool {
    !cursor.is_empty() && cursor.len() <= CURSOR_LIMIT && !cursor.chars().any(char::is_control)
}

/// Percent-encodes everything but the RFC 3986 unreserved characters, as
/// one query component or path segment.
pub fn encode(text: &str) -> String {
    text.bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
                char::from(byte).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lobby_contracts::AddressPolicy;

    fn space() -> Address {
        Address::parse("https://space.example.test", AddressPolicy::PublicHttps).expect("address")
    }

    #[test]
    fn foreign_references_and_cursors_never_become_resource_paths() {
        let space = space();
        assert_eq!(
            keys(
                &space,
                "https://space.example.test::home::welcome",
                Kind::Topic
            ),
            Some(vec!["home", "welcome"])
        );
        for bad in [
            "https://other.example.test::home::welcome",
            "https://space.example.test::home::../welcome",
            "https://space.example.test::home::wel/come",
            "https://space.example.test::home",
            "https://space.example.test::home::welcome::p1",
            "https://space.example.test.evil::home::welcome",
            "https://space.example.test::Home::welcome",
            "https://space.example.test::home::",
        ] {
            assert_eq!(keys(&space, bad, Kind::Topic), None, "{bad}");
        }
        let long = format!("https://space.example.test::home::{}", "a".repeat(600));
        assert_eq!(keys(&space, &long, Kind::Topic), None);
        assert_eq!(
            key_of(
                &space,
                "https://other.example.test::home::welcome",
                Kind::Topic,
                "topic"
            )
            .map_err(|explained| explained.sentence),
            Err(crate::tools::foreign("topic"))
        );
        assert!(
            crate::tools::foreign("topic").starts_with("topic holds a reference of another Space.")
        );
        assert!(!cursor_shaped(&"x".repeat(CURSOR_LIMIT + 1)));
        assert!(!cursor_shaped("a\nb") && !cursor_shaped(""));
        assert!(cursor_shaped("older_4"));
    }

    #[test]
    fn a_thing_is_under_its_parent_only() {
        let space = space();
        let topic = "https://space.example.test::home::welcome";
        assert!(under(&space, &format!("{topic}::p1"), Kind::Post, topic));
        assert!(!under(
            &space,
            "https://space.example.test::home::other::p1",
            Kind::Post,
            topic
        ));
        assert!(!under(
            &space,
            "https://space.example.test::home::welcomex::p1",
            Kind::Post,
            topic
        ));
    }

    #[test]
    fn query_values_are_percent_encoded() {
        assert_eq!(encode("a b&c=d/e"), "a%20b%26c%3Dd%2Fe");
    }
}
