//! The material of an atproto credential.
//!
//! The material is one JSON object tagged [`FORMAT`], which only this adapter
//! reads: the account's DID (`subject`), the DPoP-bound access token and the
//! moment it expires, the refresh token, the account's PDS and authorization
//! server, the client id the OAuth session was granted under (a refresh must
//! present it again), the session's DPoP key and the permissions granted.
//! The daemon keeps it sealed beside the credential and never decodes it;
//! nothing in it reaches a record, a projection, the journal or a log.

use lobby_contracts::{Address, AddressPolicy, SealedMaterial, Secret};
use serde_json::{Value, json};

use crate::client::CLIENT_ORIGIN;
use crate::codec::is_token;
use crate::dpop::DpopKey;
use crate::identity;
use crate::tokens::TOKEN_LIMIT;

/// The format tag of this adapter's material.
pub const FORMAT: &str = "atproto-oauth-1";

/// The longest client id kept.
const CLIENT_ID_LIMIT: usize = 2048;

/// One OAuth session's material.
pub struct Material {
    pub(crate) subject: String,
    pub(crate) access: Secret,
    /// When the access token expires, in seconds since the Unix epoch.
    pub(crate) access_expires_at: i64,
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
            "accessExpiresAt": self.access_expires_at,
            "refresh": self.refresh.expose(),
            "pds": self.pds.as_str(),
            "authserver": self.authserver.as_str(),
            "clientId": self.client_id,
            "dpopKey": self.key.encode(),
            "permissions": self.permissions,
        });
        SealedMaterial::new(body.to_string().into_bytes())
    }

    /// The material this adapter encoded, with every field in its own shape
    /// and the origins ones `policy` lets the adapter reach; anything else,
    /// whoever wrote it, is not this adapter's.
    pub fn decode(sealed: &SealedMaterial, policy: AddressPolicy) -> Option<Self> {
        let body: Value = serde_json::from_slice(sealed.as_bytes()).ok()?;
        if body["format"] != FORMAT {
            return None;
        }
        let text = |field: &str| body[field].as_str();
        let token = |field: &str| {
            text(field)
                .filter(|token| is_token(token, TOKEN_LIMIT))
                .map(Secret::new)
        };
        let origin = |field: &str| {
            let raw = text(field)?;
            Address::parse(raw, policy).filter(|address| address.as_str() == raw)
        };
        let subject = text("subject")
            .filter(|subject| identity::valid_did(subject))?
            .to_owned();
        let client_id = text("clientId")
            .filter(|id| id.len() <= CLIENT_ID_LIMIT && id.starts_with(CLIENT_ORIGIN))?
            .to_owned();
        let permissions = body["permissions"]
            .as_array()?
            .iter()
            .map(|permission| permission.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            subject,
            access: token("access")?,
            access_expires_at: body["accessExpiresAt"].as_i64()?,
            refresh: token("refresh")?,
            pds: origin("pds")?,
            authserver: origin("authserver")?,
            client_id,
            key: DpopKey::decode(text("dpopKey")?)?,
            permissions,
        })
    }

    /// The permissions as the scope a refresh may grant at most.
    pub(crate) fn scope(&self) -> String {
        self.permissions.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin(text: &str) -> Address {
        Address::parse(text, AddressPolicy::PublicHttps).expect("origin")
    }

    fn sample(key: DpopKey) -> Material {
        Material {
            subject: "did:plc:aaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            access: Secret::new("access-1"),
            access_expires_at: 1_700_000_000,
            refresh: Secret::new("refresh-1"),
            pds: origin("https://pds.test"),
            authserver: origin("https://as.test"),
            client_id: "http://localhost?scope=atproto".to_owned(),
            key,
            permissions: vec!["atproto".to_owned()],
        }
    }

    #[test]
    fn the_material_is_tagged_and_keeps_everything_a_refresh_needs() {
        let key = DpopKey::generate().expect("a key");
        let encoded_key = key.encode();
        let material = sample(key).encode();
        assert_eq!(
            format!("{material:?}"),
            format!("SealedMaterial({} bytes)", material.as_bytes().len())
        );
        let body: Value = serde_json::from_slice(material.as_bytes()).expect("JSON");
        assert_eq!(body["format"], FORMAT);
        assert_eq!(body["subject"], "did:plc:aaaaaaaaaaaaaaaaaaaaaaaa");
        assert_eq!(body["refresh"], "refresh-1");
        assert_eq!(body["accessExpiresAt"], 1_700_000_000);
        assert_eq!(body["authserver"], "https://as.test");
        assert_eq!(body["clientId"], "http://localhost?scope=atproto");
        assert_eq!(body["permissions"], json!(["atproto"]));
        let kept = DpopKey::decode(body["dpopKey"].as_str().expect("key")).expect("a key");
        assert_eq!(kept.encode(), encoded_key);

        let decoded = Material::decode(&material, AddressPolicy::PublicHttps).expect("ours");
        assert_eq!(decoded.subject, "did:plc:aaaaaaaaaaaaaaaaaaaaaaaa");
        assert_eq!(decoded.refresh.expose(), "refresh-1");
        assert_eq!(decoded.access_expires_at, 1_700_000_000);
        assert_eq!(decoded.key.encode(), encoded_key);
        assert_eq!(decoded.scope(), "atproto");
    }

    #[test]
    fn material_that_is_not_ours_or_breaks_a_rule_is_refused() {
        let material = sample(DpopKey::generate().expect("a key")).encode();
        let body: Value = serde_json::from_slice(material.as_bytes()).expect("JSON");
        for (field, value) in [
            ("format", json!("passbook-1")),
            ("subject", json!("ada.example.test")),
            ("refresh", json!("")),
            ("refresh", Value::Null),
            ("accessExpiresAt", json!("soon")),
            ("pds", json!("http://127.0.0.1:5300")),
            ("authserver", json!("https://as.test/path")),
            ("clientId", json!("https://client.test")),
            ("dpopKey", json!("not a key")),
            ("permissions", json!(["atproto", 7])),
        ] {
            let mut flawed = body.clone();
            flawed[field] = value;
            let sealed = SealedMaterial::new(flawed.to_string().into_bytes());
            assert!(
                Material::decode(&sealed, AddressPolicy::PublicHttps).is_none(),
                "{field}"
            );
        }
        for foreign in [&b""[..], b"{}", b"not json"] {
            assert!(
                Material::decode(
                    &SealedMaterial::new(foreign.to_vec()),
                    AddressPolicy::PublicHttps
                )
                .is_none()
            );
        }
    }
}
