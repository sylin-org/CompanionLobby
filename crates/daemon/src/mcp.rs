//! The MCP endpoint.
//!
//! The daemon answers MCP requests that the attach shim relays from a host,
//! one JSON-RPC message per HTTP request, on behalf of one attachment. It
//! speaks the initialize-based protocol revisions (2025-11-25 and the three
//! before it): `initialize` echoes a supported revision and counteroffers the
//! latest otherwise. The stateless revision that starts with `server/discover`
//! is not served; a host that probes it receives "method not found" and falls
//! back to `initialize`, which is what the recorded hosts do.
//!
//! The identity behind every request is the attachment the shim proved with
//! the attach secret. Whatever a host says about itself in `clientInfo` is
//! attribution, never authority. Errors use the standard JSON-RPC codes only.
//!
//! A mind sees exactly the catalog the projections build for its companion,
//! and calls only what is in it. A service tool's call is journaled, made and
//! settled by the lobby; its answer is the tool's result, or an error result
//! whose text is the lobby's own sentence for the outcome, with the outcome's
//! code in the structured content. The server declares that its tool list
//! changes: the lobby tells an attached host when the companion's connections
//! change, over the attachment's notice stream (see [`crate::http`]).

use lobby_contracts::{Companion, Label, ServiceName, ToolDescriptor, ToolResult};
use serde_json::{Map, Value, json};

use crate::domain::CallRefusal;
use crate::projections::{self, Reached};

/// The notification that tells a host its tool list changed.
pub const LIST_CHANGED: &str = "notifications/tools/list_changed";

/// What an attachment's tools are, and how they are called: the lobby, for
/// one attachment.
pub trait Toolbox {
    /// What the attachment's companion reaches right now.
    fn reached(&self) -> Vec<Reached>;

    /// The connections of the attachment's companion that wait for the
    /// sponsor to sign their account in again, by service and label.
    fn waiting(&self) -> Vec<(ServiceName, Label)>;

    /// Calls the service tool `name`.
    fn call(&self, name: &str, arguments: Map<String, Value>) -> Result<ToolResult, CallRefusal>;
}

/// The notification frame that tells a host its tool list changed.
pub fn list_changed() -> Value {
    json!({ "jsonrpc": JSONRPC_VERSION, "method": LIST_CHANGED })
}

/// The JSON-RPC version every frame carries.
pub const JSONRPC_VERSION: &str = "2.0";
/// The latest protocol revision served.
pub const LATEST_PROTOCOL: &str = "2025-11-25";
/// Every protocol revision served, oldest first.
pub const PROTOCOL_VERSIONS: [&str; 4] =
    ["2024-11-05", "2025-03-26", "2025-06-18", LATEST_PROTOCOL];
/// The name the daemon reports in `serverInfo`.
pub const SERVER_NAME: &str = "companion-lobby";

/// Invalid JSON was received.
pub const PARSE_ERROR: i64 = -32700;
/// The message is not a valid request.
pub const INVALID_REQUEST: i64 = -32600;
/// The method does not exist here.
pub const METHOD_NOT_FOUND: i64 = -32601;
/// The parameters are invalid.
pub const INVALID_PARAMS: i64 = -32602;
/// The lobby failed internally.
pub const INTERNAL_ERROR: i64 = -32603;

/// Echoes a supported requested revision; counteroffers the latest otherwise.
pub fn negotiate(requested: Option<&str>) -> &'static str {
    PROTOCOL_VERSIONS
        .into_iter()
        .find(|candidate| Some(*candidate) == requested)
        .unwrap_or(LATEST_PROTOCOL)
}

/// A JSON-RPC success response.
pub fn success(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": JSONRPC_VERSION, "id": id, "result": result })
}

/// A JSON-RPC error response with a bounded message.
pub fn rpc_error(id: &Value, code: i64, message: &str) -> Value {
    let bounded: String = message.chars().take(500).collect();
    json!({ "jsonrpc": JSONRPC_VERSION, "id": id, "error": { "code": code, "message": bounded } })
}

