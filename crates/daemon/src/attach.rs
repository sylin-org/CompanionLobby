//! The attach shim behind `companion-lobby attach <companion>`.
//!
//! This module will relay an MCP host's stdio to the daemon:
//!
//! * bounded newline framing, a local parse error for a malformed frame, and
//!   stdout reserved for protocol frames;
//! * before sending the attach secret, it reads the discovery document and
//!   verifies that the listener's pid is the lock holder's pid and that the
//!   store fingerprint matches;
//! * on a build mismatch it asks the daemon to stop and starts it again;
//! * when no daemon runs, it starts `run` from its own executable, windowless
//!   and detached, with null stdio and non-inheritable handles, and answers the
//!   host within its startup budget.
