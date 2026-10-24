//! The lobby's state and the commands that change it.
//!
//! The state is held in memory by the daemon process: companions and the live
//! attachments. A restart of the daemon starts with no companions. Every
//! change goes through one of the commands below, which validate before they
//! apply; nothing else writes the state.
//!
//! A companion exists only because the sponsor created it through the
//! operator API; an attachment exists only because the attach shim presented
//! the per-install secret and named a companion.

use std::collections::HashMap;
use std::sync::Mutex;

use lobby_contracts::{
    Attachment, AttachmentId, Companion, CompanionId, DisplayName, Moniker, NameProblem, Outcome,
    Sponsor,
};

/// Why a command was refused, with the lobby's own words for the sponsor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// The outcome.
    pub outcome: Outcome,
    /// What went wrong, written by the lobby; never text from outside it.
    pub message: String,
}

impl Refusal {
    fn new(outcome: Outcome, message: impl Into<String>) -> Self {
        Self {
            outcome,
            message: message.into(),
        }
    }
}

#[derive(Default)]
struct State {
    companions: Vec<Companion>,
    attachments: HashMap<AttachmentId, Attachment>,
}

/// The lobby: one owner of the state.
#[derive(Default)]
pub struct Lobby {
    state: Mutex<State>,
}

impl Lobby {
    /// An empty lobby.
    pub fn new() -> Self {
        Self::default()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        // A poisoned lock means a panic mid-command; the state is still the
        // last fully applied one, because commands validate before they apply.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Creates a companion. The moniker must be free.
    pub fn create_companion(
        &self,
        moniker: &str,
        display_name: &str,
    ) -> Result<Companion, Refusal> {
        let moniker = Moniker::new(moniker).map_err(|problem| name_refusal("moniker", problem))?;
        let display_name = DisplayName::new(display_name)
            .map_err(|problem| name_refusal("display name", problem))?;
        let mut state = self.state();
        if state
            .companions
            .iter()
            .any(|companion| companion.moniker == moniker)
        {
            return Err(Refusal::new(
                Outcome::Conflict,
                format!("A companion named {moniker} already exists."),
            ));
        }
        let companion = Companion {
            id: CompanionId::from_u128(uuid::Uuid::now_v7().as_u128()),
            moniker,
            display_name,
            sponsor: Sponsor::LOCAL,
        };
        state.companions.push(companion.clone());
        Ok(companion)
    }

    /// Every companion, oldest first.
    pub fn companions(&self) -> Vec<Companion> {
        self.state().companions.clone()
    }

    /// Attaches a mind as the companion named by id or moniker.
    pub fn attach(&self, name: &str) -> Result<(Attachment, Companion), Refusal> {
        let mut state = self.state();
        let companion = find(&state.companions, name).cloned().ok_or_else(|| {
            Refusal::new(
                Outcome::NotFound,
                "No companion has that id or moniker; the sponsor creates companions on the manager page.",
            )
        })?;
        let attachment = Attachment {
            id: AttachmentId::from_u128(uuid::Uuid::now_v7().as_u128()),
            companion: companion.id,
        };
        state.attachments.insert(attachment.id, attachment.clone());
        Ok((attachment, companion))
    }

    /// The live attachment and its companion.
    pub fn attachment(&self, id: AttachmentId) -> Option<(Attachment, Companion)> {
        let state = self.state();
        let attachment = state.attachments.get(&id)?.clone();
        let companion = state
            .companions
            .iter()
            .find(|companion| companion.id == attachment.companion)?
            .clone();
        Some((attachment, companion))
    }

    /// Ends an attachment. Returns whether it was live.
    pub fn detach(&self, id: AttachmentId) -> bool {
        self.state().attachments.remove(&id).is_some()
    }

    /// How many attachments are live.
    pub fn attachment_count(&self) -> usize {
        self.state().attachments.len()
    }
}

fn find<'a>(companions: &'a [Companion], name: &str) -> Option<&'a Companion> {
    match CompanionId::parse(name) {
        Some(id) => companions.iter().find(|companion| companion.id == id),
        None => companions
            .iter()
            .find(|companion| companion.moniker.as_str() == name),
    }
}

fn name_refusal(what: &str, problem: NameProblem) -> Refusal {
    let rule = match what {
        "moniker" => {
            " A moniker is 1 to 24 lowercase letters, digits and single hyphens, starting with a letter."
        }
        _ => {
            " A display name is 1 to 64 characters with no control characters or surrounding spaces."
        }
    };
    Refusal::new(
        Outcome::InvalidInput,
        format!("The {what} was refused: {problem}.{rule}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn companions_are_created_once_per_moniker() {
        let lobby = Lobby::new();
        let ada = lobby.create_companion("ada", "Ada").expect("ada");
        assert_eq!(ada.moniker.as_str(), "ada");
        let duplicate = lobby.create_companion("ada", "Another").expect_err("taken");
        assert_eq!(duplicate.outcome, Outcome::Conflict);
        let invalid = lobby.create_companion("Ada!", "Ada").expect_err("invalid");
        assert_eq!(invalid.outcome, Outcome::InvalidInput);
        assert_eq!(lobby.companions(), vec![ada]);
    }

    #[test]
    fn a_mind_attaches_by_moniker_or_id_and_detaches() {
        let lobby = Lobby::new();
        let ada = lobby.create_companion("ada", "Ada").expect("ada");
        let (by_moniker, companion) = lobby.attach("ada").expect("by moniker");
        assert_eq!(companion, ada);
        let (by_id, _) = lobby.attach(&ada.id.to_string()).expect("by id");
        assert_ne!(
            by_moniker.id, by_id.id,
            "every attach is its own attachment"
        );
        assert_eq!(lobby.attachment_count(), 2);
        assert_eq!(lobby.attachment(by_id.id).map(|(_, c)| c.id), Some(ada.id));
        assert!(lobby.detach(by_id.id));
        assert!(!lobby.detach(by_id.id));
        assert!(lobby.attachment(by_id.id).is_none());
        assert_eq!(
            lobby.attach("bea").expect_err("unknown").outcome,
            Outcome::NotFound
        );
    }
}
