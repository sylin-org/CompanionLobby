//! The daemon's side of the adapter contracts.
//!
//! A service adapter that establishes a connection asks the [`Broker`] for an
//! authorization. The broker hands the ask to the credential adapter of the
//! connection's credential, with a [`Slot`] through which that adapter reads
//! its own material, and hands back exactly what the adapter minted. The
//! slot re-checks with the lobby, at the moment of the read, that the
//! credential is still the one the establishment was pinned to and still
//! ready, and reads only that credential's material, which the daemon never
//! decodes. Nothing here holds the lobby while an adapter runs.

use std::sync::Arc;

use lobby_contracts::{
    Authority, Authorization, AuthorizationAsk, CredentialAdapter, CredentialId, MaterialSlot,
    Outcome, SealedMaterial,
};

use crate::domain::Lobby;

/// Mints authorizations from one credential for one establishment.
pub struct Broker<'a> {
    lobby: &'a Lobby,
    credential: CredentialId,
    adapter: Arc<dyn CredentialAdapter>,
}

impl<'a> Broker<'a> {
    /// A broker for `credential`, whose adapter is `adapter`.
    pub fn new(
        lobby: &'a Lobby,
        credential: CredentialId,
        adapter: Arc<dyn CredentialAdapter>,
    ) -> Self {
        Self {
            lobby,
            credential,
            adapter,
        }
    }
}

impl Authority for Broker<'_> {
    fn authorize(&self, ask: &AuthorizationAsk) -> Result<Authorization, Outcome> {
        let slot = Slot {
            lobby: self.lobby,
            credential: self.credential,
        };
        self.adapter.authorize(&slot, ask)
    }
}

/// One credential's material, read through the lobby.
pub struct Slot<'a> {
    lobby: &'a Lobby,
    credential: CredentialId,
}

impl MaterialSlot for Slot<'_> {
    fn read(&self) -> Result<SealedMaterial, Outcome> {
        self.lobby.read_material(self.credential)
    }
}
