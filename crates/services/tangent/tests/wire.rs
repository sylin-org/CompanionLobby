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
        name: Some("Ada".to_owned()),
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
            assert_eq!(
                Some(explained.sentence),
                tools::tool("edit_post").and_then(|tool| tool.unknown),
                "said in the edit's own nouns"
            );
            assert!(explained.sentence.contains("edited this Post"));
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

#[test]
fn what_is_new_names_each_person_from_their_card_and_goes_on_without_one() {
    let listener = Listener::start(|request| {
        let origin = format!("http://{}", request.header("host").unwrap_or_default());
        let topic = format!("{origin}::home::welcome");
        let item = |actor: &str, post: &str| {
            json!({
                "ref": format!("att_{post}"), "kind": "watched_activity", "actorRef": actor,
                "scopeRef": topic, "sourceRef": format!("{topic}::{post}"),
                "excerpt": "Hello there", "state": "new",
            })
        };
        let card = |reference: &str, name: &str| {
            Reply::json(
                200,
                &json!({
                    "profile": { "participantRef": reference, "displayName": name },
                    "recentPosts": [], "morePosts": false,
                }),
            )
        };
        match request.route() {
            "/api/v1/experience/updates" => Reply::json(
                200,
                &json!({
                    "scopeRef": origin,
                    "attention": {
                        "waitingCount": { "value": 0 }, "newActivityCount": { "value": 4 },
                        "items": [item("prt_2", "p1"), item("prt_3", "p2"), item("prt_4", "p3"), item("prt_2", "p4")],
                        "more": false,
                    },
                    "activityCheckpoint": "cp_1",
                }),
            ),
            "/api/v1/experience/participants/prt_2" => card("prt_2", "Bea"),
            // A card that names someone else names nobody.
            "/api/v1/experience/participants/prt_4" => card("prt_9", "Mallory"),
            _ => Reply::json(404, &json!({ "code": "not_found" })),
        }
    });
    let address =
        Address::parse(&listener.origin(), AddressPolicy::LoopbackAllowed).expect("address");
    let said = perform(&address, "updates", json!({})).expect("what is new");
    assert!(
        said.contains("A new Post by \"Bea\" (participant \"prt_2\")"),
        "{said}"
    );
    assert!(
        said.contains("by participant \"prt_3\" (tangent_participant names them)"),
        "{said}"
    );
    assert!(
        said.contains("by participant \"prt_4\" (tangent_participant names them)"),
        "{said}"
    );
    assert!(!said.contains("Mallory"), "{said}");
    assert_eq!(
        listener.count("/api/v1/experience/participants/prt_2"),
        1,
        "each card is read once"
    );
}

#[test]
fn a_card_that_never_answers_leaves_names_out_within_a_few_seconds() {
    let listener = Listener::start(|request| {
        let origin = format!("http://{}", request.header("host").unwrap_or_default());
        let topic = format!("{origin}::home::welcome");
        let item = |actor: &str, post: &str| {
            json!({
                "ref": format!("att_{post}"), "kind": "watched_activity", "actorRef": actor,
                "scopeRef": topic, "sourceRef": format!("{topic}::{post}"),
                "excerpt": "Hello there", "state": "new",
            })
        };
        match request.route() {
            "/api/v1/experience/updates" => Reply::json(
                200,
                &json!({
                    "scopeRef": origin,
                    "attention": {
                        "waitingCount": { "value": 0 }, "newActivityCount": { "value": 2 },
                        "items": [item("prt_2", "p1"), item("prt_3", "p2")],
                        "more": false,
                    },
                    "activityCheckpoint": "cp_1",
                }),
            ),
            route => {
                let reference = route.rsplit('/').next().unwrap_or_default();
                Reply::json(
                    200,
                    &json!({
                        "profile": { "participantRef": reference, "displayName": "Someone" },
                        "recentPosts": [], "morePosts": false,
                    }),
                )
            }
        }
    });
    let stalled = listener.hold_answers("/api/v1/experience/participants/prt_2");
    let address =
        Address::parse(&listener.origin(), AddressPolicy::LoopbackAllowed).expect("address");
    let began = std::time::Instant::now();
    let said = perform(&address, "updates", json!({})).expect("what is new");
    let took = began.elapsed();
    assert!(
        took < std::time::Duration::from_secs(6),
        "the names gave up in time: {took:?}"
    );
    for actor in ["prt_2", "prt_3"] {
        assert!(
            said.contains(&format!(
                "by participant \"{actor}\" (tangent_participant names them)"
            )),
            "{said}"
        );
    }
    assert_eq!(
        listener.count("/api/v1/experience/participants/prt_3"),
        0,
        "no card is read once the time ran out"
    );
    drop(stalled);
}

