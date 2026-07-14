# Apps

Platform-specific applications live here.

- `ios/` contains the SwiftUI application and XcodeGen project definition.
- `android/` contains the Compose application and Android UniFFI bootstrap.

Both apps consume the shared Rust runtime under `shared/rust-bridge/`. See
[`CONTEXT.md`](../CONTEXT.md) for ownership boundaries and retained features.
