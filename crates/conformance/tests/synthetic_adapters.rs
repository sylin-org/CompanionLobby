//! Both synthetic adapters hold to their contracts, in process, against the
//! test kit's fakes.

use lobby_conformance::credential::{self, CountingSlot, CredentialFixture};
use lobby_conformance::service::{self, ServiceFixture};
use lobby_contracts::{
    Address, AddressPolicy, Authority, Authorization, AuthorizationAsk, CredentialAdapter, Outcome,
    SealedMaterial, SignInRequest, SignInReturn, SignInStart, ToolArguments, ToolName,
};
use lobby_outbound::{Outbound, Request};
use lobby_testkit::board::{Board, BoardServer};
use lobby_testkit::passbook::{Passbook, PassbookAuthority};
use lobby_testkit::percent_decode;

/// Follows the browser from `url` to wherever the authority sends it back,
/// and returns the query it carries.
fn browse(url: &str) -> Vec<(String, String)> {
    let rest = url.strip_prefix("http://").expect("an http URL");
    let slash = rest.find('/').expect("a path");
    let origin = Address::parse(
        &format!("http://{}", &rest[..slash]),
        AddressPolicy::LoopbackAllowed,
    )
    .expect("a loopback origin");
    let answer = Outbound::new(AddressPolicy::LoopbackAllowed)
        .send(&origin, Request::new("GET", &rest[slash..]))
        .expect("the authority answers");
    assert_eq!(answer.status, 302);
    let location = answer.header("location").expect("a location");
    let (_, query) = location.split_once('?').expect("a query");
    query
        .split('&')
        .map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(name), percent_decode(value))
        })
        .collect()
}

struct PassbookFixture {
    authority: PassbookAuthority,
}

impl CredentialFixture for PassbookFixture {
    fn hint(&self) -> String {
        self.authority.origin()
    }

    fn approve_as(&self, account: &str) {
        self.authority.sign_in_as(account);
    }

    fn browse(&self, start: &SignInStart) -> Vec<(String, String)> {
        browse(&start.url)
    }

    fn requests(&self) -> usize {
        self.authority.listener().requests().len()
    }

    fn supported_asks(&self) -> Vec<AuthorizationAsk> {
        vec![AuthorizationAsk::ServiceProof {
            audience: "conformance-audience".to_owned(),
            method: "conformance.method".to_owned(),
            lifetime_secs: 60,
        }]
    }

    fn unsupported_asks(&self) -> Vec<AuthorizationAsk> {
        vec![AuthorizationAsk::Bearer]
    }
}

#[test]
fn the_passbook_holds_to_the_credential_contract() {
    credential::run(
        &Passbook::new(),
        &PassbookFixture {
            authority: PassbookAuthority::start(),
        },
    );
}

/// Mints with the passbook from material a real sign-in produced, as the
/// lobby's broker does.
struct Minting {
    adapter: Passbook,
    material: SealedMaterial,
}

impl Authority for Minting {
    fn authorize(&self, ask: &AuthorizationAsk) -> Result<Authorization, Outcome> {
        self.adapter
            .authorize(&CountingSlot::new(Ok(self.material.clone())), ask)
    }
}

struct BoardFixture {
    authority: PassbookAuthority,
    board: BoardServer,
    minting: Minting,
}

impl BoardFixture {
    fn start() -> Self {
        let authority = PassbookAuthority::start();
        let board = BoardServer::start(&authority);
        let adapter = Passbook::new();
        authority.sign_in_as("conformance-member");
        let request = SignInRequest {
            hint: authority.origin(),
            state: "board-state".to_owned(),
            return_to: "http://127.0.0.1:1/signin/passbook/return".to_owned(),
        };
        let start = adapter.begin_sign_in(&request).expect("start");
        let returned = SignInReturn {
            query: browse(&start.url),
        };
        let signed = adapter
            .complete_sign_in(&request, &returned)
            .expect("signed in");
        Self {
            authority,
            board,
            minting: Minting {
                adapter,
                material: signed.material,
            },
        }
    }
}

impl ServiceFixture for BoardFixture {
    fn address(&self) -> Address {
        Address::parse(&self.board.origin(), AddressPolicy::LoopbackAllowed).expect("address")
    }

    fn authority(&self) -> &dyn Authority {
        &self.minting
    }

    fn minted(&self) -> Vec<String> {
        self.authority
            .minted()
            .into_iter()
            .map(|minted| minted.stamp)
            .collect()
    }

    fn plant(&self, author: &str, text: &str) {
        self.board.add_post(author, text);
    }

    fn read(&self) -> (ToolName, ToolArguments) {
        (
            ToolName::new("read").expect("tool"),
            ToolArguments::default(),
        )
    }

    fn requests(&self) -> usize {
        self.board.listener().requests().len()
    }
}

#[test]
fn the_board_holds_to_the_service_contract() {
    service::run(&Board::new(), &BoardFixture::start());
}
