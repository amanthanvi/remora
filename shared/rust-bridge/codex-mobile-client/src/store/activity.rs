use crate::store::snapshot::{AppSnapshot, ServerHealthSnapshot, ThreadSnapshot};
use crate::types::{AppSubagentStatus, ThreadSummaryStatus};

/// One canonical, cross-platform projection of work that needs attention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum AgentActivityPhase {
    Starting,
    Running,
    WaitingForApproval,
    WaitingForInput,
    Completed,
    Failed,
    Stale,
}

pub(crate) fn project_agent_activity_phase(
    snapshot: &AppSnapshot,
    thread: &ThreadSnapshot,
) -> Option<AgentActivityPhase> {
    let key = &thread.key;

    // User-blocking states always win, and must be scoped by both server and
    // thread because upstream thread ids are not globally unique.
    if snapshot.pending_approvals.iter().any(|approval| {
        approval.server_id == key.server_id
            && approval.thread_id.as_deref() == Some(key.thread_id.as_str())
    }) {
        return Some(AgentActivityPhase::WaitingForApproval);
    }
    if snapshot
        .pending_user_inputs
        .iter()
        .any(|request| request.server_id == key.server_id && request.thread_id == key.thread_id)
    {
        return Some(AgentActivityPhase::WaitingForInput);
    }

    let subagent_status = thread
        .info
        .agent_status
        .as_deref()
        .map(AppSubagentStatus::from_raw)
        .unwrap_or(AppSubagentStatus::Unknown);

    if matches!(thread.info.status, ThreadSummaryStatus::SystemError)
        || matches!(
            subagent_status,
            AppSubagentStatus::Errored | AppSubagentStatus::Interrupted
        )
    {
        return Some(AgentActivityPhase::Failed);
    }

    if matches!(thread.info.status, ThreadSummaryStatus::NotLoaded)
        || matches!(subagent_status, AppSubagentStatus::PendingInit)
    {
        return Some(AgentActivityPhase::Starting);
    }

    let has_active_work = thread.active_turn_id.is_some()
        || matches!(thread.info.status, ThreadSummaryStatus::Active)
        || matches!(subagent_status, AppSubagentStatus::Running);

    if has_active_work {
        let transport_is_live = snapshot
            .servers
            .get(&key.server_id)
            .is_some_and(|server| matches!(server.health, ServerHealthSnapshot::Connected));
        if !transport_is_live {
            return Some(AgentActivityPhase::Stale);
        }
        return Some(AgentActivityPhase::Running);
    }

    let has_completion_evidence = matches!(
        subagent_status,
        AppSubagentStatus::Completed | AppSubagentStatus::Shutdown
    ) || (matches!(thread.info.status, ThreadSummaryStatus::Idle)
        && thread
            .items
            .iter()
            .any(|item| item.source_turn_id.is_some()));

    has_completion_evidence.then_some(AgentActivityPhase::Completed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::snapshot::ServerSnapshot;
    use crate::types::{ApprovalKind, PendingApproval, PendingUserInputRequest, ThreadInfo};

    fn thread(server_id: &str, thread_id: &str) -> ThreadSnapshot {
        ThreadSnapshot::from_info(
            server_id,
            ThreadInfo {
                id: thread_id.to_string(),
                title: None,
                model: None,
                status: ThreadSummaryStatus::Idle,
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
            },
        )
    }

    fn connected_server(server_id: &str) -> ServerSnapshot {
        ServerSnapshot {
            server_id: server_id.to_string(),
            display_name: server_id.to_string(),
            host: "127.0.0.1".to_string(),
            port: 1,
            wake_mac: None,
            is_local: false,
            health: ServerHealthSnapshot::Connected,
            account: None,
            requires_openai_auth: false,
            rate_limits: None,
            rate_limits_by_runtime: Default::default(),
            available_models: None,
            agent_runtimes: Vec::new(),
            connection_progress: None,
            transport: Default::default(),
            codex_version: None,
            supports_turn_pagination: false,
        }
    }

    #[test]
    fn user_blocking_precedence_is_scoped_by_server_and_thread() {
        let mut snapshot = AppSnapshot::default();
        snapshot
            .servers
            .insert("server-a".to_string(), connected_server("server-a"));
        snapshot
            .servers
            .insert("server-b".to_string(), connected_server("server-b"));

        let mut target = thread("server-a", "same-id");
        target.info.status = ThreadSummaryStatus::SystemError;
        snapshot.pending_approvals.push(PendingApproval {
            runtime_kind: "codex".to_string(),
            id: "approval".to_string(),
            server_id: "server-a".to_string(),
            kind: ApprovalKind::Command,
            thread_id: Some("same-id".to_string()),
            turn_id: None,
            item_id: None,
            command: None,
            path: None,
            grant_root: None,
            cwd: None,
            reason: None,
        });
        snapshot.pending_user_inputs.push(PendingUserInputRequest {
            runtime_kind: "codex".to_string(),
            id: "input".to_string(),
            server_id: "server-a".to_string(),
            thread_id: "same-id".to_string(),
            turn_id: "turn".to_string(),
            item_id: "item".to_string(),
            questions: Vec::new(),
            requester_agent_nickname: None,
            requester_agent_role: None,
        });

        assert_eq!(
            project_agent_activity_phase(&snapshot, &target),
            Some(AgentActivityPhase::WaitingForApproval)
        );

        let duplicate_on_other_server = thread("server-b", "same-id");
        assert_eq!(
            project_agent_activity_phase(&snapshot, &duplicate_on_other_server),
            None
        );
    }

    #[test]
    fn phase_projection_covers_failure_start_run_stale_and_idle() {
        let mut snapshot = AppSnapshot::default();
        snapshot
            .servers
            .insert("server".to_string(), connected_server("server"));

        let never_run = thread("server", "idle");
        assert_eq!(project_agent_activity_phase(&snapshot, &never_run), None);

        let mut failed = thread("server", "failed");
        failed.info.agent_status = Some("interrupted".to_string());
        assert_eq!(
            project_agent_activity_phase(&snapshot, &failed),
            Some(AgentActivityPhase::Failed)
        );

        let mut starting = thread("server", "starting");
        starting.info.agent_status = Some("pendingInit".to_string());
        assert_eq!(
            project_agent_activity_phase(&snapshot, &starting),
            Some(AgentActivityPhase::Starting)
        );

        let mut running = thread("server", "running");
        running.active_turn_id = Some("turn".to_string());
        assert_eq!(
            project_agent_activity_phase(&snapshot, &running),
            Some(AgentActivityPhase::Running)
        );

        snapshot.servers.get_mut("server").unwrap().health = ServerHealthSnapshot::Unresponsive;
        assert_eq!(
            project_agent_activity_phase(&snapshot, &running),
            Some(AgentActivityPhase::Stale)
        );
    }

    #[test]
    fn completed_requires_evidence_and_failure_wins_interrupted_race() {
        let mut snapshot = AppSnapshot::default();
        snapshot
            .servers
            .insert("server".to_string(), connected_server("server"));

        let mut completed = thread("server", "completed");
        completed.info.agent_status = Some("completed".to_string());
        assert_eq!(
            project_agent_activity_phase(&snapshot, &completed),
            Some(AgentActivityPhase::Completed)
        );

        completed.info.agent_status = Some("interrupted".to_string());
        assert_eq!(
            project_agent_activity_phase(&snapshot, &completed),
            Some(AgentActivityPhase::Failed)
        );
    }

    #[test]
    fn waiting_for_input_wins_failure_when_no_approval_exists() {
        let mut snapshot = AppSnapshot::default();
        let mut target = thread("server", "thread");
        target.info.status = ThreadSummaryStatus::SystemError;
        snapshot.pending_user_inputs.push(PendingUserInputRequest {
            runtime_kind: "codex".to_string(),
            id: "input".to_string(),
            server_id: "server".to_string(),
            thread_id: "thread".to_string(),
            turn_id: "turn".to_string(),
            item_id: "item".to_string(),
            questions: Vec::new(),
            requester_agent_nickname: None,
            requester_agent_role: None,
        });

        assert_eq!(
            project_agent_activity_phase(&snapshot, &target),
            Some(AgentActivityPhase::WaitingForInput)
        );
    }

    #[test]
    fn precedence_chain_is_table_driven() {
        struct Case {
            approval: bool,
            input: bool,
            summary_status: ThreadSummaryStatus,
            subagent_status: Option<&'static str>,
            active_turn: bool,
            connected: bool,
            expected: Option<AgentActivityPhase>,
        }

        let cases = [
            Case {
                approval: true,
                input: true,
                summary_status: ThreadSummaryStatus::SystemError,
                subagent_status: Some("pendingInit"),
                active_turn: true,
                connected: false,
                expected: Some(AgentActivityPhase::WaitingForApproval),
            },
            Case {
                approval: false,
                input: true,
                summary_status: ThreadSummaryStatus::SystemError,
                subagent_status: Some("pendingInit"),
                active_turn: true,
                connected: false,
                expected: Some(AgentActivityPhase::WaitingForInput),
            },
            Case {
                approval: false,
                input: false,
                summary_status: ThreadSummaryStatus::SystemError,
                subagent_status: Some("pendingInit"),
                active_turn: true,
                connected: false,
                expected: Some(AgentActivityPhase::Failed),
            },
            Case {
                approval: false,
                input: false,
                summary_status: ThreadSummaryStatus::NotLoaded,
                subagent_status: Some("running"),
                active_turn: true,
                connected: false,
                expected: Some(AgentActivityPhase::Starting),
            },
            Case {
                approval: false,
                input: false,
                summary_status: ThreadSummaryStatus::Active,
                subagent_status: Some("completed"),
                active_turn: true,
                connected: false,
                expected: Some(AgentActivityPhase::Stale),
            },
            Case {
                approval: false,
                input: false,
                summary_status: ThreadSummaryStatus::Active,
                subagent_status: Some("completed"),
                active_turn: true,
                connected: true,
                expected: Some(AgentActivityPhase::Running),
            },
            Case {
                approval: false,
                input: false,
                summary_status: ThreadSummaryStatus::Idle,
                subagent_status: Some("completed"),
                active_turn: false,
                connected: true,
                expected: Some(AgentActivityPhase::Completed),
            },
            Case {
                approval: false,
                input: false,
                summary_status: ThreadSummaryStatus::Idle,
                subagent_status: None,
                active_turn: false,
                connected: true,
                expected: None,
            },
        ];

        for (index, case) in cases.into_iter().enumerate() {
            let mut snapshot = AppSnapshot::default();
            let mut server = connected_server("server");
            if !case.connected {
                server.health = ServerHealthSnapshot::Disconnected;
            }
            snapshot.servers.insert("server".to_string(), server);

            let mut target = thread("server", "thread");
            target.info.status = case.summary_status;
            target.info.agent_status = case.subagent_status.map(str::to_string);
            target.active_turn_id = case.active_turn.then(|| "turn".to_string());

            if case.approval {
                snapshot.pending_approvals.push(PendingApproval {
                    runtime_kind: "codex".to_string(),
                    id: "approval".to_string(),
                    server_id: "server".to_string(),
                    kind: ApprovalKind::Command,
                    thread_id: Some("thread".to_string()),
                    turn_id: None,
                    item_id: None,
                    command: None,
                    path: None,
                    grant_root: None,
                    cwd: None,
                    reason: None,
                });
            }
            if case.input {
                snapshot.pending_user_inputs.push(PendingUserInputRequest {
                    runtime_kind: "codex".to_string(),
                    id: "input".to_string(),
                    server_id: "server".to_string(),
                    thread_id: "thread".to_string(),
                    turn_id: "turn".to_string(),
                    item_id: "item".to_string(),
                    questions: Vec::new(),
                    requester_agent_nickname: None,
                    requester_agent_role: None,
                });
            }

            assert_eq!(
                project_agent_activity_phase(&snapshot, &target),
                case.expected,
                "precedence case {index}"
            );
        }
    }
}
