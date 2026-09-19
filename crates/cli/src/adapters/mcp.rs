//! The MCP spoke: newline-delimited JSON-RPC 2.0 over stdio. The edge owns protocol framing,
//! negotiation and error codes only — every tool outcome comes from the hub, so the CLI and
//! MCP intakes observe identical domain behavior. The protocol surface (bounded framing,
//! multi-revision negotiation, stateless `server/discover`) is held to what real MCP
//! hosts exercise.

use std::collections::BTreeSet;
use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};

use crate::application::hub::ConnectorHub;
use companion_core::domain::intake::IntakeChannel;

pub const PROTOCOL_VERSION: &str = "2025-11-25";
pub const SUPPORTED_PROTOCOL_VERSIONS: [&str; 4] = ["2024-11-05", "2025-03-26", "2025-06-18", PROTOCOL_VERSION];
pub const STATELESS_DISCOVER_VERSION: &str = "2026-07-28";
const JSONRPC_VERSION: &str = "2.0";
const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
const SERVER_NAME: &str = "companion-lobby";

const INSTRUCTIONS: &str = "Your keys are instruments; your charter governs their use. \
ListCompanions names who you may be; Connect mints a labeled session and briefs you; \
WhoAmI re-anchors; CatchUp is your one inbox. Copy references, cursors and session \
handles from responses; never construct them. A requestId is an idempotency key: reuse \
it to reconcile an uncertain outcome — a conflict answer means already done. A mention \
requests attention; it never obliges you to accept work. Quiet reading is always fine.";

/// The session argument every ring key carries.
fn session_arg() -> Value {
    json!({
        "type": "string",
        "description": "The labeled session handle returned by Connect (e.g. tangent_9f01). Copy it; never construct it."
    })
}
fn view_arg() -> Value {
    json!({ "type": "string", "enum": ["orientation", "compact", "expanded"] })
}
fn request_id_arg() -> Value {
    json!({
        "type": "string",
        "minLength": 1,
        "maxLength": 128,
        "description": "Your idempotency key for this write: reuse it to reconcile an uncertain outcome."
    })
}
const REF_COPY: &str = "Copied from a response of this server; never constructed.";


/// Echoes a supported requested revision; counteroffers the latest otherwise.
pub fn negotiate_protocol_version(requested: &str) -> &'static str {
    SUPPORTED_PROTOCOL_VERSIONS
        .into_iter()
        .find(|candidate| *candidate == requested)
        .unwrap_or(PROTOCOL_VERSION)
}

/// Serves the MCP edge until stdin ends. Returns a process exit code.
///
/// The hub is constructed when the first `initialize` request names the connecting
/// client: `clientInfo.name` becomes the caller (`mcp:{name}`), which labels feed
/// attribution and scopes context binding. Attribution only — it never changes a
/// domain outcome — and the per-process single-caller rule is unchanged: one process
/// serves exactly one client.
pub fn serve(
    build_hub: impl FnOnce(&str) -> Result<Arc<ConnectorHub>, String>,
    output: &mut dyn Write,
) -> i32 {
    let initialized = Arc::new(AtomicBool::new(false));
    let mut builder = Some(build_hub);
    let mut hub: Option<Arc<ConnectorHub>> = None;
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let mut buffer = String::new();
    loop {
        buffer.clear();
        match read_line(&mut reader, &mut buffer) {
            Ok(0) => return 0,
            Ok(_length) => {}
            Err(frame_error) => {
                let _ = write_json(output, &rpc_error(Value::Null, -32700, &frame_error.to_string()));
                if matches!(frame_error, FrameError::Oversized) {
                    return 0; // The stream is unsyncable after an oversized line.
                }
                continue;
            }
        }
        let line = buffer.trim();
        if line.is_empty() {
            continue;
        }
        let message: Value = match serde_json::from_str(line) {
            Ok(message) => message,
            Err(_) => {
                let _ = write_json(output, &rpc_error(Value::Null, -32700, "Parse error"));
                continue;
            }
        };
        let method = message.get("method").and_then(Value::as_str).map(str::to_string);
        let id = message.get("id").cloned();
        let Some(method) = method else {
            if let Some(id_value) = id {
                let _ = write_json(output, &rpc_error(id_value, -32600, "A JSON-RPC method string is required."));
            }
            continue;
        };
        // Notifications (no id) never get a response.
        let Some(id) = id else {
            if method == "notifications/initialized" {
                initialized.store(true, Ordering::SeqCst);
            }
            continue;
        };
        if method == "initialize" && hub.is_none() {
            let client_name = client_name_of(&message);
            match builder.take().map(|build| build(&client_name)) {
                Some(Ok(built)) => hub = Some(built),
                Some(Err(error)) => {
                    let _ = write_json(output, &rpc_error(id, -32603, &format!("cannot open connector state: {error}")));
                    return 4;
                }
                None => unreachable!("builder exists while no hub does"),
            }
        }
        let tools_before = (method == "tools/call")
            .then(|| hub.as_deref().map(ConnectorHub::optional_tool_names).unwrap_or_default());
        let response = handle_request(hub.as_deref(), &method, &message, &id, &initialized);
        if let Some(response) = response {
            if write_json(output, &response).is_err() {
                return 0;
            }
        }
        if let Some(before) = tools_before {
            let after = hub.as_deref().map(ConnectorHub::optional_tool_names).unwrap_or_default();
            if before != after && write_json(output, &json!({
                "jsonrpc": JSONRPC_VERSION, "method": "notifications/tools/list_changed"
            })).is_err() { return 0; }
        }
    }
}

