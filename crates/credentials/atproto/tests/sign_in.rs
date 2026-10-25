//! The atproto credential adapter signs in against the test kit's fake
//! authorization server and PDS, in process, and refuses every return,
//! server or account it must not trust.

use std::time::Duration;

use lobby_contracts::{
    AuthorizationAsk, CredentialAdapter, MaterialSlot, Outcome, SealedMaterial, SignInRequest,
    SignInReturn,
};
use lobby_credential_atproto::{Atproto, World};
use lobby_testkit::atproto::{AtprotoWorld, Flaw};
use lobby_testkit::{percent_decode, secrets_in};
use serde_json::Value;

const ADA: &str = "did:plc:adaadaadaadaadaadaadaada";
const DECOY: &str = "did:plc:decoydecoydecoydecoydeco";
const RETURN_TO: &str = "http://127.0.0.1:5219/signin/atproto/return";

fn world() -> AtprotoWorld {
    let world = AtprotoWorld::start();
    world.add_account("decoy.example.test", DECOY);
    world.add_account("ada.example.test", ADA);
    world
}

fn adapter(world: &AtprotoWorld) -> Atproto {
    Atproto::new(World::loopback(&world.origin()).expect("a loopback world"))
}

fn request(hint: &str, state: &str) -> SignInRequest {
    SignInRequest {
        hint: hint.to_owned(),
        state: state.to_owned(),
        return_to: RETURN_TO.to_owned(),
    }
}

/// What the authorization server sent the browser back with.
fn returned(location: &str) -> SignInReturn {
    let (route, query) = location.split_once('?').expect("a query");
    assert_eq!(route, RETURN_TO, "back to the lobby's return route");
    SignInReturn {
        query: query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .map(|(name, value)| (percent_decode(name), percent_decode(value)))
            .collect(),
    }
}

fn without(returned: &SignInReturn, name: &str) -> SignInReturn {
    SignInReturn {
        query: returned
            .query
            .iter()
            .filter(|(key, _)| key != name)
            .cloned()
            .collect(),
    }
}

fn with(returned: &SignInReturn, name: &str, value: &str) -> SignInReturn {
    let mut query = without(returned, name).query;
    query.push((name.to_owned(), value.to_owned()));
    SignInReturn { query }
}

fn routes(world: &AtprotoWorld) -> Vec<String> {
    world
        .server()
        .requests()
        .iter()
        .map(|request| request.route().to_owned())
        .collect()
}

