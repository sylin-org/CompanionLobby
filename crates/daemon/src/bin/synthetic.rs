//! The `companion-lobby-synthetic` command, for tests only.
//!
//! It is `companion-lobby` with the test kit's synthetic adapters in its
//! registry: the passbook credential adapter and the board service adapter,
//! whose words belong to no real provider or service. When `LOBBY_AUTHSERVER`
//! names a loopback origin, the atproto credential adapter joins them,
//! resolving everything there, where the test kit's fake world stands, and
//! so does the Tangent service adapter, for the test kit's fake Space; with
//! nothing named both are left out, so no test can reach the public network.
//! It is built only with the `synthetic` feature, which only the test builds
//! turn on, and the shim it runs starts its daemon from this same executable.

use std::process::ExitCode;
use std::sync::Arc;

use lobby_contracts::{CredentialAdapter, ServiceAdapter};
use lobby_credential_atproto::{Atproto, World};
use lobby_daemon::cli;
use lobby_daemon::registry::{AUTHSERVER_ENV, Registry};
use lobby_testkit::board::Board;
use lobby_testkit::passbook::{self, Passbook};

fn synthetic() -> Result<Registry, String> {
    let mut credentials: Vec<Arc<dyn CredentialAdapter>> = vec![Arc::new(Passbook::new())];
    let mut services: Vec<Arc<dyn ServiceAdapter>> = vec![Arc::new(Board::new())];
    if let Ok(origin) = std::env::var(AUTHSERVER_ENV) {
        let world = World::loopback(&origin)
            .ok_or_else(|| format!("{AUTHSERVER_ENV} must be a loopback origin here"))?;
        credentials.push(Arc::new(Atproto::new(world)));
        services.push(Arc::new(Registry::tangent()));
    }
    Ok(Registry::new(credentials, services)
        .service_titled(lobby_service_tangent::NAME, lobby_service_tangent::TITLE)
        .titled(passbook::KIND, passbook::TITLE)
        .hinted(passbook::KIND, passbook::HINT)
        .titled(
            lobby_credential_atproto::KIND,
            lobby_credential_atproto::TITLE,
        )
        .hinted(
            lobby_credential_atproto::KIND,
            lobby_credential_atproto::HINT,
        ))
}

fn main() -> ExitCode {
    cli::main(synthetic)
}
