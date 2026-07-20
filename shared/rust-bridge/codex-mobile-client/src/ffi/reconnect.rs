//! UniFFI-exported `ReconnectController` — shared reconnection orchestration
//! consumed by both iOS and Android.

use crate::ffi::shared::{shared_mobile_client, shared_runtime};
use crate::mobile_client::MobileClient;
use crate::next_request_id;
use crate::reconnect::{
    ReconnectOutcome, ReconnectPlan, ReconnectPlanDecision, ReconnectResult, SavedServerRecord,
    SlingshotCredentialProvider, SshCredentialProvider, decide_reconnect_plan_with_slingshot,
    execute_reconnect_plan, reconnect_outcome_for_health,
};
use crate::store::ServerHealthSnapshot;
use crate::store::snapshot::AppLifecyclePhaseSnapshot;
use codex_app_server_protocol as upstream;
use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex, RwLock};
use std::time::{Duration, Instant};
use tokio::runtime::Runtime;
use tokio::sync::{Notify, Semaphore, watch};
use tokio::task::JoinSet;
use tracing::{info, warn};

const MAX_CONCURRENT_COLD_RECONNECTS: usize = 3;
const RECONNECT_SHUTDOWN_DEADLINE: Duration = Duration::from_secs(5);

struct ReconnectCoordinator {
    state: StdMutex<ReconnectCoordinatorState>,
    cold_slots: Arc<Semaphore>,
    state_changed: Notify,
}

struct ReconnectCoordinatorState {
    accepting: bool,
    in_flight: HashMap<String, ActiveReconnect>,
}

struct ActiveReconnect {
    cancel_tx: watch::Sender<bool>,
    completion_tx: watch::Sender<Option<ReconnectResult>>,
}

enum BeginReconnect {
    Started(ReconnectAttemptGuard),
    Coalesced(watch::Receiver<Option<ReconnectResult>>),
    Stopped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ReconnectShutdownOutcome {
    Drained,
    Cancelled,
    TimedOut,
}

impl ReconnectCoordinator {
    fn new() -> Self {
        Self {
            state: StdMutex::new(ReconnectCoordinatorState {
                accepting: true,
                in_flight: HashMap::new(),
            }),
            cold_slots: Arc::new(Semaphore::new(MAX_CONCURRENT_COLD_RECONNECTS)),
            state_changed: Notify::new(),
        }
    }

    fn try_begin(self: &Arc<Self>, server_id: &str) -> BeginReconnect {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(error) => error.into_inner(),
        };
        if !state.accepting {
            return BeginReconnect::Stopped;
        }
        if let Some(active) = state.in_flight.get(server_id) {
            return BeginReconnect::Coalesced(active.completion_tx.subscribe());
        }
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let (completion_tx, _completion_rx) = watch::channel(None);
        state.in_flight.insert(
            server_id.to_string(),
            ActiveReconnect {
                cancel_tx,
                completion_tx: completion_tx.clone(),
            },
        );
        BeginReconnect::Started(ReconnectAttemptGuard {
            coordinator: Arc::clone(self),
            server_id: server_id.to_string(),
            cancel_rx,
            completion_tx,
            finished: false,
        })
    }

    fn is_empty(&self) -> bool {
        match self.state.lock() {
            Ok(state) => state.in_flight.is_empty(),
            Err(error) => error.into_inner().in_flight.is_empty(),
        }
    }

    async fn wait_for_empty(&self, deadline: Duration) -> bool {
        let started = Instant::now();
        loop {
            let changed = self.state_changed.notified();
            if self.is_empty() {
                return true;
            }
            let remaining = deadline.saturating_sub(started.elapsed());
            if remaining.is_zero() || tokio::time::timeout(remaining, changed).await.is_err() {
                return self.is_empty();
            }
        }
    }

