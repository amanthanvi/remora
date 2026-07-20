use crate::store::snapshot::{AppTerminalSessionPhase, TerminalSessionSnapshot};
use crate::store::updates::AppStoreUpdateRecord;
use crate::terminal::{
    TerminalBackendKind, TerminalContextCapabilities, TerminalTransportDescriptor,
    ThreadTerminalContext, ThreadTerminalContextError,
};
use crate::types::ThreadKey;

use super::AppStoreReducer;

impl AppStoreReducer {
    /// Insert a new terminal session into the snapshot in
    /// [`AppTerminalSessionPhase::Running`] phase with an empty output
    /// tail. Caller is responsible for placing the live
    /// [`crate::terminal::TerminalSession`] handle on
    /// [`crate::MobileClient::terminal_sessions`].
    pub fn open_terminal_session_record(
        &self,
        id: String,
        backend_kind: TerminalBackendKind,
        cols: u16,
        rows: u16,
    ) {
        let transport = TerminalTransportDescriptor::from_backend(&backend_kind);
        let context = ThreadTerminalContext::unscoped_session(id.clone(), transport);
        self.open_terminal_session_record_with_context(id, context, cols, rows);
    }

    /// Internal context-aware insertion point for the thread-scoped terminal
    /// API. Existing server-wide launches deliberately use an unscoped
    /// context rather than inheriting whichever thread happens to be active.
    pub(crate) fn open_terminal_session_record_with_context(
        &self,
        id: String,
        context: ThreadTerminalContext,
        cols: u16,
        rows: u16,
    ) {
        let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
        snapshot.terminal_sessions.push(TerminalSessionSnapshot {
            id: id.clone(),
            context,
            phase: AppTerminalSessionPhase::Running,
            cols,
            rows,
            last_activity_ts_ms: now_ms(),
            output_tail: Vec::new(),
            exit_code: None,
        });
        // The first opened session becomes active by default; if the
        // caller wants a different one, they call `set_active_terminal_id`
        // after.
        if snapshot.active_terminal_id.is_none() {
            snapshot.active_terminal_id = Some(id);
        }
        drop(snapshot);
        let _ = self
            .updates_tx
            .send(AppStoreUpdateRecord::TerminalSessionsChanged);
    }

    /// Resolve the canonical thread terminal route and fail closed if a
    /// caller tries to attach a session owned by another (or no) thread.
    pub fn resolve_thread_terminal_context(
        &self,
        key: &ThreadKey,
        terminal_session_id: Option<&str>,
    ) -> Result<ThreadTerminalContext, ThreadTerminalContextError> {
        let snapshot = self.snapshot.read().expect("app store lock poisoned");
        let thread =
            snapshot
                .threads
                .get(key)
                .ok_or_else(|| ThreadTerminalContextError::UnknownThread {
                    server_id: key.server_id.clone(),
                    thread_id: key.thread_id.clone(),
                })?;

        let Some(terminal_session_id) = terminal_session_id else {
            let can_open = snapshot.servers.get(&key.server_id).is_some_and(|server| {
                matches!(
                    server.health,
                    crate::store::snapshot::ServerHealthSnapshot::Connected
                )
            });
            return Ok(ThreadTerminalContext {
                thread_key: Some(key.clone()),
                terminal_session_id: None,
                cwd: thread.info.cwd.clone(),
                // The current upstream `ThreadInfo.path` is the rollout path,
                // not an authoritative worktree root. Do not mislabel it.
                worktree_path: None,
                agent_runtime_kind: Some(thread.agent_runtime_kind.clone()),
                transport: None,
                capabilities: TerminalContextCapabilities::for_unopened_thread(can_open),
            });
        };

        let session = snapshot
            .terminal_sessions
            .iter()
            .find(|session| session.id == terminal_session_id)
            .ok_or_else(|| ThreadTerminalContextError::UnknownTerminalSession {
                terminal_session_id: terminal_session_id.to_string(),
            })?;
        if !session.context.belongs_to(key, terminal_session_id) {
            return Err(ThreadTerminalContextError::ContextMismatch);
        }

        let mut context = session.context.clone();
        context.capabilities = if matches!(session.phase, AppTerminalSessionPhase::Running) {
            TerminalContextCapabilities::for_live_session()
        } else {
            TerminalContextCapabilities::unavailable()
        };
        Ok(context)
    }

    /// Append output bytes to the session's ring buffer, capped at
    /// [`TERMINAL_OUTPUT_TAIL_LIMIT`]. Bumps `last_activity_ts_ms`. No-op
    /// if `id` is unknown.
    pub fn append_terminal_output(&self, id: &str, bytes: &[u8]) {
        let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
        let Some(session) = snapshot.terminal_sessions.iter_mut().find(|s| s.id == id) else {
            return;
        };
        push_ring(&mut session.output_tail, bytes, TERMINAL_OUTPUT_TAIL_LIMIT);
        session.last_activity_ts_ms = now_ms();
        // Output bursts are noisy on the broadcast channel; only fire a
        // change update if this is the first byte of a session or if the
        // phase shifted. The renderer subscribes to the live session
        // directly for streaming bytes; broadcast is for non-byte state
        // changes only.
    }

