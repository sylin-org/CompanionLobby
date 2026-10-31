//! A fake Tangent Space.
//!
//! The Space stands on its own loopback listener and speaks Tangent's wire
//! the way the real server does, as far as a companion's journey goes:
//!
//! * `GET /.well-known/tangent-auth` publishes the Space's authentication
//!   profile: its canonical origin and the service proof it takes (method and
//!   purpose `org.sylin.tangent.exchangeIdentity`, the audience Tangent
//!   derives from a loopback origin, `did:web:localhost%3A<port>#tangent`,
//!   ES256 or ES256K, in the `Authorization` header, at the exchange
//!   endpoint);
//! * `POST /api/v1/identity/exchange` takes a proof the atproto world's PDS
//!   minted, presented once as `Bearer` with no body: it must be for this
//!   audience and method and not expired, and it is claimed, so a proof is
//!   never taken twice. It answers `204` and a cookie that signs the account
//!   in;
//! * every page answers JSON when asked: the door `/`, the Tangents `/t`, a
//!   Tangent `/t/<tangent>`, a Topic `/t/<tangent>/<topic>` with one window of
//!   its replies, a Post's address, a Post's `edit` and `report` pages, a new
//!   Topic's page `/t/<tangent>/new` and a participant `/p/<id or handle>`.
//!   Each says who reads it in `who`, which is absent to a visitor, whom a
//!   private Space answers 401 with the `sign-in` gate;
//! * every write a page offers is a form in its JSON, saying where it posts,
//!   its fields and its token, and is absent where the reader may not write,
//!   the page's `gate` saying why. A page that carries a form sets the
//!   antiforgery cookie when the reader has none, and its token is good only
//!   with that cookie and that reader, in the `RequestVerificationToken`
//!   header beside one JSON object;
//! * a new Post's `id` is a TID of the sender's: the same one with the same
//!   words answers the same Post, with other words answers 409, and in another
//!   Topic or from another sender answers 400;
//! * a refusal is a problem, `{status, title, detail, gate}`;
//! * `POST /auth/logout` signs out.
//!
//! Knobs change the profile it publishes, refuse proofs, fail exchanges for
//! a while, ban an account, end every sign-in it made, hold every reader as an
//! applicant, lock a Topic, refuse the forms a Topic offered, and close the
//! Space to visitors. The listener records every request and can hold a
//! route, or its answer, mid-flight.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde_json::{Map, Value, json};

use crate::atproto::Proofs;
use crate::listener::{Listener, Received, Reply};
use crate::{percent_decode, unique_token};

/// The well-known path of the authentication profile.
pub const PROFILE_PATH: &str = "/.well-known/tangent-auth";
/// The exchange endpoint's path.
pub const EXCHANGE_PATH: &str = "/api/v1/identity/exchange";
/// The method and purpose of the proof the Space takes.
pub const EXCHANGE_METHOD: &str = "org.sylin.tangent.exchangeIdentity";
/// The header a form's token travels in.
pub const TOKEN_HEADER: &str = "requestverificationtoken";
/// What the Space calls itself.
pub const PLACE_LABEL: &str = "Lobby Test Space";

const SESSION_COOKIE: &str = ".AspNetCore.Koan.cookie";
const ANTIFORGERY_COOKIE: &str = ".AspNetCore.Antiforgery.lobbytest";
const WINDOW_MOST: usize = 25;
const TOPICS_MOST: usize = 30;
const DOOR_TANGENTS_MOST: usize = 8;
const ITEMS_MOST: usize = 5;

/// What the authentication profile says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// The profile the Space really has.
    Ready,
    /// 503 `exchange_unconfigured`: the Space has no address of its own.
    Unconfigured,
    /// A method other than the exchange's.
    WrongMethod,
    /// An audience of another service.
    TamperedAudience,
    /// A canonical origin other than the Space's address.
    AnotherOrigin,
    /// An exchange endpoint on another origin.
    AnotherEndpoint,
}

#[derive(Debug, Clone)]
struct SignedIn {
    cookie: String,
    did: String,
    ended: bool,
}

#[derive(Debug, Clone)]
struct Participant {
    reference: String,
    handle: String,
    display_name: String,
}

#[derive(Debug, Clone)]
struct Tangent {
    key: String,
    name: String,
    description: String,
}

#[derive(Debug, Clone)]
struct Topic {
    key: String,
    tangent: String,
    title: String,
    root: String,
    locked: bool,
    refusing: bool,
    last_sequence: u64,
}

/// One Post on the fake Space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Post {
    /// Its id, the last segment of its address.
    pub id: String,
    /// The key of its Topic.
    pub topic: String,
    /// Its author's participant reference.
    pub author: String,
    /// What it says.
    pub text: String,
    /// The id of the Post it replies to, when that is not the Topic.
    pub reply_to: Option<String>,
    /// Whether it was removed.
    pub removed: bool,
    /// Whether it was edited.
    pub edited: bool,
    root: bool,
    sequence: u64,
    version: u64,
    log: u64,
}

