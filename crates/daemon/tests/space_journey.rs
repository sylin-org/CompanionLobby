//! A companion in a Tangent Space, through the real daemon and the real
//! command, against the test kit's fakes.
//!
//! The journeys run `companion-lobby-synthetic` with `LOBBY_AUTHSERVER`
//! naming the fake atproto world, so its registry holds the atproto
//! credential adapter and the Tangent service adapter. A companion signs in
//! with the fake account; the sponsor grants it a connection to the fake
//! Space, which the adapter establishes through the proof exchange: the
//! credential adapter mints a service proof at the fake PDS, and the Space
//! takes it once for a participant token. A mind attaches through the real
//! shim. Nothing is seeded: every credential exists because a sign-in
//! completed, every connection because the Space issued it.

use std::path::Path;

use lobby_conformance::host::{Install, body, call, initialize, names, text, tools};
use lobby_daemon::mcp::LIST_CHANGED;
use lobby_service_tangent::tools::POST_DENIED;
use lobby_testkit::atproto::AtprotoWorld;
use lobby_testkit::hosts;
use lobby_testkit::secrets_in;
use lobby_testkit::space::SpaceServer;
use lobby_testkit::world::LobbyWorld;
use serde_json::{Value, json};

const SYNTHETIC: &str = env!("CARGO_BIN_EXE_companion-lobby-synthetic");
const ADA: &str = "did:plc:adaadaadaadaadaadaadaada";
const HANDLE: &str = "ada.example.test";
const EXCHANGE: &str = "/api/v1/identity/exchange";

struct World {
    atproto: AtprotoWorld,
    space: SpaceServer,
    welcome: String,
    opening: String,
}

fn world() -> World {
    let atproto = AtprotoWorld::start();
    atproto.add_account(HANDLE, ADA);
    atproto.sign_in_as(ADA);
    let space = SpaceServer::start(&atproto.proofs());
    space.add_tangent("home", "Home", "Where everyone starts.");
    space.add_topic("home", "welcome", "Welcome", "Say hello.");
    space.name_account(ADA, "Ada", HANDLE);
    let bea = space.add_participant("Bea", "bea.example.test");
    let opening = space.add_post("welcome", &bea, "Welcome, Ada! What brings you here?", None);
    let welcome = space.topic_ref("welcome");
    let opening = space.post_ref(&opening);
    World {
        atproto,
        space,
        welcome,
        opening,
    }
}

/// A fresh install with Ada signed in with the fake account and connected
/// to the fake Space as `main`, and a mind attached as Ada.
fn connected(world: &World, label: &str) -> (Install, String, lobby_testkit::Peer) {
    let install = Install::new(Path::new(SYNTHETIC), label)
        .with_env("LOBBY_AUTHSERVER", &world.atproto.origin());
    install.open();
    let ada = install.create("ada");
    let started = install.operator(
        "POST",
        "/api/sign-ins",
        &json!({ "companion": ada, "kind": "atproto", "hint": "" }),
    );
    assert_eq!(started.status, 201, "{}", text(&started));
    let url = body(&started)["url"].as_str().expect("url").to_owned();
    let page = install.arrive(&world.atproto.approve(&url));
    assert_eq!(page.status, 200, "{}", text(&page));
    let credential = install.json("/api/lobby")["credentials"][0].clone();
    assert!(
        credential["permissions"]
            .as_array()
            .expect("permissions")
            .contains(&json!(lobby_service_tangent::PERMISSION)),
        "the sign-in asked for the leave the Space needs: {credential}"
    );
    let granted = install.operator(
        "POST",
        "/api/connections",
        &json!({
            "companion": ada,
            "credential": credential["id"],
            "service": "tangent",
            "address": world.space.origin(),
            "label": "main",
        }),
    );
    assert_eq!(granted.status, 201, "{}", text(&granted));
    let connection = body(&granted)["connection"]["id"]
        .as_str()
        .expect("id")
        .to_owned();
    let mut peer = install.attach("ada");
    initialize(&mut peer);
    (install, connection, peer)
}

