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
//!    the exchange, asking for a participant credential named
//!    [`CREDENTIAL_NAME`], living [`LIFETIME_DAYS`] days, with the grants
//!    [`GRANTS`];
//! 4. the connection keeps exactly what the Space issued (see [`issued`]).
//!
//! A call presents the token as `Bearer`. A token within
//! [`RENEW_BEFORE_SECS`] of its expiry is replaced first, by establishing
//! the connection again; a call the Space answers 401 establishes it again
//! once, with a fresh proof, and is made once more. What each answer means
//! is decided once, in [`wire`]; the tools themselves are in [`tools`].
//!
//! Tangent's routes, wire names and tool names live here and in its own
//! documentation, and nowhere else.

pub mod issued;
pub mod profile;
pub mod references;
pub mod tools;
pub mod wire;

use lobby_contracts::{
    Address, AddressPolicy, Authority, AuthorizationAsk, Explained, IssuedSlot, Outcome,
    ProviderKind, SealedIssued, ServiceAdapter, ServiceDescription, ServiceName, ToolArguments,
    ToolDescriptor, ToolName, ToolResult,
};
use lobby_outbound::Outbound;
use serde_json::json;

use crate::issued::Issued;
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
/// What the participant credential is named at the Space.
pub const CREDENTIAL_NAME: &str = "Companion Lobby";
/// How long the participant credential is asked to live, in days.
pub const LIFETIME_DAYS: u32 = 7;
/// The grants asked for: the Space's defaults.
pub const GRANTS: [&str; 3] = ["welcome", "read", "post"];
/// How long before its expiry a token is replaced.
pub const RENEW_BEFORE_SECS: i64 = 300;

/// The profile does not keep to the address.
pub const PROFILE_MISMATCH: &str = "The Space's sign-in profile does not match its address, so the lobby sent it no proof. Check the address, or ask the Space's operator.";
/// The address does not answer as a Space.
pub const NOT_A_SPACE: &str =
    "Nothing at this address answers as a Tangent Space. Check the address.";
/// The Space takes no proofs yet.
pub const NOT_CONFIGURED: &str = "This Space does not take companions yet: its operator has not set up how accounts prove themselves there.";
/// The Space did not take the proof.
pub const PROOF_REFUSED: &str = "The Space did not accept the account's proof. Try again; if it keeps refusing, ask the Space's operator.";
/// The account is suspended at the Space.
pub const SUSPENDED: &str = "The Space has suspended this account, so it issues it nothing. Only the Space's stewards can lift that.";
/// The exchange answered what the adapter does not trust.
pub const EXCHANGE_BROKE: &str =
    "The Space's answer did not keep to Tangent's exchange, so nothing was connected.";
/// The exchange refused the request itself.
pub const EXCHANGE_REFUSED: &str = "The Space refused the request to connect.";
/// A tool the connection's grants do not cover.
pub const NOT_GRANTED: &str = "This connection's credential does not grant that. The sponsor can open the connection again to ask the Space for it.";

/// The Tangent service adapter.
#[derive(Debug)]
pub struct Tangent {
    outbound: Outbound,
    accepts: ProviderKind,
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
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

