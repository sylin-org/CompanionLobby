//! The Companion noun.
//!
//! A companion is a named identity with a moniker (an explicit charset and a
//! byte bound), a display name and its sponsor. A mind attaches as a companion
//! and sees exactly what that companion may reach.

use std::fmt;

use crate::Sponsor;

/// Identifier of a companion: a UUIDv7 rendered as 32 lowercase hex digits.
///
/// The daemon mints identifiers; this type only carries and checks them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CompanionId(u128);

impl CompanionId {
    /// Wraps the 128 bits of a UUID the daemon minted.
    pub const fn from_u128(bits: u128) -> Self {
        Self(bits)
    }

    /// Parses exactly 32 lowercase hex digits.
    pub fn parse(text: &str) -> Option<Self> {
        parse_hex_id(text).map(Self)
    }

    /// The 128 bits of the identifier.
    pub const fn as_u128(self) -> u128 {
        self.0
    }
}

impl fmt::Display for CompanionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:032x}", self.0)
    }
}

/// Parses the 32-lowercase-hex rendering shared by every identifier.
pub(crate) fn parse_hex_id(text: &str) -> Option<u128> {
    if text.len() != 32
        || !text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    u128::from_str_radix(text, 16).ok()
}

/// Why a moniker or display name was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameProblem {
    /// The text is empty.
    Empty,
    /// The text is longer than the bound.
    TooLong,
    /// The text holds a character outside the allowed set.
    Character,
}

impl fmt::Display for NameProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Empty => "it is empty",
            Self::TooLong => "it is too long",
            Self::Character => "it holds a character that is not allowed",
        })
    }
}

/// A companion's moniker: the name a host configuration and `attach` use.
///
/// One to 24 bytes: a lowercase ASCII letter, then lowercase ASCII letters,
/// digits or single hyphens, never ending in a hyphen. The bound keeps a
/// moniker shorter than a 32-digit identifier, so the two never collide.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Moniker(String);

impl Moniker {
    /// The longest moniker, in bytes.
    pub const MAX_BYTES: usize = 24;

    /// Checks `text` against the moniker rule.
    pub fn new(text: &str) -> Result<Self, NameProblem> {
        let bytes = text.as_bytes();
        let Some(first) = bytes.first() else {
            return Err(NameProblem::Empty);
        };
        if bytes.len() > Self::MAX_BYTES {
            return Err(NameProblem::TooLong);
        }
        let allowed =
            |byte: &u8| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-';
        if !first.is_ascii_lowercase()
            || !bytes.iter().all(allowed)
            || text.ends_with('-')
            || text.contains("--")
        {
            return Err(NameProblem::Character);
        }
        Ok(Self(text.to_owned()))
    }

    /// The moniker as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Moniker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A companion's display name: what people read.
///
/// One to 64 characters and at most 256 bytes, with no control or format
/// characters, no leading or trailing whitespace.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DisplayName(String);

impl DisplayName {
    /// The longest display name, in characters.
    pub const MAX_CHARS: usize = 64;
    /// The longest display name, in bytes.
    pub const MAX_BYTES: usize = 256;

    /// Checks `text` against the display name rule.
    pub fn new(text: &str) -> Result<Self, NameProblem> {
        if text.is_empty() {
            return Err(NameProblem::Empty);
        }
        if text.len() > Self::MAX_BYTES || text.chars().count() > Self::MAX_CHARS {
            return Err(NameProblem::TooLong);
        }
        if text.trim() != text || text.chars().any(is_control_or_format) {
            return Err(NameProblem::Character);
        }
        Ok(Self(text.to_owned()))
    }

    /// The display name as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DisplayName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Control characters plus the Unicode format characters that can disguise text
/// (bidirectional overrides, zero-width joiners, the byte order mark).
fn is_control_or_format(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{00AD}'
                | '\u{061C}'
                | '\u{180E}'
                | '\u{200B}'..='\u{200F}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{206F}'
                | '\u{FEFF}'
                | '\u{FFF9}'..='\u{FFFB}'
        )
}

/// A companion record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Companion {
    /// The immutable identifier.
    pub id: CompanionId,
    /// The moniker hosts name it by.
    pub moniker: Moniker,
    /// The name people read.
    pub display_name: DisplayName,
    /// The sponsor who owns it.
    pub sponsor: Sponsor,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_are_exactly_32_lowercase_hex_digits() {
        let id = CompanionId::from_u128(0x0190_0000_0000_7000_8000_0000_0000_00ab);
        let text = id.to_string();
        assert_eq!(text, "019000000000700080000000000000ab");
        assert_eq!(CompanionId::parse(&text), Some(id));
        assert_eq!(CompanionId::parse(&text.to_uppercase()), None);
        assert_eq!(CompanionId::parse(&text[1..]), None);
        assert_eq!(CompanionId::parse(&format!("{text}0")), None);
        assert_eq!(CompanionId::parse("g".repeat(32).as_str()), None);
    }

    #[test]
    fn monikers_follow_an_explicit_charset_and_bound() {
        for good in ["ada", "a", "bea-2", "x1-y2-z3", "abcdefghijklmnopqrstuvwx"] {
            assert!(Moniker::new(good).is_ok(), "{good} is a moniker");
        }
        assert_eq!(Moniker::new(""), Err(NameProblem::Empty));
        assert_eq!(
            Moniker::new("abcdefghijklmnopqrstuvwxy"),
            Err(NameProblem::TooLong)
        );
        for bad in [
            "Ada",
            "1ada",
            "-ada",
            "ada-",
            "a--b",
            "a_b",
            "a b",
            "a:b",
            "ádá",
            "a\u{200B}b",
        ] {
            assert_eq!(Moniker::new(bad), Err(NameProblem::Character), "{bad:?}");
        }
    }

    #[test]
    fn a_moniker_can_never_be_mistaken_for_an_identifier() {
        let id_shaped = "0".repeat(32);
        assert!(CompanionId::parse(&id_shaped).is_some());
        assert!(Moniker::new(&id_shaped).is_err());
    }

    #[test]
    fn display_names_refuse_control_and_format_characters() {
        assert!(DisplayName::new("Ada Lovelace").is_ok());
        assert!(DisplayName::new("Zoë 🌙").is_ok());
        assert_eq!(DisplayName::new(""), Err(NameProblem::Empty));
        assert_eq!(DisplayName::new(&"a".repeat(65)), Err(NameProblem::TooLong));
        for bad in [
            " Ada",
            "Ada ",
            "A\nda",
            "A\u{202E}da",
            "A\u{200D}da",
            "\u{FEFF}Ada",
        ] {
            assert_eq!(
                DisplayName::new(bad),
                Err(NameProblem::Character),
                "{bad:?}"
            );
        }
    }
}
