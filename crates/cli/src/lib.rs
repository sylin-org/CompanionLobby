//! tangent-connector: the personal local MCP connector for Tangent. A DDD-aligned monolith:
//! pure domain vocabulary, an application hub with narrow ports, adapter spokes, and
//! deterministic presentation. Both input models — MCP over stdio and the command line —
//! are intakes of the same hub.

pub mod adapters;
pub mod application;

pub mod presentation;

use std::path::PathBuf;
use std::sync::Arc;

use crate::adapters::store::StateStore;
use crate::application::bus::EventBus;
use crate::application::hub::ConnectorHub;
use companion_core::domain::companion::CallerId;

/// Where durable state lives: `$TANGENT_CONNECTOR_HOME` or `~/.tangent-connector`.
pub fn data_directory() -> PathBuf {
    if let Ok(home) = std::env::var("TANGENT_CONNECTOR_HOME") {
        return PathBuf::from(home);
    }
    let base = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    base.join(".tangent-connector")
}

/// Builds the hub over the ureq experience client, the durable store and the platform
/// browser.
pub fn build_hub(caller: CallerId, data_dir: PathBuf) -> Result<Arc<ConnectorHub>, String> {
    let events = Arc::new(EventBus::new());
    let store = StateStore::open(&data_dir)?;
    adapters::diagnostics::spawn(&events, data_dir.clone());
    Ok(Arc::new(ConnectorHub::new(store, events, caller).with_pages(adapters::browser::system())))
}