/// The bounded `clientInfo.name` of the connecting client. Attribution only; never an
/// input to a domain decision.
fn client_name_of(message: &Value) -> String {
    message
        .pointer("/params/clientInfo/name")
        .and_then(Value::as_str)
        .unwrap_or("unknown-client")
        .chars()
        .take(100)
        .collect()
}

fn handle_request(
    hub: Option<&ConnectorHub>,
    method: &str,
    message: &Value,
    id: &Value,
    initialized: &AtomicBool,
) -> Option<Value> {
    match method {
        "initialize" => Some(initialize(message, id)),
        "ping" => Some(success(id, json!({}))),
        "tools/list" => {
            if !ready(initialized, hub) {
                return Some(rpc_error(id.clone(), -32002, "Server not initialized"));
            }
            let granted = hub.map(ConnectorHub::granted_service_monikers).unwrap_or_default();
            let steward = hub.map(ConnectorHub::optional_tool_names).unwrap_or_default();
            Some(success(id, json!({ "tools": catalog(&granted, &steward) })))
        }
        "tools/call" => {
            let Some(hub) = hub else {
                return Some(rpc_error(id.clone(), -32002, "Server not initialized"));
            };
            if !initialized.load(Ordering::SeqCst) {
                return Some(rpc_error(id.clone(), -32002, "Server not initialized"));
            }
            Some(tools_call(hub, message, id))
        }
        // Tools-only server: hosts that probe resources see an honest empty set.
        "resources/list" | "resources/templates/list" => Some(success(id, json!({ "resources": [] }))),
        // Stateless discovery (2026-07-28 family): answer with the supported set.
        "server/discover" => Some(discover(message, id)),
        _ => Some(rpc_error(id.clone(), -32601, "Method not found")),
    }
}

fn ready(initialized: &AtomicBool, hub: Option<&ConnectorHub>) -> bool {
    initialized.load(Ordering::SeqCst) && hub.is_some()
}

fn initialize(message: &Value, id: &Value) -> Value {
    let requested = message
        .pointer("/params/protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or(PROTOCOL_VERSION);
    let negotiated = negotiate_protocol_version(requested);
    success(
        id,
        json!({
            "protocolVersion": negotiated,
            "capabilities": { "tools": { "listChanged": true } },
            "serverInfo": { "name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION") },
            "instructions": INSTRUCTIONS,
        }),
    )
}

fn discover(message: &Value, id: &Value) -> Value {
    let requested = message
        .pointer("/params/_meta/io.modelcontextprotocol~1protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or(STATELESS_DISCOVER_VERSION);
    success(
        id,
        json!({
            "supportedVersions": SUPPORTED_PROTOCOL_VERSIONS,
            "protocolVersion": requested,
            "resultType": "complete",
            "ttlMs": 0,
            "cacheScope": "private",
            "serverInfo": { "name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION") },
            "capabilities": { "tools": { "listChanged": true } },
        }),
    )
}

