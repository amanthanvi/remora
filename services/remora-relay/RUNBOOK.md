# Remora Relay Operations Runbook

## Deployment invariants

Before serving production traffic, verify all of the following:

1. `deployment_profile` is `hosted` or `self_hosted` and the database kind is `postgres`.
2. The PostgreSQL URL comes from the configured environment variable, uses TLS according to the database provider's policy, and points to a dedicated least-privilege database role.
3. The token-encryption key and bootstrap token exist outside the image and repository. The key file decodes to exactly 32 random bytes and is not group/world accessible on a normal host filesystem.
4. TLS or mTLS terminates at a trusted ingress. The relay port and PostgreSQL are private. `/metrics` is restricted to monitoring infrastructure.
5. Installation bootstrap is rate-limited at ingress. Capabilities are bearer credentials; do not place them in URLs, logs, analytics, crash reports, or shell history.
6. Live APNs/FCM bearer-token refresh is healthy before enabling `push.mode = "live"`. Provider endpoints remain the fixed official endpoints.
7. At least two relay replicas are used for hosted availability. All replicas share PostgreSQL and the same token-encryption key. Do not share or replicate the SQLite local profile.

Validate static configuration before rollout:

```sh
remora-relay check-config --config /etc/remora-relay/config.toml
```

Then start one canary, wait for `/health/ready`, verify aggregate metrics, and expand the deployment. Startup runs only migrations needed to advance an older schema; it rejects a database newer than the binary rather than attempting a rollback. Deploy only versions whose migration has been tested against a recent database copy.

## Health and alerting

- `/health/live` should be `200` whenever the process event loop is alive.
- `/health/ready` should be `200` only when the database accepts a probe.
- Alert on sustained increases in `remora_relay_push_dead_lettered_total`, `remora_relay_push_retried_total`, `remora_relay_outbox_leases_recovered_total`, and `remora_relay_cursor_resets_total`.
- `remora_relay_push_invalid_tokens_total` may rise after normal app uninstalls or token rotation; investigate sharp correlated spikes.
- `remora_relay_ingest_conflicts_total` indicates buggy or hostile reuse of an event id with different content.

Metrics intentionally have no installation, registration, event, provider-token, or content labels. Use database-level aggregate queries for deeper incident analysis; do not add identifier values to logs.

## Backup and restore

Back up PostgreSQL and the token-encryption key as one recovery set. The database contains encrypted provider tokens, while the external key is required to decrypt them. Losing the key makes existing registrations unusable; leaking it exposes provider tokens from a database backup.

1. Use the PostgreSQL provider's encrypted point-in-time recovery or consistent snapshot facility.
2. Back up the token key through the secret manager with version history and access audit.
3. Regularly restore both into an isolated environment with provider sending disabled.
4. Run `/health/ready`, the PostgreSQL contract test, and aggregate diagnostics against the restored copy.
5. Destroy the isolated copy and test key according to the environment's retention policy.

Event and snapshot ciphertext remains end-to-end encrypted independently of the token key. Restoring an older database can move cursors backward relative to a client; clients must detect the high watermark/replay floor and perform snapshot or authoritative host repair.

## Key rotation

The current schema stores no key identifier, so in-place token-key rotation is an operational migration, not a file replacement:

1. Disable provider dispatch while leaving event ingest/fetch online, or stop all relay replicas.
2. Take a fresh database/key backup.
3. Use a reviewed one-off migration that decrypts each active registration with the old key and re-encrypts it with the new key while preserving associated-data fields and registration generations.
4. Atomically replace the mounted key across all replicas.
5. Start one canary with push disabled, verify it can lease/decrypt registrations, then enable live providers and roll out.
6. Retain the old key only for the documented rollback window, then revoke and destroy it.

Never rotate by simply overwriting the key while old ciphertext remains. If the key is lost, tombstone existing registrations and require clients to register fresh provider tokens.

Bootstrap-token rotation is simpler: atomically replace the token file and restart/roll replicas. Existing installation capabilities are unaffected. Provider bearer tokens are read on every attempt and may be atomically refreshed without a relay restart.

