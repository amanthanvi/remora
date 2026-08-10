use super::*;
use crate::conversation_uniffi::{HydratedDividerData, HydratedUserMessageData};
use crate::types::{ApprovalKind, PendingUserInputOption, PendingUserInputQuestion};
use codex_app_server_protocol::{
    McpToolCallProgressNotification, ModelRerouteReason, ModelReroutedNotification,
    TurnDiffUpdatedNotification, TurnPlanStep, TurnPlanStepStatus, TurnPlanUpdatedNotification,
};
use tokio::sync::broadcast::error::TryRecvError;

fn make_thread_info(id: &str) -> ThreadInfo {
    ThreadInfo {
        id: id.to_string(),
        title: Some(format!("Thread {id}")),
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

fn drain_updates(
    receiver: &mut tokio::sync::broadcast::Receiver<AppStoreUpdateRecord>,
) -> Vec<AppStoreUpdateRecord> {
    let mut updates = Vec::new();
    loop {
        match receiver.try_recv() {
            Ok(update) => updates.push(update),
            Err(TryRecvError::Empty) => break,
            Err(error) => panic!("unexpected broadcast receive error: {error:?}"),
        }
    }
    updates
}

fn make_server_config(server_id: &str) -> ServerConfig {
    ServerConfig {
        server_id: server_id.to_string(),
        display_name: format!("Server {server_id}"),
        host: "example.local".to_string(),
        port: 22,
        websocket_url: None,
        is_local: false,
        tls: false,
    }
}

#[test]
fn sync_thread_list_for_runtime_tags_threads() {
    let reducer = AppStoreReducer::new();
    let config = make_server_config("srv");
    reducer.upsert_server(&config, ServerHealthSnapshot::Connected);

    reducer.sync_thread_list_for_runtime("srv", "pi".to_string(), &[make_thread_info("thread-1")]);

    let key = ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "thread-1".to_string(),
    };
    assert_eq!(
        reducer.thread_snapshot(&key).unwrap().agent_runtime_kind,
        "pi".to_string()
    );
}

#[test]
fn authoritative_refresh_fence_rejects_state_older_than_streamed_event() {
    let reducer = AppStoreReducer::new();
    let key = ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "archived".to_string(),
    };
    let stale = ThreadSnapshot::from_info("srv", make_thread_info(&key.thread_id));
    reducer.upsert_thread_snapshot(stale.clone());
    let request_generation = reducer.server_event_generation("srv");

    reducer.apply_ui_event(&UiEvent::ThreadArchived { key: key.clone() });
    let applied = reducer.apply_if_server_event_generation("srv", request_generation, |store| {
        store.upsert_thread_snapshot(stale);
    });

    assert!(applied.is_none());
    assert!(!reducer.snapshot().threads.contains_key(&key));
}

#[test]
fn authoritative_refresh_fence_ignores_unrelated_server_events() {
    let reducer = AppStoreReducer::new();
    let request_generation = reducer.server_event_generation("srv-a");

    reducer.apply_ui_event(&UiEvent::ThreadArchived {
        key: ThreadKey {
            server_id: "srv-b".to_string(),
            thread_id: "other".to_string(),
        },
    });
    let applied =
        reducer.apply_if_server_event_generation("srv-a", request_generation, |_| "applied");

    assert_eq!(applied, Some("applied"));
}

#[test]
fn authoritative_runtime_refresh_prunes_only_that_runtime() {
    let reducer = AppStoreReducer::new();
    for (runtime, thread_id) in [("codex", "stale-codex"), ("pi", "keep-pi")] {
        let mut thread = ThreadSnapshot::from_info("srv", make_thread_info(thread_id));
        thread.agent_runtime_kind = runtime.to_string();
        reducer.upsert_thread_snapshot(thread);
    }

    reducer.finalize_thread_list_sync_for_runtime("srv", "codex", &HashSet::new());

    assert!(!reducer.snapshot().threads.contains_key(&ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "stale-codex".to_string(),
    }));
    assert!(reducer.snapshot().threads.contains_key(&ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "keep-pi".to_string(),
    }));
}

#[test]
fn update_server_agent_runtimes_replaces_available_metadata() {
    let reducer = AppStoreReducer::new();
    let config = make_server_config("srv");
    reducer.upsert_server(&config, ServerHealthSnapshot::Connected);

    reducer.update_server_agent_runtimes(
        "srv",
        vec![AgentRuntimeInfo {
            kind: "opencode".to_string(),
            name: "opencode".to_string(),
            display_name: "opencode".to_string(),
            available: true,
        }],
    );

    let snapshot = reducer.snapshot();
    let server = snapshot.servers.get("srv").unwrap();
    assert_eq!(server.agent_runtimes.len(), 1);
    assert_eq!(server.agent_runtimes[0].kind, "opencode".to_string());
}

#[test]
fn failed_server_mutating_command_clears_pending_without_marking_success() {
    let reducer = AppStoreReducer::new();
    let config = make_server_config("srv");
    reducer.upsert_server(&config, ServerHealthSnapshot::Connected);

    let request_id = reducer.begin_server_mutating_command(
        "srv",
        ServerMutatingCommandKind::ApprovalResponse,
        "thread",
    );
    assert_eq!(
        reducer.server_pending_mutation_kind("srv"),
        Some(ServerMutatingCommandKind::ApprovalResponse)
    );

    reducer.finish_server_mutating_command_failure("srv", &request_id);

    let snapshot = reducer.snapshot();
    let server = snapshot.servers.get("srv").expect("server exists");
    assert!(server.transport.pending_mutation.is_none());
    assert!(server.transport.last_direct_request_ok_at.is_none());
}

#[test]
fn stale_server_mutating_failure_does_not_clear_newer_pending_command() {
    let reducer = AppStoreReducer::new();
    let config = make_server_config("srv");
    reducer.upsert_server(&config, ServerHealthSnapshot::Connected);

    let stale_request_id = reducer.begin_server_mutating_command(
        "srv",
        ServerMutatingCommandKind::ApprovalResponse,
        "thread",
    );
    let active_request_id = reducer.begin_server_mutating_command(
        "srv",
        ServerMutatingCommandKind::UserInputResponse,
        "thread",
    );

    reducer.finish_server_mutating_command_failure("srv", &stale_request_id);

    let snapshot = reducer.snapshot();
    let pending = snapshot
        .servers
        .get("srv")
        .expect("server exists")
        .transport
        .pending_mutation
        .as_ref()
        .expect("newer pending command survives stale failure");
    assert_eq!(pending.local_request_id, active_request_id);
    assert_eq!(pending.kind, ServerMutatingCommandKind::UserInputResponse);
}

#[test]
fn sync_thread_list_preserves_active_missing_thread() {
    let reducer = AppStoreReducer::new();
    let active_key = ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "active".to_string(),
    };
    reducer.upsert_thread_snapshot(ThreadSnapshot::from_info("srv", make_thread_info("active")));
    reducer.set_active_thread(Some(active_key.clone()));
    let mut receiver = reducer.subscribe();

    reducer.sync_thread_list("srv", &[make_thread_info("other")]);

    let snapshot = reducer.snapshot();
    assert!(snapshot.threads.contains_key(&active_key));
    assert_eq!(snapshot.active_thread, Some(active_key));
    let updates = drain_updates(&mut receiver);
    assert!(
        updates
            .iter()
            .all(|update| !matches!(update, AppStoreUpdateRecord::FullResync))
    );
    assert!(updates.iter().any(|update| matches!(
        update,
        AppStoreUpdateRecord::ThreadUpserted { thread, .. } if thread.key.thread_id == "other"
    )));
}

#[test]
fn update_server_wake_mac_persists_across_upsert() {
    let reducer = AppStoreReducer::new();
    let config = make_server_config("srv");

    reducer.upsert_server(&config, ServerHealthSnapshot::Connecting);
    reducer.update_server_wake_mac("srv", Some("aa:bb:cc:dd:ee:ff".to_string()));
    reducer.upsert_server(&config, ServerHealthSnapshot::Connected);

    let snapshot = reducer.snapshot();
    let server = snapshot.servers.get("srv").expect("server snapshot");
    assert_eq!(server.wake_mac.as_deref(), Some("aa:bb:cc:dd:ee:ff"));
}

#[test]
fn sync_thread_list_emits_incremental_updates_without_full_resync() {
    let reducer = AppStoreReducer::new();
    let existing_key = ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "existing".to_string(),
    };
    reducer.upsert_thread_snapshot(ThreadSnapshot::from_info(
        "srv",
        make_thread_info("existing"),
    ));
    let mut receiver = reducer.subscribe();

    let mut updated_existing = make_thread_info("existing");
    updated_existing.title = Some("Updated existing".to_string());
    updated_existing.model = Some("gpt-5.4".to_string());
    updated_existing.status = ThreadSummaryStatus::Active;

    let mut inserted = make_thread_info("inserted");
    inserted.model = Some("gpt-5.4".to_string());

    reducer.sync_thread_list("srv", &[updated_existing.clone(), inserted.clone()]);

    let updates = drain_updates(&mut receiver);
    assert!(
        updates
            .iter()
            .all(|update| !matches!(update, AppStoreUpdateRecord::FullResync))
    );
    assert!(updates.iter().any(|update| matches!(
        update,
        AppStoreUpdateRecord::ThreadMetadataChanged { state, .. }
            if state.key == existing_key
                && state.info == updated_existing
                && state.model.as_deref() == Some("gpt-5.4")
    )));
    assert!(updates.iter().any(|update| matches!(
        update,
        AppStoreUpdateRecord::ThreadUpserted { thread, .. }
            if thread.key.thread_id == "inserted"
                && thread.info == inserted
                && thread.model.as_deref() == Some("gpt-5.4")
    )));
}

