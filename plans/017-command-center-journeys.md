# Plan 017: Command-center mobile journeys

Status: **IN PROGRESS**
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

## Progress evidence

- The shared Rust store now derives a bounded `NewTaskLaunchAvailabilityV1`
  from exact runtime IDs, authenticated provider-instance readiness, Host
  connection state, and the legacy typed runtime directory. Native code does
  not infer availability from provider or model names.
- SwiftUI and Compose observe that projection in their Home composers. A known
  unavailable launch disables inline, expanded, keyboard, and hardware send
  paths; displays the exact bounded Host guidance; and leaves the draft intact.
- `Unknown` older-Link status remains compatible when the connected Host's
  typed runtime directory declares the selected runtime available. Missing,
  disconnected, or authoritatively unready Hosts fail closed.
- Focused Rust tests cover legacy compatibility, connection loss, deterministic
  named-instance selection, exact Host guidance, and UTF-8 bounds. Native build
  and parity validation evidence is recorded with tracker issue #27.
- Home exposes matching Needs You, Active, and Recent lane controls on iOS and
  Android. Counts, ordering, attention/status classification, 20-row lane
  bounds, and optional Host scope come from Rust. Selecting a lane reuses the
  existing rich native cards and gesture system; selecting it again returns to
  the unchanged pinned/recent view. Hidden Threads stay hidden and lane
  selection does not hydrate timelines.
- Bounded AppStore projections now read under the canonical Rust store lock
  instead of cloning the complete app snapshot before Mission Control,
  Sessions-page, or thread-viewport projection.

Remaining: exhaustive Sessions, Host Scratch, completion/failure attention,
acknowledgement/snooze/archive, no-eager-hydration UI tests, and the complete
phone/iPad journey matrix.
