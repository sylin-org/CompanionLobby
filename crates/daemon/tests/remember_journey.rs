//! The lobby remembers: companions in the state file, attribution in the
//! journal, and a manager page that follows both.
//!
//! Every test drives the real `companion-lobby` binary against its own data
//! directory and port, and stops every daemon it started. Nothing is seeded
//! by writing the state file: companions exist because the operator API
//! created them.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use lobby_daemon::contact::{self, Probe};
use lobby_daemon::discovery::Discovery;
use lobby_daemon::keyring::AttachSecret;
use lobby_daemon::loopback::{self, Answer, Call};
use lobby_testkit::events::EventStream;
use lobby_testkit::{Peer, TempHome, free_port, secrets_in};
use serde_json::{Value, json};

const EXE: &str = env!("CARGO_BIN_EXE_companion-lobby");

/// One install under test: a data directory and a port, stopped on drop.
struct Install {
    home: TempHome,
    port: u16,
}

impl Install {
    fn new(label: &str) -> Self {
        Self {
            home: TempHome::new(label),
            port: free_port(),
        }
    }

    fn dir(&self) -> PathBuf {
        self.home.path().join("data")
    }

    fn environment(&self) -> Vec<(&'static str, String)> {
        vec![
            ("LOBBY_DATA_DIR", self.dir().to_string_lossy().into_owned()),
            ("LOBBY_PORT", self.port.to_string()),
            ("LOBBY_NO_BROWSER", "1".to_owned()),
        ]
    }

    fn command(&self, arguments: &[&str]) -> Output {
        let mut command = Command::new(EXE);
        command.args(arguments).stdin(Stdio::null());
        for (name, value) in self.environment() {
            command.env(name, value);
        }
        command.output().expect("run companion-lobby")
    }

