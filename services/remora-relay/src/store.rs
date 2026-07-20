use std::{
    path::{Path, PathBuf},
    str::FromStr as _,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rusqlite::{Connection, OptionalExtension as _, Transaction, TransactionBehavior, params};
use secrecy::SecretString;
use sha2::{Digest as _, Sha256};

use crate::{
    AcknowledgeResponse, CapabilityKind, CreateInstallationRequest, DeviceRegistrationResponse,
    EventClass, EventEnvelope, EventPage, IngestEventRequest, IngestEventResponse,
    IssuedCapability, IssuedInstallation, OpaqueId, OpaqueWakeHint, PresentedCapability,
    PushEnvironment, PushProviderKind, RelayError, RelayMetrics, Result, SCHEMA_VERSION,
    SnapshotEnvelope, TokenCipher, crypto::SealedToken,
};

pub mod postgres;
pub use postgres::PostgresRelayStore;

const MILLIS_PER_DAY: i64 = 24 * 60 * 60 * 1_000;

#[derive(Clone, Debug)]
pub struct StoreLimits {
    pub max_event_bytes: usize,
    pub max_snapshot_bytes: usize,
    pub max_event_ttl_ms: i64,
    pub max_snapshot_ttl_ms: i64,
    pub max_page_size: u32,
    pub max_push_token_bytes: usize,
    pub lease_ms: i64,
    pub retry_base_ms: i64,
    pub retry_cap_ms: i64,
    pub max_delivery_attempts: u32,
    pub installation_receipt_ttl_ms: i64,
    pub tombstone_retention_ms: i64,
}

impl Default for StoreLimits {
    fn default() -> Self {
        Self {
            max_event_bytes: 256 * 1_024,
            max_snapshot_bytes: 2 * 1_024 * 1_024,
            max_event_ttl_ms: 7 * MILLIS_PER_DAY,
            max_snapshot_ttl_ms: 30 * MILLIS_PER_DAY,
            max_page_size: 200,
            max_push_token_bytes: 4_096,
            lease_ms: 30_000,
            retry_base_ms: 500,
            retry_cap_ms: 15 * 60 * 1_000,
            max_delivery_attempts: 10,
            installation_receipt_ttl_ms: 90 * MILLIS_PER_DAY,
            tombstone_retention_ms: 90 * MILLIS_PER_DAY,
        }
    }
}

impl StoreLimits {
    pub fn validate(&self) -> Result<()> {
        validate_limits(self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaultPoint {
    AfterInstallationInsert,
    AfterInstallationReceiptInsert,
    BeforeEventInsert,
    AfterEventInsert,
    AfterOutboxUpsert,
    BeforeCommit,
}

pub trait FaultInjector: Send + Sync {
    fn check(&self, point: FaultPoint) -> Result<()>;
}

#[derive(Default)]
pub struct NoFaults;

impl FaultInjector for NoFaults {
    fn check(&self, _point: FaultPoint) -> Result<()> {
        Ok(())
    }
}

#[derive(Clone)]
pub struct RelayStore {
    path: Arc<PathBuf>,
    cipher: TokenCipher,
    limits: StoreLimits,
    faults: Arc<dyn FaultInjector>,
    metrics: Arc<RelayMetrics>,
}

impl std::fmt::Debug for RelayStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RelayStore")
            .field("path", &"[local database path redacted]")
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct OutboxLease {
    pub outbox_id: OpaqueId,
    pub lease_id: OpaqueId,
    pub generation: u64,
    pub registration_generation: u64,
    pub provider: PushProviderKind,
    pub environment: PushEnvironment,
    pub token: SecretString,
    pub hint: OpaqueWakeHint,
    pub attempt: u32,
}

impl std::fmt::Debug for OutboxLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OutboxLease")
            .field("outbox_id", &self.outbox_id)
            .field("lease_id", &self.lease_id)
            .field("generation", &self.generation)
            .field("registration_generation", &self.registration_generation)
            .field("provider", &self.provider)
            .field("environment", &self.environment)
            .field("token", &"[redacted]")
            .field("hint", &"[opaque]")
            .field("attempt", &self.attempt)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryOutcome {
    Accepted,
    Suppressed,
    InvalidToken,
    Retry { retry_after_ms: Option<i64> },
    PermanentFailure,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoreDiagnostics {
    pub active_installations: u64,
    pub tombstoned_installations: u64,
    pub retained_events: u64,
    pub active_registrations: u64,
    pub pending_outbox: u64,
    pub leased_outbox: u64,
    pub dead_letter_outbox: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MaintenanceResult {
    pub expired_events: u64,
    pub expired_snapshots: u64,
    pub expired_outbox: u64,
    pub expired_installation_receipts: u64,
    pub purged_tombstones: u64,
}

impl RelayStore {
    pub fn open(
        path: impl AsRef<Path>,
        cipher: TokenCipher,
        limits: StoreLimits,
        metrics: Arc<RelayMetrics>,
    ) -> Result<Self> {
        Self::open_with_faults(path, cipher, limits, metrics, Arc::new(NoFaults))
    }

    pub fn open_with_faults(
        path: impl AsRef<Path>,
        cipher: TokenCipher,
        limits: StoreLimits,
        metrics: Arc<RelayMetrics>,
        faults: Arc<dyn FaultInjector>,
    ) -> Result<Self> {
        validate_limits(&limits)?;
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent)?;
        }
        let store = Self {
            path: Arc::new(path.as_ref().to_path_buf()),
            cipher,
            limits,
            faults,
            metrics,
        };
        let connection = store.connection()?;
        migrate(&connection)?;
        Ok(store)
    }

    pub fn limits(&self) -> &StoreLimits {
        &self.limits
    }

    fn connection(&self) -> Result<Connection> {
        let connection = Connection::open(&*self.path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        Ok(connection)
    }

    pub fn ready(&self) -> bool {
        self.connection()
            .and_then(|connection| {
                connection.query_row("SELECT 1", [], |_| Ok(()))?;
                Ok(())
            })
            .is_ok()
    }

    pub fn create_installation(
        &self,
        request: &CreateInstallationRequest,
        now_ms: i64,
    ) -> Result<IssuedInstallation> {
        let key_hash = installation_idempotency_hash(request.idempotency_key.as_str());
        let request_digest = installation_request_digest(request);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let receipt = transaction
            .query_row(
                "SELECT r.request_digest, r.installation_id, r.response_nonce,
                        r.response_ciphertext, r.response_expires_at_ms,
                        i.id, i.tombstoned_at_ms
                 FROM installation_receipts r
                 LEFT JOIN installations i ON i.id = r.installation_id
                 WHERE r.idempotency_key_hash = ?1",
                [key_hash.as_slice()],
                |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<Vec<u8>>>(2)?,
                        row.get::<_, Option<Vec<u8>>>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<i64>>(6)?,
                    ))
                },
            )
            .optional()?;
        if let Some((
            stored_digest,
            installation_id,
            nonce,
            ciphertext,
            response_expires_at_ms,
            active_installation_id,
            tombstoned_at_ms,
        )) = receipt
        {
            if !constant_time_equal(&stored_digest, &request_digest) {
                self.metrics
                    .installation_create_conflicts
                    .fetch_add(1, Ordering::Relaxed);
                return Err(RelayError::Conflict);
            }
            let (Some(_), None, Some(nonce), Some(ciphertext)) =
                (active_installation_id, tombstoned_at_ms, nonce, ciphertext)
            else {
                return Err(RelayError::Tombstoned);
            };
            if response_expires_at_ms <= now_ms {
                return Err(RelayError::Tombstoned);
            }
            let installation_id = OpaqueId::parse(installation_id)?;
            let nonce: [u8; 24] = nonce.try_into().map_err(|_| RelayError::Crypto)?;
            let issued = open_installation_receipt(
                &self.cipher,
                &key_hash,
                &request_digest,
                installation_id,
                SealedToken { nonce, ciphertext },
            )?;
            transaction.commit()?;
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

        transaction.execute(
            "INSERT INTO installations (
                id, write_capability_hash, read_capability_hash, manage_capability_hash,
                next_sequence, replay_floor, acknowledged_through, created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, 1, 1, 0, ?5, ?5)",
            params![
                issued.installation_id.as_str(),
                write_hash.as_slice(),
                read_hash.as_slice(),
                manage_hash.as_slice(),
                now_ms
            ],
        )?;
        self.faults.check(FaultPoint::AfterInstallationInsert)?;
        transaction.execute(
            "INSERT INTO installation_receipts (
                idempotency_key_hash, request_digest, installation_id,
                response_nonce, response_ciphertext, response_expires_at_ms, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                key_hash.as_slice(),
                request_digest.as_slice(),
                issued.installation_id.as_str(),
                sealed.nonce.as_slice(),
                sealed.ciphertext,
                response_expires_at_ms,
                now_ms
            ],
        )?;
        self.faults
            .check(FaultPoint::AfterInstallationReceiptInsert)?;
        transaction.commit()?;
        self.metrics
            .installations_created
            .fetch_add(1, Ordering::Relaxed);
        Ok(issued)
    }

    pub fn ingest_event(
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

        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        authorize(
            &transaction,
            installation_id,
            CapabilityKind::Write,
            capability,
        )?;

        if let Some((cursor, stored_digest)) = transaction
            .query_row(
                "SELECT sequence, request_digest FROM event_receipts
                 WHERE installation_id = ?1 AND event_id = ?2",
                params![installation_id.as_str(), request.event_id.as_str()],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
            )
            .optional()?
        {
            if stored_digest == digest {
                transaction.commit()?;
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

        self.faults.check(FaultPoint::BeforeEventInsert)?;
        let cursor: i64 = transaction.query_row(
            "SELECT next_sequence FROM installations WHERE id = ?1",
            [installation_id.as_str()],
            |row| row.get(0),
        )?;
        let next_cursor = cursor.checked_add(1).ok_or(RelayError::LimitExceeded)?;
        transaction.execute(
            "UPDATE installations SET next_sequence = ?2, updated_at_ms = ?3 WHERE id = ?1",
            params![installation_id.as_str(), next_cursor, now_ms],
        )?;
        transaction.execute(
            "INSERT INTO event_receipts (
                installation_id, event_id, request_digest, sequence, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                installation_id.as_str(),
                request.event_id.as_str(),
                digest.as_slice(),
                cursor,
                now_ms
            ],
        )?;
        transaction.execute(
            "INSERT INTO events (
                installation_id, sequence, event_id, request_digest, event_class,
                expires_at_ms, ciphertext, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                installation_id.as_str(),
                cursor,
                request.event_id.as_str(),
                digest.as_slice(),
                request.event_class.as_str(),
                request.expires_at_ms,
                ciphertext,
                now_ms
            ],
        )?;
        self.faults.check(FaultPoint::AfterEventInsert)?;

        if let Some((snapshot, snapshot_ciphertext)) = snapshot {
            apply_snapshot(
                &transaction,
                installation_id,
                snapshot.revision,
                cursor,
                snapshot.expires_at_ms,
                &snapshot_ciphertext,
                now_ms,
            )?;
        }

        let coalesced = upsert_outbox(&transaction, installation_id, request, cursor, now_ms)?;
        self.faults.check(FaultPoint::AfterOutboxUpsert)?;
        self.faults.check(FaultPoint::BeforeCommit)?;
        transaction.commit()?;

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

    pub fn fetch_events(
        &self,
        installation_id: &OpaqueId,
        capability: &PresentedCapability,
        after: u64,
        requested_limit: u32,
        now_ms: i64,
    ) -> Result<EventPage> {
        let connection = self.connection()?;
        authorize(
            &connection,
            installation_id,
            CapabilityKind::Read,
            capability,
        )?;
        let (next_sequence, replay_floor): (i64, i64) = connection.query_row(
            "SELECT next_sequence, replay_floor FROM installations WHERE id = ?1",
            [installation_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let high_watermark = to_u64(next_sequence.saturating_sub(1))?;
        if after > high_watermark {
            return Err(RelayError::Invalid("cursor beyond high watermark"));
        }
        let replay_floor = to_u64(replay_floor)?;
        let limit = requested_limit.clamp(1, self.limits.max_page_size);
        let mut statement = connection.prepare(
            "SELECT sequence, event_id, event_class, expires_at_ms, ciphertext
             FROM events
             WHERE installation_id = ?1 AND sequence > ?2 AND expires_at_ms > ?3
             ORDER BY sequence ASC LIMIT ?4",
        )?;
        let rows = statement.query_map(
            params![installation_id.as_str(), to_i64(after)?, now_ms, limit],
            |row| {
                let event_class: String = row.get(2)?;
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    event_class,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                ))
            },
        )?;

        let mut events = Vec::new();
        for row in rows {
            let (cursor, event_id, event_class, expires_at_ms, ciphertext) = row?;
            events.push(EventEnvelope {
                event_id: OpaqueId::parse(event_id)?,
                cursor: to_u64(cursor)?,
                event_class: EventClass::from_str(&event_class)?,
                expires_at_ms,
                ciphertext: URL_SAFE_NO_PAD.encode(ciphertext),
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

        let snapshot_available = connection.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM snapshots
                WHERE installation_id = ?1 AND expires_at_ms > ?2
             )",
            params![installation_id.as_str(), now_ms],
            |row| row.get::<_, bool>(0),
        )?;
        let next_cursor = events.last().map_or(after, |event| event.cursor);
        Ok(EventPage {
            schema_version: SCHEMA_VERSION,
            requested_after: after,
            next_cursor,
            high_watermark,
            replay_floor,
            reset_required,
            snapshot_available,
            events,
        })
    }

    pub fn acknowledge(
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
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        authorize(
            &transaction,
            installation_id,
            CapabilityKind::Read,
            capability,
        )?;
        let (next_sequence, acknowledged_through): (i64, i64) = transaction.query_row(
            "SELECT next_sequence, acknowledged_through FROM installations WHERE id = ?1",
            [installation_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let high_watermark = next_sequence.saturating_sub(1);
        if through_cursor > high_watermark {
            return Err(RelayError::Invalid("cursor beyond high watermark"));
        }
        let replayed = through_cursor <= acknowledged_through;
        let current = acknowledged_through.max(through_cursor);
        if !replayed {
            transaction.execute(
                "UPDATE installations SET acknowledged_through = ?2, updated_at_ms = ?3
                 WHERE id = ?1",
                params![installation_id.as_str(), current, now_ms],
            )?;
        }
        transaction.commit()?;
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

    pub fn fetch_snapshot(
        &self,
        installation_id: &OpaqueId,
        capability: &PresentedCapability,
        now_ms: i64,
    ) -> Result<SnapshotEnvelope> {
        let connection = self.connection()?;
        authorize(
            &connection,
            installation_id,
            CapabilityKind::Read,
            capability,
        )?;
        connection
            .query_row(
                "SELECT revision, through_sequence, expires_at_ms, ciphertext
                 FROM snapshots WHERE installation_id = ?1 AND expires_at_ms > ?2",
                params![installation_id.as_str(), now_ms],
                |row| {
                    let revision: i64 = row.get(0)?;
                    let through: i64 = row.get(1)?;
                    let expires_at_ms: i64 = row.get(2)?;
                    let ciphertext: Vec<u8> = row.get(3)?;
                    Ok((revision, through, expires_at_ms, ciphertext))
                },
            )
            .optional()?
            .map(|(revision, through, expires_at_ms, ciphertext)| {
                Ok::<SnapshotEnvelope, RelayError>(SnapshotEnvelope {
                    schema_version: SCHEMA_VERSION,
                    revision: to_u64(revision)?,
                    through_cursor: to_u64(through)?,
                    expires_at_ms,
                    ciphertext: URL_SAFE_NO_PAD.encode(ciphertext),
                })
            })
            .transpose()?
            .ok_or(RelayError::NotFound)
    }

    pub fn register_device(
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
        let token_hash = push_token_hash(installation_id, provider, environment, token);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        authorize(
            &transaction,
            installation_id,
            CapabilityKind::Manage,
            capability,
        )?;
        let existing: Option<(String, i64, Vec<u8>, Option<i64>)> = transaction
            .query_row(
                "SELECT id, generation, token_hash, tombstoned_at_ms FROM device_registrations
                 WHERE installation_id = ?1 AND provider = ?2 AND environment = ?3",
                params![
                    installation_id.as_str(),
                    provider.as_str(),
                    environment.as_str()
                ],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let (registration_id, generation, replaced, created) = match existing {
            Some((id, generation, stored_hash, tombstoned_at)) => {
                let same_active = stored_hash == token_hash && tombstoned_at.is_none();
                (
                    OpaqueId::parse(id)?,
                    if same_active {
                        generation
                    } else {
                        generation.checked_add(1).ok_or(RelayError::LimitExceeded)?
                    },
                    !same_active,
                    false,
                )
            }
            None => (OpaqueId::random("dev"), 1, false, true),
        };
        let aad = token_aad(installation_id, &registration_id, provider, environment);
        let sealed = self.cipher.seal(token.as_bytes(), &aad)?;
        transaction.execute(
            "INSERT INTO device_registrations (
                id, installation_id, provider, environment, token_hash, token_nonce,
                token_ciphertext, generation, created_at_ms, updated_at_ms, tombstoned_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, NULL)
             ON CONFLICT(installation_id, provider, environment) DO UPDATE SET
                token_hash = excluded.token_hash,
                token_nonce = excluded.token_nonce,
                token_ciphertext = excluded.token_ciphertext,
                generation = excluded.generation,
                updated_at_ms = excluded.updated_at_ms,
                tombstoned_at_ms = NULL",
            params![
                registration_id.as_str(),
                installation_id.as_str(),
                provider.as_str(),
                environment.as_str(),
                token_hash.as_slice(),
                sealed.nonce.as_slice(),
                sealed.ciphertext,
                generation,
                now_ms
            ],
        )?;
        if replaced {
            transaction.execute(
                "UPDATE push_outbox SET generation = generation + 1,
                 registration_generation = ?2, state = 'pending', attempt_count = 0,
                 next_attempt_at_ms = ?3, lease_id = NULL, lease_until_ms = NULL,
                 last_error_class = NULL,
                 updated_at_ms = ?3 WHERE registration_id = ?1",
                params![registration_id.as_str(), generation, now_ms],
            )?;
        }
        transaction.commit()?;
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

    pub fn tombstone_registration(
        &self,
        installation_id: &OpaqueId,
        capability: &PresentedCapability,
        registration_id: &OpaqueId,
        through_generation: u64,
        now_ms: i64,
    ) -> Result<()> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        authorize(
            &transaction,
            installation_id,
            CapabilityKind::Manage,
            capability,
        )?;
        let changed = transaction.execute(
            "UPDATE device_registrations
             SET tombstoned_at_ms = COALESCE(tombstoned_at_ms, ?3), updated_at_ms = ?3
             WHERE id = ?1 AND installation_id = ?2 AND generation <= ?4",
            params![
                registration_id.as_str(),
                installation_id.as_str(),
                now_ms,
                to_i64(through_generation)?
            ],
        )?;
        if changed == 0 {
            transaction.commit()?;
            return Ok(());
        }
        transaction.execute(
            "UPDATE push_outbox SET state = 'tombstoned', lease_id = NULL, lease_until_ms = NULL,
             updated_at_ms = ?2 WHERE registration_id = ?1",
            params![registration_id.as_str(), now_ms],
        )?;
        transaction.commit()?;
        self.metrics
            .registrations_tombstoned
            .fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    pub fn tombstone_installation(
        &self,
        installation_id: &OpaqueId,
        capability: &PresentedCapability,
        now_ms: i64,
    ) -> Result<()> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        authorize(
            &transaction,
            installation_id,
            CapabilityKind::Manage,
            capability,
        )?;
        transaction.execute(
            "UPDATE installations SET tombstoned_at_ms = COALESCE(tombstoned_at_ms, ?2),
             updated_at_ms = ?2 WHERE id = ?1",
            params![installation_id.as_str(), now_ms],
        )?;
        transaction.execute(
            "UPDATE device_registrations SET tombstoned_at_ms = COALESCE(tombstoned_at_ms, ?2),
             updated_at_ms = ?2 WHERE installation_id = ?1",
            params![installation_id.as_str(), now_ms],
        )?;
        transaction.execute(
            "UPDATE push_outbox SET state = 'tombstoned', lease_id = NULL, lease_until_ms = NULL,
             updated_at_ms = ?2 WHERE installation_id = ?1",
            params![installation_id.as_str(), now_ms],
        )?;
        transaction.execute(
            "UPDATE installation_receipts SET response_nonce = NULL, response_ciphertext = NULL
             WHERE installation_id = ?1",
            [installation_id.as_str()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn lease_deliveries(&self, now_ms: i64, limit: u32) -> Result<Vec<OutboxLease>> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let expired = transaction.execute(
            "UPDATE push_outbox SET state = 'expired', lease_id = NULL,
             lease_until_ms = NULL, updated_at_ms = ?1
             WHERE state IN ('pending', 'leased') AND expires_at_ms <= ?1",
            [now_ms],
        )?;
        let recovered = transaction.execute(
            "UPDATE push_outbox SET state = 'pending', lease_id = NULL, lease_until_ms = NULL,
             next_attempt_at_ms = ?1, updated_at_ms = ?1
             WHERE state = 'leased' AND lease_until_ms <= ?1 AND expires_at_ms > ?1",
            [now_ms],
        )?;

        let mut statement = transaction.prepare(
            "SELECT o.id, o.generation, o.installation_id, o.event_id, o.cursor,
                    o.event_class, o.expires_at_ms, o.attempt_count, o.registration_generation,
                    r.id, r.provider, r.environment, r.token_nonce, r.token_ciphertext
             FROM push_outbox o
             JOIN device_registrations r ON r.id = o.registration_id
             JOIN installations i ON i.id = o.installation_id
             WHERE o.state = 'pending' AND o.next_attempt_at_ms <= ?1
               AND o.expires_at_ms > ?1
               AND r.tombstoned_at_ms IS NULL AND i.tombstoned_at_ms IS NULL
             ORDER BY o.next_attempt_at_ms ASC, o.created_at_ms ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![now_ms, limit.clamp(1, 1_000)], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, String>(10)?,
                row.get::<_, String>(11)?,
                row.get::<_, Vec<u8>>(12)?,
                row.get::<_, Vec<u8>>(13)?,
            ))
        })?;
        let candidates: Vec<_> = rows.collect::<std::result::Result<_, _>>()?;
        drop(statement);

        let mut leases = Vec::with_capacity(candidates.len());
        for (
            outbox_id,
            generation,
            installation_id,
            event_id,
            cursor,
            event_class,
            expires_at_ms,
            attempts,
            registration_generation,
            registration_id,
            provider,
            environment,
            nonce,
            ciphertext,
        ) in candidates
        {
            let lease_id = OpaqueId::random("lease");
            let changed = transaction.execute(
                "UPDATE push_outbox SET state = 'leased', lease_id = ?5, lease_until_ms = ?3,
                 attempt_count = attempt_count + 1, updated_at_ms = ?2
                 WHERE id = ?1 AND generation = ?4 AND state = 'pending'",
                params![
                    outbox_id,
                    now_ms,
                    now_ms.saturating_add(self.limits.lease_ms),
                    generation,
                    lease_id.as_str()
                ],
            )?;
            if changed == 0 {
                continue;
            }
            let installation_id = OpaqueId::parse(installation_id)?;
            let registration_id = OpaqueId::parse(registration_id)?;
            let provider = PushProviderKind::from_str(&provider)?;
            let environment = PushEnvironment::from_str(&environment)?;
            let nonce: [u8; 24] = nonce.try_into().map_err(|_| RelayError::Crypto)?;
            let sealed = SealedToken { nonce, ciphertext };
            let aad = token_aad(&installation_id, &registration_id, provider, environment);
            let plaintext = self.cipher.open(&sealed, &aad)?;
            let token = String::from_utf8(plaintext.to_vec()).map_err(|_| RelayError::Crypto)?;
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
                    event_id: OpaqueId::parse(event_id)?,
                    cursor: to_u64(cursor)?,
                    event_class: EventClass::from_str(&event_class)?,
                    expires_at_ms,
                },
                attempt: u32::try_from(attempts.saturating_add(1))
                    .map_err(|_| RelayError::LimitExceeded)?,
            });
        }
        transaction.commit()?;
        let _ = expired;
        self.metrics
            .outbox_leases_recovered
            .fetch_add(recovered as u64, Ordering::Relaxed);
        Ok(leases)
    }

    pub fn complete_delivery(
        &self,
        outbox_id: &OpaqueId,
        generation: u64,
        registration_generation: u64,
        lease_id: &OpaqueId,
        outcome: DeliveryOutcome,
        now_ms: i64,
    ) -> Result<()> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row = transaction
            .query_row(
                "SELECT generation, registration_id, registration_generation, attempt_count,
                        expires_at_ms, state, lease_id
                 FROM push_outbox WHERE id = ?1",
                [outbox_id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, Option<String>>(6)?,
                    ))
                },
            )
            .optional()?
            .ok_or(RelayError::NotFound)?;
        let (
            current_generation,
            registration_id,
            current_registration_generation,
            attempts,
            expires_at_ms,
            state,
            current_lease_id,
        ) = row;

        // Completion is valid only for the current physical lease. Intent and
        // registration generations fence logical replacement; the random
        // lease identifier fences lease expiry/recovery and another worker's
        // later lease of the same intent.
        if state != "leased" || current_lease_id.as_deref() != Some(lease_id.as_str()) {
            transaction.commit()?;
            return Ok(());
        }

        if outcome == DeliveryOutcome::InvalidToken {
            if current_registration_generation != to_i64(registration_generation)? {
                transaction.execute(
                    "UPDATE push_outbox SET state = 'pending', lease_id = NULL,
                     lease_until_ms = NULL, attempt_count = 0,
                     next_attempt_at_ms = ?2, updated_at_ms = ?2 WHERE id = ?1",
                    params![outbox_id.as_str(), now_ms],
                )?;
                transaction.commit()?;
                return Ok(());
            }
            let changed = transaction.execute(
                "UPDATE device_registrations SET tombstoned_at_ms = COALESCE(tombstoned_at_ms, ?2),
                 updated_at_ms = ?2 WHERE id = ?1 AND generation = ?3",
                params![registration_id, now_ms, current_registration_generation],
            )?;
            if changed > 0 {
                transaction.execute(
                    "UPDATE push_outbox SET state = 'tombstoned', lease_id = NULL,
                     lease_until_ms = NULL,
                 updated_at_ms = ?2 WHERE registration_id = ?1",
                    params![registration_id, now_ms],
                )?;
            }
            transaction.commit()?;
            self.metrics
                .push_invalid_tokens
                .fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }

        if current_generation != to_i64(generation)? {
            transaction.execute(
                "UPDATE push_outbox SET state = 'pending', lease_id = NULL,
                 lease_until_ms = NULL, attempt_count = 0,
                 next_attempt_at_ms = ?2, updated_at_ms = ?2 WHERE id = ?1",
                params![outbox_id.as_str(), now_ms],
            )?;
            transaction.commit()?;
            return Ok(());
        }

        match outcome {
            DeliveryOutcome::Accepted => {
                set_outbox_terminal(&transaction, outbox_id, "delivered", now_ms)?;
                self.metrics.push_accepted.fetch_add(1, Ordering::Relaxed);
            }
            DeliveryOutcome::Suppressed => {
                set_outbox_terminal(&transaction, outbox_id, "suppressed", now_ms)?;
                self.metrics.push_suppressed.fetch_add(1, Ordering::Relaxed);
            }
            DeliveryOutcome::PermanentFailure => {
                set_outbox_terminal(&transaction, outbox_id, "dead_letter", now_ms)?;
                self.metrics
                    .push_dead_lettered
                    .fetch_add(1, Ordering::Relaxed);
            }
            DeliveryOutcome::Retry { retry_after_ms } => {
                if attempts >= i64::from(self.limits.max_delivery_attempts)
                    || expires_at_ms <= now_ms
                {
                    let state = if expires_at_ms <= now_ms {
                        "expired"
                    } else {
                        "dead_letter"
                    };
                    set_outbox_terminal(&transaction, outbox_id, state, now_ms)?;
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
                    transaction.execute(
                        "UPDATE push_outbox SET state = 'pending', lease_id = NULL,
                         lease_until_ms = NULL,
                         next_attempt_at_ms = ?2, updated_at_ms = ?3,
                         last_error_class = 'transient' WHERE id = ?1",
                        params![outbox_id.as_str(), now_ms.saturating_add(delay), now_ms],
                    )?;
                    self.metrics.push_retried.fetch_add(1, Ordering::Relaxed);
                }
            }
            DeliveryOutcome::InvalidToken => unreachable!(),
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn maintenance(&self, now_ms: i64) -> Result<MaintenanceResult> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut result = MaintenanceResult::default();

        let installation_ids = {
            let mut statement = transaction.prepare(
                "SELECT id, replay_floor FROM installations WHERE tombstoned_at_ms IS NULL",
            )?;
            statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        for (installation_id, floor) in installation_ids {
            let mut next_floor = floor;
            let mut statement = transaction.prepare(
                "SELECT sequence, expires_at_ms FROM events
                 WHERE installation_id = ?1 AND sequence >= ?2 ORDER BY sequence ASC",
            )?;
            let rows = statement.query_map(params![installation_id, floor], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })?;
            for row in rows {
                let (sequence, expires_at_ms) = row?;
                if sequence != next_floor || expires_at_ms > now_ms {
                    break;
                }
                next_floor = next_floor.saturating_add(1);
            }
            drop(statement);
            if next_floor > floor {
                result.expired_events += transaction.execute(
                    "DELETE FROM events WHERE installation_id = ?1 AND sequence < ?2",
                    params![installation_id, next_floor],
                )? as u64;
                transaction.execute(
                    "UPDATE installations SET replay_floor = ?2, updated_at_ms = ?3 WHERE id = ?1",
                    params![installation_id, next_floor, now_ms],
                )?;
            }
        }

        result.expired_snapshots = transaction
            .execute("DELETE FROM snapshots WHERE expires_at_ms <= ?1", [now_ms])?
            as u64;
        result.expired_outbox = transaction.execute(
            "UPDATE push_outbox SET state = 'expired', lease_id = NULL,
             lease_until_ms = NULL, updated_at_ms = ?1
             WHERE state IN ('pending', 'leased') AND expires_at_ms <= ?1",
            [now_ms],
        )? as u64;
        result.expired_installation_receipts = transaction.execute(
            "UPDATE installation_receipts
             SET response_nonce = NULL, response_ciphertext = NULL
             WHERE response_ciphertext IS NOT NULL AND response_expires_at_ms <= ?1",
            [now_ms],
        )? as u64;
        result.purged_tombstones = transaction.execute(
            "DELETE FROM installations WHERE tombstoned_at_ms IS NOT NULL
             AND tombstoned_at_ms <= ?1",
            [now_ms.saturating_sub(self.limits.tombstone_retention_ms)],
        )? as u64;
        transaction.commit()?;
        self.metrics
            .maintenance_expired_events
            .fetch_add(result.expired_events, Ordering::Relaxed);
        Ok(result)
    }

    pub fn diagnostics(&self) -> Result<StoreDiagnostics> {
        let connection = self.connection()?;
        let count = |sql: &str| -> Result<u64> {
            let value: i64 = connection.query_row(sql, [], |row| row.get(0))?;
            to_u64(value)
        };
        Ok(StoreDiagnostics {
            active_installations: count(
                "SELECT COUNT(*) FROM installations WHERE tombstoned_at_ms IS NULL",
            )?,
            tombstoned_installations: count(
                "SELECT COUNT(*) FROM installations WHERE tombstoned_at_ms IS NOT NULL",
            )?,
            retained_events: count("SELECT COUNT(*) FROM events")?,
            active_registrations: count(
                "SELECT COUNT(*) FROM device_registrations WHERE tombstoned_at_ms IS NULL",
            )?,
            pending_outbox: count("SELECT COUNT(*) FROM push_outbox WHERE state = 'pending'")?,
            leased_outbox: count("SELECT COUNT(*) FROM push_outbox WHERE state = 'leased'")?,
            dead_letter_outbox: count(
                "SELECT COUNT(*) FROM push_outbox WHERE state = 'dead_letter'",
            )?,
        })
    }
}

