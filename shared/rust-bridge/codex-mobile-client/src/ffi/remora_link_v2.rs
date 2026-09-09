//! Native custody and AppClient commands for Remora Link v2.
//!
//! Rust owns the protocol, Iroh transport, crash journal, retry identities,
//! enrollment transcript verification, and reconnect policy. Native code owns
//! only atomic persistence and non-exportable platform key operations.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use codex_app_server_client::RemoteAppServerConnectArgs;
use futures::StreamExt as _;
use futures::stream::FuturesUnordered;
use iroh::endpoint::{Connection, QuicTransportConfig, RecvStream, SendStream, VarInt};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayUrl, SecretKey};
use p256::PublicKey;
use p256::ecdsa::Signature;
use p256::elliptic_curve::sec1::ToEncodedPoint as _;
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::warn;
use zeroize::{Zeroize, Zeroizing};

use crate::ffi::AppClient;
use crate::ffi::background_relay::AppRelaySecretValue;
use crate::remote_host_pairing::identity::{V2Invite, decode_pairing_code};
use crate::remote_host_pairing::remora_link_v2::{
    ALPN, AgentInfoV2, AgentWireV2, AttachKindV2, AttachmentCustodyErrorV2, ConfirmationModeV2,
    CredentialCustodyPortV2, CredentialPortError, DeviceScopeV2, EnrollmentOutcomeV2,
    EntropyPortV2, FinishedExchangeV2, ForgetOutcomeV2, HardwareKeyV2, HostPortErrorV2, HostPortV2,
    HostRouteV2, InvitationInspectionV2, JOURNAL_SCHEMA_VERSION, JournalPhaseV2,
    JournalPortErrorV2, JournalPortV2, LifecycleErrorV2, MutationOutcomeV2, PairingJournalEntryV2,
    PairingLifecycleV2, ReconnectOutcomeV2, RequestCorrelationV2, RequestV2, ResponseV2,
    RestartDispositionV2, RestartOutcomeV2, RetainedAttachmentRegistryV2, StartedExchangeV2,
    connect_runtime_client_v2, read_response_frame, write_proof_frame, write_request_frame,
};
use crate::session::connection::{
    RemoteSessionExtras, RuntimeRemoteSessionResource, ServerConfig, ServerSession,
    remote_connect_args,
};
use crate::session::remote_transport::{
    Reconnected, RemoteTransport, ReplayOutcome, SessionKeepalive,
};
use crate::store::ServerHealthSnapshot;
use crate::transport::TransportError;
use crate::types::AgentRuntimeInfo;

const NATIVE_CALLBACK_TIMEOUT: Duration = Duration::from_secs(10);
const HOST_START_TIMEOUT: Duration = Duration::from_secs(35);
const HOST_FINISH_TIMEOUT: Duration = Duration::from_secs(35);
const AWAIT_APPROVAL_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const AWAIT_APPROVAL_INTERVAL: Duration = Duration::from_millis(750);
const MAX_JOURNAL_BYTES: usize = 2 * 1_024 * 1_024;
const MAX_JOURNAL_HOSTS: usize = 256;
const MAX_JOURNAL_CAS_ATTEMPTS: usize = 8;
const MAX_CACHED_INVITATIONS: usize = 32;
const MAX_PAIRING_CODE_BYTES: usize = 4_096 + 32;
const MAX_LIVE_EXCHANGES: usize = 64;
const MAX_LIVE_EXCHANGE_AGE: Duration = Duration::from_secs(2 * 60);
const MAX_RETAINED_ATTACHMENTS: usize = 64;
const MAX_RETAINED_ATTACHMENT_AGE: Duration = Duration::from_secs(2 * 60);
const ATTACHMENT_REAPER_INTERVAL: Duration = Duration::from_secs(5);
const BATCH_CONCURRENCY: usize = 8;
const BATCH_TIMEOUT: Duration = Duration::from_secs(2 * 60);
const JOURNAL_ENVELOPE_SCHEMA: u32 = 1;
const CLOSE_CODE: u32 = 0x22;
const DEFAULT_HOST_DISPLAY_NAME: &str = "Remote Host";

#[path = "remora_link_relay.rs"]
mod relay;
pub(crate) use relay::PairedHostRelayRepair;

