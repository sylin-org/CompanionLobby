//! A fake Tangent Space.
//!
//! The Space stands on its own loopback listener and speaks Tangent's wire
//! the way the real server does, as far as a companion's journey goes:
//!
//! * `GET /.well-known/tangent-auth` publishes the Space's authentication
//!   profile: its canonical origin, the service proof it takes (method and
//!   purpose `org.sylin.tangent.exchangeIdentity`, the audience Tangent
//!   derives from a loopback origin, `did:web:localhost%3A<port>#tangent`,
//!   ES256 or ES256K, in the `Authorization` header, at the exchange
//!   endpoint) and the participant credential it issues;
//! * `POST /api/v1/identity/exchange` takes a proof the atproto world's PDS
//!   minted, presented once as `Bearer`: it must be for this audience and
//!   method and not expired, and its `iss` and `jti` are claimed before
//!   anything is issued, so a proof is never taken twice. The body names
//!   the credential (1 to 80 printable characters), its life in days (1 to
//!   30) and the grants asked for (`welcome`, `read`, `post`, `manage`);
//!   any other member is refused. The answer is the participant credential
//!   and its `ts_` token;
//! * the experience routes under `/api/v1/experience` take the token as
//!   `Bearer`: arriving, the Tangents and their Topics, a Topic's window of
//!   Posts, what is new since a checkpoint, posting with a request id
//!   (the same request answers with the same Post; another post under the
//!   same id is a `request_conflict`), starting a Topic, editing and
//!   removing one's own Post, the read position, membership, watching,
//!   mentionables, a participant's card and reporting a Post. Every write
//!   but a `DELETE` must say `Content-Type: application/json`, or it is
//!   answered 415;
//! * a token the Space does not take (expired, revoked, or its participant
//!   suspended) is answered 401 `participant_authentication_required` with
//!   `WWW-Authenticate: Bearer`; a refusal is 403 with the problem shape
//!   `{code, message, field, retryable}`.
//!
//! Knobs change the profile it publishes, refuse proofs, suspend an
//! account, end every token it issued, issue tokens that expire soon,
//! refuse writes in a Topic, and grant fewer grants than asked. The
//! listener records every request and can hold a route, or its answer,
//! mid-flight.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::atproto::Proofs;
use crate::listener::{Listener, Received, Reply};
use crate::unique_token;

/// The well-known path of the authentication profile.
pub const PROFILE_PATH: &str = "/.well-known/tangent-auth";
/// The exchange endpoint's path.
pub const EXCHANGE_PATH: &str = "/api/v1/identity/exchange";
/// The method and purpose of the proof the Space takes.
pub const EXCHANGE_METHOD: &str = "org.sylin.tangent.exchangeIdentity";
/// The root of the experience routes.
pub const ROOT: &str = "/api/v1/experience";
/// What the Space calls itself.
pub const PLACE_LABEL: &str = "Lobby Test Space";

/// What the authentication profile says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// The profile the Space really has.
    Ready,
    /// 503 `exchange_unconfigured`: the Space takes no proofs yet.
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
struct Credential {
    token: String,
    participant: String,
    grants: Vec<String>,
    expires_at: i64,
    ended: bool,
}

#[derive(Debug, Clone)]
struct Participant {
    reference: String,
    did: Option<String>,
    handle: Option<String>,
    display_name: String,
}

#[derive(Debug, Clone)]
struct Tangent {
    key: String,
    name: String,
    description: String,
    members: Vec<String>,
}

#[derive(Debug, Clone)]
struct Topic {
    key: String,
    tangent: String,
    title: String,
    brief: String,
    refusing_writes: bool,
}

/// One Post on the fake Space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Post {
    /// Its id, the last segment of its reference.
    pub id: String,
    /// The key of its Topic.
    pub topic: String,
    /// Its author's participant reference.
    pub author: String,
    /// What it says.
    pub text: String,
    /// The id of the Post it replies to.
    pub reply_to: Option<String>,
    /// The request id it was posted under, when a participant posted it.
    pub request_id: Option<String>,
    /// Whether its author removed it.
    pub removed: bool,
    /// Whether its author edited it.
    pub edited: bool,
    seq: u64,
}

