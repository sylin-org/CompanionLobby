//! The background-check spoke: an ordinary-code polling loop per auto-check companion.
//! It fetches digests, persists state and publishes events. It never invokes a model, and a
//! host never sees anything from it except through later tool responses. Failures back off
//! exponentially with a cap; empty and unchanged checks are silent successes.

use std::sync::Arc;
use std::time::Duration;

use crate::application::hub::ConnectorHub;
use companion_core::domain::events::DomainEvent;

const BASE_BACKOFF_SECONDS: u64 = 15;
const MAX_BACKOFF_SECONDS: u64 = 600;

/// Computes the backoff after `failures` consecutive failures: 15s doubling, capped.
pub fn backoff_seconds(failures: u32) -> u64 {
    BASE_BACKOFF_SECONDS
        .saturating_mul(1u64.checked_shl(failures.saturating_sub(1).min(8)).unwrap_or(u64::MAX / 2))
        .min(MAX_BACKOFF_SECONDS)
}

/// Spawns one named checker thread per auto-check companion. The thread lives exactly as
/// long as this process: a host-owned stdio server checks only while it runs.
pub fn spawn_checkers(hub: Arc<ConnectorHub>, companions: Vec<String>, poll_seconds: u64) {
    for enrollment_id in companions {
        let hub = hub.clone();
        std::thread::Builder::new()
            .name(format!("companion-check-{enrollment_id}"))
            .spawn(move || run_checker(hub, enrollment_id, poll_seconds))
            .expect("checker thread");
    }
}

fn run_checker(hub: Arc<ConnectorHub>, enrollment_id: String, poll_seconds: u64) {
    let mut failures: u32 = 0;
    loop {
        let sleep = if failures == 0 { poll_seconds } else { backoff_seconds(failures) };
        std::thread::sleep(Duration::from_secs(sleep));
        match hub.background_check(&enrollment_id) {
            Ok(_summary) => failures = 0,
            Err(reason) => {
                failures += 1;
                hub.events().publish(DomainEvent::PollFailed {
                    enrollment_id: enrollment_id.clone(),
                    attempt: failures,
                    reason: reason.clone(),
                });
                let backoff = backoff_seconds(failures);
                hub.events().publish(DomainEvent::BackoffScheduled {
                    enrollment_id: enrollment_id.clone(),
                    seconds: backoff,
                });
                // Credential trouble never enters a tight retry loop: cap the attempts rate.
                if reason.contains("credential") || reason.contains("rejected") {
                    failures = failures.max(6);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_and_caps() {
        assert_eq!(backoff_seconds(1), 15);
        assert_eq!(backoff_seconds(2), 30);
        assert_eq!(backoff_seconds(3), 60);
        assert_eq!(backoff_seconds(9), 600);
        assert_eq!(backoff_seconds(50), 600);
    }
}
