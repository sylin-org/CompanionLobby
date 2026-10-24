//! The lobby's state and the commands that change it.
//!
//! The truth is the state file, held by [`Store`]; live attachments are
//! runtime facts held beside it. Everything that changes either goes through
//! one command pipeline, in this order:
//!
//! 1. **validate**: a command that cannot apply is refused with an outcome
//!    and a sentence the lobby wrote, and the refusal is journaled;
//! 2. **journal, pending**: an entry saying the command is under way is
//!    written before anything changes, and a command whose entry cannot be
//!    written is refused with nothing changed;
//! 3. **apply and commit**: a change to the state file is one commit that
//!    lands whole or not at all; a runtime change is made in memory;
//! 4. **journal, settled**: an entry with the command's outcome follows. When
//!    it cannot be written the command still answers with what really
//!    happened, never a false error, and the journal's health turns to
//!    failing for the sponsor to see;
//! 5. **emit**: the command's events go to one event pipeline.
//!
//! The event pipeline runs every event through small handlers, in order,
//! before the next command starts: the feed handler turns it into an item of
//! the live feed the manager page follows, and the cascade handler ends the
//! attachments of a companion that was removed, a command of its own
//! initiated by the lobby.
//!
//! A companion is created, renamed and removed only by the sponsor, through
//! the operator API; a rename or removal names the companion's revision and
//! is refused when the companion changed since, and a removal is refused
//! while minds are attached as the companion unless the sponsor confirms the
//! cascade. An attachment exists only because the attach shim presented the
//! per-install secret and named a companion by id or moniker; it keeps the
//! token its shim presents on every later request.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};

use lobby_contracts::{
    Attachment, AttachmentId, Companion, CompanionId, DisplayName, Initiator, JournalEntry,
    JournalKind, Moniker, NameProblem, Outcome, Sponsor,
};

use crate::journal::{Journal, Note, Page, PageProblem};
use crate::keyring::AttachmentToken;
use crate::projections::{self, Feed, Subscription};
use crate::store::{CompanionRecord, State, Store};
use crate::trace;

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

/// A companion and how many minds are attached as it right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompanionView {
    /// The companion as the state file keeps it.
    pub record: CompanionRecord,
    /// How many live attachments act as it.
    pub attachments: usize,
}

/// Something a command did, for the event pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A companion was created or renamed, or a mind attached or detached as it.
    CompanionChanged(CompanionView),
    /// A companion was removed.
    CompanionRemoved(CompanionId),
    /// An entry reached the journal.
    Journaled(JournalEntry),
    /// The journal started failing (`false`) or recovered (`true`).
    JournalHealth(bool),
}

/// What a rename changes; a field left `None` stays as it is.
#[derive(Debug, Clone, Copy, Default)]
pub struct Rename<'a> {
    /// The new moniker.
    pub moniker: Option<&'a str>,
    /// The new display name.
    pub display_name: Option<&'a str>,
}

/// The lobby as one consistent picture, for a page to start from: the feed
/// position is the last item that picture already includes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// Every companion, oldest first.
    pub companions: Vec<CompanionView>,
    /// The store id that names the journal.
    pub store_id: String,
    /// The live feed's epoch.
    pub epoch: String,
    /// The live feed's position.
    pub position: u64,
    /// Whether the journal took every entry since the daemon started, or the
    /// last one after it failed.
    pub journal_healthy: bool,
    /// The journal's last sequence number.
    pub journal_high_water: u64,
}

struct Live {
    attachment: Attachment,
    token: AttachmentToken,
}

struct Core {
    store: Store,
    journal: Journal,
    attachments: HashMap<AttachmentId, Live>,
    journal_healthy: bool,
    events: VecDeque<Event>,
}

/// The lobby: one owner of the state, the journal and the live feed.
pub struct Lobby {
    core: Mutex<Core>,
    feed: Feed,
    command: String,
}

/// The fixed names commands carry in the journal.
const CREATE_COMPANION: &str = "create_companion";
const RENAME_COMPANION: &str = "rename_companion";
const REMOVE_COMPANION: &str = "remove_companion";
const ATTACH: &str = "attach";
const DETACH: &str = "detach";

