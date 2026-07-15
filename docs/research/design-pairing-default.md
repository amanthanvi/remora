# RemoteHostPairing: optimize the default caller

## Recommendation

Make the common pairing flow one Rust-owned operation:

```rust
async fn pair_remote_host(
    code: RemotePairingCode,
) -> Result<RemoteHostPairingOutcome, RemoteHostPairingError>;
```

The caller supplies exactly the string it scanned or received from paste. While
the call runs, it renders typed progress already projected through `AppStore`.
On success, the returned host is connected, durably paired, and enrolled in
Rust-owned automatic resume. There is no public `resume_remote_host` method and
no platform orchestration between parse, probe, select, connect, save, and
reconnect.

Keep a reviewed path for users and security cases that cannot take the safe
default:

```rust
async fn inspect_remote_host_pairing(
    code: RemotePairingCode,
) -> Result<RemoteHostPairingReview, RemoteHostPairingError>;

async fn commit_remote_host_pairing(
    review_id: RemoteHostPairingReviewId,
    decision: RemoteHostPairingDecision,
) -> Result<RemoteHostConnection, RemoteHostPairingError>;

async fn revoke_remote_host(
    host_id: RemoteHostId,
) -> Result<RemoteHostRevocation, RemoteHostPairingError>;

async fn cancel_remote_host_pairing(
    attempt_id: RemoteHostPairingAttemptId,
) -> Result<RemoteHostPairingCancellation, RemoteHostPairingError>;
```

All five entry points live on `AppClient`. `RemoteHostPairing` is one deep
Rust module owned by `MobileClient`, not a second public UniFFI object. The
external seam and UniFFI-safe types belong in
`src/ffi/remote_host_pairing.rs`; orchestration belongs in
`src/remote_host_pairing/`. Existing Alleycat mechanics become an internal
transport adapter. Pairing progress is canonical operation state in `AppStore`
and reaches both platforms through the existing snapshot/update subscription.

This interface deliberately spends four methods on advanced, revocation, and
cancellation cases so the overwhelmingly common caller does not have to learn offers,
runtime IDs, display-name normalization, credentials, reconnect records, or
ordering. The implementation shares one prepare/commit pipeline internally;
the external interface is shaped by user intent, not internal phases.

## Why the current callers need this module

The current iOS flow parses the secret-bearing payload, loads agents, chooses
defaults, connects, saves the token, and persists the endpoint key in the view
(`RemotePairingSheet.swift:380-464`). Android repeats the same sequence
(`RemotePairingSheet.kt:124-249`). The Rust seam separately exports
`list_alleycat_agents` and `connect_remote_over_alleycat`
(`ffi/discovery.rs:288-329`). Later, platform lifecycle controllers synchronize
saved records and invoke a separate `ReconnectController`, while Rust rebuilds
an `AlleycatPairPayload` from those broad records (`reconnect.rs:287-326`).

That is a shallow cluster. A caller must know:

- that parsing precedes probing;
- which available agents count as safe defaults;
- how server IDs, node IDs, wire kinds, and display names relate;
- when the token and endpoint key must be persisted;
- how partial connection and persistence failures roll back;
- which saved fields are required for cold launch and foreground recovery;
- when network-change, reconnect, and resubscription must run.

The deletion test is decisive: delete the proposed module and this complexity
reappears in Swift, Kotlin, reconnect planning, settings removal, and tests.
With the module present, the common caller knows only "give Rust a code, render
progress, handle connected or review-required."

## External interface

The sketches below are illustrative UniFFI-safe Rust. Identifiers are records
instead of interchangeable strings.

