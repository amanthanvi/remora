# Current backend map: remote-host pairing

Date: 2026-07-15
Scope: current Remora QR pairing, generic discovery, Alleycat transport, reconnect, persistence, paired-host terminal, upstream dependencies, and host bootstrap. This is a code-grounded architecture report; it does not change production code.

## Conclusion

Remora does not currently have a deep remote-host pairing module. The user-visible operation called “pairing” is distributed across a parse-only `AlleycatBridge`, the mixed-purpose `ServerBridge`, the broad `MobileClient`, a generic saved-server reconnect planner, the app-server session worker, a separate paired-terminal backend, two platform credential stores, two platform profile stores, and native lifecycle schedulers. The nominal pairing interface hides almost no policy; callers must know how to parse, probe, select agents, connect, persist three pieces of state, reconnect, and forget.

That shape conflicts with the repository contract: shared protocol, discovery policy, reconciliation, and remote-terminal behavior belong in Rust; Swift and Kotlin should own UI and persistence adapters, and shared behavior should cross one handwritten UniFFI seam (`CONTEXT.md:23-30`). Pairing is also a primary product capability, not setup plumbing (`PRODUCT.md:14-21`, `PRODUCT.md:37-46`).

The safest target is a Rust `RemoteHostPairing` module that owns the paired-host lifecycle end to end while injecting one real external seam: platform secure/profile persistence. Generic network discovery should remain a separate module. The existing `RemoteTransport` trait should also remain: it is already a narrow, justified internal seam shared by Alleycat, SSH, and Slingshot (`shared/rust-bridge/codex-mobile-client/src/session/remote_transport.rs:1-80`).

The migration can be made independently revertible by introducing typed domain state behind the existing calls, moving one policy cluster at a time, preserving the external Alleycat wire and existing storage keys initially, and retaining old UniFFI methods only as time-bounded forwarding adapters. Production cutover should still happen on iOS and Android in the same phase.

## Current dependency and ownership map

```text
Host machine (outside this repository)
  npx kittylitter
    -> Alleycat daemon
    -> stable iroh node id + token + optional relay
    -> ALPN alleycat/1
    -> list_agents / restart_agent / connect
    -> websocket or JSONL runtime stream
                   |
                   v
Remora platform UI
  QR scanner / paste JSON
    -> AlleycatBridge.parsePairPayload          (parse only)
    -> ServerBridge.listAlleycatAgents          (probe)
    -> ServerBridge.connectRemoteOverAlleycat   (connect)
                   |
                   v
Rust MobileClient
  one app-wide iroh Endpoint
  one AlleycatReconnectTransport per selected runtime
  one multiplexed ServerSession per paired host
  AppStore runtime projection
                   |
       +-----------+------------+
       |                        |
       v                        v
platform persistence       paired terminal
  profile store              screen loads profile + token
  token store                -> shell JSONL stream
  device-key store
       |
       v
ReconnectController
  platform pushes SavedServerRecord[] + embedded token
  generic plan selects Alleycat/SSH/direct/Slingshot/local
  MobileClient recreates paired session
```

The main ownership points are:

| Concern | Current owner | Observation |
| --- | --- | --- |
| Pair-payload validation and host wire | `alleycat.rs` | Handwritten client copy of the host protocol, including compatibility aliases and replay fields (`shared/rust-bridge/codex-mobile-client/src/alleycat.rs:22-33`, `shared/rust-bridge/codex-mobile-client/src/alleycat.rs:291-369`). |
| Public pairing records and parsing | `ffi/alleycat.rs` | Many records, but the object implements only construction and parsing (`shared/rust-bridge/codex-mobile-client/src/ffi/alleycat.rs:6-38`, `shared/rust-bridge/codex-mobile-client/src/ffi/alleycat.rs:84-102`). |
| Probe and connect | `ServerBridge` | Alleycat calls sit beside direct, SSH, Slingshot, disconnect, and restart operations (`shared/rust-bridge/codex-mobile-client/src/ffi/discovery.rs:70-80`, `shared/rust-bridge/codex-mobile-client/src/ffi/discovery.rs:288-340`). |
| Endpoint identity, agent cache, session construction, restart target | `MobileClient` | Pairing state is mixed into a facade that also owns all sessions, AppStore, discovery, auth, caches, voice-related runtime, and terminals (`shared/rust-bridge/codex-mobile-client/src/mobile_client/mod.rs:66-140`). |
| Runtime health and hot reconnect | `ServerSession` workers | Each selected runtime has a worker and transport, but all workers write one host health channel (`shared/rust-bridge/codex-mobile-client/src/session/connection.rs:824-903`). |
| Cold reconnect selection | `reconnect.rs` + `ffi/reconnect.rs` | A generic, stringly `SavedServerRecord` is mirrored from both platforms and planned independently of the live transport worker (`shared/rust-bridge/codex-mobile-client/src/reconnect.rs:17-46`, `shared/rust-bridge/codex-mobile-client/src/ffi/reconnect.rs:64-150`). |
| Durable paired-host profile | Swift/Kotlin stores | Platform JSON contains node, relay, comma-separated agents, wire, and legacy fields; secret token is loaded separately (`apps/ios/Sources/Remora/Models/SavedServer.swift:3-24`, `apps/android/app/src/main/java/com/remora/android/state/SavedServerStore.kt:15-40`). |
| Token and iroh device key | Swift Keychain / Android encrypted prefs | Correctly platform-owned storage, but lifecycle policy and ordering leak into UI and app startup (`apps/ios/Sources/Remora/Models/AlleycatCredentialStore.swift:21-50`, `apps/ios/Sources/Remora/Models/AlleycatCredentialStore.swift:86-149`, `apps/android/app/src/main/java/com/remora/android/state/AlleycatCredentialStore.kt:6-44`). |
| Generic network discovery | Rust plus native seed/cache logic | Rust owns multi-source scanning and reconcile, but iOS adds a second cache/merge policy; Android is thinner (`shared/rust-bridge/codex-mobile-client/src/discovery.rs:174-204`, `apps/ios/Sources/Remora/Models/NetworkDiscovery.swift:54-73`, `apps/android/app/src/main/java/com/remora/android/state/NetworkDiscovery.kt:27-80`). |
| Paired terminal | Separate Rust backend plus native credential lookup | It reconstructs a pair payload and opens a special `shell` stream; it does not share paired-profile or reconnect policy (`shared/rust-bridge/codex-mobile-client/src/terminal/remote_alleycat.rs:20-92`). |

`AppStore` is not a durable pairing store. Its server snapshot contains runtime identity, display/host, health, account, models, runtime projections, and diagnostics, but no token, relay, selected-agent intent, or durable pairing phase (`shared/rust-bridge/codex-mobile-client/src/store/snapshot.rs:171-195`). Its reducer only upserts the runtime projection from `ServerConfig` and preserves runtime fields (`shared/rust-bridge/codex-mobile-client/src/store/reducer.rs:169-243`). That separation is sound; the missing piece is a dedicated paired-host module adjacent to the runtime store, not more fields on `AppStore`.

## End-to-end flow

### 1. Host bootstrap and dependency resolution

