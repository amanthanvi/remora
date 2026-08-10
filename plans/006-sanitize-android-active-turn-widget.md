# Plan 006: Sanitize the Android active-turn widget

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. Touch
> only the files listed as in scope. If any STOP condition occurs, stop and
> report; do not improvise. Commit the work in the isolated worktree. When
> dispatched by the Improve advisor, do not update `plans/README.md`; the
> reviewer maintains the index.
>
> **Drift check (run first)**:
> `git diff --stat 58d2e8d812ef8039cd69789a96d5427e93226fa2..HEAD -- apps/android/app/src/main/java/com/remora/android/ui/widget/ActiveTurnWidget.kt apps/android/app/src/main/java/com/remora/android/ui/widget/ActiveTurnWidgetProjection.kt apps/android/app/src/test/java/com/remora/android/ui/widget/ActiveTurnWidgetProjectionTest.kt apps/android/docs/qa-matrix.md`
> Expected: no output. Any output is a STOP condition.

## Status

- **Priority**: P0
- **Effort**: S
- **Risk**: LOW
- **Depends on**: Plans 001 and 003
- **Category**: security, privacy, android
- **Planned at**: approved Plan 003 commit
  `58d2e8d812ef8039cd69789a96d5427e93226fa2`, 2026-08-09
- **Tracker**: <https://github.com/amanthanvi/remora/issues/18>
- **Execution status**: DONE in isolated commit
  `e774968796139c67370ac098062b58e6f5c3571a`; independent of the blocked OAuth
  source files in Plans 002, 004, and 005. Not merged or pushed.

## Why this matters

`ActiveTurnWidget` currently renders `thread.resolvedPreview`, model label,
context-window percentage, and hydrated tool activity on the Android home
screen. The accepted product boundary forbids prompt/work content on system
surfaces and requires an interim status/count-only widget until the shared Rust
`SystemSurfaceProjection` ships.

The smallest safe cut is structural: the Glance view receives a projection
created only from the number of active turns. No prompt, transcript, path,
command, credential, approval, file-content, model, context, or tool value can
enter the widget renderer.

## Current state

- `ActiveTurnWidget.provideGlance` selects an `AppThreadSnapshot` and passes it
  into `ActiveTurnContent`.
- `ActiveTurnContent` reads `resolvedPreview`, `resolvedModel`,
  `contextPercent`, and hydrated conversation items.
- `resolvePhase` and `countToolCalls` scan hydrated timeline content.
- No focused unit test enforces a system-surface privacy boundary.

## Scope

**In scope**:

- `apps/android/app/src/main/java/com/remora/android/ui/widget/ActiveTurnWidget.kt`
- `apps/android/app/src/main/java/com/remora/android/ui/widget/ActiveTurnWidgetProjection.kt`
- `apps/android/app/src/test/java/com/remora/android/ui/widget/ActiveTurnWidgetProjectionTest.kt`
- `apps/android/docs/qa-matrix.md`

**Out of scope**:

- The future shared Rust `SystemSurfaceProjection`, notifications, deep links,
  privacy settings, Live Activities, or awareness relay.
- Changes to `AppModel`, canonical Rust snapshots, widget dependencies,
  manifest declarations, resources, or update scheduling.
- Project/thread/model labels or rich-mode configuration. The interim widget is
  generic by design.

## Git workflow

- Create a fresh isolated worktree from
  `58d2e8d812ef8039cd69789a96d5427e93226fa2`.
- One commit after all validation passes.
- Commit subject: `android: sanitize active-turn widget`.
- Do not merge, push, or open a pull request.

## Steps

### Step 1: Add a failing pure Kotlin projection test

Create `ActiveTurnWidgetProjectionTest.kt` in package
`com.remora.android.ui.widget`. It must call an unresolved production function
named `activeTurnWidgetProjection(activeCount: Int)` and cover:

- zero → inactive, `No active turns`, generic `Idle` status;
- one → active, `1 active turn`, generic `Running` status;
- plural → `N active turns`;
- counts above 99 → bounded `99+ active turns`;
- negative input is treated as zero rather than displayed.

