use super::*;
use crate::store::reducer::RelayHistoryMode;

/// Read-side projection held until the host's authenticated freshness fence
/// has been checked. No partially repaired state is published to native UI.
pub(crate) struct RelayProjectionRepair {
    session: Arc<ServerSession>,
    generation: u64,
    runtime_pages: Vec<(AgentRuntimeKind, Vec<ThreadInfo>)>,
    incoming_ids: HashSet<String>,
    history_epochs: Vec<(ThreadKey, Arc<()>)>,
    threads: Vec<(
        AgentRuntimeKind,
        upstream::ThreadReadResponse,
        RelayHistoryMode,
        HashMap<String, u64>,
    )>,
}

impl MobileClient {
    pub(crate) async fn read_relay_projection(
        &self,
        server_id: &str,
    ) -> Result<RelayProjectionRepair, RpcError> {
        let session = self.get_session(server_id)?;
        let generation = self.app_store.server_event_generation(server_id);
        let history = self.app_store.server_history_snapshot(server_id);
        let history_epochs = history
            .iter()
            .map(|(thread, epoch)| (thread.key.clone(), Arc::clone(epoch)))
            .collect();
        let loaded = history
            .into_iter()
            .map(|(thread, _)| thread)
            .filter(|thread| {
                thread.is_resumed || !thread.items.is_empty() || thread.active_turn_id.is_some()
            })
            .collect::<Vec<_>>();
        let (runtime_pages, incoming_ids) =
            read_thread_list_from_app_server(Arc::clone(&session), server_id).await?;
        let mut threads = Vec::new();
        for thread in loaded {
            if !incoming_ids.contains(&thread.key.thread_id) {
                continue;
            }
            let runtime = thread.agent_runtime_kind;
            let dispatched_items = thread
                .items
                .iter()
                .map(|item| {
                    (
                        item.id.clone(),
                        crate::store::reducer::item_fingerprint(item),
                    )
                })
                .collect();
            let paginated =
                self.app_store.server_supports_turn_pagination(server_id) && runtime == "codex";
            let mut response = self
                .request_typed_for_session_runtime_rpc::<upstream::ThreadReadResponse>(
                    server_id,
                    Arc::clone(&session),
                    runtime.clone(),
                    upstream::ClientRequest::ThreadRead {
                        request_id: upstream::RequestId::Integer(crate::next_request_id()),
                        params: upstream::ThreadReadParams {
                            thread_id: thread.key.thread_id.clone(),
                            include_turns: !paginated,
                        },
                    },
                )
                .await?;
            let mut history_mode = RelayHistoryMode::Replace { older_cursor: None };
            if paginated {
                let anchor = thread.active_turn_id.as_deref().or_else(|| {
                    thread
                        .items
                        .iter()
                        .rev()
                        .find_map(|item| item.source_turn_id.as_deref())
                });
                let mut cursor = None;
                let mut seen_cursors = HashSet::new();
                let mut turns = Vec::new();
                loop {
                    let page = self
                        .request_typed_for_session_runtime_rpc::<upstream::ThreadTurnsListResponse>(
                            server_id,
                            Arc::clone(&session),
                            runtime.clone(),
                            upstream::ClientRequest::ThreadTurnsList {
                                request_id: upstream::RequestId::Integer(crate::next_request_id()),
                                params: upstream::ThreadTurnsListParams {
                                    thread_id: thread.key.thread_id.clone(),
                                    cursor: cursor.clone(),
                                    limit: Some(50),
                                    sort_direction: Some(upstream::SortDirection::Desc),
                                    items_view: Some(upstream::TurnItemsView::Full),
                                },
                            },
                        )
                        .await?;
                    if page.data.len() > 50
                        || page
                            .next_cursor
                            .as_ref()
                            .is_some_and(|next| !seen_cursors.insert(next.clone()))
                    {
                        return Err(RpcError::Deserialization(
                            "invalid relay turn history pagination".into(),
                        ));
                    }
                    if anchor.is_none() && thread.items.is_empty() {
                        turns.extend(page.data);
                        history_mode = RelayHistoryMode::Replace {
                            older_cursor: page.next_cursor,
                        };
                        break;
                    }
                    let overlap = anchor
                        .and_then(|anchor| page.data.iter().position(|turn| turn.id == anchor));
                    cursor = page.next_cursor;
                    if let Some(overlap) = overlap {
                        if thread.initial_turns_loaded && !thread.items.is_empty() {
                            // Stop at the newest cached turn: older cached pages
                            // remain authoritative, and only newer turns append.
                            turns.extend(page.data.into_iter().take(overlap + 1));
                            history_mode = RelayHistoryMode::Merge;
                        } else {
                            turns.extend(page.data);
                            history_mode = RelayHistoryMode::Replace {
                                older_cursor: cursor,
                            };
                        }
                        break;
                    }
                    turns.extend(page.data);
                    if cursor.is_none() {
                        break;
                    }
                }
                response.thread.turns = turns.into_iter().rev().collect();
            }
            threads.push((runtime, response, history_mode, dispatched_items));
        }
        Ok(RelayProjectionRepair {
            session,
            generation,
            runtime_pages,
            incoming_ids,
            history_epochs,
            threads,
        })
    }

    pub(crate) fn apply_relay_projection(
        &self,
        server_id: &str,
        repair: RelayProjectionRepair,
    ) -> Result<bool, RpcError> {
        let sessions = self.sessions_read();
        if !sessions
            .get(server_id)
            .is_some_and(|current| Arc::ptr_eq(current, &repair.session))
        {
            return Ok(false);
        }
        let projections = repair
            .threads
            .into_iter()
            .map(|(runtime, response, cursor, fingerprints)| {
                let turns = response.thread.turns.clone();
                let mut snapshot = thread_snapshot_from_upstream_thread_with_overrides(
                    server_id,
                    response.thread,
                    None,
                    None,
                    response.approval_policy.map(Into::into),
                    response.sandbox.map(Into::into),
                )
                .map_err(RpcError::Deserialization)?;
                snapshot.agent_runtime_kind = runtime;
                Ok((snapshot, turns, cursor, fingerprints))
            })
            .collect::<Result<Vec<_>, RpcError>>()?;
        Ok(self
            .app_store
            .apply_if_server_event_generation(server_id, repair.generation, |store| {
                store
                    .apply_relay_history_if_current(
                        server_id,
                        &repair.history_epochs,
                        repair.runtime_pages,
                        &repair.incoming_ids,
                        |store| {
                            for (snapshot, turns, cursor, fingerprints) in projections {
                                let key = snapshot.key.clone();
                                let runtime = snapshot.agent_runtime_kind.clone();
                                store.upsert_relay_thread_snapshot(
                                    snapshot,
                                    &turns,
                                    cursor,
                                    &fingerprints,
                                );
                                self.thread_runtime_routes().insert(key, runtime);
                            }
                        },
                    )
                    .is_some()
            })
            .unwrap_or(false))
    }
}

pub fn thread_info_from_upstream_thread(thread: upstream::Thread) -> Option<ThreadInfo> {
    thread_info_from_upstream_thread_list_item(thread, None, None)
}

pub(super) fn thread_info_from_upstream_thread_list_item(
    thread: upstream::Thread,
    model: Option<String>,
    _reasoning_effort: Option<String>,
) -> Option<ThreadInfo> {
    let mut info = ThreadInfo::from(thread);
    info.model = model;
    Some(info)
}

