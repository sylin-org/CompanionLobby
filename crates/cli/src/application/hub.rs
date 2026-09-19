//! The connector hub: the single application orchestrator for every model-requested
//! operation and background check. Intakes (the stdio MCP edge and the command line) are
//! spokes in the same shape: each translates its input model into the closed [`Operation`]
//! vocabulary and calls [`ConnectorHub::execute`]; the channel is recorded for attribution
//! and never changes a domain outcome. Adapters (HTTP client, poller, store) are the
//! remaining spokes; none of them talks to another directly.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use adapter_auth_atproto::atproto_oauth::{self, AtprotoOauth, BindStart};
use crate::adapters::browser::{self, PageOpener};
use crate::adapters::manager::DEFAULT_PAGE_URL;
use crate::adapters::store::{ServerCard, StateStore};
use crate::application::bus::EventBus;
use adapter_service_tangent::contract::{self, ExperienceDto};
use crate::application::operations::{decode, Operation, ViewMode};
use companion_core::ports::{ExperienceError, RequestContext};
use companion_core::domain::events::DomainEvent;
use companion_core::domain::companion::{valid_handle, AccountSession, CallerId, Enrollment, Companion, Context};
use companion_core::domain::intake::IntakeChannel;
use companion_core::domain::refs;
use companion_core::domain::{attention::AttentionState, now_millis};
use crate::presentation::perspective::Perspective;
use crate::presentation::{render, RenderInput};

pub const DELIVERY_MODE: &str = "tool_response_only";
/// The exact exchange method of the atproto service-proof profile. Fixed by protocol;
/// the server's discovery document must agree, or the enrollment refuses honestly.
/// One source with the bind's OAuth rpc permission (the oauth module owns it).
pub const EXCHANGE_LXM: &str = atproto_oauth::EXCHANGE_LXM;
/// The public PDS used when the operator does not name one explicitly; the authoritative
/// origin from the account's DID document replaces it when the PDS reports one.
pub const DEFAULT_PDS: &str = "https://bsky.social";
/// How long a waiting-for-operator connect stays resumable before it ages out honestly.
/// Live coordination state, not durable enrollment state — ten minutes of operator
/// attention is the whole budget.
pub const PENDING_CONNECT_TIMEOUT_MS: i64 = 10 * 60 * 1000;
/// In-flight OAuth binds parked at once, across companions. A small bound: each pins
/// one DPoP key and one pushed request; an operator drives at most a few tabs.
const BIND_FLIGHT_LIMIT: usize = 8;

/// What a started sign-in will bind to: an existing companion (a re-bind), or a new
/// companion that is born from the sign-in itself — companions only ever come into
/// existence through a completed sign-in, one credential each.
#[derive(Clone)]
pub enum BindTarget {
    Existing(String),
    New,
}

/// One started, not-yet-completed OAuth bind (in memory only — live coordination
/// state, like a pending connect). Keyed by its OAuth `state`; single use.
struct BindFlight {
    target: BindTarget,
    created_at: i64,
    start: BindStart,
}

/// What a completed sign-in left behind, for the page to narrate honestly.
pub enum BindOutcome {
    /// The credential is bound; `created` says whether the companion was born with it.
    Bound { local_id: String, handle: String, created: bool },
    /// The account already belongs to another companion — nothing changed. The page
    /// routes the operator to that companion instead.
    AlreadyCompanion { local_id: String, handle: String },
}
/// Age-out scan cadence: one shared sweeper thread wakes at this period and drops
/// expired pendings, so a looping model's repeated Connects (each refreshing the
/// pending) can never pile up one sleeping thread per call.
const PENDING_CONNECT_SWEEP_MS: u64 = 15 * 1000;

/// A completed tool invocation: deterministic view text plus canonical structured facts.
pub struct ToolOutcome {
    pub is_error: bool,
    pub status: String,
    pub text: String,
    pub structured: Value,
}

/// Read-only per-enrollment attention/pending-write snapshot for the companion manager.
pub struct EnrollmentStatus {
    pub enrollment_id: String,
    pub companion_local_id: String,
    pub origin: String,
    pub waiting: i64,
    pub pending_attention: usize,
}

/// Read-only atproto binding status for the companion manager: what is bound and how old
/// the session is — never the access token.
pub struct AtprotoBinding {
    pub did: String,
    pub handle: String,
    pub pds: String,
    pub obtained_at: i64,
    /// The service classes this credential may establish sessions for.
    pub services: Vec<String>,
}

/// One service class in the connector's catalog.
pub struct ServiceClass {
    pub id: &'static str,
    pub label: &'static str,
    pub available: bool,
    /// Whether connecting names a place: forums are many servers; a single-instance
    /// service carries no address at all.
    pub needs_address: bool,
}

impl ToolOutcome {
}

/// Everything an intake-scoped operation needs after the context binding resolves.
pub struct CallFrame {
    pub request: RequestContext,
    pub companion: Enrollment,
    pub context_id: String,
}

/// One waiting-for-operator handshake the connector resumes by itself once the operator
/// completes the binding. In-memory only: live coordination, never
/// durable state — a restart simply asks the model to connect again.
#[derive(Clone)]
struct PendingConnect {
    local_id: String,
    handle: String,
    origin: String,
    initiator: String,
    recorded_at: i64,
}

pub struct ConnectorHub {
    services: std::sync::RwLock<std::collections::HashMap<String, std::sync::Arc<dyn companion_core::traits::ServiceAdapter>>>,
    auths: std::sync::RwLock<std::collections::HashMap<String, std::sync::Arc<dyn companion_core::traits::AuthAdapter>>>,
    store: Mutex<StateStore>,
    events: Arc<EventBus>,
    caller: CallerId,
    /// The loopback companion manager URL this process hosts (serve mode), set once at
    /// startup so `OpenRegistration` can construct the browser target internally.
    manager_page_url: Mutex<Option<String>>,
    /// The companion whose sign-in a `Connect` popped last: routes the next
    /// `OpenRegistration` to that companion's bind anchor instead of companion creation.
    pending_bind: Mutex<Vec<String>>,
    /// Browser targets already opened by this process: a looping model must not
    /// spawn one tab per retry. Keyed by the full target URL, so distinct anchors stay
    /// distinct.
    opened_pages: Mutex<HashSet<String>>,
    /// How pages reach a browser: nothing by default; the binary installs the platform
    /// browser with [`ConnectorHub::with_pages`].
    pages: PageOpener,
    default_page: String,
    /// Waiting-for-operator connects, shared with the one age-out sweeper.
    pending_connects: Arc<Mutex<Vec<PendingConnect>>>,
    /// The atproto OAuth client (the `/bind` flow's outbound spoke). Replaceable before
    /// serving (tests point the resolution origins at their fake).
    atproto_oauth: Arc<Mutex<Arc<AtprotoOauth>>>,
    /// Started OAuth binds, keyed by their OAuth `state` value.
    bind_flights: Mutex<Vec<BindFlight>>,
    /// How long a parked bind stays completable; [`atproto_oauth::FLIGHT_TTL_MS`] by
    /// default. A pub test seam shortens it so the TTL refusal is assertable without
    /// waiting out ten real minutes.
    bind_flight_ttl_ms: AtomicI64,
    /// Per-companion refresh serialization: one small mutex per companion so a MCP
    /// Connect and the operator auto-resume can never double-refresh one session. The
    /// map itself grows one entry per companion that ever refreshes.
    refresh_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// Public card refresh attempts are shared across companions visiting one origin.
    server_card_attempts: Mutex<HashMap<String, i64>>,
    /// Permission-shaped optional schemas learned from authenticated responses. They
    /// are ephemeral and keyed by caller-bound context, never a selected/global actor.
    optional_tools: Mutex<HashMap<String, BTreeSet<String>>>,
    /// Arms the single sweeper thread on the first recorded pending connect.
    sweep_once: std::sync::Once,
}

impl ConnectorHub {
    pub fn new(store: StateStore, events: Arc<EventBus>, caller: CallerId) -> Self {
        Self {
            services: std::sync::RwLock::new(std::collections::HashMap::new()),
            auths: std::sync::RwLock::new(std::collections::HashMap::new()),
            store: Mutex::new(store),
            events,
            caller,
            manager_page_url: Mutex::new(None),
            pending_bind: Mutex::new(Vec::new()),
            opened_pages: Mutex::new(HashSet::new()),
            pages: browser::silent(),
            default_page: DEFAULT_PAGE_URL.to_string(),
            pending_connects: Arc::new(Mutex::new(Vec::new())),
            atproto_oauth: Arc::new(Mutex::new(Arc::new(AtprotoOauth::new()))),
            bind_flights: Mutex::new(Vec::new()),
            bind_flight_ttl_ms: AtomicI64::new(atproto_oauth::FLIGHT_TTL_MS),
            refresh_locks: Mutex::new(HashMap::new()),
            server_card_attempts: Mutex::new(HashMap::new()),
            optional_tools: Mutex::new(HashMap::new()),
            sweep_once: std::sync::Once::new(),
        }
    }

    /// Gives the hub a way to open pages; the binary passes [`browser::system`].
    pub fn with_pages(mut self, pages: PageOpener) -> Self {
        self.pages = pages;
        self
    }

    /// Names the page address to fall back on when no record survives, so a test can
    /// point that last probe somewhere it knows is dead instead of depending on
    /// whether this machine happens to be running the real page.
    pub fn with_default_page(mut self, url: &str) -> Self {
        self.default_page = url.to_string();
        self
    }

    /// Installs one service adapter under its own name; the last install wins. The
    /// binary installs the Tangent adapter; tests install their fake through the same
    /// seam. An empty registry is a construction bug, never a runtime state.
    pub fn with_service(self, service: Arc<dyn companion_core::traits::ServiceAdapter>) -> Self {
        if let Ok(mut services) = self.services.write() {
            services.insert(service.name().to_string(), service);
        }
        self
    }

    /// Installs one auth adapter under its own name; the last install wins.
    pub fn with_auth(self, auth: Arc<dyn companion_core::traits::AuthAdapter>) -> Self {
        if let Ok(mut auths) = self.auths.write() {
            auths.insert(auth.name().to_string(), auth);
        }
        self
    }

    /// The service adapter the connector participates through. Programmer error if
    /// missing: every hub is built with one (build_hub or a test's fake).
    fn service(&self) -> Arc<dyn companion_core::traits::ServiceAdapter> {
        self.services
            .read()
            .ok()
            .and_then(|services| services.get("tangent").cloned())
            .expect("the tangent service adapter is installed")
    }

    /// The auth adapter that mints the bound exchange's service-auth proof.
    fn auth(&self) -> Arc<dyn companion_core::traits::AuthAdapter> {
        self.auths
            .read()
            .ok()
            .and_then(|auths| auths.get("atproto").cloned())
            .expect("the atproto auth adapter is installed")
    }

    /// Opens a page the operator asked for (the startup open, the tray), every time.
    pub fn open_page(&self, url: &str) {
        (self.pages)(url);
    }

