//! The credential adapter suite.
//!
//! [`run`] drives one credential adapter against its provider's fake, in
//! process, through every promise of the contract:
//!
//! * a sign-in records the account the provider authenticated, whatever the
//!   sponsor typed, with permissions that follow the credential's rule, and
//!   a return carrying another `state` is refused;
//! * every ask the adapter supports is answered from the material in the
//!   slot, and the authorization never prints its token;
//! * an ask the adapter does not support is refused as unsupported before
//!   the material is read or the provider is reached;
//! * material the adapter did not encode, and a slot the lobby closed, are
//!   refused without reaching the provider.
//!
//! [`renewal`] holds the adapter to the order a renewal follows, against a
//! provider whose secrets live a second, through a [`CountingSlot`] that
//! records what the adapter did and in which order:
//!
//! * material that is not due is answered fresh without reaching the
//!   provider;
//! * a due renewal marks the material rotating before the provider hears the
//!   secret, presents it once, commits new material on top of exactly what
//!   it read, which clears the mark, and the next renewal presents the
//!   rotated secret, never the spent one;
//! * the mark is on the material while the secret is in flight, before the
//!   provider has read it;
//! * two renewals racing on one credential present one secret: the second
//!   waits for the first, which holds the credential's lock while the
//!   provider holds its request, and then finds the new material fresh;
//! * a provider that asks for fewer requests leaves the material as it was
//!   and the mark cleared, and so does one that refuses, which answers that
//!   the credential needs signing in again;
//! * new material the slot cannot keep leaves the mark in place, and the
//!   answer says the outcome is unknown;
//! * revoking presents the current secret to the provider.
//!
//! Every outcome is an [`Outcome`], which cannot carry provider text.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use lobby_contracts::{
    AuthorizationAsk, Credential, CredentialAdapter, CredentialId, MaterialSlot, MaterialUpdate,
    Outcome, Revocation, SealedMaterial, SignInRequest, SignInReturn, SignInStart, SignedIn,
};
use lobby_testkit::listener::Listener;

/// How long the second of two racing renewals is given to reach the
/// provider, were nothing holding it back.
const RACE_WINDOW: Duration = Duration::from_millis(400);

/// What the credential suite needs from a provider's fake.
pub trait CredentialFixture {
    /// What the sponsor types to start a sign-in with this provider.
    fn hint(&self) -> String;
    /// An account the provider knows, in the form the adapter reports it.
    fn account(&self) -> String;
    /// Makes the provider authenticate `account` on the next sign-in.
    fn approve_as(&self, account: &str);
    /// Follows the browser from `start` until the provider sends it back to
    /// the lobby, and returns the query it carries back.
    fn browse(&self, start: &SignInStart) -> Vec<(String, String)>;
    /// How many requests have reached the provider so far.
    fn requests(&self) -> usize;
    /// The asks the adapter answers.
    fn supported_asks(&self) -> Vec<AuthorizationAsk>;
    /// The asks the adapter does not support.
    fn unsupported_asks(&self) -> Vec<AuthorizationAsk>;
}

