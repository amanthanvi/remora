# Plan 014: Shared command-center contracts and projections

Status: **IN PROGRESS**
Tracker: [#24](https://github.com/amanthanvi/remora/issues/24)
Baseline: Remora `f9dbfc7a4453e14cb508211046ba6835bdc4a5c0`; Link `42e27678cda63bda440a8f6620344f10baefea4f`
Depends on: Plan 013

## Scope and ownership

Own handwritten UniFFI command-center capability types, version-skew policy,
bounded Mission Control/Sessions projections, direct-remote fail-closed policy,
and thin Swift/Kotlin consumption. Primary paths: `ffi/command_center.rs`,
`ffi/app_store.rs`, `session/connection.rs`, native `AppModel`/home/session
adapters, generated bindings, threat model, and performance budgets.

## Contract and acceptance

- Missing Link declarations are `Unknown`; raw app-server workspace authority
  is `Unavailable`; provider names never imply capabilities.
- Sessions: max 100 rows/256 KiB. Mission Control: max 20 rows per lane/512 KiB.
- Raw direct sockets are secret-free loopback only; remote Hosts use Link/SSH.
- `make bindings`, `make rust-test`, both native unit suites, and fixture parity
  pass; hidden threads hydrate zero timeline items.

Rollback is the single contract/policy commit. STOP on a breaking v2 Link wire
change, an unbounded field, platform-side policy, or a non-loopback bypass.

## Progress evidence

Remora now pins Link `b7640e7f1a3746b0321fc829d2b0cf5a7dc00c04` and exposes
its authenticated, `InspectRuntimes`-scoped `command_center_status` operation as
one handwritten UniFFI result. The client preserves `Unknown` for an older Link
that returns the authenticated `invalid_request` terminal response, rejects
malformed or oversized responses, and opts into the 512 KiB status bound only
after correlating that exact request. Provider instances are already filtered
by the Host grant before serialization.

The authenticated status now reconciles into an independently versioned Rust
cache after every successful Link connection and explicit status query. The
cache accepts only connected, known Hosts; disconnect, removal, legacy status,
and status errors invalidate it. Its stable projection is clamped to 32 Hosts,
reports overflow, and does not clone hydrated timelines. Swift and Kotlin
AppModels subscribe to the same narrow invalidation and expose the generated
projection without adding platform policy.

Focused Link/Rust protocol tests, binding generation, the iOS simulator build,
the Android arm64 debug build, and Android unit tests pass. User-facing native
journey consumption remains before closure.
