use super::{SavedAppUpdateResult, StructuredResponseResult};
use crate::MobileClient;
use crate::next_request_id;
use codex_app_server_protocol as upstream;

/// Return the "Apps saved in this thread so far: …" context line for
/// the given `thread_id`, or `None` when:
/// - `thread_id` is `None` (no thread yet, nothing to reference),
/// - the saved-apps directory hasn't been registered by the platform,
/// - the directory has no apps for this thread.
fn saved_apps_context_line(client: &MobileClient, thread_id: Option<&str>) -> Option<String> {
    let thread_id = thread_id?;
    let directory = {
        let guard = client
            .saved_apps_directory
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.clone()?
    };
    let apps = crate::saved_apps::saved_apps_for_thread(directory, thread_id.to_string());
    if apps.is_empty() {
        return None;
    }
    let joined = apps
        .iter()
        .map(|app| format!("{} ({})", app.app_id, app.title))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!("Apps saved in this thread so far: {joined}"))
}

/// Prepend the saved-apps context line to `existing` if the thread has
/// any saved apps. If there are no apps (or no thread yet), returns
/// `existing` unchanged so callers can feed this directly into the RPC
/// request params.
pub(super) fn splice_saved_apps_context(
    client: &MobileClient,
    thread_id: Option<&str>,
    existing: Option<String>,
) -> Option<String> {
    let Some(line) = saved_apps_context_line(client, thread_id) else {
        return existing;
    };
    Some(match existing {
        Some(prev) if !prev.trim().is_empty() => format!("{line}\n\n{prev}"),
        _ => line,
    })
}

/// Prepend the generative-UI preamble when this thread is registering the
/// `show_widget` / `visualize_read_me` dynamic tools (i.e., a local-server
/// thread per the I3/A3 gating rule). Platforms decide whether to pass
/// these tools in; Rust just checks the request and conditionally injects
/// the nudge.
pub(super) fn splice_generative_ui_preamble(
    dynamic_tools: &Option<Vec<crate::types::models::AppDynamicToolSpec>>,
    existing: Option<String>,
) -> Option<String> {
    let has_show_widget = dynamic_tools
        .as_ref()
        .map(|tools| tools.iter().any(|t| t.name == "show_widget"))
        .unwrap_or(false);
    if !has_show_widget {
        return existing;
    }
    let preamble = crate::widget_guidelines::GENERATIVE_UI_PREAMBLE;
    Some(match existing {
        Some(prev) if !prev.trim().is_empty() => format!("{preamble}\n\n{prev}"),
        _ => preamble.to_string(),
    })
}

const STRUCTURED_RESPONSE_TIMEOUT_SECS: u64 = 60;

const SAVED_APP_UPDATE_TIMEOUT_SECS: u64 = 120;
const SAVED_APP_UPDATE_DEFAULT_MODEL: &str = "gpt-5.3-codex-spark";

pub(super) fn is_stale_thread_error(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    lower.contains("thread not found")
        || lower.contains("conversation not found")
        || lower.contains("unknown thread")
        || lower.contains("no such thread")
}

async fn start_ephemeral_thread_for_structured(
    client: &crate::MobileClient,
    server_id: &str,
) -> Result<String, String> {
    let start_params = upstream::ThreadStartParams {
        model: None,
        model_provider: None,
        service_tier: None,
        cwd: None,
        runtime_workspace_roots: None,
        approval_policy: None,
        approvals_reviewer: None,
        sandbox: None,
        permissions: None,
        config: None,
        service_name: None,
        base_instructions: None,
        developer_instructions: None,
        personality: None,
        ephemeral: Some(true),
        session_start_source: None,
        thread_source: None,
        environments: None,
        dynamic_tools: None,
        mock_experimental_field: None,
        experimental_raw_events: false,
        persist_extended_history: false,
    };
    let response: upstream::ThreadStartResponse = client
        .request_typed_for_server(
            server_id,
            upstream::ClientRequest::ThreadStart {
                request_id: upstream::RequestId::Integer(next_request_id()),
                params: start_params,
            },
        )
        .await
        .map_err(|e| format!("thread/start failed: {e}"))?;
    Ok(response.thread.id)
}