fn said(answer: &Value) -> String {
    answer["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

#[test]
fn a_companion_connects_through_the_proof_exchange_reads_what_is_new_and_replies_once() {
    let world = world();
    let (install, connection, mut peer) = connected(&world, "space");

    // The connection is the proof exchange: the profile read with no
    // credential, one proof minted at the PDS for this Space and presented
    // once in the header, and only the Space's token kept.
    let requests = world.space.listener().requests();
    assert_eq!(requests[0].route(), "/.well-known/tangent-auth");
    assert!(requests[0].header("authorization").is_none());
    assert_eq!(world.space.listener().count(EXCHANGE), 1);
    let proofs = world.atproto.proofs().minted();
    assert_eq!(proofs.len(), 1);
    assert_eq!(proofs[0].issuer, ADA);
    assert_eq!(
        proofs[0].audience,
        format!(
            "did:web:localhost%3A{}#tangent",
            world.space.listener().port()
        )
    );

    // Waking: reach says who the companion is, where it is and where to
    // start, and the catalog is the Space's tools.
    let listed = names(&tools(&mut peer, 1));
    for expected in [
        "reach",
        "tangent_arrive",
        "tangent_read_topic",
        "tangent_post",
    ] {
        assert!(
            listed.contains(&expected.to_owned()),
            "{expected} in {listed:?}"
        );
    }
    let reach = call(&mut peer, 2, "reach", json!({}));
    let woke = said(&reach);
    assert!(
        woke.starts_with("You are ada (moniker ada), attached through Companion Lobby. ada reaches Tangent Space at main ("),
        "{woke}"
    );
    assert!(woke.contains("start with tangent_arrive"), "{woke}");

    // Reading what is new: other people's words arrive quoted, with the
    // references to act on.
    let arrived = call(&mut peer, 3, "tangent_arrive", json!({}));
    assert_eq!(arrived["result"]["isError"], false, "{arrived}");
    let news = said(&arrived);
    assert!(
        news.contains("\"Welcome, Ada! What brings you here?\""),
        "{news}"
    );
    assert!(news.contains(&world.opening), "{news}");
    // Who wrote it is named, in the text and in the structured answer a
    // host may show instead, beside the reference to look them up by.
    assert!(
        news.contains("A new Post by \"Bea\" (participant \""),
        "{news}"
    );
    let item = &arrived["result"]["structuredContent"]["attention"]["items"][0];
    assert_eq!(item["quoted"]["actorName"], "Bea", "{item}");
    assert_eq!(
        reach["result"]["structuredContent"]["connections"][0]["startWith"], "tangent_arrive",
        "the structured reach names where to start, as its text does"
    );
    let read = call(
        &mut peer,
        4,
        "tangent_read_topic",
        json!({ "topic": world.welcome }),
    );
    assert_eq!(read["result"]["isError"], false, "{read}");
    assert!(
        said(&read).contains("You can post here."),
        "{}",
        said(&read)
    );

    // Replying with a request id; the same request again is the same Post.
    let reply = json!({
        "topic": world.welcome,
        "text": "Hello, Bea! I came to read and to say hello.",
        "reply_to": world.opening,
        "request_id": "hello-bea-1",
    });
    let posted = call(&mut peer, 5, "tangent_post", reply.clone());
    assert_eq!(posted["result"]["isError"], false, "{posted}");
    let post = posted["result"]["structuredContent"]["post"].clone();
    assert!(
        post.as_str()
            .is_some_and(|post| post.starts_with(&world.welcome)),
        "{posted}"
    );
    let again = call(&mut peer, 6, "tangent_post", reply);
    assert_eq!(again["result"]["isError"], false, "{again}");
    assert_eq!(
        again["result"]["structuredContent"]["post"], post,
        "the same Post"
    );
    let replies: Vec<_> = world
        .space
        .posts()
        .into_iter()
        .filter(|post| post.request_id.as_deref() == Some("hello-bea-1"))
        .collect();
    assert_eq!(replies.len(), 1, "posted once");
    assert_eq!(
        replies[0].author,
        world.space.participant_of(ADA).expect("Ada's participant"),
        "as the companion's own participant"
    );

    // The journal names every call by the connection it went through.
    let calls: Vec<(String, String, String)> = install
        .journal()
        .iter()
        .filter(|entry| entry["kind"] == "settled")
        .map(|entry| {
            (
                entry["tool"].as_str().unwrap_or_default().to_owned(),
                entry["connection"].as_str().unwrap_or_default().to_owned(),
                entry["outcome"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        calls,
        ["arrive", "read_topic", "post", "post"]
            .iter()
            .map(|tool| ((*tool).to_owned(), connection.clone(), "done".to_owned()))
            .collect::<Vec<_>>()
    );

    // No proof, token or atproto secret reaches any projection or file the
    // daemon writes in plain text.
    let mut secrets: Vec<String> = world.space.tokens();
    secrets.extend(world.atproto.issued());
    let secrets: Vec<&str> = secrets.iter().map(String::as_str).collect();
    for (name, projected) in [
        ("lobby", install.json("/api/lobby").to_string().into_bytes()),
        (
            "journal",
            serde_json::to_vec(&install.journal()).expect("journal"),
        ),
        ("plain files", install.plain_files()),
    ] {
        assert!(
            secrets_in(&projected, &secrets).is_empty(),
            "{name} holds a secret"
        );
    }
    install.stop();
}

#[test]
fn a_connection_revoked_while_a_reply_is_on_its_way_turns_it_into_an_unknown_outcome() {
    let world = world();
    let (install, connection, mut peer) = connected(&world, "space-late");
    let route = "/api/v1/experience/topics/welcome/posts";
    let held = world.space.listener().hold_answers(route);
    peer.send(&json!({
        "jsonrpc": "2.0", "id": 10, "method": "tools/call",
        "params": { "name": "tangent_post", "arguments": { "topic": world.welcome, "text": "Landing late.", "request_id": "late-1" } }
    }));
    world.space.listener().wait_for(route, 1);
    let revoked = install.operator(
        "POST",
        &format!("/api/connections/{connection}/revoke"),
        &json!({}),
    );
    assert_eq!(revoked.status, 200, "{}", text(&revoked));
    drop(held);
    let late = peer.answer(&json!(10));
    assert_eq!(late["result"]["isError"], true, "{late}");
    assert_eq!(
        late["result"]["structuredContent"]["outcome"],
        "outcome_unknown"
    );
    let told = said(&late);
    assert!(
        told.contains("Read the target before deciding whether to write again"),
        "{told}"
    );
    assert_eq!(
        late["result"]["structuredContent"]["message"],
        told.as_str()
    );
    assert!(
        world
            .space
            .posts()
            .iter()
            .any(|post| post.text == "Landing late."),
        "the Post did land: unknown is the honest answer"
    );
    peer.notification(LIST_CHANGED);
    assert_eq!(
        names(&tools(&mut peer, 11)),
        ["reach"],
        "the Space's tools are gone"
    );
    let after = call(
        &mut peer,
        12,
        "tangent_post",
        json!({ "topic": world.welcome, "text": "Again." }),
    );
    assert_eq!(after["error"]["code"], -32602, "the tool is gone: {after}");
    assert_eq!(
        world.space.listener().count(route),
        1,
        "nothing more reached the Space"
    );
    install.stop();
}

#[test]
fn a_space_that_refuses_reads_as_its_reason_with_a_next_step_and_a_suspension_too() {
    let world = world();
    let (install, _, mut peer) = connected(&world, "space-refused");
    world.space.refuse_writes_in("welcome");
    let refused = call(
        &mut peer,
        20,
        "tangent_post",
        json!({ "topic": world.welcome, "text": "Hello?" }),
    );
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    assert_eq!(refused["result"]["structuredContent"]["outcome"], "refused");
    assert_eq!(said(&refused), POST_DENIED);
    assert_eq!(
        refused["result"]["structuredContent"]["message"],
        POST_DENIED
    );

    // A Space that stops taking the token is given a fresh proof once; one
    // that suspended the account says so, and nothing loops.
    world.space.suspend(ADA);
    let exchanges = world.space.listener().count(EXCHANGE);
    let suspended = call(&mut peer, 21, "tangent_arrive", json!({}));
    assert_eq!(suspended["result"]["isError"], true, "{suspended}");
    assert_eq!(
        suspended["result"]["structuredContent"]["outcome"],
        "refused"
    );
    assert_eq!(said(&suspended), lobby_service_tangent::SUSPENDED);
    assert_eq!(world.space.listener().count(EXCHANGE), exchanges + 1);
    let lobby = install.json("/api/lobby");
    assert_eq!(
        lobby["credentials"][0]["state"], "ready",
        "the Space refusing is not the account needing a sign-in"
    );
    install.stop();
}

#[test]
fn a_token_the_space_stops_taking_is_exchanged_again_once_and_kept() {
    let world = world();
    let (install, _, mut peer) = connected(&world, "space-again");
    world.space.end_tokens();
    let read = call(
        &mut peer,
        30,
        "tangent_read_topic",
        json!({ "topic": world.welcome }),
    );
    assert_eq!(read["result"]["isError"], false, "{read}");
    assert_eq!(
        world.space.listener().count(EXCHANGE),
        2,
        "one fresh exchange"
    );
    let proofs = world.atproto.proofs().minted();
    assert_eq!(proofs.len(), 2);
    assert_ne!(proofs[0].id, proofs[1].id, "with a fresh proof");
    let again = call(
        &mut peer,
        31,
        "tangent_read_topic",
        json!({ "topic": world.welcome }),
    );
    assert_eq!(again["result"]["isError"], false, "{again}");
    assert_eq!(
        world.space.listener().count(EXCHANGE),
        2,
        "the new token was kept for the next call"
    );
    install.stop();
}

/// Ada connected, then the Space stops taking its token and the account's
/// server stops taking its sign-in: the next call must establish the
/// connection again, and the proof it needs cannot be minted.
fn signed_out_mid_call(world: &World, label: &str, id: i64, tool: &str, arguments: Value) {
    let (install, _, mut peer) = connected(world, label);
    let posts = world.space.posts().len();
    world.space.end_tokens();
    world.atproto.end_access_tokens();
    world.atproto.refuse_refreshes();
    let answer = call(&mut peer, id, tool, arguments);
    assert_eq!(answer["result"]["isError"], true, "{answer}");
    let structured = &answer["result"]["structuredContent"];
    assert_eq!(
        structured["outcome"], "needs_sign_in",
        "never revoked or unknown: {answer}"
    );
    let told = said(&answer);
    assert_eq!(
        told,
        "The account behind Tangent Space at main needs the sponsor to sign it in again on the manager page; nothing was done there, and its tools come back once they do."
    );
    assert_eq!(structured["message"], told.as_str());
    peer.notification(LIST_CHANGED);
    assert_eq!(names(&tools(&mut peer, id + 1)), ["reach"]);
    let lobby = install.json("/api/lobby");
    assert_eq!(lobby["credentials"][0]["state"], "needs_sign_in", "{lobby}");
    assert_eq!(lobby["connections"][0]["state"], "needs_sign_in", "{lobby}");
    let settled: Vec<String> = install
        .journal()
        .iter()
        .filter(|entry| entry["kind"] == "settled")
        .map(|entry| entry["outcome"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(settled, ["needs_sign_in"], "the journal says the same");
    assert_eq!(world.space.posts().len(), posts, "nothing landed");
    install.stop();
}

#[test]
fn a_read_that_finds_the_account_needs_a_sign_in_says_exactly_that() {
    let world = world();
    let topic = world.welcome.clone();
    signed_out_mid_call(
        &world,
        "space-signed-out-read",
        60,
        "tangent_read_topic",
        json!({ "topic": topic }),
    );
}

#[test]
fn a_write_that_finds_the_account_needs_a_sign_in_says_so_and_never_lands() {
    let world = world();
    let topic = world.welcome.clone();
    signed_out_mid_call(
        &world,
        "space-signed-out-write",
        70,
        "tangent_post",
        json!({ "topic": topic, "text": "Hello again.", "request_id": "again-1" }),
    );
}

#[test]
fn a_post_of_a_mebibyte_reaches_the_mind_cut_short_and_quoted_in_text_and_structure() {
    let world = world();
    let mallory = world
        .space
        .add_participant("Mallory", "mallory.example.test");
    let long = "\u{e9}".repeat(512 * 1024);
    let planted = world.space.add_post("welcome", &mallory, &long, None);
    let planted = world.space.post_ref(&planted);
    let (install, _, mut peer) = connected(&world, "space-long");
    let read = call(
        &mut peer,
        40,
        "tangent_read_topic",
        json!({ "topic": world.welcome }),
    );
    assert_eq!(read["result"]["isError"], false, "{read}");
    let text = said(&read);
    assert!(text.len() < 16 * 1024, "{} bytes of text", text.len());
    assert!(text.contains("(cut short here)"));
    let structured = &read["result"]["structuredContent"];
    let size = structured.to_string().len();
    assert!(size < 16 * 1024, "{size} bytes of structured content");
    let posts = structured["posts"].as_array().expect("posts");
    let cut = posts
        .iter()
        .find(|post| post["post"] == planted.as_str())
        .expect("the long Post");
    assert_eq!(
        cut["quoted"]["text"]
            .as_str()
            .expect("quoted")
            .chars()
            .count(),
        lobby_service_tangent::tools::POST_SHOWN
    );
    assert_eq!(cut["cutShort"], json!(["text"]));
    assert_eq!(cut["quoted"]["authorName"], "Mallory");
    for post in posts {
        assert!(
            post.get("text").is_none() && post.get("authorName").is_none(),
            "what other people wrote sits only under quoted: {post}"
        );
    }
    install.stop();
}

#[test]
fn a_page_of_long_posts_in_another_script_fits_its_budget_and_says_what_it_left_out() {
    let world = world();
    let mallory = world
        .space
        .add_participant("Mallory", "mallory.example.test");
    let long = "\u{4e2d}".repeat(lobby_service_tangent::tools::POST_SHOWN);
    let mut planted = Vec::new();
    for _ in 0..10 {
        let post = world.space.add_post("welcome", &mallory, &long, None);
        planted.push(world.space.post_ref(&post));
    }
    let (install, _, mut peer) = connected(&world, "space-script");
    let read = call(
        &mut peer,
        50,
        "tangent_read_topic",
        json!({ "topic": world.welcome }),
    );
    assert_eq!(read["result"]["isError"], false, "{read}");
    let text = said(&read);
    assert!(
        text.len() <= lobby_contracts::ToolResult::RENDERED_BUDGET,
        "{} bytes of text",
        text.len()
    );
    assert!(
        !text.contains("cut this result short"),
        "the page fits whole: {text}"
    );
    assert!(
        text.contains("left out to keep this answer within its reading budget; read with limit"),
        "{text}"
    );
    assert!(
        text.contains("To mark these read, call tangent_mark_read"),
        "{text}"
    );
    let structured = &read["result"]["structuredContent"];
    assert!(structured.is_object(), "the structured answer fits too");
    let posts = structured["posts"].as_array().expect("posts");
    assert!(
        !posts.is_empty() && posts.len() < 10,
        "{} Posts",
        posts.len()
    );
    assert_eq!(
        posts.last().expect("the newest")["post"],
        planted.last().expect("planted").as_str(),
        "the newest Posts are the ones shown"
    );
    assert!(
        structured["cursor"].is_null(),
        "no cursor that would skip the Posts left out"
    );
    assert!(structured["readCursor"].is_string());
    install.stop();
}

/// The recorded frame with the Space's origin of the walk it was recorded
/// on replaced by `origin`.
fn moved(frame: &Value, recorded: &str, origin: &str) -> Value {
    serde_json::from_str(&frame.to_string().replace(recorded, origin)).expect("still JSON")
}

#[test]
fn the_recorded_claude_code_mind_in_a_space_replays_through_the_shim() {
    let recording = hosts::claude_code_in_a_space();
    let world = LobbyWorld::start();
    let install = Install::new(Path::new(SYNTHETIC), "space-replay")
        .with_env("LOBBY_AUTHSERVER", &world.authserver());
    install.open();
    let created = install.operator(
        "POST",
        "/api/companions",
        &json!({ "moniker": "ada", "displayName": "Ada" }),
    );
    assert_eq!(created.status, 201, "{}", text(&created));
    let ada = body(&created)["companion"]["id"]
        .as_str()
        .expect("id")
        .to_owned();
    let started = install.operator(
        "POST",
        "/api/sign-ins",
        &json!({ "companion": ada, "kind": "atproto", "hint": "" }),
    );
    let url = body(&started)["url"].as_str().expect("url").to_owned();
    assert_eq!(install.arrive(&world.atproto.approve(&url)).status, 200);
    let credential = install.json("/api/lobby")["credentials"][0]["id"].clone();
    let granted = install.operator(
        "POST",
        "/api/connections",
        &json!({ "companion": ada, "credential": credential, "service": "tangent", "address": world.space.origin(), "label": "main" }),
    );
    assert_eq!(granted.status, 201, "{}", text(&granted));

    let reach = recording
        .sent()
        .find(|frame| frame["params"]["name"] == "reach")
        .and_then(|frame| recording.answer_to(&frame["id"]))
        .expect("the recorded reach");
    let recorded = reach["result"]["structuredContent"]["connections"][0]["address"]
        .as_str()
        .expect("the recorded Space")
        .to_owned();
    let origin = world.space.origin();
    let before = world.space.posts().len();
    let mut host = install.attach(&ada);
    for frame in recording.sent() {
        let frame = moved(frame, &recorded, &origin);
        host.send(&frame);
        let Some(id) = frame.get("id") else {
            continue;
        };
        let answer = host.answer(id);
        let expected = moved(
            recording
                .answer_to(id)
                .expect("the recording has this answer"),
            &recorded,
            &origin,
        );
        assert_eq!(
            answer["error"]["code"], expected["error"]["code"],
            "{answer}"
        );
        // What the mind read is what it reads now: a change to any of it
        // is recorded again on purpose, by walking a real mind again.
        if frame["method"] == "tools/list" {
            let read = |tools: &Value| -> Vec<Value> {
                tools
                    .as_array()
                    .expect("tools")
                    .iter()
                    .map(|tool| {
                        json!({
                            "name": tool["name"],
                            "title": tool["title"],
                            "description": tool["description"],
                            "inputSchema": tool["inputSchema"],
                            "annotations": tool["annotations"],
                        })
                    })
                    .collect()
            };
            assert_eq!(
                read(&answer["result"]["tools"]),
                read(&expected["result"]["tools"]),
                "the tools as the mind read them"
            );
        }
        if frame["method"] == "tools/call" {
            assert_eq!(answer["result"]["isError"], false, "{answer}");
            let name = frame["params"]["name"].as_str().expect("name");
            assert_eq!(
                answer["result"]["content"], expected["result"]["content"],
                "{name} reads as it did when it was recorded"
            );
            let (got, want) = (
                &answer["result"]["structuredContent"],
                &expected["result"]["structuredContent"],
            );
            if name == "reach" {
                // The companion's id is this install's own.
                for field in ["tools", "connections", "waiting"] {
                    assert_eq!(got[field], want[field], "reach's {field}");
                }
            } else {
                assert_eq!(got, want, "{name} answers as it did when it was recorded");
            }
        }
    }
    host.close_stdin();
    let posts = world.space.posts();
    assert_eq!(posts.len(), before + 1, "the mind posted one reply");
    let reply = posts.last().expect("the reply");
    assert!(reply.reply_to.is_some() && reply.request_id.is_some());
    install.stop();
}
