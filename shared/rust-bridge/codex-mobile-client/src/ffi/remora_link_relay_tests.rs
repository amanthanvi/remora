use super::*;
use crate::ffi::background_relay::BackgroundRelayError;
use crate::ffi::background_relay::tests::{MemoryJournalBackend, MemorySecretBackend};
use crate::remote_host_pairing::remora_link_v2::{
    CredentialJournalV2, EnrollmentCandidateJournalV2, EnrollmentJournalV2,
    EnrollmentTranscriptInput, HostBindingJournalV2, InvitationJournalV2,
    enrollment_transcript_hash,
};
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::Notify;

#[derive(Default)]
struct PairingBackend {
    value: std::sync::Mutex<Option<AppRemoraLinkJournalSnapshot>>,
    pause_once: AtomicBool,
    captured: Notify,
    resume: Notify,
    loads: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl AppRemoraLinkJournalBackend for Arc<PairingBackend> {
    async fn load(&self) -> AppRemoraLinkJournalLoad {
        self.loads.fetch_add(1, Ordering::SeqCst);
        let captured = self.value.lock().unwrap().clone();
        if self.pause_once.swap(false, Ordering::SeqCst) {
            self.captured.notify_one();
            self.resume.notified().await;
        }
        captured.map_or(AppRemoraLinkJournalLoad::Missing, |snapshot| {
            AppRemoraLinkJournalLoad::Loaded { snapshot }
        })
    }

    async fn compare_and_swap(
        &self,
        expected_revision: Option<u64>,
        replacement: AppRemoraLinkJournalSnapshot,
    ) -> AppRemoraLinkJournalWriteOutcome {
        let mut value = self.value.lock().unwrap();
        if value.as_ref().map(|value| value.revision) != expected_revision {
            return AppRemoraLinkJournalWriteOutcome::Conflict;
        }
        *value = Some(replacement);
        AppRemoraLinkJournalWriteOutcome::Stored
    }
}

struct Identity;
#[async_trait]
impl AppRemoraLinkTransportIdentityBackend for Identity {
    async fn load_or_create(
        &self,
        candidate: AppRelaySecretValue,
    ) -> Result<AppRelaySecretValue, AppRemoraLinkTransportIdentityError> {
        Ok(candidate)
    }
}

struct UnusedKeys;
#[async_trait]
impl AppRemoraLinkDeviceKeyBackend for UnusedKeys {
    async fn ensure_hardware_key(
        &self,
        _host_id: String,
    ) -> Result<AppRemoraLinkHardwareKey, AppRemoraLinkDeviceKeyError> {
        Err(AppRemoraLinkDeviceKeyError::Unavailable)
    }
    async fn load_hardware_key(
        &self,
        _slot: String,
    ) -> Result<AppRemoraLinkHardwareKeyLoad, AppRemoraLinkDeviceKeyError> {
        Ok(AppRemoraLinkHardwareKeyLoad::Missing)
    }
    async fn sign_message(
        &self,
        _slot: String,
        _message: AppRelaySecretValue,
    ) -> Result<AppRelaySecretValue, AppRemoraLinkDeviceKeyError> {
        Err(AppRemoraLinkDeviceKeyError::Unavailable)
    }
    async fn delete_hardware_key(
        &self,
        _slot: String,
    ) -> Result<AppRemoraLinkKeyDeletionStatus, AppRemoraLinkDeviceKeyError> {
        Ok(AppRemoraLinkKeyDeletionStatus::Deleted)
    }
}

struct Fixture {
    app: Arc<AppClient>,
    pairing: Arc<PairingBackend>,
    paired: Arc<ConfiguredRemoraLink>,
    relay: Arc<ConfiguredBackgroundRelay>,
    origin: ValidatedRelayOrigin,
    http: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn new() -> Self {
        let app = Arc::new(AppClient {
            inner: crate::MobileClient::new(),
            rt: crate::ffi::shared::shared_runtime(),
        });
        let pairing = Arc::new(PairingBackend::default());
        let paired = build_configuration(
            Box::new(pairing.clone()),
            Box::new(Identity),
            Box::new(UnusedKeys),
        )
        .await
        .unwrap();
        *remora_link_write(&app.inner.remora_link) = Some(paired.clone());
        app.configure_background_relay(
            Box::new(MemoryJournalBackend::default()),
            Box::new(MemorySecretBackend::default()),
            true,
        )
        .await
        .unwrap();
        let relay = app.inner.background_relay.read().unwrap().clone().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = ValidatedRelayOrigin::parse(
            &format!("http://{}", listener.local_addr().unwrap()),
            true,
        )
        .unwrap();
        let http = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = vec![0; 8192];
                let length = stream.read(&mut request).await.unwrap();
                let request = std::str::from_utf8(&request[..length]).unwrap();
                let response = if request.starts_with("POST ") {
                    let body = r#"{"schema_version":1,"installation_id":"installation_identifier_0001","registration_id":"registration_identifier_0001","provider":"fcm","environment":"production","generation":1,"replaced":false}"#;
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\nContent-Type: application/json\r\n\r\n{body}",
                        body.len()
                    )
                } else {
                    assert!(request.starts_with("DELETE "), "{request}");
                    "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".into()
                };
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        Self {
            app,
            pairing,
            paired,
            relay,
            origin,
            http,
        }
    }

