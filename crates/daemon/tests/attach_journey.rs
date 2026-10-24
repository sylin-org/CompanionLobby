//! A mind reaches a companion through `companion-lobby attach`.
//!
//! Every test drives the real `companion-lobby` binary: the shim a host
//! launches, the daemon it starts, `open` and `stop`. Each test has its own
//! data directory and port, and stops every daemon it started.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use lobby_daemon::contact::{self, Probe};
use lobby_daemon::discovery::Discovery;
use lobby_daemon::keyring::AttachSecret;
use lobby_daemon::loopback::{self, Call};
use lobby_testkit::{Peer, TempHome, free_port, hosts};
use serde_json::{Value, json};

const EXE: &str = env!("CARGO_BIN_EXE_companion-lobby");

/// One install under test: a data directory and a port, stopped on drop.
struct Install {
    home: TempHome,
    port: u16,
    exe: PathBuf,
}

impl Install {
    fn new(label: &str) -> Self {
        Self {
            home: TempHome::new(label),
            port: free_port(),
            exe: PathBuf::from(EXE),
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

    fn command(&self, exe: &Path, arguments: &[&str]) -> Output {
        let mut command = Command::new(exe);
        command.args(arguments).stdin(Stdio::null());
        for (name, value) in self.environment() {
            command.env(name, value);
        }
        command.output().expect("run companion-lobby")
    }

    /// Starts the daemon the way the sponsor does, through `open`.
    fn open(&self) -> Discovery {
        let output = self.command(&self.exe, &["open"]);
        assert!(
            output.status.success(),
            "open failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let printed = String::from_utf8_lossy(&output.stdout);
        assert_eq!(printed.trim(), format!("http://127.0.0.1:{}/", self.port));
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
        Peer::spawn(&self.exe, &["attach", companion], &pairs)
    }

    fn create(&self, moniker: &str, display_name: &str) -> Value {
        let body = json!({ "moniker": moniker, "displayName": display_name }).to_string();
        let origin = format!("http://127.0.0.1:{}", self.port);
        let answer = loopback::send(
            self.port,
            &Call {
                method: "POST",
                path: "/api/companions",
                headers: &[("Origin", &origin)],
                body: body.as_bytes(),
                timeout: Duration::from_secs(5),
            },
        )
        .expect("create a companion");
        assert_eq!(
            answer.status,
            201,
            "{}",
            String::from_utf8_lossy(&answer.body)
        );
        serde_json::from_slice(&answer.body).expect("JSON")
    }

    fn secret(&self) -> AttachSecret {
        AttachSecret::read(&self.dir()).expect("the install has an attach secret")
    }

    fn stop(&self) -> Output {
        self.command(&self.exe, &["stop"])
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

fn assert_pure_stdout(peer: &Peer) {
    for line in peer.stdout_lines() {
        let frame: Value = serde_json::from_str(line)
            .unwrap_or_else(|_| panic!("stdout carried a non-JSON line: {line}"));
        assert_eq!(
            frame["jsonrpc"], "2.0",
            "stdout carried a non-JSON-RPC line: {line}"
        );
    }
}

#[test]
fn the_first_attach_starts_the_daemon_detached_and_the_host_sees_stdout_close() {
    let install = Install::new("autostart");
    assert!(install.discovery().is_none(), "nothing runs yet");

    // No companion exists in a fresh daemon, so this attach is refused, but
    // only after the shim has started the daemon.
    let mut shim = install.attach("ada");
    let status = shim
        .wait_exit(Duration::from_secs(20))
        .expect("the shim exits");
    assert_eq!(status.code(), Some(1));
    assert!(
        shim.stdout_ends_within(Duration::from_secs(5)),
        "the host must see the shim's stdout close although the daemon it started keeps running"
    );
    assert!(
        shim.stdout_lines().is_empty(),
        "a refused attach writes nothing to stdout"
    );
    let stderr = shim.stderr_text();
    assert!(stderr.contains("cannot attach as ada"), "stderr: {stderr}");

    let document = install
        .discovery()
        .expect("the daemon keeps running after the shim exits");
    assert_ne!(document.lock_holder_pid, shim.pid());
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        install.discovery().map(|d| d.lock_holder_pid),
        Some(document.lock_holder_pid)
    );
    assert_eq!(
        document.build_id,
        lobby_daemon::discovery::build_id_of(Path::new(EXE)).expect("build id")
    );
}

#[test]
fn a_mind_lists_and_calls_reach_through_attach() {
    let install = Install::new("reach");
    install.open();
    let created = install.create("ada", "Ada");
    let id = created["companion"]["id"].as_str().expect("id").to_owned();

    let mut host = install.attach("ada");
    // A host that probes the stateless revision first falls back on "method not found".
    let probe = host.ask(&json!({
        "jsonrpc": "2.0", "id": "probe", "method": "server/discover",
        "params": { "_meta": { "io.modelcontextprotocol/protocolVersion": "2026-07-28" } }
    }));
    assert_eq!(probe["error"]["code"], -32601);
    let initialized = host.ask(&initialize(0));
    assert_eq!(initialized["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(
        initialized["result"]["serverInfo"]["name"],
        "companion-lobby"
    );
    host.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));

    let listed = host.ask(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }));
    let tools = listed["result"]["tools"].as_array().expect("tools");
    assert_eq!(
        tools.len(),
        1,
        "a companion with no connections sees reach alone"
    );
    assert_eq!(tools[0]["name"], "reach");

    let reached = host.ask(&reach_call(2));
    assert_eq!(reached["result"]["isError"], false);
    assert_eq!(
        reached["result"]["structuredContent"]["companion"]["moniker"],
        "ada"
    );
    assert_eq!(
        reached["result"]["structuredContent"]["companion"]["id"],
        id.as_str()
    );
    assert_eq!(
        reached["result"]["structuredContent"]["tools"],
        json!(["reach"])
    );

    assert_eq!(
        host.ask(&json!({ "jsonrpc": "2.0", "id": 3, "method": "ping" }))["result"],
        json!({})
    );

    // A line that is not JSON is answered locally and the stream goes on.
    host.send_raw("this is not json\n");
    let refused = host.next_frame();
    assert_eq!(refused["error"]["code"], -32700);
    assert!(refused["id"].is_null());
    assert_eq!(host.ask(&reach_call(4))["result"]["isError"], false);

    // Attaching by id works the same as by moniker.
    let mut by_id = install.attach(&id);
    by_id.ask(&initialize(0));
    assert_eq!(
        by_id.ask(&reach_call(1))["result"]["structuredContent"]["companion"]["moniker"],
        "ada"
    );
    by_id.close_stdin();

    host.close_stdin();
    assert_eq!(
        host.wait_exit(Duration::from_secs(10))
            .and_then(|s| s.code()),
        Some(0)
    );
    assert!(host.stdout_ends_within(Duration::from_secs(5)));
    assert_pure_stdout(&host);
    assert!(
        install.discovery().is_some(),
        "the daemon outlives every attachment"
    );
}

#[test]
fn two_hosts_attach_concurrently_each_as_its_own_companion() {
    let install = Install::new("concurrent");
    install.open();
    install.create("ada", "Ada");
    install.create("bea", "Bea");

    let mut first = install.attach("ada");
    let mut second = install.attach("bea");
    let mut third = install.attach("ada");
    for host in [&mut first, &mut second, &mut third] {
        host.ask(&initialize(0));
    }
    for round in 1..=5 {
        // Send to all three before reading any answer.
        first.send(&reach_call(round));
        second.send(&reach_call(round));
        third.send(&reach_call(round));
        let id = json!(round);
        assert_eq!(
            first.answer(&id)["result"]["structuredContent"]["companion"]["moniker"],
            "ada"
        );
        assert_eq!(
            second.answer(&id)["result"]["structuredContent"]["companion"]["moniker"],
            "bea"
        );
        assert_eq!(
            third.answer(&id)["result"]["structuredContent"]["companion"]["moniker"],
            "ada"
        );
    }
    for host in [&mut first, &mut second, &mut third] {
        host.close_stdin();
        assert_eq!(
            host.wait_exit(Duration::from_secs(10))
                .and_then(|s| s.code()),
            Some(0)
        );
        assert_pure_stdout(host);
    }
}

#[test]
fn the_recorded_claude_code_conversation_replays_through_the_shim() {
    let install = Install::new("replay");
    install.open();
    install.create("ada", "Ada");
    let recording = hosts::claude_code();
    let mut host = install.attach("ada");
    for frame in recording.sent() {
        host.send(frame);
        let Some(id) = frame.get("id") else {
            continue;
        };
        let answer = host.answer(id);
        let recorded = recording
            .answer_to(id)
            .expect("the recording has this answer");
        assert_eq!(
            answer.get("error").is_some(),
            recorded.get("error").is_some(),
            "{answer}"
        );
        assert_eq!(answer["error"]["code"], recorded["error"]["code"]);
        if frame["method"] == "tools/list" {
            assert_eq!(answer["result"]["tools"], recorded["result"]["tools"]);
        }
        if frame["method"] == "tools/call" {
            assert_eq!(answer["result"]["isError"], recorded["result"]["isError"]);
            assert_eq!(
                answer["result"]["structuredContent"]["tools"],
                recorded["result"]["structuredContent"]["tools"]
            );
        }
        if frame["method"] == "initialize" {
            assert_eq!(
                answer["result"]["protocolVersion"],
                recorded["result"]["protocolVersion"]
            );
        }
    }
    host.close_stdin();
    assert_eq!(
        host.wait_exit(Duration::from_secs(10))
            .and_then(|s| s.code()),
        Some(0)
    );
    assert_pure_stdout(&host);
}

/// Sends raw bytes and returns the head lines and the body.
fn raw(port: u16, request: &str) -> (u16, String, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    stream.write_all(request.as_bytes()).expect("write");
    let mut bytes = Vec::new();
    let _ = stream.read_to_end(&mut bytes);
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = loopback::parse(&bytes)
        .map(|answer| answer.status)
        .unwrap_or(0);
    (status, head.to_ascii_lowercase(), body.to_owned())
}

#[test]
fn the_envelope_refuses_foreign_hosts_browsers_and_missing_secrets() {
    let install = Install::new("envelope");
    install.open();
    let port = install.port;
    let secret = install.secret();
    let bearer = format!("Bearer {}", secret.expose());
    let attach_body = r#"{"companion":"ada"}"#;

    let hygiene = |head: &str| {
        assert!(head.contains("cache-control: no-store"), "{head}");
        assert!(head.contains("referrer-policy: no-referrer"), "{head}");
        assert!(head.contains("x-content-type-options: nosniff"), "{head}");
    };

    // A foreign Host is refused on a read, before anything else.
    for host in ["evil.test", "[::1]:5219", "127.0.0.1.evil.test"] {
        let (status, head, _) = raw(
            port,
            &format!("GET /api/discovery HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"),
        );
        assert_eq!(status, 403, "Host {host}");
        hygiene(&head);
        assert!(!head.contains("access-control-allow-origin"));
    }
    // localhost is sent to the one address the daemon serves.
    let (status, head, _) = raw(
        port,
        &format!(
            "GET /api/companions?x=1 HTTP/1.1\r\nHost: localhost:{port}\r\nConnection: close\r\n\r\n"
        ),
    );
    assert_eq!(status, 308);
    assert!(
        head.contains(&format!(
            "location: http://127.0.0.1:{port}/api/companions?x=1"
        )),
        "{head}"
    );

    // The discovery document is the one cross-origin read.
    let (status, head, body) = raw(
        port,
        &format!(
            "GET /api/discovery HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: http://page.test\r\nConnection: close\r\n\r\n"
        ),
    );
    assert_eq!(status, 200);
    hygiene(&head);
    assert!(head.contains("access-control-allow-origin: *"));
    assert!(!body.contains(secret.expose()));
    // A page reads only what finds the daemon, nothing that identifies the install.
    for evidence in ["Origin: http://page.test", "Sec-Fetch-Mode: cors"] {
        let (status, _, body) = raw(
            port,
            &format!(
                "GET /api/discovery HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{evidence}\r\nConnection: close\r\n\r\n"
            ),
        );
        assert_eq!(status, 200);
        let page: Value = serde_json::from_str(&body).expect("JSON");
        assert_eq!(page["product"], "companion-lobby");
        assert_eq!(page["discoveryVersion"], 1);
        assert_eq!(page["managerOrigin"], format!("http://127.0.0.1:{port}"));
        for private in ["storeFingerprint", "buildId", "lockHolderPid"] {
            assert!(page.get(private).is_none(), "{evidence} read {private}");
        }
    }
    let (_, _, body) = raw(
        port,
        &format!(
            "GET /api/discovery HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
        ),
    );
    let whole: Value = serde_json::from_str(&body).expect("JSON");
    assert_eq!(whole["storeFingerprint"], secret.fingerprint());
    // A Host naming another port is foreign: the daemon knows its own.
    let (status, _, _) = raw(
        port,
        &format!(
            "GET /api/discovery HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
            port.wrapping_add(1)
        ),
    );
    assert_eq!(status, 403);
    let (status, head, _) = raw(
        port,
        &format!(
            "GET /api/companions HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
        ),
    );
    assert_eq!(status, 200);
    assert!(!head.contains("access-control-allow-origin"));

    // A browser-style POST to the attach path is refused even with the secret.
    for evidence in [
        "Origin: http://evil.test",
        "Sec-Fetch-Site: cross-site",
        "Sec-Fetch-Mode: cors",
        "Referer: http://evil.test/",
    ] {
        let (status, head, _) = raw(
            port,
            &format!(
                "POST /mcp/attach HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{evidence}\r\nAuthorization: {bearer}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{attach_body}",
                attach_body.len()
            ),
        );
        assert_eq!(status, 403, "{evidence}");
        hygiene(&head);
    }
    // A headless POST without the secret, or with a wrong one, is refused.
    for authorization in [
        "",
        "Authorization: Bearer 0000\r\n",
        "Authorization: Basic abc\r\n",
    ] {
        let (status, _, _) = raw(
            port,
            &format!(
                "POST /mcp/attach HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{authorization}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{attach_body}",
                attach_body.len()
            ),
        );
        assert_eq!(status, 401, "{authorization:?}");
    }
    let (status, _, _) = raw(
        port,
        &format!(
            "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
        ),
    );
    assert_eq!(status, 401);

    // An attachment answers to its own token; the install secret is not one.
    install.create("ada", "Ada");
    let attached = loopback::send(
        port,
        &Call {
            method: "POST",
            path: "/mcp/attach",
            headers: &[("Authorization", &bearer)],
            body: attach_body.as_bytes(),
            timeout: Duration::from_secs(5),
        },
    )
    .expect("attach");
    assert_eq!(attached.status, 201);
    let attached: Value = serde_json::from_slice(&attached.body).expect("JSON");
    let attachment = attached["attachment"].as_str().expect("attachment id");
    let token = attached["token"].as_str().expect("attachment token");
    assert_ne!(token, secret.expose());
    let ping = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
    let frame = |authorization: &str, evidence: &str| {
        raw(
            port,
            &format!(
                "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{evidence}Authorization: {authorization}\r\nLobby-Attachment: {attachment}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{ping}",
                ping.len()
            ),
        )
        .0
    };
    assert_eq!(frame(&bearer, ""), 401, "the install secret is not a token");
    assert_eq!(frame(&format!("Bearer {}", "0".repeat(64)), ""), 401);
    // A browser-style POST to /mcp is refused even with the right token.
    for evidence in [
        "Origin: http://evil.test\r\n",
        "Sec-Fetch-Site: cross-site\r\n",
        "Sec-Fetch-Mode: no-cors\r\n",
        "Sec-Fetch-Dest: empty\r\n",
        "Referer: http://evil.test/\r\n",
    ] {
        assert_eq!(
            frame(&format!("Bearer {token}"), evidence),
            403,
            "{evidence}"
        );
    }
    assert_eq!(frame(&format!("Bearer {token}"), ""), 200);
    let detach = |authorization: &str| {
        raw(
            port,
            &format!(
                "DELETE /mcp/attach/{attachment} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: {authorization}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            ),
        )
        .0
    };
    assert_eq!(
        detach(&bearer),
        401,
        "the install secret ends no attachment"
    );
    assert_eq!(detach(&format!("Bearer {token}")), 204);
    assert_eq!(frame(&format!("Bearer {token}"), ""), 404);

    // Operator changes need JSON from the page's own origin.
    let create = r#"{"moniker":"mallory"}"#;
    for (headers, expected) in [
        (String::new(), 403),
        ("Origin: http://evil.test\r\n".to_owned(), 403),
        (
            format!("Origin: http://127.0.0.1:{port}\r\nSec-Fetch-Site: cross-site\r\n"),
            403,
        ),
        // The page's origin is built from the bound port, not from the Host.
        ("Origin: http://127.0.0.1\r\n".to_owned(), 403),
    ] {
        let (status, _, _) = raw(
            port,
            &format!(
                "POST /api/companions HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{headers}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{create}",
                create.len()
            ),
        );
        assert_eq!(status, expected, "{headers:?}");
    }
    let (status, _, _) = raw(
        port,
        &format!(
            "POST /api/companions HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: http://127.0.0.1:{port}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{create}",
            create.len()
        ),
    );
    assert_eq!(status, 403);
    let (status, _, body) = raw(
        port,
        &format!(
            "GET /api/companions HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
        ),
    );
    assert_eq!(status, 200);
    assert!(
        !body.contains("mallory"),
        "no refused request changed anything"
    );

    // A declared body over the limit is refused before it is read.
    let (status, _, _) = raw(
        port,
        &format!(
            "POST /api/companions HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: http://127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            2 * 1024 * 1024
        ),
    );
    assert_eq!(status, 413);
    // Headers over the limit are refused too.
    let (status, _, _) = raw(
        port,
        &format!(
            "GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-Padding: {}\r\nConnection: close\r\n\r\n",
            "a".repeat(9 * 1024)
        ),
    );
    assert_eq!(status, 431);
}

/// Reads one HTTP request: the head, then as much body as it declares.
fn read_request(stream: &mut TcpStream) -> String {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 16 * 1024];
    loop {
        let read = stream.read(&mut buffer).unwrap_or(0);
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
        let text = String::from_utf8_lossy(&bytes);
        if let Some((head, body)) = text.split_once("\r\n\r\n") {
            let declared = head
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
                .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if body.len() >= declared {
                break;
            }
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Serves `listener` from a thread: every request is recorded and answered
/// with `status` and `body`.
fn record_requests(
    listener: std::net::TcpListener,
    status: &'static str,
    body: String,
) -> std::sync::mpsc::Receiver<String> {
    let (seen_sender, seen) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let _ = seen_sender.send(read_request(&mut stream));
            let reply = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    seen
}

#[test]
fn a_process_that_takes_the_port_after_a_stop_never_receives_the_install_secret() {
    let install = Install::new("after-stop");
    install.open();
    install.create("ada", "Ada");
    let secret = install.secret();
    let mut host = install.attach("ada");
    assert_eq!(
        host.ask(&initialize(0))["result"]["serverInfo"]["name"],
        "companion-lobby"
    );

    // The daemon stops, and another process takes its port before any
    // daemon starts again.
    let stopped = install.stop();
    assert!(
        stopped.status.success(),
        "{}",
        String::from_utf8_lossy(&stopped.stderr)
    );
    let taker = std::net::TcpListener::bind(("127.0.0.1", install.port))
        .expect("the port is free once the daemon has stopped");
    let seen = record_requests(
        taker,
        "404 Not Found",
        r#"{"outcome":"not_found"}"#.to_owned(),
    );

    // The host's next frame goes to that process, and so does the detach.
    let failed = host.ask(&reach_call(1));
    assert_eq!(failed["error"]["code"], -32603, "{failed}");
    host.close_stdin();
    assert_eq!(
        host.wait_exit(Duration::from_secs(20))
            .and_then(|s| s.code()),
        Some(0)
    );
    assert_pure_stdout(&host);

    let requests: Vec<String> = seen.try_iter().collect();
    assert!(
        requests.iter().any(|r| r.starts_with("POST /mcp ")),
        "{requests:?}"
    );
    assert!(
        requests
            .iter()
            .any(|r| r.starts_with("DELETE /mcp/attach/")),
        "{requests:?}"
    );
    for request in &requests {
        assert!(
            !request.contains(secret.expose()),
            "the install secret reached the port's new owner: {request}"
        );
        assert!(
            !request.starts_with("POST /mcp/attach "),
            "the shim attached to an unverified process: {request}"
        );
    }
}

#[test]
fn a_process_squatting_on_the_port_never_receives_the_secret() {
    let install = Install::new("squatter");
    let real = install.open();
    install.create("ada", "Ada");

    // A squatter on another port that replays the real daemon's discovery
    // document, then records every request it gets.
    let squatter = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let squat_port = squatter.local_addr().expect("address").port();
    let document = serde_json::to_string(&real).expect("document");
    let seen = record_requests(squatter, "200 OK", document);

    let mut environment = install.environment();
    environment.retain(|(name, _)| *name != "LOBBY_PORT");
    environment.push(("LOBBY_PORT", squat_port.to_string()));
    let pairs: Vec<(&str, &str)> = environment
        .iter()
        .map(|(name, value)| (*name, value.as_str()))
        .collect();
    let mut shim = Peer::spawn(Path::new(EXE), &["attach", "ada"], &pairs);
    let status = shim
        .wait_exit(Duration::from_secs(20))
        .expect("the shim exits");
    assert_eq!(status.code(), Some(1));
    let stderr = shim.stderr_text();
    assert!(
        stderr.contains("the attach secret was not sent"),
        "stderr: {stderr}"
    );
    let secret = install.secret();
    let requests: Vec<String> = seen.try_iter().collect();
    assert!(!requests.is_empty(), "the shim probed the squatter");
    for request in &requests {
        assert!(
            !request.to_ascii_lowercase().contains("authorization"),
            "{request}"
        );
        assert!(!request.contains(secret.expose()));
        assert!(request.starts_with("GET /api/discovery "), "{request}");
    }
}

/// A copy of the binary whose bytes, and so whose build id, differ.
fn other_build(home: &Path) -> PathBuf {
    let directory = home.join("older");
    std::fs::create_dir_all(&directory).expect("directory");
    let copy = directory.join(Path::new(EXE).file_name().expect("file name"));
    let mut bytes = std::fs::read(EXE).expect("read the binary");
    bytes.extend_from_slice(b"\0another build\0");
    std::fs::write(&copy, bytes).expect("write the copy");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&copy, std::fs::Permissions::from_mode(0o755))
            .expect("make it executable");
    }
    copy
}

#[test]
fn a_daemon_of_another_build_is_replaced_and_stop_lets_go_of_its_executable() {
    let install = Install::new("rebuild");
    let older = other_build(install.home.path());

    // The older build starts its own daemon.
    let output = install.command(&older, &["open"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let before = install.discovery().expect("the older daemon answers");
    let current = lobby_daemon::discovery::build_id_of(Path::new(EXE)).expect("build id");
    assert_ne!(before.build_id, current);

    // This build's shim stops it and starts this build in its place.
    install.create("ada", "Ada");
    let mut shim = install.attach("ada");
    let status = shim
        .wait_exit(Duration::from_secs(30))
        .expect("the shim exits");
    assert_eq!(
        status.code(),
        Some(1),
        "the restarted daemon starts with no companions"
    );
    let after = install.discovery().expect("the new daemon answers");
    assert_eq!(after.build_id, current);
    assert_ne!(after.lock_holder_pid, before.lock_holder_pid);

    // The older executable is free: a rebuild could replace it.
    let mut removed = false;
    for _ in 0..40 {
        if std::fs::remove_file(&older).is_ok() {
            removed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(removed, "the stopped daemon still holds its executable");

    // `stop` drains this build's daemon too and returns once it has let go.
    let stopped = install.stop();
    assert!(
        stopped.status.success(),
        "{}",
        String::from_utf8_lossy(&stopped.stderr)
    );
    assert!(String::from_utf8_lossy(&stopped.stdout).contains("stopped"));
    assert!(install.discovery().is_none());
    assert!(!lobby_daemon::lock::is_held(&install.dir()).expect("lock probe"));
    let again = install.stop();
    assert!(again.status.success());
    assert!(String::from_utf8_lossy(&again.stdout).contains("no Companion Lobby daemon"));
}

#[cfg(windows)]
#[test]
fn a_host_that_kills_its_job_on_exit_leaves_the_daemon_running() {
    use win32job::{ExtendedLimitInfo, Job};

    let install = Install::new("job");
    // A job like the one Node-based hosts put their stdio servers in: closing
    // it kills every process inside, and breakaway is allowed.
    let mut limits = ExtendedLimitInfo::new();
    limits.limit_kill_on_job_close().limit_breakaway_ok();
    let job = Job::create_with_limit_info(&limits).expect("job object");

    let mut shim = install.attach("ada");
    job.assign_process(shim.child_handle() as isize)
        .expect("assign the shim to the job");
    let status = shim
        .wait_exit(Duration::from_secs(20))
        .expect("the shim exits");
    assert_eq!(status.code(), Some(1));
    let started = install.discovery().expect("the shim started the daemon");

    drop(job);
    std::thread::sleep(Duration::from_millis(500));
    let after = install
        .discovery()
        .expect("closing the host's job leaves the daemon running");
    assert_eq!(after.lock_holder_pid, started.lock_holder_pid);
}

#[cfg(windows)]
#[test]
fn the_daemon_narrows_a_widened_data_directory_again_when_it_starts() {
    use lobby_daemon::win32::is_owner_only;

    let install = Install::new("acl");
    install.open();
    assert!(is_owner_only(&install.dir()).expect("read the DACL"));
    let stopped = install.stop();
    assert!(stopped.status.success());

    // Someone grants Everyone read access while no daemon runs.
    let widened = Command::new("icacls")
        .arg(install.dir())
        .args(["/grant", "*S-1-1-0:(OI)(CI)R"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .expect("run icacls");
    assert!(widened.success());
    assert!(!is_owner_only(&install.dir()).expect("read the DACL"));

    install.open();
    assert!(
        is_owner_only(&install.dir()).expect("read the DACL"),
        "the daemon's start restricts the directory to its owner again"
    );
}

/// Attaches as `companion` the way the shim does, returning the attachment id and its token.
fn attach_directly(install: &Install, companion: &str) -> (String, String) {
    let bearer = format!("Bearer {}", install.secret().expose());
    let body = json!({ "companion": companion }).to_string();
    let answer = loopback::send(
        install.port,
        &Call {
            method: "POST",
            path: "/mcp/attach",
            headers: &[("Authorization", &bearer)],
            body: body.as_bytes(),
            timeout: Duration::from_secs(5),
        },
    )
    .expect("attach");
    assert_eq!(answer.status, 201);
    let answer: Value = serde_json::from_slice(&answer.body).expect("JSON");
    (
        answer["attachment"].as_str().expect("id").to_owned(),
        answer["token"].as_str().expect("token").to_owned(),
    )
}

#[test]
fn a_body_that_never_finishes_arriving_is_refused_in_bounded_time() {
    let install = Install::new("trickle");
    install.open();
    install.create("ada", "Ada");
    let (attachment, token) = attach_directly(&install, "ada");
    let port = install.port;
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .expect("timeout");
    let started = std::time::Instant::now();
    stream
        .write_all(
            format!(
                "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\nLobby-Attachment: {attachment}\r\nContent-Type: application/json\r\nContent-Length: 1000\r\nConnection: close\r\n\r\n{{\"jsonrpc\""
            )
            .as_bytes(),
        )
        .expect("write a head and the start of a body");
    let mut bytes = Vec::new();
    let _ = stream.read_to_end(&mut bytes);
    let elapsed = started.elapsed();
    let answer = loopback::parse(&bytes).expect("an answer");
    assert_eq!(answer.status, 408);
    assert!(
        elapsed < Duration::from_secs(15),
        "the body limit is 10 seconds, it took {elapsed:?}"
    );
    // The daemon still serves the attachment afterwards.
    let ping = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
    let (status, _, _) = raw(
        port,
        &format!(
            "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\nLobby-Attachment: {attachment}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{ping}",
            ping.len()
        ),
    );
    assert_eq!(status, 200);
}

#[test]
fn stop_lets_a_request_still_arriving_finish_before_the_daemon_exits() {
    let install = Install::new("drain");
    install.open();
    install.create("ada", "Ada");
    let (attachment, token) = attach_directly(&install, "ada");
    let port = install.port;
    let reach = reach_call(7).to_string();
    let (first, rest) = reach.split_at(reach.len() / 2);

    // A frame whose head has arrived and whose body is half way.
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .expect("timeout");
    stream
        .write_all(
            format!(
                "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\nLobby-Attachment: {attachment}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{first}",
                reach.len()
            )
            .as_bytes(),
        )
        .expect("write the head and half the body");
    std::thread::sleep(Duration::from_millis(300));

    // `stop` is asked while that frame is still arriving.
    let mut command = Command::new(&install.exe);
    command.arg("stop").stdin(Stdio::null());
    for (name, value) in install.environment() {
        command.env(name, value);
    }
    let stopper = std::thread::spawn(move || command.output().expect("run stop"));
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        !stopper.is_finished(),
        "stop returned while a request was still being served"
    );
    assert!(
        install.discovery().is_none(),
        "a stopping daemon accepts no new connection"
    );

    // The rest of the frame arrives, and it is answered in full.
    stream.write_all(rest.as_bytes()).expect("write the rest");
    let mut bytes = Vec::new();
    let _ = stream.read_to_end(&mut bytes);
    let answer = loopback::parse(&bytes).expect("an answer");
    assert_eq!(answer.status, 200);
    let frame: Value = serde_json::from_slice(&answer.body).expect("JSON");
    assert_eq!(frame["id"], 7);
    assert_eq!(
        frame["result"]["structuredContent"]["companion"]["moniker"],
        "ada"
    );

    let stopped = stopper.join().expect("stop");
    assert!(
        stopped.status.success(),
        "{}",
        String::from_utf8_lossy(&stopped.stderr)
    );
    assert!(String::from_utf8_lossy(&stopped.stdout).contains("stopped"));
    assert!(!lobby_daemon::lock::is_held(&install.dir()).expect("lock probe"));
}

#[test]
fn hosts_that_start_at_once_on_a_cold_install_meet_one_daemon() {
    let install = Install::new("cold");
    // Three hosts launch their shims at the same moment; nothing runs yet.
    let mut shims: Vec<Peer> = (0..3).map(|_| install.attach("ada")).collect();
    for shim in &mut shims {
        let status = shim
            .wait_exit(Duration::from_secs(30))
            .expect("the shim exits");
        assert_eq!(status.code(), Some(1));
        // Refused only because a fresh daemon has no companions: each shim
        // found, verified and reached the one daemon.
        let stderr = shim.stderr_text();
        assert!(stderr.contains("cannot attach as ada"), "stderr: {stderr}");
    }
    let document = install.discovery().expect("a daemon runs");
    let trace = std::fs::read_to_string(install.dir().join("trace.log")).expect("trace.log");
    let listening: Vec<&str> = trace
        .lines()
        .filter(|line| line.contains("listening on"))
        .collect();
    assert_eq!(listening.len(), 1, "{trace}");
    let record = lobby_daemon::lock::DaemonRecord::read(&install.dir()).expect("record");
    assert_eq!(record.pid, document.lock_holder_pid);
    let stopped = install.stop();
    assert!(
        stopped.status.success(),
        "{}
{}",
        String::from_utf8_lossy(&stopped.stderr),
        std::fs::read_to_string(install.dir().join("trace.log")).unwrap_or_default()
    );
}
