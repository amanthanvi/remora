# Android QA Matrix

## Scope

This matrix covers Android app-server behavior and the mobile parity checks that
must stay aligned with iOS.

## Automated Regression Scaffolding

Run Android unit tests:

```bash
./gradlew :app:testDebugUnitTest
```

Current automated checks:

- `RuntimeFlavorConfigTest`
  - validates startup mode/build config parity (`ENABLE_ON_DEVICE_BRIDGE`, `RUNTIME_STARTUP_MODE`)
  - validates canonical app runtime transport declaration (`APP_RUNTIME_TRANSPORT`)
- `SavedServerTransportTest` and `RealtimeWebRtcTransportTest`
  - validate persisted transport selection and WebRTC request shaping
- `ChatGPTOAuthLoopbackServerTest`
  - validates the loopback callback response and OAuth error paths
- `ActiveTerminalRegistryTest` and `GhosttySurfaceSnapshotTest`
  - validate remote-terminal selection and Ghostty surface behavior
- Composer, snapshot, session-derivation, and rendering tests
  - cover payload shaping, snapshot projections, session grouping, Markdown,
    slash commands, response errors, and text sizing

### System-surface privacy

The interim home-screen widget displays only a generic active-turn count and
status, with visible counts capped at `99+`. It never receives prompts,
transcript content, paths, commands, credentials, approvals, file content,
model labels, context metrics, or tool details. `ActiveTurnWidgetProjectionTest`
enforces the count/status projection boundary, including zero, plural,
oversized, and negative inputs.

## Manual Matrix

| Area                                    | Expected Android behavior                                                                                               |
| --------------------------------------- | ----------------------------------------------------------------------------------------------------------------------- |
| App launch                              | App launches and can start an embedded or remote app-server session                                                     |
| Connect local runtime                   | Success through the in-process Rust app-server path                                                                     |
| Connect remote server                   | Success                                                                                                                 |
| SSH-discovered remote server            | Prompts for SSH credentials, connects through SSH port forwarding, and never attempts `ws://host:22` directly           |
| Local transport drop                    | Reconnect and one-time reinitialize before next non-initialize RPC                                                      |
| Remote transport drop                   | Reconnect behavior via Rust `AppStore` updates and resumed RPC notifications                                            |
| Thread start/resume fallback sandbox    | `workspace-write` with `danger-full-access` fallback when Linux sandboxing is unavailable                               |
| Thread turn pagination (newer remotes)  | Conversation opens with last 5 turns; "Load earlier messages" fetches older 5-turn pages via `thread/turns/list`        |
| Thread turn pagination fallback         | Capability flips off via response inspection; embedded turns load fully; "Load earlier messages" is hidden             |
| New-Thread provider readiness           | Home composer uses the shared Rust launch projection; known-unready Hosts disable inline, expanded, keyboard, and hardware send, show bounded Host guidance, and preserve the draft. Older Links remain usable only when the typed runtime directory declares the selected runtime available. |

## Remote Terminal UX Matrix

The remote terminal screen renders through Ghostty on both platforms; this
section tracks parity between iOS (UIKit + Metal) and Android (Compose +
SurfaceView). Neither app bundles a local shell/rootfs.

