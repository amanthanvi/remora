# Plan 009: Assemble the reviewed M0 foundation

> **Executor instructions**: Follow this plan exactly from the reviewed Plan
> 008 commit in a fresh isolated worktree. This is assembly and validation only:
> cherry-pick the already reviewed Plan 006 change without editing it. Stop on
> any conflict or scope drift. Do not update `plans/README.md`.

## Status

- **Priority**: P0
- **Effort**: S
- **Risk**: LOW
- **Depends on**: Plans 006 and 008
- **Category**: integration, Android, privacy
- **Planned at**: reviewed Plan 008 commit
  `3cef97ca4c10cc285e9d1e6bba63d596d7cf2b11`, 2026-08-09
- **Tracker**: <https://github.com/amanthanvi/remora/issues/18>
- **Execution status**: DONE at
  `04b55b4e0cb636cc9d611550fdbec1d1b7587612`.

## Why this matters

Plans 001, 003, 007, and 008 form one linear reviewed chain. Plan 006 was
implemented independently from Plan 003 so it could proceed while OAuth work
was blocked. This plan assembles the reviewed Android widget privacy change on
top of the documentation foundation, then reruns its behavioral and static
privacy gates. Later plans receive one exact parent instead of depending on a
set of divergent local branches.

## Scope

**In scope through the exact Plan 006 cherry-pick only**:

- `apps/android/app/src/main/java/com/remora/android/ui/widget/ActiveTurnWidget.kt`
- `apps/android/app/src/main/java/com/remora/android/ui/widget/ActiveTurnWidgetProjection.kt`
- `apps/android/app/src/test/java/com/remora/android/ui/widget/ActiveTurnWidgetProjectionTest.kt`
- `apps/android/docs/qa-matrix.md`

**Out of scope**:

- Editing the cherry-picked implementation or documentation.
- Resolving a cherry-pick conflict.
- OAuth work, iOS source, Rust source, generated bindings, dependencies, plans,
  issue text, or any other file.

## Exact inputs

- Foundation parent: `3cef97ca4c10cc285e9d1e6bba63d596d7cf2b11`.
- Reviewed Plan 006 source commit:
  `e774968796139c67370ac098062b58e6f5c3571a`.
- Expected source subject: `android: sanitize active-turn widget`.

## Steps

### Step 1: Create and verify the isolated worktree

Create a fresh isolated worktree and branch from
`3cef97ca4c10cc285e9d1e6bba63d596d7cf2b11`. Run:

```sh
(
  set -euo pipefail
  PLAN008_SHA='3cef97ca4c10cc285e9d1e6bba63d596d7cf2b11'
  test "$(git rev-parse HEAD)" = "$PLAN008_SHA"
  test -z "$(git status --short --ignore-submodules=all --untracked-files=all)"
  git cat-file -e 'e774968796139c67370ac098062b58e6f5c3571a^{commit}'
  test "$(git log -1 --format=%s e774968796139c67370ac098062b58e6f5c3571a)" = \
    'android: sanitize active-turn widget'
)
```

Any output from `git status`, wrong SHA, or missing input is a STOP condition.

### Step 2: Cherry-pick without modification

Run exactly:

```sh
git cherry-pick e774968796139c67370ac098062b58e6f5c3571a
```

Any conflict is a STOP condition. Do not resolve it. Abort the cherry-pick and
report the conflicting paths.

### Step 3: Verify exact provenance and scope

Run:

```sh
(
  set -euo pipefail
  PLAN_EXPECTED_FILES="$(printf '%s\n' \
    apps/android/app/src/main/java/com/remora/android/ui/widget/ActiveTurnWidget.kt \
    apps/android/app/src/main/java/com/remora/android/ui/widget/ActiveTurnWidgetProjection.kt \
    apps/android/app/src/test/java/com/remora/android/ui/widget/ActiveTurnWidgetProjectionTest.kt \
    apps/android/docs/qa-matrix.md | LC_ALL=C sort)"
  PLAN_ACTUAL_FILES="$(git diff-tree --no-commit-id --name-only -r HEAD | LC_ALL=C sort)"
  test "$(git rev-parse HEAD^)" = '3cef97ca4c10cc285e9d1e6bba63d596d7cf2b11'
  test "$(git log -1 --format=%s)" = 'android: sanitize active-turn widget'
  test "$PLAN_ACTUAL_FILES" = "$PLAN_EXPECTED_FILES"
  git diff --quiet \
    e774968796139c67370ac098062b58e6f5c3571a \
    HEAD -- \
    apps/android/app/src/main/java/com/remora/android/ui/widget/ActiveTurnWidget.kt \
    apps/android/app/src/main/java/com/remora/android/ui/widget/ActiveTurnWidgetProjection.kt \
    apps/android/app/src/test/java/com/remora/android/ui/widget/ActiveTurnWidgetProjectionTest.kt \
    apps/android/docs/qa-matrix.md
  test -z "$(git status --short --ignore-submodules=all --untracked-files=all)"
  git diff --check HEAD^
)
```

