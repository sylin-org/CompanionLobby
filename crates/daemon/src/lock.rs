//! The single-instance guard.
//!
//! This module will hold an OS advisory lock on `daemon.lock` in the data
//! directory for the life of the process, released by the OS when the process
//! dies, and write the pid, start time and port into it so the attach shim can
//! verify that the listener it talks to belongs to the lock holder.
