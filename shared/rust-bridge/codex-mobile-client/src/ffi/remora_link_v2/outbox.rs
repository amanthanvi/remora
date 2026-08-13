//! Device-owned offline message delivery through durable Host work fences.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::warn;

use super::{
    REMORA_LINK_RUNTIME_URL, ThreadBindingOutcomeV2, WorkIntentOutcomeV2, remora_link_read,
};
use crate::device_database::{OutboxIntent, OutboxIntentKind, OutboxState};
use crate::ffi::ClientError;
use crate::remote_host_pairing::remora_link_v2::ThreadBindingReceiptV2;
use crate::types::{AppStartTurnRequest, AppUserInput, ThreadKey};
use crate::{OutboxDispatchAuthorization, OutboxTurnStartOutcome};

const MAX_OUTBOX_DELIVERY_BATCH: usize = 100;
const AUTOMATIC_OUTBOX_DELIVERY_BATCH: usize = 10;
const OUTBOX_RETRY_BASE_MS: i64 = 1_000;
const OUTBOX_RETRY_MAX_MS: i64 = 5 * 60 * 1_000;

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AppRemoraLinkOfflineMessageEnqueueOutcome {
    Queued { intent_id: String },
    Unavailable { reason: String },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, uniffi::Record)]
pub struct AppRemoraLinkOutboxDeliveryReport {
    pub considered: u32,
    pub delivered: u32,
    pub deferred: u32,
    pub outcome_unknown: u32,
    pub failed: u32,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OfflineTurnPayloadV1 {
    v: u8,
    request: AppStartTurnRequest,
}

impl crate::MobileClient {
    fn cached_remora_link_host_thread_id(&self, key: &ThreadKey) -> Option<String> {
        remora_link_read(&self.remora_link)
            .as_ref()
            .and_then(|configured| configured.cached_host_thread_id(key))
    }

    fn has_remora_link_runtime_session(&self, host_id: &str, runtime_id: Option<&str>) -> bool {
        self.sessions_read().get(host_id).is_some_and(|session| {
            session.config().websocket_url.as_deref() == Some(REMORA_LINK_RUNTIME_URL)
                && runtime_id.is_none_or(|runtime_id| {
                    session
                        .available_runtime_kinds()
                        .iter()
                        .any(|available| available == runtime_id)
                })
        })
    }

    async fn remora_link_host_thread_id(&self, key: &ThreadKey) -> Option<String> {
        if let Some(thread_id) = self.cached_remora_link_host_thread_id(key) {
            return Some(thread_id);
        }
        let runtime_id = self.runtime_for_thread(key);
        if !self.has_remora_link_runtime_session(&key.server_id, Some(&runtime_id)) {
            return None;
        }
        let configured = remora_link_read(&self.remora_link).clone()?;
        match configured
            .lifecycle
            .bind_provider_thread(&key.server_id, &runtime_id, &key.thread_id)
            .await
        {
            Ok(outcome) => {
                configured.cache_thread_binding(&key.server_id, &outcome);
                configured.cached_host_thread_id(key)
            }
            Err(error) => {
                warn!(
                    host_id = key.server_id,
                    %error,
                    "durable Remora Link Thread binding unavailable for offline queue"
                );
                None
            }
        }
    }