async fn run_structured_turn(
    client: &crate::MobileClient,
    server_id: &str,
    thread_id: &str,
    prompt: &str,
    output_schema: serde_json::Value,
) -> Result<String, StructuredTurnError> {
    // Subscribe BEFORE sending the turn so we don't miss a very fast
    // completion. `UiEvent` is the typed, thread-scoped event stream the
    // mobile client already fans out to the store.
    let mut events_rx = client.event_processor.subscribe();

    let turn_params = upstream::TurnStartParams {
        thread_id: thread_id.to_string(),
        input: vec![upstream::UserInput::Text {
            text: prompt.to_string(),
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
        output_schema: Some(output_schema),
        collaboration_mode: None,
    };
    let turn_outcome: Result<upstream::TurnStartResponse, _> = client
        .request_typed_for_server(
            server_id,
            upstream::ClientRequest::TurnStart {
                request_id: upstream::RequestId::Integer(next_request_id()),
                params: turn_params,
            },
        )
        .await;
    if let Err(e) = turn_outcome {
        if is_stale_thread_error(&e) {
            return Err(StructuredTurnError::StaleThread);
        }
        return Err(StructuredTurnError::Fatal(format!(
            "turn/start failed: {e}"
        )));
    }

    let wait_outcome = tokio::time::timeout(
        std::time::Duration::from_secs(STRUCTURED_RESPONSE_TIMEOUT_SECS),
        async {
            let mut last_agent_text: Option<String> = None;
            loop {
                match events_rx.recv().await {
                    Ok(crate::session::events::UiEvent::ItemCompleted { notification, .. })
                        if notification.thread_id == thread_id =>
                    {
                        if let upstream::ThreadItem::AgentMessage { text, .. } = &notification.item
                        {
                            last_agent_text = Some(text.clone());
                        }
                    }
                    Ok(crate::session::events::UiEvent::TurnCompleted { key, error, .. })
                        if key.thread_id == thread_id && key.server_id == server_id =>
                    {
                        return Ok((last_agent_text, error));
                    }
                    Ok(_) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        return Err("event stream closed".to_string());
                    }
                }
            }
        },
    )
    .await;

    match wait_outcome {
        Ok(Ok((_, Some(err)))) => Err(StructuredTurnError::Fatal(err)),
        Ok(Ok((Some(text), None))) => Ok(text),
        Ok(Ok((None, None))) => Err(StructuredTurnError::Fatal(
            "turn completed with no assistant message".to_string(),
        )),
        Ok(Err(msg)) => Err(StructuredTurnError::Fatal(msg)),
        Err(_) => Err(StructuredTurnError::Fatal(format!(
            "timed out after {STRUCTURED_RESPONSE_TIMEOUT_SECS}s waiting for structured response"
        ))),
    }
}

enum StructuredTurnError {
    StaleThread,
    Fatal(String),
}

pub(super) async fn perform_structured_response(
    client: &crate::MobileClient,
    server_id: String,
    cached_thread_id: Option<String>,
    prompt: String,
    output_schema_json: String,
) -> StructuredResponseResult {
    if prompt.trim().is_empty() {
        return StructuredResponseResult::Error {
            message: "prompt is empty".to_string(),
        };
    }
    let schema: serde_json::Value = match serde_json::from_str(&output_schema_json) {
        Ok(v) => v,
        Err(e) => {
            return StructuredResponseResult::Error {
                message: format!("invalid responseFormat JSON schema: {e}"),
            };
        }
    };

    // First attempt: use cached id if provided, otherwise start fresh.
    let mut thread_id = match cached_thread_id {
        Some(id) if !id.trim().is_empty() => id,
        _ => match start_ephemeral_thread_for_structured(client, &server_id).await {
            Ok(id) => id,
            Err(e) => return StructuredResponseResult::Error { message: e },
        },
    };

    match run_structured_turn(client, &server_id, &thread_id, &prompt, schema.clone()).await {
        Ok(text) => StructuredResponseResult::Success {
            thread_id,
            response_json: text,
        },
        Err(StructuredTurnError::StaleThread) => {
            // Cached thread is gone. Reseat and retry exactly once.
            thread_id = match start_ephemeral_thread_for_structured(client, &server_id).await {
                Ok(id) => id,
                Err(e) => {
                    return StructuredResponseResult::Error {
                        message: format!("stale-thread recovery failed: {e}"),
                    };
                }
            };
            match run_structured_turn(client, &server_id, &thread_id, &prompt, schema).await {
                Ok(text) => StructuredResponseResult::Success {
                    thread_id,
                    response_json: text,
                },
                Err(StructuredTurnError::StaleThread) => StructuredResponseResult::Error {
                    message: "thread became stale again on retry".to_string(),
                },
                Err(StructuredTurnError::Fatal(msg)) => {
                    StructuredResponseResult::Error { message: msg }
                }
            }
        }
        Err(StructuredTurnError::Fatal(msg)) => StructuredResponseResult::Error { message: msg },
    }
}

pub(super) async fn perform_update_saved_app(
    client: &crate::MobileClient,
    server_id: String,
    directory: String,
    app_id: String,
    user_prompt: String,
) -> SavedAppUpdateResult {
    // 1. Load the current saved-app payload so we can seed the thread.
    let current = match crate::saved_apps::saved_app_get(directory.clone(), app_id.clone()) {
        Some(payload) => payload,
        None => {
            return SavedAppUpdateResult::Error {
                message: format!("saved app '{app_id}' not found"),
            };
        }
    };
    let shape_summary = crate::saved_apps::abbreviated_state_shape(&directory, &app_id)
        .unwrap_or_else(|| "  (no saved state yet)".to_string());

    let requested_server_id = server_id;
    let snapshot = client.app_store.snapshot();
    let server_id = match choose_saved_app_update_server_id(&requested_server_id, &snapshot) {
        Some(server_id) => server_id,
        None => {
            return SavedAppUpdateResult::Error {
                message: "saved-app updates require a connected local server because app files live on this device".to_string(),
            };
        }
    };
    if server_id != requested_server_id {
        tracing::info!(
            "update_saved_app: routing edit thread to local server {} instead of requested server {}",
            server_id,
            requested_server_id
        );
    }

    // Inherit the origin thread's model / reasoning settings when the
    // thread is still known to the store. If the app has no recoverable
    // origin settings, fall back to the fast saved-app update defaults.
    let inherited = inherited_settings_for_origin(
        client,
        &requested_server_id,
        current.app.origin_thread_id.as_deref(),
    );
    let inherited = if inherited_settings_empty(&inherited) && requested_server_id != server_id {
        inherited_settings_for_origin(client, &server_id, current.app.origin_thread_id.as_deref())
    } else {
        inherited
    };
    let (model, reasoning_effort) = inherited;
    let has_complete_inherited_settings = model.is_some() && reasoning_effort.is_some();
    let model = model.unwrap_or_else(|| SAVED_APP_UPDATE_DEFAULT_MODEL.to_string());
    let reasoning_effort = reasoning_effort.unwrap_or(crate::types::ReasoningEffort::Low);
    let service_tier = if has_complete_inherited_settings {
        None
    } else {
        Some(Some(
            crate::types::server_requests::service_tier_into_upstream_string(
                crate::types::ServiceTier::Fast,
            ),
        ))
    };

    // Resolve the on-disk HTML path. saved_apps.rs writes at
    // `<directory>/html/<id>.html` directly (no extra `apps/` segment),
    // so we read from there too. The Rust live-sync poller uses this
    // iOS-sandbox path verbatim.
    let html_dir = std::path::Path::new(&directory).join("html");
    let html_filename = format!("{app_id}.html");
    let html_path = html_dir.join(&html_filename);
    let initial_html = current.widget_html.clone();

    // Path the model gets as its working directory. Local server processes
    // receive the same on-disk directory that the live-sync poller reads.
    let thread_cwd = html_dir.to_string_lossy().into_owned();

    let developer_instructions = build_saved_app_update_seed(
        &current.app.title,
        current.app.schema_version,
        &html_filename,
        &shape_summary,
    );

    // 2. Start a visible thread on the server rooted at the
    //    saved-apps HTML directory. The model uses its regular file-
    //    editing tools (apply_patch, shell) to modify the HTML file on
    //    disk — no dynamic_tools, no show_widget round-trip.
    let start_params = upstream::ThreadStartParams {
        model: Some(model.clone()),
        model_provider: None,
        service_tier: service_tier.clone(),
        cwd: Some(thread_cwd.clone()),
        runtime_workspace_roots: None,
        approval_policy: Some(upstream::AskForApproval::Never),
        approvals_reviewer: None,
        sandbox: Some(upstream::SandboxMode::DangerFullAccess),
        permissions: None,
        config: None,
        service_name: None,
        base_instructions: None,
        developer_instructions: Some(developer_instructions),
        personality: None,
        ephemeral: Some(false),
        session_start_source: None,
        thread_source: None,
        environments: None,
        dynamic_tools: None,
        mock_experimental_field: None,
        experimental_raw_events: false,
        persist_extended_history: false,
    };
    let thread_response: upstream::ThreadStartResponse = match client
        .request_typed_for_server(
            &server_id,
            upstream::ClientRequest::ThreadStart {
                request_id: upstream::RequestId::Integer(crate::next_request_id()),
                params: start_params,
            },
        )
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return SavedAppUpdateResult::Error {
                message: format!("thread/start failed: {e}"),
            };
        }
    };
    let thread_id = thread_response.thread.id.clone();

    // 3. Subscribe to store updates BEFORE sending the turn so we don't
    //    miss an extremely fast completion. We wait for ThreadMetadataChanged
    //    on our thread with `active_turn_id = None` AND `status = Idle`
    //    AFTER we've seen the turn become active at least once.
    let mut updates_rx = client.app_store.subscribe();

    // 4. Send the user's update prompt on this thread.
    let turn_params = upstream::TurnStartParams {
        thread_id: thread_id.clone(),
        input: vec![upstream::UserInput::Text {
            text: user_prompt.clone(),
            text_elements: Vec::new(),
        }],
        responsesapi_client_metadata: None,
        cwd: None,
        runtime_workspace_roots: None,
        approval_policy: Some(upstream::AskForApproval::Never),
        approvals_reviewer: None,
        sandbox_policy: Some(upstream::SandboxPolicy::DangerFullAccess),
        environments: None,
        permissions: None,
        model: Some(model),
        service_tier,
        effort: Some(
            crate::types::server_requests::reasoning_effort_into_upstream(reasoning_effort),
        ),
        summary: None,
        personality: None,
        output_schema: None,
        collaboration_mode: None,
    };
    let turn_start_outcome: Result<upstream::TurnStartResponse, _> = client
        .request_typed_for_server(
            &server_id,
            upstream::ClientRequest::TurnStart {
                request_id: upstream::RequestId::Integer(crate::next_request_id()),
                params: turn_params,
            },
        )
        .await;
    if let Err(e) = turn_start_outcome {
        return SavedAppUpdateResult::Error {
            message: format!("turn/start failed: {e}"),
        };
    }

    // 5. Wait for the turn to complete, or time out. While the turn is
    //    still running, sync each distinct HTML file change back through
    //    saved_app_replace_html so preview/detail views can refresh live.
    let wait_outcome = tokio::time::timeout(
        std::time::Duration::from_secs(SAVED_APP_UPDATE_TIMEOUT_SECS),
        wait_for_saved_app_update_turn_and_sync(
            &mut updates_rx,
            &server_id,
            &thread_id,
            &directory,
            &app_id,
            &html_path,
            current.app.width,
            current.app.height,
            initial_html,
        ),
    )
    .await;

    let final_app = match wait_outcome {
        Ok(Ok(Some(app))) => app,
        Ok(Ok(None)) => {
            return SavedAppUpdateResult::Error {
                message: "no changes were made to the app".to_string(),
            };
        }
        Ok(Err(e)) => {
            return SavedAppUpdateResult::Error { message: e };
        }
        Err(_) => {
            return SavedAppUpdateResult::Error {
                message: format!(
                    "update timed out after {SAVED_APP_UPDATE_TIMEOUT_SECS}s waiting for turn to complete"
                ),
            };
        }
    };

    SavedAppUpdateResult::Success { app: final_app }
}

