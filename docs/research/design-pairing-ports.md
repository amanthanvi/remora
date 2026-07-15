# RemoteHostPairing: desired-state hexagon over explicit ports

Status: design only. This document proposes an alternative interface and
migration shape. It does not authorize production changes.

## Conclusion

Build `RemoteHostPairing` as a desired-state hexagon in Rust. Mobile callers do
two things only:

1. propose a host from an opaque code;
2. declare the desired durable state of that host as paired or absent.

```rust
impl AppClient {
    async fn propose_remote_host(
        &self,
        code: RemotePairingCode,
    ) -> Result<RemoteHostProposal, RemoteHostPairingError>;

    async fn set_remote_host_target(
        &self,
        target: RemoteHostTarget,
    ) -> Result<RemoteHostTargetReceipt, RemoteHostPairingError>;
}
```

The receipt means the target was durably accepted, not that the remote host is
currently connected. `RemoteHostPairing` continuously reconciles the persisted
target with observed reality and projects progress, degradation, repair needs,
and revocation status into `AppStore`. Cold start, reachability changes, session
loss, and duplicate lifecycle hints all call the same internal reconciler. There
is no public `resume`, `recover`, `retry`, or `revoke` operation.

The implementation is centered on four explicit internal seams:

1. `RemotePairingHostPort` for one complete Remora-owned host dialect;
2. `OwnedRelayPort` nested inside a host adapter for relay discovery and dialing;
3. `PairingSecretPort` for device identity and host credentials;
4. `PairingJournalPort` for versioned desired state and crash recovery.

The host adapter is vertical. It owns the compatible bundle of invite decoding,
identity proof, host protocol, transport, relay use, and harness wire mechanics.
The core does not combine a decoder from one dialect with a transport or
harness from another. Universal policy—consent revision, desired runtime intent,
transaction ordering, retry classification, concurrency, revocation, recovery,
and `AppStore` projection—remains in the core.

The ports are not exported through UniFFI. `MobileClient` constructs the module
with production adapters; tests construct it with in-memory adapters. Swift and
Kotlin remain driving adapters for capture and typed intent plus narrow driven
adapters for platform secret storage.

## Why this is a genuinely different design

| Design | External time model | Primary extensibility unit | Recovery model |
| --- | --- | --- | --- |
| Minimal | Request/response inspect, connect, revoke | One internal transport adapter | Explicit resume intent |
| Flexible/data-driven | Durable attempt/action workflow | Decoder, transport, relay, and harness registries | Public recover trigger |
| This design | Declare desired host state; observe convergence | One end-to-end host dialect adapter | Automatic target reconciliation |

The radical difference is not the number of trait declarations. Both earlier
designs already contain internal adapters. The difference is the external model
and variation axis:

- Pairing and unpairing are target transitions, not imperative connection and
  revocation commands.
- `AppStore` holds observed state separately from the durable target. The
  target can remain `Paired` while the host is temporarily unavailable.
- Recovery replays neither a UI workflow nor a caller-supplied resume request;
  it converges durable target to observed state.
- A protocol family is one vertical adapter. The flexible design's horizontal
  decoder × transport × relay × harness cross-product is deliberately absent.
- User interaction is fixed and typed: review a proposal, then set `Paired` or
  `Absent`. There are no generic action IDs or server-shaped UI descriptors.

Seam nesting is equally important. The Iroh/Alleycat host adapter depends on
`OwnedRelayPort`; the core never receives route candidates or selects a relay.
Relay failover, route security, and host identity continuity stay local to the
adapter that understands them.

The journal records proposals only after the user targets `Paired`. Unaccepted
proposals remain process-local and expire; process death requires a rescan.

## Current cluster and deletion test

The current workflow is spread across shallow surfaces:

- `ffi/alleycat.rs` exposes `AppAlleycatPairPayload`, including node ID, bearer
  token, relay, and protocol version, while `AlleycatBridge` only parses it.
- `ffi/discovery.rs` separately exposes list-agents and connect operations and
  makes callers supply the chosen wire.
- `RemotePairingSheet.swift` and `RemotePairingSheet.kt` both implement parse,
  probe, runtime defaults, stable ID construction, connect, token save, and
  error ordering.
- `DiscoveryView.swift` and `DiscoveryScreen.kt` persist non-secret host
  metadata after the pairing sheet reports success.
- The token is persisted independently. A token-store failure is logged but
  does not make the apparent pairing fail.
- `MobileClient` requires the platform to load the device endpoint key before
  first endpoint initialization and read it back afterward if Rust generated
  one.
- `SavedServerRecord` is a cross-transport bag containing raw Alleycat node,
  token, relay, agent, and wire fields for cold reconnect.
- The paired-terminal path reconstructs an Alleycat payload separately from
  ordinary runtime connection.

Deleting the current bridges would remove little policy. The same ordering,
secrets, stable identity, route, runtime selection, persistence, and reconnect
knowledge would remain in callers. Deleting the proposed module would force
that policy back across both platform UIs, reconnect, settings, and terminal
code. The proposed module therefore earns depth.

## Seam topology

```text
SwiftUI / Compose
  camera, clipboard, typed proposal, desired target
                      |
                      v
       AppClient driving adapter (UniFFI)
                      |
          +-----------+------------+
          | RemoteHostPairing core  |
          | target + observed state,|
          | transaction, reconcile, |
          | consent/store projection|
          +----+---------------+----+
               |               |
        PairingJournalPort  PairingSecretPort
               |               |
     atomic-file adapter   Keychain / Keystore adapters
               |               |
        in-memory fake      in-memory fake

               RemotePairingHostPort
                         |
             IrohAlleycatDialectAdapter
                         |
                  OwnedRelayPort
                         |
             Iroh relay adapter / fake
```