#[derive(Debug, Default)]
struct Floor {
    origin: String,
    profile: Option<Profile>,
    proofs: Proofs,
    claimed: HashSet<String>,
    rejecting_proofs: bool,
    suspended: HashSet<String>,
    lifetime_secs: Option<i64>,
    granting: Option<Vec<String>>,
    credentials: Vec<Credential>,
    participants: Vec<Participant>,
    tangents: Vec<Tangent>,
    topics: Vec<Topic>,
    posts: Vec<Post>,
    read: HashMap<(String, String), u64>,
    watches: HashMap<(String, String), String>,
    reports: Vec<(String, String)>,
    exchanges: Vec<Value>,
    seq: u64,
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
fn problem(status: u16, code: &str, message: &str) -> Reply {
    Reply::json(
        status,
        &json!({ "code": code, "message": message, "field": null, "retryable": false }),
    )
}

/// An identity-exchange error in the shape the exchange answers.
fn exchange_error(status: u16, code: &str) -> Reply {
    Reply::json(status, &json!({ "error": code }))
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

    /// The reference of the Tangent `key`.
    pub fn tangent_ref(&self, key: &str) -> String {
        format!("{}::{key}", self.origin())
    }

    /// The reference of the Topic `key`.
    pub fn topic_ref(&self, key: &str) -> String {
        let floor = locked(&self.floor);
        let tangent = floor
            .topics
            .iter()
            .find(|topic| topic.key == key)
            .map(|topic| topic.tangent.clone())
            .unwrap_or_default();
        format!("{}::{tangent}::{key}", floor.origin)
    }

    /// The reference of the Post `id`.
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
            members: Vec::new(),
        });
    }

    /// Adds a Topic to the Tangent `tangent`.
    pub fn add_topic(&self, tangent: &str, key: &str, title: &str, brief: &str) {
        locked(&self.floor).topics.push(Topic {
            key: key.to_owned(),
            tangent: tangent.to_owned(),
            title: title.to_owned(),
            brief: brief.to_owned(),
            refusing_writes: false,
        });
    }

    /// Adds a participant who is no companion, known by `display_name`,
    /// and answers its reference.
    pub fn add_participant(&self, display_name: &str, handle: &str) -> String {
        let mut floor = locked(&self.floor);
        let reference = format!("prt_{}", floor.participants.len() + 1);
        floor.participants.push(Participant {
            reference: reference.clone(),
            did: None,
            handle: Some(handle.to_owned()),
            display_name: display_name.to_owned(),
        });
        reference
    }

    /// Names the participant the account `did` becomes, once it exchanges a
    /// proof, by `display_name` and `handle`.
    pub fn name_account(&self, did: &str, display_name: &str, handle: &str) {
        let mut floor = locked(&self.floor);
        let participant = participant_of(&mut floor, did);
        if let Some(found) = floor
            .participants
            .iter_mut()
            .find(|known| known.reference == participant)
        {
            found.display_name = display_name.to_owned();
            found.handle = Some(handle.to_owned());
        }
    }

    /// Posts `text` in the Topic `topic` as the participant `author`,
    /// replying to the Post `reply_to` when it names one, and answers the
    /// new Post's id.
    pub fn add_post(
        &self,
        topic: &str,
        author: &str,
        text: &str,
        reply_to: Option<&str>,
    ) -> String {
        let mut floor = locked(&self.floor);
        floor.seq += 1;
        let seq = floor.seq;
        let id = format!("p{seq}");
        floor.posts.push(Post {
            id: id.clone(),
            topic: topic.to_owned(),
            author: author.to_owned(),
            text: text.to_owned(),
            reply_to: reply_to.map(str::to_owned),
            request_id: None,
            removed: false,
            edited: false,
            seq,
        });
        id
    }

    /// Every Post, oldest first.
    pub fn posts(&self) -> Vec<Post> {
        locked(&self.floor).posts.clone()
    }

    /// The participant reference the account `did` became.
    pub fn participant_of(&self, did: &str) -> Option<String> {
        locked(&self.floor)
            .participants
            .iter()
            .find(|participant| participant.did.as_deref() == Some(did))
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

    /// Suspends the account `did`: its exchanges are refused and its
    /// tokens are not taken.
    pub fn suspend(&self, did: &str) {
        locked(&self.floor).suspended.insert(did.to_owned());
    }

    /// Ends every token the Space issued so far, as a Space that restarted
    /// with a new key would.
    pub fn end_tokens(&self) {
        for credential in &mut locked(&self.floor).credentials {
            credential.ended = true;
        }
    }

    /// Issues tokens that expire `secs` from now, from now on.
    pub fn issue_for(&self, secs: i64) {
        locked(&self.floor).lifetime_secs = Some(secs);
    }

    /// Grants only `grants` from now on, whatever an exchange asks.
    pub fn grant_only(&self, grants: &[&str]) {
        locked(&self.floor).granting =
            Some(grants.iter().map(|grant| (*grant).to_owned()).collect());
    }

    /// Refuses every write in the Topic `topic` from now on with 403
    /// `permission_denied`, as a Topic its stewards locked.
    pub fn refuse_writes_in(&self, topic: &str) {
        for found in &mut locked(&self.floor).topics {
            if found.key == topic {
                found.refusing_writes = true;
            }
        }
    }

    /// The body of every exchange the Space took up, oldest first.
    pub fn exchanges(&self) -> Vec<Value> {
        locked(&self.floor).exchanges.clone()
    }

    /// Every token the Space issued, for tests that look for secrets where
    /// none may be.
    pub fn tokens(&self) -> Vec<String> {
        locked(&self.floor)
            .credentials
            .iter()
            .map(|credential| credential.token.clone())
            .collect()
    }
}