```rust
#[derive(Clone, uniffi::Record)]
pub struct RemotePairingCode {
    /// Exact scanned or pasted text. Sensitive input; never logged.
    pub encoded: String,
}

#[derive(Clone, Eq, PartialEq, Hash, uniffi::Record)]
pub struct RemoteHostId {
    /// Stable, normalized host identity. Opaque to Swift and Kotlin.
    pub value: String,
}

#[derive(Clone, Eq, PartialEq, Hash, uniffi::Record)]
pub struct RemoteHostPairingReviewId {
    /// Process-local, short-lived capability; not a credential.
    pub value: String,
}

#[derive(Clone, Eq, PartialEq, Hash, uniffi::Record)]
pub struct RemoteHostPairingAttemptId {
    /// Opaque operation identity used only for progress and cancellation.
    pub value: String,
}

#[derive(Clone, uniffi::Enum)]
pub enum RemoteHostPairingOutcome {
    Connected { connection: RemoteHostConnection },
    ReviewRequired { review: RemoteHostPairingReview },
}

#[derive(Clone, uniffi::Record)]
pub struct RemoteHostConnection {
    pub host_id: RemoteHostId,
    pub display_name: String,
    pub disposition: RemoteHostConnectionDisposition,
}

#[derive(Clone, uniffi::Enum)]
pub enum RemoteHostConnectionDisposition {
    Paired,
    AlreadyPaired,
}
```

`pair_remote_host` returns `ReviewRequired` as a successful typed outcome, not
as an error. Needing a human decision is a valid branch of the workflow. It is
never collapsed into a localized string or guessed by platform code.

### Typed progress

Add one optional field to `AppSnapshotRecord` and one targeted store update:

```rust
pub struct AppSnapshotRecord {
    // existing fields...
    pub remote_host_pairing: Option<RemoteHostPairingProgress>,
}

pub enum AppStoreUpdateRecord {
    // existing variants...
    RemoteHostPairingChanged {
        progress: RemoteHostPairingProgress,
    },
}

#[derive(Clone, uniffi::Record)]
pub struct RemoteHostPairingProgress {
    pub attempt_id: RemoteHostPairingAttemptId,
    pub phase: RemoteHostPairingPhase,
}

#[derive(Clone, uniffi::Enum)]
pub enum RemoteHostPairingPhase {
    ValidatingCode,
    AuthenticatingHost,
    DiscoveringRuntimes,
    ConnectingRuntimes {
        connected: u32,
        desired: u32,
    },
    SecuringPairing,
    AwaitingReview {
        review_id: RemoteHostPairingReviewId,
        reason: RemoteHostPairingReviewReason,
    },
    Connected {
        host_id: RemoteHostId,
    },
    Failed {
        kind: RemoteHostPairingFailureKind,
        retryable: bool,
    },
    Cancelled,
}

#[derive(Clone, uniffi::Enum)]
pub enum RemoteHostPairingFailureKind {
    PairingInProgress,
    InvalidCode,
    IncompatibleProtocol,
    ReviewExpired,
    ReviewStale,
    InvalidRuntimeSelection,
    Revoked,
    AuthenticationRejected,
    HostUnavailable,
    NoRuntimeConnected,
    ProtocolViolation,
    PersistenceUnavailable,
}
```

Phases express stable user meaning, not Alleycat frames. They intentionally do
not include node IDs, tokens, relay URLs, endpoint keys, raw protocol errors,
or per-stream wire state. The UI may map phases to localized copy and animation
but does not infer state by inspecting strings.

Only one foreground pairing attempt may be active per `MobileClient`. A second
start returns `PairingInProgress` with the current non-secret attempt ID. This
keeps the snapshot singular, prevents two scans from racing credential commits,
and matches the single pairing sheet on both platforms. Progress remains at
its terminal phase until the next attempt or process restart, so a lagged
subscriber can recover it from a full snapshot.

Returning `ReviewRequired` releases the active-operation slot but retains that
one review capability and its original attempt ID. Starting another pair or
inspect call first invalidates that uncommitted review, then replaces the
snapshot with a new attempt; the old review can no longer commit. If no newer
attempt intervenes, `commit_remote_host_pairing` reacquires the active slot and
continues progress under the original attempt ID. A commit racing an active
operation returns `PairingInProgress`; a commit using an invalidated review
returns `ReviewStale`. A monotonic-clock expiry task changes an untouched
`AwaitingReview` snapshot to terminal `Failed(ReviewExpired)`.

