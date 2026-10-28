//! The `lobby-world` command prints the origins of a world that answers.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use lobby_contracts::{Address, AddressPolicy};
use lobby_outbound::{Outbound, Request};

#[test]
fn lobby_world_prints_origins_that_answer_and_stops_when_its_input_closes() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_lobby-world"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("lobby-world starts");
    let mut lines = Vec::new();
    let mut reader = BufReader::new(child.stdout.take().expect("stdout"));
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).expect("a line");
        if line.trim().is_empty() {
            break;
        }
        lines.push(line.trim().to_owned());
    }
    let value = |name: &str| {
        lines
            .iter()
            .find_map(|line| line.strip_prefix(&format!("{name}=")))
            .unwrap_or_else(|| panic!("{name} in {lines:?}"))
            .to_owned()
    };
    let outbound = Outbound::new(AddressPolicy::LoopbackAllowed);
    for (name, path) in [
        (
            "LOBBY_AUTHSERVER",
            "/.well-known/oauth-authorization-server",
        ),
        ("board", "/about"),
        ("second-board", "/about"),
        ("space", "/.well-known/tangent-auth"),
        ("pds", "/.well-known/oauth-protected-resource"),
    ] {
        let origin = Address::parse(&value(name), AddressPolicy::LoopbackAllowed).expect(name);
        let answer = outbound
            .send(&origin, Request::new("GET", path))
            .expect("the fake answers");
        assert_eq!(answer.status, 200, "{name}");
    }
    for knob in [
        "/keys/brief",
        "/renewals/refuse",
        "/renewals/allow",
        "/keys/long",
        "/space/tokens-end",
        "/space/locked",
    ] {
        let origin =
            Address::parse(&value("control"), AddressPolicy::LoopbackAllowed).expect("control");
        let answer = outbound
            .send(&origin, Request::new("GET", knob))
            .expect("the control page answers");
        assert_eq!(answer.status, 200, "{knob}");
    }
    assert_eq!(value("passbook-holder"), "ada");
    assert_eq!(value("atmosphere-handle"), "ada.example.test");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(b"\n")
        .expect("ask it to stop");
    assert!(child.wait().expect("it stops").success());
}
