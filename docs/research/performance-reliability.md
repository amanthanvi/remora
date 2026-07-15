# Pairing, connection, and reconnect performance and reliability

Date: 2026-07-15
Status: evidence-backed research and experiment plan; no production code changed

## Conclusion

Remora's current remote pairing path is Alleycat over a shared iroh/QUIC endpoint. Its largest measurable opportunities are not in QR parsing or Swift/Kotlin rendering; they are in duplicated control-plane work, serial runtime attachment, unbounded or correlated reconnect work, and missing end-to-end timing.

The recommended order is:

1. Instrument one correlation-aware connection timeline in shared Rust before changing retry policy. Record monotonic phase durations, trigger, network generation, route, runtime, attempt, outcome, and typed failure class.
2. Remove the second `list_agents` round trip after the user taps Connect, and compare serial runtime attachment with bounded parallel attachment.
3. Replace the current fixed five-attempt/one-second retry loop and duplicated platform triggers with one per-server, coalescing reconnect state machine. Make network availability event-driven; use seeded full jitter for repeatable tests; never blindly replay a mutation with an uncertain outcome.
4. Put explicit deadlines around iroh connection, stream opening, control request/response, app-server initialization, and the complete user journey.
5. Add a deterministic host simulator and scripted transport faults, then run the same journey matrix on iOS and Android physical devices. Treat relay/direct, cold/warm endpoint, network handoff, suspension, and multiple selected runtimes as separate cohorts.
6. Package the host as a native Rust daemon delivered by a thin npm launcher, but install it into a stable product-owned path and let the operating system supervise it. `npx` is a bootstrap channel, not a service location or restart policy.

The SLO values below are proposed starting gates, not claims about current production performance. The repository has no distributions from which to establish a baseline. Run the instrumentation-only baseline first, retain cohort labels, and revise a target only with measured evidence.

## Scope and terminology

This report covers the path from accepting a remote pairing payload through agent discovery, initial connection, foreground recovery, network-change recovery, and replay/authoritative reconciliation. It also covers discovery work that can compete for sockets or battery during those journeys and the operational shape of the host daemon.

