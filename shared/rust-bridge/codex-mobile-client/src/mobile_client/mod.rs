use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, RwLock, Weak};
use tokio::sync::{Mutex, broadcast};
use tracing::{debug, info, trace, warn};
use url::Url;

use crate::discovery::{DiscoveredServer, DiscoveryConfig, DiscoveryService, MdnsSeed};
use crate::session::connection::InProcessConfig;
use crate::session::connection::{
    RemoteSessionExtras, RuntimeRemoteSessionResource, ServerConfig, ServerEvent, ServerSession,
    SlingshotReconnectTransport, SshReconnectTransport, connect_remote_client_over_slingshot,
    remote_connect_args,
};
use crate::session::events::{EventProcessor, UiEvent};
use crate::slingshot_url::build_slingshot_connection_url;
use crate::ssh::{SshBootstrapResult, SshBootstrapTransport, SshClient, SshCredentials};
use crate::store::snapshot::ServerMutatingCommandKind;
use crate::store::{
    AppConnectionProgressSnapshot, AppQueuedFollowUpKind, AppQueuedFollowUpPreview, AppSnapshot,
    AppStoreReducer, AppStoreUpdateRecord, ServerHealthSnapshot, ThreadSnapshot,
};
use crate::transport::{RpcError, TransportError};
use crate::types::{
    AgentRuntimeInfo, AgentRuntimeKind, AppCollaborationModePreset, AppModeKind,
    ApprovalDecisionValue, PendingApproval, PendingApprovalSeed, PendingUserInputAnswer,
    PendingUserInputRequest, PendingUserInputResponseKind, PendingUserInputSeed, ThreadInfo,
    ThreadKey, ThreadSummaryStatus,
};
use codex_app_server_protocol as upstream;

mod dynamic_tools;
mod event_loop;
pub(crate) mod minigame;
mod runtime_routing;
mod slingshot;
mod ssh_connection;
mod store_listener;
#[cfg(test)]
mod tests;
mod thread_operations;
pub(crate) mod thread_projection;
mod user_input;

use self::dynamic_tools::*;
#[allow(unused_imports)]
pub(crate) use self::runtime_routing::ResolvedModelSelection;
#[allow(unused_imports)]
use self::runtime_routing::{non_empty_trimmed, runtime_for_model_hint};
#[allow(unused_imports)]
use self::slingshot::is_slingshot_initialize_timeout;
pub(crate) use self::slingshot::slingshot_user_agent;
use self::store_listener::*;
use self::thread_projection::*;
pub use self::thread_projection::{
    copy_thread_runtime_fields, reasoning_effort_from_string, reasoning_effort_string,
    reconcile_active_turn, thread_info_from_upstream_thread,
    thread_snapshot_from_upstream_thread_with_overrides,
};
#[allow(unused_imports)]
use self::user_input::normalize_pending_user_input_answers;

const MOBILE_CLIENT_TRACING_TARGET: &str = module_path!();
const DEFAULT_TURN_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Clone, Debug)]
struct PendingTurnReconciliation {
    id: i64,
    baseline_turn_ids: HashSet<String>,
    baseline_history_known: bool,
    causal_anchor_turn_id: Option<String>,
    repair_cursor: Option<String>,
    candidate_replay_turn_id: Option<String>,
    unanchored_replay_turn_id: Option<String>,
}

impl PendingTurnReconciliation {
    fn from_thread(thread: Option<&ThreadSnapshot>) -> Self {
        Self::from_thread_with_anchor(thread, None)
    }

    fn from_thread_with_anchor(
        thread: Option<&ThreadSnapshot>,
        causal_anchor_turn_id: Option<String>,
    ) -> Self {
        thread
            .map(|thread| Self {
                id: crate::next_request_id(),
                baseline_turn_ids: thread
                    .items
                    .iter()
                    .filter_map(|item| item.source_turn_id.clone())
                    .collect(),
                baseline_history_known: thread.initial_turns_loaded,
                causal_anchor_turn_id: causal_anchor_turn_id.clone(),
                repair_cursor: None,
                candidate_replay_turn_id: None,
                unanchored_replay_turn_id: None,
            })
            .unwrap_or_else(|| Self {
                id: crate::next_request_id(),
                baseline_turn_ids: HashSet::new(),
                baseline_history_known: false,
                causal_anchor_turn_id,
                repair_cursor: None,
                candidate_replay_turn_id: None,
                unanchored_replay_turn_id: None,
            })
    }
}