    /// The Forum keys the tangent experience wire honestly carries today. A key
    /// whose server counterpart does not exist yet stays absent from the catalog
    /// (ADR 0001) — the grammar exists, the ring says otherwise.
    pub fn forum_support(&self) -> &'static [&'static str] {
        &[
            "Forum_List_Spaces",
            "Forum_List_Threads",
            "Forum_Read_Thread",
            "Forum_Post",
            "Forum_Join_Space",
            "Forum_Leave_Space",
            "Forum_Mark_Read",
            "Forum_Watch",
            "Forum_Open_Case",
        ]
    }

    /// The service monikers any companion's credential grants — which rings may
    /// appear in the catalog at all.
    pub fn granted_service_monikers(&self) -> BTreeSet<String> {
        let store = self.lock_store().expect("state lock");
        store.companions().iter()
            .filter_map(|companion| store.granted_services(&companion.local_id))
            .flatten()
            .collect()
    }

    /// The stewardship facts one live context has reported: case-tool names and
    /// `manage:`-prefixed user-management rungs, exactly as the envelope offered them.
    pub fn context_optional_tools(&self, context_id: &str) -> BTreeSet<String> {
        self.optional_tools.lock().ok()
            .and_then(|contexts| contexts.get(context_id).cloned())
            .unwrap_or_default()
    }

    pub fn optional_tool_names(&self) -> BTreeSet<String> {
        self.optional_tools.lock().map(|contexts| contexts.values()
            .flat_map(|names| names.iter().cloned()).collect()).unwrap_or_default()
    }

    fn observe_optional_tools(&self, context_id: &str, experience: &ExperienceDto) {
        let Some(capabilities) = experience.capabilities.as_ref() else { return; };
        let known = |name: &str| -> Option<Vec<String>> {
            match name {
                "list_moderation_cases" => Some(vec!["Forum_List_Cases".to_string()]),
                "read_moderation_case" => Some(vec!["Forum_Read_Case".to_string()]),
                "preview_moderation_action" => Some(vec!["Forum_Preview_Action".to_string()]),
                "apply_moderation_action" => Some(vec!["Forum_Escalate_Case".to_string()]),
                "warn_user" => Some(vec!["manage:warn".to_string()]),
                "timeout_user" => Some(vec!["manage:timeout".to_string()]),
                "suspend_user" => Some(vec!["manage:suspend".to_string()]),
                "ban_user" => Some(vec!["manage:ban".to_string()]),
                // One wire string covers both role rungs.
                "assign_roles" => Some(vec!["manage:add_role".to_string(), "manage:remove_role".to_string()]),
                _ => None,
            }
        };
        let offered = experience.place.allowed_actions.iter().map(String::as_str)
            .chain(experience.actions.iter().map(|action| action.name.as_str()))
            .filter_map(known).flatten().collect::<BTreeSet<_>>();
        if let Ok(mut contexts) = self.optional_tools.lock() {
            if capabilities.stewardship && !offered.is_empty() {
                contexts.insert(context_id.to_string(), offered);
            } else {
                contexts.remove(context_id);
            }
        }
    }

    fn invalidate_optional_tools(&self, context_id: &str) {
        if let Ok(mut contexts) = self.optional_tools.lock() { contexts.remove(context_id); }
    }

    /// Shortens how long a parked bind stays completable (milliseconds). A test seam
    /// for the TTL refusal; production always runs [`atproto_oauth::FLIGHT_TTL_MS`].
    pub fn set_bind_flight_ttl_ms(&self, milliseconds: i64) {
        self.bind_flight_ttl_ms.store(milliseconds, Ordering::Relaxed);
    }

    /// Replaces the atproto OAuth client (tests point its resolution origins at a fake
    /// authorization server). Call before serving; the bind flow reads it per request.
    pub fn set_atproto_oauth(&self, client: AtprotoOauth) {
        if let Ok(mut slot) = self.atproto_oauth.lock() {
            *slot = Arc::new(client);
        }
    }

    /// The current atproto OAuth client (cloned out — never held across network I/O).
    fn oauth(&self) -> Arc<AtprotoOauth> {
        self.atproto_oauth
            .lock()
            .map(|slot| slot.clone())
            .unwrap_or_default()
    }

    pub fn events(&self) -> Arc<EventBus> {
        self.events.clone()
    }

    /// Records this process's own companion manager URL in memory. The URL never enters
    /// model-visible output; only the internal browser open uses it. The full recording
    /// (memory + durable state for cross-process Connects) is
    /// [`ConnectorHub::announce_manager_page`].
    pub fn set_manager_page_url(&self, url: &str) {
        if let Ok(mut slot) = self.manager_page_url.lock() {
            *slot = Some(url.to_string());
        }
    }

    /// Records the companion manager URL this process hosts: in memory for this process's
    /// own opens, and in durable state so a Connect in ANY process (the CLI one-shots)
    /// can pop this page at the sign-in anchor. Cookie-jar class by design — same
    /// exposure class as the per-enrollment sessions.
    pub fn announce_manager_page(&self, url: &str) {
        self.set_manager_page_url(url);
        if let Ok(mut store) = self.lock_store() {
            store.set_manager_page_url(url);
            let _ = store.save();
        }
    }

    /// Clears the persisted companion manager URL on clean shutdown, so later Connects are
    /// not pointed at a page that died with this process.
    pub fn clear_persisted_manager_page(&self) {
        if let Ok(mut store) = self.lock_store() {
            store.clear_manager_page_url();
            let _ = store.save();
        }
    }

    /// The browser target a sign-in pop for this companion would open (this process's
    /// own page, or a recorded reachable one, at the companion's bind route).
    /// Test-visible mirror of the internal resolution, so the target is assertable
    /// without opening anything.
    pub fn sign_in_target_url(&self, local_id: &str) -> Option<String> {
        let (page, _) = self.sign_in_page()?;
        Some(format!("{page}{}", bind_anchor(local_id)))
    }

    /// Opens one browser target once per process. Returns whether THIS call is the
    /// first to open it — a repeated target answers `false` without opening again, so a
    /// looping model cannot pile up tabs. First-ness is independent of the page opener:
    /// a first call "opened" the page even when the opener opens nothing.
    fn open_page_once(&self, target: &str) -> bool {
        let fresh = self
            .opened_pages
            .lock()
            .map(|mut opened| opened.insert(target.to_string()))
            .unwrap_or(false);
        if !fresh {
            return false;
        }
        self.open_page(target);
        true
    }

    /// Removes one enrollment and its stored session. Enrollment state (attention,
    /// checkpoints, contexts) cascades; the server side is untouched. Answers the
    /// companion whose graph node changed.
    pub fn forget_enrollment(&self, enrollment_id: &str) -> Result<String, String> {
        self.attributed("manager.forget_enrollment", || {
            let mut store = self.lock_store()?;
            let entry = store.enrollment(enrollment_id).ok_or_else(|| "no enrollment matches that id".to_string())?;
            let local_id = entry.local_id.clone();
            store.remove_enrollment(enrollment_id);
            store.save()?;
            drop(store);
            self.publish_node(&local_id);
            Ok(local_id)
        })
    }

    // ---------- atproto binding (operator surface) ----------

    /// Stores (or replaces) one companion's atproto session and mirrors the DID onto the
    /// companion's `bound_did`. The write tail of the OAuth bind, so enrollments and
    /// reads see one consistent shape. Existing enrollments keep their own Tangent
    /// sessions.
    fn store_atproto_session(&self, local_id: &str, session: AccountSession) -> Result<Companion, String> {
        let mut store = self.lock_store()?;
        let mut updated = store.companion(local_id).ok_or_else(|| "no local companion matches that id".to_string())?;
        updated.bound_did = Some(session.did.clone());
        store.upsert_companion(updated.clone())?;
        store.set_atproto_session(local_id, session);
        store.save()?;
        Ok(updated)
    }

    /// Disconnects the companion's credential — and with it, immediately, every session
    /// that credential established: the binding is cleared and all of the companion's
    /// stored enrollment sessions are flushed, so the very next request that needs one
    /// fails honestly instead of limping on issued tokens. The enrollment records stay
    /// as places-visited memories; a re-bind connects to them again.
    pub fn unbind_atproto(&self, local_id: &str) -> Result<companion_core::domain::companion::Companion, String> {
        self.attributed("manager.unbind_atproto", || {
            let mut store = self.lock_store()?;
            let mut companion = store.companion(local_id).ok_or_else(|| "no local companion matches that id".to_string())?;
            companion.bound_did = None;
            store.upsert_companion(companion.clone())?;
            store.remove_atproto_session(local_id);
            store.flush_enrollment_sessions(local_id);
            store.save()?;
            drop(store);
            self.publish_node(local_id);
            Ok(companion)
        })
    }

    // ---------- atproto OAuth binding (the /bind route) ----------

    /// Starts one companion's OAuth bind (the `/bind` route's GET, with no
    /// interstitial). Without a handle it goes straight to the default
    /// authorization server (`COMPANION_LOBBY_AUTHSERVER`, else the public Bluesky
    /// one) and parks no pre-declared account: the exchange's mandatory `sub` claim
    /// will name the bound DID. With a handle (the self-hosted escape hatch) it runs
    /// the discovery first — handle → DID → DID document → PDS → authorization server —
    /// and the exchange must then agree with the resolved DID. Either way the answer is
    /// the authorize URL the operator's browser is redirected to (a 302); the
    /// provider's own UI handles account selection and sign-in. Starting a new bind
    /// for the same companion replaces its in-flight one; different companions bind
    /// concurrently. All network I/O happens outside every guard. A flight
    /// that cannot be parked is an honest failure — never a dangling redirect.
    pub fn begin_atproto_bind(&self, target: BindTarget, handle: Option<&str>, redirect_uri: &str) -> Result<String, String> {
        self.attributed("manager.atproto_bind_start", || {
            if let BindTarget::Existing(local_id) = &target {
                let store = self.lock_store()?;
                store.companion(local_id).ok_or_else(|| "no local companion matches that id".to_string())?;
            }
            let start = self.oauth().start(handle, redirect_uri)?;
            let authorize_url = start.authorize_url.clone();
            let mut flights = self
                .bind_flights
                .lock()
                .map_err(|_| "bind_unparkable: the connector's bind state is unavailable; restart the connector and try again".to_string())?;
            let now = now_millis();
            let ttl = self.bind_flight_ttl_ms.load(Ordering::Relaxed);
            // Age out, then keep one bind at a time per existing companion (new-companion
            // sign-ins are keyed by their own state and may run alongside each other).
            flights.retain(|flight| now.saturating_sub(flight.created_at) < ttl);
            if let BindTarget::Existing(local_id) = &target {
                flights.retain(|flight| !matches!(&flight.target, BindTarget::Existing(id) if id == local_id));
            }
            flights.push(BindFlight { target, created_at: now, start });
            while flights.len() > BIND_FLIGHT_LIMIT {
                flights.remove(0);
            }
            Ok(authorize_url)
        })
    }

    /// Completes one OAuth bind (the loopback callback): validates the state and the
    /// issuer (`iss` is mandatory — RFC 9207 — and must be the authorization server the
    /// flight started with), exchanges the code for tokens, and binds exactly the
    /// account the exchange's `sub` names — the account the operator authenticated as;
    /// on the `?handle=` discovery path a differing `sub` is the honest
    /// account_mismatch refusal. The PDS and canonical handle come from the bound
    /// DID's document when the flight parked none. The session — refresh token and
    /// DPoP key included, cookie-jar posture — is stored, any pending sign-in routing
    /// is cleared, and every waiting-for-operator connect for this companion resumes:
    /// the handshake finishes connector-side, with no model involved.
    pub fn complete_atproto_bind(&self, state: &str, code: &str, issuer: Option<&str>, redirect_uri: &str) -> Result<BindOutcome, String> {
        let outcome = self.attributed("manager.bind_account", || {
            // The issuer comes first (RFC 9207): without `iss` the callback does not
            // even name which authorization server answered, so nothing else is
            // trustworthy enough to try.
            let issuer = issuer.filter(|iss| !iss.is_empty()).ok_or_else(|| {
                "invalid_callback: the authorization server's callback carried no issuer (iss); start the bind again".to_string()
            })?;
            let flight = {
                let mut flights = match self.bind_flights.lock() {
                    Ok(flights) => flights,
                    Err(_) => return Err("state lock poisoned".to_string()),
                };
                let position = flights
                    .iter()
                    .position(|flight| flight.start.state == state)
                    .ok_or_else(|| {
                        "state_mismatch: no started bind matches that state (it may have expired or already completed); start the bind again".to_string()
                    })?;
                let flight = flights.remove(position);
                let ttl = self.bind_flight_ttl_ms.load(Ordering::Relaxed);
                if now_millis().saturating_sub(flight.created_at) >= ttl {
                    return Err("bind_expired: this bind started too long ago; start it again".to_string());
                }
                flight
            };
            if refs::acceptable_origin(issuer).as_deref() != Some(flight.start.authserver.as_str()) {
                return Err(format!(
                    "issuer_mismatch: the callback claims issuer {issuer}, but the bind started with {}",
                    flight.start.authserver
                ));
            }
            let tokens = self.oauth().exchange(&flight.start, code, redirect_uri)?;
            // The binding IS the authenticated account: `sub` (mandatory) names the
            // DID. Only the ?handle= discovery path pre-declared one to disagree with.
            if let Some(declared) = flight.start.did.as_deref() {
                if tokens.sub != declared {
                    return Err(
                        "account_mismatch: the authorized account resolves to a different DID than the bind started with; start the bind again".to_string(),
                    );
                }
            }
            let (pds, discovered_handle) = match (flight.start.pds.as_deref(), flight.start.handle.as_deref()) {
                (Some(pds), handle) => (pds.to_string(), handle.map(str::to_string)),
                (None, _) => {
                    let (pds, doc_handle) = self.oauth().resolve_account(&tokens.sub)?;
                    (pds, doc_handle)
                }
            };
            // The bound label: the DID document's canonical handle when one is known,
            // else the DID itself (honest, never a guess).
            let handle = discovered_handle.unwrap_or_else(|| tokens.sub.clone());
            // The 1:1 rule turns on the account's DID: one credential, one companion.
            let (owner, previous_grants) = {
                let store = self.lock_store()?;
                let owner = store.companion_by_bound_did(&tokens.sub);
                let grants = match (&flight.target, &owner) {
                    (BindTarget::Existing(id), None) => store.granted_services(id),
                    (BindTarget::Existing(id), Some(owner)) if owner.local_id == *id => store.granted_services(id),
                    _ => None,
                };
                (owner, grants)
            };
            if let Some(owner) = owner.filter(|owner| !matches!(&flight.target, BindTarget::Existing(id) if *id == owner.local_id)) {
                // The account already lives on another companion; nothing changes.
                return Ok(BindOutcome::AlreadyCompanion { local_id: owner.local_id.clone(), handle: owner.handle.clone() });
            }
            // One credential, one session — only its grants differ: a fresh companion
            // starts with the default grant, a re-bind carries the operator's existing
            // grants forward. The session records the ACCOUNT's handle; the companion's
            // own handle is presentation and may be renamed freely.
            let session = AccountSession {
                did: tokens.sub.clone(),
                handle: handle.clone(),
                access_jwt: tokens.access_token,
                refresh_jwt: tokens.refresh_token,
                pds,
                authserver: Some(flight.start.authserver.clone()),
                client_id: Some(flight.start.client_id.clone()),
                dpop_key: Some(flight.start.dpop_key.clone()),
                services: previous_grants.unwrap_or_else(|| vec!["tangent".to_string()]),
                obtained_at: now_millis(),
            };
            match flight.target {
                BindTarget::Existing(local_id) => {
                    self.store_atproto_session(&local_id, session)?;
                    self.publish_node(&local_id);
                    Ok(BindOutcome::Bound { local_id, handle, created: false })
                }
                BindTarget::New => {
                    // Sign-in as creation: the companion is born already bound, its
                    // handle defaulting to the account's own (a unique, valid fallback
                    // when that is taken or unusable). Moniker and display name are
                    // the operator's to change later.
                    let mut store = self.lock_store()?;
                    let companion = Companion {
                        local_id: crate::adapters::store::new_local_id(),
                        handle: fresh_handle(&store, &handle),
                        display_name: None,
                        bound_did: Some(session.did.clone()),
                        created_at: now_millis(),
                    };
                    let (local_id, companion_handle) = (companion.local_id.clone(), companion.handle.clone());
                    store.upsert_companion(companion)?;
                    store.set_atproto_session(&local_id, session);
                    store.save()?;
                    drop(store);
                    self.publish_node(&local_id);
                    Ok(BindOutcome::Bound { local_id, handle: companion_handle, created: true })
                }
            }
        });
        if let Ok(BindOutcome::Bound { local_id, .. }) = &outcome {
            self.clear_pending_bind(local_id);
            self.resume_pending_connects(local_id);
        }
        outcome
    }

    /// The per-companion refresh mutex. Leaf lock: taken only around one
    /// companion's refresh, never while holding the store lock (the refresh takes the
    /// store inside, briefly, in its own scopes).
    fn refresh_lock_of(&self, local_id: &str) -> Arc<Mutex<()>> {
        match self.refresh_locks.lock() {
            Ok(mut locks) => locks
                .entry(local_id.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone(),
            // A poisoned registry lock must not brick refreshes: an unsynchronized
            // refresh is still correct (rotation just converges on the last writer).
            Err(_) => Arc::new(Mutex::new(())),
        }
    }

    /// Silent refresh before use (the OAuth bind's promise): an access token inside its
    /// refresh margin is renewed from the stored refresh token with the session's DPoP
    /// key, and the renewed session is stored before the caller proceeds. One small
    /// per-companion mutex serializes this, so a MCP Connect and the operator
    /// auto-resume can never double-refresh — the later waiter re-reads the session
    /// and finds the earlier one's renewal. The refreshed `sub` must be the same
    /// account (mandatory) or the honest re-bind error. App-password sessions
    /// carry no refresh material and pass through; a token without a readable expiry
    /// is used as-is (the PDS refuses it honestly if stale). A store failure AFTER a
    /// rotation keeps the rotated tokens in the live store: the call proceeds on
    /// them and the next save persists them, rather than bricking on the stale disk
    /// copy. A failed refresh is the honest expired-session error.
    fn refresh_atproto_if_stale(&self, local_id: &str) -> Result<AccountSession, String> {
        // Serialization first; the session is re-read under the lock so a concurrent
        // refresh's result is seen instead of duplicated.
        let serializer = self.refresh_lock_of(local_id);
        let _serial = serializer.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let session = {
            let store = self.lock_store()?;
            store.atproto_session(local_id)
        };
        let Some(session) = session else {
            return Err(
                "atproto_session_missing: the atproto session is gone (it may have expired). Re-bind the companion on the companion manager.".to_string(),
            );
        };
        let (Some(refresh), Some(authserver), Some(key)) = (&session.refresh_jwt, &session.authserver, &session.dpop_key) else {
            return Ok(session);
        };
        if !atproto_oauth::access_needs_refresh(&session.access_jwt, now_millis() / 1000) {
            return Ok(session);
        }
        // The client id the grant lives under: the scope-declaring form for new
        // binds, the bare localhost origin for pre-scope-era sessions.
        let client_id = session
            .client_id
            .clone()
            .unwrap_or_else(atproto_oauth::bind_client_id);
        match self.oauth().refresh(authserver, refresh, key, &client_id) {
            Ok(tokens) => {
                if tokens.sub != session.did {
                    return Err(
                        "atproto_session_expired: the refresh returned a different account. Re-bind the companion on the companion manager.".to_string(),
                    );
                }
                let mut renewed = session;
                renewed.access_jwt = tokens.access_token;
                if let Some(rotated) = tokens.refresh_token {
                    renewed.refresh_jwt = Some(rotated);
                }
                renewed.obtained_at = now_millis();
                if self.store_atproto_session(local_id, renewed.clone()).is_err() {
                    // Persistence failed, not the session: the rotated tokens are
                    // already in the live store, so the next attempt (and the next
                    // save anywhere) reuses them instead of bricking.
                }
                Ok(renewed)
            }
            Err(_) => Err(
                "atproto_session_expired: the PDS session could not be renewed (it may have expired or been revoked). Re-bind the companion on the companion manager.".to_string(),
            ),
        }
    }

    /// Read-only atproto binding status (did, handle, PDS, session age) — never the
    /// access token.
    pub fn atproto_binding(&self, local_id: &str) -> Option<AtprotoBinding> {
        let store = self.lock_store().ok()?;
        store.atproto_session(local_id).map(|session| AtprotoBinding {
            did: session.did,
            handle: session.handle,
            pds: session.pds,
            obtained_at: session.obtained_at,
            services: session.services,
        })
    }

    /// The authentication providers this connector can sign in with, straight from the
    /// registry — the add screen's entire content.
    pub fn auth_providers(&self) -> Vec<String> {
        self.auths.read().map(|auths| auths.keys().cloned().collect()).unwrap_or_default()
    }

    /// The service classes the connector knows, and whether each is actually wired in
    /// this process — the single source for the page's access list. A known class that
    /// is not installed is listed honestly as unavailable, never hidden.
    pub fn service_catalog(&self) -> Vec<ServiceClass> {
        let installed = |name: &str| self.services.read().map(|registry| registry.contains_key(name)).unwrap_or(false);
        vec![
            ServiceClass { id: "tangent", label: "Tangent servers", available: installed("tangent"), needs_address: true },
            ServiceClass { id: "bluesky", label: "Bluesky", available: installed("bluesky"), needs_address: false },
        ]
    }

    /// Replaces the credential's service grants: which service classes may establish
    /// sessions with this companion's credential. Only existing, available services
    /// can be granted; a companion without a credential has nothing to grant.
    pub fn set_companion_services(&self, local_id: &str, services: &[String]) -> Result<Vec<String>, String> {
        self.attributed("manager.set_services", || {
            let catalog = self.service_catalog();
            for service in services {
                let Some(class) = catalog.iter().find(|class| class.id == *service) else {
                    return Err(format!("unknown_service: no '{service}' service exists in this connector"));
                };
                if !class.available {
                    return Err(format!(
                        "service_unavailable: the '{}' service is not available in this connector yet",
                        class.id
                    ));
                }
            }
            let mut store = self.lock_store()?;
            store.companion(local_id).ok_or_else(|| "no local companion matches that id".to_string())?;
            if store.granted_services(local_id).is_none() {
                return Err("no_credential: this companion holds no account sign-in; allow services after connecting one".to_string());
            }
            store.set_granted_services(local_id, services.to_vec());
            store.save()?;
            drop(store);
            self.publish_node(local_id);
            Ok(services.to_vec())
        })
    }

    /// The companion manager's whole companion table in one store guard: every companion with
    /// its atproto binding status (never the access token) and its enrollment count.
    /// This is the ONLY shape the page should read companions through — a caller that
    /// instead walks the store directly and then asks per-companion questions re-enters
    /// the store lock and deadlocks the whole hub (the live popped-page freeze).
    /// The manager's projection of one companion: the companion, its account status
    /// (never a token value), its service grants, and its places with session STATUS.
    /// One builder feeds the list view, mutation responses and the node-change events,
    /// so every surface shows the same shape.
    pub fn companion_node(&self, local_id: &str) -> Option<serde_json::Value> {
        let store = self.lock_store().ok()?;
        let companion = store.companion(local_id)?;
        Some(Self::node_of(&store, &companion))
    }

    /// Every companion as its node projection, in one store guard — the same
    /// one-guard discipline the page's list fetch has always relied on.
    pub fn companion_nodes(&self) -> Vec<serde_json::Value> {
        let store = self.lock_store().expect("state lock");
        store.companions().iter().map(|companion| Self::node_of(&store, companion)).collect()
    }

    fn node_of(store: &StateStore, companion: &companion_core::domain::companion::Companion) -> serde_json::Value {
        let binding = store.atproto_session(&companion.local_id);
        let enrollments: Vec<serde_json::Value> = store
            .enrollments_of(&companion.local_id)
            .iter()
            .map(|entry| {
                json!({
                    "enrollmentId": entry.enrollment_id,
                    "companionId": entry.local_id,
                    "origin": entry.origin,
                    "participantRef": entry.participant_ref,
                    "did": entry.did,
                    "handle": entry.handle,
                    "displayName": entry.display_name,
                    "autoCheck": entry.auto_check,
                    "sessionStatus": if store.session(&entry.enrollment_id).is_some() { "stored" } else { "missing" },
                })
            })
            .collect();
        json!({
            "localId": companion.local_id,
            "handle": companion.handle,
            "displayName": companion.display_name,
            "boundDid": companion.bound_did,
            "createdAt": companion.created_at,
            "atproto": binding.map(|session| json!({
                "did": session.did,
                "handle": session.handle,
                "pds": session.pds,
                "obtainedAt": session.obtained_at,
                "services": session.services,
            })),
            "enrollmentCount": enrollments.len(),
            "enrollments": enrollments,
        })
    }

    /// Publishes the node-change event every open manager copy converges on: the
    /// event carries the node itself, so no copy ever refetches to catch up.
    fn publish_node(&self, local_id: &str) {
        if let Some(node) = self.companion_node(local_id) {
            self.events.publish(DomainEvent::CompanionChanged { local_id: local_id.to_string(), node });
        }
    }

    /// Shared discovery step of the bound handshake: reads the server's own
    /// `/.well-known/tangent-mcp` document and validates that it offers exactly the
    /// service-proof exchange this connector speaks. The proof audience comes from the
    /// server, never hardcoded. Used by `enroll_bound` and by `Connect`'s pre-flight
    /// (an unusable server is an honest error before any operator attention is asked).
    fn discover_proof_spec(&self, canonical: &str) -> Result<contract::ServiceProofDto, String> {
        let raw = self
            .service()
            .discover(canonical, "/.well-known/tangent-mcp")
            .map_err(|error| discovery_error(&error))?;
        let discovery: contract::DiscoveryDto = serde_json::from_value(raw)
            .map_err(|error| format!("malformed discovery document: {error}"))?;
        let proof_spec = discovery.service_proof.ok_or_else(|| {
            "no_service_proof: this server offers no service-proof enrollment, which is the only way a companion enrolls".to_string()
        })?;
        if proof_spec.method != EXCHANGE_LXM {
            return Err(format!(
                "exchange_method_mismatch: the server expects '{}', but this connector speaks only '{EXCHANGE_LXM}'",
                proof_spec.method
            ));
        }
        if !proof_spec.audience.starts_with("did:") || proof_spec.audience.len() > 512 {
            return Err("invalid_audience: the server's proof audience is not a DID".to_string());
        }
        Ok(proof_spec)
    }


    /// Bound enrollment — the primary path: verify the server's discovery document,
    /// have the companion's PDS mint a service-auth proof for exactly that audience, and
    /// exchange the proof at `/mcp/token` for a Tangent session stored per enrollment.
    /// The proof JWT is ephemeral (created and consumed here); the audience comes from
    /// the server, never hardcoded; the token never renders, logs or journals.
    pub fn enroll_bound(&self, local_id: &str, origin: &str) -> Result<Enrollment, String> {
        self.attributed("manager.enroll_bound", || {
            let canonical = refs::acceptable_origin(origin)
                .ok_or_else(|| "Use one HTTPS server origin, or explicit loopback HTTP for development".to_string())?;
            let (companion, atproto, live) = {
                let store = self.lock_store()?;
                let companion = store.companion(local_id).ok_or_else(|| "no local companion matches that id".to_string())?;
                // The per-credential grant gates session establishment: a held
                // credential without the grant never mints a session identifier for
                // this service at all. No credential yet is a different honest answer —
                // the waiting-for-operator flow, checked further below.
                if let Some(services) = store.granted_services(local_id) {
                    if !services.iter().any(|service| service == "tangent") {
                        return Err("service_not_allowed: the Tangent service is not allowed for this companion's credential; the operator can allow it on the companion's page".to_string());
                    }
                }
                let atproto = store.atproto_session(local_id);
                // An existing record with a live session is the honest refusal; one whose
                // session was flushed (a disconnect) is a place to connect to again, and
                // it stays until a fresh exchange actually replaces it.
                let live = store.enrollment_at(local_id, &canonical).is_some_and(|entry| store.session(&entry.enrollment_id).is_some());
                (companion, atproto, live)
            };
            if live {
                return Err(
                    "already_enrolled: this companion already holds a session for that server. Use the existing enrollment, or forget it first to re-enroll."
                        .to_string(),
                );
            }
            let Some(bound_did) = companion.bound_did.clone() else {
                return Err(
                    "atproto_binding_required: bind this companion to an atproto account on the companion manager first".to_string(),
                );
            };
            let Some(atproto) = atproto else {
                return Err(
                    "atproto_session_missing: the atproto session is gone (it may have expired). Re-bind the companion on the companion manager."
                        .to_string(),
                );
            };
            if atproto.did != bound_did {
                return Err(
                    "atproto_binding_stale: the bound DID and the stored atproto session disagree. Re-bind the companion on the companion manager."
                        .to_string(),
                );
            }
            // Silent refresh before use: an OAuth access token inside its margin renews
            // here, outside every guard and under the companion's refresh mutex;
            // the renewed session is already stored.
            let atproto = self.refresh_atproto_if_stale(local_id)?;

            // Step 1 — discovery: the proof audience comes from the server's own document.
            let proof_spec = self.discover_proof_spec(&canonical)?;

            // Step 2 — the PDS mints the proof: the discovery audience, the exact
            // exchange method, and an expiry inside the server's accepted window. The
            // adapter percent-encodes the parameters (a crafted audience or origin can
            // never inject query structure), proves the request with the session's
            // DPoP key (htm/htu of this exact call, `ath` binding the access
            // token, and the PDS's OWN nonce — RFC 9449 §8 gives each server its own
            // nonce context) and retries once on the resource server's challenge.
            let auth_state = serde_json::json!({
                "pds": atproto.pds,
                "access_jwt": atproto.access_jwt,
                "dpop_key": atproto.dpop_key,
            }).to_string();
            let token = self
                .auth()
                .get_service_auth(&auth_state, &proof_spec.audience)
                .map_err(|error| service_auth_error(&error))?;
            if token.is_empty() {
                return Err("the PDS returned no service-auth token".to_string());
            }

            // Step 3 — the exchange. Grants are bounded to welcome/read/post; manage is
            // explicitly never requested. The bearer is the ephemeral proof JWT:
            // created above, consumed here, never stored or logged.
            let body = json!({ "name": companion.handle, "lifetimeDays": 7, "grants": ["welcome", "read", "post"] });
            let raw = self
                .service()
                .exchange(&canonical, "/mcp/token", &body, &token)
                .map_err(|error| exchange_error(&error))?;
            let exchanged: contract::BoundExchangeDto = serde_json::from_value(raw)
                .map_err(|error| format!("malformed exchange response: {error}"))?;
            if exchanged.token.is_empty() {
                return Err("the server confirmed the exchange without a session".to_string());
            }
            let credential = exchanged
                .credential
                .ok_or_else(|| "the server confirmed the exchange without a credential view".to_string())?;
            if credential.participant_id.is_empty() {
                return Err("the server did not confirm a participant reference".to_string());
            }

            let mut store = self.lock_store()?;
            // Re-check under the write lock: a concurrent intake may have enrolled this
            // (companion, origin) while the exchange was in flight. The freshly issued
            // session is then discarded server-side untouched, and the honest answer is
            // already_enrolled with the existing enrollment intact. A record whose
            // session was flushed (a disconnect) is replaced, not protected.
            if let Some(existing) = store.enrollment_at(local_id, &canonical) {
                if store.session(&existing.enrollment_id).is_some() {
                    return Err(format!(
                        "already_enrolled: companion '{}' gained an enrollment at {canonical} during the exchange ({}); use it, or forget it first to re-enroll",
                        companion.handle, existing.enrollment_id
                    ));
                }
                store.remove_enrollment(&existing.enrollment_id);
            }
            // The Tangent session is stored per enrollment, keyed by its fresh companion
            // id — one companion at two servers keeps two distinct sessions, and the
            // companion-level atproto session is a third, separate thing.
            let enrollment_id = format!("cmp_{}", crate::adapters::store::short_uuid());
            let entry = Enrollment {
                enrollment_id,
                local_id: companion.local_id.clone(),
                name: companion.handle.clone(),
                origin: canonical,
                participant_ref: credential.participant_id,
                did: Some(bound_did),
                display_name: companion.display_name.clone().or(Some(atproto.handle.clone())),
                handle: Some(atproto.handle.clone()),
                enrolled_at: now_millis(),
                auto_check: true,
            };
            store.upsert_enrollment(entry.clone());
            store.set_session(&entry.enrollment_id, &exchanged.token);
            store.save()?;
            drop(store);
            self.publish_node(&entry.local_id);
            self.events.publish(DomainEvent::CompanionSelected { enrollment_id: entry.enrollment_id.clone() });
            Ok(entry)
        })
    }

    // ---------- the on-the-fly handshake (Connect) ----------

    /// `Connect { serverUrl, companion? }` — the on-the-fly handshake :
    /// the model says "connect to server X" and enrollment is a consequence, not a
    /// ceremony. (a) the acting companion resolves by behavior — an explicit `companion`
    /// argument (exact match), or exactly one local companion, for every intake alike;
    /// (b) discovery against the operator-supplied origin; (c) with no usable atproto
    /// binding the handshake pops the companion manager (this process's own, or a recorded
    /// reachable one) at that companion's sign-in anchor and returns honestly; (d) with
    /// a binding, enrollment runs only when no usable
    /// enrollment/session exists for the origin, and the handshake exits through
    /// `Arrive`'s orientation view led by the "You are … — session …" line.
    /// `SelectCompanion` + `Arrive` stay the explicit path.
    /// The front door: connect to a service as a persona, always explicitly named.
    /// The persona is resolved exactly (an honest miss lists what exists), the
    /// per-credential grant gates session establishment, a forum names its place, and
    /// the answer mints the labeled session with the full briefing.
    fn connect(&self, service_name: &str, persona: &str, address: Option<&str>, initiator: &str) -> ToolOutcome {
        let catalog = self.service_catalog();
        let Some(class) = catalog.iter().find(|class| class.id == service_name) else {
            let known: Vec<&str> = catalog.iter().map(|class| class.id).collect();
            return self.problem_outcome(
                "Connect",
                "unknown_service",
                &format!("no '{service_name}' service exists in this connector. The services are: {}.", known.join(", ")),
                None,
            );
        };
        if !class.available {
            return self.problem_outcome(
                "Connect",
                "service_unavailable",
                &format!("the '{}' service is not available in this connector yet", class.id),
                None,
            );
        }
        // A forum is many places; the address names which one. Single-instance
        // services carry no address at all.
        let canonical = if class.needs_address {
            let Some(address) = address else {
                return self.problem_outcome(
                    "Connect",
                    "address_required",
                    &format!("the '{}' service is many places; name one with its address (an https origin)", class.id),
                    None,
                );
            };
            let Some(canonical) = refs::acceptable_origin(address) else {
                return self.problem_outcome(
                    "Connect",
                    "invalid_arguments",
                    "Use one HTTPS server origin, or explicit loopback HTTP for development.",
                    None,
                );
            };
            canonical
        } else {
            String::new()
        };
        // The persona is always explicit; an honest miss lists what exists.
        let companion = match self.resolve_persona(persona) {
            Ok(companion) => companion,
            Err(reason) => {
                self.connect_failed(&canonical, "", "persona_unknown", initiator);
                return self.persona_question(&reason);
            }
        };
        // No credential yet is a different honest answer than a missing grant: the
        // waiting-for-operator flow, which pops the sign-in and finishes by itself.
        if !self.usable_binding(&companion.local_id) {
            return self.pop_sign_in(&companion, &canonical, false, initiator);
        }
        // The per-credential grant gates session establishment: without it, no session
        // identifier is ever minted for this service.
        {
            let store = self.lock_store().expect("state lock");
            let granted = store.granted_services(&companion.local_id)
                .is_some_and(|services| services.iter().any(|service| service == service_name));
            if !granted {
                return self.problem_outcome(
                    "Connect",
                    "service_not_allowed",
                    &format!("the '{service_name}' service is not allowed for {}'s credential; the operator can allow it on the companion's page", companion.handle),
                    None,
                );
            }
        }
        // Coalescing: a live pending for this (companion, origin) means the waiting
        // state is already narrated on the feed — a looping caller's repeated Connects
        // refresh the pending but stay quiet until state changes.
        let repeated = self.touch_pending_connect(&companion.local_id, &canonical);
        if !repeated {
            self.events.publish(DomainEvent::ConnectStarted { origin: canonical.clone(), initiator: initiator.to_string() });
            self.events.publish(DomainEvent::ConnectResolved {
                origin: canonical.clone(),
                companion: companion.handle.clone(),
                initiator: initiator.to_string(),
            });
        }
        // Discovery before any operator attention is requested: a server that cannot
        // do the proof exchange is an honest error, never a popped page.
        if let Err(error) = self.discover_proof_spec(&canonical) {
            let outcome = self.enrollment_problem("Connect", &error);
            self.connect_failed(&canonical, &companion.handle, &problem_code_of(&outcome), initiator);
            return outcome;
        }
        self.clear_pending_bind(&companion.local_id);
        self.drop_pending_connect(&companion.local_id, &canonical);
        self.connect_finish(&companion, &canonical, initiator)
    }

    /// Persona resolution is exact: the moniker names one companion, or the honest
    /// miss says so and lists what exists.
    fn resolve_persona(&self, persona: &str) -> Result<Companion, String> {
        let store = self.lock_store().expect("state lock");
        store.companion_by_moniker(persona).ok_or_else(|| format!("No companion matches '{persona}'"))
    }

    /// The honest unknown-persona answer: why it failed, which companions exist, and
    /// the way forward. Never a guess, never machine-wide.
    fn persona_question(&self, reason: &str) -> ToolOutcome {
        let handles: Vec<String> = self.companions().iter().map(|companion| companion.handle.clone()).collect();
        let message = if handles.is_empty() {
            format!("{reason}. No companions exist yet; ask the operator to sign one in (the manager's add screen), then connect again.")
        } else {
            format!("{reason}. Available companions: {}. ListCompanions names them; connect again with persona set to one of them.", handles.join(" · "))
        };
        self.problem_outcome("Connect", "persona_unknown", &message, None)
    }

    /// Who exists and what each may reach: the persona monikers and their granted
    /// services — facts, so a caller learns what is connectable in the same breath.
    fn list_companions_view(&self) -> ToolOutcome {
        let entries: Vec<(String, Option<String>, Vec<String>)> = {
            let store = self.lock_store().expect("state lock");
            store.companions()
                .iter()
                .map(|companion| {
                    (
                        companion.handle.clone(),
                        companion.display_name.clone(),
                        store.granted_services(&companion.local_id).unwrap_or_default(),
                    )
                })
                .collect()
        };
        let mut text = String::from("Companions:");
        for (handle, display, services) in &entries {
            let name = display.clone().unwrap_or_else(|| handle.clone());
            let services = if services.is_empty() { "no services granted".to_string() } else { services.join(", ") };
            text.push_str(&format!("\n{name} ({handle}) — may reach: {services}"));
        }
        if entries.is_empty() {
            text.push_str(" none yet. Ask the operator to sign one in (the manager's add screen).");
        }
        let structured = json!({
            "experience": null,
            "problem": null,
            "connector": {
                "view": "compact",
                "deliveryMode": DELIVERY_MODE,
                "companions": entries.iter().map(|(handle, display, services)| json!({
                    "persona": handle,
                    "displayName": display,
                    "services": services,
                })).collect::<Vec<_>>(),
            }
        });
        ToolOutcome { is_error: false, status: "ok".into(), text, structured }
    }

    /// The anchor: identity facts and the live capability readout for one session.
    /// Facts only — the ring as of now, never advice.
    fn who_am_i(&self, session: &str) -> ToolOutcome {
        let binding = {
            let store = self.lock_store().expect("state lock");
            store.context(session)
        };
        let Some(binding) = binding else { return self.context_expired("WhoAmI") };
        let Some(enrollment) = self.companion_of(&binding.enrollment_id) else {
            return self.context_expired("WhoAmI");
        };
        if !binding.belongs_to(&self.caller, &enrollment.enrollment_id) || binding.origin != enrollment.origin {
            return self.context_expired("WhoAmI");
        }
        let (persona, display) = {
            let store = self.lock_store().expect("state lock");
            match store.companion(&enrollment.local_id) {
                Some(companion) => (companion.handle, companion.display_name),
                None => (enrollment.handle.clone().unwrap_or_default(), enrollment.display_name.clone()),
            }
        };
        // The capability readout: this ring's keys as the service supports them, plus
        // the stewardship authority the live context has reported.
        let ring: Vec<&str> = self.forum_support().to_vec();
        let steward = self.context_optional_tools(&binding.context_id);
        let manage: Vec<String> = steward.iter().filter(|name| name.starts_with("manage:")).map(|name| name[7..].to_string()).collect();
        let case_tools: Vec<&str> = ring.iter().copied().chain(steward.iter().map(String::as_str)).collect();
        let mut text = format!(
            "You are {persona}{} — session {}",
            display.as_deref().map(|name| format!(" ({name})")).unwrap_or_default(),
            binding.context_id
        );
        text.push_str(&format!("\nService: {} · {}", binding.service, enrollment.origin));
        text.push_str(&format!("\nYou may: {}", case_tools.join(", ")));
        if !manage.is_empty() {
            text.push_str(&format!("\nStewardship: {}", manage.join(", ")));
        }
        text.push_str(&format!("\nSession minted {}.", millisecond_stamp(binding.created_at)));
        let structured = json!({
            "experience": null,
            "problem": null,
            "connector": {
                "view": "compact",
                "deliveryMode": DELIVERY_MODE,
                "session": binding.context_id,
                "persona": persona,
                "displayName": display,
                "service": binding.service,
                "place": enrollment.origin,
                "capabilities": { "tools": case_tools, "manageUser": manage },
                "mintedAt": binding.created_at,
            }
        });
        ToolOutcome { is_error: false, status: "ok".into(), text, structured }
    }

    /// Whether the companion holds an atproto binding usable for the proof exchange: a
    /// `bound_did` with a matching stored session. This is local staleness only — the
    /// PDS may still refuse the session, which the exchange maps honestly.
    fn usable_binding(&self, local_id: &str) -> bool {
        let Ok(store) = self.lock_store() else { return false };
        let Some(companion) = store.companion(local_id) else { return false };
        match store.atproto_session(local_id) {
            Some(session) => companion.bound_did.as_deref() == Some(session.did.as_str()),
            None => false,
        }
    }

    fn clear_pending_bind(&self, local_id: &str) {
        if let Ok(mut pendings) = self.pending_bind.lock() {
            pendings.retain(|id| id != local_id);
        }
    }

    fn page_url(&self) -> Option<String> {
        self.manager_page_url.lock().ok()?.clone()
    }

    /// The companion manager a sign-in pop should open, and whether THIS process hosts it.
    /// Order: this process's own page first (trusted — it is in-process and alive);
    /// otherwise the page URL the current long-running process recorded in state, but
    /// only after one cheap reachability probe, so a stale record from an unclean
    /// shutdown points no one at a dead port. The fixed-port world adds one last
    /// honest probe: with the deterministic default URL, a host that is running can be
    /// found even when no record survived.
    fn sign_in_page(&self) -> Option<(String, bool)> {
        if let Some(page) = self.page_url() {
            return Some((page, true));
        }
        let recorded = {
            let store = self.lock_store().ok()?;
            store.manager_page_url()
        };
        for candidate in recorded.into_iter().chain([self.default_page.clone()]) {
            if let Some(origin) = page_origin(&candidate) {
                if self.service().probe_page(&origin).is_ok() {
                    return Some((candidate, false));
                }
            }
        }
        None
    }

    /// The waiting-for-operator branch (c): pops the companion manager at this companion's
    /// sign-in anchor (guarded, once per target per process), records the pending
    /// connect so the handshake auto-resumes when the operator completes the binding,
    /// narrates it on the feed, and returns the honest outcome. No enrollment side
    /// effect happens on this branch. `repeated` means an identical pending is
    /// already narrated: the pending refreshes but the feed stays quiet.
    fn pop_sign_in(&self, companion: &Companion, canonical: &str, repeated: bool, initiator: &str) -> ToolOutcome {
        if let Ok(mut pendings) = self.pending_bind.lock() {
            pendings.retain(|id| id != &companion.local_id);
            pendings.push(companion.local_id.clone());
        }
        if !repeated {
            self.record_pending_connect(companion, canonical, initiator);
            self.events.publish(DomainEvent::ConnectWaitingForOperator {
                origin: canonical.to_string(),
                companion: companion.handle.clone(),
                needed: format!("sign in companion '{}': its atproto account (the provider's sign-in page opens in the browser)", companion.handle),
                initiator: initiator.to_string(),
            });
        }
        let page = self.sign_in_page();
        let opened = page.as_ref().map(|(url, in_process)| {
            let target = format!("{url}{}", bind_anchor(&companion.local_id));
            let fresh = self.open_page_once(&target);
            (fresh, *in_process)
        });
        let message = match opened {
            Some((true, true)) => format!(
                "operator action needed — page opened to sign in companion '{}'; ask the operator, then connect again. The connect also finishes by itself once the sign-in is done.",
                companion.handle
            ),
            Some((true, false)) => format!(
                "operator action needed — page opened to sign in companion '{}'; ask the operator, then connect again.",
                companion.handle
            ),
            Some((false, _)) => format!(
                "operator action needed — page already opened to sign in companion '{}'; ask the operator, then connect again.",
                companion.handle
            ),
            None => format!(
                "operator action needed — no reachable companion manager is running. Ask the operator to start companion-lobby manager to sign in companion '{}', then connect again.",
                companion.handle
            ),
        };
        let code = if opened.is_some() { "operator_action_needed" } else { "manager_page_unavailable" };
        self.problem_outcome("Connect", code, &message, None)
    }

    /// The (d) step shared by `Connect` and the auto-resume: with a usable binding,
    /// ensure an enrollment with a live session for exactly this origin, then arrive.
    /// Enrollment runs only when no usable enrollment/session exists — a session-less
    /// enrollment is unusable for every operation, so the honest re-enroll path is
    /// forget + bound exchange (the same one the CLI documents). An existing enrollment
    /// (of either tier) with its session intact is used as-is.
    fn connect_enroll(&self, companion: &Companion, canonical: &str, initiator: &str) -> Result<String, String> {
        let mut forget: Option<String> = None;
        let mut ready: Option<String> = None;
        {
            let store = self.lock_store()?;
            if let Some(entry) = store.enrollment_at(&companion.local_id, canonical) {
                if store.has_session(&entry.enrollment_id) {
                    ready = Some(entry.enrollment_id);
                } else {
                    forget = Some(entry.enrollment_id);
                }
            }
        }
        if let Some(broken) = forget {
            let _ = self.forget_enrollment(&broken);
        }
        if let Some(ready) = ready {
            return Ok(ready);
        }
        let entry = self.enroll_bound(&companion.local_id, canonical)?;
        self.events.publish(DomainEvent::ConnectEnrolled {
            origin: canonical.to_string(),
            companion: companion.handle.clone(),
            initiator: initiator.to_string(),
        });
        Ok(entry.enrollment_id)
    }

    /// Enroll-if-needed then arrive, publishing the handshake's live tail events and
    /// answering with `Arrive`'s orientation outcome led by the P2 line — "You are
    /// {handle} — session {contextId}": the session id IS the context handle later
    /// calls carry. Shared by the model-facing connect and the service-side
    /// auto-resume so both narrate identically. A PDS session that died mid-flight
    /// pops the sign-in page again (just-in-time re-bind).
    fn connect_finish(&self, companion: &Companion, canonical: &str, initiator: &str) -> ToolOutcome {
        match self.connect_enroll(companion, canonical, initiator) {
            Ok(enrollment_id) => {
                let mut outcome = self.arrive(&enrollment_id, canonical);
                if outcome.is_error {
                    self.connect_failed(canonical, &companion.handle, &problem_code_of(&outcome), initiator);
                } else {
                    let context_id = outcome
                        .structured
                        .pointer("/connector/contextId")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    outcome.text = format!("You are {} — session {context_id}\n{}", companion.handle, outcome.text);
                    if let Some(connector) = outcome.structured.get_mut("connector").and_then(Value::as_object_mut) {
                        connector.insert("companionHandle".into(), json!(companion.handle));
                    }
                    self.events.publish(DomainEvent::ConnectArrived {
                        origin: canonical.to_string(),
                        companion: companion.handle.clone(),
                        initiator: initiator.to_string(),
                    });
                }
                outcome
            }
            Err(error) if error.starts_with("atproto_session_expired") => {
                self.pop_sign_in(companion, canonical, false, initiator)
            }
            Err(error) => {
                let outcome = self.enrollment_problem("Connect", &error);
                self.connect_failed(canonical, &companion.handle, &problem_code_of(&outcome), initiator);
                outcome
            }
        }
    }

    /// Whether an identical waiting connect is already pending (P5a coalescing), and if
    /// so refreshes it: the looping caller keeps the auto-resume window open while its
    /// repeated Connects stay silent on the feed.
    fn touch_pending_connect(&self, local_id: &str, origin: &str) -> bool {
        self.pending_connects
            .lock()
            .map(|mut pendings| {
                let now = now_millis();
                let mut found = false;
                for entry in pendings.iter_mut() {
                    if entry.local_id == local_id && entry.origin == origin {
                        entry.recorded_at = now;
                        found = true;
                    }
                }
                found
            })
            .unwrap_or(false)
    }

    /// Drops the pending for one (companion, origin): the connect finished model-side,
    /// so a later binding must not resume it a second time.
    fn drop_pending_connect(&self, local_id: &str, origin: &str) {
        if let Ok(mut pendings) = self.pending_connects.lock() {
            pendings.retain(|entry| !(entry.local_id == local_id && entry.origin == origin));
        }
    }

    /// Records one waiting-for-operator connect and arms its honest age-out: if
    /// the operator never completes (or abandons) the sign-in, the pending connect is
    /// dropped after [`PENDING_CONNECT_TIMEOUT_MS`] with a feed event — never silently.
    /// The age-out runs on ONE shared sweeper thread (armed here on the first pending):
    /// a looping model's repeated Connects each refresh the pending but spawn nothing,
    /// and the sweeper touches the pending list only briefly, once per sweep — it can
    /// never contend with the store, the companion manager, or a Connect in flight.
    fn record_pending_connect(&self, companion: &Companion, canonical: &str, initiator: &str) {
        let pending = PendingConnect {
            local_id: companion.local_id.clone(),
            handle: companion.handle.clone(),
            origin: canonical.to_string(),
            initiator: initiator.to_string(),
            recorded_at: now_millis(),
        };
        {
            let mut pendings = match self.pending_connects.lock() {
                Ok(pendings) => pendings,
                Err(_) => return,
            };
            // One pending per (companion, origin): a repeated connect refreshes it.
            pendings.retain(|entry| !(entry.local_id == pending.local_id && entry.origin == pending.origin));
            pendings.push(pending);
        }
        let pendings = self.pending_connects.clone();
        let events = self.events();
        self.sweep_once.call_once(|| {
            let _ = std::thread::Builder::new()
                .name("companion-connect-timeout".into())
                .spawn(move || loop {
                    std::thread::sleep(std::time::Duration::from_millis(PENDING_CONNECT_SWEEP_MS));
                    let expired: Vec<PendingConnect> = pendings
                        .lock()
                        .map(|mut pendings| {
                            let now = now_millis();
                            let mut expired = Vec::new();
                            pendings.retain(|entry| {
                                if now.saturating_sub(entry.recorded_at) >= PENDING_CONNECT_TIMEOUT_MS {
                                    expired.push(entry.clone());
                                    false
                                } else {
                                    true
                                }
                            });
                            expired
                        })
                        .unwrap_or_default();
                    for entry in expired {
                        events.publish(DomainEvent::ConnectFailed {
                            origin: entry.origin,
                            companion: entry.handle,
                            code: "operator_timeout: the pending connect aged out waiting for the operator".into(),
                            initiator: entry.initiator,
                        });
                    }
                });
        });
    }

    /// The auto-resume, armed right after an operator completed a binding: every
    /// fresh pending connect for that companion finishes by itself — no model involved —
    /// through the same enroll-and-arrive steps with the same live progress. The
    /// model's next Connect or Arrive simply finds the enrollment and session ready.
    ///
    /// Lock discipline: the pending list is drained under its own lock and RELEASED
    /// before any hub work; the enroll-and-arrive steps take the store lock only in
    /// their own short scopes, with all network I/O outside it. The resume runs
    /// on the operator connection thread that performed the bind and holds no lock
    /// across its whole journey.
    fn resume_pending_connects(&self, local_id: &str) {
        let resumes: Vec<PendingConnect> = {
            let Ok(mut pendings) = self.pending_connects.lock() else { return };
            let now = now_millis();
            let (due, keep): (Vec<_>, Vec<_>) = pendings
                .drain(..)
                .partition(|entry| entry.local_id == local_id && now.saturating_sub(entry.recorded_at) < PENDING_CONNECT_TIMEOUT_MS);
            pendings.extend(keep);
            due
        };
        for pending in resumes {
            let Some(companion) = self.companion(&pending.local_id) else { continue };
            self.events.publish(DomainEvent::ConnectOperatorCompleted {
                origin: pending.origin.clone(),
                companion: companion.handle.clone(),
                // The resume is always armed by an operator action on the page.
                initiator: "companion manager".to_string(),
            });
            let _ = self.connect_finish(&companion, &pending.origin, "companion manager");
        }
    }

    fn connect_failed(&self, origin: &str, companion: &str, code: &str, initiator: &str) {
        self.events.publish(DomainEvent::ConnectFailed {
            origin: origin.to_string(),
            companion: companion.to_string(),
            code: code.to_string(),
            initiator: initiator.to_string(),
        });
    }

    /// Maps an enrollment-engine `Err(String)` (its own "code: message" discipline)
    /// onto the honest tool problem outcome — the same split the operator API applies.
    fn enrollment_problem(&self, tool: &str, error: &str) -> ToolOutcome {
        let (code, message) = match error.split_once(": ") {
            Some((code, message)) => (code.to_string(), message.to_string()),
            None => ("blocked".to_string(), error.to_string()),
        };
        self.problem_outcome(tool, &code, &message, None)
    }

    // ---------- companions (operator surface) ----------

    pub fn create_companion(&self, handle: &str, display_name: Option<&str>) -> Result<companion_core::domain::companion::Companion, String> {
        self.attributed("manager.create_companion", || {
            if !valid_handle(handle) {
                return Err("a handle is 2-253 characters without whitespace or ':'".to_string());
            }
            let companion = companion_core::domain::companion::Companion {
                local_id: crate::adapters::store::new_local_id(),
                handle: handle.to_string(),
                display_name: display_name.map(str::to_string),
                bound_did: None,
                created_at: now_millis(),
            };
            let mut store = self.lock_store()?;
            store.upsert_companion(companion.clone())?;
            store.save()?;
            drop(store);
            self.publish_node(&companion.local_id);
            Ok(companion)
        })
    }

    /// Updates handle and/or display name. `display_name`: `None` leaves it unchanged,
    /// `Some(None)` clears it, `Some(Some(v))` sets it. The local id never changes.
    pub fn update_companion(
        &self,
        local_id: &str,
        handle: Option<&str>,
        display_name: Option<Option<&str>>,
    ) -> Result<companion_core::domain::companion::Companion, String> {
        self.attributed("manager.update_companion", || {
            let mut store = self.lock_store()?;
            let mut companion = store.companion(local_id).ok_or_else(|| "no companion matches that id".to_string())?;
            if let Some(handle) = handle {
                if !valid_handle(handle) {
                    return Err("a handle is 2-253 characters without whitespace or ':'".to_string());
                }
                companion.handle = handle.to_string();
            }
            if let Some(display) = display_name {
                companion.display_name = display.map(str::to_string);
            }
            store.upsert_companion(companion.clone())?;
            store.save()?;
            drop(store);
            self.publish_node(local_id);
            Ok(companion)
        })
    }

    /// Deletes an companion. Refuses while enrollments exist unless `cascade` forgets them
    /// (and their sessions) first.
    pub fn delete_companion(&self, local_id: &str, cascade: bool) -> Result<(), String> {
        self.attributed("manager.delete_companion", || {
            let mut store = self.lock_store()?;
            let companion = store.companion(local_id).ok_or_else(|| "no companion matches that id".to_string())?;
            let enrollments = store.enrollments_of(local_id);
            if !enrollments.is_empty() && !cascade {
                return Err(format!(
                    "companion '{}' still holds {} enrollment(s); forget them first, or confirm a cascade delete",
                    companion.handle,
                    enrollments.len()
                ));
            }
            for entry in &enrollments {
                store.remove_enrollment(&entry.enrollment_id);
            }
            store.remove_companion(local_id);
            store.save()?;
            drop(store);
            self.events.publish(DomainEvent::CompanionRemoved { local_id: local_id.to_string() });
            Ok(())
        })
    }

    pub fn companions(&self) -> Vec<companion_core::domain::companion::Companion> {
        let store = self.lock_store().expect("state lock");
        store.companions().to_vec()
    }

    pub fn companion(&self, local_id: &str) -> Option<companion_core::domain::companion::Companion> {
        let store = self.lock_store().expect("state lock");
        store.companion(local_id)
    }

    pub fn enrollments_of(&self, local_id: &str) -> Vec<Enrollment> {
        let store = self.lock_store().expect("state lock");
        store.enrollments_of(local_id)
    }

    /// Every enrollment, oldest first, with its session availability (never the token).
    pub fn enrollment_inventory(&self) -> Vec<(Enrollment, bool)> {
        let store = self.lock_store().expect("state lock");
        store.enrollments().iter().map(|entry| (entry.clone(), store.has_session(&entry.enrollment_id))).collect()
    }

    /// Read-only attention state per enrollment, for the companion manager.
    pub fn enrollment_statuses(&self) -> Vec<EnrollmentStatus> {
        let store = self.lock_store().expect("state lock");
        store
            .enrollments()
            .iter()
            .map(|entry| EnrollmentStatus {
                enrollment_id: entry.enrollment_id.clone(),
                companion_local_id: entry.local_id.clone(),
                origin: entry.origin.clone(),
                waiting: store.waiting_count(&entry.enrollment_id),
                pending_attention: store
                    .attention_records(&entry.enrollment_id)
                    .iter()
                    .filter(|record| record.state != AttentionState::Delivered)
                    .count(),
            })
            .collect()
    }

    /// Immediate collection snapshot, grouped by enrolled origin. Missing or offline
    /// servers still have an honest address card; no network runs while rendering it.
    pub fn server_cards(&self) -> Vec<ServerCard> {
        let store = self.lock_store().expect("state lock");
        let mut origins: Vec<_> = store.enrollments().iter().map(|entry| entry.origin.clone()).collect();
        origins.sort();
        origins.dedup();
        origins.into_iter().map(|origin| {
            store.server_card(&origin).unwrap_or_else(|| ServerCard { origin, ..Default::default() })
        }).collect()
    }

    /// Optional operator refresh, independent of the page's core inventory request.
    /// Four concurrent requests at most; ordinary checks and arrivals share its cache.
    pub fn refresh_server_cards(&self) -> Vec<ServerCard> {
        let now = now_millis();
        let cards: Vec<_> = self.server_cards().into_iter().filter(|card| {
            now.saturating_sub(card.refreshed_at) >= 5 * 60 * 1000
                && !self.server_card_attempts.lock().expect("card refresh lock")
                    .get(&card.origin).is_some_and(|at| now.saturating_sub(*at) < 5 * 60 * 1000)
        }).take(8).collect();
        for group in cards.chunks(4) {
            std::thread::scope(|scope| {
                for card in group {
                    scope.spawn(move || self.refresh_server_card(&card.origin));
                }
            });
        }
        self.server_cards()
    }

    fn refresh_server_card(&self, origin: &str) {
        const FRESH_MS: i64 = 5 * 60 * 1000;
        let now = now_millis();
        {
            let store = self.lock_store().expect("state lock");
            if !store.enrollments().iter().any(|entry| entry.origin == origin)
                || store.server_card(origin).is_some_and(|card| now.saturating_sub(card.refreshed_at) < FRESH_MS) {
                return;
            }
        }
        {
            let mut attempts = self.server_card_attempts.lock().expect("card refresh lock");
            if attempts.get(origin).is_some_and(|at| now.saturating_sub(*at) < FRESH_MS) {
                return;
            }
            attempts.insert(origin.to_string(), now);
        }
        let Ok(raw) = self.service().server_card(origin) else { return };
        let Some(card) = project_server_card(origin, &raw, now) else { return };
        let mut store = self.lock_store().expect("state lock");
        // A forgotten enrollment must not be resurrected by an in-flight request.
        if store.enrollments().iter().any(|entry| entry.origin == origin) {
            store.set_server_card(card.clone());
            let _ = store.save();
            drop(store);
            self.events.publish(DomainEvent::ServerCardChanged {
                origin: origin.to_string(),
                card: serde_json::to_value(card).unwrap_or_default(),
            });
        }
    }

    /// Attribution wrapper for operator-page mutations: the same invoked/completed pair
    /// every intake records, with the `Operator` channel.
    fn attributed<T>(&self, action: &str, run: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        self.events.publish(DomainEvent::ToolInvoked { channel: IntakeChannel::Manager, tool: action.to_string() });
        let result = run();
        let status = if result.is_ok() { "ok" } else { "error" };
        self.events.publish(DomainEvent::ToolCompleted {
            channel: IntakeChannel::Manager,
            tool: action.to_string(),
            status: status.into(),
            text_bytes: 0,
        });
        result
    }

    /// Direct store access for intakes rendering their own views (the CLI, the tray).
    ///
    /// LOCK RULE — the store mutex is a leaf lock. While holding it, only pure
    /// `StateStore` reads and writes are allowed: never a hub method that takes the
    /// store again (`std::sync::Mutex` is not re-entrant — the second `lock()` on the
    /// same thread blocks forever while still holding the mutex, freezing every other
    /// intake), and never network I/O: the exchange runs outside the lock and the state is re-checked after.
    /// Intakes that need composed facts should ask the hub for a batched read (see
    /// [`ConnectorHub::companion_inventory`]) instead of walking the store themselves.
    pub fn store(&self) -> &Mutex<StateStore> {
        &self.store
    }

    pub fn caller(&self) -> &CallerId {
        &self.caller
    }

    /// The shared entry point for every intake. Decodes and executes one tool invocation.
    pub fn invoke(&self, channel: IntakeChannel, tool: &str, arguments: &Value) -> ToolOutcome {
        let operation = match decode(tool, arguments) {
            Ok(operation) => operation,
            Err(error) => {
                self.events.publish(DomainEvent::ToolInvoked { channel, tool: tool.to_string() });
                self.events.publish(DomainEvent::ToolCompleted {
                    channel,
                    tool: tool.to_string(),
                    status: "error".into(),
                    text_bytes: 0,
                });
                return self.problem_outcome(tool, "invalid_arguments", &error, None);
            }
        };
        self.execute(channel, operation)
    }

    pub fn execute(&self, channel: IntakeChannel, operation: Operation) -> ToolOutcome {
        let tool = operation.tool_name();
        self.events.publish(DomainEvent::ToolInvoked { channel, tool: tool.to_string() });
        let initiator = self.initiator_label(channel);
        let outcome = self.dispatch(operation, &initiator);
        self.events.publish(DomainEvent::ToolCompleted {
            channel,
            tool: tool.to_string(),
            status: outcome.status.clone(),
            text_bytes: outcome.text.len(),
        });
        outcome
    }

    /// The feed's initiator label (P5b): who started this call. MCP tool calls are the
    /// model acting through a named client; the command line and the companion manager are
    /// the operator.
    fn initiator_label(&self, channel: IntakeChannel) -> String {
        match channel {
            IntakeChannel::Mcp => format!("model (via {})", self.caller.mcp_client_name().unwrap_or("stdio")),
            IntakeChannel::Cli => "operator (CLI)".to_string(),
            IntakeChannel::Manager => "companion manager".to_string(),
        }
    }

    fn dispatch(&self, operation: Operation, initiator: &str) -> ToolOutcome {
        match operation {
            Operation::ListCompanions => self.list_companions_view(),
            Operation::Connect { service, persona, address } => self.connect(&service, &persona, address.as_deref(), initiator),
            Operation::WhoAmI { session } => self.who_am_i(&session),
            Operation::CatchUp { session, view, cursor } => self.catch_up(&session, view, cursor),
            Operation::ForumListSpaces { session, cursor } => {
                self.with_context("Forum_List_Spaces", &session, ViewMode::Compact, |frame| {
                    let path = match &cursor {
                        Some(value) => format!("/api/v1/experience/tangents?cursor={}", encode(value)),
                        None => "/api/v1/experience/tangents".to_string(),
                    };
                    let service = self.service();
                    let browsing = service.as_social_browsing().expect("the tangent service browses");
                    browsing.get(&frame.request, &path)
                })
            }
            Operation::ForumListThreads { session, space_ref, cursor } => {
                self.with_context("Forum_List_Threads", &session, ViewMode::Compact, |frame| {
                    let Some(space) = refs::tangent_key(&frame.request.origin, &space_ref) else {
                        return Err(invalid_ref("Space"));
                    };
                    let path = match &cursor {
                        Some(value) => format!("/api/v1/experience/tangents/{space}/topics?cursor={}", encode(value)),
                        None => format!("/api/v1/experience/tangents/{space}/topics"),
                    };
                    let service = self.service();
                    let browsing = service.as_social_browsing().expect("the tangent service browses");
                    browsing.get(&frame.request, &path)
                })
            }
            Operation::ForumReadThread { session, thread_ref, cursor, around_post_ref, view, limit } => {
                self.with_context("Forum_Read_Thread", &session, view, |frame| {
                    let Some((_space, thread)) = refs::topic_keys(&frame.request.origin, &thread_ref) else {
                        return Err(invalid_ref("Thread"));
                    };
                    let mut path = format!("/api/v1/experience/topics/{thread}");
                    let mut query = Vec::new();
                    if let Some(value) = &cursor {
                        query.push(format!("cursor={}", encode(value)));
                    }
                    if let Some(value) = &around_post_ref {
                        query.push(format!("aroundPostRef={}", encode(value)));
                    }
                    if let Some(value) = limit {
                        query.push(format!("limit={value}"));
                    }
                    if !query.is_empty() {
                        path.push('?');
                        path.push_str(&query.join("&"));
                    }
                    let service = self.service();
                    let browsing = service.as_social_browsing().expect("the tangent service browses");
                    browsing.get(&frame.request, &path)
                })
            }
            Operation::ForumStartThread { session, space_ref, title, text, request_id, view } => {
                self.with_context("Forum_Start_Thread", &session, view, |frame| {
                    let Some(space) = refs::tangent_key(&frame.request.origin, &space_ref) else {
                        return Err(invalid_ref("Space"));
                    };
                    let body = json!({ "requestId": request_id, "title": title, "text": text });
                    self.send_route(frame, Route::TopicStarts(space.to_string()), &body)
                })
            }
            Operation::ForumPost { session, thread_ref, request_id, text, reply_to, view } => {
                self.with_context("Forum_Post", &session, view, |frame| {
                    let Some((_space, thread)) = refs::topic_keys(&frame.request.origin, &thread_ref) else {
                        return Err(invalid_ref("Thread"));
                    };
                    let mut body = json!({ "requestId": request_id, "text": text });
                    if let Some(reference) = &reply_to {
                        body["replyTo"] = json!(reference);
                    }
                    self.send_route(frame, Route::TopicPosts(thread.to_string()), &body)
                })
            }
            Operation::ForumEditPost { session, post_ref, text, view } => {
                self.with_context("Forum_Edit_Post", &session, view, |frame| {
                    let Some((_space, _thread, post)) = refs::post_keys(&frame.request.origin, &post_ref) else {
                        return Err(invalid_ref("Post"));
                    };
                    let body = json!({ "text": text });
                    self.send_route(frame, Route::PostEdits(post.to_string()), &body)
                })
            }
            Operation::ForumDeletePost { session, post_ref, view } => {
                self.with_context("Forum_Delete_Post", &session, view, |frame| {
                    let Some((_space, _thread, post)) = refs::post_keys(&frame.request.origin, &post_ref) else {
                        return Err(invalid_ref("Post"));
                    };
                    self.send_route(frame, Route::PostDeletes(post.to_string()), &Value::Null)
                })
            }
            Operation::ForumJoinSpace { session, space_ref, request_id, invite_ref, view } => {
                self.with_context("Forum_Join_Space", &session, view, |frame| {
                    let Some(space) = refs::tangent_key(&frame.request.origin, &space_ref) else {
                        return Err(invalid_ref("Space"));
                    };
                    let mut body = json!({ "requestId": request_id });
                    if let Some(reference) = &invite_ref {
                        body["inviteRef"] = json!(reference);
                    }
                    self.send_route(frame, Route::Membership(space.to_string()), &body)
                })
            }
            Operation::ForumLeaveSpace { session, space_ref, request_id, view } => {
                self.with_context("Forum_Leave_Space", &session, view, |frame| {
                    let Some(space) = refs::tangent_key(&frame.request.origin, &space_ref) else {
                        return Err(invalid_ref("Space"));
                    };
                    self.send_route(frame, Route::Leave(space.to_string(), request_id.clone()), &Value::Null)
                })
            }
            Operation::ForumMarkRead { session, thread_ref, read_cursor, request_id, view } => {
                self.with_context("Forum_Mark_Read", &session, view, |frame| {
                    let Some((_space, thread)) = refs::topic_keys(&frame.request.origin, &thread_ref) else {
                        return Err(invalid_ref("Thread"));
                    };
                    let request_id = request_id.unwrap_or_else(|| format!("mark-{}", crate::adapters::store::short_uuid()));
                    let body = json!({ "requestId": request_id, "readCursor": read_cursor });
                    self.send_route(frame, Route::TopicReadPosition(thread.to_string()), &body)
                })
            }
            Operation::ForumWatch { session, scope_ref, on, view } => {
                self.with_context("Forum_Watch", &session, view, |frame| {
                    if refs::tangent_key(&frame.request.origin, &scope_ref).is_none()
                        && refs::topic_keys(&frame.request.origin, &scope_ref).is_none()
                    {
                        return Err(invalid_ref("Thread or Space"));
                    }
                    let request_id = format!("watch-{}", crate::adapters::store::short_uuid());
                    let body = json!({ "requestId": request_id, "scopeRef": scope_ref, "mode": if on { "on" } else { "off" } });
                    self.send_route(frame, Route::Watches, &body)
                })
            }
            Operation::ForumReadUser { session, user_ref } => {
                self.with_context("Forum_Read_User", &session, ViewMode::Compact, |frame| {
                    let service = self.service();
                    let browsing = service.as_social_browsing().expect("the tangent service browses");
                    browsing.get(&frame.request, &format!("/api/v1/experience/participants/{}", encode(&user_ref)))
                })
            }
            Operation::ForumOpenCase { session, thread_ref, subject_ref, reason, view } => {
                self.with_context("Forum_Open_Case", &session, view, |frame| {
                    let Some((_space, thread)) = refs::topic_keys(&frame.request.origin, &thread_ref) else {
                        return Err(invalid_ref("Thread"));
                    };
                    let request_id = format!("case-{}", crate::adapters::store::short_uuid());
                    let body = json!({ "requestId": request_id, "subjectRef": subject_ref, "reason": reason });
                    self.send_route(frame, Route::TopicReports(thread.to_string()), &body)
                })
            }
            Operation::ForumListCases { session, thread_ref, page, view } => {
                self.with_context("Forum_List_Cases", &session, view, |frame| {
                    let Some((_space, thread)) = refs::topic_keys(&frame.request.origin, &thread_ref) else {
                        return Err(invalid_ref("Thread"));
                    };
                    let service = self.service();
                    let moderation = service.as_social_moderation().expect("the tangent service moderates");
                    moderation.list_moderation_cases(&frame.request, thread, page)
                })
            }
            Operation::ForumReadCase { session, case_ref, view } => {
                self.with_context("Forum_Read_Case", &session, view, |frame| {
                    let Some((_space, _thread, case_id)) = refs::case_keys(&frame.request.origin, &case_ref) else {
                        return Err(invalid_ref("case"));
                    };
                    let service = self.service();
                    let moderation = service.as_social_moderation().expect("the tangent service moderates");
                    moderation.read_moderation_case(&frame.request, case_id)
                })
            }
            Operation::ForumPreviewAction { session, case_ref, action, summary, deferred_until,
                expected_case_revision, expected_subject_revision, view } => {
                self.with_context("Forum_Preview_Action", &session, view, |frame| {
                    let Some((_space, _thread, case_id)) = refs::case_keys(&frame.request.origin, &case_ref) else {
                        return Err(invalid_ref("case"));
                    };
                    let mut body = json!({ "action": action.as_str(), "summary": summary,
                        "expectedCaseRevision": expected_case_revision, "expectedSubjectRevision": expected_subject_revision });
                    if let Some(value) = &deferred_until { body["deferredUntil"] = json!(value); }
                    let service = self.service();
                    let moderation = service.as_social_moderation().expect("the tangent service moderates");
                    moderation.preview_moderation_action(&frame.request, case_id, &body)
                })
            }
            Operation::ForumEscalateCase { session, case_ref, request_id, summary, deferred_until,
                expected_case_revision, expected_subject_revision, view } => {
                self.with_context("Forum_Escalate_Case", &session, view, |frame| {
                    let Some((_space, _thread, case_id)) = refs::case_keys(&frame.request.origin, &case_ref) else {
                        return Err(invalid_ref("case"));
                    };
                    let mut body = json!({ "requestId": request_id, "action": "escalate", "summary": summary,
                        "expectedCaseRevision": expected_case_revision, "expectedSubjectRevision": expected_subject_revision });
                    if let Some(value) = &deferred_until { body["deferredUntil"] = json!(value); }
                    self.send_route(frame, Route::ModerationAction(case_id.to_string()), &body)
                })
            }
            Operation::ForumManageUser { session, user_ref, action, reason, duration_seconds, role, case_ref, view } => {
                self.with_context("Forum_Manage_User", &session, view, |frame| {
                    let mut body = json!({ "action": action.as_str(), "reason": reason });
                    if let Some(seconds) = duration_seconds { body["durationSeconds"] = json!(seconds); }
                    if let Some(role) = &role { body["role"] = json!(role); }
                    if let Some(case_ref) = &case_ref { body["caseRef"] = json!(case_ref); }
                    self.send_route(frame, Route::UserManagement(user_ref.clone()), &body)
                })
            }
        }
    }

    /// Arrival is internal now — Connect is the only front door — but the flow is
    /// unchanged: fetch the envelope, bind the labeled session, brief.
    fn arrive(&self, enrollment_id: &str, server_url: &str) -> ToolOutcome {
        let (companion, canonical_check) = {
            let store = self.lock_store().expect("state lock");
            (store.enrollment(enrollment_id), refs::acceptable_origin(server_url))
        };
        let Some(companion) = companion else {
            return self.problem_outcome("Arrive", "companion_unavailable",
                "That companion is not enrolled in this connector.", None);
        };
        // The destination must be the companion's enrolled canonical origin; a different
        // URL never silently rebinds the session.
        if canonical_check.as_deref() != Some(companion.origin.as_str()) {
            return self.problem_outcome("Arrive", "unreachable",
                &format!("That destination does not match this companion's server ({}).", companion.origin),
                Some((&companion, None)));
        }
        let session = match self.session_of(&companion) {
            Ok(session) => session,
            Err(error) => return self.problem_outcome("Arrive", "needs_manager_connection", &error, Some((&companion, None))),
        };
        let request = RequestContext { origin: companion.origin.clone(), credential: session, participant_ref: companion.participant_ref.clone(), dpop: None };
        let outcome = {
            let service = self.service();
            let browsing = service.as_social_browsing().expect("the tangent service browses");
            browsing.get(&request, "/api/v1/experience")
        };
        let raw = match outcome {
            Ok(raw) => raw,
            Err(error) => return self.transport_problem("Arrive", &companion, None, &error),
        };
        let parsed = match contract::parse(&raw) {
            Ok(parsed) => parsed,
            Err(error) => return self.problem_outcome("Arrive", "unreachable", &error, Some((&companion, None))),
        };
        let bound = {
            let mut store = self.lock_store().expect("state lock");
            let label = self.service().name().to_string();
            let bound = store.bind_context(&self.caller, &companion, now_millis(), &label);
            self.sync_attention(&mut store, &companion.enrollment_id, &parsed);
            let _ = store.save();
            bound
        };
        self.events.publish(DomainEvent::ContextArrived { context_id: bound.context_id.clone(), origin: companion.origin.clone() });
        self.refresh_server_card(&companion.origin);
        self.finish("Connect", &companion, Some(&bound), ViewMode::Orientation, raw, parsed, false)
    }

    /// One inbox: mentions, watched activity, and settlements, cursor'd from the
    /// last checkpoint. The session token is the labeled context id.
    fn catch_up(&self, session: &str, view: ViewMode, cursor: Option<String>) -> ToolOutcome {
        let binding = {
            let store = self.lock_store().expect("state lock");
            store.context(session)
        };
        let Some(context_binding) = binding else {
            return self.context_expired("CatchUp");
        };
        let Some(companion) = self.companion_of(&context_binding.enrollment_id) else {
            return self.context_expired("CatchUp");
        };
        let session_token = match self.session_of(&companion) {
            Ok(session_token) => session_token,
            Err(error) => return self.problem_outcome("CatchUp", "needs_manager_connection", &error, Some((&companion, Some(&context_binding)))),
        };
        let request = RequestContext { origin: companion.origin.clone(), credential: session_token, participant_ref: companion.participant_ref.clone(), dpop: None };
        let mut query = Vec::new();
        match &cursor {
            // A supplied cursor continues the previous page sequence.
            Some(value) => query.push(format!("pageCursor={}", encode(value))),
            None => {
                let store = self.lock_store().expect("state lock");
                if let Some(checkpoint) = store.checkpoint(&companion.enrollment_id) {
                    query.push(format!("checkpoint={}", encode(&checkpoint)));
                }
            }
        }
        let path = if query.is_empty() {
            "/api/v1/experience/updates".to_string()
        } else {
            format!("/api/v1/experience/updates?{}", query.join("&"))
        };
        let outcome = {
            let service = self.service();
            let browsing = service.as_social_browsing().expect("the tangent service browses");
            browsing.get(&request, &path)
        };
        let raw = match outcome {
            Ok(raw) => raw,
            Err(error) => return self.transport_problem("CatchUp", &companion, Some(&context_binding), &error),
        };
        let parsed = match contract::parse(&raw) {
            Ok(parsed) => parsed,
            Err(error) => return self.problem_outcome("CatchUp", "unreachable", &error, Some((&companion, Some(&context_binding)))),
        };
        let unchanged = {
            let mut store = self.lock_store().expect("state lock");
            let previous = store.revision(&companion.enrollment_id);
            let unchanged = previous.as_deref() == Some(parsed.attention.revision.as_str());
            self.sync_attention(&mut store, &companion.enrollment_id, &parsed);
            // The recovery checkpoint always names the page start, not a page continuation.
            if parsed.continuation.activity_checkpoint.is_some() && cursor.is_none() {
                if let Some(checkpoint) = &parsed.continuation.activity_checkpoint {
                    store.set_checkpoint(&companion.enrollment_id, checkpoint);
                }
            }
            let _ = store.save();
            unchanged
        };
        self.finish("CatchUp", &companion, Some(&context_binding), view, raw, parsed, unchanged)
    }

    /// Shared path for every context-scoped operation: resolve and validate the binding,
    /// run the use case, synchronize attention, then render with the delivery segment.
    fn with_context(
        &self,
        tool: &str,
        context_id: &str,
        view: ViewMode,
        run: impl FnOnce(&CallFrame) -> Result<Value, ExperienceError>,
    ) -> ToolOutcome {
        let binding = {
            let store = self.lock_store().expect("state lock");
            store.context(context_id)
        };
        let Some(context_binding) = binding else {
            return self.context_expired(tool);
        };
        let Some(companion) = self.companion_of(&context_binding.enrollment_id) else {
            return self.context_expired(tool);
        };
        if !context_binding.belongs_to(&self.caller, &companion.enrollment_id) || context_binding.origin != companion.origin {
            return self.context_expired(tool);
        }
        let session = match self.session_of(&companion) {
            Ok(session) => session,
            Err(error) => return self.problem_outcome(tool, "needs_manager_connection", &error, Some((&companion, Some(&context_binding)))),
        };
        let frame = CallFrame {
            request: RequestContext { origin: companion.origin.clone(), credential: session, participant_ref: companion.participant_ref.clone(), dpop: None },
            companion: companion.clone(),
            context_id: context_binding.context_id.clone(),
        };
        let raw = match run(&frame) {
            Ok(raw) => raw,
            Err(error) => {
                if matches!(&error, ExperienceError::Application { code, .. } if code == "permission_denied") {
                    self.invalidate_optional_tools(&context_binding.context_id);
                }
                return self.transport_problem(tool, &companion, Some(&context_binding), &error);
            }
        };
        let parsed = match contract::parse(&raw) {
            Ok(parsed) => parsed,
            Err(error) => return self.problem_outcome(tool, "unreachable", &error, Some((&companion, Some(&context_binding)))),
        };
        let unchanged = {
            let mut store = self.lock_store().expect("state lock");
            let previous = store.revision(&companion.enrollment_id);
            let unchanged = previous.as_deref() == Some(parsed.attention.revision.as_str());
            self.sync_attention(&mut store, &companion.enrollment_id, &parsed);
            let _ = store.save();
            unchanged
        };
        self.finish(tool, &companion, Some(&context_binding), view, raw, parsed, unchanged)
    }

    /// Mutation path: the full tuple is journaled before the request leaves, and settled from
    /// the returned receipt. A lost response keeps the entry unsettled for reconciliation.
    /// One mutation, straight to the service: the response is the receipt, a
    /// request-id conflict is the honest "already done", and guarantees belong to
    /// the service. The connector routes intent and reports what came back.
    fn send_route(&self, frame: &CallFrame, route: Route, body: &Value) -> Result<Value, ExperienceError> {
        let (method, path) = route.build();
        let service = self.service();
        let browsing = service.as_social_browsing().expect("the tangent service browses");
        browsing.send(&frame.request, method, &path, body)
    }

    fn finish(
        &self,
        tool: &str,
        companion: &Enrollment,
        binding: Option<&Context>,
        view: ViewMode,
        raw: Value,
        parsed: ExperienceDto,
        unchanged: bool,
    ) -> ToolOutcome {
        if let Some(binding) = binding { self.observe_optional_tools(&binding.context_id, &parsed); }
        let (text, delivered_ids) = {
            let mut store = self.lock_store().expect("state lock");
            let context_id = binding.map(|binding| binding.context_id.clone()).unwrap_or_default();
            let perspective = Perspective {
                participant_ref: companion.participant_ref.clone(),
                did: companion.did.clone(),
                display: companion.display_name.clone().unwrap_or_else(|| companion.name.clone()),
            };
            let mut canonical_refs = Vec::new();
            collect_refs(&parsed, &mut canonical_refs);
            let pending = store.attention_records(&companion.enrollment_id);
            for record in &pending {
                canonical_refs.push(record.source_ref.clone());
            }
            let mut alias_map: BTreeMap<String, String> = BTreeMap::new();
            for reference in canonical_refs {
                if !reference.is_empty() && !alias_map.contains_key(&reference) {
                    let alias = store.alias_for(&context_id, &reference);
                    alias_map.insert(reference, alias);
                }
            }
            let lookup = |reference: &str| alias_map.get(reference).cloned().unwrap_or_else(|| truncate_reference(reference));
            let pending_undelivered: Vec<_> = pending
                .iter()
                .filter(|record| record.state != AttentionState::Delivered)
                .cloned()
                .collect();
            let input = RenderInput {
                experience: Some(&parsed),
                mode: view,
                perspective: &perspective,
                aliases: &lookup,
                pending: &pending_undelivered,
                waiting_known: Some(store.waiting_count(&companion.enrollment_id)),
                unchanged,
            };
            let text = render(&input);
            // Delivery for tool-response-only hosts happens now: these previews are in the
            // response. Persist the delivery before returning.
            let delivered: Vec<String> = pending_undelivered.iter().map(|record| record.id.clone()).take(5).collect();
            store.mark_attention_delivered(&companion.enrollment_id, &delivered);
            let _ = store.save();
            (text, delivered)
        };
        if !delivered_ids.is_empty() {
            self.events.publish(DomainEvent::AttentionDelivered {
                enrollment_id: companion.enrollment_id.clone(),
                item_count: delivered_ids.len(),
                delivery_mode: DELIVERY_MODE.into(),
            });
        }
        let context_id = binding.map(|binding| binding.context_id.clone());
        let aliases_exposed: BTreeMap<String, String> = {
            let store = self.lock_store().expect("state lock");
            context_id
                .as_deref()
                .map(|context| store.aliases(context))
                .unwrap_or_default()
                .into_iter()
                .map(|(canonical, alias)| (alias, canonical))
                .collect()
        };
        let is_error = matches!(parsed.status.as_str(), "blocked" | "error");
        let _ = tool;
        ToolOutcome {
            is_error,
            status: parsed.status.clone(),
            text,
            structured: json!({
                "experience": raw,
                "problem": null,
                "connector": {
                    "enrollmentId": companion.enrollment_id,
                    "contextId": context_id,
                    "view": view.as_str(),
                    "deliveryMode": DELIVERY_MODE,
                    "aliases": aliases_exposed,
                }
            }),
        }
    }

    // ---------- background checks ----------

    /// One ordinary background check: fetch the digest, persist state, publish events. Runs no
    /// model, invokes no host; delivery remains a separate policy decision.
    pub fn background_check(&self, enrollment_id: &str) -> Result<String, String> {
        let Some(companion) = self.companion_of(enrollment_id) else {
            return Err("unknown companion".to_string());
        };
        let session = self.session_of(&companion)?;
        let request = RequestContext { origin: companion.origin.clone(), credential: session, participant_ref: companion.participant_ref.clone(), dpop: None };
        let checkpoint = {
            let store = self.lock_store().map_err(|_| "state lock poisoned")?;
            store.checkpoint(&companion.enrollment_id)
        };
        let path = match checkpoint {
            Some(value) => format!("/api/v1/experience/updates?checkpoint={}", encode(&value)),
            None => "/api/v1/experience/updates".to_string(),
        };
        let raw = {
            let service = self.service();
            let browsing = service.as_social_browsing().expect("the tangent service browses");
            browsing
                .get(&request, &path)
                .map_err(|error| format!("check failed: {error}"))?
        };
        let parsed = contract::parse(&raw).map_err(|error| error.to_string())?;
        let summary = {
            let mut store = self.lock_store().map_err(|_| "state lock poisoned")?;
            self.sync_attention(&mut store, &companion.enrollment_id, &parsed);
            if let Some(checkpoint) = &parsed.continuation.activity_checkpoint {
                store.set_checkpoint(&companion.enrollment_id, checkpoint);
            }
            store.save()?;
            format!(
                "revision {} · waiting {} · activity {}",
                parsed.attention.revision,
                parsed.attention.waiting_count.describe(),
                parsed.attention.new_activity_count.describe()
            )
        };
        self.events.publish(DomainEvent::PollCompleted {
            enrollment_id: companion.enrollment_id.clone(),
            revision: parsed.attention.revision.clone(),
            waiting: parsed.attention.waiting_count.value.unwrap_or(0),
            activity: parsed.attention.new_activity_count.value.unwrap_or(0),
        });
        self.refresh_server_card(&companion.origin);
        Ok(summary)
    }

    // ---------- attention synchronization ----------

    fn sync_attention(&self, store: &mut StateStore, enrollment_id: &str, parsed: &ExperienceDto) {
        let now = now_millis();
        let (fresh, coalesced) = store.upsert_attention(enrollment_id, &parsed.attention.items, &parsed.attention.revision, now);
        if let Some(waiting) = parsed.attention.waiting_count.value {
            store.set_waiting_count(enrollment_id, waiting);
        }
        let complete = !parsed.attention.more
            && parsed
                .attention
                .waiting_count
                .value
                .map(|waiting| {
                    waiting
                        == parsed
                            .attention
                            .items
                            .iter()
                            .filter(|item| {
                                matches!(item.relationship.as_deref(), Some("addressed_to_you") | Some("replies_to_you"))
                            })
                            .count() as i64
                })
                .unwrap_or(false);
        store.resync_attention(enrollment_id, &parsed.attention.items, parsed.attention.waiting_count.value, complete);
        store.set_revision(enrollment_id, &parsed.attention.revision);
        if !fresh.is_empty() {
            self.events.publish(DomainEvent::AttentionObserved {
                enrollment_id: enrollment_id.to_string(),
                new_items: fresh,
                coalesced,
            });
        }
    }

    // ---------- helpers ----------

    fn companion_of(&self, enrollment_id: &str) -> Option<Enrollment> {
        let store = self.lock_store().ok()?;
        store.enrollment(enrollment_id)
    }

    /// The enrollment's bearer session, from the per-enrollment session map. A missing
    /// session is an honest 're-enroll' state: the session map can lag a hand-edited
    /// state file.
    fn session_of(&self, companion: &Enrollment) -> Result<String, String> {
        let store = self.lock_store()?;
        store.session(&companion.enrollment_id).ok_or_else(|| "the stored session is missing; re-enroll this enrollment".to_string())
    }

    /// Takes the store lock. Leaf-lock discipline applies for the whole guard scope:
    /// no hub method that locks the store again (the mutex is not re-entrant), no
    /// network I/O, no process spawn. Composed reads belong in batched hub methods.
    fn lock_store(&self) -> Result<std::sync::MutexGuard<'_, StateStore>, String> {
        self.store.lock().map_err(|_| "state lock poisoned".to_string())
    }

    fn context_expired(&self, tool: &str) -> ToolOutcome {
        self.problem_outcome(tool, "context_expired",
            "That context is unknown to this connector. Select your companion and Arrive again; keep any saved request ids.", None)
    }

    fn transport_problem(
        &self,
        tool: &str,
        companion: &Enrollment,
        binding: Option<&Context>,
        error: &ExperienceError,
    ) -> ToolOutcome {
        let (code, message) = match error {
            ExperienceError::Unreachable => ("unreachable".to_string(), "The Tangent server could not be reached. Saved actions and cursors remain available.".to_string()),
            ExperienceError::Unauthorized | ExperienceError::DpopChallenge { .. } => ("needs_manager_connection".to_string(), "Authentication was rejected; the operator must renew this enrollment's session.".to_string()),
            ExperienceError::Application { code, message } => (code.clone(), message.clone()),
            ExperienceError::Transport(detail) => ("unreachable".to_string(), detail.clone()),
        };
        self.problem_outcome(tool, &code, &message, Some((companion, binding)))
    }

    fn problem_outcome(
        &self,
        tool: &str,
        code: &str,
        message: &str,
        companion: Option<(&Enrollment, Option<&Context>)>,
    ) -> ToolOutcome {
        let mut text = String::new();
        if let Some((companion, _)) = &companion {
            let name = companion.display_name.clone().unwrap_or_else(|| companion.name.clone());
            text.push_str(&format!("You: {name}\n"));
        }
        text.push_str(&format!("Blocked [{code}]: {message}"));
        let (enrollment_id, context_id) = companion
            .map(|(entry, binding)| {
                (
                    Some(entry.enrollment_id.clone()),
                    binding.map(|binding| binding.context_id.clone()),
                )
            })
            .unwrap_or((None, None));
        ToolOutcome {
            is_error: true,
            status: "blocked".into(),
            text,
            structured: json!({
                "experience": null,
                "problem": { "code": code, "message": message, "operation": tool },
                "connector": {
                    "enrollmentId": enrollment_id,
                    "contextId": context_id,
                    "view": "compact",
                    "deliveryMode": DELIVERY_MODE,
                }
            }),
        }
    }
}

