use async_trait::async_trait;

use super::types::*;

#[async_trait]
pub(crate) trait RelayBindingJournalPort: Send + Sync {
    /// Implementations must make each mutation atomic. Futures may be dropped
    /// after cancellation or deadline expiry, so a later load/CAS must reveal
    /// whether the replacement committed and allow an idempotent retry.
    async fn list(&self) -> Result<Vec<RelayBindingEntry>, RelayJournalError>;
    async fn load_by_host(
        &self,
        host_id: &RelayHostId,
    ) -> Result<Option<RelayBindingEntry>, RelayJournalError>;
    async fn load_by_installation(
        &self,
        installation_id: &RelayInstallationId,
    ) -> Result<Option<RelayBindingEntry>, RelayJournalError>;
    async fn compare_and_swap(
        &self,
        host_id: &RelayHostId,
        expected_revision: Option<u64>,
        replacement: RelayBindingEntry,
    ) -> Result<(), RelayJournalError>;
}

#[async_trait]
pub(crate) trait OpaqueRelaySecretPort: Send + Sync {
    /// Writes and deletes must be atomic and idempotent. A future may be
    /// dropped after the durable mutation but before its result is observed;
    /// retrying the same alias must therefore converge without exposing the
    /// secret bytes to the coordinator.
    async fn read(
        &self,
        alias: &RelaySecretAlias,
    ) -> Result<Option<OpaqueRelaySecret>, RelaySecretStoreError>;
    async fn write(
        &self,
        alias: &RelaySecretAlias,
        secret: OpaqueRelaySecret,
    ) -> Result<(), RelaySecretStoreError>;
    async fn delete(&self, alias: &RelaySecretAlias) -> Result<(), RelaySecretStoreError>;
}

#[derive(Clone, Debug)]
pub(crate) struct RelayTransportContext {
    pub(crate) origin: ValidatedRelayOrigin,
    pub(crate) authorization: OpaqueRelaySecret,
    pub(crate) operation: RelayOperationContext,
}

#[derive(Clone, Debug)]
pub(crate) struct RelayRegisterDeviceRequest {
    pub(crate) installation_id: RelayInstallationId,
    pub(crate) provider: RelayPushProvider,
    pub(crate) environment: RelayPushEnvironment,
    pub(crate) token: OpaqueRelaySecret,
}

#[derive(Clone, Debug)]
pub(crate) struct RelayTombstoneDeviceRequest {
    pub(crate) installation_id: RelayInstallationId,
    pub(crate) registration_id: RelayRegistrationId,
    pub(crate) through_generation: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct RelayFetchEventsRequest {
    pub(crate) installation_id: RelayInstallationId,
    pub(crate) after: u64,
    pub(crate) limit: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct RelayFetchSnapshotRequest {
    pub(crate) installation_id: RelayInstallationId,
}

#[derive(Clone, Debug)]
pub(crate) struct RelayAckRequest {
    pub(crate) installation_id: RelayInstallationId,
    pub(crate) through_cursor: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct RelayTombstoneInstallationRequest {
    pub(crate) installation_id: RelayInstallationId,
}

#[async_trait]
pub(crate) trait RelayTransportPort: Send + Sync {
    /// Concrete adapters must reject redirects and enforce
    /// `operation.max_response_bytes` while streaming, before buffering or
    /// deserializing a response. `encoded_bytes` in returned envelopes is a
    /// second semantic assertion, not the allocation guard.
    async fn register_device(
        &self,
        context: RelayTransportContext,
        request: RelayRegisterDeviceRequest,
    ) -> Result<RelayDeviceRegistrationReceipt, RelayTransportError>;

    async fn tombstone_device(
        &self,
        context: RelayTransportContext,
        request: RelayTombstoneDeviceRequest,
    ) -> Result<(), RelayTransportError>;

    async fn fetch_events(
        &self,
        context: RelayTransportContext,
        request: RelayFetchEventsRequest,
    ) -> Result<RelayEventPage, RelayTransportError>;

    async fn fetch_snapshot(
        &self,
        context: RelayTransportContext,
        request: RelayFetchSnapshotRequest,
    ) -> Result<RelaySnapshotEnvelope, RelayTransportError>;

    async fn acknowledge(
        &self,
        context: RelayTransportContext,
        request: RelayAckRequest,
    ) -> Result<RelayAckReceipt, RelayTransportError>;

    async fn tombstone_installation(
        &self,
        context: RelayTransportContext,
        request: RelayTombstoneInstallationRequest,
    ) -> Result<(), RelayTransportError>;
}

#[async_trait]
pub(crate) trait RelayAuthoritativeRepairPort: Send + Sync {
    async fn repair(
        &self,
        host_id: &RelayHostId,
        mode: RelayRepairMode,
        operation: RelayOperationContext,
    ) -> Result<RelayRepairReceipt, RelayRepairError>;
}

/// Optional delivery enrollment seam invoked by the pairing transaction.
///
/// The authenticated host adapter is free to omit relay enrollment. Pairing
/// remains usable without background delivery. When present, the adapter hands
/// an opaque, already-authenticated enrollment to this module; no host wire
/// types or pairing credentials cross this seam.
#[async_trait]
pub(crate) trait RemoteRelayEnrollmentPort: Send + Sync {
    /// Staging is replayable. If capability persistence is interrupted, the
    /// binding remains `Staged`; callers must replay this method with the same
    /// authenticated enrollment before commit or rollback can proceed. A new
    /// authenticated enrollment may replace `Tombstoned` or `NeedsRepair`;
    /// retryable retirement failures keep the terminal row authoritative.
    async fn stage_enrollment(
        &self,
        enrollment: RelayEnrollment,
        operation: RelayOperationContext,
    ) -> Result<(), RelayError>;
    async fn commit_enrollment(
        &self,
        host_id: &RelayHostId,
        operation: RelayOperationContext,
    ) -> Result<(), RelayError>;
    async fn rollback_enrollment(
        &self,
        host_id: &RelayHostId,
        operation: RelayOperationContext,
    ) -> Result<(), RelayError>;
}