#[test]
fn the_account_the_provider_authenticated_is_signed_in_and_the_wire_keeps_the_profile() {
    let world = world();
    world.sign_in_as(ADA);
    let adapter = adapter(&world);
    let request = request("", "state-journey-1");
    let start = adapter.begin_sign_in(&request).expect("a sign-in starts");
    assert!(
        start
            .url
            .starts_with(&format!("{}/oauth/authorize?client_id=", world.origin())),
        "{}",
        start.url
    );
    let back = returned(&world.approve(&start.url));
    assert_eq!(back.get("state"), Some("state-journey-1"));
    let signed = adapter
        .complete_sign_in(&request, &back)
        .expect("signed in");
    assert_eq!(signed.account.as_str(), ADA);
    assert_eq!(signed.permissions, ["atproto"]);

    let material: Value = serde_json::from_slice(signed.material.as_bytes()).expect("JSON");
    assert_eq!(material["format"], "atproto-oauth-1");
    assert_eq!(material["subject"], ADA);
    assert_eq!(material["pds"], world.pds_origin());
    assert_eq!(material["authserver"], world.origin());
    assert!(
        secrets_in(signed.material.as_bytes(), &[DECOY]).is_empty(),
        "the decoy account appears nowhere"
    );

    let requests = world.server().requests();
    let pushes: Vec<_> = requests
        .iter()
        .filter(|request| request.route() == "/oauth/par")
        .collect();
    assert_eq!(pushes.len(), 2, "one nonce challenge, then the PAR");
    let par = pushes[1];
    let client_id = par.field("client_id").expect("client_id");
    assert_eq!(
        client_id,
        format!(
            "http://localhost?redirect_uri={}&scope=atproto",
            lobby_testkit::percent_encode(RETURN_TO)
        ),
        "the loopback client id declares the return route and the scope"
    );
    assert_eq!(par.field("redirect_uri").as_deref(), Some(RETURN_TO));
    assert_eq!(par.field("scope").as_deref(), Some("atproto"));
    assert_eq!(par.field("response_type").as_deref(), Some("code"));
    assert_eq!(par.field("code_challenge_method").as_deref(), Some("S256"));
    assert_eq!(par.field("state").as_deref(), Some("state-journey-1"));
    assert_eq!(par.field("nonce"), None, "no nonce parameter");
    assert_eq!(par.field("login_hint"), None, "no account named in advance");
    let proof_claims = |proof: &str| -> Value {
        let payload = proof.split('.').nth(1).expect("claims");
        serde_json::from_slice(
            &lobby_testkit::atproto::base64url_decode(payload).expect("base64url"),
        )
        .expect("JSON")
    };
    assert!(
        proof_claims(pushes[0].header("dpop").expect("a proof"))
            .get("nonce")
            .is_none(),
        "the first push knows no nonce yet"
    );
    assert!(
        proof_claims(par.header("dpop").expect("a proof"))["nonce"].is_string(),
        "the second carries the server's nonce"
    );

    let token = requests
        .iter()
        .rposition(|request| request.route() == "/oauth/token")
        .expect("the token request");
    let exchange = &requests[token];
    assert_eq!(
        exchange.field("grant_type").as_deref(),
        Some("authorization_code")
    );
    assert_eq!(
        exchange
            .field("code_verifier")
            .map(|verifier| verifier.len()),
        Some(43)
    );
    assert_eq!(exchange.field("client_id"), Some(client_id));
    assert_eq!(exchange.field("refresh_token"), None);
    let all = routes(&world);
    let first_push = all
        .iter()
        .position(|route| route == "/oauth/par")
        .expect("PAR");
    assert!(
        !all[..first_push]
            .iter()
            .any(|route| route.starts_with("/did:") || route.contains("resolveHandle")),
        "nothing is resolved before the PAR when no account is named: {all:?}"
    );
    let document = all
        .iter()
        .position(|route| *route == format!("/{ADA}"))
        .expect("the account's document");
    assert!(
        document > token,
        "the document is read after the exchange named the account"
    );
    assert_eq!(
        world.pds().count("/.well-known/oauth-protected-resource"),
        1
    );
}

#[test]
fn the_account_is_labelled_by_its_handle_and_by_its_did_when_no_handle_answers() {
    let world = world();
    world.sign_in_as(ADA);
    let adapter = adapter(&world);
    let first = request("", "state-label-1");
    let start = adapter.begin_sign_in(&first).expect("a sign-in starts");
    let signed = adapter
        .complete_sign_in(&first, &returned(&world.approve(&start.url)))
        .expect("signed in");
    assert_eq!(signed.label.as_str(), "ada.example.test");

    // The resolver is down after the code is spent: the sign-in still
    // completes, labelled by the account's DID.
    let second = request("", "state-label-2");
    let start = adapter.begin_sign_in(&second).expect("a sign-in starts");
    let back = returned(&world.approve(&start.url));
    world.resolver_unavailable();
    let signed = adapter
        .complete_sign_in(&second, &back)
        .expect("signed in all the same");
    assert_eq!(signed.account.as_str(), ADA);
    assert_eq!(signed.label.as_str(), ADA);
}

#[test]
fn a_named_account_is_resolved_before_the_push_and_must_be_the_one_signed_in() {
    let world = world();
    world.sign_in_as(ADA);
    let adapter = adapter(&world);
    let named = request("@Ada.Example.Test", "state-named-1");
    let start = adapter.begin_sign_in(&named).expect("a sign-in starts");
    let all = routes(&world);
    let first_push = all
        .iter()
        .position(|route| route == "/oauth/par")
        .expect("PAR");
    for step in [
        "/xrpc/com.atproto.identity.resolveHandle".to_owned(),
        format!("/{ADA}"),
        "/.well-known/oauth-authorization-server".to_owned(),
    ] {
        assert!(all[..first_push].contains(&step), "{step} before the PAR");
    }
    assert_eq!(
        world.pds().count("/.well-known/oauth-protected-resource"),
        1
    );
    let signed = adapter
        .complete_sign_in(&named, &returned(&world.approve(&start.url)))
        .expect("signed in");
    assert_eq!(signed.account.as_str(), ADA);

    // The sponsor named ada, and the provider signed the decoy in.
    world.sign_in_as(DECOY);
    let decoyed = request("ada.example.test", "state-named-2");
    let start = adapter.begin_sign_in(&decoyed).expect("a sign-in starts");
    assert_eq!(
        adapter
            .complete_sign_in(&decoyed, &returned(&world.approve(&start.url)))
            .map(|signed| signed.account),
        Err(Outcome::Refused),
        "a decoy account never becomes the credential"
    );

    // The token says another subject than the account the sponsor named.
    world.sign_in_as(ADA);
    world.token_names(DECOY);
    let swapped = request(ADA, "state-named-3");
    let start = adapter.begin_sign_in(&swapped).expect("a sign-in starts");
    assert_eq!(
        adapter
            .complete_sign_in(&swapped, &returned(&world.approve(&start.url)))
            .map(|signed| signed.account),
        Err(Outcome::Refused)
    );

    // A handle the document does not claim back names nobody.
    let unknown = request("nobody.example.test", "state-named-4");
    assert_eq!(
        adapter.begin_sign_in(&unknown).map(|start| start.url),
        Err(Outcome::NotFound)
    );
    for typed in ["ada", "127.0.0.1", "ada.example.test/x", "did:key:zabc"] {
        assert_eq!(
            adapter
                .begin_sign_in(&request(typed, "state-typed"))
                .map(|start| start.url),
            Err(Outcome::InvalidInput),
            "{typed}"
        );
    }
}

