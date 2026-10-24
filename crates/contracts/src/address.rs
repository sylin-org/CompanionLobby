//! Service addresses.
//!
//! An address is a normalized origin: lowercase host, bracketed IPv6, default
//! port removed, and no user info, path, query or fragment. This is the one
//! origin parser in the workspace. The policy decides whether loopback and
//! private targets are acceptable.

/// A normalized service origin.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct Address {}

/// Which targets an address may resolve to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AddressPolicy {
    /// Only public HTTPS origins.
    PublicHttps,
    /// Loopback origins too, for test fixtures and local services.
    LoopbackAllowed,
}
