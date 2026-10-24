//! The compiled-in adapters.
//!
//! The registry lists every credential adapter and service adapter the
//! daemon runs with. It is the only place in the daemon that names a provider
//! or a service; everything else looks adapters up here by provider kind or
//! service name, and a credential or connection whose adapter is not here
//! stays in the state file, unused, and reaches no catalog.
//!
//! [`Registry::product`] is what `companion-lobby` runs with: the adapters
//! each slice adds here, one line each. The credential adapter reaches the
//! public network only; [`AUTHSERVER_ENV`] may name another public HTTPS
//! origin for the authorization server a sign-in without a handle goes to,
//! and a value that is not one stops the daemon from starting rather than
//! sending a sign-in anywhere else. The test-only
//! `companion-lobby-synthetic` command composes its own registry from the
//! test kit's passbook credential adapter and board service adapter, so the
//! host is proven through adapters whose words belong to no real provider or
//! service; this crate's library never names them.

use std::sync::Arc;

use lobby_contracts::{CredentialAdapter, ProviderKind, ServiceAdapter, ServiceName};

/// The environment variable that names the default authorization server.
pub const AUTHSERVER_ENV: &str = "LOBBY_AUTHSERVER";

/// The adapters one daemon runs with.
#[derive(Clone, Default)]
pub struct Registry {
    credentials: Vec<Arc<dyn CredentialAdapter>>,
    services: Vec<Arc<dyn ServiceAdapter>>,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field(
                "credentials",
                &self
                    .credentials
                    .iter()
                    .map(|adapter| adapter.kind())
                    .collect::<Vec<_>>(),
            )
            .field(
                "services",
                &self
                    .services
                    .iter()
                    .map(|adapter| adapter.name())
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl Registry {
    /// A registry of exactly these adapters.
    pub fn new(
        credentials: Vec<Arc<dyn CredentialAdapter>>,
        services: Vec<Arc<dyn ServiceAdapter>>,
    ) -> Self {
        Self {
            credentials,
            services,
        }
    }

    /// The adapters the product runs with.
    pub fn product() -> Result<Self, String> {
        let authserver = std::env::var(AUTHSERVER_ENV).ok();
        let world = lobby_credential_atproto::World::public(authserver.as_deref())
            .map_err(|problem| format!("{AUTHSERVER_ENV}: {problem}"))?;
        Ok(Self::new(
            vec![Arc::new(lobby_credential_atproto::Atproto::new(world))],
            Vec::new(),
        ))
    }

    /// The credential adapter of `kind`.
    pub fn credential(&self, kind: &ProviderKind) -> Option<Arc<dyn CredentialAdapter>> {
        self.credentials
            .iter()
            .find(|adapter| adapter.kind() == *kind)
            .cloned()
    }

    /// The service adapter named `name`.
    pub fn service(&self, name: &ServiceName) -> Option<Arc<dyn ServiceAdapter>> {
        self.services
            .iter()
            .find(|adapter| adapter.name() == *name)
            .cloned()
    }
}