/// Mutation routes: method + path construction stays in one place, so the journaled
/// tuple (request id + body) and the wire shape can never drift apart.
enum Route {
    TopicPosts(String),
    TopicStarts(String),
    TopicReports(String),
    PostEdits(String),
    PostDeletes(String),
    TopicReadPosition(String),
    Membership(String),
    Leave(String, String),
    Watches,
    ModerationAction(String),
    UserManagement(String),
}

impl Route {
    fn build(self) -> (&'static str, String) {
        match self {
            Self::TopicPosts(topic) => ("POST", format!("/api/v1/experience/topics/{topic}/posts")),
            Self::TopicReadPosition(topic) => ("POST", format!("/api/v1/experience/topics/{topic}/read-position")),
            Self::Membership(tangent) => ("PUT", format!("/api/v1/experience/tangents/{tangent}/membership")),
            Self::Leave(tangent, request_id) => {
                ("DELETE", format!("/api/v1/experience/tangents/{tangent}/membership?requestId={}", encode(&request_id)))
            }
            Self::TopicStarts(space) => ("POST", format!("/api/v1/experience/tangents/{space}/topics")),
            Self::TopicReports(thread) => ("POST", format!("/api/v1/experience/topics/{thread}/reports")),
            Self::PostEdits(post) => ("PATCH", format!("/api/v1/experience/posts/{post}")),
            Self::PostDeletes(post) => ("DELETE", format!("/api/v1/experience/posts/{post}")),
            Self::Watches => ("PUT", "/api/v1/experience/watches".to_string()),
            Self::ModerationAction(case_id) => ("POST", format!("/api/v1/experience/moderation/cases/{case_id}/actions")),
            Self::UserManagement(user) => ("POST", format!("/api/v1/experience/moderation/users/{user}/management")),
        }
    }
}