    pub(crate) async fn enqueue_remora_link_offline_message(
        self: &Arc<Self>,
        key: ThreadKey,
        request: AppStartTurnRequest,
    ) -> Result<AppRemoraLinkOfflineMessageEnqueueOutcome, ClientError> {
        let _configuration = self.remora_link_configuration.read().await;
        validate_offline_turn_request(&key, &request)?;
        let Some(database) = self.configured_device_database() else {
            return Ok(AppRemoraLinkOfflineMessageEnqueueOutcome::Unavailable {
                reason: "Encrypted device storage is not ready".to_string(),
            });
        };
        let Some(host_thread_id) = self.remora_link_host_thread_id(&key).await else {
            return Ok(AppRemoraLinkOfflineMessageEnqueueOutcome::Unavailable {
                reason: "This Thread is not durably bound to its Host".to_string(),
            });
        };
        let payload = serde_json::to_vec(&OfflineTurnPayloadV1 { v: 1, request })
            .map_err(|error| ClientError::Serialization(error.to_string()))?;
        for _ in 0..3 {
            let mut random = [0_u8; 16];
            OsRng.fill_bytes(&mut random);
            let intent_id = hex::encode(random);
            let inserted = database
                .enqueue_outbox(&OutboxIntent {
                    intent_id: intent_id.clone(),
                    host_id: key.server_id.clone(),
                    thread_id: Some(host_thread_id.clone()),
                    kind: OutboxIntentKind::SendMessage,
                    payload: payload.clone(),
                    state: OutboxState::Queued,
                    created_at_ms: unix_time_ms(),
                    attempt_count: 0,
                    next_attempt_at_ms: None,
                })
                .map_err(map_database_error)?;
            if inserted {
                self.schedule_remora_link_outbox_delivery();
                return Ok(AppRemoraLinkOfflineMessageEnqueueOutcome::Queued { intent_id });
            }
        }
        Err(ClientError::Serialization(
            "could not allocate a unique offline intent ID".to_string(),
        ))
    }

    pub(crate) fn schedule_remora_link_outbox_delivery(self: &Arc<Self>) {
        if self.configured_device_database().is_none() {
            return;
        }
        let client = Arc::clone(self);
        tokio::spawn(async move {
            if let Err(error) = client
                .deliver_remora_link_outbox(AUTOMATIC_OUTBOX_DELIVERY_BATCH)
                .await
            {
                warn!(%error, "automatic Remora Link outbox delivery failed");
            }
        });
    }

    pub(crate) async fn deliver_remora_link_outbox(
        self: &Arc<Self>,
        limit: usize,
    ) -> Result<AppRemoraLinkOutboxDeliveryReport, ClientError> {
        let _delivery = self.outbox_delivery.lock().await;
        let _configuration = self.remora_link_configuration.read().await;
        let database = self.configured_device_database().ok_or_else(|| {
            ClientError::InvalidParams("encrypted device storage is not configured".to_string())
        })?;
        let now_ms = unix_time_ms();
        let intents = database
            .due_outbox(now_ms, limit.clamp(1, MAX_OUTBOX_DELIVERY_BATCH))
            .map_err(map_database_error)?;
        let mut report = AppRemoraLinkOutboxDeliveryReport {
            considered: u32::try_from(intents.len()).unwrap_or(u32::MAX),
            ..Default::default()
        };
        for intent in intents {
            self.deliver_remora_link_outbox_intent(&database, intent, now_ms, &mut report)
                .await?;
        }
        Ok(report)
    }