| Area                                    | iOS                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               | Android                                                                                      |
| --------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------- |
| Full-screen surface                     | No bottom composer; the Ghostty surface fills the body                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            | Same                                                                                         |
| Accessory bar                           | Esc/Tab/Ctrl/arrows/Paste/Clear/Send-to-AI dock above the keyboard via `inputAccessoryView`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       | Compose row anchored above the IME using `imePadding`                                        |
| Tap-to-toggle keyboard                  | Single tap (no selection, no link) toggles the keyboard                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           | Same                                                                                         |
| Long-press selection                    | Long-press seeds word selection; drag extends; handles paint via `TerminalSelectionOverlayView`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   | Long-press seeds word selection; drag extends; handles painted by a sibling Compose Canvas   |
| Edit menu (Copy / Paste / Select All)   | `UIEditMenuInteraction` anchored at the selection union rect                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      | Compose floating action menu above the selection                                             |
| Pinch-to-zoom font                      | `UIPinchGestureRecognizer` clamps to 10–24 pt and re-grids on settle                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              | `ScaleGestureDetector` updates `TerminalConfigPrefs.fontSize` live and re-grids on scale-end |
| BEL haptic                              | `UIImpactFeedbackGenerator(.medium)` + system sound, throttled 250 ms                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             | `performHapticFeedback(LONG_PRESS, IGNORE_VIEW_SETTING)`, throttled 250 ms                   |
| OSC8 hyperlink tap                      | Detected through Rust `linkAtPoint`; opens via `UIApplication.shared.open`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        | Detected through Rust `linkAtPoint`; opens via `Intent.ACTION_VIEW`                          |
| Cell-grid math                          | Driven by Ghostty `surfaceMetrics`; falls back to font-size-aware estimate on first frame                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         | Same path via `nativeSurfaceSize`                                                            |
| Resize on rotation / keyboard show-hide | `layoutSubviews` plus `UIResponder.keyboardWillChangeFrame` triggers                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              | `onSizeChanged` re-fires through Compose's `imePadding` insets                               |
| Mouse-tracking apps (vim / htop)        | Single-finger drag forwards to Ghostty when `mouseCaptured`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       | Same                                                                                         |
| Remote pairing                         | Discovery opens `RemotePairingSheet`; AVFoundation scans the pairing QR, with paste-JSON fallback. The shared pairing bridge parses the payload, lists agents, connects, and persists credentials.                                                                                                                                                                                                                                                                                                                                                                                                                                  | Discovery opens `RemotePairingSheet`; CameraX + ML Kit scan the pairing QR, with paste-JSON fallback. The same pairing bridge and credential flow applies. |

## Plugin `@`-mention parity (follow-up)

iOS ships `@plugin` mentions in the composer: typing `@` now lists installed
Codex plugins above the file results, selecting one inserts an `@<name>`
chip and emits an `AppUserInput.Mention { name, path }` item on
`turn/start`. Path encoding lives in shared Rust (`PluginSummary` UniFFI
record + `list_plugins` on `AppClient`).

Android does not have an `@`-trigger composer popup yet (no inline
`@<file>` autocomplete either), so plugin mentions are a follow-up. The
shared Rust client and the `AppUserInput.Mention` variant are already
wired through the Kotlin bindings, so the platform side just needs a
composer popup, chip row, and send-time append. Track this alongside the
broader composer autocomplete work.

## Suggested Smoke Steps

1. Connect the embedded default server, start a thread, send a turn, toggle network off/on, and send another turn.
2. Force-stop and relaunch the app; confirm initialization and the thread list recover.
3. Connect a remote server and run `thread/list` plus `turn/start`.
4. Verify account read/login status refresh still updates the UI after reconnect.

## Settings — Server Connection Editor (iOS + Android)

Tapping a saved server row in Settings opens the inline editor. Local servers are
name-only; Remora Link paired servers are name-only; everything else allows
mode + host/port/wake-MAC + URL editing. Save persists, Save & Reconnect
disconnects and re-establishes the chosen transport.

| Check                                      | iOS                                                                                                      | Android                                                                                                          |
| ------------------------------------------ | -------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------- |
| Tap saved server row opens editor          | `SettingsServerSheet.edit` opens `SettingsServerConnectionEditor` form sheet                             | Tap or row-menu "Edit" opens `ServerEditSheet` ModalBottomSheet                                                  |
| Local server: only name editable           | Editor displays "managed automatically" copy; only Save & Restart action shown                           | Same — `ServerEditSheet` shows the same copy and uses "Save & Restart"                                           |
| Remora Link paired server: only name editable | Editor shows paired-pairing-metadata copy; mode picker hidden                                         | Same — mode picker hidden, message visible                                                                       |
| Switch to Direct Codex + Save & Reconnect  | Persists, disconnects, calls `serverBridge.connectRemoteServer`                                          | Same — `reconnectController.reconnectServer` reconnects via persisted record                                     |
| Switch to WebSocket + Save & Reconnect     | Persists, disconnects, calls `serverBridge.connectRemoteUrlServer` with `ws://` or `wss://`              | Same — record stores `websocketURL`; reconnect dispatches via Rust `ReconnectController`                         |
| Switch to SSH + Save & Reconnect           | Persists, dismisses editor, opens `SSHLoginSheet`; connect uses `serverBridge.startRemoteOverSshConnect` | Persists, dismisses editor, opens shared `SSHLoginDialog`; connect uses `serverBridge.startRemoteOverSshConnect` |
| Validation errors surface inline           | Alert "Invalid Server" with localized reason, dismiss returns to editor                                  | Same — `AlertDialog` with reason; dismiss returns to editor                                                      |
| Save (no reconnect)                        | Persists `SavedServerStore` + calls `store.renameServer`, leaves connection intact                       | Same                                                                                                             |
| Remove server                              | `SavedServerStore.remove` + closes SSH session + disconnects bridge                                      | Same                                                                                                             |

