//! The Tangent adapter's requests on the wire and the answers it does not
//! get, against loopback listeners that play a Space.

use std::io::{BufRead, BufReader};
use std::net::TcpListener;

use lobby_contracts::{Address, AddressPolicy, Effect, Outcome, Retry, Secret, ToolArguments};
use lobby_outbound::Outbound;
use lobby_service_tangent::issued::Issued;
use lobby_service_tangent::tools::{self, Through};
use lobby_service_tangent::wire::{self, Failed};
use lobby_testkit::listener::{Listener, Reply};
use serde_json::{Value, json};

fn issued() -> Issued {
    Issued {
        participant: "prt_1".to_owned(),
        credential: "cred-1".to_owned(),
        token: Secret::new("ts_secret"),
        expires_at: i64::MAX,
        grants: vec!["welcome".to_owned(), "read".to_owned(), "post".to_owned()],
    }
}

fn arguments(value: Value) -> ToolArguments {
    ToolArguments(value.as_object().cloned().unwrap_or_default())
}

fn perform(address: &Address, name: &str, value: Value) -> Result<String, Failed> {
    let outbound = Outbound::new(AddressPolicy::LoopbackAllowed);
    let issued = issued();
    tools::perform(
        tools::tool(name).expect("a tool"),
        &arguments(value),
        &Through {
            outbound: &outbound,
            address,
            issued: &issued,
        },
    )
    .map(|result| result.rendered())
}

/// A Space that reads one request whole and closes without answering, as
/// one whose answer is lost on the way.
fn losing() -> Address {
    let socket = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = socket.local_addr().expect("address").port();
    std::thread::spawn(move || {
        for stream in socket.incoming().flatten().take(1) {
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 && line != "\r\n" {
                line.clear();
            }
        }
    });
    Address::parse(
        &format!("http://127.0.0.1:{port}"),
        AddressPolicy::LoopbackAllowed,
    )
    .expect("address")
}

#[test]
fn a_read_asks_its_route_with_the_token_in_the_header_only_and_a_bounded_page() {
    let listener = Listener::start(|request| {
        let origin = format!("http://{}", request.header("host").unwrap_or_default());
        let topic = format!("{origin}::home::welcome");
        Reply::json(
            200,
            &json!({
                "topicRef": topic, "title": "Welcome", "brief": "A place to start",
                "position": "recent", "canWrite": true, "stewardship": false, "isLocked": false,
                "allowPostEditing": true, "historyOlderCursor": "older-1",
                "historyNewerCursor": null, "readCursor": "read-1",
                "posts": [{
                    "ref": format!("{topic}::p1"), "authorRef": "prt_2", "authorName": "Alex",
                    "text": "Hello", "replyTo": null, "url": format!("{origin}/p/p1"),
                    "createdAt": "2026-09-27T00:00:00Z", "editedAt": null, "removed": false,
                    "facets": [], "source": { "service": "tangent", "uri": null, "recordId": "p1" },
                }],
            }),
        )
    });
    let address =
        Address::parse(&listener.origin(), AddressPolicy::LoopbackAllowed).expect("address");
    let topic = format!("{}::home::welcome", address);
    let said = perform(
        &address,
        "read_topic",
        json!({ "topic": topic, "limit": 5 }),
    )
    .expect("read");
    let request = &listener.requests()[0];
    assert_eq!(request.target, "/api/v1/experience/topics/welcome?limit=5");
    assert_eq!(request.header("authorization"), Some("Bearer ts_secret"));
    assert!(request.body.is_empty());
    assert!(said.contains("\"Hello\""), "{said}");
    assert!(said.contains("\"older-1\""), "{said}");
    assert!(!said.contains("ts_secret"));
}

#[test]
fn a_write_whose_answer_is_lost_is_unknown_and_a_lost_read_is_unavailable() {
    let topic = |address: &Address| format!("{address}::home::welcome");
    let address = losing();
    assert_eq!(
        perform(
            &address,
            "post",
            json!({ "topic": topic(&address), "text": "hi", "request_id": "r-1" })
        ),
        Err(Failed::Ended(wire::lost(Effect::Write {
            retry: Retry::Safe
        })))
    );
    let address = losing();
    let edited = perform(
        &address,
        "edit_post",
        json!({ "post": format!("{}::p1", topic(&address)), "text": "edited" }),
    );
    match edited {
        Err(Failed::Ended(explained)) => {
            assert_eq!(explained.outcome, Outcome::Unknown);
            assert_eq!(explained.sentence, wire::WRITE_UNKNOWN);
        }
        other => panic!("a lost edit is unknown, not {other:?}"),
    }
    let address = losing();
    match perform(&address, "read_topic", json!({ "topic": topic(&address) })) {
        Err(Failed::Ended(explained)) => {
            assert_eq!(explained.outcome, Outcome::TemporarilyUnavailable)
        }
        other => panic!("a lost read is unavailable, not {other:?}"),
    }
}

#[test]
fn a_write_answered_with_another_request_id_is_never_a_success() {
    let listener = Listener::start(|request| {
        let origin = format!("http://{}", request.header("host").unwrap_or_default());
        Reply::json(
            200,
            &json!({ "postRef": format!("{origin}::home::welcome::p9"), "source": null, "url": null, "requestId": "someone-else" }),
        )
    });
    let address =
        Address::parse(&listener.origin(), AddressPolicy::LoopbackAllowed).expect("address");
    let answer = perform(
        &address,
        "post",
        json!({ "topic": format!("{address}::home::welcome"), "text": "hi", "request_id": "mine" }),
    );
    match answer {
        Err(Failed::Ended(explained)) => assert_eq!(explained.outcome, Outcome::Unknown),
        other => panic!("an answer for another request is unknown, not {other:?}"),
    }
    let sent = listener.requests()[0].json();
    assert_eq!(sent["requestId"], "mine");
    assert_eq!(
        listener.requests()[0].header("content-type"),
        Some("application/json")
    );
}
