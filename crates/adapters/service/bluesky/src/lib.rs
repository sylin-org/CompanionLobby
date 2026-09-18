//! A deliberately unimplemented Bluesky service adapter. It exists so the connector's
//! adapter contracts are proven to fit a second service, not just the Tangent shape;
//! every operation answers honestly that it is not implemented. No crate depends on
//! it yet — wiring it into the hub is future work, tracked in the README.

use serde_json::Value;

use companion_core::ports::{ExperienceError, RequestContext};
use companion_core::traits::{ServiceAdapter, SocialBrowsing};

pub struct BlueskyClient {}

fn unimplemented(operation: &str) -> ExperienceError {
    ExperienceError::Transport(format!("Bluesky {operation} is not implemented"))
}

impl ServiceAdapter for BlueskyClient {
    fn name(&self) -> &'static str { "bluesky" }
    fn as_social_browsing(&self) -> Option<&dyn SocialBrowsing> { Some(self) }
    // Bluesky exposes no moderation surface to agents yet.
    fn as_social_moderation(&self) -> Option<&dyn companion_core::traits::SocialModeration> { None }
    fn discover(&self, _origin: &str, _path: &str) -> Result<Value, ExperienceError> { Err(unimplemented("discovery")) }
    fn exchange(&self, _origin: &str, _path: &str, _body: &Value, _bearer: &str) -> Result<Value, ExperienceError> { Err(unimplemented("exchange")) }
    fn server_card(&self, _origin: &str) -> Result<Value, ExperienceError> { Err(unimplemented("server cards")) }
    fn probe_page(&self, _origin: &str) -> Result<(), ExperienceError> { Err(unimplemented("page probe")) }
}

impl SocialBrowsing for BlueskyClient {
    fn get(&self, _context: &RequestContext, _path: &str) -> Result<Value, ExperienceError> { Err(unimplemented("reads")) }
    fn send(&self, _context: &RequestContext, _method: &str, _path: &str, _body: &Value) -> Result<Value, ExperienceError> { Err(unimplemented("writes")) }
}