## Sidebar + Picker Parity Checklist

### Session Sidebar (iOS only)

Android has no sidebar or navigation drawer — the Compose shell navigates
straight from `HomeDashboardScreen`, so these checks apply to iOS alone. On
iOS the sidebar is the `sidebarDashboard` column of the `NavigationSplitView`
in `HomeNavigationView.swift`, which only exists at regular width; compact
width uses a plain `NavigationStack` with no sidebar.

- Sidebar column stays unmounted at compact width; local UI controls persist when the split view reopens it.
- Search + server filter + forks filter produce stable grouping and lineage chips.
- Showing/hiding the sidebar column does not trigger excessive recomposition/signpost churn in idle state.

### Thread List Consistency

- Refresh (`thread/list`) prunes non-authoritative placeholder threads unless they are currently active.
- Notification-only placeholder rows disappear on next refresh once inactive.
- No regressions in thread switching, forking, or session search after placeholder pruning.

### Directory Picker

- Primary action: one-tap `Continue in <last folder>` appears when recents exist.
- Top controls remain visible while list scrolls: connected server chip/status + search.
- Breadcrumb + `Up one level` navigation always reflects current path.
- Bottom CTA is sticky and mirrors path state: `Select <path>` (or disabled helper text).
- Error state exposes both `Retry` and `Change server`.
- `Clear recent directories` requires destructive confirmation.
- Back behavior parity:
  - Android: `Back` navigates up before dismissing sheet.
- iOS: dismiss is blocked while not at root; cancel navigates up first.

### Appearance

- Chat wallpaper can be chosen from the photo library, persists across relaunch, and can be removed from Settings.
- Conversation screen and appearance preview both render the selected wallpaper instead of the fallback theme gradient.

### Conversation Selection

- Settled assistant markdown supports long-press selection/copy.
- Reasoning text supports long-press selection/copy.
- Command output supports selection/copy and still scrolls vertically.
- Error text supports selection/copy.
- Code blocks support selection/copy and still scroll horizontally.
- Markdown links remain tappable after selection support changes.

## Home Dashboard Zoom + Swipe Reply Parity (mirrors iOS b96961b3 + 52ff299d)

Ported in parallel with the iOS "new ui" and "new ui stuff" commits. Each
item must render identically to the iOS `HomeDashboardView` on zoom-1/2/3/4
for the matching state.

