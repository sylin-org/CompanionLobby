//! The credential adapter suite.
//!
//! [`run`] drives one credential adapter against its provider's fake, in
//! process, through every promise of the contract:
//!
//! * a sign-in records the account the provider authenticated, whatever the
//!   sponsor typed, with permissions that follow the credential's rule, and
//!   a return carrying another `state` is refused;
//! * every ask the adapter supports is answered from the material in the
//!   slot, and the authorization never prints its token;
//! * an ask the adapter does not support is refused as unsupported before
//!   the material is read or the provider is reached;
//! * material the adapter did not encode, and a slot the lobby closed, are
//!   refused without reaching the provider.
//!
//! Every outcome is an [`Outcome`], which cannot carry provider text.

use std::sync::atomic::{AtomicUsize, Ordering};

use lobby_contracts::{
    AuthorizationAsk, Credential, CredentialAdapter, MaterialSlot, Outcome, SealedMaterial,
    SignInRequest, SignInReturn, SignInStart,
};

/// What the credential suite needs from a provider's fake.
pub trait CredentialFixture {
    /// What the sponsor types to start a sign-in with this provider.
    fn hint(&self) -> String;
    /// An account the provider knows, in the form the adapter reports it.
    fn account(&self) -> String;
    /// Makes the provider authenticate `account` on the next sign-in.
    fn approve_as(&self, account: &str);
    /// Follows the browser from `start` until the provider sends it back to
    /// the lobby, and returns the query it carries back.
    fn browse(&self, start: &SignInStart) -> Vec<(String, String)>;
    /// How many requests have reached the provider so far.
    fn requests(&self) -> usize;
    /// The asks the adapter answers.
    fn supported_asks(&self) -> Vec<AuthorizationAsk>;
    /// The asks the adapter does not support.
    fn unsupported_asks(&self) -> Vec<AuthorizationAsk>;
}

/// A slot holding fixed material that counts its reads.
pub struct CountingSlot {
    material: Result<SealedMaterial, Outcome>,
    reads: AtomicUsize,
}

impl CountingSlot {
    /// A slot that hands out `material`.
    pub fn new(material: Result<SealedMaterial, Outcome>) -> Self {
        Self {
            material,
            reads: AtomicUsize::new(0),
        }
    }

    /// How many times the material was read.
    pub fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }
}

impl MaterialSlot for CountingSlot {
    fn read(&self) -> Result<SealedMaterial, Outcome> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.material.clone()
    }
}

/// Runs the credential suite against `adapter`, whose provider `fixture` fakes.
pub fn run(adapter: &dyn CredentialAdapter, fixture: &dyn CredentialFixture) {
    let kind = adapter.kind();
    let request = SignInRequest {
        hint: fixture.hint(),
        state: "conformance-state-1".to_owned(),
        return_to: format!("http://127.0.0.1:1/signin/{kind}/return"),
    };

    // A sign-in records the account the provider authenticated.
    let account = fixture.account();
    fixture.approve_as(&account);
    let start = adapter.begin_sign_in(&request).expect("a sign-in starts");
    let query = fixture.browse(&start);
    let forged = SignInReturn {
        query: query
            .iter()
            .map(|(name, value)| {
                let value = if name == "state" {
                    "another-state".to_owned()
                } else {
                    value.clone()
                };
                (name.clone(), value)
            })
            .collect(),
    };
    assert!(
        adapter.complete_sign_in(&request, &forged).is_err(),
        "a return carrying another state is refused"
    );
    let returned = SignInReturn { query };
    let signed = adapter
        .complete_sign_in(&request, &returned)
        .expect("the sign-in completes");
    assert_eq!(
        signed.account.as_str(),
        account,
        "the account is the one the provider authenticated"
    );
    assert!(
        Credential::permissions_are_valid(&signed.permissions),
        "{:?} follow the rule",
        signed.permissions
    );

    // Every supported ask is answered from the slot's material.
    for ask in fixture.supported_asks() {
        let slot = CountingSlot::new(Ok(signed.material.clone()));
        let authorization = adapter
            .authorize(&slot, &ask)
            .unwrap_or_else(|outcome| panic!("{ask:?} is answered, not {outcome}"));
        assert!(!authorization.scheme.is_empty() && !authorization.token.expose().is_empty());
        assert!(
            !format!("{authorization:?}").contains(authorization.token.expose()),
            "an authorization never prints its token"
        );
        assert!(slot.reads() >= 1, "the material came from the slot");
    }

    // An unsupported ask is refused before the material or the provider.
    let before = fixture.requests();
    for ask in fixture.unsupported_asks() {
        let slot = CountingSlot::new(Ok(signed.material.clone()));
        assert_eq!(
            adapter.authorize(&slot, &ask).map(|_| ()),
            Err(Outcome::Unsupported),
            "{ask:?} is unsupported"
        );
        assert_eq!(slot.reads(), 0, "the material was not read");
    }
    assert_eq!(fixture.requests(), before, "the provider was not reached");

    // Foreign material, and a slot the lobby closed, are refused unreached.
    for ask in fixture.supported_asks() {
        for material in [
            Ok(SealedMaterial::new(
                b"material some other adapter wrote".to_vec(),
            )),
            Ok(SealedMaterial::new(Vec::new())),
            Err(Outcome::Revoked),
        ] {
            let slot = CountingSlot::new(material);
            assert!(
                adapter.authorize(&slot, &ask).is_err(),
                "{ask:?} is refused from material the adapter did not encode"
            );
        }
    }
    assert_eq!(fixture.requests(), before, "the provider was not reached");
}
