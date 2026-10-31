//! The Tangent service adapter holds to the service contract, in process,
//! against the test kit's fake Space, with proofs the atproto credential
//! adapter mints at the fake PDS for an account that really signed in.

use std::sync::Mutex;

use lobby_conformance::credential::CountingSlot;
use lobby_conformance::service::{self, HeldIssued, ServiceFixture};
use lobby_contracts::{
    Address, AddressPolicy, Authority, Authorization, AuthorizationAsk, CredentialAdapter,
    Explained, IssuedSlot, Outcome, Passage, SealedMaterial, ServiceAdapter, SignInRequest,
    SignInReturn, ToolArguments, ToolName,
};
use lobby_credential_atproto::{Atproto, World};
use lobby_service_tangent::wire::TOKEN_HEADER;
use lobby_service_tangent::{self as tangent, Tangent};
use lobby_testkit::atproto::AtprotoWorld;
use lobby_testkit::listener::Received;
use lobby_testkit::space::{EXCHANGE_PATH, Profile, SpaceServer};
use lobby_testkit::{percent_decode, secrets_in};
use serde_json::{Map, Value, json};

const ADA: &str = "did:plc:adaadaadaadaadaadaadaada";
const BEA: &str = "did:plc:beabeabeabeabeabeabeabea";

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

/// The account `did` signed in at `world`, with `permissions`.
fn signed_in(world: &AtprotoWorld, handle: &str, did: &str, permissions: &[String]) -> Minting {
    world.add_account(handle, did);
    world.sign_in_as(did);
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
    Minting {
        adapter,
        material: Mutex::new(signed.material),
        permissions: signed.permissions,
        asked: Mutex::new(Vec::new()),
    }
}

struct Fixture {
    world: AtprotoWorld,
    space: SpaceServer,
    minting: Minting,
    bea: String,
    opening: String,
}

impl Fixture {
    fn start() -> Self {
        Self::signed_in(&[tangent::PERMISSION.to_owned()])
    }

    fn signed_in(permissions: &[String]) -> Self {
        let world = AtprotoWorld::start();
        let minting = signed_in(&world, "ada.example.test", ADA, permissions);
        let space = SpaceServer::start(&world.proofs());
        space.add_tangent("home", "Home", "Where everyone starts.");
        space.add_topic("home", "welcome", "Welcome", "Say hello.");
        space.name_account(ADA, "Ada", "ada.example.test");
        let bea = space.add_participant("Bea", "bea.example.test");
        let opening = space.add_post("welcome", &bea, "Welcome, everyone.", None);
        Self {
            world,
            space,
            minting,
            bea,
            opening,
        }
    }

    fn welcome(&self) -> String {
        self.space.topic_ref("welcome")
    }

