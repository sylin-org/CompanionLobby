//! The passbook: a synthetic credential provider and its adapter.
//!
//! A passbook authority keeps holders. A sponsor signs a companion in by
//! sending the browser to the authority's approval page, which approves as
//! whichever holder the test scripted and sends the browser back to the
//! lobby with a one-use code; the adapter exchanges the code for the holder's
//! name and a key. The key is the credential's material. With it the adapter
//! asks the authority for stamps: short-lived tokens for one audience and
//! method, which a service checks with the authority. The authority never
//! mints a plain stamp that any service would take, so the adapter refuses a
//! bearer ask before it reads its material or reaches the authority.
//!
//! Neither half ships in the product: the daemon compiles them in only for
//! tests, to prove the host through adapters whose words are not those of any
//! real provider.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use lobby_contracts::{
    AccountKey, Address, AddressPolicy, Authorization, AuthorizationAsk, CredentialAdapter,
    MaterialSlot, Outcome, ProviderKind, SealedMaterial, Secret, SignInRequest, SignInReturn,
    SignInStart, SignedIn,
};
use lobby_outbound::{Failure, Outbound, Request};
use serde_json::{Value, json};

use crate::listener::{Listener, Received, Reply};
use crate::{percent_encode, unique_token};

/// The provider kind of the passbook adapter.
pub const KIND: &str = "passbook";

/// The format tag of passbook material.
const MATERIAL_FORMAT: &str = "passbook-1";

/// A stamp the authority minted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Minted {
    /// The stamp itself.
    pub stamp: String,
    /// The holder it speaks for.
    pub holder: String,
    /// The audience it is for.
    pub audience: String,
    /// The method it allows there.
    pub method: String,
}

#[derive(Debug, Default)]
struct Book {
    approving: Option<String>,
    codes: HashMap<String, String>,
    keys: HashMap<String, String>,
    minted: Vec<Minted>,
}

/// A fake passbook authority on its own loopback listener.
pub struct PassbookAuthority {
    listener: Listener,
    book: Arc<Mutex<Book>>,
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Default for PassbookAuthority {
    fn default() -> Self {
        Self::start()
    }
}

impl PassbookAuthority {
    /// Starts an authority that approves no one until told to.
    pub fn start() -> Self {
        let book = Arc::new(Mutex::new(Book::default()));
        let answering = book.clone();
        let listener = Listener::start(move |request| answer(&answering, request));
        Self { listener, book }
    }

    /// The authority's origin, which the sponsor types as the sign-in hint.
    pub fn origin(&self) -> String {
        self.listener.origin()
    }

    /// The listener, to count what reached the authority.
    pub fn listener(&self) -> &Listener {
        &self.listener
    }

    /// Approves every sign-in from now on as `holder`.
    pub fn sign_in_as(&self, holder: &str) {
        locked(&self.book).approving = Some(holder.to_owned());
    }

    /// Every stamp minted so far, oldest first.
    pub fn minted(&self) -> Vec<Minted> {
        locked(&self.book).minted.clone()
    }

    /// The stamp `presented`, when this authority minted it.
    pub fn verify(&self, presented: &str) -> Option<Minted> {
        self.verifier().verify(presented)
    }

    /// A handle a fake service keeps to check stamps with this authority.
    pub fn verifier(&self) -> Verifier {
        Verifier(self.book.clone())
    }

    /// Every key handed out, for tests that look for secrets where none may be.
    pub fn keys(&self) -> Vec<String> {
        locked(&self.book).keys.keys().cloned().collect()
    }
}

/// Checks stamps against one authority, as the services that trust it do.
#[derive(Clone)]
pub struct Verifier(Arc<Mutex<Book>>);

impl Verifier {
    /// The stamp `presented`, when the authority minted it.
    pub fn verify(&self, presented: &str) -> Option<Minted> {
        locked(&self.0)
            .minted
            .iter()
            .find(|minted| minted.stamp == presented)
            .cloned()
    }
}

fn answer(book: &Mutex<Book>, request: &Received) -> Reply {
    let mut book = locked(book);
    match (request.method.as_str(), request.route()) {
        ("GET", "/approve") => {
            let (Some(state), Some(return_to)) =
                (request.query("state"), request.query("return_to"))
            else {
                return Reply::json(400, &json!({ "error": "state and return_to are required" }));
            };
            let Some(holder) = book.approving.clone() else {
                return Reply::json(403, &json!({ "error": "no holder approves" }));
            };
            let code = unique_token("code");
            book.codes.insert(code.clone(), holder);
            Reply::redirect(&format!(
                "{return_to}?state={}&code={}",
                percent_encode(&state),
                percent_encode(&code)
            ))
        }
        ("POST", "/exchange") => {
            let body = request.json();
            let Some(holder) = body["code"]
                .as_str()
                .and_then(|code| book.codes.remove(code))
            else {
                return Reply::json(400, &json!({ "error": "unknown or used code" }));
            };
            let key = unique_token("key");
            book.keys.insert(key.clone(), holder.clone());
            Reply::json(200, &json!({ "holder": holder, "key": key }))
        }
        ("POST", "/stamp") => {
            let body = request.json();
            let Some(holder) = body["key"]
                .as_str()
                .and_then(|key| book.keys.get(key))
                .cloned()
            else {
                return Reply::json(401, &json!({ "error": "unknown key" }));
            };
            let (Some(audience), Some(method)) =
                (body["audience"].as_str(), body["method"].as_str())
            else {
                return Reply::json(
                    400,
                    &json!({ "error": "a stamp names an audience and a method" }),
                );
            };
            let minted = Minted {
                stamp: unique_token("stamp"),
                holder,
                audience: audience.to_owned(),
                method: method.to_owned(),
            };
            book.minted.push(minted.clone());
            Reply::json(200, &json!({ "stamp": minted.stamp }))
        }
        _ => Reply::json(404, &json!({ "error": "no such route" })),
    }
}

/// The passbook credential adapter.
#[derive(Debug)]
pub struct Passbook {
    outbound: Outbound,
}

impl Default for Passbook {
    fn default() -> Self {
        Self::new()
    }
}

impl Passbook {
    /// An adapter that reaches authorities on this machine.
    pub fn new() -> Self {
        Self {
            outbound: Outbound::new(AddressPolicy::LoopbackAllowed),
        }
    }

