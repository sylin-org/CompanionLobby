//! `companion-lobby stop`.
//!
//! This module will send a same-origin stop request to the running daemon,
//! which drains in-flight calls and only then exits, so a rebuild can replace
//! the executable.
