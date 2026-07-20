# Remora Relay

`remora-relay` is the durable, ciphertext-blind event and background-wake service for Remora Link. It gives an installation a monotonic event cursor, stores end-to-end encrypted event/snapshot bytes, and asks APNs or FCM to wake a client. A push acceptance is never treated as application acknowledgement and never advances or deletes authoritative state.

The implementation has two deliberately different storage profiles:

- PostgreSQL is required for hosted and production self-hosted deployments. It uses row locks for gap-free per-installation cursors and `FOR UPDATE SKIP LOCKED` for horizontally safe outbox leasing.
- SQLite is an explicit single-process local-development adapter. It is useful for tests and laptop development, but must not be used as a production or multi-replica topology.

## Security and privacy contract

The relay is content-blind by design.

- Event and snapshot bodies are client-produced end-to-end ciphertext. The relay validates only bounds, expiry, identifiers, and idempotency digests.
- Installation capabilities are random and scoped. Only domain-separated SHA-256 capability hashes are stored on the installation row.
- The three capabilities have separate authority: host-only `write` ingests encrypted events; device `read` fetches events/snapshots and advances the advisory acknowledgement cursor; device-admin `manage` rotates or revokes push registrations and tombstones the installation. Do not give the write capability to a mobile reader or use it to acknowledge.
- Installation creation is retry-safe. The caller supplies a random idempotency key; the relay stores only its domain-separated hash and an AEAD-encrypted copy of the issued capability bundle for the bounded recovery window. Exact retries return byte-identical capability values. Conflicting reuse returns `409`, and an expired or tombstoned receipt returns `410` without minting another installation.
- Provider device tokens are encrypted at rest with XChaCha20-Poly1305. Associated data binds an encrypted token to its installation, registration, provider, and environment.
- Application logs and Prometheus metrics are aggregate and low-cardinality. They do not contain capability values, provider tokens, opaque IDs, event bodies, prompt text, transcripts, host paths, or database URLs.
- APNs/FCM endpoints are fixed in code. Configuration cannot redirect provider credentials to an arbitrary URL.

The provider-visible wake contract is closed. APNs receives `aps: {"content-available": 1}` plus exactly these six application fields; FCM receives exactly these six strings in a data-only message:

```text
schema_version
installation_id
event_id
cursor
event_class
expires_at_ms
```

`event_class` is one of `state_changed`, `activity_changed`, `connection_changed`, or `security_changed`. There is no notification title/body, prompt, transcript, host, thread, user, URL, command, approval action, or arbitrary extension map. The client treats a wake as an untrusted hint and reconciles through its authenticated authoritative channel.

## Identity and device registration

`POST /v1/installations` accepts this closed request and issues the relay identity and all three capabilities:

```json
{
  "schema_version": 1,
  "idempotency_key": "txn_<at-least-28-url-safe-random-characters>"
}
```

A mobile client must generate the `txn_` suffix with a cryptographically secure random source; the complete key must be 32–128 URL-safe characters. It must durably persist the returned `installation_id`; a locally generated installation or idempotency identifier is not a relay identity. Fresh creation returns `201`; an exact retry returns `200` with the same response. The default encrypted recovery window is 90 days and is bounded to at most 365 days by `limits.installation_receipt_ttl_ms`. After that window the relay destroys the replayable ciphertext but permanently retains the key hash and request fingerprint, so the consumed key can never silently create a second installation.

The v1 idempotency namespace is relay-global because a deployment has one bootstrap principal. The stable request fingerprint covers semantic schema fields only, never timestamps or transport headers. A future multi-tenant/bootstrap-principal deployment must migrate the stored key hash to include a stable principal identifier before accepting overlapping namespaces; do not make that change by merely altering request routing.

Treat the response as one logical, one-time provisioning transfer. The bootstrap channel receives the full bundle only on the original request or its exact ambiguous retries; no lookup endpoint returns capabilities. Transfer only `write` to the authenticated host and only `read` plus `manage` to the intended device-side secure store. Keep the idempotency key until the capability bundle is durably committed, then clear it. The Rust relay wraps owned capability, decrypted-receipt, token, and idempotency-key buffers in zeroizing containers and redacts their `Debug` output. HTTP/TLS stacks and client-language copies cannot promise physical memory erasure, so clients must minimize copies, avoid immutable analytics/log strings, persist through platform secure storage, and clear mutable temporary buffers where supported.

Device registration is an atomic upsert scoped by `(installation_id, provider, environment)`:

- Re-registering the same active token is idempotent and leaves `generation` unchanged.
- Rotating or reactivating a token increments `generation`.
- APNs accepts `sandbox` and `production`; FCM accepts `production` only.
- A delete supplies `through_generation`. It is idempotent and tombstones only a registration whose current generation is not newer.
- Every leased outbox item captures the registration generation. A delayed invalid-token response from an old lease cannot revoke a newly rotated token.

The registration response is:

```json
{
  "schema_version": 1,
  "installation_id": "inst_...",
  "registration_id": "dev_...",
  "provider": "apns",
  "environment": "sandbox",
  "generation": 1,
  "replaced": false
}
```

## Cursor and reconciliation semantics

Each new event receives one server-assigned, gap-free cursor within its installation. Reusing an `event_id` with byte-for-byte equivalent request semantics returns the original cursor with `replayed: true`; reusing it with different content returns `409 conflict`.

Payload retention does not weaken idempotency. A lightweight receipt containing only the opaque event id, request digest, and original cursor remains for the installation lifetime, including after ciphertext expiry. It is deleted with the installation.

