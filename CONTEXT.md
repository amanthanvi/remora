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
Terminal sessions always run on a remote host.

The repository includes a self-hostable Remora relay foundation and opaque
mobile background-awareness clients. Push is a lossy wake hint over durable,
sequenced Rust-owned state; it never carries prompts, transcripts, credentials,
or approval actions. A managed hosted deployment and provider credentials are
operational concerns outside this checkout. Live Activity support remains a
typed bounded-status projection until its dedicated extension is implemented
and verified. Watch and complications, CarPlay, store release/distribution
automation, Fastlane, and store-feedback triage remain excluded.

## Architecture and Ownership

- Shared state, protocol shaping, hydration, reconciliation, discovery policy,
  SSH policy, remote-terminal state, and voice signaling belong in Rust.
- Swift and Kotlin own UI, permissions, persistence adapters, native
  audio/session APIs, and render-only projections.
- Shared behavior should cross one handwritten UniFFI boundary rather than be
  reimplemented independently on each platform.

## Identity and Upgrade Boundary

Remora is the only product and protocol identity in supported builds. New
pairing uses Remora Link and the `remora-link/2` contract. The minimum supported
direct-upgrade version is Remora 1.6.0 on both platforms. Its first launch
performs a fail-closed reset of pairing authority, journals, and saved servers
before Remora Link starts. Fresh host pairing is required, and the codebase
does not migrate older pairing state, secrets, saved servers, or protocol
records. Direct upgrades are supported from 1.6.0 onward.

Retired product identifiers are not present as plaintext in supported app or
host code. The 1.6 cutover retains only deletion-only byte tombstones needed
to remove unsupported persisted namespaces; those bytes are never loaded,
migrated, displayed, logged, or sent over the network. Remove the tombstones
when the direct-upgrade floor advances beyond 1.6.

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
| Remora Link | Remora-owned host daemon and v2 pairing/transport boundary. It detects and launches installed harnesses but never installs them. |
| Remora relay | Durable sequenced event/outbox service for hosted or self-hosted deployments; APNs/FCM remain non-authoritative wake hints. |
| Remote terminal | A shell on a paired or SSH-connected host; there is no on-device terminal backend. |
| Ghostty | Retained renderer/input engine for remote terminal surfaces on iOS and Android. |
| WebRTC voice | Native peer connection and audio processing on each platform, with signaling and shared state in Rust. |
| ChatGPT OAuth | Retained account authorization flow implemented with platform browser/loopback adapters. |
| UniFFI | Generated Swift/Kotlin bindings for the single public `codex-mobile-client` mobile surface. |

## Verification

The minimum cross-platform gate is:

```bash
make bootstrap-remora-link-test bindings-hardener-test
make rebuild-bindings
make rust-test
make ios-sim-fast
make test-ios
cd apps/android && ./gradlew \
  :app:testDebugUnitTest \
  :app:testReleaseUnitTest \
  :app:lintDebug \
  :app:assembleDebug
```

Complete release verification also installs these exact build outputs on a
simulator and emulator, exercises the 1.6 security cutover, captures both home
screens, and checks runtime logs for crashes and cutover failures. Branch CI is
defined in [`.github/workflows/mobile-ci.yml`](.github/workflows/mobile-ci.yml).
