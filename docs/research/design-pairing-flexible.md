# Flexible `RemoteHostPairing` module

Status: design only. This document proposes a replacement interface and migration shape; it does not authorize production changes.

## Conclusion

Make remote-host pairing one deep Rust module with three operations at its external seam:

```rust
pub(crate) struct RemoteHostPairing { /* private */ }

impl RemoteHostPairing {
    async fn ingest(
        &self,
        ingress: PairingIngress,
    ) -> Result<PairingIngressReceipt, PairingError>;

    async fn submit(
        &self,
        submission: PairingActionSubmission,
    ) -> Result<PairingActionReceipt, PairingError>;

    async fn recover(
        &self,
        trigger: PairingRecoveryTrigger,
    ) -> Result<PairingRecoveryReceipt, PairingError>;
}
```

`MobileClient` owns one `RemoteHostPairing`. The handwritten UniFFI projection exposes these operations on `AppClient`; `AppStore` owns `PairingAttemptSnapshot` state and updates. Swift and Kotlin provide captured bytes, render typed snapshots, submit actions that the current snapshot permits, and implement platform adapters such as secure blob storage and activity rendering. They never see a node ID, token, relay URL, ALPN, endpoint key, WebSocket/JSONL choice, request envelope, resume cursor, or transport fallback.

This is intentionally flexibility-first. A decoder registry accepts new invite encodings, a transport planner accepts new transports and relay strategies, and a harness registry accepts new host runtime protocols without changing the external interface. Recovery is a property of the workflow, not a platform recreation of connection parameters. Future notification ingress uses the same `ingest` operation, while Live Activities and Android notifications render the same display-safe activity projection already present in `AppStore`.

The module should eventually replace the current external `AlleycatBridge.parsePairPayload`, `listAlleycatAgents`, `connectRemoteOverAlleycat`, Alleycat-specific saved-server fields, explicit endpoint-key lifecycle calls, and the platform-owned parse/list/connect/persist sequence. Current `alleycat` and proximity `pair` implementations can remain temporarily as private adapters during a hard-cutover migration.

## Why the current seam is shallow

The current code correctly keeps JSON framing and Iroh connection internals in Rust, but its interface requires callers to know too much:

- `AppAlleycatPairPayload` exposes protocol version, node ID, token, relay, and host-name fields.
- `AppAlleycatAgentWire` makes Swift/Kotlin choose WebSocket versus JSONL.
- Both platforms mint `alleycat:<nodeId>` identities.
- Both platforms implement parse → list agents → select → connect → save token → persist endpoint key.
- Saved-server records duplicate node ID, token, relay, agent names, and wire selection across Swift, Kotlin, and Rust reconnect planning.
- Endpoint initialization has an ordering constraint: the platform must load the secret key before the first Alleycat operation and persist a generated key afterward.
- Current UI copy and previews are tied to JSON, protocol versions, node IDs, and relay URLs.
- Recovery reconstructs an Alleycat payload and transport choice from platform fields instead of resuming a Rust-owned pairing record.

The deletion test is decisive: deleting `AlleycatBridge` removes almost no complexity; the same parsing, ordering, persistence, identity, transport selection, and recovery knowledge remains distributed through both platform callers. Deleting the proposed `RemoteHostPairing` module would force all of that policy—and new encoding, transport, relay, harness, notification, and recovery policy—back into every caller. That is depth.

## Scope and seam placement

The module owns the workflow from opaque ingress through durable paired-host registration:

```text
camera / clipboard / URL / notification
                  |
                  v
       AppClient pairing methods       external seam
                  |
     +------------+-------------+
     | RemoteHostPairing (Rust)  |
     | decode, authenticate,     |
     | negotiate, select, retry, |
     | persist, recover, project |
     +------------+-------------+
       |          |           |
   invite      transport    harness       internal seams
   adapters    adapters     adapters
       |          |           |
       +----------+-----------+
                  |
          MobileClient sessions
                  |
               AppStore
                  |
       Swift / Kotlin render state
```

The external seam is the handwritten UniFFI surface on `AppClient`, not a new public bridge object and not `AppStore` methods. `AppClient` already owns direct operations; `AppStore` remains snapshots, updates, and truly store-local actions. Internally, `MobileClient` delegates to the `RemoteHostPairing` implementation.

The module ends after it has durably registered a paired host and handed connected runtime resources to the existing `MobileClient` session machinery. Long-lived thread/session hydration, terminal state, and general reconnect health remain in their existing modules. The pairing journal retains only the information required to audit, repair, or re-authorize the pairing.

The existing proximity `pair` module and Alleycat/Iroh path are not separate user concepts in the new interface. They are ways the implementation may fulfill a remote-host pairing attempt.

## Dependency classification

Following the deep-module dependency categories:

| Dependency | Category | Treatment |
| --- | --- | --- |
| Invite recognition, normalization, transition reducer, action validation, runtime selection, retry policy | In-process | Private implementation; test directly through the module interface. |
| Pairing journal and non-secret metadata store | Local-substitutable | Production filesystem/database adapter and in-memory adapter; no external port in the module interface. |
| Device key and invite/host credentials | Local-substitutable platform facility | Private `OpaqueSecretStore` adapter backed by Keychain or Android encrypted storage; in-memory adapter in tests. The platform stores opaque bytes under opaque aliases. |
| Remote host pairing endpoint and owned relay | Remote but owned | Internal ports with Iroh, LAN/proximity, future relay, and scripted in-memory adapters. |
| Host harness protocols | Remote but owned | Internal `HarnessAdapter` seam with current WebSocket and JSONL adapters, plus an in-memory adapter. |
| APNs, FCM, ActivityKit, Android notification manager | True external | Platform adapters or mocks. Do not add hosted push infrastructure while it remains outside the product boundary. |
| Clock, entropy, reachability, lifecycle signals | Local-substitutable | Inject private adapters for deterministic tests. |

The external interface does not expose these dependencies. They are internal seams used by the implementation and its tests.

## External interface

The following is illustrative Rust intended to be UniFFI-safe. Names may be adjusted during implementation, but the behavioral interface—including invariants, ordering, errors, and performance—is part of the proposal.

### 1. Ingest opaque input

```rust
#[derive(Clone, Debug, uniffi::Record)]
pub struct PairingIngress {
    /// Describes how bytes entered the app, not their encoding.
    pub carrier: PairingIngressCarrier,
    /// UTF-8 text, a URI, JSON, CBOR, a notification envelope, or a future
    /// encoding. Rust recognizes and validates it.
    pub payload: Vec<u8>,
    /// Platform delivery/capture identity when available. Used only for
    /// deduplication; it is not a host identity.
    pub delivery_id: Option<String>,
}

#[derive(Clone, Copy, Debug, uniffi::Enum)]
pub enum PairingIngressCarrier {
    Camera,
    Clipboard,
    DeepLink,
    Notification,
    NearbyShare,
    File,
    Unknown,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct PairingIngressReceipt {
    pub attempt_id: PairingAttemptId,
    pub disposition: PairingIngressDisposition,
    pub revision: u64,
}

#[derive(Clone, Copy, Debug, uniffi::Enum)]
pub enum PairingIngressDisposition {
    Started,
    Resumed,
    DuplicateIgnored,
}
```

`carrier` permits carrier-specific safety policy—notification size limits, deep-link origin checks, or camera copy—without telling the caller which invite encoding or transport is inside. `delivery_id` is optional because the module also computes a secret-safe invite fingerprint for deduplication.

`ingest` returns after bounded local validation and durable attempt creation. It does not wait for network negotiation. The caller observes progress in `AppStore` by `attempt_id`.

### 2. Submit only actions offered by the current snapshot

```rust
#[derive(Clone, Debug, uniffi::Record)]
pub struct PairingActionSubmission {
    pub attempt_id: PairingAttemptId,
    pub action_id: PairingActionId,
    /// Optimistic concurrency guard copied from the snapshot that displayed
    /// the action.
    pub expected_revision: u64,
    pub input: PairingActionInput,
}

#[derive(Clone, Debug, uniffi::Enum)]
pub enum PairingActionInput {
    None,
    Confirmed { accepted: bool },
    Choices { choice_ids: Vec<PairingChoiceId> },
    Text { value: String },
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct PairingActionReceipt {
    pub attempt_id: PairingAttemptId,
    pub accepted_revision: u64,
}
```

The platform never invents an operation such as “connect over Iroh,” “use relay X,” or “open JSONL agent.” It submits an opaque action ID issued by Rust with input that must match the action's typed input specification. Typical actions are confirm host, select runtimes, rename host, retry, cancel, or replace an existing pairing.

This data-driven action interface is the main flexibility trade: future workflows can compose existing confirmation, choice, text, retry, and cancel interactions without adding a UniFFI method. Truly new interaction semantics still require a typed interface revision; the module must fail closed rather than treat unknown interaction kinds as generic host-defined UI.

### 3. Recover after lifecycle or reachability changes

```rust
#[derive(Clone, Debug, uniffi::Enum)]
pub enum PairingRecoveryTrigger {
    ColdStart,
    AppBecameActive,
    NetworkReachable,
    BackgroundWake,
    UserRequested,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct PairingRecoveryReceipt {
    pub considered: u32,
    pub resumed: u32,
    pub deferred: u32,
}
```

`recover` is idempotent and returns after recovery work is scheduled. It hydrates journaled attempts, reconciles partially committed pairings, and resumes eligible work. It does not require Swift/Kotlin to reconstruct connection parameters. `ReconnectController` should call this module for pairing-specific recovery, then continue to own ordinary paired-host reconnection after commit.

### Opaque identities

```rust
pub type PairingAttemptId = String;
pub type PairingActionId = String;
pub type PairingChoiceId = String;
pub type PairedHostId = String;
pub type RemoteRuntimeId = String;
```

