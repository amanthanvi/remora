# Plan 015: Durable Host domain

Status: **IN PROGRESS**
Tracker: [#25](https://github.com/amanthanvi/remora/issues/25)
Baselines: Remora `f9dbfc7a4453e14cb508211046ba6835bdc4a5c0`; Link `42e27678cda63bda440a8f6620344f10baefea4f`
Pinned foundation: Link `6442c135bfe52f3a6f7ab9b548a911ddc707061a`
Depends on: Plan 014

## Scope and ownership

Remora Link owns opaque IDs and durable Host, Project, Working Copy, Thread,
Turn, Provider Session, Checkpoint, route, script-trust, provider-instance, and
browser-profile records. Persist one locked atomic JSON snapshot plus a bounded
checksum recovery journal. Remora pins the exact reviewed Link commit.

## Contract and acceptance

- Thread runtime is immutable; incompatible provider handoff creates a child.
- Journal is durable before snapshot replacement and replays a newer generation.
- Corrupt snapshots recover from the journal or fail closed; owner-only modes.
- Run Link domain/catalog tests, full `remora-host --lib`, Remora `--locked`
  Rust tests, and binding generation.

Rollback the Link commit and Remora SHA pin together. STOP on partial cross-repo
landing, silent catalog loss, path/provider display strings used as identity,
or a strict v2 wire incompatibility.

## Progress evidence

The pinned foundation implements the catalog, recovery journal, owner-only
files, crash replay, corruption recovery, and immutable Thread runtime. Link
command-center lifecycle and protocol exposure remain in progress; this plan is
not complete merely because the persistence layer is green.
