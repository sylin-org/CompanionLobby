//! A companion in a Tangent Space, through the real daemon and the real
//! command, against the test kit's fakes.
//!
//! The journeys run `companion-lobby-synthetic` with `LOBBY_AUTHSERVER`
//! naming the fake atproto world, so its registry holds the atproto
//! credential adapter and the Tangent service adapter. A companion signs in
//! with the fake account; the sponsor grants it a connection to the fake
//! Space, which the adapter establishes through the proof exchange: the
//! credential adapter mints a service proof at the fake PDS, and the Space
//! takes it once and signs the account in with a cookie, which the lobby keeps
//! for that connection alone. A mind attaches through the real shim. Nothing is seeded: every credential exists because a sign-in
//! completed, every connection because the Space issued it.

use std::path::Path;

use lobby_conformance::host::{Install, body, call, initialize, names, text, tools};
use lobby_daemon::mcp::LIST_CHANGED;
use lobby_service_tangent::tools::POST_DENIED;
use lobby_service_tangent::wire::LOCKED;
use lobby_testkit::atproto::AtprotoWorld;
use lobby_testkit::hosts;
use lobby_testkit::secrets_in;
use lobby_testkit::space::SpaceServer;
use lobby_testkit::world::{LobbyWorld, new_post};
use serde_json::{Value, json};

const SYNTHETIC: &str = env!("CARGO_BIN_EXE_companion-lobby-synthetic");
const ADA: &str = "did:plc:adaadaadaadaadaadaadaada";
const HANDLE: &str = "ada.example.test";
const EXCHANGE: &str = "/api/v1/identity/exchange";

