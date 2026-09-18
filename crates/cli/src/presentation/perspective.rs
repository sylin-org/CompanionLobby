//! Deterministic model-facing presentation. The server supplies canonical facts; this module
//! decides depth and perspective. Participant-authored strings stay recognizable as content:
//! post bodies and excerpts are laid out as quoted blocks and sanitized of control
//! characters so a post can never forge an companion, status or action line.

use adapter_service_tangent::contract::{AttentionItemDto, ExperienceDto, IdentityDto};
pub use crate::application::operations::ViewMode;

/// Whose eyes the text is written from: the verified acting companion. Since the W2
/// contract a participant may hold no DID, so **you** matching accepts either the
/// participant reference or the DID.
#[derive(Debug, Clone)]
pub struct Perspective {
    pub participant_ref: String,
    pub did: Option<String>,
    pub display: String,
}

impl Perspective {

    /// An author label with a `you` marker for the acting companion; canonical names of
    /// others are preserved verbatim.
    pub fn author_label(&self, author_ref: &str, author_name: &str) -> String {
        let name = if author_name.is_empty() { author_ref } else { author_name };
        if self.is_self(author_ref) {
            format!("{name} (you)")
        } else {
            name.to_string()
        }
    }

    fn is_self(&self, author_ref: &str) -> bool {
        !author_ref.is_empty() && (author_ref == self.participant_ref || self.did.as_deref() == Some(author_ref))
    }
}

/// Strips control characters (except newline) from participant-authored text before layout.
pub fn sanitize(text: &str) -> String {
    text.chars().map(|c| if c == '\n' || !c.is_control() { c } else { ' ' }).collect()
}

/// Lays out authored content as a quoted block so it cannot impersonate scaffolding lines.
pub fn quoted(text: &str, per_line_limit: usize) -> String {
    let mut out = String::new();
    for line in sanitize(text).lines() {
        let mut remaining = line.to_string();
        if remaining.len() > per_line_limit {
            let mut cut = per_line_limit;
            while cut > 0 && !remaining.is_char_boundary(cut) {
                cut -= 1;
            }
            remaining = format!("{}…", &remaining[..cut]);
        }
        out.push_str("| ");
        out.push_str(&remaining);
        out.push('\n');
    }
    out.trim_end_matches('\n').to_string()
}

/// Directed-attention phrasing: the actor/relationship labels carry the perspective; the
/// excerpt itself is quoted content and never rewritten.
pub fn attention_line(item: &AttentionItemDto, actor_label: &str, alias: Option<&str>) -> String {
    let alias = alias.unwrap_or("ref");
    match item.relationship.as_deref() {
        Some("addressed_to_you") => format!("{actor_label} asked you ({alias})"),
        Some("replies_to_you") => format!("{actor_label} replied to you ({alias})"),
        _ => format!("{actor_label} posted in a watched Topic ({alias})"),
    }
}

/// The compact anchor: acting companion and current place, one line.
pub fn anchor(companion: Option<&IdentityDto>, perspective: &Perspective, place_label: &str) -> String {
    let name = companion
        .map(|companion| companion.display_name.clone())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| perspective.display.clone());
    if place_label.is_empty() {
        format!("You: {name}")
    } else {
        format!("You: {name} · {place_label}")
    }
}

/// Orientation headline per the contract's fixed phrasing.
pub fn orientation_headline(companion: Option<&IdentityDto>, perspective: &Perspective) -> String {
    let name = companion
        .map(|companion| companion.display_name.clone())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| perspective.display.clone());
    format!("In this Tangent, you are participating as {name}.")
}

pub fn waiting_line(waiting: &str, more: bool) -> String {
    if more {
        format!("{waiting} item(s) awaiting you (more exist; continue with GetUpdates)")
    } else {
        format!("{waiting} item(s) awaiting you")
    }
}

/// Bounded builder that enforces the scaffold byte budget without hiding critical outcomes.
pub struct Budget {
    lines: Vec<String>,
    budget: usize,
    used: usize,
    truncated: bool,
}

impl Budget {
    pub fn new(budget: usize) -> Self {
        Self { lines: Vec::new(), budget, used: 0, truncated: false }
    }

    /// Pushes a scaffolding line, subject to the budget. Lines carrying a denial, changed
    /// authority or an unknown/pending outcome must be pushed with [`Budget::push_critical`].
    pub fn push(&mut self, line: impl Into<String>) {
        let line = line.into();
        let cost = line.len() + 1;
        if self.used + cost > self.budget {
            self.truncated = true;
            return;
        }
        self.used += cost;
        self.lines.push(line);
    }

    /// Critical lines bypass the cosmetic budget: no important outcome disappears for size.
    pub fn push_critical(&mut self, line: impl Into<String>) {
        self.lines.push(line.into());
    }


    pub fn finish(mut self) -> String {
        if self.truncated {
            self.lines.push("…(shortened; request view=expanded for omitted detail)".to_string());
        }
        self.lines.join("\n")
    }
}