    async fn close(self) {
        for binding in self.relay.journal.list().await.unwrap() {
            self.relay
                .relay
                .rollback_enrollment(&binding.host_id, &binding.staging_command_id, operation())
                .await
                .unwrap();
        }
        self.app.clear_remora_link().await;
        self.app.clear_background_relay().await.unwrap();
        self.http.abort();
        assert!(self.http.await.unwrap_err().is_cancelled());
    }

    async fn publish_pairing(&self) {
        let entry = pairing_entry();
        self.paired
            .journal
            .compare_and_swap(&entry.binding.host_id, None, entry.clone())
            .await
            .unwrap();
    }

    async fn stage(&self) {
        let enrollment = enrollment(self.origin.clone());
        self.relay
            .relay
            .stage_enrollment(enrollment.clone(), operation())
            .await
            .unwrap();
        self.relay
            .relay
            .commit_enrollment(&enrollment.host_id, &enrollment.command_id, operation())
            .await
            .unwrap();
    }

    async fn assert_active(&self) {
        let binding = self
            .relay
            .journal
            .load_by_host(&host())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(binding.state, RelayBindingState::Active);
        assert_eq!(
            binding.staging_command_id,
            enrollment(self.origin.clone()).command_id
        );
    }
}

fn operation() -> RelayOperationContext {
    RelayOperationContext::with_timeout(Duration::from_secs(5))
}

fn host() -> RelayHostId {
    RelayHostId(format!(
        "remora-link:{}",
        SecretKey::from_bytes(&[11; 32]).public()
    ))
}

fn enrollment(origin: ValidatedRelayOrigin) -> RelayEnrollment {
    RelayEnrollment {
        host_id: host(),
        origin,
        installation_id: RelayInstallationId::parse("installation_identifier_0001").unwrap(),
        command_id: RelayEnrollmentCommandId::parse(PairingLifecycleV2::relay_command_id(
            &host().0,
            &pairing_entry().credential.unwrap().credential_id,
        ))
        .unwrap(),
        read_capability: OpaqueRelaySecret::new(vec![b'r'; 32]).unwrap(),
        manage_capability: OpaqueRelaySecret::new(vec![b'm'; 32]).unwrap(),
    }
}

