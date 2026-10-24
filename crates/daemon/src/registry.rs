//! The compiled-in adapters.
//!
//! The registry lists every credential adapter and service adapter the
//! daemon runs with. It is the only place in the daemon that names a provider
//! or a service; everything else looks adapters up here by provider kind or
//! service name, and a credential or connection whose adapter is not here
//! stays in the state file, unused, and reaches no catalog.
//!
//! The product ships with the adapters each later slice adds here, one line
//! each. Built with the `synthetic` feature, which only the test builds turn
//! on, the registry also holds the test kit's passbook credential adapter and
//! board service adapter, so the host is proven through adapters whose words
//! belong to no real provider or service.

use std::sync::Arc;

use lobby_contracts::{CredentialAdapter, ProviderKind, ServiceAdapter, ServiceName};

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

    /// The adapters this build of the daemon runs with.
    pub fn compiled_in() -> Self {
        #[cfg(feature = "synthetic")]
        {
            Self::new(
                vec![Arc::new(lobby_testkit::passbook::Passbook::new())],
                vec![Arc::new(lobby_testkit::board::Board::new())],
            )
        }
        #[cfg(not(feature = "synthetic"))]
        {
            Self::default()
        }
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
