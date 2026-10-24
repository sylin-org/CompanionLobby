//! The authorization server's metadata, checked against the profile.
//!
//! The adapter signs in only with a server whose metadata document, at
//! `/.well-known/oauth-authorization-server`, names itself as the issuer and
//! promises everything the sign-in uses: pushed authorization requests,
//! required; the authorization code grant with PKCE S256; public clients
//! (`none`); DPoP with ES256; the `atproto` scope; the `iss` parameter on
//! the response; and client id metadata documents, which the loopback
//! client id stands in for. Its authorize, PAR and token endpoints must live
//! on its own origin. A server missing any of it is refused before anything
//! is pushed.

use lobby_contracts::{Address, Outcome};
use lobby_outbound::Outbound;
use serde_json::Value;

use crate::wire::{Endpoint, get_json};

/// The endpoints a sign-in uses on one authorization server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    /// Where the browser goes.
    pub authorize: Endpoint,
    /// Where the request is pushed.
    pub pushed: Endpoint,
    /// Where the code is exchanged.
    pub token: Endpoint,
}

/// The endpoints of `issuer`, when its metadata satisfies the profile.
pub fn endpoints(outbound: &Outbound, issuer: &Address) -> Result<Endpoints, Outcome> {
    let metadata = get_json(outbound, issuer, "/.well-known/oauth-authorization-server")?;
    checked(&metadata, issuer).ok_or(Outcome::Unsupported)
}

fn checked(metadata: &Value, issuer: &Address) -> Option<Endpoints> {
    let lists = |field: &str, expected: &str| {
        metadata[field]
            .as_array()
            .is_some_and(|values| values.iter().any(|value| value == expected))
    };
    let promises = |field: &str| metadata[field] == true;
    let endpoint = |field: &str| {
        metadata[field]
            .as_str()
            .and_then(|url| Endpoint::on(issuer, url))
    };
    let satisfied = metadata["issuer"] == issuer.as_str()
        && promises("require_pushed_authorization_requests")
        && lists("response_types_supported", "code")
        && lists("grant_types_supported", "authorization_code")
        && lists("code_challenge_methods_supported", "S256")
        && lists("token_endpoint_auth_methods_supported", "none")
        && lists("dpop_signing_alg_values_supported", "ES256")
        && lists("scopes_supported", "atproto")
        && promises("authorization_response_iss_parameter_supported")
        && promises("client_id_metadata_document_supported");
    if !satisfied {
        return None;
    }
    Some(Endpoints {
        authorize: endpoint("authorization_endpoint")?,
        pushed: endpoint("pushed_authorization_request_endpoint")?,
        token: endpoint("token_endpoint")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lobby_contracts::AddressPolicy;
    use serde_json::json;

    fn issuer() -> Address {
        Address::parse("https://as.test", AddressPolicy::PublicHttps).expect("issuer")
    }

    fn complete() -> Value {
        json!({
            "issuer": "https://as.test",
            "authorization_endpoint": "https://as.test/oauth/authorize",
            "token_endpoint": "https://as.test/oauth/token",
            "pushed_authorization_request_endpoint": "https://as.test/oauth/par",
            "require_pushed_authorization_requests": true,
            "response_types_supported": ["code"],
            "grant_types_supported": ["authorization_code", "refresh_token"],
            "code_challenge_methods_supported": ["S256"],
            "token_endpoint_auth_methods_supported": ["none", "private_key_jwt"],
            "dpop_signing_alg_values_supported": ["ES256"],
            "scopes_supported": ["atproto", "transition:generic"],
            "authorization_response_iss_parameter_supported": true,
            "client_id_metadata_document_supported": true,
        })
    }

    #[test]
    fn a_server_that_keeps_every_promise_is_used() {
        let endpoints = checked(&complete(), &issuer()).expect("satisfied");
        assert_eq!(endpoints.pushed.path, "/oauth/par");
        assert_eq!(endpoints.token.path, "/oauth/token");
        assert_eq!(endpoints.authorize.path, "/oauth/authorize");
    }

    #[test]
    fn a_server_missing_any_promise_is_refused() {
        let broken: Vec<(&str, Value)> = vec![
            ("issuer", json!("https://other.test")),
            ("pushed_authorization_request_endpoint", Value::Null),
            ("require_pushed_authorization_requests", json!(false)),
            ("code_challenge_methods_supported", json!(["plain"])),
            ("dpop_signing_alg_values_supported", json!(["RS256"])),
            ("scopes_supported", json!(["openid"])),
            (
                "authorization_response_iss_parameter_supported",
                Value::Null,
            ),
            ("client_id_metadata_document_supported", json!(false)),
            (
                "token_endpoint_auth_methods_supported",
                json!(["private_key_jwt"]),
            ),
            (
                "token_endpoint",
                json!("https://elsewhere.test/oauth/token"),
            ),
            (
                "authorization_endpoint",
                json!("https://as.test.evil/oauth/authorize"),
            ),
        ];
        for (field, value) in broken {
            let mut metadata = complete();
            metadata[field] = value;
            assert_eq!(checked(&metadata, &issuer()), None, "{field}");
        }
    }
}
