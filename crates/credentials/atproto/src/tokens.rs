//! The token endpoint's answer, checked.
//!
//! Whether it answers a code exchange or a refresh, the authorization
//! server's answer is used only when it keeps the profile: the token type is
//! `DPoP`, the access and refresh tokens have the shape of a token, the
//! subject is a DID atproto supports, and the granted scope includes
//! `atproto` and lies inside what was asked. Anything else is refused whole.

use lobby_contracts::{Outcome, Secret};
use serde_json::Value;

use crate::SCOPE;
use crate::codec::is_token;
use crate::identity;

/// The longest token accepted.
pub const TOKEN_LIMIT: usize = 4096;

/// How long an access token is taken to live when the answer does not say.
pub const ACCESS_SECS_UNSAID: u64 = 300;

/// The longest life an access token is taken to have, whatever the answer
/// says.
pub const ACCESS_SECS_MAX: u64 = 30 * 24 * 60 * 60;

/// A token answer that keeps the profile.
pub struct Tokens {
    pub(crate) access: Secret,
    pub(crate) refresh: Secret,
    pub(crate) subject: String,
    pub(crate) granted: Vec<String>,
    /// How many seconds the access token lives, as the answer says, within
    /// [`ACCESS_SECS_MAX`].
    pub(crate) expires_in: u64,
}

/// `answer` as tokens, when it keeps the profile for a request that asked
/// for `asked`, a space-separated scope.
pub fn checked(answer: &Value, asked: &str) -> Result<Tokens, Outcome> {
    let token = |field: &str| {
        answer[field]
            .as_str()
            .filter(|token| is_token(token, TOKEN_LIMIT))
            .map(Secret::new)
            .ok_or(Outcome::Refused)
    };
    if !answer["token_type"]
        .as_str()
        .is_some_and(|kind| kind.eq_ignore_ascii_case("DPoP"))
    {
        return Err(Outcome::Refused);
    }
    let access = token("access_token")?;
    let refresh = token("refresh_token")?;
    let subject = answer["sub"]
        .as_str()
        .filter(|subject| identity::valid_did(subject))
        .ok_or(Outcome::Refused)?
        .to_owned();
    let asked: Vec<&str> = asked.split(' ').filter(|word| !word.is_empty()).collect();
    let granted: Vec<String> = answer["scope"]
        .as_str()
        .unwrap_or_default()
        .split(' ')
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect();
    if !granted.iter().any(|word| word == SCOPE)
        || !granted.iter().all(|word| asked.contains(&word.as_str()))
    {
        return Err(Outcome::Refused);
    }
    let expires_in = answer["expires_in"]
        .as_u64()
        .unwrap_or(ACCESS_SECS_UNSAID)
        .min(ACCESS_SECS_MAX);
    Ok(Tokens {
        access,
        refresh,
        subject,
        granted,
        expires_in,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const DID: &str = "did:plc:aaaaaaaaaaaaaaaaaaaaaaaa";

    fn answer() -> Value {
        json!({
            "access_token": "access-1",
            "refresh_token": "refresh-1",
            "token_type": "DPoP",
            "expires_in": 3600,
            "scope": "atproto",
            "sub": DID,
        })
    }

    fn refused(field: &str, value: Value, asked: &str) {
        let mut flawed = answer();
        flawed[field] = value;
        assert!(
            matches!(checked(&flawed, asked), Err(Outcome::Refused)),
            "{field} flawed"
        );
    }

    #[test]
    fn an_answer_that_keeps_the_profile_is_taken() {
        let tokens = checked(&answer(), "atproto").expect("kept");
        assert_eq!(tokens.access.expose(), "access-1");
        assert_eq!(tokens.refresh.expose(), "refresh-1");
        assert_eq!(tokens.subject, DID);
        assert_eq!(tokens.granted, ["atproto"]);
        assert_eq!(tokens.expires_in, 3600);
        let mut unsaid = answer();
        unsaid["expires_in"] = Value::Null;
        assert_eq!(
            checked(&unsaid, "atproto").expect("kept").expires_in,
            ACCESS_SECS_UNSAID
        );
        let mut lower = answer();
        lower["token_type"] = json!("dpop");
        assert!(checked(&lower, "atproto").is_ok(), "the type has no case");
        let narrower = checked(&answer(), "atproto rpc:a.b?aud=*").expect("narrower");
        assert_eq!(narrower.granted, ["atproto"], "less than asked is fine");
    }

    #[test]
    fn a_token_that_is_not_dpop_bound_is_refused() {
        refused("token_type", json!("Bearer"), "atproto");
        refused("token_type", Value::Null, "atproto");
    }

    #[test]
    fn an_answer_without_a_refresh_token_is_refused() {
        refused("refresh_token", Value::Null, "atproto");
        refused("refresh_token", json!(""), "atproto");
        refused("refresh_token", json!("a b"), "atproto");
        refused("access_token", Value::Null, "atproto");
    }

    #[test]
    fn a_subject_that_is_no_supported_did_is_refused() {
        refused("sub", Value::Null, "atproto");
        refused("sub", json!("ada.example.test"), "atproto");
        refused("sub", json!("did:key:zabc"), "atproto");
    }

    #[test]
    fn a_scope_wider_than_asked_or_without_atproto_is_refused() {
        refused("scope", json!("atproto transition:generic"), "atproto");
        refused("scope", json!("transition:generic"), "transition:generic");
        refused("scope", Value::Null, "atproto");
    }
}
