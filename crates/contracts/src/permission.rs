//! Permission sets.
//!
//! A permission set is opaque to everyone but its credential adapter, which
//! defines subset and union. A verified set is a proof, built only by the
//! adapter from what the provider granted, and can never be deserialized.

/// An opaque set of permissions a credential holds or a connection needs.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PermissionSet {}

/// A permission set the credential adapter verified against the provider's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct VerifiedPermissions {}