The port direction matters. `RemoteHostPairing` owns the port interfaces and
domain types. Adapters depend inward on those interfaces. The core does not
depend on Iroh, relay URLs, JSON frames, Keychain status codes, Android
preferences, or a concrete journal schema.

`AppStore` is not a port. It is canonical in-process observed state already
owned by `MobileClient`; the module updates it directly through private Rust
collaboration. Session registration, pure normalization, locks, clocks, and ID
derivation are also implementation details, not ports created for every helper.

## External interface

### Propose from opaque input

```rust
#[derive(Clone, uniffi::Record)]
pub struct RemotePairingCode {
    /// Exact text captured from QR or clipboard. Opaque to the platform.
    pub encoded: String,
}

#[derive(Clone, Eq, PartialEq, Hash, uniffi::Record)]
pub struct RemoteHostProposalId {
    pub value: String,
}
```

```rust
#[derive(Clone, Eq, PartialEq, Hash, uniffi::Record)]
pub struct RemoteHostId {
    pub value: String,
}

#[derive(Clone, Eq, PartialEq, Hash, uniffi::Record)]
pub struct RemoteRuntimeId {
    pub value: String,
}

#[derive(Clone, uniffi::Record)]
pub struct RemoteHostProposal {
    pub proposal_id: RemoteHostProposalId,
    pub host_id: RemoteHostId,
    pub revision: u64,
    pub suggested_display_name: String,
    pub previously_paired: bool,
    pub runtimes: Vec<RemoteRuntimeReview>,
    /// Informational only. Rust enforces expiry using a monotonic clock.
    pub expires_at_unix_ms: u64,
}

#[derive(Clone, uniffi::Record)]
pub struct RemoteRuntimeReview {
    pub id: RemoteRuntimeId,
    pub display_name: String,
    pub availability: RemoteRuntimeAvailability,
    pub recommended: bool,
    pub capabilities: RemoteRuntimeCapabilities,
}

#[derive(Clone, uniffi::Enum)]
pub enum RemoteRuntimeAvailability {
    Available,
    TemporarilyUnavailable,
    UnsupportedByClient,
    RequiresHostUpdate,
}

#[derive(Clone, uniffi::Record)]
pub struct RemoteRuntimeCapabilities {
    pub conversation: bool,
    pub terminal: bool,
    pub voice_handoff: bool,
    pub permission_controls: bool,
}

```

`propose_remote_host` performs bounded recognition, authenticates the host, and
returns a current semantic offer. It does not write a durable pairing target.
The private proposal cache retains the validated invite credential, identity
pin, adapter discriminator, and route hints until a short monotonic deadline.
The public proposal omits invite bytes, token, raw node ID, endpoint key, relay URL,
ALPN, transport kind, harness wire, route candidates, and resume cursors. Host
strings are length-limited, control-character stripped, and projected into
mobile-owned semantic fields.

Equivalent codes deduplicate to the same live proposal. Conflicting adapters
that both claim an input cause `AmbiguousCode`; registration order never selects
a security meaning. A proposal is process-local, single-host, and non-durable.
After process death or expiry the user must rescan.

### Declare desired state

```rust
#[derive(Clone, uniffi::Enum)]
pub enum RemoteHostTarget {
    PairedFromProposal {
        proposal_id: RemoteHostProposalId,
        expected_proposal_revision: u64,
        display_name: Option<String>,
        selected_runtime_ids: Vec<RemoteRuntimeId>,
    },
    PairedExisting {
        host_id: RemoteHostId,
        expected_target_revision: u64,
        display_name: String,
        selected_runtime_ids: Vec<RemoteRuntimeId>,
    },
    Absent {
        host_id: RemoteHostId,
        expected_target_revision: Option<u64>,
    },
}

#[derive(Clone, uniffi::Record)]
pub struct RemoteHostTargetReceipt {
    pub host_id: RemoteHostId,
    pub target_revision: u64,
    pub disposition: RemoteHostTargetDisposition,
}

#[derive(Clone, uniffi::Enum)]
pub enum RemoteHostTargetDisposition {
    Accepted,
    Unchanged,
}
```

`PairedFromProposal` is the fixed typed consent operation. The target is
accepted only if the proposal revision, host identity, expiry, display name, and
runtime selection still validate. The core then persists desired intent and the
credential through its journal/secret transaction before returning the receipt.

`PairedExisting` changes the display name or desired runtime set for a paired
host using optimistic concurrency. Reasserting an equivalent existing target is
idempotent and also nudges reconciliation, so no separate public retry method is
needed.

`Absent` is unpairing. Once this target is durably accepted, the host becomes
locally unusable immediately. Remote credential revocation and secret cleanup
continue in the reconciler. The receipt never overstates remote invalidation.

### Observe convergence through AppStore

```rust
#[derive(Clone, uniffi::Record)]
pub struct RemoteHostPairingSnapshot {
    pub host_id: RemoteHostId,
    pub target_revision: u64,
    pub target: RemoteHostTargetKind,
    pub observed: RemoteHostObservedState,
    pub display_name: String,
    pub runtimes: Vec<RemoteRuntimeObserved>,
    pub failure: Option<RemoteHostObservedFailure>,
    pub remote_revocation: Option<RemoteCredentialRevocation>,
}

#[derive(Clone, uniffi::Enum)]
pub enum RemoteHostTargetKind {
    Paired,
    Absent,
}

#[derive(Clone, uniffi::Enum)]
pub enum RemoteHostObservedState {
    Activating,
    Connected,
    Degraded,
    Unavailable,
    NeedsRepair,
    Revoking,
    Absent,
}

#[derive(Clone, uniffi::Enum)]
pub enum RemoteCredentialRevocation {
    Confirmed,
    UnsupportedByHost,
    DeferredHostUnavailable,
}
```

