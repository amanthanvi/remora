//! In-process SSH server used by host-key trust tests.
//!
//! Real `russh` handshake against a known host key, so the trust policy is
//! exercised end-to-end (`check_server_key` → accept/reject → auth) instead of
//! being asserted from a mocked callback. No channels are serviced, which is
//! enough for every test that only cares about the connect phase.
//!
//! Two capabilities exist specifically for the trust tests:
//!
//! - **Per-connection host keys.** Each accepted connection is driven by
//!   `russh::server::run_stream` with its own `Config`, so one listening
//!   address can present key A to the first client and key B to the second.
//!   That is the only way to reproduce a first-use race on a single
//!   `host:port`, which is what the trust store is keyed by.
//! - **An authentication counter.** A rejected host key must abort *before*
//!   any credential is offered, so tests assert the server observed zero
//!   authentication attempts.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use rand_core::{OsRng, RngCore};
use russh::keys::ssh_key::private::Ed25519Keypair;
use russh::keys::{HashAlg, PrivateKey};
use russh::server::{Auth, Config, Handler};
use tokio::net::TcpListener;

use crate::terminal::{SshTrustStoreError, TerminalSshTrustBackend, TerminalSshTrustStore};

/// Generate an ephemeral Ed25519 host key for one test.
pub(crate) fn test_host_key() -> PrivateKey {
    let mut seed = [0_u8; 32];
    OsRng.fill_bytes(&mut seed);
    Ed25519Keypair::from_seed(&seed).into()
}

/// Return the SHA-256 fingerprint in the same format the client callback sees.
pub(crate) fn host_key_fingerprint(key: &PrivateKey) -> String {
    format!("{}", key.public_key().fingerprint(HashAlg::Sha256))
}

/// Volatile stand-in for the platform keychain/EncryptedSharedPreferences
/// trust backend.
#[derive(Default)]
pub(crate) struct InMemoryTrustBackend {
    entries: std::sync::Mutex<std::collections::HashMap<(String, u16), String>>,
    /// When set, every read fails instead of answering — models a keychain
    /// that is locked or otherwise unavailable.
    fail_reads: AtomicBool,
    /// When set, writes fail after authentication — models a keychain or
    /// encrypted preferences store that cannot durably record a first-use pin.
    fail_writes: AtomicBool,
    /// When set, removals fail — models a store that cannot durably delete a pin.
    fail_removes: AtomicBool,
}

impl InMemoryTrustBackend {
    pub(crate) fn set_failing(&self, failing: bool) {
        self.fail_reads.store(failing, Ordering::SeqCst);
    }

    pub(crate) fn set_write_failing(&self, failing: bool) {
        self.fail_writes.store(failing, Ordering::SeqCst);
    }

    pub(crate) fn set_remove_failing(&self, failing: bool) {
        self.fail_removes.store(failing, Ordering::SeqCst);
    }
}

impl TerminalSshTrustBackend for InMemoryTrustBackend {
    fn read(&self, host: String, port: u16) -> Result<Option<String>, SshTrustStoreError> {
        if self.fail_reads.load(Ordering::SeqCst) {
            return Err(SshTrustStoreError::Unavailable {
                detail: "test backend read failure".to_string(),
            });
        }
        Ok(self.entries.lock().unwrap().get(&(host, port)).cloned())
    }
    fn write(
        &self,
        host: String,
        port: u16,
        fingerprint: String,
    ) -> Result<(), SshTrustStoreError> {
        if self.fail_writes.load(Ordering::SeqCst) {
            return Err(SshTrustStoreError::Unavailable {
                detail: "test backend write failure".to_string(),
            });
        }
        self.entries
            .lock()
            .unwrap()
            .insert((host, port), fingerprint);
        Ok(())
    }
    fn remove(&self, host: String, port: u16) -> Result<(), SshTrustStoreError> {
        if self.fail_removes.load(Ordering::SeqCst) {
            return Err(SshTrustStoreError::Unavailable {
                detail: "test backend remove failure".to_string(),
            });
        }
        self.entries.lock().unwrap().remove(&(host, port));
        Ok(())
    }
}

/// Adapter so a test can retain a handle to the backend it installed.
struct SharedBackend(Arc<InMemoryTrustBackend>);

impl TerminalSshTrustBackend for SharedBackend {
    fn read(&self, host: String, port: u16) -> Result<Option<String>, SshTrustStoreError> {
        self.0.read(host, port)
    }
    fn write(
        &self,
        host: String,
        port: u16,
        fingerprint: String,
    ) -> Result<(), SshTrustStoreError> {
        self.0.write(host, port, fingerprint)
    }
    fn remove(&self, host: String, port: u16) -> Result<(), SshTrustStoreError> {
        self.0.remove(host, port)
    }
}

