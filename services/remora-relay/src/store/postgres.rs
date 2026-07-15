//! PostgreSQL production store.
//!
//! Hosted and production self-hosted profiles use this adapter. It relies on
//! row locks for gap-free per-installation cursors and `SKIP LOCKED` for safe
//! multi-worker outbox leasing. The SQLite parent module is intentionally a
//! single-node local/development adapter only.

use std::{
    str::FromStr as _,
    sync::{Arc, atomic::Ordering},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use secrecy::SecretString;
use sqlx::{PgPool, Postgres, Row as _, Transaction, postgres::PgPoolOptions};

use super::{
    DeliveryOutcome, MaintenanceResult, OutboxLease, StoreDiagnostics, StoreLimits,
    capability_hash, constant_time_equal, decode_ciphertext, event_digest,
    installation_idempotency_hash, installation_request_digest, open_installation_receipt,
    push_token_hash, retry_delay_ms, seal_installation_receipt, snapshot_digest, to_i64, to_u64,
    token_aad, validate_expiry, validate_limits,
};
use crate::{
    AcknowledgeResponse, CapabilityKind, CreateInstallationRequest, DeviceRegistrationResponse,
    EventClass, EventEnvelope, EventPage, IngestEventRequest, IngestEventResponse,
    IssuedCapability, IssuedInstallation, OpaqueId, OpaqueWakeHint, PresentedCapability,
    PushEnvironment, PushProviderKind, RelayError, RelayMetrics, Result, SCHEMA_VERSION,
    SnapshotEnvelope, TokenCipher, crypto::SealedToken,
};

const SUPPORTED_SCHEMA_VERSION: i32 = 4;

#[derive(Clone)]
pub struct PostgresRelayStore {
    pool: PgPool,
    cipher: TokenCipher,
    limits: StoreLimits,
    metrics: Arc<RelayMetrics>,
}

impl std::fmt::Debug for PostgresRelayStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PostgresRelayStore")
            .field("database", &"[connection details redacted]")
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl PostgresRelayStore {
    pub async fn connect(
        database_url: &str,
        max_connections: u32,
        cipher: TokenCipher,
        limits: StoreLimits,
        metrics: Arc<RelayMetrics>,
    ) -> Result<Self> {
        validate_limits(&limits)?;
        if max_connections == 0 || max_connections > 200 {
            return Err(RelayError::Configuration(
                "PostgreSQL max_connections must be between 1 and 200".into(),
            ));
        }
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .connect(database_url)
            .await?;
        sqlx::raw_sql(
            "CREATE TABLE IF NOT EXISTS relay_schema (
                singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
                version INTEGER NOT NULL,
                updated_at_ms BIGINT NOT NULL
             )",
        )
        .execute(&pool)
        .await?;
        let current_version =
            sqlx::query_scalar::<_, i32>("SELECT version FROM relay_schema WHERE singleton = TRUE")
                .fetch_optional(&pool)
                .await?;
        if current_version.is_some_and(|version| version > SUPPORTED_SCHEMA_VERSION) {
            return Err(RelayError::Configuration(
                "PostgreSQL schema is newer than this relay binary".into(),
            ));
        }
        if current_version != Some(SUPPORTED_SCHEMA_VERSION) {
            sqlx::raw_sql(include_str!("../../migrations/0001_initial.sql"))
                .execute(&pool)
                .await?;
            let migrated_version = sqlx::query_scalar::<_, i32>(
                "SELECT version FROM relay_schema WHERE singleton = TRUE",
            )
            .fetch_optional(&pool)
            .await?;
            if migrated_version != Some(SUPPORTED_SCHEMA_VERSION) {
                return Err(RelayError::Configuration(
                    "PostgreSQL schema migration did not reach the supported version".into(),
                ));
            }
        }
        Ok(Self {
            pool,
            cipher,
            limits,
            metrics,
        })
    }

    pub fn limits(&self) -> &StoreLimits {
        &self.limits
    }

    pub async fn ready(&self) -> bool {
        sqlx::query_scalar::<_, i32>("SELECT 1")
            .fetch_one(&self.pool)
            .await
            .is_ok()
    }

    pub async fn create_installation(
        &self,
        request: &CreateInstallationRequest,
        now_ms: i64,
    ) -> Result<IssuedInstallation> {
        let key_hash = installation_idempotency_hash(request.idempotency_key.as_str());
        let request_digest = installation_request_digest(request);
        let advisory_key = i64::from_be_bytes(
            key_hash[..8]
                .try_into()
                .expect("SHA-256 prefix has fixed length"),
        );
        let mut transaction = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(advisory_key)
            .execute(&mut *transaction)
            .await?;

        if let Some(row) = sqlx::query(
            "SELECT r.request_digest, r.installation_id, r.response_nonce,
                    r.response_ciphertext, r.response_expires_at_ms,
                    i.id AS active_installation_id, i.tombstoned_at_ms
             FROM installation_receipts r
             LEFT JOIN installations i ON i.id = r.installation_id
             WHERE r.idempotency_key_hash = $1",
        )
        .bind(key_hash.as_slice())
        .fetch_optional(&mut *transaction)
        .await?
        {
            let stored_digest: Vec<u8> = row.try_get("request_digest")?;
            if !constant_time_equal(&stored_digest, &request_digest) {
                self.metrics
                    .installation_create_conflicts
                    .fetch_add(1, Ordering::Relaxed);
                return Err(RelayError::Conflict);
            }
            let active_installation_id: Option<String> = row.try_get("active_installation_id")?;
            let tombstoned_at_ms: Option<i64> = row.try_get("tombstoned_at_ms")?;
            let nonce: Option<Vec<u8>> = row.try_get("response_nonce")?;
            let ciphertext: Option<Vec<u8>> = row.try_get("response_ciphertext")?;
            let response_expires_at_ms: i64 = row.try_get("response_expires_at_ms")?;
            let (Some(_), None, Some(nonce), Some(ciphertext)) =
                (active_installation_id, tombstoned_at_ms, nonce, ciphertext)
            else {
                return Err(RelayError::Tombstoned);
            };
            if response_expires_at_ms <= now_ms {
                return Err(RelayError::Tombstoned);
            }
            let installation_id = OpaqueId::parse(row.try_get::<String, _>("installation_id")?)?;
            let nonce: [u8; 24] = nonce.try_into().map_err(|_| RelayError::Crypto)?;
            let issued = open_installation_receipt(
                &self.cipher,
                &key_hash,
                &request_digest,
                installation_id,
                SealedToken { nonce, ciphertext },
            )?;
            transaction.commit().await?;
            self.metrics
                .installation_create_replayed
                .fetch_add(1, Ordering::Relaxed);
            return Ok(issued);
        }

        if request.schema_version != SCHEMA_VERSION {
            return Err(RelayError::Invalid("schema version"));
        }
        let installation_id = OpaqueId::random("inst");
        let write = IssuedCapability::random();
        let read = IssuedCapability::random();
        let manage = IssuedCapability::random();
        let write_hash = capability_hash(CapabilityKind::Write, write.as_str());
        let read_hash = capability_hash(CapabilityKind::Read, read.as_str());
        let manage_hash = capability_hash(CapabilityKind::Manage, manage.as_str());
        let response_expires_at_ms = now_ms
            .checked_add(self.limits.installation_receipt_ttl_ms)
            .ok_or(RelayError::LimitExceeded)?;
        let issued = IssuedInstallation {
            schema_version: SCHEMA_VERSION,
            installation_id,
            write_capability: write,
            read_capability: read,
            manage_capability: manage,
            created: true,
        };
        let sealed = seal_installation_receipt(&self.cipher, &key_hash, &request_digest, &issued)?;
        sqlx::query(
            "INSERT INTO installations (
                id, write_capability_hash, read_capability_hash, manage_capability_hash,
                next_sequence, replay_floor, acknowledged_through, created_at_ms, updated_at_ms
             ) VALUES ($1, $2, $3, $4, 1, 1, 0, $5, $5)",
        )
        .bind(issued.installation_id.as_str())
        .bind(write_hash.as_slice())
        .bind(read_hash.as_slice())
        .bind(manage_hash.as_slice())
        .bind(now_ms)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "INSERT INTO installation_receipts (
                idempotency_key_hash, request_digest, installation_id,
                response_nonce, response_ciphertext, response_expires_at_ms, created_at_ms
             ) VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(key_hash.as_slice())
        .bind(request_digest.as_slice())
        .bind(issued.installation_id.as_str())
        .bind(sealed.nonce.as_slice())
        .bind(sealed.ciphertext)
        .bind(response_expires_at_ms)
        .bind(now_ms)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        self.metrics
            .installations_created
            .fetch_add(1, Ordering::Relaxed);
        Ok(issued)
    }

    pub async fn ingest_event(
        &self,
        installation_id: &OpaqueId,
        capability: &PresentedCapability,
        request: &IngestEventRequest,
        now_ms: i64,
    ) -> Result<IngestEventResponse> {
        let ciphertext = decode_ciphertext(
            &request.ciphertext,
            self.limits.max_event_bytes,
            "event ciphertext",
        )?;
        let snapshot = request
            .snapshot
            .as_ref()
            .map(|snapshot| {
                if snapshot.revision == 0 {
                    return Err(RelayError::Invalid("snapshot revision"));
                }
                let ciphertext = decode_ciphertext(
                    &snapshot.ciphertext,
                    self.limits.max_snapshot_bytes,
                    "snapshot ciphertext",
                )?;
                Ok((snapshot, ciphertext))
            })
            .transpose()?;
        let digest = event_digest(request, &ciphertext, snapshot.as_ref());

        let mut transaction = self.pool.begin().await?;
        let installation = sqlx::query(
            "SELECT write_capability_hash, tombstoned_at_ms, next_sequence
             FROM installations WHERE id = $1 FOR UPDATE",
        )
        .bind(installation_id.as_str())
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(installation) = installation else {
            return Err(RelayError::Unauthorized);
        };
        authorize_row(
            installation.try_get("write_capability_hash")?,
            installation.try_get("tombstoned_at_ms")?,
            CapabilityKind::Write,
            capability,
        )?;

        if let Some(existing) = sqlx::query(
            "SELECT sequence, request_digest FROM event_receipts
             WHERE installation_id = $1 AND event_id = $2",
        )
        .bind(installation_id.as_str())
        .bind(request.event_id.as_str())
        .fetch_optional(&mut *transaction)
        .await?
        {
            let cursor: i64 = existing.try_get("sequence")?;
            let stored_digest: Vec<u8> = existing.try_get("request_digest")?;
            if stored_digest == digest {
                transaction.commit().await?;
                self.metrics.ingest_replayed.fetch_add(1, Ordering::Relaxed);
                return Ok(IngestEventResponse::new(
                    request.event_id.clone(),
                    to_u64(cursor)?,
                    true,
                ));
            }
            self.metrics
                .ingest_conflicts
                .fetch_add(1, Ordering::Relaxed);
            return Err(RelayError::Conflict);
        }

        validate_expiry(now_ms, request.expires_at_ms, self.limits.max_event_ttl_ms)?;
        if let Some((snapshot, _)) = snapshot.as_ref() {
            validate_expiry(
                now_ms,
                snapshot.expires_at_ms,
                self.limits.max_snapshot_ttl_ms,
            )?;
        }

        let cursor: i64 = installation.try_get("next_sequence")?;
        let next_cursor = cursor.checked_add(1).ok_or(RelayError::LimitExceeded)?;
        sqlx::query(
            "UPDATE installations SET next_sequence = $2, updated_at_ms = $3 WHERE id = $1",
        )
        .bind(installation_id.as_str())
        .bind(next_cursor)
        .bind(now_ms)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "INSERT INTO event_receipts (
                installation_id, event_id, request_digest, sequence, created_at_ms
             ) VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(installation_id.as_str())
        .bind(request.event_id.as_str())
        .bind(digest.as_slice())
        .bind(cursor)
        .bind(now_ms)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "INSERT INTO events (
                installation_id, sequence, event_id, request_digest, event_class,
                expires_at_ms, ciphertext, created_at_ms
             ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(installation_id.as_str())
        .bind(cursor)
        .bind(request.event_id.as_str())
        .bind(digest.as_slice())
        .bind(request.event_class.as_str())
        .bind(request.expires_at_ms)
        .bind(ciphertext)
        .bind(now_ms)
        .execute(&mut *transaction)
        .await?;

        if let Some((snapshot, snapshot_ciphertext)) = snapshot {
            apply_snapshot_pg(
                &mut transaction,
                installation_id,
                snapshot.revision,
                cursor,
                snapshot.expires_at_ms,
                &snapshot_ciphertext,
                now_ms,
            )
            .await?;
        }
        let coalesced =
            upsert_outbox_pg(&mut transaction, installation_id, request, cursor, now_ms).await?;
        transaction.commit().await?;
        self.metrics.ingest_accepted.fetch_add(1, Ordering::Relaxed);
        self.metrics
            .outbox_coalesced
            .fetch_add(coalesced, Ordering::Relaxed);
        Ok(IngestEventResponse::new(
            request.event_id.clone(),
            to_u64(cursor)?,
            false,
        ))
    }

    pub async fn fetch_events(
        &self,
        installation_id: &OpaqueId,
        capability: &PresentedCapability,
        after: u64,
        requested_limit: u32,
        now_ms: i64,
    ) -> Result<EventPage> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *transaction)
            .await?;
        let installation = sqlx::query(
            "SELECT read_capability_hash, tombstoned_at_ms, next_sequence, replay_floor
             FROM installations WHERE id = $1",
        )
        .bind(installation_id.as_str())
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(RelayError::Unauthorized)?;
        authorize_row(
            installation.try_get("read_capability_hash")?,
            installation.try_get("tombstoned_at_ms")?,
            CapabilityKind::Read,
            capability,
        )?;
        let next_sequence: i64 = installation.try_get("next_sequence")?;
        let high_watermark = to_u64(next_sequence.saturating_sub(1))?;
        if after > high_watermark {
            return Err(RelayError::Invalid("cursor beyond high watermark"));
        }
        let replay_floor = to_u64(installation.try_get("replay_floor")?)?;
        let limit = requested_limit.clamp(1, self.limits.max_page_size);
        let rows = sqlx::query(
            "SELECT sequence, event_id, event_class, expires_at_ms, ciphertext
             FROM events
             WHERE installation_id = $1 AND sequence > $2 AND expires_at_ms > $3
             ORDER BY sequence ASC LIMIT $4",
        )
        .bind(installation_id.as_str())
        .bind(to_i64(after)?)
        .bind(now_ms)
        .bind(i64::from(limit))
        .fetch_all(&mut *transaction)
        .await?;
        let mut events = Vec::with_capacity(rows.len());
        for row in rows {
            events.push(EventEnvelope {
                event_id: OpaqueId::parse(row.try_get::<String, _>("event_id")?)?,
                cursor: to_u64(row.try_get("sequence")?)?,
                event_class: EventClass::from_str(row.try_get::<&str, _>("event_class")?)?,
                expires_at_ms: row.try_get("expires_at_ms")?,
                ciphertext: URL_SAFE_NO_PAD.encode(row.try_get::<Vec<u8>, _>("ciphertext")?),
            });
        }
        let expected_first = after.saturating_add(1);
        let mut reset_required = expected_first < replay_floor;
        let mut expected = expected_first;
        for event in &events {
            if event.cursor != expected {
                reset_required = true;
                break;
            }
            expected = expected.saturating_add(1);
        }
        if events.is_empty() && after < high_watermark {
            reset_required = true;
        }
        if reset_required {
            events.clear();
            self.metrics.cursor_resets.fetch_add(1, Ordering::Relaxed);
        }
        let snapshot_available: bool = sqlx::query_scalar(
            "SELECT EXISTS(
                SELECT 1 FROM snapshots WHERE installation_id = $1 AND expires_at_ms > $2
             )",
        )
        .bind(installation_id.as_str())
        .bind(now_ms)
        .fetch_one(&mut *transaction)
        .await?;
        let next_cursor = events.last().map_or(after, |event| event.cursor);
        let page = EventPage {
            schema_version: SCHEMA_VERSION,
            requested_after: after,
            next_cursor,
            high_watermark,
            replay_floor,
            reset_required,
            snapshot_available,
            events,
        };
        transaction.commit().await?;
        Ok(page)
    }

    pub async fn acknowledge(
        &self,
        installation_id: &OpaqueId,
        capability: &PresentedCapability,
        through_cursor: u64,
        now_ms: i64,
    ) -> Result<AcknowledgeResponse> {
        if through_cursor == 0 {
            return Err(RelayError::Invalid("acknowledgement cursor"));
        }
        let through_cursor = to_i64(through_cursor)?;
        let mut transaction = self.pool.begin().await?;
        authorize_pg_transaction(
            &mut transaction,
            installation_id,
            CapabilityKind::Read,
            capability,
        )
        .await?;
        let row = sqlx::query(
            "SELECT next_sequence, acknowledged_through FROM installations WHERE id = $1",
        )
        .bind(installation_id.as_str())
        .fetch_one(&mut *transaction)
        .await?;
        let next_sequence: i64 = row.try_get("next_sequence")?;
        let acknowledged_through: i64 = row.try_get("acknowledged_through")?;
        let high_watermark = next_sequence.saturating_sub(1);
        if through_cursor > high_watermark {
            return Err(RelayError::Invalid("cursor beyond high watermark"));
        }
        let replayed = through_cursor <= acknowledged_through;
        let current = acknowledged_through.max(through_cursor);
        if !replayed {
            sqlx::query(
                "UPDATE installations SET acknowledged_through = $2, updated_at_ms = $3
                 WHERE id = $1",
            )
            .bind(installation_id.as_str())
            .bind(current)
            .bind(now_ms)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        let metric = if replayed {
            &self.metrics.acknowledgements_replayed
        } else {
            &self.metrics.acknowledgements_advanced
        };
        metric.fetch_add(1, Ordering::Relaxed);
        Ok(AcknowledgeResponse::new(
            installation_id.clone(),
            to_u64(current)?,
            replayed,
        ))
    }

    pub async fn fetch_snapshot(
        &self,
        installation_id: &OpaqueId,
        capability: &PresentedCapability,
        now_ms: i64,
    ) -> Result<SnapshotEnvelope> {
        self.authorized_installation(installation_id, CapabilityKind::Read, capability)
            .await?;
        let row = sqlx::query(
            "SELECT revision, through_sequence, expires_at_ms, ciphertext
             FROM snapshots WHERE installation_id = $1 AND expires_at_ms > $2",
        )
        .bind(installation_id.as_str())
        .bind(now_ms)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(RelayError::NotFound)?;
        Ok(SnapshotEnvelope {
            schema_version: SCHEMA_VERSION,
            revision: to_u64(row.try_get("revision")?)?,
            through_cursor: to_u64(row.try_get("through_sequence")?)?,
            expires_at_ms: row.try_get("expires_at_ms")?,
            ciphertext: URL_SAFE_NO_PAD.encode(row.try_get::<Vec<u8>, _>("ciphertext")?),
        })
    }

    pub async fn register_device(
        &self,
        installation_id: &OpaqueId,
        capability: &PresentedCapability,
        provider: PushProviderKind,
        environment: PushEnvironment,
        token: &str,
        now_ms: i64,
    ) -> Result<DeviceRegistrationResponse> {
        if token.len() < 16
            || token.len() > self.limits.max_push_token_bytes
            || token.chars().any(char::is_control)
        {
            return Err(RelayError::Invalid("push token"));
        }
        if provider == PushProviderKind::Fcm && environment != PushEnvironment::Production {
            return Err(RelayError::Invalid("FCM environment"));
        }
        let mut transaction = self.pool.begin().await?;
        authorize_pg_transaction(
            &mut transaction,
            installation_id,
            CapabilityKind::Manage,
            capability,
        )
        .await?;
        let token_hash = push_token_hash(installation_id, provider, environment, token);
        let existing = sqlx::query(
            "SELECT id, generation, token_hash, tombstoned_at_ms FROM device_registrations
             WHERE installation_id = $1 AND provider = $2 AND environment = $3",
        )
        .bind(installation_id.as_str())
        .bind(provider.as_str())
        .bind(environment.as_str())
        .fetch_optional(&mut *transaction)
        .await?;
        let (registration_id, generation, replaced, created) = match existing {
            Some(row) => {
                let current_generation: i64 = row.try_get("generation")?;
                let stored_hash: Vec<u8> = row.try_get("token_hash")?;
                let tombstoned_at: Option<i64> = row.try_get("tombstoned_at_ms")?;
                let same_active = stored_hash == token_hash && tombstoned_at.is_none();
                (
                    OpaqueId::parse(row.try_get::<String, _>("id")?)?,
                    if same_active {
                        current_generation
                    } else {
                        current_generation
                            .checked_add(1)
                            .ok_or(RelayError::LimitExceeded)?
                    },
                    !same_active,
                    false,
                )
            }
            None => (OpaqueId::random("dev"), 1, false, true),
        };
        let aad = token_aad(installation_id, &registration_id, provider, environment);
        let sealed = self.cipher.seal(token.as_bytes(), &aad)?;
        sqlx::query(
            "INSERT INTO device_registrations (
                id, installation_id, provider, environment, token_hash, token_nonce,
                token_ciphertext, generation, created_at_ms, updated_at_ms, tombstoned_at_ms
             ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $9, NULL)
             ON CONFLICT(installation_id, provider, environment) DO UPDATE SET
                token_hash = EXCLUDED.token_hash,
                token_nonce = EXCLUDED.token_nonce,
                token_ciphertext = EXCLUDED.token_ciphertext,
                generation = EXCLUDED.generation,
                updated_at_ms = EXCLUDED.updated_at_ms,
                tombstoned_at_ms = NULL",
        )
        .bind(registration_id.as_str())
        .bind(installation_id.as_str())
        .bind(provider.as_str())
        .bind(environment.as_str())
        .bind(token_hash.as_slice())
        .bind(sealed.nonce.as_slice())
        .bind(sealed.ciphertext)
        .bind(generation)
        .bind(now_ms)
        .execute(&mut *transaction)
        .await?;
        if replaced {
            sqlx::query(
                "UPDATE push_outbox SET generation = generation + 1,
                 registration_generation = $2, state = 'pending', attempt_count = 0,
                 next_attempt_at_ms = $3, lease_id = NULL, lease_until_ms = NULL,
                 last_error_class = NULL,
                 updated_at_ms = $3 WHERE registration_id = $1",
            )
            .bind(registration_id.as_str())
            .bind(generation)
            .bind(now_ms)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        self.metrics
            .registrations_created
            .fetch_add(1, Ordering::Relaxed);
        Ok(DeviceRegistrationResponse {
            schema_version: SCHEMA_VERSION,
            installation_id: installation_id.clone(),
            registration_id,
            provider,
            environment,
            generation: to_u64(generation)?,
            replaced,
            created,
        })
    }

    pub async fn tombstone_registration(
        &self,
        installation_id: &OpaqueId,
        capability: &PresentedCapability,
        registration_id: &OpaqueId,
        through_generation: u64,
        now_ms: i64,
    ) -> Result<()> {
        let mut transaction = self.pool.begin().await?;
        authorize_pg_transaction(
            &mut transaction,
            installation_id,
            CapabilityKind::Manage,
            capability,
        )
        .await?;
        let result = sqlx::query(
            "UPDATE device_registrations
             SET tombstoned_at_ms = COALESCE(tombstoned_at_ms, $3), updated_at_ms = $3
             WHERE id = $1 AND installation_id = $2 AND generation <= $4",
        )
        .bind(registration_id.as_str())
        .bind(installation_id.as_str())
        .bind(now_ms)
        .bind(to_i64(through_generation)?)
        .execute(&mut *transaction)
        .await?;
        if result.rows_affected() == 0 {
            transaction.commit().await?;
            return Ok(());
        }
        sqlx::query(
            "UPDATE push_outbox SET state = 'tombstoned', lease_id = NULL, lease_until_ms = NULL,
             updated_at_ms = $2 WHERE registration_id = $1",
        )
        .bind(registration_id.as_str())
        .bind(now_ms)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        self.metrics
            .registrations_tombstoned
            .fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    pub async fn tombstone_installation(
        &self,
        installation_id: &OpaqueId,
        capability: &PresentedCapability,
        now_ms: i64,
    ) -> Result<()> {
        let mut transaction = self.pool.begin().await?;
        authorize_pg_transaction(
            &mut transaction,
            installation_id,
            CapabilityKind::Manage,
            capability,
        )
        .await?;
        sqlx::query(
            "UPDATE installations SET tombstoned_at_ms = COALESCE(tombstoned_at_ms, $2),
             updated_at_ms = $2 WHERE id = $1",
        )
        .bind(installation_id.as_str())
        .bind(now_ms)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "UPDATE device_registrations
             SET tombstoned_at_ms = COALESCE(tombstoned_at_ms, $2), updated_at_ms = $2
             WHERE installation_id = $1",
        )
        .bind(installation_id.as_str())
        .bind(now_ms)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "UPDATE push_outbox SET state = 'tombstoned', lease_id = NULL, lease_until_ms = NULL,
             updated_at_ms = $2 WHERE installation_id = $1",
        )
        .bind(installation_id.as_str())
        .bind(now_ms)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "UPDATE installation_receipts SET response_nonce = NULL, response_ciphertext = NULL
             WHERE installation_id = $1",
        )
        .bind(installation_id.as_str())
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn lease_deliveries(&self, now_ms: i64, limit: u32) -> Result<Vec<OutboxLease>> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query(
            "UPDATE push_outbox SET state = 'expired', lease_id = NULL,
             lease_until_ms = NULL, updated_at_ms = $1
             WHERE state IN ('pending', 'leased') AND expires_at_ms <= $1",
        )
        .bind(now_ms)
        .execute(&mut *transaction)
        .await?;
        let recovered = sqlx::query(
            "UPDATE push_outbox SET state = 'pending', lease_id = NULL, lease_until_ms = NULL,
             next_attempt_at_ms = $1, updated_at_ms = $1
             WHERE state = 'leased' AND lease_until_ms <= $1 AND expires_at_ms > $1",
        )
        .bind(now_ms)
        .execute(&mut *transaction)
        .await?
        .rows_affected();

        let candidates = sqlx::query(
            "SELECT o.id, o.generation, o.installation_id, o.event_id, o.cursor,
                    o.event_class, o.expires_at_ms, o.attempt_count, o.registration_generation,
                    r.id AS registration_id, r.provider, r.environment,
                    r.token_nonce, r.token_ciphertext
             FROM push_outbox o
             JOIN device_registrations r ON r.id = o.registration_id
             JOIN installations i ON i.id = o.installation_id
             WHERE o.state = 'pending' AND o.next_attempt_at_ms <= $1
               AND o.expires_at_ms > $1
               AND r.tombstoned_at_ms IS NULL AND i.tombstoned_at_ms IS NULL
             ORDER BY o.next_attempt_at_ms ASC, o.created_at_ms ASC
             LIMIT $2
             FOR UPDATE OF o SKIP LOCKED",
        )
        .bind(now_ms)
        .bind(i64::from(limit.clamp(1, 1_000)))
        .fetch_all(&mut *transaction)
        .await?;

        let mut leases = Vec::with_capacity(candidates.len());
        for row in candidates {
            let outbox_id: String = row.try_get("id")?;
            let generation: i64 = row.try_get("generation")?;
            let installation_id = OpaqueId::parse(row.try_get::<String, _>("installation_id")?)?;
            let registration_id = OpaqueId::parse(row.try_get::<String, _>("registration_id")?)?;
            let provider = PushProviderKind::from_str(row.try_get::<&str, _>("provider")?)?;
            let environment = PushEnvironment::from_str(row.try_get::<&str, _>("environment")?)?;
            let nonce: Vec<u8> = row.try_get("token_nonce")?;
            let nonce: [u8; 24] = nonce.try_into().map_err(|_| RelayError::Crypto)?;
            let sealed = SealedToken {
                nonce,
                ciphertext: row.try_get("token_ciphertext")?,
            };
            let aad = token_aad(&installation_id, &registration_id, provider, environment);
            let plaintext = self.cipher.open(&sealed, &aad)?;
            let token = String::from_utf8(plaintext.to_vec()).map_err(|_| RelayError::Crypto)?;
            let attempts: i32 = row.try_get("attempt_count")?;
            let registration_generation: i64 = row.try_get("registration_generation")?;
            let lease_id = OpaqueId::random("lease");
            sqlx::query(
                "UPDATE push_outbox SET state = 'leased', lease_id = $5, lease_until_ms = $3,
                 attempt_count = attempt_count + 1, updated_at_ms = $2
                 WHERE id = $1 AND generation = $4 AND state = 'pending'",
            )
            .bind(&outbox_id)
            .bind(now_ms)
            .bind(now_ms.saturating_add(self.limits.lease_ms))
            .bind(generation)
            .bind(lease_id.as_str())
            .execute(&mut *transaction)
            .await?;
            leases.push(OutboxLease {
                outbox_id: OpaqueId::parse(outbox_id)?,
                lease_id,
                generation: to_u64(generation)?,
                registration_generation: to_u64(registration_generation)?,
                provider,
                environment,
                token: SecretString::from(token),
                hint: OpaqueWakeHint {
                    schema_version: SCHEMA_VERSION,
                    installation_id,
                    event_id: OpaqueId::parse(row.try_get::<String, _>("event_id")?)?,
                    cursor: to_u64(row.try_get("cursor")?)?,
                    event_class: EventClass::from_str(row.try_get::<&str, _>("event_class")?)?,
                    expires_at_ms: row.try_get("expires_at_ms")?,
                },
                attempt: u32::try_from(attempts.saturating_add(1))
                    .map_err(|_| RelayError::LimitExceeded)?,
            });
        }
        transaction.commit().await?;
        self.metrics
            .outbox_leases_recovered
            .fetch_add(recovered, Ordering::Relaxed);
        Ok(leases)
    }

    pub async fn complete_delivery(
        &self,
        outbox_id: &OpaqueId,
        generation: u64,
        registration_generation: u64,
        lease_id: &OpaqueId,
        outcome: DeliveryOutcome,
        now_ms: i64,
    ) -> Result<()> {
        let mut transaction = self.pool.begin().await?;
        let installation_id: String =
            sqlx::query_scalar("SELECT installation_id FROM push_outbox WHERE id = $1")
                .bind(outbox_id.as_str())
                .fetch_optional(&mut *transaction)
                .await?
                .ok_or(RelayError::NotFound)?;
        // All mutations that can touch a registration and its outbox take the
        // installation row first. This prevents an invalid-token completion
        // (outbox -> registration) from deadlocking token rotation
        // (registration -> outbox).
        sqlx::query("SELECT id FROM installations WHERE id = $1 FOR UPDATE")
            .bind(&installation_id)
            .fetch_optional(&mut *transaction)
            .await?
            .ok_or(RelayError::NotFound)?;
        let row = sqlx::query(
            "SELECT generation, registration_id, registration_generation, attempt_count,
                    expires_at_ms, state, lease_id
             FROM push_outbox WHERE id = $1 FOR UPDATE",
        )
        .bind(outbox_id.as_str())
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(RelayError::NotFound)?;
        let current_generation: i64 = row.try_get("generation")?;
        let registration_id: String = row.try_get("registration_id")?;
        let current_registration_generation: i64 = row.try_get("registration_generation")?;
        let attempts: i32 = row.try_get("attempt_count")?;
        let expires_at_ms: i64 = row.try_get("expires_at_ms")?;
        let state: &str = row.try_get("state")?;
        let current_lease_id: Option<&str> = row.try_get("lease_id")?;

        if state != "leased" || current_lease_id != Some(lease_id.as_str()) {
            transaction.commit().await?;
            return Ok(());
        }

        if outcome == DeliveryOutcome::InvalidToken {
            if current_registration_generation != to_i64(registration_generation)? {
                sqlx::query(
                    "UPDATE push_outbox SET state = 'pending', lease_id = NULL,
                     lease_until_ms = NULL, attempt_count = 0,
                     next_attempt_at_ms = $2, updated_at_ms = $2 WHERE id = $1",
                )
                .bind(outbox_id.as_str())
                .bind(now_ms)
                .execute(&mut *transaction)
                .await?;
                transaction.commit().await?;
                return Ok(());
            }
            let changed = sqlx::query(
                "UPDATE device_registrations
                 SET tombstoned_at_ms = COALESCE(tombstoned_at_ms, $2), updated_at_ms = $2
                 WHERE id = $1 AND generation = $3",
            )
            .bind(&registration_id)
            .bind(now_ms)
            .bind(current_registration_generation)
            .execute(&mut *transaction)
            .await?
            .rows_affected();
            if changed > 0 {
                sqlx::query(
                    "UPDATE push_outbox SET state = 'tombstoned', lease_id = NULL,
                     lease_until_ms = NULL,
                 updated_at_ms = $2 WHERE registration_id = $1",
                )
                .bind(&registration_id)
                .bind(now_ms)
                .execute(&mut *transaction)
                .await?;
            }
            transaction.commit().await?;
            self.metrics
                .push_invalid_tokens
                .fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }

        if current_generation != to_i64(generation)? {
            sqlx::query(
                "UPDATE push_outbox SET state = 'pending', lease_id = NULL,
                 lease_until_ms = NULL, attempt_count = 0,
                 next_attempt_at_ms = $2, updated_at_ms = $2 WHERE id = $1",
            )
            .bind(outbox_id.as_str())
            .bind(now_ms)
            .execute(&mut *transaction)
            .await?;
            transaction.commit().await?;
            return Ok(());
        }

        let mut terminal_state = None;
        match outcome {
            DeliveryOutcome::Accepted => {
                terminal_state = Some("delivered");
                self.metrics.push_accepted.fetch_add(1, Ordering::Relaxed);
            }
            DeliveryOutcome::Suppressed => {
                terminal_state = Some("suppressed");
                self.metrics.push_suppressed.fetch_add(1, Ordering::Relaxed);
            }
            DeliveryOutcome::PermanentFailure => {
                terminal_state = Some("dead_letter");
                self.metrics
                    .push_dead_lettered
                    .fetch_add(1, Ordering::Relaxed);
            }
            DeliveryOutcome::Retry { retry_after_ms } => {
                if attempts >= self.limits.max_delivery_attempts as i32 || expires_at_ms <= now_ms {
                    terminal_state = Some(if expires_at_ms <= now_ms {
                        "expired"
                    } else {
                        "dead_letter"
                    });
                    self.metrics
                        .push_dead_lettered
                        .fetch_add(1, Ordering::Relaxed);
                } else {
                    let delay = retry_after_ms
                        .map(|delay| delay.clamp(0, self.limits.retry_cap_ms))
                        .unwrap_or_else(|| {
                            retry_delay_ms(
                                outbox_id.as_str(),
                                attempts as u32,
                                self.limits.retry_base_ms,
                                self.limits.retry_cap_ms,
                            )
                        })
                        .max(1);
                    sqlx::query(
                        "UPDATE push_outbox SET state = 'pending', lease_id = NULL,
                         lease_until_ms = NULL,
                         next_attempt_at_ms = $2, updated_at_ms = $3,
                         last_error_class = 'transient' WHERE id = $1",
                    )
                    .bind(outbox_id.as_str())
                    .bind(now_ms.saturating_add(delay))
                    .bind(now_ms)
                    .execute(&mut *transaction)
                    .await?;
                    self.metrics.push_retried.fetch_add(1, Ordering::Relaxed);
                }
            }
            DeliveryOutcome::InvalidToken => unreachable!(),
        }
        if let Some(state) = terminal_state {
            sqlx::query(
                "UPDATE push_outbox SET state = $2, lease_id = NULL, lease_until_ms = NULL,
                 updated_at_ms = $3 WHERE id = $1",
            )
            .bind(outbox_id.as_str())
            .bind(state)
            .bind(now_ms)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn maintenance(&self, now_ms: i64) -> Result<MaintenanceResult> {
        let mut result = MaintenanceResult::default();
        const INSTALLATION_BATCH: i64 = 32;
        const EVENT_PREFIX_BATCH: i64 = 1_000;
        const ROW_BATCH: i64 = 1_000;

        // Only installations whose current replay floor is expired are locked.
        // Bounded SKIP LOCKED batches allow other replicas and unrelated
        // installation writes to continue while retention runs.
        loop {
            let mut transaction = self.pool.begin().await?;
            let installations = sqlx::query(
                "SELECT i.id, i.replay_floor FROM installations i
                 WHERE i.tombstoned_at_ms IS NULL AND EXISTS (
                    SELECT 1 FROM events e
                    WHERE e.installation_id = i.id AND e.sequence = i.replay_floor
                      AND e.expires_at_ms <= $1
                 )
                 ORDER BY i.id
                 LIMIT $2 FOR UPDATE OF i SKIP LOCKED",
            )
            .bind(now_ms)
            .bind(INSTALLATION_BATCH)
            .fetch_all(&mut *transaction)
            .await?;
            if installations.is_empty() {
                transaction.commit().await?;
                break;
            }
            for installation in installations {
                let id: String = installation.try_get("id")?;
                let floor: i64 = installation.try_get("replay_floor")?;
                let rows = sqlx::query(
                    "SELECT sequence FROM events
                     WHERE installation_id = $1 AND sequence >= $2 AND expires_at_ms <= $3
                     ORDER BY sequence ASC LIMIT $4",
                )
                .bind(&id)
                .bind(floor)
                .bind(now_ms)
                .bind(EVENT_PREFIX_BATCH)
                .fetch_all(&mut *transaction)
                .await?;
                let mut next_floor = floor;
                for row in rows {
                    let sequence: i64 = row.try_get("sequence")?;
                    if sequence != next_floor {
                        break;
                    }
                    next_floor = next_floor.saturating_add(1);
                }
                if next_floor > floor {
                    result.expired_events += sqlx::query(
                        "DELETE FROM events WHERE installation_id = $1 AND sequence < $2",
                    )
                    .bind(&id)
                    .bind(next_floor)
                    .execute(&mut *transaction)
                    .await?
                    .rows_affected();
                    sqlx::query(
                        "UPDATE installations SET replay_floor = $2, updated_at_ms = $3
                         WHERE id = $1",
                    )
                    .bind(&id)
                    .bind(next_floor)
                    .bind(now_ms)
                    .execute(&mut *transaction)
                    .await?;
                }
            }
            transaction.commit().await?;
        }

        loop {
            let changed = sqlx::query(
                "WITH due AS (
                    SELECT installation_id FROM snapshots WHERE expires_at_ms <= $1
                    ORDER BY installation_id LIMIT $2 FOR UPDATE SKIP LOCKED
                 )
                 DELETE FROM snapshots s USING due
                 WHERE s.installation_id = due.installation_id",
            )
            .bind(now_ms)
            .bind(ROW_BATCH)
            .execute(&self.pool)
            .await?
            .rows_affected();
            result.expired_snapshots += changed;
            if changed < ROW_BATCH as u64 {
                break;
            }
        }

        loop {
            let changed = sqlx::query(
                "WITH due AS (
                    SELECT id FROM push_outbox
                    WHERE state IN ('pending', 'leased') AND expires_at_ms <= $1
                    ORDER BY id LIMIT $2 FOR UPDATE SKIP LOCKED
                 )
                 UPDATE push_outbox o SET state = 'expired', lease_id = NULL,
                    lease_until_ms = NULL, updated_at_ms = $1
                 FROM due WHERE o.id = due.id",
            )
            .bind(now_ms)
            .bind(ROW_BATCH)
            .execute(&self.pool)
            .await?
            .rows_affected();
            result.expired_outbox += changed;
            if changed < ROW_BATCH as u64 {
                break;
            }
        }

        loop {
            let changed = sqlx::query(
                "WITH due AS (
                    SELECT idempotency_key_hash FROM installation_receipts
                    WHERE response_ciphertext IS NOT NULL AND response_expires_at_ms <= $1
                    ORDER BY idempotency_key_hash LIMIT $2 FOR UPDATE SKIP LOCKED
                 )
                 UPDATE installation_receipts r
                 SET response_nonce = NULL, response_ciphertext = NULL
                 FROM due WHERE r.idempotency_key_hash = due.idempotency_key_hash",
            )
            .bind(now_ms)
            .bind(ROW_BATCH)
            .execute(&self.pool)
            .await?
            .rows_affected();
            result.expired_installation_receipts += changed;
            if changed < ROW_BATCH as u64 {
                break;
            }
        }

        let purge_before = now_ms.saturating_sub(self.limits.tombstone_retention_ms);
        loop {
            let changed = sqlx::query(
                "WITH due AS (
                    SELECT id FROM installations
                    WHERE tombstoned_at_ms IS NOT NULL AND tombstoned_at_ms <= $1
                    ORDER BY id LIMIT $2 FOR UPDATE SKIP LOCKED
                 )
                 DELETE FROM installations i USING due WHERE i.id = due.id",
            )
            .bind(purge_before)
            .bind(ROW_BATCH)
            .execute(&self.pool)
            .await?
            .rows_affected();
            result.purged_tombstones += changed;
            if changed < ROW_BATCH as u64 {
                break;
            }
        }
        self.metrics
            .maintenance_expired_events
            .fetch_add(result.expired_events, Ordering::Relaxed);
        Ok(result)
    }

    pub async fn diagnostics(&self) -> Result<StoreDiagnostics> {
        let count = |sql: &'static str| async move {
            sqlx::query_scalar::<_, i64>(sql)
                .fetch_one(&self.pool)
                .await
                .map_err(RelayError::from)
                .and_then(to_u64)
        };
        Ok(StoreDiagnostics {
            active_installations: count(
                "SELECT COUNT(*) FROM installations WHERE tombstoned_at_ms IS NULL",
            )
            .await?,
            tombstoned_installations: count(
                "SELECT COUNT(*) FROM installations WHERE tombstoned_at_ms IS NOT NULL",
            )
            .await?,
            retained_events: count("SELECT COUNT(*) FROM events").await?,
            active_registrations: count(
                "SELECT COUNT(*) FROM device_registrations WHERE tombstoned_at_ms IS NULL",
            )
            .await?,
            pending_outbox: count("SELECT COUNT(*) FROM push_outbox WHERE state = 'pending'")
                .await?,
            leased_outbox: count("SELECT COUNT(*) FROM push_outbox WHERE state = 'leased'").await?,
            dead_letter_outbox: count(
                "SELECT COUNT(*) FROM push_outbox WHERE state = 'dead_letter'",
            )
            .await?,
        })
    }

    async fn authorized_installation(
        &self,
        installation_id: &OpaqueId,
        kind: CapabilityKind,
        capability: &PresentedCapability,
    ) -> Result<sqlx::postgres::PgRow> {
        let row = sqlx::query(
            "SELECT write_capability_hash, read_capability_hash, manage_capability_hash,
                    tombstoned_at_ms, next_sequence, replay_floor
             FROM installations WHERE id = $1",
        )
        .bind(installation_id.as_str())
        .fetch_optional(&self.pool)
        .await?
        .ok_or(RelayError::Unauthorized)?;
        let hash = match kind {
            CapabilityKind::Write => row.try_get("write_capability_hash")?,
            CapabilityKind::Read => row.try_get("read_capability_hash")?,
            CapabilityKind::Manage => row.try_get("manage_capability_hash")?,
        };
        authorize_row(hash, row.try_get("tombstoned_at_ms")?, kind, capability)?;
        Ok(row)
    }
}

