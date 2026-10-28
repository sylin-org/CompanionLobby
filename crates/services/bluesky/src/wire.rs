//! How a request reaches the account's PDS, and what each answer means.
//!
//! Every request goes to the connection's address, which is the account's
//! own PDS, through the outbound agent. Each one is proved on its own: the
//! authority is asked for a proof of exactly that method and URL, and the
//! answer is presented as the access token under the scheme the credential
//! adapter named (`DPoP`, never `Bearer`) with the proof in the `DPoP`
//! header. A read the app view answers names it in the `atproto-proxy`
//! header, so the PDS forwards it there.
//!
//! The PDS's nonce is learned from every answer that carries one and passed
//! with the next ask. A 401 that asks for a fresh nonce is proved and sent
//! once more with it; any other 401 means the PDS did not take the token,
//! and the authority is asked once more to renew it first. Neither did
//! anything at the PDS, so a write is never doubled by either.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use lobby_contracts::{Address, Authority, AuthorizationAsk, Effect, Explained, Outcome};
use lobby_outbound::{Failure, Outbound, Request, Response};
use serde_json::Value;

/// The app view a read is proxied to, as the `atproto-proxy` header names it.
pub const APP_VIEW: &str = "did:web:api.bsky.app#bsky_appview";
/// The longest nonce kept.
const NONCE_LIMIT: usize = 1024;
/// How many PDS nonces are kept at most.
const NONCES: usize = 64;

/// The account's server did not take its sign-in.
pub const NOT_TAKEN: &str = "The account's server did not take its sign-in, even renewed. The sponsor signs the companion in again on the manager page.";
/// The account was not granted what the call needs.
pub const NOT_GRANTED: &str = "The account was not granted leave to do this on Bluesky. The sponsor grants it by signing the companion in again on the manager page; its other connections stay as they are.";
/// Bluesky refused the request as it stands.
pub const INVALID: &str =
    "Bluesky refused the request as it stands. Check the arguments against the tool's description.";
/// Bluesky refused the request.
pub const REFUSED: &str = "Bluesky refused this request.";
/// What a read was asked about does not exist.
pub const NOT_FOUND: &str = "Bluesky does not know that post, or the companion cannot see it. Read the timeline again to find it.";
/// Bluesky asked for fewer requests.
pub const SLOW_DOWN: &str = "Bluesky asked for fewer requests. Wait a while before trying again.";
/// Bluesky could not answer a read.
pub const UNAVAILABLE: &str = "Bluesky could not answer just now. Try again later.";
/// A write whose answer was lost.
pub const MAY_HAVE_LANDED: &str =
    "Bluesky may have kept this. Read the timeline before trying again, so it is not done twice.";

/// One request to the account's PDS.
#[derive(Debug, Clone)]
pub struct Call {
    /// `GET` or `POST`.
    pub method: &'static str,
    /// The path and query.
    pub path: String,
    /// The body, already encoded, and its type.
    pub body: Option<(&'static str, Vec<u8>)>,
    /// Whether the app view answers it, through the PDS.
    pub proxied: bool,
    /// Whether it reads or writes.
    pub effect: Effect,
}

/// The PDS nonces this adapter learned, by origin.
#[derive(Debug, Default)]
pub struct Nonces(Mutex<HashMap<String, String>>);

impl Nonces {
    fn get(&self, origin: &str) -> Option<String> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(origin)
            .cloned()
    }

    fn keep(&self, origin: &str, nonce: &str) {
        let mut kept = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if kept.len() >= NONCES && !kept.contains_key(origin) {
            kept.clear();
        }
        kept.insert(origin.to_owned(), nonce.to_owned());
    }
}

fn is_token(text: &str, limit: usize) -> bool {
    !text.is_empty() && text.len() <= limit && text.bytes().all(|byte| byte.is_ascii_graphic())
}

