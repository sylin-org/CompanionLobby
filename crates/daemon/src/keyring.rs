//! Secrets at rest.
//!
//! This module keeps the per-install attach secret: 32 random bytes written as
//! 64 hex digits to `attach.secret` in the owner-only data directory. Every
//! attachment presents it; a process that cannot read the data directory
//! cannot attach. The daemon creates it on first start; the attach shim reads
//! it only after it has verified that the daemon on the port is the lock
//! holder for this directory.
//!
//! The daemon also reports a fingerprint derived from the secret, so the shim
//! can tell that the daemon on the port serves its own data directory without
//! the secret ever leaving the machine's files.
//!
//! Sealed credential material and service-issued data join this module when
//! credentials arrive.

use std::fmt;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// The secret's file name inside the data directory.
pub const ATTACH_SECRET_FILE: &str = "attach.secret";

const SECRET_BYTES: usize = 32;

/// The per-install attach secret. Its `Debug` is redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct AttachSecret(String);

impl fmt::Debug for AttachSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AttachSecret(..)")
    }
}

impl AttachSecret {
    /// Reads the secret, creating it first if this install has none.
    pub fn load_or_create(dir: &Path) -> io::Result<Self> {
        let path = secret_path(dir);
        match Self::read(dir) {
            Ok(secret) => return Ok(secret),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let mut bytes = [0u8; SECRET_BYTES];
        getrandom::fill(&mut bytes).map_err(|error| io::Error::other(error.to_string()))?;
        let text = hex(&bytes);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(mut file) => {
                file.write_all(text.as_bytes())?;
                file.sync_all()?;
                Ok(Self(text))
            }
            // Another daemon start won the race; use what it wrote.
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Self::read(dir),
            Err(error) => Err(error),
        }
    }

    /// Reads an existing secret, refusing anything that is not 64 hex digits.
    pub fn read(dir: &Path) -> io::Result<Self> {
        let text = std::fs::read_to_string(secret_path(dir))?;
        let text = text.trim();
        if text.len() != SECRET_BYTES * 2 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "attach.secret is not 64 hex digits",
            ));
        }
        Ok(Self(text.to_ascii_lowercase()))
    }

    /// The secret as the shim presents it.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Compares a presented secret without stopping at the first difference.
    pub fn matches(&self, presented: &str) -> bool {
        let expected = self.0.as_bytes();
        let presented = presented.as_bytes();
        if expected.len() != presented.len() {
            return false;
        }
        expected
            .iter()
            .zip(presented)
            .fold(0u8, |difference, (a, b)| difference | (a ^ b))
            == 0
    }

    /// A public fingerprint of the install: the first 16 bytes of a domain-separated SHA-256.
    pub fn fingerprint(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"companion-lobby install fingerprint\0");
        hasher.update(self.0.as_bytes());
        hex(&hasher.finalize()[..16])
    }
}

fn secret_path(dir: &Path) -> PathBuf {
    dir.join(ATTACH_SECRET_FILE)
}

/// Lowercase hex of `bytes`.
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut text, byte| {
            let _ = write!(text, "{byte:02x}");
            text
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lobby-keyring-{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7().simple()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn the_secret_is_created_once_and_read_back() {
        let dir = temp_dir("once");
        let first = AttachSecret::load_or_create(&dir).expect("create");
        let again = AttachSecret::load_or_create(&dir).expect("read");
        assert_eq!(first, again);
        assert_eq!(first.expose().len(), 64);
        assert_eq!(format!("{first:?}"), "AttachSecret(..)");
        assert!(first.matches(first.expose()));
        assert!(!first.matches(&"0".repeat(64)));
        assert!(!first.matches(""));
        assert_eq!(first.fingerprint().len(), 32);
        assert!(!first.fingerprint().contains(first.expose()));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_damaged_secret_is_refused_not_replaced() {
        let dir = temp_dir("damaged");
        std::fs::write(dir.join(ATTACH_SECRET_FILE), "not a secret").expect("write");
        let error = AttachSecret::load_or_create(&dir).expect_err("refused");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(
            std::fs::read_to_string(dir.join(ATTACH_SECRET_FILE)).expect("still there"),
            "not a secret"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
