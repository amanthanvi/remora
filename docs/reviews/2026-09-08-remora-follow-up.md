# Remora follow-up: recovery, history, and dependencies

Date: 2026-09-08. Base: `4d4f9a9d203a8df89cb465754325cc922950e80a`.
Continues the five implemented findings in the
[original architecture review](2026-09-07-upstream-adoption.md).
See the [September 9 risk gates](2026-09-09-risk-gates.md) for the latest
verification-only changes and refreshed external prerequisites.

## Implemented

- Durable native submitted-draft recovery. Both platforms commit before
  dispatch/clearing, restore interrupted sends as unconfirmed, retain newer
  edits and attachment/mention content, and offer confirmed deletion. No
  automatic retry or inference that a timeout means failure.
- iOS protected, excluded-from-backup atomic storage; Android Keystore AES-GCM
  custody under `noBackupFilesDir`, file/directory synchronization, and atomic
  replacement. Damaged or unsupported archives are not silently overwritten.
- Legacy pagination and scheduled authoritative repair reject responses made
  stale by streamed events, replaced sessions, or rollback. The existing
  bounded/coalesced repair worker retries rejected loads.
- Pending-request lookup and runtime metadata routing project only the needed
  owned records. Callers do not clone an entire conversation or app snapshot.
- Removed unused `russh-keys` and Media3 transformer dependencies. Updated
  compatible Rust security patches, removed yanked pinned versions, and added
  a reproducible Codex patch for current XML parsing and cache dependencies.
- Android libraries, Kotlin/Compose compiler, AGP, Gradle, and compile SDK
  updated together. Release resource generation and release JVM variants are
  explicitly enabled. Billing adapts the current product-details result type.
- Android layout reads the actual window size and configuration-aware
  resources. Xcode project generation uses XcodeGen's source-aware cache.
- Imported RSA private keys fail before connection or signing; Ed25519/ECDSA,
  password authentication, and RSA public host trust remain supported. The
  retained MCP HTTP test fixture now validates Host before all routes.
- Recovery load, encoding, encryption, atomic writes, and synchronization now
  run off the UI executor: an isolated transaction actor on iOS, and
  `Dispatchers.IO` plus one mutex on Android. Both platforms retain
  save-before-clear/send ordering and fence newer editor/context changes.
- Cancellation after an Android durable begin produces an explicit
  unconfirmed entry without dispatch. Restoring a draft keeps its durable
  source and preserves displaced edits even when the UI task is cancelled.
- Image compression/base64 preparation is also off MainActor on iOS; Android
  payload base64 preparation is off Main. No extra cache or persistence layer.
- Removed the TUI's affected LRU dependency using ratatui 0.30.2/crossterm 0.29.
  Excluded MCP HTTP server transport from normal mobile builds behind an
  explicit fixture feature. Added a real RSA-host/Ed25519-client handshake
  regression and fixed default-port HTTP fixture authorities.

## Upstream decisions

Refreshed the parent fork and T3 reference repositories on September 8.
The parent remains `94b1b04fd27bfc1a996120bc3742b66230dcc866`.
T3 advanced to `6ba15c027a6d411c7f75cc0ca59b601a295c3633`, ten commits after
the previous comparison. Its new email-label hiding does not require a duplicate
Remora usage-label system: usage badges do not include account emails. Desktop
media transfer, preview recording, review labels, and KDE fixes are not patches
for the native mobile runtime. Retain the already-adapted history-fencing,
targeted state projection, and interrupted-work recovery patterns.

No wholesale merge, runtime replacement, bundle identifier, signing change,
commit, or push. New Codex changes are represented by
`patches/codex/dependency-security-updates.patch` and
`patches/codex/rmcp-test-server-host-validation.patch`, plus
`patches/codex/rmcp-http-test-feature.patch`; both submodule gitlinks
remain unchanged.

## Performance evidence

Local debug microbenchmark, 16 items per thread, 4,096 text bytes per item.
Seven alternating batches of 200 reads after warmup; table values are median
microseconds per operation. Equivalence is checked outside the timed loop.

| Threads | Thread clone/select -> projection | Snapshot -> approval lookup | Snapshot -> input lookup | Thread clone -> routing tuple |
| --- | --- | --- | --- | --- |
| 1 | 15.98 -> 9.57 | 4.79 -> 0.164 | 5.09 -> 0.219 | 3.75 -> 0.476 |
| 16 | 90.21 -> 8.44 | 80.08 -> 0.191 | 79.56 -> 0.233 | 4.23 -> 0.449 |
| 64 | 463.76 -> 8.65 | 379.42 -> 0.179 | 376.39 -> 0.235 | 4.40 -> 0.489 |

