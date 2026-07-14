# Development Guide

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

- **meson** + **ninja** (required by `webrtc-audio-processing-sys`):

  ```bash
  brew install meson
  ```

- **xcodegen** (for regenerating `Remora.xcodeproj`):

  ```bash
  brew install xcodegen
  ```

- **Zig** (required to build the Ghostty renderer; CI pins 0.15.2):

  ```bash
  brew install zig
  ```

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

4. Fallback: run app-server manually bound to loopback and forward the port over SSH.

   On the Mac:

   ```bash
   codex app-server --listen ws://127.0.0.1:8390
   ```

   Then connect the phone via the `SSH` flow in Discovery — Remora opens the SSH connection, port-forwards `127.0.0.1:8390`, and connects through the tunnel. Do not bind `0.0.0.0` unless you fully understand the exposure; the SSH flow is the supported path.

5. Thread/session listing is `cwd`-scoped. If expected sessions are missing, choose the same working directory used when those sessions were created.

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

```bash
./apps/ios/scripts/build-rust.sh              # package mode (device + sim + xcframework)
./apps/ios/scripts/build-rust.sh --fast-device # raw device staticlib only
```

## Build and Run iOS

Regenerate project if `apps/ios/project.yml` changed:

```bash
make xcgen
```

Open in Xcode:

```bash
open apps/ios/Remora.xcodeproj
```

CLI build:

```bash
xcodebuild -project apps/ios/Remora.xcodeproj -scheme Remora -configuration Debug -destination 'platform=iOS Simulator,name=iPhone 17 Pro' build
```

## Build and Run Android

Prerequisites: Java 17 or newer, Android SDK + build tools for API 35, the
Android NDK, `cargo-ndk`, Rust via rustup, and Zig.

```bash
make android-emulator-fast                              # Rust JNI + debug APK
cd apps/android && ./gradlew :app:testDebugUnitTest    # unit tests
cd apps/android && ./gradlew :app:assembleDebug        # Gradle-only debug assemble
```