/// Top-level entry point for platform code (iOS / Android).
///
/// Ties together server sessions, thread management, event processing,
/// discovery, auth, caching, and voice handoff into a single facade.
/// All methods are safe to call from any thread (`Send + Sync`).
pub struct MobileClient {
    pub(crate) sessions: Arc<RwLock<HashMap<String, Arc<ServerSession>>>>,
    pub(crate) event_processor: Arc<EventProcessor>,
    pub app_store: Arc<AppStoreReducer>,
    pub agent_metadata: Arc<crate::store::AgentMetadataStore>,
    pub(crate) discovery: RwLock<DiscoveryService>,
    oauth_callback_tunnels: Arc<Mutex<HashMap<String, OAuthCallbackTunnel>>>,
    slingshot_apis: Arc<StdMutex<HashMap<String, codex_slingshot::SlingshotApi>>>,
    pub(crate) recorder: Arc<crate::recorder::MessageRecorder>,
    /// One-shot hooks that fulfill when the next `show_widget` dynamic tool
    /// call finalizes on a specific thread. Keyed by `thread_id`.
    /// Used by `AppClient::update_saved_app`.
    pub(crate) widget_waiters: Arc<StdMutex<HashMap<String, WidgetWaiter>>>,
    /// Directory where `saved_apps.rs` persists the app index + per-app
    /// HTML/state files. Set once at process start by the platform
    /// (iOS/Android) via `AppClient::set_saved_apps_directory`. When
    /// `Some`, the `show_widget` auto-upsert hook is enabled; when
    /// `None`, the hook is skipped (pre-R2 callers / tests).
    pub(crate) saved_apps_directory: Arc<StdMutex<Option<String>>>,
    /// Directory where the Slingshot controller enrollment is persisted.
    /// This holds the device-key enrollment and short-lived remote-control
    /// session token so cold launches can reconnect without another browser
    /// step-up while the token remains valid.
    pub(crate) slingshot_credentials_directory: Arc<StdMutex<Option<String>>>,
    direct_resumed_threads: Arc<StdMutex<HashSet<ThreadKey>>>,
    thread_runtime_routes: Arc<StdMutex<HashMap<ThreadKey, AgentRuntimeKind>>>,
    turn_start_locks: Arc<StdMutex<HashMap<ThreadKey, Weak<Mutex<()>>>>>,
    pending_turn_reconciliation: Arc<StdMutex<HashMap<ThreadKey, PendingTurnReconciliation>>>,
    turn_request_timeout: std::time::Duration,
    /// In-flight guided-SSH-connect flows, keyed by server_id. Held on
    /// `MobileClient` so repeated connect attempts can reuse the same
    /// bootstrap task.
    pub(crate) ssh_bootstrap_flows:
        Arc<tokio::sync::Mutex<HashMap<String, ManagedSshBootstrapFlow>>>,
    /// Live terminal session handles keyed by session id. The store
    /// holds the FFI-visible snapshot
    /// (`AppSnapshot.terminal_sessions`); these are the strong
    /// references that keep the underlying PTY / SSH channel alive while
    /// view-scoped renderers come and go. Cleared per-id when the
    /// session exits or the caller explicitly closes it.
    pub(crate) terminal_sessions:
        Arc<StdMutex<HashMap<String, Arc<crate::terminal::TerminalSession>>>>,
    /// Optional native persistence/custody ports for the Rust-owned background
    /// relay. Kept on the shared client so recreating an FFI `AppClient`
    /// handle cannot silently drop or fork relay state.
    pub(crate) background_relay:
        Arc<RwLock<Option<Arc<crate::background_relay::ConfiguredBackgroundRelay>>>>,
    /// Serializes configuration replacement and clearing across all FFI
    /// handles, including secure integrity-key bootstrap.
    pub(crate) background_relay_configuration: Arc<tokio::sync::Mutex<()>>,
    /// Optional native persistence and hardware-custody configuration for the
    /// Rust-owned Remora Link v2 lifecycle. Shared by every AppClient handle.
    pub(crate) remora_link:
        Arc<RwLock<Option<Arc<crate::ffi::remora_link_v2::ConfiguredRemoraLink>>>>,
    /// Serializes Remora Link configuration replacement and endpoint teardown.
    pub(crate) remora_link_configuration: Arc<tokio::sync::RwLock<()>>,
}

/// State for a single in-flight guided SSH connect.
pub struct ManagedSshBootstrapFlow {}

/// A waiter registered by `update_saved_app` to receive the next
/// finalized `show_widget` on a specific thread. See
/// `MobileClient::widget_waiters` and `dynamic_tools::try_fulfill_widget_waiter`.
pub struct WidgetWaiter {
    pub sender: tokio::sync::oneshot::Sender<WidgetFinalizedPayload>,
}

#[derive(Debug, Clone)]
pub struct WidgetFinalizedPayload {
    pub widget_html: String,
    pub width: f64,
    pub height: f64,
    pub title: String,
}

#[derive(Debug, Clone)]
struct OAuthCallbackTunnel {
    login_id: String,
    local_port: u16,
}

#[derive(Debug, Clone)]
pub struct SshBridgeConnectOutcome {
    pub server_id: String,
    pub node_id: String,
    pub agent_name: String,
}

#[derive(Clone)]
pub(crate) struct ColdReconnectGuard {
    session: Arc<ServerSession>,
    generation: u64,
}

fn should_fallback_to_thread_metadata_after_resume_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("no rollout found for thread id")
        || lower.contains("remote app-server worker channel is closed")
}

fn should_try_next_runtime_after_thread_lookup_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("no rollout found for thread id")
        || lower.contains("thread cannot be found")
        || lower.contains("thread not found")
        || lower.contains("no thread found")
        || lower.contains("unknown thread")
}

/// Returns true when an RPC error string looks like a JSON-RPC -32601
/// "method not found" error.
fn is_method_not_found(error: &str) -> bool {
    error.contains("-32601")
        || error.to_ascii_lowercase().contains("method not found")
        || error.to_ascii_lowercase().contains("not implemented")
}

impl MobileClient {
    /// Create a new `MobileClient`.
    pub fn new() -> Arc<Self> {
        Self::new_with_turn_request_timeout(DEFAULT_TURN_REQUEST_TIMEOUT)
    }

