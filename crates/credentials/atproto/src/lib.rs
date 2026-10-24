//! The atproto credential adapter.
//!
//! This adapter is the only reader of its sealed material (subject, access and
//! refresh tokens, PDS, authorization server, client id, DPoP key and granted
//! permissions). It will own:
//!
//! * identity resolution through the outbound agent with the public-HTTPS
//!   policy, the authorization server metadata checklist and PDS
//!   protected-resource discovery;
//! * sign-in with PAR, PKCE and DPoP, with parked sign-in state owned by the
//!   adapter (one flight per credential, a time limit, one use) and a loopback
//!   client id whose scope is exactly what the connection requests;
//! * completion on the lobby's own return route, checking issuer, state and
//!   code, and recording the account the authorization server authenticated;
//! * typed refresh with a per-credential lock and the marker-first quarantine
//!   protocol through the material slot, so a single-use refresh token is never
//!   sent twice;
//! * service proofs minted for any audience and method a service adapter
//!   declares, after checking the audience and that a granted permission covers
//!   the method;
//! * DPoP resource proofs for proved requests, and an opt-in portrait read.
//!
//! It contains no constant belonging to any service.
