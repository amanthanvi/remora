# Plan 010: Reject Android cross-account OAuth refresh results

> **Executor instructions**: Follow this plan exactly in a fresh isolated
> worktree. Run every gate, touch only the two scoped files, and stop on every
> STOP condition. Commit only after validation. Do not update `plans/README.md`,
> merge, push, or open a pull request.
>
> **Drift check (run first)**:
> `git diff --stat 7d2fa94ce08da7e0736e7f1c3385f389299c53a6..HEAD -- apps/android/app/src/main/java/com/remora/android/state/ChatGPTOAuth.kt apps/android/app/src/test/java/com/remora/android/state/ChatGPTOAuthAccountBindingTest.kt`
> Expected: no output. Any output is a STOP condition.

## Status

- **Priority**: P0
- **Effort**: S
- **Risk**: LOW
- **Depends on**: Plan 004
- **Category**: bug, security, Android, authentication
- **Tracker**: <https://github.com/amanthanvi/remora/issues/14>
- **Planned at**: Android refresh-fallback commit
  `7d2fa94ce08da7e0736e7f1c3385f389299c53a6`, 2026-08-10
- **Execution status**: DONE at
  `fd09fb8db97ce5a28b883752c139392c4c696a3a`.

## Why this matters

Android currently rejects a refreshed account only when both the refreshed and
stored account differ from `previousAccountId`. The normal caller supplies the
stored account as that expected value, so a different refreshed account passes
and is persisted. This is an account-confusion defect.

Plan 004 now resolves a non-rotated refresh token before validation. Preserve
that order, require both stored and refreshed account IDs to match any nonblank
expected ID, and save exactly once only after validation. iOS retains the same
bug but its reviewed implementation remains uncommitted because all permitted
simulator attempts hit a CoreSimulator asset-tool FIFO hang. This plan is an
explicit Android-only risk reduction, not a parity-complete closure of issue
#14.

## Scope

**In scope**:

- `apps/android/app/src/main/java/com/remora/android/state/ChatGPTOAuth.kt`
- `apps/android/app/src/test/java/com/remora/android/state/ChatGPTOAuthAccountBindingTest.kt`

**Out of scope**:

- iOS, loopback callbacks, PKCE/state, token parsing, request retries, endpoints,
  secure-store schema, fallback semantics, login, and remote-control exchanges.
- Rust, UniFFI contracts, Link, relay, dependencies, manifests, resources,
  logging, and tracked generated files.

## Git workflow

- Create a fresh isolated worktree/branch from
  `7d2fa94ce08da7e0736e7f1c3385f389299c53a6`.
- Commit subject: `android: reject cross-account token refresh`.
- One two-file commit only. No merge, push, or pull request.

## Steps

### Step 1: Prove the exact base and Android environment

```sh
(
  set -euo pipefail
  export ANDROID_HOME='/Users/amanthanvi/Library/Android/sdk'
  export ANDROID_SDK_ROOT="$ANDROID_HOME"
  test "$(git rev-parse HEAD)" = '7d2fa94ce08da7e0736e7f1c3385f389299c53a6'
  test -z "$(git status --short --ignore-submodules=all --untracked-files=all)"
  make bindings-kotlin
  cd apps/android
  ./gradlew :app:testDebugUnitTest \
    --tests com.remora.android.state.ChatGPTOAuthRefreshTokenFallbackTest
  ./gradlew :app:testDebugUnitTest \
    --tests com.remora.android.state.ChatGPTOAuthRedactionTest
)
```

Both existing OAuth suites and binding generation must pass before edits.

### Step 2: Add focused failing account-binding tests

Create `ChatGPTOAuthAccountBindingTest.kt` in package
`com.remora.android.state`. Exercise a production helper named
`ChatGPTOAuth.persistValidatedRefresh` with an injected save-lambda spy:

- matching expected, stored, and refreshed account IDs returns the same bundle
  and saves it exactly once;
- refreshed mismatch throws `ChatGPTOAuthException` and saves zero times even
  when stored matches expected;
- stored mismatch throws the same exception and saves zero times even when the
  refreshed account matches expected;
- null expected account preserves unbound behavior and saves once;
- blank expected account preserves unbound behavior and saves once.