Generated UniFFI task cancellation is not the cancellation seam. The UI calls
`cancel_remote_host_pairing` with the attempt ID from typed progress before it
dismisses or abandons the task. Cancellation is best effort during an
indivisible secure-store write, but the original pair/commit call does not
return `Cancelled` until rollback or commit recovery has left a coherent
durable state. Repeating cancellation is idempotent and returns
`NoActiveMatch` after the attempt becomes terminal; an unknown or stale ID has
the same no-op result.

### Safe automatic policy

`pair_remote_host` connects without a review only when all of these are true:

1. The code is valid, compatible, and authenticates one normalized host
   identity.
2. The identity does not conflict with another saved host or a revocation in
   progress.
3. At least one available, non-beta runtime belongs to the host's recommended
   set. On legacy hosts without recommendation metadata, all available
   non-beta runtimes form the default set, matching today's UI behavior.
4. No runtime requires a user choice that the client cannot derive safely.
5. The suggested display name can be normalized without colliding with an
   identity-sensitive replacement flow. Cosmetic duplicate names alone may be
   disambiguated locally and do not block pairing.

If any condition requiring judgment fails, the method returns a sanitized
`RemoteHostPairingReview`. It does not silently downgrade, pick a beta-only
runtime, replace a credential, or overwrite an existing identity.

### Reviewed path

```rust
#[derive(Clone, uniffi::Record)]
pub struct RemoteHostPairingReview {
    pub review_id: RemoteHostPairingReviewId,
    pub attempt_id: RemoteHostPairingAttemptId,
    pub host_id: RemoteHostId,
    pub suggested_display_name: String,
    pub reason: RemoteHostPairingReviewReason,
    pub runtimes: Vec<RemoteRuntimeChoice>,
    pub recommended_runtime_ids: Vec<String>,
    pub replacement: Option<RemoteHostReplacementReview>,
    pub expires_at_unix_ms: u64,
}

#[derive(Clone, uniffi::Enum)]
pub enum RemoteHostPairingReviewReason {
    UserRequested,
    NoSafeDefaultRuntime,
    BetaRuntimeOnly,
    ExistingHostConflict,
    CredentialReplacement,
}

#[derive(Clone, uniffi::Record)]
pub struct RemoteRuntimeChoice {
    pub runtime_id: String,
    pub display_name: String,
    pub available: bool,
    pub recommended: bool,
    pub is_beta: bool,
    pub presentation: Option<AppAgentPresentation>,
    pub capabilities: Option<AppAgentCapabilities>,
}

#[derive(Clone, uniffi::Record)]
pub struct RemoteHostReplacementReview {
    pub existing_host_id: RemoteHostId,
    pub candidate_host_id: RemoteHostId,
    pub existing_display_name: String,
}

#[derive(Clone, uniffi::Record)]
pub struct RemoteHostPairingDecision {
    pub display_name: Option<String>,
    pub selected_runtime_ids: Vec<String>,
    pub replacement: Option<RemoteHostReplacementApproval>,
}

#[derive(Clone, uniffi::Record)]
pub struct RemoteHostReplacementApproval {
    /// Both identities must still match the review at commit time.
    pub existing_host_id: RemoteHostId,
    pub candidate_host_id: RemoteHostId,
}
```

`inspect_remote_host_pairing` always returns a review and never durably pairs,
even when automatic pairing would be safe. It exists for an explicit "Advanced"
UI that edits the display name or runtime selection before commit.

A review omits credentials, raw node identity, relay details, transport wire,
and endpoint secrets. Its ID references sensitive material held only in a
short-lived process-local cache. Replacement approval names both identities;
a stale generic boolean cannot authorize replacement after the underlying
records change.

### Revocation

```rust
#[derive(Clone, uniffi::Record)]
pub struct RemoteHostRevocation {
    pub host_id: RemoteHostId,
    pub disposition: RemoteHostRevocationDisposition,
    pub host_credential_status: HostCredentialRevocationStatus,
}

pub enum RemoteHostRevocationDisposition {
    Revoked,
    AlreadyRevoked,
}

pub enum HostCredentialRevocationStatus {
    Confirmed,
    UnsupportedByHostProtocol,
}

#[derive(Clone, uniffi::Enum)]
pub enum RemoteHostPairingCancellation {
    CancellationRequested,
    NoActiveMatch,
}
```

