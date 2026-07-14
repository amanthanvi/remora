use crate::store::snapshot::{AppTerminalSessionPhase, TerminalSessionSnapshot};
use crate::store::updates::AppStoreUpdateRecord;
use crate::terminal::TerminalBackendKind;

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
        let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
        snapshot.terminal_sessions.push(TerminalSessionSnapshot {
            id: id.clone(),
            backend_kind,
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