    pub(crate) fn new_with_turn_request_timeout(
        turn_request_timeout: std::time::Duration,
    ) -> Arc<Self> {
        crate::logging::install_tracing_subscriber();
        let event_processor = Arc::new(EventProcessor::new());
        let app_store = Arc::new(AppStoreReducer::new());
        let sessions = Arc::new(RwLock::new(HashMap::new()));
        Arc::new_cyclic(|owner: &Weak<MobileClient>| {
            spawn_store_listener(
                owner.clone(),
                Arc::clone(&app_store),
                Arc::clone(&sessions),
                event_processor.subscribe(),
            );
            Self {
                sessions,
                event_processor,
                app_store,
                agent_metadata: crate::store::AgentMetadataStore::new(),
                discovery: RwLock::new(DiscoveryService::new(DiscoveryConfig::default())),
                oauth_callback_tunnels: Arc::new(Mutex::new(HashMap::new())),
                slingshot_apis: Arc::new(StdMutex::new(HashMap::new())),
                recorder: Arc::new(crate::recorder::MessageRecorder::new()),
                widget_waiters: Arc::new(StdMutex::new(HashMap::new())),
                saved_apps_directory: Arc::new(StdMutex::new(None)),
                slingshot_credentials_directory: Arc::new(StdMutex::new(None)),
                direct_resumed_threads: Arc::new(StdMutex::new(HashSet::new())),
                thread_runtime_routes: Arc::new(StdMutex::new(HashMap::new())),
                turn_start_locks: Arc::new(StdMutex::new(HashMap::new())),
                pending_turn_reconciliation: Arc::new(StdMutex::new(HashMap::new())),
                turn_request_timeout,
                ssh_bootstrap_flows: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
                terminal_sessions: Arc::new(StdMutex::new(HashMap::new())),
                background_relay: Arc::new(RwLock::new(None)),
                background_relay_configuration: Arc::new(tokio::sync::Mutex::new(())),
                remora_link: Arc::new(RwLock::new(None)),
                remora_link_configuration: Arc::new(tokio::sync::RwLock::new(())),
            }
        })
    }

