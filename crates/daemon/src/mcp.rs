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
//! A mind sees exactly the catalog the projections build for its companion.

use lobby_contracts::{Companion, ToolDescriptor};
use serde_json::{Map, Value, json};

use crate::projections;

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

/// Answers one message from an attachment of `companion`. Notifications get
/// no answer.
pub fn handle(companion: &Companion, message: &Value) -> Option<Value> {
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
            json!({ "tools": projections::catalog(companion).iter().map(tool_json).collect::<Vec<_>>() }),
        ),
        "tools/call" => tools_call(companion, &id, params),
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
            "capabilities": { "tools": { "listChanged": false } },
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

/// The MCP rendering of a descriptor, hints derived from its effect.
pub fn tool_json(tool: &ToolDescriptor) -> Value {
    json!({
        "name": tool.name.as_str(),
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

fn tools_call(companion: &Companion, id: &Value, params: Option<&Value>) -> Value {
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
        return rpc_error(id, INVALID_PARAMS, "Unknown tool.");
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
    let answer = projections::reach(companion);
    let mut result = json!({
        "content": [{ "type": "text", "text": answer.text }],
        "isError": false,
    });
    if let Some(structured) = answer.structured {
        result["structuredContent"] = structured;
    }
    success(id, result)
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

    fn ask(message: Value) -> Option<Value> {
        handle(&ada(), &message)
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