/// The participant the account `did` is, made on its first visit.
fn participant_of(floor: &mut Floor, did: &str) -> String {
    if let Some(found) = floor
        .participants
        .iter()
        .find(|participant| participant.did.as_deref() == Some(did))
    {
        return found.reference.clone();
    }
    let reference = format!("prt_{}", floor.participants.len() + 1);
    floor.participants.push(Participant {
        reference: reference.clone(),
        did: Some(did.to_owned()),
        handle: None,
        display_name: did.to_owned(),
    });
    reference
}

fn post_ref(floor: &Floor, post: &Post) -> String {
    let tangent = floor
        .topics
        .iter()
        .find(|topic| topic.key == post.topic)
        .map(|topic| topic.tangent.clone())
        .unwrap_or_default();
    format!("{}::{tangent}::{}::{}", floor.origin, post.topic, post.id)
}

fn answer(floor: &Mutex<Floor>, request: &Received) -> Reply {
    let mut floor = locked(floor);
    let route = request.route().to_owned();
    match (request.method.as_str(), route.as_str()) {
        ("GET", PROFILE_PATH) => profile(&floor),
        ("POST", EXCHANGE_PATH) => exchange(&mut floor, request),
        (method, path) if path == ROOT || path.starts_with(&format!("{ROOT}/")) => {
            if !matches!(method, "GET" | "DELETE")
                && request
                    .header("content-type")
                    .is_none_or(|kind| !kind.starts_with("application/json"))
            {
                return Reply::json(415, &json!({ "reason": "unsupported media type" }));
            }
            let Some(credential) = taken(&floor, request) else {
                return Reply::json(
                    401,
                    &json!({ "error": "participant_authentication_required" }),
                )
                .with_header("WWW-Authenticate", "Bearer");
            };
            let rest = path.strip_prefix(ROOT).unwrap_or_default().to_owned();
            experience(&mut floor, &credential, method, &rest, request)
        }
        _ => problem(404, "not_found", "no such route"),
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
            "credentials": {
                "type": "tangent_participant_token",
                "transport": "authorization_header",
                "defaultLifetimeDays": 1,
                "grants": ["welcome", "read", "post", "manage"],
                "manageGrantPolicy": "explicit",
            },
        }),
    )
}

fn exchange(floor: &mut Floor, request: &Received) -> Reply {
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
    // The proof is claimed before anything else is checked or issued, so a
    // proof is never taken twice whatever follows.
    if !floor
        .claimed
        .insert(format!("{}\n{}", proof.issuer, proof.id))
    {
        return exchange_error(401, "invalid_service_proof");
    }
    if floor.suspended.contains(&proof.issuer) {
        return exchange_error(403, "participant_suspended");
    }
    let body = request.json();
    floor.exchanges.push(body.clone());
    let Some(fields) = body.as_object() else {
        return exchange_error(400, "the body is one object");
    };
    if fields
        .keys()
        .any(|key| !matches!(key.as_str(), "name" | "lifetimeDays" | "grants"))
    {
        return exchange_error(400, "unknown member");
    }
    let name = body["name"].as_str().unwrap_or("forum-client").to_owned();
    if name.is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
        return exchange_error(400, "name must be 1 to 80 printable characters");
    }
    let days = body["lifetimeDays"].as_i64().unwrap_or(1);
    if !(1..=30).contains(&days) {
        return exchange_error(400, "lifetimeDays must be 1 to 30");
    }
    let mut grants: Vec<String> = Vec::new();
    for grant in body["grants"].as_array().cloned().unwrap_or_default() {
        let Some(grant) = grant.as_str() else {
            return exchange_error(400, "grants are names");
        };
        if !matches!(grant, "welcome" | "read" | "post" | "manage") {
            return exchange_error(400, "unknown grant");
        }
        if !grants.iter().any(|known| known == grant) {
            grants.push(grant.to_owned());
        }
    }
    if grants.is_empty() {
        grants = vec!["welcome".to_owned(), "read".to_owned(), "post".to_owned()];
    }
    if let Some(only) = &floor.granting {
        grants.retain(|grant| only.contains(grant));
    }
    let participant = participant_of(floor, &proof.issuer);
    let now = now_secs();
    let expires_at = now + floor.lifetime_secs.unwrap_or(days * 86_400);
    let token = format!("ts_{}", &unique_token("t").replace('-', "")[..]);
    let id = unique_token("cred");
    floor.credentials.push(Credential {
        token: token.clone(),
        participant: participant.clone(),
        grants: grants.clone(),
        expires_at,
        ended: false,
    });
    Reply::json(
        200,
        &json!({
            "profile": "tangent_participant",
            "credential": {
                "id": id,
                "participantId": participant,
                "name": name,
                "grants": grants,
                "createdAt": rfc3339(now),
                "expiresAt": rfc3339(expires_at),
                "revokedAt": null,
            },
            "token": token,
        }),
    )
}

