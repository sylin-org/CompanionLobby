//! What a Space issued for one connection.
//!
//! The identity exchange answers `204` and the cookies that sign the account
//! in. The connection keeps exactly what the Space issued and nothing the
//! account minted: its own [`Jar`] of those cookies, and what the Space's
//! door last said of it, when that could be read: the participant the Space
//! made of the account, the name it shows for them, and whether it lets them
//! post. The jar goes with every request and is never printed.

use lobby_contracts::SealedIssued;
use serde_json::{Value, json};

use crate::jar::Jar;

/// The format tag of what this adapter keeps.
pub const FORMAT: &str = "tangent-2";
/// The longest participant id kept.
pub const ID_LIMIT: usize = 512;
/// The most characters of the participant's name kept.
pub const NAME_LIMIT: usize = 200;

/// What a Space issued for one connection.
#[derive(Debug, Clone)]
pub struct Issued {
    /// The participant the Space made of the account, as its door said.
    pub participant: Option<String>,
    /// The name the Space shows for that participant, as its door said.
    pub name: Option<String>,
    /// Whether its door said the participant may post.
    pub posts: bool,
    /// This connection's cookies, which print by name alone.
    pub jar: Jar,
}

/// Whether `text` is a participant's id as kept.
pub fn id_shaped(text: &str) -> bool {
    !text.is_empty() && text.len() <= ID_LIMIT && text.bytes().all(|byte| byte.is_ascii_graphic())
}

/// Whether `text` is a name kept: not blank, at most [`NAME_LIMIT`]
/// characters, with no control characters.
pub fn name_shaped(text: &str) -> bool {
    !text.trim().is_empty()
        && text.chars().count() <= NAME_LIMIT
        && !text.chars().any(char::is_control)
}

impl Issued {
    /// A connection whose exchange set `jar`, before its door was read.
    pub fn signed_in(jar: Jar) -> Self {
        Self {
            participant: None,
            name: None,
            posts: false,
            jar,
        }
    }

    /// The sealed form the lobby keeps.
    pub fn encode(&self) -> SealedIssued {
        SealedIssued::new(
            json!({
                "format": FORMAT,
                "participant": self.participant,
                "name": self.name,
                "posts": self.posts,
                "cookies": self.jar.encode(),
            })
            .to_string()
            .into_bytes(),
        )
    }

    /// What this adapter kept, with every field in its own shape.
    pub fn decode(sealed: &SealedIssued) -> Option<Self> {
        let body: Value = serde_json::from_slice(sealed.as_bytes()).ok()?;
        if body["format"] != FORMAT || body.as_object()?.len() != 5 {
            return None;
        }
        let participant = match &body["participant"] {
            Value::Null => None,
            Value::String(id) if id_shaped(id) => Some(id.clone()),
            _ => return None,
        };
        let name = match &body["name"] {
            Value::Null => None,
            Value::String(name) if name_shaped(name) => Some(name.clone()),
            _ => return None,
        };
        Some(Self {
            participant,
            name,
            posts: body["posts"].as_bool()?,
            jar: Jar::decode(&body["cookies"])?,
        })
    }
}

/// An RFC 3339 time, such as `2026-09-18T00:00:00Z` or one with fractional
/// seconds or an offset, as seconds since the Unix epoch.
pub fn parse_rfc3339(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() < 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !matches!(bytes[10], b'T' | b't')
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }
    let number = |range: std::ops::Range<usize>| -> Option<i64> {
        let digits = text.get(range)?;
        digits
            .bytes()
            .all(|byte| byte.is_ascii_digit())
            .then(|| digits.parse().ok())
            .flatten()
    };
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let mut rest = &text[19..];
    if let Some(fraction) = rest.strip_prefix('.') {
        let digits = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        rest = &fraction[digits..];
    }
    let offset = match rest {
        "Z" | "z" => 0,
        _ => {
            let sign = match rest.as_bytes().first()? {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let zone = rest.get(1..)?;
            if zone.len() != 5 || zone.as_bytes()[2] != b':' {
                return None;
            }
            let hours: i64 = zone.get(0..2)?.parse().ok()?;
            let minutes: i64 = zone.get(3..5)?.parse().ok()?;
            sign * (hours * 3600 + minutes * 60)
        }
    };
    // Howard Hinnant's days-from-civil.
    let (y, m) = if month <= 2 {
        (year - 1, month + 9)
    } else {
        (year, month - 3)
    };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * m + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hour * 3600 + minute * 60 + second - offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc_3339_times_read_as_epoch_seconds() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("2000-02-29T00:00:00Z"), Some(951_782_400));
        assert_eq!(parse_rfc3339("2026-09-21T14:13:20Z"), Some(1_790_000_000));
        assert_eq!(
            parse_rfc3339("2026-09-21T14:13:20.123Z"),
            Some(1_790_000_000)
        );
        assert_eq!(
            parse_rfc3339("2026-09-21T16:13:20+02:00"),
            Some(1_790_000_000)
        );
        for bad in [
            "",
            "2026-09-21",
            "2026-13-01T00:00:00Z",
            "2026-09-21T14:13:20",
            "x026-09-21T14:13:20Z",
        ] {
            assert_eq!(parse_rfc3339(bad), None, "{bad}");
        }
    }

    #[test]
    fn what_was_issued_round_trips_and_never_prints_a_cookie() {
        let mut jar = Jar::default();
        jar.absorb("signed=in0123456789; path=/", "/", false, 0);
        let mut issued = Issued::signed_in(jar);
        issued.participant = Some("prt_1".to_owned());
        issued.name = Some("Ada".to_owned());
        issued.posts = true;
        assert!(!format!("{issued:?}").contains("in0123456789"));
        let again = Issued::decode(&issued.encode()).expect("decodes");
        assert_eq!(again.participant.as_deref(), Some("prt_1"));
        assert_eq!(again.name.as_deref(), Some("Ada"));
        assert!(again.posts);
        assert_eq!(again.jar, issued.jar);
        let unread = Issued::decode(&Issued::signed_in(Jar::default()).encode()).expect("decodes");
        assert_eq!(unread.participant, None);
        assert!(!unread.posts);
        assert!(Issued::decode(&SealedIssued::new(b"{\"format\":\"board-1\"}".to_vec())).is_none());
    }
}
