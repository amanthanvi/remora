//! `ServerSession` state machine for connection lifecycle management.
//!
//! Manages connection health, retry logic, auth flow, sandbox fallback,
//! and initialize handshake for a single Codex server.
//!
//! Uses upstream `RemoteAppServerClient` for remote connections and
//! upstream `InProcessClientHandle` for local (in-process) connections.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use codex_app_server_client::{
    AppServerClient, AppServerEvent, RemoteAppServerClient, RemoteAppServerConnectArgs,
    RemoteAppServerEndpoint,
};
use codex_app_server_protocol::{
    ClientNotification, ClientRequest, JSONRPCErrorError, RequestId, Result as JsonRpcResult,
    ServerNotification, ServerRequest,
};
use serde_json::Value as JsonValue;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tracing::{debug, info, warn};

use crate::logging::{LogLevelName, log_rust};
use crate::session::remote_transport::{
    Reconnected, RemoteTransport, ReplayOutcome, SessionKeepalive,
};
use crate::ssh::{RemoteShell, SshBootstrapResult, SshBootstrapTransport, SshClient};
use crate::transport::{RpcError, TransportError};
use crate::types::AgentRuntimeKind;

const REMOTE_RECONNECT_MAX_ATTEMPTS: u32 = 5;
const REMOTE_RECONNECT_BASE_DELAY: Duration = Duration::from_millis(250);
const REMOTE_RECONNECT_MAX_DELAY: Duration = Duration::from_secs(8);
const CONNECTION_TIMELINE_CAPACITY: usize = 128;
const OPENAI_BASE_URL_ENV_KEY: &str = "OPENAI_BASE_URL";
const APP_SERVER_PROXY_WEBSOCKET_URL: &str = "ws://codex-app-server-proxy.localhost/rpc";

#[derive(Clone)]
pub(crate) struct SshReconnectTransport {
    pub(crate) ssh_client: Arc<SshClient>,
    pub(crate) mode: SshReconnectMode,
    pub(crate) codex_path: String,
    pub(crate) remote_shell: RemoteShell,
    pub(crate) working_dir: Option<String>,
    pub(crate) ssh_pid: Option<Arc<StdMutex<Option<u32>>>>,
}

#[derive(Clone)]
pub(crate) struct SlingshotReconnectTransport {
    pub(crate) api: codex_slingshot::SlingshotApi,
    pub(crate) environment_id: String,
}

#[derive(Clone)]
pub(crate) enum SshReconnectMode {
    AppServerProxy {
        codex_path: String,
        remote_shell: RemoteShell,
    },
    WebSocketTunnel {
        local_port: u16,
        remote_port: Arc<StdMutex<u16>>,
        prefer_ipv6: bool,
    },
}

impl SshReconnectTransport {
    pub(crate) fn from_bootstrap(
        ssh_client: Arc<SshClient>,
        bootstrap: &SshBootstrapResult,
        working_dir: Option<String>,
        prefer_ipv6: bool,
        ssh_pid: Arc<StdMutex<Option<u32>>>,
    ) -> Self {
        let mode = match bootstrap.transport {
            SshBootstrapTransport::AppServerProxy => SshReconnectMode::AppServerProxy {
                codex_path: bootstrap.codex_path.clone(),
                remote_shell: bootstrap.shell,
            },
            SshBootstrapTransport::WebSocketTunnel => SshReconnectMode::WebSocketTunnel {
                local_port: bootstrap.tunnel_local_port,
                remote_port: Arc::new(StdMutex::new(bootstrap.server_port)),
                prefer_ipv6,
            },
        };
        Self {
            ssh_client,
            mode,
            codex_path: bootstrap.codex_path.clone(),
            remote_shell: bootstrap.shell,
            working_dir,
            ssh_pid: Some(ssh_pid),
        }
    }
}

#[derive(Clone)]
struct WebSocketReconnectState {
    pub(crate) local_port: u16,
    pub(crate) remote_port: Arc<StdMutex<u16>>,
    pub(crate) prefer_ipv6: bool,
}

fn append_android_debug_log(line: &str) {
    log_rust(
        LogLevelName::Debug,
        "session.connection",
        "bridge",
        line.to_string(),
        None,
    );
}

fn openai_base_url_from_env() -> Option<String> {
    std::env::var(OPENAI_BASE_URL_ENV_KEY)
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
}

// ---------------------------------------------------------------------------
// InProcessConfig
// ---------------------------------------------------------------------------

/// Configuration for starting an in-process Codex transport.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct InProcessConfig {
    /// Override the Codex home directory.
    pub codex_home: Option<PathBuf>,
    /// Override the working directory for Codex operations.
    pub working_directory: Option<PathBuf>,
    /// Capacity for internal event/command channels. Defaults to 256.
    pub channel_capacity: usize,
}

impl Default for InProcessConfig {
    fn default() -> Self {
        Self {
            codex_home: None,
            working_directory: None,
            channel_capacity: 256,
        }
    }
}

#[cfg(any(all(target_os = "ios", not(target_abi = "macabi")), test))]
static IOS_CACERT_PEM: &[u8] = include_bytes!("../cacert.pem");

#[allow(unused_mut)]
fn prepare_in_process_config(
    mut config: InProcessConfig,
) -> Result<InProcessConfig, TransportError> {
    #[cfg(all(target_os = "ios", not(target_abi = "macabi")))]
    {
        config = prepare_ios_in_process_config(config)?;
    }

    #[cfg(target_os = "android")]
    {
        config = prepare_android_in_process_config(config)?;
    }

    Ok(config)
}

#[cfg(target_os = "android")]
fn prepare_android_in_process_config(
    mut config: InProcessConfig,
) -> Result<InProcessConfig, TransportError> {
    // On Android, HOME and CODEX_HOME are set by UniffiInit.nativeMobileClientInit().
    // If codex_home is not set in the config, resolve from CODEX_HOME env var.
    if config.codex_home.is_none() {
        if let Ok(codex_home) = std::env::var("CODEX_HOME") {
            let path = PathBuf::from(&codex_home);
            std::fs::create_dir_all(&path).map_err(|e| {
                TransportError::ConnectionFailed(format!(
                    "failed to create CODEX_HOME {:?}: {e}",
                    path
                ))
            })?;
            config.codex_home = Some(path);
        } else if let Ok(home) = std::env::var("HOME") {
            let path = PathBuf::from(home).join(".codex");
            std::fs::create_dir_all(&path).map_err(|e| {
                TransportError::ConnectionFailed(format!(
                    "failed to create codex home {:?}: {e}",
                    path
                ))
            })?;
            unsafe {
                std::env::set_var("CODEX_HOME", &path);
            }
            config.codex_home = Some(path);
        } else {
            return Err(TransportError::ConnectionFailed(
                "Could not find home directory".to_string(),
            ));
        }
    }

    if config.working_directory.is_none() {
        if let Some(ref codex_home) = config.codex_home {
            let wd = codex_home.join("workspace");
            std::fs::create_dir_all(&wd).map_err(|e| {
                TransportError::ConnectionFailed(format!(
                    "failed to create workspace {:?}: {e}",
                    wd
                ))
            })?;
            config.working_directory = Some(wd);
        }
    }

    // Set up TLS root certificates for Android
    if let Some(ref codex_home) = config.codex_home {
        // Android uses system CAs, but set SSL_CERT_FILE if a bundle exists
        let pem_path = codex_home.join("cacert.pem");
        if pem_path.exists() {
            unsafe {
                std::env::set_var("SSL_CERT_FILE", &pem_path);
            }
        }
    }

    Ok(config)
}

#[cfg(any(all(target_os = "ios", not(target_abi = "macabi")), test))]
#[cfg_attr(test, allow(dead_code))]
fn prepare_ios_in_process_config(
    mut config: InProcessConfig,
) -> Result<InProcessConfig, TransportError> {
    let home_dir = std::env::var_os("HOME").map(PathBuf::from);
    let docs_root = home_dir.as_ref().map(|home| home.join("Documents"));

    if let Some(root) = &docs_root {
        for relative in ["home/codex", "tmp", "var/log", "etc"] {
            std::fs::create_dir_all(root.join(relative)).map_err(|e| {
                TransportError::ConnectionFailed(format!(
                    "failed to create local sandbox directory {:?}: {e}",
                    root.join(relative)
                ))
            })?;
        }
    }

    if config.working_directory.is_none()
        && let Some(root) = &docs_root
    {
        config.working_directory = Some(root.join("home").join("codex"));
    }

    if let Some(ref working_directory) = config.working_directory {
        std::fs::create_dir_all(working_directory).map_err(|e| {
            TransportError::ConnectionFailed(format!(
                "failed to create local working directory {:?}: {e}",
                working_directory
            ))
        })?;
        unsafe {
            std::env::set_var("SSH_HOME", working_directory);
            std::env::set_var("CURL_HOME", working_directory);
        }
    }

    if config.codex_home.is_none() {
        config.codex_home = Some(resolve_ios_codex_home(&home_dir)?);
    }

    if let Some(ref codex_home) = config.codex_home {
        config.codex_home = Some(prepare_ios_runtime_environment(codex_home)?);
    }

    Ok(config)
}

#[cfg(any(all(target_os = "ios", not(target_abi = "macabi")), test))]
#[cfg_attr(test, allow(dead_code))]
fn resolve_ios_codex_home(home_dir: &Option<PathBuf>) -> Result<PathBuf, TransportError> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Ok(existing) = std::env::var("CODEX_HOME")
        && !existing.is_empty()
    {
        candidates.push(PathBuf::from(existing));
    }

    if let Some(home) = home_dir {
        candidates.push(
            home.join("Library")
                .join("Application Support")
                .join("codex"),
        );
        candidates.push(home.join("Documents").join(".codex"));
        candidates.push(home.join(".codex"));
    }

    if let Ok(tmpdir) = std::env::var("TMPDIR") {
        candidates.push(PathBuf::from(tmpdir).join("codex-home"));
    }

    for candidate in candidates {
        match std::fs::create_dir_all(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(err) => {
                warn!(
                    "failed to create CODEX_HOME candidate {:?}: {err}",
                    candidate
                );
            }
        }
    }

    Err(TransportError::ConnectionFailed(
        "unable to initialize any writable CODEX_HOME location".to_string(),
    ))
}

#[cfg(any(all(target_os = "ios", not(target_abi = "macabi")), test))]
fn prepare_ios_runtime_environment(
    codex_home: &std::path::Path,
) -> Result<PathBuf, TransportError> {
    std::fs::create_dir_all(codex_home).map_err(|e| {
        TransportError::ConnectionFailed(format!(
            "failed to create CODEX_HOME {:?}: {e}",
            codex_home
        ))
    })?;

    let canonical = codex_home
        .canonicalize()
        .unwrap_or_else(|_| codex_home.to_path_buf());
    unsafe {
        std::env::set_var("CODEX_HOME", &canonical);
    }
    init_ios_tls_roots(&canonical)?;

    Ok(canonical)
}

#[cfg(any(all(target_os = "ios", not(target_abi = "macabi")), test))]
fn init_ios_tls_roots(codex_home: &std::path::Path) -> Result<(), TransportError> {
    if let Some(existing) = std::env::var_os("SSL_CERT_FILE") {
        let existing_path = std::path::PathBuf::from(existing);
        if existing_path.is_file() {
            return Ok(());
        }
        warn!(
            "replacing stale SSL_CERT_FILE {:?} with a regenerated local bundle",
            existing_path
        );
    }

    let pem_path = codex_home.join("cacert.pem");
    if !pem_path.exists() {
        std::fs::write(&pem_path, IOS_CACERT_PEM).map_err(|e| {
            TransportError::ConnectionFailed(format!(
                "failed to write local TLS roots {:?}: {e}",
                pem_path
            ))
        })?;
    }

    unsafe {
        std::env::set_var("SSL_CERT_FILE", &pem_path);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// ServerConfig
// ---------------------------------------------------------------------------

/// Configuration describing a Codex server endpoint.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfig {
    /// Unique identifier for this server.
    pub server_id: String,
    /// Human-readable name shown in the UI.
    pub display_name: String,
    /// Hostname or IP address.
    pub host: String,
    /// TCP port.
    pub port: u16,
    /// Explicit WebSocket URL override for remote connections.
    pub websocket_url: Option<String>,
    /// Whether this is a local (in-process) server.
    pub is_local: bool,
    /// Whether to use TLS for the WebSocket connection.
    pub tls: bool,
}

/// Session-wide bookkeeping passed to `connect_remote_multiplexed`.
///
/// These fields back side-channels (SSH client retained for log commands and
/// disconnect cleanup) that live on `ServerSession` for the
/// lifetime of the session. They are independent of any single runtime's RPC
/// transport — that's described per-runtime by `RuntimeRemoteSessionResource`.
#[derive(Default)]
pub struct RemoteSessionExtras {
    pub ssh_client: Option<Arc<SshClient>>,
    pub ssh_pid: Option<Arc<StdMutex<Option<u32>>>>,
}

pub struct RuntimeRemoteSessionResource {
    pub runtime_kind: AgentRuntimeKind,
    pub client: AppServerClient,
    pub(crate) transport: Option<Arc<dyn RemoteTransport>>,
    pub(crate) keepalive: Option<Arc<dyn SessionKeepalive>>,
}

// ---------------------------------------------------------------------------
// ConnectionHealth
// ---------------------------------------------------------------------------

/// Observable health state of the connection to a server.
#[derive(Debug, Clone)]
pub enum ConnectionHealth {
    Disconnected,
    Connecting { attempt: u32, max_attempts: u32 },
    Connected,
    Unresponsive { since: Instant },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ReconnectHealthState {
    pub aggregate: ConnectionHealth,
    pub has_degraded_runtime: bool,
    pub has_connecting_runtime: bool,
    pub generation: u64,
}

impl PartialEq for ConnectionHealth {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Disconnected, Self::Disconnected) => true,
            (
                Self::Connecting {
                    attempt: a1,
                    max_attempts: m1,
                },
                Self::Connecting {
                    attempt: a2,
                    max_attempts: m2,
                },
            ) => a1 == a2 && m1 == m2,
            (Self::Connected, Self::Connected) => true,
            (Self::Unresponsive { since: s1 }, Self::Unresponsive { since: s2 }) => s1 == s2,
            _ => false,
        }
    }
}