fn validate_limits(limits: &StoreLimits) -> Result<()> {
    if limits.max_event_bytes < 16
        || limits.max_snapshot_bytes < limits.max_event_bytes
        || limits.max_event_ttl_ms <= 0
        || limits.max_snapshot_ttl_ms < limits.max_event_ttl_ms
        || limits.max_page_size == 0
        || limits.max_push_token_bytes < 64
        || limits.lease_ms <= 0
        || limits.retry_base_ms <= 0
        || limits.retry_cap_ms < limits.retry_base_ms
        || limits.max_delivery_attempts == 0
        || limits.installation_receipt_ttl_ms <= 0
        || limits.installation_receipt_ttl_ms > 365 * MILLIS_PER_DAY
        || limits.tombstone_retention_ms < limits.max_snapshot_ttl_ms
    {
        return Err(RelayError::Configuration(
            "store limits are inconsistent or unsafe".into(),
        ));
    }
    Ok(())
}

fn migrate(connection: &Connection) -> Result<()> {
    let current_version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if current_version > 4 {
        return Err(RelayError::Configuration(
            "SQLite schema is newer than this relay binary".into(),
        ));
    }
    connection.execute_batch(
        "BEGIN IMMEDIATE;
         CREATE TABLE IF NOT EXISTS installations (
            id TEXT PRIMARY KEY,
            write_capability_hash BLOB NOT NULL,
            read_capability_hash BLOB NOT NULL,
            manage_capability_hash BLOB NOT NULL,
            next_sequence INTEGER NOT NULL CHECK(next_sequence >= 1),
            replay_floor INTEGER NOT NULL CHECK(replay_floor >= 1),
            acknowledged_through INTEGER NOT NULL DEFAULT 0 CHECK(acknowledged_through >= 0),
            created_at_ms INTEGER NOT NULL,
            updated_at_ms INTEGER NOT NULL,
            tombstoned_at_ms INTEGER,
            CHECK(acknowledged_through < next_sequence)
         ) STRICT;
         CREATE TABLE IF NOT EXISTS installation_receipts (
            idempotency_key_hash BLOB PRIMARY KEY CHECK(length(idempotency_key_hash) = 32),
            request_digest BLOB NOT NULL CHECK(length(request_digest) = 32),
            installation_id TEXT NOT NULL,
            response_nonce BLOB CHECK(response_nonce IS NULL OR length(response_nonce) = 24),
            response_ciphertext BLOB,
            response_expires_at_ms INTEGER NOT NULL,
            created_at_ms INTEGER NOT NULL,
            CHECK((response_nonce IS NULL) = (response_ciphertext IS NULL))
         ) STRICT;
         CREATE INDEX IF NOT EXISTS installation_receipts_installation_idx
            ON installation_receipts(installation_id);
         CREATE TABLE IF NOT EXISTS events (
            installation_id TEXT NOT NULL REFERENCES installations(id) ON DELETE CASCADE,
            sequence INTEGER NOT NULL CHECK(sequence >= 1),
            event_id TEXT NOT NULL,
            request_digest BLOB NOT NULL CHECK(length(request_digest) = 32),
            event_class TEXT NOT NULL CHECK(event_class IN (
                'state_changed', 'activity_changed', 'connection_changed', 'security_changed'
            )),
            expires_at_ms INTEGER NOT NULL,
            ciphertext BLOB NOT NULL,
            created_at_ms INTEGER NOT NULL,
            PRIMARY KEY (installation_id, sequence),
            UNIQUE (installation_id, event_id)
         ) STRICT;
         CREATE INDEX IF NOT EXISTS events_expiry_idx ON events(expires_at_ms);
         CREATE TABLE IF NOT EXISTS event_receipts (
            installation_id TEXT NOT NULL REFERENCES installations(id) ON DELETE CASCADE,
            event_id TEXT NOT NULL,
            request_digest BLOB NOT NULL CHECK(length(request_digest) = 32),
            sequence INTEGER NOT NULL CHECK(sequence >= 1),
            created_at_ms INTEGER NOT NULL,
            PRIMARY KEY (installation_id, event_id)
         ) STRICT;
         INSERT OR IGNORE INTO event_receipts (
            installation_id, event_id, request_digest, sequence, created_at_ms
         ) SELECT installation_id, event_id, request_digest, sequence, created_at_ms FROM events;
         CREATE TABLE IF NOT EXISTS snapshots (
            installation_id TEXT PRIMARY KEY REFERENCES installations(id) ON DELETE CASCADE,
            revision INTEGER NOT NULL CHECK(revision >= 1),
            through_sequence INTEGER NOT NULL CHECK(through_sequence >= 1),
            snapshot_digest BLOB NOT NULL CHECK(length(snapshot_digest) = 32),
            expires_at_ms INTEGER NOT NULL,
            ciphertext BLOB NOT NULL,
            updated_at_ms INTEGER NOT NULL
         ) STRICT;
         CREATE TABLE IF NOT EXISTS device_registrations (
            id TEXT PRIMARY KEY,
            installation_id TEXT NOT NULL REFERENCES installations(id) ON DELETE CASCADE,
            provider TEXT NOT NULL CHECK(provider IN ('apns', 'fcm')),
            environment TEXT NOT NULL CHECK(environment IN ('sandbox', 'production')),
            token_hash BLOB NOT NULL CHECK(length(token_hash) = 32),
            token_nonce BLOB NOT NULL CHECK(length(token_nonce) = 24),
            token_ciphertext BLOB NOT NULL,
            created_at_ms INTEGER NOT NULL,
            updated_at_ms INTEGER NOT NULL,
            tombstoned_at_ms INTEGER,
            generation INTEGER NOT NULL CHECK(generation >= 1),
            UNIQUE (installation_id, provider, environment),
            CHECK (provider <> 'fcm' OR environment = 'production')
         ) STRICT;
         CREATE TABLE IF NOT EXISTS push_outbox (
            id TEXT PRIMARY KEY,
            installation_id TEXT NOT NULL REFERENCES installations(id) ON DELETE CASCADE,
            registration_id TEXT NOT NULL REFERENCES device_registrations(id) ON DELETE CASCADE,
            event_id TEXT NOT NULL,
            cursor INTEGER NOT NULL CHECK(cursor >= 1),
            event_class TEXT NOT NULL CHECK(event_class IN (
                'state_changed', 'activity_changed', 'connection_changed', 'security_changed'
            )),
            expires_at_ms INTEGER NOT NULL,
            generation INTEGER NOT NULL CHECK(generation >= 1),
            registration_generation INTEGER NOT NULL CHECK(registration_generation >= 1),
            attempt_count INTEGER NOT NULL DEFAULT 0 CHECK(attempt_count >= 0),
            next_attempt_at_ms INTEGER NOT NULL,
            state TEXT NOT NULL CHECK(state IN (
                'pending', 'leased', 'delivered', 'suppressed', 'expired', 'dead_letter', 'tombstoned'
            )),
            lease_id TEXT,
            lease_until_ms INTEGER,
            last_error_class TEXT,
            created_at_ms INTEGER NOT NULL,
            updated_at_ms INTEGER NOT NULL,
            UNIQUE (registration_id, event_class)
         ) STRICT;
         CREATE INDEX IF NOT EXISTS outbox_due_idx
            ON push_outbox(state, next_attempt_at_ms, expires_at_ms);
         COMMIT;",
    )?;
    if !sqlite_has_column(connection, "push_outbox", "lease_id")? {
        connection.execute("ALTER TABLE push_outbox ADD COLUMN lease_id TEXT", [])?;
    }
    if !sqlite_has_column(connection, "installations", "acknowledged_through")? {
        connection.execute(
            "ALTER TABLE installations ADD COLUMN acknowledged_through INTEGER NOT NULL
             DEFAULT 0 CHECK(acknowledged_through >= 0)",
            [],
        )?;
    }
    connection.execute_batch(
        "CREATE TRIGGER IF NOT EXISTS installations_ack_bounds_insert
         BEFORE INSERT ON installations
         WHEN NEW.acknowledged_through < 0 OR NEW.acknowledged_through >= NEW.next_sequence
         BEGIN SELECT RAISE(ABORT, 'invalid acknowledgement cursor'); END;
         CREATE TRIGGER IF NOT EXISTS installations_ack_bounds_update
         BEFORE UPDATE OF acknowledged_through, next_sequence ON installations
         WHEN NEW.acknowledged_through < 0 OR NEW.acknowledged_through >= NEW.next_sequence
         BEGIN SELECT RAISE(ABORT, 'invalid acknowledgement cursor'); END;",
    )?;
    connection.pragma_update(None, "user_version", 4)?;
    Ok(())
}

