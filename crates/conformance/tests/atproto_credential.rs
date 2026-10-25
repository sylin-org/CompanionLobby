//! The atproto credential adapter holds to the credential contract, in
//! process, against the test kit's fake of its provider.

use lobby_conformance::credential::{self, CredentialFixture, RenewalFixture};
use lobby_contracts::{AuthorizationAsk, SignInStart};
use lobby_credential_atproto::{Atproto, World};
use lobby_testkit::atproto::AtprotoWorld;
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