// MARK: - Native callback boundaries

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AppRemoraLinkJournalSnapshot {
    /// Monotonic revision for the complete opaque blob.
    pub revision: u64,
    /// Versioned non-secret Rust payload. Native code must never parse it.
    pub payload: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AppRemoraLinkJournalLoad {
    Missing,
    Loaded {
        snapshot: AppRemoraLinkJournalSnapshot,
    },
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AppRemoraLinkJournalWriteOutcome {
    Stored,
    Conflict,
    Unavailable,
}

/// Atomic persistence for one opaque non-secret journal blob.
#[uniffi::export(callback_interface)]
#[async_trait]
pub trait AppRemoraLinkJournalBackend: Send + Sync {
    async fn load(&self) -> AppRemoraLinkJournalLoad;
    async fn compare_and_swap(
        &self,
        expected_revision: Option<u64>,
        replacement: AppRemoraLinkJournalSnapshot,
    ) -> AppRemoraLinkJournalWriteOutcome;
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum AppRemoraLinkTransportIdentityError {
    #[error("Remora Link transport identity storage is unavailable")]
    Unavailable,
}

/// Dedicated secret-bearing QR/paste carrier.
///
/// Rust owns and zeroizes the lifted bytes. The generated Swift/Kotlin
/// hardener supplies the same reference-semantic zeroizing contract for the
/// native value and transfer buffer. Pairing codes never cross this boundary
/// as immutable language strings.
pub struct AppRemoraLinkPairingCode(Vec<u8>);

impl AppRemoraLinkPairingCode {
    fn into_bytes(mut self) -> Vec<u8> {
        std::mem::take(&mut self.0)
    }
}

impl std::fmt::Debug for AppRemoraLinkPairingCode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppRemoraLinkPairingCode")
            .field("bytes", &"<redacted>")
            .finish()
    }
}

impl Drop for AppRemoraLinkPairingCode {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl From<Vec<u8>> for AppRemoraLinkPairingCode {
    fn from(value: Vec<u8>) -> Self {
        Self(value)
    }
}

impl From<AppRemoraLinkPairingCode> for Vec<u8> {
    fn from(value: AppRemoraLinkPairingCode) -> Self {
        value.into_bytes()
    }
}

uniffi::custom_type!(AppRemoraLinkPairingCode, Vec<u8>);

/// Atomic app-wide custody for the dedicated Remora Link Iroh identity.
///
/// The callback must return the already-stored value when present, otherwise
/// atomically store and return `candidate`. It must never import or alias the
/// retired v1 endpoint identity. Exactly 32 bytes are required.
#[uniffi::export(callback_interface)]
#[async_trait]
pub trait AppRemoraLinkTransportIdentityBackend: Send + Sync {
    async fn load_or_create(
        &self,
        candidate: AppRelaySecretValue,
    ) -> Result<AppRelaySecretValue, AppRemoraLinkTransportIdentityError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AppRemoraLinkKeyAssurance {
    SecureEnclave,
    StrongBox,
    TrustedExecutionEnvironment,
    /// Simulator/emulator fallback. Rust rejects this case in release builds.
    SoftwareDebugOnly,
    /// Secure platform custody whose concrete hardware class is not exposed.
    ///
    /// This is intentionally distinct from `TrustedExecutionEnvironment`; it
    /// is an app/FFI assurance classification and does not change wire or
    /// journal formats.
    UnknownSecure,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AppRemoraLinkHardwareKey {
    /// Stable Keychain tag or Android Keystore alias; never key material.
    pub slot: String,
    /// Exact uncompressed SEC1 P-256 point (`0x04 || X || Y`).
    pub public_key_sec1: Vec<u8>,
    pub assurance: AppRemoraLinkKeyAssurance,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AppRemoraLinkHardwareKeyLoad {
    Missing,
    Loaded { key: AppRemoraLinkHardwareKey },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AppRemoraLinkKeyDeletionStatus {
    Deleted,
    AlreadyMissing,
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum AppRemoraLinkDeviceKeyError {
    #[error("hardware-backed key storage is unavailable")]
    Unavailable,
    #[error("hardware-backed key is missing")]
    Missing,
    #[error("hardware-backed key was invalidated")]
    Invalidated,
    #[error("hardware-backed P-256 is unavailable")]
    HardwareUnavailable,
    #[error("device is locked")]
    Locked,
    #[error("hardware-backed signature is invalid")]
    InvalidSignature,
}

/// Non-exportable P-256 authorization-key custody.
///
/// `sign_message` must perform ECDSA-with-SHA-256 over the supplied canonical
/// message exactly once and return canonical ASN.1 DER. Private-key bytes can
/// never cross this interface. Sensitive byte arguments/results use the same
/// zeroizing carrier as relay secrets; generated Swift/Kotlin wrappers wipe
/// their transfer and callback buffers.
#[uniffi::export(callback_interface)]
#[async_trait]
pub trait AppRemoraLinkDeviceKeyBackend: Send + Sync {
    async fn ensure_hardware_key(
        &self,
        host_id: String,
    ) -> Result<AppRemoraLinkHardwareKey, AppRemoraLinkDeviceKeyError>;
    async fn load_hardware_key(
        &self,
        slot: String,
    ) -> Result<AppRemoraLinkHardwareKeyLoad, AppRemoraLinkDeviceKeyError>;
    async fn sign_message(
        &self,
        slot: String,
        message: AppRelaySecretValue,
    ) -> Result<AppRelaySecretValue, AppRemoraLinkDeviceKeyError>;
    async fn delete_hardware_key(
        &self,
        slot: String,
    ) -> Result<AppRemoraLinkKeyDeletionStatus, AppRemoraLinkDeviceKeyError>;
}

// MARK: - Display-safe public values

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AppRemoraLinkConfirmationMode {
    Interactive,
    Unattended,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, uniffi::Enum)]
pub enum AppRemoraLinkScope {
    InspectRuntimes,
    ConnectRuntime,
    RestartRuntime,
    SelfRevoke,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AppRemoraLinkRuntimeOffer {
    pub runtime_id: String,
    pub display_name: String,
    pub available: bool,
    pub recommended: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AppRemoraLinkOffer {
    /// Process-local opaque selection handle. It contains no invitation data.
    pub offer_id: String,
    pub host_id: String,
    pub host_display_name: String,
    pub expires_at_unix_ms: u64,
    pub confirmation_mode: AppRemoraLinkConfirmationMode,
    pub runtime_offers: Vec<AppRemoraLinkRuntimeOffer>,
    pub maximum_scopes: Vec<AppRemoraLinkScope>,
    pub required_scopes: Vec<AppRemoraLinkScope>,
    pub default_scopes: Vec<AppRemoraLinkScope>,
    pub default_runtime_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AppRemoraLinkInspection {
    Ready { offer: AppRemoraLinkOffer },
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AppRemoraLinkAcceptance {
    pub offer_id: String,
    pub device_display_name: String,
    pub selected_runtime_ids: Vec<String>,
    pub requested_scopes: Vec<AppRemoraLinkScope>,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AppRemoraLinkPairingOutcome {
    AwaitingHostApproval {
        host_id: String,
        sas: String,
        expires_at_unix_ms: u64,
        selected_runtime_ids: Vec<String>,
        requested_scopes: Vec<AppRemoraLinkScope>,
    },
    Paired {
        host_id: String,
        sas: String,
        selected_runtime_ids: Vec<String>,
        granted_scopes: Vec<AppRemoraLinkScope>,
        created_at_unix_ms: u64,
    },
    AlreadyPaired {
        host_id: String,
        selected_runtime_ids: Vec<String>,
        granted_scopes: Vec<AppRemoraLinkScope>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AppRemoraLinkHostState {
    Inspecting,
    Ready,
    Pairing,
    AwaitingHostApproval,
    Paired,
    Revoking,
    Revoked,
    Forgetting,
    Forgotten,
    NeedsRepair,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AppRemoraLinkPendingApproval {
    pub sas: String,
    pub expires_at_unix_ms: u64,
    pub requested_scopes: Vec<AppRemoraLinkScope>,
    pub device_display_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AppRemoraLinkPendingRestart {
    pub runtime_id: String,
    pub command_sequence: u64,
    pub outcome_unknown: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AppRemoraLinkHostSummary {
    pub host_id: String,
    pub host_display_name: String,
    pub state: AppRemoraLinkHostState,
    pub selected_runtime_ids: Vec<String>,
    pub granted_scopes: Vec<AppRemoraLinkScope>,
    pub pending_approval: Option<AppRemoraLinkPendingApproval>,
    pub pending_restart: Option<AppRemoraLinkPendingRestart>,
    pub host_revocation_still_required: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AppRemoraLinkAttachKind {
    Fresh,
    Resumed,
    DriftReload,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AppRemoraLinkReconnectResult {
    pub host_id: String,
    pub runtime_id: String,
    pub attached: AppRemoraLinkAttachKind,
    pub current_sequence: u64,
    pub floor_sequence: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AppRemoraLinkFailure {
    JournalUnavailable,
    JournalConflict,
    JournalCorrupt,
    CredentialUnavailable,
    MissingCredential,
    InvalidSignature,
    HostUnavailable,
    V2Unavailable,
    Cancelled,
    IdentityDrift,
    PolicyDrift,
    ConfirmationMismatch,
    ProtocolViolation,
    PairingUnavailable,
    AuthorizationRequired,
    RuntimeUnavailable,
    OutcomeUnknown,
    InvalidSelection,
    InvitationMismatch,
    NotPaired,
    OperationInProgress,
    NeedsRepair,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AppRemoraLinkReconnectAttempt {
    Connected {
        result: AppRemoraLinkReconnectResult,
    },
    Failed {
        host_id: String,
        runtime_id: String,
        failure: AppRemoraLinkFailure,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AppRemoraLinkReconnectBatch {
    pub attempts: Vec<AppRemoraLinkReconnectAttempt>,
    pub recovery_failures: Vec<AppRemoraLinkHostFailure>,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AppRemoraLinkHostFailure {
    pub host_id: String,
    pub failure: AppRemoraLinkFailure,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AppRemoraLinkRevocationOutcome {
    Revoked,
    OutcomeUnknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AppRemoraLinkRestartOutcome {
    Succeeded { command_sequence: u64 },
    OutcomeUnknown { command_sequence: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AppRemoraLinkPairingCancellationOutcome {
    Cancelled,
    OutcomeUnknown,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AppRemoraLinkForgetResult {
    pub host_id: String,
    pub already_forgotten: bool,
    pub host_revocation_still_required: bool,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error, uniffi::Error)]
pub enum RemoraLinkError {
    #[error("Remora Link is not configured")]
    NotConfigured,
    #[error("pairing code is invalid or unsupported")]
    InvalidPairingCode,
    #[error("pairing offer is no longer retained")]
    UnknownOffer,
    #[error("the original pairing code is required to continue")]
    PairingCodeRequired,
    #[error("Remora Link journal is unavailable")]
    JournalUnavailable,
    #[error("Remora Link journal changed concurrently")]
    JournalConflict,
    #[error("Remora Link journal is corrupt or unsupported")]
    JournalCorrupt,
    #[error("hardware-backed credential is unavailable")]
    CredentialUnavailable,
    #[error("hardware-backed credential is missing")]
    MissingCredential,
    #[error("hardware-backed signature is invalid")]
    InvalidSignature,
    #[error("Remora Link host is unavailable")]
    HostUnavailable,
    #[error("Remora Link v2 is unavailable")]
    V2Unavailable,
    #[error("Remora Link operation was cancelled")]
    Cancelled,
    #[error("Remora Link identity changed")]
    IdentityDrift,
    #[error("Remora Link host policy changed")]
    PolicyDrift,
    #[error("Remora Link enrollment confirmation did not match")]
    ConfirmationMismatch,
    #[error("Remora Link protocol was violated")]
    ProtocolViolation,
    #[error("Remora Link invitation is unavailable")]
    PairingUnavailable,
    #[error("Remora Link authorization is required")]
    AuthorizationRequired,
    #[error("selected Remora Link runtime is unavailable")]
    RuntimeUnavailable,
    #[error("Remora Link operation outcome is unknown")]
    OutcomeUnknown,
    #[error("Remora Link runtime or scope selection is invalid")]
    InvalidSelection,
    #[error("pairing invitation does not match the staged operation")]
    InvitationMismatch,
    #[error("Remora Link host is not paired")]
    NotPaired,
    #[error("another Remora Link operation is in progress")]
    OperationInProgress,
    #[error("Remora Link host requires repair")]
    NeedsRepair,
}

// MARK: - Configured lifecycle

pub(crate) struct ConfiguredRemoraLink {
    lifecycle: Arc<PairingLifecycleV2>,
    journal: Arc<NativeRemoraLinkJournal>,
    host: Arc<IrohRemoraLinkHost>,
    invitations: Mutex<InvitationCache>,
    session_connect_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl ConfiguredRemoraLink {
    pub(crate) fn close_shells_for_host(&self, host_id: &str) {
        self.host.shell_connections.close_host(host_id);
    }
}

#[derive(Default)]
struct RemoraLinkShellConnectionRegistryV2 {
    hosts: std::sync::Mutex<HashMap<String, RemoraLinkShellHostConnectionsV2>>,
}

#[derive(Default)]
struct RemoraLinkShellHostConnectionsV2 {
    generation: u64,
    connections: HashMap<String, std::sync::Weak<RegisteredRemoraLinkShellConnectionV2>>,
}

impl RemoraLinkShellConnectionRegistryV2 {
    fn claim_generation(&self, host_id: &str) -> u64 {
        self.hosts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(host_id.to_string())
            .or_default()
            .generation
    }

    fn register(
        self: &Arc<Self>,
        host_id: &str,
        generation: u64,
        connection: Arc<dyn crate::terminal::remote_shell::RemoteShellConnection>,
    ) -> Result<Arc<dyn crate::terminal::remote_shell::RemoteShellConnection>, ()> {
        let id = uuid::Uuid::new_v4().to_string();
        let registered = Arc::new(RegisteredRemoraLinkShellConnectionV2 {
            connection,
            registry: Arc::downgrade(self),
            host_id: host_id.to_string(),
            id: id.clone(),
            closed: AtomicBool::new(false),
        });
        let mut hosts = self
            .hosts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let host = hosts.entry(host_id.to_string()).or_default();
        if host.generation != generation {
            drop(hosts);
            registered.close_once();
            return Err(());
        }
        host.connections.insert(id, Arc::downgrade(&registered));
        drop(hosts);
        Ok(registered)
    }

    fn remove(&self, host_id: &str, id: &str) {
        let mut hosts = self
            .hosts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(host) = hosts.get_mut(host_id) {
            host.connections.remove(id);
        }
    }

    fn close_host(&self, host_id: &str) {
        let connections = {
            let mut hosts = self
                .hosts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let host = hosts.entry(host_id.to_string()).or_default();
            host.generation = host.generation.wrapping_add(1);
            std::mem::take(&mut host.connections)
        };
        for connection in connections
            .into_values()
            .filter_map(|value| value.upgrade())
        {
            connection.close_once();
        }
    }

    fn close_all(&self) {
        let connections = {
            let mut hosts = self
                .hosts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            hosts
                .values_mut()
                .flat_map(|host| {
                    host.generation = host.generation.wrapping_add(1);
                    std::mem::take(&mut host.connections).into_values()
                })
                .collect::<Vec<_>>()
        };
        for connection in connections.into_iter().filter_map(|value| value.upgrade()) {
            connection.close_once();
        }
    }

    #[cfg(test)]
    fn connection_count(&self, host_id: &str) -> usize {
        self.hosts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(host_id)
            .map_or(0, |host| host.connections.len())
    }
}

struct RegisteredRemoraLinkShellConnectionV2 {
    connection: Arc<dyn crate::terminal::remote_shell::RemoteShellConnection>,
    registry: std::sync::Weak<RemoraLinkShellConnectionRegistryV2>,
    host_id: String,
    id: String,
    closed: AtomicBool,
}

impl RegisteredRemoraLinkShellConnectionV2 {
    fn close_once(&self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            self.connection.close();
        }
        if let Some(registry) = self.registry.upgrade() {
            registry.remove(&self.host_id, &self.id);
        }
    }
}

impl crate::terminal::remote_shell::RemoteShellConnection
    for RegisteredRemoraLinkShellConnectionV2
{
    fn close(&self) {
        self.close_once();
    }
}

impl Drop for RegisteredRemoraLinkShellConnectionV2 {
    fn drop(&mut self) {
        self.close_once();
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RemoraLinkRuntimeConnectOutcome {
    pub(crate) server_id: String,
    pub(crate) connected_runtime_ids: Vec<String>,
    pub(crate) unavailable_runtime_ids: Vec<String>,
    connected_results: Vec<AppRemoraLinkReconnectResult>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RemoraLinkRuntimePlanEntry {
    runtime_id: String,
    display_name: String,
    wire: Option<AgentWireV2>,
    advertised_available: bool,
}

#[async_trait]
trait RemoraLinkRuntimeSessionAttacher: Send + Sync {
    async fn attach_runtime_session(
        &self,
        host_id: &str,
        force_cold_rebuild: bool,
    ) -> Result<RemoraLinkRuntimeConnectOutcome, TransportError>;
}

struct CanonicalRemoraLinkRuntimeSessionAttacher {
    client: Arc<crate::MobileClient>,
    configured: Arc<ConfiguredRemoraLink>,
}

struct PreparedRuntimeAttachmentV2 {
    outcome: ReconnectOutcomeV2,
    attachment: LiveRuntimeSession,
}

struct RemoraLinkConnectingProjectionGuard {
    app_store: Arc<crate::store::AppStoreReducer>,
    host_id: String,
    armed: bool,
}

impl RemoraLinkConnectingProjectionGuard {
    fn new(app_store: Arc<crate::store::AppStoreReducer>, host_id: String) -> Self {
        Self {
            app_store,
            host_id,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for RemoraLinkConnectingProjectionGuard {
    fn drop(&mut self) {
        if self.armed {
            project_remora_link_disconnected(&self.app_store, &self.host_id);
        }
    }
}

#[async_trait]
impl RemoraLinkRuntimeSessionAttacher for CanonicalRemoraLinkRuntimeSessionAttacher {
    async fn attach_runtime_session(
        &self,
        host_id: &str,
        force_cold_rebuild: bool,
    ) -> Result<RemoraLinkRuntimeConnectOutcome, TransportError> {
        self.client
            .connect_remora_link_runtime(
                Arc::clone(&self.configured),
                host_id,
                force_cold_rebuild,
                None,
            )
            .await
    }
}

#[derive(Default)]
struct InvitationCache {
    by_offer: HashMap<String, CachedInvitation>,
    by_host: HashMap<String, V2Invite>,
}

struct CachedInvitation {
    offer: AppRemoraLinkOffer,
    invite: V2Invite,
}

impl InvitationCache {
    fn remove_host(&mut self, host_id: &str) {
        self.by_host.remove(host_id);
        self.by_offer
            .retain(|_, value| value.offer.host_id != host_id);
    }
}

impl ConfiguredRemoraLink {
    async fn session_connect_lock(&self, host_id: &str) -> Arc<Mutex<()>> {
        let mut locks = self.session_connect_locks.lock().await;
        Arc::clone(
            locks
                .entry(host_id.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(()))),
        )
    }

    async fn shutdown(&self) {
        let mut invitations = self.invitations.lock().await;
        invitations.by_offer.clear();
        invitations.by_host.clear();
        drop(invitations);
        self.host.shutdown().await;
    }

    async fn cache_offer(&self, offer: AppRemoraLinkOffer, invite: V2Invite) {
        let mut cache = self.invitations.lock().await;
        cache
            .by_offer
            .retain(|_, cached| cached.offer.host_id != offer.host_id);
        if cache.by_offer.len() >= MAX_CACHED_INVITATIONS
            && let Some(evicted) = cache.by_offer.keys().next().cloned()
            && let Some(evicted) = cache.by_offer.remove(&evicted)
        {
            let evicted_host_id = evicted.offer.host_id;
            if !cache
                .by_offer
                .values()
                .any(|cached| cached.offer.host_id == evicted_host_id)
            {
                cache.by_host.remove(&evicted_host_id);
            }
        }
        cache.by_host.insert(offer.host_id.clone(), invite.clone());
        cache
            .by_offer
            .insert(offer.offer_id.clone(), CachedInvitation { offer, invite });
    }

    async fn remove_invitation(&self, host_id: &str) {
        let mut cache = self.invitations.lock().await;
        cache.remove_host(host_id);
    }
}

// MARK: - AppClient commands

#[uniffi::export(async_runtime = "tokio")]
impl AppClient {
    /// Install or replace the Remora Link lifecycle adapters. An existing
    /// endpoint is closed first so one transport identity is never bound by
    /// two endpoints at once. The replacement is published only after the
    /// journal, transport identity, and endpoint pass fail-closed preflight.
    pub async fn configure_remora_link(
        &self,
        journal: Box<dyn AppRemoraLinkJournalBackend>,
        transport_identity: Box<dyn AppRemoraLinkTransportIdentityBackend>,
        device_keys: Box<dyn AppRemoraLinkDeviceKeyBackend>,
    ) -> Result<(), RemoraLinkError> {
        let _configuration = self.inner.remora_link_configuration.write().await;
        let previous = remora_link_write(&self.inner.remora_link).take();
        if let Some(previous) = previous {
            self.inner.disconnect_all_remora_link_sessions().await;
            previous.shutdown().await;
        }
        let configured = build_configuration(journal, transport_identity, device_keys).await?;
        remora_link_write(&self.inner.remora_link).replace(Arc::clone(&configured));
        if let Err(error) = cold_restore_configured_remora_link(
            Arc::clone(&self.inner),
            configured,
            tokio::time::Instant::now() + BATCH_TIMEOUT,
        )
        .await
        {
            warn!(%error, "Remora Link configured but cold restore did not complete");
        }
        Ok(())
    }

    /// Drop native callback references and gracefully close the v2 endpoint.
    pub async fn clear_remora_link(&self) {
        let _configuration = self.inner.remora_link_configuration.write().await;
        let previous = remora_link_write(&self.inner.remora_link).take();
        if let Some(previous) = previous {
            self.inner.disconnect_all_remora_link_sessions().await;
            previous.shutdown().await;
        }
    }

    /// Decode, authenticate, and inspect a QR/paste value through one path.
    pub async fn inspect_remora_link_code(
        &self,
        code: AppRemoraLinkPairingCode,
    ) -> Result<AppRemoraLinkInspection, RemoraLinkError> {
        let _configuration = self.inner.remora_link_configuration.read().await;
        let invite = decode_code(code)?;
        let configured = self.configured_remora_link()?;
        let inspection = configured.lifecycle.inspect(&invite).await?;
        let offer = offer_from_inspection(&invite, inspection)?;
        configured.cache_offer(offer.clone(), invite).await;
        Ok(AppRemoraLinkInspection::Ready { offer })
    }

    /// Stage and perform one enrollment attempt. Interactive enrollment
    /// returns promptly with the Rust-computed SAS.
    pub async fn accept_remora_link_offer(
        &self,
        acceptance: AppRemoraLinkAcceptance,
    ) -> Result<AppRemoraLinkPairingOutcome, RemoraLinkError> {
        let _configuration = self.inner.remora_link_configuration.read().await;
        let configured = self.configured_remora_link()?;
        let cached = {
            let cache = configured.invitations.lock().await;
            cache
                .by_offer
                .get(&acceptance.offer_id)
                .map(|value| (value.offer.clone(), value.invite.clone()))
                .ok_or(RemoraLinkError::UnknownOffer)?
        };
        let (offer, invite) = cached;
        validate_coding_runtime_scope_selection(
            &acceptance.selected_runtime_ids,
            &acceptance.requested_scopes,
        )?;
        let outcome = configured
            .lifecycle
            .enroll(
                &invite,
                acceptance.device_display_name,
                acceptance.selected_runtime_ids,
                acceptance
                    .requested_scopes
                    .into_iter()
                    .map(Into::into)
                    .collect(),
            )
            .await?;
        let projected = project_pairing_outcome(&offer.host_id, outcome);
        if pairing_host_id(&projected).is_some() {
            configured.remove_invitation(&offer.host_id).await;
            let attacher = CanonicalRemoraLinkRuntimeSessionAttacher {
                client: Arc::clone(&self.inner),
                configured,
            };
            require_pairing_attachment(&attacher, &projected).await?;
        }
        Ok(projected)
    }

    /// Wait inside Rust for an exact interactive-enrollment replay to become
    /// approved. `code` may be omitted while this process still retains the
    /// invitation; cold recovery requires the original code because secrets
    /// are deliberately absent from the journal.
    pub async fn await_remora_link_pairing(
        &self,
        host_id: String,
        code: Option<AppRemoraLinkPairingCode>,
    ) -> Result<AppRemoraLinkPairingOutcome, RemoraLinkError> {
        let _configuration = self.inner.remora_link_configuration.read().await;
        let configured = self.configured_remora_link()?;
        let entry = configured
            .journal
            .load_entry(&host_id)
            .await?
            .ok_or(RemoraLinkError::InvitationMismatch)?;
        if matches!(entry.phase, JournalPhaseV2::Enrolled) {
            let projected = project_existing_credential(&entry)?;
            let attacher = CanonicalRemoraLinkRuntimeSessionAttacher {
                client: Arc::clone(&self.inner),
                configured,
            };
            require_pairing_attachment(&attacher, &projected).await?;
            return Ok(projected);
        }
        let supplied_code = code.is_some();
        let invite = if let Some(code) = code {
            let invite = decode_code(code)?;
            if v2_host_id(&invite) != host_id {
                return Err(RemoraLinkError::InvitationMismatch);
            }
            invite
        } else {
            configured
                .invitations
                .lock()
                .await
                .by_host
                .get(&host_id)
                .cloned()
                .ok_or(RemoraLinkError::PairingCodeRequired)?
        };
        let enrollment = entry
            .enrollment
            .ok_or(RemoraLinkError::InvitationMismatch)?;
        let started = Instant::now();
        loop {
            let outcome = configured
                .lifecycle
                .enroll(
                    &invite,
                    enrollment.display_name.clone(),
                    enrollment.selected_runtime_ids.clone(),
                    enrollment.requested_scopes.clone(),
                )
                .await?;
            let projected = project_pairing_outcome(&host_id, outcome);
            if supplied_code
                && matches!(
                    projected,
                    AppRemoraLinkPairingOutcome::AwaitingHostApproval { .. }
                )
            {
                // Retain a re-scanned invitation only after the lifecycle has
                // authenticated it against the staged public journal. A code
                // for the same endpoint but a different invitation must not
                // poison the process-local recovery cache.
                configured
                    .invitations
                    .lock()
                    .await
                    .by_host
                    .insert(host_id.clone(), invite.clone());
            }
            if !matches!(
                projected,
                AppRemoraLinkPairingOutcome::AwaitingHostApproval { .. }
            ) || started.elapsed() >= AWAIT_APPROVAL_TIMEOUT
            {
                if matches!(
                    projected,
                    AppRemoraLinkPairingOutcome::Paired { .. }
                        | AppRemoraLinkPairingOutcome::AlreadyPaired { .. }
                ) {
                    configured.remove_invitation(&host_id).await;
                    let attacher = CanonicalRemoraLinkRuntimeSessionAttacher {
                        client: Arc::clone(&self.inner),
                        configured,
                    };
                    require_pairing_attachment(&attacher, &projected).await?;
                }
                return Ok(projected);
            }
            tokio::time::sleep(AWAIT_APPROVAL_INTERVAL).await;
        }
    }

    pub async fn remora_link_hosts(
        &self,
    ) -> Result<Vec<AppRemoraLinkHostSummary>, RemoraLinkError> {
        let _configuration = self.inner.remora_link_configuration.read().await;
        let configured = self.configured_remora_link()?;
        let mut hosts: Vec<_> = configured
            .journal
            .entries()
            .await?
            .into_iter()
            .map(project_host_summary)
            .collect();
        hosts.sort_by(|left, right| left.host_id.cmp(&right.host_id));
        Ok(hosts)
    }

    pub async fn reconnect_remora_link_host(
        &self,
        host_id: String,
        runtime_id: String,
        last_sequence: Option<u64>,
    ) -> Result<AppRemoraLinkReconnectResult, RemoraLinkError> {
        let _configuration = self.inner.remora_link_configuration.read().await;
        let configured = self.configured_remora_link()?;
        let outcome = configured
            .lifecycle
            .reconnect(&host_id, runtime_id, last_sequence)
            .await?;
        let attachment = configured
            .host
            .take_runtime_attachment(
                &outcome.attachment_id,
                &outcome.host_id,
                &outcome.runtime_id,
            )
            .await
            .map_err(|_| RemoraLinkError::RuntimeUnavailable)?;
        let target_runtime_id = outcome.runtime_id.clone();
        let connected = self
            .inner
            .connect_remora_link_runtime(
                Arc::clone(&configured),
                &host_id,
                true,
                Some(PreparedRuntimeAttachmentV2 {
                    outcome,
                    attachment,
                }),
            )
            .await
            .map_err(|_| RemoraLinkError::RuntimeUnavailable)?;
        connected
            .connected_results
            .into_iter()
            .find(|result| result.runtime_id == target_runtime_id)
            .ok_or(RemoraLinkError::RuntimeUnavailable)
    }

    pub async fn reconnect_all_remora_link_hosts(
        &self,
    ) -> Result<AppRemoraLinkReconnectBatch, RemoraLinkError> {
        let deadline = tokio::time::Instant::now() + BATCH_TIMEOUT;
        let _configuration =
            tokio::time::timeout_at(deadline, self.inner.remora_link_configuration.read())
                .await
                .map_err(|_| RemoraLinkError::Cancelled)?;
        let configured = self.configured_remora_link()?;
        let attacher = Arc::new(CanonicalRemoraLinkRuntimeSessionAttacher {
            client: Arc::clone(&self.inner),
            configured: Arc::clone(&configured),
        });
        reconnect_all(configured, attacher, Vec::new(), deadline, None).await
    }

    /// Restart one explicitly granted runtime using the durable, at-most-once
    /// command sequence. An ambiguous result stays blocked until the operator
    /// acknowledges it through `acknowledge_remora_link_unknown_restart`.
    pub async fn restart_remora_link_runtime(
        &self,
        host_id: String,
        runtime_id: String,
    ) -> Result<AppRemoraLinkRestartOutcome, RemoraLinkError> {
        let _configuration = self.inner.remora_link_configuration.read().await;
        let configured = self.configured_remora_link()?;
        let outcome = configured.lifecycle.restart(&host_id, runtime_id).await?;
        self.inner.disconnect_remora_link_session(&host_id).await;
        Ok(match outcome {
            RestartOutcomeV2::Succeeded { command_sequence } => {
                AppRemoraLinkRestartOutcome::Succeeded { command_sequence }
            }
            RestartOutcomeV2::OutcomeUnknown { command_sequence } => {
                AppRemoraLinkRestartOutcome::OutcomeUnknown { command_sequence }
            }
        })
    }

    /// Confirm that the operator inspected an ambiguous restart externally and
    /// accepts issuing a new sequence on a later explicit restart request.
    pub async fn acknowledge_remora_link_unknown_restart(
        &self,
        host_id: String,
    ) -> Result<u64, RemoraLinkError> {
        let _configuration = self.inner.remora_link_configuration.read().await;
        let configured = self.configured_remora_link()?;
        configured
            .lifecycle
            .acknowledge_unknown_restart(&host_id)
            .await
            .map_err(Into::into)
    }

    /// Cancel a staged or pending enrollment without treating it as an
    /// enrolled-host revocation or a user-requested local forget.
    pub async fn cancel_remora_link_pairing(
        &self,
        host_id: String,
    ) -> Result<AppRemoraLinkPairingCancellationOutcome, RemoraLinkError> {
        let _configuration = self.inner.remora_link_configuration.read().await;
        let configured = self.configured_remora_link()?;
        let outcome =
            project_pairing_cancellation(configured.lifecycle.cancel_enrollment(&host_id).await?)?;
        // A successfully staged rollback (including an explicitly ambiguous
        // terminal outcome) no longer needs process-local invitation authority.
        configured.remove_invitation(&host_id).await;
        Ok(outcome)
    }

    pub async fn revoke_remora_link_host(
        &self,
        host_id: String,
    ) -> Result<AppRemoraLinkRevocationOutcome, RemoraLinkError> {
        let _configuration = self.inner.remora_link_configuration.read().await;
        let configured = self.configured_remora_link()?;
        let outcome = complete_remora_link_host_mutation(
            self.inner.as_ref(),
            &host_id,
            configured.lifecycle.revoke(&host_id),
        )
        .await;
        configured.remove_invitation(&host_id).await;
        drop(_configuration);
        self.inner.retire_paired_relay(&host_id).await;
        let outcome = outcome?;
        match outcome {
            MutationOutcomeV2::Revoked => Ok(AppRemoraLinkRevocationOutcome::Revoked),
            MutationOutcomeV2::OutcomeUnknown => Ok(AppRemoraLinkRevocationOutcome::OutcomeUnknown),
            MutationOutcomeV2::RolledBack => Err(RemoraLinkError::ProtocolViolation),
        }
    }

    pub async fn forget_remora_link_host(
        &self,
        host_id: String,
    ) -> Result<AppRemoraLinkForgetResult, RemoraLinkError> {
        let _configuration = self.inner.remora_link_configuration.read().await;
        let configured = self.configured_remora_link()?;
        let outcome = complete_remora_link_host_mutation(
            self.inner.as_ref(),
            &host_id,
            configured.lifecycle.forget(&host_id),
        )
        .await;
        configured.remove_invitation(&host_id).await;
        drop(_configuration);
        self.inner.retire_paired_relay(&host_id).await;
        let outcome = outcome?;
        Ok(outcome.into())
    }

    pub async fn notify_remora_link_network_change(&self) -> Result<(), RemoraLinkError> {
        let _configuration = self.inner.remora_link_configuration.read().await;
        let configured = self.configured_remora_link()?;
        configured.host.network_change().await;
        Ok(())
    }

    /// Re-evaluate Iroh paths, discard stale runtime streams, replay durable
    /// cleanup/revocation work, then reconnect every enrolled runtime.
    pub async fn remora_link_long_resume(
        &self,
    ) -> Result<AppRemoraLinkReconnectBatch, RemoraLinkError> {
        let deadline = tokio::time::Instant::now() + BATCH_TIMEOUT;
        let _configuration =
            tokio::time::timeout_at(deadline, self.inner.remora_link_configuration.write())
                .await
                .map_err(|_| RemoraLinkError::Cancelled)?;
        let configured = self.configured_remora_link()?;
        tokio::time::timeout_at(deadline, configured.host.network_change())
            .await
            .map_err(|_| RemoraLinkError::Cancelled)?;
        // Stop canonical workers before invalidating their host streams so
        // long-resume cannot race a hot runtime reconnect against the one
        // forced cold rebuild scheduled below.
        self.inner.disconnect_all_remora_link_sessions().await;
        tokio::time::timeout_at(deadline, configured.host.close_all_runtime_sessions())
            .await
            .map_err(|_| RemoraLinkError::Cancelled)?;
        let entries = tokio::time::timeout_at(deadline, configured.journal.entries())
            .await
            .map_err(|_| RemoraLinkError::Cancelled)??;
        let reconnect_fallback = entries.clone();
        let recovery_failures = recover_all(Arc::clone(&configured), entries, deadline).await;
        let attacher = Arc::new(CanonicalRemoraLinkRuntimeSessionAttacher {
            client: Arc::clone(&self.inner),
            configured: Arc::clone(&configured),
        });
        reconnect_all(
            configured,
            attacher,
            recovery_failures,
            deadline,
            Some(reconnect_fallback),
        )
        .await
    }
}

impl AppClient {
    fn configured_remora_link(&self) -> Result<Arc<ConfiguredRemoraLink>, RemoraLinkError> {
        remora_link_read(&self.inner.remora_link)
            .clone()
            .ok_or(RemoraLinkError::NotConfigured)
    }
}

impl crate::MobileClient {
    /// Open one authenticated v2 shell attachment without registering an
    /// app-server session. The durable v2 journal remains the sole authority:
    /// `reconnect` rejects unknown hosts, unselected shell runtimes, and
    /// credentials without `ConnectRuntime` before any attachment is taken.
    pub(crate) async fn open_remora_link_shell_transport(
        &self,
        host_id: &str,
    ) -> Result<crate::terminal::remote_shell::RemoteShellTransport, crate::terminal::TerminalError>
    {
        let _configuration = self.remora_link_configuration.read().await;
        let configured = remora_link_read(&self.remora_link).clone().ok_or_else(|| {
            crate::terminal::TerminalError::Backend {
                detail: "Remora Link v2 is not configured".to_string(),
            }
        })?;
        let shell_generation = configured.host.shell_connections.claim_generation(host_id);
        let outcome = configured
            .lifecycle
            .reconnect(host_id, "shell".to_string(), None)
            .await
            .map_err(map_shell_lifecycle_error)?;
        if outcome.host_id != host_id || outcome.runtime_id != "shell" {
            return Err(crate::terminal::TerminalError::Backend {
                detail: "Remora Link shell attachment identity mismatch".to_string(),
            });
        }
        let attachment = configured
            .host
            .take_runtime_attachment(
                &outcome.attachment_id,
                &outcome.host_id,
                &outcome.runtime_id,
            )
            .await
            .map_err(|error| crate::terminal::TerminalError::Backend {
                detail: format!("claiming Remora Link shell attachment: {error}"),
            })?;
        let (connection, send, recv) = attachment.into_parts();
        let connection: Arc<dyn crate::terminal::remote_shell::RemoteShellConnection> =
            Arc::new(RemoraLinkShellConnectionV2 { connection });
        let connection = configured
            .host
            .shell_connections
            .register(host_id, shell_generation, connection)
            .map_err(|()| crate::terminal::TerminalError::Backend {
                detail: "Remora Link shell attachment was invalidated by host cleanup".to_string(),
            })?;
        Ok(crate::terminal::remote_shell::RemoteShellTransport::new(
            tokio::io::join(recv, send),
            connection,
        ))
    }

    async fn disconnect_remora_link_session(&self, host_id: &str) {
        if let Some(configured) = remora_link_read(&self.remora_link).clone() {
            configured.close_shells_for_host(host_id);
        }
        self.replace_existing_session(host_id).await;
        project_remora_link_disconnected(&self.app_store, host_id);
    }

    pub(crate) async fn disconnect_all_remora_link_sessions(&self) {
        if let Some(configured) = remora_link_read(&self.remora_link).clone() {
            configured.host.shell_connections.close_all();
        }
        let mut host_ids = self
            .sessions_read()
            .keys()
            .filter(|server_id| server_id.starts_with("remora-link:"))
            .cloned()
            .collect::<HashSet<_>>();
        host_ids.extend(
            self.app_store
                .snapshot()
                .servers
                .keys()
                .filter(|server_id| server_id.starts_with("remora-link:"))
                .cloned(),
        );
        for host_id in host_ids {
            self.disconnect_remora_link_session(&host_id).await;
        }
    }

    /// Atomically build one canonical multiplexed coding-runtime session for
    /// an enrolled Remora Link v2 host.
    ///
    /// The v2 journal is the only cold-restore authority. Runtime framing is
    /// obtained from a freshly authenticated `list_agents` round, and each
    /// successful runtime owns exactly one `RemoteTransport` reconnect path.
    /// Shell is deliberately excluded from this app-server session.
    async fn connect_remora_link_runtime(
        &self,
        configured: Arc<ConfiguredRemoraLink>,
        host_id: &str,
        force_cold_rebuild: bool,
        mut prepared_attachment: Option<PreparedRuntimeAttachmentV2>,
    ) -> Result<RemoraLinkRuntimeConnectOutcome, TransportError> {
        let connect_lock = configured.session_connect_lock(host_id).await;
        let _connect = connect_lock.lock().await;

        let entry = configured
            .journal
            .load(host_id)
            .await
            .map_err(|error| TransportError::ConnectionFailed(error.to_string()))?
            .ok_or_else(|| {
                TransportError::ConnectionFailed("Remora Link host is not paired".into())
            })?;
        if !matches!(entry.phase, JournalPhaseV2::Enrolled) {
            return Err(TransportError::ConnectionFailed(
                "Remora Link host is not ready".into(),
            ));
        }
        let credential = entry.credential.as_ref().ok_or_else(|| {
            TransportError::ConnectionFailed("Remora Link journal is incomplete".into())
        })?;
        let config = ServerConfig {
            server_id: host_id.to_string(),
            display_name: entry
                .binding
                .host_display_name
                .clone()
                .unwrap_or_else(|| "Remote Host".to_string()),
            host: entry.binding.node_id.clone(),
            port: 0,
            websocket_url: Some("ws://remora-link-v2/runtime".to_string()),
            is_local: false,
            tls: false,
        };
        let unavailable_runtime_infos = credential
            .selected_runtime_ids
            .iter()
            .filter(|runtime_id| !is_shell_runtime(runtime_id))
            .map(|runtime_id| AgentRuntimeInfo {
                kind: runtime_id.clone(),
                name: runtime_id.clone(),
                display_name: runtime_id.clone(),
                available: false,
            })
            .collect::<Vec<_>>();
        if unavailable_runtime_infos.is_empty() {
            return Ok(RemoraLinkRuntimeConnectOutcome {
                server_id: host_id.to_string(),
                connected_runtime_ids: Vec::new(),
                unavailable_runtime_ids: Vec::new(),
                connected_results: Vec::new(),
            });
        }
        let agents = match configured.lifecycle.list_agents(host_id).await {
            Ok(agents) => agents,
            Err(error) => {
                self.app_store
                    .upsert_server(&config, ServerHealthSnapshot::Disconnected);
                self.app_store
                    .update_server_agent_runtimes(host_id, unavailable_runtime_infos);
                return Err(TransportError::ConnectionFailed(error.to_string()));
            }
        };
        let runtime_plan = remora_link_runtime_plan(&credential.selected_runtime_ids, &agents);

        self.agent_metadata.upsert_all(
            agents
                .iter()
                .filter(|agent| !is_shell_runtime(&agent.name))
                .map(project_remora_link_agent_metadata),
        );

        let selected_runtime_ids = runtime_plan
            .iter()
            .map(|runtime| runtime.runtime_id.clone())
            .collect::<HashSet<_>>();
        let all_selected_advertised = runtime_plan
            .iter()
            .all(|runtime| runtime.advertised_available && runtime.wire.is_some());
        let existing = { self.sessions_read().get(host_id).cloned() };
        if let Some(existing) = existing {
            if existing.reconnect_health_state().has_connecting_runtime
                && !existing
                    .wait_for_runtime_reconnects_to_settle(Duration::from_secs(30))
                    .await
            {
                return Err(TransportError::ConnectionFailed(
                    "Remora Link runtime recovery is still in progress".into(),
                ));
            }
            let state = existing.reconnect_health_state();
            let available = existing
                .available_runtime_kinds()
                .into_iter()
                .collect::<HashSet<_>>();
            if !force_cold_rebuild
                && all_selected_advertised
                && available == selected_runtime_ids
                && !state.has_degraded_runtime
            {
                let mut connected_runtime_ids = available.into_iter().collect::<Vec<_>>();
                connected_runtime_ids.sort();
                return Ok(RemoraLinkRuntimeConnectOutcome {
                    server_id: host_id.to_string(),
                    connected_runtime_ids,
                    unavailable_runtime_ids: Vec::new(),
                    connected_results: Vec::new(),
                });
            }
            if !existing.try_claim_cold_repair(state.generation) {
                return Err(TransportError::ConnectionFailed(
                    "Remora Link runtime recovery changed concurrently".into(),
                ));
            }
        }

        // Stop any prior host session before creating new runtime workers.
        // This ensures there is never a legacy/saved-server reconnect path or
        // a second v2 worker competing for the same runtime.
        self.replace_existing_session(host_id).await;
        self.app_store
            .upsert_server(&config, ServerHealthSnapshot::Connecting);
        let mut projection_guard = RemoraLinkConnectingProjectionGuard::new(
            Arc::clone(&self.app_store),
            host_id.to_string(),
        );
        let (_, args) = remote_connect_args(&config);

        let mut runtime_resources = Vec::new();
        let mut runtime_infos = runtime_plan
            .iter()
            .map(|runtime| AgentRuntimeInfo {
                kind: runtime.runtime_id.clone(),
                name: runtime.runtime_id.clone(),
                display_name: runtime.display_name.clone(),
                available: false,
            })
            .collect::<Vec<_>>();
        let mut connected_runtime_ids = Vec::new();
        let mut connected_results = Vec::new();

        for runtime in runtime_plan {
            let Some(wire) = runtime.wire.filter(|_| runtime.advertised_available) else {
                continue;
            };
            let reconnect_transport = Arc::new(RemoraLinkRemoteTransportV2::new(
                Arc::clone(&configured),
                Arc::clone(&self.app_store),
                host_id.to_string(),
                runtime.runtime_id.clone(),
                wire,
            ));
            let prepared = prepared_attachment
                .take_if(|prepared| prepared.outcome.runtime_id == runtime.runtime_id);
            let attached = async {
                let (outcome, attachment) = match prepared {
                    Some(prepared) => (prepared.outcome, Some(prepared.attachment)),
                    None => (
                        configured
                            .lifecycle
                            .reconnect(host_id, runtime.runtime_id.clone(), None)
                            .await
                            .map_err(|error| TransportError::ConnectionFailed(error.to_string()))?,
                        None,
                    ),
                };
                let result = AppRemoraLinkReconnectResult::from(outcome.clone());
                let attached = match attachment {
                    Some(attachment) => {
                        reconnect_transport
                            .connect_taken_attachment(outcome, attachment, args.clone())
                            .await
                    }
                    None => {
                        reconnect_transport
                            .connect_attachment(outcome, args.clone())
                            .await
                    }
                }?;
                let replay_outcome = reconnect_transport.take_replay_outcome();
                reconnect_transport
                    .reconcile_replay(&attached.client, replay_outcome)
                    .await?;
                Ok::<_, TransportError>((attached, result))
            }
            .await;
            let (reconnected, result) = match attached {
                Ok(reconnected) => reconnected,
                Err(error) => {
                    warn!(
                        server_id = host_id,
                        runtime = runtime.runtime_id,
                        %error,
                        "Remora Link runtime attach failed"
                    );
                    continue;
                }
            };
            if let Some(info) = runtime_infos
                .iter_mut()
                .find(|info| info.kind == runtime.runtime_id)
            {
                info.available = true;
            }
            connected_runtime_ids.push(runtime.runtime_id.clone());
            connected_results.push(result);
            let transport: Arc<dyn RemoteTransport> = reconnect_transport;
            runtime_resources.push(RuntimeRemoteSessionResource {
                runtime_kind: runtime.runtime_id,
                client: reconnected.client,
                transport: Some(transport),
                keepalive: reconnected.keepalive,
            });
        }

        let mut unavailable_runtime_ids = runtime_infos
            .iter()
            .filter(|runtime| !runtime.available)
            .map(|runtime| runtime.kind.clone())
            .collect::<Vec<_>>();
        connected_runtime_ids.sort();
        unavailable_runtime_ids.sort();
        if runtime_resources.is_empty() {
            self.app_store
                .update_server_agent_runtimes(host_id, runtime_infos);
            return Err(TransportError::ConnectionFailed(
                "no selected Remora Link coding runtime is available".to_string(),
            ));
        }

        let session = match ServerSession::connect_remote_multiplexed_with_unavailable(
            config,
            runtime_resources,
            unavailable_runtime_ids.clone(),
            RemoteSessionExtras::default(),
        )
        .await
        {
            Ok(session) => Arc::new(session),
            Err(error) => {
                self.app_store
                    .update_server_agent_runtimes(host_id, runtime_infos);
                self.app_store
                    .update_server_health(host_id, ServerHealthSnapshot::Disconnected);
                return Err(error);
            }
        };
        self.attach_remote_session(host_id, session, runtime_infos);
        projection_guard.disarm();
        Ok(RemoraLinkRuntimeConnectOutcome {
            server_id: host_id.to_string(),
            connected_runtime_ids,
            unavailable_runtime_ids,
            connected_results,
        })
    }
}

fn is_shell_runtime(runtime_id: &str) -> bool {
    runtime_id == "shell"
}

fn validate_coding_runtime_scope_selection(
    selected_runtime_ids: &[String],
    requested_scopes: &[AppRemoraLinkScope],
) -> Result<(), RemoraLinkError> {
    if selected_runtime_ids
        .iter()
        .any(|runtime_id| !is_shell_runtime(runtime_id))
        && ![
            AppRemoraLinkScope::InspectRuntimes,
            AppRemoraLinkScope::ConnectRuntime,
            AppRemoraLinkScope::SelfRevoke,
        ]
        .iter()
        .all(|scope| requested_scopes.contains(scope))
    {
        return Err(RemoraLinkError::InvalidSelection);
    }
    Ok(())
}

fn remora_link_runtime_plan(
    selected_runtime_ids: &[String],
    authenticated_agents: &[AgentInfoV2],
) -> Vec<RemoraLinkRuntimePlanEntry> {
    selected_runtime_ids
        .iter()
        .filter(|runtime_id| !is_shell_runtime(runtime_id))
        .map(|runtime_id| {
            let agent = authenticated_agents
                .iter()
                .find(|agent| agent.name == *runtime_id);
            RemoraLinkRuntimePlanEntry {
                runtime_id: runtime_id.clone(),
                display_name: agent
                    .map(|agent| agent.display_name.clone())
                    .unwrap_or_else(|| runtime_id.clone()),
                wire: agent.map(|agent| agent.wire),
                advertised_available: agent.is_some_and(|agent| agent.available),
            }
        })
        .collect()
}

fn project_remora_link_agent_metadata(agent: &AgentInfoV2) -> crate::store::AppAgentMetadata {
    crate::store::AppAgentMetadata {
        name: agent.name.clone(),
        display_name: agent.display_name.clone(),
        presentation: agent.presentation.as_ref().map(|presentation| {
            crate::store::AppAgentPresentation {
                title: presentation.title.clone(),
                is_beta: presentation.is_beta,
                sort_order: presentation.sort_order,
                description: presentation.description.clone(),
                aliases: presentation.aliases.clone(),
            }
        }),
        capabilities: agent.capabilities.as_ref().map(|capabilities| {
            crate::store::AppAgentCapabilities {
                locks_reasoning_effort_after_activity: capabilities
                    .locks_reasoning_effort_after_activity,
                visible_modes: capabilities.visible_modes.clone(),
                supports_ssh_bridge: capabilities.supports_ssh_bridge,
                uses_direct_codex_port: capabilities.uses_direct_codex_port,
                supports_thread_permission_overrides: capabilities
                    .supports_thread_permission_overrides,
                reports_effective_thread_permissions: capabilities
                    .reports_effective_thread_permissions,
            }
        }),
    }
}

async fn build_configuration(
    journal: Box<dyn AppRemoraLinkJournalBackend>,
    transport_identity: Box<dyn AppRemoraLinkTransportIdentityBackend>,
    device_keys: Box<dyn AppRemoraLinkDeviceKeyBackend>,
) -> Result<Arc<ConfiguredRemoraLink>, RemoraLinkError> {
    let journal = Arc::new(NativeRemoraLinkJournal::new(Arc::from(journal)));
    journal.entries().await?;
    let host = Arc::new(
        IrohRemoraLinkHost::bind(Arc::from(transport_identity))
            .await
            .map_err(RemoraLinkError::from)?,
    );
    host.start_attachment_reaper();
    let custody = Arc::new(NativeDeviceKeyCustody {
        backend: Arc::from(device_keys),
    });
    let lifecycle = Arc::new(PairingLifecycleV2::new(
        Arc::clone(&host) as Arc<dyn HostPortV2>,
        Arc::clone(&journal) as Arc<dyn JournalPortV2>,
        custody as Arc<dyn CredentialCustodyPortV2>,
        Arc::new(SecureEntropy),
    ));
    Ok(Arc::new(ConfiguredRemoraLink {
        lifecycle,
        journal,
        host,
        invitations: Mutex::new(InvitationCache::default()),
        session_connect_locks: Mutex::new(HashMap::new()),
    }))
}

async fn cold_restore_configured_remora_link(
    client: Arc<crate::MobileClient>,
    configured: Arc<ConfiguredRemoraLink>,
    deadline: tokio::time::Instant,
) -> Result<AppRemoraLinkReconnectBatch, RemoraLinkError> {
    let entries = tokio::time::timeout_at(deadline, configured.journal.entries())
        .await
        .map_err(|_| RemoraLinkError::Cancelled)??;
    let reconnect_fallback = entries.clone();
    let recovery_failures = recover_all(Arc::clone(&configured), entries, deadline).await;
    let attacher = Arc::new(CanonicalRemoraLinkRuntimeSessionAttacher {
        client,
        configured: Arc::clone(&configured),
    });
    reconnect_all(
        configured,
        attacher,
        recovery_failures,
        deadline,
        Some(reconnect_fallback),
    )
    .await
}

async fn reconnect_all(
    configured: Arc<ConfiguredRemoraLink>,
    attacher: Arc<dyn RemoraLinkRuntimeSessionAttacher>,
    mut recovery_failures: Vec<AppRemoraLinkHostFailure>,
    deadline: tokio::time::Instant,
    timeout_fallback: Option<Vec<PairingJournalEntryV2>>,
) -> Result<AppRemoraLinkReconnectBatch, RemoraLinkError> {
    let entries = match tokio::time::timeout_at(deadline, configured.journal.entries()).await {
        Ok(entries) => entries?,
        Err(_) => {
            let Some(entries) = timeout_fallback else {
                return Err(RemoraLinkError::Cancelled);
            };
            return Ok(cancelled_reconnect_batch(entries, recovery_failures));
        }
    };
    let mut attempts =
        restore_runtime_sessions(attacher, runtime_restore_work(&entries), deadline).await;
    attempts.sort_by(|left, right| reconnect_attempt_key(left).cmp(&reconnect_attempt_key(right)));
    recovery_failures.sort_by(|left, right| left.host_id.cmp(&right.host_id));
    Ok(AppRemoraLinkReconnectBatch {
        attempts,
        recovery_failures,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RemoraLinkRuntimeRestoreWork {
    host_id: String,
    runtime_ids: Vec<String>,
}

fn runtime_restore_work(entries: &[PairingJournalEntryV2]) -> Vec<RemoraLinkRuntimeRestoreWork> {
    let mut work = Vec::new();
    for entry in entries {
        if !matches!(entry.phase, JournalPhaseV2::Enrolled) {
            continue;
        }
        let Some(credential) = entry.credential.as_ref() else {
            continue;
        };
        let mut runtime_ids = credential
            .selected_runtime_ids
            .iter()
            .filter(|runtime_id| !is_shell_runtime(runtime_id))
            .cloned()
            .collect::<Vec<_>>();
        runtime_ids.sort();
        if !runtime_ids.is_empty() {
            work.push(RemoraLinkRuntimeRestoreWork {
                host_id: entry.binding.host_id.clone(),
                runtime_ids,
            });
        }
    }
    work.sort_by(|left, right| left.host_id.cmp(&right.host_id));
    work
}

async fn restore_runtime_sessions(
    attacher: Arc<dyn RemoraLinkRuntimeSessionAttacher>,
    work: Vec<RemoraLinkRuntimeRestoreWork>,
    deadline: tokio::time::Instant,
) -> Vec<AppRemoraLinkReconnectAttempt> {
    let mut remaining = work
        .iter()
        .map(|work| (work.host_id.clone(), work.runtime_ids.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut next = work.into_iter();
    let mut in_flight = FuturesUnordered::new();
    for work in next.by_ref().take(BATCH_CONCURRENCY) {
        in_flight.push(restore_runtime_session(Arc::clone(&attacher), work));
    }

    let mut attempts = Vec::new();
    while !in_flight.is_empty() {
        let completed = tokio::select! {
            biased;
            _ = tokio::time::sleep_until(deadline) => None,
            completed = in_flight.next() => completed,
        };
        let Some((work, result)) = completed else {
            break;
        };
        remaining.remove(&work.host_id);
        attempts.extend(project_runtime_restore_result(&work, result));
        if let Some(work) = next.next() {
            in_flight.push(restore_runtime_session(Arc::clone(&attacher), work));
        }
    }
    drop(in_flight);
    attempts.extend(remaining.into_iter().flat_map(|(host_id, runtime_ids)| {
        runtime_ids
            .into_iter()
            .map(move |runtime_id| AppRemoraLinkReconnectAttempt::Failed {
                host_id: host_id.clone(),
                runtime_id,
                failure: AppRemoraLinkFailure::Cancelled,
            })
    }));
    attempts
}

async fn restore_runtime_session(
    attacher: Arc<dyn RemoraLinkRuntimeSessionAttacher>,
    work: RemoraLinkRuntimeRestoreWork,
) -> (
    RemoraLinkRuntimeRestoreWork,
    Result<RemoraLinkRuntimeConnectOutcome, TransportError>,
) {
    let result = attacher.attach_runtime_session(&work.host_id, true).await;
    (work, result)
}

fn project_runtime_restore_result(
    work: &RemoraLinkRuntimeRestoreWork,
    result: Result<RemoraLinkRuntimeConnectOutcome, TransportError>,
) -> Vec<AppRemoraLinkReconnectAttempt> {
    match result {
        Ok(outcome) => {
            let mut attempts = outcome
                .connected_results
                .into_iter()
                .map(|result| AppRemoraLinkReconnectAttempt::Connected { result })
                .collect::<Vec<_>>();
            let represented = attempts
                .iter()
                .map(|attempt| reconnect_attempt_key(attempt).1.to_string())
                .chain(outcome.unavailable_runtime_ids.iter().cloned())
                .collect::<HashSet<_>>();
            attempts.extend(
                outcome
                    .unavailable_runtime_ids
                    .into_iter()
                    .map(|runtime_id| AppRemoraLinkReconnectAttempt::Failed {
                        host_id: work.host_id.clone(),
                        runtime_id,
                        failure: AppRemoraLinkFailure::RuntimeUnavailable,
                    }),
            );
            attempts.extend(
                work.runtime_ids
                    .iter()
                    .filter(|runtime_id| !represented.contains(*runtime_id))
                    .cloned()
                    .map(|runtime_id| AppRemoraLinkReconnectAttempt::Failed {
                        host_id: work.host_id.clone(),
                        runtime_id,
                        failure: AppRemoraLinkFailure::RuntimeUnavailable,
                    }),
            );
            attempts
        }
        Err(_) => work
            .runtime_ids
            .iter()
            .cloned()
            .map(|runtime_id| AppRemoraLinkReconnectAttempt::Failed {
                host_id: work.host_id.clone(),
                runtime_id,
                failure: AppRemoraLinkFailure::RuntimeUnavailable,
            })
            .collect(),
    }
}

fn cancelled_reconnect_batch(
    entries: Vec<PairingJournalEntryV2>,
    mut recovery_failures: Vec<AppRemoraLinkHostFailure>,
) -> AppRemoraLinkReconnectBatch {
    let attempts = runtime_restore_work(&entries)
        .into_iter()
        .flat_map(|work| {
            work.runtime_ids.into_iter().map(move |runtime_id| {
                AppRemoraLinkReconnectAttempt::Failed {
                    host_id: work.host_id.clone(),
                    runtime_id,
                    failure: AppRemoraLinkFailure::Cancelled,
                }
            })
        })
        .collect();
    recovery_failures.sort_by(|left, right| left.host_id.cmp(&right.host_id));
    AppRemoraLinkReconnectBatch {
        attempts,
        recovery_failures,
    }
}

async fn recover_all(
    configured: Arc<ConfiguredRemoraLink>,
    entries: Vec<PairingJournalEntryV2>,
    deadline: tokio::time::Instant,
) -> Vec<AppRemoraLinkHostFailure> {
    let mut remaining: Vec<_> = entries
        .into_iter()
        .map(|entry| entry.binding.host_id)
        .collect();
    let mut next = remaining.clone().into_iter();
    let mut in_flight = FuturesUnordered::new();
    for _ in 0..BATCH_CONCURRENCY {
        let Some(host_id) = next.next() else {
            break;
        };
        in_flight.push(recover_one(Arc::clone(&configured), host_id));
    }
    let mut failures = Vec::new();
    while !in_flight.is_empty() {
        let completed = tokio::select! {
            biased;
            _ = tokio::time::sleep_until(deadline) => None,
            completed = in_flight.next() => completed,
        };
        let Some((host_id, result)) = completed else {
            break;
        };
        remaining.retain(|candidate| candidate != &host_id);
        if let Err(error) = result {
            failures.push(AppRemoraLinkHostFailure {
                host_id,
                failure: error.into(),
            });
        }
        if let Some(host_id) = next.next() {
            in_flight.push(recover_one(Arc::clone(&configured), host_id));
        }
    }
    drop(in_flight);
    failures.extend(
        remaining
            .into_iter()
            .map(|host_id| AppRemoraLinkHostFailure {
                host_id,
                failure: AppRemoraLinkFailure::Cancelled,
            }),
    );
    failures
}

async fn recover_one(
    configured: Arc<ConfiguredRemoraLink>,
    host_id: String,
) -> (
    String,
    Result<crate::remote_host_pairing::remora_link_v2::RecoveryOutcomeV2, LifecycleErrorV2>,
) {
    let result = configured.lifecycle.recover(&host_id).await;
    (host_id, result)
}

fn reconnect_attempt_key(attempt: &AppRemoraLinkReconnectAttempt) -> (&str, &str) {
    match attempt {
        AppRemoraLinkReconnectAttempt::Connected { result } => {
            (&result.host_id, &result.runtime_id)
        }
        AppRemoraLinkReconnectAttempt::Failed {
            host_id,
            runtime_id,
            ..
        } => (host_id, runtime_id),
    }
}

// MARK: - Opaque journal adapter

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JournalEnvelope {
    schema_version: u32,
    entries: Vec<PairingJournalEntryV2>,
}

struct NativeRemoraLinkJournal {
    backend: Arc<dyn AppRemoraLinkJournalBackend>,
}

impl NativeRemoraLinkJournal {
    fn new(backend: Arc<dyn AppRemoraLinkJournalBackend>) -> Self {
        Self { backend }
    }

    async fn load_snapshot(
        &self,
    ) -> Result<
        Option<(AppRemoraLinkJournalSnapshot, Vec<PairingJournalEntryV2>)>,
        JournalPortErrorV2,
    > {
        let loaded = tokio::time::timeout(NATIVE_CALLBACK_TIMEOUT, self.backend.load())
            .await
            .map_err(|_| JournalPortErrorV2::Unavailable)?;
        match loaded {
            AppRemoraLinkJournalLoad::Missing => Ok(None),
            AppRemoraLinkJournalLoad::Unavailable => Err(JournalPortErrorV2::Unavailable),
            AppRemoraLinkJournalLoad::Loaded { snapshot } => {
                if snapshot.revision == 0 || snapshot.payload.len() > MAX_JOURNAL_BYTES {
                    return Err(JournalPortErrorV2::Corrupt);
                }
                let entries = decode_journal(&snapshot.payload)?;
                if entries
                    .iter()
                    .any(|entry| entry.revision > snapshot.revision)
                {
                    return Err(JournalPortErrorV2::Corrupt);
                }
                Ok(Some((snapshot, entries)))
            }
        }
    }

    async fn load_state(
        &self,
    ) -> Result<(Option<u64>, Vec<PairingJournalEntryV2>), JournalPortErrorV2> {
        match self.load_snapshot().await? {
            Some((snapshot, entries)) => Ok((Some(snapshot.revision), entries)),
            None => Ok((None, Vec::new())),
        }
    }

    async fn entries(&self) -> Result<Vec<PairingJournalEntryV2>, RemoraLinkError> {
        self.load_state()
            .await
            .map(|(_, entries)| entries)
            .map_err(Into::into)
    }

    async fn load_entry(
        &self,
        host_id: &str,
    ) -> Result<Option<PairingJournalEntryV2>, RemoraLinkError> {
        self.load(host_id).await.map_err(Into::into)
    }
}

#[async_trait]
impl JournalPortV2 for NativeRemoraLinkJournal {
    async fn load(
        &self,
        host_id: &str,
    ) -> Result<Option<PairingJournalEntryV2>, JournalPortErrorV2> {
        Ok(self
            .load_state()
            .await?
            .1
            .into_iter()
            .find(|entry| entry.binding.host_id == host_id))
    }

    async fn compare_and_swap(
        &self,
        host_id: &str,
        expected_revision: Option<u64>,
        replacement: PairingJournalEntryV2,
    ) -> Result<(), JournalPortErrorV2> {
        if replacement.binding.host_id != host_id || replacement.validate().is_err() {
            return Err(JournalPortErrorV2::Corrupt);
        }
        for _ in 0..MAX_JOURNAL_CAS_ATTEMPTS {
            let (outer_revision, mut entries) = self.load_state().await?;
            let position = entries
                .iter()
                .position(|entry| entry.binding.host_id == host_id);
            let actual_revision = position.map(|index| entries[index].revision);
            if actual_revision != expected_revision {
                return Err(JournalPortErrorV2::Conflict);
            }
            if let Some(index) = position {
                entries[index] = replacement.clone();
            } else {
                if entries.len() >= MAX_JOURNAL_HOSTS {
                    return Err(JournalPortErrorV2::Unavailable);
                }
                entries.push(replacement.clone());
            }
            entries.sort_by(|left, right| left.binding.host_id.cmp(&right.binding.host_id));
            let payload = encode_journal(entries)?;
            let revision = outer_revision
                .unwrap_or(0)
                .checked_add(1)
                .ok_or(JournalPortErrorV2::Unavailable)?;
            let submitted = AppRemoraLinkJournalSnapshot { revision, payload };
            let outcome = tokio::time::timeout(
                NATIVE_CALLBACK_TIMEOUT,
                self.backend
                    .compare_and_swap(outer_revision, submitted.clone()),
            )
            .await
            .map_err(|_| JournalPortErrorV2::Unavailable)?;
            match outcome {
                AppRemoraLinkJournalWriteOutcome::Stored => {
                    // A callback acknowledgement is not enough for a crash
                    // journal. Require the exact submitted generation and
                    // whole blob before a dependent network action. A later
                    // concurrent generation is conservatively a conflict: our
                    // durable transition happened, but this operation must
                    // reload and reconcile rather than assume its authority.
                    return match self.load_snapshot().await? {
                        Some((snapshot, _)) if snapshot == submitted => Ok(()),
                        Some(_) => Err(JournalPortErrorV2::Conflict),
                        None => Err(JournalPortErrorV2::Unavailable),
                    };
                }
                AppRemoraLinkJournalWriteOutcome::Conflict => continue,
                AppRemoraLinkJournalWriteOutcome::Unavailable => {
                    return Err(JournalPortErrorV2::Unavailable);
                }
            }
        }
        Err(JournalPortErrorV2::Conflict)
    }
}

fn decode_journal(payload: &[u8]) -> Result<Vec<PairingJournalEntryV2>, JournalPortErrorV2> {
    let envelope: JournalEnvelope =
        serde_json::from_slice(payload).map_err(|_| JournalPortErrorV2::Corrupt)?;
    if envelope.schema_version != JOURNAL_ENVELOPE_SCHEMA
        || envelope.entries.len() > MAX_JOURNAL_HOSTS
    {
        return Err(JournalPortErrorV2::Corrupt);
    }
    let mut seen = HashSet::with_capacity(envelope.entries.len());
    for entry in &envelope.entries {
        if entry.schema_version != JOURNAL_SCHEMA_VERSION
            || entry.validate().is_err()
            || !seen.insert(entry.binding.host_id.as_str())
        {
            return Err(JournalPortErrorV2::Corrupt);
        }
    }
    Ok(envelope.entries)
}

fn encode_journal(entries: Vec<PairingJournalEntryV2>) -> Result<Vec<u8>, JournalPortErrorV2> {
    let payload = serde_json::to_vec(&JournalEnvelope {
        schema_version: JOURNAL_ENVELOPE_SCHEMA,
        entries,
    })
    .map_err(|_| JournalPortErrorV2::Unavailable)?;
    if payload.len() > MAX_JOURNAL_BYTES {
        return Err(JournalPortErrorV2::Unavailable);
    }
    Ok(payload)
}

// MARK: - Native device-key adapter

struct NativeDeviceKeyCustody {
    backend: Arc<dyn AppRemoraLinkDeviceKeyBackend>,
}

#[async_trait]
impl CredentialCustodyPortV2 for NativeDeviceKeyCustody {
    async fn ensure_hardware_key(
        &self,
        host_id: &str,
    ) -> Result<HardwareKeyV2, CredentialPortError> {
        let key = tokio::time::timeout(
            NATIVE_CALLBACK_TIMEOUT,
            self.backend.ensure_hardware_key(host_id.to_string()),
        )
        .await
        .map_err(|_| CredentialPortError::Unavailable)??;
        validate_native_hardware_key(key)
    }

    async fn load_hardware_key(
        &self,
        slot: &str,
    ) -> Result<Option<HardwareKeyV2>, CredentialPortError> {
        let loaded = tokio::time::timeout(
            NATIVE_CALLBACK_TIMEOUT,
            self.backend.load_hardware_key(slot.to_string()),
        )
        .await
        .map_err(|_| CredentialPortError::Unavailable)??;
        match loaded {
            AppRemoraLinkHardwareKeyLoad::Missing => Ok(None),
            AppRemoraLinkHardwareKeyLoad::Loaded { key } => {
                validate_native_hardware_key(key).map(Some)
            }
        }
    }

    async fn sign_message(
        &self,
        slot: &str,
        message: &[u8],
    ) -> Result<Vec<u8>, CredentialPortError> {
        let message = AppRelaySecretValue::new(message.to_vec());
        let signature = tokio::time::timeout(
            NATIVE_CALLBACK_TIMEOUT,
            self.backend.sign_message(slot.to_string(), message),
        )
        .await
        .map_err(|_| CredentialPortError::Unavailable)??;
        let mut signature = signature.into_bytes();
        let parsed = Signature::from_der(&signature).map_err(|_| {
            signature.zeroize();
            CredentialPortError::InvalidSignature
        })?;
        if parsed.to_der().as_bytes() != signature.as_slice() {
            signature.zeroize();
            return Err(CredentialPortError::InvalidSignature);
        }
        Ok(signature)
    }

    async fn delete_hardware_key(&self, slot: &str) -> Result<(), CredentialPortError> {
        tokio::time::timeout(
            NATIVE_CALLBACK_TIMEOUT,
            self.backend.delete_hardware_key(slot.to_string()),
        )
        .await
        .map_err(|_| CredentialPortError::Unavailable)??;
        Ok(())
    }
}

fn validate_native_hardware_key(
    key: AppRemoraLinkHardwareKey,
) -> Result<HardwareKeyV2, CredentialPortError> {
    if key.slot.is_empty()
        || key.slot.len() > 256
        || key.slot.chars().any(char::is_control)
        || key.public_key_sec1.len() != 65
        || key.public_key_sec1.first() != Some(&4)
    {
        return Err(CredentialPortError::Unavailable);
    }
    let public = PublicKey::from_sec1_bytes(&key.public_key_sec1)
        .map_err(|_| CredentialPortError::Unavailable)?;
    if public.to_encoded_point(false).as_bytes() != key.public_key_sec1 {
        return Err(CredentialPortError::Unavailable);
    }
    if !key_assurance_is_allowed(key.assurance, cfg!(debug_assertions)) {
        return Err(CredentialPortError::Unavailable);
    }
    Ok(HardwareKeyV2 {
        slot: key.slot,
        public_key: URL_SAFE_NO_PAD.encode(key.public_key_sec1),
    })
}

fn key_assurance_is_allowed(assurance: AppRemoraLinkKeyAssurance, debug_build: bool) -> bool {
    match assurance {
        AppRemoraLinkKeyAssurance::SecureEnclave
        | AppRemoraLinkKeyAssurance::StrongBox
        | AppRemoraLinkKeyAssurance::TrustedExecutionEnvironment
        | AppRemoraLinkKeyAssurance::UnknownSecure => true,
        AppRemoraLinkKeyAssurance::SoftwareDebugOnly => debug_build,
    }
}

impl From<AppRemoraLinkDeviceKeyError> for CredentialPortError {
    fn from(value: AppRemoraLinkDeviceKeyError) -> Self {
        match value {
            AppRemoraLinkDeviceKeyError::Missing => Self::Missing,
            AppRemoraLinkDeviceKeyError::InvalidSignature => Self::InvalidSignature,
            AppRemoraLinkDeviceKeyError::Unavailable
            | AppRemoraLinkDeviceKeyError::Invalidated
            | AppRemoraLinkDeviceKeyError::HardwareUnavailable
            | AppRemoraLinkDeviceKeyError::Locked => Self::Unavailable,
        }
    }
}

struct SecureEntropy;

impl EntropyPortV2 for SecureEntropy {
    fn fresh_nonce(&self) -> [u8; 32] {
        let mut value = [0_u8; 32];
        OsRng.fill_bytes(&mut value);
        value
    }

    fn fresh_idempotency_key(&self, purpose: &'static str) -> String {
        let mut value = [0_u8; 32];
        OsRng.fill_bytes(&mut value);
        let encoded = URL_SAFE_NO_PAD.encode(value);
        value.zeroize();
        format!("remora-link-{purpose}-{encoded}")
    }
}

// MARK: - Iroh v2 host port

struct LiveExchange {
    started_at: Instant,
    host_id: String,
    runtime_id: Option<String>,
    request: RequestCorrelationV2,
    challenge: ResponseV2,
    authenticated_client_endpoint_id: String,
    connection: Connection,
    send: SendStream,
    recv: RecvStream,
}

struct LiveRuntimeSession {
    connection: Option<Connection>,
    send: Option<SendStream>,
    recv: Option<RecvStream>,
}

impl LiveRuntimeSession {
    fn new(connection: Connection, send: SendStream, recv: RecvStream) -> Self {
        Self {
            connection: Some(connection),
            send: Some(send),
            recv: Some(recv),
        }
    }

    fn into_parts(mut self) -> (Connection, SendStream, RecvStream) {
        (
            self.connection.take().expect("live runtime connection"),
            self.send.take().expect("live runtime send stream"),
            self.recv.take().expect("live runtime receive stream"),
        )
    }
}

impl Drop for LiveRuntimeSession {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.take() {
            connection.close(
                VarInt::from_u32(CLOSE_CODE),
                b"unclaimed Remora Link attachment closed",
            );
        }
    }
}

struct IrohRemoraLinkHost {
    endpoint: Endpoint,
    exchanges: std::sync::Mutex<HashMap<String, LiveExchange>>,
    attachments: Arc<Mutex<RetainedAttachmentRegistryV2<LiveRuntimeSession>>>,
    shell_connections: Arc<RemoraLinkShellConnectionRegistryV2>,
}

impl IrohRemoraLinkHost {
    fn exchanges(&self) -> std::sync::MutexGuard<'_, HashMap<String, LiveExchange>> {
        self.exchanges
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    async fn bind(
        identity: Arc<dyn AppRemoraLinkTransportIdentityBackend>,
    ) -> Result<Self, LifecycleErrorV2> {
        let mut candidate = vec![0_u8; 32];
        OsRng.fill_bytes(&mut candidate);
        let candidate = AppRelaySecretValue::new(candidate);
        let stored =
            tokio::time::timeout(NATIVE_CALLBACK_TIMEOUT, identity.load_or_create(candidate))
                .await
                .map_err(|_| LifecycleErrorV2::CredentialUnavailable)?
                .map_err(|_| LifecycleErrorV2::CredentialUnavailable)?;
        let mut stored = Zeroizing::new(stored.into_bytes());
        let mut key_bytes = Zeroizing::new(
            <[u8; 32]>::try_from(stored.as_slice())
                .map_err(|_| LifecycleErrorV2::CredentialUnavailable)?,
        );
        stored.zeroize();
        let secret_key = SecretKey::from_bytes(&key_bytes);
        key_bytes.zeroize();
        let transport = QuicTransportConfig::builder()
            .keep_alive_interval(Duration::from_secs(15))
            .build();
        let endpoint_builder = Endpoint::builder(iroh::endpoint::presets::N0)
            .transport_config(transport)
            .secret_key(secret_key);
        #[cfg(target_os = "android")]
        let endpoint_builder = endpoint_builder
            .dns_resolver(iroh::dns::DnsResolver::with_nameserver(
                std::net::SocketAddr::from(([8, 8, 8, 8], 53)),
            ))
            .ca_tls_config(iroh::tls::CaTlsConfig::embedded());
        let endpoint = endpoint_builder
            .bind()
            .await
            .map_err(|_| LifecycleErrorV2::HostUnavailable)?;
        Ok(Self {
            endpoint,
            exchanges: std::sync::Mutex::new(HashMap::new()),
            attachments: Arc::new(Mutex::new(RetainedAttachmentRegistryV2::new(
                MAX_RETAINED_ATTACHMENTS,
                MAX_RETAINED_ATTACHMENT_AGE,
            ))),
            shell_connections: Arc::new(RemoraLinkShellConnectionRegistryV2::default()),
        })
    }

    async fn shutdown(&self) {
        self.close_all_runtime_sessions().await;
        let exchanges = std::mem::take(&mut *self.exchanges());
        for (_, exchange) in exchanges {
            exchange
                .connection
                .close(VarInt::from_u32(CLOSE_CODE), b"Remora Link cleared");
        }
        self.endpoint.close().await;
    }

    fn start_attachment_reaper(self: &Arc<Self>) {
        let weak_host = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(ATTACHMENT_REAPER_INTERVAL).await;
                let Some(host) = weak_host.upgrade() else {
                    return;
                };
                host.attachments.lock().await.expire(Instant::now());
            }
        });
    }

    async fn network_change(&self) {
        if !self.endpoint.is_closed() {
            self.endpoint.network_change().await;
        }
    }

    async fn close_all_runtime_sessions(&self) {
        self.attachments.lock().await.clear();
        self.shell_connections.close_all();
    }

    async fn take_runtime_attachment(
        &self,
        attachment_id: &str,
        host_id: &str,
        runtime_id: &str,
    ) -> Result<LiveRuntimeSession, AttachmentCustodyErrorV2> {
        self.attachments
            .lock()
            .await
            .take(attachment_id, host_id, runtime_id, Instant::now())
    }
}

#[async_trait]
impl HostPortV2 for IrohRemoraLinkHost {
    async fn start_exchange(
        &self,
        route: &HostRouteV2,
        request: &RequestV2,
    ) -> Result<StartedExchangeV2, HostPortErrorV2> {
        let future = async {
            let remote_id = EndpointId::from_str(&route.node_id)
                .map_err(|_| HostPortErrorV2::ProtocolViolation)?;
            let mut address = EndpointAddr::new(remote_id);
            if let Some(relay) = route.relay_hint.as_deref() {
                address = address.with_relay_url(
                    RelayUrl::from_str(relay).map_err(|_| HostPortErrorV2::ProtocolViolation)?,
                );
            }
            // Awaiting `connect` completes the authenticated handshake. We
            // never request or accept replayable 0-RTT data.
            let connection = self
                .endpoint
                .connect(address, ALPN)
                .await
                .map_err(|_| HostPortErrorV2::Unavailable)?;
            let authenticated_host_endpoint_id = connection.remote_id().to_string();
            let authenticated_client_endpoint_id = self.endpoint.id().to_string();
            let (mut send, mut recv) = connection
                .open_bi()
                .await
                .map_err(|_| HostPortErrorV2::Unavailable)?;
            if recv.is_0rtt() {
                connection.close(VarInt::from_u32(CLOSE_CODE), b"0-RTT rejected");
                return Err(HostPortErrorV2::ProtocolViolation);
            }
            write_request_frame(&mut send, request)
                .await
                .map_err(map_control_error)?;
            let challenge = read_response_frame(&mut recv)
                .await
                .map_err(map_control_error)?;
            let exchange_id = random_id("exchange");
            let request_correlation = request
                .terminal_correlation()
                .map_err(|_| HostPortErrorV2::ProtocolViolation)?;
            let runtime_id = match request {
                RequestV2::Connect { agent, .. } => Some(agent.clone()),
                _ => None,
            };
            let host_id = format!("remora-link:{}", route.node_id);
            let mut exchanges = self.exchanges();
            exchanges.retain(|_, exchange| {
                let keep = exchange.host_id != host_id
                    && exchange.started_at.elapsed() < MAX_LIVE_EXCHANGE_AGE;
                if !keep {
                    exchange
                        .connection
                        .close(VarInt::from_u32(CLOSE_CODE), b"stale Remora Link exchange");
                }
                keep
            });
            if exchanges.len() >= MAX_LIVE_EXCHANGES {
                connection.close(VarInt::from_u32(CLOSE_CODE), b"exchange limit");
                return Err(HostPortErrorV2::Unavailable);
            }
            exchanges.insert(
                exchange_id.clone(),
                LiveExchange {
                    started_at: Instant::now(),
                    host_id,
                    runtime_id,
                    request: request_correlation,
                    challenge: challenge.clone(),
                    authenticated_client_endpoint_id: authenticated_client_endpoint_id.clone(),
                    connection,
                    send,
                    recv,
                },
            );
            drop(exchanges);
            Ok(StartedExchangeV2 {
                exchange_id,
                authenticated_host_endpoint_id,
                authenticated_client_endpoint_id,
                challenge_response: challenge,
            })
        };
        tokio::time::timeout(HOST_START_TIMEOUT, future)
            .await
            .map_err(|_| HostPortErrorV2::Unavailable)?
    }

    async fn finish_exchange(
        &self,
        exchange_id: &str,
        proof: &crate::remote_host_pairing::remora_link_v2::ProofV2,
    ) -> Result<FinishedExchangeV2, HostPortErrorV2> {
        let exchange = self
            .exchanges()
            .remove(exchange_id)
            .ok_or(HostPortErrorV2::ProtocolViolation)?;
        let future = async {
            let LiveExchange {
                started_at: _,
                host_id,
                runtime_id,
                request,
                challenge,
                authenticated_client_endpoint_id,
                connection,
                mut send,
                mut recv,
            } = exchange;
            write_proof_frame(&mut send, proof)
                .await
                .map_err(map_control_error)?;
            let terminal = read_response_frame(&mut recv)
                .await
                .map_err(map_control_error)?;
            let challenge = challenge
                .challenge
                .as_ref()
                .ok_or(HostPortErrorV2::ProtocolViolation)?;
            if terminal
                .validate_terminal_shape_for_correlation(
                    &request,
                    challenge,
                    &authenticated_client_endpoint_id,
                )
                .is_err()
            {
                connection.close(VarInt::from_u32(CLOSE_CODE), b"invalid response");
                return Err(HostPortErrorV2::ProtocolViolation);
            }
            if terminal.ok && terminal.session.is_some() {
                let runtime_id = runtime_id.ok_or(HostPortErrorV2::ProtocolViolation)?;
                let attachment_id = random_id("attachment");
                self.attachments
                    .lock()
                    .await
                    .insert(
                        attachment_id.clone(),
                        host_id,
                        runtime_id,
                        LiveRuntimeSession::new(connection, send, recv),
                        Instant::now(),
                    )
                    .map_err(|_| HostPortErrorV2::Unavailable)?;
                Ok(FinishedExchangeV2 {
                    response: terminal,
                    attachment_id: Some(attachment_id),
                })
            } else {
                let _ = send.finish();
                Ok(FinishedExchangeV2 {
                    response: terminal,
                    attachment_id: None,
                })
            }
        };
        tokio::time::timeout(HOST_FINISH_TIMEOUT, future)
            .await
            .map_err(|_| HostPortErrorV2::Unavailable)?
    }

    fn abandon_exchange(&self, exchange_id: &str) {
        if let Some(exchange) = self.exchanges().remove(exchange_id) {
            exchange
                .connection
                .close(VarInt::from_u32(CLOSE_CODE), b"exchange abandoned");
        }
    }

    async fn close_local(&self, host_id: &str) {
        self.shell_connections.close_host(host_id);
        self.attachments.lock().await.remove_host(host_id);
        let exchange_ids: Vec<_> = self
            .exchanges()
            .iter()
            .filter(|(_, exchange)| exchange.host_id == host_id)
            .map(|(id, _)| id.clone())
            .collect();
        for exchange_id in exchange_ids {
            self.abandon_exchange(&exchange_id);
        }
    }
}

struct RemoraLinkSessionKeepaliveV2 {
    connection: Connection,
}

struct RemoraLinkShellConnectionV2 {
    connection: Connection,
}

impl crate::terminal::remote_shell::RemoteShellConnection for RemoraLinkShellConnectionV2 {
    fn close(&self) {
        self.connection.close(
            VarInt::from_u32(CLOSE_CODE),
            b"Remora Link shell session closed",
        );
    }
}

impl Drop for RemoraLinkShellConnectionV2 {
    fn drop(&mut self) {
        self.connection.close(
            VarInt::from_u32(CLOSE_CODE),
            b"Remora Link shell attachment dropped",
        );
    }
}

impl RemoraLinkSessionKeepaliveV2 {
    fn close_connection(&self) {
        self.connection
            .close(VarInt::from_u32(CLOSE_CODE), b"Remora Link runtime closed");
    }
}

impl SessionKeepalive for RemoraLinkSessionKeepaliveV2 {
    fn close(&self) {
        self.close_connection();
    }
}

impl Drop for RemoraLinkSessionKeepaliveV2 {
    fn drop(&mut self) {
        self.close_connection();
    }
}

/// Reconnect adapter for a runtime whose initial stream was attached through
/// the authenticated Remora Link v2 lifecycle. The adapter owns the replay
/// cursor and the currently installed connection; it never consults retired v1
/// tokens, state, or sequence metadata.
pub(crate) struct RemoraLinkRemoteTransportV2 {
    configured: Arc<ConfiguredRemoraLink>,
    app_store: Arc<crate::store::AppStoreReducer>,
    host_id: String,
    runtime_id: String,
    wire: AgentWireV2,
    /// Socket-observed high-water mark for compatibility diagnostics only.
    /// It is not replay authority: a frame may be read before its JSON-RPC
    /// effect is applied to canonical state.
    observed_sequence: Arc<AtomicU64>,
    authoritative_refresh_required: AtomicBool,
    current_keepalive: Mutex<Option<Arc<RemoraLinkSessionKeepaliveV2>>>,
}

impl RemoraLinkRemoteTransportV2 {
    pub(crate) fn new(
        configured: Arc<ConfiguredRemoraLink>,
        app_store: Arc<crate::store::AppStoreReducer>,
        host_id: String,
        runtime_id: String,
        wire: AgentWireV2,
    ) -> Self {
        Self {
            configured,
            app_store,
            host_id,
            runtime_id,
            wire,
            observed_sequence: Arc::new(AtomicU64::new(0)),
            authoritative_refresh_required: AtomicBool::new(false),
            current_keepalive: Mutex::new(None),
        }
    }

    /// Consume exactly the attachment produced by `outcome`. Cancellation at
    /// any point after custody is taken drops the local keepalive and closes
    /// the QUIC connection.
    pub(crate) async fn connect_attachment(
        &self,
        outcome: ReconnectOutcomeV2,
        args: RemoteAppServerConnectArgs,
    ) -> Result<Reconnected, TransportError> {
        if outcome.host_id != self.host_id || outcome.runtime_id != self.runtime_id {
            return Err(TransportError::ConnectionFailed(
                "Remora Link attachment identity mismatch".to_string(),
            ));
        }
        let attachment = self
            .configured
            .host
            .take_runtime_attachment(
                &outcome.attachment_id,
                &outcome.host_id,
                &outcome.runtime_id,
            )
            .await
            .map_err(map_attachment_error)?;
        self.connect_taken_attachment(outcome, attachment, args)
            .await
    }

    async fn connect_taken_attachment(
        &self,
        outcome: ReconnectOutcomeV2,
        attachment: LiveRuntimeSession,
        args: RemoteAppServerConnectArgs,
    ) -> Result<Reconnected, TransportError> {
        let (connection, send, recv) = attachment.into_parts();
        let keepalive = Arc::new(RemoraLinkSessionKeepaliveV2 { connection });
        let stream = tokio::io::join(recv, send);
        let client =
            connect_runtime_client_v2(stream, self.wire, Arc::clone(&self.observed_sequence), args)
                .await?;
        self.authoritative_refresh_required.store(
            outcome.session.attached == AttachKindV2::DriftReload,
            Ordering::Release,
        );
        *self.current_keepalive.lock().await = Some(Arc::clone(&keepalive));
        let keepalive: Arc<dyn SessionKeepalive> = keepalive;
        Ok(Reconnected {
            client,
            keepalive: Some(keepalive),
        })
    }
}

#[async_trait]
impl RemoteTransport for RemoraLinkRemoteTransportV2 {
    async fn reconnect(
        &self,
        args: &RemoteAppServerConnectArgs,
        _websocket_url: &str,
    ) -> Result<Reconnected, TransportError> {
        // The raw observer cannot prove that a decoded JSON-RPC item was
        // applied to AppStore. Always send the conservative origin cursor;
        // the host either replays retained history or answers drift_reload,
        // which `reconcile_replay` resolves authoritatively before Connected.
        let outcome = self
            .configured
            .lifecycle
            .reconnect(&self.host_id, self.runtime_id.clone(), Some(0))
            .await
            .map_err(|error| TransportError::ConnectionFailed(error.to_string()))?;
        self.connect_attachment(outcome, args.clone()).await
    }

    fn route_label(&self) -> &'static str {
        "remora-link-v2"
    }

    fn take_replay_outcome(&self) -> ReplayOutcome {
        if self
            .authoritative_refresh_required
            .swap(false, Ordering::AcqRel)
        {
            ReplayOutcome::AuthoritativeRefreshRequired
        } else {
            ReplayOutcome::Complete
        }
    }

    async fn reconcile_replay(
        &self,
        client: &codex_app_server_client::AppServerClient,
        outcome: ReplayOutcome,
    ) -> Result<(), TransportError> {
        if outcome != ReplayOutcome::AuthoritativeRefreshRequired {
            return Ok(());
        }
        crate::mobile_client::thread_projection::refresh_runtime_thread_list_from_client(
            client,
            Arc::clone(&self.app_store),
            &self.host_id,
            self.runtime_id.clone(),
        )
        .await
        .map_err(|error| {
            TransportError::ConnectionFailed(format!(
                "Remora Link authoritative replay reconciliation failed: {error}"
            ))
        })
    }

    async fn notify_network_change(&self) {
        self.configured.host.network_change().await;
    }
}

fn map_attachment_error(error: AttachmentCustodyErrorV2) -> TransportError {
    TransportError::ConnectionFailed(error.to_string())
}

fn map_shell_lifecycle_error(error: LifecycleErrorV2) -> crate::terminal::TerminalError {
    use crate::terminal::TerminalError;

    let detail = match error {
        LifecycleErrorV2::NotEnrolled => "Remora Link host is not enrolled".to_string(),
        LifecycleErrorV2::InvalidSelection => {
            "Remora Link host is not authorized for the selected shell runtime".to_string()
        }
        LifecycleErrorV2::AgentUnavailable => {
            "Remote shell is unavailable on this paired host".to_string()
        }
        other => format!("opening Remora Link shell attachment: {other}"),
    };
    TerminalError::Backend { detail }
}

fn map_control_error(
    error: crate::remote_host_pairing::remora_link_v2::ControlExchangeError,
) -> HostPortErrorV2 {
    match error {
        crate::remote_host_pairing::remora_link_v2::ControlExchangeError::Io => {
            HostPortErrorV2::Unavailable
        }
        _ => HostPortErrorV2::ProtocolViolation,
    }
}

// MARK: - Projection and validation helpers

fn decode_code(code: AppRemoraLinkPairingCode) -> Result<V2Invite, RemoraLinkError> {
    if code.0.len() > MAX_PAIRING_CODE_BYTES {
        return Err(RemoraLinkError::InvalidPairingCode);
    }
    let code = String::from_utf8(code.into_bytes()).map_err(|error| {
        let mut bytes = error.into_bytes();
        bytes.zeroize();
        RemoraLinkError::InvalidPairingCode
    })?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    decode_pairing_code(code, now).map_err(|_| RemoraLinkError::InvalidPairingCode)
}

fn offer_from_inspection(
    invite: &V2Invite,
    inspection: InvitationInspectionV2,
) -> Result<AppRemoraLinkOffer, RemoraLinkError> {
    let selects_coding_runtime = inspection
        .max_runtime_ids
        .iter()
        .any(|runtime_id| !is_shell_runtime(runtime_id));
    let required_coding_scopes = [
        DeviceScopeV2::InspectRuntimes,
        DeviceScopeV2::ConnectRuntime,
        DeviceScopeV2::SelfRevoke,
    ];
    if selects_coding_runtime
        && !required_coding_scopes
            .iter()
            .all(|scope| inspection.max_scopes.contains(scope))
    {
        return Err(RemoraLinkError::InvalidSelection);
    }
    let required_scopes = match inspection.confirmation_mode {
        ConfirmationModeV2::Interactive if selects_coding_runtime => {
            required_coding_scopes.into_iter().map(Into::into).collect()
        }
        ConfirmationModeV2::Interactive => vec![
            AppRemoraLinkScope::ConnectRuntime,
            AppRemoraLinkScope::SelfRevoke,
        ],
        ConfirmationModeV2::Unattended => inspection
            .max_scopes
            .iter()
            .copied()
            .map(Into::into)
            .collect(),
    };
    let default_scopes = match inspection.confirmation_mode {
        ConfirmationModeV2::Interactive => required_scopes.clone(),
        ConfirmationModeV2::Unattended => required_scopes.clone(),
    };
    let default_runtime_ids = match inspection.confirmation_mode {
        ConfirmationModeV2::Interactive => inspection
            .runtime_offers
            .iter()
            .filter(|offer| offer.available && offer.recommended)
            .map(|offer| offer.runtime_id.clone())
            .collect(),
        // The authenticated unattended policy is already validated to one
        // exact runtime, so accepting its default must select that grant.
        ConfirmationModeV2::Unattended => inspection.max_runtime_ids.clone(),
    };
    Ok(AppRemoraLinkOffer {
        offer_id: random_id("offer"),
        host_id: v2_host_id(invite),
        host_display_name: invite
            .host_name
            .clone()
            .unwrap_or_else(|| DEFAULT_HOST_DISPLAY_NAME.to_string()),
        expires_at_unix_ms: unix_seconds_to_millis(inspection.expires_at),
        confirmation_mode: inspection.confirmation_mode.into(),
        runtime_offers: inspection
            .runtime_offers
            .into_iter()
            .map(Into::into)
            .collect(),
        maximum_scopes: inspection.max_scopes.into_iter().map(Into::into).collect(),
        required_scopes,
        default_scopes,
        default_runtime_ids,
    })
}

fn project_pairing_outcome(
    host_id: &str,
    outcome: EnrollmentOutcomeV2,
) -> AppRemoraLinkPairingOutcome {
    match outcome {
        EnrollmentOutcomeV2::Pending(pending) => {
            AppRemoraLinkPairingOutcome::AwaitingHostApproval {
                host_id: host_id.to_string(),
                sas: pending.enrollment_confirmation.sas,
                expires_at_unix_ms: unix_seconds_to_millis(pending.expires_at),
                selected_runtime_ids: pending.selected_runtime_ids,
                requested_scopes: pending
                    .requested_scopes
                    .into_iter()
                    .map(Into::into)
                    .collect(),
            }
        }
        EnrollmentOutcomeV2::Enrolled(enrolled) => AppRemoraLinkPairingOutcome::Paired {
            host_id: host_id.to_string(),
            sas: enrolled.enrollment_confirmation.sas,
            selected_runtime_ids: enrolled.selected_runtime_ids,
            granted_scopes: enrolled
                .granted_scopes
                .into_iter()
                .map(Into::into)
                .collect(),
            created_at_unix_ms: unix_seconds_to_millis(enrolled.created_at),
        },
        EnrollmentOutcomeV2::AlreadyEnrolled(credential) => {
            AppRemoraLinkPairingOutcome::AlreadyPaired {
                host_id: host_id.to_string(),
                selected_runtime_ids: credential.selected_runtime_ids,
                granted_scopes: credential
                    .granted_scopes
                    .into_iter()
                    .map(Into::into)
                    .collect(),
            }
        }
    }
}

fn pairing_host_id(outcome: &AppRemoraLinkPairingOutcome) -> Option<&str> {
    match outcome {
        AppRemoraLinkPairingOutcome::Paired { host_id, .. }
        | AppRemoraLinkPairingOutcome::AlreadyPaired { host_id, .. } => Some(host_id),
        AppRemoraLinkPairingOutcome::AwaitingHostApproval { .. } => None,
    }
}

async fn attach_after_pairing_outcome(
    attacher: &dyn RemoraLinkRuntimeSessionAttacher,
    outcome: &AppRemoraLinkPairingOutcome,
) -> Option<Result<RemoraLinkRuntimeConnectOutcome, TransportError>> {
    let host_id = pairing_host_id(outcome)?;
    let result = attacher.attach_runtime_session(host_id, false).await;
    match &result {
        Ok(outcome) if !outcome.unavailable_runtime_ids.is_empty() => warn!(
            server_id = host_id,
            unavailable_runtimes = ?outcome.unavailable_runtime_ids,
            "Remora Link enrollment succeeded with degraded runtime attachment"
        ),
        Err(error) => warn!(
            server_id = host_id,
            %error,
            "Remora Link enrollment succeeded but canonical runtime attachment failed"
        ),
        Ok(_) => {}
    }
    Some(result)
}

async fn require_pairing_attachment(
    attacher: &dyn RemoraLinkRuntimeSessionAttacher,
    outcome: &AppRemoraLinkPairingOutcome,
) -> Result<(), RemoraLinkError> {
    match attach_after_pairing_outcome(attacher, outcome).await {
        Some(Ok(_)) | None => Ok(()),
        Some(Err(_)) => Err(RemoraLinkError::RuntimeUnavailable),
    }
}

fn project_remora_link_disconnected(app_store: &crate::store::AppStoreReducer, host_id: &str) {
    if let Some(server) = app_store.snapshot().servers.get(host_id) {
        let mut runtimes = server.agent_runtimes.clone();
        for runtime in &mut runtimes {
            runtime.available = false;
        }
        app_store.update_server_agent_runtimes(host_id, runtimes);
    }
    app_store.update_server_health(host_id, ServerHealthSnapshot::Disconnected);
}

async fn complete_remora_link_host_mutation<T, F>(
    client: &crate::MobileClient,
    host_id: &str,
    operation: F,
) -> Result<T, RemoraLinkError>
where
    F: Future<Output = Result<T, LifecycleErrorV2>>,
{
    let shell_cleanup = remora_link_read(&client.remora_link)
        .clone()
        .map(|configured| RemoraLinkShellCleanupGuard {
            registry: Arc::clone(&configured.host.shell_connections),
            host_id: host_id.to_string(),
        });
    if let Some(cleanup) = &shell_cleanup {
        cleanup.registry.close_host(host_id);
    }
    client.disconnect_remora_link_session(host_id).await;
    let outcome = operation.await.map_err(Into::into);
    drop(shell_cleanup);
    outcome
}

struct RemoraLinkShellCleanupGuard {
    registry: Arc<RemoraLinkShellConnectionRegistryV2>,
    host_id: String,
}

impl Drop for RemoraLinkShellCleanupGuard {
    fn drop(&mut self) {
        self.registry.close_host(&self.host_id);
    }
}

fn project_pairing_cancellation(
    outcome: MutationOutcomeV2,
) -> Result<AppRemoraLinkPairingCancellationOutcome, RemoraLinkError> {
    match outcome {
        MutationOutcomeV2::RolledBack => Ok(AppRemoraLinkPairingCancellationOutcome::Cancelled),
        MutationOutcomeV2::OutcomeUnknown => {
            Ok(AppRemoraLinkPairingCancellationOutcome::OutcomeUnknown)
        }
        MutationOutcomeV2::Revoked => Err(RemoraLinkError::ProtocolViolation),
    }
}

fn project_existing_credential(
    entry: &PairingJournalEntryV2,
) -> Result<AppRemoraLinkPairingOutcome, RemoraLinkError> {
    let credential = entry
        .credential
        .as_ref()
        .ok_or(RemoraLinkError::JournalCorrupt)?;
    Ok(AppRemoraLinkPairingOutcome::AlreadyPaired {
        host_id: entry.binding.host_id.clone(),
        selected_runtime_ids: credential.selected_runtime_ids.clone(),
        granted_scopes: credential
            .granted_scopes
            .iter()
            .copied()
            .map(Into::into)
            .collect(),
    })
}

fn project_host_summary(entry: PairingJournalEntryV2) -> AppRemoraLinkHostSummary {
    let selected_runtime_ids = entry
        .credential
        .as_ref()
        .map(|value| value.selected_runtime_ids.clone())
        .or_else(|| {
            entry
                .enrollment
                .as_ref()
                .map(|value| value.selected_runtime_ids.clone())
        })
        .unwrap_or_default();
    let granted_scopes = entry
        .credential
        .as_ref()
        .map(|value| {
            value
                .granted_scopes
                .iter()
                .copied()
                .map(Into::into)
                .collect()
        })
        .unwrap_or_default();
    let pending_approval = entry
        .enrollment
        .as_ref()
        .and_then(|value| value.pending_claim.as_ref())
        .map(|value| AppRemoraLinkPendingApproval {
            sas: value.sas.clone(),
            expires_at_unix_ms: unix_seconds_to_millis(value.expires_at),
            requested_scopes: value
                .requested_scopes
                .iter()
                .copied()
                .map(Into::into)
                .collect(),
            device_display_name: value.display_name.clone(),
        });
    let pending_restart = entry
        .pending_restart
        .as_ref()
        .map(|value| AppRemoraLinkPendingRestart {
            runtime_id: value.runtime_id.clone(),
            command_sequence: value.command_sequence,
            outcome_unknown: value.disposition == RestartDispositionV2::OutcomeUnknown,
        });
    let (state, host_revocation_still_required) = match entry.phase {
        JournalPhaseV2::Inspecting => (AppRemoraLinkHostState::Inspecting, false),
        JournalPhaseV2::Ready => (AppRemoraLinkHostState::Ready, false),
        JournalPhaseV2::EnrollmentStaged => (AppRemoraLinkHostState::Pairing, true),
        JournalPhaseV2::EnrollmentPending => (AppRemoraLinkHostState::AwaitingHostApproval, true),
        JournalPhaseV2::Enrolled => (AppRemoraLinkHostState::Paired, false),
        JournalPhaseV2::RollbackPending | JournalPhaseV2::RevocationPending => {
            (AppRemoraLinkHostState::Revoking, true)
        }
        JournalPhaseV2::Revoked { .. } => (AppRemoraLinkHostState::Revoked, false),
        JournalPhaseV2::Forgetting {
            host_revocation_still_required,
        } => (
            AppRemoraLinkHostState::Forgetting,
            host_revocation_still_required,
        ),
        JournalPhaseV2::Forgotten {
            host_revocation_still_required,
        } => (
            AppRemoraLinkHostState::Forgotten,
            host_revocation_still_required,
        ),
        JournalPhaseV2::Quarantined { .. } => (AppRemoraLinkHostState::NeedsRepair, true),
    };
    AppRemoraLinkHostSummary {
        host_id: entry.binding.host_id,
        host_display_name: entry
            .binding
            .host_display_name
            .unwrap_or_else(|| DEFAULT_HOST_DISPLAY_NAME.to_string()),
        state,
        selected_runtime_ids,
        granted_scopes,
        pending_approval,
        pending_restart,
        host_revocation_still_required,
    }
}

fn v2_host_id(invite: &V2Invite) -> String {
    format!("remora-link:{}", invite.node_id)
}

fn random_id(purpose: &str) -> String {
    let mut bytes = [0_u8; 16];
    OsRng.fill_bytes(&mut bytes);
    let encoded = URL_SAFE_NO_PAD.encode(bytes);
    bytes.zeroize();
    format!("{purpose}-{encoded}")
}

fn unix_seconds_to_millis(seconds: i64) -> u64 {
    u64::try_from(seconds)
        .ok()
        .and_then(|value| value.checked_mul(1_000))
        .unwrap_or(0)
}

fn remora_link_read(
    value: &RwLock<Option<Arc<ConfiguredRemoraLink>>>,
) -> std::sync::RwLockReadGuard<'_, Option<Arc<ConfiguredRemoraLink>>> {
    value
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn remora_link_write(
    value: &RwLock<Option<Arc<ConfiguredRemoraLink>>>,
) -> std::sync::RwLockWriteGuard<'_, Option<Arc<ConfiguredRemoraLink>>> {
    value
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl From<ConfirmationModeV2> for AppRemoraLinkConfirmationMode {
    fn from(value: ConfirmationModeV2) -> Self {
        match value {
            ConfirmationModeV2::Interactive => Self::Interactive,
            ConfirmationModeV2::Unattended => Self::Unattended,
        }
    }
}

impl From<AppRemoraLinkScope> for DeviceScopeV2 {
    fn from(value: AppRemoraLinkScope) -> Self {
        match value {
            AppRemoraLinkScope::InspectRuntimes => Self::InspectRuntimes,
            AppRemoraLinkScope::ConnectRuntime => Self::ConnectRuntime,
            AppRemoraLinkScope::RestartRuntime => Self::RestartRuntime,
            AppRemoraLinkScope::SelfRevoke => Self::SelfRevoke,
        }
    }
}

impl From<DeviceScopeV2> for AppRemoraLinkScope {
    fn from(value: DeviceScopeV2) -> Self {
        match value {
            DeviceScopeV2::InspectRuntimes => Self::InspectRuntimes,
            DeviceScopeV2::ConnectRuntime => Self::ConnectRuntime,
            DeviceScopeV2::RestartRuntime => Self::RestartRuntime,
            DeviceScopeV2::SelfRevoke => Self::SelfRevoke,
        }
    }
}

impl From<crate::remote_host_pairing::remora_link_v2::RuntimeOfferV2>
    for AppRemoraLinkRuntimeOffer
{
    fn from(value: crate::remote_host_pairing::remora_link_v2::RuntimeOfferV2) -> Self {
        Self {
            runtime_id: value.runtime_id,
            display_name: value.display_name,
            available: value.available,
            recommended: value.recommended,
        }
    }
}

impl From<ReconnectOutcomeV2> for AppRemoraLinkReconnectResult {
    fn from(value: ReconnectOutcomeV2) -> Self {
        Self {
            host_id: value.host_id,
            runtime_id: value.runtime_id,
            attached: match value.session.attached {
                AttachKindV2::Fresh => AppRemoraLinkAttachKind::Fresh,
                AttachKindV2::Resumed => AppRemoraLinkAttachKind::Resumed,
                AttachKindV2::DriftReload => AppRemoraLinkAttachKind::DriftReload,
            },
            current_sequence: value.session.current_seq,
            floor_sequence: value.session.floor_seq,
        }
    }
}

impl From<ForgetOutcomeV2> for AppRemoraLinkForgetResult {
    fn from(value: ForgetOutcomeV2) -> Self {
        match value {
            ForgetOutcomeV2::ForgottenLocally {
                host_id,
                host_revocation_still_required,
            } => Self {
                host_id,
                already_forgotten: false,
                host_revocation_still_required,
            },
            ForgetOutcomeV2::AlreadyForgotten {
                host_id,
                host_revocation_still_required,
            } => Self {
                host_id,
                already_forgotten: true,
                host_revocation_still_required,
            },
        }
    }
}

impl From<LifecycleErrorV2> for AppRemoraLinkFailure {
    fn from(value: LifecycleErrorV2) -> Self {
        match value {
            LifecycleErrorV2::JournalUnavailable => Self::JournalUnavailable,
            LifecycleErrorV2::JournalConflict => Self::JournalConflict,
            LifecycleErrorV2::JournalCorrupt => Self::JournalCorrupt,
            LifecycleErrorV2::CredentialUnavailable => Self::CredentialUnavailable,
            LifecycleErrorV2::MissingCredential => Self::MissingCredential,
            LifecycleErrorV2::InvalidSignature => Self::InvalidSignature,
            LifecycleErrorV2::HostUnavailable => Self::HostUnavailable,
            LifecycleErrorV2::HostIdentityDrift
            | LifecycleErrorV2::ClientIdentityDrift
            | LifecycleErrorV2::HardwareKeyDrift => Self::IdentityDrift,
            LifecycleErrorV2::PolicyDrift => Self::PolicyDrift,
            LifecycleErrorV2::ConfirmationMismatch => Self::ConfirmationMismatch,
            LifecycleErrorV2::ProtocolViolation => Self::ProtocolViolation,
            LifecycleErrorV2::PairingUnavailable => Self::PairingUnavailable,
            LifecycleErrorV2::AuthorizationRequired => Self::AuthorizationRequired,
            LifecycleErrorV2::AgentUnavailable => Self::RuntimeUnavailable,
            LifecycleErrorV2::OutcomeUnknown => Self::OutcomeUnknown,
            LifecycleErrorV2::InvalidSelection => Self::InvalidSelection,
            LifecycleErrorV2::InvitationMismatch => Self::InvitationMismatch,
            LifecycleErrorV2::NotEnrolled => Self::NotPaired,
            LifecycleErrorV2::OperationInProgress => Self::OperationInProgress,
            LifecycleErrorV2::Quarantined | LifecycleErrorV2::CandidateLimitReached => {
                Self::NeedsRepair
            }
        }
    }
}

impl From<LifecycleErrorV2> for RemoraLinkError {
    fn from(value: LifecycleErrorV2) -> Self {
        match AppRemoraLinkFailure::from(value) {
            AppRemoraLinkFailure::JournalUnavailable => Self::JournalUnavailable,
            AppRemoraLinkFailure::JournalConflict => Self::JournalConflict,
            AppRemoraLinkFailure::JournalCorrupt => Self::JournalCorrupt,
            AppRemoraLinkFailure::CredentialUnavailable => Self::CredentialUnavailable,
            AppRemoraLinkFailure::MissingCredential => Self::MissingCredential,
            AppRemoraLinkFailure::InvalidSignature => Self::InvalidSignature,
            AppRemoraLinkFailure::HostUnavailable => Self::HostUnavailable,
            AppRemoraLinkFailure::V2Unavailable => Self::V2Unavailable,
            AppRemoraLinkFailure::Cancelled => Self::Cancelled,
            AppRemoraLinkFailure::IdentityDrift => Self::IdentityDrift,
            AppRemoraLinkFailure::PolicyDrift => Self::PolicyDrift,
            AppRemoraLinkFailure::ConfirmationMismatch => Self::ConfirmationMismatch,
            AppRemoraLinkFailure::ProtocolViolation => Self::ProtocolViolation,
            AppRemoraLinkFailure::PairingUnavailable => Self::PairingUnavailable,
            AppRemoraLinkFailure::AuthorizationRequired => Self::AuthorizationRequired,
            AppRemoraLinkFailure::RuntimeUnavailable => Self::RuntimeUnavailable,
            AppRemoraLinkFailure::OutcomeUnknown => Self::OutcomeUnknown,
            AppRemoraLinkFailure::InvalidSelection => Self::InvalidSelection,
            AppRemoraLinkFailure::InvitationMismatch => Self::InvitationMismatch,
            AppRemoraLinkFailure::NotPaired => Self::NotPaired,
            AppRemoraLinkFailure::OperationInProgress => Self::OperationInProgress,
            AppRemoraLinkFailure::NeedsRepair => Self::NeedsRepair,
        }
    }
}

impl From<JournalPortErrorV2> for RemoraLinkError {
    fn from(value: JournalPortErrorV2) -> Self {
        match value {
            JournalPortErrorV2::Unavailable => Self::JournalUnavailable,
            JournalPortErrorV2::Corrupt => Self::JournalCorrupt,
            JournalPortErrorV2::Conflict => Self::JournalConflict,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use p256::ecdsa::SigningKey;

    use super::*;
    use crate::remote_host_pairing::remora_link_v2::CredentialJournalV2;

    struct CountingShellConnection {
        closes: Arc<AtomicUsize>,
    }

    impl crate::terminal::remote_shell::RemoteShellConnection for CountingShellConnection {
        fn close(&self) {
            self.closes.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn active_shell_cleanup_is_host_scoped_and_drop_unregisters() {
        let registry = Arc::new(RemoraLinkShellConnectionRegistryV2::default());
        let target_closes = Arc::new(AtomicUsize::new(0));
        let sibling_closes = Arc::new(AtomicUsize::new(0));
        let target = registry
            .register(
                "remora-link:target",
                registry.claim_generation("remora-link:target"),
                Arc::new(CountingShellConnection {
                    closes: Arc::clone(&target_closes),
                }),
            )
            .unwrap();
        let sibling = registry
            .register(
                "remora-link:sibling",
                registry.claim_generation("remora-link:sibling"),
                Arc::new(CountingShellConnection {
                    closes: Arc::clone(&sibling_closes),
                }),
            )
            .unwrap();

        registry.close_host("remora-link:target");
        assert_eq!(target_closes.load(Ordering::SeqCst), 1);
        assert_eq!(sibling_closes.load(Ordering::SeqCst), 0);
        assert_eq!(registry.connection_count("remora-link:target"), 0);
        assert_eq!(registry.connection_count("remora-link:sibling"), 1);

        drop(target);
        drop(sibling);
        assert_eq!(sibling_closes.load(Ordering::SeqCst), 1);
        assert_eq!(registry.connection_count("remora-link:sibling"), 0);
    }

    #[test]
    fn cleanup_generation_rejects_an_inflight_shell_claim() {
        let registry = Arc::new(RemoraLinkShellConnectionRegistryV2::default());
        let host_id = "remora-link:host";
        let generation = registry.claim_generation(host_id);
        registry.close_host(host_id);
        let closes = Arc::new(AtomicUsize::new(0));

        assert!(
            registry
                .register(
                    host_id,
                    generation,
                    Arc::new(CountingShellConnection {
                        closes: Arc::clone(&closes),
                    }),
                )
                .is_err()
        );
        assert_eq!(closes.load(Ordering::SeqCst), 1);
        assert_eq!(registry.connection_count(host_id), 0);
    }

    struct ConflictInjectingJournal {
        snapshot: StdMutex<Option<AppRemoraLinkJournalSnapshot>>,
        concurrent_entry: PairingJournalEntryV2,
        conflict_once: AtomicBool,
    }

    struct AcknowledgingWithoutStoreJournal;

    struct CountingEmptyJournal {
        loads: Arc<AtomicUsize>,
    }

    struct CandidateTransportIdentity;

    struct UnusedDeviceKeys;

    enum ReadbackFault {
        StaleRevision,
        DropHost(String),
    }

    struct FaultyReadbackJournal {
        snapshot: StdMutex<Option<AppRemoraLinkJournalSnapshot>>,
        fault: ReadbackFault,
    }

    #[derive(Default)]
    struct RecordingRuntimeAttacher {
        calls: StdMutex<Vec<(String, bool)>>,
        outcomes: StdMutex<HashMap<String, RemoraLinkRuntimeConnectOutcome>>,
        failures: StdMutex<HashSet<String>>,
    }

    #[async_trait]
    impl RemoraLinkRuntimeSessionAttacher for RecordingRuntimeAttacher {
        async fn attach_runtime_session(
            &self,
            host_id: &str,
            force_cold_rebuild: bool,
        ) -> Result<RemoraLinkRuntimeConnectOutcome, TransportError> {
            self.calls
                .lock()
                .unwrap()
                .push((host_id.to_string(), force_cold_rebuild));
            if self.failures.lock().unwrap().contains(host_id) {
                return Err(TransportError::ConnectionFailed(
                    "canonical attach failed".to_string(),
                ));
            }
            Ok(self
                .outcomes
                .lock()
                .unwrap()
                .get(host_id)
                .cloned()
                .unwrap_or_else(|| RemoraLinkRuntimeConnectOutcome {
                    server_id: host_id.to_string(),
                    connected_runtime_ids: Vec::new(),
                    unavailable_runtime_ids: Vec::new(),
                    connected_results: Vec::new(),
                }))
        }
    }

    #[async_trait]
    impl AppRemoraLinkJournalBackend for AcknowledgingWithoutStoreJournal {
        async fn load(&self) -> AppRemoraLinkJournalLoad {
            AppRemoraLinkJournalLoad::Missing
        }

        async fn compare_and_swap(
            &self,
            _expected_revision: Option<u64>,
            _replacement: AppRemoraLinkJournalSnapshot,
        ) -> AppRemoraLinkJournalWriteOutcome {
            AppRemoraLinkJournalWriteOutcome::Stored
        }
    }

    #[async_trait]
    impl AppRemoraLinkJournalBackend for CountingEmptyJournal {
        async fn load(&self) -> AppRemoraLinkJournalLoad {
            self.loads.fetch_add(1, Ordering::SeqCst);
            AppRemoraLinkJournalLoad::Missing
        }

        async fn compare_and_swap(
            &self,
            _expected_revision: Option<u64>,
            _replacement: AppRemoraLinkJournalSnapshot,
        ) -> AppRemoraLinkJournalWriteOutcome {
            AppRemoraLinkJournalWriteOutcome::Unavailable
        }
    }

    #[async_trait]
    impl AppRemoraLinkTransportIdentityBackend for CandidateTransportIdentity {
        async fn load_or_create(
            &self,
            candidate: AppRelaySecretValue,
        ) -> Result<AppRelaySecretValue, AppRemoraLinkTransportIdentityError> {
            Ok(candidate)
        }
    }

    #[async_trait]
    impl AppRemoraLinkDeviceKeyBackend for UnusedDeviceKeys {
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
            Ok(AppRemoraLinkKeyDeletionStatus::AlreadyMissing)
        }
    }

    #[async_trait]
    impl AppRemoraLinkJournalBackend for ConflictInjectingJournal {
        async fn load(&self) -> AppRemoraLinkJournalLoad {
            match self.snapshot.lock().unwrap().clone() {
                Some(snapshot) => AppRemoraLinkJournalLoad::Loaded { snapshot },
                None => AppRemoraLinkJournalLoad::Missing,
            }
        }

        async fn compare_and_swap(
            &self,
            expected_revision: Option<u64>,
            replacement: AppRemoraLinkJournalSnapshot,
        ) -> AppRemoraLinkJournalWriteOutcome {
            let mut snapshot = self.snapshot.lock().unwrap();
            if snapshot.as_ref().map(|value| value.revision) != expected_revision {
                return AppRemoraLinkJournalWriteOutcome::Conflict;
            }
            if self.conflict_once.swap(false, Ordering::SeqCst) {
                let current = snapshot.as_ref().unwrap();
                let mut entries = decode_journal(&current.payload).unwrap();
                entries.push(self.concurrent_entry.clone());
                entries.sort_by(|left, right| left.binding.host_id.cmp(&right.binding.host_id));
                *snapshot = Some(AppRemoraLinkJournalSnapshot {
                    revision: current.revision + 1,
                    payload: encode_journal(entries).unwrap(),
                });
                return AppRemoraLinkJournalWriteOutcome::Conflict;
            }
            *snapshot = Some(replacement);
            AppRemoraLinkJournalWriteOutcome::Stored
        }
    }

    #[async_trait]
    impl AppRemoraLinkJournalBackend for FaultyReadbackJournal {
        async fn load(&self) -> AppRemoraLinkJournalLoad {
            match self.snapshot.lock().unwrap().clone() {
                Some(snapshot) => AppRemoraLinkJournalLoad::Loaded { snapshot },
                None => AppRemoraLinkJournalLoad::Missing,
            }
        }

        async fn compare_and_swap(
            &self,
            expected_revision: Option<u64>,
            mut replacement: AppRemoraLinkJournalSnapshot,
        ) -> AppRemoraLinkJournalWriteOutcome {
            let mut snapshot = self.snapshot.lock().unwrap();
            if snapshot.as_ref().map(|value| value.revision) != expected_revision {
                return AppRemoraLinkJournalWriteOutcome::Conflict;
            }
            match &self.fault {
                ReadbackFault::StaleRevision => replacement.revision -= 1,
                ReadbackFault::DropHost(host_id) => {
                    let mut entries = decode_journal(&replacement.payload).unwrap();
                    entries.retain(|entry| entry.binding.host_id != *host_id);
                    replacement.payload = encode_journal(entries).unwrap();
                }
            }
            *snapshot = Some(replacement);
            AppRemoraLinkJournalWriteOutcome::Stored
        }
    }

    fn forgotten_entry(suffix: &str, seed: u8) -> PairingJournalEntryV2 {
        let signing_key = SigningKey::from_bytes((&[seed; 32]).into()).unwrap();
        let public_key = signing_key.verifying_key().to_encoded_point(false);
        PairingJournalEntryV2 {
            schema_version: JOURNAL_SCHEMA_VERSION,
            revision: 1,
            binding: crate::remote_host_pairing::remora_link_v2::HostBindingJournalV2 {
                host_id: format!("remora-link:node-{suffix}"),
                node_id: format!("node-{suffix}"),
                host_display_name: Some(format!("Host {suffix}")),
                relay_hint: None,
                hardware_key_slot: format!("remora-link:key-{suffix}"),
                device_public_key: URL_SAFE_NO_PAD.encode(public_key.as_bytes()),
                client_endpoint_id: None,
            },
            invitation: None,
            enrollment: None,
            credential: None,
            mutation: None,
            restart_high_watermark: 0,
            pending_restart: None,
            phase: JournalPhaseV2::Forgotten {
                host_revocation_still_required: false,
            },
        }
    }

    fn enrolled_entry(suffix: &str, seed: u8, runtime_ids: Vec<String>) -> PairingJournalEntryV2 {
        let mut entry = forgotten_entry(suffix, seed);
        entry.phase = JournalPhaseV2::Enrolled;
        entry.credential = Some(CredentialJournalV2 {
            credential_id: URL_SAFE_NO_PAD.encode([seed; 16]),
            selected_runtime_ids: runtime_ids,
            granted_scopes: vec![DeviceScopeV2::InspectRuntimes],
            auth_epoch: 0,
            created_at: 1,
            endpoint_fingerprint: format!("endpoint-{seed}"),
            device_key_fingerprint: format!("device-{seed}"),
            transcript_hash: URL_SAFE_NO_PAD.encode([seed; 32]),
            sas: "123456".to_string(),
        });
        entry
    }

    fn cached_offer(suffix: &str, seed: u8) -> (AppRemoraLinkOffer, V2Invite) {
        let node_id = format!("node-{suffix}");
        let host_id = format!("remora-link:{node_id}");
        (
            AppRemoraLinkOffer {
                offer_id: format!("offer-{suffix}"),
                host_id,
                host_display_name: format!("Host {suffix}"),
                expires_at_unix_ms: 1_900_000_000_000,
                confirmation_mode: AppRemoraLinkConfirmationMode::Interactive,
                runtime_offers: Vec::new(),
                maximum_scopes: vec![AppRemoraLinkScope::InspectRuntimes],
                required_scopes: vec![
                    AppRemoraLinkScope::ConnectRuntime,
                    AppRemoraLinkScope::SelfRevoke,
                ],
                default_scopes: vec![
                    AppRemoraLinkScope::ConnectRuntime,
                    AppRemoraLinkScope::SelfRevoke,
                ],
                default_runtime_ids: Vec::new(),
            },
            V2Invite {
                node_id,
                invitation_id: vec![seed; 16],
                secret: vec![seed; 32],
                expires_at: 1_900_000_000,
                max_runtime_ids: vec!["codex".to_string()],
                max_scopes: vec![DeviceScopeV2::InspectRuntimes],
                confirmation_mode: ConfirmationModeV2::Interactive,
                host_name: Some(format!("Host {suffix}")),
                relay: None,
            },
        )
    }

    fn runtime_offer(
        runtime_id: &str,
        available: bool,
        recommended: bool,
    ) -> crate::remote_host_pairing::remora_link_v2::RuntimeOfferV2 {
        crate::remote_host_pairing::remora_link_v2::RuntimeOfferV2 {
            runtime_id: runtime_id.to_string(),
            display_name: format!("{runtime_id} runtime"),
            available,
            recommended,
        }
    }

    fn inspection(
        mode: ConfirmationModeV2,
        runtime_ids: &[&str],
        scopes: Vec<DeviceScopeV2>,
        runtime_offers: Vec<crate::remote_host_pairing::remora_link_v2::RuntimeOfferV2>,
    ) -> InvitationInspectionV2 {
        InvitationInspectionV2 {
            invitation_id: URL_SAFE_NO_PAD.encode([1_u8; 16]),
            expires_at: 1_900_000_000,
            max_runtime_ids: runtime_ids
                .iter()
                .map(|value| (*value).to_string())
                .collect(),
            max_scopes: scopes,
            confirmation_mode: mode,
            runtime_offers,
        }
    }

    fn invite(
        mode: ConfirmationModeV2,
        runtime_ids: &[&str],
        scopes: Vec<DeviceScopeV2>,
    ) -> V2Invite {
        V2Invite {
            node_id: "host-node".to_string(),
            invitation_id: vec![1; 16],
            secret: vec![2; 32],
            expires_at: 1_900_000_000,
            max_runtime_ids: runtime_ids
                .iter()
                .map(|value| (*value).to_string())
                .collect(),
            max_scopes: scopes,
            confirmation_mode: mode,
            host_name: Some("Studio Mac".to_string()),
            relay: None,
        }
    }

    #[test]
    fn interactive_offer_projects_required_defaults_and_directional_host_name() {
        let scopes = vec![
            DeviceScopeV2::InspectRuntimes,
            DeviceScopeV2::ConnectRuntime,
            DeviceScopeV2::RestartRuntime,
            DeviceScopeV2::SelfRevoke,
        ];
        let offer = offer_from_inspection(
            &invite(
                ConfirmationModeV2::Interactive,
                &["codex", "offline", "shell"],
                scopes.clone(),
            ),
            inspection(
                ConfirmationModeV2::Interactive,
                &["codex", "offline", "shell"],
                scopes,
                vec![
                    runtime_offer("codex", true, true),
                    runtime_offer("offline", false, false),
                    runtime_offer("shell", true, false),
                ],
            ),
        )
        .unwrap();

        assert_eq!(offer.host_display_name, "Studio Mac");
        assert_eq!(
            offer.required_scopes,
            vec![
                AppRemoraLinkScope::InspectRuntimes,
                AppRemoraLinkScope::ConnectRuntime,
                AppRemoraLinkScope::SelfRevoke,
            ]
        );
        assert_eq!(
            offer.default_scopes,
            vec![
                AppRemoraLinkScope::InspectRuntimes,
                AppRemoraLinkScope::ConnectRuntime,
                AppRemoraLinkScope::SelfRevoke,
            ]
        );
        assert_eq!(offer.default_runtime_ids, vec!["codex"]);
        assert!(
            !offer
                .default_scopes
                .contains(&AppRemoraLinkScope::RestartRuntime)
        );
    }

    #[test]
    fn coding_runtime_offer_fails_when_host_disallows_inspection() {
        let scopes = vec![DeviceScopeV2::ConnectRuntime, DeviceScopeV2::SelfRevoke];
        let result = offer_from_inspection(
            &invite(ConfirmationModeV2::Interactive, &["codex"], scopes.clone()),
            inspection(
                ConfirmationModeV2::Interactive,
                &["codex"],
                scopes,
                vec![runtime_offer("codex", true, true)],
            ),
        );

        assert!(matches!(result, Err(RemoraLinkError::InvalidSelection)));
    }

    #[test]
    fn host_display_name_uses_one_directional_fallback() {
        let scopes = vec![
            DeviceScopeV2::InspectRuntimes,
            DeviceScopeV2::ConnectRuntime,
            DeviceScopeV2::SelfRevoke,
        ];
        let mut unnamed_invite =
            invite(ConfirmationModeV2::Interactive, &["codex"], scopes.clone());
        unnamed_invite.host_name = None;
        let offer = offer_from_inspection(
            &unnamed_invite,
            inspection(
                ConfirmationModeV2::Interactive,
                &["codex"],
                scopes,
                vec![runtime_offer("codex", true, true)],
            ),
        )
        .unwrap();
        let mut entry = forgotten_entry("unnamed", 6);
        entry.binding.host_display_name = None;

        assert_eq!(offer.host_display_name, DEFAULT_HOST_DISPLAY_NAME);
        assert_eq!(
            project_host_summary(entry).host_display_name,
            DEFAULT_HOST_DISPLAY_NAME
        );
    }

    #[test]
    fn unattended_offer_defaults_to_the_exact_protocol_grant() {
        let scopes = vec![
            DeviceScopeV2::InspectRuntimes,
            DeviceScopeV2::ConnectRuntime,
            DeviceScopeV2::SelfRevoke,
        ];
        let offer = offer_from_inspection(
            &invite(ConfirmationModeV2::Unattended, &["codex"], scopes.clone()),
            inspection(
                ConfirmationModeV2::Unattended,
                &["codex"],
                scopes,
                vec![runtime_offer("codex", true, true)],
            ),
        )
        .unwrap();

        let exact_scopes = vec![
            AppRemoraLinkScope::InspectRuntimes,
            AppRemoraLinkScope::ConnectRuntime,
            AppRemoraLinkScope::SelfRevoke,
        ];
        assert_eq!(offer.required_scopes, exact_scopes);
        assert_eq!(offer.default_scopes, exact_scopes);
        assert_eq!(offer.default_runtime_ids, vec!["codex"]);
    }

    #[test]
    fn host_summary_projects_one_atomic_pending_approval_from_the_journal_claim() {
        use crate::remote_host_pairing::remora_link_v2::{
            EnrollmentJournalV2, PendingClaimJournalV2,
        };

        let mut entry = forgotten_entry("pending", 7);
        entry.phase = JournalPhaseV2::EnrollmentPending;
        entry.enrollment = Some(EnrollmentJournalV2 {
            display_name: "Remora Phone".to_string(),
            selected_runtime_ids: vec!["codex".to_string()],
            requested_scopes: vec![
                DeviceScopeV2::InspectRuntimes,
                DeviceScopeV2::ConnectRuntime,
                DeviceScopeV2::SelfRevoke,
            ],
            idempotency_key: "enrollment-key".to_string(),
            prospective_credential_id: Some(URL_SAFE_NO_PAD.encode([8_u8; 16])),
            candidates: Vec::new(),
            pending_claim: Some(PendingClaimJournalV2 {
                claim_id: URL_SAFE_NO_PAD.encode([9_u8; 16]),
                credential_id: URL_SAFE_NO_PAD.encode([8_u8; 16]),
                display_name: "Remora Phone".to_string(),
                selected_runtime_ids: vec!["codex".to_string()],
                requested_scopes: vec![
                    DeviceScopeV2::InspectRuntimes,
                    DeviceScopeV2::ConnectRuntime,
                    DeviceScopeV2::SelfRevoke,
                ],
                transcript_hash: URL_SAFE_NO_PAD.encode([10_u8; 32]),
                sas: "ABC-123".to_string(),
                created_at: 1_800_000_000,
                expires_at: 1_800_000_300,
            }),
        });

        let summary = project_host_summary(entry);

        assert_eq!(summary.host_display_name, "Host pending");
        assert!(
            summary.host_revocation_still_required,
            "a staged host credential may already exist while approval is pending"
        );
        assert_eq!(
            summary.pending_approval,
            Some(AppRemoraLinkPendingApproval {
                sas: "ABC-123".to_string(),
                expires_at_unix_ms: 1_800_000_300_000,
                requested_scopes: vec![
                    AppRemoraLinkScope::InspectRuntimes,
                    AppRemoraLinkScope::ConnectRuntime,
                    AppRemoraLinkScope::SelfRevoke,
                ],
                device_display_name: "Remora Phone".to_string(),
            })
        );
    }

    #[test]
    fn host_summary_projects_ambiguous_restart_until_operator_acknowledges_it() {
        use crate::remote_host_pairing::remora_link_v2::RestartCommandJournalV2;

        let mut entry = enrolled_entry("restart", 8, vec!["codex".to_string()]);
        entry.pending_restart = Some(RestartCommandJournalV2 {
            runtime_id: "codex".to_string(),
            idempotency_key: "restart-key".to_string(),
            command_sequence: 19,
            disposition: RestartDispositionV2::OutcomeUnknown,
        });

        let summary = project_host_summary(entry);

        assert_eq!(
            summary.pending_restart,
            Some(AppRemoraLinkPendingRestart {
                runtime_id: "codex".to_string(),
                command_sequence: 19,
                outcome_unknown: true,
            })
        );
    }

    #[test]
    fn staged_and_pending_enrollment_summaries_preserve_possible_host_authority() {
        for phase in [
            JournalPhaseV2::EnrollmentStaged,
            JournalPhaseV2::EnrollmentPending,
        ] {
            let mut entry = forgotten_entry("authority", 11);
            entry.phase = phase;
            assert!(project_host_summary(entry).host_revocation_still_required);
        }
    }

    #[tokio::test]
    async fn unsupported_code_fails_closed_without_v2_configuration() {
        let client = AppClient {
            inner: crate::MobileClient::new(),
            rt: crate::ffi::shared::shared_runtime(),
        };
        let payload = serde_json::json!({
            "v": 7,
            "node_id": "a".repeat(64),
            "deprecated_secret": "must-not-be-accepted"
        })
        .to_string()
        .into_bytes()
        .into();

        assert_eq!(
            client.inspect_remora_link_code(payload).await,
            Err(RemoraLinkError::InvalidPairingCode)
        );
    }

    #[test]
    fn journal_codec_is_deterministic_and_rejects_unknown_fields() {
        let payload = encode_journal(Vec::new()).unwrap();
        assert_eq!(payload, br#"{"schema_version":1,"entries":[]}"#);
        assert!(decode_journal(&payload).unwrap().is_empty());
        assert_eq!(
            decode_journal(br#"{"schema_version":1,"entries":[],"secret":"no"}"#),
            Err(JournalPortErrorV2::Corrupt)
        );
    }

    #[tokio::test]
    async fn journal_cas_reloads_and_preserves_concurrent_whole_blob_updates() {
        let existing = forgotten_entry("b", 2);
        let concurrent = forgotten_entry("c", 3);
        let backend = Arc::new(ConflictInjectingJournal {
            snapshot: StdMutex::new(Some(AppRemoraLinkJournalSnapshot {
                revision: 7,
                payload: encode_journal(vec![existing]).unwrap(),
            })),
            concurrent_entry: concurrent,
            conflict_once: AtomicBool::new(true),
        });
        let journal = NativeRemoraLinkJournal::new(backend.clone());
        let replacement = forgotten_entry("a", 1);
        let host_id = replacement.binding.host_id.clone();

        journal
            .compare_and_swap(&host_id, None, replacement)
            .await
            .unwrap();

        let snapshot = backend.snapshot.lock().unwrap().clone().unwrap();
        assert_eq!(snapshot.revision, 9);
        let entries = decode_journal(&snapshot.payload).unwrap();
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.binding.host_id.as_str())
                .collect::<Vec<_>>(),
            vec![
                "remora-link:node-a",
                "remora-link:node-b",
                "remora-link:node-c"
            ]
        );
    }

    #[tokio::test]
    async fn journal_cas_requires_authoritative_readback_after_stored() {
        let journal = NativeRemoraLinkJournal::new(Arc::new(AcknowledgingWithoutStoreJournal));
        let replacement = forgotten_entry("a", 1);
        let host_id = replacement.binding.host_id.clone();

        assert_eq!(
            journal.compare_and_swap(&host_id, None, replacement).await,
            Err(JournalPortErrorV2::Unavailable)
        );
    }

    #[tokio::test]
    async fn journal_cas_rejects_stale_revision_and_dropped_non_target_state() {
        for fault in [
            ReadbackFault::StaleRevision,
            ReadbackFault::DropHost("remora-link:node-a".to_string()),
        ] {
            let existing = forgotten_entry("a", 1);
            let journal = NativeRemoraLinkJournal::new(Arc::new(FaultyReadbackJournal {
                snapshot: StdMutex::new(Some(AppRemoraLinkJournalSnapshot {
                    revision: 7,
                    payload: encode_journal(vec![existing]).unwrap(),
                })),
                fault,
            }));
            let replacement = forgotten_entry("b", 2);
            let host_id = replacement.binding.host_id.clone();

            assert_eq!(
                journal.compare_and_swap(&host_id, None, replacement).await,
                Err(JournalPortErrorV2::Conflict)
            );
        }
    }

    #[tokio::test]
    async fn journal_load_rejects_an_entry_revision_newer_than_its_snapshot() {
        let mut impossible = forgotten_entry("a", 1);
        impossible.revision = 2;
        let journal = NativeRemoraLinkJournal::new(Arc::new(FaultyReadbackJournal {
            snapshot: StdMutex::new(Some(AppRemoraLinkJournalSnapshot {
                revision: 1,
                payload: encode_journal(vec![impossible]).unwrap(),
            })),
            fault: ReadbackFault::StaleRevision,
        }));

        assert_eq!(journal.load_state().await, Err(JournalPortErrorV2::Corrupt));
    }

    #[tokio::test]
    async fn clear_waits_for_an_in_flight_configuration_read_lease() {
        let client = Arc::new(AppClient {
            inner: crate::MobileClient::new(),
            rt: crate::ffi::shared::shared_runtime(),
        });
        let operation_lease = client.inner.remora_link_configuration.read().await;
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let clearing_client = Arc::clone(&client);
        let clearing = tokio::spawn(async move {
            started_tx.send(()).unwrap();
            clearing_client.clear_remora_link().await;
        });

        started_rx.await.unwrap();
        tokio::task::yield_now().await;
        assert!(!clearing.is_finished());

        drop(operation_lease);
        tokio::time::timeout(Duration::from_secs(1), clearing)
            .await
            .expect("clear must acquire the write lease after the operation finishes")
            .unwrap();
    }

    #[tokio::test]
    async fn configuring_native_callbacks_immediately_scans_cold_recovery_and_reconnect_authority()
    {
        let loads = Arc::new(AtomicUsize::new(0));
        let client = AppClient {
            inner: crate::MobileClient::new(),
            rt: crate::ffi::shared::shared_runtime(),
        };

        client
            .configure_remora_link(
                Box::new(CountingEmptyJournal {
                    loads: Arc::clone(&loads),
                }),
                Box::new(CandidateTransportIdentity),
                Box::new(UnusedDeviceKeys),
            )
            .await
            .unwrap();

        assert!(
            loads.load(Ordering::SeqCst) >= 3,
            "configuration preflight, recovery, and reconnect must all read durable authority"
        );
        client.clear_remora_link().await;
    }

    #[tokio::test]
    async fn failed_host_mutation_still_disconnects_and_marks_runtimes_unavailable() {
        let client = crate::MobileClient::new();
        let host_id = "remora-link:node-a";
        let config = ServerConfig {
            server_id: host_id.to_string(),
            display_name: "Host A".to_string(),
            host: "node-a".to_string(),
            port: 0,
            websocket_url: None,
            is_local: false,
            tls: false,
        };
        client
            .app_store
            .upsert_server(&config, ServerHealthSnapshot::Connected);
        client.app_store.update_server_agent_runtimes(
            host_id,
            vec![AgentRuntimeInfo {
                kind: "codex".to_string(),
                name: "codex".to_string(),
                display_name: "Codex".to_string(),
                available: true,
            }],
        );

        let outcome = complete_remora_link_host_mutation(&client, host_id, async {
            Err::<(), _>(LifecycleErrorV2::JournalUnavailable)
        })
        .await;

        assert!(matches!(outcome, Err(RemoraLinkError::JournalUnavailable)));
        let snapshot = client.app_store.snapshot();
        let server = snapshot.servers.get(host_id).unwrap();
        assert_eq!(server.health, ServerHealthSnapshot::Disconnected);
        assert!(!server.agent_runtimes[0].available);
    }

    #[tokio::test]
    async fn clearing_projects_even_a_store_only_remora_link_host_disconnected() {
        let client = crate::MobileClient::new();
        let host_id = "remora-link:store-only";
        client.app_store.upsert_server(
            &ServerConfig {
                server_id: host_id.to_string(),
                display_name: "Store Only".to_string(),
                host: "node".to_string(),
                port: 0,
                websocket_url: None,
                is_local: false,
                tls: false,
            },
            ServerHealthSnapshot::Connected,
        );

        client.disconnect_all_remora_link_sessions().await;

        assert_eq!(
            client.app_store.snapshot().servers[host_id].health,
            ServerHealthSnapshot::Disconnected
        );
    }

    #[test]
    fn cancelled_runtime_attach_cannot_leave_connecting_projected() {
        let app_store = Arc::new(crate::store::AppStoreReducer::new());
        let host_id = "remora-link:node-a";
        let config = ServerConfig {
            server_id: host_id.to_string(),
            display_name: "Host A".to_string(),
            host: "node-a".to_string(),
            port: 0,
            websocket_url: None,
            is_local: false,
            tls: false,
        };
        app_store.upsert_server(&config, ServerHealthSnapshot::Connecting);

        drop(RemoraLinkConnectingProjectionGuard::new(
            Arc::clone(&app_store),
            host_id.to_string(),
        ));

        assert_eq!(
            app_store.snapshot().servers[host_id].health,
            ServerHealthSnapshot::Disconnected
        );
    }

    #[tokio::test]
    async fn pairing_cancellation_requires_configuration() {
        let client = AppClient {
            inner: crate::MobileClient::new(),
            rt: crate::ffi::shared::shared_runtime(),
        };

        assert!(matches!(
            client
                .cancel_remora_link_pairing("remora-link:missing".to_string())
                .await,
            Err(RemoraLinkError::NotConfigured)
        ));
    }

    #[test]
    fn pairing_cancellation_projects_only_rollback_outcomes() {
        assert_eq!(
            project_pairing_cancellation(MutationOutcomeV2::RolledBack).unwrap(),
            AppRemoraLinkPairingCancellationOutcome::Cancelled
        );
        assert_eq!(
            project_pairing_cancellation(MutationOutcomeV2::OutcomeUnknown).unwrap(),
            AppRemoraLinkPairingCancellationOutcome::OutcomeUnknown
        );
        assert!(matches!(
            project_pairing_cancellation(MutationOutcomeV2::Revoked),
            Err(RemoraLinkError::ProtocolViolation)
        ));
    }

    #[test]
    fn pairing_cancellation_removes_only_the_target_hosts_cached_invitation() {
        let mut cache = InvitationCache::default();
        for (offer, invite) in [cached_offer("a", 1), cached_offer("b", 2)] {
            cache.by_host.insert(offer.host_id.clone(), invite.clone());
            cache
                .by_offer
                .insert(offer.offer_id.clone(), CachedInvitation { offer, invite });
        }

        cache.remove_host("remora-link:node-a");

        assert!(!cache.by_host.contains_key("remora-link:node-a"));
        assert!(cache.by_host.contains_key("remora-link:node-b"));
        assert!(!cache.by_offer.contains_key("offer-a"));
        assert!(cache.by_offer.contains_key("offer-b"));
    }

    #[test]
    fn runtime_attachments_preserve_concurrent_same_runtime_identity() {
        let now = Instant::now();
        let mut attachments = RetainedAttachmentRegistryV2::new(4, Duration::from_secs(30));
        attachments
            .insert(
                "attachment-a".to_string(),
                "host-a".to_string(),
                "codex".to_string(),
                1,
                now,
            )
            .unwrap();
        attachments
            .insert(
                "attachment-b".to_string(),
                "host-a".to_string(),
                "codex".to_string(),
                2,
                now,
            )
            .unwrap();

        assert_eq!(
            attachments.take("attachment-b", "host-a", "codex", now),
            Ok(2)
        );
        assert_eq!(
            attachments.take("attachment-a", "host-a", "codex", now),
            Ok(1)
        );
    }

    #[test]
    fn runtime_plan_uses_authenticated_wire_and_excludes_shell() {
        let selected = vec![
            "codex".to_string(),
            "shell".to_string(),
            "pi".to_string(),
            "missing".to_string(),
        ];
        let agents = vec![
            AgentInfoV2 {
                name: "codex".to_string(),
                display_name: "Codex".to_string(),
                wire: AgentWireV2::Websocket,
                available: true,
                presentation: None,
                capabilities: None,
            },
            AgentInfoV2 {
                name: "pi".to_string(),
                display_name: "Pi".to_string(),
                wire: AgentWireV2::Jsonl,
                available: false,
                presentation: None,
                capabilities: None,
            },
        ];

        assert_eq!(
            remora_link_runtime_plan(&selected, &agents),
            vec![
                RemoraLinkRuntimePlanEntry {
                    runtime_id: "codex".to_string(),
                    display_name: "Codex".to_string(),
                    wire: Some(AgentWireV2::Websocket),
                    advertised_available: true,
                },
                RemoraLinkRuntimePlanEntry {
                    runtime_id: "pi".to_string(),
                    display_name: "Pi".to_string(),
                    wire: Some(AgentWireV2::Jsonl),
                    advertised_available: false,
                },
                RemoraLinkRuntimePlanEntry {
                    runtime_id: "missing".to_string(),
                    display_name: "missing".to_string(),
                    wire: None,
                    advertised_available: false,
                },
            ]
        );
    }

    #[test]
    fn coding_runtime_selection_requires_inspect_connect_and_self_revoke() {
        let runtimes = vec!["codex".to_string()];
        assert!(matches!(
            validate_coding_runtime_scope_selection(
                &runtimes,
                &[
                    AppRemoraLinkScope::ConnectRuntime,
                    AppRemoraLinkScope::SelfRevoke,
                ],
            ),
            Err(RemoraLinkError::InvalidSelection)
        ));
        assert!(
            validate_coding_runtime_scope_selection(
                &runtimes,
                &[
                    AppRemoraLinkScope::InspectRuntimes,
                    AppRemoraLinkScope::ConnectRuntime,
                    AppRemoraLinkScope::SelfRevoke,
                ],
            )
            .is_ok()
        );
    }

    #[tokio::test]
    async fn paired_outcome_invokes_the_canonical_attach_seam_once() {
        let attacher = RecordingRuntimeAttacher::default();
        let outcome = AppRemoraLinkPairingOutcome::Paired {
            host_id: "remora-link:node-a".to_string(),
            sas: "123456".to_string(),
            selected_runtime_ids: vec!["codex".to_string()],
            granted_scopes: vec![
                AppRemoraLinkScope::InspectRuntimes,
                AppRemoraLinkScope::ConnectRuntime,
                AppRemoraLinkScope::SelfRevoke,
            ],
            created_at_unix_ms: 1,
        };

        assert!(
            attach_after_pairing_outcome(&attacher, &outcome)
                .await
                .is_some()
        );
        assert_eq!(
            *attacher.calls.lock().unwrap(),
            vec![("remora-link:node-a".to_string(), false)]
        );
    }

    #[tokio::test]
    async fn paired_outcome_surfaces_canonical_attach_failure_for_retry() {
        let attacher = RecordingRuntimeAttacher::default();
        attacher
            .failures
            .lock()
            .unwrap()
            .insert("remora-link:node-a".to_string());
        let outcome = AppRemoraLinkPairingOutcome::AlreadyPaired {
            host_id: "remora-link:node-a".to_string(),
            selected_runtime_ids: vec!["codex".to_string()],
            granted_scopes: vec![AppRemoraLinkScope::ConnectRuntime],
        };

        assert!(matches!(
            require_pairing_attachment(&attacher, &outcome).await,
            Err(RemoraLinkError::RuntimeUnavailable)
        ));
    }

    #[tokio::test]
    async fn cold_restore_uses_one_forced_attach_per_host_and_projects_degraded_runtimes() {
        let attacher = Arc::new(RecordingRuntimeAttacher::default());
        attacher.outcomes.lock().unwrap().insert(
            "remora-link:node-a".to_string(),
            RemoraLinkRuntimeConnectOutcome {
                server_id: "remora-link:node-a".to_string(),
                connected_runtime_ids: vec!["codex".to_string()],
                unavailable_runtime_ids: vec!["pi".to_string()],
                connected_results: vec![AppRemoraLinkReconnectResult {
                    host_id: "remora-link:node-a".to_string(),
                    runtime_id: "codex".to_string(),
                    attached: AppRemoraLinkAttachKind::Fresh,
                    current_sequence: 7,
                    floor_sequence: 4,
                }],
            },
        );
        attacher.outcomes.lock().unwrap().insert(
            "remora-link:node-b".to_string(),
            RemoraLinkRuntimeConnectOutcome {
                server_id: "remora-link:node-b".to_string(),
                connected_runtime_ids: Vec::new(),
                unavailable_runtime_ids: vec!["opencode".to_string()],
                connected_results: Vec::new(),
            },
        );
        let work = vec![
            RemoraLinkRuntimeRestoreWork {
                host_id: "remora-link:node-a".to_string(),
                runtime_ids: vec!["codex".to_string(), "pi".to_string()],
            },
            RemoraLinkRuntimeRestoreWork {
                host_id: "remora-link:node-b".to_string(),
                runtime_ids: vec!["opencode".to_string()],
            },
        ];

        let mut attempts = restore_runtime_sessions(
            attacher.clone(),
            work,
            tokio::time::Instant::now() + Duration::from_secs(1),
        )
        .await;
        attempts
            .sort_by(|left, right| reconnect_attempt_key(left).cmp(&reconnect_attempt_key(right)));
        let mut calls = attacher.calls.lock().unwrap().clone();
        calls.sort();

        assert_eq!(
            calls,
            vec![
                ("remora-link:node-a".to_string(), true),
                ("remora-link:node-b".to_string(), true),
            ]
        );
        assert!(matches!(
            &attempts[0],
            AppRemoraLinkReconnectAttempt::Connected { result }
                if result.host_id == "remora-link:node-a" && result.runtime_id == "codex"
        ));
        assert!(matches!(
            &attempts[1],
            AppRemoraLinkReconnectAttempt::Failed {
                host_id,
                runtime_id,
                failure: AppRemoraLinkFailure::RuntimeUnavailable,
            } if host_id == "remora-link:node-a" && runtime_id == "pi"
        ));
        assert!(matches!(
            &attempts[2],
            AppRemoraLinkReconnectAttempt::Failed {
                host_id,
                runtime_id,
                failure: AppRemoraLinkFailure::RuntimeUnavailable,
            } if host_id == "remora-link:node-b" && runtime_id == "opencode"
        ));
    }

    #[test]
    fn cold_restore_schedules_one_canonical_session_per_host_and_excludes_shell() {
        let host_a_runtimes = (0..9).map(|index| format!("a-{index}")).collect();
        let entries = vec![
            enrolled_entry("a", 1, host_a_runtimes),
            enrolled_entry(
                "b",
                2,
                vec!["b-0".to_string(), "shell".to_string(), "Shell".to_string()],
            ),
            enrolled_entry("c", 3, vec!["c-0".to_string()]),
        ];
        let work = runtime_restore_work(&entries);

        assert_eq!(work.len(), 3);
        assert_eq!(
            work.iter()
                .map(|work| work.host_id.as_str())
                .collect::<Vec<_>>(),
            vec![
                "remora-link:node-a",
                "remora-link:node-b",
                "remora-link:node-c"
            ]
        );
        assert_eq!(
            work[1].runtime_ids,
            vec!["Shell".to_string(), "b-0".to_string()],
            "only the exact reserved shell ID is excluded"
        );
    }

    #[test]
    fn expired_reconnect_deadline_returns_explicit_cancelled_attempts() {
        let batch = cancelled_reconnect_batch(
            vec![
                enrolled_entry("b", 2, vec!["z".to_string(), "y".to_string()]),
                enrolled_entry("a", 1, vec!["x".to_string()]),
            ],
            Vec::new(),
        );
        let attempts: Vec<_> = batch
            .attempts
            .into_iter()
            .map(|attempt| match attempt {
                AppRemoraLinkReconnectAttempt::Failed {
                    host_id,
                    runtime_id,
                    failure: AppRemoraLinkFailure::Cancelled,
                } => (host_id, runtime_id),
                other => panic!("deadline fallback must be an explicit cancellation: {other:?}"),
            })
            .collect();
        assert_eq!(
            attempts,
            vec![
                ("remora-link:node-a".to_string(), "x".to_string()),
                ("remora-link:node-b".to_string(), "y".to_string()),
                ("remora-link:node-b".to_string(), "z".to_string()),
            ]
        );
    }

    #[test]
    fn pairing_code_carrier_is_bounded_redacted_and_rejects_non_utf8() {
        let secret = b"remora-link:2.secret".to_vec();
        let carrier = AppRemoraLinkPairingCode::from(secret.clone());
        assert!(!format!("{carrier:?}").contains("secret"));
        assert!(matches!(
            decode_code(carrier),
            Err(RemoraLinkError::InvalidPairingCode)
        ));
        assert!(matches!(
            decode_code(AppRemoraLinkPairingCode::from(vec![0xff])),
            Err(RemoraLinkError::InvalidPairingCode)
        ));
        assert!(matches!(
            decode_code(AppRemoraLinkPairingCode::from(vec![
                b'x';
                MAX_PAIRING_CODE_BYTES
                    + 1
            ])),
            Err(RemoraLinkError::InvalidPairingCode)
        ));
    }

    #[test]
    fn public_scope_round_trip_is_closed() {
        let scopes = [
            AppRemoraLinkScope::InspectRuntimes,
            AppRemoraLinkScope::ConnectRuntime,
            AppRemoraLinkScope::RestartRuntime,
            AppRemoraLinkScope::SelfRevoke,
        ];
        for scope in scopes {
            assert_eq!(AppRemoraLinkScope::from(DeviceScopeV2::from(scope)), scope);
        }
    }

    #[test]
    fn device_key_adapter_rejects_non_sec1_material() {
        let key = AppRemoraLinkHardwareKey {
            slot: "remora-link:test".to_string(),
            public_key_sec1: vec![4; 65],
            assurance: AppRemoraLinkKeyAssurance::SecureEnclave,
        };
        assert_eq!(
            validate_native_hardware_key(key),
            Err(CredentialPortError::Unavailable)
        );
    }

    #[test]
    fn software_key_assurance_is_debug_only() {
        assert!(key_assurance_is_allowed(
            AppRemoraLinkKeyAssurance::SoftwareDebugOnly,
            true
        ));
        assert!(!key_assurance_is_allowed(
            AppRemoraLinkKeyAssurance::SoftwareDebugOnly,
            false
        ));
    }

    #[test]
    fn secure_key_assurances_are_release_allowed_without_claiming_unknown_is_tee() {
        assert_ne!(
            AppRemoraLinkKeyAssurance::UnknownSecure,
            AppRemoraLinkKeyAssurance::TrustedExecutionEnvironment
        );
        for assurance in [
            AppRemoraLinkKeyAssurance::SecureEnclave,
            AppRemoraLinkKeyAssurance::StrongBox,
            AppRemoraLinkKeyAssurance::TrustedExecutionEnvironment,
            AppRemoraLinkKeyAssurance::UnknownSecure,
        ] {
            assert!(key_assurance_is_allowed(assurance, false));
        }
    }
}
