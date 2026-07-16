# Pairing v2 security architecture

Date: 2026-07-15
Status: research record plus implementation guide for the pinned v2 host contract

## Decision

Replace the current persistent, host-wide bearer token with a two-stage design:

1. **Enrollment uses a short-lived, single-use invitation.** The default invitation is a self-contained QR/copy payload containing the pinned host Iroh `EndpointId`, route hints, a random invite ID, and exactly 32 random secret bytes. The pinned contract caps interactive invitations at 300 seconds and unattended invitations at 60 seconds. An invitation is consumed by at most one device and never becomes a runtime credential.
2. **Routine access uses a per-host, per-installation signing credential.** The phone creates a non-exportable P-256 key in Secure Enclave or Android Keystore before enrollment. The host stores its public key, exact scopes, the client's authenticated Iroh identity, lifecycle state, and an authorization epoch. Every privileged operation proves possession against fresh host and client nonces. There is no long-lived bearer secret to copy from a QR, mobile database, or host authorization database.
3. **Interactive host confirmation is the default.** Both phone and host show the same transcript-derived short authentication string (SAS), the device label, and the requested scopes. The host commits its authoritative device record only after approval. An explicitly unattended invite may omit approval only when it is deliberately created with narrow scopes; the CLI must make that weaker policy conspicuous.
4. **QR and paste are equal encodings of the same invitation.** Neither path compresses or weakens the v2 envelope. Short locator codes, PAKE, and hosted rendezvous are explicitly deferred beyond this wire version.
5. **Pairing v2 gets a separate ALPN and authorization path.** Use `remora-link/2`; retain only the existing `alleycat/1` compatibility lane. Do not negotiate v2 inside `alleycat/1`, do not silently fall back, and do not let a v1 bearer token mint a v2 credential without a local host approval.

This keeps Iroh's authenticated encrypted transport and routing, while moving application authorization from “whoever knows the global token” to “this approved device proves possession of this scoped key.” Iroh itself documents its endpoint public key as the peer identity and provides mutual endpoint authentication; authorization remains an application responsibility ([Iroh key types](https://github.com/n0-computer/iroh/blob/v0.98.1/iroh-base/src/key.rs#L58-L70), [Iroh authenticated encryption](https://github.com/n0-computer/iroh/blob/v0.98.1/iroh/src/lib.rs#L81-L95), [accepted peer identity](https://github.com/n0-computer/iroh/blob/v0.98.1/iroh/src/endpoint/connection.rs#L1063-L1079)).

## Success criteria

Pairing v2 is successful only if all of these are true:

- A photographed or copied invitation stops working after its short expiry or first claim and grants no routine access by itself.
- A network recording cannot be replayed to enroll a second device or authenticate a later connection.
- A compromised relay cannot impersonate the pinned host, recover invitation secrets, or authorize a device. It may still observe routing metadata, delay traffic, or deny service.
- Compromise or revocation of one device does not revoke every other device and does not expose credentials accepted by another host.
- The host can narrow scopes, revoke one device record, and terminate that device's active streams without rotating every credential.
- A copied mobile application database or backup is insufficient to authenticate because the signing private key is non-exportable and device-bound.
- iOS and Android follow one Rust-owned protocol and state machine. Swift and Kotlin only create/use platform keys, obtain permissions, capture ingress, and render typed state.
- The protocol works on a direct LAN with no Remora service and works through existing Iroh relays without trusting the relay for identity or authorization.
- The Rust host and Rust mobile adapter match the same pinned byte-level contract and golden vectors without either platform redefining the protocol.
- Migration invalidates the old bearer path on a stated schedule and never creates a permanent, implicit dual authorization system.

## Threat model and trust boundaries

### Protected assets

