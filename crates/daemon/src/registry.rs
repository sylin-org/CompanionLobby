//! The compiled-in adapters.
//!
//! The registry lists every credential adapter and service adapter the
//! daemon runs with. It is the only place in the daemon that names a provider
//! or a service; everything else looks adapters up here by provider kind or
//! service name, and a credential or connection whose adapter is not here
//! stays in the state file, unused, and reaches no catalog. Beside each
//! provider kind it keeps the title the sponsor reads on the manager page,
//! as the kind's own adapter names it; a kind without one is shown by its
//! name.
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
    titles: Vec<(ProviderKind, String)>,
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
            titles: Vec::new(),
        }
    }

    /// The same registry, showing the provider kind `kind` to the sponsor
    /// as `title`.
    #[must_use]
    pub fn titled(mut self, kind: &str, title: &str) -> Self {
        if let Some(kind) = ProviderKind::new(kind) {
            self.titles.retain(|(titled, _)| *titled != kind);
            self.titles.push((kind, title.to_owned()));
        }
        self
    }

    /// The title the sponsor reads for the provider kind `kind`.
    pub fn title(&self, kind: &ProviderKind) -> String {
        self.titles
            .iter()
            .find(|(titled, _)| titled == kind)
            .map_or_else(|| capitalized(kind.as_str()), |(_, title)| title.clone())
    }

    /// The name of every service adapter, in the order listed, with the
    /// provider kinds it accepts.
    pub fn services(&self) -> Vec<(ServiceName, Vec<ProviderKind>)> {
        self.services
            .iter()
            .map(|adapter| (adapter.name(), adapter.describe().accepts))
            .collect()
    }

    /// The adapters the product runs with.
    pub fn product() -> Result<Self, String> {
        let authserver = std::env::var(AUTHSERVER_ENV).ok();
        let world = lobby_credential_atproto::World::public(authserver.as_deref())
            .map_err(|problem| format!("{AUTHSERVER_ENV}: {problem}"))?;
        Ok(Self::new(
            vec![Arc::new(lobby_credential_atproto::Atproto::new(world))],
            Vec::new(),
        )
        .titled(
            lobby_credential_atproto::KIND,
            lobby_credential_atproto::TITLE,
        ))
    }

    /// The provider kinds of every credential adapter, in the order listed.
    pub fn credential_kinds(&self) -> Vec<ProviderKind> {
        self.credentials
            .iter()
            .map(|adapter| adapter.kind())
            .collect()
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

/// `name` with its first letter in capitals, as the sponsor reads a service
/// or a provider kind that names no title of its own.
pub fn capitalized(name: &str) -> String {
    let mut letters = name.chars();
    letters.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(letters).collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kind_reads_as_its_title_or_its_capitalized_name() {
        let registry = Registry::default().titled("passbook", "Pass book");
        let kind = |name: &str| ProviderKind::new(name).expect("kind");
        assert_eq!(registry.title(&kind("passbook")), "Pass book");
        assert_eq!(registry.title(&kind("quillbook")), "Quillbook");
        assert_eq!(capitalized("board"), "Board");
        assert_eq!(capitalized(""), "");
    }
}
