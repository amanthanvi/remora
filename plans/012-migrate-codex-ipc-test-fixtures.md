# Plan 012: Migrate stale codex-ipc test fixtures

> **Executor instructions**: Follow this plan exactly in a fresh isolated
> worktree. Run every gate, touch only the two scoped test modules, and stop on
> every STOP condition. Commit only after validation. Do not update
> `plans/README.md`, merge, push, or open a pull request.
>
> **Drift check (run first)**:
> `git diff --stat fd09fb8db97ce5a28b883752c139392c4c696a3a..HEAD -- shared/rust-bridge/codex-ipc/src/bridge.rs shared/rust-bridge/codex-ipc/src/conversation_state.rs`
> Expected: no output. Any output is a STOP condition.

## Status

- **Priority**: P0
- **Effort**: S
- **Risk**: LOW
- **Depends on**: Plan 010
- **Unblocks**: Plan 011
- **Category**: bug, Rust, test baseline, upstream compatibility
- **Tracker**: <https://github.com/amanthanvi/remora/issues/22>
- **Planned at**: reviewed Plan 010 commit
  `fd09fb8db97ce5a28b883752c139392c4c696a3a`, 2026-08-10
- **Execution status**: DONE at
  `7ef63264997200c79674b9705241cc05f1fad894` after the approved fresh retry.

## Why this matters

The current `codex-ipc` test target has twelve compiler errors because two
existing test modules still construct older upstream `Thread`, `Turn`, path,
and status values. This blocks every crate regression gate, including Plan
011's request/task lifecycle cleanup.

This is a test-fixture compatibility repair, not a production projection or
protocol change. Use current upstream value types explicitly and preserve all
existing semantic assertions.

## Scope

**In scope**:

- `shared/rust-bridge/codex-ipc/src/bridge.rs`, only its
  `#[cfg(test)] mod tests` beginning at the existing marker.
- `shared/rust-bridge/codex-ipc/src/conversation_state.rs`, only its
  `#[cfg(test)] mod tests` beginning at the existing marker.

**Out of scope**:

- Production code above either `#[cfg(test)]` boundary.
- Existing test expectations, projection behavior, runtime logic, router,
  reconnect, IPC lifecycle, wire protocol, or upstream types.
- Fixture builders or general constructor abstractions.
- Submodule/patch files, dependencies, manifests/lockfiles, generated output,
  UniFFI, Swift, Kotlin, or documentation.

## Exact diagnostic mapping

The captured baseline contains exactly five errors in `bridge.rs` and seven in
`conversation_state.rs`:

1. Bridge `Thread.cwd`: `PathBuf` to `AbsolutePathBuf`.
2. Bridge `Thread`: add `session_id`, `forked_from_id`, `thread_source`.
3. Bridge `Turn`: add `items_view`, `started_at`, `completed_at`, `duration_ms`.
4. Bridge neutral fixture status: replace unavailable
   `ThreadStatus::default()` with `ThreadStatus::Idle`.
5. Bridge command `cwd`: `PathBuf` to `AbsolutePathBuf`.
6. Conversation projection assertion: compare `cwd` to `AbsolutePathBuf`.
7. First conversation fixture `cwd`: `AbsolutePathBuf`.
8. First nested `Turn`: four current fields.
9. First `Thread`: three current fields.
10. Second conversation fixture `cwd`: `AbsolutePathBuf`.
11. Second nested `Turn`: four current fields.
12. Second `Thread`: three current fields.

## Git workflow

- Create a fresh isolated branch/worktree from
  `fd09fb8db97ce5a28b883752c139392c4c696a3a`.
- Commit subject: `ipc: update protocol test fixtures`.
- One two-file commit only. No merge, push, or pull request.

## Steps

### Step 1: Prove the exact base, bootstrap, and capture the known red baseline

```sh
(
  set -euo pipefail
  test "$(git rev-parse HEAD)" = 'fd09fb8db97ce5a28b883752c139392c4c696a3a'
  test -z "$(git status --short --ignore-submodules=all --untracked-files=all)"
  make patch
  git -C shared/third_party/codex diff --binary | shasum -a 256 \
    > "$(git rev-parse --git-path remora-plan012-submodule.sha256)"

  set +e
  PLAN_RED_OUTPUT="$(
    cargo test --color never --locked \
      --manifest-path shared/rust-bridge/Cargo.toml \
      -p codex-ipc --lib --no-run 2>&1
  )"
  PLAN_RED_STATUS=$?
  set -e
  test "$PLAN_RED_STATUS" -ne 0
  test "$(rg -c '^error\[E[0-9]+\]' <<<"$PLAN_RED_OUTPUT")" -eq 12
  test "$(rg -c -- '--> codex-ipc/src/bridge.rs:' <<<"$PLAN_RED_OUTPUT")" -eq 5
  test "$(rg -c -- '--> codex-ipc/src/conversation_state.rs:' <<<"$PLAN_RED_OUTPUT")" -eq 7
  rg -q 'AbsolutePathBuf|missing fields|ThreadStatus' <<<"$PLAN_RED_OUTPUT"
)
```

