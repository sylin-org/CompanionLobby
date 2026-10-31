//! A Space's authentication profile, read as untrusted input.
//!
//! `GET /.well-known/tangent-auth` says how an account proves itself to the
//! Space. Before the lobby asks the account for anything, the profile must
//! keep to the Space's reviewed address in every particular, so that a
//! hostile or misconfigured profile can neither steer the proof to another
//! audience nor the exchange to another endpoint:
//!
//! * its `canonicalOrigin` is exactly the connection's address;
//! * the proof's `method` and `purpose` are both [`EXCHANGE_METHOD`];
//! * the exchange `endpoint` is the address followed by [`EXCHANGE_PATH`];
//! * the proof travels in the `Authorization` header;
//! * the `audience` is the one Tangent derives from the address, at most
//!   [`AUDIENCE_LIMIT`] bytes;
//! * the algorithms it names are ES256 or ES256K, and there is at least one.
//!
//! The audience is derived the way Tangent derives its public DID: a
//! loopback origin (`localhost`, `127.x`, `[::1]`) is
//! `did:web:localhost`, and a DNS name with a dot is `did:web:<host>`, each
//! with `%3A<port>` when the port is not the scheme's default; anything else
//! has no derived DID, and a Space there cannot be reached. Either way the
//! audience names the `#tangent` service.

use lobby_contracts::Address;
use serde::Deserialize;

/// The well-known path of the authentication profile.
pub const PROFILE_PATH: &str = "/.well-known/tangent-auth";
/// The identity exchange's path.
pub const EXCHANGE_PATH: &str = "/api/v1/identity/exchange";
/// The method, and the purpose, of the proof the exchange takes.
pub const EXCHANGE_METHOD: &str = "org.sylin.tangent.exchangeIdentity";
/// The longest audience accepted, in bytes.
pub const AUDIENCE_LIMIT: usize = 512;

/// The profile as the Space publishes it.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    #[serde(default)]
    canonical_origin: String,
    #[serde(default)]
    service_proof: Option<ServiceProof>,
}

/// The proof a profile says the exchange takes.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceProof {
    #[serde(default)]
    audience: String,
    #[serde(default)]
    method: String,
    #[serde(default)]
    purpose: String,
    #[serde(default)]
    endpoint: String,
    #[serde(default)]
    transport: String,
    #[serde(default)]
    algorithms: Vec<String>,
}

impl Profile {
    /// The audience the proof is for, when the profile keeps to `address`
    /// in every particular.
    pub fn audience_for(&self, address: &Address) -> Option<String> {
        let origin = address.as_str();
        let proof = self.service_proof.as_ref()?;
        let derived = derived_audience(address)?;
        let keeps = self.canonical_origin == origin
            && proof.method == EXCHANGE_METHOD
            && proof.purpose == EXCHANGE_METHOD
            && proof.endpoint == address.url(EXCHANGE_PATH)
            && proof.transport == "authorization_header"
            && proof.audience == derived
            && proof.audience.len() <= AUDIENCE_LIMIT
            && !proof.algorithms.is_empty()
            && proof
                .algorithms
                .iter()
                .all(|algorithm| algorithm == "ES256" || algorithm == "ES256K");
        keeps.then(|| proof.audience.clone())
    }
}