#[derive(Debug, Default)]
struct Floor {
    origin: String,
    profile: Option<Profile>,
    proofs: Proofs,
    claimed: HashSet<String>,
    rejecting_proofs: bool,
    failing_exchanges: bool,
    banned: HashSet<String>,
    applicants: bool,
    private: bool,
    signed_in: Vec<SignedIn>,
    antiforgery: HashMap<String, String>,
    participants: Vec<Participant>,
    tangents: Vec<Tangent>,
    topics: Vec<Topic>,
    posts: Vec<Post>,
    topic_seen: HashMap<(String, String), i64>,
    door_seen: HashMap<String, u64>,
    watches: HashSet<(String, String)>,
    reports: Vec<(String, String, String)>,
    exchanges: Vec<Vec<u8>>,
    log: u64,
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The seconds since the Unix epoch.
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// `secs` since the Unix epoch as an RFC 3339 time in UTC.
pub fn rfc3339(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rest = secs.rem_euclid(86_400);
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// The audience Tangent derives for a loopback origin.
pub fn loopback_audience(origin: &str) -> String {
    let port = origin.rsplit(':').next().unwrap_or("0");
    format!("did:web:localhost%3A{port}#tangent")
}

/// A fake Space on its own loopback listener, trusting the proofs one fake
/// atproto world's PDS mints.
pub struct SpaceServer {
    listener: Listener,
    floor: Arc<Mutex<Floor>>,
}

/// A problem in the shape the Space answers.
fn problem(status: u16, title: &str, detail: &str, gate: Option<&str>) -> Reply {
    let mut body = json!({ "status": status, "title": title, "detail": detail });
    if let Some(gate) = gate {
        body["gate"] = json!(gate);
    }
    let mut reply = Reply::json(status, &body);
    reply.headers[0].1 = "application/problem+json; charset=utf-8".to_owned();
    reply
}

/// What an address with nothing at it answers.
fn absent() -> Reply {
    problem(
        404,
        "Not found",
        "Nothing answers at this address.",
        Some("absent"),
    )
}

/// A gate's problem: 401 to sign in, 403 for any other.
fn gated(gate: &str) -> Reply {
    problem(
        if gate == "sign-in" { 401 } else { 403 },
        "Not open to you",
        "This isn't open to you here.",
        Some(gate),
    )
}

/// An identity-exchange error in the shape the exchange answers.
fn exchange_error(status: u16, code: &str) -> Reply {
    Reply::json(status, &json!({ "error": code }))
}

/// Percent-encodes what a path segment may not hold as it is.
fn segment(text: &str) -> String {
    text.bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
                char::from(byte).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

/// Whether `id` has a TID's shape.
fn tid_shaped(id: &str) -> bool {
    const ALPHABET: &str = "234567abcdefghijklmnopqrstuvwxyz";
    id.len() == 13
        && id.chars().all(|character| ALPHABET.contains(character))
        && ALPHABET
            .find(id.chars().next().unwrap_or(' '))
            .is_some_and(|first| first <= 15)
}

impl SpaceServer {
    /// Starts an empty Space that takes the proofs `proofs` holds.
    pub fn start(proofs: &Proofs) -> Self {
        let floor = Arc::new(Mutex::new(Floor {
            proofs: proofs.clone(),
            ..Floor::default()
        }));
        let answering = floor.clone();
        let listener = Listener::start(move |request| answer(&answering, request));
        locked(&floor).origin = listener.origin();
        Self { listener, floor }
    }

    /// The Space's origin, which the sponsor names as the connection's address.
    pub fn origin(&self) -> String {
        self.listener.origin()
    }

    /// The listener, to count what reached the Space and to hold requests.
    pub fn listener(&self) -> &Listener {
        &self.listener
    }

    /// The address of the Tangent `key`.
    pub fn tangent_ref(&self, key: &str) -> String {
        format!("{}/t/{key}", self.origin())
    }

    /// The route of the Topic `key`, as requests to it are recorded.
    pub fn topic_route(&self, key: &str) -> String {
        topic_route(&locked(&self.floor), key)
    }

    /// The address of the Topic `key`.
    pub fn topic_ref(&self, key: &str) -> String {
        format!("{}{}", self.origin(), self.topic_route(key))
    }

    /// The address of the Post `id`.
    pub fn post_ref(&self, id: &str) -> String {
        let floor = locked(&self.floor);
        floor
            .posts
            .iter()
            .find(|post| post.id == id)
            .map(|post| post_ref(&floor, post))
            .unwrap_or_default()
    }

    /// Adds a Tangent.
    pub fn add_tangent(&self, key: &str, name: &str, description: &str) {
        locked(&self.floor).tangents.push(Tangent {
            key: key.to_owned(),
            name: name.to_owned(),
            description: description.to_owned(),
        });
    }

    /// Adds a Topic to the Tangent `tangent`, its root by a steward of the
    /// Space saying `brief`, and answers the root's id.
    pub fn add_topic(&self, tangent: &str, key: &str, title: &str, brief: &str) -> String {
        let mut floor = locked(&self.floor);
        let steward = "did:plc:stewardstewardstewardstew".to_owned();
        if !floor
            .participants
            .iter()
            .any(|known| known.reference == steward)
        {
            floor.participants.push(Participant {
                reference: steward.clone(),
                handle: "steward.example.test".to_owned(),
                display_name: "Steward".to_owned(),
            });
        }
        floor.log += 1;
        let log = floor.log;
        let id = format!("root{log}");
        floor.posts.push(Post {
            id: id.clone(),
            topic: key.to_owned(),
            author: steward,
            text: brief.to_owned(),
            reply_to: None,
            removed: false,
            edited: false,
            root: true,
            sequence: 0,
            version: 1,
            log,
        });
        floor.topics.push(Topic {
            key: key.to_owned(),
            tangent: tangent.to_owned(),
            title: title.to_owned(),
            root: id.clone(),
            locked: false,
            refusing: false,
            last_sequence: 0,
        });
        id
    }

    /// Adds a participant who is no companion, known by `display_name`,
    /// and answers its reference.
    pub fn add_participant(&self, display_name: &str, handle: &str) -> String {
        let mut floor = locked(&self.floor);
        let reference = format!("did:plc:participant{}", floor.participants.len() + 1);
        floor.participants.push(Participant {
            reference: reference.clone(),
            handle: handle.to_owned(),
            display_name: display_name.to_owned(),
        });
        reference
    }

    /// Names the participant the account `did` becomes by `display_name` and
    /// `handle`.
    pub fn name_account(&self, did: &str, display_name: &str, handle: &str) {
        let mut floor = locked(&self.floor);
        participant_of(&mut floor, did);
        if let Some(found) = floor
            .participants
            .iter_mut()
            .find(|known| known.reference == did)
        {
            found.display_name = display_name.to_owned();
            found.handle = handle.to_owned();
        }
    }

    /// Replies with `text` in the Topic `topic` as the participant `author`,
    /// to the Post `reply_to` when it names one, and answers the new Post's
    /// id.
    pub fn add_post(
        &self,
        topic: &str,
        author: &str,
        text: &str,
        reply_to: Option<&str>,
    ) -> String {
        let mut floor = locked(&self.floor);
        let id = format!("p{}", floor.log + 1);
        reply(&mut floor, &id, topic, author, text, reply_to);
        id
    }

    /// Every reply, oldest first; a Topic's root is not among them.
    pub fn posts(&self) -> Vec<Post> {
        locked(&self.floor)
            .posts
            .iter()
            .filter(|post| !post.root)
            .cloned()
            .collect()
    }

    /// The participant reference the account `did` became, when it has signed
    /// in or been named.
    pub fn participant_of(&self, did: &str) -> Option<String> {
        locked(&self.floor)
            .participants
            .iter()
            .find(|participant| participant.reference == did)
            .map(|participant| participant.reference.clone())
    }

    /// The reference of the participant the Space shows as `display_name`.
    pub fn participant_named(&self, display_name: &str) -> Option<String> {
        locked(&self.floor)
            .participants
            .iter()
            .find(|participant| participant.display_name == display_name)
            .map(|participant| participant.reference.clone())
    }

    /// Makes the profile say `profile` from now on.
    pub fn publish(&self, profile: Profile) {
        locked(&self.floor).profile = Some(profile);
    }

    /// Refuses every proof from now on, as `invalid_service_proof`.
    pub fn reject_proofs(&self, rejecting: bool) {
        locked(&self.floor).rejecting_proofs = rejecting;
    }

    /// Answers every exchange 500 while `failing`, signing nobody in, as a
    /// Space that cannot answer just now.
    pub fn fail_exchanges(&self, failing: bool) {
        locked(&self.floor).failing_exchanges = failing;
    }

    /// Bans the account `did`: it still signs in, and finds the Space absent
    /// at every address, in the sign-in it holds and in a new one.
    pub fn ban(&self, did: &str) {
        locked(&self.floor).banned.insert(did.to_owned());
    }

    /// Ends every sign-in the Space made so far, as a Space that restarted
    /// with a new key would: their cookies read as a visitor's.
    pub fn end_sign_ins(&self) {
        for signed_in in &mut locked(&self.floor).signed_in {
            signed_in.ended = true;
        }
    }

    /// Ends the one sign-in the cookie value `cookie` holds, as signing out at
    /// the address a page's `who.signOut` names does.
    pub fn end_sign_in(&self, cookie: &str) {
        for signed_in in &mut locked(&self.floor).signed_in {
            if signed_in.cookie == cookie {
                signed_in.ended = true;
            }
        }
    }

    /// Holds every reader as an applicant from now on: they read and write
    /// nothing until admitted.
    pub fn hold_as_applicants(&self, holding: bool) {
        locked(&self.floor).applicants = holding;
    }

    /// Closes the Space to visitors from now on: every page asks them to sign
    /// in.
    pub fn close_to_visitors(&self) {
        locked(&self.floor).private = true;
    }

    /// Locks the Topic `topic`: its page offers no reply, and says why.
    pub fn lock(&self, topic: &str) {
        for found in &mut locked(&self.floor).topics {
            if found.key == topic {
                found.locked = true;
            }
        }
    }

    /// Refuses replies in the Topic `topic` with a 403 and no reason, though
    /// its page still offers the form, as a rule the page does not know of.
    pub fn refuse_replies_in(&self, topic: &str) {
        for found in &mut locked(&self.floor).topics {
            if found.key == topic {
                found.refusing = true;
            }
        }
    }

    /// The body of every exchange the Space took up, oldest first.
    pub fn exchanges(&self) -> Vec<Vec<u8>> {
        locked(&self.floor).exchanges.clone()
    }

    /// Every cookie value the Space issued, for tests that look for secrets
    /// where none may be.
    pub fn cookies(&self) -> Vec<String> {
        let floor = locked(&self.floor);
        floor
            .signed_in
            .iter()
            .map(|signed_in| signed_in.cookie.clone())
            .chain(floor.antiforgery.keys().cloned())
            .collect()
    }

    /// Who wrote the reports the Space took, as (Post, reporter, reason).
    pub fn reports(&self) -> Vec<(String, String, String)> {
        locked(&self.floor).reports.clone()
    }

    /// Whom the cookie the request `request` carried signs in.
    pub fn signed_in_as(&self, request: &Received) -> Option<String> {
        let floor = locked(&self.floor);
        reader_of(&floor, request)
    }
}

fn topic_route(floor: &Floor, key: &str) -> String {
    let tangent = floor
        .topics
        .iter()
        .find(|topic| topic.key == key)
        .map(|topic| topic.tangent.clone())
        .unwrap_or_default();
    format!("/t/{tangent}/{key}")
}

fn post_ref(floor: &Floor, post: &Post) -> String {
    format!(
        "{}{}/{}",
        floor.origin,
        topic_route(floor, &post.topic),
        post.id
    )
}

/// Makes the account `did` a participant on its first visit.
fn participant_of(floor: &mut Floor, did: &str) {
    if floor
        .participants
        .iter()
        .any(|known| known.reference == did)
    {
        return;
    }
    floor.participants.push(Participant {
        reference: did.to_owned(),
        handle: did.to_owned(),
        display_name: did.to_owned(),
    });
}

/// Adds a reply and answers it.
fn reply(
    floor: &mut Floor,
    id: &str,
    topic: &str,
    author: &str,
    text: &str,
    reply_to: Option<&str>,
) -> Post {
    floor.log += 1;
    let log = floor.log;
    let found = floor.topics.iter_mut().find(|known| known.key == topic);
    let sequence = found.map_or(1, |found| {
        found.last_sequence += 1;
        found.last_sequence
    });
    let post = Post {
        id: id.to_owned(),
        topic: topic.to_owned(),
        author: author.to_owned(),
        text: text.to_owned(),
        reply_to: reply_to.map(str::to_owned),
        removed: false,
        edited: false,
        root: false,
        sequence,
        version: 1,
        log,
    };
    floor.posts.push(post.clone());
    post
}

fn cookie_of(request: &Received, name: &str) -> Option<String> {
    request
        .header("cookie")?
        .split(';')
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value.to_owned())
}

/// Whom `request`'s cookie signs in, while the Space knows it.
fn reader_of(floor: &Floor, request: &Received) -> Option<String> {
    let cookie = cookie_of(request, SESSION_COOKIE)?;
    floor
        .signed_in
        .iter()
        .find(|signed_in| signed_in.cookie == cookie && !signed_in.ended)
        .map(|signed_in| signed_in.did.clone())
}

fn answer(floor: &Mutex<Floor>, request: &Received) -> Reply {
    let mut floor = locked(floor);
    let route = request.route().to_owned();
    match (request.method.as_str(), route.as_str()) {
        ("GET", PROFILE_PATH) => profile(&floor),
        ("POST", EXCHANGE_PATH) => exchange(&mut floor, request),
        ("POST", "/auth/logout") => {
            if let Some(cookie) = cookie_of(request, SESSION_COOKIE) {
                for signed_in in &mut floor.signed_in {
                    if signed_in.cookie == cookie {
                        signed_in.ended = true;
                    }
                }
            }
            Reply::redirect("/").with_header(
                "Set-Cookie",
                &format!("{SESSION_COOKIE}=; expires=Thu, 01 Jan 1970 00:00:00 GMT; path=/"),
            )
        }
        _ => pages(&mut floor, request, &route),
    }
}

fn profile(floor: &Floor) -> Reply {
    let origin = floor.origin.clone();
    let mut audience = loopback_audience(&origin);
    let mut method = EXCHANGE_METHOD.to_owned();
    let mut canonical = origin.clone();
    let mut endpoint = format!("{origin}{EXCHANGE_PATH}");
    match floor.profile.unwrap_or(Profile::Ready) {
        Profile::Ready => {}
        Profile::Unconfigured => return exchange_error(503, "exchange_unconfigured"),
        Profile::WrongMethod => method = "com.atproto.simplespace.checkUserAccess".to_owned(),
        Profile::TamperedAudience => audience = "did:web:victim.example.test#tangent".to_owned(),
        Profile::AnotherOrigin => canonical = "http://127.0.0.1:1".to_owned(),
        Profile::AnotherEndpoint => endpoint = format!("http://127.0.0.1:1{EXCHANGE_PATH}"),
    }
    Reply::json(
        200,
        &json!({
            "canonicalOrigin": canonical,
            "serviceProof": {
                "method": method,
                "purpose": EXCHANGE_METHOD,
                "audience": audience,
                "algorithms": ["ES256", "ES256K"],
                "transport": "authorization_header",
                "endpoint": endpoint,
            },
        }),
    )
}

fn exchange(floor: &mut Floor, request: &Received) -> Reply {
    if floor.failing_exchanges {
        return exchange_error(500, "internal_error");
    }
    if floor.profile == Some(Profile::Unconfigured) {
        return exchange_error(503, "exchange_unconfigured");
    }
    let presented = request.header("authorization").unwrap_or_default();
    let Some(token) = presented
        .strip_prefix("Bearer ")
        .filter(|token| !token.is_empty())
    else {
        return exchange_error(401, "service_proof_required");
    };
    let Some(proof) = floor.proofs.verify(token) else {
        return exchange_error(401, "invalid_service_proof");
    };
    if floor.rejecting_proofs
        || proof.audience != loopback_audience(&floor.origin)
        || proof.method != EXCHANGE_METHOD
        || proof.expires_at <= now_secs()
    {
        return exchange_error(401, "invalid_service_proof");
    }
    if !floor
        .claimed
        .insert(format!("{}\n{}", proof.issuer, proof.id))
    {
        return exchange_error(401, "invalid_service_proof");
    }
    floor.exchanges.push(request.body.clone());
    participant_of(floor, &proof.issuer);
    let cookie = format!("ts_{}", unique_token("t").replace('-', ""));
    floor.signed_in.push(SignedIn {
        cookie: cookie.clone(),
        did: proof.issuer.clone(),
        ended: false,
    });
    Reply {
        status: 204,
        headers: vec![(
            "Set-Cookie".to_owned(),
            format!("{SESSION_COOKIE}={cookie}; path=/; samesite=lax; httponly"),
        )],
        body: Vec::new(),
    }
}

/// One request for a page: whom it reads as, and the antiforgery cookie it
/// came with or was given.
struct Visit<'a> {
    request: &'a Received,
    reader: Option<String>,
    antiforgery: Option<String>,
    minted: bool,
}

impl Visit<'_> {
    /// The value of the token a form on this page carries; none for a visitor.
    fn token(&mut self, floor: &mut Floor) -> Option<String> {
        let reader = self.reader.clone()?;
        if self.antiforgery.is_none() {
            let cookie = format!("af_{}", unique_token("a").replace('-', ""));
            floor.antiforgery.insert(cookie.clone(), reader);
            self.antiforgery = Some(cookie);
            self.minted = true;
        }
        self.antiforgery
            .as_ref()
            .map(|cookie| format!("rv_{cookie}"))
    }