Any different count, path, or diagnostic family is a STOP. Do not start from a
new compiler failure or silently add it to this plan.

### Step 2: Update bridge test fixtures only

Inside `bridge.rs`'s test module:

- remove the now-unused `std::path::PathBuf` import;
- import `TurnItemsView` with the current upstream test types;
- import `test_path_buf` and `PathBufExt` from
  `codex_utils_absolute_path::test_support`;
- set each `cwd` with `test_path_buf("/tmp").abs()`;
- set root fixture `session_id` equal to its thread `id`;
- set `forked_from_id: None` and `thread_source: None`;
- set each turn's `items_view: TurnItemsView::Full` and `started_at`,
  `completed_at`, and `duration_ms` to `None`; and
- use explicit `ThreadStatus::Idle` for the neutral projection fixture.

Do not change fixture IDs, source, status parameters, items, expected
notifications, or assertions.

### Step 3: Update conversation-state test fixtures only

Inside `conversation_state.rs`'s test module:

- retain `std::path::PathBuf` because rollout-path fixtures still use it;
- import `test_path_buf` and `PathBufExt` from
  `codex_utils_absolute_path::test_support`;
- compare projected `cwd` against `test_path_buf("/repo").abs()`;
- set both fixture `cwd` fields through `test_path_buf(...).abs()`;
- set root `session_id` to the corresponding thread ID:
  `conversation-1` and `thread-1`;
- set both `forked_from_id` and `thread_source` to `None`; and
- set both nested turns to `upstream::TurnItemsView::Full` with all three
  timing fields `None`.

Do not change `path`, conversation JSON, patch operations, item ordering,
runtime status, active turn, or existing expectations.

### Step 4: Validate the repaired baseline

Run package-scoped formatting before the test matrix, then require every gate:

```sh
(
  set -euo pipefail
  cargo fmt \
    --manifest-path shared/rust-bridge/Cargo.toml \
    -p codex-ipc -- --check
  cargo test --locked \
    --manifest-path shared/rust-bridge/Cargo.toml \
    -p codex-ipc --lib
  cargo test --locked \
    --manifest-path shared/rust-bridge/Cargo.toml \
    -p codex-ipc
  cargo check --locked \
    --manifest-path shared/rust-bridge/Cargo.toml \
    -p codex-ipc --all-targets
)
```

Run production-boundary, stale-constructor, and scope gates from repository
root:

```sh
(
  set -euo pipefail
  BRIDGE='shared/rust-bridge/codex-ipc/src/bridge.rs'
  CONVERSATION='shared/rust-bridge/codex-ipc/src/conversation_state.rs'

  diff -u \
    <(git show HEAD:"$BRIDGE" | sed '/^#\[cfg(test)\]/,$d') \
    <(sed '/^#\[cfg(test)\]/,$d' "$BRIDGE")
  diff -u \
    <(git show HEAD:"$CONVERSATION" | sed '/^#\[cfg(test)\]/,$d') \
    <(sed '/^#\[cfg(test)\]/,$d' "$CONVERSATION")

  if rg -n 'cwd:[[:space:]]*PathBuf::from|thread\.cwd,[[:space:]]*PathBuf::from|ThreadStatus::default\(\)' "$BRIDGE" "$CONVERSATION"; then
    echo 'stale test constructor remains' >&2
    exit 1
  else
    PLAN_RG_STATUS=$?
    test "$PLAN_RG_STATUS" -eq 1
  fi

  test "$(rg -c 'session_id:' "$BRIDGE")" -ge 1
  test "$(rg -c 'session_id:' "$CONVERSATION")" -ge 2
  test "$(rg -c 'items_view:' "$BRIDGE")" -ge 1
  test "$(rg -c 'items_view:' "$CONVERSATION")" -ge 2
  git diff --check

  PLAN_EXPECTED_FILES="$(printf '%s\n' \
    shared/rust-bridge/codex-ipc/src/bridge.rs \
    shared/rust-bridge/codex-ipc/src/conversation_state.rs | LC_ALL=C sort)"
  PLAN_CHANGED_FILES="$(git status --short --ignore-submodules=all --untracked-files=all | cut -c4- | LC_ALL=C sort)"
  test "$PLAN_CHANGED_FILES" = "$PLAN_EXPECTED_FILES"

  test -z "$(git diff --ignore-submodules=dirty --name-only -- \
    shared/rust-bridge/Cargo.toml \
    shared/rust-bridge/Cargo.lock \
    shared/third_party/codex)"
  git -C shared/third_party/codex diff --binary | shasum -a 256 | \
    diff -u "$(git rev-parse --git-path remora-plan012-submodule.sha256)" -
)
```

