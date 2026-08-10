# Plan 011: Clean IPC request and task lifecycle

> **Executor instructions**: Follow this plan exactly in a fresh isolated
> worktree. Run every gate, touch only the two scoped files, and stop on every
> STOP condition. Commit only after validation. Do not update `plans/README.md`,
> merge, push, or open a pull request.
>
> **Drift check (run first)**:
> `git diff --stat 7ef63264997200c79674b9705241cc05f1fad894..HEAD -- shared/rust-bridge/codex-ipc/src/client/handle.rs shared/rust-bridge/codex-ipc/src/client/connection.rs`
> Expected: no output. Any output is a STOP condition.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: MEDIUM
- **Depends on**: Plan 012
- **Category**: bug, Rust, IPC, lifecycle, performance
- **Tracker**: <https://github.com/amanthanvi/remora/issues/21>
- **Planned at**: reviewed Plan 010 commit
  `fd09fb8db97ce5a28b883752c139392c4c696a3a`, 2026-08-10
- **Retry base**: reviewed Plan 012 commit
  `7ef63264997200c79674b9705241cc05f1fad894`, 2026-08-10
- **Execution status**: DONE at
  `ea115b3e785d9c4b26779238791559b035e1d905` after Plan 012 restored the
  existing `codex-ipc` test baseline.

## Why this matters

Handshake and normal IPC requests enter `PendingRequests` before a fallible
channel send and a timeout/cancellation point. Send failure, timeout, or future
cancellation leaves the registration behind until an unrelated response or
connection-wide clear occurs. Separately, `IpcConnection` owns the reader and
writer `JoinHandle`s but has no `Drop`; dropping a Tokio `JoinHandle` detaches
its task. A failed connection attempt or final-client drop can therefore retain
pending entries, stream halves, and background tasks.

The invariant is narrow: every registered request leaves the map on response,
error, timeout, or future cancellation, and dropping the final connection owner
aborts both I/O tasks and clears pending requests.

## Scope

**In scope**:

- `shared/rust-bridge/codex-ipc/src/client/handle.rs`
- `shared/rust-bridge/codex-ipc/src/client/connection.rs`

**Out of scope**:

- Router, reconnect, framing, wire protocol, request/response types, or timeout
  policy changes.
- Graceful frame flushing on implicit drop. Existing explicit shutdown is
  abort-and-clear, and implicit drop must match that resource-safety policy.
- Tracking or aborting independently spawned inbound request-handler tasks.
- `PendingRequests` API changes, dependencies, Cargo manifests/lockfiles,
  UniFFI, generated bindings, Swift, Kotlin, or documentation.

## Git workflow

- Create a fresh isolated branch/worktree from
  `7ef63264997200c79674b9705241cc05f1fad894`.
- Commit subject: `ipc: clean request and task lifecycle`.
- One two-file commit only. No merge, push, or pull request.

## Steps

### Step 1: Prove the exact base and bootstrap supported Rust inputs

```sh
(
  set -euo pipefail
  test "$(git rev-parse HEAD)" = '7ef63264997200c79674b9705241cc05f1fad894'
  test -z "$(git status --short --ignore-submodules=all --untracked-files=all)"
  make patch
  git -C shared/third_party/codex diff --binary | shasum -a 256 \
    > "$(git rev-parse --git-path remora-plan011-submodule.sha256)"
  cargo test --locked \
    --manifest-path shared/rust-bridge/Cargo.toml \
    -p codex-ipc client::handle::tests -- --nocapture
  cargo test --locked \
    --manifest-path shared/rust-bridge/Cargo.toml \
    -p codex-ipc client::pending::tests -- --nocapture
)
```

The first filtered command may report zero tests because `handle.rs` has no
test module yet; it must still compile successfully. The existing pending-map
tests must pass. A bootstrap, compile, or existing-test failure is a STOP.

### Step 2: Add four focused failing lifecycle tests

Add a private `#[cfg(test)] mod tests` to `handle.rs`. Reuse
`tokio::io::duplex`, the production frame codec, and real initialization
envelopes. Do not add production-only test hooks.

Build one small test helper that completes a valid initialize handshake and
returns the connected client plus the peer half. Use bounded timeouts of at
most one second for EOF/cancellation observations; keep the client request
timeout at 50 milliseconds or less.

Cover exactly these behaviors:

1. **Request timeout cleanup**: the peer reads but does not answer a normal
   request. After `send_request` returns `RequestError::Timeout`, resolving its
   captured request ID through the client's pending tracker returns `false`.
2. **Caller cancellation cleanup**: spawn a normal `send_request`, let the peer
   capture its request ID, abort and await the task, then assert pending
   resolution for that ID returns `false`.
3. **Failed initialization closes the stream**: let initialize time out, assert
   `connect_with_stream` fails, and assert the peer observes EOF within the
   bounded timeout. The peer must concurrently read and validate the buffered
   initialize request, deliberately withhold its response, await the client
   failure, and only then perform a second bounded frame read that returns
   `TransportError::ConnectionClosed`.
4. **Final client drop closes the stream**: complete initialization, drop the
   sole `IpcClient`, and assert the peer observes EOF within the bounded
   timeout.

