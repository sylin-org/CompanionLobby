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
//! used, and replacing one is a command the lobby journals. The shim makes the same check before it reads the secret, and
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
//!
//! Beside the secret the keyring keeps what adapters hand the daemon and only
//! they decode: each credential's material in `<credential id>.material` and
//! what a service issued for each connection in `<connection id>.issued`.
//! Each file is written atomically and sealed like the secret, with a purpose
//! of its own, so a file of one kind never opens as the other; it is deleted
//! when its credential or connection is revoked or removed.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use lobby_contracts::{ConnectionId, CredentialId, SealedIssued, SealedMaterial};
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

/// Why the daemon must write a new attach secret when it starts.
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
    /// Whether a secret that was there is replaced, which the lobby journals;
    /// an install's first secret replaces nothing.
    pub fn replaces_one(self) -> bool {
        !matches!(self, Self::Created)
    }

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
    /// Reads the secret for the daemon, which holds the daemon lock: the
    /// secret, or why [`AttachSecret::renew`] must write a fresh one because
    /// there is none or the one there cannot be trusted.
    pub fn load(dir: &Path) -> io::Result<Result<Self, Renewal>> {
        match Self::read(dir) {
            Ok(secret) => Ok(Ok(secret)),
            Err(error) => match error.kind() {
                io::ErrorKind::NotFound => Ok(Err(Renewal::Created)),
                io::ErrorKind::InvalidData => Ok(Err(Renewal::Unreadable)),
                io::ErrorKind::PermissionDenied => Ok(Err(Renewal::Exposed)),
                _ => Err(error),
            },
        }
    }

    /// Writes a fresh secret in place of whatever `dir` holds, owner-only and
    /// sealed. Only the daemon that holds the lock calls it.
    pub fn renew(dir: &Path) -> io::Result<Self> {
        let secret = Self(random_hex()?);
        atomic_write(&secret_path(dir), &seal(&secret.0)?)?;
        Ok(secret)
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

/// The suffix of a credential's material file.
pub const MATERIAL_SUFFIX: &str = ".material";
/// The suffix of a connection's issued-data file.
pub const ISSUED_SUFFIX: &str = ".issued";

/// What material is sealed for.
#[cfg(windows)]
const MATERIAL_PURPOSE: &[u8] = b"companion-lobby credential material\0";
/// What issued data is sealed for.
#[cfg(windows)]
const ISSUED_PURPOSE: &[u8] = b"companion-lobby connection issued data\0";

/// The file name of a credential's material.
pub fn material_file(id: CredentialId) -> String {
    format!("{id}{MATERIAL_SUFFIX}")
}

/// The file name of a connection's issued data.
pub fn issued_file(id: ConnectionId) -> String {
    format!("{id}{ISSUED_SUFFIX}")
}

/// Whether `name` is a material or issued-data file, or a temporary one
/// written on the way to it.
pub fn is_sealed_file(name: &str) -> bool {
    let sealed = |name: &str| {
        [MATERIAL_SUFFIX, ISSUED_SUFFIX].iter().any(|suffix| {
            name.strip_suffix(suffix)
                .is_some_and(|id| CredentialId::parse(id).is_some())
        })
    };
    sealed(name)
        || name
            .strip_suffix(".tmp")
            .and_then(|rest| rest.rsplit_once('.'))
            .is_some_and(|(file, _)| sealed(file) && crate::store::is_temporary_of(name, file))
}

/// Keeps `material` for the credential `id`, sealed.
pub fn write_material(dir: &Path, id: CredentialId, material: &SealedMaterial) -> io::Result<()> {
    atomic_write(
        &dir.join(material_file(id)),
        &seal_bytes(material.as_bytes(), Kept::Material)?,
    )
}

/// The material kept for the credential `id`.
pub fn read_material(dir: &Path, id: CredentialId) -> io::Result<SealedMaterial> {
    let sealed = std::fs::read(dir.join(material_file(id)))?;
    unseal_bytes(&sealed, Kept::Material).map(SealedMaterial::new)
}

/// Keeps `issued` for the connection `id`, sealed.
pub fn write_issued(dir: &Path, id: ConnectionId, issued: &SealedIssued) -> io::Result<()> {
    atomic_write(
        &dir.join(issued_file(id)),
        &seal_bytes(issued.as_bytes(), Kept::Issued)?,
    )
}

/// The issued data kept for the connection `id`.
pub fn read_issued(dir: &Path, id: ConnectionId) -> io::Result<SealedIssued> {
    let sealed = std::fs::read(dir.join(issued_file(id)))?;
    unseal_bytes(&sealed, Kept::Issued).map(SealedIssued::new)
}

/// Deletes a file the keyring kept; one already gone is not an error.
pub fn remove_kept(dir: &Path, name: &str) -> io::Result<()> {
    match std::fs::remove_file(dir.join(name)) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// Which kind of blob the keyring keeps.
#[derive(Clone, Copy)]
enum Kept {
    Material,
    Issued,
}

#[cfg(windows)]
fn seal_bytes(bytes: &[u8], kept: Kept) -> io::Result<Vec<u8>> {
    crate::win32::protect(bytes, purpose(kept))
}

#[cfg(windows)]
fn unseal_bytes(bytes: &[u8], kept: Kept) -> io::Result<Vec<u8>> {
    crate::win32::unprotect(bytes, purpose(kept))
}

#[cfg(windows)]
fn purpose(kept: Kept) -> &'static [u8] {
    match kept {
        Kept::Material => MATERIAL_PURPOSE,
        Kept::Issued => ISSUED_PURPOSE,
    }
}

/// Elsewhere a kept file is the blob itself, owner-only in an owner-only
/// directory.
#[cfg(not(windows))]
fn seal_bytes(bytes: &[u8], _kept: Kept) -> io::Result<Vec<u8>> {
    Ok(bytes.to_vec())
}

#[cfg(not(windows))]
fn unseal_bytes(bytes: &[u8], _kept: Kept) -> io::Result<Vec<u8>> {
    Ok(bytes.to_vec())
}

/// A fresh random token: 32 random bytes as 64 hex digits.
pub fn random_token() -> io::Result<String> {
    random_hex()
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

    /// What the daemon does when it starts: the secret, renewed when it must be.
    fn load_or_renew(dir: &Path) -> io::Result<(AttachSecret, Option<Renewal>)> {
        match AttachSecret::load(dir)? {
            Ok(secret) => Ok((secret, None)),
            Err(renewal) => Ok((AttachSecret::renew(dir)?, Some(renewal))),
        }
    }

    #[test]
    fn the_secret_is_created_once_sealed_and_read_back() {
        let dir = private_dir("once");
        let (first, renewal) = load_or_renew(&dir).expect("create");
        assert_eq!(renewal, Some(Renewal::Created));
        assert!(!Renewal::Created.replaces_one());
        assert!(Renewal::Unreadable.replaces_one() && Renewal::Exposed.replaces_one());
        let (again, renewal) = load_or_renew(&dir).expect("read");
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
        // Planted owner-only, as the daemon itself writes it, so only what
        // the file holds is wrong.
        crate::store::write_new(&dir.join(ATTACH_SECRET_FILE), content.as_bytes()).expect("plant");
        let refused = AttachSecret::read(&dir).expect_err("the shim refuses it");
        assert_eq!(refused.kind(), io::ErrorKind::InvalidData);
        let (renewed, renewal) = load_or_renew(&dir).expect("renew");
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
        let (first, _) = load_or_renew(&dir).expect("create");
        let granted = std::process::Command::new("icacls")
            .arg(dir.join(ATTACH_SECRET_FILE))
            .args(["/grant", "*S-1-1-0:R"])
            .stdout(std::process::Stdio::null())
            .status()
            .expect("run icacls");
        assert!(granted.success());
        let refused = AttachSecret::read(&dir).expect_err("the shim refuses it");
        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
        let (renewed, renewal) = load_or_renew(&dir).expect("renew");
        assert_eq!(renewal, Some(Renewal::Exposed));
        assert_ne!(renewed, first);
        assert!(crate::win32::is_private_file(&dir.join(ATTACH_SECRET_FILE)).expect("read"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_secret_others_may_read_is_refused_to_the_shim_and_renewed_by_the_daemon() {
        use std::os::unix::fs::PermissionsExt;
        let dir = private_dir("exposed");
        let planted = "ab".repeat(32);
        let path = dir.join(ATTACH_SECRET_FILE);
        std::fs::write(&path, &planted).expect("plant");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("open it to others");
        let refused = AttachSecret::read(&dir).expect_err("the shim refuses it");
        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
        let (renewed, renewal) = load_or_renew(&dir).expect("renew");
        assert_eq!(renewal, Some(Renewal::Exposed));
        assert!(!renewed.matches(&planted));
        let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
        assert_eq!(mode & 0o077, 0, "the renewed secret is owner-only");
        assert_eq!(AttachSecret::read(&dir).expect("read"), renewed);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn material_and_issued_data_are_kept_sealed_each_for_its_own_kind() {
        let dir = private_dir("kept");
        let credential = CredentialId::from_u128(7);
        let connection = ConnectionId::from_u128(7);
        let material = SealedMaterial::new(b"passbook key 0123".to_vec());
        let issued = SealedIssued::new(b"board badge 4567".to_vec());
        write_material(&dir, credential, &material).expect("material");
        write_issued(&dir, connection, &issued).expect("issued");
        assert_eq!(read_material(&dir, credential).expect("read"), material);
        assert_eq!(read_issued(&dir, connection).expect("read"), issued);
        if cfg!(windows) {
            let at_rest = std::fs::read(dir.join(material_file(credential))).expect("read");
            assert!(!String::from_utf8_lossy(&at_rest).contains("passbook key"));
            // The same bytes under the other kind's name do not open.
            std::fs::copy(
                dir.join(material_file(credential)),
                dir.join(issued_file(connection)),
            )
            .expect("copy");
            assert!(read_issued(&dir, connection).is_err());
        }
        for name in [material_file(credential), issued_file(connection)] {
            assert!(is_sealed_file(&name), "{name}");
            remove_kept(&dir, &name).expect("remove");
            remove_kept(&dir, &name).expect("removing twice is fine");
        }
        assert!(read_material(&dir, credential).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn only_sealed_file_names_are_recognized() {
        let id = CredentialId::from_u128(0xab).to_string();
        for name in [
            format!("{id}.material"),
            format!("{id}.issued"),
            format!("{id}.material.42-7.tmp"),
        ] {
            assert!(is_sealed_file(&name), "{name}");
        }
        for name in [
            "x.material".to_owned(),
            format!("{id}.material.tmp"),
            format!("{id}.other"),
            format!("{}.issued", id.to_uppercase()),
            format!("{id}.issued.x-1.tmp"),
        ] {
            assert!(!is_sealed_file(&name), "{name}");
        }
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
