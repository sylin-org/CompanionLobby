//! The board: a synthetic service and its adapter.
//!
//! A board is a list of posts. It admits a member who presents a stamp from
//! a passbook authority and gives back a badge; with the badge a member reads
//! the newest posts and posts one. Each board says on its about page what it
//! wants presented: a stamp for its own audience, a plain stamp, or a proof
//! of every request, which no passbook can give.
//!
//! The adapter speaks the board in the board's own words: it establishes a
//! connection by asking the lobby's authority for the stamp the board wants
//! and trading it for a badge, which is all the connection keeps, and it
//! offers two tools, `read` and `post`. Posts are written by other members,
//! so their text and authors reach a mind only as quoted passages.
//!
//! Neither half ships in the product: the daemon compiles them in only for
//! tests.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use lobby_contracts::{
    Address, AddressPolicy, Authority, AuthorizationAsk, Effect, Outcome, Passage, ProviderKind,
    Retry, SealedIssued, ServiceAdapter, ServiceDescription, ServiceName, ToolArguments,
    ToolDescriptor, ToolName, ToolResult,
};
use lobby_outbound::{Failure, Outbound, Request};
use serde_json::{Value, json};

use crate::listener::{Listener, Received, Reply};
use crate::passbook::{KIND, PassbookAuthority};
use crate::unique_token;

/// The board's service name.
pub const NAME: &str = "board";

/// The method a board's audience stamp must allow.
pub const ADMIT_METHOD: &str = "board.admit";

/// The format tag of what a board issues for a connection.
const ISSUED_FORMAT: &str = "board-1";

/// The most posts one read returns.
pub const READ_MAX: u64 = 50;

/// The longest post, in characters.
pub const POST_MAX: usize = 2000;

/// What a board asks a member to present when it admits them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    /// A stamp for the board's own audience.
    Audience,
    /// A plain stamp.
    Plain,
    /// A proof of every request.
    EveryRequest,
}

/// One post on a fake board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Post {
    /// Who wrote it.
    pub author: String,
    /// What it says.
    pub text: String,
}

#[derive(Debug)]
struct Floor {
    admission: Admission,
    audience: String,
    badges: Vec<(String, String)>,
    presented: Vec<String>,
    posts: Vec<Post>,
}