async fn wait_for_saved_app_update_turn_and_sync(
    updates_rx: &mut tokio::sync::broadcast::Receiver<crate::store::updates::AppStoreUpdateRecord>,
    server_id: &str,
    thread_id: &str,
    directory: &str,
    app_id: &str,
    html_path: &std::path::Path,
    width: f64,
    height: f64,
    initial_html: String,
) -> Result<Option<crate::saved_apps::SavedApp>, String> {
    let mut saw_active = false;
    let mut last_synced_html = initial_html;
    let mut latest_app: Option<crate::saved_apps::SavedApp> = None;
    let mut poll = tokio::time::interval(std::time::Duration::from_millis(750));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = poll.tick() => {
                match sync_saved_app_html_change(
                    directory,
                    app_id,
                    html_path,
                    width,
                    height,
                    &mut last_synced_html,
                    false,
                ) {
                    Ok(Some(app)) => latest_app = Some(app),
                    Ok(None) => {}
                    Err(e) => tracing::warn!("update_saved_app: live HTML sync skipped: {e}"),
                }
            }
            update = updates_rx.recv() => {
                match update {
                    Ok(crate::store::updates::AppStoreUpdateRecord::ThreadMetadataChanged {
                        state,
                        ..
                    }) if state.key.thread_id == thread_id && state.key.server_id == server_id => {
                        if state.active_turn_id.is_some() {
                            saw_active = true;
                        } else if saw_active {
                            if let Some(app) = sync_saved_app_html_change(
                                directory,
                                app_id,
                                html_path,
                                width,
                                height,
                                &mut last_synced_html,
                                true,
                            )? {
                                latest_app = Some(app);
                            }
                            return Ok(latest_app);
                        }
                    }
                    Ok(_) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        return Err("update channel closed".to_string());
                    }
                }
            }
        }
    }
}

