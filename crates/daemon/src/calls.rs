//! Tool calls through a ticket.
//!
//! A call begins under the lobby, which resolves the tool and the connection
//! the mind named with `at`, journals the call as invoked, and hands back a
//! [`Ticket`] pinning the connection and its revision. The service call then
//! runs on a blocking thread with no lock held. It settles under the lobby
//! again, where the ticket is checked against the connection as it is now:
//! when the connection was revoked, needs a sign-in or was signed in again
//! while the call ran, the late result never reaches the mind. A read
//! answers with what happened to the connection; a write, which the service
//! may have carried out, answers with an unknown outcome that says what
//! happened, so the mind reads before it retries. Either way the call is
//! journaled as settled with the connection it went through.

use lobby_contracts::{
    Address, ConnectionId, CredentialAdapter, CredentialId, Effect, Explained, Outcome,
    ServiceAdapter, ServiceName, ToolDescriptor, ToolName, ToolResult,
};
use std::sync::Arc;

/// What a call is pinned to while it runs.
#[derive(Clone)]
pub struct Ticket {
    /// The connection the call goes through.
    pub connection: ConnectionId,
    /// The connection's revision when the call began.
    pub revision: u64,
    /// The credential the connection was established with.
    pub credential: CredentialId,
    /// Where the service is.
    pub address: Address,
    /// The service.
    pub service: ServiceName,
    /// The tool, without the service prefix.
    pub tool: ToolName,
    /// Whether the tool reads or writes.
    pub effect: Effect,
    /// The service's adapter.
    pub adapter: Arc<dyn ServiceAdapter>,
    /// The adapter of the connection's credential, which mints what the
    /// service adapter asks for when it establishes the connection again.
    pub credential_adapter: Arc<dyn CredentialAdapter>,
}

impl std::fmt::Debug for Ticket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ticket")
            .field("connection", &self.connection)
            .field("revision", &self.revision)
            .field("service", &self.service)
            .field("tool", &self.tool)
            .finish_non_exhaustive()
    }
}

/// The tool `projected` names among `catalog`, the tools of `service`: the
/// part after `<service>_`.
pub fn find_tool(
    service: &ServiceName,
    catalog: &[ToolDescriptor],
    projected: &str,
) -> Option<ToolDescriptor> {
    let tool = projected
        .strip_prefix(service.as_str())?
        .strip_prefix('_')?;
    catalog
        .iter()
        .find(|descriptor| descriptor.name.as_str() == tool)
        .cloned()
}

/// Where a call's connection stands when the call settles, against the
/// ticket it began with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Standing {
    /// Still ready at the ticket's revision: the answer reaches the mind.
    Pinned,
    /// Its account needs the sponsor to sign it in again. `place` names the
    /// connection as the mind knows it, such as `Board at main`.
    NeedsSignIn { place: String },
    /// Ready again at a later revision: its account was signed in again
    /// while the call ran.
    SignedInAgain { place: String },
    /// Revoked, or gone.
    Revoked,
}

