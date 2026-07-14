# Shared Rust Runtime

This workspace contains the Rust runtime and developer clients shared by Remora:

- `codex-mobile-client` is the single public mobile crate. It owns `MobileClient`,
  `AppStore`, `AppClient`, discovery, SSH, remote terminal state, realtime voice
  signaling, hydration, reducers, and the handwritten UniFFI boundary.
- `uniffi-bindgen` and `generate-bindings.sh` generate the Swift and Kotlin
  bindings consumed directly by the platform apps.
- `codex-debug-cli` and `codex-tui` are retained developer clients.
- `codex-ipc` and `codex-slingshot` provide supporting transport/runtime code.

Legacy mobile bridge responsibilities have been folded into
`codex-mobile-client`. Platform UI and native APIs remain in Swift/Kotlin;
shared protocol and state policy belong here. See
[`CONTEXT.md`](../../CONTEXT.md) for the ownership glossary.
