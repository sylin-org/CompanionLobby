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
//! after the credential is revoked; signing the companion in again creates a
//! new credential and the revoked one stays as history. A companion waits on
//! at most one sign-in of a kind at a time: a second is refused before its
//! adapter is asked to start anything. The sponsor follows a sign-in by an
//! id of its own, never by the `state` the provider carries back, and may
//! cancel it while it waits; one that waits past its time expires, and a
//! return for a cancelled or expired sign-in signs nothing in; whatever its
//! adapter kept for it is let go at once. A connection exists only because
//! its service adapter established it with a credential of its companion.
//!
//! A credential is kept alive by its adapter, which the lobby asks to renew
//! it whenever the adapter said it would want to be asked, on a blocking
//! thread with nothing held. A renewal is journaled from the moment its
//! adapter marks the material rotating, just before the provider's
//! single-use secret is spent, and the mark is on the disk before that.
//! When the provider refuses the renewal, or the renewal ends with the mark
//! still in place so the lobby cannot know whether the provider rotated the
//! secret, the credential needs signing in again: in one commit it and every
//! ready connection made from it wait, their tools leave the catalog, and
//! every attachment of the companion hears its tools changed once. A daemon
//! that finds a mark when it starts does the same before anything else, and
//! the old material is never renewed again. Signing the same account in
//! again restores the credential, under its own id, and every connection
//! that waited, with what their services issued, without a new grant;
//! signing in another account while one waits is refused.
//!
//! Revoking a credential revokes every connection made from it in the same
//! commit, under one journal entry, and deletes what they keep; then, with
//! nothing held, its adapter revokes it at the provider, journaled as a
//! command of its own, and the sponsor reads whether the provider confirmed.
//!
//! Each journal entry carries the names the sponsor read for what it
//! concerns when it was written: the companion, the account and its kind,
//! the connection and its service.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use lobby_contracts::{
    Address, Attachment, AttachmentId, Authority, Companion, CompanionId, Connection, ConnectionId,
    ConnectionState, Credential, CredentialAdapter, CredentialId, CredentialState, DisplayName,
    Explained, Freshness, Initiator, IssuedSlot, JournalEntry, JournalKind, JournalNames, Label,
    MaterialSlot, MaterialUpdate, Moniker, NameProblem, Outcome, ProviderKind, Revocation,
    SealedIssued, SealedMaterial, ServiceAdapter, ServiceName, SignInRequest, SignInReturn,
    SignInStart, SignedIn, Sponsor, ToolArguments, ToolName, ToolResult,
};
use serde_json::{Map, Value};
use tokio::sync::watch;

use crate::broker::{Broker, Held, Slot};
use crate::calls::{self, Ticket};
use crate::journal::{Journal, Note, Page, PageProblem};
use crate::keyring::{self, AttachmentToken};
use crate::portrait::{self, Portrait};
use crate::projections::{self, Feed, Reached, Subscription};
use crate::registry::Registry;
use crate::store::{CompanionRecord, State, Store};
use crate::trace;

/// How long the lobby waits for a sign-in it started to come back.
pub const SIGN_IN_TIME_LIMIT: Duration = Duration::from_secs(10 * 60);
/// How many sign-ins the lobby waits on at once; the oldest gives way.
pub const SIGN_INS_WAITING: usize = 32;
/// How many ended sign-ins the lobby remembers, to say why a late return
/// signs nothing in.
pub const SIGN_INS_REMEMBERED: usize = 64;
/// The shortest pause between two passes that keep credentials alive.
pub const RENEWAL_PAUSE_MIN: Duration = Duration::from_secs(1);
/// The longest pause between two such passes, so every ready credential's
/// adapter is asked at least this often.
pub const RENEWAL_PAUSE_MAX: Duration = Duration::from_secs(300);
/// How long after a renewal that could not finish, and changed nothing,
/// the credential is asked again.
pub const RENEWAL_RETRY: Duration = Duration::from_secs(30);

/// What a service issued for the connection one call goes through, with
/// the authority to establish it again.
struct Through<'a> {
    lobby: &'a Lobby,
    ticket: &'a Ticket,
    authority: &'a Broker<'a>,
}

impl IssuedSlot for Through<'_> {
    fn read(&self) -> Result<SealedIssued, Outcome> {
        keyring::read_issued(&self.lobby.dir, self.ticket.connection).map_err(|_| Outcome::Revoked)
    }

    fn authority(&self) -> &dyn Authority {
        self.authority
    }

    fn reissue(&self, issued: SealedIssued) -> Result<(), Outcome> {
        self.lobby.reissue(self.ticket, &issued)
    }
}

/// Where a sign-in the lobby started stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignInStatus {
    /// Waiting for the provider to send the browser back.
    Pending,
    /// The provider sent it back and the credential was created.
    SignedIn,
    /// The sponsor cancelled it.
    Cancelled,
    /// It waited past its time.
    Expired,
    /// The return did not prove the sign-in, or the lobby refused it.
    Refused,
}

impl SignInStatus {
    /// The stable wire name.
    pub fn code(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::SignedIn => "signed_in",
            Self::Cancelled => "cancelled",
            Self::Expired => "expired",
            Self::Refused => "refused",
        }
    }
}

/// A sign-in as the sponsor follows it. Its `state` never shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignInView {
    /// The id the sponsor follows it by: 32 hex digits.
    pub id: String,
    /// The companion being signed in.
    pub companion: CompanionId,
    /// The provider kind.
    pub kind: ProviderKind,
    /// The account the sponsor meant, as typed, or empty when the sponsor
    /// left the choice to the provider's own page.
    pub account: String,
    /// Where it stands.
    pub status: SignInStatus,
    /// When it expires, in milliseconds since the Unix epoch.
    pub expires_at: u64,
}

/// Which conflict stopped a sign-in the provider completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignInConflict {
    /// The account is signed in for another companion.
    AnotherCompanion,
    /// The companion already holds a ready account of the kind.
    AlreadyHeld,
    /// The companion waits for another account of the kind to sign in again.
    AnotherWaits,
}

impl SignInConflict {
    /// The stable wire name.
    pub fn code(self) -> &'static str {
        match self {
            Self::AnotherCompanion => "another_companion",
            Self::AlreadyHeld => "already_held",
            Self::AnotherWaits => "another_waits",
        }
    }

    /// The conflict whose wire name is `code`.
    pub fn from_code(code: &str) -> Option<Self> {
        [
            Self::AnotherCompanion,
            Self::AlreadyHeld,
            Self::AnotherWaits,
        ]
        .into_iter()
        .find(|conflict| conflict.code() == code)
    }
}

/// Why a command was refused, with the lobby's own words for the sponsor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// The outcome.
    pub outcome: Outcome,
    /// What went wrong, written by the lobby; never text from outside it.
    pub message: String,
    /// For a sign-in refused as a conflict, which one.
    pub conflict: Option<SignInConflict>,
}

impl Refusal {
    fn new(outcome: Outcome, message: impl Into<String>) -> Self {
        Self {
            outcome,
            message: message.into(),
            conflict: None,
        }
    }

    /// The same refusal, saying which sign-in conflict it is.
    fn because(mut self, conflict: SignInConflict) -> Self {
        self.conflict = Some(conflict);
        self
    }

    /// The refusal of an adapter that answered `explained`: the adapter's
    /// own sentence when it wrote one, the lobby's words otherwise.
    fn from_explained(explained: Explained) -> Self {
        if explained.sentence == explained.outcome.sentence() {
            Self::from_adapter(explained.outcome)
        } else {
            Self::new(explained.outcome, explained.sentence)
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
    /// A sign-in started, ended or changed where it stands.
    SignInChanged(SignInView),
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
    /// Every sign-in the lobby waits on, oldest first, then the sign-ins it
    /// remembers ending, oldest first.
    pub sign_ins: Vec<SignInView>,
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
    id: String,
    companion: CompanionId,
    kind: ProviderKind,
    /// The account the sponsor meant: what was typed, or the label of the
    /// account signed in again.
    meant: String,
    request: SignInRequest,
    started: Instant,
    expires_at: u64,
}

impl Awaited {
    fn view(&self, status: SignInStatus) -> SignInView {
        SignInView {
            id: self.id.clone(),
            companion: self.companion,
            kind: self.kind.clone(),
            account: self.meant.clone(),
            status,
            expires_at: self.expires_at,
        }
    }
}

struct Core {
    store: Store,
    journal: Journal,
    /// The adapters, for the titles journal entries carry.
    registry: Registry,
    dir: PathBuf,
    attachments: HashMap<AttachmentId, Live>,
    awaited: HashMap<String, Awaited>,
    /// Sign-ins whose adapter is starting them, with nothing parked yet.
    starting: Vec<(CompanionId, ProviderKind)>,
    /// Sign-ins that ended, by their `state`, newest last.
    ended: VecDeque<(String, SignInView)>,
    /// Sign-ins that ended since the last command, whose adapters let go
    /// of them once the lobby is released.
    released: Vec<(ProviderKind, SignInRequest)>,
    /// When each ready credential's adapter wants to be asked to renew it.
    due: HashMap<CredentialId, Instant>,
    /// Advances whenever a credential changes, so the keep-alive pass looks
    /// again at once.
    renewals: watch::Sender<u64>,
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
    sign_in_limit: Duration,
}

/// How a credential the sponsor revoked was revoked at its provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtProvider {
    /// The provider confirmed it.
    Confirmed,
    /// The provider offers no revocation; what it granted ends when it
    /// expires there.
    NotOffered,
    /// The provider could not be reached, or did not confirm it.
    NotConfirmed,
    /// The credential waited for a sign-in, so the provider was sent the
    /// last secret the lobby held, which a renewal may already have spent;
    /// its answer cannot confirm that what it granted ended.
    SentLastKnown,
    /// The lobby held no material to name it to the provider with, so the
    /// provider was not asked.
    NotAsked,
}

impl AtProvider {
    /// The stable wire name.
    pub fn code(self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::NotOffered => "not_offered",
            Self::NotConfirmed => "not_confirmed",
            Self::SentLastKnown => "sent_last_known",
            Self::NotAsked => "not_asked",
        }
    }
}

/// A revoked credential, and what its provider said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revoked {
    /// The credential as the revocation left it.
    pub credential: Credential,
    /// What its provider said.
    pub provider: AtProvider,
}

/// How a sign-in the provider sent back ended, and the companion it was for
/// when the lobby knew the sign-in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completed {
    /// The companion the sign-in was for, when the lobby waited for it.
    pub companion: Option<CompanionId>,
    /// The credential it signed in or restored, or why it did not.
    pub result: Result<Credential, Refusal>,
}

/// Whether `entry` is routine: a renewal that kept an account signed in, or
/// the pending entry written before one.
pub fn is_routine(entry: &Value) -> bool {
    entry["command"] == RENEW_CREDENTIAL
        && matches!(entry["outcome"].as_str(), Some("done" | "pending"))
}