fn pairing_entry() -> PairingJournalEntryV2 {
    let key = p256::ecdsa::SigningKey::from_bytes((&[7; 32]).into()).unwrap();
    let public_key = key.verifying_key().to_encoded_point(false);
    let device_public_key = URL_SAFE_NO_PAD.encode(public_key.as_bytes());
    let client_endpoint_id = SecretKey::from_bytes(&[12; 32]).public().to_string();
    let runtime_ids = vec!["codex".to_owned()];
    let scopes = vec![
        DeviceScopeV2::InspectRuntimes,
        DeviceScopeV2::ConnectRuntime,
        DeviceScopeV2::SelfRevoke,
    ];
    let credential_id = URL_SAFE_NO_PAD.encode([7; 16]);
    let mut candidate = EnrollmentCandidateJournalV2 {
        host_endpoint_id: SecretKey::from_bytes(&[11; 32]).public().to_string(),
        client_endpoint_id: client_endpoint_id.clone(),
        credential_id: credential_id.clone(),
        invitation_id: URL_SAFE_NO_PAD.encode([8; 16]),
        device_public_key: device_public_key.clone(),
        enrollment_idempotency_key: "pairing-command-0000000000000001".into(),
        selected_runtime_ids: runtime_ids.clone(),
        requested_scopes: scopes.clone(),
        server_nonce: URL_SAFE_NO_PAD.encode([9; 32]),
        client_nonce: URL_SAFE_NO_PAD.encode([10; 32]),
        confirmation_mode: ConfirmationModeV2::Interactive,
        max_runtime_ids: runtime_ids.clone(),
        max_scopes: scopes.clone(),
        transcript_hash: String::new(),
    };
    candidate.transcript_hash = URL_SAFE_NO_PAD.encode(
        enrollment_transcript_hash(EnrollmentTranscriptInput {
            host_endpoint_id: &candidate.host_endpoint_id,
            client_endpoint_id: &candidate.client_endpoint_id,
            invitation_id: &candidate.invitation_id,
            device_public_key: &candidate.device_public_key,
            idempotency_key: &candidate.enrollment_idempotency_key,
            selected_runtime_ids: &candidate.selected_runtime_ids,
            requested_scopes: &candidate.requested_scopes,
            server_nonce: &candidate.server_nonce,
            client_nonce: &candidate.client_nonce,
            confirmation_mode: candidate.confirmation_mode,
            max_runtime_ids: &candidate.max_runtime_ids,
            max_scopes: &candidate.max_scopes,
        })
        .unwrap(),
    );
    let entry = PairingJournalEntryV2 {
        schema_version: JOURNAL_SCHEMA_VERSION,
        revision: 1,
        binding: HostBindingJournalV2 {
            host_id: host().0,
            node_id: candidate.host_endpoint_id.clone(),
            host_display_name: None,
            relay_hint: None,
            hardware_key_slot: "remora-link:key-a".into(),
            device_public_key,
            client_endpoint_id: Some(client_endpoint_id.clone()),
        },
        invitation: Some(InvitationJournalV2 {
            invitation_id: candidate.invitation_id.clone(),
            expires_at: i64::MAX,
            max_runtime_ids: runtime_ids.clone(),
            max_scopes: scopes.clone(),
            confirmation_mode: ConfirmationModeV2::Interactive,
            runtime_offers: Vec::new(),
        }),
        enrollment: Some(EnrollmentJournalV2 {
            display_name: "Fixture".into(),
            selected_runtime_ids: runtime_ids.clone(),
            requested_scopes: scopes.clone(),
            idempotency_key: candidate.enrollment_idempotency_key.clone(),
            prospective_credential_id: Some(credential_id.clone()),
            candidates: vec![candidate.clone()],
            pending_claim: None,
        }),
        credential: Some(CredentialJournalV2 {
            credential_id,
            selected_runtime_ids: runtime_ids,
            granted_scopes: scopes,
            auth_epoch: 0,
            created_at: 1,
            endpoint_fingerprint: hex::encode(&Sha256::digest(client_endpoint_id.as_bytes())[..8]),
            device_key_fingerprint: hex::encode(&Sha256::digest(public_key.as_bytes())[..8]),
            transcript_hash: candidate.transcript_hash,
            sas: "123-456".into(),
        }),
        mutation: None,
        restart_high_watermark: 0,
        pending_restart: None,
        phase: JournalPhaseV2::Enrolled,
    };
    entry
        .validate()
        .expect("fixture has a valid enrollment transcript");
    entry
}

