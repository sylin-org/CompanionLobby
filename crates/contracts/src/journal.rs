//! The Journal noun.
//!
//! The journal is attribution, not truth: the state file is truth. Every entry
//! records a sequence number, a time, who initiated it (a mind through its
//! attachment, the sponsor, or the lobby itself), the companion, and when
//! relevant the connection it went through, the service and the tool, the
//! kind (invoked, settled, command) and the outcome. It also carries the
//! names the sponsor read for what it concerns as of the moment it was
//! written ([`JournalNames`]), so its sentence reads the same after a
//! rename and a removed companion keeps its history by name. By
//! construction an entry holds no content and no secrets: every field is a
//! typed identifier, a validated name, a fixed lobby word or an
//! [`Outcome`], and none is free text.

use crate::{
    AccountLabel, AttachmentId, CompanionId, ConnectionId, DisplayName, Label, Outcome,
    ServiceName, ToolName,
};

/// Who caused a journal entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Initiator {
    /// A mind, through its attachment.
    Mind {
        /// The attachment the mind acted through.
        attachment: AttachmentId,
    },
    /// The sponsor, on the manager page or the command line.
    Sponsor,
    /// The lobby itself, reacting to an event.
    Lobby,
}

/// What kind of thing a journal entry records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JournalKind {
    /// An external call is about to be made. No entry, no call.
    Invoked,
    /// An external call finished with the entry's outcome.
    Settled,
    /// A command changed the lobby's own state.
    Command {
        /// The command's fixed name, such as `create_companion` or `attach`.
        name: &'static str,
    },
}

/// The names the sponsor read, when an entry was written, for what it
/// concerns. Each is a validated name; none is text a provider or a service
/// wrote beyond an account's label.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JournalNames {
    /// The companion's display name.
    pub companion: Option<DisplayName>,
    /// The title of the account's kind, such as the kind's adapter names it.
    pub kind: Option<DisplayName>,
    /// The label the sponsor knows the account by.
    pub account: Option<AccountLabel>,
    /// The service's title.
    pub service: Option<DisplayName>,
    /// The title the service's adapter gives the tool called.
    pub tool: Option<DisplayName>,
    /// The sponsor's label for the connection.
    pub connection: Option<Label>,
}

/// One attributed journal line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalEntry {
    /// The position in this install's journal, strictly increasing.
    pub seq: u64,
    /// When it happened, in milliseconds since the Unix epoch, as the daemon's clock read it.
    pub at_unix_ms: u64,
    /// Who caused it.
    pub initiator: Initiator,
    /// The companion it concerns, if any.
    pub companion: Option<CompanionId>,
    /// The connection it went through, if any.
    pub connection: Option<ConnectionId>,
    /// The service it reached, if any.
    pub service: Option<ServiceName>,
    /// The tool it called, if any.
    pub tool: Option<ToolName>,
    /// What kind of entry this is.
    pub kind: JournalKind,
    /// How it ended.
    pub outcome: Outcome,
    /// The names of what it concerns, as of the moment it was written.
    pub names: JournalNames,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_attach_is_attributed_to_the_mind_through_its_attachment() {
        let attachment = AttachmentId::from_u128(7);
        let entry = JournalEntry {
            seq: 1,
            at_unix_ms: 0,
            initiator: Initiator::Mind { attachment },
            companion: Some(CompanionId::from_u128(9)),
            connection: None,
            service: None,
            tool: None,
            kind: JournalKind::Command { name: "attach" },
            outcome: Outcome::Done,
            names: JournalNames {
                companion: DisplayName::new("Ada").ok(),
                ..JournalNames::default()
            },
        };
        assert_eq!(entry.initiator, Initiator::Mind { attachment });
        assert_eq!(entry.outcome.code(), "done");
    }

    #[test]
    fn a_tool_call_names_its_connection_service_and_tool_but_carries_no_text() {
        let connection = ConnectionId::from_u128(11);
        let entry = JournalEntry {
            seq: 2,
            at_unix_ms: 1,
            initiator: Initiator::Mind {
                attachment: AttachmentId::from_u128(7),
            },
            companion: Some(CompanionId::from_u128(9)),
            connection: Some(connection),
            service: ServiceName::new("board"),
            tool: ToolName::new("post"),
            kind: JournalKind::Invoked,
            outcome: Outcome::Pending,
            names: JournalNames {
                companion: DisplayName::new("Ada").ok(),
                service: DisplayName::new("Board").ok(),
                connection: Label::new("front").ok(),
                ..JournalNames::default()
            },
        };
        assert_eq!(entry.connection, Some(connection));
        let rendered = format!("{entry:?}");
        assert!(rendered.contains("board") && rendered.contains("post"));
    }
}
