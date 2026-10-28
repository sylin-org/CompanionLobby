//! One account serves two services, through the real daemon and the real
//! command, against the test kit's fakes.
//!
//! The journeys run `companion-lobby-synthetic` with `LOBBY_AUTHSERVER`
//! naming the fake atproto world, so its registry holds the atproto
//! credential adapter, the Tangent service adapter and the Bluesky service
//! adapter. A companion signs in once with the fake account; the sponsor
//! grants it a connection to the fake Space and one to Bluesky, which lives
//! on the account's own fake PDS and needs no address. A mind attaches
//! through the real shim. Nothing is seeded in the lobby: the credential
//! exists because a sign-in completed, each connection because its service
//! adapter established it.

use std::path::Path;

use lobby_conformance::host::{Install, body, call, initialize, names, text, tools};
use lobby_daemon::mcp::LIST_CHANGED;
use lobby_testkit::atproto::AtprotoWorld;
use lobby_testkit::secrets_in;
use lobby_testkit::space::SpaceServer;
use serde_json::{Value, json};

const SYNTHETIC: &str = env!("CARGO_BIN_EXE_companion-lobby-synthetic");
const ADA: &str = "did:plc:adaadaadaadaadaadaadaada";
const HANDLE: &str = "ada.example.test";
const BEA: &str = "did:plc:beabeabeabeabeabeabeabea";
const TANGENT_ONLY: &str = "atproto rpc:org.sylin.tangent.exchangeIdentity?aud=*";

struct World {
    atproto: AtprotoWorld,
    space: SpaceServer,
    opening: String,
}

fn world() -> World {
    let atproto = AtprotoWorld::start();
    atproto.add_account(HANDLE, ADA);
    atproto.add_account("bea.example.test", BEA);
    atproto.display_name(BEA, "Bea");
    let opening = atproto.add_post(BEA, "Who else is growing basil this year?");
    atproto.sign_in_as(ADA);
    let space = SpaceServer::start(&atproto.proofs());
    space.add_tangent("home", "Home", "Where everyone starts.");
    space.add_topic("home", "welcome", "Welcome", "Say hello.");
    space.name_account(ADA, "Ada", HANDLE);
    let bea = space.add_participant("Bea", "bea.example.test");
    space.add_post("welcome", &bea, "Welcome, Ada!", None);
    World {
        atproto,
        space,
        opening,
    }
}

/// A fresh install with Ada signed in once with the fake account.
fn signed_in(world: &World, label: &str) -> (Install, String, String) {
    let install = Install::new(Path::new(SYNTHETIC), label)
        .with_env("LOBBY_AUTHSERVER", &world.atproto.origin());
    install.open();
    let ada = install.create("ada");
    let page = install.sign_in_again(&ada, "atproto", |url| world.atproto.approve(url));
    assert_eq!(page.status, 200, "{}", text(&page));
    let credential = install.json("/api/lobby")["credentials"][0]["id"]
        .as_str()
        .expect("a credential")
        .to_owned();
    (install, ada, credential)
}

fn grant(install: &Install, ada: &str, credential: &str, service: &str, address: &str) -> Value {
    let label = if service == "tangent" { "main" } else { "" };
    let granted = install.operator(
        "POST",
        "/api/connections",
        &json!({
            "companion": ada,
            "credential": credential,
            "service": service,
            "address": address,
            "label": label,
        }),
    );
    assert_eq!(granted.status, 201, "{service}: {}", text(&granted));
    body(&granted)["connection"].clone()
}

/// Ada signed in and connected to the Space as `main` and to Bluesky.
fn connected(world: &World, label: &str) -> (Install, String, String, Value, Value) {
    let (install, ada, credential) = signed_in(world, label);
    let tangent = grant(
        &install,
        &ada,
        &credential,
        "tangent",
        &world.space.origin(),
    );
    let bluesky = grant(&install, &ada, &credential, "bluesky", "");
    (install, ada, credential, tangent, bluesky)
}