The only in-repository user contract for starting a host is `npx kittylitter` (`README.md:64-68`). That exact command is repeated in the iOS pairing form and scanner, the iOS chooser, the Android pairing sheet, and the Android chooser (`apps/ios/Sources/Remora/Views/RemotePairingSheet.swift:148-153`, `apps/ios/Sources/Remora/Views/RemotePairingSheet.swift:221-228`, `apps/ios/Sources/Remora/Views/RemotePairingSheet.swift:537-542`, `apps/ios/Sources/Remora/Views/DiscoveryView.swift:251-260`, `apps/android/app/src/main/java/com/remora/android/ui/discovery/RemotePairingSheet.kt:611-616`, `apps/android/app/src/main/java/com/remora/android/ui/discovery/DiscoveryScreen.kt:511-519`). The host package and daemon implementation are not in this repository, so the command, payload, ALPN, and host responses are external compatibility contracts.

The Rust workspace pins four Alleycat crates—bridge core, Pi, Claude, and OpenCode bridges—to a reviewed commit from Aman's fork (`shared/rust-bridge/Cargo.toml:28-31`). The current lock resolves those packages and the transitive `alleycat-codex-proto` to commit `3c6dfe2c6b060864d8cb0fcae58f73a6ed1ea10f` (`shared/rust-bridge/Cargo.lock:313-394`). These dependencies do **not** provide Remora's remote-host client protocol: Remora hand-defines that in `alleycat.rs`. The crates are consumed primarily by the SSH multi-agent bridge and shared Codex resolver (`shared/rust-bridge/codex-mobile-client/src/ssh_bridge.rs:10-35`, `shared/rust-bridge/codex-mobile-client/src/local_server/mod.rs:40-79`, `shared/rust-bridge/codex-mobile-client/src/ssh_launcher.rs:7-30`).

Normal Rust build/check/test lanes compile that pin without fetching a newer revision (`Makefile:350-378`, `Makefile:405-408`). Updating it is an explicit `make update-remora-link REV=<40-character-commit>` operation: the script verifies that the SHA exists in Aman's fork, refuses to overwrite dirty manifest/lock changes, backs both files up, updates all four packages precisely, and restores the originals on failure (`Makefile:352-354`, `tools/scripts/update-remora-link.sh:10-59`). This removes build-time dependency drift. One compatibility risk remains: host daemon behavior and Remora's handwritten remote-host wire are not type-shared with the pinned bridge crates, so a reviewed dependency/host update still needs protocol fixtures and migration validation. The repository's deterministic verification gate no longer needs a skip-update environment variable (`CONTEXT.md:64-73`).

### 2. App startup and iroh device identity

`MobileClient` owns one lazily initialized iroh endpoint and one optional 32-byte secret key. The first Alleycat operation captures the current key; setting a key after endpoint initialization cannot change that endpoint (`shared/rust-bridge/codex-mobile-client/src/mobile_client/mod.rs:98-117`, `shared/rust-bridge/codex-mobile-client/src/mobile_client/mod.rs:264-305`). The public `AppClient` therefore exposes four separate lifecycle calls: set key, read key, force endpoint initialization, and shutdown (`shared/rust-bridge/codex-mobile-client/src/ffi/client.rs:266-317`).

iOS starts reachability and then loads the saved key into Rust (`apps/ios/Sources/Remora/Models/AppRuntimeController.swift:14-37`). After a pairing or reconnect operation, callers must remember to read the key back and persist it (`apps/ios/Sources/Remora/Models/AppRuntimeController.swift:39-55`). Catalyst duplicates the same load/save/shutdown/reconnect sequence in a separate implementation (`apps/ios/Sources/Remora/Models/CatalystRuntimeStubs.swift:8-68`).

Android likewise starts reachability before constructing/loading the credential store and pushing the key; the comments require the key to precede any endpoint bind, but the order remains a native convention rather than a Rust invariant (`apps/android/app/src/main/java/com/remora/android/state/AppModel.kt:108-157`). Android has a helper to persist the generated key (`apps/android/app/src/main/java/com/remora/android/state/AppModel.kt:160-170`), but the pairing sheet does not call it after connecting. Persistence occurs later during lifecycle recovery. No Android call site invokes endpoint shutdown; iOS wires the hook through its app delegate (`apps/ios/Sources/Remora/AppDelegate.swift:112-130`).

The endpoint itself uses the persisted-or-fresh key, a 15-second QUIC keepalive, iroh's default roughly 30-second connection idle timeout, and a platform-specific Android DNS/CA fallback (`shared/rust-bridge/codex-mobile-client/src/alleycat.rs:608-666`). This is coherent transport policy, but its required persistence ordering is exposed to every platform entry path.

### 3. QR or pasted payload to live session

The iOS sheet owns parsed payload, available agents, selection, loading/error phases, and the parse-only bridge (`apps/ios/Sources/Remora/Views/RemotePairingSheet.swift:5-37`). A scan follows this sequence:

1. `AlleycatBridge.parsePairPayload` validates the JSON.
2. `ServerBridge.listAlleycatAgents` probes the host.
3. The UI selects every available non-beta agent by default.
4. `ServerBridge.connectRemoteOverAlleycat` connects the selected set.
5. The UI separately saves the host token and the endpoint device key.
6. A callback later asks `DiscoveryView` to synthesize a `DiscoveredServer` and persist the non-secret profile.

Those steps are visible at `apps/ios/Sources/Remora/Views/RemotePairingSheet.swift:380-427`, `apps/ios/Sources/Remora/Views/RemotePairingSheet.swift:429-481`, and `apps/ios/Sources/Remora/Views/DiscoveryView.swift:936-977`.

Android has the same split interface and state machine (`apps/android/app/src/main/java/com/remora/android/ui/discovery/RemotePairingSheet.kt:84-172`). It connects, separately saves the token, then reports success to `DiscoveryScreen`, which separately saves the non-secret profile (`apps/android/app/src/main/java/com/remora/android/ui/discovery/RemotePairingSheet.kt:204-249`, `apps/android/app/src/main/java/com/remora/android/ui/discovery/DiscoveryScreen.kt:814-840`).

The Rust parser preserves host compatibility by:

- requiring protocol version 1, a valid iroh node ID, a nonempty token, and a valid optional relay URL;
- accepting `hostname`, `display_name`, and `name` as old aliases for `host_name`;
- normalizing known agent aliases while passing unknown advertised agents through as lowercase runtime IDs; and
- defaulting two missing permission-capability flags to `true` for old daemons.

The evidence is `shared/rust-bridge/codex-mobile-client/src/alleycat.rs:70-114`, `shared/rust-bridge/codex-mobile-client/src/alleycat.rs:291-299`, `shared/rust-bridge/codex-mobile-client/src/alleycat.rs:414-463`.

`MobileClient::connect_remote_over_alleycat` then repeats the probe, trims and deduplicates selections, maps agents to runtime kinds, and canonicalizes the UI's `alleycat:{node_id}` identity only when the caller-supplied ID starts with that form (`shared/rust-bridge/codex-mobile-client/src/mobile_client/mod.rs:622-687`). It short-circuits an existing healthy session to avoid a documented race with hot reconnect (`shared/rust-bridge/codex-mobile-client/src/mobile_client/mod.rs:689-722`), fabricates a `ws://alleycat/{node_id}` runtime URL, stores an in-memory restart target, and replaces the previous session (`shared/rust-bridge/codex-mobile-client/src/mobile_client/mod.rs:724-753`).

