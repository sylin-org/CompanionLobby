//! The loopback HTTP listener.
//!
//! This module will listen on 127.0.0.1 only. The Host header is checked before
//! a body is read, headers and bodies are capped, and `localhost:<port>` is
//! redirected to `127.0.0.1:<port>`. Operator mutations under `/api/` require
//! same-origin browser evidence and JSON; `/mcp` refuses browser-origin
//! evidence and requires the per-install attach secret. Every response is
//! sent with no-store, no-referrer and nosniff.