fn authorize_row(
    stored_hash: Vec<u8>,
    tombstoned_at_ms: Option<i64>,
    kind: CapabilityKind,
    capability: &PresentedCapability,
) -> Result<()> {
    let presented_hash = capability_hash(kind, capability.expose());
    if !constant_time_equal(&stored_hash, &presented_hash) {
        return Err(RelayError::Unauthorized);
    }
    if tombstoned_at_ms.is_some() {
        return Err(RelayError::Tombstoned);
    }
    Ok(())
}

async fn authorize_pg_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    installation_id: &OpaqueId,
    kind: CapabilityKind,
    capability: &PresentedCapability,
) -> Result<()> {
    let row = sqlx::query(
        "SELECT write_capability_hash, read_capability_hash, manage_capability_hash,
                tombstoned_at_ms FROM installations WHERE id = $1 FOR UPDATE",
    )
    .bind(installation_id.as_str())
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or(RelayError::Unauthorized)?;
    let hash = match kind {
        CapabilityKind::Write => row.try_get("write_capability_hash")?,
        CapabilityKind::Read => row.try_get("read_capability_hash")?,
        CapabilityKind::Manage => row.try_get("manage_capability_hash")?,
    };
    authorize_row(hash, row.try_get("tombstoned_at_ms")?, kind, capability)
}

