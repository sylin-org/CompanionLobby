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
use lobby_service_tangent::wire::PERMISSION_DENIED;
use lobby_testkit::atproto::AtprotoWorld;
use lobby_testkit::secrets_in;
use lobby_testkit::space::SpaceServer;
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
    assert_eq!(said(&refused), PERMISSION_DENIED);

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