    fn sessions_write(
        &self,
    ) -> std::sync::RwLockWriteGuard<'_, HashMap<String, Arc<ServerSession>>> {
        match self.sessions.write() {
            Ok(guard) => guard,
            Err(error) => {
                warn!("MobileClient: recovering poisoned sessions write lock");
                error.into_inner()
            }
        }
    }

    pub(crate) fn sessions_read(
        &self,
    ) -> std::sync::RwLockReadGuard<'_, HashMap<String, Arc<ServerSession>>> {
        match self.sessions.read() {
            Ok(guard) => guard,
            Err(error) => {
                warn!("MobileClient: recovering poisoned sessions read lock");
                error.into_inner()
            }
        }
    }

    fn turn_start_lock(&self, key: &ThreadKey) -> Arc<Mutex<()>> {
        let mut locks = self
            .turn_start_locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(key).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(Mutex::new(()));
        locks.insert(key.clone(), Arc::downgrade(&lock));
        lock
    }

    // ── Internal RPC helpers ──────────────────────────────────────────────

    pub(crate) async fn server_get_account(
        &self,
        server_id: &str,
        params: upstream::GetAccountParams,
    ) -> Result<upstream::GetAccountResponse, crate::RpcClientError> {
        use crate::{RpcClientError, next_request_id};
        self.request_typed_for_server(
            server_id,
            upstream::ClientRequest::GetAccount {
                request_id: upstream::RequestId::Integer(next_request_id()),
                params,
            },
        )
        .await
        .map_err(RpcClientError::Rpc)
    }

    pub(crate) async fn server_thread_fork(
        &self,
        server_id: &str,
        params: upstream::ThreadForkParams,
    ) -> Result<upstream::ThreadForkResponse, crate::RpcClientError> {
        use crate::{RpcClientError, next_request_id};
        self.request_typed_for_server(
            server_id,
            upstream::ClientRequest::ThreadFork {
                request_id: upstream::RequestId::Integer(next_request_id()),
                params,
            },
        )
        .await
        .map_err(RpcClientError::Rpc)
    }

    pub(crate) async fn server_thread_rollback(
        &self,
        server_id: &str,
        params: upstream::ThreadRollbackParams,
    ) -> Result<upstream::ThreadRollbackResponse, crate::RpcClientError> {
        use crate::{RpcClientError, next_request_id};
        self.request_typed_for_server(
            server_id,
            upstream::ClientRequest::ThreadRollback {
                request_id: upstream::RequestId::Integer(next_request_id()),
                params,
            },
        )
        .await
        .map_err(RpcClientError::Rpc)
    }

    #[allow(dead_code)]
    pub(crate) async fn server_thread_list(
        &self,
        server_id: &str,
        params: upstream::ThreadListParams,
    ) -> Result<upstream::ThreadListResponse, crate::RpcClientError> {
        use crate::{RpcClientError, next_request_id};
        let runtime_kinds = self
            .get_session(server_id)
            .map_err(|error| RpcClientError::Rpc(error.to_string()))?
            .runtime_kinds();
        let mut merged = upstream::ThreadListResponse {
            data: Vec::new(),
            next_cursor: None,
            backwards_cursor: None,
        };

        for runtime_kind in runtime_kinds {
            let response: upstream::ThreadListResponse = self
                .request_typed_for_server_runtime(
                    server_id,
                    runtime_kind,
                    upstream::ClientRequest::ThreadList {
                        request_id: upstream::RequestId::Integer(next_request_id()),
                        params: params.clone(),
                    },
                )
                .await
                .map_err(RpcClientError::Rpc)?;
            merged.data.extend(response.data);
            if merged.next_cursor.is_none() {
                merged.next_cursor = response.next_cursor;
            }
            if merged.backwards_cursor.is_none() {
                merged.backwards_cursor = response.backwards_cursor;
            }
        }

        Ok(merged)
    }

    pub(crate) async fn server_collaboration_mode_list(
        &self,
        server_id: &str,
    ) -> Result<Vec<AppCollaborationModePreset>, crate::RpcClientError> {
        use crate::{RpcClientError, next_request_id};
        let response = self
            .request_typed_for_server::<upstream::CollaborationModeListResponse>(
                server_id,
                upstream::ClientRequest::CollaborationModeList {
                    request_id: upstream::RequestId::Integer(next_request_id()),
                    params: upstream::CollaborationModeListParams::default(),
                },
            )
            .await
            .map_err(RpcClientError::Rpc)?;

        Ok(response
            .data
            .into_iter()
            .filter_map(|mask| AppCollaborationModePreset::try_from(mask).ok())
            .collect())
    }

    fn discovery_write(&self) -> std::sync::RwLockWriteGuard<'_, DiscoveryService> {
        match self.discovery.write() {
            Ok(guard) => guard,
            Err(error) => {
                warn!("MobileClient: recovering poisoned discovery write lock");
                error.into_inner()
            }
        }
    }

    fn discovery_read(&self) -> std::sync::RwLockReadGuard<'_, DiscoveryService> {
        match self.discovery.read() {
            Ok(guard) => guard,
            Err(error) => {
                warn!("MobileClient: recovering poisoned discovery read lock");
                error.into_inner()
            }
        }
    }

    async fn clear_oauth_callback_tunnel(&self, server_id: &str) {
        self.clear_oauth_callback_tunnel_for_session(server_id, None)
            .await;
    }

    async fn clear_oauth_callback_tunnel_for_session(
        &self,
        server_id: &str,
        session: Option<Arc<ServerSession>>,
    ) {
        let tunnel = {
            let mut tunnels = self.oauth_callback_tunnels.lock().await;
            tunnels.remove(server_id)
        };
        let session = session.or_else(|| self.sessions_read().get(server_id).cloned());
        if let Some(tunnel) = tunnel
            && let Some(session) = session
            && let Some(ssh_client) = session.ssh_client()
        {
            ssh_client.abort_forward_port(tunnel.local_port).await;
        }
    }

    async fn replace_oauth_callback_tunnel(
        &self,
        server_id: &str,
        login_id: &str,
        local_port: u16,
    ) {
        self.clear_oauth_callback_tunnel(server_id).await;
        let mut tunnels = self.oauth_callback_tunnels.lock().await;
        tunnels.insert(
            server_id.to_string(),
            OAuthCallbackTunnel {
                login_id: login_id.to_string(),
                local_port,
            },
        );
    }

    fn existing_active_session(&self, server_id: &str) -> Option<Arc<ServerSession>> {
        let session = self.sessions_read().get(server_id).cloned()?;
        let health_rx = session.health();
        match health_rx.borrow().clone() {
            crate::session::connection::ConnectionHealth::Disconnected => None,
            _ => Some(session),
        }
    }

    pub(crate) fn connection_reconnect_state(
        &self,
        server_id: &str,
    ) -> Option<crate::session::connection::ReconnectHealthState> {
        let session = self.sessions_read().get(server_id).cloned()?;
        Some(session.reconnect_health_state())
    }

    pub(crate) fn cold_reconnect_guard(&self, server_id: &str) -> Option<ColdReconnectGuard> {
        let session = self.sessions_read().get(server_id).cloned()?;
        let state = session.reconnect_health_state();
        if !state.has_degraded_runtime || state.has_connecting_runtime {
            return None;
        }
        Some(ColdReconnectGuard {
            session,
            generation: state.generation,
        })
    }

    pub(crate) async fn wait_for_runtime_reconnects_to_settle(
        &self,
        server_id: &str,
        deadline: std::time::Duration,
    ) -> bool {
        let session = self.sessions_read().get(server_id).cloned();
        match session {
            Some(session) => {
                session
                    .wait_for_runtime_reconnects_to_settle(deadline)
                    .await
            }
            None => true,
        }
    }

    pub(crate) async fn replace_existing_session(&self, server_id: &str) {
        let _ = self
            .replace_existing_session_with_guard(server_id, None)
            .await;
    }

    async fn replace_existing_session_with_guard(
        &self,
        server_id: &str,
        cold_guard: Option<&ColdReconnectGuard>,
    ) -> bool {
        let existing = {
            let mut sessions = self.sessions_write();
            if let Some(cold_guard) = cold_guard {
                let Some(current) = sessions.get(server_id).cloned() else {
                    return false;
                };
                if !Arc::ptr_eq(&current, &cold_guard.session)
                    || !current.try_claim_cold_repair(cold_guard.generation)
                {
                    return false;
                }
            }
            sessions.remove(server_id)
        };
        self.clear_oauth_callback_tunnel_for_session(server_id, existing.clone())
            .await;
        self.clear_direct_resume_markers_for_server(server_id);
        if let Some(session) = existing {
            info!("MobileClient: replacing existing server session {server_id}");
            session.disconnect().await;
        }
        true
    }

    /// Common post-`connect_remote_multiplexed` attach work shared by every
    /// remote-connect orchestrator (Remora Link, SSH-direct, SSH-bridges).
    ///
    /// Runs the steps that are identical across transports: marking the server
    /// `Connected`, registering runtime info, spawning event/health readers,
    /// inserting into the session map, and queuing post-connect warmup.
    pub(crate) fn attach_remote_session(
        &self,
        server_id: &str,
        session: Arc<ServerSession>,
        runtime_infos: Vec<AgentRuntimeInfo>,
    ) {
        let session_runtime_kinds = session.runtime_kinds();
        info!(
            "MobileClient: attaching remote session server_id={} session_runtimes={:?} runtime_infos={:?}",
            server_id, session_runtime_kinds, runtime_infos
        );
        self.app_store
            .upsert_server(session.config(), ServerHealthSnapshot::Connected);
        self.app_store
            .update_server_agent_runtimes(server_id, runtime_infos);
        self.sessions_write()
            .insert(server_id.to_string(), Arc::clone(&session));
        self.spawn_event_reader(server_id.to_string(), Arc::clone(&session));
        self.spawn_health_reader(server_id.to_string(), Arc::clone(&session));
        self.spawn_post_connect_warmup(server_id.to_string(), session);
    }

    // ── Server Management ─────────────────────────────────────────────

    /// Connect to a local (in-process) Codex server.
    ///
    /// Returns the `server_id` from the config on success.
    pub async fn connect_local(
        &self,
        config: ServerConfig,
        in_process: InProcessConfig,
    ) -> Result<String, TransportError> {
        let server_id = config.server_id.clone();
        if self.existing_active_session(server_id.as_str()).is_some() {
            info!("MobileClient: reusing existing local server session {server_id}");
            return Ok(server_id);
        }
        self.replace_existing_session(server_id.as_str()).await;
        let session = Arc::new(ServerSession::connect_local(config, in_process).await?);
        self.app_store
            .upsert_server(session.config(), ServerHealthSnapshot::Connected);

        self.sessions_write()
            .insert(server_id.clone(), Arc::clone(&session));
        self.spawn_event_reader(server_id.clone(), Arc::clone(&session));
        self.spawn_health_reader(server_id.clone(), Arc::clone(&session));
        self.spawn_post_connect_warmup(server_id.clone(), session);

        info!("MobileClient: connected local server {server_id}");
        Ok(server_id)
    }

    /// Connect to a remote Codex server via WebSocket.
    ///
    /// Returns the `server_id` from the config on success.
    pub async fn connect_remote(&self, config: ServerConfig) -> Result<String, TransportError> {
        let server_id = config.server_id.clone();
        if self.existing_active_session(server_id.as_str()).is_some() {
            info!("MobileClient: reusing existing remote server session {server_id}");
            return Ok(server_id);
        }
        self.replace_existing_session(server_id.as_str()).await;
        let session = Arc::new(ServerSession::connect_remote(config).await?);
        self.app_store
            .upsert_server(session.config(), ServerHealthSnapshot::Connected);

        self.sessions_write()
            .insert(server_id.clone(), Arc::clone(&session));
        self.spawn_event_reader(server_id.clone(), Arc::clone(&session));
        self.spawn_health_reader(server_id.clone(), Arc::clone(&session));
        self.spawn_post_connect_warmup(server_id.clone(), session);

        info!("MobileClient: connected remote server {server_id}");
        Ok(server_id)
    }

    /// Hint every active session that the host network may have changed.
    /// Iroh-backed Remora Link sessions re-evaluate paths while TCP-based
    /// sessions use the default no-op.
    pub async fn notify_network_change(&self) {
        let sessions: Vec<Arc<ServerSession>> = self.sessions_read().values().cloned().collect();
        for session in sessions {
            session.notify_network_change().await;
        }
    }

    /// Disconnect a server by its ID.
    ///
    /// Always clears the server from the app store snapshot and drops any
    /// OAuth callback tunnel, even when no live session exists (e.g. the
    /// server was already disconnected or never connected this launch).
    /// Otherwise removing a disconnected server pill from the UI would be a
    /// no-op because the snapshot would still carry it.
    pub fn disconnect_server(&self, server_id: &str) {
        if server_id.starts_with("remora-link:") {
            let configured = match self.remora_link.read() {
                Ok(value) => value.clone(),
                Err(error) => error.into_inner().clone(),
            };
            if let Some(configured) = configured {
                configured.close_shells_for_host(server_id);
            }
        }
        let session = self.sessions_write().remove(server_id);
        self.clear_direct_resume_markers_for_server(server_id);
        self.app_store.remove_server(server_id);

        let inner = Arc::clone(&self.oauth_callback_tunnels);
        let server_id_owned = server_id.to_string();
        Self::spawn_detached(async move {
            inner.lock().await.remove(&server_id_owned);
            if let Some(session) = session {
                session.disconnect().await;
            }
        });
        info!("MobileClient: disconnected server {server_id}");
    }

    pub async fn restart_app_server(&self, server_id: &str) -> Result<(), TransportError> {
        self.clear_oauth_callback_tunnel(server_id).await;
        let session = self.sessions_write().remove(server_id);
        self.clear_direct_resume_markers_for_server(server_id);
        self.app_store.remove_server(server_id);
        let Some(session) = session else {
            return Err(TransportError::Disconnected);
        };

        info!("MobileClient: restarting app server {server_id}");
        session.restart_app_server_and_disconnect().await;
        Ok(())
    }

    /// Return the configs of all currently connected servers.
    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn connected_servers(&self) -> Vec<ServerConfig> {
        self.sessions_read()
            .values()
            .map(|s| s.config().clone())
            .collect()
    }

    // ── Threads ───────────────────────────────────────────────────────

    pub async fn sync_server_account(&self, server_id: &str) -> Result<(), RpcError> {
        self.get_session(server_id)?;
        let response = self
            .server_get_account(
                server_id,
                upstream::GetAccountParams {
                    refresh_token: false,
                },
            )
            .await
            .map_err(map_rpc_client_error)?;
        self.apply_account_response(server_id, &response);
        Ok(())
    }

    fn spawn_post_connect_warmup(&self, server_id: String, session: Arc<ServerSession>) {
        run_connect_warmup(
            Arc::clone(&self.sessions),
            Arc::clone(&self.app_store),
            server_id,
            session,
            "post-connect",
        );
    }

    pub async fn start_remote_ssh_oauth_login(&self, server_id: &str) -> Result<String, RpcError> {
        let session = self.get_session(server_id)?;
        if session.config().is_local {
            return Err(RpcError::Transport(TransportError::ConnectionFailed(
                "remote SSH OAuth is only available for remote servers".to_string(),
            )));
        }
        let ssh_client = session.ssh_client().ok_or_else(|| {
            RpcError::Transport(TransportError::ConnectionFailed(
                "remote ChatGPT login requires an SSH-backed connection".to_string(),
            ))
        })?;

        let params = upstream::LoginAccountParams::Chatgpt {
            codex_streamlined_login: false,
        };
        let response = self
            .request_typed_for_server::<upstream::LoginAccountResponse>(
                server_id,
                upstream::ClientRequest::LoginAccount {
                    request_id: upstream::RequestId::Integer(crate::next_request_id()),
                    params,
                },
            )
            .await
            .map_err(RpcError::Deserialization)?;
        self.reconcile_public_rpc(
            "account/login/start",
            server_id,
            Option::<&()>::None,
            &response,
        )
        .await?;

        let upstream::LoginAccountResponse::Chatgpt { login_id, auth_url } = response else {
            return Err(RpcError::Deserialization(
                "expected ChatGPT login response for remote SSH OAuth".to_string(),
            ));
        };

        let callback_port = remote_oauth_callback_port(&auth_url)?;
        self.clear_oauth_callback_tunnel(server_id).await;
        if let Err(error) = ssh_client
            .ensure_forward_port_to(callback_port, "127.0.0.1", callback_port)
            .await
        {
            let _ = self
                .request_typed_for_server::<upstream::CancelLoginAccountResponse>(
                    server_id,
                    upstream::ClientRequest::CancelLoginAccount {
                        request_id: upstream::RequestId::Integer(crate::next_request_id()),
                        params: upstream::CancelLoginAccountParams {
                            login_id: login_id.clone(),
                        },
                    },
                )
                .await;
            return Err(RpcError::Transport(TransportError::ConnectionFailed(
                format!(
                    "failed to open localhost callback tunnel on port {callback_port}: {error}"
                ),
            )));
        }
        self.replace_oauth_callback_tunnel(server_id, &login_id, callback_port)
            .await;
        Ok(auth_url)
    }

    pub fn snapshot(&self) -> AppSnapshot {
        self.app_store.snapshot()
    }

    pub fn subscribe_updates(&self) -> broadcast::Receiver<AppStoreUpdateRecord> {
        self.app_store.subscribe()
    }

    pub fn app_snapshot(&self) -> AppSnapshot {
        self.snapshot()
    }

    pub fn subscribe_app_updates(&self) -> broadcast::Receiver<AppStoreUpdateRecord> {
        self.subscribe_updates()
    }

    /// Open a new terminal session, store the strong handle on the
    /// client, register a snapshot entry on the reducer, and wire output
    /// bytes back into the ring buffer. Returns the generated session
    /// id.
    pub async fn open_terminal_session(
        &self,
        kind: crate::terminal::TerminalBackendKind,
        size: crate::terminal::TerminalSize,
        trust_store: Option<Arc<crate::terminal::TerminalSshTrustStore>>,
    ) -> Result<String, crate::terminal::TerminalError> {
        let session = match trust_store {
            Some(store) => {
                crate::terminal::TerminalSession::open_with_trust_store(kind.clone(), size, store)
                    .await?
            }
            None => crate::terminal::TerminalSession::open(kind.clone(), size).await?,
        };
        let session = Arc::new(session);
        let id = uuid::Uuid::new_v4().to_string();
        self.terminal_sessions
            .lock()
            .expect("terminal_sessions poisoned")
            .insert(id.clone(), Arc::clone(&session));
        self.app_store
            .open_terminal_session_record(id.clone(), kind, size.cols, size.rows);

        // Subscribe to the session's output to feed the ring buffer.
        let reducer = Arc::clone(&self.app_store);
        let id_for_listener = id.clone();
        let strong = Arc::clone(&session);
        let sessions = Arc::clone(&self.terminal_sessions);
        let listener: Box<dyn crate::terminal::TerminalOutputEventListener> =
            Box::new(TerminalRingListener {
                reducer,
                id: id_for_listener,
                sessions,
            });
        strong.subscribe_output_events_retained(listener);
        Ok(id)
    }

    /// Close a terminal session: drop the strong handle (which kills the
    /// underlying backend on the last reference being released), then
    /// mark the snapshot as exited. The snapshot's output_tail is
    /// retained until [`MobileClient::forget_terminal_session`].
    pub async fn close_terminal_session(
        &self,
        id: &str,
    ) -> Result<(), crate::terminal::TerminalError> {
        let session = self
            .terminal_sessions
            .lock()
            .expect("terminal_sessions poisoned")
            .remove(id);
        if let Some(session) = session {
            session.close_session().await?;
        }
        self.app_store.mark_terminal_exited(id, 0);
        Ok(())
    }

    /// Forget a session entirely (drop the snapshot's buffered output).
    pub fn forget_terminal_session(&self, id: &str) {
        self.terminal_sessions
            .lock()
            .expect("terminal_sessions poisoned")
            .remove(id);
        self.app_store.remove_terminal_session_record(id);
    }

    /// Return the live session handle for `id`, or `None` if the session
    /// has been closed.
    pub fn terminal_session_handle(
        &self,
        id: &str,
    ) -> Option<Arc<crate::terminal::TerminalSession>> {
        self.terminal_sessions
            .lock()
            .expect("terminal_sessions poisoned")
            .get(id)
            .cloned()
    }

    /// Write `bytes` to the currently-active terminal session, if any.
    /// Returns `Ok(false)` if there is no active session.
    pub async fn write_to_active_terminal(
        &self,
        bytes: Vec<u8>,
    ) -> Result<bool, crate::terminal::TerminalError> {
        let active_id = self.app_store.snapshot().active_terminal_id.clone();
        let Some(id) = active_id else {
            return Ok(false);
        };
        let Some(session) = self.terminal_session_handle(&id) else {
            return Ok(false);
        };
        session.write_input(bytes).await?;
        Ok(true)
    }

    pub fn set_active_thread(&self, key: Option<ThreadKey>) {
        self.app_store.set_active_thread(key);
    }

    pub async fn set_thread_collaboration_mode(
        &self,
        key: &ThreadKey,
        mode: AppModeKind,
    ) -> Result<(), RpcError> {
        self.get_session(&key.server_id)?;
        self.app_store.set_thread_collaboration_mode(key, mode);
        Ok(())
    }

    pub fn dismiss_plan_implementation_prompt(&self, key: &ThreadKey) {
        self.app_store.dismiss_plan_implementation_prompt(key);
    }

    pub async fn implement_plan(self: &Arc<Self>, key: &ThreadKey) -> Result<(), RpcError> {
        self.app_store.dismiss_plan_implementation_prompt(key);
        let thread = self.snapshot_thread(key).ok();
        self.app_store
            .set_thread_collaboration_mode(key, AppModeKind::Default);
        let collaboration_mode = thread
            .as_ref()
            .and_then(|t| collaboration_mode_from_thread(t, AppModeKind::Default, None, None));
        self.start_turn(
            &key.server_id,
            upstream::TurnStartParams {
                thread_id: key.thread_id.clone(),
                input: vec![upstream::UserInput::Text {
                    text: "Implement the plan.".to_string(),
                    text_elements: Vec::new(),
                }],
                responsesapi_client_metadata: None,
                cwd: None,
                runtime_workspace_roots: None,
                approval_policy: None,
                approvals_reviewer: None,
                sandbox_policy: None,
                environments: None,
                permissions: None,
                model: None,
                service_tier: None,
                effort: None,
                summary: None,
                personality: None,
                output_schema: None,
                collaboration_mode,
            },
        )
        .await
    }

    pub fn set_voice_handoff_thread(&self, key: Option<ThreadKey>) {
        self.app_store.set_voice_handoff_thread(key);
    }

    pub async fn scan_servers_with_mdns_context(
        &self,
        mdns_results: Vec<MdnsSeed>,
        local_ipv4: Option<String>,
    ) -> Vec<DiscoveredServer> {
        let discovery = self.discovery_write();
        discovery
            .scan_once_with_context(&mdns_results, local_ipv4.as_deref())
            .await
    }

    pub fn subscribe_scan_servers_with_mdns_context(
        &self,
        mdns_results: Vec<MdnsSeed>,
        local_ipv4: Option<String>,
    ) -> broadcast::Receiver<crate::discovery::ProgressiveDiscoveryUpdate> {
        let (tx, rx) = broadcast::channel(32);
        let discovery = self.discovery_read().clone_for_one_shot();

        Self::spawn_detached(async move {
            let _ = discovery
                .scan_once_progressive_with_context(&mdns_results, local_ipv4.as_deref(), &tx)
                .await;
        });

        rx
    }
}

