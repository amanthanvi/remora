//! Shared SSH host-key trust policy.
//!
//! Every SSH path in the app — interactive terminal, app-server bootstrap,
//! SSH-bridge connect, and automatic background reconnect — routes its
//! `russh` `check_server_key` decision through [`HostKeyVerifier`] so the
//! policy is written once, in Rust:
//!
//! - a pin exists and the presented fingerprint matches → accept
//!   ([`HostKeyDecision::Matches`]);
//! - no pin exists and first-use trust is allowed → accept, and record the
//!   fingerprint after the handshake *and authentication* succeed
//!   ([`HostKeyDecision::TrustOnFirstUse`]);
//! - a pin exists and the presented fingerprint differs → **always** reject
//!   ([`HostKeyDecision::Mismatch`]), including on automatic reconnect. A
//!   changed host key is never auto-accepted;
//! - no pin exists and first-use trust is not allowed → reject
//!   ([`HostKeyDecision::Untrusted`]).
//!
//! Storage is platform-owned (iOS Keychain / Android EncryptedSharedPreferences)
//! behind [`crate::terminal::TerminalSshTrustBackend`]. The terminal path
//! passes its store explicitly at session-open time; the app-server paths have
//! no natural place to thread a store through, so platforms register one
//! process-wide at startup via
//! [`crate::ffi::ssh::register_ssh_host_trust_store`], which lands here in
//! [`register_host_trust_store`].
//!
//! When no store is registered (headless tools, tests, a platform that has not
//! registered yet) the verifier degrades to the pre-existing behavior: accept
//! when first-use trust is allowed, and record nothing. It never becomes *more*
//! permissive than the pinned policy, because a missing store means there is no
//! pin to violate.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::Mutex as StdMutex;
use std::sync::RwLock;
use std::sync::Weak;

use futures::future::BoxFuture;
use tokio::sync::Mutex;
use tracing::{info, warn};

use super::{SshClient, SshCredentials, SshError, normalize_host_key};
use crate::terminal::{SshTrustStoreError, TerminalSshTrustStore};

/// Process-wide trust store used by SSH paths that cannot thread a store
/// through their call chain (app-server connect, SSH bridge, reconnect).
static GLOBAL_TRUST_STORE: RwLock<Option<Arc<TerminalSshTrustStore>>> = RwLock::new(None);

/// Install the process-wide host-key trust store. Called once per process by
/// each platform at startup. Re-registering replaces the previous store.
pub fn register_host_trust_store(store: Arc<TerminalSshTrustStore>) {
    let mut guard = match GLOBAL_TRUST_STORE.write() {
        Ok(guard) => guard,
        Err(poisoned) => {
            warn!("ssh host trust: recovering poisoned trust-store lock");
            poisoned.into_inner()
        }
    };
    *guard = Some(store);
}

/// Serializes tests that mutate the process-wide store.
#[cfg(test)]
pub(crate) static HOST_TRUST_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Drop the process-wide store so one test's registration cannot leak into
/// another. Hold [`HOST_TRUST_TEST_LOCK`] across register/clear.
#[cfg(test)]
pub(crate) fn clear_host_trust_store() {
    let mut guard = match GLOBAL_TRUST_STORE.write() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    *guard = None;
}

/// The registered process-wide trust store, if any.
pub(crate) fn global_host_trust_store() -> Option<Arc<TerminalSshTrustStore>> {
    match GLOBAL_TRUST_STORE.read() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

/// Outcome of evaluating a presented host-key fingerprint against the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostKeyDecision {
    /// A pin exists and the presented fingerprint matches it.
    Matches,
    /// No pin exists and first-use trust is allowed. The caller records the
    /// fingerprint only after the connection fully succeeds.
    TrustOnFirstUse,
    /// A pin exists and the presented fingerprint differs. Always fatal.
    Mismatch,
    /// No pin exists and first-use trust is not allowed.
    Untrusted,
}

