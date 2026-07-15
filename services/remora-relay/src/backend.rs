use std::sync::Arc;
use zeroize::Zeroizing;

use crate::{
    DeliveryOutcome, DeviceRegistrationResponse, EventPage, IngestEventRequest,
    IngestEventResponse, IssuedInstallation, MaintenanceResult, OpaqueId, OutboxLease,
    PostgresRelayStore, PresentedCapability, PushEnvironment, PushProviderKind, RelayError,
    RelayStore, Result, SnapshotEnvelope, StoreDiagnostics,
};

/// Runtime-selected storage adapter.
///
/// PostgreSQL is required for hosted and production self-hosted deployments.
/// SQLite is intentionally available only for a single-process local profile.
#[derive(Clone, Debug)]
pub enum RelayBackend {
    Postgres(Arc<PostgresRelayStore>),
    LocalSqlite(Arc<RelayStore>),
}

impl RelayBackend {
    pub async fn ready(&self) -> bool {
        match self {
            Self::Postgres(store) => store.ready().await,
            Self::LocalSqlite(store) => {
                let store = Arc::clone(store);
                tokio::task::spawn_blocking(move || store.ready())
                    .await
                    .unwrap_or(false)
            }
        }
    }

    pub async fn create_installation(&self, now_ms: i64) -> Result<IssuedInstallation> {
        match self {
            Self::Postgres(store) => store.create_installation(now_ms).await,
            Self::LocalSqlite(store) => {
                let store = Arc::clone(store);
                blocking(move || store.create_installation(now_ms)).await
            }
        }
    }

    pub async fn ingest_event(
        &self,
        installation_id: OpaqueId,
        capability: PresentedCapability,
        request: IngestEventRequest,
        now_ms: i64,
    ) -> Result<IngestEventResponse> {
        match self {
            Self::Postgres(store) => {
                store
                    .ingest_event(&installation_id, &capability, &request, now_ms)
                    .await
            }
            Self::LocalSqlite(store) => {
                let store = Arc::clone(store);
                blocking(move || {
                    store.ingest_event(&installation_id, &capability, &request, now_ms)
                })
                .await
            }
        }
    }

    pub async fn fetch_events(
        &self,
        installation_id: OpaqueId,
        capability: PresentedCapability,
        after: u64,
        limit: u32,
        now_ms: i64,
    ) -> Result<EventPage> {
        match self {
            Self::Postgres(store) => {
                store
                    .fetch_events(&installation_id, &capability, after, limit, now_ms)
                    .await
            }
            Self::LocalSqlite(store) => {
                let store = Arc::clone(store);
                blocking(move || {
                    store.fetch_events(&installation_id, &capability, after, limit, now_ms)
                })
                .await
            }
        }
    }

    pub async fn fetch_snapshot(
        &self,
        installation_id: OpaqueId,
        capability: PresentedCapability,
        now_ms: i64,
    ) -> Result<SnapshotEnvelope> {
        match self {
            Self::Postgres(store) => {
                store
                    .fetch_snapshot(&installation_id, &capability, now_ms)
                    .await
            }
            Self::LocalSqlite(store) => {
                let store = Arc::clone(store);
                blocking(move || store.fetch_snapshot(&installation_id, &capability, now_ms)).await
            }
        }
    }

    pub async fn register_device(
        &self,
        installation_id: OpaqueId,
        capability: PresentedCapability,
        provider: PushProviderKind,
        environment: PushEnvironment,
        token: Zeroizing<String>,
        now_ms: i64,
    ) -> Result<DeviceRegistrationResponse> {
        match self {
            Self::Postgres(store) => {
                store
                    .register_device(
                        &installation_id,
                        &capability,
                        provider,
                        environment,
                        &token,
                        now_ms,
                    )
                    .await
            }
            Self::LocalSqlite(store) => {
                let store = Arc::clone(store);
                blocking(move || {
                    store.register_device(
                        &installation_id,
                        &capability,
                        provider,
                        environment,
                        &token,
                        now_ms,
                    )
                })
                .await
            }
        }
    }

    pub async fn tombstone_registration(
        &self,
        installation_id: OpaqueId,
        capability: PresentedCapability,
        registration_id: OpaqueId,
        through_generation: u64,
        now_ms: i64,
    ) -> Result<()> {
        match self {
            Self::Postgres(store) => {
                store
                    .tombstone_registration(
                        &installation_id,
                        &capability,
                        &registration_id,
                        through_generation,
                        now_ms,
                    )
                    .await
            }
            Self::LocalSqlite(store) => {
                let store = Arc::clone(store);
                blocking(move || {
                    store.tombstone_registration(
                        &installation_id,
                        &capability,
                        &registration_id,
                        through_generation,
                        now_ms,
                    )
                })
                .await
            }
        }
    }

    pub async fn tombstone_installation(
        &self,
        installation_id: OpaqueId,
        capability: PresentedCapability,
        now_ms: i64,
    ) -> Result<()> {
        match self {
            Self::Postgres(store) => {
                store
                    .tombstone_installation(&installation_id, &capability, now_ms)
                    .await
            }
            Self::LocalSqlite(store) => {
                let store = Arc::clone(store);
                blocking(move || {
                    store.tombstone_installation(&installation_id, &capability, now_ms)
                })
                .await
            }
        }
    }

    pub async fn lease_deliveries(&self, now_ms: i64, limit: u32) -> Result<Vec<OutboxLease>> {
        match self {
            Self::Postgres(store) => store.lease_deliveries(now_ms, limit).await,
            Self::LocalSqlite(store) => {
                let store = Arc::clone(store);
                blocking(move || store.lease_deliveries(now_ms, limit)).await
            }
        }
    }

    pub async fn complete_delivery(
        &self,
        outbox_id: OpaqueId,
        generation: u64,
        registration_generation: u64,
        lease_id: OpaqueId,
        outcome: DeliveryOutcome,
        now_ms: i64,
    ) -> Result<()> {
        match self {
            Self::Postgres(store) => {
                store
                    .complete_delivery(
                        &outbox_id,
                        generation,
                        registration_generation,
                        &lease_id,
                        outcome,
                        now_ms,
                    )
                    .await
            }
            Self::LocalSqlite(store) => {
                let store = Arc::clone(store);
                blocking(move || {
                    store.complete_delivery(
                        &outbox_id,
                        generation,
                        registration_generation,
                        &lease_id,
                        outcome,
                        now_ms,
                    )
                })
                .await
            }
        }
    }

    pub async fn maintenance(&self, now_ms: i64) -> Result<MaintenanceResult> {
        match self {
            Self::Postgres(store) => store.maintenance(now_ms).await,
            Self::LocalSqlite(store) => {
                let store = Arc::clone(store);
                blocking(move || store.maintenance(now_ms)).await
            }
        }
    }

    pub async fn diagnostics(&self) -> Result<StoreDiagnostics> {
        match self {
            Self::Postgres(store) => store.diagnostics().await,
            Self::LocalSqlite(store) => {
                let store = Arc::clone(store);
                blocking(move || store.diagnostics()).await
            }
        }
    }
}

async fn blocking<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|_| RelayError::Provider)?
}