Inspect the full unstaged diff, stage only the two scoped files, inspect the full
cached diff, and commit. Verify:

```sh
(
  set -euo pipefail
  PLAN_EXPECTED_FILES="$(printf '%s\n' \
    shared/rust-bridge/codex-ipc/src/bridge.rs \
    shared/rust-bridge/codex-ipc/src/conversation_state.rs | LC_ALL=C sort)"
  test "$(git log -1 --format=%s)" = 'ipc: update protocol test fixtures'
  test "$(git rev-parse HEAD^)" = 'fd09fb8db97ce5a28b883752c139392c4c696a3a'
  PLAN_COMMIT_FILES="$(git diff-tree --no-commit-id --name-only -r HEAD | LC_ALL=C sort)"
  test "$PLAN_COMMIT_FILES" = "$PLAN_EXPECTED_FILES"
  test -z "$(git status --short --ignore-submodules=all --untracked-files=all)"
  git diff --check HEAD^
)
```

## Done criteria

- [x] The exact five bridge and seven conversation-state compiler diagnostics
      are resolved without a new compiler error.
- [x] Production prefixes and all existing semantic assertions are unchanged.
- [x] Fixtures use upstream cross-platform absolute-path test support.
- [x] Root-thread identity, no-lineage/no-source, full item view, absent timing,
      and neutral idle status are explicit.
- [x] No abstraction, dependency, protocol, binding, generated, submodule, or
      native-client change.
- [x] Formatting, lib/full tests, all-targets check, exact scope, subject,
      parent, whitespace, and clean gates pass.

## Execution record

- First worktree: `/tmp/remora-plan012.l834p0/worktree`; branch:
  `executor/012-migrate-codex-ipc-test-fixtures`; exact base:
  `fd09fb8db97ce5a28b883752c139392c4c696a3a`.
- Exact base, clean, drift, bootstrap, and red gates passed. Red reproduced
  exactly twelve compiler errors: five in `bridge.rs`, seven in
  `conversation_state.rs`.
- The executor applied only the prescribed test-module fixture changes.
  Production prefixes, exact two-file scope, whitespace, manifest/lock/gitlink,
  and the captured submodule binary-diff hash fence remained unchanged.
- Validation stopped at the first command because the original workspace-wide
  `cargo fmt --all -- --check` reported only pre-existing out-of-scope
  formatting in `codex-mobile-client` and the patched Codex submodule; neither
  scoped file appeared in its diff. No test/check command or commit followed.
- The uncommitted first worktree is historical evidence only. The approved
  retry must use a fresh worktree and the package-scoped `cargo fmt -p
  codex-ipc -- --check` command above. Do not copy or commit the old worktree.
- Approved retry worktree: `/tmp/remora-plan012-retry.IETGff/worktree`; branch:
  `executor/012-migrate-codex-ipc-test-fixtures-retry`; commit:
  `7ef63264997200c79674b9705241cc05f1fad894`; exact parent:
  `fd09fb8db97ce5a28b883752c139392c4c696a3a`.
- The fresh retry reproduced the exact 12-error red, applied only the two test
  modules, passed package formatting, passed 78/78 library tests plus binary
  and doc targets, and passed the all-targets check. Production prefixes,
  manifest/lock/gitlink, exact scope, submodule hash fence, whitespace,
  subject, parent, and clean gates passed.
- Independent review confirmed 31 insertions/8 deletions entirely below the
  two `#[cfg(test)]` markers, byte-identical production prefixes, no assertion
  weakening, and the intended path/identity/items/timing/status semantics.
  No merge, push, or pull request was performed.

## STOP conditions

Stop and report without improvising if:

- Exact base, drift, clean, bootstrap, or 12-diagnostic red proof fails.
- The pre-edit red baseline produces any new error outside the captured
  twelve.
- An existing semantic test fails after compilation; do not rewrite the
  expectation automatically.
- Repair requires production code, a constructor abstraction, upstream or
  patch-file edits, a new dependency, a binding, or any third file.
- Any validation/static/post-commit gate fails twice after correcting only an
  in-scope fixture defect or a command/environment typo. Do not expand scope or
  bypass validation.

## Rollback

Revert the single executor commit. No production code, schema, migration,
protocol, dependency, generated artifact, or persisted state is affected.