/// A local-time rendering of an epoch-milliseconds stamp, for the few identity
/// facts that name a moment (WhoAmI's minted-at).
fn millisecond_stamp(epoch_ms: i64) -> String {
    let seconds = epoch_ms.div_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let time_of_day = seconds.rem_euclid(86_400);
    let (hour, minute) = (time_of_day / 3600, (time_of_day % 3600) / 60);
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02} {hour:02}:{minute:02}")
}

fn invalid_ref(kind: &str) -> ExperienceError {
    ExperienceError::Application {
        code: "invalid_arguments".into(),
        message: format!("Copy a {kind} reference returned by this server."),
    }
}

/// Presentation fields are bounded and cannot change a connection's destination.
fn project_server_card(origin: &str, raw: &Value, refreshed_at: i64) -> Option<ServerCard> {
    fn field(raw: &Value, name: &str, limit: usize) -> String {
        raw.get(name).and_then(Value::as_str).unwrap_or_default().trim()
            .chars().filter(|c| !c.is_control() || matches!(c, '\n' | '\t')).take(limit).collect()
    }
    // An unrelated JSON endpoint must not replace a previously captured server card.
    let name = field(raw, "name", 120);
    if name.is_empty() { return None; }
    let image = field(raw, "coverImageUrl", 2049);
    let safe_image = image.len() <= 2048
        && !image.chars().any(|c| c.is_control() || matches!(c, '\\' | '\'' | '"'));
    let cover_image_url = if safe_image && image.starts_with('/') && !image.starts_with("//") {
        format!("{origin}{image}")
    } else if safe_image && image.starts_with("https://")
        && image[8..].split(['/', '?', '#']).next().is_some_and(|host| !host.is_empty() && !host.contains('@') && !host.chars().any(char::is_whitespace)) {
        image
    } else { String::new() };
    Some(ServerCard {
        origin: origin.to_string(),
        name,
        description: field(raw, "welcomeMessage", 4096),
        byline: field(raw, "byline", 240),
        cover_image_url,
        motd: field(raw, "motd", 4096),
        owner_participant_id: field(raw, "ownerParticipantId", 256),
        refreshed_at,
    })
}

