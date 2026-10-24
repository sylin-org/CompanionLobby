//! The discovery document and the build id.
//!
//! `GET /api/discovery` answers with the product name, the discovery version
//! and the manager origin, plus the build id, the protocol revisions served,
//! the install fingerprint and the lock holder's pid. Fields are only ever
//! added, so a page that probes the loopback port for a running daemon keeps
//! working. It is the one document served with cross-origin headers, and it
//! says nothing about companions.
//!
//! The attach shim reads it to decide whether the daemon on the port is the
//! right one: the same product, the same build, the same install, and the
//! process that holds the daemon lock.

use std::io::{self, Read};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::keyring::hex;
use crate::mcp::PROTOCOL_VERSIONS;

/// The product name the document carries.
pub const PRODUCT: &str = "companion-lobby";
/// The document's version.
pub const DISCOVERY_VERSION: u32 = 1;
/// The path it is served at.
pub const DISCOVERY_PATH: &str = "/api/discovery";

/// The discovery document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Discovery {
    /// Always [`PRODUCT`].
    pub product: String,
    /// Always [`DISCOVERY_VERSION`].
    pub discovery_version: u32,
    /// Where the manager page is, such as `http://127.0.0.1:5219`.
    pub manager_origin: String,
    /// The build the daemon runs.
    pub build_id: String,
    /// The MCP protocol revisions it serves.
    pub protocol_versions: Vec<String>,
    /// The fingerprint of the install whose data directory it serves.
    pub store_fingerprint: String,
    /// The pid of the process holding the daemon lock: the daemon itself.
    pub lock_holder_pid: u32,
}

impl Discovery {
    /// The document for this daemon.
    pub fn new(port: u16, build_id: &str, store_fingerprint: &str) -> Self {
        Self {
            product: PRODUCT.to_owned(),
            discovery_version: DISCOVERY_VERSION,
            manager_origin: manager_origin(port),
            build_id: build_id.to_owned(),
            protocol_versions: PROTOCOL_VERSIONS.iter().map(|v| (*v).to_owned()).collect(),
            store_fingerprint: store_fingerprint.to_owned(),
            lock_holder_pid: std::process::id(),
        }
    }
}

/// The manager page's origin on `port`.
pub fn manager_origin(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

/// The build id of the running executable: the first 16 bytes of the SHA-256
/// of its file, as hex. Two processes agree on it exactly when they run the
/// same bytes, so a rebuilt binary never talks to a daemon of an older build.
pub fn build_id() -> io::Result<String> {
    static BUILD_ID: OnceLock<String> = OnceLock::new();
    if let Some(id) = BUILD_ID.get() {
        return Ok(id.clone());
    }
    let id = build_id_of(&std::env::current_exe()?)?;
    Ok(BUILD_ID.get_or_init(|| id).clone())
}

/// The build id of the executable at `path`.
pub fn build_id_of(path: &std::path::Path) -> io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 16];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex(&hasher.finalize()[..16]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_document_keeps_the_compatible_fields_and_names_no_companion() {
        let document = Discovery::new(5219, "b", "f");
        let value = serde_json::to_value(&document).expect("json");
        assert_eq!(value["product"], "companion-lobby");
        assert_eq!(value["discoveryVersion"], 1);
        assert_eq!(value["managerOrigin"], "http://127.0.0.1:5219");
        assert_eq!(value["lockHolderPid"], std::process::id());
        assert_eq!(value["protocolVersions"][3], "2025-11-25");
        let rendered = value.to_string();
        assert!(!rendered.contains("companion\"") && !rendered.contains("secret"));
    }

    #[test]
    fn the_build_id_is_stable_within_a_process() {
        let first = build_id().expect("build id");
        assert_eq!(first.len(), 32);
        assert_eq!(build_id().expect("again"), first);
    }
}
