//! This crate's adapter spokes: the stdio MCP edge, the operator manager page, the
//! tray, the browser opener, the durable store (with per-enrollment sessions), the
//! data-directory lock, the background checker and the diagnostics journal. The HTTP
//! experience client lives in `adapter-service-tangent`; the atproto OAuth client and
//! proof mint in `adapter-auth-atproto`.


pub mod browser;
pub mod diagnostics;

pub mod lockfile;
pub mod mcp;
pub mod manager;
pub mod poller;
pub mod store;
pub mod tray;