The durable target and observed state are intentionally separate. A target can
be `Paired` while observed state is `Activating`, `Degraded`, or `Unavailable`.
That is not false success: the receipt acknowledges durable intent, while the
snapshot reports current reality. A target can be `Absent` while observed state
is `Revoking` until remote and local cleanup finish.

Ordinary runtime health, thread/session hydration, and terminal state remain in
their existing `AppStore` projections. The pairing snapshot owns only desired
host membership, activation/repair, and revocation convergence.

## External errors and retry contract

```rust
#[derive(Debug, uniffi::Error)]
pub enum RemoteHostPairingError {
    InvalidCode { reason: PairingCodeFailure },
    AmbiguousCode,
    IncompatibleHost { required_client_version: Option<String> },
    ProposalNotFound,
    ProposalExpired,
    ProposalConsumed,
    ProposalChanged { current_revision: u64 },
    InvalidRuntimeSelection,
    AuthenticationRejected,
    HostIdentityMismatch,
    HostUnavailable { retry_after_ms: Option<u64> },
    NoSafeRoute { retry_after_ms: Option<u64> },
    NotPaired,
    StaleTarget { current_revision: u64 },
    SecretStoreUnavailable { operation: SecretOperation },
    JournalUnavailable { operation: JournalOperation },
    ConcurrentModification,
    Cancelled,
    Internal { correlation_id: String },
}

#[derive(Clone, uniffi::Enum)]
pub enum PairingCodeFailure {
    Empty,
    Malformed,
    MissingHostIdentity,
    MissingCredential,
    InvalidRouteHint,
    Expired,
}

#[derive(Clone, uniffi::Enum)]
pub enum SecretOperation {
    LoadDeviceIdentity,
    StageHostCredential,
    LoadHostCredential,
    DeleteHostCredential,
}

#[derive(Clone, uniffi::Enum)]
pub enum JournalOperation {
    AcceptPairedTarget,
    UpdatePairedTarget,
    AcceptAbsentTarget,
    LoadTargets,
    Recover,
}
```

These errors mean the requested target transition was not durably accepted.
Host/route/runtime failures after a target is accepted belong in
`RemoteHostObservedFailure`, not as a late error from `set_remote_host_target`.
External errors still hide adapter mechanics:

- host and relay failures during proposal creation are normalized to
  `HostUnavailable`/`NoSafeRoute` in the proposal error detail;
- failures during reconciliation become display-safe observed failures;
- bearer rejection or device-proof rejection during proposal creation becomes
  `AuthenticationRejected`; after acceptance it becomes `NeedsRepair` and never
  silently falls back to a weaker dialect;
- Keychain, keystore, preferences, file, and compare-and-swap errors map to
  their typed secret/journal operation;
- raw adapter errors remain in redacted Rust diagnostics under the correlation
  ID.

Retry rules are part of the interface:

- proposal-time host/route failures may retry the same code while it remains
  valid;
- `ProposalChanged` requires a fresh returned proposal and new consent;
- `SecretStoreUnavailable` and `JournalUnavailable` are retryable only after
  the failing facility becomes available;
- `ProposalExpired`, `InvalidCode`, `AuthenticationRejected`,
  `HostIdentityMismatch`, and `IncompatibleHost` require a new code or a
  host/client update.
- `StaleTarget` requires reloading the current target revision from `AppStore`.
- A retryable observed failure is retried automatically with bounded backoff;
  reasserting an unchanged paired target may request immediate reconsideration
  but cannot bypass host rate limits or authentication failures.

## Required invariants

### Proposal, target, and consent

1. A proposal is created only by a specific `MobileClient`, is process-local,
   time-limited, single-host, and single-consume.
2. A proposal binds one authenticated host identity, one dialect adapter, one
   invite credential, and one current semantic offer. It cannot be retargeted.
3. Propose never creates durable desired state. Process death before accepting
   `PairedFromProposal` requires a rescan.
4. `PairedFromProposal` accepts only the exact proposal revision the user saw.
   Identity or runtime-offer changes never inherit prior consent.
5. `PairedExisting` and `Absent` require the current target revision. Stale
   settings screens cannot overwrite newer desired state.
6. Every selected runtime ID must appear in the proposal and be selectable. At
   least one runtime must be selected.
7. Desired target is durable; observed state is current. A target receipt never
   implies current connectivity or completed remote revocation.

### Security and identity

8. Invite credentials, host credentials, relay hints, raw endpoint IDs, device
   private keys, resume cursors, and wire details never enter platform UI state,
   saved-server records, `AppStore` snapshots, analytics, or normal logs.
9. `RemoteHostId` is derived in Rust from the authenticated normalized host
   identity. Swift/Kotlin never construct `alleycat:<nodeId>`.
10. A relay supplies reachability, never identity authority. Every direct or
    relayed connection must prove the host identity pinned by the invite.
11. Route fallback may change performance but cannot weaken identity,
    confidentiality, protocol version, or consent.
12. The device endpoint identity is loaded or durably created before the first
    network adapter binds it. There is no platform load-before-bind call order.
13. Secret aliases are generated by Rust and opaque to platform adapters.
    Journal records refer to aliases, never secret bytes.
14. A v1 bearer credential remains a compatibility secret. A future v2
    device-bound credential changes the host and secret adapters, not the
    external mobile interface.

### Transactions and recovery

15. Pairing success is durable. Commit does not return success until the journal
    is active and at least one selected runtime is attached.
16. The journal and secret store are independent failure domains. The module
    uses explicit transaction markers; it never assumes cross-store atomicity.
17. Every staged secret is named by a journal transaction before it is written,
    so recovery can identify and delete or finish it.
18. Journal transitions use compare-and-swap revisions. A stale writer cannot
    overwrite an absent target or a newer paired generation.
19. Absence wins races. Once `Absent`/`Revoking` is durably written, a stale
    paired reconciler and late adapter completion cannot reactivate the host.
