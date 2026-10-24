//! Sealed material and the slot that guards it.
//!
//! Credential material and service-issued data are opaque byte blobs: only the
//! adapter that wrote one decodes it, and the daemon only keeps it, sealed at
//! rest, beside the record it belongs to. The daemon implements
//! [`MaterialSlot`] so a credential adapter reads its material through the
//! daemon, which re-checks, at the moment of the read, that the credential is
//! still the one the call was pinned to.

use std::fmt;

use crate::Outcome;

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

/// The daemon's guard around one credential's material.
pub trait MaterialSlot: Send + Sync {
    /// Reads the material, when the credential is still the one this slot was
    /// handed out for.
    fn read(&self) -> Result<SealedMaterial, Outcome>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_bytes_never_print() {
        let material = SealedMaterial::new(b"key-0123".to_vec());
        let issued = SealedIssued::new(b"badge-0123".to_vec());
        assert_eq!(format!("{material:?}"), "SealedMaterial(8 bytes)");
        assert_eq!(format!("{issued:?}"), "SealedIssued(10 bytes)");
        assert_eq!(material.as_bytes(), b"key-0123");
    }
}
