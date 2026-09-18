//! The atproto auth adapter: one job — minting the PDS service-auth proof of the
//! bound enrollment exchange through the connector's `AuthAdapter` contract. The
//! proof's audience comes from the server's discovery document, its parameters are
//! percent-encoded into the query, and it is consumed by the exchange in the same
//! enrollment, never stored or logged.

pub mod atproto_oauth;

use companion_core::ports::ExperienceError;
use companion_core::traits::AuthAdapter;

use crate::atproto_oauth::{AtprotoOauth, EXCHANGE_LXM};

/// The PDS-issued proof lives just long enough for the exchange that consumes it.
const PROOF_EXPIRY_SECS: i64 = 120;

/// One attempt of the getServiceAuth mint: a DPoP-proved GET when the session carries
/// a key (OAuth sessions do), a plain Bearer GET otherwise (app-password shape).
struct MintRequest<'a> {
    pds: &'a str,
    access_jwt: &'a str,
    dpop_key: Option<&'a str>,
}

impl AuthAdapter for AtprotoOauth {
    fn name(&self) -> &'static str { "atproto" }

    fn get_service_auth(&self, auth_state: &str, audience: &str) -> Result<String, ExperienceError> {
        let state: serde_json::Value = serde_json::from_str(auth_state)
            .map_err(|error| ExperienceError::Transport(format!("invalid atproto session state: {error}")))?;
        let field = |name: &str| state.get(name).and_then(serde_json::Value::as_str).map(str::to_string);
        let pds = field("pds").ok_or_else(|| ExperienceError::Transport("the atproto session names no PDS".into()))?;
        let access_jwt = field("access_jwt")
            .ok_or_else(|| ExperienceError::Transport("the atproto session carries no access token".into()))?;
        let dpop_key = field("dpop_key");
        let request = MintRequest { pds: &pds, access_jwt: &access_jwt, dpop_key: dpop_key.as_deref() };
        if request.pds.starts_with("http://") && !request.pds.contains("127.0.0.1") && !request.pds.contains("localhost") {
            return Err(ExperienceError::Transport("the PDS origin must be HTTPS (loopback excepted)".into()));
        }
        let url = format!(
            "{}/xrpc/com.atproto.server.getServiceAuth?aud={}&lxm={}&exp={}",
            request.pds.trim_end_matches('/'),
            urlencoding::encode(audience),
            urlencoding::encode(EXCHANGE_LXM),
            now_secs() + PROOF_EXPIRY_SECS,
        );
        // The proof binds this exact method and URI without its query (RFC 9449 §4.1);
        // the resource server's own nonce rides the retry, never the authorization
        // server's (§8: each server has its own nonce context).
        let htu = format!("{}/xrpc/com.atproto.server.getServiceAuth", request.pds.trim_end_matches('/'));
        // A cached nonce saves the challenge round trip; the first ask may carry none.
        let cached = self.resource_nonce(&htu);
        match self.mint(&request, &url, &htu, cached.as_deref()) {
            Err(ExperienceError::DpopChallenge { nonce }) => {
                self.remember_resource_nonce(&htu, &nonce);
                self.mint(&request, &url, &htu, Some(&nonce))
            }
            outcome => outcome,
        }
    }
}

impl AtprotoOauth {
    /// One mint attempt. A DPoP-proved 401 carrying a `DPoP-Nonce` header is the
    /// RFC 9449 §8 challenge (surfaced once for the caller's single retry); every
    /// other refusal is a settled error.
    fn mint(&self, request: &MintRequest<'_>, url: &str, htu: &str, nonce: Option<&str>) -> Result<String, ExperienceError> {
        let mut call = self.agent.get(url);
        call = match request.dpop_key {
            Some(key) => {
                let proof = self
                    .resource_proof(key, "GET", htu, request.access_jwt, nonce)
                    .map_err(|error| ExperienceError::Transport(format!("cannot prove the request: {error}")))?;
                call.set("Authorization", &format!("DPoP {}", request.access_jwt))
                    .set("DPoP", &proof)
            }
            None => call.set("Authorization", &format!("Bearer {}", request.access_jwt)),
        };
        match call.call() {
            Ok(response) => {
                let body: serde_json::Value = response
                    .into_json()
                    .map_err(|error| ExperienceError::Transport(format!("malformed getServiceAuth response: {error}")))?;
                let token = body.get("token").and_then(serde_json::Value::as_str).unwrap_or_default();
                if token.is_empty() {
                    return Err(ExperienceError::Transport("the PDS returned no service-auth token".into()));
                }
                Ok(token.to_string())
            }
            Err(ureq::Error::Status(status, response)) => {
                if status == 401 && request.dpop_key.is_some() {
                    if let Some(fresh) = response.header("DPoP-Nonce") {
                        return Err(ExperienceError::DpopChallenge { nonce: fresh.to_string() });
                    }
                }
                let problem = response
                    .into_json::<serde_json::Value>()
                    .ok()
                    .and_then(|value| {
                        let code = value.get("error").or_else(|| value.get("code"))?.as_str()?.to_string();
                        let message = value.get("message").or_else(|| value.get("error_description")).and_then(|v| v.as_str()).unwrap_or_default().to_string();
                        Some((code, message))
                    });
                Err(match problem {
                    Some((code, message)) => ExperienceError::Application { code, message },
                    None if status == 401 => ExperienceError::Unauthorized,
                    None => ExperienceError::Application {
                        code: format!("http_{status}"),
                        message: format!("the PDS rejected the request with HTTP {status}"),
                    },
                })
            }
            Err(ureq::Error::Transport(_)) => Err(ExperienceError::Unreachable),
        }
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