/// What a call answers once it settled, given where its connection stands:
/// the result, or the outcome and the sentence the mind reads. A late
/// answer never reaches the mind: a read says why it is not shown, and a
/// write, which the service may have carried out, says the outcome is
/// unknown, why, and to read before writing again.
pub fn settle(
    effect: Effect,
    standing: &Standing,
    result: Result<ToolResult, Explained>,
) -> Result<ToolResult, (Outcome, String)> {
    let write = matches!(effect, Effect::Write { .. });
    Err(match (standing, write) {
        (Standing::Pinned, _) => {
            return result.map_err(|explained| (explained.outcome, explained.sentence.to_owned()));
        }
        (Standing::Revoked, false) => (
            Outcome::Revoked,
            "The sponsor revoked this connection while the read was on its way, so its answer is not shown. Call reach to see what this companion reaches now.".to_owned(),
        ),
        (Standing::NeedsSignIn { place }, false) => (
            Outcome::NeedsSignIn,
            format!(
                "The account behind {place} needs the sponsor to sign it in again on the manager page; this read's answer is not shown, and its tools come back once they do."
            ),
        ),
        (Standing::SignedInAgain { place }, false) => (
            Outcome::TemporarilyUnavailable,
            format!(
                "The account behind {place} was signed in again while this read was on its way, so its answer is not shown; the read is safe to make again."
            ),
        ),
        (Standing::Revoked, true) => (
            Outcome::Unknown,
            "The sponsor revoked this connection while the write was on its way, so the lobby cannot say whether the service carried it out. Read the target before deciding whether to write again; the connection is gone until the sponsor opens it again.".to_owned(),
        ),
        (Standing::NeedsSignIn { place }, true) => (
            Outcome::Unknown,
            format!(
                "The account behind {place} needed signing in again while this write was on its way, so the lobby cannot say whether the service carried it out. Once the sponsor signs it in again, read the target before writing again."
            ),
        ),
        (Standing::SignedInAgain { place }, true) => (
            Outcome::Unknown,
            format!(
                "The account behind {place} was signed in again while this write was on its way, so the lobby cannot say whether the service carried it out. Read the target before deciding whether to write again."
            ),
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lobby_contracts::{Passage, Retry};

    fn done() -> Result<ToolResult, Explained> {
        Ok(ToolResult {
            text: vec![Passage::Authored("late".to_owned())],
            structured: None,
        })
    }

    fn outcome(answer: Result<ToolResult, (Outcome, String)>) -> Result<ToolResult, Outcome> {
        answer.map_err(|(outcome, _)| outcome)
    }

    #[test]
    fn a_result_that_arrives_after_its_connection_changed_never_reaches_the_mind() {
        let write = Effect::Write {
            retry: Retry::ReadFirst,
        };
        let (read, said) =
            settle(Effect::Read, &Standing::Revoked, done()).expect_err("never shown");
        assert_eq!(read, Outcome::Revoked);
        assert!(said.contains("Call reach"), "{said}");
        let (late, said) = settle(write, &Standing::Revoked, done()).expect_err("never a success");
        assert_eq!(late, Outcome::Unknown);
        assert!(said.contains("Read the target before"));
        assert_eq!(
            outcome(settle(
                write,
                &Standing::Revoked,
                Err(Outcome::Refused.into())
            )),
            Err(Outcome::Unknown),
            "a write the service may have carried out is never reported as refused"
        );
        assert_eq!(
            settle(Effect::Read, &Standing::Pinned, done()),
            Ok(done().expect("done"))
        );
        assert_eq!(
            outcome(settle(
                write,
                &Standing::Pinned,
                Err(Outcome::Refused.into())
            )),
            Err(Outcome::Refused)
        );
    }

    #[test]
    fn a_call_whose_account_needs_a_sign_in_meanwhile_never_says_the_sponsor_revoked_it() {
        let write = Effect::Write {
            retry: Retry::ReadFirst,
        };
        let waiting = Standing::NeedsSignIn {
            place: "Board at main".to_owned(),
        };
        let (read, said) = settle(Effect::Read, &waiting, done()).expect_err("never shown");
        assert_eq!(read, Outcome::NeedsSignIn);
        assert_eq!(
            said,
            "The account behind Board at main needs the sponsor to sign it in again on the manager page; this read's answer is not shown, and its tools come back once they do."
        );
        let (late, said) = settle(write, &waiting, done()).expect_err("never a success");
        assert_eq!(late, Outcome::Unknown);
        assert!(said.contains("needed signing in again"), "{said}");
        assert!(said.contains("read the target before writing again"));
        let again = Standing::SignedInAgain {
            place: "Board at main".to_owned(),
        };
        for effect in [Effect::Read, write] {
            let (_, said) = settle(effect, &again, done()).expect_err("never shown");
            assert!(!said.contains("revoked"), "{said}");
            assert!(said.contains("signed in again"));
        }
    }

    #[test]
    fn a_projected_name_finds_its_tool_only_under_its_own_service() {
        let catalog = vec![ToolDescriptor {
            name: ToolName::new("read").expect("tool"),
            title: "Read".to_owned(),
            description: "Reads.".to_owned(),
            input_schema: serde_json::json!({ "type": "object" }),
            effect: Effect::Read,
        }];
        let board = ServiceName::new("board").expect("service");
        assert!(find_tool(&board, &catalog, "board_read").is_some());
        for other in ["board_post", "boardread", "read", "boards_read", "board_"] {
            assert!(find_tool(&board, &catalog, other).is_none(), "{other}");
        }
    }
}
