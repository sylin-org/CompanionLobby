//! The Bluesky service adapter holds to the service contract, in process,
//! against the test kit's fake PDS, with every request proved by the atproto
//! credential adapter for an account that really signed in.

use std::sync::Mutex;

use lobby_conformance::credential::CountingSlot;
use lobby_conformance::service::{self, HeldIssued, ServiceFixture};
use lobby_contracts::{
    Address, AddressPolicy, Authority, Authorization, AuthorizationAsk, CredentialAdapter,
    Explained, Outcome, ProviderKind, SealedMaterial, Secret, ServiceAdapter, SignInRequest,
    SignInReturn, ToolArguments, ToolName, ToolResult,
};
use lobby_credential_atproto::{Atproto, World};
use lobby_service_bluesky::{self as bluesky, Bluesky};
use lobby_testkit::atproto::AtprotoWorld;
use lobby_testkit::{percent_decode, secrets_in};
use serde_json::{Value, json};

const ADA: &str = "did:plc:adaadaadaadaadaadaadaada";
const BEA: &str = "did:plc:beabeabeabeabeabeabeabea";
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0";

fn adapter() -> Bluesky {
    Bluesky::accepting(
        ProviderKind::new(lobby_credential_atproto::KIND).expect("a kind"),
        AddressPolicy::LoopbackAllowed,
    )
}

/// An account signed in at the fake world, minting through the atproto
/// adapter as the lobby's broker does, and keeping what it minted.
struct Minting {
    adapter: Atproto,
    material: Mutex<SealedMaterial>,
    permissions: Vec<String>,
    minted: Mutex<Vec<String>>,
}

impl Authority for Minting {
    fn authorize(&self, ask: &AuthorizationAsk) -> Result<Authorization, Explained> {
        let slot = CountingSlot::holding(
            Ok(self.material.lock().expect("material").clone()),
            self.permissions.clone(),
        );
        let minted = self.adapter.authorize(&slot, ask);
        if let Ok(material) = slot.material() {
            *self.material.lock().expect("material") = material;
        }
        if let Ok(authorization) = &minted {
            let mut kept = self.minted.lock().expect("minted");
            kept.push(authorization.token.expose().to_owned());
            if let Some(proof) = &authorization.proof {
                kept.push(proof.expose().to_owned());
            }
        }
        minted
    }
}

struct Fixture {
    world: AtprotoWorld,
    minting: Minting,
}

impl Fixture {
    fn start() -> Self {
        let words: Vec<String> = bluesky::PERMISSIONS
            .iter()
            .map(|word| (*word).to_owned())
            .collect();
        Self::signed_in(&words)
    }

