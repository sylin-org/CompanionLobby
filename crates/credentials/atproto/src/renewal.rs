//! Keeping an OAuth session alive, and ending it at the provider.
//!
//! The access token's expiry, kept in the material from the token answer's
//! `expires_in`, says when a session is due: a minute before it
//! ([`MARGIN_SECS`]). A refresh follows the contract's order. The
//! authorization server's metadata is read first, and a failure to read it
//! is only a reason to try again later, since nothing was spent. Then the
//! slot marks the material rotating, and only then is the refresh token
//! presented, once, with the client id the session was granted under and a
//! proof made with the session's key. The server's DPoP nonce is learned
//! from its first answer and the request made once more with it; a refresh
//! token is never presented a third time.
//!
//! From the moment the refresh token may have reached the server, the answer
//! decides:
//!
//! * a token answer that keeps the profile, for the same subject and within
//!   the scope the session held, is committed through the slot, which clears
//!   the mark;
//! * 429 spent nothing: the mark is cleared and the lobby tries again later;
//! * any other 4xx is the server refusing the session: the mark is cleared,
//!   since nothing was spent, and the credential needs signing in again;
//! * anything else (no answer read whole, a 5xx, an answer that breaks the
//!   profile, a subject or a scope the session does not hold, or new
//!   material the lobby could not keep) may have spent the refresh token
//!   with nothing new kept: the mark stays, the answer is
//!   [`Outcome::Unknown`], and the lobby never presents that token again.
//!
//! One session renews once at a time: a lock per credential, keyed by the
//! slot's credential, is held across the whole renewal, and a second renewal
//! waits, reads the renewed material and finds it fresh. The lock is the
//! adapter's own; the daemon holds none of its own while the adapter runs.
//!
//! Nonces are kept per server origin and per session key, so two sessions
//! at one server never share one; each is trusted for [`NONCE_SECS`], and
//! at most [`NONCES`] are kept, the oldest giving way.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use lobby_contracts::{
    AddressPolicy, CredentialId, Freshness, MaterialSlot, MaterialUpdate, Outcome, Revocation,
};
use lobby_outbound::{Failure, Outbound, Request};
use serde_json::Value;

use crate::codec::{is_token, now_secs};
use crate::material::Material;
use crate::server;
use crate::tokens;
use crate::wire::{json_object, proved_post, status_outcome};

/// How long before the access token expires a session is renewed.
pub const MARGIN_SECS: i64 = 60;
/// How long a server's nonce is trusted.
pub const NONCE_SECS: u64 = 300;
/// How many nonces are kept at most.
pub const NONCES: usize = 64;
/// The longest nonce kept.
const NONCE_LIMIT: usize = 1024;

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One lock per credential.
#[derive(Debug, Default)]
pub struct Locks {
    held: Mutex<HashMap<CredentialId, Arc<Mutex<()>>>>,
}

impl Locks {
    /// The lock of `credential`. A registry poisoned by a panic still hands
    /// out the lock it holds for that credential, never a fresh one, so one
    /// session never renews twice at once.
    pub fn of(&self, credential: CredentialId) -> Arc<Mutex<()>> {
        locked(&self.held).entry(credential).or_default().clone()
    }
}

/// The servers' DPoP nonces, per origin and per session key.
#[derive(Debug, Default)]
pub struct Nonces {
    kept: Mutex<HashMap<(String, String), (String, Instant)>>,
}

impl Nonces {
    /// The nonce `origin` issued last to the key `thumbprint`, while it is
    /// trusted.
    pub fn get(&self, origin: &str, thumbprint: &str) -> Option<String> {
        let kept = locked(&self.kept);
        let (nonce, at) = kept.get(&(origin.to_owned(), thumbprint.to_owned()))?;
        (at.elapsed() < Duration::from_secs(NONCE_SECS)).then(|| nonce.clone())
    }