fn sqlite_has_column(connection: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    Ok(statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .any(|candidate| candidate == column))
}

fn authorize(
    connection: &Connection,
    installation_id: &OpaqueId,
    kind: CapabilityKind,
    capability: &PresentedCapability,
) -> Result<()> {
    let column = match kind {
        CapabilityKind::Write => "write_capability_hash",
        CapabilityKind::Read => "read_capability_hash",
        CapabilityKind::Manage => "manage_capability_hash",
    };
    let sql = format!("SELECT {column}, tombstoned_at_ms FROM installations WHERE id = ?1");
    let row: Option<(Vec<u8>, Option<i64>)> = connection
        .query_row(&sql, [installation_id.as_str()], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()?;
    let Some((stored_hash, tombstoned_at)) = row else {
        return Err(RelayError::Unauthorized);
    };
    let presented_hash = capability_hash(kind, capability.expose());
    if !constant_time_equal(&stored_hash, &presented_hash) {
        return Err(RelayError::Unauthorized);
    }
    if tombstoned_at.is_some() {
        return Err(RelayError::Tombstoned);
    }
    Ok(())
}

fn capability_hash(kind: CapabilityKind, capability: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"remora-relay-capability-v1\0");
    hasher.update(match kind {
        CapabilityKind::Write => b"write" as &[u8],
        CapabilityKind::Read => b"read",
        CapabilityKind::Manage => b"manage",
    });
    hasher.update(b"\0");
    hasher.update(capability.as_bytes());
    hasher.finalize().into()
}

