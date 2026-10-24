//! The lobby's state and the commands that change it.
//!
//! The truth is the state file, held by [`Store`]; live attachments and the
//! sign-ins the lobby waits on are runtime facts held beside it. Everything
//! that changes either goes through one command pipeline, in this order:
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
//! A command that needs an adapter (completing a sign-in, establishing a
//! connection, a tool call) is split around it: validation and the pending
//! entry under the lobby, the adapter on the request's blocking thread with
//! nothing held, then a re-check of what was pinned, the commit and the
//! settled entry under the lobby again. What changed while the adapter ran
//! wins: a credential revoked meanwhile is never connected, and a connection
//! revoked meanwhile never delivers its late result.
//!
//! The event pipeline runs every event through small handlers, in order,
//! before the next command starts: the feed handler turns it into an item of
//! the live feed the manager page follows, the catalog handler tells every
//! attachment of a companion whose connections changed that its tools
//! changed, and the cascade handler ends the attachments of a companion that
//! was removed, a command of its own initiated by the lobby. The handlers run
//! while the lobby is held, which is what makes a snapshot's feed position
//! exact, so a handler only changes state in memory and writes the journal;
//! it never calls an adapter, which would hold the lobby across a service's
//! I/O.
//!
//! A companion is created, renamed and removed only by the sponsor, through
//! the operator API; a rename or removal names the companion's revision and
//! is refused when the companion changed since, and a removal is refused
//! while minds are attached as the companion unless the sponsor confirms the
//! cascade. Removing a companion removes its credentials and connections too.
//! An attachment exists only because the attach shim presented the
//! per-install secret and named a companion by id or moniker; it keeps the
//! token its shim presents on every later request. When the daemon starts and
//! finds an attach secret that could have been read or planted, replacing it
//! is a command the lobby initiates, journaled like any other.
//!
//! A credential exists only because its adapter completed a sign-in the
//! sponsor started: a companion holds at most one ready credential of a
//! provider kind, and an account belongs to one companion, which keeps it
//! after the credential is revoked. A connection exists only because its
//! service adapter established it with a credential of its companion.
//! Revoking a credential revokes every connection made from it in the same
//! commit, under one journal entry, and deletes what they keep.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use lobby_contracts::{
    Address, Attachment, AttachmentId, Companion, CompanionId, Connection, ConnectionId,
    ConnectionState, Credential, CredentialAdapter, CredentialId, CredentialState, DisplayName,
    Initiator, JournalEntry, JournalKind, Label, Moniker, NameProblem, Outcome, ProviderKind,
    SealedMaterial, ServiceAdapter, ServiceName, SignInRequest, SignInReturn, SignInStart, Sponsor,
    ToolArguments, ToolResult,
};
use serde_json::{Map, Value};
use tokio::sync::watch;

use crate::broker::Broker;
use crate::calls::{self, Ticket};
use crate::journal::{Journal, Note, Page, PageProblem};
use crate::keyring::{self, AttachmentToken};
use crate::projections::{self, Feed, Reached, Subscription};
use crate::registry::Registry;
use crate::store::{CompanionRecord, State, Store};
use crate::trace;

/// How long the lobby waits for a sign-in it started to come back.
pub const SIGN_IN_TIME_LIMIT: Duration = Duration::from_secs(10 * 60);
/// How many sign-ins the lobby waits on at once; the oldest gives way.
pub const SIGN_INS_WAITING: usize = 32;

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

    /// The refusal of an adapter that answered `outcome`, in the lobby's words.
    fn from_adapter(outcome: Outcome) -> Self {
        let message = match outcome {
            Outcome::Unsupported => {
                "The credential cannot give this service the kind of proof it asks for."
            }
            Outcome::NeedsSignIn => {
                "The provider refused this credential; sign the companion in again."
            }
            Outcome::Refused => "The provider or the service refused.",
            other => other.sentence(),
        };
        Self::new(outcome, message)
    }
}

/// Why a tool call was not made or did not succeed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallRefusal {
    /// No tool of that name is in this attachment's catalog.
    UnknownTool,
    /// The call was refused or ended with this outcome.
    Ended(Refusal),
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
    /// A credential was signed in or revoked.
    CredentialChanged(Credential),
    /// A connection was established or revoked.
    ConnectionChanged(Connection),
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

/// What the sponsor asks for when granting a connection.
#[derive(Debug, Clone, Copy)]
pub struct Grant<'a> {
    /// The companion that will reach the service.
    pub companion: CompanionId,
    /// The credential it reaches the service with.
    pub credential: CredentialId,
    /// The service's name.
    pub service: &'a str,
    /// Where the service is.
    pub address: &'a str,
    /// The sponsor's name for the connection.
    pub label: &'a str,
}

/// The lobby as one consistent picture, for a page to start from: the feed
/// position is the last item that picture already includes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// Every companion, oldest first.
    pub companions: Vec<CompanionView>,
    /// Every credential, oldest first.
    pub credentials: Vec<Credential>,
    /// Every connection, oldest first.
    pub connections: Vec<Connection>,
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
    /// Advances whenever the attachment's tools change.
    catalog: watch::Sender<u64>,
}

/// A sign-in the lobby started and waits to see come back.
struct Awaited {
    companion: CompanionId,
    kind: ProviderKind,
    request: SignInRequest,
    started: Instant,
}

struct Core {
    store: Store,
    journal: Journal,
    attachments: HashMap<AttachmentId, Live>,
    awaited: HashMap<String, Awaited>,
    journal_healthy: bool,
    events: VecDeque<Event>,
}

/// The lobby: one owner of the state, the journal and the live feed.
pub struct Lobby {
    core: Mutex<Core>,
    feed: Feed,
    command: String,
    dir: PathBuf,
    registry: Registry,
}

