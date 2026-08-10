# Plan 002: Reject cross-account OAuth refresh results

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. Touch
> only the files listed as in scope. If any STOP condition occurs, stop and
> report; do not improvise. Commit the work in the isolated worktree. When
> dispatched by the Improve advisor, do not update `plans/README.md`; the
> reviewer maintains the index.
>
> **Drift check (run first)**:
> `git diff --stat 58d2e8d812ef8039cd69789a96d5427e93226fa2..HEAD -- apps/ios/Sources/Remora/Models/ChatGPTOAuth.swift apps/ios/Tests/RemoraTests/ChatGPTOAuthTests.swift apps/android/app/src/main/java/com/remora/android/state/ChatGPTOAuth.kt apps/android/app/src/test/java/com/remora/android/state/ChatGPTOAuthAccountBindingTest.kt`
> Expected: no output. Any output is a STOP condition.

## Status

- **Priority**: P0
- **Effort**: S
- **Risk**: LOW
- **Depends on**: Plans 001 and 003
- **Category**: bug, security, ios, android
- **Planned at**: approved Plan 003 commit
  `58d2e8d812ef8039cd69789a96d5427e93226fa2`, 2026-08-09
- **Tracker**: <https://github.com/amanthanvi/remora/issues/14>
- **Execution status**: DONE. Android landed at
  `fd09fb8db97ce5a28b883752c139392c4c696a3a`; iOS landed at
  `566a584` after a fresh simulator and validation-only asset-catalog exclusion
  bypassed the external Xcode 26.6 FIFO defect. The original stopped worktree
  remains historical evidence and was not reused.

## Why this matters

Both native OAuth implementations accept and persist a refreshed token bundle
for a different account during the normal stored-token path. The caller passes
the stored account as the expected account, making the existing final
`stored != expected` conjunction false even when `refreshed != expected`.

The invariant belongs beside platform OAuth and secure credential custody:
when a nonblank expected account is supplied, both the stored and refreshed
account IDs must equal it before the result is saved. A mismatch must fail with
the existing platform error and must not persist the refreshed bundle.

This plan does not move credentials into Rust, change OAuth endpoints, add a
dependency, or alter the existing caller fallback behavior.

## Current state

### iOS

`apps/ios/Sources/Remora/Models/ChatGPTOAuth.swift` currently contains:

```swift
let refreshed = try await exchangeRefreshToken(
    refreshToken,
    fallbackRefreshToken: refreshToken
)
if let previousAccountID, !previousAccountID.isEmpty,
   refreshed.accountID != previousAccountID,
   stored.accountID != previousAccountID {
    throw ChatGPTOAuthError.refreshAccountMismatch
}
try ChatGPTOAuthTokenStore.shared.save(refreshed)
return refreshed
```

`apps/ios/Tests/RemoraTests/ChatGPTOAuthTests.swift` uses `@testable import
Remora` and already covers token-bundle refresh-token preservation, but not
account binding.

### Android

`apps/android/app/src/main/java/com/remora/android/state/ChatGPTOAuth.kt`
currently contains:

```kotlin
val refreshed = exchangeToken(body)
if (!previousAccountId.isNullOrBlank() &&
    refreshed.accountId != previousAccountId &&
    stored.accountId != previousAccountId
) {
    throw ChatGPTOAuthException("ChatGPT refresh returned a different account than expected.")
}
withContext(Dispatchers.IO) {
    ChatGPTOAuthTokenStore(context).save(refreshed)
}
return refreshed
```

There is no account-binding unit test under
`apps/android/app/src/test/java/com/remora/android/state/`.

## Scope

**In scope**:

- `apps/ios/Sources/Remora/Models/ChatGPTOAuth.swift`
- `apps/ios/Tests/RemoraTests/ChatGPTOAuthTests.swift`
- `apps/android/app/src/main/java/com/remora/android/state/ChatGPTOAuth.kt`
- `apps/android/app/src/test/java/com/remora/android/state/ChatGPTOAuthAccountBindingTest.kt`

**Out of scope**:

- OAuth callback listener binding, request limits, state consumption, PKCE, or
  token-response parsing.
- Android refresh-token fallback when the response omits a replacement token;
  that is a separate finding and plan.
- Rust, UniFFI, Link, relay, secure-store formats, and generated bindings.
- Changing `loadStoredOrRefreshedTokens` fallback behavior.
- Logging token or account values.

## Git workflow

- Base the fresh isolated branch on approved Plan 003 executor commit
  `58d2e8d812ef8039cd69789a96d5427e93226fa2`.
