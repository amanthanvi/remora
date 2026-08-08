use super::*;

const SUBAGENT_METADATA_HYDRATE_DELAYS_MS: [u64; 3] = [150, 800, 2500];

pub(super) fn spawn_store_listener(
    app_store: Arc<AppStoreReducer>,
    sessions: Arc<RwLock<HashMap<String, Arc<ServerSession>>>>,
    mut rx: broadcast::Receiver<UiEvent>,
) {
    MobileClient::spawn_detached(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    app_store.apply_ui_event(&event);
                    maybe_hydrate_collab_agent_metadata(
                        Arc::clone(&app_store),
                        Arc::clone(&sessions),
                        &event,
                    );
                    if let UiEvent::TurnCompleted { key, .. } = &event {
                        let app_store = Arc::clone(&app_store);
                        let sessions = Arc::clone(&sessions);
                        let key = key.clone();
                        MobileClient::spawn_detached(async move {
                            let Some(client) = listener_mobile_client(&app_store, &sessions) else {
                                warn!(
                                    "MobileClient: queued follow-up skipped because the listener client is unavailable"
                                );
                                return;
                            };
                            maybe_send_next_local_queued_follow_up(client, key).await;
                        });
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    warn!("MobileClient: lagged {skipped} UI events");
                    let app_store = Arc::clone(&app_store);
                    let sessions = Arc::clone(&sessions);
                    MobileClient::spawn_detached(async move {
                        let Some(client) = listener_mobile_client(&app_store, &sessions) else {
                            warn!(
                                "MobileClient: lag reconcile skipped because the listener client is unavailable"
                            );
                            return;
                        };
                        reconcile_after_store_listener_lag(&client).await;
                    });
                }
            }
        }
    });
}

fn listener_mobile_client(
    app_store: &Arc<AppStoreReducer>,
    sessions: &Arc<RwLock<HashMap<String, Arc<ServerSession>>>>,
) -> Option<Arc<MobileClient>> {
    crate::ffi::shared::shared_mobile_client_if_initialized().filter(|client| {
        Arc::ptr_eq(&client.app_store, app_store) && Arc::ptr_eq(&client.sessions, sessions)
    })
}

async fn reconcile_after_store_listener_lag(client: &MobileClient) {
    let connected_server_ids = client
        .sessions_read()
        .keys()
        .cloned()
        .collect::<HashSet<_>>();
    let keys = client
        .app_store
        .snapshot()
        .threads
        .keys()
        .filter(|key| connected_server_ids.contains(&key.server_id))
        .cloned()
        .collect::<Vec<_>>();

    for key in keys {
        if let Err(error) = client
            .force_refresh_thread_authoritative(&key.server_id, &key.thread_id)
            .await
        {
            warn!(
                "MobileClient: lag reconcile failed for {} thread {}: {}",
                key.server_id, key.thread_id, error
            );
        }
    }
}

fn maybe_hydrate_collab_agent_metadata(
    app_store: Arc<AppStoreReducer>,
    sessions: Arc<RwLock<HashMap<String, Arc<ServerSession>>>>,
    event: &UiEvent,
) {
    let Some((server_id, receiver_thread_ids)) = collab_receiver_thread_ids(event) else {
        return;
    };
    if receiver_thread_ids.is_empty() {
        return;
    }

    for thread_id in receiver_thread_ids {
        if !subagent_label_missing(&app_store, &server_id, &thread_id) {
            continue;
        }
        let app_store = Arc::clone(&app_store);
        let sessions = Arc::clone(&sessions);
        let server_id = server_id.clone();
        MobileClient::spawn_detached(async move {
            for delay_ms in std::iter::once(0_u64).chain(SUBAGENT_METADATA_HYDRATE_DELAYS_MS) {
                if !subagent_label_missing(&app_store, &server_id, &thread_id) {
                    return;
                }
                if delay_ms > 0 {
                    tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
                    if !subagent_label_missing(&app_store, &server_id, &thread_id) {
                        return;
                    }
                }

                let session = match sessions.read() {
                    Ok(guard) => guard.get(&server_id).cloned(),
                    Err(error) => {
                        warn!("MobileClient: recovering poisoned sessions read lock");
                        error.into_inner().get(&server_id).cloned()
                    }
                };
                let Some(session) = session else {
                    return;
                };
                if !session_is_current(&sessions, &server_id, &session) {
                    return;
                }

                match read_thread_response_from_app_server(Arc::clone(&session), &thread_id, false)
                    .await
                {
                    Ok(response) => {
                        if !session_is_current(&sessions, &server_id, &session) {
                            return;
                        }
                        if let Err(error) = upsert_thread_snapshot_from_app_server_read_response(
                            &app_store, &server_id, response,
                        ) {
                            warn!(
                                "MobileClient: failed to hydrate collab receiver metadata for server={} thread={}: {}",
                                server_id, thread_id, error
                            );
                            continue;
                        }
                    }
                    Err(error) => {
                        warn!(
                            "MobileClient: failed to read collab receiver metadata for server={} thread={}: {}",
                            server_id, thread_id, error
                        );
                    }
                }
            }
        });
    }
}

