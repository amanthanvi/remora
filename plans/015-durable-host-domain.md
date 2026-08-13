# Plan 015: Durable Host domain

Status: **IN PROGRESS**
Tracker: [#25](https://github.com/amanthanvi/remora/issues/25)
Baselines: Remora `f9dbfc7a4453e14cb508211046ba6835bdc4a5c0`; Link `42e27678cda63bda440a8f6620344f10baefea4f`
Pinned implementation: Link `b7640e7f1a3746b0321fc829d2b0cf5a7dc00c04`
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

The pinned implementation includes the catalog, recovery journal, owner-only
files, crash replay, corruption recovery, immutable Thread runtime, bounded Host
and provider capability status, and the authenticated status wire operation.
The endpoint requires `InspectRuntimes`, discloses only provider instances whose
runtime IDs are authorized by the grant, and excludes workspace content. Full
catalog validation now bounds provider sessions, checkpoints, route handles,
trusted scripts, browser profiles, script argv/environment, and every durable
string. It also fails closed on cross-Project Working Copies, mismatched
runtime/provider bindings, missing or cross-Thread session/checkpoint/route
references, invalid Git OIDs/hidden refs, duplicate model IDs/profiles, and
backwards archive/completion timestamps. Full Project/Working Copy/Thread
lifecycle mutation exposure remains before closure.
