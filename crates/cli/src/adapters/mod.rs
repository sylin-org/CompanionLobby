//! The adapters (spokes): stdio MCP edge, command-line edge, operator web page, tray,
//! browser opener, HTTP experience client, atproto OAuth client, durable store (with
//! per-enrollment sessions), the data-directory lock, background checker, diagnostics
//! journal, and host delivery.


pub mod browser;
pub mod diagnostics;

pub mod lockfile;
pub mod mcp;
pub mod manager;
pub mod poller;
pub mod store;
pub mod tray;