impl HostKeyDecision {
    pub(crate) fn accepts(self) -> bool {
        matches!(self, Self::Matches | Self::TrustOnFirstUse)
    }
}

/// The whole host-key policy, in one place.
///
/// Every SSH path in the app resolves to exactly this function, so there is a
/// single place to audit: a recorded pin that no longer matches is *always*
/// [`HostKeyDecision::Mismatch`], regardless of `allow_first_use`.
fn decide_host_key(
    pinned: Option<&str>,
    allow_first_use: bool,
    fingerprint: &str,
) -> HostKeyDecision {
    match pinned {
        Some(expected) if expected == fingerprint => HostKeyDecision::Matches,
        Some(_) => HostKeyDecision::Mismatch,
        None if allow_first_use => HostKeyDecision::TrustOnFirstUse,
        None => HostKeyDecision::Untrusted,
    }
}

/// Read the recorded pin, mapping a storage failure to a fail-closed error.
fn lookup_pin(
    store: Option<&TerminalSshTrustStore>,
    host: &str,
    port: u16,
) -> Result<Option<String>, SshError> {
    let Some(store) = store else {
        return Ok(None);
    };
    store
        .lookup(host, port)
        .map_err(|error| SshError::HostKeyStoreUnavailable {
            host: host.to_string(),
            port,
            message: error.to_string(),
        })
}

/// Per-`host:port` locks guarding the trust-on-first-use claim.
///
/// A first contact reads "no pin", runs a handshake, authenticates, and only
/// then records. Without serialization two concurrent first contacts to the
/// same address both observe "no pin" and both record, so the loser can
/// overwrite the winner's pin with a different key. Connections that already
/// have a pin never take this lock — they only compare, never write.
type HostLocks<T> = LazyLock<StdMutex<HashMap<(String, u16), Weak<T>>>>;

static TOFU_LOCKS: HostLocks<Mutex<()>> = LazyLock::new(|| StdMutex::new(HashMap::new()));

/// Per-host mutation generations shared by first-use recording and platform
/// pin removal. A verifier retains its fence across authentication, so an
/// intervening forget/unpin cannot be followed by that verifier's stale write.
static TRUST_MUTATION_FENCES: HostLocks<StdMutex<u64>> =
    LazyLock::new(|| StdMutex::new(HashMap::new()));

fn tofu_lock(host: &str, port: u16) -> Arc<Mutex<()>> {
    let mut guard = match TOFU_LOCKS.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.retain(|_, lock| lock.strong_count() > 0);
    let key = (host.to_string(), port);
    if let Some(lock) = guard.get(&key).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(Mutex::new(()));
    guard.insert(key, Arc::downgrade(&lock));
    lock
}

fn trust_mutation_fence(host: &str, port: u16) -> Arc<StdMutex<u64>> {
    let mut guard = match TRUST_MUTATION_FENCES.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.retain(|_, fence| fence.strong_count() > 0);
    let key = (host.to_string(), port);
    if let Some(fence) = guard.get(&key).and_then(Weak::upgrade) {
        return fence;
    }
    let fence = Arc::new(StdMutex::new(0));
    guard.insert(key, Arc::downgrade(&fence));
    fence
}

fn lock_trust_generation(fence: &StdMutex<u64>) -> std::sync::MutexGuard<'_, u64> {
    match fence.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[derive(Clone)]
struct TrustMutationClaim {
    fence: Arc<StdMutex<u64>>,
    generation: u64,
}

pub(crate) fn pin_host_trust(
    store: &TerminalSshTrustStore,
    host: &str,
    port: u16,
    fingerprint: String,
) -> Result<(), SshTrustStoreError> {
    let host = normalize_host_key(host);
    let fence = trust_mutation_fence(&host, port);
    let mut generation = lock_trust_generation(&fence);
    *generation = generation.wrapping_add(1);
    store.write_pin(&host, port, fingerprint)
}

