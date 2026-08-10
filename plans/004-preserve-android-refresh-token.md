# Plan 004: Preserve Android refresh tokens when rotation is omitted

> **Executor instructions**: Follow this plan step by step in a fresh isolated
> worktree. Run every verification command, touch only scoped files, and stop on
> every STOP condition. Commit only after all gates pass. Do not update
> `plans/README.md`, merge, push, or open a pull request.
>
> **Drift check (run first)**:
> `git diff --stat 04b55b4e0cb636cc9d611550fdbec1d1b7587612..HEAD -- apps/android/app/src/main/java/com/remora/android/state/ChatGPTOAuth.kt apps/android/app/src/test/java/com/remora/android/state/ChatGPTOAuthRefreshTokenFallbackTest.kt`
> Expected: no output. Any output is a STOP condition.

## Status

- **Priority**: P0
- **Effort**: S
- **Risk**: LOW
- **Depends on**: Plan 009
- **Category**: bug, Android, authentication
- **Tracker**: <https://github.com/amanthanvi/remora/issues/16>
- **Planned at**: assembled M0 foundation
  `04b55b4e0cb636cc9d611550fdbec1d1b7587612`, 2026-08-10
- **Execution status**: DONE at
  `7d2fa94ce08da7e0736e7f1c3385f389299c53a6`; unblocked from Plan 002 after
  dependency review.

## Why this matters

Android parses a successful refresh response with no `refresh_token` into a
bundle whose refresh token is `null`, then persists it. Providers may omit the
field when they do not rotate the refresh token. The next refresh then lacks a
usable credential and eventually forces a fresh login. iOS already applies the
existing token as a refresh-only fallback.

This fix is independent of the blocked cross-account remediation. Resolve the
refresh token immediately after `exchangeToken(body)`, then pass the resolved
bundle through the existing account check and persistence path. A later Plan
002 rebase must preserve this ordering when it replaces that validation block.

## Scope

**In scope**:

- `apps/android/app/src/main/java/com/remora/android/state/ChatGPTOAuth.kt`
- `apps/android/app/src/test/java/com/remora/android/state/ChatGPTOAuthRefreshTokenFallbackTest.kt`

**Out of scope**:

- iOS, callback listeners, account-binding semantics, request retries, token
  parsing, secure-store formats, logging, or OAuth endpoints.
- Authorization-code and remote-control exchanges. They have no stored refresh
  token to preserve.
- Rust, UniFFI contracts, Link, relay, dependencies, manifests, resources, and
  tracked generated files.

## Git workflow

- Create a fresh isolated worktree and branch from
  `04b55b4e0cb636cc9d611550fdbec1d1b7587612`.
- Commit subject: `android: preserve non-rotated refresh token`.
- One two-file commit only. No merge, push, or pull request.

## Steps

### Step 1: Prove the fresh Android test environment

From repository root, initialize/generate ignored Kotlin bindings through the
supported path, then run the existing OAuth redaction tests:

```sh
(
  set -euo pipefail
  export ANDROID_HOME='/Users/amanthanvi/Library/Android/sdk'
  export ANDROID_SDK_ROOT="$ANDROID_HOME"
  test "$(git rev-parse HEAD)" = '04b55b4e0cb636cc9d611550fdbec1d1b7587612'
  test -z "$(git status --short --ignore-submodules=all --untracked-files=all)"
  make bindings-kotlin
  cd apps/android
  ./gradlew :app:testDebugUnitTest \
    --tests com.remora.android.state.ChatGPTOAuthRedactionTest
)
```

Any bootstrap, binding, compile, or existing-test failure is a STOP condition.
Generated bindings, stamps, and Make-managed submodule patches remain ignored.

### Step 2: Add the focused failing test

Create `ChatGPTOAuthRefreshTokenFallbackTest.kt` in package
`com.remora.android.state`. It calls a production helper named
`ChatGPTOAuth.withRefreshTokenFallback` and covers:

- `refreshToken = null` receives the stored nonblank fallback;
- an explicit returned replacement stays authoritative;
- the access token, ID token, account ID, and plan type are unchanged in both
  cases.

Use opaque fixtures such as `access_new`, `id_new`, `acct_expected`,
`refresh_stored`, and `refresh_rotated`. Never print or log them.

Before production changes, capture exact red evidence:

```sh
(
  set -euo pipefail
  export ANDROID_HOME='/Users/amanthanvi/Library/Android/sdk'
  export ANDROID_SDK_ROOT="$ANDROID_HOME"
  set +e
  PLAN_RED_OUTPUT="$(
    cd apps/android &&
      ./gradlew :app:testDebugUnitTest \
        --tests com.remora.android.state.ChatGPTOAuthRefreshTokenFallbackTest 2>&1
  )"
  PLAN_RED_STATUS=$?
  set -e
  test "$PLAN_RED_STATUS" -ne 0
  rg -q 'Unresolved reference' <<<"$PLAN_RED_OUTPUT"
  rg -q 'withRefreshTokenFallback' <<<"$PLAN_RED_OUTPUT"
)
```

Only the missing production helper is acceptable red evidence. Any other
compiler, dependency, fixture, or environment failure is a STOP condition.

### Step 3: Add the minimal refresh-only fallback

Add this pure internal helper on `ChatGPTOAuth`:

```kotlin
internal fun withRefreshTokenFallback(
    refreshed: ChatGPTOAuthTokenBundle,
    fallbackRefreshToken: String,
): ChatGPTOAuthTokenBundle
```

If `refreshed.refreshToken` is nonnull, return `refreshed` unchanged. Otherwise
return `refreshed.copy(refreshToken = fallbackRefreshToken)`. The caller already
rejects a null or blank stored token, so add no normalization or policy layer.

In `refreshStoredTokens`, replace:

```kotlin
val refreshed = exchangeToken(body)
```

with:

```kotlin
val refreshed = withRefreshTokenFallback(
    refreshed = exchangeToken(body),
    fallbackRefreshToken = refreshToken,
)
```

Do not change the existing account comparison, persistence block, return value,
request body, or any other exchange. The resolved `refreshed` bundle must reach
the existing check before it is saved.

### Step 4: Validate behavior, call-site confinement, and scope

Run:

```sh
(
  set -euo pipefail
  export ANDROID_HOME='/Users/amanthanvi/Library/Android/sdk'
  export ANDROID_SDK_ROOT="$ANDROID_HOME"
  cd apps/android
  ./gradlew :app:testDebugUnitTest \
    --tests com.remora.android.state.ChatGPTOAuthRefreshTokenFallbackTest
  ./gradlew :app:testDebugUnitTest
)
```

Run static and exact-scope gates from repository root:

```sh
(
  set -euo pipefail
  SOURCE='apps/android/app/src/main/java/com/remora/android/state/ChatGPTOAuth.kt'
  TEST='apps/android/app/src/test/java/com/remora/android/state/ChatGPTOAuthRefreshTokenFallbackTest.kt'
  rg -n 'internal fun withRefreshTokenFallback' "$SOURCE"
  rg -n 'fallbackRefreshToken = refreshToken' "$SOURCE"
  test "$(rg -c 'withRefreshTokenFallback\(' "$SOURCE")" -eq 2
  test "$(rg -c 'withRefreshTokenFallback\(' "$TEST")" -eq 2
  rg -n -U 'val refreshed = withRefreshTokenFallback\(\n[[:space:]]+refreshed = exchangeToken\(body\),\n[[:space:]]+fallbackRefreshToken = refreshToken,\n[[:space:]]+\)' "$SOURCE"

  PLAN_ACCOUNT_BLOCK="$(printf '%s\n' \
    '        if (!previousAccountId.isNullOrBlank() &&' \
    '            refreshed.accountId != previousAccountId &&' \
    '            stored.accountId != previousAccountId' \
    '        ) {' \
    '            throw ChatGPTOAuthException("ChatGPT refresh returned a different account than expected.")' \
    '        }')"
  PLAN_SAVE_RETURN_BLOCK="$(printf '%s\n' \
    '        withContext(Dispatchers.IO) {' \
    '            ChatGPTOAuthTokenStore(context).save(refreshed)' \
    '        }' \
    '        return refreshed')"
  rg -n -F -U "$PLAN_ACCOUNT_BLOCK" "$SOURCE"
  rg -n -F -U "$PLAN_SAVE_RETURN_BLOCK" "$SOURCE"
  PLAN_FALLBACK_LINE="$(rg -n 'val refreshed = withRefreshTokenFallback' "$SOURCE" | cut -d: -f1)"
  PLAN_ACCOUNT_LINE="$(rg -n -F 'if (!previousAccountId.isNullOrBlank() &&' "$SOURCE" | cut -d: -f1)"
  PLAN_SAVE_LINE="$(rg -n -F 'ChatGPTOAuthTokenStore(context).save(refreshed)' "$SOURCE" | cut -d: -f1)"
  test "$PLAN_FALLBACK_LINE" -lt "$PLAN_ACCOUNT_LINE"
  test "$PLAN_ACCOUNT_LINE" -lt "$PLAN_SAVE_LINE"
  git diff --check

  PLAN_EXPECTED_FILES="$(printf '%s\n' \
    apps/android/app/src/main/java/com/remora/android/state/ChatGPTOAuth.kt \
    apps/android/app/src/test/java/com/remora/android/state/ChatGPTOAuthRefreshTokenFallbackTest.kt | LC_ALL=C sort)"
  PLAN_CHANGED_FILES="$(git status --short --ignore-submodules=all --untracked-files=all | cut -c4- | LC_ALL=C sort)"
  test "$PLAN_CHANGED_FILES" = "$PLAN_EXPECTED_FILES"
)
```