struct World {
    atproto: AtprotoWorld,
    space: SpaceServer,
    bea: String,
    welcome: String,
    route: String,
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
    let route = space.topic_route("welcome");
    let opening = space.post_ref(&opening);
    World {
        atproto,
        space,
        bea,
        welcome,
        route,
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
fn a_host_that_dies_without_detaching_is_no_longer_counted_as_attached() {
    let install = Install::new(Path::new(SYNTHETIC), "space-dies");
    install.open();
    let id = install.create("ada");
    let attachments = |install: &Install| {
        install.json("/api/companions")["companions"]
            .as_array()
            .expect("companions")
            .iter()
            .find(|companion| companion["id"] == id.as_str())
            .expect("ada")["attachments"]
            .clone()
    };
    let mut host = install.attach("ada");
    initialize(&mut host);
    assert_eq!(attachments(&install), 1);
    host.kill();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while attachments(&install) != 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the page still counts a host that died"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(
        install
            .journal()
            .iter()
            .any(|entry| entry["command"] == "detach"
                && entry["outcome"] == "done"
                && entry["initiator"] == "lobby"),
        "the journal says the lobby ended it"
    );
    install.stop();
}

#[test]
fn a_mind_connects_to_what_its_account_enables_and_a_closed_connection_stays_closed() {
    let world = world();
    let install = Install::new(Path::new(SYNTHETIC), "space-connect")
        .with_env("LOBBY_AUTHSERVER", &world.atproto.origin());
    install.open();
    let started = install.operator(
        "POST",
        "/api/sign-ins",
        &json!({ "kind": "atproto", "hint": "" }),
    );
    assert_eq!(started.status, 201, "{}", text(&started));
    let url = body(&started)["url"].as_str().expect("url").to_owned();
    assert_eq!(install.arrive(&world.atproto.approve(&url)).status, 200);
    let lobby = install.json("/api/lobby");
    let moniker = lobby["companions"][0]["moniker"]
        .as_str()
        .expect("moniker")
        .to_owned();
    assert_eq!(
        lobby["connections"],
        json!([]),
        "signing in connects nothing"
    );
    let mut peer = install.attach(&moniker);
    initialize(&mut peer);

    // The account enables the services, each with its own way to connect.
    let tools = |peer: &mut lobby_testkit::Peer| -> Vec<Value> {
        peer.ask(&json!({ "jsonrpc": "2.0", "id": 900, "method": "tools/list" }))["result"]["tools"]
            .as_array()
            .expect("tools")
            .clone()
    };
    let listed = tools(&mut peer);
    let tool =
        |listed: &[Value], name: &str| listed.iter().find(|tool| tool["name"] == name).cloned();
    assert_eq!(
        tool(&listed, "tangent_connect").expect("by address")["inputSchema"]["required"],
        json!(["address"])
    );
    assert_eq!(
        tool(&listed, "bluesky_connect").expect("where the account lives")["inputSchema"]["properties"],
        json!({})
    );
    let reach = call(&mut peer, 1, "reach", json!({}));
    assert!(
        said(&reach).contains("can connect to a Tangent Space by its address with tangent_connect")
            && said(&reach).contains("can connect to Bluesky with bluesky_connect"),
        "{}",
        said(&reach)
    );
    assert_eq!(
        reach["result"]["structuredContent"]["canConnect"][0]["takesAddress"],
        true
    );

    // A missing address says what it takes, and a bad one reaches nothing.
    let bare = call(&mut peer, 2, "tangent_connect", json!({}));
    assert_eq!(bare["result"]["isError"], true, "{bare}");
    assert!(said(&bare).contains("address"), "{}", said(&bare));
    let bad = call(
        &mut peer,
        3,
        "tangent_connect",
        json!({ "address": "http://10.0.0.1" }),
    );
    assert_eq!(bad["result"]["isError"], true, "{bad}");

    // Connected: its tools appear, and connecting again changes nothing.
    // However the mind was told to name this machine, the connection keeps the
    // name the Space goes by.
    let origin = world.space.origin();
    let opened = call(
        &mut peer,
        4,
        "tangent_connect",
        json!({ "address": origin.replace("127.0.0.1", "localhost") }),
    );
    assert_eq!(opened["result"]["isError"], false, "{opened}");
    assert!(
        said(&opened).starts_with("Connected to Tangent Space at tangent"),
        "{}",
        said(&opened)
    );
    assert!(
        said(&opened).contains("Start with tangent_"),
        "{}",
        said(&opened)
    );
    assert!(
        tool(&tools(&mut peer), "tangent_topics").is_some(),
        "its tools appear"
    );
    let again = call(
        &mut peer,
        5,
        "tangent_connect",
        json!({ "address": origin }),
    );
    assert!(
        said(&again).starts_with("Already connected"),
        "{}",
        said(&again)
    );
    let lobby = install.json("/api/lobby");
    assert_eq!(lobby["connections"].as_array().map(Vec::len), Some(1));
    assert_eq!(lobby["connections"][0]["address"], origin.as_str());
    let connection = lobby["connections"][0]["id"]
        .as_str()
        .expect("id")
        .to_owned();
    assert!(
        install
            .journal()
            .iter()
            .any(|entry| entry["command"] == "grant_connection"
                && entry["outcome"] == "done"
                && entry["initiator"] == "mind"),
        "the journal says the mind did it"
    );

    // What the sponsor closes stays closed until the sponsor opens it again.
    let closed = install.operator(
        "POST",
        &format!("/api/connections/{connection}/revoke"),
        &json!({}),
    );
    assert_eq!(closed.status, 200, "{}", text(&closed));
    let refused = call(
        &mut peer,
        6,
        "tangent_connect",
        json!({ "address": origin }),
    );
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    assert!(
        said(&refused).contains("The sponsor closed"),
        "{}",
        said(&refused)
    );

    // The service that lives where the account lives takes nothing, and its
    // tool is gone once it stands.
    let home = call(&mut peer, 7, "bluesky_connect", json!({}));
    assert_eq!(home["result"]["isError"], false, "{home}");
    assert!(tool(&tools(&mut peer), "bluesky_connect").is_none());
    peer.close_stdin();
    peer.wait_exit(std::time::Duration::from_secs(10));
    install.stop();
}

#[test]
fn a_companion_connects_through_the_proof_exchange_reads_what_is_new_and_replies_once() {
    let world = world();
    let (install, connection, mut peer) = connected(&world, "space");

    // The connection is the proof exchange: the profile read with no
    // credential, one proof minted at the PDS for this Space and presented
    // once in the header, and only the Space's cookie kept.
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
    assert!(woke.starts_with("You are ada,"), "{woke}");
    assert!(
        !woke.contains("(moniker"),
        "a moniker that is the name is not said twice: {woke}"
    );
    assert!(woke.contains("reaches Tangent Space at main ("), "{woke}");
    assert!(woke.contains("start with tangent_arrive"), "{woke}");

    // Reading a Topic: other people's words arrive quoted, with the
    // addresses to act on.
    let read = call(
        &mut peer,
        3,
        "tangent_read_topic",
        json!({ "topic": world.welcome }),
    );
    assert_eq!(read["result"]["isError"], false, "{read}");
    let opened = said(&read);
    assert!(
        opened.contains("; 2 Posts, oldest first. You can post here."),
        "{opened}"
    );
    assert!(
        opened.contains("\"Welcome, Ada! What brings you here?\""),
        "{opened}"
    );
    assert!(opened.contains(&world.opening), "{opened}");
    assert_eq!(
        read["result"]["structuredContent"]["replyWith"],
        json!({ "tool": "tangent_post", "topic": world.welcome }),
        "the structured answer names how to reply, as the text does"
    );
    assert_eq!(
        read["result"]["structuredContent"]["message"], read["result"]["content"][0]["text"],
        "and holds the text for a host that shows it alone"
    );
    assert_eq!(
        reach["result"]["structuredContent"]["connections"][0]["startWith"], "tangent_arrive",
        "the structured reach names where to start, as its text does"
    );

    // Replying with a request id, through the form the page carries; the same
    // request again is the same Post.
    let reply = json!({
        "topic": world.welcome,
        "text": "Hello, Bea! I came to read and to say hello.",
        "reply_to": world.opening,
        "request_id": "hello-bea-1",
    });
    let posted = call(&mut peer, 4, "tangent_post", reply.clone());
    assert_eq!(posted["result"]["isError"], false, "{posted}");
    let post = posted["result"]["structuredContent"]["post"].clone();
    assert!(
        post.as_str()
            .is_some_and(|post| post.starts_with(&world.welcome)),
        "{posted}"
    );
    let again = call(&mut peer, 5, "tangent_post", reply);
    assert_eq!(again["result"]["isError"], false, "{again}");
    assert_eq!(
        again["result"]["structuredContent"]["post"], post,
        "the same Post"
    );
    let replies: Vec<_> = world
        .space
        .posts()
        .into_iter()
        .filter(|post| post.text == "Hello, Bea! I came to read and to say hello.")
        .collect();
    assert_eq!(replies.len(), 1, "posted once");
    assert_eq!(
        replies[0].author,
        world.space.participant_of(ADA).expect("Ada's participant"),
        "as the companion's own participant"
    );

    // What is new names who wrote it, in the text and in the structured
    // answer a host may show instead, beside the reference to look them up by.
    world.space.add_post(
        "welcome",
        &world.bea,
        "Glad you came, Ada.",
        Some(&replies[0].id),
    );
    let arrived = call(&mut peer, 6, "tangent_arrive", json!({}));
    assert_eq!(arrived["result"]["isError"], false, "{arrived}");
    let news = said(&arrived);
    assert!(news.contains("\"Glad you came, Ada.\""), "{news}");
    assert!(
        news.contains("A reply to you by \"Bea\" (participant \""),
        "{news}"
    );
    let item = &arrived["result"]["structuredContent"]["attention"]["items"][0];
    assert_eq!(item["quoted"]["actorName"], "Bea", "{item}");

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
        ["read_topic", "post", "post", "arrive"]
            .iter()
            .map(|tool| ((*tool).to_owned(), connection.clone(), "done".to_owned()))
            .collect::<Vec<_>>()
    );