pub(crate) fn unpin_host_trust(
    store: &TerminalSshTrustStore,
    host: &str,
    port: u16,
) -> Result<(), SshTrustStoreError> {
    let host = normalize_host_key(host);
    let fence = trust_mutation_fence(&host, port);
    let mut generation = lock_trust_generation(&fence);
    *generation = generation.wrapping_add(1);
    store.remove_pin(&host, port)
}

/// Evaluates presented host keys for one `host:port` against the trust store.
pub(crate) struct HostKeyVerifier {
    host: String,
    port: u16,
    pinned: Option<String>,
    allow_first_use: bool,
    store: Option<Arc<TerminalSshTrustStore>>,
    observed: Arc<Mutex<Option<String>>>,
    mutation_fence: Arc<StdMutex<u64>>,
    mutation_generation: u64,
}

impl HostKeyVerifier {
    /// Build a verifier against an explicitly supplied store. Used by the
    /// terminal path, which already owns a per-open store handle, and by tests.
    ///
    /// Fails closed when the store cannot be read: without a trustworthy
    /// answer we cannot distinguish "new host" from "pinned host", and
    /// guessing "new host" is the downgrade pinning exists to prevent.
    #[cfg(test)]
    pub(crate) fn try_with_store(
        store: Option<Arc<TerminalSshTrustStore>>,
        host: &str,
        port: u16,
        allow_first_use: bool,
    ) -> Result<Self, SshError> {
        Self::try_with_store_claim(store, host, port, allow_first_use, None)
    }

    fn try_with_store_claim(
        store: Option<Arc<TerminalSshTrustStore>>,
        host: &str,
        port: u16,
        allow_first_use: bool,
        mutation_claim: Option<TrustMutationClaim>,
    ) -> Result<Self, SshError> {
        let host = normalize_host_key(host);
        let mutation_fence = mutation_claim
            .as_ref()
            .map(|claim| Arc::clone(&claim.fence))
            .unwrap_or_else(|| trust_mutation_fence(&host, port));
        let (pinned, mutation_generation) = {
            let generation = lock_trust_generation(&mutation_fence);
            (
                lookup_pin(store.as_deref(), &host, port)?,
                mutation_claim
                    .as_ref()
                    .map(|claim| claim.generation)
                    .unwrap_or(*generation),
            )
        };
        Ok(Self {
            host,
            port,
            pinned,
            allow_first_use,
            store,
            observed: Arc::new(Mutex::new(None)),
            mutation_fence,
            mutation_generation,
        })
    }

    /// Policy decision for one presented fingerprint, without running a
    /// handshake. Used by the decision-table test; the live path goes through
    /// [`Self::callback`], which calls the same [`decide_host_key`].
    #[cfg(test)]
    pub(crate) fn decide(&self, fingerprint: &str) -> HostKeyDecision {
        decide_host_key(self.pinned.as_deref(), self.allow_first_use, fingerprint)
    }