| Feature                        | Check                                                                                                                                                                                                                                                                                                       |
| ------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Zoom toolbar button            | Top-right of header cycles 1→2→3→4→3→2→1 with icon matching level (ViewQuilt→ViewList→ViewAgenda→ViewStream). Persists across app restart via `DashboardZoomPrefs` SharedPreferences.                                                                                                                       |
| Pinch-to-zoom                  | Pinch on home LazyColumn crosses level thresholds (`pinchAccumulator ± 0.4 → 1 level`), emits haptic on transition, does not steal single-finger vertical scroll.                                                                                                                                           |
| Zoom 1 (scan)                  | Title + StatusDot only; no meta/body/chips.                                                                                                                                                                                                                                                                 |
| Zoom 2 (glance)                | + time · server · workspace meta line. Tool-activity label + pulsing dots appear only when `isActive && isToolCallRunning(hydratedItems)` — pure thinking falls through to server metadata.                                                                                                                 |
| Zoom 3 (read)                  | + modelBadgeLine (server icon + server + model + fork/subagent) with trailing inline stats, + user-message quote `>` prefix, + compact tool log (1 row), + response preview capped at 25% screen.                                                                                                           |
| Zoom 4 (deep)                  | Tool log expands to 3 rows; response preview cap rises to 50% screen; preview scroll-anchors to bottom when overflowing.                                                                                                                                                                                    |
| Response preview crossfade     | New assistant-block id flip triggers Crossfade on the preview; preserved on empty new-turn assistant items via `displayedAssistantMessage` walking back to last non-empty.                                                                                                                                  |
| TurnStopwatchChip              | Live 1Hz tick while turn active (end=null) via `produceState` + `delay(1000)`; static elapsed when ended. Format `<60s → "Xs"`, `<3600s → "Xm" or "XmYs"`.                                                                                                                                                  |
| Tool log grouping              | Consecutive exploration commands (read/search/listFiles `HydratedCommandActionKind`) collapse into `⌕ Explored N files, M searches, K listings` summary row; other tool kinds render as single-line rows with `toolIconForName` glyph.                                                                      |
| inlineStats chips              | Turn count, tool count, diff `+N/-N`, TurnStopwatchChip, token % (warning tint ≥80%). Left text truncates first; chips stay pinned.                                                                                                                                                                         |
| recentUserMessage              | `>` chip prefix + FormattedText at `RemoraFont.conversationBodyPointSize × textScale`. Only shown when message exists and differs from title.                                                                                                                                                               |
| StatusDot shimmer              | Active state gets both the 800ms alpha pulse AND a 2s linear-gradient sweep overlay.                                                                                                                                                                                                                        |
| Home hydration                 | Home list calls `appModel.externalResumeThread(session.key)` — not `client.readThread` — so the server attaches a live listener and cards update without opening the thread.                                                                                                                                |
| Swipe reply                    | Android: right-swipe on a home row reveals the reply affordance (`SessionReplySwipe` via `SwipeableRow.leadingAction`), left-swipe reveals hide (`trailingAction`). iOS: the equivalent lives in the UIKit `HomeSessionsScrollView` (`Views/HomeSessionsScrollView.swift`), whose row `UILongPressGestureRecognizer` drives the same two affordances through `Callbacks.onReply` / `Callbacks.onHide` past a 120pt commit threshold. Both platforms: past the commit threshold reply opens the quick-reply modal, and the send path resumes the thread before `startTurn` to avoid "thread cannot be found" on cold launches. |
| SavedProjectStore              | Last-selected server + project persist across app restart via Rust `preferencesSetHomeSelection` / `HomeSelection`. Wired through `RemoraApp.kt`.                                                                                                                                                           |
| StreamingMarkdownView bodySize | Optional `bodySize` parameter thread through to TextView font size; opt-in by response preview and by direct consumers that need parametric sizing.                                                                                                                                                         |

## Tool Call Card Parity Matrix (iOS + Android)

Renderer contract for this release:

- default collapsed for tool cards, except `failed` cards (default expanded)
- header order: icon, summary/title, spacer, status chip, optional duration chip, chevron
- section order: metadata KV, payload sections (`Command/Arguments/Result/Output/Action`), auxiliary sections (`Prompt/Targets/Progress`)
- parse miss fallback: legacy markdown rendering unchanged

| Tool kind         | Summary rule                                             | Status chip                                   | Expected sections                                             |
| ----------------- | -------------------------------------------------------- | --------------------------------------------- | ------------------------------------------------------------- |
| Command Execution | stripped command + status/duration suffix                | `inProgress`/`completed`/`failed`/`unknown`   | Metadata, Command, Output (if present), Progress (if present) |
| Command Output    | output label fallback (`Command Output`) when no command | usually `unknown`                             | Output text/code                                              |
| File Change       | first basename + `+N files`                              | normalized from `Status:`                     | Metadata, repeated `Change N` metadata + diff/text content    |
| File Diff         | first path basename when available, else `File Diff`     | usually `unknown`                             | Diff panel                                                    |
| MCP Tool Call     | `Tool:` value + status suffix/check                      | normalized from `Status:`                     | Metadata, Arguments/Result, Error/Progress as available       |
| MCP Tool Progress | tool/status fallback or title                            | usually `unknown` unless merged into MCP call | Progress timeline text                                        |
| Web Search        | `Query:` value                                           | usually `unknown`                             | Metadata, Action JSON                                         |
| Collaboration     | `Tool:` value fallback                                   | normalized from `Status:`                     | Metadata, Prompt text, Targets list                           |
| Image View        | basename from `Path:`                                    | usually `unknown`                             | Metadata (`Path`)                                             |

