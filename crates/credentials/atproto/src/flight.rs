//! Sign-ins the adapter has pushed and waits to see come back.
//!
//! A flight is parked under the OAuth `state` the lobby minted, in this
//! process's memory only: the PKCE verifier and the DPoP key never touch a
//! file until the sign-in completes. A flight is used once, lives
//! [`TIME_LIMIT`] at most (the authorization server parks a pushed request
//! for about five minutes; ten is the sponsor's whole budget), and at most
//! [`WAITING`] wait at once, the oldest giving way.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use lobby_contracts::{Address, Outcome, Secret};

use crate::dpop::DpopKey;
use crate::wire::Endpoint;

/// How long a pushed sign-in may take to come back.
pub const TIME_LIMIT: Duration = Duration::from_secs(10 * 60);
/// How many sign-ins wait at once.
pub const WAITING: usize = 32;

/// What completing one sign-in needs.
pub struct Flight {
    pub(crate) started: Instant,
    pub(crate) verifier: Secret,
    pub(crate) key: DpopKey,
    pub(crate) authserver: Address,
    pub(crate) token: Endpoint,
    pub(crate) client_id: String,
    pub(crate) redirect_uri: String,
    pub(crate) scope: String,
    /// The account the sponsor named and the PDS its document named, when
    /// the sponsor named one: the exchange must sign in that account.
    pub(crate) expected: Option<(String, Address)>,
    /// The authorization server's last DPoP nonce.
    pub(crate) nonce: Option<String>,
}

/// The parked flights.
pub struct Flights {
    parked: Mutex<HashMap<String, Flight>>,
    time_limit: Duration,
}

impl Flights {
    /// No flights, each allowed `time_limit`.
    pub fn new(time_limit: Duration) -> Self {
        Self {
            parked: Mutex::new(HashMap::new()),
            time_limit,
        }
    }

    fn parked(&self) -> MutexGuard<'_, HashMap<String, Flight>> {
        self.parked.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether a flight waits under `state`.
    pub fn waits(&self, state: &str) -> bool {
        self.parked().contains_key(state)
    }

    /// Parks `flight` under `state`, after letting go of every flight past
    /// its time and, when too many wait, the oldest.
    pub fn park(&self, state: &str, flight: Flight) -> Result<(), Outcome> {
        let mut parked = self.parked();
        let limit = self.time_limit;
        parked.retain(|_, flight| flight.started.elapsed() < limit);
        if parked.contains_key(state) {
            return Err(Outcome::Conflict);
        }
        while parked.len() >= WAITING {
            let oldest = parked
                .iter()
                .min_by_key(|(_, flight)| flight.started)
                .map(|(state, _)| state.clone());
            match oldest {
                Some(state) => parked.remove(&state),
                None => break,
            };
        }
        parked.insert(state.to_owned(), flight);
        Ok(())
    }

    /// Takes the flight parked under `state`, once, while it is within its
    /// time.
    pub fn take(&self, state: &str) -> Option<Flight> {
        let flight = self.parked().remove(state)?;
        (flight.started.elapsed() < self.time_limit).then_some(flight)
    }
}
