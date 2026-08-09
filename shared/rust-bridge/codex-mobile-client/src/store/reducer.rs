use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hasher};
use std::sync::RwLock;

use codex_app_server_protocol as upstream;
use tokio::sync::broadcast;

use crate::conversation::{
    command_output_is_truncated, make_error_item, make_model_rerouted_item, make_turn_diff_item,
    truncate_command_output_text,
};
use crate::conversation_uniffi::{
    HydratedAssistantMessageData, HydratedCommandExecutionData, HydratedConversationItem,
    HydratedConversationItemContent, HydratedMcpToolCallData, HydratedProposedPlanData,
    HydratedReasoningData,
};
#[cfg(test)]
use crate::conversation_uniffi::{
    HydratedUserInputResponseData, HydratedUserInputResponseQuestionData,
};
use crate::session::connection::ServerConfig;
use crate::session::events::UiEvent;
#[cfg(test)]
use crate::types::PendingApprovalWithSeed;
use crate::types::{
    AgentRuntimeInfo, AgentRuntimeKind, PendingApproval, PendingApprovalKey, PendingApprovalSeed,
    PendingUserInputAnswer, PendingUserInputKey, PendingUserInputRequest, PendingUserInputSeed,
    ThreadInfo, ThreadKey, ThreadSummaryStatus,
};
use crate::types::{
    AppModeKind, AppOperationStatus, AppPlanProgressSnapshot, AppPlanStep, AppThreadGoal,
};

use super::actions::{
    conversation_item_from_upstream_with_turn, thread_info_from_upstream,
    thread_info_from_upstream_status_change,
};
use super::boundary::{
    AppSessionSummary, app_session_summary, current_agent_directory_version, empty_session_summary,
    project_hydrated_item, project_thread_state_update, project_thread_update,
};
use super::snapshot::{
    AppConnectionProgressSnapshot, AppLifecyclePhaseSnapshot, AppQueuedFollowUpPreview,
    AppSnapshot, AppVoiceSessionSnapshot, PendingServerMutatingCommand, QueuedFollowUpDraft,
    ServerHealthSnapshot, ServerMutatingCommandKind, ServerSnapshot, ServerTransportDiagnostics,
    ThreadSnapshot,
};
use super::updates::{AppStoreUpdateRecord, ThreadStreamingDeltaKind};
use super::voice::VoiceRealtimeState;

/// Compute a 64-bit fingerprint of a projected `HydratedConversationItem`
/// suitable for redundant-emit dedup in `emit_thread_item_changed`. Streams
/// the item's serde representation directly into the hasher so we never
/// retain a full clone of the item alongside the canonical store copy.
///
/// A u64 collision would only cost us a single skipped `ThreadItemChanged`
/// emit (followed immediately by another differing fingerprint on the next
/// delta), which is acceptable at our item counts.
fn item_fingerprint(item: &HydratedConversationItem) -> u64 {
    struct HashWriter<'a>(&'a mut DefaultHasher);
    impl std::io::Write for HashWriter<'_> {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.write(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut hasher = DefaultHasher::new();
    serde_json::to_writer(HashWriter(&mut hasher), item)
        .expect("HydratedConversationItem Serialize impl is infallible");
    hasher.finish()
}

fn dedupe_agent_runtimes(runtimes: Vec<AgentRuntimeInfo>) -> Vec<AgentRuntimeInfo> {
    let mut indexes_by_kind: HashMap<AgentRuntimeKind, usize> = HashMap::new();
    let mut deduped: Vec<AgentRuntimeInfo> = Vec::new();
    for runtime in runtimes {
        if let Some(index) = indexes_by_kind.get(&runtime.kind).copied() {
            if runtime.available && !deduped[index].available {
                deduped[index] = runtime;
            }
        } else {
            indexes_by_kind.insert(runtime.kind.clone(), deduped.len());
            deduped.push(runtime);
        }
    }
    deduped
}

pub struct AppStoreReducer {
    snapshot: RwLock<AppSnapshot>,
    last_thread_state_updates: RwLock<
        HashMap<
            ThreadKey,
            (
                crate::store::boundary::AppThreadStateRecord,
                crate::store::boundary::AppSessionSummary,
                u64,
            ),
        >,
    >,
    /// Per-(thread, item_id) fingerprint of the last emitted projected item,
    /// used to skip redundant `ThreadItemChanged` emits. Storing a u64 hash
    /// instead of the item itself avoids a long-lived second copy of every
    /// item in memory — on a 5k-item streaming thread that doubled the
    /// canonical `ThreadSnapshot.items` heap footprint.
    last_thread_item_upserts: RwLock<HashMap<(ThreadKey, String), u64>>,
    /// Per-call-id running buffer of streaming `dynamic_tool_call` argument
    /// JSON. Keyed by `(thread_key, call_id)`. Entries are cleared when the
    /// call completes or fails (via `ItemCompleted` on the matching
    /// item). The partial buffer is parsed tolerantly so platforms can
    /// render widgets as the model streams the HTML body.
    dynamic_tool_arg_buffers: RwLock<HashMap<(ThreadKey, String), DynamicToolCallArgBuffer>>,
    updates_tx: broadcast::Sender<AppStoreUpdateRecord>,
    voice_state: VoiceRealtimeState,
}

/// State we carry per (thread, call_id) while streaming argument deltas.
/// Holds the concatenated JSON so far plus the item_id the call was
/// announced with — we need it to clear on `ItemCompleted`, and to fill
/// the `item_id` field of the broadcast even when the server later omits
/// it on a particular delta.
#[derive(Debug, Clone)]
pub(crate) struct DynamicToolCallArgBuffer {
    pub(crate) item_id: String,
    pub(crate) buffer: String,
}

enum ItemMutationUpdate {
    Upsert(HydratedConversationItem),
}

impl AppStoreReducer {
    pub fn new() -> Self {
        // Streaming turns can burst small deltas quickly; keep enough headroom so
        // native subscribers do not immediately fall into lagged/full-resync mode.
        let (updates_tx, _) = broadcast::channel(1024);
        Self {
            snapshot: RwLock::new(AppSnapshot::default()),
            last_thread_state_updates: RwLock::new(HashMap::new()),
            last_thread_item_upserts: RwLock::new(HashMap::new()),
            dynamic_tool_arg_buffers: RwLock::new(HashMap::new()),
            updates_tx,
            voice_state: VoiceRealtimeState::default(),
        }
    }

    pub fn snapshot(&self) -> AppSnapshot {
        self.snapshot
            .read()
            .expect("app store lock poisoned")
            .clone()
    }

    pub(crate) fn thread_snapshot(&self, key: &ThreadKey) -> Option<ThreadSnapshot> {
        self.snapshot
            .read()
            .expect("app store lock poisoned")
            .threads
            .get(key)
            .cloned()
    }

