//! Both synthetic adapters hold to their contracts, in process, against the
//! test kit's fakes.

use lobby_conformance::credential::{self, CountingSlot, CredentialFixture, RenewalFixture};
use lobby_conformance::service::{self, ServiceFixture};
use lobby_contracts::{
    Address, AddressPolicy, Authority, Authorization, AuthorizationAsk, CredentialAdapter,
    Explained, Outcome, SealedMaterial, ServiceAdapter, SignInRequest, SignInReturn, SignInStart,
    ToolArguments, ToolName,
};
use lobby_outbound::{Outbound, Request};
use lobby_testkit::board::{ADMIT_METHOD, Board, BoardServer};
use lobby_testkit::listener::Listener;
use lobby_testkit::passbook::{self, Passbook, PassbookAuthority, stamp_permission};
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

    fn account(&self) -> String {
        "conformance-holder".to_owned()
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
        Vec::new()
    }

    fn permissions(&self) -> Vec<String> {
        vec![stamp_permission("conformance.method")]
    }
}

impl RenewalFixture for PassbookFixture {
    fn issue_briefly(&self) {
        self.authority.key_lifetime(1);
    }

    fn issue_lastingly(&self) {
        self.authority.key_lifetime(3600);
    }

    fn renewals_presented(&self) -> Vec<String> {
        self.authority.renewals_presented()
    }

    fn live_secret(&self) -> Option<String> {
        self.authority.live_renewal()
    }

    fn rate_limit(&self, limiting: bool) {
        self.authority.rate_limit_renewals(limiting);
    }

    fn refuse(&self) {
        self.authority.refuse_renewals(true);
    }

    fn revocations(&self) -> Vec<String> {
        self.authority.revoked()
    }

    fn renewals(&self) -> (&Listener, &'static str) {
        (self.authority.listener(), "/renew")
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

#[test]
fn the_passbook_renews_in_the_order_the_contract_lays_down() {
    credential::renewal(
        &Passbook::new(),
        &PassbookFixture {
            authority: PassbookAuthority::start(),
        },
    );
}

#[test]
fn two_authorizations_racing_a_due_key_renew_it_once() {
    let fixture = PassbookFixture {
        authority: PassbookAuthority::start(),
    };
    fixture.authority.key_lifetime(1);
    let adapter = Passbook::new();
    fixture.approve_as("racer");
    let request = SignInRequest {
        hint: fixture.hint(),
        state: "racing-state".to_owned(),
        return_to: "http://127.0.0.1:1/signin/passbook/return".to_owned(),
        again: None,
        permissions: vec![stamp_permission("racing.method")],
    };
    let start = adapter.begin_sign_in(&request).expect("start");
    let signed = adapter
        .complete_sign_in(
            &request,
            &SignInReturn {
                query: browse(&start.url),
            },
        )
        .expect("signed in");
    // The key the authority issues next lives long, so one renewal is
    // enough for both.
    fixture.authority.key_lifetime(3600);
    let slot = CountingSlot::holding(Ok(signed.material), signed.permissions);
    let ask = AuthorizationAsk::ServiceProof {
        audience: "racing-audience".to_owned(),
        method: "racing.method".to_owned(),
        lifetime_secs: 60,
    };
    std::thread::scope(|scope| {
        let one = scope.spawn(|| adapter.authorize(&slot, &ask));
        let two = scope.spawn(|| adapter.authorize(&slot, &ask));
        assert!(one.join().expect("one").is_ok());
        assert!(two.join().expect("two").is_ok());
    });
    assert_eq!(
        fixture.authority.renewals_presented().len(),
        1,
        "one renewal for two authorizations"
    );
    assert_eq!(slot.steps(), ["mark", "commit"]);
    assert_eq!(fixture.authority.minted().len(), 2);
}

/// Mints with the passbook from material a real sign-in produced, as the
/// lobby's broker does.
struct Minting {
    adapter: Passbook,
    material: SealedMaterial,
}

impl Authority for Minting {
    fn authorize(&self, ask: &AuthorizationAsk) -> Result<Authorization, Explained> {
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
            again: None,
            permissions: Board::new().describe().permissions,
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

    fn drop_issued(&self) {
        self.board.drop_badges();
    }
}

#[test]
fn the_board_holds_to_the_service_contract() {
    service::run(&Board::new(), &BoardFixture::start());
}

#[test]
fn the_board_needs_a_stamp_the_passbook_is_asked_for_and_refuses_without_it() {
    let board = Board::new().describe();
    assert_eq!(board.permissions, [stamp_permission(ADMIT_METHOD)]);
    let authority = PassbookAuthority::start();
    authority.sign_in_as("withheld-member");
    authority.withhold(&stamp_permission(ADMIT_METHOD));
    let adapter = Passbook::new();
    let request = SignInRequest {
        hint: authority.origin(),
        state: "withheld-state".to_owned(),
        return_to: "http://127.0.0.1:1/signin/passbook/return".to_owned(),
        again: None,
        permissions: board.permissions.clone(),
    };
    let start = adapter.begin_sign_in(&request).expect("start");
    let signed = adapter
        .complete_sign_in(
            &request,
            &SignInReturn {
                query: browse(&start.url),
            },
        )
        .expect("signed in");
    assert_eq!(
        authority.asked(),
        std::slice::from_ref(&board.permissions),
        "the sign-in asked for what the board needs"
    );
    assert_eq!(signed.permissions, [passbook::PERMISSION]);
    let before = authority.listener().requests().len();
    let refused = adapter
        .authorize(
            &CountingSlot::holding(Ok(signed.material), signed.permissions),
            &AuthorizationAsk::ServiceProof {
                audience: "a-board".to_owned(),
                method: ADMIT_METHOD.to_owned(),
                lifetime_secs: 60,
            },
        )
        .expect_err("not granted");
    assert_eq!(
        refused,
        Explained::new(Outcome::NeedsPermission, passbook::NOT_GRANTED)
    );
    assert_eq!(
        authority.listener().requests().len(),
        before,
        "refused before anything reached the authority"
    );
    assert!(authority.minted().is_empty());
}
