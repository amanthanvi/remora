# Remora

Remora is a native iOS and Android client for
[Codex](https://github.com/openai/codex). It connects to local or remote Codex
servers, manages sessions, and shares mobile runtime logic through a Rust core.
See [CONTEXT.md](CONTEXT.md) for the product boundary and architecture glossary.

This repository contains the app sources, shared runtime, developer build
tooling, and the self-hostable Remora relay foundation. Store distribution
automation and managed service deployment are roadmap work not yet implemented
in this checkout.

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
The architectural target keeps Swift and Kotlin thin and puts session state,
streaming, hydration, discovery, reconnect, reducer, and shared auth policy in
Rust. The current checkout has the canonical Rust store/reconnect/reducer but
remains transitional: native `AppModel` caches and stream/projection merges,
duplicate provider-label inference, and native OAuth flows remain. Full
thin-shell convergence is Planned.

## Security

The [command-center threat model](docs/security/remora-threat-model.md)
distinguishes implemented controls from release requirements for planned
command-center features. The narrower implemented Remora Link boundary is
documented in the [Remora Link threat model](docs/research/remora-link-threat-model.md)
and [pairing v2 security architecture](docs/research/pairing-v2-security.md).

## Supported Scope

Remora supports embedded and remote Codex app-server sessions, ChatGPT OAuth,
WebRTC voice, and remote terminals over SSH or paired hosts rendered by Ghostty.
The embedded app-server is not an on-device shell; terminal sessions always run
on a remote host. Opaque background wakeups are reconciled against durable
state; notification payloads are never a source of truth or an approval
surface. The Android active-turn home widget is a separate current system
surface; on this base it can display prompt, model, context, and tool details,
and its sanitization remains Planned. The full Live Activity extension and
store-release automation are not yet implemented in this checkout. Watch,
complications, CarPlay, Fastlane, and store-feedback triage remain excluded.

New remote pairing uses Remora Link. During development, build the native host
from the reviewed source revision recorded in the Rust lockfile:

```bash
make bootstrap-remora-link REV=<40-character-commit>
```

The bootstrap prefers an in-repository `remora-link-host` checkout and
otherwise fetches the Remora-owned host source recorded in the Rust manifest.
Package-registry launchers are not part of the supported bootstrap path.

## Upgrade Support

Remora 1.6.0 is the security and direct-upgrade floor on both iOS and Android.
Its first launch atomically discards unsupported saved hosts, pairing journals,
transport identities, and signing authority before Remora Link starts. Remote
hosts must then be paired again. No compatibility or migration guarantee
applies to state created by an older build; direct upgrades are supported from
1.6.0 onward.

## License

GPLv3 with the repository's additional distribution permission; see
[LICENSE](LICENSE).
