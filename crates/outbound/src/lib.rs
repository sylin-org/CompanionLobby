//! The one vetted outbound HTTP agent.
//!
//! Every adapter that talks to the network builds its agent here, so the
//! properties that stop server-side request forgery cannot be dropped by
//! accident. This crate will own:
//!
//! * a blocking agent built from an [`AddressPolicy`](lobby_contracts::AddressPolicy);
//! * a resolver that pins DNS once per request and refuses non-public targets
//!   unless the policy allows loopback;
//! * no redirects, no proxy taken from the environment, a 10 s connect and a
//!   30 s read timeout;
//! * bounded bodies with a per-call limit (256 KiB by default, raised per route
//!   by the adapter that needs it), JSON content-type checks, and 3xx refused;
//! * raw status, headers and body returned, so each adapter maps outcomes in
//!   its own words.
//!
//! It is synchronous: the daemon calls adapters on blocking threads.
