//! Sealed material at rest.
//!
//! This module will keep one `.material` file per credential and one `.issued`
//! file per connection, sealed with DPAPI in user scope on Windows and written
//! with mode 0600 elsewhere, plus the rotation markers that make a single-use
//! refresh safe across a crash: mark, rotate at the provider, commit, clear.
//! It stores bytes; only the adapter that sealed a blob decodes it. The
//! per-install attach secret lives here too.
