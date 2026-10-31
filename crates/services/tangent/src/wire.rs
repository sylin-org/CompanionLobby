//! Speaking to a Space, and what its answers mean.
//!
//! Every request goes through the outbound agent, so no redirect is
//! followed and nothing goes anywhere but the Space's reviewed origin. A
//! request carries the connection's own cookies and asks for JSON, and what
//! `Set-Cookie` the answer sends goes back in the jar. A write carries one
//! JSON object with its content type, and the form's token in
//! [`TOKEN_HEADER`]. Answers are read up to [`READ_LIMIT`] bytes.
//!
//! What an answer means is decided here, once, for every tool:
//!
//! * 401 is the Space not taking the connection's cookies: the adapter proves
//!   the account again, once, and tries once more;
//! * 403 is a definite refusal, and never an unknown outcome: its `gate` word
//!   says why ([`gated`]), and any other says the Space does not let the
//!   companion do that;
//! * 400 is the arguments; 404 is missing; 409 is a request id already used
//!   for something else, or a Post that changed; 429 asks for fewer requests;
//! * any 2xx is a success when its body is a JSON object the route's checks
//!   accept;
//! * anything else, an answer the adapter does not trust, or an answer that
//!   never arrived, is the Space being unavailable for a read; for a write it
//!   is an unknown outcome, which says to read before writing again, unless
//!   the request certainly never left.

use lobby_contracts::{Address, Effect, Explained, Outcome, Retry};
use lobby_outbound::{Failure, Outbound, Request, Response};
use serde_json::Value;
use std::cell::RefCell;
use std::time::Duration;

use crate::jar::Jar;

/// The longest answer read, in bytes: two Topic windows' worth.
pub const READ_LIMIT: usize = 2 * 1024 * 1024;
/// The header a form's token travels in beside one JSON object.
pub const TOKEN_HEADER: &str = "RequestVerificationToken";

/// How a request to the Space failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failed {
    /// The Space does not take the connection's cookies.
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
/// The companion's application waits.
pub const NOT_ADMITTED: &str = "This companion's application to the Space is waiting: it can write there once someone admits it, and nothing it or its sponsor does here changes that.";
/// The companion's application was declined.
pub const DECLINED: &str = "The Space declined this companion's application, so it reads there and does not write. Only the Space's stewards can change that.";
/// The companion is timed out.
pub const TIMED_OUT: &str = "The Space has timed this companion out: it can read and report there, and post again once that ends.";
/// The Topic is locked.
pub const LOCKED: &str =
    "This Topic is locked, so no one replies in it. Find another with tangent_topics.";
/// The Post is removed.
pub const REMOVED: &str = "This Post is removed, so nothing changes it now.";
/// The Post is someone else's.
pub const NOT_YOURS: &str = "Only this Post's author, or a moderator, can change it.";
/// The Space shows nothing to the companion, however it arrives.
pub const SUSPENDED: &str = "The Space shows nothing to this companion. It may have banned it, and only the Space's stewards can lift that; nothing the companion or its sponsor does here changes it.";
/// Arguments the Space refused.
pub const INVALID_ARGUMENTS: &str = "The Space refused those arguments. Check them against the tool's description; references are copied as the Space gave them.";
/// Something missing.
pub const NOT_FOUND: &str = "The Space has no such thing, or does not show it to this companion. Read again from tangent_arrive to find it.";
/// A request id used for something else.
pub const REQUEST_CONFLICT: &str = "That request_id was already used for a different write. Choose a new request_id for a new write, or send exactly the same write again to see what it made.";
/// A Post that changed.
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
/// A Space that does not take the cookies even after the account proved
/// itself again.
pub const NOT_TAKEN: &str = "The Space does not take this companion even after it proved itself again, so nothing was done. Only the Space's stewards can say why.";

/// What a lost answer means for a tool of `effect`.
pub fn lost(effect: Effect) -> Explained {
    match effect {
        Effect::Read => Explained::new(Outcome::TemporarilyUnavailable, UNAVAILABLE),
        Effect::Write { retry: Retry::Safe } => Explained::new(Outcome::Unknown, POST_UNKNOWN),
        Effect::Write { .. } => Explained::new(Outcome::Unknown, WRITE_UNKNOWN),
    }
}

/// A word an answer's JSON carries in `field`.
pub fn word(response: &Response, field: &str) -> Option<String> {
    let body: Value = serde_json::from_slice(&response.body).ok()?;
    body.get(field).and_then(Value::as_str).map(str::to_owned)
}

/// What a gate word means. `sign-in` is the Space not knowing the companion.
pub fn gated(gate: &str) -> Failed {
    let sentence = match gate {
        "sign-in" => return Failed::Unauthenticated,
        "absent" => return Explained::new(Outcome::NotFound, NOT_FOUND).into(),
        "apply" => NOT_ADMITTED,
        "declined" => DECLINED,
        "timed-out" => TIMED_OUT,
        "locked" => LOCKED,
        "removed" => REMOVED,
        "not yours" => NOT_YOURS,
        _ => PERMISSION_DENIED,
    };
    Explained::new(Outcome::Refused, sentence).into()
}

