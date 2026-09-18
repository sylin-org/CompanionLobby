use companion_core::traits::{ServiceAdapter, SocialBrowsing};

pub struct BlueskyClient {}

impl ServiceAdapter for BlueskyClient {
    fn name(&self) -> &'static str { "bluesky" }
    fn as_social_browsing(&self) -> Option<&dyn SocialBrowsing> { Some(self) }
    fn as_social_moderation(&self) -> Option<&dyn companion_core::traits::SocialModeration> { None } // Bluesky doesn't expose moderation API to agents yet
}

impl SocialBrowsing for BlueskyClient {
    fn list_destinations(&self, _destination_url: &str, _auth_adapter: &dyn companion_core::traits::AuthAdapter, _auth_token: &str, _cursor: Option<&str>) -> Result<serde_json::Value, String> { Err("Bluesky listing not implemented".into()) }
    fn list_conversations(&self, _destination_url: &str, _auth_adapter: &dyn companion_core::traits::AuthAdapter, _auth_token: &str, _destination_id: &str, _cursor: Option<&str>) -> Result<serde_json::Value, String> { Err("Bluesky conversations not implemented".into()) }
    fn read_conversation(&self, _destination_url: &str, _auth_adapter: &dyn companion_core::traits::AuthAdapter, _auth_token: &str, _conversation_id: &str, _cursor: Option<&str>) -> Result<serde_json::Value, String> { Err("Bluesky read not implemented".into()) }
    fn publish(&self, _destination_url: &str, _auth_adapter: &dyn companion_core::traits::AuthAdapter, _auth_token: &str, _destination_id: &str, _content: &str) -> Result<serde_json::Value, String> { Err("Bluesky publish not implemented".into()) }
    fn reply(&self, _destination_url: &str, _auth_adapter: &dyn companion_core::traits::AuthAdapter, _auth_token: &str, _conversation_id: &str, _reply_to_id: &str, _content: &str) -> Result<serde_json::Value, String> { Err("Bluesky reply not implemented".into()) }
    fn mark_read(&self, _destination_url: &str, _auth_adapter: &dyn companion_core::traits::AuthAdapter, _auth_state: &str, _conversation_id: &str, _body: &serde_json::Value) -> Result<serde_json::Value, String> { Err("not implemented".into()) }
    fn join_destination(&self, _destination_url: &str, _auth_adapter: &dyn companion_core::traits::AuthAdapter, _auth_state: &str, _destination_id: &str, _body: &serde_json::Value) -> Result<serde_json::Value, String> { Err("not implemented".into()) }
    fn leave_destination(&self, _destination_url: &str, _auth_adapter: &dyn companion_core::traits::AuthAdapter, _auth_state: &str, _destination_id: &str, _request_id: &str) -> Result<serde_json::Value, String> { Err("not implemented".into()) }
    fn set_watch(&self, _destination_url: &str, _auth_adapter: &dyn companion_core::traits::AuthAdapter, _auth_state: &str, _body: &serde_json::Value) -> Result<serde_json::Value, String> { Err("not implemented".into()) }
    fn get_profile(&self, _destination_url: &str, _auth_adapter: &dyn companion_core::traits::AuthAdapter, _auth_state: &str) -> Result<serde_json::Value, String> { Err("not implemented".into()) }
    fn get_operation(&self, _destination_url: &str, _auth_adapter: &dyn companion_core::traits::AuthAdapter, _auth_state: &str, _operation_id: &str) -> Result<serde_json::Value, String> { Err("not implemented".into()) }
    fn probe(&self, _destination_url: &str) -> Result<(), String> { Err("not implemented".into()) }
    fn server_profile(&self, _destination_url: &str) -> Result<serde_json::Value, String> { Err("not implemented".into()) }
}
