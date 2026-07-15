# RemoteHostPairing: a minimal deep module

## Recommendation

Put one Rust-owned `remote_host_pairing` module behind the existing handwritten
UniFFI seam. Present exactly three entry points on `AppClient`:

```rust
async fn inspect_remote_host_pairing(
    code: RemotePairingCode,
) -> Result<RemotePairingOffer, RemoteHostPairingError>;

async fn connect_remote_host(
    intent: RemoteHostConnectIntent,
) -> Result<RemoteHostConnection, RemoteHostPairingError>;

async fn revoke_remote_host(
    host_id: RemoteHostId,
) -> Result<RemoteHostRevocation, RemoteHostPairingError>;
```

This is the whole external interface. QR scanning, clipboard access, camera
permission, and screen state remain native. A scanner and a pasted code both
produce the same opaque `RemotePairingCode`; Swift and Kotlin do not parse or
interpret it.

`RemoteHostPairing` is a module, not another public UniFFI object. Direct server
operations continue to live on `AppClient`, while the implementation is owned by
`MobileClient` and updates the canonical `AppStore`.

Place the external seam and UniFFI-safe types in
`src/ffi/remote_host_pairing.rs`, re-export the three methods through
`src/ffi/client.rs`, and keep orchestration in `src/remote_host_pairing/`. Treat
the existing `alleycat.rs` mechanics as the first transport adapter rather than
as a second public surface. This preserves the repository's single handwritten
mobile interface and keeps `AppStore` small.

The three methods correspond to three irreducible user intents:

1. Inspect an untrusted code and show what host/runtimes it represents.
2. Connect, either by accepting that inspected offer or resuming a saved host.
3. Revoke this device's saved relationship with a host.

A single command method would reduce the method count but enlarge the interface
with an unrelated mega-enum. More methods would expose implementation phases
such as parse, list agents, save token, bind endpoint, and reconnect.

## Why the current cluster should become one module

The current behavior is distributed across both platform UIs and several Rust
surfaces:

- Payload parsing is a standalone `AlleycatBridge` operation
  (`src/ffi/alleycat.rs:98`).
- Agent probing and connection are separate `ServerBridge` operations
  (`src/ffi/discovery.rs:288-329`).
- Both pairing sheets orchestrate parse, probe, selection, connection, token
  persistence, and endpoint-key persistence
  (`RemotePairingSheet.swift:380-480`,
  `RemotePairingSheet.kt:124-249`).
- Successful connection metadata is persisted later, in another caller
  (`DiscoveryView.swift:936-963`, `DiscoveryScreen.kt:824-835`).
- Resume reconstructs a transport credential by injecting a token into a broad
  `SavedServerRecord` (`reconnect.rs:299-322`, `SavedServer.swift:282-305`,
  `SavedServerStore.kt:263-285`).
- Removing a server only removes metadata and disconnects the session
  (`SettingsView.swift:388-392`, `SettingsSheet.kt:292-298`). Both credential
  stores define `deleteToken`, but neither method has a caller.
- Platforms must load the device secret before the first endpoint bind and save
  a newly generated one afterwards (`AppRuntimeController.swift:19-54`,
  `AppModel.kt:151-170`).

This cluster is shallow: callers must learn ordering and failure policy that
belong to pairing. The deletion test is decisive. If the proposed module were
deleted, parsing, credential handling, endpoint identity, probing, multiplexed
connection, resume, rollback, and revocation would reappear across Swift,
Kotlin, reconnect code, and settings code.

## Interface types

The following is illustrative UniFFI-safe Rust. Identifiers are records rather
than naked strings so node IDs, offer IDs, and server IDs cannot be mixed at the
seam.