fn sync_saved_app_html_change(
    directory: &str,
    app_id: &str,
    html_path: &std::path::Path,
    width: f64,
    height: f64,
    last_synced_html: &mut String,
    final_check: bool,
) -> Result<Option<crate::saved_apps::SavedApp>, String> {
    let html = match std::fs::read_to_string(html_path) {
        Ok(html) => html,
        Err(e) if !final_check => {
            tracing::warn!("update_saved_app: could not read live HTML change: {e}");
            return Ok(None);
        }
        Err(e) => {
            return Err(format!("could not read updated HTML: {e}"));
        }
    };

    if html == *last_synced_html {
        return Ok(None);
    }

    let app = crate::saved_apps::saved_app_replace_html(
        directory.to_string(),
        app_id.to_string(),
        html.clone(),
        width,
        height,
    )
    .map_err(|e| format!("replace_html failed: {e}"))?;
    *last_synced_html = html;
    Ok(Some(app))
}

fn choose_saved_app_update_server_id(
    requested_server_id: &str,
    snapshot: &crate::store::AppSnapshot,
) -> Option<String> {
    let mut local_server_ids = snapshot
        .servers
        .values()
        .filter(|server| {
            server.is_local
                && matches!(server.health, crate::store::ServerHealthSnapshot::Connected)
        })
        .map(|server| server.server_id.clone())
        .collect::<Vec<_>>();

    if local_server_ids
        .iter()
        .any(|server_id| server_id == requested_server_id)
    {
        return Some(requested_server_id.to_string());
    }
    if local_server_ids
        .iter()
        .any(|server_id| server_id == "local")
    {
        return Some("local".to_string());
    }

    local_server_ids.sort();
    local_server_ids.dedup();
    local_server_ids.into_iter().next()
}

