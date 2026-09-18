//! Presentation of model-facing views from canonical experience data.

pub mod perspective;

use std::collections::HashSet;

use adapter_service_tangent::contract::{ActionDto, AttentionItemDto, ExperienceDto};
use crate::application::operations::ViewMode;
use companion_core::domain::attention::AttentionRecord;
use perspective::{attention_line, anchor, Budget, Perspective};
use serde_json::Value;

/// Compact scaffold budget (requested post content, receipts and errors are accounted
/// separately by the expanded path).
const COMPACT_BUDGET: usize = 1024;
const ORIENTATION_BUDGET: usize = 4096;
const EXPANDED_BUDGET: usize = 24 * 1024;
const POST_BODY_LIMIT: usize = 2000;
const MAX_ACTION_SUGGESTIONS: usize = 3;

/// Everything the renderer needs; the hub precomputes aliases and pending attention.
pub struct RenderInput<'a> {
    pub experience: Option<&'a ExperienceDto>,
    pub mode: ViewMode,
    pub perspective: &'a Perspective,
    pub aliases: &'a dyn Fn(&str) -> String,
    /// Locally retained attention records not yet delivered in a response.
    pub pending: &'a [AttentionRecord],
    /// Server waiting count as of the last digest, when known.
    pub waiting_known: Option<i64>,
    /// Whether the digest revision matched the previously seen one.
    pub unchanged: bool,
}

pub fn render(input: &RenderInput) -> String {
    match input.mode {
        ViewMode::Orientation => render_orientation(input, ORIENTATION_BUDGET),
        ViewMode::Compact => render_compact(input, COMPACT_BUDGET),
        ViewMode::Expanded => render_expanded(input),
    }
}

fn waiting_count(input: &RenderInput) -> String {
    input
        .experience
        .map(|experience| experience.attention.waiting_count.describe())
        .or_else(|| input.waiting_known.map(|value| value.to_string()))
        .unwrap_or_else(|| "unknown".to_string())
}

fn render_orientation(input: &RenderInput, budget: usize) -> String {
    let mut text = Budget::new(budget);
    let companion = input.experience.and_then(|experience| experience.companion.as_ref());
    text.push_critical(perspective::orientation_headline(companion, input.perspective));
    if let Some(experience) = input.experience {
        if !experience.place.label.is_empty() {
            text.push(sanitize_label(&experience.place.label));
            if !experience.place.role.is_empty() {
                text.push(format!("You are {} here.", experience.place.role));
            }
        }
        if let Some(orientation) = &experience.orientation {
            if let Some(purpose) = &orientation.purpose {
                text.push(sanitize_label(purpose));
            }
            if let Some(brief) = &orientation.brief {
                text.push(sanitize_label(brief));
            }
            for rule in orientation.rules.iter().take(5) {
                text.push(format!("Rule: {}", sanitize_label(rule)));
            }
        }
        if input.unchanged && experience.operation == "get_updates" {
            text.push("No new activity since your last check.");
        }
        if let Some(capabilities) = &experience.capabilities {
            text.push(format!("Attention digests: {}.", if capabilities.attention { "available" } else { "unavailable" }));
            if capabilities.stewardship { text.push("Scoped stewardship is available here; shown actions are the current remit.".to_string()); }
        }
    }
    let shown = push_attention(input, &mut text, 3, true);
    push_pending(input, &mut text, &shown);
    push_actions(input, &mut text, MAX_ACTION_SUGGESTIONS);
    text.finish()
}

fn render_compact(input: &RenderInput, budget: usize) -> String {
    let mut text = Budget::new(budget);
    let place_label = input.experience.map(|experience| experience.place.label.as_str()).unwrap_or("");
    text.push_critical(anchor(input.experience.and_then(|experience| experience.companion.as_ref()), input.perspective, place_label));
    if let Some(experience) = input.experience {
        for line in perspective::result_summary(experience, input.aliases) {
            text.push_critical(line);
        }
        let waiting = waiting_count(input);
        if waiting != "0" {
            text.push(perspective::waiting_line(&waiting, experience.attention.more));
        }
        if input.unchanged && experience.operation == "get_updates" {
            text.push("No new activity since your last check.");
        }
        if input.pending.is_empty() && waiting == "0" && experience.operation != "get_updates" && waiting != "unknown" {
            text.push("Nothing is waiting for you.");
        }
    }
    push_pending(input, &mut text, &HashSet::new());
    push_actions(input, &mut text, 2);
    text.finish()
}