async fn apply_snapshot_pg(
    transaction: &mut Transaction<'_, Postgres>,
    installation_id: &OpaqueId,
    revision: u64,
    through_sequence: i64,
    expires_at_ms: i64,
    ciphertext: &[u8],
    now_ms: i64,
) -> Result<()> {
    let digest = snapshot_digest(revision, expires_at_ms, ciphertext);
    if let Some(row) = sqlx::query(
        "SELECT revision, snapshot_digest FROM snapshots
         WHERE installation_id = $1 FOR UPDATE",
    )
    .bind(installation_id.as_str())
    .fetch_optional(&mut **transaction)
    .await?
    {
        let current_revision: i64 = row.try_get("revision")?;
        let current_digest: Vec<u8> = row.try_get("snapshot_digest")?;
        let revision_i64 = to_i64(revision)?;
        if revision_i64 < current_revision
            || (revision_i64 == current_revision && current_digest != digest)
        {
            return Err(RelayError::Conflict);
        }
        if revision_i64 == current_revision {
            return Ok(());
        }
    }
    sqlx::query(
        "INSERT INTO snapshots (
            installation_id, revision, through_sequence, snapshot_digest,
            expires_at_ms, ciphertext, updated_at_ms
         ) VALUES ($1, $2, $3, $4, $5, $6, $7)
         ON CONFLICT(installation_id) DO UPDATE SET
            revision = EXCLUDED.revision,
            through_sequence = EXCLUDED.through_sequence,
            snapshot_digest = EXCLUDED.snapshot_digest,
            expires_at_ms = EXCLUDED.expires_at_ms,
            ciphertext = EXCLUDED.ciphertext,
            updated_at_ms = EXCLUDED.updated_at_ms",
    )
    .bind(installation_id.as_str())
    .bind(to_i64(revision)?)
    .bind(through_sequence)
    .bind(digest.as_slice())
    .bind(expires_at_ms)
    .bind(ciphertext)
    .bind(now_ms)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn upsert_outbox_pg(
    transaction: &mut Transaction<'_, Postgres>,
    installation_id: &OpaqueId,
    request: &IngestEventRequest,
    cursor: i64,
    now_ms: i64,
) -> Result<u64> {
    let registrations = sqlx::query(
        "SELECT id, generation FROM device_registrations
         WHERE installation_id = $1 AND tombstoned_at_ms IS NULL",
    )
    .bind(installation_id.as_str())
    .fetch_all(&mut **transaction)
    .await?;
    let mut coalesced = 0;
    for registration in registrations {
        let registration_id: String = registration.try_get("id")?;
        let registration_generation: i64 = registration.try_get("generation")?;
        let existed: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM push_outbox
             WHERE registration_id = $1 AND event_class = $2)",
        )
        .bind(&registration_id)
        .bind(request.event_class.as_str())
        .fetch_one(&mut **transaction)
        .await?;
        coalesced += u64::from(existed);
        let outbox_id = OpaqueId::random("wake");
        sqlx::query(
            "INSERT INTO push_outbox (
                id, installation_id, registration_id, event_id, cursor, event_class,
                expires_at_ms, generation, registration_generation, attempt_count, next_attempt_at_ms, state,
                lease_until_ms, created_at_ms, updated_at_ms
             ) VALUES ($1, $2, $3, $4, $5, $6, $7, 1, $8, 0, $9, 'pending', NULL, $9, $9)
             ON CONFLICT(registration_id, event_class) DO UPDATE SET
                event_id = EXCLUDED.event_id,
                cursor = EXCLUDED.cursor,
                expires_at_ms = EXCLUDED.expires_at_ms,
                generation = push_outbox.generation + 1,
                registration_generation = EXCLUDED.registration_generation,
                attempt_count = 0,
                next_attempt_at_ms = CASE WHEN push_outbox.state = 'leased'
                    THEN push_outbox.next_attempt_at_ms ELSE EXCLUDED.next_attempt_at_ms END,
                state = CASE WHEN push_outbox.state = 'leased'
                    THEN 'leased' ELSE 'pending' END,
                lease_until_ms = CASE WHEN push_outbox.state = 'leased'
                    THEN push_outbox.lease_until_ms ELSE NULL END,
                lease_id = CASE WHEN push_outbox.state = 'leased'
                    THEN push_outbox.lease_id ELSE NULL END,
                last_error_class = NULL,
                updated_at_ms = EXCLUDED.updated_at_ms",
        )
        .bind(outbox_id.as_str())
        .bind(installation_id.as_str())
        .bind(registration_id)
        .bind(request.event_id.as_str())
        .bind(cursor)
        .bind(request.event_class.as_str())
        .bind(request.expires_at_ms)
        .bind(registration_generation)
        .bind(now_ms)
        .execute(&mut **transaction)
        .await?;
    }
    Ok(coalesced)
}