20. A secure-delete failure leaves `Revoking` in place. The host remains locally
    unusable until cleanup succeeds.
21. Recovery is idempotent. Repeating startup recovery after any crash point
    reaches the same active host, tombstone, or clean absence.
22. Unknown future journal versions are quarantined and surface a typed repair
    state; they are never partially decoded.

### Sessions and platform parity

23. The full selected runtime target is persisted even if zero or only a subset
    attaches initially. The reconciler retains intent and retries eligible
    members; it never silently shrinks the target.
24. One host operation lock serializes target transitions and reconciliation
    effects for that host. Unrelated hosts may proceed concurrently.
25. Duplicate equivalent target submissions return `Unchanged` and do not
    create duplicate sessions or remote operations.
26. Replacement resources become authoritative before stale resources are
    closed, except revocation, which closes first and forbids replacement.
27. `AppStore` updates are authoritative. Native code never hand-patches a
    paired host after a successful operation.
28. iOS and Android use the same proposal, target, error, and snapshot types.
    Native code owns only capture, rendering, navigation, permissions, and the
    narrow secret adapter implementation.

## Internal port 1: remote-owned host

The host is remote but owned, so each complete protocol family satisfies one
domain-level port. The port is private to the Rust module and deals in
authenticated domain types, not JSON frames, QUIC streams, or relay URLs.

```rust
#[async_trait]
trait RemotePairingHostPort: Send + Sync {
    /// Cheap, bounded, and side-effect-free. Ambiguity fails closed.
    fn recognize(&self, code: &RemotePairingCode) -> Recognition;

    async fn propose(
        &self,
        request: HostProposalRequest,
    ) -> Result<AuthenticatedHostOffer, HostPortError>;

    async fn reconcile_paired(
        &self,
        request: HostReconcileRequest,
    ) -> Result<EstablishedRemoteHost, HostPortError>;

    async fn reconcile_absent(
        &self,
        request: HostRevokeRequest,
    ) -> Result<HostRevocation, HostPortError>;
}
```

Private request types carry the opaque code or sealed dialect grant, pinned host
identity, a secret lease, selected semantic runtime IDs, idempotency key, and
resume cursors.
Private results carry authenticated offers or logical runtime resources that
can be handed to `MobileClient`. They do not expose the underlying transport.

Adapters:

- `IrohAlleycatDialectAdapter`: production vertical adapter for current v1 code
  recognition, `alleycat/1`, list agents, Iroh transport, WebSocket/JSONL
  attachment, sequence resume, and future compatible Remora Link versions.
- `ScriptedHostAdapter`: in-memory test adapter that can change offers, reject
  credentials, delay completions, partially attach runtimes, report replay
  drift, and support or reject remote revocation.
- A future proximity or hosted-host protocol becomes another adapter only if it
  provides the same authenticated semantics. The older plaintext proximity
  pairing path must not be wrapped as equivalent without a separate security
  redesign.

The adapter owns code decoding, protocol negotiation, transport, and harness
selection. The core owns consent, target ordering, durable state, and convergence policy. This is a
real seam because production and scripted adapters both exist, and a future
host protocol can vary without changing Swift/Kotlin.

Registration is deterministic. Exactly one adapter must strongly recognize a
code. No adapter may delegate an unrecognized or authentication-failed code to a
weaker adapter as fallback. A genuinely new protocol family adds one vertical
adapter and one registration entry, not coordinated decoder, planner,
transport, relay, and harness plug-ins.

## Internal port 2: owned relay, nested in the host adapter

Relay is also remote but owned. It varies for production and testing, but its
seam belongs inside the network adapter rather than at the pairing core.

```rust
#[async_trait]
trait OwnedRelayPort: Send + Sync {
    async fn resolve(
        &self,
        request: RelayResolutionRequest,
    ) -> Result<Vec<AuthenticatedRouteCandidate>, RelayPortError>;

    async fn open_route(
        &self,
        candidate: AuthenticatedRouteCandidate,
        expected_host: HostIdentityPin,
    ) -> Result<OwnedRoute, RelayPortError>;
}
```

Adapters:

- `IrohRelayAdapter`: current Iroh direct/relay discovery and route opening.
  Protocol identifiers such as `alleycat/1` and compatibility relay fields stay
  here.
- `InMemoryRelayAdapter`: deterministic routes, outages, reordering, latency,
  stale hints, and malicious identity substitution for tests.
- A future Remora-hosted relay adapter is not added until hosted relay
  infrastructure enters the product boundary. The port permits it; this design
  does not claim it exists.

`IrohAlleycatHostAdapter<R: OwnedRelayPort>` consumes this port. The core sees
only `HostPortError::RouteExhausted`, then maps it to `NoSafeRoute`. It never
ranks URLs or applies relay fallback policy itself.

Nesting produces better locality:

- Iroh version or relay-policy changes stay in the host adapter package;
- host identity verification is enforced at every opened route;
- core interface tests do not need to script transport trivia;
- relay adapter contract tests can still exhaustively verify failover and
  identity preservation.

Flattening relay candidates into `RemoteHostPairing` would make the core a
transport planner and would force every host adapter to share Iroh-shaped
concepts. That would widen both the internal port and the knowledge callers
need to maintain.

## Internal port 3: local secret storage

Secret persistence is local-substitutable and platform-backed. The module owns
a narrow opaque-blob port:

```rust
#[async_trait]
trait PairingSecretPort: Send + Sync {
    async fn read(
        &self,
        alias: SecretAlias,
    ) -> Result<Option<SecretBytes>, SecretPortError>;

    async fn write(
        &self,
        alias: SecretAlias,
        value: SecretBytes,
        policy: SecretAccessPolicy,
    ) -> Result<(), SecretPortError>;

    async fn delete(
        &self,
        alias: SecretAlias,
    ) -> Result<SecretDeleteDisposition, SecretPortError>;
}
```