    async fn shutdown(&self, deadline: Duration) -> ReconnectShutdownOutcome {
        {
            let mut state = match self.state.lock() {
                Ok(state) => state,
                Err(error) => error.into_inner(),
            };
            state.accepting = false;
        }
        self.state_changed.notify_waiters();

        let started = Instant::now();
        if self.wait_for_empty(deadline / 2).await {
            return ReconnectShutdownOutcome::Drained;
        }

        let cancelled = {
            let state = match self.state.lock() {
                Ok(state) => state,
                Err(error) => error.into_inner(),
            };
            for active in state.in_flight.values() {
                let _ = active.cancel_tx.send(true);
            }
            !state.in_flight.is_empty()
        };
        if !cancelled {
            return ReconnectShutdownOutcome::Drained;
        }

        let remaining = deadline.saturating_sub(started.elapsed());
        if self.wait_for_empty(remaining).await {
            ReconnectShutdownOutcome::Cancelled
        } else {
            ReconnectShutdownOutcome::TimedOut
        }
    }
}

struct ReconnectAttemptGuard {
    coordinator: Arc<ReconnectCoordinator>,
    server_id: String,
    cancel_rx: watch::Receiver<bool>,
    completion_tx: watch::Sender<Option<ReconnectResult>>,
    finished: bool,
}

impl ReconnectAttemptGuard {
    async fn cancelled(&mut self) {
        if *self.cancel_rx.borrow() {
            return;
        }
        while self.cancel_rx.changed().await.is_ok() {
            if *self.cancel_rx.borrow() {
                return;
            }
        }
    }

    fn finish(&mut self, result: ReconnectResult) -> ReconnectResult {
        self.finished = true;
        self.completion_tx.send_replace(Some(result.clone()));
        result
    }
}

impl Drop for ReconnectAttemptGuard {
    fn drop(&mut self) {
        if !self.finished {
            self.completion_tx.send_replace(Some(result_for_outcome(
                self.server_id.clone(),
                ReconnectOutcome::Cancelled,
            )));
        }
        match self.coordinator.state.lock() {
            Ok(mut state) => {
                state.in_flight.remove(&self.server_id);
            }
            Err(error) => {
                error.into_inner().in_flight.remove(&self.server_id);
            }
        }
        self.coordinator.state_changed.notify_waiters();
    }
}

