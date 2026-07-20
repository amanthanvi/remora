use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use tokio::sync::watch;

use crate::{
    DeliveryOutcome, ProviderOutcome, ProviderRegistry, PushAttempt, RelayBackend, Result,
};

#[derive(Clone, Debug)]
pub struct PushDispatcher {
    backend: RelayBackend,
    providers: Arc<ProviderRegistry>,
    batch_size: u32,
}

impl PushDispatcher {
    pub fn new(
        backend: RelayBackend,
        providers: Arc<ProviderRegistry>,
        batch_size: u32,
    ) -> Result<Self> {
        if batch_size == 0 || batch_size > 1_000 {
            return Err(crate::RelayError::Configuration(
                "dispatch batch size must be between 1 and 1000".into(),
            ));
        }
        Ok(Self {
            backend,
            providers,
            batch_size,
        })
    }

    pub async fn dispatch_once(&self, now_ms: i64) -> Result<usize> {
        let leases = self
            .backend
            .lease_deliveries(now_ms, self.batch_size)
            .await?;
        let count = leases.len();
        let mut jobs = tokio::task::JoinSet::new();
        for lease in leases {
            let backend = self.backend.clone();
            let provider = self.providers.provider(lease.provider, lease.environment);
            jobs.spawn(async move {
                let outcome = provider
                    .send(
                        &PushAttempt {
                            token: lease.token,
                            hint: lease.hint,
                        },
                        now_ms,
                    )
                    .await;
                let outcome = match outcome {
                    ProviderOutcome::Accepted => DeliveryOutcome::Accepted,
                    ProviderOutcome::Suppressed => DeliveryOutcome::Suppressed,
                    ProviderOutcome::InvalidToken => DeliveryOutcome::InvalidToken,
                    ProviderOutcome::Retry { retry_after_ms } => {
                        DeliveryOutcome::Retry { retry_after_ms }
                    }
                    ProviderOutcome::PermanentFailure => DeliveryOutcome::PermanentFailure,
                };
                backend
                    .complete_delivery(
                        lease.outbox_id,
                        lease.generation,
                        lease.registration_generation,
                        lease.lease_id,
                        outcome,
                        now_ms,
                    )
                    .await
            });
        }
        let mut first_error = None;
        while let Some(result) = jobs.join_next().await {
            let result = result
                .map_err(|_| crate::RelayError::Provider)
                .and_then(|result| result);
            if let Err(error) = result
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        Ok(count)
    }

    /// Drain all work that was due at `now_ms` in bounded-concurrency batches.
    /// Retry scheduling always advances beyond this timestamp, so a transient
    /// provider failure cannot hot-loop inside one drain cycle.
    pub async fn dispatch_due(&self, now_ms: i64) -> Result<usize> {
        let mut total = 0_usize;
        loop {
            let count = self.dispatch_once(now_ms).await?;
            total = total.saturating_add(count);
            if count < self.batch_size as usize {
                return Ok(total);
            }
            tokio::task::yield_now().await;
        }
    }

    pub async fn run(
        &self,
        poll_interval: Duration,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<()> {
        let mut interval = tokio::time::interval(poll_interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    match self.dispatch_due(unix_time_ms()).await {
                        Ok(count) if count > 0 => tracing::debug!(count, "push dispatch drain completed"),
                        Ok(_) => {}
                        Err(error) => tracing::warn!(error_kind = error_class(&error), "push dispatch batch failed"),
                    }
                }
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        return Ok(());
                    }
                }
            }
        }
    }
}

pub fn unix_time_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