pub fn thread_snapshot_from_upstream_thread_with_overrides(
    server_id: &str,
    thread: upstream::Thread,
    model: Option<String>,
    reasoning_effort: Option<String>,
    effective_approval_policy: Option<crate::types::AppAskForApproval>,
    effective_sandbox_policy: Option<crate::types::AppSandboxPolicy>,
) -> Result<ThreadSnapshot, String> {
    Ok(thread_snapshot_from_upstream_thread_state(
        server_id,
        thread,
        model,
        reasoning_effort,
        effective_approval_policy,
        effective_sandbox_policy,
        None,
    ))
}

pub fn copy_thread_runtime_fields(source: &ThreadSnapshot, target: &mut ThreadSnapshot) {
    target.collaboration_mode = source.collaboration_mode;
    if target.model.is_none() {
        target.model = source.model.clone();
    }
    if target.reasoning_effort.is_none() {
        target.reasoning_effort = source.reasoning_effort.clone();
    }
    if target.queued_follow_ups.is_empty() {
        target.queued_follow_ups = source.queued_follow_ups.clone();
    }
    if target.queued_follow_up_drafts.is_empty() {
        target.queued_follow_up_drafts = source.queued_follow_up_drafts.clone();
    }
    target.context_tokens_used = source.context_tokens_used;
    target.model_context_window = source.model_context_window;
    target.rate_limits = source.rate_limits.clone();
    target.realtime_session_id = source.realtime_session_id.clone();
    if target.goal.is_none() {
        target.goal = source.goal.clone();
    }
    if target.active_plan_progress.is_none() {
        target.active_plan_progress = source.active_plan_progress.clone();
    }
    if target.pending_plan_implementation_turn_id.is_none() {
        target.pending_plan_implementation_turn_id =
            source.pending_plan_implementation_turn_id.clone();
    }
    target.is_resumed = target.is_resumed || source.is_resumed;
}

#[cfg(test)]
pub(super) fn queued_follow_up_preview_from_inputs(
    inputs: &[upstream::UserInput],
    kind: AppQueuedFollowUpKind,
) -> Option<AppQueuedFollowUpPreview> {
    queued_follow_up_draft_from_inputs(inputs, kind).map(|draft| draft.preview)
}

pub(super) fn queued_follow_up_draft_from_inputs(
    inputs: &[upstream::UserInput],
    kind: AppQueuedFollowUpKind,
) -> Option<crate::store::QueuedFollowUpDraft> {
    let text = queued_follow_up_text_from_inputs(inputs)?;

    Some(crate::store::QueuedFollowUpDraft {
        preview: AppQueuedFollowUpPreview {
            id: uuid::Uuid::new_v4().to_string(),
            kind,
            text,
        },
        inputs: inputs.to_vec(),
        source_message_json: queued_follow_up_message_json_from_inputs(inputs),
        causal_anchor_turn_id: None,
        autosend_claimed: false,
    })
}

pub(super) fn queued_follow_up_text_from_inputs(inputs: &[upstream::UserInput]) -> Option<String> {
    let mut text_parts: Vec<String> = Vec::new();
    let mut attachment_count = 0usize;

    for input in inputs {
        match input {
            upstream::UserInput::Text { text, .. } => {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    text_parts.push(trimmed.to_string());
                }
            }
            upstream::UserInput::Image { .. } | upstream::UserInput::LocalImage { .. } => {
                attachment_count += 1;
            }
            upstream::UserInput::Skill { .. } | upstream::UserInput::Mention { .. } => {}
        }
    }

    if !text_parts.is_empty() {
        Some(text_parts.join("\n"))
    } else {
        attachment_summary(attachment_count)
    }
}

