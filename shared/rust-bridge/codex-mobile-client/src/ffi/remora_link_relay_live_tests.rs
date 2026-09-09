//! Opt-in real Iroh host + relay fixture. Software key custody is test-only.

use super::*;
use crate::background_relay::RelayBindingState;
use crate::ffi::remora_link_v2::*;
use futures::FutureExt as _;
use p256::ecdsa::{SigningKey, signature::Signer};
use std::sync::Mutex;

#[derive(Clone, Default)]
struct PairingJournal(Arc<Mutex<Option<AppRemoraLinkJournalSnapshot>>>);

#[async_trait]
impl AppRemoraLinkJournalBackend for PairingJournal {
    async fn load(&self) -> AppRemoraLinkJournalLoad {
        self.0
            .lock()
            .unwrap()
            .clone()
            .map_or(AppRemoraLinkJournalLoad::Missing, |snapshot| {
                AppRemoraLinkJournalLoad::Loaded { snapshot }
            })
    }

    async fn compare_and_swap(
        &self,
        expected_revision: Option<u64>,
        replacement: AppRemoraLinkJournalSnapshot,
    ) -> AppRemoraLinkJournalWriteOutcome {
        let mut value = self.0.lock().unwrap();
        if value.as_ref().map(|value| value.revision) != expected_revision {
            return AppRemoraLinkJournalWriteOutcome::Conflict;
        }
        *value = Some(replacement);
        AppRemoraLinkJournalWriteOutcome::Stored
    }
}

struct TransportIdentity;

#[async_trait]
impl AppRemoraLinkTransportIdentityBackend for TransportIdentity {
    async fn load_or_create(
        &self,
        candidate: AppRelaySecretValue,
    ) -> Result<AppRelaySecretValue, AppRemoraLinkTransportIdentityError> {
        Ok(candidate)
    }
}

#[derive(Clone)]
struct DeviceKey(Arc<Mutex<Option<SigningKey>>>);

impl DeviceKey {
    fn record(&self) -> Result<AppRemoraLinkHardwareKey, AppRemoraLinkDeviceKeyError> {
        let key = self.0.lock().unwrap();
        let key = key.as_ref().ok_or(AppRemoraLinkDeviceKeyError::Missing)?;
        Ok(AppRemoraLinkHardwareKey {
            slot: "relay-live-test-key".into(),
            public_key_sec1: key
                .verifying_key()
                .to_encoded_point(false)
                .as_bytes()
                .to_vec(),
            assurance: AppRemoraLinkKeyAssurance::SoftwareDebugOnly,
        })
    }
}

#[async_trait]
impl AppRemoraLinkDeviceKeyBackend for DeviceKey {
    async fn ensure_hardware_key(
        &self,
        _host_id: String,
    ) -> Result<AppRemoraLinkHardwareKey, AppRemoraLinkDeviceKeyError> {
        self.record()
    }

    async fn load_hardware_key(
        &self,
        _slot: String,
    ) -> Result<AppRemoraLinkHardwareKeyLoad, AppRemoraLinkDeviceKeyError> {
        Ok(match self.record() {
            Ok(key) => AppRemoraLinkHardwareKeyLoad::Loaded { key },
            Err(AppRemoraLinkDeviceKeyError::Missing) => AppRemoraLinkHardwareKeyLoad::Missing,
            Err(error) => return Err(error),
        })
    }

    async fn sign_message(
        &self,
        _slot: String,
        message: AppRelaySecretValue,
    ) -> Result<AppRelaySecretValue, AppRemoraLinkDeviceKeyError> {
        let message = Zeroizing::new(message.into_bytes());
        let key = self.0.lock().unwrap();
        let key = key.as_ref().ok_or(AppRemoraLinkDeviceKeyError::Missing)?;
        let signature: p256::ecdsa::Signature = key.sign(&message);
        Ok(signature.to_der().as_bytes().to_vec().into())
    }