/// The credential a request's `Bearer` token names, while the Space takes it.
fn taken(floor: &Floor, request: &Received) -> Option<Credential> {
    let token = request.header("authorization")?.strip_prefix("Bearer ")?;
    let credential = floor
        .credentials
        .iter()
        .find(|credential| credential.token == token)?;
    let did = floor
        .participants
        .iter()
        .find(|participant| participant.reference == credential.participant)
        .and_then(|participant| participant.did.clone());
    let suspended = did.is_some_and(|did| floor.suspended.contains(&did));
    (!credential.ended && credential.expires_at > now_secs() && !suspended)
        .then(|| credential.clone())
}

fn grants(credential: &Credential, grant: &str) -> bool {
    credential.grants.iter().any(|held| held == grant)
}

fn participant_json(floor: &Floor, reference: &str) -> Value {
    let participant = floor
        .participants
        .iter()
        .find(|participant| participant.reference == reference);
    json!({
        "participantRef": reference,
        "did": participant.and_then(|participant| participant.did.clone()),
        "displayName": participant.map(|participant| participant.display_name.clone()).unwrap_or_default(),
        "handle": participant.and_then(|participant| participant.handle.clone()),
    })
}

fn display_of(floor: &Floor, reference: &str) -> String {
    floor
        .participants
        .iter()
        .find(|participant| participant.reference == reference)
        .map(|participant| participant.display_name.clone())
        .unwrap_or_default()
}

fn post_json(floor: &Floor, post: &Post) -> Value {
    let reference = post_ref(floor, post);
    let reply = post.reply_to.as_ref().and_then(|id| {
        floor
            .posts
            .iter()
            .find(|candidate| candidate.id == *id)
            .map(|candidate| post_ref(floor, candidate))
    });
    let url = format!("{}/p/{}", floor.origin, post.id);
    json!({
        "ref": reference,
        "authorRef": post.author,
        "authorName": display_of(floor, &post.author),
        "text": if post.removed { String::new() } else { post.text.clone() },
        "replyTo": reply,
        "url": url,
        "createdAt": rfc3339(1_790_000_000 + post.seq as i64 * 60),
        "editedAt": if post.edited { Some(rfc3339(1_790_000_000 + post.seq as i64 * 60 + 30)) } else { None },
        "removed": post.removed,
        "facets": [],
        "source": { "service": "tangent", "uri": url, "recordId": post.id },
    })
}

fn tangent_json(floor: &Floor, tangent: &Tangent, participant: &str) -> Value {
    let topics: Vec<&Topic> = floor
        .topics
        .iter()
        .filter(|topic| topic.tangent == tangent.key)
        .collect();
    json!({
        "tangentRef": format!("{}::{}", floor.origin, tangent.key),
        "name": tangent.name,
        "description": tangent.description,
        "membership": if tangent.members.iter().any(|member| member == participant) { "member" } else { "none" },
        "admission": "open",
        "canJoin": true,
        "hasReadableTopics": !topics.is_empty(),
        "hasWritableTopics": topics.iter().any(|topic| !topic.refusing_writes),
    })
}