/// What the renewal suite needs from a provider's fake, beyond signing in.
pub trait RenewalFixture: CredentialFixture {
    /// Makes what the provider issues from now on live one second.
    fn issue_briefly(&self);
    /// Makes what the provider issues from now on live an hour.
    fn issue_lastingly(&self);
    /// Every secret presented to the provider to renew, oldest first.
    fn renewals_presented(&self) -> Vec<String>;
    /// The secret a renewal of what the provider granted last would
    /// present, while it is live.
    fn live_secret(&self) -> Option<String>;
    /// Makes the provider answer every renewal with 429 while `limiting`.
    fn rate_limit(&self, limiting: bool);
    /// Makes the provider refuse every renewal from now on.
    fn refuse(&self);
    /// Every secret the provider was asked to revoke, oldest first.
    fn revocations(&self) -> Vec<String>;
    /// The provider's listener and the route a renewal is sent to, so the
    /// suite can hold a renewal in flight.
    fn renewals(&self) -> (&Listener, &'static str);
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A slot holding material in memory, the way the daemon's does: it commits
/// an update only on top of the material it expected and within the
/// permissions held, and records what the adapter did, in order.
pub struct CountingSlot {
    credential: CredentialId,
    material: Mutex<Result<SealedMaterial, Outcome>>,
    held: Vec<String>,
    marked: AtomicBool,
    refusing_commits: AtomicBool,
    reads: AtomicUsize,
    steps: Mutex<Vec<&'static str>>,
}

impl CountingSlot {
    /// A slot that hands out `material` and holds no permission.
    pub fn new(material: Result<SealedMaterial, Outcome>) -> Self {
        Self::holding(material, Vec::new())
    }

    /// A slot that hands out `material` for a credential holding `held`.
    pub fn holding(material: Result<SealedMaterial, Outcome>, held: Vec<String>) -> Self {
        Self {
            credential: CredentialId::from_u128(0x5107),
            material: Mutex::new(material),
            held,
            marked: AtomicBool::new(false),
            refusing_commits: AtomicBool::new(false),
            reads: AtomicUsize::new(0),
            steps: Mutex::new(Vec::new()),
        }
    }

    /// How many times the material was read.
    pub fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }

    /// The material as it is now.
    pub fn material(&self) -> Result<SealedMaterial, Outcome> {
        locked(&self.material).clone()
    }

    /// Whether the material is marked rotating now.
    pub fn marked(&self) -> bool {
        self.marked.load(Ordering::SeqCst)
    }

    /// What the adapter did, in order: `mark`, `unmark` and `commit`.
    pub fn steps(&self) -> Vec<&'static str> {
        locked(&self.steps).clone()
    }

    /// Makes every commit fail, as a daemon that cannot write the new
    /// material.
    pub fn refuse_commits(&self) {
        self.refusing_commits.store(true, Ordering::SeqCst);
    }
}

impl MaterialSlot for CountingSlot {
    fn credential(&self) -> CredentialId {
        self.credential
    }

    fn read(&self) -> Result<SealedMaterial, Outcome> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.material()
    }

    fn mark_rotating(&self, read: &SealedMaterial) -> Result<(), Outcome> {
        if self.material().as_ref() != Ok(read) || self.marked() {
            return Err(Outcome::Conflict);
        }
        self.marked.store(true, Ordering::SeqCst);
        locked(&self.steps).push("mark");
        Ok(())
    }

    fn unmark_rotating(&self) {
        self.marked.store(false, Ordering::SeqCst);
        locked(&self.steps).push("unmark");
    }

    fn commit(&self, update: MaterialUpdate) -> Result<(), Outcome> {
        if self.refusing_commits.load(Ordering::SeqCst) {
            return Err(Outcome::TemporarilyUnavailable);
        }
        let mut material = locked(&self.material);
        if material.as_ref() != Ok(update.expected()) {
            return Err(Outcome::Conflict);
        }
        if !update
            .permissions()
            .iter()
            .all(|permission| self.held.contains(permission))
        {
            return Err(Outcome::Refused);
        }
        *material = Ok(update.replacement().clone());
        self.marked.store(false, Ordering::SeqCst);
        locked(&self.steps).push("commit");
        Ok(())
    }
}

/// Signs `account` in through `adapter` against `fixture`, as `state`.
fn signed_in(
    adapter: &dyn CredentialAdapter,
    fixture: &dyn CredentialFixture,
    state: &str,
) -> SignedIn {
    let kind = adapter.kind();
    let request = SignInRequest {
        hint: fixture.hint(),
        state: state.to_owned(),
        return_to: format!("http://127.0.0.1:1/signin/{kind}/return"),
        again: None,
    };
    fixture.approve_as(&fixture.account());
    let start = adapter.begin_sign_in(&request).expect("a sign-in starts");
    let returned = SignInReturn {
        query: fixture.browse(&start),
    };
    adapter
        .complete_sign_in(&request, &returned)
        .expect("the sign-in completes")
}