    // No proof, cookie or atproto secret reaches any projection or file the
    // daemon writes in plain text.
    let mut secrets: Vec<String> = world.space.cookies();
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
fn reach_names_the_account_and_how_the_space_knows_the_companion_and_never_lets_a_reader_write() {
    let world = world();
    world.space.hold_as_applicants(true);
    let (install, _, mut peer) = connected(&world, "space-reach");
    let participant = world.space.participant_of(ADA).expect("a participant");
    let origin = world.space.origin();
    let reach = call(&mut peer, 110, "reach", json!({}));
    let told = said(&reach);
    assert_eq!(
        told,
        format!(
            "You are ada, attached through Companion Lobby. ada holds the Bluesky account \"{HANDLE}\". ada reaches Tangent Space at main ({origin}) with the account \"{HANDLE}\"; Tangent Space knows it there as \"Ada\" (participant \"{participant}\") and lets it read, but not post: start with tangent_arrive. ada can connect to a Tangent Space by its address with tangent_connect. ada can connect to Bluesky with bluesky_connect."
        )
    );
    let structured = &reach["result"]["structuredContent"];
    assert_eq!(structured["message"], told.as_str());
    assert_eq!(
        structured["accounts"],
        json!([{ "label": HANDLE, "kind": "atproto", "title": "Bluesky", "waitsForSignIn": false }])
    );
    let connection = &structured["connections"][0];
    assert_eq!(connection["account"], HANDLE);
    assert_eq!(connection["allows"], "read, but not post");
    assert_eq!(connection["knownAs"]["referenceIs"], "participant");
    assert!(
        !told.contains("write") && !told.contains("can post"),
        "a connection that only reads is never told it can write: {told}"
    );

    // The Space names the companion as reach said it would.
    let arrived = call(&mut peer, 111, "tangent_arrive", json!({}));
    let you = &arrived["result"]["structuredContent"]["you"]["quoted"];
    assert_eq!(you["displayName"], connection["knownAs"]["quoted"]["name"]);
    assert_eq!(
        you["participant"],
        connection["knownAs"]["quoted"]["reference"]
    );
    let welcomed = said(&arrived);
    assert!(
        welcomed.contains("this connection cannot post here")
            && !welcomed.contains("post with tangent_post"),
        "{welcomed}"
    );
    assert_eq!(
        arrived["result"]["structuredContent"]["may"],
        json!(["read"])
    );
    assert!(
        !names(&tools(&mut peer, 112)).contains(&"tangent_post".to_owned()),
        "and it is offered no post"
    );
    install.stop();
}

#[test]
fn a_connection_revoked_while_a_reply_is_on_its_way_turns_it_into_an_unknown_outcome() {
    let world = world();
    let (install, connection, mut peer) = connected(&world, "space-late");
    let route = world.route.as_str();
    let held = world
        .space
        .listener()
        .hold_answers_to(route, "body", "Landing late.");
    peer.send(&json!({
        "jsonrpc": "2.0", "id": 10, "method": "tools/call",
        "params": { "name": "tangent_post", "arguments": { "topic": world.welcome, "text": "Landing late.", "request_id": "late-1" } }
    }));
    // The page that offers the form, and then the form.
    world.space.listener().wait_for_handled(route, 2);
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
        told.starts_with(
            "The sponsor revoked Tangent Space at main while this write was on its way"
        ) && told.contains("read the target before deciding whether to write again"),
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
        ["bluesky_connect", "reach", "tangent_connect"],
        "the Space's tools are gone"
    );
    let after = call(
        &mut peer,
        12,
        "tangent_post",
        json!({ "topic": world.welcome, "text": "Again." }),
    );
    assert_eq!(
        after["result"]["isError"], true,
        "the tool is gone: {after}"
    );
    assert_eq!(
        said(&after),
        "This companion cannot reach a tool called \"tangent_post\" now. Call reach to see what it reaches and the tools it has there."
    );
    assert!(!said(&after).contains("revoked"), "never a guess at why");
    assert_eq!(
        world.space.listener().count(route),
        2,
        "nothing more reached the Space"
    );
    install.stop();
}