    fn signed_in(permissions: &[String]) -> Self {
        let world = AtprotoWorld::start();
        world.add_account("ada.example.test", ADA);
        world.add_account("bea.example.test", BEA);
        world.display_name(BEA, "Bea");
        world.sign_in_as(ADA);
        let adapter = Atproto::new(World::loopback(&world.origin()).expect("a loopback world"));
        let request = SignInRequest {
            hint: String::new(),
            state: "bluesky-state".to_owned(),
            return_to: "http://127.0.0.1:5219/signin/atproto/return".to_owned(),
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
        Self {
            world,
            minting: Minting {
                adapter,
                material: Mutex::new(signed.material),
                permissions: signed.permissions,
                minted: Mutex::new(Vec::new()),
            },
        }
    }

    fn connected(&self) -> (Bluesky, HeldIssued<'_>) {
        let adapter = adapter();
        let issued = adapter
            .establish(&self.address(), &self.minting)
            .expect("established");
        (adapter, HeldIssued::new(issued, &self.minting))
    }

    fn call(
        &self,
        connected: &(Bluesky, HeldIssued<'_>),
        name: &str,
        value: Value,
    ) -> Result<ToolResult, Explained> {
        connected.0.call(
            &tool(name),
            &arguments(value),
            &self.address(),
            &connected.1,
        )
    }
}

impl ServiceFixture for Fixture {
    fn address(&self) -> Address {
        Address::parse(&self.world.pds_origin(), AddressPolicy::LoopbackAllowed).expect("address")
    }

    fn authority(&self) -> &dyn Authority {
        &self.minting
    }

    fn minted(&self) -> Vec<String> {
        self.minting.minted.lock().expect("minted").clone()
    }

    fn plant(&self, author: &str, text: &str) {
        self.world.display_name(BEA, author);
        self.world.add_post(BEA, text);
    }

    fn read(&self) -> (ToolName, ToolArguments) {
        (tool("timeline"), ToolArguments::default())
    }

    fn requests(&self) -> usize {
        self.world.pds().requests().len()
    }

    fn drop_issued(&self) {}

    fn grant_reading_only(&self) {}

    fn write(&self) -> (ToolName, ToolArguments) {
        (tool("post"), arguments(json!({ "text": "Hello." })))
    }

    fn issues_per_connection(&self) -> bool {
        false
    }
}

fn arguments(value: Value) -> ToolArguments {
    ToolArguments(value.as_object().cloned().unwrap_or_default())
}

fn tool(name: &str) -> ToolName {
    ToolName::new(name).expect("a tool name")
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let triple = chunk
            .iter()
            .enumerate()
            .fold(0u32, |triple, (index, byte)| {
                triple | (u32::from(*byte) << (16 - 8 * index))
            });
        for index in 0..4 {
            if index <= chunk.len() {
                out.push(char::from(
                    ALPHABET[(triple >> (18 - 6 * index)) as usize & 63],
                ));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[test]
fn the_bluesky_adapter_holds_to_the_service_contract() {
    service::run(&adapter(), &Fixture::start());
}

#[test]
fn the_atproto_adapter_answers_a_proof_of_one_request_in_the_credential_contract() {
    // The credential suite, with the request ask among those answered, runs
    // in atproto_credential.rs; here the answer's shape is pinned.
    let fixture = Fixture::start();
    let url = format!("{}{}", fixture.world.pds_origin(), bluesky::SESSION_PATH);
    let proved = fixture
        .minting
        .authorize(&AuthorizationAsk::ProvedRequest {
            method: "GET".to_owned(),
            url,
            nonce: None,
            refused_before: false,
        })
        .expect("a proof of the request");
    assert_eq!(
        proved.scheme, "DPoP",
        "a DPoP-bound token is never a bearer token"
    );
    assert!(proved.proof.is_some());
    let elsewhere = fixture
        .minting
        .authorize(&AuthorizationAsk::ProvedRequest {
            method: "GET".to_owned(),
            url: format!("{}{}", fixture.world.origin(), bluesky::SESSION_PATH),
            nonce: None,
            refused_before: false,
        })
        .expect_err("no proof for another origin");
    assert_eq!(elsewhere.outcome, Outcome::Refused);
    assert_eq!(
        elsewhere.sentence,
        lobby_credential_atproto::requests::ELSEWHERE
    );
}

#[test]
fn every_request_is_proved_on_its_own_and_its_token_never_goes_as_a_bearer_token() {
    let fixture = Fixture::start();
    // The PDS asks for its nonce in the header, as the reference PDS does.
    fixture.world.challenge_in_header(true);
    let connected = fixture.connected();
    let first = fixture.world.add_post(BEA, "Good morning.");
    fixture
        .call(&connected, "timeline", json!({}))
        .expect("the timeline");
    fixture
        .call(&connected, "like", json!({ "post": first }))
        .expect("liked");
    let requests = fixture.world.pds().requests();
    let resources: Vec<_> = requests
        .iter()
        .filter(|request| {
            request.route().starts_with("/xrpc/") && request.header("authorization").is_some()
        })
        .collect();
    assert!(resources.len() >= 4, "{}", resources.len());
    let mut proofs = std::collections::HashSet::new();
    for request in &resources {
        let authorization = request.header("authorization").unwrap_or_default();
        assert!(
            authorization.starts_with("DPoP "),
            "{} carried {authorization:.12}",
            request.route()
        );
        assert!(
            !authorization.starts_with("Bearer"),
            "a DPoP-bound token is never sent as a bearer token"
        );
        let proof = request.header("dpop").expect("a proof").to_owned();
        assert!(
            proofs.insert(proof),
            "each request carries a proof of its own"
        );
    }
    // The fake refuses a bound token under Bearer, so the scheme is checked
    // against a PDS that tells the difference.
    let access = fixture
        .world
        .issued()
        .into_iter()
        .find(|token| token.starts_with("eyJ"))
        .expect("an access token");
    let address = fixture.address();
    let answer = lobby_outbound::Outbound::new(AddressPolicy::LoopbackAllowed)
        .send(
            &address,
            lobby_outbound::Request::new("GET", bluesky::SESSION_PATH)
                .header("Authorization", format!("Bearer {access}")),
        )
        .expect("an answer");
    assert_eq!(answer.status, 400, "Malformed token");

    // An authority that answers with a bare token is never presented to
    // the PDS.
    struct Bare;
    impl Authority for Bare {
        fn authorize(&self, _ask: &AuthorizationAsk) -> Result<Authorization, Explained> {
            Ok(Authorization {
                scheme: "Bearer".to_owned(),
                token: Secret::new("bare-token"),
                proof: None,
            })
        }
    }
    let before = fixture.world.pds().requests().len();
    let refused = adapter()
        .establish(&address, &Bare)
        .expect_err("nothing is sent without a proof");
    assert_eq!(refused.outcome, Outcome::Unsupported);
    assert_eq!(fixture.world.pds().requests().len(), before);
}

#[test]
fn posts_replies_likes_and_reposts_are_records_in_the_companions_own_repository() {
    let fixture = Fixture::start();
    let connected = fixture.connected();
    let opening = fixture.world.add_post(BEA, "What are you reading?");

    let posted = fixture
        .call(
            &connected,
            "post",
            json!({ "text": "Reading about gardens: https://example.test/basil." }),
        )
        .expect("posted");
    let post = posted.structured.expect("structured")["post"]
        .as_str()
        .expect("a uri")
        .to_owned();
    assert!(
        post.starts_with(&format!("at://{ADA}/app.bsky.feed.post/")),
        "{post}"
    );
    let mine = fixture.world.records("app.bsky.feed.post");
    let kept = mine.iter().find(|record| record.uri == post).expect("kept");
    assert_eq!(kept.did, ADA, "in the companion's own repository");
    assert_eq!(
        kept.value["facets"][0]["features"][0]["uri"],
        "https://example.test/basil"
    );

    let replied = fixture
        .call(
            &connected,
            "reply",
            json!({ "post": opening, "text": "A book about basil." }),
        )
        .expect("replied");
    let reply = replied.structured.expect("structured")["post"]
        .as_str()
        .expect("a uri")
        .to_owned();
    let replying = fixture
        .world
        .records("app.bsky.feed.post")
        .into_iter()
        .find(|record| record.uri == reply)
        .expect("the reply");
    assert_eq!(replying.value["reply"]["parent"]["uri"], json!(opening));
    assert_eq!(replying.value["reply"]["root"]["uri"], json!(opening));

    // A reply to a reply keeps the thread's root.
    let deeper = fixture
        .call(
            &connected,
            "reply",
            json!({ "post": reply, "text": "And one about figs." }),
        )
        .expect("replied again");
    let deeper = deeper.structured.expect("structured")["post"]
        .as_str()
        .expect("a uri")
        .to_owned();
    let deepest = fixture
        .world
        .records("app.bsky.feed.post")
        .into_iter()
        .find(|record| record.uri == deeper)
        .expect("the deeper reply");
    assert_eq!(deepest.value["reply"]["root"]["uri"], json!(opening));
    assert_eq!(deepest.value["reply"]["parent"]["uri"], json!(reply));

    fixture
        .call(&connected, "like", json!({ "post": opening }))
        .expect("liked");
    fixture
        .call(&connected, "repost", json!({ "post": opening }))
        .expect("reposted");
    for collection in ["app.bsky.feed.like", "app.bsky.feed.repost"] {
        let marks = fixture.world.records(collection);
        assert_eq!(marks.len(), 1, "{collection}");
        assert_eq!(marks[0].did, ADA);
        assert_eq!(marks[0].value["subject"]["uri"], json!(opening));
    }

    // An image is uploaded as a blob first and embedded with its alt text.
    let pictured = fixture
        .call(
            &connected,
            "post",
            json!({ "text": "", "images": [{ "data": base64(PNG), "alt": "A basil plant." }] }),
        )
        .expect("posted with an image");
    let pictured = pictured.structured.expect("structured")["post"].clone();
    let blobs = fixture.world.blobs();
    assert_eq!(blobs.len(), 1);
    assert_eq!(blobs[0].mime, "image/png");
    let record = fixture
        .world
        .records("app.bsky.feed.post")
        .into_iter()
        .find(|record| json!(record.uri) == pictured)
        .expect("the post with an image");
    assert_eq!(record.value["embed"]["images"][0]["alt"], "A basil plant.");

    // The timeline shows other people's words quoted, with the uri to act on.
    let read = fixture
        .call(&connected, "timeline", json!({ "limit": 25 }))
        .expect("the timeline");
    let structured = read.structured.clone().expect("structured");
    let shown = structured["posts"]
        .as_array()
        .expect("posts")
        .iter()
        .find(|post| post["post"] == json!(opening))
        .expect("Bea's post")
        .clone();
    assert_eq!(shown["quoted"]["text"], "What are you reading?");
    assert_eq!(shown["quoted"]["author"], "Bea");
    assert_eq!(shown["likes"], 1);
    assert_eq!(shown["reposts"], 1);
    assert_eq!(shown["replies"], 1);
}

#[test]
fn a_page_of_the_timeline_says_how_to_read_on() {
    let fixture = Fixture::start();
    let connected = fixture.connected();
    for index in 0..3 {
        fixture.world.add_post(BEA, &format!("Note {index}."));
    }
    let first = fixture
        .call(&connected, "timeline", json!({ "limit": 2 }))
        .expect("a page");
    let more = first.structured.expect("structured")["more"].clone();
    assert_eq!(more["tool"], "bluesky_timeline");
    let rest = fixture
        .call(
            &connected,
            "timeline",
            json!({ "limit": 2, "cursor": more["cursor"] }),
        )
        .expect("the next page");
    let rest = rest.structured.expect("structured");
    assert_eq!(rest["posts"].as_array().map(Vec::len), Some(1));
    assert!(rest.get("more").is_none());
    for bad in [
        json!({ "limit": 0 }),
        json!({ "limit": 26 }),
        json!({ "cursor": "" }),
    ] {
        assert_eq!(
            fixture
                .call(&connected, "timeline", bad)
                .map(|_| ())
                .map_err(|explained| explained.outcome),
            Err(Outcome::InvalidInput)
        );
    }
}

#[test]
fn a_token_the_pds_stopped_taking_is_renewed_once_and_the_call_goes_on() {
    let fixture = Fixture::start();
    let connected = fixture.connected();
    fixture.world.end_access_tokens();
    let before = fixture.world.refresh_tokens_presented().len();
    fixture
        .call(&connected, "post", json!({ "text": "Still here." }))
        .expect("posted with a renewed token");
    assert_eq!(fixture.world.refresh_tokens_presented().len(), before + 1);
    assert_eq!(fixture.world.records("app.bsky.feed.post").len(), 1, "once");
}

#[test]
fn an_account_without_leave_to_post_is_told_who_grants_it() {
    let fixture = Fixture::signed_in(&[
        "rpc:app.bsky.feed.getTimeline?aud=*".to_owned(),
        "rpc:app.bsky.feed.getPosts?aud=*".to_owned(),
    ]);
    let connected = fixture.connected();
    let refused = fixture
        .call(&connected, "post", json!({ "text": "Hello." }))
        .expect_err("not granted");
    assert_eq!(refused.outcome, Outcome::NeedsPermission);
    assert_eq!(refused.sentence, bluesky::wire::NOT_GRANTED);
    assert!(fixture.world.records("app.bsky.feed.post").is_empty());
    let read = fixture
        .call(&connected, "timeline", json!({}))
        .expect("reading still works");
    assert!(!read.text.is_empty());
}

#[test]
fn nothing_a_mind_could_misuse_is_accepted_or_kept() {
    let fixture = Fixture::start();
    let connected = fixture.connected();
    for (name, value) in [
        (
            "like",
            json!({ "post": "https://bsky.app/profile/bea/post/1" }),
        ),
        (
            "reply",
            json!({ "post": "at://did:plc:x/app.bsky.feed.like/1", "text": "Hi" }),
        ),
        ("post", json!({ "text": "é".repeat(301) })),
        (
            "post",
            json!({ "text": "Hi", "images": [{ "data": base64(b"%PDF-1.7"), "alt": "" }] }),
        ),
        (
            "post",
            json!({ "text": "Hi", "images": [{ "data": base64(PNG), "alt": "", "x": 1 }] }),
        ),
    ] {
        let before = fixture.world.pds().requests().len();
        assert_eq!(
            fixture
                .call(&connected, name, value)
                .map(|_| ())
                .map_err(|explained| explained.outcome),
            Err(Outcome::InvalidInput),
            "{name}"
        );
        assert_eq!(
            fixture.world.pds().requests().len(),
            before,
            "{name} reached nothing"
        );
    }
    let unknown = format!("at://{BEA}/app.bsky.feed.post/3lzzzzzzzzzzz");
    assert_eq!(
        fixture
            .call(&connected, "like", json!({ "post": unknown }))
            .map(|_| ())
            .map_err(|explained| explained.outcome),
        Err(Outcome::NotFound)
    );
    let kept = String::from_utf8_lossy(connected.1.issued().as_bytes()).into_owned();
    let issued: Vec<String> = fixture.world.issued();
    let secrets: Vec<&str> = issued.iter().map(String::as_str).collect();
    assert!(secrets_in(kept.as_bytes(), &secrets).is_empty());
}