/// Why the session worker entered transport recovery. These values are used
/// only in the app's bounded local timeline and structured local logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConnectionTrigger {
    RequestTransportError,
    EventStreamEnded,
    TransportDisconnected,
}

impl ConnectionTrigger {
    fn as_str(self) -> &'static str {
        match self {
            Self::RequestTransportError => "request_transport_error",
            Self::EventStreamEnded => "event_stream_ended",
            Self::TransportDisconnected => "transport_disconnected",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConnectionStage {
    AttemptStarted,
    DialFinished,
    BackoffScheduled,
    Ready,
    Exhausted,
}

impl ConnectionStage {
    fn as_str(self) -> &'static str {
        match self {
            Self::AttemptStarted => "attempt_started",
            Self::DialFinished => "dial_finished",
            Self::BackoffScheduled => "backoff_scheduled",
            Self::Ready => "ready",
            Self::Exhausted => "exhausted",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConnectionAttemptOutcome {
    Pending,
    Succeeded,
    TransientFailure,
    Exhausted,
}

impl ConnectionAttemptOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Succeeded => "succeeded",
            Self::TransientFailure => "transient_failure",
            Self::Exhausted => "exhausted",
        }
    }
}

/// Redacted entry in a monotonically-sequenced, process-local reconnect
/// timeline. It intentionally has no host, URL, node ID, token, account, or
/// payload field. The bounded ring supports deterministic regression tests
/// and local diagnostics without creating remote telemetry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConnectionTimelineEntry {
    pub sequence: u64,
    pub correlation_id: u64,
    pub generation: u64,
    pub trigger: ConnectionTrigger,
    pub stage: ConnectionStage,
    pub stage_elapsed_ms: u64,
    pub journey_elapsed_ms: u64,
    pub route: &'static str,
    pub runtime_kind: AgentRuntimeKind,
    pub attempt: u32,
    pub outcome: ConnectionAttemptOutcome,
}

#[derive(Default)]
struct ConnectionTimelineState {
    next_sequence: u64,
    entries: VecDeque<ConnectionTimelineEntry>,
}

#[derive(Clone, Default)]
struct ConnectionTimeline {
    state: Arc<StdMutex<ConnectionTimelineState>>,
}

static NEXT_CONNECTION_CORRELATION_ID: AtomicU64 = AtomicU64::new(1);

impl ConnectionTimeline {
    fn next_correlation_id(&self) -> u64 {
        NEXT_CONNECTION_CORRELATION_ID.fetch_add(1, Ordering::Relaxed)
    }

    #[allow(clippy::too_many_arguments)]
    fn record(
        &self,
        correlation_id: u64,
        generation: u64,
        trigger: ConnectionTrigger,
        stage: ConnectionStage,
        stage_elapsed: Duration,
        journey_elapsed: Duration,
        route: &'static str,
        runtime_kind: &AgentRuntimeKind,
        attempt: u32,
        outcome: ConnectionAttemptOutcome,
    ) {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(error) => {
                warn!("connection timeline: recovering poisoned lock");
                error.into_inner()
            }
        };
        state.next_sequence = state.next_sequence.saturating_add(1);
        let entry = ConnectionTimelineEntry {
            sequence: state.next_sequence,
            correlation_id,
            generation,
            trigger,
            stage,
            stage_elapsed_ms: duration_millis(stage_elapsed),
            journey_elapsed_ms: duration_millis(journey_elapsed),
            route,
            runtime_kind: runtime_kind.clone(),
            attempt,
            outcome,
        };
        if state.entries.len() == CONNECTION_TIMELINE_CAPACITY {
            state.entries.pop_front();
        }
        state.entries.push_back(entry.clone());
        drop(state);

        info!(
            connection_timeline = true,
            sequence = entry.sequence,
            correlation_id = entry.correlation_id,
            generation = entry.generation,
            trigger = entry.trigger.as_str(),
            stage = entry.stage.as_str(),
            stage_elapsed_ms = entry.stage_elapsed_ms,
            journey_elapsed_ms = entry.journey_elapsed_ms,
            route = entry.route,
            runtime = entry.runtime_kind.as_str(),
            attempt = entry.attempt,
            outcome = entry.outcome.as_str(),
            "connection timeline"
        );
    }

    #[cfg(test)]
    fn snapshot(&self) -> Vec<ConnectionTimelineEntry> {
        match self.state.lock() {
            Ok(state) => state.entries.iter().cloned().collect(),
            Err(error) => error.into_inner().entries.iter().cloned().collect(),
        }
    }
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[derive(Clone)]
struct RuntimeHealthReporter {
    runtime_kind: AgentRuntimeKind,
    state: Arc<StdMutex<RuntimeHealthState>>,
    session_health_tx: watch::Sender<ConnectionHealth>,
}

struct RuntimeHealthState {
    by_runtime: HashMap<AgentRuntimeKind, ConnectionHealth>,
    generation: u64,
    cold_repair_claimed: bool,
}

impl RuntimeHealthReporter {
    fn update(&self, health: ConnectionHealth) -> bool {
        let aggregate = {
            let mut state = match self.state.lock() {
                Ok(state) => state,
                Err(error) => {
                    warn!("runtime health: recovering poisoned lock");
                    error.into_inner()
                }
            };
            if state.cold_repair_claimed {
                return false;
            }
            state.by_runtime.insert(self.runtime_kind.clone(), health);
            state.generation = state.generation.saturating_add(1);
            aggregate_runtime_health(state.by_runtime.values())
        };
        let _ = self.session_health_tx.send(aggregate);
        true
    }
}

fn aggregate_runtime_health<'a>(
    health: impl IntoIterator<Item = &'a ConnectionHealth>,
) -> ConnectionHealth {
    let health = health.into_iter().collect::<Vec<_>>();
    if health
        .iter()
        .any(|value| matches!(value, ConnectionHealth::Connected))
    {
        // A multiplexed server remains useful while at least one selected
        // runtime is ready. Individual reconnect events still drive
        // per-runtime replay/reconciliation, so a failed sibling cannot hide.
        return ConnectionHealth::Connected;
    }

    let connecting = health.iter().filter_map(|value| match value {
        ConnectionHealth::Connecting {
            attempt,
            max_attempts,
        } => Some((*attempt, *max_attempts)),
        _ => None,
    });
    if let Some((attempt, max_attempts)) = connecting.max_by_key(|value| value.0) {
        return ConnectionHealth::Connecting {
            attempt,
            max_attempts,
        };
    }

    if let Some(since) = health
        .iter()
        .filter_map(|value| match value {
            ConnectionHealth::Unresponsive { since } => Some(*since),
            _ => None,
        })
        .min()
    {
        return ConnectionHealth::Unresponsive { since };
    }

    ConnectionHealth::Disconnected
}

fn runtime_health_is_degraded(health: &HashMap<AgentRuntimeKind, ConnectionHealth>) -> bool {
    health.values().any(|value| {
        matches!(
            value,
            ConnectionHealth::Disconnected | ConnectionHealth::Unresponsive { .. }
        )
    })
}

fn runtime_health_has_connecting(health: &HashMap<AgentRuntimeKind, ConnectionHealth>) -> bool {
    health
        .values()
        .any(|value| matches!(value, ConnectionHealth::Connecting { .. }))
}

fn available_runtime_kinds_from_health(
    health: &HashMap<AgentRuntimeKind, ConnectionHealth>,
) -> Vec<AgentRuntimeKind> {
    let mut kinds = health
        .iter()
        .filter(|(_, health)| {
            matches!(
                health,
                ConnectionHealth::Connected | ConnectionHealth::Connecting { .. }
            )
        })
        .map(|(runtime_kind, _)| runtime_kind.clone())
        .collect::<Vec<_>>();
    kinds.sort();
    kinds
}

fn initial_runtime_health(
    requested_runtime_kinds: &[AgentRuntimeKind],
    connected_runtime_kinds: &HashSet<AgentRuntimeKind>,
) -> HashMap<AgentRuntimeKind, ConnectionHealth> {
    requested_runtime_kinds
        .iter()
        .cloned()
        .map(|runtime_kind| {
            let health = if connected_runtime_kinds.contains(&runtime_kind) {
                ConnectionHealth::Connected
            } else {
                ConnectionHealth::Disconnected
            };
            (runtime_kind, health)
        })
        .collect()
}

struct ReconnectBackoff {
    base: Duration,
    cap: Duration,
    state: u64,
}

impl ReconnectBackoff {
    fn production() -> Self {
        let bytes = uuid::Uuid::new_v4().into_bytes();
        let mut seed = 0_u64;
        for chunk in bytes.chunks_exact(8) {
            seed ^= u64::from_le_bytes(chunk.try_into().expect("uuid chunk length"));
        }
        Self::with_seed(seed)
    }

    fn with_seed(seed: u64) -> Self {
        Self {
            base: REMOTE_RECONNECT_BASE_DELAY,
            cap: REMOTE_RECONNECT_MAX_DELAY,
            state: seed.max(1),
        }
    }

    fn delay_after_failure(&mut self, failed_attempt: u32) -> Duration {
        let exponent = failed_attempt.saturating_sub(1).min(31);
        let multiplier = 1_u32.checked_shl(exponent).unwrap_or(u32::MAX);
        let window = self.base.saturating_mul(multiplier).min(self.cap);

        // Xorshift64*: compact, deterministic for tests, and sufficient for
        // desynchronizing retries. Production seeds come from OS-backed UUID
        // randomness; this is not used for cryptography.
        let mut value = self.state;
        value ^= value >> 12;
        value ^= value << 25;
        value ^= value >> 27;
        self.state = value;
        let sample = value.wrapping_mul(0x2545_F491_4F6C_DD1D);
        let window_millis = duration_millis(window);
        Duration::from_millis(sample % window_millis.saturating_add(1))
    }
}

// ---------------------------------------------------------------------------
// Internal command type for the worker task
// ---------------------------------------------------------------------------

#[allow(
    clippy::large_enum_variant,
    reason = "commands own protocol payloads while queued; boxing individual variants would complicate every request dispatch path"
)]
enum SessionCommand {
    Request {
        request: ClientRequest,
        response_tx: oneshot::Sender<Result<JsonValue, RpcError>>,
    },
    Notify {
        notification: ClientNotification,
        response_tx: oneshot::Sender<Result<(), RpcError>>,
    },
    Resolve {
        request_id: RequestId,
        result: JsonRpcResult,
        response_tx: oneshot::Sender<Result<(), RpcError>>,
    },
    Reject {
        request_id: RequestId,
        error: JSONRPCErrorError,
        response_tx: oneshot::Sender<Result<(), RpcError>>,
    },
    Shutdown,
}

// ---------------------------------------------------------------------------
// ServerSession
// ---------------------------------------------------------------------------

/// Typed event from the server: either a typed notification, a legacy notification,
/// or a typed server request.
#[derive(Debug, Clone)]
pub enum ServerEvent {
    Notification {
        runtime_kind: AgentRuntimeKind,
        notification: ServerNotification,
    },
    LegacyNotification {
        runtime_kind: AgentRuntimeKind,
        method: String,
        params: JsonValue,
    },
    Request {
        runtime_kind: AgentRuntimeKind,
        request: ServerRequest,
    },
    /// A runtime worker installed a replacement transport. Re-subscription is
    /// always required because the server-side connection ID changed; a
    /// replay drift additionally requires an authoritative state refresh.
    TransportReconnected {
        runtime_kind: AgentRuntimeKind,
        generation: u64,
        authoritative_refresh_required: bool,
    },
}

/// Manages the full connection lifecycle to a single Codex server.
///
/// Wraps the upstream `AppServerClient` (both in-process and remote variants)
/// behind a worker task that owns the client and multiplexes between command
/// dispatch and event consumption.
pub struct ServerSession {
    config: ServerConfig,
    health_tx: watch::Sender<ConnectionHealth>,
    health_rx: watch::Receiver<ConnectionHealth>,
    command_tx: mpsc::Sender<SessionCommand>,
    runtime_command_txs: std::collections::HashMap<AgentRuntimeKind, mpsc::Sender<SessionCommand>>,
    runtime_transports: Vec<Arc<dyn RemoteTransport>>,
    event_tx: broadcast::Sender<ServerEvent>,
    ssh_client: Option<Arc<SshClient>>,
    ssh_pid: Option<Arc<StdMutex<Option<u32>>>>,
    runtime_health_state: Option<Arc<StdMutex<RuntimeHealthState>>>,
    worker_handle: tokio::task::JoinHandle<()>,
}

#[cfg(test)]
pub(crate) type TestRequestHandler =
    Arc<dyn Fn(ClientRequest) -> Result<JsonValue, RpcError> + Send + Sync>;
#[cfg(test)]
pub(crate) type TestResolveHandler =
    Arc<dyn Fn(RequestId, JsonRpcResult) -> Result<(), RpcError> + Send + Sync>;
#[cfg(test)]
pub(crate) type TestRejectHandler =
    Arc<dyn Fn(RequestId, JSONRPCErrorError) -> Result<(), RpcError> + Send + Sync>;

