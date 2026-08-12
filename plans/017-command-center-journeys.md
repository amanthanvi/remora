# Plan 017: Command-center mobile journeys

Status: **TODO**
Tracker: [#27](https://github.com/amanthanvi/remora/issues/27)
Baseline: Plan 016 reviewed commit
Depends on: Plans 014–016

## Scope and ownership

SwiftUI and Compose render the same Rust Mission Control, Sessions, New Task,
Needs You, and provider-readiness projections. Rust owns filtering, pagination,
attention, acknowledgement, snooze, archive, and feature availability.

## Contract and acceptance

Project-first and Host Scratch flows expose only relevant controls. Unavailable
actions are never tappable. Timeline approval/input stays inline. Test matching
fixtures, offline compose/reconcile, exhaustive filters, no eager hydration,
iPhone/Android journeys, and iPad smoke; update Android parity QA.

Rollback route-by-route while preserving shared records. STOP if either platform
needs provider-name guessing, duplicated reducer policy, or a feature ships
without parity and an intrinsic platform reason.
