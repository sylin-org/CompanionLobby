//! Application layer: the connector hub, its ports, and the wire contract with Tangent
//! experience APIs. The hub composes domain policies and adapters; it never touches stdio or
//! sockets directly.

pub mod bus;

pub mod hub;
pub mod operations;

