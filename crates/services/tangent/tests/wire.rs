//! The Tangent adapter's requests on the wire and the answers it does not
//! get, against loopback listeners that play a Space.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;

use lobby_contracts::{
    Address, AddressPolicy, Authority, Authorization, AuthorizationAsk, Effect, Explained, Outcome,
    ProviderKind, Retry, Secret, ServiceAdapter, ToolArguments,
};
use lobby_outbound::Outbound;
use lobby_service_tangent::issued::Issued;
use lobby_service_tangent::jar::Jar;
use lobby_service_tangent::tools::{self, Through};
use lobby_service_tangent::wire::{self, Failed};
use lobby_service_tangent::{self as tangent, Tangent};
use lobby_testkit::listener::{Listener, Reply};
use serde_json::{Value, json};

const COOKIE: &str = "signed=secret-cookie";

fn issued() -> Issued {
    let mut jar = Jar::default();
    jar.absorb(&format!("{COOKIE}; path=/"), "/", false, 0);
    let mut issued = Issued::signed_in(jar);
    issued.participant = Some("prt_1".to_owned());
    issued.name = Some("Ada".to_owned());
    issued.posts = true;
    issued
}

fn arguments(value: Value) -> ToolArguments {
    ToolArguments(value.as_object().cloned().unwrap_or_default())
}

fn perform(address: &Address, name: &str, value: Value) -> Result<String, Failed> {
    let outbound = Outbound::new(AddressPolicy::LoopbackAllowed);
    tools::perform(
        tools::tool(name).expect("a tool"),
        &arguments(value),
        &Through::new(&outbound, address, issued()),
    )
    .map(|result| result.rendered())
}

fn who() -> Value {
    json!({ "id": "prt_1", "name": "Ada", "handle": "ada.example.test", "signOut": "http://x/auth/logout" })
}

fn origin_of(request: &lobby_testkit::listener::Received) -> String {
    format!("http://{}", request.header("host").unwrap_or_default())
}

/// A Space that reads one request whole and closes without answering, as
/// one whose answer is lost on the way.
fn losing() -> Address {
    let socket = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = socket.local_addr().expect("address").port();
    std::thread::spawn(move || {
        for stream in socket.incoming().flatten().take(1) {
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 && line != "\r\n" {
                line.clear();
            }
        }
    });
    Address::parse(
        &format!("http://127.0.0.1:{port}"),
        AddressPolicy::LoopbackAllowed,
    )
    .expect("address")
}

/// A Space that answers every read with `page`, whose forms its `{origin}`
/// names, and loses the answer to the first write.
fn losing_writes(page: Value) -> Address {
    let socket = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = socket.local_addr().expect("address").port();
    let page = page
        .to_string()
        .replace("{origin}", &format!("http://127.0.0.1:{port}"));
    std::thread::spawn(move || {
        for stream in socket.incoming().flatten() {
            let mut reader = BufReader::new(stream);
            let mut method = String::new();
            let mut length = 0;
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 && line != "\r\n" {
                if method.is_empty() {
                    method = line.split(' ').next().unwrap_or_default().to_owned();
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap_or(0);
                }
                line.clear();
            }
            let mut body = vec![0; length];
            let _ = reader.read_exact(&mut body);
            if method == "GET" {
                let mut stream = reader.into_inner();
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
                    page.len()
                );
            }
        }
    });
    Address::parse(
        &format!("http://127.0.0.1:{port}"),
        AddressPolicy::LoopbackAllowed,
    )
    .expect("address")
}

fn form_page(field: &str) -> Value {
    json!({
        "who": who(),
        "address": "{origin}/t/home/welcome",
        field: {
            "action": "{origin}/t/home/welcome/form",
            "method": "POST",
            "fields": [
                { "name": "id", "holds": "An id.", "value": "3mz5acrpyshjl" },
                { "name": "title", "holds": "A title." },
                { "name": "body", "holds": "Words." },
                { "name": "parent", "holds": "A parent.", "value": "" },
            ],
            "token": { "field": "__RequestVerificationToken", "value": "form-token" },
        },
    })
}

