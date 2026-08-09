use super::*;

const SUBAGENT_METADATA_HYDRATE_DELAYS_MS: [u64; 3] = [150, 800, 2500];
const LAG_STALE_RETRY_DELAYS_MS: [u64; 3] = [50, 250, 1000];
const LAG_STALE_RETRY_CYCLE_DELAY_MS: u64 = 5000;

fn next_lag_stale_retry_delay_ms(stale_retry_index: &mut usize) -> u64 {
    if let Some(delay_ms) = LAG_STALE_RETRY_DELAYS_MS.get(*stale_retry_index) {
        *stale_retry_index += 1;
        return *delay_ms;
    }
    *stale_retry_index = 0;
    LAG_STALE_RETRY_CYCLE_DELAY_MS
}

#[derive(Default)]
struct LagReconcileState {
    running: bool,
    pending: bool,
}

#[derive(Default)]
struct LagReconcileGate {
    state: StdMutex<LagReconcileState>,
}

impl LagReconcileGate {
    fn request_pass(&self) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.running {
            state.pending = true;
            return false;
        }
        state.running = true;
        true
    }

    fn finish_pass(&self) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.pending {
            state.pending = false;
            return true;
        }
        state.running = false;
        false
    }

    fn cancel(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *state = LagReconcileState::default();
    }
}

async fn run_lag_reconcile_worker<F, Fut>(
    owner: std::sync::Weak<MobileClient>,
    lag_reconcile_gate: Arc<LagReconcileGate>,
    mut reconcile: F,
) where
    F: FnMut(Arc<MobileClient>) -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let mut stale_retry_index = 0;
    loop {
        let Some(client) = owner.upgrade() else {
            warn!("MobileClient: lag reconcile skipped because the listener owner was dropped");
            lag_reconcile_gate.cancel();
            return;
        };
        if reconcile(client).await {
            let delay_ms = next_lag_stale_retry_delay_ms(&mut stale_retry_index);
            if delay_ms == LAG_STALE_RETRY_CYCLE_DELAY_MS {
                warn!(
                    "MobileClient: lag reconciliation remained stale after bounded retries; scheduling another coalesced pass"
                );
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
            continue;
        }
        if !lag_reconcile_gate.finish_pass() {
            break;
        }
        // A separately observed lag burst warrants its own bounded
        // stale-response backoff sequence.
        stale_retry_index = 0;
    }
}

pub(super) fn spawn_store_listener(
    owner: std::sync::Weak<MobileClient>,
    app_store: Arc<AppStoreReducer>,
    sessions: Arc<RwLock<HashMap<String, Arc<ServerSession>>>>,
    mut rx: broadcast::Receiver<UiEvent>,
) {
    let lag_reconcile_gate = Arc::new(LagReconcileGate::default());
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
                        let owner = owner.clone();
                        let key = key.clone();
                        MobileClient::spawn_detached(async move {
                            let Some(client) = owner.upgrade() else {
                                warn!(
                                    "MobileClient: queued follow-up skipped because the listener owner was dropped"
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
                    if !lag_reconcile_gate.request_pass() {
                        continue;
                    }
                    let owner = owner.clone();
                    let lag_reconcile_gate = Arc::clone(&lag_reconcile_gate);
                    MobileClient::spawn_detached(async move {
                        run_lag_reconcile_worker(
                            owner,
                            lag_reconcile_gate,
                            reconcile_after_store_listener_lag,
                        )
                        .await;
                    });
                }
            }
        }
    });
}

