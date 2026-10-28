//! The compiled-in adapters.
//!
//! The registry lists every credential adapter and service adapter the
//! daemon runs with. It is the only place in the daemon that names a provider
//! or a service; everything else looks adapters up here by provider kind or
//! service name, and a credential or connection whose adapter is not here
//! stays in the state file, unused, and reaches no catalog. Beside each
//! provider kind it keeps the title the sponsor reads on the manager page,
//! as the kind's own adapter names it, and the hint beside the field where
//! the sponsor names the account to sign in with; a kind without a title is
//! shown by its name, and one without a hint gets a plain one.
//!
//! [`Registry::product`] is what `companion-lobby` runs with: the atproto
//! credential adapter, the Tangent service adapter and the Bluesky service
//! adapter, which both take atproto accounts and are shown as their makers
//! name them; one atproto account serves both. The credential adapter reaches the
//! public network only; [`AUTHSERVER_ENV`] may name another public HTTPS
//! origin for the authorization server a sign-in without a handle goes to,
//! and a value that is not one stops the daemon from starting rather than
//! sending a sign-in anywhere else. The test-only
//! `companion-lobby-synthetic` command composes its own registry from the
//! test kit's passbook credential adapter and board service adapter, so the
//! host is proven through adapters whose words belong to no real provider or
//! service; this crate's library never names them.

use std::sync::Arc;

use lobby_contracts::{
    AddressPolicy, CredentialAdapter, ProviderKind, ServiceAdapter, ServiceDescription, ServiceName,
};

/// The environment variable that names the default authorization server.
pub const AUTHSERVER_ENV: &str = "LOBBY_AUTHSERVER";