The test and production interface accept no user string or thread snapshot.

Run from a fail-fast subshell after repository-supported Kotlin binding
generation:

```sh
(
  set -euo pipefail
  make bindings-kotlin
  cd apps/android
  ./gradlew :app:testDebugUnitTest \
    --tests 'com.remora.android.ui.widget.ActiveTurnWidgetProjectionTest'
)
```

Expected red: compilation fails specifically because
`activeTurnWidgetProjection` is unresolved. Any dependency, generated-binding,
or unrelated compile failure is a STOP.

### Step 2: Implement the status/count-only projection

Create `ActiveTurnWidgetProjection.kt` as pure Kotlin with one internal data
record and one internal function. The record contains only:

- `isActive: Boolean`;
- `countLabel: String`;
- `statusLabel: String`.

Normalize negative counts to zero. Bound visible counts at `99+`. Emit only
the exact generic labels from Step 1. Do not accept a thread, snapshot, model,
title, preview, timeline item, or arbitrary display string.

### Step 3: Make Glance render only the projection

In `ActiveTurnWidget.kt`:

- derive only
  `val activeCount = snapshot?.threads?.count { it.hasActiveTurn } ?: 0` from
  the Rust snapshot;
- remove active-thread list creation and best-thread selection;
- construct `val projection = activeTurnWidgetProjection(activeCount)`;
- pass only that projection into `ActiveTurnContent`;
- render a generic `Remora` heading plus `countLabel` and `statusLabel`;
- choose active versus idle layout from `isActive`;
- delete `resolvePhase`, `countToolCalls`, prompt/model/context/tool rendering,
  and their now-unused imports.

Do not add click actions, settings, new resources, or a second projection.

### Step 4: Update Android QA documentation

Add a short `System-surface privacy` subsection near the automated regression
scaffolding. Record that the interim home-screen widget displays only generic
active-turn count/status, caps the count at `99+`, and never receives prompts,
transcript content, paths, commands, credentials, approvals, file content,
model labels, context metrics, or tool details. Name the focused regression
test.

### Step 5: Verify and commit

Run:

```sh
(
  set -euo pipefail
  make bindings-kotlin
  cd apps/android
  ./gradlew :app:testDebugUnitTest \
    --tests 'com.remora.android.ui.widget.ActiveTurnWidgetProjectionTest'
  ./gradlew :app:testDebugUnitTest
)

(
  set -euo pipefail
  PLAN_WIDGET='apps/android/app/src/main/java/com/remora/android/ui/widget/ActiveTurnWidget.kt'
  PLAN_PROJECTION='apps/android/app/src/main/java/com/remora/android/ui/widget/ActiveTurnWidgetProjection.kt'
  if rg -n 'AppThreadSnapshot|HydratedConversationItemContent|resolvedPreview|resolvedModel|contextPercent|hydratedConversationItems|best|activeThreads|prompt|transcript|path|command|credential|approval|fileContent|model|toolCount|countToolCalls|resolvePhase' \
    "$PLAN_WIDGET" "$PLAN_PROJECTION"; then
    echo 'forbidden widget input remains' >&2
    exit 1
  else
    PLAN_RG_STATUS=$?
    test "$PLAN_RG_STATUS" -eq 1
  fi
  rg -n 'val activeCount = snapshot\?\.threads\?\.count \{ it\.hasActiveTurn \} \?: 0' "$PLAN_WIDGET"
  test "$(rg -c 'count \{ it\.hasActiveTurn \}' "$PLAN_WIDGET")" -eq 1
  rg -n 'ActiveTurnContent\(projection = projection\)' "$PLAN_WIDGET"
  rg -n 'projection: ActiveTurnWidgetProjection' "$PLAN_WIDGET"
  rg -n 'internal fun activeTurnWidgetProjection\(activeCount: Int\)' "$PLAN_PROJECTION"
  git diff --check
)
```

Then run the exact changed-file check from the repository root:

