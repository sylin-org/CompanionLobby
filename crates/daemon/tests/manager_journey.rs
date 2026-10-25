//! The sponsor's moments on the manager page, through the real daemon and
//! the real command, against the test kit's whole fake world.
//!
//! The journey runs `companion-lobby-synthetic` with `LOBBY_AUTHSERVER`
//! naming the fake world [`LobbyWorld`] stands up, the same world the
//! `lobby-world` command prints for a person to walk in a browser. It plays
//! the page: it reads what the page reads and asks what the page asks, and
//! holds every answer to the page's rules: an account shown by its label
//! with its key kept for the details, a sign-in that remembers the account
//! the sponsor meant across a reload, connections that are ready or revoked
//! and never more, and a journal that reads as sentences with no id, code or
//! secret in them. No credential or connection is seeded: each exists
//! because a sign-in completed or a service adapter established it.

use std::path::Path;
use std::time::Duration;

use lobby_conformance::host::{Install, body, text};
use lobby_contracts::{Address, AddressPolicy};
use lobby_daemon::loopback::{self, Call};
use lobby_outbound::{Outbound, Request};
use lobby_testkit::world::{DID, HANDLE, HOLDER, LobbyWorld};
use lobby_testkit::{Peer, secrets_in};
use serde_json::{Value, json};

const SYNTHETIC: &str = env!("CARGO_BIN_EXE_companion-lobby-synthetic");

