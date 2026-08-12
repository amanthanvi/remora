# Plan 023: Release factory and signed Link updates

Status: **TODO**
Tracker: [#33](https://github.com/amanthanvi/remora/issues/33)
Baseline: Plan 022 reviewed commit
Depends on: Plans 015 and 022

## Scope and ownership

Add immutable release-candidate, Host-release, and managed-deploy workflows;
signed manifest schema/verifier; mobile internal promotion; staged managed Host
rollout; opt-in self-host updates; side-by-side Link preflight, activation,
reconnect verification, and rollback.

## Contract and acceptance

Promote without rebuilding. Manifest binds version, SHA, digest, platform,
protocol range, and signature. Test tamper/downgrade/skew rejection, artifact
identity, 10/50/100 staging, interrupted activation, reconnect failure, and
known-good rollback.

Rollback promotion, never recreate artifacts. STOP on unsigned fallback,
untraceable artifact, self-host auto-update, protocol-incompatible rollout, or
loss of the known-good Link binary.