fn collab_receiver_thread_ids(event: &UiEvent) -> Option<(String, Vec<String>)> {
    match event {
        UiEvent::ItemStarted { key, notification } => match &notification.item {
            upstream::ThreadItem::CollabAgentToolCall {
                receiver_thread_ids,
                ..
            } if !receiver_thread_ids.is_empty() => Some((
                key.server_id.clone(),
                normalized_thread_ids(receiver_thread_ids.iter().map(String::as_str)),
            )),
            _ => None,
        },
        UiEvent::ItemCompleted { key, notification } => match &notification.item {
            upstream::ThreadItem::CollabAgentToolCall {
                receiver_thread_ids,
                ..
            } if !receiver_thread_ids.is_empty() => Some((
                key.server_id.clone(),
                normalized_thread_ids(receiver_thread_ids.iter().map(String::as_str)),
            )),
            _ => None,
        },
        UiEvent::RawNotification {
            server_id,
            method,
            params,
        } if method.contains("collab") => {
            let ids = params
                .get("receiver_agents")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|value| value.get("thread_id"))
                .filter_map(serde_json::Value::as_str);
            let ids = normalized_thread_ids(ids);
            (!ids.is_empty()).then(|| (server_id.clone(), ids))
        }
        _ => None,
    }
}

fn normalized_thread_ids<'a>(thread_ids: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();
    for thread_id in thread_ids {
        let trimmed = thread_id.trim();
        if trimmed.is_empty() || !seen.insert(trimmed.to_string()) {
            continue;
        }
        normalized.push(trimmed.to_string());
    }
    normalized
}

fn subagent_label_missing(app_store: &AppStoreReducer, server_id: &str, thread_id: &str) -> bool {
    let snapshot = app_store.snapshot();
    let key = ThreadKey {
        server_id: server_id.to_string(),
        thread_id: thread_id.to_string(),
    };
    snapshot.threads.get(&key).is_none_or(|thread| {
        thread
            .info
            .agent_nickname
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_none()
            && thread
                .info
                .agent_role
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .is_none()
    })
}