Use opaque fixture values only. Assert exception type and save counts, not
account/token values or error-message contents.

Capture red before production changes:

```sh
(
  set -euo pipefail
  export ANDROID_HOME='/Users/amanthanvi/Library/Android/sdk'
  export ANDROID_SDK_ROOT="$ANDROID_HOME"
  set +e
  PLAN_RED_OUTPUT="$(
    cd apps/android &&
      ./gradlew :app:testDebugUnitTest \
        --tests com.remora.android.state.ChatGPTOAuthAccountBindingTest 2>&1
  )"
  PLAN_RED_STATUS=$?
  set -e
  test "$PLAN_RED_STATUS" -ne 0
  rg -q 'Unresolved reference' <<<"$PLAN_RED_OUTPUT"
  rg -q 'persistValidatedRefresh' <<<"$PLAN_RED_OUTPUT"
)
```

Only the missing helper is acceptable red evidence.

### Step 3: Add the validation/persistence helper

Add this synchronous pure-policy helper on `ChatGPTOAuth`:

```kotlin
internal fun persistValidatedRefresh(
    refreshed: ChatGPTOAuthTokenBundle,
    expectedAccountId: String?,
    storedAccountId: String,
    save: (ChatGPTOAuthTokenBundle) -> Unit,
): ChatGPTOAuthTokenBundle
```

When `expectedAccountId` is nonnull and nonblank, require both
`storedAccountId == expectedAccountId` and
`refreshed.accountId == expectedAccountId`. On either mismatch, throw the
existing display-safe `ChatGPTOAuthException` message without invoking `save`.
Otherwise invoke `save(refreshed)` exactly once and return `refreshed`.

After Plan 004's existing `withRefreshTokenFallback` call, replace the old
three-way mismatch condition, direct IO save, and separate return with:

```kotlin
return withContext(Dispatchers.IO) {
    persistValidatedRefresh(
        refreshed = refreshed,
        expectedAccountId = previousAccountId,
        storedAccountId = stored.accountId,
        save = { ChatGPTOAuthTokenStore(context).save(it) },
    )
}
```

Do not change the fallback helper/call, request body, token exchange, parser,
exception text, or any non-refresh flow.

### Step 4: Validate focused behavior and full Android regression

```sh
(
  set -euo pipefail
  export ANDROID_HOME='/Users/amanthanvi/Library/Android/sdk'
  export ANDROID_SDK_ROOT="$ANDROID_HOME"
  cd apps/android
  ./gradlew :app:testDebugUnitTest \
    --tests com.remora.android.state.ChatGPTOAuthAccountBindingTest
  ./gradlew :app:testDebugUnitTest \
    --tests com.remora.android.state.ChatGPTOAuthRefreshTokenFallbackTest
  ./gradlew :app:testDebugUnitTest
)
```

Run static, ordering, and scope gates from repository root:

```sh
(
  set -euo pipefail
  SOURCE='apps/android/app/src/main/java/com/remora/android/state/ChatGPTOAuth.kt'
  TEST='apps/android/app/src/test/java/com/remora/android/state/ChatGPTOAuthAccountBindingTest.kt'
  test "$(rg -c 'persistValidatedRefresh\(' "$SOURCE")" -eq 2
  test "$(rg -c 'persistValidatedRefresh\(' "$TEST")" -eq 5
  test "$(rg -c 'withRefreshTokenFallback\(' "$SOURCE")" -eq 2
  if rg -n 'refreshed\.accountId != previousAccountId &&|stored\.accountId != previousAccountId' "$SOURCE"; then
    echo 'old conjunctive account check remains' >&2
    exit 1
  else
    PLAN_RG_STATUS=$?
    test "$PLAN_RG_STATUS" -eq 1
  fi
  rg -n -U 'val refreshed = withRefreshTokenFallback\(\n[[:space:]]+refreshed = exchangeToken\(body\),\n[[:space:]]+fallbackRefreshToken = refreshToken,\n[[:space:]]+\)\n[[:space:]]+return withContext\(Dispatchers\.IO\) \{\n[[:space:]]+persistValidatedRefresh\(' "$SOURCE"
  rg -n -U 'if \(!expectedAccountId\.isNullOrBlank\(\) &&\n[[:space:]]+\(storedAccountId != expectedAccountId \|\|\n[[:space:]]+refreshed\.accountId != expectedAccountId\)\n[[:space:]]+\)' "$SOURCE"
  rg -n -F 'save = { ChatGPTOAuthTokenStore(context).save(it) }' "$SOURCE"
  git diff --check

  PLAN_EXPECTED_FILES="$(printf '%s\n' \
    apps/android/app/src/main/java/com/remora/android/state/ChatGPTOAuth.kt \
    apps/android/app/src/test/java/com/remora/android/state/ChatGPTOAuthAccountBindingTest.kt | LC_ALL=C sort)"
  PLAN_CHANGED_FILES="$(git status --short --ignore-submodules=all --untracked-files=all | cut -c4- | LC_ALL=C sort)"
  test "$PLAN_CHANGED_FILES" = "$PLAN_EXPECTED_FILES"
)
```