/// What is new for `participant`: Posts by others it has not read past,
/// newest first, within `scope` and after `checkpoint`.
fn attention(
    floor: &Floor,
    participant: &str,
    scope: Option<&str>,
    checkpoint: u64,
    limit: usize,
) -> (Value, u64) {
    let own: HashSet<&str> = floor
        .posts
        .iter()
        .filter(|post| post.author == participant)
        .map(|post| post.id.as_str())
        .collect();
    let mut news: Vec<&Post> = floor
        .posts
        .iter()
        .filter(|post| post.author != participant && !post.removed && post.seq > checkpoint)
        .filter(|post| {
            let read = floor
                .read
                .get(&(participant.to_owned(), post.topic.clone()))
                .copied()
                .unwrap_or(0);
            post.seq > read
        })
        .filter(|post| {
            scope.is_none_or(|scope| {
                let reference = post_ref(floor, post);
                reference.starts_with(&format!("{scope}::")) || scope == floor.origin
            })
        })
        .collect();
    news.reverse();
    let high = floor.posts.iter().map(|post| post.seq).max().unwrap_or(0);
    let replies = news
        .iter()
        .filter(|post| post.reply_to.as_deref().is_some_and(|id| own.contains(id)))
        .count();
    let more = news.len() > limit;
    let items: Vec<Value> = news
        .iter()
        .take(limit)
        .map(|post| {
            let reply = post.reply_to.as_deref().is_some_and(|id| own.contains(id));
            let topic = floor
                .topics
                .iter()
                .find(|topic| topic.key == post.topic)
                .map(|topic| format!("{}::{}::{}", floor.origin, topic.tangent, topic.key))
                .unwrap_or_default();
            json!({
                "ref": format!("att_{}", post.seq),
                "kind": if reply { "direct_reply" } else { "watched_activity" },
                "actorRef": post.author,
                "scopeRef": topic,
                "sourceRef": post_ref(floor, post),
                "excerpt": post.text.chars().take(160).collect::<String>(),
                "state": "new",
            })
        })
        .collect();
    (
        json!({
            "waitingCount": { "value": replies },
            "newActivityCount": { "value": news.len() },
            "items": items,
            "more": more,
        }),
        high,
    )
}

/// A page size the Space takes: out of range is its default.
fn coerced(request: &Received, default: usize, most: usize) -> usize {
    request
        .query("limit")
        .and_then(|limit| limit.parse::<usize>().ok())
        .filter(|limit| (1..=most).contains(limit))
        .unwrap_or(default)
}