fn tools_call(hub: &ConnectorHub, message: &Value, id: &Value) -> Value {
    let name = message.pointer("/params/name").and_then(Value::as_str).map(str::to_string);
    let arguments = message.pointer("/params/arguments").cloned().unwrap_or_else(|| json!({}));
    let Some(name) = name else {
        return rpc_error(id.clone(), -32602, "tools/call requires a tool name string.");
    };
    if !arguments.is_object() {
        return rpc_error(id.clone(), -32602, "arguments must be an object.");
    }
    let outcome = hub.invoke(IntakeChannel::Mcp, &name, &arguments);
    success(
        id,
        json!({
            "content": [ { "type": "text", "text": outcome.text } ],
            "structuredContent": outcome.structured,
            "isError": outcome.is_error,
        }),
    )
}

fn success(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": JSONRPC_VERSION, "id": id, "result": result })
}

fn rpc_error(id: Value, code: i32, message: &str) -> Value {
    // Bounded error text, like every untrusted string crossing the edge.
    let bounded: String = message.chars().take(500).collect();
    json!({ "jsonrpc": JSONRPC_VERSION, "id": id, "error": { "code": code, "message": bounded } })
}

fn write_json(output: &mut dyn Write, value: &Value) -> std::io::Result<()> {
    output.write_all(serde_json::to_string(value)?.as_bytes())?;
    output.write_all(b"\n")?;
    output.flush()
}

#[derive(Debug, thiserror::Error)]
enum FrameError {
    #[error("frame exceeds 8 MiB")]
    Oversized,
    #[error("stream read failed")]
    Read,
}

fn read_line(reader: &mut impl BufRead, buffer: &mut String) -> Result<usize, FrameError> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => {
                if bytes.is_empty() {
                    return Ok(0);
                }
                break;
            }
            Ok(count) => {
                bytes.extend_from_slice(&chunk[..count]);
                if bytes.len() > MAX_FRAME_BYTES {
                    return Err(FrameError::Oversized);
                }
                if bytes.contains(&b'\n') {
                    break;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(FrameError::Read),
        }
    }
    let end = bytes.iter().position(|byte| *byte == b'\n').unwrap_or(bytes.len());
    buffer.push_str(&String::from_utf8_lossy(&bytes[..end]));
    Ok(buffer.len())
}

/// The stable tool catalog. Fourteen participation tools; setup and stewardship stay in the CLI.
fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema,
        "annotations": {
            "title": name,
            "readOnlyHint": matches!(name, "ListCompanions" | "Connect" | "WhoAmI" | "CatchUp" | "Forum_List_Spaces" | "Forum_List_Threads" | "Forum_Read_Thread" | "Forum_Read_User" | "Forum_List_Cases" | "Forum_Read_Case" | "Forum_Preview_Action"),
        }
    })
}

