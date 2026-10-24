//! The Companion Lobby daemon: the one process.
//!
//! This is the only crate that knows the async runtime, the file system and
//! how the pieces are composed. It is an event-driven monolith:
//!
//! * one command pipeline: every change validates, applies, commits to the
//!   state file, journals, then emits events;
//! * one event pipeline: small reactive handlers turn events into further
//!   commands and refresh projections;
//! * projections hold no truth: the state file is truth and the journal is
//!   attribution;
//! * the async runtime lives only here; adapters are synchronous and run on
//!   blocking threads with no lock held.
//!
//! The `companion-lobby` binary is a thin front over [`cli`].

pub mod attach;
pub mod broker;
pub mod browser;
pub mod calls;
pub mod cli;
pub mod discovery;
pub mod domain;
pub mod home;
pub mod http;
pub mod journal;
pub mod keyring;
pub mod lock;
pub mod manager;
pub mod mcp;
pub mod projections;
pub mod registry;
pub mod stop;
pub mod store;
pub mod trace;