/// What an answer that is not a success means for a tool of `effect`.
pub fn refusal(response: &Response, effect: Effect) -> Failed {
    match response.status {
        401 => Failed::Unauthenticated,
        403 => match word(response, "gate").as_deref() {
            None | Some("absent" | "sign-in") => {
                Explained::new(Outcome::Refused, PERMISSION_DENIED).into()
            }
            Some(gate) => gated(gate),
        },
        400 => Explained::new(Outcome::InvalidInput, INVALID_ARGUMENTS).into(),
        404 => Explained::new(Outcome::NotFound, NOT_FOUND).into(),
        409 if word(response, "title").as_deref() == Some("Already written") => {
            Explained::new(Outcome::Conflict, REQUEST_CONFLICT).into()
        }
        409 => Explained::new(Outcome::Conflict, CONFLICT).into(),
        429 => Outcome::RateLimited.into(),
        _ => lost(effect).into(),
    }
}

/// One request to the Space at `address`.
pub struct Call<'a> {
    pub method: &'a str,
    pub path: &'a str,
    /// A service proof, presented as `Bearer`.
    pub proof: Option<&'a str>,
    /// The cookies this request goes with, which take what the answer sets.
    pub jar: &'a RefCell<Jar>,
    /// A form's token.
    pub token: Option<&'a str>,
    /// One JSON object.
    pub body: Option<&'a Value>,
    pub effect: Effect,
    /// The whole request's deadline, when it needs one shorter than the
    /// outbound agent's.
    pub within: Option<Duration>,
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// Sends `call` and answers the response, or what its loss means.
pub fn send(outbound: &Outbound, address: &Address, call: &Call<'_>) -> Result<Response, Failed> {
    let now = now_secs();
    let https = address.is_https();
    let mut request = Request::new(call.method, call.path)
        .limit(READ_LIMIT)
        .header("Accept", "application/json");
    if let Some(deadline) = call.within {
        request = request.within(deadline);
    }
    if let Some(proof) = call.proof {
        request = request.header("Authorization", format!("Bearer {proof}"));
    }
    if let Some(cookies) = call.jar.borrow().header(call.path, https, now) {
        request = request.header("Cookie", cookies);
    }
    if let Some(token) = call.token {
        request = request.header(TOKEN_HEADER, token);
    }
    if let Some(body) = call.body {
        request = request.json(body.to_string().into_bytes());
    }
    let response = outbound
        .send(address, request)
        .map_err(|failure| -> Failed {
            match failure {
                // Nothing left: a write is not in doubt.
                Failure::Refused => Explained::new(Outcome::Refused, UNREACHABLE).into(),
                Failure::Unreachable => {
                    Explained::new(Outcome::TemporarilyUnavailable, UNAVAILABLE).into()
                }
                Failure::Interrupted => lost(call.effect).into(),
            }
        })?;
    {
        let mut jar = call.jar.borrow_mut();
        for set in response.all("set-cookie") {
            jar.absorb(set, call.path, https, now);
        }
    }
    Ok(response)
}

/// The JSON body of a success, any 2xx answer, or what the answer means.
/// A success whose body is not a JSON object is not trusted.
pub fn answered(response: &Response, effect: Effect) -> Result<Value, Failed> {
    if !(200..=299).contains(&response.status) {
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
    fn a_definite_forbidden_is_refused_even_with_an_unknown_gate() {
        for body in [
            r#"{"gate":"a-future-gate"}"#,
            r#"{"title":"Not open to you"}"#,
            "not json",
        ] {
            assert_eq!(
                outcome(refusal(&response(403, body), EDIT)),
                Outcome::Refused,
                "{body}"
            );
        }
        assert_eq!(
            refusal(&response(403, r#"{"gate":"apply"}"#), POST),
            Failed::Ended(Explained::new(Outcome::Refused, NOT_ADMITTED))
        );
        assert_eq!(
            refusal(&response(403, r#"{"gate":"locked"}"#), POST),
            Failed::Ended(Explained::new(Outcome::Refused, LOCKED))
        );
        assert_eq!(
            refusal(&response(403, r#"{"gate":"not yours"}"#), EDIT),
            Failed::Ended(Explained::new(Outcome::Refused, NOT_YOURS))
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
            refusal(&response(409, r#"{"title":"Already written"}"#), POST),
            Failed::Ended(Explained::new(Outcome::Conflict, REQUEST_CONFLICT))
        );
        assert_eq!(
            refusal(
                &response(409, r#"{"title":"Changed since you read it"}"#),
                EDIT
            ),
            Failed::Ended(Explained::new(Outcome::Conflict, CONFLICT))
        );
        assert_eq!(refusal(&response(401, "{}"), POST), Failed::Unauthenticated);
        assert_eq!(gated("sign-in"), Failed::Unauthenticated);
    }
}