/// The fixed names commands carry in the journal.
const CREATE_COMPANION: &str = "create_companion";
const RENAME_COMPANION: &str = "rename_companion";
const REMOVE_COMPANION: &str = "remove_companion";
const ATTACH: &str = "attach";
const DETACH: &str = "detach";
const RENEW_ATTACH_SECRET: &str = "renew_attach_secret";
const BEGIN_SIGN_IN: &str = "begin_sign_in";
const COMPLETE_SIGN_IN: &str = "complete_sign_in";
const REVOKE_CREDENTIAL: &str = "revoke_credential";
const GRANT_CONNECTION: &str = "grant_connection";
const REVOKE_CONNECTION: &str = "revoke_connection";
const SWEEP_KEYRING: &str = "sweep_keyring";

/// Who a journal entry is about, short of its kind and outcome.
#[derive(Debug, Clone)]
struct Subject {
    initiator: Initiator,
    companion: Option<CompanionId>,
    connection: Option<ConnectionId>,
    service: Option<ServiceName>,
    name: &'static str,
}

impl Subject {
    fn new(initiator: Initiator, companion: Option<CompanionId>, name: &'static str) -> Self {
        Self {
            initiator,
            companion,
            connection: None,
            service: None,
            name,
        }
    }

    fn through(mut self, connection: Option<ConnectionId>, service: Option<ServiceName>) -> Self {
        self.connection = connection;
        self.service = service;
        self
    }

    fn note(&self, outcome: Outcome) -> Note {
        Note {
            initiator: self.initiator,
            companion: self.companion,
            connection: self.connection,
            service: self.service.clone(),
            tool: None,
            kind: JournalKind::Command { name: self.name },
            outcome,
        }
    }
}

fn new_id() -> u128 {
    uuid::Uuid::now_v7().as_u128()
}

impl Lobby {
    /// Opens the state file and the journal in `dir`. `command` is the path
    /// host configurations name to start the shim; `registry` holds the
    /// adapters the lobby runs with.
    pub fn open(dir: &Path, command: &str, registry: Registry) -> Result<Self, String> {
        let store = Store::open(dir).map_err(|problem| problem.to_string())?;
        let journal = Journal::open(dir, &store.state().store_id)
            .map_err(|error| format!("cannot read the journal: {error}"))?;
        Ok(Self {
            core: Mutex::new(Core {
                store,
                journal,
                attachments: HashMap::new(),
                awaited: HashMap::new(),
                journal_healthy: true,
                events: VecDeque::new(),
            }),
            feed: Feed::new(),
            command: command.to_owned(),
            dir: dir.to_path_buf(),
            registry,
        })
    }

    fn core(&self) -> MutexGuard<'_, Core> {
        // A poisoned lock means a panic mid-command; the state is still the
        // last committed one, because a commit lands whole or not at all.
        self.core.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs one command, then every event it emitted, before the next command.
    fn run<T, E>(&self, command: impl FnOnce(&mut Core) -> Result<T, E>) -> Result<T, E> {
        let mut core = self.core();
        let result = command(&mut core);
        core.settle_events(&self.feed, &self.command);
        result
    }

    /// Replaces an attach secret that could have been read or planted, a
    /// command the lobby initiates when it starts: journaled pending, then
    /// `write` puts a fresh secret in place, then journaled settled. When the
    /// pending entry cannot be written, nothing is replaced.
    pub fn renew_attach_secret<T>(
        &self,
        write: impl FnOnce() -> io::Result<T>,
    ) -> Result<T, Refusal> {
        self.run(|core| {
            core.begin(Initiator::Lobby, None, RENEW_ATTACH_SECRET)?;
            match write() {
                Ok(written) => {
                    core.settle(Initiator::Lobby, None, RENEW_ATTACH_SECRET, Outcome::Done);
                    Ok(written)
                }
                Err(error) => {
                    trace::line(&format!("a new attach secret could not be written: {error}"));
                    core.settle(
                        Initiator::Lobby,
                        None,
                        RENEW_ATTACH_SECRET,
                        Outcome::Internal,
                    );
                    Err(Refusal::new(
                        Outcome::Internal,
                        "The lobby could not write a new attach secret; the trace log in the data directory says why.",
                    ))
                }
            }
        })
    }

    /// Deletes every material and issued-data file that no ready credential
    /// or connection names: what a daemon that stopped between writing a
    /// file and committing its record left behind, or a file a revocation
    /// could not delete. A command the lobby initiates when it starts,
    /// journaled only when there is something to sweep. Returns how many
    /// files were swept.
    pub fn sweep_keyring(&self) -> Result<usize, Refusal> {
        let found = keyring::sealed_files(&self.dir).map_err(|error| {
            trace::line(&format!("the keyring could not be listed: {error}"));
            Refusal::new(
                Outcome::Internal,
                "The lobby could not list the files it keeps; the trace log in the data directory says why.",
            )
        })?;
        self.run(|core| {
            let state = core.store.state();
            let named: Vec<String> = state
                .credentials
                .iter()
                .filter(|credential| credential.state == CredentialState::Ready)
                .map(|credential| keyring::material_file(credential.id))
                .chain(
                    state
                        .connections
                        .iter()
                        .filter(|connection| connection.state == ConnectionState::Ready)
                        .map(|connection| keyring::issued_file(connection.id)),
                )
                .collect();
            let orphans: Vec<String> = found
                .into_iter()
                .filter(|name| !named.contains(name))
                .collect();
            if orphans.is_empty() {
                return Ok(0);
            }
            core.begin(Initiator::Lobby, None, SWEEP_KEYRING)?;
            let mut outcome = Outcome::Done;
            for name in &orphans {
                if let Err(error) = keyring::remove_kept(&self.dir, name) {
                    trace::line(&format!("{name} could not be swept: {error}"));
                    outcome = Outcome::Internal;
                }
            }
            core.settle(Initiator::Lobby, None, SWEEP_KEYRING, outcome);
            trace::line(&format!(
                "swept {} kept file(s) no record names",
                orphans.len()
            ));
            Ok(orphans.len())
        })
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
                    id: CompanionId::from_u128(new_id()),
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
    /// revision, with its credentials and connections. While minds are
    /// attached as it the removal is refused unless `cascade` confirms that
    /// their attachments end too. Returns how many attachments ended.
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
            let kept: Vec<String> = next
                .credentials
                .iter()
                .filter(|credential| credential.companion == id)
                .map(|credential| keyring::material_file(credential.id))
                .chain(
                    next.connections
                        .iter()
                        .filter(|connection| connection.companion == id)
                        .map(|connection| keyring::issued_file(connection.id)),
                )
                .collect();
            next.credentials
                .retain(|credential| credential.companion != id);
            next.connections
                .retain(|connection| connection.companion != id);
            core.commit(sponsor, Some(id), REMOVE_COMPANION, next)?;
            for file in kept {
                self.discard(&file);
            }
            core.awaited.retain(|_, awaited| awaited.companion != id);
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
            let Some(record) = find(&core.store.state().companions, name).cloned() else {
                // No attachment came to exist, so the lobby that refused it
                // is journaled, never an attachment id nothing ever held.
                return Err(core.refused(
                    Initiator::Lobby,
                    None,
                    ATTACH,
                    Refusal::new(
                        Outcome::NotFound,
                        "No companion has that id or moniker; the sponsor creates companions on the manager page.",
                    ),
                ));
            };
            let attachment = AttachmentId::from_u128(new_id());
            let mind = Initiator::Mind { attachment };
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
                    catalog: watch::channel(0).0,
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
        let live = core.proven(id, presented)?;
        let attachment = live.attachment.clone();
        let companion = core
            .store
            .state()
            .companion(attachment.companion)
            .map(|record| record.companion.clone())
            .ok_or_else(gone)?;
        Ok((attachment, companion))
    }

