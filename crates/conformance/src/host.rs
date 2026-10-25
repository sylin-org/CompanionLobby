//! The host suite: the daemon proven through the two synthetic adapters.
//!
//! Every scenario runs the real `companion-lobby` binary named by `exe`,
//! built with the synthetic adapters compiled in, against its own data
//! directory and port, with a passbook authority and boards from the test
//! kit on their own loopback listeners. Minds attach through the real attach
//! shim, as a host would. Nothing is seeded by writing the state file: a
//! credential exists because the sponsor signed a companion in through the
//! passbook authority, and a connection because the board adapter
//! established it. Every scenario stops the daemon it started.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use lobby_contracts::{Address, AddressPolicy};
use lobby_daemon::contact::{self, Probe};
use lobby_daemon::loopback::{self, Answer, Call};
use lobby_daemon::mcp::LIST_CHANGED;
use lobby_outbound::{Outbound, Request};
use lobby_testkit::board::BoardServer;
use lobby_testkit::passbook::PassbookAuthority;
use lobby_testkit::{Peer, TempHome, free_port, secrets_in};
use serde_json::{Value, json};

/// How long a test waits for a notification it should not get.
const QUIET: Duration = Duration::from_millis(1500);

/// One install under test: a data directory and a port, stopped on drop.
pub struct Install {
    exe: PathBuf,
    home: TempHome,
    port: u16,
    extra: Vec<(&'static str, String)>,
}

impl Install {
    /// A fresh install of the binary `exe`.
    pub fn new(exe: &Path, label: &str) -> Self {
        Self {
            exe: exe.to_path_buf(),
            home: TempHome::new(label),
            port: free_port(),
            extra: Vec::new(),
        }
    }

    /// The same install, run with the environment variable `name` set to
    /// `value` as well.
    #[must_use]
    pub fn with_env(mut self, name: &'static str, value: &str) -> Self {
        self.extra.push((name, value.to_owned()));
        self
    }

    /// The port the daemon listens on.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The data directory.
    pub fn dir(&self) -> PathBuf {
        self.home.path().join("data")
    }

    fn environment(&self) -> Vec<(&'static str, String)> {
        let mut environment = vec![
            ("LOBBY_DATA_DIR", self.dir().to_string_lossy().into_owned()),
            ("LOBBY_PORT", self.port.to_string()),
            ("LOBBY_NO_BROWSER", "1".to_owned()),
        ];
        environment.extend(self.extra.iter().cloned());
        environment
    }

    /// Runs `companion-lobby` with `arguments` and this install's
    /// environment, and returns what it did.
    pub fn command(&self, arguments: &[&str]) -> Output {
        let mut command = Command::new(&self.exe);
        command.args(arguments).stdin(Stdio::null());
        for (name, value) in self.environment() {
            command.env(name, value);
        }
        command.output().expect("run companion-lobby")
    }