The production adapters are:

- iOS Keychain, using device-only accessibility appropriate to the secret;
- Android Keystore/encrypted storage;
- in-memory crash-injectable storage for Rust interface tests.

The platform adapter may receive bytes because it is the trusted persistence
edge, but those bytes never enter view models, saved-server types, or UI code.
The callback implementation must live in a dedicated adapter file and expose no
Alleycat-specific methods such as `saveToken(nodeId, token)` or
`loadDeviceSecretKey()`.

Aliases and serialization are Rust-owned. Required logical purposes include:

- one app installation/device endpoint identity;
- one active or staged credential per paired-host generation;
- future per-host non-exportable device-key handles where the platform facility
  supports operations by handle rather than raw key export.

The v1 adapter may need raw bearer bytes. A v2 adapter should prefer a
non-exportable signing-key handle and proof operation. That evolution can add a
capability-oriented secret operation internally without changing the external
pairing interface.

## Internal port 4: local journal

The journal is local-substitutable and non-secret. Its adapter stores opaque,
versioned bytes with compare-and-swap; Rust owns the record schema and
migration.

```rust
#[async_trait]
trait PairingJournalPort: Send + Sync {
    async fn read(
        &self,
        key: JournalKey,
    ) -> Result<Option<VersionedJournalBlob>, JournalPortError>;

    async fn compare_exchange(
        &self,
        key: JournalKey,
        expected_revision: Option<u64>,
        replacement: Option<JournalBlob>,
    ) -> Result<JournalWrite, JournalPortError>;

    async fn scan(
        &self,
        namespace: JournalNamespace,
    ) -> Result<Vec<(JournalKey, VersionedJournalBlob)>, JournalPortError>;
}
```

Adapters:

- `AtomicFilePairingJournalAdapter`: production Rust adapter rooted in an app
  support directory supplied at bootstrap; writes use a same-directory
  temporary file, flush, atomic replace, and directory durability where the OS
  permits it.
- `InMemoryPairingJournalAdapter`: deterministic test adapter with CAS races,
  torn-write injection, and restart snapshots.

If the repository later standardizes on SQLite or another local store, that
becomes a replacement adapter. The port does not expose tables or queries.

Representative private records are:

```text
CommitIntent {
  transaction_id,
  host_id,
  host_identity_pin,
  credential_alias,
  display_name,
  desired_runtime_ids,
  generation,
  created_at,
}

ActiveHost {
  host_id,
  host_identity_pin,
  credential_alias,
  display_name,
  desired_runtime_ids,
  protocol_profile,
  generation,
  last_sequence_cursors,
}

RevokingHost {
  prior_active_host,
  remote_revocation_status,
  cleanup_attempt,
}

RevokedHost {
  host_id,
  terminal_generation,
  remote_revocation_status,
}
```

Wire-specific fields may exist inside the versioned private `protocol_profile`,
but neither the adapter nor platform code decodes them.

## Cross-port transaction ordering

The secret store and journal cannot participate in one OS-level transaction.
The module therefore owns a small recovery protocol.

### Propose

1. Enforce global code size and text limits.
2. Ask every registered host adapter for bounded, side-effect-free recognition.
   Zero matches is `InvalidCode`; more than one strong match is `AmbiguousCode`.
3. Load the device identity from the secret port. If absent, generate it, write
   it durably, read it back, and only then allow the selected adapter to bind.
4. Ask that adapter to authenticate the host and produce a semantic offer. The
   adapter may use its nested relay port.
5. Normalize display fields and runtime semantics in the core. Cache the sealed
   adapter grant under a random, expiring proposal ID.
6. Return the secret-free proposal. Do not journal unaccepted intent.

### Accept a paired target

1. Acquire the per-host operation lock and validate proposal ownership, expiry,
   revision, display name, and runtime selection.
2. Read the current target generation. A `Revoking`/`Absent` generation blocks
   stale acceptance until secure cleanup finishes and a new proposal is made.
3. CAS a `CommitIntent` containing desired state, adapter discriminator,
   identity pin, and Rust-generated credential alias. No secret is written yet.
4. Write and verify the sealed host credential through the secret port.
5. CAS `CommitIntent` to `ActiveHost`; consume the proposal.
6. Return `Accepted` and schedule reconciliation. Do not wait for the host to be
   online or for every selected runtime to attach.

If the process dies after step 3, recovery removes the intent with no secret. If
it dies after step 4, recovery can delete the named staged secret or finish the
same target generation. If it dies after step 5, startup sees an active paired
target and reconciles it.

This ordering makes the target receipt honest: durable user intent and recovery
material exist, while connectivity remains separately observable.

### Reconcile a paired target

1. Load `ActiveHost` and its exact target revision.
2. If current sessions already satisfy desired runtimes, project `Connected` or
   `Degraded` and stop.
3. Read the credential by opaque alias.
4. Ask the recorded vertical adapter to reconcile the pinned host and desired
   runtimes using the target generation as an idempotency key.
5. Reject any identity change. Replace stale resources only after replacements
   prove the same host identity.
6. Update sequence cursors and observed `AppStore` state only if the target
   revision is still current. Late results for superseded or absent targets are
   closed and ignored.
7. Classify failures into automatic backoff, `Unavailable`, or `NeedsRepair`.

Cold launch, network changes, session loss, and reasserted targets schedule this
same implementation. Duplicate lifecycle hints coalesce per host. Native code
does not reconstruct pair payloads or push `SavedServerRecord` into Rust.

### Accept and reconcile an absent target

1. CAS the current target to `RevokingHost` with a new absent target revision.
2. Return `Accepted`. From this point local connection is forbidden.
3. Cancel paired reconciliation, invalidate proposals for the host, close
   runtime and terminal resources, and remove active canonical session state.
4. Ask the recorded host adapter for device/token revocation. Record confirmed,
   unsupported, or deferred without weakening local absence.