The user-facing pairing sheets on both platforms parse an Alleycat payload, list remote agents, and call the shared `connect_remote_over_alleycat` path ([iOS](../../apps/ios/Sources/Remora/Views/RemotePairingSheet.swift#L380), [Android](../../apps/android/app/src/main/java/com/remora/android/ui/discovery/RemotePairingSheet.kt#L124)). The separate `pair` WebSocket module still has loopback tests, but a repository search found no Swift or Kotlin call site. It should not be used as the baseline for today's pairing SLO.

Definitions used below:

- **Direct**: iroh reaches the host without carrying application data through a relay.
- **Relay**: the selected iroh path uses the configured relay.
- **Cold endpoint**: the process has not yet bound the shared iroh endpoint.
- **Warm endpoint**: the shared endpoint exists, but the specific remote connection may not.
- **Ready**: the selected app-server runtime has completed transport establishment and initialize, is attached to the Rust session/store, and a benign typed RPC succeeds.
- **Recovered**: the session is ready and replay or authoritative refresh has restored a self-consistent thread view.
- **Network generation**: one stable platform path fingerprint after debounce; all callbacks associated with that fingerprint coalesce into the same generation.

## Current critical paths

### Pair and connect

After a scan or paste, each platform immediately makes a remote agent-list request. Tapping Connect then calls Rust, which lists those agents again before connecting any selected runtime ([`MobileClient::connect_remote_over_alleycat`](../../shared/rust-bridge/codex-mobile-client/src/mobile_client/mod.rs#L622)). `list_agents` creates a fresh iroh connection and bidirectional stream, performs one request/response, and closes the connection ([`alleycat.rs`](../../shared/rust-bridge/codex-mobile-client/src/alleycat.rs#L471)).

The effective current path is therefore:

```text
payload accepted
  -> lazy endpoint bind, if cold
  -> iroh dial + control stream + list_agents request/response
  -> user selects agents and taps Connect
  -> another iroh dial + control stream + list_agents request/response
  -> for each selected runtime, in sequence:
       iroh dial + control stream + connect request/response + app-server initialize
  -> build/attach multiplexed ServerSession
  -> UI observes ready state
```

The runtime loop is serial ([`mobile_client/mod.rs`](../../shared/rust-bridge/codex-mobile-client/src/mobile_client/mod.rs#L764)). One failed runtime is skipped and the overall connection succeeds if at least one selected runtime attaches. Remora deliberately persists the complete selection intent for a later reconnect ([`mobile_client/mod.rs`](../../shared/rust-bridge/codex-mobile-client/src/mobile_client/mod.rs#L847)), but the UI currently waits for the serial loop and does not expose `first ready`, `all ready`, and `partial by deadline` as separate outcomes.

For `N` selected runtimes, the visible connect latency is approximately:

```text
Tconnect = Tlist-again + sum(Truntime-connect[i]) + Tattach + Tsnapshot
```

The first experiment should compare this against reuse of the already validated agent list and bounded concurrency of two:

```text
Tconnect-candidate ~= max of bounded Truntime-connect batches + Tattach + Tsnapshot
```

An agent-list snapshot must be bound to the node, token context, protocol version, and a short monotonic age. A stale or unavailable selected agent should be reported as a typed partial result, not silently trusted forever.

### Timeout surface

The Alleycat control path awaits `endpoint.connect`, `open_bi`, frame reads, frame writes, and flushes without a Remora-owned deadline ([`open_stream_on`](../../shared/rust-bridge/codex-mobile-client/src/alleycat.rs#L668), [`read_json_frame`](../../shared/rust-bridge/codex-mobile-client/src/alleycat.rs#L695)). Frame size is bounded, which protects memory, but time is not bounded.

The upstream app-server client defines 10-second connect and initialize timeouts ([`remote.rs`](../../shared/third_party/codex/codex-rs/app-server-client/src/remote.rs#L65), [initialize](../../shared/third_party/codex/codex-rs/app-server-client/src/remote.rs#L899)). Direct WebSocket attempts can therefore spend the connect and initialize budgets serially. Alleycat stream establishment happens before the initialize timeout and can wait independently. A user-journey deadline must wrap the complete operation; adding only another inner timeout can still leave the sum unbounded or excessively long.

Use one absolute monotonic deadline and report which phase exhausted it. Suggested initial phase caps inside an eight-second foreground connect budget are:

| Phase | Initial cap | Notes |
| --- | ---: | --- |
| Endpoint cold bind and resolver/relay setup | 2.0 s | Cache and report separately from a warm endpoint. |
| QUIC connection and path selection | 3.0 s | Distinguish direct, relay, and no viable path. |
| Bidirectional stream open | 1.0 s | Usually negligible after a healthy connection. |
| Control request/response | 2.0 s | Includes host scheduling delay; preserve remaining journey deadline. |
| App-server initialize | 4.0 s | Candidate reduction from upstream's generic 10 s; validate on slow hosts before adoption. |
| Session attach and first benign RPC | 1.0 s | Detect a false-positive `Connected` state. |

These are nested maxima, not additive entitlements. No phase may extend the journey's absolute deadline.

### Reconnect and recovery

The shared session worker currently tries five times with a fixed one-second sleep and no jitter ([constants and loop](../../shared/rust-bridge/codex-mobile-client/src/session/connection.rs#L32), [`reconnect_remote_client`](../../shared/rust-bridge/codex-mobile-client/src/session/connection.rs#L1247)). It does not classify authentication, protocol, network-unavailable, load-shed, or transient transport failures. Because an individual attempt can consume its own transport timeouts, “five attempts” does not bound total time tightly.

A transport failure while processing any `SessionCommand::Request` reconnects and then clones/replays that request once without discriminating by method ([worker](../../shared/rust-bridge/codex-mobile-client/src/session/connection.rs#L1598)). A send failure does not prove that the host failed to apply a mutation. Until a stable idempotency key and host-side deduplication exist, automatically replaying a mutation creates an uncertain-outcome risk. Read-only requests may be retried; mutations should reconcile authoritative state or surface an explicit uncertain outcome.

Bulk reconnect is protected by one global `try_lock`; a concurrent trigger receives an empty result and is not queued for a later pass ([`ffi/reconnect.rs`](../../shared/rust-bridge/codex-mobile-client/src/ffi/reconnect.rs#L340)). Plans for all saved remote servers then enter an unbounded `JoinSet` together ([`ffi/reconnect.rs`](../../shared/rust-bridge/codex-mobile-client/src/ffi/reconnect.rs#L436)). This avoids serial recovery but can create synchronized relay, DNS, SSH, and host load after a common outage. A single-server reconnect does not share this global guard, so it can overlap bulk work.

Platform lifecycle code amplifies triggers:

- `on_app_became_active` already performs a network hint, saved-server reconnect, then sequential account probes ([`ffi/reconnect.rs`](../../shared/rust-bridge/codex-mobile-client/src/ffi/reconnect.rs#L258)).
- Android runs saved-server reconnect twice at launch and calls it again immediately after `onAppBecameActive` on resume ([`AppLifecycleController.kt`](../../apps/android/app/src/main/java/com/remora/android/state/AppLifecycleController.kt#L29), [resume](../../apps/android/app/src/main/java/com/remora/android/state/AppLifecycleController.kt#L90)).
- iOS may issue a second initial reconnect and then refresh selected threads sequentially ([`AppLifecycleController.swift`](../../apps/ios/Sources/Remora/Models/AppLifecycleController.swift#L104)).
- Both network observers call `notifyNetworkChange` and, after an outage, call `onNetworkReachable`, which calls the same hint again ([iOS](../../apps/ios/Sources/Remora/Models/NetworkReachabilityObserver.swift#L87), [Android](../../apps/android/app/src/main/java/com/remora/android/state/NetworkReachabilityObserver.kt#L130), [Rust](../../shared/rust-bridge/codex-mobile-client/src/ffi/reconnect.rs#L334)).

The 15-second long-resume decision uses wall time (`Date` / `System.currentTimeMillis`) rather than a monotonic elapsed clock ([iOS](../../apps/ios/Sources/Remora/Models/AppLifecycleController.swift#L114), [Android](../../apps/android/app/src/main/java/com/remora/android/state/AppLifecycleController.kt#L77)). User or network time changes can therefore misclassify a resume.

### Resume and replay correctness

Alleycat records the maximum `_alleycat_seq` observed and sends `resume.last_seq` on a new connection ([`AlleycatReconnectTransport`](../../shared/rust-bridge/codex-mobile-client/src/alleycat.rs#L122)). The host can answer with fresh, resumed, or drift-reload attachment. Drift reload currently produces a warning saying the client should reload authoritative state ([`log_session_info`](../../shared/rust-bridge/codex-mobile-client/src/alleycat.rs#L747)); this path needs an explicit correctness assertion in the integration harness.

Recovery success must therefore mean more than “QUIC connected.” It must include:

1. resume result recorded;
2. replay cursor monotonicity checked;
3. a forced authoritative reload on drift;
4. active-turn, approval, queued-follow-up, and account state reconciled; and
5. one benign request succeeds on the replacement client.

## Proposed SLOs and SLIs

These are release gates for controlled direct and relay test cohorts, plus rolling field SLIs if privacy-reviewed collection is later added. Do not merge direct and relay distributions, cold and warm endpoints, or one-runtime and multi-runtime connects.

| Journey / invariant | Start and stop signal | Proposed target | Required cohorts |
| --- | --- | --- | --- |
| Agent choices ready | Valid payload accepted -> selectable agent list rendered | Direct p50 <= 0.75 s, p95 <= 2.0 s, p99 <= 4.0 s; relay p50 <= 1.25 s, p95 <= 3.0 s, p99 <= 6.0 s; terminal error <= 8 s | iOS/Android; cold/warm endpoint; direct/relay |
| Primary runtime ready | Connect tapped -> selected primary runtime attached and benign RPC succeeds | Direct p50 <= 1.0 s, p95 <= 2.5 s, p99 <= 5.0 s; relay p50 <= 1.75 s, p95 <= 4.0 s, p99 <= 8.0 s | 1, 3, and 7 selected runtimes; host idle/loaded |
| Complete selected set | Connect tapped -> all selected runtimes ready or typed partial deadline | Three runtimes: direct p95 <= 4 s, relay p95 <= 7 s; hard deadline 8 s; every missing runtime named with a typed failure | Serial, concurrency 2, and unbounded experimental arms |
| Foreground transport recovery | Recoverable live connection loss -> benign RPC succeeds on replacement | p95 <= 3 s direct, <= 5 s relay; p99 <= 8 s; >= 99.5% within 10 s and >= 99.9% within 30 s after a usable path exists | reset, blackhole, host restart, relay/direct path loss |
| Resume recovery | app-active callback -> active thread authoritative and benign RPC succeeds | p95 <= 3 s direct, <= 5 s relay; p99 <= 8 s | 5, 14, 16, 30, and 120 s suspension; unchanged and changed path |
| Replay correctness | Disconnect injection -> recovered snapshot | Zero missing or duplicate user-visible effects in the deterministic fault suite; zero blind mutation replays; drift reload always performs authoritative reconcile | Fresh, resumed, duplicate, reordered, and below-floor cursor cases |
| Reconnect amplification | One network generation or lifecycle transition | One active reconnect per server; triggers coalesce; <= 3 dials/server in first 10 s; <= 3 aggregate remote dials concurrently until experiments justify another value | 1, 10, and 50 saved servers; synchronized clients |
| Discovery first useful result | Discovery sheet opened -> first saved/cached or live server visible | Saved/cached p95 <= 100 ms; first live result p95 <= 1.5 s; complete/cancelled sweep <= 6 s/250 ms | empty/populated mDNS; /24; Tailscale absent/present |
| Constrained-network discovery | Path becomes expensive, constrained, metered, or cellular | Zero automatic /24 probes; user-initiated scan must explain cost before probing | iOS Low Data Mode/cellular; Android metered/cellular |
| Foreground idle network cost | Stable paired foreground idle for 60 min | <= 4 deliberate keepalive intervals/min/runtime and <= 100 KiB bidirectional keepalive traffic/hour/runtime; calibrate from packet capture | Wi-Fi/cellular; direct/relay; 1/3 runtimes |
| Background idle work | Background with no active voice/session exception | Zero Remora-scheduled discovery or retry timers; at most one coalesced recovery burst after foregrounding | iOS suspension; Android Doze/App Standby |
| Host service recovery | Native daemon terminated unexpectedly | Supervisor observes nonzero exit, restart attempt begins <= 2 s, health returns <= 5 s, and launcher reports the transition | launchd, systemd, Windows Service Control Manager |

For availability percentages, the denominator is an attempt that has a usable network path and valid credentials. Authentication rejection, explicit protocol incompatibility, user cancellation, and an unavailable host are separately counted outcomes, not hidden exclusions.

The first baseline should also publish:

- p50/p95/p99 and maximum per phase;
- success, timeout, cancellation, permanent failure, partial success, and uncertain-mutation counts;
- attempts per recovery and peak dials per 100 ms;
- direct/relay transition count and path-change-to-ready time;
- replayed event count, drift-reload count, and authoritative-reconcile duration;
- bytes, radio/network energy, CPU time, and wakeups in idle tests; and
- thermal state and battery/charging state for every physical-device run.

## Instrumentation contract

### Current gaps

The current code has useful local logs and health states but cannot compute the SLOs above:

- Pair/connect logs have start or outcome messages but no shared correlation ID or elapsed phase timings.
- `ConnectionHealth::Connecting` exposes attempt/max-attempt, but not trigger, failure class, route, elapsed time, or next retry.
- `ReconnectResult` exposes only success, error text, and local-auth restoration. It cannot distinguish coalesced, skipped, partial, transient, permanent, deadline, or recovered-by-migration outcomes.
- `ServerTransportDiagnostics` records the last successful direct request, lifecycle transitions, and one pending mutation, but not attempt history or duration ([`snapshot.rs`](../../shared/rust-bridge/codex-mobile-client/src/store/snapshot.rs#L151)).
- The only lifecycle signpost in the inspected iOS controller surrounds entering background; foreground recovery and pairing have no signposted intervals ([`AppLifecycleController.swift`](../../apps/ios/Sources/Remora/Models/AppLifecycleController.swift#L37)).
- The platform reachability fingerprints include constrained/expensive or metered/validated state, but Rust receives only an undifferentiated “network changed” hint.
- Discovery has progress events, but no scan elapsed time, candidate count, probe count, peak sockets, bytes, cancellation completion, or per-source deadline metrics.

### One shared Rust timeline

Add an internal, narrow `ConnectionAttemptTrace` emitted as structured tracing fields and as a deterministic test record. It should not expose raw tokens, payloads, node IDs, hostnames, full URLs, account data, or thread contents.

Recommended fields:

| Field | Shape |
| --- | --- |
| `correlation_id` | Random per user journey; copied across platform, Rust, and host logs |
| `server_key` | Ephemeral keyed hash for grouping within one install, not raw identity |
| `runtime_kind` | Typed runtime, or `control` for list/restart operations |
| `transport` / `route` | Alleycat, direct WebSocket, SSH, slingshot; direct, relay, unknown |
| `trigger` | pair, user-connect, request-failure, event-EOF, foreground, long-resume, network-generation, manual |
| `network_generation` | Monotonic local counter after path debounce |
| `attempt` / `retry_budget_remaining` | Integers |
| `phase` | endpoint-bind, resolve, quic-connect, stream-open, control-write, control-read, app-initialize, attach, replay, reconcile, first-RPC |
| `phase_elapsed_ms` / `journey_elapsed_ms` | Monotonic durations |
| `outcome` | success, partial, cancelled, coalesced, deadline, transient, permanent, uncertain |
| `error_kind` | Typed low-cardinality class; keep raw details in local debug logs only |
| `resume_kind` / `events_replayed` | fresh, resumed, drift-reload; count |
| `selected_count` / `ready_count` | Integers for multiplexed connection |
| `active_dial_count` | Gauge sampled at attempt start/end |

Use `std::time::Instant` inside Rust and a monotonic clock on each platform. Wall-clock timestamps may be attached only for log ordering. Carry the journey ID through the UniFFI call and include it in host control messages when the protocol can evolve compatibly.

On iOS, wrap the user journeys and Rust phases with signposts so XCTest can collect `XCTClockMetric`, `XCTMemoryMetric`, and `XCTOSSignpostMetric`; Apple documents those metrics in [XCTest performance tests](https://developer.apple.com/documentation/xctest/performance-tests). On Android, use app-owned trace sections and Macrobenchmark. Android's official benchmark tooling supports end-user flows and `TraceSectionMetric`; `PowerMetric` includes a NETWORK category but is system-wide and limited to supported Pixel devices ([overview](https://developer.android.com/topic/performance/benchmarking/benchmarking-overview), [metrics](https://developer.android.com/topic/performance/benchmarking/macrobenchmark-metrics)).

Keep initial metrics local: JSONL artifacts from Rust tests, XCTest attachments, Perfetto traces, packet captures, and host logs. Any field telemetry is a separate privacy/product decision and should use sampled low-cardinality aggregates rather than raw server identifiers.

## Reconnect model to test

### State machine

Use one Rust-owned state machine per server:

```text
Disconnected
  -> WaitingForNetwork
  -> Dialing
  -> Authenticating
  -> Initializing
  -> Resuming
  -> Reconciling
  -> Connected

Any phase -> PermanentFailure
Any mutating request with ambiguous delivery -> UncertainMutation -> Reconciling
```

All lifecycle, reachability, request-failure, EOF, and manual triggers feed this machine. A trigger for the current network generation joins the active attempt. A newer generation cancels stale dialing safely and schedules exactly one immediate attempt. Callers await the same result rather than receiving an empty “skipped” vector.

Classify before retrying:

| Class | Examples | Policy |
| --- | --- | --- |
| Permanent until user/config changes | Invalid token, protocol mismatch, malformed payload, unsupported runtime, trust/auth rejection | Stop immediately; surface a typed action. |
| Network unavailable | No validated path, iOS path unsatisfied, Android network unvalidated/Doze | Do not burn attempts; wait for a new network generation or foreground. |
| Transient transport | Reset, EOF, relay failure, timeout, path abandonment | Retry within budget using jitter. |
| Load shed | Host busy, explicit retry-after | Honor server delay plus seeded jitter. |
| Ambiguous mutation | Connection lost after send but before response | Do not replay blindly; query/reconcile using the stable local request ID. |
| Application rejection | JSON-RPC method/validation error | Return to caller; no transport reconnect. |

### Backoff candidates

The leading candidate is event-gated full jitter:

```text
attempt 0: immediate after a genuinely new usable path or explicit user action
attempt n: sleep random(0, min(cap, 250 ms * 2^n))
foreground cap: 8 s
background/no active exception: no timer; wait for OS/lifecycle event
reset: after 30 s stable connection or one successful authenticated RPC
```

Seed test jitter from `(test_seed, server_key, network_generation)` so simulations reproduce exactly. For production, use per-install entropy so multiple devices do not synchronize. AWS's primary guidance explains bounded timeouts, retry safety/idempotency, exponential backoff, and jitter ([timeouts, retries, and backoff](https://aws.amazon.com/builders-library/timeouts-retries-and-backoff-with-jitter/)); its correlated-failure guidance describes stable seeded jitter as a way to preserve repeatability while spreading work ([correlated failures](https://aws.amazon.com/builders-library/minimizing-correlated-failures-in-distributed-systems/)).

Compare four arms rather than adopting constants by intuition:

| Arm | Delay policy | Expected tradeoff |
| --- | --- | --- |
| Current | 1 s fixed, five attempts | Simple, but correlated; attempts burn while offline. |
| Full jitter | `U(0, min(cap, base * 2^n))` | Low peak load and fast median; may occasionally retry nearly immediately. |
| Equal jitter | `cap/2 + U(0, cap/2)` | Higher minimum delay and slower median; lower chance of adjacent retries. |
| Decorrelated jitter | `U(base, previous * 3)` capped | Responsive to variable outages, but state and tails are less intuitive. |

For each arm, simulate 1, 10, 1,000, and 50,000 clients recovering from 1 s, 5 s, 30 s, and 5 min outages. Compare median/p99 time to ready, total dials, peak dials per 100 ms, host CPU, relay errors, and energy proxy (radio-active windows). Repeat with 1, 3, 10, and 50 saved servers per client and with a retry-after response. The winning policy must meet the recovery SLO without concentrating work.

Bound aggregate dialing as a separately tunable experiment. Start with two runtime attachments per host and three remote server dials per app; compare against serial and unbounded arms. Concurrency is a result to measure, not a permanent magic number.

## Benchmark harness

### Layer 1: deterministic Rust tests

Build a small test-only scripted `RemoteTransport` and clock. Existing seams already support this direction: the session worker accepts `Arc<dyn RemoteTransport>`, the reconnect test uses in-memory streams, and Tokio can pause/advance time.

Capabilities:

- scripted phase outcomes (`success`, typed error, pending until deadline);
- seeded jitter and a recorded sleep schedule;
- network-generation events and trigger coalescing;
- host apply-before-drop for ambiguous mutation tests;
- event sequence injection, including duplicates, gaps, and replay floor drift;
- gauges for concurrent dials and exact attempt timestamps; and
- JSONL output with one record per phase and one summary per journey.

This layer should run on every PR and assert exact schedules and invariants without real sleeps.

### Layer 2: loopback integration host

Create a test-only Alleycat-compatible host simulator with a fault script such as:

```text
delay phase=control_response by=750ms
drop phase=initialize direction=downstream after=1-frame
reset connection=2 after=request-applied
resume floor_seq=120 current_seq=150
duplicate seq=147 count=2
restart after=3s downtime=2s
```

Route TCP/WebSocket and any proxyable host edges through [Toxiproxy](https://github.com/Shopify/toxiproxy), whose documented toxics include latency, timeout/blackhole, reset, bandwidth, slow close, slicing, and byte limits. QUIC/UDP-specific behavior needs either host-simulator hooks or OS network emulation; a TCP proxy alone cannot model QUIC path migration.

Record both client and host correlation IDs, route selection, ready time, replay result, and connection counts. Run on macOS and Linux CI. Keep relay tests in a separate job so external route variability cannot make the deterministic suite flaky.

### Layer 3: physical-device journeys

Run release-like builds on at least one current and one lower-performance iPhone and Android device. A simulator/emulator is suitable for correctness and orchestration, not energy or absolute latency.

- iOS: XCTest performance test around scan -> list -> connect -> first RPC; signposted phase metrics; Instruments Energy/Network; controlled Wi-Fi/cellular and Network Link Conditioner where available.
- Android: Macrobenchmark around the same UI journey; Perfetto trace sections; `PowerMetric`/NETWORK on a supported Pixel; `adb`-driven Doze and App Standby.
- Host: fixed hardware and loaded-host variants; direct-only, relay-only, and path-change cases; packet capture for keepalive bytes and intervals.
- Statistics: at least 30 warm and 30 cold iterations per deterministic local cohort; randomize arm order; report median, p95/p99 bootstrap intervals, failures, device thermal state, and all outliers rather than trimming them.

The official Android Doze procedure includes deterministic `adb shell dumpsys deviceidle force-idle`, `unforce`, battery reset, and App Standby commands, and documents that Doze suspends network access ([Android Doze/App Standby](https://developer.android.com/training/monitoring-device-state/doze-standby)).

### Existing validation run

The following narrow existing tests passed on 2026-07-15 with `REMORA_SKIP_ALLEYCAT_UPDATE=1`:

```text
cargo test -p codex-mobile-client --test pair
  3 passed; 0 failed

cargo test -p codex-mobile-client remote_runtime_worker_reconnects_and_retries_request_after_stream_drop
  1 passed; 0 failed

cargo test -p codex-mobile-client reconnect::tests
  40 passed; 0 failed
```

They validate happy-path legacy pair decisions, one reconnect-and-retry case, and reconnect planning. They do not exercise current QR-to-Alleycat user-journey latency, control-path deadlines, jitter, concurrent triggers, suspension, network handoff, relay fallback, replay drift, mutation ambiguity, or energy.

## Proposed benchmark and fault matrix

Every row must run for iOS and Android unless marked Rust-only or host-package-only. A pass includes the expected state invariant as well as latency; “eventually reconnects” is insufficient.

| ID | Journey / injected condition | Deterministic injection | Expected invariant | Primary measurements |
| --- | --- | --- | --- | --- |
| P1 | Valid payload, warm endpoint, direct path | Loopback host; no faults | One list request; choices match host; no secret fields logged | Pair-to-list phase histogram, connections opened |
| P2 | Cold endpoint | Fresh process/key loaded vs fresh key | Endpoint identity persistence works; cold bind separately attributed | Bind, resolve, direct/relay selection, total time |
| P3 | Relay-only | Host/direct route blocked; fixed relay | Ready within relay SLO; route labeled relay | QUIC/connect phases, relay bytes, errors |
| P4 | Host accepts list then changes availability | Versioned agent snapshot; selected runtime removed before Connect | Typed partial/permanent result; no indefinite stale use | Extra list calls, error latency, UI outcome |
| P5 | 1/3/7 selected runtimes | Host adds fixed per-runtime delay | First-ready/all-ready semantics preserved | Serial vs bounded-2 vs unbounded latency and peak dials |
| P6 | One runtime fails, others succeed | Reset one named runtime during initialize | Server can become usable; missing runtime named; selection intent retained | Ready count, partial deadline, later recovery |
| T1 | QUIC connect blackhole | Never complete scripted connect | Absolute journey deadline wins; task/resources released | Deadline accuracy, leaked tasks/sockets |
| T2 | Stream opens; control response never arrives | Host reads request and withholds response | Control deadline fires; failure typed transient | Phase timeout and retry schedule |
| T3 | Oversized/malformed frame | Host emits invalid length/JSON | Immediate protocol failure; bounded allocation; no retry storm | Error class, RSS peak, attempts |
| T4 | Protocol or token rejection | Fixed mismatch/invalid token | Permanent failure; zero automatic retries | Attempts, user action surfaced |
| R1 | Foreground stream EOF | Drop event stream at seeded sequence | One reconnect owner; replay from exact last sequence | Ready time, dials, replay count |
| R2 | Reset after request applied, before response | Host persists mutation then resets | No blind replay; authoritative reconcile yields one effect | Mutation apply count, uncertain outcome duration |
| R3 | Duplicate/late event | Duplicate and reorder seeded sequences | Reducer remains consistent; no duplicate UI effect | Dedup decisions, final snapshot hash |
| R4 | Replay cursor below host floor | Host returns drift-reload | Mandatory authoritative reload before recovered | Drift count, reconcile time, snapshot hash |
| R5 | Host restart | Kill daemon for 1/5/30 s | Backoff follows seeded schedule; recovery meets SLO after host ready | Attempts, peak dials, time from host-ready |
| R6 | Fifty clients resume together | Virtual clients, same outage, distinct seeds | No fixed-delay synchronization; host remains responsive | Dials/100 ms, CPU, p99 recovery |
| L1 | Suspend 5/14/16/30/120 s, path unchanged | Platform lifecycle harness | Monotonic threshold; <= 1 recovery journey | Close/migrate choice, triggers coalesced, ready time |
| L2 | Wi-Fi -> cellular/VPN/Tailscale | Network-generation script and device handoff | Old generation cancelled/joined; one immediate attempt on new usable path | Callback-to-ready, route changes, dials |
| L3 | Offline 30 s, then online | Unsatisfied/unvalidated path | Zero polling attempts while offline; immediate event-gated attempt | Attempts while offline, path-to-ready |
| L4 | Expensive/constrained/metered path | Device setting or injected fingerprint | No automatic subnet scan; policy visible in trace | Probes, bytes, radio energy |
| L5 | Android Doze/App Standby | Official `adb` commands | No assumption persistent socket survives; one coalesced foreground recovery | Background attempts, resume ready, energy |
| D1 | Discovery on populated /24 | Deterministic 253-host map, three ports | First useful result streams early; peak probes within chosen budget | First/complete result, 759 max candidates, peak sockets |
| D2 | Discovery cancelled on sheet close | Cancel at 100/500/2,000 ms | Native scan stops <= 250 ms; no detached probe tail | Post-cancel sockets/bytes/tasks |
| D3 | Tailscale absent/blackholed | Local API endpoints fail/blackhole | Source deadline does not hold complete scan; notice policy correct | Per-source and complete duration |
| D4 | Android DNS blocks `8.8.8.8` | Firewall only public resolver | Failure is typed; system/private DNS alternative arm measured | Resolver time, connect success, privacy/network compatibility |
| E1 | Foreground idle 1/3 runtimes | 60-minute physical-device run | Keepalive cadence/traffic meets budget; no discovery | Packets, bytes, PowerMetric/Instruments, thermal |
| E2 | Background idle, no voice | 60-minute background/Doze run | No app retry/discovery timers; clean foreground recovery | Wakeups, bytes, attempts, resume ready |
| H1 | Host daemon crash | Exit 42 after ready | OS supervisor sees failure and restarts; health reports generation | Exit visibility, restart and ready time |
| H2 | Upgrade during active sessions | Atomic binary replacement + service restart | No npm-cache path dependency; version changes once; clients recover | Downtime, rollback, version/health output |
| H3 | Package mismatch / optional package omitted | Root/platform version mismatch; `--omit=optional` | Fast actionable install/start error; never download executable in lifecycle script | Error clarity, exit code, network activity |

## Discovery and battery constraints

The Rust discovery defaults probe three ports, allow 64 host tasks concurrently, and define a 30-second continuous-scan interval ([defaults](../../shared/rust-bridge/codex-mobile-client/src/discovery.rs#L18)). A /24 sweep can examine 253 hosts and three ports each, or 759 TCP attempts. Because each host probes its three ports concurrently while the host semaphore is held, the instantaneous ceiling is roughly 192 socket attempts, not 64 ([LAN scan](../../shared/rust-bridge/codex-mobile-client/src/discovery.rs#L539)).

The inspected platform path uses a one-shot progressive scan when the discovery sheet opens, not the Rust continuous scanner ([iOS](../../apps/ios/Sources/Remora/Models/NetworkDiscovery.swift#L92), [Android](../../apps/android/app/src/main/java/com/remora/android/state/NetworkDiscovery.kt#L48)). However:

- iOS waits up to five seconds to collect Bonjour seeds before starting the Rust progressive sources ([`NetworkDiscovery.swift`](../../apps/ios/Sources/Remora/Models/NetworkDiscovery.swift#L149)).
- Android's two NSD browse operations take about four seconds before Rust scanning starts, with up to two more seconds for resolves ([`NetworkDiscovery.kt`](../../apps/android/app/src/main/java/com/remora/android/state/NetworkDiscovery.kt#L93)).
- Dropping/cancelling the platform subscription does not cancel the detached Rust scan task: the subscriber is created and the scan is spawned independently ([`mobile_client/mod.rs`](../../shared/rust-bridge/codex-mobile-client/src/mobile_client/mod.rs#L2542)). This is why D2 measures the socket tail after closing the sheet.
- Android's iroh endpoint uses `8.8.8.8:53` and embedded roots because the packaged Rust resolver cannot use the system surface ([`alleycat.rs`](../../shared/rust-bridge/codex-mobile-client/src/alleycat.rs#L652)). Enterprise, captive, private-DNS, filtered, and privacy-sensitive networks need explicit coverage.

The shared endpoint configures a 15-second QUIC keepalive ([`alleycat.rs`](../../shared/rust-bridge/codex-mobile-client/src/alleycat.rs#L619)). The value is an implementation hypothesis, not yet an energy result. QUIC's effective idle timeout is the minimum advertised by the peers, and keepalives prevent a healthy connection from going idle; RFC 9000 defines idle termination and connection migration behavior ([RFC 9000](https://datatracker.ietf.org/doc/html/rfc9000.html#section-10.1)). Measure packet cadence, NAT survival, reconnection delay, and energy at 5, 15, 30, and disabled-while-background intervals before changing it.

Apple recommends batching network work, waiting for connectivity, and limiting expensive or constrained access to reduce energy ([Apple networking power guidance](https://developer.apple.com/documentation/xcode/reducing-networking-and-bluetooth-power-usage)). iOS normally suspends ordinary background execution, so Remora must treat foreground recovery as a normal state transition rather than trying to preserve an immortal socket. Android Doze explicitly suspends network access and App Standby defers background network work; a fixed retry loop during either state is wasted work, not resilience ([Android Doze/App Standby](https://developer.android.com/training/monitoring-device-state/doze-standby)).

Pass full path attributes into shared policy as typed data: available/satisfied/validated, interface class, expensive/constrained/metered, and lifecycle state. Do not let Swift and Kotlin implement separate retry policies. Rust should decide whether to migrate, reconnect, defer, or suppress discovery; platforms only report OS facts and execute native measurement hooks.

## Native Rust daemon versus long-lived Node daemon

### Disposable experiment

A disposable harness under `/tmp` compared three trivial loopback daemons on this Mac. Each variant wrote readiness only after binding `127.0.0.1:0`; the harness measured process start to readiness, waited 250 ms, sampled resident set size with `ps`, then terminated it. There were 40 samples per arm. A second scenario made the ready daemon exit with code 42 after 50 ms.

Environment: Darwin 25.5.0 arm64, Mac17,6, 64 GiB; Node v24.13.0; npm 11.18.0; rustc 1.97.0. The Rust executable was compiled directly with `rustc`. No npm install, protocol stack, TLS, logging, persistence, or production dependencies were included.

| Variant | Ready median | Ready p95 | Idle RSS median | Idle RSS p95 | Crash visible to launcher caller? |
| --- | ---: | ---: | ---: | ---: | --- |
| Native Rust daemon, direct | 3.35 ms | 6.74 ms | 1,696 KiB | 1,696 KiB | Yes, when the supervisor owns it |
| Long-lived Node daemon | 22.38 ms | 24.67 ms | 46,672 KiB | 46,784 KiB | Yes; foreground process exited 42 |
| Node/npm-style launcher that detaches Rust | 22.65 ms | 25.98 ms | 1,696 KiB resident child | 1,696 KiB | No; launcher exited 0 before child exited 42 |

Directional result:

- The trivial native process was about 6.7x faster at median cold readiness than the thin Node launcher and used about 27.5x less idle RSS than the trivial long-lived Node process.
- A thin launcher still pays Node startup on every foreground invocation. Its persistent cost disappears after it exits, leaving the Rust child's RSS.
- The detached launcher deliberately demonstrated the operational trap: it returned success before the daemon later crashed. That is not an inherent weakness of Rust; it is a supervision bug. A foreground launcher must wait, forward signals, and mirror child exit status. A service install must delegate lifetime and restart to the OS.
- These numbers are not a production forecast. Real Rust and Node daemon sizes, startup work, dynamic libraries, allocator behavior, protocol dependencies, and platform security checks can materially change them. The experiment is strong enough to select the architecture for further packaging tests, not to set the host SLO by itself.

### Recommended host delivery and supervision

Use a zero-runtime-dependency npm launcher plus exact-version per-platform packages, but use npm only to deliver and invoke installation:

1. Resolve the exact native package for OS, CPU, and Linux libc. Fail clearly on missing optional dependencies, `--omit=optional`, root/platform version mismatch, or unsupported Windows ARM64; do not silently substitute x64.
2. Verify package version and artifact integrity. Avoid postinstall/lifecycle downloaders; the currently researched kittylitter npm 0.3.4 pattern downloads/extracts GitHub binaries in postinstall without verifying a published SHA-256 manifest.
3. Atomically copy the verified binary from the npm package/cache into a stable product-owned versioned path. Never register a service against the transient `npx`/npm exec cache.
4. Register launchd, systemd, or Windows Service Control Manager against that absolute path. Expose `version`, `health`, `pid`, running binary path, restart count, and last exit.
5. For foreground commands, invoke an absolute executable with an argument array and no shell; forward signals and return the child's exit status. Node documents detached/unreferenced children and the resulting independent lifetime in its [child process API](https://nodejs.org/api/child_process.html).
6. Publish platform packages under a staging tag, smoke-test every supported matrix entry, publish the root package last, and only then promote dist-tags. npm publication is not transactional.

The package matrix should explicitly test macOS arm64/x64, Linux arm64/x64 glibc, and Windows x64, with additional libc/architecture entries only when built and exercised. npm documents that optional dependencies can be omitted and that `os`, `cpu`, and `libc` select compatibility ([npm `package.json`](https://docs.npmjs.com/files/package.json/)); the launcher must turn a missing native package into a precise diagnostic.

Add these host-package measurements to H1-H3:

- cold `npx` bootstrap and warm installed command separately;
- service start-to-health, crash-to-restart, restart backoff, and restart-loop suppression;
- stable binary path across npm cache eviction and package upgrade;
- install/upgrade rollback after a killed copy, full disk, permission denial, signature/quarantine failure, or antivirus lock;
- argument paths containing spaces and Unicode;
- SIGINT/SIGTERM on Unix and console/service stop on Windows;
- stdout/stderr rotation and log path permissions; and
- root package/platform package version skew and staged publication rollback.

## Decision experiments

Run these in order because each one removes ambiguity for the next:

1. **Instrumentation-only baseline.** Add the shared timeline and no behavior change. Collect 30 cold and 30 warm runs for iOS/Android, direct/relay, and 1/3 runtimes. Success: every journey decomposes to phases and all attempts have one terminal outcome.
2. **Agent-list reuse.** A/B current second list versus a short-lived validated snapshot. Success: no stale-capability regression; at least one control round trip removed; pair/connect p95 improves.
3. **Runtime attachment concurrency.** Test serial, bounded two, and unbounded at 1/3/7 runtimes under idle and loaded host. Choose the smallest concurrency meeting primary/all-ready SLO without raising partial failures or peak host load.
4. **Reconnect state machine.** Compare current, full, equal, and decorrelated jitter with virtual time and client-fleet simulation. Then exercise the winning arm in the loopback host matrix. Success: one owner/server, no offline polling, bounded peak dials, recovery SLO met.
5. **Mutation ambiguity and replay.** Drop after host apply/before response for every mutating command kind. Success: one final effect, no generic replay, authoritative reconciliation terminates.
6. **Lifecycle and network generation.** Test exact 14/16-second boundary, clock changes, repeated path callbacks, Wi-Fi/cellular/VPN changes, iOS suspension, Android Doze. Success: monotonic classification and one recovery journey per generation.
7. **Keepalive and discovery energy.** Physical-device 60-minute tests for 5/15/30 seconds and background suppression, plus early/cancelled discovery. Choose based on recovery and energy jointly.
8. **Host package/service.** Run the full platform package matrix, crash/upgrade tests, and cold/warm startup comparison. Success: service path survives cache eviction, failures are visible, and restart/health SLOs pass.

## Release gates and residual unknowns

Do not call the reliability work complete until:

- all rows P1-P6, T1-T4, R1-R6, and L1-L5 pass deterministically;
- direct and relay physical-device p95/p99 meet the adopted SLOs;
- mutation ambiguity has a proven reconciliation path;
- a closed discovery sheet leaves no detached scan traffic;
- Android passes Doze/App Standby and filtered-DNS cases;
- idle packet/energy measurements justify the chosen keepalive policy; and
- the installed daemon is OS-supervised from a stable verified path on every supported host platform.

Unknowns that require experiments rather than more code reading:

- actual direct-versus-relay distributions and iroh path-selection time on user networks;
- the host's replay retention floor and event volume under long-running turns;
- app-server initialization latency on slow/loaded hosts and across all selected runtimes;
- whether a shorter initialize cap is safe;
- NAT survival and energy at each keepalive interval;
- real Rust daemon RSS/startup after all production dependencies and signing; and
- acceptable background semantics for active voice versus ordinary paired idle sessions.

Those unknowns are intentionally encoded in the matrix so that architectural choices can be made from distributions and invariants rather than anecdotal reconnect successes.