async fn reconcile_after_store_listener_lag(client: Arc<MobileClient>) -> bool {
    let mut needs_retry = false;
    let sessions = client
        .sessions_read()
        .iter()
        .map(|(server_id, session)| (server_id.clone(), Arc::clone(session)))
        .collect::<Vec<_>>();
    let connected_server_ids = sessions
        .iter()
        .map(|(server_id, _)| server_id.clone())
        .collect::<HashSet<_>>();

    for (server_id, session) in sessions {
        let expected_generation = client.app_store.server_event_generation(&server_id);
        match refresh_thread_list_from_app_server_if_ui_generation(
            session,
            Arc::clone(&client.sessions),
            Arc::clone(&client.app_store),
            &server_id,
            expected_generation,
        )
        .await
        {
            Ok(true) => {}
            Ok(false) => {
                debug!(
                    "MobileClient: discarding stale lag thread-list refresh for {}",
                    server_id
                );
                return true;
            }
            Err(error) => {
                warn!(
                    "MobileClient: lag reconcile thread-list refresh failed for {}: {}",
                    server_id, error
                );
                needs_retry = true;
            }
        }
    }

    let keys = client
        .app_store
        .snapshot()
        .threads
        .keys()
        .filter(|key| connected_server_ids.contains(&key.server_id))
        .cloned()
        .collect::<Vec<_>>();

    for key in keys {
        let expected_generation = client.app_store.server_event_generation(&key.server_id);
        match client
            .force_refresh_thread_authoritative_if_ui_generation(
                &key.server_id,
                &key.thread_id,
                expected_generation,
            )
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                debug!(
                    "MobileClient: discarding stale lag refresh for {} thread {}",
                    key.server_id, key.thread_id
                );
                return true;
            }
            Err(error) => {
                warn!(
                    "MobileClient: lag reconcile failed for {} thread {}: {}",
                    key.server_id, key.thread_id, error
                );
                needs_retry = true;
                continue;
            }
        }
        maybe_send_next_local_queued_follow_up(Arc::clone(&client), key).await;
    }
    needs_retry
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
    let Some(draft) = client.app_store.try_claim_first_queued_follow_up(&key) else {
        return;
    };
    let result = client
        .start_turn_with_claim(
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
            Some(draft.preview.id.clone()),
        )
        .await;
    if let Err(error) = result {
        // A transport error is ambiguous: the detached reconciliation task owns
        // claim resolution so a timed-out turn cannot be sent twice.
        if matches!(error, RpcError::Transport(_)) {
            client.schedule_ambiguous_turn_reconciliation(key.clone());
        } else if !super::thread_operations::turn_request_error_is_ambiguous(&error) {
            client
                .app_store
                .release_thread_follow_up_claim(&key, &draft.preview.id);
        }
        warn!(
            "MobileClient: failed to autosend queued follow-up for {} thread {}: {}",
            key.server_id, key.thread_id, error
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation_uniffi::{
        HydratedConversationItem, HydratedConversationItemContent, HydratedUserMessageData,
    };
    use crate::mobile_client::thread_operations::AMBIGUOUS_TURN_REPAIR_PAGE_LIMIT;
    use crate::session::connection::TestRequestHandler;
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn lag_reconcile_gate_coalesces_concurrent_requests_into_one_follow_up() {
        let gate = LagReconcileGate::default();

        assert!(gate.request_pass());
        assert!(!gate.request_pass());
        assert!(!gate.request_pass());
        assert!(gate.finish_pass(), "one follow-up pass should be pending");
        assert!(!gate.finish_pass(), "the gate should return to idle");
        assert!(gate.request_pass(), "idle gate should start a new pass");
        gate.cancel();
        assert!(gate.request_pass(), "cancel should return the gate to idle");
    }

    #[tokio::test(start_paused = true)]
    async fn lag_reconcile_retries_past_one_bounded_cycle_until_clean() {
        let client = MobileClient::new();
        let gate = Arc::new(LagReconcileGate::default());
        assert!(gate.request_pass());
        let attempts = Arc::new(AtomicUsize::new(0));
        let worker = tokio::spawn(run_lag_reconcile_worker(
            Arc::downgrade(&client),
            Arc::clone(&gate),
            {
                let attempts = Arc::clone(&attempts);
                move |_| {
                    let attempts = Arc::clone(&attempts);
                    async move { attempts.fetch_add(1, Ordering::SeqCst) < 6 }
                }
            },
        ));

        while attempts.load(Ordering::SeqCst) < 7 {
            tokio::task::yield_now().await;
            tokio::time::advance(tokio::time::Duration::from_secs(5)).await;
        }
        worker.await.expect("lag reconcile worker should finish");
        assert_eq!(attempts.load(Ordering::SeqCst), 7);
        assert!(
            gate.request_pass(),
            "clean pass should return the gate to idle"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn lag_reconcile_owner_drop_cancels_during_backoff() {
        let client = MobileClient::new();
        let gate = Arc::new(LagReconcileGate::default());
        assert!(gate.request_pass());
        let attempts = Arc::new(AtomicUsize::new(0));
        let worker = tokio::spawn(run_lag_reconcile_worker(
            Arc::downgrade(&client),
            Arc::clone(&gate),
            {
                let attempts = Arc::clone(&attempts);
                move |_| {
                    let attempts = Arc::clone(&attempts);
                    async move {
                        attempts.fetch_add(1, Ordering::SeqCst);
                        true
                    }
                }
            },
        ));
        while attempts.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }

        drop(client);
        tokio::time::advance(tokio::time::Duration::from_millis(50)).await;
        worker.await.expect("owner drop should stop the worker");
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert!(
            gate.request_pass(),
            "cancellation should return the gate to idle"
        );
    }

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

    #[test]
    fn queued_follow_ups_capture_inherit_and_advance_active_turn_anchor() {
        let client = MobileClient::new();
        let key = ThreadKey {
            server_id: "srv".to_string(),
            thread_id: "thread-1".to_string(),
        };
        let mut thread = make_thread_snapshot(&key.server_id, &key.thread_id);
        thread.active_turn_id = Some("turn-anchor".to_string());
        client.app_store.upsert_thread_snapshot(thread);
        enqueue_follow_up(&client, &key, "repeat");

        let mut idle = client
            .app_store
            .thread_snapshot(&key)
            .expect("queued thread");
        idle.active_turn_id = None;
        client.app_store.upsert_thread_snapshot(idle);
        enqueue_follow_up(&client, &key, "repeat");
        enqueue_follow_up(&client, &key, "repeat");

        let thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("queued thread");
        assert_eq!(thread.queued_follow_up_drafts.len(), 3);
        assert!(
            thread
                .queued_follow_up_drafts
                .iter()
                .all(|draft| { draft.causal_anchor_turn_id.as_deref() == Some("turn-anchor") })
        );

        let first_claim = client
            .app_store
            .try_claim_first_queued_follow_up(&key)
            .expect("first queued follow-up claim");
        client.app_store.mark_turn_started_from_response(
            &key,
            "turn-first-follow-up",
            Some(&first_claim.preview.id),
        );

        let thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("re-anchored queued thread");
        assert_eq!(thread.queued_follow_up_drafts.len(), 2);
        assert!(thread.queued_follow_up_drafts.iter().all(|draft| {
            draft.preview.text == "repeat"
                && draft.causal_anchor_turn_id.as_deref() == Some("turn-first-follow-up")
        }));
    }

    async fn client_with_ambiguous_autosend(
        turn_error: fn() -> TransportError,
    ) -> (Arc<MobileClient>, ThreadKey, ServerConfig, Arc<AtomicUsize>) {
        let client = MobileClient::new();
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
        let mut active_thread = make_thread_snapshot(server_id, thread_id);
        active_thread.active_turn_id = Some("turn-anchor".to_string());
        active_thread.info.status = ThreadSummaryStatus::Active;
        client.app_store.upsert_thread_snapshot(active_thread);
        enqueue_follow_up(&client, &key, "retained");
        let mut idle_thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("queued active thread");
        idle_thread.active_turn_id = None;
        idle_thread.info.status = ThreadSummaryStatus::Idle;
        client.app_store.upsert_thread_snapshot(idle_thread);
        let requests = Arc::new(AtomicUsize::new(0));
        let handler: TestRequestHandler = {
            let requests = Arc::clone(&requests);
            Arc::new(move |request| match request {
                upstream::ClientRequest::TurnStart { .. } => {
                    requests.fetch_add(1, Ordering::SeqCst);
                    Err(RpcError::Transport(turn_error()))
                }
                upstream::ClientRequest::ThreadResume { .. } => {
                    Err(RpcError::Transport(TransportError::Disconnected))
                }
                other => Err(RpcError::Deserialization(format!(
                    "unexpected ambiguous autosend request: {}",
                    other.method()
                ))),
            })
        };
        let session = Arc::new(ServerSession::test_stub_with_handlers(
            config.clone(),
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
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert!(client.pending_turn_reconciliation().contains_key(&key));
        assert_eq!(
            client
                .pending_turn_reconciliation()
                .get(&key)
                .and_then(|pending| pending.causal_anchor_turn_id.as_deref()),
            Some("turn-anchor"),
        );
        let thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("ambiguous thread snapshot");
        assert!(thread.queued_follow_up_drafts[0].autosend_claimed);
        (client, key, config, requests)
    }

    async fn reconcile_completed_ambiguous_claim(
        initial_thread: ThreadSnapshot,
        claimed_text: &str,
        response_turn_id: &str,
        response_text: &str,
        next_cursor: Option<&str>,
        causal_anchor_turn_id: Option<&str>,
    ) -> (Arc<MobileClient>, ThreadKey) {
        let client = MobileClient::new();
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
            .set_server_supports_turn_pagination(server_id, true);
        client.app_store.upsert_thread_snapshot(initial_thread);
        enqueue_follow_up(&client, &key, claimed_text);
        assert!(
            client
                .app_store
                .try_claim_first_queued_follow_up(&key)
                .is_some()
        );
        assert!(client.mark_turn_start_ambiguous_after_turn(key.clone(), causal_anchor_turn_id,));

        let response_turn_id = response_turn_id.to_string();
        let response_text = response_text.to_string();
        let next_cursor = next_cursor.map(str::to_string);
        let handler: TestRequestHandler = Arc::new(move |request| match request {
            upstream::ClientRequest::ThreadResume { .. } => {
                Ok(successful_thread_resume_response(thread_id))
            }
            upstream::ClientRequest::ThreadTurnsList { params, .. } => {
                let skeleton =
                    matches!(params.items_view, Some(upstream::TurnItemsView::NotLoaded));
                let response = completed_thread_turns_list_response(
                    &response_turn_id,
                    "item-authoritative",
                    &response_text,
                    skeleton,
                    next_cursor.as_deref(),
                );
                Ok(response)
            }
            other => Err(RpcError::Deserialization(format!(
                "unexpected completed ambiguity request: {}",
                other.method()
            ))),
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

        client
            .force_refresh_thread_authoritative(server_id, thread_id)
            .await
            .expect("completed ambiguity refresh succeeds");
        (client, key)
    }

    async fn paginated_ambiguous_client_with_pages(
        initial_thread: ThreadSnapshot,
        claimed_text: &str,
        causal_anchor_turn_id: Option<&str>,
        page_for_cursor: Arc<dyn Fn(Option<&str>) -> serde_json::Value + Send + Sync>,
    ) -> (
        Arc<MobileClient>,
        ThreadKey,
        Arc<StdMutex<Vec<Option<String>>>>,
    ) {
        let client = MobileClient::new();
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
            .set_server_supports_turn_pagination(server_id, true);
        client.app_store.upsert_thread_snapshot(initial_thread);
        enqueue_follow_up(&client, &key, claimed_text);
        assert!(
            client
                .app_store
                .try_claim_first_queued_follow_up(&key)
                .is_some()
        );
        assert!(client.mark_turn_start_ambiguous_after_turn(key.clone(), causal_anchor_turn_id,));

        let full_page_cursors = Arc::new(StdMutex::new(Vec::new()));
        let handler: TestRequestHandler = {
            let full_page_cursors = Arc::clone(&full_page_cursors);
            Arc::new(move |request| match request {
                upstream::ClientRequest::ThreadResume { params, .. } => {
                    assert!(params.exclude_turns);
                    Ok(successful_thread_resume_response(thread_id))
                }
                upstream::ClientRequest::ThreadTurnsList { params, .. } => {
                    if matches!(params.items_view, Some(upstream::TurnItemsView::NotLoaded)) {
                        return Ok(completed_thread_turns_list_response(
                            "turn-probe",
                            "item-probe",
                            "",
                            true,
                            None,
                        ));
                    }
                    full_page_cursors
                        .lock()
                        .expect("full-page cursor log lock")
                        .push(params.cursor.clone());
                    Ok(page_for_cursor(params.cursor.as_deref()))
                }
                other => Err(RpcError::Deserialization(format!(
                    "unexpected paginated ambiguity request: {}",
                    other.method()
                ))),
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

        (client, key, full_page_cursors)
    }

    async fn reconcile_embedded_ambiguous_claim(
        initial_thread: ThreadSnapshot,
        supports_pagination: bool,
        claimed_text: &str,
        response_turn_id: &str,
        response_text: &str,
        refresh_count: usize,
        causal_anchor_turn_id: Option<&str>,
    ) -> (Arc<MobileClient>, ThreadKey, Arc<StdMutex<Vec<String>>>) {
        let client = MobileClient::new();
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
            .set_server_supports_turn_pagination(server_id, supports_pagination);
        client.app_store.upsert_thread_snapshot(initial_thread);
        enqueue_follow_up(&client, &key, claimed_text);
        assert!(
            client
                .app_store
                .try_claim_first_queued_follow_up(&key)
                .is_some()
        );
        assert!(client.mark_turn_start_ambiguous_after_turn(key.clone(), causal_anchor_turn_id,));

        let response_turn_id = response_turn_id.to_string();
        let response_text = response_text.to_string();
        let requests = Arc::new(StdMutex::new(Vec::<String>::new()));
        let handler: TestRequestHandler = {
            let requests = Arc::clone(&requests);
            Arc::new(move |request| match request {
                upstream::ClientRequest::ThreadResume { params, .. } => {
                    requests
                        .lock()
                        .expect("request log lock")
                        .push(format!("thread/resume:{}", params.exclude_turns));
                    let response = completed_thread_resume_response(
                        thread_id,
                        &response_turn_id,
                        "item-authoritative",
                        &response_text,
                    );
                    Ok(response)
                }
                upstream::ClientRequest::ThreadTurnsList { .. } => {
                    requests
                        .lock()
                        .expect("request log lock")
                        .push("thread/turns/list".to_string());
                    Ok(successful_thread_turns_list_response())
                }
                other => Err(RpcError::Deserialization(format!(
                    "unexpected embedded ambiguity request: {}",
                    other.method()
                ))),
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

        for _ in 0..refresh_count {
            client
                .force_refresh_thread_authoritative(server_id, thread_id)
                .await
                .expect("embedded ambiguity refresh succeeds");
        }
        (client, key, requests)
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

    fn turn_start_params(thread_id: &str, text: &str) -> upstream::TurnStartParams {
        upstream::TurnStartParams {
            thread_id: thread_id.to_string(),
            input: vec![upstream::UserInput::Text {
                text: text.to_string(),
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
            collaboration_mode: None,
        }
    }

    fn upstream_thread_response(thread_id: &str) -> serde_json::Value {
        serde_json::json!({
            "id": thread_id,
            "sessionId": thread_id,
            "preview": "Thread",
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
            "name": "Thread",
            "turns": []
        })
    }

    fn successful_thread_list_response(thread_ids: &[&str]) -> serde_json::Value {
        serde_json::json!({
            "data": thread_ids
                .iter()
                .map(|thread_id| upstream_thread_response(thread_id))
                .collect::<Vec<_>>(),
            "nextCursor": null,
            "backwardsCursor": null
        })
    }

    fn successful_thread_resume_response(thread_id: &str) -> serde_json::Value {
        serde_json::json!({
            "thread": upstream_thread_response(thread_id),
            "model": "gpt-5",
            "modelProvider": "openai",
            "cwd": "/tmp/thread",
            "approvalPolicy": "never",
            "approvalsReviewer": "user",
            "sandbox": { "type": "dangerFullAccess" },
            "reasoningEffort": "medium"
        })
    }

    fn completed_thread_resume_response(
        thread_id: &str,
        turn_id: &str,
        item_id: &str,
        text: &str,
    ) -> serde_json::Value {
        let mut response = successful_thread_resume_response(thread_id);
        response["thread"]["turns"] =
            completed_thread_turns_list_response(turn_id, item_id, text, false, None)["data"]
                .clone();
        response
    }

    fn successful_thread_read_response(thread_id: &str) -> serde_json::Value {
        serde_json::json!({
            "thread": upstream_thread_response(thread_id),
            "approvalPolicy": "never",
            "sandbox": { "type": "dangerFullAccess" }
        })
    }

    fn completed_thread_turns_list_response(
        turn_id: &str,
        item_id: &str,
        text: &str,
        skeleton: bool,
        next_cursor: Option<&str>,
    ) -> serde_json::Value {
        serde_json::json!({
            "data": [{
                "id": turn_id,
                "items": if skeleton {
                    serde_json::json!([])
                } else {
                    serde_json::json!([{
                        "id": item_id,
                        "type": "userMessage",
                        "content": [{
                            "type": "text",
                            "text": text,
                            "textElements": []
                        }]
                    }])
                },
                "itemsView": if skeleton { "notLoaded" } else { "full" },
                "status": "completed",
                "error": null,
                "startedAt": 1,
                "completedAt": 2,
                "durationMs": 1
            }],
            "nextCursor": next_cursor,
            "backwardsCursor": null
        })
    }

    fn completed_undated_thread_turns_list_response(
        turn_id: &str,
        item_id: &str,
        text: &str,
        next_cursor: Option<&str>,
    ) -> serde_json::Value {
        let mut response =
            completed_thread_turns_list_response(turn_id, item_id, text, false, next_cursor);
        response["data"][0]
            .as_object_mut()
            .expect("turn response object")
            .remove("startedAt");
        response
    }

    fn hydrated_user_item(turn_id: &str, item_id: &str, text: &str) -> HydratedConversationItem {
        HydratedConversationItem {
            id: item_id.to_string(),
            content: HydratedConversationItemContent::User(HydratedUserMessageData {
                text: text.to_string(),
                image_data_uris: Vec::new(),
            }),
            source_turn_id: Some(turn_id.to_string()),
            source_turn_index: Some(0),
            timestamp: None,
            is_from_user_turn_boundary: true,
        }
    }

    fn successful_active_thread_resume_response(thread_id: &str) -> serde_json::Value {
        let mut response = successful_thread_resume_response(thread_id);
        response["thread"]["status"] = serde_json::json!({ "type": "active", "activeFlags": [] });
        response["thread"]["turns"] = serde_json::json!([{
            "id": "turn-authoritative",
            "items": [],
            "itemsView": "full",
            "status": "inProgress",
            "error": null,
            "startedAt": 1,
            "completedAt": null,
            "durationMs": null
        }]);
        response
    }

    fn successful_thread_turns_list_response() -> serde_json::Value {
        serde_json::json!({
            "data": [],
            "nextCursor": null,
            "backwardsCursor": null
        })
    }

    fn successful_active_thread_turns_list_response() -> serde_json::Value {
        serde_json::json!({
            "data": [{
                "id": "turn-authoritative",
                "items": [],
                "itemsView": "notLoaded",
                "status": "inProgress",
                "error": null,
                "startedAt": 1,
                "completedAt": null,
                "durationMs": null
            }],
            "nextCursor": null,
            "backwardsCursor": null
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
        let client = MobileClient::new();
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
    async fn store_listener_routes_follow_up_through_its_owning_non_singleton_client() {
        let client = MobileClient::new();
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

        let (sent_tx, sent_rx) = tokio::sync::oneshot::channel();
        let sent_tx = Arc::new(StdMutex::new(Some(sent_tx)));
        let handler: TestRequestHandler = {
            let sent_tx = Arc::clone(&sent_tx);
            Arc::new(move |request| {
                assert!(matches!(request, upstream::ClientRequest::TurnStart { .. }));
                if let Some(sender) = sent_tx.lock().expect("send signal lock").take() {
                    let _ = sender.send(());
                }
                Ok(successful_turn_start_response())
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

        let (event_tx, event_rx) = broadcast::channel(4);
        spawn_store_listener(
            Arc::downgrade(&client),
            Arc::clone(&client.app_store),
            Arc::clone(&client.sessions),
            event_rx,
        );
        event_tx
            .send(UiEvent::TurnCompleted {
                key,
                turn_id: "turn-1".to_string(),
                error: None,
            })
            .expect("listener remains subscribed");

        tokio::time::timeout(tokio::time::Duration::from_secs(2), sent_rx)
            .await
            .expect("owning client should dispatch queued follow-up")
            .expect("send signal should remain open");
    }

    #[tokio::test]
    async fn concurrent_queued_follow_up_autosend_claims_the_draft_once() {
        let client = MobileClient::new();
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

        let requests = Arc::new(StdMutex::new(0usize));
        let handler: TestRequestHandler = {
            let requests = Arc::clone(&requests);
            Arc::new(move |request| {
                assert!(matches!(request, upstream::ClientRequest::TurnStart { .. }));
                *requests.lock().expect("request count lock") += 1;
                Ok(successful_turn_start_response())
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

        let first = maybe_send_next_local_queued_follow_up(Arc::clone(&client), key.clone());
        let second = maybe_send_next_local_queued_follow_up(Arc::clone(&client), key.clone());
        tokio::join!(first, second);

        assert_eq!(*requests.lock().expect("request count lock"), 1);
        let snapshot = client.app_store.snapshot();
        let thread = snapshot.threads.get(&key).expect("thread snapshot");
        assert!(thread.queued_follow_up_drafts.is_empty());
        assert_eq!(thread.active_turn_id.as_deref(), Some("turn-follow-up"));
    }

    #[tokio::test]
    async fn ambiguous_disconnect_retains_claim_without_matching_active_turn_evidence() {
        let (client, key, config, turn_start_requests) =
            client_with_ambiguous_autosend(|| TransportError::Disconnected).await;
        maybe_send_next_local_queued_follow_up(Arc::clone(&client), key.clone()).await;
        assert_eq!(turn_start_requests.load(Ordering::SeqCst), 1);

        let handler: TestRequestHandler = Arc::new(move |request| match request {
            upstream::ClientRequest::ThreadResume { .. } => {
                Ok(successful_active_thread_resume_response("thread-1"))
            }
            other => Err(RpcError::Deserialization(format!(
                "unexpected request in active hydration test: {}",
                other.method()
            ))),
        });
        let replacement = Arc::new(ServerSession::test_stub_with_handlers(
            config,
            Some(handler),
            None,
            None,
        ));
        client
            .sessions
            .write()
            .expect("sessions lock")
            .insert(key.server_id.clone(), replacement);

        client
            .external_resume_thread(&key.server_id, &key.thread_id, None)
            .await
            .expect("active authoritative hydration succeeds");

        assert!(client.pending_turn_reconciliation().contains_key(&key));
        let thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("hydrated active thread");
        assert_eq!(thread.active_turn_id.as_deref(), Some("turn-authoritative"));
        assert_eq!(thread.queued_follow_up_drafts.len(), 1);
        assert!(thread.queued_follow_up_drafts[0].autosend_claimed);
        assert_eq!(turn_start_requests.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn ambiguous_disconnect_hydrates_idle_then_releases_claim_once() {
        let (client, key, config, initial_turn_start_requests) =
            client_with_ambiguous_autosend(|| TransportError::Disconnected).await;
        let retry_turn_start_requests = Arc::new(AtomicUsize::new(0));
        let handler: TestRequestHandler = {
            let retry_turn_start_requests = Arc::clone(&retry_turn_start_requests);
            Arc::new(move |request| match request {
                upstream::ClientRequest::ThreadResume { .. } => {
                    Ok(successful_thread_resume_response("thread-1"))
                }
                upstream::ClientRequest::ThreadTurnsList { .. } => {
                    Ok(successful_thread_turns_list_response())
                }
                upstream::ClientRequest::TurnStart { .. } => {
                    retry_turn_start_requests.fetch_add(1, Ordering::SeqCst);
                    Ok(successful_turn_start_response())
                }
                other => Err(RpcError::Deserialization(format!(
                    "unexpected request in idle hydration test: {}",
                    other.method()
                ))),
            })
        };
        let replacement = Arc::new(ServerSession::test_stub_with_handlers(
            config,
            Some(handler),
            None,
            None,
        ));
        client
            .sessions
            .write()
            .expect("sessions lock")
            .insert(key.server_id.clone(), replacement);

        client
            .external_resume_thread(&key.server_id, &key.thread_id, None)
            .await
            .expect("idle authoritative hydration succeeds");

        assert!(!client.pending_turn_reconciliation().contains_key(&key));
        let thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("hydrated idle thread");
        assert_eq!(thread.active_turn_id, None);
        assert_eq!(thread.queued_follow_up_drafts.len(), 1);
        assert!(!thread.queued_follow_up_drafts[0].autosend_claimed);
        assert_eq!(initial_turn_start_requests.load(Ordering::SeqCst), 1);

        maybe_send_next_local_queued_follow_up(Arc::clone(&client), key.clone()).await;
        assert_eq!(retry_turn_start_requests.load(Ordering::SeqCst), 1);
        let thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("retried thread");
        assert_eq!(thread.active_turn_id.as_deref(), Some("turn-follow-up"));
        assert!(thread.queued_follow_up_drafts.is_empty());
    }

    #[tokio::test]
    async fn paginated_active_probe_without_items_retains_ambiguous_claim() {
        let client = MobileClient::new();
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
            .set_server_supports_turn_pagination(server_id, true);
        let mut initial_thread = make_thread_snapshot(server_id, thread_id);
        initial_thread.initial_turns_loaded = true;
        client.app_store.upsert_thread_snapshot(initial_thread);
        enqueue_follow_up(&client, &key, "retained");
        assert!(
            client
                .app_store
                .try_claim_first_queued_follow_up(&key)
                .is_some()
        );
        assert!(client.mark_turn_start_ambiguous(key.clone()));

        let requests = Arc::new(StdMutex::new(Vec::<String>::new()));
        let handler: TestRequestHandler = {
            let requests = Arc::clone(&requests);
            Arc::new(move |request| {
                requests
                    .lock()
                    .expect("request log lock")
                    .push(request.method().to_string());
                match request {
                    upstream::ClientRequest::ThreadResume { params, .. } => {
                        assert!(params.exclude_turns);
                        Ok(successful_thread_resume_response(thread_id))
                    }
                    upstream::ClientRequest::ThreadTurnsList { .. } => {
                        Ok(successful_active_thread_turns_list_response())
                    }
                    other => Err(RpcError::Deserialization(format!(
                        "unexpected paginated ambiguity request: {}",
                        other.method()
                    ))),
                }
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

        client
            .force_refresh_thread_authoritative(server_id, thread_id)
            .await
            .expect("paginated authoritative refresh succeeds");

        assert_eq!(
            requests.lock().expect("request log lock").as_slice(),
            ["thread/resume", "thread/turns/list"]
        );
        assert!(client.pending_turn_reconciliation().contains_key(&key));
        let thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("authoritative active thread");
        assert_eq!(thread.active_turn_id.as_deref(), Some("turn-authoritative"));
        assert_eq!(thread.queued_follow_up_drafts.len(), 1);
        assert!(thread.queued_follow_up_drafts[0].autosend_claimed);
        assert!(
            client
                .app_store
                .try_claim_first_queued_follow_up(&key)
                .is_none()
        );
    }

    #[tokio::test]
    async fn paginated_ambiguous_claim_uses_full_repair_page_after_fast_completion() {
        let client = MobileClient::new();
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
            .set_server_supports_turn_pagination(server_id, true);
        let mut initial_thread = make_thread_snapshot(server_id, thread_id);
        initial_thread.initial_turns_loaded = true;
        client.app_store.upsert_thread_snapshot(initial_thread);
        enqueue_follow_up(&client, &key, "retained");
        assert!(
            client
                .app_store
                .try_claim_first_queued_follow_up(&key)
                .is_some()
        );
        assert!(client.mark_turn_start_ambiguous(key.clone()));

        let requests = Arc::new(StdMutex::new(Vec::<String>::new()));
        let handler: TestRequestHandler = {
            let requests = Arc::clone(&requests);
            Arc::new(move |request| match request {
                upstream::ClientRequest::ThreadResume { params, .. } => {
                    assert!(params.exclude_turns);
                    requests
                        .lock()
                        .expect("request log lock")
                        .push("thread/resume".to_string());
                    Ok(successful_thread_resume_response(thread_id))
                }
                upstream::ClientRequest::ThreadTurnsList { params, .. } => {
                    let skeleton =
                        matches!(params.items_view, Some(upstream::TurnItemsView::NotLoaded));
                    requests.lock().expect("request log lock").push(
                        if skeleton {
                            "thread/turns/list:skeleton"
                        } else {
                            "thread/turns/list:full"
                        }
                        .to_string(),
                    );
                    Ok(serde_json::json!({
                        "data": [{
                            "id": "turn-authoritative",
                            "items": if skeleton {
                                serde_json::json!([])
                            } else {
                                serde_json::json!([{
                                    "id": "item-user",
                                    "type": "userMessage",
                                    "content": [{
                                        "type": "text",
                                        "text": "retained",
                                        "textElements": []
                                    }]
                                }])
                            },
                            "itemsView": if skeleton { "notLoaded" } else { "full" },
                            "status": "completed",
                            "error": null,
                            "startedAt": 1,
                            "completedAt": 2,
                            "durationMs": 1
                        }],
                        "nextCursor": null,
                        "backwardsCursor": null
                    }))
                }
                other => Err(RpcError::Deserialization(format!(
                    "unexpected paginated completion request: {}",
                    other.method()
                ))),
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

        client
            .force_refresh_thread_authoritative(server_id, thread_id)
            .await
            .expect("paginated authoritative repair succeeds");

        assert_eq!(
            requests.lock().expect("request log lock").as_slice(),
            [
                "thread/resume",
                "thread/turns/list:skeleton",
                "thread/turns/list:full"
            ]
        );
        assert!(!client.pending_turn_reconciliation().contains_key(&key));
        let thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("authoritatively repaired thread");
        assert_eq!(thread.active_turn_id, None);
        assert!(thread.queued_follow_up_drafts.is_empty());
        assert_eq!(thread.items.len(), 1);
        assert!(
            client
                .app_store
                .try_claim_first_queued_follow_up(&key)
                .is_none()
        );
    }

    #[tokio::test]
    async fn repeated_prompt_in_known_turn_does_not_consume_new_ambiguous_claim() {
        let mut initial_thread = make_thread_snapshot("srv", "thread-1");
        initial_thread.initial_turns_loaded = true;
        initial_thread.items.push(hydrated_user_item(
            "turn-old",
            "item-authoritative",
            "continue",
        ));

        let (client, key) = reconcile_completed_ambiguous_claim(
            initial_thread,
            "continue",
            "turn-old",
            "continue",
            None,
            None,
        )
        .await;

        assert!(!client.pending_turn_reconciliation().contains_key(&key));
        let thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("reconciled repeated-prompt thread");
        assert_eq!(thread.queued_follow_up_drafts.len(), 1);
        assert!(!thread.queued_follow_up_drafts[0].autosend_claimed);
    }

    #[tokio::test]
    async fn nonmatching_prompt_without_causal_anchor_releases_ambiguous_claim() {
        let initial_thread = make_thread_snapshot("srv", "thread-1");

        let (client, key) = reconcile_completed_ambiguous_claim(
            initial_thread,
            "continue",
            "turn-unknown",
            "different",
            None,
            None,
        )
        .await;

        assert!(!client.pending_turn_reconciliation().contains_key(&key));
        let thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("unknown-history thread");
        assert_eq!(thread.queued_follow_up_drafts.len(), 1);
        assert!(!thread.queued_follow_up_drafts[0].autosend_claimed);
        assert!(thread.initial_turns_loaded);
        assert_eq!(thread.items.len(), 1);
        assert!(
            client
                .app_store
                .try_claim_first_queued_follow_up(&key)
                .is_some()
        );
    }

    #[tokio::test]
    async fn matching_prompt_without_causal_anchor_retains_ambiguous_claim() {
        let initial_thread = make_thread_snapshot("srv", "thread-1");

        let (client, key) = reconcile_completed_ambiguous_claim(
            initial_thread,
            "continue",
            "turn-unknown",
            "continue",
            None,
            None,
        )
        .await;

        for refresh in 0..2 {
            assert!(client.pending_turn_reconciliation().contains_key(&key));
            assert!(
                client
                    .pending_turn_reconciliation()
                    .get(&key)
                    .is_some_and(|pending| pending.unanchored_replay_turn_id.is_some())
            );
            let thread = client
                .app_store
                .thread_snapshot(&key)
                .expect("unanchored unknown-history thread");
            assert_eq!(thread.queued_follow_up_drafts.len(), 1);
            assert!(thread.queued_follow_up_drafts[0].autosend_claimed);
            assert!(thread.items.is_empty());
            if refresh == 0 {
                client
                    .force_refresh_thread_authoritative("srv", "thread-1")
                    .await
                    .expect("second unanchored ambiguity refresh succeeds");
            }
        }
    }

    #[tokio::test]
    async fn matching_prompt_after_known_empty_baseline_consumes_ambiguous_claim() {
        let mut initial_thread = make_thread_snapshot("srv", "thread-1");
        initial_thread.initial_turns_loaded = true;

        let (client, key) = reconcile_completed_ambiguous_claim(
            initial_thread,
            "continue",
            "turn-unknown",
            "continue",
            None,
            None,
        )
        .await;

        assert!(!client.pending_turn_reconciliation().contains_key(&key));
        let thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("known-empty-baseline thread");
        assert!(thread.queued_follow_up_drafts.is_empty());
        assert!(thread.initial_turns_loaded);
        assert_eq!(thread.items.len(), 1);
        assert!(
            client
                .app_store
                .try_claim_first_queued_follow_up(&key)
                .is_none()
        );
    }

    #[tokio::test]
    async fn repair_page_before_known_history_boundary_retains_ambiguous_claim() {
        let mut initial_thread = make_thread_snapshot("srv", "thread-1");
        initial_thread.initial_turns_loaded = true;
        initial_thread
            .items
            .push(hydrated_user_item("turn-anchor", "item-anchor", "earlier"));

        let (client, key) = reconcile_completed_ambiguous_claim(
            initial_thread,
            "continue",
            "turn-newer",
            "different",
            Some("older-page"),
            None,
        )
        .await;

        assert!(client.pending_turn_reconciliation().contains_key(&key));
        let thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("unanchored repair thread");
        assert_eq!(thread.queued_follow_up_drafts.len(), 1);
        assert!(thread.queued_follow_up_drafts[0].autosend_claimed);
        assert_eq!(thread.items.len(), 1);
        assert_eq!(
            thread.items[0].source_turn_id.as_deref(),
            Some("turn-anchor")
        );

        client
            .force_refresh_thread_authoritative("srv", "thread-1")
            .await
            .expect("second unanchored repair refresh succeeds");

        assert!(client.pending_turn_reconciliation().contains_key(&key));
        let thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("unanchored repair thread after retry");
        assert_eq!(thread.queued_follow_up_drafts.len(), 1);
        assert!(thread.queued_follow_up_drafts[0].autosend_claimed);
        assert_eq!(thread.items.len(), 1);
        assert_eq!(
            thread.items[0].source_turn_id.as_deref(),
            Some("turn-anchor")
        );
    }

    #[tokio::test]
    async fn paginated_ambiguity_walks_to_older_anchor_before_releasing_claim() {
        let mut initial_thread = make_thread_snapshot("srv", "thread-1");
        initial_thread.initial_turns_loaded = true;
        initial_thread
            .items
            .push(hydrated_user_item("turn-anchor", "item-anchor", "earlier"));
        let pages: Arc<dyn Fn(Option<&str>) -> serde_json::Value + Send + Sync> =
            Arc::new(|cursor| match cursor {
                None => completed_thread_turns_list_response(
                    "turn-newer",
                    "item-newer",
                    "different",
                    false,
                    Some("page-2"),
                ),
                Some("page-2") => completed_thread_turns_list_response(
                    "turn-anchor",
                    "item-anchor",
                    "earlier",
                    false,
                    Some("page-3"),
                ),
                other => panic!("unexpected repair cursor: {other:?}"),
            });
        let (client, key, cursors) = paginated_ambiguous_client_with_pages(
            initial_thread,
            "continue",
            Some("turn-anchor"),
            pages,
        )
        .await;

        client
            .force_refresh_thread_authoritative("srv", "thread-1")
            .await
            .expect("anchor walk succeeds");

        assert_eq!(
            cursors.lock().expect("cursor log lock").as_slice(),
            [None, Some("page-2".to_string())]
        );
        assert!(!client.pending_turn_reconciliation().contains_key(&key));
        let thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("anchor-walk thread");
        assert_eq!(thread.queued_follow_up_drafts.len(), 1);
        assert!(!thread.queued_follow_up_drafts[0].autosend_claimed);
        assert_eq!(thread.older_turns_cursor.as_deref(), Some("page-3"));
    }

    #[tokio::test]
    async fn paginated_ambiguity_consumes_replay_beyond_first_page() {
        let initial_thread = make_thread_snapshot("srv", "thread-1");
        let pages: Arc<dyn Fn(Option<&str>) -> serde_json::Value + Send + Sync> =
            Arc::new(|cursor| match cursor {
                None => completed_thread_turns_list_response(
                    "turn-newer",
                    "item-newer",
                    "different",
                    false,
                    Some("page-2"),
                ),
                Some("page-2") => completed_thread_turns_list_response(
                    "turn-replay",
                    "item-replay",
                    "continue",
                    false,
                    Some("page-3"),
                ),
                Some("page-3") => completed_thread_turns_list_response(
                    "turn-anchor",
                    "item-anchor",
                    "earlier",
                    false,
                    None,
                ),
                other => panic!("unexpected replay cursor: {other:?}"),
            });
        let (client, key, cursors) = paginated_ambiguous_client_with_pages(
            initial_thread,
            "continue",
            Some("turn-anchor"),
            pages,
        )
        .await;
        enqueue_follow_up(&client, &key, "continue");
        client
            .app_store
            .mark_turn_started_from_response(&key, "turn-concurrent", None);
        enqueue_follow_up(&client, &key, "continue");

        client
            .force_refresh_thread_authoritative("srv", "thread-1")
            .await
            .expect("replay walk succeeds");

        assert_eq!(
            cursors.lock().expect("cursor log lock").as_slice(),
            [None, Some("page-2".to_string()), Some("page-3".to_string())]
        );
        assert!(!client.pending_turn_reconciliation().contains_key(&key));
        let thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("replay-walk thread");
        assert_eq!(thread.queued_follow_up_drafts.len(), 2);
        assert!(!thread.queued_follow_up_drafts[0].autosend_claimed);
        assert_eq!(
            thread.queued_follow_up_drafts[0]
                .causal_anchor_turn_id
                .as_deref(),
            Some("turn-replay")
        );
        assert!(!thread.queued_follow_up_drafts[1].autosend_claimed);
        assert_eq!(
            thread.queued_follow_up_drafts[1]
                .causal_anchor_turn_id
                .as_deref(),
            Some("turn-concurrent")
        );
        let next_claim = client
            .app_store
            .try_claim_first_queued_follow_up(&key)
            .expect("next repeated follow-up claim");
        assert_eq!(
            next_claim.causal_anchor_turn_id.as_deref(),
            Some("turn-replay")
        );
    }

    #[tokio::test]
    async fn future_dated_repeat_behind_anchor_does_not_consume_unknown_history_claim() {
        let initial_thread = make_thread_snapshot("srv", "thread-1");
        let pages: Arc<dyn Fn(Option<&str>) -> serde_json::Value + Send + Sync> =
            Arc::new(|cursor| {
                assert!(cursor.is_none());
                let anchor = completed_thread_turns_list_response(
                    "turn-anchor",
                    "item-anchor",
                    "earlier",
                    false,
                    None,
                )["data"][0]
                    .clone();
                let mut repeated = completed_thread_turns_list_response(
                    "turn-old-repeat",
                    "item-old-repeat",
                    "continue",
                    false,
                    None,
                )["data"][0]
                    .clone();
                repeated["startedAt"] = i64::MAX.into();
                serde_json::json!({
                    "data": [anchor, repeated],
                    "nextCursor": null,
                    "backwardsCursor": null
                })
            });
        let (client, key, _) = paginated_ambiguous_client_with_pages(
            initial_thread,
            "continue",
            Some("turn-anchor"),
            pages,
        )
        .await;

        client
            .force_refresh_thread_authoritative("srv", "thread-1")
            .await
            .expect("future-dated repeated prompt reconciliation succeeds");

        assert!(!client.pending_turn_reconciliation().contains_key(&key));
        let thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("future-dated repeated-prompt thread");
        assert_eq!(thread.queued_follow_up_drafts.len(), 1);
        assert!(!thread.queued_follow_up_drafts[0].autosend_claimed);
    }

    #[tokio::test]
    async fn paginated_ambiguity_rechecks_head_after_page_cap_and_consumes_late_replay() {
        let mut initial_thread = make_thread_snapshot("srv", "thread-1");
        initial_thread.initial_turns_loaded = true;
        initial_thread
            .items
            .push(hydrated_user_item("turn-anchor", "item-anchor", "earlier"));
        let head_requests = Arc::new(AtomicUsize::new(0));
        let head_requests_for_pages = Arc::clone(&head_requests);
        let pages: Arc<dyn Fn(Option<&str>) -> serde_json::Value + Send + Sync> =
            Arc::new(move |cursor| {
                if cursor.is_none() && head_requests_for_pages.fetch_add(1, Ordering::SeqCst) > 0 {
                    return completed_thread_turns_list_response(
                        "turn-late",
                        "item-late",
                        "continue",
                        false,
                        Some("page-1"),
                    );
                }
                let page_index = cursor
                    .and_then(|cursor| cursor.strip_prefix("page-"))
                    .map_or(0, |index| index.parse::<usize>().expect("numeric cursor"));
                if page_index == AMBIGUOUS_TURN_REPAIR_PAGE_LIMIT {
                    return completed_thread_turns_list_response(
                        "turn-anchor",
                        "item-anchor",
                        "earlier",
                        false,
                        None,
                    );
                }
                let turn_id = format!("turn-newer-{page_index}");
                let item_id = format!("item-newer-{page_index}");
                let next_cursor = format!("page-{}", page_index + 1);
                completed_thread_turns_list_response(
                    &turn_id,
                    &item_id,
                    "different",
                    false,
                    Some(&next_cursor),
                )
            });
        let (client, key, cursors) = paginated_ambiguous_client_with_pages(
            initial_thread,
            "continue",
            Some("turn-anchor"),
            pages,
        )
        .await;

        client
            .force_refresh_thread_authoritative("srv", "thread-1")
            .await
            .expect("capped repair walk succeeds");

        assert!(client.pending_turn_reconciliation().contains_key(&key));
        assert_eq!(
            client
                .pending_turn_reconciliation()
                .get(&key)
                .and_then(|pending| pending.repair_cursor.as_deref())
                .expect("continuation cursor"),
            format!("page-{AMBIGUOUS_TURN_REPAIR_PAGE_LIMIT}")
        );
        let thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("capped repair thread");
        assert_eq!(thread.items.len(), 1);
        assert_eq!(
            thread.items[0].source_turn_id.as_deref(),
            Some("turn-anchor")
        );

        client
            .force_refresh_thread_authoritative("srv", "thread-1")
            .await
            .expect("continued repair walk succeeds");

        assert!(!client.pending_turn_reconciliation().contains_key(&key));
        assert_eq!(
            cursors.lock().expect("cursor log lock").as_slice(),
            [
                None,
                Some("page-1".to_string()),
                Some("page-2".to_string()),
                Some("page-3".to_string()),
                Some("page-4".to_string()),
                Some("page-5".to_string()),
                Some("page-6".to_string()),
                Some("page-7".to_string()),
                Some("page-8".to_string()),
                Some("page-9".to_string()),
                None,
                Some(format!("page-{AMBIGUOUS_TURN_REPAIR_PAGE_LIMIT}")),
            ]
        );
        let thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("continued repair thread");
        assert!(thread.queued_follow_up_drafts.is_empty());
        assert_eq!(thread.items.len(), 2);
        assert_eq!(thread.older_turns_cursor.as_deref(), Some("page-1"));
        assert!(
            thread
                .items
                .iter()
                .all(|item| !item.id.starts_with("item-newer-"))
        );

        let outcome = client
            .load_thread_turns_page("srv", "thread-1", Some("page-1".to_string()), Some(5))
            .await
            .expect("load preserved gap page succeeds");
        assert!(outcome.loaded);
        assert!(outcome.has_more);
        assert_eq!(
            cursors.lock().expect("cursor log lock").last(),
            Some(&Some("page-1".to_string()))
        );
        let thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("gap page thread");
        assert!(thread.items.iter().any(|item| item.id == "item-newer-1"));
        assert_eq!(thread.older_turns_cursor.as_deref(), Some("page-2"));
    }

    async fn assert_paginated_unanchored_replay_observation_survives_page_cap() {
        let initial_thread = make_thread_snapshot("srv", "thread-1");
        let head_requests = Arc::new(AtomicUsize::new(0));
        let head_requests_for_pages = Arc::clone(&head_requests);
        let pages: Arc<dyn Fn(Option<&str>) -> serde_json::Value + Send + Sync> =
            Arc::new(move |cursor| {
                if cursor.is_none() {
                    let head_request = head_requests_for_pages.fetch_add(1, Ordering::SeqCst);
                    return completed_undated_thread_turns_list_response(
                        if head_request == 0 {
                            "turn-undated-match"
                        } else {
                            "turn-undated-changed"
                        },
                        if head_request == 0 {
                            "item-undated-match"
                        } else {
                            "item-undated-changed"
                        },
                        if head_request == 0 {
                            "continue"
                        } else {
                            "different"
                        },
                        Some("page-1"),
                    );
                }
                let page_index = cursor
                    .and_then(|cursor| cursor.strip_prefix("page-"))
                    .expect("page cursor")
                    .parse::<usize>()
                    .expect("numeric cursor");
                if page_index == AMBIGUOUS_TURN_REPAIR_PAGE_LIMIT {
                    return completed_thread_turns_list_response(
                        "turn-old-floor",
                        "item-old-floor",
                        "different",
                        false,
                        None,
                    );
                }
                let turn_id = format!("turn-undated-{page_index}");
                let item_id = format!("item-undated-{page_index}");
                let next_cursor = format!("page-{}", page_index + 1);
                completed_undated_thread_turns_list_response(
                    &turn_id,
                    &item_id,
                    "different",
                    Some(&next_cursor),
                )
            });
        let (client, key, cursors) =
            paginated_ambiguous_client_with_pages(initial_thread, "continue", None, pages).await;

        client
            .force_refresh_thread_authoritative("srv", "thread-1")
            .await
            .expect("capped undated repair succeeds");
        assert!(
            client
                .pending_turn_reconciliation()
                .get(&key)
                .is_some_and(|pending| pending.unanchored_replay_turn_id.is_some()
                    && pending.repair_cursor.as_deref()
                        == Some(&format!("page-{AMBIGUOUS_TURN_REPAIR_PAGE_LIMIT}")))
        );

        client
            .force_refresh_thread_authoritative("srv", "thread-1")
            .await
            .expect("resumed undated repair succeeds");

        assert_eq!(
            cursors.lock().expect("cursor log lock").as_slice(),
            [
                None,
                Some("page-1".to_string()),
                Some("page-2".to_string()),
                Some("page-3".to_string()),
                Some("page-4".to_string()),
                Some("page-5".to_string()),
                Some("page-6".to_string()),
                Some("page-7".to_string()),
                Some("page-8".to_string()),
                Some("page-9".to_string()),
                None,
                Some(format!("page-{AMBIGUOUS_TURN_REPAIR_PAGE_LIMIT}")),
            ]
        );
        assert!(client.pending_turn_reconciliation().contains_key(&key));
        let thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("retained undated repair thread");
        assert_eq!(thread.queued_follow_up_drafts.len(), 1);
        assert!(thread.queued_follow_up_drafts[0].autosend_claimed);
        assert!(thread.items.is_empty());
    }

    #[tokio::test]
    async fn paginated_unanchored_replay_observation_survives_page_cap() {
        assert_paginated_unanchored_replay_observation_survives_page_cap().await;
    }

    #[tokio::test]
    async fn embedded_replay_consumes_ambiguous_claim_when_pagination_is_disabled() {
        let mut initial_thread = make_thread_snapshot("srv", "thread-1");
        initial_thread.initial_turns_loaded = true;

        let (client, key, requests) = reconcile_embedded_ambiguous_claim(
            initial_thread,
            false,
            "retained",
            "turn-new",
            "retained",
            1,
            None,
        )
        .await;

        assert_eq!(
            requests.lock().expect("request log lock").as_slice(),
            ["thread/resume:false"]
        );
        assert!(!client.pending_turn_reconciliation().contains_key(&key));
        let thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("embedded replay thread");
        assert!(thread.queued_follow_up_drafts.is_empty());
        assert_eq!(thread.items.len(), 1);
    }

    #[tokio::test]
    async fn legacy_ignored_exclude_turns_consumes_ambiguous_claim() {
        let mut initial_thread = make_thread_snapshot("srv", "thread-1");
        initial_thread.initial_turns_loaded = true;

        let (client, key, requests) = reconcile_embedded_ambiguous_claim(
            initial_thread,
            true,
            "retained",
            "turn-new",
            "retained",
            1,
            None,
        )
        .await;

        assert_eq!(
            requests.lock().expect("request log lock").as_slice(),
            ["thread/resume:true", "thread/turns/list"]
        );
        assert!(!client.app_store.server_supports_turn_pagination("srv"));
        assert!(!client.pending_turn_reconciliation().contains_key(&key));
        assert!(
            client
                .app_store
                .thread_snapshot(&key)
                .expect("legacy replay thread")
                .queued_follow_up_drafts
                .is_empty()
        );
    }

    #[tokio::test]
    async fn embedded_known_repeated_prompt_releases_ambiguous_claim() {
        let mut initial_thread = make_thread_snapshot("srv", "thread-1");
        initial_thread.initial_turns_loaded = true;
        initial_thread.items.push(hydrated_user_item(
            "turn-old",
            "item-authoritative",
            "continue",
        ));

        let (client, key, _) = reconcile_embedded_ambiguous_claim(
            initial_thread,
            false,
            "continue",
            "turn-old",
            "continue",
            1,
            None,
        )
        .await;

        assert!(!client.pending_turn_reconciliation().contains_key(&key));
        let thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("embedded repeated-prompt thread");
        assert_eq!(thread.queued_follow_up_drafts.len(), 1);
        assert!(!thread.queued_follow_up_drafts[0].autosend_claimed);
    }

    #[tokio::test]
    async fn embedded_no_match_releases_claim_with_unknown_baseline() {
        let initial_thread = make_thread_snapshot("srv", "thread-1");

        let (client, key, _) = reconcile_embedded_ambiguous_claim(
            initial_thread,
            false,
            "retained",
            "turn-new",
            "different",
            1,
            None,
        )
        .await;

        assert!(!client.pending_turn_reconciliation().contains_key(&key));
        let thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("embedded no-match thread");
        assert_eq!(thread.queued_follow_up_drafts.len(), 1);
        assert!(!thread.queued_follow_up_drafts[0].autosend_claimed);
    }

    #[tokio::test]
    async fn embedded_match_without_causal_anchor_retains_claim() {
        let initial_thread = make_thread_snapshot("srv", "thread-1");

        let (client, key, requests) = reconcile_embedded_ambiguous_claim(
            initial_thread,
            false,
            "retained",
            "turn-unknown",
            "retained",
            2,
            None,
        )
        .await;

        assert_eq!(
            requests.lock().expect("request log lock").as_slice(),
            ["thread/resume:false", "thread/resume:false"]
        );
        assert!(client.pending_turn_reconciliation().contains_key(&key));
        let thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("embedded unanchored thread");
        assert_eq!(thread.queued_follow_up_drafts.len(), 1);
        assert!(thread.queued_follow_up_drafts[0].autosend_claimed);
        assert!(thread.initial_turns_loaded);
        assert_eq!(thread.items.len(), 1);
    }

    #[tokio::test]
    async fn embedded_match_after_known_empty_baseline_consumes_claim() {
        let mut initial_thread = make_thread_snapshot("srv", "thread-1");
        initial_thread.initial_turns_loaded = true;

        let (client, key, requests) = reconcile_embedded_ambiguous_claim(
            initial_thread,
            false,
            "retained",
            "turn-unknown",
            "retained",
            1,
            None,
        )
        .await;

        assert_eq!(
            requests.lock().expect("request log lock").as_slice(),
            ["thread/resume:false"]
        );
        assert!(!client.pending_turn_reconciliation().contains_key(&key));
        let thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("embedded known-empty-baseline thread");
        assert!(thread.queued_follow_up_drafts.is_empty());
        assert!(thread.initial_turns_loaded);
        assert_eq!(thread.items.len(), 1);
    }

    #[tokio::test]
    async fn metadata_only_fallback_retains_ambiguous_claim() {
        let client = MobileClient::new();
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
        enqueue_follow_up(&client, &key, "retained");
        assert!(
            client
                .app_store
                .try_claim_first_queued_follow_up(&key)
                .is_some()
        );
        assert!(client.mark_turn_start_ambiguous(key.clone()));

        let handler: TestRequestHandler = Arc::new(move |request| match request {
            upstream::ClientRequest::ThreadResume { .. } => {
                Err(RpcError::Transport(TransportError::SendFailed(
                    "remote app-server worker channel is closed".to_string(),
                )))
            }
            upstream::ClientRequest::ThreadRead { params, .. } => {
                assert!(!params.include_turns);
                Ok(successful_thread_read_response(thread_id))
            }
            other => Err(RpcError::Deserialization(format!(
                "unexpected metadata ambiguity request: {}",
                other.method()
            ))),
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

        client
            .force_refresh_thread_authoritative(server_id, thread_id)
            .await
            .expect("metadata fallback succeeds");

        assert!(client.pending_turn_reconciliation().contains_key(&key));
        let thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("metadata-refreshed thread");
        assert_eq!(thread.queued_follow_up_drafts.len(), 1);
        assert!(thread.queued_follow_up_drafts[0].autosend_claimed);
    }

    #[tokio::test]
    async fn send_failed_turn_start_retains_claim_for_authoritative_reconciliation() {
        let (client, key, _config, turn_start_requests) = client_with_ambiguous_autosend(|| {
            TransportError::SendFailed("response channel closed after write".to_string())
        })
        .await;

        assert_eq!(turn_start_requests.load(Ordering::SeqCst), 1);
        assert!(client.pending_turn_reconciliation().contains_key(&key));
        let thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("ambiguous thread snapshot");
        assert_eq!(thread.queued_follow_up_drafts.len(), 1);
        assert!(thread.queued_follow_up_drafts[0].autosend_claimed);
    }

    #[tokio::test(start_paused = true)]
    async fn ambiguous_reconciliation_retries_transient_refresh_failure() {
        let client = MobileClient::new();
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
            .set_server_supports_turn_pagination(server_id, true);
        client
            .app_store
            .upsert_thread_snapshot(make_thread_snapshot(server_id, thread_id));
        enqueue_follow_up(&client, &key, "retained");
        assert!(
            client
                .app_store
                .try_claim_first_queued_follow_up(&key)
                .is_some()
        );

        let resume_attempts = Arc::new(AtomicUsize::new(0));
        let handler: TestRequestHandler = {
            let resume_attempts = Arc::clone(&resume_attempts);
            Arc::new(move |request| match request {
                upstream::ClientRequest::ThreadResume { .. } => {
                    if resume_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                        Err(RpcError::Transport(TransportError::Disconnected))
                    } else {
                        Ok(successful_thread_resume_response(thread_id))
                    }
                }
                upstream::ClientRequest::ThreadTurnsList { .. } => {
                    Ok(successful_thread_turns_list_response())
                }
                other => Err(RpcError::Deserialization(format!(
                    "unexpected ambiguity retry request: {}",
                    other.method()
                ))),
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

        client.schedule_ambiguous_turn_reconciliation(key.clone());
        while resume_attempts.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        assert!(client.pending_turn_reconciliation().contains_key(&key));

        while resume_attempts.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
            tokio::time::advance(tokio::time::Duration::from_millis(250)).await;
        }
        while client.pending_turn_reconciliation().contains_key(&key) {
            tokio::task::yield_now().await;
        }

        assert_eq!(resume_attempts.load(Ordering::SeqCst), 2);
        let thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("reconciled thread");
        assert_eq!(thread.queued_follow_up_drafts.len(), 1);
        assert!(!thread.queued_follow_up_drafts[0].autosend_claimed);
    }

    #[tokio::test]
    async fn pre_send_disconnect_hydrates_idle_then_retries_claim_once() {
        let client = MobileClient::new();
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
        enqueue_follow_up(&client, &key, "retained");

        maybe_send_next_local_queued_follow_up(Arc::clone(&client), key.clone()).await;

        assert!(client.pending_turn_reconciliation().contains_key(&key));
        let thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("ambiguous thread snapshot");
        assert!(thread.queued_follow_up_drafts[0].autosend_claimed);

        let turn_start_requests = Arc::new(AtomicUsize::new(0));
        let handler: TestRequestHandler = {
            let turn_start_requests = Arc::clone(&turn_start_requests);
            Arc::new(move |request| match request {
                upstream::ClientRequest::ThreadResume { .. } => {
                    Ok(successful_thread_resume_response(thread_id))
                }
                upstream::ClientRequest::ThreadTurnsList { .. } => {
                    Ok(successful_thread_turns_list_response())
                }
                upstream::ClientRequest::TurnStart { .. } => {
                    turn_start_requests.fetch_add(1, Ordering::SeqCst);
                    Ok(successful_turn_start_response())
                }
                other => Err(RpcError::Deserialization(format!(
                    "unexpected pre-send recovery request: {}",
                    other.method()
                ))),
            })
        };
        let replacement = Arc::new(ServerSession::test_stub_with_handlers(
            config,
            Some(handler),
            None,
            None,
        ));
        client
            .sessions
            .write()
            .expect("sessions lock")
            .insert(server_id.to_string(), replacement);

        client
            .external_resume_thread(server_id, thread_id, None)
            .await
            .expect("idle authoritative hydration succeeds");
        assert!(!client.pending_turn_reconciliation().contains_key(&key));
        let thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("hydrated idle thread");
        assert!(!thread.queued_follow_up_drafts[0].autosend_claimed);

        maybe_send_next_local_queued_follow_up(Arc::clone(&client), key.clone()).await;
        assert_eq!(turn_start_requests.load(Ordering::SeqCst), 1);
        let thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("retried thread");
        assert_eq!(thread.active_turn_id.as_deref(), Some("turn-follow-up"));
        assert!(thread.queued_follow_up_drafts.is_empty());
    }

    #[tokio::test]
    async fn autosend_releases_claim_without_duplication_when_turn_activates_behind_lock() {
        let client = MobileClient::new();
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
        enqueue_follow_up(&client, &key, "retained");

        let requests = Arc::new(AtomicUsize::new(0));
        let handler: TestRequestHandler = {
            let requests = Arc::clone(&requests);
            Arc::new(move |_| {
                requests.fetch_add(1, Ordering::SeqCst);
                Ok(successful_turn_start_response())
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

        let lock_guard = client.turn_start_lock(&key).lock_owned().await;
        let autosend = tokio::spawn(maybe_send_next_local_queued_follow_up(
            Arc::clone(&client),
            key.clone(),
        ));
        loop {
            let claimed = client
                .app_store
                .thread_snapshot(&key)
                .and_then(|thread| thread.queued_follow_up_drafts.first().cloned())
                .is_some_and(|draft| draft.autosend_claimed);
            if claimed {
                break;
            }
            tokio::task::yield_now().await;
        }
        client
            .app_store
            .mark_turn_started_from_response(&key, "turn-other", None);
        drop(lock_guard);
        autosend.await.expect("autosend task should finish");

        assert_eq!(requests.load(Ordering::SeqCst), 0);
        let thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("thread snapshot");
        assert_eq!(thread.active_turn_id.as_deref(), Some("turn-other"));
        assert_eq!(thread.queued_follow_up_drafts.len(), 1);
        assert_eq!(thread.queued_follow_up_drafts[0].preview.text, "retained");
        assert!(!thread.queued_follow_up_drafts[0].autosend_claimed);
    }

    #[tokio::test]
    async fn autosend_lock_timeout_releases_undispatched_claim() {
        let client =
            MobileClient::new_with_turn_request_timeout(tokio::time::Duration::from_millis(20));
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
        enqueue_follow_up(&client, &key, "retained");
        let session = Arc::new(ServerSession::test_stub_with_handlers(
            config, None, None, None,
        ));
        client
            .sessions
            .write()
            .expect("sessions lock")
            .insert(server_id.to_string(), session);

        let lock_guard = client.turn_start_lock(&key).lock_owned().await;
        maybe_send_next_local_queued_follow_up(Arc::clone(&client), key.clone()).await;
        drop(lock_guard);

        let thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("thread snapshot");
        assert_eq!(thread.queued_follow_up_drafts.len(), 1);
        assert!(!thread.queued_follow_up_drafts[0].autosend_claimed);
        assert!(!client.pending_turn_reconciliation().contains_key(&key));
    }

    #[tokio::test]
    async fn autosend_blocked_by_pending_reconciliation_releases_undispatched_claim() {
        let client = MobileClient::new();
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
        enqueue_follow_up(&client, &key, "retained");
        let session = Arc::new(ServerSession::test_stub_with_handlers(
            config, None, None, None,
        ));
        client
            .sessions
            .write()
            .expect("sessions lock")
            .insert(server_id.to_string(), session);
        assert!(client.mark_turn_start_ambiguous(key.clone()));

        maybe_send_next_local_queued_follow_up(Arc::clone(&client), key.clone()).await;

        let thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("thread snapshot");
        assert_eq!(thread.queued_follow_up_drafts.len(), 1);
        assert!(!thread.queued_follow_up_drafts[0].autosend_claimed);
        assert!(client.pending_turn_reconciliation().contains_key(&key));
    }

    #[tokio::test]
    async fn concurrent_manual_turn_starts_serialize_and_queue_second_message() {
        let client = MobileClient::new();
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

        let requests = Arc::new(StdMutex::new(Vec::new()));
        let handler: TestRequestHandler = {
            let requests = Arc::clone(&requests);
            Arc::new(move |request| {
                let upstream::ClientRequest::TurnStart { params, .. } = request else {
                    panic!("expected turn/start");
                };
                let text = params
                    .input
                    .iter()
                    .find_map(|input| match input {
                        upstream::UserInput::Text { text, .. } => Some(text.clone()),
                        _ => None,
                    })
                    .expect("text input");
                requests.lock().expect("request log lock").push(text);
                Ok(successful_turn_start_response())
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

        let first = client.start_turn(server_id, turn_start_params(thread_id, "first"));
        let second = client.start_turn(server_id, turn_start_params(thread_id, "second"));
        let (first_result, second_result) = tokio::join!(first, second);
        first_result.expect("first start succeeds");
        second_result.expect("second start queues");

        assert_eq!(
            requests.lock().expect("request log lock").as_slice(),
            ["first"]
        );
        let snapshot = client.app_store.snapshot();
        let thread = snapshot.threads.get(&key).expect("thread snapshot");
        assert_eq!(thread.active_turn_id.as_deref(), Some("turn-follow-up"));
        assert_eq!(thread.queued_follow_up_drafts.len(), 1);
        assert_eq!(thread.queued_follow_up_drafts[0].preview.text, "second");
        assert!(!thread.queued_follow_up_drafts[0].autosend_claimed);
    }

    #[tokio::test]
    async fn manual_send_retries_idle_retained_draft_before_new_message() {
        let client = MobileClient::new();
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
        enqueue_follow_up(&client, &key, "retained");

        let sent = Arc::new(StdMutex::new(Vec::new()));
        let handler: TestRequestHandler = {
            let sent = Arc::clone(&sent);
            Arc::new(move |request| {
                let upstream::ClientRequest::TurnStart { params, .. } = request else {
                    panic!("expected turn/start");
                };
                let text = params
                    .input
                    .iter()
                    .find_map(|input| match input {
                        upstream::UserInput::Text { text, .. } => Some(text.clone()),
                        _ => None,
                    })
                    .expect("text input");
                sent.lock().expect("request log lock").push(text);
                Ok(successful_turn_start_response())
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

        client
            .start_turn(server_id, turn_start_params(thread_id, "new"))
            .await
            .expect("retained draft retry succeeds");

        assert_eq!(
            sent.lock().expect("request log lock").as_slice(),
            ["retained"]
        );
        let thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("thread snapshot");
        assert_eq!(thread.active_turn_id.as_deref(), Some("turn-follow-up"));
        assert_eq!(thread.queued_follow_up_drafts.len(), 1);
        assert_eq!(thread.queued_follow_up_drafts[0].preview.text, "new");
        assert!(!thread.queued_follow_up_drafts[0].autosend_claimed);
    }

    #[tokio::test]
    async fn failed_manual_retry_keeps_both_idle_drafts_claimable() {
        let client = MobileClient::new();
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
        enqueue_follow_up(&client, &key, "retained");
        let handler: TestRequestHandler = Arc::new(|request| {
            assert!(matches!(request, upstream::ClientRequest::TurnStart { .. }));
            Err(RpcError::Deserialization("retry failed".to_string()))
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

        assert!(
            client
                .start_turn(server_id, turn_start_params(thread_id, "new"))
                .await
                .is_err()
        );

        let thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("thread snapshot");
        assert_eq!(thread.active_turn_id, None);
        assert_eq!(thread.queued_follow_up_drafts.len(), 2);
        assert_eq!(thread.queued_follow_up_drafts[0].preview.text, "retained");
        assert_eq!(thread.queued_follow_up_drafts[1].preview.text, "new");
        assert!(
            thread
                .queued_follow_up_drafts
                .iter()
                .all(|draft| !draft.autosend_claimed)
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn timed_out_manual_retry_holds_claim_until_exact_response_resolves() {
        let client =
            MobileClient::new_with_turn_request_timeout(tokio::time::Duration::from_millis(100));
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
        enqueue_follow_up(&client, &key, "retained");
        let requests = Arc::new(AtomicUsize::new(0));
        let handler: TestRequestHandler = {
            let requests = Arc::clone(&requests);
            Arc::new(move |request| {
                assert!(matches!(request, upstream::ClientRequest::TurnStart { .. }));
                requests.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(250));
                Ok(successful_turn_start_response())
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

        let error = client
            .start_turn(server_id, turn_start_params(thread_id, "new"))
            .await
            .expect_err("hung turn/start should time out");

        assert!(matches!(error, RpcError::Timeout));
        assert!(client.timed_out_turn_request_pending(&key));
        let retry_error = client
            .start_turn(server_id, turn_start_params(thread_id, "retry"))
            .await
            .expect_err("second start should time out behind the exact waiter");
        assert!(matches!(retry_error, RpcError::Timeout));
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        let thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("thread snapshot");
        assert_eq!(thread.active_turn_id, None);
        assert_eq!(thread.queued_follow_up_drafts.len(), 2);
        assert_eq!(thread.queued_follow_up_drafts[0].preview.text, "retained");
        assert_eq!(thread.queued_follow_up_drafts[1].preview.text, "new");
        assert!(thread.queued_follow_up_drafts[0].autosend_claimed);
        assert!(!thread.queued_follow_up_drafts[1].autosend_claimed);
        assert!(
            client
                .app_store
                .try_claim_first_queued_follow_up(&key)
                .is_none()
        );

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                let resolved = client
                    .app_store
                    .thread_snapshot(&key)
                    .is_some_and(|thread| {
                        thread.active_turn_id.as_deref() == Some("turn-follow-up")
                    });
                if resolved {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("late turn/start response should resolve the held claim");

        let snapshot = client.app_store.snapshot();
        let thread = snapshot.threads.get(&key).expect("thread snapshot");
        assert_eq!(thread.active_turn_id.as_deref(), Some("turn-follow-up"));
        assert!(!client.timed_out_turn_request_pending(&key));
        assert_eq!(thread.queued_follow_up_drafts.len(), 1);
        assert_eq!(thread.queued_follow_up_drafts[0].preview.text, "new");
        assert!(!thread.queued_follow_up_drafts[0].autosend_claimed);
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn autosend_timeout_idle_hydration_retains_claim_until_late_success() {
        let client =
            MobileClient::new_with_turn_request_timeout(tokio::time::Duration::from_millis(100));
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
        enqueue_follow_up(&client, &key, "retained");

        let original_requests = Arc::new(AtomicUsize::new(0));
        let original_handler: TestRequestHandler = {
            let original_requests = Arc::clone(&original_requests);
            Arc::new(move |request| {
                assert!(matches!(request, upstream::ClientRequest::TurnStart { .. }));
                original_requests.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(250));
                Ok(successful_turn_start_response())
            })
        };
        let original_session = Arc::new(ServerSession::test_stub_with_handlers(
            config.clone(),
            Some(original_handler),
            None,
            None,
        ));
        client
            .sessions
            .write()
            .expect("sessions lock")
            .insert(server_id.to_string(), original_session);

        maybe_send_next_local_queued_follow_up(Arc::clone(&client), key.clone()).await;

        assert_eq!(original_requests.load(Ordering::SeqCst), 1);
        assert!(!client.pending_turn_reconciliation().contains_key(&key));
        let thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("timed-out autosend thread");
        assert!(thread.queued_follow_up_drafts[0].autosend_claimed);

        let replacement_turn_starts = Arc::new(AtomicUsize::new(0));
        let replacement_handler: TestRequestHandler = {
            let replacement_turn_starts = Arc::clone(&replacement_turn_starts);
            Arc::new(move |request| match request {
                upstream::ClientRequest::ThreadResume { .. } => {
                    Ok(successful_thread_resume_response(thread_id))
                }
                upstream::ClientRequest::ThreadTurnsList { .. } => {
                    Ok(successful_thread_turns_list_response())
                }
                upstream::ClientRequest::TurnStart { .. } => {
                    replacement_turn_starts.fetch_add(1, Ordering::SeqCst);
                    Ok(successful_turn_start_response())
                }
                other => Err(RpcError::Deserialization(format!(
                    "unexpected timeout hydration request: {}",
                    other.method()
                ))),
            })
        };
        let replacement_session = Arc::new(ServerSession::test_stub_with_handlers(
            config,
            Some(replacement_handler),
            None,
            None,
        ));
        client
            .sessions
            .write()
            .expect("sessions lock")
            .insert(server_id.to_string(), replacement_session);

        client
            .external_resume_thread(server_id, thread_id, None)
            .await
            .expect("idle hydration should complete while request remains in flight");
        let thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("idle hydrated thread");
        assert!(thread.queued_follow_up_drafts[0].autosend_claimed);

        maybe_send_next_local_queued_follow_up(Arc::clone(&client), key.clone()).await;
        assert_eq!(replacement_turn_starts.load(Ordering::SeqCst), 0);

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                let resolved = client
                    .app_store
                    .thread_snapshot(&key)
                    .is_some_and(|thread| {
                        thread.active_turn_id.as_deref() == Some("turn-follow-up")
                    });
                if resolved {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("late turn/start response should consume the held claim");

        let thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("late-resolved thread");
        assert!(thread.queued_follow_up_drafts.is_empty());
        assert_eq!(original_requests.load(Ordering::SeqCst), 1);
        assert_eq!(replacement_turn_starts.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn failed_queued_follow_up_releases_the_claim_for_retry() {
        let client = MobileClient::new();
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
        assert_eq!(thread.queued_follow_up_drafts.len(), 1);
        assert!(!thread.queued_follow_up_drafts[0].autosend_claimed);
        assert!(
            client
                .app_store
                .try_claim_first_queued_follow_up(&key)
                .is_some(),
            "a transient failure must leave the draft claimable for retry"
        );
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

        assert!(
            reconcile_after_store_listener_lag(Arc::clone(&client)).await,
            "RPC failures must request another lag-reconciliation pass"
        );

        assert_eq!(
            requests.lock().expect("request log lock").as_slice(),
            ["thread/list", "thread/resume"]
        );
    }

    #[tokio::test]
    async fn lag_reconcile_transient_rpc_failures_retry_until_clean() {
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

        let list_attempts = Arc::new(AtomicUsize::new(0));
        let resume_attempts = Arc::new(AtomicUsize::new(0));
        let handler: TestRequestHandler = {
            let list_attempts = Arc::clone(&list_attempts);
            let resume_attempts = Arc::clone(&resume_attempts);
            Arc::new(move |request| match request {
                upstream::ClientRequest::ThreadList { .. } => {
                    if list_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                        Err(RpcError::Deserialization(
                            "intentional transient thread-list failure".to_string(),
                        ))
                    } else {
                        Ok(successful_thread_list_response(&[thread_id]))
                    }
                }
                upstream::ClientRequest::ThreadResume { .. } => {
                    if resume_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                        Err(RpcError::Deserialization(
                            "intentional transient thread-resume failure".to_string(),
                        ))
                    } else {
                        Ok(successful_thread_resume_response(thread_id))
                    }
                }
                upstream::ClientRequest::ThreadTurnsList { .. } => {
                    Ok(successful_thread_turns_list_response())
                }
                other => panic!("unexpected request: {other:?}"),
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

        assert!(
            reconcile_after_store_listener_lag(Arc::clone(&client)).await,
            "the transient failures should keep reconciliation scheduled"
        );
        assert!(
            !reconcile_after_store_listener_lag(Arc::clone(&client)).await,
            "the successful retry should finish cleanly"
        );
        assert_eq!(list_attempts.load(Ordering::SeqCst), 2);
        assert_eq!(resume_attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn authoritative_refresh_from_replaced_session_is_discarded() {
        let client = MobileClient::new();
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
        let generation = client.app_store.server_event_generation(server_id);
        let replacement = Arc::new(ServerSession::test_stub_with_handlers(
            config.clone(),
            None,
            None,
            None,
        ));
        let handler: TestRequestHandler = {
            let sessions = Arc::clone(&client.sessions);
            let replacement = Arc::clone(&replacement);
            Arc::new(move |request| {
                assert!(matches!(
                    request,
                    upstream::ClientRequest::ThreadResume { .. }
                ));
                sessions
                    .write()
                    .expect("sessions lock")
                    .insert(server_id.to_string(), Arc::clone(&replacement));
                Ok(successful_thread_resume_response(thread_id))
            })
        };
        let stale_session = Arc::new(ServerSession::test_stub_with_handlers(
            config,
            Some(handler),
            None,
            None,
        ));
        client
            .sessions
            .write()
            .expect("sessions lock")
            .insert(server_id.to_string(), stale_session);

        let applied = client
            .force_refresh_thread_authoritative_if_ui_generation(server_id, thread_id, generation)
            .await
            .expect("stale refresh should return cleanly");

        assert!(!applied);
        let thread = client
            .app_store
            .thread_snapshot(&key)
            .expect("cached thread remains");
        assert!(!thread.is_resumed);
        assert!(!client.direct_resumed_threads().contains(&key));
        assert!(!client.thread_runtime_routes().contains_key(&key));
    }

    #[tokio::test]
    async fn post_reconnect_refresh_retries_after_unrelated_event_invalidates_response() {
        let client = MobileClient::new();
        let server_id = "srv";
        let thread_id = "thread-1";
        let key = ThreadKey {
            server_id: server_id.to_string(),
            thread_id: thread_id.to_string(),
        };
        let unrelated_key = ThreadKey {
            server_id: server_id.to_string(),
            thread_id: "thread-unrelated".to_string(),
        };
        let config = make_server_config(server_id);
        client
            .app_store
            .upsert_server(&config, ServerHealthSnapshot::Connected);
        client
            .app_store
            .upsert_thread_snapshot(make_thread_snapshot(server_id, thread_id));

        let attempts = Arc::new(AtomicUsize::new(0));
        let handler: TestRequestHandler = {
            let app_store = Arc::clone(&client.app_store);
            let attempts = Arc::clone(&attempts);
            Arc::new(move |request| match request {
                upstream::ClientRequest::ThreadResume { .. } => {
                    if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                        app_store.apply_ui_event(&UiEvent::ThreadNameUpdated {
                            key: unrelated_key.clone(),
                            thread_name: Some("unrelated update".to_string()),
                        });
                    }
                    Ok(successful_thread_resume_response(thread_id))
                }
                upstream::ClientRequest::ThreadTurnsList { .. } => {
                    Ok(successful_thread_turns_list_response())
                }
                other => Err(RpcError::Deserialization(format!(
                    "unexpected post-reconnect request: {}",
                    other.method()
                ))),
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

        let applied = super::super::refresh_post_reconnect_thread_authoritative(&client, &key)
            .await
            .expect("post-reconnect refresh should retry cleanly");

        assert!(applied);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert!(
            client
                .app_store
                .thread_snapshot(&key)
                .expect("refreshed thread")
                .is_resumed
        );
    }

    #[tokio::test]
    async fn post_reconnect_refresh_retries_transient_transport_failure() {
        let client = MobileClient::new();
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

        let attempts = Arc::new(AtomicUsize::new(0));
        let handler: TestRequestHandler = {
            let attempts = Arc::clone(&attempts);
            Arc::new(move |request| match request {
                upstream::ClientRequest::ThreadResume { .. } => {
                    if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                        Err(RpcError::Transport(TransportError::Disconnected))
                    } else {
                        Ok(successful_thread_resume_response(thread_id))
                    }
                }
                upstream::ClientRequest::ThreadTurnsList { .. } => {
                    Ok(successful_thread_turns_list_response())
                }
                other => Err(RpcError::Deserialization(format!(
                    "unexpected post-reconnect request: {}",
                    other.method()
                ))),
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

        let applied = super::super::refresh_post_reconnect_thread_authoritative(&client, &key)
            .await
            .expect("transient post-reconnect failure should retry cleanly");

        assert!(applied);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn lag_reconcile_refreshes_authoritative_thread_inventory() {
        let client = MobileClient::new();
        let server_id = "srv";
        let config = make_server_config(server_id);
        client
            .app_store
            .upsert_server(&config, ServerHealthSnapshot::Connected);
        client
            .app_store
            .upsert_thread_snapshot(make_thread_snapshot(server_id, "archived-thread"));

        let requests = Arc::new(StdMutex::new(Vec::<String>::new()));
        let handler: TestRequestHandler = {
            let requests = Arc::clone(&requests);
            Arc::new(move |request| {
                requests
                    .lock()
                    .expect("request log lock")
                    .push(request.method().to_string());
                match request {
                    upstream::ClientRequest::ThreadList { .. } => {
                        Ok(successful_thread_list_response(&["new-thread"]))
                    }
                    upstream::ClientRequest::ThreadResume { params, .. } => {
                        Ok(successful_thread_resume_response(&params.thread_id))
                    }
                    upstream::ClientRequest::ThreadTurnsList { .. } => {
                        Ok(successful_thread_turns_list_response())
                    }
                    other => Err(RpcError::Deserialization(format!(
                        "unexpected request: {}",
                        other.method()
                    ))),
                }
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

        reconcile_after_store_listener_lag(Arc::clone(&client)).await;

        let snapshot = client.app_store.snapshot();
        assert!(!snapshot.threads.contains_key(&ThreadKey {
            server_id: server_id.to_string(),
            thread_id: "archived-thread".to_string(),
        }));
        assert!(snapshot.threads.contains_key(&ThreadKey {
            server_id: server_id.to_string(),
            thread_id: "new-thread".to_string(),
        }));
        assert_eq!(
            requests.lock().expect("request log lock").as_slice(),
            ["thread/list", "thread/resume", "thread/turns/list"]
        );
    }

    #[tokio::test]
    async fn lag_reconcile_autosends_follow_up_after_missed_turn_completed() {
        let client = MobileClient::new();
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
        enqueue_follow_up(&client, &key, "continue after lag");

        let requests = Arc::new(StdMutex::new(Vec::<String>::new()));
        let handler: TestRequestHandler = {
            let requests = Arc::clone(&requests);
            Arc::new(move |request| {
                requests
                    .lock()
                    .expect("request log lock")
                    .push(request.method().to_string());
                match request {
                    upstream::ClientRequest::ThreadList { .. } => {
                        Ok(successful_thread_list_response(&[thread_id]))
                    }
                    upstream::ClientRequest::ThreadResume { .. } => {
                        Ok(successful_thread_resume_response(thread_id))
                    }
                    upstream::ClientRequest::ThreadTurnsList { .. } => {
                        Ok(successful_thread_turns_list_response())
                    }
                    upstream::ClientRequest::TurnStart { .. } => {
                        Ok(successful_turn_start_response())
                    }
                    other => Err(RpcError::Deserialization(format!(
                        "unexpected request: {}",
                        other.method()
                    ))),
                }
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

        reconcile_after_store_listener_lag(Arc::clone(&client)).await;

        assert_eq!(
            requests.lock().expect("request log lock").as_slice(),
            [
                "thread/list",
                "thread/resume",
                "thread/turns/list",
                "turn/start"
            ]
        );
        let snapshot = client.app_store.snapshot();
        let thread = snapshot.threads.get(&key).expect("thread snapshot");
        assert!(thread.queued_follow_up_drafts.is_empty());
        assert_eq!(thread.active_turn_id.as_deref(), Some("turn-follow-up"));
    }
}