impl Lobby {
    /// Opens the state file and the journal in `dir`. `command` is the path
    /// host configurations name to start the shim.
    pub fn open(dir: &Path, command: &str) -> Result<Self, String> {
        let store = Store::open(dir).map_err(|problem| problem.to_string())?;
        let journal = Journal::open(dir, &store.state().store_id)
            .map_err(|error| format!("cannot read the journal: {error}"))?;
        Ok(Self {
            core: Mutex::new(Core {
                store,
                journal,
                attachments: HashMap::new(),
                journal_healthy: true,
                events: VecDeque::new(),
            }),
            feed: Feed::new(),
            command: command.to_owned(),
        })
    }

    fn core(&self) -> MutexGuard<'_, Core> {
        // A poisoned lock means a panic mid-command; the state is still the
        // last committed one, because a commit lands whole or not at all.
        self.core.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs one command, then every event it emitted, before the next command.
    fn run<T>(&self, command: impl FnOnce(&mut Core) -> Result<T, Refusal>) -> Result<T, Refusal> {
        let mut core = self.core();
        let result = command(&mut core);
        core.settle_events(&self.feed, &self.command);
        result
    }

    /// Creates a companion; the moniker must be free.
    pub fn create_companion(
        &self,
        moniker: &str,
        display_name: &str,
    ) -> Result<CompanionView, Refusal> {
        self.run(|core| {
            let sponsor = Initiator::Sponsor;
            let validated = Moniker::new(moniker)
                .map_err(|problem| name_refusal("moniker", problem))
                .and_then(|moniker| {
                    DisplayName::new(display_name)
                        .map(|display_name| (moniker, display_name))
                        .map_err(|problem| name_refusal("display name", problem))
                })
                .and_then(|(moniker, display_name)| {
                    core.moniker_free(&moniker, None)?;
                    Ok((moniker, display_name))
                });
            let (moniker, display_name) = validated
                .map_err(|refusal| core.refused(sponsor, None, CREATE_COMPANION, refusal))?;
            let record = CompanionRecord {
                companion: Companion {
                    id: CompanionId::from_u128(uuid::Uuid::now_v7().as_u128()),
                    moniker,
                    display_name,
                    sponsor: Sponsor::LOCAL,
                },
                revision: 1,
            };
            let id = record.companion.id;
            let mut next = core.store.state().clone();
            next.companions.push(record.clone());
            core.commit(sponsor, Some(id), CREATE_COMPANION, next)?;
            let view = core.view(&record);
            core.events.push_back(Event::CompanionChanged(view.clone()));
            Ok(view)
        })
    }

    /// Renames the companion `id`, which must still be at `expected` revision.
    pub fn rename_companion(
        &self,
        id: CompanionId,
        expected: u64,
        rename: Rename<'_>,
    ) -> Result<CompanionView, Refusal> {
        self.run(|core| {
            let sponsor = Initiator::Sponsor;
            let record = core
                .current(id, expected)
                .and_then(|mut record| {
                    if rename.moniker.is_none() && rename.display_name.is_none() {
                        return Err(Refusal::new(
                            Outcome::InvalidInput,
                            "A rename needs a new moniker, a new display name or both.",
                        ));
                    }
                    if let Some(moniker) = rename.moniker {
                        let moniker = Moniker::new(moniker)
                            .map_err(|problem| name_refusal("moniker", problem))?;
                        core.moniker_free(&moniker, Some(id))?;
                        record.companion.moniker = moniker;
                    }
                    if let Some(display_name) = rename.display_name {
                        record.companion.display_name = DisplayName::new(display_name)
                            .map_err(|problem| name_refusal("display name", problem))?;
                    }
                    record.revision += 1;
                    Ok(record)
                })
                .map_err(|refusal| core.refused(sponsor, Some(id), RENAME_COMPANION, refusal))?;
            let mut next = core.store.state().clone();
            for slot in &mut next.companions {
                if slot.companion.id == id {
                    *slot = record.clone();
                }
            }
            core.commit(sponsor, Some(id), RENAME_COMPANION, next)?;
            let view = core.view(&record);
            core.events.push_back(Event::CompanionChanged(view.clone()));
            Ok(view)
        })
    }

