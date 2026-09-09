use std::any::Any;
use std::collections::HashSet;

use crate::MobileClient;
use crate::conversation_uniffi::{HydratedConversationItem, HydratedConversationItemContent};
use crate::store::{AppStoreReducer, ThreadSnapshot};
use crate::transport::RpcError;
use crate::types::server_requests::{AppListThreadTurnsResponse, AppTurnsSortDirection};
use crate::types::{AgentRuntimeKind, ThreadInfo, ThreadKey};
use codex_app_server_protocol as upstream;

impl MobileClient {
    /// Reconcile direct public RPC calls into the canonical app store.
    ///
    /// The public client RPC surface calls this hook after the upstream RPC
    /// returns. The reconciliation policy lives here:
    /// - snapshot/query RPCs reduce authoritative responses directly
    /// - mutations without authoritative payloads trigger targeted refreshes
    /// - event-complete RPCs are no-ops because upstream notifications drive
    ///   the reducer already
    pub async fn reconcile_public_rpc<P: Any, R: Any>(
        &self,
        wire_method: &str,
        server_id: &str,
        params: Option<&P>,
        response: &R,
    ) -> Result<(), RpcError> {
        if wire_method == "turn/start" {
            tracing::info!(
                "reconcile_public_rpc wire_method={} server_id={}",
                wire_method,
                server_id
            );
        }
        match wire_method {
            "thread/start" => {
                let response = downcast_public_rpc_response::<upstream::ThreadStartResponse>(
                    wire_method,
                    response,
                )?;
                self.apply_thread_start_response(server_id, response)
                    .map(|_| ())
                    .map_err(RpcError::Deserialization)
            }
            "thread/list" => {
                let response = downcast_public_rpc_response::<upstream::ThreadListResponse>(
                    wire_method,
                    response,
                )?;
                self.sync_thread_list(server_id, &response.data)
                    .map(|_| ())
                    .map_err(RpcError::Deserialization)
            }
            "thread/read" => {
                let response = downcast_public_rpc_response::<upstream::ThreadReadResponse>(
                    wire_method,
                    response,
                )?;
                self.apply_thread_read_response(server_id, response)
                    .map(|_| ())
                    .map_err(RpcError::Deserialization)
            }
            "thread/resume" => {
                let response = downcast_public_rpc_response::<upstream::ThreadResumeResponse>(
                    wire_method,
                    response,
                )?;
                self.apply_thread_resume_response(server_id, response)
                    .map(|_| ())
                    .map_err(RpcError::Deserialization)
            }
            "thread/fork" => {
                let response = downcast_public_rpc_response::<upstream::ThreadForkResponse>(
                    wire_method,
                    response,
                )?;
                self.apply_thread_fork_response(server_id, response)
                    .map(|_| ())
                    .map_err(RpcError::Deserialization)
            }
            "thread/rollback" => {
                let response = downcast_public_rpc_response::<upstream::ThreadRollbackResponse>(
                    wire_method,
                    response,
                )?;
                let params = downcast_public_rpc_params::<upstream::ThreadRollbackParams>(
                    wire_method,
                    params.map(|value| value as &dyn Any),
                )?;
                self.apply_thread_rollback_response(server_id, &params.thread_id, response)
                    .map(|_| ())
                    .map_err(RpcError::Deserialization)
            }
            "account/read" => {
                let response = downcast_public_rpc_response::<upstream::GetAccountResponse>(
                    wire_method,
                    response,
                )?;
                self.apply_account_response(server_id, response);
                Ok(())
            }
            "account/rateLimits/read" => {
                let response = downcast_public_rpc_response::<
                    upstream::GetAccountRateLimitsResponse,
                >(wire_method, response)?;
                // `account/rateLimits/read` is Codex-runtime specific upstream.
                self.apply_account_rate_limits_response(server_id, "codex".to_string(), response);
                Ok(())
            }
            "model/list" => {
                let response = downcast_public_rpc_response::<upstream::ModelListResponse>(
                    wire_method,
                    response,
                )?;
                self.apply_model_list_response(server_id, response);
                Ok(())
            }
            "account/login/start" => self.sync_server_account(server_id).await,
            "account/logout" => self.sync_server_account_after_logout(server_id).await,
            _ => Ok(()),
        }
    }

    pub(crate) fn clear_server_account(&self, server_id: &str) {
        self.app_store.update_server_account(server_id, None, false);
    }

    pub fn apply_account_response(&self, server_id: &str, response: &upstream::GetAccountResponse) {
        self.app_store.update_server_account(
            server_id,
            response.account.clone().map(Into::into),
            response.requires_openai_auth,
        );
    }

    pub fn apply_account_rate_limits_response(
        &self,
        server_id: &str,
        runtime_kind: AgentRuntimeKind,
        response: &upstream::GetAccountRateLimitsResponse,
    ) {
        self.app_store.update_server_rate_limits(
            server_id,
            runtime_kind,
            Some(response.rate_limits.clone().into()),
        );
    }

    pub fn apply_model_list_response(
        &self,
        server_id: &str,
        response: &upstream::ModelListResponse,
    ) {
        self.app_store.update_server_models(
            server_id,
            Some(response.data.iter().cloned().map(Into::into).collect()),
        );
    }

    pub fn sync_thread_list(
        &self,
        server_id: &str,
        threads: &[upstream::Thread],
    ) -> Result<Vec<ThreadInfo>, String> {
        let threads = threads
            .iter()
            .cloned()
            .filter_map(crate::thread_info_from_upstream_thread)
            .collect::<Vec<_>>();
        self.app_store.sync_thread_list(server_id, &threads);
        Ok(threads)
    }

    pub fn upsert_thread_list_page(
        &self,
        server_id: &str,
        threads: &[upstream::Thread],
    ) -> Vec<ThreadInfo> {
        self.upsert_thread_list_page_for_runtime(server_id, "codex".to_string(), threads)
    }

    pub fn upsert_thread_list_page_for_runtime(
        &self,
        server_id: &str,
        runtime_kind: AgentRuntimeKind,
        threads: &[upstream::Thread],
    ) -> Vec<ThreadInfo> {
        let threads = threads
            .iter()
            .cloned()
            .filter_map(crate::thread_info_from_upstream_thread)
            .collect::<Vec<_>>();
        self.app_store
            .upsert_thread_list_page_for_runtime(server_id, runtime_kind, &threads);
        threads
    }

    pub fn finalize_thread_list_sync(
        &self,
        server_id: &str,
        thread_ids: impl IntoIterator<Item = String>,
    ) {
        let incoming_ids = thread_ids.into_iter().collect();
        self.app_store
            .finalize_thread_list_sync(server_id, &incoming_ids);
    }

    pub(crate) async fn sync_server_account_after_logout(
        &self,
        server_id: &str,
    ) -> Result<(), RpcError> {
        match self.sync_server_account(server_id).await {
            Ok(()) => Ok(()),
            Err(error) => {
                self.clear_server_account(server_id);
                Err(error)
            }
        }
    }

    pub fn apply_thread_start_response(
        &self,
        server_id: &str,
        response: &upstream::ThreadStartResponse,
    ) -> Result<ThreadKey, String> {
        let mut snapshot = crate::thread_snapshot_from_upstream_thread_with_overrides(
            server_id,
            response.thread.clone(),
            Some(response.model.clone()),
            response
                .reasoning_effort
                .map(Into::into)
                .map(crate::reasoning_effort_string),
            Some(response.approval_policy.into()),
            Some(response.sandbox.clone().into()),
        )
        .map_err(|e| e.to_string())?;
        // A freshly-started thread has no turns to page; mark the initial
        // page as loaded so UI does not auto-fire `thread/turns/list`
        // (which the server rejects until the first user message lands).
        snapshot.initial_turns_loaded = true;
        snapshot.older_turns_cursor = None;
        let key = snapshot.key.clone();
        let existing = self.app_store.thread_snapshot(&key);
        crate::reconcile_active_turn(existing.as_ref(), &mut snapshot, &response.thread.turns);
        self.app_store.upsert_thread_snapshot(snapshot);
        Ok(key)
    }