    /// Whether this write carried a token good for its reader.
    fn token_holds(&self, floor: &Floor) -> bool {
        let (Some(reader), Some(cookie)) = (&self.reader, &self.antiforgery) else {
            return false;
        };
        floor.antiforgery.get(cookie) == Some(reader)
            && self.request.header(TOKEN_HEADER) == Some(format!("rv_{cookie}").as_str())
    }

    /// `reply`, setting the antiforgery cookie when this page minted it.
    fn finish(&self, reply: Reply) -> Reply {
        match (&self.antiforgery, self.minted) {
            (Some(cookie), true) => reply.with_header(
                "Set-Cookie",
                &format!("{ANTIFORGERY_COOKIE}={cookie}; path=/; samesite=strict; httponly"),
            ),
            _ => reply,
        }
    }
}

/// A form a page carries.
fn form(action: &str, fields: Value, token: &str) -> Value {
    json!({
        "action": action,
        "method": "POST",
        "fields": fields,
        "token": { "field": "__RequestVerificationToken", "value": token },
    })
}

fn field(name: &str, value: Option<&str>) -> Value {
    json!({ "name": name, "holds": format!("What {name} holds."), "value": value })
}

fn pages(floor: &mut Floor, request: &Received, route: &str) -> Reply {
    let reader = reader_of(floor, request);
    let antiforgery = cookie_of(request, ANTIFORGERY_COOKIE)
        .filter(|cookie| floor.antiforgery.contains_key(cookie));
    let mut visit = Visit {
        request,
        reader,
        antiforgery,
        minted: false,
    };
    if visit
        .reader
        .as_ref()
        .is_some_and(|reader| floor.banned.contains(reader))
    {
        return absent();
    }
    if visit.reader.is_none() && floor.private {
        return gated("sign-in");
    }
    let segments: Vec<String> = route
        .trim_matches('/')
        .split('/')
        .filter(|part| !part.is_empty())
        .map(percent_decode)
        .collect();
    let segments: Vec<&str> = segments.iter().map(String::as_str).collect();
    let reply = match (request.method.as_str(), segments.as_slice()) {
        ("GET", []) => door(floor, &mut visit),
        ("POST", []) => mark_door_seen(floor, &visit),
        ("GET", ["t"]) => tangents_page(floor, &visit),
        ("GET", ["t", tangent]) => tangent_page(floor, &mut visit, tangent),
        ("GET", ["t", tangent, "new"]) => new_topic_page(floor, &mut visit, tangent),
        ("POST", ["t", tangent, "new"]) => start_topic(floor, &visit, tangent),
        ("POST", ["t", tangent, "watch"]) => set_watch(floor, &visit, tangent, None),
        ("GET", ["t", tangent, topic]) => topic_page(floor, &mut visit, tangent, topic, None),
        ("POST", ["t", tangent, topic]) => post_reply(floor, &visit, tangent, topic),
        ("POST", ["t", tangent, topic, "seen"]) => mark_seen(floor, &visit, tangent, topic),
        ("POST", ["t", tangent, topic, "watch"]) => set_watch(floor, &visit, tangent, Some(topic)),
        ("GET", ["t", tangent, topic, post]) => {
            topic_page(floor, &mut visit, tangent, topic, Some(post))
        }
        ("GET", ["t", tangent, topic, post, "edit"]) => {
            edit_page(floor, &mut visit, tangent, topic, post)
        }
        ("POST", ["t", tangent, topic, post, "edit"]) => {
            edit_post(floor, &visit, tangent, topic, post)
        }
        ("POST", ["t", tangent, topic, post, "remove"]) => {
            remove_post(floor, &visit, tangent, topic, post)
        }
        ("GET", ["t", tangent, topic, post, "report"]) => {
            report_page(floor, &mut visit, tangent, topic, post, false)
        }
        ("POST", ["t", tangent, topic, post, "report"]) => {
            report_post(floor, &mut visit, tangent, topic, post)
        }
        ("GET", ["p", someone]) => participant_page(floor, &visit, someone),
        _ => absent(),
    };
    visit.finish(reply)
}

