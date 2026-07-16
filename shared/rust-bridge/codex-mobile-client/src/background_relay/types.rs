use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use url::Url;
use zeroize::Zeroize;

pub(crate) const RELAY_SCHEMA_VERSION: u16 = 1;
// A relay snapshot may contain 2 MiB of decoded ciphertext. Leave room for
// base64 expansion and a small, bounded JSON envelope while still preventing
// unbounded response buffering in native HTTP adapters.
pub(crate) const DEFAULT_MAX_RESPONSE_BYTES: usize = 3 * 1_024 * 1_024;
pub(crate) const DEFAULT_PAGE_LIMIT: u32 = 100;
pub(crate) const DEFAULT_MAX_FETCH_PAGES: usize = 8;
pub(crate) const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
pub(crate) const MAX_RETIRED_TOKEN_ALIASES: usize = 64;
pub(crate) const MAX_PROVIDER_TOMBSTONE_FENCES: usize = 3;

const MIN_OPAQUE_ID_BYTES: usize = 16;
const MAX_OPAQUE_ID_BYTES: usize = 128;
const MIN_CAPABILITY_BYTES: usize = 32;
const MAX_CAPABILITY_BYTES: usize = 256;
const MAX_PROVIDER_TOKEN_BYTES: usize = 4_096;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct RelayHostId(pub(crate) String);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct RelayInstallationId(pub(crate) String);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct RelayRegistrationId(pub(crate) String);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct RelayEventId(pub(crate) String);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct RelaySecretAlias(pub(crate) String);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct RelayEnrollmentCommandId(pub(crate) String);

impl RelayInstallationId {
    pub(crate) fn parse(value: impl Into<String>) -> Result<Self, RelayError> {
        parse_opaque_id(value.into()).map(Self)
    }
}

impl RelayRegistrationId {
    pub(crate) fn parse(value: impl Into<String>) -> Result<Self, RelayError> {
        parse_opaque_id(value.into()).map(Self)
    }
}

impl RelayEventId {
    pub(crate) fn parse(value: impl Into<String>) -> Result<Self, RelayError> {
        parse_opaque_id(value.into()).map(Self)
    }
}

impl RelayEnrollmentCommandId {
    pub(crate) fn parse(value: impl Into<String>) -> Result<Self, RelayError> {
        parse_opaque_id(value.into()).map(Self)
    }
}

fn parse_opaque_id(value: String) -> Result<String, RelayError> {
    if !(MIN_OPAQUE_ID_BYTES..=MAX_OPAQUE_ID_BYTES).contains(&value.len())
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(RelayError::InvalidResponse);
    }
    Ok(value)
}

#[derive(Clone)]
pub(crate) struct OpaqueRelaySecret(Arc<RelaySecretBytes>);

struct RelaySecretBytes(Vec<u8>);

impl Drop for RelaySecretBytes {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl OpaqueRelaySecret {
    pub(crate) fn new(bytes: Vec<u8>) -> Result<Self, RelayError> {
        // Wrap first so even rejected inbound FFI values are zeroized when the
        // validation path returns early.
        let bytes = RelaySecretBytes(bytes);
        if !(MIN_CAPABILITY_BYTES..=MAX_PROVIDER_TOKEN_BYTES).contains(&bytes.0.len()) {
            return Err(RelayError::InvalidSecret);
        }
        Ok(Self(Arc::new(bytes)))
    }

    pub(crate) fn expose_for_adapter(&self) -> &[u8] {
        &self.0.0
    }

    pub(crate) fn validate_capability(&self) -> Result<(), RelayError> {
        let bytes = self.expose_for_adapter();
        if !(MIN_CAPABILITY_BYTES..=MAX_CAPABILITY_BYTES).contains(&bytes.len())
            || bytes.iter().any(u8::is_ascii_whitespace)
        {
            return Err(RelayError::InvalidSecret);
        }
        Ok(())
    }

    pub(crate) fn validate_provider_token(&self) -> Result<(), RelayError> {
        let length = self.expose_for_adapter().len();
        if length == 0 || length > MAX_PROVIDER_TOKEN_BYTES {
            return Err(RelayError::InvalidSecret);
        }
        Ok(())
    }
}

impl fmt::Debug for OpaqueRelaySecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OpaqueRelaySecret(<redacted>)")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ValidatedRelayOrigin(Url);

impl ValidatedRelayOrigin {
    pub(crate) fn parse(value: &str, allow_loopback_http: bool) -> Result<Self, RelayError> {
        let url = Url::parse(value).map_err(|_| RelayError::InvalidRelayOrigin)?;
        if url.username() != ""
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.cannot_be_a_base()
            || !matches!(url.path(), "" | "/")
        {
            return Err(RelayError::InvalidRelayOrigin);
        }
        let host = url.host_str().ok_or(RelayError::InvalidRelayOrigin)?;
        let secure = url.scheme() == "https";
        let explicit_local = allow_loopback_http
            && url.scheme() == "http"
            && matches!(host, "localhost" | "127.0.0.1" | "::1");
        if !secure && !explicit_local {
            return Err(RelayError::InsecureRelayOrigin);
        }
        let mut normalized = url;
        let normalized_path = normalized.path().trim_end_matches('/').to_owned();
        normalized.set_path(&normalized_path);
        Ok(Self(normalized))
    }

