//! A whole fake world on loopback, for walking the sponsor's journey.
//!
//! [`LobbyWorld`] starts a passbook authority that approves every sign-in as
//! [`HOLDER`], two boards that trust it, each with a post from another
//! member, and a fake atproto world that knows one account, [`HANDLE`], and
//! approves every sign-in as it. `companion-lobby-synthetic`, run with
//! `LOBBY_AUTHSERVER` naming [`LobbyWorld::authserver`], signs companions in
//! against it and connects them to its boards, so a person or a test can
//! walk the whole journey in a real browser with nothing on the public
//! network. The `lobby-world` command runs one and prints its origins.
//!
//! A control page turns the passbook authority's knobs, so a walk can bring
//! on trouble: `/keys/brief` makes the keys it issues from then on live two
//! seconds, so the lobby renews them again and again, `/keys/long` makes
//! them live an hour, `/renewals/refuse` refuses every renewal and
//! `/renewals/allow` allows them again. Each answers with a short page
//! saying what it did.

use serde_json::json;

use crate::atproto::AtprotoWorld;
use crate::board::BoardServer;
use crate::listener::{Listener, Reply};
use crate::passbook::{KEY_SECS, PassbookAuthority};

/// The passbook holder every passbook sign-in approves as.
pub const HOLDER: &str = "ada";
/// The handle of the one atproto account the world knows.
pub const HANDLE: &str = "ada.example.test";
/// That account's DID.
pub const DID: &str = "did:plc:aaaaaaaaaaaaaaaaaaaaaaaa";

/// The fakes a sponsor's journey needs, each on its own loopback listener.
pub struct LobbyWorld {
    /// The passbook authority.
    pub passbook: PassbookAuthority,
    /// The first board.
    pub board: BoardServer,
    /// A second board, for a companion that reaches two.
    pub second_board: BoardServer,
    /// The fake atproto world.
    pub atproto: AtprotoWorld,
    /// The control page.
    pub control: Listener,
}

impl Default for LobbyWorld {
    fn default() -> Self {
        Self::start()
    }
}

impl LobbyWorld {
    /// Starts every fake and scripts who signs in.
    pub fn start() -> Self {
        let passbook = PassbookAuthority::start();
        passbook.sign_in_as(HOLDER);
        let board = BoardServer::start(&passbook);
        board.add_post("bea", "Welcome to the board.");
        let second_board = BoardServer::start(&passbook);
        second_board.add_post("cy", "The second board is quiet.");
        let atproto = AtprotoWorld::start();
        atproto.add_account(HANDLE, DID);
        atproto.sign_in_as(DID);
        let knobs = passbook.controls();
        let control = Listener::start(move |request| {
            let did = match request.route() {
                "/keys/brief" => {
                    knobs.key_lifetime(2);
                    "Passbook keys issued from now on live two seconds."
                }
                "/keys/long" => {
                    knobs.key_lifetime(KEY_SECS);
                    "Passbook keys issued from now on live an hour."
                }
                "/renewals/refuse" => {
                    knobs.refuse_renewals(true);
                    "The Passbook authority refuses every renewal."
                }
                "/renewals/allow" => {
                    knobs.refuse_renewals(false);
                    "The Passbook authority renews keys again."
                }
                _ => return Reply::json(404, &json!({ "error": "no such knob" })),
            };
            Reply::json(200, &json!({ "done": did }))
        });
        Self {
            passbook,
            board,
            second_board,
            atproto,
            control,
        }
    }

    /// The origin `LOBBY_AUTHSERVER` names for the synthetic command.
    pub fn authserver(&self) -> String {
        self.atproto.origin()
    }

    /// What the `lobby-world` command prints: one `name=value` line per
    /// origin and account, in a fixed order.
    pub fn lines(&self) -> Vec<String> {
        vec![
            format!("LOBBY_AUTHSERVER={}", self.authserver()),
            format!("passbook={}", self.passbook.origin()),
            format!("passbook-holder={HOLDER}"),
            format!("board={}", self.board.origin()),
            format!("second-board={}", self.second_board.origin()),
            format!("atmosphere-handle={HANDLE}"),
            format!("control={}", self.control.origin()),
        ]
    }
}
