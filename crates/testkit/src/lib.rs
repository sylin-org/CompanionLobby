//! Wire fakes, fixtures and harnesses for tests.
//!
//! The test kit depends only on the contracts, never on the daemon or a real
//! adapter, so every crate can use it as a dev-dependency. It holds:
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
//! * [`secrets_in`], the secrets-grep every projection must pass.
//!
//! The fake provider and service roles and the synthetic adapters join it when
//! the daemon first reaches a service. Nothing here reaches the live network.

pub mod events;
pub mod hosts;
mod peer;

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
    fn free_ports_are_bindable() {
        let port = free_port();
        assert_ne!(port, 0);
        let _listener =
            std::net::TcpListener::bind(("127.0.0.1", port)).expect("listen on it again");
    }
}
