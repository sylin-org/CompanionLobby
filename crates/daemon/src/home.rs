//! The data directory.
//!
//! This module will resolve where the daemon keeps its files:
//!
//! * Windows: the Local AppData known folder, resolved with
//!   `SHGetKnownFolderPath` and never from environment variables, followed by
//!   `CompanionLobby`;
//! * macOS: `~/Library/Application Support/CompanionLobby`;
//! * elsewhere: `$XDG_STATE_HOME/companion-lobby`;
//! * `LOBBY_DATA_DIR` overrides all of them.
//!
//! Any path whose final component is `.companion-lobby` is refused outright and
//! never read. The directory is created with an owner-only ACL.