#[test]
fn a_read_asks_for_the_page_as_json_with_the_cookies_and_shows_it_quoted() {
    let listener = Listener::start(|request| {
        let origin = origin_of(request);
        let topic = format!("{origin}/t/home/welcome");
        let post = |id: &str, text: &str| {
            json!({
                "id": id, "sequence": 1, "address": format!("{topic}/{id}"),
                "author": { "id": "prt_2", "handle": "alex.example.test", "name": "Alex", "kind": "person" },
                "body": text, "createdAt": "2026-09-27T00:00:00Z", "version": 1, "new": true,
            })
        };
        Reply::json(
            200,
            &json!({
                "address": topic, "title": "Welcome", "locked": false,
                "root": post("root1", "Hello"),
                "replies": [post("p1", "World")],
                "older": format!("{topic}?before=1"),
                "newer": null,
                "who": who(),
            }),
        )
    });
    let address =
        Address::parse(&listener.origin(), AddressPolicy::LoopbackAllowed).expect("address");
    let topic = format!("{address}/t/home/welcome");
    let said = perform(
        &address,
        "read_topic",
        json!({ "topic": topic, "limit": 5 }),
    )
    .expect("read");
    let request = &listener.requests()[0];
    assert_eq!(request.target, "/t/home/welcome");
    assert_eq!(request.header("accept"), Some("application/json"));
    assert_eq!(request.header("cookie"), Some(COOKIE));
    assert!(request.header("authorization").is_none());
    assert!(request.body.is_empty());
    assert!(said.contains("\"World\""), "{said}");
    assert!(said.contains(&format!("\"{topic}?before=1\"")), "{said}");
    assert!(!said.contains("secret-cookie"));
    assert!(
        said.contains("You cannot post here."),
        "no form, no post: {said}"
    );
}

#[test]
fn a_write_whose_answer_is_lost_is_unknown_and_a_lost_read_is_unavailable() {
    let topic = |address: &Address| format!("{address}/t/home/welcome");
    let address = losing_writes(form_page("reply"));
    assert_eq!(
        perform(
            &address,
            "post",
            json!({ "topic": topic(&address), "text": "Hello" })
        ),
        Err(Failed::Ended(Explained::new(
            Outcome::Unknown,
            wire::POST_UNKNOWN
        )))
    );

    let address = losing_writes(form_page("start"));
    let unknown = perform(
        &address,
        "start_topic",
        json!({ "tangent": format!("{address}/t/home"), "title": "A", "text": "B" }),
    );
    let Err(Failed::Ended(explained)) = unknown else {
        panic!("{unknown:?}");
    };
    assert_eq!(explained.outcome, Outcome::Unknown);
    assert!(explained.sentence.contains("started this Topic"));

    let address = losing_writes(form_page("edit"));
    let unknown = perform(
        &address,
        "edit_post",
        json!({ "post": format!("{}/p1", topic(&address)), "text": "New" }),
    );
    let Err(Failed::Ended(explained)) = unknown else {
        panic!("{unknown:?}");
    };
    assert_eq!(explained.outcome, Outcome::Unknown);
    assert!(explained.sentence.contains("edited this Post"));

    let address = losing();
    assert_eq!(
        perform(&address, "read_topic", json!({ "topic": topic(&address) })),
        Err(Failed::Ended(Explained::new(
            Outcome::TemporarilyUnavailable,
            wire::UNAVAILABLE
        )))
    );
}

