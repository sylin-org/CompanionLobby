//! Tangent's references, checked before any of them reaches a request.
//!
//! A Space names everything by its address: `<origin>/t/<tangent>` for a
//! Tangent, `<origin>/t/<tangent>/<topic>` for a Topic, and
//! `<origin>/t/<tangent>/<topic>/<post>` for a Post, where the origin is the
//! Space's own. A mind copies them as given. Before a reference becomes a
//! request it must name this connection's Space exactly, have exactly its
//! number of segments, each 1 to 64 bytes of `[a-z0-9_-]`, and be at most
//! [`REFERENCE_LIMIT`] bytes. So a reference from another Space, or one that
//! would climb out of its route, never becomes a request. The same holds for
//! what the Space answers: a reference it sends back that is not of this
//! Space is an answer the adapter does not trust.
//!
//! A cursor is the address of another page, as the page before it named it:
//! at most [`CURSOR_LIMIT`] bytes, with no whitespace or control characters,
//! and it must be a query on the page it continues.

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
}

/// The sentence for a reference of another Space.
pub const FOREIGN: &str = "That reference belongs to another Space. Use a reference this Space gave, or pass at to act at the Space it came from.";

/// The keys of `reference`, after the Space's origin and `/t/`, when it names
/// a thing of `kind` in the Space at `address`.
pub fn keys<'a>(address: &Address, reference: &'a str, kind: Kind) -> Option<Vec<&'a str>> {
    let rest = reference
        .strip_prefix(address.as_str())?
        .strip_prefix("/t/")?;
    if reference.len() > REFERENCE_LIMIT {
        return None;
    }
    let keys: Vec<&str> = rest.split('/').collect();
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

/// The path of the reference a mind passed as `argument`, when it names a
/// thing of `kind` in the Space at `address`; otherwise a refusal that says
/// what was wrong, before anything is sent.
pub fn path_of<'a>(
    address: &Address,
    reference: &'a str,
    kind: Kind,
    argument: &'static str,
) -> Result<&'a str, Explained> {
    if keys(address, reference, kind).is_some() {
        return Ok(&reference[address.as_str().len()..]);
    }
    if !reference.starts_with(&format!("{}/", address.as_str())) {
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
            .and_then(|rest| rest.strip_prefix('/'))
            .is_some_and(|rest| !rest.contains('/'))
}

/// The path of the page `cursor` names, when it is a query on the page at
/// `page`, an address of the Space at `address`.
pub fn cursor_path<'a>(address: &Address, cursor: &'a str, page: &str) -> Option<&'a str> {
    let query = cursor.strip_prefix(page)?.strip_prefix('?')?;
    let plain = !cursor
        .chars()
        .any(|character| character.is_control() || character.is_whitespace() || character == '#');
    if cursor.len() > CURSOR_LIMIT || query.is_empty() || !plain {
        return None;
    }
    cursor.strip_prefix(address.as_str())
}

/// The path of `named`, an address a page gave, when it is under the Space at
/// `address`.
pub fn path_under<'a>(address: &Address, named: &'a str) -> Option<&'a str> {
    let path = named.strip_prefix(address.as_str())?;
    (path.starts_with('/') && path.len() <= CURSOR_LIMIT).then_some(path)
}