- One commit after all verification passes.
- Commit subject: `auth: reject cross-account token refresh`.
- Do not push, merge, or open a pull request.

## Steps

### Step 1: Add failing iOS account-binding and persistence tests

In `ChatGPTOAuthTests`, add focused tests for an internal production helper
named `persistValidatedRefresh` using an injected save closure as a spy:

- matching expected, stored, and refreshed IDs returns the refreshed bundle and
  invokes the save closure exactly once;
- a refreshed ID different from the expected ID throws
  `ChatGPTOAuthError.refreshAccountMismatch`, even when stored matches, and
  invokes the save closure zero times;
- a stored ID different from the expected ID throws the same error, even when
  refreshed matches, and invokes the save closure zero times;
- nil and empty expected IDs preserve existing unbound behavior, return the
  refreshed bundle, and invoke the save closure exactly once per call.

Use opaque examples such as `acct_expected`, `acct_stored`, and
`acct_refreshed`. Do not use real identifiers or log them.

### Step 2: Make the iOS helper the sole refresh-persistence path

Add an internal, synchronous function on `ChatGPTOAuth`:

```swift
static func persistValidatedRefresh(
    _ refreshed: ChatGPTOAuthTokenBundle,
    expectedAccountID: String?,
    storedAccountID: String,
    save: (ChatGPTOAuthTokenBundle) throws -> Void
) throws -> ChatGPTOAuthTokenBundle
```

Behavior:

- when `expectedAccountID` is nonnil and nonempty, require both
  `storedAccountID == expectedAccountID` and
  `refreshed.accountID == expectedAccountID`;
- throw the existing `ChatGPTOAuthError.refreshAccountMismatch` on either
  mismatch without invoking `save`;
- otherwise invoke `save(refreshed)` exactly once and return `refreshed`;
- contain no logging and no persistence implementation beyond the injected
  closure.

Immediately after `exchangeRefreshToken`, return the result of
`persistValidatedRefresh`, passing a closure that calls
`ChatGPTOAuthTokenStore.shared.save`. Delete both the old three-way condition
and the separate direct save, making this helper the sole persistence path for
refresh results. Do not change the exchange or fallback refresh-token
arguments.

### Step 3: Add failing Android account-binding and persistence tests

Create `ChatGPTOAuthAccountBindingTest.kt` in package
`com.remora.android.state`. Cover the same cases and exact save counts as iOS
against internal `ChatGPTOAuth.persistValidatedRefresh`, injecting a lambda
spy. For mismatch cases, assert `ChatGPTOAuthException`; do not assert or print
account values.

### Step 4: Make the Android helper the sole refresh-persistence path

Add an internal, synchronous function on `ChatGPTOAuth`:

```kotlin
internal fun persistValidatedRefresh(
    refreshed: ChatGPTOAuthTokenBundle,
    expectedAccountId: String?,
    storedAccountId: String,
    save: (ChatGPTOAuthTokenBundle) -> Unit,
): ChatGPTOAuthTokenBundle
```

Behavior:

- when `expectedAccountId` is nonnull and nonblank, require both the stored and
  refreshed IDs to equal it;
- throw the existing display-safe `ChatGPTOAuthException` message on either
  mismatch without invoking `save`;
- otherwise invoke `save(refreshed)` exactly once and return `refreshed`;
- contain no logging and no persistence implementation beyond the injected
  lambda.

Immediately after `exchangeToken`, return the result of
`persistValidatedRefresh` from the existing IO persistence block, passing a
lambda that saves through `ChatGPTOAuthTokenStore`. Delete both the old
conjunction and the separate direct save, making this helper the sole
refresh-result persistence path. Do not change request-body construction or
token parsing.

### Step 5: Run focused verification

First run static and scope checks:

```sh
rg -n 'persistValidatedRefresh' \
  apps/ios/Sources/Remora/Models/ChatGPTOAuth.swift \
  apps/ios/Tests/RemoraTests/ChatGPTOAuthTests.swift \
  apps/android/app/src/main/java/com/remora/android/state/ChatGPTOAuth.kt \
  apps/android/app/src/test/java/com/remora/android/state/ChatGPTOAuthAccountBindingTest.kt
! rg -n 'refreshed\.accountID != previousAccountID,|refreshed\.accountId != previousAccountId &&' \
  apps/ios/Sources/Remora/Models/ChatGPTOAuth.swift \
  apps/android/app/src/main/java/com/remora/android/state/ChatGPTOAuth.kt
git diff --check
```