It opens one iroh connection and one `AlleycatReconnectTransport` per selected runtime. Individual runtime failures are skipped; pairing succeeds if at least one runtime connects (`shared/rust-bridge/codex-mobile-client/src/mobile_client/mod.rs:755-847`). The returned comma-separated agent string intentionally preserves the user's entire requested set rather than only the connected survivors (`shared/rust-bridge/codex-mobile-client/src/mobile_client/mod.rs:849-871`).

### 4. Host protocol and runtime transport

Remora's external wire constants are protocol version 1, ALPN `alleycat/1`, and a 1 MiB frame ceiling (`shared/rust-bridge/codex-mobile-client/src/alleycat.rs:22-24`). The repository explicitly permits Alleycat identity only where changing it would break host compatibility (`CONTEXT.md:32-43`). Each operation opens an iroh connection and bidirectional stream to the node ID plus optional relay using that ALPN (`shared/rust-bridge/codex-mobile-client/src/alleycat.rs:668-692`).

The first stream frame is a length-prefixed JSON `list_agents`, `restart_agent`, or `connect` request carrying `v` and the token; connect can carry `resume.last_seq` (`shared/rust-bridge/codex-mobile-client/src/alleycat.rs:301-351`, `shared/rust-bridge/codex-mobile-client/src/alleycat.rs:695-745`). After an accepted connect, the stream is adapted to upstream app-server WebSocket or the shared JSONL client according to the advertised wire (`shared/rust-bridge/codex-mobile-client/src/alleycat.rs:524-578`).

This is an important compatibility seam: the client wire structs are private handwritten copies, not types imported from the locked Alleycat crates. Moving them into `RemoteHostPairing` should localize them in an `alleycat_protocol` adapter and add fixtures captured from the host; it should not rename or reinterpret them.

### 5. Durable profile, credential, and forget behavior

iOS stores profiles as JSON under `codex_saved_servers`, retaining legacy fields so old records decode and migrate (`apps/ios/Sources/Remora/Models/SavedServerStore.swift:7-38`, `apps/ios/Sources/Remora/Models/SavedServer.swift:68-121`). A `SavedServerRecord` is built by synchronously loading the token from Keychain and embedding it in the FFI record (`apps/ios/Sources/Remora/Models/SavedServer.swift:282-305`). Android mirrors the profile in `codex_saved_servers_prefs` / `codex_saved_servers` and likewise injects the encrypted token while constructing the FFI record (`apps/android/app/src/main/java/com/remora/android/state/SavedServerStore.kt:263-319`). Thus secrets cross UniFFI in every cold-reconnect snapshot instead of being requested only by the paired-host module when needed.

Both secure stores retain old Alleycat-branded persistence identities: iOS uses `com.alleycat.token` and `com.alleycat.device_key`; Android uses `alleycat_credentials` (`apps/ios/Sources/Remora/Models/AlleycatCredentialStore.swift:21-25`, `apps/ios/Sources/Remora/Models/AlleycatCredentialStore.swift:143-160`, `apps/android/app/src/main/java/com/remora/android/state/AlleycatCredentialStore.kt:38-44`). These names predate the current interop rule that new persistence keys use Remora naming (`CONTEXT.md:32-43`). They cannot be hard-cut without orphaning existing pairs; a later naming migration needs Remora-write plus legacy-read fallback, explicit success telemetry, and a removal version.

Pairing is not durable as one operation. Both UIs establish the live session first and treat token-save failure as a log-only warning (`apps/ios/Sources/Remora/Views/RemotePairingSheet.swift:439-474`, `apps/android/app/src/main/java/com/remora/android/ui/discovery/RemotePairingSheet.kt:215-243`). The caller saves profile metadata afterward. Crash and error windows include:

- live session, no token;
- token, no profile;
- profile, no token after Android encrypted-prefs recovery;
- newly generated Android device key not yet saved; and
- a successful UI state even though the next cold reconnect cannot work.

Android deliberately wipes only the affected encrypted prefs file after a backup/Keystore mismatch, which can remove both pairing tokens and the device key while leaving ordinary profile prefs intact (`apps/android/app/src/main/java/com/remora/android/state/EncryptedPrefs.kt:8-28`). That recovery behavior is sensible, but the current reconnect result has no typed `NeedsRepair` or `RePairRequired` state.

Forget is also split. The Rust disconnect drops runtime session/store state and in-memory restart metadata (`shared/rust-bridge/codex-mobile-client/src/mobile_client/mod.rs:1261-1289`). Native profile removal deletes only the JSON entry (`apps/ios/Sources/Remora/Models/SavedServerStore.swift:110-119`, `apps/android/app/src/main/java/com/remora/android/state/SavedServerStore.kt:394-401`). Both credential stores implement token deletion, but repository search finds no call sites beyond those definitions (`apps/ios/Sources/Remora/Models/AlleycatCredentialStore.swift:79-84`, `apps/android/app/src/main/java/com/remora/android/state/AlleycatCredentialStore.kt:16-18`). Forgetting a host therefore leaves an orphaned credential.

### 6. Cold reconnect from persisted state

`SavedServerRecord` is a cross-transport compatibility bag. It contains generic network fields, SSH preferences, legacy relay pairing, current node/token/relay/agent/wire fields, and is also overloaded for SSH-bridge metadata (`shared/rust-bridge/codex-mobile-client/src/reconnect.rs:17-46`, `shared/rust-bridge/codex-mobile-client/src/reconnect.rs:786-812`). The comment explicitly says it mirrors the platform structs.

The shared plan order is:

1. skip exactly `Connected`;
2. current Alleycat if the feature flag, node, token, and agent string are present;
3. multiplexed SSH bridge;
4. Slingshot or generic WebSocket URL;
5. explicit SSH;
6. direct Codex port;
7. legacy SSH fallback;
8. local; otherwise no plan.

The decision tree and default-to-WebSocket behavior are in `shared/rust-bridge/codex-mobile-client/src/reconnect.rs:287-417`. The Alleycat execution path splits the comma-separated agent string and calls the same large `MobileClient` method used by initial pairing (`shared/rust-bridge/codex-mobile-client/src/reconnect.rs:741-781`). A missing token/profile field silently produces no paired-host plan; the result cannot distinguish a temporarily unreachable host from a pair requiring repair.

`ReconnectController` keeps another in-memory copy of saved servers plus credential providers, feature state, and a reconnect mutex (`shared/rust-bridge/codex-mobile-client/src/ffi/reconnect.rs:64-150`). Bulk reconnect snapshots current runtime state, skips only hosts whose projected health is exactly connected, computes all plans, and runs them concurrently (`shared/rust-bridge/codex-mobile-client/src/ffi/reconnect.rs:340-455`). A host already `Connecting` or `Unresponsive` can therefore be cold-reconnected while its session worker is still healing.