```rust
#[derive(Clone, uniffi::Record)]
pub struct RemotePairingCode {
    /// Exact text emitted by the host, whether scanned or pasted.
    pub encoded: String,
}

#[derive(Clone, Eq, PartialEq, Hash, uniffi::Record)]
pub struct RemoteHostId {
    /// Opaque to platform callers. Stable across launches.
    pub value: String,
}

#[derive(Clone, Eq, PartialEq, Hash, uniffi::Record)]
pub struct RemotePairingOfferId {
    /// Opaque, process-local, short-lived, and not a credential.
    pub value: String,
}

#[derive(Clone, uniffi::Record)]
pub struct RemotePairingOffer {
    pub offer_id: RemotePairingOfferId,
    pub host_id: RemoteHostId,
    pub suggested_display_name: String,
    pub runtimes: Vec<RemoteRuntimeOffer>,
    pub recommended_runtime_ids: Vec<String>,
    /// Informational for UI countdowns. Expiry is enforced with a monotonic
    /// clock inside the implementation.
    pub expires_at_unix_ms: u64,
}

#[derive(Clone, uniffi::Record)]
pub struct RemoteRuntimeOffer {
    /// Host-advertised stable identifier, normalized in Rust.
    pub runtime_id: String,
    pub display_name: String,
    pub available: bool,
    pub recommended: bool,
    pub presentation: Option<AppAgentPresentation>,
    pub capabilities: Option<AppAgentCapabilities>,
}

#[derive(Clone, uniffi::Enum)]
pub enum RemoteHostConnectIntent {
    Pair {
        offer_id: RemotePairingOfferId,
        display_name: Option<String>,
        selected_runtime_ids: Vec<String>,
    },
    Resume {
        host_id: RemoteHostId,
    },
}

#[derive(Clone, uniffi::Record)]
pub struct RemoteHostConnection {
    pub host_id: RemoteHostId,
    pub disposition: RemoteHostConnectionDisposition,
}

#[derive(Clone, uniffi::Enum)]
pub enum RemoteHostConnectionDisposition {
    Connected,
    AlreadyConnected,
}

#[derive(Clone, uniffi::Record)]
pub struct RemoteHostRevocation {
    pub host_id: RemoteHostId,
    pub disposition: RemoteHostRevocationDisposition,
    pub host_credential_status: HostCredentialRevocationStatus,
}

#[derive(Clone, uniffi::Enum)]
pub enum RemoteHostRevocationDisposition {
    Revoked,
    AlreadyRevoked,
}

#[derive(Clone, uniffi::Enum)]
pub enum HostCredentialRevocationStatus {
    Confirmed,
    UnsupportedByHostProtocol,
}
```

`RemotePairingOffer` intentionally omits the token, relay wire details, transport
wire kind, raw node ID, and endpoint key. The platform receives only values it
must render or return as user selections.

The connection result is also intentionally small. Runtime connection health,
partial availability, and reconnect progress are canonical `AppStore` state,
not a second state model in the pairing result. A successful connection means
at least one selected runtime is attached and the complete desired selection is
durably remembered; it does not promise every selected runtime is currently
reachable.

## Error interface

Errors are typed. No caller parses upstream or transport strings.

```rust
#[derive(Debug, uniffi::Error)]
pub enum RemoteHostPairingError {
    InvalidCode { problem: PairingCodeProblem },
    IncompatibleProtocol { host_version: u32, client_version: u32 },
    OfferExpired,
    InvalidRuntimeSelection,
    NotPaired,
    Revoked,
    AuthenticationRejected,
    HostUnavailable,
    ProtocolViolation,
    NoRuntimeConnected,
    PersistenceUnavailable { phase: PairingPersistencePhase },
    Cancelled,
}

#[derive(Debug, uniffi::Enum)]
pub enum PairingCodeProblem {
    Malformed,
    MissingNode,
    InvalidNode,
    MissingCredential,
    InvalidRelay,
}

#[derive(Debug, uniffi::Enum)]
pub enum PairingPersistencePhase {
    DeviceIdentity,
    CommitPairing,
    BeginRevocation,
    EraseCredential,
}
```

Retry policy is part of the interface:

- `HostUnavailable`, `NoRuntimeConnected`, and `PersistenceUnavailable` are
  retryable without rescanning while the offer has not expired.
- `OfferExpired` requires inspecting the code again.
- `AuthenticationRejected`, `InvalidCode`, and `IncompatibleProtocol` require a
  new code or a host/client update.
- `Revoked` and `NotPaired` require a new pairing.
- `InvalidRuntimeSelection` requires a selection from the returned offer.
- `ProtocolViolation` is non-retryable for that host response but must retain a
  redacted diagnostic internally.

## Invariants

1. **Secrets stay behind the seam.** Pairing tokens, relay details, endpoint
   secret keys, and saved envelopes are never returned to general Swift/Kotlin
   code, copied into `SavedServerRecord`, or logged.
