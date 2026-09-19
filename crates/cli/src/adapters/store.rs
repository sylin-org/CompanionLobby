//! Durable connector state: atomic JSON snapshots. Guarantees of execution live at the
//! service; this store keeps only the connector's own memory.
//! the batch is saved. Crashes replay safely from these files.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use companion_core::domain::attention::{AttentionRecord, AttentionState, ATTENTION_RECORD_LIMIT};
use companion_core::domain::companion::{AccountSession, CallerId, Enrollment, Companion, Context};
use companion_core::domain::policy::AttentionPolicy;

const STATE_FILE: &str = "state.json";
const JOURNAL_LINE_LIMIT: u64 = 256 * 1024;

/// Public presentation only. The enrolled origin remains the routing companion.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerCard {
    pub origin: String,
    pub name: String,
    pub description: String,
    pub byline: String,
    pub cover_image_url: String,
    pub motd: String,
    pub owner_participant_id: String,
    pub refreshed_at: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StateFile {
    #[serde(default)]
    companions: Vec<Companion>,
    #[serde(default)]
    server_cards: HashMap<String, ServerCard>,
    /// The loopback companion manager URL (token included) of the long-running process
    /// currently hosting it, so a Connect in ANY process can pop that page at the
    /// sign-in anchor. Cookie-jar class by design: the page token is
    /// user-profile state, the same exposure class as the sessions above; cleared on
    /// clean shutdown and re-checked for reachability before use.
    #[serde(default)]
    manager_page_url: Option<String>,
    #[serde(default)]
    enrollments: Vec<Enrollment>,
    /// Bearer sessions per enrollment, keyed by companion id. Sessions live in
    /// user-profile state by design: a `ts_…` token is a
    /// cookie-equivalent session id, same exposure class as a browser cookie jar —
    /// not a vault secret. Two enrollments of one companion hold two distinct entries.
    #[serde(default)]
    sessions: HashMap<String, String>,
    /// Atproto sessions per companion, keyed by the companion's local id. Same cookie-jar
    /// posture as `sessions`: the PDS `accessJwt` is a session token,
    /// not a vault secret.
    #[serde(default)]
    atproto_sessions: HashMap<String, AccountSession>,
    #[serde(default)]
    contexts: Vec<Context>,
    #[serde(default)]
    aliases: HashMap<String, BTreeMap<String, String>>,
    #[serde(default)]
    alias_counters: HashMap<String, u32>,
    #[serde(default)]
    attention: HashMap<String, Vec<AttentionRecord>>,
    #[serde(default)]
    checkpoints: HashMap<String, String>,
    #[serde(default)]
    revisions: HashMap<String, String>,
    #[serde(default)]
    waiting_counts: HashMap<String, i64>,
    #[serde(default)]
    policy: Option<AttentionPolicy>,
}

/// The connector's durable state store. Interior state is guarded by the owning hub.
pub struct StateStore {
    path: PathBuf,
    state: StateFile,
}

impl StateStore {
    /// Loads (or initializes) state under the data directory.
    pub fn open(data_dir: &Path) -> Result<Self, String> {
        fs::create_dir_all(data_dir).map_err(|error| format!("cannot create data directory: {error}"))?;
        let path = data_dir.join(STATE_FILE);
        let state = match fs::read(&path) {
            Ok(bytes) if !bytes.is_empty() => serde_json::from_slice(&bytes)
                .map_err(|error| format!("state file is malformed: {error}"))?,
            _ => StateFile::default(),
        };
        Ok(Self { path, state })
    }