/// Runs the credential suite against `adapter`, whose provider `fixture` fakes.
pub fn run(adapter: &dyn CredentialAdapter, fixture: &dyn CredentialFixture) {
    let kind = adapter.kind();
    let request = SignInRequest {
        hint: fixture.hint(),
        state: "conformance-state-1".to_owned(),
        return_to: format!("http://127.0.0.1:1/signin/{kind}/return"),
        again: None,
    };

    // A sign-in records the account the provider authenticated.
    let account = fixture.account();
    fixture.approve_as(&account);
    let start = adapter.begin_sign_in(&request).expect("a sign-in starts");
    let query = fixture.browse(&start);
    let forged = SignInReturn {
        query: query
            .iter()
            .map(|(name, value)| {
                let value = if name == "state" {
                    "another-state".to_owned()
                } else {
                    value.clone()
                };
                (name.clone(), value)
            })
            .collect(),
    };
    assert!(
        adapter.complete_sign_in(&request, &forged).is_err(),
        "a return carrying another state is refused"
    );
    let returned = SignInReturn { query };
    let signed = adapter
        .complete_sign_in(&request, &returned)
        .expect("the sign-in completes");
    assert_eq!(
        signed.account.as_str(),
        account,
        "the account is the one the provider authenticated"
    );
    assert!(
        Credential::permissions_are_valid(&signed.permissions),
        "{:?} follow the rule",
        signed.permissions
    );

    // Every supported ask is answered from the slot's material.
    for ask in fixture.supported_asks() {
        let slot = CountingSlot::holding(Ok(signed.material.clone()), signed.permissions.clone());
        let authorization = adapter
            .authorize(&slot, &ask)
            .unwrap_or_else(|outcome| panic!("{ask:?} is answered, not {outcome}"));
        assert!(!authorization.scheme.is_empty() && !authorization.token.expose().is_empty());
        assert!(
            !format!("{authorization:?}").contains(authorization.token.expose()),
            "an authorization never prints its token"
        );
        assert!(slot.reads() >= 1, "the material came from the slot");
    }

    // An unsupported ask is refused before the material or the provider.
    let before = fixture.requests();
    for ask in fixture.unsupported_asks() {
        let slot = CountingSlot::new(Ok(signed.material.clone()));
        assert_eq!(
            adapter.authorize(&slot, &ask).map(|_| ()),
            Err(Outcome::Unsupported),
            "{ask:?} is unsupported"
        );
        assert_eq!(slot.reads(), 0, "the material was not read");
    }
    assert_eq!(fixture.requests(), before, "the provider was not reached");

    // Foreign material, and a slot the lobby closed, are refused unreached.
    for material in [
        Ok(SealedMaterial::new(
            b"material some other adapter wrote".to_vec(),
        )),
        Ok(SealedMaterial::new(Vec::new())),
        Err(Outcome::Revoked),
    ] {
        for ask in fixture.supported_asks() {
            let slot = CountingSlot::new(material.clone());
            assert!(
                adapter.authorize(&slot, &ask).is_err(),
                "{ask:?} is refused from material the adapter did not encode"
            );
        }
        let slot = CountingSlot::new(material.clone());
        assert!(
            adapter.renew(&slot).is_err(),
            "a renewal is refused from material the adapter did not encode"
        );
        assert!(!slot.marked(), "nothing was marked");
        assert!(
            adapter.revoke(&CountingSlot::new(material)).is_err(),
            "a revocation is refused from material the adapter did not encode"
        );
    }
    assert_eq!(fixture.requests(), before, "the provider was not reached");

    // Material that is not due is fresh, and nothing reaches the provider.
    let slot = CountingSlot::holding(Ok(signed.material.clone()), signed.permissions.clone());
    let fresh = adapter.renew(&slot).expect("fresh material is no failure");
    assert!(fresh.renew_in_secs > 0, "{fresh:?}");
    assert!(slot.steps().is_empty(), "nothing was marked");
    assert_eq!(slot.material(), Ok(signed.material.clone()));
    assert_eq!(fixture.requests(), before, "the provider was not reached");

    // Cancelling a sign-in lets go of it: its return completes nothing.
    let cancelled = SignInRequest {
        state: "conformance-state-cancelled".to_owned(),
        ..request
    };
    fixture.approve_as(&account);
    let start = adapter.begin_sign_in(&cancelled).expect("a sign-in starts");
    let returned = SignInReturn {
        query: fixture.browse(&start),
    };
    adapter.cancel_sign_in(&cancelled);
    assert!(
        adapter.complete_sign_in(&cancelled, &returned).is_err(),
        "a cancelled sign-in completes nothing"
    );
}