2. **One identity.** `RemoteHostId` is derived in Rust from the validated,
   normalized host node identity. Platform code never constructs it from a raw
   node ID.
3. **Inspect does not pair.** It may bind the local endpoint and authenticate a
   one-shot probe, but it does not create a durable host record or live app
   session.
4. **Offers are capabilities, not credentials.** An offer ID references a
   process-local secret cache, expires after a short TTL, is single-host, and is
   invalidated by revocation. Restarting the app requires rescanning an
   unaccepted offer.
5. **Success is durable.** `connect_remote_host(Pair)` returns success only
   after at least one runtime session is attached and the pairing envelope is
   committed. A persistence failure rolls back the new session.
6. **Selection is authoritative intent.** Every selected runtime ID must belong
   to the current offer and be advertised as available. The full desired set is
   persisted even if a transient failure leaves some runtimes pending.
7. **Resume has no secret parameters.** A caller resumes by `RemoteHostId` only.
   The implementation loads the credential, relay hint, display name, desired
   runtimes, and wire choices from its own persisted envelope.
8. **Connect is idempotent.** A healthy session satisfying the desired runtime
   set returns `AlreadyConnected`. Concurrent connects for one host coalesce
   behind one per-host operation lock.
9. **Reconnect is internal.** Sequence cursors, connection replacement, network
   change handling, retry/backoff, and post-reconnect resubscription never cross
   the external seam.
10. **Revocation wins races.** Once a durable revocation tombstone exists, new
    connects and reconnect workers for that host fail with `Revoked` and cannot
    recreate the pairing.
11. **Revocation is idempotent.** Repeating it after complete cleanup returns
    `AlreadyRevoked`.
12. **The device endpoint key is stable.** The implementation loads or creates
    and durably stores the device key before first endpoint bind. Revoking one
    host does not rotate the app-wide key used by other hosts.
13. **Store updates are authoritative.** The implementation updates `AppStore`;
    platform callers do not synthesize or hand-patch server state after a
    successful call.

## Ordering

### Inspect a code

1. Trim and decode the raw string in Rust; detect supported representations
   internally.
2. Validate protocol version, node identity, non-empty credential, and relay
   syntax.
3. Derive `RemoteHostId`. If a prior revocation tombstone exists, finish its
   secure cleanup before accepting a new offer; return a typed persistence error
   if cleanup cannot complete.
4. Load-or-create the device endpoint key and persist it **before** binding the
   endpoint.
5. Open a one-shot authenticated host probe and fetch typed runtime metadata.
6. Normalize/deduplicate runtime IDs and calculate recommended defaults in Rust.
7. Cache the parsed secret material under a random, short-lived offer ID.
8. Return the sanitized offer.

An older inspect response cannot overwrite a newer scan in UI because offer IDs
are unique. The platform may still discard a response whose local task was
cancelled.

### Pair and connect

1. Resolve and reserve the offer under its host operation lock. Invalidate it on
   success; retain it for a retryable failure until its TTL expires.
2. Recheck expiry, revocation state, display-name normalization, and selection.
3. Mark `AppStore` health as connecting.
4. Open selected runtime streams on the shared endpoint. Preserve the desired
   set while allowing unavailable members to remain pending; fail if none
   attach.
5. Build and attach the multiplexed `ServerSession`, including reconnect
   transports and sequence tracking.
6. Atomically write one opaque active pairing envelope containing host identity,
   credential, relay hint, display name, desired runtimes, and schema version.
7. If step 6 fails, disconnect the newly attached session, clear reconnect
   targets/store state, retain the unexpired offer for retry, and return
   `PersistenceUnavailable(CommitPairing)`.
8. Publish connected state through `AppStore` and return the host ID.

The live session precedes the durable commit so a token is not retained for a
host that has never successfully connected. Rollback makes the externally
observable result all-or-nothing.

### Resume

1. Serialize with pair/revoke for the host.
2. Load the active envelope. Missing is `NotPaired`; a tombstone is `Revoked`.
3. Return `AlreadyConnected` if the current healthy session satisfies the
   desired runtime set.
4. Probe current runtime availability and open the desired streams using the
   same connection implementation as fresh pairing.
5. Resume stream sequence from the last observed cursor when possible; let the
   transport choose fresh/resumed/drift-reload internally.
6. Replace stale session resources only after replacement resources are ready,
   then publish canonical health and resubscribe.

