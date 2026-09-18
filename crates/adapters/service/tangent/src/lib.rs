pub mod experience;
pub mod contract;

use companion_core::traits::{ServiceAdapter, SocialBrowsing, SocialModeration};

impl ServiceAdapter for experience::UreqExperience {
    fn name(&self) -> &'static str { "tangent" }
    fn as_social_browsing(&self) -> Option<&dyn SocialBrowsing> { Some(self) }
    fn as_social_moderation(&self) -> Option<&dyn SocialModeration> { Some(self) }
}

impl SocialBrowsing for experience::UreqExperience {
        fn list_destinations(&self, destination_url: &str, auth_adapter: &dyn companion_core::traits::AuthAdapter, auth_state: &str, cursor: Option<&str>) -> Result<serde_json::Value, String> {
        let mut path = format!("{destination_url}/api/v1/experience/tangents");
        if let Some(c) = cursor {
            path.push_str(&format!("?cursor={}", urlencoding::encode(c)));
        }
        let token = auth_adapter.get_service_auth(auth_state, destination_url)?;
        let response = self.agent.get(&path).set("Authorization", &format!("Bearer {token}")).call().map_err(|e| e.to_string())?;
        response.into_json().map_err(|e| e.to_string())
    }
        fn list_conversations(&self, destination_url: &str, auth_adapter: &dyn companion_core::traits::AuthAdapter, auth_state: &str, destination_id: &str, cursor: Option<&str>) -> Result<serde_json::Value, String> {
        let mut path = format!("{destination_url}/api/v1/experience/tangents/{destination_id}/topics");
        if let Some(c) = cursor {
            path.push_str(&format!("?cursor={}", urlencoding::encode(c)));
        }
        let token = auth_adapter.get_service_auth(auth_state, destination_url)?;
        let response = self.agent.get(&path).set("Authorization", &format!("Bearer {token}")).call().map_err(|e| e.to_string())?;
        response.into_json().map_err(|e| e.to_string())
    }
        fn read_conversation(&self, destination_url: &str, auth_adapter: &dyn companion_core::traits::AuthAdapter, auth_state: &str, conversation_id: &str, cursor: Option<&str>) -> Result<serde_json::Value, String> {
        let mut path = format!("{destination_url}/api/v1/experience/topics/{conversation_id}");
        if let Some(c) = cursor {
            path.push_str(&format!("?cursor={}", urlencoding::encode(c)));
        }
        
        let token = auth_adapter.get_service_auth(auth_state, destination_url)?;
        let response = self.agent.get(&path).set("Authorization", &format!("Bearer {token}")).call().map_err(|e| e.to_string())?;
        response.into_json().map_err(|e| e.to_string())
    }
    fn publish(&self, _destination_url: &str, _auth_adapter: &dyn companion_core::traits::AuthAdapter, _auth_token: &str, _destination_id: &str, _content: &str) -> Result<serde_json::Value, String> { Err("not implemented".into()) }
    fn reply(&self, _destination_url: &str, _auth_adapter: &dyn companion_core::traits::AuthAdapter, _auth_token: &str, _conversation_id: &str, _reply_to_id: &str, _content: &str) -> Result<serde_json::Value, String> { Err("not implemented".into()) }
    fn mark_read(&self, _destination_url: &str, _auth_adapter: &dyn companion_core::traits::AuthAdapter, _auth_state: &str, _conversation_id: &str, _body: &serde_json::Value) -> Result<serde_json::Value, String> { Err("not implemented".into()) }
    fn join_destination(&self, _destination_url: &str, _auth_adapter: &dyn companion_core::traits::AuthAdapter, _auth_state: &str, _destination_id: &str, _body: &serde_json::Value) -> Result<serde_json::Value, String> { Err("not implemented".into()) }
    fn leave_destination(&self, _destination_url: &str, _auth_adapter: &dyn companion_core::traits::AuthAdapter, _auth_state: &str, _destination_id: &str, _request_id: &str) -> Result<serde_json::Value, String> { Err("not implemented".into()) }
    fn set_watch(&self, _destination_url: &str, _auth_adapter: &dyn companion_core::traits::AuthAdapter, _auth_state: &str, _body: &serde_json::Value) -> Result<serde_json::Value, String> { Err("not implemented".into()) }
    fn get_profile(&self, destination_url: &str, auth_adapter: &dyn companion_core::traits::AuthAdapter, auth_state: &str) -> Result<serde_json::Value, String> {
        let path = format!("{destination_url}/api/v1/experience");
        let token = auth_adapter.get_service_auth(auth_state, destination_url)?;
        let response = self.agent.get(&path).set("Authorization", &format!("Bearer {token}")).call().map_err(|e| e.to_string())?;
        response.into_json().map_err(|e| e.to_string())
    }
    
    fn get_operation(&self, destination_url: &str, auth_adapter: &dyn companion_core::traits::AuthAdapter, auth_state: &str, operation_id: &str) -> Result<serde_json::Value, String> {
        let path = format!("{destination_url}/api/v1/experience/operations/{}", urlencoding::encode(operation_id));
        let token = auth_adapter.get_service_auth(auth_state, destination_url)?;
        let response = self.agent.get(&path).set("Authorization", &format!("Bearer {token}")).call().map_err(|e| e.to_string())?;
        response.into_json().map_err(|e| e.to_string())
    }
    
    fn probe(&self, destination_url: &str) -> Result<(), String> {
        // Just checking if it responds to discovery
        let path = format!("{destination_url}/.well-known/mcp-companion");
        self.agent.get(&path).call().map_err(|e| e.to_string())?;
        Ok(())
    }
    
    fn server_profile(&self, destination_url: &str) -> Result<serde_json::Value, String> {
        let path = format!("{destination_url}/.well-known/mcp-companion");
        let response = self.agent.get(&path).call().map_err(|e| e.to_string())?;
        response.into_json().map_err(|e| e.to_string())
    }
}

impl SocialModeration for experience::UreqExperience {
    fn list_moderation_cases(&self, _destination_url: &str, _auth_adapter: &dyn companion_core::traits::AuthAdapter, _auth_token: &str, _destination_id: &str, _page: Option<u16>) -> Result<serde_json::Value, String> { Err("not implemented".into()) }
    fn read_moderation_case(&self, _destination_url: &str, _auth_adapter: &dyn companion_core::traits::AuthAdapter, _auth_token: &str, _case_id: &str) -> Result<serde_json::Value, String> { Err("not implemented".into()) }
    fn preview_moderation_action(&self, _destination_url: &str, _auth_adapter: &dyn companion_core::traits::AuthAdapter, _auth_token: &str, _case_id: &str, _action: &str, _summary: &str) -> Result<serde_json::Value, String> { Err("not implemented".into()) }
    fn apply_moderation_action(&self, _destination_url: &str, _auth_adapter: &dyn companion_core::traits::AuthAdapter, _auth_token: &str, _case_id: &str, _action: &str, _summary: &str) -> Result<serde_json::Value, String> { Err("not implemented".into()) }
}
