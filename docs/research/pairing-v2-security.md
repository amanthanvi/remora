# Pairing v2 security architecture

Date: 2026-07-15
Status: research and protocol recommendation; no production changes are authorized by this document

## Decision

Replace the current persistent, host-wide bearer token with a two-stage design:

1. **Enrollment uses a short-lived, single-use invitation.** The default invitation is a self-contained QR/copy payload containing the pinned host Iroh `EndpointId`, route hints, a random invite ID, and at least 128 bits of random secret. It expires after 5–10 minutes, is consumed by at most one device, and never becomes a runtime credential.
2. **Routine access uses a per-host, per-installation signing credential.** The phone creates a non-exportable P-256 key in Secure Enclave or Android Keystore before enrollment. The host stores its public key, exact scopes, the client's authenticated Iroh identity, lifecycle state, and an authorization epoch. Every new connection proves possession against a fresh server nonce. There is no long-lived bearer secret to copy from a QR, mobile database, or host grant database.
3. **Interactive host confirmation is the default.** Both phone and host show the same transcript-derived short authentication string (SAS), the device label, and the requested scopes. The host commits the grant only after approval. An explicitly unattended invite may omit approval only when it is deliberately created with narrow scopes; the CLI must make that weaker policy conspicuous.
4. **Manual codes are a separate ingress, not a compressed QR.** On the same LAN, mDNS/NSD supplies the route and host identity. An 8-character base-20 code identifies the pending invitation. The immediately shippable flow treats this code only as a locator and requires bilateral SAS comparison plus host confirmation. A future headless flow may use RFC 9382 SPAKE2, but only after a reviewed implementation, independent test vectors, and interop testing are available. OPAQUE is not justified for ephemeral host-generated invitations.
5. **Pairing v2 gets a separate ALPN and authorization path.** Use `alleycat/2` while the Alleycat compatibility name is still required. Do not negotiate v2 inside `alleycat/1`, do not silently fall back, and do not let a v1 bearer token mint a v2 credential without a local host approval.

This keeps Iroh's authenticated encrypted transport and routing, while moving application authorization from “whoever knows the global token” to “this approved device proves possession of this scoped key.” Iroh itself documents its endpoint public key as the peer identity and provides mutual endpoint authentication; authorization remains an application responsibility ([Iroh key types](https://github.com/n0-computer/iroh/blob/v0.98.1/iroh-base/src/key.rs#L58-L70), [Iroh authenticated encryption](https://github.com/n0-computer/iroh/blob/v0.98.1/iroh/src/lib.rs#L81-L95), [accepted peer identity](https://github.com/n0-computer/iroh/blob/v0.98.1/iroh/src/endpoint/connection.rs#L1063-L1079)).

## Success criteria

Pairing v2 is successful only if all of these are true:

- A photographed or copied invitation stops working after its short expiry or first claim and grants no routine access by itself.
- A network recording cannot be replayed to enroll a second device or authenticate a later connection.
- A compromised relay cannot impersonate the pinned host, recover a manual code offline, or authorize a device. It may still observe routing metadata, delay traffic, or deny service.
- Compromise or revocation of one device does not revoke every other device and does not expose credentials accepted by another host.
- The host can narrow scopes, revoke one grant, and terminate that grant's active streams without rotating every credential.
- A copied mobile application database or backup is insufficient to authenticate because the signing private key is non-exportable and device-bound.
- iOS and Android follow one Rust-owned protocol and state machine. Swift and Kotlin only create/use platform keys, obtain permissions, capture ingress, and render typed state.
- The protocol works on a direct LAN with no Remora service and works through existing Iroh relays without trusting the relay for identity or authorization.
- A Rust host uses the same canonical protocol implementation as the Rust mobile core. A future Node host can interoperate from the same byte-level test vectors without redefining the protocol.
- Migration invalidates the old bearer path on a stated schedule and never creates a permanent, implicit dual authorization system.

## Threat model and trust boundaries

### Protected assets