#[cfg(test)]
fn spawn_test_command_worker(
    request_handler: Option<TestRequestHandler>,
    resolve_handler: Option<TestResolveHandler>,
    reject_handler: Option<TestRejectHandler>,
) -> (mpsc::Sender<SessionCommand>, tokio::task::JoinHandle<()>) {
    let (command_tx, mut command_rx) = mpsc::channel(16);
    let worker_handle = tokio::spawn(async move {
        while let Some(command) = command_rx.recv().await {
            match command {
                SessionCommand::Request {
                    request,
                    response_tx,
                } => {
                    let result = request_handler
                        .as_ref()
                        .map(|handler| handler(request))
                        .unwrap_or_else(|| Err(RpcError::Transport(TransportError::Disconnected)));
                    let _ = response_tx.send(result);
                }
                SessionCommand::Notify { response_tx, .. } => {
                    let _ = response_tx.send(Ok(()));
                }
                SessionCommand::Resolve {
                    request_id,
                    result,
                    response_tx,
                } => {
                    let outcome = resolve_handler
                        .as_ref()
                        .map(|handler| handler(request_id, result))
                        .unwrap_or(Ok(()));
                    let _ = response_tx.send(outcome);
                }
                SessionCommand::Reject {
                    request_id,
                    error,
                    response_tx,
                } => {
                    let outcome = reject_handler
                        .as_ref()
                        .map(|handler| handler(request_id, error))
                        .unwrap_or(Ok(()));
                    let _ = response_tx.send(outcome);
                }
                SessionCommand::Shutdown => break,
            }
        }
    });
    (command_tx, worker_handle)
}

impl ServerSession {
    /// Connect to a local (in-process) Codex server.
    pub async fn connect_local(
        config: ServerConfig,
        in_process: InProcessConfig,
    ) -> Result<Self, TransportError> {
        use codex_app_server::in_process::InProcessStartArgs;
        use codex_app_server_protocol::{ClientInfo, InitializeCapabilities, InitializeParams};
        use codex_arg0::Arg0DispatchPaths;
        use codex_cloud_requirements::cloud_requirements_loader;
        use codex_config::LoaderOverrides;
        use codex_core::config::ConfigBuilder;
        use codex_feedback::CodexFeedback;
        use codex_login::AuthManager;
        use codex_protocol::protocol::SessionSource;

        let (health_tx, health_rx) = watch::channel(ConnectionHealth::Connecting {
            attempt: 1,
            max_attempts: 1,
        });

        let in_process = prepare_in_process_config(in_process)?;

        // Apply codex_home override if provided.
        if let Some(ref codex_home) = in_process.codex_home {
            if let Err(e) = std::fs::create_dir_all(codex_home) {
                return Err(TransportError::ConnectionFailed(format!(
                    "failed to create codex_home {:?}: {e}",
                    codex_home
                )));
            }
            unsafe {
                std::env::set_var("CODEX_HOME", codex_home);
            }
        }

        if let Some(ref working_dir) = in_process.working_directory {
            std::env::set_current_dir(working_dir).map_err(|e| {
                TransportError::ConnectionFailed(format!(
                    "failed to set working directory {:?}: {e}",
                    working_dir
                ))
            })?;
        }

        let mut cli_overrides = vec![
            ("features.goals".to_string(), true.into()),
            ("features.realtime_conversation".to_string(), true.into()),
            (
                "experimental_realtime_ws_model".to_string(),
                "gpt-realtime-2".to_string().into(),
            ),
            ("realtime.version".to_string(), "v2".to_string().into()),
            (
                "realtime.type".to_string(),
                "conversational".to_string().into(),
            ),
        ];
        if let Some(base_url) = openai_base_url_from_env() {
            cli_overrides.push(("openai_base_url".to_string(), base_url.into()));
        }

        let mut base_builder = ConfigBuilder::default().cli_overrides(cli_overrides.clone());
        if let Some(ref codex_home) = in_process.codex_home {
            base_builder = base_builder.codex_home(codex_home.clone());
        }
        if let Some(ref working_dir) = in_process.working_directory {
            base_builder = base_builder.fallback_cwd(Some(working_dir.clone()));
        }

        let base_config = base_builder
            .build()
            .await
            .map_err(|e| TransportError::ConnectionFailed(format!("config build failed: {e}")))?;

        let auth_manager = AuthManager::shared(
            base_config.codex_home.to_path_buf(),
            false,
            base_config.cli_auth_credentials_store_mode,
            Some(base_config.chatgpt_base_url.clone()),
        )
        .await;

        let cloud_requirements = cloud_requirements_loader(
            auth_manager.clone(),
            base_config.chatgpt_base_url.clone(),
            base_config.codex_home.to_path_buf(),
        );

        let mut resolved_builder = ConfigBuilder::default()
            .cli_overrides(cli_overrides.clone())
            .cloud_requirements(cloud_requirements.clone());
        if let Some(ref codex_home) = in_process.codex_home {
            resolved_builder = resolved_builder.codex_home(codex_home.clone());
        }
        if let Some(ref working_dir) = in_process.working_directory {
            resolved_builder = resolved_builder.fallback_cwd(Some(working_dir.clone()));
        }

        let resolved_config = resolved_builder.build().await.unwrap_or(base_config);

        let feedback = CodexFeedback::new();
        let session_source = SessionSource::VSCode;

        let args = InProcessStartArgs {
            arg0_paths: Arg0DispatchPaths::default(),
            config: Arc::new(resolved_config),
            cli_overrides,
            loader_overrides: LoaderOverrides::default(),
            strict_config: false,
            cloud_requirements,
            feedback,
            log_db: None,
            state_db: None,
            thread_config_loader: Arc::new(codex_config::NoopThreadConfigLoader),
            environment_manager: Arc::new(
                codex_exec_server::EnvironmentManager::default_for_tests(),
            ),
            config_warnings: Vec::new(),
            session_source,
            enable_codex_api_key_env: true,
            initialize: InitializeParams {
                client_info: ClientInfo {
                    name: "Remora".to_string(),
                    version: "1.0".to_string(),
                    title: None,
                },
                capabilities: Some(InitializeCapabilities {
                    experimental_api: true,
                    request_attestation: false,
                    opt_out_notification_methods: None,
                }),
            },
            channel_capacity: in_process.channel_capacity,
        };

        let mut handle = codex_app_server::in_process::start(args)
            .await
            .map_err(|e| {
                TransportError::ConnectionFailed(format!("in-process start failed: {e}"))
            })?;

        let sender = handle.sender();
        let (event_tx, _) = broadcast::channel::<ServerEvent>(256);
        let (command_tx, mut command_rx) = mpsc::channel::<SessionCommand>(256);

        let evt_tx = event_tx.clone();

        let worker_handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    command = command_rx.recv() => {
                        let Some(command) = command else { break; };
                        match command {
                            SessionCommand::Request { request, response_tx } => {
                                let sender = sender.clone();
                                tokio::spawn(async move {
                                    let result = match sender.request(request).await {
                                        Ok(Ok(value)) => Ok(value),
                                        Ok(Err(error)) => Err(RpcError::Server {
                                            code: error.code,
                                            message: error.message,
                                        }),
                                        Err(e) => Err(RpcError::Transport(
                                            TransportError::SendFailed(e.to_string()),
                                        )),
                                    };
                                    let _ = response_tx.send(result);
                                });
                            }
                            SessionCommand::Notify { notification, response_tx } => {
                                let result = sender
                                    .notify(notification)
                                    .map_err(|e| {
                                        RpcError::Transport(TransportError::SendFailed(
                                            e.to_string(),
                                        ))
                                    });
                                let _ = response_tx.send(result);
                            }
                            SessionCommand::Resolve { request_id, result, response_tx } => {
                                let res = sender
                                    .respond_to_server_request(request_id, result)
                                    .map_err(|e| {
                                        RpcError::Transport(TransportError::SendFailed(
                                            e.to_string(),
                                        ))
                                    });
                                let _ = response_tx.send(res);
                            }
                            SessionCommand::Reject { request_id, error, response_tx } => {
                                let res = sender
                                    .fail_server_request(request_id, error)
                                    .map_err(|e| {
                                        RpcError::Transport(TransportError::SendFailed(
                                            e.to_string(),
                                        ))
                                    });
                                let _ = response_tx.send(res);
                            }
                            SessionCommand::Shutdown => {
                                break;
                            }
                        }
                    }
                    event = handle.next_event() => {
                        let Some(event) = event else { break; };
                        route_in_process_event(&evt_tx, event);
                    }
                }
            }
            debug!("in-process session worker exited");
        });

        let _ = health_tx.send(ConnectionHealth::Connected);
        info!("local server session connected: {}", config.display_name);

        Ok(Self {
            config,
            health_tx,
            health_rx,
            command_tx,
            runtime_command_txs: std::collections::HashMap::new(),
            runtime_transports: Vec::new(),
            event_tx,
            ssh_client: None,
            ssh_pid: None,
            runtime_health_state: None,
            worker_handle,
        })
    }

    /// Connect to a remote Codex server via plain WebSocket.
    ///
    /// Uses the upstream `RemoteAppServerClient` which handles the
    /// initialize/initialized handshake, request routing, and event streaming.
    pub async fn connect_remote(config: ServerConfig) -> Result<Self, TransportError> {
        let (_, args) = remote_connect_args(&config);
        let client = connect_remote_client(&args).await?;
        let resource = RuntimeRemoteSessionResource {
            runtime_kind: "codex".to_string(),
            client,
            transport: None,
            keepalive: None,
        };
        Self::connect_remote_multiplexed(config, vec![resource], RemoteSessionExtras::default())
            .await
    }

    pub async fn connect_remote_multiplexed(
        config: ServerConfig,
        resources: Vec<RuntimeRemoteSessionResource>,
        extras: RemoteSessionExtras,
    ) -> Result<Self, TransportError> {
        Self::connect_remote_multiplexed_with_unavailable(config, resources, Vec::new(), extras)
            .await
    }

    /// Build one multiplexed session while retaining selected runtimes whose
    /// initial authenticated attach failed. Failed runtimes have no worker and
    /// therefore no competing reconnect authority; they remain visible as
    /// `Disconnected` until the owning transport orchestrator performs one
    /// cold rebuild of the host session.
    pub(crate) async fn connect_remote_multiplexed_with_unavailable(
        config: ServerConfig,
        resources: Vec<RuntimeRemoteSessionResource>,
        unavailable_runtime_kinds: Vec<AgentRuntimeKind>,
        extras: RemoteSessionExtras,
    ) -> Result<Self, TransportError> {
        let connected_runtime_kinds = resources
            .iter()
            .map(|resource| resource.runtime_kind.clone())
            .collect::<Vec<_>>();
        let mut requested_runtime_kinds = connected_runtime_kinds.clone();
        for runtime_kind in unavailable_runtime_kinds {
            if !requested_runtime_kinds.contains(&runtime_kind) {
                requested_runtime_kinds.push(runtime_kind);
            }
        }
        let first_runtime_kind = resources
            .first()
            .map(|resource| resource.runtime_kind.clone())
            .ok_or_else(|| {
                TransportError::ConnectionFailed("no runtime streams available".to_string())
            })?;
        let (health_tx, health_rx) = watch::channel(ConnectionHealth::Connecting {
            attempt: 1,
            max_attempts: REMOTE_RECONNECT_MAX_ATTEMPTS,
        });
        let (url, args) = remote_connect_args(&config);
        let (event_tx, _) = broadcast::channel::<ServerEvent>(256);
        let connection_timeline = ConnectionTimeline::default();
        let connected_runtime_kinds = connected_runtime_kinds.into_iter().collect::<HashSet<_>>();
        let runtime_health_state = Arc::new(StdMutex::new(RuntimeHealthState {
            by_runtime: initial_runtime_health(&requested_runtime_kinds, &connected_runtime_kinds),
            generation: 0,
            cold_repair_claimed: false,
        }));
        let mut runtime_command_txs = std::collections::HashMap::new();
        let mut runtime_transports: Vec<Arc<dyn RemoteTransport>> = Vec::new();
        let mut worker_handles = Vec::new();
        let mut primary_tx = None;

        for resource in resources {
            info!(
                "multiplexed remote runtime worker start server_id={} runtime={:?}",
                config.server_id, resource.runtime_kind
            );
            let (command_tx, command_rx) = mpsc::channel::<SessionCommand>(256);
            if primary_tx.is_none() || resource.runtime_kind == first_runtime_kind {
                primary_tx = Some(command_tx.clone());
            }
            let runtime_kind = resource.runtime_kind.clone();
            runtime_command_txs.insert(resource.runtime_kind.clone(), command_tx);
            if let Some(transport) = resource.transport.as_ref() {
                runtime_transports.push(Arc::clone(transport));
            }
            worker_handles.push(spawn_remote_runtime_worker(
                runtime_kind,
                resource.client,
                resource.keepalive,
                command_rx,
                event_tx.clone(),
                RuntimeHealthReporter {
                    runtime_kind: resource.runtime_kind.clone(),
                    state: Arc::clone(&runtime_health_state),
                    session_health_tx: health_tx.clone(),
                },
                connection_timeline.clone(),
                args.clone(),
                url.clone(),
                resource.transport,
            ));
        }

        let command_tx = primary_tx.ok_or_else(|| {
            TransportError::ConnectionFailed("no runtime command channel available".to_string())
        })?;
        let worker_handle = tokio::spawn(async move {
            for handle in worker_handles {
                let _ = handle.await;
            }
        });

        let _ = health_tx.send(ConnectionHealth::Connected);
        info!(
            "multiplexed remote server session connected: {} ({}) runtimes={:?}",
            config.display_name, url, requested_runtime_kinds
        );

        Ok(Self {
            config,
            health_tx,
            health_rx,
            command_tx,
            runtime_command_txs,
            runtime_transports,
            event_tx,
            ssh_client: extras.ssh_client,
            ssh_pid: extras.ssh_pid,
            runtime_health_state: Some(runtime_health_state),
            worker_handle,
        })
    }

    /// Hint each remote-runtime transport that the host network may have
    /// changed. Iroh-backed transports use this to call
    /// `Endpoint::network_change()` so QUIC re-evaluates paths instead of
    /// waiting for the idle timeout. TCP-based transports default to a
    /// no-op since the OS already surfaces those changes.
    pub async fn notify_network_change(&self) {
        for transport in &self.runtime_transports {
            transport.notify_network_change().await;
        }
    }

    /// Get the server configuration.
    pub fn config(&self) -> &ServerConfig {
        &self.config
    }

    pub fn ssh_client(&self) -> Option<Arc<SshClient>> {
        self.ssh_client.clone()
    }

    /// Get a watch receiver for health state changes.
    pub fn health(&self) -> watch::Receiver<ConnectionHealth> {
        self.health_rx.clone()
    }

    pub(crate) fn reconnect_health_state(&self) -> ReconnectHealthState {
        let Some(runtime_health_state) = self.runtime_health_state.as_ref() else {
            let health = self.health_rx.borrow().clone();
            let is_connecting = matches!(health, ConnectionHealth::Connecting { .. });
            return ReconnectHealthState {
                aggregate: health,
                has_degraded_runtime: false,
                has_connecting_runtime: is_connecting,
                generation: 0,
            };
        };
        let state = match runtime_health_state.lock() {
            Ok(state) => state,
            Err(error) => {
                warn!("runtime health: recovering poisoned lock");
                error.into_inner()
            }
        };
        ReconnectHealthState {
            aggregate: aggregate_runtime_health(state.by_runtime.values()),
            has_degraded_runtime: runtime_health_is_degraded(&state.by_runtime),
            has_connecting_runtime: runtime_health_has_connecting(&state.by_runtime),
            generation: state.generation,
        }
    }

    pub(crate) fn try_claim_cold_repair(&self, expected_generation: u64) -> bool {
        let Some(runtime_health_state) = self.runtime_health_state.as_ref() else {
            return false;
        };
        let mut state = match runtime_health_state.lock() {
            Ok(state) => state,
            Err(error) => {
                warn!("runtime health: recovering poisoned lock");
                error.into_inner()
            }
        };
        if state.cold_repair_claimed
            || state.generation != expected_generation
            || runtime_health_has_connecting(&state.by_runtime)
        {
            return false;
        }
        state.cold_repair_claimed = true;
        state.generation = state.generation.saturating_add(1);
        true
    }

    pub(crate) async fn wait_for_runtime_reconnects_to_settle(&self, deadline: Duration) -> bool {
        let mut health_rx = self.health();
        let started = Instant::now();
        loop {
            if !self.reconnect_health_state().has_connecting_runtime {
                return true;
            }
            let remaining = deadline.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return !self.reconnect_health_state().has_connecting_runtime;
            }
            match tokio::time::timeout(remaining, health_rx.changed()).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) | Err(_) => {
                    return !self.reconnect_health_state().has_connecting_runtime;
                }
            }
        }
    }

    pub fn runtime_kinds(&self) -> Vec<AgentRuntimeKind> {
        if self.runtime_command_txs.is_empty() {
            return vec!["codex".to_string()];
        }
        let mut kinds = self.runtime_command_txs.keys().cloned().collect::<Vec<_>>();
        kinds.sort();
        kinds
    }

    pub(crate) fn available_runtime_kinds(&self) -> Vec<AgentRuntimeKind> {
        let Some(runtime_health_state) = self.runtime_health_state.as_ref() else {
            return self.runtime_kinds();
        };
        let health = match runtime_health_state.lock() {
            Ok(health) => health,
            Err(error) => {
                warn!("runtime health: recovering poisoned lock");
                error.into_inner()
            }
        };
        available_runtime_kinds_from_health(&health.by_runtime)
    }

    /// Send a typed `ClientRequest` and await the raw JSON response.
    pub async fn request_client(&self, request: ClientRequest) -> Result<JsonValue, RpcError> {
        self.request_client_for_runtime("codex".to_string(), request)
            .await
    }

    pub async fn request_client_for_runtime(
        &self,
        runtime_kind: AgentRuntimeKind,
        request: ClientRequest,
    ) -> Result<JsonValue, RpcError> {
        let wire_method = serde_json::to_value(&request)
            .ok()
            .and_then(|value| {
                value
                    .get("method")
                    .and_then(|method| method.as_str().map(str::to_string))
            })
            .unwrap_or_else(|| "<unknown>".to_string());
        let (response_tx, response_rx) = oneshot::channel();
        let command_tx = self
            .runtime_command_txs
            .get(&runtime_kind)
            .unwrap_or(&self.command_tx);
        debug!(
            "session request route server_id={} runtime={:?} method={}",
            self.config.server_id, runtime_kind, wire_method
        );
        command_tx
            .send(SessionCommand::Request {
                request,
                response_tx,
            })
            .await
            .map_err(|_| RpcError::Transport(TransportError::Disconnected))?;

        response_rx
            .await
            .map_err(|_| RpcError::Transport(TransportError::Disconnected))?
    }

    /// Send a JSON-RPC request (constructed from method + params) and await the response.
    pub async fn request(&self, method: &str, params: JsonValue) -> Result<JsonValue, RpcError> {
        let request_id = RequestId::Integer(next_request_id());
        let request_value = serde_json::json!({
            "id": request_id,
            "method": method,
            "params": params,
        });
        let request: ClientRequest = serde_json::from_value(request_value)
            .map_err(|e| RpcError::Deserialization(format!("failed to build request: {e}")))?;
        self.request_client(request).await
    }

    /// Send a method/params request to a specific runtime. Used by callers
    /// that need to reach a non-Codex runtime (e.g. a remote Pi or
    /// Opencode tunnel). Falls back to the default channel when the
    /// `runtime_kind` is not registered for this session.
    pub async fn request_for_runtime(
        &self,
        runtime_kind: AgentRuntimeKind,
        method: &str,
        params: JsonValue,
    ) -> Result<JsonValue, RpcError> {
        let request_id = RequestId::Integer(next_request_id());
        let request_value = serde_json::json!({
            "id": request_id,
            "method": method,
            "params": params,
        });
        let request: ClientRequest = serde_json::from_value(request_value)
            .map_err(|e| RpcError::Deserialization(format!("failed to build request: {e}")))?;
        self.request_client_for_runtime(runtime_kind, request).await
    }

    /// Send a JSON-RPC notification (fire-and-forget).
    pub async fn notify(&self, method: &str, params: JsonValue) -> Result<(), RpcError> {
        let notif_value = serde_json::json!({
            "method": method,
            "params": params,
        });
        let notification: ClientNotification = serde_json::from_value(notif_value)
            .map_err(|e| RpcError::Deserialization(format!("failed to build notification: {e}")))?;

        let (response_tx, response_rx) = oneshot::channel();
        self.command_tx
            .send(SessionCommand::Notify {
                notification,
                response_tx,
            })
            .await
            .map_err(|_| RpcError::Transport(TransportError::Disconnected))?;

        response_rx
            .await
            .map_err(|_| RpcError::Transport(TransportError::Disconnected))?
    }

    /// Subscribe to typed server events (notifications, legacy notifications, requests).
    pub fn events(&self) -> broadcast::Receiver<ServerEvent> {
        self.event_tx.subscribe()
    }

    /// Respond to a server-initiated request.
    pub async fn respond(&self, id: JsonValue, result: JsonValue) -> Result<(), RpcError> {
        self.respond_for_runtime("codex".to_string(), id, result)
            .await
    }

    pub async fn respond_for_runtime(
        &self,
        runtime_kind: AgentRuntimeKind,
        id: JsonValue,
        result: JsonValue,
    ) -> Result<(), RpcError> {
        let request_id = json_value_to_request_id(&id)?;
        let (response_tx, response_rx) = oneshot::channel();
        let command_tx = self
            .runtime_command_txs
            .get(&runtime_kind)
            .unwrap_or(&self.command_tx);
        command_tx
            .send(SessionCommand::Resolve {
                request_id,
                result,
                response_tx,
            })
            .await
            .map_err(|_| RpcError::Transport(TransportError::Disconnected))?;

        response_rx
            .await
            .map_err(|_| RpcError::Transport(TransportError::Disconnected))?
    }

    /// Reject a server-initiated request with a JSON-RPC error.
    pub async fn reject(&self, id: JsonValue, error: JSONRPCErrorError) -> Result<(), RpcError> {
        let request_id = json_value_to_request_id(&id)?;
        let (response_tx, response_rx) = oneshot::channel();
        self.command_tx
            .send(SessionCommand::Reject {
                request_id,
                error,
                response_tx,
            })
            .await
            .map_err(|_| RpcError::Transport(TransportError::Disconnected))?;

        response_rx
            .await
            .map_err(|_| RpcError::Transport(TransportError::Disconnected))?
    }

    /// Disconnect from the server, shutting down all background tasks.
    pub async fn disconnect(&self) {
        self.disconnect_inner(false).await;
    }

    /// Disconnect and force-stop the remote app-server listener even when
    /// this session reused an existing process whose PID was not tracked.
    pub async fn restart_app_server_and_disconnect(&self) {
        self.disconnect_inner(true).await;
    }

    async fn disconnect_inner(&self, kill_reused_app_server: bool) {
        let _ = self.health_tx.send(ConnectionHealth::Disconnected);
        let _ = self.command_tx.send(SessionCommand::Shutdown).await;
        for tx in self.runtime_command_txs.values() {
            let _ = tx.send(SessionCommand::Shutdown).await;
        }
        if let Some(ssh_client) = self.ssh_client.as_ref() {
            if let Some(pid) = self.ssh_pid.as_ref() {
                let pid = match pid.lock() {
                    Ok(mut guard) => guard.take(),
                    Err(error) => {
                        warn!("ServerSession: recovering poisoned ssh_pid lock");
                        error.into_inner().take()
                    }
                };
                if let Some(pid) = pid {
                    let _ = ssh_client.exec(&format!("kill {pid} 2>/dev/null")).await;
                }
            }
            if kill_reused_app_server && self.config.port > 0 {
                match ssh_client.kill_listener_on_port(self.config.port).await {
                    Ok(result) => {
                        info!(
                            "restart app server stop listener port={} exit_code={} stdout={} stderr={}",
                            self.config.port,
                            result.exit_code,
                            result.stdout.trim(),
                            result.stderr.trim()
                        );
                    }
                    Err(error) => {
                        warn!(
                            "restart app server stop listener failed port={} error={}",
                            self.config.port, error
                        );
                    }
                }
            }
            ssh_client.disconnect().await;
        }
        // Give the worker a moment to shut down gracefully.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        self.worker_handle.abort();
        info!("server session disconnected: {}", self.config.display_name);
    }
}