Inspect the complete unstaged diff, stage only the two scoped files, inspect the
complete cached diff, and commit. Then run:

```sh
(
  set -euo pipefail
  PLAN_EXPECTED_FILES="$(printf '%s\n' \
    apps/android/app/src/main/java/com/remora/android/state/ChatGPTOAuth.kt \
    apps/android/app/src/test/java/com/remora/android/state/ChatGPTOAuthRefreshTokenFallbackTest.kt | LC_ALL=C sort)"
  test "$(git log -1 --format=%s)" = 'android: preserve non-rotated refresh token'
  test "$(git rev-parse HEAD^)" = '04b55b4e0cb636cc9d611550fdbec1d1b7587612'
  PLAN_COMMIT_FILES="$(git diff-tree --no-commit-id --name-only -r HEAD | LC_ALL=C sort)"
  test "$PLAN_COMMIT_FILES" = "$PLAN_EXPECTED_FILES"
  test -z "$(git status --short --ignore-submodules=all --untracked-files=all)"
  git diff --check HEAD^
)
```

## Done criteria

- [x] Missing rotation preserves the stored nonblank refresh token.
- [x] Explicit rotation retains the returned replacement.
- [x] Every other token-bundle field remains unchanged.
- [x] Only the refresh-token exchange invokes the fallback helper.
- [x] Existing account validation receives the resolved bundle before save.
- [x] Focused and full Android unit suites pass.
- [x] No token/account values are logged and no dependency is added.
- [x] Exact two-file commit scope and clean worktree gates pass.

## Execution record

- Worktree: `/tmp/remora-plan004.zPK032/worktree`; branch:
  `executor/004-preserve-android-refresh-token`.
- Commit: `7d2fa94ce08da7e0736e7f1c3385f389299c53a6`; exact parent:
  `04b55b4e0cb636cc9d611550fdbec1d1b7587612`.
- `make bindings-kotlin` and the existing OAuth redaction suite passed before
  source edits. The first red wrapper lacked invocation-scoped Android SDK
  variables; after the reviewed plan correction, red failed only on unresolved
  `withRefreshTokenFallback` before production changed.
- Focused fallback tests passed 2/2; the full Android debug suite passed.
  Independent artifact review found 40 suite XML files and no nonzero
  failure/error count.
- Exact helper counts, fixed account/save blocks, fallback-before-account-before-
  save ordering, two-file scope, subject/parent/file set, whitespace, and clean
  status gates passed.
- Make-managed generated artifacts and Codex patches remain ignored. No merge,
  push, or pull request was performed.
- Future Plan 002 work must rebase onto this commit and preserve the resolved
  bundle before replacing account validation/persistence.

## STOP conditions

Stop and report without improvising if:

- The exact Plan 009 parent is missing or the drift check reports output.
- Fresh-worktree binding generation or the existing OAuth redaction suite fails.
- Red evidence is anything except the missing helper.
- A login, remote-control, parser, account-validation, persistence, endpoint,
  logging, dependency, or third file must change.
- The helper has more than one production call site.
- Focused/full tests, static checks, exact scope, parent, subject, whitespace, or
  clean-worktree validation fails twice after correcting only an environment or
  command typo.

## Rollback

Revert the single implementation commit. No schema, persisted-data format,
endpoint, or protocol changes.
