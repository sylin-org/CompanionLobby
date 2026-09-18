//! Application layer: the connector hub and the closed operation vocabulary. The wire
//! contract lives in `adapter-service-tangent::contract`, the ports and adapter traits
//! in `companion-core`; the hub composes domain policies and adapters and never touches
//! stdio or sockets directly.

pub mod bus;

pub mod hub;
pub mod operations;