pub(crate) fn remote_connect_args(config: &ServerConfig) -> (String, RemoteAppServerConnectArgs) {
    let url = if let Some(url) = config.websocket_url.clone() {
        url
    } else {
        let scheme = if config.tls { "wss" } else { "ws" };
        format!("{scheme}://{}:{}", config.host, config.port)
    };

    let args = RemoteAppServerConnectArgs {
        endpoint: RemoteAppServerEndpoint::WebSocket {
            websocket_url: url.clone(),
            auth_token: None,
        },
        client_name: "Remora".to_string(),
        client_version: "1.0".to_string(),
        experimental_api: true,
        opt_out_notification_methods: Vec::new(),
        channel_capacity: 256,
    };

    (url, args)
}

pub(crate) async fn connect_remote_client(
    args: &RemoteAppServerConnectArgs,
) -> Result<AppServerClient, TransportError> {
    #[cfg(all(target_os = "ios", not(target_abi = "macabi")))]
    {
        let home_dir = std::env::var_os("HOME").map(PathBuf::from);
        let codex_home = resolve_ios_codex_home(&home_dir)?;
        let _ = prepare_ios_runtime_environment(&codex_home)?;
    }

    Ok(AppServerClient::Remote(
        RemoteAppServerClient::connect(args.clone())
            .await
            .map_err(|e| TransportError::ConnectionFailed(e.to_string()))?,
    ))
}

pub(crate) async fn connect_remote_client_over_app_server_proxy(
    ssh_client: &SshClient,
    args: &RemoteAppServerConnectArgs,
    codex_path: &str,
    remote_shell: RemoteShell,
) -> Result<AppServerClient, TransportError> {
    let label = "app-server-proxy:default".to_string();
    let mut proxy_args = args.clone();
    // The SSH exec proxy supplies the connected byte stream. The WebSocket
    // client still needs a syntactically valid URI for the HTTP Upgrade
    // request, so use a synthetic loopback-ish URL and keep the real transport
    // in the label passed to `connect_websocket_stream`. Preserve any auth_token
    // that was carried in `args.endpoint` so the Bearer header is still sent.
    let preserved_auth_token = match &proxy_args.endpoint {
        RemoteAppServerEndpoint::WebSocket { auth_token, .. } => auth_token.clone(),
        RemoteAppServerEndpoint::UnixSocket { .. } => None,
    };
    proxy_args.endpoint = RemoteAppServerEndpoint::WebSocket {
        websocket_url: APP_SERVER_PROXY_WEBSOCKET_URL.to_string(),
        auth_token: preserved_auth_token,
    };
    let stream = ssh_client
        .open_app_server_proxy_stream(codex_path, remote_shell, None)
        .await
        .map_err(|e| TransportError::ConnectionFailed(e.to_string()))?;
    Ok(AppServerClient::Remote(
        RemoteAppServerClient::connect_websocket_stream(stream, proxy_args, label)
            .await
            .map_err(|e| TransportError::ConnectionFailed(e.to_string()))?,
    ))
}

pub(crate) async fn connect_remote_client_over_slingshot(
    api: codex_slingshot::SlingshotApi,
    environment_id: String,
    args: &RemoteAppServerConnectArgs,
) -> Result<AppServerClient, TransportError> {
    codex_slingshot::connect_app_server_client(api, environment_id, args.clone())
        .await
        .map_err(|e| TransportError::ConnectionFailed(e.to_string()))
}

