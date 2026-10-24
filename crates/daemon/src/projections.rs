//! Read models derived from the state file and the journal.
//!
//! This module will build, and rebuild on every event:
//!
//! * the manager page's cards and sections;
//! * each attachment's catalog: `reach` plus the native tools of the
//!   companion's ready connections as `<service>_<tool>`, with an `at` argument
//!   carrying sponsor-authored labels for services with many addresses;
//! * the live feed the manager page follows.
//!
//! Projections hold no truth and can always be rebuilt.
