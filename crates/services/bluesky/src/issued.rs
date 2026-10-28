//! What a connection keeps: the account as its PDS named it.
//!
//! Bluesky issues a connection nothing of its own: the account's records
//! live in its own repository on its PDS, and every request is proved with
//! the account's credential. What the connection keeps is the account's
//! DID, which names its repository, and the handle the network shows for
//! it, as the PDS answered when the connection was established. Neither is
//! a secret.

use lobby_contracts::SealedIssued;
use serde_json::{Value, json};

/// The kind written into what a connection keeps, so nothing another
/// adapter wrote is read as this.
const KIND: &str = "bluesky-account-v1";
/// The longest handle kept, in bytes.
pub const HANDLE_LIMIT: usize = 253;
/// The longest DID kept, in bytes.
pub const DID_LIMIT: usize = 2048;

/// The account a connection reaches Bluesky as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issued {
    /// The account's DID, which names its repository.
    pub did: String,
    /// The handle the network shows for it.
    pub handle: String,
}

/// Whether `did` is a did:plc or did:web DID.
pub fn valid_did(did: &str) -> bool {
    let body = did
        .strip_prefix("did:plc:")
        .or_else(|| did.strip_prefix("did:web:"));
    did.len() <= DID_LIMIT
        && body.is_some_and(|body| {
            !body.is_empty()
                && body
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._:%-".contains(&byte))
        })
}

/// Whether `handle` is a handle: dot-separated labels of letters, digits and
/// hyphens, at least two of them.
pub fn valid_handle(handle: &str) -> bool {
    let labels: Vec<&str> = handle.split('.').collect();
    handle.len() <= HANDLE_LIMIT
        && labels.len() >= 2
        && labels.iter().all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

impl Issued {
    /// The account `getSession` named, when it names one this adapter keeps.
    pub fn from_answer(answer: &Value) -> Option<Self> {
        let did = answer["did"].as_str().filter(|did| valid_did(did))?;
        let handle = answer["handle"]
            .as_str()
            .filter(|handle| valid_handle(handle))?;
        Some(Self {
            did: did.to_owned(),
            handle: handle.to_ascii_lowercase(),
        })
    }

    /// What the connection keeps.
    pub fn encode(&self) -> SealedIssued {
        SealedIssued::new(
            json!({ "kind": KIND, "did": self.did, "handle": self.handle })
                .to_string()
                .into_bytes(),
        )
    }

    /// What a connection kept, when this adapter wrote it.
    pub fn decode(sealed: &SealedIssued) -> Option<Self> {
        let value: Value = serde_json::from_slice(sealed.as_bytes()).ok()?;
        if value["kind"] != KIND || value.as_object().map(serde_json::Map::len) != Some(3) {
            return None;
        }
        Self::from_answer(&value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_a_connection_keeps_round_trips_and_nothing_else_is_read_as_it() {
        let issued = Issued::from_answer(&json!({
            "did": "did:plc:aaaaaaaaaaaaaaaaaaaaaaaa",
            "handle": "Ada.Example.Test",
            "accessJwt": "never kept",
        }))
        .expect("an answer");
        assert_eq!(issued.handle, "ada.example.test");
        let sealed = issued.encode();
        assert!(!String::from_utf8_lossy(sealed.as_bytes()).contains("never kept"));
        assert_eq!(Issued::decode(&sealed), Some(issued));
        for foreign in [
            &b"{}"[..],
            b"not json",
            br#"{"kind":"bluesky-account-v1","did":"did:key:z","handle":"a.test"}"#,
        ] {
            assert_eq!(Issued::decode(&SealedIssued::new(foreign.to_vec())), None);
        }
    }

    #[test]
    fn dids_and_handles_follow_their_shapes() {
        assert!(valid_did("did:web:localhost%3A5300"));
        assert!(!valid_did("did:key:zabc"));
        assert!(!valid_did("did:plc:"));
        assert!(valid_handle("ada.example.test"));
        for bad in ["ada", "ada..test", "-ada.test", "ada.test/x", ""] {
            assert!(!valid_handle(bad), "{bad}");
        }
    }
}
