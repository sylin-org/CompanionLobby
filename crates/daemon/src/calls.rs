//! Tool calls through a ticket.
//!
//! This module will begin a call by issuing a ticket that pins the connection,
//! the credential epoch and the issued revision; run the authorization and the
//! service call on a blocking thread with no lock held; and settle the call by
//! rechecking the ticket, so a read that drifted answers revoked and a write
//! that drifted answers with an unknown outcome.