pub(super) async fn maybe_send_next_local_queued_follow_up(
    client: Arc<MobileClient>,
    key: ThreadKey,
) {
    let snapshot = client.app_store.snapshot();
    let Some(thread) = snapshot.threads.get(&key).cloned() else {
        return;
    };
    if thread.active_turn_id.is_some() || thread.queued_follow_up_drafts.is_empty() {
        return;
    }

    let next = thread.queued_follow_up_drafts.first().cloned();
    let Some(draft) = next else {
        return;
    };
    let result = client
        .start_turn(
            &key.server_id,
            upstream::TurnStartParams {
                thread_id: key.thread_id.clone(),
                input: draft.inputs,
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
                collaboration_mode: None,
            },
        )
        .await;
    if let Err(error) = result {
        client
            .app_store
            .remove_thread_follow_up_draft(&key, &draft.preview.id);
        warn!(
            "MobileClient: failed to autosend queued follow-up for {} thread {}: {}",
            key.server_id, key.thread_id, error
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::connection::TestRequestHandler;
    use std::sync::Mutex as StdMutex;

    fn make_server_config(server_id: &str) -> ServerConfig {
        ServerConfig {
            server_id: server_id.to_string(),
            display_name: server_id.to_string(),
            host: "127.0.0.1".to_string(),
            port: 0,
            websocket_url: Some("ws://127.0.0.1:0".to_string()),
            is_local: false,
            tls: false,
        }
    }

    fn make_thread_snapshot(server_id: &str, thread_id: &str) -> ThreadSnapshot {
        ThreadSnapshot::from_info(
            server_id,
            ThreadInfo {
                id: thread_id.to_string(),
                title: Some("Thread".to_string()),
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
            },
        )
    }

    fn enqueue_follow_up(client: &MobileClient, key: &ThreadKey, text: &str) {
        let inputs = vec![upstream::UserInput::Text {
            text: text.to_string(),
            text_elements: Vec::new(),
        }];
        let draft = queued_follow_up_draft_from_inputs(&inputs, AppQueuedFollowUpKind::Message)
            .expect("queued follow-up draft");
        client.app_store.enqueue_thread_follow_up_draft(key, draft);
    }

    fn successful_turn_start_response() -> serde_json::Value {
        serde_json::json!({
            "turn": {
                "id": "turn-follow-up",
                "items": [],
                "itemsView": "full",
                "status": "inProgress",
                "error": null,
                "startedAt": 1,
                "completedAt": null,
                "durationMs": null
            }
        })
    }

    #[test]
    fn collab_receiver_thread_ids_extracts_spawn_agent_targets() {
        let event = UiEvent::ItemCompleted {
            key: ThreadKey {
                server_id: "srv".to_string(),
                thread_id: "parent".to_string(),
            },
            notification: upstream::ItemCompletedNotification {
                item: upstream::ThreadItem::CollabAgentToolCall {
                    id: "call-1".to_string(),
                    tool: upstream::CollabAgentTool::SpawnAgent,
                    status: upstream::CollabAgentToolCallStatus::Completed,
                    sender_thread_id: "parent".to_string(),
                    receiver_thread_ids: vec![
                        " child-1 ".to_string(),
                        "child-2".to_string(),
                        "child-1".to_string(),
                    ],
                    prompt: None,
                    model: None,
                    reasoning_effort: None,
                    agents_states: HashMap::new(),
                },
                thread_id: "parent".to_string(),
                turn_id: "turn-1".to_string(),
                completed_at_ms: 0,
            },
        };

        assert_eq!(
            collab_receiver_thread_ids(&event),
            Some((
                "srv".to_string(),
                vec!["child-1".to_string(), "child-2".to_string()],
            ))
        );
    }

    #[test]
    fn collab_receiver_thread_ids_extracts_legacy_receiver_agents() {
        let event = UiEvent::RawNotification {
            server_id: "srv".to_string(),
            method: "codex/event/collab_wait_end".to_string(),
            params: serde_json::json!({
                "receiver_agents": [
                    { "thread_id": "child-1" },
                    { "thread_id": " child-2 " },
                    { "thread_id": "child-1" }
                ]
            }),
        };

        assert_eq!(
            collab_receiver_thread_ids(&event),
            Some((
                "srv".to_string(),
                vec!["child-1".to_string(), "child-2".to_string()],
            ))
        );
    }

    #[tokio::test]
    async fn queued_follow_up_uses_canonical_runtime_routing_and_plan_mode() {
        let client = Arc::new(MobileClient::new());
        let server_id = "srv";
        let thread_id = "thread-1";
        let key = ThreadKey {
            server_id: server_id.to_string(),
            thread_id: thread_id.to_string(),
        };
        let config = make_server_config(server_id);
        client
            .app_store
            .upsert_server(&config, ServerHealthSnapshot::Connected);
        let mut thread = make_thread_snapshot(server_id, thread_id);
        thread.agent_runtime_kind = "claude".to_string();
        thread.collaboration_mode = AppModeKind::Plan;
        thread.model = Some("claude-sonnet-4.5".to_string());
        client.app_store.upsert_thread_snapshot(thread);
        enqueue_follow_up(&client, &key, "continue");

        let requests = Arc::new(StdMutex::new(Vec::<String>::new()));
        let codex_handler: TestRequestHandler = {
            let requests = Arc::clone(&requests);
            Arc::new(move |request| {
                requests
                    .lock()
                    .expect("request log lock")
                    .push(format!("codex:{}", request.method()));
                Err(RpcError::Deserialization(
                    "queued follow-up must not use codex".to_string(),
                ))
            })
        };
        let claude_handler: TestRequestHandler = {
            let requests = Arc::clone(&requests);
            Arc::new(move |request| {
                requests
                    .lock()
                    .expect("request log lock")
                    .push(format!("claude:{}", request.method()));
                let upstream::ClientRequest::TurnStart { params, .. } = request else {
                    return Err(RpcError::Deserialization("expected turn/start".to_string()));
                };
                let collaboration_mode = serde_json::to_value(params.collaboration_mode)
                    .map_err(|error| RpcError::Deserialization(error.to_string()))?;
                assert_eq!(collaboration_mode["mode"], "plan");
                Ok(successful_turn_start_response())
            })
        };
        let session = Arc::new(ServerSession::test_stub_with_runtime_handlers(
            config,
            vec![
                ("codex".to_string(), codex_handler),
                ("claude".to_string(), claude_handler),
            ],
        ));
        client
            .sessions
            .write()
            .expect("sessions lock")
            .insert(server_id.to_string(), session);

        maybe_send_next_local_queued_follow_up(Arc::clone(&client), key).await;

        assert_eq!(
            requests.lock().expect("request log lock").as_slice(),
            ["claude:turn/start"]
        );
    }

    #[tokio::test]
    async fn failed_queued_follow_up_clears_the_draft() {
        let client = Arc::new(MobileClient::new());
        let server_id = "srv";
        let thread_id = "thread-1";
        let key = ThreadKey {
            server_id: server_id.to_string(),
            thread_id: thread_id.to_string(),
        };
        let config = make_server_config(server_id);
        client
            .app_store
            .upsert_server(&config, ServerHealthSnapshot::Connected);
        client
            .app_store
            .upsert_thread_snapshot(make_thread_snapshot(server_id, thread_id));
        enqueue_follow_up(&client, &key, "continue");

        let handler: TestRequestHandler = Arc::new(|request| {
            assert!(matches!(request, upstream::ClientRequest::TurnStart { .. }));
            Err(RpcError::Deserialization(
                "intentional turn/start failure".to_string(),
            ))
        });
        let session = Arc::new(ServerSession::test_stub_with_handlers(
            config,
            Some(handler),
            None,
            None,
        ));
        client
            .sessions
            .write()
            .expect("sessions lock")
            .insert(server_id.to_string(), session);

        maybe_send_next_local_queued_follow_up(Arc::clone(&client), key.clone()).await;

        let snapshot = client.app_store.snapshot();
        let thread = snapshot.threads.get(&key).expect("thread snapshot");
        assert!(thread.queued_follow_up_drafts.is_empty());
        assert!(thread.local_overlay_items.is_empty());
        assert!(
            snapshot
                .servers
                .get(server_id)
                .expect("server snapshot")
                .transport
                .pending_mutation
                .is_none()
        );
    }

    #[tokio::test]
    async fn lag_reconcile_triggers_authoritative_refresh_for_connected_threads() {
        let client = MobileClient::new();
        let server_id = "srv";
        let thread_id = "thread-1";
        let config = make_server_config(server_id);
        client
            .app_store
            .upsert_server(&config, ServerHealthSnapshot::Connected);
        client
            .app_store
            .upsert_thread_snapshot(make_thread_snapshot(server_id, thread_id));

        let requests = Arc::new(StdMutex::new(Vec::<String>::new()));
        let handler: TestRequestHandler = {
            let requests = Arc::clone(&requests);
            Arc::new(move |request| {
                requests
                    .lock()
                    .expect("request log lock")
                    .push(request.method().to_string());
                Err(RpcError::Deserialization(
                    "intentional authoritative refresh failure".to_string(),
                ))
            })
        };
        let session = Arc::new(ServerSession::test_stub_with_handlers(
            config,
            Some(handler),
            None,
            None,
        ));
        client
            .sessions
            .write()
            .expect("sessions lock")
            .insert(server_id.to_string(), session);

        reconcile_after_store_listener_lag(&client).await;

        assert_eq!(
            requests.lock().expect("request log lock").as_slice(),
            ["thread/resume"]
        );
    }
}