These strings are opaque at the external seam. Their format is not an interface guarantee. In particular, `PairedHostId` is not `alleycat:<nodeId>`, and `RemoteRuntimeId` is not a harness name or wire selector.

## `AppStore` projection

`AppStore` remains the canonical observable state owner. Add pairing attempts to its snapshot and typed updates instead of introducing a second poll queue.

```rust
#[derive(Clone, Debug, uniffi::Record)]
pub struct PairingAttemptSnapshot {
    pub id: PairingAttemptId,
    pub revision: u64,
    pub phase: PairingPhase,
    pub host: Option<PairingHostPreview>,
    pub runtimes: Vec<PairingRuntimeChoice>,
    pub actions: Vec<PairingActionDescriptor>,
    pub activity: PairingActivitySnapshot,
    pub failure: Option<PairingFailure>,
}

#[derive(Clone, Debug, uniffi::Enum)]
pub enum PairingPhase {
    Inspecting,
    ContactingHost,
    VerifyingHost,
    AwaitingUser,
    Establishing,
    Recovering,
    Paired { host_id: PairedHostId },
    Failed,
    Cancelled,
    Expired,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct PairingHostPreview {
    pub suggested_name: String,
    pub verification_summary: Option<String>,
    pub previously_paired: bool,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct PairingRuntimeChoice {
    pub id: RemoteRuntimeId,
    pub title: String,
    pub subtitle: Option<String>,
    pub availability: PairingRuntimeAvailability,
    pub recommended: bool,
    pub selected: bool,
    pub capabilities: PairingRuntimeCapabilities,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct PairingRuntimeCapabilities {
    pub conversation: bool,
    pub terminal: bool,
    pub voice_handoff: bool,
    pub permission_controls: bool,
}

#[derive(Clone, Debug, uniffi::Enum)]
pub enum PairingRuntimeAvailability {
    Available,
    TemporarilyUnavailable,
    UnsupportedByClient,
    RequiresHostUpdate,
}
```

`PairingRuntimeCapabilities` is a mobile-owned semantic projection. It does not expose the host's protocol names, transport, or WebSocket/JSONL wire. The host may advertise an unknown harness; a private adapter can support it without platform changes if it maps to known semantics. If its semantics cannot be safely projected, the runtime is `UnsupportedByClient` with display-safe copy.

### Data-driven actions

```rust
#[derive(Clone, Debug, uniffi::Record)]
pub struct PairingActionDescriptor {
    pub id: PairingActionId,
    pub title: String,
    pub role: PairingActionRole,
    pub input: PairingActionInputSpec,
    pub enabled: bool,
    pub expires_at_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, uniffi::Enum)]
pub enum PairingActionRole {
    Primary,
    Secondary,
    Cancel,
    Destructive,
}

#[derive(Clone, Debug, uniffi::Enum)]
pub enum PairingActionInputSpec {
    None,
    Confirmation { body: String },
    Choices {
        choices: Vec<PairingChoice>,
        minimum: u32,
        maximum: u32,
    },
    Text {
        label: String,
        initial_value: String,
        maximum_bytes: u32,
    },
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct PairingChoice {
    pub id: PairingChoiceId,
    pub title: String,
    pub subtitle: Option<String>,
    pub selected: bool,
    pub enabled: bool,
}
```

Action descriptors are generated by trusted Rust code, not passed through from an untrusted host. Host-provided names and descriptions are normalized, length-limited, stripped of control characters, and inserted only into designated fields. Rust owns action titles, roles, validation, and ordering.

### Activity projection for future Live Activities and notifications

```rust
#[derive(Clone, Debug, uniffi::Record)]
pub struct PairingActivitySnapshot {
    pub title: String,
    pub detail: Option<String>,
    pub progress: Option<f32>,
    pub state: PairingActivityState,
    pub updated_at_ms: u64,
    pub stale_at_ms: Option<u64>,
    pub deep_link_attempt_id: PairingAttemptId,
    pub supports_cancel: bool,
}

#[derive(Clone, Copy, Debug, uniffi::Enum)]
pub enum PairingActivityState {
    Active,
    WaitingForUser,
    Recovering,
    Succeeded,
    Failed,
    Ended,
}
```

This record contains display-safe, secret-free, bounded copy. ActivityKit, Android notifications, widgets, or a future Watch projection can render it without understanding pairing wire state. The platform decides whether and how to publish an activity; Rust decides what state is safe and meaningful to present.

Notification delivery is intentionally not implemented by this design because hosted push/proxy infrastructure is outside the current product boundary. If that scope changes, the notification payload enters as `PairingIngressCarrier::Notification`; Rust verifies and decodes the opaque bytes. No new parse or transport method is required. Device-token registration belongs in a separate delivery module, not in `RemoteHostPairing`.

## State machine and ordering

The implementation reducer owns all transitions. A representative flow is:

```text
ingest
  -> Inspecting
  -> ContactingHost
  -> VerifyingHost
  -> AwaitingUser (confirm and/or runtime choices)
  -> Establishing
  -> Paired

Any active phase
  -> Recovering -> previous eligible phase
  -> Failed -> AwaitingUser(retry) or Expired
  -> Cancelled
```

