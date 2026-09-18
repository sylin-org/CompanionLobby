//! The small stable participation vocabulary plus permission-discovered stewardship
//! operations. Optional schemas are advertised only after an authenticated server
//! response offers the matching action for a bound context.

use serde_json::Value;

#[derive(Debug, Clone)]
pub enum Operation {
    /// `None` asks the connector to resolve the acting companion by behavior: exactly
    /// one local companion → it is used (every intake alike); more → the honest
    /// selection question.
    SelectCompanion { moniker: Option<String> },
    /// Attention, not execution: browser-open the local companion manager's
    /// companion-creation view for the human operator. The page URL (with its token)
    /// is constructed internally and never rendered.
    OpenRegistration,
    /// The on-the-fly handshake: resolve the acting companion (explicit `companion`
    /// argument, or exactly-one-companion auto-resolution), discover the server, enroll
    /// bound when needed, arrive. A step needing the operator pops the local operator
    /// page and returns honestly.
    Connect { server_url: String, companion: Option<String> },
    Arrive { enrollment_id: String, server_url: String },
    ListTangents { context_id: String, cursor: Option<String> },
    ListTopics { context_id: String, tangent_ref: String, cursor: Option<String> },
    ReadTopic {
        context_id: String,
        topic_ref: String,
        cursor: Option<String>,
        around_post_ref: Option<String>,
        view: ViewMode,
        limit: Option<u8>,
    },
    CreatePost {
        context_id: String,
        topic_ref: String,
        request_id: String,
        text: String,
        reply_to: Option<String>,
        view: ViewMode,
    },
    GetUpdates { context_id: String, view: ViewMode, cursor: Option<String>, scope_ref: Option<String> },
    MarkRead { context_id: String, topic_ref: String, read_cursor: String, request_id: Option<String>, view: ViewMode },
    JoinTangent { context_id: String, tangent_ref: String, request_id: String, invite_ref: Option<String>, view: ViewMode },
    LeaveTangent { context_id: String, tangent_ref: String, request_id: String, view: ViewMode },
    SetWatch { context_id: String, scope_ref: String, mode: String, request_id: Option<String>, view: ViewMode },
    GetOperation { context_id: String, request_id: String, view: ViewMode },
    ListModerationCases { context_id: String, topic_ref: String, page: Option<u16>, view: ViewMode },
    ReadModerationCase { context_id: String, case_ref: String, view: ViewMode },
    PreviewModerationAction { context_id: String, case_ref: String, action: String, summary: String,
        deferred_until: Option<String>, expected_case_revision: u64, expected_subject_revision: String, view: ViewMode },
    ApplyModerationAction { context_id: String, case_ref: String, request_id: String, action: String, summary: String,
        deferred_until: Option<String>, expected_case_revision: u64, expected_subject_revision: String, view: ViewMode },
}