    /// The `russh` host-key callback for this verifier. Also records the
    /// presented fingerprint so a successful first-use connect can pin it.
    #[allow(
        clippy::type_complexity,
        reason = "mirrors the boxed async callback shape SshClient::connect accepts"
    )]
    pub(crate) fn callback(&self) -> Box<dyn Fn(&str) -> BoxFuture<'static, bool> + Send + Sync> {
        let pinned = self.pinned.clone();
        let allow_first_use = self.allow_first_use;
        let observed = Arc::clone(&self.observed);
        let host = self.host.clone();
        let port = self.port;
        Box::new(move |fingerprint| {
            let fingerprint = fingerprint.to_string();
            let observed = Arc::clone(&observed);
            let pinned = pinned.clone();
            let host = host.clone();
            Box::pin(async move {
                *observed.lock().await = Some(fingerprint.clone());
                let decision = decide_host_key(pinned.as_deref(), allow_first_use, &fingerprint);
                match decision {
                    HostKeyDecision::Mismatch => warn!(
                        "ssh host trust: refusing changed host key host={host} port={port} presented={fingerprint}"
                    ),
                    HostKeyDecision::Untrusted => warn!(
                        "ssh host trust: refusing unknown host key host={host} port={port} presented={fingerprint}"
                    ),
                    HostKeyDecision::TrustOnFirstUse | HostKeyDecision::Matches => {}
                }
                decision.accepts()
            })
        })
    }

    /// The fingerprint presented during the last handshake, if the callback ran.
    pub(crate) async fn observed_fingerprint(&self) -> Option<String> {
        self.observed.lock().await.clone()
    }

    /// Persist the observed fingerprint when this connect was a trust-on-first-use.
    ///
    /// Call only after the connection *and* authentication succeeded, so a
    /// half-open handshake from an unauthenticated peer cannot install a pin.
    ///
    /// The write is a compare-and-set, not a blind overwrite: the store is
    /// re-read immediately before recording, and a pin that appeared in the
    /// meantime (another first-use connection that won the race, or any other
    /// writer) is authoritative. If that pin disagrees with what this
    /// connection was shown, this connection is talking to a different key
    /// than the one now trusted, so it fails closed instead of overwriting.
    pub(crate) async fn record_trust_on_first_use(&self) -> Result<(), SshError> {
        let (Some(store), None) = (self.store.as_ref(), self.pinned.as_deref()) else {
            return Ok(());
        };
        if !self.allow_first_use {
            return Ok(());
        }
        let Some(fingerprint) = self.observed_fingerprint().await else {
            return Ok(());
        };
        let generation = lock_trust_generation(&self.mutation_fence);
        if *generation != self.mutation_generation {
            let pinned = lookup_pin(Some(store.as_ref()), &self.host, self.port)?;
            warn!(
                "ssh host trust: refusing a stale first-use pin after trust changed host={} port={} presented={}",
                self.host, self.port, fingerprint
            );
            return Err(SshError::HostKeyVerification {
                host: self.host.clone(),
                port: self.port,
                fingerprint,
                pinned,
            });
        }
        if let Some(winner) = lookup_pin(Some(store.as_ref()), &self.host, self.port)? {
            if winner == fingerprint {
                // Someone recorded the same key first; nothing to do.
                return Ok(());
            }
            warn!(
                "ssh host trust: refusing to overwrite a concurrently recorded pin host={} port={} pinned={} presented={}",
                self.host, self.port, winner, fingerprint
            );
            return Err(SshError::HostKeyVerification {
                host: self.host.clone(),
                port: self.port,
                fingerprint,
                pinned: Some(winner),
            });
        }
        info!(
            "ssh host trust: recording first-use pin host={} port={} fingerprint={}",
            self.host, self.port, fingerprint
        );
        store
            .write_pin(&self.host, self.port, fingerprint)
            .map_err(|error| SshError::HostKeyStoreUnavailable {
                host: self.host.clone(),
                port: self.port,
                message: error.to_string(),
            })?;
        Ok(())
    }

    /// Attach host/port/pin context to a bare host-key rejection so platform
    /// error surfaces can tell "changed key" from "unknown key".
    pub(crate) fn enrich_error(&self, error: SshError) -> SshError {
        match error {
            SshError::HostKeyVerification { fingerprint, .. } => SshError::HostKeyVerification {
                host: self.host.clone(),
                port: self.port,
                fingerprint,
                pinned: self.pinned.clone(),
            },
            other => other,
        }
    }
}

/// Stable, machine-parsable message for a rejected host key.
///
/// The `host-key-changed:` / `unknown-host:` prefixes mirror the detail strings
/// the terminal path already emits, so platform error surfaces can key off one
/// convention.
pub(crate) fn host_key_error_message(
    host: &str,
    port: u16,
    fingerprint: &str,
    pinned: Option<&str>,
) -> String {
    match pinned {
        Some(pinned) => format!(
            "host-key-changed:{host}:{port}:{fingerprint} — the SSH host key for {host}:{port} \
             changed (previously trusted {pinned}). Remora refused to connect. If the change is \
             expected, forget the saved fingerprint for this host and connect again."
        ),
        None => format!(
            "unknown-host:{host}:{port}:{fingerprint} — the SSH host key for {host}:{port} is \
             not trusted."
        ),
    }
}