/// What the reader's standing lets them do in the Space, and why not.
fn can_and_gate(floor: &Floor, visit: &Visit<'_>) -> (Vec<&'static str>, Option<&'static str>) {
    match &visit.reader {
        None => (vec!["View"], Some("sign-in")),
        Some(_) if floor.applicants => (vec!["View"], Some("apply")),
        Some(_) => (vec!["View", "Post", "Topic"], None),
    }
}

/// A participant as a page shows them.
fn author_json(floor: &Floor, reference: &str) -> Value {
    let found = floor
        .participants
        .iter()
        .find(|participant| participant.reference == reference);
    let (name, handle) = found.map_or((reference, reference), |found| {
        (found.display_name.as_str(), found.handle.as_str())
    });
    json!({
        "id": reference,
        "handle": handle,
        "name": name,
        "kind": "person",
        "profile": format!("{}/p/{}", floor.origin, segment(handle)),
    })
}

/// Who reads the page at `route`: absent for a visitor.
fn who_json(floor: &Floor, visit: &Visit<'_>, route: &str) -> Value {
    let Some(reader) = &visit.reader else {
        return Value::Null;
    };
    let mut who = author_json(floor, reader);
    who["signOut"] = json!(format!(
        "{}/auth/logout?return={}",
        floor.origin,
        segment(route)
    ));
    who
}

fn door(floor: &mut Floor, visit: &mut Visit<'_>) -> Reply {
    let (can, gate) = can_and_gate(floor, visit);
    if visit.reader.is_some() && floor.applicants && floor.private {
        return gated("apply");
    }
    let through = floor.log;
    let mut body = json!({
        "name": PLACE_LABEL,
        "address": format!("{}/", floor.origin),
        "can": can,
        "gate": gate,
        "tangents": [],
        "moreTangents": floor.tangents.len() > DOOR_TANGENTS_MOST,
        "featured": [],
        "alive": [],
        "day": "2026-10-31",
        "poll": { "through": through, "next": format!("{}/events?since={through}", floor.origin) },
        "api": 1,
        "who": who_json(floor, visit, "/"),
    });
    let mut new_in: HashMap<String, u64> = HashMap::new();
    if let Some(reader) = visit.reader.clone() {
        let (news, per_tangent) = door_news(floor, visit, &reader, through);
        body["news"] = news;
        new_in = per_tangent;
    }
    body["tangents"] = floor
        .tangents
        .iter()
        .take(DOOR_TANGENTS_MOST)
        .map(|tangent| {
            let mut listed = tangent_listed(floor, tangent);
            listed["new"] = json!(new_in.get(&tangent.key).copied().unwrap_or(0));
            listed
        })
        .collect();
    Reply::json(200, &body)
}

