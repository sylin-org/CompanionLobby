//! Sealed material and the slot that guards it.
//!
//! Credential material and service-issued data are opaque byte blobs: only the
//! adapter that wrote one decodes it, and the daemon only keeps it, sealed at
//! rest, beside the record it belongs to. The daemon implements
//! [`MaterialSlot`] so a credential adapter reads its material through the
//! daemon, which re-checks, at the moment of the read, that the credential is
//! still the one the call was pinned to, and commits a renewal only on top
//! of the material it replaces. It implements [`IssuedSlot`] for each tool
//! call, so a service adapter whose service stopped taking what it issued
//! can establish the connection again, once, and keep what it issues anew.

use std::fmt;

use crate::{Authority, CredentialId, Outcome};

/// Credential material, encoded by its credential adapter and opaque to
/// everyone else. Its `Debug` never prints the bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct SealedMaterial(Vec<u8>);

/// Data a service issued for a connection, encoded by its service adapter and
/// opaque to everyone else. Its `Debug` never prints the bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct SealedIssued(Vec<u8>);

impl SealedMaterial {
    /// Wraps the bytes an adapter encoded.
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// The bytes, for the adapter that encoded them and for the keyring.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl SealedIssued {
    /// Wraps the bytes an adapter encoded.
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// The bytes, for the adapter that encoded them and for the keyring.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for SealedMaterial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SealedMaterial({} bytes)", self.0.len())
    }
}

impl fmt::Debug for SealedIssued {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SealedIssued({} bytes)", self.0.len())
    }
}

/// A renewal's new material, to replace exactly the material it was made
/// from, granting no more than that material held.
///
/// It cannot be cloned, and committing it consumes it, so one renewal
/// commits at most once; the slot commits it only while the credential's
/// material is still [`expected`](Self::expected).
#[derive(Debug, PartialEq, Eq)]
pub struct MaterialUpdate {
    expected: SealedMaterial,
    replacement: SealedMaterial,
    permissions: Vec<String>,
}

impl MaterialUpdate {
    /// An update replacing `expected`, whose permissions were `held`, with
    /// `replacement`, granting `permissions`. A renewal has no consent
    /// step, so permissions wider than `held` are refused.
    pub fn new(
        expected: SealedMaterial,
        replacement: SealedMaterial,
        permissions: Vec<String>,
        held: &[String],
    ) -> Option<Self> {
        permissions
            .iter()
            .all(|permission| held.contains(permission))
            .then_some(Self {
                expected,
                replacement,
                permissions,
            })
    }

    /// The material this update replaces.
    pub fn expected(&self) -> &SealedMaterial {
        &self.expected
    }

    /// The new material.
    pub fn replacement(&self) -> &SealedMaterial {
        &self.replacement
    }

    /// What the provider granted with the new material, never more than
    /// the material it replaces held.
    pub fn permissions(&self) -> &[String] {
        &self.permissions
    }
}

/// The daemon's guard around one credential's material.
pub trait MaterialSlot: Send + Sync {
    /// The credential this slot guards, for the adapter's own lock.
    fn credential(&self) -> CredentialId;

    /// Reads the material, when the credential is still the one this slot was
    /// handed out for.
    fn read(&self) -> Result<SealedMaterial, Outcome>;

    /// Marks, durably, that the provider's single-use secret in `read`, the
    /// material as last read, is about to be spent. The adapter sends the
    /// request that spends it only once this answered; it is refused when
    /// the material changed since it was read.
    fn mark_rotating(&self, read: &SealedMaterial) -> Result<(), Outcome>;

    /// Clears the mark: the provider certainly spent nothing.
    fn unmark_rotating(&self);

    /// Keeps `update`'s new material in place of what it expected, and
    /// clears the mark. Refused when the credential or its material changed
    /// meanwhile, or the update grants more than the credential holds.
    fn commit(&self, update: MaterialUpdate) -> Result<(), Outcome>;
}

/// The daemon's guard around what a service issued for one connection,
/// for one tool call through it.
///
/// A service adapter reads what the service issued through it. When the
/// service stops taking that (it expired, or the service answers that it
/// does not know it), the adapter establishes the connection again
/// through [`IssuedSlot::authority`], which mints a fresh proof from the
/// connection's credential, and keeps what the service issued anew with
/// [`IssuedSlot::reissue`]. The lobby keeps it only while the connection is
/// still the one the call was pinned to.
pub trait IssuedSlot: Send + Sync {
    /// What the service issued, as the lobby keeps it now.
    fn read(&self) -> Result<SealedIssued, Outcome>;

    /// The authority that mints from the connection's credential, to
    /// establish the connection again.
    fn authority(&self) -> &dyn Authority;

    /// Keeps `issued`, which the service issued when the connection was
    /// established again, in place of what was read. Refused when the
    /// connection was revoked or changed meanwhile.
    fn reissue(&self, issued: SealedIssued) -> Result<(), Outcome>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_update_never_grants_more_than_the_material_it_replaces() {
        let words = |list: &[&str]| {
            list.iter()
                .map(|word| (*word).to_owned())
                .collect::<Vec<_>>()
        };
        let held = words(&["identity", "rpc:a.b?aud=*"]);
        let old = SealedMaterial::new(b"old".to_vec());
        let new = SealedMaterial::new(b"new".to_vec());
        let same = MaterialUpdate::new(old.clone(), new.clone(), held.clone(), &held)
            .expect("the same permissions");
        assert_eq!(same.expected(), &old);
        assert_eq!(same.replacement(), &new);
        assert_eq!(same.permissions(), held.as_slice());
        assert!(
            MaterialUpdate::new(old.clone(), new.clone(), words(&["identity"]), &held).is_some(),
            "fewer is kept"
        );
        assert!(
            MaterialUpdate::new(old, new, words(&["identity", "transition:generic"]), &held)
                .is_none(),
            "more is refused"
        );
    }

    #[test]
    fn sealed_bytes_never_print() {
        let material = SealedMaterial::new(b"key-0123".to_vec());
        let issued = SealedIssued::new(b"badge-0123".to_vec());
        assert_eq!(format!("{material:?}"), "SealedMaterial(8 bytes)");
        assert_eq!(format!("{issued:?}"), "SealedIssued(10 bytes)");
        assert_eq!(material.as_bytes(), b"key-0123");
    }
}
