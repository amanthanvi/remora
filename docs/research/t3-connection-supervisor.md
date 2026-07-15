# T3 connection-supervisor research for Remora

Status: recommendation only; no production code changed

Researched: 2026-07-15

Pinned sources: [T3 Code `ecb35f7`](https://github.com/pingdotgg/t3code/commit/ecb35f75839925dd1ac6f854efeef5c9e291d11b) and [Remora `f7b1420`](https://github.com/amanthanvi/remora/commit/f7b1420bb3226494c4cad07a0ef761452cccc762)

## Recommendation

Adopt a Rust-owned, per-environment connection supervisor behind a registry keyed by a stable `EnvironmentId`. Model a saved environment separately from the routes used to reach it, and model each route as a typed `AccessEndpoint` plus a typed `LaunchMethod`. Split connection work into resolve, launch/prepare, open, synchronize, and generation-checked commit stages.

This should be a hard runtime cutover, with a short-lived persistence importer and API adapter rather than two permanent connection paths. `AppStore` remains the canonical runtime state: its reducer owns desired/network state, phase, attempt, generation, failure, and retry deadline. The supervisor serializes commands, drives readiness and route selection, and owns only ephemeral timers, cancellation handles, and live connection resources. Native clients only persist typed catalog data, supply credentials and platform signals, invoke `AppClient`, and render `AppStore` projections.

The most important adaptation beyond T3 is to make Remora's generation an enforcement token. Every attempt, progress update, session installation, detached reader, warm-up, resubscribe, and failure must carry a `ConnectionGeneration`; a result may mutate the session map or `AppStore` only if its generation is still current. T3's structured, serial supervisor already scopes attempts, so its generation is primarily observational: it computes `nextGeneration`, runs a scoped attempt, and only advances the stored generation after establishment ([supervisor](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/packages/client-runtime/src/connection/supervisor.ts#L587-L623)). Remora currently has detached work and overlapping reconnect mechanisms, so cancellation alone is not a sufficient stale-result defense.

## What T3 actually establishes

### The architectural vocabulary is sound, but it is ahead of the concrete types

T3 defines an `ExecutionEnvironment` as one running server with a stable identity that owns projects, threads, terminal processes, filesystem access, provider state, and settings. It defines an `AccessEndpoint` as one way to reach that same environment, allowing several paths without duplicating the environment. Endpoint providers normalize candidates, while core owns connection lifecycle ([remote architecture](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/docs/architecture/remote.md#L63-L150)). It separately asks how a client reaches a server and how that server comes to exist; SSH may assist both, but normal traffic remains on the ordinary environment transport ([access and launch methods](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/docs/architecture/remote.md#L190-L306)).

The code is not yet a literal implementation of all three named abstractions:

- `ExecutionEnvironmentDescriptor` is a first-class contract with `environmentId`, label, platform, server version, and capabilities ([contract](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/packages/contracts/src/environment.ts#L6-L35)).
- `AdvertisedEndpoint` is also typed, but it is explicitly a server- or provider-authored candidate with reachability and compatibility metadata, not the whole saved access model ([contract](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/packages/contracts/src/remoteAccess.ts#L5-L68)).
- The older `KnownEnvironment` still stores one HTTP/WebSocket target and only learns the authoritative environment ID after connecting ([known environment](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/packages/client-runtime/src/environment/knownEnvironment.ts#L3-L40)).
- The active runtime uses tagged `ConnectionTarget` variants for primary, bearer, relay, and SSH connections. A catalog entry combines one target with an optional profile ([connection model](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/packages/client-runtime/src/connection/model.ts#L4-L56), [catalog](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/packages/client-runtime/src/connection/catalog.ts#L13-L42)).
- There are no concrete types named `AccessEndpoint` or `LaunchMethod` at this revision. The SSH resolver currently performs launch/forward preparation and returns an ordinary prepared connection, so those responsibilities are behaviorally separated but still packaged behind one target/profile path ([SSH resolver](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/packages/client-runtime/src/connection/resolver.ts#L189-L241)).

Therefore Remora should adopt the vocabulary and separation, not copy the current TypeScript shapes verbatim. In particular, Remora already has environments with several viable access paths, so one `ConnectionTarget` per catalog entry would preserve an existing limitation.

### The useful part to copy is the supervisor lifecycle

T3's runtime has a per-environment supervisor with explicit desired state, network state, phases, attempt stages, failure classification, retry time, and generation ([model](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/packages/client-runtime/src/connection/model.ts#L58-L173)). A driver resolves a target, opens a session, reports `preparing`/`opening`/`synchronizing`, and does not return a lease until the session is ready ([driver](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/packages/client-runtime/src/connection/driver.ts#L15-L58)). Readiness means more than an open socket: it waits for the WebSocket and the first `serverGetConfig`, racing both against disconnect ([RPC session](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/packages/client-runtime/src/rpc/session.ts#L70-L138)).

The supervisor then supplies the operational semantics Remora is missing:

- transient and blocked errors are distinct;
- a setup/readiness attempt has a 15-second deadline;
- transient failures retry indefinitely at 1, 2, 4, 8, then 16 seconds;
- 30 seconds of stability resets the failure count;
- offline and explicit disconnect release the scoped lease;
- retry and network signals interrupt waits;
- foreground wakeups probe a healthy lease instead of reconnecting it blindly; and
- a registry gives each environment a separately scoped supervisor and replaces that scope when its catalog entry changes.

These behaviors are visible in the [supervisor constants and API](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/packages/client-runtime/src/connection/supervisor.ts#L32-L47), [attempt and retry loop](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/packages/client-runtime/src/connection/supervisor.ts#L456-L667), [foreground probe handling](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/packages/client-runtime/src/connection/supervisor.ts#L386-L454), and [registry scope management](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/packages/client-runtime/src/connection/registry.ts#L247-L386). T3 tests the high-risk cases with a virtual clock, including infinite capped backoff, readiness timeout, blocked-idle behavior, offline release, flapping, involuntary close, probe interruption, credential changes, and concurrent signals ([representative tests](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/packages/client-runtime/src/connection/supervisor.test.ts#L289-L434), [lease and signal tests](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/packages/client-runtime/src/connection/supervisor.test.ts#L466-L528), [reconnect tests](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/packages/client-runtime/src/connection/supervisor.test.ts#L590-L653), [probe and concurrency tests](https://github.com/pingdotgg/t3code/blob/ecb35f75839925dd1ac6f854efeef5c9e291d11b/packages/client-runtime/src/connection/supervisor.test.ts#L728-L845)).

## Remora's current gaps

### Environment, access, launch, and credentials are flattened together

`SavedServerRecord` combines identity, hostname, direct ports, an untyped source and preferred mode, WebSocket URL, SSH flags, Alleycat node/relay/agent fields, and an Alleycat token in one record ([saved record](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/shared/rust-bridge/codex-mobile-client/src/reconnect.rs#L19-L46)). `ReconnectPlan` then turns that record into mutually exclusive SSH, SSH bridge, local, direct, URL, Slingshot, or Alleycat branches ([plan variants](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/shared/rust-bridge/codex-mobile-client/src/reconnect.rs#L96-L142)). The decision tree gives one mode priority over another, including string comparisons for SSH and special URL parsing for Slingshot ([plan selection](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/shared/rust-bridge/codex-mobile-client/src/reconnect.rs#L287-L417)).

This has three concrete costs:

1. one saved record cannot represent several ranked routes cleanly;
2. access policy is coupled to how a target is launched; and
3. route-specific fields and credentials leak across the general persistence/UniFFI boundary.

iOS still owns a parallel flat `SavedServer` and converts enum values back to raw strings for Rust ([iOS record](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/apps/ios/Sources/Remora/Models/SavedServer.swift#L3-L24), [Rust conversion](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/apps/ios/Sources/Remora/Models/SavedServer.swift#L282-L305)). That is a persistence adapter doing connection-domain work, even though both app shells are otherwise already thin projections over Rust `AppStore` and Rust bridges ([iOS bridge ownership](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/apps/ios/Sources/Remora/Models/AppModel.swift#L25-L73), [Android thin-store contract](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/apps/android/app/src/main/java/com/remora/android/state/AppModel.kt#L52-L58)).

### Reconnect is a one-shot operation, not persistent supervision

`ReconnectController` holds a global reconnect mutex, snapshots every saved server, computes plans, and launches all current plans in a `JoinSet`. A second trigger is skipped while that pass holds the guard ([controller state](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/shared/rust-bridge/codex-mobile-client/src/ffi/reconnect.rs#L64-L74), [reconnect pass](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/shared/rust-bridge/codex-mobile-client/src/ffi/reconnect.rs#L340-L454)). It does not retain per-environment desired state, a current attempt, retry deadline, failure classification, or lease generation. A foreground probe logs failures but does not itself drive a state transition, then the controller performs a full saved-server reconnect pass ([probe and foreground flow](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/shared/rust-bridge/codex-mobile-client/src/ffi/reconnect.rs#L209-L268)).

There is also a second retry owner inside each remote session: it retries five times with a fixed one-second delay before declaring the transport disconnected ([constants](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/shared/rust-bridge/codex-mobile-client/src/session/connection.rs#L32-L33), [inner retry loop](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/shared/rust-bridge/codex-mobile-client/src/session/connection.rs#L1247-L1311)). Two retry owners cannot reliably present one attempt number, one backoff schedule, or one cancellation contract.

### Stale work is partially defended, but not end to end

Event and health readers check that their `Arc<ServerSession>` is still the session-map value before processing, which is a useful local stale-reader defense ([pointer guard](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/shared/rust-bridge/codex-mobile-client/src/mobile_client/event_loop.rs#L19-L45), [health reader](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/shared/rust-bridge/codex-mobile-client/src/mobile_client/event_loop.rs#L172-L234)). It does not guard work before a session is installed, nor every detached warm-up/resubscribe/store mutation.

The source already documents a concrete race: saved-server reconnect can tear down a session that an inner Alleycat retry has just healed while a post-reconnect resubscribe is running ([race comment](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/shared/rust-bridge/codex-mobile-client/src/mobile_client/mod.rs#L689-L697)). This is evidence for one connection owner and an explicit generation token, not just more special-case session reuse.

### Ready, disconnected, and forgotten are currently conflated

Remote clients perform the Codex initialization handshake, but the multiplexed session publishes `Connected` immediately after worker creation ([remote connect](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/shared/rust-bridge/codex-mobile-client/src/session/connection.rs#L807-L903)). `MobileClient` then upserts `Connected`, installs the session, and starts asynchronous warm-up ([attach path](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/shared/rust-bridge/codex-mobile-client/src/mobile_client/mod.rs#L547-L597)). Separately, local process readiness only polls for a WebSocket upgrade, not a full app-server initialization/synchronization boundary ([local readiness](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/shared/rust-bridge/codex-mobile-client/src/local_server/mod.rs#L30-L36), [attach/spawn](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/shared/rust-bridge/codex-mobile-client/src/local_server/mod.rs#L195-L233)).

Finally, `disconnect_server` removes the session and the server from `AppStore` ([disconnect](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/shared/rust-bridge/codex-mobile-client/src/mobile_client/mod.rs#L1261-L1289)); `AppStore::remove_server` also removes that server's threads and related state ([reducer](https://github.com/amanthanvi/remora/blob/f7b1420bb3226494c4cad07a0ef761452cccc762/shared/rust-bridge/codex-mobile-client/src/store/reducer.rs#L246-L275)). A supervisor must be able to release a failed, offline, or superseded lease without forgetting the environment or deleting its conversation state.

## Proposed Rust domain model

The exact field names can follow existing crate conventions, but the boundaries should be explicit:

```rust
#[derive(Clone, Debug, Eq, Hash, uniffi::Record)]
pub struct EnvironmentId {
    pub value: String,
}

#[derive(Clone, Debug, Eq, Hash, uniffi::Record)]
pub struct RouteId {
    pub value: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, uniffi::Record)]
pub struct ConnectionGeneration {
    pub value: u64,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct ExecutionEnvironment {
    pub id: EnvironmentId,
    pub display_name: String,
    pub descriptor: Option<ExecutionEnvironmentDescriptor>,
}

#[derive(Clone, Debug, uniffi::Enum)]
pub enum AccessEndpoint {
    InProcess,
    WebSocket { url: String },
    Iroh { node_id: String, relay_hint: Option<String> },
    SshForward { profile_id: String },
    Slingshot { base_url: String, remote_environment_id: String },
}

#[derive(Clone, Debug, uniffi::Enum)]
pub enum LaunchMethod {
    PreExisting,
    Embedded,
    LocalProcess { profile_id: String },
    SshBootstrap { profile_id: String },
    PairedHostAgent { agent_names: Vec<String> },
    ManagedService,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct EnvironmentRoute {
    pub id: RouteId,
    pub access: AccessEndpoint,
    pub launch: LaunchMethod,
    pub priority: i32,
    pub enabled: bool,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct EnvironmentCatalogEntry {
    pub environment: ExecutionEnvironment,
    pub routes: Vec<EnvironmentRoute>,
    pub preferred_route_id: Option<RouteId>,
}
```

This is deliberately a catalog, not live connection state. Route selection may consider preference, current network, last typed failure, and capability, but it must be deterministic and Rust-owned. Credentials are referenced by stable profile or endpoint identity and loaded at preparation time; secrets must not appear in catalog records, `AppStore` snapshots, errors, or logs.

Keep the current `server_id` as the initial `EnvironmentId` during migration. That avoids re-keying `ThreadKey(server_id, thread_id)` and persisted conversation state. When a handshake later supplies a stronger server-authored identity, record an alias and merge only through an explicit, tested catalog operation; do not silently re-key during connection.

### Supervisor state and commands

```rust
pub enum SupervisorPhase {
    Available,
    Offline,
    Connecting,
    Backoff,
    Ready,
    Blocked,
}

pub enum ConnectionStage {
    Resolving,
    Launching,
    Opening,
    Synchronizing,
}

pub enum ConnectionFailureKind {
    Transient(TransientFailure),
    Blocked(BlockedFailure),
}

pub struct ConnectionSupervisorSnapshot {
    pub environment_id: EnvironmentId,
    pub desired: bool,
    pub network: NetworkStatus,
    pub phase: SupervisorPhase,
    pub stage: Option<ConnectionStage>,
    pub attempt: u32,
    pub generation: ConnectionGeneration,
    pub selected_route_id: Option<RouteId>,
    pub last_failure: Option<ConnectionFailure>,
    pub retry_at_unix_ms: Option<u64>,
}

enum SupervisorCommand {
    SetDesired(bool),
    RetryNow,
    NetworkChanged(NetworkStatus),
    ApplicationActive,
    CredentialsChanged(CredentialScope),
    CatalogChanged(u64),
    AttemptProgress { generation: ConnectionGeneration, stage: ConnectionStage },
    AttemptFinished { generation: ConnectionGeneration, result: AttemptResult },
    LeaseClosed { generation: ConnectionGeneration, failure: ConnectionFailure },
}
```

One Tokio task serializes commands for one environment. Attempts run in child tasks so a slow environment does not block its actor or other environments. The actor holds a cancellation token and optional live lease; the authoritative generation is in `AppStore`. Every invalidation advances the monotonic epoch through the reducer before cancellation, and every new attempt receives a fresh epoch. The reducer accepts completion only when all of these are true:

```text
completion.generation == active_generation
desired == true
network != offline
catalog revision and selected route still match
```

The actor owns only ephemeral control mechanics and resource handles. It asks the `AppStore` reducer to validate and apply state transitions. Lease commit is one ordered actor action: recheck the authoritative generation, install the matching session, then publish `Ready` before processing another command. Native code observes only the resulting `AppStore` record. That preserves one canonical runtime state rather than creating a second platform or supervisor cache.

### Connection driver and readiness

Use a prepare/commit boundary:

1. **Resolve:** choose a viable typed route without mutating the session map or `AppStore`.
2. **Launch:** start/reuse the target and create any SSH, iroh, Slingshot, or local-process resources.
3. **Open:** construct the runtime client(s) and transport.
4. **Synchronize:** complete each required runtime's protocol initialization and a bounded, runtime-specific readiness probe.
5. **Commit:** send the prepared lease to the actor; install it only after the generation check, then publish `Ready` to `AppStore`.

For Codex, client construction already performs `initialize`/`initialized`; treat that as the minimum protocol boundary and add a cheap pre-auth-safe liveness request if needed. For a multiplexed environment, all required runtime clients must be initialized before commit. Full thread hydration, model loading, and conversation warm-up can continue after `Ready`, but those tasks must also carry the generation. A raw TCP or WebSocket upgrade is never sufficient readiness.

The prepared lease must own every cleanup resource: session, SSH tunnel/process, local child, iroh keepalive, callback tunnel, and reader tasks. Dropping or explicitly closing the lease must be idempotent. Add an internal `release_connection(environment_id, generation)` that removes only the matching live session and resources and updates health; reserve `forget_environment(environment_id)` for the explicit destructive operation that removes server/thread state.

### Retry, network, and wakeup policy

- The outer supervisor is the only long-lived retry owner.
- Transient failures retry indefinitely while desired and online, with exponential full jitter, a one-second base, and a 30-second cap. `retryNow` cancels the timer.
- A lease that stays healthy for 30 seconds resets the failure count. Short-lived flaps continue escalating.
- Authentication, missing credentials, invalid configuration, permission, unsupported route, and pairing-required failures are `Blocked`; they have no retry timer and wait for a relevant signal.
- Within one retry cycle, try each eligible route at most once in deterministic priority order. Back off only after the cycle is exhausted; a route-specific blocked failure disables that route for the cycle, and the environment becomes blocked only when no viable route remains.
- Identity, authorization-scope, or endpoint-pinning failures must never trigger an automatic downgrade to an unpinned or less trusted route.
- Offline cancels the current attempt or releases the lease and pauses without increasing failure count.
- An online interface change first passes a transport-specific network hint and probes the current lease. It reconnects only when the probe fails or times out.
- Application activation probes a healthy lease rather than starting a parallel reconnect pass.
- Credential changes restart only a blocked attempt or a live route whose credential scope changed.

The existing five-attempt transport loop may remain temporarily as bounded within-lease healing, but it must not own desired state, outer backoff, or `AppStore` phase. It should eventually either heal transparently within a short budget or report `LeaseClosed` to the supervisor. There must be one displayed attempt count and one retry deadline.

## Design comparison

| Design | Stale-completion safety | Isolation | Testability | Migration cost | Assessment |
| --- | --- | --- | --- | --- | --- |
| **A. Per-environment actor plus registry** | Strong with generation-checked messages | Strong; one slow/flapping environment does not block another | Strong with paused Tokio time and command traces | Medium | **Recommended.** Closest to T3 while fitting Rust, multiple environments, and `AppStore`. |
| **B. One global actor for all environments** | Strong if every effect is tagged | Weaker; heavy work or a bug can create global head-of-line blocking | Good; one deterministic event log | Medium | Viable for a very small environment count, but still needs child tasks and per-environment generations, recreating much of A inside one actor. |
| **C. Pure reducer plus external effect runner/timer wheel** | Strongest if effect IDs and cancellation are exhaustive | Strong | Excellent for exhaustive model/property testing and replay | High | Architecturally clean, but introduces an effect runtime and timer/cancellation protocol larger than the current problem warrants. |
| **D. Independent retry tasks around current `ReconnectPlan`** | Weak; cancellation and late completion remain distributed | Superficially strong | Poor for global invariants | Low initially, high over time | Reject. This is the current failure mode with more tasks: duplicate retry owners, ad hoc guards, and no authoritative lease epoch. |

Design A is the smallest design that makes the important invariants local. The registry manages catalog lifecycle and exposes `connect`, `disconnect`, `retry`, and signals; each actor serializes one environment; the driver owns scoped effects; `AppStore` owns the projection.

## Small generation-model experiment

I exhaustively enumerated every event prefix through depth five over this alphabet:

```text
start, disconnect, success(1), success(2), success(3)
```

The checked invariant was: an installed result must belong to the current generation and the environment must still be desired. There were 3,906 prefixes (`sum(5^0 ... 5^5)`).

- A naive model accepted success from any generation that had once started. It produced 299 violating prefixes. The shortest were `start, start, success(1)` and `start, disconnect, success(1)`.
- A guarded model accepted success only when `desired && completion_generation == current_generation`. It produced zero violations in the same search.

This is a deliberately small model, not a proof of the production implementation. It isolates the property that cancellation and pointer checks do not express: late success must be harmless after supersession or disconnect. The production test suite should extend it with route replacement, offline/online, blocked errors, readiness timeouts, lease closure, credential changes, and multiple environments.

## Public boundary and native responsibilities

Keep `AppStore` minimal: snapshots, subscriptions, and internal reducer transitions. Put direct operations on `AppClient`, for example:

```text
upsertEnvironment(entry)
forgetEnvironment(environmentId)
connectEnvironment(environmentId)
disconnectEnvironment(environmentId)
retryEnvironment(environmentId)
noteNetworkState(status, fingerprint)
noteApplicationActive()
noteCredentialsChanged(scope)
```

Add `ConnectionSupervisorSnapshot` to each environment/server projection in the existing `AppSnapshotRecord`. Preserve a coarse health projection for UI compatibility during migration, but derive it from supervisor phase rather than maintaining it independently. `Ready` maps to connected; resolving/launching/opening/synchronizing map to connecting; available/offline/backoff/blocked map to disconnected plus their typed detail.

`DiscoveryBridge` should emit typed candidate routes or route ingredients. `SshBridge` remains a utility used by the Rust resolver. Swift and Kotlin should only:

- store/load a versioned catalog payload or typed records;
- provide secure credential callbacks and platform permissions;
- provide OS network, foreground, and background signals;
- call the direct `AppClient` operations; and
- render `AppStore` snapshots.

They should not rank routes, parse mode strings, infer retry policy, decide whether a probe implies reconnect, or keep a separate connection state machine.

## Persistence migration

Introduce `EnvironmentCatalogV2` with a Rust-owned codec and a one-time importer from `SavedServerRecord` V1. Storage mechanics may remain in `UserDefaults` and `SharedPreferences`, but both platforms should persist the same versioned shape without interpreting it. Generate deterministic route IDs from normalized non-secret route identity.

| V1 evidence | V2 access | V2 launch | Migration rule |
| --- | --- | --- | --- |
| `source == local` | `InProcess` | `Embedded` | Keep environment ID `local`. |
| ordinary `websocket_url` | `WebSocket` | `PreExisting` | Preserve URL; validate scheme and redact credentials. |
| Slingshot marker URL | `Slingshot` | `ManagedService` | Parse once; store remote environment ID separately. |
| Alleycat node ID plus agent | `Iroh` | `PairedHostAgent` | Store credential reference only, never token. |
| SSH bridge marker | `SshForward` | `SshBootstrap` | Preserve runtime/agent selection in launch metadata. |
| preferred mode `ssh` | `SshForward` | `SshBootstrap` | Block with `Authentication` if credentials are absent. |
| selected direct Codex port | `WebSocket` | `PreExisting` | Materialize a normalized direct endpoint. |
| legacy Alleycat host/UDP only | none | none | Import as `Blocked(PairingRequired)`; do not guess an iroh identity. |

Do not permanently write both V1 and V2. Keep the V1 reader for one defined migration window, write only V2 after successful import, and retain a non-secret V1 backup until that installation has loaded V2 successfully on a subsequent launch. Verify this path against both platform fixture sets, then remove V1 record construction and string-mode helpers after the migration window.

## Incremental implementation sequence

1. **Types and fixtures, no behavior change.** Add the typed catalog, failures, supervisor snapshot, V1-to-V2 importer, and reducer projection. Build fixtures from current iOS and Android persisted records.
2. **Prepare/commit and generation guards.** Extract route resolution and session construction from `execute_reconnect_plan`/`MobileClient` so attempts return an uncommitted lease. Add generation checks to session installation, event and health readers, warm-up, resubscribe, and cleanup.
3. **Per-environment supervisors.** Add the Rust registry and actors behind existing UniFFI construction. Route lifecycle/network/retry callbacks into commands. Project every transition through `AppStore`.
4. **Both-platform hard cutover.** Make iOS and Android call the new `AppClient` methods in the same change. Keep `ReconnectController` only as a one-release compatibility adapter that forwards to the registry; it must not retain a second algorithm.
5. **Persistence cutover.** Import V1, write V2, remove platform route/mode decision helpers, and verify cold launch from representative old records on both platforms.
6. **Delete the old path.** Remove `ReconnectPlan`, global reconnect guard/`JoinSet` orchestration, V1 writes, and any inner retry behavior that competes with supervisor policy. Remove the compatibility adapter once no native caller uses it.

## Required invariants and validation

Treat these as acceptance criteria, not optional tests:

1. At most one current attempt or committed lease exists per environment.
2. Only the current generation may mutate the session map, connection projection, or post-connect state.
3. `Ready` implies the current lease is installed and protocol readiness completed.
4. No attempt runs while desired is false or the network is offline.
5. Blocked failures have no timer; transient backoff is bounded; 30 seconds stable resets it.
6. Disconnect, route replacement, catalog removal, and credential-invalidating changes advance the generation before cancellation.
7. Lease loss preserves the environment and threads; only explicit forget removes them.
8. Secrets never appear in catalog payloads, `AppStore`, errors, tracing, or generated debug logs.
9. One environment's flapping, blocked credentials, or slow launch cannot delay another environment.
10. iOS and Android observe the same typed phases and retry behavior from Rust.

Validation should include:

- pure transition tests plus `proptest` command sequences for the invariants above;
- Tokio paused-time tests for readiness deadlines, jitter bounds, backoff cap, stable reset, and interrupted timers;
- a slow generation-1 attempt followed by generation 2, then late generation-1 success and failure, both ignored;
- disconnect during resolve, launch, opening, synchronization, and foreground probe;
- route replacement and credential change during each stage;
- network loss/regain, interface change with successful probe, and failed probe;
- flapping leases, blocked-idle behavior, manual retry, and multi-environment concurrency;
- V1 fixtures from both platform stores, including legacy Alleycat records and missing credentials;
- regenerated UniFFI bindings plus Swift/Kotlin compile tests; and
- fast iOS simulator and Android emulator smoke tests that compare the same phase sequence.

## Residual decisions

Three details should be settled during implementation from protocol evidence, without changing the architecture:

1. Which cheap Codex request is guaranteed to be safe before account login and should serve as the post-initialize readiness probe. If initialization alone is authoritative, encode that in the runtime adapter rather than inventing a generic RPC.
2. Whether full jitter should cap at 16 seconds to match T3 or 30 seconds for mobile network conditions. The state machine and tests should take a policy value, but this need not become user configuration.
3. How an authoritative server environment ID aliases a migrated local `server_id`. Start by preserving existing IDs; make any later merge explicit because it affects persisted thread keys.

None of these decisions justify retaining the current flat record or multiple reconnect owners.
