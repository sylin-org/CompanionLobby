//! `companion-lobby stop`.
//!
//! Verifies that the daemon on the port serves this data directory, sends it a
//! same-origin stop request, and waits until it has drained its accepted
//! requests and let go of the daemon lock, so a rebuild can replace the
//! executable as soon as this command returns.

use crate::contact::{self, Probe};
use crate::{home, http};

/// Runs `companion-lobby stop`, returning what to tell the sponsor.
pub fn run() -> Result<String, String> {
    let dir = home::resolve().map_err(|problem| problem.to_string())?;
    let port = http::resolve_port()?;
    match contact::probe(port) {
        Probe::Nothing => {
            return Ok(format!(
                "no Companion Lobby daemon answers on 127.0.0.1:{port}"
            ));
        }
        Probe::Foreign => {
            return Err(format!(
                "the program on 127.0.0.1:{port} is not Companion Lobby; nothing was stopped"
            ));
        }
        Probe::Lobby(document) => {
            contact::verify(&dir, port, &document)?;
        }
    }
    contact::request_stop(port)?;
    if contact::wait_released(&dir, contact::STOP_WAIT) {
        Ok(format!(
            "the Companion Lobby daemon on 127.0.0.1:{port} stopped"
        ))
    } else {
        Err(format!(
            "the daemon on 127.0.0.1:{port} accepted the stop but has not exited yet"
        ))
    }
}