5. Delete the credential through the secret port.
6. CAS to `RevokedHost`/observed `Absent`. Retain a bounded tombstone so stale
   restores and late completions cannot resurrect the prior generation.

If secret deletion fails, remain `RevokingHost`, project the typed storage
failure, and retry cleanup on startup. No paired reconciliation is permitted
from that journal state.

## Hidden implementation and locality

A coherent layout is:

```text
shared/rust-bridge/codex-mobile-client/src/remote_host_pairing/
  mod.rs                    facade and external-operation implementation
  ffi.rs                    UniFFI-safe proposal, target, snapshot, errors
  proposal.rs               expiry, dedupe, adapter recognition, sealed grants
  target.rs                 desired target model and optimistic concurrency
  identity.rs               code validation and stable host/runtime identity
  transaction.rs            target acceptance and cross-store recovery
  reconciler.rs             desired-to-observed convergence and backoff
  recovery.rs               journal scan and startup convergence
  projection.rs             AppStore and semantic runtime projection
  ports/
    host.rs                 RemotePairingHostPort
    relay.rs                OwnedRelayPort
    secrets.rs              PairingSecretPort
    journal.rs              PairingJournalPort
  adapters/
    host_alleycat.rs         current vertical dialect adapter
    relay_iroh.rs            direct/relay Iroh behavior
    journal_atomic_file.rs   shared production journal adapter
```

Platform-only adapter files remain narrow:

```text
apps/ios/Sources/Remora/Bridge/PairingSecretAdapter.swift
apps/android/core/bridge/.../PairingSecretAdapter.kt
```

The module hides:

- dialect recognition, v1 JSON/URL parsing, compatibility aliases, version
  checks, and redaction;
- future v2 invite/device-credential semantics;
- stable host/runtime IDs and display-safe projection;
- device identity load/create/bind ordering;
- Iroh endpoint reuse, relay discovery, route fallback, ALPN, and identity pin;
- list-agents, harness-wire selection, multiplexed attachment, resume cursors,
  replay drift, and partial runtime recovery;
- secret aliases, journal versions, CAS generations, transaction IDs,
  compensation, tombstones, and crash recovery;
- desired/observed reconciliation, per-host locks, backoff, and late-completion
  suppression;
- `MobileClient` session handoff and authoritative `AppStore` updates;
- legacy record import and hard-cutover cleanup.

Terminal opening should eventually accept only `RemoteHostId` and a semantic
terminal intent. It loads the same active record and credential internally,
rather than reconstructing raw Alleycat fields in terminal code.

## Thin Swift usage

```swift
@State private var proposal: RemoteHostProposal?

func inspect(_ scannedOrPastedText: String) {
    Task {
        proposal = try await appModel.client.proposeRemoteHost(
            code: RemotePairingCode(encoded: scannedOrPastedText)
        )
    }
}

func accept(name: String?, selected: [RemoteRuntimeId]) {
    guard let proposal else { return }
    Task {
        let receipt = try await appModel.client.setRemoteHostTarget(
            target: .pairedFromProposal(
                proposalId: proposal.proposalId,
                expectedProposalRevision: proposal.revision,
                displayName: name,
                selectedRuntimeIds: selected
            )
        )
        // Navigate by receipt.hostId. Observe convergence in AppStore.
    }
}

func remove(_ host: RemoteHostPairingSnapshot) async throws {
    _ = try await appModel.client.setRemoteHostTarget(
        target: .absent(
            hostId: host.hostId,
            expectedTargetRevision: host.targetRevision
        )
    )
}
```

Swift owns the camera permission, scanner, clipboard, text editing, selections,
and navigation. It observes `RemoteHostPairingSnapshot` through the existing
`AppStore` subscription. It has no parse/list/connect/save-token/remember-server
sequence, reconnect choreography, or endpoint-key lifecycle calls.

## Thin Kotlin usage

```kotlin
var proposal by mutableStateOf<RemoteHostProposal?>(null)

suspend fun inspect(scannedOrPastedText: String) {
    proposal = appModel.client.proposeRemoteHost(
        RemotePairingCode(encoded = scannedOrPastedText),
    )
}

suspend fun accept(name: String?, selected: List<RemoteRuntimeId>) {
    val shown = requireNotNull(proposal)
    appModel.client.setRemoteHostTarget(
        RemoteHostTarget.PairedFromProposal(
            proposalId = shown.proposalId,
            expectedProposalRevision = shown.revision,
            displayName = name,
            selectedRuntimeIds = selected,
        ),
    )
}

suspend fun remove(host: RemoteHostPairingSnapshot) {
    appModel.client.setRemoteHostTarget(
        RemoteHostTarget.Absent(
            hostId = host.hostId,
            expectedTargetRevision = host.targetRevision,
        ),
    )
}
```

Compose and SwiftUI render the same fixed proposal and observed-state semantics.
They do not render generic server-authored actions and do not decode journal or
adapter state.

## Testing strategy

The external interface remains the primary test surface. Construct the real
module with `ScriptedHostAdapter`, `InMemoryRelayAdapter`,
`InMemoryPairingSecretAdapter`, `InMemoryPairingJournalAdapter`, deterministic
clock/entropy, and a real in-memory `AppStore`. Call only propose and set-target,
then observe convergence through `AppStore`.

### Interface contract tests

1. Malformed, missing-identity, missing-credential, invalid-route, expired, and
   incompatible codes produce typed errors.
2. A proposal contains semantic host/runtime data but none of the token,
   relay, raw node, wire, ALPN, endpoint key, or cursor sentinels.
3. Zero adapter recognizers produces `InvalidCode`; two strong recognizers
   produce `AmbiguousCode` independent of registration order.
4. A proposal from one `MobileClient` cannot be accepted by another.
5. Equivalent duplicate paired target returns `Unchanged`; a conflicting stale
   target revision fails without side effects.