    /// Follows the changes of the tools of the attachment `id`, when
    /// `presented` is its token. The value advances on every change and the
    /// channel closes when the attachment ends.
    pub fn catalog_changes(
        &self,
        id: AttachmentId,
        presented: &str,
    ) -> Result<watch::Receiver<u64>, Refusal> {
        let core = self.core();
        Ok(core.proven(id, presented)?.catalog.subscribe())
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

    /// Starts signing `companion` in with the provider `kind`, as the sponsor
    /// asked with `hint`. `origin` is the lobby's own origin, where the
    /// provider sends the browser back. Returns where the browser goes.
    pub fn begin_sign_in(
        &self,
        companion: CompanionId,
        kind: &str,
        hint: &str,
        origin: &str,
    ) -> Result<SignInStart, Refusal> {
        let subject = Subject::new(Initiator::Sponsor, Some(companion), BEGIN_SIGN_IN);
        let (adapter, kind) = self.run(|core| {
            let validated = (|| {
                core.store
                    .state()
                    .companion(companion)
                    .ok_or_else(|| Refusal::new(Outcome::NotFound, "No companion has that id."))?;
                let (adapter, kind) = self.credential_adapter(kind)?;
                core.no_ready_credential(companion, &kind)?;
                Ok((adapter, kind))
            })();
            let found = validated.map_err(|refusal| core.refused_as(&subject, refusal))?;
            core.begin_as(&subject)?;
            Ok(found)
        })?;
        let state = keyring::random_token();
        let started = state
            .as_ref()
            .map_err(|_| Outcome::Internal)
            .and_then(|state| {
                let request = SignInRequest {
                    hint: hint.to_owned(),
                    state: state.clone(),
                    return_to: format!("{origin}/signin/{kind}/return"),
                };
                adapter
                    .begin_sign_in(&request)
                    .map(|start| (start, request))
            });
        self.run(|core| match started {
            Ok((start, request)) => {
                core.prune_awaited();
                core.awaited.insert(
                    request.state.clone(),
                    Awaited {
                        companion,
                        kind,
                        request,
                        started: Instant::now(),
                    },
                );
                core.settle_as(&subject, Outcome::Done);
                Ok(start)
            }
            Err(outcome) => {
                core.settle_as(&subject, outcome);
                Err(match outcome {
                    Outcome::InvalidInput => Refusal::new(
                        outcome,
                        "The provider cannot start a sign-in from what was typed; check it and try again.",
                    ),
                    other => Refusal::from_adapter(other),
                })
            }
        })
    }

    /// Completes a sign-in of the provider `kind` from what the provider sent
    /// back to the return route. A return the lobby is not waiting for is
    /// refused without a journal entry: nothing proves who sent it.
    pub fn complete_sign_in(
        &self,
        kind: &str,
        query: Vec<(String, String)>,
    ) -> Result<Credential, Refusal> {
        let returned = SignInReturn { query };
        let awaited = self.run(|core| {
            core.prune_awaited();
            let awaited = returned
                .get("state")
                .and_then(|state| core.awaited.remove(state))
                .filter(|awaited| awaited.kind.as_str() == kind)
                .ok_or_else(|| {
                    Refusal::new(
                        Outcome::NotFound,
                        "The lobby is not waiting for this sign-in; start it again on the manager page.",
                    )
                })?;
            let subject = Subject::new(Initiator::Sponsor, Some(awaited.companion), COMPLETE_SIGN_IN);
            core.begin_as(&subject)?;
            Ok(awaited)
        })?;
        let subject = Subject::new(
            Initiator::Sponsor,
            Some(awaited.companion),
            COMPLETE_SIGN_IN,
        );
        let signed = match self.registry.credential(&awaited.kind) {
            Some(adapter) => adapter.complete_sign_in(&awaited.request, &returned),
            None => Err(Outcome::Unsupported),
        };
        self.run(|core| {
            let signed = signed.map_err(|outcome| {
                core.settle_as(&subject, outcome);
                Refusal::from_adapter(outcome)
            })?;
            let state = core.store.state();
            let checked = if !Credential::permissions_are_valid(&signed.permissions) {
                Err(Refusal::new(
                    Outcome::Internal,
                    "The provider's adapter reported permissions the lobby cannot keep, so nothing was signed in.",
                ))
            } else if state.companion(awaited.companion).is_none() {
                Err(Refusal::new(
                    Outcome::NotFound,
                    "The companion was removed while it was signing in.",
                ))
            } else if state.credentials.iter().any(|credential| {
                credential.kind == awaited.kind
                    && credential.account == signed.account
                    && credential.companion != awaited.companion
            }) {
                Err(Refusal::new(
                    Outcome::Conflict,
                    "That account is signed in for another companion; an account belongs to one companion.",
                ))
            } else {
                core.no_ready_credential(awaited.companion, &awaited.kind)
            };
            checked.inspect_err(|refusal| core.settle_as(&subject, refusal.outcome))?;
            let credential = Credential {
                id: CredentialId::from_u128(new_id()),
                companion: awaited.companion,
                kind: awaited.kind.clone(),
                account: signed.account,
                permissions: signed.permissions,
                state: CredentialState::Ready,
            };
            if let Err(error) = keyring::write_material(&self.dir, credential.id, &signed.material) {
                trace::line(&format!("the material of a new credential could not be kept: {error}"));
                core.settle_as(&subject, Outcome::TemporarilyUnavailable);
                return Err(Refusal::new(
                    Outcome::TemporarilyUnavailable,
                    "The lobby could not keep the new credential, so nothing was signed in; try again.",
                ));
            }
            let mut next = core.store.state().clone();
            next.credentials.push(credential.clone());
            if let Err(refusal) = core.land_as(&subject, next) {
                self.discard(&keyring::material_file(credential.id));
                return Err(refusal);
            }
            core.events
                .push_back(Event::CredentialChanged(credential.clone()));
            Ok(credential)
        })
    }

    /// Revokes the credential `id` and, in the same commit, every connection
    /// made from it; what they keep is deleted. The account stays reserved
    /// to the credential's companion.
    pub fn revoke_credential(&self, id: CredentialId) -> Result<Credential, Refusal> {
        self.run(|core| {
            let found = core.store.state().credential(id).cloned();
            let subject = Subject::new(
                Initiator::Sponsor,
                found.as_ref().map(|credential| credential.companion),
                REVOKE_CREDENTIAL,
            );
            let mut credential = match found {
                None => {
                    return Err(core.refused_as(
                        &subject,
                        Refusal::new(Outcome::NotFound, "No credential has that id."),
                    ));
                }
                Some(credential) if credential.state == CredentialState::Revoked => {
                    return Err(core.refused_as(
                        &subject,
                        Refusal::new(Outcome::Conflict, "That credential is already revoked."),
                    ));
                }
                Some(credential) => credential,
            };
            core.begin_as(&subject)?;
            credential.state = CredentialState::Revoked;
            let mut next = core.store.state().clone();
            let mut revoked = Vec::new();
            for slot in &mut next.credentials {
                if slot.id == id {
                    slot.state = CredentialState::Revoked;
                }
            }
            for connection in &mut next.connections {
                if connection.credential == id && connection.state == ConnectionState::Ready {
                    connection.state = ConnectionState::Revoked;
                    connection.revision += 1;
                    revoked.push(connection.clone());
                }
            }
            core.land_as(&subject, next)?;
            self.discard(&keyring::material_file(id));
            for connection in &revoked {
                self.discard(&keyring::issued_file(connection.id));
            }
            core.events
                .push_back(Event::CredentialChanged(credential.clone()));
            core.events
                .extend(revoked.into_iter().map(Event::ConnectionChanged));
            Ok(credential)
        })
    }

    /// Establishes a connection the sponsor granted: the companion's
    /// credential is pinned, the service adapter establishes it with no lock
    /// held, minting what it needs through the credential's adapter, and the
    /// connection is committed only when the credential is still ready and
    /// the label still free.
    pub fn grant_connection(&self, grant: Grant<'_>) -> Result<Connection, Refusal> {
        let pinned = self.run(|core| {
            let subject = Subject::new(Initiator::Sponsor, Some(grant.companion), GRANT_CONNECTION)
                .through(None, ServiceName::new(grant.service));
            let validated = self.pin_grant(core, &grant);
            let pinned = validated.map_err(|refusal| core.refused_as(&subject, refusal))?;
            core.begin_as(&subject)?;
            Ok(pinned)
        })?;
        let Pinned {
            service,
            adapter,
            credential_adapter,
            credential,
            address,
            label,
        } = pinned;
        let subject = Subject::new(Initiator::Sponsor, Some(grant.companion), GRANT_CONNECTION)
            .through(None, Some(service.clone()));
        let broker = Broker::new(self, credential.id, credential_adapter);
        let established = adapter.establish(&address, &broker);
        self.run(|core| {
            let issued = established.map_err(|outcome| {
                core.settle_as(&subject, outcome);
                Refusal::from_adapter(outcome)
            })?;
            let state = core.store.state();
            let still_ready = state
                .credential(credential.id)
                .is_some_and(|current| current.state == CredentialState::Ready)
                && state.companion(grant.companion).is_some();
            let checked = if !still_ready {
                Err(Refusal::new(
                    Outcome::Revoked,
                    "The credential was revoked while the connection was being made.",
                ))
            } else {
                core.label_free(grant.companion, &service, &label)
            };
            checked.inspect_err(|refusal| core.settle_as(&subject, refusal.outcome))?;
            let connection = Connection {
                id: ConnectionId::from_u128(new_id()),
                companion: grant.companion,
                credential: credential.id,
                service: service.clone(),
                address,
                label,
                state: ConnectionState::Ready,
                revision: 1,
            };
            let subject = subject.through(Some(connection.id), Some(service));
            if let Err(error) = keyring::write_issued(&self.dir, connection.id, &issued) {
                trace::line(&format!("what a service issued could not be kept: {error}"));
                core.settle_as(&subject, Outcome::TemporarilyUnavailable);
                return Err(Refusal::new(
                    Outcome::TemporarilyUnavailable,
                    "The lobby could not keep what the service issued, so nothing was connected; try again.",
                ));
            }
            let mut next = core.store.state().clone();
            next.connections.push(connection.clone());
            if let Err(refusal) = core.land_as(&subject, next) {
                self.discard(&keyring::issued_file(connection.id));
                return Err(refusal);
            }
            core.events
                .push_back(Event::ConnectionChanged(connection.clone()));
            Ok(connection)
        })
    }

    /// Revokes the connection `id`; what the service issued for it is deleted.
    pub fn revoke_connection(&self, id: ConnectionId) -> Result<Connection, Refusal> {
        self.run(|core| {
            let found = core.store.state().connection(id).cloned();
            let subject = Subject::new(
                Initiator::Sponsor,
                found.as_ref().map(|connection| connection.companion),
                REVOKE_CONNECTION,
            )
            .through(
                found.as_ref().map(|connection| connection.id),
                found.as_ref().map(|connection| connection.service.clone()),
            );
            let mut connection = match found {
                None => {
                    return Err(core.refused_as(
                        &subject,
                        Refusal::new(Outcome::NotFound, "No connection has that id."),
                    ));
                }
                Some(connection) if connection.state == ConnectionState::Revoked => {
                    return Err(core.refused_as(
                        &subject,
                        Refusal::new(Outcome::Conflict, "That connection is already revoked."),
                    ));
                }
                Some(connection) => connection,
            };
            core.begin_as(&subject)?;
            connection.state = ConnectionState::Revoked;
            connection.revision += 1;
            let mut next = core.store.state().clone();
            for slot in &mut next.connections {
                if slot.id == id {
                    *slot = connection.clone();
                }
            }
            core.land_as(&subject, next)?;
            self.discard(&keyring::issued_file(id));
            core.events
                .push_back(Event::ConnectionChanged(connection.clone()));
            Ok(connection)
        })
    }

    /// The material of the credential `id`, for its adapter, when the
    /// credential is still ready.
    pub fn read_material(&self, id: CredentialId) -> Result<SealedMaterial, Outcome> {
        let ready = self
            .core()
            .store
            .state()
            .credential(id)
            .is_some_and(|credential| credential.state == CredentialState::Ready);
        if !ready {
            return Err(Outcome::Revoked);
        }
        keyring::read_material(&self.dir, id).map_err(|error| {
            trace::line(&format!(
                "the material of credential {id} could not be read: {error}"
            ));
            Outcome::Internal
        })
    }

    /// What `companion` reaches right now: each installed service it holds a
    /// ready connection to, its tools, and the labels and addresses of those
    /// connections.
    pub fn reached(&self, companion: CompanionId) -> Vec<Reached> {
        let mut by_service: Vec<(ServiceName, Vec<(Label, Address)>)> = Vec::new();
        {
            let core = self.core();
            for connection in &core.store.state().connections {
                if connection.companion != companion || connection.state != ConnectionState::Ready {
                    continue;
                }
                let place = (connection.label.clone(), connection.address.clone());
                match by_service
                    .iter_mut()
                    .find(|(service, _)| *service == connection.service)
                {
                    Some((_, places)) => places.push(place),
                    None => by_service.push((connection.service.clone(), vec![place])),
                }
            }
        }
        // Catalogs are the adapters' fixed declarations, read with nothing held.
        by_service
            .into_iter()
            .filter_map(|(service, connections)| {
                let adapter = self.registry.service(&service)?;
                Some(Reached {
                    tools: adapter.catalog(),
                    service,
                    connections,
                })
            })
            .collect()
    }

    /// Calls the tool `name` for `attachment`: journaled as invoked through
    /// the connection `at` names, performed with no lock held, and settled
    /// against that connection as it is when the service answers.
    pub fn call_tool(
        &self,
        attachment: &Attachment,
        name: &str,
        mut arguments: Map<String, Value>,
    ) -> Result<ToolResult, CallRefusal> {
        let companion = attachment.companion;
        let mind = Initiator::Mind {
            attachment: attachment.id,
        };
        let candidates = self.reached(companion);
        let (service, tool) = candidates
            .iter()
            .find_map(|reached| {
                calls::find_tool(&reached.service, &reached.tools, name)
                    .map(|tool| (reached.service.clone(), tool))
            })
            .ok_or(CallRefusal::UnknownTool)?;
        let at = arguments.remove(projections::AT);
        let ticket = self.run(|core| {
            let ready: Vec<&Connection> = core
                .store
                .state()
                .connections
                .iter()
                .filter(|connection| {
                    connection.companion == companion
                        && connection.service == service
                        && connection.state == ConnectionState::Ready
                })
                .collect();
            let labels = ready
                .iter()
                .map(|connection| connection.label.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let chosen = match &at {
                Some(Value::String(label)) => ready
                    .iter()
                    .find(|connection| connection.label.as_str() == label)
                    .ok_or_else(|| {
                        format!("This companion reaches no {service} called that; at is one of: {labels}.")
                    }),
                Some(_) => Err(format!(
                    "at names one {service} connection as text, one of: {labels}."
                )),
                None if ready.len() == 1 => Ok(&ready[0]),
                None if ready.is_empty() => Err(format!(
                    "This companion does not reach {service} now; call reach to see what it reaches."
                )),
                None => Err(format!(
                    "This companion reaches more than one {service}; name one with at: {labels}."
                )),
            };
            let connection = (*chosen.map_err(|message| {
                CallRefusal::Ended(Refusal::new(Outcome::InvalidInput, message))
            })?)
            .clone();
            let adapter = self
                .registry
                .service(&service)
                .ok_or(CallRefusal::UnknownTool)?;
            let ticket = Ticket {
                connection: connection.id,
                revision: connection.revision,
                address: connection.address,
                service: service.clone(),
                tool: tool.name.clone(),
                effect: tool.effect,
                adapter,
            };
            core.record(call_note(mind, companion, &ticket, JournalKind::Invoked, Outcome::Pending))
                .map_err(|_| {
                    CallRefusal::Ended(Refusal::new(
                        Outcome::TemporarilyUnavailable,
                        "The journal could not be written, so the call was not made; the trace log in the data directory says why.",
                    ))
                })?;
            Ok(ticket)
        })?;
        let result = keyring::read_issued(&self.dir, ticket.connection)
            .map_err(|_| Outcome::Revoked)
            .and_then(|issued| {
                ticket.adapter.call(
                    &ticket.tool,
                    &ToolArguments(arguments),
                    &ticket.address,
                    &issued,
                )
            });
        let answer = self.run(|core| {
            let still_pinned = core
                .store
                .state()
                .connection(ticket.connection)
                .is_some_and(|connection| {
                    connection.state == ConnectionState::Ready
                        && connection.revision == ticket.revision
                });
            let answer = calls::settle(ticket.effect, still_pinned, result);
            let outcome = answer
                .as_ref()
                .map_or_else(|outcome| *outcome, |_| Outcome::Done);
            let _ = core.record(call_note(
                mind,
                companion,
                &ticket,
                JournalKind::Settled,
                outcome,
            ));
            Ok::<_, CallRefusal>(answer)
        })?;
        answer.map_err(|outcome| CallRefusal::Ended(Refusal::new(outcome, outcome.sentence())))
    }

    /// Every companion with its live attachments, oldest first.
    pub fn companions(&self) -> Vec<CompanionView> {
        let core = self.core();
        core.views()
    }

    /// The lobby as one consistent picture.
    pub fn snapshot(&self) -> Snapshot {
        let core = self.core();
        let state = core.store.state();
        Snapshot {
            companions: core.views(),
            credentials: state.credentials.clone(),
            connections: state.connections.clone(),
            store_id: state.store_id.clone(),
            epoch: self.feed.epoch().to_owned(),
            // Items are published only while the core is held, so this is
            // exactly the last item the picture includes.
            position: self.feed.position(),
            journal_healthy: core.journal_healthy,
            journal_high_water: core.journal.high_water(),
        }
    }

    /// A page of the journal; see [`crate::journal::Reader::page`]. The
    /// lobby is held only while the reader is taken, never while the files
    /// are read.
    pub fn journal_page(&self, before: Option<&str>, limit: usize) -> Result<Page, PageProblem> {
        let reader = self.core().journal.reader();
        reader.page(before, limit)
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

    /// The credential adapter of the provider kind `kind`.
    fn credential_adapter(
        &self,
        kind: &str,
    ) -> Result<(Arc<dyn CredentialAdapter>, ProviderKind), Refusal> {
        ProviderKind::new(kind)
            .and_then(|kind| {
                self.registry
                    .credential(&kind)
                    .map(|adapter| (adapter, kind))
            })
            .ok_or_else(|| {
                Refusal::new(
                    Outcome::NotFound,
                    "No credential provider of that kind is installed.",
                )
            })
    }

    /// Validates a grant and pins what establishing it needs.
    fn pin_grant(&self, core: &Core, grant: &Grant<'_>) -> Result<Pinned, Refusal> {
        let state = core.store.state();
        state
            .companion(grant.companion)
            .ok_or_else(|| Refusal::new(Outcome::NotFound, "No companion has that id."))?;
        let (service, adapter) = ServiceName::new(grant.service)
            .and_then(|service| {
                self.registry
                    .service(&service)
                    .map(|adapter| (service, adapter))
            })
            .ok_or_else(|| {
                Refusal::new(Outcome::NotFound, "No service of that name is installed.")
            })?;
        let credential = state
            .credential(grant.credential)
            .filter(|credential| credential.companion == grant.companion)
            .cloned()
            .ok_or_else(|| {
                Refusal::new(
                    Outcome::NotFound,
                    "That companion holds no credential with that id.",
                )
            })?;
        if credential.state != CredentialState::Ready {
            return Err(Refusal::new(
                Outcome::Revoked,
                "That credential was revoked; sign the companion in again first.",
            ));
        }
        let description = adapter.describe();
        if !description.accepts.contains(&credential.kind) {
            return Err(Refusal::new(
                Outcome::InvalidInput,
                format!(
                    "{service} does not accept a {} credential.",
                    credential.kind
                ),
            ));
        }
        let (credential_adapter, _) = self.credential_adapter(credential.kind.as_str())?;
        let address = Address::parse(grant.address, description.addresses).ok_or_else(|| {
            Refusal::new(
                Outcome::InvalidInput,
                format!(
                    "That is not an address {service} can reach: one origin, such as https://example.test, and nothing after it."
                ),
            )
        })?;
        let label = Label::new(grant.label).map_err(|problem| name_refusal("label", problem))?;
        core.label_free(grant.companion, &service, &label)?;
        Ok(Pinned {
            service,
            adapter,
            credential_adapter,
            credential,
            address,
            label,
        })
    }

    /// Deletes a file the keyring kept, after the commit that let go of it.
    /// A file that cannot be deleted is reported in the trace log; nothing
    /// reads it again.
    fn discard(&self, file: &str) {
        if let Err(error) = keyring::remove_kept(&self.dir, file) {
            trace::line(&format!("{file} could not be deleted: {error}"));
        }
    }
}

/// What establishing a granted connection is pinned to.
struct Pinned {
    service: ServiceName,
    adapter: Arc<dyn ServiceAdapter>,
    credential_adapter: Arc<dyn CredentialAdapter>,
    credential: Credential,
    address: Address,
    label: Label,
}

fn call_note(
    mind: Initiator,
    companion: CompanionId,
    ticket: &Ticket,
    kind: JournalKind,
    outcome: Outcome,
) -> Note {
    Note {
        initiator: mind,
        companion: Some(companion),
        connection: Some(ticket.connection),
        service: Some(ticket.service.clone()),
        tool: Some(ticket.tool.clone()),
        kind,
        outcome,
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

    /// The live attachment `id`, when `presented` is its token.
    fn proven(&self, id: AttachmentId, presented: &str) -> Result<&Live, Refusal> {
        let live = self.attachments.get(&id).ok_or_else(gone)?;
        if !live.token.matches(presented) {
            return Err(Refusal::new(
                Outcome::Refused,
                "That is not this attachment's token.",
            ));
        }
        Ok(live)
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

    /// Refuses a second ready credential of `kind` for `companion`.
    fn no_ready_credential(
        &self,
        companion: CompanionId,
        kind: &ProviderKind,
    ) -> Result<(), Refusal> {
        let held = self.store.state().credentials.iter().any(|credential| {
            credential.companion == companion
                && credential.kind == *kind
                && credential.state == CredentialState::Ready
        });
        if held {
            return Err(Refusal::new(
                Outcome::Conflict,
                format!(
                    "This companion is already signed in with {kind}; revoke that credential first."
                ),
            ));
        }
        Ok(())
    }

    /// Refuses a label that already names a ready connection of `service`
    /// for `companion`.
    fn label_free(
        &self,
        companion: CompanionId,
        service: &ServiceName,
        label: &Label,
    ) -> Result<(), Refusal> {
        let taken = self.store.state().connections.iter().any(|connection| {
            connection.companion == companion
                && connection.service == *service
                && connection.label == *label
                && connection.state == ConnectionState::Ready
        });
        if taken {
            return Err(Refusal::new(
                Outcome::Conflict,
                format!("This companion already reaches a {service} called {label}."),
            ));
        }
        Ok(())
    }

    /// Lets go of sign-ins that waited too long, and of the oldest when too
    /// many wait.
    fn prune_awaited(&mut self) {
        self.awaited
            .retain(|_, awaited| awaited.started.elapsed() < SIGN_IN_TIME_LIMIT);
        while self.awaited.len() >= SIGN_INS_WAITING {
            let oldest = self
                .awaited
                .iter()
                .min_by_key(|(_, awaited)| awaited.started)
                .map(|(state, _)| state.clone());
            match oldest {
                Some(state) => {
                    self.awaited.remove(&state);
                }
                None => break,
            }
        }
    }

    /// Writes one journal entry, emitting it.
    fn record(&mut self, note: Note) -> io::Result<JournalEntry> {
        let what = match note.kind {
            JournalKind::Command { name } => name,
            JournalKind::Invoked => "an invoked call",
            JournalKind::Settled => "a settled call",
        };
        let outcome = note.outcome;
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
                    "the journal could not record {what} ({}): {error}",
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
    fn refused_as(&mut self, subject: &Subject, refusal: Refusal) -> Refusal {
        let _ = self.record(subject.note(refusal.outcome));
        refusal
    }

    fn refused(
        &mut self,
        initiator: Initiator,
        companion: Option<CompanionId>,
        name: &'static str,
        refusal: Refusal,
    ) -> Refusal {
        self.refused_as(&Subject::new(initiator, companion, name), refusal)
    }

    /// The pending entry that precedes every effect: no entry, no effect.
    fn begin_as(&mut self, subject: &Subject) -> Result<(), Refusal> {
        self.record(subject.note(Outcome::Pending))
            .map(drop)
            .map_err(|_| {
                Refusal::new(
                    Outcome::TemporarilyUnavailable,
                    "The journal could not be written, so nothing was changed; the trace log in the data directory says why.",
                )
            })
    }

    fn begin(
        &mut self,
        initiator: Initiator,
        companion: Option<CompanionId>,
        name: &'static str,
    ) -> Result<(), Refusal> {
        self.begin_as(&Subject::new(initiator, companion, name))
    }

    /// The settled entry that follows an effect. When it cannot be written
    /// the effect still stands and is reported as it happened; the journal's
    /// health says what is missing.
    fn settle_as(&mut self, subject: &Subject, outcome: Outcome) {
        let _ = self.record(subject.note(outcome));
    }

    fn settle(
        &mut self,
        initiator: Initiator,
        companion: Option<CompanionId>,
        name: &'static str,
        outcome: Outcome,
    ) {
        self.settle_as(&Subject::new(initiator, companion, name), outcome);
    }

    /// Commits `next` after a pending entry was written, then writes the
    /// settled entry.
    fn land_as(&mut self, subject: &Subject, next: State) -> Result<(), Refusal> {
        if let Err(error) = self.store.commit(next) {
            trace::line(&format!("{} could not be committed: {error}", subject.name));
            let refusal = Refusal::new(
                Outcome::TemporarilyUnavailable,
                "The lobby could not write its state file, so nothing was changed; try again.",
            );
            self.settle_as(subject, refusal.outcome);
            return Err(refusal);
        }
        self.settle_as(subject, Outcome::Done);
        Ok(())
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
        let subject = Subject::new(initiator, companion, name);
        self.begin_as(&subject)?;
        self.land_as(&subject, next)
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
            match event {
                // The catalog handler: every attachment of the companion
                // learns that its tools changed.
                Event::ConnectionChanged(connection) => {
                    for live in self.attachments.values() {
                        if live.attachment.companion == connection.companion {
                            live.catalog.send_modify(|changes| *changes += 1);
                        }
                    }
                }
                // The cascade handler: a removed companion's attachments end.
                Event::CompanionRemoved(companion) => self.end_attachments_of(companion),
                _ => {}
            }
        }
    }

    /// Ends every attachment of `companion`, a command the lobby initiates,
    /// journaled pending then settled like any other. The companion is gone,
    /// so the attachments end even when the journal cannot record it; the
    /// journal's health then says what is missing.
    fn end_attachments_of(&mut self, companion: CompanionId) {
        let ended: Vec<AttachmentId> = self
            .attachments
            .values()
            .filter(|live| live.attachment.companion == companion)
            .map(|live| live.attachment.id)
            .collect();
        for id in ended {
            let _ = self.begin(Initiator::Lobby, Some(companion), DETACH);
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
        "label" => {
            " A label is 1 to 24 lowercase letters, digits and single hyphens, starting with a letter."
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
            Lobby::open(
                &self.0,
                r"C:\Tools\companion-lobby.exe",
                Registry::default(),
            )
            .expect("open")
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
    fn renewing_the_attach_secret_is_journaled_as_the_lobby_pending_then_settled() {
        let dir = Dir::new("renew");
        let lobby = dir.lobby();
        let written = lobby
            .renew_attach_secret(|| Ok::<_, io::Error>(7))
            .expect("renewed");
        assert_eq!(written, 7);
        let failed = lobby
            .renew_attach_secret(|| Err::<(), _>(io::Error::other("disk full")))
            .expect_err("the write failed");
        assert_eq!(failed.outcome, Outcome::Internal);
        let entries: Vec<(String, String, String)> = dir
            .journal()
            .iter()
            .map(|entry| {
                assert!(entry.get("companion").is_none(), "{entry}");
                assert!(entry.get("attachment").is_none(), "{entry}");
                (
                    entry["initiator"].as_str().expect("initiator").to_owned(),
                    entry["command"].as_str().expect("command").to_owned(),
                    entry["outcome"].as_str().expect("outcome").to_owned(),
                )
            })
            .collect();
        let renew = |outcome: &str| {
            (
                "lobby".to_owned(),
                RENEW_ATTACH_SECRET.to_owned(),
                outcome.to_owned(),
            )
        };
        assert_eq!(
            entries,
            [
                renew("pending"),
                renew("done"),
                renew("pending"),
                renew("internal")
            ]
        );
    }

    /// Signs `companion` in with a passbook `authority` as `holder`, the
    /// way the sponsor's browser does, in process.
    fn passbook_sign_in(
        lobby: &Lobby,
        companion: CompanionId,
        authority: &lobby_testkit::passbook::PassbookAuthority,
        holder: &str,
    ) -> Credential {
        use lobby_contracts::AddressPolicy;
        use lobby_outbound::{Outbound, Request};
        authority.sign_in_as(holder);
        let start = lobby
            .begin_sign_in(
                companion,
                "passbook",
                &authority.origin(),
                "http://127.0.0.1:1",
            )
            .expect("a sign-in starts");
        let (origin, target) = start.url.split_at(authority.origin().len());
        let origin = Address::parse(origin, AddressPolicy::LoopbackAllowed).expect("origin");
        let approved = Outbound::new(AddressPolicy::LoopbackAllowed)
            .send(&origin, Request::new("GET", target))
            .expect("the authority answers");
        let back = approved.header("location").expect("sent back").to_owned();
        let query = back.split_once('?').expect("a query").1;
        let pairs = query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .map(|(name, value)| {
                (
                    lobby_testkit::percent_decode(name),
                    lobby_testkit::percent_decode(value),
                )
            })
            .collect();
        lobby
            .complete_sign_in("passbook", pairs)
            .expect("signed in")
    }

    #[test]
    fn files_no_record_names_are_swept_at_start_and_the_sweep_is_journaled() {
        let dir = Dir::new("sweep");
        let registry = Registry::new(
            vec![Arc::new(lobby_testkit::passbook::Passbook::new())],
            Vec::new(),
        );
        let authority = lobby_testkit::passbook::PassbookAuthority::start();
        let lobby = Lobby::open(&dir.0, "companion-lobby", registry.clone()).expect("open");
        let ada = lobby
            .create_companion("ada", "Ada")
            .expect("ada")
            .record
            .companion
            .id;
        let credential = passbook_sign_in(&lobby, ada, &authority, "holder-ada");
        assert_eq!(lobby.sweep_keyring(), Ok(0), "nothing to sweep");
        let entries = dir.journal().len();
        assert!(
            !dir.journal()
                .iter()
                .any(|entry| entry["command"] == SWEEP_KEYRING),
            "a sweep with nothing to do is not journaled"
        );
        drop(lobby);

        // What a daemon that stopped between writing a file and committing
        // its record leaves behind: files no record names.
        let lost_credential = CredentialId::from_u128(new_id());
        let lost_connection = ConnectionId::from_u128(new_id());
        keyring::write_material(
            &dir.0,
            lost_credential,
            &SealedMaterial::new(b"lost".to_vec()),
        )
        .expect("write");
        keyring::write_issued(
            &dir.0,
            lost_connection,
            &lobby_contracts::SealedIssued::new(b"lost".to_vec()),
        )
        .expect("write");
        let lobby = Lobby::open(&dir.0, "companion-lobby", registry).expect("open again");
        assert_eq!(lobby.sweep_keyring(), Ok(2));
        assert_eq!(
            keyring::sealed_files(&dir.0).expect("list"),
            [keyring::material_file(credential.id)],
            "only the material a ready credential names is kept"
        );
        assert!(lobby.read_material(credential.id).is_ok());
        let swept: Vec<(String, String)> = dir.journal()[entries..]
            .iter()
            .map(|entry| {
                (
                    entry["initiator"].as_str().expect("initiator").to_owned(),
                    format!(
                        "{}:{}",
                        entry["command"].as_str().expect("command"),
                        entry["outcome"].as_str().expect("outcome")
                    ),
                )
            })
            .collect();
        assert_eq!(
            swept,
            [
                ("lobby".to_owned(), "sweep_keyring:pending".to_owned()),
                ("lobby".to_owned(), "sweep_keyring:done".to_owned())
            ]
        );
        assert_eq!(lobby.sweep_keyring(), Ok(0));
    }

    #[test]
    fn a_refused_attach_is_journaled_as_the_lobby_with_no_attachment() {
        let dir = Dir::new("refused-attach");
        let lobby = dir.lobby();
        let refused = lobby
            .attach("nobody", token())
            .expect_err("no such companion");
        assert_eq!(refused.outcome, Outcome::NotFound);
        let journal = dir.journal();
        let last = journal.last().expect("the refusal is journaled");
        assert_eq!(last["command"], ATTACH);
        assert_eq!(last["initiator"], "lobby");
        assert_eq!(last["outcome"], "not_found");
        assert!(last.get("attachment").is_none(), "{last}");
        assert_eq!(lobby.attachment_count(), 0);
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
        let detaches: Vec<(String, String)> = dir
            .journal()
            .into_iter()
            .filter(|entry| entry["command"] == DETACH)
            .map(|entry| {
                (
                    entry["initiator"].as_str().expect("initiator").to_owned(),
                    entry["outcome"].as_str().expect("outcome").to_owned(),
                )
            })
            .collect();
        let pair = [
            ("lobby".to_owned(), "pending".to_owned()),
            ("lobby".to_owned(), "done".to_owned()),
        ];
        assert_eq!(detaches, [pair.clone(), pair].concat());
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
