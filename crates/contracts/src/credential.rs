//! The Credential noun and the credential adapter contract.
//!
//! A credential record names its companion, the provider kind that signed it
//! in, the account the provider authenticated, and its state. It never holds
//! the material itself: material is a blob only the adapter of its provider
//! kind can decode, which the daemon keeps sealed beside the record and hands
//! back only through a [`MaterialSlot`].

use std::fmt;

use crate::companion::parse_hex_id;
use crate::service::is_short_name;
use crate::{Authorization, AuthorizationAsk, CompanionId, MaterialSlot, Outcome, SealedMaterial};

/// Identifier of a credential: a UUIDv7 rendered as 32 lowercase hex digits.
///
/// The daemon mints identifiers; this type only carries and checks them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CredentialId(u128);

impl CredentialId {
    /// Wraps the 128 bits of a UUID the daemon minted.
    pub const fn from_u128(bits: u128) -> Self {
        Self(bits)
    }

    /// Parses exactly 32 lowercase hex digits.
    pub fn parse(text: &str) -> Option<Self> {
        parse_hex_id(text).map(Self)
    }
}

impl fmt::Display for CredentialId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:032x}", self.0)
    }
}

/// The provider kind a credential adapter serves: `[a-z][a-z0-9]{1,15}`,
/// the rule service names follow.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProviderKind(String);

impl ProviderKind {
    /// Checks `text` against the provider kind rule.
    pub fn new(text: &str) -> Option<Self> {
        is_short_name(text).then(|| Self(text.to_owned()))
    }

    /// The kind as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The account a provider authenticated, in the provider's own stable form:
/// 1 to 256 printable ASCII characters with no spaces.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AccountKey(String);

impl AccountKey {
    /// The longest account key, in bytes.
    pub const MAX_BYTES: usize = 256;

    /// Checks `text` against the account key rule.
    pub fn new(text: &str) -> Option<Self> {
        let valid = !text.is_empty()
            && text.len() <= Self::MAX_BYTES
            && text.bytes().all(|byte| byte.is_ascii_graphic());
        valid.then(|| Self(text.to_owned()))
    }

    /// The account key as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AccountKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Whether a credential can still be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CredentialState {
    /// Signed in; its material can mint authorizations.
    Ready,
    /// The sponsor revoked it: its material is gone, every connection made
    /// from it is revoked, and the account stays reserved to its companion.
    Revoked,
}

/// A credential record, without its material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credential {
    /// The immutable identifier.
    pub id: CredentialId,
    /// The companion it was signed in for.
    pub companion: CompanionId,
    /// The provider kind whose adapter signed it in and alone reads its material.
    pub kind: ProviderKind,
    /// The account the provider authenticated.
    pub account: AccountKey,
    /// Whether it can still be used.
    pub state: CredentialState,
}

/// The sponsor's request to sign a companion in to an account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignInRequest {
    /// What the sponsor typed to say which account or provider to use, in
    /// the provider kind's own terms.
    pub hint: String,
    /// The value the provider must return as the `state` parameter, so the
    /// lobby can match the return to this request; it is used once.
    pub state: String,
    /// The lobby's own return URL, where the provider sends the sponsor's
    /// browser back.
    pub return_to: String,
}

/// Where the sponsor's browser goes to sign in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignInStart {
    /// The URL the browser opens.
    pub url: String,
}

/// What the provider sent back to the lobby's return URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignInReturn {
    /// The query parameters, decoded, in the order they arrived.
    pub query: Vec<(String, String)>,
}

impl SignInReturn {
    /// The first value of the parameter `name`.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// A completed sign-in: the account the provider authenticated and the
/// material to keep for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedIn {
    /// The account the provider authenticated, whatever the hint said.
    pub account: AccountKey,
    /// The material only this adapter decodes.
    pub material: SealedMaterial,
}

/// The contract every credential adapter implements.
///
/// Calls are synchronous and may block on the network; the daemon runs them
/// on a blocking thread with no lock held. An adapter refuses an ask it does
/// not support before it reads material or touches the network, refuses
/// material it did not encode, and never returns provider text: every
/// failure is an [`Outcome`].
pub trait CredentialAdapter: Send + Sync {
    /// The provider kind this adapter serves.
    fn kind(&self) -> ProviderKind;

    /// Starts a sign-in and says where the sponsor's browser goes.
    fn begin_sign_in(&self, request: &SignInRequest) -> Result<SignInStart, Outcome>;

    /// Completes the sign-in `request` started from what the provider sent
    /// back, recording the account the provider authenticated.
    fn complete_sign_in(
        &self,
        request: &SignInRequest,
        returned: &SignInReturn,
    ) -> Result<SignedIn, Outcome>;

    /// Mints an authorization for `ask` from the material in `slot`.
    fn authorize(
        &self,
        slot: &dyn MaterialSlot,
        ask: &AuthorizationAsk,
    ) -> Result<Authorization, Outcome>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_identifiers_round_trip_as_32_hex_digits() {
        let id = CredentialId::from_u128(42);
        assert_eq!(id.to_string(), format!("{:032x}", 42));
        assert_eq!(CredentialId::parse(&id.to_string()), Some(id));
        assert_eq!(CredentialId::parse("42"), None);
    }

    #[test]
    fn provider_kinds_follow_the_service_name_rule() {
        assert!(ProviderKind::new("passbook").is_some());
        for bad in ["", "p", "Passbook", "pass_book", "pass-book"] {
            assert!(ProviderKind::new(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn account_keys_are_printable_ascii_without_spaces() {
        assert!(AccountKey::new("holder-7").is_some());
        assert!(AccountKey::new("member:0001/aaaa").is_some());
        for bad in ["", "a b", "a\nb", "á", &"a".repeat(257)] {
            assert!(AccountKey::new(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn a_return_names_its_parameters() {
        let returned = SignInReturn {
            query: vec![
                ("state".to_owned(), "s".to_owned()),
                ("code".to_owned(), "c".to_owned()),
            ],
        };
        assert_eq!(returned.get("code"), Some("c"));
        assert_eq!(returned.get("missing"), None);
    }
}