Single-host reconnect disconnects the current session before loading credentials and proving a viable plan exists (`shared/rust-bridge/codex-mobile-client/src/ffi/reconnect.rs:457-540`). That creates avoidable downtime and can turn a partially healthy session into a hard failure.

Both native lifecycle controllers still schedule policy around the Rust controller:

- iOS pushes records, sets the feature flag, sends a network hint, reconnects, refreshes, and persists the device key (`apps/ios/Sources/Remora/Models/AppLifecycleController.swift:20-35`). On foreground it may call `onAppBecameActive`—which already reconnects—and then run another saved-server reconnect on initial launch (`apps/ios/Sources/Remora/Models/AppLifecycleController.swift:104-143`, `shared/rust-bridge/codex-mobile-client/src/ffi/reconnect.rs:258-269`).
- Android performs two unconditional full reconnect passes, both at launch and again after `onAppBecameActive` during resume (`apps/android/app/src/main/java/com/remora/android/state/AppLifecycleController.kt:29-44`, `apps/android/app/src/main/java/com/remora/android/state/AppLifecycleController.kt:61-117`).
- Both reachability observers send `notifyNetworkChange`; after connectivity returns they call `onNetworkReachable`, which sends the same hint again before reconnecting (`apps/ios/Sources/Remora/Models/NetworkReachabilityObserver.swift:70-109`, `apps/android/app/src/main/java/com/remora/android/state/NetworkReachabilityObserver.kt:114-143`, `shared/rust-bridge/codex-mobile-client/src/ffi/reconnect.rs:334-337`).

This is shared scheduling policy expressed three times: inside Rust, in Swift, and in Kotlin.

### 7. Hot reconnect and replay

The internal `RemoteTransport` seam is appropriately deep. A session worker only knows how to ask for a fresh client, hint a network change, close a current connection, and hold transport-scoped keepalive state (`shared/rust-bridge/codex-mobile-client/src/session/remote_transport.rs:16-80`). `AlleycatReconnectTransport` holds the pair params, agent, wire, shared endpoint, current connection, and highest observed sequence; reconnect opens a new connection on the same endpoint and sends the cursor (`shared/rust-bridge/codex-mobile-client/src/alleycat.rs:122-238`).

The worker makes five fixed attempts one second apart (`shared/rust-bridge/codex-mobile-client/src/session/connection.rs:32-33`, `shared/rust-bridge/codex-mobile-client/src/session/connection.rs:1247-1311`). It reconnects and retries one failed request, and reconnects after EOF or a disconnected event; notify/resolve/reject commands are not retried (`shared/rust-bridge/codex-mobile-client/src/session/connection.rs:1598-1715`).

Replay tracking parses every JSONL line for the private `_alleycat_seq` field (`shared/rust-bridge/codex-mobile-client/src/alleycat.rs:811-858`). If the host responds `drift_reload`, the client only logs that authoritative state should be reloaded; it does not emit a typed signal or perform that reload (`shared/rust-bridge/codex-mobile-client/src/alleycat.rs:747-781`).

Hot and cold reconnect are competing authorities. `MobileClient` contains an explicit healthy-session short-circuit whose comment documents saved-server reconnect tearing down a transport that just self-healed (`shared/rust-bridge/codex-mobile-client/src/mobile_client/mod.rs:689-722`). After a new connection ID is installed, a separate health-reader path must clear old direct-resume markers and force-authoritatively refresh every loaded thread so the new connection is subscribed (`shared/rust-bridge/codex-mobile-client/src/mobile_client/mod.rs:2630-2713`).

Multi-runtime health is also lossy: every runtime worker receives the same `health_tx`, so any one worker can write `Connecting`, `Connected`, or `Disconnected` for the whole host (`shared/rust-bridge/codex-mobile-client/src/session/connection.rs:824-903`, `shared/rust-bridge/codex-mobile-client/src/session/connection.rs:1247-1311`). The paired-host module needs per-runtime health plus an explicit aggregate; it should not let the last writer win.

### 8. Paired-host terminal

The Rust terminal dispatcher accepts raw `node_id`, `token`, `relay`, and shell fields in `TerminalBackendKind::RemoteAlleycat` (`shared/rust-bridge/codex-mobile-client/src/terminal/backend.rs:26-61`). The paired backend gets the shared endpoint, reconstructs a `ParsedPairPayload`, connects the special `shell` JSONL agent with no resume cursor, initializes JSON-RPC, and spawns a shell (`shared/rust-bridge/codex-mobile-client/src/terminal/remote_alleycat.rs:20-92`, `shared/rust-bridge/codex-mobile-client/src/alleycat.rs:580-605`).

The stream reader reports exit `-1` on EOF/error and fails pending requests; it does not reconnect or reopen the shell (`shared/rust-bridge/codex-mobile-client/src/terminal/remote_alleycat.rs:247-315`). Transparent PTY resume is therefore **not** an existing host capability. The migration should centralize profile/credential lookup, endpoint reuse, and error normalization, but must not promise terminal resume until the host protocol provides a resumable shell-session contract.

Both platform terminal screens rebuild the same target independently. iOS scans runtime snapshots and saved profiles, decodes `alleycat:` IDs, loads tokens, and constructs raw terminal backends (`apps/ios/Sources/Remora/Views/TerminalScreen.swift:493-560`, `apps/ios/Sources/Remora/Views/TerminalScreen.swift:596-605`). Android loads remembered profiles, loads tokens, and constructs the same raw backend (`apps/android/app/src/main/java/com/remora/android/ui/terminal/TerminalScreen.kt:667-699`). A deep module should let callers open a terminal by typed paired-host ID without ever receiving the token.

### 9. Generic network discovery

Generic discovery is a neighboring domain, not the QR pairing mechanism. Rust scans Bonjour seeds, Tailscale, a local `/24`, ARP, and manual entries concurrently, then reconciles by normalized host and source rank (`shared/rust-bridge/codex-mobile-client/src/discovery.rs:18-28`, `shared/rust-bridge/codex-mobile-client/src/discovery.rs:232-272`, `shared/rust-bridge/codex-mobile-client/src/discovery.rs:1022-1067`). QR-paired hosts are identified by an iroh public key and enter the UI through saved profiles/runtime snapshots, not these network results.

The discovery module itself owns a second in-memory server cache and a continuous 30-second/90-second-stale mode (`shared/rust-bridge/codex-mobile-client/src/discovery.rs:174-181`, `shared/rust-bridge/codex-mobile-client/src/discovery.rs:397-430`). The public bridge exposes one-shot/progressive scans and reconcile, not the continuous interface (`shared/rust-bridge/codex-mobile-client/src/ffi/discovery.rs:89-139`).

iOS adds a seven-day native cache, saved-profile merge, last-seen map, native Bonjour browsing, native Tailscale diagnostics, Rust progressive scan, and then another Rust reconcile round trip (`apps/ios/Sources/Remora/Models/NetworkDiscovery.swift:54-117`, `apps/ios/Sources/Remora/Models/NetworkDiscovery.swift:131-219`, `apps/ios/Sources/Remora/Models/NetworkDiscovery.swift:249-313`, `apps/ios/Sources/Remora/Models/NetworkDiscovery.swift:316-390`). Android is substantially thinner: native NSD only supplies seeds and Rust owns the scan/reconcile result (`apps/android/app/src/main/java/com/remora/android/state/NetworkDiscovery.kt:27-80`, `apps/android/app/src/main/java/com/remora/android/state/NetworkDiscovery.kt:89-174`). This is current parity drift.

