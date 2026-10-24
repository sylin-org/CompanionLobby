//! The daemon's side of the adapter contracts.
//!
//! This module will implement `Authority`, `MaterialSlot` and `Ingress` for one
//! call. Each re-reads the state through the domain thread under the call's
//! ticket, dispatches to the credential adapter of the credential's provider
//! kind, and never decodes a sealed blob itself.