#[test]
fn paginated_thread_list_upserts_pages_before_final_prune() {
    let reducer = AppStoreReducer::new();
    reducer.upsert_thread_snapshot(ThreadSnapshot::from_info("srv", make_thread_info("stale")));
    let mut receiver = reducer.subscribe();

    let page_one = make_thread_info("page-one");
    let page_two = make_thread_info("page-two");
    reducer.upsert_thread_list_page("srv", &[page_one.clone()]);
    reducer.upsert_thread_list_page("srv", &[page_two.clone()]);
    reducer.finalize_thread_list_sync(
        "srv",
        &HashSet::from([page_one.id.clone(), page_two.id.clone()]),
    );

    let snapshot = reducer.snapshot();
    assert!(snapshot.threads.contains_key(&ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "page-one".to_string(),
    }));
    assert!(snapshot.threads.contains_key(&ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "page-two".to_string(),
    }));
    assert!(!snapshot.threads.contains_key(&ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "stale".to_string(),
    }));

    let updates = drain_updates(&mut receiver);
    assert!(updates.iter().any(|update| matches!(
        update,
        AppStoreUpdateRecord::ThreadUpserted { thread, .. }
            if thread.key.thread_id == "page-one"
    )));
    assert!(updates.iter().any(|update| matches!(
        update,
        AppStoreUpdateRecord::ThreadUpserted { thread, .. }
            if thread.key.thread_id == "page-two"
    )));
    assert!(updates.iter().any(|update| matches!(
        update,
        AppStoreUpdateRecord::ThreadRemoved { key, .. }
            if key.thread_id == "stale"
    )));
}

#[test]
fn sync_thread_list_preserves_existing_title_when_incoming_title_missing() {
    let reducer = AppStoreReducer::new();
    let key = ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "thread".to_string(),
    };
    reducer.upsert_thread_snapshot(ThreadSnapshot::from_info("srv", make_thread_info("thread")));

    let mut incoming = make_thread_info("thread");
    incoming.title = None;
    incoming.preview = Some("First user message".to_string());

    reducer.sync_thread_list("srv", &[incoming]);

    let snapshot = reducer.snapshot();
    let thread = snapshot.threads.get(&key).expect("thread exists");
    assert_eq!(thread.info.title.as_deref(), Some("Thread thread"));
    assert_eq!(thread.info.preview.as_deref(), Some("First user message"));
}

#[test]
fn turn_diff_updates_become_conversation_items() {
    let reducer = AppStoreReducer::new();
    let key = ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "thread".to_string(),
    };
    reducer.upsert_thread_snapshot(ThreadSnapshot::from_info("srv", make_thread_info("thread")));

    reducer.apply_ui_event(&UiEvent::TurnDiffUpdated {
        key: key.clone(),
        notification: TurnDiffUpdatedNotification {
            thread_id: key.thread_id.clone(),
            turn_id: "turn-1".to_string(),
            diff: "@@ -1 +1 @@\n-old\n+new".to_string(),
        },
    });

    let snapshot = reducer.snapshot();
    let thread = snapshot.threads.get(&key).expect("thread exists");
    let diff_item = thread
        .items
        .iter()
        .find(|item| item.id == "turn-diff-turn-1")
        .expect("turn diff item exists");
    match &diff_item.content {
        HydratedConversationItemContent::TurnDiff(data) => assert!(data.diff.contains("+new")),
        other => panic!("expected turn diff item, got {other:?}"),
    }
}

#[test]
fn turn_plan_updates_populate_active_plan_progress_without_timeline_items() {
    let reducer = AppStoreReducer::new();
    let key = ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "thread".to_string(),
    };
    reducer.upsert_thread_snapshot(ThreadSnapshot::from_info("srv", make_thread_info("thread")));

    reducer.apply_ui_event(&UiEvent::TurnPlanUpdated {
        key: key.clone(),
        notification: TurnPlanUpdatedNotification {
            thread_id: key.thread_id.clone(),
            turn_id: "turn-1".to_string(),
            explanation: Some("working".to_string()),
            plan: vec![
                TurnPlanStep {
                    step: "Inspect renderer".to_string(),
                    status: TurnPlanStepStatus::Completed,
                },
                TurnPlanStep {
                    step: "Restore task cards".to_string(),
                    status: TurnPlanStepStatus::InProgress,
                },
            ],
        },
    });

    let snapshot = reducer.snapshot();
    let thread = snapshot.threads.get(&key).expect("thread exists");
    assert!(
        thread
            .items
            .iter()
            .all(|item| item.id != "turn-plan-turn-1"),
        "turn/plan/updated should not create historical timeline items"
    );
    let progress = thread
        .active_plan_progress
        .as_ref()
        .expect("active plan progress should exist");
    assert_eq!(progress.turn_id, "turn-1");
    assert_eq!(progress.explanation.as_deref(), Some("working"));
    assert_eq!(progress.plan.len(), 2);
    assert_eq!(
        progress.plan[0].status,
        crate::types::AppPlanStepStatus::Completed
    );
    assert_eq!(
        progress.plan[1].status,
        crate::types::AppPlanStepStatus::InProgress
    );
    assert_eq!(progress.plan[1].step, "Restore task cards");
}

#[test]
fn mcp_progress_updates_append_to_existing_item() {
    let reducer = AppStoreReducer::new();
    let key = ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "thread".to_string(),
    };
    let mut thread = ThreadSnapshot::from_info("srv", make_thread_info("thread"));
    thread
        .items
        .push(crate::conversation_uniffi::HydratedConversationItem {
            id: "mcp-1".to_string(),
            content: HydratedConversationItemContent::McpToolCall(
                crate::conversation_uniffi::HydratedMcpToolCallData {
                    server: "github".to_string(),
                    tool: "search".to_string(),
                    status: crate::types::AppOperationStatus::InProgress,
                    duration_ms: None,
                    arguments_json: None,
                    content_summary: None,
                    structured_content_json: None,
                    raw_output_json: None,
                    error_message: None,
                    progress_messages: Vec::new(),
                    computer_use: None,
                },
            ),
            source_turn_id: Some("turn-1".to_string()),
            source_turn_index: None,
            timestamp: None,
            is_from_user_turn_boundary: false,
        });
    reducer.upsert_thread_snapshot(thread);

    reducer.apply_ui_event(&UiEvent::McpToolCallProgress {
        key: key.clone(),
        notification: McpToolCallProgressNotification {
            thread_id: key.thread_id.clone(),
            turn_id: "turn-1".to_string(),
            item_id: "mcp-1".to_string(),
            message: "Fetched 3 results".to_string(),
        },
    });

    let snapshot = reducer.snapshot();
    let thread = snapshot.threads.get(&key).expect("thread exists");
    let mcp_item = thread.items.iter().find(|item| item.id == "mcp-1").unwrap();
    match &mcp_item.content {
        HydratedConversationItemContent::McpToolCall(data) => {
            assert_eq!(
                data.progress_messages,
                vec!["Fetched 3 results".to_string()]
            );
        }
        other => panic!("expected mcp tool item, got {other:?}"),
    }
}

#[test]
fn reasoning_delta_missing_item_creates_placeholder_without_thread_upsert() {
    let reducer = AppStoreReducer::new();
    let key = ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "thread".to_string(),
    };
    let mut thread = ThreadSnapshot::from_info("srv", make_thread_info("thread"));
    thread.active_turn_id = Some("turn-1".to_string());
    reducer.upsert_thread_snapshot(thread);
    let mut receiver = reducer.subscribe();
    assert!(drain_updates(&mut receiver).is_empty());

    reducer.apply_ui_event(&UiEvent::ReasoningDelta {
        key: key.clone(),
        item_id: "reasoning-1".to_string(),
        delta: "thinking".to_string(),
    });

    let updates = drain_updates(&mut receiver);
    assert!(updates.iter().any(|update| matches!(
        update,
        AppStoreUpdateRecord::ThreadItemChanged { key: update_key, item, .. }
            if update_key == &key && item.id == "reasoning-1"
    )));
    assert!(
        updates
            .iter()
            .all(|update| !matches!(update, AppStoreUpdateRecord::ThreadUpserted { .. }))
    );

    let snapshot = reducer.snapshot();
    let thread = snapshot.threads.get(&key).expect("thread exists");
    let item = thread
        .items
        .iter()
        .find(|item| item.id == "reasoning-1")
        .unwrap();
    match &item.content {
        HydratedConversationItemContent::Reasoning(data) => {
            assert_eq!(data.content, vec!["thinking".to_string()]);
        }
        other => panic!("expected reasoning item, got {other:?}"),
    }
}

#[test]
fn completed_empty_reasoning_item_preserves_streamed_placeholder_content() {
    let reducer = AppStoreReducer::new();
    let key = ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "thread".to_string(),
    };
    let mut thread = ThreadSnapshot::from_info("srv", make_thread_info("thread"));
    thread.active_turn_id = Some("turn-1".to_string());
    reducer.upsert_thread_snapshot(thread);
    let mut receiver = reducer.subscribe();
    assert!(drain_updates(&mut receiver).is_empty());

    reducer.apply_ui_event(&UiEvent::ReasoningDelta {
        key: key.clone(),
        item_id: "reasoning-1".to_string(),
        delta: "streamed reasoning".to_string(),
    });
    assert!(!drain_updates(&mut receiver).is_empty());

    reducer.apply_ui_event(&UiEvent::ItemCompleted {
        key: key.clone(),
        notification: upstream::ItemCompletedNotification {
            item: upstream::ThreadItem::Reasoning {
                id: "reasoning-1".to_string(),
                summary: Vec::new(),
                content: Vec::new(),
            },
            thread_id: key.thread_id.clone(),
            turn_id: "turn-1".to_string(),
            completed_at_ms: 0,
        },
    });

    let snapshot = reducer.snapshot();
    let thread = snapshot.threads.get(&key).expect("thread exists");
    let item = thread
        .items
        .iter()
        .find(|item| item.id == "reasoning-1")
        .unwrap();
    match &item.content {
        HydratedConversationItemContent::Reasoning(data) => {
            assert_eq!(data.content, vec!["streamed reasoning".to_string()]);
        }
        other => panic!("expected reasoning item, got {other:?}"),
    }
}

