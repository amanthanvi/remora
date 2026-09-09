# Selective upstream adoption and code-quality review

Reviewed 2026-09-07. Base: `4d4f9a9d203a8df89cb465754325cc922950e80a`.
This is the original implementation record. The
[current verification ledger](2026-09-09-risk-gates.md) supersedes the remaining
risks and validation status below; the
[September 8 follow-up](2026-09-08-remora-follow-up.md) records subsequent changes.
Scope: shared mobile runtime hot paths, conversation journeys, native projection,
CI, and developer documentation. This is not an exhaustive security audit,
whole-repository rewrite, or measured performance study.

## Implemented

- Shared model/reasoning normalization at canonical turn dispatch, including
  runtime-specific aliases, inherited models, and collaboration mode settings.
- Shared history pagination fencing and atomic page merge, preserving live
  streamed items instead of replacing them with a read-time snapshot.
- Targeted thread/server projections instead of whole-store clones for thread
  lookup and model routing. No benchmark claim: the avoided clones are visible
  in the implementation; end-to-end impact is not measured.
- Both native hydration helpers fail for the requested conversation instead of
  silently returning a different active thread.
- iOS attachment access during active turns, matching Android.
- Android observable, request-keyed answer state and accessible option selection
  in both prompt renderers; focused overlay instrumentation regressions.
- JSON-based single-simulator selection with tests; Android lint and isolated
  release JVM checks in CI; a shorter agent guide with conditional pointers to
  authoritative product, security, development, and design documents.
- Pending-request identity carried through Rust, UniFFI, both native clients,
  and the debug CLI/TUI as `(server_id, runtime_kind, request_id)`. Responses
  fail closed when their originating runtime is unavailable.
- Nonblocking remote request polling with 32 outstanding slots, including
  cancelled-but-unanswered requests. Reconnect retires old-connection futures;
  only the existing safe-read allowlist can replay, at most once.
- Typed loss propagation from transport and session queues into the existing
  coalesced authoritative repair worker.
- Native submitted-draft recovery on home and conversation composers. Recovery
  retains attachments and mentions, preserves newer edits, and never resends
  automatically. iOS also preserves newer home edits before navigation.
- Serialized Android projection/cache commits, off-lock native reads,
  navigation intent fencing, and serialized subscription retirement.
- Delayed post-input reads fenced by current session, event generation, and
  history identity. Removed or rolled-back history cannot be resurrected.

## Upstream evidence and choices

Compared parent `0xSero/litter` at
`94b1b04fd27bfc1a996120bc3742b66230dcc866` and T3 Code at
`8588d7f63bf57e6285daafe035708cc22a99619c`. The parent comparison has 85
Remora-only and 129 parent-only commits after merge base
`abee3ace684204a3cbc4ea1e0e903b9f31518dac`; this is not a count of missing features.

| Reference | Decision |
| --- | --- |
| Parent reasoning fixes `7dae398abd5484eeeaebb6b34dc5823fdcf06311`, `98b7279af633742da5e24ec4b56749df79cfecd0` | Adapt behavior in shared Rust dispatch, not two native implementations. |
| Parent attachment fix `648c12449112b5446e1bfa0f4d88e4b8febad0f0` | Restore attachment access while generating on iOS. |
| T3 `packages/client-runtime/src/state/threads.ts` at the pinned revision | Adopt history epochs and late-result rejection in the Rust owner. |
| Parent snapshot coalescer `0824605d51516548a0b1cf4502443a9bf3c3e1e6` | Defer until native projection ownership is serialized and streaming workloads are measured. |
| Parent Watch/CarPlay/local shell/Local Studio/store automation | Excluded by Remora product scope; no bulk merge. |
| T3 navigation/source review/terminal supervision patterns | Remora already has equivalents; retain native workflows and investigate specific gaps instead of adding duplicate systems. |

