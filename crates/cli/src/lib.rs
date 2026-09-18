//! companion-lobby: the personal local MCP connector for Tangent. A DDD-aligned
//! workspace: pure domain vocabulary in `companion-core`, the application hub with
//! narrow ports and adapter crates, and this crate's own edges — the MCP stdio server,
//! the command line, the operator manager page. Both input models are intakes of the
//! same hub.

pub mod adapters;
pub mod application;

pub mod presentation;

use std::path::PathBuf;
use std::sync::Arc;

use crate::adapters::store::StateStore;
use crate::application::bus::EventBus;
use crate::application::hub::ConnectorHub;
use companion_core::domain::companion::CallerId;

/// Where durable state lives: `$COMPANION_LOBBY_HOME` or `~/.companion-lobby`.
pub fn data_directory() -> PathBuf {
    if let Ok(home) = std::env::var("COMPANION_LOBBY_HOME") {
        return PathBuf::from(home);
    }
    let base = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    base.join(".companion-lobby")
}

/// Builds the hub over the real adapters — the ureq Tangent experience client, the
/// atproto proof mint, the durable store and the platform browser. Tests replace the
/// service through the same registration seam.
pub fn build_hub(caller: CallerId, data_dir: PathBuf) -> Result<Arc<ConnectorHub>, String> {
    let events = Arc::new(EventBus::new());
    let store = StateStore::open(&data_dir)?;
    adapters::diagnostics::spawn(&events, data_dir.clone());
    let hub = ConnectorHub::new(store, events, caller)
        .with_service(Arc::new(adapter_service_tangent::experience::UreqExperience::new()))
        .with_auth(Arc::new(adapter_auth_atproto::atproto_oauth::AtprotoOauth::new()))
        .with_pages(adapters::browser::system());
    Ok(Arc::new(hub))
}