    /// Project a small read-only value from one canonical thread without
    /// cloning the complete conversation snapshot. Callers must keep the
    /// closure bounded because it executes while the store read lock is held.
    pub(crate) fn project_thread<R>(
        &self,
        key: &ThreadKey,
        project: impl FnOnce(&ThreadSnapshot) -> R,
    ) -> Option<R> {
        let snapshot = self.snapshot.read().expect("app store lock poisoned");
        snapshot.threads.get(key).map(project)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AppStoreUpdateRecord> {
        self.updates_tx.subscribe()
    }

    pub fn upsert_server(&self, config: &ServerConfig, health: ServerHealthSnapshot) {
        {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            let (
                existing_wake_mac,
                existing_account,
                requires_openai_auth,
                existing_rate_limits,
                existing_rate_limits_by_runtime,
                existing_available_models,
                existing_agent_runtimes,
                existing_connection_progress,
                existing_transport,
                existing_codex_version,
                existing_supports_turn_pagination,
            ) = if let Some(existing) = snapshot.servers.get(&config.server_id) {
                (
                    existing.wake_mac.clone(),
                    existing.account.clone(),
                    existing.requires_openai_auth,
                    existing.rate_limits.clone(),
                    existing.rate_limits_by_runtime.clone(),
                    existing.available_models.clone(),
                    existing.agent_runtimes.clone(),
                    existing.connection_progress.clone(),
                    existing.transport.clone(),
                    existing.codex_version.clone(),
                    existing.supports_turn_pagination,
                )
            } else {
                (
                    None,
                    None,
                    false,
                    None,
                    HashMap::new(),
                    None,
                    vec![AgentRuntimeInfo {
                        kind: "codex".to_string(),
                        name: "codex".to_string(),
                        display_name: "Codex".to_string(),
                        available: true,
                    }],
                    None,
                    ServerTransportDiagnostics::default(),
                    None,
                    true,
                )
            };
            snapshot.servers.insert(
                config.server_id.clone(),
                ServerSnapshot {
                    server_id: config.server_id.clone(),
                    display_name: config.display_name.clone(),
                    host: config.host.clone(),
                    port: config.port,
                    wake_mac: existing_wake_mac,
                    is_local: config.is_local,
                    health,
                    account: existing_account,
                    requires_openai_auth,
                    rate_limits: existing_rate_limits,
                    rate_limits_by_runtime: existing_rate_limits_by_runtime,
                    available_models: existing_available_models,
                    agent_runtimes: existing_agent_runtimes,
                    connection_progress: existing_connection_progress,
                    transport: existing_transport,
                    codex_version: existing_codex_version,
                    supports_turn_pagination: existing_supports_turn_pagination,
                },
            );
        }
        self.emit(AppStoreUpdateRecord::ServerChanged {
            server_id: config.server_id.clone(),
        });
    }

    pub fn remove_server(&self, server_id: &str) {
        let mut removed_thread_keys = Vec::new();
        let agent_directory_version;
        {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            snapshot.servers.remove(server_id);
            snapshot.threads.retain(|key, _| {
                let keep = key.server_id != server_id;
                if !keep {
                    removed_thread_keys.push(key.clone());
                }
                keep
            });
            if snapshot
                .active_thread
                .as_ref()
                .is_some_and(|key| key.server_id == server_id)
            {
                snapshot.active_thread = None;
            }
            snapshot.pending_approvals.retain(|approval| {
                approval
                    .thread_id
                    .as_deref()
                    .is_none_or(|tid| !removed_thread_keys.iter().any(|key| key.thread_id == tid))
            });
            snapshot
                .pending_approval_seeds
                .retain(|key, _| key.server_id != server_id);
            snapshot
                .pending_user_inputs
                .retain(|request| request.server_id != server_id);
            snapshot
                .pending_user_input_seeds
                .retain(|key, _| key.server_id != server_id);
            if snapshot
                .voice_session
                .active_thread
                .as_ref()
                .is_some_and(|key| key.server_id == server_id)
            {
                snapshot.voice_session = AppVoiceSessionSnapshot::default();
            }
            agent_directory_version = current_agent_directory_version(&snapshot);
        }
        self.emit(AppStoreUpdateRecord::ServerRemoved {
            server_id: server_id.to_string(),
        });
        for key in removed_thread_keys {
            self.clear_removed_thread_caches(&key);
            self.emit(AppStoreUpdateRecord::ThreadRemoved {
                key,
                agent_directory_version,
            });
        }
        self.emit(AppStoreUpdateRecord::ActiveThreadChanged { key: None });
    }

    pub fn sync_thread_list(&self, server_id: &str, threads: &[ThreadInfo]) {
        self.sync_thread_list_for_runtime(server_id, "codex".to_string(), threads);
    }

    pub fn sync_thread_list_for_runtime(
        &self,
        server_id: &str,
        runtime_kind: AgentRuntimeKind,
        threads: &[ThreadInfo],
    ) {
        let incoming_ids = threads
            .iter()
            .map(|info| info.id.clone())
            .collect::<HashSet<_>>();
        let mut upserted_thread_keys = Vec::new();
        let mut updated_thread_keys = Vec::new();
        let mut removed_thread_keys = Vec::new();
        let mut active_thread_cleared = false;
        let mut pending_approvals = None;
        let mut pending_user_inputs = None;
        let agent_directory_version;
        {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            let active_thread_key = snapshot.active_thread.clone();
            snapshot.threads.retain(|key, _| {
                let keep = key.server_id != server_id
                    || incoming_ids.contains(&key.thread_id)
                    || active_thread_key.as_ref() == Some(key);
                if !keep {
                    removed_thread_keys.push(key.clone());
                }
                keep
            });
            for info in threads {
                let key = ThreadKey {
                    server_id: server_id.to_string(),
                    thread_id: info.id.clone(),
                };
                if let Some(entry) = snapshot.threads.get_mut(&key) {
                    let mut next_info = info.clone();
                    preserve_thread_title(&entry.info, &mut next_info);
                    preserve_thread_preview(&entry.info, &mut next_info);
                    preserve_thread_created_at(&entry.info, &mut next_info);
                    // thread/list carries no turn data, so it cannot authoritatively
                    // close an in-flight turn. Only TurnCompleted (or a rebuild that
                    // includes the turn list) can downgrade Active → Idle.
                    if matches!(next_info.status, ThreadSummaryStatus::Idle)
                        && entry.active_turn_id.is_some()
                    {
                        next_info.status = entry.info.status.clone();
                    }
                    let next_model = next_info.model.clone().or_else(|| entry.model.clone());
                    let info_changed = entry.info != next_info;
                    let model_changed = entry.model != next_model;
                    if info_changed || model_changed {
                        entry.info = next_info;
                        entry.model = next_model;
                        updated_thread_keys.push(key.clone());
                    }
                    if entry.agent_runtime_kind != runtime_kind {
                        entry.agent_runtime_kind = runtime_kind.clone();
                        updated_thread_keys.push(key);
                    }
                } else {
                    let mut thread = ThreadSnapshot::from_info(server_id, info.clone());
                    thread.agent_runtime_kind = runtime_kind.clone();
                    snapshot.threads.insert(key.clone(), thread);
                    upserted_thread_keys.push(key);
                }
            }
            if snapshot.active_thread.as_ref().is_some_and(|key| {
                key.server_id == server_id && !incoming_ids.contains(&key.thread_id)
            }) {
                let should_clear = snapshot
                    .active_thread
                    .as_ref()
                    .is_some_and(|key| !snapshot.threads.contains_key(key));
                if should_clear {
                    snapshot.active_thread = None;
                    active_thread_cleared = true;
                }
            }
            let approvals_before = snapshot.pending_approvals.len();
            snapshot.pending_approvals.retain(|approval| {
                approval.thread_id.as_deref().is_none_or(|tid| {
                    !removed_thread_keys
                        .iter()
                        .any(|key| key.thread_id.as_str() == tid)
                })
            });
            let remaining_approval_keys = snapshot
                .pending_approvals
                .iter()
                .map(|approval| PendingApprovalKey {
                    server_id: approval.server_id.clone(),
                    request_id: approval.id.clone(),
                })
                .collect::<HashSet<_>>();
            snapshot
                .pending_approval_seeds
                .retain(|key, _| remaining_approval_keys.contains(key));
            if snapshot.pending_approvals.len() != approvals_before {
                pending_approvals = Some(snapshot.pending_approvals.clone());
            }
            let pending_user_inputs_before = snapshot.pending_user_inputs.len();
            snapshot.pending_user_inputs.retain(|request| {
                !(request.server_id == server_id
                    && removed_thread_keys
                        .iter()
                        .any(|key| key.thread_id == request.thread_id))
            });
            if snapshot.pending_user_inputs.len() != pending_user_inputs_before {
                pending_user_inputs = Some(snapshot.pending_user_inputs.clone());
            }
            let remaining_user_input_keys = snapshot
                .pending_user_inputs
                .iter()
                .map(|request| PendingUserInputKey {
                    server_id: request.server_id.clone(),
                    request_id: request.id.clone(),
                })
                .collect::<HashSet<_>>();
            snapshot
                .pending_user_input_seeds
                .retain(|key, _| remaining_user_input_keys.contains(key));
            // See `finalize_thread_list_sync`: we do NOT tear down an
            // active voice session just because the thread/list page
            // happens to omit the voice thread. The thread may simply
            // be too new, or the page may be filtered. Let
            // RealtimeStarted/RealtimeClosed drive voice_session state.
            agent_directory_version = current_agent_directory_version(&snapshot);
        }
        for key in removed_thread_keys {
            self.clear_removed_thread_caches(&key);
            self.emit(AppStoreUpdateRecord::ThreadRemoved {
                key,
                agent_directory_version,
            });
        }
        for key in upserted_thread_keys {
            self.emit_thread_upsert(&key);
        }
        for key in updated_thread_keys {
            self.emit_thread_metadata_changed(&key);
        }
        if let Some(approvals) = pending_approvals {
            self.emit(AppStoreUpdateRecord::PendingApprovalsChanged { approvals });
        }
        if let Some(requests) = pending_user_inputs {
            self.emit(AppStoreUpdateRecord::PendingUserInputsChanged { requests });
        }
        if active_thread_cleared {
            self.emit(AppStoreUpdateRecord::ActiveThreadChanged { key: None });
        }
    }

    pub fn upsert_thread_list_page(&self, server_id: &str, threads: &[ThreadInfo]) {
        self.upsert_thread_list_page_for_runtime(server_id, "codex".to_string(), threads);
    }

    pub fn upsert_thread_list_page_for_runtime(
        &self,
        server_id: &str,
        runtime_kind: AgentRuntimeKind,
        threads: &[ThreadInfo],
    ) {
        for info in threads {
            let mut snapshot = ThreadSnapshot::from_info(server_id, info.clone());
            snapshot.agent_runtime_kind = runtime_kind.clone();
            self.upsert_thread_snapshot(snapshot);
        }
    }

    pub fn finalize_thread_list_sync(&self, server_id: &str, incoming_ids: &HashSet<String>) {
        let mut removed_thread_keys = Vec::new();
        let mut active_thread_cleared = false;
        let mut pending_approvals = None;
        let mut pending_user_inputs = None;
        let agent_directory_version;
        {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            let active_thread_key = snapshot.active_thread.clone();
            snapshot.threads.retain(|key, _| {
                let keep = key.server_id != server_id
                    || incoming_ids.contains(&key.thread_id)
                    || active_thread_key.as_ref() == Some(key);
                if !keep {
                    removed_thread_keys.push(key.clone());
                }
                keep
            });
            if snapshot.active_thread.as_ref().is_some_and(|key| {
                key.server_id == server_id && !incoming_ids.contains(&key.thread_id)
            }) {
                let should_clear = snapshot
                    .active_thread
                    .as_ref()
                    .is_some_and(|key| !snapshot.threads.contains_key(key));
                if should_clear {
                    snapshot.active_thread = None;
                    active_thread_cleared = true;
                }
            }
            let approvals_before = snapshot.pending_approvals.len();
            snapshot.pending_approvals.retain(|approval| {
                approval.thread_id.as_deref().is_none_or(|tid| {
                    !removed_thread_keys
                        .iter()
                        .any(|key| key.thread_id.as_str() == tid)
                })
            });
            let remaining_approval_keys = snapshot
                .pending_approvals
                .iter()
                .map(|approval| PendingApprovalKey {
                    server_id: approval.server_id.clone(),
                    request_id: approval.id.clone(),
                })
                .collect::<HashSet<_>>();
            snapshot
                .pending_approval_seeds
                .retain(|key, _| remaining_approval_keys.contains(key));
            if snapshot.pending_approvals.len() != approvals_before {
                pending_approvals = Some(snapshot.pending_approvals.clone());
            }
            let pending_user_inputs_before = snapshot.pending_user_inputs.len();
            snapshot.pending_user_inputs.retain(|request| {
                !(request.server_id == server_id
                    && removed_thread_keys
                        .iter()
                        .any(|key| key.thread_id == request.thread_id))
            });
            if snapshot.pending_user_inputs.len() != pending_user_inputs_before {
                pending_user_inputs = Some(snapshot.pending_user_inputs.clone());
            }
            let remaining_user_input_keys = snapshot
                .pending_user_inputs
                .iter()
                .map(|request| PendingUserInputKey {
                    server_id: request.server_id.clone(),
                    request_id: request.id.clone(),
                })
                .collect::<HashSet<_>>();
            snapshot
                .pending_user_input_seeds
                .retain(|key, _| remaining_user_input_keys.contains(key));
            // Intentionally do NOT clear voice_session here based on
            // list-sync output. A list_threads RPC may omit a brand-new
            // voice thread (e.g. the one realtime voice is running on
            // right now was created seconds ago and isn't materialized
            // in the listing yet). When the assistant itself calls the
            // `list_sessions` tool mid-conversation, clearing
            // voice_session here would tear down the live call. The
            // authoritative lifecycle signal is RealtimeStarted /
            // RealtimeClosed.
            agent_directory_version = current_agent_directory_version(&snapshot);
        }
        for key in removed_thread_keys {
            self.clear_removed_thread_caches(&key);
            self.emit(AppStoreUpdateRecord::ThreadRemoved {
                key,
                agent_directory_version,
            });
        }
        if let Some(approvals) = pending_approvals {
            self.emit(AppStoreUpdateRecord::PendingApprovalsChanged { approvals });
        }
        if let Some(requests) = pending_user_inputs {
            self.emit(AppStoreUpdateRecord::PendingUserInputsChanged { requests });
        }
        if active_thread_cleared {
            self.emit(AppStoreUpdateRecord::ActiveThreadChanged { key: None });
        }
    }

    /// Complete an authoritative list refresh for one multiplexed runtime
    /// without pruning sibling runtimes that were not part of this request.
    pub(crate) fn finalize_thread_list_sync_for_runtime(
        &self,
        server_id: &str,
        runtime_kind: &str,
        incoming_ids: &HashSet<String>,
    ) {
        let sibling_ids = self
            .snapshot()
            .threads
            .iter()
            .filter(|(key, thread)| {
                key.server_id == server_id && thread.agent_runtime_kind != runtime_kind
            })
            .map(|(key, _)| key.thread_id.clone())
            .collect::<HashSet<_>>();
        let mut retained_ids = incoming_ids.clone();
        retained_ids.extend(sibling_ids);
        self.finalize_thread_list_sync(server_id, &retained_ids);
    }

    pub fn upsert_thread_snapshot(&self, mut thread: ThreadSnapshot) {
        let key = thread.key.clone();
        {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            let existing = snapshot.threads.get(&key).cloned();
            if let Some(existing) = existing.as_ref() {
                let consume_claimed_follow_up = existing.active_turn_id.is_none()
                    && thread.active_turn_id.is_some()
                    && existing
                        .queued_follow_up_drafts
                        .first()
                        .is_some_and(|draft| draft.autosend_claimed);
                // Diagnostic for the duplicate-user-message bug (task #11):
                // catch transient overlap where the incoming snapshot's
                // hydrated User items match an overlay that's already in
                // existing.local_overlay_items (sticking around past its
                // dedupe). Logs once per upsert when the condition fires.
                let user_overlay_count = existing
                    .local_overlay_items
                    .iter()
                    .filter(|item| {
                        matches!(&item.content, HydratedConversationItemContent::User(_))
                    })
                    .count();
                let incoming_user_count = thread
                    .items
                    .iter()
                    .filter(|item| {
                        matches!(&item.content, HydratedConversationItemContent::User(_))
                    })
                    .count();
                if user_overlay_count > 0 && incoming_user_count > 0 {
                    tracing::warn!(
                        target: "store",
                        server_id = key.server_id,
                        thread_id = key.thread_id,
                        existing_user_overlay_count = user_overlay_count,
                        incoming_user_item_count = incoming_user_count,
                        existing_user_item_count = existing
                            .items
                            .iter()
                            .filter(|item| {
                                matches!(
                                    &item.content,
                                    HydratedConversationItemContent::User(_)
                                )
                            })
                            .count(),
                        "upsert_thread_snapshot: existing overlay + incoming user items overlap"
                    );
                }
                preserve_thread_title(&existing.info, &mut thread.info);
                preserve_thread_preview(&existing.info, &mut thread.info);
                preserve_thread_created_at(&existing.info, &mut thread.info);
                preserve_thread_fork_lineage(&existing.info, &mut thread.info);
                preserve_thread_runtime_state(existing, &mut thread);
                if thread.agent_runtime_kind == "codex" && existing.agent_runtime_kind != "codex" {
                    thread.agent_runtime_kind = existing.agent_runtime_kind.clone();
                }
                thread.is_resumed = thread.is_resumed || existing.is_resumed;
                preserve_local_overlay_items(existing, &mut thread);
                preserve_queued_follow_ups(existing, &mut thread);
                if consume_claimed_follow_up {
                    remove_first_queued_follow_up(&mut thread);
                }
                // Preserve existing items when the incoming snapshot has none
                // (e.g. thread/read with include_turns=false).
                if thread.items.is_empty() && !existing.items.is_empty() {
                    thread.items = existing.items.clone();
                }
            }
            if !thread.queued_follow_up_drafts.is_empty() || thread.queued_follow_ups.is_empty() {
                sync_thread_follow_up_projection(&mut thread);
            }
            snapshot.threads.insert(key.clone(), thread);
        }
        self.emit_thread_upsert(&key);
    }

    pub fn mark_thread_resumed(&self, key: &ThreadKey, is_resumed: bool) {
        let changed = {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            let Some(thread) = snapshot.threads.get_mut(key) else {
                return;
            };
            if thread.is_resumed == is_resumed {
                false
            } else {
                thread.is_resumed = is_resumed;
                true
            }
        };
        if changed {
            self.emit_thread_metadata_changed(key);
        }
    }

    pub fn enqueue_thread_follow_up_preview(
        &self,
        key: &ThreadKey,
        preview: AppQueuedFollowUpPreview,
    ) {
        self.enqueue_thread_follow_up_draft(
            key,
            QueuedFollowUpDraft {
                preview,
                inputs: Vec::new(),
                source_message_json: None,
                autosend_claimed: false,
            },
        );
    }

    pub(crate) fn enqueue_thread_follow_up_draft(
        &self,
        key: &ThreadKey,
        draft: QueuedFollowUpDraft,
    ) {
        if self
            .mutate_thread_with_result(key, |thread| {
                thread.queued_follow_up_drafts.push(draft);
                sync_thread_follow_up_projection(thread);
            })
            .is_some()
        {
            self.emit_thread_metadata_changed(key);
        }
    }

    pub(crate) fn try_claim_first_queued_follow_up(
        &self,
        key: &ThreadKey,
    ) -> Option<QueuedFollowUpDraft> {
        let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
        let thread = snapshot.threads.get_mut(key)?;
        if thread.active_turn_id.is_some() {
            return None;
        }
        let draft = thread.queued_follow_up_drafts.first_mut()?;
        if draft.autosend_claimed {
            return None;
        }
        draft.autosend_claimed = true;
        Some(draft.clone())
    }

    pub(crate) fn stage_local_user_message_overlay(
        &self,
        key: &ThreadKey,
        inputs: &[upstream::UserInput],
    ) -> Option<String> {
        let item = local_user_message_overlay_item(inputs)?;
        let emitted_item = item.clone();
        let item_id = item.id.clone();
        let updated = self.mutate_thread_with_result(key, |thread| {
            thread
                .local_overlay_items
                .retain(|existing| !is_duplicate_overlay_item(&item, existing));
            thread.local_overlay_items.push(item);
        });
        if updated.is_none() {
            // Diagnostic for the duplicate-user-message bug (task #11): if
            // the thread isn't in the store yet, the overlay silently drops
            // and the iOS UI relies entirely on the upstream item arriving
            // via events. If something else later inserts a second copy,
            // we get two bubbles. Log so the next device run shows whether
            // this is the path being hit.
            tracing::warn!(
                target: "store",
                server_id = key.server_id,
                thread_id = key.thread_id,
                overlay_id = item_id,
                "stage_local_user_message_overlay skipped: thread not in store"
            );
            return None;
        }
        self.emit_thread_item_changed(key, emitted_item);
        Some(item_id)
    }

    pub(crate) fn bind_local_user_message_overlay_to_turn(
        &self,
        key: &ThreadKey,
        item_id: &str,
        turn_id: &str,
    ) {
        if let Some((updated_item, removed_item_ids, needs_reprojection)) = self
            .mutate_thread_with_result(key, |thread| {
                let mut updated_item = None;
                let mut needs_reprojection = false;
                if let Some(item) = thread
                    .local_overlay_items
                    .iter_mut()
                    .find(|item| item.id == item_id)
                {
                    if item.source_turn_id.as_deref() != Some(turn_id) {
                        item.source_turn_id = Some(turn_id.to_string());
                        needs_reprojection = true;
                    }
                    updated_item = Some(item.clone());
                }
                let removed_item_ids = duplicate_local_overlay_item_ids(thread);
                remove_duplicate_local_overlay_items(thread);
                (
                    updated_item.filter(|item| {
                        thread
                            .local_overlay_items
                            .iter()
                            .any(|existing| existing.id == item.id)
                    }),
                    removed_item_ids,
                    needs_reprojection,
                )
            })
        {
            if !removed_item_ids.is_empty() || needs_reprojection {
                self.emit_thread_upsert(key);
            } else if let Some(item) = updated_item {
                self.emit_thread_item_changed(key, item);
            }
        }
    }

    pub(crate) fn bind_first_pending_local_user_message_overlay_to_turn(
        &self,
        key: &ThreadKey,
        turn_id: &str,
    ) {
        if let Some((updated_item, removed_item_ids, needs_reprojection)) = self
            .mutate_thread_with_result(key, |thread| {
                let mut updated_item = None;
                let mut needs_reprojection = false;
                if let Some(item) = thread.local_overlay_items.iter_mut().find(|item| {
                    item.id.starts_with(LOCAL_USER_MESSAGE_ITEM_PREFIX)
                        && item.source_turn_id.is_none()
                }) {
                    item.source_turn_id = Some(turn_id.to_string());
                    needs_reprojection = true;
                    updated_item = Some(item.clone());
                }
                let removed_item_ids = duplicate_local_overlay_item_ids(thread);
                remove_duplicate_local_overlay_items(thread);
                (
                    updated_item.filter(|item| {
                        thread
                            .local_overlay_items
                            .iter()
                            .any(|existing| existing.id == item.id)
                    }),
                    removed_item_ids,
                    needs_reprojection,
                )
            })
        {
            if !removed_item_ids.is_empty() || needs_reprojection {
                self.emit_thread_upsert(key);
            } else if let Some(item) = updated_item {
                self.emit_thread_item_changed(key, item);
            }
        }
    }

    pub(crate) fn remove_local_overlay_item(&self, key: &ThreadKey, item_id: &str) {
        if self
            .mutate_thread_with_result(key, |thread| {
                let before = thread.local_overlay_items.len();
                thread.local_overlay_items.retain(|item| item.id != item_id);
                (before != thread.local_overlay_items.len()).then_some(())
            })
            .flatten()
            .is_some()
        {
            self.emit_thread_upsert(key);
        }
    }

    pub fn set_thread_collaboration_mode(&self, key: &ThreadKey, mode: AppModeKind) {
        if self
            .mutate_thread_with_result(key, |thread| {
                if thread.collaboration_mode == mode {
                    return false;
                }
                thread.collaboration_mode = mode;
                true
            })
            .unwrap_or(false)
        {
            self.emit_thread_metadata_changed(key);
        }
    }

    pub fn dismiss_plan_implementation_prompt(&self, key: &ThreadKey) {
        if self
            .mutate_thread_with_result(key, |thread| {
                let had_prompt = thread.pending_plan_implementation_turn_id.is_some();
                thread.pending_plan_implementation_turn_id = None;
                had_prompt
            })
            .unwrap_or(false)
        {
            self.emit_thread_metadata_changed(key);
        }
    }

    pub(crate) fn remove_thread_follow_up_draft(&self, key: &ThreadKey, preview_id: &str) {
        if self
            .mutate_thread_with_result(key, |thread| {
                thread
                    .queued_follow_up_drafts
                    .retain(|draft| draft.preview.id != preview_id);
                sync_thread_follow_up_projection(thread);
            })
            .is_some()
        {
            self.emit_thread_metadata_changed(key);
        }
    }

    /// Atomically transitions a queued follow-up draft from `Message` to
    /// `PendingSteer`. Returns the updated draft and a snapshot of the
    /// thread's drafts in the new state on success. Returns `None` when the
    /// draft is missing or already in `PendingSteer`/`RetryingSteer` — used
    /// to drop duplicate steer taps before they reach the server.
    pub(crate) fn try_begin_steer_queued_follow_up(
        &self,
        key: &ThreadKey,
        preview_id: &str,
    ) -> Option<(QueuedFollowUpDraft, Vec<QueuedFollowUpDraft>)> {
        let result = self
            .mutate_thread_with_result(key, |thread| {
                let position = thread
                    .queued_follow_up_drafts
                    .iter()
                    .position(|d| d.preview.id == preview_id)?;
                let current_kind = thread.queued_follow_up_drafts[position].preview.kind;
                if current_kind != super::snapshot::AppQueuedFollowUpKind::Message {
                    return None;
                }
                thread.queued_follow_up_drafts[position].preview.kind =
                    super::snapshot::AppQueuedFollowUpKind::PendingSteer;
                let updated = thread.queued_follow_up_drafts[position].clone();
                let next = thread.queued_follow_up_drafts.clone();
                sync_thread_follow_up_projection(thread);
                Some((updated, next))
            })
            .flatten();
        if result.is_some() {
            self.emit_thread_metadata_changed(key);
        }
        result
    }

    pub(crate) fn set_thread_follow_up_drafts(
        &self,
        key: &ThreadKey,
        drafts: Vec<QueuedFollowUpDraft>,
    ) {
        if self
            .mutate_thread_with_result(key, |thread| {
                if thread.queued_follow_up_drafts == drafts {
                    return false;
                }
                thread.queued_follow_up_drafts = drafts;
                sync_thread_follow_up_projection(thread);
                true
            })
            .unwrap_or(false)
        {
            self.emit_thread_metadata_changed(key);
        }
    }

    pub fn remove_thread(&self, key: &ThreadKey) {
        let agent_directory_version;
        {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            snapshot.threads.remove(key);
            if snapshot.active_thread.as_ref() == Some(key) {
                snapshot.active_thread = None;
            }
            if snapshot.voice_session.active_thread.as_ref() == Some(key) {
                snapshot.voice_session = AppVoiceSessionSnapshot::default();
            }
            snapshot
                .pending_approvals
                .retain(|approval| approval.thread_id.as_deref() != Some(key.thread_id.as_str()));
            snapshot.pending_user_inputs.retain(|request| {
                !(request.server_id == key.server_id && request.thread_id == key.thread_id)
            });
            let remaining_user_input_keys = snapshot
                .pending_user_inputs
                .iter()
                .map(|request| PendingUserInputKey {
                    server_id: request.server_id.clone(),
                    request_id: request.id.clone(),
                })
                .collect::<HashSet<_>>();
            snapshot
                .pending_user_input_seeds
                .retain(|key, _| remaining_user_input_keys.contains(key));
            agent_directory_version = current_agent_directory_version(&snapshot);
        }
        self.clear_removed_thread_caches(key);
        self.emit(AppStoreUpdateRecord::ThreadRemoved {
            key: key.clone(),
            agent_directory_version,
        });
    }

    pub fn set_active_thread(&self, key: Option<ThreadKey>) {
        {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            snapshot.active_thread = key.clone();
        }
        self.emit(AppStoreUpdateRecord::ActiveThreadChanged { key });
    }

    pub fn set_voice_handoff_thread(&self, key: Option<ThreadKey>) {
        {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            snapshot.voice_session.handoff_thread_key = key;
        }
        self.emit(AppStoreUpdateRecord::VoiceSessionChanged);
    }

    pub fn replace_pending_approvals(&self, approvals: Vec<PendingApproval>) {
        let changed = {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            if snapshot.pending_approvals == approvals && snapshot.pending_approval_seeds.is_empty()
            {
                false
            } else {
                snapshot.pending_approvals = approvals.clone();
                snapshot.pending_approval_seeds.clear();
                true
            }
        };
        if changed {
            self.emit(AppStoreUpdateRecord::PendingApprovalsChanged { approvals });
        }
    }

    #[cfg(test)]
    pub(crate) fn replace_pending_approvals_with_seeds(
        &self,
        approvals: Vec<PendingApprovalWithSeed>,
    ) {
        let public_approvals = approvals
            .iter()
            .map(|entry| entry.approval.clone())
            .collect::<Vec<_>>();
        let next_seeds = approvals
            .into_iter()
            .map(|entry| {
                (
                    PendingApprovalKey {
                        server_id: entry.approval.server_id.clone(),
                        request_id: entry.approval.id.clone(),
                    },
                    entry.seed,
                )
            })
            .collect::<HashMap<_, _>>();
        let changed = {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            if snapshot.pending_approvals == public_approvals
                && snapshot.pending_approval_seeds == next_seeds
            {
                false
            } else {
                snapshot.pending_approvals = public_approvals.clone();
                snapshot.pending_approval_seeds = next_seeds;
                true
            }
        };
        if changed {
            self.emit(AppStoreUpdateRecord::PendingApprovalsChanged {
                approvals: public_approvals,
            });
        }
    }

    pub fn replace_pending_user_inputs(&self, requests: Vec<PendingUserInputRequest>) {
        let changed = {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            if snapshot.pending_user_inputs == requests {
                false
            } else {
                snapshot.pending_user_inputs = requests.clone();
                let remaining_keys = snapshot
                    .pending_user_inputs
                    .iter()
                    .map(|request| PendingUserInputKey {
                        server_id: request.server_id.clone(),
                        request_id: request.id.clone(),
                    })
                    .collect::<HashSet<_>>();
                snapshot
                    .pending_user_input_seeds
                    .retain(|key, _| remaining_keys.contains(key));
                true
            }
        };
        if changed {
            self.emit(AppStoreUpdateRecord::PendingUserInputsChanged { requests });
        }
    }

    pub fn resolve_approval(&self, request_id: &str) {
        let approvals = {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            snapshot
                .pending_approvals
                .retain(|approval| approval.id != request_id);
            snapshot
                .pending_approval_seeds
                .retain(|key, _| key.request_id != request_id);
            snapshot.pending_approvals.clone()
        };
        self.emit(AppStoreUpdateRecord::PendingApprovalsChanged { approvals });
    }

    pub(crate) fn pending_approval_seed(
        &self,
        server_id: &str,
        request_id: &str,
    ) -> Option<PendingApprovalSeed> {
        self.snapshot
            .read()
            .expect("app store lock poisoned")
            .pending_approval_seeds
            .get(&PendingApprovalKey {
                server_id: server_id.to_string(),
                request_id: request_id.to_string(),
            })
            .cloned()
    }

    pub(crate) fn pending_user_input_seed(
        &self,
        server_id: &str,
        request_id: &str,
    ) -> Option<PendingUserInputSeed> {
        self.snapshot
            .read()
            .expect("app store lock poisoned")
            .pending_user_input_seeds
            .get(&PendingUserInputKey {
                server_id: server_id.to_string(),
                request_id: request_id.to_string(),
            })
            .cloned()
    }

    pub fn resolve_pending_user_input(&self, request_id: &str) {
        let requests = {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            snapshot
                .pending_user_inputs
                .retain(|request| request.id != request_id);
            snapshot
                .pending_user_input_seeds
                .retain(|key, _| key.request_id != request_id);
            snapshot.pending_user_inputs.clone()
        };
        self.emit(AppStoreUpdateRecord::PendingUserInputsChanged { requests });
    }

    pub fn resolve_pending_user_input_with_response(
        &self,
        request_id: &str,
        answers: Vec<PendingUserInputAnswer>,
    ) {
        let (requests, thread_key) = {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            let request = snapshot
                .pending_user_inputs
                .iter()
                .find(|request| request.id == request_id)
                .cloned();

            let mut thread_key = None;
            if let Some(request) = request {
                thread_key = Some(ThreadKey {
                    server_id: request.server_id.clone(),
                    thread_id: request.thread_id.clone(),
                });
                if let Some(thread) = snapshot.threads.get_mut(&ThreadKey {
                    server_id: request.server_id.clone(),
                    thread_id: request.thread_id.clone(),
                }) {
                    let item = answered_user_input_item(&request, &answers);
                    thread
                        .local_overlay_items
                        .retain(|existing| !is_duplicate_overlay_item(&item, existing));
                    thread.local_overlay_items.push(item);
                }
            }

            snapshot
                .pending_user_inputs
                .retain(|request| request.id != request_id);
            snapshot
                .pending_user_input_seeds
                .retain(|key, _| key.request_id != request_id);
            (snapshot.pending_user_inputs.clone(), thread_key)
        };
        self.emit(AppStoreUpdateRecord::PendingUserInputsChanged { requests });
        if let Some(key) = thread_key {
            self.emit_thread_upsert(&key);
        }
    }

    pub fn update_server_account(
        &self,
        server_id: &str,
        account: Option<crate::types::Account>,
        requires_openai_auth: bool,
    ) {
        {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            if let Some(server) = snapshot.servers.get_mut(server_id) {
                server.account = account;
                server.requires_openai_auth = requires_openai_auth;
            }
        }
        self.emit(AppStoreUpdateRecord::ServerChanged {
            server_id: server_id.to_string(),
        });
    }

    pub fn update_server_rate_limits(
        &self,
        server_id: &str,
        runtime_kind: AgentRuntimeKind,
        rate_limits: Option<crate::types::RateLimitSnapshot>,
    ) {
        {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            if let Some(server) = snapshot.servers.get_mut(server_id) {
                match rate_limits.clone() {
                    Some(snapshot_value) => {
                        server
                            .rate_limits_by_runtime
                            .insert(runtime_kind, snapshot_value);
                    }
                    None => {
                        server.rate_limits_by_runtime.remove(&runtime_kind);
                    }
                }
                server.rate_limits = rate_limits;
            }
        }
        self.emit(AppStoreUpdateRecord::ServerChanged {
            server_id: server_id.to_string(),
        });
    }

    pub fn update_server_models(
        &self,
        server_id: &str,
        models: Option<Vec<crate::types::ModelInfo>>,
    ) {
        {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            if let Some(server) = snapshot.servers.get_mut(server_id) {
                server.available_models = models;
            }
        }
        self.emit(AppStoreUpdateRecord::ServerChanged {
            server_id: server_id.to_string(),
        });
    }

    pub fn update_server_agent_runtimes(&self, server_id: &str, runtimes: Vec<AgentRuntimeInfo>) {
        let runtimes = dedupe_agent_runtimes(runtimes);
        {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            if let Some(server) = snapshot.servers.get_mut(server_id) {
                server.agent_runtimes = runtimes;
            }
        }
        self.emit(AppStoreUpdateRecord::ServerChanged {
            server_id: server_id.to_string(),
        });
    }

    pub fn set_thread_agent_runtime(&self, key: &ThreadKey, runtime_kind: AgentRuntimeKind) {
        let changed = {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            let Some(thread) = snapshot.threads.get_mut(key) else {
                return;
            };
            if thread.agent_runtime_kind == runtime_kind {
                false
            } else {
                thread.agent_runtime_kind = runtime_kind;
                true
            }
        };
        if changed {
            self.emit_thread_metadata_changed(key);
        }
    }

    pub fn update_server_health(&self, server_id: &str, health: ServerHealthSnapshot) {
        {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            if let Some(server) = snapshot.servers.get_mut(server_id) {
                server.health = health;
            }
        }
        self.emit(AppStoreUpdateRecord::ServerChanged {
            server_id: server_id.to_string(),
        });
    }

    pub fn set_server_supports_turn_pagination(&self, server_id: &str, supports: bool) {
        let changed = {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            match snapshot.servers.get_mut(server_id) {
                Some(server) if server.supports_turn_pagination != supports => {
                    server.supports_turn_pagination = supports;
                    true
                }
                _ => false,
            }
        };
        if changed {
            self.emit(AppStoreUpdateRecord::ServerChanged {
                server_id: server_id.to_string(),
            });
        }
    }

    pub fn server_supports_turn_pagination(&self, server_id: &str) -> bool {
        let snapshot = self.snapshot.read().expect("app store lock poisoned");
        snapshot
            .servers
            .get(server_id)
            .map(|server| server.supports_turn_pagination)
            .unwrap_or(true)
    }

    pub fn note_app_lifecycle_phase(&self, phase: AppLifecyclePhaseSnapshot) {
        let now = std::time::Instant::now();
        let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
        for server in snapshot.servers.values_mut() {
            server.transport.last_lifecycle_phase = phase;
            server.transport.last_lifecycle_transition_at = Some(now);
            if phase == AppLifecyclePhaseSnapshot::Active {
                server.transport.last_resumed_at = Some(now);
            }
        }
    }

    pub fn server_pending_mutation_kind(
        &self,
        server_id: &str,
    ) -> Option<ServerMutatingCommandKind> {
        self.snapshot
            .read()
            .expect("app store lock poisoned")
            .servers
            .get(server_id)
            .and_then(|server| server.transport.pending_mutation.as_ref())
            .map(|pending| pending.kind)
    }

    pub fn begin_server_mutating_command(
        &self,
        server_id: &str,
        kind: ServerMutatingCommandKind,
        thread_id: &str,
    ) -> String {
        let request_id = uuid::Uuid::new_v4().to_string();
        let started_at = std::time::Instant::now();
        let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
        if let Some(server) = snapshot.servers.get_mut(server_id) {
            server.transport.pending_mutation = Some(PendingServerMutatingCommand {
                kind,
                thread_id: thread_id.to_string(),
                local_request_id: request_id.clone(),
                started_at,
                lifecycle_phase_at_send: server.transport.last_lifecycle_phase,
            });
        }
        request_id
    }

    pub fn finish_server_mutating_command_success(&self, server_id: &str, local_request_id: &str) {
        let now = std::time::Instant::now();
        let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
        if let Some(server) = snapshot.servers.get_mut(server_id) {
            if server
                .transport
                .pending_mutation
                .as_ref()
                .is_some_and(|pending| pending.local_request_id == local_request_id)
            {
                server.transport.pending_mutation = None;
            }
            server.transport.last_direct_request_ok_at = Some(now);
        }
    }

    pub fn finish_server_mutating_command_failure(&self, server_id: &str, local_request_id: &str) {
        let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
        if let Some(server) = snapshot.servers.get_mut(server_id)
            && server
                .transport
                .pending_mutation
                .as_ref()
                .is_some_and(|pending| pending.local_request_id == local_request_id)
        {
            server.transport.pending_mutation = None;
        }
    }

    pub fn note_server_direct_request_success(&self, server_id: &str) {
        let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
        if let Some(server) = snapshot.servers.get_mut(server_id) {
            server.transport.last_direct_request_ok_at = Some(std::time::Instant::now());
        }
    }

    pub fn update_server_connection_progress(
        &self,
        server_id: &str,
        connection_progress: Option<AppConnectionProgressSnapshot>,
    ) {
        {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            if let Some(server) = snapshot.servers.get_mut(server_id) {
                server.connection_progress = connection_progress;
            }
        }
        self.emit(AppStoreUpdateRecord::ServerChanged {
            server_id: server_id.to_string(),
        });
    }

    pub fn rename_server(&self, server_id: &str, display_name: String) {
        {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            if let Some(server) = snapshot.servers.get_mut(server_id) {
                server.display_name = display_name;
            }
        }
        self.emit(AppStoreUpdateRecord::ServerChanged {
            server_id: server_id.to_string(),
        });
    }

    pub fn update_server_wake_mac(&self, server_id: &str, wake_mac: Option<String>) {
        {
            let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
            if let Some(server) = snapshot.servers.get_mut(server_id) {
                server.wake_mac = wake_mac;
            }
        }
        self.emit(AppStoreUpdateRecord::ServerChanged {
            server_id: server_id.to_string(),
        });
    }

    pub(crate) fn apply_ui_event(&self, event: &UiEvent) {
        match event {
            UiEvent::ThreadStarted { key, notification } => {
                let info = thread_info_from_upstream(notification.thread.clone());
                self.upsert_or_merge_thread(key.clone(), info, |thread| {
                    thread.info.status = ThreadSummaryStatus::Active;
                    if thread.info.parent_thread_id.is_some() {
                        thread.info.agent_status = Some("running".to_string());
                    }
                });
            }
            UiEvent::ThreadArchived { key } => {
                self.remove_thread(key);
            }
            UiEvent::ThreadNameUpdated { key, thread_name } => {
                self.mutate_thread(key, |thread| {
                    thread.info.title = thread_name.clone();
                });
            }
            UiEvent::ThreadStatusChanged { key, notification } => {
                let info = thread_info_from_upstream_status_change(
                    &notification.thread_id,
                    notification.status.clone(),
                );
                let status = info.status.clone();
                self.upsert_or_merge_thread(key.clone(), info, |thread| {
                    if matches!(
                        status,
                        ThreadSummaryStatus::Idle | ThreadSummaryStatus::SystemError
                    ) {
                        thread.active_turn_id = None;
                        thread.active_plan_progress = None;
                        thread.info.status = status.clone();
                    }
                    if thread.info.parent_thread_id.is_some() {
                        thread.info.agent_status = match thread.info.status {
                            ThreadSummaryStatus::Active => Some("running".to_string()),
                            ThreadSummaryStatus::SystemError => Some("errored".to_string()),
                            ThreadSummaryStatus::Idle => thread
                                .info
                                .agent_status
                                .clone()
                                .or(Some("completed".to_string())),
                            ThreadSummaryStatus::NotLoaded => thread.info.agent_status.clone(),
                        };
                    }
                });
            }
            UiEvent::ThreadGoalUpdated { key, goal, .. } => {
                self.apply_thread_goal(key, goal.clone());
            }
            UiEvent::ThreadGoalCleared { key } => {
                self.clear_thread_goal(key);
            }
            UiEvent::ModelRerouted { key, notification } => {
                let item = make_model_rerouted_item(
                    &notification.turn_id,
                    Some(notification.from_model.clone()),
                    notification.to_model.clone(),
                    Some(format_model_reroute_reason(&notification.reason)),
                    Some(&notification.turn_id),
                );
                if self
                    .mutate_thread_with_result(key, |thread| {
                        thread.model = Some(notification.to_model.clone());
                        thread.info.model = Some(notification.to_model.clone());
                        upsert_item(thread, item.clone());
                    })
                    .is_some()
                {
                    self.emit_thread_metadata_changed(key);
                    self.emit_thread_item_changed(key, item);
                }
            }
            UiEvent::TurnStarted { key, turn_id } => {
                if self
                    .mutate_thread_with_result(key, |thread| {
                        remove_first_queued_follow_up(thread);
                        thread.active_turn_id = Some(turn_id.clone());
                        thread.active_plan_progress = None;
                        thread.pending_plan_implementation_turn_id = None;
                        thread.info.status = ThreadSummaryStatus::Active;
                        if thread.info.parent_thread_id.is_some() {
                            thread.info.agent_status = Some("running".to_string());
                        }
                    })
                    .is_some()
                {
                    self.bind_first_pending_local_user_message_overlay_to_turn(key, turn_id);
                    self.emit_thread_metadata_changed(key);
                }
            }
            UiEvent::TurnCompleted { key, turn_id, .. } => {
                self.clear_dynamic_tool_arg_buffers_for_thread(key);
                if self
                    .mutate_thread_with_result(key, |thread| {
                        thread.active_turn_id = None;
                        thread.active_plan_progress = None;
                        thread.info.status = ThreadSummaryStatus::Idle;
                        if thread.info.parent_thread_id.is_some() {
                            thread.info.agent_status = Some("completed".to_string());
                        }
                        // Clean up user input response overlays — they were
                        // answered during this turn and no longer need to show.
                        thread
                            .local_overlay_items
                            .retain(|item| !item.id.starts_with(USER_INPUT_RESPONSE_ITEM_PREFIX));
                        // Clean up steered follow-ups now that the turn is done.
                        thread.queued_follow_up_drafts.retain(|draft| {
                            draft.preview.kind
                                != super::snapshot::AppQueuedFollowUpKind::PendingSteer
                        });
                        sync_thread_follow_up_projection(thread);
                        // Show the plan implementation prompt after the plan
                        // turn finishes so implement_plan can safely start a
                        // new turn without colliding with the active one.
                        if thread.collaboration_mode == AppModeKind::Plan
                            && thread.items.iter().any(|item| {
                                matches!(
                                    item.content,
                                    HydratedConversationItemContent::ProposedPlan { .. }
                                )
                            })
                            && thread.pending_plan_implementation_turn_id.is_none()
                        {
                            thread.pending_plan_implementation_turn_id = Some(turn_id.to_string());
                        }
                    })
                    .is_some()
                {
                    self.emit_thread_metadata_changed(key);
                }
            }
            UiEvent::TurnPlanUpdated { key, notification } => {
                if self
                    .mutate_thread_with_result(key, |thread| {
                        thread.active_plan_progress = Some(AppPlanProgressSnapshot {
                            turn_id: notification.turn_id.clone(),
                            explanation: notification.explanation.clone(),
                            plan: notification
                                .plan
                                .iter()
                                .cloned()
                                .map(AppPlanStep::from)
                                .collect(),
                        });
                    })
                    .is_some()
                {
                    self.emit_thread_metadata_changed(key);
                }
            }
            UiEvent::ItemStarted { key, notification } => {
                if let Some(item) = conversation_item_from_upstream_with_turn(
                    notification.item.clone(),
                    Some(&notification.turn_id),
                ) {
                    self.apply_item_update(key, item);
                }
            }
            UiEvent::ItemCompleted { key, notification } => {
                // Clear any streaming dynamic-tool-call argument buffers
                // that were accumulating for this item — the call has
                // finalized (or failed) and further partial-parse work
                // is wasted.
                if matches!(
                    notification.item,
                    codex_app_server_protocol::ThreadItem::DynamicToolCall { .. }
                ) {
                    let item_id = notification.item.id().to_string();
                    self.clear_dynamic_tool_arg_buffers_for_item(key, &item_id);
                }
                if let Some(item) = conversation_item_from_upstream_with_turn(
                    notification.item.clone(),
                    Some(&notification.turn_id),
                ) {
                    self.apply_item_update(key, item);
                }
                if matches!(
                    notification.item,
                    codex_app_server_protocol::ThreadItem::Plan { .. }
                ) && self
                    .mutate_thread_with_result(key, |thread| {
                        if thread.collaboration_mode != AppModeKind::Plan {
                            thread.collaboration_mode = AppModeKind::Plan;
                            return true;
                        }
                        false
                    })
                    .unwrap_or(false)
                {
                    self.emit_thread_metadata_changed(key);
                }
            }
            UiEvent::MessageDelta {
                key,
                item_id,
                delta,
            } => {
                let inserted_placeholder = self
                    .mutate_thread_with_result(key, |thread| {
                        append_assistant_delta(thread, item_id, delta)
                    })
                    .unwrap_or(false);
                if inserted_placeholder {
                    self.emit_thread_item_changed_by_id(key, item_id);
                } else {
                    self.emit_thread_streaming_delta(
                        key,
                        item_id,
                        ThreadStreamingDeltaKind::AssistantText,
                        delta,
                    );
                }
            }
            UiEvent::ReasoningDelta {
                key,
                item_id,
                delta,
            } => {
                let result = self
                    .mutate_thread_with_result(key, |thread| {
                        append_reasoning_delta(thread, item_id, delta)
                    })
                    .unwrap_or(LiveDeltaApplyResult::Failed);
                if result.streamed() {
                    self.emit_thread_streaming_delta(
                        key,
                        item_id,
                        ThreadStreamingDeltaKind::ReasoningText,
                        delta,
                    );
                } else if result.requires_item_upsert() {
                    self.emit_thread_item_changed_by_id(key, item_id);
                } else {
                    tracing::debug!(
                        target: "store",
                        server_id = key.server_id,
                        thread_id = key.thread_id,
                        item_id,
                        kind = "reasoning",
                        "falling back to ThreadUpserted after live delta repair failed"
                    );
                    self.emit_thread_upsert(key);
                }
            }
            UiEvent::PlanDelta {
                key,
                item_id,
                delta,
            } => {
                let result = self
                    .mutate_thread_with_result(key, |thread| {
                        append_plan_delta(thread, item_id, delta)
                    })
                    .unwrap_or(LiveDeltaApplyResult::Failed);
                if result.streamed() {
                    self.emit_thread_streaming_delta(
                        key,
                        item_id,
                        ThreadStreamingDeltaKind::PlanText,
                        delta,
                    );
                } else if result.requires_item_upsert() {
                    self.emit_thread_item_changed_by_id(key, item_id);
                } else {
                    tracing::debug!(
                        target: "store",
                        server_id = key.server_id,
                        thread_id = key.thread_id,
                        item_id,
                        kind = "plan",
                        "falling back to ThreadUpserted after live delta repair failed"
                    );
                    self.emit_thread_upsert(key);
                }
            }
            UiEvent::CommandOutputDelta {
                key,
                item_id,
                delta,
            } => {
                let result = self
                    .mutate_thread_with_result(key, |thread| {
                        append_command_output_delta(thread, item_id, delta)
                    })
                    .unwrap_or(LiveDeltaApplyResult::Failed);
                if result.streamed() {
                    self.emit_thread_streaming_delta(
                        key,
                        item_id,
                        ThreadStreamingDeltaKind::CommandOutput,
                        delta,
                    );
                } else if result.requires_item_upsert() {
                    self.emit_thread_item_changed_by_id(key, item_id);
                } else {
                    tracing::debug!(
                        target: "store",
                        server_id = key.server_id,
                        thread_id = key.thread_id,
                        item_id,
                        kind = "command_output",
                        "falling back to ThreadUpserted after live delta repair failed"
                    );
                    self.emit_thread_upsert(key);
                }
            }
            UiEvent::DynamicToolCallArgumentsDelta {
                key,
                item_id,
                call_id,
                delta,
            } => {
                // `item_id` may be empty before the provider confirms
                // the item id; key on `call_id` as primary, fall back
                // to `item_id` only when `call_id` is absent. Track the
                // latest non-empty item_id in the buffer so follow-up
                // deltas that do carry one don't lose it.
                let buffer_key = call_id.clone().unwrap_or_else(|| item_id.clone());
                // Hard cap on per-call buffer growth. Widget HTML well past
                // this size is either a runaway stream or pathological; the
                // saved-apps state cap is 256 KB so we match it. Once hit,
                // further deltas are silently dropped for this call; the
                // final ItemCompleted still delivers the full payload.
                const MAX_BUFFER_BYTES: usize = 256 * 1024;
                let (partial, known_item_id) = {
                    let mut guard = self
                        .dynamic_tool_arg_buffers
                        .write()
                        .expect("app store dynamic_tool_arg_buffers poisoned");
                    let entry = guard
                        .entry((key.clone(), buffer_key.clone()))
                        .or_insert_with(|| DynamicToolCallArgBuffer {
                            item_id: item_id.clone(),
                            buffer: String::new(),
                        });
                    if !item_id.is_empty() && entry.item_id.is_empty() {
                        entry.item_id = item_id.clone();
                    }
                    if entry.buffer.len() < MAX_BUFFER_BYTES {
                        let remaining = MAX_BUFFER_BYTES - entry.buffer.len();
                        if delta.len() <= remaining {
                            entry.buffer.push_str(delta);
                        } else {
                            // UTF-8-safe prefix; splitting mid
                            // code-point would poison the buffer.
                            let mut cut = remaining;
                            while cut > 0 && !delta.is_char_boundary(cut) {
                                cut -= 1;
                            }
                            entry.buffer.push_str(&delta[..cut]);
                        }
                    }
                    (entry.buffer.clone(), entry.item_id.clone())
                };

                // Tolerantly extract widget fields from the accumulated
                // buffer and broadcast via `DynamicWidgetStreaming` so
                // platforms can progressively render the widget bubble.
                // Platforms already consume this variant (shipped with
                // SW-I1/SW-A1). `known_item_id` may still be empty on
                // the very first delta; in that case use the buffer_key
                // so platforms have a stable correlation id.
                let streaming_item_id = if !item_id.is_empty() {
                    item_id.clone()
                } else if !known_item_id.is_empty() {
                    known_item_id
                } else {
                    buffer_key.clone()
                };
                if let Some(widget) =
                    crate::conversation::streaming_widget_data_from_partial_arguments(&partial)
                {
                    self.emit(AppStoreUpdateRecord::DynamicWidgetStreaming {
                        key: key.clone(),
                        item_id: streaming_item_id,
                        call_id: buffer_key,
                        widget,
                    });
                }
            }
            UiEvent::TurnDiffUpdated { key, notification } => {
                let item = make_turn_diff_item(
                    &notification.turn_id,
                    notification.diff.clone(),
                    Some(&notification.turn_id),
                );
                if self
                    .mutate_thread_with_result(key, |thread| upsert_item(thread, item.clone()))
                    .is_some()
                {
                    self.emit_thread_item_changed(key, item);
                }
            }
            UiEvent::FileChangePatchUpdated { key, notification } => {
                // Synthesize the in-flight FileChange item from the
                // progressive notification (codex emits these as a patch
                // is being applied; the canonical ItemCompleted lands at
                // the end). Going through the normal `apply_item_update`
                // path keeps fingerprint dedupe + `emit_thread_item_changed`
                // behaviour identical to a real ItemStarted, so home-row
                // diff stats + Edit log entries climb in real time.
                let upstream_item = upstream::ThreadItem::FileChange {
                    id: notification.item_id.clone(),
                    changes: notification.changes.clone(),
                    status: upstream::PatchApplyStatus::InProgress,
                };
                if let Some(item) = conversation_item_from_upstream_with_turn(
                    upstream_item,
                    Some(&notification.turn_id),
                ) {
                    self.apply_item_update(key, item);
                }
            }
            UiEvent::McpToolCallProgress { key, notification } => {
                let result = self
                    .mutate_thread_with_result(key, |thread| {
                        append_mcp_progress(thread, &notification.item_id, &notification.message)
                    })
                    .unwrap_or(LiveDeltaApplyResult::Failed);
                if result.streamed() {
                    self.emit_thread_streaming_delta(
                        key,
                        &notification.item_id,
                        ThreadStreamingDeltaKind::McpProgress,
                        &notification.message,
                    );
                } else if result.requires_item_upsert() {
                    self.emit_thread_item_changed_by_id(key, &notification.item_id);
                } else {
                    tracing::debug!(
                        target: "store",
                        server_id = key.server_id,
                        thread_id = key.thread_id,
                        item_id = notification.item_id,
                        kind = "mcp_progress",
                        "falling back to ThreadUpserted after live delta repair failed"
                    );
                    self.emit_thread_upsert(key);
                }
            }
            UiEvent::ApprovalRequested { approval, .. } => {
                let approvals = {
                    let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
                    if !snapshot
                        .pending_approvals
                        .iter()
                        .any(|existing| existing.id == approval.approval.id)
                    {
                        snapshot.pending_approvals.push(approval.approval.clone());
                        snapshot.pending_approval_seeds.insert(
                            PendingApprovalKey {
                                server_id: approval.approval.server_id.clone(),
                                request_id: approval.approval.id.clone(),
                            },
                            approval.seed.clone(),
                        );
                    }
                    snapshot.pending_approvals.clone()
                };
                self.emit(AppStoreUpdateRecord::PendingApprovalsChanged { approvals });
            }
            UiEvent::ServerRequestResolved { notification, .. } => {
                let request_id_string;
                let request_id = match &notification.request_id {
                    codex_app_server_protocol::RequestId::String(value) => value.as_str(),
                    codex_app_server_protocol::RequestId::Integer(value) => {
                        request_id_string = value.to_string();
                        request_id_string.as_str()
                    }
                };
                self.resolve_approval(request_id);
                self.resolve_pending_user_input(request_id);
            }
            UiEvent::AccountRateLimitsUpdated {
                server_id,
                runtime_kind,
                notification,
            } => {
                let rate_limits = notification.rate_limits.clone().into();
                self.update_server_rate_limits(server_id, runtime_kind.clone(), Some(rate_limits));
            }
            UiEvent::ConnectionStateChanged { server_id, health } => {
                {
                    let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
                    if let Some(server) = snapshot.servers.get_mut(server_id) {
                        let next_health = ServerHealthSnapshot::from_wire(health);
                        server.health = match (&server.connection_progress, next_health) {
                            // During SSH session replacement the old session can report a transient
                            // disconnect while the new transport is still bootstrapping. Keep the
                            // server visible as connecting until that progress finishes or an
                            // explicit reconnect failure updates the store directly.
                            (Some(_), ServerHealthSnapshot::Disconnected) => {
                                ServerHealthSnapshot::Connecting
                            }
                            (_, health) => health,
                        };
                    }
                }
                self.emit(AppStoreUpdateRecord::ServerChanged {
                    server_id: server_id.clone(),
                });
            }
            UiEvent::ContextTokensUpdated { key, used, limit } => {
                if self
                    .mutate_thread_with_result(key, |thread| {
                        thread.context_tokens_used = Some(*used);
                        thread.model_context_window = Some(*limit);
                    })
                    .is_some()
                {
                    self.emit_thread_metadata_changed(key);
                }
            }
            event @ (UiEvent::RealtimeStarted { .. }
            | UiEvent::RealtimeSdp { .. }
            | UiEvent::RealtimeTranscriptUpdated { .. }
            | UiEvent::RealtimeItemAdded { .. }
            | UiEvent::RealtimeOutputAudioDelta { .. }
            | UiEvent::RealtimeError { .. }
            | UiEvent::RealtimeClosed { .. }) => self.apply_realtime_event(event),
            UiEvent::Error { key, message, code } => {
                if let Some(key) = key {
                    let item = {
                        let mut item = None;
                        self.mutate_thread_with_result(key, |thread| {
                            let next = make_error_item(
                                format!("error-{}-{}", key.thread_id, thread.items.len()),
                                message.clone(),
                                *code,
                            );
                            thread.items.push(next.clone());
                            item = Some(next);
                        });
                        item
                    };
                    if let Some(item) = item {
                        self.emit_thread_item_changed(key, item);
                    }
                }
            }
            UiEvent::UserInputRequested { request, seed } => {
                let requests = {
                    let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
                    snapshot
                        .pending_user_inputs
                        .retain(|existing| existing.id != request.id);
                    snapshot.pending_user_inputs.push(request.clone());
                    let key = PendingUserInputKey {
                        server_id: request.server_id.clone(),
                        request_id: request.id.clone(),
                    };
                    if let Some(seed) = seed {
                        snapshot.pending_user_input_seeds.insert(key, seed.clone());
                    } else {
                        snapshot.pending_user_input_seeds.remove(&key);
                    }
                    snapshot.pending_user_inputs.clone()
                };
                self.emit(AppStoreUpdateRecord::PendingUserInputsChanged { requests });
                let key = ThreadKey {
                    server_id: request.server_id.clone(),
                    thread_id: request.thread_id.clone(),
                };
                self.emit_thread_metadata_changed(&key);
            }
            UiEvent::RawNotification { .. } => {}
        }
    }

    pub(crate) fn mutate_thread<F>(&self, key: &ThreadKey, mutate: F)
    where
        F: FnOnce(&mut ThreadSnapshot),
    {
        if self
            .mutate_thread_with_result(key, |thread| {
                mutate(thread);
            })
            .is_some()
        {
            self.emit_thread_metadata_changed(key);
        }
    }

    pub(crate) fn mutate_thread_with_result<F, R>(&self, key: &ThreadKey, mutate: F) -> Option<R>
    where
        F: FnOnce(&mut ThreadSnapshot) -> R,
    {
        let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
        let thread = snapshot.threads.get_mut(key)?;
        Some(mutate(thread))
    }

    pub(crate) fn apply_thread_goal(&self, key: &ThreadKey, goal: AppThreadGoal) {
        if self
            .mutate_thread_with_result(key, |thread| {
                thread.goal = Some(goal);
            })
            .is_some()
        {
            self.emit_thread_metadata_changed(key);
        }
    }

    pub(crate) fn clear_thread_goal(&self, key: &ThreadKey) {
        if self
            .mutate_thread_with_result(key, |thread| {
                thread.goal = None;
            })
            .is_some()
        {
            self.emit_thread_metadata_changed(key);
        }
    }

    pub(crate) fn emit_thread_metadata_changed(&self, key: &ThreadKey) {
        let update = {
            let snapshot = self.snapshot.read().expect("app store lock poisoned");
            match project_thread_state_update(&snapshot, key) {
                Ok(Some((mut state, mut session_summary, agent_directory_version))) => {
                    if state.active_turn_id.is_some()
                        || state.info.status == ThreadSummaryStatus::Active
                    {
                        state.info.updated_at = None;
                        session_summary.updated_at = None;
                    }
                    let cached = self
                        .last_thread_state_updates
                        .read()
                        .expect("thread state cache lock poisoned")
                        .get(key)
                        .cloned();
                    if cached
                        == Some((
                            state.clone(),
                            session_summary.clone(),
                            agent_directory_version,
                        ))
                    {
                        None
                    } else {
                        self.last_thread_state_updates
                            .write()
                            .expect("thread state cache lock poisoned")
                            .insert(
                                key.clone(),
                                (
                                    state.clone(),
                                    session_summary.clone(),
                                    agent_directory_version,
                                ),
                            );
                        Some(AppStoreUpdateRecord::ThreadMetadataChanged {
                            state,
                            session_summary,
                            agent_directory_version,
                        })
                    }
                }
                Ok(None) => None,
                Err(error) => {
                    tracing::error!(
                        target: "store",
                        server_id = key.server_id,
                        thread_id = key.thread_id,
                        %error,
                        "failed to project ThreadMetadataChanged"
                    );
                    Some(AppStoreUpdateRecord::FullResync)
                }
            }
        };
        if let Some(update) = update {
            self.emit(update);
        }
    }

    fn clear_thread_update_caches(&self, key: &ThreadKey) {
        self.last_thread_state_updates
            .write()
            .expect("thread state cache lock poisoned")
            .remove(key);
        self.last_thread_item_upserts
            .write()
            .expect("thread item cache lock poisoned")
            .retain(|(thread_key, _), _| thread_key != key);
    }

    fn clear_removed_thread_caches(&self, key: &ThreadKey) {
        self.clear_thread_update_caches(key);
        self.clear_dynamic_tool_arg_buffers_for_thread(key);
    }

    pub(crate) fn emit_thread_upsert(&self, key: &ThreadKey) {
        self.clear_thread_update_caches(key);
        let update = {
            let snapshot = self.snapshot.read().expect("app store lock poisoned");
            match project_thread_update(&snapshot, key) {
                Ok(Some((thread, session_summary, agent_directory_version))) => {
                    tracing::warn!(
                        target: "store",
                        server_id = key.server_id,
                        thread_id = key.thread_id,
                        item_count = thread.hydrated_conversation_items.len(),
                        active_turn = ?thread.active_turn_id,
                        "emit_thread_upsert"
                    );
                    Some(AppStoreUpdateRecord::ThreadUpserted {
                        thread,
                        session_summary,
                        agent_directory_version,
                    })
                }
                Ok(None) => None,
                Err(error) => {
                    tracing::error!(
                        target: "store",
                        server_id = key.server_id,
                        thread_id = key.thread_id,
                        %error,
                        "failed to project ThreadUpserted"
                    );
                    Some(AppStoreUpdateRecord::FullResync)
                }
            }
        };
        if let Some(update) = update {
            self.emit(update);
        }
    }

    pub(crate) fn emit_thread_item_changed(&self, key: &ThreadKey, item: HydratedConversationItem) {
        let item = {
            let snapshot = self.snapshot.read().expect("app store lock poisoned");
            project_hydrated_item(&snapshot, &key.server_id, &item)
        };
        let fingerprint = item_fingerprint(&item);
        let cache_key = (key.clone(), item.id.clone());
        {
            let mut cache = self
                .last_thread_item_upserts
                .write()
                .expect("thread item cache lock poisoned");
            if cache.get(&cache_key) == Some(&fingerprint) {
                return;
            }
            cache.insert(cache_key, fingerprint);
        }
        let session_summary = self.compute_session_summary(key);
        self.emit(AppStoreUpdateRecord::ThreadItemChanged {
            key: key.clone(),
            item,
            session_summary,
        });
    }

    /// Snapshot a single thread's `AppSessionSummary` for piggybacking on
    /// per-item events. Falls back to a minimal summary if the thread is
    /// gone by the time this runs (shouldn't happen in practice — emit
    /// sites hold the snapshot lock while deciding to emit).
    fn compute_session_summary(&self, key: &ThreadKey) -> AppSessionSummary {
        let snapshot = self.snapshot.read().expect("app store lock poisoned");
        let Some(thread) = snapshot.threads.get(key) else {
            // Placeholder empty summary — the matching thread has been
            // removed between the mutation and this read. Platform side
            // will get a ThreadRemoved event next and discard anyway.
            return empty_session_summary(key.clone());
        };
        app_session_summary(&snapshot, thread, snapshot.servers.get(&key.server_id))
    }

    pub(crate) fn emit_thread_item_changed_by_id(&self, key: &ThreadKey, item_id: &str) {
        let item = {
            let snapshot = self.snapshot.read().expect("app store lock poisoned");
            snapshot
                .threads
                .get(key)
                .and_then(|thread| thread.items.iter().find(|item| item.id == item_id).cloned())
        };
        if let Some(item) = item {
            self.emit_thread_item_changed(key, item);
        }
    }

    pub(crate) fn emit_thread_streaming_delta(
        &self,
        key: &ThreadKey,
        item_id: &str,
        kind: ThreadStreamingDeltaKind,
        text: &str,
    ) {
        self.emit(AppStoreUpdateRecord::ThreadStreamingDelta {
            key: key.clone(),
            item_id: item_id.to_string(),
            kind,
            text: text.to_string(),
        });
    }

    fn apply_item_update(&self, key: &ThreadKey, item: HydratedConversationItem) {
        let is_user_message = matches!(&item.content, HydratedConversationItemContent::User(_));
        let incoming_item_id = item.id.clone();
        let result = self.mutate_thread_with_result(key, |thread| {
            let existing = thread
                .items
                .iter()
                .find(|existing| existing.id == item.id)
                .cloned();
            let item = merge_reasoning_item_with_existing(existing.as_ref(), item);
            let active_turn_id = thread.active_turn_id.as_deref();
            let removed_overlay_ids = thread
                .local_overlay_items
                .iter()
                .filter(|existing| is_superseded_overlay_item(existing, &item, active_turn_id))
                .map(|existing| existing.id.clone())
                .collect::<Vec<_>>();
            thread
                .local_overlay_items
                .retain(|existing| !is_superseded_overlay_item(existing, &item, active_turn_id));
            let queued_count_before = thread.queued_follow_ups.len();
            let mutation = classify_item_mutation(existing.as_ref(), &item);
            let clears_queued_follow_up = item.is_from_user_turn_boundary
                && matches!(&item.content, HydratedConversationItemContent::User(_));
            upsert_item(thread, item);
            if clears_queued_follow_up {
                remove_first_queued_follow_up(thread);
            }
            (
                mutation,
                queued_count_before != thread.queued_follow_ups.len(),
                removed_overlay_ids,
            )
        });
        if result.is_none() && is_user_message {
            tracing::warn!(
                target: "store",
                server_id = key.server_id,
                thread_id = key.thread_id,
                item_id = incoming_item_id,
                "apply_item_update UserMessage skipped: thread not in store"
            );
        }

        match result {
            Some((Some(ItemMutationUpdate::Upsert(item)), queued_changed, removed_overlay_ids)) => {
                if !removed_overlay_ids.is_empty() {
                    self.emit_thread_upsert(key);
                } else {
                    if queued_changed {
                        self.emit_thread_metadata_changed(key);
                    }
                    // Route through `emit_thread_item_changed` so multi-agent
                    // target ids get projected to display labels (e.g.
                    // "child-thread" → "Scout [explorer]"). The direct
                    // emission used to skip the projector, leaving raw
                    // thread ids in the wire-bound record.
                    self.emit_thread_item_changed(key, item);
                }
            }
            Some((None, queued_changed, removed_overlay_ids)) => {
                if !removed_overlay_ids.is_empty() {
                    self.emit_thread_upsert(key);
                } else if queued_changed {
                    self.emit_thread_metadata_changed(key);
                }
            }
            None => {}
        }
    }

    fn emit(&self, update: AppStoreUpdateRecord) {
        match &update {
            AppStoreUpdateRecord::FullResync => tracing::debug!(target: "store", "emit FullResync"),
            AppStoreUpdateRecord::ServerChanged { server_id } => {
                tracing::debug!(target: "store", server_id, "emit ServerChanged")
            }
            AppStoreUpdateRecord::ServerRemoved { server_id } => {
                tracing::debug!(target: "store", server_id, "emit ServerRemoved")
            }
            AppStoreUpdateRecord::ThreadUpserted { thread, .. } => {
                tracing::debug!(
                    target: "store",
                    server_id = thread.key.server_id,
                    thread_id = thread.key.thread_id,
                    "emit ThreadUpserted"
                )
            }
            AppStoreUpdateRecord::ThreadMetadataChanged { state, .. } => {
                tracing::debug!(
                    target: "store",
                    server_id = state.key.server_id,
                    thread_id = state.key.thread_id,
                    "emit ThreadMetadataChanged"
                )
            }
            AppStoreUpdateRecord::ThreadItemChanged { key, item, .. } => {
                tracing::debug!(
                    target: "store",
                    server_id = key.server_id,
                    thread_id = key.thread_id,
                    item_id = item.id,
                    "emit ThreadItemChanged"
                )
            }
            AppStoreUpdateRecord::ThreadStreamingDelta {
                key, item_id, kind, ..
            } => {
                tracing::trace!(
                    target: "store",
                    server_id = key.server_id,
                    thread_id = key.thread_id,
                    item_id,
                    kind = ?kind,
                    "emit ThreadStreamingDelta"
                )
            }
            AppStoreUpdateRecord::ThreadRemoved { key, .. } => {
                tracing::debug!(target: "store", server_id = key.server_id, thread_id = key.thread_id, "emit ThreadRemoved")
            }
            AppStoreUpdateRecord::ActiveThreadChanged { key } => {
                tracing::debug!(target: "store", thread_id = ?key.as_ref().map(|k| &k.thread_id), "emit ActiveThreadChanged")
            }
            AppStoreUpdateRecord::PendingApprovalsChanged { approvals } => {
                tracing::debug!(target: "store", count = approvals.len(), "emit PendingApprovalsChanged")
            }
            AppStoreUpdateRecord::PendingUserInputsChanged { requests } => {
                tracing::debug!(target: "store", count = requests.len(), "emit PendingUserInputsChanged")
            }
            AppStoreUpdateRecord::VoiceSessionChanged => {
                tracing::debug!(target: "store", "emit VoiceSessionChanged")
            }
            AppStoreUpdateRecord::RealtimeTranscriptUpdated { key, .. } => {
                tracing::trace!(target: "store", server_id = key.server_id, thread_id = key.thread_id, "emit RealtimeTranscriptUpdated")
            }
            AppStoreUpdateRecord::RealtimeHandoffRequested { key, .. } => {
                tracing::debug!(target: "store", server_id = key.server_id, thread_id = key.thread_id, "emit RealtimeHandoffRequested")
            }
            AppStoreUpdateRecord::RealtimeSpeechStarted { key } => {
                tracing::debug!(target: "store", server_id = key.server_id, thread_id = key.thread_id, "emit RealtimeSpeechStarted")
            }
            AppStoreUpdateRecord::RealtimeStarted { key, .. } => {
                tracing::debug!(target: "store", server_id = key.server_id, thread_id = key.thread_id, "emit RealtimeStarted")
            }
            AppStoreUpdateRecord::RealtimeSdp { key, .. } => {
                tracing::debug!(target: "store", server_id = key.server_id, thread_id = key.thread_id, "emit RealtimeSdp")
            }
            AppStoreUpdateRecord::RealtimeOutputAudioDelta { .. } => {} // too noisy even for trace
            AppStoreUpdateRecord::RealtimeError { key, .. } => {
                tracing::warn!(target: "store", server_id = key.server_id, thread_id = key.thread_id, "emit RealtimeError")
            }
            AppStoreUpdateRecord::RealtimeClosed { key, .. } => {
                tracing::debug!(target: "store", server_id = key.server_id, thread_id = key.thread_id, "emit RealtimeClosed")
            }
            AppStoreUpdateRecord::SavedAppsChanged => {
                tracing::debug!(target: "store", "emit SavedAppsChanged")
            }
            AppStoreUpdateRecord::DynamicWidgetStreaming {
                key,
                item_id,
                call_id,
                widget,
            } => {
                tracing::trace!(
                    target: "store",
                    server_id = key.server_id,
                    thread_id = key.thread_id,
                    item_id,
                    call_id,
                    html_len = widget.widget_html.len(),
                    "emit DynamicWidgetStreaming"
                )
            }
            AppStoreUpdateRecord::TerminalSessionsChanged => {
                tracing::debug!(target: "store", "emit TerminalSessionsChanged")
            }
        }
        let _ = self.updates_tx.send(update);
    }

    /// Broadcast that the `saved_apps.rs` on-disk index mutated.
    /// Called by every saved-apps UniFFI mutation and by the
    /// finalize-time auto-upsert hook in `dynamic_tools.rs`. Platforms
    /// respond by re-reading via `saved_apps_list`.
    pub(crate) fn emit_saved_apps_changed(&self) {
        self.emit(AppStoreUpdateRecord::SavedAppsChanged);
    }

    /// Drop any streaming `DynamicToolCallArgumentsDelta` buffers that
    /// were accumulating for the given `(thread_key, item_id)`. Called
    /// from `ItemCompleted` for `DynamicToolCall` items, when the
    /// call has finalized on the server. Keyed by the buffer map's
    /// inner value (stored item_id), so we drop every buffer whose
    /// item_id matches — regardless of call_id.
    fn clear_dynamic_tool_arg_buffers_for_item(&self, thread_key: &ThreadKey, item_id: &str) {
        let mut guard = self
            .dynamic_tool_arg_buffers
            .write()
            .expect("app store dynamic_tool_arg_buffers poisoned");
        guard.retain(|(existing_key, _), buffer| {
            !(existing_key == thread_key && buffer.item_id == item_id)
        });
    }

    fn clear_dynamic_tool_arg_buffers_for_thread(&self, thread_key: &ThreadKey) {
        self.dynamic_tool_arg_buffers
            .write()
            .expect("app store dynamic_tool_arg_buffers poisoned")
            .retain(|(existing_key, _), _| existing_key != thread_key);
    }
}

impl Default for AppStoreReducer {
    fn default() -> Self {
        Self::new()
    }
}

fn upsert_item(
    thread: &mut ThreadSnapshot,
    item: crate::conversation_uniffi::HydratedConversationItem,
) {
    if let Some(existing) = thread
        .items
        .iter_mut()
        .find(|existing| existing.id == item.id)
    {
        *existing = item;
    } else {
        thread.items.push(item);
    }
}

fn merge_reasoning_item_with_existing(
    existing: Option<&HydratedConversationItem>,
    mut item: HydratedConversationItem,
) -> HydratedConversationItem {
    let Some(existing) = existing else {
        return item;
    };
    let (
        HydratedConversationItemContent::Reasoning(existing_reasoning),
        HydratedConversationItemContent::Reasoning(incoming_reasoning),
    ) = (&existing.content, &mut item.content)
    else {
        return item;
    };

    if incoming_reasoning
        .summary
        .iter()
        .all(|part| part.trim().is_empty())
        && incoming_reasoning
            .content
            .iter()
            .all(|part| part.trim().is_empty())
        && existing_reasoning
            .summary
            .iter()
            .chain(existing_reasoning.content.iter())
            .any(|part| !part.trim().is_empty())
    {
        incoming_reasoning.summary = existing_reasoning.summary.clone();
        incoming_reasoning.content = existing_reasoning.content.clone();
    }

    item
}

fn append_assistant_delta(thread: &mut ThreadSnapshot, item_id: &str, delta: &str) -> bool {
    let mut inserted_placeholder = false;
    if !thread.items.iter().any(|item| item.id == item_id) {
        thread.items.push(HydratedConversationItem {
            id: item_id.to_string(),
            content: HydratedConversationItemContent::Assistant(HydratedAssistantMessageData {
                text: String::new(),
                agent_nickname: None,
                agent_role: None,
                phase: None,
            }),
            source_turn_id: thread.active_turn_id.clone(),
            source_turn_index: None,
            timestamp: None,
            is_from_user_turn_boundary: false,
        });
        inserted_placeholder = true;
    }

    let Some(item) = thread.items.iter_mut().find(|item| item.id == item_id) else {
        return inserted_placeholder;
    };
    if let HydratedConversationItemContent::Assistant(message) = &mut item.content {
        message.text.push_str(delta);
    }
    inserted_placeholder
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LiveDeltaApplyResult {
    Streamed,
    InsertedPlaceholder,
    RepairedPlaceholder,
    Failed,
}

impl LiveDeltaApplyResult {
    fn streamed(self) -> bool {
        matches!(self, Self::Streamed)
    }

    fn requires_item_upsert(self) -> bool {
        matches!(self, Self::InsertedPlaceholder | Self::RepairedPlaceholder)
    }
}

fn append_reasoning_delta(
    thread: &mut ThreadSnapshot,
    item_id: &str,
    delta: &str,
) -> LiveDeltaApplyResult {
    match thread.items.iter().position(|item| item.id == item_id) {
        Some(index) => {
            let item = &mut thread.items[index];
            match &mut item.content {
                HydratedConversationItemContent::Reasoning(reasoning) => {
                    if let Some(last) = reasoning.content.last_mut() {
                        last.push_str(delta);
                    } else {
                        reasoning.content.push(delta.to_string());
                    }
                    LiveDeltaApplyResult::Streamed
                }
                _ => {
                    item.content =
                        HydratedConversationItemContent::Reasoning(HydratedReasoningData {
                            summary: Vec::new(),
                            content: vec![delta.to_string()],
                        });
                    if item.source_turn_id.is_none() {
                        item.source_turn_id = thread.active_turn_id.clone();
                    }
                    LiveDeltaApplyResult::RepairedPlaceholder
                }
            }
        }
        None => {
            thread.items.push(HydratedConversationItem {
                id: item_id.to_string(),
                content: HydratedConversationItemContent::Reasoning(HydratedReasoningData {
                    summary: Vec::new(),
                    content: vec![delta.to_string()],
                }),
                source_turn_id: thread.active_turn_id.clone(),
                source_turn_index: None,
                timestamp: None,
                is_from_user_turn_boundary: false,
            });
            LiveDeltaApplyResult::InsertedPlaceholder
        }
    }
}

fn append_plan_delta(
    thread: &mut ThreadSnapshot,
    item_id: &str,
    delta: &str,
) -> LiveDeltaApplyResult {
    match thread.items.iter().position(|item| item.id == item_id) {
        Some(index) => {
            let item = &mut thread.items[index];
            match &mut item.content {
                HydratedConversationItemContent::ProposedPlan(plan) => {
                    plan.content.push_str(delta);
                    LiveDeltaApplyResult::Streamed
                }
                _ => {
                    item.content =
                        HydratedConversationItemContent::ProposedPlan(HydratedProposedPlanData {
                            content: delta.to_string(),
                        });
                    if item.source_turn_id.is_none() {
                        item.source_turn_id = thread.active_turn_id.clone();
                    }
                    LiveDeltaApplyResult::RepairedPlaceholder
                }
            }
        }
        None => {
            thread.items.push(HydratedConversationItem {
                id: item_id.to_string(),
                content: HydratedConversationItemContent::ProposedPlan(HydratedProposedPlanData {
                    content: delta.to_string(),
                }),
                source_turn_id: thread.active_turn_id.clone(),
                source_turn_index: None,
                timestamp: None,
                is_from_user_turn_boundary: false,
            });
            LiveDeltaApplyResult::InsertedPlaceholder
        }
    }
}

fn append_command_output_delta(
    thread: &mut ThreadSnapshot,
    item_id: &str,
    delta: &str,
) -> LiveDeltaApplyResult {
    match thread.items.iter().position(|item| item.id == item_id) {
        Some(index) => {
            let item = &mut thread.items[index];
            match &mut item.content {
                HydratedConversationItemContent::CommandExecution(command) => {
                    let output = command.output.get_or_insert_with(String::new);
                    if !command_output_is_truncated(output) {
                        output.push_str(delta);
                        if output.len() > 128 * 1024 {
                            *output = truncate_command_output_text(output);
                        }
                    }
                    LiveDeltaApplyResult::Streamed
                }
                _ => {
                    item.content = HydratedConversationItemContent::CommandExecution(
                        HydratedCommandExecutionData {
                            command: String::new(),
                            cwd: String::new(),
                            status: AppOperationStatus::InProgress,
                            output: Some(truncate_command_output_text(delta)),
                            exit_code: None,
                            duration_ms: None,
                            process_id: None,
                            actions: Vec::new(),
                        },
                    );
                    if item.source_turn_id.is_none() {
                        item.source_turn_id = thread.active_turn_id.clone();
                    }
                    LiveDeltaApplyResult::RepairedPlaceholder
                }
            }
        }
        None => {
            thread.items.push(HydratedConversationItem {
                id: item_id.to_string(),
                content: HydratedConversationItemContent::CommandExecution(
                    HydratedCommandExecutionData {
                        command: String::new(),
                        cwd: String::new(),
                        status: AppOperationStatus::InProgress,
                        output: Some(truncate_command_output_text(delta)),
                        exit_code: None,
                        duration_ms: None,
                        process_id: None,
                        actions: Vec::new(),
                    },
                ),
                source_turn_id: thread.active_turn_id.clone(),
                source_turn_index: None,
                timestamp: None,
                is_from_user_turn_boundary: false,
            });
            LiveDeltaApplyResult::InsertedPlaceholder
        }
    }
}

fn append_mcp_progress(
    thread: &mut ThreadSnapshot,
    item_id: &str,
    message: &str,
) -> LiveDeltaApplyResult {
    match thread.items.iter().position(|item| item.id == item_id) {
        Some(index) => {
            let item = &mut thread.items[index];
            match &mut item.content {
                HydratedConversationItemContent::McpToolCall(call) => {
                    if !message.trim().is_empty() {
                        call.progress_messages.push(message.to_string());
                    }
                    LiveDeltaApplyResult::Streamed
                }
                _ => {
                    item.content =
                        HydratedConversationItemContent::McpToolCall(HydratedMcpToolCallData {
                            server: String::new(),
                            tool: String::new(),
                            status: AppOperationStatus::InProgress,
                            duration_ms: None,
                            arguments_json: None,
                            content_summary: None,
                            structured_content_json: None,
                            raw_output_json: None,
                            error_message: None,
                            progress_messages: if message.trim().is_empty() {
                                Vec::new()
                            } else {
                                vec![message.to_string()]
                            },
                            computer_use: None,
                        });
                    if item.source_turn_id.is_none() {
                        item.source_turn_id = thread.active_turn_id.clone();
                    }
                    LiveDeltaApplyResult::RepairedPlaceholder
                }
            }
        }
        None => {
            thread.items.push(HydratedConversationItem {
                id: item_id.to_string(),
                content: HydratedConversationItemContent::McpToolCall(HydratedMcpToolCallData {
                    server: String::new(),
                    tool: String::new(),
                    status: AppOperationStatus::InProgress,
                    duration_ms: None,
                    arguments_json: None,
                    content_summary: None,
                    structured_content_json: None,
                    raw_output_json: None,
                    error_message: None,
                    progress_messages: if message.trim().is_empty() {
                        Vec::new()
                    } else {
                        vec![message.to_string()]
                    },
                    computer_use: None,
                }),
                source_turn_id: thread.active_turn_id.clone(),
                source_turn_index: None,
                timestamp: None,
                is_from_user_turn_boundary: false,
            });
            LiveDeltaApplyResult::InsertedPlaceholder
        }
    }
}

mod realtime;
mod thread_merge;
use thread_merge::{
    LOCAL_USER_MESSAGE_ITEM_PREFIX, USER_INPUT_RESPONSE_ITEM_PREFIX, answered_user_input_item,
    duplicate_local_overlay_item_ids, is_duplicate_overlay_item, is_superseded_overlay_item,
    local_user_message_overlay_item, preserve_local_overlay_items, preserve_queued_follow_ups,
    preserve_thread_created_at, preserve_thread_fork_lineage, preserve_thread_preview,
    preserve_thread_runtime_state, preserve_thread_title, sync_thread_follow_up_projection,
};
pub(crate) use thread_merge::{
    remove_duplicate_local_overlay_items, remove_first_queued_follow_up,
};

mod terminal_state;
pub use terminal_state::TERMINAL_OUTPUT_TAIL_LIMIT;

#[cfg(test)]
mod tests;

fn appended_text_delta(existing: &str, projected: &str) -> Option<String> {
    projected
        .starts_with(existing)
        .then(|| projected[existing.len()..].to_string())
}

fn appended_optional_text_delta(
    existing: &Option<String>,
    projected: &Option<String>,
) -> Option<String> {
    match (existing.as_deref(), projected.as_deref()) {
        (None, None) => Some(String::new()),
        (None, Some(projected)) => Some(projected.to_string()),
        (Some(existing), Some(projected)) => appended_text_delta(existing, projected),
        (Some(_), None) => None,
    }
}

fn classify_item_mutation(
    existing: Option<&HydratedConversationItem>,
    item: &HydratedConversationItem,
) -> Option<ItemMutationUpdate> {
    let Some(existing) = existing else {
        return Some(ItemMutationUpdate::Upsert(item.clone()));
    };

    match (&existing.content, &item.content) {
        (
            HydratedConversationItemContent::CommandExecution(existing_data),
            HydratedConversationItemContent::CommandExecution(projected_data),
        ) => {
            if existing.id != item.id
                || existing.source_turn_id != item.source_turn_id
                || existing.source_turn_index != item.source_turn_index
                || existing.timestamp != item.timestamp
                || existing.is_from_user_turn_boundary != item.is_from_user_turn_boundary
                || existing_data.command != projected_data.command
                || existing_data.cwd != projected_data.cwd
                || existing_data.actions != projected_data.actions
            {
                return Some(ItemMutationUpdate::Upsert(item.clone()));
            }

            let output_delta =
                appended_optional_text_delta(&existing_data.output, &projected_data.output)?;
            let status_changed = existing_data.status != projected_data.status
                || existing_data.exit_code != projected_data.exit_code
                || existing_data.duration_ms != projected_data.duration_ms
                || existing_data.process_id != projected_data.process_id;
            if output_delta.is_empty() && !status_changed {
                None
            } else {
                Some(ItemMutationUpdate::Upsert(item.clone()))
            }
        }
        _ if existing.content == item.content => None,
        _ => Some(ItemMutationUpdate::Upsert(item.clone())),
    }
}

fn format_model_reroute_reason(reason: &codex_app_server_protocol::ModelRerouteReason) -> String {
    let raw = format!("{reason:?}");
    let mut formatted = String::new();
    for (index, ch) in raw.chars().enumerate() {
        if index > 0 && ch.is_uppercase() {
            formatted.push(' ');
        }
        formatted.push(ch);
    }
    formatted
}
