# Remora verification ledger, September 9, 2026

This is the current gate ledger. It supersedes intermediate counts and pending
assessments in the [implementation record](2026-09-08-remora-follow-up.md).
Dependency provenance and migration evidence live in the
[dependency record](2026-09-08-dependency-security.md).

## Implemented corrections

- Host and mobile protocol changes now share the checkout. The imported
  `services/remora-link/REMORA.md` records the reviewed upstream revision and
  retained licenses; four bridge dependencies use ordinary local source.
- Authenticated pairing transfers relay read/manage capabilities to native
  custody before the host erases its transfer copy. Rust owns provisioning,
  token replay, reconciliation, cursor/barrier receipts, ACK, and retirement.
  Native clients supply secure persistence, OS input custody and wake ingress.
- Configuration leases cover provisioning through host commit. Replacement or
  clearing rejects every non-Tombstoned binding, including interrupted and
  ambiguous transfers. Serialized provisioning and fresh credential-bound
  checks prevent stale orphan cleanup from retiring a new pairing.
- Push cursors are untrusted fetch hints. Only authenticated relay responses
  and durable applied/remote-ACK state establish a repair target. An exaggerated
  hint cannot permanently disable enrollment; actual rollback still fails.
- The host retains detached Codex sessions and pending approvals, fences
  attachments, and certifies complete runtime/session/revision barriers.
  Automatic Codex detection supports the Unix proxy; long configuration paths
  use a bounded private socket path.
- Authoritative repair preserves loaded older history, fetches gaps until a
  cached anchor, and fences concurrent page loads, new events, resets and
  removals. An unloaded conversation reads one bounded page, not full history.
  Durable receipts do not persist AppStore: every foreground/cold start repairs
  even when the relay cursor is unchanged.
- Session and direct RPC operations share one request-ID allocator. Paged
  transcript merges insert missing persisted items without replacing newer
  live content or duplicating protected legacy aliases. Unknown model reasoning
  options are filtered only at the typed catalog boundary, never downgraded.
- SSH drains exit status after EOF, rejects RSA private keys before connection,
  and removes RSA host-key algorithms from negotiation. Ed25519, ECDSA and
  password authentication remain supported with fail-closed host trust.
- FCM sends `message.fid` and recognizes invalidation only in a typed FCM error
  detail. Actual HTTP adapter tests cover credential rotation, retries,
  malformed bodies, invalid tokens and redirect rejection.
- Android release prerequisites apply to the task graph, including aggregate
  tasks and configuration-cache reuse. Non-production Firebase fixtures are
  restricted to JVM tests, not distributable APKs.
- Shared discovery no longer holds a synchronous lock across awaits. Shared
  Clippy is a CI gate; host CI includes native macOS, Linux and Windows tests.
- IPC fixtures now use current typed protocol records. `make rust-test` covers
  all first-party workspace targets, not only the mobile library. The paired
  runner selects its compiled test from Cargo JSON before minting an invitation
  and executes it directly, avoiding Cargo lock waits after pairing.
- Android Rust builds resolve LLVM from the selected Darwin toolchain within
  the build process, preserving inherited lookup paths. Fresh compilation of
  every previously affected package now strips successfully without warnings.
- iOS dashboard tests supply isolated preferences instead of reading simulator
  pins. Production defaults retain the same persistence and load pinned/hidden
  preferences together; a regression covers explicit pinned/hidden filtering.
- Ghostty iOS builds retain Zig's cache when compiler, Xcode, SDK and Metal
  context is unchanged. Context changes invalidate it; failed tool discovery
  exits without discarding the existing cache.
- Conformance validates explicit response mappings and the upstream notification
  envelope. Unknown/missing schemas, missing live prerequisites and empty or
  wrong-target captures fail. Four experimental response fixtures come directly
  from the retained generator and have a byte-parity CI gate. Schema-proof tests
  also fail when validation is explicitly disabled.

## Current execution evidence

Log paths below are local artifacts, not committed test results. A green local
gate does not establish hosted CI or production-provider delivery.