Reproduce with `cargo test -p codex-mobile-client
targeted_projection_and_dispatch_reads_benchmark -- --ignored --nocapture
--test-threads=1` from `shared/rust-bridge`. This is not a release/device
benchmark, allocation study, or UI frame-time measurement. No timing threshold
is asserted in CI.

### Native recovery responsiveness

| Measured local workload | Before UI stall | After maximum heartbeat gap | Total work after |
| --- | --- | --- | --- |
| iOS 4096x3072 PNG preparation including base64 | 53.44 ms | 6.07 ms | 53.77 ms |
| iOS 4096x3072 JPEG preparation including base64 | 17.21 ms | 6.05 ms | 17.24 ms |
| Android 6 MiB attachment, real Keystore, 16,944,791-byte journal | 1,635 ms | 19 ms | 2,711 ms including reload |

The iOS durable 3072x2048 image write took 29.27 ms for a 647,983-byte archive,
with four main-actor heartbeat ticks and a 9.13 ms maximum observed gap. A
deterministically blocked write also proves that edits continue and concurrent
mutations survive reload. There was no before-change durable-write baseline.

These are simulator/emulator measurements, not physical-device frame-time or
release benchmarks. Moving work off Main improves responsiveness, not total
encoding speed. Android's attachment fixture represents encoded-image
persistence, not JPEG decoding. The instrumentation recovered the journal in
separate processes (PID 4419 then 4462) and rejected replay with zero sends.
Evidence: `/tmp/remora-ios-recovery-result.md`,
`/tmp/remora-android-recovery-performance.txt`, and the native test suites.

## Initial validation

| Command / gate | Result |
| --- | --- |
| `make rust-test` | 1,120 passed, zero failed, five ignored: four live-environment cases and the separately run benchmark |
| Projection benchmark command above | Passed; medians recorded above |
| `make rebuild-bindings` | Passed; native builds consumed regenerated bindings |
| `cargo check --manifest-path shared/rust-bridge/Cargo.toml -p codex-debug-cli -p codex-tui` | Passed |
| `cargo test --manifest-path shared/rust-bridge/Cargo.toml -p codex-mobile-client --test protocol_xml` | Passed; patched hook XML roundtrip and malformed input |
| `make ci-tools-test bootstrap-remora-link-test bindings-hardener-test` | Passed, including source-inventory XcodeGen cache regression |
| `make ios-sim-fast test-ios` | Passed; 273 XCTest cases |
| `make android-emulator-fast` | Passed; final Rust JNI and debug APK |
| Android `:app:testDebugUnitTest :app:testReleaseUnitTest` | 250 debug and 254 release JVM cases passed |
| Android `:app:lintDebug :app:assembleDebug` | Passed; zero lint errors, two SDK/version warnings and eight autoboxing hints |
| Android `:app:connectedDebugAndroidTest` filtered to `UserInputCardTest` and then `ComposerDraftRecoveryJourneyTest` | Three prompt UI cases and one real-Keystore recovery case passed on API 35 |
| `git diff --check` | Passed |
| `cargo test --offline --manifest-path /tmp/remora-rmcp-host-tests.AMV3vu/Cargo.toml --bin test_streamable_http_server` | Both embedded Host tests passed against the actual patched fixture source; isolated harness pins direct versions but resolves its own transitive graph |
| `git -C shared/third_party/codex apply --reverse --check` for both new patches | Passed; retained changes are represented by applicable patches |
| `bash -n apps/ios/scripts/sync-codex.sh apps/ios/scripts/regenerate-project.sh` | Passed |
| `cargo fmt --manifest-path shared/rust-bridge/Cargo.toml --all -- --check` | Passed after mechanical cleanup; stable rustfmt still warns about upstream's nightly-only import-granularity setting |

Android Gradle commands use JDK 21. Release JVM tests use non-provider
`REMORA_FIREBASE_*` resource fixtures, not production credentials or release
delivery validation. Run Gradle verification serially with native rebuilding.

Installed the exact final simulator app with `xcrun simctl install`, launched
`com.remora.app`, and captured `/tmp/remora-ios-final-20260908.png`. Installed
the exact debug APK with `adb -s emulator-5554 install -r`, launched
`com.remora.android/.MainActivity`, and captured
`/tmp/remora-android-final-20260908.png`. Both home screens rendered. The checked
Android crash log was empty; iOS logged simulator launch-measurement errors,
with no observed app crash or cutover failure. This smoke check does not prove
an upgrade from every supported persisted state.