## Provider incidents

### Transient APNs/FCM outage

The outbox uses bounded full-jitter retry, honors bounded `Retry-After`, recovers expired leases, and stops at event expiry or the configured maximum attempts. Authoritative events are retained independently of push outcome.

- Confirm database health first.
- Check aggregate retry/dead-letter deltas and provider status pages.
- Do not lower provider timeouts or retry intervals enough to create a retry storm.
- Clients continue reconciling on foreground/reconnect even when every wake is lost.

### Invalid-token spike

Only explicit provider invalid-token results tombstone a registration. Tombstoning is bound to the leased registration generation, so stale responses cannot revoke a newer token.

- Verify the APNs topic/environment and FCM project have not changed unexpectedly.
- Verify mobile registration responses include and persist the relay generation.
- Do not manually reactivate rows; let clients perform an authenticated token upsert.

### Credential refresh failure

Missing/invalid provider bearer-token files fail provider delivery without exposing the credential or deleting authoritative events. Restore the refresh agent/file, verify permissions, and observe retries. Do not place long-lived APNs signing keys or Google service-account JSON directly in relay configuration.

## Database incidents

When PostgreSQL is unavailable, readiness fails and API/storage work returns a coarse temporary-unavailable error. Outbox leases expire and become eligible after recovery.

1. Stop routing new traffic if readiness has failed.
2. Restore PostgreSQL availability or fail over through the managed provider.
3. Confirm database time/network behavior and `/health/ready`.
4. Watch `outbox_leases_recovered_total`; recovery is expected and idempotent.
5. Verify cursor pages and one device registration before restoring full traffic.

Do not delete outbox rows to clear a backlog. Delivery is coalesced by registration and event class, and terminal push state does not affect event correctness.

## Retention and deletion

Maintenance removes expired events/snapshots, marks expired wake intents, advances each installation's replay floor, and eventually purges tombstoned installations after `tombstone_retention_ms`.

PostgreSQL maintenance processes bounded `SKIP LOCKED` batches so it does not become a fleet-wide write barrier. Lightweight event-id receipts remain until installation purge to preserve durable idempotency after ciphertext retention ends.

- A replay-floor gap intentionally returns `reset_required`; the client must use an encrypted snapshot or full authoritative repair.
- Installation deletion first tombstones the installation, registrations, and outbox. It is not an immediate physical erase because a short retention window supports safe in-flight shutdown and audit-free operational recovery.
- To satisfy a shorter deletion requirement, reduce the configured retention only after verifying provider leases and client revocation behavior. Never bypass generation checks on device revocation.

## Rollback

Application rollback is safe only when the old binary understands the current schema and wake schema version. Before rollout, record the prior image digest and verify its startup against a migrated test database.

- If the canary fails before accepting writes, roll back the image.
- If a migration is not backward compatible, prefer forward-fixing the binary. Restore from backup only with explicit approval because it can move authoritative cursors backward.
- Keep push disabled during an uncertain rollback. Clients remain correct through foreground/authenticated reconciliation.

## Security incident response

### Suspected installation capability exposure

There is no capability rotation endpoint in this version. Tombstone the installation with an uncompromised manage capability if available, otherwise quarantine it administratively at the database/ingress layer, and issue a new relay installation. Re-pair through the authenticated Remora Link flow.

### Suspected database-only exposure

Provider tokens remain XChaCha20-Poly1305 encrypted and capabilities remain hashed, but event metadata/ciphertext timing and aggregate topology are visible. Rotate database credentials, preserve evidence, and assess whether the token key was separately exposed.

### Suspected database plus token-key exposure

Treat every active provider token as exposed. Stop dispatch, rotate infrastructure credentials, tombstone registrations, require client re-registration, and rotate the at-rest key through the migration procedure. Event/snapshot contents still require the separate end-to-end application keys.

Logs must remain redacted during incident debugging. Do not enable request-body, authorization-header, URL, SQL-parameter, or provider-body logging.
