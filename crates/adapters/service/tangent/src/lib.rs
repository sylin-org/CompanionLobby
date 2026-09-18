//! The Tangent service adapter: the ureq experience client exposed through the
//! connector's adapter contracts. The HTTP spoke itself lives in [`experience`] and
//! speaks the low-level `ExperiencePort`; this module maps it onto `ServiceAdapter`,
//! `SocialBrowsing` and `SocialModeration` without duplicating any wire behavior.

pub mod contract;
pub mod experience;

use serde_json::Value;

use companion_core::ports::{ExperienceError, ExperiencePort, RequestContext};
use companion_core::traits::{ServiceAdapter, SocialBrowsing, SocialModeration};

use experience::UreqExperience;

impl ServiceAdapter for UreqExperience {
    fn name(&self) -> &'static str { "tangent" }
    fn as_social_browsing(&self) -> Option<&dyn SocialBrowsing> { Some(self) }
    fn as_social_moderation(&self) -> Option<&dyn SocialModeration> { Some(self) }

    fn discover(&self, origin: &str, path: &str) -> Result<Value, ExperienceError> {
        ExperiencePort::discover(self, origin, path)
    }

    fn exchange(&self, origin: &str, path: &str, body: &Value, bearer: &str) -> Result<Value, ExperienceError> {
        ExperiencePort::exchange(self, origin, path, body, bearer)
    }

    fn server_card(&self, origin: &str) -> Result<Value, ExperienceError> {
        ExperiencePort::server_profile(self, origin)
    }

    fn probe_page(&self, origin: &str) -> Result<(), ExperienceError> {
        ExperiencePort::probe(self, origin)
    }
}

impl SocialBrowsing for UreqExperience {
    fn get(&self, context: &RequestContext, path: &str) -> Result<Value, ExperienceError> {
        ExperiencePort::get(self, context, path)
    }

    fn send(&self, context: &RequestContext, method: &str, path: &str, body: &Value) -> Result<Value, ExperienceError> {
        ExperiencePort::send(self, context, method, path, body)
    }
}

impl SocialModeration for UreqExperience {
    /// The stewardship surface rides the same credentialled experience API: case
    /// routes are topic- and case-scoped reads and writes, offered only by servers
    /// whose envelope advertises them.
    fn list_moderation_cases(&self, context: &RequestContext, topic: &str, page: Option<u16>) -> Result<Value, ExperienceError> {
        let path = match page {
            Some(page) => format!("/api/v1/experience/topics/{}/moderation/cases?page={page}", urlencoding::encode(topic)),
            None => format!("/api/v1/experience/topics/{}/moderation/cases", urlencoding::encode(topic)),
        };
        ExperiencePort::get(self, context, &path)
    }

    fn read_moderation_case(&self, context: &RequestContext, case_id: &str) -> Result<Value, ExperienceError> {
        let path = format!("/api/v1/experience/moderation/cases/{}", urlencoding::encode(case_id));
        ExperiencePort::get(self, context, &path)
    }

    fn preview_moderation_action(&self, context: &RequestContext, case_id: &str, body: &Value) -> Result<Value, ExperienceError> {
        let path = format!("/api/v1/experience/moderation/cases/{}/previews", urlencoding::encode(case_id));
        ExperiencePort::send(self, context, "POST", &path, body)
    }

    fn apply_moderation_action(&self, context: &RequestContext, case_id: &str, body: &Value) -> Result<Value, ExperienceError> {
        let path = format!("/api/v1/experience/moderation/cases/{}/actions", urlencoding::encode(case_id));
        ExperiencePort::send(self, context, "POST", &path, body)
    }
}
