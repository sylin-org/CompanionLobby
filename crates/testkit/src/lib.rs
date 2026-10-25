//! Wire fakes, fixtures and harnesses for tests.
//!
//! The test kit depends only on the contracts and the outbound agent, never
//! on the daemon or a real adapter, so every crate can use it as a
//! dev-dependency. It holds:
//!
//! * [`TempHome`], a temporary data directory per test that is removed on
//!   drop, even when an assertion fails;
//! * [`free_port`], a loopback port nothing listens on;
//! * [`Peer`], the stdio harness that drives a spawned `companion-lobby
//!   attach` the way an MCP host does and matches answers by request id;
//! * [`hosts`], the MCP conversations recorded from real hosts, used as
//!   fixtures;
//! * [`PLANTED_ATTACH_SECRET`], an attach secret someone other than the
//!   daemon wrote, for tests that plant it in a data directory;
//! * [`events::EventStream`], a server-sent events client that follows a
//!   live feed the way the manager page does;
//! * [`secrets_in`], the secrets-grep every projection must pass;
//! * [`listener::Listener`], the scripted loopback listener every fake stands
//!   on, with a record of what it received and a gate that freezes a request
//!   mid-flight;
//! * the two synthetic adapters and their fakes: the [`passbook`] credential
//!   adapter with its authority and the [`board`] service adapter with its
//!   boards, each in its own words, which prove the host without any real
//!   provider or service;
//! * [`atproto`], a fake atproto authorization server and PDS, for the
//!   atproto credential adapter;
//! * [`space`], a fake of the Space the first service adapter speaks to,
//!   taking the proofs the fake PDS mints;
//! * [`world::LobbyWorld`], all of these at once on loopback, which the
//!   `lobby-world` command runs for walking the sponsor's journey by hand.
//!
//! Nothing here reaches the live network.

pub mod atproto;
pub mod board;
pub mod events;
pub mod hosts;
pub mod listener;
pub mod passbook;
mod peer;
pub mod space;
pub mod world;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

pub use peer::Peer;

/// An attach secret as plain hex text, known to whoever planted it: the
/// daemon must never let it attach anyone.
pub const PLANTED_ATTACH_SECRET: &str = include_str!("../fixtures/planted/attach.secret");

/// A temporary directory, removed with everything in it on drop.
#[derive(Debug)]
pub struct TempHome {
    path: PathBuf,
}

impl TempHome {
    /// Creates a fresh, empty directory whose name starts with `label`.
    pub fn new(label: &str) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.subsec_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "lobby-{label}-{}-{}-{nanos}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("create a temporary directory");
        Self { path }
    }

    /// The directory.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        // A process that just exited may hold a handle for a moment longer.
        for _ in 0..20 {
            if std::fs::remove_dir_all(&self.path).is_ok() || !self.path.exists() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}

/// The secrets among `secrets` that appear anywhere in `haystack`, in lower
/// or upper case, each named by its first eight characters only, so a failing
/// test never prints a whole secret.
pub fn secrets_in(haystack: &[u8], secrets: &[&str]) -> Vec<String> {
    let text = String::from_utf8_lossy(haystack).to_ascii_lowercase();
    secrets
        .iter()
        .filter(|secret| !secret.is_empty() && text.contains(&secret.to_ascii_lowercase()))
        .map(|secret| format!("{}..", secret.chars().take(8).collect::<String>()))
        .collect()
}

/// A token no other call in this process returns: `<label>-<n>-<nanos>`.
pub fn unique_token(label: &str) -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    format!("{label}-{}-{nanos:x}", NEXT.fetch_add(1, Ordering::Relaxed))
}

/// Percent-encodes everything but the RFC 3986 unreserved characters.
pub fn percent_encode(text: &str) -> String {
    text.bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

/// Decodes `%XX` escapes and `+` as a space; a malformed escape is kept as it is.
pub fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                let hex = |byte: u8| (byte as char).to_digit(16);
                match (hex(bytes[index + 1]), hex(bytes[index + 2])) {
                    (Some(high), Some(low)) => {
                        out.push((high * 16 + low) as u8);
                        index += 3;
                    }
                    _ => {
                        out.push(b'%');
                        index += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A loopback port that nothing listened on a moment ago.
pub fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
    listener.local_addr().expect("local address").port()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_temporary_home_is_removed_on_drop() {
        let home = TempHome::new("kit");
        let path = home.path().to_path_buf();
        std::fs::write(path.join("file"), b"x").expect("write");
        assert!(path.is_dir());
        drop(home);
        assert!(!path.exists());
    }

    #[test]
    fn the_secrets_grep_finds_a_secret_in_either_case_and_names_it_partly() {
        let secret = "abcdef0123456789abcdef0123456789";
        let found = secrets_in(
            b"{\"x\":\"ABCDEF0123456789ABCDEF0123456789\"}",
            &[secret, "other"],
        );
        assert_eq!(found, vec!["abcdef01..".to_owned()]);
        assert!(secrets_in(b"nothing here", &[secret]).is_empty());
    }

    #[test]
    fn percent_encoding_round_trips() {
        let text = "http://127.0.0.1:5219/signin?x=a b&y=é";
        let encoded = percent_encode(text);
        assert!(
            encoded
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-._~%".contains(&byte))
        );
        assert_eq!(percent_decode(&encoded), text);
        assert_eq!(percent_decode("a+b%2"), "a b%2");
        assert_ne!(unique_token("t"), unique_token("t"));
    }

    #[test]
    fn free_ports_are_bindable() {
        let port = free_port();
        assert_ne!(port, 0);
        let _listener =
            std::net::TcpListener::bind(("127.0.0.1", port)).expect("listen on it again");
    }
}