/// Listener that feeds session output bytes into the reducer's ring
/// buffer and marks the session exited when the backend reports exit.
struct TerminalRingListener {
    reducer: Arc<AppStoreReducer>,
    id: String,
    sessions: Arc<StdMutex<HashMap<String, Arc<crate::terminal::TerminalSession>>>>,
}

impl crate::terminal::TerminalOutputEventListener for TerminalRingListener {
    fn on_event(&self, event: crate::terminal::TerminalOutputStreamEvent) {
        match event {
            crate::terminal::TerminalOutputStreamEvent::Snapshot { snapshot }
            | crate::terminal::TerminalOutputStreamEvent::Reset { snapshot } => {
                self.reducer
                    .replace_terminal_output(&self.id, &snapshot.bytes);
                if let Some(code) = snapshot.exit_code {
                    self.mark_exited(code);
                }
            }
            crate::terminal::TerminalOutputStreamEvent::Output { data, .. } => {
                self.reducer.append_terminal_output(&self.id, &data);
            }
            crate::terminal::TerminalOutputStreamEvent::Exited { code, .. } => {
                self.mark_exited(code);
            }
        }
    }
}

impl TerminalRingListener {
    fn mark_exited(&self, code: i32) {
        self.reducer.mark_terminal_exited(&self.id, code);
        self.sessions
            .lock()
            .expect("terminal_sessions poisoned")
            .remove(&self.id);
    }
}

