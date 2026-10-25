//! Minting a service's proof at the account's PDS.
//!
//! A service that asks for a proof names the audience it declared for
//! itself and the method the proof allows, as an XRPC method name (an
//! `lxm`). Before anything is read or sent, the ask must be one an atproto
//! account can answer safely:
//!
//! * the audience is an exact service reference, `<did>#<service>`, never
//!   a bare DID: a proof for a bare DID would be taken by any service of
//!   that DID. The DID is a did:plc, or a did:web whose host carries a port
//!   only when it is `localhost` (percent-encoded as `%3A`, as did:web
//!   writes it); the whole reference is at most [`AUDIENCE_LIMIT`] bytes;
//! * the method is an NSID: at least three dot-separated segments of
//!   letters, digits and hyphens.
//!
//! Then, holding the credential's lock and renewing the session first when
//! it is due, the material must have been granted the method: an
//! `rpc:<lxm>?aud=*` permission, or one naming this very audience, as the
//! PDS checks it. A session without it was signed in before a service
//! needed it, and only a new sign-in grants it; the lobby says so before
//! anything reaches the PDS.
//!
//! The proof is asked for with `GET
//! /xrpc/com.atproto.server.getServiceAuth?aud=..&lxm=..&exp=..` at the
//! account's PDS, presenting the access token under the `DPoP` scheme with
//! a proof made with the session's key: `htm` `GET`, `htu` the URL without
//! its query, `ath` the access token's hash, and the PDS's own nonce for
//! that key once it issued one (never the authorization server's). A PDS
//! that answers 401 with a fresh `DPoP-Nonce` and `use_dpop_nonce`, in the
//! body's `error` or in the `WWW-Authenticate` header, is asked once more
//! with it; a second challenge is a PDS that cannot be satisfied just now.
//! The proof expires at most [`PROOF_SECS`] after it is asked for, or
//! sooner when the service asked for less. A 200 carrying a token is the
//! proof, presented to the service under `Bearer`; a 403 is the PDS
//! refusing the method, which only a new sign-in can widen; any other 401
//! is a session the PDS does not take, which needs signing in again.
//!
//! Nothing the PDS sends ever reaches a sentence: every refusal is one of
//! this module's own.

use lobby_contracts::{Authorization, AuthorizationAsk, Explained, MaterialSlot, Outcome, Secret};
use lobby_outbound::{Failure, Outbound, Request};
use serde_json::Value;

use crate::codec::{is_token, now_secs, percent_encode};
use crate::identity;
use crate::material::Material;
use crate::renewal::{Nonces, Renewer};
use crate::tokens::TOKEN_LIMIT;

/// The longest audience accepted, in bytes.
pub const AUDIENCE_LIMIT: usize = 512;
/// The longest method accepted, in bytes.
pub const METHOD_LIMIT: usize = 253;
/// The longest life a proof is asked for, in seconds.
pub const PROOF_SECS: u32 = 120;
/// The PDS route that mints a service's proof.
pub const SERVICE_AUTH_PATH: &str = "/xrpc/com.atproto.server.getServiceAuth";
/// The longest nonce kept.
const NONCE_LIMIT: usize = 1024;

/// Why an account signed in without leave to prove itself to a service
/// cannot give one, and what brings it.
pub const NOT_GRANTED: &str = "The account was signed in without leave to prove itself to this service. Sign the companion in again on the manager page to grant it.";

/// What the PDS refusing the method says.
pub const REFUSED_BY_PDS: &str = "The account's server refused to prove the account to this service. Sign the companion in again on the manager page to grant it.";

/// What a session the PDS does not take says.
pub const NOT_TAKEN: &str = "The account's server does not take this sign-in any more. Sign the companion in again on the manager page.";

/// Whether `audience` is an exact service reference an atproto account may
/// prove itself to.
pub fn valid_audience(audience: &str) -> bool {
    if audience.len() > AUDIENCE_LIMIT || audience.bytes().any(|byte| !byte.is_ascii_graphic()) {
        return false;
    }
    let Some((did, service)) = audience.split_once('#') else {
        return false;
    };
    let service_ok = !service.is_empty()
        && service.len() <= 64
        && service
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-');
    service_ok && audience_did(did)
}