#[test]
fn thread_status_changed_idle_clears_active_turn() {
    let reducer = AppStoreReducer::new();
    let key = ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "thread".to_string(),
    };
    let mut thread = ThreadSnapshot::from_info("srv", make_thread_info("thread"));
    thread.active_turn_id = Some("turn-1".to_string());
    thread.info.status = ThreadSummaryStatus::Active;
    reducer.upsert_thread_snapshot(thread);
    let mut receiver = reducer.subscribe();
    assert!(drain_updates(&mut receiver).is_empty());

    reducer.apply_ui_event(&UiEvent::ThreadStatusChanged {
        key: key.clone(),
        notification: upstream::ThreadStatusChangedNotification {
            thread_id: key.thread_id.clone(),
            status: upstream::ThreadStatus::Idle,
        },
    });

    let snapshot = reducer.snapshot();
    let thread = snapshot.threads.get(&key).expect("thread exists");
    assert_eq!(thread.active_turn_id, None);
    assert_eq!(thread.info.status, ThreadSummaryStatus::Idle);

    let updates = drain_updates(&mut receiver);
    assert!(updates.iter().any(|update| matches!(
        update,
        AppStoreUpdateRecord::ThreadMetadataChanged { state, .. }
            if state.key == key
                && state.active_turn_id.is_none()
                && state.info.status == ThreadSummaryStatus::Idle
    )));
}

#[test]
fn thread_item_changed_projects_multi_agent_targets_to_display_labels() {
    let reducer = AppStoreReducer::new();
    let parent_key = ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "parent".to_string(),
    };

    reducer.upsert_thread_snapshot(ThreadSnapshot::from_info("srv", make_thread_info("parent")));

    let mut child_info = make_thread_info("child-thread");
    child_info.agent_nickname = Some("Scout".to_string());
    child_info.agent_role = Some("explorer".to_string());
    child_info.parent_thread_id = Some("parent".to_string());
    reducer.upsert_thread_snapshot(ThreadSnapshot::from_info("srv", child_info));

    let mut receiver = reducer.subscribe();
    assert!(drain_updates(&mut receiver).is_empty());

    reducer.apply_item_update(
        &parent_key,
        HydratedConversationItem {
            id: "collab-1".to_string(),
            content: HydratedConversationItemContent::MultiAgentAction(
                crate::conversation_uniffi::HydratedMultiAgentActionData {
                    tool: "spawnAgent".to_string(),
                    status: AppOperationStatus::Completed,
                    prompt: Some("Inspect".to_string()),
                    targets: vec!["child-thread".to_string()],
                    receiver_thread_ids: vec!["child-thread".to_string()],
                    agent_states: vec![crate::conversation_uniffi::HydratedMultiAgentStateData {
                        target_id: "child-thread".to_string(),
                        status: crate::types::AppSubagentStatus::Running,
                        message: Some("Working".to_string()),
                    }],
                },
            ),
            source_turn_id: Some("turn-1".to_string()),
            source_turn_index: None,
            timestamp: None,
            is_from_user_turn_boundary: false,
        },
    );

    let updates = drain_updates(&mut receiver);
    let update_item = updates
        .into_iter()
        .find_map(|update| match update {
            AppStoreUpdateRecord::ThreadItemChanged { key, item, .. } if key == parent_key => {
                Some(item)
            }
            _ => None,
        })
        .expect("expected ThreadItemChanged update");

    let HydratedConversationItemContent::MultiAgentAction(data) = update_item.content else {
        panic!("expected multi-agent action update");
    };
    assert_eq!(data.targets, vec!["Scout [explorer]".to_string()]);
    assert_eq!(data.receiver_thread_ids, vec!["child-thread".to_string()]);
}

#[test]
fn command_output_delta_repairs_wrong_type_without_thread_upsert() {
    let reducer = AppStoreReducer::new();
    let key = ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "thread".to_string(),
    };
    let mut thread = ThreadSnapshot::from_info("srv", make_thread_info("thread"));
    thread.active_turn_id = Some("turn-1".to_string());
    thread.items.push(HydratedConversationItem {
        id: "call-1".to_string(),
        content: HydratedConversationItemContent::Assistant(HydratedAssistantMessageData {
            text: "wrong".to_string(),
            agent_nickname: None,
            agent_role: None,
            phase: None,
        }),
        source_turn_id: Some("turn-1".to_string()),
        source_turn_index: None,
        timestamp: None,
        is_from_user_turn_boundary: false,
    });
    reducer.upsert_thread_snapshot(thread);
    let mut receiver = reducer.subscribe();
    assert!(drain_updates(&mut receiver).is_empty());

    reducer.apply_ui_event(&UiEvent::CommandOutputDelta {
        key: key.clone(),
        item_id: "call-1".to_string(),
        delta: "stdout".to_string(),
    });

    let updates = drain_updates(&mut receiver);
    assert!(updates.iter().any(|update| matches!(
        update,
        AppStoreUpdateRecord::ThreadItemChanged { key: update_key, item, .. }
            if update_key == &key && item.id == "call-1"
    )));
    assert!(
        updates
            .iter()
            .all(|update| !matches!(update, AppStoreUpdateRecord::ThreadUpserted { .. }))
    );

    let snapshot = reducer.snapshot();
    let thread = snapshot.threads.get(&key).expect("thread exists");
    let item = thread
        .items
        .iter()
        .find(|item| item.id == "call-1")
        .unwrap();
    match &item.content {
        HydratedConversationItemContent::CommandExecution(data) => {
            assert_eq!(data.output.as_deref(), Some("stdout"));
            assert_eq!(data.status, AppOperationStatus::InProgress);
        }
        other => panic!("expected command execution item, got {other:?}"),
    }
}

#[test]
fn mcp_progress_missing_item_creates_placeholder_without_thread_upsert() {
    let reducer = AppStoreReducer::new();
    let key = ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "thread".to_string(),
    };
    let mut thread = ThreadSnapshot::from_info("srv", make_thread_info("thread"));
    thread.active_turn_id = Some("turn-1".to_string());
    reducer.upsert_thread_snapshot(thread);
    let mut receiver = reducer.subscribe();
    assert!(drain_updates(&mut receiver).is_empty());

    reducer.apply_ui_event(&UiEvent::McpToolCallProgress {
        key: key.clone(),
        notification: McpToolCallProgressNotification {
            thread_id: key.thread_id.clone(),
            turn_id: "turn-1".to_string(),
            item_id: "mcp-1".to_string(),
            message: "Fetched 3 results".to_string(),
        },
    });

    let updates = drain_updates(&mut receiver);
    assert!(updates.iter().any(|update| matches!(
        update,
        AppStoreUpdateRecord::ThreadItemChanged { key: update_key, item, .. }
            if update_key == &key && item.id == "mcp-1"
    )));
    assert!(
        updates
            .iter()
            .all(|update| !matches!(update, AppStoreUpdateRecord::ThreadUpserted { .. }))
    );

    let snapshot = reducer.snapshot();
    let thread = snapshot.threads.get(&key).expect("thread exists");
    let item = thread.items.iter().find(|item| item.id == "mcp-1").unwrap();
    match &item.content {
        HydratedConversationItemContent::McpToolCall(data) => {
            assert_eq!(
                data.progress_messages,
                vec!["Fetched 3 results".to_string()]
            );
            assert_eq!(data.status, AppOperationStatus::InProgress);
        }
        other => panic!("expected mcp tool item, got {other:?}"),
    }
}

#[test]
fn model_reroutes_become_divider_items() {
    let reducer = AppStoreReducer::new();
    let key = ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "thread".to_string(),
    };
    reducer.upsert_thread_snapshot(ThreadSnapshot::from_info("srv", make_thread_info("thread")));

    reducer.apply_ui_event(&UiEvent::ModelRerouted {
        key: key.clone(),
        notification: ModelReroutedNotification {
            thread_id: key.thread_id.clone(),
            turn_id: "turn-1".to_string(),
            from_model: "gpt-5".to_string(),
            to_model: "gpt-5-mini".to_string(),
            reason: ModelRerouteReason::HighRiskCyberActivity,
        },
    });

    let snapshot = reducer.snapshot();
    let thread = snapshot.threads.get(&key).expect("thread exists");
    let reroute_item = thread
        .items
        .iter()
        .find(|item| item.id == "model-rerouted-turn-1")
        .expect("model reroute item exists");
    match &reroute_item.content {
        HydratedConversationItemContent::Divider(HydratedDividerData::ModelRerouted {
            from_model,
            to_model,
            reason,
        }) => {
            assert_eq!(from_model.as_deref(), Some("gpt-5"));
            assert_eq!(to_model, "gpt-5-mini");
            assert_eq!(reason.as_deref(), Some("High Risk Cyber Activity"));
        }
        other => panic!("expected model reroute divider, got {other:?}"),
    }
}