| Gate | Result and evidence |
| --- | --- |
| Shared mobile suite | 1,149 passed, zero failed; three separately executed live/timing tests are ignored by the normal gate. `/tmp/remora-final-rust-test-v2-20260909.log` |
| IPC suite | 78 passed, zero failed/ignored after typed fixture updates. `/tmp/remora-final-ipc-tests-20260909.log` |
| Broader workspace | All-target compile passed; `make rust-test` passed 1,246 workspace tests plus 20 DNS-adapter tests. The three normal-gate exclusions each passed separately. `/tmp/remora-final-workspace-check-v3-20260909.log`, `/tmp/remora-final-workspace-tests-20260909.log` |
| Shared strict Clippy | Final rerun passed with warnings denied. `/tmp/remora-final-clippy-v2-20260909.log` |
| Relay wake regression suite | 80 passed, zero failed; the live-fixture test is explicitly ignored here and runs separately. `/tmp/remora-relay-wake-final-green-20260909.log` |
| Dependency boundaries | Host and all six supported mobile targets passed. `/tmp/remora-final-boundaries-20260909.log` |
| Shared audit | Final refresh: zero vulnerabilities, no ignores; three transitive maintenance notices. `/tmp/remora-final-audit-v2-20260909.json` |
| Host/core | 184 passed, zero failed/ignored; strict native Clippy and Windows test cross-Clippy passed. `/tmp/remora-host-push-current.md` |
| Host workspace | Final rerun: 752 passed, zero failed, 13 normal-gate exclusions. The real relay fixture and Pi abort round-trip passed separately; remaining exclusions require external harnesses. `/tmp/remora-final-host-workspace-tests-v3-20260909.log`, `/tmp/remora-final-pi-abort-smoke-20260909.log` |
| Schema conformance | 26 deterministic checks passed; ten provider-backed tests remain ignored by default. Strict Clippy and generated experimental-fixture byte parity passed. Explicitly disabled schema proof correctly exits 101. `/tmp/remora-conformance-hardened-tests.log`, `/tmp/remora-conformance-hardened-clippy.log`, `/tmp/remora-conformance-schema-fixtures-check.log`, `/tmp/remora-conformance-skip-negative.log` |
| Linux host/core | 187 passed, zero failed; the separate relay-fixture test remains excluded from this command. Executed with Rust 1.98.1 in a disposable Linux container and read-only checkout; container removed on exit. `/tmp/remora-final-linux-host-tests-20260909.log` |
| Retained dependency migration | 455 tests plus one SQLite doctest passed; strict scoped Clippy passed. The rebuilt standalone MCP server passed 14 additional unit/subprocess tests. All 16 registered patches reproduce the retained dirty source exactly. `/tmp/remora-dependency-final-tests.log`, `/tmp/remora-mcp-server-runtime-tests.log`, `/tmp/remora-dependency-final-current.md` |
| Relay | Full HTTP adapter suite and required PostgreSQL 17.6 contract passed; release binary rebuilt afterward. `/tmp/remora-relay-fid-release-20260909.log` |
| Build helpers | 19 CI-helper tests, 17 binding-hardener tests and immutable bootstrap test passed. Native build regressions cover Android LLVM lookup and iOS cache retention/toolchain invalidation, including failed SDK discovery. Both changed build scripts pass ShellCheck. `/tmp/remora-final-tools-v4-20260909.log` |
| Bindings | Swift/Kotlin generation, secret-buffer hardening and Swift cancellation/sensitive-carrier runtime proof passed. `/tmp/remora-final-bindings-20260909.log` |
| iOS final product | Build passed; full XCTest rerun against the final Ghostty archives passed 288 tests after fixing preference isolation. Exact tested app installed with matching built/installed dylib hashes. Cold launch, background/synthetic silent wake/foreground smoke and screenshot inspection passed. Simulator pins preserved. `/tmp/remora-ios-relay-final-all-tests-20260909-v3.log`, `/tmp/remora-ios-relay-current.md` |
| Android final product | Fresh JNI/APK build passed. Debug 262 and release 266 JVM tests passed; lint has zero errors, three version advisories and eight hints. Exact APK installed with matching local/device hashes. Cold-start, Keystore custody and actual SSH/background/resume instrumentation each passed; the resume fixture made exactly one model request. Runtime logs and screenshots inspected. `/tmp/remora-android-final-verified-*.log`, `/tmp/remora-android-relay-current.md` |
| Exact paired and SSH journeys | Both final runners passed, one live test each, with owned resource cleanup. `/tmp/remora-final-paired-runner-20260909.log`, `/tmp/remora-final-live-ssh-20260909.log` |
| Projection benchmark | Passed for 1/16/64-thread fixtures. At 64 threads: whole-store clone 342,549 ns versus targeted projection 6,614 ns. Local debug comparison under concurrent build load, not a device latency guarantee. `/tmp/remora-final-projection-benchmark-20260909.log` |
| Ghostty cache | Actual cold build 188.56 seconds, warm build 2.68 seconds. Device/simulator/Catalyst archives are byte-identical between those runs. `/tmp/remora-ghostty-cache-cold-20260909-v2.log`, `/tmp/remora-ghostty-cache-warm-20260909.log` |

The audit database revision is
`b50980aad8b8f14f77e25a97b32dd94bf008b0af`. Maintenance notices affect
`derivative 2.2.0`, `fxhash 0.2.1`, and `paste 1.0.15`; these are not
vulnerability entries and are not suppressed. Their current Starlark, BM25,
V8 and Linux netlink consumers still retain them. Renaming or relabeling those
dependencies would not constitute remediation.

