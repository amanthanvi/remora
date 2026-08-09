//! Narrow UniFFI seam for the Rust-owned background relay coordinator.
//!
//! Native code supplies only atomic persistence, secure custody, and an
//! authoritative host-repair hook. HTTP protocol handling, enrollment,
//! provider-token aliasing, wake ordering, reconciliation, and cleanup stay in
//! Rust. Secret-bearing values cross only direct command/custody calls; they
//! never enter status records, AppStore snapshots, diagnostics, or logs.

use std::{
    collections::HashSet,
    mem::size_of,
    sync::{Arc, RwLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use hmac::{Hmac, Mac};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

use crate::{
    background_relay::{
        BackgroundRelay, ConfiguredBackgroundRelay, MAX_RETIRED_TOKEN_ALIASES, OpaqueRelaySecret,
        OpaqueRelaySecretPort, OpaqueWakeHint, PushTokenObservation, PushTokenTombstone,
        RelayAuthoritativeRepairPort, RelayBindingEntry, RelayBindingJournalPort,
        RelayBindingState, RelayBindingStatus, RelayEnrollment, RelayEnrollmentCommandId,
        RelayError, RelayEventClass, RelayEventId, RelayHostId, RelayInstallationId,
        RelayJournalError, RelayOperationContext, RelayPreviousProviderRegistration,
        RelayProviderRegistration, RelayProviderTombstoneFence, RelayPushEnvironment,
        RelayPushProvider, RelayReconcileOutcome, RelayReconcileReceipt,
        RelayRegistrationDisposition, RelayRegistrationId, RelayRepairError, RelayRepairMode,
        RelayRepairReceipt, RelaySecretAlias, RelaySecretCasOutcome, RelaySecretCreateOutcome,
        RelaySecretRevision, RelaySecretStoreError, RelayWakeLedger, RemoteRelayEnrollmentPort,
        ReqwestRelayTransport, SeenWake,
    },
    ffi::AppClient,
};

const OPERATION_TIMEOUT: Duration = Duration::from_secs(25);
const JOURNAL_SCHEMA_VERSION: u16 = 1;
const JOURNAL_INTEGRITY_ALIAS: &str = "remora_relay_journal_integrity_v1";
const JOURNAL_ROLLBACK_ANCHOR_ALIAS: &str = "remora_relay_journal_anchor_v1";
const JOURNAL_INTEGRITY_KEY_BYTES: usize = 32;
const JOURNAL_AUTH_MAGIC: &[u8] = b"remora-relay-journal-auth-v1\0";
const JOURNAL_ANCHOR_MAGIC: &[u8] = b"remora-relay-journal-anchor-v1\0";
const JOURNAL_AUTH_TAG_BYTES: usize = 32;
const JOURNAL_ANCHOR_DIGEST_BYTES: usize = 32;
const MAX_JOURNAL_BYTES: usize = 512 * 1_024;
const MAX_BINDINGS: usize = 256;
const MAX_REGISTRATIONS_PER_BINDING: usize = 3;
const MAX_RECENT_WAKES: usize = 32;
const MAX_HOST_ID_BYTES: usize = 256;

// ── Native persistence/custody callbacks ────────────────────────────────

#[derive(Clone, uniffi::Record)]
pub struct AppRelayJournalSnapshot {
    /// Monotonic generation used only for whole-blob CAS.
    pub revision: u64,
    /// Opaque, versioned, non-secret Rust payload. Native code must not parse
    /// or rewrite it.
    pub payload: Vec<u8>,
}

#[derive(Clone, uniffi::Enum)]
pub enum AppRelayJournalLoad {
    Missing,
    Loaded { snapshot: AppRelayJournalSnapshot },
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AppRelayJournalWriteOutcome {
    Stored,
    Conflict,
    Unavailable,
}

/// Atomic persistence for one opaque journal blob.
///
/// `compare_and_swap` must atomically compare the currently persisted outer
/// revision with `expected_revision` (`None` means no blob) and replace it.
/// It may be called from a background runtime thread. Payload bytes contain no
/// capabilities or provider tokens.
#[uniffi::export(callback_interface)]
#[async_trait]
pub trait AppRelayJournalBackend: Send + Sync {
    async fn load(&self) -> AppRelayJournalLoad;
    async fn compare_and_swap(
        &self,
        expected_revision: Option<u64>,
        replacement: AppRelayJournalSnapshot,
    ) -> AppRelayJournalWriteOutcome;
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum AppRelaySecretReadError {
    #[error("relay secret is missing")]
    Missing,
    #[error("relay secure storage is unavailable")]
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AppRelaySecretWriteOutcome {
    Applied,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AppRelaySecretCreateOutcome {
    Created,
    AlreadyExists,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AppRelaySecretRevision {
    Missing,
    Found { revision: u64 },
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AppRelaySecretCasOutcome {
    Stored,
    Conflict,
    Unavailable,
}

/// Secret bytes passed directly to native secure custody. Rust zeroizes every
/// local instance when the callback finishes. Generated Swift and Kotlin
/// adapters also wipe the transfer buffer and their transient native value.
#[derive(Clone)]
pub struct AppRelaySecretValue(Vec<u8>);

impl AppRelaySecretValue {
    pub(crate) fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    #[cfg(test)]
    fn expose_for_adapter(&self) -> &[u8] {
        &self.0
    }

    pub(crate) fn into_bytes(mut self) -> Vec<u8> {
        std::mem::take(&mut self.0)
    }
}

impl Drop for AppRelaySecretValue {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl From<AppRelaySecretValue> for Vec<u8> {
    fn from(value: AppRelaySecretValue) -> Self {
        // UniFFI necessarily creates its own transfer buffer. Clone while the
        // custom value is still alive so its Rust-owned bytes are zeroized as
        // soon as lowering completes.
        value.0.clone()
    }
}

impl From<Vec<u8>> for AppRelaySecretValue {
    fn from(value: Vec<u8>) -> Self {
        Self(value)
    }
}

uniffi::custom_type!(AppRelaySecretValue, Vec<u8>);

/// Platform secure-storage custody keyed by Rust-generated aliases.
///
/// Writes and deletes must be atomic and idempotent. Implementations must not
/// log aliases together with values, include this store in device backups, or
/// surface values through observable native state. `write` and
/// `create_if_absent` must finish copying into secure storage before returning
/// and must not retain or asynchronously reuse the supplied value; the
/// generated callback wrapper wipes that transient value immediately after the
/// callback returns.
///
/// The reserved journal rollback-anchor alias additionally requires durable,
/// rollback-resistant storage: native restore/backup must never replace it
/// with an older value or lower its atomic revision. Rust advances that anchor
/// only with version-fenced CAS after the corresponding journal CAS commits.
#[uniffi::export(callback_interface)]
#[async_trait]
pub trait AppRelaySecretBackend: Send + Sync {
    /// Return the requested secret directly. Missing and unavailable are typed
    /// callback errors so the secret does not acquire an outer enum transfer
    /// buffer. Generated wrappers wipe the native source value after lowering.
    async fn read(&self, alias: String) -> Result<AppRelaySecretValue, AppRelaySecretReadError>;
    async fn write(&self, alias: String, value: AppRelaySecretValue) -> AppRelaySecretWriteOutcome;
    /// Atomically create `alias` only when it is absent. A timed-out invocation
    /// may complete late, so ordinary read-then-write is not sufficient for
    /// stable aliases such as the journal integrity key.
    async fn create_if_absent(
        &self,
        alias: String,
        value: AppRelaySecretValue,
    ) -> AppRelaySecretCreateOutcome;
    /// Return a non-secret revision for fenced staging writes.
    async fn revision(&self, alias: String) -> AppRelaySecretRevision;
    /// Atomic secret CAS. `expected_revision == None` means the alias must be
    /// absent. `replacement_revision` is nonzero and strictly greater than the
    /// matched revision, and must be stored atomically with the value.
    async fn compare_and_swap(
        &self,
        alias: String,
        expected_revision: Option<u64>,
        replacement_revision: u64,
        value: AppRelaySecretValue,
    ) -> AppRelaySecretCasOutcome;
    /// Atomically remove the value while retaining `replacement_revision` as
    /// a non-secret tombstone. `read` must report Missing afterward while
    /// `revision` reports the retained revision.
    async fn compare_and_tombstone(
        &self,
        alias: String,
        expected_revision: Option<u64>,
        replacement_revision: u64,
    ) -> AppRelaySecretCasOutcome;
    async fn delete(&self, alias: String) -> AppRelaySecretWriteOutcome;
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AppRelayRepairMode {
    Incremental {
        after_cursor: u64,
        through_cursor: u64,
    },
    Snapshot {
        revision: u64,
        through_cursor: u64,
    },
    Full {
        through_cursor: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AppRelayRepairResult {
    /// Return only after authenticated authoritative host state has been
    /// applied durably through the exact cursor.
    Applied {
        through_cursor: u64,
    },
    Unavailable,
    RePairRequired,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AppRelayRepairRequest {
    pub host_id: String,
    pub mode: AppRelayRepairMode,
    /// Authenticated, journal-backed per-host generation allocated before the
    /// native call. Native code must atomically accept it only when it is
    /// strictly greater than its persisted generation, then compare the same
    /// generation again in the durable repair commit transaction.
    pub generation: u64,
    /// Absolute Unix deadline. A callback that reaches its commit point at or
    /// after this time must return `Cancelled` without changing state.
    pub deadline_unix_ms: u64,
}

/// Native hook into the existing authenticated host lifecycle.
///
/// The callback selects the already-paired host by `host_id`; relay contents
/// are never passed to it. Before doing work, it atomically persists
/// `generation` only when it is strictly greater than the host's current
/// generation. Otherwise it returns `Cancelled`. Immediately before its
/// durable commit it must atomically verify both that the same generation is
/// still current and that `deadline_unix_ms` has not elapsed. Thus an older
/// queued callback cannot replace a newer fence even if it starts later.
/// Returning `Applied` asserts that canonical Rust/AppStore state is durably
/// authoritative through the requested cursor.
#[uniffi::export(callback_interface)]
#[async_trait]
pub trait AppRelayRepairBackend: Send + Sync {
    async fn repair(&self, request: AppRelayRepairRequest) -> AppRelayRepairResult;
}

// ── Public command and projection types ─────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AppRelayPushProvider {
    Apns,
    Fcm,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AppRelayPushEnvironment {
    Sandbox,
    Production,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AppRelayEventClass {
    StateChanged,
    ActivityChanged,
    ConnectionChanged,
    SecurityChanged,
}

/// Direct command value. Supply raw APNs device-token bytes or UTF-8 FCM token
/// bytes. Rust canonicalizes APNs bytes to lowercase hexadecimal before
/// custody and rejects non-text FCM values before mutating durable state. The
/// token is supplied separately as a direct custom-type method argument so
/// its transfer buffer moves immediately into Rust's zeroizing wrapper.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AppRelayPushTokenObservation {
    pub provider: AppRelayPushProvider,
    pub environment: AppRelayPushEnvironment,
    pub local_generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AppRelayPushTokenTombstone {
    pub provider: AppRelayPushProvider,
    pub environment: AppRelayPushEnvironment,
    pub through_local_generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AppRelayWakeHint {
    pub schema_version: u16,
    pub installation_id: String,
    pub event_id: String,
    pub cursor: u64,
    pub event_class: AppRelayEventClass,
    pub expires_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AppRelayFanoutReceipt {
    pub attempted: u32,
    pub synchronized: u32,
    pub pending_retry: u32,
    pub re_pair_required: u32,
    pub rejected: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AppRelayReconcileReceipt {
    pub host_id: String,
    pub applied_through_cursor: u64,
    pub acknowledged_through_cursor: u64,
    pub changed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AppRelayFailure {
    InvalidRelayOrigin,
    InsecureRelayOrigin,
    InvalidSecret,
    InvalidProviderRegistration,
    InvalidWake,
    InvalidResponse,
    UnknownInstallation,
    JournalUnavailable,
    SecureStorageUnavailable,
    DeadlineExceeded,
    Cancelled,
    Retryable,
    RePairRequired,
    Tombstoned,
    PermanentFailure,
    RepairFailed,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AppRelayReconcileOutcome {
    Applied {
        receipt: AppRelayReconcileReceipt,
    },
    CleanupCompleted {
        host_id: String,
    },
    Failed {
        host_id: String,
        failure: AppRelayFailure,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AppRelayBindingState {
    Preparing,
    RollbackPending,
    Staged,
    Active,
    TombstonePending,
    CleanupPending,
    Tombstoned,
    NeedsRepair,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AppRelayBindingStatus {
    pub host_id: String,
    pub installation_id: String,
    pub state: AppRelayBindingState,
    pub highest_seen_cursor: u64,
    pub applied_cursor: u64,
    pub pending_ack_cursor: Option<u64>,
    pub provider_registration_count: u32,
    pub has_pending_provider_sync: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AppRelayStatusSnapshot {
    pub configured: bool,
    pub bindings: Vec<AppRelayBindingStatus>,
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum BackgroundRelayError {
    #[error("background relay is not configured")]
    NotConfigured,
    #[error("relay origin is invalid")]
    InvalidRelayOrigin,
    #[error("relay origin must use HTTPS")]
    InsecureRelayOrigin,
    #[error("relay secret is invalid")]
    InvalidSecret,
    #[error("provider registration is invalid")]
    InvalidProviderRegistration,
    #[error("wake hint is invalid")]
    InvalidWake,
    #[error("relay response or durable state is invalid")]
    InvalidResponse,
    #[error("relay installation is unknown")]
    UnknownInstallation,
    #[error("relay journal is unavailable")]
    JournalUnavailable,
    #[error("relay secure storage is unavailable")]
    SecureStorageUnavailable,
    #[error("relay operation exceeded its deadline")]
    DeadlineExceeded,
    #[error("relay operation was cancelled")]
    Cancelled,
    #[error("relay operation should be retried")]
    Retryable,
    #[error("relay enrollment requires re-pairing")]
    RePairRequired,
    #[error("relay installation is tombstoned")]
    Tombstoned,
    #[error("relay rejected the operation permanently")]
    PermanentFailure,
    #[error("authoritative host repair failed")]
    RepairFailed,
}

impl From<RelayError> for BackgroundRelayError {
    fn from(value: RelayError) -> Self {
        match value {
            RelayError::InvalidRelayOrigin => Self::InvalidRelayOrigin,
            RelayError::InsecureRelayOrigin => Self::InsecureRelayOrigin,
            RelayError::InvalidSecret => Self::InvalidSecret,
            RelayError::InvalidProviderRegistration => Self::InvalidProviderRegistration,
            RelayError::InvalidWake => Self::InvalidWake,
            RelayError::InvalidResponse => Self::InvalidResponse,
            RelayError::UnknownInstallation => Self::UnknownInstallation,
            RelayError::JournalUnavailable => Self::JournalUnavailable,
            RelayError::SecureStorageUnavailable => Self::SecureStorageUnavailable,
            RelayError::DeadlineExceeded => Self::DeadlineExceeded,
            RelayError::Cancelled => Self::Cancelled,
            RelayError::Retryable => Self::Retryable,
            RelayError::RePairRequired => Self::RePairRequired,
            RelayError::Tombstoned => Self::Tombstoned,
            RelayError::PermanentFailure => Self::PermanentFailure,
            RelayError::RepairFailed => Self::RepairFailed,
        }
    }
}

impl From<RelayError> for AppRelayFailure {
    fn from(value: RelayError) -> Self {
        match value {
            RelayError::InvalidRelayOrigin => Self::InvalidRelayOrigin,
            RelayError::InsecureRelayOrigin => Self::InsecureRelayOrigin,
            RelayError::InvalidSecret => Self::InvalidSecret,
            RelayError::InvalidProviderRegistration => Self::InvalidProviderRegistration,
            RelayError::InvalidWake => Self::InvalidWake,
            RelayError::InvalidResponse => Self::InvalidResponse,
            RelayError::UnknownInstallation => Self::UnknownInstallation,
            RelayError::JournalUnavailable => Self::JournalUnavailable,
            RelayError::SecureStorageUnavailable => Self::SecureStorageUnavailable,
            RelayError::DeadlineExceeded => Self::DeadlineExceeded,
            RelayError::Cancelled => Self::Cancelled,
            RelayError::Retryable => Self::Retryable,
            RelayError::RePairRequired => Self::RePairRequired,
            RelayError::Tombstoned => Self::Tombstoned,
            RelayError::PermanentFailure => Self::PermanentFailure,
            RelayError::RepairFailed => Self::RepairFailed,
        }
    }
}

// ── Configured module and AppClient interface ───────────────────────────

#[uniffi::export(async_runtime = "tokio")]
impl AppClient {
    /// Install or atomically replace the optional relay adapters.
    ///
    /// The journal is loaded and validated before the new configuration
    /// becomes visible. Existing in-flight calls retain the old configuration
    /// until they finish.
    pub async fn configure_background_relay(
        &self,
        journal: Box<dyn AppRelayJournalBackend>,
        secrets: Box<dyn AppRelaySecretBackend>,
        repair: Box<dyn AppRelayRepairBackend>,
        allow_loopback_http: bool,
    ) -> Result<(), BackgroundRelayError> {
        let _configuration = self.inner.background_relay_configuration.lock().await;
        // One absolute deadline covers every callback while the configuration
        // lock is held. Individual callback timeouts consume, rather than
        // reset, this shared budget.
        let operation = RelayOperationContext::with_timeout(OPERATION_TIMEOUT);
        let configured = build_background_relay_configuration(
            journal,
            secrets,
            repair,
            allow_loopback_http,
            &operation,
        )
        .await?;
        *relay_write(&self.inner.background_relay) = Some(configured);
        Ok(())
    }

    /// Remove callback references. In-flight operations keep their cloned
    /// configuration and remain bounded by the operation deadline.
    pub async fn clear_background_relay(&self) {
        let _configuration = self.inner.background_relay_configuration.lock().await;
        *relay_write(&self.inner.background_relay) = None;
    }

    pub async fn background_relay_status(
        &self,
    ) -> Result<AppRelayStatusSnapshot, BackgroundRelayError> {
        let Some(configured) = relay_read(&self.inner.background_relay).clone() else {
            return Ok(AppRelayStatusSnapshot {
                configured: false,
                bindings: Vec::new(),
            });
        };
        let statuses = configured
            .relay
            .statuses(operation())
            .await?
            .into_iter()
            .map(AppRelayBindingStatus::from)
            .collect();
        Ok(AppRelayStatusSnapshot {
            configured: true,
            bindings: statuses,
        })
    }

    pub async fn background_relay_observe_push_token(
        &self,
        observation: AppRelayPushTokenObservation,
        token: AppRelaySecretValue,
    ) -> Result<AppRelayFanoutReceipt, BackgroundRelayError> {
        let configured = self.configured_background_relay()?;
        let provider: RelayPushProvider = observation.provider.into();
        let observation = PushTokenObservation {
            provider,
            environment: observation.environment.into(),
            token: canonical_provider_token(provider, token.into_bytes())?,
            local_generation: observation.local_generation,
            observed_at_ms: unix_time_ms()?,
        };
        Ok(configured
            .relay
            .observe_push_token(observation, operation())
            .await?
            .into())
    }

    pub async fn background_relay_tombstone_push_token(
        &self,
        tombstone: AppRelayPushTokenTombstone,
    ) -> Result<AppRelayFanoutReceipt, BackgroundRelayError> {
        let configured = self.configured_background_relay()?;
        Ok(configured
            .relay
            .tombstone_push_token(
                PushTokenTombstone {
                    provider: tombstone.provider.into(),
                    environment: tombstone.environment.into(),
                    through_local_generation: tombstone.through_local_generation,
                },
                operation(),
            )
            .await?
            .into())
    }

    pub async fn background_relay_ingest_wake(
        &self,
        hint: AppRelayWakeHint,
    ) -> Result<AppRelayReconcileReceipt, BackgroundRelayError> {
        let configured = self.configured_background_relay()?;
        let now_ms = unix_time_ms()?;
        let hint = OpaqueWakeHint {
            schema_version: hint.schema_version,
            installation_id: RelayInstallationId::parse(hint.installation_id)?,
            event_id: RelayEventId::parse(hint.event_id)?,
            cursor: hint.cursor,
            event_class: hint.event_class.into(),
            expires_at_ms: hint.expires_at_ms,
        };
        Ok(configured
            .relay
            .ingest_wake(hint, now_ms, operation())
            .await?
            .into())
    }

    pub async fn background_relay_reconcile(
        &self,
    ) -> Result<Vec<AppRelayReconcileOutcome>, BackgroundRelayError> {
        let configured = self.configured_background_relay()?;
        Ok(configured
            .relay
            .reconcile_all(unix_time_ms()?, operation())
            .await?
            .into_iter()
            .map(AppRelayReconcileOutcome::from)
            .collect())
    }

    /// Stage an optional authenticated relay enrollment. Capabilities are
    /// direct custom-type arguments so inbound transfer buffers immediately
    /// move into zeroizing Rust custody.
    pub async fn background_relay_stage_enrollment(
        &self,
        host_id: String,
        relay_origin: String,
        installation_id: String,
        command_id: String,
        read_capability: AppRelaySecretValue,
        manage_capability: AppRelaySecretValue,
    ) -> Result<(), BackgroundRelayError> {
        let configured = self.configured_background_relay()?;
        let enrollment = RelayEnrollment {
            host_id: parse_host_id(host_id)?,
            origin: crate::background_relay::ValidatedRelayOrigin::parse(
                &relay_origin,
                configured.allow_loopback_http,
            )?,
            installation_id: RelayInstallationId::parse(installation_id)?,
            command_id: RelayEnrollmentCommandId::parse(command_id)?,
            read_capability: OpaqueRelaySecret::new(read_capability.into_bytes())?,
            manage_capability: OpaqueRelaySecret::new(manage_capability.into_bytes())?,
        };
        configured
            .relay
            .stage_enrollment(enrollment, operation())
            .await?;
        Ok(())
    }

    pub async fn background_relay_commit_enrollment(
        &self,
        host_id: String,
        command_id: String,
    ) -> Result<(), BackgroundRelayError> {
        let configured = self.configured_background_relay()?;
        configured
            .relay
            .commit_enrollment(
                &parse_host_id(host_id)?,
                &RelayEnrollmentCommandId::parse(command_id)?,
                operation(),
            )
            .await?;
        Ok(())
    }

    pub async fn background_relay_rollback_enrollment(
        &self,
        host_id: String,
        command_id: String,
    ) -> Result<(), BackgroundRelayError> {
        let configured = self.configured_background_relay()?;
        configured
            .relay
            .rollback_enrollment(
                &parse_host_id(host_id)?,
                &RelayEnrollmentCommandId::parse(command_id)?,
                operation(),
            )
            .await?;
        Ok(())
    }
}

async fn build_background_relay_configuration(
    journal: Box<dyn AppRelayJournalBackend>,
    secrets: Box<dyn AppRelaySecretBackend>,
    repair: Box<dyn AppRelayRepairBackend>,
    allow_loopback_http: bool,
    operation: &RelayOperationContext,
) -> Result<Arc<ConfiguredBackgroundRelay>, BackgroundRelayError> {
    let journal_backend: Arc<dyn AppRelayJournalBackend> = Arc::from(journal);
    let journal_exists = match run_preflight_callback(operation, journal_backend.load()).await? {
        AppRelayJournalLoad::Missing => false,
        AppRelayJournalLoad::Loaded { .. } => true,
        AppRelayJournalLoad::Unavailable => {
            return Err(BackgroundRelayError::JournalUnavailable);
        }
    };
    let secrets = Arc::new(NativeSecretAdapter::new(Arc::from(secrets)));
    let integrity_key =
        load_or_create_journal_integrity_key(secrets.as_ref(), !journal_exists, operation).await?;
    let journal = Arc::new(NativeJournalAdapter::new(
        journal_backend,
        Arc::clone(&secrets),
        allow_loopback_http,
        integrity_key,
    ));
    // Fail closed on corrupt or unavailable durable state. Configuration is
    // not published until the complete journal can be authenticated/decoded.
    validate_journal_before_publish(journal.as_ref(), operation).await?;
    let repair = Arc::new(NativeRepairAdapter::new(Arc::from(repair)));
    let transport = Arc::new(ReqwestRelayTransport::new()?);
    let relay = Arc::new(BackgroundRelay::new(journal, secrets, transport, repair));
    Ok(Arc::new(ConfiguredBackgroundRelay {
        relay,
        allow_loopback_http,
    }))
}

impl AppClient {
    fn configured_background_relay(
        &self,
    ) -> Result<Arc<ConfiguredBackgroundRelay>, BackgroundRelayError> {
        relay_read(&self.inner.background_relay)
            .clone()
            .ok_or(BackgroundRelayError::NotConfigured)
    }
}

fn relay_read(
    relay: &RwLock<Option<Arc<ConfiguredBackgroundRelay>>>,
) -> std::sync::RwLockReadGuard<'_, Option<Arc<ConfiguredBackgroundRelay>>> {
    relay
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn relay_write(
    relay: &RwLock<Option<Arc<ConfiguredBackgroundRelay>>>,
) -> std::sync::RwLockWriteGuard<'_, Option<Arc<ConfiguredBackgroundRelay>>> {
    relay
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn operation() -> RelayOperationContext {
    RelayOperationContext::with_timeout(OPERATION_TIMEOUT)
}

async fn validate_journal_before_publish(
    journal: &NativeJournalAdapter,
    operation: &RelayOperationContext,
) -> Result<(), BackgroundRelayError> {
    match run_preflight_callback(operation, journal.list()).await? {
        Ok(_) => Ok(()),
        Err(_) => Err(BackgroundRelayError::JournalUnavailable),
    }
}

async fn load_or_create_journal_integrity_key(
    secrets: &NativeSecretAdapter,
    allow_create: bool,
    operation: &RelayOperationContext,
) -> Result<OpaqueRelaySecret, BackgroundRelayError> {
    let alias = RelaySecretAlias(JOURNAL_INTEGRITY_ALIAS.to_owned());
    if let Some(key) = run_preflight_callback(operation, secrets.read(&alias))
        .await?
        .map_err(|_| BackgroundRelayError::SecureStorageUnavailable)?
    {
        return validate_journal_integrity_key(key);
    }
    if !allow_create {
        return Err(BackgroundRelayError::SecureStorageUnavailable);
    }
    let mut bytes = vec![0_u8; JOURNAL_INTEGRITY_KEY_BYTES];
    OsRng.fill_bytes(&mut bytes);
    let key = OpaqueRelaySecret::new(bytes)?;
    match run_preflight_callback(operation, secrets.create_if_absent(&alias, key.clone()))
        .await?
        .map_err(|_| BackgroundRelayError::SecureStorageUnavailable)?
    {
        RelaySecretCreateOutcome::Created => Ok(key),
        RelaySecretCreateOutcome::AlreadyExists => {
            let existing = run_preflight_callback(operation, secrets.read(&alias))
                .await?
                .map_err(|_| BackgroundRelayError::SecureStorageUnavailable)?
                .ok_or(BackgroundRelayError::SecureStorageUnavailable)?;
            validate_journal_integrity_key(existing)
        }
    }
}

async fn run_preflight_callback<T>(
    operation: &RelayOperationContext,
    future: impl std::future::Future<Output = T>,
) -> Result<T, BackgroundRelayError> {
    let timeout = operation.remaining()?;
    tokio::time::timeout(timeout, future)
        .await
        .map_err(|_| BackgroundRelayError::DeadlineExceeded)
}

fn validate_journal_integrity_key(
    key: OpaqueRelaySecret,
) -> Result<OpaqueRelaySecret, BackgroundRelayError> {
    if key.expose_for_adapter().len() != JOURNAL_INTEGRITY_KEY_BYTES {
        return Err(BackgroundRelayError::SecureStorageUnavailable);
    }
    Ok(key)
}

fn unix_time_ms() -> Result<u64, BackgroundRelayError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| BackgroundRelayError::InvalidWake)?
        .as_millis();
    u64::try_from(millis).map_err(|_| BackgroundRelayError::InvalidWake)
}

fn parse_host_id(value: String) -> Result<RelayHostId, RelayError> {
    if value.is_empty() || value.len() > MAX_HOST_ID_BYTES || value.chars().any(char::is_control) {
        return Err(RelayError::InvalidResponse);
    }
    Ok(RelayHostId(value))
}

fn canonical_provider_token(
    provider: RelayPushProvider,
    token: Vec<u8>,
) -> Result<OpaqueRelaySecret, RelayError> {
    let token = Zeroizing::new(token);
    let canonical = match provider {
        RelayPushProvider::Apns => {
            if token.is_empty() || token.len() > 512 {
                return Err(RelayError::InvalidProviderRegistration);
            }
            hex::encode(token.as_slice()).into_bytes()
        }
        RelayPushProvider::Fcm => {
            let text =
                std::str::from_utf8(&token).map_err(|_| RelayError::InvalidProviderRegistration)?;
            if text.is_empty()
                || text
                    .bytes()
                    .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
            {
                return Err(RelayError::InvalidProviderRegistration);
            }
            token.to_vec()
        }
    };
    OpaqueRelaySecret::new(canonical).map_err(|_| RelayError::InvalidProviderRegistration)
}

impl From<AppRelayPushProvider> for RelayPushProvider {
    fn from(value: AppRelayPushProvider) -> Self {
        match value {
            AppRelayPushProvider::Apns => Self::Apns,
            AppRelayPushProvider::Fcm => Self::Fcm,
        }
    }
}

impl From<RelayPushProvider> for AppRelayPushProvider {
    fn from(value: RelayPushProvider) -> Self {
        match value {
            RelayPushProvider::Apns => Self::Apns,
            RelayPushProvider::Fcm => Self::Fcm,
        }
    }
}

impl From<AppRelayPushEnvironment> for RelayPushEnvironment {
    fn from(value: AppRelayPushEnvironment) -> Self {
        match value {
            AppRelayPushEnvironment::Sandbox => Self::Sandbox,
            AppRelayPushEnvironment::Production => Self::Production,
        }
    }
}

impl From<RelayPushEnvironment> for AppRelayPushEnvironment {
    fn from(value: RelayPushEnvironment) -> Self {
        match value {
            RelayPushEnvironment::Sandbox => Self::Sandbox,
            RelayPushEnvironment::Production => Self::Production,
        }
    }
}

impl From<AppRelayEventClass> for RelayEventClass {
    fn from(value: AppRelayEventClass) -> Self {
        match value {
            AppRelayEventClass::StateChanged => Self::StateChanged,
            AppRelayEventClass::ActivityChanged => Self::ActivityChanged,
            AppRelayEventClass::ConnectionChanged => Self::ConnectionChanged,
            AppRelayEventClass::SecurityChanged => Self::SecurityChanged,
        }
    }
}

impl From<crate::background_relay::PushFanoutReceipt> for AppRelayFanoutReceipt {
    fn from(value: crate::background_relay::PushFanoutReceipt) -> Self {
        Self {
            attempted: value.attempted,
            synchronized: value.synchronized,
            pending_retry: value.pending_retry,
            re_pair_required: value.re_pair_required,
            rejected: value.rejected,
        }
    }
}

impl From<RelayReconcileReceipt> for AppRelayReconcileReceipt {
    fn from(value: RelayReconcileReceipt) -> Self {
        Self {
            host_id: value.host_id.0,
            applied_through_cursor: value.applied_through_cursor,
            acknowledged_through_cursor: value.acknowledged_through_cursor,
            changed: value.changed,
        }
    }
}

impl From<RelayReconcileOutcome> for AppRelayReconcileOutcome {
    fn from(value: RelayReconcileOutcome) -> Self {
        match value {
            RelayReconcileOutcome::Applied(receipt) => Self::Applied {
                receipt: receipt.into(),
            },
            RelayReconcileOutcome::CleanupCompleted { host_id } => {
                Self::CleanupCompleted { host_id: host_id.0 }
            }
            RelayReconcileOutcome::Failed { host_id, error } => Self::Failed {
                host_id: host_id.0,
                failure: error.into(),
            },
        }
    }
}

impl From<RelayBindingState> for AppRelayBindingState {
    fn from(value: RelayBindingState) -> Self {
        match value {
            RelayBindingState::Preparing => Self::Preparing,
            RelayBindingState::RollbackPending => Self::RollbackPending,
            RelayBindingState::Staged => Self::Staged,
            RelayBindingState::Active => Self::Active,
            RelayBindingState::TombstonePending => Self::TombstonePending,
            RelayBindingState::CleanupPending => Self::CleanupPending,
            RelayBindingState::Tombstoned => Self::Tombstoned,
            RelayBindingState::NeedsRepair => Self::NeedsRepair,
        }
    }
}

impl From<RelayBindingStatus> for AppRelayBindingStatus {
    fn from(value: RelayBindingStatus) -> Self {
        Self {
            host_id: value.host_id.0,
            installation_id: value.installation_id.0,
            state: value.state.into(),
            highest_seen_cursor: value.highest_seen_cursor,
            applied_cursor: value.applied_cursor,
            pending_ack_cursor: value.pending_ack_cursor,
            provider_registration_count: value.provider_registration_count,
            has_pending_provider_sync: value.has_pending_provider_sync,
        }
    }
}

// ── Callback adapters ───────────────────────────────────────────────────

struct NativeJournalAdapter {
    backend: Arc<dyn AppRelayJournalBackend>,
    secrets: Arc<NativeSecretAdapter>,
    allow_loopback_http: bool,
    integrity_key: OpaqueRelaySecret,
}

impl NativeJournalAdapter {
    fn new(
        backend: Arc<dyn AppRelayJournalBackend>,
        secrets: Arc<NativeSecretAdapter>,
        allow_loopback_http: bool,
        integrity_key: OpaqueRelaySecret,
    ) -> Self {
        Self {
            backend,
            secrets,
            allow_loopback_http,
            integrity_key,
        }
    }

    async fn load_state(&self) -> Result<(Option<u64>, DecodedJournal), RelayJournalError> {
        let result = self.backend.load().await;
        match result {
            AppRelayJournalLoad::Missing => {
                if self.load_anchor().await?.is_some() {
                    return Err(RelayJournalError::Unavailable);
                }
                Ok((
                    None,
                    DecodedJournal {
                        entries: Vec::new(),
                        provider_tombstone_fences: Vec::new(),
                    },
                ))
            }
            AppRelayJournalLoad::Unavailable => Err(RelayJournalError::Unavailable),
            AppRelayJournalLoad::Loaded { snapshot } => {
                if snapshot.revision == 0 || snapshot.payload.len() > MAX_JOURNAL_BYTES {
                    return Err(RelayJournalError::Unavailable);
                }
                let state = decode_journal_state(
                    &snapshot.payload,
                    snapshot.revision,
                    self.allow_loopback_http,
                    &self.integrity_key,
                )?;
                self.verify_or_advance_anchor(&snapshot).await?;
                Ok((Some(snapshot.revision), state))
            }
        }
    }

    async fn store_state(
        &self,
        expected_outer_revision: Option<u64>,
        state: &DecodedJournal,
    ) -> Result<(), RelayJournalError> {
        let revision = expected_outer_revision
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(RelayJournalError::Unavailable)?;
        let payload = encode_journal_state(
            &state.entries,
            &state.provider_tombstone_fences,
            revision,
            &self.integrity_key,
        )?;
        let snapshot = AppRelayJournalSnapshot { revision, payload };
        match self
            .backend
            .compare_and_swap(expected_outer_revision, snapshot.clone())
            .await
        {
            AppRelayJournalWriteOutcome::Stored => self.advance_anchor(&snapshot).await,
            AppRelayJournalWriteOutcome::Conflict => Err(RelayJournalError::Conflict),
            AppRelayJournalWriteOutcome::Unavailable => Err(RelayJournalError::Unavailable),
        }
    }

    async fn load_anchor(&self) -> Result<Option<(u64, JournalRollbackAnchor)>, RelayJournalError> {
        let alias = RelaySecretAlias(JOURNAL_ROLLBACK_ANCHOR_ALIAS.to_owned());
        for _ in 0..4 {
            let before = self
                .secrets
                .revision(&alias)
                .await
                .map_err(|_| RelayJournalError::Unavailable)?;
            let value = self
                .secrets
                .read(&alias)
                .await
                .map_err(|_| RelayJournalError::Unavailable)?;
            let after = self
                .secrets
                .revision(&alias)
                .await
                .map_err(|_| RelayJournalError::Unavailable)?;
            if before != after {
                continue;
            }
            return match (before, value) {
                (RelaySecretRevision::Missing, None) => Ok(None),
                (RelaySecretRevision::Found(revision), Some(value)) if revision > 0 => {
                    Ok(Some((revision, decode_journal_anchor(&value)?)))
                }
                _ => Err(RelayJournalError::Unavailable),
            };
        }
        Err(RelayJournalError::Unavailable)
    }

    async fn verify_or_advance_anchor(
        &self,
        snapshot: &AppRelayJournalSnapshot,
    ) -> Result<(), RelayJournalError> {
        let expected = journal_anchor(snapshot);
        if let Some((_, anchor)) = self.load_anchor().await? {
            if anchor.journal_revision > expected.journal_revision {
                return Err(RelayJournalError::Unavailable);
            }
            if anchor.journal_revision == expected.journal_revision {
                return if anchor.payload_digest == expected.payload_digest {
                    Ok(())
                } else {
                    Err(RelayJournalError::Unavailable)
                };
            }
        }
        self.advance_anchor(snapshot).await
    }

    async fn advance_anchor(
        &self,
        snapshot: &AppRelayJournalSnapshot,
    ) -> Result<(), RelayJournalError> {
        let alias = RelaySecretAlias(JOURNAL_ROLLBACK_ANCHOR_ALIAS.to_owned());
        let expected = journal_anchor(snapshot);
        for _ in 0..4 {
            match self.load_anchor().await? {
                None => {
                    let value = encode_journal_anchor(&expected)?;
                    match self.secrets.create_if_absent(&alias, value).await {
                        Ok(RelaySecretCreateOutcome::Created)
                        | Ok(RelaySecretCreateOutcome::AlreadyExists)
                        | Err(_) => continue,
                    }
                }
                Some((storage_revision, current)) => {
                    if current.journal_revision > expected.journal_revision {
                        return Err(RelayJournalError::Conflict);
                    }
                    if current.journal_revision == expected.journal_revision {
                        return if current.payload_digest == expected.payload_digest {
                            Ok(())
                        } else {
                            Err(RelayJournalError::Unavailable)
                        };
                    }
                    let replacement_revision = storage_revision
                        .checked_add(1)
                        .ok_or(RelayJournalError::Unavailable)?;
                    let value = encode_journal_anchor(&expected)?;
                    match self
                        .secrets
                        .compare_and_swap(
                            &alias,
                            Some(storage_revision),
                            replacement_revision,
                            value,
                        )
                        .await
                    {
                        Ok(RelaySecretCasOutcome::Stored)
                        | Ok(RelaySecretCasOutcome::Conflict)
                        | Err(_) => continue,
                    }
                }
            }
        }
        // Ambiguous native writes are accepted only after an authoritative
        // reread proves the exact target anchor.
        match self.load_anchor().await? {
            Some((_, current)) if current == expected => Ok(()),
            _ => Err(RelayJournalError::Unavailable),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct JournalRollbackAnchor {
    journal_revision: u64,
    payload_digest: [u8; JOURNAL_ANCHOR_DIGEST_BYTES],
}

fn journal_anchor(snapshot: &AppRelayJournalSnapshot) -> JournalRollbackAnchor {
    let mut digest = Sha256::new();
    digest.update(snapshot.revision.to_be_bytes());
    digest.update(&snapshot.payload);
    JournalRollbackAnchor {
        journal_revision: snapshot.revision,
        payload_digest: digest.finalize().into(),
    }
}

fn encode_journal_anchor(
    anchor: &JournalRollbackAnchor,
) -> Result<OpaqueRelaySecret, RelayJournalError> {
    let mut bytes = Vec::with_capacity(
        JOURNAL_ANCHOR_MAGIC.len() + size_of::<u64>() + JOURNAL_ANCHOR_DIGEST_BYTES,
    );
    bytes.extend_from_slice(JOURNAL_ANCHOR_MAGIC);
    bytes.extend_from_slice(&anchor.journal_revision.to_be_bytes());
    bytes.extend_from_slice(&anchor.payload_digest);
    OpaqueRelaySecret::new(bytes).map_err(|_| RelayJournalError::Unavailable)
}

fn decode_journal_anchor(
    value: &OpaqueRelaySecret,
) -> Result<JournalRollbackAnchor, RelayJournalError> {
    let bytes = value.expose_for_adapter();
    let revision_start = JOURNAL_ANCHOR_MAGIC.len();
    let digest_start = revision_start + size_of::<u64>();
    if bytes.len() != digest_start + JOURNAL_ANCHOR_DIGEST_BYTES
        || !bytes.starts_with(JOURNAL_ANCHOR_MAGIC)
    {
        return Err(RelayJournalError::Unavailable);
    }
    let journal_revision = u64::from_be_bytes(
        bytes[revision_start..digest_start]
            .try_into()
            .map_err(|_| RelayJournalError::Unavailable)?,
    );
    if journal_revision == 0 {
        return Err(RelayJournalError::Unavailable);
    }
    Ok(JournalRollbackAnchor {
        journal_revision,
        payload_digest: bytes[digest_start..]
            .try_into()
            .map_err(|_| RelayJournalError::Unavailable)?,
    })
}

#[async_trait]
impl RelayBindingJournalPort for NativeJournalAdapter {
    async fn list(&self) -> Result<Vec<RelayBindingEntry>, RelayJournalError> {
        self.load_state().await.map(|(_, state)| state.entries)
    }

    async fn load_by_host(
        &self,
        host_id: &RelayHostId,
    ) -> Result<Option<RelayBindingEntry>, RelayJournalError> {
        Ok(self
            .load_state()
            .await?
            .1
            .entries
            .into_iter()
            .find(|entry| &entry.host_id == host_id))
    }

    async fn load_by_installation(
        &self,
        installation_id: &RelayInstallationId,
    ) -> Result<Option<RelayBindingEntry>, RelayJournalError> {
        Ok(self
            .load_state()
            .await?
            .1
            .entries
            .into_iter()
            .find(|entry| &entry.installation_id == installation_id))
    }

    async fn provider_tombstone_fences(
        &self,
    ) -> Result<Vec<RelayProviderTombstoneFence>, RelayJournalError> {
        self.load_state()
            .await
            .map(|(_, state)| state.provider_tombstone_fences)
    }

    async fn advance_provider_tombstone_fence(
        &self,
        tombstone: &PushTokenTombstone,
    ) -> Result<(), RelayJournalError> {
        for _ in 0..4 {
            let (outer_revision, mut state) = self.load_state().await?;
            let before = state.provider_tombstone_fences.clone();
            if let Some(fence) = state.provider_tombstone_fences.iter_mut().find(|fence| {
                fence.provider == tombstone.provider && fence.environment == tombstone.environment
            }) {
                fence.through_local_generation = fence
                    .through_local_generation
                    .max(tombstone.through_local_generation);
            } else {
                state
                    .provider_tombstone_fences
                    .push(RelayProviderTombstoneFence {
                        provider: tombstone.provider,
                        environment: tombstone.environment,
                        through_local_generation: tombstone.through_local_generation,
                    });
            }
            if state.provider_tombstone_fences == before {
                return Ok(());
            }
            match self.store_state(outer_revision, &state).await {
                Ok(()) => return Ok(()),
                Err(RelayJournalError::Conflict) => continue,
                Err(error) => return Err(error),
            }
        }
        Err(RelayJournalError::Conflict)
    }

    async fn compare_and_swap(
        &self,
        host_id: &RelayHostId,
        expected_revision: Option<u64>,
        replacement: RelayBindingEntry,
    ) -> Result<(), RelayJournalError> {
        if &replacement.host_id != host_id {
            return Err(RelayJournalError::Conflict);
        }
        let (outer_revision, mut state) = self.load_state().await?;
        let current_index = state
            .entries
            .iter()
            .position(|entry| &entry.host_id == host_id);
        let actual_revision = current_index.map(|index| state.entries[index].revision);
        if actual_revision != expected_revision {
            return Err(RelayJournalError::Conflict);
        }
        if state.entries.iter().enumerate().any(|(index, entry)| {
            Some(index) != current_index && entry.installation_id == replacement.installation_id
        }) {
            return Err(RelayJournalError::Conflict);
        }
        if let Some(index) = current_index {
            state.entries[index] = replacement;
        } else {
            state.entries.push(replacement);
        }
        state
            .entries
            .sort_by(|left, right| left.host_id.0.cmp(&right.host_id.0));
        self.store_state(outer_revision, &state).await
    }
}

struct NativeSecretAdapter {
    backend: Arc<dyn AppRelaySecretBackend>,
}

impl NativeSecretAdapter {
    fn new(backend: Arc<dyn AppRelaySecretBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl OpaqueRelaySecretPort for NativeSecretAdapter {
    async fn read(
        &self,
        alias: &RelaySecretAlias,
    ) -> Result<Option<OpaqueRelaySecret>, RelaySecretStoreError> {
        let alias = alias.0.clone();
        match self.backend.read(alias).await {
            Err(AppRelaySecretReadError::Missing) => Ok(None),
            Err(AppRelaySecretReadError::Unavailable) => Err(RelaySecretStoreError::Unavailable),
            Ok(value) => OpaqueRelaySecret::new(value.into_bytes())
                .map(Some)
                .map_err(|_| RelaySecretStoreError::Unavailable),
        }
    }

    async fn compare_and_tombstone(
        &self,
        alias: &RelaySecretAlias,
        expected_revision: Option<u64>,
        replacement_revision: u64,
    ) -> Result<RelaySecretCasOutcome, RelaySecretStoreError> {
        if replacement_revision == 0
            || expected_revision.is_some_and(|expected| replacement_revision <= expected)
        {
            return Err(RelaySecretStoreError::Unavailable);
        }
        match self
            .backend
            .compare_and_tombstone(alias.0.clone(), expected_revision, replacement_revision)
            .await
        {
            AppRelaySecretCasOutcome::Stored => Ok(RelaySecretCasOutcome::Stored),
            AppRelaySecretCasOutcome::Conflict => Ok(RelaySecretCasOutcome::Conflict),
            AppRelaySecretCasOutcome::Unavailable => Err(RelaySecretStoreError::Unavailable),
        }
    }

    async fn create_if_absent(
        &self,
        alias: &RelaySecretAlias,
        secret: OpaqueRelaySecret,
    ) -> Result<RelaySecretCreateOutcome, RelaySecretStoreError> {
        match self
            .backend
            .create_if_absent(
                alias.0.clone(),
                AppRelaySecretValue::new(secret.expose_for_adapter().to_vec()),
            )
            .await
        {
            AppRelaySecretCreateOutcome::Created => Ok(RelaySecretCreateOutcome::Created),
            AppRelaySecretCreateOutcome::AlreadyExists => {
                Ok(RelaySecretCreateOutcome::AlreadyExists)
            }
            AppRelaySecretCreateOutcome::Unavailable => Err(RelaySecretStoreError::Unavailable),
        }
    }

    async fn revision(
        &self,
        alias: &RelaySecretAlias,
    ) -> Result<RelaySecretRevision, RelaySecretStoreError> {
        match self.backend.revision(alias.0.clone()).await {
            AppRelaySecretRevision::Missing => Ok(RelaySecretRevision::Missing),
            AppRelaySecretRevision::Found { revision } if revision > 0 => {
                Ok(RelaySecretRevision::Found(revision))
            }
            AppRelaySecretRevision::Found { .. } | AppRelaySecretRevision::Unavailable => {
                Err(RelaySecretStoreError::Unavailable)
            }
        }
    }

    async fn compare_and_swap(
        &self,
        alias: &RelaySecretAlias,
        expected_revision: Option<u64>,
        replacement_revision: u64,
        secret: OpaqueRelaySecret,
    ) -> Result<RelaySecretCasOutcome, RelaySecretStoreError> {
        if replacement_revision == 0
            || expected_revision.is_some_and(|expected| replacement_revision <= expected)
        {
            return Err(RelaySecretStoreError::Unavailable);
        }
        match self
            .backend
            .compare_and_swap(
                alias.0.clone(),
                expected_revision,
                replacement_revision,
                AppRelaySecretValue::new(secret.expose_for_adapter().to_vec()),
            )
            .await
        {
            AppRelaySecretCasOutcome::Stored => Ok(RelaySecretCasOutcome::Stored),
            AppRelaySecretCasOutcome::Conflict => Ok(RelaySecretCasOutcome::Conflict),
            AppRelaySecretCasOutcome::Unavailable => Err(RelaySecretStoreError::Unavailable),
        }
    }
}

struct NativeRepairAdapter {
    backend: Arc<dyn AppRelayRepairBackend>,
}

impl NativeRepairAdapter {
    fn new(backend: Arc<dyn AppRelayRepairBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl RelayAuthoritativeRepairPort for NativeRepairAdapter {
    async fn repair(
        &self,
        host_id: &RelayHostId,
        generation: u64,
        mode: RelayRepairMode,
        operation: RelayOperationContext,
    ) -> Result<RelayRepairReceipt, RelayRepairError> {
        if generation == 0 || operation.cancellation.is_cancelled() {
            return Err(RelayRepairError::Cancelled);
        }
        let remaining = operation
            .remaining()
            .map_err(|_| RelayRepairError::DeadlineExceeded)?;
        let remaining_ms =
            u64::try_from(remaining.as_millis()).map_err(|_| RelayRepairError::DeadlineExceeded)?;
        let deadline_unix_ms = repair_unix_time_ms()?
            .checked_add(remaining_ms)
            .ok_or(RelayRepairError::DeadlineExceeded)?;
        let request = AppRelayRepairRequest {
            host_id: host_id.0.clone(),
            mode: AppRelayRepairMode::from(mode),
            generation,
            deadline_unix_ms,
        };
        let result = tokio::select! {
            biased;
            _ = operation.cancellation.cancelled() => {
                return Err(RelayRepairError::Cancelled);
            }
            result = tokio::time::timeout(remaining, self.backend.repair(request)) => {
                result.map_err(|_| RelayRepairError::DeadlineExceeded)?
            }
        };
        match result {
            AppRelayRepairResult::Applied { through_cursor } => Ok(RelayRepairReceipt {
                applied_through_cursor: through_cursor,
                authoritative: true,
            }),
            AppRelayRepairResult::Unavailable => Err(RelayRepairError::Unavailable),
            AppRelayRepairResult::RePairRequired => Err(RelayRepairError::RePairRequired),
            AppRelayRepairResult::Cancelled => Err(RelayRepairError::Cancelled),
        }
    }
}

fn repair_unix_time_ms() -> Result<u64, RelayRepairError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| RelayRepairError::DeadlineExceeded)?
        .as_millis();
    u64::try_from(millis).map_err(|_| RelayRepairError::DeadlineExceeded)
}

impl From<RelayRepairMode> for AppRelayRepairMode {
    fn from(value: RelayRepairMode) -> Self {
        match value {
            RelayRepairMode::Incremental {
                after_cursor,
                through_cursor,
            } => Self::Incremental {
                after_cursor,
                through_cursor,
            },
            RelayRepairMode::Snapshot {
                revision,
                through_cursor,
            } => Self::Snapshot {
                revision,
                through_cursor,
            },
            RelayRepairMode::Full { through_cursor } => Self::Full { through_cursor },
        }
    }
}

// ── Opaque non-secret journal codec ─────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedJournal {
    schema_version: u16,
    entries: Vec<PersistedBinding>,
    #[serde(default)]
    provider_tombstone_fences: Vec<PersistedProviderTombstoneFence>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedBinding {
    revision: u64,
    staging_generation: u64,
    staging_command_id: String,
    read_capability_revision: Option<u64>,
    manage_capability_revision: Option<u64>,
    #[serde(default)]
    repair_generation: u64,
    host_id: String,
    origin: String,
    installation_id: String,
    read_capability_alias: String,
    manage_capability_alias: String,
    state: PersistedBindingState,
    registrations: Vec<PersistedRegistration>,
    #[serde(default)]
    provider_tombstone_fences: Vec<PersistedProviderTombstoneFence>,
    retired_token_aliases: Vec<String>,
    wake: PersistedWakeLedger,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PersistedBindingState {
    Preparing,
    RollbackPending,
    Staged,
    Active,
    TombstonePending,
    CleanupPending,
    Tombstoned,
    NeedsRepair,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedRegistration {
    provider: PersistedPushProvider,
    environment: PersistedPushEnvironment,
    local_generation: u64,
    token_alias: String,
    #[serde(default)]
    token_revision: Option<u64>,
    relay_registration_id: Option<String>,
    relay_generation: Option<u64>,
    disposition: PersistedRegistrationDisposition,
    pending_sync: bool,
    previous: Option<PersistedPreviousRegistration>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedPreviousRegistration {
    token_alias: String,
    #[serde(default)]
    token_revision: Option<u64>,
    relay_registration_id: Option<String>,
    relay_generation: Option<u64>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedProviderTombstoneFence {
    provider: PersistedPushProvider,
    environment: PersistedPushEnvironment,
    through_local_generation: u64,
}

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
enum PersistedPushProvider {
    Apns,
    Fcm,
}

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
enum PersistedPushEnvironment {
    Sandbox,
    Production,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PersistedRegistrationDisposition {
    Active,
    Tombstone,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedWakeLedger {
    highest_seen_cursor: u64,
    applied_cursor: u64,
    pending_ack_cursor: Option<u64>,
    remote_ack_ahead_cursor: Option<u64>,
    recently_seen: Vec<PersistedSeenWake>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedSeenWake {
    cursor: u64,
    event_id: String,
}

#[derive(Clone)]
struct DecodedJournal {
    entries: Vec<RelayBindingEntry>,
    provider_tombstone_fences: Vec<RelayProviderTombstoneFence>,
}

#[cfg(test)]
fn encode_journal(
    entries: &[RelayBindingEntry],
    outer_revision: u64,
    integrity_key: &OpaqueRelaySecret,
) -> Result<Vec<u8>, RelayJournalError> {
    encode_journal_state(entries, &[], outer_revision, integrity_key)
}

fn encode_journal_state(
    entries: &[RelayBindingEntry],
    provider_tombstone_fences: &[RelayProviderTombstoneFence],
    outer_revision: u64,
    integrity_key: &OpaqueRelaySecret,
) -> Result<Vec<u8>, RelayJournalError> {
    if outer_revision == 0
        || entries.len() > MAX_BINDINGS
        || !valid_provider_tombstone_fences(provider_tombstone_fences)
        || entries.iter().any(|entry| {
            entry.retired_token_aliases.len() > MAX_RETIRED_TOKEN_ALIASES
                || !valid_provider_tombstone_fences(&entry.provider_tombstone_fences)
        })
    {
        return Err(RelayJournalError::Unavailable);
    }
    let journal = PersistedJournal {
        schema_version: JOURNAL_SCHEMA_VERSION,
        entries: entries.iter().map(PersistedBinding::from).collect(),
        provider_tombstone_fences: provider_tombstone_fences
            .iter()
            .map(PersistedProviderTombstoneFence::from)
            .collect(),
    };
    let body = serde_json::to_vec(&journal).map_err(|_| RelayJournalError::Unavailable)?;
    let mut mac = Hmac::<Sha256>::new_from_slice(integrity_key.expose_for_adapter())
        .map_err(|_| RelayJournalError::Unavailable)?;
    mac.update(JOURNAL_AUTH_MAGIC);
    mac.update(&outer_revision.to_be_bytes());
    mac.update(&body);
    let tag = mac.finalize().into_bytes();
    let payload_len = JOURNAL_AUTH_MAGIC
        .len()
        .checked_add(JOURNAL_AUTH_TAG_BYTES)
        .and_then(|length| length.checked_add(body.len()))
        .ok_or(RelayJournalError::Unavailable)?;
    if payload_len > MAX_JOURNAL_BYTES {
        return Err(RelayJournalError::Unavailable);
    }
    let mut payload = Vec::with_capacity(payload_len);
    payload.extend_from_slice(JOURNAL_AUTH_MAGIC);
    payload.extend_from_slice(&tag);
    payload.extend_from_slice(&body);
    Ok(payload)
}

fn valid_provider_tombstone_fences(fences: &[RelayProviderTombstoneFence]) -> bool {
    if fences.len() > MAX_REGISTRATIONS_PER_BINDING {
        return false;
    }
    let mut kinds = HashSet::new();
    fences.iter().all(|fence| {
        fence.through_local_generation > 0
            && !(fence.provider == RelayPushProvider::Fcm
                && fence.environment != RelayPushEnvironment::Production)
            && kinds.insert((fence.provider, fence.environment))
    })
}

#[cfg(test)]
fn decode_journal(
    payload: &[u8],
    outer_revision: u64,
    allow_loopback_http: bool,
    integrity_key: &OpaqueRelaySecret,
) -> Result<Vec<RelayBindingEntry>, RelayJournalError> {
    decode_journal_state(payload, outer_revision, allow_loopback_http, integrity_key)
        .map(|journal| journal.entries)
}

fn decode_journal_state(
    payload: &[u8],
    outer_revision: u64,
    allow_loopback_http: bool,
    integrity_key: &OpaqueRelaySecret,
) -> Result<DecodedJournal, RelayJournalError> {
    let header_len = JOURNAL_AUTH_MAGIC
        .len()
        .checked_add(JOURNAL_AUTH_TAG_BYTES)
        .ok_or(RelayJournalError::Unavailable)?;
    if outer_revision == 0
        || payload.len() > MAX_JOURNAL_BYTES
        || payload.len() <= header_len
        || !payload.starts_with(JOURNAL_AUTH_MAGIC)
    {
        return Err(RelayJournalError::Unavailable);
    }
    let tag_start = JOURNAL_AUTH_MAGIC.len();
    let body_start = header_len;
    let mut mac = Hmac::<Sha256>::new_from_slice(integrity_key.expose_for_adapter())
        .map_err(|_| RelayJournalError::Unavailable)?;
    mac.update(JOURNAL_AUTH_MAGIC);
    mac.update(&outer_revision.to_be_bytes());
    mac.update(&payload[body_start..]);
    mac.verify_slice(&payload[tag_start..body_start])
        .map_err(|_| RelayJournalError::Unavailable)?;
    let journal: PersistedJournal = serde_json::from_slice(&payload[body_start..])
        .map_err(|_| RelayJournalError::Unavailable)?;
    if journal.schema_version != JOURNAL_SCHEMA_VERSION
        || journal.entries.len() > MAX_BINDINGS
        || journal.provider_tombstone_fences.len() > MAX_REGISTRATIONS_PER_BINDING
    {
        return Err(RelayJournalError::Unavailable);
    }
    let provider_tombstone_fences = journal
        .provider_tombstone_fences
        .into_iter()
        .map(PersistedProviderTombstoneFence::into_relay)
        .collect::<Result<Vec<_>, _>>()?;
    if !valid_provider_tombstone_fences(&provider_tombstone_fences) {
        return Err(RelayJournalError::Unavailable);
    }
    let mut host_ids = HashSet::new();
    let mut installation_ids = HashSet::new();
    let mut aliases = HashSet::new();
    let mut entries = Vec::with_capacity(journal.entries.len());
    for entry in journal.entries {
        let entry = entry.into_relay(allow_loopback_http)?;
        if !host_ids.insert(entry.host_id.clone())
            || !installation_ids.insert(entry.installation_id.clone())
        {
            return Err(RelayJournalError::Unavailable);
        }
        for alias in
            std::iter::once(&entry.read_capability_alias)
                .chain(std::iter::once(&entry.manage_capability_alias))
                .chain(entry.registrations.iter().map(|item| &item.token_alias))
                .chain(entry.registrations.iter().filter_map(|item| {
                    item.previous.as_ref().map(|previous| &previous.token_alias)
                }))
                .chain(entry.retired_token_aliases.iter())
        {
            if !aliases.insert(alias.clone()) {
                return Err(RelayJournalError::Unavailable);
            }
        }
        entries.push(entry);
    }
    entries.sort_by(|left, right| left.host_id.0.cmp(&right.host_id.0));
    Ok(DecodedJournal {
        entries,
        provider_tombstone_fences,
    })
}

impl From<&RelayBindingEntry> for PersistedBinding {
    fn from(value: &RelayBindingEntry) -> Self {
        Self {
            revision: value.revision,
            staging_generation: value.staging_generation,
            staging_command_id: value.staging_command_id.0.clone(),
            read_capability_revision: value.read_capability_revision,
            manage_capability_revision: value.manage_capability_revision,
            repair_generation: value.repair_generation,
            host_id: value.host_id.0.clone(),
            origin: value.origin.as_url().as_str().to_owned(),
            installation_id: value.installation_id.0.clone(),
            read_capability_alias: value.read_capability_alias.0.clone(),
            manage_capability_alias: value.manage_capability_alias.0.clone(),
            state: value.state.into(),
            registrations: value
                .registrations
                .iter()
                .map(PersistedRegistration::from)
                .collect(),
            provider_tombstone_fences: value
                .provider_tombstone_fences
                .iter()
                .map(PersistedProviderTombstoneFence::from)
                .collect(),
            retired_token_aliases: value
                .retired_token_aliases
                .iter()
                .map(|alias| alias.0.clone())
                .collect(),
            wake: PersistedWakeLedger::from(&value.wake),
        }
    }
}

impl PersistedBinding {
    fn into_relay(self, allow_loopback_http: bool) -> Result<RelayBindingEntry, RelayJournalError> {
        if self.revision == 0
            || self.staging_generation == 0
            || self.registrations.len() > MAX_REGISTRATIONS_PER_BINDING
            || self.provider_tombstone_fences.len() > MAX_REGISTRATIONS_PER_BINDING
            || self.retired_token_aliases.len() > MAX_RETIRED_TOKEN_ALIASES
        {
            return Err(RelayJournalError::Unavailable);
        }
        let host_id = parse_host_id(self.host_id).map_err(|_| RelayJournalError::Unavailable)?;
        let origin =
            crate::background_relay::ValidatedRelayOrigin::parse(&self.origin, allow_loopback_http)
                .map_err(|_| RelayJournalError::Unavailable)?;
        let installation_id = RelayInstallationId::parse(self.installation_id)
            .map_err(|_| RelayJournalError::Unavailable)?;
        let staging_command_id = RelayEnrollmentCommandId::parse(self.staging_command_id)
            .map_err(|_| RelayJournalError::Unavailable)?;
        let read_capability_alias = parse_capability_alias(self.read_capability_alias)?;
        let manage_capability_alias = parse_capability_alias(self.manage_capability_alias)?;
        let registrations = self
            .registrations
            .into_iter()
            .map(PersistedRegistration::into_relay)
            .collect::<Result<Vec<_>, _>>()?;
        let mut registration_kinds = HashSet::new();
        if registrations.iter().any(|registration| {
            !registration_kinds.insert((registration.provider, registration.environment))
        }) {
            return Err(RelayJournalError::Unavailable);
        }
        let mut provider_tombstone_fences = self
            .provider_tombstone_fences
            .into_iter()
            .map(PersistedProviderTombstoneFence::into_relay)
            .collect::<Result<Vec<_>, _>>()?;
        // Legacy journals stored the tombstone high-watermark only on the
        // pending registration. Preserve it during authenticated decode so an
        // upgrade/restart cannot resurrect the same native observation.
        for registration in &registrations {
            if registration.disposition == RelayRegistrationDisposition::Tombstone {
                if let Some(existing) = provider_tombstone_fences.iter_mut().find(|fence| {
                    fence.provider == registration.provider
                        && fence.environment == registration.environment
                }) {
                    existing.through_local_generation = existing
                        .through_local_generation
                        .max(registration.local_generation);
                } else {
                    provider_tombstone_fences.push(RelayProviderTombstoneFence {
                        provider: registration.provider,
                        environment: registration.environment,
                        through_local_generation: registration.local_generation,
                    });
                }
            }
        }
        let mut fence_kinds = HashSet::new();
        if provider_tombstone_fences.len() > MAX_REGISTRATIONS_PER_BINDING
            || provider_tombstone_fences
                .iter()
                .any(|fence| !fence_kinds.insert((fence.provider, fence.environment)))
        {
            return Err(RelayJournalError::Unavailable);
        }
        let retired_token_aliases = self
            .retired_token_aliases
            .into_iter()
            .map(parse_token_alias)
            .collect::<Result<Vec<_>, _>>()?;
        let wake = self.wake.into_relay()?;
        let state: RelayBindingState = self.state.into();
        if (matches!(
            state,
            RelayBindingState::Preparing | RelayBindingState::RollbackPending
        ) && (self.read_capability_revision.is_some()
            || self.manage_capability_revision.is_some()
            || !registrations.is_empty()
            || !retired_token_aliases.is_empty()
            || wake != RelayWakeLedger::default()))
            || (matches!(state, RelayBindingState::Staged | RelayBindingState::Active)
                && (self.read_capability_revision.is_none()
                    || self.manage_capability_revision.is_none()))
            || (state == RelayBindingState::Staged
                && (!registrations.is_empty()
                    || !retired_token_aliases.is_empty()
                    || wake != RelayWakeLedger::default()))
            || (state == RelayBindingState::Tombstoned
                && (!registrations.is_empty() || !retired_token_aliases.is_empty()))
        {
            return Err(RelayJournalError::Unavailable);
        }
        Ok(RelayBindingEntry {
            revision: self.revision,
            staging_generation: self.staging_generation,
            staging_command_id,
            read_capability_revision: self.read_capability_revision,
            manage_capability_revision: self.manage_capability_revision,
            repair_generation: self.repair_generation,
            host_id,
            origin,
            installation_id,
            read_capability_alias,
            manage_capability_alias,
            state,
            registrations,
            provider_tombstone_fences,
            retired_token_aliases,
            wake,
        })
    }
}

impl From<RelayBindingState> for PersistedBindingState {
    fn from(value: RelayBindingState) -> Self {
        match value {
            RelayBindingState::Preparing => Self::Preparing,
            RelayBindingState::RollbackPending => Self::RollbackPending,
            RelayBindingState::Staged => Self::Staged,
            RelayBindingState::Active => Self::Active,
            RelayBindingState::TombstonePending => Self::TombstonePending,
            RelayBindingState::CleanupPending => Self::CleanupPending,
            RelayBindingState::Tombstoned => Self::Tombstoned,
            RelayBindingState::NeedsRepair => Self::NeedsRepair,
        }
    }
}

impl From<PersistedBindingState> for RelayBindingState {
    fn from(value: PersistedBindingState) -> Self {
        match value {
            PersistedBindingState::Preparing => Self::Preparing,
            PersistedBindingState::RollbackPending => Self::RollbackPending,
            PersistedBindingState::Staged => Self::Staged,
            PersistedBindingState::Active => Self::Active,
            PersistedBindingState::TombstonePending => Self::TombstonePending,
            PersistedBindingState::CleanupPending => Self::CleanupPending,
            PersistedBindingState::Tombstoned => Self::Tombstoned,
            PersistedBindingState::NeedsRepair => Self::NeedsRepair,
        }
    }
}

impl From<&RelayProviderRegistration> for PersistedRegistration {
    fn from(value: &RelayProviderRegistration) -> Self {
        Self {
            provider: value.provider.into(),
            environment: value.environment.into(),
            local_generation: value.local_generation,
            token_alias: value.token_alias.0.clone(),
            token_revision: value.token_revision,
            relay_registration_id: value.relay_registration_id.as_ref().map(|id| id.0.clone()),
            relay_generation: value.relay_generation,
            disposition: value.disposition.into(),
            pending_sync: value.pending_sync,
            previous: value
                .previous
                .as_ref()
                .map(|previous| PersistedPreviousRegistration {
                    token_alias: previous.token_alias.0.clone(),
                    token_revision: previous.token_revision,
                    relay_registration_id: previous
                        .relay_registration_id
                        .as_ref()
                        .map(|id| id.0.clone()),
                    relay_generation: previous.relay_generation,
                }),
        }
    }
}

impl PersistedRegistration {
    fn into_relay(self) -> Result<RelayProviderRegistration, RelayJournalError> {
        if self.local_generation == 0
            || self.token_revision.is_some_and(|revision| revision == 0)
            || (self.provider == PersistedPushProvider::Fcm
                && self.environment != PersistedPushEnvironment::Production)
            || self
                .relay_generation
                .is_some_and(|generation| generation == 0)
            || self.relay_registration_id.is_some() != self.relay_generation.is_some()
            || self.previous.as_ref().is_some_and(|previous| {
                previous
                    .token_revision
                    .is_some_and(|revision| revision == 0)
                    || previous.relay_registration_id.is_some()
                        != previous.relay_generation.is_some()
                    || previous
                        .relay_generation
                        .is_some_and(|generation| generation == 0)
            })
            || (matches!(
                self.disposition,
                PersistedRegistrationDisposition::Tombstone
            ) && !self.pending_sync)
            || (!self.pending_sync
                && (!matches!(self.disposition, PersistedRegistrationDisposition::Active)
                    || self.relay_registration_id.is_none()))
        {
            return Err(RelayJournalError::Unavailable);
        }
        Ok(RelayProviderRegistration {
            provider: self.provider.into(),
            environment: self.environment.into(),
            local_generation: self.local_generation,
            token_alias: parse_token_alias(self.token_alias)?,
            token_revision: self.token_revision,
            relay_registration_id: self
                .relay_registration_id
                .map(RelayRegistrationId::parse)
                .transpose()
                .map_err(|_| RelayJournalError::Unavailable)?,
            relay_generation: self.relay_generation,
            disposition: self.disposition.into(),
            pending_sync: self.pending_sync,
            previous: self
                .previous
                .map(PersistedPreviousRegistration::into_relay)
                .transpose()?,
        })
    }
}

impl PersistedPreviousRegistration {
    fn into_relay(self) -> Result<RelayPreviousProviderRegistration, RelayJournalError> {
        Ok(RelayPreviousProviderRegistration {
            token_alias: parse_token_alias(self.token_alias)?,
            token_revision: self.token_revision,
            relay_registration_id: self
                .relay_registration_id
                .map(RelayRegistrationId::parse)
                .transpose()
                .map_err(|_| RelayJournalError::Unavailable)?,
            relay_generation: self.relay_generation,
        })
    }
}

impl From<&RelayProviderTombstoneFence> for PersistedProviderTombstoneFence {
    fn from(value: &RelayProviderTombstoneFence) -> Self {
        Self {
            provider: value.provider.into(),
            environment: value.environment.into(),
            through_local_generation: value.through_local_generation,
        }
    }
}

impl PersistedProviderTombstoneFence {
    fn into_relay(self) -> Result<RelayProviderTombstoneFence, RelayJournalError> {
        if self.through_local_generation == 0
            || (self.provider == PersistedPushProvider::Fcm
                && self.environment != PersistedPushEnvironment::Production)
        {
            return Err(RelayJournalError::Unavailable);
        }
        Ok(RelayProviderTombstoneFence {
            provider: self.provider.into(),
            environment: self.environment.into(),
            through_local_generation: self.through_local_generation,
        })
    }
}

impl From<RelayPushProvider> for PersistedPushProvider {
    fn from(value: RelayPushProvider) -> Self {
        match value {
            RelayPushProvider::Apns => Self::Apns,
            RelayPushProvider::Fcm => Self::Fcm,
        }
    }
}

impl From<PersistedPushProvider> for RelayPushProvider {
    fn from(value: PersistedPushProvider) -> Self {
        match value {
            PersistedPushProvider::Apns => Self::Apns,
            PersistedPushProvider::Fcm => Self::Fcm,
        }
    }
}

impl From<RelayPushEnvironment> for PersistedPushEnvironment {
    fn from(value: RelayPushEnvironment) -> Self {
        match value {
            RelayPushEnvironment::Sandbox => Self::Sandbox,
            RelayPushEnvironment::Production => Self::Production,
        }
    }
}

impl From<PersistedPushEnvironment> for RelayPushEnvironment {
    fn from(value: PersistedPushEnvironment) -> Self {
        match value {
            PersistedPushEnvironment::Sandbox => Self::Sandbox,
            PersistedPushEnvironment::Production => Self::Production,
        }
    }
}

impl From<RelayRegistrationDisposition> for PersistedRegistrationDisposition {
    fn from(value: RelayRegistrationDisposition) -> Self {
        match value {
            RelayRegistrationDisposition::Active => Self::Active,
            RelayRegistrationDisposition::Tombstone => Self::Tombstone,
        }
    }
}

impl From<PersistedRegistrationDisposition> for RelayRegistrationDisposition {
    fn from(value: PersistedRegistrationDisposition) -> Self {
        match value {
            PersistedRegistrationDisposition::Active => Self::Active,
            PersistedRegistrationDisposition::Tombstone => Self::Tombstone,
        }
    }
}

impl From<&RelayWakeLedger> for PersistedWakeLedger {
    fn from(value: &RelayWakeLedger) -> Self {
        Self {
            highest_seen_cursor: value.highest_seen_cursor,
            applied_cursor: value.applied_cursor,
            pending_ack_cursor: value.pending_ack_cursor,
            remote_ack_ahead_cursor: value.remote_ack_ahead_cursor,
            recently_seen: value
                .recently_seen
                .iter()
                .map(|wake| PersistedSeenWake {
                    cursor: wake.cursor,
                    event_id: wake.event_id.0.clone(),
                })
                .collect(),
        }
    }
}

impl PersistedWakeLedger {
    fn into_relay(self) -> Result<RelayWakeLedger, RelayJournalError> {
        if self.recently_seen.len() > MAX_RECENT_WAKES
            || self.applied_cursor > self.highest_seen_cursor
            || self
                .pending_ack_cursor
                .is_some_and(|cursor| cursor == 0 || cursor != self.applied_cursor)
            || (self.pending_ack_cursor.is_some() && self.remote_ack_ahead_cursor.is_some())
            || self.remote_ack_ahead_cursor.is_some_and(|cursor| {
                cursor <= self.applied_cursor || cursor > self.highest_seen_cursor
            })
        {
            return Err(RelayJournalError::Unavailable);
        }
        let mut cursors = HashSet::new();
        let mut event_ids = HashSet::new();
        let recently_seen = self
            .recently_seen
            .into_iter()
            .map(|wake| {
                if wake.cursor == 0
                    || wake.cursor > self.highest_seen_cursor
                    || !cursors.insert(wake.cursor)
                {
                    return Err(RelayJournalError::Unavailable);
                }
                let event_id = RelayEventId::parse(wake.event_id)
                    .map_err(|_| RelayJournalError::Unavailable)?;
                if !event_ids.insert(event_id.clone()) {
                    return Err(RelayJournalError::Unavailable);
                }
                Ok(SeenWake {
                    cursor: wake.cursor,
                    event_id,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        if recently_seen
            .windows(2)
            .any(|pair| pair[0].cursor <= pair[1].cursor)
        {
            return Err(RelayJournalError::Unavailable);
        }
        Ok(RelayWakeLedger {
            highest_seen_cursor: self.highest_seen_cursor,
            applied_cursor: self.applied_cursor,
            pending_ack_cursor: self.pending_ack_cursor,
            remote_ack_ahead_cursor: self.remote_ack_ahead_cursor,
            recently_seen,
        })
    }
}

fn parse_capability_alias(value: String) -> Result<RelaySecretAlias, RelayJournalError> {
    parse_alias(value, "relay_capability_")
}

fn parse_token_alias(value: String) -> Result<RelaySecretAlias, RelayJournalError> {
    parse_alias(value, "relay_token_")
}

fn parse_alias(value: String, prefix: &str) -> Result<RelaySecretAlias, RelayJournalError> {
    let suffix = value
        .strip_prefix(prefix)
        .ok_or(RelayJournalError::Unavailable)?;
    if suffix.len() != 32
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(RelayJournalError::Unavailable);
    }
    Ok(RelaySecretAlias(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MemoryJournalBackend {
        value: Mutex<Option<AppRelayJournalSnapshot>>,
        fail_writes: Mutex<bool>,
    }

    #[async_trait]
    impl AppRelayJournalBackend for MemoryJournalBackend {
        async fn load(&self) -> AppRelayJournalLoad {
            self.value
                .lock()
                .unwrap()
                .clone()
                .map_or(AppRelayJournalLoad::Missing, |snapshot| {
                    AppRelayJournalLoad::Loaded { snapshot }
                })
        }

        async fn compare_and_swap(
            &self,
            expected_revision: Option<u64>,
            replacement: AppRelayJournalSnapshot,
        ) -> AppRelayJournalWriteOutcome {
            if *self.fail_writes.lock().unwrap() {
                return AppRelayJournalWriteOutcome::Unavailable;
            }
            let mut value = self.value.lock().unwrap();
            if value.as_ref().map(|snapshot| snapshot.revision) != expected_revision {
                return AppRelayJournalWriteOutcome::Conflict;
            }
            *value = Some(replacement);
            AppRelayJournalWriteOutcome::Stored
        }
    }

    #[derive(Default)]
    struct MemorySecretBackend {
        values: Mutex<HashMap<String, Vec<u8>>>,
        revisions: Mutex<HashMap<String, u64>>,
        unavailable: Mutex<bool>,
        delay: Mutex<Option<Duration>>,
        create_unavailable_after_apply: Mutex<u32>,
        cas_unavailable_after_apply: Mutex<u32>,
    }

    #[async_trait]
    impl AppRelaySecretBackend for MemorySecretBackend {
        async fn read(
            &self,
            alias: String,
        ) -> Result<AppRelaySecretValue, AppRelaySecretReadError> {
            let delay = *self.delay.lock().unwrap();
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            if *self.unavailable.lock().unwrap() {
                return Err(AppRelaySecretReadError::Unavailable);
            }
            self.values
                .lock()
                .unwrap()
                .get(&alias)
                .cloned()
                .map(AppRelaySecretValue::new)
                .ok_or(AppRelaySecretReadError::Missing)
        }

        async fn write(
            &self,
            alias: String,
            value: AppRelaySecretValue,
        ) -> AppRelaySecretWriteOutcome {
            let delay = *self.delay.lock().unwrap();
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            if *self.unavailable.lock().unwrap() {
                return AppRelaySecretWriteOutcome::Unavailable;
            }
            self.values
                .lock()
                .unwrap()
                .insert(alias.clone(), value.expose_for_adapter().to_vec());
            let mut revisions = self.revisions.lock().unwrap();
            let revision = revisions
                .get(&alias)
                .copied()
                .unwrap_or(0)
                .saturating_add(1);
            revisions.insert(alias, revision);
            AppRelaySecretWriteOutcome::Applied
        }

        async fn create_if_absent(
            &self,
            alias: String,
            value: AppRelaySecretValue,
        ) -> AppRelaySecretCreateOutcome {
            let delay = *self.delay.lock().unwrap();
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            if *self.unavailable.lock().unwrap() {
                return AppRelaySecretCreateOutcome::Unavailable;
            }
            let mut values = self.values.lock().unwrap();
            if values.contains_key(&alias) {
                AppRelaySecretCreateOutcome::AlreadyExists
            } else {
                values.insert(alias.clone(), value.expose_for_adapter().to_vec());
                self.revisions.lock().unwrap().insert(alias, 1);
                drop(values);
                let mut failures = self.create_unavailable_after_apply.lock().unwrap();
                if *failures > 0 {
                    *failures -= 1;
                    AppRelaySecretCreateOutcome::Unavailable
                } else {
                    AppRelaySecretCreateOutcome::Created
                }
            }
        }

        async fn revision(&self, alias: String) -> AppRelaySecretRevision {
            self.revisions
                .lock()
                .unwrap()
                .get(&alias)
                .copied()
                .map_or(AppRelaySecretRevision::Missing, |revision| {
                    AppRelaySecretRevision::Found { revision }
                })
        }

        async fn compare_and_swap(
            &self,
            alias: String,
            expected_revision: Option<u64>,
            replacement_revision: u64,
            value: AppRelaySecretValue,
        ) -> AppRelaySecretCasOutcome {
            let mut revisions = self.revisions.lock().unwrap();
            if revisions.get(&alias).copied() != expected_revision {
                return AppRelaySecretCasOutcome::Conflict;
            }
            self.values
                .lock()
                .unwrap()
                .insert(alias.clone(), value.expose_for_adapter().to_vec());
            revisions.insert(alias, replacement_revision);
            drop(revisions);
            let mut failures = self.cas_unavailable_after_apply.lock().unwrap();
            if *failures > 0 {
                *failures -= 1;
                AppRelaySecretCasOutcome::Unavailable
            } else {
                AppRelaySecretCasOutcome::Stored
            }
        }

        async fn compare_and_tombstone(
            &self,
            alias: String,
            expected_revision: Option<u64>,
            replacement_revision: u64,
        ) -> AppRelaySecretCasOutcome {
            let mut revisions = self.revisions.lock().unwrap();
            if revisions.get(&alias).copied() != expected_revision {
                return AppRelaySecretCasOutcome::Conflict;
            }
            self.values.lock().unwrap().remove(&alias);
            revisions.insert(alias, replacement_revision);
            AppRelaySecretCasOutcome::Stored
        }

        async fn delete(&self, alias: String) -> AppRelaySecretWriteOutcome {
            if *self.unavailable.lock().unwrap() {
                return AppRelaySecretWriteOutcome::Unavailable;
            }
            self.values.lock().unwrap().remove(&alias);
            self.revisions.lock().unwrap().remove(&alias);
            AppRelaySecretWriteOutcome::Applied
        }
    }

    struct AppliedRepair;

    #[async_trait]
    impl AppRelayRepairBackend for AppliedRepair {
        async fn repair(&self, request: AppRelayRepairRequest) -> AppRelayRepairResult {
            let through_cursor = match request.mode {
                AppRelayRepairMode::Incremental { through_cursor, .. }
                | AppRelayRepairMode::Snapshot { through_cursor, .. }
                | AppRelayRepairMode::Full { through_cursor } => through_cursor,
            };
            AppRelayRepairResult::Applied { through_cursor }
        }
    }

    #[derive(Default)]
    struct FencedLateRepair {
        current_fence: Arc<Mutex<HashMap<String, u64>>>,
        applied_cursor: Arc<Mutex<HashMap<String, u64>>>,
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl AppRelayRepairBackend for FencedLateRepair {
        async fn repair(&self, request: AppRelayRepairRequest) -> AppRelayRepairResult {
            use std::sync::atomic::Ordering;

            {
                let mut fences = self.current_fence.lock().unwrap();
                if fences
                    .get(&request.host_id)
                    .is_some_and(|current| request.generation <= *current)
                {
                    return AppRelayRepairResult::Cancelled;
                }
                fences.insert(request.host_id.clone(), request.generation);
            }
            let delay = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Duration::from_millis(50)
            } else {
                Duration::ZERO
            };
            let cursor = match request.mode {
                AppRelayRepairMode::Incremental { through_cursor, .. }
                | AppRelayRepairMode::Snapshot { through_cursor, .. }
                | AppRelayRepairMode::Full { through_cursor } => through_cursor,
            };
            let current_fence = self.current_fence.clone();
            let applied_cursor = self.applied_cursor.clone();
            let (sent, received) = tokio::sync::oneshot::channel();
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                let is_current = current_fence
                    .lock()
                    .unwrap()
                    .get(&request.host_id)
                    .is_some_and(|generation| *generation == request.generation);
                let before_deadline =
                    repair_unix_time_ms().is_ok_and(|now_ms| now_ms < request.deadline_unix_ms);
                let result = if is_current && before_deadline {
                    applied_cursor
                        .lock()
                        .unwrap()
                        .insert(request.host_id, cursor);
                    AppRelayRepairResult::Applied {
                        through_cursor: cursor,
                    }
                } else {
                    AppRelayRepairResult::Cancelled
                };
                let _ = sent.send(result);
            });
            received.await.unwrap_or(AppRelayRepairResult::Cancelled)
        }
    }

    struct DelayedJournalBackend;

    #[async_trait]
    impl AppRelayJournalBackend for DelayedJournalBackend {
        async fn load(&self) -> AppRelayJournalLoad {
            tokio::time::sleep(Duration::from_millis(50)).await;
            AppRelayJournalLoad::Missing
        }

        async fn compare_and_swap(
            &self,
            _expected_revision: Option<u64>,
            _replacement: AppRelayJournalSnapshot,
        ) -> AppRelayJournalWriteOutcome {
            AppRelayJournalWriteOutcome::Stored
        }
    }

    struct RacingSecretBackend {
        values: Arc<Mutex<HashMap<String, Vec<u8>>>>,
        revisions: Arc<Mutex<HashMap<String, u64>>>,
        create_calls: std::sync::atomic::AtomicUsize,
    }

    impl Default for RacingSecretBackend {
        fn default() -> Self {
            Self {
                values: Arc::new(Mutex::new(HashMap::new())),
                revisions: Arc::new(Mutex::new(HashMap::new())),
                create_calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl AppRelaySecretBackend for RacingSecretBackend {
        async fn read(
            &self,
            alias: String,
        ) -> Result<AppRelaySecretValue, AppRelaySecretReadError> {
            self.values
                .lock()
                .unwrap()
                .get(&alias)
                .cloned()
                .map(AppRelaySecretValue::new)
                .ok_or(AppRelaySecretReadError::Missing)
        }

        async fn write(
            &self,
            alias: String,
            value: AppRelaySecretValue,
        ) -> AppRelaySecretWriteOutcome {
            self.values
                .lock()
                .unwrap()
                .insert(alias.clone(), value.expose_for_adapter().to_vec());
            let mut revisions = self.revisions.lock().unwrap();
            let revision = revisions
                .get(&alias)
                .copied()
                .unwrap_or(0)
                .saturating_add(1);
            revisions.insert(alias, revision);
            AppRelaySecretWriteOutcome::Applied
        }

        async fn create_if_absent(
            &self,
            alias: String,
            value: AppRelaySecretValue,
        ) -> AppRelaySecretCreateOutcome {
            use std::sync::atomic::Ordering;

            let delay = if self.create_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Duration::from_millis(50)
            } else {
                Duration::ZERO
            };
            let values = self.values.clone();
            let revisions = self.revisions.clone();
            let bytes = value.expose_for_adapter().to_vec();
            let (sent, received) = tokio::sync::oneshot::channel();
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                let mut values = values.lock().unwrap();
                let outcome = if values.contains_key(&alias) {
                    AppRelaySecretCreateOutcome::AlreadyExists
                } else {
                    values.insert(alias.clone(), bytes);
                    revisions.lock().unwrap().insert(alias, 1);
                    AppRelaySecretCreateOutcome::Created
                };
                let _ = sent.send(outcome);
            });
            received
                .await
                .unwrap_or(AppRelaySecretCreateOutcome::Unavailable)
        }

        async fn revision(&self, alias: String) -> AppRelaySecretRevision {
            self.revisions
                .lock()
                .unwrap()
                .get(&alias)
                .copied()
                .map_or(AppRelaySecretRevision::Missing, |revision| {
                    AppRelaySecretRevision::Found { revision }
                })
        }

        async fn compare_and_swap(
            &self,
            alias: String,
            expected_revision: Option<u64>,
            replacement_revision: u64,
            value: AppRelaySecretValue,
        ) -> AppRelaySecretCasOutcome {
            let mut revisions = self.revisions.lock().unwrap();
            if revisions.get(&alias).copied() != expected_revision {
                return AppRelaySecretCasOutcome::Conflict;
            }
            self.values
                .lock()
                .unwrap()
                .insert(alias.clone(), value.expose_for_adapter().to_vec());
            revisions.insert(alias, replacement_revision);
            AppRelaySecretCasOutcome::Stored
        }

        async fn compare_and_tombstone(
            &self,
            alias: String,
            expected_revision: Option<u64>,
            replacement_revision: u64,
        ) -> AppRelaySecretCasOutcome {
            let mut revisions = self.revisions.lock().unwrap();
            if revisions.get(&alias).copied() != expected_revision {
                return AppRelaySecretCasOutcome::Conflict;
            }
            self.values.lock().unwrap().remove(&alias);
            revisions.insert(alias, replacement_revision);
            AppRelaySecretCasOutcome::Stored
        }

        async fn delete(&self, alias: String) -> AppRelaySecretWriteOutcome {
            self.values.lock().unwrap().remove(&alias);
            self.revisions.lock().unwrap().remove(&alias);
            AppRelaySecretWriteOutcome::Applied
        }
    }

    fn binding() -> RelayBindingEntry {
        RelayBindingEntry {
            revision: 1,
            staging_generation: 1,
            staging_command_id: RelayEnrollmentCommandId::parse("command_identifier_0001").unwrap(),
            read_capability_revision: Some(1),
            manage_capability_revision: Some(1),
            repair_generation: 0,
            host_id: RelayHostId("host-a".to_owned()),
            origin: crate::background_relay::ValidatedRelayOrigin::parse(
                "https://relay.example",
                false,
            )
            .unwrap(),
            installation_id: RelayInstallationId::parse("installation_identifier_0001").unwrap(),
            read_capability_alias: RelaySecretAlias(
                "relay_capability_00000000000000000000000000000001".to_owned(),
            ),
            manage_capability_alias: RelaySecretAlias(
                "relay_capability_00000000000000000000000000000002".to_owned(),
            ),
            state: RelayBindingState::Active,
            registrations: vec![RelayProviderRegistration {
                provider: RelayPushProvider::Apns,
                environment: RelayPushEnvironment::Production,
                local_generation: 1,
                token_alias: RelaySecretAlias(
                    "relay_token_00000000000000000000000000000003".to_owned(),
                ),
                token_revision: Some(1),
                relay_registration_id: Some(
                    RelayRegistrationId::parse("registration_identifier_0001").unwrap(),
                ),
                relay_generation: Some(1),
                disposition: RelayRegistrationDisposition::Active,
                pending_sync: false,
                previous: None,
            }],
            provider_tombstone_fences: Vec::new(),
            retired_token_aliases: Vec::new(),
            wake: RelayWakeLedger {
                highest_seen_cursor: 2,
                applied_cursor: 1,
                pending_ack_cursor: Some(1),
                remote_ack_ahead_cursor: None,
                recently_seen: vec![SeenWake {
                    cursor: 2,
                    event_id: RelayEventId::parse("event_identifier_0002").unwrap(),
                }],
            },
        }
    }

    fn journal_integrity_key() -> OpaqueRelaySecret {
        OpaqueRelaySecret::new(vec![0x5a; JOURNAL_INTEGRITY_KEY_BYTES]).unwrap()
    }

    fn native_test_secrets() -> Arc<NativeSecretAdapter> {
        native_test_secret_pair().1
    }

    fn native_test_secret_pair() -> (Arc<MemorySecretBackend>, Arc<NativeSecretAdapter>) {
        let backend = Arc::new(MemorySecretBackend::default());
        let adapter = Arc::new(NativeSecretAdapter::new(backend.clone()));
        (backend, adapter)
    }

    #[tokio::test]
    async fn opaque_journal_round_trip_preserves_state_without_secret_bytes() {
        let backend = Arc::new(MemoryJournalBackend::default());
        let adapter = NativeJournalAdapter::new(
            backend.clone(),
            native_test_secrets(),
            false,
            journal_integrity_key(),
        );
        adapter
            .compare_and_swap(&RelayHostId("host-a".to_owned()), None, binding())
            .await
            .expect("journal insert");
        let loaded = adapter.list().await.expect("journal load");
        assert_eq!(loaded, vec![binding()]);
        let stored = backend.value.lock().unwrap().clone().unwrap();
        let text = String::from_utf8_lossy(&stored.payload);
        assert!(!text.contains("read-capability-secret"));
        assert!(!text.contains("provider-token-secret"));
        assert!(text.contains("relay_capability_"));
    }

    #[tokio::test]
    async fn rollback_anchor_rejects_an_older_valid_journal_across_clients() {
        let backend = Arc::new(MemoryJournalBackend::default());
        let (_, secrets) = native_test_secret_pair();
        let adapter = NativeJournalAdapter::new(
            backend.clone(),
            secrets.clone(),
            false,
            journal_integrity_key(),
        );
        adapter
            .compare_and_swap(&RelayHostId("host-a".to_owned()), None, binding())
            .await
            .expect("first authenticated journal snapshot");
        let older_valid = backend.value.lock().unwrap().clone().unwrap();
        let mut replacement = binding();
        replacement.revision = 2;
        replacement.wake.highest_seen_cursor = 3;
        adapter
            .compare_and_swap(
                &RelayHostId("host-a".to_owned()),
                Some(binding().revision),
                replacement,
            )
            .await
            .expect("advance journal and independent anchor");
        let newer_valid = backend.value.lock().unwrap().clone().unwrap();
        assert!(newer_valid.revision > older_valid.revision);

        *backend.value.lock().unwrap() = Some(older_valid);
        let restarted_client =
            NativeJournalAdapter::new(backend.clone(), secrets, false, journal_integrity_key());
        assert_eq!(
            restarted_client.list().await,
            Err(RelayJournalError::Unavailable)
        );
    }

    #[tokio::test]
    async fn rollback_anchor_heals_journal_ahead_and_rejects_equal_revision_digest_mismatch() {
        let backend = Arc::new(MemoryJournalBackend::default());
        let (secret_backend, secrets) = native_test_secret_pair();
        let adapter = NativeJournalAdapter::new(
            backend.clone(),
            secrets.clone(),
            false,
            journal_integrity_key(),
        );
        *secret_backend
            .create_unavailable_after_apply
            .lock()
            .unwrap() = 1;
        adapter
            .compare_and_swap(&RelayHostId("host-a".to_owned()), None, binding())
            .await
            .expect("ambiguous anchor creation is accepted only after reread");
        let (_, anchor_one) = adapter
            .load_anchor()
            .await
            .expect("load anchor")
            .expect("anchor exists");
        assert_eq!(anchor_one.journal_revision, 1);

        let mut journal_two_binding = binding();
        journal_two_binding.revision = 2;
        journal_two_binding.wake.highest_seen_cursor = 3;
        let journal_two = AppRelayJournalSnapshot {
            revision: 2,
            payload: encode_journal_state(
                &[journal_two_binding.clone()],
                &[],
                2,
                &journal_integrity_key(),
            )
            .expect("valid journal-ahead payload"),
        };
        *backend.value.lock().unwrap() = Some(journal_two.clone());
        *secret_backend.cas_unavailable_after_apply.lock().unwrap() = 1;
        assert_eq!(
            adapter.list().await.expect("journal-ahead anchor heals"),
            vec![journal_two_binding.clone()]
        );
        let (_, anchor_two) = adapter
            .load_anchor()
            .await
            .expect("load healed anchor")
            .expect("healed anchor exists");
        assert_eq!(anchor_two, journal_anchor(&journal_two));
        let other_client =
            NativeJournalAdapter::new(backend.clone(), secrets, false, journal_integrity_key());
        assert_eq!(
            other_client
                .list()
                .await
                .expect("second client sees healed state"),
            vec![journal_two_binding.clone()]
        );

        let mut alternate = journal_two_binding;
        alternate.wake.highest_seen_cursor = 4;
        let same_revision_different_payload = AppRelayJournalSnapshot {
            revision: 2,
            payload: encode_journal_state(&[alternate], &[], 2, &journal_integrity_key())
                .expect("alternate payload is independently authenticated"),
        };
        assert_ne!(
            journal_anchor(&same_revision_different_payload),
            journal_anchor(&journal_two)
        );
        *backend.value.lock().unwrap() = Some(same_revision_different_payload);
        assert_eq!(
            other_client.list().await,
            Err(RelayJournalError::Unavailable)
        );
    }

    #[tokio::test]
    async fn journal_cas_conflict_and_corruption_fail_closed() {
        let backend = Arc::new(MemoryJournalBackend::default());
        let adapter = NativeJournalAdapter::new(
            backend.clone(),
            native_test_secrets(),
            false,
            journal_integrity_key(),
        );
        adapter
            .compare_and_swap(&RelayHostId("host-a".to_owned()), None, binding())
            .await
            .unwrap();
        assert_eq!(
            adapter
                .compare_and_swap(&RelayHostId("host-a".to_owned()), None, binding())
                .await,
            Err(RelayJournalError::Conflict)
        );
        let valid_payload =
            encode_journal(&[binding()], u64::MAX, &journal_integrity_key()).unwrap();
        *backend.value.lock().unwrap() = Some(AppRelayJournalSnapshot {
            revision: u64::MAX,
            payload: valid_payload,
        });
        assert_eq!(
            adapter
                .compare_and_swap(
                    &RelayHostId("host-a".to_owned()),
                    Some(binding().revision),
                    binding(),
                )
                .await,
            Err(RelayJournalError::Unavailable)
        );
        *backend.value.lock().unwrap() = Some(AppRelayJournalSnapshot {
            revision: 2,
            payload: br#"{"schema_version":99,"entries":[]}"#.to_vec(),
        });
        assert_eq!(adapter.list().await, Err(RelayJournalError::Unavailable));
    }

    #[tokio::test]
    async fn authenticated_journal_rejects_origin_and_alias_tampering_before_use() {
        let backend = Arc::new(MemoryJournalBackend::default());
        let adapter = NativeJournalAdapter::new(
            backend.clone(),
            native_test_secrets(),
            false,
            journal_integrity_key(),
        );
        adapter
            .compare_and_swap(&RelayHostId("host-a".to_owned()), None, binding())
            .await
            .unwrap();
        let mut snapshot = backend.value.lock().unwrap().clone().unwrap();
        let original = snapshot.clone();
        snapshot.revision += 1;
        *backend.value.lock().unwrap() = Some(snapshot);
        assert_eq!(adapter.list().await, Err(RelayJournalError::Unavailable));

        let mut snapshot = original;
        let origin = b"https://relay.example";
        let offset = snapshot
            .payload
            .windows(origin.len())
            .position(|window| window == origin)
            .expect("encoded origin");
        snapshot.payload[offset] = b'X';
        *backend.value.lock().unwrap() = Some(snapshot);
        assert_eq!(adapter.list().await, Err(RelayJournalError::Unavailable));
    }

    #[test]
    fn authenticated_decoder_rejects_impossible_binding_and_registration_states() {
        let key = journal_integrity_key();
        let mut impossible_binding = binding();
        impossible_binding.state = RelayBindingState::Staged;
        let payload = encode_journal(&[impossible_binding], 1, &key).unwrap();
        assert_eq!(
            decode_journal(&payload, 1, false, &key),
            Err(RelayJournalError::Unavailable)
        );

        let mut impossible_registration = binding();
        impossible_registration.registrations[0].disposition =
            RelayRegistrationDisposition::Tombstone;
        impossible_registration.registrations[0].pending_sync = false;
        let payload = encode_journal(&[impossible_registration], 1, &key).unwrap();
        assert_eq!(
            decode_journal(&payload, 1, false, &key),
            Err(RelayJournalError::Unavailable)
        );

        let mut stale_pending = binding();
        stale_pending.wake.applied_cursor = 2;
        stale_pending.wake.pending_ack_cursor = Some(1);
        let payload = encode_journal(&[stale_pending], 1, &key).unwrap();
        assert_eq!(
            decode_journal(&payload, 1, false, &key),
            Err(RelayJournalError::Unavailable)
        );

        let mut contradictory_ack = binding();
        contradictory_ack.wake.remote_ack_ahead_cursor = Some(2);
        let payload = encode_journal(&[contradictory_ack], 1, &key).unwrap();
        assert_eq!(
            decode_journal(&payload, 1, false, &key),
            Err(RelayJournalError::Unavailable)
        );

        let mut remote_beyond_seen = binding();
        remote_beyond_seen.wake.pending_ack_cursor = None;
        remote_beyond_seen.wake.remote_ack_ahead_cursor = Some(3);
        let payload = encode_journal(&[remote_beyond_seen], 1, &key).unwrap();
        assert_eq!(
            decode_journal(&payload, 1, false, &key),
            Err(RelayJournalError::Unavailable)
        );
    }

    #[test]
    fn authenticated_encoder_rejects_unbounded_retired_token_aliases() {
        let mut impossible = binding();
        impossible.retired_token_aliases = (0..=MAX_RETIRED_TOKEN_ALIASES)
            .map(|index| RelaySecretAlias(format!("relay_token_{index:032x}")))
            .collect();
        assert_eq!(
            encode_journal(&[impossible], 1, &journal_integrity_key()),
            Err(RelayJournalError::Unavailable)
        );
    }

    #[tokio::test]
    async fn secret_callback_unavailability_is_typed_and_values_are_not_status() {
        let backend = Arc::new(MemorySecretBackend::default());
        let adapter = NativeSecretAdapter::new(backend.clone());
        let alias =
            RelaySecretAlias("relay_capability_00000000000000000000000000000001".to_owned());
        assert_eq!(
            adapter
                .compare_and_swap(
                    &alias,
                    None,
                    1,
                    OpaqueRelaySecret::new(vec![7; 32]).unwrap(),
                )
                .await,
            Ok(RelaySecretCasOutcome::Stored)
        );
        assert_eq!(
            adapter
                .read(&alias)
                .await
                .unwrap()
                .unwrap()
                .expose_for_adapter(),
            &[7; 32]
        );
        *backend.unavailable.lock().unwrap() = true;
        assert!(matches!(
            adapter.read(&alias).await,
            Err(RelaySecretStoreError::Unavailable)
        ));

        let status = AppRelayBindingStatus::from(RelayBindingStatus {
            host_id: RelayHostId("host-a".to_owned()),
            installation_id: RelayInstallationId::parse("installation_identifier_0001").unwrap(),
            state: RelayBindingState::Active,
            highest_seen_cursor: 2,
            applied_cursor: 1,
            pending_ack_cursor: Some(1),
            provider_registration_count: 1,
            has_pending_provider_sync: false,
        });
        assert!(!format!("{status:?}").contains("07070707"));
    }

    #[tokio::test]
    async fn repair_callback_must_report_the_exact_authoritative_cursor() {
        let adapter = NativeRepairAdapter::new(Arc::new(AppliedRepair));
        let receipt = adapter
            .repair(
                &RelayHostId("host-a".to_owned()),
                1,
                RelayRepairMode::Full { through_cursor: 42 },
                operation(),
            )
            .await
            .unwrap();
        assert!(receipt.authoritative);
        assert_eq!(receipt.applied_through_cursor, 42);
    }

    #[tokio::test]
    async fn late_repair_callback_cannot_overwrite_newer_fenced_generation() {
        let backend = Arc::new(FencedLateRepair::default());
        let adapter = NativeRepairAdapter::new(backend.clone());
        let host = RelayHostId("host-a".to_owned());

        assert_eq!(
            adapter
                .repair(
                    &host,
                    1,
                    RelayRepairMode::Full { through_cursor: 1 },
                    RelayOperationContext::with_timeout(Duration::from_millis(10)),
                )
                .await,
            Err(RelayRepairError::DeadlineExceeded)
        );
        let newer = adapter
            .repair(
                &host,
                2,
                RelayRepairMode::Full { through_cursor: 2 },
                operation(),
            )
            .await
            .expect("newer fenced repair applies");
        assert_eq!(newer.applied_through_cursor, 2);
        tokio::time::sleep(Duration::from_millis(60)).await;

        assert_eq!(
            backend.applied_cursor.lock().unwrap().get(&host.0),
            Some(&2)
        );
    }

    #[tokio::test]
    async fn configuration_preflight_is_bounded_and_not_published_per_handle() {
        let delayed = NativeJournalAdapter::new(
            Arc::new(DelayedJournalBackend),
            native_test_secrets(),
            false,
            journal_integrity_key(),
        );
        assert!(matches!(
            validate_journal_before_publish(
                &delayed,
                &RelayOperationContext::with_timeout(Duration::from_millis(5)),
            )
            .await,
            Err(BackgroundRelayError::DeadlineExceeded)
        ));

        let inner = crate::MobileClient::new();
        let first = AppClient {
            inner: inner.clone(),
            rt: crate::ffi::shared::shared_runtime(),
        };
        let second = AppClient {
            inner,
            rt: crate::ffi::shared::shared_runtime(),
        };
        assert!(!first.background_relay_status().await.unwrap().configured);
        first
            .configure_background_relay(
                Box::new(MemoryJournalBackend::default()),
                Box::new(MemorySecretBackend::default()),
                Box::new(AppliedRepair),
                false,
            )
            .await
            .unwrap();
        assert!(second.background_relay_status().await.unwrap().configured);
        second.clear_background_relay().await;
        assert!(!first.background_relay_status().await.unwrap().configured);
    }

    #[tokio::test]
    async fn configuration_preflight_callbacks_share_one_absolute_deadline() {
        let secrets = MemorySecretBackend::default();
        *secrets.delay.lock().unwrap() = Some(Duration::from_millis(50));
        let operation = RelayOperationContext::with_timeout(Duration::from_millis(75));

        let result = build_background_relay_configuration(
            Box::new(DelayedJournalBackend),
            Box::new(secrets),
            Box::new(AppliedRepair),
            false,
            &operation,
        )
        .await;

        assert!(matches!(
            result,
            Err(BackgroundRelayError::DeadlineExceeded)
        ));
    }

    #[test]
    fn provider_tokens_are_canonicalized_before_secure_custody() {
        let apns = canonical_provider_token(RelayPushProvider::Apns, vec![0xab; 32]).unwrap();
        assert_eq!(
            std::str::from_utf8(apns.expose_for_adapter()).unwrap(),
            "abababababababababababababababababababababababababababababababab"
        );
        let fcm_text = "fcm-token_abcdefghijklmnopqrstuvwxyz0123456789";
        let fcm =
            canonical_provider_token(RelayPushProvider::Fcm, fcm_text.as_bytes().to_vec()).unwrap();
        assert_eq!(fcm.expose_for_adapter(), fcm_text.as_bytes());
        assert!(canonical_provider_token(RelayPushProvider::Fcm, vec![0xff; 64]).is_err());
    }

    #[tokio::test]
    async fn integrity_key_is_never_recreated_over_an_existing_journal() {
        let backend = Arc::new(MemorySecretBackend::default());
        let adapter = NativeSecretAdapter::new(backend.clone());
        assert!(matches!(
            load_or_create_journal_integrity_key(&adapter, false, &operation()).await,
            Err(BackgroundRelayError::SecureStorageUnavailable)
        ));
        assert!(backend.values.lock().unwrap().is_empty());

        let created = load_or_create_journal_integrity_key(&adapter, true, &operation())
            .await
            .unwrap();
        assert_eq!(
            created.expose_for_adapter().len(),
            JOURNAL_INTEGRITY_KEY_BYTES
        );
        let loaded = load_or_create_journal_integrity_key(&adapter, false, &operation())
            .await
            .unwrap();
        assert_eq!(loaded.expose_for_adapter(), created.expose_for_adapter());
    }

    #[tokio::test]
    async fn timed_out_integrity_key_create_cannot_overwrite_retry_key() {
        let backend = Arc::new(RacingSecretBackend::default());
        let adapter = NativeSecretAdapter::new(backend.clone());

        assert!(matches!(
            load_or_create_journal_integrity_key(
                &adapter,
                true,
                &RelayOperationContext::with_timeout(Duration::from_millis(10)),
            )
            .await,
            Err(BackgroundRelayError::DeadlineExceeded)
        ));
        let retry_key = load_or_create_journal_integrity_key(&adapter, true, &operation())
            .await
            .expect("retry atomically creates the stable key");
        tokio::time::sleep(Duration::from_millis(60)).await;

        let stored = backend
            .values
            .lock()
            .unwrap()
            .get(JOURNAL_INTEGRITY_ALIAS)
            .cloned()
            .expect("one integrity key remains");
        assert_eq!(stored, retry_key.expose_for_adapter());
    }

    #[test]
    fn persisted_aliases_accept_only_canonical_lowercase_hex() {
        assert!(
            parse_capability_alias("relay_capability_0000000000000000000000000000000a".to_owned())
                .is_ok()
        );
        assert!(
            parse_capability_alias("relay_capability_0000000000000000000000000000000A".to_owned())
                .is_err()
        );
    }
}