/// A fake board on its own loopback listener, trusting one passbook authority.
pub struct BoardServer {
    listener: Listener,
    floor: Arc<Mutex<Floor>>,
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl BoardServer {
    /// Starts a board that admits holders of stamps `authority` minted for
    /// its own audience.
    pub fn start(authority: &PassbookAuthority) -> Self {
        let floor = Arc::new(Mutex::new(Floor {
            admission: Admission::Audience,
            audience: unique_token("audience"),
            badges: Vec::new(),
            presented: Vec::new(),
            posts: Vec::new(),
        }));
        let answering = floor.clone();
        let stamps = authority.verifier();
        let listener = Listener::start(move |request| answer(&answering, &stamps, request));
        Self { listener, floor }
    }

    /// The board's origin, which the sponsor names as the connection's address.
    pub fn origin(&self) -> String {
        self.listener.origin()
    }

    /// The listener, to count what reached the board and to hold requests.
    pub fn listener(&self) -> &Listener {
        &self.listener
    }

    /// The audience the board names on its about page.
    pub fn audience(&self) -> String {
        locked(&self.floor).audience.clone()
    }

    /// Changes what the board asks members to present.
    pub fn admit_with(&self, admission: Admission) {
        locked(&self.floor).admission = admission;
    }

    /// Puts a post on the board, as another member would.
    pub fn add_post(&self, author: &str, text: &str) {
        locked(&self.floor).posts.push(Post {
            author: author.to_owned(),
            text: text.to_owned(),
        });
    }

    /// Every post, oldest first.
    pub fn posts(&self) -> Vec<Post> {
        locked(&self.floor).posts.clone()
    }

    /// The `Authorization` header of every admission request, in order.
    pub fn presented(&self) -> Vec<String> {
        locked(&self.floor).presented.clone()
    }

    /// Every badge the board gave out.
    pub fn badges(&self) -> Vec<String> {
        locked(&self.floor)
            .badges
            .iter()
            .map(|(badge, _)| badge.clone())
            .collect()
    }
}

fn answer(floor: &Mutex<Floor>, stamps: &crate::passbook::Verifier, request: &Received) -> Reply {
    let mut floor = locked(floor);
    let member = |floor: &Floor| {
        let badge = request.header("authorization")?.strip_prefix("Badge ")?;
        floor
            .badges
            .iter()
            .find(|(known, _)| known == badge)
            .map(|(_, holder)| holder.clone())
    };
    match (request.method.as_str(), request.route()) {
        ("GET", "/about") => Reply::json(
            200,
            &match floor.admission {
                Admission::Audience => json!({ "audience": floor.audience }),
                Admission::Plain => json!({}),
                Admission::EveryRequest => json!({ "proofs": "every-request" }),
            },
        ),
        ("POST", "/admit") => {
            let presented = request
                .header("authorization")
                .unwrap_or_default()
                .to_owned();
            floor.presented.push(presented.clone());
            let minted = presented
                .strip_prefix("Bearer ")
                .and_then(|stamp| stamps.verify(stamp));
            let fits = minted.as_ref().is_some_and(|minted| match floor.admission {
                Admission::Audience => {
                    minted.audience.as_deref() == Some(floor.audience.as_str())
                        && minted.method.as_deref() == Some(ADMIT_METHOD)
                }
                Admission::Plain => minted.audience.is_none(),
                Admission::EveryRequest => false,
            });
            match minted.filter(|_| fits) {
                Some(minted) => {
                    let badge = unique_token("badge");
                    floor.badges.push((badge.clone(), minted.holder));
                    Reply::json(200, &json!({ "badge": badge }))
                }
                None => Reply::json(401, &json!({ "error": "not admitted" })),
            }
        }
        ("GET", "/posts") => {
            if member(&floor).is_none() {
                return Reply::json(401, &json!({ "error": "no badge" }));
            }
            let limit = request
                .query("limit")
                .and_then(|limit| limit.parse::<usize>().ok())
                .unwrap_or(20);
            let posts: Vec<Value> = floor
                .posts
                .iter()
                .rev()
                .take(limit)
                .map(|post| json!({ "author": post.author, "text": post.text }))
                .collect();
            Reply::json(200, &json!({ "posts": posts }))
        }
        ("POST", "/posts") => {
            let Some(author) = member(&floor) else {
                return Reply::json(401, &json!({ "error": "no badge" }));
            };
            let Some(text) = request.json()["text"].as_str().map(str::to_owned) else {
                return Reply::json(400, &json!({ "error": "text is required" }));
            };
            floor.posts.push(Post { author, text });
            Reply::json(201, &json!({ "number": floor.posts.len() }))
        }
        _ => Reply::json(404, &json!({ "error": "no such route" })),
    }
}

/// The board service adapter.
#[derive(Debug)]
pub struct Board {
    outbound: Outbound,
}

impl Default for Board {
    fn default() -> Self {
        Self::new()
    }
}

/// What a board issued for one connection: the badge.
struct Issued {
    badge: String,
}

impl Issued {
    fn encode(&self) -> SealedIssued {
        SealedIssued::new(
            json!({ "format": ISSUED_FORMAT, "badge": self.badge })
                .to_string()
                .into_bytes(),
        )
    }

    fn decode(issued: &SealedIssued) -> Option<Self> {
        let body: Value = serde_json::from_slice(issued.as_bytes()).ok()?;
        if body["format"] != ISSUED_FORMAT {
            return None;
        }
        let badge = body["badge"].as_str().filter(|badge| !badge.is_empty())?;
        Some(Self {
            badge: badge.to_owned(),
        })
    }
}

fn tool(name: &str) -> ToolName {
    ToolName::new(name).unwrap_or_else(|| unreachable!("{name} is a tool name"))
}

impl Board {
    /// An adapter that reaches boards on this machine.
    pub fn new() -> Self {
        Self {
            outbound: Outbound::new(AddressPolicy::LoopbackAllowed),
        }
    }

