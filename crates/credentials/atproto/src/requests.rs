//! Proving one request to the account's own PDS.
//!
//! A service that lives on the account's PDS, such as one whose records
//! sit in the account's own repository, sends its requests there itself
//! and asks for a proof of each one. Before anything is read, the ask must
//! be one this adapter can answer safely:
//!
//! * the method is `GET` or `POST`;
//! * the URL is one origin the world's address policy admits, followed by
//!   `/xrpc/` and an NSID, and nothing that is not printable ASCII, at most
//!   [`URL_LIMIT`] bytes;
//! * a nonce, when one is given, is at most [`NONCE_LIMIT`] printable
//!   ASCII characters.
//!
//! Then, holding the credential's lock and renewing the session first when
//! it is due (or now, when the PDS did not take the token the previous
//! proof carried), the URL's origin must be the PDS the material names: an
//! access token is presented to that origin and nowhere else, so a proof
//! for any other one is refused, and a service whose connection names an
//! origin the account left is told so.
//!
//! The answer is the access token under the `DPoP` scheme, never `Bearer`,
//! and a proof made with the session's key: `htm` the method, `htu` the
//! URL without its query, `ath` the access token's hash, and the PDS's own
//! nonce, the one the service passed or else the last this adapter learned
//! for that key; a nonce the service passes is kept for the next proof.

use lobby_contracts::{Address, Authorization, Explained, MaterialSlot, Outcome, Secret};

use crate::codec::is_token;
use crate::material::Material;
use crate::proofs::{NOT_TAKEN, valid_method};
use crate::renewal::{Nonces, Renewer};

/// The longest URL a proof is made for, in bytes.
pub const URL_LIMIT: usize = 4096;
/// The longest nonce accepted.
pub const NONCE_LIMIT: usize = 1024;
/// The path every request to the PDS starts with.
pub const XRPC: &str = "/xrpc/";

/// What a service says when the account lives on another PDS than the
/// connection names.
pub const ELSEWHERE: &str = "The account lives on another server than this connection names, so the lobby sent it nothing. The sponsor revokes the connection on the manager page and opens it again.";

/// One request a service asks the adapter to prove.
#[derive(Debug, Clone, Copy)]
pub struct Asked<'a> {
    /// Its method.
    pub method: &'a str,
    /// Its whole URL.
    pub url: &'a str,
    /// The nonce the service last saw.
    pub nonce: Option<&'a str>,
    /// Whether the PDS did not take the token the previous proof carried.
    pub refused_before: bool,
}

/// The origin of `url` and its path, when `url` is a request this adapter
/// may prove under `policy`.
pub fn shape(asked: &Asked<'_>, policy: lobby_contracts::AddressPolicy) -> Option<Address> {
    if !matches!(asked.method, "GET" | "POST")
        || !is_token(asked.url, URL_LIMIT)
        || asked
            .nonce
            .is_some_and(|nonce| !is_token(nonce, NONCE_LIMIT))
    {
        return None;
    }
    let (scheme, rest) = asked.url.split_once("://")?;
    let slash = rest.find('/')?;
    let origin = Address::parse(&format!("{scheme}://{}", &rest[..slash]), policy)?;
    let path = &rest[slash..];
    if path.contains('#') {
        return None;
    }
    let method = path.strip_prefix(XRPC)?;
    let method = method.split('?').next().unwrap_or_default();
    // The URL must name the origin exactly as the parser writes it, so the
    // proof's `htu` is the URL the request goes to.
    let normalized = format!("{origin}{path}");
    (valid_method(method) && normalized == asked.url).then_some(origin)
}

/// Proves `asked` from the session in `slot`.
pub fn prove(
    renewer: &Renewer<'_>,
    nonces: &Nonces,
    slot: &dyn MaterialSlot,
    asked: &Asked<'_>,
) -> Result<Authorization, Explained> {
    let origin = shape(asked, renewer.policy).ok_or(Outcome::Unsupported)?;
    let lock = renewer.locks.of(slot.credential());
    let _held = lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if asked.refused_before {
        // The PDS stopped taking an access token before its time: renewing
        // once, due or not, is the session's own remedy.
        renewer
            .renew_now_held(slot)
            .map_err(|outcome| match outcome {
                Outcome::NeedsSignIn => Explained::new(Outcome::NeedsSignIn, NOT_TAKEN),
                other => other.into(),
            })?;
    } else {
        renewer.renew_held(slot)?;
    }
    let material = Material::decode(&slot.read()?, renewer.policy).ok_or(Outcome::NeedsSignIn)?;
    if material.pds != origin {
        return Err(Explained::new(Outcome::Refused, ELSEWHERE));
    }
    let thumbprint = material.key.thumbprint();
    let nonce = match asked.nonce {
        Some(nonce) => {
            nonces.keep(origin.as_str(), &thumbprint, nonce);
            Some(nonce.to_owned())
        }
        None => nonces.get(origin.as_str(), &thumbprint),
    };
    let proof = material.key.resource_proof(
        asked.method,
        asked.url,
        nonce.as_deref(),
        material.access.expose(),
    )?;
    Ok(Authorization {
        scheme: "DPoP".to_owned(),
        token: Secret::new(material.access.expose()),
        proof: Some(Secret::new(proof)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lobby_contracts::AddressPolicy;

    fn asked(method: &'static str, url: &'static str) -> Asked<'static> {
        Asked {
            method,
            url,
            nonce: None,
            refused_before: false,
        }
    }

    #[test]
    fn only_a_request_to_an_xrpc_method_at_one_origin_is_proved() {
        let policy = AddressPolicy::LoopbackAllowed;
        for good in [
            asked(
                "GET",
                "http://127.0.0.1:5300/xrpc/com.atproto.server.getSession",
            ),
            asked(
                "POST",
                "http://127.0.0.1:5300/xrpc/com.atproto.repo.createRecord",
            ),
            asked(
                "GET",
                "https://pds.example.test/xrpc/app.example.feed.read?limit=2&cursor=a%20b",
            ),
        ] {
            assert!(shape(&good, policy).is_some(), "{good:?}");
        }
        for bad in [
            asked("DELETE", "http://127.0.0.1:5300/xrpc/com.atproto.a.b"),
            asked("GET", "http://127.0.0.1:5300/api/com.atproto.a.b"),
            asked("GET", "http://127.0.0.1:5300/xrpc/nsid"),
            asked("GET", "http://127.0.0.1:5300/xrpc/a.b.c#frag"),
            asked("GET", "http://127.0.0.1:5300"),
            asked("GET", "http://user@127.0.0.1:5300/xrpc/a.b.c"),
            asked("GET", "HTTP://127.0.0.1:5300/xrpc/a.b.c"),
            asked("GET", "http://127.0.0.1:5300/xrpc/a.b.c?x=a b"),
            asked("GET", "http://10.0.0.1/xrpc/a.b.c"),
            asked("GET", "ftp://pds.example.test/xrpc/a.b.c"),
        ] {
            assert!(shape(&bad, policy).is_none(), "{bad:?}");
        }
        assert!(
            shape(
                &asked("GET", "http://127.0.0.1:5300/xrpc/a.b.c"),
                AddressPolicy::PublicHttps
            )
            .is_none(),
            "the world's policy decides which origins are reachable"
        );
        let with_nonce = Asked {
            nonce: Some("bad nonce"),
            ..asked("GET", "http://127.0.0.1:5300/xrpc/a.b.c")
        };
        assert!(shape(&with_nonce, policy).is_none());
    }
}
