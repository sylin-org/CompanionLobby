//! The atproto credential adapter holds to the credential contract, in
//! process, against the test kit's fake of its provider.

use lobby_conformance::credential::{self, CountingSlot, CredentialFixture, RenewalFixture};
use lobby_contracts::{
    AuthorizationAsk, CredentialAdapter, Outcome, SignInRequest, SignInReturn, SignInStart,
};
use lobby_credential_atproto::{Atproto, World};
use lobby_testkit::atproto::AtprotoWorld;
use lobby_testkit::listener::Listener;
use lobby_testkit::percent_decode;

const HOLDER: &str = "did:plc:conformanceconformanceco";

struct AtprotoFixture {
    world: AtprotoWorld,
}

impl CredentialFixture for AtprotoFixture {
    fn hint(&self) -> String {
        String::new()
    }

    fn account(&self) -> String {
        HOLDER.to_owned()
    }

    fn approve_as(&self, account: &str) {
        self.world.add_account("holder.example.test", account);
        self.world.sign_in_as(account);
    }

    fn browse(&self, start: &SignInStart) -> Vec<(String, String)> {
        let back = self.world.approve(&start.url);
        let (_, query) = back.split_once('?').expect("a query");
        query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .map(|(name, value)| (percent_decode(name), percent_decode(value)))
            .collect()
    }

    fn requests(&self) -> usize {
        self.world.server().requests().len() + self.world.pds().requests().len()
    }

    fn supported_asks(&self) -> Vec<AuthorizationAsk> {
        Vec::new()
    }

    fn unsupported_asks(&self) -> Vec<AuthorizationAsk> {
        vec![AuthorizationAsk::ServiceProof {
            audience: "did:web:service.example.test".to_owned(),
            method: "com.example.method".to_owned(),
            lifetime_secs: 60,
        }]
    }
}

impl RenewalFixture for AtprotoFixture {
    fn issue_briefly(&self) {
        self.world.access_lifetime(1);
    }

    fn issue_lastingly(&self) {
        self.world.access_lifetime(3600);
    }

    fn renewals_presented(&self) -> Vec<String> {
        self.world.refresh_tokens_presented()
    }

    fn live_secret(&self) -> Option<String> {
        self.world.live_refresh_token(HOLDER)
    }

    fn rate_limit(&self, limiting: bool) {
        self.world.rate_limit_refreshes(limiting);
    }

    fn refuse(&self) {
        self.world.refuse_refreshes();
    }

    fn revocations(&self) -> Vec<String> {
        self.world.revocations()
    }

    fn renewals(&self) -> (&Listener, &'static str) {
        (self.world.server(), "/oauth/token")
    }
}

#[test]
fn the_atproto_adapter_holds_to_the_credential_contract() {
    let world = AtprotoWorld::start();
    let adapter = Atproto::new(World::loopback(&world.origin()).expect("a loopback world"));
    credential::run(&adapter, &AtprotoFixture { world });
}

#[test]
fn the_atproto_adapter_renews_in_the_order_the_contract_lays_down() {
    let world = AtprotoWorld::start();
    let adapter = Atproto::new(World::loopback(&world.origin()).expect("a loopback world"));
    credential::renewal(&adapter, &AtprotoFixture { world });
}

#[test]
fn a_refresh_that_grants_more_than_the_sign_in_held_is_never_kept() {
    let world = AtprotoWorld::start();
    let adapter = Atproto::new(World::loopback(&world.origin()).expect("a loopback world"));
    let fixture = AtprotoFixture { world };
    fixture.approve_as(HOLDER);
    fixture.issue_briefly();
    let request = SignInRequest {
        hint: String::new(),
        state: "widened-state".to_owned(),
        return_to: "http://127.0.0.1:1/signin/atproto/return".to_owned(),
        again: None,
        permissions: Vec::new(),
    };
    let start = adapter.begin_sign_in(&request).expect("a sign-in starts");
    let returned = SignInReturn {
        query: fixture.browse(&start),
    };
    let signed = adapter
        .complete_sign_in(&request, &returned)
        .expect("signed in");
    assert_eq!(signed.permissions, ["atproto"]);
    fixture.world.widen_refreshes("atproto transition:generic");
    let slot = CountingSlot::holding(Ok(signed.material.clone()), signed.permissions.clone());
    assert_eq!(
        adapter.renew(&slot).map(|_| ()),
        Err(Outcome::Unknown),
        "the provider rotated the secret but granted more than the sign-in held"
    );
    assert_eq!(slot.steps(), ["mark"], "nothing was committed");
    assert!(
        slot.marked(),
        "the mark stays, so the spent secret is never offered again"
    );
    assert_eq!(slot.material(), Ok(signed.material));
    assert_eq!(fixture.renewals_presented().len(), 1);
}