/// Answers one message from an attachment of `companion`, whose tools are
/// `tools`. Notifications get no answer.
pub fn handle(companion: &Companion, tools: &dyn Toolbox, message: &Value) -> Option<Value> {
    let Some(object) = message.as_object() else {
        return Some(rpc_error(
            &Value::Null,
            INVALID_REQUEST,
            "A JSON-RPC message is one object.",
        ));
    };
    let id = object.get("id").cloned();
    if object.get("jsonrpc").and_then(Value::as_str) != Some(JSONRPC_VERSION) {
        return id.map(|id| rpc_error(&id, INVALID_REQUEST, "jsonrpc must be \"2.0\"."));
    }
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        // A response from the host to a server request; none are sent, so none are expected.
        return id
            .filter(|_| !object.contains_key("result") && !object.contains_key("error"))
            .map(|id| {
                rpc_error(
                    &id,
                    INVALID_REQUEST,
                    "A JSON-RPC method string is required.",
                )
            });
    };
    // Notifications never get a response.
    let id = id?;
    let params = object.get("params");
    Some(match method {
        "initialize" => initialize(companion, &id, params),
        "ping" => success(&id, json!({})),
        "tools/list" => success(
            &id,
            json!({
                "tools": projections::catalog(&tools.reached())
                    .iter()
                    .map(|(name, tool)| tool_json(name, tool))
                    .collect::<Vec<_>>()
            }),
        ),
        "tools/call" => tools_call(companion, tools, &id, params),
        _ => rpc_error(&id, METHOD_NOT_FOUND, "Method not found"),
    })
}

fn initialize(companion: &Companion, id: &Value, params: Option<&Value>) -> Value {
    let requested = params
        .and_then(|params| params.get("protocolVersion"))
        .and_then(Value::as_str);
    success(
        id,
        json!({
            "protocolVersion": negotiate(requested),
            "capabilities": { "tools": { "listChanged": true } },
            "serverInfo": {
                "name": SERVER_NAME,
                "title": "Companion Lobby",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "instructions": format!(
                "You are attached to Companion Lobby as the companion {} (moniker {}). Call reach \
                 to learn what this companion can reach and which tools act there.",
                companion.display_name, companion.moniker
            ),
        }),
    )
}

/// The MCP rendering of a descriptor under the name a mind calls it by,
/// hints derived from its effect.
pub fn tool_json(name: &str, tool: &ToolDescriptor) -> Value {
    json!({
        "name": name,
        "title": tool.title,
        "description": tool.description,
        "inputSchema": tool.input_schema,
        "annotations": {
            "title": tool.title,
            "readOnlyHint": tool.read_only_hint(),
            "destructiveHint": tool.destructive_hint(),
            "idempotentHint": tool.idempotent_hint(),
            "openWorldHint": false,
        },
    })
}

fn tools_call(
    companion: &Companion,
    tools: &dyn Toolbox,
    id: &Value,
    params: Option<&Value>,
) -> Value {
    let Some(name) = params
        .and_then(|params| params.get("name"))
        .and_then(Value::as_str)
    else {
        return rpc_error(id, INVALID_PARAMS, "tools/call needs a tool name string.");
    };
    let arguments = match params.and_then(|params| params.get("arguments")) {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(arguments)) => arguments.clone(),
        Some(_) => return rpc_error(id, INVALID_PARAMS, "arguments must be an object."),
    };
    if name != projections::REACH {
        return match tools.call(name, arguments) {
            Ok(answer) => success(id, result_json(&answer, false)),
            Err(CallRefusal::UnknownTool) => rpc_error(id, INVALID_PARAMS, "Unknown tool."),
            Err(CallRefusal::Ended(refusal)) => success(
                id,
                json!({
                    "content": [{ "type": "text", "text": refusal.message }],
                    "structuredContent": { "outcome": refusal.outcome.code() },
                    "isError": true,
                }),
            ),
        };
    }
    if !arguments.is_empty() {
        return success(
            id,
            json!({
                "content": [{ "type": "text", "text": "reach takes no arguments; call it with an empty object." }],
                "isError": true,
            }),
        );
    }
    let answer = projections::reach(companion, &tools.reached(), &tools.waiting());
    success(id, result_json(&answer, false))
}

