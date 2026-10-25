//! The passbook: a synthetic credential provider and its adapter.
//!
//! A passbook authority keeps holders. A sponsor signs a companion in by
//! sending the browser to the authority's approval page, which approves as
//! whichever holder the test scripted and sends the browser back to the
//! lobby with a one-use code; the adapter exchanges the code for the holder's
//! name, a key that lives for a while, and a one-use renewal. The key, the
//! renewal and when the key expires are the credential's material. With the
//! key the adapter asks the authority for stamps: short-lived tokens for one
//! audience and method, which a service checks with the authority. The
//! authority never mints a plain stamp that any service would take.
//!
//! A key is renewed before it expires: the adapter presents the renewal and
//! gets a new key and a new renewal, the old ones ending at once, in the
//! order the credential contract lays down, marking the material rotating
//! before the renewal is presented. Revoking presents the renewal to end
//! both. Knobs make the authority refuse renewals, answer them 429, and set
//! how long keys live.
//!
//! Neither half ships in the product: the daemon compiles them in only for
//! tests, to prove the host through adapters whose words are not those of any
//! real provider.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use lobby_contracts::{
    AccountKey, AccountLabel, Address, AddressPolicy, Authorization, AuthorizationAsk,
    CredentialAdapter, CredentialId, Freshness, MaterialSlot, MaterialUpdate, Outcome,
    ProviderKind, Revocation, SealedMaterial, Secret, SignInRequest, SignInReturn, SignInStart,
    SignedIn,
};
use lobby_outbound::{Failure, Outbound, Request};
use serde_json::{Value, json};

use crate::listener::{Listener, Received, Reply};
use crate::{percent_encode, unique_token};

/// The provider kind of the passbook adapter.
pub const KIND: &str = "passbook";

/// What the sponsor reads for a passbook account.
pub const TITLE: &str = "Passbook";

/// What the sponsor reads beside the field naming a passbook account: a
/// passbook sign-in starts from the authority's address and nothing else.
pub const HINT: &str = "The address of the Passbook authority to sign in at.";

/// What every passbook sign-in grants: stamps for any audience.
pub const PERMISSION: &str = "stamp";

/// How long a key lives when a test says nothing else, in seconds.
pub const KEY_SECS: u64 = 3600;

/// How long before its key expires the adapter renews it, in seconds.
pub const MARGIN_SECS: i64 = 60;

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
    /// Each live renewal, with the holder and the key it renews.
    renewals: HashMap<String, (String, String)>,
    /// Every renewal issued, oldest first.
    issued: Vec<String>,
    minted: Vec<Minted>,
    revoked: Vec<String>,
    key_secs: Option<u64>,
    refusing: bool,
    limiting: bool,
}

impl Book {
    /// A fresh key and renewal for `holder`, as the authority answers them.
    fn issue(&mut self, holder: &str) -> Value {
        let key = unique_token("key");
        let renewal = unique_token("renewal");
        self.keys.insert(key.clone(), holder.to_owned());
        self.renewals
            .insert(renewal.clone(), (holder.to_owned(), key.clone()));
        self.issued.push(renewal.clone());
        json!({
            "holder": holder,
            "key": key,
            "renewal": renewal,
            "expiresIn": self.key_secs.unwrap_or(KEY_SECS),
        })
    }
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

    /// Issues keys that live `secs` seconds from now on.
    pub fn key_lifetime(&self, secs: u64) {
        locked(&self.book).key_secs = Some(secs);
    }

    /// Refuses every renewal while `refusing`, as an authority that ended the
    /// holder's keys.
    pub fn refuse_renewals(&self, refusing: bool) {
        locked(&self.book).refusing = refusing;
    }