fn error_class(error: &crate::RelayError) -> &'static str {
    match error {
        crate::RelayError::Unauthorized => "unauthorized",
        crate::RelayError::NotFound => "not_found",
        crate::RelayError::Conflict => "conflict",
        crate::RelayError::Invalid(_) => "invalid",
        crate::RelayError::LimitExceeded => "limit",
        crate::RelayError::Tombstoned => "tombstoned",
        crate::RelayError::ResetRequired => "reset",
        crate::RelayError::NotConfigured => "not_configured",
        crate::RelayError::Storage(_) | crate::RelayError::Postgres(_) => "storage",
        crate::RelayError::Crypto => "crypto",
        crate::RelayError::Provider => "provider",
        crate::RelayError::InjectedFault => "fault",
        crate::RelayError::Configuration(_) => "configuration",
        crate::RelayError::Io(_) => "io",
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

    use super::*;
    use crate::{
        CreateInstallationRequest, EventClass, IngestEventRequest, MockProvider, OpaqueId,
        PresentedCapability, PushEnvironment, PushProviderKind, RelayMetrics, RelayStore,
        StoreLimits, TokenCipher,
    };

    #[tokio::test]
    async fn transient_then_success_preserves_event_and_retries_hint() {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(
            RelayStore::open(
                directory.path().join("relay.sqlite3"),
                TokenCipher::from_key([3; 32]),
                StoreLimits {
                    retry_base_ms: 1,
                    retry_cap_ms: 1,
                    ..StoreLimits::default()
                },
                Arc::new(RelayMetrics::default()),
            )
            .unwrap(),
        );
        let creation =
            CreateInstallationRequest::new("txn_worker_retry_000000000000000001").unwrap();
        let installation = store.create_installation(&creation, 1_000).unwrap();
        let manage =
            PresentedCapability::parse(installation.manage_capability.as_str().to_owned()).unwrap();
        let write =
            PresentedCapability::parse(installation.write_capability.as_str().to_owned()).unwrap();
        store
            .register_device(
                &installation.installation_id,
                &manage,
                PushProviderKind::Fcm,
                PushEnvironment::Production,
                "fcm-destination-token-000000",
                1_000,
            )
            .unwrap();
        store
            .ingest_event(
                &installation.installation_id,
                &write,
                &IngestEventRequest {
                    event_id: OpaqueId::parse("evt_worker_retry_00001").unwrap(),
                    event_class: EventClass::StateChanged,
                    expires_at_ms: 20_000,
                    ciphertext: URL_SAFE_NO_PAD.encode([8_u8; 32]),
                    snapshot: None,
                },
                1_000,
            )
            .unwrap();

        let fcm = Arc::new(MockProvider::new(
            PushProviderKind::Fcm,
            PushEnvironment::Production,
        ));
        fcm.script([
            ProviderOutcome::Retry {
                retry_after_ms: Some(1),
            },
            ProviderOutcome::Accepted,
        ]);
        let providers = Arc::new(ProviderRegistry::new(
            Arc::new(crate::DisabledProvider::new(
                PushProviderKind::Apns,
                PushEnvironment::Production,
            )),
            Arc::new(crate::DisabledProvider::new(
                PushProviderKind::Apns,
                PushEnvironment::Sandbox,
            )),
            fcm.clone(),
        ));
        let dispatcher =
            PushDispatcher::new(RelayBackend::LocalSqlite(store.clone()), providers, 8).unwrap();
        assert_eq!(dispatcher.dispatch_once(1_001).await.unwrap(), 1);
        assert_eq!(dispatcher.dispatch_once(1_003).await.unwrap(), 1);
        assert_eq!(fcm.records().len(), 2);
        assert_eq!(store.diagnostics().unwrap().retained_events, 1);
        assert_eq!(store.diagnostics().unwrap().pending_outbox, 0);
    }

    #[tokio::test]
    async fn due_work_drains_across_multiple_batches() {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(
            RelayStore::open(
                directory.path().join("relay.sqlite3"),
                TokenCipher::from_key([4; 32]),
                StoreLimits::default(),
                Arc::new(RelayMetrics::default()),
            )
            .unwrap(),
        );
        let creation =
            CreateInstallationRequest::new("txn_worker_drain_000000000000000001").unwrap();
        let installation = store.create_installation(&creation, 1_000).unwrap();
        let manage =
            PresentedCapability::parse(installation.manage_capability.as_str().to_owned()).unwrap();
        let write =
            PresentedCapability::parse(installation.write_capability.as_str().to_owned()).unwrap();
        for (provider, environment, token) in [
            (
                PushProviderKind::Apns,
                PushEnvironment::Production,
                "apns-production-drain-token",
            ),
            (
                PushProviderKind::Apns,
                PushEnvironment::Sandbox,
                "apns-sandbox-drain-token-0001",
            ),
            (
                PushProviderKind::Fcm,
                PushEnvironment::Production,
                "fcm-production-drain-token-0001",
            ),
        ] {
            store
                .register_device(
                    &installation.installation_id,
                    &manage,
                    provider,
                    environment,
                    token,
                    1_000,
                )
                .unwrap();
        }
        for (index, event_class) in [
            EventClass::StateChanged,
            EventClass::ActivityChanged,
            EventClass::ConnectionChanged,
            EventClass::SecurityChanged,
        ]
        .into_iter()
        .enumerate()
        {
            store
                .ingest_event(
                    &installation.installation_id,
                    &write,
                    &IngestEventRequest {
                        event_id: OpaqueId::parse(format!("evt_worker_drain_{index:04}")).unwrap(),
                        event_class,
                        expires_at_ms: 20_000,
                        ciphertext: URL_SAFE_NO_PAD.encode([index as u8; 32]),
                        snapshot: None,
                    },
                    1_000,
                )
                .unwrap();
        }

        let apns = Arc::new(MockProvider::new(
            PushProviderKind::Apns,
            PushEnvironment::Production,
        ));
        let apns_sandbox = Arc::new(MockProvider::new(
            PushProviderKind::Apns,
            PushEnvironment::Sandbox,
        ));
        let fcm = Arc::new(MockProvider::new(
            PushProviderKind::Fcm,
            PushEnvironment::Production,
        ));
        let providers = Arc::new(ProviderRegistry::new(
            apns.clone(),
            apns_sandbox.clone(),
            fcm.clone(),
        ));
        let dispatcher =
            PushDispatcher::new(RelayBackend::LocalSqlite(store.clone()), providers, 3).unwrap();
        assert_eq!(dispatcher.dispatch_due(1_001).await.unwrap(), 12);
        assert_eq!(
            apns.records().len() + apns_sandbox.records().len() + fcm.records().len(),
            12
        );
        assert_eq!(store.diagnostics().unwrap().pending_outbox, 0);
    }
}