#[test]
fn a_did_web_account_is_resolved_from_its_document() {
    let world = world();
    let did = "did:web:web.example.test";
    world.add_account("web.example.test", did);
    world.sign_in_as(did);
    let adapter = adapter(&world);
    let request = request(did, "state-web-1");
    let start = adapter.begin_sign_in(&request).expect("a sign-in starts");
    let signed = adapter
        .complete_sign_in(&request, &returned(&world.approve(&start.url)))
        .expect("signed in");
    assert_eq!(signed.account.as_str(), did);
    assert!(world.server().count("/.well-known/did.json") >= 2);
}

#[test]
fn a_return_that_does_not_prove_the_sign_in_signs_nothing_in() {
    let world = world();
    world.sign_in_as(ADA);
    let adapter = adapter(&world);

    type Tamper = Box<dyn Fn(&SignInReturn) -> SignInReturn>;
    let cases: Vec<(&str, Tamper)> = vec![
        ("no issuer", Box::new(|back| without(back, "iss"))),
        (
            "another issuer",
            Box::new(|back| with(back, "iss", "http://127.0.0.1:1")),
        ),
        ("no code", Box::new(|back| without(back, "code"))),
        (
            "a declined sign-in",
            Box::new(|back| with(back, "error", "access_denied")),
        ),
    ];
    for (index, (case, tamper)) in cases.into_iter().enumerate() {
        let request = request("", &format!("state-tampered-{index}"));
        let start = adapter.begin_sign_in(&request).expect("a sign-in starts");
        let back = returned(&world.approve(&start.url));
        assert_eq!(
            adapter
                .complete_sign_in(&request, &tamper(&back))
                .map(|signed| signed.account),
            Err(Outcome::Refused),
            "{case}"
        );
        assert_eq!(
            adapter
                .complete_sign_in(&request, &back)
                .map(|signed| signed.account),
            Err(Outcome::Refused),
            "{case}: the flight was used once"
        );
    }

    let request = request("", "state-forged");
    let start = adapter.begin_sign_in(&request).expect("a sign-in starts");
    let back = returned(&world.approve(&start.url));
    assert_eq!(
        adapter
            .complete_sign_in(&request, &with(&back, "state", "forged"))
            .map(|signed| signed.account),
        Err(Outcome::Refused),
        "another state"
    );
    assert!(
        adapter.complete_sign_in(&request, &back).is_ok(),
        "a forged return does not use up the real flight"
    );

    assert_eq!(
        world.server().count("/oauth/token"),
        1,
        "only the real return reached the token endpoint"
    );
}

#[test]
fn a_declined_sign_in_comes_back_without_a_code_and_signs_nothing_in() {
    let world = world();
    let adapter = adapter(&world);
    let request = request("", "state-declined");
    let start = adapter.begin_sign_in(&request).expect("a sign-in starts");
    let back = returned(&world.approve(&start.url));
    assert_eq!(back.get("error"), Some("access_denied"));
    assert_eq!(
        adapter
            .complete_sign_in(&request, &back)
            .map(|signed| signed.account),
        Err(Outcome::Refused)
    );
    assert_eq!(world.server().count("/oauth/token"), 0);
}