The protected capability is not merely “connect to a host.” In the **historical
v1 baseline**, a successful legacy pairing session could enumerate agent runtimes,
restart a runtime, and attach a bidirectional stream to it; the historical
request enum and host dispatcher show those operations ([Remora request shapes](../../shared/rust-bridge/codex-mobile-client/src/alleycat.rs#L301-L320), [legacy v1 dispatch](https://github.com/dnakov/alleycat/blob/3c6dfe2c6b060864d8cb0fcae58f73a6ed1ea10f/crates/alleycat/src/host.rs#L123-L163)). A pairing credential therefore protects terminal, repository, account, and agent-session authority available through those runtimes.

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

- A live compromise of the host account that can read host memory, invoke the host Iroh identity, mutate authoritative device records, or replace the daemon can authorize itself. Cryptography cannot protect the daemon from its own execution principal.
- A fully compromised unlocked phone may be able to invoke its non-exportable key even if it cannot extract it. Revocation is the containment mechanism.
- Relay confidentiality does not hide that two endpoints communicate. Iroh's relay/address material is routing information, not an authorization credential ([`EndpointAddr` definition](https://github.com/n0-computer/iroh/blob/v0.98.1/iroh-base/src/endpoint_addr.rs#L17-L62)).
- Physical observation of an invitation before the legitimate user claims it creates a race. Default host confirmation turns that race into a visible, rejectable attempt; an unattended invite knowingly accepts this risk.

## Historical v1 baseline

The `3c6dfe2c6b060864d8cb0fcae58f73a6ed1ea10f` references in this section are
immutable evidence for the legacy `alleycat/1` bearer design. They do not
describe the pinned Remora Link host. The current host is pinned at
[`0e625bece349a2ce53b7926cac7fc6a81121ca37`](https://github.com/amanthanvi/alleycat/tree/0e625bece349a2ce53b7926cac7fc6a81121ca37)
and implements [the v2 wire contract](https://github.com/amanthanvi/alleycat/blob/0e625bece349a2ce53b7926cac7fc6a81121ca37/docs/remora-link-v2-wire.md)
with [golden vectors](https://github.com/amanthanvi/alleycat/tree/0e625bece349a2ce53b7926cac7fc6a81121ca37/tests/fixtures/remora-link-v2).

The current implementation is transport-secure but authorization-shallow:

- Remora fixes protocol version 1 and ALPN `alleycat/1`; the parsed payload contains `node_id`, `token`, optional relay, and display name ([Remora constants and payload](../../shared/rust-bridge/codex-mobile-client/src/alleycat.rs#L22-L33), [parser](../../shared/rust-bridge/codex-mobile-client/src/alleycat.rs#L434-L463)).
- Alleycat emits that same schema from one stable host Iroh public key and one host configuration token ([v1 schema](https://github.com/dnakov/alleycat/blob/3c6dfe2c6b060864d8cb0fcae58f73a6ed1ea10f/crates/alleycat/src/protocol.rs#L3-L15), [payload construction](https://github.com/dnakov/alleycat/blob/3c6dfe2c6b060864d8cb0fcae58f73a6ed1ea10f/crates/alleycat/src/host.rs#L238-L249)). The token is 32 OS-random bytes encoded as hex and stored in a mode-0600 host config file ([generation and persistence](https://github.com/dnakov/alleycat/blob/3c6dfe2c6b060864d8cb0fcae58f73a6ed1ea10f/crates/alleycat/src/config.rs#L305-L365)). High entropy prevents guessing; it does not make a copied bearer revocable per device.
- Every list, restart, and connect request repeats the same token ([Remora requests](../../shared/rust-bridge/codex-mobile-client/src/alleycat.rs#L301-L320), [connect request](../../shared/rust-bridge/codex-mobile-client/src/alleycat.rs#L524-L578)). The host extracts the mutually authenticated Iroh `remote_id`, but authorization only compares the request token with the host-global value ([connection identity](https://github.com/dnakov/alleycat/blob/3c6dfe2c6b060864d8cb0fcae58f73a6ed1ea10f/crates/alleycat/src/host.rs#L86-L105), [token check](https://github.com/dnakov/alleycat/blob/3c6dfe2c6b060864d8cb0fcae58f73a6ed1ea10f/crates/alleycat/src/host.rs#L123-L142), [comparison](https://github.com/dnakov/alleycat/blob/3c6dfe2c6b060864d8cb0fcae58f73a6ed1ea10f/crates/alleycat/src/host.rs#L271-L288)). The Iroh identity currently keys resumable session behavior, not a scoped authorization grant.
- iOS stores the bearer token per host as a `WhenUnlockedThisDeviceOnly` Keychain item, but stores the raw 32-byte app-wide Iroh secret as another exportable generic-password item ([iOS token storage](../../apps/ios/Sources/Remora/Models/AlleycatCredentialStore.swift#L21-L84), [iOS Iroh key storage](../../apps/ios/Sources/Remora/Models/AlleycatCredentialStore.swift#L86-L149)). Android stores both values in encrypted preferences ([Android credential store](../../apps/android/app/src/main/java/com/remora/android/state/AlleycatCredentialStore.kt#L6-L44)).
- The same Iroh endpoint key is loaded once and reused for every legacy v1 host ([Rust endpoint construction](../../shared/rust-bridge/codex-mobile-client/src/alleycat.rs#L608-L666), [iOS lifecycle](../../apps/ios/Sources/Remora/Models/AppRuntimeController.swift#L22-L55), [Android lifecycle](../../apps/android/app/src/main/java/com/remora/android/state/AppModel.kt#L151-L170)). It is useful as a second identity signal, but it is not currently per-host, non-exportable, or used by the host authorization decision.
- The historical v1 mobile lane pinned `3c6dfe2c...` and Iroh `0.98.1`. The
  current host pin is `0e625bece349a2ce53b7926cac7fc6a81121ca37`; it advertises
  isolated `remora-link/2` and `alleycat/1` lanes. V2 uses bounded 64 KiB
  frames, opaque credential IDs, fresh P-256 proof, host-confirmed scoped
  runtime grants, and durable idempotency. A v2 client must consume the host
  vectors and never downgrade to v1 after a v2 error.

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

## Implemented pairing v2 contract

This section is the mobile implementation guide for the exact host contract
pinned at
[`0e625bece349a2ce53b7926cac7fc6a81121ca37`](https://github.com/amanthanvi/alleycat/tree/0e625bece349a2ce53b7926cac7fc6a81121ca37).
The byte-level authority is the pinned
[v2 wire contract](https://github.com/amanthanvi/alleycat/blob/0e625bece349a2ce53b7926cac7fc6a81121ca37/docs/remora-link-v2-wire.md)
and its committed
[golden vectors](https://github.com/amanthanvi/alleycat/blob/0e625bece349a2ce53b7926cac7fc6a81121ca37/tests/fixtures/remora-link-v2/golden-vectors.json).
If this summary and those artifacts differ, the pinned artifacts win and the
mobile adapter must fail conformance until reviewed together.

V2 deliberately does not include short manual locator codes, SPAKE2, portable
authorization objects, peer administration, device-key rotation, recovery, or
delegation. Those remain separate future protocol questions and must not be
inferred from the research comparisons above.

### Transport, framing, and invitation

- Connect to the invitation's pinned Iroh `node_id` with the exact ALPN
  `remora-link/2`. `relay` and `host_name` are optional route/display
  hints, never identity or authority.
- Wait for the full Iroh handshake. Never send enrollment or a privileged
  operation as replayable early data.
- Frame each UTF-8 JSON control message as `u32be(length) || JSON`, with a
  65,536-byte frame limit enforced before allocation. Request and proof types
  reject unknown fields.
- Parse only
  `remora-link:v2:<base64url-no-pad(JSON PairingInvitation)>`, with a
  4,096-byte encoded-segment limit. The closed invitation fields are `v`,
  `node_id`, `invitation_id`, `secret`, `expires_at`,
  `max_runtime_ids`, `max_scopes`, `confirmation_mode`, and optional
  `host_name` and `relay`.
- Require version 2; a 22-character invitation ID derived from 16 random bytes;
  exactly 32 random secret bytes encoded as 43 base64url characters; at most 16
  canonical runtime IDs; and only the four closed scopes below.
- Validate the envelope timestamp's type but do not reject it against the
  phone's clock. Proof-bound host inspection is authoritative for expiry and
  policy. Interactive invitations are capped at 300 seconds; unattended
  invitations at 60 seconds.
- Treat the secret, label, and envelope policy as untrusted until inspection.
  Raw invitation secret bytes never enter logs, snapshots, errors, or crash
  metadata.

The only v2 scopes, in canonical order, are
`inspect_runtimes`, `connect_runtime`, `restart_runtime`, and
`self_revoke`. Runtime IDs are case-sensitive, 1–64 ASCII bytes from
`[A-Za-z0-9._/-]`, sorted, deduplicated, and bounded to 16. Every usable
authorization record has at least one runtime plus `connect_runtime` and
`self_revoke`. Unattended mode is limited to one runtime and exactly
`inspect_runtimes`, `connect_runtime`, and `self_revoke`.

### Request, challenge, proof, and response

One client-initiated bidirectional stream carries exactly:

```text
RequestV2 -> ResponseV2 { challenge } -> ProofV2 -> terminal ResponseV2
```

For `connect`, the same stream becomes the selected runtime byte stream after
the `session` response. Every request contains `v: 2` and a fresh 32-byte
client nonce. The closed operations are:

- `inspect_invitation`: invitation ID and secret, proposed 65-byte
  uncompressed SEC1 P-256 public key, and fresh nonce;
- `enroll`: invitation material, device label, public key, canonical runtime
  and scope selections, a durable enrollment idempotency key, and fresh nonce;
- `list_agents`: opaque credential ID and fresh nonce;
- `restart_agent`: credential ID, allowed runtime ID, durable idempotency key,
  next lifetime-monotonic command sequence, and fresh nonce;
- `connect`: credential ID, allowed runtime ID, optional resume cursor, and
  fresh nonce;
- `revoke_self`: credential ID, durable revoke idempotency key, and fresh
  nonce;
- `rollback_enrollment`: credential ID, original enrollment key, durable
  rollback key, and fresh nonce.

The host first returns a 30-second challenge with `challenge_id`,
`credential_id`, `auth_epoch`, `server_nonce`, and `expires_at`. The
client signs the exact domain-separated proof transcript with ECDSA P-256 and
SHA-256, returning only the exact challenge ID and an ASN.1 DER signature.
Neither Swift nor Kotlin constructs, normalizes, hashes, or parses protocol
transcripts.

The canonical proof, operation-payload, prospective-credential, enrollment,
policy, and SAS domains are respectively:

```text
remora-link/2/proof/v2
remora-link/2/payload/v2
remora-link/2/prospective-credential/v2
remora-link/2/enrollment/v2
remora-link/2/policy/v2
remora-link/2/sas/v2
```

All field lengths and integers use network byte order exactly as specified by
the pinned contract. The adapter must copy the pinned golden fixture and compare
raw request JSON, payload hashes, transcript bytes and hashes, prospective
credential ID, DER signatures, and SAS byte-for-byte. Reconstructing
pretty-printed JSON is not conformance.

Interactive enrollment returns `pending` until the host approves it, then an
exact retry returns `enrolled`. Both responses carry the first accepted
claim's transcript hash and six-character SAS. Before each proof the client
durably stages all confirmation-transcript inputs. After response loss it
retains every ambiguous candidate, matches the returned hash to exactly one
candidate, and recomputes both the hash and SAS before accepting either state.

The host's terminal success fields are closed: `inspection`, `pending`,
`enrolled`, `agents`, `session`, `restart`, and `revocation`. Closed
error codes are `pairing_unavailable`, `authorization_required`,
`invalid_request`, `agent_unavailable`, `outcome_unknown`, and
`internal`. Enrollment failures remain deliberately coarse.

### Durable lifecycle and retry rules

The host-local device record is authoritative. The client retains only the
opaque credential ID, non-exportable key handle, pinned host identity,
display-safe policy, and crash-recovery journal. It never receives a portable
bearer or authorization object.

The first accepted claim reserves the invitation. Exact enrollment retries use
the same enrollment idempotency key and request fingerprint but fresh nonces,
challenges, and proofs. Competing keys, endpoints, operation keys, or
fingerprints fail generically. Issued and pending invitations survive daemon
restart with their original host-enforced absolute expiry; restart never
extends or silently expires them.

Mobile persistence is two-phase:

1. Durably create the per-host key and stage the enrollment operation before
   sending.
2. Verify the returned confirmation transcript and SAS.
3. Durably persist the opaque credential record before reporting success.
4. If host enrollment committed but local secure persistence fails, retain the
   original enrollment key and retry `rollback_enrollment` with one stable
   rollback key until a durable receipt settles.

A revoke or rollback `outcome_unknown` means the host applied the mutation but
could not prove parent-directory durability. The client retains the credential
and exact mutation key and retries that same operation until `ok: true`.
Restart ambiguity is different: a prepared or pruned command at or below the
durable high watermark returns `outcome_unknown` and is never automatically
re-executed. A new operator-authorized restart must durably reserve the next
sequence and a fresh stable idempotency key.

Revocation increments the host authorization epoch and closes only that
credential's registered sessions. New proofs at an old epoch fail. Connect
admission and restart dispatch are fenced against revocation. A resume response
of `drift_reload` requires authoritative state reload rather than local patching.

### Platform key custody and boundary

iOS creates one per-host P-256 signing key in Secure Enclave when available,
with a device-only Keychain accessibility class. Android creates one per-host
P-256 key in Android Keystore and uses StrongBox opportunistically. Simulator
and emulator software fallbacks are explicitly labelled and excluded from
hardware-assurance claims. Private key material never crosses UniFFI or enters
backup.

The native callback surface is limited to key creation/public-key retrieval,
signing Rust-owned bytes exactly once with ECDSA/SHA-256, and deletion after a
settled rollback, revoke, or forget operation. Rust owns invitation parsing,
canonical ordering, transcript and hash construction, DER validation, network
state, durable operation identities, recovery, and typed display-safe results.

### Migration and no-downgrade policy

New pairing writes only Remora Link v2 credential namespaces. Legacy
`alleycat/1` input is classified as migration-only and tells the user to
re-pair. The client never imports, converts, or copies a legacy bearer token,
host key, service state, or session. It never sends a v1 token on
`remora-link/2` and never retries v1 after any v2 parse, negotiation,
identity, proof, authorization, or transport failure.

The old `npx kittylitter` string may appear only in explicit installed-service
detection and re-pair guidance. The new development bootstrap is the pinned
native Remora Link host.

### Implementation placement

- Put the private wire adapter, exact fixture, lifecycle journal, reconnect
  policy, and terminal transport in
  `shared/rust-bridge/codex-mobile-client/`.
- Expose direct typed operations through `AppClient`; expose observation-only,
  display-safe lifecycle state through the Rust store.
- Keep Swift and Kotlin to QR/camera/paste UI, secure-key adapters, platform
  permissions, and rendering.
- Preserve one shared Rust behavior for iOS and Android. Platform code must not
  parse wire strings, infer statuses, or patch canonical state after RPC
  success.
- Keep the legacy v1 adapter isolated for read, classify, re-pair, and delete
  only. No new v1 credential writes or automatic fallback remain.

### Acceptance and release gates

- Copy the exact pinned golden fixture with source SHA attribution and assert
  every JSON document, domain, payload hash, transcript byte string and hash,
  prospective credential ID, DER signature, and SAS byte-for-byte.
- Reject oversized frames/envelopes, padded or malformed base64url, unknown
  fields/versions/scopes, noncanonical arrays, duplicate runtime IDs, invalid
  SEC1 keys, malformed DER, stale challenges, identity changes, and any v2-to-v1
  fallback.
- Fault-inject process death and response loss around every enrollment,
  confirmation, secure-store, revoke, rollback, restart, and journal boundary.
  Exact retries converge without duplicate enrollment or mutation.
- Prove a copied mobile database cannot authenticate without the platform key;
  two hosts use independent key handles; deleting one pairing leaves the other.
- Verify runtime listing, restart sequence behavior, connect/resume,
  `drift_reload`, terminal byte streaming, revocation closure, and forget vs
  revoke semantics on both platforms.
- Run the pinned host conformance suite plus Rust tests, regenerated UniFFI
  bindings, iOS simulator tests, Android JVM/instrumented tests, and interactive
  QR and paste smoke tests before release.
- Scan logs, crash captures, snapshots, errors, and screenshots for invitation
  secrets, signatures, credential internals, and disallowed legacy branding.

### Deferred research outside v2

Short locator codes, PAKE/SPAKE2, hosted rendezvous, peer administration,
device-key rotation, recovery secrets, attestation policy, delegated
authorization, and a true Node host are not implemented by this wire version.
The earlier comparison sections explain why some may merit later experiments;
none is an adapter requirement or a fallback. Each future addition requires a
new explicit protocol version or compatible extension, primary-source review,
cross-implementation vectors, crash/replay testing, and a separate threat-model
update.

The accepted product defaults for this version are host-confirmed interactive
pairing, paste and QR as equal first-class ingress, opaque device authorization,
no notification approvals, no harness installation, and no security downgrade.

## Primary source register

### Existing Remora and legacy v1 behavior

- [Remora v1 protocol, payload, parser, and Iroh connection](../../shared/rust-bridge/codex-mobile-client/src/alleycat.rs#L22-L33)
- [Remora iOS credential storage](../../apps/ios/Sources/Remora/Models/AlleycatCredentialStore.swift#L21-L149)
- [Remora Android credential storage](../../apps/android/app/src/main/java/com/remora/android/state/AlleycatCredentialStore.kt#L6-L44)
- [Legacy v1 protocol schema](https://github.com/dnakov/alleycat/blob/3c6dfe2c6b060864d8cb0fcae58f73a6ed1ea10f/crates/alleycat/src/protocol.rs#L3-L15)
- [Legacy v1 token validation and pair payload](https://github.com/dnakov/alleycat/blob/3c6dfe2c6b060864d8cb0fcae58f73a6ed1ea10f/crates/alleycat/src/host.rs#L86-L105)
- [Legacy v1 token persistence](https://github.com/dnakov/alleycat/blob/3c6dfe2c6b060864d8cb0fcae58f73a6ed1ea10f/crates/alleycat/src/config.rs#L305-L365)

### Transport and cryptographic protocols

- [Iroh 0.98.1 key types](https://github.com/n0-computer/iroh/blob/v0.98.1/iroh-base/src/key.rs#L58-L70)
- [Iroh 0.98.1 authenticated transport overview](https://github.com/n0-computer/iroh/blob/v0.98.1/iroh/src/lib.rs#L81-L95)
- [Iroh 0.98.1 accepted remote identity](https://github.com/n0-computer/iroh/blob/v0.98.1/iroh/src/endpoint/connection.rs#L1063-L1079)
- [RFC 9382: SPAKE2](https://www.rfc-editor.org/rfc/rfc9382.html)
- [RFC 9807: OPAQUE](https://www.rfc-editor.org/rfc/rfc9807.html)
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

Ship **high-entropy QR/copy enrollment → local host confirmation → opaque credential ID plus a per-host non-exportable P-256 key**. Each operation proves possession with the pinned v2 DER ECDSA transcript; the host-local device record is the sole authority. Keep Iroh 0.98.1, retain the explicit `remora-link/2` lane, make revocation stateful and immediate, recover local-first, and require re-pairing for v1 migration.

If a future protocol adds a typed short code, evaluate **locator + bilateral SAS + host confirmation** before any headless mode. Add **SPAKE2** only if that separate feature is important enough to fund a reviewed RFC 9382 implementation and conformance program. None of these ideas is part of v2 or a fallback from it.
