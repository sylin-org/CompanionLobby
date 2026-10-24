//! The material of an atproto credential.
//!
//! The material is one JSON object tagged [`FORMAT`], which only this adapter
//! reads: the account's DID (`subject`), the DPoP-bound access token and the
//! refresh token, the account's PDS and authorization server, the client id
//! the OAuth session was granted under (a refresh must present it again),
//! the session's DPoP key and the permissions granted. The daemon keeps it
//! sealed beside the credential and never decodes it; nothing in it reaches
//! a record, a projection, the journal or a log.

use lobby_contracts::{Address, SealedMaterial, Secret};
use serde_json::json;

use crate::dpop::DpopKey;

/// The format tag of this adapter's material.
pub const FORMAT: &str = "atproto-oauth-1";

/// One OAuth session's material.
pub struct Material {
    pub(crate) subject: String,
    pub(crate) access: Secret,
    pub(crate) refresh: Secret,
    pub(crate) pds: Address,
    pub(crate) authserver: Address,
    pub(crate) client_id: String,
    pub(crate) key: DpopKey,
    pub(crate) permissions: Vec<String>,
}

impl Material {
    /// The sealed form the daemon keeps.
    pub fn encode(&self) -> SealedMaterial {
        let body = json!({
            "format": FORMAT,
            "subject": self.subject,
            "access": self.access.expose(),
            "refresh": self.refresh.expose(),
            "pds": self.pds.as_str(),
            "authserver": self.authserver.as_str(),
            "clientId": self.client_id,
            "dpopKey": self.key.encode(),
            "permissions": self.permissions,
        });
        SealedMaterial::new(body.to_string().into_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lobby_contracts::AddressPolicy;
    use serde_json::Value;

    #[test]
    fn the_material_is_tagged_and_keeps_everything_a_refresh_needs() {
        let key = DpopKey::generate().expect("a key");
        let encoded_key = key.encode();
        let origin = |text: &str| Address::parse(text, AddressPolicy::PublicHttps).expect("origin");
        let material = Material {
            subject: "did:plc:aaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            access: Secret::new("access-1"),
            refresh: Secret::new("refresh-1"),
            pds: origin("https://pds.test"),
            authserver: origin("https://as.test"),
            client_id: "http://localhost?scope=atproto".to_owned(),
            key,
            permissions: vec!["atproto".to_owned()],
        }
        .encode();
        assert_eq!(
            format!("{material:?}"),
            format!("SealedMaterial({} bytes)", material.as_bytes().len())
        );
        let body: Value = serde_json::from_slice(material.as_bytes()).expect("JSON");
        assert_eq!(body["format"], FORMAT);
        assert_eq!(body["subject"], "did:plc:aaaaaaaaaaaaaaaaaaaaaaaa");
        assert_eq!(body["refresh"], "refresh-1");
        assert_eq!(body["authserver"], "https://as.test");
        assert_eq!(body["clientId"], "http://localhost?scope=atproto");
        assert_eq!(body["permissions"], json!(["atproto"]));
        let kept = DpopKey::decode(body["dpopKey"].as_str().expect("key")).expect("a key");
        assert_eq!(kept.encode(), encoded_key);
    }
}
