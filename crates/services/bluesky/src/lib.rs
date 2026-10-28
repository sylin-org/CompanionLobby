//! The Bluesky service adapter: Bluesky in Bluesky's own words.
//!
//! Bluesky lives where the account lives. A companion's posts, likes and
//! reposts are records in its own repository on its account's PDS, and
//! what it reads comes from the app view through that same PDS. So a
//! connection names no address of its own: it reaches the account's PDS,
//! which the credential adapter names as the account's home, and one
//! credential holds at most one Bluesky connection. The adapter accepts the
//! one provider kind the registry hands it, and asks every sign-in of that
//! kind for [`PERMISSIONS`]: creating posts, likes and reposts in the
//! account's repository, uploading images, and reading the timeline and
//! posts through the app view.
//!
//! Every request to the PDS is proved on its own: the authority is asked
//! for a proof of exactly that method and URL, which the credential adapter
//! mints only for the account's own PDS, and the access token goes under
//! `DPoP` beside it, never as a bearer token (see [`wire`]).
//!
//! Establishing a connection asks the PDS who the account is
//! (`com.atproto.server.getSession`), with a proved request, and keeps
//! only its DID and handle (see [`issued`]). Nothing the service issues
//! expires, so a call never establishes the connection again. The tools
//! are in [`tools`].
//!
//! Lexicon names, routes and the network's words live here and in its own
//! documentation, and nowhere else.

pub mod issued;
pub mod tools;
pub mod wire;

use lobby_contracts::{
    Address, AddressPolicy, Authority, Effect, Explained, IssuedSlot, KnownAs, Outcome,
    ProviderKind, SealedIssued, ServiceAdapter, ServiceDescription, ServiceName, ToolArguments,
    ToolDescriptor, ToolName, ToolResult,
};
use lobby_outbound::Outbound;

use crate::issued::Issued;
use crate::wire::{Call, Nonces};

/// The service's name, which prefixes every tool.
pub const NAME: &str = "bluesky";
/// What the sponsor and a mind call it.
pub const TITLE: &str = "Bluesky";
/// What signing an account in again for Bluesky lets a companion do, as a
/// clause that follows "can".
pub const LETS: &str = "post to Bluesky";
/// What a connection lets the companion do there, as a clause that
/// follows "lets it".
pub const ALLOWS: &str = "read its timeline, post, reply, like and repost";
/// What an account must be granted, in the network's own permission words.
pub const PERMISSIONS: [&str; 6] = [
    "repo:app.bsky.feed.post",
    "repo:app.bsky.feed.like",
    "repo:app.bsky.feed.repost",
    "blob:image/*",
    "rpc:app.bsky.feed.getTimeline?aud=*",
    "rpc:app.bsky.feed.getPosts?aud=*",
];
/// The PDS route that names the account.
pub const SESSION_PATH: &str = "/xrpc/com.atproto.server.getSession";

/// The PDS named an account this adapter cannot read.
pub const NOT_AN_ACCOUNT: &str = "The account's server did not name the account in a way the lobby reads, so nothing was connected. Try again later.";
/// What a tool was given that it does not take.
pub const UNKNOWN_ARGUMENT: &str =
    "That tool was given an argument it does not take; its description names each one it takes.";

/// The Bluesky service adapter.
#[derive(Debug)]
pub struct Bluesky {
    outbound: Outbound,
    nonces: Nonces,
    accepts: ProviderKind,
    policy: AddressPolicy,
}

impl Bluesky {
    /// An adapter that takes accounts of the provider kind `accepts`, whose
    /// servers `policy` admits: public HTTPS origins for the product, and
    /// the loopback address where a test's fake PDS stands.
    pub fn accepting(accepts: ProviderKind, policy: AddressPolicy) -> Self {
        Self {
            outbound: Outbound::new(policy),
            nonces: Nonces::default(),
            accepts,
            policy,
        }
    }
}

impl ServiceAdapter for Bluesky {
    fn name(&self) -> ServiceName {
        ServiceName::new(NAME).unwrap_or_else(|| unreachable!("bluesky is a service name"))
    }

    fn describe(&self) -> ServiceDescription {
        ServiceDescription {
            accepts: vec![self.accepts.clone()],
            addresses: self.policy,
            home: true,
            permissions: PERMISSIONS.iter().map(|word| (*word).to_owned()).collect(),
        }
    }

    fn establish(
        &self,
        address: &Address,
        authority: &dyn Authority,
    ) -> Result<SealedIssued, Explained> {
        let answer = wire::send(
            &self.outbound,
            &self.nonces,
            address,
            authority,
            &Call {
                method: "GET",
                path: SESSION_PATH.to_owned(),
                body: None,
                proxied: false,
                effect: Effect::Read,
            },
        )?;
        let issued = Issued::from_answer(&answer).ok_or(Explained::new(
            Outcome::TemporarilyUnavailable,
            NOT_AN_ACCOUNT,
        ))?;
        Ok(issued.encode())
    }

    fn catalog(&self) -> Vec<ToolDescriptor> {
        tools::TOOLS.iter().map(tools::Tool::descriptor).collect()
    }

    fn known_as(&self, issued: &SealedIssued) -> Option<KnownAs> {
        let issued = Issued::decode(issued)?;
        Some(KnownAs {
            name: None,
            reference: Some(format!("@{}", issued.handle)),
            reference_is: "handle",
            allows: ALLOWS,
        })
    }

    fn call(
        &self,
        tool: &ToolName,
        arguments: &ToolArguments,
        address: &Address,
        slot: &dyn IssuedSlot,
    ) -> Result<ToolResult, Explained> {
        let Some(found) = tools::tool(tool.as_str()) else {
            return Err(Outcome::Unsupported.into());
        };
        if arguments
            .0
            .keys()
            .any(|key| !found.arguments.contains(&key.as_str()))
        {
            return Err(Explained::new(Outcome::InvalidInput, UNKNOWN_ARGUMENT));
        }
        let issued = Issued::decode(&slot.read()?).ok_or(Outcome::Internal)?;
        tools::perform(
            found,
            arguments,
            &tools::Through {
                outbound: &self.outbound,
                nonces: &self.nonces,
                address,
                authority: slot.authority(),
                issued: &issued,
            },
        )
    }
}