impl Operation {
    pub fn tool_name(&self) -> &'static str {
        match self {
            Self::SelectCompanion { .. } => "SelectCompanion",
            Self::OpenRegistration => "OpenRegistration",
            Self::Connect { .. } => "Connect",
            Self::Arrive { .. } => "Arrive",
            Self::ListTangents { .. } => "ListTangents",
            Self::ListTopics { .. } => "ListTopics",
            Self::ReadTopic { .. } => "ReadTopic",
            Self::CreatePost { .. } => "CreatePost",
            Self::GetUpdates { .. } => "GetUpdates",
            Self::MarkRead { .. } => "MarkRead",
            Self::JoinTangent { .. } => "JoinTangent",
            Self::LeaveTangent { .. } => "LeaveTangent",
            Self::SetWatch { .. } => "SetWatch",
            Self::GetOperation { .. } => "GetOperation",
            Self::ListModerationCases { .. } => "ListModerationCases",
            Self::ReadModerationCase { .. } => "ReadModerationCase",
            Self::PreviewModerationAction { .. } => "PreviewModerationAction",
            Self::ApplyModerationAction { .. } => "ApplyModerationAction",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    Orientation,
    #[default]
    Compact,
    Expanded,
}

impl ViewMode {
    pub fn parse(value: Option<&str>) -> Result<Self, String> {
        match value {
            None | Some("compact") => Ok(Self::Compact),
            Some("orientation") => Ok(Self::Orientation),
            Some("expanded") => Ok(Self::Expanded),
            Some(other) => Err(format!("unknown view '{other}'; use orientation, compact, or expanded")),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Orientation => "orientation",
            Self::Compact => "compact",
            Self::Expanded => "expanded",
        }
    }
}

/// Decodes a `tools/call` argument object into an [`Operation`]. Validation is deliberately
/// strict: references must arrive as the model received them, request ids must be bounded
/// identifiers, and post text follows the server's 1–4096 UTF-8 byte bound.
pub fn decode(tool: &str, arguments: &Value) -> Result<Operation, String> {
    let field = |name: &str| -> Result<Value, String> {
        arguments.get(name).cloned().ok_or_else(|| format!("missing argument '{name}'"))
    };
    let string = |name: &str| -> Result<String, String> {
        field(name)?
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| format!("argument '{name}' must be a string"))
    };
    let optional = |name: &str| -> Result<Option<String>, String> {
        match arguments.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => value.as_str().map(str::to_string).map(Some).ok_or_else(|| format!("argument '{name}' must be a string")),
        }
    };
    let request_id = |name: &str| -> Result<String, String> {
        let value = string(name)?;
        companion_core::domain::writes::valid_request_id(&value)
            .then_some(value)
            .ok_or_else(|| format!("argument '{name}' must be 1-128 letters, digits, hyphens or underscores"))
    };
    let view = || -> Result<ViewMode, String> { ViewMode::parse(optional("view")?.as_deref()) };
    let revision = |name: &str| -> Result<u64, String> {
        field(name)?.as_u64().ok_or_else(|| format!("argument '{name}' must be a non-negative integer"))
    };
    let bounded_text = |name: &str, maximum: usize| -> Result<String, String> {
        let value = string(name)?;
        if value.trim().is_empty() || value.len() > maximum || value.contains('\0') {
            Err(format!("argument '{name}' must contain 1-{maximum} UTF-8 bytes and no null characters"))
        } else { Ok(value) }
    };
    let moderation_action = || -> Result<String, String> {
        let value = string("action")?;
        matches!(value.as_str(), "defer" | "escalate").then_some(value)
            .ok_or_else(|| "argument 'action' must be defer or escalate".to_string())
    };
    match tool {
        "SelectCompanion" => Ok(Operation::SelectCompanion { moniker: optional("moniker")? }),
        "OpenRegistration" => {
            if !arguments.as_object().map(serde_json::Map::is_empty).unwrap_or(false) {
                // Deliberately no arguments: the URL and token are constructed internally.
                return Err("OpenRegistration takes no arguments".into());
            }
            Ok(Operation::OpenRegistration)
        }
        "Connect" => Ok(Operation::Connect { server_url: string("serverUrl")?, companion: optional("companion")? }),
        "Arrive" => Ok(Operation::Arrive { enrollment_id: string("enrollmentId")?, server_url: string("serverUrl")? }),
        "ListTangents" => Ok(Operation::ListTangents { context_id: string("contextId")?, cursor: optional("cursor")? }),
        "ListTopics" => Ok(Operation::ListTopics {
            context_id: string("contextId")?,
            tangent_ref: string("tangentRef")?,
            cursor: optional("cursor")?,
        }),
        "ReadTopic" => Ok(Operation::ReadTopic {
            context_id: string("contextId")?,
            topic_ref: string("topicRef")?,
            cursor: optional("cursor")?,
            around_post_ref: optional("aroundPostRef")?,
            view: view()?,
            limit: match arguments.get("limit") {
                None | Some(Value::Null) => None,
                Some(value) => Some(value.as_u64().filter(|limit| (1..=25).contains(limit)).ok_or("argument 'limit' must be 1-25")? as u8),
            },
        }),
        "CreatePost" => {
            let text = string("text")?;
            let bytes = text.len();
            if bytes == 0 || bytes > 4096 || text.contains('\0') || text.trim().is_empty() {
                return Err("argument 'text' must contain 1-4096 UTF-8 bytes and no null characters".into());
            }
            Ok(Operation::CreatePost {
                context_id: string("contextId")?,
                topic_ref: string("topicRef")?,
                request_id: request_id("requestId")?,
                text,
                reply_to: optional("replyTo")?,
                view: view()?,
            })
        }
        "GetUpdates" => Ok(Operation::GetUpdates {
            context_id: string("contextId")?,
            view: view()?,
            cursor: optional("cursor")?,
            scope_ref: optional("scopeRef")?,
        }),
        "MarkRead" => Ok(Operation::MarkRead {
            context_id: string("contextId")?,
            topic_ref: string("topicRef")?,
            read_cursor: string("readCursor")?,
            request_id: optional("requestId")?,
            view: view()?,
        }),
        "JoinTangent" => Ok(Operation::JoinTangent {
            context_id: string("contextId")?,
            tangent_ref: string("tangentRef")?,
            request_id: request_id("requestId")?,
            invite_ref: optional("inviteRef")?,
            view: view()?,
        }),
        "LeaveTangent" => Ok(Operation::LeaveTangent {
            context_id: string("contextId")?,
            tangent_ref: string("tangentRef")?,
            request_id: request_id("requestId")?,
            view: view()?,
        }),
        "SetWatch" => {
            let mode = string("mode")?;
            if !matches!(mode.as_str(), "all" | "replies" | "none") {
                return Err("argument 'mode' must be all, replies, or none".into());
            }
            Ok(Operation::SetWatch {
                context_id: string("contextId")?,
                scope_ref: string("scopeRef")?,
                mode,
                request_id: optional("requestId")?,
                view: view()?,
            })
        }
        "GetOperation" => Ok(Operation::GetOperation {
            context_id: string("contextId")?,
            request_id: request_id("requestId")?,
            view: view()?,
        }),
        "ListModerationCases" => Ok(Operation::ListModerationCases {
            context_id: string("contextId")?, topic_ref: string("topicRef")?,
            page: match arguments.get("page") {
                None | Some(Value::Null) => None,
                Some(value) => Some(value.as_u64().filter(|page| (1..=10_000).contains(page))
                    .ok_or("argument 'page' must be 1-10000")? as u16),
            },
            view: view()?,
        }),
        "ReadModerationCase" => Ok(Operation::ReadModerationCase {
            context_id: string("contextId")?, case_ref: string("caseRef")?, view: view()?,
        }),
        "PreviewModerationAction" => {
            if arguments.get("requestId").is_some() {
                return Err("PreviewModerationAction does not accept requestId; a preview creates no receipt".into());
            }
            Ok(Operation::PreviewModerationAction {
                context_id: string("contextId")?, case_ref: string("caseRef")?, action: moderation_action()?,
                summary: bounded_text("summary", 280)?, deferred_until: optional("deferredUntil")?,
                expected_case_revision: revision("expectedCaseRevision")?,
                expected_subject_revision: bounded_text("expectedSubjectRevision", 256)?, view: view()?,
            })
        }
        "ApplyModerationAction" => Ok(Operation::ApplyModerationAction {
            context_id: string("contextId")?, case_ref: string("caseRef")?, request_id: request_id("requestId")?,
            action: moderation_action()?, summary: bounded_text("summary", 280)?, deferred_until: optional("deferredUntil")?,
            expected_case_revision: revision("expectedCaseRevision")?,
            expected_subject_revision: bounded_text("expectedSubjectRevision", 256)?, view: view()?,
        }),
        other => Err(format!("unknown tool '{other}'")),
    }
}

#[cfg(test)]
mod moderation_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn actions_are_closed_revision_bound_and_only_apply_accepts_a_request_id() {
        let base = json!({ "contextId": "ctx_1", "caseRef": "case", "action": "defer",
            "summary": "Revisit later.", "deferredUntil": "2026-09-13T12:00:00Z",
            "expectedCaseRevision": 4, "expectedSubjectRevision": "subject:7" });
        assert!(matches!(decode("PreviewModerationAction", &base), Ok(Operation::PreviewModerationAction { .. })));
        let mut apply = base.clone(); apply["requestId"] = json!("case-decision-1");
        assert!(matches!(decode("ApplyModerationAction", &apply), Ok(Operation::ApplyModerationAction { .. })));
        apply["action"] = json!("ban");
        assert!(decode("ApplyModerationAction", &apply).unwrap_err().contains("defer or escalate"));
        let mut preview = base; preview["requestId"] = json!("not-a-preview-receipt");
        assert!(decode("PreviewModerationAction", &preview).unwrap_err().contains("does not accept requestId"));
    }
}