Status normalization parity:

- `inProgress`, `in progress`, `running`, `pending`, `started` -> in progress (amber)
- `completed`, `complete`, `success`, `ok`, `done` -> completed (green)
- `failed`, `failure`, `error`, `denied`, `cancelled`, `aborted` -> failed (red)
- anything else/missing -> unknown (neutral)

## Realtime Voice (WebRTC transport)

Replaces the prior WebSocket + base64-PCM audio pump with a platform-native WebRTC peer connection on both iOS and Android. Upstream `thread/realtime/start` receives a client offer SDP via `AppRealtimeStartTransport.Webrtc`; the app-server responds with an answer SDP via `ThreadRealtimeSdpNotification`. All other realtime notifications (transcripts, item-added, handoff, closed, error) continue over the existing RPC WebSocket — only the audio byte path moved to the peer connection.

| Area                                    | iOS                                                                                                                                                            | Android                                                                                                      |
| --------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------ |
| Start request carries Webrtc transport  | `AppStartRealtimeSessionRequest.transport == .webrtc(sdp:)` with a non-empty offer SDP (log at session start)                                                  | Same — `AppRealtimeStartTransport.Webrtc(sdp)`                                                               |
| Answer SDP applied                      | `AppStoreUpdateRecord.realtimeSdp` → `RealtimeWebRtcSession.applyAnswer(_:)` → `setRemoteDescription` succeeds                                                 | `AppStoreUpdateRecord.RealtimeSdp` → `RealtimeWebRtcSession.applyAnswer` → `setRemoteDescription` succeeds   |
| Peer connection reaches connected state | `RTCPeerConnectionState.connected` observed via delegate                                                                                                       | `PeerConnection.IceConnectionState.CONNECTED` observed                                                       |
| Bidirectional audio                     | Assistant voice plays back; mic input produces responses                                                                                                       | Same                                                                                                         |
| Transcript deltas (RPC path)            | `ThreadRealtimeTranscriptDelta`/`Done` notifications still render                                                                                              | Same                                                                                                         |
| Client-controlled handoff during voice  | `HandoffManager` receives `HandoffRequested`, `resolveHandoff` / `finalizeHandoff` round-trip completes                                                        | Same                                                                                                         |
| Dynamic tool call during voice          | Argument deltas stream via RPC `ConversationItemAdded`; tool output returns via `resolveHandoff`                                                               | Same                                                                                                         |
| Session stop                            | `RealtimeWebRtcSession.stop()` closes peer + data channel, deactivates `RTCAudioSession`                                                                       | `stop()` disposes peer, restores audio mode, abandons audio focus                                            |
| Session cycle (start/stop x5)           | No leaked peer connections, microphone releases between sessions                                                                                               | No leaked peer, mic indicator clears between sessions                                                        |
| Known non-blockers                      | Per-frame input/output meter animation no longer drives — requires `RTCRtpReceiver.stats` polling to restore (follow-up)                                       | Same flat meter behavior; speaker toggle currently stubbed to a boolean — follow-up to honor runtime routing |
| Regression: custom AEC path             | Retired — `codex-ios-audio` crate + `AecBridge.swift` / `VoiceSessionAudioCodec.swift` were deleted; libwebrtc AEC3 handles echo cancellation natively         | Retired — `AecBridge.kt` deleted; `JavaAudioDeviceModule` enables the hardware AEC + NS                      |
| Regression: SSH-tunneled codex server   | RPC still flows through SSH; WebRTC peer goes direct to OpenAI edge from device. If client runs in fully air-gapped network, realtime voice will not establish | Same                                                                                                         |