#[test]
fn a_read_cut_off_by_a_revoke_says_which_connection_and_to_call_reach() {
    let world = world();
    let (install, connection, mut peer) = connected(&world, "space-read-revoked");
    let route = world.route.as_str();
    let held = world.space.listener().hold_answers(route);
    peer.send(&json!({
        "jsonrpc": "2.0", "id": 15, "method": "tools/call",
        "params": { "name": "tangent_read_topic", "arguments": { "topic": world.welcome } }
    }));
    world.space.listener().wait_for_handled(route, 1);
    let revoked = install.operator(
        "POST",
        &format!("/api/connections/{connection}/revoke"),
        &json!({}),
    );
    assert_eq!(revoked.status, 200, "{}", text(&revoked));
    drop(held);
    let late = peer.answer(&json!(15));
    assert_eq!(late["result"]["isError"], true, "{late}");
    assert_eq!(late["result"]["structuredContent"]["outcome"], "revoked");
    let told = said(&late);
    assert_eq!(
        told,
        "The sponsor revoked Tangent Space at main while this read was on its way, so its answer is not shown. Call reach to see what this companion reaches now."
    );
    assert_eq!(
        late["result"]["structuredContent"]["message"],
        told.as_str()
    );
    assert!(!told.contains("Welcome"), "nothing of the Topic is shown");
    install.stop();
}