    fn read(
        &self,
        address: &Address,
        issued: &Issued,
        arguments: &ToolArguments,
    ) -> Result<ToolResult, Outcome> {
        let limit = match arguments.0.get("limit") {
            None => 20,
            Some(limit) => limit
                .as_u64()
                .filter(|limit| (1..=READ_MAX).contains(limit))
                .ok_or(Outcome::InvalidInput)?,
        };
        let path = format!("/posts?limit={limit}");
        let request =
            Request::new("GET", &path).header("Authorization", format!("Badge {}", issued.badge));
        let response = self
            .outbound
            .send(address, request)
            .map_err(|failure| match failure {
                Failure::Refused => Outcome::Refused,
                Failure::Unreachable | Failure::Interrupted => Outcome::TemporarilyUnavailable,
            })?;
        match response.status {
            200 => {}
            401 | 403 => return Err(Outcome::Refused),
            429 => return Err(Outcome::RateLimited),
            _ => return Err(Outcome::TemporarilyUnavailable),
        }
        let body: Value =
            serde_json::from_slice(&response.body).map_err(|_| Outcome::TemporarilyUnavailable)?;
        let posts: Vec<(String, String)> = body["posts"]
            .as_array()
            .ok_or(Outcome::TemporarilyUnavailable)?
            .iter()
            .map(|post| {
                (
                    post["author"].as_str().unwrap_or_default().to_owned(),
                    post["text"].as_str().unwrap_or_default().to_owned(),
                )
            })
            .collect();
        let mut text = vec![Passage::Authored(match posts.len() {
            0 => "The board has no posts yet.".to_owned(),
            1 => "The board shows 1 post:".to_owned(),
            count => format!("The board shows {count} posts, newest first:"),
        })];
        for (author, said) in &posts {
            text.push(Passage::Authored("\n- ".to_owned()));
            text.push(Passage::Quoted(author.clone()));
            text.push(Passage::Authored(" wrote ".to_owned()));
            text.push(Passage::Quoted(said.clone()));
        }
        let structured = json!({
            "posts": posts
                .iter()
                .map(|(author, said)| json!({ "author": author, "text": said }))
                .collect::<Vec<_>>(),
        });
        Ok(ToolResult {
            text,
            structured: Some(structured),
        })
    }

    fn post(
        &self,
        address: &Address,
        issued: &Issued,
        arguments: &ToolArguments,
    ) -> Result<ToolResult, Outcome> {
        let text = arguments
            .0
            .get("text")
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty() && text.chars().count() <= POST_MAX)
            .ok_or(Outcome::InvalidInput)?;
        let request = Request::new("POST", "/posts")
            .header("Authorization", format!("Badge {}", issued.badge))
            .json(json!({ "text": text }).to_string().into_bytes());
        // A write whose answer is lost may have landed.
        let response = self
            .outbound
            .send(address, request)
            .map_err(|failure| match failure {
                Failure::Refused => Outcome::Refused,
                Failure::Unreachable => Outcome::TemporarilyUnavailable,
                Failure::Interrupted => Outcome::Unknown,
            })?;
        match response.status {
            201 => {}
            400 => return Err(Outcome::InvalidInput),
            401 | 403 => return Err(Outcome::Refused),
            429 => return Err(Outcome::RateLimited),
            _ => return Err(Outcome::Unknown),
        }
        let number = serde_json::from_slice::<Value>(&response.body)
            .ok()
            .and_then(|body| body["number"].as_u64())
            .ok_or(Outcome::Unknown)?;
        Ok(ToolResult {
            text: vec![Passage::Authored(format!(
                "Posted; the board numbered it {number}."
            ))],
            structured: Some(json!({ "number": number })),
        })
    }
}