The internal mDNS seed supports TXT metadata, but the UniFFI record has no TXT field and always constructs an empty map (`shared/rust-bridge/codex-mobile-client/src/discovery.rs:52-60`, `shared/rust-bridge/codex-mobile-client/src/discovery_uniffi.rs:44-61`). If paired-host advertisement is ever added to network discovery, that loss must be fixed first; the new pairing module should not infer identity from incomplete mDNS data.

### 10. Older proximity pairing

There is a second pairing implementation unrelated to Alleycat QR pairing. `pair/mod.rs` describes an iPhone-to-Mac Bonjour/WebSocket/NearbyInteraction flow advertising `_remora-pair._tcp.` and returning a LAN Codex URL (`shared/rust-bridge/codex-mobile-client/src/pair/mod.rs:1-25`). It has its own wire types, host/client state machines, polling queues, and exported handles (`shared/rust-bridge/codex-mobile-client/src/pair/mod.rs:40-115`, `shared/rust-bridge/codex-mobile-client/src/pair/mod.rs:177-343`). `AppClient` still exports start-host and pair-from-iPhone calls (`shared/rust-bridge/codex-mobile-client/src/ffi/client.rs:1499-1555`).

Repository search found no non-generated Swift or Kotlin callers. Its only active consumers are Rust loopback tests (`shared/rust-bridge/codex-mobile-client/tests/pair.rs:1-206`). It should not be silently folded into `RemoteHostPairing`: quarantine or removal is an independent product decision because it represents a different trust and transport model.

## Module-depth diagnosis

### Shallow interfaces

| Existing module/interface | Depth assessment | Why |
| --- | --- | --- |
| `AlleycatBridge` | Shallow | It exports records but implements one parse call. Removing it would recreate a one-line call to `parse_pair_payload` (`shared/rust-bridge/codex-mobile-client/src/ffi/alleycat.rs:91-102`). |
| `RustAlleycatBridge` on iOS | Shallow adapter | Singleton plus direct pass-through to the generated object (`apps/ios/Sources/Remora/Bridge/RustAlleycatBridge.swift:1-17`). Android calls the generated object directly, already demonstrating that the Swift wrapper adds no policy. |
| `ServerBridge` | Broad but not deep | It groups operations by “server” rather than hiding a coherent lifecycle. Pairing callers still understand payloads, agent selection, wire types, IDs, and persistence (`shared/rust-bridge/codex-mobile-client/src/ffi/discovery.rs:141-150`, `shared/rust-bridge/codex-mobile-client/src/ffi/discovery.rs:288-340`). |
| `ReconnectController` | Leaky | Platforms must set providers, push a saved-record mirror, set a feature flag, send lifecycle hints, schedule reconnect, refresh snapshots, and persist the endpoint key in order (`shared/rust-bridge/codex-mobile-client/src/ffi/reconnect.rs:64-207`). |
| `MobileClient` pairing methods | High leverage, wrong locality | They contain real policy, but endpoint identity, agent probing, selection, race avoidance, session construction, restart targets, and general runtime concerns are intermixed (`shared/rust-bridge/codex-mobile-client/src/mobile_client/mod.rs:599-871`). |
| Paired terminal backend | Isolated implementation, leaky caller interface | It owns shell JSON-RPC details but accepts raw credentials and duplicates host connection setup (`shared/rust-bridge/codex-mobile-client/src/terminal/backend.rs:26-38`, `shared/rust-bridge/codex-mobile-client/src/terminal/remote_alleycat.rs:20-43`). |

### Duplicated policy

- Pairing phase and errors are UI-local state in both Swift and Compose rather than one shared state machine (`apps/ios/Sources/Remora/Views/RemotePairingSheet.swift:19-37`, `apps/android/app/src/main/java/com/remora/android/ui/discovery/RemotePairingSheet.kt:101-123`).
- Stable ID construction, wire-to-string mapping, selected-agent serialization, and display-name fallback occur in native UI plus Rust (`apps/ios/Sources/Remora/Views/RemotePairingSheet.swift:429-471`, `apps/android/app/src/main/java/com/remora/android/ui/discovery/RemotePairingSheet.kt:204-243`, `shared/rust-bridge/codex-mobile-client/src/mobile_client/mod.rs:680-687`).
- Profile schemas and transport preference resolution are implemented in Swift, Kotlin, and Rust; the Rust comments explicitly describe mirroring the platform rules (`shared/rust-bridge/codex-mobile-client/src/reconnect.rs:146-170`, `apps/android/app/src/main/java/com/remora/android/state/SavedServerStore.kt:76-153`, `apps/ios/Sources/Remora/Models/SavedServer.swift:123-140`).
- Endpoint key load/bind/save and lifecycle scheduling are separately implemented for iOS, Catalyst, and Android (`apps/ios/Sources/Remora/Models/AppRuntimeController.swift:14-66`, `apps/ios/Sources/Remora/Models/CatalystRuntimeStubs.swift:8-68`, `apps/android/app/src/main/java/com/remora/android/state/AppModel.kt:148-170`).
- Network-change/reconnect scheduling exists in each native reachability observer and again in Rust (`apps/ios/Sources/Remora/Models/NetworkReachabilityObserver.swift:70-109`, `apps/android/app/src/main/java/com/remora/android/state/NetworkReachabilityObserver.kt:114-143`, `shared/rust-bridge/codex-mobile-client/src/ffi/reconnect.rs:258-337`).
- Paired terminal discovery/credential lookup is duplicated in both screens (`apps/ios/Sources/Remora/Views/TerminalScreen.swift:493-560`, `apps/android/app/src/main/java/com/remora/android/ui/terminal/TerminalScreen.kt:667-699`).
- Generic discovery normalization/cache/merge has a Rust implementation plus a richer iOS implementation, while Android follows the intended thin projection (`shared/rust-bridge/codex-mobile-client/src/discovery.rs:183-204`, `apps/ios/Sources/Remora/Models/NetworkDiscovery.swift:201-285`).

### Concrete correctness risks

1. **Non-atomic pairing persistence:** connect, token, device key, and profile can partially succeed.
2. **Orphaned credentials:** forget does not delete tokens.
3. **Lost endpoint identity on Android:** a crash after first pairing but before lifecycle persistence creates a new client endpoint ID next launch.
4. **Competing reconnect authorities:** cold reconnect can tear down a hot-recovered session.
5. **Last-writer-wins multi-runtime health:** one runtime's state represents the whole host.
6. **Unacted replay drift:** the host requests authoritative reload, but Remora only logs it.
7. **Destructive single reconnect:** current session is dropped before a viable plan is known.
8. **Restart mismatch:** the in-memory target stores only pair params, and restart always requests agent `codex` even for a multi-runtime pairing (`shared/rust-bridge/codex-mobile-client/src/mobile_client/mod.rs:1292-1305`).
9. **Silent missing-credential state:** no reconnect plan and no typed repair outcome.
10. **Protocol-source split:** the pinned bridge dependencies do not type-share the manually copied remote-host wire, so an explicit host/dependency update can still drift without compatibility fixtures.