/// A did:plc, or a did:web whose host carries a port only when it is
/// `localhost`.
fn audience_did(did: &str) -> bool {
    if did.starts_with("did:plc:") {
        return identity::valid_did(did);
    }
    let Some(host) = did.strip_prefix("did:web:") else {
        return false;
    };
    match host.split_once("%3A") {
        Some(("localhost", port)) => port
            .parse::<u16>()
            .is_ok_and(|number| number != 0 && number.to_string() == port),
        Some(_) => false,
        None => host == "localhost" || identity::valid_handle(host),
    }
}

/// Whether `method` is an NSID.
pub fn valid_method(method: &str) -> bool {
    let segments: Vec<&str> = method.split('.').collect();
    method.len() <= METHOD_LIMIT
        && segments.len() >= 3
        && segments.iter().all(|segment| {
            !segment.is_empty()
                && segment.len() <= 63
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

/// Whether `permissions` grant proving the account to `audience` for
/// `method`.
pub fn granted(permissions: &[String], audience: &str, method: &str) -> bool {
    let any = format!("rpc:{method}?aud=*");
    let this = format!("rpc:{method}?aud={audience}");
    permissions
        .iter()
        .any(|permission| *permission == any || *permission == this)
}

/// Whether a 401 asks for the PDS's nonce: `use_dpop_nonce` in the body's
/// `error`, or as the `error` of a challenge in the `WWW-Authenticate`
/// header, quoted or bare.
pub fn asks_for_nonce(response: &lobby_outbound::Response) -> bool {
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

/// Mints the proof `ask` names from the session in `slot`.
pub fn mint(
    renewer: &Renewer<'_>,
    outbound: &Outbound,
    nonces: &Nonces,
    slot: &dyn MaterialSlot,
    ask: &AuthorizationAsk,
) -> Result<Authorization, Explained> {
    let AuthorizationAsk::ServiceProof {
        audience,
        method,
        lifetime_secs,
    } = ask;
    if !valid_audience(audience) || !valid_method(method) || *lifetime_secs == 0 {
        return Err(Outcome::Unsupported.into());
    }
    let lock = renewer.locks.of(slot.credential());
    let _held = lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // A session that is due is renewed before it proves anything.
    renewer.renew_held(slot)?;
    let material = Material::decode(&slot.read()?, renewer.policy).ok_or(Outcome::NeedsSignIn)?;
    if !granted(&material.permissions, audience, method) {
        return Err(Explained::new(Outcome::NeedsPermission, NOT_GRANTED));
    }
    let exp = now_secs() + i64::from((*lifetime_secs).min(PROOF_SECS));
    let path = format!(
        "{SERVICE_AUTH_PATH}?aud={}&lxm={}&exp={exp}",
        percent_encode(audience),
        percent_encode(method)
    );
    let url = material.pds.url(&path);
    let thumbprint = material.key.thumbprint();
    let origin = material.pds.as_str().to_owned();
    let mut nonce = nonces.get(&origin, &thumbprint);
    for attempt in 0..2 {
        let proof =
            material
                .key
                .resource_proof("GET", &url, nonce.as_deref(), material.access.expose())?;
        let response = outbound
            .send(
                &material.pds,
                Request::new("GET", &path)
                    .header(
                        "Authorization",
                        format!("DPoP {}", material.access.expose()),
                    )
                    .header("DPoP", proof),
            )
            .map_err(|failure| match failure {
                Failure::Refused => Outcome::Refused,
                Failure::Unreachable | Failure::Interrupted => Outcome::TemporarilyUnavailable,
            })?;
        let fresh = response
            .header("dpop-nonce")
            .filter(|value| is_token(value, NONCE_LIMIT))
            .map(str::to_owned);
        if let Some(fresh) = &fresh {
            nonces.keep(&origin, &thumbprint, fresh);
        }
        match response.status {
            200 => {
                let token = serde_json::from_slice::<Value>(&response.body)
                    .ok()
                    .and_then(|body| body["token"].as_str().map(str::to_owned))
                    .filter(|token| is_token(token, TOKEN_LIMIT))
                    .ok_or(Outcome::TemporarilyUnavailable)?;
                return Ok(Authorization {
                    scheme: "Bearer".to_owned(),
                    token: Secret::new(token),
                });
            }
            401 if asks_for_nonce(&response) && fresh.is_some() => {
                if attempt == 0 {
                    nonce = fresh;
                    continue;
                }
                return Err(Outcome::TemporarilyUnavailable.into());
            }
            401 => return Err(Explained::new(Outcome::NeedsSignIn, NOT_TAKEN)),
            403 => return Err(Explained::new(Outcome::NeedsPermission, REFUSED_BY_PDS)),
            429 => return Err(Outcome::RateLimited.into()),
            400..=499 => return Err(Outcome::Refused.into()),
            _ => return Err(Outcome::TemporarilyUnavailable.into()),
        }
    }
    Err(Outcome::TemporarilyUnavailable.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_exact_service_reference_is_an_audience() {
        for good in [
            "did:web:space.example.test#svc",
            "did:web:localhost%3A5300#svc",
            "did:web:localhost#svc",
            "did:plc:aaaaaaaaaaaaaaaaaaaaaaaa#service_1",
        ] {
            assert!(valid_audience(good), "{good}");
        }
        for bad in [
            "did:web:space.example.test",
            "did:plc:aaaaaaaaaaaaaaaaaaaaaaaa",
            "did:web:space.example.test#",
            "did:web:space.example.test#a#b",
            "did:web:space.example.test%3A8443#svc",
            "did:web:localhost%3A0#svc",
            "did:web:localhost%3A05300#svc",
            "did:web:space.example.test#svc&lxm=evil",
            "did:web:space.example.test#svc x",
            "did:key:zabc#svc",
            "",
        ] {
            assert!(!valid_audience(bad), "{bad}");
        }
        let long = format!("did:web:{}.test#svc", "a".repeat(AUDIENCE_LIMIT));
        assert!(!valid_audience(&long));
    }

    #[test]
    fn a_method_is_an_nsid() {
        assert!(valid_method("org.example.service.exchangeIdentity"));
        assert!(valid_method("com.atproto.server.getServiceAuth"));
        for bad in ["", "exchange", "a.b", "a..b.c", "a.b.c/d", "a.b.c?x"] {
            assert!(!valid_method(bad), "{bad}");
        }
    }

    #[test]
    fn a_method_is_granted_for_any_audience_or_for_this_one() {
        let method = "org.example.service.exchangeIdentity";
        let audience = "did:web:space.example.test#svc";
        let words = |list: &[&str]| {
            list.iter()
                .map(|word| (*word).to_owned())
                .collect::<Vec<_>>()
        };
        assert!(granted(
            &words(&["atproto", &format!("rpc:{method}?aud=*")]),
            audience,
            method
        ));
        assert!(granted(
            &words(&[&format!("rpc:{method}?aud={audience}")]),
            audience,
            method
        ));
        assert!(!granted(&words(&["atproto"]), audience, method));
        assert!(!granted(
            &words(&[&format!("rpc:{method}?aud=did:web:other.test#svc")]),
            audience,
            method
        ));
        assert!(!granted(
            &words(&["rpc:org.example.other.method?aud=*"]),
            audience,
            method
        ));
    }

    fn challenge(body: &str, header: Option<&str>) -> lobby_outbound::Response {
        let mut headers = vec![("dpop-nonce".to_owned(), "n-1".to_owned())];
        if let Some(value) = header {
            headers.push(("www-authenticate".to_owned(), value.to_owned()));
        }
        lobby_outbound::Response {
            status: 401,
            headers,
            body: body.as_bytes().to_vec(),
        }
    }

    #[test]
    fn a_nonce_challenge_is_read_from_the_body_or_the_header() {
        assert!(asks_for_nonce(&challenge(
            r#"{"error":"use_dpop_nonce"}"#,
            None
        )));
        assert!(asks_for_nonce(&challenge(
            "",
            Some(
                r#"DPoP error="use_dpop_nonce", error_description="Resource server requires nonce in DPoP proof""#
            )
        )));
        assert!(asks_for_nonce(&challenge(
            "",
            Some("DPoP error=use_dpop_nonce")
        )));
        assert!(asks_for_nonce(&challenge(
            "",
            Some(r#"Bearer realm="x", DPoP ERROR="use_dpop_nonce""#)
        )));
        assert!(!asks_for_nonce(&challenge(
            r#"{"error":"InvalidToken"}"#,
            Some(r#"DPoP error="invalid_token""#)
        )));
    }
}
