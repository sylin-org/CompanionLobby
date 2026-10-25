//! A whole fake world on loopback, for walking the sponsor's journey.
//!
//! [`LobbyWorld`] starts a passbook authority that approves every sign-in as
//! [`HOLDER`], two boards that trust it, each with a post from another
//! member, a fake atproto world that knows one account, [`HANDLE`], and
//! approves every sign-in as it, and a fake Space that takes the proofs
//! that world's PDS mints, with one Tangent, two Topics and a short
//! conversation between two other participants. `companion-lobby-synthetic`,
//! run with `LOBBY_AUTHSERVER` naming [`LobbyWorld::authserver`], signs
//! companions in against it and connects them to its boards and its Space,
//! so a person, a test or a mind can walk the whole journey with nothing on
//! the public network. The `lobby-world` command runs one and prints its
//! origins.
//!
//! A control page turns the knobs, so a walk can bring on trouble:
//! `/keys/brief` makes the keys the passbook authority issues from then on
//! live two seconds, so the lobby renews them again and again, `/keys/long`
//! makes them live an hour, `/renewals/refuse` refuses every renewal and
//! `/renewals/allow` allows them again; `/space/locked` makes the Space
//! refuse every new Post in its welcome Topic, and `/space/tokens-end` ends
//! every token the Space issued, so the next call exchanges a fresh proof.
//! Each answers with a short page saying what it did.

use serde_json::json;

use std::sync::Arc;

use crate::atproto::AtprotoWorld;
use crate::board::BoardServer;
use crate::listener::{Listener, Reply};
use crate::passbook::{KEY_SECS, PassbookAuthority};
use crate::space::SpaceServer;

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
    /// The fake Space.
    pub space: Arc<SpaceServer>,
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
        let space = Arc::new(SpaceServer::start(&atproto.proofs()));
        space.add_tangent("home", "Home", "Where everyone starts.");
        space.add_topic(
            "home",
            "welcome",
            "Welcome",
            "Say hello, and tell everyone what you are working on.",
        );
        space.add_topic(
            "home",
            "gardens",
            "Small gardens",
            "Balconies, windowsills and pots.",
        );
        space.name_account(DID, "Ada", HANDLE);
        let bea = space.add_participant("Bea", "bea.example.test");
        let cy = space.add_participant("Cy", "cy.example.test");
        let opening = space.add_post(
            "welcome",
            &bea,
            "Welcome to the Space! What is everyone working on this week?",
            None,
        );
        space.add_post(
            "welcome",
            &cy,
            "Repotting a fig tree on my balcony. Slow going, but it is coming along.",
            Some(&opening),
        );
        space.add_post(
            "gardens",
            &bea,
            "Basil keeps bolting on my windowsill. Any tips?",
            None,
        );
        let knobs = passbook.controls();
        let knobs_space = space.clone();
        let control = Listener::start(move |request| {
            let did = match request.route() {
                "/space/locked" => {
                    knobs_space.refuse_writes_in("welcome");
                    "The Space refuses every new Post in its welcome Topic."
                }
                "/space/tokens-end" => {
                    knobs_space.end_tokens();
                    "The Space ended every token it issued."
                }
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
            space,
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
            format!("space={}", self.space.origin()),
            format!("control={}", self.control.origin()),
        ]
    }
}
