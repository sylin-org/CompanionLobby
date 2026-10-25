//! Signing a companion in with atproto, through the real daemon and the real
//! command, against the test kit's fake authorization server and its fake
//! account host.
//!
//! The journeys run `companion-lobby-synthetic` with `LOBBY_AUTHSERVER`
//! naming the fake world, which is how that command learns to point the
//! atproto adapter at it; `companion-lobby` itself refuses such an origin
//! before it reaches anything. The sponsor's browser is played by the test:
//! it reads where the operator API sends it, lets the fake authorization
//! server approve, and follows it back to the lobby's return page. Every
//! daemon runs with proxy variables pointing at a listener that must hear
//! nothing. No credential is seeded: each exists because a sign-in
//! completed.

use std::path::Path;

use lobby_conformance::host::{Install, body, split_url, text};
use lobby_daemon::loopback::Answer;
use lobby_testkit::atproto::{AtprotoWorld, base64url_decode};
use lobby_testkit::listener::{Listener, Reply};
use lobby_testkit::{percent_decode, percent_encode, secrets_in};
use serde_json::{Value, json};

const SYNTHETIC: &str = env!("CARGO_BIN_EXE_companion-lobby-synthetic");
const PRODUCT: &str = env!("CARGO_BIN_EXE_companion-lobby");

const ADA: &str = "did:plc:adaadaadaadaadaadaadaada";
const DECOY: &str = "did:plc:decoydecoydecoydecoydeco";

/// A fake world with the decoy registered first, and a listener standing in
/// for a proxy.
fn world() -> (AtprotoWorld, Listener) {
    let world = AtprotoWorld::start();
    world.add_account("decoy.example.test", DECOY);
    world.add_account("ada.example.test", ADA);
    let proxy = Listener::start(|_| Reply::json(502, &json!({})));
    (world, proxy)
}

fn install(world: &AtprotoWorld, proxy: &Listener, label: &str) -> Install {
    let proxy = proxy.origin();
    Install::new(Path::new(SYNTHETIC), label)
        .with_env("LOBBY_AUTHSERVER", &world.origin())
        .with_env("HTTP_PROXY", &proxy)
        .with_env("HTTPS_PROXY", &proxy)
        .with_env("ALL_PROXY", &proxy)
        .with_env("http_proxy", &proxy)
        .with_env("https_proxy", &proxy)
}

/// Starts a sign-in of `companion` from the operator API.
fn begin(install: &Install, companion: &str, hint: &str) -> Answer {
    install.operator(
        "POST",
        "/api/sign-ins",
        &json!({ "companion": companion, "kind": "atproto", "hint": hint }),
    )
}

/// The sponsor's browser at the lobby's return route, with `location`'s
/// path and query.
fn return_page(install: &Install, location: &str) -> Answer {
    let (lobby, target) = split_url(location);
    assert_eq!(lobby.port(), install.port(), "back to this lobby");
    assert!(target.starts_with("/signin/atproto/return?"), "{target}");
    install.arrive(location)
}

/// Signs `companion` in all the way through and returns the return page.
fn sign_in(install: &Install, world: &AtprotoWorld, companion: &str, hint: &str) -> Answer {
    let started = begin(install, companion, hint);
    assert_eq!(started.status, 201, "{}", text(&started));
    let url = body(&started)["url"].as_str().expect("url").to_owned();
    return_page(install, &world.approve(&url))
}

fn credentials(install: &Install) -> Vec<Value> {
    install.json("/api/lobby")["credentials"]
        .as_array()
        .expect("credentials")
        .clone()
}

fn query_value(location: &str, name: &str) -> String {
    let (_, query) = location.split_once('?').expect("a query");
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| percent_decode(value))
        .expect("the parameter")
}

fn without(location: &str, name: &str) -> String {
    let (route, query) = location.split_once('?').expect("a query");
    let kept: Vec<&str> = query
        .split('&')
        .filter(|pair| !pair.starts_with(&format!("{name}=")))
        .collect();
    format!("{route}?{}", kept.join("&"))
}

