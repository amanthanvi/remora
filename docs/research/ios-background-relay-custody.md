# iOS Background Relay Custody

`NativeRelayJournalBackend` persists the opaque Rust journal in the app group's
`RemoraRelay/v1` directory. It reuses the pairing journal's cross-process lock,
revision CAS, bounded envelope, atomic rename, and file/directory synchronization.
The relay directory is explicitly excluded from backup before any write; failure
to establish that attribute makes storage unavailable. The pairing namespace and
its existing policy are unchanged.

`NativeRelaySecretBackend` stores each secret and its revision in one Data
Protection Keychain item. `SecItemAdd` provides create-if-absent. `SecItemUpdate`
matches the exact previous revision metadata and atomically replaces that
metadata and the value. A tombstone preserves its revision but contains no secret.
The reserved journal anchor cannot be overwritten or deleted through unfenced
callbacks. Rust alone interprets the anchor and authenticates the journal.

## Restore Boundary

The storage class is `kSecAttrAccessibleWhenPasscodeSetThisDeviceOnly`, with
synchronization disabled. Apple states that this class is not backed up, synced,
or included in escrow keybags. Removing or resetting the passcode discards its
class keys. Other `ThisDeviceOnly` classes can be backed up with device-bound
protection; that does not establish same-device restore resistance.

Source: [Apple Platform Security: Keychain data protection](https://support.apple.com/guide/security/keychain-data-protection-secb0694df1a/web),
published December 19, 2024; retrieved September 9, 2026.

This policy deliberately requires a passcode and an unlocked device. Locked
relay callbacks must defer and retry after unlock; there is no weaker class or
in-memory fallback. A lost custody item is not permission to recreate an
integrity key over an existing journal. Rust configuration must remain fail-closed.

This provides exclusion from the supported native backup/restore path, not a
hardware monotonic counter or protection against arbitrary privileged snapshots
of the entire Keychain. Simulator tests prove API-level atomicity and selected
attributes, not Secure Enclave enforcement, physical lock behavior, passcode
removal, or a real device restore. Those device tests remain a release gate.

## Plaintext Lifetime

Writes borrow the generated secret carrier directly during synchronous Keychain
calls and wipe the carrier on every callback exit. Reads copy directly from
Security's returned CFData into the generated wipeable carrier inside an
autorelease pool, without another Data or Array copy. No adapter cache or log
retains plaintext. Security framework and operating-system internal allocations
remain platform-owned; this implementation does not claim to erase those copies.

## Verification

Production wiring initializes after the security cutover without requiring a
SwiftUI scene. The same cutover clears orphaned relay custody and its journal
alongside paired-host authority. APNs hints pass the strict native background
envelope decoder, then Rust owns installation validation, deduplication,
authoritative repair, and acknowledgements. Native code never persists a relay
cursor or a single installation identity.

The latest OS token or explicit tombstone is held separately in device-only
custody. Its CAS revision orders native inputs per APNs environment; it is not
the relay registration generation. The input is replayed after authenticated
pairing and reconciliation, covering tokens received before any host exists and
later multi-host enrollment. Foreground and protected-data recovery re-request
the current token from APNs and retry Rust reconciliation even without a hint.

A native callback has one 27-second budget covering cold initialization,
configuration and ingest. Timeout cancels the local operation without waiting
for an uncooperative FFI future. It does not assert that remote work failed or
roll back Rust state. Shared generation and authoritative-state fences remain
responsible for late repair. Background-only launches do not warm keyboards or
wait for a splash scene.

`BackgroundRelayNativeStoresTests` exercises real Simulator Keychain creation and
revision races across independent adapter instances, stale writes, tombstone
retention, malformed metadata, weak-policy rejection, reserved-anchor guards,
callback buffer wiping, opaque journal restart, concurrent CAS, corruption and
backup-exclusion attributes. Existing `RemoraLinkJournalStoreTests` cover the
reused durability owner. End-to-end relay enrollment and provider delivery are
separate integration gates, not established by these storage tests.