## Compatibility constraints

### Must remain stable during the migration

| Contract | Required behavior |
| --- | --- |
| Host wire identity | Keep `ALLEYCAT_PROTOCOL_VERSION = 1`, `ALLEYCAT_ALPN = alleycat/1`, length-prefixed control frames, existing operation/field names, token authentication, and 1 MiB frame limit (`shared/rust-bridge/codex-mobile-client/src/alleycat.rs:22-24`, `shared/rust-bridge/codex-mobile-client/src/alleycat.rs:301-351`, `shared/rust-bridge/codex-mobile-client/src/alleycat.rs:695-745`). |
| Pair payload | Keep `v`, `node_id`, `token`, optional `relay`, optional `host_name`, and old host-name aliases (`shared/rust-bridge/codex-mobile-client/src/alleycat.rs:291-299`, `shared/rust-bridge/codex-mobile-client/src/alleycat.rs:434-463`). |
| Agent compatibility | Keep WebSocket/JSONL values, known alias normalization, unknown-agent pass-through, optional presentation/capabilities, and legacy permission defaults (`shared/rust-bridge/codex-mobile-client/src/alleycat.rs:70-120`, `shared/rust-bridge/codex-mobile-client/src/alleycat.rs:353-430`). |
| Replay | Keep `_alleycat_seq`, optional `resume.last_seq`, and `fresh` / `resumed` / `drift_reload` response meanings (`shared/rust-bridge/codex-mobile-client/src/alleycat.rs:313-351`, `shared/rust-bridge/codex-mobile-client/src/alleycat.rs:850-858`). |
| Host bootstrap | Preserve the exact `npx kittylitter` command in README and pairing UI until the upstream distribution contract changes (`CONTEXT.md:34-40`, `README.md:64-68`). Centralize the copy without renaming it. |
| Stable paired-host identity | Preserve `alleycat:{node_id}` for existing profiles/runtime/thread keys. `ThreadKey` includes server ID, so changing it would fork runtime identity (`CONTEXT.md:49-54`, `shared/rust-bridge/codex-mobile-client/src/mobile_client/mod.rs:680-687`). |
| Existing profile records | Decode old JSON fields, `rememberedByUser` default, old relay-only records, comma-separated agent strings, wire strings, and SSH-bridge overloading until typed migration is complete (`apps/ios/Sources/Remora/Models/SavedServer.swift:68-121`, `shared/rust-bridge/codex-mobile-client/src/reconnect.rs:19-45`, `shared/rust-bridge/codex-mobile-client/src/reconnect.rs:786-812`). |
| Existing secure storage | Continue reading `com.alleycat.token`, `com.alleycat.device_key`, and `alleycat_credentials` during a time-bounded migration (`apps/ios/Sources/Remora/Models/AlleycatCredentialStore.swift:21-25`, `apps/ios/Sources/Remora/Models/AlleycatCredentialStore.swift:143-147`, `apps/android/app/src/main/java/com/remora/android/state/AlleycatCredentialStore.kt:41-44`). |
| Endpoint identity | Load the existing 32-byte device key before first bind and persist a newly generated key before reporting durable pairing success (`shared/rust-bridge/codex-mobile-client/src/mobile_client/mod.rs:264-305`). |
| Generated interface | Keep old generated methods as forwarding adapters until both native callers are cut over; regenerate Swift and Kotlin together using the repository binding lane (`CONTEXT.md:55-62`, `CONTEXT.md:64-73`). |

### May change behind the module interface

- Native pairing-state booleans and step ordering.
- The `multi_clanker_and_quic_enabled` feature flag, because both current apps set it to `true` at construction and again before reconnect (`apps/ios/Sources/Remora/Models/AppModel.swift:25-55`, `apps/android/app/src/main/java/com/remora/android/state/AppModel.kt:139-147`).
- The comma-separated internal agent representation, after a decoder preserves old profiles.
- Raw token fields in `SavedServerRecord`, once the persistence port supplies credentials directly to the module.
- Caller-supplied server IDs and wire types for initial pairing.
- Native endpoint-key hooks and native paired-terminal target construction.
- Last-writer-wins host health, once typed per-runtime aggregate health has a compatibility projection.

## Existing tests and missing verification

### Current coverage

| Layer | What is covered | Evidence |
| --- | --- | --- |
| Alleycat wire | Connect request with/without resume, highest-sequence extraction, payload success/legacy alias/bad ID, runtime aliasing and unknown pass-through, legacy/explicit capabilities, JSONL agent decoding | `shared/rust-bridge/codex-mobile-client/src/alleycat.rs:902-1071` |
| Reconnect plan | Connection-mode/port resolution, connected skip, remote URL, Alleycat enabled/default wire/disabled fallback, SSH/direct/local, old pairing requiring a scan, SSH bridge | `shared/rust-bridge/codex-mobile-client/src/reconnect.rs:906-1371` |
| Transport seam | Keepalive replacement/drop ordering and trait object safety | `shared/rust-bridge/codex-mobile-client/src/session/remote_transport.rs:83-175` |
| Session worker | Fake JSONL stream drop causes reconnect and one request retry | `shared/rust-bridge/codex-mobile-client/src/session/connection.rs:2045-2124` |
| Runtime selection | Healthy-session short-circuit detects a missing selected runtime and serializes selection | `shared/rust-bridge/codex-mobile-client/src/mobile_client/tests.rs:417-467` |
| Generic discovery | Defaults, manual entry, unreachable probe, empty scan, continuous start/stop, reconcile source/ports, mock Bonjour | `shared/rust-bridge/codex-mobile-client/src/discovery.rs:1342-1599` |
| Paired terminal | Shell-agent error string recognition; ignored live test can round-trip a shell when given a real pair payload | `shared/rust-bridge/codex-mobile-client/src/terminal/remote_alleycat.rs:413-426`, `shared/rust-bridge/codex-mobile-client/src/terminal/session.rs:392-455` |
| Legacy proximity pairing | Loopback accept, reject, and distance update | `shared/rust-bridge/codex-mobile-client/tests/pair.rs:48-206` |
| iOS projections | Old SSH/profile migration, old Alleycat placeholder name, offline selected-runtime display; Tailscale diagnostics/parser | `apps/ios/Tests/RemoraTests/HomeDashboardSupportTests.swift:73-118`, `apps/ios/Tests/RemoraTests/HomeDashboardSupportTests.swift:260-290`, `apps/ios/Tests/RemoraTests/NetworkDiscoveryTests.swift:4-133` |
| Android projections/native init | Old profile name and local transport-choice helpers; iroh Android context reaches `listAlleycatAgents` without the missing-context panic | `apps/android/app/src/test/java/com/remora/android/SavedServerTransportTest.kt:11-97`, `apps/android/app/src/androidTest/java/com/remora/android/NativeContextInitTest.kt:14-48` |