    /// Answers every renewal with 429 while `limiting`, renewing nothing.
    pub fn rate_limit_renewals(&self, limiting: bool) {
        locked(&self.book).limiting = limiting;
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

    /// Every key handed out and still live, for tests that look for secrets
    /// where none may be.
    pub fn keys(&self) -> Vec<String> {
        locked(&self.book).keys.keys().cloned().collect()
    }

    /// Every renewal presented to the authority, oldest first, whether or
    /// not it renewed anything.
    pub fn renewals_presented(&self) -> Vec<String> {
        self.listener
            .requests()
            .iter()
            .filter(|request| request.route() == "/renew")
            .filter_map(|request| request.json()["renewal"].as_str().map(str::to_owned))
            .collect()
    }

    /// The renewal issued last, while it is live.
    pub fn live_renewal(&self) -> Option<String> {
        let book = locked(&self.book);
        book.issued
            .last()
            .filter(|renewal| book.renewals.contains_key(*renewal))
            .cloned()
    }

    /// Every renewal the authority was asked to revoke, oldest first.
    pub fn revoked(&self) -> Vec<String> {
        locked(&self.book).revoked.clone()
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
            Reply::json(200, &book.issue(&holder))
        }
        ("POST", "/renew") => {
            if book.limiting {
                return Reply::json(429, &json!({ "error": "renew less often" }));
            }
            if book.refusing {
                return Reply::json(400, &json!({ "error": "renewal refused" }));
            }
            let body = request.json();
            let Some((holder, key)) = body["renewal"]
                .as_str()
                .and_then(|renewal| book.renewals.remove(renewal))
            else {
                return Reply::json(400, &json!({ "error": "unknown or used renewal" }));
            };
            book.keys.remove(&key);
            Reply::json(200, &book.issue(&holder))
        }
        ("POST", "/revoke") => {
            let body = request.json();
            let renewal = body["renewal"].as_str().unwrap_or_default().to_owned();
            book.revoked.push(renewal.clone());
            if let Some((_, key)) = book.renewals.remove(&renewal) {
                book.keys.remove(&key);
            }
            Reply::json(200, &json!({}))
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
    /// The authority each waiting sign-in goes to, by its `state`.
    parked: Mutex<HashMap<String, Address>>,
    /// One lock per credential, held across a renewal and every
    /// authorization that reads its material.
    locks: Mutex<HashMap<CredentialId, Arc<Mutex<()>>>>,
}

impl Default for Passbook {
    fn default() -> Self {
        Self::new()
    }
}

/// The seconds since the Unix epoch.
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

impl Passbook {
    /// An adapter that reaches authorities on this machine.
    pub fn new() -> Self {
        Self {
            outbound: Outbound::new(AddressPolicy::LoopbackAllowed),
            parked: Mutex::new(HashMap::new()),
            locks: Mutex::new(HashMap::new()),
        }
    }

    fn lock_of(&self, credential: CredentialId) -> Arc<Mutex<()>> {
        locked(&self.locks).entry(credential).or_default().clone()
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

    /// Renews the key in `slot` when it is due, under the credential's
    /// lock, which whoever asks holds already.
    fn renew_held(&self, slot: &dyn MaterialSlot) -> Result<Freshness, Outcome> {
        let read = slot.read()?;
        let material = Material::decode(&read).ok_or(Outcome::Refused)?;
        let left = material.expires_at - now_secs();
        if left > MARGIN_SECS {
            return Ok(Freshness {
                renew_in_secs: u64::try_from(left - MARGIN_SECS).unwrap_or(0),
            });
        }
        slot.mark_rotating(&read)?;
        let request = Request::new("POST", "/renew").json(
            json!({ "renewal": material.renewal.expose() })
                .to_string()
                .into_bytes(),
        );
        let response = match self.outbound.send(&material.authority, request) {
            Ok(response) => response,
            Err(Failure::Refused | Failure::Unreachable) => {
                slot.unmark_rotating();
                return Err(Outcome::TemporarilyUnavailable);
            }
            Err(Failure::Interrupted) => return Err(Outcome::Unknown),
        };
        match response.status {
            200 => {}
            429 => {
                slot.unmark_rotating();
                return Err(Outcome::RateLimited);
            }
            400..=499 => {
                slot.unmark_rotating();
                return Err(Outcome::NeedsSignIn);
            }
            _ => return Err(Outcome::Unknown),
        }
        let answer: Value = serde_json::from_slice(&response.body).map_err(|_| Outcome::Unknown)?;
        let renewed =
            Material::issued(material.authority.clone(), &answer).ok_or(Outcome::Unknown)?;
        let lives = renewed.expires_at - now_secs();
        let update = MaterialUpdate::new(
            read,
            renewed.encode(),
            vec![PERMISSION.to_owned()],
            &[PERMISSION.to_owned()],
        )
        .ok_or(Outcome::Unknown)?;
        slot.commit(update).map_err(|_| Outcome::Unknown)?;
        Ok(Freshness {
            renew_in_secs: u64::try_from(lives - MARGIN_SECS).unwrap_or(0),
        })
    }
}

/// Passbook material: the authority that issued the key, the key, its
/// renewal and when the key expires.
struct Material {
    authority: Address,
    key: Secret,
    renewal: Secret,
    expires_at: i64,
}

impl Material {
    /// The material an authority's answer issues, from `authority`.
    fn issued(authority: Address, answer: &Value) -> Option<Self> {
        let token = |field: &str| {
            answer[field]
                .as_str()
                .filter(|token| !token.is_empty())
                .map(Secret::new)
        };
        let lives = answer["expiresIn"].as_u64()?;
        Some(Self {
            authority,
            key: token("key")?,
            renewal: token("renewal")?,
            expires_at: now_secs().saturating_add(i64::try_from(lives).ok()?),
        })
    }

    fn encode(&self) -> SealedMaterial {
        let body = json!({
            "format": MATERIAL_FORMAT,
            "authority": self.authority.as_str(),
            "key": self.key.expose(),
            "renewal": self.renewal.expose(),
            "expiresAt": self.expires_at,
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
        let secret = |field: &str| {
            body[field]
                .as_str()
                .filter(|text| !text.is_empty())
                .map(Secret::new)
        };
        Some(Self {
            authority,
            key: secret("key")?,
            renewal: secret("renewal")?,
            expires_at: body["expiresAt"].as_i64()?,
        })
    }
}

impl CredentialAdapter for Passbook {
    fn kind(&self) -> ProviderKind {
        ProviderKind::new(KIND).unwrap_or_else(|| unreachable!("passbook is a provider kind"))
    }

    fn begin_sign_in(&self, request: &SignInRequest) -> Result<SignInStart, Outcome> {
        // Signing in again goes to the authority the material names.
        let authority = match &request.again {
            Some(again) => Material::decode(again).ok_or(Outcome::Refused)?.authority,
            None => Address::parse(request.hint.trim(), AddressPolicy::LoopbackAllowed)
                .ok_or(Outcome::InvalidInput)?,
        };
        let url = authority.url(&format!(
            "/approve?state={}&return_to={}",
            percent_encode(&request.state),
            percent_encode(&request.return_to)
        ));
        locked(&self.parked).insert(request.state.clone(), authority);
        Ok(SignInStart { url })
    }

    fn complete_sign_in(
        &self,
        request: &SignInRequest,
        returned: &SignInReturn,
    ) -> Result<SignedIn, Outcome> {
        if returned.get("state") != Some(request.state.as_str()) {
            return Err(Outcome::Refused);
        }
        let authority = locked(&self.parked)
            .remove(&request.state)
            .ok_or(Outcome::Refused)?;
        let code = returned.get("code").ok_or(Outcome::Refused)?;
        let answer = self
            .post(&authority, "/exchange", &json!({ "code": code }))
            .map_err(|outcome| match outcome {
                Outcome::NeedsSignIn => Outcome::Refused,
                other => other,
            })?;
        let holder = answer["holder"].as_str().unwrap_or_default();
        let (account, label) = AccountKey::new(holder)
            .zip(AccountLabel::new(holder))
            .ok_or(Outcome::TemporarilyUnavailable)?;
        let material =
            Material::issued(authority, &answer).ok_or(Outcome::TemporarilyUnavailable)?;
        Ok(SignedIn {
            account,
            label,
            permissions: vec![PERMISSION.to_owned()],
            material: material.encode(),
        })
    }

    fn cancel_sign_in(&self, request: &SignInRequest) {
        locked(&self.parked).remove(&request.state);
    }

    fn authorize(
        &self,
        slot: &dyn MaterialSlot,
        ask: &AuthorizationAsk,
    ) -> Result<Authorization, Outcome> {
        let AuthorizationAsk::ServiceProof {
            audience, method, ..
        } = ask;
        let lock = self.lock_of(slot.credential());
        let _held = locked(&lock);
        // A key that is due is renewed before it mints anything.
        self.renew_held(slot)?;
        let material = Material::decode(&slot.read()?).ok_or(Outcome::NeedsSignIn)?;
        let body = json!({
            "audience": audience,
            "method": method,
            "key": material.key.expose(),
        });
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

    fn renew(&self, slot: &dyn MaterialSlot) -> Result<Freshness, Outcome> {
        let lock = self.lock_of(slot.credential());
        let _held = locked(&lock);
        self.renew_held(slot)
    }

    fn revoke(&self, slot: &dyn MaterialSlot) -> Result<Revocation, Outcome> {
        let lock = self.lock_of(slot.credential());
        let _held = locked(&lock);
        let material = Material::decode(&slot.read()?).ok_or(Outcome::Refused)?;
        self.post(
            &material.authority,
            "/revoke",
            &json!({ "renewal": material.renewal.expose() }),
        )?;
        Ok(Revocation::Confirmed)
    }
}