#[test]
fn a_space_that_refuses_reads_as_its_reason_with_a_next_step_and_a_ban_too() {
    let world = world();
    let (install, _, mut peer) = connected(&world, "space-refused");
    world.space.refuse_replies_in("welcome");
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

    // A locked Topic offers no form, and says why.
    world.space.lock("welcome");
    let locked = call(
        &mut peer,
        22,
        "tangent_post",
        json!({ "topic": world.welcome, "text": "Hello?" }),
    );
    assert_eq!(said(&locked), LOCKED);

    // A Space that ended the sign-in is given a fresh proof once; one that
    // banned the account says so, and nothing loops.
    world.space.end_sign_ins();
    world.space.ban(ADA);
    let exchanges = world.space.listener().count(EXCHANGE);
    let banned = call(&mut peer, 21, "tangent_arrive", json!({}));
    assert_eq!(banned["result"]["isError"], true, "{banned}");
    assert_eq!(banned["result"]["structuredContent"]["outcome"], "refused");
    assert_eq!(said(&banned), lobby_service_tangent::SUSPENDED);
    assert_eq!(world.space.listener().count(EXCHANGE), exchanges + 1);
    let lobby = install.json("/api/lobby");
    assert_eq!(
        lobby["credentials"][0]["state"], "ready",
        "the Space refusing is not the account needing a sign-in"
    );
    install.stop();
}

#[test]
fn a_sign_in_the_space_ended_is_proved_again_once_and_kept() {
    let world = world();
    let (install, _, mut peer) = connected(&world, "space-again");
    world.space.end_sign_ins();
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
        "the new cookie was kept for the next call"
    );
    install.stop();
}