    /// Starts the daemon the way the sponsor does, through `open`.
    fn open(&self) -> Discovery {
        let output = self.command(&["open"]);
        assert!(
            output.status.success(),
            "open failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        self.discovery().expect("the daemon answers after open")
    }

    fn discovery(&self) -> Option<Discovery> {
        match contact::probe(self.port) {
            Probe::Lobby(document) => Some(document),
            _ => None,
        }
    }

    fn attach(&self, companion: &str) -> Peer {
        let environment = self.environment();
        let pairs: Vec<(&str, &str)> = environment
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();
        Peer::spawn(Path::new(EXE), &["attach", companion], &pairs)
    }

    fn stop(&self) -> Output {
        self.command(&["stop"])
    }

    /// An operator request from the page's own origin.
    fn operator(&self, method: &str, path: &str, body: &Value) -> Answer {
        let origin = format!("http://127.0.0.1:{}", self.port);
        let body = body.to_string();
        loopback::send(
            self.port,
            &Call {
                method,
                path,
                headers: &[("Origin", &origin)],
                body: body.as_bytes(),
                timeout: Duration::from_secs(10),
            },
        )
        .expect("an operator request")
    }

    fn get(&self, path: &str) -> Answer {
        loopback::send(self.port, &Call::get(path, Duration::from_secs(10))).expect("a read")
    }

    fn json(&self, path: &str) -> Value {
        let answer = self.get(path);
        assert_eq!(answer.status, 200, "{path}");
        serde_json::from_slice(&answer.body).expect("JSON")
    }

    fn create(&self, moniker: &str, display_name: &str) -> Value {
        let answer = self.operator(
            "POST",
            "/api/companions",
            &json!({ "moniker": moniker, "displayName": display_name }),
        );
        assert_eq!(
            answer.status,
            201,
            "{}",
            String::from_utf8_lossy(&answer.body)
        );
        serde_json::from_slice::<Value>(&answer.body).expect("JSON")["companion"].clone()
    }

    fn monikers(&self) -> Vec<String> {
        self.json("/api/companions")["companions"]
            .as_array()
            .expect("companions")
            .iter()
            .map(|companion| companion["moniker"].as_str().expect("moniker").to_owned())
            .collect()
    }

    /// Every journal entry, oldest first, read through the fenced pages.
    fn journal(&self) -> Vec<Value> {
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

    fn secret(&self) -> AttachSecret {
        AttachSecret::read(&self.dir()).expect("the install has an attach secret")
    }

    /// Attaches the way the shim does, returning the attachment and its token.
    fn attach_directly(&self, companion: &str) -> (String, String) {
        let bearer = format!("Bearer {}", self.secret().expose());
        let body = json!({ "companion": companion }).to_string();
        let answer = loopback::send(
            self.port,
            &Call {
                method: "POST",
                path: "/mcp/attach",
                headers: &[("Authorization", &bearer)],
                body: body.as_bytes(),
                timeout: Duration::from_secs(10),
            },
        )
        .expect("attach");
        assert_eq!(
            answer.status,
            201,
            "{}",
            String::from_utf8_lossy(&answer.body)
        );
        let answer: Value = serde_json::from_slice(&answer.body).expect("JSON");
        (
            answer["attachment"].as_str().expect("id").to_owned(),
            answer["token"].as_str().expect("token").to_owned(),
        )
    }
}

impl Drop for Install {
    fn drop(&mut self) {
        if self.discovery().is_some() {
            let _ = self.stop();
        }
    }
}

fn initialize(id: i64) -> Value {
    json!({
        "jsonrpc": "2.0", "id": id, "method": "initialize",
        "params": { "protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": { "name": "journey", "version": "0" } }
    })
}

fn reach_call(id: i64) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": "reach", "arguments": {} } })
}

fn sequences(entries: &[Value]) -> Vec<u64> {
    entries
        .iter()
        .map(|entry| entry["seq"].as_u64().expect("seq"))
        .collect()
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn companions_and_their_journal_survive_a_daemon_killed_outright() {
    let install = Install::new("killed");
    let first = install.open();
    let ada = install.create("ada", "Ada");
    let before = install.journal();
    assert_eq!(before.len(), 2, "pending, then done");

    // The daemon is killed without a chance to clean up.
    let pid = first.lock_holder_pid.to_string();
    let killed = if cfg!(windows) {
        Command::new("taskkill")
            .args(["/F", "/PID", &pid])
            .stdout(Stdio::null())
            .status()
    } else {
        Command::new("kill").args(["-KILL", &pid]).status()
    };
    assert!(killed.expect("kill the daemon").success());
    wait_until("the killed daemon lets go of its lock", || {
        !lobby_daemon::lock::is_held(&install.dir()).expect("lock probe")
    });

    // A host's shim starts the next daemon: the lock left nothing behind
    // that blocks it, and it remembers the companion.
    let mut host = install.attach("ada");
    assert_eq!(
        host.ask(&initialize(0))["result"]["serverInfo"]["name"],
        "companion-lobby"
    );
    let reach = host.ask(&reach_call(1));
    assert_eq!(
        reach["result"]["structuredContent"]["companion"]["id"],
        ada["id"]
    );
    // The sponsor's own command meets that same daemon.
    let shim_started = install.discovery().expect("the shim started a daemon");
    assert_ne!(shim_started.lock_holder_pid, first.lock_holder_pid);
    assert_eq!(install.open().lock_holder_pid, shim_started.lock_holder_pid);
    host.close_stdin();
    assert_eq!(
        host.wait_exit(Duration::from_secs(20))
            .and_then(|status| status.code()),
        Some(0)
    );

    // The journal carries on after the highest number it holds.
    install.create("bea", "Bea");
    let after = install.journal();
    let numbers = sequences(&after);
    assert!(
        numbers.windows(2).all(|pair| pair[1] > pair[0]),
        "{numbers:?}"
    );
    assert_eq!(&after[..before.len()], &before[..]);
    let store = before[0]["id"]
        .as_str()
        .expect("id")
        .split(':')
        .next()
        .expect("store");
    assert!(
        after
            .iter()
            .all(|entry| entry["id"] == format!("{store}:{}", entry["seq"]))
    );
    assert_eq!(install.monikers(), ["ada", "bea"]);
}

#[test]
fn a_second_daemon_on_the_same_directory_is_refused() {
    let install = Install::new("second");
    let running = install.open();
    let refused = install.command(&["run"]);
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("already runs"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    // Another port changes nothing: the directory decides.
    let mut command = Command::new(EXE);
    command.arg("run").stdin(Stdio::null());
    for (name, value) in install.environment() {
        command.env(name, value);
    }
    command.env("LOBBY_PORT", free_port().to_string());
    let refused = command.output().expect("run");
    assert!(!refused.status.success());
    assert_eq!(
        install.discovery().expect("still running").lock_holder_pid,
        running.lock_holder_pid
    );
}

#[test]
fn files_the_lobby_did_not_write_are_never_read_and_a_state_file_it_cannot_read_is_left_alone() {
    let install = Install::new("foreign");
    install.open();
    install.create("ada", "Ada");
    assert!(install.stop().status.success());

    // A state file of an older implementation sits in the private directory.
    let older = install.dir().join("state.json");
    let older_bytes = br#"{"schemaVersion":1,"companions":{"x":{"handle":"mallory"}}}"#;
    std::fs::write(&older, older_bytes).expect("plant an older state file");
    #[cfg(windows)]
    let held = {
        use std::os::windows::fs::OpenOptionsExt;
        // No sharing at all: a daemon that tried to read it would fail.
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&older)
            .expect("hold the older file")
    };
    install.open();
    assert_eq!(
        install.monikers(),
        ["ada"],
        "nothing came from the older file"
    );
    #[cfg(windows)]
    drop(held);
    assert_eq!(std::fs::read(&older).expect("read"), older_bytes);
    assert!(install.stop().status.success());

    // A state file this build does not read stops the daemon and stays as it was.
    let state = install.dir().join("lobby.json");
    let mut newer: Value =
        serde_json::from_slice(&std::fs::read(&state).expect("read")).expect("JSON");
    newer["schema"] = json!(2);
    let newer = serde_json::to_vec_pretty(&newer).expect("encode");
    std::fs::write(&state, &newer).expect("write");
    let refused = install.command(&["run"]);
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(stderr.contains("has schema 2"), "{stderr}");
    assert!(stderr.contains("left as it is"), "{stderr}");
    assert_eq!(std::fs::read(&state).expect("read"), newer);

    // A directory inside .companion-lobby is refused before anything is made.
    let refused_dir = install.home.path().join(".companion-lobby").join("data");
    let mut command = Command::new(EXE);
    command
        .arg("run")
        .stdin(Stdio::null())
        .env("LOBBY_DATA_DIR", &refused_dir)
        .env("LOBBY_PORT", free_port().to_string());
    let refused = command.output().expect("run");
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains(".companion-lobby"));
    assert!(!install.home.path().join(".companion-lobby").exists());
}

#[cfg(windows)]
#[test]
fn a_replace_while_another_handle_holds_the_state_file_neither_truncates_nor_corrupts() {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 1;

    let install = Install::new("held");
    install.open();
    install.create("ada", "Ada");
    let state = install.dir().join("lobby.json");
    let before = std::fs::read(&state).expect("read");

    // A reader that does not let the file be replaced: the commit waits,
    // then fails closed and changes nothing.
    let holder = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(&state)
        .expect("hold the state file");
    let refused = install.operator(
        "POST",
        "/api/companions",
        &json!({ "moniker": "bea", "displayName": "Bea" }),
    );
    assert_eq!(refused.status, 503);
    let refused: Value = serde_json::from_slice(&refused.body).expect("JSON");
    assert_eq!(refused["outcome"], "temporarily_unavailable");
    assert_eq!(std::fs::read(&state).expect("read"), before);
    assert_eq!(install.monikers(), ["ada"], "the state rolled back");
    drop(holder);
    let journal = install.journal();
    let last = &journal[journal.len() - 1];
    assert_eq!(last["command"], "create_companion");
    assert_eq!(last["outcome"], "temporarily_unavailable");

    // A reader that allows replacing keeps reading the old file whole.
    let mut reader = std::fs::File::open(&state).expect("open a reader");
    install.create("bea", "Bea");
    let mut seen = Vec::new();
    reader.read_to_end(&mut seen).expect("read");
    assert_eq!(seen, before);
    let now: Value = serde_json::from_slice(&std::fs::read(&state).expect("read")).expect("JSON");
    assert_eq!(now["schema"], 1);
    assert_eq!(install.monikers(), ["ada", "bea"]);
    let leftovers: Vec<String> = std::fs::read_dir(install.dir())
        .expect("list")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[cfg(windows)]
#[test]
fn a_journal_that_cannot_be_written_stops_a_command_before_it_changes_anything() {
    use std::os::windows::fs::OpenOptionsExt;

    let install = Install::new("journal-held");
    install.open();
    install.create("ada", "Ada");
    let state = install.dir().join("lobby.json");
    let before = std::fs::read(&state).expect("read");
    let journal = install.dir().join("journal.jsonl");
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&journal)
        .expect("hold the journal");

    let refused = install.operator(
        "POST",
        "/api/companions",
        &json!({ "moniker": "bea", "displayName": "Bea" }),
    );
    assert_eq!(refused.status, 503);
    assert_eq!(
        std::fs::read(&state).expect("read"),
        before,
        "nothing was committed"
    );
    assert_eq!(install.monikers(), ["ada"]);
    // An attach is a command too: without its entry it does not happen.
    let bearer = format!("Bearer {}", install.secret().expose());
    let attach = loopback::send(
        install.port,
        &Call {
            method: "POST",
            path: "/mcp/attach",
            headers: &[("Authorization", &bearer)],
            body: br#"{"companion":"ada"}"#,
            timeout: Duration::from_secs(10),
        },
    )
    .expect("attach");
    assert_eq!(attach.status, 503);
    let lobby = install.json("/api/lobby");
    assert_eq!(lobby["journal"]["healthy"], false);
    assert_eq!(lobby["companions"][0]["attachments"], 0);

    drop(held);
    install.create("bea", "Bea");
    assert_eq!(install.json("/api/lobby")["journal"]["healthy"], true);
    assert_eq!(install.monikers(), ["ada", "bea"]);
}

#[test]
fn renames_and_removals_are_guarded_by_revision_and_removal_cascades_to_attachments() {
    let install = Install::new("revisions");
    install.open();
    let ada = install.create("ada", "Ada");
    let id = ada["id"].as_str().expect("id").to_owned();
    let path = format!("/api/companions/{id}");
    assert_eq!(ada["revision"], 1);
    let config: Value =
        serde_json::from_str(ada["hostConfig"].as_str().expect("config")).expect("JSON");
    assert_eq!(config["mcpServers"]["ada"]["args"], json!(["attach", id]));

    // A host attached by moniker, another by id.
    let mut by_moniker = install.attach("ada");
    assert!(by_moniker.ask(&initialize(0))["result"].is_object());
    let mut by_id = install.attach(&id);
    assert!(by_id.ask(&initialize(0))["result"].is_object());
    assert_eq!(
        install.json("/api/lobby")["companions"][0]["attachments"],
        2
    );

    // A rename that names an old revision is refused.
    let renamed = install.operator(
        "PATCH",
        &path,
        &json!({ "expectedRevision": 1, "moniker": "ada-l", "displayName": "Ada Lovelace" }),
    );
    assert_eq!(renamed.status, 200);
    let stale = install.operator(
        "PATCH",
        &path,
        &json!({ "expectedRevision": 1, "displayName": "Stale" }),
    );
    assert_eq!(stale.status, 409);
    // The attachments carry on: they are bound to the id, not the moniker.
    assert_eq!(
        by_moniker.ask(&reach_call(1))["result"]["structuredContent"]["companion"]["moniker"],
        "ada-l"
    );
    // A host configured with the old moniker reaches nothing now; the id still does.
    let mut old_name = install.attach("ada");
    assert_eq!(
        old_name
            .wait_exit(Duration::from_secs(20))
            .and_then(|status| status.code()),
        Some(1)
    );
    let mut new_host = install.attach(&id);
    assert!(new_host.ask(&initialize(0))["result"].is_object());
    new_host.close_stdin();
    assert_eq!(
        new_host
            .wait_exit(Duration::from_secs(20))
            .and_then(|status| status.code()),
        Some(0)
    );

    // Removal names the revision, and refuses while minds are attached.
    assert_eq!(
        install
            .operator("DELETE", &path, &json!({ "expectedRevision": 1 }))
            .status,
        409
    );
    let attached = install.operator("DELETE", &path, &json!({ "expectedRevision": 2 }));
    assert_eq!(attached.status, 409);
    let attached: Value = serde_json::from_slice(&attached.body).expect("JSON");
    assert!(
        attached["message"]
            .as_str()
            .expect("message")
            .contains("2 live attachments"),
        "{attached}"
    );
    let removed = install.operator(
        "DELETE",
        &path,
        &json!({ "expectedRevision": 2, "cascade": true }),
    );
    assert_eq!(removed.status, 200);
    let removed: Value = serde_json::from_slice(&removed.body).expect("JSON");
    assert_eq!(removed["endedAttachments"], 2);
    assert!(install.monikers().is_empty());

    // The cascade ended both attachments; the minds hear that the lobby is gone.
    let after = by_moniker.ask(&reach_call(2));
    assert_eq!(after["error"]["code"], -32603, "{after}");
    let ended: Vec<Value> = install
        .journal()
        .into_iter()
        .filter(|entry| entry["command"] == "detach" && entry["initiator"] == "lobby")
        .map(|entry| entry["outcome"].clone())
        .collect();
    assert_eq!(
        ended,
        [
            json!("pending"),
            json!("done"),
            json!("pending"),
            json!("done")
        ],
        "each ended attachment is journaled pending, then settled"
    );
    for mut host in [by_moniker, by_id] {
        host.close_stdin();
        let _ = host.wait_exit(Duration::from_secs(20));
    }
}

/// A page following the live feed: it reloads the lobby on a reset and
/// applies every later item to what it loaded. It is the manager page's rule
/// written again in Rust, so what it proves is the daemon's side: the feed
/// and the snapshot positions agree. The page's own script is not run here.
struct Follower {
    stream: EventStream,
    companions: BTreeMap<String, Value>,
    position: u64,
}

impl Follower {
    fn new(install: &Install) -> Self {
        let mut follower = Self {
            stream: EventStream::open(install.port, "/api/events", None),
            companions: BTreeMap::new(),
            position: 0,
        };
        let first = follower.stream.next_event().expect("the feed starts");
        let first: Value = serde_json::from_str(&first.data).expect("JSON");
        assert_eq!(first["kind"], "reset", "a fresh page reloads first");
        follower.load(install);
        follower
    }

