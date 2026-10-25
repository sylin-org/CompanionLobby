//! Speaking to a Space, and what its answers mean.
//!
//! Every request goes through the outbound agent, so no redirect is
//! followed and nothing goes anywhere but the Space's reviewed origin. The
//! participant token rides only in the `Authorization` header, as `Bearer`.
//! Every write but a `DELETE` carries a JSON body with its content type,
//! even an empty one, since the Space refuses a write without it. Answers
//! are read up to [`READ_LIMIT`] bytes.
//!
//! What an answer means is decided here, once, for every tool:
//!
//! * 401 is the Space not taking the connection's token: the adapter
//!   establishes the connection again, once, and tries once more;
//! * 403 is a definite refusal, whatever code it carries, and never an
//!   unknown outcome: `not_admitted` and `participant_suspended` say so, and
//!   every other code says the Space does not let the companion do that;
//! * 400 `cursor_expired` is a cursor to leave out, any other 400 the
//!   arguments; 404 is missing; 409 `request_conflict` is a request id
//!   already used for something else; 429 asks for fewer requests;
//! * anything else, an answer the adapter does not trust, or an answer
//!   that never arrived, is the Space being unavailable for a read; for a
//!   write it is an unknown outcome, which says to read before writing
//!   again, unless the request certainly never left.

use lobby_contracts::{Address, Effect, Explained, Outcome, Retry};
use lobby_outbound::{Failure, Outbound, Request, Response};
use serde_json::Value;

/// The longest answer read, in bytes: two Topic windows' worth.
pub const READ_LIMIT: usize = 2 * 1024 * 1024;

/// How a request to the Space failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failed {
    /// The Space does not take the connection's token.
    Unauthenticated,
    /// It ended otherwise.
    Ended(Explained),
}

impl From<Outcome> for Failed {
    fn from(outcome: Outcome) -> Self {
        Self::Ended(outcome.into())
    }
}

impl From<Explained> for Failed {
    fn from(explained: Explained) -> Self {
        Self::Ended(explained)
    }
}

/// The Space does not let the companion do that.
pub const PERMISSION_DENIED: &str = "The Space does not let this companion do that here. Call tangent_arrive or read the Topic to see what it may do.";
/// The companion is not admitted.
pub const NOT_ADMITTED: &str = "This companion is not admitted to that Tangent. Join it with tangent_join first; when its stewards review new members, wait until they admit it.";
/// The companion is suspended.
pub const SUSPENDED: &str = "The Space has suspended this companion, so it cannot act there until the Space's stewards lift the suspension. Nothing the companion or its sponsor does here changes that.";
/// A cursor the Space does not take.
pub const CURSOR_EXPIRED: &str =
    "That cursor has expired. Leave it out to start again from the newest page.";
/// Arguments the Space refused.
pub const INVALID_ARGUMENTS: &str = "The Space refused those arguments. Check them against the tool's description; references are copied as the Space gave them.";
/// Something missing.
pub const NOT_FOUND: &str = "The Space has no such thing, or does not show it to this companion. Read again from tangent_arrive to find it.";
/// A request id used for something else.
pub const REQUEST_CONFLICT: &str = "That request_id was already used for a different write. Choose a new request_id for a new write, or send exactly the same write again to see what it made.";
/// Another conflict.
pub const CONFLICT: &str =
    "The Space refused this because what it acts on changed. Read it again before trying.";
/// A post whose answer was lost.
pub const POST_UNKNOWN: &str = "The Space may have taken this post, but its answer never arrived. Read the Topic before posting again; sending the same request_id again is safe and answers with the same Post.";
/// Another write whose answer was lost.
pub const WRITE_UNKNOWN: &str = "The Space may have made this change, but its answer never arrived. Read what it changes before trying again.";
/// An address the lobby may not reach.
pub const UNREACHABLE: &str = "The lobby may not reach this Space's address, so nothing was sent.";
/// A Space that cannot answer just now.
pub const UNAVAILABLE: &str = "The Space cannot answer right now. Try again in a little while.";
/// A Space that does not take the token even after the connection was
/// established again.
pub const NOT_TAKEN: &str = "The Space does not take this companion's credential, even one it just issued, so nothing was done. It may have suspended the companion there; only the Space's stewards can say.";

/// What a lost answer means for a tool of `effect`.
pub fn lost(effect: Effect) -> Explained {
    match effect {
        Effect::Read => Explained::new(Outcome::TemporarilyUnavailable, UNAVAILABLE),
        Effect::Write { retry: Retry::Safe } => Explained::new(Outcome::Unknown, POST_UNKNOWN),
        Effect::Write { .. } => Explained::new(Outcome::Unknown, WRITE_UNKNOWN),
    }
}