`Paired`, `Cancelled`, and `Expired` are terminal for that attempt. A failed attempt is non-terminal only when its `PairingFailure.recovery` offers a current retry action. Re-pairing an existing host creates a new attempt and an explicit replacement action; it does not mutate a terminal historical attempt.

The durable success ordering is:

1. Authenticate the invite and pin the host identity.
2. Negotiate host capabilities and translate them to mobile-owned runtime descriptors.
3. Obtain current user approval for the pinned identity and selected runtimes.
4. Establish at least one selected runtime and validate the returned identity against the pin.
5. Store credentials in the opaque secret store.
6. Commit the pairing journal and paired-host record atomically, referring only to credential aliases.
7. Attach runtime resources to `MobileClient` and publish authoritative `AppStore` server state.
8. Publish the terminal `Paired` attempt snapshot.

If the process dies between steps 5 and 7, `recover(ColdStart)` either completes the commit or removes orphaned credentials. The UI never observes `Paired` before durable recovery information exists. If selected runtimes attach partially, the module preserves user intent and reports a degraded host only if the existing session model explicitly supports that state; it must not silently shrink the saved selection.

## Required invariants

### Security and identity

1. Invite bytes, tokens, endpoint IDs, relay addresses, ALPNs, transport hints, harness wires, resume cursors, and device secret keys never appear in a UniFFI snapshot, platform saved-server record, log field, analytics field, or activity projection.
2. Host identity is authenticated before host metadata is treated as trusted. A relay or route change cannot change the pinned host identity.
3. Transport fallback may relax performance preferences, but never identity verification, confidentiality, or user-consent requirements.
4. A notification, deep link, or recovered journal entry may resume work but cannot approve a security-sensitive action on the user's behalf.
5. Duplicate/replayed ingress is idempotent. A one-time invite that the host reports as consumed cannot create a second pairing.
6. Credentials are addressed by opaque aliases. Non-secret metadata cannot be used to reconstruct a credential.
7. Action descriptors are produced locally by Rust. Untrusted host content cannot choose an action role, mark an action primary, or inject arbitrary UI.

### State and concurrency

8. Every snapshot revision increases monotonically per attempt.
9. Every submission must name an action in the exact revision the user saw. Stale, unknown, expired, disabled, or already-consumed actions fail without side effects.
10. Each action ID is single-consume. Retrying the same submission returns the same receipt or `ActionAlreadyApplied`; it never repeats a non-idempotent remote operation.
11. Operations for one attempt are serialized. Different attempts may progress concurrently, subject only to real adapter resource constraints.
12. At most one non-terminal attempt exists for the same authenticated invite fingerprint. Re-ingress resumes it.
13. A paired-host replacement cannot delete the old usable pairing until the replacement is durably committed.
14. Cancellation is best-effort against remote work but immediate in local state. Late adapter completions are ignored by generation/revision.

### Platform and presentation

15. Swift and Kotlin make no decisions from raw strings naming a protocol, transport, harness, status, or agent wire.
16. Both platforms render the same `PairingAttemptSnapshot` semantics and submit the same typed action inputs.
17. All platform-visible text is bounded and safe for display; activity text is additionally free of credential-like values and host-supplied control characters.
18. If an older app cannot render a required action input kind, the module exposes a typed unsupported-client failure; it does not guess or use an untyped payload.

### Persistence and recovery

19. Pairing journal writes are atomic and versioned. Unknown future journal versions are quarantined, not partially decoded.
20. The module can distinguish an uncommitted attempt, a committed paired host, an orphaned credential, and an attached session after a crash.
21. Recovery is idempotent under repeated lifecycle callbacks and duplicate background wakes.
22. Once a pairing becomes a normal paired host, ordinary connection health belongs to the existing session/reconnect modules; pairing recovery does not compete with session reconnect.

## Error interface

Errors crossing UniFFI are typed and sanitized. Raw adapter errors stay in structured Rust diagnostics with a correlation ID.

