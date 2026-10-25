//! The `companion-lobby-synthetic` command, for tests only.
//!
//! It is `companion-lobby` with the test kit's synthetic adapters in its
//! registry: the passbook credential adapter and the board service adapter,
//! whose words belong to no real provider or service. When `LOBBY_AUTHSERVER`
//! names a loopback origin, the atproto credential adapter joins them,
//! resolving everything there, where the test kit's fake world stands; with
//! nothing named it is left out, so no test can reach the public network.
//! It is built only with the `synthetic` feature, which only the test builds
//! turn on, and the shim it runs starts its daemon from this same executable.

use std::process::ExitCode;
use std::sync::Arc;

use lobby_contracts::CredentialAdapter;
use lobby_credential_atproto::{Atproto, World};
use lobby_daemon::cli;
use lobby_daemon::registry::{AUTHSERVER_ENV, Registry};
use lobby_testkit::board::Board;
use lobby_testkit::passbook::{self, Passbook};

fn synthetic() -> Result<Registry, String> {
    let mut credentials: Vec<Arc<dyn CredentialAdapter>> = vec![Arc::new(Passbook::new())];
    if let Ok(origin) = std::env::var(AUTHSERVER_ENV) {
        let world = World::loopback(&origin)
            .ok_or_else(|| format!("{AUTHSERVER_ENV} must be a loopback origin here"))?;
        credentials.push(Arc::new(Atproto::new(world)));
    }
    Ok(Registry::new(credentials, vec![Arc::new(Board::new())])
        .titled(passbook::KIND, passbook::TITLE)
        .titled(
            lobby_credential_atproto::KIND,
            lobby_credential_atproto::TITLE,
        ))
}

fn main() -> ExitCode {
    cli::main(synthetic)
}
