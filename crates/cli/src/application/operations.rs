//! The closed tool vocabulary: a four-key web-shaped core, plus capability rings a
//! granted service may speak (Forum today; Social later). Every tool is an
//! instrument — atomic, factual, named to be cited in a companion's charter. The
//! catalog is projected elsewhere (see the hub); this module only decodes.

use serde_json::Value;

#[derive(Debug, Clone)]
pub enum Operation {
    // ----- the core -----
    /// Who exists, and what each persona may reach (its service grants).
    ListCompanions,
    /// The only mint of a session: connect to a service as a persona, always
    /// explicitly named. The answer is the briefing — "You are lumen — session
    /// tangent_9f01."
    Connect { service: String, persona: String, address: Option<String> },
    /// Identity facts plus the live capability readout: the ring, as of now.
    WhoAmI { session: String },
    /// One inbox: mentions, watched activity, and the settlement of one's own writes.
    CatchUp { session: String, view: ViewMode, cursor: Option<String> },

    // ----- the Forum ring -----
    ForumListSpaces { session: String, cursor: Option<String> },
    ForumListThreads { session: String, space_ref: String, cursor: Option<String> },
    ForumReadThread {
        session: String,
        thread_ref: String,
        cursor: Option<String>,
        around_post_ref: Option<String>,
        view: ViewMode,
        limit: Option<u8>,
    },
    ForumStartThread {
        session: String,
        space_ref: String,
        title: String,
        text: String,
        request_id: String,
        view: ViewMode,
    },
    ForumPost {
        session: String,
        thread_ref: String,
        request_id: String,
        text: String,
        reply_to: Option<String>,
        view: ViewMode,
    },
    ForumEditPost { session: String, post_ref: String, text: String, view: ViewMode },
    ForumDeletePost { session: String, post_ref: String, view: ViewMode },
    ForumJoinSpace {
        session: String,
        space_ref: String,
        request_id: String,
        invite_ref: Option<String>,
        view: ViewMode,
    },
    ForumLeaveSpace { session: String, space_ref: String, request_id: String, view: ViewMode },
    ForumMarkRead {
        session: String,
        thread_ref: String,
        read_cursor: String,
        request_id: Option<String>,
        view: ViewMode,
    },
    ForumWatch { session: String, scope_ref: String, mode: WatchMode, view: ViewMode },
    ForumReadUser { session: String, user_ref: String },
    ForumOpenCase { session: String, thread_ref: String, subject_ref: String, reason: String, view: ViewMode },

    // ----- the Forum ring, stewardship (projected by reported authority) -----
    ForumListCases { session: String, thread_ref: String, page: Option<u16>, view: ViewMode },
    ForumReadCase { session: String, case_ref: String, view: ViewMode },
    ForumPreviewAction {
        session: String,
        case_ref: String,
        action: CaseAction,
        summary: String,
        deferred_until: Option<String>,
        expected_case_revision: u64,
        expected_subject_revision: String,
        view: ViewMode,
    },
    ForumEscalateCase {
        session: String,
        case_ref: String,
        request_id: String,
        summary: String,
        deferred_until: Option<String>,
        expected_case_revision: u64,
        expected_subject_revision: String,
        view: ViewMode,
    },
    /// The user-management ladder in one key: the action argument is the rung, and
    /// the projected schema's enum carries the identity's authority.
    ForumManageUser {
        session: String,
        user_ref: String,
        action: ManageAction,
        reason: String,
        duration_seconds: Option<u64>,
        role: Option<String>,
        case_ref: Option<String>,
        view: ViewMode,
    },
}

/// The case actions the services' rules engines currently speak. Escalate files to
/// the owner; defer holds. The user-management ladder lives in [`ManageAction`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseAction {
    Defer,
    Escalate,
}

impl CaseAction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Defer => "defer",
            Self::Escalate => "escalate",
        }
    }
}

/// The attention modes a watch can hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchMode {
    All,
    Replies,
    None,
}

impl WatchMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Replies => "replies",
            Self::None => "none",
        }
    }
}

/// One rung of the user-management ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManageAction {
    Warn,
    Timeout,
    Suspend,
    Ban,
    AddRole,
    RemoveRole,
}

impl ManageAction {
    pub const ALL: &'static [ManageAction] = &[
        Self::Warn,
        Self::Timeout,
        Self::Suspend,
        Self::Ban,
        Self::AddRole,
        Self::RemoveRole,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Warn => "warn",
            Self::Timeout => "timeout",
            Self::Suspend => "suspend",
            Self::Ban => "ban",
            Self::AddRole => "add_role",
            Self::RemoveRole => "remove_role",
        }
    }

    /// The wire form: the `allowedActions` string a service reports to widen the
    /// projected enum. `assign_roles` covers both role rungs.
    pub fn from_allowed_action(action: &str) -> Option<Self> {
        match action {
            "warn_user" => Some(Self::Warn),
            "timeout_user" => Some(Self::Timeout),
            "suspend_user" => Some(Self::Suspend),
            "ban_user" => Some(Self::Ban),
            "assign_roles" => Some(Self::AddRole),
            _ => None,
        }
    }
}