Reference repositories: <https://github.com/0xSero/litter>,
<https://github.com/pingdotgg/t3code>. Prose cleanup followed
<https://github.com/cursor/plugins/blob/main/pstack/skills/unslop/SKILL.md>.
No upstream ref, product identifier, bundle identifier, or signing identity was
changed. Builds applied the repository's existing local submodule patches.

## Addressed findings

The five findings approved after the HTML review are implemented. Independent
review also found and corrected unhydrated approval cleanup, overlapping-page
replacement, cancelled-request accounting, old-connection retention, Android
activation/cache races, and iOS home handoff draft loss. No production incident
or end-to-end exploit is claimed.

| Priority | Original finding | Resolution | Record |
| --- | --- | --- | --- |
| P1 | ID-only pending insertion, resolution, and response lookup | Server/runtime/request identity at every boundary; server-local removal even without hydrated threads | [001](../../plans/001-request-identity.md) |
| P1 | Remote worker awaited RPCs inline and hid early event loss | Bounded concurrent polling, connection retirement, typed loss, existing repair gate | [002](../../plans/002-event-pump.md) |
| P1 | Failed sends lost drafts or overwrote newer edits | Separate submitted drafts; explicit, context-scoped recovery on both platforms | [003](../../plans/003-recoverable-drafts.md) |
| P1 | Concurrent Android snapshot/cache writers | Serialized native projection owner, fenced reads/navigation, cache invalidation | [004](../../plans/004-android-projection-owner.md) |
| P1 | Delayed post-input refresh unconditionally applied old reads | Atomic session/event/history checks, weak ownership, bounded polling | [005](../../plans/005-post-input-refresh.md) |

Rust evidence paths above are under
`shared/rust-bridge/codex-mobile-client/src/`. The plans include exact paths,
implementation decisions, regression cases, and remaining validation limits.

## Direction, not defects

1. Retain the native interrupted-work recovery and shared request/event ownership
   established here when adding future conversation journeys.
2. Measure streaming projection cost after fixing Android ownership. Compare
   allocations, frame times, and update counts before adopting batching; keep
   approvals, completion, and terminal output responsive.

## Validation record

| Command | Result |
| --- | --- |
| `make rust-test` | 1,114 passed, 0 failed, 4 live-environment tests ignored; shell and PowerShell checks passed |
| `make bindings` | Swift/Kotlin bindings regenerated and mobile Rust compiled |
| `cargo check -p codex-mobile-client -p codex-debug-cli -p codex-tui` from `shared/rust-bridge` | Passed for the changed client contracts; subsequent transport changes compiled in bindings and native builds |
| `make ci-tools-test bindings-hardener-test` | 2 simulator-selection and 17 binding-hardening tests passed |
| `./apps/ios/scripts/regenerate-project.sh` | Xcode project regenerated, including new recovery source/tests |
| `make ios-sim-fast test-ios` | Simulator build passed; 261 tests passed |
| `make android-emulator-fast` | Final Rust JNI library and debug APK built |
| `apps/android/gradlew -p apps/android :app:testDebugUnitTest :app:lintDebug :app:assembleDebug` | 239 tests passed; build passed; lint has 0 errors, 29 SDK/dependency warnings, 8 hints |
| `apps/android/gradlew -p apps/android :app:testReleaseUnitTest` | 243 tests passed with explicit, non-provider Firebase fixtures |
| `apps/android/gradlew -p apps/android :app:connectedDebugAndroidTest -Pandroid.testInstrumentationRunnerArguments.class=com.remora.android.ui.conversation.UserInputCardTest` | 3 tests passed on API 35 emulator; final run used normal debug configuration without Firebase fixtures |
| `git diff --check` | Passed |

