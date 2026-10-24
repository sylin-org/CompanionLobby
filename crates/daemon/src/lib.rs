//! The Companion Lobby daemon: the one process.
//!
//! This is the only crate that knows the async runtime, the file system and
//! how the pieces are composed. What runs in this crate:
//!
//! * the loopback listener and its envelope ([`http`]), the attach shim
//!   ([`attach`]) and how every subcommand finds, verifies, starts and stops
//!   the daemon ([`contact`], [`lock`], [`discovery`], [`loopback`], [`stop`],
//!   [`browser`]);
//! * the lobby itself ([`domain`]): one command pipeline that validates,
//!   journals, commits to the state file, journals the outcome and emits
//!   events, and one event pipeline of small handlers; the state file
//!   ([`store`]) is the truth and the journal ([`journal`]) the attribution;
//! * what is projected from them and holds no truth ([`projections`]): the
//!   catalog a mind sees ([`mcp`]), and the manager page with its live feed
//!   ([`manager`]);
//! * the data directory, the attach secret and the sealed material and issued
//!   data adapters hand the daemon ([`home`], [`keyring`]), the
//!   daemon's own trace log ([`trace`]) and, on Windows, the crate's one module
//!   allowed `unsafe` (`win32`);
//! * the adapters the daemon runs with ([`registry`]), the broker through
//!   which a service adapter asks a credential adapter for authorizations
//!   ([`broker`]), and tool calls pinned by a ticket and settled against the
//!   connection as it is when the service answers ([`calls`]). Adapters run
//!   synchronously on blocking threads with no lock held.
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