Revocation is included because automatic resume is part of pairing's contract.
The same module that decides a host may resume must provide the authoritative
way to end that relationship. Today's bearer-token Alleycat protocol can
guarantee local revocation by this installation but cannot honestly claim host-
side invalidation; the result makes that limitation explicit.

Successful revocation atomically replaces the secret-bearing active envelope
with a non-secret tombstone containing the host ID, a one-way fingerprint of
the revoked credential generation, and revocation status. It does not delete
the slot. This tombstone makes `AlreadyRevoked` durable and prevents a copied
old bearer code from silently re-enrolling the host. A later code with a new
credential generation must enter the reviewed `CredentialReplacement` path;
an exact revoked credential fails with `Revoked`.

## Error interface

```rust
#[derive(Debug, uniffi::Error)]
pub enum RemoteHostPairingError {
    PairingInProgress { attempt_id: RemoteHostPairingAttemptId },
    InvalidCode { problem: PairingCodeProblem },
    IncompatibleProtocol { host_version: u32, client_version: u32 },
    ReviewExpired,
    ReviewStale,
    InvalidRuntimeSelection,
    NotPaired,
    Revoked,
    AuthenticationRejected,
    HostUnavailable,
    NoRuntimeConnected,
    ProtocolViolation,
    PersistenceUnavailable { phase: PairingPersistencePhase },
    Cancelled,
}

pub enum PairingCodeProblem {
    Malformed,
    MissingNode,
    InvalidNode,
    MissingCredential,
    InvalidRelay,
}

pub enum PairingPersistencePhase {
    DeviceIdentity,
    CommitPairing,
    BeginRevocation,
    EraseCredential,
}
```

Retry semantics are part of the interface:

- `HostUnavailable`, `NoRuntimeConnected`, and
  `PersistenceUnavailable(CommitPairing)` are retryable while a returned review
  remains valid; automatic callers may submit the original code again.
- `ReviewExpired` requires a fresh inspect or pair call.
- `ReviewStale` requires a fresh review because host identity, availability, or
  replacement state changed.
- `InvalidRuntimeSelection` requires a non-empty selection from the current
  review's available runtimes.
- `InvalidCode`, `AuthenticationRejected`, and `IncompatibleProtocol` require a
  new code or host/client update.
- `Revoked` and `NotPaired` require a new pairing before connection.
- `ProtocolViolation` is not retried against the same response. The
  implementation keeps a redacted diagnostic for support.
- `PairingInProgress` is retryable after the current attempt terminates; the
  caller may instead keep rendering the already-observable progress.

Errors thrown by the call and the terminal `Failed` progress phase carry the
same stable failure kind. Error descriptions may add user-facing context but
callers never branch on descriptions.

## Invariants

1. **Secrets stay behind the seam.** Tokens, raw relay details, endpoint secret
   keys, and persisted envelopes never cross into general Swift/Kotlin state or
   logs.
2. **One normalized identity.** `RemoteHostId` is derived in Rust from the
   validated node identity. Platforms cannot synthesize it from payload text.
3. **The default is safe, not merely convenient.** Automatic pairing happens
   only under the stated safe-default policy. Every identity replacement,
   beta-only selection, or ambiguous selection becomes `ReviewRequired`.
4. **Inspect does not pair.** It may perform an authenticated one-shot probe,
   but it creates neither a durable host record nor a live app session.
5. **Reviews are capabilities, not credentials.** They are process-local,
   single-host, short-lived, invalidated by revocation, and retained after only
   retryable commit failures.
6. **Success is connected and durable.** `Connected` is returned only after at
   least one desired runtime is attached and the complete pairing envelope is
   atomically committed. Persistence failure rolls back the new session.
7. **Desired selection survives partial availability.** A reviewed selection
   is authoritative intent. If at least one runtime attaches, the entire set is
   persisted and missing runtimes remain pending for automatic resume.
8. **Pairing is idempotent.** Submitting a code for an already-paired healthy
   host with the same credential and desired defaults returns `AlreadyPaired`.
   Concurrent attempts do not create duplicate sessions or envelopes.