pub(crate) fn installation_idempotency_hash(idempotency_key: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"remora-relay-installation-idempotency-v1\0");
    hasher.update(idempotency_key.as_bytes());
    hasher.finalize().into()
}

pub(crate) fn installation_request_digest(request: &CreateInstallationRequest) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"remora-relay-create-installation-request-v1\0");
    hasher.update(request.schema_version.to_be_bytes());
    hasher.finalize().into()
}

pub(crate) fn installation_receipt_aad(
    key_hash: &[u8; 32],
    request_digest: &[u8; 32],
    installation_id: &OpaqueId,
) -> Vec<u8> {
    [
        b"remora-relay-installation-receipt-v1\0".as_slice(),
        key_hash,
        request_digest,
        installation_id.as_str().as_bytes(),
    ]
    .concat()
}

pub(crate) fn seal_installation_receipt(
    cipher: &TokenCipher,
    key_hash: &[u8; 32],
    request_digest: &[u8; 32],
    installation: &IssuedInstallation,
) -> Result<SealedToken> {
    let mut plaintext = zeroize::Zeroizing::new(Vec::with_capacity(3 * 48));
    for capability in [
        &installation.write_capability,
        &installation.read_capability,
        &installation.manage_capability,
    ] {
        let length = u16::try_from(capability.as_str().len()).map_err(|_| RelayError::Crypto)?;
        plaintext.extend_from_slice(&length.to_be_bytes());
        plaintext.extend_from_slice(capability.as_str().as_bytes());
    }
    cipher.seal(
        &plaintext,
        &installation_receipt_aad(key_hash, request_digest, &installation.installation_id),
    )
}