#[test]
fn resolved_user_input_appends_response_item() {
    let reducer = AppStoreReducer::new();
    let key = ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "thread".to_string(),
    };
    reducer.upsert_thread_snapshot(ThreadSnapshot::from_info("srv", make_thread_info("thread")));
    reducer.replace_pending_user_inputs(vec![PendingUserInputRequest {
        id: "req-1".to_string(),
        server_id: key.server_id.clone(),
        thread_id: key.thread_id.clone(),
        turn_id: "turn-1".to_string(),
        item_id: "tool-1".to_string(),
        questions: vec![PendingUserInputQuestion {
            id: "q-1".to_string(),
            header: Some("Choice".to_string()),
            question: "Pick one".to_string(),
            is_other_allowed: false,
            is_secret: false,
            options: vec![PendingUserInputOption {
                label: "A".to_string(),
                description: Some("First".to_string()),
            }],
        }],
        requester_agent_nickname: None,
        requester_agent_role: None,
    }]);

    reducer.resolve_pending_user_input_with_response(
        "req-1",
        vec![PendingUserInputAnswer {
            question_id: "q-1".to_string(),
            answers: vec!["A".to_string()],
        }],
    );

    let snapshot = reducer.snapshot();
    let thread = snapshot.threads.get(&key).expect("thread exists");
    let item = thread
        .local_overlay_items
        .iter()
        .find(|item| item.id == "user-input-response:req-1")
        .expect("response item exists");
    match &item.content {
        HydratedConversationItemContent::UserInputResponse(data) => {
            assert_eq!(data.questions.len(), 1);
            assert_eq!(data.questions[0].answer, "A");
        }
        other => panic!("expected user input response item, got {other:?}"),
    }
}

#[test]
fn replacing_identical_pending_user_inputs_is_silent() {
    let reducer = AppStoreReducer::new();
    let request = PendingUserInputRequest {
        id: "req-1".to_string(),
        server_id: "srv".to_string(),
        thread_id: "thread".to_string(),
        turn_id: "turn-1".to_string(),
        item_id: "tool-1".to_string(),
        questions: vec![PendingUserInputQuestion {
            id: "q-1".to_string(),
            header: Some("Choice".to_string()),
            question: "Pick one".to_string(),
            is_other_allowed: false,
            is_secret: false,
            options: vec![PendingUserInputOption {
                label: "A".to_string(),
                description: Some("First".to_string()),
            }],
        }],
        requester_agent_nickname: None,
        requester_agent_role: None,
    };

    reducer.replace_pending_user_inputs(vec![request.clone()]);
    let mut receiver = reducer.subscribe();
    assert!(drain_updates(&mut receiver).is_empty());

    reducer.replace_pending_user_inputs(vec![request]);

    assert!(drain_updates(&mut receiver).is_empty());
}

#[test]
fn replacing_identical_pending_approvals_with_seeds_is_silent() {
    let reducer = AppStoreReducer::new();
    let approval = PendingApproval {
        id: "approval-1".to_string(),
        server_id: "srv".to_string(),
        kind: ApprovalKind::Command,
        thread_id: Some("thread".to_string()),
        turn_id: Some("turn-1".to_string()),
        item_id: Some("item-1".to_string()),
        command: Some("ls".to_string()),
        path: None,
        grant_root: None,
        cwd: Some("/tmp".to_string()),
        reason: Some("Need approval".to_string()),
    };
    let seeded = PendingApprovalWithSeed {
        approval,
        seed: PendingApprovalSeed {
            request_id: upstream::RequestId::String("approval-1".to_string()),
            raw_params: serde_json::json!({"command": "ls"}),
        },
    };

    reducer.replace_pending_approvals_with_seeds(vec![seeded.clone()]);
    let mut receiver = reducer.subscribe();
    assert!(drain_updates(&mut receiver).is_empty());

    reducer.replace_pending_approvals_with_seeds(vec![seeded]);

    assert!(drain_updates(&mut receiver).is_empty());
}

#[test]
fn resolved_user_input_hides_other_placeholder_when_note_present() {
    let reducer = AppStoreReducer::new();
    let key = ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "thread".to_string(),
    };
    reducer.upsert_thread_snapshot(ThreadSnapshot::from_info("srv", make_thread_info("thread")));
    reducer.replace_pending_user_inputs(vec![PendingUserInputRequest {
        id: "req-1".to_string(),
        server_id: key.server_id.clone(),
        thread_id: key.thread_id.clone(),
        turn_id: "turn-1".to_string(),
        item_id: "tool-1".to_string(),
        questions: vec![PendingUserInputQuestion {
            id: "q-1".to_string(),
            header: Some("Choice".to_string()),
            question: "Pick one".to_string(),
            is_other_allowed: true,
            is_secret: false,
            options: vec![PendingUserInputOption {
                label: "A".to_string(),
                description: Some("First".to_string()),
            }],
        }],
        requester_agent_nickname: None,
        requester_agent_role: None,
    }]);

    reducer.resolve_pending_user_input_with_response(
        "req-1",
        vec![PendingUserInputAnswer {
            question_id: "q-1".to_string(),
            answers: vec![
                "None of the above".to_string(),
                "user_note: Custom answer".to_string(),
            ],
        }],
    );

    let snapshot = reducer.snapshot();
    let thread = snapshot.threads.get(&key).expect("thread exists");
    let item = thread
        .local_overlay_items
        .iter()
        .find(|item| item.id == "user-input-response:req-1")
        .expect("response item exists");
    match &item.content {
        HydratedConversationItemContent::UserInputResponse(data) => {
            assert_eq!(data.questions.len(), 1);
            assert_eq!(data.questions[0].answer, "Custom answer");
        }
        other => panic!("expected user input response item, got {other:?}"),
    }
}

#[test]
fn server_backed_user_input_response_supersedes_local_synthetic_copy() {
    let reducer = AppStoreReducer::new();
    let key = ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "thread".to_string(),
    };
    let mut local = ThreadSnapshot::from_info("srv", make_thread_info("thread"));
    local.items.push(HydratedConversationItem {
        id: "user-input-response:req-1".to_string(),
        content: HydratedConversationItemContent::UserInputResponse(
            HydratedUserInputResponseData {
                questions: vec![HydratedUserInputResponseQuestionData {
                    id: "q-1".to_string(),
                    header: Some("Choice".to_string()),
                    question: "Pick one".to_string(),
                    answer: "A".to_string(),
                    options: vec![],
                }],
            },
        ),
        source_turn_id: Some("turn-1".to_string()),
        source_turn_index: None,
        timestamp: None,
        is_from_user_turn_boundary: false,
    });
    reducer.upsert_thread_snapshot(local);

    let mut server = ThreadSnapshot::from_info("srv", make_thread_info("thread"));
    server.items.push(HydratedConversationItem {
        id: "server-item-1".to_string(),
        content: HydratedConversationItemContent::UserInputResponse(
            HydratedUserInputResponseData {
                questions: vec![HydratedUserInputResponseQuestionData {
                    id: "q-1".to_string(),
                    header: Some("Choice".to_string()),
                    question: "Pick one".to_string(),
                    answer: "A".to_string(),
                    options: vec![],
                }],
            },
        ),
        source_turn_id: Some("turn-1".to_string()),
        source_turn_index: None,
        timestamp: None,
        is_from_user_turn_boundary: false,
    });
    reducer.upsert_thread_snapshot(server);

    let snapshot = reducer.snapshot();
    let thread = snapshot.threads.get(&key).expect("thread exists");
    assert!(thread.local_overlay_items.is_empty());
    assert_eq!(thread.items.len(), 1);
    assert_eq!(thread.items[0].id, "server-item-1");
}

#[test]
fn stage_local_user_message_overlay_projects_immediately() {
    let reducer = AppStoreReducer::new();
    let key = ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "thread".to_string(),
    };
    reducer.upsert_thread_snapshot(ThreadSnapshot::from_info("srv", make_thread_info("thread")));

    let overlay_id = reducer
        .stage_local_user_message_overlay(
            &key,
            &[upstream::UserInput::Text {
                text: "hello from composer".to_string(),
                text_elements: Vec::new(),
            }],
        )
        .expect("overlay id");

    let snapshot = reducer.snapshot();
    let thread = snapshot.threads.get(&key).expect("thread exists");
    let item = thread
        .local_overlay_items
        .iter()
        .find(|item| item.id == overlay_id)
        .expect("overlay item exists");
    match &item.content {
        HydratedConversationItemContent::User(data) => {
            assert_eq!(data.text, "hello from composer");
            assert!(data.image_data_uris.is_empty());
        }
        other => panic!("expected user overlay item, got {other:?}"),
    }
    assert!(item.is_from_user_turn_boundary);
    assert!(item.source_turn_id.is_none());
}

#[test]
fn server_backed_user_message_supersedes_local_overlay_after_turn_binding() {
    let reducer = AppStoreReducer::new();
    let key = ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "thread".to_string(),
    };
    reducer.upsert_thread_snapshot(ThreadSnapshot::from_info("srv", make_thread_info("thread")));
    let mut receiver = reducer.subscribe();
    assert!(drain_updates(&mut receiver).is_empty());

    let overlay_id = reducer
        .stage_local_user_message_overlay(
            &key,
            &[upstream::UserInput::Text {
                text: "hello from composer".to_string(),
                text_elements: Vec::new(),
            }],
        )
        .expect("overlay id");
    reducer.bind_local_user_message_overlay_to_turn(&key, &overlay_id, "turn-1");

    reducer.apply_item_update(
        &key,
        HydratedConversationItem {
            id: "server-user-item".to_string(),
            content: HydratedConversationItemContent::User(
                crate::conversation_uniffi::HydratedUserMessageData {
                    text: "hello from composer".to_string(),
                    image_data_uris: Vec::new(),
                },
            ),
            source_turn_id: Some("turn-1".to_string()),
            source_turn_index: None,
            timestamp: None,
            is_from_user_turn_boundary: true,
        },
    );

    let updates = drain_updates(&mut receiver);
    assert!(updates.iter().any(|update| matches!(
        update,
        AppStoreUpdateRecord::ThreadUpserted { thread, .. }
            if thread.key == key
    )));

    let snapshot = reducer.snapshot();
    let thread = snapshot.threads.get(&key).expect("thread exists");
    assert!(thread.local_overlay_items.is_empty());
    assert_eq!(thread.items.len(), 1);
    assert_eq!(thread.items[0].id, "server-user-item");
}

