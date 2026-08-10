# Plan 005: Bind and bound iOS OAuth loopback callbacks

> **Executor instructions**: Follow this plan exactly in a fresh isolated
> worktree. Touch only the two scoped files. Run each gate separately and stop
> on any unexpected failure. Commit only after all gates pass. Do not edit this
> plan, push, merge, or open a pull request.
>
> **Drift check**:
> `git diff --stat 566a584..HEAD -- apps/ios/Sources/Remora/Models/ChatGPTOAuth.swift apps/ios/Tests/RemoraTests/ChatGPTOAuthTests.swift`
> Expected: no output. An unresolved placeholder or any output is a STOP
> condition.

## Status

- **Priority**: P0
- **Effort**: M
- **Risk**: MED
- **Depends on**: Plan 002
- **Category**: bug, security, ios
- **Tracker**: [#17](https://github.com/amanthanvi/remora/issues/17)
- **Planned at**: iOS Plan 002 commit `566a584`, 2026-08-10
- **Execution status**: DONE at `c4df344`
- **Plan review**: APPROVED after three cold passes

## Why this matters

The iOS OAuth server stores `127.0.0.1` as its bind host but creates a wildcard
`NWListener`. Receive chunks are individually bounded while the accumulated
header is not, and listener delivery is unlimited. Malformed or adjacent-
network clients can consume memory or callback capacity during login.

Use Network.framework directly. Bind the listener to the configured IPv4
loopback endpoint, allow four simultaneous in-flight clients, cap complete
headers at 16 KiB, and give each client five seconds to finish its header. Keep
the existing localhost redirect, PKCE, state validation, overall callback
timeout, and native browser flow.

## Verified Network.framework constraints

- `NWParameters.requiredLocalEndpoint` and `NWListener.newConnectionLimit` are
  available below the app's iOS 18 deployment target.
- When `requiredLocalEndpoint` already contains `127.0.0.1:1455`, calling
  `NWListener(using:on:)` with the same port throws `POSIX EINVAL`.
- Construct the listener with `NWListener(using: parameters)` only.
- `newConnectionLimit` is a consumable delivery budget, not an automatically
  replenished concurrent-connection cap. Each terminal client path must return
  exactly one permit while the listener remains active.

## Scope

**In scope**:

- `apps/ios/Sources/Remora/Models/ChatGPTOAuth.swift`
- `apps/ios/Tests/RemoraTests/ChatGPTOAuthTests.swift`

**Out of scope**:

- Android callback parsing; track separately.
- OAuth endpoints, PKCE, state/token semantics, credential persistence, and
  callback UI.
- IPv6 dual-listener support, a general HTTP server, third-party networking,
  and dependency changes.
- Logging request data, query values, codes, state, or credentials.

## Locked bounds and behavior

- Maximum complete request headers: 16 KiB.
- Maximum simultaneous in-flight connections: four.
- Per-client header deadline: five seconds.
- Only an exact three-token origin-form request line:
  `GET /auth/callback?... HTTP/1.1`.
- Strict UTF-8; a complete `\r\n\r\n` terminator is required before parsing.
- Oversize, incomplete, invalid UTF-8, wrong-method, malformed-line, and
  wrong-path requests close only that client and replenish one permit.
- Invalid clients never complete or fail the overall OAuth wait.
- Valid callback, listener failure, explicit stop, and overall timeout retain
  the existing locked continuation path.

## Git workflow

- Base a fresh branch/worktree on `566a584`.
- Commit subject: `ios: bind and bound OAuth callback listener`.
- Create one commit only after all verification gates pass.
- Do not push, merge, or open a pull request.

## Steps

All commands start from repository root. Record one simulator destination and
reuse it in every command.

### Step 1: Prove the Plan 002 iOS baseline

```sh
make ios-sim-fast
xcodebuild test \
  -project apps/ios/Remora.xcodeproj \
  -scheme Remora \
  -configuration Debug \
  -destination 'platform=iOS Simulator,name=iPhone 17 Pro' \
  -only-testing:RemoraTests/ChatGPTOAuthTests
```

Both must pass before edits. An unavailable named simulator may be replaced
only with the repository-configured `IOS_SIM_DEVICE` or an already-installed
simulator; record and reuse the exact substitution. Other failures stop work.

### Step 2: Add focused failing tests

Add `import Network` explicitly to `ChatGPTOAuthTests`. Extend it for production
helpers named `callbackListener`, `callbackReceiveDecision`, and
`callbackRequestURL`, plus the one-shot connection permit used by the listener:

- the unstarted production listener has exact required endpoint
  `127.0.0.1:1455` and initial `newConnectionLimit == 4`;
- a connection permit invokes its injected restore closure exactly once when
  terminal cleanup is requested concurrently from multiple tasks (and still
  only once after an additional sequential call);
- split valid headers remain in receive state until the terminator, then enter
  process state with the complete bounded data;
- an exactly 16 KiB complete header is processable;
- 16 KiB plus one byte is rejected before append;
- EOF without a terminator is rejected;
- no request parsing occurs before the terminator;
- strict parser accepts the exact valid GET request and preserves its query;
- strict parser rejects invalid UTF-8, non-GET, non-origin-form target,
  malformed token count, non-HTTP/1.1, and wrong path.

Use only synthetic codes/state and never print request/query content. Before
production edits, run and validate the exact red state:

```sh
if PLAN_RED_OUTPUT="$(xcodebuild test \
  -project apps/ios/Remora.xcodeproj \
  -scheme Remora \
  -configuration Debug \
  -destination 'platform=iOS Simulator,name=iPhone 17 Pro' \
  -only-testing:RemoraTests/ChatGPTOAuthTests 2>&1)"; then
  echo 'ERROR: focused pre-change test unexpectedly passed'
  exit 1
fi
printf '%s\n' "$PLAN_RED_OUTPUT" |
  rg -q 'callbackListener|callbackRequestURL|callbackReceiveDecision'
printf '%s\n' "$PLAN_RED_OUTPUT" |
  rg -q 'has no member|cannot find'
```

Reuse any recorded simulator substitution. Output must contain both a planned
symbol and a missing-symbol diagnostic. Anything else is a STOP condition.

### Step 3: Add narrow production-used helpers

Because `ChatGPTOAuth` is `@MainActor` while the listener runs on its own queue,
mark the immutable bounds and all three synchronous helpers `nonisolated
static`. Add:

```swift
nonisolated static func callbackListener(
    bindHost: String,
    port: NWEndpoint.Port
) throws -> NWListener

nonisolated static func callbackReceiveDecision(
    buffer: Data,
    incoming: Data?,
    isComplete: Bool
) -> CallbackReceiveDecision

nonisolated static func callbackRequestURL(
    from data: Data,
    publicHost: String,
    port: UInt16,
    path: String
) throws -> URL
```

The throwing listener helper creates `NWParameters.tcp`, assigns the exact
`.hostPort` to `requiredLocalEndpoint`, constructs
`NWListener(using: parameters)` with no `on:` argument, sets the initial
delivery limit to four, and returns the unstarted listener. The real server
must use this helper.

The receive-decision helper is the sole accumulator policy. It checks remaining
capacity before append, never grows beyond 16 KiB, returns process only after a
terminator, rejects completed input without a terminator, and is used directly
by `receiveRequest`.

The parser checks the byte bound before decoding, uses
`String(data:encoding: .utf8)` rather than lossy decoding, isolates headers at
the terminator, and requires exactly three space-separated tokens: `GET`, an
origin-form target beginning with one `/`, and `HTTP/1.1`. It builds against the
configured public host/port and requires the exact path. Reuse
`ChatGPTOAuthError.invalidCallbackURL`; do not add a public error or HTTP layer.

Define `CallbackReceiveDecision` as an internal file-scope `Sendable` enum.
Define one small internal file-scope one-shot permit as a `final class` with a
`@Sendable` injected terminal closure. Its only mutable flag is protected by an
`NSLock`, which is the explicit justification for the narrow `@unchecked
Sendable` conformance. Production uses it to make timeout/receive/EOF/response
races idempotent; its terminal closure runs at most once.

### Step 4: Apply bounded connection lifecycle to the server

- Replace the current listener construction with `callbackListener`.
- Maintain one minimal client registry confined to the existing serial listener
  queue. It stores only the accepted connection and its five-second deadline
  task by opaque client ID so `stop()` can clean them up; it is lifecycle
  bookkeeping, not a reusable connection pool.
- For every accepted connection, register it, create one permit, and create its
  five-second deadline Task.
- Funnel timeout, receive error, rejected receive decision, invalid parser,
  incomplete EOF, and response completion through the permit's one-shot
  terminal closure.
- The permit's terminal closure does no cleanup directly. It dispatches exactly
  one `finishClient(id:)` operation onto the existing serial listener queue.
- `finishClient(id:)` is the sole owner of client-registry, deadline-task,
  connection, and `newConnectionLimit` mutations: remove the registry entry,
  cancel and release its deadline task, cancel its connection, then restore one
  permit only if the same listener instance remains active.
- A successful response follows the same one-shot connection cleanup, then the
  existing locked callback delivery remains authoritative.
- Invalid client traffic never calls `resumeCallback`.
- `stop()` marks and clears the listener as inactive before queueing cleanup of
  every registered client, so client cleanup cannot restore permits to a
  stopped or replacement listener.

Keep all mutable client lifecycle bookkeeping on the existing serial queue;
use the existing lock only for the already-established continuation/listener
completion state. Do not add an actor, connection pool, custom server, retry
framework, or new configuration surface.

### Step 5: Validate behavior and exact scope

```sh
git diff --check
xcodebuild test \
  -project apps/ios/Remora.xcodeproj \
  -scheme Remora \
  -configuration Debug \
  -destination 'platform=iOS Simulator,name=iPhone 17 Pro' \
  -only-testing:RemoraTests/ChatGPTOAuthTests
make ios-sim-fast
rg -n 'requiredLocalEndpoint|newConnectionLimit|callbackRequestMaxBytes|callbackReceiveDecision|callbackRequestURL' \
  apps/ios/Sources/Remora/Models/ChatGPTOAuth.swift \
  apps/ios/Tests/RemoraTests/ChatGPTOAuthTests.swift
```

Reuse the recorded simulator substitution. Then verify exact tracked scope:

```sh
PLAN_CHANGED_FILES="$(git status --short --ignore-submodules=all --untracked-files=all | cut -c4- | LC_ALL=C sort)"
PLAN_EXPECTED_CHANGED_FILES="$(printf '%s\n' \
  apps/ios/Sources/Remora/Models/ChatGPTOAuth.swift \
  apps/ios/Tests/RemoraTests/ChatGPTOAuthTests.swift | LC_ALL=C sort)"
test "$PLAN_CHANGED_FILES" = "$PLAN_EXPECTED_CHANGED_FILES"
```

Inspect and commit with exact commands:

```sh
git diff --ignore-submodules=all
git add -- \
  apps/ios/Sources/Remora/Models/ChatGPTOAuth.swift \
  apps/ios/Tests/RemoraTests/ChatGPTOAuthTests.swift
git diff --cached --check
PLAN_STAGED_FILES="$(git diff --cached --name-only | LC_ALL=C sort)"
PLAN_EXPECTED_FILES="$(printf '%s\n' \
  apps/ios/Sources/Remora/Models/ChatGPTOAuth.swift \
  apps/ios/Tests/RemoraTests/ChatGPTOAuthTests.swift | LC_ALL=C sort)"
test "$PLAN_STAGED_FILES" = "$PLAN_EXPECTED_FILES"
git diff --cached
git commit -m 'ios: bind and bound OAuth callback listener'
test "$(git log -1 --format=%s)" = 'ios: bind and bound OAuth callback listener'
PLAN_COMMIT_FILES="$(git diff-tree --no-commit-id --name-only -r HEAD | LC_ALL=C sort)"
test "$PLAN_COMMIT_FILES" = "$PLAN_EXPECTED_FILES"
test -z "$(git status --short --ignore-submodules=all --untracked-files=all)"
```

## Acceptance criteria

- Listener requires the configured IPv4 loopback endpoint and constructs
  without `EINVAL`.
- Four in-flight permits are replenished exactly once per terminal client.
- Each client has a five-second header deadline.
- Request storage never exceeds 16 KiB.
- Only a complete strict-UTF-8 origin-form GET/HTTP1.1 request for the exact
  callback path can produce a callback.
- Invalid clients do not terminate the OAuth wait.
- Focused tests and the fast simulator build pass.
- Exactly two files are committed; no dependency/generated file is staged.

## Execution record

- Exact Plan 002 base `566a584` was established before implementation.
- Focused parser, permit, listener, split-header, size-limit, EOF, deadline,
  and invalid-client tests passed.
- The listener requires `127.0.0.1`, accepts at most four in-flight clients,
  caps headers at 16 KiB, uses five-second per-client deadlines, and restores a
  permit at most once on every terminal path.
- `make ios-sim-fast` passed against simulator
  `64C4CECF-EF8E-4175-B9A7-FC67A3EE340A` with only `Assets.xcassets` excluded
  through `XCODE_EXTRA_ARGS` to avoid the external Xcode 26.6 FIFO defect.
- Exact two-file implementation committed as `c4df344`; no dependency,
  generated binding, redirect, provider contract, or persistence format changed.

## STOP conditions

- Plan 002 SHA is unresolved, drift exists, or its focused iOS baseline fails.
- Listener construction uses both `requiredLocalEndpoint` and `on:`.
- A terminal client path cannot be proven to restore at most one permit.
- Binding requires changing the public redirect or OAuth provider contract.
- Bounds require a new dependency or general HTTP server.
- Red output lacks both a planned missing symbol and expected compiler text.
- Any post-edit test, build, scope, or commit check fails twice after correcting
  only an environment/command typo.

## Rollback

Revert the single implementation commit. No persistence, credential, or
protocol format changes.
