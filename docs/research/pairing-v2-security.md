# Remora Link v2 security architecture

Status: implemented mobile contract and release guardrails.

Host source pin: `amanthanvi/alleycat@0e625bece349a2ce53b7926cac7fc6a81121ca37`

## Decision

Remora Link v2 is the only active paired-host authorization path. It uses the
`remora-link/2` ALPN and the pinned host's byte-level wire contract and golden
vectors. The retired v1 bearer path is not a fallback. A v1 invitation is
recognized only so the app can require explicit re-pairing.

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

The host adapter accepts only the pinned v2 contract. It never silently retries
`alleycat/1` after a v2 error and never imports a legacy bearer or endpoint key.

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
- missing local key material requires re-pairing and never downgrades to v1.

## Legacy migration boundary

The following historical identifiers remain only where removal would weaken a
direct-upgrade migration or break a pinned compatibility contract:

- `ALLEYCAT_*` constants and `alleycat/1` in the isolated host compatibility
  implementation;
- a private, token-zeroizing v1 invitation classifier that returns
  `LegacyRePairRequired`;
- the explicit `npx kittylitter` string used to explain an existing legacy
  installation;
- `_alleycat_seq` as a time-bounded read fallback for the exact pinned host,
  while `_remora_link_seq` is canonical;
- historical SSH-bridge crate/package identity;
- exact legacy secret namespaces used by idempotent deletion and Android backup
  exclusions.

The apps never write v1 pairing credentials. At startup they repeatedly delete
the historical stores:

- iOS generic-password service `com.alleycat.token`, all accounts;
- iOS service `com.alleycat.device_key`, account
  `__device_secret_key__`;
- Android shared preferences `alleycat_credentials`.

There is intentionally no completion marker. The purge retries when protected
data is unavailable and remains in place until direct upgrades from every
v1-writing build are outside the supported upgrade window.

## Verification gates

Every host pin or protocol change requires:

- host golden-vector parity and explicit review of the exact 40-character pin;
- deterministic binding generation;
- Rust lifecycle, replay, reconnect, revocation, attachment, and shell-cleanup
  tests;
- platform journal/key-provider and legacy-purge tests;
- iOS simulator and Android unit/debug builds;
- pairing, reconnect, terminal, revoke/forget, and stale-resume smoke tests on
  both platforms;
- a residual-identifier audit against the documented interop allowlist.

The normal local gate is:

```bash
make bindings
make rust-test
make ios-sim-fast
cd apps/android && ./gradlew :app:testDebugUnitTest :app:assembleDebug
```

The pinned host contract is documented in the fork's
[v2 wire specification](https://github.com/amanthanvi/alleycat/blob/0e625bece349a2ce53b7926cac7fc6a81121ca37/docs/remora-link-v2-wire.md)
and
[golden vectors](https://github.com/amanthanvi/alleycat/tree/0e625bece349a2ce53b7926cac7fc6a81121ca37/tests/fixtures/remora-link-v2).
