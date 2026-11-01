//! Who an account is and where it lives.
//!
//! A handle is resolved to a DID by the handle resolver, or, on the public
//! network, by the handle host's own `/.well-known/atproto-did`. A DID is
//! resolved to its document: a did:plc through the DID directory, a did:web
//! from its host's `/.well-known/did.json` (hostname-level did:web only, as
//! atproto allows). The document must name itself, and its `#atproto_pds`
//! service is the account's PDS, an origin the world may reach. The PDS's
//! protected-resource metadata names exactly one authorization server.

use lobby_contracts::{AccountProfile, Address, Outcome};
use lobby_outbound::{Outbound, Request};
use serde_json::Value;

use crate::codec::percent_encode;
use crate::wire::{World, failure_outcome, get_json, status_outcome};

/// The longest handle, in bytes.
const HANDLE_LIMIT: usize = 253;

/// What the sponsor typed, normalized: a handle or a DID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Named {
    /// A handle, lowercased, without a leading `@`.
    Handle(String),
    /// A DID typed as it is.
    Did(String),
}

/// Normalizes what the sponsor typed: trimmed, one leading `@` dropped, and
/// lowercased; a DID passes as a DID and must be one atproto supports.
pub fn named(input: &str) -> Option<Named> {
    let trimmed = input.trim();
    let text = trimmed
        .strip_prefix('@')
        .unwrap_or(trimmed)
        .to_ascii_lowercase();
    if text.starts_with("did:") {
        return valid_did(&text).then_some(Named::Did(text));
    }
    valid_handle(&text).then_some(Named::Handle(text))
}

/// A handle is a DNS name of at least two labels whose last label is not
/// all digits.
pub fn valid_handle(text: &str) -> bool {
    text.len() <= HANDLE_LIMIT
        && text.contains('.')
        && !text
            .rsplit('.')
            .next()
            .is_some_and(|last| last.bytes().all(|byte| byte.is_ascii_digit()))
        && text.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
}

/// A did:plc of 24 base32 characters, or a hostname-level did:web.
pub fn valid_did(text: &str) -> bool {
    if let Some(identifier) = text.strip_prefix("did:plc:") {
        return identifier.len() == 24
            && identifier
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || (b'2'..=b'7').contains(&byte));
    }
    text.strip_prefix("did:web:").is_some_and(valid_handle)
}

/// An account's document, as far as signing in needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    /// The account's PDS.
    pub pds: Address,
    /// The handles the document claims, from its `at://` aliases.
    pub handles: Vec<String>,
}

/// The DID `handle` names.
pub fn resolve_handle(world: &World, outbound: &Outbound, handle: &str) -> Result<String, Outcome> {
    let path = format!(
        "/xrpc/com.atproto.identity.resolveHandle?handle={}",
        percent_encode(handle)
    );
    match get_json(outbound, &world.resolver, &path) {
        Ok(answer) => {
            return answer["did"]
                .as_str()
                .filter(|did| valid_did(did))
                .map(str::to_owned)
                .ok_or(Outcome::Refused);
        }
        // The resolver says it knows no such handle; its own host may.
        Err(Outcome::Refused | Outcome::NotFound) if world.reads_handle_hosts() => {}
        Err(Outcome::Refused) => return Err(Outcome::NotFound),
        Err(other) => return Err(other),
    }
    let host = world
        .origin(&format!("https://{handle}"))
        .ok_or(Outcome::NotFound)?;
    let response = outbound
        .send(
            &host,
            Request::new("GET", "/.well-known/atproto-did").limit(1024),
        )
        .map_err(failure_outcome)?;
    if response.status != 200 {
        return Err(match status_outcome(response.status) {
            Outcome::Refused => Outcome::NotFound,
            other => other,
        });
    }
    let did = String::from_utf8_lossy(&response.body).trim().to_owned();
    valid_did(&did).then_some(did).ok_or(Outcome::NotFound)
}

