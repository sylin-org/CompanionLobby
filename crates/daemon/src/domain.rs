//! The lobby's state and the commands that change it.
//!
//! The state is held in memory by the daemon process: companions and the live
//! attachments. A restart of the daemon starts with no companions. Every
//! change goes through one of the commands below, which validate before they
//! apply; nothing else writes the state.
//!
//! A companion exists only because the sponsor created it through the
//! operator API; an attachment exists only because the attach shim presented
//! the per-install secret and named a companion. Each live attachment keeps
//! the token its shim presents on every later request.

use std::collections::HashMap;
use std::sync::Mutex;

use lobby_contracts::{
    Attachment, AttachmentId, Companion, CompanionId, DisplayName, Moniker, NameProblem, Outcome,
    Sponsor,
};

use crate::keyring::AttachmentToken;

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
    attachments: HashMap<AttachmentId, (Attachment, AttachmentToken)>,
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

    /// Attaches a mind as the companion named by id or moniker; `token` is
    /// what the attachment presents from then on.
    pub fn attach(
        &self,
        name: &str,
        token: AttachmentToken,
    ) -> Result<(Attachment, Companion), Refusal> {
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
        state
            .attachments
            .insert(attachment.id, (attachment.clone(), token));
        Ok((attachment, companion))
    }

    /// The live attachment and its companion, when `presented` is its token.
    pub fn attachment(
        &self,
        id: AttachmentId,
        presented: &str,
    ) -> Result<(Attachment, Companion), Refusal> {
        let state = self.state();
        let (attachment, token) = state.attachments.get(&id).ok_or_else(|| {
            Refusal::new(
                Outcome::NotFound,
                "No such attachment is live; attach again.",
            )
        })?;
        if !token.matches(presented) {
            return Err(Refusal::new(
                Outcome::Refused,
                "That is not this attachment's token.",
            ));
        }
        let attachment = attachment.clone();
        let companion = state
            .companions
            .iter()
            .find(|companion| companion.id == attachment.companion)
            .cloned()
            .ok_or_else(|| {
                Refusal::new(
                    Outcome::NotFound,
                    "No such attachment is live; attach again.",
                )
            })?;
        Ok((attachment, companion))
    }

    /// Ends an attachment, when `presented` is its token.
    pub fn detach(&self, id: AttachmentId, presented: &str) -> Result<(), Refusal> {
        let mut state = self.state();
        match state.attachments.get(&id) {
            None => Err(Refusal::new(
                Outcome::NotFound,
                "No such attachment is live.",
            )),
            Some((_, token)) if !token.matches(presented) => Err(Refusal::new(
                Outcome::Refused,
                "That is not this attachment's token.",
            )),
            Some(_) => {
                state.attachments.remove(&id);
                Ok(())
            }
        }
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

    fn token() -> AttachmentToken {
        AttachmentToken::generate().expect("token")
    }

    #[test]
    fn a_mind_attaches_by_moniker_or_id_and_detaches() {
        let lobby = Lobby::new();
        let ada = lobby.create_companion("ada", "Ada").expect("ada");
        let (by_moniker, companion) = lobby.attach("ada", token()).expect("by moniker");
        assert_eq!(companion, ada);
        let id_token = token();
        let (by_id, _) = lobby
            .attach(&ada.id.to_string(), id_token.clone())
            .expect("by id");
        assert_ne!(
            by_moniker.id, by_id.id,
            "every attach is its own attachment"
        );
        assert_eq!(lobby.attachment_count(), 2);
        assert_eq!(
            lobby
                .attachment(by_id.id, id_token.expose())
                .map(|(_, c)| c.id),
            Ok(ada.id)
        );
        assert!(lobby.detach(by_id.id, id_token.expose()).is_ok());
        assert_eq!(
            lobby
                .detach(by_id.id, id_token.expose())
                .expect_err("gone")
                .outcome,
            Outcome::NotFound
        );
        assert!(lobby.attachment(by_id.id, id_token.expose()).is_err());
        assert_eq!(
            lobby.attach("bea", token()).expect_err("unknown").outcome,
            Outcome::NotFound
        );
    }

    #[test]
    fn an_attachment_answers_only_to_its_own_token() {
        let lobby = Lobby::new();
        lobby.create_companion("ada", "Ada").expect("ada");
        let (first, _) = lobby.attach("ada", token()).expect("first");
        let second_token = token();
        lobby.attach("ada", second_token.clone()).expect("second");
        let refused = lobby
            .attachment(first.id, second_token.expose())
            .expect_err("another attachment's token");
        assert_eq!(refused.outcome, Outcome::Refused);
        assert_eq!(
            lobby
                .detach(first.id, second_token.expose())
                .expect_err("refused")
                .outcome,
            Outcome::Refused
        );
        assert_eq!(lobby.attachment_count(), 2, "a refused detach ends nothing");
    }
}
