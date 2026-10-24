//! Secrets at rest.
//!
//! This module keeps the per-install attach secret: 32 random bytes as 64 hex
//! digits in `attach.secret` in the owner-only data directory. On Windows the
//! file holds those digits sealed with DPAPI in the current user's scope, so
//! a copy of it is useless to any other account or machine; elsewhere it holds
//! them as text with mode 0600. The shim presents the secret once per
//! attachment, when it attaches; a process that cannot read the data
//! directory cannot attach.
//!
//! The daemon reads the secret when it starts, after it holds the daemon
//! lock, and replaces it with a fresh one when it is missing, when it does
//! not unseal as this user's, or when the file grants access to anyone but
//! its owner: a secret someone else could have read or planted is never
//! used. The shim makes the same check before it reads the secret, and
//! refuses rather than replaces, since only the lock holder writes it.
//!
//! Every attachment then gets its own [`AttachmentToken`], which lives only in
//! the daemon's memory and in the shim that attached. Frames and the detach
//! carry that token, never the install secret, so a process that takes the
//! port after the daemon stops learns nothing a later daemon accepts.
//!
//! The daemon also reports a fingerprint derived from the secret, so the shim
//! can tell that the daemon on the port serves its own data directory without
//! the secret ever leaving the machine's files.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::store::atomic_write;

/// The secret's file name inside the data directory.
pub const ATTACH_SECRET_FILE: &str = "attach.secret";

const SECRET_BYTES: usize = 32;

/// What the attach secret is sealed for; a blob sealed for anything else
/// does not unseal as the secret.
#[cfg(windows)]
const SEAL_PURPOSE: &[u8] = b"companion-lobby attach secret\0";

/// The per-install attach secret. Its `Debug` is redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct AttachSecret(String);

impl fmt::Debug for AttachSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AttachSecret(..)")
    }
}

/// Why the daemon wrote a new attach secret when it started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Renewal {
    /// There was none.
    Created,
    /// The file did not unseal as a secret this user sealed.
    Unreadable,
    /// The file granted access to someone other than its owner.
    Exposed,
}

impl Renewal {
    /// A sentence for the trace log.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Created => "the install had no attach secret; a new one was written",
            Self::Unreadable => {
                "attach.secret did not unseal as one this user wrote; a new one replaced it"
            }
            Self::Exposed => {
                "attach.secret granted access to someone other than its owner; a new one replaced it"
            }
        }
    }
}

impl AttachSecret {
    /// Reads the secret for the daemon, which holds the daemon lock, writing a
    /// fresh one when there is none or the one there cannot be trusted.
    pub fn load_or_renew(dir: &Path) -> io::Result<(Self, Option<Renewal>)> {
        let renewal = match Self::read(dir) {
            Ok(secret) => return Ok((secret, None)),
            Err(error) => match error.kind() {
                io::ErrorKind::NotFound => Renewal::Created,
                io::ErrorKind::InvalidData => Renewal::Unreadable,
                io::ErrorKind::PermissionDenied => Renewal::Exposed,
                _ => return Err(error),
            },
        };
        let secret = Self(random_hex()?);
        atomic_write(&secret_path(dir), &seal(&secret.0)?)?;
        Ok((secret, Some(renewal)))
    }

    /// The fingerprint of the secret in `dir`, read without the privacy
    /// check: comparing fingerprints sends the secret nowhere, so a command
    /// that only needs to recognize its daemon may do so even while the file
    /// is exposed. Anything that presents the secret uses [`AttachSecret::read`].
    pub fn installed_fingerprint(dir: &Path) -> io::Result<String> {
        Self::decode(&std::fs::read(secret_path(dir))?).map(|secret| secret.fingerprint())
    }

    /// Reads an existing secret, refusing a file that grants access to anyone
    /// but its owner (`PermissionDenied`) and one that does not unseal as 64
    /// hex digits this user sealed (`InvalidData`).
    pub fn read(dir: &Path) -> io::Result<Self> {
        let path = secret_path(dir);
        let bytes = std::fs::read(&path)?;
        if !is_private(dir, &path)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "attach.secret grants access to someone other than its owner; the daemon replaces it when it next starts",
            ));
        }
        Self::decode(&bytes)
    }

    fn decode(bytes: &[u8]) -> io::Result<Self> {
        let text = unseal(bytes)?;
        let text = text.trim();
        if !is_token_text(text) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "attach.secret does not hold a secret this user sealed; the daemon replaces it when it next starts",
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

#[cfg(windows)]
fn seal(text: &str) -> io::Result<Vec<u8>> {
    crate::win32::protect(text.as_bytes(), SEAL_PURPOSE)
}

#[cfg(not(windows))]
fn seal(text: &str) -> io::Result<Vec<u8>> {
    Ok(text.as_bytes().to_vec())
}

#[cfg(windows)]
fn unseal(bytes: &[u8]) -> io::Result<String> {
    let opened = crate::win32::unprotect(bytes, SEAL_PURPOSE).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "attach.secret does not unseal as a secret this user sealed on this machine; the daemon replaces it when it next starts",
        )
    })?;
    String::from_utf8(opened).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[cfg(not(windows))]