/// The projected catalog: core keys always; a ring's keys iff a granted service
/// speaks it and the wire supports them; stewardship keys iff a live context
/// reported the authority; `Forum_Manage_User`'s action enum carries that authority.
/// Never declared — only projected (ADR 0001).
pub fn catalog(granted: &BTreeSet<String>, steward: &BTreeSet<String>) -> Value {
    let mut tools: Vec<Value> = Vec::new();

    // ----- the core -----
    tools.push(tool(
        "ListCompanions",
        "Who exists, and what each persona may reach (its granted services). The first call, before any session exists.",
        json!({ "type": "object", "properties": {}, "additionalProperties": false }),
    ));
    tools.push(tool(
        "Connect",
        "The only mint of a session: connect to a service as a persona, always explicitly named. For multi-place services (forums) the address names which place. Returns the labeled session (e.g. tangent_9f01) and the briefing: You are {persona} — session {id}.",
        json!({
            "type": "object",
            "properties": {
                "service": { "type": "string", "description": "The service moniker: tangent (forums) — bluesky when granted." },
                "persona": { "type": "string", "description": "The companion to act as, by moniker (ListCompanions names them)." },
                "address": { "type": "string", "description": "The place's https origin. Required for tangent; absent for single-instance services." },
            },
            "required": ["service", "persona"],
            "additionalProperties": false,
        }),
    ));
    tools.push(tool(
        "WhoAmI",
        "Identity facts and the live capability readout for one session: who you are, where you are, what is on your ring — as of now.",
        json!({
            "type": "object",
            "properties": { "session": session_arg() },
            "required": ["session"],
            "additionalProperties": false,
        }),
    ));
    tools.push(tool(
        "CatchUp",
        "One inbox: mentions, watched activity, and the settlement of your own writes, cursor'd from where you left off.",
        json!({
            "type": "object",
            "properties": { "session": session_arg(), "cursor": { "type": "string" }, "view": view_arg() },
            "required": ["session"],
            "additionalProperties": false,
        }),
    ));

    // ----- the Forum ring, projected by grant and wire support -----
    if granted.contains("tangent") {
        let session = session_arg();
        tools.push(tool(
            "Forum_List_Spaces",
            "The Tangents (spaces) visible to this session.",
            json!({ "type": "object", "properties": { "session": session, "cursor": { "type": "string" } }, "required": ["session"], "additionalProperties": false }),
        ));
        let session = session_arg();
        tools.push(tool(
            "Forum_List_Threads",
            "The threads (topics) of one space.",
            json!({ "type": "object", "properties": { "session": session, "spaceRef": { "type": "string", "description": REF_COPY }, "cursor": { "type": "string" } }, "required": ["session", "spaceRef"], "additionalProperties": false }),
        ));
        let session = session_arg();
        tools.push(tool(
            "Forum_Read_Thread",
            "A window of posts in one thread; aroundPostRef gathers the evidence around one post.",
            json!({ "type": "object", "properties": { "session": session, "threadRef": { "type": "string", "description": REF_COPY }, "cursor": { "type": "string" }, "aroundPostRef": { "type": "string", "description": REF_COPY }, "limit": { "type": "integer", "minimum": 1, "maximum": 25 }, "view": view_arg() }, "required": ["session", "threadRef"], "additionalProperties": false }),
        ));
        let session = session_arg();
        tools.push(tool(
            "Forum_Post",
            "The write key: post into a thread, optionally as a reply. requestId is the idempotency key — reuse it to reconcile an uncertain outcome; a conflict answer means already done.",
            json!({ "type": "object", "properties": { "session": session, "threadRef": { "type": "string", "description": REF_COPY }, "requestId": request_id_arg(), "text": { "type": "string", "minLength": 1, "maxLength": 4096 }, "replyTo": { "type": "string", "description": REF_COPY }, "view": view_arg() }, "required": ["session", "threadRef", "requestId", "text"], "additionalProperties": false }),
        ));
        let session = session_arg();
        tools.push(tool(
            "Forum_Join_Space",
            "Become a member of a space.",
            json!({ "type": "object", "properties": { "session": session, "spaceRef": { "type": "string", "description": REF_COPY }, "requestId": request_id_arg(), "inviteRef": { "type": "string", "description": REF_COPY }, "view": view_arg() }, "required": ["session", "spaceRef", "requestId"], "additionalProperties": false }),
        ));
        let session = session_arg();
        tools.push(tool(
            "Forum_Leave_Space",
            "End membership of a space.",
            json!({ "type": "object", "properties": { "session": session, "spaceRef": { "type": "string", "description": REF_COPY }, "requestId": request_id_arg(), "view": view_arg() }, "required": ["session", "spaceRef", "requestId"], "additionalProperties": false }),
        ));
        let session = session_arg();
        tools.push(tool(
            "Forum_Mark_Read",
            "Advance the read state of one thread to a cursor copied from a response.",
            json!({ "type": "object", "properties": { "session": session, "threadRef": { "type": "string", "description": REF_COPY }, "readCursor": { "type": "string" }, "requestId": request_id_arg(), "view": view_arg() }, "required": ["session", "threadRef", "readCursor"], "additionalProperties": false }),
        ));
        let session = session_arg();
        tools.push(tool(
            "Forum_Watch",
            "Hold attention on one space or thread: all activity, replies to you, or none.",
            json!({ "type": "object", "properties": { "session": session, "scopeRef": { "type": "string", "description": REF_COPY }, "mode": { "type": "string", "enum": ["all", "replies", "none"] }, "view": view_arg() }, "required": ["session", "scopeRef", "mode"], "additionalProperties": false }),
        ));
        let session = session_arg();
        tools.push(tool(
            "Forum_Open_Case",
            "Reporting is a member ability: flag a post into the community's case system.",
            json!({ "type": "object", "properties": { "session": session, "threadRef": { "type": "string", "description": REF_COPY }, "subjectRef": { "type": "string", "description": REF_COPY }, "reason": { "type": "string", "minLength": 1, "maxLength": 2000 }, "view": view_arg() }, "required": ["session", "threadRef", "subjectRef", "reason"], "additionalProperties": false }),
        ));

        // ----- stewardship, projected by the authority live contexts reported -----
        if !steward.is_empty() {
            if steward.contains("Forum_List_Cases") {
                let session = session_arg();
                tools.push(tool(
                    "Forum_List_Cases",
                    "One bounded page of the community's moderation cases in an authorized thread.",
                    json!({ "type": "object", "properties": { "session": session, "threadRef": { "type": "string", "description": REF_COPY }, "page": { "type": "integer", "minimum": 0 }, "view": view_arg() }, "required": ["session", "threadRef"], "additionalProperties": false }),
                ));
            }
            if steward.contains("Forum_Read_Case") {
                let session = session_arg();
                tools.push(tool(
                    "Forum_Read_Case",
                    "One case's evidence and state.",
                    json!({ "type": "object", "properties": { "session": session, "caseRef": { "type": "string", "description": REF_COPY }, "view": view_arg() }, "required": ["session", "caseRef"], "additionalProperties": false }),
                ));
            }
            if steward.contains("Forum_Preview_Action") {
                let session = session_arg();
                tools.push(tool(
                    "Forum_Preview_Action",
                    "The service's rules engine reporting what a case action would do — facts, never advice.",
                    json!({ "type": "object", "properties": { "session": session, "caseRef": { "type": "string", "description": REF_COPY }, "action": { "type": "string", "enum": ["defer", "escalate"] }, "summary": { "type": "string", "minLength": 1, "maxLength": 2000 }, "deferredUntil": { "type": "string" }, "expectedCaseRevision": { "type": "integer", "minimum": 0 }, "expectedSubjectRevision": { "type": "string" }, "view": view_arg() }, "required": ["session", "caseRef", "action", "summary", "expectedCaseRevision", "expectedSubjectRevision"], "additionalProperties": false }),
                ));
            }
            if steward.contains("Forum_Escalate_Case") {
                let session = session_arg();
                tools.push(tool(
                    "Forum_Escalate_Case",
                    "File a case to the owner. Deliberation itself stays conversational; this is filing.",
                    json!({ "type": "object", "properties": { "session": session, "caseRef": { "type": "string", "description": REF_COPY }, "requestId": request_id_arg(), "summary": { "type": "string", "minLength": 1, "maxLength": 2000 }, "deferredUntil": { "type": "string" }, "expectedCaseRevision": { "type": "integer", "minimum": 0 }, "expectedSubjectRevision": { "type": "string" }, "view": view_arg() }, "required": ["session", "caseRef", "requestId", "summary", "expectedCaseRevision", "expectedSubjectRevision"], "additionalProperties": false }),
                ));
            }
            let manage: Vec<String> = steward.iter().filter_map(|name| name.strip_prefix("manage:")).map(str::to_string).collect();
            if !manage.is_empty() {
                tools.push(manage_tool(&manage));
            }
        }
    }

    json!(tools)
}