6. An absent target racing paired reconciliation wins after its CAS and ignores
   late adapter completion.
7. Expiry uses the monotonic clock even if wall time moves backward.
8. A changed authenticated offer increments the proposal revision and requires
   fresh acceptance.
9. Empty, unknown, unavailable, duplicate, and cross-host runtime selections
   fail before secret or journal side effects.
10. Accepted paired target produces one `ActiveHost` and one credential alias,
    returns before network completion, then converges to authoritative
    `AppStore` state.
11. Zero initial attachment preserves the paired target as `Unavailable`;
    partial attachment preserves the complete desired set as `Degraded`; later
    reconciliation retries eligible members.
12. A healthy session makes reconciliation a no-op; concurrent triggers
    coalesce.
13. Absent target racing activation writes `Revoking`/`Revoked`, closes late resources, and
    cannot be undone by a stale CAS.
14. Unsupported v1 remote revocation is reported honestly while local
    revocation remains effective.
15. Duplicate cold-start, network-change, session-loss, and manual target hints
    converge without storms or duplicate approval.
16. Logs, errors, journal bytes, and external records contain no raw secret
    sentinels except encrypted/opaque secret-adapter storage.

### Cross-port crash matrix

Inject process death or adapter failure at every transition:

| Last durable state | Secret state | Expected recovery |
| --- | --- | --- |
| none | none | no host, no work |
| `CommitIntent` | none | remove intent |
| `CommitIntent` | staged | delete staged secret or safely retry identical commit |
| `CommitIntent` | staged, host attached | close/expire remote work, then clean or finish idempotently |
| `ActiveHost` | present, no local session | reconnect and attach |
| `RevokingHost` | present | block reconnect, retry remote/local cleanup |
| `RevokingHost` | absent | finalize tombstone |
| `RevokedHost` | absent | remain terminal and idempotent |

Run the matrix with duplicate recovery calls and reordered late host completions.
Every row must converge to one active generation or one terminal tombstone,
never both.

### Adapter contract tests

`RemotePairingHostPort` adapters share a contract for:

- authenticated identity proof on inspect, establish, reconnect, and revoke;
- idempotency-key behavior after lost responses;
- runtime-offer normalization and changed-offer detection;
- partial attachment, cancellation, timeout, and late completion;
- resume cursor and replay-drift reporting;
- remote revocation status.

`OwnedRelayPort` adapters share a contract for:

- preserving the expected host identity across direct and relayed routes;
- never treating route metadata as an identity proof;
- deterministic exhaustion, backoff hints, cancellation, and stale-route
  rejection;
- failure under an identity-substituting malicious relay fake.

`PairingSecretPort` adapters share a contract for:

- opaque alias semantics, device-only accessibility, overwrite policy, exact
  readback, idempotent delete, unavailable/locked behavior, and no logging;
- platform-specific device restore/clone expectations;
- future non-exportable signing-key handles where supported.

`PairingJournalPort` adapters share a contract for:

- atomic replacement, compare-and-swap conflict, durable revision, namespace
  scan, unknown-version preservation, and crash/torn-write behavior.

Keep frame and codec tests inside `IrohAlleycatHostAdapter`. Keep file-system
atomicity tests inside the journal adapter. Replace platform parse/list/connect
and token-save workflow tests after both platforms cut over; do not layer a new
test suite over the shallow old one.

### Cross-platform verification

- Generated bindings expose only code, proposal, typed target, host ID, observed
  snapshot, semantic runtime types, receipts, and typed errors.
- Generated bindings do not expose node ID, token, relay URL, protocol version,
  ALPN, WebSocket/JSONL, endpoint key, or journal record.
- iOS Keychain and Android secure-storage adapters pass the same adapter
  contract fixtures.
- SwiftUI and Compose fixtures render identical proposal, target, observed-state,
  and error semantics.
- QR, paste, accept-paired, automatic reconnect, and accept-absent work on both
  platforms.
- The minimum integration gate remains the commands in `CONTEXT.md`.

## Migration and hard cutover

Use an independently revertible, time-bounded transition with one default.

1. Add characterization tests for current v1 parsing, stable host identity,
   runtime projection, endpoint-key reuse, reconnect, and terminal access.
2. Introduce the four private port interfaces. Initially wrap current
   `alleycat.rs`, endpoint, and reconnect mechanics in
   `IrohAlleycatHostAdapter`; do not change mobile callers yet.
3. Move relay discovery/dialing behind `OwnedRelayPort` inside that host adapter.
   Preserve `alleycat/1` and compatibility fields privately.
4. Add the generic secret callback and production iOS/Android adapters. Migrate
   device key and tokens from old Alleycat-named slots by read-old, write-new,
   verify-new, then delete-old.
5. Add the production Rust journal adapter and transaction/recovery core.
6. Import each legacy paired host once:
   - derive and verify its stable authenticated host identity;
   - allocate a Rust-owned credential alias;
   - copy and verify the legacy credential through the secret port;
   - write one `ActiveHost` record;
   - mark the legacy record imported;
   - delete old non-secret fields only after the new record reconnects.
7. Records missing a credential, identity pin, or usable runtime intent become a
   typed `NeedsRepair`/`RePairRequired` projection. Do not guess a wire or claim
   successful migration.
8. Add the propose/set-target external interface and observed pairing snapshots;
   cut both pairing sheets to it in the same change set. This becomes the only
   default pairing path.
9. Make target reconciliation the sole paired-host cold-recovery authority.
   Route paired terminals through `RemoteHostId` plus the journal/secret ports.
   Stop sending `SavedServerRecord` Alleycat fields across UniFFI.
10. Route remove-server through target `Absent`, including asynchronous remote
    revocation, secret deletion, and tombstone recovery.
