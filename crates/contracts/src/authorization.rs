//! What a service asks a credential for, and what it gets back.
//!
//! A service adapter never holds credential material. When it needs to prove
//! who it acts for, it asks the daemon's [`Authority`](crate::Authority) for
//! an authorization in the exact shape the service wants: a proof for one
//! audience and method, or a proof of one request to the account's own
//! server. The credential adapter of the connection's credential mints it
//! from material only it can read, or refuses an ask it does not support
//! before it reads anything. The answer is a scheme and a [`Secret`] token,
//! and, for a proof of one request, the proof that goes beside it. A shape
//! joins this enum with the first adapter that answers it.

use std::fmt;

/// A shape of authorization a service adapter asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorizationAsk {
    /// A short-lived proof, for the service that declared `audience`, that
    /// the account may call `method` there.
    ServiceProof {
        /// Who the proof is for, as the service declared itself.
        audience: String,
        /// The method the proof allows, in the service's own words.
        method: String,
        /// How long the proof may live, in seconds.
        lifetime_secs: u32,
    },
    /// A proof of exactly one request to the account's own server, where
    /// the account's token may be presented and nowhere else: `method` to
    /// `url`. A credential adapter mints one only for a URL on the server
    /// its material names, so a token never leaves for another origin.
    ProvedRequest {
        /// The request's method, such as `GET` or `POST`.
        method: String,
        /// The whole URL the request goes to, query included.
        url: String,
        /// The value the server last asked proofs to carry, when it asked
        /// for one; the adapter may also know one of its own.
        nonce: Option<String>,
        /// Whether the server did not take the token the previous proof of
        /// this request carried, as a server that stops taking one early
        /// does: the adapter renews the material first, due or not.
        refused_before: bool,
    },
}

/// The authorization a credential adapter minted for one ask.
#[derive(Debug, PartialEq, Eq)]
pub struct Authorization {
    /// The scheme the token is presented with, such as `Bearer`.
    pub scheme: String,
    /// The token.
    pub token: Secret,
    /// For a proof of one request, the proof sent beside the token in the
    /// header the scheme names; that request alone carries it.
    pub proof: Option<Secret>,
}

impl Authorization {
    /// The value of an `Authorization` header carrying this authorization.
    pub fn header_value(&self) -> String {
        format!("{} {}", self.scheme, self.token.expose())
    }
}

/// A secret: a token or a key. It never prints in `Debug`, implements no
/// serialization, and its bytes are overwritten when it is dropped.
#[derive(PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wraps `text`.
    pub fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    /// The secret itself, for the one place that presents it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(..)")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        let mut bytes = std::mem::take(&mut self.0).into_bytes();
        bytes.iter_mut().for_each(|byte| *byte = 0);
        // Keeps the overwrite from being optimized away as a dead store.
        std::hint::black_box(&bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_never_prints() {
        let authorization = Authorization {
            scheme: "Bearer".to_owned(),
            token: Secret::new("stamp-0123456789"),
            proof: Some(Secret::new("proof-0123456789")),
        };
        let printed = format!("{authorization:?}");
        assert!(!printed.contains("stamp-0123456789"), "{printed}");
        assert!(!printed.contains("proof-0123456789"), "{printed}");
        assert!(printed.contains("Secret(..)"));
        assert_eq!(authorization.header_value(), "Bearer stamp-0123456789");
    }
}