9. **Auto-resume is installed by success.** A successful envelope contains all
   data needed for cold launch, foreground recovery, network change, sequence
   resume, and resubscription. No platform supplies a token or reconstructs a
   pair payload later.
10. **Resume has no external seam.** Retry/backoff, connection replacement,
    path migration, sequence cursors, and post-reconnect hydration are internal
    policy. Current health remains observable in canonical `AppStore` server
    state.
11. **Revocation wins races.** Once a durable revocation tombstone exists,
    pair, commit, and automatic-resume workers cannot recreate the relationship.
12. **Revocation is idempotent.** A repeated request after cleanup returns
    `AlreadyRevoked`. Cleanup removes credential material but retains the
    non-secret tombstone until an explicitly reviewed new credential replaces
    it.
13. **Endpoint identity is stable.** The device key is durably stored before
    first bind. Revoking one host does not rotate the app-wide key used by other
    hosts.
14. **Progress is ordered and sanitized.** For one attempt, phases move forward
    in semantic order; repeated `ConnectingRuntimes` values may only increase
    `connected`. Progress contains no secret-bearing diagnostic data.
15. **The store is authoritative.** The module updates pairing progress and
    server health in `AppStore`; platforms do not hand-patch connection state
    after a return.

## Operation ordering

### Automatic pair

1. Reserve the single active-attempt slot and publish `ValidatingCode`.
2. Trim, decode, validate, and normalize the code entirely in Rust.
3. Derive `RemoteHostId`; reconcile any revocation tombstone before continuing.
4. Load or create the stable device endpoint key and durably save it before
   endpoint bind.
5. Publish `AuthenticatingHost`, then run a one-shot authenticated probe.
6. Publish `DiscoveringRuntimes`; normalize runtime metadata and compute the
   safe default set.
7. If judgment is required, cache the sensitive offer, publish
   `AwaitingReview`, and return `ReviewRequired`.
8. Otherwise publish `ConnectingRuntimes`; attach selected runtime streams and
   construct the multiplexed `ServerSession`. Fail if none attach.
9. Publish `SecuringPairing`; atomically commit one opaque, versioned pairing
   envelope containing identity, credential, relay hint, display name, desired
   runtimes, wire choices, and resume metadata.
10. If commit fails, disconnect the new session, clear reconnect targets and
    server state, preserve only a still-valid in-memory review when safe, and
    return a typed persistence error.
11. Publish canonical connected health and terminal `Connected`; return the
    connection.

The live session is established before durable commit so Remora does not retain
a credential for a host it has never reached. Transactional rollback makes the
external result all-or-nothing.

### Inspect and commit

Inspect shares steps 1-6, caches an offer, always publishes `AwaitingReview`,
and returns sanitized review data. The review retains the original attempt ID;
another start invalidates it, while expiry terminalizes its progress. Commit
then:

1. Acquires the per-host operation lock and resolves the review capability.
2. Rechecks expiry, revocation, host identity, replacement records, runtime
   availability, display-name normalization, and selection.
3. Requires exact replacement identities when replacement approval is needed.
4. Runs steps 8-11 of automatic pairing through the same private commit path.

The automatic and reviewed paths cannot drift because they share preparation,
selection validation, connection, persistence, and rollback implementation.

### Automatic resume

After a successful pair, startup, foreground recovery, network reachability,
and stream loss all enter one internal `resume(host_id)` implementation:

1. Load the opaque envelope. A tombstone blocks resume.
2. Return internally if the current healthy session satisfies the desired set.
3. Probe availability and open desired streams on the stable endpoint.
4. Resume sequence when possible; choose fresh, resumed, or drift-reload
   behavior internally.
5. Replace stale resources only after replacements are ready.
6. Publish canonical health, restore pending runtimes, and resubscribe/hydrate.

Platform lifecycle code supplies only lifecycle and network hints. It does not
sync pairing records, inject credentials, choose wire kinds, retry twice, or
persist endpoint keys after reconnect.

### Revoke

