pub mod atproto_oauth;

use companion_core::traits::AuthAdapter;

impl AuthAdapter for atproto_oauth::AtprotoOauth {
    fn name(&self) -> &'static str { "atproto" }
    fn authenticate(&self, _params: &serde_json::Value) -> Result<String, String> { Err("not implemented".into()) }
    fn refresh(&self, _session_token: &str) -> Result<String, String> { Err("not implemented".into()) }
        fn sign_request(&self, auth_state: &str, method: &str, url: &str) -> Result<serde_json::Value, String> {
        let state: serde_json::Value = serde_json::from_str(auth_state).map_err(|e| format!("invalid auth_state: {e}"))?;
        let dpop_key = state.get("dpop_key").and_then(|v| v.as_str()).ok_or_else(|| "missing dpop_key".to_string())?;
        let access_token = state.get("access_jwt").and_then(|v| v.as_str()).ok_or_else(|| "missing access_jwt".to_string())?;
        // For Bluesky, we might not have a tracked nonce yet in this simple adapter interface, but we can pass None for now.
        let proof = self.resource_proof(dpop_key, method, url, access_token, None)?;
        Ok(serde_json::json!({
            "Authorization": format!("DPoP {}", access_token),
            "DPoP": proof
        }))
    }
        fn get_service_auth(&self, auth_state: &str, audience: &str) -> Result<String, String> {
        let state: serde_json::Value = serde_json::from_str(auth_state).map_err(|e| format!("invalid auth_state: {e}"))?;
        let pds = state.get("pds").and_then(|v| v.as_str()).ok_or_else(|| "missing pds".to_string())?;
        
        // Expiry 2 minutes from now for the token
        let exp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() + 120;
        
        let path = format!("/xrpc/com.atproto.server.getServiceAuth?aud={}&lxm=com.tangent.exchange&exp={}", urlencoding::encode(audience), exp);
        let url = format!("{}{}", pds, path);
        
        let headers = self.sign_request(auth_state, "GET", &url)?;
        let auth = headers.get("Authorization").and_then(|v| v.as_str()).unwrap();
        let dpop = headers.get("DPoP").and_then(|v| v.as_str()).unwrap();
        
        let response = self.agent.get(&url)
            .set("Authorization", auth)
            .set("DPoP", dpop)
            .call()
            .map_err(|e| e.to_string())?;
            
        let body: serde_json::Value = response.into_json().map_err(|e| e.to_string())?;
        let token = body.get("token").and_then(|v| v.as_str()).ok_or_else(|| "no token in response".to_string())?;
        Ok(token.to_string())
    }
}
