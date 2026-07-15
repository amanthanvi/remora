# Remora

Remora is a native iOS and Android client for
[Codex](https://github.com/openai/codex). It connects to local or remote Codex
servers, manages sessions, and shares mobile runtime logic through a Rust core.
See [CONTEXT.md](CONTEXT.md) for the product boundary and architecture glossary.

This repository contains the app sources, shared runtime, developer build
tooling, and the self-hostable Remora relay foundation. Store distribution
automation and managed service deployment are intentionally not included.

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
Swift and Kotlin stay thin: UI, permissions, native audio, and platform APIs.
Session state, streaming, hydration, discovery, and auth logic belong in Rust.

## Supported Scope

Remora supports embedded and remote Codex app-server sessions, ChatGPT OAuth,
WebRTC voice, and remote terminals over SSH or paired hosts rendered by Ghostty.
The embedded app-server is not an on-device shell: Remora does not bundle
an on-device Linux wrapper, Alpine rootfs, or proot. Opaque background wakeups
are reconciled against durable state; notification payloads are never a source
of truth or an approval surface. Watch, CarPlay, the full Live Activity
extension, and store-release automation remain out of scope.

New remote pairing uses Remora Link. During development, build the native host
from the pinned maintenance fork; the `npx remora-link` launcher will become the
default bootstrap after its first trusted npm publication.

```bash
git clone https://github.com/amanthanvi/alleycat.git remora-link-host
cd remora-link-host
cargo run -p remora-link
```

## License

GPLv3 with the repository's additional distribution permission; see
[LICENSE](LICENSE).