1. Acquire the per-host operation lock.
2. Atomically replace the active envelope with a revocation tombstone. Do not
   begin destructive cleanup if this write fails.
3. Cancel pairing/reconnect work, invalidate reviews, close host sessions and
   terminals, and clear restart targets.
4. Request host-side credential revocation if the protocol supports it.
5. Securely erase credential material by atomically replacing the revoking
   record with a non-secret completed tombstone, then remove active canonical
   host state. If replacement fails, retain the effective revoking tombstone
   and return a retryable persistence error.
6. Finalize leftover tombstones before scheduling startup resume.

## Common usage

Swift needs one operation after scanning or paste. `AppModel` already observes
the store, so the view renders `snapshot.remoteHostPairing` while awaiting:

```swift
func pair(_ scannedOrPastedText: String) {
    pairingTask = Task {
        do {
            let outcome = try await appModel.client.pairRemoteHost(
                code: RemotePairingCode(encoded: scannedOrPastedText)
            )
            switch outcome {
            case let .connected(connection):
                onConnected(connection.hostId)
            case let .reviewRequired(review):
                presentedReview = review
            }
        } catch {
            // Includes typed RemoteHostPairingError.cancelled after an
            // explicit cancelRemoteHostPairing request has completed rollback.
            presentedError = error
        }
    }
}

func cancelPairing() {
    guard let attemptId = appModel.snapshot?.remoteHostPairing?.attemptId else {
        return
    }
    Task {
        _ = try? await appModel.client.cancelRemoteHostPairing(
            attemptId: attemptId
        )
    }
}

// Existing AppModel subscription projects this; the view only renders it.
PairingProgressView(progress: appModel.snapshot?.remoteHostPairing)
```

Kotlin is the same shape:

```kotlin
pairingJob = scope.launch {
    when (val outcome = appModel.client.pairRemoteHost(
        RemotePairingCode(encoded = scannedOrPastedText),
    )) {
        is RemoteHostPairingOutcome.Connected -> onConnected(outcome.connection.hostId)
        is RemoteHostPairingOutcome.ReviewRequired -> presentedReview = outcome.review
    }
}

fun cancelPairing() {
    val attemptId = appModel.snapshot.value?.remoteHostPairing?.attemptId ?: return
    scope.launch {
        appModel.client.cancelRemoteHostPairing(attemptId)
    }
}

// snapshot is already a StateFlow fed by AppStore updates.
PairingProgress(progress = appModel.snapshot.value?.remoteHostPairing)
```

There is no parse button requirement, agent-loading task, connect button
choreography, credential save, remembered-server write, endpoint-key write, or
resume registration in the default caller. QR scanning, clipboard access,
camera permission, and presentation state remain native.

The explicit advanced path is still direct:

```swift
let review = try await appModel.client.inspectRemoteHostPairing(
    code: RemotePairingCode(encoded: raw)
)
let connection = try await appModel.client.commitRemoteHostPairing(
    reviewId: review.reviewId,
    decision: RemoteHostPairingDecision(
        displayName: editedName,
        selectedRuntimeIds: selectedRuntimeIds,
        replacement: replacementApproval
    )
)
```

On later launches and foreground transitions, platform code invokes the normal
app lifecycle hook only. The successful pairing resumes automatically; callers
do not call a pairing-specific resume method.

## Hidden implementation

The module absorbs:

- code representation detection, JSON/URL compatibility, validation,
  normalization, and version negotiation;
- secret redaction, review TTL/cache management, and CSPRNG IDs;
- safe-default runtime policy, beta filtering, recommendation fallback,
  capability shaping, deduplication, and selection validation;
- stable host/server identity and cosmetic name disambiguation;
- endpoint-key load/create/persist-before-bind ordering;
- Alleycat `alleycat/1`, `ALLEYCAT_*`, iroh relay/path handling, authenticated
  probe, list-agents/connect frames, wire selection, response validation, and
  graceful probe close;
- shared endpoint reuse, per-runtime streams, multiplexed session assembly,
  partial-runtime recovery, health readers, and warmup;
- pairing progress reduction, operation ownership, cancellation, per-host
  locking, and duplicate-call coalescing;