#[cfg(test)]
pub(super) fn queued_follow_up_inputs_from_json_value(
    value: &serde_json::Value,
) -> Vec<upstream::UserInput> {
    let Some(object) = value.as_object() else {
        return Vec::new();
    };

    let message = object
        .get("userMessage")
        .or_else(|| object.get("user_message"))
        .and_then(serde_json::Value::as_object)
        .unwrap_or(object);

    let mut inputs = Vec::new();

    let text_elements = message
        .get("textElements")
        .or_else(|| message.get("text_elements"))
        .cloned()
        .and_then(|value| serde_json::from_value::<Vec<upstream::TextElement>>(value).ok())
        .unwrap_or_default();

    if let Some(text) = string_field(message, &["text", "message", "summary"]) {
        inputs.push(upstream::UserInput::Text {
            text,
            text_elements,
        });
    }

    let remote_images = message
        .get("remoteImageUrls")
        .or_else(|| message.get("remote_image_urls"))
        .or_else(|| message.get("images"))
        .or_else(|| message.get("imageUrls"))
        .or_else(|| message.get("image_urls"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(|url| upstream::UserInput::Image {
            url: url.to_string(),
            detail: None,
        });
    inputs.extend(remote_images);

    let local_images = message
        .get("localImages")
        .or_else(|| message.get("local_images"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_object)
        .filter_map(|image| {
            image
                .get("path")
                .and_then(serde_json::Value::as_str)
                .map(std::path::PathBuf::from)
        })
        .map(|path| upstream::UserInput::LocalImage { path, detail: None });
    inputs.extend(local_images);

    let mentions = message
        .get("mentionBindings")
        .or_else(|| message.get("mention_bindings"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_object)
        .filter_map(|binding| {
            let name = binding
                .get("mention")
                .or_else(|| binding.get("name"))
                .and_then(serde_json::Value::as_str)?;
            let path = binding.get("path").and_then(serde_json::Value::as_str)?;
            Some(upstream::UserInput::Mention {
                name: name.to_string(),
                path: path.to_string(),
            })
        });
    inputs.extend(mentions);

    let skills = message
        .get("skillBindings")
        .or_else(|| message.get("skill_bindings"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_object)
        .filter_map(|binding| {
            let name = binding
                .get("name")
                .or_else(|| binding.get("skill"))
                .and_then(serde_json::Value::as_str)?;
            let path = binding.get("path").and_then(serde_json::Value::as_str)?;
            Some(upstream::UserInput::Skill {
                name: name.to_string(),
                path: std::path::PathBuf::from(path),
            })
        });
    inputs.extend(skills);

    inputs
}

pub(super) fn queued_follow_up_message_json_from_inputs(
    inputs: &[upstream::UserInput],
) -> Option<serde_json::Value> {
    let mut text = None;
    let mut text_elements = Vec::new();
    let mut remote_image_urls = Vec::new();
    let mut local_images = Vec::new();
    let mut mention_bindings = Vec::new();
    let mut skill_bindings = Vec::new();

    for input in inputs {
        match input {
            upstream::UserInput::Text {
                text: current_text,
                text_elements: current_elements,
            } => {
                let trimmed = current_text.trim();
                if !trimmed.is_empty() {
                    text = Some(trimmed.to_string());
                }
                text_elements = current_elements.clone();
            }
            upstream::UserInput::Image { url, .. } => {
                remote_image_urls.push(url.clone());
            }
            upstream::UserInput::LocalImage { path, .. } => {
                let placeholder = format!("[Image #{}]", local_images.len() + 1);
                local_images.push(serde_json::json!({
                    "placeholder": placeholder,
                    "path": path,
                }));
            }
            upstream::UserInput::Mention { name, path } => {
                mention_bindings.push(serde_json::json!({
                    "mention": name,
                    "path": path,
                }));
            }
            upstream::UserInput::Skill { name, path } => {
                skill_bindings.push(serde_json::json!({
                    "name": name,
                    "path": path,
                }));
            }
        }
    }

    if text.is_none()
        && text_elements.is_empty()
        && remote_image_urls.is_empty()
        && local_images.is_empty()
        && mention_bindings.is_empty()
        && skill_bindings.is_empty()
    {
        return None;
    }

    Some(serde_json::json!({
        "text": text.unwrap_or_default(),
        "textElements": text_elements,
        "remoteImageUrls": remote_image_urls,
        "localImages": local_images,
        "mentionBindings": mention_bindings,
        "skillBindings": skill_bindings,
    }))
}

#[cfg(test)]
pub(super) fn string_field(
    object: &serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
) -> Option<String> {
    keys.iter()
        .filter_map(|key| object.get(*key))
        .find_map(|value| match value {
            serde_json::Value::String(text) => {
                let trimmed = text.trim();
                (!trimmed.is_empty()).then(|| trimmed.to_string())
            }
            serde_json::Value::Array(values) => {
                let joined = values
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
                (!joined.is_empty()).then_some(joined)
            }
            _ => None,
        })
}

pub(super) fn attachment_summary(attachment_count: usize) -> Option<String> {
    match attachment_count {
        0 => None,
        1 => Some("1 image attachment".to_string()),
        count => Some(format!("{count} image attachments")),
    }
}

pub(super) fn remote_oauth_callback_port(auth_url: &str) -> Result<u16, RpcError> {
    let parsed = Url::parse(auth_url).map_err(|error| {
        RpcError::Deserialization(format!("invalid auth URL for remote OAuth: {error}"))
    })?;
    let redirect_uri = parsed
        .query_pairs()
        .find(|(key, _)| key == "redirect_uri")
        .map(|(_, value)| value.into_owned())
        .ok_or_else(|| {
            RpcError::Deserialization("missing redirect_uri in remote OAuth auth URL".to_string())
        })?;
    let redirect = Url::parse(&redirect_uri).map_err(|error| {
        RpcError::Deserialization(format!(
            "invalid redirect_uri in remote OAuth auth URL: {error}"
        ))
    })?;
    let host = redirect.host_str().unwrap_or_default();
    if host != "localhost" && host != "127.0.0.1" {
        return Err(RpcError::Deserialization(format!(
            "unsupported remote OAuth callback host: {host}"
        )));
    }
    redirect.port_or_known_default().ok_or_else(|| {
        RpcError::Deserialization("missing callback port in remote OAuth redirect_uri".to_string())
    })
}

pub(super) fn ensure_thread_is_editable(snapshot: &ThreadSnapshot) -> Result<(), RpcError> {
    if snapshot.items.is_empty() {
        return Err(RpcError::Deserialization(
            "thread has no conversation items".to_string(),
        ));
    }
    Ok(())
}

pub(super) fn rollback_depth_for_turn(
    snapshot: &ThreadSnapshot,
    selected_turn_index: usize,
) -> Result<u32, RpcError> {
    let user_turn_indices = snapshot
        .items
        .iter()
        .enumerate()
        .filter_map(|(idx, item)| {
            matches!(
                item.content,
                crate::conversation_uniffi::HydratedConversationItemContent::User(_)
            )
            .then_some(idx)
        })
        .collect::<Vec<_>>();
    let item_index = *user_turn_indices.get(selected_turn_index).ok_or_else(|| {
        RpcError::Deserialization(format!("unknown user turn index {}", selected_turn_index))
    })?;
    let turns_after = snapshot.items.len().saturating_sub(item_index + 1);
    u32::try_from(turns_after)
        .map_err(|_| RpcError::Deserialization("rollback depth overflow".to_string()))
}

pub(super) fn user_boundary_text_for_turn(
    snapshot: &ThreadSnapshot,
    selected_turn_index: usize,
) -> Result<String, RpcError> {
    let item = snapshot
        .items
        .iter()
        .filter(|item| {
            matches!(
                item.content,
                crate::conversation_uniffi::HydratedConversationItemContent::User(_)
            )
        })
        .nth(selected_turn_index)
        .ok_or_else(|| {
            RpcError::Deserialization(format!("unknown user turn index {}", selected_turn_index))
        })?;
    match &item.content {
        crate::conversation_uniffi::HydratedConversationItemContent::User(data) => {
            Ok(data.text.clone())
        }
        _ => Err(RpcError::Deserialization(
            "selected turn has no editable text".to_string(),
        )),
    }
}

pub fn reasoning_effort_string(value: crate::types::ReasoningEffort) -> String {
    match value {
        crate::types::ReasoningEffort::None => "none".to_string(),
        crate::types::ReasoningEffort::Minimal => "minimal".to_string(),
        crate::types::ReasoningEffort::Low => "low".to_string(),
        crate::types::ReasoningEffort::Medium => "medium".to_string(),
        crate::types::ReasoningEffort::High => "high".to_string(),
        crate::types::ReasoningEffort::XHigh => "xhigh".to_string(),
        crate::types::ReasoningEffort::Max => "max".to_string(),
    }
}

pub fn reasoning_effort_from_string(value: &str) -> Option<crate::types::ReasoningEffort> {
    match value.trim().to_ascii_lowercase().as_str() {
        "none" => Some(crate::types::ReasoningEffort::None),
        "minimal" => Some(crate::types::ReasoningEffort::Minimal),
        "low" => Some(crate::types::ReasoningEffort::Low),
        "medium" => Some(crate::types::ReasoningEffort::Medium),
        "high" => Some(crate::types::ReasoningEffort::High),
        "xhigh" => Some(crate::types::ReasoningEffort::XHigh),
        "max" => Some(crate::types::ReasoningEffort::Max),
        _ => None,
    }
}

pub(super) fn core_reasoning_effort_from_mobile(
    value: crate::types::ReasoningEffort,
) -> codex_protocol::openai_models::ReasoningEffort {
    match value {
        crate::types::ReasoningEffort::None => codex_protocol::openai_models::ReasoningEffort::None,
        crate::types::ReasoningEffort::Minimal => {
            codex_protocol::openai_models::ReasoningEffort::Minimal
        }
        crate::types::ReasoningEffort::Low => codex_protocol::openai_models::ReasoningEffort::Low,
        crate::types::ReasoningEffort::Medium => {
            codex_protocol::openai_models::ReasoningEffort::Medium
        }
        crate::types::ReasoningEffort::High => codex_protocol::openai_models::ReasoningEffort::High,
        crate::types::ReasoningEffort::XHigh => {
            codex_protocol::openai_models::ReasoningEffort::XHigh
        }
        crate::types::ReasoningEffort::Max => codex_protocol::openai_models::ReasoningEffort::XHigh,
    }
}

pub(super) fn collaboration_mode_from_thread(
    thread: &ThreadSnapshot,
    mode: AppModeKind,
    model_override: Option<String>,
    effort_override: Option<codex_protocol::openai_models::ReasoningEffort>,
) -> Option<codex_protocol::config_types::CollaborationMode> {
    let model = model_override
        .or_else(|| thread.model.clone())
        .or_else(|| thread.info.model.clone())?;
    let reasoning_effort = effort_override.or_else(|| {
        thread
            .reasoning_effort
            .as_deref()
            .and_then(reasoning_effort_from_string)
            .map(core_reasoning_effort_from_mobile)
    });
    Some(codex_protocol::config_types::CollaborationMode {
        mode: match mode {
            AppModeKind::Default => codex_protocol::config_types::ModeKind::Default,
            AppModeKind::Plan => codex_protocol::config_types::ModeKind::Plan,
        },
        settings: codex_protocol::config_types::Settings {
            model,
            reasoning_effort,
            developer_instructions: None,
        },
    })
}

pub(super) fn map_rpc_client_error(error: crate::RpcClientError) -> RpcError {
    match error {
        crate::RpcClientError::Rpc(message) | crate::RpcClientError::Serialization(message) => {
            RpcError::Deserialization(message)
        }
    }
}

pub(super) fn map_ssh_transport_error(error: crate::ssh::SshError) -> TransportError {
    TransportError::ConnectionFailed(error.to_string())
}

pub(super) async fn refresh_thread_list_from_app_server(
    session: Arc<ServerSession>,
    app_store: Arc<AppStoreReducer>,
    server_id: &str,
) -> Result<(), RpcError> {
    let (runtime_pages, incoming_ids) =
        read_thread_list_from_app_server(session, server_id).await?;
    apply_thread_list_refresh(&app_store, server_id, runtime_pages, incoming_ids);
    Ok(())
}

pub(super) async fn refresh_thread_list_from_app_server_if_ui_generation(
    session: Arc<ServerSession>,
    sessions: Arc<RwLock<HashMap<String, Arc<ServerSession>>>>,
    app_store: Arc<AppStoreReducer>,
    server_id: &str,
    expected_generation: u64,
) -> Result<bool, RpcError> {
    let (runtime_pages, incoming_ids) =
        read_thread_list_from_app_server(Arc::clone(&session), server_id).await?;
    let sessions = match sessions.read() {
        Ok(guard) => guard,
        Err(error) => error.into_inner(),
    };
    if !sessions
        .get(server_id)
        .is_some_and(|current| Arc::ptr_eq(current, &session))
    {
        return Ok(false);
    }
    Ok(app_store
        .apply_if_server_event_generation(server_id, expected_generation, |store| {
            apply_thread_list_refresh(store, server_id, runtime_pages, incoming_ids);
        })
        .is_some())
}

async fn read_thread_list_from_app_server(
    session: Arc<ServerSession>,
    server_id: &str,
) -> Result<(Vec<(AgentRuntimeKind, Vec<ThreadInfo>)>, HashSet<String>), RpcError> {
    // Multiplexed sessions carry a separate command channel per
    // agent runtime. `thread/list` is not thread-scoped, so the default
    // dispatcher routes it to Codex only — pi and opencode threads would
    // never appear in the UI. Fan the request out across every runtime the
    // session knows about and merge the pages, so the user sees their pi /
    // opencode threads alongside codex's. `runtime_kinds()` returns
    // `[Codex]` for non-multiplexed sessions, preserving the previous
    // single-runtime behavior.
    let runtime_kinds = session.runtime_kinds();

    let mut incoming_ids = HashSet::new();
    let mut runtime_pages = Vec::new();
    for runtime_kind in runtime_kinds {
        let mut cursor = None;
        let mut runtime_threads = Vec::new();
        loop {
            let response =
                request_thread_list_page_for_runtime(&session, runtime_kind.clone(), cursor)
                    .await
                    .map_err(|error| {
                        warn!(
                            "thread/list failed for runtime {:?} on server {}: {}",
                            runtime_kind, server_id, error
                        );
                        error
                    })?;
            let page = thread_list_page_to_thread_infos(response.data, &mut incoming_ids);
            runtime_threads.extend(page);

            let Some(next_cursor) = response.next_cursor else {
                break;
            };
            cursor = Some(next_cursor);
        }
        runtime_pages.push((runtime_kind, runtime_threads));
    }

    Ok((runtime_pages, incoming_ids))
}

fn apply_thread_list_refresh(
    app_store: &AppStoreReducer,
    server_id: &str,
    runtime_pages: Vec<(AgentRuntimeKind, Vec<ThreadInfo>)>,
    incoming_ids: HashSet<String>,
) {
    for (runtime_kind, threads) in runtime_pages {
        app_store.upsert_thread_list_page_for_runtime(server_id, runtime_kind, &threads);
    }
    app_store.finalize_thread_list_sync(server_id, &incoming_ids);
}

pub(crate) async fn refresh_runtime_thread_list_from_client(
    client: &codex_app_server_client::AppServerClient,
    app_store: Arc<AppStoreReducer>,
    server_id: &str,
    runtime_kind: AgentRuntimeKind,
) -> Result<(), RpcError> {
    let mut incoming_ids = HashSet::new();
    let mut cursor: Option<String> = None;
    loop {
        let params = match cursor {
            Some(cursor) => serde_json::json!({ "cursor": cursor }),
            None => serde_json::json!({}),
        };
        let request: upstream::ClientRequest = serde_json::from_value(serde_json::json!({
            "id": format!("remora-link-reconcile-{}", uuid::Uuid::new_v4()),
            "method": "thread/list",
            "params": params,
        }))
        .map_err(|error| RpcError::Deserialization(format!("build thread/list: {error}")))?;
        let response = client
            .request(request)
            .await
            .map_err(|error| RpcError::Transport(TransportError::SendFailed(error.to_string())))?
            .map_err(|error| RpcError::Server {
                code: error.code,
                message: error.message,
            })?;
        let mut response = response;
        normalize_empty_thread_list_cwds(&mut response);
        let response =
            serde_json::from_value::<upstream::ThreadListResponse>(response).map_err(|error| {
                RpcError::Deserialization(format!("deserialize thread/list: {error}"))
            })?;
        let page = thread_list_page_to_thread_infos(response.data, &mut incoming_ids);
        app_store.upsert_thread_list_page_for_runtime(server_id, runtime_kind.clone(), &page);
        let Some(next_cursor) = response.next_cursor else {
            break;
        };
        cursor = Some(next_cursor);
    }
    app_store.finalize_thread_list_sync_for_runtime(server_id, &runtime_kind, &incoming_ids);
    Ok(())
}

pub(super) async fn refresh_account_from_app_server(
    session: Arc<ServerSession>,
    app_store: Arc<AppStoreReducer>,
    sessions: Arc<RwLock<HashMap<String, Arc<ServerSession>>>>,
    server_id: &str,
) -> Result<(), RpcError> {
    let response = session
        .request("account/read", serde_json::json!({ "refreshToken": false }))
        .await?;
    if !session_is_current(&sessions, server_id, &session) {
        return Ok(());
    }
    let response =
        serde_json::from_value::<upstream::GetAccountResponse>(response).map_err(|error| {
            RpcError::Deserialization(format!("deserialize account/read response: {error}"))
        })?;
    app_store.update_server_account(
        server_id,
        response.account.map(Into::into),
        response.requires_openai_auth,
    );
    Ok(())
}

async fn request_thread_list_page_for_runtime(
    session: &ServerSession,
    runtime_kind: AgentRuntimeKind,
    cursor: Option<String>,
) -> Result<upstream::ThreadListResponse, RpcError> {
    let params = match cursor {
        Some(cursor) => serde_json::json!({ "cursor": cursor }),
        None => serde_json::json!({}),
    };
    let response = session
        .request_for_runtime(runtime_kind, "thread/list", params)
        .await?;
    let mut response = response;
    normalize_empty_thread_list_cwds(&mut response);
    serde_json::from_value::<upstream::ThreadListResponse>(response)
        .map_err(|error| RpcError::Deserialization(format!("deserialize thread/list: {error}")))
}

fn normalize_empty_thread_list_cwds(value: &mut serde_json::Value) {
    let Some(data) = value
        .get_mut("data")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };
    for item in data {
        let Some(map) = item.as_object_mut() else {
            continue;
        };
        if let Some(serde_json::Value::String(cwd)) = map.get_mut("cwd")
            && cwd.is_empty()
        {
            *cwd = "/".to_string();
        }
    }
}

fn thread_list_page_to_thread_infos(
    data: Vec<upstream::Thread>,
    incoming_ids: &mut HashSet<String>,
) -> Vec<ThreadInfo> {
    let mut threads = Vec::new();
    for thread in data {
        let Some(info) = thread_info_from_upstream_thread(thread) else {
            continue;
        };
        incoming_ids.insert(info.id.clone());
        threads.push(info);
    }
    threads
}

pub(super) fn session_is_current(
    sessions: &Arc<RwLock<HashMap<String, Arc<ServerSession>>>>,
    server_id: &str,
    session: &Arc<ServerSession>,
) -> bool {
    match sessions.read() {
        Ok(guard) => guard
            .get(server_id)
            .map(|current| Arc::ptr_eq(current, session))
            .unwrap_or(false),
        Err(error) => error
            .into_inner()
            .get(server_id)
            .map(|current| Arc::ptr_eq(current, session))
            .unwrap_or(false),
    }
}

#[cfg(test)]
mod authoritative_thread_list_tests {
    use super::*;
    use crate::session::connection::TestRequestHandler;

    fn cached_thread_info(id: &str) -> ThreadInfo {
        ThreadInfo {
            id: id.to_string(),
            title: Some("Cached thread".to_string()),
            model: None,
            status: ThreadSummaryStatus::Idle,
            preview: None,
            cwd: Some("/tmp".to_string()),
            path: None,
            model_provider: None,
            agent_nickname: None,
            agent_role: None,
            parent_thread_id: None,
            forked_from_id: None,
            agent_status: None,
            created_at: None,
            updated_at: None,
        }
    }

    fn relay_projection_fixture() -> (Arc<MobileClient>, ThreadKey, ServerConfig) {
        relay_projection_fixture_with_pages(|params| {
            if params.cursor.is_some() {
                return serde_json::json!({"data": [], "nextCursor": null});
            }
            serde_json::json!({"data": [{
                "id": "turn-1", "items": [{"type": "agentMessage", "id": "answer", "text": "REMOTE_COMPLETE"}],
                "itemsView": "full", "status": "completed", "error": null
            }], "nextCursor": "older"})
        })
    }

    fn relay_projection_fixture_with_pages(
        pages: impl Fn(upstream::ThreadTurnsListParams) -> serde_json::Value + Send + Sync + 'static,
    ) -> (Arc<MobileClient>, ThreadKey, ServerConfig) {
        let client = MobileClient::new();
        let config = ServerConfig {
            server_id: "relay-host".into(),
            display_name: "Host".into(),
            host: "localhost".into(),
            port: 0,
            websocket_url: None,
            is_local: false,
            tls: false,
        };
        client
            .app_store
            .upsert_server(&config, ServerHealthSnapshot::Connected);
        client
            .app_store
            .set_server_supports_turn_pagination("relay-host", true);
        let mut stale = ThreadSnapshot::from_info("relay-host", cached_thread_info("thread-1"));
        stale.active_turn_id = Some("turn-1".into());
        stale.info.status = ThreadSummaryStatus::Active;
        stale.is_resumed = true;
        let key = stale.key.clone();
        client.app_store.upsert_thread_snapshot(stale);
        let thread = serde_json::json!({
            "id": "thread-1", "sessionId": "session-1", "preview": "complete", "ephemeral": false,
            "modelProvider": "openai", "createdAt": 1, "updatedAt": 2,
            "status": {"type": "idle"}, "path": "/tmp/thread", "cwd": "/tmp",
            "cliVersion": "1", "source": "cli", "turns": []
        });
        let handler: TestRequestHandler = Arc::new(move |request| match request {
            upstream::ClientRequest::ThreadList { .. } => {
                Ok(serde_json::json!({"data": [thread], "nextCursor": null}))
            }
            upstream::ClientRequest::ThreadRead { params, .. } => {
                assert!(!params.include_turns);
                Ok(serde_json::json!({"thread": thread}))
            }
            upstream::ClientRequest::ThreadTurnsList { params, .. } => {
                assert_eq!(params.limit, Some(50));
                assert_eq!(params.items_view, Some(upstream::TurnItemsView::Full));
                Ok(pages(params))
            }
            _ => panic!(
                "repair must not issue mutating/subscription RPCs: {}",
                request.method()
            ),
        });
        let session = Arc::new(ServerSession::test_stub_with_handlers(
            config.clone(),
            Some(handler),
            None,
            None,
        ));
        client
            .sessions
            .write()
            .unwrap()
            .insert("relay-host".into(), session);
        (client, key, config)
    }

    #[tokio::test]
    async fn relay_projection_is_read_only_bounded_and_commits_only_to_the_current_session() {
        let (client, key, config) = relay_projection_fixture();
        let projection = client.read_relay_projection("relay-host").await.unwrap();
        assert_eq!(
            client
                .app_store
                .thread_snapshot(&key)
                .unwrap()
                .active_turn_id
                .as_deref(),
            Some("turn-1")
        );
        assert!(
            client
                .apply_relay_projection("relay-host", projection)
                .unwrap()
        );
        let complete = client.app_store.thread_snapshot(&key).unwrap();
        assert_eq!(complete.active_turn_id, None);
        assert_eq!(complete.older_turns_cursor.as_deref(), Some("older"));
        assert!(complete.items.iter().any(|item| matches!(
            &item.content,
            crate::conversation_uniffi::HydratedConversationItemContent::Assistant(message)
                if message.text == "REMOTE_COMPLETE"
        )));
        let event_stale_projection = client.read_relay_projection("relay-host").await.unwrap();
        client
            .app_store
            .apply_ui_event(&UiEvent::ThreadArchived { key: key.clone() });
        assert!(
            !client
                .apply_relay_projection("relay-host", event_stale_projection)
                .unwrap()
        );
        let stale_projection = client.read_relay_projection("relay-host").await.unwrap();
        client.sessions.write().unwrap().insert(
            "relay-host".into(),
            Arc::new(ServerSession::test_stub(config)),
        );
        assert!(
            !client
                .apply_relay_projection("relay-host", stale_projection)
                .unwrap()
        );
    }

    fn relay_history_page(
        turn_id: &str,
        text: &str,
        cursor: &str,
    ) -> crate::types::AppListThreadTurnsResponse {
        let turn: upstream::Turn = serde_json::from_value(serde_json::json!({
            "id": turn_id, "items": [{"type": "agentMessage", "id": format!("item-{turn_id}"), "text": text}],
            "itemsView": "full", "status": "completed", "error": null
        })).unwrap();
        crate::types::AppListThreadTurnsResponse {
            turns: crate::conversation::hydrate_turns(&[turn], &Default::default()),
            next_cursor: Some(cursor.into()),
            backwards_cursor: None,
        }
    }

    fn numbered_relay_turn(index: usize) -> serde_json::Value {
        serde_json::json!({
            "id": format!("turn-{index}"),
            "items": [{"type": "agentMessage", "id": format!("item-{index}"), "text": format!("MESSAGE_{index}")}],
            "itemsView": "full", "status": "completed", "error": null
        })
    }

    fn numbered_relay_page(
        total: usize,
        params: upstream::ThreadTurnsListParams,
    ) -> serde_json::Value {
        let end = params
            .cursor
            .map(|cursor| cursor.parse::<usize>().unwrap())
            .unwrap_or(total);
        let start = end.saturating_sub(50);
        serde_json::json!({
            "data": (start..end).rev().map(numbered_relay_turn).collect::<Vec<_>>(),
            "nextCursor": (start > 0).then(|| start.to_string()),
        })
    }

    #[tokio::test]
    async fn relay_projection_fills_more_than_fifty_intervening_turns_through_cached_overlap() {
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = Arc::clone(&requests);
        let (client, key, _) = relay_projection_fixture_with_pages(move |params| {
            count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            numbered_relay_page(130, params)
        });
        let mut cached = client.app_store.thread_snapshot(&key).unwrap();
        let turns: Vec<upstream::Turn> = (0..20)
            .map(|index| serde_json::from_value(numbered_relay_turn(index)).unwrap())
            .collect();
        cached.items = crate::conversation::hydrate_turns(&turns, &Default::default());
        cached.active_turn_id = None;
        cached.initial_turns_loaded = true;
        cached.older_turns_cursor = Some("before-cached".into());
        client.app_store.upsert_thread_snapshot(cached);
        let repair = client.read_relay_projection("relay-host").await.unwrap();
        assert_eq!(requests.load(std::sync::atomic::Ordering::Relaxed), 3);
        assert!(client.apply_relay_projection("relay-host", repair).unwrap());
        let current = client.app_store.thread_snapshot(&key).unwrap();
        assert_eq!(
            current
                .items
                .iter()
                .map(|item| item.id.clone())
                .collect::<Vec<_>>(),
            (0..130)
                .map(|index| format!("item-{index}"))
                .collect::<Vec<_>>()
        );
        assert_eq!(current.older_turns_cursor.as_deref(), Some("before-cached"));
    }

    #[tokio::test]
    async fn relay_projection_initial_history_reads_one_bounded_page() {
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = Arc::clone(&requests);
        let (client, key, _) = relay_projection_fixture_with_pages(move |params| {
            count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            numbered_relay_page(130, params)
        });
        let mut unloaded = client.app_store.thread_snapshot(&key).unwrap();
        unloaded.active_turn_id = None;
        client.app_store.upsert_thread_snapshot(unloaded);
        let repair = client.read_relay_projection("relay-host").await.unwrap();
        assert_eq!(requests.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert!(client.apply_relay_projection("relay-host", repair).unwrap());
        let current = client.app_store.thread_snapshot(&key).unwrap();
        assert_eq!(
            current
                .items
                .iter()
                .map(|item| item.id.clone())
                .collect::<Vec<_>>(),
            (80..130)
                .map(|index| format!("item-{index}"))
                .collect::<Vec<_>>()
        );
        assert_eq!(current.older_turns_cursor.as_deref(), Some("80"));
    }

    #[tokio::test]
    async fn relay_projection_reads_to_end_without_overlap_and_replaces_obsolete_history() {
        for total in [120, 0] {
            let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let count = Arc::clone(&requests);
            let (client, key, _) = relay_projection_fixture_with_pages(move |params| {
                count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                numbered_relay_page(total, params)
            });
            let mut cached = client.app_store.thread_snapshot(&key).unwrap();
            cached.items = relay_history_page("obsolete", "OBSOLETE", "stale-cursor").turns;
            cached.active_turn_id = Some("obsolete".into());
            cached.initial_turns_loaded = true;
            cached.older_turns_cursor = Some("stale-cursor".into());
            client.app_store.upsert_thread_snapshot(cached);
            let epoch = client.app_store.thread_history_epoch(&key).unwrap();
            let repair = client.read_relay_projection("relay-host").await.unwrap();
            assert_eq!(
                requests.load(std::sync::atomic::Ordering::Relaxed),
                if total == 0 { 1 } else { 3 }
            );
            assert!(client.apply_relay_projection("relay-host", repair).unwrap());
            let current = client.app_store.thread_snapshot(&key).unwrap();
            assert_eq!(
                current
                    .items
                    .iter()
                    .map(|item| item.id.clone())
                    .collect::<Vec<_>>(),
                (0..total)
                    .map(|index| format!("item-{index}"))
                    .collect::<Vec<_>>()
            );
            assert_eq!(current.older_turns_cursor, None);
            assert_eq!(current.active_turn_id, None);
            let stale_page = relay_history_page("obsolete", "OBSOLETE", "stale-cursor");
            assert!(
                !client
                    .app_store
                    .apply_thread_turns_pages(
                        &key,
                        Some(&epoch),
                        [&stale_page],
                        crate::types::AppTurnsSortDirection::Descending
                    )
                    .unwrap()
            );
        }
    }

    #[tokio::test]
    async fn relay_projection_rejects_repeated_cursor_without_publishing_partial_pages() {
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = Arc::clone(&requests);
        let (client, key, _) = relay_projection_fixture_with_pages(move |_| {
            count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            serde_json::json!({"data": [numbered_relay_turn(999)], "nextCursor": "repeated"})
        });
        let before = client.app_store.thread_snapshot(&key).unwrap();
        assert!(client.read_relay_projection("relay-host").await.is_err());
        assert_eq!(requests.load(std::sync::atomic::Ordering::Relaxed), 2);
        let after = client.app_store.thread_snapshot(&key).unwrap();
        assert_eq!(after.items, before.items);
        assert_eq!(after.active_turn_id, before.active_turn_id);
    }

    #[tokio::test]
    async fn relay_projection_merges_older_page_completed_during_delayed_repair() {
        let (client, key, _) = relay_projection_fixture();
        let first = client.read_relay_projection("relay-host").await.unwrap();
        assert!(client.apply_relay_projection("relay-host", first).unwrap());
        let old = relay_history_page("old", "OLD", "before-old");
        client
            .app_store
            .apply_thread_turns_pages(
                &key,
                None,
                [&old],
                crate::types::AppTurnsSortDirection::Descending,
            )
            .unwrap();
        client
            .app_store
            .mutate_thread_history(&key, None, |thread| {
                if let crate::conversation_uniffi::HydratedConversationItemContent::Assistant(
                    message,
                ) = &mut thread
                    .items
                    .iter_mut()
                    .find(|item| item.id == "answer")
                    .unwrap()
                    .content
                {
                    message.text = "STALE_CACHED".into();
                }
            })
            .unwrap();
        let delayed = client.read_relay_projection("relay-host").await.unwrap();
        let older = relay_history_page("older", "OLDER", "before-older");
        client
            .app_store
            .apply_thread_turns_pages(
                &key,
                None,
                [&older],
                crate::types::AppTurnsSortDirection::Descending,
            )
            .unwrap();
        assert!(
            client
                .apply_relay_projection("relay-host", delayed)
                .unwrap()
        );
        let current = client.app_store.thread_snapshot(&key).unwrap();
        assert_eq!(
            current
                .items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["item-older", "item-old", "answer"]
        );
        assert_eq!(current.older_turns_cursor.as_deref(), Some("before-older"));
        assert!(current.items.iter().any(|item| matches!(
            &item.content,
            crate::conversation_uniffi::HydratedConversationItemContent::Assistant(message)
                if message.text == "REMOTE_COMPLETE"
        )));
        let delayed = client.read_relay_projection("relay-host").await.unwrap();
        client
            .app_store
            .mutate_thread_history(&key, None, |thread| {
                if let crate::conversation_uniffi::HydratedConversationItemContent::Assistant(
                    message,
                ) = &mut thread
                    .items
                    .iter_mut()
                    .find(|item| item.id == "answer")
                    .unwrap()
                    .content
                {
                    message.text = "LOCALLY_NEWER".into();
                }
            })
            .unwrap();
        assert!(
            client
                .apply_relay_projection("relay-host", delayed)
                .unwrap()
        );
        assert!(
            client
                .app_store
                .thread_snapshot(&key)
                .unwrap()
                .items
                .iter()
                .any(|item| matches!(
                    &item.content,
                    crate::conversation_uniffi::HydratedConversationItemContent::Assistant(message)
                        if message.text == "LOCALLY_NEWER"
                ))
        );
    }

    #[tokio::test]
    async fn relay_projection_rejects_local_reset_and_removal_after_read() {
        let (client, key, _) = relay_projection_fixture();
        let delayed = client.read_relay_projection("relay-host").await.unwrap();
        let mut reset = client.app_store.thread_snapshot(&key).unwrap();
        reset.items.clear();
        reset.active_turn_id = None;
        reset.info.preview = Some("reset locally".into());
        client.app_store.replace_thread_history(reset);
        assert!(
            !client
                .apply_relay_projection("relay-host", delayed)
                .unwrap()
        );
        let current = client.app_store.thread_snapshot(&key).unwrap();
        assert!(current.items.is_empty());
        assert_eq!(current.info.preview.as_deref(), Some("reset locally"));
        let delayed = client.read_relay_projection("relay-host").await.unwrap();
        client.app_store.remove_thread(&key);
        assert!(
            !client
                .apply_relay_projection("relay-host", delayed)
                .unwrap()
        );
        assert!(client.app_store.thread_snapshot(&key).is_none());
    }

    #[tokio::test]
    async fn partial_runtime_refresh_preserves_cached_sibling_threads() {
        let server_id = "srv";
        let config = ServerConfig {
            server_id: server_id.to_string(),
            display_name: "Server".to_string(),
            host: "example.local".to_string(),
            port: 0,
            websocket_url: None,
            is_local: false,
            tls: false,
        };
        let app_store = Arc::new(AppStoreReducer::new());
        let stale_key = ThreadKey {
            server_id: server_id.to_string(),
            thread_id: "pi-cached".to_string(),
        };
        let mut stale_thread =
            ThreadSnapshot::from_info(server_id, cached_thread_info(stale_key.thread_id.as_str()));
        stale_thread.agent_runtime_kind = "pi".to_string();
        app_store.upsert_thread_snapshot(stale_thread);

        let codex_handler: TestRequestHandler = Arc::new(|_| {
            serde_json::to_value(upstream::ThreadListResponse {
                data: Vec::new(),
                next_cursor: None,
                backwards_cursor: None,
            })
            .map_err(|error| RpcError::Deserialization(error.to_string()))
        });
        let failed_sibling_handler: TestRequestHandler =
            Arc::new(|_| Err(RpcError::Transport(TransportError::Disconnected)));
        let session = Arc::new(ServerSession::test_stub_with_runtime_handlers(
            config,
            vec![
                ("codex".to_string(), codex_handler),
                ("pi".to_string(), failed_sibling_handler),
            ],
        ));

        let result =
            refresh_thread_list_from_app_server(session, Arc::clone(&app_store), server_id).await;

        assert!(result.is_err());
        assert!(
            app_store.snapshot().threads.contains_key(&stale_key),
            "a failed sibling runtime must prevent global stale-thread pruning"
        );
    }

    #[tokio::test]
    async fn stale_thread_list_response_cannot_resurrect_archived_thread() {
        let server_id = "srv";
        let thread_id = "archived";
        let config = ServerConfig {
            server_id: server_id.to_string(),
            display_name: "Server".to_string(),
            host: "example.local".to_string(),
            port: 0,
            websocket_url: None,
            is_local: false,
            tls: false,
        };
        let app_store = Arc::new(AppStoreReducer::new());
        let key = ThreadKey {
            server_id: server_id.to_string(),
            thread_id: thread_id.to_string(),
        };
        app_store.upsert_thread_snapshot(ThreadSnapshot::from_info(
            server_id,
            cached_thread_info(thread_id),
        ));
        let request_generation = app_store.server_event_generation(server_id);
        let event_store = Arc::clone(&app_store);
        let event_key = key.clone();
        let handler: TestRequestHandler = Arc::new(move |_| {
            event_store.apply_ui_event(&UiEvent::ThreadArchived {
                key: event_key.clone(),
            });
            Ok(serde_json::json!({
                "data": [{
                    "id": thread_id,
                    "sessionId": thread_id,
                    "preview": "stale",
                    "ephemeral": false,
                    "modelProvider": "openai",
                    "createdAt": 1,
                    "updatedAt": 2,
                    "status": { "type": "idle" },
                    "path": "/tmp/thread",
                    "cwd": "/tmp/thread",
                    "cliVersion": "1.0.0",
                    "source": "cli",
                    "agentNickname": null,
                    "agentRole": null,
                    "gitInfo": null,
                    "name": "Archived",
                    "turns": []
                }],
                "nextCursor": null,
                "backwardsCursor": null
            }))
        });
        let session = Arc::new(ServerSession::test_stub_with_handlers(
            config,
            Some(handler),
            None,
            None,
        ));
        let sessions = Arc::new(RwLock::new(HashMap::from([(
            server_id.to_string(),
            Arc::clone(&session),
        )])));

        let applied = refresh_thread_list_from_app_server_if_ui_generation(
            session,
            sessions,
            Arc::clone(&app_store),
            server_id,
            request_generation,
        )
        .await
        .expect("thread/list should deserialize");

        assert!(!applied);
        assert!(!app_store.snapshot().threads.contains_key(&key));
    }

    #[tokio::test]
    async fn stale_thread_list_response_from_replaced_session_is_discarded() {
        let server_id = "srv";
        let thread_id = "cached";
        let config = ServerConfig {
            server_id: server_id.to_string(),
            display_name: "Server".to_string(),
            host: "example.local".to_string(),
            port: 0,
            websocket_url: None,
            is_local: false,
            tls: false,
        };
        let app_store = Arc::new(AppStoreReducer::new());
        let key = ThreadKey {
            server_id: server_id.to_string(),
            thread_id: thread_id.to_string(),
        };
        app_store.upsert_thread_snapshot(ThreadSnapshot::from_info(
            server_id,
            cached_thread_info(thread_id),
        ));
        let request_generation = app_store.server_event_generation(server_id);
        let sessions = Arc::new(RwLock::new(HashMap::new()));
        let replacement = Arc::new(ServerSession::test_stub_with_handlers(
            config.clone(),
            None,
            None,
            None,
        ));
        let handler: TestRequestHandler = {
            let sessions = Arc::clone(&sessions);
            let replacement = Arc::clone(&replacement);
            Arc::new(move |_| {
                sessions
                    .write()
                    .expect("sessions lock")
                    .insert(server_id.to_string(), Arc::clone(&replacement));
                Ok(serde_json::json!({
                    "data": [{
                        "id": thread_id,
                        "sessionId": thread_id,
                        "preview": "stale",
                        "ephemeral": false,
                        "modelProvider": "openai",
                        "createdAt": 1,
                        "updatedAt": 2,
                        "status": { "type": "idle" },
                        "path": "/tmp/stale",
                        "cwd": "/tmp/stale",
                        "cliVersion": "1.0.0",
                        "source": "cli",
                        "agentNickname": null,
                        "agentRole": null,
                        "gitInfo": null,
                        "name": "Stale thread",
                        "turns": []
                    }],
                    "nextCursor": null,
                    "backwardsCursor": null
                }))
            })
        };
        let stale_session = Arc::new(ServerSession::test_stub_with_handlers(
            config,
            Some(handler),
            None,
            None,
        ));
        sessions
            .write()
            .expect("sessions lock")
            .insert(server_id.to_string(), Arc::clone(&stale_session));

        let applied = refresh_thread_list_from_app_server_if_ui_generation(
            stale_session,
            Arc::clone(&sessions),
            Arc::clone(&app_store),
            server_id,
            request_generation,
        )
        .await
        .expect("thread/list should deserialize");

        assert!(!applied);
        let thread = app_store
            .thread_snapshot(&key)
            .expect("cached thread remains");
        assert_eq!(thread.info.title.as_deref(), Some("Cached thread"));
        assert_eq!(thread.info.cwd.as_deref(), Some("/tmp"));
    }
}

pub(super) async fn read_thread_response_from_app_server(
    session: Arc<ServerSession>,
    thread_id: &str,
    include_turns: bool,
) -> Result<upstream::ThreadReadResponse, RpcError> {
    let response = session
        .request(
            "thread/read",
            serde_json::json!({ "threadId": thread_id, "includeTurns": include_turns }),
        )
        .await?;
    serde_json::from_value::<upstream::ThreadReadResponse>(response).map_err(|error| {
        RpcError::Deserialization(format!("deserialize thread/read response: {error}"))
    })
}

pub(super) fn upsert_thread_snapshot_from_app_server_read_response(
    app_store: &AppStoreReducer,
    server_id: &str,
    response: upstream::ThreadReadResponse,
) -> Result<(), RpcError> {
    let turns = response.thread.turns.clone();
    let thread_id = response.thread.id.clone();
    let existing = app_store
        .snapshot()
        .threads
        .get(&ThreadKey {
            server_id: server_id.to_string(),
            thread_id: thread_id.to_string(),
        })
        .cloned();
    let mut snapshot = thread_snapshot_from_upstream_thread_with_overrides(
        server_id,
        response.thread,
        None,
        None,
        response.approval_policy.map(Into::into),
        response.sandbox.map(Into::into),
    )
    .map_err(RpcError::Deserialization)?;
    if let Some(existing) = existing.as_ref() {
        copy_thread_runtime_fields(existing, &mut snapshot);
    }
    reconcile_active_turn(existing.as_ref(), &mut snapshot, &turns);
    app_store.upsert_thread_snapshot(snapshot);
    Ok(())
}

pub(super) fn thread_snapshot_from_upstream_thread_state(
    server_id: &str,
    thread: upstream::Thread,
    model: Option<String>,
    reasoning_effort: Option<String>,
    effective_approval_policy: Option<crate::types::AppAskForApproval>,
    effective_sandbox_policy: Option<crate::types::AppSandboxPolicy>,
    active_turn_id: Option<String>,
) -> ThreadSnapshot {
    let info = ThreadInfo::from(thread.clone());
    let items = crate::conversation::hydrate_turns(&thread.turns, &Default::default());
    let mut snapshot = ThreadSnapshot::from_info(server_id, info);
    snapshot.items = items;
    snapshot.model = model;
    snapshot.reasoning_effort = reasoning_effort;
    snapshot.effective_approval_policy = effective_approval_policy;
    snapshot.effective_sandbox_policy = effective_sandbox_policy;
    snapshot.active_turn_id = active_turn_id.or_else(|| active_turn_id_from_turns(&thread.turns));
    snapshot
}

pub(super) fn active_turn_id_from_turns(turns: &[upstream::Turn]) -> Option<String> {
    turns
        .iter()
        .rev()
        .find(|turn| matches!(turn.status, upstream::TurnStatus::InProgress))
        .map(|turn| turn.id.clone())
}

/// Decide active-turn state for a freshly-rebuilt thread snapshot, given the
/// caller's existing snapshot (if any) and the upstream turn list the rebuild
/// was derived from.
///
/// Rules:
/// - If `target` already shows an InProgress turn, trust it.
/// - Otherwise, if existing has an active turn:
///   - With no turn list available (e.g. include_turns=false), preserve local
///     state — we have no evidence the turn ended.
///   - With a turn list, preserve only if our local id appears as InProgress
///     (defensive); otherwise honor the rebuild and clear.
/// - `info.status` is derived from the resolved `active_turn_id`: Active iff
///   Some, otherwise the upstream-supplied value is left untouched.
pub fn reconcile_active_turn(
    existing: Option<&ThreadSnapshot>,
    target: &mut ThreadSnapshot,
    upstream_turns: &[upstream::Turn],
) {
    if target.active_turn_id.is_some() {
        target.info.status = ThreadSummaryStatus::Active;
        return;
    }
    let Some(local_id) = existing.and_then(|t| t.active_turn_id.clone()) else {
        return;
    };
    let preserve = if upstream_turns.is_empty() {
        true
    } else {
        upstream_turns
            .iter()
            .any(|t| t.id == local_id && matches!(t.status, upstream::TurnStatus::InProgress))
    };
    if preserve {
        target.active_turn_id = Some(local_id);
        target.info.status = ThreadSummaryStatus::Active;
    }
}

pub(super) fn approval_response_json(
    approval: &PendingApproval,
    seed: Option<&PendingApprovalSeed>,
    decision: ApprovalDecisionValue,
) -> Result<serde_json::Value, RpcError> {
    match approval.kind {
        crate::types::ApprovalKind::Command => {
            serde_json::to_value(upstream::CommandExecutionRequestApprovalResponse {
                decision: match decision {
                    ApprovalDecisionValue::Accept => {
                        upstream::CommandExecutionApprovalDecision::Accept
                    }
                    ApprovalDecisionValue::AcceptForSession => {
                        upstream::CommandExecutionApprovalDecision::AcceptForSession
                    }
                    ApprovalDecisionValue::Decline => {
                        upstream::CommandExecutionApprovalDecision::Decline
                    }
                    ApprovalDecisionValue::Cancel => {
                        upstream::CommandExecutionApprovalDecision::Cancel
                    }
                },
            })
        }
        crate::types::ApprovalKind::FileChange => {
            serde_json::to_value(upstream::FileChangeRequestApprovalResponse {
                decision: match decision {
                    ApprovalDecisionValue::Accept => upstream::FileChangeApprovalDecision::Accept,
                    ApprovalDecisionValue::AcceptForSession => {
                        upstream::FileChangeApprovalDecision::AcceptForSession
                    }
                    ApprovalDecisionValue::Decline => upstream::FileChangeApprovalDecision::Decline,
                    ApprovalDecisionValue::Cancel => upstream::FileChangeApprovalDecision::Cancel,
                },
            })
        }
        crate::types::ApprovalKind::Permissions | crate::types::ApprovalKind::McpElicitation => {
            let requested_permissions = seed
                .map(|seed| seed.raw_params.clone())
                .and_then(|value: serde_json::Value| value.get("permissions").cloned())
                .and_then(|value| {
                    serde_json::from_value::<upstream::GrantedPermissionProfile>(value).ok()
                })
                .unwrap_or(upstream::GrantedPermissionProfile {
                    network: None,
                    file_system: None,
                });
            serde_json::to_value(upstream::PermissionsRequestApprovalResponse {
                permissions: match decision {
                    ApprovalDecisionValue::Accept | ApprovalDecisionValue::AcceptForSession => {
                        requested_permissions
                    }
                    ApprovalDecisionValue::Decline | ApprovalDecisionValue::Cancel => {
                        upstream::GrantedPermissionProfile {
                            network: None,
                            file_system: None,
                        }
                    }
                },
                scope: match decision {
                    ApprovalDecisionValue::AcceptForSession => {
                        upstream::PermissionGrantScope::Session
                    }
                    _ => upstream::PermissionGrantScope::Turn,
                },
                strict_auto_review: None,
            })
        }
    }
    .map_err(|e| RpcError::Deserialization(format!("serialize approval response: {e}")))
}

pub(super) fn approval_request_id(
    approval: &PendingApproval,
    seed: Option<&PendingApprovalSeed>,
) -> upstream::RequestId {
    seed.map(|seed| seed.request_id.clone())
        .unwrap_or_else(|| fallback_server_request_id(&approval.id))
}

pub(super) fn fallback_server_request_id(id: &str) -> upstream::RequestId {
    id.parse::<i64>()
        .map(upstream::RequestId::Integer)
        .unwrap_or_else(|_| upstream::RequestId::String(id.to_string()))
}

pub(super) fn server_request_id_json(id: upstream::RequestId) -> serde_json::Value {
    match id {
        upstream::RequestId::Integer(value) => serde_json::Value::Number(value.into()),
        upstream::RequestId::String(value) => serde_json::Value::String(value),
    }
}
