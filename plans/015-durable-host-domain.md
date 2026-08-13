# Plan 015: Durable Host domain

Status: **IN PROGRESS**
Tracker: [#25](https://github.com/amanthanvi/remora/issues/25)
Baselines: Remora `f9dbfc7a4453e14cb508211046ba6835bdc4a5c0`; Link `42e27678cda63bda440a8f6620344f10baefea4f`
Pinned implementation: Link `94e20108739b89d726f54804458416ab10d9cadb`
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

The Host catalog now also owns bounded send-message work-intent receipts. A
credential-scoped prepare → dispatch-fence → success sequence is journaled
before acknowledgement, is bound to the exact origin credential, durable
Thread ID, and lowercase SHA-256 request fingerprint, and can never transition
backwards or be deleted/rebound through a catalog update. A replay after the
dispatch fence returns `outcome_unknown` rather than authorizing a duplicate
provider send. Snapshot-mirror failure after journal fsync now advances live
state to the durable generation and reports committed-unknown, closing a
pre-existing journal/live-state split. Full Link workspace tests and clippy
pass at the pinned revision.