#[test]
fn binding_turn_after_server_user_item_arrives_removes_local_overlay() {
    let reducer = AppStoreReducer::new();
    let key = ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "thread".to_string(),
    };
    reducer.upsert_thread_snapshot(ThreadSnapshot::from_info("srv", make_thread_info("thread")));
    let mut receiver = reducer.subscribe();
    assert!(drain_updates(&mut receiver).is_empty());

    let overlay_id = reducer
        .stage_local_user_message_overlay(
            &key,
            &[upstream::UserInput::Text {
                text: "hello from composer".to_string(),
                text_elements: Vec::new(),
            }],
        )
        .expect("overlay id");

    reducer.apply_item_update(
        &key,
        HydratedConversationItem {
            id: "server-user-item".to_string(),
            content: HydratedConversationItemContent::User(
                crate::conversation_uniffi::HydratedUserMessageData {
                    text: "hello from composer".to_string(),
                    image_data_uris: Vec::new(),
                },
            ),
            source_turn_id: Some("turn-1".to_string()),
            source_turn_index: None,
            timestamp: None,
            is_from_user_turn_boundary: true,
        },
    );

    reducer.bind_local_user_message_overlay_to_turn(&key, &overlay_id, "turn-1");

    let updates = drain_updates(&mut receiver);
    assert!(updates.iter().any(|update| matches!(
        update,
        AppStoreUpdateRecord::ThreadUpserted { thread, .. }
            if thread.key == key
    )));

    let snapshot = reducer.snapshot();
    let thread = snapshot.threads.get(&key).expect("thread exists");
    assert!(thread.local_overlay_items.is_empty());
    assert_eq!(thread.items.len(), 1);
    assert_eq!(thread.items[0].id, "server-user-item");
}

#[test]
fn server_backed_user_message_immediately_supersedes_unbound_local_overlay() {
    let reducer = AppStoreReducer::new();
    let key = ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "thread".to_string(),
    };
    reducer.upsert_thread_snapshot(ThreadSnapshot::from_info("srv", make_thread_info("thread")));

    reducer
        .stage_local_user_message_overlay(
            &key,
            &[upstream::UserInput::Text {
                text: "hello from composer".to_string(),
                text_elements: Vec::new(),
            }],
        )
        .expect("overlay id");

    reducer.apply_item_update(
        &key,
        HydratedConversationItem {
            id: "server-user-item".to_string(),
            content: HydratedConversationItemContent::User(
                crate::conversation_uniffi::HydratedUserMessageData {
                    text: "hello from composer".to_string(),
                    image_data_uris: Vec::new(),
                },
            ),
            source_turn_id: Some("turn-1".to_string()),
            source_turn_index: None,
            timestamp: None,
            is_from_user_turn_boundary: true,
        },
    );

    let snapshot = reducer.snapshot();
    let thread = snapshot.threads.get(&key).expect("thread exists");
    assert!(thread.local_overlay_items.is_empty());
    assert_eq!(thread.items.len(), 1);
    assert_eq!(thread.items[0].id, "server-user-item");
}

/// Repro for the duplicate-first-user-message bug surfaced by user
/// testing on a v0.125 remote. iOS flow on a brand-new thread is:
///   1. apply_thread_start_response (handled by MobileClient).
///   2. stage_local_user_message_overlay + bind_local_user_message_overlay_to_turn.
///   3. apply_item_update twice (ItemStarted + ItemCompleted, same content).
///   4. apply_thread_read_response from refreshThreadSnapshot — embedded
///      thread.turns carry the same UserMessage.
///
/// After step 3 the overlay should be superseded; after step 4 the
/// authoritative thread.items must contain exactly one user item.
/// The merged view (boundary projection) must not duplicate.
#[test]
fn first_user_message_does_not_duplicate_after_thread_read() {
    let reducer = AppStoreReducer::new();
    let key = ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "thread-1".to_string(),
    };
    // Step 1: thread/start equivalent — populate empty thread snapshot.
    reducer.upsert_thread_snapshot(ThreadSnapshot::from_info(
        "srv",
        make_thread_info("thread-1"),
    ));

    // Step 2: stage overlay + bind via the TurnStarted-event path
    // (`bind_first_pending_local_user_message_overlay_to_turn`), which
    // is what the real device run does — `start_turn` itself binds via
    // the RPC response, but typically the TurnStarted notification
    // arrives first and the bind there is a no-op for the second call.
    let inputs = vec![upstream::UserInput::Text {
        text: "run some tools im testing".to_string(),
        text_elements: Vec::new(),
    }];
    let overlay_id = reducer
        .stage_local_user_message_overlay(&key, &inputs)
        .expect("overlay id");
    // First bind comes from the TurnStarted event handler (sets the
    // active_turn_id on the thread before the bind, so let's simulate).
    reducer
        .mutate_thread_with_result(&key, |thread| {
            thread.active_turn_id = Some("turn-1".to_string());
        })
        .expect("set active_turn_id");
    reducer.bind_first_pending_local_user_message_overlay_to_turn(&key, "turn-1");
    // Second bind comes from start_turn's RPC response — no-op since
    // overlay is already bound to turn-1.
    reducer.bind_local_user_message_overlay_to_turn(&key, &overlay_id, "turn-1");

    // Step 3: ItemStarted/ItemCompleted with a UserMessage item built via
    // the same `conversation_item_from_upstream` path the event loop uses.
    // This sets `source_turn_id: None` (events don't carry the turn id)
    // and exercises the real dedupe surface.
    let mut update_receiver = reducer.subscribe();
    // Drain any prior emits so we observe only the upcoming ones.
    let _ = drain_updates(&mut update_receiver);

    let upstream_item = upstream::ThreadItem::UserMessage {
        id: "server-user-item".to_string(),
        content: inputs.clone(),
    };
    let server_user_item = crate::store::actions::conversation_item_from_upstream_with_turn(
        upstream_item.clone(),
        None,
    )
    .expect("hydrate user item");
    reducer.apply_item_update(&key, server_user_item.clone());

    // The first apply_item_update must fire `ThreadUpserted` because
    // it removes the local overlay. If it fires only `ThreadItemChanged`
    // (the bug shape from the device log), iOS will see both the overlay
    // and the upstream item as separate bubbles.
    let updates = drain_updates(&mut update_receiver);
    let saw_upsert = updates
        .iter()
        .any(|u| matches!(u, AppStoreUpdateRecord::ThreadUpserted { .. }));
    assert!(
        saw_upsert,
        "apply_item_update for the upstream UserMessage must emit \
         ThreadUpserted to clear the local overlay; got updates={updates:?}"
    );

    // ItemCompleted re-applies the same item idempotently.
    reducer.apply_item_update(&key, server_user_item.clone());

    // After events the local overlay must be superseded and exactly
    // one user item lives in `thread.items`.
    {
        let snap = reducer.snapshot();
        let thread = snap.threads.get(&key).expect("thread");
        assert!(
            thread.local_overlay_items.is_empty(),
            "overlay should be swept once upstream item arrives; got {:?}",
            thread.local_overlay_items
        );
        assert_eq!(thread.items.len(), 1, "events should yield one item");
    }

    // Step 4: thread/read carrying the same UserMessage in
    // thread.turns. Build the equivalent ThreadSnapshot via the same
    // helper apply_thread_read_response uses, then upsert.
    let upstream_thread = upstream::Thread {
        id: "thread-1".to_string(),
        session_id: "session-1".to_string(),
        forked_from_id: None,
        preview: "run some tools im testing".to_string(),
        ephemeral: false,
        model_provider: "openai".to_string(),
        created_at: 1,
        updated_at: 2,
        status: upstream::ThreadStatus::Idle,
        path: Some(std::path::PathBuf::from("/tmp/thread.jsonl")),
        cwd: codex_utils_absolute_path::AbsolutePathBuf::from_absolute_path_checked("/tmp")
            .expect("absolute path"),
        cli_version: "0.125.0".to_string(),
        source: upstream::SessionSource::default(),
        thread_source: None,
        agent_nickname: None,
        agent_role: None,
        git_info: None,
        name: Some("Thread".to_string()),
        turns: vec![upstream::Turn {
            id: "turn-1".to_string(),
            status: upstream::TurnStatus::Completed,
            items: vec![upstream::ThreadItem::UserMessage {
                id: "server-user-item".to_string(),
                content: inputs.clone(),
            }],
            items_view: upstream::TurnItemsView::Full,
            error: None,
            started_at: None,
            completed_at: None,
            duration_ms: None,
        }],
    };
    let mut read_snapshot = crate::thread_snapshot_from_upstream_thread_with_overrides(
        "srv",
        upstream_thread,
        None,
        None,
        None,
        None,
    )
    .expect("read snapshot");
    read_snapshot.initial_turns_loaded = true;
    read_snapshot.older_turns_cursor = None;
    reducer.upsert_thread_snapshot(read_snapshot);

    // Final state: store + overlay merged should yield exactly one
    // User item with the submitted text.
    let snap = reducer.snapshot();
    let thread = snap.threads.get(&key).expect("thread");
    let user_items_in_store: Vec<_> = thread
        .items
        .iter()
        .filter(|item| matches!(item.content, HydratedConversationItemContent::User(_)))
        .collect();
    let user_overlays: Vec<_> = thread
        .local_overlay_items
        .iter()
        .filter(|item| matches!(item.content, HydratedConversationItemContent::User(_)))
        .collect();
    assert_eq!(
        user_items_in_store.len(),
        1,
        "exactly one user item expected in store after thread/read; \
         store={user_items_in_store:?} overlays={user_overlays:?}"
    );
    assert!(
        user_overlays.is_empty(),
        "overlay should not survive thread/read; remaining: {user_overlays:?}"
    );

    // Final UI projection: AppThreadSnapshot.hydrated_conversation_items
    // is what iOS renders. Confirm the merged view also yields exactly
    // one user bubble.
    let app_snapshot = reducer.snapshot();
    let app_thread = crate::store::boundary::project_thread_snapshot(&app_snapshot, &key)
        .expect("project ok")
        .expect("thread present");
    let user_bubbles_in_view: Vec<_> = app_thread
        .hydrated_conversation_items
        .iter()
        .filter(|item| matches!(item.content, HydratedConversationItemContent::User(_)))
        .collect();
    assert_eq!(
        user_bubbles_in_view.len(),
        1,
        "merged hydrated view must show exactly one user bubble; got {user_bubbles_in_view:?}"
    );
}