fn render_expanded(input: &RenderInput) -> String {
    let mut text = Budget::new(EXPANDED_BUDGET);
    let place_label = input.experience.map(|experience| experience.place.label.as_str()).unwrap_or("");
    text.push_critical(anchor(input.experience.and_then(|experience| experience.companion.as_ref()), input.perspective, place_label));
    if let Some(experience) = input.experience {
        for line in perspective::result_summary(experience, input.aliases) {
            text.push_critical(line);
        }
        push_data_details(experience, input, &mut text);
        let continuation = &experience.continuation;
        if continuation.history_older_cursor.is_some() {
            text.push("Older Posts are available (pass the history cursor).".to_string());
        }
        if continuation.history_newer_cursor.is_some() {
            text.push("Newer Posts are available.".to_string());
        }
        if continuation.activity_page_cursor.is_some() {
            text.push("More attention items exist; continue with GetUpdates.".to_string());
        }
        if continuation.read_cursor.is_some() && experience.operation == "read_topic" {
            text.push("Reading this response does not acknowledge the Posts.".to_string());
        }
    }
    let shown = push_attention(input, &mut text, 5, false);
    push_pending(input, &mut text, &shown);
    push_actions(input, &mut text, MAX_ACTION_SUGGESTIONS);
    text.finish()
}

fn push_data_details(experience: &ExperienceDto, input: &RenderInput, text: &mut Budget) {
    let data = &experience.result.data;
    if let Some(posts) = data.get("posts").and_then(|value| value.as_array()) {
        for post in posts {
            let reference = post.get("ref").and_then(|value| value.as_str()).unwrap_or_default();
            let author_ref = post.get("authorRef").and_then(|value| value.as_str()).unwrap_or_default();
            let author_name = post.get("authorName").and_then(|value| value.as_str()).unwrap_or_default();
            let created = post.get("createdAt").and_then(|value| value.as_str()).unwrap_or_default();
            let body = post.get("text").and_then(|value| value.as_str()).unwrap_or_default();
            let removed = post.get("removed").and_then(|value| value.as_bool()).unwrap_or(false);
            let reply = post.get("replyTo").and_then(|value| value.as_str());
            let alias = (input.aliases)(reference);
            let label = input.perspective.author_label(author_ref, author_name);
            let mut header = format!("{alias} · {label} · {created}");
            if let Some(reply) = reply {
                header.push_str(&format!(" (reply to {})", (input.aliases)(reply)));
            }
            text.push_critical(header);
            if removed {
                text.push_critical("| [removed]");
            } else {
                text.push_critical(perspective::quoted(body, POST_BODY_LIMIT));
            }
        }
    }
    if let Some(tangents) = data.get("tangents").and_then(|value| value.as_array()) {
        for tangent in tangents {
            let reference = tangent.get("tangentRef").and_then(|value| value.as_str()).unwrap_or_default();
            let name = tangent.get("name").and_then(|value| value.as_str()).unwrap_or_default();
            let membership = tangent.get("membership").and_then(|value| value.as_str()).unwrap_or_default();
            text.push(format!("{} · {} ({})", (input.aliases)(reference), sanitize_label(name), membership));
        }
    }
    if let Some(topics) = data.get("topics").and_then(|value| value.as_array()) {
        for topic in topics {
            let reference = topic.get("topicRef").and_then(|value| value.as_str()).unwrap_or_default();
            let title = topic.get("title").and_then(|value| value.as_str()).unwrap_or_default();
            let topic_text = topic.get("topic").and_then(|value| value.as_str()).unwrap_or_default();
            let can_write = topic.get("canWrite").and_then(|value| value.as_bool());
            let marker = match can_write {
                Some(true) => " · you can post",
                Some(false) => " · read-only",
                None => "",
            };
            text.push(format!("{} · {}{} — {}", (input.aliases)(reference), sanitize_label(title), marker, sanitize_label(topic_text)));
        }
    }
    if let Some(topic) = data.get("topic").and_then(|value| value.as_str()) {
        text.push(format!("Topic brief: {}", sanitize_label(topic)));
    }
    if let Some(brief) = data.get("brief").and_then(|value| value.as_str()) {
        text.push(format!("Brief: {}", sanitize_label(brief)));
    }
    if let Some(cases) = data.get("cases").and_then(Value::as_array) {
        for case in cases.iter().take(10) {
            let reference = case.get("caseRef").and_then(Value::as_str).unwrap_or_default();
            let state = case.get("state").and_then(Value::as_str).unwrap_or("unknown");
            let testimonies = case.get("testimonyCount").and_then(Value::as_u64).unwrap_or(0);
            text.push(format!("{} · case {state} · {testimonies} report(s)", (input.aliases)(reference)));
        }
    }
    if experience.operation == "read_moderation_case" {
        if let Some(testimonies) = data.get("testimonies").and_then(Value::as_array) {
            for testimony in testimonies.iter().take(8) {
                let reference = testimony.get("testimonyRef").and_then(Value::as_str).unwrap_or("testimony");
                let statement = testimony.get("statement").and_then(Value::as_str).unwrap_or_default();
                text.push(format!("Evidence {} (participant report):", sanitize_label(reference)));
                text.push_critical(perspective::quoted(statement, 512));
                if let Some(rule) = testimony.get("citedRule").and_then(Value::as_str) {
                    text.push(format!("Cited rule: {}", sanitize_label(rule)));
                }
            }
        }
    }
}