    fn post(&self, authority: &Address, path: &str, body: &Value) -> Result<Value, Outcome> {
        let request = Request::new("POST", path).json(body.to_string().into_bytes());
        let response = self
            .outbound
            .send(authority, request)
            .map_err(|failure| match failure {
                Failure::Refused => Outcome::Refused,
                Failure::Unreachable | Failure::Interrupted => Outcome::TemporarilyUnavailable,
            })?;
        match response.status {
            200 if response.is_json() => {
                serde_json::from_slice(&response.body).map_err(|_| Outcome::TemporarilyUnavailable)
            }
            401 | 403 => Err(Outcome::NeedsSignIn),
            429 => Err(Outcome::RateLimited),
            500..=599 | 200 => Err(Outcome::TemporarilyUnavailable),
            _ => Err(Outcome::Refused),
        }
    }
}

/// Passbook material: the authority that issued the key, and the key.
struct Material {
    authority: Address,
    key: Secret,
}

impl Material {
    fn encode(&self) -> SealedMaterial {
        let body = json!({
            "format": MATERIAL_FORMAT,
            "authority": self.authority.as_str(),
            "key": self.key.expose(),
        });
        SealedMaterial::new(body.to_string().into_bytes())
    }

    /// Decodes material this adapter encoded; anything else is not ours.
    fn decode(material: &SealedMaterial) -> Option<Self> {
        let body: Value = serde_json::from_slice(material.as_bytes()).ok()?;
        if body["format"] != MATERIAL_FORMAT {
            return None;
        }
        let authority =
            Address::parse(body["authority"].as_str()?, AddressPolicy::LoopbackAllowed)?;
        let key = body["key"].as_str().filter(|key| !key.is_empty())?;
        Some(Self {
            authority,
            key: Secret::new(key),
        })
    }
}

impl CredentialAdapter for Passbook {
    fn kind(&self) -> ProviderKind {
        ProviderKind::new(KIND).unwrap_or_else(|| unreachable!("passbook is a provider kind"))
    }

    fn begin_sign_in(&self, request: &SignInRequest) -> Result<SignInStart, Outcome> {
        let authority = Address::parse(request.hint.trim(), AddressPolicy::LoopbackAllowed)
            .ok_or(Outcome::InvalidInput)?;
        Ok(SignInStart {
            url: authority.url(&format!(
                "/approve?state={}&return_to={}",
                percent_encode(&request.state),
                percent_encode(&request.return_to)
            )),
        })
    }

    fn complete_sign_in(
        &self,
        request: &SignInRequest,
        returned: &SignInReturn,
    ) -> Result<SignedIn, Outcome> {
        if returned.get("state") != Some(request.state.as_str()) {
            return Err(Outcome::Refused);
        }
        let code = returned.get("code").ok_or(Outcome::Refused)?;
        let authority = Address::parse(request.hint.trim(), AddressPolicy::LoopbackAllowed)
            .ok_or(Outcome::InvalidInput)?;
        let answer = self
            .post(&authority, "/exchange", &json!({ "code": code }))
            .map_err(|outcome| match outcome {
                Outcome::NeedsSignIn => Outcome::Refused,
                other => other,
            })?;
        let account = answer["holder"]
            .as_str()
            .and_then(AccountKey::new)
            .ok_or(Outcome::TemporarilyUnavailable)?;
        let key = answer["key"]
            .as_str()
            .filter(|key| !key.is_empty())
            .ok_or(Outcome::TemporarilyUnavailable)?;
        let material = Material {
            authority,
            key: Secret::new(key),
        };
        Ok(SignedIn {
            account,
            material: material.encode(),
        })
    }

    fn authorize(
        &self,
        slot: &dyn MaterialSlot,
        ask: &AuthorizationAsk,
    ) -> Result<Authorization, Outcome> {
        // An ask the authority cannot answer is refused before the material
        // is read or the authority is reached.
        let mut body = match ask {
            AuthorizationAsk::Bearer => return Err(Outcome::Unsupported),
            AuthorizationAsk::ServiceProof {
                audience, method, ..
            } => json!({ "audience": audience, "method": method }),
        };
        let material = Material::decode(&slot.read()?).ok_or(Outcome::NeedsSignIn)?;
        body["key"] = Value::String(material.key.expose().to_owned());
        let answer = self.post(&material.authority, "/stamp", &body)?;
        let stamp = answer["stamp"]
            .as_str()
            .filter(|stamp| !stamp.is_empty())
            .ok_or(Outcome::TemporarilyUnavailable)?;
        Ok(Authorization {
            scheme: "Bearer".to_owned(),
            token: Secret::new(stamp),
        })
    }
}