The host debug test link reports an oversized unwind-table warning; tests pass,
but no exception-unwinding performance measurement was made. Stable
rustfmt warns about upstream nightly-only import settings while the formatting
check passes. These warnings are not hidden by the strict source-lint result.
Android's retained code-mode build also reports two unused-metadata warnings.
Its LLVM stripping diagnostics are resolved, not suppressed.

Installed artifact SHA256 values:

- iOS `Remora.debug.dylib`:
  `b02f21f16c8d81cdc867a516f08f38733875ea7129c5080df062467a85bb4816`.
- Android `base.apk`:
  `92ded21427361747d37d6488e0e1bd490284d506fbd6b7cf8f142acf948be6e3`.

Screenshots: `/tmp/remora-ios-relay-native-final.png` and
`/var/folders/xf/7tqtycv15vl6m4zg71bcxhc40000gn/T/remora-native-remote-z8_xosl5/resumed-conversation.png`.

## Reproduce the gates

Use [DEVELOPMENT.md](../DEVELOPMENT.md) for toolchains and native installation.
The minimum source gate is:

```sh
make ci-tools-test bootstrap-remora-link-test bindings-hardener-test
make dependency-boundaries-check
make rebuild-bindings
make rust-clippy rust-test
make ios-sim-fast test-ios
make android-emulator-fast
```

Run Android debug/release JVM tests and lint as documented in `CONTEXT.md`.
Release JVM tests need complete CI-only Firebase resource fixtures. Install
the exact rebuilt products before native cold-start, custody and remote-resume
checks; retain matching output/installed hashes and inspect screenshots/logs.

```sh
cargo build --locked --manifest-path services/remora-link/Cargo.toml -p remora-link
cargo test --locked --manifest-path services/remora-link/Cargo.toml --workspace --all-targets
cargo build --locked --manifest-path services/remora-relay/Cargo.toml --release
python3 tools/scripts/verify-paired-relay.py
python3 tools/scripts/verify-local-ssh.py
python3 apps/android/scripts/verify-remote-resume.py
```

The paired runner uses actual Codex, Iroh pairing and loopback SQLite relay,
with disposable identities and synthetic model/push providers. Set
`REMORA_CODEX_BINARY` to the package executable when its launcher depends on
the normal HOME. The SSH runner uses disposable OpenSSH in Docker, not a host
account. Both reject zero-test success and clean up only their owned resources.

The Linux host gate used `rust:1.98.1-bookworm` with the checkout mounted
read-only at `/workspace`, working directory `/workspace/services/remora-link`,
and `CARGO_TARGET_DIR=/tmp/remora-target` inside a disposable `docker run --rm`
container. Its command was
`cargo test --locked -p remora-host -p remora-bridge-core --all-targets`.
Host CI applies the same retained Codex patches before schema conformance;
patch and sync-script changes trigger that workflow. YAML parsing and the
workflow's exact patch step passed locally.
Experimental fixture freshness is checked with
`bash services/remora-link/crates/bridge-conformance/scripts/experimental-schemas.sh --check`
from the repository root.

## External release gates

- Physical secure-storage, locked-device protection, restore behavior and
  background scheduling require trusted physical phones. The current inventory
  has two unavailable registered iPhones and an Android emulator, not an
  attached physical Android device. Simulator/emulator tests cannot establish
  these physical properties.
- Live APNs/FCM delivery requires deployment configuration, registered device
  destinations and provider credentials. Local HTTP contracts and synthetic
  silent-wake injection do not establish provider delivery. No deployment
  configuration was found in the bounded project/environment inspection;
  arbitrary credential stores were not searched.
- Windows custody has owner-only DACL implementation and a passing Windows
  cross-compile of its tests. Actual native Windows execution remains a hosted
  CI gate; cross-compilation is not execution evidence. The local VM-tool probe
  found only a stale VirtualBox launcher; `VBoxManage list vms` failed because
  VirtualBox.app is absent. No usable local Windows VM was discovered.
- Hosted CI has not run for this uncommitted worktree. Dirty Codex/Ghostty
  submodule changes must remain reproducible through the registered patch sets;
  a parent commit alone does not publish dirty submodule contents.
- Multi-provider live conformance and full standalone Codex/Bazel execution
  remain unrun. The two retired `skills/remote/list` and `skills/remote/export`
  methods are absent from retained upstream and explicitly unsupported by the
  schema validator, not silently counted as conformant. Generated coverage of
  four experimental responses does not establish all experimental-method parity.

No commit, push, deployment, signing identity or app bundle identifier change
was requested or performed. No production provider credentials were read or
printed. Live Activity extensions, watch/CarPlay and store distribution remain
outside the product boundary defined in `CONTEXT.md`.