#[allow(
    clippy::too_many_arguments,
    reason = "reconnect state is passed explicitly so transport, health, and timeline ownership remain visible at the retry boundary"
)]
async fn reconnect_remote_client(
    client: &mut AppServerClient,
    keepalive: &mut Option<Arc<dyn SessionKeepalive>>,
    args: &RemoteAppServerConnectArgs,
    websocket_url: &str,
    health: &RuntimeHealthReporter,
    timeline: &ConnectionTimeline,
    runtime_kind: &AgentRuntimeKind,
    trigger: ConnectionTrigger,
    generation: u64,
    transport: Option<&Arc<dyn RemoteTransport>>,
) -> Option<ReplayOutcome> {
    let correlation_id = timeline.next_correlation_id();
    let journey_started = Instant::now();
    let route = transport.map_or("websocket", |transport| transport.route_label());
    let mut backoff = ReconnectBackoff::production();

    for attempt in 1..=REMOTE_RECONNECT_MAX_ATTEMPTS {
        append_android_debug_log(&format!(
            "reconnect_start correlation_id={} generation={} route={} runtime={} trigger={} attempt={}/{}",
            correlation_id,
            generation,
            route,
            runtime_kind,
            trigger.as_str(),
            attempt,
            REMOTE_RECONNECT_MAX_ATTEMPTS
        ));
        timeline.record(
            correlation_id,
            generation,
            trigger,
            ConnectionStage::AttemptStarted,
            Duration::ZERO,
            journey_started.elapsed(),
            route,
            runtime_kind,
            attempt,
            ConnectionAttemptOutcome::Pending,
        );
        if !health.update(ConnectionHealth::Connecting {
            attempt,
            max_attempts: REMOTE_RECONNECT_MAX_ATTEMPTS,
        }) {
            return None;
        }

        let dial_started = Instant::now();
        let connect_result: Result<(Reconnected, ReplayOutcome), TransportError> = async {
            let next = match transport {
                Some(t) => t.reconnect(args, websocket_url).await?,
                None => Reconnected {
                    client: connect_remote_client(args).await?,
                    keepalive: None,
                },
            };
            let replay_outcome = transport
                .map(|transport| transport.take_replay_outcome())
                .unwrap_or(ReplayOutcome::Complete);
            if let Some(transport) = transport {
                transport
                    .reconcile_replay(&next.client, replay_outcome)
                    .await?;
            }
            Ok((next, replay_outcome))
        }
        .await;

        match connect_result {
            Ok((next, replay_outcome)) => {
                *client = next.client;
                if next.keepalive.is_some() {
                    *keepalive = next.keepalive;
                }
                if !health.update(ConnectionHealth::Connected) {
                    return None;
                }
                timeline.record(
                    correlation_id,
                    generation,
                    trigger,
                    ConnectionStage::DialFinished,
                    dial_started.elapsed(),
                    journey_started.elapsed(),
                    route,
                    runtime_kind,
                    attempt,
                    ConnectionAttemptOutcome::Succeeded,
                );
                timeline.record(
                    correlation_id,
                    generation,
                    trigger,
                    ConnectionStage::Ready,
                    Duration::ZERO,
                    journey_started.elapsed(),
                    route,
                    runtime_kind,
                    attempt,
                    ConnectionAttemptOutcome::Succeeded,
                );
                append_android_debug_log(&format!(
                    "reconnect_success correlation_id={} generation={} route={} runtime={} attempt={}/{}",
                    correlation_id,
                    generation,
                    route,
                    runtime_kind,
                    attempt,
                    REMOTE_RECONNECT_MAX_ATTEMPTS
                ));
                return Some(replay_outcome);
            }
            Err(error) => {
                warn!(
                    "remote server reconnect failed route={} runtime={} correlation_id={} generation={} attempt={}/{} error={}",
                    route,
                    runtime_kind,
                    correlation_id,
                    generation,
                    attempt,
                    REMOTE_RECONNECT_MAX_ATTEMPTS,
                    error
                );
                timeline.record(
                    correlation_id,
                    generation,
                    trigger,
                    ConnectionStage::DialFinished,
                    dial_started.elapsed(),
                    journey_started.elapsed(),
                    route,
                    runtime_kind,
                    attempt,
                    ConnectionAttemptOutcome::TransientFailure,
                );
                append_android_debug_log(&format!(
                    "reconnect_failed correlation_id={} generation={} route={} runtime={} attempt={}/{}",
                    correlation_id,
                    generation,
                    route,
                    runtime_kind,
                    attempt,
                    REMOTE_RECONNECT_MAX_ATTEMPTS
                ));
                if attempt < REMOTE_RECONNECT_MAX_ATTEMPTS {
                    let delay = backoff.delay_after_failure(attempt);
                    timeline.record(
                        correlation_id,
                        generation,
                        trigger,
                        ConnectionStage::BackoffScheduled,
                        delay,
                        journey_started.elapsed(),
                        route,
                        runtime_kind,
                        attempt,
                        ConnectionAttemptOutcome::TransientFailure,
                    );
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }

    let _ = health.update(ConnectionHealth::Disconnected);
    timeline.record(
        correlation_id,
        generation,
        trigger,
        ConnectionStage::Exhausted,
        Duration::ZERO,
        journey_started.elapsed(),
        route,
        runtime_kind,
        REMOTE_RECONNECT_MAX_ATTEMPTS,
        ConnectionAttemptOutcome::Exhausted,
    );
    None
}

fn ssh_reconnect_remote_host(state: &WebSocketReconnectState) -> &'static str {
    if state.prefer_ipv6 {
        "::1"
    } else {
        "127.0.0.1"
    }
}

fn ssh_reconnect_remote_port(state: &WebSocketReconnectState) -> u16 {
    match state.remote_port.lock() {
        Ok(guard) => *guard,
        Err(error) => {
            warn!("remote reconnect: recovering poisoned remote_port lock");
            *error.into_inner()
        }
    }
}

fn update_ssh_reconnect_remote_port(state: &WebSocketReconnectState, port: u16) {
    match state.remote_port.lock() {
        Ok(mut guard) => *guard = port,
        Err(error) => {
            warn!("remote reconnect: recovering poisoned remote_port lock");
            *error.into_inner() = port;
        }
    }
}

fn update_ssh_reconnect_pid(transport: &SshReconnectTransport, pid: Option<u32>) {
    let Some(pid_slot) = transport.ssh_pid.as_ref() else {
        return;
    };
    match pid_slot.lock() {
        Ok(mut guard) => *guard = pid,
        Err(error) => {
            warn!("remote reconnect: recovering poisoned ssh_pid lock");
            *error.into_inner() = pid;
        }
    }
}

async fn rebootstrap_websocket_client_over_ssh(
    transport: &SshReconnectTransport,
    state: &WebSocketReconnectState,
    websocket_url: &str,
) -> bool {
    let bootstrap = match transport
        .ssh_client
        .bootstrap_codex_websocket_server_with_binary_and_shell(
            &crate::ssh::RemoteCodexBinary::Codex(transport.codex_path.clone()),
            transport.working_dir.as_deref(),
            state.prefer_ipv6,
            transport.remote_shell,
        )
        .await
    {
        Ok(bootstrap) => bootstrap,
        Err(error) => {
            warn!(
                "remote reconnect ssh rebootstrap failed: {} error={}",
                websocket_url, error
            );
            return false;
        }
    };

    finalize_websocket_rebootstrap(transport, state, bootstrap, websocket_url).await
}

async fn finalize_websocket_rebootstrap(
    transport: &SshReconnectTransport,
    state: &WebSocketReconnectState,
    bootstrap: SshBootstrapResult,
    websocket_url: &str,
) -> bool {
    let remote_host = ssh_reconnect_remote_host(state);
    let existing_local_port = state.local_port;
    let previous_remote_port = ssh_reconnect_remote_port(state);

    if bootstrap.tunnel_local_port != existing_local_port {
        let _ = transport
            .ssh_client
            .abort_forward_port(bootstrap.tunnel_local_port)
            .await;
    }
    if bootstrap.server_port != previous_remote_port {
        let _ = transport
            .ssh_client
            .abort_forward_port(existing_local_port)
            .await;
    }

    if let Err(error) = transport
        .ssh_client
        .ensure_forward_port_to(existing_local_port, remote_host, bootstrap.server_port)
        .await
    {
        warn!(
            "remote reconnect ssh rebootstrap forward failed: {} local_port={} remote={}:{} error={}",
            websocket_url, existing_local_port, remote_host, bootstrap.server_port, error
        );
        return false;
    }

    update_ssh_reconnect_remote_port(state, bootstrap.server_port);
    update_ssh_reconnect_pid(transport, bootstrap.pid);
    true
}

async fn rebootstrap_app_server_proxy_over_ssh(
    transport: &SshReconnectTransport,
    codex_path: &str,
    remote_shell: RemoteShell,
    websocket_url: &str,
) -> bool {
    let bootstrap = match transport
        .ssh_client
        .bootstrap_codex_app_server_proxy_with_binary_and_shell(
            &crate::ssh::RemoteCodexBinary::Codex(codex_path.to_string()),
            transport.working_dir.as_deref(),
            remote_shell,
        )
        .await
    {
        Ok(bootstrap) => bootstrap,
        Err(error) => {
            warn!(
                "remote reconnect app-server proxy bootstrap failed: {} error={}",
                websocket_url, error
            );
            return false;
        }
    };

    update_ssh_reconnect_pid(transport, bootstrap.pid);
    info!(
        "remote reconnect app-server proxy bootstrap completed: {} pid={:?}",
        websocket_url, bootstrap.pid
    );
    true
}

#[async_trait::async_trait]
impl RemoteTransport for SshReconnectTransport {
    async fn reconnect(
        &self,
        args: &RemoteAppServerConnectArgs,
        websocket_url: &str,
    ) -> Result<Reconnected, TransportError> {
        match &self.mode {
            SshReconnectMode::AppServerProxy {
                codex_path,
                remote_shell,
            } => match connect_remote_client_over_app_server_proxy(
                &self.ssh_client,
                args,
                codex_path,
                *remote_shell,
            )
            .await
            {
                Ok(client) => Ok(Reconnected {
                    client,
                    keepalive: None,
                }),
                Err(error) => {
                    if rebootstrap_app_server_proxy_over_ssh(
                        self,
                        codex_path,
                        *remote_shell,
                        websocket_url,
                    )
                    .await
                    {
                        match connect_remote_client_over_app_server_proxy(
                            &self.ssh_client,
                            args,
                            codex_path,
                            *remote_shell,
                        )
                        .await
                        {
                            Ok(client) => {
                                info!(
                                    "remote reconnect succeeded after app-server proxy bootstrap: {}",
                                    websocket_url
                                );
                                Ok(Reconnected {
                                    client,
                                    keepalive: None,
                                })
                            }
                            Err(retry_error) => {
                                warn!(
                                    "remote reconnect after app-server proxy bootstrap still failed: {} - {}",
                                    websocket_url, retry_error
                                );
                                Err(retry_error)
                            }
                        }
                    } else {
                        Err(error)
                    }
                }
            },
            SshReconnectMode::WebSocketTunnel { .. } => {
                let state = match &self.mode {
                    SshReconnectMode::WebSocketTunnel {
                        local_port,
                        remote_port,
                        prefer_ipv6,
                    } => WebSocketReconnectState {
                        local_port: *local_port,
                        remote_port: Arc::clone(remote_port),
                        prefer_ipv6: *prefer_ipv6,
                    },
                    SshReconnectMode::AppServerProxy { .. } => unreachable!(),
                };
                let remote_host = ssh_reconnect_remote_host(&state);
                let remote_port = ssh_reconnect_remote_port(&state);
                if let Err(error) = self
                    .ssh_client
                    .ensure_forward_port_to(state.local_port, remote_host, remote_port)
                    .await
                {
                    warn!(
                        "remote reconnect forward restore failed: {} local_port={} remote={}:{} error={}",
                        websocket_url, state.local_port, remote_host, remote_port, error
                    );
                }

                match connect_remote_client(args).await {
                    Ok(client) => Ok(Reconnected {
                        client,
                        keepalive: None,
                    }),
                    Err(error) => {
                        if rebootstrap_websocket_client_over_ssh(self, &state, websocket_url).await
                        {
                            match connect_remote_client(args).await {
                                Ok(client) => {
                                    info!(
                                        "remote reconnect succeeded after ssh rebootstrap: {}",
                                        websocket_url
                                    );
                                    Ok(Reconnected {
                                        client,
                                        keepalive: None,
                                    })
                                }
                                Err(retry_error) => {
                                    warn!(
                                        "remote reconnect after ssh rebootstrap still failed: {} - {}",
                                        websocket_url, retry_error
                                    );
                                    Err(retry_error)
                                }
                            }
                        } else {
                            Err(error)
                        }
                    }
                }
            }
        }
    }

    fn route_label(&self) -> &'static str {
        "ssh"
    }
}

#[async_trait::async_trait]
impl RemoteTransport for SlingshotReconnectTransport {
    async fn reconnect(
        &self,
        args: &RemoteAppServerConnectArgs,
        _websocket_url: &str,
    ) -> Result<Reconnected, TransportError> {
        connect_remote_client_over_slingshot(self.api.clone(), self.environment_id.clone(), args)
            .await
            .map(|client| Reconnected {
                client,
                keepalive: None,
            })
    }

    fn route_label(&self) -> &'static str {
        "slingshot"
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the worker owns independent runtime channels and reconnect resources that must move into one spawned task"
)]
fn spawn_remote_runtime_worker(
    runtime_kind: AgentRuntimeKind,
    mut client: AppServerClient,
    initial_keepalive: Option<Arc<dyn SessionKeepalive>>,
    mut command_rx: mpsc::Receiver<SessionCommand>,
    event_tx: broadcast::Sender<ServerEvent>,
    health: RuntimeHealthReporter,
    timeline: ConnectionTimeline,
    reconnect_args: RemoteAppServerConnectArgs,
    reconnect_url: String,
    reconnect_transport: Option<Arc<dyn RemoteTransport>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut keepalive: Option<Arc<dyn SessionKeepalive>> = initial_keepalive;
        let mut reconnect_generation = 0_u64;
        loop {
            tokio::select! {
                command = command_rx.recv() => {
                    let Some(command) = command else { break; };
                    match command {
                        SessionCommand::Request { request, response_tx } => {
                            let request_retry = request.clone();
                            let mut result = match client.request(request).await {
                                Ok(Ok(value)) => Ok(value),
                                Ok(Err(error)) => Err(RpcError::Server {
                                    code: error.code,
                                    message: error.message,
                                }),
                                Err(error) => Err(RpcError::Transport(
                                    TransportError::SendFailed(error.to_string()),
                                )),
                            };
                            if matches!(result, Err(RpcError::Transport(_))) {
                                reconnect_generation = reconnect_generation.saturating_add(1);
                                if let Some(replay_outcome) = reconnect_remote_client(
                                    &mut client,
                                    &mut keepalive,
                                    &reconnect_args,
                                    &reconnect_url,
                                    &health,
                                    &timeline,
                                    &runtime_kind,
                                    ConnectionTrigger::RequestTransportError,
                                    reconnect_generation,
                                    reconnect_transport.as_ref(),
                                )
                                .await
                                {
                                    emit_transport_reconnected(
                                        &event_tx,
                                        &runtime_kind,
                                        reconnect_generation,
                                        replay_outcome,
                                    );
                                    result = match client.request(request_retry).await {
                                        Ok(Ok(value)) => Ok(value),
                                        Ok(Err(error)) => Err(RpcError::Server {
                                            code: error.code,
                                            message: error.message,
                                        }),
                                        Err(error) => Err(RpcError::Transport(
                                            TransportError::SendFailed(error.to_string()),
                                        )),
                                    };
                                }
                            }
                            let _ = response_tx.send(result);
                        }
                        SessionCommand::Notify { notification, response_tx } => {
                            let result = client.notify(notification).await.map_err(|error| {
                                RpcError::Transport(TransportError::SendFailed(error.to_string()))
                            });
                            let _ = response_tx.send(result);
                        }
                        SessionCommand::Resolve { request_id, result, response_tx } => {
                            let result = client
                                .resolve_server_request(request_id, result)
                                .await
                                .map_err(|error| {
                                    RpcError::Transport(TransportError::SendFailed(
                                        error.to_string(),
                                    ))
                                });
                            let _ = response_tx.send(result);
                        }
                        SessionCommand::Reject { request_id, error, response_tx } => {
                            let result = client
                                .reject_server_request(request_id, error)
                                .await
                                .map_err(|error| {
                                    RpcError::Transport(TransportError::SendFailed(
                                        error.to_string(),
                                    ))
                                });
                            let _ = response_tx.send(result);
                        }
                        SessionCommand::Shutdown => {
                            let _ = client.shutdown().await;
                            break;
                        }
                    }
                }
                event = client.next_event() => {
                    let Some(event) = event else {
                        reconnect_generation = reconnect_generation.saturating_add(1);
                        if let Some(replay_outcome) = reconnect_remote_client(
                            &mut client,
                            &mut keepalive,
                            &reconnect_args,
                            &reconnect_url,
                            &health,
                            &timeline,
                            &runtime_kind,
                            ConnectionTrigger::EventStreamEnded,
                            reconnect_generation,
                            reconnect_transport.as_ref(),
                        )
                        .await {
                            emit_transport_reconnected(
                                &event_tx,
                                &runtime_kind,
                                reconnect_generation,
                                replay_outcome,
                            );
                            continue;
                        }
                        break;
                    };
                    if let AppServerEvent::Disconnected { .. } = &event {
                        reconnect_generation = reconnect_generation.saturating_add(1);
                        if let Some(replay_outcome) = reconnect_remote_client(
                            &mut client,
                            &mut keepalive,
                            &reconnect_args,
                            &reconnect_url,
                            &health,
                            &timeline,
                            &runtime_kind,
                            ConnectionTrigger::TransportDisconnected,
                            reconnect_generation,
                            reconnect_transport.as_ref(),
                        )
                        .await {
                            emit_transport_reconnected(
                                &event_tx,
                                &runtime_kind,
                                reconnect_generation,
                                replay_outcome,
                            );
                            continue;
                        }
                    }
                    route_app_server_event(&event_tx, &health, runtime_kind.clone(), &event);
                }
            }
        }
        // Send a graceful close to the peer (e.g. iroh `Connection::close`)
        // before dropping the keepalive Arc. Idempotent on already-errored
        // connections, and avoids "Aborting ungracefully" log spam from
        // iroh when the worker exits via `SessionCommand::Shutdown`.
        if let Some(keepalive) = keepalive.as_ref() {
            keepalive.close();
        }
        // Hold the keepalive Arc for the entire worker lifetime so transport-scoped
        // resources (e.g. an iroh Connection) are dropped only after the worker exits.
        drop(keepalive);
    })
}

