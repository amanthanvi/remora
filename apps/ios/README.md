# Remora for iOS

The SwiftUI application lives in `Sources/Remora`. Its project definition is
`project.yml`; regenerate `Remora.xcodeproj` with
`./scripts/regenerate-project.sh` after changing targets or source layout.

For local iteration, run `make ios-sim-fast` from the repository root. Remora
connects to Codex hosts over the network and exposes remote SSH terminals through
the Ghostty renderer; it does not bundle an on-device Linux environment.

See [`CONTEXT.md`](../../CONTEXT.md) for the shared runtime glossary and exact
interop-branding boundary.
