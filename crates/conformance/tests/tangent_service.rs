//! The Tangent service adapter holds to the service contract, in process,
//! against the test kit's fake Space, with proofs the atproto credential
//! adapter mints at the fake PDS for an account that really signed in.

use std::sync::Mutex;

use lobby_conformance::credential::CountingSlot;
use lobby_conformance::service::{self, HeldIssued, ServiceFixture};
use lobby_contracts::{
    Address, AddressPolicy, Authority, Authorization, AuthorizationAsk, CredentialAdapter,
    Explained, Outcome, Passage, SealedMaterial, ServiceAdapter, SignInRequest, SignInReturn,
    ToolArguments, ToolName,
};
use lobby_credential_atproto::{Atproto, World};
use lobby_service_tangent::{self as tangent, Tangent};
use lobby_testkit::atproto::AtprotoWorld;
use lobby_testkit::space::{EXCHANGE_PATH, Profile, SpaceServer};
use lobby_testkit::{percent_decode, secrets_in};
use serde_json::{Map, Value, json};

const ADA: &str = "did:plc:adaadaadaadaadaadaadaada";

fn tangent_adapter() -> Tangent {
    Tangent::accepting(
        lobby_contracts::ProviderKind::new(lobby_credential_atproto::KIND).expect("a kind"),
    )
}

/// An account signed in at the fake world, minting through the atproto
/// adapter as the lobby's broker does, and counting what it was asked.
struct Minting {
    adapter: Atproto,
    material: Mutex<SealedMaterial>,
    permissions: Vec<String>,
    asked: Mutex<Vec<AuthorizationAsk>>,
}

impl Authority for Minting {
    fn authorize(&self, ask: &AuthorizationAsk) -> Result<Authorization, Explained> {
        self.asked.lock().expect("asked").push(ask.clone());
        let slot = CountingSlot::holding(
            Ok(self.material.lock().expect("material").clone()),
            self.permissions.clone(),
        );
        let minted = self.adapter.authorize(&slot, ask);
        if let Ok(material) = slot.material() {
            *self.material.lock().expect("material") = material;
        }
        minted
    }
}

struct Fixture {
    world: AtprotoWorld,
    space: SpaceServer,
    minting: Minting,
}

impl Fixture {
    fn start() -> Self {
        Self::signed_in(&[tangent::PERMISSION.to_owned()])
    }