    pub(crate) fn as_url(&self) -> &Url {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum RelayPushProvider {
    Apns,
    Fcm,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum RelayPushEnvironment {
    Sandbox,
    Production,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[allow(clippy::enum_variant_names)] // Exact closed relay wire vocabulary.
pub(crate) enum RelayEventClass {
    StateChanged,
    ActivityChanged,
    ConnectionChanged,
    SecurityChanged,
}

#[derive(Clone, Debug)]
pub(crate) struct RelayEnrollment {
    pub(crate) host_id: RelayHostId,
    pub(crate) origin: ValidatedRelayOrigin,
    pub(crate) installation_id: RelayInstallationId,
    /// Stable, non-secret identity supplied by the authenticated pairing
    /// command. Retries reuse it; a newer command must use a new identity.
    pub(crate) command_id: RelayEnrollmentCommandId,
    pub(crate) read_capability: OpaqueRelaySecret,
    pub(crate) manage_capability: OpaqueRelaySecret,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RelayBindingState {
    Preparing,
    /// Durable local rollback fence for an unpublished enrollment. Once this
    /// row is visible, every late Staged publication for the prior Preparing
    /// revision must conflict while capability-slot tombstones converge.
    RollbackPending,
    Staged,
    Active,
    TombstonePending,
    CleanupPending,
    Tombstoned,
    NeedsRepair,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RelayRegistrationDisposition {
    Active,
    Tombstone,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RelayProviderRegistration {
    pub(crate) provider: RelayPushProvider,
    pub(crate) environment: RelayPushEnvironment,
    pub(crate) local_generation: u64,
    pub(crate) token_alias: RelaySecretAlias,
    /// Exact secure-store revision durably journaled before any register RPC.
    /// `None` is limited to a newly reserved/unwritten alias or a legacy row.
    pub(crate) token_revision: Option<u64>,
    pub(crate) relay_registration_id: Option<RelayRegistrationId>,
    pub(crate) relay_generation: Option<u64>,
    pub(crate) disposition: RelayRegistrationDisposition,
    pub(crate) pending_sync: bool,
    /// Receipt and token custody for the registration that preceded a
    /// journal-reserved rotation. It remains durable until the newer
    /// registration is known to have superseded it or logout revokes it.
    pub(crate) previous: Option<RelayPreviousProviderRegistration>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RelayPreviousProviderRegistration {
    pub(crate) token_alias: RelaySecretAlias,
    /// Preserved custody revision for the registration being superseded.
    /// Legacy journals may decode this as `None`.
    pub(crate) token_revision: Option<u64>,
    pub(crate) relay_registration_id: Option<RelayRegistrationId>,
    pub(crate) relay_generation: Option<u64>,
}

/// Durable local high-watermark preventing stale native token observations
/// from recreating a provider registration after logout/revocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RelayProviderTombstoneFence {
    pub(crate) provider: RelayPushProvider,
    pub(crate) environment: RelayPushEnvironment,
    pub(crate) through_local_generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SeenWake {
    pub(crate) cursor: u64,
    pub(crate) event_id: RelayEventId,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RelayWakeLedger {
    pub(crate) highest_seen_cursor: u64,
    pub(crate) applied_cursor: u64,
    pub(crate) pending_ack_cursor: Option<u64>,
    pub(crate) remote_ack_ahead_cursor: Option<u64>,
    pub(crate) recently_seen: Vec<SeenWake>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RelayBindingEntry {
    pub(crate) revision: u64,
    /// Monotonic per-host generation allocated by a durable Preparing row
    /// before either capability slot is touched.
    pub(crate) staging_generation: u64,
    /// Stable authenticated command identity bound to the staged capability
    /// revisions and checked again at activation.
    pub(crate) staging_command_id: RelayEnrollmentCommandId,
    pub(crate) read_capability_revision: Option<u64>,
    pub(crate) manage_capability_revision: Option<u64>,
    /// Durable, authenticated fence allocated before every native repair.
    /// Native state accepts only a strictly greater generation and verifies
    /// the same generation again at its authoritative commit point.
    pub(crate) repair_generation: u64,
    pub(crate) host_id: RelayHostId,
    pub(crate) origin: ValidatedRelayOrigin,
    pub(crate) installation_id: RelayInstallationId,
    pub(crate) read_capability_alias: RelaySecretAlias,
    pub(crate) manage_capability_alias: RelaySecretAlias,
    pub(crate) state: RelayBindingState,
    pub(crate) registrations: Vec<RelayProviderRegistration>,
    /// One monotonic high-watermark per valid provider/environment pair.
    /// These survive provider-registration cleanup so restarts and other
    /// coordinator instances cannot replay an observation at or below a
    /// completed tombstone generation.
    pub(crate) provider_tombstone_fences: Vec<RelayProviderTombstoneFence>,
    /// Provider-token aliases retired by a durable rotation. An alias remains
    /// here until secure storage confirms idempotent deletion, so crashes and
    /// ambiguous delete results cannot orphan provider credentials.
    pub(crate) retired_token_aliases: Vec<RelaySecretAlias>,
    pub(crate) wake: RelayWakeLedger,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OpaqueWakeHint {
    pub(crate) schema_version: u16,
    pub(crate) installation_id: RelayInstallationId,
    pub(crate) event_id: RelayEventId,
    pub(crate) cursor: u64,
    pub(crate) event_class: RelayEventClass,
    pub(crate) expires_at_ms: u64,
}

impl OpaqueWakeHint {
    pub(crate) fn validate(&self, now_ms: u64) -> Result<(), RelayError> {
        const MAX_LIFETIME_MS: u64 = 24 * 60 * 60 * 1_000;
        if self.schema_version != RELAY_SCHEMA_VERSION
            || self.cursor == 0
            || self.expires_at_ms <= now_ms
            || self.expires_at_ms - now_ms > MAX_LIFETIME_MS
        {
            return Err(RelayError::InvalidWake);
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PushTokenObservation {
    pub(crate) provider: RelayPushProvider,
    pub(crate) environment: RelayPushEnvironment,
    pub(crate) token: OpaqueRelaySecret,
    pub(crate) local_generation: u64,
    pub(crate) observed_at_ms: u64,
}

impl PushTokenObservation {
    pub(crate) fn validate(&self) -> Result<(), RelayError> {
        if self.local_generation == 0
            || self.observed_at_ms == 0
            || (self.provider == RelayPushProvider::Fcm
                && self.environment != RelayPushEnvironment::Production)
        {
            return Err(RelayError::InvalidProviderRegistration);
        }
        self.token.validate_provider_token()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PushTokenTombstone {
    pub(crate) provider: RelayPushProvider,
    pub(crate) environment: RelayPushEnvironment,
    pub(crate) through_local_generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PushFanoutReceipt {
    pub(crate) attempted: u32,
    pub(crate) synchronized: u32,
    pub(crate) pending_retry: u32,
    pub(crate) re_pair_required: u32,
    pub(crate) rejected: u32,
}

impl PushFanoutReceipt {
    pub(crate) fn empty() -> Self {
        Self {
            attempted: 0,
            synchronized: 0,
            pending_retry: 0,
            re_pair_required: 0,
            rejected: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RelayEventEnvelope {
    pub(crate) event_id: RelayEventId,
    pub(crate) cursor: u64,
    pub(crate) event_class: RelayEventClass,
    pub(crate) expires_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RelayEventPage {
    pub(crate) schema_version: u16,
    pub(crate) requested_after: u64,
    pub(crate) next_cursor: u64,
    pub(crate) high_watermark: u64,
    pub(crate) replay_floor: u64,
    pub(crate) reset_required: bool,
    pub(crate) snapshot_available: bool,
    pub(crate) events: Vec<RelayEventEnvelope>,
    pub(crate) encoded_bytes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RelaySnapshotEnvelope {
    pub(crate) schema_version: u16,
    pub(crate) revision: u64,
    pub(crate) through_cursor: u64,
    pub(crate) expires_at_ms: u64,
    pub(crate) encoded_bytes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RelayDeviceRegistrationReceipt {
    pub(crate) schema_version: u16,
    pub(crate) installation_id: RelayInstallationId,
    pub(crate) registration_id: RelayRegistrationId,
    pub(crate) provider: RelayPushProvider,
    pub(crate) environment: RelayPushEnvironment,
    pub(crate) generation: u64,
    pub(crate) replaced: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RelayAckReceipt {
    pub(crate) schema_version: u16,
    pub(crate) installation_id: RelayInstallationId,
    pub(crate) acknowledged_through: u64,
    pub(crate) replayed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RelayRepairMode {
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RelayRepairReceipt {
    pub(crate) applied_through_cursor: u64,
    pub(crate) authoritative: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RelayReconcileReceipt {
    pub(crate) host_id: RelayHostId,
    pub(crate) applied_through_cursor: u64,
    pub(crate) acknowledged_through_cursor: u64,
    pub(crate) changed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RelayReconcileOutcome {
    Applied(RelayReconcileReceipt),
    CleanupCompleted {
        host_id: RelayHostId,
    },
    Failed {
        host_id: RelayHostId,
        error: RelayError,
    },
}

/// Secret-free operational projection for native lifecycle and diagnostics.
///
/// Capability and provider-token aliases deliberately remain private. Native
/// callers only need to know whether durable work is pending and whether a
/// binding requires user-visible repair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RelayBindingStatus {
    pub(crate) host_id: RelayHostId,
    pub(crate) installation_id: RelayInstallationId,
    pub(crate) state: RelayBindingState,
    pub(crate) highest_seen_cursor: u64,
    pub(crate) applied_cursor: u64,
    pub(crate) pending_ack_cursor: Option<u64>,
    pub(crate) provider_registration_count: u32,
    pub(crate) has_pending_provider_sync: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RelayRetryClass {
    Retry,
    RePairRequired,
    Tombstoned,
    Permanent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RelayTransportError {
    Unauthorized,
    NotFound,
    Gone,
    RateLimited,
    Server,
    Network,
    Timeout,
    RedirectRejected,
    ResponseTooLarge,
    InvalidResponse,
    Cancelled,
}

impl RelayTransportError {
    pub(crate) fn retry_class(self) -> RelayRetryClass {
        match self {
            Self::RateLimited | Self::Server | Self::Network | Self::Timeout | Self::Cancelled => {
                RelayRetryClass::Retry
            }
            Self::Unauthorized => RelayRetryClass::RePairRequired,
            Self::NotFound | Self::Gone => RelayRetryClass::Tombstoned,
            Self::RedirectRejected | Self::ResponseTooLarge | Self::InvalidResponse => {
                RelayRetryClass::Permanent
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RelayJournalError {
    Conflict,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RelaySecretStoreError {
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RelaySecretCreateOutcome {
    Created,
    AlreadyExists,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RelaySecretRevision {
    Missing,
    Found(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RelaySecretCasOutcome {
    Stored,
    Conflict,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RelayRepairError {
    Unavailable,
    RePairRequired,
    Cancelled,
    DeadlineExceeded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RelayError {
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

#[derive(Clone, Debug)]
pub(crate) struct RelayCancellation {
    inner: Arc<RelayCancellationInner>,
}

#[derive(Debug)]
struct RelayCancellationInner {
    cancelled: AtomicBool,
    notify: tokio::sync::Notify,
}

impl RelayCancellation {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(RelayCancellationInner {
                cancelled: AtomicBool::new(false),
                notify: tokio::sync::Notify::new(),
            }),
        }
    }

    #[cfg(test)]
    pub(crate) fn cancel(&self) {
        if !self.inner.cancelled.swap(true, Ordering::AcqRel) {
            self.inner.notify.notify_waiters();
        }
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::Acquire)
    }

    pub(crate) async fn cancelled(&self) {
        loop {
            let notified = self.inner.notify.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RelayOperationContext {
    deadline: tokio::time::Instant,
    pub(crate) cancellation: RelayCancellation,
    pub(crate) max_response_bytes: usize,
    pub(crate) page_limit: u32,
    pub(crate) follow_redirects: bool,
}

impl RelayOperationContext {
    pub(crate) fn with_timeout(timeout: Duration) -> Self {
        Self {
            deadline: tokio::time::Instant::now() + timeout,
            cancellation: RelayCancellation::new(),
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            page_limit: DEFAULT_PAGE_LIMIT,
            follow_redirects: false,
        }
    }

    pub(crate) fn remaining(&self) -> Result<Duration, RelayError> {
        if self.cancellation.is_cancelled() {
            return Err(RelayError::Cancelled);
        }
        self.deadline
            .checked_duration_since(tokio::time::Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or(RelayError::DeadlineExceeded)
    }

    pub(crate) fn request_timeout(&self, configured: Duration) -> Result<Duration, RelayError> {
        Ok(self.remaining()?.min(configured))
    }
}

pub(crate) fn map_transport_error(error: RelayTransportError) -> RelayError {
    match error.retry_class() {
        RelayRetryClass::Retry => {
            if error == RelayTransportError::Cancelled {
                RelayError::Cancelled
            } else if error == RelayTransportError::Timeout {
                RelayError::DeadlineExceeded
            } else {
                RelayError::Retryable
            }
        }
        RelayRetryClass::RePairRequired => RelayError::RePairRequired,
        RelayRetryClass::Tombstoned => RelayError::Tombstoned,
        RelayRetryClass::Permanent => RelayError::PermanentFailure,
    }
}