/// The audience Tangent derives from `address`, naming its `#tangent`
/// service, when it derives one.
pub fn derived_audience(address: &Address) -> Option<String> {
    let host = address.host();
    let default_port = if address.is_https() { 443 } else { 80 };
    let loopback = host == "localhost" || host.starts_with("127.") || host == "[::1]";
    let named = if loopback {
        "localhost"
    } else if host.contains('.')
        && !host.starts_with('[')
        && !host
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
    {
        host
    } else {
        return None;
    };
    Some(if address.port() == default_port {
        format!("did:web:{named}#tangent")
    } else {
        format!("did:web:{named}%3A{}#tangent", address.port())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lobby_contracts::AddressPolicy;

    fn address(text: &str) -> Address {
        Address::parse(text, AddressPolicy::LoopbackAllowed).expect("an address")
    }

    fn profile(origin: &str, audience: &str) -> Profile {
        Profile {
            canonical_origin: origin.to_owned(),
            service_proof: Some(ServiceProof {
                audience: audience.to_owned(),
                method: EXCHANGE_METHOD.to_owned(),
                purpose: EXCHANGE_METHOD.to_owned(),
                endpoint: format!("{origin}{EXCHANGE_PATH}"),
                transport: "authorization_header".to_owned(),
                algorithms: vec!["ES256".to_owned()],
            }),
        }
    }

    #[test]
    fn a_profile_cannot_redirect_the_proof_or_the_exchange() {
        let space = address("https://space.example.test");
        let origin = space.as_str();
        let good = profile(origin, "did:web:space.example.test#tangent");
        assert_eq!(
            good.audience_for(&space).as_deref(),
            Some("did:web:space.example.test#tangent")
        );
        assert_eq!(
            good.audience_for(&address("https://other.example.test")),
            None
        );

        let mut bare = good.clone();
        bare.service_proof.as_mut().expect("proof").audience =
            "did:web:space.example.test".to_owned();
        assert_eq!(bare.audience_for(&space), None);

        let mut elsewhere = good.clone();
        elsewhere.service_proof.as_mut().expect("proof").endpoint =
            format!("https://other.example.test{EXCHANGE_PATH}");
        assert_eq!(elsewhere.audience_for(&space), None);

        let mut victim = good.clone();
        victim.service_proof.as_mut().expect("proof").audience =
            "did:web:victim.example.test#tangent".to_owned();
        assert_eq!(victim.audience_for(&space), None);

        let mut configured = good.clone();
        configured.service_proof.as_mut().expect("proof").audience =
            "did:plc:aaaaaaaaaaaaaaaaaaaaaaaa#tangent".to_owned();
        assert_eq!(configured.audience_for(&space), None);

        let mut method = good.clone();
        method.service_proof.as_mut().expect("proof").method =
            "com.atproto.simplespace.checkUserAccess".to_owned();
        assert_eq!(method.audience_for(&space), None);

        let mut body = good.clone();
        body.service_proof.as_mut().expect("proof").transport = "body".to_owned();
        assert_eq!(body.audience_for(&space), None);

        for algorithms in [vec![], vec!["HS256".to_owned()]] {
            let mut weak = good.clone();
            weak.service_proof.as_mut().expect("proof").algorithms = algorithms;
            assert_eq!(weak.audience_for(&space), None);
        }

        let mut renamed = good.clone();
        renamed.canonical_origin = "https://space.example.test/".to_owned();
        assert_eq!(renamed.audience_for(&space), None);
        assert_eq!(Profile::default().audience_for(&space), None);
    }

    #[test]
    fn a_space_on_a_public_port_of_its_own_keeps_to_its_address() {
        let space = address("https://space.example.test:8443");
        let good = profile(space.as_str(), "did:web:space.example.test%3A8443#tangent");
        assert_eq!(
            good.audience_for(&space).as_deref(),
            Some("did:web:space.example.test%3A8443#tangent")
        );
        let portless = profile(space.as_str(), "did:web:space.example.test#tangent");
        assert_eq!(portless.audience_for(&space), None);
    }

    #[test]
    fn the_audience_is_derived_from_the_reviewed_address() {
        assert_eq!(
            derived_audience(&address("http://127.0.0.1:5220")).as_deref(),
            Some("did:web:localhost%3A5220#tangent")
        );
        assert_eq!(
            derived_audience(&address("http://localhost")).as_deref(),
            Some("did:web:localhost#tangent")
        );
        assert_eq!(
            derived_audience(&address("https://space.example.test")).as_deref(),
            Some("did:web:space.example.test#tangent")
        );
        assert_eq!(
            derived_audience(&address("https://space.example.test:8443")).as_deref(),
            Some("did:web:space.example.test%3A8443#tangent")
        );
        assert_eq!(derived_audience(&address("https://1.1.1.1")), None);
    }
}