fn proof_claims(proof: &str) -> Value {
    let payload = proof.split('.').nth(1).expect("claims");
    serde_json::from_slice(&base64url_decode(payload).expect("base64url")).expect("JSON")
}

#[test]
fn a_sponsor_signs_a_companion_in_and_the_wire_keeps_the_profile() {
    let (world, proxy) = world();
    world.sign_in_as(ADA);
    let install = install(&world, &proxy, "sign-in");
    install.open();
    let ada = install.create("ada");
    assert_eq!(
        install.json("/api/lobby")["kinds"],
        json!([
            { "kind": "passbook", "title": "Passbook", "hint": lobby_testkit::passbook::HINT },
            { "kind": "atproto", "title": "Atmosphere", "hint": lobby_credential_atproto::HINT },
        ]),
        "each kind with the title and the hint the sponsor reads"
    );

    let started = begin(&install, &ada, "");
    assert_eq!(started.status, 201, "{}", text(&started));
    let waiting = body(&started)["signIn"].clone();
    assert_eq!(waiting["status"], "pending");
    assert_eq!(waiting["companion"], ada.as_str());
    let url = body(&started)["url"].as_str().expect("url").to_owned();
    assert!(
        url.starts_with(&format!("{}/oauth/authorize?client_id=", world.origin())),
        "{url}"
    );
    assert_eq!(install.json("/api/lobby")["signIns"], json!([waiting]));

    // The companion already waits on a sign-in: a second is refused before
    // anything is pushed.
    let pushed = world.server().count("/oauth/par");
    let again = begin(&install, &ada, "");
    assert_eq!(again.status, 409, "{}", text(&again));
    assert_eq!(world.server().count("/oauth/par"), pushed);

    let location = world.approve(&url);
    let page = return_page(&install, &location);
    assert_eq!(page.status, 200, "{}", text(&page));
    let said = text(&page);
    assert!(said.contains("The account is signed in"), "{said}");
    for echoed in [
        query_value(&location, "code"),
        query_value(&location, "state"),
        world.origin(),
        ADA.to_owned(),
    ] {
        assert!(!said.contains(&echoed), "the return page echoes {echoed}");
    }

    let held = credentials(&install);
    assert_eq!(held.len(), 1);
    assert_eq!(held[0]["companion"], ada.as_str());
    assert_eq!(held[0]["kind"], "atproto");
    assert_eq!(
        held[0]["account"], ADA,
        "the account the provider authenticated"
    );
    assert_eq!(
        held[0]["label"], "ada.example.test",
        "the handle that resolves back to it, as the sponsor knows it"
    );
    assert_eq!(held[0]["permissions"], json!(["atproto"]));
    assert_eq!(held[0]["state"], "ready");
    let ended = install.json("/api/lobby")["signIns"].clone();
    assert_eq!(ended.as_array().map(Vec::len), Some(1), "{ended}");
    assert_eq!(ended[0]["status"], "signed_in");
    assert_eq!(ended[0]["id"], waiting["id"]);

    // The wire, as the fake authorization server saw it.
    let requests = world.server().requests();
    let pushes: Vec<_> = requests
        .iter()
        .filter(|request| request.route() == "/oauth/par")
        .collect();
    let par = pushes.last().expect("the PAR");
    let return_to = format!("http://127.0.0.1:{}/signin/atproto/return", install.port());
    assert_eq!(
        par.field("client_id"),
        Some(format!(
            "http://localhost?redirect_uri={}&scope=atproto",
            percent_encode(&return_to)
        )),
        "the loopback client id declares the return route and the identity scope"
    );
    assert_eq!(par.field("redirect_uri"), Some(return_to));
    assert_eq!(par.field("scope").as_deref(), Some("atproto"));
    assert_eq!(par.field("code_challenge_method").as_deref(), Some("S256"));
    assert_eq!(par.field("nonce"), None);
    assert_eq!(par.field("login_hint"), None);
    assert!(proof_claims(par.header("dpop").expect("a proof"))["nonce"].is_string());
    let routes: Vec<String> = requests
        .iter()
        .map(|request| request.route().to_owned())
        .collect();
    let first_push = routes
        .iter()
        .position(|route| route == "/oauth/par")
        .expect("PAR");
    assert!(
        !routes[..first_push]
            .iter()
            .any(|route| route.starts_with("/did:") || route.contains("resolveHandle")),
        "no account is resolved before the PAR: {routes:?}"
    );
    let token = routes
        .iter()
        .position(|route| route == "/oauth/token")
        .expect("token");
    let document = routes
        .iter()
        .position(|route| *route == format!("/{ADA}"))
        .expect("the account's document");
    assert!(
        document > token,
        "the document is read after the exchange: {routes:?}"
    );
    assert_eq!(
        requests[token]
            .field("code_verifier")
            .map(|verifier| verifier.len()),
        Some(43)
    );

    // Nothing secret, and not the decoy, reaches what the lobby shows or
    // writes in the clear; the material is a sealed file of its own.
    let mut secrets: Vec<String> = world.issued();
    secrets.push(DECOY.to_owned());
    secrets.push(query_value(&location, "code"));
    let secrets: Vec<&str> = secrets.iter().map(String::as_str).collect();
    let mut shown = install.json("/api/lobby").to_string().into_bytes();
    shown.extend(Value::Array(install.journal()).to_string().into_bytes());
    shown.extend(install.plain_files());
    shown.extend(page.body.clone());
    assert_eq!(secrets_in(&shown, &secrets), Vec::<String>::new());
    let id = held[0]["id"].as_str().expect("id");
    let material = std::fs::read(install.dir().join(format!("{id}.material"))).expect("material");
    assert!(!material.is_empty());
    #[cfg(windows)]
    assert_eq!(
        secrets_in(&material, &secrets),
        Vec::<String>::new(),
        "the material is sealed with DPAPI"
    );
    assert!(proxy.requests().is_empty(), "no proxy from the environment");

    let journal = install.journal();
    let commands: Vec<(String, String)> = journal
        .iter()
        .filter(|entry| {
            entry["command"]
                .as_str()
                .is_some_and(|name| name.contains("sign_in"))
        })
        .map(|entry| {
            (
                entry["command"].as_str().expect("command").to_owned(),
                entry["outcome"].as_str().expect("outcome").to_owned(),
            )
        })
        .collect();
    let pair = |name: &str, outcome: &str| (name.to_owned(), outcome.to_owned());
    assert_eq!(
        commands,
        [
            pair("begin_sign_in", "pending"),
            pair("begin_sign_in", "done"),
            pair("begin_sign_in", "conflict"),
            pair("complete_sign_in", "pending"),
            pair("complete_sign_in", "done"),
        ]
    );

    // A second companion cannot claim the account.
    let bea = install.create("bea");
    let taken = sign_in(&install, &world, &bea, "");
    assert_eq!(taken.status, 409, "{}", text(&taken));
    assert!(!text(&taken).contains(ADA));
    assert_eq!(credentials(&install).len(), 1);

    // Revoked, the credential stays as history and the account stays
    // reserved; signing ada in again makes a new credential.
    let revoked = install.operator("POST", &format!("/api/credentials/{id}/revoke"), &json!({}));
    assert_eq!(revoked.status, 200, "{}", text(&revoked));
    assert!(!install.files().contains(&format!("{id}.material")));
    assert_eq!(sign_in(&install, &world, &bea, "").status, 409);
    let again = sign_in(&install, &world, &ada, "");
    assert_eq!(again.status, 200, "{}", text(&again));
    let held = credentials(&install);
    assert_eq!(held.len(), 2);
    assert_eq!(
        (held[0]["id"].as_str(), held[0]["state"].as_str()),
        (Some(id), Some("revoked"))
    );
    assert_eq!(held[1]["state"], "ready");
    assert_ne!(held[1]["id"], held[0]["id"]);
    install.stop();
}

