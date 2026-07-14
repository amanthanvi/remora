# Remora

Remora is a native iOS and Android client for
[Codex](https://github.com/openai/codex). It connects to local or remote Codex
servers, manages sessions, and shares mobile runtime logic through a Rust core.

This repository keeps the app build surfaces. Branded distribution wrappers,
marketing site assets, and signup funnels are intentionally not part of this
fork.

## Quick Start

```bash
make ios-device-fast       # fast iOS device build
make ios-sim-fast          # fast iOS simulator build
make android-emulator-fast # fast Android emulator build
```

For machine prerequisites and the full build matrix, see
[docs/DEVELOPMENT.md](docs/DEVELOPMENT.md).

## Repository Layout

```text
apps/ios/                  iOS app; current scheme/path names use Remora
apps/android/              Android app; Compose UI and Gradle build
shared/rust-bridge/        Shared Rust client crate and UniFFI bindings
shared/third_party/codex/  Upstream Codex submodule
patches/codex/             Local Codex patch set applied during builds
tools/scripts/             Build and maintenance helper scripts
services/push-proxy/       Runtime push infrastructure retained for now
```

## Common Targets

| Target                       | Description                                              |
| ---------------------------- | -------------------------------------------------------- |
| `make ios-device-fast`       | Fast iOS device build using raw staticlib outputs        |
| `make ios-sim-fast`          | Fast iOS simulator build                                 |
| `make ios`                   | Full iOS package lane                                    |
| `make android-emulator-fast` | Fast Android emulator build                              |
| `make android`               | Full Android pipeline                                    |
| `make rust-check`            | Host `cargo check` for shared Rust crates                |
| `make rust-test`             | Host `cargo test` for shared Rust crates                 |
| `make bindings`              | Regenerate UniFFI Swift and Kotlin bindings              |
| `make xcgen`                 | Regenerate the Xcode project from `apps/ios/project.yml` |
| `make clean`                 | Remove build artifacts                                   |

## Architecture

Both platforms share `codex-mobile-client` through UniFFI-generated bindings.
Swift and Kotlin stay thin: UI, permissions, notifications, and platform APIs.
Session state, streaming, hydration, discovery, and auth logic belong in Rust.

Remote pairing should use upstream Alleycat, for example:

```bash
npx kittylitter
```

## License

GPLv3 with the repository's additional distribution permission; see
[LICENSE](LICENSE).