Android commands used JDK 21 at
`/Library/Java/JavaVirtualMachines/jdk-21.jdk/Contents/Home` and SDK root
`/Users/amanthanvi/Library/Android/sdk`. Release JVM fixtures were
`REMORA_FIREBASE_APP_ID=1:1234567890:android:remora-ci`,
`REMORA_FIREBASE_PROJECT_ID=remora-ci-unit-tests`,
`REMORA_FIREBASE_API_KEY=remora-ci-not-a-provider-key`, and
`REMORA_FIREBASE_SENDER_ID=1234567890`. No release APK was packaged with them.
The final debug build and instrumentation were repeated without those fixtures.

Installed the exact final simulator app from DerivedData with `xcrun simctl
install`, then launched `com.remora.app` on iPhone 17 Pro. Installed the final
debug APK with `adb -e install -r` and launched
`com.remora.android/com.remora.android.MainActivity`. Both launch checks passed;
screenshots were inspected. This does not verify provider connectivity or draft
recovery through a real delayed network failure.

Independent final source review found no remaining issue in Android navigation
fencing. Its regressions cover coalesced activation events, missing events,
same-subscription resync, repeated A/B/A selection, and newer canonical navigation.

## Changed files

- Shared Rust: `mobile_client/{event_loop,runtime_routing,store_listener,tests,thread_operations,user_input}.rs`,
  `session/{connection,events}.rs`, `store/{activity,boundary,reconcile,reducer}.rs`,
  `store/reducer/tests.rs`, `types/server_requests.rs`, and `ffi/app_store.rs`.
- Debug clients: `codex-debug-cli/src/main.rs` and `codex-tui/src/app.rs`.
- Android state: `AppModel.kt`; new `AppModelNavigationIntent.kt`,
  `AppModelProjectionOwner.kt`, `AppModelThreadSnapshotCache.kt`, and
  `ComposerDraftRecoveryStore.kt`, with corresponding JVM tests.
- Android UI: `RemoraApp.kt`, `ApprovalOverlay.kt`, `ComposerBar.kt`,
  `ConversationScreen.kt`, `HomeComposerBar.kt`; new `RecoverableDraftsRow.kt`
  and `UserInputCardTest.kt` instrumentation.
- iOS: `ContentView.swift`, `AppModel.swift`, `AppState.swift`,
  `ConversationBottomChrome.swift`, `ConversationComposerEntryRowView.swift`,
  `ConversationInputBar.swift`, `ConversationView.swift`, `HomeComposerView.swift`;
  new `ComposerRecoveryStore.swift`, `ComposerRecoveryMenu.swift`, and recovery
  tests; updated observation/snapshot tests and regenerated Xcode project.
- Build/docs: `.github/workflows/mobile-ci.yml`, `Makefile`, simulator selection
  script/tests, `tools/ci/README.md`, `AGENTS.md`, `CONTEXT.md`, `CONTRIBUTING.md`,
  `docs/DEVELOPMENT.md`, Android QA matrix, this review, and `plans/001` through
  `005` with their index. The original HTML review is updated locally.

## Historical risks at the September 7 checkpoint

The [September 8 follow-up](2026-09-08-remora-follow-up.md) supersedes the
process-lifetime, unmeasured-projection, and dependency-upgrade limitations below.
This section records the earlier checkpoint, not the current implementation.

- Submitted-draft recovery is process-lifetime only; process termination loses
  it. Recovery restores editable contents, not an exact-payload retry, and never
  proves whether an uncertain submission reached the provider.
- Sustained streaming during rapid navigation/backgrounding, live delayed-send
  recovery with attachments, inline question submission, and physical-device
  behavior remain manual QA. Four live-environment Rust tests remain ignored.
- Performance changes remove identified blocking/cloning paths, but latency,
  allocations, and frame times were not benchmarked. SDK/dependency upgrades
  reported by lint are outside the five approved findings.
- Generated bindings/libraries remain local. Codex and Ghostty have the existing
  build-applied local patches; neither submodule gitlink moved. No commit, push,
  deployment, signing identity change, or bundle identifier change was made.