    async fn deliver_remora_link_outbox_intent(
        self: &Arc<Self>,
        database: &crate::device_database::DeviceDatabase,
        intent: OutboxIntent,
        now_ms: i64,
        report: &mut AppRemoraLinkOutboxDeliveryReport,
    ) -> Result<(), ClientError> {
        if intent.kind != OutboxIntentKind::SendMessage {
            report.failed = report.failed.saturating_add(1);
            return defer_intent(database, &intent, now_ms);
        }
        let Some(host_thread_id) = intent.thread_id.as_deref() else {
            report.failed = report.failed.saturating_add(1);
            return defer_intent(database, &intent, now_ms);
        };
        let payload = match serde_json::from_slice::<OfflineTurnPayloadV1>(&intent.payload) {
            Ok(payload) if payload.v == 1 => payload,
            _ => {
                report.failed = report.failed.saturating_add(1);
                return defer_intent(database, &intent, now_ms);
            }
        };
        let Some(configured) = remora_link_read(&self.remora_link).clone() else {
            report.deferred = report.deferred.saturating_add(1);
            return defer_intent(database, &intent, now_ms);
        };
        let binding = match configured
            .lifecycle
            .resolve_thread_binding(&intent.host_id, host_thread_id)
            .await
        {
            Ok(ThreadBindingOutcomeV2::Bound(binding)) => binding,
            Ok(ThreadBindingOutcomeV2::Unavailable) | Err(_) => {
                report.deferred = report.deferred.saturating_add(1);
                return defer_intent(database, &intent, now_ms);
            }
        };
        configured.cache_thread_binding(
            &intent.host_id,
            &ThreadBindingOutcomeV2::Bound(binding.clone()),
        );
        if !self.has_remora_link_runtime_session(&intent.host_id, Some(&binding.runtime_id)) {
            report.deferred = report.deferred.saturating_add(1);
            return defer_intent(database, &intent, now_ms);
        }
        let provider_key = provider_thread_key(&intent, &binding);
        self.note_thread_runtime(provider_key.clone(), binding.runtime_id.clone());
        if self
            .external_resume_thread(&provider_key.server_id, &provider_key.thread_id, None)
            .await
            .is_err()
        {
            report.deferred = report.deferred.saturating_add(1);
            return defer_intent(database, &intent, now_ms);
        }
        let mut request = payload.request;
        request.thread_id = provider_key.thread_id.clone();
        validate_offline_turn_request(&provider_key, &request)?;
        let params = request.try_into().map_err(|error: crate::RpcClientError| {
            ClientError::Serialization(error.to_string())
        })?;
        let fingerprint = request_fingerprint(&intent.payload);
        match configured
            .lifecycle
            .prepare_send_message_intent(
                &intent.host_id,
                &intent.intent_id,
                host_thread_id,
                &fingerprint,
            )
            .await
        {
            Ok(WorkIntentOutcomeV2::Succeeded { .. }) => {
                return acknowledge_intent(database, &intent, report);
            }
            Ok(WorkIntentOutcomeV2::Execute | WorkIntentOutcomeV2::Reserved) => {}
            Ok(WorkIntentOutcomeV2::OutcomeUnknown) => {
                report.outcome_unknown = report.outcome_unknown.saturating_add(1);
                return defer_intent(database, &intent, now_ms);
            }
            Ok(WorkIntentOutcomeV2::Unavailable) | Err(_) => {
                report.deferred = report.deferred.saturating_add(1);
                return defer_intent(database, &intent, now_ms);
            }
        }

        let fence_configured = Arc::clone(&configured);
        let fence_host_id = intent.host_id.clone();
        let fence_intent_id = intent.intent_id.clone();
        let fence_thread_id = host_thread_id.to_string();
        let fence_fingerprint = fingerprint.clone();
        let dispatch_fence = Box::pin(async move {
            match fence_configured
                .lifecycle
                .begin_send_message_intent(
                    &fence_host_id,
                    &fence_intent_id,
                    &fence_thread_id,
                    &fence_fingerprint,
                )
                .await
            {
                Ok(WorkIntentOutcomeV2::Execute) => OutboxDispatchAuthorization::Execute,
                Ok(WorkIntentOutcomeV2::Succeeded { .. }) => {
                    OutboxDispatchAuthorization::HostAlreadySucceeded
                }
                _ => OutboxDispatchAuthorization::OutcomeUnknown,
            }
        });
        match self
            .start_outbox_turn(&provider_key.server_id, params, dispatch_fence)
            .await
        {
            Ok(OutboxTurnStartOutcome::Deferred) => {
                report.deferred = report.deferred.saturating_add(1);
                defer_intent(database, &intent, now_ms)
            }
            Ok(OutboxTurnStartOutcome::HostAlreadySucceeded) => {
                acknowledge_intent(database, &intent, report)
            }
            Ok(OutboxTurnStartOutcome::OutcomeUnknown) | Err(_) => {
                report.outcome_unknown = report.outcome_unknown.saturating_add(1);
                defer_intent(database, &intent, now_ms)
            }
            Ok(OutboxTurnStartOutcome::Accepted) => match configured
                .lifecycle
                .complete_send_message_intent(
                    &intent.host_id,
                    &intent.intent_id,
                    host_thread_id,
                    &fingerprint,
                )
                .await
            {
                Ok(WorkIntentOutcomeV2::Succeeded { .. }) => {
                    acknowledge_intent(database, &intent, report)
                }
                _ => {
                    report.outcome_unknown = report.outcome_unknown.saturating_add(1);
                    defer_intent(database, &intent, now_ms)
                }
            },
        }
    }
}

fn provider_thread_key(intent: &OutboxIntent, binding: &ThreadBindingReceiptV2) -> ThreadKey {
    ThreadKey {
        server_id: intent.host_id.clone(),
        thread_id: binding.provider_thread_id.clone(),
    }
}

fn validate_offline_turn_request(
    key: &ThreadKey,
    request: &AppStartTurnRequest,
) -> Result<(), ClientError> {
    if request.thread_id != key.thread_id {
        return Err(ClientError::InvalidParams(
            "offline message Thread identity does not match".to_string(),
        ));
    }
    if request.input.is_empty()
        || request
            .input
            .iter()
            .any(|input| !matches!(input, AppUserInput::Text { .. }))
    {
        return Err(ClientError::InvalidParams(
            "offline delivery accepts text input only".to_string(),
        ));
    }
    if !request
        .input
        .iter()
        .any(|input| matches!(input, AppUserInput::Text { text, .. } if !text.trim().is_empty()))
    {
        return Err(ClientError::InvalidParams(
            "offline message text must not be empty".to_string(),
        ));
    }
    Ok(())
}

fn unix_time_ms() -> i64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(millis).unwrap_or(i64::MAX).max(1)
}