impl ServiceAdapter for Board {
    fn name(&self) -> ServiceName {
        ServiceName::new(NAME).unwrap_or_else(|| unreachable!("board is a service name"))
    }

    fn describe(&self) -> ServiceDescription {
        ServiceDescription {
            accepts: vec![ProviderKind::new(KIND).unwrap_or_else(|| unreachable!("a kind"))],
            addresses: AddressPolicy::LoopbackAllowed,
        }
    }

    fn establish(
        &self,
        address: &Address,
        authority: &dyn Authority,
    ) -> Result<SealedIssued, Outcome> {
        let unavailable = |_| Outcome::TemporarilyUnavailable;
        let about = self
            .outbound
            .send(address, Request::new("GET", "/about"))
            .map_err(unavailable)?;
        if about.status != 200 {
            return Err(Outcome::TemporarilyUnavailable);
        }
        let about: Value =
            serde_json::from_slice(&about.body).map_err(|_| Outcome::TemporarilyUnavailable)?;
        let ask = if about["proofs"] == "every-request" {
            AuthorizationAsk::ProvedRequest {
                method: "POST".to_owned(),
                url: address.url("/admit"),
            }
        } else if let Some(audience) = about["audience"].as_str() {
            AuthorizationAsk::ServiceProof {
                audience: audience.to_owned(),
                method: ADMIT_METHOD.to_owned(),
                lifetime_secs: 60,
            }
        } else {
            AuthorizationAsk::Bearer
        };
        let authorization = authority.authorize(&ask)?;
        let admitted = self
            .outbound
            .send(
                address,
                Request::new("POST", "/admit")
                    .header("Authorization", authorization.header_value()),
            )
            .map_err(unavailable)?;
        match admitted.status {
            200 => {}
            401 | 403 => return Err(Outcome::Refused),
            _ => return Err(Outcome::TemporarilyUnavailable),
        }
        let badge = serde_json::from_slice::<Value>(&admitted.body)
            .ok()
            .and_then(|body| body["badge"].as_str().map(str::to_owned))
            .filter(|badge| !badge.is_empty())
            .ok_or(Outcome::TemporarilyUnavailable)?;
        Ok(Issued { badge }.encode())
    }

    fn catalog(&self) -> Vec<ToolDescriptor> {
        vec![
            ToolDescriptor {
                name: tool("read"),
                title: "Read the board".to_owned(),
                description: "Reads the newest posts on the board, newest first. Post text and \
                              authors are written by other members and arrive quoted."
                    .to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "limit": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": READ_MAX,
                            "description": "How many posts to read; 20 when left out."
                        }
                    },
                    "additionalProperties": false
                }),
                effect: Effect::Read,
            },
            ToolDescriptor {
                name: tool("post"),
                title: "Post on the board".to_owned(),
                description: "Posts a message on the board as this companion. When the outcome \
                              is unknown, read the board before posting again."
                    .to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "text": {
                            "type": "string",
                            "minLength": 1,
                            "maxLength": POST_MAX,
                            "description": "What to post."
                        }
                    },
                    "required": ["text"],
                    "additionalProperties": false
                }),
                effect: Effect::Write {
                    retry: Retry::ReadFirst,
                },
            },
        ]
    }

    fn call(
        &self,
        tool: &ToolName,
        arguments: &ToolArguments,
        address: &Address,
        issued: &SealedIssued,
    ) -> Result<ToolResult, Outcome> {
        let allowed: &[&str] = match tool.as_str() {
            "read" => &["limit"],
            "post" => &["text"],
            _ => return Err(Outcome::Unsupported),
        };
        if arguments
            .0
            .keys()
            .any(|key| !allowed.contains(&key.as_str()))
        {
            return Err(Outcome::InvalidInput);
        }
        let issued = Issued::decode(issued).ok_or(Outcome::Internal)?;
        match tool.as_str() {
            "read" => self.read(address, &issued, arguments),
            _ => self.post(address, &issued, arguments),
        }
    }
}