/// `Forum_Manage_User`: the user-management ladder in one key. The action enum IS
/// the authority — a schema that cannot express `ban` cannot ban.
fn manage_tool(actions: &[String]) -> Value {
    let mut schema = json!({
        "type": "object",
        "properties": {
            "session": session_arg(),
            "userRef": { "type": "string", "description": REF_COPY },
            "action": { "type": "string", "enum": actions },
            "reason": { "type": "string", "minLength": 1, "maxLength": 2000 },
            "durationSeconds": { "type": "integer", "minimum": 0, "description": "For the rungs that allow it (timeout, ban)." },
            "role": { "type": "string", "description": "For add_role / remove_role." },
            "caseRef": { "type": "string", "description": "Binds the act to its evidence when one exists." },
            "view": view_arg(),
        },
        "required": ["session", "userRef", "action", "reason"],
        "additionalProperties": false,
    });
    let _ = &mut schema;
    let mut entry = tool(
        "Forum_Manage_User",
        "The user-management ladder: warn, timeout, suspend, ban, add_role, remove_role. The action enum carries the authority this identity holds; the companion's charter governs when to climb it.",
        schema,
    );
    entry["annotations"]["destructiveHint"] = json!(true);
    entry["annotations"]["idempotentHint"] = json!(false);
    entry
}