    /// Atomic snapshot write: unique temp file, write, sync, rename.
    pub fn save(&self) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(&self.state).map_err(|error| format!("cannot encode state: {error}"))?;
        atomic_write(&self.path, &bytes)
    }

    // ----- sessions -----

    pub fn server_card(&self, origin: &str) -> Option<ServerCard> {
        self.state.server_cards.get(origin).cloned()
    }

    pub fn set_server_card(&mut self, card: ServerCard) {
        self.state.server_cards.insert(card.origin.clone(), card);
    }

    /// The bearer session of one enrollment. The token is handed only to the port layer;
    /// it never renders, logs or journals.
    pub fn session(&self, enrollment_id: &str) -> Option<String> {
        self.state.sessions.get(enrollment_id).cloned()
    }

    pub fn set_session(&mut self, enrollment_id: &str, token: &str) {
        self.state.sessions.insert(enrollment_id.to_string(), token.to_string());
    }

    /// Whether an enrollment holds a session (status reporting only).
    pub fn has_session(&self, enrollment_id: &str) -> bool {
        self.state.sessions.contains_key(enrollment_id)
    }

    // ----- atproto sessions (per companion) -----

    /// The atproto session one companion holds. The token is handed only to the port layer;
    /// it never renders, logs or journals.
    pub fn atproto_session(&self, local_id: &str) -> Option<AccountSession> {
        self.state.atproto_sessions.get(local_id).cloned()
    }

    /// Stores (or replaces — re-bind) the companion's atproto session.
    pub fn set_atproto_session(&mut self, local_id: &str, session: AccountSession) {
        self.state.atproto_sessions.insert(local_id.to_string(), session);
    }

    /// Clears the companion's atproto session (unbind / companion cascade).
    pub fn remove_atproto_session(&mut self, local_id: &str) {
        self.state.atproto_sessions.remove(local_id);
    }

    /// The companion a given account DID is bound to, if any. The DID is the join key
    /// of the 1:1 companion-credential rule: one account can belong to exactly one
    /// companion.
    pub fn companion_by_bound_did(&self, did: &str) -> Option<Companion> {
        self.state.companions.iter().find(|companion| companion.bound_did.as_deref() == Some(did)).cloned()
    }

    /// The service classes this companion's credential may establish sessions for.
    /// `None` when no credential is held.
    pub fn granted_services(&self, local_id: &str) -> Option<Vec<String>> {
        self.state.atproto_sessions.get(local_id).map(|session| session.services.clone())
    }

    /// Replaces the credential's service grants (validation is the hub's; the store stores).
    pub fn set_granted_services(&mut self, local_id: &str, services: Vec<String>) {
        if let Some(session) = self.state.atproto_sessions.get_mut(local_id) {
            session.services = services;
        }
    }

    /// Drops every stored enrollment session of this companion — the flush half of
    /// unbinding: the credential is gone, so every session it established stops being
    /// usable immediately. The enrollment records stay as places-visited memories; their
    /// sessions read as missing until a re-bind connects again.
    pub fn flush_enrollment_sessions(&mut self, local_id: &str) -> usize {
        let ids: Vec<String> = self
            .state
            .enrollments
            .iter()
            .filter(|entry| entry.local_id == local_id)
            .map(|entry| entry.enrollment_id.clone())
            .collect();
        let mut flushed = 0;
        for id in ids {
            if self.state.sessions.remove(&id).is_some() {
                flushed += 1;
            }
        }
        flushed
    }

    pub fn policy(&self) -> AttentionPolicy {
        self.state.policy.clone().unwrap_or_default()
    }

    pub fn set_policy(&mut self, policy: AttentionPolicy) {
        self.state.policy = Some(policy);
    }

    // ----- companions -----

    pub fn companions(&self) -> &[Companion] {
        &self.state.companions
    }

    pub fn companion(&self, local_id: &str) -> Option<Companion> {
        self.state.companions.iter().find(|companion| companion.local_id == local_id).cloned()
    }

    /// Exact handle lookup, case-insensitive; `@`-prefixed input is accepted.
    pub fn companion_by_handle(&self, handle: &str) -> Option<Companion> {
        let supplied = handle.trim().strip_prefix('@').unwrap_or(handle.trim());
        self.state
            .companions
            .iter()
            .find(|companion| companion.handle.eq_ignore_ascii_case(supplied))
            .cloned()
    }

    pub fn companion_by_moniker(&self, moniker: &str) -> Option<Companion> {
        if let Some(companion) = self.companion_by_handle(moniker) {
            return Some(companion);
        }
        let trimmed = moniker.trim();
        self.state.companions.iter().find(|companion| companion.local_id == trimmed).cloned()
    }

    /// Inserts or updates one companion. The caller enforces handle validity; this side
    /// enforces connector-wide handle uniqueness (case-insensitive, excluding the companion
    /// being updated).
    pub fn upsert_companion(&mut self, companion: Companion) -> Result<(), String> {
        if let Some(clash) = self.state.companions.iter().find(|existing| {
            existing.local_id != companion.local_id && existing.handle.eq_ignore_ascii_case(&companion.handle)
        }) {
            return Err(format!("handle '{}' is already used by companion '{}'", companion.handle, clash.handle));
        }
        match self.state.companions.iter_mut().find(|existing| existing.local_id == companion.local_id) {
            Some(existing) => *existing = companion,
            None => self.state.companions.push(companion),
        }
        Ok(())
    }

    /// Removes one companion. Enrollments must be gone first (the hub cascades them);
    /// its atproto session goes too.
    pub fn remove_companion(&mut self, local_id: &str) {
        self.state.companions.retain(|companion| companion.local_id != local_id);
        self.state.atproto_sessions.remove(local_id);
    }

    // ----- the running companion manager (cross-process discovery) -----

    /// The companion manager URL a long-running process recorded (token included), if any.
    pub fn manager_page_url(&self) -> Option<String> {
        self.state.manager_page_url.clone()
    }

    /// Records the companion manager URL this process hosts. Callers save afterwards.
    pub fn set_manager_page_url(&mut self, url: &str) {
        self.state.manager_page_url = Some(url.to_string());
    }

    /// Clears the recorded companion manager URL (clean shutdown). Callers save afterwards.
    pub fn clear_manager_page_url(&mut self) {
        self.state.manager_page_url = None;
    }

    // ----- enrollments -----

    pub fn enrollments(&self) -> &[Enrollment] {
        &self.state.enrollments
    }

    pub fn enrollment(&self, enrollment_id: &str) -> Option<Enrollment> {
        self.state.enrollments.iter().find(|entry| entry.enrollment_id == enrollment_id).cloned()
    }

    pub fn find_enrollment(&self, moniker: &str) -> Option<Enrollment> {
        self.state.enrollments.iter().find(|entry| entry.matches(moniker)).cloned()
    }

    pub fn enrollments_of(&self, local_id: &str) -> Vec<Enrollment> {
        self.state.enrollments.iter().filter(|entry| entry.local_id == local_id).cloned().collect()
    }

    /// The enrollment of one companion at one canonical origin.
    pub fn enrollment_at(&self, local_id: &str, origin: &str) -> Option<Enrollment> {
        self.state
            .enrollments
            .iter()
            .find(|entry| entry.local_id == local_id && entry.origin == origin)
            .cloned()
    }

    pub fn upsert_enrollment(&mut self, entry: Enrollment) {
        match self.state.enrollments.iter_mut().find(|existing| existing.enrollment_id == entry.enrollment_id) {
            Some(existing) => *existing = entry,
            None => self.state.enrollments.push(entry),
        }
    }

    pub fn remove_enrollment(&mut self, enrollment_id: &str) {
        remove_enrollment_state(&mut self.state, enrollment_id);
    }

    // ----- contexts -----

    pub fn context(&self, context_id: &str) -> Option<Context> {
        self.state.contexts.iter().find(|context| context.context_id == context_id).cloned()
    }

    /// Reuses the live context for this caller/companion/origin binding, or issues a new one.
    /// A context never rebinds: mismatched companions or origins get distinct ids.
    pub fn bind_context(&mut self, caller: &CallerId, companion: &Enrollment, now: i64, service: &str) -> Context {
        if let Some(existing) = self.state.contexts.iter_mut().find(|context| {
            context.belongs_to(caller, &companion.enrollment_id) && context.origin == companion.origin
        }) {
            existing.last_used_at = now;
            return existing.clone();
        }
        let context = Context {
            context_id: format!("{service}_{}", short_uuid()),
            service: service.to_string(),
            caller: caller.clone(),
            enrollment_id: companion.enrollment_id.clone(),
            origin: companion.origin.clone(),
            participant_ref: companion.participant_ref.clone(),
            created_at: now,
            last_used_at: now,
        };
        self.state.contexts.push(context.clone());
        context
    }

    // ----- aliases -----

    /// Stable per-context short aliases. The canonical reference is always available in
    /// structured output; the alias is presentational and never valid in another context.
    pub fn alias_for(&mut self, context_id: &str, canonical: &str) -> String {
        let map = self.state.aliases.entry(context_id.to_string()).or_default();
        if let Some(existing) = map.get(canonical) {
            return existing.clone();
        }
        let counter = self.state.alias_counters.entry(context_id.to_string()).or_insert(0);
        *counter += 1;
        let alias = if canonical.matches("::").count() >= 3 {
            format!("p{counter}")
        } else if canonical.matches("::").count() == 2 {
            format!("t{counter}")
        } else {
            format!("z{counter}")
        };
        map.insert(canonical.to_string(), alias.clone());
        alias
    }

    pub fn aliases(&self, context_id: &str) -> BTreeMap<String, String> {
        self.state.aliases.get(context_id).cloned().unwrap_or_default()
    }

    // ----- attention -----

    pub fn attention_records(&self, enrollment_id: &str) -> Vec<AttentionRecord> {
        self.state.attention.get(enrollment_id).cloned().unwrap_or_default()
    }

    /// Upserts digest occurrences, coalescing by stable item companion. Returns how many
    /// occurrences matched an existing record (coalesced) and how many were genuinely new.
    pub fn upsert_attention(
        &mut self,
        enrollment_id: &str,
        items: &[adapter_service_tangent::contract::AttentionItemDto],
        revision: &str,
        now: i64,
    ) -> (Vec<String>, usize) {
        let records = self.state.attention.entry(enrollment_id.to_string()).or_default();
        let mut fresh = Vec::new();
        let mut coalesced = 0;
        for item in items {
            match records.iter_mut().find(|record| record.id == item.reference) {
                Some(record) => {
                    record.refresh(item.actor_name.clone(), truncate(&item.excerpt, 160), revision.to_string(), now);
                    coalesced += 1;
                }
                None => {
                    fresh.push(item.reference.clone());
                    records.push(AttentionRecord {
                        id: item.reference.clone(),
                        enrollment_id: enrollment_id.to_string(),
                        kind: item.kind.clone(),
                        actor_ref: item.actor_ref.clone(),
                        actor_name: item.actor_name.clone(),
                        scope_ref: item.scope_ref.clone(),
                        source_ref: item.source_ref.clone(),
                        relationship: item.relationship.clone(),
                        excerpt: truncate(&item.excerpt, 160),
                        source_revision: item.source_revision.clone(),
                        state: AttentionState::Pending,
                        first_seen_at: now,
                        last_seen_at: now,
                        revision_seen: revision.to_string(),
                    });
                }
            }
        }
        // Bounded retention: evict the oldest delivered records first.
        if records.len() > ATTENTION_RECORD_LIMIT {
            records.sort_by_key(|record| (record.state == AttentionState::Delivered, record.last_seen_at));
            let excess = records.len() - ATTENTION_RECORD_LIMIT;
            records.drain(0..excess);
        }
        (fresh, coalesced)
    }

    /// Complete authorized resynchronization: on a full first page whose directed count
    /// matches the server's waiting count, records the server no longer lists are withdrawn.
    /// Disappearance from a partial page never withdraws anything.
    pub fn resync_attention(
        &mut self,
        enrollment_id: &str,
        items: &[adapter_service_tangent::contract::AttentionItemDto],
        waiting_count: Option<i64>,
        page_complete: bool,
    ) -> usize {
        let Some(expected) = waiting_count else { return 0 };
        if !page_complete {
            return 0;
        }
        let directed: Vec<&str> = items
            .iter()
            .filter(|item| matches!(item.relationship.as_deref(), Some("addressed_to_you") | Some("replies_to_you")))
            .map(|item| item.reference.as_str())
            .collect();
        if directed.len() as i64 != expected {
            return 0;
        }
        let records = self.state.attention.entry(enrollment_id.to_string()).or_default();
        let before = records.len();
        records.retain(|record| {
            !matches!(record.relationship.as_deref(), Some("addressed_to_you") | Some("replies_to_you"))
                || directed.contains(&record.id.as_str())
        });
        before - records.len()
    }

    pub fn mark_attention_delivered(&mut self, enrollment_id: &str, ids: &[String]) {
        if let Some(records) = self.state.attention.get_mut(enrollment_id) {
            for record in records.iter_mut() {
                if ids.contains(&record.id) {
                    record.state = AttentionState::Delivered;
                }
            }
        }
    }

    pub fn set_waiting_count(&mut self, enrollment_id: &str, waiting: i64) {
        self.state.waiting_counts.insert(enrollment_id.to_string(), waiting);
    }

    pub fn waiting_count(&self, enrollment_id: &str) -> i64 {
        self.state.waiting_counts.get(enrollment_id).copied().unwrap_or(0)
    }

    // ----- checkpoints and ledgers -----

    pub fn checkpoint(&self, enrollment_id: &str) -> Option<String> {
        self.state.checkpoints.get(enrollment_id).cloned()
    }

    pub fn set_checkpoint(&mut self, enrollment_id: &str, checkpoint: &str) {
        self.state.checkpoints.insert(enrollment_id.to_string(), checkpoint.to_string());
    }

    /// The digest revision last observed for this companion.
    pub fn revision(&self, enrollment_id: &str) -> Option<String> {
        self.state.revisions.get(enrollment_id).cloned()
    }

    pub fn set_revision(&mut self, enrollment_id: &str, revision: &str) {
        self.state.revisions.insert(enrollment_id.to_string(), revision.to_string());
    }
}