/// Runs the renewal suite against `adapter`, whose provider `fixture` fakes.
pub fn renewal(adapter: &dyn CredentialAdapter, fixture: &dyn RenewalFixture) {
    fixture.issue_briefly();

    // A due renewal marks, presents the secret once, and commits on top of
    // what it read.
    let signed = signed_in(adapter, fixture, "renewal-state-1");
    let first = fixture.live_secret().expect("a live secret");
    let slot = CountingSlot::holding(Ok(signed.material.clone()), signed.permissions.clone());
    adapter.renew(&slot).expect("renewed");
    assert_eq!(
        slot.steps(),
        ["mark", "commit"],
        "marked before, committed after"
    );
    assert!(!slot.marked(), "the commit cleared the mark");
    assert_eq!(fixture.renewals_presented(), std::slice::from_ref(&first));
    let renewed = slot.material().expect("material");
    assert_ne!(renewed, signed.material, "new material was kept");
    let second = fixture.live_secret().expect("a rotated secret");
    assert_ne!(second, first, "the provider rotated the secret");

    // The next renewal presents the rotated secret, never the spent one.
    adapter.renew(&slot).expect("renewed again");
    assert_eq!(
        fixture.renewals_presented(),
        [first.clone(), second.clone()]
    );
    assert_eq!(slot.steps(), ["mark", "commit", "mark", "commit"]);

    // Two renewals racing on one credential present one secret. The first
    // is held at the provider before the provider reads it: the mark is
    // already on the material. The second, given time to reach the
    // provider too, waits for the first instead, and once the first
    // committed it finds the new material fresh.
    let racing = CountingSlot::holding(slot.material(), signed.permissions.clone());
    let before = fixture.renewals_presented().len();
    fixture.issue_lastingly();
    let (listener, route) = fixture.renewals();
    let arrived = listener.count(route);
    std::thread::scope(|scope| {
        let hold = listener.hold(route);
        let one = scope.spawn(|| adapter.renew(&racing));
        listener.wait_for(route, arrived + 1);
        assert!(
            racing.marked(),
            "the material is marked while its secret is in flight"
        );
        assert_eq!(
            fixture.renewals_presented().len(),
            before,
            "the provider has not read the secret yet"
        );
        let two = scope.spawn(|| adapter.renew(&racing));
        std::thread::sleep(RACE_WINDOW);
        drop(hold);
        let answers = [one.join().expect("one"), two.join().expect("two")];
        assert!(
            answers.iter().all(Result::is_ok),
            "the second renewal waited for the first: {answers:?}"
        );
    });
    assert_eq!(
        fixture.renewals_presented().len(),
        before + 1,
        "one secret was presented for two renewals"
    );
    assert_eq!(racing.steps(), ["mark", "commit"]);
    fixture.issue_briefly();

    // A provider that asks for fewer requests spends nothing.
    let signed = signed_in(adapter, fixture, "renewal-state-limited");
    let limited = CountingSlot::holding(Ok(signed.material.clone()), signed.permissions.clone());
    fixture.rate_limit(true);
    assert_eq!(
        adapter.renew(&limited).map(|_| ()),
        Err(Outcome::RateLimited)
    );
    assert_eq!(limited.steps(), ["mark", "unmark"]);
    assert_eq!(limited.material(), Ok(signed.material), "nothing changed");
    fixture.rate_limit(false);
    adapter.renew(&limited).expect("renewed once allowed");

    // Revoking presents the current secret.
    let live = fixture.live_secret().expect("a live secret");
    let revoking = CountingSlot::new(limited.material());
    assert_eq!(adapter.revoke(&revoking), Ok(Revocation::Confirmed));
    assert_eq!(fixture.revocations().last(), Some(&live));

    // New material the slot cannot keep leaves the mark: the outcome is
    // unknown, and the spent secret is never offered again.
    let signed = signed_in(adapter, fixture, "renewal-state-2");
    let unkept = CountingSlot::holding(Ok(signed.material.clone()), signed.permissions.clone());
    unkept.refuse_commits();
    assert_eq!(adapter.renew(&unkept).map(|_| ()), Err(Outcome::Unknown));
    assert!(unkept.marked(), "the mark stays");
    assert_eq!(unkept.steps(), ["mark"]);

    // A provider that refuses the renewal: the credential needs signing in
    // again, and nothing was spent.
    let signed = signed_in(adapter, fixture, "renewal-state-3");
    fixture.refuse();
    let refused = CountingSlot::holding(Ok(signed.material.clone()), signed.permissions.clone());
    assert_eq!(
        adapter.renew(&refused).map(|_| ()),
        Err(Outcome::NeedsSignIn)
    );
    assert_eq!(refused.steps(), ["mark", "unmark"]);
    assert_eq!(refused.material(), Ok(signed.material));
}
