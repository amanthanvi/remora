# Shared Scripts

Cross-platform build, run, and desktop-driver helpers live here.

- `build-android-rust.sh`: builds Android Rust bridge JNI libs into `apps/android/core/bridge/src/main/jniLibs`.
- `build-ghostty-android.sh`: builds the pinned Ghostty renderer for Android ABIs.
- `resolve-zig.sh`: selects Zig 0.15.2 exactly, downloading a checksum-pinned
  project-local toolchain when the system Zig is missing or incompatible.
- `codex-app-driver.applescript`: launches `Codex.app`, opens a project root, creates a thread, and pastes/sends prompts through GUI scripting for desktop-side conversation automation.
- `codex-desktop-controller.mjs`: launches or attaches to a remote-debugging-enabled `Codex.app` instance, then drives the real renderer UI through CDP so it can open projects, create threads, send prompts, wait for the turn to finish, and dump the visible transcript as JSON without macOS accessibility scripting.
- `deploy-android-device.sh`: builds Rust JNI libs, assembles the debug APK, installs on a target device (`--serial`/`ANDROID_SERIAL`), and launches the app.
- `loop-ios.sh`: repeats the supported iOS build/run lanes for local iteration.
- `run-android.sh`: installs, launches, and captures Logcat for an emulator or device.
- `switch-app-identity.sh`: switches local app IDs between `com.remora.app` and `com.<your-identifier>.remora` for Android+iOS (`--to your-identifier --identifier <name>`), with optional `--team-id` for iOS signing. For iOS it updates `apps/ios/project.yml` and regenerates `apps/ios/Remora.xcodeproj` via `xcodegen` (no direct `.xcodeproj` edits).
- `update-remora-link.sh`: pins the shared Rust dependencies to a reviewed
  commit from the Remora-owned host source. Set `REMORA_LINK_SOURCE` to verify
  the commit from a local checkout, or `REMORA_LINK_GIT_URL` to override the
  Remora-owned Git remote recorded in the Rust manifest.
- `bootstrap-remora-link.sh`: builds and installs a reviewed Remora Link commit
  from an immutable commit archive. It prefers an in-repository
  `remora-link-host` checkout, accepts `REMORA_LINK_SOURCE` for another local
  checkout, and otherwise clones the Remora-owned Git remote recorded in the
  Rust manifest. Dirty and untracked local files are never installed. The
  default install root is `.local/remora-link`; override it with
  `REMORA_LINK_INSTALL_ROOT`. The default package path is
  `crates/remora-link`; override it with `REMORA_LINK_PACKAGE_DIR`.
- `test-bootstrap-remora-link.sh`: proves local bootstrap installs the exact
  reviewed commit while ignoring dirty tracked and untracked source.

Common `codex-desktop-controller.mjs` flows:

```bash
# Launch a separate Codex instance with Electron remote debugging enabled.
node tools/scripts/codex-desktop-controller.mjs launch \
  --app "/Applications/Codex copy.app" \
  --port 9333 \
  --user-data-dir /tmp/codex-desktop-controller-profile

# Reuse that same instance on later commands. Add --fresh-launch only if you
# intentionally want a brand new app instance on the same port/profile setup.
node tools/scripts/codex-desktop-controller.mjs thread-state --port 9333 --launch

# Attach to an already-running automation instance and inspect the active thread.
node tools/scripts/codex-desktop-controller.mjs thread-state --port 9333

# Send one turn into the current thread, wait for the assistant to finish, and print JSON.
node tools/scripts/codex-desktop-controller.mjs run-turn \
  --port 9333 \
  --message 'Reply with exactly: OK'

# Start a fresh thread in a sidebar project, run the first turn, and print the final transcript.
node tools/scripts/codex-desktop-controller.mjs run-turn \
  --port 9333 \
  --project codex-test \
  --message 'Reply with exactly: OK'
```