#[test]
fn a_flight_past_its_time_signs_nothing_in() {
    let world = world();
    world.sign_in_as(ADA);
    let adapter = Atproto::with_time_limit(
        World::loopback(&world.origin()).expect("world"),
        Duration::from_millis(50),
    );
    let request = request("", "state-aged");
    let start = adapter.begin_sign_in(&request).expect("a sign-in starts");
    let back = returned(&world.approve(&start.url));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        adapter
            .complete_sign_in(&request, &back)
            .map(|signed| signed.account),
        Err(Outcome::Refused)
    );
    assert_eq!(
        world.server().count("/oauth/token"),
        0,
        "nothing was exchanged"
    );
}

#[test]
fn a_second_sign_in_under_a_waiting_state_is_refused_before_the_push() {
    let world = world();
    let adapter = adapter(&world);
    let request = request("", "state-twice");
    adapter.begin_sign_in(&request).expect("a sign-in starts");
    let pushed = world.server().count("/oauth/par");
    assert_eq!(
        adapter.begin_sign_in(&request).map(|start| start.url),
        Err(Outcome::Conflict)
    );
    assert_eq!(world.server().count("/oauth/par"), pushed);
}

#[test]
fn a_server_that_breaks_a_promise_is_refused_before_anything_is_pushed() {
    for flaw in [
        Flaw::NoPushedRequests,
        Flaw::NoS256,
        Flaw::NoDpop,
        Flaw::NoAtprotoScope,
        Flaw::AnotherIssuer,
    ] {
        let world = world();
        world.break_metadata(flaw);
        let adapter = adapter(&world);
        assert_eq!(
            adapter
                .begin_sign_in(&request("", "state-flawed"))
                .map(|start| start.url),
            Err(Outcome::Unsupported),
            "{flaw:?}"
        );
        assert_eq!(world.server().count("/oauth/par"), 0, "{flaw:?}");
    }
    let world = world();
    world.break_metadata(Flaw::Redirects);
    let adapter = adapter(&world);
    assert_eq!(
        adapter
            .begin_sign_in(&request("", "state-redirected"))
            .map(|start| start.url),
        Err(Outcome::Refused),
        "a redirect is never followed"
    );
    assert_eq!(
        world.pds().count("/.well-known/oauth-authorization-server"),
        0,
        "nothing reached where the redirect pointed"
    );
}

#[test]
fn an_account_whose_pds_names_another_server_signs_nothing_in() {
    let world = world();
    world.sign_in_as(ADA);
    world.pds_names("http://127.0.0.1:1");
    let adapter = adapter(&world);
    let request = request("", "state-routed");
    let start = adapter.begin_sign_in(&request).expect("a sign-in starts");
    assert_eq!(
        adapter
            .complete_sign_in(&request, &returned(&world.approve(&start.url)))
            .map(|signed| signed.account),
        Err(Outcome::Refused)
    );
}

#[test]
fn an_opaque_access_token_signs_in_all_the_same() {
    let world = world();
    world.sign_in_as(ADA);
    world.opaque_tokens();
    let adapter = adapter(&world);
    let request = request("", "state-opaque");
    let start = adapter.begin_sign_in(&request).expect("a sign-in starts");
    let signed = adapter
        .complete_sign_in(&request, &returned(&world.approve(&start.url)))
        .expect("signed in");
    assert_eq!(signed.account.as_str(), ADA);
}

struct Untouched;

impl MaterialSlot for Untouched {
    fn read(&self) -> Result<SealedMaterial, Outcome> {
        panic!("the material is never read for an ask the adapter does not answer")
    }
}

#[test]
fn every_ask_is_refused_as_unsupported_before_the_material_is_read() {
    let world = world();
    let adapter = adapter(&world);
    for ask in [
        AuthorizationAsk::Bearer,
        AuthorizationAsk::ServiceProof {
            audience: "did:web:service.example.test".to_owned(),
            method: "com.example.method".to_owned(),
            lifetime_secs: 60,
        },
    ] {
        assert_eq!(
            adapter.authorize(&Untouched, &ask).map(|_| ()),
            Err(Outcome::Unsupported)
        );
    }
    assert!(world.server().requests().is_empty());
}

#[test]
fn the_public_world_refuses_loopback_and_private_targets_before_anything_is_sent() {
    let adapter = Atproto::new(World::public(None).expect("the public world"));
    for typed in ["127.0.0.1", "10.0.0.1", "localhost"] {
        assert_eq!(
            adapter
                .begin_sign_in(&request(typed, "state-public"))
                .map(|start| start.url),
            Err(Outcome::InvalidInput),
            "{typed}"
        );
    }
    assert!(World::public(Some("http://127.0.0.1:5300")).is_err());
}