- versioned opaque envelopes, legacy-record import, atomic commit/rollback,
  revocation tombstones, and crash recovery;
- reconnect backoff, network-change hints, connection replacement, sequence
  cursor tracking, runtime reconciliation, resubscription, and hydration;
- `AppStore` progress/health projection and typed error mapping.

Swift and Kotlin retain camera/clipboard facilities, permissions, secure-storage
adapters wired at bootstrap, localization, and render-only projections.

## Dependency categories and adapters

### In-process

Parsing, normalization, identity derivation, safe-default planning, selection
validation, progress reduction, review caching, operation locks, reconnect
policy, error mapping, and `AppStore` projection are in-process. Merge them into
the module and test through its external interface. Do not create ports for pure
helpers or for the canonical store.

### Local-substitutable

Secure persistence is local-substitutable and platform-specific. Define one
internal port wired at bootstrap with operations equivalent to `read(slot)`,
`atomic_write(slot, opaque_bytes)`, `delete(slot)`, and `list(prefix)`.
Serialization and slot naming remain inside the module.

- Production adapters: iOS Keychain and Android encrypted
  preferences/keystore.
- Test adapter: an in-memory, crash-injectable store supporting atomic writes
  and secure-delete failure simulation.

This is a real internal seam: two production adapters and a test adapter vary.
It is not an argument on any pairing method, and platforms never see envelope
contents. Store one complete per-host active envelope or non-secret revocation
tombstone in one secure slot so metadata, credential, and revocation state
cannot diverge.

Monotonic time and randomness are also local-substitutable internal
dependencies: system monotonic clock/CSPRNG in production, deterministic clock
and ID source in tests. Do not expose either through the external interface.

### Remote but owned

The Alleycat host is remote but owned/controlled. Define an internal
`PairingHostPort` at the network seam. It speaks typed probe, connect, resume,
and revoke outcomes rather than JSON frames.

- Production adapter: iroh plus the Alleycat protocol.
- Test adapter: an in-memory scripted host that can vary runtime metadata,
  reject credentials, drop streams, advance sequence floors, change identity,
  and support or reject host-side revocation.

The module owns workflow, safety policy, persistence, rollback, and resume. The
adapter owns transport mechanics. A new Alleycat version or another owned host
transport can replace the adapter without widening the external interface.

### True external

There is no true-external network dependency in pairing. Camera and clipboard
facilities remain entirely outside the module; both yield an opaque input string.
OS secure stores are covered by the local-substitutable port rather than mocked
at the external seam.

## Interface tests

Construct the real module with the in-memory persistence adapter, scripted host
adapter, deterministic clock, deterministic IDs, and real `AppStore` reducer.
Call only the same five entry points exposed to Swift and Kotlin. The interface
is the test surface.

### Default-path contracts

- One valid code plus stable non-beta runtimes produces progress in semantic
  order and returns `Connected` without any platform-supplied selection.
- Legacy runtime metadata selects every available non-beta runtime, matching
  the current platform default.
- A healthy identical pairing returns `AlreadyPaired` without creating a
  second session or envelope.
- A successful return commits exactly one opaque envelope and publishes
  connected server health.
- No progress, result, error description, or log contains the token, raw relay,
  endpoint secret, or serialized envelope.
- A lagged `AppStore` subscriber receives `FullResync` and can still read the
  current/terminal typed progress from the snapshot.

### Review and security contracts

- Explicit inspect always returns `UserRequested`, creates no durable pairing,
  and opens no lasting runtime session.
- Beta-only, no-safe-default, identity-conflict, and credential-replacement
  cases return `ReviewRequired` and never auto-commit.
- Reviews contain sanitized host/runtime data but no credential or transport
  internals.
- Commit rejects empty, unknown, unavailable, expired, and stale selections.
- Replacement requires matching existing and candidate identities; a stale
  approval cannot authorize a different replacement.
- Review IDs are invalid after success, revocation, expiry, or process restart.

### Transaction and concurrency contracts

- Zero attached runtimes leaves no durable pairing and no reconnect target.
- Partial attachment commits the full desired set; automatic resume later
  retries missing runtimes.