/// The browser target of one operator-page anchor. Pure construction, so tests can
/// assert the URL without opening anything. The anchor
/// carries its own sigil: `#create-companion` (a fragment) or `bind/{localId}/atproto`
/// (the connector-served bind route, a path).
pub fn registration_target(page_url: &str, anchor: &str) -> String {
    format!("{page_url}{anchor}")
}

/// A companion handle derived from an authenticated handle: the account's own when it
/// is valid and free, then numbered fallbacks, then a minted name. Presentation only —
/// the credential's DID is the identity, so any of these is honest.
fn fresh_handle(store: &StateStore, authenticated: &str) -> String {
    let base = authenticated.trim().trim_start_matches('@');
    if valid_handle(base) && store.companion_by_handle(base).is_none() {
        return base.to_string();
    }
    for attempt in 2..50u32 {
        let candidate = format!("{base}-{attempt}");
        if valid_handle(&candidate) && store.companion_by_handle(&candidate).is_none() {
            return candidate;
        }
    }
    format!("companion-{}", crate::adapters::store::short_uuid())
}

/// The per-companion sign-in target on the companion manager: the connector-served
/// `/bind` page `bind/{localId}/atproto` — a path, not a fragment, since the bind flow
/// is its own route now.
pub fn bind_anchor(local_id: &str) -> String {
    format!("bind/{local_id}/atproto")
}

