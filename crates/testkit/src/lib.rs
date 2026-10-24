//! Wire fakes, fixtures and synthetic adapters for tests.
//!
//! The test kit depends only on the contracts, never on the daemon or a real
//! adapter, so every crate can use it as a dev-dependency. It will own:
//!
//! * a scripted loopback HTTP/1.1 listener with handler injection, lowercase
//!   header capture, raw body bytes and a gate that freezes a request
//!   mid-flight;
//! * separately startable fake roles for the provider and service wire
//!   protocols the real adapters speak, each on its own listener;
//! * a synthetic credential adapter and a synthetic service adapter with their
//!   own nouns, many addresses per companion, an authority-dependent catalog,
//!   a write whose outcome can be unknown and an optional push stream; they are
//!   the first implementations of both contracts;
//! * a temporary data directory per test;
//! * the stdio peer harness that drives `companion-lobby attach` and matches
//!   responses by request id;
//! * the recorded MCP host transcripts used as fixtures.
//!
//! Nothing here reaches the live network.
