//! The domain thread: one owner of truth.
//!
//! This module will run the command pipeline and the event pipeline on one
//! thread that owns the state:
//!
//! * commands (create, rename and remove a companion; begin and complete a
//!   sign-in; revoke a credential; grant and revoke a connection; attach and
//!   detach; begin and settle a call; commit material) each validate, apply,
//!   commit, journal and then emit events;
//! * small reactive handlers turn events into further commands, for example a
//!   refused authorization marks the credential as needing sign-in, which
//!   updates its connections, which changes the catalog, which notifies
//!   attached minds;
//! * pushed service events enter the same queue through the ingress.
//!
//! Channels are in-process only.