fn emit_transport_reconnected(
    event_tx: &broadcast::Sender<ServerEvent>,
    runtime_kind: &AgentRuntimeKind,
    generation: u64,
    replay_outcome: ReplayOutcome,
) {
    let _ = event_tx.send(ServerEvent::TransportReconnected {
        runtime_kind: runtime_kind.clone(),
        generation,
        authoritative_refresh_required: replay_outcome
            == ReplayOutcome::AuthoritativeRefreshRequired,
    });
}

// ---------------------------------------------------------------------------
// Event routing helpers
// ---------------------------------------------------------------------------

fn route_app_server_event(
    event_tx: &broadcast::Sender<ServerEvent>,
    health: &RuntimeHealthReporter,
    runtime_kind: AgentRuntimeKind,
    event: &AppServerEvent,
) {
    match event {
        AppServerEvent::ServerNotification(notification) => {
            // Log only the variant kind (via strum Display) — formatting the
            // full `{:?}` body is a per-event allocation in the hundreds of KB
            // for hot variants like `TurnDiffUpdated` and was contributing to
            // memory pressure during streaming.
            info!("remote event notification {}", notification);
            let _ = event_tx.send(ServerEvent::Notification {
                runtime_kind,
                notification: notification.clone(),
            });
        }
        AppServerEvent::ServerRequest(request) => {
            info!("remote event server request {:?}", request);
            append_android_debug_log(&format!("server_request={request:?}"));
            let _ = event_tx.send(ServerEvent::Request {
                runtime_kind,
                request: request.clone(),
            });
        }
        AppServerEvent::Lagged { skipped } => {
            warn!("event: lagged, skipped {skipped} events");
        }
        AppServerEvent::Disconnected { message } => {
            warn!("event: disconnected: {message}");
            append_android_debug_log(&format!("disconnected={message}"));
            let _ = health.update(ConnectionHealth::Disconnected);
        }
    }
}

fn route_in_process_event(
    event_tx: &broadcast::Sender<ServerEvent>,
    event: codex_app_server::in_process::InProcessServerEvent,
) {
    use codex_app_server::in_process::InProcessServerEvent;

    match event {
        InProcessServerEvent::ServerNotification(notification) => {
            let _ = event_tx.send(ServerEvent::Notification {
                runtime_kind: "codex".to_string(),
                notification,
            });
        }
        InProcessServerEvent::ServerRequest(request) => {
            let _ = event_tx.send(ServerEvent::Request {
                runtime_kind: "codex".to_string(),
                request,
            });
        }
        InProcessServerEvent::Lagged { skipped } => {
            warn!("in-process event: lagged, skipped {skipped} events");
        }
    }
}

#[cfg(test)]
impl ServerSession {
    pub(crate) fn test_stub(config: ServerConfig) -> Self {
        Self::test_stub_with_handlers(config, None, None, None)
    }

    pub(crate) fn test_stub_with_handlers(
        config: ServerConfig,
        request_handler: Option<TestRequestHandler>,
        resolve_handler: Option<TestResolveHandler>,
        reject_handler: Option<TestRejectHandler>,
    ) -> Self {
        let (health_tx, health_rx) = watch::channel(ConnectionHealth::Connected);
        let (command_tx, mut command_rx) = mpsc::channel(16);
        let (event_tx, _) = broadcast::channel(16);
        let worker_handle = tokio::spawn(async move {
            while let Some(command) = command_rx.recv().await {
                match command {
                    SessionCommand::Request {
                        request,
                        response_tx,
                    } => {
                        let result = request_handler
                            .as_ref()
                            .map(|handler| handler(request))
                            .unwrap_or_else(|| {
                                Err(RpcError::Transport(TransportError::Disconnected))
                            });
                        let _ = response_tx.send(result);
                    }
                    SessionCommand::Notify { response_tx, .. } => {
                        let _ = response_tx.send(Ok(()));
                    }
                    SessionCommand::Resolve {
                        request_id,
                        result,
                        response_tx,
                    } => {
                        let outcome = resolve_handler
                            .as_ref()
                            .map(|handler| handler(request_id, result))
                            .unwrap_or(Ok(()));
                        let _ = response_tx.send(outcome);
                    }
                    SessionCommand::Reject {
                        request_id,
                        error,
                        response_tx,
                    } => {
                        let outcome = reject_handler
                            .as_ref()
                            .map(|handler| handler(request_id, error))
                            .unwrap_or(Ok(()));
                        let _ = response_tx.send(outcome);
                    }
                    SessionCommand::Shutdown => break,
                }
            }
        });
        Self {
            config,
            health_tx,
            health_rx,
            command_tx,
            runtime_command_txs: std::collections::HashMap::new(),
            runtime_transports: Vec::new(),
            event_tx,
            ssh_client: None,
            ssh_pid: None,
            runtime_health_state: None,
            worker_handle,
        }
    }

    #[cfg(test)]
    pub(crate) fn test_stub_with_runtime_handlers(
        config: ServerConfig,
        runtime_handlers: Vec<(AgentRuntimeKind, TestRequestHandler)>,
    ) -> Self {
        let (health_tx, health_rx) = watch::channel(ConnectionHealth::Connected);
        let (command_tx, default_worker_handle) = spawn_test_command_worker(None, None, None);
        let (event_tx, _) = broadcast::channel(16);
        let mut runtime_command_txs = std::collections::HashMap::new();
        let mut worker_handles = vec![default_worker_handle];
        for (runtime_kind, handler) in runtime_handlers {
            let (runtime_tx, runtime_worker_handle) =
                spawn_test_command_worker(Some(handler), None, None);
            runtime_command_txs.insert(runtime_kind, runtime_tx);
            worker_handles.push(runtime_worker_handle);
        }
        let worker_handle = tokio::spawn(async move {
            for handle in worker_handles {
                let _ = handle.await;
            }
        });

        Self {
            config,
            health_tx,
            health_rx,
            command_tx,
            runtime_command_txs,
            runtime_transports: Vec::new(),
            event_tx,
            ssh_client: None,
            ssh_pid: None,
            runtime_health_state: None,
            worker_handle,
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn json_value_to_request_id(value: &JsonValue) -> Result<RequestId, RpcError> {
    match value {
        JsonValue::Number(n) => Ok(RequestId::Integer(n.as_i64().unwrap_or(0))),
        JsonValue::String(s) => Ok(RequestId::String(s.clone())),
        _ => Err(RpcError::Deserialization(
            "invalid request id type".to_string(),
        )),
    }
}

fn next_request_id() -> i64 {
    use std::sync::atomic::{AtomicI64, Ordering};
    static COUNTER: AtomicI64 = AtomicI64::new(1);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Mutex, OnceLock};
    use std::time::{SystemTime, UNIX_EPOCH};

    use async_trait::async_trait;
    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, duplex};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn unique_temp_path(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("remora-{label}-{nanos}"))
    }

    fn test_remote_args(label: &str) -> RemoteAppServerConnectArgs {
        RemoteAppServerConnectArgs {
            endpoint: RemoteAppServerEndpoint::WebSocket {
                websocket_url: format!("test://{label}"),
                auth_token: None,
            },
            client_name: "RemoraTest".to_string(),
            client_version: "0".to_string(),
            experimental_api: true,
            opt_out_notification_methods: Vec::new(),
            channel_capacity: 16,
        }
    }

    enum TestJsonLineServer {
        DropOnFirstRequest,
        Respond(JsonValue),
    }