`GET .../events?after=N` returns a high watermark and replay floor. If expiry/retention produced a gap, `reset_required` is true and the event list is empty. The client must fetch the encrypted snapshot when available or perform a full authoritative host repair. Push delivery and outbox state never alter this rule.

After authenticated reconciliation succeeds, the device may `PUT .../ack` with `{ "schema_version": 1, "through_cursor": N }` using the read capability. `N` must be at least one and no greater than the installation high watermark. The relay atomically stores `max(current, N)` and returns the current `acknowledged_through`; `replayed` is true for equal or lower requests. Acknowledgement is advisory per installation. It never deletes events, advances the replay floor, suppresses push, or proves that every possible reader consumed data; expiry and snapshot repair remain authoritative.

```json
{
  "schema_version": 1,
  "installation_id": "inst_...",
  "acknowledged_through": 42,
  "replayed": false
}
```

An event may atomically carry a newer complete encrypted snapshot. Snapshot revisions are monotonic. A stale revision conflicts and rolls back the event and all associated outbox work in the same transaction.

## HTTP surface

| Method | Path | Authority | Result |
| --- | --- | --- | --- |
| `GET` | `/health/live` | none | Process liveness. |
| `GET` | `/health/ready` | none | Database readiness. |
| `GET` | `/metrics` | deployment/network policy | Aggregate Prometheus counters only. |
| `POST` | `/v1/installations` | bootstrap bearer, or explicit loopback-only local mode | Idempotently issues relay identity and scoped capabilities. |
| `POST` | `/v1/installations/{id}/events` | write | Durable encrypted event ingest. |
| `GET` | `/v1/installations/{id}/events?after=N&limit=M` | read | Cursor page and reset metadata. |
| `GET` | `/v1/installations/{id}/snapshot` | read | Current unexpired encrypted snapshot. |
| `PUT` | `/v1/installations/{id}/ack` | read | Monotonically advances advisory device acknowledgement. |
| `POST` | `/v1/installations/{id}/devices` | manage | Generation-aware provider-token upsert. |
| `DELETE` | `/v1/installations/{id}/devices/{registration_id}?through_generation=N` | manage | Generation-bounded registration tombstone. |
| `DELETE` | `/v1/installations/{id}` | manage | Installation and device tombstone. |

All installation endpoint authority is sent as `Authorization: Bearer <capability>`. JSON request structures reject unknown fields. Error bodies use stable coarse error classes and do not echo storage/provider details.

## Local development

Rust 1.94 or newer is required. The minimal SQLite/mock setup is:

```sh
mkdir -p .local
cp config.local.toml.example .local/config.toml
cargo run --locked -- check-config --config .local/config.toml
cargo run --locked -- serve --config .local/config.toml
```

The local profile creates `.local/push-token.key` with mode `0600`. It binds to loopback and explicitly permits unauthenticated installation bootstrap. Do not expose that profile through a proxy or non-loopback listener.

For the production PostgreSQL code path locally:

```sh
mkdir -p .local
printf 'local-development-only\n' > .local/postgres-password
openssl rand -base64 48 | tr -d '\n' > .local/bootstrap-token
docker compose up --build
```

The Compose listener is published only on `127.0.0.1:8787`; PostgreSQL is not published to the host. `.dockerignore` excludes `.local` so secret material is not sent in the image build context. The Compose credentials are development-only and must never be reused.

## Production configuration

Start with `config.self-hosted.toml.example` and provision these outside the repository:

- `REMORA_RELAY_DATABASE_URL`: a TLS-protected PostgreSQL URL supplied by the secret manager.
- `security.token_key_path`: URL-safe unpadded base64 encoding of exactly 32 random bytes, readable only by the relay service account. This key is required before production startup and must be backed up with the database.
- `security.bootstrap_token_file`: a high-entropy bootstrap bearer token of at least 32 non-whitespace characters.
- For live push, short-lived APNs JWT and/or Google OAuth bearer-token files maintained by a separate credential refresh agent. The relay reloads the file for every send; long-lived signing/service-account keys do not need to enter this process.

Hosted and self-hosted profiles fail closed on SQLite, mock push, missing key material, unsafe worker limits, invalid provider identifiers, and unauthenticated non-loopback bootstrap. Provider endpoints are not configurable.

The relay currently assumes TLS is terminated by a trusted ingress or service mesh. Bind the service only to a private network, require TLS/mTLS at ingress, restrict `/metrics`, rate-limit installation bootstrap, and do not expose PostgreSQL. See [RUNBOOK.md](RUNBOOK.md) for deployment, backup, rotation, and incident procedures.

## Verification

The fast deterministic suite includes unit, concurrency, property, transaction fault-injection, provider-payload, and API-boundary tests:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets
cargo build --locked --release
```

Production storage behavior must also run against an isolated PostgreSQL database that the test may truncate:

```sh
REMORA_RELAY_TEST_DATABASE_URL='postgres://...' \
  cargo test --locked --features required-postgres-tests \
  --test postgres_contract -- --nocapture
```

That contract test verifies the populated v3-to-v4 acknowledgement/provisioning-receipt migration and newer-schema rejection, races 32 exact installation creations, concurrently allocates and acknowledges 64 cursors, verifies exact replay, exercises competing outbox leasers and physical lease recovery, checks concurrent rotation versus invalid-token completion, and proves maintenance skips a locked unrelated installation. CI must use the `required-postgres-tests` feature: the target fails if the database variable is absent. Ordinary local unit runs print an explicit skip message rather than substituting SQLite.
