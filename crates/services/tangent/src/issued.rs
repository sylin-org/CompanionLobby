//! What a Space issued for one connection.
//!
//! The identity exchange answers a participant credential and its token.
//! The connection keeps exactly what the Space issued and nothing the
//! account minted: the participant the Space made of the account, the
//! name its card shows for that participant when the card could be read
//! as the connection was established, the credential's id, its token, when
//! it expires and the grants it holds.
//! The token is presented as `Bearer` on every call and never printed.

use lobby_contracts::{SealedIssued, Secret};
use serde_json::{Value, json};

/// The format tag of what this adapter keeps.
pub const FORMAT: &str = "tangent-1";
/// The longest token kept.
pub const TOKEN_LIMIT: usize = 4096;
/// The longest participant or credential id kept.
pub const ID_LIMIT: usize = 512;
/// The most characters of the participant's name kept.
pub const NAME_LIMIT: usize = 200;

/// What a Space issued for one connection.
pub struct Issued {
    /// The participant the Space made of the account.
    pub participant: String,
    /// The name the Space's card shows for that participant, as it read
    /// when the connection was established, if it could be read.
    pub name: Option<String>,
    /// The participant credential's id.
    pub credential: String,
    /// The token presented on every call.
    pub token: Secret,
    /// When the credential expires, in seconds since the Unix epoch.
    pub expires_at: i64,
    /// The grants it holds, in the Space's words.
    pub grants: Vec<String>,
}

impl std::fmt::Debug for Issued {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Issued")
            .field("participant", &self.participant)
            .field("name", &self.name)
            .field("expires_at", &self.expires_at)
            .field("grants", &self.grants)
            .finish_non_exhaustive()
    }
}

fn id_shaped(text: &str) -> bool {
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
    /// Whether the credential holds `grant`.
    pub fn grants(&self, grant: &str) -> bool {
        self.grants.iter().any(|held| held == grant)
    }

    /// The sealed form the lobby keeps.
    pub fn encode(&self) -> SealedIssued {
        SealedIssued::new(
            json!({
                "format": FORMAT,
                "participant": self.participant,
                "name": self.name,
                "credential": self.credential,
                "token": self.token.expose(),
                "expiresAt": self.expires_at,
                "grants": self.grants,
            })
            .to_string()
            .into_bytes(),
        )
    }

    /// What this adapter kept, with every field in its own shape.
    pub fn decode(sealed: &SealedIssued) -> Option<Self> {
        let body: Value = serde_json::from_slice(sealed.as_bytes()).ok()?;
        if body["format"] != FORMAT || body.as_object()?.len() != 7 {
            return None;
        }
        let name = match &body["name"] {
            Value::Null => None,
            Value::String(name) if name_shaped(name) => Some(name.clone()),
            _ => return None,
        };
        let text = |field: &str| body[field].as_str().filter(|text| id_shaped(text));
        let token = body["token"]
            .as_str()
            .filter(|token| id_shaped(token) && token.len() <= TOKEN_LIMIT)?;
        let grants = body["grants"]
            .as_array()?
            .iter()
            .map(|grant| {
                grant
                    .as_str()
                    .filter(|grant| id_shaped(grant))
                    .map(str::to_owned)
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            participant: text("participant")?.to_owned(),
            name,
            credential: text("credential")?.to_owned(),
            token: Secret::new(token),
            expires_at: body["expiresAt"].as_i64()?,
            grants,
        })
    }

    /// What the exchange answered, when it keeps the profile: a
    /// `tangent_participant` credential naming a participant, an id, an
    /// expiry and grants, with a token. The participant's name is not in
    /// it; the adapter reads it from the participant's card.
    pub fn from_exchange(answer: &Value) -> Option<Self> {
        if answer["profile"] != "tangent_participant" {
            return None;
        }
        let credential = &answer["credential"];
        let text = |value: &Value| {
            value
                .as_str()
                .filter(|text| id_shaped(text))
                .map(str::to_owned)
        };
        let token = answer["token"]
            .as_str()
            .filter(|token| id_shaped(token) && token.len() <= TOKEN_LIMIT)?;
        let grants = credential["grants"]
            .as_array()?
            .iter()
            .map(|grant| {
                grant
                    .as_str()
                    .filter(|grant| id_shaped(grant))
                    .map(str::to_owned)
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            participant: text(&credential["participantId"])?,
            name: None,
            credential: text(&credential["id"])?,
            token: Secret::new(token),
            expires_at: credential["expiresAt"].as_str().and_then(parse_rfc3339)?,
            grants,
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
    fn what_was_issued_round_trips_and_never_prints_its_token() {
        let answer = json!({
            "profile": "tangent_participant",
            "credential": {
                "id": "cred-1",
                "participantId": "prt_1",
                "name": "Companion Lobby",
                "grants": ["welcome", "read", "post"],
                "createdAt": "2026-09-21T14:13:20Z",
                "expiresAt": "2026-09-28T14:13:20Z",
                "revokedAt": null,
            },
            "token": "ts_0123456789",
        });
        let issued = Issued::from_exchange(&answer).expect("kept");
        assert!(issued.grants("post") && !issued.grants("manage"));
        assert!(!format!("{issued:?}").contains("ts_0123456789"));
        let again = Issued::decode(&issued.encode()).expect("decodes");
        assert_eq!(again.participant, "prt_1");
        assert_eq!(again.name, None);
        let mut named = Issued::decode(&issued.encode()).expect("decodes");
        named.name = Some("Ada".to_owned());
        let named = Issued::decode(&named.encode()).expect("decodes");
        assert_eq!(named.name.as_deref(), Some("Ada"));
        assert_eq!(again.token.expose(), "ts_0123456789");
        assert_eq!(again.expires_at, 1_790_000_000 + 7 * 86_400);
        let mut broken = answer.clone();
        broken["profile"] = json!("other");
        assert!(Issued::from_exchange(&broken).is_none());
        let mut tokenless = answer;
        tokenless["token"] = json!("");
        assert!(Issued::from_exchange(&tokenless).is_none());
        assert!(Issued::decode(&SealedIssued::new(b"{\"format\":\"board-1\"}".to_vec())).is_none());
    }
}
