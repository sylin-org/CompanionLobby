use serde_json::Value;

pub trait AuthAdapter: Send + Sync {
    fn name(&self) -> &'static str;
    fn authenticate(&self, params: &Value) -> Result<String, String>;
    fn refresh(&self, session_token: &str) -> Result<String, String>;
    
    /// Generates cryptographic headers for a specific request, if needed.
    fn sign_request(&self, auth_state: &str, method: &str, url: &str) -> Result<Value, String>;
    
    /// Requests a downstream service authentication token for a target audience.
    fn get_service_auth(&self, auth_state: &str, audience: &str) -> Result<String, String>;
}

pub trait ServiceAdapter: Send + Sync {
    fn name(&self) -> &'static str;
    fn as_social_browsing(&self) -> Option<&dyn SocialBrowsing> { None }
    fn as_social_moderation(&self) -> Option<&dyn SocialModeration> { None }
}

pub trait SocialBrowsing: Send + Sync {
    fn list_destinations(&self, destination_url: &str, auth_adapter: &dyn AuthAdapter, auth_state: &str, cursor: Option<&str>) -> Result<Value, String>;
    fn list_conversations(&self, destination_url: &str, auth_adapter: &dyn AuthAdapter, auth_state: &str, destination_id: &str, cursor: Option<&str>) -> Result<Value, String>;
    fn read_conversation(&self, destination_url: &str, auth_adapter: &dyn AuthAdapter, auth_state: &str, conversation_id: &str, cursor: Option<&str>) -> Result<Value, String>;
    fn publish(&self, destination_url: &str, auth_adapter: &dyn AuthAdapter, auth_state: &str, destination_id: &str, content: &str) -> Result<Value, String>;
    fn reply(&self, destination_url: &str, auth_adapter: &dyn AuthAdapter, auth_state: &str, conversation_id: &str, reply_to_id: &str, content: &str) -> Result<Value, String>;
    fn mark_read(&self, destination_url: &str, auth_adapter: &dyn AuthAdapter, auth_state: &str, conversation_id: &str, body: &Value) -> Result<Value, String>;
    fn join_destination(&self, destination_url: &str, auth_adapter: &dyn AuthAdapter, auth_state: &str, destination_id: &str, body: &Value) -> Result<Value, String>;
    fn leave_destination(&self, destination_url: &str, auth_adapter: &dyn AuthAdapter, auth_state: &str, destination_id: &str, request_id: &str) -> Result<Value, String>;
    fn set_watch(&self, destination_url: &str, auth_adapter: &dyn AuthAdapter, auth_state: &str, body: &Value) -> Result<Value, String>;
    fn get_profile(&self, destination_url: &str, auth_adapter: &dyn AuthAdapter, auth_state: &str) -> Result<Value, String>;
    fn get_operation(&self, destination_url: &str, auth_adapter: &dyn AuthAdapter, auth_state: &str, operation_id: &str) -> Result<Value, String>;
    fn probe(&self, destination_url: &str) -> Result<(), String>;
    fn server_profile(&self, destination_url: &str) -> Result<Value, String>;
}

pub trait SocialModeration: Send + Sync {
    fn list_moderation_cases(&self, destination_url: &str, auth_adapter: &dyn AuthAdapter, auth_state: &str, destination_id: &str, page: Option<u16>) -> Result<Value, String>;
    fn read_moderation_case(&self, destination_url: &str, auth_adapter: &dyn AuthAdapter, auth_state: &str, case_id: &str) -> Result<Value, String>;
    fn preview_moderation_action(&self, destination_url: &str, auth_adapter: &dyn AuthAdapter, auth_state: &str, case_id: &str, action: &str, summary: &str) -> Result<Value, String>;
    fn apply_moderation_action(&self, destination_url: &str, auth_adapter: &dyn AuthAdapter, auth_state: &str, case_id: &str, action: &str, summary: &str) -> Result<Value, String>;
}