pub(crate) fn open_installation_receipt(
    cipher: &TokenCipher,
    key_hash: &[u8; 32],
    request_digest: &[u8; 32],
    installation_id: OpaqueId,
    sealed: SealedToken,
) -> Result<IssuedInstallation> {
    let plaintext = cipher.open(
        &sealed,
        &installation_receipt_aad(key_hash, request_digest, &installation_id),
    )?;
    let mut offset: usize = 0;
    let mut next_capability = || -> Result<IssuedCapability> {
        let length_bytes = plaintext
            .get(offset..offset.saturating_add(2))
            .ok_or(RelayError::Crypto)?;
        let length = usize::from(u16::from_be_bytes(
            length_bytes.try_into().map_err(|_| RelayError::Crypto)?,
        ));
        offset = offset.saturating_add(2);
        let value = plaintext
            .get(offset..offset.saturating_add(length))
            .ok_or(RelayError::Crypto)?;
        offset = offset.saturating_add(length);
        let value = String::from_utf8(value.to_vec()).map_err(|_| RelayError::Crypto)?;
        IssuedCapability::from_stored(value)
    };
    let write_capability = next_capability()?;
    let read_capability = next_capability()?;
    let manage_capability = next_capability()?;
    if offset != plaintext.len() {
        return Err(RelayError::Crypto);
    }
    Ok(IssuedInstallation {
        schema_version: SCHEMA_VERSION,
        installation_id,
        write_capability,
        read_capability,
        manage_capability,
        created: false,
    })
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn decode_ciphertext(encoded: &str, maximum: usize, field: &'static str) -> Result<Vec<u8>> {
    if encoded.len() > maximum.saturating_mul(2) {
        return Err(RelayError::LimitExceeded);
    }
    let decoded = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| RelayError::Invalid(field))?;
    if decoded.len() < 16 {
        return Err(RelayError::Invalid(field));
    }
    if decoded.len() > maximum {
        return Err(RelayError::LimitExceeded);
    }
    Ok(decoded)
}

