//! Trouble, keeping a credential alive, and revoking it everywhere,
//! through the real daemon and the real command, against the test kit's
//! fakes.
//!
//! The journeys run `companion-lobby-synthetic`, whose registry holds the
//! passbook and the board, and the atproto adapter pointed at the fake world
//! `LOBBY_AUTHSERVER` names. A mind attaches through the real shim. Keys and
//! access tokens are issued to live a second, so the daemon's own loop
//! renews them while the test watches; nothing is seeded, and every
//! credential exists because a sign-in completed and every connection
//! because the board admitted it.

use std::path::Path;
use std::time::{Duration, Instant};

use lobby_conformance::host::{
    Install, approve_at_authority, at_of, body, call, initialize, names, text, tools,
};
use lobby_daemon::mcp::LIST_CHANGED;
use lobby_testkit::atproto::{AtprotoWorld, Flaw};
use lobby_testkit::board::BoardServer;
use lobby_testkit::passbook::PassbookAuthority;
use serde_json::{Value, json};

const SYNTHETIC: &str = env!("CARGO_BIN_EXE_companion-lobby-synthetic");

const ADA: &str = "did:plc:adaadaadaadaadaadaadaada";
const HANDLE: &str = "ada.example.test";

/// How long a test waits for something that should not happen.
const QUIET: Duration = Duration::from_millis(2500);

fn world() -> AtprotoWorld {
    let world = AtprotoWorld::start();
    world.add_account(HANDLE, ADA);
    world.sign_in_as(ADA);
    world
}

fn install(world: &AtprotoWorld, label: &str) -> Install {
    Install::new(Path::new(SYNTHETIC), label).with_env("LOBBY_AUTHSERVER", &world.origin())
}

/// Waits until `done`, failing after twenty seconds with `what`.
fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn credential_of(install: &Install, companion: &str, kind: &str) -> Value {
    install.json("/api/lobby")["credentials"]
        .as_array()
        .expect("credentials")
        .iter()
        .rev()
        .find(|credential| credential["companion"] == companion && credential["kind"] == kind)
        .cloned()
        .expect("a credential")
}

fn connection_states(install: &Install) -> Vec<String> {
    install.json("/api/lobby")["connections"]
        .as_array()
        .expect("connections")
        .iter()
        .map(|connection| connection["state"].as_str().expect("state").to_owned())
        .collect()
}

fn outcomes_of(install: &Install, command: &str) -> Vec<String> {
    install
        .journal()
        .iter()
        .filter(|entry| entry["command"] == command)
        .map(|entry| entry["outcome"].as_str().expect("outcome").to_owned())
        .collect()
}

fn sentences(install: &Install) -> Vec<String> {
    install
        .journal()
        .iter()
        .filter_map(|entry| entry["sentence"].as_str().map(str::to_owned))
        .collect()
}

/// Signs `companion` in with atproto, with nothing typed, and answers the
/// page the browser lands on.
fn atproto_sign_in(install: &Install, world: &AtprotoWorld, companion: &str) {
    let started = install.operator(
        "POST",
        "/api/sign-ins",
        &json!({ "companion": companion, "kind": "atproto", "hint": "" }),
    );
    assert_eq!(started.status, 201, "{}", text(&started));
    let url = body(&started)["url"].as_str().expect("url").to_owned();
    let page = install.arrive(&world.approve(&url));
    assert_eq!(page.status, 200, "{}", text(&page));
}