    fn connected(&self) -> HeldIssued<'_> {
        HeldIssued::new(
            tangent_adapter()
                .establish(&self.address(), &self.minting)
                .expect("established"),
            &self.minting,
        )
    }

    fn call(&self, slot: &dyn IssuedSlot, name: &str, value: Value) -> Result<Value, Explained> {
        tangent_adapter()
            .call(&tool(name), &arguments(value), &self.address(), slot)
            .map(|result| result.structured.expect("structured"))
    }

    fn proofs_sent(&self) -> Vec<String> {
        self.space
            .listener()
            .requests()
            .iter()
            .filter(|request| request.route() == EXCHANGE_PATH)
            .map(|request| {
                request
                    .header("authorization")
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect()
    }

    /// The requests that wrote through the Space's forms, oldest first.
    fn writes(&self) -> Vec<Received> {
        self.space
            .listener()
            .requests()
            .into_iter()
            .filter(|request| request.method == "POST" && request.route() != EXCHANGE_PATH)
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
        self.space.end_sign_ins();
    }

    fn grant_reading_only(&self) {
        self.space.hold_as_applicants(true);
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

fn kept(slot: &HeldIssued<'_>) -> String {
    String::from_utf8_lossy(slot.issued().as_bytes()).into_owned()
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

    // The profile was read with no credential, then the proof went once, in
    // the Authorization header only and with no body, to the exchange.
    let requests = fixture.space.listener().requests();
    let profile = &requests[0];
    assert_eq!(profile.route(), "/.well-known/tangent-auth");
    assert!(profile.header("authorization").is_none());
    let exchange = &requests[1];
    assert_eq!(exchange.route(), EXCHANGE_PATH);
    let presented = exchange.header("authorization").expect("the proof");
    let proof = presented.strip_prefix("Bearer ").expect("as Bearer");
    assert!(exchange.body.is_empty(), "the exchange reads no body");
    assert!(exchange.header("cookie").is_none());
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

    // Then the door, by the cookie alone, says who the Space takes it for.
    let door = &requests[2];
    assert_eq!(door.route(), "/");
    assert!(
        door.header("authorization").is_none(),
        "the proof went once"
    );
    assert_eq!(door.header("accept"), Some("application/json"));
    assert_eq!(
        fixture.space.signed_in_as(door).as_deref(),
        Some(ADA),
        "by the cookie the exchange set"
    );
    let known = adapter.known_as(&issued).expect("known");
    assert_eq!(known.name.as_deref(), Some("Ada"));
    assert_eq!(known.reference.as_deref(), Some(ADA));
    assert_eq!(known.allows, "read and post");

    // What the connection keeps is the Space's cookie, never the proof.
    let held = String::from_utf8_lossy(issued.as_bytes()).into_owned();
    assert!(!held.contains(proof));
    assert!(
        fixture
            .space
            .cookies()
            .iter()
            .any(|cookie| held.contains(cookie.as_str()))
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
fn a_banned_account_and_a_refused_proof_read_as_the_spaces_reason() {
    let fixture = Fixture::start();
    fixture.space.ban(ADA);
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
fn one_cookie_jar_per_connection_goes_with_every_request_and_grows_as_the_space_sets_more() {
    let fixture = Fixture::start();
    let slot = fixture.connected();
    let exchanged = kept(&slot);
    assert!(
        !exchanged.contains("Antiforgery"),
        "the exchange set only what signs the account in"
    );

    // Reading the page that carries forms sets the antiforgery cookie, and
    // the connection keeps it for its next call.
    fixture
        .call(&slot, "read_topic", json!({ "topic": fixture.welcome() }))
        .expect("read");
    assert!(kept(&slot).contains("Antiforgery"), "{}", kept(&slot));
    fixture
        .call(&slot, "read_topic", json!({ "topic": fixture.welcome() }))
        .expect("read again");
    let requests = fixture.space.listener().requests();
    let last = requests.last().expect("a request");
    let cookies = last.header("cookie").expect("cookies");
    assert!(
        cookies.contains(".AspNetCore.Koan.cookie=") && cookies.contains("Antiforgery"),
        "{cookies}"
    );
    assert_eq!(
        fixture.proofs_sent().len(),
        1,
        "nothing asks the account to prove itself again"
    );
    assert_eq!(slot.reissued().len(), 1, "kept once, when it changed");
}

#[test]
fn two_connections_at_one_space_as_two_accounts_never_share_a_cookie_or_a_write() {
    let fixture = Fixture::start();
    let bea_minting = signed_in(
        &fixture.world,
        "beatrice.example.test",
        BEA,
        &[tangent::PERMISSION.to_owned()],
    );
    fixture
        .space
        .name_account(BEA, "Beatrice", "beatrice.example.test");
    let adapter = tangent_adapter();
    let address = fixture.address();
    let ada = fixture.connected();
    let bea = HeldIssued::new(
        adapter.establish(&address, &bea_minting).expect("Bea's"),
        &bea_minting,
    );

    let welcome = fixture.welcome();
    let as_ada = fixture
        .call(
            &ada,
            "post",
            json!({ "topic": welcome, "text": "From Ada.", "request_id": "same-name" }),
        )
        .expect("Ada's post");
    let as_bea = fixture
        .call(
            &bea,
            "post",
            json!({ "topic": welcome, "text": "From Bea.", "request_id": "same-name" }),
        )
        .expect("Bea's post");
    assert_ne!(
        as_ada["post"], as_bea["post"],
        "two Posts, under one name each"
    );
    let posts = fixture.space.posts();
    let author_of = |text: &str| {
        posts
            .iter()
            .find(|post| post.text == text)
            .map(|post| post.author.clone())
    };
    assert_eq!(author_of("From Ada.").as_deref(), Some(ADA));
    assert_eq!(author_of("From Bea.").as_deref(), Some(BEA));

    // Each connection's forms went with its own cookies, and each kept only
    // its own: no value one holds is in what the other keeps.
    let (ada_kept, bea_kept) = (kept(&ada), kept(&bea));
    for write in fixture.writes() {
        let author = fixture.space.signed_in_as(&write).expect("signed in");
        let cookies = write.header("cookie").expect("cookies");
        let own = if author == ADA { &ada_kept } else { &bea_kept };
        for pair in cookies.split("; ") {
            let value = pair.split_once('=').map_or("", |(_, value)| value);
            assert!(own.contains(value), "{author} sent what it does not hold");
        }
    }
    let issued = fixture.space.cookies();
    let ada_values: Vec<&str> = issued
        .iter()
        .map(String::as_str)
        .filter(|cookie| ada_kept.contains(cookie))
        .collect();
    assert!(ada_values.len() >= 2, "signing in and its forms' cookie");
    assert!(
        secrets_in(bea_kept.as_bytes(), &ada_values).is_empty(),
        "Bea's connection holds nothing of Ada's"
    );
}

#[test]
fn two_connections_of_one_account_at_one_space_end_and_prove_again_apart() {
    let fixture = Fixture::start();
    let first = fixture.connected();
    let first_cookie = fixture.space.cookies()[0].clone();
    let second = fixture.connected();
    assert_ne!(kept(&first), kept(&second), "each signed in for itself");
    assert!(!kept(&second).contains(&first_cookie));

    // The Space ends the first connection's sign-in: it proves the account
    // again, once, and the second never hears of it.
    fixture.space.end_sign_in(&first_cookie);
    let before = fixture.proofs_sent().len();
    fixture
        .call(&second, "read_topic", json!({ "topic": fixture.welcome() }))
        .expect("the second reads");
    assert_eq!(fixture.proofs_sent().len(), before, "with what it holds");
    fixture
        .call(&first, "read_topic", json!({ "topic": fixture.welcome() }))
        .expect("the first reads");
    assert_eq!(
        fixture.proofs_sent().len(),
        before + 1,
        "after proving itself"
    );
    assert!(
        !kept(&first).contains(&first_cookie),
        "with a cookie of its own"
    );
    assert_ne!(kept(&first), kept(&second));
}

#[test]
fn a_tool_reads_the_page_and_writes_the_form_the_page_carries() {
    let fixture = Fixture::start();
    let slot = fixture.connected();
    let before = fixture.space.listener().requests().len();
    let welcome = fixture.welcome();
    let opening = fixture.space.post_ref(&fixture.opening);
    let posted = fixture
        .call(
            &slot,
            "post",
            json!({ "topic": welcome, "text": "Hello, Bea!", "reply_to": opening, "request_id": "hello-1" }),
        )
        .expect("posted");

    let requests = fixture.space.listener().requests();
    let [read, wrote] = &requests[before..] else {
        panic!("a page and its form: {:?}", &requests[before..]);
    };
    let route = fixture.space.topic_route("welcome");
    assert_eq!(
        (read.method.as_str(), read.route()),
        ("GET", route.as_str())
    );
    assert_eq!(read.header("accept"), Some("application/json"));
    assert_eq!(
        (wrote.method.as_str(), wrote.route()),
        ("POST", route.as_str())
    );
    assert_eq!(wrote.header("accept"), Some("application/json"));
    assert_eq!(wrote.header("content-type"), Some("application/json"));
    assert!(
        wrote.header("authorization").is_none(),
        "cookies, and no bearer"
    );
    let token = wrote.header(TOKEN_HEADER).expect("the form's token");
    assert!(token.starts_with("rv_"), "{token}");

    let sent = wrote.json();
    assert_eq!(sent["body"], "Hello, Bea!");
    assert_eq!(
        sent["parent"], fixture.opening,
        "the Post it answers, by its id"
    );
    let id = sent["id"].as_str().expect("an id");
    assert_eq!(id.len(), 13, "a TID: {id}");
    assert_eq!(
        sent.as_object().expect("one object").len(),
        3,
        "nothing the form does not name: {sent}"
    );
    let post = fixture.space.posts().pop().expect("the reply");
    assert_eq!(post.author, ADA);
    assert_eq!(post.id, id);
    assert_eq!(post.reply_to.as_deref(), Some(fixture.opening.as_str()));
    assert_eq!(posted["post"], format!("{welcome}/{id}"));

    // And it reads back through the page.
    let read = fixture
        .call(&slot, "read_topic", json!({ "topic": welcome }))
        .expect("read");
    let posts = read["posts"].as_array().expect("posts");
    let mine = posts.last().expect("the newest");
    assert_eq!(mine["post"], posted["post"]);
    assert_eq!(mine["quoted"]["author"], ADA);
    assert_eq!(mine["quoted"]["text"], "Hello, Bea!");
    assert_eq!(mine["replyTo"], opening);
    assert_eq!(mine["canChange"], true);
    assert_eq!(read["canPost"], true);
}

#[test]
fn a_post_with_a_request_id_replays_as_the_same_post_and_a_conflict_is_named() {
    let fixture = Fixture::start();
    let adapter = tangent_adapter();
    let address = fixture.address();
    let slot = fixture.connected();
    let first_post = fixture.space.post_ref(&fixture.opening);
    let replies_before = fixture.space.posts().len();
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
    assert_eq!(
        fixture.space.posts().len(),
        replies_before + 1,
        "posted once"
    );
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
    assert_eq!(fixture.space.posts().len(), replies_before + 2);

    // A request id means one write at one target: the same name in another
    // Topic is a write of its own.
    fixture
        .space
        .add_topic("home", "elsewhere", "Elsewhere", "Over here.");
    let elsewhere = adapter
        .call(
            &tool("post"),
            &arguments(json!({
                "topic": fixture.space.topic_ref("elsewhere"),
                "text": "Hello, Bea!",
                "request_id": "reply-to-bea-1",
            })),
            &address,
            &slot,
        )
        .expect("posted elsewhere");
    assert!(
        rendered(&elsewhere.text).contains("/t/home/elsewhere/"),
        "{}",
        rendered(&elsewhere.text)
    );
}

#[test]
fn every_write_a_page_offers_goes_through_its_form() {
    let fixture = Fixture::start();
    let slot = fixture.connected();
    let home = fixture.space.tangent_ref("home");
    let welcome = fixture.welcome();
    let started = fixture
        .call(
            &slot,
            "start_topic",
            json!({ "tangent": home, "title": "A Topic of Ada's", "text": "It opens here." }),
        )
        .expect("started");
    let topic = started["topic"].as_str().expect("a Topic");
    assert_eq!(topic, format!("{home}/a-topic-of-ada-s"));
    let again = fixture
        .call(
            &slot,
            "start_topic",
            json!({ "tangent": home, "title": "A Topic of Ada's", "text": "It opens here." }),
        )
        .expect("the same");
    assert_eq!(again["post"], started["post"], "started once");

    let posted = fixture
        .call(&slot, "post", json!({ "topic": welcome, "text": "Mine." }))
        .expect("posted");
    let mine = posted["post"].as_str().expect("a Post");
    fixture
        .call(
            &slot,
            "edit_post",
            json!({ "post": mine, "text": "Mine, edited." }),
        )
        .expect("edited");
    assert!(
        fixture
            .space
            .posts()
            .iter()
            .any(|post| post.text == "Mine, edited." && post.edited),
        "the form's version went with it"
    );
    fixture
        .call(&slot, "remove_post", json!({ "post": mine }))
        .expect("removed");
    assert!(fixture.space.posts().iter().any(|post| post.removed));

    // Someone else's Post offers a report and no edit.
    let hers = fixture.space.post_ref(&fixture.opening);
    let refused = fixture
        .call(
            &slot,
            "edit_post",
            json!({ "post": hers, "text": "Not mine." }),
        )
        .expect_err("refused");
    assert_eq!(
        refused,
        Explained::new(Outcome::Refused, lobby_service_tangent::wire::NOT_YOURS)
    );
    fixture
        .call(
            &slot,
            "report_post",
            json!({ "post": hers, "reason": "A reason.", "request_id": "r1" }),
        )
        .expect("reported");
    assert_eq!(
        fixture.space.reports(),
        [(
            fixture.opening.clone(),
            ADA.to_owned(),
            "A reason.".to_owned()
        )]
    );

    fixture
        .call(&slot, "watch", json!({ "scope": welcome, "watch": true }))
        .expect("watching");
    fixture
        .call(&slot, "watch", json!({ "scope": home, "watch": true }))
        .expect("watching the Tangent");
    let read = fixture
        .call(&slot, "read_topic", json!({ "topic": welcome }))
        .expect("read");
    let through = read["through"].as_u64().expect("how far to mark");
    let marked = fixture
        .call(
            &slot,
            "mark_read",
            json!({ "topic": welcome, "through": through }),
        )
        .expect("marked");
    assert_eq!(marked["through"], through);
    assert!(
        fixture
            .call(&slot, "read_topic", json!({ "topic": welcome }))
            .expect("read again")["through"]
            .is_null(),
        "nothing left to mark"
    );

    // Every write carried the form's token and the cookie it pairs with.
    for write in fixture.writes() {
        assert!(write.header(TOKEN_HEADER).is_some(), "{}", write.target);
        assert!(
            write
                .header("cookie")
                .is_some_and(|cookies| cookies.contains("Antiforgery"))
        );
    }

    // Catching up clears what is new, and then there is nothing to clear.
    fixture
        .space
        .add_post("welcome", &fixture.bea, "Anyone?", None);
    let news = fixture.call(&slot, "updates", json!({})).expect("news");
    assert_eq!(
        news["attention"]["catchUpWith"], "tangent_caught_up",
        "{news}"
    );
    fixture
        .call(&slot, "caught_up", json!({}))
        .expect("caught up");
    let quiet = fixture.call(&slot, "updates", json!({})).expect("quiet");
    assert!(quiet["attention"]["catchUpWith"].is_null(), "{quiet}");
    fixture
        .call(&slot, "caught_up", json!({}))
        .expect("nothing to do");
}

#[test]
fn a_page_without_a_form_says_the_gate_and_a_refused_form_says_it_in_the_tools_nouns() {
    let fixture = Fixture::start();
    let slot = fixture.connected();
    let writes = fixture.writes().len();
    fixture.space.lock("welcome");
    let refused = fixture
        .call(
            &slot,
            "post",
            json!({ "topic": fixture.welcome(), "text": "Hello?" }),
        )
        .expect_err("refused");
    assert_eq!(
        refused,
        Explained::new(Outcome::Refused, lobby_service_tangent::wire::LOCKED),
        "the page's own reason"
    );
    assert_eq!(fixture.writes().len(), writes, "and no form to send");

    let fixture = Fixture::start();
    let slot = fixture.connected();
    fixture.space.refuse_replies_in("welcome");
    let refused = fixture
        .call(
            &slot,
            "post",
            json!({ "topic": fixture.welcome(), "text": "Hello?" }),
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
fn a_sign_in_the_space_ended_is_proved_again_once_with_a_fresh_proof() {
    let fixture = Fixture::start();
    let adapter = tangent_adapter();
    let address = fixture.address();
    let slot = fixture.connected();
    let (read, read_arguments) = fixture.read();
    let held = kept(&slot);
    fixture.space.end_sign_ins();
    adapter
        .call(&read, &read_arguments, &address, &slot)
        .expect("read");
    let sent = fixture.proofs_sent();
    assert_eq!(sent.len(), 2, "proved again once");
    assert_ne!(sent[0], sent[1], "with a fresh proof");
    assert_ne!(kept(&slot), held, "and what it set is kept");

    // A write finds it out just the same, and writes once.
    fixture.space.end_sign_ins();
    let posted = fixture
        .call(
            &slot,
            "post",
            json!({ "topic": fixture.welcome(), "text": "After proving again." }),
        )
        .expect("posted");
    assert!(posted["post"].is_string());
    assert_eq!(fixture.proofs_sent().len(), 3);

    // A Space closed to visitors says so by 401, with the same answer.
    fixture.space.close_to_visitors();
    fixture.space.end_sign_ins();
    adapter
        .call(&read, &read_arguments, &address, &slot)
        .expect("read");
    assert_eq!(fixture.proofs_sent().len(), 4);

    // A Space that shows nothing to the account: one exchange, then the
    // Space's refusal, and no loop.
    fixture.space.end_sign_ins();
    fixture.space.ban(ADA);
    let refused = adapter
        .call(&read, &read_arguments, &address, &slot)
        .expect_err("refused");
    assert_eq!(
        refused,
        Explained::new(Outcome::Refused, tangent::SUSPENDED)
    );
    assert_eq!(fixture.proofs_sent().len(), 5, "one more exchange, no more");
}

#[test]
fn a_reference_of_another_space_never_becomes_a_request() {
    let fixture = Fixture::start();
    let adapter = tangent_adapter();
    let address = fixture.address();
    let slot = fixture.connected();
    let before = fixture.space.listener().requests().len();
    let origin = fixture.space.origin();
    for (name, value) in [
        (
            "read_topic",
            json!({ "topic": "http://127.0.0.1:1/t/home/welcome" }),
        ),
        (
            "read_topic",
            json!({ "topic": format!("{origin}/t/home/../welcome") }),
        ),
        (
            "read_topic",
            json!({ "topic": fixture.welcome(), "cursor": "http://127.0.0.1:1/t/home/welcome?after=1" }),
        ),
        (
            "post",
            json!({ "topic": fixture.welcome(), "text": "Hi", "reply_to": "http://127.0.0.1:1/t/home/welcome/p1" }),
        ),
        (
            "edit_post",
            json!({ "post": format!("{origin}/t/home/welcome"), "text": "x" }),
        ),
        (
            "topics",
            json!({ "tangent": format!("{origin}/t/home/../x") }),
        ),
        ("watch", json!({ "scope": origin, "watch": true })),
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
    let first_cookies = fixture.space.cookies();
    let second_cookies = second.cookies();
    let other_kept = String::from_utf8_lossy(other.as_bytes()).into_owned();
    let first_kept = String::from_utf8_lossy(first.as_bytes()).into_owned();
    assert!(
        secrets_in(
            other_kept.as_bytes(),
            &first_cookies.iter().map(String::as_str).collect::<Vec<_>>()
        )
        .is_empty()
    );
    assert!(
        secrets_in(
            first_kept.as_bytes(),
            &second_cookies
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
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
    assert!(rendered(&read.text).contains("1 Post"), "its opening");
}

#[test]
fn arriving_says_who_the_companion_is_and_what_is_new_with_other_peoples_words_quoted() {
    let fixture = Fixture::start();
    let adapter = tangent_adapter();
    let address = fixture.address();
    let slot = fixture.connected();
    let first = adapter
        .call(&tool("arrive"), &ToolArguments::default(), &address, &slot)
        .expect("arrived");
    assert!(
        rendered(&first.text).contains("first arrival")
            || !rendered(&first.text).contains("A reply"),
        "{}",
        rendered(&first.text)
    );
    let posted = fixture
        .call(
            &slot,
            "post",
            json!({ "topic": fixture.welcome(), "text": "Hello." }),
        )
        .expect("posted");
    let mine = fixture.space.posts().pop().expect("mine").id;
    fixture
        .space
        .add_post("welcome", &fixture.bea, "Welcome, Ada.", Some(&mine));
    let reply = fixture.space.posts().pop().expect("the reply").id;
    let arrived = adapter
        .call(&tool("arrive"), &ToolArguments::default(), &address, &slot)
        .expect("arrived");
    let said = rendered(&arrived.text);
    assert!(said.starts_with("You are \"Ada\" here"), "{said}");
    assert!(said.contains("You may read and post here."), "{said}");
    assert!(said.contains("A reply to you by \"Bea\""), "{said}");
    assert!(said.contains(&fixture.space.post_ref(&reply)), "{said}");
    assert!(said.contains(&fixture.space.tangent_ref("home")), "{said}");
    assert!(said.contains("tangent_caught_up"), "{said}");
    for passage in &arrived.text {
        if let Passage::Authored(text) = passage {
            assert!(!text.contains("Welcome, Ada"), "quoted, never authored");
        }
    }
    let item = &arrived.structured.expect("structured")["attention"]["items"][0];
    assert_eq!(item["kind"], "reply");
    assert_eq!(item["quoted"]["actorName"], "Bea");
    assert_eq!(item["at"], fixture.space.post_ref(&reply));
    assert!(posted["post"].is_string());
}

#[test]
fn an_applicant_reads_is_offered_no_write_and_hears_so_until_arriving_finds_it_admitted() {
    let fixture = Fixture::start();
    fixture.space.hold_as_applicants(true);
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
    assert_eq!(
        adapter.known_as(&issued).expect("known").allows,
        "read, but not post"
    );
    let slot = HeldIssued::new(issued, &fixture.minting);
    let refused = fixture
        .call(
            &slot,
            "post",
            json!({ "topic": fixture.welcome(), "text": "Hi" }),
        )
        .expect_err("not yet");
    assert_eq!(
        refused,
        Explained::new(Outcome::NeedsPermission, tangent::NOT_GRANTED)
    );
    let arrived = adapter
        .call(&tool("arrive"), &ToolArguments::default(), &address, &slot)
        .expect("arrived");
    let said = rendered(&arrived.text);
    assert!(said.contains("You may read here."), "{said}");
    assert!(
        said.contains(lobby_service_tangent::wire::NOT_ADMITTED),
        "{said}"
    );
    assert!(said.contains("this connection cannot post here"), "{said}");
    let read = fixture
        .call(&slot, "read_topic", json!({ "topic": fixture.welcome() }))
        .expect("it reads");
    assert_eq!(read["canPost"], false);

    // Admitted, its next arrival finds out, and the connection writes.
    fixture.space.hold_as_applicants(false);
    fixture.call(&slot, "arrive", json!({})).expect("arrived");
    assert!(
        adapter
            .available(&slot.issued())
            .iter()
            .any(|name| name.as_str() == "post")
    );
    fixture
        .call(
            &slot,
            "post",
            json!({ "topic": fixture.welcome(), "text": "Hi" }),
        )
        .expect("posted");
}

#[test]
fn what_the_lobby_cannot_read_as_its_own_is_proved_anew() {
    let fixture = Fixture::start();
    let slot = HeldIssued::new(
        lobby_contracts::SealedIssued::new(br#"{"format":"tangent-1"}"#.to_vec()),
        &fixture.minting,
    );
    let adapter = tangent_adapter();
    assert!(
        adapter
            .available(&slot.issued())
            .iter()
            .any(|name| name.as_str() == "arrive"),
        "its tools stay offered"
    );
    let arrived = fixture.call(&slot, "arrive", json!({})).expect("arrived");
    assert_eq!(arrived["you"]["quoted"]["participant"], ADA);
    assert_eq!(fixture.proofs_sent().len(), 1);
    assert!(adapter.known_as(&slot.issued()).is_some());
}

#[test]
fn a_long_topic_is_read_a_page_at_a_time_and_every_post_is_reached() {
    let fixture = Fixture::start();
    let mut planted = Vec::new();
    for index in 0..30 {
        planted.push(fixture.space.add_post(
            "welcome",
            &fixture.bea,
            &format!("Post number {index}."),
            None,
        ));
    }
    let slot = fixture.connected();
    let welcome = fixture.welcome();
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    let mut read_through = 0;
    for _ in 0..10 {
        let mut asked = json!({ "topic": welcome, "limit": 7 });
        if let Some(cursor) = &cursor {
            asked["cursor"] = json!(cursor);
        }
        let read = fixture.call(&slot, "read_topic", asked).expect("read");
        for post in read["posts"].as_array().expect("posts") {
            seen.push(post["post"].as_str().expect("a Post").to_owned());
        }
        read_through = read["through"].as_u64().unwrap_or(read_through);
        match read["cursor"].as_str() {
            Some(next) => cursor = Some(next.to_owned()),
            None => break,
        }
    }
    for id in &planted {
        let address = fixture.space.post_ref(id);
        assert_eq!(
            seen.iter().filter(|post| **post == address).count(),
            1,
            "{address} once"
        );
    }
    assert_eq!(read_through, 31, "the last one, with the first Bea posted");
}