#[test]
fn upsert_thread_snapshot_preserves_existing_title_when_incoming_title_missing() {
    let reducer = AppStoreReducer::new();
    let key = ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "thread".to_string(),
    };

    reducer.upsert_thread_snapshot(ThreadSnapshot::from_info("srv", make_thread_info("thread")));

    let mut incoming = ThreadSnapshot::from_info("srv", make_thread_info("thread"));
    incoming.info.title = None;
    incoming.info.preview = Some("First user message".to_string());

    reducer.upsert_thread_snapshot(incoming);

    let snapshot = reducer.snapshot();
    let thread = snapshot.threads.get(&key).expect("thread exists");
    assert_eq!(thread.info.title.as_deref(), Some("Thread thread"));
    assert_eq!(thread.info.preview.as_deref(), Some("First user message"));
}

#[test]
fn upsert_thread_snapshot_preserves_existing_preview_when_incoming_preview_missing() {
    let reducer = AppStoreReducer::new();
    let key = ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "thread".to_string(),
    };

    let mut initial = ThreadSnapshot::from_info("srv", make_thread_info("thread"));
    initial.info.preview = Some("First user message".to_string());
    reducer.upsert_thread_snapshot(initial);

    // Incoming snapshot has no preview (e.g. from thread/resume with empty preview)
    let mut incoming = ThreadSnapshot::from_info("srv", make_thread_info("thread"));
    incoming.info.preview = None;
    reducer.upsert_thread_snapshot(incoming);

    let snapshot = reducer.snapshot();
    let thread = snapshot.threads.get(&key).expect("thread exists");
    assert_eq!(
        thread.info.preview.as_deref(),
        Some("First user message"),
        "existing preview should be preserved when incoming is None"
    );
}

#[test]
fn upsert_thread_snapshot_binds_pending_local_user_overlay_to_incoming_active_turn() {
    let reducer = AppStoreReducer::new();
    let key = ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "thread".to_string(),
    };
    reducer.upsert_thread_snapshot(ThreadSnapshot::from_info("srv", make_thread_info("thread")));
    let overlay_id = reducer
        .stage_local_user_message_overlay(
            &key,
            &[upstream::UserInput::Text {
                text: "hello from composer".to_string(),
                text_elements: Vec::new(),
            }],
        )
        .expect("overlay staged");

    let mut incoming = ThreadSnapshot::from_info("srv", make_thread_info("thread"));
    incoming.active_turn_id = Some("turn-1".to_string());
    incoming.info.status = ThreadSummaryStatus::Active;
    incoming
        .items
        .push(crate::conversation_uniffi::HydratedConversationItem {
            id: "server-user-item".to_string(),
            content: HydratedConversationItemContent::User(HydratedUserMessageData {
                text: "hello from composer".to_string(),
                image_data_uris: Vec::new(),
            }),
            source_turn_id: Some("turn-1".to_string()),
            source_turn_index: Some(0),
            timestamp: None,
            is_from_user_turn_boundary: true,
        });

    reducer.upsert_thread_snapshot(incoming);

    let snapshot = reducer.snapshot();
    let thread = snapshot.threads.get(&key).expect("thread exists");
    assert_eq!(thread.active_turn_id.as_deref(), Some("turn-1"));
    assert!(thread.local_overlay_items.is_empty());
    assert_eq!(thread.items.len(), 1);
    assert_eq!(thread.items[0].id, "server-user-item");

    let overlay_still_present = thread
        .items
        .iter()
        .chain(thread.local_overlay_items.iter())
        .any(|item| item.id == overlay_id);
    assert!(!overlay_still_present, "overlay should be deduped");
}

#[test]
fn upsert_thread_snapshot_dedupes_matching_unbound_local_overlay() {
    let reducer = AppStoreReducer::new();
    let key = ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "thread".to_string(),
    };
    reducer.upsert_thread_snapshot(ThreadSnapshot::from_info("srv", make_thread_info("thread")));
    reducer
        .stage_local_user_message_overlay(
            &key,
            &[upstream::UserInput::Text {
                text: "hello from composer".to_string(),
                text_elements: Vec::new(),
            }],
        )
        .expect("overlay staged");

    let mut incoming = ThreadSnapshot::from_info("srv", make_thread_info("thread"));
    incoming
        .items
        .push(crate::conversation_uniffi::HydratedConversationItem {
            id: "server-user-item".to_string(),
            content: HydratedConversationItemContent::User(HydratedUserMessageData {
                text: "hello from composer".to_string(),
                image_data_uris: Vec::new(),
            }),
            source_turn_id: Some("turn-1".to_string()),
            source_turn_index: Some(0),
            timestamp: None,
            is_from_user_turn_boundary: true,
        });

    reducer.upsert_thread_snapshot(incoming);

    let snapshot = reducer.snapshot();
    let thread = snapshot.threads.get(&key).expect("thread exists");
    assert!(thread.local_overlay_items.is_empty());
    assert_eq!(thread.items.len(), 1);
    assert_eq!(thread.items[0].id, "server-user-item");
}

#[test]
fn turn_started_consumes_and_selectively_reanchors_queued_follow_up_preview() {
    let reducer = AppStoreReducer::new();
    let key = ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "thread".to_string(),
    };
    let mut initial = ThreadSnapshot::from_info("srv", make_thread_info("thread"));
    initial.active_turn_id = Some("turn-a".to_string());
    reducer.upsert_thread_snapshot(initial);
    reducer.enqueue_thread_follow_up_preview(
        &key,
        AppQueuedFollowUpPreview {
            id: "queued-1".to_string(),
            kind: crate::store::snapshot::AppQueuedFollowUpKind::Message,
            text: "repeated".to_string(),
        },
    );
    reducer.enqueue_thread_follow_up_preview(
        &key,
        AppQueuedFollowUpPreview {
            id: "queued-2".to_string(),
            kind: crate::store::snapshot::AppQueuedFollowUpKind::Message,
            text: "repeated".to_string(),
        },
    );
    reducer.enqueue_thread_follow_up_preview(
        &key,
        AppQueuedFollowUpPreview {
            id: "queued-3".to_string(),
            kind: crate::store::snapshot::AppQueuedFollowUpKind::Message,
            text: "newer".to_string(),
        },
    );
    reducer.mutate_thread_with_result(&key, |thread| {
        thread.queued_follow_up_drafts[2].causal_anchor_turn_id = Some("turn-c".to_string());
    });
    reducer.apply_ui_event(&UiEvent::TurnCompleted {
        key: key.clone(),
        turn_id: "turn-a".to_string(),
        error: None,
    });
    assert!(reducer.try_claim_first_queued_follow_up(&key).is_some());

    reducer.apply_ui_event(&UiEvent::TurnStarted {
        key: key.clone(),
        turn_id: "turn-b".to_string(),
    });

    let snapshot = reducer.snapshot();
    let thread = snapshot.threads.get(&key).expect("thread exists");
    assert_eq!(thread.active_turn_id.as_deref(), Some("turn-b"));
    assert_eq!(thread.queued_follow_ups.len(), 2);
    assert_eq!(thread.queued_follow_ups[0].id, "queued-2");
    assert_eq!(thread.queued_follow_ups[1].id, "queued-3");
    assert_eq!(
        thread.queued_follow_up_drafts[0]
            .causal_anchor_turn_id
            .as_deref(),
        Some("turn-b")
    );
    assert_eq!(
        thread.queued_follow_up_drafts[1]
            .causal_anchor_turn_id
            .as_deref(),
        Some("turn-c")
    );
}