#[test]
fn an_expired_key_and_an_expired_access_token_are_renewed_without_the_mind_noticing() {
    let world = world();
    let authority = PassbookAuthority::start();
    let board = BoardServer::start(&authority);
    board.add_post("bea", "Welcome.");
    let install = install(&world, "renewed");
    install.open();
    let ada = install.create("ada");

    // A key that lives a second is renewed before the board's stamp is
    // minted, and again and again by the daemon's own loop.
    authority.key_lifetime(1);
    let credential = install.signed_in(&ada, &authority, "holder-ada");
    install.connected(&ada, &credential, &board, "front");
    let mut peer = install.attach("ada");
    initialize(&mut peer);
    assert_eq!(
        names(&tools(&mut peer, 1)),
        ["board_post", "board_read", "reach"]
    );
    let renewed = authority.renewals_presented().len();
    wait_until("the daemon renewed the key on its own", || {
        authority.renewals_presented().len() >= renewed + 2
    });
    authority.key_lifetime(3600);
    let presented = authority.renewals_presented();
    let mut distinct = presented.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        presented.len(),
        "no renewal is presented twice"
    );

    // The mind never noticed: its call works, its tools never changed.
    let read = call(&mut peer, 2, "board_read", json!({}));
    assert_eq!(read["result"]["isError"], false, "{read}");
    assert!(
        peer.notification_within(LIST_CHANGED, QUIET).is_none(),
        "a renewal changes no tool"
    );
    assert_eq!(credential_of(&install, &ada, "passbook")["state"], "ready");
    assert!(
        outcomes_of(&install, "renew_credential")
            .iter()
            .all(|outcome| outcome == "pending" || outcome == "done")
    );
    assert!(
        sentences(&install)
            .contains(&"The lobby kept holder-ada signed in for ada with Passbook.".to_owned()),
        "the journal says the lobby kept it signed in"
    );

    // An atproto access token that lives a second is refreshed with the
    // client id the sign-in was granted under, and the next refresh presents
    // the rotated refresh token.
    world.access_lifetime(1);
    atproto_sign_in(&install, &world, &ada);
    let first = world.live_refresh_token(ADA).expect("a refresh token");
    wait_until("the daemon refreshed the access token", || {
        world.refresh_tokens_presented().len() >= 2
    });
    world.access_lifetime(3600);
    let presented = world.refresh_tokens_presented();
    assert_eq!(presented[0], first, "the token the sign-in issued");
    assert_ne!(
        presented[1], first,
        "the rotated token, never the spent one"
    );
    let requests = world.server().requests();
    let pushed_client_id = requests
        .iter()
        .find(|request| request.route() == "/oauth/par")
        .and_then(|request| request.field("client_id"))
        .expect("the client id the sign-in pushed");
    for refresh in requests.iter().filter(|request| {
        request.route() == "/oauth/token"
            && request.field("grant_type").as_deref() == Some("refresh_token")
    }) {
        assert_eq!(
            refresh.field("client_id").as_deref(),
            Some(pushed_client_id.as_str()),
            "every refresh presents the stored client id"
        );
    }
    assert_eq!(credential_of(&install, &ada, "atproto")["state"], "ready");
    assert!(
        peer.notification_within(LIST_CHANGED, Duration::from_millis(500))
            .is_none()
    );
    install.stop();
}

