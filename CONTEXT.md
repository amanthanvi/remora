# Remora Context

This glossary defines the product boundary and ownership rules for the native
iOS and Android Codex clients.

## Product Boundary

Remora keeps mobile parity for:

- embedded and remote Codex app-server sessions;
- ChatGPT OAuth;
- WebRTC realtime voice;
- remote SSH and paired-host terminals rendered by Ghostty;
- the `codex-debug-cli` and `codex-tui` developer clients.

The embedded app-server is a retained Codex runtime. It is not a local terminal.
Remora does not bundle an on-device shell, Linux rootfs, or proot.

The repository intentionally excludes hosted push/proxy infrastructure, Watch
and complications, CarPlay, Live Activities, store release/distribution
automation, Fastlane, and store-feedback triage.

## Architecture and Ownership

- Shared state, protocol shaping, hydration, reconciliation, discovery policy,
  SSH policy, remote-terminal state, and voice signaling belong in Rust.
- Swift and Kotlin own UI, permissions, persistence adapters, native
  audio/session APIs, and render-only projections.
- Shared behavior should cross one handwritten UniFFI boundary rather than be
  reimplemented independently on each platform.

## Interop Identity Boundary

Remora is the only product identity. Upstream protocol identifiers remain only
where changing them would break host compatibility:

- `ALLEYCAT_*` constants and the `alleycat/1` ALPN;
- the upstream host-bootstrap command already shown in the README and pairing
  UI;
- precise terminal input protocol terminology in implementation comments.

These are transport details, not UI branding. New product copy, persistence
keys, package names, and symbols use Remora naming.

## Glossary

| Term | Meaning |
| --- | --- |
| `MobileClient` | Top-level internal Rust facade for mobile runtime operations and event routing. |
| `AppStore` | Canonical Rust-owned runtime state, snapshots, subscriptions, reducers, and composite actions. |
| `AppClient` | Public UniFFI client for direct server operations and typed results. |
| `AppModel` | Thin Swift/Kotlin observation shell that projects Rust snapshots into platform UI. |
| `AppState` | Platform-only UI state; never the canonical session/thread/account store. |
| `ThreadKey` | Stable `(serverId, threadId)` identity for a conversation. |
| `DiscoveryBridge` | Rust utility surface for discovery merge, ranking, dedupe, and probing policy. |
| `SshBridge` | Rust utility surface for SSH connection, trust, forwarding, and remote bootstrap. |
| Alleycat | Upstream remote-host pairing and transport protocol. |
| Remote terminal | A shell on a paired or SSH-connected host; there is no on-device terminal backend. |
| Ghostty | Retained renderer/input engine for remote terminal surfaces on iOS and Android. |
| WebRTC voice | Native peer connection and audio processing on each platform, with signaling and shared state in Rust. |
| ChatGPT OAuth | Retained account authorization flow implemented with platform browser/loopback adapters. |
| UniFFI | Generated Swift/Kotlin bindings for the single public `codex-mobile-client` mobile surface. |

## Verification

The minimum cross-platform gate is:

```bash
REMORA_SKIP_ALLEYCAT_UPDATE=1 make rebuild-bindings
REMORA_SKIP_ALLEYCAT_UPDATE=1 make rust-test
REMORA_SKIP_ALLEYCAT_UPDATE=1 make ios-sim-fast
cd apps/android && ./gradlew :app:testDebugUnitTest :app:assembleDebug
```

Branch CI is defined in [`.github/workflows/mobile-ci.yml`](.github/workflows/mobile-ci.yml).