/// The fixed names commands carry in the journal.
const CREATE_COMPANION: &str = "create_companion";
const RENAME_COMPANION: &str = "rename_companion";
const SET_PORTRAIT: &str = "set_portrait";
const REMOVE_COMPANION: &str = "remove_companion";
const ATTACH: &str = "attach";
const DETACH: &str = "detach";
const RENEW_ATTACH_SECRET: &str = "renew_attach_secret";
const BEGIN_SIGN_IN: &str = "begin_sign_in";
const COMPLETE_SIGN_IN: &str = "complete_sign_in";
const CANCEL_SIGN_IN: &str = "cancel_sign_in";
const REVOKE_CREDENTIAL: &str = "revoke_credential";
const REVOKE_AT_PROVIDER: &str = "revoke_at_provider";
const RENEW_CREDENTIAL: &str = "renew_credential";
const END_UNFINISHED_RENEWAL: &str = "end_unfinished_renewal";
const GRANT_CONNECTION: &str = "grant_connection";
const REVOKE_CONNECTION: &str = "revoke_connection";
const SWEEP_KEYRING: &str = "sweep_keyring";

/// Who a journal entry is about, short of its kind and outcome, with the
/// names the sponsor reads for it.
#[derive(Debug, Clone)]
struct Subject {
    initiator: Initiator,
    companion: Option<CompanionId>,
    connection: Option<ConnectionId>,
    service: Option<ServiceName>,
    name: &'static str,
    names: JournalNames,
}