    /// Removes the companion `id`, which must still be at `expected`
    /// revision. While minds are attached as it the removal is refused unless
    /// `cascade` confirms that their attachments end too. Returns how many
    /// attachments ended.
    pub fn remove_companion(
        &self,
        id: CompanionId,
        expected: u64,
        cascade: bool,
    ) -> Result<usize, Refusal> {
        self.run(|core| {
            let sponsor = Initiator::Sponsor;
            let attached = core.attached_as(id);
            core.current(id, expected)
                .and_then(|record| {
                    if attached > 0 && !cascade {
                        return Err(Refusal::new(
                            Outcome::Conflict,
                            format!(
                                "{} has {attached} live attachment{}; removing it ends {}. Confirm the cascade to remove it anyway.",
                                record.companion.display_name,
                                if attached == 1 { "" } else { "s" },
                                if attached == 1 { "it" } else { "them" },
                            ),
                        ));
                    }
                    Ok(())
                })
                .map_err(|refusal| core.refused(sponsor, Some(id), REMOVE_COMPANION, refusal))?;
            let mut next = core.store.state().clone();
            next.companions.retain(|record| record.companion.id != id);
            core.commit(sponsor, Some(id), REMOVE_COMPANION, next)?;
            core.events.push_back(Event::CompanionRemoved(id));
            Ok(attached)
        })
    }

    /// Attaches a mind as the companion named by id or moniker; `token` is
    /// what the attachment presents from then on.
    pub fn attach(
        &self,
        name: &str,
        token: AttachmentToken,
    ) -> Result<(Attachment, Companion), Refusal> {
        self.run(|core| {
            let attachment = AttachmentId::from_u128(uuid::Uuid::now_v7().as_u128());
            let mind = Initiator::Mind { attachment };
            let Some(record) = find(&core.store.state().companions, name).cloned() else {
                return Err(core.refused(
                    mind,
                    None,
                    ATTACH,
                    Refusal::new(
                        Outcome::NotFound,
                        "No companion has that id or moniker; the sponsor creates companions on the manager page.",
                    ),
                ));
            };
            let companion = record.companion.clone();
            core.begin(mind, Some(companion.id), ATTACH)?;
            let attachment = Attachment {
                id: attachment,
                companion: companion.id,
            };
            core.attachments.insert(
                attachment.id,
                Live {
                    attachment: attachment.clone(),
                    token,
                },
            );
            core.settle(mind, Some(companion.id), ATTACH, Outcome::Done);
            let view = core.view(&record);
            core.events.push_back(Event::CompanionChanged(view));
            Ok((attachment, companion))
        })
    }

    /// The live attachment and its companion, when `presented` is its token.
    pub fn attachment(
        &self,
        id: AttachmentId,
        presented: &str,
    ) -> Result<(Attachment, Companion), Refusal> {
        let core = self.core();
        let live = core.attachments.get(&id).ok_or_else(gone)?;
        if !live.token.matches(presented) {
            return Err(Refusal::new(
                Outcome::Refused,
                "That is not this attachment's token.",
            ));
        }
        let attachment = live.attachment.clone();
        let companion = core
            .store
            .state()
            .companion(attachment.companion)
            .map(|record| record.companion.clone())
            .ok_or_else(gone)?;
        Ok((attachment, companion))
    }

    /// Ends an attachment, when `presented` is its token. A refused detach is
    /// not journaled: nothing proved who asked.
    pub fn detach(&self, id: AttachmentId, presented: &str) -> Result<(), Refusal> {
        self.run(|core| {
            let live = core
                .attachments
                .get(&id)
                .ok_or_else(|| Refusal::new(Outcome::NotFound, "No such attachment is live."))?;
            if !live.token.matches(presented) {
                return Err(Refusal::new(
                    Outcome::Refused,
                    "That is not this attachment's token.",
                ));
            }
            let companion = live.attachment.companion;
            let mind = Initiator::Mind { attachment: id };
            core.begin(mind, Some(companion), DETACH)?;
            core.attachments.remove(&id);
            core.settle(mind, Some(companion), DETACH, Outcome::Done);
            core.announce(companion);
            Ok(())
        })
    }

    /// Every companion with its live attachments, oldest first.
    pub fn companions(&self) -> Vec<CompanionView> {
        let core = self.core();
        core.views()
    }

    /// The lobby as one consistent picture.
    pub fn snapshot(&self) -> Snapshot {
        let core = self.core();
        Snapshot {
            companions: core.views(),
            store_id: core.store.state().store_id.clone(),
            epoch: self.feed.epoch().to_owned(),
            // Items are published only while the core is held, so this is
            // exactly the last item the picture includes.
            position: self.feed.position(),
            journal_healthy: core.journal_healthy,
            journal_high_water: core.journal.high_water(),
        }
    }