fn said(answer: &Value) -> String {
    answer["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

fn state_of(install: &Install, kind: &str, id: &str) -> Value {
    install.json("/api/lobby")[kind]
        .as_array()
        .expect("records")
        .iter()
        .find(|record| record["id"] == id)
        .cloned()
        .unwrap_or_else(|| panic!("{kind} {id}"))
}

fn has_tools_of(listed: &[String], service: &str) -> bool {
    listed
        .iter()
        .any(|name| name.starts_with(&format!("{service}_")))
}

#[test]
fn one_account_serves_the_space_and_bluesky_and_every_call_is_journaled_to_its_service() {
    let world = world();
    let (install, ada, credential, tangent, bluesky) = connected(&world, "bluesky-both");

    // The one sign-in asked for what both services need.
    let held = state_of(&install, "credentials", &credential);
    let permissions = held["permissions"].as_array().expect("permissions");
    for needed in lobby_service_bluesky::PERMISSIONS
        .iter()
        .chain([lobby_service_tangent::PERMISSION].iter())
    {
        assert!(permissions.contains(&json!(needed)), "{needed} in {held}");
    }

    // Bluesky needs no address: it reaches the account's own PDS, and one
    // account holds one such connection.
    assert_eq!(bluesky["address"], json!(world.atproto.pds_origin()));
    assert_eq!(bluesky["label"], "bluesky");
    assert_eq!(bluesky["credential"], tangent["credential"]);
    let again = install.operator(
        "POST",
        "/api/connections",
        &json!({ "companion": ada, "credential": credential, "service": "bluesky", "address": "", "label": "" }),
    );
    assert_eq!(again.status, 409, "{}", text(&again));
    let addressed = install.operator(
        "POST",
        "/api/connections",
        &json!({ "companion": ada, "credential": credential, "service": "bluesky", "address": world.space.origin(), "label": "other" }),
    );
    assert_eq!(addressed.status, 400, "{}", text(&addressed));

    // Both tool sets appear, and reach names both connections.
    let mut peer = install.attach("ada");
    initialize(&mut peer);
    let listed = names(&tools(&mut peer, 1));
    for expected in [
        "reach",
        "tangent_arrive",
        "bluesky_timeline",
        "bluesky_post",
        "bluesky_reply",
        "bluesky_like",
        "bluesky_repost",
    ] {
        assert!(
            listed.contains(&expected.to_owned()),
            "{expected} in {listed:?}"
        );
    }
    let reach = said(&call(&mut peer, 2, "reach", json!({})));
    assert!(reach.contains("Bluesky"), "{reach}");
    assert!(reach.contains("Tangent Space"), "{reach}");

    // Reading, posting and liking on Bluesky; arriving in the Space.
    let timeline = call(&mut peer, 3, "bluesky_timeline", json!({}));
    assert_eq!(timeline["result"]["isError"], false, "{timeline}");
    assert!(
        said(&timeline).contains("\"Who else is growing basil this year?\""),
        "{}",
        said(&timeline)
    );
    let posted = call(
        &mut peer,
        4,
        "bluesky_post",
        json!({ "text": "Hello from the lobby." }),
    );
    assert_eq!(posted["result"]["isError"], false, "{posted}");
    let liked = call(
        &mut peer,
        5,
        "bluesky_like",
        json!({ "post": world.opening }),
    );
    assert_eq!(liked["result"]["isError"], false, "{liked}");
    let arrived = call(&mut peer, 6, "tangent_arrive", json!({}));
    assert_eq!(arrived["result"]["isError"], false, "{arrived}");
    let posts = world.atproto.records("app.bsky.feed.post");
    let mine: Vec<_> = posts.iter().filter(|record| record.did == ADA).collect();
    assert_eq!(mine.len(), 1, "posted once, in Ada's own repository");
    assert_eq!(mine[0].value["text"], "Hello from the lobby.");
    assert_eq!(world.atproto.records("app.bsky.feed.like").len(), 1);

    // The journal names every call by its service and the connection it
    // went through.
    let settled: Vec<(String, String, String)> = install
        .journal()
        .iter()
        .filter(|entry| entry["kind"] == "settled" && entry["tool"].is_string())
        .map(|entry| {
            (
                entry["tool"].as_str().unwrap_or_default().to_owned(),
                entry["service"].as_str().unwrap_or_default().to_owned(),
                entry["connection"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    let bluesky_id = bluesky["id"].as_str().expect("id").to_owned();
    let tangent_id = tangent["id"].as_str().expect("id").to_owned();
    assert_eq!(
        settled,
        [
            ("timeline", "bluesky", &bluesky_id),
            ("post", "bluesky", &bluesky_id),
            ("like", "bluesky", &bluesky_id),
            ("arrive", "tangent", &tangent_id),
        ]
        .iter()
        .map(|(tool, service, connection)| (
            (*tool).to_owned(),
            (*service).to_owned(),
            (*connection).clone()
        ))
        .collect::<Vec<_>>()
    );

    // Every request to the PDS carried the token under DPoP with a proof of
    // its own; the token was never sent as a bearer token.
    let requests = world.atproto.pds().requests();
    let resources: Vec<_> = requests
        .iter()
        .filter(|request| {
            request.route().starts_with("/xrpc/")
                && request.route() != "/xrpc/com.atproto.server.getServiceAuth"
        })
        .collect();
    assert!(resources.len() >= 5, "{}", resources.len());
    for request in &resources {
        let authorization = request.header("authorization").unwrap_or_default();
        assert!(
            authorization.starts_with("DPoP ") && request.header("dpop").is_some(),
            "{} was proved",
            request.route()
        );
    }
    assert!(
        requests.iter().all(|request| !request
            .header("authorization")
            .unwrap_or_default()
            .starts_with("Bearer")),
        "a DPoP-bound token never goes as a bearer token"
    );

    // No token or proof reaches any projection or plain file.
    let mut secrets: Vec<String> = world.space.tokens();
    secrets.extend(world.atproto.issued());
    secrets.extend(
        resources
            .iter()
            .filter_map(|request| request.header("dpop").map(str::to_owned)),
    );
    let secrets: Vec<&str> = secrets.iter().map(String::as_str).collect();
    let journal = Value::Array(install.journal()).to_string();
    assert!(secrets_in(journal.as_bytes(), &secrets).is_empty());
    let lobby = install.json("/api/lobby").to_string();
    assert!(secrets_in(lobby.as_bytes(), &secrets).is_empty());
    assert!(secrets_in(&install.plain_files(), &secrets).is_empty());
    install.stop();
}

#[test]
fn revoking_the_account_takes_both_tool_sets_away() {
    let world = world();
    let (install, _ada, credential, tangent, bluesky) = connected(&world, "bluesky-pull");
    let mut peer = install.attach("ada");
    initialize(&mut peer);
    let listed = names(&tools(&mut peer, 1));
    assert!(has_tools_of(&listed, "tangent") && has_tools_of(&listed, "bluesky"));

    let revoked = install.operator(
        "POST",
        &format!("/api/credentials/{credential}/revoke"),
        &json!({}),
    );
    assert_eq!(revoked.status, 200, "{}", text(&revoked));
    peer.notification(LIST_CHANGED);
    let listed = names(&tools(&mut peer, 2));
    assert!(
        !has_tools_of(&listed, "tangent") && !has_tools_of(&listed, "bluesky"),
        "{listed:?}"
    );
    for connection in [&tangent, &bluesky] {
        let id = connection["id"].as_str().expect("id");
        assert_eq!(state_of(&install, "connections", id)["state"], "revoked");
    }
    let before = world.atproto.pds().requests().len();
    let refused = call(&mut peer, 3, "bluesky_timeline", json!({}));
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    assert_eq!(
        world.atproto.pds().requests().len(),
        before,
        "nothing reached the PDS"
    );
    install.stop();
}

#[test]
fn revoking_only_the_bluesky_connection_leaves_the_space_ready() {
    let world = world();
    let (install, _ada, credential, tangent, bluesky) = connected(&world, "bluesky-one");
    let mut peer = install.attach("ada");
    initialize(&mut peer);
    let bluesky_id = bluesky["id"].as_str().expect("id");
    let revoked = install.operator(
        "POST",
        &format!("/api/connections/{bluesky_id}/revoke"),
        &json!({}),
    );
    assert_eq!(revoked.status, 200, "{}", text(&revoked));
    peer.notification(LIST_CHANGED);
    let listed = names(&tools(&mut peer, 1));
    assert!(has_tools_of(&listed, "tangent"), "{listed:?}");
    assert!(!has_tools_of(&listed, "bluesky"), "{listed:?}");
    let tangent_id = tangent["id"].as_str().expect("id");
    assert_eq!(
        state_of(&install, "connections", tangent_id)["state"],
        "ready"
    );
    assert_eq!(
        state_of(&install, "credentials", &credential)["state"],
        "ready"
    );
    let arrived = call(&mut peer, 2, "tangent_arrive", json!({}));
    assert_eq!(arrived["result"]["isError"], false, "{arrived}");

    // Bluesky can be opened again from the same account.
    let ada = install.json("/api/lobby")["companions"][0]["id"]
        .as_str()
        .expect("ada")
        .to_owned();
    let reopened = grant(&install, &ada, &credential, "bluesky", "");
    assert_eq!(reopened["state"], "ready");
    install.stop();
}

#[test]
fn signing_in_again_wider_keeps_the_account_and_never_touches_the_space() {
    let world = world();
    // An account signed in when the provider granted only what the Space
    // needs, as one signed in before Bluesky was in the lobby.
    world.atproto.grants_scope(TANGENT_ONLY);
    let (install, ada, credential) = signed_in(&world, "bluesky-wider");
    let tangent = grant(
        &install,
        &ada,
        &credential,
        "tangent",
        &world.space.origin(),
    );
    let tangent_id = tangent["id"].as_str().expect("id").to_owned();
    let mut peer = install.attach("ada");
    initialize(&mut peer);
    assert!(!has_tools_of(&names(&tools(&mut peer, 1)), "bluesky"));

    // Bluesky is refused with what to do, before anything reaches the PDS.
    let before = world.atproto.pds().requests().len();
    let refused = install.operator(
        "POST",
        "/api/connections",
        &json!({ "companion": ada, "credential": credential, "service": "bluesky", "address": "", "label": "" }),
    );
    assert_eq!(refused.status, 403, "{}", text(&refused));
    assert_eq!(body(&refused)["outcome"], "needs_permission");
    assert!(
        text(&refused).contains("Sign ada in again to grant it"),
        "{}",
        text(&refused)
    );
    assert_eq!(world.atproto.pds().requests().len(), before);

    // A sign-in that comes back as another account changes nothing: the
    // sign-in named the account it widens, so another one never lands.
    world.atproto.grants_what_is_asked();
    world
        .atproto
        .add_account("cy.example.test", "did:plc:cycycycycycycycycycycycy");
    world.atproto.sign_in_as("did:plc:cycycycycycycycycycycycy");
    let other = install.sign_in_again(&ada, "atproto", |url| world.atproto.approve(url));
    assert_eq!(other.status, 403, "{}", text(&other));
    let held = state_of(&install, "credentials", &credential);
    assert_eq!(held["state"], "ready");
    assert_eq!(
        held["permissions"],
        json!(TANGENT_ONLY.split(' ').collect::<Vec<_>>())
    );

    // A sign-in that grants less than the account held changes nothing.
    world.atproto.sign_in_as(ADA);
    world
        .atproto
        .grants_scope("atproto repo:app.bsky.feed.post");
    let narrower = install.sign_in_again(&ada, "atproto", |url| world.atproto.approve(url));
    assert_eq!(narrower.status, 409, "{}", text(&narrower));
    assert!(
        text(&narrower).contains("never narrows"),
        "{}",
        text(&narrower)
    );
    assert_eq!(
        state_of(&install, "credentials", &credential)["permissions"],
        held["permissions"]
    );

    // The same account signed in again, wider: the sign-in starts though
    // the account is signed in, names the account, and the Space stays
    // ready and usable while it waits and after it lands.
    world.atproto.grants_what_is_asked();
    let started = install.operator(
        "POST",
        "/api/sign-ins",
        &json!({ "companion": ada, "kind": "atproto", "hint": "" }),
    );
    assert_eq!(started.status, 201, "{}", text(&started));
    assert_eq!(body(&started)["signIn"]["account"], HANDLE);
    let tangent_before = state_of(&install, "connections", &tangent_id);
    assert_eq!(tangent_before["state"], "ready");
    let waiting = call(&mut peer, 2, "tangent_arrive", json!({}));
    assert_eq!(waiting["result"]["isError"], false, "{waiting}");
    let url = body(&started)["url"].as_str().expect("url").to_owned();
    let landed = install.arrive(&world.atproto.approve(&url));
    assert_eq!(landed.status, 200, "{}", text(&landed));

    let widened = state_of(&install, "credentials", &credential);
    assert_eq!(widened["state"], "ready", "the same credential, kept");
    let granted = widened["permissions"].as_array().expect("permissions");
    for word in TANGENT_ONLY.split(' ') {
        assert!(granted.contains(&json!(word)), "never narrower: {widened}");
    }
    for needed in lobby_service_bluesky::PERMISSIONS {
        assert!(granted.contains(&json!(needed)), "wider: {widened}");
    }
    assert_eq!(
        install.json("/api/lobby")["credentials"]
            .as_array()
            .map(Vec::len),
        Some(1),
        "no second credential"
    );
    assert_eq!(
        state_of(&install, "connections", &tangent_id),
        tangent_before,
        "the Space's connection never changed"
    );
    assert!(
        install
            .journal()
            .iter()
            .all(|entry| entry["connection"] != json!(tangent_id)
                || entry["command"].is_null()
                || entry["command"] == "grant_connection"),
        "no command touched the Space's connection"
    );
    let after = call(&mut peer, 3, "tangent_arrive", json!({}));
    assert_eq!(after["result"]["isError"], false, "{after}");

    // Now Bluesky opens, and the account holds everything, so another
    // sign-in is refused as held.
    let bluesky = grant(&install, &ada, &credential, "bluesky", "");
    peer.notification(LIST_CHANGED);
    assert!(has_tools_of(&names(&tools(&mut peer, 4)), "bluesky"));
    let posted = call(
        &mut peer,
        5,
        "bluesky_post",
        json!({ "text": "Signed in again, and here." }),
    );
    assert_eq!(posted["result"]["isError"], false, "{posted}");
    assert_eq!(bluesky["state"], "ready");
    let held = install.operator(
        "POST",
        "/api/sign-ins",
        &json!({ "companion": ada, "kind": "atproto", "hint": "" }),
    );
    assert_eq!(held.status, 409, "{}", text(&held));
    install.stop();
}
