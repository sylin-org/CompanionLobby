//! Secrets at rest.
//!
//! This module keeps the per-install attach secret: 32 random bytes written as
//! 64 hex digits to `attach.secret` in the owner-only data directory. The
//! shim presents it once per attachment, when it attaches; a process that
//! cannot read the data directory cannot attach. The daemon creates it on
//! first start; the attach shim reads it only after it has verified that the
//! daemon on the port is the lock holder for this directory.
//!
//! Every attachment then gets its own [`AttachmentToken`], which lives only in
//! the daemon's memory and in the shim that attached. Frames and the detach
//! carry that token, never the install secret, so a process that takes the
//! port after the daemon stops learns nothing a later daemon accepts.
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
        let text = random_hex()?;
        // Write a private temporary file completely, then publish it with a
        // hard link, which fails when the name exists. A crash at any point
        // leaves either no secret or a whole one, never a short file.
        let temporary = dir.join(format!("{ATTACH_SECRET_FILE}.{}.tmp", std::process::id()));
        let _ = std::fs::remove_file(&temporary);
        let published = write_new(&temporary, text.as_bytes())
            .and_then(|()| std::fs::hard_link(&temporary, &path));
        let _ = std::fs::remove_file(&temporary);
        match published {
            Ok(()) => Ok(Self(text)),
            // Another daemon start won the race; use what it wrote.
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Self::read(dir),
            Err(error) => Err(error),
        }
    }

    /// Reads an existing secret, refusing anything that is not 64 hex digits.
    pub fn read(dir: &Path) -> io::Result<Self> {
        let text = std::fs::read_to_string(secret_path(dir))?;
        let text = text.trim();
        if !is_token_text(text) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "attach.secret is damaged (not 64 hex digits); delete it, and the next daemon start creates a new one that each host's server uses once it restarts",
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
        same_text(&self.0, presented)
    }

    /// A public fingerprint of the install: the first 16 bytes of a domain-separated SHA-256.
    pub fn fingerprint(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"companion-lobby install fingerprint\0");
        hasher.update(self.0.as_bytes());
        hex(&hasher.finalize()[..16])
    }
}

/// The token of one attachment: 32 random bytes as 64 hex digits, held only
/// in the daemon's memory and by the shim that attached. Its `Debug` is
/// redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct AttachmentToken(String);

impl fmt::Debug for AttachmentToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AttachmentToken(..)")
    }
}

impl AttachmentToken {
    /// A fresh random token.
    pub fn generate() -> io::Result<Self> {
        random_hex().map(Self)
    }

    /// A token as the daemon sent it, refused unless it is 64 hex digits.
    pub fn parse(text: &str) -> Option<Self> {
        is_token_text(text).then(|| Self(text.to_ascii_lowercase()))
    }

    /// The token as the shim presents it.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Compares a presented token without stopping at the first difference.
    pub fn matches(&self, presented: &str) -> bool {
        same_text(&self.0, presented)
    }
}

fn random_hex() -> io::Result<String> {
    let mut bytes = [0u8; SECRET_BYTES];
    getrandom::fill(&mut bytes).map_err(|error| io::Error::other(error.to_string()))?;
    Ok(hex(&bytes))
}

fn is_token_text(text: &str) -> bool {
    text.len() == SECRET_BYTES * 2 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Whether two strings are equal, looking at every byte whatever differs first.
fn same_text(expected: &str, presented: &str) -> bool {
    let expected = expected.as_bytes();
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

/// Creates `path`, which must not exist yet (owner-only off Windows), and
/// writes `bytes` through to the disk.
fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
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
        assert!(error.to_string().contains("delete it"), "{error}");
        assert_eq!(
            std::fs::read_to_string(dir.join(ATTACH_SECRET_FILE)).expect("still there"),
            "not a secret"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn creating_the_secret_leaves_no_temporary_file_and_keeps_an_existing_one() {
        let dir = temp_dir("publish");
        // A temporary file left by a start that crashed mid-write changes nothing.
        let leftover = dir.join(format!("{ATTACH_SECRET_FILE}.{}.tmp", std::process::id()));
        std::fs::write(&leftover, "").expect("leftover");
        let first = AttachSecret::load_or_create(&dir).expect("create");
        let names: Vec<String> = std::fs::read_dir(&dir)
            .expect("list")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(names, vec![ATTACH_SECRET_FILE.to_owned()]);
        assert_eq!(AttachSecret::load_or_create(&dir).expect("read"), first);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn attachment_tokens_are_random_and_compared_whole() {
        let one = AttachmentToken::generate().expect("token");
        let two = AttachmentToken::generate().expect("token");
        assert_ne!(one, two);
        assert!(one.matches(one.expose()));
        assert!(!one.matches(two.expose()));
        assert!(!one.matches(&one.expose()[..63]));
        assert_eq!(format!("{one:?}"), "AttachmentToken(..)");
        assert_eq!(AttachmentToken::parse(one.expose()), Some(one.clone()));
        assert_eq!(AttachmentToken::parse("not a token"), None);
    }
}