fn validate_expiry(now_ms: i64, expires_at_ms: i64, maximum_ttl_ms: i64) -> Result<()> {
    if expires_at_ms <= now_ms
        || expires_at_ms
            > now_ms
                .checked_add(maximum_ttl_ms)
                .ok_or(RelayError::LimitExceeded)?
    {
        return Err(RelayError::Invalid("expiry"));
    }
    Ok(())
}

fn event_digest(
    request: &IngestEventRequest,
    ciphertext: &[u8],
    snapshot: Option<&(&crate::SnapshotUpdate, Vec<u8>)>,
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"remora-relay-event-v1\0");
    hasher.update(request.event_class.as_str().as_bytes());
    hasher.update(request.expires_at_ms.to_be_bytes());
    hasher.update((ciphertext.len() as u64).to_be_bytes());
    hasher.update(ciphertext);
    if let Some((snapshot, snapshot_ciphertext)) = snapshot {
        hasher.update([1]);
        hasher.update(snapshot.revision.to_be_bytes());
        hasher.update(snapshot.expires_at_ms.to_be_bytes());
        hasher.update((snapshot_ciphertext.len() as u64).to_be_bytes());
        hasher.update(snapshot_ciphertext);
    } else {
        hasher.update([0]);
    }
    hasher.finalize().into()
}

fn snapshot_digest(revision: u64, expires_at_ms: i64, ciphertext: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"remora-relay-snapshot-v1\0");
    hasher.update(revision.to_be_bytes());
    hasher.update(expires_at_ms.to_be_bytes());
    hasher.update(ciphertext);
    hasher.finalize().into()
}

fn apply_snapshot(
    transaction: &Transaction<'_>,
    installation_id: &OpaqueId,
    revision: u64,
    through_sequence: i64,
    expires_at_ms: i64,
    ciphertext: &[u8],
    now_ms: i64,
) -> Result<()> {
    let digest = snapshot_digest(revision, expires_at_ms, ciphertext);
    if let Some((current_revision, current_digest)) = transaction
        .query_row(
            "SELECT revision, snapshot_digest FROM snapshots WHERE installation_id = ?1",
            [installation_id.as_str()],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )
        .optional()?
    {
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
    transaction.execute(
        "INSERT INTO snapshots (
            installation_id, revision, through_sequence, snapshot_digest,
            expires_at_ms, ciphertext, updated_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(installation_id) DO UPDATE SET
            revision = excluded.revision,
            through_sequence = excluded.through_sequence,
            snapshot_digest = excluded.snapshot_digest,
            expires_at_ms = excluded.expires_at_ms,
            ciphertext = excluded.ciphertext,
            updated_at_ms = excluded.updated_at_ms",
        params![
            installation_id.as_str(),
            to_i64(revision)?,
            through_sequence,
            digest.as_slice(),
            expires_at_ms,
            ciphertext,
            now_ms
        ],
    )?;
    Ok(())
}

fn upsert_outbox(
    transaction: &Transaction<'_>,
    installation_id: &OpaqueId,
    request: &IngestEventRequest,
    cursor: i64,
    now_ms: i64,
) -> Result<u64> {
    let registrations = {
        let mut statement = transaction.prepare(
            "SELECT id, generation FROM device_registrations
             WHERE installation_id = ?1 AND tombstoned_at_ms IS NULL",
        )?;
        statement
            .query_map([installation_id.as_str()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    let mut coalesced = 0;
    for (registration_id, registration_generation) in registrations {
        let existed: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM push_outbox
             WHERE registration_id = ?1 AND event_class = ?2)",
            params![registration_id, request.event_class.as_str()],
            |row| row.get(0),
        )?;
        if existed {
            coalesced += 1;
        }
        let outbox_id = OpaqueId::random("wake");
        transaction.execute(
            "INSERT INTO push_outbox (
                id, installation_id, registration_id, event_id, cursor, event_class,
                expires_at_ms, generation, registration_generation, attempt_count, next_attempt_at_ms, state,
                lease_until_ms, created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8, 0, ?9, 'pending', NULL, ?9, ?9)
             ON CONFLICT(registration_id, event_class) DO UPDATE SET
                event_id = excluded.event_id,
                cursor = excluded.cursor,
                expires_at_ms = excluded.expires_at_ms,
                generation = push_outbox.generation + 1,
                registration_generation = excluded.registration_generation,
                attempt_count = 0,
                next_attempt_at_ms = CASE WHEN push_outbox.state = 'leased'
                    THEN push_outbox.next_attempt_at_ms ELSE excluded.next_attempt_at_ms END,
                state = CASE WHEN push_outbox.state = 'leased'
                    THEN 'leased' ELSE 'pending' END,
                lease_until_ms = CASE WHEN push_outbox.state = 'leased'
                    THEN push_outbox.lease_until_ms ELSE NULL END,
                lease_id = CASE WHEN push_outbox.state = 'leased'
                    THEN push_outbox.lease_id ELSE NULL END,
                last_error_class = NULL,
                updated_at_ms = excluded.updated_at_ms",
            params![
                outbox_id.as_str(),
                installation_id.as_str(),
                registration_id,
                request.event_id.as_str(),
                cursor,
                request.event_class.as_str(),
                request.expires_at_ms,
                registration_generation,
                now_ms
            ],
        )?;
    }
    Ok(coalesced)
}

fn push_token_hash(
    installation_id: &OpaqueId,
    provider: PushProviderKind,
    environment: PushEnvironment,
    token: &str,
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"remora-relay-push-token-v1\0");
    hasher.update(installation_id.as_str().as_bytes());
    hasher.update(b"\0");
    hasher.update(provider.as_str().as_bytes());
    hasher.update(b"\0");
    hasher.update(environment.as_str().as_bytes());
    hasher.update(b"\0");
    hasher.update(token.as_bytes());
    hasher.finalize().into()
}

fn token_aad(
    installation_id: &OpaqueId,
    registration_id: &OpaqueId,
    provider: PushProviderKind,
    environment: PushEnvironment,
) -> Vec<u8> {
    [
        b"remora-relay-token-v1\0".as_slice(),
        installation_id.as_str().as_bytes(),
        b"\0",
        registration_id.as_str().as_bytes(),
        b"\0",
        provider.as_str().as_bytes(),
        b"\0",
        environment.as_str().as_bytes(),
    ]
    .concat()
}

fn set_outbox_terminal(
    transaction: &Transaction<'_>,
    outbox_id: &OpaqueId,
    state: &'static str,
    now_ms: i64,
) -> Result<()> {
    transaction.execute(
        "UPDATE push_outbox SET state = ?2, lease_id = NULL, lease_until_ms = NULL,
         updated_at_ms = ?3 WHERE id = ?1",
        params![outbox_id.as_str(), state, now_ms],
    )?;
    Ok(())
}

fn retry_delay_ms(outbox_id: &str, attempt: u32, base_ms: i64, cap_ms: i64) -> i64 {
    let exponent = attempt.saturating_sub(1).min(20);
    let ceiling = base_ms.saturating_mul(1_i64 << exponent).min(cap_ms).max(1);
    let mut hasher = Sha256::new();
    hasher.update(b"remora-relay-retry-v1\0");
    hasher.update(outbox_id.as_bytes());
    hasher.update(attempt.to_be_bytes());
    let digest: [u8; 32] = hasher.finalize().into();
    let sample = u64::from_be_bytes(digest[..8].try_into().expect("fixed digest"));
    i64::try_from(sample % (ceiling as u64 + 1)).unwrap_or(ceiling)
}

fn to_i64(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| RelayError::LimitExceeded)
}

