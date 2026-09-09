# Development Guide

See [CONTEXT.md](../CONTEXT.md) for the supported product boundary, architecture
ownership, and interop terminology.

## Prerequisites

- **Xcode.app** (full install, not only Command Line Tools):

  ```bash
  sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
  ```

- **Rust via rustup** with iOS targets. If Homebrew's `rust` formula is installed, its `cargo`/`rustc` will shadow rustup and break cross-compilation. Either `brew uninstall rust` or ensure `~/.cargo/bin` appears before `/opt/homebrew/bin` in your `PATH`.

  ```bash
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios
  ```

- **xcodegen** (for regenerating `Remora.xcodeproj`):

  ```bash
  brew install xcodegen
  ```

- **Zig 0.15.2** for Ghostty. Build scripts use `tools/scripts/resolve-zig.sh`
  to select this version or download it into the project-local tool cache.
  A different system Zig version does not satisfy this requirement.

Check `xcode-select -p` before an iOS build. It must resolve to the full Xcode
developer directory, not `/Library/Developer/CommandLineTools`. The Makefile
prepends the rustup toolchain to `PATH`; standalone script invocations must
also resolve `cargo` and `rustc` through rustup.

## Connect Your Mac to Remora Over SSH

Use this flow to make Codex sessions from your Mac visible in the iOS/Android app.

1. Enable SSH on the Mac.

   - UI: `System Settings` -> `General` -> `Sharing` -> enable `Remote Login`.
   - CLI:
     ```bash
     sudo systemsetup -setremotelogin on
     ```
   - If you get a Full Disk Access error, grant it to your terminal app in `System Settings` -> `Privacy & Security` -> `Full Disk Access`, then restart terminal and retry.

2. Verify SSH and Codex binaries from a non-interactive SSH shell.

   ```bash
   ssh <mac-user>@<mac-host-or-ip> 'echo ok'
   ssh <mac-user>@<mac-host-or-ip> 'command -v codex || command -v codex-app-server'
   ```

   If the second command prints nothing, install Codex and/or fix shell PATH startup files.

3. Connect from the Remora app.

   - Keep phone and Mac on the same LAN (or same Tailnet).
   - In Discovery: tap a host showing `codex running` to connect directly, or tap an `SSH` host and enter credentials.

   SSH requires an Ed25519 or ECDSA host key. RSA cryptography is excluded from
   supported builds because its implementation has an unresolved timing advisory.
   Use Ed25519, ECDSA, or password authentication; RSA-only hosts must enable a
   supported host key. Existing host trust remains fail-closed.

4. Fallback: run app-server manually bound to loopback and forward the port over SSH.

   On the Mac:

   ```bash
   codex app-server --listen ws://127.0.0.1:8390
   ```

   Then connect the phone via the `SSH` flow in Discovery — Remora opens the SSH connection, port-forwards `127.0.0.1:8390`, and connects through the tunnel. Do not bind `0.0.0.0` unless you fully understand the exposure; the SSH flow is the supported path.

5. Thread/session listing is `cwd`-scoped. If expected sessions are missing, choose the same working directory used when those sessions were created.

Terminal views are remote-only. The in-process Rust app-server remains a
supported Codex runtime and is not a terminal backend.

## Codex Submodule + Patches

Upstream Codex is vendored as a submodule at `shared/third_party/codex`. The
applied patch set and the downstream reason for each patch are documented in
[`patches/codex/README.md`](../patches/codex/README.md).

Sync/apply (idempotent):

```bash
./apps/ios/scripts/sync-codex.sh
```

Pass `--recorded-gitlink` to reset the submodule to the commit recorded in the superproject.

## Build the Rust Bridge

The fast iOS lanes link raw static libraries from `apps/ios/GeneratedRust/`.
The package lane builds device and simulator libraries and creates
`apps/ios/Frameworks/codex_mobile_client.xcframework`. These outputs and
generated bindings are local build artifacts.

```bash
./apps/ios/scripts/build-rust.sh              # package mode (device + sim + xcframework)
./apps/ios/scripts/build-rust.sh --fast-device # raw device staticlib only
./apps/ios/scripts/build-rust.sh --fast-sim    # raw simulator staticlib only
```