Cold-launch restoration and network recovery call this same implementation from
inside Rust. They do not reconstruct `AppAlleycatPairPayload` in platform code.

### Revoke

1. Acquire the host operation lock.
2. Atomically replace the active envelope with a `revoking` tombstone. If this
   fails, do not claim or begin revocation.
3. Cancel reconnect work, invalidate offers, clear restart targets, and close
   active runtime/terminal resources for the host.
4. If the host protocol supports device/token revocation, request it and record
   `Confirmed`. With today's Alleycat protocol, record
   `UnsupportedByHostProtocol`.
5. Delete the credential envelope and remove the host from canonical store
   state. If secure deletion fails, retain the tombstone and return
   `PersistenceUnavailable(EraseCredential)`; retry continues cleanup but no
   connection can resume.
6. Return `Revoked`. On startup, tombstones are finalized before any reconnect
   scheduling.

The present host protocol authenticates with a bearer token and exposes no
revoke operation. Therefore this interface can guarantee local revocation—no
future use by this installation—but cannot claim that a copied token was
invalidated at the host. Adding a host-side revoke command later changes only
the internal transport adapter and the returned status, not the three methods.

## Hidden implementation

The module absorbs all of the following:

- JSON/URL/code representation detection, protocol validation, compatibility
  aliases, and normalization;
- secret redaction and offer TTL/cache management;
- stable endpoint identity load/create/persist/bind ordering;
- Alleycat `alleycat/1`, `ALLEYCAT_*`, iroh relay/path handling, list-agents and
  connect frames, wire selection, response validation, and graceful probe close;
- runtime name normalization, capability shaping, recommendation policy,
  deduplication, and desired-vs-attached reconciliation;
- stable host/server ID construction;
- shared endpoint reuse, per-runtime stream creation, multiplexed session
  assembly, partial-runtime recovery, health/event readers, and warmup;
- sequence cursor tracking, auto-resume, reconnect single-flight, network-change
  hints, replacement ordering, and resubscription;
- opaque persistence schema/version migration and legacy-record import;
- transactional commit/rollback, revocation tombstones, cleanup recovery, and
  per-host concurrency control;
- `AppStore` projection and typed error mapping.

Swift and Kotlin retain only camera/clipboard UI, display-name editing, runtime
selection controls, secure-storage adapters, and rendering from `AppStore`.

## Dependency categories and adapters

### In-process

Parsing, validation, normalization, recommendation policy, offer caching,
operation locks, reconciliation, error mapping, and `AppStore` projection are
in-process dependencies. Merge them into the module and test them through the
external interface. Do not add ports for pure helpers or the canonical store.

### Local-substitutable

Secure persistence is local-substitutable but platform-specific. Define one
**internal port**, wired once at app bootstrap, with operations equivalent to
`read(slot)`, `write(slot, opaque_bytes)`, `delete(slot)`, and `list(prefix)`.
The implementation owns envelope serialization; adapters treat values as opaque.

- Production adapters: iOS Keychain and Android encrypted preferences/keystore.
- Test adapter: an in-memory crash-injectable store with atomic single-slot
  writes.

This seam is real: it has two production adapters and a test adapter. It is not
an argument on the three methods and does not expose the persistence schema to
callers. Store the whole per-host envelope in one secure slot so metadata and
credential cannot diverge as they do today.

Clock and random-ID generation are also local-substitutable internal adapters:
system monotonic clock/CSPRNG in production, deterministic clock/ID source in
tests.

### Remote but owned

The Alleycat host is remote but owned/controlled. Define an internal
`PairingHostPort` at the network seam. It deals in typed inspect/connect/resume/
revoke outcomes, not JSON frames.

- Production adapter: iroh + Alleycat protocol.
- Test adapter: an in-memory scripted host that can expire credentials, vary
  runtime availability, drop streams, advance sequence floors, and support or
  reject remote revocation.

The deep module owns ordering, persistence, session policy, and reconciliation;
the adapter owns transport mechanics. A future Alleycat protocol version or a
different owned host transport replaces this adapter without widening the
external interface.

### True external

There is no true-external network dependency in the pairing path. Camera APIs
and OS secure stores remain platform concerns, but camera output is merely an
input string and secure storage is covered by the local-substitutable port.

## Usage

Thin Swift:

```swift
let offer = try await appModel.client.inspectRemoteHostPairing(
    code: RemotePairingCode(encoded: scannedOrPastedText)
)

let connection = try await appModel.client.connectRemoteHost(
    intent: .pair(
        offerId: offer.offerId,
        displayName: editedName,
        selectedRuntimeIds: selectedRuntimeIds
    )
)
// Navigate using connection.hostId; render health from AppStore.

_ = try await appModel.client.connectRemoteHost(
    intent: .resume(hostId: savedHostId)
)

let revoked = try await appModel.client.revokeRemoteHost(hostId: savedHostId)
```

Thin Kotlin:

```kotlin
val offer = appModel.client.inspectRemoteHostPairing(
    RemotePairingCode(encoded = scannedOrPastedText),
)

val connection = appModel.client.connectRemoteHost(
    RemoteHostConnectIntent.Pair(
        offerId = offer.offerId,
        displayName = editedName,
        selectedRuntimeIds = selectedRuntimeIds,
    ),
)

appModel.client.connectRemoteHost(
    RemoteHostConnectIntent.Resume(hostId = savedHostId),
)

val revoked = appModel.client.revokeRemoteHost(savedHostId)
```

Neither platform calls parse, list-agents, save-token, remember-server,
set/read-device-key, or disconnect as part of these flows.

## Testing seam

The interface is the test surface. Construct the real module with the in-memory
persistence adapter, scripted host adapter, deterministic clock, and deterministic
ID source; call only the same three entry points used by Swift and Kotlin.

High-value contract tests:

- malformed, incompatible, missing-token, invalid-node, and invalid-relay codes;
- inspect returns typed runtimes but never the token or wire details;
- offer expiry and offer invalidation after revoke;
- selection must be a non-empty subset of currently offered runtimes;
- successful pair commits one envelope and updates `AppStore`;
- zero attached runtimes leaves no durable pairing;
- partial attachment succeeds, persists the full desired set, and later resume
  retries missing runtimes;
- persistence failure after session attach rolls back session/store/reconnect
  state and allows retry with the still-valid offer;
- duplicate concurrent connect coalesces and creates one session;
- resume uses only host ID, sends the correct sequence cursor, and handles
  fresh/resumed/drift-reload without caller branching;
- revoke racing connect/reconnect wins after its tombstone commit;
- a simulated crash after tombstone write finalizes revocation before startup
  reconnect;
- secure-delete failure leaves an effective tombstone and a retryable typed
  error;
- repeated revoke returns `AlreadyRevoked`;
- unsupported remote credential revocation is reported honestly;
- logs and error descriptions contain neither token nor endpoint secret.

Run adapter contract tests against both platform persistence adapters to prove
atomic replace/delete behavior and device-only accessibility. Keep protocol
frame tests inside the Alleycat transport adapter. Remove overlapping tests of
the old shallow parse/list/connect/persistence orchestration after the new
interface tests exist; replace, do not layer.

## Tradeoffs and deliberate constraints

- **Inspect costs a network round trip.** This yields an authenticated,
  up-to-date runtime offer and keeps protocol/fallback logic out of the UI.
- **Unaccepted offers do not survive process death.** Users must rescan after a
  restart. This avoids persisting bearer tokens before pairing succeeds.
- **Pairing success requires secure persistence.** A host may be reachable while
  the keychain/keystore is unavailable; the module fails and rolls back rather
  than creating a connection that cannot be resumed or revoked coherently.
- **Partial runtime attachment is intentionally hidden in `AppStore`.** The
  connection result stays small; detailed health remains observable in the one
  canonical state model.
- **Revocation is only locally strong today.** The host protocol needs a
  device-scoped credential and revoke operation for true remote invalidation.
  The status type prevents UI copy from overstating security.
- **Opaque persistence requires a one-time hard cutover.** Import legacy pairing
  metadata plus keychain/prefs tokens into one versioned envelope, verify it,
  then delete the legacy records. Do not retain permanent dual read/write paths.
- **Three methods are the minimum honest interface.** Combining inspect with
  connect would prevent preview/runtime selection; combining revoke with
  connect would create a command bag; splitting resume from connect would expose
  duplicate lifecycle semantics.

This shape earns depth by making the easiest platform usage also the only
correct ordering. Changes to the Alleycat wire protocol, persistence schema,
reconnect policy, or revocation support remain local to one Rust module and its
internal adapters.