    async fn app_server_client_for_json_line_server(
        behavior: TestJsonLineServer,
        label: &str,
    ) -> AppServerClient {
        let (client_io, server_io) = duplex(64 * 1024);
        tokio::spawn(async move {
            let (reader, mut writer) = tokio::io::split(server_io);
            let mut lines = BufReader::new(reader).lines();
            let Some(Ok(initialize_line)) = lines.next_line().await.transpose() else {
                return;
            };
            let initialize: JsonValue = serde_json::from_str(&initialize_line)
                .expect("client should send JSON-RPC initialize");
            let initialize_id = initialize.get("id").cloned().unwrap_or(JsonValue::Null);
            let initialize_response = json!({
                "jsonrpc": "2.0",
                "id": initialize_id,
                "result": {}
            });
            let _ = writer
                .write_all(format!("{initialize_response}\n").as_bytes())
                .await;
            let _ = writer.flush().await;

            // The client sends an `initialized` notification after the
            // initialize response. It does not require a response.
            let _ = lines.next_line().await;

            match behavior {
                TestJsonLineServer::DropOnFirstRequest => {
                    // Consume one request and then close the stream without a
                    // response, simulating the app-server process/bridge dying
                    // after the mobile client has already enqueued an RPC.
                    let _ = lines.next_line().await;
                }
                TestJsonLineServer::Respond(response) => {
                    while let Ok(Some(line)) = lines.next_line().await {
                        let request: JsonValue = serde_json::from_str(&line)
                            .expect("client should send JSON-RPC request");
                        let Some(id) = request.get("id").cloned() else {
                            continue;
                        };
                        let response = json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": response
                        });
                        if writer
                            .write_all(format!("{response}\n").as_bytes())
                            .await
                            .is_err()
                        {
                            break;
                        }
                        let _ = writer.flush().await;
                    }
                }
            }
        });
        AppServerClient::Remote(
            codex_slingshot::json_line_wire::connect_json_line_stream(
                client_io,
                test_remote_args(label),
                label.to_string(),
            )
            .await
            .expect("test JSON-line server should initialize"),
        )
    }

    struct TestReconnectTransport {
        reconnects: Arc<AtomicUsize>,
    }

    struct BlockingReconcileTransport {
        started: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }

    #[async_trait]
    impl RemoteTransport for TestReconnectTransport {
        async fn reconnect(
            &self,
            _args: &RemoteAppServerConnectArgs,
            _websocket_url: &str,
        ) -> Result<Reconnected, TransportError> {
            self.reconnects.fetch_add(1, Ordering::SeqCst);
            let client = app_server_client_for_json_line_server(
                TestJsonLineServer::Respond(json!({"source": "reconnected"})),
                "reconnected-test-bridge",
            )
            .await;
            Ok(Reconnected {
                client,
                keepalive: None,
            })
        }
    }

    #[async_trait]
    impl RemoteTransport for BlockingReconcileTransport {
        async fn reconnect(
            &self,
            _args: &RemoteAppServerConnectArgs,
            _websocket_url: &str,
        ) -> Result<Reconnected, TransportError> {
            Ok(Reconnected {
                client: app_server_client_for_json_line_server(
                    TestJsonLineServer::Respond(json!({})),
                    "reconcile-test-bridge",
                )
                .await,
                keepalive: None,
            })
        }

        fn take_replay_outcome(&self) -> ReplayOutcome {
            ReplayOutcome::AuthoritativeRefreshRequired
        }

        async fn reconcile_replay(
            &self,
            _client: &AppServerClient,
            outcome: ReplayOutcome,
        ) -> Result<(), TransportError> {
            assert_eq!(outcome, ReplayOutcome::AuthoritativeRefreshRequired);
            self.started.notify_one();
            self.release.notified().await;
            Ok(())
        }
    }

    #[tokio::test]
    async fn replay_drift_reconciliation_finishes_before_connected_is_published() {
        let mut client = app_server_client_for_json_line_server(
            TestJsonLineServer::Respond(json!({})),
            "initial-reconcile-test-bridge",
        )
        .await;
        let mut keepalive = None;
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let transport: Arc<dyn RemoteTransport> = Arc::new(BlockingReconcileTransport {
            started: Arc::clone(&started),
            release: Arc::clone(&release),
        });
        let (health_tx, health_rx) = watch::channel(ConnectionHealth::Disconnected);
        let health_state = Arc::new(StdMutex::new(RuntimeHealthState {
            by_runtime: HashMap::from([("codex".to_string(), ConnectionHealth::Disconnected)]),
            generation: 0,
            cold_repair_claimed: false,
        }));
        let health = RuntimeHealthReporter {
            runtime_kind: "codex".to_string(),
            state: health_state,
            session_health_tx: health_tx,
        };
        let args = test_remote_args("reconcile-test-bridge");
        let timeline = ConnectionTimeline::default();
        let runtime_kind = "codex".to_string();
        let reconnect = reconnect_remote_client(
            &mut client,
            &mut keepalive,
            &args,
            "reconcile-test-bridge",
            &health,
            &timeline,
            &runtime_kind,
            ConnectionTrigger::TransportDisconnected,
            1,
            Some(&transport),
        );
        tokio::pin!(reconnect);

        tokio::select! {
            _ = started.notified() => {}
            outcome = &mut reconnect => panic!("reconnect published early: {outcome:?}"),
        }
        assert!(matches!(
            *health_rx.borrow(),
            ConnectionHealth::Connecting { .. }
        ));

        release.notify_one();
        assert_eq!(
            reconnect.await,
            Some(ReplayOutcome::AuthoritativeRefreshRequired)
        );
        assert_eq!(*health_rx.borrow(), ConnectionHealth::Connected);
    }

    #[tokio::test]
    async fn remote_runtime_worker_reconnects_and_retries_request_after_stream_drop() {
        let initial_client = app_server_client_for_json_line_server(
            TestJsonLineServer::DropOnFirstRequest,
            "drop-test-bridge",
        )
        .await;
        let reconnects = Arc::new(AtomicUsize::new(0));
        let reconnect_transport: Arc<dyn RemoteTransport> = Arc::new(TestReconnectTransport {
            reconnects: Arc::clone(&reconnects),
        });
        let (command_tx, command_rx) = mpsc::channel(4);
        let (event_tx, mut event_rx) = broadcast::channel(4);
        let (health_tx, mut health_rx) = watch::channel(ConnectionHealth::Connected);
        let timeline = ConnectionTimeline::default();
        let health_state = Arc::new(StdMutex::new(RuntimeHealthState {
            by_runtime: HashMap::from([("pi".to_string(), ConnectionHealth::Connected)]),
            generation: 0,
            cold_repair_claimed: false,
        }));
        let worker = spawn_remote_runtime_worker(
            "pi".to_string(),
            initial_client,
            None,
            command_rx,
            event_tx,
            RuntimeHealthReporter {
                runtime_kind: "pi".to_string(),
                state: health_state,
                session_health_tx: health_tx,
            },
            timeline.clone(),
            test_remote_args("drop-test-bridge"),
            "drop-test-bridge".to_string(),
            Some(reconnect_transport),
        );

        let request: ClientRequest = serde_json::from_value(json!({
            "id": 1,
            "method": "model/list",
            "params": {"limit": 5}
        }))
        .expect("valid app-server request");
        let (response_tx, response_rx) = oneshot::channel();
        command_tx
            .send(SessionCommand::Request {
                request,
                response_tx,
            })
            .await
            .expect("worker should accept request");

        let response = tokio::time::timeout(std::time::Duration::from_secs(2), response_rx)
            .await
            .expect("dropped stream request should reconnect instead of hanging")
            .expect("worker should return a response")
            .expect("request should succeed after reconnect");
        assert_eq!(response, json!({"source": "reconnected"}));
        assert_eq!(reconnects.load(Ordering::SeqCst), 1);
        assert_eq!(*health_rx.borrow_and_update(), ConnectionHealth::Connected);
        let event = tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
            .await
            .expect("reconnect event should be emitted")
            .expect("reconnect event channel should remain open");
        assert!(matches!(
            event,
            ServerEvent::TransportReconnected {
                runtime_kind,
                generation: 1,
                authoritative_refresh_required: false,
            } if runtime_kind == "pi"
        ));
        let entries = timeline.snapshot();
        assert!(
            entries
                .windows(2)
                .all(|pair| pair[0].sequence < pair[1].sequence)
        );
        assert_eq!(entries.first().map(|entry| entry.generation), Some(1));
        assert_eq!(
            entries.last().map(|entry| entry.outcome),
            Some(ConnectionAttemptOutcome::Succeeded)
        );
        assert!(entries.iter().all(|entry| entry.route == "managed"));

        command_tx
            .send(SessionCommand::Shutdown)
            .await
            .expect("worker should accept shutdown");
        worker.await.expect("worker should shut down cleanly");
    }

    #[test]
    fn replay_drift_is_forwarded_as_authoritative_refresh_event() {
        let (event_tx, mut event_rx) = broadcast::channel(1);
        emit_transport_reconnected(
            &event_tx,
            &"codex".to_string(),
            7,
            ReplayOutcome::AuthoritativeRefreshRequired,
        );
        assert!(matches!(
            event_rx.try_recv().expect("reconnect event"),
            ServerEvent::TransportReconnected {
                runtime_kind,
                generation: 7,
                authoritative_refresh_required: true,
            } if runtime_kind == "codex"
        ));
    }

    #[test]
    fn multiplexed_health_stays_connected_while_one_runtime_recovers() {
        let reconnecting = ConnectionHealth::Connecting {
            attempt: 2,
            max_attempts: 5,
        };
        assert_eq!(
            aggregate_runtime_health([&ConnectionHealth::Connected, &reconnecting]),
            ConnectionHealth::Connected
        );
        assert_eq!(
            aggregate_runtime_health([&ConnectionHealth::Disconnected, &reconnecting]),
            reconnecting
        );
        assert_eq!(
            aggregate_runtime_health([
                &ConnectionHealth::Disconnected,
                &ConnectionHealth::Disconnected,
            ]),
            ConnectionHealth::Disconnected
        );

        let selected_runtime_health = HashMap::from([
            ("codex".to_string(), ConnectionHealth::Connected),
            ("pi".to_string(), ConnectionHealth::Disconnected),
            ("opencode".to_string(), reconnecting),
        ]);
        assert_eq!(
            aggregate_runtime_health(selected_runtime_health.values()),
            ConnectionHealth::Connected,
            "a healthy sibling keeps the aggregate session usable"
        );
        assert!(
            runtime_health_is_degraded(&selected_runtime_health),
            "an exhausted selected runtime must remain visible to cold-repair orchestration"
        );
        assert!(
            runtime_health_has_connecting(&selected_runtime_health),
            "cold repair must not replace a session while a sibling owns hot recovery"
        );
        assert_eq!(
            available_runtime_kinds_from_health(&selected_runtime_health),
            vec!["codex".to_string(), "opencode".to_string()],
            "an exhausted runtime must not satisfy selected-runtime availability checks"
        );
    }

    #[test]
    fn initially_unavailable_selected_runtime_is_visible_but_isolated() {
        let requested = vec!["codex".to_string(), "pi".to_string()];
        let connected = HashSet::from(["codex".to_string()]);
        let health = initial_runtime_health(&requested, &connected);

        assert_eq!(health["codex"], ConnectionHealth::Connected);
        assert_eq!(health["pi"], ConnectionHealth::Disconnected);
        assert_eq!(
            aggregate_runtime_health(health.values()),
            ConnectionHealth::Connected,
            "a failed selected sibling must not take down the usable runtime"
        );
        assert!(runtime_health_is_degraded(&health));
        assert_eq!(
            available_runtime_kinds_from_health(&health),
            vec!["codex".to_string()]
        );
    }

    #[tokio::test]
    async fn cold_repair_waits_for_a_hot_sibling_to_settle() {
        let config = ServerConfig {
            server_id: "srv".to_string(),
            display_name: "Server".to_string(),
            host: "example.local".to_string(),
            port: 0,
            websocket_url: None,
            is_local: false,
            tls: false,
        };
        let mut session = ServerSession::test_stub(config);
        let runtime_health_state = Arc::new(StdMutex::new(RuntimeHealthState {
            by_runtime: HashMap::from([
                ("codex".to_string(), ConnectionHealth::Connected),
                ("pi".to_string(), ConnectionHealth::Disconnected),
                (
                    "opencode".to_string(),
                    ConnectionHealth::Connecting {
                        attempt: 2,
                        max_attempts: 5,
                    },
                ),
            ]),
            generation: 0,
            cold_repair_claimed: false,
        }));
        session.runtime_health_state = Some(Arc::clone(&runtime_health_state));
        let reporter = RuntimeHealthReporter {
            runtime_kind: "opencode".to_string(),
            state: runtime_health_state,
            session_health_tx: session.health_tx.clone(),
        };
        let session = Arc::new(session);

        assert!(
            !session
                .wait_for_runtime_reconnects_to_settle(Duration::ZERO)
                .await
        );
        let waiting_session = Arc::clone(&session);
        let waiter = tokio::spawn(async move {
            waiting_session
                .wait_for_runtime_reconnects_to_settle(Duration::from_secs(1))
                .await
        });
        tokio::task::yield_now().await;
        assert!(reporter.update(ConnectionHealth::Connected));

        assert!(waiter.await.expect("settle waiter"));
        let settled = session.reconnect_health_state();
        assert_eq!(settled.aggregate, ConnectionHealth::Connected);
        assert!(settled.has_degraded_runtime);
        assert!(!settled.has_connecting_runtime);
        assert!(session.try_claim_cold_repair(settled.generation));
        assert!(
            !reporter.update(ConnectionHealth::Connecting {
                attempt: 1,
                max_attempts: 5,
            }),
            "a hot reconnect starting after the final snapshot must lose to the atomic cold claim"
        );
        session.disconnect().await;
    }

    #[test]
    fn reconnect_backoff_is_seeded_bounded_full_jitter() {
        let mut first = ReconnectBackoff::with_seed(0xC0DE);
        let mut second = ReconnectBackoff::with_seed(0xC0DE);
        let first_schedule = (1..=12)
            .map(|attempt| first.delay_after_failure(attempt))
            .collect::<Vec<_>>();
        let second_schedule = (1..=12)
            .map(|attempt| second.delay_after_failure(attempt))
            .collect::<Vec<_>>();
        assert_eq!(first_schedule, second_schedule);
        assert!(
            first_schedule
                .iter()
                .all(|delay| *delay <= REMOTE_RECONNECT_MAX_DELAY)
        );
        assert!(first_schedule.iter().any(|delay| !delay.is_zero()));

        for (index, delay) in first_schedule.iter().take(6).enumerate() {
            let multiplier = 1_u32 << index;
            let window = REMOTE_RECONNECT_BASE_DELAY
                .saturating_mul(multiplier)
                .min(REMOTE_RECONNECT_MAX_DELAY);
            assert!(
                *delay <= window,
                "attempt {} exceeded jitter window",
                index + 1
            );
        }
    }

    #[test]
    fn connection_timeline_is_monotonic_and_bounded() {
        let timeline = ConnectionTimeline::default();
        let correlation_id = timeline.next_correlation_id();
        for generation in 1..=(CONNECTION_TIMELINE_CAPACITY as u64 + 5) {
            timeline.record(
                correlation_id,
                generation,
                ConnectionTrigger::EventStreamEnded,
                ConnectionStage::Ready,
                Duration::from_millis(1),
                Duration::from_millis(generation),
                "managed",
                &"codex".to_string(),
                1,
                ConnectionAttemptOutcome::Succeeded,
            );
        }
        let entries = timeline.snapshot();
        assert_eq!(entries.len(), CONNECTION_TIMELINE_CAPACITY);
        assert!(
            entries
                .windows(2)
                .all(|pair| pair[0].sequence < pair[1].sequence)
        );
        assert_eq!(
            entries.last().map(|entry| entry.generation),
            Some(CONNECTION_TIMELINE_CAPACITY as u64 + 5)
        );
    }

    #[test]
    fn server_config_local() {
        let config = ServerConfig {
            server_id: "local-1".into(),
            display_name: "My Mac".into(),
            host: "127.0.0.1".into(),
            port: 0,
            websocket_url: None,
            is_local: true,
            tls: false,
        };
        assert!(config.is_local);
        assert_eq!(config.server_id, "local-1");
    }

    #[test]
    fn server_config_remote() {
        let config = ServerConfig {
            server_id: "remote-1".into(),
            display_name: "Cloud Server".into(),
            host: "codex.example.com".into(),
            port: 443,
            websocket_url: None,
            is_local: false,
            tls: true,
        };
        assert!(!config.is_local);
        assert!(config.tls);
        assert_eq!(config.port, 443);
    }

    #[test]
    fn connection_health_disconnected_eq() {
        assert_eq!(
            ConnectionHealth::Disconnected,
            ConnectionHealth::Disconnected
        );
    }

    #[test]
    fn connection_health_connecting_eq() {
        let a = ConnectionHealth::Connecting {
            attempt: 1,
            max_attempts: 5,
        };
        let b = ConnectionHealth::Connecting {
            attempt: 1,
            max_attempts: 5,
        };
        assert_eq!(a, b);
    }

    #[test]
    fn connection_health_connecting_ne_different_attempt() {
        let a = ConnectionHealth::Connecting {
            attempt: 1,
            max_attempts: 5,
        };
        let b = ConnectionHealth::Connecting {
            attempt: 2,
            max_attempts: 5,
        };
        assert_ne!(a, b);
    }

    #[test]
    fn connection_health_connected_eq() {
        assert_eq!(ConnectionHealth::Connected, ConnectionHealth::Connected);
    }

    #[test]
    fn connection_health_different_variants_ne() {
        assert_ne!(ConnectionHealth::Connected, ConnectionHealth::Disconnected);
        assert_ne!(
            ConnectionHealth::Connecting {
                attempt: 1,
                max_attempts: 5
            },
            ConnectionHealth::Connected,
        );
    }

    #[test]
    fn connection_health_unresponsive_same_instant() {
        let now = Instant::now();
        let a = ConnectionHealth::Unresponsive { since: now };
        let b = ConnectionHealth::Unresponsive { since: now };
        assert_eq!(a, b);
    }

    #[test]
    fn health_watch_initial_value() {
        let (tx, rx) = watch::channel(ConnectionHealth::Disconnected);
        assert_eq!(*rx.borrow(), ConnectionHealth::Disconnected);
        let _ = tx.send(ConnectionHealth::Connected);
        assert_eq!(*rx.borrow(), ConnectionHealth::Connected);
    }

    #[test]
    fn health_watch_multiple_transitions() {
        let (tx, rx) = watch::channel(ConnectionHealth::Disconnected);

        let _ = tx.send(ConnectionHealth::Connecting {
            attempt: 1,
            max_attempts: 3,
        });
        assert_eq!(
            *rx.borrow(),
            ConnectionHealth::Connecting {
                attempt: 1,
                max_attempts: 3
            }
        );

        let _ = tx.send(ConnectionHealth::Connected);
        assert_eq!(*rx.borrow(), ConnectionHealth::Connected);

        let _ = tx.send(ConnectionHealth::Disconnected);
        assert_eq!(*rx.borrow(), ConnectionHealth::Disconnected);
    }

    // -- Event bridge tests (using string-based bridge for backward compat) --

    fn spawn_string_event_bridge(
        mut event_rx: broadcast::Receiver<String>,
        notification_tx: broadcast::Sender<(String, JsonValue)>,
        server_request_tx: broadcast::Sender<(JsonValue, String, JsonValue)>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                match event_rx.recv().await {
                    Ok(json_str) => {
                        let parsed: JsonValue = match serde_json::from_str(&json_str) {
                            Ok(v) => v,
                            Err(e) => {
                                warn!("event bridge: failed to parse event JSON: {e}");
                                continue;
                            }
                        };

                        let has_id = parsed.get("id").is_some();
                        let method = parsed
                            .get("method")
                            .and_then(|m| m.as_str())
                            .map(|s| s.to_string());
                        let params = parsed.get("params").cloned().unwrap_or(JsonValue::Null);

                        match (has_id, method) {
                            (true, Some(method)) => {
                                let id = parsed.get("id").cloned().unwrap_or(JsonValue::Null);
                                let _ = server_request_tx.send((id, method, params));
                            }
                            (false, Some(method)) => {
                                let _ = notification_tx.send((method, params));
                            }
                            (true, None) => {}
                            (false, None) => {}
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        })
    }

    #[tokio::test]
    async fn event_bridge_routes_notification() {
        let (event_tx, _) = broadcast::channel::<String>(16);
        let (notif_tx, mut notif_rx) = broadcast::channel::<(String, JsonValue)>(16);
        let (req_tx, _req_rx) = broadcast::channel::<(JsonValue, String, JsonValue)>(16);

        let event_rx = event_tx.subscribe();
        let _handle = spawn_string_event_bridge(event_rx, notif_tx, req_tx);

        let notif = json!({"method": "codex/event/turnComplete", "params": {"turn_id": "t1"}});
        event_tx.send(notif.to_string()).unwrap();

        let (method, params) =
            tokio::time::timeout(std::time::Duration::from_secs(2), notif_rx.recv())
                .await
                .expect("should receive within timeout")
                .expect("should receive notification");

        assert_eq!(method, "codex/event/turnComplete");
        assert_eq!(params, json!({"turn_id": "t1"}));
        _handle.abort();
    }

    #[tokio::test]
    async fn event_bridge_routes_server_request() {
        let (event_tx, _) = broadcast::channel::<String>(16);
        let (notif_tx, _notif_rx) = broadcast::channel::<(String, JsonValue)>(16);
        let (req_tx, mut req_rx) = broadcast::channel::<(JsonValue, String, JsonValue)>(16);

        let event_rx = event_tx.subscribe();
        let _handle = spawn_string_event_bridge(event_rx, notif_tx, req_tx);

        let req = json!({"id": "srv-42", "method": "tools/approve", "params": {"tool": "bash"}});
        event_tx.send(req.to_string()).unwrap();

        let (id, method, params) =
            tokio::time::timeout(std::time::Duration::from_secs(2), req_rx.recv())
                .await
                .expect("should receive within timeout")
                .expect("should receive server request");

        assert_eq!(id, json!("srv-42"));
        assert_eq!(method, "tools/approve");
        assert_eq!(params, json!({"tool": "bash"}));
        _handle.abort();
    }

    #[tokio::test]
    async fn event_bridge_skips_response_like_events() {
        let (event_tx, _) = broadcast::channel::<String>(16);
        let (notif_tx, mut notif_rx) = broadcast::channel::<(String, JsonValue)>(16);
        let (req_tx, mut req_rx) = broadcast::channel::<(JsonValue, String, JsonValue)>(16);

        let event_rx = event_tx.subscribe();
        let _handle = spawn_string_event_bridge(event_rx, notif_tx, req_tx);

        let resp = json!({"id": 1, "result": {"ok": true}});
        event_tx.send(resp.to_string()).unwrap();

        let notif = json!({"method": "ping"});
        event_tx.send(notif.to_string()).unwrap();

        let (method, _) = tokio::time::timeout(std::time::Duration::from_secs(2), notif_rx.recv())
            .await
            .expect("should receive within timeout")
            .expect("should receive notification");

        assert_eq!(method, "ping");
        assert!(req_rx.try_recv().is_err());
        _handle.abort();
    }

    #[tokio::test]
    async fn event_bridge_handles_malformed_json() {
        let (event_tx, _) = broadcast::channel::<String>(16);
        let (notif_tx, mut notif_rx) = broadcast::channel::<(String, JsonValue)>(16);
        let (req_tx, _req_rx) = broadcast::channel::<(JsonValue, String, JsonValue)>(16);

        let event_rx = event_tx.subscribe();
        let _handle = spawn_string_event_bridge(event_rx, notif_tx, req_tx);

        event_tx.send("not valid json".to_string()).unwrap();

        let notif = json!({"method": "test/ok"});
        event_tx.send(notif.to_string()).unwrap();

        let (method, _) = tokio::time::timeout(std::time::Duration::from_secs(2), notif_rx.recv())
            .await
            .expect("should receive within timeout")
            .expect("should receive notification");

        assert_eq!(method, "test/ok");
        _handle.abort();
    }

    #[tokio::test]
    async fn event_bridge_handles_missing_params() {
        let (event_tx, _) = broadcast::channel::<String>(16);
        let (notif_tx, mut notif_rx) = broadcast::channel::<(String, JsonValue)>(16);
        let (req_tx, _req_rx) = broadcast::channel::<(JsonValue, String, JsonValue)>(16);

        let event_rx = event_tx.subscribe();
        let _handle = spawn_string_event_bridge(event_rx, notif_tx, req_tx);

        let notif = json!({"method": "heartbeat"});
        event_tx.send(notif.to_string()).unwrap();

        let (method, params) =
            tokio::time::timeout(std::time::Duration::from_secs(2), notif_rx.recv())
                .await
                .expect("should receive within timeout")
                .expect("should receive notification");

        assert_eq!(method, "heartbeat");
        assert_eq!(params, JsonValue::Null);
        _handle.abort();
    }

    #[tokio::test]
    async fn event_bridge_stops_on_channel_close() {
        let (event_tx, _) = broadcast::channel::<String>(16);
        let (notif_tx, _notif_rx) = broadcast::channel::<(String, JsonValue)>(16);
        let (req_tx, _req_rx) = broadcast::channel::<(JsonValue, String, JsonValue)>(16);

        let event_rx = event_tx.subscribe();
        let handle = spawn_string_event_bridge(event_rx, notif_tx, req_tx);

        drop(event_tx);

        let result = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
        assert!(
            result.is_ok(),
            "bridge task should complete when channel closes"
        );
    }

    #[test]
    fn ws_url_construction_no_tls() {
        let config = ServerConfig {
            server_id: "s1".into(),
            display_name: "Test".into(),
            host: "192.168.1.100".into(),
            port: 8080,
            websocket_url: None,
            is_local: false,
            tls: false,
        };
        let scheme = if config.tls { "wss" } else { "ws" };
        let url = format!("{scheme}://{}:{}", config.host, config.port);
        assert_eq!(url, "ws://192.168.1.100:8080");
    }

    #[test]
    fn ws_url_construction_with_tls() {
        let config = ServerConfig {
            server_id: "s2".into(),
            display_name: "Secure".into(),
            host: "codex.example.com".into(),
            port: 443,
            websocket_url: None,
            is_local: false,
            tls: true,
        };
        let scheme = if config.tls { "wss" } else { "ws" };
        let url = format!("{scheme}://{}:{}", config.host, config.port);
        assert_eq!(url, "wss://codex.example.com:443");
    }

    #[test]
    fn json_value_to_request_id_integer() {
        let id = json_value_to_request_id(&json!(42)).unwrap();
        assert!(matches!(id, RequestId::Integer(42)));
    }

    #[test]
    fn json_value_to_request_id_string() {
        let id = json_value_to_request_id(&json!("srv-1")).unwrap();
        assert!(matches!(id, RequestId::String(ref s) if s == "srv-1"));
    }

    #[test]
    fn json_value_to_request_id_invalid() {
        let result = json_value_to_request_id(&json!(true));
        assert!(result.is_err());
    }

    #[test]
    fn next_request_id_is_monotonic() {
        let a = next_request_id();
        let b = next_request_id();
        let c = next_request_id();
        assert!(b > a);
        assert!(c > b);
    }

    #[test]
    fn in_process_config_default() {
        let config = InProcessConfig::default();
        assert_eq!(config.channel_capacity, 256);
        assert!(config.codex_home.is_none());
        assert!(config.working_directory.is_none());
    }

    #[test]
    fn openai_base_url_from_env_trims_empty_and_trailing_slashes() {
        let _guard = env_lock().lock().expect("env lock should not be poisoned");
        let original = std::env::var_os(OPENAI_BASE_URL_ENV_KEY);

        unsafe {
            std::env::set_var(OPENAI_BASE_URL_ENV_KEY, " http://localhost:11434/v1/// ");
        }
        assert_eq!(
            openai_base_url_from_env(),
            Some("http://localhost:11434/v1".to_string())
        );

        unsafe {
            std::env::set_var(OPENAI_BASE_URL_ENV_KEY, "   ");
        }
        assert_eq!(openai_base_url_from_env(), None);

        match original {
            Some(value) => unsafe {
                std::env::set_var(OPENAI_BASE_URL_ENV_KEY, value);
            },
            None => unsafe {
                std::env::remove_var(OPENAI_BASE_URL_ENV_KEY);
            },
        }
    }

    #[test]
    fn prepare_ios_runtime_environment_sets_codex_home_and_tls_bundle() {
        let _guard = env_lock().lock().expect("env lock should not be poisoned");
        let original_codex_home = std::env::var_os("CODEX_HOME");
        let original_ssl_cert_file = std::env::var_os("SSL_CERT_FILE");
        let codex_home = unique_temp_path("ios-runtime");

        unsafe {
            std::env::set_var("CODEX_HOME", &codex_home);
            std::env::remove_var("SSL_CERT_FILE");
        }

        let canonical = prepare_ios_runtime_environment(&codex_home)
            .expect("ios runtime environment should initialize");
        let pem_path = canonical.join("cacert.pem");

        assert_eq!(
            std::env::var_os("CODEX_HOME"),
            Some(canonical.clone().into())
        );
        assert_eq!(
            std::env::var_os("SSL_CERT_FILE"),
            Some(pem_path.clone().into())
        );
        assert!(pem_path.is_file(), "cacert.pem should be written");

        if let Some(value) = original_codex_home {
            unsafe {
                std::env::set_var("CODEX_HOME", value);
            }
        } else {
            unsafe {
                std::env::remove_var("CODEX_HOME");
            }
        }

        if let Some(value) = original_ssl_cert_file {
            unsafe {
                std::env::set_var("SSL_CERT_FILE", value);
            }
        } else {
            unsafe {
                std::env::remove_var("SSL_CERT_FILE");
            }
        }

        let _ = std::fs::remove_dir_all(codex_home);
    }
}