#[test]
fn a_form_that_posts_elsewhere_or_lacks_a_field_is_never_followed() {
    for (name, broken) in [
        (
            "another origin",
            json!({ "action": "http://elsewhere.test/steal", "method": "POST", "fields": [
            { "name": "id", "holds": "" }, { "name": "body", "holds": "" }, { "name": "parent", "holds": "" }
        ], "token": { "value": "t" } }),
        ),
        (
            "another method",
            json!({ "action": "{origin}/t/home/welcome", "method": "PUT", "fields": [
            { "name": "id", "holds": "" }, { "name": "body", "holds": "" }, { "name": "parent", "holds": "" }
        ], "token": { "value": "t" } }),
        ),
        (
            "no token",
            json!({ "action": "{origin}/t/home/welcome", "method": "POST", "fields": [
            { "name": "id", "holds": "" }, { "name": "body", "holds": "" }, { "name": "parent", "holds": "" }
        ], "token": { "value": "" } }),
        ),
        (
            "no body field",
            json!({ "action": "{origin}/t/home/welcome", "method": "POST", "fields": [
            { "name": "id", "holds": "" }
        ], "token": { "value": "t" } }),
        ),
    ] {
        let listener = Listener::start({
            let broken = broken.clone();
            move |request| {
                let origin = origin_of(request);
                let page = json!({ "who": who(), "reply": broken }).to_string();
                Reply::json(
                    200,
                    &serde_json::from_str(&page.replace("{origin}", &origin)).expect("JSON"),
                )
            }
        });
        let address =
            Address::parse(&listener.origin(), AddressPolicy::LoopbackAllowed).expect("address");
        let refused = perform(
            &address,
            "post",
            json!({ "topic": format!("{address}/t/home/welcome"), "text": "Hello" }),
        );
        let Err(Failed::Ended(explained)) = refused else {
            panic!("{name}: {refused:?}");
        };
        assert_eq!(explained.outcome, Outcome::Unknown, "{name}");
        assert!(
            listener
                .requests()
                .iter()
                .all(|request| request.method == "GET"),
            "{name}: nothing was posted"
        );
    }
}

#[test]
fn a_write_answered_with_another_id_is_never_a_success() {
    let listener = Listener::start(|request| {
        let origin = origin_of(request);
        if request.method == "GET" {
            let page = form_page("reply").to_string().replace("{origin}", &origin);
            return Reply::json(200, &serde_json::from_str(&page).expect("JSON"));
        }
        Reply::json(
            201,
            &json!({ "id": "3zzzzzzzzzzzz", "address": format!("{origin}/t/home/welcome/3zzzzzzzzzzzz") }),
        )
    });
    let address =
        Address::parse(&listener.origin(), AddressPolicy::LoopbackAllowed).expect("address");
    let answered = perform(
        &address,
        "post",
        json!({ "topic": format!("{address}/t/home/welcome"), "text": "Hello" }),
    );
    assert_eq!(
        answered,
        Err(Failed::Ended(Explained::new(
            Outcome::Unknown,
            wire::POST_UNKNOWN
        )))
    );
}

#[test]
fn a_page_that_reads_as_a_visitors_or_a_401_is_the_space_not_knowing_the_cookies() {
    for (status, body) in [
        (200, json!({ "address": "x", "who": null, "signIn": "x" })),
        (200, json!({ "address": "x" })),
        (401, json!({ "status": 401, "gate": "sign-in" })),
    ] {
        let listener = Listener::start({
            let body = body.clone();
            move |_| Reply::json(status, &body)
        });
        let address =
            Address::parse(&listener.origin(), AddressPolicy::LoopbackAllowed).expect("address");
        assert_eq!(
            perform(&address, "tangents", json!({})),
            Err(Failed::Unauthenticated),
            "{status} {body}"
        );
    }
}

