//! The Companion Lobby daemon: the one process.
//!
//! This is the only crate that knows the async runtime, the file system and
//! how the pieces are composed. What runs in this crate:
//!
//! * the loopback listener and its envelope ([`http`]), the attach shim
//!   ([`attach`]) and how every subcommand finds, verifies, starts and stops
//!   the daemon ([`contact`], [`lock`], [`discovery`], [`loopback`], [`stop`],
//!   [`browser`]);
//! * the lobby's state, held in memory, with commands that validate before
//!   they apply ([`domain`]), and the catalog a mind sees, projected from it
//!   ([`projections`], [`mcp`]);
//! * the data directory and the attach secret ([`home`], [`keyring`]), the
//!   daemon's own trace log ([`trace`]) and, on Windows, the crate's one module
//!   allowed `unsafe` (`win32`).
//!
//! The modules [`store`], [`journal`], [`calls`], [`broker`], [`registry`]
//! and [`manager`] hold no code yet; each says what it will own when the
//! state file, the journal and the adapters arrive. Then every change will
//! validate, apply, commit to the state file, journal and emit events, and
//! adapters will run synchronously on blocking threads with no lock held.
//!
//! The `companion-lobby` binary is a thin front over [`cli`].

pub mod attach;
pub mod broker;
pub mod browser;
pub mod calls;
pub mod cli;
pub mod contact;
pub mod discovery;
pub mod domain;
pub mod home;
pub mod http;
pub mod journal;
pub mod keyring;
pub mod lock;
pub mod loopback;
pub mod manager;
pub mod mcp;
pub mod projections;
pub mod registry;
pub mod stop;
pub mod store;
pub mod trace;
#[cfg(windows)]
pub mod win32;