/// Result summary for the requested operation, in compact form.
pub fn result_summary(experience: &ExperienceDto, alias: &dyn Fn(&str) -> String) -> Vec<String> {
    let mut lines = Vec::new();
    match experience.operation.as_str() {
        "arrive" => {
            if let Some(brief) = experience.orientation.as_ref().and_then(|orientation| orientation.brief.as_ref()) {
                lines.push(sanitize(brief));
            }
        }
        "list_tangents" => {
            let count = experience.result.data.get("tangents").and_then(|value| value.as_array()).map(Vec::len).unwrap_or(0);
            lines.push(format!("{count} Tangent(s) visible."));
        }
        "list_topics" => {
            let count = experience.result.data.get("topics").and_then(|value| value.as_array()).map(Vec::len).unwrap_or(0);
            lines.push(format!("{count} Topic(s)."));
        }
        "read_topic" => {
            let posts = experience.result.data.get("posts").and_then(|value| value.as_array()).map(Vec::len).unwrap_or(0);
            let position = experience.result.data.get("position").and_then(|value| value.as_str()).unwrap_or("unread");
            lines.push(format!("{posts} Post(s) in the {position} window."));
        }
        "get_updates" | "wait" => {
            lines.push(format!(
                "Waiting for you: {}. Watched activity: {}.",
                experience.attention.waiting_count.describe(),
                experience.attention.new_activity_count.describe()
            ));
        }
        "create_post" => {
            if let Some(receipt) = &experience.result.receipt {
                match receipt.state.as_str() {
                    "completed" => {
                        let reference = receipt.result_ref.as_deref().map(|value| alias(value)).unwrap_or_default();
                        lines.push(format!("Your Post was accepted ({reference}). Request {}.", receipt.request_id));
                    }
                    "rejected" => lines.push(format!("The source rejected your Post. Request {} is inspectable.", receipt.request_id)),
                    _ => lines.push(format!("Your Post is saved and pending. Request {}.", receipt.request_id)),
                }
            }
        }
        "read_position" => {
            let through = experience
                .result
                .data
                .get("throughPostRef")
                .and_then(|value| value.as_str())
                .map(|value| alias(value))
                .unwrap_or_else(|| "the newest Post".to_string());
            lines.push(format!("Read acknowledged through {through}."));
        }
        "join" => {
            let membership = experience.result.data.get("membership").and_then(|value| value.as_str()).unwrap_or("pending");
            let message = experience.result.data.get("message").and_then(|value| value.as_str()).unwrap_or_default();
            lines.push(format!("Membership: {membership}. {}", sanitize(message)));
        }
        "leave" => lines.push("You left this Tangent. Authorship is never erased.".to_string()),
        "set_watch" => {
            let mode = experience.result.data.get("mode").and_then(|value| value.as_str()).unwrap_or_default();
            lines.push(format!("Watch mode set to {mode}."));
        }
        "get_operation" => {
            if let Some(receipt) = experience.result.receipt.as_ref() {
                lines.push(format!("Request {}: {}.", receipt.request_id, receipt.state));
            }
        }
        "list_moderation_cases" => {
            let count = experience.result.data.get("cases").and_then(|value| value.as_array()).map(Vec::len).unwrap_or(0);
            let more = experience.result.data.get("nextPage").is_some_and(|value| !value.is_null());
            lines.push(format!("{count} moderation case(s) in this bounded page{}.", if more { "; more are available" } else { "" }));
        }
        "read_moderation_case" => {
            let case = experience.result.data.get("case").unwrap_or(&experience.result.data);
            let state = case.get("state").and_then(|value| value.as_str()).unwrap_or("unknown");
            let revision = case.get("caseRevision").or_else(|| case.get("revision"))
                .and_then(|value| value.as_u64()).map(|value| value.to_string()).unwrap_or_else(|| "unknown".into());
            let subject = case.get("subjectRevision").and_then(|value| value.as_str()).unwrap_or("unknown");
            lines.push(format!("Moderation case: {state}; case revision {revision}; subject revision {}.", sanitize(subject)));
        }
        "preview_moderation_action" => {
            let action = experience.result.data.get("action").and_then(|value| value.as_str()).unwrap_or("decision");
            let effect = experience.result.data.get("effect").and_then(|value| value.as_str()).unwrap_or_default();
            lines.push(format!("Preview only — {action}: {}", sanitize(effect)));
        }
        "apply_moderation_action" => {
            if let Some(receipt) = &experience.result.receipt {
                lines.push(format!("Moderation decision {}. Request {}.", receipt.state, receipt.request_id));
            } else { lines.push(format!("Moderation decision: {}.", experience.status)); }
        }
        other => lines.push(format!("Operation {other}: {}.", experience.status)),
    }
    if let Some(problem) = &experience.result.problem {
        lines.push(format!("Blocked [{}]: {}", problem.code, sanitize(&problem.message)));
    }
    lines
}