fn unseal(bytes: &[u8]) -> io::Result<String> {
    String::from_utf8(bytes.to_vec())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Whether the file at `path` is readable by its owner alone: on Windows its
/// owner is the current user and its DACL allows the current user and no one
/// else; elsewhere it has the directory's owner and mode bits for the owner
/// only.
#[cfg(windows)]
fn is_private(_dir: &Path, path: &Path) -> io::Result<bool> {
    crate::win32::is_private_file(path)
}

#[cfg(unix)]
fn is_private(dir: &Path, path: &Path) -> io::Result<bool> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let file = std::fs::metadata(path)?;
    let directory = std::fs::metadata(dir)?;
    Ok(file.uid() == directory.uid() && file.permissions().mode() & 0o077 == 0)
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

    /// A data directory as the daemon has it: private to its owner.
    fn private_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lobby-keyring-{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7().simple()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        crate::home::restrict_to_owner(&dir).expect("restrict");
        dir
    }

    #[test]
    fn the_secret_is_created_once_sealed_and_read_back() {
        let dir = private_dir("once");
        let (first, renewal) = AttachSecret::load_or_renew(&dir).expect("create");
        assert_eq!(renewal, Some(Renewal::Created));
        let (again, renewal) = AttachSecret::load_or_renew(&dir).expect("read");
        assert_eq!(renewal, None);
        assert_eq!(first, again);
        assert_eq!(AttachSecret::read(&dir).expect("read"), first);
        assert_eq!(first.expose().len(), 64);
        assert_eq!(format!("{first:?}"), "AttachSecret(..)");
        assert!(first.matches(first.expose()));
        assert!(!first.matches(&"0".repeat(64)));
        assert!(!first.matches(""));
        assert_eq!(first.fingerprint().len(), 32);
        assert!(!first.fingerprint().contains(first.expose()));
        let at_rest = std::fs::read(dir.join(ATTACH_SECRET_FILE)).expect("read");
        if cfg!(windows) {
            assert!(
                !String::from_utf8_lossy(&at_rest).contains(first.expose()),
                "the secret is sealed at rest"
            );
        }
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
        assert_eq!(
            names,
            vec![ATTACH_SECRET_FILE.to_owned()],
            "no temporary file"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_secret_that_does_not_unseal_as_ours_is_refused_to_the_shim_and_renewed_by_the_daemon() {
        let dir = private_dir("planted");
        let planted = "ab".repeat(32);
        let content = if cfg!(windows) {
            planted.clone()
        } else {
            "not a secret".to_owned()
        };
        std::fs::write(dir.join(ATTACH_SECRET_FILE), &content).expect("plant");
        let refused = AttachSecret::read(&dir).expect_err("the shim refuses it");
        assert_eq!(refused.kind(), io::ErrorKind::InvalidData);
        let (renewed, renewal) = AttachSecret::load_or_renew(&dir).expect("renew");
        assert_eq!(renewal, Some(Renewal::Unreadable));
        assert!(!renewed.matches(&planted));
        assert_ne!(
            std::fs::read(dir.join(ATTACH_SECRET_FILE)).expect("read"),
            content.as_bytes()
        );
        assert_eq!(AttachSecret::read(&dir).expect("read"), renewed);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(windows)]
    #[test]
    fn a_secret_others_may_read_is_refused_to_the_shim_and_renewed_by_the_daemon() {
        let dir = private_dir("exposed");
        let (first, _) = AttachSecret::load_or_renew(&dir).expect("create");
        let granted = std::process::Command::new("icacls")
            .arg(dir.join(ATTACH_SECRET_FILE))
            .args(["/grant", "*S-1-1-0:R"])
            .stdout(std::process::Stdio::null())
            .status()
            .expect("run icacls");
        assert!(granted.success());
        let refused = AttachSecret::read(&dir).expect_err("the shim refuses it");
        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
        let (renewed, renewal) = AttachSecret::load_or_renew(&dir).expect("renew");
        assert_eq!(renewal, Some(Renewal::Exposed));
        assert_ne!(renewed, first);
        assert!(crate::win32::is_private_file(&dir.join(ATTACH_SECRET_FILE)).expect("read"));
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