fn experience(
    floor: &mut Floor,
    credential: &Credential,
    method: &str,
    rest: &str,
    request: &Received,
) -> Reply {
    let me = credential.participant.clone();
    let segments: Vec<&str> = rest.split('/').filter(|part| !part.is_empty()).collect();
    let reads = grants(credential, "read");
    let posts = grants(credential, "post");
    let denied = || {
        problem(
            403,
            "permission_denied",
            "this credential does not allow that",
        )
    };
    match (method, segments.as_slice()) {
        ("GET", []) => {
            if !reads {
                return denied();
            }
            let scope = request.query("scopeRef");
            let (attention, _) = attention(floor, &me, scope.as_deref(), 0, 10);
            let tangents: Vec<Value> = floor
                .tangents
                .iter()
                .take(10)
                .map(|tangent| tangent_json(floor, tangent, &me))
                .collect();
            Reply::json(
                200,
                &json!({
                    "identity": participant_json(floor, &me),
                    "place": { "label": PLACE_LABEL },
                    "authority": {
                        "canRead": reads,
                        "canPost": posts,
                        "canManage": grants(credential, "manage"),
                        "canStewardScope": false,
                        "management": { "canWarn": false, "canTimeout": false, "canSuspend": false, "canBan": false, "canAssignRoles": false },
                    },
                    "tangents": tangents,
                    "continuation": null,
                    "attention": attention,
                }),
            )
        }
        ("GET", ["tangents"]) => {
            if !reads {
                return denied();
            }
            let tangents: Vec<Value> = floor
                .tangents
                .iter()
                .take(10)
                .map(|tangent| tangent_json(floor, tangent, &me))
                .collect();
            Reply::json(200, &json!({ "tangents": tangents, "continuation": null }))
        }
        ("GET", ["tangents", key, "topics"]) => {
            if !reads {
                return denied();
            }
            if !floor.tangents.iter().any(|tangent| tangent.key == *key) {
                return problem(404, "not_found", "no such tangent");
            }
            let topics: Vec<Value> = floor
                .topics
                .iter()
                .filter(|topic| topic.tangent == *key)
                .take(10)
                .map(|topic| {
                    json!({
                        "topicRef": format!("{}::{}::{}", floor.origin, topic.tangent, topic.key),
                        "title": topic.title,
                        "topic": topic.brief,
                        "canRead": true,
                        "canWrite": posts && !topic.refusing_writes,
                        "isLocked": topic.refusing_writes,
                        "allowPostEditing": true,
                    })
                })
                .collect();
            Reply::json(200, &json!({ "topics": topics, "continuation": null }))
        }
        ("GET", ["topics", key]) => {
            if !reads {
                return denied();
            }
            topic_window(floor, &me, posts, key, request)
        }
        ("GET", ["updates"]) => {
            if !reads {
                return denied();
            }
            let checkpoint = request.query("checkpoint").and_then(|checkpoint| {
                checkpoint
                    .strip_prefix("cp_")
                    .and_then(|seq| seq.parse().ok())
            });
            if request.query("checkpoint").is_some() && checkpoint.is_none() {
                return problem(
                    400,
                    "cursor_expired",
                    "the checkpoint is not one this Space gave",
                );
            }
            let scope = request.query("scopeRef");
            let limit = coerced(request, 10, 25);
            let (attention, high) =
                attention(floor, &me, scope.as_deref(), checkpoint.unwrap_or(0), limit);
            Reply::json(
                200,
                &json!({
                    "scopeRef": scope.unwrap_or_else(|| floor.origin.clone()),
                    "attention": attention,
                    "activityCheckpoint": format!("cp_{high}"),
                    "activityPageCursor": null,
                }),
            )
        }
        ("POST", ["topics", key, "posts"]) => {
            if !posts {
                return denied();
            }
            create_post(floor, &me, key, request)
        }
        ("POST", ["tangents", key, "topics"]) => {
            if !posts {
                return denied();
            }
            start_topic(floor, &me, key, request)
        }
        ("PATCH" | "DELETE", ["posts", id]) => {
            if !posts {
                return denied();
            }
            let Some(index) = floor.posts.iter().position(|post| post.id == *id) else {
                return problem(404, "not_found", "no such post");
            };
            if floor.posts[index].author != me {
                return problem(403, "permission_denied", "only its author changes a post");
            }
            if method == "PATCH" {
                let Some(text) = request.json()["text"].as_str().map(str::to_owned) else {
                    return problem(400, "invalid_arguments", "text is required");
                };
                floor.posts[index].text = text;
                floor.posts[index].edited = true;
            } else {
                floor.posts[index].removed = true;
            }
            let post = floor.posts[index].clone();
            Reply::json(
                200,
                &json!({
                    "postRef": post_ref(floor, &post),
                    "source": null,
                    "url": format!("{}/p/{}", floor.origin, post.id),
                    "requestId": null,
                }),
            )
        }
        ("POST", ["topics", key, "read-position"]) => {
            if !reads {
                return denied();
            }
            let Some(topic) = floor.topics.iter().find(|topic| topic.key == *key).cloned() else {
                return problem(404, "not_found", "no such topic");
            };
            let Some(seq) = request.json()["readCursor"]
                .as_str()
                .and_then(|cursor| cursor.strip_prefix("rc_"))
                .and_then(|seq| seq.parse::<u64>().ok())
            else {
                return problem(
                    400,
                    "cursor_expired",
                    "the read cursor is not one this Space gave",
                );
            };
            floor.read.insert((me.clone(), topic.key.clone()), seq);
            let through = floor
                .posts
                .iter()
                .filter(|post| post.topic == topic.key && post.seq <= seq)
                .max_by_key(|post| post.seq)
                .map(|post| post_ref(floor, post));
            Reply::json(
                200,
                &json!({
                    "throughPostRef": through,
                    "topicRef": format!("{}::{}::{}", floor.origin, topic.tangent, topic.key),
                }),
            )
        }
        ("PUT" | "DELETE", ["tangents", key, "membership"]) => {
            if !reads {
                return denied();
            }
            let Some(tangent) = floor
                .tangents
                .iter_mut()
                .find(|tangent| tangent.key == *key)
            else {
                return problem(404, "not_found", "no such tangent");
            };
            tangent.members.retain(|member| *member != me);
            if method == "PUT" {
                tangent.members.push(me.clone());
                Reply::json(
                    200,
                    &json!({ "membership": "member", "message": "Welcome in." }),
                )
            } else {
                Reply::json(
                    200,
                    &json!({ "membership": "none", "message": "You left." }),
                )
            }
        }
        ("PUT", ["watches"]) => {
            if !reads {
                return denied();
            }
            let body = request.json();
            let (Some(scope), Some(mode)) = (body["scopeRef"].as_str(), body["mode"].as_str())
            else {
                return problem(400, "invalid_arguments", "scopeRef and mode are required");
            };
            if !matches!(mode, "all" | "replies" | "none") {
                return problem(400, "invalid_arguments", "mode is all, replies or none");
            }
            floor
                .watches
                .insert((me.clone(), scope.to_owned()), mode.to_owned());
            Reply::json(200, &json!({ "scopeRef": scope, "mode": mode }))
        }
        ("GET", ["topics", key, "mentionables"]) => {
            if !reads {
                return denied();
            }
            let prefix = request.query("prefix").unwrap_or_default().to_lowercase();
            let mut seen = Vec::new();
            for post in floor.posts.iter().filter(|post| post.topic == *key) {
                if !seen.contains(&post.author) {
                    seen.push(post.author.clone());
                }
            }
            let targets: Vec<Value> = seen
                .iter()
                .filter_map(|reference| {
                    floor
                        .participants
                        .iter()
                        .find(|participant| participant.reference == *reference)
                })
                .filter(|participant| participant.display_name.to_lowercase().starts_with(&prefix))
                .take(10)
                .map(|participant| {
                    json!({
                        "kind": "participant",
                        "label": participant.display_name,
                        "insert": format!("@{}", participant.handle.clone().unwrap_or_else(|| participant.reference.clone())),
                        "did": participant.did,
                        "count": null,
                    })
                })
                .collect();
            Reply::json(200, &json!({ "targets": targets }))
        }
        ("GET", ["participants", identifier]) => {
            if !reads {
                return denied();
            }
            let identifier = crate::percent_decode(identifier);
            let Some(participant) = floor
                .participants
                .iter()
                .find(|participant| {
                    participant.reference == identifier
                        || participant.handle.as_deref() == Some(identifier.as_str())
                })
                .cloned()
            else {
                return problem(404, "not_found", "no such participant");
            };
            let recent: Vec<Value> = floor
                .posts
                .iter()
                .rev()
                .filter(|post| post.author == participant.reference && !post.removed)
                .take(10)
                .map(|post| post_json(floor, post))
                .collect();
            Reply::json(
                200,
                &json!({
                    "profile": {
                        "participantRef": participant.reference,
                        "did": participant.did,
                        "handle": participant.handle,
                        "displayName": participant.display_name,
                        "classification": "member",
                        "description": null,
                        "avatar": null,
                        "profileUrl": null,
                    },
                    "recentPosts": recent,
                    "morePosts": false,
                }),
            )
        }
        ("POST", ["topics", _key, "reports"]) => {
            if !reads {
                return denied();
            }
            let body = request.json();
            let Some(post) = body["postRef"].as_str().map(str::to_owned) else {
                return problem(400, "invalid_arguments", "postRef is required");
            };
            let already = floor
                .reports
                .iter()
                .any(|(reporter, reported)| *reporter == me && *reported == post);
            if !already {
                floor.reports.push((me.clone(), post.clone()));
            }
            Reply::json(
                200,
                &json!({ "postRef": post, "accepted": true, "alreadyReported": already, "caseSaturated": false }),
            )
        }
        _ => problem(404, "not_found", "no such route"),
    }
}