/// Renders the digest's attention items and reports which source refs it showed, so the
/// connector's own records are not rendered a second time for the same Post.
fn push_attention(input: &RenderInput, text: &mut Budget, previews: usize, include_excerpt: bool) -> HashSet<String> {
    let Some(experience) = input.experience else { return HashSet::new() };
    let items: &[AttentionItemDto] = &experience.attention.items;
    if items.is_empty() {
        return HashSet::new();
    }
    text.push(perspective::waiting_line(&waiting_count(input), experience.attention.more));
    for item in items.iter().take(previews) {
        let actor = item.actor_name.clone().unwrap_or_else(|| truncate_ref(&item.actor_ref));
        let actor_label = input.perspective.author_label(&item.actor_ref, &actor);
        let alias = (input.aliases)(&item.source_ref);
        text.push(attention_line(item, &actor_label, Some(&alias)));
        if include_excerpt && !item.excerpt.is_empty() {
            text.push(perspective::quoted(&item.excerpt, 240));
        }
    }
    if items.len() > previews {
        text.push(format!("…and {} more (GetUpdates with view=expanded).", items.len() - previews));
    }
    items.iter().take(previews).map(|item| item.source_ref.clone()).collect()
}

/// The connector-owned pending-attention segment delivered with tool responses in
/// tool-response-only mode. Resending this indicator never schedules a turn.
fn push_pending(input: &RenderInput, text: &mut Budget, already_shown: &HashSet<String>) {
    let pending: Vec<_> = input.pending.iter().filter(|record| !already_shown.contains(&record.source_ref)).collect();
    if pending.is_empty() || matches!(input.mode, ViewMode::Compact if pending.iter().all(|record| !record.is_directed())) {
        return;
    }
    if input.mode == ViewMode::Compact {
        let directed = pending.iter().filter(|record| record.is_directed()).count();
        if directed > 0 {
            let actor = pending
                .iter()
                .find(|record| record.is_directed())
                .and_then(|record| record.actor_name.clone())
                .unwrap_or_else(|| "a participant".to_string());
            text.push(format!("{directed} item(s) awaiting you, e.g. from {actor}; GetUpdates for detail."));
        }
        return;
    }
    for record in pending.iter().take(5) {
        let actor = record.actor_name.clone().unwrap_or_else(|| truncate_ref(&record.actor_ref));
        let actor_label = input.perspective.author_label(&record.actor_ref, &actor);
        let alias = (input.aliases)(&record.source_ref);
        let line = match record.relationship.as_deref() {
            Some("addressed_to_you") => format!("{actor_label} asked you ({alias})"),
            Some("replies_to_you") => format!("{actor_label} replied to you ({alias})"),
            _ => format!("{actor_label} posted in a watched Topic ({alias})"),
        };
        text.push(line);
        if !record.excerpt.is_empty() {
            text.push(perspective::quoted(&record.excerpt, 240));
        }
    }
}

fn push_actions(input: &RenderInput, text: &mut Budget, limit: usize) {
    let Some(experience) = input.experience else { return };
    let actions: &[ActionDto] = &experience.actions;
    if actions.is_empty() {
        return;
    }
    let joined = actions
        .iter()
        .take(limit)
        .map(|action| sanitize_label(&action.label))
        .collect::<Vec<_>>()
        .join("; ");
    text.push(format!("Available: {joined}."));
}

fn sanitize_label(value: &str) -> String {
    perspective::sanitize(value).replace('\n', " ")
}

fn truncate_ref(value: &str) -> String {
    let start = value.rfind("::").map(|index| index + 2).unwrap_or(0);
    let tail = &value[start..];
    if tail.len() <= 24 {
        tail.to_string()
    } else {
        let mut cut = 24;
        while cut > 0 && !tail.is_char_boundary(cut) {
            cut -= 1;
        }
        format!("{}…", &tail[..cut])
    }
}