The `AlleycatReconnectTransport` test at the end of `alleycat.rs` is only a compile-time coercion helper; it does not instantiate an endpoint or reconnect against a host (`shared/rust-bridge/codex-mobile-client/src/alleycat.rs:1061-1071`).

### High-priority gaps

1. No in-process or live host compatibility suite for list/probe/connect/restart, wire framing, authentication rejection, agent metadata, or replay attach kinds.
2. No profile-adapter → credential-adapter → cold-reconnect round trip.
3. No pairing transaction test for token/profile/device-key partial failures and compensation.
4. No forget test proving runtime, profile, and credential cleanup are idempotent.
5. No typed test for missing credential, invalid old profile, token rotation, or `RePairRequired`.
6. No replay-drift test proving an authoritative thread reload occurs.
7. No multi-runtime partial-connect/reconnect test or aggregate-health test.
8. No hot-worker versus cold-controller race test.
9. No network-change/long-resume integration test across the native adapter and Rust state machine.
10. No paired-terminal credential encapsulation or disconnect/reopen behavior test.
11. No parity test that iOS and Android decode/encode the same profile fixture.
12. No test protecting the exact host-bootstrap copy from drifting across five call sites.

## Target deep module

### Purpose and limits

`RemoteHostPairing` should own one cohesive decision: “Given a host offer and selected runtimes, establish and durably maintain this paired remote host.” It should hide:

- payload parsing and compatibility;
- host probing and agent normalization;
- selected-runtime validation and intent;
- stable paired-host identity;
- shared endpoint/device-identity lifecycle;
- live connection transaction and rollback;
- paired profile/credential transaction orchestration;
- per-runtime hot reconnect and aggregate host health;
- cold restore and repair outcomes;
- replay-drift reconciliation signal;
- restart target semantics;
- forget cleanup; and
- opening a paired terminal by host ID.

It should **not** own generic LAN/Tailscale/Bonjour discovery, native QR/camera UI, Keychain/EncryptedSharedPreferences mechanics, native app lifecycle detection, generic SSH/direct/Slingshot planning, or transparent shell resume not supported by the host.

### Proposed external seam

The public UniFFI interface should be small and domain-shaped. One workable shape is:

```rust
#[derive(uniffi::Object)]
pub struct RemoteHostPairing { /* shared facade */ }

#[uniffi::export(callback_interface)]
pub trait RemoteHostPairingPersistence: Send + Sync {
    fn load_profiles(&self) -> Vec<StoredRemoteHostProfile>;
    fn load_credential(&self, host_id: PairedHostId) -> Option<RemoteHostCredential>;
    fn load_device_identity(&self) -> Option<Vec<u8>>;
    fn commit_pairing(&self, commit: RemoteHostPairingCommit) -> Result<(), PersistenceError>;
    fn delete_pairing(&self, host_id: PairedHostId) -> Result<(), PersistenceError>;
}

impl RemoteHostPairing {
    async fn inspect_offer(&self, raw: String) -> Result<PairingOfferHandle, PairingError>;
    async fn probe(&self, offer: Arc<PairingOfferHandle>) -> Result<Vec<RemoteAgent>, PairingError>;
    async fn pair(&self, request: PairRemoteHostRequest) -> Result<PairedHost, PairingError>;
    async fn reconnect(&self, host_id: PairedHostId) -> PairingReconnectResult;
    async fn forget(&self, host_id: PairedHostId) -> Result<(), PairingError>;
    async fn open_terminal(&self, host_id: PairedHostId, request: TerminalOpenRequest)
        -> Result<Arc<TerminalSession>, PairingError>;
}
```

The precise generated names can change, but the semantics should not:

- `PairingOfferHandle` keeps the token in Rust rather than returning it to Swift/Kotlin after scanning.
- `PairedHostId` canonicalizes `alleycat:{node_id}` once.
- `AgentSelection` is a typed ordered set, not a comma-separated string.
- `PairedHostProfile` contains non-secret durable intent; the token is a distinct credential.
- `PairingReconnectResult` has explicit `Connected`, `Degraded`, `NeedsRepair`, `RePairRequired`, and transport-failure outcomes.
- `RemoteHostPairingPersistence` is justified variation: Keychain/UserDefaults, encrypted/ordinary Android prefs, and an in-memory test adapter really differ. It is the one external seam; storage policy stays native, lifecycle policy stays Rust.
- `commit_pairing` is one adapter operation. The native adapter should write the credential first, then the profile/device identity, compensate on failure, and return an explicit partial-cleanup error if compensation fails. Rust disconnects the newly opened runtime session before returning a failed pairing.
- `delete_pairing` is idempotent and removes profile plus credential; the Rust module disconnects runtime state and records cleanup failure rather than silently orphaning secrets.

### Internal invariants

1. The endpoint loads or creates its identity exactly once inside the module. A fresh identity is persisted before a pairing can become `Durable`.
2. Only the module constructs paired-host IDs, pair params, restart targets, and persisted agent intent.
3. At most one lifecycle operation per paired host mutates its session generation.
4. A live hot-reconnect worker owns recovery while present. Cold restore only creates an absent/exhausted session; it never tears down a connecting or just-healed generation.
5. Health is stored per selected runtime. Host health is a deterministic aggregate with a typed degraded state.
6. `drift_reload` schedules an authoritative reload before the reconnected generation is declared fully synchronized.
7. Secrets never appear in `SavedServerRecord`, terminal options, or UI callbacks.
8. Pair success means both usable live session and durable profile/credential/device identity. If durability fails, the live session is rolled back.
9. Forget is complete only when runtime and persistence cleanup finish or an explicit retryable cleanup state is recorded.
10. Generic discovery may project paired profiles beside network results, but it does not reinterpret iroh identity as a LAN host.

### Internal adapters to preserve

- Keep `RemoteTransport` as the transport seam. Move `AlleycatReconnectTransport` under the new module and continue passing it to `ServerSession`.
- Keep `AppStore` as runtime state. Project paired-host runtime/aggregate health into it; do not turn it into a credential store.
- Keep the host wire in an `alleycat_protocol` adapter with compatibility fixtures. Upstream naming is valid there because it is an external protocol identity.
- Let generic `ReconnectController` delegate a typed paired-host record to `RemoteHostPairing`; do not duplicate the paired plan.
- Let terminal internals keep their JSON-RPC backend, but obtain connection/profile state from `RemoteHostPairing` by ID.

## Safest independently revertible migration sequence

Each phase below should be one reviewable change with its own green gate. “Forwarding adapter” means old methods call the new implementation; it does not mean maintaining two policies.

### Phase 0 — Characterize the external and persisted contracts

Add tests only:

- golden pair payloads including every host-name alias;
- golden list/connect/restart responses for old and current capabilities;
- stable `alleycat:{node_id}` identity;
- comma-agent and wire-string profile fixtures from both platforms;
- old relay-only and SSH-bridge-overloaded records;
- token missing, token-store failure, profile-store failure, and device-key failure;
- multi-runtime partial attach and health;
- hot/cold reconnect race and `drift_reload`;
- exact bootstrap-command copy.