type InheritedSettings = (
    Option<String>,
    Option<crate::types::models::ReasoningEffort>,
);

fn inherited_settings_empty(settings: &InheritedSettings) -> bool {
    settings.0.is_none() && settings.1.is_none()
}

/// Look up an origin thread in the app store and extract its effective
/// model / reasoning settings so the saved-app update thread can run with
/// the same model configuration the user chose for the source conversation.
/// Returns `(None, None)` when the thread is unknown, never hydrated, or
/// belongs to a different server.
fn inherited_settings_for_origin(
    client: &crate::MobileClient,
    server_id: &str,
    origin_thread_id: Option<&str>,
) -> InheritedSettings {
    use crate::types::models::ReasoningEffort;

    let Some(thread_id) = origin_thread_id.and_then(|s| {
        let t = s.trim();
        if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        }
    }) else {
        return (None, None);
    };

    let snapshot = client.app_store.snapshot();
    let key = crate::types::ThreadKey {
        server_id: server_id.to_string(),
        thread_id,
    };
    let Some(thread) = snapshot.threads.get(&key) else {
        return (None, None);
    };

    let model = thread
        .model
        .as_ref()
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty());
    let effort = thread.reasoning_effort.as_deref().and_then(|raw| {
        match raw.trim().to_ascii_lowercase().as_str() {
            "none" => Some(ReasoningEffort::None),
            "minimal" => Some(ReasoningEffort::Minimal),
            "low" => Some(ReasoningEffort::Low),
            "medium" => Some(ReasoningEffort::Medium),
            "high" => Some(ReasoningEffort::High),
            "xhigh" | "x-high" => Some(ReasoningEffort::XHigh),
            "max" => Some(ReasoningEffort::Max),
            _ => None,
        }
    });
    (model, effort)
}

