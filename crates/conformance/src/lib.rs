//! The executable specification of both contracts and of the daemon.
//!
//! * [`credential::run`] holds a credential adapter to the credential
//!   contract against its provider's fake, in process;
//! * [`service::run`] holds a service adapter to the service contract against
//!   its service's fake, in process;
//! * [`host`] proves the daemon through the two synthetic adapters of the
//!   test kit, running the real `companion-lobby` binary with them compiled
//!   in: the catalog a mind sees, isolation between companions, journal
//!   attribution, a late result after a revocation, the revoke cascade, and
//!   an ask a credential cannot answer.
//!
//! The suites run against the passbook and board adapters in this crate's
//! tests, and the host scenarios run from the daemon's tests, which know
//! where the binary is. No suite reaches the live network.

pub mod credential;
pub mod host;
pub mod service;