Inspect the full unstaged diff, stage only the two scoped files, inspect the full
cached diff, and commit. Verify:

```sh
(
  set -euo pipefail
  PLAN_EXPECTED_FILES="$(printf '%s\n' \
    apps/android/app/src/main/java/com/remora/android/state/ChatGPTOAuth.kt \
    apps/android/app/src/test/java/com/remora/android/state/ChatGPTOAuthAccountBindingTest.kt | LC_ALL=C sort)"
  test "$(git log -1 --format=%s)" = 'android: reject cross-account token refresh'
  test "$(git rev-parse HEAD^)" = '7d2fa94ce08da7e0736e7f1c3385f389299c53a6'
  PLAN_COMMIT_FILES="$(git diff-tree --no-commit-id --name-only -r HEAD | LC_ALL=C sort)"
  test "$PLAN_COMMIT_FILES" = "$PLAN_EXPECTED_FILES"
  test -z "$(git status --short --ignore-submodules=all --untracked-files=all)"
  git diff --check HEAD^
)
```

## Done criteria

- [x] Refreshed-account mismatch and stored-account mismatch each fail before
      persistence.
- [x] Matching, null-expected, and blank-expected cases save exactly once.
- [x] Plan 004 fallback remains first and its focused tests still pass.
- [x] Login, remote-control, parsing, endpoints, and logging are unchanged.
- [x] Focused and full Android suites pass.
- [x] Exact two-file scope, subject, parent, whitespace, and clean gates pass.
- [x] Issue #14 remains open for the simulator-blocked iOS half.

## Execution record

- Worktree: `/tmp/remora-plan010.kv3iV2/worktree`; branch:
  `executor/010-reject-android-cross-account-refresh`.
- Commit: `fd09fb8db97ce5a28b883752c139392c4c696a3a`; exact parent:
  `7d2fa94ce08da7e0736e7f1c3385f389299c53a6`.
- Binding generation plus existing fallback and OAuth-redaction preflights
  passed before edits. Red failed only on unresolved
  `persistValidatedRefresh`.
- New account-binding tests passed 5/5, fallback regression passed 2/2, and the
  full Android debug unit suite passed. Independent artifact review found 41
  suite XML files and no nonzero failure/error count.
- Exact helper/fallback counts, old-conjunction absence, fallback-before-
  validation ordering, OR-condition validation, save boundary, two-file scope,
  subject/parent/file set, whitespace, and clean gates passed.
- Make-managed generated artifacts and submodule patches remain ignored. No
  merge, push, or pull request was performed.

## STOP conditions

Stop and report without improvising if:

- Exact base/drift/clean preflight fails.
- Binding generation or either existing OAuth suite fails.
- Red evidence is anything except the missing helper.
- The fallback helper/call, exception text, non-refresh exchange, parser,
  endpoint, logging, dependency, or third file must change.
- A mismatch reaches the save spy, or an unbound/matching case does not save
  exactly once.
- Any focused/full test, static/order/scope, parent/subject/file-set,
  whitespace, or clean-worktree gate fails twice after correcting only an
  environment or command typo.

## Rollback

Revert the single Android implementation commit. No schema, persistence-format,
endpoint, protocol, or iOS change.
