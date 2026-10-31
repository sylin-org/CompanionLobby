//! The Tangent service adapter: Tangent in Tangent's own words.
//!
//! A companion holds one connection per Tangent Space, at the Space's
//! origin; a companion with several names the one it means with `at`. The
//! adapter accepts the one provider kind the registry hands it, whose
//! accounts can prove themselves to a Space with a service proof minted
//! at their own server, and asks every sign-in of that kind for
//! [`PERMISSION`], the leave to mint that proof.
//!
//! Establishing a connection:
//!
//! 1. the Space's authentication profile is read from its well-known path
//!    with no credential, and must keep to the connection's address in
//!    every particular (see [`profile`]);
//! 2. the authority is asked for a proof of the account for the audience
//!    the profile names, which is the one derived from the address, and
//!    the exchange's method, living [`PROOF_SECS`] seconds; a fresh proof
//!    every time, never one sent before;
//! 3. the proof is presented once, in the `Authorization` header only, to
//!    the exchange, with no body, which answers `204` and cookies that sign
//!    the account in as a browser's sign-in does;
//! 4. the Space's door is read with those cookies, within [`DOOR_WITHIN`],
//!    for who the Space says the companion is and whether it may post; a door
//!    that cannot be read leaves those out and fails nothing, and a door that
//!    reads as a visitor's means the exchange did not sign anyone in;
//! 5. the connection keeps exactly what the Space issued (see [`issued`]).
//!
//! A call goes with the connection's own cookies (see [`jar`]), and what the
//! Space sets along the way is kept for its next call. Reading is the Space's
//! pages as JSON, and writing the forms those pages carry (see [`tools`]). A
//! Space that answers 401, or that answers a page as it answers a visitor,
//! has ended what the cookies signed in: the adapter proves the account
//! again, once, with a fresh proof, and makes the call once more. What each
//! answer means is decided once, in [`wire`].
//!
//! Tangent's routes, wire names and tool names live here and in its own
//! documentation, and nowhere else.

pub mod issued;
pub mod jar;
pub mod profile;
pub mod references;
pub mod tools;
pub mod wire;

use lobby_contracts::{
    Address, AddressPolicy, Authority, AuthorizationAsk, Effect, Explained, IssuedSlot, KnownAs,
    Outcome, ProviderKind, SealedIssued, ServiceAdapter, ServiceDescription, ServiceName,
    ToolArguments, ToolDescriptor, ToolName, ToolResult,
};
use lobby_outbound::Outbound;
use std::cell::RefCell;
use std::time::Duration;

use crate::issued::Issued;
use crate::jar::Jar;
use crate::profile::{EXCHANGE_METHOD, EXCHANGE_PATH, PROFILE_PATH, Profile};
use crate::wire::{Call, Failed};

/// The service's name, which prefixes every tool.
pub const NAME: &str = "tangent";
/// What the sponsor and a mind call it: the product's own name.
pub const TITLE: &str = "Tangent Space";
/// What an account of that kind must be granted: minting the exchange's
/// proof for any audience.
pub const PERMISSION: &str = "rpc:org.sylin.tangent.exchangeIdentity?aud=*";
/// How long a proof is asked to live, in seconds.
pub const PROOF_SECS: u32 = 60;
/// How long reading the door may take as a connection is established.
pub const DOOR_WITHIN: Duration = Duration::from_secs(3);

/// The profile does not keep to the address.
pub const PROFILE_MISMATCH: &str = "The Space's sign-in profile does not match its address, so the lobby sent it no proof. Check the address, or ask the Space's operator.";
/// The address does not answer as a Space.
pub const NOT_A_SPACE: &str =
    "Nothing at this address answers as a Tangent Space. Check the address.";
/// The Space has no address of its own, which a proof is addressed to.
pub const NOT_CONFIGURED: &str = "This Space has no address of its own yet, and a proof is addressed to one, so it takes none. Its operator configures one, or someone signs in there in a browser, which gives it one.";
/// The Space did not take the proof.
pub const PROOF_REFUSED: &str = "The Space did not accept the account's proof. Try again; if it keeps refusing, ask the Space's operator.";
/// The Space shows nothing to the account, however it signs in.
pub const SUSPENDED: &str = wire::SUSPENDED;
/// The exchange answered what the adapter does not trust.
pub const EXCHANGE_BROKE: &str =
    "The Space's answer did not keep to Tangent's exchange, so nothing was connected.";