#[test]
fn what_is_new_names_each_person_as_the_door_does_and_a_door_of_another_participant_is_not_trusted()
{
    let listener = Listener::start(|request| {
        let origin = origin_of(request);
        Reply::json(
            200,
            &json!({
                "name": "A Space",
                "can": ["View", "Post"],
                "who": who(),
                "tangents": [],
                "moreTangents": true,
                "news": {
                    "first": false,
                    "replies": { "count": 7, "posts": [{
                        "who": { "id": "prt_2", "name": "Bea" },
                        "topic": "Welcome",
                        "address": format!("{origin}/t/home/welcome/p1"),
                        "line": "Hello there",
                    }] },
                    "perTangent": [{ "address": format!("{origin}/t/home"), "name": "Home", "topics": 1, "replies": 2 }],
                    "watched": [{ "address": format!("{origin}/t/home/welcome"), "name": "Welcome", "new": 3 }],
                    "caughtUp": { "action": origin, "method": "POST", "fields": [], "token": { "value": "t" } },
                },
            }),
        )
    });
    let address =
        Address::parse(&listener.origin(), AddressPolicy::LoopbackAllowed).expect("address");
    let outbound = Outbound::new(AddressPolicy::LoopbackAllowed);
    let result = tools::perform(
        tools::tool("arrive").expect("a tool"),
        &ToolArguments::default(),
        &Through::new(&outbound, &address, issued()),
    )
    .expect("arrived");
    let said = result.rendered();
    assert!(
        said.contains("A reply to you by \"Bea\" (participant \"prt_2\")"),
        "{said}"
    );
    assert!(said.contains("which begins \"Hello there\""), "{said}");
    assert!(said.contains("3 new Posts"), "{said}");
    assert!(said.contains("1 new Topic and 2 new replies"), "{said}");
    assert!(
        said.contains("There are more Tangents; tangent_tangents lists them."),
        "{said}"
    );
    let structured = result.structured.expect("structured");
    assert_eq!(structured["moreTangents"]["readWith"], "tangent_tangents");
    assert_eq!(
        structured["attention"]["items"][0]["quoted"]["actorName"],
        "Bea"
    );
    assert_eq!(structured["attention"]["catchUpWith"], "tangent_caught_up");
    assert_eq!(structured["may"], json!(["read", "post"]));

    let mut other = issued();
    other.participant = Some("prt_9".to_owned());
    let through = Through::new(&outbound, &address, other);
    let untrusted = tools::perform(
        tools::tool("arrive").expect("a tool"),
        &ToolArguments::default(),
        &through,
    );
    assert!(
        matches!(untrusted, Err(Failed::Ended(explained)) if explained.outcome == Outcome::TemporarilyUnavailable),
        "the door shows the participant the connection is"
    );
}

#[test]
fn a_page_of_posts_past_its_budget_shows_what_fits_and_continues_from_there() {
    let long = "\u{4e2d}".repeat(tools::POST_SHOWN);
    let listener = Listener::start(move |request| {
        let origin = origin_of(request);
        let topic = format!("{origin}/t/home/welcome");
        let post = |sequence: u64, text: &str| {
            json!({
                "sequence": sequence, "address": format!("{topic}/p{sequence}"),
                "author": { "id": "prt_2", "name": "Alex" },
                "body": text, "createdAt": "2026-09-27T00:00:00Z",
            })
        };
        let replies: Vec<Value> = (1..=25).map(|sequence| post(sequence, &long)).collect();
        Reply::json(
            200,
            &json!({
                "address": topic, "title": "Welcome",
                "root": post(0, "It opens."),
                "replies": replies,
                "newer": format!("{topic}?after=25"),
                "seen": { "action": format!("{topic}/seen"), "method": "POST", "fields": [], "token": { "value": "t" } },
                "who": who(),
            }),
        )
    });
    let address =
        Address::parse(&listener.origin(), AddressPolicy::LoopbackAllowed).expect("address");
    let outbound = Outbound::new(AddressPolicy::LoopbackAllowed);
    let topic = format!("{address}/t/home/welcome");
    let result = tools::perform(
        tools::tool("read_topic").expect("a tool"),
        &arguments(json!({ "topic": topic, "limit": 25 })),
        &Through::new(&outbound, &address, issued()),
    )
    .expect("read");
    let said = result.rendered();
    assert!(
        said.len() <= lobby_contracts::ToolResult::RENDERED_BUDGET,
        "{} bytes of text",
        said.len()
    );
    assert!(
        !said.contains("cut this result short"),
        "the page fits whole"
    );
    let structured = result.structured.expect("structured");
    let posts = structured["posts"].as_array().expect("posts");
    let last = posts.len() - 1;
    assert!(last > 0 && last < 25, "{last} replies");
    assert_eq!(
        structured["cursor"],
        format!("{topic}?after={last}"),
        "the next read starts at the first left out"
    );
    assert_eq!(structured["through"], last, "and marking read stops here");
    assert!(said.contains(&format!("through {last}")), "{said}");
}