#[test]
fn a_refused_renewal_shrinks_the_catalog_in_one_notice_and_signing_in_again_restores_it() {
    let world = world();
    let authority = PassbookAuthority::start();
    let front = BoardServer::start(&authority);
    let back = BoardServer::start(&authority);
    let install = install(&world, "refused");
    install.open();
    let ada = install.create("ada");
    authority.key_lifetime(1);
    let credential = install.signed_in(&ada, &authority, "holder-ada");
    install.connected(&ada, &credential, &front, "front");
    install.connected(&ada, &credential, &back, "back");
    let admitted = front.listener().count("/admit") + back.listener().count("/admit");
    let mut peer = install.attach("ada");
    initialize(&mut peer);
    assert_eq!(
        at_of(&tools(&mut peer, 1), "board_read"),
        json!(["front", "back"])
    );

    // The authority ends the holder's keys: the next renewal is refused.
    authority.refuse_renewals(true);
    peer.notification(LIST_CHANGED);
    assert!(
        peer.notification_within(LIST_CHANGED, QUIET).is_none(),
        "both connections leave the catalog in one notice"
    );
    assert_eq!(names(&tools(&mut peer, 2)), ["reach"]);
    let lobby = install.json("/api/lobby");
    assert_eq!(lobby["credentials"][0]["state"], "needs_sign_in");
    assert_eq!(
        connection_states(&install),
        ["needs_sign_in", "needs_sign_in"]
    );
    let gone = call(&mut peer, 3, "board_read", json!({ "at": "front" }));
    assert_eq!(gone["result"]["isError"], true, "{gone}");
    assert_eq!(
        gone["result"]["structuredContent"]["outcome"],
        "needs_sign_in"
    );
    let told = gone["result"]["content"][0]["text"].as_str().expect("text");
    assert!(told.contains("sign it in again"), "{told}");
    assert!(told.contains("board at front"), "{told}");
    let named = call(&mut peer, 30, "board_read", json!({ "at": "back" }));
    assert_eq!(
        named["result"]["structuredContent"]["outcome"],
        "needs_sign_in"
    );
    let told = named["result"]["content"][0]["text"]
        .as_str()
        .expect("text");
    assert!(
        told.contains("board at back"),
        "the connection at names is the one said to wait: {told}"
    );
    let reach = call(&mut peer, 4, "reach", json!({}));
    let said = reach["result"]["content"][0]["text"]
        .as_str()
        .expect("text");
    assert!(
        said.contains("cannot reach board at front until the sponsor signs its account in again"),
        "{said}"
    );
    assert!(outcomes_of(&install, "renew_credential").contains(&"needs_sign_in".to_owned()));
    assert!(
        sentences(&install).contains(
            &"The lobby tried to keep holder-ada signed in for ada with Passbook, but Passbook ended that sign-in, so ada needs signing in again."
                .to_owned()
        ),
        "the journal says why, in the sponsor's words"
    );
    let renewals = authority.renewals_presented().len();
    std::thread::sleep(QUIET);
    assert_eq!(
        authority.renewals_presented().len(),
        renewals,
        "a credential that needs signing in is never renewed"
    );

    // Another account cannot take its place while it waits.
    let other = install.sign_in(&ada, &authority, "holder-other");
    assert_eq!(other.status, 409, "{}", text(&other));
    assert!(
        text(&other).contains("another account than holder-ada, which waits for ada")
            && !text(&other).contains("another companion"),
        "the page names the one conflict it met: {}",
        text(&other)
    );
    assert_eq!(
        credential_of(&install, &ada, "passbook")["state"],
        "needs_sign_in"
    );

    // Signing the same account in again restores the credential and every
    // connection, without a new grant.
    authority.refuse_renewals(false);
    authority.key_lifetime(3600);
    authority.sign_in_as("holder-ada");
    let page = install.sign_in_again(&ada, "passbook", approve_at_authority);
    assert_eq!(page.status, 200, "{}", text(&page));
    let restored = credential_of(&install, &ada, "passbook");
    assert_eq!(restored["id"], credential.as_str(), "the same credential");
    assert_eq!(restored["state"], "ready");
    assert_eq!(connection_states(&install), ["ready", "ready"]);
    peer.notification(LIST_CHANGED);
    assert_eq!(
        at_of(&tools(&mut peer, 5), "board_read"),
        json!(["front", "back"])
    );
    assert_eq!(
        front.listener().count("/admit") + back.listener().count("/admit"),
        admitted,
        "no board admitted anyone again"
    );
    let read = call(&mut peer, 6, "board_read", json!({ "at": "back" }));
    assert_eq!(read["result"]["isError"], false, "{read}");
    install.stop();
}

#[test]
fn a_daemon_killed_between_the_rotation_and_the_commit_comes_back_needing_sign_in() {
    let world = world();
    let install = install(&world, "killed");
    install.open();
    let ada = install.create("ada");
    world.access_lifetime(1);
    // The provider rotates the refresh token; its answer never arrives.
    let held = world
        .server()
        .hold_answers_to("/oauth/token", "grant_type", "refresh_token");
    atproto_sign_in(&install, &world, &ada);
    let credential = credential_of(&install, &ada, "atproto");
    let spent = world
        .live_refresh_token(ADA)
        .expect("the token the sign-in issued");
    wait_until("the provider took the refresh token up", || {
        world.refresh_tokens_presented().len() == 1
    });
    assert_ne!(
        world.live_refresh_token(ADA).as_deref(),
        Some(spent.as_str()),
        "the provider rotated it"
    );
    assert!(
        install.files().contains(&format!(
            "{}.rotating",
            credential["id"].as_str().expect("id")
        )),
        "the mark is on the disk before the token was spent"
    );
    install.kill();
    drop(held);

    // The next daemon finds the mark and never presents the old token again.
    install.open();
    let after = credential_of(&install, &ada, "atproto");
    assert_eq!(after["id"], credential["id"]);
    assert_eq!(after["state"], "needs_sign_in");
    assert_eq!(
        outcomes_of(&install, "end_unfinished_renewal"),
        ["pending", "done"]
    );
    assert!(
        sentences(&install).contains(
            &format!("The lobby stopped while it renewed {HANDLE} for ada with Atmosphere and cannot tell whether Atmosphere renewed it, so ada needs signing in again.")
        )
    );
    std::thread::sleep(QUIET);
    assert_eq!(
        world.refresh_tokens_presented(),
        [spent],
        "the spent token was presented once, and never again"
    );

    // Signing the same account in again restores it.
    world.access_lifetime(3600);
    let page = install.sign_in_again(&ada, "atproto", |url| world.approve(url));
    assert_eq!(page.status, 200, "{}", text(&page));
    let restored = credential_of(&install, &ada, "atproto");
    assert_eq!(restored["id"], credential["id"]);
    assert_eq!(restored["state"], "ready");
    assert!(
        !install
            .files()
            .iter()
            .any(|name| name.ends_with(".rotating")),
        "no mark is left"
    );
    install.stop();
}