```rust
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum PairingError {
    #[error("pairing ingress is too large")]
    IngressTooLarge,
    #[error("invalid pairing ingress: {reason:?}")]
    InvalidIngress { reason: PairingIngressFailure },
    #[error("invite is unsupported by this client")]
    UnsupportedInvite { required_client: Option<String> },
    #[error("invite has expired")]
    ExpiredInvite,
    #[error("invite has already been used")]
    InviteAlreadyUsed,
    #[error("remote host identity does not match the invite")]
    HostIdentityMismatch,
    #[error("remote host rejected pairing")]
    HostRejected,
    #[error("remote host has no compatible runtime")]
    NoCompatibleRuntime,
    #[error("required platform capability is unavailable: {capability:?}")]
    PlatformCapabilityUnavailable { capability: PairingPlatformCapability },
    #[error("secure pairing storage is unavailable")]
    SecureStorageUnavailable,
    #[error("remote host is unavailable")]
    HostUnavailable { retry_after_ms: Option<u64> },
    #[error("no safe route to the remote host is available")]
    RouteUnavailable { retry_after_ms: Option<u64> },
    #[error("pairing is rate limited")]
    RateLimited { retry_after_ms: u64 },
    #[error("pairing action is stale")]
    StaleAction { current_revision: u64 },
    #[error("pairing action is invalid")]
    InvalidAction,
    #[error("pairing action was already applied")]
    ActionAlreadyApplied { accepted_revision: u64 },
    #[error("pairing attempt was not found")]
    AttemptNotFound,
    #[error("pairing attempt is already terminal")]
    AttemptTerminal,
    #[error("pairing recovery is deferred")]
    RecoveryDeferred,
    #[error("internal pairing failure; correlation id: {correlation_id}")]
    Internal { correlation_id: String },
}

#[derive(Clone, Copy, Debug, uniffi::Enum)]
pub enum PairingIngressFailure {
    Empty,
    Malformed,
    UntrustedOrigin,
    InvalidSignature,
    UnsupportedCarrier,
}

#[derive(Clone, Copy, Debug, uniffi::Enum)]
pub enum PairingPlatformCapability {
    SecureStorage,
    LocalNetwork,
    ProximityProof,
    BackgroundExecution,
}
```

Asynchronous failures live in the snapshot:

```rust
#[derive(Clone, Debug, uniffi::Record)]
pub struct PairingFailure {
    pub category: PairingFailureCategory,
    pub title: String,
    pub detail: String,
    pub recovery: PairingRecoveryDisposition,
    pub correlation_id: Option<String>,
}

#[derive(Clone, Copy, Debug, uniffi::Enum)]
pub enum PairingFailureCategory {
    InvalidInvite,
    Authentication,
    HostUnavailable,
    RouteUnavailable,
    RuntimeUnavailable,
    PlatformUnavailable,
    Storage,
    UnsupportedClient,
    Internal,
}

#[derive(Clone, Debug, uniffi::Enum)]
pub enum PairingRecoveryDisposition {
    None,
    Automatic { retry_at_ms: u64 },
    UserAction { action_id: PairingActionId },
    NewInviteRequired,
    ClientUpdateRequired,
}
```

The distinction is deliberate:

- A synchronous `PairingError` means the requested interface operation was not accepted.
- `PairingFailure` means an accepted attempt later reached a user-visible failure state.
- Temporary route/relay/harness failures are implementation concerns until policy is exhausted; the user sees one normalized failure, not a cascade of wire errors.
- Authentication and identity failures never automatically fall back or retry indefinitely.

## Hidden implementation

A coherent initial layout would be:

```text
shared/rust-bridge/codex-mobile-client/src/remote_host_pairing/
  mod.rs                 facade and the three-operation interface
  model.rs               internal identities, intent, journal records
  reducer.rs             state transitions and action validation
  coordinator.rs         async effects and generation control
  ingress.rs             size/origin checks and decoder registry
  planner.rs             transport/relay/harness candidate policy
  projection.rs          AppStore and activity snapshots
  recovery.rs            journal hydration and crash reconciliation
  persistence.rs         journal + credential-alias transactions
  adapters/
    invite_json_v1.rs
    invite_uri_v1.rs
    invite_binary_v2.rs  when a second binary encoding is real
    transport_iroh.rs
    transport_proximity.rs
    relay_iroh.rs
    harness_websocket.rs
    harness_jsonl.rs
```

Only `mod.rs` and UniFFI-safe projection types participate in the external seam. The rest are private implementation or internal seams.

### Invite decoder registry

Each decoder performs cheap recognition before full parsing:

```rust
trait InviteDecoder: Send + Sync {
    fn recognize(&self, carrier: PairingIngressCarrier, payload: &[u8]) -> Recognition;
    fn decode(&self, payload: &[u8]) -> Result<DecodedInvite, InviteDecodeError>;
}
```

`DecodedInvite` is private and may carry protocol versions, identity material, route hints, relay hints, expiry, capabilities, and proof challenges. Decoder precedence is deterministic. Ambiguous payloads fail rather than letting adapter order select a meaning. The registry enforces global and encoding-specific size/depth limits before allocation.

Do not add `invite_format` to `PairingIngress`; recognizing the format is work the module should hide. Do not expose `DecodedInvite` through UniFFI.

### Transport and relay adapters

The transport seam is real because current Iroh and proximity/LAN paths already vary and the test suite needs a scripted adapter:

```rust
#[async_trait]
trait PairingTransport: Send + Sync {
    fn supports(&self, invite: &DecodedInvite, environment: &NetworkContext) -> Support;
    async fn inspect(&self, request: InspectRequest) -> Result<HostOffer, TransportFailure>;
    async fn establish(&self, request: EstablishRequest) -> Result<EstablishedLink, TransportFailure>;
    async fn resume(&self, request: ResumeRequest) -> Result<EstablishedLink, TransportFailure>;
}
```