- Persistence failure after attachment rolls back session, store health, and
  resume registration before returning an error.
- Explicit cancellation during probe performs no commit; cancellation during
  commit reaches a coherent committed or rolled-back state before the original
  call returns `Cancelled`.
- Cancelling a terminal, unknown, or stale attempt is an idempotent
  `NoActiveMatch`; cancelling the Swift/Kotlin task alone is not treated as a
  Rust cancellation request.
- Two simultaneous starts produce one active attempt and a typed
  `PairingInProgress` for the other.
- Duplicate commit for a successfully consumed review is idempotent for the
  resulting host, not a second connection.

### Resume and revocation contracts

- Cold launch resumes from the opaque envelope with no platform token, payload,
  runtime, or wire input.
- Network loss and foreground recovery use the same internal resume path and
  do not duplicate sessions.
- Resume sends the last sequence cursor and handles fresh, resumed, and
  drift-reload outcomes without caller branching.
- Replacement resources become visible only after they are ready, followed by
  authoritative resubscription/hydration.
- Revocation racing pair, commit, or resume wins after tombstone commit.
- A crash after tombstone write finalizes cleanup before startup resume.
- Secure-delete failure retains an effective tombstone and reports a retryable
  typed error.
- Repeated revoke returns `AlreadyRevoked`; unsupported remote invalidation is
  reported honestly.

Run adapter contract tests against both production secure-storage adapters for
atomic write, read-after-write, delete, device-only accessibility, and failure
mapping. Keep Alleycat frame-level tests inside the transport adapter. Once
these interface tests cover behavior, remove overlapping tests for the old
parse/list/connect/save/reconnect choreography: replace, do not layer.

## Tradeoffs and deliberate constraints

- **The common path gets maximum leverage at the cost of a larger advanced
  surface.** Five intent-level methods are more than the three-method minimal
  design, but ordinary callers learn only one. This is the right trade when QR
  scan/paste plus automatic resume dominates usage.
- **`pair` and `inspect` share work internally.** They are not separate
  implementations. The apparent duplication is interface-level clarity:
  automatic intent versus explicit review intent.
- **Transient progress lives in `AppStore`.** This slightly broadens the
  canonical snapshot, but it reuses the one cross-platform observation seam,
  survives subscriber lag, and avoids callback or operation-object lifecycle
  surfaces.
- **One pairing attempt at a time.** This matches the product UI and makes
  credential commits understandable. Bulk provisioning would require a future
  explicit module rather than silently weakening this invariant.
- **Automatic selection is opinionated.** Stable recommended runtimes connect
  with no confirmation. Beta-only and ambiguous hosts incur one review step.
  That makes the default fast without turning convenience into a security
  downgrade.
- **Inspect costs a network round trip.** Authenticated, current runtime data is
  worth the latency; cached or code-only previews could misrepresent the host.
- **Uncommitted reviews do not survive process death.** The user must scan
  again, avoiding durable storage of a bearer token before pairing succeeds.
- **Pairing success requires secure persistence.** Reachable-but-unpersistable
  hosts fail and roll back rather than producing a connection that cannot
  resume or revoke coherently.
- **There is no public manual resume.** This concentrates reconnect policy and
  prevents platform drift, but diagnostics must inspect typed server health and
  internal tracing rather than force individual transport phases from UI code.
- **Terminal progress is retained until the next attempt or restart.** This
  favors reliable observation over automatic cleanup; closed screens simply
  stop rendering it.
- **Remote revocation is only as strong as the host protocol.** Local
  tombstones are strong today. True remote invalidation later changes the
  transport adapter and status, not the pairing interface.
- **Persistence migration should be a hard cutover.** Import legacy saved
  metadata and tokens into one verified versioned envelope, then delete the old
  records. Do not maintain permanent dual read/write paths.

This design is deep because a single default call exercises validation,
authentication, selection, connection, secure commit, canonical progress, and
future automatic resume. Protocol changes, persistence migrations, runtime
policy, and reconnect repair remain local to one Rust module and its internal
adapters, while the uncommon reviewed path retains explicit control where a
safe default is impossible.