/// The adapters one daemon runs with.
#[derive(Clone, Default)]
pub struct Registry {
    credentials: Vec<Arc<dyn CredentialAdapter>>,
    services: Vec<Arc<dyn ServiceAdapter>>,
    titles: Vec<(ProviderKind, String)>,
    hints: Vec<(ProviderKind, String)>,
    service_titles: Vec<(ServiceName, String)>,
    service_lets: Vec<(ServiceName, String)>,
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
            hints: Vec::new(),
            service_titles: Vec::new(),
            service_lets: Vec::new(),
        }
    }

    /// The same registry, saying what signing an account in again for the
    /// service `name` lets a companion do, as a clause after "can", such as
    /// `post to Bluesky`.
    #[must_use]
    pub fn lets(mut self, name: &str, clause: &str) -> Self {
        if let Some(name) = ServiceName::new(name) {
            self.service_lets.retain(|(named, _)| *named != name);
            self.service_lets.push((name, clause.to_owned()));
        }
        self
    }

    /// What signing an account in again for the service `name` lets a
    /// companion do: its own clause, or using it by its title.
    pub fn lets_for(&self, name: &ServiceName) -> String {
        self.service_lets
            .iter()
            .find(|(named, _)| named == name)
            .map_or_else(
                || format!("use {}", self.service_title(name)),
                |(_, clause)| clause.clone(),
            )
    }

    /// The same registry, showing the service `name` to the sponsor and to
    /// a mind as `title`, the name its own makers give it.
    #[must_use]
    pub fn service_titled(mut self, name: &str, title: &str) -> Self {
        if let Some(name) = ServiceName::new(name) {
            self.service_titles.retain(|(titled, _)| *titled != name);
            self.service_titles.push((name, title.to_owned()));
        }
        self
    }

    /// The title of the service `name`: its own, or its capitalized name.
    pub fn service_title(&self, name: &ServiceName) -> String {
        self.service_titles
            .iter()
            .find(|(titled, _)| titled == name)
            .map_or_else(|| capitalized(name.as_str()), |(_, title)| title.clone())
    }

    /// What the services that accept the provider kind `kind` need an
    /// account of it granted, each once, in the order the services and
    /// their declarations list them.
    pub fn permissions(&self, kind: &ProviderKind) -> Vec<String> {
        let mut permissions: Vec<String> = Vec::new();
        for adapter in &self.services {
            let description = adapter.describe();
            if !description.accepts.contains(kind) {
                continue;
            }
            for permission in description.permissions {
                if !permissions.contains(&permission) {
                    permissions.push(permission);
                }
            }
        }
        permissions
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

    /// The same registry, showing `hint` beside the field where the sponsor
    /// names an account of the provider kind `kind`.
    #[must_use]
    pub fn hinted(mut self, kind: &str, hint: &str) -> Self {
        if let Some(kind) = ProviderKind::new(kind) {
            self.hints.retain(|(hinted, _)| *hinted != kind);
            self.hints.push((kind, hint.to_owned()));
        }
        self
    }

    /// What the sponsor reads beside the field naming an account of the
    /// provider kind `kind`.
    pub fn hint(&self, kind: &ProviderKind) -> String {
        self.hints
            .iter()
            .find(|(hinted, _)| hinted == kind)
            .map_or_else(
                || "The account to sign in with, as its provider names it.".to_owned(),
                |(_, hint)| hint.clone(),
            )
    }

    /// The title the sponsor reads for the provider kind `kind`.
    pub fn title(&self, kind: &ProviderKind) -> String {
        self.titles
            .iter()
            .find(|(titled, _)| titled == kind)
            .map_or_else(|| capitalized(kind.as_str()), |(_, title)| title.clone())
    }

    /// The name of every service adapter, in the order listed, with what it
    /// accepts.
    pub fn services(&self) -> Vec<(ServiceName, ServiceDescription)> {
        self.services
            .iter()
            .map(|adapter| (adapter.name(), adapter.describe()))
            .collect()
    }

    /// The adapters the product runs with.
    pub fn product() -> Result<Self, String> {
        let authserver = std::env::var(AUTHSERVER_ENV).ok();
        let world = lobby_credential_atproto::World::public(authserver.as_deref())
            .map_err(|problem| format!("{AUTHSERVER_ENV}: {problem}"))?;
        Ok(Self::new(
            vec![Arc::new(lobby_credential_atproto::Atproto::new(world))],
            vec![
                Arc::new(Self::tangent()),
                Arc::new(Self::bluesky(AddressPolicy::PublicHttps)),
            ],
        )
        .service_titled(lobby_service_tangent::NAME, lobby_service_tangent::TITLE)
        .service_titled(lobby_service_bluesky::NAME, lobby_service_bluesky::TITLE)
        .lets(lobby_service_bluesky::NAME, lobby_service_bluesky::LETS)
        .titled(
            lobby_credential_atproto::KIND,
            lobby_credential_atproto::TITLE,
        )
        .hinted(
            lobby_credential_atproto::KIND,
            lobby_credential_atproto::HINT,
        ))
    }

    /// The Tangent service adapter, taking atproto accounts: the one line
    /// that composes the two.
    pub fn tangent() -> lobby_service_tangent::Tangent {
        lobby_service_tangent::Tangent::accepting(
            ProviderKind::new(lobby_credential_atproto::KIND)
                .unwrap_or_else(|| unreachable!("atproto is a provider kind")),
        )
    }

    /// The Bluesky service adapter, taking atproto accounts whose server
    /// `policy` admits: the one line that composes the two.
    pub fn bluesky(policy: AddressPolicy) -> lobby_service_bluesky::Bluesky {
        lobby_service_bluesky::Bluesky::accepting(
            ProviderKind::new(lobby_credential_atproto::KIND)
                .unwrap_or_else(|| unreachable!("atproto is a provider kind")),
            policy,
        )
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
        let registry = registry.hinted("passbook", "The authority's address.");
        assert_eq!(registry.hint(&kind("passbook")), "The authority's address.");
        assert_eq!(
            registry.hint(&kind("quillbook")),
            "The account to sign in with, as its provider names it."
        );
        assert_eq!(capitalized(""), "");
        let titled = registry.service_titled("board", "The Board");
        assert_eq!(
            titled.service_title(&ServiceName::new("board").expect("name")),
            "The Board"
        );
        assert_eq!(
            titled.service_title(&ServiceName::new("quill").expect("name")),
            "Quill"
        );
    }

    #[test]
    fn a_kind_is_asked_for_what_every_service_taking_it_needs_once() {
        let product = Registry::default().service_titled("x", "X");
        assert!(
            product
                .permissions(&ProviderKind::new("atproto").expect("kind"))
                .is_empty()
        );
        let registry = Registry::new(
            Vec::new(),
            vec![Arc::new(Registry::tangent()), Arc::new(Registry::tangent())],
        );
        assert_eq!(
            registry.permissions(&ProviderKind::new(lobby_credential_atproto::KIND).expect("kind")),
            [lobby_service_tangent::PERMISSION]
        );
        let both = Registry::new(
            Vec::new(),
            vec![
                Arc::new(Registry::tangent()),
                Arc::new(Registry::bluesky(AddressPolicy::PublicHttps)),
            ],
        );
        let asked =
            both.permissions(&ProviderKind::new(lobby_credential_atproto::KIND).expect("kind"));
        assert_eq!(asked[0], lobby_service_tangent::PERMISSION);
        assert_eq!(&asked[1..], lobby_service_bluesky::PERMISSIONS);
        assert!(
            registry
                .permissions(&ProviderKind::new("passbook").expect("kind"))
                .is_empty()
        );
    }
}