#[test]
fn two_companions_at_one_space_hold_a_cookie_each_and_keep_it_across_a_restart() {
    const CY: &str = "did:plc:cycycycycycycycycycycycy";
    const CY_HANDLE: &str = "cy.example.test";
    let world = world();
    world.space.name_account(CY, "Cy", CY_HANDLE);
    world.atproto.add_account(CY_HANDLE, CY);
    let (install, _, mut ada) = connected(&world, "space-two");

    world.atproto.sign_in_as(CY);
    let cy_id = install.create("cy");
    let started = install.operator(
        "POST",
        "/api/sign-ins",
        &json!({ "companion": cy_id, "kind": "atproto", "hint": "" }),
    );
    assert_eq!(started.status, 201, "{}", text(&started));
    let url = body(&started)["url"].as_str().expect("url").to_owned();
    assert_eq!(install.arrive(&world.atproto.approve(&url)).status, 200);
    let credentials = install.json("/api/lobby")["credentials"].clone();
    let credential = credentials
        .as_array()
        .expect("credentials")
        .iter()
        .find(|credential| credential["companion"] == cy_id.as_str())
        .expect("Cy's credential")["id"]
        .clone();
    let granted = install.operator(
        "POST",
        "/api/connections",
        &json!({ "companion": cy_id, "credential": credential, "service": "tangent", "address": world.space.origin(), "label": "main" }),
    );
    assert_eq!(granted.status, 201, "{}", text(&granted));
    let mut cy = install.attach("cy");
    initialize(&mut cy);

    // One topic, one request id, two companions: two Posts, each its own.
    let write = |text: &str| json!({ "topic": world.welcome, "text": text, "request_id": "hello" });
    let from_ada = call(&mut ada, 120, "tangent_post", write("From Ada."));
    let from_cy = call(&mut cy, 121, "tangent_post", write("From Cy."));
    assert_eq!(from_ada["result"]["isError"], false, "{from_ada}");
    assert_eq!(from_cy["result"]["isError"], false, "{from_cy}");
    let author = |text: &str| {
        world
            .space
            .posts()
            .into_iter()
            .find(|post| post.text == text)
            .map(|post| post.author)
    };
    assert_eq!(author("From Ada.").as_deref(), Some(ADA));
    assert_eq!(author("From Cy.").as_deref(), Some(CY));
    let arrived = call(&mut cy, 122, "tangent_arrive", json!({}));
    assert_eq!(
        arrived["result"]["structuredContent"]["you"]["quoted"]["participant"],
        CY
    );

    // Each signed in once, with a cookie of its own, and neither is in a file
    // the daemon writes in plain text.
    assert_eq!(world.space.listener().count(EXCHANGE), 2);
    let cookies = world.space.cookies();
    let secrets: Vec<&str> = cookies.iter().map(String::as_str).collect();
    assert!(
        secrets_in(&install.plain_files(), &secrets).is_empty(),
        "cookies stay sealed"
    );

    // A restarted lobby still holds them: nothing proves itself again.
    drop(ada);
    drop(cy);
    install.stop();
    install.open();
    let mut ada = install.attach("ada");
    initialize(&mut ada);
    let read = call(
        &mut ada,
        123,
        "tangent_read_topic",
        json!({ "topic": world.welcome }),
    );
    assert_eq!(read["result"]["isError"], false, "{read}");
    assert_eq!(world.space.listener().count(EXCHANGE), 2);
    install.stop();
}