/// The MCP result of a tool's answer: its rendered text, and its structured
/// content when it has one.
fn result_json(answer: &ToolResult, is_error: bool) -> Value {
    let mut result = json!({
        "content": [{ "type": "text", "text": answer.rendered() }],
        "isError": is_error,
    });
    if let Some(structured) = &answer.structured {
        result["structuredContent"] = structured.clone();
    }
    result
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

    /// An attachment that reaches nothing; a call to anything but `reach`
    /// names no tool.
    struct Nothing;

    impl Toolbox for Nothing {
        fn reached(&self) -> Vec<Reached> {
            Vec::new()
        }

        fn waiting(&self) -> Vec<(ServiceName, Label)> {
            Vec::new()
        }

        fn call(&self, _: &str, _: Map<String, Value>) -> Result<ToolResult, CallRefusal> {
            Err(CallRefusal::UnknownTool)
        }
    }

    fn ask(message: Value) -> Option<Value> {
        handle(&ada(), &Nothing, &message)
    }

    #[test]
    fn initialize_echoes_a_supported_revision_and_counteroffers_the_latest() {
        assert_eq!(negotiate(Some("2025-03-26")), "2025-03-26");
        assert_eq!(negotiate(Some("2030-01-01")), LATEST_PROTOCOL);
        assert_eq!(negotiate(None), LATEST_PROTOCOL);
        let answer = ask(json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}))
            .expect("answer");
        assert_eq!(answer["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(answer["result"]["serverInfo"]["name"], SERVER_NAME);
        assert_eq!(
            answer["result"]["capabilities"]["tools"]["listChanged"],
            true
        );
        assert!(
            answer["result"]["instructions"]
                .as_str()
                .expect("text")
                .contains("ada")
        );
    }

    #[test]
    fn notifications_get_no_answer_and_ping_gets_an_empty_result() {
        assert!(ask(json!({"jsonrpc":"2.0","method":"notifications/initialized"})).is_none());
        assert!(
            ask(
                json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1}})
            )
            .is_none()
        );
        assert_eq!(
            ask(json!({"jsonrpc":"2.0","id":"p","method":"ping"})).expect("pong")["result"],
            json!({})
        );
    }

    #[test]
    fn the_stateless_probe_and_unknown_methods_are_not_found() {
        for method in [
            "server/discover",
            "resources/list",
            "prompts/list",
            "subscriptions/listen",
        ] {
            let answer = ask(json!({"jsonrpc":"2.0","id":1,"method":method})).expect("answer");
            assert_eq!(answer["error"]["code"], METHOD_NOT_FOUND, "{method}");
        }
    }

    #[test]
    fn malformed_messages_are_invalid_requests() {
        assert_eq!(
            ask(json!([1, 2])).expect("answer")["error"]["code"],
            INVALID_REQUEST
        );
        assert_eq!(
            ask(json!({"jsonrpc":"1.0","id":1,"method":"ping"})).expect("answer")["error"]["code"],
            INVALID_REQUEST
        );
        assert_eq!(
            ask(json!({"jsonrpc":"2.0","id":1})).expect("answer")["error"]["code"],
            INVALID_REQUEST
        );
        assert!(ask(json!({"jsonrpc":"2.0","id":1,"result":{}})).is_none());
    }

    #[test]
    fn tools_list_and_call_serve_reach() {
        let list = ask(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"})).expect("list");
        let tools = list["result"]["tools"].as_array().expect("tools");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "reach");
        assert_eq!(tools[0]["annotations"]["readOnlyHint"], true);

        let call = ask(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"reach","arguments":{}}}))
            .expect("call");
        assert_eq!(call["result"]["isError"], false);
        assert_eq!(
            call["result"]["structuredContent"]["companion"]["moniker"],
            "ada"
        );

        let unknown = ask(json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"board_post","arguments":{}}}))
            .expect("unknown");
        assert_eq!(unknown["error"]["code"], INVALID_PARAMS);
        let extra = ask(json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"reach","arguments":{"x":1}}}))
            .expect("extra");
        assert_eq!(extra["result"]["isError"], true);
        let bad = ask(json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"reach","arguments":[]}}))
            .expect("bad");
        assert_eq!(bad["error"]["code"], INVALID_PARAMS);
    }

    #[test]
    fn error_text_is_bounded() {
        let long = "x".repeat(2000);
        let error = rpc_error(&json!(1), INTERNAL_ERROR, &long);
        assert_eq!(error["error"]["message"].as_str().expect("text").len(), 500);
    }
}