#[test]
fn authoritative_active_turn_consumes_claimed_queued_follow_up() {
    let reducer = AppStoreReducer::new();
    let key = ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "thread".to_string(),
    };
    reducer.upsert_thread_snapshot(ThreadSnapshot::from_info("srv", make_thread_info("thread")));
    reducer.enqueue_thread_follow_up_draft(
        &key,
        QueuedFollowUpDraft {
            preview: AppQueuedFollowUpPreview {
                id: "queued-1".to_string(),
                kind: crate::store::snapshot::AppQueuedFollowUpKind::Message,
                text: "first".to_string(),
            },
            inputs: vec![upstream::UserInput::Text {
                text: "first".to_string(),
                text_elements: Vec::new(),
            }],
            source_message_json: None,
            causal_anchor_turn_id: None,
            autosend_claimed: false,
        },
    );
    reducer.enqueue_thread_follow_up_preview(
        &key,
        AppQueuedFollowUpPreview {
            id: "queued-2".to_string(),
            kind: crate::store::snapshot::AppQueuedFollowUpKind::Message,
            text: "second".to_string(),
        },
    );
    assert!(reducer.try_claim_first_queued_follow_up(&key).is_some());

    let mut authoritative = ThreadSnapshot::from_info("srv", make_thread_info("thread"));
    authoritative.active_turn_id = Some("turn-follow-up".to_string());
    authoritative.info.status = ThreadSummaryStatus::Active;
    authoritative.items.push(HydratedConversationItem {
        id: "user-follow-up".to_string(),
        content: HydratedConversationItemContent::User(HydratedUserMessageData {
            text: "first".to_string(),
            image_data_uris: Vec::new(),
        }),
        source_turn_id: Some("turn-follow-up".to_string()),
        source_turn_index: Some(0),
        timestamp: None,
        is_from_user_turn_boundary: true,
    });
    reducer.upsert_thread_snapshot(authoritative);

    let snapshot = reducer.snapshot();
    let thread = snapshot.threads.get(&key).expect("thread exists");
    assert_eq!(thread.active_turn_id.as_deref(), Some("turn-follow-up"));
    assert_eq!(thread.queued_follow_up_drafts.len(), 1);
    assert_eq!(
        thread.queued_follow_up_drafts[0]
            .causal_anchor_turn_id
            .as_deref(),
        Some("turn-follow-up")
    );
}

#[test]
fn authoritative_unrelated_active_turn_preserves_claimed_queued_follow_up() {
    let reducer = AppStoreReducer::new();
    let key = ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "thread".to_string(),
    };
    reducer.upsert_thread_snapshot(ThreadSnapshot::from_info("srv", make_thread_info("thread")));
    reducer.enqueue_thread_follow_up_draft(
        &key,
        QueuedFollowUpDraft {
            preview: AppQueuedFollowUpPreview {
                id: "queued-1".to_string(),
                kind: crate::store::snapshot::AppQueuedFollowUpKind::Message,
                text: "claimed follow-up".to_string(),
            },
            inputs: vec![upstream::UserInput::Text {
                text: "claimed follow-up".to_string(),
                text_elements: Vec::new(),
            }],
            source_message_json: None,
            causal_anchor_turn_id: None,
            autosend_claimed: false,
        },
    );
    assert!(reducer.try_claim_first_queued_follow_up(&key).is_some());

    let mut authoritative = ThreadSnapshot::from_info("srv", make_thread_info("thread"));
    authoritative.active_turn_id = Some("turn-other-client".to_string());
    authoritative.info.status = ThreadSummaryStatus::Active;
    authoritative.items.push(HydratedConversationItem {
        id: "other-user-message".to_string(),
        content: HydratedConversationItemContent::User(HydratedUserMessageData {
            text: "unrelated request".to_string(),
            image_data_uris: Vec::new(),
        }),
        source_turn_id: Some("turn-other-client".to_string()),
        source_turn_index: Some(0),
        timestamp: None,
        is_from_user_turn_boundary: true,
    });
    reducer.upsert_thread_snapshot(authoritative);

    let snapshot = reducer.snapshot();
    let thread = snapshot.threads.get(&key).expect("thread exists");
    assert_eq!(thread.active_turn_id.as_deref(), Some("turn-other-client"));
    assert_eq!(thread.queued_follow_up_drafts.len(), 1);
    assert_eq!(thread.queued_follow_up_drafts[0].preview.id, "queued-1");
    assert!(thread.queued_follow_up_drafts[0].autosend_claimed);
}

#[test]
fn turn_started_binds_first_pending_local_user_message_overlay() {
    let reducer = AppStoreReducer::new();
    let key = ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "thread".to_string(),
    };
    reducer.upsert_thread_snapshot(ThreadSnapshot::from_info("srv", make_thread_info("thread")));
    let overlay_id = reducer
        .stage_local_user_message_overlay(
            &key,
            &[upstream::UserInput::Text {
                text: "hello from composer".to_string(),
                text_elements: Vec::new(),
            }],
        )
        .expect("overlay id");

    reducer.apply_ui_event(&UiEvent::TurnStarted {
        key: key.clone(),
        turn_id: "turn-2".to_string(),
    });

    let snapshot = reducer.snapshot();
    let thread = snapshot.threads.get(&key).expect("thread exists");
    let item = thread
        .local_overlay_items
        .iter()
        .find(|item| item.id == overlay_id)
        .expect("overlay item exists");
    assert_eq!(item.source_turn_id.as_deref(), Some("turn-2"));
}

#[test]
fn turn_started_binding_local_user_overlay_emits_thread_upsert_for_reprojection() {
    let reducer = AppStoreReducer::new();
    let key = ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "thread".to_string(),
    };
    let mut thread = ThreadSnapshot::from_info("srv", make_thread_info("thread"));
    thread
        .items
        .push(crate::conversation_uniffi::HydratedConversationItem {
            id: "assistant-1".to_string(),
            content: HydratedConversationItemContent::Assistant(HydratedAssistantMessageData {
                text: "response".to_string(),
                agent_nickname: None,
                agent_role: None,
                phase: None,
            }),
            source_turn_id: Some("turn-1".to_string()),
            source_turn_index: None,
            timestamp: None,
            is_from_user_turn_boundary: false,
        });
    reducer.upsert_thread_snapshot(thread);
    let overlay_id = reducer
        .stage_local_user_message_overlay(
            &key,
            &[upstream::UserInput::Text {
                text: "prompt".to_string(),
                text_elements: Vec::new(),
            }],
        )
        .expect("overlay id");

    let mut receiver = reducer.subscribe();
    let _ = drain_updates(&mut receiver);

    reducer.bind_local_user_message_overlay_to_turn(&key, &overlay_id, "turn-1");

    let updates = drain_updates(&mut receiver);
    assert!(
        updates.iter().any(
            |update| matches!(update, AppStoreUpdateRecord::ThreadUpserted { thread, .. }
                if thread.key == key
                    && thread
                        .hydrated_conversation_items
                        .iter()
                        .map(|item| item.id.as_str())
                        .collect::<Vec<_>>()
                        == vec![overlay_id.as_str(), "assistant-1"])
        ),
        "binding a local user overlay must emit a reprojected thread upsert; got {updates:?}"
    );
}

#[test]
fn thread_name_updated_emits_thread_state_updated_without_thread_upsert() {
    let reducer = AppStoreReducer::new();
    let key = ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "thread".to_string(),
    };
    reducer.upsert_thread_snapshot(ThreadSnapshot::from_info("srv", make_thread_info("thread")));
    let mut receiver = reducer.subscribe();
    assert!(drain_updates(&mut receiver).is_empty());

    reducer.apply_ui_event(&UiEvent::ThreadNameUpdated {
        key: key.clone(),
        thread_name: Some("Renamed".to_string()),
    });

    let updates = drain_updates(&mut receiver);
    assert!(updates.iter().any(|update| matches!(
        update,
        AppStoreUpdateRecord::ThreadMetadataChanged { state, .. }
            if state.key == key && state.info.title.as_deref() == Some("Renamed")
    )));
    assert!(
        updates
            .iter()
            .all(|update| !matches!(update, AppStoreUpdateRecord::ThreadUpserted { .. }))
    );
}

#[test]
fn thread_status_changed_emits_thread_metadata_changed_for_existing_thread() {
    let reducer = AppStoreReducer::new();
    let key = ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "thread".to_string(),
    };
    reducer.upsert_thread_snapshot(ThreadSnapshot::from_info("srv", make_thread_info("thread")));
    let mut receiver = reducer.subscribe();
    assert!(drain_updates(&mut receiver).is_empty());

    reducer.apply_ui_event(&UiEvent::ThreadStatusChanged {
        key: key.clone(),
        notification: codex_app_server_protocol::ThreadStatusChangedNotification {
            thread_id: key.thread_id.clone(),
            status: upstream::ThreadStatus::Active {
                active_flags: Vec::new(),
            },
        },
    });

    let updates = drain_updates(&mut receiver);
    assert!(updates.iter().any(|update| matches!(
        update,
        AppStoreUpdateRecord::ThreadMetadataChanged { state, .. }
            if state.key == key && state.info.status == ThreadSummaryStatus::Active
    )));
    assert!(
        updates
            .iter()
            .all(|update| !matches!(update, AppStoreUpdateRecord::ThreadUpserted { .. }))
    );
}

#[test]
fn duplicate_thread_item_upsert_is_suppressed() {
    let reducer = AppStoreReducer::new();
    let key = ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "thread".to_string(),
    };
    reducer.upsert_thread_snapshot(ThreadSnapshot::from_info("srv", make_thread_info("thread")));
    let mut receiver = reducer.subscribe();
    assert!(drain_updates(&mut receiver).is_empty());

    let item = HydratedConversationItem {
        id: "call-1".to_string(),
        content: HydratedConversationItemContent::CommandExecution(HydratedCommandExecutionData {
            command: "echo hi".to_string(),
            cwd: "/tmp".to_string(),
            output: Some("hi".to_string()),
            exit_code: Some(0),
            status: AppOperationStatus::Completed,
            duration_ms: Some(10),
            process_id: None,
            actions: Vec::new(),
        }),
        source_turn_id: Some("turn-1".to_string()),
        source_turn_index: Some(1),
        timestamp: None,
        is_from_user_turn_boundary: false,
    };

    reducer.emit_thread_item_changed(&key, item.clone());
    reducer.emit_thread_item_changed(&key, item);

    let updates = drain_updates(&mut receiver);
    let upsert_count = updates
        .iter()
        .filter(|update| {
            matches!(
                update,
                AppStoreUpdateRecord::ThreadItemChanged {
                    key: update_key,
                    item,
                    ..
                } if update_key == &key && item.id == "call-1"
            )
        })
        .count();
    assert_eq!(upsert_count, 1);
}