/// What is new to `reader` past their door's checkpoint, and how many Posts
/// are new in each Tangent. Their first door counts nothing as new, and sets
/// the checkpoint.
fn door_news(
    floor: &mut Floor,
    visit: &mut Visit<'_>,
    reader: &str,
    through: u64,
) -> (Value, HashMap<String, u64>) {
    let empty = json!({ "count": 0, "atLeast": false, "posts": [] });
    let Some(since) = floor.door_seen.get(reader).copied() else {
        floor.door_seen.insert(reader.to_owned(), through);
        return (
            json!({
                "from": 0, "through": through, "first": true, "quiet": true, "watched": [],
                "replies": empty, "mentions": empty, "perTangent": [], "articles": empty,
            }),
            HashMap::new(),
        );
    };
    let new: Vec<Post> = floor
        .posts
        .iter()
        .filter(|post| post.log > since && post.author != reader && !post.removed)
        .cloned()
        .collect();
    let tangent_of = |floor: &Floor, topic: &str| {
        floor
            .topics
            .iter()
            .find(|known| known.key == topic)
            .map(|known| known.tangent.clone())
            .unwrap_or_default()
    };
    let replies: Vec<&Post> = new
        .iter()
        .rev()
        .filter(|post| !post.root && answers(floor, post) == reader)
        .collect();
    let items: Vec<Value> = replies
        .iter()
        .take(ITEMS_MOST)
        .map(|post| {
            let topic = floor.topics.iter().find(|known| known.key == post.topic);
            json!({
                "postId": post.id,
                "who": author_json(floor, &post.author),
                "topic": topic.map(|topic| topic.title.clone()),
                "tangent": topic.map(|topic| topic.tangent.clone()),
                "address": post_ref(floor, post),
                "line": post.text.chars().take(180).collect::<String>(),
                "at": rfc3339(1_790_000_000 + post.log as i64 * 60),
            })
        })
        .collect();
    let mut per_tangent: Vec<(String, u64, u64)> = Vec::new();
    let mut new_in: HashMap<String, u64> = HashMap::new();
    for post in &new {
        let tangent = tangent_of(floor, &post.topic);
        *new_in.entry(tangent.clone()).or_default() += 1;
        if !per_tangent.iter().any(|(key, _, _)| *key == tangent) {
            per_tangent.push((tangent.clone(), 0, 0));
        }
        if let Some(counts) = per_tangent.iter_mut().find(|(key, _, _)| *key == tangent) {
            if post.root {
                counts.1 += 1;
            } else {
                counts.2 += 1;
            }
        }
    }
    let per_tangent: Vec<Value> = per_tangent
        .iter()
        .map(|(key, topics, replies)| {
            let name = floor
                .tangents
                .iter()
                .find(|tangent| tangent.key == *key)
                .map(|tangent| tangent.name.clone());
            json!({
                "slug": key, "name": name, "address": format!("{}/t/{key}", floor.origin),
                "topics": topics, "replies": replies, "atLeast": false,
            })
        })
        .collect();
    let mut watched = Vec::new();
    for topic in &floor.topics {
        if floor
            .watches
            .contains(&(reader.to_owned(), topic.key.clone()))
        {
            let count = new.iter().filter(|post| post.topic == topic.key).count();
            if count > 0 {
                watched.push(json!({
                    "scope": "topic", "name": topic.title, "tangent": topic.tangent,
                    "address": format!("{}{}", floor.origin, topic_route(floor, &topic.key)),
                    "new": count, "atLeast": false,
                }));
            }
        }
    }
    for tangent in &floor.tangents {
        if floor
            .watches
            .contains(&(reader.to_owned(), tangent.key.clone()))
        {
            let count = new
                .iter()
                .filter(|post| tangent_of(floor, &post.topic) == tangent.key)
                .count();
            if count > 0 {
                watched.push(json!({
                    "scope": "tangent", "name": tangent.name, "tangent": tangent.key,
                    "address": format!("{}/t/{}", floor.origin, tangent.key),
                    "new": count, "atLeast": false,
                }));
            }
        }
    }
    let quiet = new.is_empty();
    let mut news = json!({
        "from": since,
        "through": through,
        "watched": watched,
        "replies": { "count": replies.len(), "atLeast": false, "posts": items },
        "mentions": empty,
        "perTangent": per_tangent,
        "articles": empty,
        "first": false,
        "quiet": quiet,
    });
    if !quiet && let Some(token) = visit.token(floor) {
        news["caughtUp"] = form(
            &format!("{}/", floor.origin),
            json!([field("through", Some(&through.to_string()))]),
            &token,
        );
    }
    (news, new_in)
}

/// Whose Post `post` answers: the Post it replies to, or its Topic's root.
fn answers<'f>(floor: &'f Floor, post: &Post) -> &'f str {
    let parent = post.reply_to.clone().or_else(|| {
        floor
            .topics
            .iter()
            .find(|topic| topic.key == post.topic)
            .map(|topic| topic.root.clone())
    });
    floor
        .posts
        .iter()
        .find(|candidate| Some(&candidate.id) == parent.as_ref())
        .map_or("", |parent| parent.author.as_str())
}

/// The JSON object a write sent.
fn fields_of(request: &Received) -> Option<Map<String, Value>> {
    request.json().as_object().cloned()
}

fn unreadable() -> Reply {
    problem(
        400,
        "Not a form or a JSON object",
        "A write sends its fields as a form, or as one JSON object.",
        None,
    )
}

/// What a write answers before anything else is judged, when it is not one
/// to judge: a visitor is asked to sign in, and a token that does not hold
/// writes nothing.
fn refused_at_once(floor: &Floor, visit: &Visit<'_>) -> Option<Reply> {
    if visit.reader.is_none() {
        return Some(gated("sign-in"));
    }
    (!visit.token_holds(floor)).then(|| {
        problem(
            400,
            "Open the form again",
            "This form's token is missing or has expired, so nothing was written.",
            None,
        )
    })
}

fn mark_door_seen(floor: &mut Floor, visit: &Visit<'_>) -> Reply {
    if let Some(refused) = refused_at_once(floor, visit) {
        return refused;
    }
    let Some(through) = fields_of(visit.request)
        .and_then(|fields| fields.get("through")?.as_str()?.parse::<u64>().ok())
    else {
        return problem(400, "Not a sequence of the log", "Name one.", None);
    };
    let reader = visit.reader.clone().unwrap_or_default();
    let moved = floor.door_seen.entry(reader).or_default();
    *moved = (*moved).max(through);
    Reply {
        status: 303,
        headers: vec![("Location".to_owned(), "/".to_owned())],
        body: Vec::new(),
    }
}

fn tangent_listed(floor: &Floor, tangent: &Tangent) -> Value {
    json!({
        "slug": tangent.key,
        "name": tangent.name,
        "kind": "topic",
        "address": format!("{}/t/{}", floor.origin, tangent.key),
        "topicCount": floor.topics.iter().filter(|topic| topic.tangent == tangent.key).count(),
    })
}

fn tangents_page(floor: &Floor, visit: &Visit<'_>) -> Reply {
    let (can, _) = can_and_gate(floor, visit);
    Reply::json(
        200,
        &json!({
            "tangents": floor.tangents.iter().map(|tangent| tangent_listed(floor, tangent)).collect::<Vec<_>>(),
            "next": null,
            "continued": false,
            "first": format!("{}/t", floor.origin),
            "can": can,
            "place": PLACE_LABEL,
            "door": format!("{}/", floor.origin),
            "who": who_json(floor, visit, "/t"),
        }),
    )
}

fn watching_json(
    floor: &Floor,
    visit: &Visit<'_>,
    token: Option<&str>,
    address: &str,
    scope: &str,
) -> Value {
    let (Some(reader), Some(token)) = (&visit.reader, token) else {
        return Value::Null;
    };
    let watch = floor.watches.contains(&(reader.clone(), scope.to_owned()));
    json!({
        "watch": watch,
        "form": form(
            &format!("{address}/watch"),
            json!([field("watch", Some(if watch { "false" } else { "true" }))]),
            token,
        ),
    })
}