/// Ada connected, then the Space ends its sign-in and the account's server
/// stops taking its sign-in: the next call must establish the connection
/// again, and the proof it needs cannot be minted.
fn signed_out_mid_call(world: &World, label: &str, id: i64, tool: &str, arguments: Value) {
    let (install, _, mut peer) = connected(world, label);
    let posts = world.space.posts().len();
    world.space.end_sign_ins();
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

/// Ada connected and a call of `tool` on its way, its answer held at the
/// Space; meanwhile a second mind attached as Ada calls, finds that the
/// account needs a sign-in, and leaves the connection waiting for it. The
/// first call then settles against a connection someone else changed, and
/// answers `outcome` with `told`, never that the sponsor revoked anything.
fn flipped_mid_call(
    world: &World,
    label: &str,
    tool: &str,
    arguments: Value,
    expected: (&str, &str),
) {
    let (install, _, mut first) = connected(world, label);
    let route = world.route.as_str();
    let writes = tool == "tangent_post";
    let held = if writes {
        let body = arguments["text"].as_str().expect("the words");
        world.space.listener().hold_answers_to(route, "body", body)
    } else {
        world.space.listener().hold_answers(route)
    };
    first.send(&json!({
        "jsonrpc": "2.0", "id": 80, "method": "tools/call",
        "params": { "name": tool, "arguments": arguments }
    }));
    world
        .space
        .listener()
        .wait_for_handled(route, if writes { 2 } else { 1 });
    world.space.end_sign_ins();
    world.atproto.end_access_tokens();
    world.atproto.refuse_refreshes();
    let mut second = install.attach("ada");
    initialize(&mut second);
    let flipped = call(&mut second, 81, "tangent_arrive", json!({}));
    assert_eq!(
        flipped["result"]["structuredContent"]["outcome"], "needs_sign_in",
        "{flipped}"
    );
    drop(held);
    let late = first.answer(&json!(80));
    assert_eq!(late["result"]["isError"], true, "{late}");
    let structured = &late["result"]["structuredContent"];
    let told = said(&late);
    assert_eq!(structured["outcome"], expected.0, "{late}");
    assert_eq!(told, expected.1);
    assert!(!told.contains("revoked"), "{told}");
    assert_eq!(structured["message"], told.as_str());
    let settled: Vec<String> = install
        .journal()
        .iter()
        .filter(|entry| entry["kind"] == "settled")
        .map(|entry| entry["outcome"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(settled, ["needs_sign_in", expected.0]);
    install.stop();
}

#[test]
fn a_read_whose_account_another_call_found_needs_a_sign_in_says_so_and_shows_nothing() {
    let world = world();
    let topic = world.welcome.clone();
    flipped_mid_call(
        &world,
        "space-flipped-read",
        "tangent_read_topic",
        json!({ "topic": topic }),
        (
            "needs_sign_in",
            "The account behind Tangent Space at main needs the sponsor to sign it in again on the manager page; this read's answer is not shown, and its tools come back once they do.",
        ),
    );
}

#[test]
fn a_write_whose_account_another_call_found_needs_a_sign_in_is_unknown_and_says_why() {
    let world = world();
    let topic = world.welcome.clone();
    flipped_mid_call(
        &world,
        "space-flipped-write",
        "tangent_post",
        json!({ "topic": topic, "text": "Held at the door.", "request_id": "held-1" }),
        (
            "outcome_unknown",
            "The account behind Tangent Space at main needed signing in again while this write was on its way, so the lobby cannot say whether the service carried it out. Once the sponsor signs it in again, read the target before writing again.",
        ),
    );
    assert!(
        world
            .space
            .posts()
            .iter()
            .any(|post| post.text == "Held at the door."),
        "the Post did land: unknown is the honest answer"
    );
}

#[test]
fn a_write_whose_own_renewal_succeeded_while_another_call_found_the_account_signed_out_is_unknown()
{
    let world = world();
    let (install, _, mut first) = connected(&world, "space-renewed-then-flipped");
    // The Space ended the sign-in, so the first call proves the account again,
    // and that proof is minted only after its account is renewed at its
    // server, which succeeds; the Space then refuses the Post, and that answer
    // is held while a second mind finds the account signed out.
    world.space.end_sign_ins();
    world.space.refuse_replies_in("welcome");
    world.atproto.end_access_tokens();
    let route = world.route.as_str();
    let held = world
        .space
        .listener()
        .hold_answers_to(route, "body", "Renewed first.");
    first.send(&json!({
        "jsonrpc": "2.0", "id": 90, "method": "tools/call",
        "params": { "name": "tangent_post", "arguments": { "topic": world.welcome, "text": "Renewed first.", "request_id": "renewed-1" } }
    }));
    // The page read as a visitor, the page read again, and the form.
    world.space.listener().wait_for_handled(route, 3);
    assert_eq!(
        world.space.listener().count(EXCHANGE),
        2,
        "the first call established its connection again"
    );
    world.space.end_sign_ins();
    world.atproto.end_access_tokens();
    world.atproto.refuse_refreshes();
    let mut second = install.attach("ada");
    initialize(&mut second);
    let flipped = call(&mut second, 91, "tangent_arrive", json!({}));
    assert_eq!(
        flipped["result"]["structuredContent"]["outcome"], "needs_sign_in",
        "{flipped}"
    );
    drop(held);
    let late = first.answer(&json!(90));
    assert_eq!(late["result"]["isError"], true, "{late}");
    let told = said(&late);
    assert_eq!(
        late["result"]["structuredContent"]["outcome"], "outcome_unknown",
        "a write someone else's change cut off is never said to have done nothing: {told}"
    );
    assert_eq!(
        told,
        "The account behind Tangent Space at main needed signing in again while this write was on its way, so the lobby cannot say whether the service carried it out. Once the sponsor signs it in again, read the target before writing again."
    );
    assert!(!told.contains("nothing was done"), "{told}");
    // The journal says the same: the first call's renewal kept the account
    // signed in, the second call's found it needs a sign-in, and nothing
    // was revoked.
    let journal = install.journal();
    let ended = |command: &str| -> Vec<String> {
        journal
            .iter()
            .filter(|entry| entry["command"] == command && entry["outcome"] != "pending")
            .map(|entry| entry["outcome"].as_str().unwrap_or_default().to_owned())
            .collect()
    };
    let mut renewals = ended("renew_credential");
    renewals.sort();
    assert_eq!(renewals, ["done", "needs_sign_in"]);
    let settled: Vec<&str> = journal
        .iter()
        .filter(|entry| entry["kind"] == "settled")
        .filter_map(|entry| entry["outcome"].as_str())
        .collect();
    assert_eq!(settled, ["needs_sign_in", "outcome_unknown"]);
    install.stop();
}

#[test]
fn an_argument_the_mind_got_wrong_is_named_in_the_refusal() {
    let world = world();
    let (install, _, mut peer) = connected(&world, "space-arguments");
    let requests = world.space.listener().requests().len();
    let misspelled = call(
        &mut peer,
        100,
        "tangent_post",
        json!({ "topic": world.welcome, "txt": "Hello." }),
    );
    assert_eq!(misspelled["result"]["isError"], true, "{misspelled}");
    assert_eq!(
        misspelled["result"]["structuredContent"]["outcome"],
        "invalid_input"
    );
    assert_eq!(
        said(&misspelled),
        "tangent_post takes no argument called \"txt\"; it takes reply_to, request_id, text, topic and at."
    );
    let missing = call(&mut peer, 101, "tangent_read_topic", json!({}));
    assert_eq!(
        said(&missing),
        "topic is missing; this tool needs it, as its description says."
    );
    let not_a_topic = call(
        &mut peer,
        102,
        "tangent_read_topic",
        json!({ "topic": world.opening }),
    );
    assert!(
        said(&not_a_topic).starts_with("topic is not a Topic's reference."),
        "{not_a_topic}"
    );
    let foreign = call(
        &mut peer,
        103,
        "tangent_post",
        json!({ "topic": "https://elsewhere.example.test/t/home/welcome", "text": "Hi." }),
    );
    assert!(
        said(&foreign).starts_with("topic holds a reference of another Space."),
        "{foreign}"
    );
    for answer in [&missing, &not_a_topic, &foreign] {
        assert_eq!(
            answer["result"]["structuredContent"]["message"],
            said(answer).as_str()
        );
    }
    assert_eq!(
        world.space.listener().requests().len(),
        requests,
        "nothing reached the Space"
    );
    install.stop();
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
    assert!(text.contains("There is more: pass cursor"), "{text}");
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
    assert!(structured["message"].is_string(), "{structured}");
    assert!(structured["through"].is_u64(), "{structured}");

    // A host that shows the structured answer alone follows its cursor to
    // the Posts this answer had no room for, and reaches the newest.
    let mut cursor = structured["cursor"].as_str().expect("a cursor").to_owned();
    let mut reached = Vec::new();
    for id in 51..70 {
        let next = call(
            &mut peer,
            id,
            "tangent_read_topic",
            json!({ "topic": world.welcome, "cursor": cursor }),
        );
        let structured = &next["result"]["structuredContent"];
        reached.extend(structured["posts"].as_array().expect("posts").clone());
        let Some(further) = structured["cursor"].as_str() else {
            break;
        };
        cursor = further.to_owned();
    }
    assert_eq!(
        reached.last().expect("the newest")["post"],
        planted.last().expect("planted").as_str(),
        "the newest Post is reached"
    );
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
    // What the walk's control page did once the connection was made: a Post
    // that is new to the mind when it arrives.
    new_post(&world.space);

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
                for field in ["tools", "accounts", "connections", "waiting", "message"] {
                    assert_eq!(got[field], want[field], "reach's {field}");
                }
                for field in ["displayName", "moniker"] {
                    assert_eq!(
                        got["companion"][field], want["companion"][field],
                        "reach's companion {field}"
                    );
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
    assert!(reply.reply_to.is_some());
    install.stop();
}