/// The document of `did`, which must name `did` itself and a PDS the world
/// may reach.
pub fn document(world: &World, outbound: &Outbound, did: &str) -> Result<Document, Outcome> {
    if !valid_did(did) {
        return Err(Outcome::Refused);
    }
    let body = if let Some(host) = did.strip_prefix("did:web:") {
        let origin = match &world.web_documents {
            Some(origin) => origin.clone(),
            None => world
                .origin(&format!("https://{host}"))
                .ok_or(Outcome::Refused)?,
        };
        get_json(outbound, &origin, "/.well-known/did.json")?
    } else {
        get_json(outbound, &world.directory, &format!("/{did}"))?
    };
    if body["id"] != did {
        return Err(Outcome::Refused);
    }
    let pds = body["service"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|service| {
            service["id"]
                .as_str()
                .is_some_and(|id| id == "#atproto_pds" || id == format!("{did}#atproto_pds"))
                && service["type"] == "AtprotoPersonalDataServer"
        })
        .and_then(|service| service["serviceEndpoint"].as_str())
        .and_then(|endpoint| world.origin(endpoint))
        .ok_or(Outcome::Refused)?;
    let handles = body["alsoKnownAs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(|alias| alias.strip_prefix("at://"))
        .map(str::to_ascii_lowercase)
        .filter(|handle| valid_handle(handle))
        .collect();
    Ok(Document { pds, handles })
}

/// The one authorization server `pds` names in its protected-resource
/// metadata.
pub fn authorization_server(
    world: &World,
    outbound: &Outbound,
    pds: &Address,
) -> Result<Address, Outcome> {
    let metadata = get_json(outbound, pds, "/.well-known/oauth-protected-resource")?;
    match metadata["authorization_servers"]
        .as_array()
        .map(Vec::as_slice)
    {
        Some([only]) => only
            .as_str()
            .and_then(|origin| world.origin(origin))
            .ok_or(Outcome::Refused),
        _ => Err(Outcome::Refused),
    }
}

/// The largest picture read from a profile, in bytes: what the record's own
/// schema allows.
const AVATAR_LIMIT: usize = 1_000_000;

/// What the account `did` says of itself in the public profile record on its
/// `pds`. Reading it needs no grant, and whatever cannot be read is simply
/// not said: a profile only names a companion, so it never fails a sign-in.
pub fn profile(outbound: &Outbound, did: &str, pds: &Address) -> AccountProfile {
    let did = percent_encode(did);
    let path = format!(
        "/xrpc/com.atproto.repo.getRecord?repo={did}&collection=app.bsky.actor.profile&rkey=self"
    );
    let Ok(record) = get_json(outbound, pds, &path) else {
        return AccountProfile::default();
    };
    let value = &record["value"];
    let portrait = value["avatar"]["ref"]["$link"].as_str().and_then(|cid| {
        let path = format!(
            "/xrpc/com.atproto.sync.getBlob?did={did}&cid={}",
            percent_encode(cid)
        );
        outbound
            .send(pds, Request::new("GET", &path).limit(AVATAR_LIMIT))
            .ok()
            .filter(|answer| answer.status == 200)
            .map(|answer| answer.body)
    });
    AccountProfile {
        display_name: value["displayName"].as_str().map(str::to_owned),
        portrait,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_the_sponsor_types_is_normalized_strictly() {
        assert_eq!(
            named("  @Ada.Example.COM "),
            Some(Named::Handle("ada.example.com".to_owned()))
        );
        assert_eq!(
            named("did:plc:aaaaaaaaaaaaaaaaaaaaaaaa"),
            Some(Named::Did("did:plc:aaaaaaaaaaaaaaaaaaaaaaaa".to_owned()))
        );
        for bad in [
            "",
            "ada",
            "@@ada.example.com",
            "ada@example.com",
            "ada.example.com/x",
            "ada.example.com:443",
            "ada..example.com",
            "-ada.example.com",
            "ada.example.123",
            "127.0.0.1",
            "did:plc:short",
            "did:key:zabc",
        ] {
            assert_eq!(named(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn did_web_is_hostname_only_and_did_plc_is_24_base32_characters() {
        assert!(valid_did("did:web:ada.example.com"));
        assert!(valid_did("did:plc:abcdefghijklmnopqrstuvwx"));
        for bad in [
            "did:web:ada.example.com:8443",
            "did:web:ada.example.com%3A8443",
            "did:web:localhost",
            "did:web:10.0.0.1",
            "did:plc:abcdefghijklmnopqrstuvw1",
            "did:plc:ABCDEFGHIJKLMNOPQRSTUVWX",
            "did:plc:aaaaaaaaaaaaaaaaaaaaaaaaa",
        ] {
            assert!(!valid_did(bad), "{bad}");
        }
    }
}