11. After migration tests and the defined compatibility window, remove
    `AlleycatBridge`, `AppAlleycatPairPayload`, `AppAlleycatAgentWire`, external
    list/connect methods, explicit endpoint-key lifecycle calls, platform token
    methods, and Alleycat-specific saved-server fields.
12. Remove forwarding adapters after the old call sites are gone. Do not retain
    dual public pairing paths permanently.

The legacy proximity `pair` module is a separate decision. If it has no current
callers, remove or quarantine it independently. Do not silently reinterpret it
as an adapter that satisfies authenticated remote-host semantics.

## Depth, locality, and seam tradeoffs

### Where this design is deep

- A caller learns proposal plus desired target while the module hides host
  authentication, relay behavior, secrets, transaction recovery, reconnect,
  runtime attachment, and revocation.
- Automatic reconciliation removes resume, recover, retry, and lifecycle
  choreography from callers.
- Vertical adapters prevent invalid decoder/transport/relay/harness
  combinations structurally.
- Host dialect changes are local to one adapter.
- Relay policy changes are local to the nested relay adapter.
- OS secret behavior is local to two narrow platform adapters.
- Journal schema and crash recovery are local to Rust.
- The same implementation pays back across iOS, Android, cold start, settings,
  and terminal access.

### Where this design is intentionally less deep

- The caller must understand the difference between durable target and observed
  state. That distinction is essential to avoid equating accepted intent with
  current connectivity.
- The caller handles proposal revision changes by re-rendering and asking for
  consent again. Hiding that would silently apply stale approval.
- Unaccepted proposals do not survive process death. The user rescans.
- A vertical dialect adapter can be internally large. Its depth is valuable only
  while universal consent, persistence, retry, and store policy remain in the
  core.

### Locality gains

- Network failures are normalized once at the host adapter/core seam rather
  than in two pairing sheets.
- Relay details never spread into the transaction core.
- Cross-store failure policy lives beside the journal state machine.
- Secret aliases and migration live beside the port that enforces them.
- Activation, reconnect, runtime repair, and revocation share one target
  generation/CAS authority.

### Costs and risks

- Desired-state semantics require a clear UI: `Paired + Unavailable` means the
  app will keep trying, not that the operation was falsely reported connected.
- Reasserted targets and lifecycle hints need coalescing and bounded backoff to
  prevent network storms.
- Four internal ports create more implementation types. Each is justified by a
  production and test adapter, and secret storage has two production adapters.
  Do not create further ports for pure helpers.
- The atomic-file journal adds durability engineering. If an existing robust
  local database is adopted, replace the adapter rather than changing core
  semantics.
- Nested relay tests require a host-adapter contract harness; core tests alone
  will not prove relay behavior.
- V1 remote revocation remains weak because the host uses a shared bearer token.
  The result type must remain honest until a device-scoped host protocol ships.

## Rejected variants

### Put relay selection in the pairing core

Rejected because route candidates, fallback, relay URLs, and Iroh policy would
widen the host port and couple every future host protocol to Iroh-shaped
mechanisms. Relay is a dependency of the production host adapter, not of user
pairing intent.

### Expose the four ports through UniFFI

Rejected because Swift/Kotlin would become the orchestration layer. Ports are
internal seams for the Rust implementation and its tests. Native code supplies
only the platform secret adapter at bootstrap.

### Make the draft a serializable record

Rejected because a record would either expose credentials or require a public
lookup ID and durable attempt cache. The process-local object is the authority
and keeps secret lifetime explicit.

### Journal prepare and resume it after restart

Rejected for this design. Persisting unaccepted invites creates a workflow
recovery problem, approval recreation rules, expiry translation, and additional
secret lifetime. That is the flexible design's strength and cost. Here, only
explicitly accepted commit/revoke transactions are durable.

### Replace typed acceptance with generic actions

Rejected because the present product has one concrete review interaction:
rename, select runtimes, accept/cancel. A generic action schema weakens
compile-time UI exhaustiveness and adds revision/action lifecycle unrelated to
the four dependency seams.

### Merge secret and journal into one persistence port

Rejected because they are materially different local facilities with different
security, atomicity, backup, and availability behavior. A combined port would
either hide impossible cross-store atomicity or force platform adapters to
implement transaction policy that belongs in Rust.

### Let `AppStore` own drafts or direct operations

Rejected because a draft is a short-lived authority, not canonical runtime
state. `AppStore` remains the snapshot/update owner for connected hosts;
`AppClient` remains the direct-operation seam.

## Success criteria

This design is successfully implemented when:

1. Swift and Kotlin pairing code contains no Alleycat/Iroh, node ID, token,
   relay, protocol version, ALPN, WebSocket/JSONL, endpoint key, journal state,
   or persistence ordering.
2. A scan produces an opaque Rust draft and a fixed typed review; commit accepts
   only that draft and review revision.
3. The production host adapter and scripted host fake satisfy the same host-port
   contract.
4. Relay outages, failover, and identity substitution are verified behind the
   nested relay port without changing core or platform fixtures.
5. iOS and Android secure-storage adapters satisfy the same secret-port
   contract while platform UI never sees secret bytes.
6. Crash injection after every commit/revoke durable step leaves one active
   generation, one terminal tombstone, or clean absence—never an apparent
   pairing with missing credentials.
7. Cold reconnect and paired terminal access use `RemoteHostId` only.
8. Remove-server performs local revocation, credential cleanup, and honest
   remote-revocation reporting.
9. Generated bindings contain semantic pairing types only.
10. The old external Alleycat and saved-record surfaces are removed after the
    explicit migration window.

This is the strongest design when the main concern is controlling remote and
local I/O failure at explicit seams without turning pairing into a general
workflow engine. It gives up the minimal design's smallest method count and the
flexible design's extensible action graph in exchange for capability safety,
adapter replaceability, and sharply localized transaction logic.
