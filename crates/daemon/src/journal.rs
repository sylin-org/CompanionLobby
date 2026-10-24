//! The journal: attribution.
//!
//! This module will append JSON lines with ids of the form `storeId:seq`. An
//! `invoked` entry is written before every external call, and the call is
//! refused if that entry cannot be written; a `settled` entry follows the call,
//! and a failed settled append raises an operator condition instead of a false
//! error. Every command is journaled with its initiator: a mind through its
//! attachment, the sponsor, or the lobby. Files rotate by a fixed number of
//! generations, and reads are fenced by cursor. Entries never hold content or
//! secrets.
