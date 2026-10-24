//! The daemon's own operational log.
//!
//! This module will write a rotating, bounded log of what the daemon itself
//! did (start, stop, refusals, faults) and install a panic hook that records
//! the panic before the process aborts. It never holds secrets or content and
//! is separate from the journal, which attributes what companions did.