#[cfg(test)]
mod tests {
    use super::*;

    fn granted(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn known_revisions_echo_and_future_ones_counteroffer() {
        assert_eq!(negotiate_protocol_version("2024-11-05"), "2024-11-05");
        assert_eq!(negotiate_protocol_version("2025-06-18"), "2025-06-18");
        assert_eq!(negotiate_protocol_version("2030-01-01"), PROTOCOL_VERSION);
        assert_eq!(negotiate_protocol_version("garbage"), PROTOCOL_VERSION);
    }

    #[test]
    fn a_member_sees_the_core_and_the_forum_ring_alone() {
        let tools = catalog(&granted(&["tangent"]), &BTreeSet::new());
        let names: Vec<&str> = tools.as_array().unwrap().iter()
            .filter_map(|entry| entry.get("name").and_then(Value::as_str)).collect();
        assert_eq!(names, vec![
            "ListCompanions", "Connect", "WhoAmI", "CatchUp",
            "Forum_List_Spaces", "Forum_List_Threads", "Forum_Read_Thread", "Forum_Post",
            "Forum_Join_Space", "Forum_Leave_Space", "Forum_Mark_Read", "Forum_Watch",
            "Forum_Open_Case",
        ]);
        for entry in tools.as_array().unwrap() {
            assert!(entry.get("inputSchema").is_some(), "every tool carries a schema");
        }
    }

    #[test]
    fn no_grant_no_ring_and_the_social_ring_waits_for_its_service() {
        let tools = catalog(&BTreeSet::new(), &BTreeSet::new());
        let names: Vec<&str> = tools.as_array().unwrap().iter()
            .filter_map(|entry| entry.get("name").and_then(Value::as_str)).collect();
        assert_eq!(names, vec!["ListCompanions", "Connect", "WhoAmI", "CatchUp"]);

        let tools = catalog(&granted(&["tangent", "bluesky"]), &BTreeSet::new());
        let names: Vec<&str> = tools.as_array().unwrap().iter()
            .filter_map(|entry| entry.get("name").and_then(Value::as_str)).collect();
        assert_eq!(names, vec!["ListCompanions", "Connect", "WhoAmI", "CatchUp",
            "Forum_List_Spaces", "Forum_List_Threads", "Forum_Read_Thread", "Forum_Post",
            "Forum_Join_Space", "Forum_Leave_Space", "Forum_Mark_Read", "Forum_Watch",
            "Forum_Open_Case"],
            "the Social ring appears only when a Social service speaks it");
    }

    #[test]
    fn the_manage_enum_is_the_authority_and_keys_project_by_it() {
        let steward = granted(&["Forum_List_Cases", "Forum_Read_Case", "Forum_Preview_Action", "Forum_Escalate_Case",
            "manage:warn", "manage:timeout", "manage:ban"]);
        let tools = catalog(&granted(&["tangent"]), &steward);
        let entries = tools.as_array().unwrap();
        assert_eq!(entries.len(), 13 + 5, "four case keys and the manage key join the member ring");
        let manage = entries.iter().find(|entry| entry["name"] == "Forum_Manage_User").unwrap();
        assert_eq!(manage["inputSchema"]["properties"]["action"]["enum"], json!(["ban", "timeout", "warn"]),
            "the enum carries exactly the reported authority, sorted and no wider");
        assert_eq!(manage["annotations"]["destructiveHint"], true);

        // A warn-only steward cannot express ban; the case keys project independently.
        let steward = granted(&["manage:warn"]);
        let tools = catalog(&granted(&["tangent"]), &steward);
        let manage = tools.as_array().unwrap().iter()
            .find(|entry| entry["name"] == "Forum_Manage_User").unwrap();
        assert_eq!(manage["inputSchema"]["properties"]["action"]["enum"], json!(["warn"]));
        assert!(!tools.as_array().unwrap().iter().any(|entry| entry["name"] == "Forum_List_Cases"),
            "case keys appear only when their authority was reported");
    }
}