#[tokio::test]
async fn synchronization_rechecks_pairing_after_stale_empty_snapshot() {
    let fixture = Fixture::new().await;
    fixture.stage().await;
    fixture
        .relay
        .relay
        .observe_push_token(
            PushTokenObservation {
                provider: RelayPushProvider::Fcm,
                environment: RelayPushEnvironment::Production,
                token: OpaqueRelaySecret::new(vec![b't'; 32]).unwrap(),
                local_generation: 1,
                observed_at_ms: 1,
            },
            operation(),
        )
        .await
        .unwrap();
    fixture.pairing.pause_once.store(true, Ordering::SeqCst);
    let app = fixture.app.clone();
    let sync = tokio::spawn(async move { app.inner.synchronize_paired_relays().await });
    fixture.pairing.captured.notified().await;
    fixture.publish_pairing().await;
    fixture.pairing.resume.notify_one();
    sync.await.unwrap();
    fixture.assert_active().await;
    let binding = fixture
        .relay
        .journal
        .load_by_host(&host())
        .await
        .unwrap()
        .unwrap();
    assert!(binding.registrations[0].relay_registration_id.is_some());
    fixture.close().await;
}

#[tokio::test]
async fn overlapping_synchronization_waits_before_reading_pairing_authority() {
    let fixture = Fixture::new().await;
    fixture.pairing.pause_once.store(true, Ordering::SeqCst);
    let mut first = Box::pin(fixture.app.inner.synchronize_paired_relays());
    assert!(matches!(
        futures::poll!(&mut first),
        std::task::Poll::Pending
    ));
    fixture.pairing.captured.notified().await;
    let first_loads = fixture.pairing.loads.load(Ordering::SeqCst);
    let mut second = Box::pin(fixture.app.inner.synchronize_paired_relays());
    assert!(matches!(
        futures::poll!(&mut second),
        std::task::Poll::Pending
    ));
    assert_eq!(fixture.pairing.loads.load(Ordering::SeqCst), first_loads);
    fixture.pairing.resume.notify_one();
    first.await;
    tokio::time::timeout(Duration::from_secs(1), second)
        .await
        .unwrap();
    assert!(fixture.pairing.loads.load(Ordering::SeqCst) > first_loads);
    fixture.close().await;
}

#[tokio::test]
async fn delayed_retirement_cannot_tombstone_a_new_pairing_binding() {
    let fixture = Fixture::new().await;
    let provisioning = fixture.relay.provisioning.lock().await;
    let host_id = host();
    let mut retirement = Box::pin(fixture.app.inner.retire_paired_relay(&host_id.0));
    assert!(matches!(
        futures::poll!(&mut retirement),
        std::task::Poll::Pending
    ));
    fixture.publish_pairing().await;
    fixture.stage().await;
    drop(provisioning);
    retirement.await;
    fixture.assert_active().await;
    fixture.close().await;
}

#[tokio::test]
async fn custody_transfer_lease_delays_then_rejects_replacement_after_local_commit() {
    let fixture = Fixture::new().await;
    fixture.publish_pairing().await;
    let lease = fixture
        .app
        .inner
        .paired_relay_configuration()
        .await
        .unwrap();
    let provisioning = lease.relay.provisioning.lock().await;
    let mut replacement = Box::pin(fixture.app.configure_background_relay(
        Box::new(MemoryJournalBackend::default()),
        Box::new(MemorySecretBackend::default()),
        true,
    ));
    assert!(matches!(
        futures::poll!(&mut replacement),
        std::task::Poll::Pending
    ));
    assert!(Arc::ptr_eq(
        fixture
            .app
            .inner
            .background_relay
            .read()
            .unwrap()
            .as_ref()
            .unwrap(),
        &fixture.relay,
    ));
    fixture.stage().await;
    fixture.assert_active().await;
    drop(provisioning);
    drop(lease);
    assert!(matches!(
        replacement.await,
        Err(BackgroundRelayError::CustodyInUse)
    ));
    assert!(Arc::ptr_eq(
        fixture
            .app
            .inner
            .background_relay
            .read()
            .unwrap()
            .as_ref()
            .unwrap(),
        &fixture.relay,
    ));
    fixture.close().await;
}

