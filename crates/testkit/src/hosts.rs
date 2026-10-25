//! MCP conversations recorded from real hosts.
//!
//! Each fixture is the JSON-lines record `companion-lobby attach --log`
//! writes, reduced to the frames: what the host sent (`host`) and what the
//! lobby answered (`lobby`), in order, with the milliseconds since the shim
//! started. They are replayed against the attach shim so a change that would
//! break a real host fails a test first.

use serde_json::Value;

/// Who sent a recorded frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// The MCP host sent it to the shim.
    Host,
    /// The lobby answered it through the shim.
    Lobby,
}

/// One recorded frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    /// Who sent it.
    pub direction: Direction,
    /// Milliseconds since the shim started.
    pub ms: u64,
    /// The JSON-RPC frame.
    pub frame: Value,
}

/// A recorded conversation.
#[derive(Debug, Clone, PartialEq)]
pub struct Recording {
    /// The host and its version, as it named itself.
    pub host: &'static str,
    /// The frames, in order.
    pub frames: Vec<Frame>,
}

impl Recording {
    /// The frames the host sent.
    pub fn sent(&self) -> impl Iterator<Item = &Value> {
        self.frames
            .iter()
            .filter(|frame| frame.direction == Direction::Host)
            .map(|frame| &frame.frame)
    }

    /// The lobby's recorded answer to the request with `id`.
    pub fn answer_to(&self, id: &Value) -> Option<&Value> {
        self.frames
            .iter()
            .filter(|frame| frame.direction == Direction::Lobby)
            .map(|frame| &frame.frame)
            .find(|frame| frame.get("id") == Some(id))
    }
}

fn parse(host: &'static str, text: &str) -> Recording {
    let frames = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let record: Value = serde_json::from_str(line).expect("a fixture line is JSON");
            let direction = match record["dir"].as_str() {
                Some("host") => Direction::Host,
                Some("lobby") => Direction::Lobby,
                other => panic!("unexpected direction {other:?} in a host fixture"),
            };
            Frame {
                direction,
                ms: record["ms"].as_u64().unwrap_or(0),
                frame: record["frame"].clone(),
            }
        })
        .collect();
    Recording { host, frames }
}

/// Claude Code 2.1.303 on Windows, run headless with `claude -p`, attaching
/// through the shim, listing tools and calling `reach` once.
///
/// It probes the stateless revision with `server/discover` first, falls back
/// to `initialize` at 2025-11-25 on "method not found", and sends
/// `notifications/initialized` before `tools/list`.
pub fn claude_code() -> Recording {
    parse(
        "claude-code 2.1.303",
        include_str!("../fixtures/hosts/claude-code-2.1.303.jsonl"),
    )
}

/// Claude Code 2.1.303 on Windows, run headless with `claude -p` and the
/// haiku model, attached as Ada to a lobby whose only connection is the
/// test kit's fake Space in `lobby-world`, given the task: find out where
/// you can go, read what is new, and reply to the newest conversation in
/// one friendly sentence.
///
/// The mind calls `reach`, which names its account and how the Space knows
/// it, asks the Space what is new and learns who wrote each Post by name,
/// reads the Topic of the newest Post, replies to Bea's Post there with a
/// request id of its own choosing and marks the Topic read, with no refused
/// call and every reference copied from an earlier answer. Its host showed it each result's structured
/// content, which held the answer's text as `message`. The Space's origin
/// in the frames is the one that walk ran against; a replay puts its own
/// in its place.
pub fn claude_code_in_a_space() -> Recording {
    parse(
        "claude-code 2.1.303",
        include_str!("../fixtures/hosts/claude-code-2.1.303-space.jsonl"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_claude_code_recording_probes_then_initializes_then_calls_reach() {
        let recording = claude_code();
        let methods: Vec<&str> = recording
            .sent()
            .filter_map(|frame| frame["method"].as_str())
            .collect();
        assert_eq!(
            methods,
            [
                "server/discover",
                "initialize",
                "notifications/initialized",
                "tools/list",
                "tools/call"
            ]
        );
        let initialize = recording
            .sent()
            .find(|frame| frame["method"] == "initialize")
            .expect("initialize");
        assert_eq!(initialize["params"]["protocolVersion"], "2025-11-25");
        let probe = recording
            .answer_to(&Value::from("server-discover-probe-1"))
            .expect("probe answer");
        assert_eq!(probe["error"]["code"], -32601);
    }

    #[test]
    fn the_space_recording_reaches_hears_what_is_new_reads_and_replies_with_no_wrong_turn() {
        let recording = claude_code_in_a_space();
        let calls: Vec<&str> = recording
            .sent()
            .filter(|frame| frame["method"] == "tools/call")
            .filter_map(|frame| frame["params"]["name"].as_str())
            .collect();
        assert_eq!(
            calls,
            [
                "reach",
                "tangent_updates",
                "tangent_read_topic",
                "tangent_post",
                "tangent_mark_read"
            ]
        );
        for frame in recording
            .sent()
            .filter(|frame| frame["method"] == "tools/call")
        {
            let answer = recording.answer_to(&frame["id"]).expect("an answer");
            assert_eq!(answer["result"]["isError"], false, "{answer}");
        }
        let post = recording
            .sent()
            .find(|frame| frame["params"]["name"] == "tangent_post")
            .expect("the reply");
        let arguments = &post["params"]["arguments"];
        assert!(arguments["reply_to"].is_string() && arguments["request_id"].is_string());

        // Every reference the mind passed was copied from an answer the
        // lobby gave before it, never derived or guessed.
        let mut answered = String::new();
        for frame in &recording.frames {
            match frame.direction {
                Direction::Lobby => answered.push_str(&frame.frame.to_string()),
                Direction::Host if frame.frame["method"] == "tools/call" => {
                    let arguments = &frame.frame["params"]["arguments"];
                    for name in [
                        "topic",
                        "reply_to",
                        "tangent",
                        "read_cursor",
                        "scope",
                        "around",
                    ] {
                        if let Some(reference) = arguments[name].as_str() {
                            assert!(
                                answered.contains(&Value::String(reference.to_owned()).to_string()),
                                "{name} {reference} was given before the mind used it"
                            );
                        }
                    }
                }
                Direction::Host => {}
            }
        }
        let reach = recording
            .sent()
            .find(|frame| frame["params"]["name"] == "reach")
            .and_then(|frame| recording.answer_to(&frame["id"]))
            .expect("reach");
        let connection = &reach["result"]["structuredContent"]["connections"][0];
        assert_eq!(
            connection["knownAs"]["quoted"]["name"], "Ada",
            "reach told the mind the name the Space knows it by"
        );
        assert_eq!(connection["allows"], "read and post");
        let updates = recording
            .sent()
            .find(|frame| frame["params"]["name"] == "tangent_updates")
            .and_then(|frame| recording.answer_to(&frame["id"]))
            .expect("what is new");
        assert_eq!(
            updates["result"]["structuredContent"]["attention"]["items"][0]["quoted"]["actorName"],
            "Bea",
            "the mind read who wrote the newest Post by name"
        );
        for frame in recording
            .sent()
            .filter(|frame| frame["method"] == "tools/call")
        {
            let answer = recording.answer_to(&frame["id"]).expect("an answer");
            assert_eq!(
                answer["result"]["structuredContent"]["message"],
                answer["result"]["content"][0]["text"],
                "the structured form the host showed held the text too"
            );
        }
    }
}
