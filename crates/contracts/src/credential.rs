//! The Credential noun and the credential adapter contract.
//!
//! A credential record names its companion, the provider kind that signed it
//! in, the account the provider authenticated with the label the sponsor
//! knows it by, the permissions the provider granted, and its state. It
//! never holds the material itself: material is a blob only the adapter of
//! its provider kind can decode, which the daemon keeps sealed beside the
//! record and hands back only through a [`MaterialSlot`].
//!
//! A credential is kept alive by its adapter: the daemon asks it to
//! [`renew`](CredentialAdapter::renew) the material whenever the adapter
//! said it would want to be asked, and the adapter spends the provider's
//! single-use secret only after the slot has durably marked it rotating, so
//! a daemon that stops between the provider's rotation and the commit of the
//! new material comes back knowing the credential needs signing in again.

use std::fmt;

use crate::companion::parse_hex_id;
use crate::service::is_short_name;
use crate::{
    Authorization, AuthorizationAsk, CompanionId, Explained, MaterialSlot, Outcome, SealedMaterial,
};

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

/// What the sponsor knows an account by, as its provider adapter reported
/// it: a handle or an account name, never the provider's stable key unless
/// the provider has nothing else. It is 1 to [`Self::MAX_BYTES`] bytes of
/// text with no control characters and no space at either end.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AccountLabel(String);

impl AccountLabel {
    /// The longest account label, in bytes.
    pub const MAX_BYTES: usize = 256;

    /// Checks `text` against the account label rule.
    pub fn new(text: &str) -> Option<Self> {
        let valid = !text.is_empty()
            && text.len() <= Self::MAX_BYTES
            && text == text.trim()
            && !text.chars().any(char::is_control);
        valid.then(|| Self(text.to_owned()))
    }

    /// The label as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AccountLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Whether a credential can still be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CredentialState {
    /// Signed in; its material can mint authorizations.
    Ready,
    /// The provider refused to renew it, or the lobby cannot be sure a
    /// renewal landed: its material mints nothing and is never renewed
    /// again, its connections wait, and signing the same account in again
    /// restores it and them.
    NeedsSignIn,
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
    /// What the sponsor knows that account by.
    pub label: AccountLabel,
    /// What the provider granted at sign-in, in the provider's own words, as
    /// its adapter reported them. Nothing but a new sign-in widens them: a
    /// service that needs more asks the sponsor to sign in again.
    pub permissions: Vec<String>,
    /// Whether it can still be used.
    pub state: CredentialState,
}

impl Credential {
    /// The most permissions one credential holds.
    pub const MAX_PERMISSIONS: usize = 32;
    /// The longest permission, in bytes.
    pub const MAX_PERMISSION_BYTES: usize = 256;

    /// Whether `permissions` follow the rule: at most
    /// [`Self::MAX_PERMISSIONS`], each 1 to [`Self::MAX_PERMISSION_BYTES`]
    /// printable ASCII characters with no spaces, none twice.
    pub fn permissions_are_valid(permissions: &[String]) -> bool {
        permissions.len() <= Self::MAX_PERMISSIONS
            && permissions.iter().enumerate().all(|(index, permission)| {
                !permission.is_empty()
                    && permission.len() <= Self::MAX_PERMISSION_BYTES
                    && permission.bytes().all(|byte| byte.is_ascii_graphic())
                    && !permissions[..index].contains(permission)
            })
    }
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
    /// When the sponsor signs a credential that needs signing in again, its
    /// material: the sign-in is for that same account, named to the provider
    /// from the material whatever `hint` says, and completing it must sign
    /// in that account.
    pub again: Option<SealedMaterial>,
    /// What the lobby's services need an account of this kind granted, in
    /// the provider's own words (see
    /// [`ServiceDescription::permissions`](crate::ServiceDescription::permissions)),
    /// which the adapter asks the provider for beside what it always asks.
    pub permissions: Vec<String>,
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
    /// What the sponsor knows that account by: the provider's handle or
    /// account name for it.
    pub label: AccountLabel,
    /// What the provider granted, in its own words; see
    /// [`Credential::permissions`].
    pub permissions: Vec<String>,
    /// The material only this adapter decodes.
    pub material: SealedMaterial,
}