/// Whether a 401 asks for the PDS's nonce: `use_dpop_nonce` in the body's
/// `error`, or as the `error` of a challenge in the `WWW-Authenticate`
/// header, quoted or bare.
pub fn asks_for_nonce(response: &Response) -> bool {
    let in_body = serde_json::from_slice::<Value>(&response.body)
        .is_ok_and(|body| body["error"] == "use_dpop_nonce");
    let in_header = response.header("www-authenticate").is_some_and(|value| {
        value.split(',').any(|challenge| {
            challenge.split_whitespace().any(|parameter| {
                parameter.split_once('=').is_some_and(|(name, value)| {
                    name.eq_ignore_ascii_case("error")
                        && value.trim_matches('"') == "use_dpop_nonce"
                })
            })
        })
    });
    in_body || in_header
}

/// The outcome of a request that got no answer.
fn lost(failure: Failure, effect: Effect) -> Explained {
    match (failure, effect) {
        (Failure::Refused, _) => Outcome::Refused.into(),
        (Failure::Unreachable, _) | (Failure::Interrupted, Effect::Read) => {
            Explained::new(Outcome::TemporarilyUnavailable, UNAVAILABLE)
        }
        (Failure::Interrupted, Effect::Write { .. }) => {
            Explained::new(Outcome::Unknown, MAY_HAVE_LANDED)
        }
    }
}

/// Sends `call` to `address`, proved through `authority`, and answers the
/// body of a 200 as JSON, or what the answer means.
pub fn send(
    outbound: &Outbound,
    nonces: &Nonces,
    address: &Address,
    authority: &dyn Authority,
    call: &Call,
) -> Result<Value, Explained> {
    let url = address.url(&call.path);
    let mut nonce = nonces.get(address.as_str());
    let mut refused_before = false;
    let mut challenged = false;
    loop {
        let authorization = authority.authorize(&AuthorizationAsk::ProvedRequest {
            method: call.method.to_owned(),
            url: url.clone(),
            nonce: nonce.clone(),
            refused_before,
        })?;
        let Some(proof) = authorization
            .proof
            .as_ref()
            .filter(|_| authorization.scheme == "DPoP")
        else {
            // A token that is not proved per request is never presented
            // here.
            return Err(Outcome::Unsupported.into());
        };
        let mut request = Request::new(call.method, &call.path)
            .header("Authorization", authorization.header_value())
            .header("DPoP", proof.expose());
        if call.proxied {
            request = request.header("atproto-proxy", APP_VIEW);
        }
        if let Some((kind, body)) = &call.body {
            request = request.bytes(kind, body.clone());
        }
        let response = outbound
            .send(address, request)
            .map_err(|failure| lost(failure, call.effect))?;
        let fresh = response
            .header("dpop-nonce")
            .filter(|value| is_token(value, NONCE_LIMIT))
            .map(str::to_owned);
        if let Some(fresh) = &fresh {
            nonces.keep(address.as_str(), fresh);
        }
        match response.status {
            401 if asks_for_nonce(&response) && fresh.is_some() && !challenged => {
                challenged = true;
                nonce = fresh;
            }
            401 if !asks_for_nonce(&response) && !refused_before => {
                refused_before = true;
                nonce = fresh.or(nonce);
            }
            _ => return answer(&response, call.effect),
        }
    }
}

