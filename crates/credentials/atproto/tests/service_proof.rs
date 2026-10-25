//! The atproto credential adapter mints a service's proof at the account's
//! PDS, in process, against the test kit's fake authorization server and
//! PDS, and refuses every ask it must not answer before anything is read
//! or sent.

use std::sync::Mutex;

use lobby_contracts::{
    AuthorizationAsk, CredentialAdapter, CredentialId, MaterialSlot, MaterialUpdate, Outcome,
    SealedMaterial, SignInRequest, SignInReturn,
};
use lobby_credential_atproto::proofs::{NOT_GRANTED, SERVICE_AUTH_PATH};
use lobby_credential_atproto::{Atproto, World};
use lobby_testkit::atproto::{AtprotoWorld, base64url_decode};
use lobby_testkit::percent_decode;
use serde_json::Value;

const ADA: &str = "did:plc:adaadaadaadaadaadaadaada";
const RETURN_TO: &str = "http://127.0.0.1:5219/signin/atproto/return";
const METHOD: &str = "org.example.service.exchangeIdentity";
const AUDIENCE: &str = "did:web:localhost%3A5300#svc";

fn granted() -> String {
    format!("rpc:{METHOD}?aud=*")
}

fn ask() -> AuthorizationAsk {
    AuthorizationAsk::ServiceProof {
        audience: AUDIENCE.to_owned(),
        method: METHOD.to_owned(),
        lifetime_secs: 60,
    }
}

/// A slot holding one credential's material in memory, as the daemon's does.
struct Held {
    material: Mutex<SealedMaterial>,
    reads: Mutex<usize>,
}

impl Held {
    fn new(material: SealedMaterial) -> Self {
        Self {
            material: Mutex::new(material),
            reads: Mutex::new(0),
        }
    }

    fn reads(&self) -> usize {
        *self.reads.lock().expect("reads")
    }
}

impl MaterialSlot for Held {
    fn credential(&self) -> CredentialId {
        CredentialId::from_u128(7)
    }

    fn read(&self) -> Result<SealedMaterial, Outcome> {
        *self.reads.lock().expect("reads") += 1;
        Ok(self.material.lock().expect("material").clone())
    }

    fn mark_rotating(&self, _read: &SealedMaterial) -> Result<(), Outcome> {
        Ok(())
    }

    fn unmark_rotating(&self) {}

    fn commit(&self, update: MaterialUpdate) -> Result<(), Outcome> {
        let mut material = self.material.lock().expect("material");
        if *material != *update.expected() {
            return Err(Outcome::Conflict);
        }
        *material = update.replacement().clone();
        Ok(())
    }
}

/// Signs ADA in through `adapter`, asking for `permissions`, and answers
/// the material and what was granted.
fn signed_in(
    world: &AtprotoWorld,
    adapter: &Atproto,
    permissions: &[String],
) -> (SealedMaterial, Vec<String>) {
    world.add_account("ada.example.test", ADA);
    world.sign_in_as(ADA);
    let request = SignInRequest {
        hint: String::new(),
        state: "proof-state".to_owned(),
        return_to: RETURN_TO.to_owned(),
        again: None,
        permissions: permissions.to_vec(),
    };
    let start = adapter.begin_sign_in(&request).expect("a sign-in starts");
    let back = world.approve(&start.url);
    let (_, query) = back.split_once('?').expect("a query");
    let returned = SignInReturn {
        query: query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .map(|(name, value)| (percent_decode(name), percent_decode(value)))
            .collect(),
    };
    let signed = adapter
        .complete_sign_in(&request, &returned)
        .expect("signed in");
    (signed.material, signed.permissions)
}

fn adapter(world: &AtprotoWorld) -> Atproto {
    Atproto::new(World::loopback(&world.origin()).expect("a loopback world"))
}

fn claims(proof: &str) -> Value {
    let payload = proof.split('.').nth(1).expect("claims");
    serde_json::from_slice(&base64url_decode(payload).expect("base64url")).expect("JSON")
}

fn mints(world: &AtprotoWorld) -> Vec<lobby_testkit::listener::Received> {
    world
        .pds()
        .requests()
        .into_iter()
        .filter(|request| request.route() == SERVICE_AUTH_PATH)
        .collect()
}

