# Remora agent guide

## Read before changing code

- Read [CONTEXT.md](CONTEXT.md) for product scope, domain names, runtime
  ownership, and the Remora 1.6 security cutover.
- For UI work, read [PRODUCT.md](PRODUCT.md) and [DESIGN.md](DESIGN.md).
  Keep iOS and Android workflows equivalent while using native controls.
- For builds, generated bindings, device runs, or toolchain failures, read
  [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md). `make help` lists build targets;
  the root `Makefile` defines their commands and configuration defaults.
- For pairing or relay changes, read
  [pairing security](docs/research/pairing-v2-security.md) and the
  [threat model](docs/research/remora-link-threat-model.md).

## Put changes in the owning module

| Work | Location |
| --- | --- |
| Canonical state, reducers, reconciliation | `shared/rust-bridge/codex-mobile-client/src/store/` |
| Direct server operations | `src/ffi/client.rs` and `src/mobile_client/` within that crate |
| Hydration and typed conversation items | `src/conversation.rs`, `src/conversation_uniffi.rs`, `src/ffi/shared.rs` |
| Discovery policy | `src/discovery.rs`, `src/discovery_uniffi.rs` |
| SSH trust, connection, and bootstrap policy | `src/ssh/`, `src/ssh_bridge.rs`, `src/ssh_scripts/` |
| Pairing and paired-host transport | `src/remote_host_pairing/remora_link_v2/`, `src/ffi/remora_link_v2.rs` |
| Relay custody, registration, and repair | `src/background_relay/`, `src/ffi/background_relay.rs`, `src/ffi/remora_link_relay.rs` |
| Voice transcript and handoff state | `src/store/voice.rs` and reducer types |
| iOS UI and platform adapters | `apps/ios/Sources/Remora/Views/`, `Models/`, `Bridge/` |
| Android UI and platform adapters | `apps/android/app/src/main/java/com/remora/android/ui/`, `state/`, `apps/android/core/bridge/` |
| Self-hosted relay | `services/remora-relay/` |
| Remora Link host and runtime bridges | `services/remora-link/` |

`MobileClient` is the internal Rust facade. `AppClient` owns direct server
operations. `AppStore` owns snapshots, subscriptions, and composite actions.
Use authoritative events, then targeted reconciliation where events are
insufficient. Swift and Kotlin project that state; they do not parse upstream
wire strings or maintain a second reducer, status model, or runtime cache.

Keep one handwritten UniFFI interface in `codex-mobile-client`. Expose shared
statuses and payloads as typed Rust records or enums. Generate Swift and Kotlin
bindings with `make bindings`; generated `*.generated.rs` files stay untracked.
Android consumes generated Kotlin directly from
`shared/rust-bridge/generated/kotlin/`, not copied source files.

Host/mobile protocol changes belong in the same checkout. The host import's
`REMORA.md` records provenance; edit that source, not Cargo's Git cache.

Native WebRTC owns peer connections and audio processing. Rust owns voice
signaling, lifecycle, transcript state, and handoff. Ghostty renders remote
terminals; the embedded app-server is not an on-device shell.

## Preserve local work and trust

- Accommodate concurrent changes. Never revert work you did not author.
- Keep `shared/third_party/codex` edits local unless the user explicitly asks
  for a separate submodule commit or push. A parent-repo push does not include
  dirty submodule contents. Document patch changes in `patches/codex/README.md`.
- Keep bundle identifiers and signing identities unchanged unless approved.
- Preserve fail-closed pairing, SSH trust, secret cleanup, and ambiguous-send
  handling. A timeout does not prove a remote mutation failed.
- Preserve semantic success colors, accessibility scaling, and user-selected
  terminal palettes. Apply the app's design tokens only to app chrome.

## Verification and delivery

- Trace callers before changing shared behavior. Add regression coverage at
  the owning Rust interface; verify both native consumers for shared changes.
- Use XCTest in `apps/ios/Tests/RemoraTests/` and Android unit tests in
  `apps/android/app/src/test/java/`. Follow existing four-space formatting and
  explicit actor/coroutine ownership.
- Use the verification commands in [CONTEXT.md](CONTEXT.md). Match the checks
  to the change; report commands, results, and any unrun gates.
- Update `apps/android/docs/qa-matrix.md` when workflow parity changes. Explain
  any intentional one-platform change and the other platform's follow-up.
- Edit `apps/ios/project.yml`, then run `make xcgen` for target or source-layout
  changes. Never hand-edit the generated Xcode project.
- Use concise imperative commit subjects. PRs describe purpose, changes,
  verification, and screenshots for UI changes. Do not commit or push unless
  requested.