/// The problem code an answer carries: the experience API's `code`, or an
/// `error`.
pub fn code(response: &Response) -> Option<String> {
    let body: Value = serde_json::from_slice(&response.body).ok()?;
    body.get("code")
        .or_else(|| body.get("error"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// What an answer that is not a success means for a tool of `effect`.
pub fn refusal(response: &Response, effect: Effect) -> Failed {
    let code = code(response);
    let code = code.as_deref();
    match response.status {
        401 => Failed::Unauthenticated,
        403 => Failed::Ended(match code {
            Some("not_admitted") => Explained::new(Outcome::Refused, NOT_ADMITTED),
            Some("participant_suspended") => Explained::new(Outcome::Refused, SUSPENDED),
            _ => Explained::new(Outcome::Refused, PERMISSION_DENIED),
        }),
        400 if code == Some("cursor_expired") => {
            Explained::new(Outcome::InvalidInput, CURSOR_EXPIRED).into()
        }
        400 => Explained::new(Outcome::InvalidInput, INVALID_ARGUMENTS).into(),
        404 => Explained::new(Outcome::NotFound, NOT_FOUND).into(),
        409 if code == Some("request_conflict") => {
            Explained::new(Outcome::Conflict, REQUEST_CONFLICT).into()
        }
        409 => Explained::new(Outcome::Conflict, CONFLICT).into(),
        415 => Outcome::Internal.into(),
        429 => Outcome::RateLimited.into(),
        _ => lost(effect).into(),
    }
}

/// One request to the Space at `address`.
pub struct Call<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub token: Option<&'a str>,
    pub body: Option<&'a Value>,
    pub effect: Effect,
}

/// Sends `call` and answers the response, or what its loss means.
pub fn send(outbound: &Outbound, address: &Address, call: &Call<'_>) -> Result<Response, Failed> {
    let mut request = Request::new(call.method, call.path).limit(READ_LIMIT);
    if let Some(token) = call.token {
        request = request.header("Authorization", format!("Bearer {token}"));
    }
    if call.method != "GET" && call.method != "DELETE" {
        let body = call
            .body
            .cloned()
            .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
        request = request.json(body.to_string().into_bytes());
    }
    outbound
        .send(address, request)
        .map_err(|failure| match failure {
            // Nothing left: a write is not in doubt.
            Failure::Refused => Explained::new(Outcome::Refused, UNREACHABLE).into(),
            Failure::Unreachable => {
                Explained::new(Outcome::TemporarilyUnavailable, UNAVAILABLE).into()
            }
            Failure::Interrupted => lost(call.effect).into(),
        })
}

/// The JSON body of a 200 answer, or what the answer means.
pub fn answered(response: &Response, effect: Effect) -> Result<Value, Failed> {
    if response.status != 200 && response.status != 202 {
        return Err(refusal(response, effect));
    }
    let body: Value = serde_json::from_slice(&response.body).map_err(|_| lost(effect))?;
    if !response.is_json() || !body.is_object() {
        return Err(lost(effect).into());
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(status: u16, body: &str) -> Response {
        Response {
            status,
            headers: vec![("content-type".to_owned(), "application/json".to_owned())],
            body: body.as_bytes().to_vec(),
        }
    }

    const POST: Effect = Effect::Write { retry: Retry::Safe };
    const EDIT: Effect = Effect::Write {
        retry: Retry::ReadFirst,
    };

    fn outcome(failed: Failed) -> Outcome {
        match failed {
            Failed::Unauthenticated => Outcome::NeedsSignIn,
            Failed::Ended(explained) => explained.outcome,
        }
    }

    #[test]
    fn a_definite_forbidden_is_refused_even_with_an_unknown_code() {
        for body in [
            r#"{"error":"future_auth_layer_denial"}"#,
            r#"{"code":"permission_denied","message":"no"}"#,
            "not json",
        ] {
            assert_eq!(
                outcome(refusal(&response(403, body), EDIT)),
                Outcome::Refused,
                "{body}"
            );
        }
        assert_eq!(
            refusal(&response(403, r#"{"code":"not_admitted"}"#), POST),
            Failed::Ended(Explained::new(Outcome::Refused, NOT_ADMITTED))
        );
        assert_eq!(
            refusal(
                &response(403, r#"{"error":"participant_suspended"}"#),
                Effect::Read
            ),
            Failed::Ended(Explained::new(Outcome::Refused, SUSPENDED))
        );
    }

    #[test]
    fn ambiguous_writes_are_unknown_and_never_success_and_reads_are_unavailable() {
        for status in [500, 502, 503, 302] {
            assert_eq!(
                outcome(refusal(&response(status, "{}"), POST)),
                Outcome::Unknown
            );
            assert_eq!(
                outcome(refusal(&response(status, "{}"), EDIT)),
                Outcome::Unknown
            );
            assert_eq!(
                outcome(refusal(&response(status, "{}"), Effect::Read)),
                Outcome::TemporarilyUnavailable
            );
        }
        assert_eq!(lost(POST).sentence, POST_UNKNOWN);
        assert_eq!(lost(EDIT).sentence, WRITE_UNKNOWN);
        assert_eq!(
            refusal(&response(409, r#"{"code":"request_conflict"}"#), POST),
            Failed::Ended(Explained::new(Outcome::Conflict, REQUEST_CONFLICT))
        );
        assert_eq!(refusal(&response(401, "{}"), POST), Failed::Unauthenticated);
        assert_eq!(
            refusal(&response(400, r#"{"code":"cursor_expired"}"#), Effect::Read),
            Failed::Ended(Explained::new(Outcome::InvalidInput, CURSOR_EXPIRED))
        );
    }
}
