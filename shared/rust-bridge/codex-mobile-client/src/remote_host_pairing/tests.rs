use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use super::identity::DecodedPairingCode;
use super::ports::*;
use super::types::*;
use super::*;

const NODE_ID: &str = "2c2b9ae25435c9ec1cdcb51d81cdfe30fbc35be9dcb0e8e311725bf233e81d6f";

struct TestClock(AtomicU64);

impl PairingClock for TestClock {
    fn unix_seconds(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

struct TestIds(AtomicU64);

impl PairingIdSource for TestIds {
    fn next_id(&self, purpose: &'static str) -> String {
        format!("{purpose}-{}", self.0.fetch_add(1, Ordering::SeqCst))
    }
}

#[derive(Default)]
struct MemoryJournal {
    entries: StdMutex<HashMap<RemoteHostId, PairingJournalEntry>>,
    unavailable: AtomicBool,
    fail_next_state: StdMutex<Option<JournalHostState>>,
}

#[async_trait]
impl PairingJournalPort for MemoryJournal {
    async fn load(
        &self,
        host_id: &RemoteHostId,
    ) -> Result<Option<PairingJournalEntry>, JournalError> {
        if self.unavailable.load(Ordering::SeqCst) {
            return Err(JournalError::Unavailable);
        }
        Ok(self.entries.lock().unwrap().get(host_id).cloned())
    }

    async fn compare_and_swap(
        &self,
        host_id: &RemoteHostId,
        expected_revision: Option<u64>,
        replacement: PairingJournalEntry,
    ) -> Result<(), JournalError> {
        if self.unavailable.load(Ordering::SeqCst) {
            return Err(JournalError::Unavailable);
        }
        let mut fail_next_state = self.fail_next_state.lock().unwrap();
        if fail_next_state.as_ref() == Some(&replacement.state) {
            *fail_next_state = None;
            return Err(JournalError::Unavailable);
        }
        drop(fail_next_state);
        let mut entries = self.entries.lock().unwrap();
        let current = entries.get(host_id).map(|entry| entry.revision);
        if current != expected_revision {
            return Err(JournalError::Conflict);
        }
        entries.insert(host_id.clone(), replacement);
        Ok(())
    }
}

#[derive(Default)]
struct MemorySecrets {
    values: StdMutex<HashMap<String, OpaqueCredential>>,
    fail_write: AtomicBool,
    fail_delete: AtomicBool,
    write_count: AtomicUsize,
}

#[async_trait]
impl PairingSecretPort for MemorySecrets {
    async fn read(&self, alias: &str) -> Result<Option<OpaqueCredential>, SecretStoreError> {
        Ok(self.values.lock().unwrap().get(alias).cloned())
    }

    async fn write(
        &self,
        alias: &str,
        credential: OpaqueCredential,
    ) -> Result<(), SecretStoreError> {
        self.write_count.fetch_add(1, Ordering::SeqCst);
        if self.fail_write.load(Ordering::SeqCst) {
            return Err(SecretStoreError::Unavailable);
        }
        self.values
            .lock()
            .unwrap()
            .insert(alias.to_string(), credential);
        Ok(())
    }

    async fn delete(&self, alias: &str) -> Result<(), SecretStoreError> {
        if self.fail_delete.load(Ordering::SeqCst) {
            return Err(SecretStoreError::Unavailable);
        }
        self.values.lock().unwrap().remove(alias);
        Ok(())
    }
}

#[derive(Default)]
struct ScriptedHost {
    inspect_count: AtomicUsize,
    establish_count: AtomicUsize,
    reconnect_count: AtomicUsize,
    confirm_reconnect_count: AtomicUsize,
    revoke_count: AtomicUsize,
    rollback_count: AtomicUsize,
    close_count: AtomicUsize,
    reconnect_already_connected: AtomicBool,
    reconnect_rotates: AtomicBool,
    revoke_deferred: AtomicBool,
    rollback_unavailable: AtomicBool,
    establish_error: StdMutex<Option<HostPortError>>,
    reconnect_error: StdMutex<Option<HostPortError>>,
    confirm_reconnect_error: StdMutex<Option<HostPortError>>,
    revoke_error: StdMutex<Option<HostPortError>>,
    establish_keys: StdMutex<Vec<String>>,
    reconnect_keys: StdMutex<Vec<String>>,
    confirm_reconnect_keys: StdMutex<Vec<String>>,
    revoke_keys: StdMutex<Vec<String>>,
    revoke_credentials: StdMutex<Vec<Vec<u8>>>,
    rollback_keys: StdMutex<Vec<String>>,
}

#[async_trait]
impl RemotePairingHostPort for ScriptedHost {
    async fn inspect(
        &self,
        request: HostInspectRequest,
    ) -> Result<AuthenticatedHostOffer, HostPortError> {
        self.inspect_count.fetch_add(1, Ordering::SeqCst);
        Ok(AuthenticatedHostOffer {
            host_id: request.invite.host_id(),
            suggested_display_name: "Verified Studio".to_string(),
            runtimes: vec![
                RemoteRuntimeOffer {
                    runtime_id: "codex".to_string(),
                    display_name: "Codex".to_string(),
                    available: true,
                    recommended: true,
                },
                RemoteRuntimeOffer {
                    runtime_id: "claude".to_string(),
                    display_name: "Claude Code".to_string(),
                    available: true,
                    recommended: false,
                },
            ],
        })
    }

    async fn establish(
        &self,
        request: HostEstablishRequest,
    ) -> Result<EstablishedRemoteHost, HostPortError> {
        self.establish_count.fetch_add(1, Ordering::SeqCst);
        self.establish_keys
            .lock()
            .unwrap()
            .push(request.idempotency_key.clone());
        if let Some(error) = self.establish_error.lock().unwrap().take() {
            return Err(error);
        }
        assert!(matches!(
            request.invite,
            DecodedPairingCode::DeviceGrantV2(_)
        ));
        Ok(EstablishedRemoteHost {
            already_connected: false,
            connected_runtime_ids: request.selected_runtime_ids,
            credential: Some(OpaqueCredential::new(b"device-bound-grant".to_vec())),
        })
    }

    async fn reconnect(
        &self,
        request: HostReconnectRequest,
    ) -> Result<EstablishedRemoteHost, HostPortError> {
        self.reconnect_count.fetch_add(1, Ordering::SeqCst);
        self.reconnect_keys
            .lock()
            .unwrap()
            .push(request.idempotency_key.clone());
        assert_eq!(
            request.credential.expose_for_adapter(),
            b"device-bound-grant"
        );
        if let Some(error) = self.reconnect_error.lock().unwrap().take() {
            return Err(error);
        }
        Ok(EstablishedRemoteHost {
            already_connected: self.reconnect_already_connected.load(Ordering::SeqCst),
            connected_runtime_ids: request.selected_runtime_ids,
            credential: self
                .reconnect_rotates
                .load(Ordering::SeqCst)
                .then(|| OpaqueCredential::new(b"rotated-device-grant".to_vec())),
        })
    }

    async fn confirm_reconnect(
        &self,
        request: HostConfirmReconnectRequest,
    ) -> Result<(), HostPortError> {
        self.confirm_reconnect_count.fetch_add(1, Ordering::SeqCst);
        self.confirm_reconnect_keys
            .lock()
            .unwrap()
            .push(request.idempotency_key);
        assert_eq!(
            request.credential.expose_for_adapter(),
            b"rotated-device-grant"
        );
        if let Some(error) = self.confirm_reconnect_error.lock().unwrap().take() {
            return Err(error);
        }
        Ok(())
    }

    async fn revoke(
        &self,
        request: HostRevokeRequest,
    ) -> Result<HostCredentialRevocationStatus, HostPortError> {
        self.revoke_count.fetch_add(1, Ordering::SeqCst);
        self.revoke_keys
            .lock()
            .unwrap()
            .push(request.idempotency_key.clone());
        self.revoke_credentials
            .lock()
            .unwrap()
            .push(request.credential.expose_for_adapter().to_vec());
        assert!(matches!(
            request.credential.expose_for_adapter(),
            b"device-bound-grant" | b"rotated-device-grant"
        ));
        if let Some(error) = self.revoke_error.lock().unwrap().take() {
            return Err(error);
        }
        if self.revoke_deferred.load(Ordering::SeqCst) {
            Err(HostPortError::Unavailable)
        } else {
            Ok(HostCredentialRevocationStatus::Confirmed)
        }
    }

    async fn rollback_establish(
        &self,
        _host_id: &RemoteHostId,
        idempotency_key: &str,
    ) -> Result<(), HostPortError> {
        self.rollback_count.fetch_add(1, Ordering::SeqCst);
        self.rollback_keys
            .lock()
            .unwrap()
            .push(idempotency_key.to_string());
        if self.rollback_unavailable.load(Ordering::SeqCst) {
            Err(HostPortError::Unavailable)
        } else {
            Ok(())
        }
    }

    async fn close_local(&self, _host_id: &RemoteHostId) {
        self.close_count.fetch_add(1, Ordering::SeqCst);
    }
}

struct Harness {
    core: Arc<RemoteHostPairing>,
    host: Arc<ScriptedHost>,
    journal: Arc<MemoryJournal>,
    secrets: Arc<MemorySecrets>,
}

fn harness() -> Harness {
    let host = Arc::new(ScriptedHost::default());
    let journal = Arc::new(MemoryJournal::default());
    let secrets = Arc::new(MemorySecrets::default());
    let clock = Arc::new(TestClock(AtomicU64::new(10_000)));
    let ids = Arc::new(TestIds(AtomicU64::new(1)));
    let core = Arc::new(RemoteHostPairing::new(
        host.clone(),
        journal.clone(),
        secrets.clone(),
        clock,
        ids,
    ));
    Harness {
        core,
        host,
        journal,
        secrets,
    }
}

fn v2_code() -> RemotePairingCode {
    let json = serde_json::json!({
        "v": 2,
        "node_id": NODE_ID,
        "invitation_id": URL_SAFE_NO_PAD.encode([3_u8; 16]),
        "secret": URL_SAFE_NO_PAD.encode([4_u8; 32]),
        "expires_at": 10_300,
        "max_runtime_ids": ["claude", "codex"],
        "max_scopes": ["inspect_runtimes", "connect_runtime", "restart_runtime", "self_revoke"],
        "confirmation_mode": "interactive",
        "host_name": "Studio",
        "relay": "https://relay.example"
    })
    .to_string();
    RemotePairingCode {
        encoded: format!("remora-link:v2:{}", URL_SAFE_NO_PAD.encode(json)),
    }
}

fn v1_code() -> RemotePairingCode {
    RemotePairingCode {
        encoded: serde_json::json!({
            "v": 1,
            "node_id": NODE_ID,
            "token": "legacy-bearer",
            "host_name": "Legacy Studio",
            "relay": "https://relay.example"
        })
        .to_string(),
    }
}

async fn inspect_and_pair(harness: &Harness) -> (RemotePairingOffer, RemotePairingOutcome) {
    let offer = harness.core.inspect(v2_code()).await.unwrap();
    let outcome = harness
        .core
        .pair(RemotePairingAcceptance {
            offer_id: offer.offer_id.clone(),
            display_name: None,
            selected_runtime_ids: vec!["codex".to_string()],
        })
        .await
        .unwrap();
    (offer, outcome)
}

#[tokio::test]
async fn legacy_offer_is_classified_but_never_sent_or_upgraded() {
    let harness = harness();
    let offer = harness.core.inspect(v1_code()).await.unwrap();
    assert_eq!(offer.protocol, RemotePairingProtocol::LegacyV1);
    assert_eq!(
        offer.disposition,
        RemotePairingOfferDisposition::RePairRequired
    );
    assert!(offer.runtimes.is_empty());
    assert_eq!(harness.host.inspect_count.load(Ordering::SeqCst), 0);
    assert!(
        harness
            .core
            .offers
            .lock()
            .await
            .get(&offer.offer_id.value)
            .expect("cached classification")
            .invite
            .is_none()
    );

    let outcome = harness
        .core
        .pair(RemotePairingAcceptance {
            offer_id: offer.offer_id,
            display_name: None,
            selected_runtime_ids: Vec::new(),
        })
        .await
        .unwrap();
    assert_eq!(
        outcome,
        RemotePairingOutcome::RePairRequired {
            host_id: RemoteHostId {
                value: format!("alleycat:{NODE_ID}")
            },
            reason: RemoteRePairReason::LegacyBearerCredential,
        }
    );
    assert_eq!(harness.host.establish_count.load(Ordering::SeqCst), 0);
    assert!(harness.journal.entries.lock().unwrap().is_empty());
    assert!(harness.secrets.values.lock().unwrap().is_empty());
}

#[tokio::test]
async fn v2_pairing_commits_one_secret_free_record_and_reconnects_by_host_id() {
    let harness = harness();
    let (offer, outcome) = inspect_and_pair(&harness).await;
    assert_eq!(
        outcome,
        RemotePairingOutcome::Paired {
            host_id: offer.host_id.clone()
        }
    );
    assert_eq!(harness.host.inspect_count.load(Ordering::SeqCst), 1);
    assert_eq!(harness.host.establish_count.load(Ordering::SeqCst), 1);

    let entry = harness
        .journal
        .entries
        .lock()
        .unwrap()
        .get(&offer.host_id)
        .cloned()
        .unwrap();
    assert_eq!(entry.state, JournalHostState::Active);
    assert_eq!(entry.operation_id, None);
    assert_eq!(entry.desired_runtime_ids, vec!["codex"]);
    assert!(!entry.credential_alias.contains(NODE_ID));
    assert!(!entry.display_name.contains("device-bound-grant"));
    assert_eq!(harness.secrets.values.lock().unwrap().len(), 1);
    assert_eq!(harness.secrets.write_count.load(Ordering::SeqCst), 1);

    harness.secrets.fail_write.store(true, Ordering::SeqCst);
    let reconnect = harness.core.reconnect(offer.host_id.clone()).await.unwrap();
    assert_eq!(
        reconnect,
        RemoteReconnectOutcome::Connected {
            host_id: offer.host_id
        }
    );
    assert_eq!(harness.host.reconnect_count.load(Ordering::SeqCst), 1);
    assert_eq!(harness.secrets.write_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn duplicate_concurrent_pair_calls_coalesce() {
    let harness = harness();
    let offer = harness.core.inspect(v2_code()).await.unwrap();
    let acceptance = RemotePairingAcceptance {
        offer_id: offer.offer_id,
        display_name: Some("Studio".to_string()),
        selected_runtime_ids: vec!["codex".to_string()],
    };
    let (left, right) = tokio::join!(
        harness.core.pair(acceptance.clone()),
        harness.core.pair(acceptance)
    );
    let outcomes = [left.unwrap(), right.unwrap()];
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, RemotePairingOutcome::Paired { .. }))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, RemotePairingOutcome::AlreadyPaired { .. }))
            .count(),
        1
    );
    assert_eq!(harness.host.establish_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn interrupted_pair_replays_the_persisted_idempotency_key() {
    let harness = harness();
    let offer = harness.core.inspect(v2_code()).await.unwrap();
    harness.journal.entries.lock().unwrap().insert(
        offer.host_id.clone(),
        PairingJournalEntry {
            revision: 4,
            generation: 2,
            host_id: offer.host_id.clone(),
            protocol: RemotePairingProtocol::DeviceGrantV2,
            display_name: "Verified Studio".to_string(),
            desired_runtime_ids: vec!["codex".to_string()],
            credential_alias: "credential-interrupted".to_string(),
            pending_credential_alias: None,
            operation_id: Some("pair-interrupted".to_string()),
            state: JournalHostState::CommitPending,
        },
    );

    assert_eq!(
        harness
            .core
            .pair(RemotePairingAcceptance {
                offer_id: offer.offer_id,
                display_name: None,
                selected_runtime_ids: vec!["codex".to_string()],
            })
            .await
            .unwrap(),
        RemotePairingOutcome::Paired {
            host_id: offer.host_id.clone(),
        }
    );
    assert_eq!(
        harness.host.establish_keys.lock().unwrap().as_slice(),
        ["pair-interrupted"]
    );
    let entry = harness
        .journal
        .entries
        .lock()
        .unwrap()
        .get(&offer.host_id)
        .cloned()
        .unwrap();
    assert_eq!(entry.generation, 2);
    assert_eq!(entry.credential_alias, "credential-interrupted");
    assert_eq!(entry.operation_id, None);
    assert_eq!(entry.state, JournalHostState::Active);
}

#[tokio::test]
async fn interrupted_pair_rejects_a_changed_intent() {
    let harness = harness();
    let offer = harness.core.inspect(v2_code()).await.unwrap();
    harness.journal.entries.lock().unwrap().insert(
        offer.host_id.clone(),
        PairingJournalEntry {
            revision: 4,
            generation: 2,
            host_id: offer.host_id.clone(),
            protocol: RemotePairingProtocol::DeviceGrantV2,
            display_name: "Verified Studio".to_string(),
            desired_runtime_ids: vec!["claude".to_string()],
            credential_alias: "credential-interrupted".to_string(),
            pending_credential_alias: None,
            operation_id: Some("pair-interrupted".to_string()),
            state: JournalHostState::CommitPending,
        },
    );

    assert_eq!(
        harness
            .core
            .pair(RemotePairingAcceptance {
                offer_id: offer.offer_id,
                display_name: None,
                selected_runtime_ids: vec!["codex".to_string()],
            })
            .await
            .unwrap(),
        RemotePairingOutcome::NeedsRepair {
            host_id: offer.host_id,
            reason: RemotePairingRepairReason::InterruptedCommit,
        }
    );
    assert_eq!(harness.host.establish_count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn ambiguous_establish_failure_replays_the_same_transaction() {
    let harness = harness();
    let offer = harness.core.inspect(v2_code()).await.unwrap();
    let acceptance = RemotePairingAcceptance {
        offer_id: offer.offer_id,
        display_name: None,
        selected_runtime_ids: vec!["codex".to_string()],
    };
    *harness.host.establish_error.lock().unwrap() = Some(HostPortError::Unavailable);

    assert!(matches!(
        harness.core.pair(acceptance.clone()).await,
        Err(RemoteHostPairingError::HostUnavailable)
    ));
    assert_eq!(
        harness
            .journal
            .entries
            .lock()
            .unwrap()
            .get(&offer.host_id)
            .expect("pending transaction")
            .state,
        JournalHostState::CommitPending
    );

    assert_eq!(
        harness.core.pair(acceptance).await.unwrap(),
        RemotePairingOutcome::Paired {
            host_id: offer.host_id,
        }
    );
    let establish_keys = harness.host.establish_keys.lock().unwrap();
    assert_eq!(establish_keys.len(), 2);
    assert_eq!(establish_keys[0], establish_keys[1]);
}

#[tokio::test]
async fn revoke_of_ambiguous_establish_uses_the_original_rollback_key() {
    let harness = harness();
    let offer = harness.core.inspect(v2_code()).await.unwrap();
    *harness.host.establish_error.lock().unwrap() = Some(HostPortError::Unavailable);

    assert!(matches!(
        harness
            .core
            .pair(RemotePairingAcceptance {
                offer_id: offer.offer_id,
                display_name: None,
                selected_runtime_ids: vec!["codex".to_string()],
            })
            .await,
        Err(RemoteHostPairingError::HostUnavailable)
    ));
    assert_eq!(
        harness.core.revoke(offer.host_id.clone()).await.unwrap(),
        RemoteRevokeOutcome::AlreadyRevoked {
            host_id: offer.host_id,
        }
    );
    let establish_keys = harness.host.establish_keys.lock().unwrap();
    let rollback_keys = harness.host.rollback_keys.lock().unwrap();
    assert_eq!(rollback_keys.as_slice(), establish_keys.as_slice());
    assert_eq!(harness.host.revoke_count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn invalid_runtime_selection_never_reaches_host_or_persistence() {
    let harness = harness();
    let offer = harness.core.inspect(v2_code()).await.unwrap();
    let error = harness
        .core
        .pair(RemotePairingAcceptance {
            offer_id: offer.offer_id,
            display_name: None,
            selected_runtime_ids: vec!["not-advertised".to_string()],
        })
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        RemoteHostPairingError::InvalidRuntimeSelection
    ));
    assert_eq!(harness.host.establish_count.load(Ordering::SeqCst), 0);
    assert!(harness.journal.entries.lock().unwrap().is_empty());
}

#[tokio::test]
async fn secure_store_failure_rolls_back_and_surfaces_needs_repair() {
    let harness = harness();
    harness.secrets.fail_write.store(true, Ordering::SeqCst);
    let offer = harness.core.inspect(v2_code()).await.unwrap();
    let outcome = harness
        .core
        .pair(RemotePairingAcceptance {
            offer_id: offer.offer_id,
            display_name: None,
            selected_runtime_ids: vec!["codex".to_string()],
        })
        .await
        .unwrap();
    assert_eq!(
        outcome,
        RemotePairingOutcome::NeedsRepair {
            host_id: offer.host_id.clone(),
            reason: RemotePairingRepairReason::SecureStorageUnavailable,
        }
    );
    assert_eq!(harness.host.rollback_count.load(Ordering::SeqCst), 1);
    let entry = harness
        .journal
        .entries
        .lock()
        .unwrap()
        .get(&offer.host_id)
        .cloned()
        .unwrap();
    assert_eq!(
        entry.state,
        JournalHostState::EnrollmentRolledBack(RemotePairingRepairReason::SecureStorageUnavailable)
    );
}

#[tokio::test]
async fn unavailable_rollback_keeps_enrollment_pending_for_exact_replay() {
    let harness = harness();
    harness.secrets.fail_write.store(true, Ordering::SeqCst);
    harness
        .host
        .rollback_unavailable
        .store(true, Ordering::SeqCst);
    let offer = harness.core.inspect(v2_code()).await.unwrap();
    let acceptance = RemotePairingAcceptance {
        offer_id: offer.offer_id,
        display_name: None,
        selected_runtime_ids: vec!["codex".to_string()],
    };

    assert_eq!(
        harness.core.pair(acceptance.clone()).await.unwrap(),
        RemotePairingOutcome::NeedsRepair {
            host_id: offer.host_id.clone(),
            reason: RemotePairingRepairReason::SecureStorageUnavailable,
        }
    );
    assert_eq!(
        harness
            .journal
            .entries
            .lock()
            .unwrap()
            .get(&offer.host_id)
            .expect("pending enrollment")
            .state,
        JournalHostState::EnrollmentRollbackPending(
            RemotePairingRepairReason::SecureStorageUnavailable
        )
    );

    harness.secrets.fail_write.store(false, Ordering::SeqCst);
    harness
        .host
        .rollback_unavailable
        .store(false, Ordering::SeqCst);
    assert_eq!(
        harness.core.pair(acceptance).await.unwrap(),
        RemotePairingOutcome::NeedsRepair {
            host_id: offer.host_id.clone(),
            reason: RemotePairingRepairReason::SecureStorageUnavailable,
        }
    );
    let fresh = harness.core.inspect(v2_code()).await.unwrap();
    assert_eq!(
        harness
            .core
            .pair(RemotePairingAcceptance {
                offer_id: fresh.offer_id,
                display_name: None,
                selected_runtime_ids: vec!["codex".to_string()],
            })
            .await
            .unwrap(),
        RemotePairingOutcome::Paired {
            host_id: fresh.host_id,
        }
    );
    let establish_keys = harness.host.establish_keys.lock().unwrap();
    assert_eq!(establish_keys.len(), 2);
    assert_ne!(establish_keys[0], establish_keys[1]);
}

#[tokio::test]
async fn revoke_resumes_an_unavailable_enrollment_rollback_without_minting_a_revoke_key() {
    let harness = harness();
    harness.secrets.fail_write.store(true, Ordering::SeqCst);
    harness
        .host
        .rollback_unavailable
        .store(true, Ordering::SeqCst);
    let offer = harness.core.inspect(v2_code()).await.unwrap();

    assert!(matches!(
        harness
            .core
            .pair(RemotePairingAcceptance {
                offer_id: offer.offer_id,
                display_name: None,
                selected_runtime_ids: vec!["codex".to_string()],
            })
            .await
            .unwrap(),
        RemotePairingOutcome::NeedsRepair { .. }
    ));
    assert!(matches!(
        harness.core.revoke(offer.host_id.clone()).await.unwrap(),
        RemoteRevokeOutcome::NeedsRepair {
            reason: RemotePairingRepairReason::SecureStorageUnavailable,
            ..
        }
    ));
    assert_eq!(harness.host.revoke_count.load(Ordering::SeqCst), 0);

    harness
        .host
        .rollback_unavailable
        .store(false, Ordering::SeqCst);
    assert_eq!(
        harness.core.revoke(offer.host_id.clone()).await.unwrap(),
        RemoteRevokeOutcome::AlreadyRevoked {
            host_id: offer.host_id,
        }
    );
    let establish_keys = harness.host.establish_keys.lock().unwrap();
    let rollback_keys = harness.host.rollback_keys.lock().unwrap();
    assert_eq!(rollback_keys.len(), 3);
    assert!(rollback_keys.iter().all(|key| key == &establish_keys[0]));
    assert!(harness.host.revoke_keys.lock().unwrap().is_empty());
}

#[tokio::test]
async fn reconnect_response_loss_replays_the_persisted_operation_key() {
    let harness = harness();
    let (offer, _) = inspect_and_pair(&harness).await;
    *harness.host.reconnect_error.lock().unwrap() = Some(HostPortError::Unavailable);

    assert_eq!(
        harness.core.reconnect(offer.host_id.clone()).await.unwrap(),
        RemoteReconnectOutcome::TemporarilyUnavailable {
            host_id: offer.host_id.clone(),
        }
    );
    let pending = harness
        .journal
        .entries
        .lock()
        .unwrap()
        .get(&offer.host_id)
        .cloned()
        .unwrap();
    assert_eq!(pending.state, JournalHostState::ReconnectPending);
    assert!(pending.operation_id.is_some());

    assert_eq!(
        harness.core.reconnect(offer.host_id.clone()).await.unwrap(),
        RemoteReconnectOutcome::Connected {
            host_id: offer.host_id.clone(),
        }
    );
    let keys = harness.host.reconnect_keys.lock().unwrap();
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0], keys[1]);
    let active = harness
        .journal
        .entries
        .lock()
        .unwrap()
        .get(&offer.host_id)
        .cloned()
        .unwrap();
    assert_eq!(active.state, JournalHostState::Active);
    assert!(active.operation_id.is_none());
    assert!(active.pending_credential_alias.is_none());
}

#[tokio::test]
async fn rotated_reconnect_keeps_old_credential_until_storage_and_confirmation_succeed() {
    let harness = harness();
    let (offer, _) = inspect_and_pair(&harness).await;
    harness.host.reconnect_rotates.store(true, Ordering::SeqCst);
    harness.secrets.fail_write.store(true, Ordering::SeqCst);

    assert_eq!(
        harness.core.reconnect(offer.host_id.clone()).await.unwrap(),
        RemoteReconnectOutcome::NeedsRepair {
            host_id: offer.host_id.clone(),
            reason: RemotePairingRepairReason::SecureStorageUnavailable,
        }
    );
    assert_eq!(harness.secrets.values.lock().unwrap().len(), 1);
    assert_eq!(
        harness
            .journal
            .entries
            .lock()
            .unwrap()
            .get(&offer.host_id)
            .unwrap()
            .state,
        JournalHostState::ReconnectPending
    );

    harness.secrets.fail_write.store(false, Ordering::SeqCst);
    assert!(matches!(
        harness.core.reconnect(offer.host_id.clone()).await.unwrap(),
        RemoteReconnectOutcome::Connected { .. }
    ));
    let reconnect_keys = harness.host.reconnect_keys.lock().unwrap();
    assert_eq!(reconnect_keys.len(), 2);
    assert_eq!(reconnect_keys[0], reconnect_keys[1]);
    drop(reconnect_keys);
    assert_eq!(
        harness.host.confirm_reconnect_count.load(Ordering::SeqCst),
        1
    );
    let secrets = harness.secrets.values.lock().unwrap();
    assert_eq!(secrets.len(), 1);
    assert_eq!(
        secrets.values().next().unwrap().expose_for_adapter(),
        b"rotated-device-grant"
    );
}

#[tokio::test]
async fn lost_rotation_confirmation_replays_confirmation_without_reconnecting() {
    let harness = harness();
    let (offer, _) = inspect_and_pair(&harness).await;
    harness.host.reconnect_rotates.store(true, Ordering::SeqCst);
    *harness.host.confirm_reconnect_error.lock().unwrap() = Some(HostPortError::Unavailable);

    assert_eq!(
        harness.core.reconnect(offer.host_id.clone()).await.unwrap(),
        RemoteReconnectOutcome::TemporarilyUnavailable {
            host_id: offer.host_id.clone(),
        }
    );
    assert_eq!(harness.secrets.values.lock().unwrap().len(), 2);
    assert!(matches!(
        harness
            .journal
            .entries
            .lock()
            .unwrap()
            .get(&offer.host_id)
            .unwrap()
            .state,
        JournalHostState::ReconnectConfirmPending { .. }
    ));

    assert!(matches!(
        harness.core.reconnect(offer.host_id).await.unwrap(),
        RemoteReconnectOutcome::Connected { .. }
    ));
    assert_eq!(harness.host.reconnect_count.load(Ordering::SeqCst), 1);
    assert_eq!(
        harness.host.confirm_reconnect_count.load(Ordering::SeqCst),
        2
    );
    let keys = harness.host.confirm_reconnect_keys.lock().unwrap();
    assert_eq!(keys[0], keys[1]);
    assert_eq!(harness.secrets.values.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn revoke_after_lost_rotation_confirmation_uses_the_staged_credential() {
    let harness = harness();
    let (offer, _) = inspect_and_pair(&harness).await;
    harness.host.reconnect_rotates.store(true, Ordering::SeqCst);
    *harness.host.confirm_reconnect_error.lock().unwrap() = Some(HostPortError::Unavailable);

    assert!(matches!(
        harness.core.reconnect(offer.host_id.clone()).await.unwrap(),
        RemoteReconnectOutcome::TemporarilyUnavailable { .. }
    ));
    assert!(matches!(
        harness.core.revoke(offer.host_id).await.unwrap(),
        RemoteRevokeOutcome::Revoked {
            host_credential_status: HostCredentialRevocationStatus::Confirmed,
            ..
        }
    ));
    assert_eq!(
        harness.host.revoke_credentials.lock().unwrap().as_slice(),
        [b"rotated-device-grant".to_vec()]
    );
    assert!(harness.secrets.values.lock().unwrap().is_empty());
}

#[tokio::test]
async fn identity_change_quarantines_credential_until_explicit_revoke() {
    let harness = harness();
    let (offer, _) = inspect_and_pair(&harness).await;
    *harness.host.reconnect_error.lock().unwrap() = Some(HostPortError::HostIdentityChanged);

    assert_eq!(
        harness.core.reconnect(offer.host_id.clone()).await.unwrap(),
        RemoteReconnectOutcome::RePairRequired {
            host_id: offer.host_id.clone(),
            reason: RemoteRePairReason::HostIdentityChanged,
        }
    );
    assert_eq!(harness.secrets.values.lock().unwrap().len(), 1);
    let fresh = harness.core.inspect(v2_code()).await.unwrap();
    assert_eq!(
        harness
            .core
            .pair(RemotePairingAcceptance {
                offer_id: fresh.offer_id,
                display_name: None,
                selected_runtime_ids: vec!["codex".to_string()],
            })
            .await
            .unwrap(),
        RemotePairingOutcome::NeedsRepair {
            host_id: offer.host_id.clone(),
            reason: RemotePairingRepairReason::HostCredentialNeedsRevocation,
        }
    );
    assert!(matches!(
        harness.core.revoke(offer.host_id.clone()).await.unwrap(),
        RemoteRevokeOutcome::Revoked {
            host_credential_status: HostCredentialRevocationStatus::Confirmed,
            ..
        }
    ));
    assert!(harness.secrets.values.lock().unwrap().is_empty());
}

#[tokio::test]
async fn rejected_reconnect_tombstones_then_allows_fresh_pairing() {
    let harness = harness();
    let (offer, _) = inspect_and_pair(&harness).await;
    *harness.host.reconnect_error.lock().unwrap() = Some(HostPortError::AuthenticationRejected);

    assert_eq!(
        harness.core.reconnect(offer.host_id.clone()).await.unwrap(),
        RemoteReconnectOutcome::RePairRequired {
            host_id: offer.host_id.clone(),
            reason: RemoteRePairReason::CredentialRejected,
        }
    );
    let tombstone = harness
        .journal
        .entries
        .lock()
        .unwrap()
        .get(&offer.host_id)
        .cloned()
        .unwrap();
    assert_eq!(
        tombstone.state,
        JournalHostState::RePairRequired(RemoteRePairReason::CredentialRejected)
    );
    assert!(tombstone.credential_alias.is_empty());
    assert!(harness.secrets.values.lock().unwrap().is_empty());

    let fresh = harness.core.inspect(v2_code()).await.unwrap();
    assert_eq!(
        harness
            .core
            .pair(RemotePairingAcceptance {
                offer_id: fresh.offer_id,
                display_name: None,
                selected_runtime_ids: vec!["codex".to_string()],
            })
            .await
            .unwrap(),
        RemotePairingOutcome::Paired {
            host_id: fresh.host_id,
        }
    );
    assert_eq!(harness.host.establish_count.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn confirmed_revoke_is_tombstone_first_idempotent_and_deletes_secret() {
    let harness = harness();
    let (offer, _) = inspect_and_pair(&harness).await;
    let revoked = harness.core.revoke(offer.host_id.clone()).await.unwrap();
    assert_eq!(
        revoked,
        RemoteRevokeOutcome::Revoked {
            host_id: offer.host_id.clone(),
            host_credential_status: HostCredentialRevocationStatus::Confirmed,
        }
    );
    assert!(harness.secrets.values.lock().unwrap().is_empty());
    assert_eq!(harness.host.close_count.load(Ordering::SeqCst), 1);
    assert_eq!(harness.host.revoke_count.load(Ordering::SeqCst), 1);

    assert_eq!(
        harness.core.revoke(offer.host_id.clone()).await.unwrap(),
        RemoteRevokeOutcome::AlreadyRevoked {
            host_id: offer.host_id.clone()
        }
    );
    assert_eq!(
        harness.core.reconnect(offer.host_id.clone()).await.unwrap(),
        RemoteReconnectOutcome::RePairRequired {
            host_id: offer.host_id,
            reason: RemoteRePairReason::Revoked,
        }
    );
}

#[tokio::test]
async fn confirmed_revoke_recovers_after_secret_delete_before_final_cas() {
    let harness = harness();
    let (offer, _) = inspect_and_pair(&harness).await;
    *harness.journal.fail_next_state.lock().unwrap() = Some(JournalHostState::Revoked);

    assert!(matches!(
        harness.core.revoke(offer.host_id.clone()).await,
        Err(RemoteHostPairingError::JournalUnavailable)
    ));
    assert!(harness.secrets.values.lock().unwrap().is_empty());
    assert_eq!(
        harness
            .journal
            .entries
            .lock()
            .unwrap()
            .get(&offer.host_id)
            .expect("settled revocation")
            .state,
        JournalHostState::RevocationSettled {
            host_credential_status: HostCredentialRevocationStatus::Confirmed,
        }
    );
    assert_eq!(harness.host.revoke_count.load(Ordering::SeqCst), 1);

    assert_eq!(
        harness.core.revoke(offer.host_id.clone()).await.unwrap(),
        RemoteRevokeOutcome::Revoked {
            host_id: offer.host_id,
            host_credential_status: HostCredentialRevocationStatus::Confirmed,
        }
    );
    assert_eq!(harness.host.revoke_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn deferred_revoke_keeps_only_retry_credential_and_blocks_reconnect() {
    let harness = harness();
    let (offer, _) = inspect_and_pair(&harness).await;
    harness.host.revoke_deferred.store(true, Ordering::SeqCst);
    assert_eq!(
        harness.core.revoke(offer.host_id.clone()).await.unwrap(),
        RemoteRevokeOutcome::Revoked {
            host_id: offer.host_id.clone(),
            host_credential_status: HostCredentialRevocationStatus::Deferred,
        }
    );
    assert_eq!(harness.secrets.values.lock().unwrap().len(), 1);
    assert_eq!(
        harness.core.reconnect(offer.host_id.clone()).await.unwrap(),
        RemoteReconnectOutcome::NeedsRepair {
            host_id: offer.host_id.clone(),
            reason: RemotePairingRepairReason::InterruptedRevocation,
        }
    );

    harness.host.revoke_deferred.store(false, Ordering::SeqCst);
    assert!(matches!(
        harness.core.revoke(offer.host_id).await.unwrap(),
        RemoteRevokeOutcome::Revoked {
            host_credential_status: HostCredentialRevocationStatus::Confirmed,
            ..
        }
    ));
    let revoke_keys = harness.host.revoke_keys.lock().unwrap();
    assert_eq!(revoke_keys.len(), 2);
    assert_eq!(revoke_keys[0], revoke_keys[1]);
    assert!(harness.secrets.values.lock().unwrap().is_empty());
}

#[tokio::test]
async fn already_revoked_host_credential_settles_without_retrying_forever() {
    let harness = harness();
    let (offer, _) = inspect_and_pair(&harness).await;
    *harness.host.revoke_error.lock().unwrap() = Some(HostPortError::CredentialRevoked);

    assert_eq!(
        harness.core.revoke(offer.host_id.clone()).await.unwrap(),
        RemoteRevokeOutcome::Revoked {
            host_id: offer.host_id,
            host_credential_status: HostCredentialRevocationStatus::Confirmed,
        }
    );
    assert_eq!(harness.host.revoke_count.load(Ordering::SeqCst), 1);
    assert!(harness.secrets.values.lock().unwrap().is_empty());
}

#[tokio::test]
async fn cancelled_revoke_preserves_credential_and_operation_for_exact_retry() {
    let harness = harness();
    let (offer, _) = inspect_and_pair(&harness).await;
    *harness.host.revoke_error.lock().unwrap() = Some(HostPortError::Cancelled);

    assert!(matches!(
        harness.core.revoke(offer.host_id.clone()).await,
        Err(RemoteHostPairingError::Cancelled)
    ));
    assert_eq!(harness.secrets.values.lock().unwrap().len(), 1);
    assert_eq!(
        harness
            .journal
            .entries
            .lock()
            .unwrap()
            .get(&offer.host_id)
            .unwrap()
            .state,
        JournalHostState::Revoking
    );

    assert!(matches!(
        harness.core.revoke(offer.host_id).await.unwrap(),
        RemoteRevokeOutcome::Revoked { .. }
    ));
    let keys = harness.host.revoke_keys.lock().unwrap();
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0], keys[1]);
}

#[tokio::test]
async fn forget_is_local_only_and_never_claims_remote_revocation() {
    let harness = harness();
    let (offer, _) = inspect_and_pair(&harness).await;
    assert_eq!(
        harness.core.forget(offer.host_id.clone()).await.unwrap(),
        RemoteForgetOutcome::ForgottenLocally {
            host_id: offer.host_id.clone(),
            host_revocation_still_required: true,
        }
    );
    assert_eq!(
        harness.core.reconnect(offer.host_id.clone()).await.unwrap(),
        RemoteReconnectOutcome::RePairRequired {
            host_id: offer.host_id.clone(),
            reason: RemoteRePairReason::MissingPairing,
        }
    );
    assert_eq!(harness.host.revoke_count.load(Ordering::SeqCst), 0);
    assert!(harness.secrets.values.lock().unwrap().is_empty());
    assert_eq!(
        harness.core.forget(offer.host_id.clone()).await.unwrap(),
        RemoteForgetOutcome::AlreadyForgotten {
            host_id: offer.host_id
        }
    );
}

#[tokio::test]
async fn interrupted_forget_keeps_a_retryable_local_tombstone() {
    let harness = harness();
    let (offer, _) = inspect_and_pair(&harness).await;
    harness.secrets.fail_delete.store(true, Ordering::SeqCst);

    assert_eq!(
        harness.core.forget(offer.host_id.clone()).await.unwrap(),
        RemoteForgetOutcome::NeedsRepair {
            host_id: offer.host_id.clone(),
            reason: RemotePairingRepairReason::SecureStorageUnavailable,
        }
    );
    let state = harness
        .journal
        .entries
        .lock()
        .unwrap()
        .get(&offer.host_id)
        .expect("forget tombstone")
        .state;
    assert_eq!(
        state,
        JournalHostState::Forgetting {
            host_revocation_still_required: true,
        }
    );
    assert_eq!(
        harness.core.reconnect(offer.host_id.clone()).await.unwrap(),
        RemoteReconnectOutcome::NeedsRepair {
            host_id: offer.host_id.clone(),
            reason: RemotePairingRepairReason::SecureStorageUnavailable,
        }
    );

    harness.secrets.fail_delete.store(false, Ordering::SeqCst);
    assert_eq!(
        harness.core.forget(offer.host_id.clone()).await.unwrap(),
        RemoteForgetOutcome::ForgottenLocally {
            host_id: offer.host_id.clone(),
            host_revocation_still_required: true,
        }
    );
    assert!(harness.secrets.values.lock().unwrap().is_empty());
    assert_eq!(
        harness
            .journal
            .entries
            .lock()
            .unwrap()
            .get(&offer.host_id)
            .expect("final tombstone")
            .state,
        JournalHostState::Forgotten
    );
}

#[tokio::test]
async fn missing_credential_becomes_typed_needs_repair() {
    let harness = harness();
    let (offer, _) = inspect_and_pair(&harness).await;
    harness.secrets.values.lock().unwrap().clear();
    assert_eq!(
        harness.core.reconnect(offer.host_id.clone()).await.unwrap(),
        RemoteReconnectOutcome::NeedsRepair {
            host_id: offer.host_id.clone(),
            reason: RemotePairingRepairReason::MissingHostCredential,
        }
    );
    let state = harness
        .journal
        .entries
        .lock()
        .unwrap()
        .get(&offer.host_id)
        .unwrap()
        .state;
    assert_eq!(
        state,
        JournalHostState::NeedsRepair(RemotePairingRepairReason::MissingHostCredential)
    );
}

#[tokio::test]
async fn missing_revoke_credential_is_an_interrupted_revocation() {
    let harness = harness();
    let (offer, _) = inspect_and_pair(&harness).await;
    harness.secrets.values.lock().unwrap().clear();

    assert_eq!(
        harness.core.revoke(offer.host_id.clone()).await.unwrap(),
        RemoteRevokeOutcome::NeedsRepair {
            host_id: offer.host_id.clone(),
            reason: RemotePairingRepairReason::InterruptedRevocation,
        }
    );
    assert_eq!(
        harness
            .journal
            .entries
            .lock()
            .unwrap()
            .get(&offer.host_id)
            .expect("revocation tombstone")
            .state,
        JournalHostState::NeedsRepair(RemotePairingRepairReason::InterruptedRevocation)
    );
}

#[tokio::test]
async fn revocation_invalidates_an_offer_waiting_for_acceptance() {
    let harness = harness();
    let (first, _) = inspect_and_pair(&harness).await;
    let second = harness.core.inspect(v2_code()).await.unwrap();
    harness.core.revoke(first.host_id).await.unwrap();

    assert!(matches!(
        harness
            .core
            .pair(RemotePairingAcceptance {
                offer_id: second.offer_id,
                display_name: None,
                selected_runtime_ids: vec!["codex".to_string()],
            })
            .await,
        Err(RemoteHostPairingError::UnknownOffer)
    ));
}

#[tokio::test]
async fn queued_pair_does_not_resurrect_an_offer_invalidated_by_revoke() {
    let harness = harness();
    let (paired, _) = inspect_and_pair(&harness).await;
    let offer = harness.core.inspect(v2_code()).await.unwrap();

    let operation_lock = harness.core.host_lock(&paired.host_id).await;
    let operation = operation_lock.lock().await;
    let revoke_core = Arc::clone(&harness.core);
    let revoke_host_id = paired.host_id.clone();
    let revoke = tokio::spawn(async move { revoke_core.revoke(revoke_host_id).await });
    tokio::task::yield_now().await;

    let pair_core = Arc::clone(&harness.core);
    let pair = tokio::spawn(async move {
        pair_core
            .pair(RemotePairingAcceptance {
                offer_id: offer.offer_id,
                display_name: None,
                selected_runtime_ids: vec!["codex".to_string()],
            })
            .await
    });
    tokio::task::yield_now().await;
    drop(operation);

    assert!(revoke.await.unwrap().is_ok());
    assert!(matches!(
        pair.await.unwrap(),
        Err(RemoteHostPairingError::UnknownOffer)
    ));
    assert_eq!(harness.host.establish_count.load(Ordering::SeqCst), 1);
}

#[test]
fn credential_debug_is_redacted() {
    let credential = OpaqueCredential::new(b"sentinel-private-grant".to_vec());
    let debug = format!("{credential:?}");
    assert_eq!(debug, "OpaqueCredential(<redacted>)");
    assert!(!debug.contains("sentinel-private-grant"));
}