fn topic_window(floor: &Floor, me: &str, posts: bool, key: &str, request: &Received) -> Reply {
    let Some(topic) = floor.topics.iter().find(|topic| topic.key == key).cloned() else {
        return problem(404, "not_found", "no such topic");
    };
    let limit = coerced(request, 10, 25);
    let before = match request.query("cursor") {
        None => None,
        Some(cursor) => match cursor
            .strip_prefix("older_")
            .and_then(|seq| seq.parse::<u64>().ok())
        {
            Some(seq) => Some(seq),
            None => {
                return problem(
                    400,
                    "cursor_expired",
                    "the cursor is not one this Space gave",
                );
            }
        },
    };
    let all: Vec<&Post> = floor
        .posts
        .iter()
        .filter(|post| post.topic == topic.key && before.is_none_or(|seq| post.seq < seq))
        .collect();
    let window: Vec<&Post> = all.iter().rev().take(limit).rev().copied().collect();
    let older = (all.len() > window.len())
        .then(|| window.first().map(|post| format!("older_{}", post.seq)))
        .flatten();
    let high = window.last().map_or(0, |post| post.seq);
    let _ = me;
    Reply::json(
        200,
        &json!({
            "topicRef": format!("{}::{}::{}", floor.origin, topic.tangent, topic.key),
            "title": topic.title,
            "brief": topic.brief,
            "posts": window.iter().map(|post| post_json(floor, post)).collect::<Vec<_>>(),
            "position": if before.is_some() { "older" } else { "recent" },
            "canWrite": posts && !topic.refusing_writes,
            "stewardship": false,
            "isLocked": topic.refusing_writes,
            "allowPostEditing": true,
            "historyOlderCursor": older,
            "historyNewerCursor": null,
            "readCursor": format!("rc_{high}"),
        }),
    )
}

