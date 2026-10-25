//! The daemon's side of the adapter contracts.
//!
//! A service adapter that establishes a connection, or establishes it again
//! during a tool call, asks the [`Broker`] for an authorization. The broker hands the ask to the credential adapter of the
//! connection's credential, with a [`Slot`] through which that adapter reads
//! its own material, and hands back exactly what the adapter minted. The
//! slot re-checks with the lobby, at the moment of the read, that the
//! credential is still the one the establishment was pinned to and still
//! ready, and reads only that credential's material, which the daemon never
//! decodes. Nothing here holds the lobby while an adapter runs.
//!
//! The same slot is how an adapter renews its material: it marks the
//! material rotating through the lobby, which journals the renewal and
//! writes the mark to the disk before the adapter spends the provider's
//! secret, and commits the new material through the lobby, which keeps it
//! only on top of the material it replaces. The slot remembers what it
//! handed out and whether it marked anything, so the lobby can tell, once
//! the adapter answered, whether a renewal ended cleanly. A [`Held`] slot
//! serves a credential the sponsor revoked: it hands its adapter the
//! material the lobby kept in memory, to revoke at the provider, and
//! renews nothing.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use lobby_contracts::{
    Authority, Authorization, AuthorizationAsk, CredentialAdapter, CredentialId, Explained,
    MaterialSlot, MaterialUpdate, Outcome, SealedMaterial,
};

use crate::domain::Lobby;

/// Mints authorizations from one credential for one establishment.
pub struct Broker<'a> {
    slot: Slot<'a>,
    adapter: Arc<dyn CredentialAdapter>,
    refused: Mutex<Option<Outcome>>,
}

impl<'a> Broker<'a> {
    /// A broker for `credential`, whose adapter is `adapter`.
    pub fn new(
        lobby: &'a Lobby,
        credential: CredentialId,
        adapter: Arc<dyn CredentialAdapter>,
    ) -> Self {
        Self {
            slot: Slot::new(lobby, credential),
            adapter,
            refused: Mutex::new(None),
        }
    }

    /// The slot the adapter read and renewed through.
    pub fn slot(&self) -> &Slot<'a> {
        &self.slot
    }

    /// The outcome of an authorization that said the credential needs
    /// signing in again, or that a renewal may have spent its secret, if one
    /// did.
    pub fn refused(&self) -> Option<Outcome> {
        *self.refused.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Authority for Broker<'_> {
    fn authorize(&self, ask: &AuthorizationAsk) -> Result<Authorization, Explained> {
        let minted = self.adapter.authorize(&self.slot, ask);
        if let Err(Explained {
            outcome: outcome @ (Outcome::NeedsSignIn | Outcome::Unknown),
            ..
        }) = minted
        {
            *self.refused.lock().unwrap_or_else(PoisonError::into_inner) = Some(outcome);
        }
        minted
    }
}

/// One credential's material, read and renewed through the lobby.
pub struct Slot<'a> {
    lobby: &'a Lobby,
    credential: CredentialId,
    handed: Mutex<Option<SealedMaterial>>,
    marked: AtomicBool,
}

impl<'a> Slot<'a> {
    /// The slot of `credential`.
    pub fn new(lobby: &'a Lobby, credential: CredentialId) -> Self {
        Self {
            lobby,
            credential,
            handed: Mutex::new(None),
            marked: AtomicBool::new(false),
        }
    }

    /// The material this slot handed out last.
    pub fn handed(&self) -> Option<SealedMaterial> {
        self.handed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Whether this slot journaled a renewal by marking the material.
    pub fn marked(&self) -> bool {
        self.marked.load(Ordering::SeqCst)
    }
}

impl MaterialSlot for Slot<'_> {
    fn credential(&self) -> CredentialId {
        self.credential
    }

    fn read(&self) -> Result<SealedMaterial, Outcome> {
        let material = self.lobby.read_material(self.credential)?;
        *self.handed.lock().unwrap_or_else(PoisonError::into_inner) = Some(material.clone());
        Ok(material)
    }

    fn mark_rotating(&self, read: &SealedMaterial) -> Result<(), Outcome> {
        self.lobby.mark_rotating(self.credential, read)?;
        self.marked.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn unmark_rotating(&self) {
        self.lobby.unmark_rotating(self.credential);
    }

    fn commit(&self, update: MaterialUpdate) -> Result<(), Outcome> {
        self.lobby.commit_material(self.credential, update)
    }
}

/// A revoked credential's material, held in memory for its adapter to
/// revoke it at the provider. It renews nothing.
pub struct Held {
    credential: CredentialId,
    material: SealedMaterial,
}

impl Held {
    /// The material `material` of the revoked `credential`.
    pub fn new(credential: CredentialId, material: SealedMaterial) -> Self {
        Self {
            credential,
            material,
        }
    }
}

impl MaterialSlot for Held {
    fn credential(&self) -> CredentialId {
        self.credential
    }

    fn read(&self) -> Result<SealedMaterial, Outcome> {
        Ok(self.material.clone())
    }

    fn mark_rotating(&self, _read: &SealedMaterial) -> Result<(), Outcome> {
        Err(Outcome::Revoked)
    }

    fn unmark_rotating(&self) {}

    fn commit(&self, _update: MaterialUpdate) -> Result<(), Outcome> {
        Err(Outcome::Revoked)
    }
}
