//! Sealed material and the slot that guards it.
//!
//! Credential material and service-issued data are opaque byte blobs: only the
//! adapter that wrote one decodes it. The daemon implements [`MaterialSlot`]
//! so an adapter can read its material, mark it as rotating before a
//! single-use refresh, and commit the rotated material with a compare-and-swap
//! that never widens permissions.

use crate::Outcome;

/// Credential material sealed by its credential adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SealedMaterial {}

/// Data a service issued for a connection, sealed by its service adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SealedIssued {}

/// A compare-and-swap replacement of credential material.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct MaterialUpdate {}

/// The daemon's guard around one credential's material.
pub trait MaterialSlot: Send + Sync {
    /// Reads the current sealed material.
    fn read(&self) -> Result<SealedMaterial, Outcome>;

    /// Records, durably, that a rotation is about to be sent to the provider.
    fn mark_rotating(&self) -> Result<(), Outcome>;

    /// Commits rotated material if the stored revision is still the expected one.
    fn commit(&self, update: MaterialUpdate) -> Result<(), Outcome>;
}