    /// A page of the journal; see [`Journal::page`].
    pub fn journal_page(&self, before: Option<&str>, limit: usize) -> Result<Page, PageProblem> {
        self.core().journal.page(before, limit)
    }

    /// Subscribes to the live feed after the position `last`.
    pub fn subscribe(&self, last: Option<&str>) -> Subscription {
        self.feed.subscribe(last)
    }

    /// The live feed.
    pub fn feed(&self) -> &Feed {
        &self.feed
    }

    /// The path host configurations name to start the shim.
    pub fn command(&self) -> &str {
        &self.command
    }

    /// How many attachments are live.
    pub fn attachment_count(&self) -> usize {
        self.core().attachments.len()
    }
}

fn gone() -> Refusal {
    Refusal::new(
        Outcome::NotFound,
        "No such attachment is live; attach again.",
    )
}

impl Core {
    fn attached_as(&self, companion: CompanionId) -> usize {
        self.attachments
            .values()
            .filter(|live| live.attachment.companion == companion)
            .count()
    }

    fn view(&self, record: &CompanionRecord) -> CompanionView {
        CompanionView {
            record: record.clone(),
            attachments: self.attached_as(record.companion.id),
        }
    }

    fn views(&self) -> Vec<CompanionView> {
        self.store
            .state()
            .companions
            .iter()
            .map(|record| self.view(record))
            .collect()
    }

    /// Emits the current view of `companion`, if it still exists.
    fn announce(&mut self, companion: CompanionId) {
        if let Some(record) = self.store.state().companion(companion).cloned() {
            let view = self.view(&record);
            self.events.push_back(Event::CompanionChanged(view));
        }
    }

    /// The companion `id` when it is still at revision `expected`.
    fn current(&self, id: CompanionId, expected: u64) -> Result<CompanionRecord, Refusal> {
        let record = self
            .store
            .state()
            .companion(id)
            .cloned()
            .ok_or_else(|| Refusal::new(Outcome::NotFound, "No companion has that id."))?;
        if record.revision != expected {
            return Err(Refusal::new(
                Outcome::Conflict,
                format!(
                    "{} changed since it was read (revision {} now, {expected} named); reload it and try again.",
                    record.companion.display_name, record.revision
                ),
            ));
        }
        Ok(record)
    }

    /// Refuses a moniker another companion than `except` already has.
    fn moniker_free(&self, moniker: &Moniker, except: Option<CompanionId>) -> Result<(), Refusal> {
        let taken = self.store.state().companions.iter().any(|record| {
            record.companion.moniker == *moniker && Some(record.companion.id) != except
        });
        if taken {
            return Err(Refusal::new(
                Outcome::Conflict,
                format!("A companion named {moniker} already exists."),
            ));
        }
        Ok(())
    }

    /// Writes one journal entry for the command `name`, emitting it.
    fn note(
        &mut self,
        initiator: Initiator,
        companion: Option<CompanionId>,
        name: &'static str,
        outcome: Outcome,
    ) -> io::Result<JournalEntry> {
        let note = Note {
            initiator,
            companion,
            kind: JournalKind::Command { name },
            outcome,
        };
        match self.journal.append(note) {
            Ok(entry) => {
                self.events.push_back(Event::Journaled(entry.clone()));
                if !self.journal_healthy {
                    self.journal_healthy = true;
                    trace::line("the journal takes entries again");
                    self.events.push_back(Event::JournalHealth(true));
                }
                Ok(entry)
            }
            Err(error) => {
                trace::line(&format!(
                    "the journal could not record {name} ({}): {error}",
                    outcome.code()
                ));
                if self.journal_healthy {
                    self.journal_healthy = false;
                    self.events.push_back(Event::JournalHealth(false));
                }
                Err(error)
            }
        }
    }

    /// Journals a refusal; the refusal stands whether or not that worked.
    fn refused(
        &mut self,
        initiator: Initiator,
        companion: Option<CompanionId>,
        name: &'static str,
        refusal: Refusal,
    ) -> Refusal {
        let _ = self.note(initiator, companion, name, refusal.outcome);
        refusal
    }