Prefer `make ios-sim-fast`, `make ios-device-fast`, and
`make android-emulator-fast` for iteration. Package targets disable incremental
compilation. Fast targets unset `CARGO_INCREMENTAL` because explicitly enabling
it conflicts with the repository's sccache setup. Run `make rebuild-bindings`
when the generated UniFFI output needs to be rebuilt without its stamp cache.

## Build and Run iOS

Regenerate the project after changing its spec or adding/removing source files:

```bash
make xcgen
```

Use this target or `apps/ios/scripts/regenerate-project.sh`. Passing
`--project Remora.xcodeproj` to XcodeGen from inside `apps/ios` creates an
unwanted nested project. XcodeGen's native cache in `.build-stamps` tracks the
source inventory and skips unchanged projects; Make does not use a spec-only
timestamp to decide whether new sources are included.

Open in Xcode:

```bash
open apps/ios/Remora.xcodeproj
```

CLI build:

```bash
xcodebuild -project apps/ios/Remora.xcodeproj -scheme Remora -configuration Debug -destination 'platform=iOS Simulator,name=iPhone 17 Pro' build
```

Install the app from that build's DerivedData directory before testing. An
older installed copy is not evidence for the current source:

```bash
xcrun simctl install booted <DerivedData>/Build/Products/Debug-iphonesimulator/Remora.app
xcrun simctl launch booted com.remora.app
```

## Build and Run Android

Prerequisites: JDK 21, Android SDK platform 37.0, build tools 36.0.0, the
Android NDK, `cargo-ndk`, Rust via rustup, and Zig. The build uses AGP 9.4 with
built-in Kotlin and the committed Gradle 9.6 wrapper. Generated bindings belong
to the Android Kotlin source set, not the Java source set.

```bash
make android-emulator-fast                              # Rust JNI + debug APK
cd apps/android && ./gradlew :app:testDebugUnitTest    # unit tests
cd apps/android && ./gradlew :app:assembleDebug        # Gradle-only debug assemble
```

Use the committed Gradle wrapper. On macOS, the Makefile detects SDK, NDK, and
JDK paths when possible. Override `ANDROID_SDK_ROOT`, `ANDROID_NDK_HOME`, and
`JAVA_HOME` for other installations. `apps/android/app/build.gradle.kts` defines
the SDK/NDK versions; `.github/workflows/mobile-ci.yml` defines CI provisioning.

After building, install and launch the exact APK:

```bash
adb -e install -r apps/android/app/build/outputs/apk/debug/app-debug.apk
adb -e shell am start -n com.remora.android/com.remora.android.MainActivity
```

Keep a simulator and emulator available for shared runtime validation. Inspect
iOS logs in Xcode/device console, Android logs with Logcat, and Rust `tracing`
output locally. There is no log collector or spool directory.

## Verification

For the real paired-host relay path, build the co-owned host and relay, then run
the disposable fixture:

```bash
cargo build --locked --manifest-path services/remora-link/Cargo.toml -p remora-link
cargo build --locked --manifest-path services/remora-relay/Cargo.toml --release
python3 tools/scripts/verify-paired-relay.py
```

The runner uses an isolated HOME/CODEX_HOME, actual Codex and Iroh pairing,
loopback SQLite relay, and synthetic model/push providers. Set
`REMORA_CODEX_BINARY` to the actual package executable when the `codex` launcher
depends on the normal HOME. Fixture processes, invitation, keys, and database
are removed on exit. Failure logs are retained in the system temporary
directory; the runner reports their path. The opt-in Rust test fails when its
fixture is absent, and the runner rejects zero-test success.

Production host settings are documented in
[`services/remora-link/docs/background-relay.md`](../services/remora-link/docs/background-relay.md).
Provider acceptance, device wake scheduling, authoritative repair, and durable
ACK are separate gates. This local runner does not establish physical secure
storage or live APNs/FCM delivery.

Shared client Clippy runs with warnings denied through `make rust-clippy`.
The host has separate macOS, Linux, and Windows CI in
`.github/workflows/host-ci.yml`; Windows ACL tests require native Windows
execution, not only a cross-compile on macOS.

Run the cross-platform gates listed in [CONTEXT.md](../CONTEXT.md). Cheap build
helper checks can run before any native compilation:

```bash
make ci-tools-test bootstrap-remora-link-test bindings-hardener-test rust-shellcheck
```

The branch CI definition is [`.github/workflows/mobile-ci.yml`](../.github/workflows/mobile-ci.yml).