/// Cascades every piece of derived state that belongs to one enrollment — including its
/// session.
fn remove_enrollment_state(state: &mut StateFile, enrollment_id: &str) {
    state.enrollments.retain(|entry| entry.enrollment_id != enrollment_id);
    state.sessions.remove(enrollment_id);
    state.attention.remove(enrollment_id);
    state.checkpoints.remove(enrollment_id);
    state.revisions.remove(enrollment_id);
    state.waiting_counts.remove(enrollment_id);
    state.contexts.retain(|context| context.enrollment_id != enrollment_id);
}

fn truncate(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        value.to_string()
    } else {
        let mut cut = limit;
        while cut > 0 && !value.is_char_boundary(cut) {
            cut -= 1;
        }
        format!("{}…", &value[..cut])
    }
}

pub fn short_uuid() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_string()
}

/// Connector-minted companion id: GUIDv7, 32 hex characters. Never formatted as a DID.
pub fn new_local_id() -> String {
    uuid::Uuid::now_v7().simple().to_string()
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    {
        let mut file = fs::File::create(&temporary).map_err(|error| format!("cannot create temp state file: {error}"))?;
        // Owner-only where the platform supports it: state.json carries the
        // cookie-jar sessions, so no other local account should read it. Windows has
        // no portable mode bits; its per-user profile ACLs are the boundary there.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600));
        }
        file.write_all(bytes).map_err(|error| format!("cannot write state: {error}"))?;
        file.sync_all().map_err(|error| format!("cannot sync state: {error}"))?;
    }
    match fs::rename(&temporary, path) {
        Ok(()) => Ok(()),
        Err(_) => {
            // Windows rename-over-existing can need a remove first.
            let _ = fs::remove_file(path);
            fs::rename(&temporary, path).map_err(|error| format!("cannot replace state file: {error}"))
        }
    }
}


/// Bounded file read for credentials and journals.
pub fn read_bounded(path: &Path, limit: u64) -> Result<String, String> {
    let mut file = fs::File::open(path).map_err(|error| format!("cannot open {}: {error}", path.display()))?;
    let mut bytes = Vec::new();
    std::io::Read::take(&mut file, limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    if bytes.len() as u64 > limit {
        return Err(format!("{} exceeds the supported size", path.display()));
    }
    String::from_utf8(bytes).map_err(|error| format!("{} is not valid UTF-8: {error}", path.display()))
}