#[tokio::test]
async fn cancelled_transfer_releases_configuration_without_staging_old_custody() {
    let fixture = Fixture::new().await;
    let captured = Arc::new(Notify::new());
    let paused = captured.clone();
    let app = fixture.app.clone();
    let transfer = tokio::spawn(async move {
        let _lease = app.inner.paired_relay_configuration().await.unwrap();
        paused.notify_one();
        std::future::pending::<()>().await;
    });
    captured.notified().await;
    let mut replacement = Box::pin(fixture.app.clear_background_relay());
    assert!(matches!(
        futures::poll!(&mut replacement),
        std::task::Poll::Pending
    ));
    transfer.abort();
    assert!(transfer.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(1), replacement)
        .await
        .unwrap()
        .unwrap();
    assert!(fixture.relay.journal.list().await.unwrap().is_empty());
    fixture.close().await;
}

#[tokio::test]
async fn cancelled_host_commit_cannot_release_durable_custody_to_replacement() {
    let fixture = Fixture::new().await;
    fixture.publish_pairing().await;
    let captured = Arc::new(Notify::new());
    let paused = captured.clone();
    let app = fixture.app.clone();
    let bundle = enrollment(fixture.origin.clone());
    let transfer = tokio::spawn(async move {
        let lease = app.inner.paired_relay_configuration().await.unwrap();
        let _provisioning = lease.relay.provisioning.lock().await;
        lease
            .relay
            .relay
            .stage_enrollment(bundle.clone(), operation())
            .await
            .unwrap();
        lease
            .relay
            .relay
            .commit_enrollment(&bundle.host_id, &bundle.command_id, operation())
            .await
            .unwrap();
        paused.notify_one();
        // Model the cancellable interval while the host commit is in flight.
        std::future::pending::<()>().await;
    });
    captured.notified().await;
    let mut clear = Box::pin(fixture.app.clear_background_relay());
    assert!(matches!(
        futures::poll!(&mut clear),
        std::task::Poll::Pending
    ));
    transfer.abort();
    assert!(transfer.await.unwrap_err().is_cancelled());
    assert!(matches!(
        clear.await,
        Err(BackgroundRelayError::CustodyInUse)
    ));
    assert!(matches!(
        fixture
            .app
            .configure_background_relay(
                Box::new(MemoryJournalBackend::default()),
                Box::new(MemorySecretBackend::default()),
                true,
            )
            .await,
        Err(BackgroundRelayError::CustodyInUse)
    ));
    assert!(Arc::ptr_eq(
        fixture
            .app
            .inner
            .background_relay
            .read()
            .unwrap()
            .as_ref()
            .unwrap(),
        &fixture.relay,
    ));
    fixture.assert_active().await;
    fixture.close().await;
}

#[tokio::test]
async fn repair_does_not_wait_behind_writer_while_provisioning_holds_config_lease() {
    let fixture = Fixture::new().await;
    fixture.stage().await;
    let lease = fixture
        .app
        .inner
        .paired_relay_configuration()
        .await
        .unwrap();
    let mut writer = Box::pin(fixture.app.inner.background_relay_configuration.write());
    assert!(matches!(
        futures::poll!(&mut writer),
        std::task::Poll::Pending
    ));
    let binding = fixture
        .relay
        .journal
        .load_by_host(&host())
        .await
        .unwrap()
        .unwrap();
    let repair = PairedHostRelayRepair::new(
        Arc::downgrade(&fixture.app.inner),
        fixture.relay.journal.clone(),
    );
    let result = tokio::time::timeout(
        Duration::from_millis(100),
        repair.repair(
            &host(),
            binding.repair_generation,
            RelayRepairMode::Full { through_cursor: 0 },
            operation(),
        ),
    )
    .await
    .expect("repair must release its operation instead of waiting for a writer");
    assert_eq!(result, Err(RelayRepairError::Cancelled));
    drop(lease);
    drop(writer.await);
    fixture.close().await;
}
