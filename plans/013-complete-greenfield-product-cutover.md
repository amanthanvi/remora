# Plan 013: Complete the greenfield obsolete-state cutover

Status: **DONE**

Baseline: `fb9919437f3e837fe8e728195f52b2ea870d4b1b`

Tracker: [#23](https://github.com/amanthanvi/remora/issues/23)

## Outcome

Removed the obsolete product surfaces named by the accepted greenfield plan:

- Saved Apps and their persistence, routes, update flows, and state bridges;
- thinking-indicator minigames;
- generated-HTML actions, hydration, WebViews, and native script bridges;
- dedicated aggregate server usage/token/model/activity dashboards.

Generic typed tool calls, per-thread context visibility, actionable rate limits,
voice, terminal, discovery, and the sanitized Android status/count widget remain.

## Cutover

Both clients run a one-time product-state rebuild after the existing security
cutover and before normal runtime initialization. It removes only obsolete
mobile preferences, Saved Apps files, and the retired minigame override.
Credentials, pairing material, transport identity, Host trust, and unrelated
files are outside the deletion set. Completion is durable and the user sees one
non-blocking `Local workspace rebuilt` notice.

## Acceptance evidence

- `make bindings`: passed; Swift/Kotlin sensitive-carrier hardening passed.
- `cargo test -p codex-mobile-client`: 1,052 passed, 0 failed, 4 intentional
  live-environment ignores.
- `XCODE_EXTRA_ARGS='EXCLUDED_SOURCE_FILE_NAMES=Assets.xcassets' make ios-sim-fast`:
  passed with regenerated simulator static library and app link.
- Full iOS XCTest: 264 passed, 0 failed, 0 skipped.
- `make android-emulator-fast`: passed with rebuilt arm64 JNI and APK.
- Android JVM tests: 234 passed, 0 failed, 0 skipped.
- Android instrumentation: 29 passed, 0 failed.
- Android rebuilt APK: installed; cold launch succeeded in 941 ms; no fatal,
  native-link, or Rust panic signatures.
- iOS rebuilt app: installed and launched; process remained live with no
  simulator error/fault entries.
- `git diff --check`: passed.
- Retired-symbol search: only cutover tombstones, deletion assertions, and
  threat-model statements remain.

## Rollback and STOP conditions

Rollback is the single Plan 013 commit. Stop release if bindings fail to
regenerate, either client fails to build, a deleted route or bridge remains
reachable, the cutover touches a security store, or any native/runtime suite
regresses.

## Known environment constraint

Xcode 26.6's `AssetCatalogSimulatorAgent` FIFO defect remains external to the
repository. The documented validation-only asset-catalog exclusion was used;
production project inputs were not changed.