/// A writing tool on a connection whose door said the companion may not post.
pub const NOT_GRANTED: &str = "The Space does not let this companion post here yet. Call tangent_arrive to read why and to look again; reading still works.";
/// The Space could not read the account's document.
pub const ISSUER_UNREACHABLE: &str = "The Space could not reach the account's server to check its proof. Try again in a little while.";

/// The Tangent service adapter.
#[derive(Debug)]
pub struct Tangent {
    outbound: Outbound,
    accepts: ProviderKind,
}

impl Tangent {
    /// An adapter that takes accounts of the provider kind `accepts`, and
    /// reaches Spaces at public HTTPS origins and on this machine, where a
    /// Space's operator may run one.
    pub fn accepting(accepts: ProviderKind) -> Self {
        Self {
            outbound: Outbound::new(AddressPolicy::LoopbackAllowed),
            accepts,
        }
    }

    /// Presents `proof` to the exchange, and answers what it signed in, as
    /// the door shows it.
    fn exchange(&self, address: &Address, proof: &str) -> Result<Issued, Explained> {
        let jar = RefCell::new(Jar::default());
        let call = Call {
            method: "POST",
            path: EXCHANGE_PATH,
            proof: Some(proof),
            jar: &jar,
            token: None,
            body: None,
            effect: Effect::Read,
            within: None,
        };
        let response =
            wire::send(&self.outbound, address, &call).map_err(|failed| match failed {
                Failed::Ended(explained) => explained,
                Failed::Unauthenticated => Explained::new(Outcome::Refused, PROOF_REFUSED),
            })?;
        match (response.status, wire::word(&response, "error").as_deref()) {
            (204, _) if !jar.borrow().is_empty() => {}
            (200..=299, _) => {
                return Err(Explained::new(
                    Outcome::TemporarilyUnavailable,
                    EXCHANGE_BROKE,
                ));
            }
            (401, _) => return Err(Explained::new(Outcome::Refused, PROOF_REFUSED)),
            (503, Some("exchange_unconfigured")) => {
                return Err(Explained::new(Outcome::Refused, NOT_CONFIGURED));
            }
            (503, Some("exchange_unavailable")) => {
                return Err(Explained::new(
                    Outcome::TemporarilyUnavailable,
                    ISSUER_UNREACHABLE,
                ));
            }
            _ => {
                return Err(Explained::new(
                    Outcome::TemporarilyUnavailable,
                    wire::UNAVAILABLE,
                ));
            }
        }
        let through =
            tools::Through::new(&self.outbound, address, Issued::signed_in(jar.into_inner()));
        match through.door_within(DOOR_WITHIN) {
            Ok(_) => {}
            // The cookies the exchange set do not sign anyone in.
            Err(Failed::Unauthenticated) => {
                return Err(Explained::new(
                    Outcome::TemporarilyUnavailable,
                    EXCHANGE_BROKE,
                ));
            }
            Err(Failed::Ended(explained)) if explained.sentence == SUSPENDED => {
                return Err(explained);
            }
            // A door that cannot be read says nothing yet; arriving asks again.
            Err(Failed::Ended(_)) => {}
        }
        Ok(through.issued())
    }

    /// Establishes the connection at `address` again through `slot`, keeps
    /// what the Space issued anew, and answers it.
    fn again(&self, address: &Address, slot: &dyn IssuedSlot) -> Result<Issued, Explained> {
        let fresh = self.establish(address, slot.authority())?;
        let issued = Issued::decode(&fresh).ok_or(Outcome::Internal)?;
        slot.reissue(fresh)?;
        Ok(issued)
    }
}

impl ServiceAdapter for Tangent {
    fn name(&self) -> ServiceName {
        ServiceName::new(NAME).unwrap_or_else(|| unreachable!("tangent is a service name"))
    }

    fn describe(&self) -> ServiceDescription {
        ServiceDescription {
            accepts: vec![self.accepts.clone()],
            addresses: AddressPolicy::LoopbackAllowed,
            home: false,
            permissions: vec![PERMISSION.to_owned()],
        }
    }

