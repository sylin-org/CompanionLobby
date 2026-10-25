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

use crate::atproto::AtprotoWorld;
use crate::board::BoardServer;
use crate::passbook::PassbookAuthority;

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
        Self {
            passbook,
            board,
            second_board,
            atproto,
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
        ]
    }
}