    /// Starts the daemon the way the sponsor does, through `open`.
    pub fn open(&self) {
        let output = self.command(&["open"]);
        assert!(
            output.status.success(),
            "open failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Stops the daemon.
    pub fn stop(&self) {
        let output = self.command(&["stop"]);
        assert!(
            output.status.success(),
            "stop failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Whether a daemon answers on this install's port.
    pub fn running(&self) -> bool {
        matches!(contact::probe(self.port), Probe::Lobby(_))
    }

    /// Attaches a mind as `companion` through the attach shim.
    pub fn attach(&self, companion: &str) -> Peer {
        let environment = self.environment();
        let pairs: Vec<(&str, &str)> = environment
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();
        Peer::spawn(&self.exe, &["attach", companion], &pairs)
    }

    /// An operator request from the page's own origin.
    pub fn operator(&self, method: &str, path: &str, body: &Value) -> Answer {
        let origin = format!("http://127.0.0.1:{}", self.port);
        let body = body.to_string();
        loopback::send(
            self.port,
            &Call {
                method,
                path,
                headers: &[("Origin", &origin)],
                body: body.as_bytes(),
                timeout: Duration::from_secs(30),
            },
        )
        .expect("an operator request")
    }

    /// A read of the operator API.
    pub fn json(&self, path: &str) -> Value {
        let answer =
            loopback::send(self.port, &Call::get(path, Duration::from_secs(10))).expect("a read");
        assert_eq!(answer.status, 200, "{path}");
        serde_json::from_slice(&answer.body).expect("JSON")
    }

    /// Creates a companion and returns its id.
    pub fn create(&self, moniker: &str) -> String {
        let answer = self.operator(
            "POST",
            "/api/companions",
            &json!({ "moniker": moniker, "displayName": moniker }),
        );
        assert_eq!(answer.status, 201, "{}", text(&answer));
        body(&answer)["companion"]["id"]
            .as_str()
            .expect("id")
            .to_owned()
    }

    /// Signs `companion` in with the passbook `authority` as `holder`, the way
    /// the sponsor's browser does, and returns the return page's answer.
    pub fn sign_in(&self, companion: &str, authority: &PassbookAuthority, holder: &str) -> Answer {
        authority.sign_in_as(holder);
        let started = self.operator(
            "POST",
            "/api/sign-ins",
            &json!({ "companion": companion, "kind": "passbook", "hint": authority.origin() }),
        );
        assert_eq!(started.status, 201, "{}", text(&started));
        let url = body(&started)["url"].as_str().expect("url").to_owned();
        let (origin, target) = split_url(&url);
        let approved = Outbound::new(AddressPolicy::LoopbackAllowed)
            .send(&origin, Request::new("GET", &target))
            .expect("the authority answers");
        assert_eq!(approved.status, 302, "the authority sends the browser back");
        let back = approved.header("location").expect("a location").to_owned();
        self.arrive(&back)
    }

    /// The sponsor's browser arriving at `location`, a sign-in's return
    /// route on this lobby: the route sends it straight on, carrying only
    /// the outcome, and this answers the page it is sent to.
    pub fn arrive(&self, location: &str) -> Answer {
        let (lobby, target) = split_url(location);
        assert_eq!(lobby.port(), self.port, "back to this lobby");
        let returned = Outbound::new(AddressPolicy::LoopbackAllowed)
            .send(&lobby, Request::new("GET", &target))
            .expect("the return route answers");
        assert_eq!(returned.status, 303, "the browser is sent straight on");
        assert!(returned.body.is_empty());
        let onward = returned.header("location").expect("a location").to_owned();
        assert!(
            onward.starts_with("/signin/done?outcome=")
                && !onward.contains("state")
                && !onward.contains("code"),
            "nothing the provider sent rides on: {onward}"
        );
        loopback::send(self.port, &Call::get(&onward, Duration::from_secs(10)))
            .expect("the page that says how it ended")
    }

    /// Signs `companion` in and returns the new credential's id.
    pub fn signed_in(
        &self,
        companion: &str,
        authority: &PassbookAuthority,
        holder: &str,
    ) -> String {
        let page = self.sign_in(companion, authority, holder);
        assert_eq!(page.status, 200, "{}", text(&page));
        self.json("/api/lobby")["credentials"]
            .as_array()
            .expect("credentials")
            .iter()
            .find(|credential| {
                credential["companion"] == companion && credential["state"] == "ready"
            })
            .and_then(|credential| credential["id"].as_str())
            .expect("a ready credential")
            .to_owned()
    }

    /// Asks the lobby to connect `companion` to `board` under `label`.
    pub fn grant(
        &self,
        companion: &str,
        credential: &str,
        board: &BoardServer,
        label: &str,
    ) -> Answer {
        self.operator(
            "POST",
            "/api/connections",
            &json!({
                "companion": companion,
                "credential": credential,
                "service": "board",
                "address": board.origin(),
                "label": label,
            }),
        )
    }

    /// Connects `companion` to `board` under `label` and returns the connection's id.
    pub fn connected(
        &self,
        companion: &str,
        credential: &str,
        board: &BoardServer,
        label: &str,
    ) -> String {
        let answer = self.grant(companion, credential, board, label);
        assert_eq!(answer.status, 201, "{}", text(&answer));
        body(&answer)["connection"]["id"]
            .as_str()
            .expect("id")
            .to_owned()
    }

    /// Every journal entry, oldest first.
    pub fn journal(&self) -> Vec<Value> {
        let mut entries = Vec::new();
        let mut path = "/api/journal?limit=100".to_owned();
        loop {
            let page = self.json(&path);
            entries.extend(page["entries"].as_array().expect("entries").iter().cloned());
            match page["next"].as_str() {
                Some(next) => path = format!("/api/journal?limit=100&before={next}"),
                None => break,
            }
        }
        entries.reverse();
        entries
    }

    /// The names of the files in the data directory.
    pub fn files(&self) -> Vec<String> {
        std::fs::read_dir(self.dir())
            .expect("the data directory")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    }

    /// The bytes of every file in the data directory that is not a sealed
    /// keyring file: the state file, the journal, the trace log and the rest.
    pub fn plain_files(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        for name in self.files() {
            if name.ends_with(".material") || name.ends_with(".issued") {
                continue;
            }
            bytes.extend(std::fs::read(self.dir().join(name)).unwrap_or_default());
        }
        bytes
    }
}

impl Drop for Install {
    fn drop(&mut self) {
        if self.running() {
            let _ = self.command(&["stop"]);
        }
    }
}

/// An answer's body as JSON, or `null`.
pub fn body(answer: &Answer) -> Value {
    serde_json::from_slice(&answer.body).unwrap_or(Value::Null)
}

/// An answer's body as text.
pub fn text(answer: &Answer) -> String {
    String::from_utf8_lossy(&answer.body).into_owned()
}

/// The origin and the path and query of an `http://` URL.
pub fn split_url(url: &str) -> (Address, String) {
    let rest = url.strip_prefix("http://").expect("an http URL");
    let slash = rest.find('/').unwrap_or(rest.len());
    let origin = Address::parse(
        &format!("http://{}", &rest[..slash]),
        AddressPolicy::LoopbackAllowed,
    )
    .expect("a loopback origin");
    let target = if slash == rest.len() {
        "/"
    } else {
        &rest[slash..]
    };
    (origin, target.to_owned())
}

fn initialize(peer: &mut Peer) {
    let answer = peer.ask(&json!({
        "jsonrpc": "2.0", "id": 0, "method": "initialize",
        "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "conformance", "version": "1" } }
    }));
    assert_eq!(
        answer["result"]["capabilities"]["tools"]["listChanged"],
        true
    );
    peer.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
}

fn tools(peer: &mut Peer, id: i64) -> Vec<Value> {
    let answer = peer.ask(&json!({ "jsonrpc": "2.0", "id": id, "method": "tools/list" }));
    answer["result"]["tools"].as_array().expect("tools").clone()
}

fn names(tools: &[Value]) -> Vec<String> {
    let mut names: Vec<String> = tools
        .iter()
        .map(|tool| tool["name"].as_str().expect("name").to_owned())
        .collect();
    names.sort();
    names
}

fn call(peer: &mut Peer, id: i64, name: &str, arguments: Value) -> Value {
    peer.ask(&call_frame(id, name, arguments))
}

fn call_frame(id: i64, name: &str, arguments: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": name, "arguments": arguments } })
}

/// The `at` values a tool takes.
fn at_of(tools: &[Value], name: &str) -> Value {
    tools
        .iter()
        .find(|tool| tool["name"] == name)
        .map(|tool| tool["inputSchema"]["properties"]["at"]["enum"].clone())
        .unwrap_or(Value::Null)
}

/// A companion with two board connections sees exactly `reach` and the
/// board's tools, whose `at` names both of the sponsor's labels, and no other
/// tool; the broker hands each board exactly the stamp the authority minted
/// for it, and the connection keeps only the badge the board issued, which
/// no projection shows. The catalog is the same after the daemon restarts.
pub fn two_boards_are_reached_through_one_set_of_tools(exe: &Path) {
    let authority = PassbookAuthority::start();
    let front = BoardServer::start(&authority);
    let back = BoardServer::start(&authority);
    back.add_post("moss", "The back board's first post.");
    let install = Install::new(exe, "two-boards");
    install.open();
    let ada = install.create("ada");
    let credential = install.signed_in(&ada, &authority, "holder-ada");
    install.connected(&ada, &credential, &front, "front");
    install.connected(&ada, &credential, &back, "back");

    let mut peer = install.attach("ada");
    initialize(&mut peer);
    let listed = tools(&mut peer, 1);
    assert_eq!(names(&listed), ["board_post", "board_read", "reach"]);
    for name in ["board_read", "board_post"] {
        assert_eq!(at_of(&listed, name), json!(["front", "back"]), "{name}");
        let tool = listed
            .iter()
            .find(|tool| tool["name"] == name)
            .expect("tool");
        assert!(
            tool["inputSchema"]["required"]
                .as_array()
                .expect("required")
                .contains(&json!("at")),
            "{name} needs at with two boards"
        );
    }
    assert_eq!(
        listed
            .iter()
            .find(|tool| tool["name"] == "board_read")
            .expect("read")["annotations"]["readOnlyHint"],
        true
    );
    let listing = Value::Array(listed.clone())
        .to_string()
        .to_ascii_lowercase();
    for word in [
        "session",
        "connect\"",
        "listcompanions",
        "whoami",
        "context",
    ] {
        assert!(!listing.contains(word), "{word} in {listing}");
    }

    let reach = call(&mut peer, 2, "reach", json!({}));
    let reached: Vec<Value> = reach["result"]["structuredContent"]["connections"]
        .as_array()
        .expect("connections")
        .clone();
    assert_eq!(
        reached
            .iter()
            .map(|connection| connection["at"].as_str().expect("at"))
            .collect::<Vec<_>>(),
        ["front", "back"]
    );

    let missing = call(&mut peer, 3, "board_read", json!({}));
    assert_eq!(missing["result"]["isError"], true, "two boards need at");
    let read = call(&mut peer, 4, "board_read", json!({ "at": "back" }));
    assert_eq!(read["result"]["isError"], false, "{read}");
    let said = read["result"]["content"][0]["text"].as_str().expect("text");
    assert!(said.contains("\"The back board's first post.\""), "{said}");
    assert_eq!(
        front.listener().count("/posts"),
        0,
        "only the board named by at"
    );
    assert_eq!(back.listener().count("/posts"), 1);

    for board in [&front, &back] {
        let presented = board.presented();
        assert_eq!(presented.len(), 1, "one admission");
        let stamp = presented[0].strip_prefix("Bearer ").expect("a stamp");
        let minted = authority.verify(stamp).expect("the authority minted it");
        assert_eq!(minted.audience, board.audience());
        assert_eq!(minted.holder, "holder-ada");
    }

    let mut secrets: Vec<String> = authority.keys();
    secrets.extend(authority.minted().into_iter().map(|minted| minted.stamp));
    secrets.extend(front.badges());
    secrets.extend(back.badges());
    let secrets: Vec<&str> = secrets.iter().map(String::as_str).collect();
    let mut projections = install.json("/api/lobby").to_string().into_bytes();
    projections.extend(Value::Array(install.journal()).to_string().into_bytes());
    projections.extend(peer.stdout_lines().join("\n").into_bytes());
    projections.extend(install.plain_files());
    assert_eq!(secrets_in(&projections, &secrets), Vec::<String>::new());

    peer.close_stdin();
    peer.wait_exit(Duration::from_secs(10));
    peer.stderr_text();
    install.stop();
    let mut again = install.attach("ada");
    initialize(&mut again);
    assert_eq!(
        tools(&mut again, 1),
        listed,
        "the catalog survives a restart"
    );
}

/// A second attachment, as another companion, sees none of the first's
/// tools and cannot call them, and nothing it tries reaches the board; the
/// first companion's account cannot be signed in for it.
pub fn another_companion_sees_none_of_the_first_ones_tools(exe: &Path) {
    let authority = PassbookAuthority::start();
    let board = BoardServer::start(&authority);
    let install = Install::new(exe, "isolation");
    install.open();
    let ada = install.create("ada");
    let bea = install.create("bea");
    let credential = install.signed_in(&ada, &authority, "holder-ada");
    install.connected(&ada, &credential, &board, "front");

    let mut first = install.attach("ada");
    initialize(&mut first);
    assert_eq!(
        names(&tools(&mut first, 1)),
        ["board_post", "board_read", "reach"]
    );
    let mut second = install.attach("bea");
    initialize(&mut second);
    assert_eq!(names(&tools(&mut second, 1)), ["reach"]);
    let reach = call(&mut second, 2, "reach", json!({}));
    assert_eq!(
        reach["result"]["structuredContent"]["connections"],
        json!([])
    );
    for (id, arguments) in [(3, json!({ "at": "front" })), (4, json!({}))] {
        let borrowed = call(&mut second, id, "board_read", arguments);
        assert_eq!(borrowed["error"]["code"], -32602, "{borrowed}");
    }
    assert_eq!(board.listener().count("/posts"), 0);

    let taken = install.sign_in(&bea, &authority, "holder-ada");
    assert_eq!(taken.status, 409, "an account belongs to one companion");
    assert!(
        !text(&taken).contains("holder-ada"),
        "the return page echoes nothing it was sent"
    );
}

/// A call is journaled as invoked, pending, before the service is reached,
/// and as settled after, with the attachment, the connection it went
/// through, the service and the tool, and with no content. The board holds
/// the call as it arrives, and the journal read while the board holds it
/// already has the pending entry and nothing settled.
pub fn a_call_is_journaled_pending_then_settled_through_its_connection(exe: &Path) {
    let authority = PassbookAuthority::start();
    let board = BoardServer::start(&authority);
    let install = Install::new(exe, "journal");
    install.open();
    let ada = install.create("ada");
    let credential = install.signed_in(&ada, &authority, "holder-ada");
    let connection = install.connected(&ada, &credential, &board, "front");
    let mut peer = install.attach("ada");
    initialize(&mut peer);
    let held = board.listener().hold("/posts");
    peer.send(&call_frame(
        1,
        "board_post",
        json!({ "text": "words only the board keeps" }),
    ));
    board.listener().wait_for("/posts", 1);
    let in_flight: Vec<(String, String)> = install
        .journal()
        .iter()
        .filter(|entry| entry["tool"] == "post")
        .map(|entry| {
            (
                entry["kind"].as_str().expect("kind").to_owned(),
                entry["outcome"].as_str().expect("outcome").to_owned(),
            )
        })
        .collect();
    assert_eq!(
        in_flight,
        [("invoked".to_owned(), "pending".to_owned())],
        "the pending entry is written before the board answers, and nothing is settled yet"
    );
    assert!(board.posts().is_empty(), "the board has not acted yet");
    drop(held);
    let posted = peer.answer(&json!(1));
    assert_eq!(posted["result"]["isError"], false, "{posted}");
    assert_eq!(board.posts().len(), 1);

    let journal = install.journal();
    let calls: Vec<&Value> = journal
        .iter()
        .filter(|entry| entry["tool"] == "post")
        .collect();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert_eq!(
        (calls[0]["kind"].as_str(), calls[0]["outcome"].as_str()),
        (Some("invoked"), Some("pending"))
    );
    assert_eq!(
        (calls[1]["kind"].as_str(), calls[1]["outcome"].as_str()),
        (Some("settled"), Some("done"))
    );
    for entry in &calls {
        assert_eq!(entry["initiator"], "mind");
        assert!(entry["attachment"].is_string());
        assert_eq!(entry["companion"], ada);
        assert_eq!(entry["connection"], connection);
        assert_eq!(entry["service"], "board");
    }
    assert!(calls[0]["seq"].as_u64() < calls[1]["seq"].as_u64());
    let grants: Vec<&Value> = journal
        .iter()
        .filter(|entry| entry["command"] == "grant_connection")
        .collect();
    assert_eq!(grants.len(), 2);
    assert_eq!(grants[1]["connection"], connection);
    let everything = Value::Array(journal).to_string();
    assert!(!everything.contains("words only the board keeps"));
    assert!(
        !install
            .plain_files()
            .windows(26)
            .any(|window| window == b"words only the board keeps")
    );
}

/// A connection revoked while a call is in flight turns the late result of
/// a read into a refusal and that of a write into an unknown outcome, never
/// a success, and the late text never reaches the mind; a call after the
/// revocation never reaches the board.
pub fn a_late_result_after_a_revocation_is_never_a_success(exe: &Path) {
    let authority = PassbookAuthority::start();
    let board = BoardServer::start(&authority);
    board.add_post("moss", "A late post no one may read.");
    let install = Install::new(exe, "late");
    install.open();
    let ada = install.create("ada");
    let credential = install.signed_in(&ada, &authority, "holder-ada");
    let first = install.connected(&ada, &credential, &board, "front");
    let mut peer = install.attach("ada");
    initialize(&mut peer);

    let held = board.listener().hold("/posts");
    peer.send(&call_frame(10, "board_read", json!({})));
    board.listener().wait_for("/posts", 1);
    let revoked = install.operator(
        "POST",
        &format!("/api/connections/{first}/revoke"),
        &json!({}),
    );
    assert_eq!(revoked.status, 200, "{}", text(&revoked));
    drop(held);
    let read = peer.answer(&json!(10));
    assert_eq!(read["result"]["isError"], true, "{read}");
    assert_eq!(read["result"]["structuredContent"]["outcome"], "revoked");
    assert!(!read.to_string().contains("A late post"), "{read}");

    let second = install.connected(&ada, &credential, &board, "front");
    peer.notification(LIST_CHANGED);
    let held = board.listener().hold("/posts");
    peer.send(&call_frame(
        11,
        "board_post",
        json!({ "text": "landed late" }),
    ));
    board.listener().wait_for("/posts", 2);
    let revoked = install.operator(
        "POST",
        &format!("/api/connections/{second}/revoke"),
        &json!({}),
    );
    assert_eq!(revoked.status, 200, "{}", text(&revoked));
    drop(held);
    let wrote = peer.answer(&json!(11));
    assert_eq!(wrote["result"]["isError"], true, "{wrote}");
    assert_eq!(
        wrote["result"]["structuredContent"]["outcome"],
        "outcome_unknown"
    );
    assert!(
        board.posts().iter().any(|post| post.text == "landed late"),
        "the write did land: unknown is the honest answer"
    );

    let after = call(&mut peer, 12, "board_read", json!({}));
    assert_eq!(after["error"]["code"], -32602, "the tool is gone");
    assert_eq!(
        board.listener().count("/posts"),
        2,
        "nothing more reached the board"
    );
    let settled: Vec<(String, String)> = install
        .journal()
        .iter()
        .filter(|entry| entry["kind"] == "settled")
        .map(|entry| {
            (
                entry["connection"].as_str().expect("connection").to_owned(),
                entry["outcome"].as_str().expect("outcome").to_owned(),
            )
        })
        .collect();
    assert_eq!(
        settled,
        [
            (first, "revoked".to_owned()),
            (second, "outcome_unknown".to_owned())
        ]
    );
}

/// Revoking one connection tells the host its tools changed and drops that
/// label from `at`; revoking the credential revokes every connection made
/// from it in one journaled command, deletes what they kept, empties the
/// catalog down to `reach` and tells the host again, once; then the
/// credential is revoked at its provider too, a step journaled on its own,
/// and the answer says the provider confirmed it.
pub fn revoking_the_credential_cascades_to_its_connections(exe: &Path) {
    let authority = PassbookAuthority::start();
    let front = BoardServer::start(&authority);
    let back = BoardServer::start(&authority);
    let install = Install::new(exe, "cascade");
    install.open();
    let ada = install.create("ada");
    install.create("bea");
    let credential = install.signed_in(&ada, &authority, "holder-ada");
    install.connected(&ada, &credential, &front, "front");
    let back_id = install.connected(&ada, &credential, &back, "back");
    let mut peer = install.attach("ada");
    initialize(&mut peer);
    let mut other = install.attach("bea");
    initialize(&mut other);
    assert_eq!(
        at_of(&tools(&mut peer, 1), "board_read"),
        json!(["front", "back"])
    );

    let revoked = install.operator(
        "POST",
        &format!("/api/connections/{back_id}/revoke"),
        &json!({}),
    );
    assert_eq!(revoked.status, 200, "{}", text(&revoked));
    peer.notification(LIST_CHANGED);
    let narrowed = tools(&mut peer, 2);
    assert_eq!(at_of(&narrowed, "board_read"), json!(["front"]));
    assert!(
        narrowed.iter().all(
            |tool| tool["inputSchema"]
                .get("required")
                .is_none_or(|required| !required
                    .as_array()
                    .expect("required")
                    .contains(&json!("at")))
        ),
        "one board needs no at"
    );

    let files_before = install.files();
    assert!(files_before.iter().any(|name| name.ends_with(".material")));
    let journal_before = install.journal().len();
    let revoked = install.operator(
        "POST",
        &format!("/api/credentials/{credential}/revoke"),
        &json!({}),
    );
    assert_eq!(revoked.status, 200, "{}", text(&revoked));
    assert_eq!(body(&revoked)["provider"], "confirmed");
    assert_eq!(authority.revoked().len(), 1, "the authority was asked");
    peer.notification(LIST_CHANGED);
    assert!(
        peer.notification_within(LIST_CHANGED, QUIET).is_none(),
        "the host hears the change once"
    );
    assert_eq!(names(&tools(&mut peer, 3)), ["reach"]);
    let refused = call(&mut peer, 4, "board_read", json!({}));
    assert_eq!(refused["error"]["code"], -32602);
    assert_eq!(front.listener().count("/posts"), 0);
    assert!(
        other.notification_within(LIST_CHANGED, QUIET).is_none(),
        "another companion's host hears nothing"
    );

    let lobby = install.json("/api/lobby");
    assert!(
        lobby["connections"]
            .as_array()
            .expect("connections")
            .iter()
            .all(|connection| connection["state"] == "revoked")
    );
    assert_eq!(lobby["credentials"][0]["state"], "revoked");
    assert!(
        install
            .files()
            .iter()
            .all(|name| !name.ends_with(".material") && !name.ends_with(".issued")),
        "what they kept is deleted"
    );
    let commands: Vec<(String, String)> = install.journal()[journal_before..]
        .iter()
        .filter_map(|entry| {
            Some((
                entry["command"].as_str()?.to_owned(),
                entry["outcome"].as_str()?.to_owned(),
            ))
        })
        .collect();
    assert_eq!(
        commands,
        [
            ("revoke_credential".to_owned(), "pending".to_owned()),
            ("revoke_credential".to_owned(), "done".to_owned()),
            ("revoke_at_provider".to_owned(), "pending".to_owned()),
            ("revoke_at_provider".to_owned(), "done".to_owned()),
        ],
        "one journaled command revokes the credential and its connections, and one at the provider"
    );
    let again = install.operator(
        "POST",
        &format!("/api/credentials/{credential}/revoke"),
        &json!({}),
    );
    assert_eq!(again.status, 409);
}