fn wrong_kind(kind: Kind, argument: &'static str) -> &'static str {
    match (kind, argument) {
        (Kind::Tangent, "tangent") => {
            "tangent is not a Tangent's reference. A Tangent's reference is its address, <space>/t/<tangent>, as tangent_arrive and tangent_tangents give it."
        }
        (Kind::Tangent, _) => {
            "That is not a Tangent's reference. A Tangent's reference is its address, <space>/t/<tangent>, as tangent_arrive and tangent_tangents give it."
        }
        (Kind::Topic, "topic") => {
            "topic is not a Topic's reference. A Topic's reference is its address, <space>/t/<tangent>/<topic>, as tangent_topics and tangent_arrive give it."
        }
        (Kind::Topic, _) => {
            "That is not a Topic's reference. A Topic's reference is its address, <space>/t/<tangent>/<topic>, as tangent_topics and tangent_arrive give it."
        }
        (Kind::Post, "reply_to") => {
            "reply_to is not a Post's reference. A Post's reference is its address, <space>/t/<tangent>/<topic>/<post>, as tangent_read_topic gives it."
        }
        (Kind::Post, "around") => {
            "around is not a Post's reference. A Post's reference is its address, <space>/t/<tangent>/<topic>/<post>, as tangent_read_topic gives it."
        }
        (Kind::Post, "post") => {
            "post is not a Post's reference. A Post's reference is its address, <space>/t/<tangent>/<topic>/<post>, as tangent_read_topic gives it."
        }
        (Kind::Post, _) => {
            "That is not a Post's reference. A Post's reference is its address, <space>/t/<tangent>/<topic>/<post>, as tangent_read_topic gives it."
        }
    }
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
    fn foreign_references_never_become_resource_paths() {
        let space = space();
        assert_eq!(
            keys(
                &space,
                "https://space.example.test/t/home/welcome",
                Kind::Topic
            ),
            Some(vec!["home", "welcome"])
        );
        for bad in [
            "https://other.example.test/t/home/welcome",
            "https://space.example.test/t/home/../welcome",
            "https://space.example.test/t/home/wel%2Fcome",
            "https://space.example.test/t/home",
            "https://space.example.test/t/home/welcome/p1",
            "https://space.example.test.evil/t/home/welcome",
            "https://space.example.test/t/Home/welcome",
            "https://space.example.test/t/home/",
            "https://space.example.test/t/home/welcome?before=1",
            "https://space.example.test/p/home/welcome",
        ] {
            assert_eq!(keys(&space, bad, Kind::Topic), None, "{bad}");
        }
        let long = format!("https://space.example.test/t/home/{}", "a".repeat(600));
        assert_eq!(keys(&space, &long, Kind::Topic), None);
        assert_eq!(
            path_of(
                &space,
                "https://other.example.test/t/home/welcome",
                Kind::Topic,
                "topic"
            )
            .map_err(|explained| explained.sentence),
            Err(crate::tools::foreign("topic"))
        );
        assert_eq!(
            path_of(
                &space,
                "https://space.example.test/t/home/welcome",
                Kind::Topic,
                "topic"
            ),
            Ok("/t/home/welcome")
        );
        assert!(
            crate::tools::foreign("topic").starts_with("topic holds a reference of another Space.")
        );
    }

    #[test]
    fn a_cursor_continues_the_page_it_was_given_on_and_nothing_else() {
        let space = space();
        let page = "https://space.example.test/t/home/welcome";
        assert_eq!(
            cursor_path(&space, &format!("{page}?before=4"), page),
            Some("/t/home/welcome?before=4")
        );
        for bad in [
            "https://other.example.test/t/home/welcome?before=4".to_owned(),
            "https://space.example.test/t/home/other?before=4".to_owned(),
            page.to_owned(),
            format!("{page}?"),
            format!("{page}?a b"),
            format!("{page}?a\nb"),
            format!("{page}?{}", "x".repeat(CURSOR_LIMIT)),
        ] {
            assert_eq!(cursor_path(&space, &bad, page), None, "{bad}");
        }
    }

    #[test]
    fn a_thing_is_under_its_parent_only() {
        let space = space();
        let topic = "https://space.example.test/t/home/welcome";
        assert!(under(&space, &format!("{topic}/p1"), Kind::Post, topic));
        assert!(!under(
            &space,
            "https://space.example.test/t/home/other/p1",
            Kind::Post,
            topic
        ));
        assert!(!under(
            &space,
            "https://space.example.test/t/home/welcomex/p1",
            Kind::Post,
            topic
        ));
    }

    #[test]
    fn query_values_are_percent_encoded() {
        assert_eq!(encode("a b&c=d/e"), "a%20b%26c%3Dd%2Fe");
    }
}