Validation: `make rust-test`, iOS unit tests, Android unit tests.
Rollback: delete tests; no production behavior changed.

### Phase 1 — Introduce internal domain types behind current calls

Create `remote_host_pairing/` with `PairedHostId`, `PairingOffer`, `AgentSelection`, `PairedHostProfile`, pairing phases, typed health, and repair outcomes. Initially delegate protocol operations to existing `alleycat.rs` functions and session creation to `MobileClient`; keep every generated interface unchanged.

Move canonical ID and agent-selection normalization into these types, then make `MobileClient` call them. Preserve old comma-string encoding only at the profile compatibility adapter.

Validation: Phase 0 fixtures plus existing Alleycat/reconnect/mobile-client unit tests.
Rollback: remove the internal module and restore the small call sites; persisted state and UniFFI are unchanged.

### Phase 2 — Localize host protocol and endpoint lifecycle

Move the handwritten request/response structs, framing, payload compatibility, endpoint, and `AlleycatReconnectTransport` under the module. Keep `alleycat.rs` as a temporary internal forwarding adapter if necessary. Add an in-process fake-host adapter that exercises framed list/connect/restart/replay without public-network dependencies.

The module loads the device identity from an injected test/production persistence port at first use and saves a fresh key immediately after bind. Remove native timing as an invariant, but leave native methods forwarding during cutover.

Validation: fake-host compatibility suite, endpoint identity survives reconstructing the module, existing worker reconnect test.
Rollback: point forwarding calls back to the old files; storage format remains unchanged.

### Phase 3 — Add paired-host persistence adapters without renaming keys

Implement `RemoteHostPairingPersistence` in Swift and Kotlin using the **existing** profile and secure-storage keys. Add in-memory and failure-injecting Rust test adapters. Move token lookup out of `SavedServer.toRecord` / `SavedServer.toRecord(context)` and into the module.

Implement commit compensation and idempotent delete. Do not combine this phase with a storage-key rename or schema cleanup.

Validation: success and every partial-write/delete failure; existing profiles cold-reconnect on both platforms; forgetting removes token and profile.
Rollback: restore token injection into `SavedServerRecord`; existing keys were never changed.

### Phase 4 — Add the deep UniFFI object and cut over both pairing UIs

Expose offer inspection/probe/pair/forget through `RemoteHostPairing`. Switch iOS and Android in the same change. UI owns only camera/paste, display name, selection presentation, and rendering of typed phase/error output. Pair success comes only after Rust connection plus persistence commit.

Keep `AlleycatBridge.parsePairPayload` and `ServerBridge` Alleycat methods as forwarding adapters for one release or until all generated/native references are absent and the Phase 0 compatibility suite is green. State the removal criterion in code comments.

Validation: regenerate both bindings; build iOS/Android; UI test pair success, invalid QR, unavailable agent, persistence failure, and retry.
Rollback: restore UI calls to the forwarding methods; both paths still execute the same new policy.

### Phase 5 — Make the module the sole paired-host reconnect authority

Replace the Alleycat branch in the generic reconnect planner with a typed delegation. Stop embedding tokens in `SavedServerRecord`. Do not disconnect a current session before a viable plan exists. Gate cold restore on session generation/state so a live hot worker and saved-host reconnect cannot compete.

Move endpoint network-change/long-resume handling into the paired-host state machine. Native reachability should report only a typed network/lifecycle event once; Rust chooses hint, close, reconnect, probe, and retry. Remove duplicate second reconnect passes. Aggregate per-runtime health and turn `drift_reload` into an authoritative refresh before synchronization completes.

Validation: race test, long-resume test, network-regain test, multi-runtime degraded/recovery test, authoritative drift reload, request resubscription.
Rollback: restore the generic planner branch; profile and protocol compatibility are unchanged.

### Phase 6 — Route paired terminals by host ID

Replace raw node/token/relay terminal options with `open_terminal(PairedHostId, ...)`. Reuse the paired profile, secure credential, endpoint, and normalized host errors inside the module. Keep the current behavior that a broken shell exits unless/until the host adds resumable shell sessions.

Validation: token is absent from generated terminal-facing records; fake host can open/input/resize/close; disconnect yields the typed reopen-needed result. Retain the ignored live smoke test.
Rollback: restore the raw terminal constructor while leaving core pairing intact.

### Phase 7 — Clean the adjacent discovery presentation

Keep `DiscoveryService` independent. Add a Rust-owned projection that composes generic discovered hosts with paired profiles for UI, rather than making each native client merge them. Decide one cache/staleness policy; remove iOS's second reconcile/cache only after equivalent UX is implemented in Rust. Preserve native NSD/NWBrowser as seed adapters. Add TXT to `AppMdnsSeed` if product behavior needs it.

Validation: identical fixture ordering/dedupe on iOS and Android; saved paired hosts appear offline without entering LAN discovery; generic host choice behavior remains unchanged.
Rollback: restore native composition; pairing transport is unaffected.

### Phase 8 — Migrate names/schema, then remove forwarding surfaces

Only after the new module has shipped stably:

1. introduce a versioned typed paired-profile schema;
2. write Remora-named secure/profile keys while reading both new and legacy keys;
3. migrate on successful read and verify the new copy before deleting the old one;
4. retain a documented fallback/removal version;
5. remove `multi_clanker_and_quic_enabled`, comma-agent policy, raw token fields, native endpoint hooks, Alleycat operations on `ServerBridge`, and the parse-only bridge;
6. regenerate both platform bindings; and
7. independently decide whether to delete the unused proximity-pairing exports and tests.

Validation: upgrade fixtures from every old schema/key; downgrade/rollback behavior during the support window; no native references to forwarding methods; full repository gate from `CONTEXT.md:64-73`.
Rollback: continue dual-read and restore old forwarding methods until the removal version; never delete legacy storage before the migration success check.

## Completion criteria for the migration

The deepening is complete when all of the following are true:

- The platform pairing screens call one pairing object and never construct pair payload records, server IDs, wire strings, or tokens.
- A pair cannot report success unless session, token, profile, and device identity are durable; failed durability rolls the session back.
- Forget removes runtime state, profile, and token idempotently.
- A missing/invalid credential yields a typed repair outcome visible in both UIs.
- Hot and cold reconnect cannot operate on the same session generation concurrently.
- Multi-runtime host health is an aggregate, and replay drift performs authoritative reconciliation.
- Paired terminals open by host ID; no platform terminal code loads pairing credentials.
- Generic discovery has one shared reconcile/cache policy and remains distinct from iroh pairing.
- Old payloads, hosts, profiles, storage keys, stable IDs, and bootstrap command pass compatibility fixtures.
- Old UniFFI forwarding methods and the feature flag have explicit removal criteria and are actually removed after both platforms cut over.
- Cross-platform verification is green against the pinned Remora Alleycat fork revision, and any `make update-remora-link REV=<sha>` change is reviewed separately.

## Recommended first implementation slice

Start with Phases 0 and 1 only: characterization tests plus internal typed domain state behind existing interfaces. That slice has the highest architectural leverage with no persistence, generated-binding, UI, or host-protocol cutover. It establishes the new module's invariants and gives every later phase a stable interface-level test target.