    pub fn apply_thread_read_response(
        &self,
        server_id: &str,
        response: &upstream::ThreadReadResponse,
    ) -> Result<ThreadKey, String> {
        let mut snapshot = crate::thread_snapshot_from_upstream_thread_with_overrides(
            server_id,
            response.thread.clone(),
            None,
            None,
            response.approval_policy.map(Into::into),
            response.sandbox.clone().map(Into::into),
        )
        .map_err(|e| e.to_string())?;
        let key = snapshot.key.clone();
        let existing = self.app_store.thread_snapshot(&key);
        // Share the preserve-on-empty merge with resume/fork. A paginated
        // v0.125+ server returns `thread.turns: []` on thread/read — we
        // must keep any items + `older_turns_cursor` the prior
        // `load_thread_turns_page` stored. A legacy (or authoritative)
        // response with embedded turns clears the cursor because the
        // embedded list is the full history.
        apply_pagination_merge(existing.as_ref(), &mut snapshot, &response.thread.turns);
        // thread/read is authoritative for `initial_turns_loaded`: if the
        // server returned no turns AND no prior state exists, treat the
        // thread as having no history rather than a pending page load, so
        // the iOS spinner doesn't stick (task #10 invariant).
        if existing.is_none() && response.thread.turns.is_empty() {
            snapshot.initial_turns_loaded = true;
        }
        crate::reconcile_active_turn(existing.as_ref(), &mut snapshot, &response.thread.turns);
        self.app_store.upsert_thread_snapshot(snapshot);
        Ok(key)
    }

    pub fn apply_thread_resume_response(
        &self,
        server_id: &str,
        response: &upstream::ThreadResumeResponse,
    ) -> Result<ThreadKey, String> {
        let mut snapshot = crate::thread_snapshot_from_upstream_thread_with_overrides(
            server_id,
            response.thread.clone(),
            Some(response.model.clone()),
            response
                .reasoning_effort
                .map(Into::into)
                .map(crate::reasoning_effort_string),
            Some(response.approval_policy.into()),
            Some(response.sandbox.clone().into()),
        )
        .map_err(|e| e.to_string())?;
        let key = snapshot.key.clone();
        let existing = self.app_store.thread_snapshot(&key);
        apply_pagination_merge(existing.as_ref(), &mut snapshot, &response.thread.turns);
        crate::reconcile_active_turn(existing.as_ref(), &mut snapshot, &response.thread.turns);
        self.app_store.upsert_thread_snapshot(snapshot);
        Ok(key)
    }

    pub fn apply_thread_fork_response(
        &self,
        server_id: &str,
        response: &upstream::ThreadForkResponse,
    ) -> Result<ThreadKey, String> {
        let mut snapshot = crate::thread_snapshot_from_upstream_thread_with_overrides(
            server_id,
            response.thread.clone(),
            Some(response.model.clone()),
            response
                .reasoning_effort
                .map(Into::into)
                .map(crate::reasoning_effort_string),
            Some(response.approval_policy.into()),
            Some(response.sandbox.clone().into()),
        )
        .map_err(|e| e.to_string())?;
        let key = snapshot.key.clone();
        let existing = self.app_store.thread_snapshot(&key);
        apply_pagination_merge(existing.as_ref(), &mut snapshot, &response.thread.turns);
        crate::reconcile_active_turn(existing.as_ref(), &mut snapshot, &response.thread.turns);
        self.app_store.upsert_thread_snapshot(snapshot);
        Ok(key)
    }

    /// Merge a paged `thread/turns/list` response into the canonical thread
    /// snapshot.
    ///
    /// `direction` matches the `sortDirection` the client sent with the
    /// request. For `Descending` (newest-first) pages the hydrated turns
    /// are in newest-first order; the store keeps items in chronological
    /// (ascending) order, so this reverses per-page before merging. Turns
    /// already in the store (by `source_turn_id`) are deduped.
    pub fn apply_thread_turns_page(
        &self,
        server_id: &str,
        thread_id: &str,
        page: &AppListThreadTurnsResponse,
        direction: AppTurnsSortDirection,
    ) -> Result<(), String> {
        self.apply_thread_turns_pages(server_id, thread_id, std::iter::once(page), direction)
    }

    pub(crate) fn apply_thread_turns_pages<'a>(
        &self,
        server_id: &str,
        thread_id: &str,
        pages: impl IntoIterator<Item = &'a AppListThreadTurnsResponse>,
        direction: AppTurnsSortDirection,
    ) -> Result<(), String> {
        let key = ThreadKey {
            server_id: server_id.to_string(),
            thread_id: thread_id.to_string(),
        };
        self.app_store
            .apply_thread_turns_pages(&key, None, pages, direction)
            .map(|_| ())
    }

    pub fn apply_thread_rollback_response(
        &self,
        server_id: &str,
        thread_id: &str,
        response: &upstream::ThreadRollbackResponse,
    ) -> Result<ThreadKey, String> {
        let key = ThreadKey {
            server_id: server_id.to_string(),
            thread_id: thread_id.to_string(),
        };
        let current = self.app_store.thread_snapshot(&key);
        let mut snapshot = crate::thread_snapshot_from_upstream_thread_with_overrides(
            server_id,
            response.thread.clone(),
            current.as_ref().and_then(|thread| thread.model.clone()),
            current.as_ref().and_then(|thread| {
                thread
                    .reasoning_effort
                    .as_deref()
                    .and_then(crate::reasoning_effort_from_string)
                    .map(crate::reasoning_effort_string)
            }),
            current
                .as_ref()
                .and_then(|thread| thread.effective_approval_policy.clone()),
            current
                .as_ref()
                .and_then(|thread| thread.effective_sandbox_policy.clone()),
        )
        .map_err(|e| e.to_string())?;
        if let Some(current) = current.as_ref() {
            crate::copy_thread_runtime_fields(current, &mut snapshot);
            crate::reconcile_active_turn(Some(current), &mut snapshot, &response.thread.turns);
        }
        let next_key = snapshot.key.clone();
        self.app_store.replace_thread_history(snapshot);
        Ok(next_key)
    }
}

impl AppStoreReducer {
    pub(crate) fn apply_thread_turns_pages<'a>(
        &self,
        key: &ThreadKey,
        expected_epoch: Option<&std::sync::Arc<()>>,
        pages: impl IntoIterator<Item = &'a AppListThreadTurnsResponse>,
        direction: AppTurnsSortDirection,
    ) -> Result<bool, String> {
        self.apply_thread_turns_pages_preserving(key, expected_epoch, None, pages, direction)
    }

    pub(crate) fn apply_thread_turns_pages_preserving<'a>(
        &self,
        key: &ThreadKey,
        expected_epoch: Option<&std::sync::Arc<()>>,
        dispatched_items: Option<&std::collections::HashMap<String, u64>>,
        pages: impl IntoIterator<Item = &'a AppListThreadTurnsResponse>,
        direction: AppTurnsSortDirection,
    ) -> Result<bool, String> {
        self.mutate_thread_history(key, expected_epoch, |thread| {
            let protected_items = thread
                .items
                .iter()
                .filter(|item| {
                    dispatched_items.is_some_and(|items| {
                        items.get(&item.id) != Some(&super::reducer::item_fingerprint(item))
                    })
                })
                .map(|item| item.id.clone())
                .collect();
            for page in pages {
                merge_paged_turns_preserving(thread, page, direction, &protected_items);
            }
            tracing::info!(
                target: "store",
                server_id = key.server_id,
                thread_id = key.thread_id,
                item_count = thread.items.len(),
                older_turns_cursor = thread.older_turns_cursor.as_deref().unwrap_or(""),
                initial_turns_loaded = thread.initial_turns_loaded,
                "apply_thread_turns_page merged"
            );
        })
    }
}