/// The origin (`scheme://host:port`) of a page URL, for the reachability probe. Pure
/// construction; a URL without the expected shape answers `None` (honestly unprobeable).
fn page_origin(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let authority = rest.split('/').next()?;
    if authority.is_empty() {
        return None;
    }
    Some(format!("{scheme}://{authority}"))
}

/// The honest problem code of a tool outcome, for feed narration. Outcomes without a
/// problem (or unrenderable ones) report the generic blocked shape.
fn problem_code_of(outcome: &ToolOutcome) -> String {
    outcome
        .structured
        .pointer("/problem/code")
        .and_then(Value::as_str)
        .unwrap_or("blocked")
        .to_string()
}

/// Honest operator wording for a refused discovery document. The 503 family on this
/// surface all means one thing: the server has no proof audience configured.
fn discovery_error(error: &ExperienceError) -> String {
    match error {
        ExperienceError::Unreachable => "the server could not be reached".to_string(),
        ExperienceError::Unauthorized | ExperienceError::DpopChallenge { .. } => {
            "the discovery document refused the request".to_string()
        }
        ExperienceError::Application { code, message } => match code.as_str() {
            "exchange_unconfigured" | "public_origin_unconfigured" | "exchange_unavailable" => {
                "exchange_unavailable: this server has no proof audience configured (service-proof enrollment is off there)".to_string()
            }
            other => format!("the discovery document was refused ({other}): {message}"),
        },
        ExperienceError::Transport(detail) => detail.clone(),
    }
}