impl Subject {
    fn new(initiator: Initiator, companion: Option<CompanionId>, name: &'static str) -> Self {
        Self {
            initiator,
            companion,
            connection: None,
            service: None,
            name,
            names: JournalNames::default(),
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
            names: self.names.clone(),
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
                registry: registry.clone(),
                dir: dir.to_path_buf(),
                attachments: HashMap::new(),
                awaited: HashMap::new(),
                starting: Vec::new(),
                ended: VecDeque::new(),
                released: Vec::new(),
                due: HashMap::new(),
                renewals: watch::channel(0).0,
                journal_healthy: true,
                events: VecDeque::new(),
            }),
            feed: Feed::new(),
            command: command.to_owned(),
            dir: dir.to_path_buf(),
            registry,
            sign_in_limit: SIGN_IN_TIME_LIMIT,
        })
    }

    /// The same lobby, waiting `limit` for a sign-in to come back.
    #[must_use]
    pub fn with_sign_in_limit(mut self, limit: Duration) -> Self {
        self.sign_in_limit = limit;
        self
    }

    fn core(&self) -> MutexGuard<'_, Core> {
        // A poisoned lock means a panic mid-command; the state is still the
        // last committed one, because a commit lands whole or not at all.
        self.core.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs one command, then every event it emitted, before the next
    /// command; then, with the lobby released, lets the adapters of
    /// the sign-ins that ended go of them.
    fn run<T, E>(&self, command: impl FnOnce(&mut Core) -> Result<T, E>) -> Result<T, E> {
        let (result, released) = {
            let mut core = self.core();
            let result = command(&mut core);
            core.settle_events(&self.feed, &self.command);
            (result, std::mem::take(&mut core.released))
        };
        self.release(released);
        result
    }

    /// Tells each sign-in's adapter to let go of it.
    fn release(&self, released: Vec<(ProviderKind, SignInRequest)>) {
        for (kind, request) in released {
            if let Some(adapter) = self.registry.credential(&kind) {
                adapter.cancel_sign_in(&request);
            }
        }
    }

    /// Follows the changes of credentials, so the pass that keeps them
    /// alive looks again at once when one is signed in or restored.
    pub fn renewal_changes(&self) -> watch::Receiver<u64> {
        self.core().renewals.subscribe()
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

    /// Ends every renewal a daemon that stopped left unfinished: each ready
    /// credential with a rotating mark needs signing in again, with its
    /// connections, in one commit the lobby initiates and journals, and
    /// only then is its mark cleared. Run when the daemon starts, before
    /// anything renews or reads material; when it cannot finish, the daemon
    /// does not start. Returns how many credentials it ended.
    pub fn end_unfinished_renewals(&self) -> Result<usize, Refusal> {
        let marked = keyring::rotating_marks(&self.dir).map_err(|error| {
            trace::line(&format!("the rotating marks could not be listed: {error}"));
            Refusal::new(
                Outcome::Internal,
                "The lobby could not list the renewals it marks; the trace log in the data directory says why.",
            )
        })?;
        self.run(|core| {
            let mut ended = 0;
            for id in marked {
                let ready = core
                    .store
                    .state()
                    .credential(id)
                    .is_some_and(|credential| credential.state == CredentialState::Ready);
                if ready {
                    let subject = core.credential_subject(Initiator::Lobby, id, END_UNFINISHED_RENEWAL);
                    core.begin_as(&subject)?;
                    core.needs_sign_in(id, &subject, Outcome::Done)?;
                    ended += 1;
                    trace::line(&format!(
                        "credential {id} had a renewal that never finished; it needs signing in again"
                    ));
                }
            }
            Ok(ended)
        })
    }

    /// Deletes every material and issued-data file that no live credential
    /// or connection names, and every rotating mark no ready credential
    /// holds: what a daemon that stopped between writing a file and
    /// committing its record left behind, or a file a revocation could not
    /// delete. A command the lobby initiates when it starts, journaled only
    /// when there is something to sweep. Returns how many files were swept.
    pub fn sweep_keyring(&self) -> Result<usize, Refusal> {
        let listed = keyring::sealed_files(&self.dir).and_then(|mut found| {
            found.extend(
                keyring::rotating_marks(&self.dir)?
                    .into_iter()
                    .map(keyring::rotating_file),
            );
            Ok(found)
        });
        let found = listed.map_err(|error| {
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
                .filter(|credential| credential.state != CredentialState::Revoked)
                .map(|credential| keyring::material_file(credential.id))
                .chain(
                    state
                        .connections
                        .iter()
                        .filter(|connection| connection.state != ConnectionState::Revoked)
                        .map(|connection| keyring::issued_file(connection.id)),
                )
                .chain(
                    state
                        .credentials
                        .iter()
                        .filter(|credential| credential.state == CredentialState::Ready)
                        .map(|credential| keyring::rotating_file(credential.id)),
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
                portrait: None,
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

    /// Gives the companion `id`, which must still be at `expected`
    /// revision, the portrait `image`, or takes its portrait away when
    /// `image` is `None`, so it shows its initials. Only a PNG, JPEG, WebP
    /// or GIF image of at most [`portrait::MAX_BYTES`] is kept.
    pub fn set_portrait(
        &self,
        id: CompanionId,
        expected: u64,
        image: Option<&[u8]>,
    ) -> Result<CompanionView, Refusal> {
        self.run(|core| {
            let sponsor = Initiator::Sponsor;
            let kind = match image {
                None => Ok(None),
                Some(bytes) if bytes.len() > portrait::MAX_BYTES => Err(Refusal::new(
                    Outcome::InvalidInput,
                    "That image is too large; a portrait is at most 512 KiB.",
                )),
                Some(bytes) => Portrait::sniff(bytes).map(Some).ok_or_else(|| {
                    Refusal::new(
                        Outcome::InvalidInput,
                        "That file is not a PNG, JPEG, WebP or GIF image.",
                    )
                }),
            };
            let (mut record, kind) = core
                .current(id, expected)
                .and_then(|record| kind.map(|kind| (record, kind)))
                .map_err(|refusal| core.refused(sponsor, Some(id), SET_PORTRAIT, refusal))?;
            let had = record.portrait;
            record.portrait = kind;
            record.revision += 1;
            let subject = core.named(Subject::new(sponsor, Some(id), SET_PORTRAIT));
            core.begin_as(&subject)?;
            // A replacement keeps the portrait it replaces until its commit
            // lands, and puts it back when the commit fails.
            let replaced = match (had, image) {
                (Some(_), Some(_)) => portrait::read(&self.dir, id).ok(),
                _ => None,
            };
            if let Some(bytes) = image
                && let Err(error) = portrait::write(&self.dir, id, bytes)
            {
                trace::line(&format!("a portrait could not be kept: {error}"));
                core.settle_as(&subject, Outcome::TemporarilyUnavailable);
                return Err(Refusal::new(
                    Outcome::TemporarilyUnavailable,
                    "The lobby could not keep that portrait; try again.",
                ));
            }
            let mut next = core.store.state().clone();
            for slot in &mut next.companions {
                if slot.companion.id == id {
                    *slot = record.clone();
                }
            }
            if let Err(refusal) = core.land_as(&subject, next) {
                if image.is_some() {
                    match &replaced {
                        Some(bytes) => {
                            if let Err(error) = portrait::write(&self.dir, id, bytes) {
                                trace::line(&format!(
                                    "the replaced portrait of {id} could not be put back: {error}"
                                ));
                            }
                        }
                        None if had.is_none() => self.discard_portrait(id),
                        None => {}
                    }
                }
                return Err(refusal);
            }
            if image.is_none() {
                self.discard_portrait(id);
            }
            let view = core.view(&record);
            core.events.push_back(Event::CompanionChanged(view.clone()));
            Ok(view)
        })
    }

    /// The portrait of the companion `id`, when it has one.
    pub fn portrait(&self, id: CompanionId) -> Option<Vec<u8>> {
        let held = self
            .core()
            .store
            .state()
            .companion(id)
            .is_some_and(|record| record.portrait.is_some());
        if !held {
            return None;
        }
        portrait::read(&self.dir, id)
            .inspect_err(|error| {
                trace::line(&format!("the portrait of {id} could not be read: {error}"));
            })
            .ok()
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
                .flat_map(|credential| {
                    [
                        keyring::material_file(credential.id),
                        keyring::rotating_file(credential.id),
                    ]
                })
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
            self.discard_portrait(id);
            let waiting: Vec<String> = core
                .awaited
                .iter()
                .filter(|(_, awaited)| awaited.companion == id)
                .map(|(state, _)| state.clone())
                .collect();
            for state in waiting {
                core.end_sign_in(&state, SignInStatus::Cancelled);
            }
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
    /// provider sends the browser back. Refused while the companion already
    /// waits on a sign-in of that kind, before the adapter starts anything.
    /// When the companion holds a credential of that kind that needs signing
    /// in again, this is that sign-in: the adapter names the same account
    /// to the provider from its material, whatever `hint` says. Returns the
    /// sign-in and where the browser goes.
    pub fn begin_sign_in(
        &self,
        companion: CompanionId,
        kind: &str,
        hint: &str,
        origin: &str,
    ) -> Result<(SignInView, SignInStart), Refusal> {
        let limit = self.sign_in_limit;
        let (adapter, kind, again, subject) = self.run(|core| {
            core.expire_overdue(limit);
            let mut subject = core.named(Subject::new(
                Initiator::Sponsor,
                Some(companion),
                BEGIN_SIGN_IN,
            ));
            let validated = (|| {
                core.store
                    .state()
                    .companion(companion)
                    .ok_or_else(|| Refusal::new(Outcome::NotFound, "No companion has that id."))?;
                let (adapter, kind) = self.credential_adapter(kind)?;
                core.no_ready_credential(companion, &kind)?;
                core.no_sign_in_waiting(companion, &kind)?;
                Ok((adapter, kind))
            })();
            if let Ok((_, kind)) = &validated {
                subject = core.with_kind(subject, kind);
            }
            let (adapter, kind) =
                validated.map_err(|refusal| core.refused_as(&subject, refusal))?;
            let again = core.waiting_credential(companion, &kind);
            if let Some(credential) = &again {
                subject = core.with_account(subject, credential);
            }
            core.begin_as(&subject)?;
            core.starting.push((companion, kind.clone()));
            Ok((adapter, kind, again, subject))
        })?;
        let state = keyring::random_token();
        let meant = again.as_ref().map_or_else(
            || hint.trim().to_owned(),
            |credential| credential.label.to_string(),
        );
        let material = match &again {
            Some(credential) => keyring::read_material(&self.dir, credential.id)
                .map(Some)
                .map_err(|error| {
                    trace::line(&format!(
                        "the material of credential {} could not be read to sign it in again: {error}",
                        credential.id
                    ));
                    Outcome::Internal
                }),
            None => Ok(None),
        };
        let started = state
            .as_ref()
            .map_err(|_| Outcome::Internal)
            .and_then(|state| material.map(|material| (state, material)))
            .and_then(|(state, material)| {
                let mut request = SignInRequest {
                    hint: hint.to_owned(),
                    state: state.clone(),
                    return_to: format!("{origin}/signin/{kind}/return"),
                    again: material,
                    permissions: self.registry.permissions(&kind),
                };
                let start = adapter.begin_sign_in(&request)?;
                // The adapter keeps what it needs; the lobby keeps no
                // material beside the sign-in while it waits.
                request.again = None;
                Ok((start, request))
            });
        self.run(|core| {
            core.starting
                .retain(|(waiting, of)| !(*waiting == companion && *of == kind));
            match started {
                Ok((start, request)) => {
                    core.expire_overdue(limit);
                    core.make_room();
                    let awaited = Awaited {
                        id: uuid::Uuid::now_v7().simple().to_string(),
                        companion,
                        kind,
                        meant,
                        request,
                        started: Instant::now(),
                        expires_at: epoch_millis().saturating_add(limit.as_millis() as u64),
                    };
                    let view = awaited.view(SignInStatus::Pending);
                    core.awaited.insert(awaited.request.state.clone(), awaited);
                    core.settle_as(&subject, Outcome::Done);
                    core.events.push_back(Event::SignInChanged(view.clone()));
                    Ok((view, start))
                }
                Err(outcome) => {
                    core.settle_as(&subject, outcome);
                    Err(begin_refusal(outcome, hint))
                }
            }
        })
    }

    /// Cancels the sign-in `id` while it waits; a return for it afterwards
    /// signs nothing in.
    pub fn cancel_sign_in(&self, id: &str) -> Result<SignInView, Refusal> {
        let limit = self.sign_in_limit;
        self.run(|core| {
            core.expire_overdue(limit);
            let found = core
                .awaited
                .iter()
                .find(|(_, awaited)| awaited.id == id)
                .map(|(state, awaited)| (state.clone(), awaited.companion, awaited.kind.clone()));
            let Some((state, companion, kind)) = found else {
                return Err(core.refused_as(
                    &Subject::new(Initiator::Sponsor, None, CANCEL_SIGN_IN),
                    Refusal::new(
                        Outcome::NotFound,
                        "No sign-in with that id is waiting; it may have ended already.",
                    ),
                ));
            };
            let subject = core.named(Subject::new(
                Initiator::Sponsor,
                Some(companion),
                CANCEL_SIGN_IN,
            ));
            let subject = core.with_kind(subject, &kind);
            core.begin_as(&subject)?;
            let view = core.end_sign_in(&state, SignInStatus::Cancelled);
            core.settle_as(&subject, Outcome::Done);
            view.ok_or_else(gone)
        })
    }

    /// Completes a sign-in of the provider `kind` from what the provider sent
    /// back to the return route. The sign-in ends whatever its adapter says:
    /// it is used once. A return the lobby is not waiting for is refused
    /// without a journal entry: nothing proves who sent it.
    pub fn complete_sign_in(&self, kind: &str, query: Vec<(String, String)>) -> Completed {
        let returned = SignInReturn { query };
        let limit = self.sign_in_limit;
        let waited = self.run(|core| {
            core.expire_overdue(limit);
            let state = returned.get("state").unwrap_or_default().to_owned();
            if !core
                .awaited
                .get(&state)
                .is_some_and(|awaited| awaited.kind.as_str() == kind)
            {
                let companion = core.ended_for(&state);
                return Err((companion, core.why_not_waiting(&state)));
            }
            let awaited = core.awaited.remove(&state).ok_or_else(|| (None, gone()))?;
            let subject = core.named(Subject::new(
                Initiator::Sponsor,
                Some(awaited.companion),
                COMPLETE_SIGN_IN,
            ));
            let subject = core.with_kind(subject, &awaited.kind);
            if let Err(refusal) = core.begin_as(&subject) {
                let companion = awaited.companion;
                core.awaited.insert(state, awaited);
                return Err((Some(companion), refusal));
            }
            core.starting
                .push((awaited.companion, awaited.kind.clone()));
            Ok((state, awaited, subject))
        });
        let (state, awaited, subject) = match waited {
            Ok(waited) => waited,
            Err((companion, refusal)) => {
                return Completed {
                    companion,
                    result: Err(refusal),
                };
            }
        };
        let signed = match self.registry.credential(&awaited.kind) {
            Some(adapter) => adapter.complete_sign_in(&awaited.request, &returned),
            None => Err(Outcome::Unsupported),
        };
        let result = self.run(|core| {
            core.starting
                .retain(|(waiting, of)| !(*waiting == awaited.companion && *of == awaited.kind));
            let landed = self.land_sign_in(core, subject, &awaited, signed);
            let status = if landed.is_ok() {
                SignInStatus::SignedIn
            } else {
                SignInStatus::Refused
            };
            core.remember_ended(&state, awaited.view(status));
            landed
        });
        Completed {
            companion: Some(awaited.companion),
            result,
        }
    }

    /// Checks what the adapter signed in and commits the credential: a new
    /// one, or the one that waited for this account to sign in again,
    /// restored with every connection that waited for it.
    fn land_sign_in(
        &self,
        core: &mut Core,
        subject: Subject,
        awaited: &Awaited,
        signed: Result<SignedIn, Outcome>,
    ) -> Result<Credential, Refusal> {
        let signed = signed.map_err(|outcome| {
            core.settle_as(&subject, outcome);
            complete_refusal(outcome)
        })?;
        let state = core.store.state();
        let waiting = core.waiting_credential(awaited.companion, &awaited.kind);
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
            )
            .because(SignInConflict::AnotherCompanion))
        } else if let Some(waiting) = waiting
            .as_ref()
            .filter(|waiting| waiting.account != signed.account)
        {
            Err(Refusal::new(
                Outcome::Conflict,
                format!(
                    "That is another account than {}, which waits to be signed in again; sign in as it, or revoke it first.",
                    waiting.label
                ),
            )
            .because(SignInConflict::AnotherWaits))
        } else {
            core.no_ready_credential(awaited.companion, &awaited.kind)
                .map_err(|refusal| refusal.because(SignInConflict::AlreadyHeld))
        };
        checked.inspect_err(|refusal| core.settle_as(&subject, refusal.outcome))?;
        let credential = Credential {
            id: waiting
                .as_ref()
                .map_or_else(|| CredentialId::from_u128(new_id()), |waiting| waiting.id),
            companion: awaited.companion,
            kind: awaited.kind.clone(),
            account: signed.account,
            label: signed.label,
            permissions: signed.permissions,
            state: CredentialState::Ready,
        };
        let subject = core.with_account(subject, &credential);
        // A restored credential's material goes back as it was when the
        // commit that restores it fails.
        let replaced = waiting
            .as_ref()
            .and_then(|_| keyring::read_material(&self.dir, credential.id).ok());
        if let Err(error) = keyring::write_material(&self.dir, credential.id, &signed.material) {
            trace::line(&format!(
                "the material of a signed-in credential could not be kept: {error}"
            ));
            core.settle_as(&subject, Outcome::TemporarilyUnavailable);
            return Err(Refusal::new(
                Outcome::TemporarilyUnavailable,
                "The lobby could not keep the new credential, so nothing was signed in; try again.",
            ));
        }
        let mut next = core.store.state().clone();
        let mut restored = Vec::new();
        if waiting.is_some() {
            for slot in &mut next.credentials {
                if slot.id == credential.id {
                    *slot = credential.clone();
                }
            }
            for connection in &mut next.connections {
                if connection.credential == credential.id
                    && connection.state == ConnectionState::NeedsSignIn
                {
                    connection.state = ConnectionState::Ready;
                    connection.revision += 1;
                    restored.push(connection.clone());
                }
            }
        } else {
            next.credentials.push(credential.clone());
        }
        if let Err(refusal) = core.land_as(&subject, next) {
            match replaced {
                Some(material) => {
                    if let Err(error) = keyring::write_material(&self.dir, credential.id, &material)
                    {
                        trace::line(&format!(
                            "the material of credential {} could not be put back: {error}",
                            credential.id
                        ));
                    }
                }
                None => self.discard(&keyring::material_file(credential.id)),
            }
            return Err(refusal);
        }
        if waiting.is_some()
            && let Err(error) = keyring::unmark_rotating(&self.dir, credential.id)
        {
            trace::line(&format!(
                "the rotating mark of credential {} could not be cleared: {error}",
                credential.id
            ));
        }
        core.events
            .push_back(Event::CredentialChanged(credential.clone()));
        core.events
            .extend(restored.into_iter().map(Event::ConnectionChanged));
        Ok(credential)
    }

    /// Revokes the credential `id` and, in the same commit, every connection
    /// made from it; what they keep is deleted, and the account stays
    /// reserved to the credential's companion. Then, with nothing held, its
    /// adapter revokes it at the provider, a step journaled on its own, and
    /// the answer says what the provider said.
    pub fn revoke_credential(&self, id: CredentialId) -> Result<Revoked, Refusal> {
        let (credential, material, subject, waited) = self.run(|core| {
            let found = core.store.state().credential(id).cloned();
            let mut subject = core.named(Subject::new(
                Initiator::Sponsor,
                found.as_ref().map(|credential| credential.companion),
                REVOKE_CREDENTIAL,
            ));
            if let Some(credential) = &found {
                subject = core.with_account(subject, credential);
            }
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
            // The material names the account to the provider once the
            // lobby's own revocation landed; it is held only in memory. A
            // credential that waited for a sign-in holds a secret a renewal
            // may already have spent, so the provider's answer for it can
            // confirm nothing.
            let material = keyring::read_material(&self.dir, id).ok();
            let waited = credential.state == CredentialState::NeedsSignIn;
            credential.state = CredentialState::Revoked;
            let mut next = core.store.state().clone();
            let mut revoked = Vec::new();
            for slot in &mut next.credentials {
                if slot.id == id {
                    slot.state = CredentialState::Revoked;
                }
            }
            for connection in &mut next.connections {
                if connection.credential == id && connection.state != ConnectionState::Revoked {
                    connection.state = ConnectionState::Revoked;
                    connection.revision += 1;
                    revoked.push(connection.clone());
                }
            }
            core.land_as(&subject, next)?;
            core.due.remove(&id);
            self.discard(&keyring::material_file(id));
            self.discard(&keyring::rotating_file(id));
            for connection in &revoked {
                self.discard(&keyring::issued_file(connection.id));
            }
            core.events
                .push_back(Event::CredentialChanged(credential.clone()));
            core.events
                .extend(revoked.into_iter().map(Event::ConnectionChanged));
            let at_provider = Subject {
                name: REVOKE_AT_PROVIDER,
                ..subject
            };
            let material = match material {
                Some(material) if core.begin_as(&at_provider).is_ok() => Some(material),
                _ => None,
            };
            Ok((credential, material, at_provider, waited))
        })?;
        let Some(material) = material else {
            return Ok(Revoked {
                credential,
                provider: AtProvider::NotAsked,
            });
        };
        let answered = match self.registry.credential(&credential.kind) {
            Some(adapter) => adapter.revoke(&Held::new(id, material)),
            None => Err(Outcome::Unsupported),
        };
        let (provider, outcome) = match answered {
            Ok(Revocation::Confirmed) if waited => (AtProvider::SentLastKnown, Outcome::Unknown),
            Ok(Revocation::Confirmed) => (AtProvider::Confirmed, Outcome::Done),
            Ok(Revocation::NotOffered) => (AtProvider::NotOffered, Outcome::Unsupported),
            Err(outcome) => (AtProvider::NotConfirmed, outcome),
        };
        self.run(|core| {
            core.settle_as(&subject, outcome);
            Ok::<_, Refusal>(())
        })?;
        Ok(Revoked {
            credential,
            provider,
        })
    }

    /// Establishes a connection the sponsor granted: the companion's
    /// credential is pinned, the service adapter establishes it with no lock
    /// held, minting what it needs through the credential's adapter, and the
    /// connection is committed only when the credential is still ready and
    /// the label still free.
    pub fn grant_connection(&self, grant: Grant<'_>) -> Result<Connection, Refusal> {
        let (pinned, subject) = self.run(|core| {
            let mut subject = core.named(
                Subject::new(Initiator::Sponsor, Some(grant.companion), GRANT_CONNECTION)
                    .through(None, ServiceName::new(grant.service)),
            );
            subject.names.connection = Label::new(grant.label).ok();
            if let Some(credential) = core
                .store
                .state()
                .credential(grant.credential)
                .filter(|credential| credential.companion == grant.companion)
                .cloned()
            {
                subject = core.with_account(subject, &credential);
            }
            let validated = self.pin_grant(core, &grant);
            let pinned = validated.map_err(|refusal| core.refused_as(&subject, refusal))?;
            core.begin_as(&subject)?;
            Ok((pinned, subject))
        })?;
        let Pinned {
            service,
            adapter,
            credential_adapter,
            credential,
            address,
            label,
        } = pinned;
        let broker = Broker::new(self, credential.id, credential_adapter);
        let established = adapter.establish(&address, &broker);
        // An authorization that renewed the credential, or found it needs
        // signing in again, settles that before the grant does.
        self.settle_renewal(broker.slot(), broker.refused().map_or(Ok(None), Err));
        self.run(|core| {
            let issued = established.map_err(|explained| {
                core.settle_as(&subject, explained.outcome);
                Refusal::from_explained(explained)
            })?;
            let state = core.store.state();
            let now = state
                .credential(credential.id)
                .map(|current| current.state)
                .filter(|_| state.companion(grant.companion).is_some());
            let checked = if now == Some(CredentialState::NeedsSignIn) {
                Err(Refusal::new(
                    Outcome::NeedsSignIn,
                    "The account needs signing in again, so nothing was connected; sign the companion in again first.",
                ))
            } else if now != Some(CredentialState::Ready) {
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
            let subject = core.named(
                Subject::new(
                    Initiator::Sponsor,
                    found.as_ref().map(|connection| connection.companion),
                    REVOKE_CONNECTION,
                )
                .through(
                    found.as_ref().map(|connection| connection.id),
                    found.as_ref().map(|connection| connection.service.clone()),
                ),
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

    /// Marks the credential `id` rotating for its adapter, when `read` is
    /// still its material and it is ready: the renewal's pending entry is
    /// journaled and the mark is on the disk before this answers, and
    /// neither happens otherwise.
    pub fn mark_rotating(&self, id: CredentialId, read: &SealedMaterial) -> Result<(), Outcome> {
        self.run(|core| {
            let ready = core
                .store
                .state()
                .credential(id)
                .is_some_and(|credential| credential.state == CredentialState::Ready);
            if !ready {
                return Err(Outcome::Revoked);
            }
            let current = keyring::read_material(&self.dir, id).map_err(|error| {
                trace::line(&format!(
                    "the material of credential {id} could not be read to mark it: {error}"
                ));
                Outcome::Internal
            })?;
            if current != *read || self.dir.join(keyring::rotating_file(id)).exists() {
                return Err(Outcome::Conflict);
            }
            let subject = core.credential_subject(Initiator::Lobby, id, RENEW_CREDENTIAL);
            core.begin_as(&subject)
                .map_err(|_| Outcome::TemporarilyUnavailable)?;
            if let Err(error) = keyring::mark_rotating(&self.dir, id) {
                trace::line(&format!(
                    "credential {id} could not be marked rotating, so it was not renewed: {error}"
                ));
                core.settle_as(&subject, Outcome::TemporarilyUnavailable);
                return Err(Outcome::TemporarilyUnavailable);
            }
            Ok(())
        })
    }

    /// Clears the credential `id`'s rotating mark for its adapter: the
    /// provider spent nothing.
    pub fn unmark_rotating(&self, id: CredentialId) {
        if let Err(error) = keyring::unmark_rotating(&self.dir, id) {
            trace::line(&format!(
                "the rotating mark of credential {id} could not be cleared: {error}"
            ));
        }
    }

    /// Keeps a renewal's new material for the credential `id`, when it is
    /// still ready, its material is still what the renewal replaces, and the
    /// renewal grants no more than the credential holds; then clears the
    /// mark. When the new material cannot be kept the mark stays, and the
    /// renewal ends with the credential needing signing in again.
    pub fn commit_material(&self, id: CredentialId, update: MaterialUpdate) -> Result<(), Outcome> {
        self.run(|core| {
            let credential = core
                .store
                .state()
                .credential(id)
                .filter(|credential| credential.state == CredentialState::Ready)
                .cloned()
                .ok_or(Outcome::Revoked)?;
            let current = keyring::read_material(&self.dir, id).map_err(|error| {
                trace::line(&format!(
                    "the material of credential {id} could not be read to renew it: {error}"
                ));
                Outcome::Internal
            })?;
            if current != *update.expected() {
                return Err(Outcome::Conflict);
            }
            if !update
                .permissions()
                .iter()
                .all(|permission| credential.permissions.contains(permission))
            {
                return Err(Outcome::Refused);
            }
            if let Err(error) = keyring::write_material(&self.dir, id, update.replacement()) {
                trace::line(&format!(
                    "the renewed material of credential {id} could not be kept: {error}"
                ));
                return Err(Outcome::TemporarilyUnavailable);
            }
            if update.permissions() != credential.permissions.as_slice() {
                let mut narrowed = credential.clone();
                narrowed.permissions = update.permissions().to_vec();
                let mut next = core.store.state().clone();
                for slot in &mut next.credentials {
                    if slot.id == id {
                        *slot = narrowed.clone();
                    }
                }
                match core.store.commit(next) {
                    Ok(()) => core.events.push_back(Event::CredentialChanged(narrowed)),
                    Err(error) => trace::line(&format!(
                        "the narrowed permissions of credential {id} could not be committed: {error}"
                    )),
                }
            }
            if let Err(error) = keyring::unmark_rotating(&self.dir, id) {
                trace::line(&format!(
                    "the rotating mark of credential {id} could not be cleared: {error}"
                ));
            }
            Ok(())
        })
    }

    /// Settles what an adapter did through `slot`, once it answered:
    /// `answered` is its renewal's answer, with `Ok(None)` for an answer
    /// that says nothing about when to renew next. A renewal that ended with
    /// its mark still in place, or that the provider refused, leaves the
    /// credential needing signing in again with its connections; one that
    /// could not finish, and changed nothing, is tried again later.
    pub fn settle_renewal(&self, slot: &Slot<'_>, answered: Result<Option<Freshness>, Outcome>) {
        let id = slot.credential();
        let handed = slot.handed();
        let marked = slot.marked();
        let result: Result<(), Refusal> = self.run(|core| {
            let subject = core.credential_subject(Initiator::Lobby, id, RENEW_CREDENTIAL);
            let ready = core
                .store
                .state()
                .credential(id)
                .is_some_and(|credential| credential.state == CredentialState::Ready);
            if !ready {
                core.due.remove(&id);
                if marked {
                    core.settle_as(&subject, Outcome::Revoked);
                }
                return Ok(());
            }
            let still_marked = self.dir.join(keyring::rotating_file(id)).exists();
            let unchanged = || {
                handed.as_ref().is_some_and(|handed| {
                    keyring::read_material(&self.dir, id).is_ok_and(|current| current == *handed)
                })
            };
            match answered {
                // The secret may have been spent and nothing new was kept:
                // the old material is never renewed again. A mark this slot
                // did not write belongs to a renewal still under way, which
                // settles itself.
                _ if marked && still_marked => {
                    let outcome = match answered {
                        Err(Outcome::NeedsSignIn) => Outcome::NeedsSignIn,
                        _ => Outcome::Unknown,
                    };
                    trace::line(&format!(
                        "credential {id} ended a renewal with its mark in place; it needs signing in again"
                    ));
                    core.needs_sign_in(id, &subject, outcome)
                }
                Ok(freshness) => {
                    if marked {
                        core.settle_as(&subject, Outcome::Done);
                    }
                    if let Some(freshness) = freshness {
                        core.due.insert(
                            id,
                            Instant::now() + Duration::from_secs(freshness.renew_in_secs),
                        );
                    } else if marked {
                        core.due.remove(&id);
                    }
                    Ok(())
                }
                Err(outcome @ (Outcome::NeedsSignIn | Outcome::Refused)) if unchanged() => {
                    if !marked {
                        core.begin_as(&subject)?;
                    }
                    trace::line(&format!(
                        "credential {id} could not be renewed ({}); it needs signing in again",
                        outcome.code()
                    ));
                    core.needs_sign_in(id, &subject, outcome)
                }
                Err(outcome) => {
                    if marked {
                        core.settle_as(&subject, outcome);
                    }
                    trace::line(&format!(
                        "credential {id} could not be renewed just now ({}); it is tried again later",
                        outcome.code()
                    ));
                    core.due.insert(id, Instant::now() + RENEWAL_RETRY);
                    Ok(())
                }
            }
        });
        if let Err(refusal) = result {
            trace::line(&format!(
                "the end of a renewal of credential {id} could not be recorded: {}",
                refusal.message
            ));
        }
    }

    /// Keeps every ready credential alive: asks each one's adapter, when it
    /// is due, to renew it, one at a time and with nothing held, and settles
    /// what each renewal did. Answers how long to wait before the next pass.
    pub fn keep_alive(&self) -> Duration {
        let now = Instant::now();
        let due: Vec<(CredentialId, Arc<dyn CredentialAdapter>)> = {
            let core = self.core();
            core.store
                .state()
                .credentials
                .iter()
                .filter(|credential| credential.state == CredentialState::Ready)
                .filter(|credential| core.due.get(&credential.id).is_none_or(|at| *at <= now))
                .filter_map(|credential| {
                    self.registry
                        .credential(&credential.kind)
                        .map(|adapter| (credential.id, adapter))
                })
                .collect()
        };
        for (id, adapter) in due {
            let slot = Slot::new(self, id);
            let answered = adapter.renew(&slot).map(Some);
            self.settle_renewal(&slot, answered);
        }
        let core = self.core();
        let now = Instant::now();
        core.store
            .state()
            .credentials
            .iter()
            .filter(|credential| credential.state == CredentialState::Ready)
            .filter_map(|credential| core.due.get(&credential.id))
            .map(|at| at.saturating_duration_since(now))
            .min()
            .unwrap_or(RENEWAL_PAUSE_MAX)
            .clamp(RENEWAL_PAUSE_MIN, RENEWAL_PAUSE_MAX)
    }

    /// The connections of `companion` that wait for a sign-in, by service.
    pub fn waiting(&self, companion: CompanionId) -> Vec<(ServiceName, Label)> {
        self.core()
            .store
            .state()
            .connections
            .iter()
            .filter(|connection| {
                connection.companion == companion
                    && connection.state == ConnectionState::NeedsSignIn
            })
            .map(|connection| (connection.service.clone(), connection.label.clone()))
            .collect()
    }

    /// What `companion` reaches right now: each installed service it holds a
    /// ready connection to, its tools, and the labels and addresses of those
    /// connections.
    pub fn reached(&self, companion: CompanionId) -> Vec<Reached> {
        type Reaching = (Label, Address, ConnectionId);
        let mut by_service: Vec<(ServiceName, Vec<Reaching>)> = Vec::new();
        {
            let core = self.core();
            for connection in &core.store.state().connections {
                if connection.companion != companion || connection.state != ConnectionState::Ready {
                    continue;
                }
                let place = (
                    connection.label.clone(),
                    connection.address.clone(),
                    connection.id,
                );
                match by_service
                    .iter_mut()
                    .find(|(service, _)| *service == connection.service)
                {
                    Some((_, places)) => places.push(place),
                    None => by_service.push((connection.service.clone(), vec![place])),
                }
            }
        }
        // Catalogs are the adapters' fixed declarations, and what each
        // connection's issued data allows is read from the keyring, both
        // with nothing held. A service's tools are those any of its ready
        // connections allows, in catalog order.
        by_service
            .into_iter()
            .filter_map(|(service, connections)| {
                let adapter = self.registry.service(&service)?;
                let allowed: Vec<lobby_contracts::ToolName> = connections
                    .iter()
                    .filter_map(|(_, _, id)| keyring::read_issued(&self.dir, *id).ok())
                    .flat_map(|issued| adapter.available(&issued))
                    .collect();
                let tools = adapter
                    .catalog()
                    .into_iter()
                    .filter(|tool| allowed.contains(&tool.name))
                    .collect();
                Some(Reached {
                    title: self.registry.service_title(&service),
                    tools,
                    service,
                    connections: connections
                        .into_iter()
                        .map(|(label, address, _)| (label, address))
                        .collect(),
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
        let found = candidates.iter().find_map(|reached| {
            calls::find_tool(&reached.service, &reached.tools, name)
                .map(|tool| (reached.service.clone(), tool))
        });
        let named_at = arguments
            .get(projections::AT)
            .and_then(Value::as_str)
            .map(str::to_owned);
        let Some((service, tool)) = found.clone() else {
            // A tool whose connections all wait for a sign-in is named, so
            // the mind hears why it is gone and what brings it back: the
            // connection `at` names when it names one of them.
            let waiting = self.waiting(companion);
            let offers = |service: &ServiceName| {
                self.registry.service(service).is_some_and(|adapter| {
                    calls::find_tool(service, &adapter.catalog(), name).is_some()
                })
            };
            let named = waiting
                .iter()
                .find(|(service, label)| {
                    named_at.as_deref() == Some(label.as_str()) && offers(service)
                })
                .or_else(|| waiting.iter().find(|(service, _)| offers(service)));
            return Err(match named {
                Some((service, label)) => CallRefusal::Ended(Refusal::new(
                    Outcome::NeedsSignIn,
                    format!(
                        "{name} is gone for now: the account behind {title} at {label} needs the sponsor to sign it in again on the manager page, and the tool comes back once they do.",
                        title = self.registry.service_title(service)
                    ),
                )),
                None => CallRefusal::UnknownTool,
            });
        };
        let at = arguments.remove(projections::AT);
        let title = self.registry.service_title(&service);
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
            let waits = |label: &str| {
                core.store.state().connections.iter().any(|connection| {
                    connection.companion == companion
                        && connection.service == service
                        && connection.state == ConnectionState::NeedsSignIn
                        && connection.label.as_str() == label
                })
            };
            let chosen = match &at {
                Some(Value::String(label)) if waits(label) => {
                    return Err(CallRefusal::Ended(Refusal::new(
                        Outcome::NeedsSignIn,
                        format!(
                            "The {title} called {label} waits for the sponsor to sign its account in again on the manager page; it comes back then. at is one of: {labels}."
                        ),
                    )));
                }
                Some(Value::String(label)) => ready
                    .iter()
                    .find(|connection| connection.label.as_str() == label)
                    .ok_or_else(|| {
                        format!("This companion reaches no {title} called that; at is one of: {labels}.")
                    }),
                Some(_) => Err(format!(
                    "at names one {title} connection as text, one of: {labels}."
                )),
                None if ready.len() == 1 => Ok(&ready[0]),
                None if ready.is_empty() => Err(format!(
                    "This companion does not reach {title} now; call reach to see what it reaches."
                )),
                None => Err(format!(
                    "This companion reaches more than one {title}; name one with at: {labels}."
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
            let credential_adapter = core
                .store
                .state()
                .credential(connection.credential)
                .and_then(|credential| self.registry.credential(&credential.kind))
                .ok_or(CallRefusal::UnknownTool)?;
            let ticket = Ticket {
                connection: connection.id,
                revision: connection.revision,
                credential: connection.credential,
                address: connection.address,
                service: service.clone(),
                tool: tool.name.clone(),
                effect: tool.effect,
                adapter,
                credential_adapter,
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
        // The service adapter may establish the connection again, once,
        // through a broker for the connection's credential; a minting that
        // renewed the credential, or found it needs signing in again,
        // settles that before the call does.
        let broker = Broker::new(self, ticket.credential, ticket.credential_adapter.clone());
        let slot = Through {
            lobby: self,
            ticket: &ticket,
            authority: &broker,
        };
        let result = ticket.adapter.call(
            &ticket.tool,
            &ToolArguments(arguments),
            &ticket.address,
            &slot,
        );
        self.settle_renewal(broker.slot(), broker.refused().map_or(Ok(None), Err));
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
                .map_or_else(|explained| explained.outcome, |_| Outcome::Done);
            let _ = core.record(call_note(
                mind,
                companion,
                &ticket,
                JournalKind::Settled,
                outcome,
            ));
            Ok::<_, CallRefusal>(answer)
        })?;
        answer.map_err(|explained| {
            CallRefusal::Ended(Refusal::new(explained.outcome, explained.sentence))
        })
    }

    /// Keeps `issued`, which the service issued when a call established the
    /// connection `ticket` pins again, while that connection is still ready
    /// at the pinned revision.
    fn reissue(&self, ticket: &Ticket, issued: &SealedIssued) -> Result<(), Outcome> {
        self.run(|core| {
            let pinned = core
                .store
                .state()
                .connection(ticket.connection)
                .is_some_and(|connection| {
                    connection.state == ConnectionState::Ready
                        && connection.revision == ticket.revision
                });
            if !pinned {
                return Err(Outcome::Revoked);
            }
            keyring::write_issued(&self.dir, ticket.connection, issued).map_err(|error| {
                trace::line(&format!(
                    "what a service issued anew for connection {} could not be kept: {error}",
                    ticket.connection
                ));
                Outcome::Internal
            })
        })
    }

    /// The provider kinds the sponsor may sign companions in with.
    pub fn credential_kinds(&self) -> Vec<ProviderKind> {
        self.registry.credential_kinds()
    }

    /// The adapters the lobby runs with.
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Every companion with its live attachments, oldest first.
    pub fn companions(&self) -> Vec<CompanionView> {
        let core = self.core();
        core.views()
    }

    /// The lobby as one consistent picture. Sign-ins that waited past their
    /// time end as expired first, so the picture never shows one still
    /// waiting that cannot come back.
    pub fn snapshot(&self) -> Snapshot {
        let limit = self.sign_in_limit;
        let mut core = self.core();
        core.expire_overdue(limit);
        core.settle_events(&self.feed, &self.command);
        let released = std::mem::take(&mut core.released);
        let state = core.store.state();
        let snapshot = Snapshot {
            companions: core.views(),
            credentials: state.credentials.clone(),
            connections: state.connections.clone(),
            sign_ins: {
                let mut waiting: Vec<&Awaited> = core.awaited.values().collect();
                waiting.sort_by_key(|awaited| awaited.started);
                waiting
                    .into_iter()
                    .map(|awaited| awaited.view(SignInStatus::Pending))
                    .chain(core.ended.iter().map(|(_, view)| view.clone()))
                    .collect()
            },
            store_id: state.store_id.clone(),
            epoch: self.feed.epoch().to_owned(),
            // Items are published only while the core is held, so this is
            // exactly the last item the picture includes.
            position: self.feed.position(),
            journal_healthy: core.journal_healthy,
            journal_high_water: core.journal.high_water(),
        };
        drop(core);
        self.release(released);
        snapshot
    }

    /// A page of the journal; see [`crate::journal::Reader::page`]. With
    /// `companion`, only the entries about that companion; with `finished`,
    /// only the entries that say how something ended, leaving out the
    /// pending entry written before it; without `routine`, none of the
    /// renewals that kept an account signed in as they should, which the
    /// journal keeps all the same. Each entry carries its sentence,
    /// told with the names the lobby holds when the page is read. The lobby
    /// is held only while the reader and the names are taken, never while
    /// the files are read.
    pub fn journal_page(
        &self,
        before: Option<&str>,
        limit: usize,
        companion: Option<CompanionId>,
        finished: bool,
        routine: bool,
    ) -> Result<Page, PageProblem> {
        let (reader, names) = {
            let core = self.core();
            (
                core.journal.reader(),
                projections::Names::of(core.store.state()),
            )
        };
        let companion = companion.map(|id| id.to_string());
        let keep = |entry: &Value| {
            companion
                .as_ref()
                .is_none_or(|id| entry["companion"].as_str() == Some(id.as_str()))
                && !(finished && entry["outcome"] == "pending")
                && (routine || !is_routine(entry))
        };
        let mut page = reader.page_where(before, limit, &keep)?;
        for entry in &mut page.entries {
            projections::told(entry, &names);
        }
        Ok(page)
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
        match credential.state {
            CredentialState::Ready => {}
            CredentialState::NeedsSignIn => {
                return Err(Refusal::new(
                    Outcome::NeedsSignIn,
                    format!(
                        "{} needs signing in again first; its connections come back with it.",
                        credential.label
                    ),
                ));
            }
            CredentialState::Revoked => {
                return Err(Refusal::new(
                    Outcome::Revoked,
                    "That credential was revoked; sign the companion in again first.",
                ));
            }
        }
        let description = adapter.describe();
        if !description.accepts.contains(&credential.kind) {
            return Err(Refusal::new(
                Outcome::InvalidInput,
                format!("{service} does not accept that kind of account."),
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

    /// Deletes the portrait of `id`, after the commit that let go of it.
    fn discard_portrait(&self, id: CompanionId) {
        if let Err(error) = portrait::remove(&self.dir, id) {
            trace::line(&format!(
                "the portrait of {id} could not be deleted: {error}"
            ));
        }
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
        names: JournalNames::default(),
    }
}

/// Milliseconds since the Unix epoch.
fn epoch_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// The refusal of a sign-in its adapter could not start, in the lobby's words.
fn begin_refusal(outcome: Outcome, hint: &str) -> Refusal {
    let message = match outcome {
        Outcome::InvalidInput if hint.trim().is_empty() => {
            "This kind of account cannot be chosen on the sign-in page; type the account to sign in with first."
        }
        Outcome::InvalidInput => {
            "The provider cannot start a sign-in from what was typed; check it and try again."
        }
        Outcome::NotFound => "No account answers to what was typed; check it and try again.",
        Outcome::Unsupported => {
            "The provider's server does not offer the sign-in the lobby requires, so nothing was started."
        }
        Outcome::Refused => "The provider refused to start a sign-in.",
        Outcome::Conflict => "A sign-in like this one is already waiting.",
        Outcome::TemporarilyUnavailable | Outcome::RateLimited => {
            "The sign-in page could not be reached just now, so nothing was started. Try again in a moment."
        }
        other => return Refusal::from_adapter(other),
    };
    Refusal::new(outcome, message)
}

/// The refusal of a return its adapter did not accept, in the lobby's words.
fn complete_refusal(outcome: Outcome) -> Refusal {
    let message = match outcome {
        Outcome::Refused | Outcome::NotFound | Outcome::InvalidInput => {
            "The provider's answer did not prove the sign-in the lobby started, so nothing was signed in. Start it again on the manager page."
        }
        Outcome::Unsupported => "The lobby cannot complete sign-ins of this kind.",
        Outcome::TemporarilyUnavailable | Outcome::RateLimited => {
            "The sign-in page could not be reached just now, so nothing was signed in. Start it again on the manager page in a moment."
        }
        other => return Refusal::from_adapter(other),
    };
    Refusal::new(outcome, message)
}

fn gone() -> Refusal {
    Refusal::new(
        Outcome::NotFound,
        "No such attachment is live; attach again.",
    )
}

impl Core {
    /// `subject` with the names the sponsor reads now for what it names,
    /// where it names none yet.
    fn named(&self, mut subject: Subject) -> Subject {
        self.fill(
            &mut subject.names,
            subject.companion,
            subject.connection,
            subject.service.as_ref(),
            None,
        );
        subject
    }

    /// Fills the names `names` lacks for `companion`, `connection`,
    /// `service` and its `tool` from the state and the registry as they are
    /// now.
    fn fill(
        &self,
        names: &mut JournalNames,
        companion: Option<CompanionId>,
        connection: Option<ConnectionId>,
        service: Option<&ServiceName>,
        tool: Option<&ToolName>,
    ) {
        let state = self.store.state();
        if names.companion.is_none() {
            names.companion = companion
                .and_then(|id| state.companion(id))
                .map(|record| record.companion.display_name.clone());
        }
        if names.service.is_none() {
            names.service = service
                .and_then(|service| DisplayName::new(&self.registry.service_title(service)).ok());
        }
        if names.connection.is_none() {
            names.connection = connection
                .and_then(|id| state.connection(id))
                .map(|connection| connection.label.clone());
        }
        if names.tool.is_none()
            && let (Some(service), Some(tool)) = (service, tool)
        {
            names.tool = self.registry.service(service).and_then(|adapter| {
                adapter
                    .catalog()
                    .into_iter()
                    .find(|descriptor| descriptor.name == *tool)
                    .and_then(|descriptor| DisplayName::new(&descriptor.title).ok())
            });
        }
    }

    /// `subject` naming the kind of account `kind` by its title.
    fn with_kind(&self, mut subject: Subject, kind: &ProviderKind) -> Subject {
        subject.names.kind = DisplayName::new(&self.registry.title(kind)).ok();
        subject
    }

    /// `subject` naming `credential`'s account and its kind.
    fn with_account(&self, subject: Subject, credential: &Credential) -> Subject {
        let mut subject = self.with_kind(subject, &credential.kind);
        subject.names.account = Some(credential.label.clone());
        subject
    }

    /// The subject of a command about the credential `id`, with every name.
    fn credential_subject(
        &self,
        initiator: Initiator,
        id: CredentialId,
        name: &'static str,
    ) -> Subject {
        let credential = self.store.state().credential(id).cloned();
        let subject = self.named(Subject::new(
            initiator,
            credential.as_ref().map(|credential| credential.companion),
            name,
        ));
        match &credential {
            Some(credential) => self.with_account(subject, credential),
            None => subject,
        }
    }

    /// The credential of `kind` that `companion` holds and that needs
    /// signing in again, if it holds one.
    fn waiting_credential(
        &self,
        companion: CompanionId,
        kind: &ProviderKind,
    ) -> Option<Credential> {
        self.store
            .state()
            .credentials
            .iter()
            .find(|credential| {
                credential.companion == companion
                    && credential.kind == *kind
                    && credential.state == CredentialState::NeedsSignIn
            })
            .cloned()
    }

    /// The companion of the sign-in that ended under `state`, if the lobby
    /// remembers it.
    fn ended_for(&self, state: &str) -> Option<CompanionId> {
        self.ended
            .iter()
            .rev()
            .find(|(ended, _)| ended == state)
            .map(|(_, view)| view.companion)
    }

    /// Commits the credential `id` and every ready connection made from it
    /// as waiting for a sign-in, in one commit, then settles `subject`,
    /// whose pending entry was written before, with `settled`. Its material
    /// and what the services issued stay, for signing in again and for
    /// revoking at the provider; its rotating mark is cleared once the
    /// commit landed, since the state now says what the mark said.
    fn needs_sign_in(
        &mut self,
        id: CredentialId,
        subject: &Subject,
        settled: Outcome,
    ) -> Result<(), Refusal> {
        let mut next = self.store.state().clone();
        let mut credential = None;
        for slot in &mut next.credentials {
            if slot.id == id && slot.state == CredentialState::Ready {
                slot.state = CredentialState::NeedsSignIn;
                credential = Some(slot.clone());
            }
        }
        let Some(credential) = credential else {
            self.settle_as(subject, settled);
            return Ok(());
        };
        let mut waiting = Vec::new();
        for connection in &mut next.connections {
            if connection.credential == id && connection.state == ConnectionState::Ready {
                connection.state = ConnectionState::NeedsSignIn;
                connection.revision += 1;
                waiting.push(connection.clone());
            }
        }
        self.land_with(subject, next, settled)?;
        self.due.remove(&id);
        if let Err(error) = keyring::unmark_rotating(&self.dir, id) {
            trace::line(&format!(
                "the rotating mark of credential {id} could not be cleared: {error}"
            ));
        }
        self.events.push_back(Event::CredentialChanged(credential));
        self.events
            .extend(waiting.into_iter().map(Event::ConnectionChanged));
        Ok(())
    }

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
                "This companion is already signed in with an account of this kind; revoke it first.",
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

    /// Refuses a second sign-in of `kind` for `companion` while one waits.
    fn no_sign_in_waiting(
        &self,
        companion: CompanionId,
        kind: &ProviderKind,
    ) -> Result<(), Refusal> {
        let waiting = self
            .awaited
            .values()
            .any(|awaited| awaited.companion == companion && awaited.kind == *kind)
            || self.starting.contains(&(companion, kind.clone()));
        if waiting {
            return Err(Refusal::new(
                Outcome::Conflict,
                "A sign-in for this companion is already waiting; finish it or cancel it first.",
            ));
        }
        Ok(())
    }

    /// Ends the sign-in waiting under `state` as `status`.
    fn end_sign_in(&mut self, state: &str, status: SignInStatus) -> Option<SignInView> {
        let awaited = self.awaited.remove(state)?;
        let view = awaited.view(status);
        self.released.push((awaited.kind, awaited.request));
        self.remember_ended(state, view.clone());
        Some(view)
    }

    /// Remembers a sign-in that ended, and says so on the feed.
    fn remember_ended(&mut self, state: &str, view: SignInView) {
        self.ended.push_back((state.to_owned(), view.clone()));
        while self.ended.len() > SIGN_INS_REMEMBERED {
            self.ended.pop_front();
        }
        self.events.push_back(Event::SignInChanged(view));
    }

    /// Why a return under `state` finds no sign-in waiting, in the lobby's words.
    fn why_not_waiting(&self, state: &str) -> Refusal {
        let ended = self
            .ended
            .iter()
            .rev()
            .find(|(ended, _)| ended == state)
            .map(|(_, view)| view.status);
        let message = match ended {
            Some(SignInStatus::Cancelled) => {
                "This sign-in was cancelled, so nothing was signed in."
            }
            Some(SignInStatus::Expired) => {
                "This sign-in waited too long, so nothing was signed in. Start it again on the manager page."
            }
            Some(_) => {
                "This sign-in already finished. Start another on the manager page if you need one."
            }
            None => {
                "The lobby is not waiting for this sign-in; start it again on the manager page."
            }
        };
        Refusal::new(Outcome::NotFound, message)
    }

    /// Ends the sign-ins that waited past `limit` as expired.
    fn expire_overdue(&mut self, limit: Duration) {
        let expired: Vec<String> = self
            .awaited
            .iter()
            .filter(|(_, awaited)| awaited.started.elapsed() >= limit)
            .map(|(state, _)| state.clone())
            .collect();
        for state in expired {
            self.end_sign_in(&state, SignInStatus::Expired);
        }
    }

    /// Ends the oldest waiting sign-ins as expired until one more may wait.
    /// Only starting a sign-in makes room; reading the lobby never does.
    fn make_room(&mut self) {
        let mut kept: Vec<(&String, Instant)> = self
            .awaited
            .iter()
            .map(|(state, awaited)| (state, awaited.started))
            .collect();
        kept.sort_by_key(|(_, started)| *started);
        let excess = (kept.len() + 1).saturating_sub(SIGN_INS_WAITING);
        let oldest: Vec<String> = kept
            .into_iter()
            .take(excess)
            .map(|(state, _)| state.clone())
            .collect();
        for state in oldest {
            self.end_sign_in(&state, SignInStatus::Expired);
        }
    }

    /// Writes one journal entry, emitting it.
    fn record(&mut self, mut note: Note) -> io::Result<JournalEntry> {
        let (companion, connection, service, tool) = (
            note.companion,
            note.connection,
            note.service.clone(),
            note.tool.clone(),
        );
        self.fill(
            &mut note.names,
            companion,
            connection,
            service.as_ref(),
            tool.as_ref(),
        );
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
        self.land_with(subject, next, Outcome::Done)
    }

    /// [`Core::land_as`], settling a landed commit with `settled`.
    fn land_with(
        &mut self,
        subject: &Subject,
        next: State,
        settled: Outcome,
    ) -> Result<(), Refusal> {
        if let Err(error) = self.store.commit(next) {
            trace::line(&format!("{} could not be committed: {error}", subject.name));
            let refusal = Refusal::new(
                Outcome::TemporarilyUnavailable,
                "The lobby could not write its state file, so nothing was changed; try again.",
            );
            self.settle_as(subject, refusal.outcome);
            return Err(refusal);
        }
        self.settle_as(subject, settled);
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
        let subject = self.named(Subject::new(initiator, companion, name));
        self.begin_as(&subject)?;
        self.land_as(&subject, next)
    }

    /// Runs every emitted event through the handlers, in order, including the
    /// events the handlers' own commands emit.
    fn settle_events(&mut self, feed: &Feed, command: &str) {
        let mut changed: Vec<CompanionId> = Vec::new();
        while let Some(event) = self.events.pop_front() {
            // The feed handler: every event becomes one item of the live feed.
            feed.publish(&projections::event_json(
                &event,
                self.store.state(),
                command,
            ));
            match event {
                // The catalog handler: a companion whose connections changed
                // is noted, and hears it once when the drain ends.
                Event::ConnectionChanged(connection) => {
                    if !changed.contains(&connection.companion) {
                        changed.push(connection.companion);
                    }
                }
                // The keep-alive handler: a credential that changed is
                // looked at again at once.
                Event::CredentialChanged(_) => {
                    self.renewals.send_modify(|changes| *changes += 1);
                }
                // The cascade handler: a removed companion's attachments end.
                Event::CompanionRemoved(companion) => self.end_attachments_of(companion),
                _ => {}
            }
        }
        // Every attachment of each companion whose connections changed
        // learns, once for the whole drain, that its tools changed.
        for live in self.attachments.values() {
            if changed.contains(&live.attachment.companion) {
                live.catalog.send_modify(|changes| *changes += 1);
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

    #[test]
    fn a_sign_in_page_out_of_reach_is_named_as_the_sign_in_page() {
        for outcome in [Outcome::TemporarilyUnavailable, Outcome::RateLimited] {
            for refusal in [begin_refusal(outcome, "ada"), complete_refusal(outcome)] {
                assert_eq!(refusal.outcome, outcome);
                assert!(
                    refusal
                        .message
                        .starts_with("The sign-in page could not be reached just now"),
                    "{}",
                    refusal.message
                );
                assert!(!refusal.message.contains("service"), "{}", refusal.message);
            }
        }
    }

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
        let pairs = passbook_return(lobby, companion, authority, holder);
        lobby
            .complete_sign_in("passbook", pairs)
            .result
            .expect("signed in")
    }

    /// Starts a passbook sign-in and follows the browser to the authority
    /// and back, returning what the authority sent back.
    fn passbook_return(
        lobby: &Lobby,
        companion: CompanionId,
        authority: &lobby_testkit::passbook::PassbookAuthority,
        holder: &str,
    ) -> Vec<(String, String)> {
        use lobby_contracts::AddressPolicy;
        use lobby_outbound::{Outbound, Request};
        authority.sign_in_as(holder);
        let (_, start) = lobby
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
        query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .map(|(name, value)| {
                (
                    lobby_testkit::percent_decode(name),
                    lobby_testkit::percent_decode(value),
                )
            })
            .collect()
    }

    fn passbook_lobby(dir: &Dir) -> Lobby {
        let registry = Registry::new(
            vec![Arc::new(lobby_testkit::passbook::Passbook::new())],
            Vec::new(),
        );
        Lobby::open(&dir.0, "companion-lobby", registry).expect("open")
    }

    /// A lobby with the passbook and the board, a companion `ada` signed in
    /// at `authority` as `holder-ada`, and connected to `board` as `front`.
    fn connected_lobby(
        dir: &Dir,
        authority: &lobby_testkit::passbook::PassbookAuthority,
        board: &lobby_testkit::board::BoardServer,
    ) -> (Lobby, CompanionId, Credential, Connection) {
        let registry = Registry::new(
            vec![Arc::new(lobby_testkit::passbook::Passbook::new())],
            vec![Arc::new(lobby_testkit::board::Board::new())],
        );
        let lobby = Lobby::open(&dir.0, "companion-lobby", registry).expect("open");
        let ada = lobby
            .create_companion("ada", "Ada")
            .expect("ada")
            .record
            .companion
            .id;
        let credential = passbook_sign_in(&lobby, ada, authority, "holder-ada");
        let connection = lobby
            .grant_connection(Grant {
                companion: ada,
                credential: credential.id,
                service: "board",
                address: &board.origin(),
                label: "front",
            })
            .expect("connected");
        (lobby, ada, credential, connection)
    }

    fn states(lobby: &Lobby) -> (Vec<CredentialState>, Vec<ConnectionState>) {
        let snapshot = lobby.snapshot();
        (
            snapshot
                .credentials
                .iter()
                .map(|credential| credential.state)
                .collect(),
            snapshot
                .connections
                .iter()
                .map(|connection| connection.state)
                .collect(),
        )
    }

    #[test]
    fn a_renewal_commits_once_on_top_of_what_it_read_and_never_more_than_held() {
        let dir = Dir::new("commit-once");
        let authority = lobby_testkit::passbook::PassbookAuthority::start();
        let board = lobby_testkit::board::BoardServer::start(&authority);
        let (lobby, _, credential, _) = connected_lobby(&dir, &authority, &board);
        let slot = Slot::new(&lobby, credential.id);
        let read = slot.read().expect("material");
        let held = vec!["stamp".to_owned()];
        let first = MaterialUpdate::new(
            read.clone(),
            SealedMaterial::new(b"renewed once".to_vec()),
            held.clone(),
            &held,
        )
        .expect("an update");
        slot.commit(first).expect("it lands on what it read");
        let again = MaterialUpdate::new(
            read,
            SealedMaterial::new(b"renewed twice".to_vec()),
            held.clone(),
            &held,
        )
        .expect("an update");
        assert_eq!(
            slot.commit(again),
            Err(Outcome::Conflict),
            "an update made from material already replaced never lands"
        );
        assert_eq!(
            lobby.read_material(credential.id),
            Ok(SealedMaterial::new(b"renewed once".to_vec()))
        );
        let claimed = vec!["stamp".to_owned(), "more".to_owned()];
        let wider = MaterialUpdate::new(
            SealedMaterial::new(b"renewed once".to_vec()),
            SealedMaterial::new(b"widened".to_vec()),
            claimed.clone(),
            &claimed,
        )
        .expect("the adapter claims it held more");
        assert_eq!(
            slot.commit(wider),
            Err(Outcome::Refused),
            "the lobby holds the credential to what it was granted"
        );
        assert_eq!(
            lobby.read_material(credential.id),
            Ok(SealedMaterial::new(b"renewed once".to_vec()))
        );
    }

    #[test]
    fn a_renewal_left_marked_by_a_stopped_daemon_needs_sign_in_and_is_never_renewed_again() {
        let dir = Dir::new("left-marked");
        let authority = lobby_testkit::passbook::PassbookAuthority::start();
        let board = lobby_testkit::board::BoardServer::start(&authority);
        authority.key_lifetime(1);
        let (lobby, ada, credential, connection) = connected_lobby(&dir, &authority, &board);
        // The grant's stamp renewed the key first, as it was due.
        assert_eq!(commands(&dir, RENEW_CREDENTIAL), ["pending", "done"]);
        let presented = authority.renewals_presented().len();
        assert_eq!(presented, 1);
        // A renewal marks the material, and the daemon stops before the
        // provider's answer is kept.
        let slot = Slot::new(&lobby, credential.id);
        let read = slot.read().expect("material");
        slot.mark_rotating(&read).expect("marked");
        drop(slot);
        drop(lobby);

        let registry = Registry::new(
            vec![Arc::new(lobby_testkit::passbook::Passbook::new())],
            vec![Arc::new(lobby_testkit::board::Board::new())],
        );
        let lobby = Lobby::open(&dir.0, "companion-lobby", registry).expect("open again");
        assert_eq!(lobby.end_unfinished_renewals(), Ok(1));
        assert_eq!(
            states(&lobby),
            (
                vec![CredentialState::NeedsSignIn],
                vec![ConnectionState::NeedsSignIn]
            )
        );
        assert!(
            !dir.0.join(keyring::rotating_file(credential.id)).exists(),
            "the state says what the mark said"
        );
        assert_eq!(
            lobby.sweep_keyring(),
            Ok(0),
            "its files stay for signing in again"
        );
        lobby.keep_alive();
        assert_eq!(
            authority.renewals_presented().len(),
            presented,
            "the marked renewal is never presented"
        );
        assert_eq!(commands(&dir, END_UNFINISHED_RENEWAL), ["pending", "done"]);
        assert!(lobby.reached(ada).is_empty(), "its tools are gone");

        // Signing the same account in again restores it and its connection.
        let restored = passbook_sign_in(&lobby, ada, &authority, "holder-ada");
        assert_eq!(restored.id, credential.id, "the same credential");
        assert_eq!(
            states(&lobby),
            (vec![CredentialState::Ready], vec![ConnectionState::Ready])
        );
        let back = lobby.snapshot().connections[0].clone();
        assert_eq!(back.id, connection.id, "the same connection, no new grant");
        assert_eq!(back.revision, connection.revision + 2);
        assert_eq!(board.listener().count("/admit"), 1, "no new admission");
        assert_eq!(lobby.reached(ada).len(), 1, "its tools are back");
    }

    #[test]
    fn a_refused_renewal_leaves_the_credential_and_its_connections_waiting_in_one_commit() {
        let dir = Dir::new("refused-renewal");
        let authority = lobby_testkit::passbook::PassbookAuthority::start();
        let board = lobby_testkit::board::BoardServer::start(&authority);
        authority.key_lifetime(1);
        let (lobby, ada, _, _) = connected_lobby(&dir, &authority, &board);
        authority.refuse_renewals(true);
        let revision = std::fs::read_to_string(dir.0.join(crate::store::STATE_FILE))
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|state| state["revision"].as_u64())
            .expect("a revision");
        lobby.keep_alive();
        assert_eq!(
            states(&lobby),
            (
                vec![CredentialState::NeedsSignIn],
                vec![ConnectionState::NeedsSignIn]
            )
        );
        let after = std::fs::read_to_string(dir.0.join(crate::store::STATE_FILE))
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|state| state["revision"].as_u64())
            .expect("a revision");
        assert_eq!(after, revision + 1, "one commit");
        assert_eq!(
            commands(&dir, RENEW_CREDENTIAL),
            ["pending", "done", "pending", "needs_sign_in"],
            "the grant renewed the key once, and the next renewal was refused"
        );
        assert_eq!(authority.renewals_presented().len(), 2);
        lobby.keep_alive();
        assert_eq!(
            authority.renewals_presented().len(),
            2,
            "a credential that needs signing in is never renewed"
        );

        // Another account is refused while this one waits.
        let other = lobby
            .complete_sign_in(
                "passbook",
                passbook_return(&lobby, ada, &authority, "holder-other"),
            )
            .result
            .expect_err("another account");
        assert_eq!(other.outcome, Outcome::Conflict);
        assert_eq!(
            states(&lobby).0,
            [CredentialState::NeedsSignIn],
            "nothing was signed in"
        );
    }

    #[test]
    fn a_renewal_the_provider_asks_to_slow_down_changes_nothing() {
        let dir = Dir::new("rate-limited");
        let authority = lobby_testkit::passbook::PassbookAuthority::start();
        let board = lobby_testkit::board::BoardServer::start(&authority);
        authority.key_lifetime(1);
        let (lobby, _, credential, _) = connected_lobby(&dir, &authority, &board);
        let before = lobby.read_material(credential.id).expect("material");
        authority.rate_limit_renewals(true);
        lobby.keep_alive();
        assert_eq!(
            states(&lobby),
            (vec![CredentialState::Ready], vec![ConnectionState::Ready])
        );
        assert_eq!(lobby.read_material(credential.id), Ok(before));
        assert!(!dir.0.join(keyring::rotating_file(credential.id)).exists());
        assert_eq!(
            commands(&dir, RENEW_CREDENTIAL),
            ["pending", "done", "pending", "rate_limited"]
        );
    }

    fn commands(dir: &Dir, name: &str) -> Vec<String> {
        dir.journal()
            .iter()
            .filter(|entry| entry["command"] == name)
            .map(|entry| entry["outcome"].as_str().expect("outcome").to_owned())
            .collect()
    }

    #[test]
    fn a_companion_waits_on_one_sign_in_of_a_kind_and_the_sponsor_may_cancel_it() {
        let dir = Dir::new("waiting");
        let lobby = passbook_lobby(&dir);
        let authority = lobby_testkit::passbook::PassbookAuthority::start();
        let ada = lobby
            .create_companion("ada", "Ada")
            .expect("ada")
            .record
            .companion
            .id;
        authority.sign_in_as("holder-ada");
        let (first, _) = lobby
            .begin_sign_in(ada, "passbook", &authority.origin(), "http://127.0.0.1:1")
            .expect("a sign-in starts");
        assert_eq!(first.status, SignInStatus::Pending);
        assert_eq!(first.id.len(), 32);
        let again = lobby
            .begin_sign_in(ada, "passbook", &authority.origin(), "http://127.0.0.1:1")
            .expect_err("one waits already");
        assert_eq!(again.outcome, Outcome::Conflict);
        assert_eq!(lobby.snapshot().sign_ins, std::slice::from_ref(&first));

        let cancelled = lobby.cancel_sign_in(&first.id).expect("cancelled");
        assert_eq!(cancelled.status, SignInStatus::Cancelled);
        assert_eq!(
            lobby.snapshot().sign_ins,
            std::slice::from_ref(&cancelled),
            "the picture remembers how it ended"
        );
        assert_eq!(
            cancelled.account,
            authority.origin(),
            "the account the sponsor meant"
        );
        assert_eq!(
            lobby.cancel_sign_in(&first.id).map(|view| view.status),
            Err(Refusal::new(
                Outcome::NotFound,
                "No sign-in with that id is waiting; it may have ended already."
            ))
        );
        assert_eq!(
            commands(&dir, CANCEL_SIGN_IN),
            ["pending", "done", "not_found"]
        );

        // The browser comes back for a cancelled sign-in: nothing signs in.
        let pairs = passbook_return(&lobby, ada, &authority, "holder-ada");
        let waiting: Vec<SignInView> = lobby
            .snapshot()
            .sign_ins
            .into_iter()
            .filter(|sign_in| sign_in.status == SignInStatus::Pending)
            .collect();
        assert_eq!(waiting.len(), 1, "a new sign-in may start once none waits");
        lobby.cancel_sign_in(&waiting[0].id).expect("cancelled");
        let refused = lobby
            .complete_sign_in("passbook", pairs)
            .result
            .expect_err("cancelled");
        assert_eq!(refused.outcome, Outcome::NotFound);
        assert!(refused.message.contains("cancelled"), "{}", refused.message);
        assert!(lobby.snapshot().credentials.is_empty());
        assert_eq!(
            authority.listener().count("/exchange"),
            0,
            "the authority never heard of it"
        );
    }

    #[test]
    fn a_sign_in_that_waits_past_its_time_expires_and_signs_nothing_in() {
        let dir = Dir::new("expired");
        let lobby = passbook_lobby(&dir).with_sign_in_limit(Duration::from_millis(50));
        let authority = lobby_testkit::passbook::PassbookAuthority::start();
        let ada = lobby
            .create_companion("ada", "Ada")
            .expect("ada")
            .record
            .companion
            .id;
        let pairs = passbook_return(&lobby, ada, &authority, "holder-ada");
        std::thread::sleep(Duration::from_millis(100));
        let refused = lobby
            .complete_sign_in("passbook", pairs)
            .result
            .expect_err("expired");
        assert_eq!(refused.outcome, Outcome::NotFound);
        assert!(
            refused.message.contains("waited too long"),
            "{}",
            refused.message
        );
        assert!(lobby.snapshot().credentials.is_empty());
        let statuses: Vec<SignInStatus> = lobby
            .snapshot()
            .sign_ins
            .iter()
            .map(|sign_in| sign_in.status)
            .collect();
        assert_eq!(statuses, [SignInStatus::Expired]);
        assert_eq!(authority.listener().count("/exchange"), 0);
    }

    #[test]
    fn reading_the_lobby_never_ends_a_sign_in_that_still_has_time() {
        let dir = Dir::new("full");
        let lobby = passbook_lobby(&dir);
        let authority = lobby_testkit::passbook::PassbookAuthority::start();
        let mut started = Vec::new();
        for index in 0..=SIGN_INS_WAITING {
            let companion = lobby
                .create_companion(&format!("c{index}"), &format!("C {index}"))
                .expect("companion")
                .record
                .companion
                .id;
            let (view, _) = lobby
                .begin_sign_in(
                    companion,
                    "passbook",
                    &authority.origin(),
                    "http://127.0.0.1:1",
                )
                .expect("a sign-in starts");
            started.push(view.id);
            if index + 1 == SIGN_INS_WAITING {
                for _ in 0..3 {
                    let waiting = lobby
                        .snapshot()
                        .sign_ins
                        .iter()
                        .filter(|sign_in| sign_in.status == SignInStatus::Pending)
                        .count();
                    assert_eq!(waiting, SIGN_INS_WAITING, "every one still waits");
                }
            }
        }
        let picture = lobby.snapshot().sign_ins;
        let status = |id: &str| {
            picture
                .iter()
                .find(|sign_in| sign_in.id == id)
                .map(|sign_in| sign_in.status)
        };
        assert_eq!(
            status(&started[0]),
            Some(SignInStatus::Expired),
            "starting one more makes room by ending the oldest"
        );
        assert!(
            started[1..]
                .iter()
                .all(|id| status(id) == Some(SignInStatus::Pending))
        );
    }

    #[test]
    fn a_revoked_credential_stays_as_history_and_a_new_sign_in_makes_a_new_one() {
        let dir = Dir::new("history");
        let lobby = passbook_lobby(&dir);
        let authority = lobby_testkit::passbook::PassbookAuthority::start();
        let ada = lobby
            .create_companion("ada", "Ada")
            .expect("ada")
            .record
            .companion
            .id;
        let bea = lobby
            .create_companion("bea", "Bea")
            .expect("bea")
            .record
            .companion
            .id;
        let first = passbook_sign_in(&lobby, ada, &authority, "holder-ada");
        assert_eq!(first.permissions, ["stamp"]);
        lobby.revoke_credential(first.id).expect("revoked");
        let taken = lobby
            .complete_sign_in(
                "passbook",
                passbook_return(&lobby, bea, &authority, "holder-ada"),
            )
            .result
            .expect_err("the account stays reserved to ada");
        assert_eq!(taken.outcome, Outcome::Conflict);
        let second = passbook_sign_in(&lobby, ada, &authority, "holder-ada");
        assert_ne!(second.id, first.id);
        let credentials = lobby.snapshot().credentials;
        assert_eq!(credentials.len(), 2);
        assert_eq!(credentials[0].state, CredentialState::Revoked);
        assert_eq!(credentials[1].state, CredentialState::Ready);
        assert!(lobby.read_material(first.id).is_err());
        assert!(lobby.read_material(second.id).is_ok());
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