Use opaque request IDs and payloads. Do not log frames, credentials, paths, or
payload contents. Assert error variants, cancellation completion, pending-map
membership, and EOF only.

Capture red before production changes:

```sh
(
  set -euo pipefail
  HANDLE='shared/rust-bridge/codex-ipc/src/client/handle.rs'
  for test_name in \
    request_timeout_removes_pending \
    cancelled_request_removes_pending \
    failed_initialize_closes_peer \
    final_client_drop_closes_peer; do
    rg -q "async fn $test_name" "$HANDLE"
  done
  set +e
  PLAN_RED_OUTPUT="$(
    cargo test --locked \
      --manifest-path shared/rust-bridge/Cargo.toml \
      -p codex-ipc client::handle::tests -- --nocapture 2>&1
  )"
  PLAN_RED_STATUS=$?
  set -e
  test "$PLAN_RED_STATUS" -ne 0
  if ! rg -q 'assertion failed|Elapsed|deadline has elapsed|timed out' <<<"$PLAN_RED_OUTPUT"; then
    printf '%s\n' "$PLAN_RED_OUTPUT" >&2
    exit 1
  fi
  if rg -q 'error\[E[0-9]+\]|could not compile' <<<"$PLAN_RED_OUTPUT"; then
    printf '%s\n' "$PLAN_RED_OUTPUT" >&2
    exit 1
  fi
)
```

Expected red: one or more pending-resolution assertions are still `true`, or a
peer-EOF observation reaches its bounded timeout. Compile errors, harness
errors, or unrelated failures are STOP conditions.

### Step 3: Make pending registration cancellation-safe

In `handle.rs`, add one private RAII registration guard. It owns:

- an `Arc<PendingRequests>`; and
- the registered request ID.

Construction registers the ID and returns the guard plus its oneshot receiver.
`Drop` calls the existing idempotent `PendingRequests::remove`. Do not add a
public API or change `pending.rs`.

Add one private async helper that:

1. creates the guard/receiver;
2. sends the already-built request envelope;
3. awaits the receiver under the supplied timeout; and
4. returns the typed `Response` or the existing `IpcError` mapping.

Use it for both initialization and `send_request`. Keep request construction,
IDs, source/target fields, serialization, timeout values, response parsing,
and error mapping unchanged. The guard must remain live across the send and
response await; normal response resolution removes the entry first, making
guard drop a no-op.

### Step 4: Make connection ownership clean up on drop

In `connection.rs`, extract the current idempotent synchronous resource cleanup
into one private method that:

- aborts `read_task`;
- aborts `write_task`; and
- clears `pending`.

Call it from `shutdown`, `shutdown_ref`, and `Drop for IpcConnection`. Preserve
the existing async public signatures and abort semantics. Do not await task
joins, flush frames, close inbound handler tasks, or introduce flags/locks.

### Step 5: Validate focused behavior and the complete crate

```sh
(
  set -euo pipefail
  cargo test --locked \
    --manifest-path shared/rust-bridge/Cargo.toml \
    -p codex-ipc client::handle::tests -- --nocapture
  cargo test --locked \
    --manifest-path shared/rust-bridge/Cargo.toml \
    -p codex-ipc
  cargo check --locked \
    --manifest-path shared/rust-bridge/Cargo.toml \
    -p codex-ipc --all-targets
  cargo fmt \
    --manifest-path shared/rust-bridge/Cargo.toml \
    -p codex-ipc -- --check
)
```

Run scope and architecture gates from repository root:

```sh
(
  set -euo pipefail
  HANDLE='shared/rust-bridge/codex-ipc/src/client/handle.rs'
  CONNECTION='shared/rust-bridge/codex-ipc/src/client/connection.rs'

  test "$(rg -c 'struct PendingRequestGuard' "$HANDLE")" -eq 1
  test "$(rg -c 'impl Drop for PendingRequestGuard' "$HANDLE")" -eq 1
  test "$(rg -c 'impl Drop for IpcConnection' "$CONNECTION")" -eq 1
  for test_name in \
    request_timeout_removes_pending \
    cancelled_request_removes_pending \
    failed_initialize_closes_peer \
    final_client_drop_closes_peer; do
    test "$(rg -c "async fn $test_name" "$HANDLE")" -eq 1
  done
  git diff --check

  PLAN_EXPECTED_FILES="$(printf '%s\n' \
    shared/rust-bridge/codex-ipc/src/client/connection.rs \
    shared/rust-bridge/codex-ipc/src/client/handle.rs | LC_ALL=C sort)"
  PLAN_CHANGED_FILES="$(git status --short --ignore-submodules=all --untracked-files=all | cut -c4- | LC_ALL=C sort)"
  test "$PLAN_CHANGED_FILES" = "$PLAN_EXPECTED_FILES"

  test -z "$(git diff --name-only -- \
    shared/rust-bridge/codex-ipc/src/client/pending.rs \
    shared/rust-bridge/Cargo.toml \
    shared/rust-bridge/Cargo.lock)"
  git -C shared/third_party/codex diff --binary | shasum -a 256 | \
    diff -u "$(git rev-parse --git-path remora-plan011-submodule.sha256)" -
)
```