/// Decide how a resume/fork response's embedded `thread.turns` field maps
/// into the store's existing items list.
///
/// v0.125+ servers honor `exclude_turns: true` by returning an empty turns
/// array; we must preserve the store's existing hydrated items so the UI
/// does not flicker to empty while pagination loads the first page. Legacy
/// servers ignore `exclude_turns` and return the embedded turns — we treat
/// those as an authoritative hydration.
fn apply_pagination_merge(
    existing: Option<&ThreadSnapshot>,
    target: &mut ThreadSnapshot,
    upstream_turns: &[upstream::Turn],
) {
    if upstream_turns.is_empty() {
        if let Some(current) = existing {
            target.items = current.items.clone();
            target.older_turns_cursor = current.older_turns_cursor.clone();
            target.initial_turns_loaded = current.initial_turns_loaded;
        } else {
            target.initial_turns_loaded = false;
            target.older_turns_cursor = None;
        }
    } else {
        // Legacy remote (or explicit hydration): the response carries the
        // full turn history. Authoritative — paged cursor no longer applies.
        target.initial_turns_loaded = true;
        target.older_turns_cursor = None;
    }
}

fn user_replay_item_key(item: &HydratedConversationItem) -> Option<String> {
    match &item.content {
        HydratedConversationItemContent::User(data) => Some(format!(
            "user:{}:{}",
            data.text,
            serde_json::to_string(&data.image_data_uris).unwrap_or_default()
        )),
        _ => None,
    }
}

fn logical_replay_item_key(item: &HydratedConversationItem) -> Option<String> {
    match &item.content {
        HydratedConversationItemContent::User(_) => user_replay_item_key(item),
        HydratedConversationItemContent::Assistant(data) => {
            Some(format!("assistant:{}:{:?}", data.text, data.phase))
        }
        HydratedConversationItemContent::Reasoning(data) => Some(format!(
            "reasoning:{}:{}",
            serde_json::to_string(&data.summary).unwrap_or_default(),
            serde_json::to_string(&data.content).unwrap_or_default()
        )),
        _ => None,
    }
}

fn is_stream_text_item(item: &HydratedConversationItem) -> bool {
    matches!(
        item.content,
        HydratedConversationItemContent::Assistant(_)
            | HydratedConversationItemContent::Reasoning(_)
    )
}

fn prune_replayed_live_span(
    thread: &mut ThreadSnapshot,
    group_user_keys: &[String],
    incoming_item_ids: &HashSet<String>,
    protected_items: &HashSet<String>,
) {
    if group_user_keys.is_empty() {
        return;
    }

    let mut in_replayed_live_span = false;
    thread.items.retain(|item| {
        if protected_items.contains(&item.id) {
            return true;
        }
        if let Some(key) = user_replay_item_key(item) {
            if group_user_keys.contains(&key) {
                if item.source_turn_id.is_none() {
                    in_replayed_live_span = true;
                    return false;
                }
                in_replayed_live_span = incoming_item_ids.contains(&item.id);
            } else {
                in_replayed_live_span = false;
            }
            return true;
        }

        if in_replayed_live_span && item.source_turn_id.is_none() && is_stream_text_item(item) {
            return false;
        }

        true
    });
}

fn replace_existing_items_by_id(
    thread: &mut ThreadSnapshot,
    incoming: impl IntoIterator<Item = HydratedConversationItem>,
    protected_items: &HashSet<String>,
) -> HashSet<String> {
    let mut replaced = HashSet::new();
    for item in incoming {
        if let Some(existing) = thread
            .items
            .iter_mut()
            .find(|existing| existing.id == item.id)
        {
            replaced.insert(item.id.clone());
            if !protected_items.contains(&item.id) {
                *existing = item;
            }
        }
    }
    replaced
}

#[cfg(test)]
fn merge_paged_turns(
    thread: &mut ThreadSnapshot,
    page: &AppListThreadTurnsResponse,
    direction: AppTurnsSortDirection,
) {
    merge_paged_turns_preserving(thread, page, direction, &HashSet::new());
}

pub(super) fn merge_paged_turns_preserving(
    thread: &mut ThreadSnapshot,
    page: &AppListThreadTurnsResponse,
    direction: AppTurnsSortDirection,
    protected_items: &HashSet<String>,
) {
    // Items within a single paged turn come in hydrated order (ascending).
    // When the server returns a Desc page (newest-first) we receive turns in
    // reverse chronological order, so we need to group by source_turn_id,
    // reverse the turn-order, then flatten.
    let mut turns_in_page: Vec<Vec<HydratedConversationItem>> = Vec::new();
    let mut current_turn_id: Option<String> = None;
    for item in &page.turns {
        let item_turn = item.source_turn_id.clone();
        if item_turn != current_turn_id {
            turns_in_page.push(Vec::new());
            current_turn_id = item_turn;
        }
        if let Some(group) = turns_in_page.last_mut() {
            group.push(item.clone());
        }
    }

    if matches!(direction, AppTurnsSortDirection::Descending) {
        // Desc page: newest turn first — reverse to get chronological order.
        turns_in_page.reverse();
    }

    // Build sets of existing turn ids and item ids already in the store for
    // dedupe. Item-id dedupe is essential because live ItemStarted/Completed
    // events hydrate with `source_turn_id: None` (see
    // `conversation_item_from_upstream` in store/actions.rs), so a paged turn
    // carrying the same upstream item id won't share a turn id with the
    // already-stored copy and would otherwise be added twice.
    let existing_turn_ids: HashSet<String> = thread
        .items
        .iter()
        .filter_map(|item| item.source_turn_id.clone())
        .collect();
    let existing_item_ids: HashSet<String> =
        thread.items.iter().map(|item| item.id.clone()).collect();

    let mut new_items: Vec<HydratedConversationItem> = Vec::new();
    for group in turns_in_page {
        let group_turn_id = group.first().and_then(|item| item.source_turn_id.clone());
        let incoming_item_ids: HashSet<String> = group.iter().map(|item| item.id.clone()).collect();

        // Preferred path for runtime bridges: live stream items and
        // later replay items carry the same upstream item id. Replace the
        // sourceless live copy with the authoritative paged copy so metadata
        // such as `source_turn_id` is repaired without any content guessing.
        let replaced_item_ids =
            replace_existing_items_by_id(thread, group.iter().cloned(), protected_items);

        let group_user_keys = group
            .iter()
            .filter_map(user_replay_item_key)
            .collect::<Vec<_>>();
        let group_replays_existing_user = group_user_keys.iter().any(|key| {
            thread
                .items
                .iter()
                .filter_map(user_replay_item_key)
                .any(|existing_key| existing_key == *key)
        });
        let group_has_persisted_text = group.iter().any(|item| {
            item.source_turn_id.is_some()
                && matches!(
                    item.content,
                    HydratedConversationItemContent::Assistant(_)
                        | HydratedConversationItemContent::Reasoning(_)
                )
        });
        if let Some(id) = group_turn_id.as_deref()
            && (existing_turn_ids.contains(id) || !replaced_item_ids.is_empty())
        {
            // A reconnect repair page is authoritative for completed turn
            // text. Drop stale streaming assistant/reasoning placeholders
            // absent from the replay, while preserving the historical
            // turn-id dedupe for non-stream/user items.
            if thread.active_turn_id.is_none()
                && group_replays_existing_user
                && group_has_persisted_text
            {
                prune_replayed_live_span(
                    thread,
                    &group_user_keys,
                    &incoming_item_ids,
                    protected_items,
                );
                thread.items.retain(|item| {
                    protected_items.contains(&item.id)
                        || incoming_item_ids.contains(&item.id)
                        || !is_stream_text_item(item)
                        || item.source_turn_id.as_deref() != Some(id)
                });
            }
            // A known turn can be incomplete after transport loss. Merge
            // new replay items between existing anchors, not into the
            // older-turn prepend buffer, preserving the page's item order.
            let mut insertion_index = thread
                .items
                .iter()
                .position(|item| {
                    item.source_turn_id.as_deref() == Some(id)
                        || incoming_item_ids.contains(&item.id)
                })
                .unwrap_or(thread.items.len());
            // Legacy aliases are only eligible inside this anchored turn;
            // identical text in another turn is not replay evidence.
            let mut span_start = insertion_index;
            if thread
                .items
                .get(span_start)
                .is_some_and(|item| user_replay_item_key(item).is_none())
            {
                while span_start > 0 && thread.items[span_start - 1].source_turn_id.is_none() {
                    span_start -= 1;
                    if user_replay_item_key(&thread.items[span_start]).is_some() {
                        break;
                    }
                }
            }
            let mut span_end = thread
                .items
                .iter()
                .rposition(|item| {
                    item.source_turn_id.as_deref() == Some(id)
                        || incoming_item_ids.contains(&item.id)
                })
                .map_or(span_start, |index| index + 1);
            while thread.items.get(span_end).is_some_and(|item| {
                item.source_turn_id.is_none() && user_replay_item_key(item).is_none()
            }) {
                span_end += 1;
            }
            let sourceless_alias_ids: HashSet<String> = thread.items[span_start..span_end]
                .iter()
                .filter(|item| item.source_turn_id.is_none() && !protected_items.contains(&item.id))
                .map(|item| item.id.clone())
                .collect();
            let mut matched_aliases = HashSet::new();
            for item in group {
                let logical_key = logical_replay_item_key(&item);
                let existing_index = thread
                    .items
                    .iter()
                    .position(|existing| existing.id == item.id)
                    .or_else(|| {
                        thread.items.iter().position(|existing| {
                            (existing.source_turn_id.as_deref() == Some(id)
                                || sourceless_alias_ids.contains(&existing.id))
                                && !incoming_item_ids.contains(&existing.id)
                                && !protected_items.contains(&existing.id)
                                && !matched_aliases.contains(&existing.id)
                                && logical_key.is_some()
                                && logical_replay_item_key(existing) == logical_key
                        })
                    });
                if let Some(index) = existing_index {
                    matched_aliases.insert(thread.items[index].id.clone());
                    if sourceless_alias_ids.contains(&thread.items[index].id) {
                        thread.items[index] = item;
                    }
                    insertion_index = index + 1;
                } else {
                    thread.items.insert(insertion_index, item);
                    insertion_index += 1;
                }
            }
            continue;
        }
        if thread.active_turn_id.is_none()
            && group_replays_existing_user
            && group_has_persisted_text
        {
            prune_replayed_live_span(
                thread,
                &group_user_keys,
                &incoming_item_ids,
                protected_items,
            );
        }
        for item in group {
            if existing_item_ids.contains(&item.id) || replaced_item_ids.contains(&item.id) {
                continue;
            }
            // Compatibility path for older/non-Codex bridges that synthesized
            // optimistic/live item ids before the underlying agent persisted
            // history. A later `thread/turns/list` can return the same logical
            // content with a different persisted id. Prefer the replay copy
            // when it carries a turn id, and avoid duplicate user / assistant /
            // reasoning bubbles after reconnect repair pages.
            if let Some(key) = logical_replay_item_key(&item)
                && let Some(existing) = thread.items.iter_mut().find(|existing| {
                    !protected_items.contains(&existing.id)
                        && existing.source_turn_id.is_none()
                        && logical_replay_item_key(existing).as_deref() == Some(&key)
                })
            {
                if item.source_turn_id.is_some() && existing.source_turn_id.is_none() {
                    *existing = item;
                }
                continue;
            }
            new_items.push(item);
        }
    }

    // For Desc direction the `next_cursor` points at older turns; prepend new
    // items before existing ones since our store is chronological ascending.
    // For Asc direction (future use) append.
    if matches!(direction, AppTurnsSortDirection::Descending) {
        let mut merged = new_items;
        merged.extend(thread.items.iter().cloned());
        thread.items = merged;
        thread.older_turns_cursor = page.next_cursor.clone();
    } else {
        thread.items.extend(new_items);
    }
    thread.initial_turns_loaded = true;
}