/// Honest operator wording when the PDS refuses the service-auth request. A rejection
/// here is almost always an expired PDS session: the message says to re-bind.
fn service_auth_error(error: &ExperienceError) -> String {
    match error {
        ExperienceError::Unreachable => "the PDS could not be reached; re-try, or re-bind the companion if the PDS moved".to_string(),
        ExperienceError::Unauthorized => {
            "atproto_session_expired: the PDS session was rejected (it may have expired). Re-bind the companion on the companion manager.".to_string()
        }
        ExperienceError::DpopChallenge { .. } => {
            "atproto_session_expired: the PDS kept challenging the DPoP proof for a fresh nonce. Re-try, or re-bind the companion on the companion manager.".to_string()
        }
        ExperienceError::Application { code, message } => match code.as_str() {
            // The reference PDS's granular-scope refusal: a session bound before the
            // rpc permission existed cannot mint service proofs. One re-bind (the
            // updated consent screen shows the permission) extends the grant.
            "ScopeMissingError" => format!(
                "atproto_scope_missing: the bound account's OAuth grant lacks the service-proof permission this PDS demands ({message}). Re-bind the companion on the companion manager to grant it."
            ),
            _ => format!("the PDS refused the service-auth request ({code}): {message}"),
        },
        ExperienceError::Transport(detail) => detail.clone(),
    }
}