`TransportPlanner` ranks candidates using authenticated invite constraints, network context, prior failures, and product policy. It records diagnostics privately. An `EstablishedLink` proves the same pinned host identity and offers logical channels; it does not leak Iroh connections or sockets into the coordinator.

Relay behavior sits behind transport-private adapters. Current Iroh relay resolution and a future owned relay can produce route candidates. The relay is never the identity authority. If only one relay implementation exists at implementation time, keep its seam private and concrete; promote a relay port only when a second production or test adapter makes it real.

### Harness adapters

Host offers advertise authenticated harness descriptors. The registry maps them to private adapters:

```rust
#[async_trait]
trait HarnessAdapter: Send + Sync {
    fn supports(&self, descriptor: &HarnessDescriptor) -> bool;
    fn project_runtime(&self, descriptor: &HarnessDescriptor) -> Result<RuntimeOffer, HarnessFailure>;
    async fn attach(&self, channel: LogicalChannel, intent: RuntimeIntent)
        -> Result<RuntimeSessionResource, HarnessFailure>;
}
```

Current WebSocket and JSONL paths satisfy this seam. Adapter selection is based on an authenticated host descriptor, not a platform choice. Runtime capability projection is conservative: unknown capability bits do not become true. `RuntimeSessionResource` plugs into `ServerSession::connect_remote_multiplexed` and the existing reconnect transport abstraction.

### Persistence adapters

The module stores two forms of data:

- A versioned journal and paired-host record containing non-secret state, opaque credential aliases, host pin, user intent, and recovery generation.
- Secret blobs such as device keys and invite/host credentials in an `OpaqueSecretStore`.

Swift's Keychain and Android's encrypted storage can remain production adapters, but their interface becomes `get(alias) -> bytes`, `put(alias, bytes)`, and `delete(alias)` instead of Alleycat-specific token and endpoint-key methods. Platform code does not assign aliases or decode bytes. An in-memory adapter supports tests.

Journal and secret writes use a small transaction protocol with intent markers so crash recovery can remove or complete partial writes even when the OS secret store cannot join a database transaction.

## Usage from Swift

The platform maps capture output to bytes and otherwise follows Rust state:

```swift
func acceptScannedCode(_ text: String) {
    Task {
        do {
            let receipt = try await appModel.client.ingestRemoteHostPairing(
                ingress: PairingIngress(
                    carrier: .camera,
                    payload: Data(text.utf8),
                    deliveryId: nil
                )
            )
            selectedAttemptId = receipt.attemptId
            // The existing AppModel observes AppStore. No parse/list/connect
            // sequence and no credential persistence occurs here.
        } catch {
            presentPairingIngressError(error)
        }
    }
}

func submit(_ action: PairingActionDescriptor, input: PairingActionInput) {
    guard let attempt else { return }
    Task {
        _ = try await appModel.client.submitRemoteHostPairingAction(
            submission: PairingActionSubmission(
                attemptId: attempt.id,
                actionId: action.id,
                expectedRevision: attempt.revision,
                input: input
            )
        )
    }
}
```

The view renders `attempt.host`, `attempt.runtimes`, `attempt.actions`, and `attempt.failure`. It does not show a protocol version, relay URL, node ID, or wire label. A platform activity controller observes `attempt.activity` and mirrors it into ActivityKit if that feature is later in scope.

## Usage from Kotlin

```kotlin
fun acceptQrText(text: String) {
    scope.launch {
        val receipt = appModel.serverBridge.ingestRemoteHostPairing(
            PairingIngress(
                carrier = PairingIngressCarrier.CAMERA,
                payload = text.encodeToByteArray(),
                deliveryId = null,
            )
        )
        selectedAttemptId = receipt.attemptId
    }
}

fun submit(action: PairingActionDescriptor, input: PairingActionInput) {
    val attempt = selectedAttempt ?: return
    scope.launch {
        appModel.serverBridge.submitRemoteHostPairingAction(
            PairingActionSubmission(
                attemptId = attempt.id,
                actionId = action.id,
                expectedRevision = attempt.revision,
                input = input,
            )
        )
    }
}
```

Compose and SwiftUI therefore have the same workflow and test vocabulary.

## Recovery behavior

Recovery is not simply “retry connect.” It reconciles durable intent and external effects:

| Journal state | Observable state | Recovery action |
| --- | --- | --- |
| Ingress accepted, no host contact | `Inspecting` or `Recovering` | Re-run decoder from sealed invite material if unexpired. |
| Identity pinned, awaiting action | `AwaitingUser` | Recreate locally generated actions; never replay approval. |
| Establishing, no credential commit | `Recovering` | Probe host with the same identity pin and idempotency key. |
| Credential stored, record not committed | `Recovering` | Complete record commit or delete orphan after validation. |
| Record committed, no session attached | `Recovering` | Hand the paired-host record to normal connection establishment. |
| Paired host/session attached | `Paired` | No pairing work; ordinary reconnect owns health. |
| Cancelled/expired | terminal | Clean private ephemeral state; never resume. |

