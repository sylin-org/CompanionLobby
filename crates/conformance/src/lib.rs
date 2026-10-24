//! The executable specification of both contracts and of the daemon.
//!
//! Suites run through the real daemon on a real loopback port with the real
//! `companion-lobby` binary. This crate will own:
//!
//! * `credential::run(adapter, fixture)`: an unsupported ask is refused before
//!   material is read or the network is touched; refresh never widens; a
//!   failed rotation commit quarantines and never resubmits; sign-in records
//!   the account the provider authenticated; outcomes carry no provider text;
//! * `service::run(adapter, fixture)`: catalog names are valid, unique and at
//!   most 64 characters; every write declares its retry rule; availability is
//!   monotone in authority; unknown codes map to an unknown outcome on writes
//!   and to temporarily unavailable on reads; a definite 403 is refused; foreign
//!   references never become paths; third-party strings never reach tool names,
//!   descriptions or schemas;
//! * `host::run`: attachment, catalog projection, the revoke cascade, refusal
//!   of a late result, the quarantine fence, isolation between attachments,
//!   journal attribution and redaction, the loopback envelope per route,
//!   restart durability, daemon start and detachment, and restart on a build
//!   mismatch;
//! * replay of the recorded MCP host transcripts against the attach shim;
//! * test-mode knobs (ephemeral port, per-test data directory, no detach) so a
//!   test run never leaves a daemon behind.
//!
//! No suite reaches the live network.