The protected capability is not merely “connect to a host.” A successful Alleycat connection can enumerate agent runtimes, restart a runtime, and attach a bidirectional stream to it; the current request enum and host dispatcher show those operations ([Remora request shapes](../../shared/rust-bridge/codex-mobile-client/src/alleycat.rs#L301-L320), [Alleycat host dispatch](https://github.com/dnakov/alleycat/blob/3c6dfe2c6b060864d8cb0fcae58f73a6ed1ea10f/crates/alleycat/src/host.rs#L123-L163)). A pairing credential therefore protects terminal, repository, account, and agent-session authority available through those runtimes.

### In-scope attackers

- A passive or active observer on the local network.
- A malicious or compromised Iroh relay.
- A person who obtains a QR screenshot, clipboard history item, short code, or old app backup.
- A remote attacker who guesses manual codes and distributes guesses across addresses.
- A previously approved device that is later lost, stolen, or intentionally revoked.
- A process that copies app storage but cannot extract or invoke a platform hardware key.
- Replay, reordering, duplicate delivery, client crash, host crash, and response loss at every enrollment transition.
- A malicious discovery/rendezvous service that substitutes route or host metadata.

### Explicit limits

- A live compromise of the host account that can read host memory, invoke the host signing/Iroh key, or replace the daemon can authorize itself. Cryptography cannot protect the daemon from its own execution principal.
- A fully compromised unlocked phone may be able to invoke its non-exportable key even if it cannot extract it. Revocation is the containment mechanism.
- Relay confidentiality does not hide that two endpoints communicate. Iroh's relay/address material is routing information, not an authorization credential ([`EndpointAddr` definition](https://github.com/n0-computer/iroh/blob/v0.98.1/iroh-base/src/endpoint_addr.rs#L17-L62)).
- Physical observation of an invitation before the legitimate user claims it creates a race. Default host confirmation turns that race into a visible, rejectable attempt; an unattended invite knowingly accepts this risk.

## Current v1 baseline

The current implementation is transport-secure but authorization-shallow:

- Remora fixes protocol version 1 and ALPN `alleycat/1`; the parsed payload contains `node_id`, `token`, optional relay, and display name ([Remora constants and payload](../../shared/rust-bridge/codex-mobile-client/src/alleycat.rs#L22-L33), [parser](../../shared/rust-bridge/codex-mobile-client/src/alleycat.rs#L434-L463)).
- Alleycat emits that same schema from one stable host Iroh public key and one host configuration token ([v1 schema](https://github.com/dnakov/alleycat/blob/3c6dfe2c6b060864d8cb0fcae58f73a6ed1ea10f/crates/alleycat/src/protocol.rs#L3-L15), [payload construction](https://github.com/dnakov/alleycat/blob/3c6dfe2c6b060864d8cb0fcae58f73a6ed1ea10f/crates/alleycat/src/host.rs#L238-L249)). The token is 32 OS-random bytes encoded as hex and stored in a mode-0600 host config file ([generation and persistence](https://github.com/dnakov/alleycat/blob/3c6dfe2c6b060864d8cb0fcae58f73a6ed1ea10f/crates/alleycat/src/config.rs#L305-L365)). High entropy prevents guessing; it does not make a copied bearer revocable per device.
- Every list, restart, and connect request repeats the same token ([Remora requests](../../shared/rust-bridge/codex-mobile-client/src/alleycat.rs#L301-L320), [connect request](../../shared/rust-bridge/codex-mobile-client/src/alleycat.rs#L524-L578)). The host extracts the mutually authenticated Iroh `remote_id`, but authorization only compares the request token with the host-global value ([connection identity](https://github.com/dnakov/alleycat/blob/3c6dfe2c6b060864d8cb0fcae58f73a6ed1ea10f/crates/alleycat/src/host.rs#L86-L105), [token check](https://github.com/dnakov/alleycat/blob/3c6dfe2c6b060864d8cb0fcae58f73a6ed1ea10f/crates/alleycat/src/host.rs#L123-L142), [comparison](https://github.com/dnakov/alleycat/blob/3c6dfe2c6b060864d8cb0fcae58f73a6ed1ea10f/crates/alleycat/src/host.rs#L271-L288)). The Iroh identity currently keys resumable session behavior, not a scoped authorization grant.
- iOS stores the bearer token per host as a `WhenUnlockedThisDeviceOnly` Keychain item, but stores the raw 32-byte app-wide Iroh secret as another exportable generic-password item ([iOS token storage](../../apps/ios/Sources/Remora/Models/AlleycatCredentialStore.swift#L21-L84), [iOS Iroh key storage](../../apps/ios/Sources/Remora/Models/AlleycatCredentialStore.swift#L86-L149)). Android stores both values in encrypted preferences ([Android credential store](../../apps/android/app/src/main/java/com/remora/android/state/AlleycatCredentialStore.kt#L6-L44)).
- The same Iroh endpoint key is loaded once and reused for every Alleycat host ([Rust endpoint construction](../../shared/rust-bridge/codex-mobile-client/src/alleycat.rs#L608-L666), [iOS lifecycle](../../apps/ios/Sources/Remora/Models/AppRuntimeController.swift#L22-L55), [Android lifecycle](../../apps/android/app/src/main/java/com/remora/android/state/AppModel.kt#L151-L170)). It is useful as a second identity signal, but it is not currently per-host, non-exportable, or used by the host authorization decision.
- Remora pins Alleycat commit `3c6dfe2c...` and Iroh `0.98.1` ([workspace dependencies](../../shared/rust-bridge/Cargo.toml#L28-L31), [mobile Iroh dependency](../../shared/rust-bridge/codex-mobile-client/Cargo.toml#L11-L16)). Alleycat at that commit accepts only `alleycat/1` ([host endpoint configuration](https://github.com/dnakov/alleycat/blob/3c6dfe2c6b060864d8cb0fcae58f73a6ed1ea10f/crates/alleycat/src/host.rs#L18-L43)). A v2 deployment therefore requires coordinated host and mobile changes, not just a new QR parser.

Consequences of v1 are direct: any valid QR remains a host-wide access bearer until rotation; the host cannot identify which scanned copy is using it; rotation is global; active sessions need not disappear merely because the persisted token changes; and a token holder can repeatedly create new Iroh connections. These are application authorization properties, not failures of Iroh's encrypted channel.

## Auto-research loop

The investigation used an explicit hypothesis → source/spec check → adversarial evaluation → score loop. The final design combines the hypotheses that survived rather than forcing one encoding to serve every ingress.

### H0 — Keep the v1 QR and only shorten its lifetime

**Result: rejected.** Expiry reduces exposure but leaves one bearer capable of routine access and still cannot identify, scope, or revoke devices independently. “Single-use token exchanged for another bearer token” only moves the copyable secret; it does not create proof of possession.

### H1 — Authorize the existing Iroh `remote_id`

**Result: useful defense, insufficient sole credential.** Iroh already proves control of the endpoint private key during the transport handshake, and Alleycat already receives that identity. A host allowlist would immediately eliminate repeated bearer-token authorization. However, Remora's current key is raw, exportable, app-wide, and shared across hosts. Making it the only credential preserves backup-copy risk, correlates the device across hosts, and couples authorization migration to Iroh endpoint lifecycle. Pairing v2 should bind the observed Iroh ID into a grant but use a per-host hardware-backed application key as the durable PoP credential.

### H2 — High-entropy single-use QR, then hardware-backed device PoP

**Result: accepted as the default.** A random 128–256-bit invitation can safely be presented inside the host-authenticated Iroh channel without password stretching or a PAKE. The host stores only a hash of the invitation secret, consumes it atomically, and persists only the device public key and grant. A copied QR still creates a first-use race, so interactive host confirmation remains the default.

### H3 — Low-entropy code over Iroh TLS

**Result: accepted only as locator + confirmation.** Sending a weak code inside authenticated TLS stops a passive relay from reading it but does not stop an active online guesser. Treating the code as a route/nameplate and requiring bilateral SAS plus local host approval prevents a correct guess from silently enrolling. The code must not be persisted as a runtime credential or used directly as an HMAC key.

This is analogous to the separation in Magic Wormhole between a short public “nameplate” used to locate a mailbox and a PAKE exchange used to authenticate the peers ([client protocol](https://github.com/magic-wormhole/magic-wormhole-protocols/blob/master/client-protocol.md), [server protocol](https://github.com/magic-wormhole/magic-wormhole-protocols/blob/master/server-protocol.md), [security analysis](https://github.com/magic-wormhole/magic-wormhole-protocols/blob/master/security.md)). Remora should not add a public rendezvous service merely to reproduce that routing model; local discovery already supplies the route for offline pairing.

### H4 — SPAKE2 bound to the manual code

**Result: cryptographically accepted, implementation-gated.** RFC 9382 SPAKE2 prevents a passive transcript from becoming an offline dictionary oracle; each active execution permits an online guess. It requires explicit identities/context and a key-confirmation step before the resulting key authorizes enrollment ([RFC 9382 protocol](https://www.rfc-editor.org/rfc/rfc9382.html#section-3), [identities and context](https://www.rfc-editor.org/rfc/rfc9382.html#section-3.2), [key confirmation](https://www.rfc-editor.org/rfc/rfc9382.html#section-4)). It does not solve discovery or rate limiting.

The obvious Rust crate is not ready to become Remora's trust root without more work: its own primary README says it has never received an independent third-party audit, and it documents compatibility with a pre-RFC Ed25519 SPAKE2 implementation rather than claiming RFC 9382 wire compatibility ([RustCrypto SPAKE2 README](https://github.com/RustCrypto/PAKEs/blob/master/spake2/README.md)). Therefore the release gate is a reviewed RFC-compatible implementation, the RFC vectors plus Remora-specific transcript vectors, fuzzing, and Rust-host/mobile interop. Until then, H3 is safer operationally because its security assumption is visible human approval rather than an unaudited PAKE implementation.

### H5 — OPAQUE bound to the manual code

**Result: rejected for this use case.** OPAQUE is an augmented PAKE built around a registration record and later password-authenticated login. It is excellent when a server must store a long-lived password verifier and resist precomputation/server-file compromise, but a Remora host creates an ephemeral random invitation and is itself the approving authority. OPAQUE adds registration, OPRF configuration, envelope handling, more messages, and a substantially larger interop surface without removing discovery, online rate limiting, or the need to approve scopes ([RFC 9807 overview and protocol phases](https://www.rfc-editor.org/rfc/rfc9807.html#section-1), [registration](https://www.rfc-editor.org/rfc/rfc9807.html#section-5.1), [online AKE](https://www.rfc-editor.org/rfc/rfc9807.html#section-5.3)).

The primary Rust `opaque-ke` implementation has an audit history, but its current README advertises a pre-release 4.1 line and Rust 1.87; its earlier audit applied to older releases ([opaque-ke README and audit statement](https://github.com/facebook/opaque-ke#readme)). That is not a reason to distrust OPAQUE; it is evidence that dependency selection and version-specific review would itself be a project. There is no corresponding benefit over SPAKE2 for an ephemeral shared code.

### H6 — Encode a high-entropy secret as words

**Result: accepted as recovery/manual fallback, not the default.** A sequence of uniformly selected words is just an encoding of random bits, so it avoids offline guessing if it carries enough entropy. With a 2,048-word list, each random word carries 11 bits. Six words carry 66 bits; twelve independent words carry 132 encoded bits. In the familiar BIP-39 12-word construction, 128 bits are entropy and 4 bits are checksum ([BIP-39 specification](https://github.com/bitcoin/bips/blob/master/bip-0039.mediawiki)). Twelve words are secure but burdensome to read and type; shorter sequences trade away the margin that made a PAKE unnecessary.

If Remora adds words, it must use a product-specific, versioned list and checksum—not present the string as a cryptocurrency seed phrase, and not inherit BIP-39's Unicode normalization semantics by accident. A full copy/paste URI is more reliable for remote manual transfer.

## Candidate scorecard

Scores are 1–5, where 5 is best. The weighted total uses security 30%, reliability 15%, performance 10%, offline operation 10%, user-input usability 15%, maintainability 10%, and v1 migration 10%. Scores assume all stated mitigations; in particular, the first row assumes bilateral SAS and host approval. These are decision scores, not cryptographic measurements.

| Candidate | Security | Reliability | Performance | Offline | User input | Maintainability | Migration | Weighted |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Short code as locator + bilateral SAS + host confirmation | 4 | 5 | 5 | 5 | 5 | 5 | 4 | **4.60** |
| RFC 9382 SPAKE2 + confirmation + rate limit | 5 | 3 | 5 | 5 | 5 | 2 | 4 | **4.35** |
| RFC 9807 OPAQUE | 5 | 2 | 4 | 5 | 5 | 1 | 3 | **3.95** |
| High-entropy 12-word secret | 5 | 5 | 5 | 5 | 2 | 4 | 5 | **4.55** |
| High-entropy QR/copy invite + device PoP | 5 | 5 | 5 | 5 | 5 | 5 | 5 | **5.00** |

The table explains the hybrid decision:

- QR/copy should be the default because it can carry both route identity and a strong one-use secret without typing.
- Locator + confirmation is the best manual flow that can ship without taking a new cryptographic implementation dependency.
- SPAKE2 is the best manual-code primitive when unattended/headless enrollment is a hard requirement, but its current implementation-readiness score blocks immediate adoption.
- OPAQUE solves a different password-storage problem.
- Words are a sound break-glass encoding but poor primary UX.

## Required comparison of the three manual-code models

### 1. Low-entropy code as relay locator + host confirmation

**Model.** The code selects a pending invite/nameplate. The route comes from local discovery or a rendezvous lookup. The client connects to an Iroh host, submits the locator, public device key, and nonces, and both screens display a SAS. The host operator confirms the intended phone, scopes, and SAS before commit.

**Guessing.** A passive observer cannot read the code when it is submitted inside the fully authenticated Iroh channel. An attacker can still make online guesses. The host must cap attempts per invite, globally, and per source; persist the failure count; introduce randomized delay; and terminally lock the invite after the cap. Responses must not distinguish “unknown,” “expired,” “already claimed,” and “wrong code.” Host confirmation, not the weak code, is the final authorization factor.

**Relay compromise.** A relay can map the locator to the wrong host, block the intended mapping, enumerate active nameplates, or race a claim. It cannot make the intended host display the same pending transcript unless it reaches that host. Bilateral SAS comparison detects substitution; one-sided “approve device name” does not. A public relay therefore remains a DoS/metadata boundary, never an authorization boundary.

**Offline operation.** This model is strongest on the same LAN: mDNS/NSD provides candidates, and the host Iroh identity plus direct addresses provide routing. A short locator alone cannot encode or recover a 32-byte Iroh host identity. Remote use requires either a hosted rendezvous service or a separately supplied full host address/ID. Remora should use the full QR/copy URI remotely instead of creating a cloud dependency.

**Interop and maintenance.** It uses ordinary Iroh transport, hashes, nonces, and a host UI/CLI confirmation state machine. Rust mobile and Rust host share all logic. A Node host can implement the same typed messages and confirmation without a PAKE library. Its weakness is operational: unattended pairing is intentionally unavailable.

### 2. PAKE bound to the code

**Model.** The host and client run SPAKE2 with the human code as the password. The SPAKE2 transcript is bound to protocol version, ALPN, host Iroh ID, client Iroh ID, invite ID, both nonces, requested scopes, and device public key. Both sides perform explicit key confirmation. Only then can the derived key authenticate the enrollment statement.

**Guessing.** A recorded SPAKE2 exchange does not enable an offline dictionary test. An active attacker gets one password guess per execution, so a 34.6-bit human code is only as strong as the host's durable attempt cap. PAKE does not turn a six-digit code into a 128-bit secret. It prevents passive/offline amplification and malicious-relay transcript testing ([RFC 9382 security considerations](https://www.rfc-editor.org/rfc/rfc9382.html#section-7)).

**Relay compromise.** A compromised relay sees PAKE messages and can drop, reorder, or substitute them. It cannot derive the session key without a correct online guess. Identity/context binding and key confirmation prevent the relay from splicing a successful exchange into a different host, device key, or grant request. The relay can still exhaust attempts or deny service, so host-side rate limiting and confirmation policy remain necessary.

**Offline operation.** SPAKE2 can run over a direct LAN Iroh connection. It still needs a route. It does not replace mDNS/NSD, a full host identity, or a rendezvous/nameplate service.

**Interop and maintenance.** The deployed host path is a Rust daemon packaged behind npm, not a JavaScript protocol implementation; upstream `kittylitter` is a small Rust binary delegating to Alleycat ([wrapper manifest](https://github.com/dnakov/litter/blob/main/services/kittylitter/Cargo.toml), [distribution workspace](https://github.com/dnakov/litter/blob/main/dist-workspace.toml)). The preferred implementation is therefore one audited Rust pairing core used by host and mobile. A true Node host has no built-in SPAKE2 API in Node's official crypto surface ([Node crypto API](https://nodejs.org/api/crypto.html)); it would need the same Rust core through N-API/Wasm or a separately reviewed implementation. That materially increases release and conformance burden.

### 3. High-entropy secret encoded as human words

**Model.** The host generates at least 128 bits randomly and encodes them as a checksummed word sequence. The sequence is a one-use enrollment bearer transported inside the pinned Iroh connection. It can replace the QR secret, but not the route/host identity unless the user also supplies those fields.

**Guessing.** A properly generated 128-bit value makes online and offline brute force irrelevant. No PAKE is needed merely to protect it from guessing. Exposure is physical/operational instead: screenshots, shoulder surfing, clipboard history, logs, voice transcription, and mistyping.

**Relay compromise.** If the secret is submitted only after an authenticated Iroh handshake, the relay cannot read it. If the rendezvous service itself receives the words, they become a relay-visible bearer and the design loses its advantage. The secret must be end-to-end input, never a server-side lookup token.

**Offline operation.** It works directly over LAN and is the strongest no-QR, no-hosted-service fallback. It remains cumbersome: roughly twelve words for a 128-bit-plus-checksum construction, in addition to any route/host identity needed.

**Interop and maintenance.** Random generation and checking are simple in Rust and Node. The maintenance risk is the codec: exact wordlist version, checksum, case, Unicode normalization, typo behavior, and localization must be frozen in test vectors. Do not reuse wallet vocabulary or APIs in a way that trains users to enter cryptocurrency recovery phrases into Remora.

## Entropy and payload experiment

A disposable arithmetic experiment was run to check the order of magnitude of manual-code proposals. It created no repository files.

| Encoding | Search space | Entropy | Five uniformly random online attempts |
| --- | ---: | ---: | ---: |
| 6 decimal digits | 1,000,000 | 19.93 bits | `5 × 10^-6` (about `2^-17.6`) |
| 8 decimal digits | 100,000,000 | 26.58 bits | `5 × 10^-8` (about `2^-24.3`) |
| 8 characters, unambiguous base 20 | 25.6 billion | 34.58 bits | `1.95 × 10^-10` (about `2^-32.3`) |
| 10 characters, unambiguous base 20 | 10.24 trillion | 43.22 bits | `4.88 × 10^-13` (about `2^-40.9`) |
| 6 random words from 2,048 | `2048^6` | 66 bits | not operationally guessable |
| BIP-39-style 12 words | 128-bit entropy + checksum | 128 bits | not operationally guessable |

RFC 8628 uses the same governing calculation for device user codes and gives an example where an 8-character base-20 code with five attempts provides about 2^-32 guessing probability ([RFC 8628 user-code entropy](https://www.rfc-editor.org/rfc/rfc8628.html#section-6.1)). That is an appropriate minimum for a five-attempt, short-lived manual invite; it is not a substitute for host confirmation or PAKE.

A representative compact v1 JSON payload measured 232 bytes and a representative v2 JSON envelope measured 312 bytes. This is only an encoding-size sanity check, not a QR scan-quality or QR-version proof. Final QR encoding should be measured with actual host IDs, route hints, error correction, camera distance, and both platform decoders.

## Proposed pairing v2 protocol

### Cryptographic choices

| Purpose | Choice | Reason |
| --- | --- | --- |
| Host transport identity | Existing Iroh Ed25519 `EndpointId` | Already authenticated by Iroh and already present in v1 payloads |
| Invitation secret | 32 random bytes; 16 bytes is the minimum | Simple, non-guessable, compact, no password KDF required |
| Invitation ID / redemption ID | Independent 16 random bytes each | Non-enumerable lookup and idempotency keys |
| Device credential | P-256 ECDSA / ES256, one key per host per app installation | Native non-exportable support on Apple Secure Enclave and Android Keystore; direct Rust and Node support |
| Transcript representation | Versioned deterministic binary structure; COSE Sign1/ES256 for signatures | Avoids JSON canonicalization and algorithm ambiguity; COSE defines protected signature structure ([RFC 9052](https://www.rfc-editor.org/rfc/rfc9052.html), [ES256](https://www.rfc-editor.org/rfc/rfc9053.html#section-2.1)) |
| Freshness | 32-byte server nonce plus 16-byte client nonce | Makes every proof unique and replay-detectable |
| SAS | Truncated hash/HMAC of the complete confirmed transcript | Human check covers host, device key, transport identity, and exact scopes |

The DPoP standard is not adopted as a wire protocol, but its core pattern is relevant: a public key is bound to an authorization grant, and each proof covers method/context plus a fresh value so a copied proof cannot be replayed elsewhere ([RFC 9449 proof contents and replay protection](https://www.rfc-editor.org/rfc/rfc9449.html#section-4.2)).

### Invitation envelope

The camera/clipboard input is opaque to Swift and Kotlin. Rust decodes a versioned envelope equivalent to:

```text
InviteV2 {
  protocol = 2
  host_id = Iroh EndpointId
  route_hints = [relay URL and/or direct socket addresses]
  invite_id = 128 random bits
  invite_secret = 256 random bits
  expires_at = informational host timestamp
  offered_scopes = typed bounded set
  confirmation = required | explicit_unattended
  host_label = untrusted display string
}
```

Security rules:

- `host_id`, not a relay URL or display name, is the identity pinned in `Endpoint::connect` ([Iroh endpoint connect API](https://github.com/n0-computer/iroh/blob/v0.98.1/iroh/src/endpoint.rs#L958-L999)).
- Route hints are mutable performance hints. A relay substitution may cause failure but must not change the pinned host.
- The host enforces expiry from its own record. The timestamp in the envelope is only UI information.
- The host stores `SHA-256(invite_secret)` and never logs the raw secret. The client sends the secret only inside the fully authenticated Iroh channel. Hash storage is safe because the secret is high entropy; it would not be safe for a short code.
- The invitation record precommits the maximum scopes. A claim may request a subset, never an expansion.
- QR rendering and copy output are ephemeral. The host stops showing them immediately after claim, expiry, cancellation, or daemon restart.
- A route-only manual code is a different type and parser. It must not be accepted in `invite_secret`.

### Enrollment sequence

```text
Phone                                  Host
  |                                     |
  |-- Iroh connect: pinned host_id ---->|
  |<==== full mutual TLS handshake =====>|
  |<-- PairChallenge(server_nonce,       |
  |                  policy, host_id) ---|
  |                                     |
  | generate per-host P-256 key          |
  |-- PairClaim(invite_id, secret,       |
  |     redemption_id, client_nonce,     |
  |     client_iroh_id, public_key,      |
  |     requested_scopes, device_info) ->|
  |                                     | atomically ISSUED -> CLAIMED
  |-- ES256 proof over full transcript ->|
  |<-- pending confirmation + SAS ------>|
  | phone confirms SAS                  | host confirms SAS/device/scopes
  |                                     | atomically CLAIMED -> COMMITTED
  |<-- credential_id, granted_scopes,    |
  |    auth_epoch, receipt --------------|
```

`client_iroh_id` above is the identity the host obtains from the authenticated connection, not an untrusted client assertion. If it is serialized in the client message, the host must require exact equality with `connection.remote_id()`.

Enrollment must wait for the full handshake. QUIC 0-RTT data is replayable by design, so a single-use state transition must never be accepted in early data ([QUIC 0-RTT replay considerations](https://www.rfc-editor.org/rfc/rfc9001.html#section-9.2)). Remora's current client awaits `Endpoint::connect` before opening a stream, which is the safe baseline to preserve ([current connection path](../../shared/rust-bridge/codex-mobile-client/src/alleycat.rs#L668-L692)).

### Signed enrollment transcript

The exact deterministic transcript must include, in order and with length-delimited fields:

```text
domain_separator = "remora pairing v2 enrollment"
wire_version
ALPN
host_iroh_endpoint_id
client_iroh_endpoint_id
invite_id
redemption_id
server_nonce
client_nonce
device_public_key
requested_scopes
host_policy_digest
confirmation_mode
```

The client signs this before approval to prove it controls the key being enrolled. The host's confirmation UI is rendered from the same parsed transcript, not from separate client-supplied labels. Unknown fields, duplicate fields, non-canonical values, out-of-order scopes, and algorithm substitution are rejected before signing or verification.

The SAS derives from the same transcript. In the QR flow it may be `Truncate(HMAC(invite_secret, transcript_hash))`; in the SPAKE2 flow it derives from the confirmed PAKE key; in locator-only mode it derives from the transcript hash plus both connection nonces and is meaningful only when compared on the intended host and phone. These are distinct labeled derivations so a value from one mode cannot confirm another.

### Durable invite state machine

```text
ISSUED
  -> CLAIMED(device_key_hash, client_iroh_id, redemption_id, transcript_hash)
  -> AWAITING_CONFIRMATION
  -> COMMITTED(credential_id)

ISSUED/CLAIMED/AWAITING_CONFIRMATION
  -> EXPIRED | CANCELLED | LOCKED | REJECTED
```

Required transition behavior:

- State changes are transactional and durable before a success response is sent.
- The first valid claim reserves the invite. A different device key, Iroh identity, or redemption ID receives one generic terminal rejection and cannot replace the claim.
- A retry with the same redemption ID, key, identity, and transcript is idempotent and returns the existing status or committed receipt. This handles a lost response without allowing a second enrollment.
- A rejected/locked claim does not return to `ISSUED`; the operator creates a fresh invite. This avoids ambiguous races after a stolen invitation.
- Process restart expires all uncommitted invitations unless their durable clock/expiry semantics are explicitly proven. “Expire on restart” is simpler and safer than extending a code after wall-clock rollback.
- Invitation failure counters and terminal states are persisted. A daemon restart must not reset an online-guess budget.
- Raw invite secrets are zeroized after verification and never enter tracing, crash reports, analytics, pairing snapshots, or user-visible errors.

### Manual locator mode

For same-LAN pairing:

1. The host advertises a v2 pairing service over existing platform discovery with its Iroh identity and direct route hints.
2. The user types an 8-character code from an unambiguous 20-symbol alphabet. Formatting separators and case are display-only.
3. Rust considers discovered hosts without revealing which accepted the code. It caps aggregate probes so one entry cannot become a distributed scan.
4. A matching host reserves the invite, receives the device key and transcript, and displays the same SAS, device information, and requested scopes as the phone.
5. Both sides confirm; the host commits the normal v2 grant.

If a hosted locator is later introduced, it receives only an opaque, rate-limited nameplate and mailbox messages. It never receives the invite secret, device private key, grant, recovery code, or authority to choose the confirmed host identity. That service requires its own abuse, privacy, enumeration, retention, and availability review.

### Optional SPAKE2 mode

SPAKE2 may replace the weak-code submission in step 3, but not the rest of the state machine. Its release requirements are:

- RFC 9382 group, serialization, identities, context, and key derivation—not an incompatible draft dialect.
- Distinct roles and identities; no symmetric “same identity on both sides” shortcut.
- Context contains protocol/ALPN, host and client Iroh IDs, invite ID, nonces, device public key, and exact scope request.
- Explicit, role-separated key-confirmation MACs before any enrollment proof is accepted.
- The PAKE result is fed through a labeled KDF into confirmation and enrollment keys; the raw shared value is not reused.
- Durable per-invite and aggregate attempt limits remain in force.
- Published golden vectors for Rust↔Rust and Rust↔Node/Wasm, malformed-element tests, reflection/role-confusion tests, transcript mutation tests, and fuzzing.
- Independent review of the concrete dependency/version and its integration.

Host confirmation can remain as defense in depth. If product requirements remove it for headless use, the security claim becomes explicitly “at most five online guesses against an 8-base20 code during the invite lifetime,” not “128-bit authentication.”

## Routine device authentication

### Grant record

The host persists one record per paired installation:

```text
DeviceGrant {
  credential_id
  device_public_key_p256
  bound_client_iroh_id
  scopes
  state = active | suspended | revoked
  auth_epoch
  created_at
  approved_by
  last_seen_at
  assurance_metadata
  revocation_reason
}
```

`credential_id` is a random public lookup identifier, not a secret. `assurance_metadata` may say whether the key was reported hardware-backed; it must not contain attestation data unless an optional attestation feature is deliberately implemented.

### Connection proof

After each full Iroh handshake:

1. The client sends `credential_id` and a fresh client nonce.
2. The host loads an active grant, verifies that the connection's `remote_id()` matches the bound Iroh ID, and returns a fresh 32-byte server nonce plus current `auth_epoch`.
3. The client signs a COSE Sign1 payload containing a domain separator, protocol/ALPN, host Iroh ID, observed client Iroh ID, credential ID, both nonces, authorization epoch, and requested connection purpose.
4. The host verifies ES256, atomically consumes the challenge, rechecks grant state/scopes, and attaches the principal to that connection.
5. Each new connection repeats the proof. Privileged operations recheck grant state/epoch; they do not trust a process-lifetime cache after revocation.

The invitation secret is absent. A database thief gets public keys and scopes, not authenticators. A recording gets a signature over a consumed server nonce, not a reusable token. Binding the proof to both Iroh IDs also prevents using a copied application proof over a different transport identity.

### Scope model

Use closed, typed Rust enums or capability records, not arbitrary strings or prefix matching. At minimum separate:

- discover/list approved runtimes;
- connect to specific runtime IDs or a bounded runtime set;
- restart a runtime;
- inspect/manage devices;
- create invitations;
- recover/rebind a device.

The default phone grant should have only the runtime access the user selected. It should not inherit device administration, invite minting, or recovery authority. An invite precommits a maximum scope; a device may request less; the host may approve less; no stage may widen it.

## Platform key custody

### iOS

Create a `SecKey` P-256 signing key with Secure Enclave and a `ThisDeviceOnly` accessibility class. The private key must never be exported into Rust or serialized into an app record. Swift receives canonical bytes from Rust and returns the signature. Apple documents Secure Enclave key creation and device-only Keychain accessibility in its primary platform guidance ([Secure Enclave keys](https://developer.apple.com/documentation/security/protecting-keys-with-the-secure-enclave), [`WhenUnlockedThisDeviceOnly`](https://developer.apple.com/documentation/security/ksecattraccessiblewhenunlockedthisdeviceonly), [Keychain accessibility](https://developer.apple.com/documentation/security/restricting-keychain-item-accessibility)).

Do not require Face ID/Touch ID for every reconnect unless that UX is an explicit product decision; such a flag would break unattended foreground/background recovery. Simulator/debug builds may use a clearly marked software key and must not report hardware assurance.

### Android

Generate an EC P-256 signing key in Android Keystore. Request hardware backing and use StrongBox opportunistically where available, but do not make StrongBox a compatibility requirement. Query and record the actual security level. Exclude key aliases and paired-host authorization metadata from backup/restore so a restore cannot create a half-valid credential. Android's official Keystore guidance states that key material can remain non-exportable and may be bound to secure hardware ([Android Keystore](https://developer.android.com/privacy-and-security/keystore)).

As on iOS, normal reconnect should not require per-signature user authentication unless the product deliberately chooses that tradeoff. Attestation is optional evidence, not baseline authorization; Android's verification model can require remote trust roots and revocation checks ([Android key attestation](https://developer.android.com/privacy-and-security/security-key-attestation)).

### UniFFI boundary

Rust owns every byte that is signed and verified. Platform adapters expose only operations equivalent to:

```text
create_per_host_key(host_id) -> public_key + opaque_key_handle
sign(opaque_key_handle, canonical_bytes) -> signature
delete_key(opaque_key_handle)
key_assurance(opaque_key_handle) -> metadata
```

The opaque handle is local platform metadata, not a credential transferable to the host. Swift/Kotlin must not construct transcripts, parse scopes, choose algorithms, or normalize ECDSA signatures. The Rust adapter converts Apple's/Android's DER ECDSA output to the fixed `r || s` form required by COSE and rejects non-canonical signatures.

## Revocation, rotation, and recovery

### Revocation

Revocation is stateful and immediate:

- Local host CLI/UI can revoke any grant without the device.
- A device may self-revoke by signing a fresh revocation challenge.
- A separately scoped admin device may request revocation, preferably with host confirmation for another admin.
- Revocation changes `state`, increments the authorization epoch, writes an append-only/tombstone audit event, rejects all new proofs, and closes all live Iroh connections and agent streams belonging to that credential.
- The host rechecks state/epoch at privileged boundaries so a stream opened before revocation cannot create new privileged sub-operations afterward.
- Revoked credential IDs are not immediately reusable or deleted. A tombstone prevents stale receipts or restored databases from resurrecting them.

RFC 7009's token endpoint is not the proposed protocol, but its security principle applies: revocation may invalidate related authorization material and must be idempotent ([RFC 7009](https://www.rfc-editor.org/rfc/rfc7009.html)).

### Routine key rotation

Device-key rotation is an authenticated grant update, not re-pairing with the old invite:

1. Active old key proves possession on a fresh challenge.
2. New hardware key proves possession and signs a transcript binding old credential, new public key, host/client Iroh IDs, scopes, nonces, and next epoch.
3. Host atomically activates the new key and revokes/tombstones the old one.
4. If either proof or commit response is lost, the same rotation ID is idempotent.

A device that cannot use its old key does not get this path; it must use recovery or pair again.

### Recovery

Recovery must not recreate the global bearer:

- **Primary recovery:** local host administration creates and approves a fresh invite. This works offline and has the smallest trust boundary.
- **Optional peer-admin recovery:** another explicitly admin-scoped device proposes a new device grant; the host still confirms for high-value scopes.
- **Optional break-glass code:** a separately generated 128-bit-or-stronger, single-use recovery secret, shown once and stored only as a hash. It is not the short pairing code and cannot authorize routine runtime access directly. It opens a pending recovery that still binds and proves a new device key.
- **Client loss/reinstall:** because platform keys are device-only and excluded from backup, re-pair. Never export the private key to make restore seamless.
- **Host identity loss:** the Iroh host key and authorization database form one trust unit. Back them up encrypted together or treat a lost host key as a new host requiring re-pairing. Restoring only one side must fail closed.

NIST's current authenticator guidance distinguishes binding a new authenticator and recovery from ordinary authentication; recovery should not silently weaken the normal authenticator assurance ([NIST SP 800-63B](https://pages.nist.gov/800-63-4/sp800-63b.html)).

## Relay, discovery, and offline properties

Iroh's relay is useful for reachability but unnecessary as a trust anchor. The client already builds an `EndpointAddr` from the QR's host ID and relay hint, then asks Iroh to connect using the pinned ALPN ([current Remora dial path](../../shared/rust-bridge/codex-mobile-client/src/alleycat.rs#L668-L692)). Pairing v2 preserves that model:

- **Remote QR/copy:** self-contained host ID + relay/address hints + high-entropy invite.
- **Offline LAN QR:** host ID + direct address hints; relay omitted.
- **Offline LAN manual:** mDNS/NSD returns candidates and direct addresses; manual code selects a pending invite; bilateral SAS identifies the intended terminal.
- **Remote manual without QR/copy:** not supported by a short code alone unless Remora adds a rendezvous service. The safe fallback is a full copyable URI or high-entropy words plus separately supplied host identity/route.

A malicious relay can deny service and observe metadata. It cannot impersonate the QR-pinned Iroh host, alter the encrypted transcript, validate a SPAKE2 password offline, or produce the enrolled device's hardware signature. Relay hints must never be included as authenticated identity in a way that prevents legitimate route updates; they may be included in a diagnostics hash, but host identity and authorization fields are the security boundary.

## Rust, Alleycat, Iroh, and Node interoperability

### Current compatible path

Keep Iroh 0.98.1 for pairing v2's first implementation and make the host accept both explicit ALPNs during the migration window. Iroh 1.0 introduced breaking changes and has already moved beyond the pinned release; upgrading Iroh while replacing authorization would multiply the test matrix without improving the v2 security model ([Iroh v1.0.0 release](https://github.com/n0-computer/iroh/releases/tag/v1.0.0), [current v1.0.2 release](https://github.com/n0-computer/iroh/releases/tag/v1.0.2)). Upgrade Iroh separately after v2 is proven.

The host endpoint currently advertises a one-item ALPN list, so dual-stack migration needs the host builder and dispatcher to advertise/branch on both `alleycat/2` and `alleycat/1` ([current host ALPN](https://github.com/dnakov/alleycat/blob/3c6dfe2c6b060864d8cb0fcae58f73a6ed1ea10f/crates/alleycat/src/host.rs#L18-L43)). The v2 dispatcher must never pass a v2 connection into the v1 token validator or vice versa.

### Shared Rust core

The canonical invitation codec, state-machine types, transcript encoder, hash/KDF labels, COSE verification, scope evaluator, and test vectors should live in one small Rust pairing crate/module consumable by:

- `codex-mobile-client`, exposed through the existing handwritten `AppClient` and `AppStore` patterns;
- the Rust Alleycat/Remora Link host daemon; and
- a future N-API/Wasm wrapper if a true Node host is ever required.

Do not generate a second Swift/Kotlin protocol implementation. Do not let platform code parse wire strings. Hardware signing remains a narrow callback because the private key cannot cross into Rust.

### True Node host compatibility

The baseline v2 protocol deliberately uses common primitives: SHA-256, random bytes, P-256 ECDSA, deterministic byte encoding, and full-handshake Iroh transport. Node's official crypto API supports EC keys, ECDSA verification, and IEEE-P1363 signature encoding ([Node `crypto.verify`](https://nodejs.org/api/crypto.html#cryptoverifyalgorithm-data-key-signature), [ECDSA `dsaEncoding`](https://nodejs.org/api/crypto.html#cryptosignsignprivatekey-outputencoding)). A Node host still needs an Iroh-compatible transport binding or a different explicitly versioned transport, but it does not need a bespoke credential primitive.

SPAKE2 is the exception: Node has no built-in API. If SPAKE2 ships, bind the reviewed Rust implementation rather than maintaining independent Rust and TypeScript cryptographic code. OPAQUE has Rust/Wasm options, but that ecosystem fact does not change the protocol-fit rejection above.

### Required conformance corpus

Publish fixtures for:

- every invitation encoding and malformed-field rejection;
- canonical transcript bytes for enrollment, confirmation, connection auth, rotation, revocation, and recovery;
- DER ↔ fixed-width `r || s` ECDSA conversion, including leading-zero edge cases;
- valid and invalid ES256 COSE Sign1 messages;
- scope ordering and unknown-scope rejection;
- SPAKE2 messages/keys/confirmation if that mode is enabled;
- Rust-mobile ↔ Rust-host and Rust ↔ Node verification in both directions.

No implementation is conformant merely because it can parse the happy-path JSON.

## Migration from v1 plaintext-token QR

Use a time-bounded dual-stack cutover:

### Phase 0 — host capability and storage

- Add the v2 authorization database, durable invite state, revocation/session index, and local confirmation UI/CLI.
- Advertise both ALPNs, but keep their dispatch and persistence completely separate.
- Create v2 invites only when the host has durable atomic state and can close a revoked grant's live sessions.

### Phase 1 — v2 clients and v2-by-default output

- Mobile accepts v2 invites and creates hardware-backed per-host keys.
- Host CLI/QR output emits only v2 by default. Legacy output requires an explicit diagnostic flag and states that it is a global bearer.
- A client that sees a v2 envelope connects only with `alleycat/2`. Failure must not trigger an automatic `alleycat/1` attempt.

### Phase 2 — existing paired devices

The safest default is **re-pair**. An optional convenience migration may exist only as this explicit local workflow:

1. Legacy token authenticates a temporary v1 connection.
2. Client generates and proves a v2 device key.
3. Host shows the device's Iroh ID, key fingerprint, scopes, and migration SAS.
4. Local operator approves.
5. Host creates a v2 grant; client verifies the receipt; client deletes the v1 token.

Never auto-upgrade a v1 token holder. A leaked old QR would otherwise convert itself from a global bearer into an apparently legitimate durable device. Existing platform stores already expose token deletion operations, so migration must wire deletion into the successful commit path rather than merely stop reading the item ([iOS deletion API](../../apps/ios/Sources/Remora/Models/AlleycatCredentialStore.swift#L79-L84), [Android deletion API](../../apps/android/app/src/main/java/com/remora/android/state/AlleycatCredentialStore.kt#L16-L18)).

### Phase 3 — retire v1

- Publish a removal release/date and expose telemetry locally as counts only: active v2 grants, last v1 connection, and whether v1 remains enabled. Do not log tokens or invite material.
- Rotate the v1 global token after the migration window, close all v1 sessions, and disable `alleycat/1` by default.
- A short emergency compatibility extension must be explicit, visible, and have a new removal date. Do not leave permanent silent fallback.
- Eventually remove v1 payload parsing and token storage after supported clients no longer need migration.

Rollback is protocol-level: operators may temporarily re-enable the v1 listener with a newly rotated token. A v2 client must still never downgrade an invite automatically. This preserves a clear security boundary even during operational rollback.

## Implementation placement

This report does not change code, but the repository architecture constrains the eventual implementation:

- Put invitation decoding, pairing state, transcript construction, reconciliation, and status normalization in `shared/rust-bridge/codex-mobile-client/` or a small Rust crate shared with the host.
- Expose direct pairing operations on `AppClient`; expose progress/snapshots through `AppStore`. Do not add handwritten orchestration in views or another broad `AppStore` command surface.
- Keep Swift and Kotlin to QR/camera/clipboard UI, secure-key adapters, permission prompts, and rendering.
- Keep host grant/invite state in the host daemon and make commit/revoke transactions durable before responding.
- Add a v2 host protocol beside v1; do not patch the upstream Codex or Ghostty submodules.
- Preserve the current Iroh host key so existing QR host pinning remains intelligible, but do not preserve the global token as a v2 credential.

## Acceptance tests and release gates

### Enrollment and replay

- Two clients claim one invite concurrently: exactly one `(credential_id, device_key, client_iroh_id)` can commit.
- A duplicate claim with the same redemption ID and transcript returns the same state/receipt; any changed key, identity, scope, or nonce fails.
- Replay every captured enrollment frame on a new connection, old connection, restarted host, and different host: no new grant.
- Expired, cancelled, rejected, locked, used, and unknown invite IDs produce indistinguishable remote errors.
- Crash the host before/after every durable transition and before/after each response; recovery never returns a consumed invite to another device and never loses a committed grant.
- Daemon restart does not reset manual-code attempt counts and expires pending invitations according to the declared policy.
- Enrollment in QUIC 0-RTT/early data is rejected even if a transport API later exposes it.

### Host identity and relay

- Substitute relay URL, direct addresses, display name, and discovery records while preserving the legitimate host ID: connection either reaches the pinned host or fails.
- Substitute a different host ID: phone and intended host cannot show a matching accepted transcript/SAS.
- A malicious locator service cannot turn its own endpoint into the approved host through one-sided confirmation.
- Direct LAN pairing succeeds with relay and external DNS unavailable.

### Manual code and PAKE

- Wrong-code failures consume the durable budget; fifth failure locks the invite; restart does not restore attempts.
- Per-source controls do not replace an aggregate cap and cannot be bypassed by source rotation.
- Locator-only mode cannot commit without both confirmations.
- If SPAKE2 is enabled: passive transcript corpus cannot validate dictionary candidates; every malformed group element fails; role reflection, unknown-key-share, identity/context mutation, missing key confirmation, and cross-invite replay fail.

### Device PoP and scope

- A copied app database with credential ID, public metadata, and host routes cannot authenticate without the platform key.
- A captured valid signature fails against a new nonce, host, client Iroh ID, ALPN, epoch, scope, or connection purpose.
- The same mobile installation has independent P-256 keys for two hosts; deleting one does not remove the other.
- Grant scope cannot exceed invite maximum, client request, or host approval; unknown scope values fail closed.
- Device key proof is required before host approval and on every new connection.
- iOS and physical Android tests confirm the private key cannot be exported. Simulator/emulator software keys are labeled and excluded from assurance claims.

### Revocation and recovery

- Revoking one device immediately rejects new proofs and closes every live connection/agent stream for that credential without affecting other devices.
- A connection authorized under an old epoch cannot perform a new privileged operation after epoch increment.
- Restoring an old grant database cannot resurrect a tombstoned credential under the current host state.
- Device-key rotation is atomic and idempotent; at no point are two keys silently active for one credential.
- Recovery secrets are single-use, at least 128 bits, hash-stored, and cannot directly open a runtime session.
- Restoring only a host key or only the authorization database fails closed; reinstalling a phone requires re-pairing unless an explicit recovery flow runs.

### Interop and quality

- Rust host ↔ iOS hardware signer and Rust host ↔ Android hardware signer pass the same conformance vectors.
- Rust and Node verify each other's ES256/COSE fixtures, including all DER/P1363 edge cases.
- Invitation and protocol decoders are fuzzed for size bounds, duplicate fields, integer overflow, Unicode display confusion, and unknown versions.
- Logs, crash captures, snapshots, and errors are scanned to prove they omit raw invite secrets, manual codes, recovery codes, signatures, and legacy tokens.
- Benchmarks show one hardware signature and one verification per new connection; no per-frame signing is added to the agent data path.
- Security review signs off on the exact PAKE library/version before any confirmation-free manual flow is enabled.

## Residual risks and explicit product choices

- **Unattended invitation policy:** a stolen unattended QR can win the first-use race. The default should require host confirmation; unattended mode is an explicit weaker option with narrow scopes and a shorter TTL.
- **Hardware-key UX:** requiring user presence for every signature is stronger against an unlocked-device process but conflicts with automatic reconnect. The baseline uses non-exportability without a biometric prompt; a high-assurance opt-in can add user presence.
- **Per-host Iroh identity:** v2 binds the current app-wide Iroh identity plus a per-host P-256 credential. Generating a distinct Iroh endpoint per host would improve unlinkability but is a larger transport/lifecycle change and is not required to eliminate bearer authorization.
- **Host database rollback:** tombstones and epochs help only if the current authorization state is itself protected from rollback. A future host master key/monotonic checkpoint can harden backups, but same-account daemon compromise remains out of scope.
- **Attestation:** Apple/Android attestation may classify assurance but adds online vendor trust, privacy, root rotation, and revocation dependencies. It must remain optional so offline pairing and ordinary devices continue to work.
- **PAKE readiness:** the protocol recommendation does not authorize shipping an unaudited or draft-incompatible SPAKE2 crate. Locator + bilateral confirmation is the release-safe manual baseline.

## Primary source register

### Existing Remora and Alleycat behavior

- [Remora v1 protocol, payload, parser, and Iroh connection](../../shared/rust-bridge/codex-mobile-client/src/alleycat.rs#L22-L33)
- [Remora iOS credential storage](../../apps/ios/Sources/Remora/Models/AlleycatCredentialStore.swift#L21-L149)
- [Remora Android credential storage](../../apps/android/app/src/main/java/com/remora/android/state/AlleycatCredentialStore.kt#L6-L44)
- [Alleycat v1 protocol schema](https://github.com/dnakov/alleycat/blob/3c6dfe2c6b060864d8cb0fcae58f73a6ed1ea10f/crates/alleycat/src/protocol.rs#L3-L15)
- [Alleycat host identity, token validation, and pair payload](https://github.com/dnakov/alleycat/blob/3c6dfe2c6b060864d8cb0fcae58f73a6ed1ea10f/crates/alleycat/src/host.rs#L86-L105)
- [Alleycat token generation and atomic mode-0600 persistence](https://github.com/dnakov/alleycat/blob/3c6dfe2c6b060864d8cb0fcae58f73a6ed1ea10f/crates/alleycat/src/config.rs#L305-L365)

### Transport and cryptographic protocols

- [Iroh 0.98.1 key types](https://github.com/n0-computer/iroh/blob/v0.98.1/iroh-base/src/key.rs#L58-L70)
- [Iroh 0.98.1 authenticated transport overview](https://github.com/n0-computer/iroh/blob/v0.98.1/iroh/src/lib.rs#L81-L95)
- [Iroh 0.98.1 accepted remote identity](https://github.com/n0-computer/iroh/blob/v0.98.1/iroh/src/endpoint/connection.rs#L1063-L1079)
- [RFC 9382: SPAKE2](https://www.rfc-editor.org/rfc/rfc9382.html)
- [RFC 9807: OPAQUE](https://www.rfc-editor.org/rfc/rfc9807.html)
- [RFC 9052: COSE structures](https://www.rfc-editor.org/rfc/rfc9052.html)
- [RFC 9053: COSE algorithms](https://www.rfc-editor.org/rfc/rfc9053.html)
- [RFC 9449: proof-of-possession pattern](https://www.rfc-editor.org/rfc/rfc9449.html)
- [RFC 9001 §9.2: QUIC 0-RTT replay](https://www.rfc-editor.org/rfc/rfc9001.html#section-9.2)
- [RFC 8628: device code/user code security](https://www.rfc-editor.org/rfc/rfc8628.html#section-6.1)
- [Magic Wormhole protocol specifications](https://github.com/magic-wormhole/magic-wormhole-protocols)
- [BIP-39 entropy/checksum/word encoding](https://github.com/bitcoin/bips/blob/master/bip-0039.mediawiki)

### Platform key custody and runtime interoperability

- [Apple: protecting keys with Secure Enclave](https://developer.apple.com/documentation/security/protecting-keys-with-the-secure-enclave)
- [Apple: Keychain item accessibility](https://developer.apple.com/documentation/security/restricting-keychain-item-accessibility)
- [Android: Keystore](https://developer.android.com/privacy-and-security/keystore)
- [Android: key attestation](https://developer.android.com/privacy-and-security/security-key-attestation)
- [Node.js crypto API](https://nodejs.org/api/crypto.html)
- [RustCrypto SPAKE2 implementation and audit warning](https://github.com/RustCrypto/PAKEs/blob/master/spake2/README.md)
- [opaque-ke implementation and audit history](https://github.com/facebook/opaque-ke#readme)
- [NIST SP 800-63B authenticator binding and recovery](https://pages.nist.gov/800-63-4/sp800-63b.html)

## Final recommendation

Ship **high-entropy QR/copy enrollment → local host confirmation → per-host hardware-backed ES256 device grant**, bound to the authenticated Iroh client and host identities. Keep Iroh 0.98.1 and add an explicit `alleycat/2` lane for the first rollout. Make revocation stateful and immediate, recovery local-first, and v1 migration approval-gated.

For a typed short code, ship **locator + bilateral SAS + host confirmation** first. Add **SPAKE2** only if headless pairing is important enough to fund a reviewed RFC 9382 implementation and conformance program. Do not use **OPAQUE** for ephemeral invites, do not make a short code a bearer, and do not turn a **12-word high-entropy fallback** into the default experience.