/// Honest operator wording for the `/mcp/token` exchange outcomes: 503 → no proof
/// audience configured; 401 → the proof was rejected (invalid or replayed); 403 → the
/// participant is suspended there.
fn exchange_error(error: &ExperienceError) -> String {
    match error {
        ExperienceError::Unreachable => "the server could not be reached".to_string(),
        ExperienceError::Unauthorized | ExperienceError::DpopChallenge { .. } => {
            "invalid_service_proof: the server rejected the proof (invalid or already used); enroll again".to_string()
        }
        ExperienceError::Application { code, message } => match code.as_str() {
            "exchange_unavailable" | "exchange_unconfigured" | "public_origin_unconfigured" => {
                format!("exchange_unavailable: this server has no proof audience configured ({message})")
            }
            "participant_suspended" => format!("participant_suspended: the server reports this companion is suspended there ({message})"),
            "invalid_service_proof" | "service_proof_required" => {
                format!("invalid_service_proof: the server rejected the proof ({message}); enroll again")
            }
            other => format!("the server blocked the exchange ({other}): {message}"),
        },
        ExperienceError::Transport(detail) => detail.clone(),
    }
}

/// Collects the canonical references a response exposes, for alias assignment.
fn collect_refs(experience: &ExperienceDto, refs_out: &mut Vec<String>) {
    if let Some(place) = &experience.place.server_ref {
        refs_out.push(place.clone());
    }
    if let Some(reference) = &experience.place.tangent_ref {
        refs_out.push(reference.clone());
    }
    if let Some(reference) = &experience.place.topic_ref {
        refs_out.push(reference.clone());
    }
    for item in &experience.attention.items {
        refs_out.push(item.source_ref.clone());
        refs_out.push(item.scope_ref.clone());
    }
    for action in &experience.actions {
        refs_out.push(action.target_ref.clone());
    }
    for key in ["postRef", "throughPostRef", "resultRef"] {
        if let Some(reference) = experience.result.data.get(key).and_then(Value::as_str) {
            refs_out.push(reference.to_string());
        }
    }
    if let Some(receipt) = &experience.result.receipt {
        if let Some(reference) = &receipt.result_ref {
            refs_out.push(reference.clone());
        }
    }
    if let Some(posts) = experience.result.data.get("posts").and_then(Value::as_array) {
        for post in posts {
            if let Some(reference) = post.get("ref").and_then(Value::as_str) {
                refs_out.push(reference.to_string());
            }
        }
    }
}

fn encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn truncate_reference(reference: &str) -> String {
    let tail = reference.rsplit("::").next().unwrap_or(reference);
    tail.chars().take(12).collect()
}

#[cfg(test)]
mod server_card_checks {
    use super::*;

    #[test]
    fn server_card_keeps_enrolled_origin_and_rejects_unsafe_art() {
        let origin = "https://garage.example";
        let mut profile = json!({
            "name": "Leo's Garage", "welcomeMessage": "A place to make things.",
            "coverImageUrl": "/art/garage.png", "origin": "https://unrelated.example"
        });
        let card = project_server_card(origin, &profile, 42).unwrap();
        assert_eq!(card.origin, origin);
        assert_eq!(card.cover_image_url, "https://garage.example/art/garage.png");
        assert_eq!(card.description, "A place to make things.");
        assert_eq!(project_server_card("http://127.0.0.1:5220", &profile, 42).unwrap().cover_image_url,
            "http://127.0.0.1:5220/art/garage.png");
        for image in ["//unrelated.example/art.png", "javascript:alert(1)", "https://user:pass@example.com/art.png", "/\\unrelated.example/art.png"] {
            profile["coverImageUrl"] = json!(image);
            assert!(project_server_card(origin, &profile, 42).unwrap().cover_image_url.is_empty());
        }
        assert!(project_server_card(origin, &json!({"status":"ok"}), 42).is_none());
    }
}