fn tangent_page(floor: &mut Floor, visit: &mut Visit<'_>, key: &str) -> Reply {
    let Some(tangent) = floor
        .tangents
        .iter()
        .find(|tangent| tangent.key == key)
        .cloned()
    else {
        return absent();
    };
    let (can, gate) = can_and_gate(floor, visit);
    let token = visit.token(floor);
    let address = format!("{}/t/{key}", floor.origin);
    let last_log = |topic: &Topic| {
        floor
            .posts
            .iter()
            .filter(|post| post.topic == topic.key)
            .map(|post| post.log)
            .max()
            .unwrap_or(0)
    };
    let mut topics: Vec<&Topic> = floor
        .topics
        .iter()
        .filter(|topic| topic.tangent == key)
        .collect();
    topics.sort_by_key(|topic| std::cmp::Reverse(last_log(topic)));
    let from = visit
        .request
        .query("after")
        .and_then(|after| after.parse::<usize>().ok())
        .unwrap_or(0);
    let rows: Vec<Value> = topics
        .iter()
        .skip(from)
        .take(TOPICS_MOST)
        .map(|topic| {
            let root = floor.posts.iter().find(|post| post.id == topic.root);
            let seen = visit
                .reader
                .as_ref()
                .and_then(|reader| floor.topic_seen.get(&(reader.clone(), topic.key.clone())))
                .copied()
                .unwrap_or(-1);
            let new = floor
                .posts
                .iter()
                .filter(|post| post.topic == topic.key && post.sequence as i64 > seen)
                .count();
            json!({
                "id": topic.root,
                "slug": topic.key,
                "title": topic.title,
                "kind": "topic",
                "address": format!("{address}/{}", topic.key),
                "author": root.map(|root| author_json(floor, &root.author)),
                "opening": root.map(|root| root.text.chars().take(180).collect::<String>()),
                "readMinutes": 1,
                "replyCount": topic.last_sequence,
                "pinned": false,
                "locked": topic.locked,
                "lastActivityAt": rfc3339(1_790_000_000 + last_log(topic) as i64 * 60),
                "people": [],
                "new": visit.reader.is_some() && new > 0,
                "newPosts": if visit.reader.is_some() { new } else { 0 },
            })
        })
        .collect();
    let next = (topics.len() > from + TOPICS_MOST)
        .then(|| format!("{address}?after={}", from + TOPICS_MOST));
    Reply::json(
        200,
        &json!({
            "slug": key,
            "name": tangent.name,
            "description": tangent.description,
            "kind": "topic",
            "audience": "public",
            "address": address,
            "can": can,
            "topics": rows,
            "next": next,
            "continued": from > 0,
            "newTopic": gate.is_none().then(|| format!("{address}/new")),
            "watching": watching_json(floor, visit, token.as_deref(), &address, key),
            "place": PLACE_LABEL,
            "door": format!("{}/", floor.origin),
            "who": who_json(floor, visit, &format!("/t/{key}")),
        }),
    )
}

fn new_topic_page(floor: &mut Floor, visit: &mut Visit<'_>, key: &str) -> Reply {
    if !floor.tangents.iter().any(|tangent| tangent.key == key) {
        return absent();
    }
    let (can, gate) = can_and_gate(floor, visit);
    let address = format!("{}/t/{key}", floor.origin);
    let start = if gate.is_none() {
        visit.token(floor).map(|token| {
            form(
                &format!("{address}/new"),
                json!([
                    field("id", Some("3mz5acqquq4jl")),
                    field("title", None),
                    field("body", None),
                    field("kind", None),
                ]),
                &token,
            )
        })
    } else {
        None
    };
    Reply::json(
        200,
        &json!({
            "tangent": key,
            "tangentAddress": address,
            "kind": "topic",
            "start": start,
            "can": can,
            "gate": gate,
            "place": PLACE_LABEL,
            "door": format!("{}/", floor.origin),
            "who": who_json(floor, visit, &format!("/t/{key}/new")),
        }),
    )
}

/// A Post, as a write answers a JSON request.
fn written_json(floor: &Floor, post: &Post) -> Value {
    let topic = floor.topics.iter().find(|topic| topic.key == post.topic);
    json!({
        "id": post.id,
        "address": post_ref(floor, post),
        "tangent": topic.map(|topic| topic.tangent.clone()),
        "topicId": topic.map(|topic| topic.root.clone()),
        "parentId": post.reply_to,
        "slug": post.root.then(|| post.topic.clone()),
        "title": if post.root { topic.map(|topic| topic.title.clone()) } else { None },
        "body": post.text,
        "version": post.version,
        "sequence": post.sequence,
        "authorId": post.author,
        "createdAt": rfc3339(1_790_000_000 + post.log as i64 * 60),
        "removed": post.removed,
        "can": ["View", "Post", "Topic", "EditOwn", "RemoveOwn"],
    })
}

