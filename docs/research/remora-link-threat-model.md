# Remora Link threat model

Status: release-blocking security model for the implemented v2 mobile client
and co-owned host contract.

## Security objective

Only an explicitly enrolled mobile installation may exercise the exact runtime
and terminal capabilities granted by a Remora Link host. Delivery routes,
relays, notifications, harnesses, and saved UI state must
not create or widen authority.

## Trust boundaries

| Boundary | Required property |
| --- | --- |
| Mobile UI/platform adapters → shared Rust | Native code supplies key operations, persistence, permissions, and intent; Rust owns protocol, lifecycle, replay, and reconciliation policy. |
| Mobile → Remora Link | Pinned host identity, `remora-link/2`, opaque credential ID, fresh P-256 proof, exact authorization epoch and scope checks, bounded frames. |
| Mobile → SSH host | Verified host key, authenticated user, encrypted channel, and Rust-owned bridge/runtime policy. |
| Remora Link → harness | Installed-runtime allowlist, typed runtime ID, constrained launch policy, and no mobile-supplied executable or bypass flags. |
| Client/host → relay | Distinct read/manage/write capabilities; opaque encrypted wake markers contain no conversation data. Relay metadata never creates host authority. |
| Relay/host → APNs or FCM | Opaque, expiring wake hints only; authenticated reconciliation before displaying content or enabling action. |
| Host grant store → active streams | Durable revocation/epoch state closes all runtime and shell streams for the affected host/device. |

## Protected assets

- host identity and device authorization records;
- platform signing keys and opaque credential IDs;
- SSH credentials and host-key pins;
- repository contents, agent sessions, transcripts, tool results, and terminal
  streams;
- runtime launch/restart authority;
- approval challenges and decisions;
- relay routing identifiers and durable event cursors.

## Attacker model

In scope:

- passive or active local-network observers;
- a malicious or compromised Iroh/owned relay;
- copied QR images, clipboard contents, backups, or application data;
- replay, duplication, reordering, response loss, crashes, and network changes;
- a lost or revoked mobile device;
- malicious or compromised harness processes;
- stale async UI work and restored navigation state;
- a dependency or host binary that differs from the reviewed source.

Explicit limits:

- a host-account compromise capable of replacing Remora Link or mutating its
  grant store can grant itself authority;
- a fully compromised unlocked phone may invoke its non-exportable key;
- relays and push providers can observe delivery metadata and deny service;
- this model does not claim anonymity or protection from compromised platform
  kernels.

## Primary threats and controls

| Threat | Control | Verification |
| --- | --- | --- |
| Invitation theft or replay | Short expiry, single-use consumption, pinned host identity, host confirmation, durable operation IDs. | Golden vectors plus duplicate/expired/ambiguous enrollment tests. |
| Portable bearer reuse | V2 has no client-carried authority object; routine access requires a fresh proof from the enrolled key. | Platform key-provider and proof transcript tests. |
| Relay impersonation or tampering | Iroh peer authentication and end-to-end encrypted QUIC to the pinned endpoint. | Direct/relayed interop and wrong-host tests. |
| Scope confusion | Host-authoritative runtime set and authorization epoch are included in proof and lifecycle state. | Narrowed-grant and wrong-runtime tests. |
| Revoked stream survives | Host-scoped live connection registry, generation fencing, take-once attachment custody, and cleanup on revoke/forget/removal/shutdown. | Deterministic cleanup and stale-claim tests. |
| Reconnect skips events | Cursor advances only for decoded application events; drift forces authoritative reload. | Replay, fragmented-frame, WebSocket-upgrade, and drift tests. |
| Duplicate mutation after response loss | Durable idempotency/receipt semantics and operation-specific retry rules. | Lost-response lifecycle tests. |
| Wrong-machine terminal | Opaque host-ID backend, exact preferred-host fail-closed behavior, versioned restoration token, and stale-open fencing. | iOS/Android controller and route tests plus smoke tests. |
| Notification approval replay | Push is an opaque wake hint; lock-screen payloads cannot approve, launch, grant, or revoke. | Payload-shape tests and interactive inspection. |
| Forged freshness or premature ACK | Host-certified publication cursor and equal authenticated session vectors around Rust-owned repair; durable local generation/cursor commit precedes ACK. | Protocol, repair-fence, and journal ordering regressions. |
| Restored relay journal | Authenticated journal plus independent platform secure-store high-water anchor, excluded from application backup. | Native CAS/rollback tests; physical restore remains a release gate. |
| Arbitrary harness execution | Host advertises and launches only configured installed runtimes; mobile cannot provide a path or arguments. | Host compatibility matrix and negative launch tests. |
| Unsupported invitation downgrade | Unsupported formats fail closed; v2 never retries a different protocol. | Malformed-invitation and downgrade-negative tests. |

## Terminal-specific controls

Paired shells use the same v2 host authority as coding runtimes. A shell
attachment is claimed exactly once, registered by host and generation, and
closed on session close, revoke, forget, long resume, configuration replacement,
server removal, or shutdown. Explicit close is cancellation-safe: transport
closure does not depend on receiving a `shell/kill` acknowledgement.

Native UI controllers generation-fence asynchronous opens. A completion from an
older request is immediately closed and cannot overwrite the current target.
An exact server-specific route never falls back to a different paired or SSH
host.

## SSH bridge boundary

SSH bridge persistence uses a neutral typed field:

- `None`: not a bridge record;
- `Some([])`: bridge and probe all supported runtimes;
- `Some(kinds)`: bridge restricted to the selected runtimes.

Saved records use the current Remora-owned schema. Records from unsupported
versions are not migrated into active host authority.

## Upgrade floor

Remora 1.6.0 is the security and direct-upgrade floor on iOS and Android.
Before Remora Link can initialize, a missing 1.6 cutover marker triggers a
fail-closed reset of pairing credentials, signing authority, journals, and
saved records. Fresh host pairing is required. Security review and testing do
not assume compatibility with older state, and direct upgrades are supported
from 1.6.0 onward.

## Release gates

- coordinated host/mobile source review and golden-vector parity;
- Rust full suite, iOS fast simulator build, Android unit tests and debug
  assemble;
- interactive pairing/reconnect/terminal/revoke smoke tests on both platforms;
- no secrets, prompts, paths, commands, or approval data in push payloads;
- residual product naming is Remora-owned;
- branch CI green before merge.

The reviewed host source lives in `services/remora-link/`; `REMORA.md` records
its import provenance. Bridge dependencies use that tree rather than a mutable
Cargo cache or independently unpublished host commit.
