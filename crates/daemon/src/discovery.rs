//! The discovery document.
//!
//! This module will serve `GET /api/discovery` with the product name, the
//! discovery version and the manager origin, plus the build id, the supported
//! protocol revisions, the store fingerprint and the lock holder's pid. Fields
//! are only ever added. The attach shim reads it to decide whether the daemon
//! on the port is the right one.
