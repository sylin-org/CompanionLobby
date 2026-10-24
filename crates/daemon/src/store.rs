//! The state file: the truth.
//!
//! This module will own `lobby.json` (schema 1, camelCase, unknown fields
//! refused). A file it cannot understand is refused and never rewritten.
//! Writes are atomic: a per-call temporary name, a flush, a rename, a parent
//! directory sync on unix and a bounded retry on Windows sharing violations,
//! failing closed. One `commit` applies a change or rolls it back. Forward
//! schema upgrades follow the policy recorded in ADR 0001.