fn create_post(floor: &mut Floor, me: &str, key: &str, request: &Received) -> Reply {
    let Some(topic) = floor.topics.iter().find(|topic| topic.key == key).cloned() else {
        return problem(404, "not_found", "no such topic");
    };
    if topic.refusing_writes {
        return problem(403, "permission_denied", "this topic takes no new posts");
    }
    let body = request.json();
    let (Some(request_id), Some(text)) = (body["requestId"].as_str(), body["text"].as_str()) else {
        return problem(400, "invalid_arguments", "requestId and text are required");
    };
    if request_id.is_empty() || request_id.len() > 128 || text.trim().is_empty() {
        return problem(400, "invalid_arguments", "requestId and text are required");
    }
    let reply_to = match body["replyTo"].as_str() {
        None => None,
        Some(reference) => {
            let id = reference.rsplit("::").next().unwrap_or_default().to_owned();
            if !floor
                .posts
                .iter()
                .any(|post| post.id == id && post.topic == topic.key)
            {
                return problem(
                    400,
                    "invalid_arguments",
                    "replyTo names no post of this topic",
                );
            }
            Some(id)
        }
    };
    if let Some(earlier) = floor
        .posts
        .iter()
        .find(|post| post.author == me && post.request_id.as_deref() == Some(request_id))
        .cloned()
    {
        // The same request answers with the same Post; another post under
        // the same request id is a conflict.
        if earlier.topic != topic.key || earlier.text != text || earlier.reply_to != reply_to {
            return problem(
                409,
                "request_conflict",
                "that request id named another post",
            );
        }
        return post_result(floor, &earlier, request_id);
    }
    floor.seq += 1;
    let seq = floor.seq;
    let post = Post {
        id: format!("p{seq}"),
        topic: topic.key.clone(),
        author: me.to_owned(),
        text: text.to_owned(),
        reply_to,
        request_id: Some(request_id.to_owned()),
        removed: false,
        edited: false,
        seq,
    };
    floor.posts.push(post.clone());
    floor.read.insert((me.to_owned(), topic.key.clone()), seq);
    post_result(floor, &post, request_id)
}

fn post_result(floor: &Floor, post: &Post, request_id: &str) -> Reply {
    Reply::json(
        200,
        &json!({
            "postRef": post_ref(floor, post),
            "source": null,
            "url": format!("{}/p/{}", floor.origin, post.id),
            "requestId": request_id,
        }),
    )
}

fn start_topic(floor: &mut Floor, me: &str, tangent: &str, request: &Received) -> Reply {
    if !floor.tangents.iter().any(|known| known.key == tangent) {
        return problem(404, "not_found", "no such tangent");
    }
    let body = request.json();
    let (Some(request_id), Some(title), Some(text)) = (
        body["requestId"].as_str(),
        body["title"].as_str(),
        body["text"].as_str(),
    ) else {
        return problem(
            400,
            "invalid_arguments",
            "requestId, title and text are required",
        );
    };
    let digest = Sha256::digest(format!("{me}\n{tangent}\n{request_id}").as_bytes());
    let key = format!(
        "thread-{}",
        digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()[..24]
            .to_owned()
    );
    let existing = floor.topics.iter().find(|topic| topic.key == key).cloned();
    let opening = match existing {
        Some(topic) => {
            let first = floor
                .posts
                .iter()
                .find(|post| post.topic == topic.key)
                .cloned();
            match first {
                Some(first) if topic.title == title && first.text == text => first,
                _ => {
                    return problem(
                        409,
                        "request_conflict",
                        "that request id named another topic",
                    );
                }
            }
        }
        None => {
            floor.topics.push(Topic {
                key: key.clone(),
                tangent: tangent.to_owned(),
                title: title.to_owned(),
                brief: String::new(),
                refusing_writes: false,
            });
            floor.seq += 1;
            let seq = floor.seq;
            let post = Post {
                id: format!("p{seq}"),
                topic: key.clone(),
                author: me.to_owned(),
                text: text.to_owned(),
                reply_to: None,
                request_id: Some(request_id.to_owned()),
                removed: false,
                edited: false,
                seq,
            };
            floor.posts.push(post.clone());
            post
        }
    };
    Reply::json(
        200,
        &json!({
            "topicRef": format!("{}::{tangent}::{key}", floor.origin),
            "tangentRef": format!("{}::{tangent}", floor.origin),
            "postRef": post_ref(floor, &opening),
            "title": title,
            "url": format!("{}/t/{key}", floor.origin),
            "requestId": request_id,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_are_written_as_rfc_3339_in_utc() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339(1_790_000_000), "2026-09-21T14:13:20Z");
    }

    #[test]
    fn the_audience_of_a_loopback_space_names_its_port() {
        assert_eq!(
            loopback_audience("http://127.0.0.1:5300"),
            "did:web:localhost%3A5300#tangent"
        );
    }
}