struct Proving;

impl Authority for Proving {
    fn authorize(&self, _: &AuthorizationAsk) -> Result<Authorization, Explained> {
        Ok(Authorization {
            scheme: "Bearer".to_owned(),
            token: Secret::new("a-proof"),
            proof: None,
        })
    }
}

/// What establishing a connection comes to at a Space whose exchange answers
/// with `answer`.
fn establishing(answer: impl Fn() -> Reply + Send + Sync + 'static) -> Result<(), Explained> {
    let listener = Listener::start(move |request| {
        let origin = origin_of(request);
        match request.route() {
            "/.well-known/tangent-auth" => {
                let port = origin.rsplit(':').next().unwrap_or_default();
                Reply::json(
                    200,
                    &json!({
                        "canonicalOrigin": origin,
                        "serviceProof": {
                            "method": "org.sylin.tangent.exchangeIdentity",
                            "purpose": "org.sylin.tangent.exchangeIdentity",
                            "audience": format!("did:web:localhost%3A{port}#tangent"),
                            "algorithms": ["ES256"],
                            "transport": "authorization_header",
                            "endpoint": format!("{origin}/api/v1/identity/exchange"),
                        },
                    }),
                )
            }
            "/api/v1/identity/exchange" => answer(),
            _ => Reply::json(200, &json!({ "who": null })),
        }
    });
    let address =
        Address::parse(&listener.origin(), AddressPolicy::LoopbackAllowed).expect("address");
    Tangent::accepting(ProviderKind::new("atproto").expect("a kind"))
        .establish(&address, &Proving)
        .map(|_| ())
}

#[test]
fn only_204_and_a_cookie_that_signs_in_is_a_connection() {
    let broke = Explained::new(Outcome::TemporarilyUnavailable, tangent::EXCHANGE_BROKE);
    assert_eq!(
        establishing(|| Reply {
            status: 204,
            headers: Vec::new(),
            body: Vec::new()
        }),
        Err(broke),
        "no cookie"
    );
    assert_eq!(
        establishing(|| Reply::json(
            200,
            &json!({ "profile": "another", "credential": {}, "token": "x" })
        )),
        Err(broke),
        "any other success"
    );
    assert_eq!(
        establishing(|| Reply {
            status: 204,
            headers: vec![("Set-Cookie".to_owned(), "signed=in; path=/".to_owned())],
            body: Vec::new()
        }),
        Err(broke),
        "a cookie the door does not read as anyone"
    );
    assert_eq!(
        establishing(|| Reply::json(401, &json!({ "error": "invalid_service_proof" }))),
        Err(Explained::new(Outcome::Refused, tangent::PROOF_REFUSED))
    );
    assert_eq!(
        establishing(|| Reply::json(503, &json!({ "error": "exchange_unconfigured" }))),
        Err(Explained::new(Outcome::Refused, tangent::NOT_CONFIGURED))
    );
    assert_eq!(
        establishing(|| Reply::json(503, &json!({ "error": "exchange_unavailable" }))),
        Err(Explained::new(
            Outcome::TemporarilyUnavailable,
            tangent::ISSUER_UNREACHABLE
        ))
    );
    assert_eq!(
        establishing(|| Reply::json(500, &json!({}))),
        Err(Explained::new(
            Outcome::TemporarilyUnavailable,
            wire::UNAVAILABLE
        ))
    );
}

#[test]
fn every_write_is_said_to_be_safe_to_repeat_or_to_read_first() {
    const SAFE: Effect = Effect::Write { retry: Retry::Safe };
    const READ_FIRST: Effect = Effect::Write {
        retry: Retry::ReadFirst,
    };
    for name in [
        "post",
        "start_topic",
        "mark_read",
        "watch",
        "report_post",
        "caught_up",
    ] {
        assert_eq!(tools::tool(name).expect("a tool").effect, SAFE, "{name}");
    }
    for name in ["edit_post", "remove_post"] {
        assert_eq!(
            tools::tool(name).expect("a tool").effect,
            READ_FIRST,
            "{name}"
        );
    }
}
