# 001: Pending-request identity

Status: implemented. Original priority P1; high-risk cross-platform contract.

## Change

Pending requests are identified by `(server_id, runtime_kind, request_id)`.
The runtime comes from the originating event, not the current conversation.
The same identity covers reducer insertion, seeds, lookup, resolution,
dismissal, UniFFI responses, and CLI/TUI callers. Integer/string wire IDs are
preserved. Responses fail closed if that runtime is unavailable.

Server removal filters approvals directly by `server_id`, including approvals
whose thread has not been hydrated. It cannot remove sibling-server state.

## Evidence

- `pending_responses_route_by_server_and_originating_runtime`
- `pending_response_does_not_fall_back_when_originating_runtime_is_missing`
- `pending_request_resolution_and_server_removal_are_scope_local`
- iOS dismissal scope XCTest and Android `UserInputCardTest` runtime/server cases

Sources are in `shared/rust-bridge/codex-mobile-client/src/` under
`types/server_requests.rs`, `session/events.rs`, `store/reducer.rs`,
`mobile_client/user_input.rs`, `mobile_client/event_loop.rs`, and
`ffi/app_store.rs`, plus their native and command-line consumers.

## Contract

Generated bindings and native libraries must be rebuilt together. Do not restore
an ID-only response helper or guess the source from active UI state. Validation
commands and outcomes are in [the review](../docs/reviews/2026-09-07-upstream-adoption.md).