/// What an answer means.
fn answer(response: &Response, effect: Effect) -> Result<Value, Explained> {
    let code = serde_json::from_slice::<Value>(&response.body)
        .ok()
        .and_then(|body| body["error"].as_str().map(str::to_owned));
    match (response.status, code.as_deref()) {
        (200, _) => serde_json::from_slice::<Value>(&response.body)
            .ok()
            .filter(Value::is_object)
            .ok_or(match effect {
                Effect::Read => Explained::new(Outcome::TemporarilyUnavailable, UNAVAILABLE),
                Effect::Write { .. } => Explained::new(Outcome::Unknown, MAY_HAVE_LANDED),
            }),
        (401, _) => Err(Explained::new(Outcome::Refused, NOT_TAKEN)),
        (403, Some("ScopeMissingError" | "InsufficientScope")) => {
            Err(Explained::new(Outcome::NeedsPermission, NOT_GRANTED))
        }
        (403, _) => Err(Explained::new(Outcome::Refused, REFUSED)),
        (404, _) | (400, Some("NotFound" | "RecordNotFound")) => {
            Err(Explained::new(Outcome::NotFound, NOT_FOUND))
        }
        (429, _) => Err(Explained::new(Outcome::RateLimited, SLOW_DOWN)),
        (400..=499, _) => Err(Explained::new(Outcome::InvalidInput, INVALID)),
        _ => Err(match effect {
            Effect::Read => Explained::new(Outcome::TemporarilyUnavailable, UNAVAILABLE),
            Effect::Write { .. } => Explained::new(Outcome::Unknown, MAY_HAVE_LANDED),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lobby_contracts::Retry;

    fn response(status: u16, body: &str, headers: &[(&str, &str)]) -> Response {
        Response {
            status,
            headers: headers
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
            body: body.as_bytes().to_vec(),
        }
    }

    const WRITE: Effect = Effect::Write {
        retry: Retry::ReadFirst,
    };

    #[test]
    fn a_nonce_challenge_is_read_from_the_body_or_the_header() {
        assert!(asks_for_nonce(&response(
            401,
            r#"{"error":"use_dpop_nonce"}"#,
            &[]
        )));
        assert!(asks_for_nonce(&response(
            401,
            "",
            &[(
                "www-authenticate",
                r#"DPoP error="use_dpop_nonce", error_description="x""#
            )]
        )));
        assert!(!asks_for_nonce(&response(
            401,
            r#"{"error":"InvalidToken"}"#,
            &[]
        )));
    }

    #[test]
    fn every_answer_means_one_outcome_in_the_networks_words() {
        let outcome = |status, body: &str, effect| {
            answer(&response(status, body, &[]), effect).map_err(|explained| explained.outcome)
        };
        assert!(outcome(200, r#"{"uri":"at://x"}"#, Effect::Read).is_ok());
        assert_eq!(
            outcome(200, "[]", Effect::Read),
            Err(Outcome::TemporarilyUnavailable)
        );
        assert_eq!(outcome(200, "not json", WRITE), Err(Outcome::Unknown));
        assert_eq!(
            outcome(403, r#"{"error":"ScopeMissingError"}"#, WRITE),
            Err(Outcome::NeedsPermission)
        );
        assert_eq!(
            outcome(403, r#"{"error":"Other"}"#, WRITE),
            Err(Outcome::Refused)
        );
        assert_eq!(
            outcome(400, r#"{"error":"InvalidRequest"}"#, WRITE),
            Err(Outcome::InvalidInput)
        );
        assert_eq!(outcome(404, "", Effect::Read), Err(Outcome::NotFound));
        assert_eq!(outcome(429, "", WRITE), Err(Outcome::RateLimited));
        assert_eq!(outcome(502, "", WRITE), Err(Outcome::Unknown));
        assert_eq!(
            outcome(502, "", Effect::Read),
            Err(Outcome::TemporarilyUnavailable)
        );
        assert_eq!(
            lost(Failure::Interrupted, WRITE).outcome,
            Outcome::Unknown,
            "a write whose answer was lost may have landed"
        );
        assert_eq!(
            lost(Failure::Unreachable, WRITE).outcome,
            Outcome::TemporarilyUnavailable,
            "a write that never left did nothing"
        );
    }

    #[test]
    fn nonces_are_kept_by_origin_within_a_bound() {
        let nonces = Nonces::default();
        nonces.keep("http://127.0.0.1:1", "n-1");
        assert_eq!(nonces.get("http://127.0.0.1:1").as_deref(), Some("n-1"));
        for index in 0..NONCES + 1 {
            nonces.keep(&format!("http://127.0.0.1:{}", index + 2), "n");
        }
        assert!(nonces.0.lock().expect("nonces").len() <= NONCES);
    }
}
