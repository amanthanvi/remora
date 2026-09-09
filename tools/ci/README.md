# CI Helpers

`select_ios_simulator.py` reads `simctl list devices available -j` from stdin
and prints one available iPhone UDID. It prefers an exact `iPhone 17 Pro` name,
falls back to the first available iPhone, and fails when none exists. Run its
regression tests with `make ci-tools-test`.

`make dependency-boundaries-check` checks the locked normal/build Cargo graphs
for the host workspace and every iOS, Catalyst, and Android target supported by
`apps/ios/scripts/build-rust.sh` and `tools/scripts/build-android-rust.sh`. Update
its target list when those build lanes change. It requires Cargo and dependency
sources (network access on a fresh checkout), but does not compile native code
or require mobile SDKs. Run submodule sync first; Mobile CI does this explicitly.

The guard rejects rmcp's HTTP server/session features, the retained client's
`http-test-server` feature, Hickory DNSSEC features, LRU releases older than
0.18.2, Hickory protocol/network releases older than 0.26.1, and the unselected
optional MySQL/RSA 0.9 dependency path. The explicitly
enabled MCP HTTP fixture is outside these production graphs. This is a regression
check for reviewed mitigations, not a replacement for `cargo audit`; it adds no audit
ignores and does not claim that all upstream advisories are fixed.

Cargo metadata unifies resolved features beyond normal/build edges, so the
guard reads Cargo tree's documented `{p}|{f}` format with `--edges normal,build`,
`--prefix none`, and color disabled instead. It checks every record, including
duplicates with distinct feature sets. Empty output, missing application roots,
malformed records, command failures, and timeouts fail closed. Dependencies may
be upgraded or removed without editing the guard. Its stdlib regression tests
run under `make ci-tools-test`.

A separate structured `cargo metadata --no-deps` check ensures the retained
HTTP fixture binary and integration targets require `http-test-server` when
present. Removing a fixture is allowed; removing its opt-in gate is not.

Run `python3 tools/scripts/verify-local-ssh.py` to exercise the live terminal
gate without personal SSH credentials. It requires Docker, Cargo, and network
access to Debian packages. It starts disposable OpenSSH on a random loopback
port, generates a temporary password, runs the Rust terminal test, and removes
its own container in cleanup. It does not enable host Remote Login or change
host accounts. This verifies SSH/PTY behavior against a real local server, not
mobile background execution or a remote-network journey.

The supported workflows are:

- `.github/workflows/mobile-ci.yml` for the shared Rust bridge and native apps.
- `.github/workflows/host-ci.yml` for native macOS/Linux/Windows host custody,
  host CLI builds, and the macOS/Linux workspace with retained-schema checks.
- `.github/workflows/relay-ci.yml` for the standalone relay and PostgreSQL
  contract.

The CI-helper suite also checks native build behavior: XcodeGen cache invalidation
on source additions/removals, and Android's scoped Darwin LLVM lookup without
changing the installed Rust toolchain or inherited search paths.

## Android release prerequisite regression

`android-release-gate.gradle` adds a neutral task that depends on release
BuildConfig generation. Without Firebase resources, this must fail at
`validateReleaseFirebaseConfiguration`, including when configuration is cached:

```sh
apps/android/gradlew -p apps/android \
  --init-script "$PWD/tools/ci/android-release-gate.gradle" :app:remoraGateProbe
```

Mobile CI checks the failure reason before running release JVM tests with its
non-provider fixtures. No release APK is built by the probe. Firebase resources
are a transport prerequisite, not proof of an installed relay registration or
reconciliation adapter, authenticated enrollment, or provider delivery.