/// The smallest file that starts like a PNG image.
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR portrait";

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let bits = chunk.iter().enumerate().fold(0u32, |bits, (index, byte)| {
            bits | u32::from(*byte) << (16 - 8 * index)
        });
        for index in 0..4 {
            if index <= chunk.len() {
                out.push(ALPHABET[(bits >> (18 - 6 * index) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Whether `text` holds a run of 32 hex digits, the shape of every id the
/// lobby mints.
fn holds_an_id(text: &str) -> bool {
    text.as_bytes()
        .windows(32)
        .any(|window| window.iter().all(u8::is_ascii_hexdigit))
}

fn get(install: &Install, path: &str) -> loopback::Answer {
    loopback::send(install.port(), &Call::get(path, Duration::from_secs(10))).expect("a read")
}

fn initialize(peer: &mut Peer) {
    let answer = peer.ask(&json!({
        "jsonrpc": "2.0", "id": 0, "method": "initialize",
        "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "journey", "version": "1" } }
    }));
    assert!(answer["result"].is_object(), "{answer}");
    peer.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
}

fn companion(install: &Install, id: &str) -> Value {
    install.json("/api/lobby")["companions"]
        .as_array()
        .expect("companions")
        .iter()
        .find(|companion| companion["id"] == id)
        .cloned()
        .expect("the companion")
}

fn sentences(install: &Install, query: &str) -> Vec<Value> {
    install.json(&format!("/api/journal?{query}"))["entries"]
        .as_array()
        .expect("entries")
        .clone()
}

#[test]
fn a_sponsor_walks_from_nothing_to_an_attached_companion_with_a_lent_account_and_a_revoked_connection()
 {
    let world = LobbyWorld::start();
    let install = Install::new(Path::new(SYNTHETIC), "manager-walk")
        .with_env("LOBBY_AUTHSERVER", &world.authserver());
    install.open();

    // First run: an empty lobby, the kinds and the services the page offers.
    let lobby = install.json("/api/lobby");
    assert_eq!(lobby["companions"], json!([]));
    assert_eq!(
        lobby["kinds"],
        json!([
            { "kind": "passbook", "title": "Passbook" },
            { "kind": "atproto", "title": "Atmosphere" },
        ])
    );
    assert_eq!(
        lobby["services"],
        json!([{ "name": "board", "title": "Board", "accepts": ["passbook"] }])
    );
    let page = get(&install, "/");
    assert_eq!(page.status, 200);

    // Naming the companion gives one mcpServers block, keyed by its name.
    let created = install.operator(
        "POST",
        "/api/companions",
        &json!({ "moniker": "ada", "displayName": "Ada Lovelace" }),
    );
    assert_eq!(created.status, 201, "{}", text(&created));
    let ada = body(&created)["companion"].clone();
    let id = ada["id"].as_str().expect("id").to_owned();
    assert_eq!(ada["portrait"], Value::Null, "initials until a portrait");
    let block: Value =
        serde_json::from_str(ada["hostConfig"].as_str().expect("block")).expect("JSON");
    let servers = block["mcpServers"].as_object().expect("mcpServers");
    assert_eq!(servers.keys().collect::<Vec<_>>(), ["ada"]);
    assert_eq!(servers["ada"]["args"], json!(["attach", id]));
    assert_eq!(
        get(&install, &format!("/companion/{id}/attach")).status,
        200
    );

    // A portrait, recognized by its own bytes; anything else is refused.
    let refused = install.operator(
        "PUT",
        &format!("/api/companions/{id}/portrait"),
        &json!({ "expectedRevision": 1, "image": format!("data:image/png;base64,{}", base64(b"<svg onload=x>")) }),
    );
    assert_eq!(refused.status, 400, "{}", text(&refused));
    let portrayed = install.operator(
        "PUT",
        &format!("/api/companions/{id}/portrait"),
        &json!({ "expectedRevision": 1, "image": format!("data:image/png;base64,{}", base64(PNG)) }),
    );
    assert_eq!(portrayed.status, 200, "{}", text(&portrayed));
    let portrait = body(&portrayed)["companion"]["portrait"]
        .as_str()
        .expect("a portrait path")
        .to_owned();
    assert!(portrait.starts_with(&format!("/api/companions/{id}/portrait")));
    let lobby_origin = Address::parse(
        &format!("http://127.0.0.1:{}", install.port()),
        AddressPolicy::LoopbackAllowed,
    )
    .expect("the lobby's origin");
    let image = Outbound::new(AddressPolicy::LoopbackAllowed)
        .send(&lobby_origin, Request::new("GET", &portrait))
        .expect("the portrait");
    assert_eq!(image.status, 200);
    assert_eq!(image.body, PNG);
    assert_eq!(image.header("content-type"), Some("image/png"));

    // A host attaches as the companion, and the page sees it.
    let mut peer = install.attach("ada");
    initialize(&mut peer);
    assert_eq!(companion(&install, &id)["attachments"], 1);

    // Lending an identity: the handoff remembers the account meant, from
    // the lobby, across a reload or a second tab.
    let started = install.operator(
        "POST",
        "/api/sign-ins",
        &json!({ "companion": id, "kind": "atproto", "hint": HANDLE }),
    );
    assert_eq!(started.status, 201, "{}", text(&started));
    let sign_in = body(&started)["signIn"].clone();
    assert_eq!(sign_in["account"], HANDLE);
    let url = body(&started)["url"].as_str().expect("url").to_owned();
    let handoff = format!(
        "/companion/{id}/sign-in/{}",
        sign_in["id"].as_str().expect("id")
    );
    assert_eq!(get(&install, &handoff).status, 200);
    let reloaded = install.json("/api/lobby")["signIns"].clone();
    assert_eq!(
        reloaded,
        json!([sign_in]),
        "a reload reads the same sign-in"
    );
    let back = install.arrive(&world.atproto.approve(&url));
    assert_eq!(back.status, 200, "{}", text(&back));
    let ended = install.json("/api/lobby")["signIns"].clone();
    assert_eq!(ended[0]["status"], "signed_in");
    assert_eq!(ended[0]["account"], HANDLE, "the handoff still names it");

    // A second account, of the kind the board takes.
    let page = install.sign_in(&id, &world.passbook, HOLDER);
    assert_eq!(page.status, 200, "{}", text(&page));
    let credentials = install.json("/api/lobby")["credentials"].clone();
    let passbook = credentials
        .as_array()
        .expect("credentials")
        .iter()
        .find(|credential| credential["kind"] == "passbook")
        .and_then(|credential| credential["id"].as_str())
        .expect("the passbook credential")
        .to_owned();
    let labels: Vec<(&str, &str, &str)> = credentials
        .as_array()
        .expect("credentials")
        .iter()
        .map(|credential| {
            (
                credential["kind"].as_str().expect("kind"),
                credential["label"].as_str().expect("label"),
                credential["account"].as_str().expect("account"),
            )
        })
        .collect();
    assert_eq!(
        labels,
        [("atproto", HANDLE, DID), ("passbook", HOLDER, HOLDER)],
        "each account by its label, its key beside it for the details"
    );

    // Opening a connection: ready, and the mind's tools follow.
    let front = install.connected(&id, &passbook, &world.board, "front");
    let lobby = install.json("/api/lobby");
    assert_eq!(lobby["connections"][0]["state"], "ready");
    let read = peer.ask(&json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "board_read", "arguments": {} }
    }));
    assert_eq!(read["result"]["isError"], false, "{read}");

    // Pulling the key: the connection is revoked, and says so.
    let revoked = install.operator(
        "POST",
        &format!("/api/connections/{front}/revoke"),
        &json!({}),
    );
    assert_eq!(revoked.status, 200, "{}", text(&revoked));
    assert_eq!(body(&revoked)["connection"]["state"], "revoked");
    let late = peer.ask(&json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": { "name": "board_read", "arguments": {} }
    }));
    assert!(
        late["error"].is_object() || late["result"]["isError"] == true,
        "{late}"
    );

    // The glance: the companion's journal reads as sentences, newest first,
    // with nothing pending, no id, no code and no secret.
    let entries = sentences(&install, &format!("companion={id}&finished=1"));
    let said: Vec<&str> = entries
        .iter()
        .map(|entry| entry["sentence"].as_str().expect("a sentence"))
        .collect();
    for expected in [
        "You created Ada Lovelace.",
        "You changed the portrait of Ada Lovelace.",
        "A mind attached as Ada Lovelace.",
        "You started signing Ada Lovelace in.",
        "You signed Ada Lovelace in.",
        "You opened a connection for Ada Lovelace to Board at front.",
        "A mind as Ada Lovelace called read on Board at front.",
        "You revoked the connection of Ada Lovelace to Board at front.",
    ] {
        assert!(said.contains(&expected), "{expected} in {said:#?}");
    }
    assert_eq!(
        said[0],
        "You revoked the connection of Ada Lovelace to Board at front."
    );
    assert!(
        entries
            .iter()
            .all(|entry| entry["outcome"] != "pending" && entry["companion"] == id.as_str())
    );
    let mut secrets: Vec<String> = world.passbook.keys();
    secrets.extend(world.atproto.issued());
    secrets.extend(world.board.badges());
    let secrets: Vec<&str> = secrets.iter().map(String::as_str).collect();
    for sentence in &said {
        assert!(!holds_an_id(sentence), "an id in {sentence}");
        assert!(
            !sentence.contains(DID) && !sentence.contains('_'),
            "{sentence}"
        );
        assert_eq!(
            secrets_in(sentence.as_bytes(), &secrets),
            Vec::<String>::new()
        );
    }
    let paged = sentences(&install, &format!("companion={id}&finished=1&limit=3"));
    assert_eq!(paged.len(), 3);
    assert_eq!(paged[0]["sentence"], said[0]);

    // The page itself carries no account key: it shows labels it reads.
    for asset in ["/", "/manager.js", "/manager.css", handoff.as_str()] {
        let served = get(&install, asset);
        assert_eq!(served.status, 200, "{asset}");
        assert!(
            !String::from_utf8_lossy(&served.body).contains(DID),
            "{asset}"
        );
    }

    peer.close_stdin();
    peer.wait_exit(Duration::from_secs(10));
    install.stop();
}