    /// Replace the projected output tail from an authoritative terminal
    /// snapshot. This is used for initial attach and explicit reset recovery;
    /// appending the snapshot would duplicate bytes already represented by the
    /// live stream.
    pub fn replace_terminal_output(&self, id: &str, bytes: &[u8]) {
        let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
        let Some(session) = snapshot.terminal_sessions.iter_mut().find(|s| s.id == id) else {
            return;
        };
        session.output_tail.clear();
        push_ring(&mut session.output_tail, bytes, TERMINAL_OUTPUT_TAIL_LIMIT);
        session.last_activity_ts_ms = now_ms();
    }

    /// Update the session's row/col dimensions after a successful resize.
    pub fn update_terminal_size(&self, id: &str, cols: u16, rows: u16) {
        let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
        if let Some(session) = snapshot.terminal_sessions.iter_mut().find(|s| s.id == id) {
            session.cols = cols;
            session.rows = rows;
            session.last_activity_ts_ms = now_ms();
        }
    }

    /// Mark the session as exited with the given code and clear it from
    /// being active.
    pub fn mark_terminal_exited(&self, id: &str, exit_code: i32) {
        let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
        if let Some(session) = snapshot.terminal_sessions.iter_mut().find(|s| s.id == id) {
            session.phase = AppTerminalSessionPhase::Exited;
            session.exit_code = Some(exit_code);
            session.last_activity_ts_ms = now_ms();
        }
        if snapshot.active_terminal_id.as_deref() == Some(id) {
            snapshot.active_terminal_id = None;
        }
        drop(snapshot);
        let _ = self
            .updates_tx
            .send(AppStoreUpdateRecord::TerminalSessionsChanged);
    }

    /// Remove a session's snapshot entirely. Used after the caller has
    /// dropped the live session handle and no longer needs the buffered
    /// output (e.g. an explicit "close session and forget" path).
    pub fn remove_terminal_session_record(&self, id: &str) {
        let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
        snapshot.terminal_sessions.retain(|s| s.id != id);
        if snapshot.active_terminal_id.as_deref() == Some(id) {
            snapshot.active_terminal_id = snapshot.terminal_sessions.last().map(|s| s.id.clone());
        }
        drop(snapshot);
        let _ = self
            .updates_tx
            .send(AppStoreUpdateRecord::TerminalSessionsChanged);
    }

    /// Set the currently-focused terminal session id. The id must match
    /// an existing snapshot entry, otherwise active is cleared.
    pub fn set_active_terminal_id(&self, id: Option<String>) {
        let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
        let next = id.filter(|candidate| {
            snapshot
                .terminal_sessions
                .iter()
                .any(|s| &s.id == candidate)
        });
        if snapshot.active_terminal_id == next {
            return;
        }
        snapshot.active_terminal_id = next;
        drop(snapshot);
        let _ = self
            .updates_tx
            .send(AppStoreUpdateRecord::TerminalSessionsChanged);
    }

    /// Take a snapshot of a single terminal session. Cheap clone of the
    /// stored entry; safe to call on hot paths.
    pub fn terminal_session_snapshot(&self, id: &str) -> Option<TerminalSessionSnapshot> {
        let snapshot = self.snapshot.read().expect("app store lock poisoned");
        snapshot
            .terminal_sessions
            .iter()
            .find(|s| s.id == id)
            .cloned()
    }
}

/// Maximum bytes of output we keep per session for re-attach repaint.
/// 64 KiB matches the size budgeted in the plan; on a 100x40 grid that's
/// roughly 16 full screenfuls — enough to cover quick navigations and
/// app backgrounding without permanently retaining megabytes of logs.
pub const TERMINAL_OUTPUT_TAIL_LIMIT: usize = 64 * 1024;

