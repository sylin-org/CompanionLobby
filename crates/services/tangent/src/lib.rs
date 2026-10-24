//! The Tangent service adapter: Tangent in Tangent's own words.
//!
//! A companion holds one connection per Tangent Space origin. The adapter
//! accepts the provider kind whose credentials can mint a service proof for
//! Tangent's identity exchange, with the permission to call it. It will own:
//!
//! * establishment: reading the Space's auth discovery document against the
//!   reviewed origin, deriving or proving the audience, asking the authority
//!   for a short-lived service proof, and exchanging it for a Tangent token
//!   with the companion's moniker as the participant name, a fresh proof per
//!   attempt and a proactive re-exchange before the token expires;
//! * one typed tool per Tangent route, each carrying its route's invariants,
//!   its effect and retry rule, page sizes well below the server maximum and a
//!   rendered-output budget;
//! * the tools a connection's authority makes available;
//! * the outcome table built from Tangent's own problem codes, with exactly one
//!   re-establishment on a resource 401;
//! * attention by polling for updates.
//!
//! Tangent's DTOs, route strings and tool names live here and nowhere else.
