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

use russh::keys::{HashAlg, PrivateKey, decode_secret_key};
use russh::server::{Auth, Config, Handler};
use tokio::net::TcpListener;

use crate::terminal::{SshTrustStoreError, TerminalSshTrustBackend, TerminalSshTrustStore};

/// Ed25519 host key "A". Test-only; never used outside `cfg(test)`.
pub(crate) const TEST_HOST_KEY_A: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW
QyNTUxOQAAACDcrxpoNzrXk1wkkshlJIsss1JkaI6kAtflX7SQESrgNwAAAJCl80CKpfNA
igAAAAtzc2gtZWQyNTUxOQAAACDcrxpoNzrXk1wkkshlJIsss1JkaI6kAtflX7SQESrgNw
AAAEC/7O8hj0ZMGflgReW4oBc6MdhimIqxjN3QTeA5n8Qk8tyvGmg3OteTXCSSyGUkiyyz
UmRojqQC1+VftJARKuA3AAAADXJlbW9yYS10ZXN0LWE=
-----END OPENSSH PRIVATE KEY-----
";

/// Ed25519 host key "B" — the "host key changed" counterpart to key A.
pub(crate) const TEST_HOST_KEY_B: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW
QyNTUxOQAAACBxH6kRdnvn52ZVkJGBhE89Yub0dHHBHPK5h4rmrDaPUwAAAJCLx89zi8fP
cwAAAAtzc2gtZWQyNTUxOQAAACBxH6kRdnvn52ZVkJGBhE89Yub0dHHBHPK5h4rmrDaPUw
AAAECwbgfZFAwk1CZANSWs0dMjtPyZJrINXrUPEL2/+R1qanEfqRF2e+fnZlWQkYGETz1i
5vR0ccEc8rmHiuasNo9TAAAADXJlbW9yYS10ZXN0LWI=
-----END OPENSSH PRIVATE KEY-----
";

/// Parse a test host key, returning the key and its SHA-256 fingerprint in the
/// same format the client host-key callback receives.
pub(crate) fn host_key(pem: &str) -> (PrivateKey, String) {
    let key = decode_secret_key(pem, None).expect("decode test host key");
    let fingerprint = format!("{}", key.public_key().fingerprint(HashAlg::Sha256));
    (key, fingerprint)
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
    /// Serve `host_key_pem` to every connection.
    pub(crate) async fn start(host_key_pem: &str) -> Self {
        Self::start_with_keys(&[host_key_pem], true).await
    }

    /// Serve `host_key_pems[i]` to the i-th connection, reusing the last entry
    /// once the list is exhausted. `accept_auth` controls whether the server
    /// accepts credentials, so a test can assert nothing is pinned when
    /// authentication fails.
    pub(crate) async fn start_with_keys(host_key_pems: &[&str], accept_auth: bool) -> Self {
        let parsed: Vec<(PrivateKey, String)> =
            host_key_pems.iter().map(|pem| host_key(pem)).collect();
        let fingerprint = parsed[0].1.clone();
        let configs: Vec<Arc<Config>> = parsed
            .iter()
            .map(|(key, _)| {
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