#[test]
fn a_sign_in_asks_for_what_the_services_need_and_a_proof_is_minted_with_it() {
    let world = AtprotoWorld::start();
    let adapter = adapter(&world);
    let (material, permissions) = signed_in(&world, &adapter, &[granted()]);
    assert_eq!(permissions, ["atproto".to_owned(), granted()]);
    let pushed = world
        .server()
        .requests()
        .into_iter()
        .find(|request| request.route() == "/oauth/par")
        .expect("the push");
    assert_eq!(
        pushed.field("scope").as_deref(),
        Some(format!("atproto {}", granted()).as_str())
    );

    let slot = Held::new(material);
    let proof = adapter.authorize(&slot, &ask()).expect("a proof");
    assert_eq!(proof.scheme, "Bearer");
    let minted = world
        .proofs()
        .verify(proof.token.expose())
        .expect("minted by the PDS");
    assert_eq!(
        (
            minted.issuer.as_str(),
            minted.audience.as_str(),
            minted.method.as_str()
        ),
        (ADA, AUDIENCE, METHOD)
    );

    // The first ask carried no nonce and was challenged in the body; the
    // second carried the PDS's own nonce, the access token under DPoP, and
    // a proof bound to the token, for the URL without its query.
    let asked = mints(&world);
    assert_eq!(asked.len(), 2, "one challenge, then the proof");
    for request in &asked {
        let authorization = request.header("authorization").expect("authorization");
        assert!(authorization.starts_with("DPoP "), "{authorization}");
        let dpop = claims(request.header("dpop").expect("a proof"));
        assert_eq!(dpop["htm"], "GET");
        assert_eq!(
            dpop["htu"],
            format!("{}{SERVICE_AUTH_PATH}", world.pds_origin())
        );
        assert!(dpop["ath"].is_string());
        assert_eq!(request.query("aud").as_deref(), Some(AUDIENCE));
        assert_eq!(request.query("lxm").as_deref(), Some(METHOD));
    }
    assert!(
        claims(asked[0].header("dpop").expect("proof"))
            .get("nonce")
            .is_none()
    );
    assert!(claims(asked[1].header("dpop").expect("proof"))["nonce"].is_string());
    let exp: i64 = asked[1]
        .query("exp")
        .and_then(|exp| exp.parse().ok())
        .expect("exp");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .expect("now");
    assert!(
        (now..=now + 61).contains(&exp),
        "the life the service asked for"
    );

    // The PDS's nonce is kept for the key: the next proof needs one request,
    // and carries a fresh jti.
    let again = adapter.authorize(&slot, &ask()).expect("another proof");
    assert_eq!(mints(&world).len(), 3, "the kept nonce saves the challenge");
    assert_ne!(again.token, proof.token);
    assert_ne!(
        claims(again.token.expose())["jti"],
        claims(proof.token.expose())["jti"]
    );
}

#[test]
fn a_nonce_challenge_in_the_header_is_answered_the_same() {
    let world = AtprotoWorld::start();
    world.challenge_in_header(true);
    let adapter = adapter(&world);
    let (material, _) = signed_in(&world, &adapter, &[granted()]);
    let proof = adapter
        .authorize(&Held::new(material), &ask())
        .expect("a proof");
    assert!(world.proofs().verify(proof.token.expose()).is_some());
    let asked = mints(&world);
    assert_eq!(asked.len(), 2);
    assert!(claims(asked[1].header("dpop").expect("proof"))["nonce"].is_string());
}

#[test]
fn an_account_signed_in_without_the_method_is_told_to_sign_in_again_before_the_pds_hears() {
    let world = AtprotoWorld::start();
    let adapter = adapter(&world);
    let (material, permissions) = signed_in(&world, &adapter, &[]);
    assert_eq!(permissions, ["atproto"]);
    let refused = adapter
        .authorize(&Held::new(material), &ask())
        .expect_err("not granted");
    assert_eq!(refused.outcome, Outcome::NeedsPermission);
    assert_eq!(refused.sentence, NOT_GRANTED);
    assert!(mints(&world).is_empty(), "the PDS was not asked");
}

#[test]
fn a_bare_did_or_a_method_that_is_no_nsid_is_refused_before_the_material_is_read() {
    let world = AtprotoWorld::start();
    let adapter = adapter(&world);
    let (material, _) = signed_in(&world, &adapter, &[granted()]);
    let slot = Held::new(material);
    for ask in [
        AuthorizationAsk::ServiceProof {
            audience: "did:web:localhost%3A5300".to_owned(),
            method: METHOD.to_owned(),
            lifetime_secs: 60,
        },
        AuthorizationAsk::ServiceProof {
            audience: "did:web:localhost%3A5300#svc&lxm=evil".to_owned(),
            method: METHOD.to_owned(),
            lifetime_secs: 60,
        },
        AuthorizationAsk::ServiceProof {
            audience: AUDIENCE.to_owned(),
            method: "exchange".to_owned(),
            lifetime_secs: 60,
        },
    ] {
        let refused = adapter.authorize(&slot, &ask).expect_err("refused");
        assert_eq!(refused.outcome, Outcome::Unsupported, "{ask:?}");
    }
    assert_eq!(slot.reads(), 0, "the material was not read");
    assert!(mints(&world).is_empty());
}

#[test]
fn a_due_session_is_renewed_before_it_proves_anything() {
    let world = AtprotoWorld::start();
    world.access_lifetime(1);
    let adapter = adapter(&world);
    let (material, _) = signed_in(&world, &adapter, &[granted()]);
    world.access_lifetime(3600);
    let slot = Held::new(material.clone());
    let proof = adapter.authorize(&slot, &ask()).expect("a proof");
    assert_eq!(world.refresh_tokens_presented().len(), 1, "renewed first");
    assert_ne!(
        *slot.material.lock().expect("material"),
        material,
        "the renewed material was kept"
    );
    assert!(world.proofs().verify(proof.token.expose()).is_some());
}