```sh
(
  set -euo pipefail
  PLAN_CHANGED_FILES="$(git status --short --ignore-submodules=all --untracked-files=all | cut -c4- | LC_ALL=C sort)"
  PLAN_EXPECTED_CHANGED_FILES="$(printf '%s\n' \
    apps/android/app/src/main/java/com/remora/android/ui/widget/ActiveTurnWidget.kt \
    apps/android/app/src/main/java/com/remora/android/ui/widget/ActiveTurnWidgetProjection.kt \
    apps/android/app/src/test/java/com/remora/android/ui/widget/ActiveTurnWidgetProjectionTest.kt \
    apps/android/docs/qa-matrix.md | LC_ALL=C sort)"
  test "$PLAN_CHANGED_FILES" = "$PLAN_EXPECTED_CHANGED_FILES"
)
```

Inspect `git diff --ignore-submodules=all`, stage only the four explicit files,
inspect `git diff --cached`, commit, and verify:

```sh
(
  set -euo pipefail
  PLAN_EXPECTED_COMMIT_FILES="$(printf '%s\n' \
    apps/android/app/src/main/java/com/remora/android/ui/widget/ActiveTurnWidget.kt \
    apps/android/app/src/main/java/com/remora/android/ui/widget/ActiveTurnWidgetProjection.kt \
    apps/android/app/src/test/java/com/remora/android/ui/widget/ActiveTurnWidgetProjectionTest.kt \
    apps/android/docs/qa-matrix.md | LC_ALL=C sort)"
  test "$(git log -1 --format=%s)" = 'android: sanitize active-turn widget'
  test "$(git rev-parse HEAD^)" = '58d2e8d812ef8039cd69789a96d5427e93226fa2'
  PLAN_COMMIT_FILES="$(git diff-tree --no-commit-id --name-only -r HEAD | LC_ALL=C sort)"
  test "$PLAN_COMMIT_FILES" = "$PLAN_EXPECTED_COMMIT_FILES"
  test -z "$(git status --short --ignore-submodules=all --untracked-files=all)"
)
```

## Done criteria

- [x] The widget renderer receives no thread snapshot or arbitrary work text.
- [x] The widget displays only generic status and a bounded active count.
- [x] Timeline hydration is absent from the widget path.
- [x] Zero, one, plural, oversized, and negative cases pass focused tests.
- [x] The full Android debug unit suite passes.
- [x] The QA matrix documents the enforced privacy boundary.
- [x] Exactly the four scoped files are committed from the approved base.
- [x] The isolated worktree is clean.

## STOP conditions

Stop and report without improvising if:

- The drift check reports scoped-file changes after the planned-at commit.
- The red test fails for anything other than the missing projection function.
- Generated bindings change or must be staged.
- A new dependency, manifest/resource change, AppModel change, or Rust contract
  is needed.
- Sanitization requires preserving prompt, transcript, model, context, tool, or
  other work-content fields.
- Any required test, static check, scope check, or post-commit check fails twice
  after correcting only an environment or command typo.

## Rollback

Revert the single executor commit. No data, persistence, schema, route, or
network contract changes.

## Execution record

- Worktree: `/tmp/remora-plan006.lrQIJy/worktree`
- Branch: `executor/006-sanitize-active-turn-widget`
- Commit: `e774968796139c67370ac098062b58e6f5c3571a`
- Parent: `58d2e8d812ef8039cd69789a96d5427e93226fa2`
- Red gate: focused compilation failed only at the five deliberately unresolved
  `activeTurnWidgetProjection` calls after process-local Android SDK variables
  were supplied.
- Green gates: `make bindings-kotlin`, focused
  `ActiveTurnWidgetProjectionTest`, and full `:app:testDebugUnitTest` passed.
- Static gates: forbidden work-content inputs absent; direct active-count
  derivation and projection-only renderer signature present; `git diff --check`
  passed.
- Scope gates: exactly the four planned files committed; exact subject and
  parent; isolated worktree clean.
- Independent review: full commit diff inspected; focused JVM suite and exact
  structural privacy gate rerun successfully.
- Residual validation: no on-device widget visual smoke was required or run.