fn request_fingerprint(payload: &[u8]) -> String {
    hex::encode(Sha256::digest(payload))
}

fn map_database_error(error: crate::device_database::DeviceDatabaseError) -> ClientError {
    ClientError::Serialization(error.to_string())
}

fn defer_intent(
    database: &crate::device_database::DeviceDatabase,
    intent: &OutboxIntent,
    now_ms: i64,
) -> Result<(), ClientError> {
    let shift = intent.attempt_count.min(8);
    let delay = OUTBOX_RETRY_BASE_MS
        .saturating_mul(1_i64 << shift)
        .min(OUTBOX_RETRY_MAX_MS);
    database
        .record_outbox_attempt(&intent.intent_id, now_ms.saturating_add(delay))
        .map(|_| ())
        .map_err(map_database_error)
}

fn acknowledge_intent(
    database: &crate::device_database::DeviceDatabase,
    intent: &OutboxIntent,
    report: &mut AppRemoraLinkOutboxDeliveryReport,
) -> Result<(), ClientError> {
    database
        .acknowledge_outbox(&intent.intent_id)
        .map_err(map_database_error)?;
    report.delivered = report.delivered.saturating_add(1);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_is_text_only_thread_bound_and_fingerprinted() {
        let key = ThreadKey {
            server_id: "host".to_string(),
            thread_id: "provider-thread".to_string(),
        };
        let mut request = AppStartTurnRequest {
            thread_id: key.thread_id.clone(),
            input: vec![AppUserInput::Text {
                text: "private prompt".to_string(),
                text_elements: Vec::new(),
            }],
            approval_policy: None,
            sandbox_policy: None,
            model: None,
            service_tier: None,
            effort: None,
            output_schema: None,
        };
        validate_offline_turn_request(&key, &request).expect("valid text intent");
        let payload = serde_json::to_vec(&OfflineTurnPayloadV1 {
            v: 1,
            request: request.clone(),
        })
        .expect("payload");
        let fingerprint = request_fingerprint(&payload);
        assert_eq!(fingerprint.len(), 64);
        assert!(
            fingerprint
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        );
        assert!(!fingerprint.contains("private prompt"));

        request.thread_id = "other-thread".to_string();
        assert!(matches!(
            validate_offline_turn_request(&key, &request),
            Err(ClientError::InvalidParams(_))
        ));
        request.thread_id = key.thread_id.clone();
        request.input = vec![AppUserInput::Image {
            url: "https://example.invalid/private.png".to_string(),
        }];
        assert!(matches!(
            validate_offline_turn_request(&key, &request),
            Err(ClientError::InvalidParams(_))
        ));
    }
}