#[test]
fn a_named_account_must_be_the_one_the_provider_signs_in() {
    let (world, proxy) = world();
    let install = install(&world, &proxy, "named");
    install.open();
    let ada = install.create("ada");
    world.sign_in_as(DECOY);
    let decoyed = sign_in(&install, &world, &ada, "ada.example.test");
    assert_eq!(decoyed.status, 403, "{}", text(&decoyed));
    assert!(!text(&decoyed).contains(DECOY) && !text(&decoyed).contains(ADA));
    assert!(
        credentials(&install).is_empty(),
        "the decoy never becomes the credential"
    );

    world.sign_in_as(ADA);
    let named = sign_in(&install, &world, &ada, "@Ada.Example.Test");
    assert_eq!(named.status, 200, "{}", text(&named));
    assert_eq!(credentials(&install)[0]["account"], ADA);
    assert!(proxy.requests().is_empty());
    install.stop();
}

#[test]
fn a_return_that_does_not_prove_the_sign_in_signs_nothing_in() {
    let (world, proxy) = world();
    world.sign_in_as(ADA);
    let install = install(&world, &proxy, "unproven");
    install.open();
    let ada = install.create("ada");

    // A state the lobby never minted.
    let forged = return_page(
        &install,
        &format!(
            "http://127.0.0.1:{}/signin/atproto/return?code=forged-code&state=forged-state&iss=http%3A%2F%2F127.0.0.1%3A1",
            install.port()
        ),
    );
    assert_eq!(forged.status, 404, "{}", text(&forged));
    assert!(!text(&forged).contains("forged"));

    // The real return, without the issuer.
    let started = begin(&install, &ada, "");
    let url = body(&started)["url"].as_str().expect("url").to_owned();
    let location = world.approve(&url);
    let stripped = return_page(&install, &without(&location, "iss"));
    assert_eq!(stripped.status, 403, "{}", text(&stripped));
    assert!(!text(&stripped).contains(&query_value(&location, "code")));
    // The sign-in was used once; the untampered return now finds nothing.
    assert_eq!(return_page(&install, &location).status, 404);
    assert_eq!(
        world.server().count("/oauth/token"),
        0,
        "nothing was exchanged"
    );

    // A cancelled sign-in signs nothing in when the browser comes back.
    let started = begin(&install, &ada, "");
    assert_eq!(
        started.status, 201,
        "a refused sign-in has ended, so another may start"
    );
    let id = body(&started)["signIn"]["id"]
        .as_str()
        .expect("id")
        .to_owned();
    let url = body(&started)["url"].as_str().expect("url").to_owned();
    let cancelled = install.operator("POST", &format!("/api/sign-ins/{id}/cancel"), &json!({}));
    assert_eq!(cancelled.status, 200, "{}", text(&cancelled));
    assert_eq!(body(&cancelled)["signIn"]["status"], "cancelled");
    let late = return_page(&install, &world.approve(&url));
    assert_eq!(late.status, 404, "{}", text(&late));
    assert_eq!(world.server().count("/oauth/token"), 0);
    assert!(credentials(&install).is_empty());
    let again = install.operator("POST", &format!("/api/sign-ins/{id}/cancel"), &json!({}));
    assert_eq!(again.status, 404);
    install.stop();
}

#[test]
fn companion_lobby_refuses_a_loopback_authorization_server_and_reaches_nothing() {
    let (world, proxy) = world();
    let refused = Install::new(Path::new(PRODUCT), "public-only")
        .with_env("LOBBY_AUTHSERVER", &world.origin());
    let output = refused.command(&["run"]);
    assert!(!output.status.success());
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(said.contains("LOBBY_AUTHSERVER"), "{said}");
    assert!(!refused.running());
    assert!(world.server().requests().is_empty());
    assert!(proxy.requests().is_empty());

    // Without it, the product signs companions in with atproto, on the
    // public network only.
    let product = Install::new(Path::new(PRODUCT), "product-kinds");
    product.open();
    assert_eq!(
        product.json("/api/lobby")["kinds"],
        json!([{ "kind": "atproto", "title": "Atmosphere", "hint": lobby_credential_atproto::HINT }])
    );
    assert_eq!(
        product.json("/api/lobby")["services"],
        json!([]),
        "the product has no service installed yet"
    );
    product.stop();
}
