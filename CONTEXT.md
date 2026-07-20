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

## Interop Identity Boundary

Remora is the only product identity. Upstream protocol identifiers remain only
where changing them would break host compatibility:

- `ALLEYCAT_*` constants and the legacy `alleycat/1` ALPN;
- the historical SSH-bridge dependency identity required by retained bridge
  crates;
- the time-bounded `_alleycat_seq` replay fallback accepted from the pinned
  host while `_remora_link_seq` is the canonical v2 field;
- the old `npx kittylitter` bootstrap string only where needed to detect or
  explain an existing installation during the re-pair transition;
- precise terminal Kitty protocol terminology in implementation comments;
- exact legacy secret identifiers and Android backup exclusions retained as an
  idempotent purge tombstone until direct upgrades from v1-writing builds are
  no longer supported;
- exact retired saved-server keys and host-ID prefix used only to migrate or
  discard v1 records, after which surviving records are rewritten without
  those keys.

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
| Legacy pairing v1 | The retired bearer-token host pairing protocol. It is retained only for detection and explicit re-pair guidance. |
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
make rebuild-bindings
make rust-test
make ios-sim-fast
cd apps/android && ./gradlew :app:testDebugUnitTest :app:assembleDebug
```

Branch CI is defined in [`.github/workflows/mobile-ci.yml`](.github/workflows/mobile-ci.yml).