### Step 4: Re-run Android behavior and privacy gates

Use the local Android SDK only for this command invocation:

```sh
(
  set -euo pipefail
  export ANDROID_HOME='/Users/amanthanvi/Library/Android/sdk'
  export ANDROID_SDK_ROOT="$ANDROID_HOME"
  make bindings-kotlin
  cd apps/android
  ./gradlew :app:testDebugUnitTest \
    --tests com.remora.android.ui.widget.ActiveTurnWidgetProjectionTest
  ./gradlew :app:testDebugUnitTest
)
```

The first execution reached `compileDebugKotlin` before tests and failed only
because the fresh worktree lacked the ignored generated UniFFI Kotlin binding.
The executor paused after copying a byte-identical ignored binding from the
reviewed Plan 006 worktree; tracked and visible untracked status remained clean,
and no test was rerun. This revision requires the repository-supported
`make bindings-kotlin` path to initialize/regenerate the artifact before the
single allowed focused/full retry. The generated binding and build stamps stay
ignored and must not be committed.

Then run the static privacy gate:

```sh
(
  set -euo pipefail
  WIDGET='apps/android/app/src/main/java/com/remora/android/ui/widget/ActiveTurnWidget.kt'
  PROJECTION='apps/android/app/src/main/java/com/remora/android/ui/widget/ActiveTurnWidgetProjection.kt'
  if rg -n 'AppThreadSnapshot|HydratedConversationItemContent|resolvedPreview|resolvedModel|contextPercent|hydratedConversationItems|best|activeThreads|prompt|transcript|path|command|credential|approval|fileContent|model|toolCount|countToolCalls|resolvePhase' \
    "$WIDGET" "$PROJECTION"; then
    echo 'forbidden widget input remains' >&2
    exit 1
  else
    PLAN_RG_STATUS=$?
    test "$PLAN_RG_STATUS" -eq 1
  fi
  rg -n 'val activeCount = snapshot\?\.threads\?\.count \{ it\.hasActiveTurn \} \?: 0' "$WIDGET"
  test "$(rg -c 'count \{ it\.hasActiveTurn \}' "$WIDGET")" -eq 1
  rg -n 'ActiveTurnContent\(projection = projection\)' "$WIDGET"
  rg -n 'projection: ActiveTurnWidgetProjection' "$WIDGET"
  rg -n 'internal fun activeTurnWidgetProjection\(activeCount: Int\)' "$PROJECTION"
  git diff --check HEAD^
  test -z "$(git status --short --ignore-submodules=all --untracked-files=all)"
)
```

## Done criteria

- [x] The exact reviewed Plan 006 commit is cherry-picked without conflict or
      modification onto the exact Plan 008 parent.
- [x] The resulting commit contains only the four reviewed Android files.
- [x] Focused and full Android unit tests pass.
- [x] The static privacy gate confirms no prompt, model, context metrics, tool,
      transcript, command, path, credential, or other work content enters the
      widget surface.
- [x] The isolated worktree is clean under the prescribed ignored-artifact and
      ignored-submodule policy.

## Execution record

- Worktree: `/tmp/remora-plan009.mi3OHB/worktree`; branch:
  `executor/009-assemble-m0-foundation`.
- Resulting commit: `04b55b4e0cb636cc9d611550fdbec1d1b7587612`;
  exact parent: `3cef97ca4c10cc285e9d1e6bba63d596d7cf2b11`.
- Cherry-pick completed without conflict; subject, exact four-file scope, and
  file contents match reviewed Plan 006 commit
  `e774968796139c67370ac098062b58e6f5c3571a`.
- The initial Gradle attempt stopped before tests because ignored UniFFI Kotlin
  bindings were absent. After a reviewed plan revision, `make bindings-kotlin`
  initialized the pinned Codex submodule and regenerated bindings through the
  supported path.
- Focused `ActiveTurnWidgetProjectionTest` passed 5/5; the full Android debug
  unit suite passed. Independent artifact inspection found 39 suite XML files
  and no nonzero failure/error count.
- Structural privacy, provenance, whitespace, and post-test cleanliness gates
  passed. Generated files, stamps, and patched submodule state remain ignored.
- No merge, push, or pull request was performed.

## STOP conditions

Stop and report without improvising if:

- The parent differs from reviewed Plan 008 commit
  `3cef97ca4c10cc285e9d1e6bba63d596d7cf2b11`.
- The cherry-pick conflicts or becomes empty.
- The resulting diff or subject differs from reviewed Plan 006.
- Any file outside the four-file scope changes.
- Android validation fails twice after correcting only an environment or
  command typo.
- Validation requires a source edit, dependency change, cache deletion, global
  configuration change, merge, push, or pull request.

## Rollback

Remove the isolated worktree/branch or revert the single cherry-picked commit.
No remote state changes.
