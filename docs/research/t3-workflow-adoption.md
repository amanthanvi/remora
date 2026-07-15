# T3 workflow adoption for Remora

Status: research and roadmap only. No production code was changed.

## Conclusion

Remora should adopt the T3 patterns selectively, in this order:

1. Bind every terminal to a typed, non-secret thread context.
2. Finish Remora's existing snapshot-plus-tail terminal design by making gaps observable and recoverable.
3. Replace timing-based worker shutdown and blanket RPC replay with explicit drain and retry semantics.
4. Project one canonical Rust-owned agent activity phase for both platforms.
5. Build a shared action catalog, then use it for hardware shortcuts and a small mobile command palette.
6. Make the shell geometry-adaptive on both platforms.
7. Move diff shaping into Rust, virtualize the native review surface, and only then add a root-confined, read-only source browser.

This is an incremental hardening and convergence plan, not a T3 port. Remora already has a Rust-owned store, remote Ghostty terminals, bounded terminal replay, native diff renderers, iOS split navigation, thread search, and Catalyst commands. The strongest opportunities are to close correctness gaps and eliminate duplicated platform policy before adding more UI.

The installed comparator was **T3 Code Nightly 0.0.29-nightly.20260712.791**. T3 primary-source findings are pinned to [`pingdotgg/t3code@ecb35f7`](https://github.com/pingdotgg/t3code/tree/ecb35f75839925dd1ac6f854efeef5c9e291d11b), inspected on 2026-07-15. Remora findings are pinned to [`amanthanvi/remora@f7b1420`](https://github.com/amanthanvi/remora/tree/f7b1420bb3226494c4cad07a0ef761452cccc762). The comparison respects Remora's stated boundary: shared runtime behavior belongs in Rust; Swift and Kotlin own native UI; terminals remain remote; hosted relay, Live Activities, and on-device shells remain excluded ([`CONTEXT.md`, lines 6-30](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/CONTEXT.md#L6-L30)).

## Ranked candidates

Scores are 1-5. Higher is better for impact, reliability, performance, parity, and security; higher is *more expensive* for cost. Overall score weights impact and reliability at 25% each, performance at 10%, parity and security at 15% each, and inverse cost at 10%. The score ranks value, not dependency order.

| Rank | Candidate | Impact | Reliability | Performance | Parity | Security | Cost | Overall | Decision |
| ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| 1 | Typed `ThreadTerminalContext` | 4 | 5 | 4 | 5 | 5 | 2 | 4.55 | Adopt first |
| 2 | Snapshot-plus-tail terminal sequencing | 4 | 5 | 4 | 5 | 4 | 2 | 4.40 | Harden existing implementation |
| 3 | Canonical `AgentActivityPhase` | 4 | 5 | 4 | 5 | 4 | 2 | 4.40 | Adopt in Rust |
| 4 | Adaptive navigation | 5 | 4 | 4 | 5 | 4 | 4 | 4.20 | Adopt in two increments |
| 5 | Native file/source preview and diff review | 5 | 4 | 4 | 5 | 4 | 5 | 4.10 | Diff first; files second |
| 6 | Command receipts and worker drains | 3 | 5 | 4 | 5 | 4 | 4 | 3.95 | Drain now; durable receipts only when justified |
| 7 | Hardware shortcuts | 4 | 3 | 5 | 4 | 4 | 2 | 3.85 | Adopt after action catalog |
| 8 | Command palette | 4 | 3 | 4 | 5 | 3 | 3 | 3.65 | Adopt a mobile subset |

Two lower-ranked items still belong early in the implementation sequence: explicit worker drain is a small reliability prerequisite, and hardware shortcuts should land with the action catalog that the later palette reuses.

## 1. Typed terminal context

### Finding

T3 does not currently declare a symbol literally named `ThreadTerminalContext`. The useful pattern is spread across typed identities:

- `ThreadTerminalSubscriptionIdentity` keys a subscription by environment, thread, terminal, cwd, and worktree, then derives the attach request from that identity ([T3 `threadTerminalPanelModel.ts`, lines 3-39](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/apps/mobile/src/features/terminal/threadTerminalPanelModel.ts#L3-L39)).
- Pending launches are keyed by environment/thread/terminal, and cwd/worktree precedence is resolved once in a pure function ([T3 `terminalLaunchContext.ts`, lines 8-89](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/apps/mobile/src/features/terminal/terminalLaunchContext.ts#L8-L89)).
- Route bootstrap distinguishes redirect, already-hydrated, and open states instead of letting the view infer them ad hoc ([T3 `terminalRouteBootstrap.ts`, lines 1-35](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/apps/mobile/src/features/terminal/terminalRouteBootstrap.ts#L1-L35)).

Remora currently loses most of that identity at navigation. The iOS route carries only an optional preferred Alleycat node, while the terminal view separately accepts an optional cwd ([`HomeNavigationView.swift`, lines 37-60](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/apps/ios/Sources/Remora/HomeNavigationView.swift#L37-L60), [`TerminalScreen.swift`, lines 11-20](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/apps/ios/Sources/Remora/Views/TerminalScreen.swift#L11-L20)). Android's route likewise carries only the preferred node ([`Navigation.kt`, lines 8-22](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/apps/android/app/src/main/java/com/remora/android/ui/Navigation.kt#L8-L22)). Both controllers open directly from a `TerminalBackendKind`, then separately retain a generated session id ([iOS controller, lines 42-74](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/apps/ios/Sources/Remora/Models/TerminalSessionController.swift#L42-L74), [Android controller, lines 62-114](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/apps/android/app/src/main/java/com/remora/android/state/TerminalSessionController.kt#L62-L114)).

There is also a concrete secret-boundary problem to fix while introducing the context. The public terminal snapshot stores `TerminalBackendKind` ([`snapshot.rs`, lines 332-349](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/shared/rust-bridge/codex-mobile-client/src/store/snapshot.rs#L332-L349)); that enum contains an Alleycat token or SSH auth material ([`terminal/session.rs`, lines 19-36](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/shared/rust-bridge/codex-mobile-client/src/terminal/session.rs#L19-L36)); and `AppSnapshotRecord` forwards terminal snapshots through UniFFI ([`store/boundary.rs`, lines 452-464](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/shared/rust-bridge/codex-mobile-client/src/store/boundary.rs#L452-L464)). UI state should not need transport credentials.

### Recommendation

Add one Rust-owned, UniFFI-safe `ThreadTerminalContext` (the exact name is reasonable in Remora) with:

- `ThreadKey` as the authoritative server/thread identity;
- optional existing terminal session id;
- canonical cwd and optional worktree path resolved from Rust thread state;
- runtime kind and terminal capabilities;
- a non-secret transport reference or display descriptor.

Credential-bearing backend configuration stays internal to Rust and is resolved from the selected server/credential store. Replace the snapshot's credential-bearing `backend_kind` with a non-secret descriptor such as paired-host versus SSH plus a display label. The native route should carry `ThreadKey` and optional terminal id, not a token, auth object, arbitrary cwd, or independently reconstructed backend.

This context becomes the only input to open, attach, restore, write, resize, and close. A context mismatch must fail closed: a terminal opened for `(server A, thread X)` cannot be attached or written through `(server B, thread X)` or another thread that happens to share a cwd.

### Acceptance criteria

- Identical context resolution tests run against the Rust boundary used by both platforms.
- No token, private key/password object, or raw auth material appears in `AppSnapshotRecord`, debug descriptions, or native route values.
- Open/attach/write reject a mismatched `ThreadKey` or terminal id.
- Existing server-wide terminal launch remains possible by representing an explicit unscoped server context; it is not silently attached to the active thread.

## 2. Snapshot-plus-tail terminal sequencing

### Finding

T3's attach protocol gives snapshots and every tail event an optional sequence ([T3 `terminal.ts`, lines 96-110 and 152-186](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/packages/contracts/src/terminal.ts#L96-L186)). Its manager subscribes first, buffers live events, reads and delivers the snapshot, drops buffered events already covered by the snapshot, replays the rest, and only then switches to live delivery ([T3 `Manager.ts`, lines 2305-2359](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/apps/server/src/terminal/Manager.ts#L2305-L2359)). The client reducer treats snapshots/restarts as replacement state and bounds the UTF-8 buffer to 512 KiB ([T3 `terminalSession.ts`, lines 65-102 and 125-172](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/packages/client-runtime/src/state/terminalSession.ts#L65-L172)).

Remora has already implemented the core race-free idea internally. `TerminalOutputHistory` assigns monotonic sequence numbers and retains a bounded 64 KiB replay window ([`terminal/session.rs`, lines 62-121](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/shared/rust-bridge/codex-mobile-client/src/terminal/session.rs#L62-L121)); `subscribe_output` subscribes before snapshotting history and deduplicates the live tail by sequence ([`terminal/session.rs`, lines 169-201](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/shared/rust-bridge/codex-mobile-client/src/terminal/session.rs#L169-L201)); and a second bounded tail is retained in canonical store state for view reattachment ([`terminal_state.rs`, lines 43-58 and 135-162](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/shared/rust-bridge/codex-mobile-client/src/store/reducer/terminal_state.rs#L43-L162)).

The remaining correctness gap is explicit in the current code: a lagged broadcast receiver silently skips lost events and resumes on fresh bytes ([`terminal/session.rs`, lines 182-199](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/shared/rust-bridge/codex-mobile-client/src/terminal/session.rs#L182-L199)). Sequence is internal, while `TerminalOutputListener` exposes only bytes and exit ([`terminal/session.rs`, lines 50-66](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/shared/rust-bridge/codex-mobile-client/src/terminal/session.rs#L50-L66)), and the FFI snapshot's output tail has no base/latest cursor ([`snapshot.rs`, lines 332-349](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/shared/rust-bridge/codex-mobile-client/src/store/snapshot.rs#L332-L349)). A renderer therefore cannot distinguish complete replay from a silent hole.

### Recommendation

Evolve the existing implementation rather than replace it:

- expose a typed attach stream: `Snapshot { bytes, base_sequence, latest_sequence, truncated }`, `Output { sequence, bytes }`, `Exited`, and `ResetRequired` or an equivalent reset snapshot;
- track the listener's expected next sequence;
- on `Lagged`, replay missing envelopes from history when still retained; otherwise emit a reset snapshot with `truncated = true` and reset the native renderer before applying its tail;
- keep one bounded history owner in `TerminalSession`; make the store snapshot a projection with the same cursor instead of a second independently meaningful stream;
- preserve byte payloads end to end because Ghostty consumes terminal bytes, and cap event size, replay bytes, and subscriber capacity.

The goal is exactly-once application relative to the delivered snapshot, not durable terminal history. When the retained window is insufficient, the UI should recover explicitly and indicate truncated scrollback rather than render a plausible but incomplete screen.

### Acceptance criteria

- A stress test emits more than channel capacity while a listener is paused; the listener receives either every sequence exactly once or an explicit reset, never a silent gap.
- Attach during an output burst proves snapshot/tail ordering and deduplication.
- Background/foreground and renderer teardown preserve the latest retained scrollback on both platforms.
- Exit cannot overtake earlier output.
- Buffer and event limits are enforced under adversarial output.

## 3. Canonical agent activity phase

### Finding

T3 projects a small activity vocabulary—starting, running, waiting for approval, waiting for input, completed, failed, and stale—and gives user-blocking states highest precedence ([T3 `agentAwareness.ts`, lines 8-28 and 85-124](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/packages/shared/src/agentAwareness.ts#L8-L124)). Its UI then sorts attention first and derives one consistent outcome ([T3 `AgentActivity.tsx`, lines 95-147](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/apps/mobile/src/widgets/AgentActivity.tsx#L95-L147)). The cautionary part is that T3 repeats the same phase union in its relay contract and widget ([T3 `relay.ts`, lines 17-26](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/packages/contracts/src/relay.ts#L17-L26), [T3 `AgentActivity.tsx`, lines 21-28](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/apps/mobile/src/widgets/AgentActivity.tsx#L21-L28)). Remora should copy the projection idea, not the duplication.

Remora already holds every required input in Rust, but exposes them separately: `ThreadSummaryStatus` only distinguishes not-loaded/idle/active/system-error ([`types/enums.rs`, lines 118-150](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/shared/rust-bridge/codex-mobile-client/src/types/enums.rs#L118-L150)); session summaries carry subagent status and active-turn state ([`store/boundary.rs`, lines 394-419](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/shared/rust-bridge/codex-mobile-client/src/store/boundary.rs#L394-L419)); and pending approvals and user input are separate snapshot arrays ([`store/boundary.rs`, lines 452-464](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/shared/rust-bridge/codex-mobile-client/src/store/boundary.rs#L452-L464)). Swift and Kotlin currently re-resolve live subagent status in their own views ([iOS `SubagentCardView.swift`, lines 149-178](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/apps/ios/Sources/Remora/Views/SubagentCardView.swift#L149-L178), [Android `SubagentCard.kt`, lines 237-260](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/apps/android/app/src/main/java/com/remora/android/ui/conversation/SubagentCard.kt#L237-L260)).

### Recommendation

Add one Rust `AgentActivityPhase` UniFFI enum and one projector in the store boundary. Add the projected phase to `AppSessionSummary` and the active thread snapshot. Native code maps it only to icon, color, localized text, and layout.

Use an explicit precedence table:

1. matching pending approval;
2. matching pending user input;
3. current turn or session failure;
4. starting/hydrating;
5. active turn/running subagent;
6. completed turn/completed subagent;
7. stale active state after transport loss.

Return no phase (or a separate idle presentation) for a never-run or merely loaded idle thread. T3's `ready`/`idle` to completed fallback is appropriate for its session model, but Remora's persistent historical thread list should require completion evidence to avoid labeling every old idle thread “finished.” Scope approvals by both server and thread; thread ids are not globally unique.

This work is in-app state only. It does **not** add T3's relay, notifications, widgets, or Live Activities, which Remora explicitly excludes.

### Acceptance criteria

- Table-driven Rust tests cover every precedence pair, cross-server duplicate thread ids, reconnect staleness, interrupted/completed races, and never-run idle threads.
- Swift and Kotlin contain no independent activity precedence logic.
- Both platforms sort attention-needed work first using the same phase.

## 4. Command receipts and drainable workers

### Finding

T3 serializes orchestration commands, checks a durable receipt before deciding them, and writes events, projections, and the accepted receipt in one SQL transaction ([T3 `OrchestrationEngine.ts`, lines 128-205](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/apps/server/src/orchestration/Layers/OrchestrationEngine.ts#L128-L205)). Its receipt stores command id, aggregate identity, result sequence, status, and error ([T3 `OrchestrationCommandReceipts.ts`, lines 25-62](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/apps/server/src/persistence/Services/OrchestrationCommandReceipts.ts#L25-L62)). Its reusable drainable worker atomically counts accepted work and resolves only after the queue and active item are both finished ([T3 `DrainableWorker.ts`, lines 1-69](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/packages/shared/src/DrainableWorker.ts#L1-L69)).

Remora already serializes each server runtime through bounded `mpsc` workers ([`session/connection.rs`, lines 447-512](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/shared/rust-bridge/codex-mobile-client/src/session/connection.rs#L447-L512)). Shutdown, however, waits a fixed 100 ms and aborts the worker ([`session/connection.rs`, lines 1156-1161](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/shared/rust-bridge/codex-mobile-client/src/session/connection.rs#L1156-L1161)); in-process requests are spawned into untracked tasks ([`session/connection.rs`, lines 724-778](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/shared/rust-bridge/codex-mobile-client/src/session/connection.rs#L724-L778)). Remote workers retry a cloned request once after a transport error without classifying whether the operation is safe to replay ([`session/connection.rs`, lines 1613-1650](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/shared/rust-bridge/codex-mobile-client/src/session/connection.rs#L1613-L1650)). The cloned JSON-RPC request keeps its id, but Remora cannot assume an arbitrary upstream server durably deduplicates a mutation whose response was lost.

### Recommendation

Split this candidate:

**Adopt drain semantics now.** Make shutdown stop intake, enqueue `Shutdown { ack }`, finish accepted FIFO work and tracked child requests, await the worker with a bounded deadline, then abort only as a last resort. Expose a test-only or internal `drain()` that observes accepted work, not queue emptiness alone. Replace sleep-based tests with deterministic drain assertions.

**Classify replay now.** Mark RPC operations as safe read, idempotent with a server-supported key, or non-idempotent. Automatically reconnect/retry only safe reads and explicitly idempotent operations. For a mutation with an ambiguous transport outcome, reconcile authoritative state or return a typed `AmbiguousOutcome`; do not blindly execute it again.

**Do not copy T3's SQL receipts yet.** A client-only receipt cannot create exactly-once behavior across a remote server boundary. Durable receipts become appropriate only if Remora adds an offline mutation queue or owns a host-side orchestration transaction. For Remora-owned composite/store-local commands, an in-memory command ledger can collapse duplicate taps during one process lifetime; persistence should have an explicit product requirement and expiry policy.

### Acceptance criteria

- Disconnect after enqueuing N commands either drains all accepted work or returns typed cancellations; no request vanishes behind a 100 ms timer.
- A lost response to a mutating request never causes an automatic second execution without a documented server idempotency guarantee.
- Repeated in-process command ids return the same result or rejection when a ledger applies.
- Shutdown and drain tests contain no arbitrary sleeps.

## 5. Adaptive navigation

### Finding

T3 chooses compact versus split from available width *and height*, not device/orientation labels ([T3 `layout.ts`, lines 5-80](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/apps/mobile/src/lib/layout.ts#L5-L80)). Its pane derivation can suppress the leading sidebar when an inspector would squeeze the main content below a floor ([T3 `layout.ts`, lines 82-149](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/apps/mobile/src/lib/layout.ts#L82-L149)). Navigation preserves a compact back stack but replaces peer detail destinations in split mode ([T3 `adaptive-navigation.ts`, lines 1-35](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/apps/mobile/src/lib/adaptive-navigation.ts#L1-L35)). During pane animation it freezes the settled content width to avoid re-laying out the chat feed every frame ([T3 `AdaptiveWorkspaceLayout.tsx`, lines 421-445](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/apps/mobile/src/features/layout/AdaptiveWorkspaceLayout.tsx#L421-L445)).

Remora iOS already switches to `NavigationSplitView` for a regular size class ([`HomeNavigationView.swift`, lines 103-133](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/apps/ios/Sources/Remora/HomeNavigationView.swift#L103-L133)). It does not yet use available height or support an inspector column. Android maintains a single route stack and renders one current route ([`RemoraApp.kt`, lines 250-299](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/apps/android/app/src/main/java/com/remora/android/ui/RemoraApp.kt#L250-L299)).

### Recommendation

Keep layout policy native, as `CONTEXT.md` requires, but define and test one cross-platform behavior matrix:

- compact: one destination stack with list -> detail push and a usable back path;
- split: persistent thread list plus peer detail replacement;
- inspector-capable: optional third pane only when the remaining conversation width stays above a reading floor;
- resizing across thresholds preserves selected thread, detail route, drafts, and back semantics.

First add Android two-pane parity and make iOS use actual available geometry rather than size class alone. Add the third inspector only after review/files have a useful inspector surface. Keep pane preference and route state separate so an automatically suppressed pane can return when space becomes available.

Do not share SwiftUI/Compose layout code through Rust. Share the behavior specification and QA cases; keep platform-native navigation and restoration.

### Acceptance criteria

- Test compact portrait, phone landscape, split-screen tablet, full tablet, Stage Manager/resizable Catalyst, Android foldable-size windows, and threshold crossings.
- Selecting another thread in split mode does not grow the back stack; collapsing to compact retains a path back to Home.
- Opening/closing a pane does not repeatedly reparse or reflow the entire conversation feed.
- Accessibility focus never enters a hidden pane.

## 6. Native source preview and diff review

### Finding

T3 builds source documents and syntax tokens off the render path, uses a bounded document cache, prefers a native row surface, and retains a virtualized fallback ([T3 `SourceFileSurface.tsx`, lines 117-138 and 155-290](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/apps/mobile/src/features/files/SourceFileSurface.tsx#L117-L290), [T3 `source-file-document.ts`, lines 3-53](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/apps/mobile/src/features/files/source-file-document.ts#L3-L53)). Its native diff contract is row-oriented rather than one giant styled string and includes file/hunk/line/comment identity ([T3 `nativeReviewDiffSurface.ts`, lines 23-145](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/apps/mobile/src/features/diffs/nativeReviewDiffSurface.ts#L23-L145)). The file tree preloads on press and renders in small virtualized batches ([T3 `FileTreeBrowser.tsx`, lines 50-76 and 242-264](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/apps/mobile/src/features/files/FileTreeBrowser.tsx#L50-L264)).

The security design is as important as the renderer. T3 accepts a bounded relative path and returns byte length plus truncation ([T3 `project.ts`, lines 119-140](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/packages/contracts/src/project.ts#L119-L140)); rejects absolute and traversal paths ([T3 `WorkspacePaths.ts`, lines 202-231](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/apps/server/src/workspace/WorkspacePaths.ts#L202-L231)); then realpaths root and target to reject symlink escape, verifies a regular file, caps reads at 1 MiB, and rejects NUL-containing binary data ([T3 `WorkspaceFileSystem.ts`, lines 135-244](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/apps/server/src/workspace/WorkspaceFileSystem.ts#L135-L244)).

Remora already presents session diffs on both platforms and uses native text widgets ([iOS `DiffRendering.swift`, lines 12-83](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/apps/ios/Sources/Remora/Views/DiffRendering.swift#L12-L83), [Android `DiffRendering.kt`, lines 32-103](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/apps/android/app/src/main/java/com/remora/android/ui/conversation/DiffRendering.kt#L32-L103)). Rust already computes file-change counts, but still exposes raw diff strings ([`conversation.rs`, lines 1147-1174](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/shared/rust-bridge/codex-mobile-client/src/conversation.rs#L1147-L1174), [`conversation_uniffi.rs`, lines 144-162](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/shared/rust-bridge/codex-mobile-client/src/conversation_uniffi.rs#L144-L162)). Swift and Kotlin separately split unified diffs, infer file titles, and merge sections ([iOS `ConversationTimelineDiffPresentation.swift`, lines 488-565](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/apps/ios/Sources/Remora/Views/ConversationTimelineDiffPresentation.swift#L488-L565), [Android `ConversationSessionDiff.kt`, lines 124-203](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/apps/android/app/src/main/java/com/remora/android/ui/conversation/ConversationSessionDiff.kt#L124-L203)). That is shared protocol parsing in native code and has already drifted into two implementations.

For files, `AppClient` exposes fuzzy search but no general bounded source-read result ([`ffi/client.rs`, lines 1130-1144](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/shared/rust-bridge/codex-mobile-client/src/ffi/client.rs#L1130-L1144)). Existing internal image/pet helpers can read an arbitrary path through a one-off remote command and allow a 20 MB response ([`remote_content.rs`, lines 86-170](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/shared/rust-bridge/codex-mobile-client/src/ffi/client/remote_content.rs#L86-L170)). That helper must not become the public file-browser primitive.

### Recommendation

Ship this in two cuts.

**Diff review first:** move unified-diff parsing, file identity, change kind, counts, hunks, line numbers, and stable row ids into Rust. Expose bounded typed records through UniFFI. SwiftUI/UIKit and Compose/Android View render virtualized rows, selection, collapse state, and theme only. Retain raw patch text only where copy/export needs it. This removes current platform duplication before introducing more review behavior.

**Source browser second:** add narrow read-only operations on `AppClient`, keyed by `ThreadKey` plus workspace-relative path. Rust resolves the authoritative thread cwd/worktree and requires a host capability that can enforce root confinement. The result should be typed text/image/binary/unsupported, include byte length and truncation, cap payloads near 1 MiB, support cancellation, and reject absolute paths, traversal, symlink escape, non-files, and unsupported encodings. Do not silently fall back to arbitrary `OneOffCommandExec` with a client-supplied cwd when the safe capability is absent.

Remora should use its existing native UI stacks directly; it does not need T3's React Native JSON bridge. Start read-only. File writes, inline review comments, and arbitrary command execution are separate security/product decisions.

### Acceptance criteria

- Rust golden tests parse add/delete/rename/binary/malformed/large diffs once; both platforms render the same row ids and counts.
- Large diffs and source files use bounded memory and lazy rows; parsing/highlighting does not run synchronously on the UI thread.
- Absolute paths, `..`, mixed separators, symlink escape, path swaps, directories, binary/NUL files, oversize files, and cancellation are tested.
- A server without the safe workspace-read capability shows “unsupported”; it does not use a broader shell fallback.

## 7. Hardware shortcuts

### Finding

T3 registers typed commands with the most-recent scoped handler, computes enabled commands from the active route, and falls back to global navigation only when no scoped handler consumes the event ([T3 `hardwareKeyboardCommands.ts`, lines 4-78](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/apps/mobile/src/features/keyboard/hardwareKeyboardCommands.ts#L4-L78), [T3 `HardwareKeyboardCommandProvider.tsx`, lines 24-64](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/apps/mobile/src/features/keyboard/HardwareKeyboardCommandProvider.tsx#L24-L64)). Its iOS bridge only advertises currently enabled `UIKeyCommand`s and gives them discoverability titles ([T3 `T3KeyboardCommandsModule.swift`, lines 23-52 and 91-119](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/apps/mobile/modules/t3-native-controls/ios/T3KeyboardCommandsModule.swift#L23-L119)).

Remora has Catalyst commands for new/send/back/forward/session selection, but they are stringly NotificationCenter routes and compile only for Catalyst ([`MacCommands.swift`, lines 1-74](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/apps/ios/Sources/Remora/MacCommands/MacCommands.swift#L1-L74)). The iOS composer captures hardware submit, and both Ghostty surfaces correctly keep terminal key translation local to the renderer ([iOS composer, lines 250-260](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/apps/ios/Sources/Remora/Views/ConversationComposerTextView.swift#L250-L260), [Android Ghostty, lines 502-529](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/apps/android/app/src/main/java/com/remora/android/ui/terminal/GhosttySurfaceView.kt#L502-L529)). There is no equivalent root action dispatch for iPad hardware keyboards or Android.

### Recommendation

Create a typed action catalog before adding more shortcuts. Initial safe actions: new thread, search/focus search, back, open terminal, open review, open files, settings, next/previous thread, and send when the composer explicitly owns focus. Each action has Rust- or route-derived availability and a native execution handler.

Add iPad/Catalyst `Commands`/`UIKeyCommand` and Android Compose key-event handlers against the same action ids. Let the deepest focused surface win. Terminal keys remain Ghostty input; global actions must not steal printable, control, navigation, IME, or system shortcuts while the terminal/composer is focused. Avoid destructive actions and approval decisions in the first shortcut set.

### Acceptance criteria

- The same availability table drives shortcut discoverability and palette enablement.
- Focus tests prove terminal and composer input is not intercepted.
- Each supported action works on iPad/Catalyst and Android hardware keyboards, or the parity exception is documented in the QA matrix.

## 8. Command palette

### Finding

T3 models palette entries as typed action/submenu items with search terms, disabled state, shortcut id, and async execution ([T3 `CommandPalette.logic.ts`, lines 14-55](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/apps/web/src/components/CommandPalette.logic.ts#L14-L55)). It bounds and schemas command ids and contextual keybinding expressions ([T3 `keybindings.ts`, lines 4-9 and 50-106](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/packages/contracts/src/keybindings.ts#L4-L106)); contextual defaults prevent terminal-focused keys from colliding with global actions ([T3 `keybindings.ts`, lines 21-54](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/packages/shared/src/keybindings.ts#L21-L54)).

Remora already has focused thread and model search surfaces, so the missing value is action discovery, not another general text-search engine. A desktop-sized nested palette and a user-editable boolean keybinding grammar would add disproportionate mobile complexity.

### Recommendation

After the typed action catalog exists, add a native searchable sheet with a bounded flat list grouped into Navigate, Thread, Review, Terminal, and Settings. Show current shortcut hints, disable unavailable actions with a reason, and validate the active `ThreadTerminalContext` immediately before execution. Keep async execution errors in the sheet rather than dismissing optimistically.

Initial versions should not include arbitrary scripts, raw shell commands, approval decisions, file writes, or user-authored keybinding expressions. Add recent threads/projects only if measurement shows that the existing Home search is not sufficient.

### Acceptance criteria

- Palette, menu, and hardware shortcuts execute through the same action handler and availability check.
- Context changes while the palette is open cannot execute against the previously selected server/thread.
- Filtering stays responsive with a hard-bounded catalog and preserves VoiceOver/TalkBack order.

## Incremental roadmap

### Phase 0 — correctness and secret boundary

1. Introduce non-secret terminal descriptors and Rust-resolved `ThreadTerminalContext`; migrate both terminal routes/controllers.
2. Extend the current terminal replay with exposed cursors, gap detection, resnapshot/reset, and stress tests.
3. Add deterministic worker drain and tracked shutdown; classify safe versus ambiguous RPC replay.

Each item is independently shippable. The terminal context change should land before new terminal entry points so no additional route grows around the current loose backend/cwd model.

### Phase 1 — canonical workflow state

1. Add Rust `AgentActivityPhase` projection and remove native precedence derivation.
2. Add a typed action catalog and availability projection, with no new UI yet.
3. Update `apps/android/docs/qa-matrix.md` with phase, action, terminal context, and reconnect parity cases.

### Phase 2 — efficient tablet/keyboard workflow

1. Route existing Catalyst commands through the action catalog; add iPad and Android hardware shortcuts.
2. Add Android two-pane navigation and geometry-based iOS split behavior with one shared QA matrix.
3. Add the bounded native command palette against the same action catalog.

This phase can ship without files or a third pane. It improves the current conversation/terminal workflow directly.

### Phase 3 — review and source surfaces

1. Move diff section/hunk/row shaping to Rust; migrate current iOS and Android diff sheets.
2. Virtualize the native review surfaces and measure large-diff memory/frame behavior.
3. Add the capability-gated, root-confined read-only workspace API and native source preview.
4. Add an optional third inspector pane once review/files have a stable surface.

### Phase 4 — only with demonstrated need

- durable command receipts and an offline mutation queue, implemented at a Remora-owned host transaction boundary;
- user-configurable shortcut/keybinding grammar;
- file writes, inline review comments, or other workspace mutations;
- advanced multi-terminal grids or persistent terminal metadata beyond the current mobile use case.

## Explicit non-adoptions

- No T3 relay, hosted push, widgets, or Live Activities.
- No on-device shell or local terminal backend.
- No client-only “exactly once” claim for upstream mutations.
- No public arbitrary-path file read or generic shell fallback for source preview.
- No duplicated Swift/Kotlin diff parser, activity state machine, or terminal reconciliation.
- No React Native/JSON native-view bridge; Remora uses SwiftUI/UIKit and Compose/Android View directly.
- No wholesale desktop keybinding or command-palette feature set.

## Validation gates for implementation PRs

Use the repository's normal cross-platform gate after boundary changes:

```bash
REMORA_SKIP_ALLEYCAT_UPDATE=1 make rebuild-bindings
REMORA_SKIP_ALLEYCAT_UPDATE=1 make rust-test
REMORA_SKIP_ALLEYCAT_UPDATE=1 make ios-sim-fast
cd apps/android && ./gradlew :app:testDebugUnitTest :app:assembleDebug
```

Additionally:

- run terminal sequence/gap/drain stress tests under Tokio paused time where possible;
- exercise iOS simulator and Android emulator reconnect, background/foreground, hardware-keyboard focus, and compact/split resizing;
- benchmark a fixed large diff and 1 MiB source fixture before and after native virtualization;
- run traversal/symlink/binary/oversize workspace-read tests on POSIX and the Windows path-normalization unit surface;
- verify generated Swift/Kotlin bindings contain the same terminal context, action, phase, diff, and file result types.