fn build_saved_app_update_seed(
    title: &str,
    schema_version: u32,
    html_filename: &str,
    state_shape_summary: &str,
) -> String {
    let app_guidelines = crate::widget_guidelines::get_guidelines(&["app".to_string()]);
    format!(
        "You are updating an existing saved app called \"{title}\".\n\n\
The app's HTML lives in the current working directory as `./{html_filename}`. \
**Read it first, then edit it with `apply_patch`** (or rewrite it \
wholesale if the change is extensive). Do NOT call `show_widget` — that \
tool is not available on this thread. Your job is to modify the HTML \
file on disk.\n\n\
The app persists user data via `window.loadAppState()` / `window.saveAppState()`. \
The current state schema_version is {schema_version}. You MUST:\n\n\
- Preserve the `loadAppState`/`saveAppState` contract so the user's existing \
data keeps working.\n\
- If state-field shapes changed, migrate them defensively on load.\n\
- Keep the widget self-contained (no cross-file deps; inline CSS/JS is fine).\n\n\
Abbreviated shape of the current persisted state (top-level keys + sample values; \
the raw user data is NOT included):\n\
```\n{{\n{state_shape_summary}\n}}\n```\n\n\
---\n\n\
Widget construction guidelines (for reference when making UI decisions):\n\n\
{app_guidelines}"
    )
}