/// What a renewal left: how long until the adapter wants to be asked again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Freshness {
    /// Seconds until the material should be renewed.
    pub renew_in_secs: u64,
}

/// What the provider said when the lobby revoked a credential there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Revocation {
    /// The provider confirmed that what it granted is revoked.
    Confirmed,
    /// The provider offers no way to revoke; what it granted ends when it
    /// expires there.
    NotOffered,
}

/// The contract every credential adapter implements.
///
/// Calls are synchronous and may block on the network; the daemon runs them
/// on a blocking thread with no lock held. An adapter refuses an ask it does
/// not support before it reads material or touches the network, refuses
/// material it did not encode, and never returns provider text: every
/// failure is an [`Outcome`].
///
/// Renewing a credential follows one order, whatever the provider: read the
/// material, decide it is due, [`mark`](MaterialSlot::mark_rotating) it
/// rotating, and only then send the request that spends the provider's
/// single-use secret; on an answer that keeps the profile,
/// [`commit`](MaterialSlot::commit) the new material, which clears the mark.
/// When the provider certainly spent nothing (it asked for fewer requests,
/// or refused before acting) the adapter
/// [`unmarks`](MaterialSlot::unmark_rotating) it. Any other end leaves the
/// mark, and the daemon then treats the credential as needing sign-in and
/// never offers the old secret again. One credential renews once at a time:
/// the adapter holds a lock of its own, keyed by
/// [`MaterialSlot::credential`], across the whole renewal and every
/// authorization that reads the material, and the daemon holds none of its
/// own while the adapter runs.
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

    /// Lets go at once of whatever the adapter keeps for the sign-in
    /// `request` started, which the lobby stopped waiting for.
    fn cancel_sign_in(&self, request: &SignInRequest);

    /// Mints an authorization for `ask` from the material in `slot`,
    /// renewing the material first when it is due. A refusal says, in the
    /// provider's words, why the account cannot give that proof and what
    /// the sponsor does about it.
    fn authorize(
        &self,
        slot: &dyn MaterialSlot,
        ask: &AuthorizationAsk,
    ) -> Result<Authorization, Explained>;

    /// Keeps the credential in `slot` alive: renews its material when it is
    /// due, in the order the contract lays down, and says when to ask again.
    /// A refusal the provider meant is [`Outcome::NeedsSignIn`]; a renewal
    /// that may have spent the secret without a new one kept is
    /// [`Outcome::Unknown`].
    fn renew(&self, slot: &dyn MaterialSlot) -> Result<Freshness, Outcome>;

    /// Revokes at the provider what it granted for the material in `slot`,
    /// which the lobby has already revoked on its side.
    fn revoke(&self, slot: &dyn MaterialSlot) -> Result<Revocation, Outcome>;
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
    fn account_labels_are_trimmed_text_without_control_characters() {
        for good in ["ada.example.test", "Ada Lovelace", "ada"] {
            assert_eq!(AccountLabel::new(good).expect(good).as_str(), good);
        }
        for bad in ["", " ada", "ada ", "a\nb", "a\u{7}b", &"a".repeat(257)] {
            assert!(AccountLabel::new(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn permissions_are_printable_words_without_repeats() {
        let words = |list: &[&str]| {
            list.iter()
                .map(|word| (*word).to_owned())
                .collect::<Vec<_>>()
        };
        assert!(Credential::permissions_are_valid(&[]));
        assert!(Credential::permissions_are_valid(&words(&[
            "stamp",
            "rpc:a.b?aud=*"
        ])));
        for bad in [
            words(&[""]),
            words(&["a b"]),
            words(&["stamp", "stamp"]),
            words(&[&"a".repeat(257)]),
            (0..33).map(|n| n.to_string()).collect(),
        ] {
            assert!(!Credential::permissions_are_valid(&bad), "{bad:?}");
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
