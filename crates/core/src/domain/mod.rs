//! Domain vocabulary for the Tangent connector. Pure types and policies: no I/O, no adapter
//! knowledge, no secrets. The application hub composes these; adapters move bytes.

pub mod attention;
pub mod events;
pub mod companion;
pub mod intake;
pub mod policy;
pub mod refs;
pub mod writes;

/// A short-lived presentational identifier (milliseconds since the Unix epoch is fine for
/// ordering; wall-clock formatting happens only in presentation).
pub fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or(0)
}
