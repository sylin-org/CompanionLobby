//! What a service asks a credential for, and what it gets back.
//!
//! An ask is one of: a bearer token, a service proof for a declared audience
//! and method with a bounded lifetime, or a proved request for one method and
//! URL. The answer is a scheme, a secret token, extra headers and an expiry.
//! Secrets redact themselves in `Debug`, zeroize on drop and never serialize.

/// The kind of authorization a service adapter asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AuthorizationAsk {}

/// The authorization a credential adapter minted for one ask.
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Authorization {}