Inspect the full unstaged diff, stage only the two scoped files, inspect the full
cached diff, and commit. Verify:

```sh
(
  set -euo pipefail
  PLAN_EXPECTED_FILES="$(printf '%s\n' \
    shared/rust-bridge/codex-ipc/src/client/connection.rs \
    shared/rust-bridge/codex-ipc/src/client/handle.rs | LC_ALL=C sort)"
  test "$(git log -1 --format=%s)" = 'ipc: clean request and task lifecycle'
  test "$(git rev-parse HEAD^)" = '7ef63264997200c79674b9705241cc05f1fad894'
  PLAN_COMMIT_FILES="$(git diff-tree --no-commit-id --name-only -r HEAD | LC_ALL=C sort)"
  test "$PLAN_COMMIT_FILES" = "$PLAN_EXPECTED_FILES"
  test -z "$(git status --short --ignore-submodules=all --untracked-files=all)"
  git diff --check HEAD^
)
```

## Done criteria

- [x] Timeout, send failure, and caller cancellation cannot retain a pending
      request registration.
- [x] Normal responses keep their existing behavior and error mapping.
- [x] Failed initialization and final-client drop release both stream halves.
- [x] Explicit shutdown and implicit drop share one idempotent cleanup path.
- [x] No protocol, reconnect, inbound-handler, dependency, binding, or native
      scope change.
- [x] Focused tests, full crate tests, all-targets check, formatting, exact
      scope, subject, parent, whitespace, and clean gates pass.

## Execution record

- Worktree: `/tmp/remora-plan011.lkp5Lu/worktree`; branch:
  `executor/011-clean-ipc-request-and-task-lifecycle`; exact base:
  `fd09fb8db97ce5a28b883752c139392c4c696a3a`.
- Exact base, clean, and drift checks passed. `make patch` succeeded.
- The first mandatory focused-test preflight failed before any Plan 011 edit:
  existing tests in `codex-ipc/src/bridge.rs` and
  `codex-ipc/src/conversation_state.rs` have drifted from current upstream
  `AbsolutePathBuf`, `Thread`, `Turn`, and `ThreadStatus` definitions.
- The executor stopped at Step 1. The second preflight command was not run; no
  tracked file changed; no commit, merge, push, pull request, retry, or
  validation bypass occurred.
- Resume only from a fresh worktree after a separate reviewed plan restores the
  existing `codex-ipc` test baseline. Do not fold that repair into this plan's
  two-file lifecycle scope.
- Plan 012 restored that baseline at
  `7ef63264997200c79674b9705241cc05f1fad894`. The retry must use a fresh
  worktree at that exact commit; the stopped Plan 011 worktree remains
  historical evidence only.
- Approved retry worktree: `/tmp/remora-plan011-retry.Hp8iZn/worktree`;
  branch: `executor/011-clean-ipc-request-and-task-lifecycle-retry`; commit:
  `ea115b3e785d9c4b26779238791559b035e1d905`; exact parent:
  `7ef63264997200c79674b9705241cc05f1fad894`.
- The fresh retry passed the known-good preflight, captured behavioral red for
  all four lifecycle tests, and implemented one private pending-registration
  guard, one shared request await helper, and one idempotent connection cleanup
  path used by explicit shutdown and `Drop`.
- Final validation passed 4/4 focused lifecycle tests, 82/82 library tests plus
  binary/doc targets, all-targets check, package formatting, exact structural
  counts/test names, two-file scope, submodule hash fence, whitespace, subject,
  parent, and clean gates. The first validation attempt found only two scoped
  rustfmt wraps; the second full matrix passed after correcting them.
- Independent review confirmed send-error/timeout/receiver-error/future-
  cancellation cleanup, mutex-safe response races, unchanged request/error
  semantics, final-`Arc` connection cleanup, and deterministic event-ordered
  duplex tests. No merge, push, or pull request was performed.
- Follow-up commit `a311747` generalized the test handshake helper over
  `AsyncRead + AsyncWrite` and added a real Unix-domain peer EOF regression.
  The complete crate then passed 83/83 tests, all-targets check, and formatting.

## STOP conditions

Stop and report without improvising if:

- Exact base, drift, bootstrap, clean, compile, or existing-test preflight
  fails.
- Red evidence is a compile/harness/unrelated failure rather than leaked
  pending state or missing peer EOF.
- Cleanup requires router/reconnect/wire semantics, graceful flush, task join
  awaiting, inbound-handler tracking, a new dependency, or a public API.
- A deterministic peer EOF cannot be observed with `tokio::io::duplex` and a
  one-second bound after the prescribed abort/drop behavior.
- Any file outside the two-file scope changes after ignored generated/submodule
  state is excluded.
- A focused, crate-wide, check, formatting, static, or post-commit gate fails
  twice after correcting only an in-scope implementation defect or a command/
  environment typo. Do not expand scope or bypass validation.

## Rollback

Revert the single executor commit. No schema, migration, generated artifact,
dependency, protocol, or persisted-state rollback is required.
