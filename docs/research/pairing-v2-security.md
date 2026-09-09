# Remora Link v2 security architecture

Status: implemented mobile contract and release guardrails.

Host source: [`services/remora-link`](../../services/remora-link/REMORA.md),
owned and reviewed atomically with the mobile client.

## Decision

Remora Link v2 is the only active paired-host authorization path. It uses the
`remora-link/2` ALPN and the host's byte-level wire contract and golden
vectors. Unsupported invitation formats fail closed and require fresh pairing.

Enrollment and routine access are deliberately different:

1. Enrollment starts from a short-lived, single-use invitation pinned to the
   host identity and requested capability ceiling.
2. The mobile installation creates a P-256 signing key through its platform
   security provider. The host stores the public key, an opaque credential ID,
   exact runtime scopes, and its authorization epoch.
3. Every privileged operation uses a fresh host challenge and a transcript-bound
   proof. The invitation never becomes a runtime credential.
4. The host confirms the device and requested runtimes before committing the
   grant. Failed, cancelled, expired, or ambiguous enrollment is recoverable
   through the durable journal without widening authority.
5. QR and paste are only encodings of the same invitation. Neither path changes
   the authorization policy.

The host adapter accepts only the reviewed v2 contract. It never retries another
protocol after a v2 error and never imports unsupported credentials.

## Implemented ownership boundary

- Shared Rust owns invitation inspection, the enrollment/revocation state
  machine, durable journal transitions, reconnect, replay/drift handling,
  runtime attachments, shell custody, and typed public results.
- Swift and Kotlin own secure-key operations, journal persistence adapters,
  ingress capture, permissions, and presentation.
- A paired terminal is opened by opaque Remora Link host ID. Raw tokens, relay
  details, and host transport objects never cross the terminal UI boundary.
- The host launches only installed, host-advertised runtimes. Mobile runtime
  selection can narrow that set but cannot widen it.
- Relay and push layers are delivery infrastructure. Durable sequenced state is
  authoritative; notifications are opaque wake hints only.

## Security invariants

- Invitation expiry and single-use consumption are enforced before a device
  grant is committed.
- Challenges expire before signing and bind host, credential, request,
  operation, authorization epoch, scopes, and both nonces.
- P-256 signatures use the exact DER transcript contract shared with the host
  vectors.
- Credential IDs are opaque identifiers, not bearer authority.
- Revocation is host-authoritative and closes active runtime and shell streams.
- Host removal, configuration replacement, long resume, and app shutdown close
  retained and claimed connections with generation fencing.
- Runtime attachments are take-once. Stale attachment claims cannot register
  after a lifecycle cleanup.
- Replay cursors advance from decoded, accepted application events rather than
  a host high-water mark or unread socket bytes.
- WebSocket replay observation begins only after the HTTP upgrade.
- State-changing requests are idempotent or receipt-bound across reconnect.
- No approval action is accepted from a lock-screen notification.

## Durable lifecycle

The mobile journal records pending enrollment, the last authoritative host
response, local credential state, granted runtimes/scopes, revocation, and
recovery intent. Compare-and-swap persistence prevents concurrent writers from
silently overwriting lifecycle transitions.

Recovery is conservative:

- a lost response is reconciled by operation ID rather than repeated as a new
  operation;
- an ambiguous enrollment cannot be treated as a fresh invitation;
- a revoked or forgotten host cannot be resurrected by a stale async result;
- long background gaps trigger authoritative reconnect/reload;
- missing local key material requires fresh pairing.

## Supported upgrade boundary

Remora 1.6.0 is the security and direct-upgrade floor on iOS and Android. When
its versioned cutover marker is absent, startup removes unsupported credentials,
signing authority, saved hosts, and protocol journals before configuring the
shared runtime. Every host must then be paired again. No migration or
compatibility guarantee applies to older state; direct upgrades are supported
from 1.6.0 onward.

## Background delivery

The authenticated `relay_enroll`, `relay_commit`, and `relay_barrier` operations
require both runtime inspection and connection scopes. The host keeps write
authority; read/manage capabilities transfer to device-only secure custody.
The same enrollment command is retried after ambiguous responses. Only a
durable device enrollment permits host transfer-secret erasure.

The host persists publication identity before relay HTTP submission and only
certifies confirmed cursors. A repair barrier binds the installation,
credential/epoch, host boot, and complete runtime session/revision vector.
Rust reads authoritative projections between matching barriers and checks
local configuration, journal generation, session and history fences before
publication. The authenticated digest and cursor are committed before relay
ACK. They record a verified observation, not a persisted transcript. Every
foreground/cold start repairs even when no newer event exists.

See [host delivery contract](../../services/remora-link/docs/background-relay.md)
and [iOS custody](ios-background-relay-custody.md). Native token caches hold OS
inputs only; they cannot assign installations or advance repair cursors.

## Verification gates

Every host source or protocol change requires:

- host golden-vector parity and review of the coordinated host/mobile diff;
- deterministic binding generation;
- Rust lifecycle, replay, reconnect, revocation, attachment, and shell-cleanup
  tests;
- platform journal and key-provider tests;
- iOS simulator and Android unit/debug builds;
- pairing, reconnect, terminal, revoke/forget, and stale-resume smoke tests on
  both platforms;
- a residual product-identity audit.

The normal local gate is:

```bash
make bindings
make rust-test
make ios-sim-fast
cd apps/android && ./gradlew :app:testDebugUnitTest :app:assembleDebug
```

The host tree carries the v2 wire specification and golden vectors. Its
`REMORA.md` records import provenance; local path dependencies keep bridge and
host changes in the same reviewable source tree.