Then bootstrap only through repository-supported Make targets if generated
artifacts are absent. Use the fast simulator lane rather than the package lane:

```sh
make ios-sim-fast
xcodebuild test \
  -project apps/ios/Remora.xcodeproj \
  -scheme Remora \
  -configuration Debug \
  -destination 'platform=iOS Simulator,name=iPhone 17 Pro' \
  -only-testing:RemoraTests/ChatGPTOAuthTests
make bindings-kotlin
(cd apps/android && ./gradlew :app:testDebugUnitTest \
  --tests 'com.remora.android.state.ChatGPTOAuthAccountBindingTest')
```

Expected: the fast iOS build succeeds and both focused suites pass. If the
named simulator is unavailable, use the repository-configured
`IOS_SIM_DEVICE` value or another already-installed iOS simulator and record
the exact substitution. Do not hand-initialize submodules or generate files
outside the Make targets.

Run the exact changed-file check from repository root:

```sh
PLAN_CHANGED_FILES="$(git status --short --ignore-submodules=all --untracked-files=all | cut -c4- | LC_ALL=C sort)"
PLAN_EXPECTED_CHANGED_FILES="$(printf '%s\n' \
  apps/android/app/src/main/java/com/remora/android/state/ChatGPTOAuth.kt \
  apps/android/app/src/test/java/com/remora/android/state/ChatGPTOAuthAccountBindingTest.kt \
  apps/ios/Sources/Remora/Models/ChatGPTOAuth.swift \
  apps/ios/Tests/RemoraTests/ChatGPTOAuthTests.swift | LC_ALL=C sort)"
test "$PLAN_CHANGED_FILES" = "$PLAN_EXPECTED_CHANGED_FILES"
```

Ignored generated/build artifacts and Make-managed local-only submodule patch
state are allowed; no generated or submodule file may be staged. Inspect
`git diff --ignore-submodules=all`, stage the four explicit files, inspect
`git diff --cached`, and commit. Then verify:

```sh
test "$(git log -1 --format=%s)" = 'auth: reject cross-account token refresh'
PLAN_COMMIT_FILES="$(git diff-tree --no-commit-id --name-only -r HEAD | LC_ALL=C sort)"
PLAN_EXPECTED_COMMIT_FILES="$(printf '%s\n' \
  apps/android/app/src/main/java/com/remora/android/state/ChatGPTOAuth.kt \
  apps/android/app/src/test/java/com/remora/android/state/ChatGPTOAuthAccountBindingTest.kt \
  apps/ios/Sources/Remora/Models/ChatGPTOAuth.swift \
  apps/ios/Tests/RemoraTests/ChatGPTOAuthTests.swift | LC_ALL=C sort)"
test "$PLAN_COMMIT_FILES" = "$PLAN_EXPECTED_COMMIT_FILES"
test -z "$(git status --short --ignore-submodules=all --untracked-files=all)"
```

## Test plan

- iOS unit tests prove the exact refreshed-mismatch regression and the
  symmetric stale-stored-account case.
- Android unit tests prove parity for the same invariant.
- Full-match and missing-expected cases prevent accidental tightening of
  successful or explicitly unbound flows.
- Focused tests avoid network and secure-store access while exercising the
  production validation-and-save helper with a save spy.
- The fast iOS build catches integration and actor-isolation errors.

## Done criteria

- [x] iOS rejects a refreshed account different from a nonblank expected ID.
- [x] Android rejects the same mismatch.
- [x] Both platforms also reject a stored account different from the expected
      ID before saving a refresh result.
- [x] Nil/empty iOS and null/blank Android expectations preserve prior
      unbound behavior.
- [x] Both focused test suites pass.
- [x] The iOS fast simulator build passes with the asset catalog excluded to
  bypass the external Xcode 26.6 simulator-agent FIFO defect; the full Rust,
  Swift, link, install, and XCTest path is exercised.
- [x] No token or account value is logged.
- [x] All four scoped files are committed across the platform-specific commits.
- [x] The superproject worktree is clean when plan documents and ignored
      generated/submodule state are excluded.

## STOP conditions

Stop and report without improvising if:

- The drift check reports any scoped-file change after the planned-at commit.
- Enforcing the invariant requires changing token parsing, persistence format,
  OAuth endpoints, callback handling, or `loadStoredOrRefreshedTokens` fallback.
- Tests require network access or real credentials.
- A runtime dependency or generated-binding change appears necessary.
- Platform behavior cannot remain symmetric without a product decision.
- Any required Make bootstrap/build, focused test, static check, exact scope
  check, or post-commit verification fails twice after correcting only an
  environment or command typo.

