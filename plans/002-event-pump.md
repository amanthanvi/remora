# 002: Remote event progress and loss repair

Status: implemented. Original priority P1; high-risk transport change.

## Change

The existing session worker polls up to 32 request futures while continuing to
read notifications and process control commands. Requests rejected at capacity
are not dispatched. Cancelled-but-dispatched requests retain their slot until
completion or retirement of their connection; cancelling a waiter is not proof
of remote cancellation.

Reconnect remains serialized in the existing supervisor. A generation change
retires old request handles, including timeout-disabled commands. Mutations are
never replayed; only the existing safe-read allowlist may replay, at most once,
with its original deadline. Typed loss from remote, in-process, session, and UI
queues reaches the existing coalesced authoritative repair gate.

## Evidence

Session tests hold an RPC while delivering notifications, approvals, responses,
and shutdown. Additional tests saturate the request limit, cancel unanswered
requests repeatedly, replace a connection with a held mutation, verify wire
closure, and prove safe replay and ambiguous-send behavior. Loss tests cover
each route and the existing repair gate's coalescing behavior.

Sources: `session/connection.rs`, `session/events.rs`,
`mobile_client/event_loop.rs`, and `mobile_client/store_listener.rs` under
`shared/rust-bridge/codex-mobile-client/src/`.

## Limit

Timeout-disabled commands intentionally occupy capacity until completion or
connection retirement. Control commands remain available. Tests use deterministic
local transports, not a live relay/provider. Validation is in
[the review](../docs/reviews/2026-09-07-upstream-adoption.md).