fn to_u64(value: i64) -> Result<u64> {
    u64::try_from(value)
        .map_err(|_| RelayError::Storage(rusqlite::Error::IntegralValueOutOfRange(0, value)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use std::collections::BTreeMap;

    fn fixture() -> (tempfile::TempDir, RelayStore, IssuedInstallation) {
        let directory = tempfile::tempdir().unwrap();
        let metrics = Arc::new(RelayMetrics::default());
        let store = RelayStore::open(
            directory.path().join("relay.sqlite3"),
            TokenCipher::from_key([9; 32]),
            StoreLimits::default(),
            metrics,
        )
        .unwrap();
        let request = CreateInstallationRequest::new("txn_store_fixture_0000000000000001").unwrap();
        let installation = store.create_installation(&request, 1_000).unwrap();
        (directory, store, installation)
    }

    fn presented(capability: &IssuedCapability) -> PresentedCapability {
        PresentedCapability::parse(capability.as_str().to_owned()).unwrap()
    }

    fn event(id: &str, class: EventClass, expiry: i64) -> IngestEventRequest {
        IngestEventRequest {
            event_id: OpaqueId::parse(id).unwrap(),
            event_class: class,
            expires_at_ms: expiry,
            ciphertext: URL_SAFE_NO_PAD.encode([42_u8; 32]),
            snapshot: None,
        }
    }

    #[test]
    fn installation_creation_replays_exact_encrypted_capabilities_and_conflicts() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("relay.sqlite3");
        let metrics = Arc::new(RelayMetrics::default());
        let store = RelayStore::open(
            &database,
            TokenCipher::from_key([21; 32]),
            StoreLimits::default(),
            metrics.clone(),
        )
        .unwrap();
        let request =
            CreateInstallationRequest::new("txn_sqlite_exact_replay_00000000001").unwrap();
        let first = store.create_installation(&request, 1_000).unwrap();
        let replay = store.create_installation(&request, 1_001).unwrap();
        assert!(first.created);
        assert!(!replay.created);
        assert_eq!(replay.installation_id, first.installation_id);
        assert_eq!(
            replay.write_capability.as_str(),
            first.write_capability.as_str()
        );
        assert_eq!(
            replay.read_capability.as_str(),
            first.read_capability.as_str()
        );
        assert_eq!(
            replay.manage_capability.as_str(),
            first.manage_capability.as_str()
        );

        let connection = Connection::open(&database).unwrap();
        let (receipt_count, installation_count): (i64, i64) = connection
            .query_row(
                "SELECT (SELECT COUNT(*) FROM installation_receipts),
                        (SELECT COUNT(*) FROM installations)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((receipt_count, installation_count), (1, 1));
        let ciphertext: Vec<u8> = connection
            .query_row(
                "SELECT response_ciphertext FROM installation_receipts",
                [],
                |row| row.get(0),
            )
            .unwrap();
        for capability in [
            first.write_capability.as_str(),
            first.read_capability.as_str(),
            first.manage_capability.as_str(),
        ] {
            assert!(
                !ciphertext
                    .windows(capability.len())
                    .any(|window| window == capability.as_bytes())
            );
        }

        let wrong_key_store = RelayStore::open(
            &database,
            TokenCipher::from_key([99; 32]),
            StoreLimits::default(),
            Arc::new(RelayMetrics::default()),
        )
        .unwrap();
        assert!(matches!(
            wrong_key_store.create_installation(&request, 1_002),
            Err(RelayError::Crypto)
        ));
        let reopened = RelayStore::open(
            &database,
            TokenCipher::from_key([21; 32]),
            StoreLimits::default(),
            Arc::new(RelayMetrics::default()),
        )
        .unwrap();
        assert_eq!(
            reopened
                .create_installation(&request, 1_003)
                .unwrap()
                .installation_id,
            first.installation_id
        );

        let mut conflicting = request.clone();
        conflicting.schema_version = SCHEMA_VERSION + 1;
        assert!(matches!(
            store.create_installation(&conflicting, 1_004),
            Err(RelayError::Conflict)
        ));
        assert_eq!(metrics.installations_created.load(Ordering::Relaxed), 1);
        assert_eq!(
            metrics.installation_create_replayed.load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            metrics
                .installation_create_conflicts
                .load(Ordering::Relaxed),
            1
        );
    }

    #[test]
    fn expired_or_tombstoned_creation_receipt_consumes_key_without_reissuing() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("relay.sqlite3");
        let limits = StoreLimits {
            installation_receipt_ttl_ms: 10,
            ..StoreLimits::default()
        };
        let store = RelayStore::open(
            &database,
            TokenCipher::from_key([22; 32]),
            limits.clone(),
            Arc::new(RelayMetrics::default()),
        )
        .unwrap();
        let request = CreateInstallationRequest::new("txn_sqlite_expiry_0000000000000001").unwrap();
        let _issued = store.create_installation(&request, 1_000).unwrap();
        assert_eq!(
            store
                .maintenance(1_011)
                .unwrap()
                .expired_installation_receipts,
            1
        );
        assert!(matches!(
            store.create_installation(&request, 1_012),
            Err(RelayError::Tombstoned)
        ));

        let second_request =
            CreateInstallationRequest::new("txn_sqlite_tombstone_00000000000001").unwrap();
        let second = store.create_installation(&second_request, 2_000).unwrap();
        store
            .tombstone_installation(
                &second.installation_id,
                &presented(&second.manage_capability),
                2_001,
            )
            .unwrap();
        assert!(matches!(
            store.create_installation(&second_request, 2_002),
            Err(RelayError::Tombstoned)
        ));
        store
            .maintenance(2_002 + limits.tombstone_retention_ms)
            .unwrap();
        assert!(matches!(
            store.create_installation(&second_request, 2_003 + limits.tombstone_retention_ms),
            Err(RelayError::Tombstoned)
        ));
        let connection = Connection::open(&database).unwrap();
        let receipts: i64 = connection
            .query_row("SELECT COUNT(*) FROM installation_receipts", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(receipts, 2);
        let installations: i64 = connection
            .query_row("SELECT COUNT(*) FROM installations", [], |row| row.get(0))
            .unwrap();
        assert_eq!(installations, 1);
    }

    #[test]
    fn acknowledgement_is_monotonic_bounded_read_authority_and_advisory() {
        let (_directory, store, installation) = fixture();
        let read = presented(&installation.read_capability);
        let write = presented(&installation.write_capability);
        let manage = presented(&installation.manage_capability);
        assert!(matches!(
            store.acknowledge(&installation.installation_id, &read, 0, 1_000),
            Err(RelayError::Invalid(_))
        ));
        assert!(matches!(
            store.acknowledge(&installation.installation_id, &read, 1, 1_000),
            Err(RelayError::Invalid(_))
        ));
        for index in 1..=3 {
            store
                .ingest_event(
                    &installation.installation_id,
                    &write,
                    &event(
                        &format!("evt_ack_sqlite_{index:08}"),
                        EventClass::StateChanged,
                        10_000,
                    ),
                    1_000,
                )
                .unwrap();
        }
        assert!(matches!(
            store.acknowledge(&installation.installation_id, &write, 1, 1_001),
            Err(RelayError::Unauthorized)
        ));
        let second_request =
            CreateInstallationRequest::new("txn_sqlite_ack_cross_install_000000001").unwrap();
        let second = store.create_installation(&second_request, 1_001).unwrap();
        assert!(matches!(
            store.acknowledge(&second.installation_id, &read, 1, 1_001),
            Err(RelayError::Unauthorized)
        ));
        let advanced = store
            .acknowledge(&installation.installation_id, &read, 2, 1_001)
            .unwrap();
        assert_eq!(advanced.acknowledged_through, 2);
        assert!(!advanced.replayed);
        let lower = store
            .acknowledge(&installation.installation_id, &read, 1, 1_002)
            .unwrap();
        assert_eq!(lower.acknowledged_through, 2);
        assert!(lower.replayed);
        let high = store
            .acknowledge(&installation.installation_id, &read, 3, 1_003)
            .unwrap();
        assert_eq!(high.acknowledged_through, 3);
        assert!(!high.replayed);
        assert!(matches!(
            store.acknowledge(&installation.installation_id, &read, 4, 1_004),
            Err(RelayError::Invalid(_))
        ));
        assert!(matches!(
            store.acknowledge(&installation.installation_id, &read, u64::MAX, 1_004),
            Err(RelayError::LimitExceeded)
        ));

        assert_eq!(store.maintenance(9_000).unwrap().expired_events, 0);
        let page = store
            .fetch_events(&installation.installation_id, &read, 0, 10, 9_000)
            .unwrap();
        assert_eq!(page.events.len(), 3);
        store
            .tombstone_installation(&installation.installation_id, &manage, 9_001)
            .unwrap();
        assert!(matches!(
            store.acknowledge(&installation.installation_id, &read, 3, 9_002),
            Err(RelayError::Tombstoned)
        ));
    }

    #[test]
    fn sqlite_v3_migration_adds_ack_and_receipt_contract_once() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("relay.sqlite3");
        let connection = Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE installations (
                    id TEXT PRIMARY KEY,
                    write_capability_hash BLOB NOT NULL,
                    read_capability_hash BLOB NOT NULL,
                    manage_capability_hash BLOB NOT NULL,
                    next_sequence INTEGER NOT NULL CHECK(next_sequence >= 1),
                    replay_floor INTEGER NOT NULL CHECK(replay_floor >= 1),
                    created_at_ms INTEGER NOT NULL,
                    updated_at_ms INTEGER NOT NULL,
                    tombstoned_at_ms INTEGER
                 ) STRICT;
                 PRAGMA user_version = 3;",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO installations (
                    id, write_capability_hash, read_capability_hash, manage_capability_hash,
                    next_sequence, replay_floor, created_at_ms, updated_at_ms
                 ) VALUES ('inst_sqlite_legacy_0001', ?1, ?2, ?3, 4, 1, 1, 1)",
                params![
                    [1_u8; 32].as_slice(),
                    [2_u8; 32].as_slice(),
                    [3_u8; 32].as_slice()
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO installations (
                    id, write_capability_hash, read_capability_hash, manage_capability_hash,
                    next_sequence, replay_floor, created_at_ms, updated_at_ms, tombstoned_at_ms
                 ) VALUES ('inst_sqlite_legacy_tombstone_0001', ?1, ?2, ?3, 2, 1, 1, 2, 2)",
                params![
                    [4_u8; 32].as_slice(),
                    [5_u8; 32].as_slice(),
                    [6_u8; 32].as_slice()
                ],
            )
            .unwrap();
        drop(connection);

        let store = RelayStore::open(
            &database,
            TokenCipher::from_key([23; 32]),
            StoreLimits::default(),
            Arc::new(RelayMetrics::default()),
        )
        .unwrap();
        let connection = store.connection().unwrap();
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 4);
        let acknowledged: i64 = connection
            .query_row(
                "SELECT acknowledged_through FROM installations
                 WHERE id = 'inst_sqlite_legacy_0001'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(acknowledged, 0);
        let tombstoned_ack: i64 = connection
            .query_row(
                "SELECT acknowledged_through FROM installations
                 WHERE id = 'inst_sqlite_legacy_tombstone_0001'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tombstoned_ack, 0);
        assert!(sqlite_has_column(&connection, "installations", "acknowledged_through").unwrap());
        let receipt_table: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name = 'installation_receipts'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(receipt_table, 1);
        drop(connection);
        drop(store);
        RelayStore::open(
            &database,
            TokenCipher::from_key([23; 32]),
            StoreLimits::default(),
            Arc::new(RelayMetrics::default()),
        )
        .unwrap();
    }

    #[test]
    fn exact_event_replay_returns_original_cursor() {
        let (_directory, store, installation) = fixture();
        let capability = presented(&installation.write_capability);
        let request = event("evt_exact_replay_0001", EventClass::StateChanged, 10_000);
        let first = store
            .ingest_event(&installation.installation_id, &capability, &request, 1_000)
            .unwrap();
        let second = store
            .ingest_event(&installation.installation_id, &capability, &request, 1_001)
            .unwrap();
        assert_eq!(first.cursor, 1);
        assert_eq!(second.cursor, 1);
        assert!(second.replayed);
        assert_eq!(store.diagnostics().unwrap().retained_events, 1);
    }

    #[test]
    fn idempotency_receipt_survives_payload_expiry() {
        let (_directory, store, installation) = fixture();
        let capability = presented(&installation.write_capability);
        let mut request = event("evt_expired_replay_0001", EventClass::StateChanged, 2_000);
        let first = store
            .ingest_event(&installation.installation_id, &capability, &request, 1_000)
            .unwrap();
        assert_eq!(store.maintenance(2_001).unwrap().expired_events, 1);
        assert_eq!(store.diagnostics().unwrap().retained_events, 0);

        let replay = store
            .ingest_event(&installation.installation_id, &capability, &request, 2_002)
            .unwrap();
        assert_eq!(replay.cursor, first.cursor);
        assert!(replay.replayed);

        request.event_class = EventClass::SecurityChanged;
        assert!(matches!(
            store.ingest_event(&installation.installation_id, &capability, &request, 2_003),
            Err(RelayError::Conflict)
        ));
    }

    #[test]
    fn reused_event_id_with_different_bytes_conflicts() {
        let (_directory, store, installation) = fixture();
        let capability = presented(&installation.write_capability);
        let mut request = event("evt_conflict_0000001", EventClass::StateChanged, 10_000);
        store
            .ingest_event(&installation.installation_id, &capability, &request, 1_000)
            .unwrap();
        request.event_class = EventClass::SecurityChanged;
        assert!(matches!(
            store.ingest_event(&installation.installation_id, &capability, &request, 1_001),
            Err(RelayError::Conflict)
        ));
    }

    #[test]
    fn wrong_capability_kind_is_rejected_without_enumeration() {
        let (_directory, store, installation) = fixture();
        let read = presented(&installation.read_capability);
        let request = event("evt_wrong_cap_000001", EventClass::StateChanged, 10_000);
        assert!(matches!(
            store.ingest_event(&installation.installation_id, &read, &request, 1_000),
            Err(RelayError::Unauthorized)
        ));
        let missing = OpaqueId::parse("inst_missing_00000001").unwrap();
        assert!(matches!(
            store.ingest_event(&missing, &read, &request, 1_000),
            Err(RelayError::Unauthorized)
        ));
    }

    #[test]
    fn registration_token_is_encrypted_and_outbox_contains_only_hint() {
        let (_directory, store, installation) = fixture();
        let manage = presented(&installation.manage_capability);
        let write = presented(&installation.write_capability);
        store
            .register_device(
                &installation.installation_id,
                &manage,
                PushProviderKind::Apns,
                PushEnvironment::Sandbox,
                "canary-super-secret-push-token",
                1_000,
            )
            .unwrap();
        let request = event("evt_push_hint_000001", EventClass::ActivityChanged, 10_000);
        store
            .ingest_event(&installation.installation_id, &write, &request, 1_000)
            .unwrap();
        let leases = store.lease_deliveries(1_001, 10).unwrap();
        assert_eq!(leases.len(), 1);
        assert_eq!(leases[0].hint.cursor, 1);
        assert_eq!(leases[0].hint.event_id, request.event_id);
        assert!(!format!("{:?}", leases[0]).contains("canary-super-secret"));
    }

    #[test]
    fn newer_generation_survives_in_flight_completion() {
        let (_directory, store, installation) = fixture();
        let manage = presented(&installation.manage_capability);
        let write = presented(&installation.write_capability);
        store
            .register_device(
                &installation.installation_id,
                &manage,
                PushProviderKind::Fcm,
                PushEnvironment::Production,
                "fcm-canary-token-000000000000",
                1_000,
            )
            .unwrap();
        store
            .ingest_event(
                &installation.installation_id,
                &write,
                &event("evt_generation_000001", EventClass::StateChanged, 10_000),
                1_000,
            )
            .unwrap();
        let first = store.lease_deliveries(1_001, 1).unwrap().remove(0);
        store
            .ingest_event(
                &installation.installation_id,
                &write,
                &event("evt_generation_000002", EventClass::StateChanged, 10_000),
                1_002,
            )
            .unwrap();
        store
            .complete_delivery(
                &first.outbox_id,
                first.generation,
                first.registration_generation,
                &first.lease_id,
                DeliveryOutcome::Accepted,
                1_003,
            )
            .unwrap();
        let next = store.lease_deliveries(1_004, 1).unwrap().remove(0);
        assert_eq!(next.hint.cursor, 2);
        assert_eq!(next.attempt, 1);
    }

    #[test]
    fn late_completion_cannot_overwrite_a_recovered_lease() {
        let (_directory, store, installation) = fixture();
        let manage = presented(&installation.manage_capability);
        let write = presented(&installation.write_capability);
        store
            .register_device(
                &installation.installation_id,
                &manage,
                PushProviderKind::Fcm,
                PushEnvironment::Production,
                "fcm-late-completion-token-000001",
                1_000,
            )
            .unwrap();
        store
            .ingest_event(
                &installation.installation_id,
                &write,
                &event("evt_late_completion_01", EventClass::StateChanged, 100_000),
                1_000,
            )
            .unwrap();
        let first = store.lease_deliveries(1_001, 1).unwrap().remove(0);
        let second = store.lease_deliveries(31_002, 1).unwrap().remove(0);
        assert_ne!(first.lease_id, second.lease_id);
        assert_eq!(second.attempt, 2);

        store
            .complete_delivery(
                &first.outbox_id,
                first.generation,
                first.registration_generation,
                &first.lease_id,
                DeliveryOutcome::Accepted,
                31_003,
            )
            .unwrap();
        assert_eq!(store.diagnostics().unwrap().leased_outbox, 1);

        store
            .complete_delivery(
                &second.outbox_id,
                second.generation,
                second.registration_generation,
                &second.lease_id,
                DeliveryOutcome::Accepted,
                31_004,
            )
            .unwrap();
        assert_eq!(store.diagnostics().unwrap().leased_outbox, 0);
    }

    #[test]
    fn retention_gap_requires_snapshot_reconciliation() {
        let (_directory, store, installation) = fixture();
        let write = presented(&installation.write_capability);
        let read = presented(&installation.read_capability);
        store
            .ingest_event(
                &installation.installation_id,
                &write,
                &event("evt_expiring_0000001", EventClass::StateChanged, 2_000),
                1_000,
            )
            .unwrap();
        store.maintenance(2_001).unwrap();
        let page = store
            .fetch_events(&installation.installation_id, &read, 0, 10, 2_001)
            .unwrap();
        assert!(page.reset_required);
        assert_eq!(page.replay_floor, 2);
        assert!(page.events.is_empty());
    }

    #[test]
    fn retry_delay_is_bounded_and_stable() {
        let first = retry_delay_ms("wake_0123456789abcdef", 4, 500, 10_000);
        let second = retry_delay_ms("wake_0123456789abcdef", 4, 500, 10_000);
        assert_eq!(first, second);
        assert!((0..=4_000).contains(&first));
    }

    #[test]
    fn diagnostic_counts_are_aggregate_only() {
        let (_directory, store, _installation) = fixture();
        let diagnostics = store.diagnostics().unwrap();
        assert_eq!(diagnostics.active_installations, 1);
        assert_eq!(diagnostics.retained_events, 0);
    }

    #[test]
    fn event_class_sql_round_trip_is_closed() {
        let values = BTreeMap::from([
            ("activity_changed", EventClass::ActivityChanged),
            ("connection_changed", EventClass::ConnectionChanged),
            ("security_changed", EventClass::SecurityChanged),
            ("state_changed", EventClass::StateChanged),
        ]);
        for (wire, expected) in values {
            assert_eq!(EventClass::from_str(wire).unwrap(), expected);
        }
        assert!(EventClass::from_str("approval_text").is_err());
    }
}