fn normalized_local_display_name(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed == "This Device" {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn resolved_local_display_name(
    snapshot: &crate::store::AppSnapshot,
    saved_servers: &[SavedServerRecord],
    server_id: &str,
) -> String {
    snapshot
        .servers
        .get(server_id)
        .filter(|server| server.is_local)
        .and_then(|server| normalized_local_display_name(&server.display_name))
        .or_else(|| {
            snapshot
                .servers
                .values()
                .find(|server| server.is_local)
                .and_then(|server| normalized_local_display_name(&server.display_name))
        })
        .or_else(|| {
            saved_servers
                .iter()
                .find(|server| {
                    (server.id == server_id || server.id == "local")
                        && (server.source == "local" || server.id == "local")
                })
                .and_then(|server| normalized_local_display_name(&server.name))
        })
        .unwrap_or_else(|| "This Device".to_string())
}

fn live_reconnect_outcome(inner: &MobileClient, server_id: &str) -> Option<ReconnectOutcome> {
    let state = inner.connection_reconnect_state(server_id)?;
    reconnect_outcome_for_health(
        &state.aggregate,
        state.has_degraded_runtime,
        state.has_connecting_runtime,
    )
}

fn outcome_message(outcome: ReconnectOutcome) -> &'static str {
    match outcome {
        ReconnectOutcome::MissingSshCredential => "saved SSH credential is unavailable",
        ReconnectOutcome::MissingSlingshotCredential => "saved ChatGPT credential is unavailable",
        ReconnectOutcome::RePairRequired => {
            "saved pairing is no longer usable; pair the host again"
        }
        ReconnectOutcome::RepairRequired => "saved pairing is incomplete and requires repair",
        ReconnectOutcome::ConfigurationRequired => "no reconnect transport is configured",
        ReconnectOutcome::NotFound => "server not found in saved list or live snapshot",
        ReconnectOutcome::Cancelled => "reconnect was cancelled during shutdown",
        ReconnectOutcome::Failed => "reconnect failed",
        ReconnectOutcome::Connected
        | ReconnectOutcome::AlreadyConnected
        | ReconnectOutcome::SelfHealing
        | ReconnectOutcome::Coalesced => "",
    }
}

fn result_for_outcome(server_id: impl Into<String>, outcome: ReconnectOutcome) -> ReconnectResult {
    let server_id = server_id.into();
    match outcome {
        ReconnectOutcome::Connected
        | ReconnectOutcome::AlreadyConnected
        | ReconnectOutcome::SelfHealing
        | ReconnectOutcome::Coalesced => ReconnectResult::completed(server_id, outcome, false),
        _ => ReconnectResult::blocked(server_id, outcome, outcome_message(outcome)),
    }
}

async fn wait_for_coalesced_result(
    mut completion_rx: watch::Receiver<Option<ReconnectResult>>,
    server_id: String,
) -> ReconnectResult {
    loop {
        let current = { completion_rx.borrow_and_update().clone() };
        if let Some(result) = current {
            return result;
        }
        if completion_rx.changed().await.is_err() {
            return completion_rx
                .borrow()
                .clone()
                .unwrap_or_else(|| result_for_outcome(server_id, ReconnectOutcome::Cancelled));
        }
    }
}

async fn execute_coordinated_plan(
    plan: ReconnectPlan,
    inner: Arc<MobileClient>,
    coordinator: Arc<ReconnectCoordinator>,
) -> ReconnectResult {
    let server_id = plan.server_id().to_string();
    let mut attempt = match coordinator.try_begin(&server_id) {
        BeginReconnect::Started(attempt) => attempt,
        BeginReconnect::Coalesced(completion_rx) => {
            info!(
                server_id,
                "ReconnectController: reconnect attempt coalesced with in-flight owner"
            );
            return wait_for_coalesced_result(completion_rx, server_id).await;
        }
        BeginReconnect::Stopped => {
            return result_for_outcome(server_id, ReconnectOutcome::Cancelled);
        }
    };
    let acquire_slot = Arc::clone(&coordinator.cold_slots).acquire_owned();
    tokio::pin!(acquire_slot);
    let slot_result = tokio::select! {
        result = &mut acquire_slot => result.map_err(|error| {
            ReconnectResult::failed(
                server_id.clone(),
                format!("reconnect coordinator unavailable: {error}"),
            )
        }),
        () = attempt.cancelled() => Err(result_for_outcome(
            server_id.clone(),
            ReconnectOutcome::Cancelled,
        )),
    };
    let slot = match slot_result {
        Ok(slot) => slot,
        Err(result) => return attempt.finish(result),
    };
    let reconnect = execute_reconnect_plan(&plan, &inner);
    tokio::pin!(reconnect);
    let result = tokio::select! {
        result = &mut reconnect => result,
        () = attempt.cancelled() => {
            result_for_outcome(server_id, ReconnectOutcome::Cancelled)
        }
    };
    drop(slot);
    attempt.finish(result)
}

#[derive(uniffi::Object)]
pub struct ReconnectController {
    inner: Arc<MobileClient>,
    rt: Arc<Runtime>,
    saved_servers: Arc<RwLock<Vec<SavedServerRecord>>>,
    credential_provider: Arc<tokio::sync::Mutex<Option<Arc<dyn SshCredentialProvider>>>>,
    slingshot_credential_provider:
        Arc<tokio::sync::Mutex<Option<Arc<dyn SlingshotCredentialProvider>>>>,
    reconnect_coordinator: Arc<ReconnectCoordinator>,
}

#[uniffi::export(async_runtime = "tokio")]
impl ReconnectController {
    #[uniffi::constructor]
    pub fn new() -> Self {
        Self {
            inner: shared_mobile_client(),
            rt: shared_runtime(),
            saved_servers: Arc::new(RwLock::new(Vec::new())),
            credential_provider: Arc::new(tokio::sync::Mutex::new(None)),
            slingshot_credential_provider: Arc::new(tokio::sync::Mutex::new(None)),
            reconnect_coordinator: Arc::new(ReconnectCoordinator::new()),
        }
    }

    pub fn set_credential_provider(&self, provider: Box<dyn SshCredentialProvider>) {
        let provider: Arc<dyn SshCredentialProvider> = Arc::from(provider);
        // Try non-blocking first; if contended, spawn an async task.
        let fast = {
            let cp = Arc::clone(&self.credential_provider);
            cp.try_lock().ok().map(|mut g| {
                *g = Some(Arc::clone(&provider));
            })
        };
        if fast.is_none() {
            let cp = Arc::clone(&self.credential_provider);
            self.rt.spawn(async move {
                *cp.lock().await = Some(provider);
            });
        }
    }

    pub fn set_slingshot_credential_provider(
        &self,
        provider: Box<dyn SlingshotCredentialProvider>,
    ) {
        let provider: Arc<dyn SlingshotCredentialProvider> = Arc::from(provider);
        let fast = {
            let cp = Arc::clone(&self.slingshot_credential_provider);
            cp.try_lock().ok().map(|mut g| {
                *g = Some(Arc::clone(&provider));
            })
        };
        if fast.is_none() {
            let cp = Arc::clone(&self.slingshot_credential_provider);
            self.rt.spawn(async move {
                *cp.lock().await = Some(provider);
            });
        }
    }

    pub fn sync_saved_servers(&self, servers: Vec<SavedServerRecord>) {
        match self.saved_servers.write() {
            Ok(mut guard) => *guard = servers,
            Err(e) => *e.into_inner() = servers,
        }
    }

    pub async fn reconnect_saved_servers(&self) -> Vec<ReconnectResult> {
        let inner = Arc::clone(&self.inner);
        let saved_servers = Arc::clone(&self.saved_servers);
        let credential_provider = Arc::clone(&self.credential_provider);
        let slingshot_credential_provider = Arc::clone(&self.slingshot_credential_provider);
        let reconnect_coordinator = Arc::clone(&self.reconnect_coordinator);

        // Keep the full reconnect body off the foreign async executor stack.
        // iOS can poll UniFFI futures from a small cooperative thread stack,
        // and reconnect reaches the SSH/websocket handshake path.
        self.rt
            .spawn(async move {
                reconnect_saved_servers_inner(
                    inner,
                    saved_servers,
                    credential_provider,
                    slingshot_credential_provider,
                    reconnect_coordinator,
                )
                .await
            })
            .await
            .unwrap_or_else(|error| {
                warn!("ReconnectController: reconnect_saved_servers task failed: {error}");
                Vec::new()
            })
    }

    pub async fn reconnect_server(&self, server_id: String) -> ReconnectResult {
        let inner = Arc::clone(&self.inner);
        let saved_servers = Arc::clone(&self.saved_servers);
        let credential_provider = Arc::clone(&self.credential_provider);
        let slingshot_credential_provider = Arc::clone(&self.slingshot_credential_provider);
        let reconnect_coordinator = Arc::clone(&self.reconnect_coordinator);
        let server_id_for_error = server_id.clone();

        // Match the SSH bridge behavior and run reconnect on Tokio so the
        // websocket connect path does not execute on Swift's smaller stack.
        self.rt
            .spawn(async move {
                let result = reconnect_server_inner(
                    Arc::clone(&inner),
                    saved_servers,
                    credential_provider,
                    slingshot_credential_provider,
                    reconnect_coordinator,
                    server_id,
                )
                .await;
                result
            })
            .await
            .unwrap_or_else(|error| {
                ReconnectResult::failed(
                    server_id_for_error,
                    format!("reconnect task failed: {error}"),
                )
            })
    }

    pub async fn probe_active_remote_servers(&self) {
        let inner = Arc::clone(&self.inner);

        // Run the probe body on the shared tokio runtime: the probe awaits
        // session.request_client(...), which uses tokio primitives and would
        // panic ("no reactor running") when polled from the Swift/Kotlin
        // foreign async executor.
        let _ = self
            .rt
            .spawn(async move {
                let snapshot = inner.app_snapshot();
                let remote_connected: Vec<String> = snapshot
                    .servers
                    .values()
                    .filter(|s| !s.is_local && s.health == ServerHealthSnapshot::Connected)
                    .map(|s| s.server_id.clone())
                    .collect();

                for server_id in &remote_connected {
                    let request = upstream::ClientRequest::GetAccount {
                        request_id: upstream::RequestId::Integer(next_request_id()),
                        params: upstream::GetAccountParams {
                            refresh_token: false,
                        },
                    };
                    match inner
                        .request_typed_for_server::<upstream::GetAccountResponse>(
                            server_id, request,
                        )
                        .await
                    {
                        Ok(response) => {
                            inner.apply_account_response(server_id, &response);
                        }
                        Err(e) => {
                            warn!(
                                "ReconnectController: probe failed server_id={} error={}",
                                server_id, e
                            );
                        }
                    }
                }
            })
            .await
            .inspect_err(|error| {
                warn!("ReconnectController: probe_active_remote_servers task failed: {error}");
            });
    }

    pub async fn on_app_became_active(&self) -> Vec<ReconnectResult> {
        self.note_app_became_active();
        // Hint iroh-backed sessions that the host network may have changed
        // before we run the reconnect plan. This lets healthy Remora Link
        // sessions migrate paths/refresh relays without going through the
        // (heavier) full reconnect path; reconnect_saved_servers is still
        // run for transports that can't recover on their own.
        self.notify_network_change().await;
        let results = self.reconnect_saved_servers().await;
        self.probe_active_remote_servers().await;
        results
    }

    pub fn note_app_became_active(&self) {
        self.inner
            .app_store
            .note_app_lifecycle_phase(AppLifecyclePhaseSnapshot::Active);
    }

    /// Tell every session that the host network may have changed. iOS
    /// suspends app processes (which freezes UDP sockets and relay
    /// keepalives) and there's no in-process API to detect that — without
    /// this hint, iroh would only notice paths are dead via the QUIC idle
    /// timeout (10 min). Calling this on `appDidBecomeActive` lets iroh
    /// re-probe paths immediately. Cheap when nothing changed.
    pub async fn notify_network_change(&self) {
        let inner = Arc::clone(&self.inner);
        let _ = self
            .rt
            .spawn(async move {
                inner.notify_network_change().await;
            })
            .await
            .inspect_err(|error| {
                warn!("ReconnectController: notify_network_change task failed: {error}");
            });
    }

    pub fn on_app_became_inactive(&self) {
        self.inner
            .app_store
            .note_app_lifecycle_phase(AppLifecyclePhaseSnapshot::Inactive);
    }

    pub fn on_app_entered_background(&self) {
        self.inner
            .app_store
            .note_app_lifecycle_phase(AppLifecyclePhaseSnapshot::Background);
    }

    pub async fn on_network_reachable(&self) -> Vec<ReconnectResult> {
        self.notify_network_change().await;
        self.reconnect_saved_servers().await
    }

    /// Stop accepting reconnect work, let active attempts drain, then cancel
    /// and join stragglers within a fixed deadline. A controller is terminal
    /// after shutdown and should be replaced before accepting new work.
    pub async fn shutdown_reconnects(&self) -> ReconnectShutdownOutcome {
        let coordinator = Arc::clone(&self.reconnect_coordinator);
        self.rt
            .spawn(async move { coordinator.shutdown(RECONNECT_SHUTDOWN_DEADLINE).await })
            .await
            .unwrap_or(ReconnectShutdownOutcome::TimedOut)
    }
}

async fn reconnect_saved_servers_inner(
    inner: Arc<MobileClient>,
    saved_servers: Arc<RwLock<Vec<SavedServerRecord>>>,
    credential_provider: Arc<tokio::sync::Mutex<Option<Arc<dyn SshCredentialProvider>>>>,
    slingshot_credential_provider: Arc<
        tokio::sync::Mutex<Option<Arc<dyn SlingshotCredentialProvider>>>,
    >,
    reconnect_coordinator: Arc<ReconnectCoordinator>,
) -> Vec<ReconnectResult> {
    let servers = match saved_servers.read() {
        Ok(s) => s.clone(),
        Err(e) => e.into_inner().clone(),
    };

    let snapshot = inner.app_snapshot();
    let local_display_name = resolved_local_display_name(&snapshot, &servers, "local");

    let credential_provider = credential_provider.lock().await.clone();
    let slingshot_credential_provider = slingshot_credential_provider.lock().await.clone();
    let slingshot_credential = slingshot_credential_provider
        .as_ref()
        .and_then(|provider| provider.load_credential());

    let mut results = Vec::new();
    let local_plan = ReconnectPlan::Local {
        server_id: "local".to_string(),
        display_name: local_display_name,
    };
    results.push(
        execute_coordinated_plan(
            local_plan,
            Arc::clone(&inner),
            Arc::clone(&reconnect_coordinator),
        )
        .await,
    );

    let mut plans = Vec::new();
    for server in &servers {
        if !server.remembered_by_user || server.source == "local" {
            continue;
        }

        if let Some(outcome) = live_reconnect_outcome(&inner, &server.id) {
            results.push(result_for_outcome(server.id.clone(), outcome));
            continue;
        }

        let credential = credential_provider.as_ref().and_then(|provider| {
            let ssh_port = crate::reconnect::resolved_ssh_port(server);
            provider.load_credential(server.hostname.clone(), ssh_port)
        });
        match decide_reconnect_plan_with_slingshot(
            server,
            credential.as_ref(),
            slingshot_credential.as_ref(),
            false,
        ) {
            ReconnectPlanDecision::Plan(plan) => plans.push(plan),
            ReconnectPlanDecision::NoAction(outcome) => {
                results.push(result_for_outcome(server.id.clone(), outcome));
            }
        }
    }

    let mut join_set = JoinSet::new();
    for plan in plans {
        let client = Arc::clone(&inner);
        let coordinator = Arc::clone(&reconnect_coordinator);
        join_set.spawn(async move { execute_coordinated_plan(plan, client, coordinator).await });
    }

    while let Some(result) = join_set.join_next().await {
        match result {
            Ok(r) => results.push(r),
            Err(e) => warn!("ReconnectController: join error: {}", e),
        }
    }
    results
}

async fn reconnect_server_inner(
    inner: Arc<MobileClient>,
    saved_servers: Arc<RwLock<Vec<SavedServerRecord>>>,
    credential_provider: Arc<tokio::sync::Mutex<Option<Arc<dyn SshCredentialProvider>>>>,
    slingshot_credential_provider: Arc<
        tokio::sync::Mutex<Option<Arc<dyn SlingshotCredentialProvider>>>,
    >,
    reconnect_coordinator: Arc<ReconnectCoordinator>,
    server_id: String,
) -> ReconnectResult {
    let snapshot = inner.app_snapshot();
    let saved_server = {
        let servers = match saved_servers.read() {
            Ok(s) => s,
            Err(e) => e.into_inner(),
        };
        servers.iter().find(|s| s.id == server_id).cloned()
    };

    let is_local = snapshot.servers.get(&server_id).is_some_and(|s| s.is_local)
        || server_id == "local"
        || saved_server
            .as_ref()
            .is_some_and(|server| server.source == "local");

    if is_local {
        let plan = ReconnectPlan::Local {
            server_id: server_id.clone(),
            display_name: resolved_local_display_name(
                &snapshot,
                saved_server.as_ref().map_or(&[], std::slice::from_ref),
                &server_id,
            ),
        };
        return execute_coordinated_plan(plan, inner, reconnect_coordinator).await;
    }

    if let Some(outcome) = live_reconnect_outcome(&inner, &server_id) {
        return result_for_outcome(server_id, outcome);
    }

    if let Some(server) = saved_server {
        let credential_provider = credential_provider.lock().await.clone();
        let slingshot_credential_provider = slingshot_credential_provider.lock().await.clone();
        let credential = credential_provider.as_ref().and_then(|p| {
            let ssh_port = crate::reconnect::resolved_ssh_port(&server);
            p.load_credential(server.hostname.clone(), ssh_port)
        });
        let slingshot_credential = slingshot_credential_provider
            .as_ref()
            .and_then(|provider| provider.load_credential());

        return match decide_reconnect_plan_with_slingshot(
            &server,
            credential.as_ref(),
            slingshot_credential.as_ref(),
            false,
        ) {
            ReconnectPlanDecision::Plan(plan) => {
                execute_coordinated_plan(plan, inner, reconnect_coordinator).await
            }
            ReconnectPlanDecision::NoAction(outcome) => result_for_outcome(server_id, outcome),
        };
    }

    if let Some(snap_server) = snapshot.servers.get(&server_id) {
        let plan = ReconnectPlan::DirectRemote {
            server_id: snap_server.server_id.clone(),
            display_name: snap_server.display_name.clone(),
            host: snap_server.host.clone(),
            port: snap_server.port,
        };
        return execute_coordinated_plan(plan, inner, reconnect_coordinator).await;
    }

    result_for_outcome(server_id, ReconnectOutcome::NotFound)
}

#[cfg(test)]
mod tests {
    use super::{
        BeginReconnect, ReconnectCoordinator, ReconnectShutdownOutcome,
        resolved_local_display_name, wait_for_coalesced_result,
    };
    use crate::reconnect::{ReconnectOutcome, ReconnectResult, SavedServerRecord};
    use crate::store::snapshot::{
        AppSnapshot, AppVoiceSessionSnapshot, ServerHealthSnapshot, ServerSnapshot,
        ServerTransportDiagnostics,
    };
    use std::collections::HashMap;

    fn empty_snapshot() -> AppSnapshot {
        AppSnapshot {
            servers: HashMap::new(),
            threads: HashMap::new(),
            active_thread: None,
            pending_approvals: Vec::new(),
            pending_approval_seeds: HashMap::new(),
            pending_user_inputs: Vec::new(),
            pending_user_input_seeds: HashMap::new(),
            voice_session: AppVoiceSessionSnapshot::default(),
            terminal_sessions: Vec::new(),
            active_terminal_id: None,
        }
    }

    #[test]
    fn reconnect_coordinator_coalesces_per_server_only() {
        let coordinator = std::sync::Arc::new(ReconnectCoordinator::new());
        let BeginReconnect::Started(first) = coordinator.try_begin("srv-a") else {
            panic!("expected first owner");
        };
        assert!(matches!(
            coordinator.try_begin("srv-a"),
            BeginReconnect::Coalesced(_)
        ));
        let BeginReconnect::Started(other) = coordinator.try_begin("srv-b") else {
            panic!("expected independent owner");
        };
        assert_eq!(coordinator.cold_slots.available_permits(), 3);

        drop(first);
        assert!(matches!(
            coordinator.try_begin("srv-a"),
            BeginReconnect::Started(_)
        ));
        drop(other);
    }

    #[tokio::test]
    async fn reconnect_coordinator_shares_owner_failure_with_coalesced_waiter() {
        let coordinator = std::sync::Arc::new(ReconnectCoordinator::new());
        let BeginReconnect::Started(mut owner) = coordinator.try_begin("srv-a") else {
            panic!("expected first owner");
        };
        let BeginReconnect::Coalesced(completion_rx) = coordinator.try_begin("srv-a") else {
            panic!("expected coalesced waiter");
        };

        let owner_result = ReconnectResult::failed("srv-a", "dial exhausted");
        owner.finish(owner_result);
        drop(owner);

        let shared = wait_for_coalesced_result(completion_rx, "srv-a".to_string()).await;
        assert!(!shared.success);
        assert_eq!(shared.outcome, ReconnectOutcome::Failed);
        assert_eq!(shared.error_message.as_deref(), Some("dial exhausted"));
    }

    #[tokio::test]
    async fn reconnect_coordinator_reports_cancelled_when_owner_drops_unfinished() {
        let coordinator = std::sync::Arc::new(ReconnectCoordinator::new());
        let BeginReconnect::Started(owner) = coordinator.try_begin("srv-a") else {
            panic!("expected first owner");
        };
        let BeginReconnect::Coalesced(completion_rx) = coordinator.try_begin("srv-a") else {
            panic!("expected coalesced waiter");
        };

        drop(owner);

        let shared = wait_for_coalesced_result(completion_rx, "srv-a".to_string()).await;
        assert!(!shared.success);
        assert_eq!(shared.outcome, ReconnectOutcome::Cancelled);
    }

    #[tokio::test]
    async fn reconnect_coordinator_shutdown_stops_intake_and_drains() {
        let coordinator = std::sync::Arc::new(ReconnectCoordinator::new());
        let BeginReconnect::Started(attempt) = coordinator.try_begin("srv-a") else {
            panic!("expected first owner");
        };
        let shutdown_coordinator = std::sync::Arc::clone(&coordinator);
        let shutdown = tokio::spawn(async move {
            shutdown_coordinator
                .shutdown(std::time::Duration::from_secs(1))
                .await
        });
        tokio::task::yield_now().await;
        assert!(matches!(
            coordinator.try_begin("srv-b"),
            BeginReconnect::Stopped
        ));

        drop(attempt);
        assert_eq!(
            shutdown.await.expect("shutdown task"),
            ReconnectShutdownOutcome::Drained
        );
    }

    #[tokio::test]
    async fn reconnect_coordinator_cancels_and_joins_stragglers() {
        let coordinator = std::sync::Arc::new(ReconnectCoordinator::new());
        let BeginReconnect::Started(mut attempt) = coordinator.try_begin("srv-a") else {
            panic!("expected first owner");
        };
        let attempt_task = tokio::spawn(async move {
            attempt.cancelled().await;
            drop(attempt);
        });

        assert_eq!(
            coordinator
                .shutdown(std::time::Duration::from_millis(100))
                .await,
            ReconnectShutdownOutcome::Cancelled
        );
        attempt_task.await.expect("cancelled attempt joined");
        assert!(matches!(
            coordinator.try_begin("srv-b"),
            BeginReconnect::Stopped
        ));
    }

    #[test]
    fn local_display_name_prefers_snapshot_name() {
        let mut snapshot = empty_snapshot();
        snapshot.servers.insert(
            "local".to_string(),
            ServerSnapshot {
                server_id: "local".to_string(),
                display_name: "Desk Mac".to_string(),
                host: "127.0.0.1".to_string(),
                port: 0,
                wake_mac: None,
                is_local: true,
                health: ServerHealthSnapshot::Disconnected,
                account: None,
                requires_openai_auth: false,
                rate_limits: None,
                rate_limits_by_runtime: std::collections::HashMap::new(),
                available_models: None,
                agent_runtimes: Vec::new(),
                connection_progress: None,
                transport: ServerTransportDiagnostics::default(),
                codex_version: None,
                supports_turn_pagination: true,
            },
        );

        assert_eq!(
            resolved_local_display_name(&snapshot, &[], "local"),
            "Desk Mac"
        );
    }

    #[test]
    fn local_display_name_falls_back_to_saved_server_name() {
        let saved = SavedServerRecord {
            id: "local".to_string(),
            name: "Laptop".to_string(),
            hostname: "127.0.0.1".to_string(),
            port: 0,
            codex_ports: Vec::new(),
            ssh_port: None,
            source: "local".to_string(),
            has_codex_server: false,
            wake_mac: None,
            preferred_connection_mode: None,
            preferred_codex_port: None,
            ssh_port_forwarding_enabled: None,
            websocket_url: None,
            remembered_by_user: true,
            ssh_bridge_runtime_kinds: None,
        };

        assert_eq!(
            resolved_local_display_name(&empty_snapshot(), &[saved], "local"),
            "Laptop"
        );
    }

    #[test]
    fn local_display_name_ignores_legacy_placeholder() {
        let mut snapshot = empty_snapshot();
        snapshot.servers.insert(
            "local".to_string(),
            ServerSnapshot {
                server_id: "local".to_string(),
                display_name: "This Device".to_string(),
                host: "127.0.0.1".to_string(),
                port: 0,
                wake_mac: None,
                is_local: true,
                health: ServerHealthSnapshot::Disconnected,
                account: None,
                requires_openai_auth: false,
                rate_limits: None,
                rate_limits_by_runtime: std::collections::HashMap::new(),
                available_models: None,
                agent_runtimes: Vec::new(),
                connection_progress: None,
                transport: ServerTransportDiagnostics::default(),
                codex_version: None,
                supports_turn_pagination: true,
            },
        );

        let saved = SavedServerRecord {
            id: "local".to_string(),
            name: "Desk Mac".to_string(),
            hostname: "127.0.0.1".to_string(),
            port: 0,
            codex_ports: Vec::new(),
            ssh_port: None,
            source: "local".to_string(),
            has_codex_server: false,
            wake_mac: None,
            preferred_connection_mode: None,
            preferred_codex_port: None,
            ssh_port_forwarding_enabled: None,
            websocket_url: None,
            remembered_by_user: true,
            ssh_bridge_runtime_kinds: None,
        };

        assert_eq!(
            resolved_local_display_name(&snapshot, &[saved], "local"),
            "Desk Mac"
        );
    }
}