## Advisor recovery addendum: exact simulator reboot

The first two supported fast-lane attempts completed Codex/Ghostty bootstrap,
all existing patches, UniFFI generation, the simulator Rust archive, and Xcode
project regeneration. Both then hung in Xcode asset thinning. Bounded samples
of the exact owned descendants showed `ibtoold` blocked in
`_openFIFOsForPID` → `IBOpenConnectionWithRemoteFIFOReturningFileDescriptors`
→ `open`. Exact orphan descendants were terminated and verified gone; no
name-based process kill or global service restart occurred.

One additional validation attempt is permitted only after rebooting the exact
test simulator, not CoreSimulator globally. From the preserved fresh worktree:

Run the block in a tracked PTY/session and record its root process PID before
waiting on output:

```sh
(
  set -euo pipefail
  cd /tmp/remora-plan002-v2.k7ntRN

  PLAN_SIMULATOR_UDID='E00E4757-DFBC-4BAF-AA8C-CF4309EC1DDA'
  test -z "$(ps -p 30004,39916 -o pid= 2>/dev/null || true)"

  xcrun simctl shutdown "$PLAN_SIMULATOR_UDID"
  xcrun simctl boot "$PLAN_SIMULATOR_UDID"
  xcrun simctl bootstatus "$PLAN_SIMULATOR_UDID" -b
  xcrun simctl spawn "$PLAN_SIMULATOR_UDID" /usr/bin/true

  IOS_SIM_DESTINATION="platform=iOS Simulator,id=$PLAN_SIMULATOR_UDID" \
    make ios-sim-fast
)
```

Expected: exact-device reboot and spawn succeed, then the supported fast lane
completes using the already-generated Make artifacts and exact rebooted device.
If the same asset-tool FIFO hang recurs after this device reboot, terminate only
the recorded root process and exact descendants resolved through parentage,
then verify those exact PIDs are absent. Stop permanently and report the
environment blocker; do not kill by name/path, restart services, delete broad
caches, or try a fourth build. If it passes, continue the original focused iOS,
Kotlin binding, Android focused test, scope, commit, and post-commit gates
exactly as written.

## Maintenance notes

- Account IDs are opaque identifiers. Compare for exact equality; do not
  normalize, lowercase, truncate, or expose them.
- Keep the validation-and-persistence helper beside native OAuth because the
  tokens remain in platform secure custody. This small parity invariant does
  not justify a new Rust credential interface.
- Android refresh-token preservation is tracked separately and should build on
  this commit without broadening this plan.

## Execution record

- The four scoped source/test edits were implemented in the isolated worktree
  and passed static helper-presence, obsolete-condition absence,
  `git diff --check`, and exact-scope checks.
- Attempts one and two completed Codex/Ghostty bootstrap, UniFFI generation,
  the simulator Rust archive, and Xcode project generation before hanging in
  asset thinning. Exact `ibtoold` samples showed
  `_openFIFOsForPID` → `IBOpenConnectionWithRemoteFIFOReturningFileDescriptors`
  → blocking `open`.
- The approved final attempt shut down and booted simulator
  `E00E4757-DFBC-4BAF-AA8C-CF4309EC1DDA`, waited for boot, and successfully ran
  `/usr/bin/true` inside it before invoking the exact-ID fast lane.
- The final attempt made no asset-thinning progress for approximately five
  minutes and reproduced the same sampled FIFO stack. The tracked build root
  PID `44614` and its owned build descendants were interrupted and verified
  absent. Reparented simulator-tool PIDs were deliberately not signalled.
- Focused XCTest, Kotlin binding generation, Android focused tests, staging,
  and commit were not run because the permanent STOP boundary occurred first.
- No source commit was created. Preserve the isolated worktree for diagnosis or
  resume only after the external CoreSimulator condition changes and a new
  reviewed recovery plan explicitly supersedes this STOP.
- A later recovery run created simulator
  `64C4CECF-EF8E-4175-B9A7-FC67A3EE340A` and used the repository's existing
  `XCODE_EXTRA_ARGS` seam to exclude only `Assets.xcassets` from the validation
  build. `make ios-sim-fast` and focused `ChatGPTOAuthTests` passed through the
  full Rust, Swift, link, install, and XCTest path.
- iOS account binding landed at `566a584`; Android account binding remains at
  `fd09fb8`. No credential values were logged and no dependency or persistence
  format changed.