    async fn delete_hardware_key(
        &self,
        _slot: String,
    ) -> Result<AppRemoraLinkKeyDeletionStatus, AppRemoraLinkDeviceKeyError> {
        self.0.lock().unwrap().take();
        Ok(AppRemoraLinkKeyDeletionStatus::Deleted)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires disposable real host/relay; use tools/scripts/verify-paired-relay.py"]
async fn real_paired_host_relay_repairs_detached_turn_before_ack() {
    let code_path = std::env::var_os("REMORA_RELAY_LIVE_CODE_FILE")
        .expect("REMORA_RELAY_LIVE_CODE_FILE must name the disposable invitation file");
    let code = Zeroizing::new(std::fs::read(code_path).expect("read fixture invitation"));
    let app = AppClient {
        inner: crate::MobileClient::new(),
        rt: crate::ffi::shared::shared_runtime(),
    };
    let result = std::panic::AssertUnwindSafe(async {
    app.configure_remora_link(
        Box::new(PairingJournal::default()),
        Box::new(TransportIdentity),
        Box::new(DeviceKey(Arc::new(Mutex::new(Some(SigningKey::random(
            &mut OsRng,
        )))))),
    )
    .await
    .unwrap();
    let AppRemoraLinkInspection::Ready { offer } = app
        .inspect_remora_link_code(code.to_vec().into())
        .await
        .unwrap();
    let host_id = offer.host_id.clone();
    let paired = app
        .accept_remora_link_offer(AppRemoraLinkAcceptance {
            offer_id: offer.offer_id,
            device_display_name: "Disposable relay integration".into(),
            selected_runtime_ids: vec!["codex".into()],
            requested_scopes: vec![
                AppRemoraLinkScope::InspectRuntimes,
                AppRemoraLinkScope::ConnectRuntime,
                AppRemoraLinkScope::SelfRevoke,
            ],
        })
        .await
        .unwrap();
    assert!(matches!(paired, AppRemoraLinkPairingOutcome::Paired { .. }));
    app.configure_background_relay(
        Box::new(super::tests::MemoryJournalBackend::default()),
        Box::new(super::tests::MemorySecretBackend::default()),
        true,
    )
    .await
    .unwrap();
    let registered = app
        .background_relay_observe_push_token(
            AppRelayPushTokenObservation {
                provider: AppRelayPushProvider::Fcm,
                environment: AppRelayPushEnvironment::Production,
                local_generation: 1,
            },
            vec![b't'; 64].into(),
        )
        .await
        .unwrap();
    assert_eq!(
        registered.synchronized, 1,
        "real relay provider registration"
    );
    let response = app.inner.request_raw_for_server(&host_id, serde_json::from_value(serde_json::json!({
        "id": crate::next_request_id(), "method": "thread/start", "params": {"model": "fixture-model", "cwd": std::env::var("REMORA_RELAY_LIVE_CWD").unwrap(), "approvalPolicy": "never", "sandbox": "danger-full-access"}
    })).unwrap()).await.unwrap();
    let thread_id = response["thread"]["id"].as_str().unwrap().to_owned();
    app.inner
        .external_resume_thread(&host_id, &thread_id, None)
        .await
        .unwrap();
    let params = serde_json::from_value(serde_json::json!({
        "threadId": thread_id, "input": [{"type": "text", "text": "Return REMOTE_RESUME_COMPLETE", "text_elements": []}]
    })).unwrap();
    app.inner.start_turn(&host_id, params).await.unwrap();
    app.inner.disconnect_all_remora_link_sessions().await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    let configured = app.configured_background_relay().unwrap();
    tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            let results = app.background_relay_reconcile().await.unwrap();
            let binding = configured.journal.load_by_host(&RelayHostId(host_id.clone())).await.unwrap().unwrap();
            let key = crate::types::ThreadKey { server_id: host_id.clone(), thread_id: thread_id.clone() };
            let restored = app.inner.app_store.thread_snapshot(&key).is_some_and(|thread| {
                thread.active_turn_id.is_none() && thread.items.iter().any(|item| matches!(
                    &item.content, crate::conversation_uniffi::HydratedConversationItemContent::Assistant(message)
                        if message.text.contains("REMOTE_RESUME_COMPLETE")
                ))
            });
            if restored && binding.wake.applied_cursor > 0 && binding.wake.pending_ack_cursor.is_none() {
                assert!(binding.wake.verified_barrier_id.is_some());
                assert_eq!(binding.state, RelayBindingState::Active);
                assert!(results.iter().any(|result| matches!(result, AppRelayReconcileOutcome::Applied { .. })));
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }).await.expect("authenticated detached-turn repair and ACK");
    app.forget_remora_link_host(host_id.clone()).await.unwrap();
    let retired = configured
        .journal
        .load_by_host(&RelayHostId(host_id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(retired.state, RelayBindingState::Tombstoned);
    }).catch_unwind().await;
    app.clear_remora_link().await;
    let _ = app.clear_background_relay().await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}