    /// Keeps `nonce` as the one `origin` issued last to `thumbprint`,
    /// letting go of every nonce past its time and, when too many are kept,
    /// the oldest.
    pub fn keep(&self, origin: &str, thumbprint: &str, nonce: &str) {
        if !is_token(nonce, NONCE_LIMIT) {
            return;
        }
        let mut kept = locked(&self.kept);
        kept.retain(|_, (_, at)| at.elapsed() < Duration::from_secs(NONCE_SECS));
        while kept.len() >= NONCES {
            let oldest = kept
                .iter()
                .min_by_key(|(_, (_, at))| *at)
                .map(|(key, _)| key.clone());
            match oldest {
                Some(key) => kept.remove(&key),
                None => break,
            };
        }
        kept.insert(
            (origin.to_owned(), thumbprint.to_owned()),
            (nonce.to_owned(), Instant::now()),
        );
    }
}

/// What renewing and revoking need from the adapter.
pub struct Renewer<'a> {
    pub(crate) outbound: &'a Outbound,
    pub(crate) policy: AddressPolicy,
    pub(crate) locks: &'a Locks,
    pub(crate) nonces: &'a Nonces,
}

impl Renewer<'_> {
    /// Renews the session in `slot` when it is due, holding the
    /// credential's lock throughout.
    pub fn renew(&self, slot: &dyn MaterialSlot) -> Result<Freshness, Outcome> {
        let lock = self.locks.of(slot.credential());
        let _held = locked(&lock);
        self.renew_held(slot)
    }

    fn renew_held(&self, slot: &dyn MaterialSlot) -> Result<Freshness, Outcome> {
        let read = slot.read()?;
        let material = Material::decode(&read, self.policy).ok_or(Outcome::Refused)?;
        let left = material.access_expires_at - now_secs();
        if left > MARGIN_SECS {
            return Ok(Freshness {
                renew_in_secs: u64::try_from(left - MARGIN_SECS).unwrap_or(0),
            });
        }
        // Nothing is spent yet: a server that cannot be read now is asked
        // again later.
        let endpoints = server::endpoints(self.outbound, &material.authserver).map_err(
            |outcome| match outcome {
                Outcome::RateLimited => Outcome::RateLimited,
                _ => Outcome::TemporarilyUnavailable,
            },
        )?;
        slot.mark_rotating(&read)?;
        let fields = [
            ("grant_type", "refresh_token"),
            ("refresh_token", material.refresh.expose()),
            ("client_id", material.client_id.as_str()),
        ];
        let thumbprint = material.key.thumbprint();
        let origin = endpoints.token.origin.as_str().to_owned();
        let url = endpoints.token.url();
        let mut nonce = self.nonces.get(&origin, &thumbprint);
        for attempt in 0..2 {
            let proof = match material.key.proof("POST", &url, nonce.as_deref()) {
                Ok(proof) => proof,
                Err(outcome) => {
                    slot.unmark_rotating();
                    return Err(outcome);
                }
            };
            let sent = self.outbound.send(
                &endpoints.token.origin,
                Request::new("POST", &endpoints.token.path)
                    .header("DPoP", proof)
                    .form(&fields),
            );
            let response = match sent {
                Ok(response) => response,
                // No connection opened: nothing reached the server.
                Err(Failure::Refused | Failure::Unreachable) => {
                    slot.unmark_rotating();
                    return Err(Outcome::TemporarilyUnavailable);
                }
                // The request may have reached it.
                Err(Failure::Interrupted) => return Err(Outcome::Unknown),
            };
            if let Some(fresh) = response.header("dpop-nonce") {
                self.nonces.keep(&origin, &thumbprint, fresh);
                if is_token(fresh, NONCE_LIMIT) {
                    nonce = Some(fresh.to_owned());
                }
            }
            let error = serde_json::from_slice::<Value>(&response.body)
                .ok()
                .and_then(|body| body["error"].as_str().map(str::to_owned));
            let challenged = matches!(response.status, 400 | 401)
                && matches!(
                    error.as_deref(),
                    Some("use_dpop_nonce" | "invalid_dpop_nonce")
                );
            if challenged {
                if attempt == 0 && nonce.is_some() {
                    continue;
                }
                // A second challenge spent nothing either.
                slot.unmark_rotating();
                return Err(Outcome::TemporarilyUnavailable);
            }
            return match response.status {
                200 => self.commit(slot, read, &material, &response),
                429 => {
                    slot.unmark_rotating();
                    Err(Outcome::RateLimited)
                }
                400..=499 => {
                    slot.unmark_rotating();
                    Err(Outcome::NeedsSignIn)
                }
                _ => Err(Outcome::Unknown),
            };
        }
        Err(Outcome::Unknown)
    }

    /// Keeps the new session a refresh answered, when the answer keeps the
    /// profile for the same account within the scope the session held.
    fn commit(
        &self,
        slot: &dyn MaterialSlot,
        read: lobby_contracts::SealedMaterial,
        material: &Material,
        response: &lobby_outbound::Response,
    ) -> Result<Freshness, Outcome> {
        let answer = json_object(response).map_err(|_| Outcome::Unknown)?;
        let tokens = tokens::checked(&answer, &material.scope()).map_err(|_| Outcome::Unknown)?;
        if tokens.subject != material.subject {
            return Err(Outcome::Unknown);
        }
        let renewed = Material {
            subject: material.subject.clone(),
            access: tokens.access,
            access_expires_at: now_secs()
                .saturating_add(i64::try_from(tokens.expires_in).unwrap_or(i64::MAX)),
            refresh: tokens.refresh,
            pds: material.pds.clone(),
            authserver: material.authserver.clone(),
            client_id: material.client_id.clone(),
            key: material.key.clone(),
            permissions: tokens.granted.clone(),
        };
        let update = MaterialUpdate::new(
            read,
            renewed.encode(),
            tokens.granted,
            &material.permissions,
        )
        .ok_or(Outcome::Unknown)?;
        slot.commit(update).map_err(|_| Outcome::Unknown)?;
        Ok(Freshness {
            renew_in_secs: tokens
                .expires_in
                .saturating_sub(u64::try_from(MARGIN_SECS).unwrap_or(0)),
        })
    }

    /// Revokes the session in `slot` at its authorization server, when the
    /// server offers revocation.
    pub fn revoke(&self, slot: &dyn MaterialSlot) -> Result<Revocation, Outcome> {
        let lock = self.locks.of(slot.credential());
        let _held = locked(&lock);
        let material = Material::decode(&slot.read()?, self.policy).ok_or(Outcome::Refused)?;
        let endpoints = server::endpoints(self.outbound, &material.authserver)?;
        let Some(revocation) = endpoints.revocation else {
            return Ok(Revocation::NotOffered);
        };
        let fields = [
            ("token", material.refresh.expose()),
            ("token_type_hint", "refresh_token"),
            ("client_id", material.client_id.as_str()),
        ];
        let thumbprint = material.key.thumbprint();
        let origin = revocation.origin.as_str().to_owned();
        let answered = proved_post(
            self.outbound,
            &revocation,
            &fields,
            &material.key,
            self.nonces.get(&origin, &thumbprint),
        )?;
        if let Some(nonce) = &answered.nonce {
            self.nonces.keep(&origin, &thumbprint, nonce);
        }
        match answered.response.status {
            200 => Ok(Revocation::Confirmed),
            status => Err(status_outcome(status)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonces_are_kept_per_origin_and_key_and_the_oldest_gives_way() {
        let nonces = Nonces::default();
        nonces.keep("https://as.test", "key-a", "n-1");
        assert_eq!(
            nonces.get("https://as.test", "key-a").as_deref(),
            Some("n-1")
        );
        assert_eq!(nonces.get("https://as.test", "key-b"), None, "another key");
        assert_eq!(
            nonces.get("https://pds.test", "key-a"),
            None,
            "another server"
        );
        nonces.keep("https://as.test", "key-a", "bad nonce");
        assert_eq!(
            nonces.get("https://as.test", "key-a").as_deref(),
            Some("n-1"),
            "a nonce of the wrong shape is never kept"
        );
        for n in 0..NONCES {
            nonces.keep("https://as.test", &format!("other-{n}"), "n");
        }
        assert_eq!(
            nonces.get("https://as.test", "key-a"),
            None,
            "the oldest gave way"
        );
        assert_eq!(locked(&nonces.kept).len(), NONCES);
    }

    #[test]
    fn one_credential_has_one_lock() {
        let locks = Locks::default();
        let first = CredentialId::from_u128(1);
        assert!(Arc::ptr_eq(&locks.of(first), &locks.of(first)));
        assert!(!Arc::ptr_eq(
            &locks.of(first),
            &locks.of(CredentialId::from_u128(2))
        ));
    }
}