Retry policy uses failure classes, attempt generation, monotonic deadlines while the process is alive, persisted wall-clock deadlines across restarts, bounded jittered backoff, and adapter-provided retry hints. User-requested retry may bypass a scheduled delay but not a host rate limit, invite expiry, or authentication failure.

An authenticated notification can carry a recovery hint for an existing attempt or paired host. `ingest` deduplicates it, verifies that it names the pinned identity internally, and then schedules `recover(BackgroundWake)`. The platform does not parse the hint.

## Performance characteristics

These are part of the interface:

- `ingest` performs bounded decoding and a journal write, then returns. It must not wait for host discovery or connection establishment.
- Default maximum ingress size is 64 KiB; the notification carrier may impose a smaller limit. Decoders apply nesting, collection-count, and string-length limits.
- `submit` performs local revision/action validation before any remote work and returns a receipt once the command is durably accepted.
- `recover` schedules eligible work and does not wait for all attempts to finish.
- AppStore updates are coalesced by attempt revision. There is no 100 ms polling queue.
- Snapshot collections are bounded: runtime and action lists reject or truncate only through an explicit safe policy, with a visible unsupported-host failure if required choices cannot be represented.
- One shared Iroh endpoint remains a private implementation optimization; the external interface does not promise endpoint lifetime or transport reuse.
- Attempts serialize their own effects but do not globally serialize unrelated hosts.
- Background activity projections are small and do not include full diagnostics or host offers.

## Test strategy

The interface is the test surface. Tests should construct `RemoteHostPairing` with in-memory adapters, call only `ingest`, `submit`, `recover`, and observe `AppStore` snapshots plus paired-host/session effects.

### Interface contract tests

1. JSON QR ingress reaches an authenticated runtime-choice snapshot without exposing wire fields.
2. URI/deep-link encoding produces the same semantic attempt and host identity as the equivalent JSON invite.
3. A future binary encoding can be added by registering a decoder without changing caller fixtures or UniFFI types.
4. Duplicate delivery IDs and equivalent invite fingerprints return the existing attempt.
5. Ambiguous decoder recognition fails closed.
6. Runtime selection attaches WebSocket and JSONL harnesses through the same external actions.
7. An unknown compatible harness becomes usable when a private adapter is registered; platform fixtures remain unchanged.
8. Transport fallback preserves the identity pin and user approval.
9. Relay failure followed by direct-path success produces one normalized progress stream.
10. A stale action revision, duplicate action, wrong input kind, invalid choice, and expired action each fail without remote side effects.
11. Cancel racing with transport success remains cancelled.
12. Partial multi-runtime success preserves requested intent and follows the documented degraded/failure policy.

### Recovery crash matrix

Inject a process stop after every durable-success ordering step. On restart, assert that recovery reaches exactly one of:

- the same current user action with no duplicated approval;
- one committed paired host with no orphan secret;
- a normalized recoverable failure;
- a cleaned cancelled/expired attempt.

Run each crash point for current Iroh, proximity/LAN, direct and relay routes, WebSocket, JSONL, and multi-runtime attachment. Use a deterministic clock and entropy adapter.

### Property and fuzz tests

- Decoder fuzzing: arbitrary bytes never panic, allocate beyond limits, or produce unvalidated identity material.
- Reducer property tests: revisions are monotonic; terminal states never leave terminal; only listed actions transition state.
- Idempotency property tests: arbitrary duplicate/reordered ingress, submissions, adapter completions, and recovery triggers produce one semantic outcome.
- Secret projection tests: seed every private field with sentinels and assert no UniFFI snapshot, log formatter, activity projection, or serialized non-secret record contains them.
- Host-copy sanitization tests: control characters, bidi markers, oversized strings, and credential-like values do not escape designated bounded fields.

### Adapter contract tests

Every transport adapter runs a common contract suite for identity proof, cancellation, timeout, resume, retry classification, and late completion. Every harness adapter runs a common suite for capability projection, attach, resume cursor behavior, and stream closure. Every persistence adapter runs atomicity and orphan-cleanup tests.

Keep narrow codec/frame tests inside adapters, but replace platform pairing workflow tests with interface tests. Tests that assert `AppAlleycatPairPayload`, `AppAlleycatAgentWire`, `alleycat:<nodeId>`, or platform token persistence should be deleted after cutover because they test past the new interface.

### Cross-platform verification

- Generated Swift and Kotlin bindings contain only carrier/action/snapshot semantic types, not Alleycat/Iroh/wire records.
- SwiftUI and Compose snapshot fixtures render the same phases, runtime choices, actions, and failures.
- QR camera, clipboard, and deep-link ingress work on both platforms.
- Lifecycle tests call `recover` repeatedly and prove idempotency.
- Existing Rust, iOS simulator, and Android unit/build gates in `CONTEXT.md` remain the minimum integration gate.

## Migration and cutover

Prefer a time-bounded transition with one clear default:

1. Add `remote_host_pairing` internally and implement the Iroh invite, transport, relay, WebSocket, and JSONL adapters around current code.
2. Add pairing snapshots to `AppStore` and the three `AppClient` operations.
3. Add generic opaque journal/secret-store platform adapters.
4. Move both pairing sheets to snapshot/action rendering. The new interface becomes the only default path on both platforms in the same change set.
5. Import existing paired-host records once into the Rust-owned record form. Records lacking sufficient secure material become a typed `NeedsRepairing` host and require a new invite.
6. Route reconnect through the paired-host repository. Stop reconstructing `ParsedPairPayload` from `SavedServerRecord`.
7. Remove the external `AlleycatBridge`, `AppAlleycatPairPayload`, `AppAlleycatAgentWire`, explicit endpoint-key lifecycle methods, Alleycat-specific platform credential methods, and saved-server fields after one release or once migration telemetry/tests show no remaining records—whichever removal criterion the release owner chooses.
8. Keep protocol identifiers such as `alleycat/1` only inside private compatibility adapters, per `CONTEXT.md`.

Do not layer the new module on top of the old public interface permanently. The old seam should disappear; otherwise callers still have two ways to pair and the implementation cannot enforce its invariants.

## Tradeoffs

### What is stronger

- **Depth:** three operations exercise invite parsing, negotiation, transport and relay choice, harness selection, user interaction, durable commit, recovery, and activity projection.
- **Locality:** a new encoding, transport, relay, or harness changes one private registry/adapter rather than Swift, Kotlin, saved-server models, reconnect code, and UI copy.
- **Parity:** both platforms render the same Rust-owned state machine.
- **Recovery:** a journaled workflow can distinguish partial durable states instead of blindly rebuilding parameters.
- **Security:** the external seam is secret-free and cannot choose unsafe wire details or stale actions.
- **Future delivery:** opaque ingress and display-safe activity snapshots accommodate notification wakes and Live Activities without putting hosted push concerns into pairing now.

### Costs and risks

- The implementation is significantly more sophisticated than the current linear parse/list/connect flow. Reducer/effect separation and fault-injection tests are required to keep it understandable.
- Data-driven actions trade some compile-time UI exhaustiveness for flexibility. The allowed input specifications must remain small, typed, locally generated, and versioned; a generic JSON action payload would make the module shallow again.
- Opaque persistence improves locality but makes direct platform debugging harder. Provide Rust diagnostics and an explicit redacted support export rather than exposing stored internals.
- A transactional protocol across a journal and OS secret store adds recovery complexity. That complexity already exists implicitly today; this design concentrates and tests it.
- Multiple private adapters can tempt speculative abstraction. Apply the “two adapters means a real seam” rule: build registries around current Iroh/proximity and WebSocket/JSONL variation, but do not create a push-delivery port or multiple relay tiers until a second real adapter exists.
- Existing host compatibility constrains a hard cutover. Private compatibility adapters may retain old protocol names and fields, but no new caller may depend on them.
- AppStore pairing snapshots add state volume. Bound attempts, archive terminal history, and project only current/recent attempts to mobile.

## Rejected alternatives

### Expose a generalized decoded invite

Rejected because a record such as `{ protocol, endpoints, relay, token, harnesses }` merely renames the current leak. Every new encoding or transport would still change Swift/Kotlin and persistence.

### Let callers select a transport and harness

Rejected because callers cannot safely rank route security, identity continuity, resume support, or harness compatibility. Platform UI should select user intent—runtimes and confirmation—not mechanisms.

### Add one method per workflow step

Methods such as `parse`, `probe`, `list_runtimes`, `select_transport`, `connect_runtime`, `save_token`, and `resume` form a shallow module. They export ordering constraints and make crash recovery a caller problem.

### Put pairing actions on `AppStore`

Rejected because network and credential operations are direct workflow operations. `AppStore` should remain the canonical state/update surface; `AppClient` is the external seam for commands.

### Return an async event stream or retain a poll queue

Rejected because the repository already has `AppStore` snapshot/update observation. A second event mechanism creates ordering and lifecycle ambiguity. Return receipts for command acceptance and observe authoritative state in one place.

### Add push registration now

Rejected because hosted push/proxy infrastructure is explicitly outside the current product boundary, and one hypothetical adapter does not justify a seam. Opaque notification ingress plus the activity projection keeps the future path open without speculative production machinery.

## Success criteria

The design is successfully implemented when:

1. Swift and Kotlin pairing code contain no Alleycat/Iroh, node ID, relay, token, protocol-version, ALPN, WebSocket, JSONL, or endpoint-key concepts.
2. Both platforms invoke only ingest, submit, and recover, then render `AppStore` snapshots.
3. Current QR/Iroh pairing, proximity pairing where retained, multi-runtime selection, reconnect, cancellation, and credential persistence pass through the new module.
4. Adding a second invite encoding and a scripted transport requires no external interface or platform workflow change.
5. Crash tests at every commit step leave no duplicate host, orphan credential, repeated approval, or false `Paired` state.
6. A future activity renderer can consume `PairingActivitySnapshot` without access to private pairing state.
7. The old external pairing surface and Alleycat-specific platform persistence are removed on the documented cutover schedule.