/// Append `bytes` to `buf`, capping the result at `limit` by dropping
/// from the front. Designed for the per-session output tail.
fn push_ring(buf: &mut Vec<u8>, bytes: &[u8], limit: usize) {
    if bytes.is_empty() {
        return;
    }
    // Fast path: incoming chunk is itself larger than the limit; keep
    // only its tail and replace the buffer.
    if bytes.len() >= limit {
        let start = bytes.len() - limit;
        buf.clear();
        buf.extend_from_slice(&bytes[start..]);
        return;
    }
    // Compact only if needed.
    let combined = buf.len() + bytes.len();
    if combined > limit {
        let drop_n = combined - limit;
        buf.drain(..drop_n);
    }
    buf.extend_from_slice(bytes);
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::AppStoreReducer;
    use crate::terminal::{TerminalTransportDescriptor, TerminalTransportKind};
    use crate::types::{ThreadInfo, ThreadSummaryStatus};

    fn thread_info(id: &str) -> ThreadInfo {
        ThreadInfo {
            id: id.to_string(),
            title: None,
            model: None,
            status: ThreadSummaryStatus::Idle,
            preview: None,
            cwd: Some("/workspace/project".to_string()),
            path: Some("/rollouts/thread.jsonl".to_string()),
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

    #[test]
    fn terminal_snapshot_never_retains_pairing_routing_ids() {
        let reducer = AppStoreReducer::new();
        reducer.open_terminal_session_record(
            "terminal".to_string(),
            TerminalBackendKind::RemoteRemoraLink {
                host_id: "remora-link:sensitive-node".to_string(),
                shell: None,
            },
            80,
            24,
        );

        let stored = reducer.terminal_session_snapshot("terminal").unwrap();
        let boundary_debug = format!("{stored:?}");
        assert_eq!(stored.context.thread_key, None);
        assert_eq!(
            stored.context.transport.unwrap().kind,
            TerminalTransportKind::RemoraLink
        );
        assert!(!boundary_debug.contains("sensitive-node"));
    }

    #[test]
    fn v2_terminal_snapshot_never_retains_host_id_or_shell_choice() {
        let reducer = AppStoreReducer::new();
        reducer.open_terminal_session_record(
            "terminal-v2".to_string(),
            TerminalBackendKind::RemoteRemoraLink {
                host_id: "remora-link:sensitive-endpoint-id".to_string(),
                shell: Some("/private/bin/zsh".to_string()),
            },
            80,
            24,
        );

        let stored = reducer.terminal_session_snapshot("terminal-v2").unwrap();
        let boundary_debug = format!("{stored:?}");
        assert_eq!(
            stored.context.transport.unwrap().kind,
            TerminalTransportKind::RemoraLink
        );
        assert!(!boundary_debug.contains("sensitive-endpoint-id"));
        assert!(!boundary_debug.contains("/private/bin/zsh"));
    }

    #[test]
    fn unscoped_or_foreign_terminal_cannot_attach_to_thread() {
        let reducer = AppStoreReducer::new();
        reducer.sync_thread_list("server", &[thread_info("thread-a")]);
        reducer.open_terminal_session_record(
            "unscoped".to_string(),
            TerminalBackendKind::RemoteRemoraLink {
                host_id: "remora-link:node".to_string(),
                shell: None,
            },
            80,
            24,
        );
        let key = ThreadKey {
            server_id: "server".to_string(),
            thread_id: "thread-a".to_string(),
        };

        assert!(matches!(
            reducer.resolve_thread_terminal_context(&key, Some("unscoped")),
            Err(ThreadTerminalContextError::ContextMismatch)
        ));
        assert!(matches!(
            reducer.resolve_thread_terminal_context(&key, Some("missing")),
            Err(ThreadTerminalContextError::UnknownTerminalSession { .. })
        ));

        let unopened = reducer.resolve_thread_terminal_context(&key, None).unwrap();
        assert_eq!(unopened.thread_key, Some(key));
        assert_eq!(unopened.cwd.as_deref(), Some("/workspace/project"));
        assert_eq!(unopened.worktree_path, None);
        assert!(!unopened.capabilities.can_attach);
    }

    #[test]
    fn scoped_terminal_requires_exact_thread_key_and_session_id() {
        let reducer = AppStoreReducer::new();
        reducer.sync_thread_list(
            "server",
            &[thread_info("thread-a"), thread_info("thread-b")],
        );
        let key = ThreadKey {
            server_id: "server".to_string(),
            thread_id: "thread-a".to_string(),
        };
        reducer.open_terminal_session_record_with_context(
            "terminal".to_string(),
            ThreadTerminalContext {
                thread_key: Some(key.clone()),
                terminal_session_id: Some("terminal".to_string()),
                cwd: Some("/workspace/project".to_string()),
                worktree_path: None,
                agent_runtime_kind: Some("codex".to_string()),
                transport: Some(TerminalTransportDescriptor {
                    kind: TerminalTransportKind::RemoraLink,
                    display_label: "Remora Link".to_string(),
                }),
                capabilities: TerminalContextCapabilities::for_live_session(),
            },
            80,
            24,
        );

        let context = reducer
            .resolve_thread_terminal_context(&key, Some("terminal"))
            .unwrap();
        assert!(context.capabilities.can_write);

        let foreign = ThreadKey {
            server_id: "server".to_string(),
            thread_id: "thread-b".to_string(),
        };
        assert!(matches!(
            reducer.resolve_thread_terminal_context(&foreign, Some("terminal")),
            Err(ThreadTerminalContextError::ContextMismatch)
        ));
    }

    #[test]
    fn authoritative_output_snapshot_replaces_instead_of_duplicating_tail() {
        let reducer = AppStoreReducer::new();
        reducer.open_terminal_session_record(
            "terminal".to_string(),
            TerminalBackendKind::RemoteRemoraLink {
                host_id: "remora-link:node".to_string(),
                shell: None,
            },
            80,
            24,
        );

        reducer.append_terminal_output("terminal", b"old prefix");
        reducer.replace_terminal_output("terminal", b"authoritative");
        reducer.append_terminal_output("terminal", b" tail");

        assert_eq!(
            reducer
                .terminal_session_snapshot("terminal")
                .unwrap()
                .output_tail,
            b"authoritative tail"
        );
    }
}