    /// The pending entry that precedes every effect: no entry, no effect.
    fn begin(
        &mut self,
        initiator: Initiator,
        companion: Option<CompanionId>,
        name: &'static str,
    ) -> Result<(), Refusal> {
        self.note(initiator, companion, name, Outcome::Pending)
            .map(drop)
            .map_err(|_| {
                Refusal::new(
                    Outcome::TemporarilyUnavailable,
                    "The journal could not be written, so nothing was changed; the trace log in the data directory says why.",
                )
            })
    }

    /// The settled entry that follows an effect. When it cannot be written
    /// the effect still stands and is reported as it happened; the journal's
    /// health says what is missing.
    fn settle(
        &mut self,
        initiator: Initiator,
        companion: Option<CompanionId>,
        name: &'static str,
        outcome: Outcome,
    ) {
        let _ = self.note(initiator, companion, name, outcome);
    }

    /// The pipeline of a change to the state file: pending entry, one commit,
    /// settled entry.
    fn commit(
        &mut self,
        initiator: Initiator,
        companion: Option<CompanionId>,
        name: &'static str,
        next: State,
    ) -> Result<(), Refusal> {
        self.begin(initiator, companion, name)?;
        if let Err(error) = self.store.commit(next) {
            trace::line(&format!("{name} could not be committed: {error}"));
            let refusal = Refusal::new(
                Outcome::TemporarilyUnavailable,
                "The lobby could not write its state file, so nothing was changed; try again.",
            );
            self.settle(initiator, companion, name, refusal.outcome);
            return Err(refusal);
        }
        self.settle(initiator, companion, name, Outcome::Done);
        Ok(())
    }

    /// Runs every emitted event through the handlers, in order, including the
    /// events the handlers' own commands emit.
    fn settle_events(&mut self, feed: &Feed, command: &str) {
        while let Some(event) = self.events.pop_front() {
            // The feed handler: every event becomes one item of the live feed.
            feed.publish(&projections::event_json(
                &event,
                &self.store.state().store_id,
                command,
            ));
            // The cascade handler: a removed companion's attachments end.
            if let Event::CompanionRemoved(companion) = event {
                self.end_attachments_of(companion);
            }
        }
    }

    /// Ends every attachment of `companion`, a command the lobby initiates.
    /// The companion is gone, so the attachments end even when the journal
    /// cannot record it.
    fn end_attachments_of(&mut self, companion: CompanionId) {
        let ended: Vec<AttachmentId> = self
            .attachments
            .values()
            .filter(|live| live.attachment.companion == companion)
            .map(|live| live.attachment.id)
            .collect();
        for id in ended {
            self.attachments.remove(&id);
            self.settle(Initiator::Lobby, Some(companion), DETACH, Outcome::Done);
            trace::line(&format!(
                "attachment {id} ended with its companion; {} live",
                self.attachments.len()
            ));
        }
    }
}