/// Connect to `credentials.host:credentials.port` under the shared host-key
/// trust policy.
///
/// `allow_first_use` means "trust this host key if we have never seen one for
/// this host:port" — it does **not** mean "accept any key". A recorded
/// fingerprint that no longer matches always fails closed.
pub(crate) async fn connect_with_host_trust(
    credentials: SshCredentials,
    allow_first_use: bool,
) -> Result<SshClient, SshError> {
    connect_with_trust_store(global_host_trust_store(), credentials, allow_first_use).await
}

/// [`connect_with_host_trust`] against an explicitly supplied store. The
/// terminal path uses this because it owns a per-session store handle.
///
/// A connect that could record a pin (first contact with first-use trust
/// allowed) is serialized per `host:port`, so concurrent first contacts cannot
/// both claim the pin. Connects that already have a pin skip the lock: they
/// only compare fingerprints and never write.
pub(crate) async fn connect_with_trust_store(
    store: Option<Arc<TerminalSshTrustStore>>,
    credentials: SshCredentials,
    allow_first_use: bool,
) -> Result<SshClient, SshError> {
    let host = normalize_host_key(&credentials.host);
    let port = credentials.port;
    let (initial_pin, mutation_claim) = if store.is_some() {
        let fence = trust_mutation_fence(&host, port);
        let generation = lock_trust_generation(&fence);
        let initial_pin = lookup_pin(store.as_deref(), &host, port)?;
        let claim = TrustMutationClaim {
            fence: Arc::clone(&fence),
            generation: *generation,
        };
        drop(generation);
        (initial_pin, Some(claim))
    } else {
        (None, None)
    };
    let may_record = allow_first_use && store.is_some() && initial_pin.is_none();
    if !may_record {
        return connect_verified(store, credentials, allow_first_use, mutation_claim).await;
    }
    let lock = tofu_lock(&host, port);
    let _claim = lock.lock().await;
    // Re-read under the lock: another first contact may have recorded a pin
    // between the check above and acquiring the claim, in which case this
    // connect is now an ordinary pinned connect.
    connect_verified(store, credentials, allow_first_use, mutation_claim).await
}

