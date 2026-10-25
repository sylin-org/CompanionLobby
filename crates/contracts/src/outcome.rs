//! The single outcome vocabulary.
//!
//! Every adapter, the daemon and the MCP projection answer with one of these
//! outcomes. No variant can carry provider text: messages a mind or a sponsor
//! reads are authored by the lobby, or by an adapter's author as an
//! [`Explained`] outcome, whose sentence is fixed text compiled into the
//! adapter.

use std::fmt;

/// How a call ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Outcome {
    /// The call completed.
    Done,
    /// The service accepted the request and will complete it later.
    Accepted,
    /// The request is waiting on the service.
    Pending,
    /// The service or the lobby refused the request definitively.
    Refused,
    /// The target does not exist or is not visible to this companion.
    NotFound,
    /// The request conflicts with the target's state.
    Conflict,
    /// The arguments do not satisfy the tool's schema or invariants.
    InvalidInput,
    /// The credential needs the sponsor to sign in again.
    NeedsSignIn,
    /// The credential lacks a permission the call needs.
    NeedsPermission,
    /// The connection or credential was revoked.
    Revoked,
    /// The service is unreachable or failing; retrying later may help.
    TemporarilyUnavailable,
    /// The service asked the lobby to slow down.
    RateLimited,
    /// A write may or may not have landed; read before retrying. Its wire name is `outcome_unknown`.
    Unknown,
    /// The adapter does not offer this operation.
    Unsupported,
    /// The lobby itself failed.
    Internal,
}

/// Whether a write may be repeated after an unknown outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Retry {
    /// Repeating the call cannot duplicate its effect.
    Safe,
    /// Read the target first; repeat only if the effect is absent.
    ReadFirst,
    /// Never repeat automatically.
    Never,
}

impl Outcome {
    /// Every outcome, in declaration order.
    pub const ALL: [Self; 15] = [
        Self::Done,
        Self::Accepted,
        Self::Pending,
        Self::Refused,
        Self::NotFound,
        Self::Conflict,
        Self::InvalidInput,
        Self::NeedsSignIn,
        Self::NeedsPermission,
        Self::Revoked,
        Self::TemporarilyUnavailable,
        Self::RateLimited,
        Self::Unknown,
        Self::Unsupported,
        Self::Internal,
    ];

    /// The stable wire name of this outcome.
    pub const fn code(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Accepted => "accepted",
            Self::Pending => "pending",
            Self::Refused => "refused",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::InvalidInput => "invalid_input",
            Self::NeedsSignIn => "needs_sign_in",
            Self::NeedsPermission => "needs_permission",
            Self::Revoked => "revoked",
            Self::TemporarilyUnavailable => "temporarily_unavailable",
            Self::RateLimited => "rate_limited",
            Self::Unknown => "outcome_unknown",
            Self::Unsupported => "unsupported",
            Self::Internal => "internal",
        }
    }

    /// Whether this outcome means the call did what was asked, now or later.
    pub const fn is_success(self) -> bool {
        matches!(self, Self::Done | Self::Accepted | Self::Pending)
    }

    /// The fixed sentence the lobby says for this outcome. Nothing a provider
    /// or service sent can reach it.
    pub const fn sentence(self) -> &'static str {
        match self {
            Self::Done => "Done.",
            Self::Accepted => "The service accepted the request and will complete it later.",
            Self::Pending => "The request is waiting on the service.",
            Self::Refused => "The request was refused.",
            Self::NotFound => "That does not exist, or this companion cannot see it.",
            Self::Conflict => {
                "This request conflicts with the current state of its target; read it again before changing it."
            }
            Self::InvalidInput => "The input was refused.",
            Self::NeedsSignIn => {
                "The sponsor needs to sign in to this account again on the manager page."
            }
            Self::NeedsPermission => "Your current access does not allow this action.",
            Self::Revoked => "The sponsor revoked this.",
            Self::TemporarilyUnavailable => {
                "The service is unavailable right now; try again later."
            }
            Self::RateLimited => "The service asked for fewer requests; wait before trying again.",
            Self::Unknown => {
                "The service may have completed this write. Read the target before deciding whether to retry."
            }
            Self::Unsupported => "This is not offered here.",
            Self::Internal => {
                "The lobby could not complete this; the sponsor can check its trace log."
            }
        }
    }
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

/// An outcome with the sentence an adapter's author wrote for it: the fact
/// that ended the request and what to do next, in the words of the service
/// or provider the adapter speaks. The sentence is `'static` text compiled
/// into the adapter, so nothing a provider or a service sent can reach it.
/// An adapter with nothing to add answers a bare [`Outcome`], which becomes
/// one carrying the lobby's own sentence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Explained {
    /// How the request ended.
    pub outcome: Outcome,
    /// Why, and what to do next.
    pub sentence: &'static str,
}

impl Explained {
    /// `outcome`, explained by `sentence`.
    pub const fn new(outcome: Outcome, sentence: &'static str) -> Self {
        Self { outcome, sentence }
    }
}

impl From<Outcome> for Explained {
    fn from(outcome: Outcome) -> Self {
        Self::new(outcome, outcome.sentence())
    }
}

impl fmt::Display for Explained {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.outcome.code())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_names_are_unique_and_snake_case() {
        let mut codes: Vec<&str> = Outcome::ALL.iter().map(|outcome| outcome.code()).collect();
        assert!(codes.iter().all(|code| {
            !code.is_empty()
                && code
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
        }));
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), Outcome::ALL.len());
        assert_eq!(Outcome::Unknown.code(), "outcome_unknown");
    }

    #[test]
    fn every_sentence_is_fixed_lobby_text() {
        // An outcome holds no data, so nothing a service sent can become the
        // text a mind or a sponsor reads; the sentences are compile-time constants.
        for outcome in Outcome::ALL {
            let sentence = outcome.sentence();
            assert!(sentence.ends_with('.'), "{outcome}: {sentence}");
            assert!(!sentence.contains('{') && !sentence.contains('}'));
        }
        assert_eq!(std::mem::size_of::<Outcome>(), 1);
    }

    #[test]
    fn a_bare_outcome_is_explained_by_the_lobbys_own_sentence() {
        let explained = Explained::from(Outcome::Unknown);
        assert_eq!(explained.outcome, Outcome::Unknown);
        assert_eq!(explained.sentence, Outcome::Unknown.sentence());
        let authored = Explained::new(Outcome::Refused, "The board is closed to new posts.");
        assert_eq!(authored.to_string(), "refused");
    }

    #[test]
    fn the_honest_retry_sentences_say_what_to_do() {
        assert!(Outcome::Unknown.sentence().contains("Read the target"));
        assert!(Outcome::Conflict.sentence().contains("read it again"));
        assert!(
            Outcome::NeedsPermission
                .sentence()
                .contains("does not allow")
        );
    }
}