#[test]
fn revoking_an_account_reaches_its_provider_and_says_whether_it_confirmed() {
    let world = world();
    let install = install(&world, "revoked");
    install.open();
    let ada = install.create("ada");
    atproto_sign_in(&install, &world, &ada);
    let credential = credential_of(&install, &ada, "atproto");
    let live = world.live_refresh_token(ADA).expect("a live token");
    let revoked = install.operator(
        "POST",
        &format!(
            "/api/credentials/{}/revoke",
            credential["id"].as_str().expect("id")
        ),
        &json!({}),
    );
    assert_eq!(revoked.status, 200, "{}", text(&revoked));
    assert_eq!(body(&revoked)["provider"], "confirmed");
    assert_eq!(body(&revoked)["credential"]["state"], "revoked");
    assert_eq!(world.revocations(), [live], "the provider was told");
    assert_eq!(world.live_refresh_token(ADA), None, "its grant ended there");
    let said = sentences(&install);
    for expected in [
        format!("You revoked {HANDLE}, the Atmosphere account ada held."),
        format!("You revoked {HANDLE} at Atmosphere too."),
    ] {
        assert!(said.contains(&expected), "{expected} in {said:#?}");
    }

    // A provider that offers no revocation is said to, plainly.
    world.break_metadata(Flaw::NoRevocation);
    atproto_sign_in(&install, &world, &ada);
    let again = credential_of(&install, &ada, "atproto");
    let revoked = install.operator(
        "POST",
        &format!(
            "/api/credentials/{}/revoke",
            again["id"].as_str().expect("id")
        ),
        &json!({}),
    );
    assert_eq!(revoked.status, 200, "{}", text(&revoked));
    assert_eq!(body(&revoked)["provider"], "not_offered");
    assert!(sentences(&install).contains(&format!(
        "You tried to revoke {HANDLE} at Atmosphere too, but Atmosphere offers no way to revoke it there, so it ends there when it expires."
    )));
    install.stop();
}

#[test]
fn revoking_an_account_that_waits_sends_its_last_key_and_never_claims_a_confirmation() {
    let world = world();
    let install = install(&world, "waits-revoked");
    install.open();
    let ada = install.create("ada");
    world.access_lifetime(1);
    atproto_sign_in(&install, &world, &ada);
    let credential = credential_of(&install, &ada, "atproto");
    let last = world.live_refresh_token(ADA).expect("a live token");
    world.refuse_refreshes();
    wait_until("the account needs signing in again", || {
        credential_of(&install, &ada, "atproto")["state"] == "needs_sign_in"
    });
    let revoked = install.operator(
        "POST",
        &format!(
            "/api/credentials/{}/revoke",
            credential["id"].as_str().expect("id")
        ),
        &json!({}),
    );
    assert_eq!(revoked.status, 200, "{}", text(&revoked));
    assert_eq!(
        body(&revoked)["provider"],
        "sent_last_known",
        "a provider that answers for a secret a renewal may have spent confirms nothing"
    );
    assert_eq!(
        world.revocations(),
        [last],
        "the provider was still sent the last key the lobby held"
    );
    assert!(sentences(&install).contains(&format!(
        "You tried to revoke {HANDLE} at Atmosphere too, but it waited for a sign-in, so Atmosphere was sent the last key the lobby held, which a renewal may already have replaced; the lobby cannot tell whether it is revoked there."
    )), "{:#?}", sentences(&install));
    install.stop();
}
