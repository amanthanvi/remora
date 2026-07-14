# iOS Quickstart

See [CONTEXT.md](../../CONTEXT.md) for the supported product boundary and shared
runtime glossary.

## Prerequisites

- Xcode.app
- xcodegen (`brew install xcodegen`)
- Rust toolchain (`rustup`)
- Zig (`brew install zig`; CI pins 0.15.2)
- Optional: sccache (`brew install sccache`) for faster Rust rebuilds

## Build with Make (recommended)

```bash
# Full iOS package build + simulator
make ios

# Fast simulator lane
make ios-sim-fast

# Fast device lane
make ios-device-fast

# Build + open Xcode
make ios-run
```

This handles submodule sync, patching, UniFFI bindings, Rust
cross-compilation, Ghostty renderer artifacts, Xcode project generation, and
the Xcode build, with stamp caching for repeated runs.

The iOS lanes build Remora, `codex-mobile-client`, and Ghostty. They do not
download or package an on-device Linux environment; terminal sessions require
SSH or a paired remote host.

## Build manually (step by step)

1. Sync Codex submodule + apply iOS patch:
   - `./apps/ios/scripts/sync-codex.sh`
   - This preserves the current submodule checkout by default. Use `--recorded-gitlink` only if you want to reset to the commit recorded in the parent repo.
2. Build Rust bridge XCFramework:
   - `./apps/ios/scripts/build-rust.sh`
   - Add `--with-intel-sim` only if you need an Intel Mac simulator slice.
3. Build the Ghostty renderer:
   - `./apps/ios/scripts/build-ghostty.sh`
4. Generate the project:
   - `./apps/ios/scripts/regenerate-project.sh`
5. Build app:
   - `xcodebuild -project apps/ios/Remora.xcodeproj -scheme Remora -configuration Debug -destination 'generic/platform=iOS Simulator' build`

## Configuration

Override via environment variables:

- `IOS_SIM_DEVICE="iPhone 16"` — change simulator target
- `XCODE_CONFIG=Release` — use the Release build configuration