    fn load(&mut self, install: &Install) {
        let lobby = install.json("/api/lobby");
        self.companions = companions_of(&lobby);
        self.position = lobby["feed"]["position"].as_u64().expect("position");
    }

    /// Applies items until the follower has seen `target`.
    fn catch_up(&mut self, install: &Install, target: u64) {
        while self.position < target {
            let event = self.stream.next_event().expect("the feed carries on");
            let item: Value = serde_json::from_str(&event.data).expect("JSON");
            if item["kind"] == "reset" {
                self.load(install);
                continue;
            }
            let seq: u64 = event
                .id
                .as_deref()
                .and_then(|id| id.rsplit(':').next())
                .and_then(|seq| seq.parse().ok())
                .expect("an item names its position");
            if seq <= self.position {
                continue;
            }
            self.position = seq;
            match item["kind"].as_str() {
                Some("companion") => {
                    let companion = item["companion"].clone();
                    let id = companion["id"].as_str().expect("id").to_owned();
                    self.companions.insert(id, companion);
                }
                Some("companion_removed") => {
                    self.companions.remove(item["id"].as_str().expect("id"));
                }
                _ => {}
            }
        }
    }
}

fn companions_of(lobby: &Value) -> BTreeMap<String, Value> {
    lobby["companions"]
        .as_array()
        .expect("companions")
        .iter()
        .map(|companion| {
            (
                companion["id"].as_str().expect("id").to_owned(),
                companion.clone(),
            )
        })
        .collect()
}

/// Two followers of the feed, one joining late, hold the same lobby after
/// every change: the daemon's feed and snapshots converge.
#[test]
fn two_pages_following_the_feed_converge_after_every_change() {
    let install = Install::new("converge");
    install.open();
    let mut first = Follower::new(&install);
    install.create("ada", "Ada");
    let mut second = Follower::new(&install);

    let check = |first: &mut Follower, second: &mut Follower| {
        let lobby = install.json("/api/lobby");
        let target = lobby["feed"]["position"].as_u64().expect("position");
        first.catch_up(&install, target);
        second.catch_up(&install, target);
        let truth = companions_of(&lobby);
        assert_eq!(first.companions, truth);
        assert_eq!(second.companions, truth);
    };
    check(&mut first, &mut second);

    let bea = install.create("bea", "Bea");
    check(&mut first, &mut second);
    let bea_path = format!("/api/companions/{}", bea["id"].as_str().expect("id"));
    let renamed = install.operator(
        "PATCH",
        &bea_path,
        &json!({ "expectedRevision": 1, "displayName": "Bea Zoë" }),
    );
    assert_eq!(renamed.status, 200);
    check(&mut first, &mut second);
    let (attachment, token) = install.attach_directly("bea");
    check(&mut first, &mut second);
    assert_eq!(
        first.companions[bea["id"].as_str().expect("id")]["attachments"],
        1
    );
    let detached = loopback::send(
        install.port,
        &Call {
            method: "DELETE",
            path: &format!("/mcp/attach/{attachment}"),
            headers: &[("Authorization", &format!("Bearer {token}"))],
            body: &[],
            timeout: Duration::from_secs(10),
        },
    )
    .expect("detach");
    assert_eq!(detached.status, 204);
    check(&mut first, &mut second);
    let removed = install.operator("DELETE", &bea_path, &json!({ "expectedRevision": 2 }));
    assert_eq!(removed.status, 200);
    check(&mut first, &mut second);

    // A page that reconnects with the last position it saw continues from
    // there; one that names a position of another start reloads.
    let lobby = install.json("/api/lobby");
    let epoch = lobby["feed"]["epoch"].as_str().expect("epoch");
    let position = lobby["feed"]["position"].as_u64().expect("position");
    let mut resumed = EventStream::open(
        install.port,
        "/api/events",
        Some(&format!("{epoch}:{}", position - 1)),
    );
    let next = resumed.next_event().expect("the item after it");
    assert_eq!(next.id, Some(format!("{epoch}:{position}")));
    let mut stale = EventStream::open(install.port, "/api/events", Some("another-start:3"));
    let reset: Value =
        serde_json::from_str(&stale.next_event().expect("a reset").data).expect("JSON");
    assert_eq!(reset["kind"], "reset");
    // Stopping the daemon ends every feed, so the stop is not held up.
    assert!(install.stop().status.success());
    assert!(first.stream.next_within(Duration::from_secs(5)).is_none());
}

#[test]
fn the_page_opens_no_dialog_and_no_projection_carries_a_secret() {
    let install = Install::new("projections");
    install.open();
    let mut feed = EventStream::open(install.port, "/api/events", None);
    let ada = install.create("ada", "Ada");
    let mut host = install.attach("ada");
    assert!(host.ask(&initialize(0))["result"].is_object());
    assert!(host.ask(&reach_call(1))["result"].is_object());
    let (attachment, token) = install.attach_directly("ada");
    let secret = install.secret();
    let secrets = [secret.expose(), token.as_str()];

    // The shell: no dialog, no markup written from data.
    for path in ["/", "/journal", "/manager.js"] {
        let answer = install.get(path);
        assert_eq!(answer.status, 200, "{path}");
        let text = String::from_utf8_lossy(&answer.body);
        for blocking in [
            "alert(",
            "confirm(",
            "prompt(",
            "showModal(",
            "<dialog",
            "innerHTML",
        ] {
            assert!(!text.contains(blocking), "{path} uses {blocking}");
        }
    }
    let page_path = format!("/companion/{}", ada["id"].as_str().expect("id"));
    assert_eq!(install.get(&page_path).status, 200);

    // Every projection, every file in the data directory but the secret's
    // own, the feed and what the shim said: none holds a secret.
    let mut haystacks: Vec<(String, Vec<u8>)> = Vec::new();
    for path in [
        "/",
        "/manager.js",
        "/manager.css",
        "/api/lobby",
        "/api/companions",
        "/api/journal",
        "/api/discovery",
        page_path.as_str(),
    ] {
        haystacks.push((path.to_owned(), install.get(path).body));
    }
    let detached = loopback::send(
        install.port,
        &Call {
            method: "DELETE",
            path: &format!("/mcp/attach/{attachment}"),
            headers: &[("Authorization", &format!("Bearer {token}"))],
            body: &[],
            timeout: Duration::from_secs(10),
        },
    )
    .expect("detach");
    assert_eq!(detached.status, 204);
    host.close_stdin();
    assert_eq!(
        host.wait_exit(Duration::from_secs(20))
            .and_then(|status| status.code()),
        Some(0)
    );
    haystacks.push(("shim stderr".to_owned(), host.stderr_text().into_bytes()));
    wait_until("the feed carried the detaches", || {
        String::from_utf8_lossy(feed.bytes())
            .matches("\"detach\"")
            .count()
            >= 4
    });
    haystacks.push(("the live feed".to_owned(), feed.bytes().to_vec()));
    assert!(install.stop().status.success());
    for entry in std::fs::read_dir(install.dir()).expect("list") {
        let entry = entry.expect("entry");
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "attach.secret" {
            continue;
        }
        haystacks.push((name, std::fs::read(entry.path()).expect("read")));
    }
    assert!(haystacks.len() > 12, "the harness looked everywhere");
    for (name, haystack) in &haystacks {
        let found = secrets_in(haystack, &secrets);
        assert!(found.is_empty(), "{name} holds {found:?}");
    }
}

/// A raw request with no `Origin` shows that reads need none and changes do.
#[test]
fn reads_need_no_origin_and_changes_do() {
    let install = Install::new("origin");
    install.open();
    let port = install.port;
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    let body = r#"{"moniker":"ada"}"#;
    stream
        .write_all(
            format!(
                "POST /api/companions HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .expect("write");
    let mut bytes = Vec::new();
    let _ = stream.read_to_end(&mut bytes);
    assert_eq!(loopback::parse(&bytes).expect("answer").status, 403);
    assert!(install.monikers().is_empty());
    for path in ["/api/lobby", "/api/journal", "/api/companions"] {
        assert_eq!(install.get(path).status, 200, "{path}");
    }
    for (method, path) in [
        ("PUT", "/api/companions"),
        ("POST", "/api/lobby"),
        ("POST", "/journal"),
    ] {
        let answer = install.operator(method, path, &json!({}));
        assert_eq!(answer.status, 405, "{method} {path}");
    }
}