#[test]
fn there_being_more_is_said_in_the_structured_answer_as_in_the_text() {
    let listener = Listener::start(|request| {
        let origin = format!("http://{}", request.header("host").unwrap_or_default());
        let attention = json!({
            "waitingCount": { "value": 0 }, "newActivityCount": { "value": 0 },
            "items": [], "more": true,
        });
        match request.route() {
            "/api/v1/experience/updates" => Reply::json(
                200,
                &json!({ "scopeRef": origin, "attention": attention, "activityCheckpoint": "cp_1" }),
            ),
            _ => Reply::json(
                200,
                &json!({
                    "identity": { "participantRef": "prt_1", "displayName": "Ada" },
                    "authority": { "canPost": true },
                    "place": { "label": "Home" },
                    "attention": attention,
                    "tangents": [],
                    "continuation": "next-1",
                }),
            ),
        }
    });
    let address =
        Address::parse(&listener.origin(), AddressPolicy::LoopbackAllowed).expect("address");
    let outbound = Outbound::new(AddressPolicy::LoopbackAllowed);
    let issued = issued();
    let through = Through {
        outbound: &outbound,
        address: &address,
        issued: &issued,
    };
    let structured = |name: &str| {
        let result = tools::perform(
            tools::tool(name).expect("a tool"),
            &arguments(json!({})),
            &through,
        )
        .unwrap_or_else(|_| panic!("{name} answers"));
        (result.rendered(), result.structured.expect("structured"))
    };
    let (said, updates) = structured("updates");
    assert!(said.contains("There is more"), "{said}");
    assert_eq!(updates["attention"]["more"]["readWith"], "tangent_updates");
    let (said, arrived) = structured("arrive");
    assert!(said.contains("There are more Tangents"), "{said}");
    assert_eq!(arrived["moreTangents"]["readWith"], "tangent_tangents");
    assert_eq!(arrived["attention"]["more"]["readWith"], "tangent_updates");
}

#[test]
fn a_participant_whose_posts_do_not_all_fit_says_how_many_it_left_out_in_both_forms() {
    let long = "\u{4e2d}".repeat(tools::POST_SHOWN);
    let listener = Listener::start(move |request| {
        let origin = format!("http://{}", request.header("host").unwrap_or_default());
        let posts: Vec<Value> = (1..=10)
            .map(|n| {
                json!({
                    "ref": format!("{origin}::home::welcome::p{n}"),
                    "authorRef": "prt_2",
                    "authorName": "Bea",
                    "createdAt": "2026-09-21T14:13:20Z",
                    "text": long,
                })
            })
            .collect();
        Reply::json(
            200,
            &json!({
                "profile": {
                    "participantRef": "prt_2",
                    "displayName": "Bea",
                    "handle": "bea.example.test",
                    "classification": "human",
                    "description": "Grows basil.",
                },
                "recentPosts": posts,
            }),
        )
    });
    let address =
        Address::parse(&listener.origin(), AddressPolicy::LoopbackAllowed).expect("address");
    let outbound = Outbound::new(AddressPolicy::LoopbackAllowed);
    let issued = issued();
    let result = tools::perform(
        tools::tool("participant").expect("a tool"),
        &arguments(json!({ "participant": "prt_2" })),
        &Through {
            outbound: &outbound,
            address: &address,
            issued: &issued,
        },
    )
    .unwrap_or_else(|_| panic!("the card answers"));
    let said = result.rendered();
    let structured = result.structured.expect("structured");
    let shown = structured["posts"].as_array().expect("posts").len();
    assert!(shown > 0 && shown < 10, "{shown} Posts shown");
    assert!(
        said.contains(&format!("{} more recent Posts are left out", 10 - shown)),
        "{said}"
    );
    assert_eq!(structured["leftOut"], json!(10 - shown), "{structured}");
    assert!(
        structured["readLeftOutIn"]
            .as_str()
            .is_some_and(|read| read.contains("tangent_read_topic")),
        "{structured}"
    );
    // Every field the description names is one the structured answer gives.
    let quoted = &structured["quoted"];
    for field in [
        "participant",
        "displayName",
        "handle",
        "kind",
        "description",
    ] {
        assert!(quoted[field].is_string(), "{field}: {structured}");
    }
}

#[test]
fn a_post_the_space_answers_201_is_a_success() {
    let listener = Listener::start(|request| {
        let origin = format!("http://{}", request.header("host").unwrap_or_default());
        Reply::json(
            201,
            &json!({ "postRef": format!("{origin}::home::welcome::p9"), "requestId": "r-201" }),
        )
    });
    let address =
        Address::parse(&listener.origin(), AddressPolicy::LoopbackAllowed).expect("address");
    let said = perform(
        &address,
        "post",
        json!({ "topic": format!("{address}::home::welcome"), "text": "hi", "request_id": "r-201" }),
    )
    .expect("posted");
    assert!(said.starts_with("Posted in Topic"), "{said}");
}