pub(super) fn run_connect_warmup(
    sessions: Arc<RwLock<HashMap<String, Arc<ServerSession>>>>,
    app_store: Arc<AppStoreReducer>,
    server_id: String,
    session: Arc<ServerSession>,
    label: &'static str,
) {
    MobileClient::spawn_detached(async move {
        let runtime_kinds = session.runtime_kinds();
        if !runtime_kinds_support_account_sync(&runtime_kinds) {
            trace!(
                "MobileClient: {label} account sync skipped server_id={server_id} runtime_kinds={runtime_kinds:?}"
            );
            return;
        }
        match refresh_account_from_app_server(
            session,
            Arc::clone(&app_store),
            Arc::clone(&sessions),
            server_id.as_str(),
        )
        .await
        {
            Ok(()) => trace!("MobileClient: {label} account sync completed server_id={server_id}"),
            Err(error) => {
                warn!("MobileClient: {label} account sync failed server_id={server_id}: {error}")
            }
        }
    });
}

pub(super) fn runtime_kinds_support_account_sync(runtime_kinds: &[AgentRuntimeKind]) -> bool {
    runtime_kinds
        .iter()
        .any(|runtime_kind| runtime_kind == "codex")
}

/// Re-establish per-thread subscriptions on the server after a remote
/// transport reconnect.
///
/// Upstream codex routes per-turn events (`TurnStarted`, `Item*`,
/// `TurnCompleted`) only to the connections currently in each thread's
/// subscription set. When a remote transport reconnect swaps
/// in a fresh `AppServerClient`, the server sees a brand-new
/// `ConnectionId` that isn't subscribed to anything; the old one was
/// already unregistered when its connection dropped. The mobile client's
/// `external_resume_thread` short-circuits via the `direct_resumed_threads`
/// marker set during the previous (now-dead) connection, so without
/// intervention the new connection never re-subscribes — and turn-stream
/// events go missing until the user manually navigates.
///
/// On a Disconnected→Connected transition we therefore:
///   1. Clear the direct-resume markers for this server (they're stale —
///      the live `ConnectionId` has changed).
///   2. Re-issue `external_resume_thread` for the active thread plus every
///      thread on this server that already had loaded turns. Each call
///      ends up routing through `thread/resume`, which calls
///      `try_add_connection_to_thread` server-side and replays any
///      in-flight requests for the new connection.
pub(super) fn run_post_reconnect_resubscribe(
    app_store: Arc<AppStoreReducer>,
    server_id: String,
    authoritative_refresh_required: bool,
) {
    MobileClient::spawn_detached(async move {
        let Some(client) = crate::ffi::shared::shared_mobile_client_if_initialized() else {
            return;
        };
        client.clear_direct_resume_markers_for_server(&server_id);

        if authoritative_refresh_required {
            let session = match client.get_session(&server_id) {
                Ok(session) => session,
                Err(error) => {
                    warn!(
                        "MobileClient: replay-drift reconcile missing session server_id={}: {}",
                        server_id, error
                    );
                    return;
                }
            };
            info!(
                "MobileClient: replay drift requires authoritative thread-list reconcile server_id={}",
                server_id
            );
            if let Err(error) =
                refresh_thread_list_from_app_server(session, Arc::clone(&app_store), &server_id)
                    .await
            {
                warn!(
                    "MobileClient: replay-drift thread-list reconcile failed server_id={}: {}",
                    server_id, error
                );
            }
        }

        let snapshot = app_store.snapshot();
        let mut keys_to_resume: Vec<ThreadKey> = Vec::new();
        if let Some(active) = snapshot.active_thread.as_ref()
            && active.server_id == server_id
        {
            keys_to_resume.push(active.clone());
        }
        for (key, thread) in snapshot.threads.iter() {
            if key.server_id != server_id {
                continue;
            }
            if keys_to_resume.iter().any(|k| k == key) {
                continue;
            }
            if !thread.items.is_empty() || thread.initial_turns_loaded {
                keys_to_resume.push(key.clone());
            }
        }

        if keys_to_resume.is_empty() {
            debug!(
                "MobileClient: post-reconnect resubscribe nothing to do server_id={}",
                server_id
            );
            return;
        }

        info!(
            "MobileClient: post-reconnect resubscribe server_id={} thread_count={}",
            server_id,
            keys_to_resume.len()
        );

        for key in keys_to_resume {
            let expected_generation = app_store.server_event_generation(&server_id);
            // Force-authoritative so the response carries the embedded
            // turn list. Without it `thread/resume` short-circuits via the
            // direct-resume marker (or returns an empty turn list under
            // `exclude_turns: true`), and `reconcile_active_turn` keeps
            // any stale `active_turn_id` whose turn has already completed
            // server-side.
            match client
                .force_refresh_thread_authoritative_if_ui_generation(
                    &key.server_id,
                    &key.thread_id,
                    expected_generation,
                )
                .await
            {
                Ok(true) => debug!(
                    "MobileClient: post-reconnect resubscribe ok server_id={} thread_id={}",
                    key.server_id, key.thread_id
                ),
                Ok(false) => debug!(
                    "MobileClient: post-reconnect resubscribe discarded stale response server_id={} thread_id={}",
                    key.server_id, key.thread_id
                ),
                Err(error) => warn!(
                    "MobileClient: post-reconnect resubscribe failed server_id={} thread_id={}: {}",
                    key.server_id, key.thread_id, error
                ),
            }
        }
    });
}