impl Operation {
    pub fn tool_name(&self) -> &'static str {
        match self {
            Self::ListCompanions => "ListCompanions",
            Self::Connect { .. } => "Connect",
            Self::WhoAmI { .. } => "WhoAmI",
            Self::CatchUp { .. } => "CatchUp",
            Self::ForumListSpaces { .. } => "Forum_List_Spaces",
            Self::ForumListThreads { .. } => "Forum_List_Threads",
            Self::ForumReadThread { .. } => "Forum_Read_Thread",
            Self::ForumStartThread { .. } => "Forum_Start_Thread",
            Self::ForumPost { .. } => "Forum_Post",
            Self::ForumEditPost { .. } => "Forum_Edit_Post",
            Self::ForumDeletePost { .. } => "Forum_Delete_Post",
            Self::ForumJoinSpace { .. } => "Forum_Join_Space",
            Self::ForumLeaveSpace { .. } => "Forum_Leave_Space",
            Self::ForumMarkRead { .. } => "Forum_Mark_Read",
            Self::ForumWatch { .. } => "Forum_Watch",
            Self::ForumReadUser { .. } => "Forum_Read_User",
            Self::ForumOpenCase { .. } => "Forum_Open_Case",
            Self::ForumListCases { .. } => "Forum_List_Cases",
            Self::ForumReadCase { .. } => "Forum_Read_Case",
            Self::ForumPreviewAction { .. } => "Forum_Preview_Action",
            Self::ForumEscalateCase { .. } => "Forum_Escalate_Case",
            Self::ForumManageUser { .. } => "Forum_Manage_User",
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
    let optional_request_id = |name: &str| -> Result<Option<String>, String> {
        match optional(name)? {
            None => Ok(None),
            Some(value) if companion_core::domain::writes::valid_request_id(&value) => Ok(Some(value)),
            Some(_) => Err(format!("argument '{name}' must be 1-128 letters, digits, hyphens or underscores")),
        }
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
    let case_action = || -> Result<CaseAction, String> {
        match string("action")?.as_str() {
            "defer" => Ok(CaseAction::Defer),
            "escalate" => Ok(CaseAction::Escalate),
            other => Err(format!("argument 'action' must be defer or escalate, not '{other}'")),
        }
    };
    let manage_action = || -> Result<ManageAction, String> {
        let value = string("action")?;
        ManageAction::ALL
            .iter()
            .find(|action| action.as_str() == value)
            .copied()
            .ok_or_else(|| {
                format!(
                    "argument 'action' must be one of warn, timeout, suspend, ban, add_role, remove_role — not '{value}'"
                )
            })
    };
    let watch_mode = || -> Result<WatchMode, String> {
        match string("mode")?.as_str() {
            "all" => Ok(WatchMode::All),
            "replies" => Ok(WatchMode::Replies),
            "none" => Ok(WatchMode::None),
            other => Err(format!("argument 'mode' must be all, replies, or none — not '{other}'")),
        }
    };
    let page = || -> Result<Option<u16>, String> {
        match arguments.get("page") {
            None | Some(Value::Null) => Ok(None),
            Some(value) => value
                .as_u64()
                .filter(|page| *page <= u16::MAX as u64)
                .map(|page| Some(page as u16))
                .ok_or_else(|| "argument 'page' must be a non-negative integer".to_string()),
        }
    };
    let duration = || -> Result<Option<u64>, String> {
        match arguments.get("durationSeconds") {
            None | Some(Value::Null) => Ok(None),
            Some(value) => value.as_u64().map(Some).ok_or_else(|| "argument 'durationSeconds' must be a non-negative integer".to_string()),
        }
    };
    let limit = || -> Result<Option<u8>, String> {
        match optional("limit")? {
            None => Ok(None),
            Some(value) => match value.parse::<u8>() {
                Ok(parsed) if (1..=25).contains(&parsed) => Ok(Some(parsed)),
                _ => Err("argument 'limit' must be 1-25".to_string()),
            },
        }
    };
    match tool {
        "ListCompanions" => Ok(Operation::ListCompanions),
        "Connect" => Ok(Operation::Connect {
            service: string("service")?,
            persona: string("persona")?,
            address: optional("address")?,
        }),
        "WhoAmI" => Ok(Operation::WhoAmI { session: string("session")? }),
        "CatchUp" => Ok(Operation::CatchUp { session: string("session")?, view: view()?, cursor: optional("cursor")? }),

        "Forum_List_Spaces" => Ok(Operation::ForumListSpaces { session: string("session")?, cursor: optional("cursor")? }),
        "Forum_List_Threads" => Ok(Operation::ForumListThreads {
            session: string("session")?,
            space_ref: string("spaceRef")?,
            cursor: optional("cursor")?,
        }),
        "Forum_Read_Thread" => Ok(Operation::ForumReadThread {
            session: string("session")?,
            thread_ref: string("threadRef")?,
            cursor: optional("cursor")?,
            around_post_ref: optional("aroundPostRef")?,
            view: view()?,
            limit: limit()?,
        }),
        "Forum_Start_Thread" => Ok(Operation::ForumStartThread {
            session: string("session")?,
            space_ref: string("spaceRef")?,
            title: bounded_text("title", 256)?,
            text: bounded_text("text", 4096)?,
            request_id: request_id("requestId")?,
            view: view()?,
        }),
        "Forum_Post" => Ok(Operation::ForumPost {
            session: string("session")?,
            thread_ref: string("threadRef")?,
            request_id: request_id("requestId")?,
            text: bounded_text("text", 4096)?,
            reply_to: optional("replyTo")?,
            view: view()?,
        }),
        "Forum_Edit_Post" => Ok(Operation::ForumEditPost {
            session: string("session")?,
            post_ref: string("postRef")?,
            text: bounded_text("text", 4096)?,
            view: view()?,
        }),
        "Forum_Delete_Post" => Ok(Operation::ForumDeletePost {
            session: string("session")?,
            post_ref: string("postRef")?,
            view: view()?,
        }),
        "Forum_Join_Space" => Ok(Operation::ForumJoinSpace {
            session: string("session")?,
            space_ref: string("spaceRef")?,
            request_id: request_id("requestId")?,
            invite_ref: optional("inviteRef")?,
            view: view()?,
        }),
        "Forum_Leave_Space" => Ok(Operation::ForumLeaveSpace {
            session: string("session")?,
            space_ref: string("spaceRef")?,
            request_id: request_id("requestId")?,
            view: view()?,
        }),
        "Forum_Mark_Read" => Ok(Operation::ForumMarkRead {
            session: string("session")?,
            thread_ref: string("threadRef")?,
            read_cursor: string("readCursor")?,
            request_id: optional_request_id("requestId")?,
            view: view()?,
        }),
        "Forum_Watch" => Ok(Operation::ForumWatch { session: string("session")?, scope_ref: string("scopeRef")?, mode: watch_mode()?, view: view()? }),
        "Forum_Read_User" => Ok(Operation::ForumReadUser { session: string("session")?, user_ref: string("userRef")? }),
        "Forum_Open_Case" => Ok(Operation::ForumOpenCase {
            session: string("session")?,
            thread_ref: string("threadRef")?,
            subject_ref: string("subjectRef")?,
            reason: bounded_text("reason", 2000)?,
            view: view()?,
        }),

        "Forum_List_Cases" => Ok(Operation::ForumListCases {
            session: string("session")?,
            thread_ref: string("threadRef")?,
            page: page()?,
            view: view()?,
        }),
        "Forum_Read_Case" => Ok(Operation::ForumReadCase { session: string("session")?, case_ref: string("caseRef")?, view: view()? }),
        "Forum_Preview_Action" => Ok(Operation::ForumPreviewAction {
            session: string("session")?,
            case_ref: string("caseRef")?,
            action: case_action()?,
            summary: bounded_text("summary", 2000)?,
            deferred_until: optional("deferredUntil")?,
            expected_case_revision: revision("expectedCaseRevision")?,
            expected_subject_revision: string("expectedSubjectRevision")?,
            view: view()?,
        }),
        "Forum_Escalate_Case" => Ok(Operation::ForumEscalateCase {
            session: string("session")?,
            case_ref: string("caseRef")?,
            request_id: request_id("requestId")?,
            summary: bounded_text("summary", 2000)?,
            deferred_until: optional("deferredUntil")?,
            expected_case_revision: revision("expectedCaseRevision")?,
            expected_subject_revision: string("expectedSubjectRevision")?,
            view: view()?,
        }),
        "Forum_Manage_User" => Ok(Operation::ForumManageUser {
            session: string("session")?,
            user_ref: string("userRef")?,
            action: manage_action()?,
            reason: bounded_text("reason", 2000)?,
            duration_seconds: duration()?,
            role: optional("role")?,
            case_ref: optional("caseRef")?,
            view: view()?,
        }),
        other => Err(format!("unknown tool '{other}'")),
    }
}
