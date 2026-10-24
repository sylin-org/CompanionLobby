//! Read models derived from the state.
//!
//! Each attachment's catalog is projected, never declared: `reach`, plus the
//! native tools of the companion's ready connections as `<service>_<tool>`.
//! A companion with no connections sees `reach` alone. `reach` always reports
//! the live tool set, so a mind can trust it even when its host ignores tool
//! list changes.
//!
//! Projections hold no truth and can always be rebuilt.

use lobby_contracts::{Companion, Effect, Passage, ToolDescriptor, ToolName, ToolResult};
use serde_json::json;

/// The name of the one tool every attachment has.
pub const REACH: &str = "reach";

/// The descriptor of `reach`.
pub fn reach_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: ToolName::new(REACH).unwrap_or_else(|| unreachable!("reach is a valid tool name")),
        title: "Reach".to_owned(),
        description: "Says which companion you are attached as and what that companion can reach \
                      right now: each connected service, the addresses it reaches there, and the \
                      tools you can call for it. Call it first, and again whenever a tool you \
                      expected is missing."
            .to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
        effect: Effect::Read,
    }
}

/// The tools an attachment of `companion` sees, in order.
pub fn catalog(_companion: &Companion) -> Vec<ToolDescriptor> {
    vec![reach_descriptor()]
}

/// The answer to `reach` for `companion`.
pub fn reach(companion: &Companion) -> ToolResult {
    let tools: Vec<String> = catalog(companion)
        .iter()
        .map(|tool| tool.name.as_str().to_owned())
        .collect();
    let text = format!(
        "You are attached as {display} (moniker {moniker}). {display} reaches nothing yet: \
         the sponsor has not connected a service for this companion. Your tools: {tools}.",
        display = companion.display_name,
        moniker = companion.moniker,
        tools = tools.join(", "),
    );
    ToolResult {
        text: vec![Passage::Authored(text)],
        structured: Some(json!({
            "companion": {
                "id": companion.id.to_string(),
                "moniker": companion.moniker.as_str(),
                "displayName": companion.display_name.as_str(),
            },
            "connections": [],
            "tools": tools,
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lobby_contracts::{CompanionId, DisplayName, Moniker, Sponsor};

    fn ada() -> Companion {
        Companion {
            id: CompanionId::from_u128(1),
            moniker: Moniker::new("ada").expect("moniker"),
            display_name: DisplayName::new("Ada").expect("name"),
            sponsor: Sponsor::LOCAL,
        }
    }

    #[test]
    fn a_companion_without_connections_sees_reach_alone() {
        let catalog = catalog(&ada());
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].name.as_str(), REACH);
        assert!(catalog[0].read_only_hint());
    }

    #[test]
    fn reach_reports_the_companion_and_the_live_tool_set() {
        let answer = reach(&ada());
        let text = answer.rendered();
        assert!(text.contains("Ada") && text.contains("reach"));
        let structured = answer.structured.expect("structured");
        assert_eq!(structured["companion"]["moniker"], "ada");
        assert_eq!(structured["tools"], json!(["reach"]));
        assert_eq!(structured["connections"], json!([]));
    }
}