#[test]
fn upsert_thread_snapshot_replaces_existing_thread_with_thread_upserted() {
    let reducer = AppStoreReducer::new();
    let key = ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "thread".to_string(),
    };

    let mut existing = ThreadSnapshot::from_info("srv", make_thread_info("thread"));
    existing.items = vec![
        HydratedConversationItem {
            id: "user-1".to_string(),
            content: HydratedConversationItemContent::User(
                crate::conversation_uniffi::HydratedUserMessageData {
                    text: "hello".to_string(),
                    image_data_uris: Vec::new(),
                },
            ),
            source_turn_id: Some("turn-1".to_string()),
            source_turn_index: Some(1),
            timestamp: None,
            is_from_user_turn_boundary: true,
        },
        HydratedConversationItem {
            id: "assistant-1".to_string(),
            content: HydratedConversationItemContent::Assistant(HydratedAssistantMessageData {
                text: "old".to_string(),
                agent_nickname: None,
                agent_role: None,
                phase: None,
            }),
            source_turn_id: Some("turn-1".to_string()),
            source_turn_index: Some(1),
            timestamp: None,
            is_from_user_turn_boundary: false,
        },
    ];
    reducer.upsert_thread_snapshot(existing);

    let mut incoming = ThreadSnapshot::from_info("srv", make_thread_info("thread"));
    incoming.items = vec![
        HydratedConversationItem {
            id: "user-1".to_string(),
            content: HydratedConversationItemContent::User(
                crate::conversation_uniffi::HydratedUserMessageData {
                    text: "hello".to_string(),
                    image_data_uris: Vec::new(),
                },
            ),
            source_turn_id: Some("turn-1".to_string()),
            source_turn_index: Some(1),
            timestamp: None,
            is_from_user_turn_boundary: true,
        },
        HydratedConversationItem {
            id: "assistant-1".to_string(),
            content: HydratedConversationItemContent::Assistant(HydratedAssistantMessageData {
                text: "new".to_string(),
                agent_nickname: None,
                agent_role: None,
                phase: None,
            }),
            source_turn_id: Some("turn-1".to_string()),
            source_turn_index: Some(1),
            timestamp: None,
            is_from_user_turn_boundary: false,
        },
    ];

    let mut receiver = reducer.subscribe();
    assert!(drain_updates(&mut receiver).is_empty());

    reducer.upsert_thread_snapshot(incoming);

    let updates = drain_updates(&mut receiver);
    assert!(updates.iter().any(|update| matches!(
        update,
        AppStoreUpdateRecord::ThreadUpserted { thread, .. }
            if thread.key == key
    )));
}

#[test]
fn upsert_thread_snapshot_emits_thread_upserted_for_changed_items() {
    let reducer = AppStoreReducer::new();
    let key = ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "thread".to_string(),
    };

    let mut existing = ThreadSnapshot::from_info("srv", make_thread_info("thread"));
    existing.items = vec![HydratedConversationItem {
        id: "assistant-old".to_string(),
        content: HydratedConversationItemContent::Assistant(HydratedAssistantMessageData {
            text: "old".to_string(),
            agent_nickname: None,
            agent_role: None,
            phase: None,
        }),
        source_turn_id: Some("turn-1".to_string()),
        source_turn_index: Some(1),
        timestamp: None,
        is_from_user_turn_boundary: false,
    }];
    reducer.upsert_thread_snapshot(existing);

    let mut incoming = ThreadSnapshot::from_info("srv", make_thread_info("thread"));
    incoming.items = vec![HydratedConversationItem {
        id: "assistant-new".to_string(),
        content: HydratedConversationItemContent::Assistant(HydratedAssistantMessageData {
            text: "new".to_string(),
            agent_nickname: None,
            agent_role: None,
            phase: None,
        }),
        source_turn_id: Some("turn-1".to_string()),
        source_turn_index: Some(1),
        timestamp: None,
        is_from_user_turn_boundary: false,
    }];

    let mut receiver = reducer.subscribe();
    assert!(drain_updates(&mut receiver).is_empty());

    reducer.upsert_thread_snapshot(incoming);

    let updates = drain_updates(&mut receiver);
    assert!(updates.iter().any(|update| matches!(
        update,
        AppStoreUpdateRecord::ThreadUpserted { thread, .. }
            if thread.key == key
    )));
}

#[test]
fn user_turn_boundary_item_consumes_stale_queued_follow_up_preview() {
    let reducer = AppStoreReducer::new();
    let key = ThreadKey {
        server_id: "srv".to_string(),
        thread_id: "thread".to_string(),
    };
    let mut initial = ThreadSnapshot::from_info("srv", make_thread_info("thread"));
    initial.active_turn_id = Some("turn-1".to_string());
    reducer.upsert_thread_snapshot(initial);
    reducer.enqueue_thread_follow_up_preview(
        &key,
        AppQueuedFollowUpPreview {
            id: "queued-1".to_string(),
            kind: crate::store::snapshot::AppQueuedFollowUpKind::Message,
            text: "queued follow-up".to_string(),
        },
    );
    reducer.enqueue_thread_follow_up_preview(
        &key,
        AppQueuedFollowUpPreview {
            id: "queued-2".to_string(),
            kind: crate::store::snapshot::AppQueuedFollowUpKind::Message,
            text: "next follow-up".to_string(),
        },
    );
    reducer.apply_ui_event(&UiEvent::TurnCompleted {
        key: key.clone(),
        turn_id: "turn-1".to_string(),
        error: None,
    });
    assert!(reducer.try_claim_first_queued_follow_up(&key).is_some());

    reducer.apply_item_update(
        &key,
        HydratedConversationItem {
            id: "user-1".to_string(),
            content: HydratedConversationItemContent::User(
                crate::conversation_uniffi::HydratedUserMessageData {
                    text: "queued follow-up".to_string(),
                    image_data_uris: Vec::new(),
                },
            ),
            source_turn_id: Some("turn-2".to_string()),
            source_turn_index: None,
            timestamp: None,
            is_from_user_turn_boundary: true,
        },
    );

    let snapshot = reducer.snapshot();
    let thread = snapshot.threads.get(&key).expect("thread exists");
    assert_eq!(thread.queued_follow_ups.len(), 1);
    assert_eq!(thread.queued_follow_ups[0].id, "queued-2");
    assert_eq!(
        thread.queued_follow_up_drafts[0]
            .causal_anchor_turn_id
            .as_deref(),
        Some("turn-2")
    );
}

fn key_thread(thread_id: &str) -> ThreadKey {
    ThreadKey {
        server_id: "srv".to_string(),
        thread_id: thread_id.to_string(),
    }
}

#[test]
fn remove_server_clears_all_thread_update_caches() {
    let reducer = AppStoreReducer::new();
    let config = make_server_config("srv");
    let key = key_thread("thread-cache-cleanup");
    reducer.upsert_server(&config, ServerHealthSnapshot::Connected);
    reducer.upsert_thread_snapshot(ThreadSnapshot::from_info(
        "srv",
        make_thread_info("thread-cache-cleanup"),
    ));
    reducer.emit_thread_metadata_changed(&key);
    reducer.emit_thread_item_changed(
        &key,
        HydratedConversationItem {
            id: "cached-item".to_string(),
            content: HydratedConversationItemContent::User(HydratedUserMessageData {
                text: "cached".to_string(),
                image_data_uris: Vec::new(),
            }),
            source_turn_id: Some("turn-1".to_string()),
            source_turn_index: None,
            timestamp: None,
            is_from_user_turn_boundary: true,
        },
    );

    assert!(
        reducer
            .last_thread_state_updates
            .read()
            .unwrap()
            .contains_key(&key)
    );
    assert!(
        reducer
            .last_thread_item_upserts
            .read()
            .unwrap()
            .keys()
            .any(|(thread_key, _)| thread_key == &key)
    );
    reducer.remove_server("srv");

    assert!(reducer.last_thread_state_updates.read().unwrap().is_empty());
    assert!(reducer.last_thread_item_upserts.read().unwrap().is_empty());
}

#[test]
fn authoritative_thread_removal_clears_all_thread_update_caches() {
    let reducer = AppStoreReducer::new();
    let config = make_server_config("srv");
    let key = key_thread("thread-list-cleanup");
    reducer.upsert_server(&config, ServerHealthSnapshot::Connected);
    reducer.upsert_thread_snapshot(ThreadSnapshot::from_info(
        "srv",
        make_thread_info("thread-list-cleanup"),
    ));
    reducer.emit_thread_metadata_changed(&key);
    reducer.emit_thread_item_changed(
        &key,
        HydratedConversationItem {
            id: "cached-item".to_string(),
            content: HydratedConversationItemContent::User(HydratedUserMessageData {
                text: "cached".to_string(),
                image_data_uris: Vec::new(),
            }),
            source_turn_id: Some("turn-1".to_string()),
            source_turn_index: None,
            timestamp: None,
            is_from_user_turn_boundary: true,
        },
    );

    reducer.finalize_thread_list_sync("srv", &HashSet::new());

    assert!(!reducer.snapshot().threads.contains_key(&key));
    assert!(reducer.last_thread_state_updates.read().unwrap().is_empty());
    assert!(reducer.last_thread_item_upserts.read().unwrap().is_empty());
}
