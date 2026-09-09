# Architecture review implementation

The five findings from `architecture-review-20260907-224300.html` are implemented
in the worktree based on `4d4f9a9`. Nothing has been committed or published.
The linked records describe the changes, not unexecuted work.

| Record | Result |
| --- | --- |
| [001 Request identity](001-request-identity.md) | Server/runtime/request identity across Rust, UniFFI, native UI, CLI, and TUI |
| [002 Event pump](002-event-pump.md) | Bounded outstanding requests, live event polling, connection retirement, and loss repair |
| [003 Recoverable drafts](003-recoverable-drafts.md) | Submitted-draft retention and explicit recovery on both platforms |
| [004 Android projection owner](004-android-projection-owner.md) | Serialized projection commits, fenced navigation, cache invalidation, and subscription retirement |
| [005 Post-input refresh](005-post-input-refresh.md) | Session/event/history-fenced delayed reads with bounded worker lifetime |

Current commands, outcomes, and device-validation limits are centralized in
[the verification ledger](../docs/reviews/2026-09-09-risk-gates.md).

## Deliberate exclusions

- No blanket parent merge, excluded products, signing changes, or publication.
- No TypeScript runtime copied into native clients; Rust remains canonical.
- No duplicate navigation, terminal, connection supervisor, or source-review stack.
- No snapshot batching without measured streaming/frame-time evidence.
- No automatic resend of an uncertain mutation or disk-backed draft framework.