/// One connect attempt under an already-decided serialization policy.
async fn connect_verified(
    store: Option<Arc<TerminalSshTrustStore>>,
    credentials: SshCredentials,
    allow_first_use: bool,
    mutation_claim: Option<TrustMutationClaim>,
) -> Result<SshClient, SshError> {
    let verifier = HostKeyVerifier::try_with_store_claim(
        store,
        &credentials.host,
        credentials.port,
        allow_first_use,
        mutation_claim,
    )?;
    match SshClient::connect(credentials, verifier.callback()).await {
        Ok(client) => {
            if let Err(error) = verifier.record_trust_on_first_use().await {
                // We authenticated, but could not establish durable trust for
                // this host. Tear the session down rather than hand back a
                // client that may silently return to first-use trust later.
                client.disconnect().await;
                return Err(error);
            }
            Ok(client)
        }
        Err(error) => Err(verifier.enrich_error(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ssh::SshAuth;
    use crate::ssh::test_server::{
        TestSshServer, host_key_fingerprint, in_memory_trust_store as store,
        in_memory_trust_store_with_backend, test_host_key,
    };
    use crate::terminal::SshTrustStoreError;

    /// The pin currently recorded for the server's address.
    fn pin_for(store: &TerminalSshTrustStore, server: &TestSshServer) -> Option<String> {
        store
            .pinned(server.host.clone(), server.port)
            .expect("test store reads never fail unless the test asks them to")
    }

    fn password_credentials(server: &TestSshServer) -> SshCredentials {
        SshCredentials {
            host: server.host.clone(),
            port: server.port,
            username: "tester".to_string(),
            auth: SshAuth::Password("hunter2".to_string()),
            unlock_macos_keychain: false,
        }
    }

    /// Mirror of [`connect_with_host_trust`] against an explicit store. The
    /// connected client is dropped — every assertion here is about the trust
    /// decision, and `SshClient` is not `Debug`.
    async fn connect(
        store: Option<Arc<TerminalSshTrustStore>>,
        credentials: SshCredentials,
        allow_first_use: bool,
    ) -> Result<(), SshError> {
        let client = connect_with_trust_store(store, credentials, allow_first_use).await?;
        client.disconnect().await;
        Ok(())
    }

    #[test]
    fn decision_table_never_auto_accepts_a_changed_key() {
        let store = store();
        store
            .pin("pinned.example".into(), 22, "SHA256:pin".into())
            .unwrap();

        let known =
            HostKeyVerifier::try_with_store(Some(store.clone()), "pinned.example", 22, true)
                .expect("store reads succeed");
        assert_eq!(known.decide("SHA256:pin"), HostKeyDecision::Matches);
        assert_eq!(known.decide("SHA256:other"), HostKeyDecision::Mismatch);

        // `allow_first_use` must not weaken the pinned check.
        let known_strict =
            HostKeyVerifier::try_with_store(Some(store.clone()), "pinned.example", 22, false)
                .expect("store reads succeed");
        assert_eq!(
            known_strict.decide("SHA256:other"),
            HostKeyDecision::Mismatch
        );

        let unknown =
            HostKeyVerifier::try_with_store(Some(store.clone()), "fresh.example", 22, true)
                .expect("store reads succeed");
        assert_eq!(
            unknown.decide("SHA256:whatever"),
            HostKeyDecision::TrustOnFirstUse
        );

        let unknown_strict =
            HostKeyVerifier::try_with_store(Some(store), "fresh.example", 22, false)
                .expect("store reads succeed");
        assert_eq!(
            unknown_strict.decide("SHA256:whatever"),
            HostKeyDecision::Untrusted
        );
    }

    #[test]
    fn tofu_locks_do_not_retain_inactive_host_entries() {
        let first = tofu_lock("weak-lock-test.example", 2222);
        let weak = Arc::downgrade(&first);
        let same = tofu_lock("weak-lock-test.example", 2222);
        assert!(Arc::ptr_eq(&first, &same));

        drop(first);
        drop(same);
        assert!(weak.upgrade().is_none());

        let replacement = tofu_lock("weak-lock-test.example", 2222);
        assert_eq!(Arc::strong_count(&replacement), 1);
    }

    #[tokio::test]
    async fn first_connect_records_the_presented_fingerprint() {
        let server = TestSshServer::start(test_host_key()).await;
        let store = store();

        connect(Some(store.clone()), password_credentials(&server), true)
            .await
            .expect("first-use connect should succeed");

        assert_eq!(
            pin_for(&store, &server),
            Some(server.fingerprint.clone()),
            "trust-on-first-use must durably record the fingerprint"
        );
    }

    #[tokio::test]
    async fn matching_pin_proceeds() {
        let server = TestSshServer::start(test_host_key()).await;
        let store = store();
        store
            .pin(server.host.clone(), server.port, server.fingerprint.clone())
            .unwrap();

        connect(Some(store.clone()), password_credentials(&server), false)
            .await
            .expect("pinned connect should succeed even without first-use trust");
    }

    #[tokio::test]
    async fn changed_host_key_fails_closed_even_when_first_use_is_allowed() {
        // Pin key A, then have the host present key B — the classic
        // man-in-the-middle / re-provisioned-host case.
        let trusted_key = test_host_key();
        let pinned = host_key_fingerprint(&trusted_key);
        let server = TestSshServer::start(test_host_key()).await;
        let store = store();
        store
            .pin(server.host.clone(), server.port, pinned.clone())
            .unwrap();

        let error = connect(Some(store.clone()), password_credentials(&server), true)
            .await
            .expect_err("changed host key must fail closed");
        match error {
            SshError::HostKeyVerification {
                fingerprint,
                pinned: recorded,
                ..
            } => {
                assert_eq!(fingerprint, server.fingerprint);
                assert_eq!(recorded.as_deref(), Some(pinned.as_str()));
            }
            other => panic!("expected HostKeyVerification, got {other:?}"),
        }
        assert_eq!(
            pin_for(&store, &server),
            Some(pinned),
            "a rejected key must never overwrite the stored pin"
        );
        assert_eq!(
            server.auth_attempts(),
            0,
            "a refused host key must abort before any credential is offered"
        );
    }

    #[tokio::test]
    async fn unknown_host_without_first_use_trust_is_rejected() {
        let server = TestSshServer::start(test_host_key()).await;
        let store = store();

        let error = connect(Some(store.clone()), password_credentials(&server), false)
            .await
            .expect_err("unknown host must be rejected when first-use trust is off");
        assert!(
            matches!(error, SshError::HostKeyVerification { .. }),
            "got {error:?}"
        );
        assert_eq!(pin_for(&store, &server), None);
        assert_eq!(
            server.auth_attempts(),
            0,
            "a refused host key must abort before any credential is offered"
        );
    }

    #[tokio::test]
    async fn missing_store_does_not_record_and_still_connects_on_first_use() {
        let server = TestSshServer::start(test_host_key()).await;
        connect(None, password_credentials(&server), true)
            .await
            .expect("connect without a registered store keeps working");
    }

    #[tokio::test]
    async fn concurrent_first_use_connects_cannot_both_claim_the_pin() {
        // One address, two different host keys: connection #1 is served key A
        // and connection #2 key B. Both start with an empty store, so without
        // serialization both would observe "no pin", both would authenticate,
        // and the loser would overwrite the winner's pin — leaving a trusted
        // fingerprint that belongs to whichever key happened to finish last.
        let key_a = test_host_key();
        let key_b = test_host_key();
        let fingerprint_a = host_key_fingerprint(&key_a);
        let fingerprint_b = host_key_fingerprint(&key_b);
        let server = TestSshServer::start_with_keys(&[key_a, key_b], true).await;
        let store = store();

        let first = connect(Some(store.clone()), password_credentials(&server), true);
        let second = connect(Some(store.clone()), password_credentials(&server), true);
        let (first, second) = tokio::join!(first, second);

        let recorded = pin_for(&store, &server).expect("one connection must record a pin");
        assert!(
            recorded == fingerprint_a || recorded == fingerprint_b,
            "recorded pin {recorded} is neither served key"
        );
        // Exactly one may succeed: the winner pins its key, and the other
        // connection is talking to a different key than the one now trusted.
        let outcomes = [first.is_ok(), second.is_ok()];
        assert_eq!(
            outcomes.iter().filter(|ok| **ok).count(),
            1,
            "exactly one concurrent first-use connect may succeed, got {outcomes:?}"
        );
        assert_eq!(
            pin_for(&store, &server),
            Some(recorded),
            "the losing connection must not overwrite the winner's pin"
        );
    }

    #[tokio::test]
    async fn failed_authentication_records_no_pin() {
        // The handshake completes and the host key is accepted under
        // first-use, but the credential is rejected. Pinning here would let an
        // impostor install a trusted fingerprint without ever proving it is
        // the host the user has an account on.
        let server = TestSshServer::start_with_keys(&[test_host_key()], false).await;
        let store = store();

        connect(Some(store.clone()), password_credentials(&server), true)
            .await
            .expect_err("auth failure must fail the connect");

        assert_eq!(
            pin_for(&store, &server),
            None,
            "a connection that never authenticated must not record a pin"
        );
        assert!(
            server.auth_attempts() > 0,
            "the test server should have seen the rejected credential"
        );
    }

    #[tokio::test]
    async fn unpin_fences_a_late_authenticated_first_use_write() {
        let store = store();
        let verifier =
            HostKeyVerifier::try_with_store(Some(store.clone()), "removed.example", 22, true)
                .expect("store reads succeed");
        assert!((verifier.callback())("SHA256:observed").await);

        store
            .unpin("removed.example".to_string(), 22)
            .expect("removal succeeds even without an existing pin");

        let error = verifier
            .record_trust_on_first_use()
            .await
            .expect_err("the pre-removal verifier must not recreate the pin");
        assert!(matches!(error, SshError::HostKeyVerification { .. }));
        assert_eq!(
            store
                .pinned("removed.example".to_string(), 22)
                .expect("store remains readable"),
            None
        );
    }

    #[tokio::test]
    async fn unreadable_trust_store_fails_closed_instead_of_trusting_on_first_use() {
        // A locked keychain must not read as "no pin recorded" — that would
        // silently downgrade an already-pinned host back to first-use trust.
        let server = TestSshServer::start(test_host_key()).await;
        let (store, backend) = in_memory_trust_store_with_backend();
        backend.set_failing(true);

        let error = connect(Some(store.clone()), password_credentials(&server), true)
            .await
            .expect_err("an unreadable trust store must fail closed");
        match error {
            SshError::HostKeyStoreUnavailable { host, port, .. } => {
                assert_eq!(host, server.host);
                assert_eq!(port, server.port);
            }
            other => panic!("expected HostKeyStoreUnavailable, got {other:?}"),
        }
        assert_eq!(
            server.auth_attempts(),
            0,
            "we must not offer credentials to a host we cannot evaluate"
        );

        // Once the store is readable again the same connect proceeds normally.
        backend.set_failing(false);
        connect(Some(store.clone()), password_credentials(&server), true)
            .await
            .expect("connect should succeed once the store is readable");
        assert_eq!(pin_for(&store, &server), Some(server.fingerprint.clone()));
    }

    #[tokio::test]
    async fn unwritable_trust_store_rejects_authenticated_first_use_connection() {
        let server = TestSshServer::start(test_host_key()).await;
        let (store, backend) = in_memory_trust_store_with_backend();
        backend.set_write_failing(true);

        let error = connect(Some(store.clone()), password_credentials(&server), true)
            .await
            .expect_err("a first-use connect must fail when its pin cannot be persisted");
        match error {
            SshError::HostKeyStoreUnavailable { host, port, .. } => {
                assert_eq!(host, server.host);
                assert_eq!(port, server.port);
            }
            other => panic!("expected HostKeyStoreUnavailable, got {other:?}"),
        }
        assert!(
            server.auth_attempts() > 0,
            "write failure should occur only after successful authentication"
        );
        assert_eq!(pin_for(&store, &server), None);
    }

    #[test]
    fn unpin_propagates_trust_store_removal_failure() {
        let (store, backend) = in_memory_trust_store_with_backend();
        store
            .pin("host.example".to_string(), 22, "SHA256:pinned".to_string())
            .expect("test pin succeeds");
        backend.set_remove_failing(true);

        let error = store
            .unpin("host.example".to_string(), 22)
            .expect_err("removal failure should propagate");

        assert!(matches!(error, SshTrustStoreError::Unavailable { .. }));
        backend.set_remove_failing(false);
        assert_eq!(
            store
                .pinned("host.example".to_string(), 22)
                .expect("pin remains readable")
                .as_deref(),
            Some("SHA256:pinned")
        );
    }

    #[test]
    fn error_message_distinguishes_changed_from_unknown() {
        let changed = host_key_error_message("host.example", 22, "SHA256:new", Some("SHA256:old"));
        assert!(changed.starts_with("host-key-changed:host.example:22:SHA256:new"));
        assert!(changed.contains("SHA256:old"));

        let unknown = host_key_error_message("host.example", 22, "SHA256:new", None);
        assert!(unknown.starts_with("unknown-host:host.example:22:SHA256:new"));
    }
}