Final build/test logs are `/tmp/remora-rust-verified-20260908.log`,
`/tmp/remora-ios-verified-20260908.log`,
`/tmp/remora-android-native-final-20260908.log`,
`/tmp/remora-android-verified-20260908.log`, and
`/tmp/remora-android-recovery-final-20260908.log`.

The initial iOS test asserted a data-protection class that simulators do not
expose. Only that class assertion is device-only now; backup exclusion and
permissions remain asserted on the simulator. The final 273-case run passed.
An overlapping Gradle/native build produced transient unresolved classes; the
final serial native and instrumentation runs passed. Retained native tooling
still emits a non-fatal `rust-objcopy`/libLLVM warning and generated Kotlin
warnings; builds are not claimed warning-free.

The final cleanup after native verification changed only Rust formatting and
documentation. The shared 1,120-case suite was rerun after bridge formatting.
Three retained Codex files also received formatting-only changes; their exact
sections in `realtime-client-controlled-handoff.patch`,
`remote-app-server-websocket-cap.patch`, and `realtime-webrtc-env-apikey.patch`
were regenerated and reverse-checked. Other patch sections were preserved.
Final workspace formatting and whitespace checks passed. Native builds were
not repeated solely for those formatting-only changes.

The self-contained visual review is
`/tmp/architecture-review-20260908-remora.html`. The local server returned HTTP
200, but the collaborative browser navigated to `chrome-error://chromewebdata/`
and screenshot automation failed on both direct and environment-port attempts.
The report's final visual layout is therefore not browser-verified in this pass.
Native screenshots were captured and inspected independently.

## Final risk-follow-up validation

These results supersede the initial counts above. Native apps were rebuilt
against the final Rust bridge before installation and smoke verification.

| Command / gate | Result |
| --- | --- |
| `make rust-test` | 1,121 passed, zero failed, five ignored; includes real RSA-host KEX with Ed25519 client authentication |
| `cargo check --locked --offline --manifest-path shared/rust-bridge/Cargo.toml -p codex-tui --all-targets` | Passed with aligned ratatui 0.30.2/crossterm 0.29 |
| `cargo test --locked --offline --manifest-path shared/rust-bridge/Cargo.toml -p codex-tui --bin codex-tui` | Repeated-resize rendering regression passed |
| `cargo test --locked --offline --manifest-path /tmp/remora-rmcp-host-tests.AMV3vu/Cargo.toml --bin test_streamable_http_server` | All three Host regressions passed, including default port 80 |
| `make ios-sim-fast` and `make android-emulator-fast` | Passed; final Swift and JNI consumers rebuilt |
| `make test-ios` | 277 tests passed against the rebuilt bridge; 23 focused tests passed again after timing-only test cleanup |
| Android `:app:testDebugUnitTest :app:testReleaseUnitTest :app:lintDebug` | 254 debug and 258 release tests passed; lint: zero errors, two warnings, eight hints |
| Android `:app:assembleDebugAndroidTest`, exact APK installation, focused `am instrument` | Passed; final real-Keystore off-main/reopen test passed, zero failures |
| `cargo fmt --manifest-path shared/rust-bridge/Cargo.toml --all -- --check` | Passed; existing nightly-only import setting warnings remain |
| `git diff --check` and reverse checks for both rmcp patches | Passed |
| Refreshed `cargo-audit audit --json --no-fetch` | Exit 1: five vulnerability entries and three unmaintained notices; neither LRU warning remains |

Timing measurements have no scheduler-dependent heartbeat-count threshold.
Payload assertions and deterministic blocked-write/off-main checks remain.
The exact iOS debug app was installed, its debug-library digest matched the
tested product, and its relaunched home screen showed a connected local server.
No new Remora crash report or panic was found; nonfatal simulator diagnostics
and unauthenticated plugin-sync warnings remain. Screenshot:
`/tmp/remora-ios-final-home-20260908.png`.

The exact final Android APK also launched with a connected local server and
the `completed_1_6` cutover marker. Its startup log contained no fatal exception,
native loading failure, security exception, or recovery-storage failure. The
final large-attachment test passed in 5.863 seconds; its informational sample
measured 2,836 ms including reload and a 20 ms maximum Main heartbeat gap.
The earlier separate-process recovery evidence remains distinct from this
final in-process reopen test. Screenshot: `/tmp/remora-android-final-home.png`.
Both final screenshots were inspected. The owned Android emulator was stopped;
the iOS app remains open on the existing simulator. No build/test command is
left running.

Final evidence:

- `/tmp/remora-risk-rust-tests-final-20260908.log`
- `/tmp/remora-risk-tui-check-final-20260908.log`
- `/tmp/remora-risk-tui-final-20260908.log`
- `/tmp/remora-risk-fixture-final-20260908.log`
- `/tmp/remora-risk-ios-build-20260908.log`
- `/tmp/remora-risk-android-build-20260908.log`
- `/tmp/remora-ios-final-bridge-tests-20260908.log`
- `/tmp/remora-ios-final-review-tests-20260908.xcresult`
- `/tmp/remora-android-final-debug-jvm-lint.log`
- `/tmp/remora-android-final-release-jvm.log`
- `/tmp/remora-android-final-instrumentation-build.log`
- `/tmp/remora-android-final-keystore-instrumentation.log`
- `/tmp/remora-android-final-keystore-performance.txt`
- `/tmp/remora-android-final-smoke-logcat.txt`
- `/tmp/remora-risk-audit-20260908.json`

## Risk follow-up files

This pass additionally changes:

- Shared dependencies: `shared/rust-bridge/Cargo.toml`, `Cargo.lock`,
  `codex-tui/Cargo.toml`, `codex-tui/src/app.rs`,
  `codex-tui/src/screens/home.rs`, and `codex-mobile-client/src/ssh/tests.rs`.
  The unused crossterm 0.28 fork override was removed; the retained Codex
  workspace's own configuration is unchanged.
- iOS: `Models/ComposerRecoveryStore.swift`,
  `Models/ConversationAttachmentSupport.swift`, `Views/ConversationInputBar.swift`,
  `Views/HomeComposerView.swift`, `Views/ConversationView.swift`,
  `Views/ComposerRecoveryMenu.swift`, and both owning XCTest files.
- Android: `state/ComposerDraftRecoveryStore.kt`, `state/AppModel.kt`,
  `ui/conversation/ComposerBar.kt`, `ui/home/HomeComposerBar.kt`, both recovery
  JVM test files, `ComposerDraftRecoveryJourneyTest.kt`, and
  `ComposerDraftRecoveryResponsivenessTest.kt`.
- Retained Codex: `codex-rs/rmcp-client/Cargo.toml` and
  `src/bin/test_streamable_http_server.rs`; their two tracked patches,
  `apps/ios/scripts/sync-codex.sh`, and `patches/codex/README.md`.
- Reports: this record, `2026-09-08-dependency-security.md`,
  `apps/android/docs/qa-matrix.md`, and the local HTML review.

## Initial changed files

- Runtime: `src/mobile_client/`, `src/store/`, `src/session/`, `src/ffi/app_store.rs`,
  `src/types/server_requests.rs`, `src/ssh/`, and `tests/protocol_xml.rs` in
  `shared/rust-bridge/codex-mobile-client`; bridge manifests/lockfile and CLI/TUI
  consumers. Shared tests cover identity, bounded dispatch, history fencing,
  narrow projections, model reasoning normalization, and SSH policy.
  Formatting-only cleanup also touches `src/ffi/ssh.rs` and
  `src/remote_host_pairing/remora_link_v2/mod.rs`.
- Android: `state/AppModel*`, `state/*ComposerDraftRecovery*`, home/conversation
  composers and recovery controls, approval/runtime presentation, settings
  Billing integration, unit/instrumentation tests, and Gradle configuration.
- iOS: `Models/ComposerRecoveryStore.swift`, AppModel/AppState, both composers,
  `Views/ComposerRecoveryMenu.swift`, conversation observation/chrome, XCTest,
  and the generated Xcode project.
- Build/docs: `Makefile`, mobile CI, iOS generation/sync scripts, `tools/ci/`,
  Codex patch files/README, agent/product-context/development guidance, Android
  QA matrix, `plans/001` through `005`, and the review records.

## Limits

- Recovery restores editable content, not an exact-payload retry. Unconfirmed
  delivery still requires checking conversation history before resending.
  File attachments retain existing remote-path semantics, not copied files.
- Real provider credentials, physical devices, push delivery, and sustained
  streaming/backgrounding journeys are not supplied by repository tests.
  Both registered iPhones were offline and no physical Android device was
  attached. `REMORA_TERMINAL_LIVE_SSH` and release Firebase environment variables
  were unset. No credential values were printed or production credentials
  manufactured; release JVM fixtures are not deployment configuration.
- Draft persistence and image preparation now run off the native UI owner;
  save-before-dispatch ordering remains. Physical-device latency and sustained
  release frame times still need profiling.
- iOS data-protection-class enforcement needs a physical-device test; simulator
  archive/permissions/backup-exclusion tests do not establish hardware custody.
- See the dependency-security review for remaining advisories and reachability;
  a successful build is not a clean security audit.