#[cfg(test)]
mod tests {
    use super::{choose_saved_app_update_server_id, splice_generative_ui_preamble};
    use crate::store::snapshot::ServerTransportDiagnostics;
    use crate::store::{AppSnapshot, ServerHealthSnapshot, ServerSnapshot};
    use crate::types::models::AppDynamicToolSpec;
    use crate::widget_guidelines::GENERATIVE_UI_PREAMBLE;
    use std::collections::HashMap;

    fn show_widget_spec() -> AppDynamicToolSpec {
        AppDynamicToolSpec {
            name: "show_widget".to_string(),
            description: "test".to_string(),
            input_schema_json: "{}".to_string(),
            defer_loading: false,
        }
    }

    fn server_snapshot(
        server_id: &str,
        is_local: bool,
        health: ServerHealthSnapshot,
    ) -> ServerSnapshot {
        ServerSnapshot {
            server_id: server_id.to_string(),
            display_name: server_id.to_string(),
            host: "127.0.0.1".to_string(),
            port: 0,
            wake_mac: None,
            is_local,
            health,
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
        }
    }

    fn app_snapshot_with_servers(servers: Vec<ServerSnapshot>) -> AppSnapshot {
        AppSnapshot {
            servers: servers
                .into_iter()
                .map(|server| (server.server_id.clone(), server))
                .collect::<HashMap<_, _>>(),
            ..AppSnapshot::default()
        }
    }

    #[test]
    fn preamble_prepended_when_show_widget_registered() {
        let tools = Some(vec![show_widget_spec()]);
        let result = splice_generative_ui_preamble(&tools, Some("user instructions".to_string()));
        let out = result.expect("expected Some");
        assert!(out.starts_with(GENERATIVE_UI_PREAMBLE));
        assert!(out.ends_with("user instructions"));
    }

    #[test]
    fn preamble_skipped_without_show_widget() {
        let other = AppDynamicToolSpec {
            name: "list_servers".to_string(),
            description: "x".to_string(),
            input_schema_json: "{}".to_string(),
            defer_loading: false,
        };
        let tools = Some(vec![other]);
        let result = splice_generative_ui_preamble(&tools, Some("user instructions".to_string()));
        assert_eq!(result.as_deref(), Some("user instructions"));
    }

    #[test]
    fn preamble_used_alone_when_no_existing_instructions() {
        let tools = Some(vec![show_widget_spec()]);
        assert_eq!(
            splice_generative_ui_preamble(&tools, None).as_deref(),
            Some(GENERATIVE_UI_PREAMBLE)
        );
    }

    #[test]
    fn preamble_skipped_when_no_dynamic_tools() {
        assert_eq!(
            splice_generative_ui_preamble(&None, Some("keep".to_string())).as_deref(),
            Some("keep")
        );
    }

    #[test]
    fn saved_app_update_server_keeps_requested_local_server() {
        let snapshot = app_snapshot_with_servers(vec![
            server_snapshot("local", true, ServerHealthSnapshot::Connected),
            server_snapshot("remote", false, ServerHealthSnapshot::Connected),
        ]);

        let chosen = choose_saved_app_update_server_id("local", &snapshot);
        assert_eq!(chosen.as_deref(), Some("local"));
    }

    #[test]
    fn saved_app_update_server_routes_remote_request_to_local_server() {
        let snapshot = app_snapshot_with_servers(vec![
            server_snapshot("remote", false, ServerHealthSnapshot::Connected),
            server_snapshot("local", true, ServerHealthSnapshot::Connected),
        ]);

        let chosen = choose_saved_app_update_server_id("remote", &snapshot);
        assert_eq!(chosen.as_deref(), Some("local"));
    }

    #[test]
    fn saved_app_update_server_ignores_disconnected_local_server() {
        let snapshot = app_snapshot_with_servers(vec![
            server_snapshot("remote", false, ServerHealthSnapshot::Connected),
            server_snapshot("local", true, ServerHealthSnapshot::Disconnected),
        ]);

        let chosen = choose_saved_app_update_server_id("remote", &snapshot);
        assert_eq!(chosen, None);
    }
}