/// A word to make a slug of.
fn slug_of(title: &str) -> String {
    let slug: String = title
        .to_lowercase()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect();
    slug.split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

fn start_topic(floor: &mut Floor, visit: &Visit<'_>, key: &str) -> Reply {
    if let Some(refused) = refused_at_once(floor, visit) {
        return refused;
    }
    if !floor.tangents.iter().any(|tangent| tangent.key == key) {
        return absent();
    }
    if let (_, Some(gate)) = can_and_gate(floor, visit) {
        return gated(gate);
    }
    let Some(fields) = fields_of(visit.request) else {
        return unreadable();
    };
    let text = |name: &str| fields.get(name).and_then(Value::as_str).unwrap_or_default();
    let (id, title, body) = (text("id"), text("title"), text("body"));
    if !tid_shaped(id) || title.is_empty() || body.is_empty() {
        return problem(
            400,
            "Not written",
            "A Topic needs a TID, a title and a body.",
            None,
        );
    }
    let reader = visit.reader.clone().unwrap_or_default();
    if let Some(existing) = floor.posts.iter().find(|post| post.id == id) {
        return if existing.author == reader && existing.text == body {
            Reply::json(201, &written_json(floor, existing))
        } else {
            problem(409, "Already written", "That id holds other words.", None)
        };
    }
    let slug = slug_of(title);
    floor.log += 1;
    let log = floor.log;
    let post = Post {
        id: id.to_owned(),
        topic: slug.clone(),
        author: reader,
        text: body.to_owned(),
        reply_to: None,
        removed: false,
        edited: false,
        root: true,
        sequence: 0,
        version: 1,
        log,
    };
    floor.posts.push(post.clone());
    floor.topics.push(Topic {
        key: slug,
        tangent: key.to_owned(),
        title: title.to_owned(),
        root: id.to_owned(),
        locked: false,
        refusing: false,
        last_sequence: 0,
    });
    Reply::json(201, &written_json(floor, &post))
}

/// The Topic `topic` of the Tangent `tangent`.
fn located(floor: &Floor, tangent: &str, topic: &str) -> Option<Topic> {
    floor
        .topics
        .iter()
        .find(|found| found.key == topic && found.tangent == tangent)
        .cloned()
}

/// One Post as a Topic page shows it.
fn post_json(floor: &Floor, visit: &Visit<'_>, post: &Post, token: Option<&str>) -> Value {
    let reader = visit.reader.as_deref();
    let mine = reader == Some(post.author.as_str());
    let address = post_ref(floor, post);
    let parent = post.reply_to.as_ref().and_then(|id| {
        floor
            .posts
            .iter()
            .find(|candidate| candidate.id == *id)
            .map(|candidate| post_ref(floor, candidate))
    });
    let seen = reader
        .and_then(|reader| {
            floor
                .topic_seen
                .get(&(reader.to_owned(), post.topic.clone()))
                .copied()
        })
        .unwrap_or(-1);
    let report = match token {
        Some(token) if !mine && !post.removed => form(
            &format!("{address}/report"),
            json!([field("id", Some("3mz5acrpyshjl")), field("body", None)]),
            token,
        ),
        _ => Value::Null,
    };
    json!({
        "id": post.id,
        "sequence": post.sequence,
        "address": address,
        "author": author_json(floor, &post.author),
        "body": if post.removed { None } else { Some(post.text.clone()) },
        "mentions": [],
        "createdAt": rfc3339(1_790_000_000 + post.log as i64 * 60),
        "version": post.version,
        "parentId": post.reply_to,
        "parent": parent,
        "removed": post.removed.then(|| json!({
            "by": author_json(floor, &post.author),
            "at": rfc3339(1_790_000_000 + post.log as i64 * 60),
        })),
        "new": reader.is_some() && post.sequence as i64 > seen,
        "can": ["View"],
        "edit": (mine && !post.removed).then(|| format!("{address}/edit")),
        "report": report,
    })
}

fn topic_page(
    floor: &mut Floor,
    visit: &mut Visit<'_>,
    tangent: &str,
    topic: &str,
    anchor: Option<&str>,
) -> Reply {
    let Some(found) = located(floor, tangent, topic) else {
        return absent();
    };
    let anchored = match anchor {
        Some(id) => match floor
            .posts
            .iter()
            .find(|post| post.id == id && post.topic == topic)
        {
            Some(post) => Some(post.clone()),
            None => return absent(),
        },
        None => None,
    };
    let (can, standing) = can_and_gate(floor, visit);
    let token = visit.token(floor);
    let address = format!("{}{}", floor.origin, topic_route(floor, topic));
    let replies: Vec<Post> = floor
        .posts
        .iter()
        .filter(|post| post.topic == topic && !post.root)
        .cloned()
        .collect();
    let seen = visit.reader.as_ref().and_then(|reader| {
        floor
            .topic_seen
            .get(&(reader.clone(), topic.to_owned()))
            .copied()
    });
    let last = found.last_sequence as i64;
    let before = visit
        .request
        .query("before")
        .and_then(|v| v.parse::<i64>().ok());
    let after = visit
        .request
        .query("after")
        .and_then(|v| v.parse::<i64>().ok());
    let most = WINDOW_MOST as i64;
    let (low, high) = if let Some(post) = &anchored {
        if post.root {
            (1, most)
        } else {
            let low = (post.sequence as i64 - most / 2).max(1);
            (low, low + most - 1)
        }
    } else if let Some(before) = before {
        ((before - most).max(1), before - 1)
    } else if let Some(after) = after {
        (after + 1, after + most)
    } else {
        match seen {
            Some(seen) if seen >= last => ((last - most + 1).max(1), last),
            Some(seen) => (seen.max(0) + 1, seen.max(0) + most),
            None => (1, most),
        }
    };
    let window: Vec<&Post> = replies
        .iter()
        .filter(|post| (post.sequence as i64) >= low && (post.sequence as i64) <= high)
        .collect();
    let shown_through = window.last().map_or(0, |post| post.sequence as i64);
    let seen_sequence = seen.unwrap_or(-1);
    let continues = low <= seen_sequence.max(0) + 1;
    let root = floor
        .posts
        .iter()
        .find(|post| post.id == found.root)
        .cloned()
        .unwrap_or_else(|| replies[0].clone());
    let gate = standing.or(found.locked.then_some("locked"));
    let reply = match (&token, gate) {
        (Some(token), None) => form(
            &address,
            json!([
                field("id", Some("3mz5acrpyshjl")),
                field("body", None),
                field(
                    "parent",
                    Some(
                        anchored
                            .as_ref()
                            .filter(|post| {
                                !post.root && Some(post.author.as_str()) != visit.reader.as_deref()
                            })
                            .map_or("", |post| post.id.as_str())
                    )
                ),
            ]),
            token,
        ),
        _ => Value::Null,
    };
    let seen_form = match &token {
        Some(token) if continues && shown_through > seen_sequence => form(
            &format!("{address}/seen"),
            json!([field("through", Some(&shown_through.to_string()))]),
            token,
        ),
        _ => Value::Null,
    };
    let older = (low > 1).then(|| format!("{address}?before={low}"));
    let newer = (shown_through < last && !window.is_empty())
        .then(|| format!("{address}?after={shown_through}"));
    let route = topic_route(floor, topic);
    Reply::json(
        200,
        &json!({
            "id": found.root,
            "slug": topic,
            "title": found.title,
            "kind": "topic",
            "address": address,
            "tangent": { "slug": tangent, "name": tangent, "address": format!("{}/t/{tangent}", floor.origin) },
            "locked": found.locked,
            "pinned": false,
            "replyCount": replies.len(),
            "can": can,
            "gate": gate,
            "reply": reply,
            "seen": seen_form,
            "watching": watching_json(floor, visit, token.as_deref(), &address, topic),
            "root": post_json(floor, visit, &root, token.as_deref()),
            "replies": window.iter().map(|post| post_json(floor, visit, post, token.as_deref())).collect::<Vec<_>>(),
            "from": window.first().map(|post| post.sequence),
            "through": window.last().map(|post| post.sequence),
            "older": older,
            "newer": newer,
            "anchor": anchored.as_ref().map(|post| post.id.clone()),
            "place": PLACE_LABEL,
            "door": format!("{}/", floor.origin),
            "who": who_json(floor, visit, &route),
            "day": "2026-10-31",
        }),
    )
}

fn post_reply(floor: &mut Floor, visit: &Visit<'_>, tangent: &str, topic: &str) -> Reply {
    if let Some(refused) = refused_at_once(floor, visit) {
        return refused;
    }
    let Some(found) = located(floor, tangent, topic) else {
        return absent();
    };
    if let (_, Some(gate)) = can_and_gate(floor, visit) {
        return gated(gate);
    }
    if found.locked {
        return gated("locked");
    }
    if found.refusing {
        return problem(403, "Not open to you", "This isn't open to you here.", None);
    }
    let Some(fields) = fields_of(visit.request) else {
        return unreadable();
    };
    let text = |name: &str| fields.get(name).and_then(Value::as_str).unwrap_or_default();
    let (id, body, parent) = (text("id"), text("body"), text("parent"));
    if !tid_shaped(id) || body.is_empty() {
        return problem(400, "Not written", "A reply needs a TID and a body.", None);
    }
    let reader = visit.reader.clone().unwrap_or_default();
    let parent = (!parent.is_empty() && parent != found.root).then_some(parent);
    if let Some(parent) = parent
        && !floor
            .posts
            .iter()
            .any(|post| post.id == parent && post.topic == topic)
    {
        return problem(400, "Not written", "That Post is not of this Topic.", None);
    }
    if let Some(existing) = floor.posts.iter().find(|post| post.id == id) {
        if existing.author != reader || existing.topic != topic {
            return problem(400, "Not written", "That id is taken.", None);
        }
        return if existing.text == body && existing.reply_to.as_deref() == parent {
            Reply::json(201, &written_json(floor, existing))
        } else {
            problem(409, "Already written", "That id holds other words.", None)
        };
    }
    let post = reply(floor, id, topic, &reader, body, parent);
    Reply::json(201, &written_json(floor, &post))
}

fn checkpoint_answer(scope: &str, seq: Option<i64>, watch: bool) -> Reply {
    Reply::json(200, &json!({ "scope": scope, "seq": seq, "watch": watch }))
}

fn mark_seen(floor: &mut Floor, visit: &Visit<'_>, tangent: &str, topic: &str) -> Reply {
    if let Some(refused) = refused_at_once(floor, visit) {
        return refused;
    }
    if located(floor, tangent, topic).is_none() {
        return absent();
    }
    let Some(through) = fields_of(visit.request)
        .and_then(|fields| fields.get("through")?.as_str()?.parse::<i64>().ok())
    else {
        return problem(400, "Not a sequence", "Name one.", None);
    };
    let reader = visit.reader.clone().unwrap_or_default();
    let watch = floor.watches.contains(&(reader.clone(), topic.to_owned()));
    let seen = floor
        .topic_seen
        .entry((reader, topic.to_owned()))
        .or_insert(-1);
    *seen = (*seen).max(through);
    checkpoint_answer(&format!("topic:{topic}"), Some(*seen), watch)
}

fn set_watch(floor: &mut Floor, visit: &Visit<'_>, tangent: &str, topic: Option<&&str>) -> Reply {
    if let Some(refused) = refused_at_once(floor, visit) {
        return refused;
    }
    let scope = match topic {
        Some(topic) if located(floor, tangent, topic).is_some() => (*topic).to_owned(),
        None if floor.tangents.iter().any(|known| known.key == tangent) => tangent.to_owned(),
        _ => return absent(),
    };
    let Some(watch) = fields_of(visit.request)
        .and_then(|fields| fields.get("watch")?.as_str()?.parse::<bool>().ok())
    else {
        return problem(400, "Not a watch flag", "It is true or false.", None);
    };
    let reader = visit.reader.clone().unwrap_or_default();
    if watch {
        floor.watches.insert((reader, scope.clone()));
    } else {
        floor.watches.remove(&(reader, scope.clone()));
    }
    checkpoint_answer(&scope, None, watch)
}

/// The Post `post` of the Topic `topic`.
fn located_post(floor: &Floor, tangent: &str, topic: &str, post: &str) -> Option<Post> {
    located(floor, tangent, topic)?;
    floor
        .posts
        .iter()
        .find(|found| found.id == post && found.topic == topic)
        .cloned()
}

fn edit_page(
    floor: &mut Floor,
    visit: &mut Visit<'_>,
    tangent: &str,
    topic: &str,
    post: &str,
) -> Reply {
    let Some(found) = located_post(floor, tangent, topic, post) else {
        return absent();
    };
    let mine = visit.reader.as_deref() == Some(found.author.as_str());
    let address = post_ref(floor, &found);
    let may = mine && !found.removed;
    let token = if may { visit.token(floor) } else { None };
    let gate = if found.removed {
        Some("removed")
    } else if !mine {
        Some("not yours")
    } else {
        None
    };
    Reply::json(
        200,
        &json!({
            "id": found.id,
            "address": address,
            "tangent": tangent,
            "topicAddress": format!("{}{}", floor.origin, topic_route(floor, topic)),
            "root": found.root,
            "body": found.text,
            "version": found.version,
            "removed": found.removed,
            "edit": token.as_ref().map(|token| form(
                &format!("{address}/edit"),
                json!([field("version", Some(&found.version.to_string())), field("body", None)]),
                token,
            )),
            "remove": token.as_ref().map(|token| form(&format!("{address}/remove"), json!([]), token)),
            "can": ["View"],
            "gate": gate,
            "removeGate": gate,
            "place": PLACE_LABEL,
            "door": format!("{}/", floor.origin),
            "who": who_json(floor, visit, &format!("{}/{post}/edit", topic_route(floor, topic))),
        }),
    )
}

/// What refuses a change to `found` by the visit's reader, if anything does.
fn change_refused(visit: &Visit<'_>, found: &Post) -> Option<Reply> {
    if found.removed {
        return Some(gated("removed"));
    }
    (visit.reader.as_deref() != Some(found.author.as_str())).then(|| gated("not yours"))
}

fn edit_post(
    floor: &mut Floor,
    visit: &Visit<'_>,
    tangent: &str,
    topic: &str,
    post: &str,
) -> Reply {
    if let Some(refused) = refused_at_once(floor, visit) {
        return refused;
    }
    let Some(found) = located_post(floor, tangent, topic, post) else {
        return absent();
    };
    if let Some(refused) = change_refused(visit, &found) {
        return refused;
    }
    let Some(fields) = fields_of(visit.request) else {
        return unreadable();
    };
    let body = fields
        .get("body")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let version = fields
        .get("version")
        .and_then(Value::as_str)
        .and_then(|version| version.parse::<u64>().ok());
    if body.is_empty() || version.is_none() {
        return problem(
            400,
            "Not written",
            "An edit needs a version and a body.",
            None,
        );
    }
    if version != Some(found.version) {
        return problem(409, "Changed since you read it", "Read it again.", None);
    }
    let Some(held) = floor.posts.iter_mut().find(|held| held.id == post) else {
        return absent();
    };
    held.text = body.to_owned();
    held.version += 1;
    held.edited = true;
    let edited = held.clone();
    Reply::json(200, &written_json(floor, &edited))
}

fn remove_post(
    floor: &mut Floor,
    visit: &Visit<'_>,
    tangent: &str,
    topic: &str,
    post: &str,
) -> Reply {
    if let Some(refused) = refused_at_once(floor, visit) {
        return refused;
    }
    let Some(found) = located_post(floor, tangent, topic, post) else {
        return absent();
    };
    if let Some(refused) = change_refused(visit, &found) {
        return refused;
    }
    let Some(held) = floor.posts.iter_mut().find(|held| held.id == post) else {
        return absent();
    };
    held.removed = true;
    let removed = held.clone();
    Reply::json(200, &written_json(floor, &removed))
}

fn report_page(
    floor: &mut Floor,
    visit: &mut Visit<'_>,
    tangent: &str,
    topic: &str,
    post: &str,
    received: bool,
) -> Reply {
    let Some(found) = located_post(floor, tangent, topic, post) else {
        return absent();
    };
    let mine = visit.reader.as_deref() == Some(found.author.as_str());
    let address = post_ref(floor, &found);
    let offered = !received && !mine && !found.removed;
    let token = if offered { visit.token(floor) } else { None };
    Reply::json(
        200,
        &json!({
            "reported": { "id": found.id, "address": address, "text": found.text, "removed": found.removed },
            "received": received,
            "report": token.as_ref().map(|token| form(
                &format!("{address}/report"),
                json!([field("id", Some("3mz5acrpyshjl")), field("body", None)]),
                token,
            )),
            "gate": if found.removed { Some("removed") } else { None },
            "place": PLACE_LABEL,
            "door": format!("{}/", floor.origin),
            "who": who_json(floor, visit, &format!("{}/{post}/report", topic_route(floor, topic))),
        }),
    )
}

fn report_post(
    floor: &mut Floor,
    visit: &mut Visit<'_>,
    tangent: &str,
    topic: &str,
    post: &str,
) -> Reply {
    if let Some(refused) = refused_at_once(floor, visit) {
        return refused;
    }
    if located_post(floor, tangent, topic, post).is_none() {
        return absent();
    }
    let Some(fields) = fields_of(visit.request) else {
        return unreadable();
    };
    let text = |name: &str| fields.get(name).and_then(Value::as_str).unwrap_or_default();
    let (id, reason) = (text("id"), text("body"));
    if !tid_shaped(id) || reason.is_empty() {
        return problem(
            400,
            "Not written",
            "A report needs a TID and a reason.",
            None,
        );
    }
    let reader = visit.reader.clone().unwrap_or_default();
    let reported = (post.to_owned(), reader, reason.to_owned());
    if !floor.reports.contains(&reported) {
        floor.reports.push(reported);
    }
    report_page(floor, visit, tangent, topic, post, true)
}

fn participant_page(floor: &Floor, visit: &Visit<'_>, someone: &str) -> Reply {
    let Some(found) = floor
        .participants
        .iter()
        .find(|participant| participant.reference == someone || participant.handle == someone)
    else {
        return absent();
    };
    let (can, gate) = can_and_gate(floor, visit);
    let posts: Vec<Value> = floor
        .posts
        .iter()
        .rev()
        .filter(|post| post.author == found.reference && !post.removed)
        .take(20)
        .map(|post| {
            let topic = floor.topics.iter().find(|topic| topic.key == post.topic);
            let route = topic_route(floor, &post.topic);
            json!({
                "id": post.id,
                "kind": if post.root { "topic" } else { "reply" },
                "topic": topic.map(|topic| topic.title.clone()),
                "tangent": topic.map(|topic| topic.tangent.clone()),
                "tangentAddress": topic.map(|topic| format!("{}/t/{}", floor.origin, topic.tangent)),
                "address": if post.root {
                    format!("{}{route}", floor.origin)
                } else {
                    post_ref(floor, post)
                },
                "line": post.text.chars().take(180).collect::<String>(),
                "at": rfc3339(1_790_000_000 + post.log as i64 * 60),
            })
        })
        .collect();
    let address = format!("{}/p/{}", floor.origin, segment(&found.handle));
    Reply::json(
        200,
        &json!({
            "id": found.reference,
            "handle": found.handle,
            "name": found.display_name,
            "displayName": found.display_name,
            "kind": "person",
            "roles": [],
            "address": address,
            "can": can,
            "gate": gate,
            "posts": posts,
            "older": null,
            "continued": false,
            "place": PLACE_LABEL,
            "door": format!("{}/", floor.origin),
            "who": who_json(floor, visit, &format!("/p/{someone}")),
            "day": "2026-10-31",
        }),
    )
}