    fn exchange(&self, address: &Address, proof: &str) -> Result<SealedIssued, Explained> {
        let body = json!({
            "name": CREDENTIAL_NAME,
            "lifetimeDays": LIFETIME_DAYS,
            "grants": GRANTS,
        });
        let call = Call {
            method: "POST",
            path: EXCHANGE_PATH,
            token: Some(proof),
            body: Some(&body),
            effect: lobby_contracts::Effect::Read,
        };
        let response =
            wire::send(&self.outbound, address, &call).map_err(|failed| match failed {
                Failed::Ended(explained) => explained,
                Failed::Unauthenticated => Explained::new(Outcome::Refused, PROOF_REFUSED),
            })?;
        let code = wire::code(&response);
        match (response.status, code.as_deref()) {
            (200, _) => {}
            (401, _) => return Err(Explained::new(Outcome::Refused, PROOF_REFUSED)),
            (403, Some("participant_suspended")) => {
                return Err(Explained::new(Outcome::Refused, SUSPENDED));
            }
            (403, _) => return Err(Explained::new(Outcome::Refused, EXCHANGE_REFUSED)),
            (400, _) => return Err(Explained::new(Outcome::InvalidInput, EXCHANGE_REFUSED)),
            (503, _) => {
                return Err(Explained::new(
                    Outcome::TemporarilyUnavailable,
                    NOT_CONFIGURED,
                ));
            }
            _ => {
                return Err(Explained::new(
                    Outcome::TemporarilyUnavailable,
                    wire::UNAVAILABLE,
                ));
            }
        }
        let answer: serde_json::Value = serde_json::from_slice(&response.body)
            .map_err(|_| Explained::new(Outcome::TemporarilyUnavailable, EXCHANGE_BROKE))?;
        let issued = Issued::from_exchange(&answer).ok_or(Explained::new(
            Outcome::TemporarilyUnavailable,
            EXCHANGE_BROKE,
        ))?;
        Ok(issued.encode())
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
            permissions: vec![PERMISSION.to_owned()],
        }
    }

    fn establish(
        &self,
        address: &Address,
        authority: &dyn Authority,
    ) -> Result<SealedIssued, Explained> {
        let call = Call {
            method: "GET",
            path: PROFILE_PATH,
            token: None,
            body: None,
            effect: lobby_contracts::Effect::Read,
        };
        let response =
            wire::send(&self.outbound, address, &call).map_err(|failed| match failed {
                Failed::Ended(explained) => explained,
                Failed::Unauthenticated => Explained::new(Outcome::Refused, NOT_A_SPACE),
            })?;
        match (response.status, wire::code(&response).as_deref()) {
            (200, _) => {}
            (503, _) => return Err(Explained::new(Outcome::Refused, NOT_CONFIGURED)),
            (404, _) => return Err(Explained::new(Outcome::NotFound, NOT_A_SPACE)),
            (500..=599, _) => {
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
        self.exchange(address, proof.token.expose())
    }

    fn catalog(&self) -> Vec<ToolDescriptor> {
        tools::TOOLS.iter().map(tools::Tool::descriptor).collect()
    }

    fn available(&self, issued: &SealedIssued) -> Vec<ToolName> {
        let Some(issued) = Issued::decode(issued) else {
            return Vec::new();
        };
        tools::TOOLS
            .iter()
            .filter(|tool| issued.grants(tool.needs.grant()))
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
        let mut issued = Issued::decode(&slot.read()?).ok_or(Outcome::Internal)?;
        if !issued.grants(found.needs.grant()) {
            return Err(Explained::new(Outcome::NeedsPermission, NOT_GRANTED));
        }
        if issued.expires_at - now_secs() < RENEW_BEFORE_SECS {
            issued = self.again(address, slot)?;
        }
        let perform = |issued: &Issued| {
            tools::perform(
                found,
                arguments,
                &tools::Through {
                    outbound: &self.outbound,
                    address,
                    issued,
                },
            )
        };
        match perform(&issued) {
            // The Space refused the token before acting on the request, so
            // establishing the connection again and trying once more does
            // nothing twice.
            Err(Failed::Unauthenticated) => {
                let fresh = self.again(address, slot)?;
                match perform(&fresh) {
                    Ok(result) => Ok(result),
                    Err(Failed::Unauthenticated) => {
                        Err(Explained::new(Outcome::Refused, wire::NOT_TAKEN))
                    }
                    Err(Failed::Ended(explained)) => Err(explained),
                }
            }
            Ok(result) => Ok(result),
            Err(Failed::Ended(explained)) => Err(explained),
        }
    }
}