/// A trust store backed by [`InMemoryTrustBackend`].
pub(crate) fn in_memory_trust_store() -> Arc<TerminalSshTrustStore> {
    in_memory_trust_store_with_backend().0
}

/// A trust store plus a handle to its backend, so a test can flip the backend
/// into the failing state or inspect it directly.
pub(crate) fn in_memory_trust_store_with_backend()
-> (Arc<TerminalSshTrustStore>, Arc<InMemoryTrustBackend>) {
    let backend = Arc::new(InMemoryTrustBackend::default());
    let store = Arc::new(TerminalSshTrustStore::new(Box::new(SharedBackend(
        Arc::clone(&backend),
    ))));
    (store, backend)
}

#[derive(Clone)]
struct CountingHandler {
    auth_attempts: Arc<AtomicUsize>,
    accept_auth: bool,
}

impl CountingHandler {
    fn record(&self) -> Auth {
        self.auth_attempts.fetch_add(1, Ordering::SeqCst);
        if self.accept_auth {
            Auth::Accept
        } else {
            Auth::Reject {
                proceed_with_methods: None,
                partial_success: false,
            }
        }
    }
}

impl Handler for CountingHandler {
    type Error = russh::Error;

    async fn auth_password(&mut self, _user: &str, _password: &str) -> Result<Auth, Self::Error> {
        Ok(self.record())
    }

    async fn auth_publickey(
        &mut self,
        _user: &str,
        _key: &russh::keys::PublicKey,
    ) -> Result<Auth, Self::Error> {
        Ok(self.record())
    }
}

/// A running loopback SSH server bound to an ephemeral port.
pub(crate) struct TestSshServer {
    pub(crate) host: String,
    pub(crate) port: u16,
    /// SHA-256 fingerprint of the host key the first connection is served.
    pub(crate) fingerprint: String,
    auth_attempts: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl TestSshServer {
    /// Serve `host_key` to every connection.
    pub(crate) async fn start(host_key: PrivateKey) -> Self {
        Self::start_with_keys(&[host_key], true).await
    }

    /// Serve `host_keys[i]` to the i-th connection, reusing the last entry
    /// once the list is exhausted. `accept_auth` controls whether the server
    /// accepts credentials, so a test can assert nothing is pinned when
    /// authentication fails.
    pub(crate) async fn start_with_keys(host_keys: &[PrivateKey], accept_auth: bool) -> Self {
        let fingerprint =
            host_key_fingerprint(host_keys.first().expect("test server requires a host key"));
        let configs: Vec<Arc<Config>> = host_keys
            .iter()
            .map(|key| {
                Arc::new(Config {
                    inactivity_timeout: Some(std::time::Duration::from_secs(30)),
                    auth_rejection_time: std::time::Duration::from_millis(50),
                    auth_rejection_time_initial: Some(std::time::Duration::from_millis(0)),
                    keys: vec![key.clone()],
                    ..Default::default()
                })
            })
            .collect();

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback listener");
        let port = listener.local_addr().expect("listener addr").port();
        let auth_attempts = Arc::new(AtomicUsize::new(0));

        let task = {
            let auth_attempts = Arc::clone(&auth_attempts);
            tokio::spawn(async move {
                let mut accepted = 0usize;
                while let Ok((stream, _)) = listener.accept().await {
                    let config = Arc::clone(&configs[accepted.min(configs.len() - 1)]);
                    accepted += 1;
                    let handler = CountingHandler {
                        auth_attempts: Arc::clone(&auth_attempts),
                        accept_auth,
                    };
                    tokio::spawn(async move {
                        // Each connection owns its session; an error here is
                        // the client hanging up after a rejected host key,
                        // which is exactly what several tests provoke.
                        if let Ok(session) =
                            russh::server::run_stream(config, stream, handler).await
                        {
                            let _ = session.await;
                        }
                    });
                }
            })
        };

        Self {
            host: "127.0.0.1".to_string(),
            port,
            fingerprint,
            auth_attempts,
            task,
        }
    }

    /// How many times a client offered credentials. A host-key rejection must
    /// leave this at zero.
    pub(crate) fn auth_attempts(&self) -> usize {
        self.auth_attempts.load(Ordering::SeqCst)
    }
}

impl Drop for TestSshServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