fn find<'a>(companions: &'a [CompanionRecord], name: &str) -> Option<&'a CompanionRecord> {
    match CompanionId::parse(name) {
        Some(id) => companions.iter().find(|record| record.companion.id == id),
        None => companions
            .iter()
            .find(|record| record.companion.moniker.as_str() == name),
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
    use std::path::PathBuf;

    struct Dir(PathBuf);

    impl Dir {
        fn new(label: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "lobby-domain-{label}-{}-{}",
                std::process::id(),
                uuid::Uuid::now_v7().simple()
            ));
            std::fs::create_dir_all(&dir).expect("temp dir");
            Self(dir)
        }

        fn lobby(&self) -> Lobby {
            Lobby::open(&self.0, r"C:\Tools\companion-lobby.exe").expect("open")
        }

        fn journal(&self) -> Vec<serde_json::Value> {
            std::fs::read_to_string(self.0.join(crate::journal::JOURNAL_FILE))
                .unwrap_or_default()
                .lines()
                .map(|line| serde_json::from_str(line).expect("JSON"))
                .collect()
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn token() -> AttachmentToken {
        AttachmentToken::generate().expect("token")
    }

    fn rename<'a>(moniker: Option<&'a str>, display_name: Option<&'a str>) -> Rename<'a> {
        Rename {
            moniker,
            display_name,
        }
    }

    #[test]
    fn companions_are_created_renamed_and_removed_under_their_revision() {
        let dir = Dir::new("crud");
        let lobby = dir.lobby();
        let ada = lobby.create_companion("ada", "Ada").expect("ada");
        let id = ada.record.companion.id;
        assert_eq!(ada.record.revision, 1);
        assert_eq!(
            lobby
                .create_companion("ada", "Another")
                .expect_err("taken")
                .outcome,
            Outcome::Conflict
        );
        assert_eq!(
            lobby
                .create_companion("Ada!", "Ada")
                .expect_err("invalid")
                .outcome,
            Outcome::InvalidInput
        );
        let renamed = lobby
            .rename_companion(id, 1, rename(Some("ada-l"), Some("Ada Lovelace")))
            .expect("rename");
        assert_eq!(renamed.record.revision, 2);
        assert_eq!(renamed.record.companion.moniker.as_str(), "ada-l");
        assert_eq!(renamed.record.companion.id, id, "the id never changes");
        let stale = lobby
            .rename_companion(id, 1, rename(None, Some("Stale")))
            .expect_err("a stale revision");
        assert_eq!(stale.outcome, Outcome::Conflict);
        assert_eq!(
            lobby
                .remove_companion(id, 1, false)
                .expect_err("stale")
                .outcome,
            Outcome::Conflict
        );
        lobby.create_companion("bea", "Bea").expect("bea");
        assert_eq!(
            lobby
                .rename_companion(id, 2, rename(Some("bea"), None))
                .expect_err("taken")
                .outcome,
            Outcome::Conflict
        );
        // What was committed survives a restart.
        drop(lobby);
        let lobby = dir.lobby();
        let names: Vec<String> = lobby
            .companions()
            .iter()
            .map(|view| view.record.companion.moniker.as_str().to_owned())
            .collect();
        assert_eq!(names, ["ada-l", "bea"]);
        assert_eq!(lobby.remove_companion(id, 2, false).expect("remove"), 0);
        assert_eq!(
            lobby
                .remove_companion(id, 2, false)
                .expect_err("gone")
                .outcome,
            Outcome::NotFound
        );
        assert_eq!(lobby.companions().len(), 1);
    }

    #[test]
    fn a_companion_with_live_attachments_is_removed_only_by_a_cascade() {
        let dir = Dir::new("cascade");
        let lobby = dir.lobby();
        let ada = lobby.create_companion("ada", "Ada").expect("ada");
        let id = ada.record.companion.id;
        let first = token();
        let (attachment, _) = lobby.attach("ada", first.clone()).expect("by moniker");
        lobby.attach(&id.to_string(), token()).expect("by id");
        assert_eq!(lobby.companions()[0].attachments, 2);
        let refused = lobby.remove_companion(id, 1, false).expect_err("attached");
        assert_eq!(refused.outcome, Outcome::Conflict);
        assert!(
            refused.message.contains("2 live attachments"),
            "{}",
            refused.message
        );
        assert_eq!(lobby.attachment_count(), 2);
        assert_eq!(lobby.remove_companion(id, 1, true).expect("cascade"), 2);
        assert_eq!(lobby.attachment_count(), 0);
        assert_eq!(
            lobby
                .attachment(attachment.id, first.expose())
                .expect_err("ended")
                .outcome,
            Outcome::NotFound
        );
        let detaches: Vec<serde_json::Value> = dir
            .journal()
            .into_iter()
            .filter(|entry| entry["command"] == DETACH)
            .collect();
        assert_eq!(detaches.len(), 2);
        assert!(detaches.iter().all(|entry| entry["initiator"] == "lobby"));
    }

    #[test]
    fn every_command_is_journaled_pending_then_settled_with_its_initiator() {
        let dir = Dir::new("journal");
        let lobby = dir.lobby();
        let ada = lobby.create_companion("ada", "Ada").expect("ada");
        let (attachment, _) = lobby.attach("ada", token()).expect("attach");
        let detach_token = token();
        let (second, _) = lobby.attach("ada", detach_token.clone()).expect("attach");
        lobby
            .detach(second.id, detach_token.expose())
            .expect("detach");
        let _ = lobby.create_companion("ada", "Again");
        let entries = dir.journal();
        let summary: Vec<(String, String, String)> = entries
            .iter()
            .map(|entry| {
                (
                    entry["command"].as_str().expect("command").to_owned(),
                    entry["initiator"].as_str().expect("initiator").to_owned(),
                    entry["outcome"].as_str().expect("outcome").to_owned(),
                )
            })
            .collect();
        let expected = [
            (CREATE_COMPANION, "sponsor", "pending"),
            (CREATE_COMPANION, "sponsor", "done"),
            (ATTACH, "mind", "pending"),
            (ATTACH, "mind", "done"),
            (ATTACH, "mind", "pending"),
            (ATTACH, "mind", "done"),
            (DETACH, "mind", "pending"),
            (DETACH, "mind", "done"),
            (CREATE_COMPANION, "sponsor", "conflict"),
        ];
        let expected: Vec<(String, String, String)> = expected
            .iter()
            .map(|(a, b, c)| ((*a).to_owned(), (*b).to_owned(), (*c).to_owned()))
            .collect();
        assert_eq!(summary, expected);
        assert_eq!(entries[2]["attachment"], attachment.id.to_string());
        assert_eq!(entries[0]["companion"], ada.record.companion.id.to_string());
        let sequences: Vec<u64> = entries
            .iter()
            .map(|entry| entry["seq"].as_u64().expect("seq"))
            .collect();
        assert!(sequences.windows(2).all(|pair| pair[1] == pair[0] + 1));
    }

    #[test]
    fn a_journal_that_cannot_be_written_refuses_the_command_before_any_effect() {
        let dir = Dir::new("refuse");
        let lobby = dir.lobby();
        let state_before = std::fs::read(dir.0.join(crate::store::STATE_FILE)).expect("read");
        lobby.core().journal.refuse_appends_from(1);
        let refused = lobby.create_companion("ada", "Ada").expect_err("refused");
        assert_eq!(refused.outcome, Outcome::TemporarilyUnavailable);
        assert!(lobby.companions().is_empty());
        assert_eq!(
            std::fs::read(dir.0.join(crate::store::STATE_FILE)).expect("read"),
            state_before,
            "nothing was committed"
        );
        assert!(!lobby.snapshot().journal_healthy);
        assert!(
            lobby.attach("ada", token()).is_err(),
            "nothing to attach to, and nothing journaled"
        );
    }

    #[test]
    fn a_settled_entry_that_cannot_be_written_never_turns_a_landed_change_into_an_error() {
        let dir = Dir::new("settle");
        let lobby = dir.lobby();
        // The pending entry is 1; the settled entry 2 fails.
        lobby.core().journal.refuse_appends_from(2);
        let ada = lobby
            .create_companion("ada", "Ada")
            .expect("the change landed");
        assert_eq!(lobby.companions(), vec![ada]);
        let snapshot = lobby.snapshot();
        assert!(
            !snapshot.journal_healthy,
            "the sponsor sees the journal failing"
        );
        assert_eq!(snapshot.journal_high_water, 1);
        drop(lobby);
        assert_eq!(dir.lobby().companions().len(), 1, "it was committed");
    }

    #[test]
    fn every_event_reaches_the_feed_in_order() {
        let dir = Dir::new("feed");
        let lobby = dir.lobby();
        let subscription = lobby.subscribe(None);
        assert_eq!(subscription.reset, Some("fresh"));
        let mut receiver = subscription.receiver;
        let ada = lobby.create_companion("ada", "Ada").expect("ada");
        lobby
            .rename_companion(ada.record.companion.id, 1, rename(None, Some("Ada L")))
            .expect("rename");
        let kinds: Vec<String> = std::iter::from_fn(|| receiver.try_recv().ok())
            .map(|item| {
                let value: serde_json::Value = serde_json::from_str(&item.data).expect("JSON");
                value["kind"].as_str().expect("kind").to_owned()
            })
            .collect();
        assert_eq!(
            kinds,
            [
                "journal",
                "journal",
                "companion",
                "journal",
                "journal",
                "companion"
            ]
        );
        let snapshot = lobby.snapshot();
        assert_eq!(snapshot.position, 6);
        let resumed = lobby.subscribe(Some(&format!("{}:4", snapshot.epoch)));
        assert_eq!(resumed.reset, None);
        assert_eq!(
            resumed
                .replay
                .iter()
                .map(|item| item.seq)
                .collect::<Vec<_>>(),
            [5, 6]
        );
        assert_eq!(lobby.subscribe(Some("other:4")).reset, Some("another_feed"));
        assert_eq!(
            lobby
                .subscribe(Some(&format!("{}:9", snapshot.epoch)))
                .reset,
            Some("ahead")
        );
    }
}
