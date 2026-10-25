//! Tool calls through a ticket.
//!
//! A call begins under the lobby, which resolves the tool and the connection
//! the mind named with `at`, journals the call as invoked, and hands back a
//! [`Ticket`] pinning the connection and its revision. The service call then
//! runs on a blocking thread with no lock held. It settles under the lobby
//! again, where the ticket is checked against the connection as it is now:
//! when the connection was revoked or changed while the call ran, the late
//! result never reaches the mind. A read answers revoked; a write, which the
//! service may have carried out, answers with an unknown outcome so the mind
//! reads before it retries. Either way the call is journaled as settled with
//! the connection it went through.

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

/// What a call answers once it settled. `still_pinned` says whether the
/// connection is still at the ticket's revision and ready.
pub fn settle(
    effect: Effect,
    still_pinned: bool,
    result: Result<ToolResult, Explained>,
) -> Result<ToolResult, Explained> {
    if still_pinned {
        return result;
    }
    match effect {
        Effect::Read => Err(Outcome::Revoked.into()),
        Effect::Write { .. } => Err(Explained::new(
            Outcome::Unknown,
            "The sponsor revoked this connection while the write was on its way, so the lobby cannot say whether the service carried it out. Read the target before deciding whether to write again; the connection is gone until the sponsor opens it again.",
        )),
    }
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

    fn outcome(answer: Result<ToolResult, Explained>) -> Result<ToolResult, Outcome> {
        answer.map_err(|explained| explained.outcome)
    }

    #[test]
    fn a_result_that_arrives_after_its_connection_changed_never_reaches_the_mind() {
        let write = Effect::Write {
            retry: Retry::ReadFirst,
        };
        assert_eq!(
            outcome(settle(Effect::Read, false, done())),
            Err(Outcome::Revoked)
        );
        let late = settle(write, false, done()).expect_err("never a success");
        assert_eq!(late.outcome, Outcome::Unknown);
        assert!(late.sentence.contains("Read the target before"));
        assert_eq!(
            outcome(settle(write, false, Err(Outcome::Refused.into()))),
            Err(Outcome::Unknown),
            "a write the service may have carried out is never reported as refused"
        );
        assert_eq!(settle(Effect::Read, true, done()), done());
        assert_eq!(
            outcome(settle(write, true, Err(Outcome::Refused.into()))),
            Err(Outcome::Refused)
        );
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
