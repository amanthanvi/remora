# 005: Delayed post-input reconciliation

Status: implemented. Original priority P1; shared mobile runtime fix.

## Change

`spawn_post_user_input_reconcile` retains a weak MobileClient owner and the
original thread-history identity. Each runtime-specific read captures the event
generation. Its result is applied only while the session, event generation, and
history identity still match, under the existing store guards. Responses for
another thread are rejected.

Lock order is sessions, event generation, history commit, snapshot, then epoch
map. Removal and rollback acquire the history barrier before mutation. There is
no unguarded late upsert. Polling stops on completed turns or owner loss and
retains the existing three-attempt schedule.

## Evidence

- `delayed_post_input_reads_cannot_replace_newer_state`: clean response, newer
  event, thread/server removal, recreation, rollback, and session replacement
- `post_input_worker_stops_after_completion_or_owner_drop_and_bounds_errors`:
  paused-clock verification of completion, owner loss, and bounded failed reads

Source: `shared/rust-bridge/codex-mobile-client/src/mobile_client/user_input.rs`.
The guard implementation remains in `store/reducer.rs`. No new FFI or retry
policy was added. Aggregate validation is in
[the review](../docs/reviews/2026-09-07-upstream-adoption.md).