    fn establish(
        &self,
        address: &Address,
        authority: &dyn Authority,
    ) -> Result<SealedIssued, Explained> {
        let jar = RefCell::new(Jar::default());
        let call = Call {
            method: "GET",
            path: PROFILE_PATH,
            proof: None,
            jar: &jar,
            token: None,
            body: None,
            effect: Effect::Read,
            within: None,
        };
        let response =
            wire::send(&self.outbound, address, &call).map_err(|failed| match failed {
                Failed::Ended(explained) => explained,
                Failed::Unauthenticated => Explained::new(Outcome::Refused, NOT_A_SPACE),
            })?;
        match response.status {
            200 => {}
            503 => return Err(Explained::new(Outcome::Refused, NOT_CONFIGURED)),
            404 => return Err(Explained::new(Outcome::NotFound, NOT_A_SPACE)),
            500..=599 => {
                return Err(Explained::new(
                    Outcome::TemporarilyUnavailable,
                    wire::UNAVAILABLE,
                ));
            }
            _ => return Err(Explained::new(Outcome::Refused, NOT_A_SPACE)),
        }
        let profile: Profile = serde_json::from_slice(&response.body)
            .map_err(|_| Explained::new(Outcome::Refused, NOT_A_SPACE))?;
        let audience = profile
            .audience_for(address)
            .ok_or(Explained::new(Outcome::Refused, PROFILE_MISMATCH))?;
        let proof = authority.authorize(&AuthorizationAsk::ServiceProof {
            audience,
            method: EXCHANGE_METHOD.to_owned(),
            lifetime_secs: PROOF_SECS,
        })?;
        if proof.scheme != "Bearer" {
            return Err(Outcome::Unsupported.into());
        }
        Ok(self.exchange(address, proof.token.expose())?.encode())
    }

    fn catalog(&self) -> Vec<ToolDescriptor> {
        tools::TOOLS.iter().map(tools::Tool::descriptor).collect()
    }

    fn known_as(&self, issued: &SealedIssued) -> Option<KnownAs> {
        let issued = Issued::decode(issued)?;
        Some(KnownAs {
            name: issued.name.clone(),
            reference: issued.participant.clone(),
            reference_is: "participant",
            allows: if issued.posts {
                "read and post"
            } else {
                "read, but not post"
            },
        })
    }

    fn available(&self, issued: &SealedIssued) -> Vec<ToolName> {
        let posts = Issued::decode(issued).is_none_or(|issued| issued.posts);
        tools::TOOLS
            .iter()
            .filter(|tool| posts || tool.needs != tools::Needs::Post)
            .map(|tool| tool.descriptor().name)
            .collect()
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
            return Err(Explained::new(
                Outcome::InvalidInput,
                "That tool was given an argument it does not take; its description names each one it takes.",
            ));
        }
        let mut kept = slot.read()?;
        // What the lobby cannot read as this adapter's is proved anew, as is
        // what the Space has ended.
        let issued = match Issued::decode(&kept) {
            Some(issued) => issued,
            None => {
                let fresh = self.again(address, slot)?;
                kept = fresh.encode();
                fresh
            }
        };
        if found.needs == tools::Needs::Post && !issued.posts {
            return Err(Explained::new(Outcome::NeedsPermission, NOT_GRANTED));
        }
        let through = tools::Through::new(&self.outbound, address, issued);
        let answer = match tools::perform(found, arguments, &through) {
            // The Space did not know the companion, so it acted on nothing,
            // and proving the account again and trying once more does
            // nothing twice.
            Err(Failed::Unauthenticated) => {
                let fresh = self.again(address, slot)?;
                kept = fresh.encode();
                through.replace(fresh);
                match tools::perform(found, arguments, &through) {
                    Err(Failed::Unauthenticated) => {
                        Err(Explained::new(Outcome::Refused, wire::NOT_TAKEN))
                    }
                    Err(Failed::Ended(explained)) => Err(explained),
                    Ok(result) => Ok(result),
                }
            }
            Err(Failed::Ended(explained)) => Err(explained),
            Ok(result) => Ok(result),
        };
        // What the Space set along the way goes with the next call. A
        // connection revoked meanwhile keeps nothing, and that says nothing
        // of this answer, which the lobby settles itself.
        let settled = through.issued().encode();
        if settled.as_bytes() != kept.as_bytes() {
            let _ = slot.reissue(settled);
        }
        answer
    }
}