    fn signed_in(permissions: &[String]) -> Self {
        let world = AtprotoWorld::start();
        world.add_account("ada.example.test", ADA);
        world.sign_in_as(ADA);
        let adapter = Atproto::new(World::loopback(&world.origin()).expect("a loopback world"));
        let request = SignInRequest {
            hint: String::new(),
            state: "tangent-state".to_owned(),
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
        let space = SpaceServer::start(&world.proofs());
        space.add_tangent("home", "Home", "Where everyone starts.");
        space.add_topic("home", "welcome", "Welcome", "Say hello.");
        space.name_account(ADA, "Ada", "ada.example.test");
        let bea = space.add_participant("Bea", "bea.example.test");
        space.add_post("welcome", &bea, "Welcome, everyone.", None);
        Self {
            world,
            space,
            minting: Minting {
                adapter,
                material: Mutex::new(signed.material),
                permissions: signed.permissions,
                asked: Mutex::new(Vec::new()),
            },
        }
    }

    fn welcome(&self) -> String {
        self.space.topic_ref("welcome")
    }

    fn proofs_sent(&self) -> Vec<String> {
        self.space
            .listener()
            .requests()
            .iter()
            .filter(|request| request.route() == "/api/v1/identity/exchange")
            .map(|request| {
                request
                    .header("authorization")
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect()
    }
}

impl ServiceFixture for Fixture {
    fn address(&self) -> Address {
        Address::parse(&self.space.origin(), AddressPolicy::LoopbackAllowed).expect("address")
    }

    fn authority(&self) -> &dyn Authority {
        &self.minting
    }

    fn minted(&self) -> Vec<String> {
        self.world
            .proofs()
            .minted()
            .into_iter()
            .map(|proof| proof.token)
            .collect()
    }

    fn plant(&self, author: &str, text: &str) {
        let someone = self.space.add_participant(author, "someone.example.test");
        self.space.add_post("welcome", &someone, text, None);
    }

    fn read(&self) -> (ToolName, ToolArguments) {
        let mut arguments = Map::new();
        arguments.insert("topic".to_owned(), Value::String(self.welcome()));
        (
            ToolName::new("read_topic").expect("tool"),
            ToolArguments(arguments),
        )
    }

    fn requests(&self) -> usize {
        self.space.listener().requests().len()
    }

    fn drop_issued(&self) {
        self.space.end_tokens();
    }

    fn grant_reading_only(&self) {
        self.space.grant_only(&["welcome", "read"]);
    }

    fn write(&self) -> (ToolName, ToolArguments) {
        (
            tool("post"),
            arguments(json!({ "topic": self.welcome(), "text": "Hello." })),
        )
    }
}

fn arguments(value: Value) -> ToolArguments {
    ToolArguments(value.as_object().cloned().unwrap_or_default())
}

fn tool(name: &str) -> ToolName {
    ToolName::new(name).expect("a tool name")
}

fn rendered(text: &[Passage]) -> String {
    lobby_contracts::ToolResult {
        text: text.to_vec(),
        structured: None,
    }
    .rendered()
}

#[test]
fn the_tangent_adapter_holds_to_the_service_contract() {
    service::run(&tangent_adapter(), &Fixture::start());
}

#[test]
fn a_connection_is_the_proof_exchange_and_keeps_only_what_the_space_issued() {
    let fixture = Fixture::start();
    let adapter = tangent_adapter();
    let address = fixture.address();
    let issued = adapter
        .establish(&address, &fixture.minting)
        .expect("established");

    // The profile was read with no credential, then the proof went once,
    // in the Authorization header only, to the exchange.
    let requests = fixture.space.listener().requests();
    let profile = &requests[0];
    assert_eq!(profile.route(), "/.well-known/tangent-auth");
    assert!(profile.header("authorization").is_none());
    let exchange = &requests[1];
    assert_eq!(exchange.route(), "/api/v1/identity/exchange");
    let presented = exchange.header("authorization").expect("the proof");
    let proof = presented.strip_prefix("Bearer ").expect("as Bearer");
    assert!(
        !String::from_utf8_lossy(&exchange.body).contains(proof),
        "never in the body"
    );
    assert_eq!(
        exchange.json(),
        json!({ "name": "Companion Lobby", "lifetimeDays": 7, "grants": ["welcome", "read", "post"] })
    );
    let asked = fixture.minting.asked.lock().expect("asked").clone();
    let audience = format!(
        "did:web:localhost%3A{}#tangent",
        fixture.space.listener().port()
    );
    assert_eq!(
        asked,
        [AuthorizationAsk::ServiceProof {
            audience: audience.clone(),
            method: "org.sylin.tangent.exchangeIdentity".to_owned(),
            lifetime_secs: 60,
        }]
    );
    let minted = fixture
        .world
        .proofs()
        .verify(proof)
        .expect("minted at the PDS");
    assert_eq!(
        (minted.issuer.as_str(), minted.audience.as_str()),
        (ADA, audience.as_str())
    );

    // What the connection keeps is the Space's token, never the proof.
    let kept = String::from_utf8_lossy(issued.as_bytes()).into_owned();
    assert!(!kept.contains(proof));
    assert!(
        fixture
            .space
            .tokens()
            .iter()
            .any(|token| kept.contains(token.as_str()))
    );

    // Establishing again mints a fresh proof, never one sent before.
    adapter
        .establish(&address, &fixture.minting)
        .expect("established again");
    let sent = fixture.proofs_sent();
    assert_eq!(sent.len(), 2);
    assert_ne!(sent[0], sent[1], "a fresh proof every time");
}

#[test]
fn a_profile_that_strays_from_the_address_gets_no_proof() {
    for profile in [
        Profile::WrongMethod,
        Profile::TamperedAudience,
        Profile::AnotherOrigin,
        Profile::AnotherEndpoint,
    ] {
        let fixture = Fixture::start();
        fixture.space.publish(profile);
        let refused = tangent_adapter()
            .establish(&fixture.address(), &fixture.minting)
            .expect_err("refused");
        assert_eq!(refused.sentence, tangent::PROFILE_MISMATCH, "{profile:?}");
        assert!(
            fixture.minting.asked.lock().expect("asked").is_empty(),
            "{profile:?}: the account was asked for nothing"
        );
        assert!(fixture.world.proofs().minted().is_empty());
    }
    let fixture = Fixture::start();
    fixture.space.publish(Profile::Unconfigured);
    let refused = tangent_adapter()
        .establish(&fixture.address(), &fixture.minting)
        .expect_err("refused");
    assert_eq!(refused.sentence, tangent::NOT_CONFIGURED);
}

#[test]
fn an_account_without_the_permission_is_told_to_sign_in_again_before_the_space_hears() {
    let fixture = Fixture::signed_in(&[]);
    let refused = tangent_adapter()
        .establish(&fixture.address(), &fixture.minting)
        .expect_err("refused");
    assert_eq!(refused.outcome, Outcome::NeedsPermission);
    assert!(fixture.proofs_sent().is_empty());
}

#[test]
fn a_suspended_account_and_a_refused_proof_read_as_the_spaces_reason() {
    let fixture = Fixture::start();
    fixture.space.suspend(ADA);
    let refused = tangent_adapter()
        .establish(&fixture.address(), &fixture.minting)
        .expect_err("refused");
    assert_eq!(
        refused,
        Explained::new(Outcome::Refused, tangent::SUSPENDED),
        "refused by the Space, never a sign-in to repeat"
    );
    let fixture = Fixture::start();
    fixture.space.reject_proofs(true);
    let refused = tangent_adapter()
        .establish(&fixture.address(), &fixture.minting)
        .expect_err("refused");
    assert_eq!(
        refused,
        Explained::new(Outcome::Refused, tangent::PROOF_REFUSED)
    );
}

#[test]
fn a_post_with_a_request_id_replays_as_the_same_post_and_a_conflict_is_named() {
    let fixture = Fixture::start();
    let adapter = tangent_adapter();
    let address = fixture.address();
    let slot = HeldIssued::new(
        adapter
            .establish(&address, &fixture.minting)
            .expect("established"),
        &fixture.minting,
    );
    let first_post = fixture.space.post_ref("p1");
    let reply = arguments(json!({
        "topic": fixture.welcome(),
        "text": "Hello, Bea!",
        "reply_to": first_post,
        "request_id": "reply-to-bea-1",
    }));
    let posted = adapter
        .call(&tool("post"), &reply, &address, &slot)
        .expect("posted");
    let again = adapter
        .call(&tool("post"), &reply, &address, &slot)
        .expect("replayed");
    let post = posted.structured.expect("structured")["post"].clone();
    assert_eq!(
        again.structured.expect("structured")["post"],
        post,
        "the same Post"
    );
    assert_eq!(fixture.space.posts().len(), 2, "posted once");
    assert!(rendered(&posted.text).contains("request_id reply-to-bea-1"));

    let other = arguments(json!({
        "topic": fixture.welcome(),
        "text": "Something else.",
        "request_id": "reply-to-bea-1",
    }));
    let conflict = adapter
        .call(&tool("post"), &other, &address, &slot)
        .expect_err("a conflict");
    assert_eq!(conflict.outcome, Outcome::Conflict);
    assert_eq!(
        conflict.sentence,
        lobby_service_tangent::wire::REQUEST_CONFLICT
    );

    // Left out, the request id is made from the content: sending the same
    // post again is safe too.
    let unnamed = arguments(json!({ "topic": fixture.welcome(), "text": "Twice, once." }));
    let one = adapter
        .call(&tool("post"), &unnamed, &address, &slot)
        .expect("posted");
    let two = adapter
        .call(&tool("post"), &unnamed, &address, &slot)
        .expect("replayed");
    assert_eq!(
        one.structured.expect("structured")["post"],
        two.structured.expect("structured")["post"]
    );
    assert_eq!(fixture.space.posts().len(), 3);
}

#[test]
fn a_forbidden_write_is_refused_in_the_spaces_words_and_never_unknown() {
    let fixture = Fixture::start();
    let adapter = tangent_adapter();
    let address = fixture.address();
    let slot = HeldIssued::new(
        adapter
            .establish(&address, &fixture.minting)
            .expect("established"),
        &fixture.minting,
    );
    fixture.space.refuse_writes_in("welcome");
    let refused = adapter
        .call(
            &tool("post"),
            &arguments(json!({ "topic": fixture.welcome(), "text": "Hello?" })),
            &address,
            &slot,
        )
        .expect_err("refused");
    assert_eq!(
        refused,
        Explained::new(Outcome::Refused, lobby_service_tangent::tools::POST_DENIED),
        "in the post's own nouns"
    );
    assert!(
        refused.sentence.contains("tangent_read_topic"),
        "with the next step"
    );
}

#[test]
fn a_token_the_space_stops_taking_is_exchanged_again_once_with_a_fresh_proof() {
    let fixture = Fixture::start();
    let adapter = tangent_adapter();
    let address = fixture.address();
    let slot = HeldIssued::new(
        adapter
            .establish(&address, &fixture.minting)
            .expect("established"),
        &fixture.minting,
    );
    let (read, read_arguments) = fixture.read();
    fixture.space.end_tokens();
    adapter
        .call(&read, &read_arguments, &address, &slot)
        .expect("read");
    assert_eq!(slot.reissued().len(), 1, "exchanged again once");
    let sent = fixture.proofs_sent();
    assert_eq!(sent.len(), 2);
    assert_ne!(sent[0], sent[1], "with a fresh proof");

    // A Space that refuses even the fresh token: one exchange, then the
    // Space's refusal, and no loop.
    fixture.space.suspend(ADA);
    let refused = adapter
        .call(&read, &read_arguments, &address, &slot)
        .expect_err("refused");
    assert_eq!(
        refused,
        Explained::new(Outcome::Refused, tangent::SUSPENDED)
    );
    assert_eq!(fixture.proofs_sent().len(), 3, "one more exchange, no more");
}

#[test]
fn a_token_about_to_expire_is_replaced_before_the_call() {
    let fixture = Fixture::start();
    fixture.space.issue_for(60);
    let adapter = tangent_adapter();
    let address = fixture.address();
    let slot = HeldIssued::new(
        adapter
            .establish(&address, &fixture.minting)
            .expect("established"),
        &fixture.minting,
    );
    fixture.space.issue_for(3600);
    let (read, read_arguments) = fixture.read();
    adapter
        .call(&read, &read_arguments, &address, &slot)
        .expect("read");
    assert_eq!(slot.reissued().len(), 1, "replaced before it expired");
    let reads = fixture
        .space
        .listener()
        .requests()
        .iter()
        .filter(|request| request.route().starts_with("/api/v1/experience/topics/"))
        .count();
    assert_eq!(reads, 1, "and the read went once, with the new token");
}

#[test]
fn a_token_about_to_expire_still_serves_the_call_when_it_cannot_be_replaced_just_now() {
    let fixture = Fixture::start();
    fixture.space.issue_for(60);
    let adapter = tangent_adapter();
    let address = fixture.address();
    let slot = HeldIssued::new(
        adapter
            .establish(&address, &fixture.minting)
            .expect("established"),
        &fixture.minting,
    );
    fixture.space.fail_exchanges(true);
    let exchanges = fixture.space.listener().count(EXCHANGE_PATH);
    let (read, read_arguments) = fixture.read();
    adapter
        .call(&read, &read_arguments, &address, &slot)
        .expect("read with the token it has");
    assert_eq!(
        fixture.space.listener().count(EXCHANGE_PATH),
        exchanges + 1,
        "a replacement was tried"
    );
    assert!(slot.reissued().is_empty(), "and nothing new was kept");
}

#[test]
fn a_reference_of_another_space_never_becomes_a_request() {
    let fixture = Fixture::start();
    let adapter = tangent_adapter();
    let address = fixture.address();
    let slot = HeldIssued::new(
        adapter
            .establish(&address, &fixture.minting)
            .expect("established"),
        &fixture.minting,
    );
    let before = fixture.space.listener().requests().len();
    for (name, value) in [
        (
            "read_topic",
            json!({ "topic": "http://127.0.0.1:1::home::welcome" }),
        ),
        (
            "read_topic",
            json!({ "topic": format!("{}::home::../welcome", fixture.space.origin()) }),
        ),
        (
            "post",
            json!({ "topic": fixture.welcome(), "text": "Hi", "reply_to": "http://127.0.0.1:1::home::welcome::p1" }),
        ),
        (
            "edit_post",
            json!({ "post": format!("{}::home::welcome", fixture.space.origin()), "text": "x" }),
        ),
        (
            "topics",
            json!({ "tangent": format!("{}::home/../x", fixture.space.origin()) }),
        ),
    ] {
        let refused = adapter
            .call(&tool(name), &arguments(value.clone()), &address, &slot)
            .expect_err("refused");
        assert_eq!(refused.outcome, Outcome::InvalidInput, "{name} {value}");
    }
    assert_eq!(
        fixture.space.listener().requests().len(),
        before,
        "nothing reached the Space"
    );
}

#[test]
fn one_account_at_two_spaces_keeps_two_connections_that_never_leak() {
    let fixture = Fixture::start();
    let second = SpaceServer::start(&fixture.world.proofs());
    second.add_tangent("home", "Home", "The second Space.");
    second.add_topic("home", "quiet", "Quiet", "Nothing yet.");
    second.name_account(ADA, "Ada", "ada.example.test");
    let adapter = tangent_adapter();
    let first_address = fixture.address();
    let second_address =
        Address::parse(&second.origin(), AddressPolicy::LoopbackAllowed).expect("address");
    let first = adapter
        .establish(&first_address, &fixture.minting)
        .expect("first");
    let other = adapter
        .establish(&second_address, &fixture.minting)
        .expect("second");
    let asked = fixture.minting.asked.lock().expect("asked").clone();
    let audiences: Vec<String> = asked
        .iter()
        .filter_map(|ask| match ask {
            AuthorizationAsk::ServiceProof { audience, .. } => Some(audience.clone()),
            AuthorizationAsk::ProvedRequest { .. } => None,
        })
        .collect();
    assert_ne!(
        audiences[0], audiences[1],
        "each Space gets a proof for itself"
    );
    let first_tokens = fixture.space.tokens();
    let second_tokens = second.tokens();
    let other_kept = String::from_utf8_lossy(other.as_bytes()).into_owned();
    let first_kept = String::from_utf8_lossy(first.as_bytes()).into_owned();
    assert!(
        secrets_in(
            other_kept.as_bytes(),
            &first_tokens.iter().map(String::as_str).collect::<Vec<_>>()
        )
        .is_empty()
    );
    assert!(
        secrets_in(
            first_kept.as_bytes(),
            &second_tokens.iter().map(String::as_str).collect::<Vec<_>>()
        )
        .is_empty()
    );
    // A Topic of the first Space read through the second connection is a
    // reference of another Space.
    let slot = HeldIssued::new(other, &fixture.minting);
    let refused = adapter
        .call(
            &tool("read_topic"),
            &arguments(json!({ "topic": fixture.welcome() })),
            &second_address,
            &slot,
        )
        .expect_err("refused");
    assert!(
        refused
            .sentence
            .starts_with("topic holds a reference of another Space."),
        "{}",
        refused.sentence
    );
    let read = adapter
        .call(
            &tool("read_topic"),
            &arguments(json!({ "topic": second.topic_ref("quiet") })),
            &second_address,
            &slot,
        )
        .expect("read at the second");
    assert!(rendered(&read.text).contains("0 Posts"));
}

#[test]
fn arriving_says_who_the_companion_is_and_what_is_new_with_other_peoples_words_quoted() {
    let fixture = Fixture::start();
    let adapter = tangent_adapter();
    let address = fixture.address();
    let slot = HeldIssued::new(
        adapter
            .establish(&address, &fixture.minting)
            .expect("established"),
        &fixture.minting,
    );
    let arrived = adapter
        .call(&tool("arrive"), &ToolArguments::default(), &address, &slot)
        .expect("arrived");
    let said = rendered(&arrived.text);
    assert!(said.starts_with("You are \"Ada\" here"), "{said}");
    assert!(said.contains("You may read and post here."), "{said}");
    assert!(said.contains("1 new item"), "{said}");
    assert!(said.contains(&fixture.space.post_ref("p1")), "{said}");
    assert!(said.contains(&fixture.space.tangent_ref("home")), "{said}");
    for passage in &arrived.text {
        if let Passage::Authored(text) = passage {
            assert!(
                !text.contains("Welcome, everyone"),
                "quoted, never authored"
            );
        }
    }
}

#[test]
fn a_connection_granted_less_offers_less() {
    let fixture = Fixture::start();
    fixture.space.grant_only(&["welcome", "read"]);
    let adapter = tangent_adapter();
    let address = fixture.address();
    let issued = adapter
        .establish(&address, &fixture.minting)
        .expect("established");
    let available: Vec<String> = adapter
        .available(&issued)
        .iter()
        .map(|name| name.as_str().to_owned())
        .collect();
    assert!(available.contains(&"read_topic".to_owned()));
    assert!(!available.contains(&"post".to_owned()));
    let slot = HeldIssued::new(issued, &fixture.minting);
    let refused = adapter
        .call(
            &tool("post"),
            &arguments(json!({ "topic": fixture.welcome(), "text": "Hi" })),
            &address,
            &slot,
        )
        .expect_err("not granted");
    assert_eq!(
        refused,
        Explained::new(Outcome::NeedsPermission, tangent::NOT_GRANTED)
    );
}
