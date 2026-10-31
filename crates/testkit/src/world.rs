//! A whole fake world on loopback, for walking the sponsor's journey.
//!
//! [`LobbyWorld`] starts a passbook authority that approves every sign-in as
//! [`HOLDER`], two boards that trust it, each with a post from another
//! member, a fake atproto world that knows one account, [`HANDLE`], and
//! approves every sign-in as it, whose PDS holds two posts by other
//! accounts for its timeline, and a fake Space that takes the proofs
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
//! `/renewals/allow` allows them again; `/space/locked` locks the Space's
//! welcome Topic, `/space/sign-ins-end` ends every sign-in the Space made,
//! so the next call exchanges a fresh proof, and `/space/new-post` has Cy post
//! in the Space's gardens Topic, which is new to a companion that has already
//! arrived;
//! `/atmosphere/older` makes Atmosphere sign-ins grant only identity and
//! the Space's leave, as an account signed in before the lobby's other
//! services needed more, and `/atmosphere/asked` makes them grant what they
//! ask for again.
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
/// What an Atmosphere sign-in grants after `/atmosphere/older`: identity
/// and the leave the Space needs, and nothing a later service asks for.
pub const OLDER_SCOPE: &str = "atproto rpc:org.sylin.tangent.exchangeIdentity?aud=*";
/// The DID of Bea, who posts at the same PDS.
pub const BEA_DID: &str = "did:plc:bbbbbbbbbbbbbbbbbbbbbbbb";
/// The DID of Cy, who posts there too.
pub const CY_DID: &str = "did:plc:cccccccccccccccccccccccc";

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

/// Has Cy post in the gardens Topic of the world's Space.
pub fn new_post(space: &SpaceServer) {
    if let Some(cy) = space.participant_named("Cy") {
        space.add_post(
            "gardens",
            &cy,
            "The fig is in its new pot. Now to find it some sun.",
            None,
        );
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
        // Bea and Cy post at the same PDS, so Ada's timeline has their
        // posts in it.
        atproto.add_account("bea.example.test", BEA_DID);
        atproto.add_account("cy.example.test", CY_DID);
        atproto.display_name(BEA_DID, "Bea");
        atproto.display_name(CY_DID, "Cy");
        atproto.add_post(BEA_DID, "Morning, everyone. The basil finally came up.");
        atproto.add_post(CY_DID, "Repotting the fig this morning. Wish me luck.");
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
        let knobs_atproto = atproto.knobs();
        let control = Listener::start(move |request| {
            let did = match request.route() {
                "/atmosphere/older" => {
                    knobs_atproto.grants_scope(OLDER_SCOPE);
                    "Atmosphere sign-ins from now on grant only identity and the Space's leave, as one made before the lobby's other services needed more."
                }
                "/atmosphere/asked" => {
                    knobs_atproto.grants_what_is_asked();
                    "Atmosphere sign-ins from now on grant what they ask for."
                }
                "/space/locked" => {
                    knobs_space.lock("welcome");
                    "The Space locked its welcome Topic."
                }
                "/space/new-post" => {
                    new_post(&knobs_space);
                    "Cy posted in the Space's gardens Topic."
                }
                "/space/sign-ins-end" => {
                    knobs_space.end_sign_ins();
                    "The Space ended every sign-in it made."
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
            format!("pds={}", self.atproto.pds_origin()),
            format!("space={}", self.space.origin()),
            format!("control={}", self.control.origin()),
        ]
    }
}