fn downcast_public_rpc_response<'a, T: Any>(
    wire_method: &str,
    response: &'a dyn Any,
) -> Result<&'a T, RpcError> {
    response.downcast_ref::<T>().ok_or_else(|| {
        RpcError::Deserialization(format!(
            "unexpected response type while reconciling {wire_method}"
        ))
    })
}

fn downcast_public_rpc_params<'a, T: Any>(
    wire_method: &str,
    params: Option<&'a dyn Any>,
) -> Result<&'a T, RpcError> {
    params
        .and_then(|value| value.downcast_ref::<T>())
        .ok_or_else(|| {
            RpcError::Deserialization(format!(
                "unexpected params type while reconciling {wire_method}"
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::connection::ServerConfig;
    use crate::store::ServerHealthSnapshot;
    use codex_app_server_protocol as upstream;
    use std::path::PathBuf;

    fn test_abs_path(path: &str) -> codex_utils_absolute_path::AbsolutePathBuf {
        codex_utils_absolute_path::AbsolutePathBuf::from_absolute_path_checked(path)
            .expect("test path must be absolute")
    }

    fn test_upstream_thread(id: &str) -> upstream::Thread {
        upstream::Thread {
            id: id.to_string(),
            session_id: format!("session-{id}"),
            forked_from_id: None,
            preview: "hello".to_string(),
            ephemeral: false,
            model_provider: "openai".to_string(),
            created_at: 1,
            updated_at: 2,
            status: upstream::ThreadStatus::Idle,
            path: Some(PathBuf::from("/tmp/thread.jsonl")),
            cwd: test_abs_path("/tmp"),
            cli_version: "1.0.0".to_string(),
            source: upstream::SessionSource::default(),
            thread_source: None,
            agent_nickname: None,
            agent_role: None,
            git_info: None,
            name: Some("Thread".to_string()),
            turns: Vec::new(),
        }
    }

    #[tokio::test]
    async fn account_read_reconciliation_updates_store() {
        let client = MobileClient::new();
        client.app_store.upsert_server(
            &ServerConfig {
                server_id: "srv".into(),
                display_name: "Server".into(),
                host: "127.0.0.1".into(),
                port: 9234,
                websocket_url: None,
                is_local: true,
                tls: false,
            },
            ServerHealthSnapshot::Connected,
        );

        let response = upstream::GetAccountResponse {
            account: Some(upstream::Account::Chatgpt {
                email: "user@example.com".into(),
                plan_type: codex_protocol::account::PlanType::Pro,
            }),
            requires_openai_auth: true,
        };

        client
            .reconcile_public_rpc("account/read", "srv", Option::<&()>::None, &response)
            .await
            .expect("account/read reconciliation should succeed");

        let snapshot = client.app_snapshot();
        let server = snapshot
            .servers
            .get("srv")
            .expect("server should still exist");
        assert_eq!(server.account, response.account.clone().map(Into::into));
        assert!(server.requires_openai_auth);
    }

    #[tokio::test]
    async fn account_rate_limits_reconciliation_updates_store() {
        let client = MobileClient::new();
        client.app_store.upsert_server(
            &ServerConfig {
                server_id: "srv".to_string(),
                display_name: "Server".to_string(),
                host: "localhost".to_string(),
                port: 8390,
                websocket_url: None,
                is_local: true,
                tls: false,
            },
            ServerHealthSnapshot::Connected,
        );

        let response = upstream::GetAccountRateLimitsResponse {
            rate_limits: upstream::RateLimitSnapshot {
                limit_id: Some("primary".to_string()),
                limit_name: Some("Primary".to_string()),
                primary: Some(upstream::RateLimitWindow {
                    used_percent: 42,
                    window_duration_mins: Some(60),
                    resets_at: Some(123456789),
                }),
                secondary: None,
                credits: Some(upstream::CreditsSnapshot {
                    has_credits: true,
                    unlimited: false,
                    balance: Some("5.00".to_string()),
                }),
                plan_type: Some(codex_protocol::account::PlanType::Plus),
                rate_limit_reached_type: None,
            },
            rate_limits_by_limit_id: None,
        };

        client
            .reconcile_public_rpc(
                "account/rateLimits/read",
                "srv",
                Option::<&()>::None,
                &response,
            )
            .await
            .expect("account/rateLimits/read reconciliation should succeed");

        let snapshot = client.app_snapshot();
        let server = snapshot
            .servers
            .get("srv")
            .expect("server snapshot should exist");
        assert_eq!(
            server.rate_limits,
            Some(response.rate_limits.clone().into())
        );
    }

    #[tokio::test]
    async fn model_list_reconciliation_updates_store() {
        let client = MobileClient::new();
        client.app_store.upsert_server(
            &ServerConfig {
                server_id: "srv".to_string(),
                display_name: "Server".to_string(),
                host: "localhost".to_string(),
                port: 8390,
                websocket_url: None,
                is_local: true,
                tls: false,
            },
            ServerHealthSnapshot::Connected,
        );

        let response = upstream::ModelListResponse {
            data: vec![upstream::Model {
                id: "gpt-5.4".to_string(),
                model: "gpt-5.4".to_string(),
                upgrade: None,
                display_name: "gpt-5.4".to_string(),
                description: "Balanced flagship".to_string(),
                hidden: false,
                supported_reasoning_efforts: vec![upstream::ReasoningEffortOption {
                    reasoning_effort: codex_protocol::openai_models::ReasoningEffort::Medium,
                    description: "Balanced".to_string(),
                }],
                default_reasoning_effort: codex_protocol::openai_models::ReasoningEffort::Medium,
                input_modalities: vec![codex_protocol::openai_models::InputModality::Text],
                supports_personality: true,
                additional_speed_tiers: Vec::new(),
                service_tiers: Vec::new(),
                is_default: true,
                availability_nux: None,
                upgrade_info: None,
            }],
            next_cursor: None,
        };

        client
            .reconcile_public_rpc("model/list", "srv", Option::<&()>::None, &response)
            .await
            .expect("model/list reconciliation should succeed");

        let snapshot = client.app_snapshot();
        let server = snapshot
            .servers
            .get("srv")
            .expect("server snapshot should exist");
        assert_eq!(
            server.available_models,
            Some(response.data.into_iter().map(Into::into).collect())
        );
    }

    #[tokio::test]
    async fn thread_reconciliation_param_handling_matches_wire_method() {
        let client = MobileClient::new();
        client.app_store.upsert_server(
            &ServerConfig {
                server_id: "srv".to_string(),
                display_name: "Server".to_string(),
                host: "localhost".to_string(),
                port: 8390,
                websocket_url: None,
                is_local: true,
                tls: false,
            },
            ServerHealthSnapshot::Connected,
        );

        let list_response = upstream::ThreadListResponse {
            data: vec![test_upstream_thread("thread-1")],
            next_cursor: None,
            backwards_cursor: None,
        };

        client
            .reconcile_public_rpc("thread/list", "srv", Option::<&()>::None, &list_response)
            .await
            .expect("thread/list reconciliation should succeed without params");

        let rollback_response = upstream::ThreadRollbackResponse {
            thread: test_upstream_thread("thread-1"),
        };

        let missing_params_error = client
            .reconcile_public_rpc(
                "thread/rollback",
                "srv",
                Option::<&()>::None,
                &rollback_response,
            )
            .await
            .expect_err("thread/rollback should reject missing params");
        assert!(
            missing_params_error
                .to_string()
                .contains("unexpected params type while reconciling thread/rollback")
        );

        let params = upstream::ThreadRollbackParams {
            thread_id: "thread-1".to_string(),
            num_turns: 1,
        };
        client
            .reconcile_public_rpc("thread/rollback", "srv", Some(&params), &rollback_response)
            .await
            .expect("thread/rollback reconciliation should succeed with params");

        let snapshot = client.app_snapshot();
        assert!(snapshot.threads.contains_key(&ThreadKey {
            server_id: "srv".to_string(),
            thread_id: "thread-1".to_string(),
        }));
    }

    fn item_with_turn(turn_id: &str, item_id: &str) -> HydratedConversationItem {
        use crate::conversation_uniffi::{
            HydratedConversationItemContent, HydratedUserMessageData,
        };
        HydratedConversationItem {
            id: item_id.to_string(),
            content: HydratedConversationItemContent::User(HydratedUserMessageData {
                text: "hi".to_string(),
                image_data_uris: Vec::new(),
            }),
            source_turn_id: Some(turn_id.to_string()),
            source_turn_index: None,
            timestamp: None,
            is_from_user_turn_boundary: false,
        }
    }

    fn assistant_item(
        turn_id: Option<&str>,
        item_id: &str,
        text: &str,
    ) -> HydratedConversationItem {
        use crate::conversation_uniffi::{
            HydratedAssistantMessageData, HydratedConversationItemContent,
        };
        HydratedConversationItem {
            id: item_id.to_string(),
            content: HydratedConversationItemContent::Assistant(HydratedAssistantMessageData {
                text: text.to_string(),
                agent_nickname: None,
                agent_role: None,
                phase: None,
            }),
            source_turn_id: turn_id.map(ToOwned::to_owned),
            source_turn_index: None,
            timestamp: None,
            is_from_user_turn_boundary: false,
        }
    }

    fn test_thread_snapshot() -> ThreadSnapshot {
        let info = ThreadInfo {
            id: "thread-1".to_string(),
            title: None,
            model: None,
            status: crate::types::ThreadSummaryStatus::Idle,
            preview: None,
            cwd: None,
            path: None,
            model_provider: None,
            agent_nickname: None,
            agent_role: None,
            parent_thread_id: None,
            forked_from_id: None,
            agent_status: None,
            created_at: None,
            updated_at: None,
        };
        ThreadSnapshot::from_info("srv", info)
    }

    #[tokio::test]
    async fn rollback_rejects_old_pages_and_current_pages_keep_live_state() {
        let client = MobileClient::new();
        let mut thread = test_thread_snapshot();
        let key = thread.key.clone();
        thread.items = vec![item_with_turn("removed-turn", "removed-item")];
        thread.older_turns_cursor = Some("before-rollback".to_string());
        client.app_store.upsert_thread_snapshot(thread);
        let old_epoch = client
            .app_store
            .thread_history_epoch(&key)
            .expect("history epoch");
        client
            .apply_thread_rollback_response(
                &key.server_id,
                &key.thread_id,
                &upstream::ThreadRollbackResponse {
                    thread: test_upstream_thread(&key.thread_id),
                },
            )
            .expect("rollback to empty history");
        let rolled_back = client.app_store.thread_snapshot(&key).unwrap();
        assert!(rolled_back.items.is_empty());
        assert!(rolled_back.initial_turns_loaded);
        assert!(rolled_back.older_turns_cursor.is_none());
        let stale_page = AppListThreadTurnsResponse {
            turns: vec![item_with_turn("removed-turn", "removed-item")],
            next_cursor: Some("stale-cursor".to_string()),
            backwards_cursor: None,
        };
        assert!(
            !client
                .app_store
                .apply_thread_turns_pages(
                    &key,
                    Some(&old_epoch),
                    [&stale_page],
                    AppTurnsSortDirection::Descending,
                )
                .expect("reject stale page")
        );

        let epoch = client
            .app_store
            .thread_history_epoch(&key)
            .expect("new epoch");
        client
            .app_store
            .apply_ui_event(&crate::session::events::UiEvent::MessageDelta {
                key: key.clone(),
                item_id: "live-item".to_string(),
                delta: "new text".to_string(),
            });
        client.app_store.enqueue_thread_follow_up_preview(
            &key,
            crate::store::AppQueuedFollowUpPreview {
                id: "queued".to_string(),
                kind: crate::store::AppQueuedFollowUpKind::Message,
                text: "follow-up".to_string(),
            },
        );
        let before = client.app_store.thread_snapshot(&key).unwrap();
        let page = AppListThreadTurnsResponse {
            turns: vec![item_with_turn("older-turn", "older-item")],
            next_cursor: None,
            backwards_cursor: None,
        };
        assert!(
            client
                .app_store
                .apply_thread_turns_pages(
                    &key,
                    Some(&epoch),
                    [&page],
                    AppTurnsSortDirection::Descending,
                )
                .expect("merge current page")
        );
        let after = client.app_store.thread_snapshot(&key).unwrap();
        assert_eq!(after.items.last(), before.items.last());
        assert_eq!(
            after.queued_follow_up_drafts,
            before.queued_follow_up_drafts
        );
        assert_eq!(after.items.len(), 2);
        assert!(after.items.iter().all(|item| item.id != "removed-item"));
        assert!(after.older_turns_cursor.is_none());
    }

    #[test]
    fn merge_paged_turns_empty_store_first_desc_page() {
        let mut thread = test_thread_snapshot();
        let page = AppListThreadTurnsResponse {
            // Desc page: turn-3 newest, then turn-2, then turn-1.
            turns: vec![
                item_with_turn("turn-3", "i3"),
                item_with_turn("turn-2", "i2"),
                item_with_turn("turn-1", "i1"),
            ],
            next_cursor: Some("cursor-older".to_string()),
            backwards_cursor: None,
        };
        merge_paged_turns(&mut thread, &page, AppTurnsSortDirection::Descending);
        // Store should be chronological ascending.
        let ids: Vec<Option<String>> = thread
            .items
            .iter()
            .map(|item| item.source_turn_id.clone())
            .collect();
        assert_eq!(
            ids,
            vec![
                Some("turn-1".to_string()),
                Some("turn-2".to_string()),
                Some("turn-3".to_string()),
            ]
        );
        assert_eq!(thread.older_turns_cursor.as_deref(), Some("cursor-older"));
        assert!(thread.initial_turns_loaded);
    }

    #[test]
    fn merge_paged_turns_prepends_older_page() {
        let mut thread = test_thread_snapshot();
        thread.items = vec![item_with_turn("turn-3", "i3")];
        thread.initial_turns_loaded = true;
        thread.older_turns_cursor = Some("cursor-first".to_string());
        let page = AppListThreadTurnsResponse {
            turns: vec![
                item_with_turn("turn-2", "i2"),
                item_with_turn("turn-1", "i1"),
            ],
            next_cursor: None, // no more older
            backwards_cursor: None,
        };
        merge_paged_turns(&mut thread, &page, AppTurnsSortDirection::Descending);
        let ids: Vec<Option<String>> = thread
            .items
            .iter()
            .map(|item| item.source_turn_id.clone())
            .collect();
        assert_eq!(
            ids,
            vec![
                Some("turn-1".to_string()),
                Some("turn-2".to_string()),
                Some("turn-3".to_string()),
            ]
        );
        assert!(thread.older_turns_cursor.is_none());
        assert!(thread.initial_turns_loaded);
    }

    #[test]
    fn merge_paged_turns_dedupes_existing_turn_id() {
        let mut thread = test_thread_snapshot();
        thread.items = vec![item_with_turn("turn-3", "i3")];
        thread.initial_turns_loaded = true;
        let page = AppListThreadTurnsResponse {
            turns: vec![
                item_with_turn("turn-3", "i3-dup"),
                item_with_turn("turn-2", "i2"),
            ],
            next_cursor: None,
            backwards_cursor: None,
        };
        merge_paged_turns(&mut thread, &page, AppTurnsSortDirection::Descending);
        // Dupe of turn-3 should not reappear; turn-2 prepended.
        let ids: Vec<Option<String>> = thread
            .items
            .iter()
            .map(|item| item.source_turn_id.clone())
            .collect();
        assert_eq!(
            ids,
            vec![Some("turn-2".to_string()), Some("turn-3".to_string())]
        );
    }

    #[test]
    fn merge_paged_turns_completes_existing_turn_in_order_and_is_idempotent() {
        for direction in [
            AppTurnsSortDirection::Ascending,
            AppTurnsSortDirection::Descending,
        ] {
            let mut thread = test_thread_snapshot();
            thread.items = vec![
                item_with_turn("turn-0", "older-user"),
                item_with_turn("turn-1", "user"),
                item_with_turn("turn-2", "newer-user"),
            ];
            let page = AppListThreadTurnsResponse {
                turns: vec![
                    item_with_turn("turn-1", "user"),
                    assistant_item(Some("turn-1"), "assistant", "completed offline"),
                ],
                next_cursor: None,
                backwards_cursor: None,
            };
            for _ in 0..2 {
                merge_paged_turns(&mut thread, &page, direction);
                let ids: Vec<&str> = thread.items.iter().map(|item| item.id.as_str()).collect();
                assert_eq!(ids, vec!["older-user", "user", "assistant", "newer-user"]);
            }
        }
    }

    #[test]
    fn merge_paged_turns_completes_stable_live_items_in_order() {
        let page = AppListThreadTurnsResponse {
            turns: vec![
                item_with_turn("turn-1", "user"),
                assistant_item(Some("turn-1"), "assistant", "completed offline"),
            ],
            next_cursor: None,
            backwards_cursor: None,
        };
        for replay_item in &page.turns {
            for protect_live_item in [false, true] {
                let mut thread = test_thread_snapshot();
                let mut live_item = replay_item.clone();
                live_item.source_turn_id = None;
                thread.items = vec![live_item.clone()];
                let protected = if protect_live_item {
                    HashSet::from([live_item.id.clone()])
                } else {
                    HashSet::new()
                };
                for _ in 0..2 {
                    merge_paged_turns_preserving(
                        &mut thread,
                        &page,
                        AppTurnsSortDirection::Descending,
                        &protected,
                    );
                    let ids: Vec<&str> = thread.items.iter().map(|item| item.id.as_str()).collect();
                    assert_eq!(ids, vec!["user", "assistant"]);
                    let retained = thread
                        .items
                        .iter()
                        .find(|item| item.id == live_item.id)
                        .unwrap();
                    assert_eq!(
                        retained,
                        if protect_live_item {
                            &live_item
                        } else {
                            replay_item
                        }
                    );
                }
            }
        }
    }

    #[test]
    fn merge_paged_turns_inserts_missing_items_between_protected_live_anchors() {
        let mut thread = test_thread_snapshot();
        thread.active_turn_id = Some("turn-1".to_string());
        thread.items = vec![
            item_with_turn("turn-1", "user"),
            assistant_item(Some("turn-1"), "assistant", "newer live text"),
        ];
        let page = AppListThreadTurnsResponse {
            turns: vec![
                item_with_turn("turn-1", "user"),
                assistant_item(Some("turn-1"), "middle", "intermediate result"),
                assistant_item(Some("turn-1"), "assistant", "stale page text"),
                assistant_item(Some("turn-1"), "last", "another result"),
            ],
            next_cursor: None,
            backwards_cursor: None,
        };
        for _ in 0..2 {
            merge_paged_turns_preserving(
                &mut thread,
                &page,
                AppTurnsSortDirection::Descending,
                &HashSet::from(["assistant".to_string()]),
            );
            let ids: Vec<&str> = thread.items.iter().map(|item| item.id.as_str()).collect();
            assert_eq!(ids, vec!["user", "middle", "assistant", "last"]);
            assert_eq!(
                thread.items[2],
                assistant_item(Some("turn-1"), "assistant", "newer live text")
            );
        }
    }

    #[test]
    fn merge_paged_turns_distinguishes_repeated_text_from_legacy_aliases() {
        let mut thread = test_thread_snapshot();
        thread.active_turn_id = Some("turn-1".to_string());
        thread.items = vec![
            item_with_turn("turn-1", "user"),
            assistant_item(Some("turn-1"), "legacy-assistant", "same text"),
        ];
        let page = AppListThreadTurnsResponse {
            turns: vec![
                item_with_turn("turn-1", "user"),
                assistant_item(Some("turn-1"), "persisted-assistant", "same text"),
                assistant_item(Some("turn-1"), "second-assistant", "same text"),
            ],
            next_cursor: None,
            backwards_cursor: None,
        };
        for _ in 0..2 {
            merge_paged_turns(&mut thread, &page, AppTurnsSortDirection::Descending);
            let ids: Vec<&str> = thread.items.iter().map(|item| item.id.as_str()).collect();
            assert_eq!(ids, vec!["user", "legacy-assistant", "second-assistant"]);
        }
    }

    #[test]
    fn merge_paged_turns_adopts_only_unprotected_legacy_alias_in_anchored_span() {
        for protected_user in [false, true] {
            let mut thread = test_thread_snapshot();
            thread.active_turn_id = Some("turn-1".to_string());
            let mut other_user = item_with_turn("other-turn", "other-user");
            other_user.source_turn_id = None;
            let mut legacy_user = item_with_turn("turn-1", "legacy-user");
            legacy_user.source_turn_id = None;
            thread.items = vec![
                other_user.clone(),
                assistant_item(Some("other-turn"), "other-assistant", "older"),
                legacy_user.clone(),
                assistant_item(None, "assistant", "partial"),
            ];
            let page = AppListThreadTurnsResponse {
                turns: vec![
                    item_with_turn("turn-1", "persisted-user"),
                    assistant_item(Some("turn-1"), "assistant", "partial"),
                ],
                next_cursor: None,
                backwards_cursor: None,
            };
            let protected = if protected_user {
                HashSet::from(["legacy-user".to_string()])
            } else {
                HashSet::new()
            };
            for _ in 0..2 {
                merge_paged_turns_preserving(
                    &mut thread,
                    &page,
                    AppTurnsSortDirection::Descending,
                    &protected,
                );
                let ids: Vec<&str> = thread.items.iter().map(|item| item.id.as_str()).collect();
                let expected = if protected_user {
                    vec![
                        "other-user",
                        "other-assistant",
                        "legacy-user",
                        "persisted-user",
                        "assistant",
                    ]
                } else {
                    vec![
                        "other-user",
                        "other-assistant",
                        "persisted-user",
                        "assistant",
                    ]
                };
                assert_eq!(ids, expected);
                assert_eq!(thread.items[0], other_user);
                if protected_user {
                    assert_eq!(thread.items[2], legacy_user);
                }
            }
        }
    }

    #[test]
    fn merge_paged_turns_replaces_same_item_id_with_authoritative_replay_item() {
        let mut thread = test_thread_snapshot();
        let mut live_item = item_with_turn("turn-live", "stable-user-id");
        live_item.source_turn_id = None;
        thread.items = vec![live_item];
        let page = AppListThreadTurnsResponse {
            turns: vec![item_with_turn("turn-1", "stable-user-id")],
            next_cursor: None,
            backwards_cursor: None,
        };
        merge_paged_turns(&mut thread, &page, AppTurnsSortDirection::Descending);
        assert_eq!(thread.items.len(), 1);
        assert_eq!(thread.items[0].id, "stable-user-id");
        assert_eq!(thread.items[0].source_turn_id.as_deref(), Some("turn-1"));
    }

    #[test]
    fn merge_paged_turns_replaces_same_logical_live_item_with_replay_item() {
        let mut thread = test_thread_snapshot();
        let mut live_item = item_with_turn("turn-live", "live-user-id");
        live_item.source_turn_id = None;
        let replay_item = item_with_turn("turn-1", "persisted-user-id");
        thread.items = vec![live_item];
        let page = AppListThreadTurnsResponse {
            turns: vec![replay_item],
            next_cursor: None,
            backwards_cursor: None,
        };
        merge_paged_turns(&mut thread, &page, AppTurnsSortDirection::Descending);
        assert_eq!(thread.items.len(), 1);
        assert_eq!(thread.items[0].id, "persisted-user-id");
        assert_eq!(thread.items[0].source_turn_id.as_deref(), Some("turn-1"));
    }

    #[test]
    fn merge_paged_turns_removes_sourceless_stream_text_when_replay_repairs_turn() {
        let mut thread = test_thread_snapshot();
        let mut live_user = item_with_turn("turn-live", "live-user-id");
        live_user.source_turn_id = None;
        thread.items = vec![
            live_user,
            assistant_item(None, "live-assistant-id", "partial"),
        ];
        let page = AppListThreadTurnsResponse {
            turns: vec![
                item_with_turn("turn-1", "persisted-user-id"),
                assistant_item(Some("turn-1"), "persisted-assistant-id", "final"),
            ],
            next_cursor: None,
            backwards_cursor: None,
        };
        merge_paged_turns(&mut thread, &page, AppTurnsSortDirection::Descending);
        let ids: Vec<String> = thread.items.iter().map(|item| item.id.clone()).collect();
        assert_eq!(ids, vec!["persisted-user-id", "persisted-assistant-id"]);
    }

    #[test]
    fn merge_paged_turns_repair_preserves_text_from_other_turns() {
        let mut thread = test_thread_snapshot();
        let mut live_user = item_with_turn("turn-live", "live-user-id");
        live_user.source_turn_id = None;
        thread.items = vec![
            item_with_turn("turn-0", "older-user-id"),
            assistant_item(Some("turn-0"), "older-assistant-id", "older final"),
            live_user,
            assistant_item(None, "live-assistant-id", "partial"),
        ];
        let page = AppListThreadTurnsResponse {
            turns: vec![
                item_with_turn("turn-1", "persisted-user-id"),
                assistant_item(Some("turn-1"), "persisted-assistant-id", "final"),
            ],
            next_cursor: None,
            backwards_cursor: None,
        };
        merge_paged_turns(&mut thread, &page, AppTurnsSortDirection::Descending);
        let ids: Vec<String> = thread.items.iter().map(|item| item.id.clone()).collect();
        assert!(
            ids.contains(&"older-assistant-id".to_string()),
            "repair for turn-1 must preserve assistant text from turn-0; ids={ids:?}"
        );
        assert!(
            !ids.contains(&"live-assistant-id".to_string()),
            "repair for turn-1 should prune the stale live assistant placeholder; ids={ids:?}"
        );
    }

    #[test]
    fn merge_paged_turns_keeps_active_stream_text_while_loading_pages() {
        let mut thread = test_thread_snapshot();
        thread.active_turn_id = Some("active-turn".to_string());
        let mut live_user = item_with_turn("turn-live", "live-user-id");
        live_user.source_turn_id = None;
        thread.items = vec![
            live_user,
            assistant_item(Some("active-turn"), "active-assistant-id", "partial"),
        ];
        let page = AppListThreadTurnsResponse {
            turns: vec![
                item_with_turn("turn-1", "persisted-user-id"),
                assistant_item(Some("turn-1"), "persisted-assistant-id", "final"),
            ],
            next_cursor: None,
            backwards_cursor: None,
        };
        merge_paged_turns(&mut thread, &page, AppTurnsSortDirection::Descending);
        assert!(
            thread
                .items
                .iter()
                .any(|item| item.id == "active-assistant-id")
        );
    }

    #[test]
    fn merge_paged_turns_prunes_stale_stream_text_for_existing_turn_replay() {
        let mut thread = test_thread_snapshot();
        thread.items = vec![
            item_with_turn("turn-1", "persisted-user-id"),
            assistant_item(Some("turn-1"), "persisted-assistant-id", "final"),
            assistant_item(Some("turn-1"), "late-stream-assistant-id", "late duplicate"),
        ];
        let page = AppListThreadTurnsResponse {
            turns: vec![
                item_with_turn("turn-1", "persisted-user-id"),
                assistant_item(Some("turn-1"), "persisted-assistant-id", "final"),
            ],
            next_cursor: None,
            backwards_cursor: None,
        };
        merge_paged_turns(&mut thread, &page, AppTurnsSortDirection::Descending);
        let ids: Vec<String> = thread.items.iter().map(|item| item.id.clone()).collect();
        assert_eq!(ids, vec!["persisted-user-id", "persisted-assistant-id"]);
    }

    #[test]
    fn apply_pagination_merge_preserves_existing_on_empty_turns() {
        let mut existing = test_thread_snapshot();
        existing.items = vec![item_with_turn("turn-1", "i1")];
        existing.initial_turns_loaded = true;
        existing.older_turns_cursor = Some("cursor-1".to_string());
        let mut target = test_thread_snapshot();
        target.items = Vec::new();
        apply_pagination_merge(Some(&existing), &mut target, &[]);
        assert_eq!(target.items.len(), 1);
        assert!(target.initial_turns_loaded);
        assert_eq!(target.older_turns_cursor.as_deref(), Some("cursor-1"));
    }

    #[test]
    fn apply_pagination_merge_legacy_nonempty_is_authoritative() {
        let mut existing = test_thread_snapshot();
        existing.items = vec![item_with_turn("stale", "s1")];
        existing.initial_turns_loaded = true;
        existing.older_turns_cursor = Some("cursor-1".to_string());
        let mut target = test_thread_snapshot();
        // target already populated from upstream thread with hydrated items.
        target.items = vec![item_with_turn("turn-1", "i1")];
        let upstream_turn = upstream::Turn {
            id: "turn-1".to_string(),
            status: upstream::TurnStatus::Completed,
            items: Vec::new(),
            items_view: upstream::TurnItemsView::Full,
            error: None,
            started_at: None,
            completed_at: None,
            duration_ms: None,
        };
        apply_pagination_merge(Some(&existing), &mut target, &[upstream_turn]);
        // Legacy path: keep target's upstream-hydrated items, clear older
        // cursor, mark loaded.
        assert_eq!(target.items.len(), 1);
        assert!(target.initial_turns_loaded);
        assert!(target.older_turns_cursor.is_none());
    }

    #[test]
    fn apply_pagination_merge_no_existing_marks_initial_false() {
        let mut target = test_thread_snapshot();
        apply_pagination_merge(None, &mut target, &[]);
        assert!(!target.initial_turns_loaded);
        assert!(target.older_turns_cursor.is_none());
    }

    /// A freshly-started thread has no turns to page. The reducer must mark
    /// `initial_turns_loaded = true` immediately so the iOS conversation
    /// view does not auto-fire `thread/turns/list` (which the server
    /// rejects with "thread not materialized" until the first user turn
    /// lands).
    #[tokio::test]
    async fn apply_thread_start_response_marks_initial_turns_loaded() {
        let client = MobileClient::new();
        client.app_store.upsert_server(
            &ServerConfig {
                server_id: "srv".to_string(),
                display_name: "Server".to_string(),
                host: "localhost".to_string(),
                port: 8390,
                websocket_url: None,
                is_local: true,
                tls: false,
            },
            ServerHealthSnapshot::Connected,
        );
        let response = upstream::ThreadStartResponse {
            thread: test_upstream_thread("thread-1"),
            model: "gpt-5".to_string(),
            model_provider: "openai".to_string(),
            service_tier: None,
            cwd: test_abs_path("/tmp"),
            runtime_workspace_roots: Vec::new(),
            instruction_sources: Vec::new(),
            approval_policy: upstream::AskForApproval::Never,
            approvals_reviewer: upstream::ApprovalsReviewer::User,
            sandbox: upstream::SandboxPolicy::DangerFullAccess,
            active_permission_profile: None,
            reasoning_effort: None,
        };
        let key = client
            .apply_thread_start_response("srv", &response)
            .expect("thread/start reconciliation");
        let snapshot = client.app_store.thread_snapshot(&key).expect("snapshot");
        assert!(
            snapshot.initial_turns_loaded,
            "new thread must be marked initial_turns_loaded"
        );
        assert!(snapshot.older_turns_cursor.is_none());
    }

    /// `thread/read` carries embedded turns and is authoritative — the
    /// reducer must mark `initial_turns_loaded = true` so the spinner
    /// clears and `older_turns_cursor` gets cleared.
    #[tokio::test]
    async fn apply_thread_read_response_marks_initial_turns_loaded() {
        let client = MobileClient::new();
        client.app_store.upsert_server(
            &ServerConfig {
                server_id: "srv".to_string(),
                display_name: "Server".to_string(),
                host: "localhost".to_string(),
                port: 8390,
                websocket_url: None,
                is_local: true,
                tls: false,
            },
            ServerHealthSnapshot::Connected,
        );
        let response = upstream::ThreadReadResponse {
            thread: test_upstream_thread("thread-1"),
            approval_policy: None,
            sandbox: None,
        };
        let key = client
            .apply_thread_read_response("srv", &response)
            .expect("thread/read reconciliation");
        let snapshot = client.app_store.thread_snapshot(&key).expect("snapshot");
        assert!(
            snapshot.initial_turns_loaded,
            "thread/read response must mark initial_turns_loaded"
        );
        assert!(snapshot.older_turns_cursor.is_none());
    }

    /// Regression for task #12. A `thread/read` response with embedded
    /// turns is authoritative: it should clear `older_turns_cursor` and
    /// mark `initial_turns_loaded`.
    #[tokio::test]
    async fn apply_thread_read_with_embedded_turns_clears_cursor() {
        let client = MobileClient::new();
        client.app_store.upsert_server(
            &ServerConfig {
                server_id: "srv".to_string(),
                display_name: "Server".to_string(),
                host: "localhost".to_string(),
                port: 8390,
                websocket_url: None,
                is_local: true,
                tls: false,
            },
            ServerHealthSnapshot::Connected,
        );
        // Prime the snapshot with a stale cursor to confirm the embedded
        // path clears it.
        let info = crate::types::ThreadInfo {
            id: "thread-1".to_string(),
            title: None,
            model: None,
            status: crate::types::ThreadSummaryStatus::Idle,
            preview: None,
            cwd: None,
            path: None,
            model_provider: None,
            agent_nickname: None,
            agent_role: None,
            parent_thread_id: None,
            forked_from_id: None,
            agent_status: None,
            created_at: None,
            updated_at: None,
        };
        let mut primed = ThreadSnapshot::from_info("srv", info);
        primed.older_turns_cursor = Some("stale-cursor".to_string());
        primed.initial_turns_loaded = false;
        client.app_store.upsert_thread_snapshot(primed);

        let mut embedded_thread = test_upstream_thread("thread-1");
        embedded_thread.turns = vec![upstream::Turn {
            id: "turn-1".to_string(),
            status: upstream::TurnStatus::Completed,
            items: vec![upstream::ThreadItem::UserMessage {
                id: "server-user-item".to_string(),
                content: vec![upstream::UserInput::Text {
                    text: "hi".to_string(),
                    text_elements: Vec::new(),
                }],
            }],
            items_view: upstream::TurnItemsView::Full,
            error: None,
            started_at: None,
            completed_at: None,
            duration_ms: None,
        }];
        let response = upstream::ThreadReadResponse {
            thread: embedded_thread,
            approval_policy: None,
            sandbox: None,
        };
        let key = client
            .apply_thread_read_response("srv", &response)
            .expect("thread/read");
        let snapshot = client.app_store.thread_snapshot(&key).expect("snapshot");
        assert!(snapshot.initial_turns_loaded);
        assert!(
            snapshot.older_turns_cursor.is_none(),
            "embedded-turns path must clear cursor"
        );
    }

    /// Regression for task #12. A `thread/read` response with NO embedded
    /// turns (paginated server reply) must preserve the existing
    /// `older_turns_cursor` so the cursor stored by
    /// `apply_thread_turns_page` survives subsequent refreshes. Without
    /// this preservation, "Load earlier messages" never shows up on
    /// Android after `load_thread_turns_page` returned `has_more=true`.
    #[tokio::test]
    async fn apply_thread_read_with_empty_turns_preserves_pagination_state() {
        let client = MobileClient::new();
        client.app_store.upsert_server(
            &ServerConfig {
                server_id: "srv".to_string(),
                display_name: "Server".to_string(),
                host: "localhost".to_string(),
                port: 8390,
                websocket_url: None,
                is_local: true,
                tls: false,
            },
            ServerHealthSnapshot::Connected,
        );
        // Prime the snapshot as if load_thread_turns_page had just
        // applied a page: items populated, cursor stored, flag set.
        let info = crate::types::ThreadInfo {
            id: "thread-1".to_string(),
            title: None,
            model: None,
            status: crate::types::ThreadSummaryStatus::Idle,
            preview: None,
            cwd: None,
            path: None,
            model_provider: None,
            agent_nickname: None,
            agent_role: None,
            parent_thread_id: None,
            forked_from_id: None,
            agent_status: None,
            created_at: None,
            updated_at: None,
        };
        let mut primed = ThreadSnapshot::from_info("srv", info);
        primed.items = vec![item_with_turn("turn-5", "i5")];
        primed.older_turns_cursor = Some("older-cursor".to_string());
        primed.initial_turns_loaded = true;
        client.app_store.upsert_thread_snapshot(primed);

        // thread/read arrives with no embedded turns (paginated server).
        let mut empty_thread = test_upstream_thread("thread-1");
        empty_thread.turns = Vec::new();
        let response = upstream::ThreadReadResponse {
            thread: empty_thread,
            approval_policy: None,
            sandbox: None,
        };
        let key = client
            .apply_thread_read_response("srv", &response)
            .expect("thread/read");
        let snapshot = client.app_store.thread_snapshot(&key).expect("snapshot");
        assert!(
            snapshot.initial_turns_loaded,
            "initial_turns_loaded must remain true"
        );
        assert_eq!(
            snapshot.older_turns_cursor.as_deref(),
            Some("older-cursor"),
            "empty-turns thread/read must preserve existing pagination cursor"
        );
        assert_eq!(
            snapshot.items.len(),
            1,
            "existing paged items must be preserved when embedded turns are empty"
        );
    }
}
