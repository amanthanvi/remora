//! Encrypted device-local cache for offline intent and bounded search.
//!
//! This database is never authoritative for live Host/provider state. Platform
//! code owns the master key in Keychain/Keystore and passes it only while
//! opening the database.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Mutex;

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use hmac::{Hmac, Mac};
use rand_core::{OsRng, RngCore};
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use zeroize::{Zeroize, Zeroizing};

const SCHEMA_VERSION: i64 = 5;
// Schema v3 through v5 add only metadata or new encrypted record types.
// Keep the encrypted record envelope at v2 so additive migrations do not
// invalidate existing outbox, review-note, or search ciphertext.
const ENCRYPTION_RECORD_VERSION: i64 = 2;
const KEY_BYTES: usize = 32;
const NONCE_BYTES: usize = 24;
const MAX_ID_BYTES: usize = 128;
const MAX_PROVIDER_THREAD_ID_BYTES: usize =
    remora_bridge_core::command_center::MAX_DISPLAY_LABEL_BYTES;
const MAX_OUTBOX_PAYLOAD_BYTES: usize = 1024 * 1024;
const MAX_SEARCH_BODY_BYTES: usize = 1024 * 1024;
const MAX_OUTBOX_ROWS: usize = 100;
const MAX_THREAD_BINDINGS: usize = 20_000;
const MAX_SEARCH_RESULTS: usize = 50;
const MAX_SEARCH_CANDIDATES: usize = 200;
const MAX_SEARCH_INDEX_BATCH: usize = 500;
const MAX_SEARCH_HOST_STATES: usize = 100;
const MAX_SEARCH_QUERY_BYTES: usize = 4_096;
const MAX_QUERY_TOKENS: usize = 8;
const MAX_TOKEN_CHARS: usize = 64;
const MAX_PREFIX_CHARS: usize = 24;
const MAX_SNIPPET_BYTES: usize = 512;
const MAX_REVIEW_BODY_BYTES: usize = 64 * 1024;
const MAX_REVIEW_PATH_BYTES: usize = 4_096;
const MAX_REVIEW_NOTES: usize = 100;
const MAX_THREAD_ATTENTION_ROWS: usize = 100_000;
const MAX_SNOOZE_DURATION_MS: i64 = 30 * 24 * 60 * 60 * 1_000;
const SEARCH_RETENTION_MS: i64 = 90 * 24 * 60 * 60 * 1_000;
const SEARCH_RETENTION_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_RETENTION_DELETIONS: usize = 1_000;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, thiserror::Error)]
pub enum DeviceDatabaseError {
    #[error("invalid device database key")]
    InvalidKey,
    #[error("invalid device database input: {0}")]
    InvalidInput(String),
    #[error("device database is unavailable: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("device database authentication failed")]
    Authentication,
    #[error("device database lock is poisoned")]
    Poisoned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboxIntentKind {
    SendMessage,
    CreateThread,
    SetOrganizationState,
}

impl OutboxIntentKind {
    fn as_i64(self) -> i64 {
        match self {
            Self::SendMessage => 0,
            Self::CreateThread => 1,
            Self::SetOrganizationState => 2,
        }
    }

    fn from_i64(value: i64) -> Result<Self, DeviceDatabaseError> {
        match value {
            0 => Ok(Self::SendMessage),
            1 => Ok(Self::CreateThread),
            2 => Ok(Self::SetOrganizationState),
            _ => Err(DeviceDatabaseError::Authentication),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboxState {
    Queued,
    Delivering,
    OutcomeUnknown,
}

impl OutboxState {
    fn as_i64(self) -> i64 {
        match self {
            Self::Queued => 0,
            Self::Delivering => 1,
            Self::OutcomeUnknown => 2,
        }
    }

    fn from_i64(value: i64) -> Result<Self, DeviceDatabaseError> {
        match value {
            0 => Ok(Self::Queued),
            1 => Ok(Self::Delivering),
            2 => Ok(Self::OutcomeUnknown),
            _ => Err(DeviceDatabaseError::Authentication),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OutboxThreadStatus {
    pub queued_count: u32,
    pub outcome_unknown_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxIntent {
    pub intent_id: String,
    pub host_id: String,
    pub thread_id: Option<String>,
    pub kind: OutboxIntentKind,
    pub payload: Vec<u8>,
    pub state: OutboxState,
    pub created_at_ms: i64,
    pub attempt_count: u32,
    pub next_attempt_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    pub document_id: String,
    pub host_id: String,
    pub thread_id: String,
    pub snippet: String,
    /// Decrypted only after the bounded postings candidate query. Kept inside
    /// Rust so typed projections can recover cached display metadata without
    /// exposing the encrypted document format to Swift or Kotlin.
    pub body: String,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchDocumentInput {
    pub document_id: String,
    pub host_id: String,
    pub thread_id: String,
    pub body: String,
    pub search_text: String,
    pub updated_at_ms: i64,
    pub protected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHostState {
    pub host_id: String,
    pub last_indexed_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadOrganization {
    pub host_id: String,
    pub thread_id: String,
    pub pinned: bool,
    pub hidden: bool,
    pub snoozed_until_ms: Option<i64>,
    pub acknowledged_at_ms: Option<i64>,
    pub relay_sequence: u64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadAttentionState {
    pub host_id: String,
    pub thread_id: String,
    pub terminal_event_id: Option<String>,
    pub occurred_at_ms: Option<i64>,
    pub failed: bool,
    pub snoozed_until_ms: Option<i64>,
    pub acknowledged_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewNoteState {
    Open,
    Resolved,
}

impl ReviewNoteState {
    fn as_i64(self) -> i64 {
        match self {
            Self::Open => 0,
            Self::Resolved => 1,
        }
    }

    fn from_i64(value: i64) -> Result<Self, DeviceDatabaseError> {
        match value {
            0 => Ok(Self::Open),
            1 => Ok(Self::Resolved),
            _ => Err(DeviceDatabaseError::Authentication),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewNote {
    pub note_id: String,
    pub host_id: String,
    pub thread_id: String,
    pub checkpoint_id: String,
    pub path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub body: String,
    pub state: ReviewNoteState,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct EncryptedReviewNote {
    path: String,
    start_line: u32,
    end_line: u32,
    body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EncryptedThreadBinding {
    provider_thread_id: String,
    host_thread_id: String,
}

pub struct DeviceDatabase {
    connection: Mutex<Connection>,
    master_key: Zeroizing<[u8; KEY_BYTES]>,
}

impl DeviceDatabase {
    pub fn open(path: &Path, mut master_key: Vec<u8>) -> Result<Self, DeviceDatabaseError> {
        let key = take_key(&mut master_key)?;
        let connection = Connection::open(path)?;
        Self::initialize(connection, key)
    }

    #[cfg(test)]
    fn open_in_memory(master_key: Vec<u8>) -> Result<Self, DeviceDatabaseError> {
        let mut master_key = master_key;
        let key = take_key(&mut master_key)?;
        let connection = Connection::open_in_memory()?;
        Self::initialize(connection, key)
    }

    fn initialize(
        connection: Connection,
        master_key: Zeroizing<[u8; KEY_BYTES]>,
    ) -> Result<Self, DeviceDatabaseError> {
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = FULL;
             PRAGMA secure_delete = ON;
             CREATE TABLE IF NOT EXISTS meta (
                 key TEXT PRIMARY KEY,
                 value BLOB NOT NULL
             ) STRICT;
             CREATE TABLE IF NOT EXISTS thread_organization (
                 host_id TEXT NOT NULL,
                 thread_id TEXT NOT NULL,
                 pinned INTEGER NOT NULL DEFAULT 0,
                 hidden INTEGER NOT NULL DEFAULT 0,
                 snoozed_until_ms INTEGER,
                 acknowledged_at_ms INTEGER,
                 relay_sequence INTEGER NOT NULL,
                 updated_at_ms INTEGER NOT NULL,
                 PRIMARY KEY (host_id, thread_id)
             ) STRICT;
             CREATE TABLE IF NOT EXISTS thread_attention (
                 host_id TEXT NOT NULL,
                 thread_id TEXT NOT NULL,
                 terminal_event_id TEXT NOT NULL,
                 occurred_at_ms INTEGER NOT NULL,
                 failed INTEGER NOT NULL,
                 PRIMARY KEY (host_id, thread_id)
             ) STRICT;
             CREATE TABLE IF NOT EXISTS outbox (
                 intent_id TEXT PRIMARY KEY,
                 host_id TEXT NOT NULL,
                 thread_id TEXT,
                 kind INTEGER NOT NULL,
                 nonce BLOB NOT NULL,
                 encrypted_payload BLOB NOT NULL,
                 state INTEGER NOT NULL,
                 created_at_ms INTEGER NOT NULL,
                 attempt_count INTEGER NOT NULL,
                 next_attempt_at_ms INTEGER
             ) STRICT;
             CREATE INDEX IF NOT EXISTS outbox_due
                 ON outbox(state, next_attempt_at_ms, created_at_ms);
             CREATE TABLE IF NOT EXISTS thread_bindings (
                 binding_hash BLOB PRIMARY KEY,
                 host_id TEXT NOT NULL,
                 nonce BLOB NOT NULL,
                 encrypted_binding BLOB NOT NULL,
                 updated_at_ms INTEGER NOT NULL
             ) STRICT;
             CREATE INDEX IF NOT EXISTS thread_bindings_updated
                 ON thread_bindings(updated_at_ms);
             CREATE TABLE IF NOT EXISTS review_notes (
                 note_id TEXT PRIMARY KEY,
                 host_id TEXT NOT NULL,
                 thread_id TEXT NOT NULL,
                 checkpoint_id TEXT NOT NULL,
                 nonce BLOB NOT NULL,
                 encrypted_note BLOB NOT NULL,
                 state INTEGER NOT NULL,
                 created_at_ms INTEGER NOT NULL,
                 updated_at_ms INTEGER NOT NULL
             ) STRICT;
             CREATE INDEX IF NOT EXISTS review_notes_thread
                 ON review_notes(host_id, thread_id, state, updated_at_ms);
             CREATE TABLE IF NOT EXISTS search_documents (
                 document_id TEXT PRIMARY KEY,
                 host_id TEXT NOT NULL,
                 thread_id TEXT NOT NULL,
                 nonce BLOB NOT NULL,
                 encrypted_body BLOB NOT NULL,
                 updated_at_ms INTEGER NOT NULL,
                 protected INTEGER NOT NULL DEFAULT 0
             ) STRICT;
             CREATE INDEX IF NOT EXISTS search_documents_retention
                 ON search_documents(protected, updated_at_ms);
             CREATE TABLE IF NOT EXISTS search_terms (
                 term_hash BLOB NOT NULL,
                 document_id TEXT NOT NULL REFERENCES search_documents(document_id)
                     ON DELETE CASCADE,
                 PRIMARY KEY (term_hash, document_id)
             ) STRICT;
             CREATE INDEX IF NOT EXISTS search_terms_document
                 ON search_terms(document_id);
             CREATE TABLE IF NOT EXISTS search_hosts (
                 host_id TEXT PRIMARY KEY,
                 last_indexed_at_ms INTEGER NOT NULL
             ) STRICT;",
        )?;

        let database = Self {
            connection: Mutex::new(connection),
            master_key,
        };
        database.verify_or_create_key_check()?;
        database.set_schema_version()?;
        Ok(database)
    }

    pub fn enqueue_outbox(&self, intent: &OutboxIntent) -> Result<bool, DeviceDatabaseError> {
        validate_id("intent_id", &intent.intent_id)?;
        validate_id("host_id", &intent.host_id)?;
        if let Some(thread_id) = &intent.thread_id {
            validate_id("thread_id", thread_id)?;
        }
        if intent.payload.len() > MAX_OUTBOX_PAYLOAD_BYTES {
            return Err(DeviceDatabaseError::InvalidInput(
                "outbox payload exceeds 1 MiB".to_string(),
            ));
        }
        let (nonce, ciphertext) = self.encrypt(
            "outbox",
            &intent.intent_id,
            &intent.host_id,
            &intent.payload,
        )?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let inserted = transaction.execute(
            "INSERT OR IGNORE INTO outbox (
                 intent_id, host_id, thread_id, kind, nonce, encrypted_payload,
                 state, created_at_ms, attempt_count, next_attempt_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                intent.intent_id,
                intent.host_id,
                intent.thread_id,
                intent.kind.as_i64(),
                nonce.as_slice(),
                ciphertext,
                intent.state.as_i64(),
                intent.created_at_ms,
                i64::from(intent.attempt_count),
                intent.next_attempt_at_ms,
            ],
        )? == 1;
        transaction.commit()?;
        Ok(inserted)
    }

    pub fn due_outbox(
        &self,
        now_ms: i64,
        limit: usize,
    ) -> Result<Vec<OutboxIntent>, DeviceDatabaseError> {
        let limit = limit.clamp(1, MAX_OUTBOX_ROWS) as i64;
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT intent_id, host_id, thread_id, kind, nonce, encrypted_payload,
                    state, created_at_ms, attempt_count, next_attempt_at_ms
             FROM outbox AS candidate
             WHERE candidate.state = ?2
               AND (candidate.next_attempt_at_ms IS NULL OR candidate.next_attempt_at_ms <= ?1)
               AND NOT EXISTS (
                   SELECT 1
                   FROM outbox AS blocker
                   WHERE blocker.host_id = candidate.host_id
                     AND blocker.thread_id IS candidate.thread_id
                     AND blocker.state = ?3
               )
               AND NOT EXISTS (
                   SELECT 1
                   FROM outbox AS predecessor
                   WHERE predecessor.host_id = candidate.host_id
                     AND predecessor.thread_id IS candidate.thread_id
                     AND predecessor.state = ?2
                     AND (
                         predecessor.created_at_ms < candidate.created_at_ms
                         OR (
                             predecessor.created_at_ms = candidate.created_at_ms
                             AND predecessor.intent_id < candidate.intent_id
                         )
                     )
               )
             ORDER BY candidate.created_at_ms ASC, candidate.intent_id ASC
             LIMIT ?4",
        )?;
        let rows = statement.query_map(
            params![
                now_ms,
                OutboxState::Queued.as_i64(),
                OutboxState::OutcomeUnknown.as_i64(),
                limit,
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                    row.get::<_, Vec<u8>>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, Option<i64>>(9)?,
                ))
            },
        )?;

        let mut intents = Vec::new();
        for row in rows {
            let (
                intent_id,
                host_id,
                thread_id,
                kind,
                nonce,
                ciphertext,
                state,
                created_at_ms,
                attempt_count,
                next_attempt_at_ms,
            ) = row?;
            let payload = self.decrypt("outbox", &intent_id, &host_id, &nonce, &ciphertext)?;
            intents.push(OutboxIntent {
                intent_id,
                host_id,
                thread_id,
                kind: OutboxIntentKind::from_i64(kind)?,
                payload,
                state: OutboxState::from_i64(state)?,
                created_at_ms,
                attempt_count: u32::try_from(attempt_count)
                    .map_err(|_| DeviceDatabaseError::Authentication)?,
                next_attempt_at_ms,
            });
        }
        Ok(intents)
    }

    /// Earliest retry deadline across deliverable Threads. A queued intent
    /// without a deadline is due immediately; Threads fenced by an uncertain
    /// predecessor remain excluded until the user resolves that copy.
    pub fn next_outbox_attempt_at_ms(&self) -> Result<Option<i64>, DeviceDatabaseError> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT MIN(COALESCE(candidate.next_attempt_at_ms, 0))
                 FROM outbox AS candidate
                 WHERE candidate.state = ?1
                   AND NOT EXISTS (
                       SELECT 1
                       FROM outbox AS blocker
                       WHERE blocker.host_id = candidate.host_id
                         AND blocker.thread_id IS candidate.thread_id
                         AND blocker.state = ?2
                   )
                   AND NOT EXISTS (
                       SELECT 1
                       FROM outbox AS predecessor
                       WHERE predecessor.host_id = candidate.host_id
                         AND predecessor.thread_id IS candidate.thread_id
                         AND predecessor.state = ?1
                         AND (
                             predecessor.created_at_ms < candidate.created_at_ms
                             OR (
                                 predecessor.created_at_ms = candidate.created_at_ms
                                 AND predecessor.intent_id < candidate.intent_id
                             )
                         )
                   )",
                params![
                    OutboxState::Queued.as_i64(),
                    OutboxState::OutcomeUnknown.as_i64(),
                ],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    /// Persist only the provider-to-Host Thread correlation needed to extend
    /// an existing offline queue after a cold launch. The lookup key is an
    /// HMAC and both identities remain inside the authenticated ciphertext.
    pub fn upsert_thread_binding(
        &self,
        host_id: &str,
        provider_thread_id: &str,
        host_thread_id: &str,
        updated_at_ms: i64,
    ) -> Result<(), DeviceDatabaseError> {
        validate_id("host_id", host_id)?;
        validate_provider_thread_id(provider_thread_id)?;
        validate_id("host_thread_id", host_thread_id)?;
        validate_timestamp("updated_at_ms", updated_at_ms)?;
        let binding_hash = self.thread_binding_hash(host_id, provider_thread_id)?;
        let primary_key = hex::encode(binding_hash);
        let plaintext = serde_json::to_vec(&EncryptedThreadBinding {
            provider_thread_id: provider_thread_id.to_string(),
            host_thread_id: host_thread_id.to_string(),
        })
        .map_err(|error| DeviceDatabaseError::InvalidInput(error.to_string()))?;
        let (nonce, ciphertext) =
            self.encrypt("thread_binding", &primary_key, host_id, &plaintext)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let exists = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM thread_bindings WHERE binding_hash = ?1)",
            [binding_hash.as_slice()],
            |row| row.get::<_, bool>(0),
        )?;
        if !exists {
            let count =
                transaction.query_row("SELECT COUNT(*) FROM thread_bindings", [], |row| {
                    row.get::<_, i64>(0)
                })?;
            if count >= MAX_THREAD_BINDINGS as i64 {
                return Err(DeviceDatabaseError::InvalidInput(
                    "thread binding limit reached".to_string(),
                ));
            }
        }
        transaction.execute(
            "INSERT INTO thread_bindings (
                 binding_hash, host_id, nonce, encrypted_binding, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(binding_hash) DO UPDATE SET
                 host_id = excluded.host_id,
                 nonce = excluded.nonce,
                 encrypted_binding = excluded.encrypted_binding,
                 updated_at_ms = excluded.updated_at_ms",
            params![
                binding_hash.as_slice(),
                host_id,
                nonce.as_slice(),
                ciphertext,
                updated_at_ms,
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn host_thread_id_for_provider(
        &self,
        host_id: &str,
        provider_thread_id: &str,
    ) -> Result<Option<String>, DeviceDatabaseError> {
        validate_id("host_id", host_id)?;
        validate_provider_thread_id(provider_thread_id)?;
        let binding_hash = self.thread_binding_hash(host_id, provider_thread_id)?;
        let primary_key = hex::encode(binding_hash);
        let connection = self.lock()?;
        let row = connection
            .query_row(
                "SELECT nonce, encrypted_binding
                 FROM thread_bindings
                 WHERE binding_hash = ?1 AND host_id = ?2",
                params![binding_hash.as_slice(), host_id],
                |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?)),
            )
            .optional()?;
        let Some((nonce, ciphertext)) = row else {
            return Ok(None);
        };
        let plaintext =
            self.decrypt("thread_binding", &primary_key, host_id, &nonce, &ciphertext)?;
        let binding: EncryptedThreadBinding =
            serde_json::from_slice(&plaintext).map_err(|_| DeviceDatabaseError::Authentication)?;
        if binding.provider_thread_id != provider_thread_id {
            return Err(DeviceDatabaseError::Authentication);
        }
        validate_id("host_thread_id", &binding.host_thread_id)?;
        Ok(Some(binding.host_thread_id))
    }

    pub fn outbox_thread_status(
        &self,
        host_id: &str,
        thread_id: &str,
    ) -> Result<OutboxThreadStatus, DeviceDatabaseError> {
        validate_id("host_id", host_id)?;
        validate_id("thread_id", thread_id)?;
        let connection = self.lock()?;
        let (queued_count, outcome_unknown_count) = connection.query_row(
            "SELECT
                 COALESCE(SUM(CASE WHEN state = ?3 THEN 1 ELSE 0 END), 0),
                 COALESCE(SUM(CASE WHEN state = ?4 THEN 1 ELSE 0 END), 0)
             FROM outbox
             WHERE host_id = ?1 AND thread_id = ?2",
            params![
                host_id,
                thread_id,
                OutboxState::Queued.as_i64(),
                OutboxState::OutcomeUnknown.as_i64(),
            ],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )?;
        Ok(OutboxThreadStatus {
            queued_count: u32::try_from(queued_count)
                .map_err(|_| DeviceDatabaseError::Authentication)?,
            outcome_unknown_count: u32::try_from(outcome_unknown_count)
                .map_err(|_| DeviceDatabaseError::Authentication)?,
        })
    }

    pub fn record_outbox_attempt(
        &self,
        intent_id: &str,
        next_attempt_at_ms: i64,
    ) -> Result<bool, DeviceDatabaseError> {
        validate_id("intent_id", intent_id)?;
        let connection = self.lock()?;
        Ok(connection.execute(
            "UPDATE outbox
             SET state = ?2, attempt_count = attempt_count + 1, next_attempt_at_ms = ?3
             WHERE intent_id = ?1 AND state = ?2",
            params![intent_id, OutboxState::Queued.as_i64(), next_attempt_at_ms],
        )? == 1)
    }

    pub fn record_outbox_outcome_unknown(
        &self,
        intent_id: &str,
    ) -> Result<bool, DeviceDatabaseError> {
        validate_id("intent_id", intent_id)?;
        let connection = self.lock()?;
        Ok(connection.execute(
            "UPDATE outbox
             SET state = ?2, attempt_count = attempt_count + 1, next_attempt_at_ms = NULL
             WHERE intent_id = ?1 AND state = ?3",
            params![
                intent_id,
                OutboxState::OutcomeUnknown.as_i64(),
                OutboxState::Queued.as_i64(),
            ],
        )? == 1)
    }

    /// User-confirmed removal of uncertain copies. This never deletes queued
    /// work that has not crossed the Host dispatch fence.
    pub fn discard_outbox_outcome_unknown(
        &self,
        host_id: &str,
        thread_id: &str,
        expected_count: u32,
    ) -> Result<u32, DeviceDatabaseError> {
        validate_id("host_id", host_id)?;
        validate_id("thread_id", thread_id)?;
        if expected_count == 0 {
            return Ok(0);
        }
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let current = transaction.query_row(
            "SELECT COUNT(*) FROM outbox
             WHERE host_id = ?1 AND thread_id = ?2 AND state = ?3",
            params![host_id, thread_id, OutboxState::OutcomeUnknown.as_i64()],
            |row| row.get::<_, i64>(0),
        )?;
        if current != i64::from(expected_count) {
            return Ok(0);
        }
        let deleted = transaction.execute(
            "DELETE FROM outbox
             WHERE host_id = ?1 AND thread_id = ?2 AND state = ?3",
            params![host_id, thread_id, OutboxState::OutcomeUnknown.as_i64()],
        )?;
        transaction.commit()?;
        Ok(deleted.min(u32::MAX as usize) as u32)
    }

    /// Delete only after an authoritative Host acknowledgement is reconciled.
    pub fn acknowledge_outbox(&self, intent_id: &str) -> Result<bool, DeviceDatabaseError> {
        validate_id("intent_id", intent_id)?;
        let connection = self.lock()?;
        Ok(connection.execute("DELETE FROM outbox WHERE intent_id = ?1", [intent_id])? == 1)
    }

    /// Applies only a newer relay-accepted organization event. Sequence zero
    /// is reserved for a first local projection; delivered relay events must
    /// advance monotonically.
    pub fn apply_thread_organization(
        &self,
        organization: &ThreadOrganization,
    ) -> Result<bool, DeviceDatabaseError> {
        validate_id("host_id", &organization.host_id)?;
        validate_id("thread_id", &organization.thread_id)?;
        let relay_sequence = i64::try_from(organization.relay_sequence).map_err(|_| {
            DeviceDatabaseError::InvalidInput("relay_sequence exceeds SQLite range".to_string())
        })?;
        let connection = self.lock()?;
        Ok(connection.execute(
            "INSERT INTO thread_organization (
                 host_id, thread_id, pinned, hidden, snoozed_until_ms,
                 acknowledged_at_ms, relay_sequence, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(host_id, thread_id) DO UPDATE SET
                 pinned = excluded.pinned,
                 hidden = excluded.hidden,
                 snoozed_until_ms = excluded.snoozed_until_ms,
                 acknowledged_at_ms = excluded.acknowledged_at_ms,
                 relay_sequence = excluded.relay_sequence,
                 updated_at_ms = excluded.updated_at_ms
             WHERE excluded.relay_sequence > thread_organization.relay_sequence",
            params![
                organization.host_id,
                organization.thread_id,
                i64::from(organization.pinned),
                i64::from(organization.hidden),
                organization.snoozed_until_ms,
                organization.acknowledged_at_ms,
                relay_sequence,
                organization.updated_at_ms,
            ],
        )? == 1)
    }

    pub fn thread_organization(
        &self,
        host_id: &str,
        thread_id: &str,
    ) -> Result<Option<ThreadOrganization>, DeviceDatabaseError> {
        validate_id("host_id", host_id)?;
        validate_id("thread_id", thread_id)?;
        let connection = self.lock()?;
        let row = connection
            .query_row(
                "SELECT pinned, hidden, snoozed_until_ms, acknowledged_at_ms,
                        relay_sequence, updated_at_ms
                 FROM thread_organization
                 WHERE host_id = ?1 AND thread_id = ?2",
                params![host_id, thread_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            )
            .optional()?;
        row.map(
            |(
                pinned,
                hidden,
                snoozed_until_ms,
                acknowledged_at_ms,
                relay_sequence,
                updated_at_ms,
            )| {
                Ok(ThreadOrganization {
                    host_id: host_id.to_string(),
                    thread_id: thread_id.to_string(),
                    pinned: decode_bool(pinned)?,
                    hidden: decode_bool(hidden)?,
                    snoozed_until_ms,
                    acknowledged_at_ms,
                    relay_sequence: u64::try_from(relay_sequence)
                        .map_err(|_| DeviceDatabaseError::Authentication)?,
                    updated_at_ms,
                })
            },
        )
        .transpose()
    }

    /// Records an attention event only when the live Host emits a terminal
    /// turn transition. Historical idle Threads therefore remain baseline-
    /// neutral when a device first enables the command center.
    pub fn record_terminal_attention(
        &self,
        host_id: &str,
        thread_id: &str,
        terminal_event_id: &str,
        now_ms: i64,
        failed: bool,
    ) -> Result<ThreadAttentionState, DeviceDatabaseError> {
        validate_id("host_id", host_id)?;
        validate_id("thread_id", thread_id)?;
        validate_id("terminal_event_id", terminal_event_id)?;
        validate_timestamp("now_ms", now_ms)?;
        let mut connection = self.lock()?;
        let previous = connection
            .query_row(
                "SELECT occurred_at_ms, terminal_event_id FROM thread_attention
                 WHERE host_id = ?1 AND thread_id = ?2",
                params![host_id, thread_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        if previous
            .as_ref()
            .is_some_and(|(_, previous_event_id)| previous_event_id == terminal_event_id)
        {
            drop(connection);
            return self
                .thread_attention_state(host_id, thread_id)?
                .ok_or_else(|| {
                    DeviceDatabaseError::Database(rusqlite::Error::QueryReturnedNoRows)
                });
        }
        let occurred_at_ms = previous
            .and_then(|(value, _)| value.checked_add(1))
            .map_or(now_ms, |minimum| now_ms.max(minimum));
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO thread_organization (
                 host_id, thread_id, pinned, hidden, snoozed_until_ms,
                 acknowledged_at_ms, relay_sequence, updated_at_ms
             ) VALUES (?1, ?2, 0, 0, NULL, NULL, 0, ?3)
             ON CONFLICT(host_id, thread_id) DO UPDATE SET
                 snoozed_until_ms = NULL,
                 acknowledged_at_ms = NULL,
                 updated_at_ms = MAX(thread_organization.updated_at_ms, excluded.updated_at_ms)",
            params![host_id, thread_id, occurred_at_ms],
        )?;
        transaction.execute(
            "INSERT INTO thread_attention (
                 host_id, thread_id, terminal_event_id, occurred_at_ms, failed
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(host_id, thread_id) DO UPDATE SET
                 terminal_event_id = excluded.terminal_event_id,
                 occurred_at_ms = excluded.occurred_at_ms,
                 failed = excluded.failed",
            params![
                host_id,
                thread_id,
                terminal_event_id,
                occurred_at_ms,
                i64::from(failed)
            ],
        )?;
        transaction.commit()?;
        drop(connection);
        self.thread_attention_state(host_id, thread_id)?
            .ok_or_else(|| DeviceDatabaseError::Database(rusqlite::Error::QueryReturnedNoRows))
    }

    pub fn acknowledge_thread_attention(
        &self,
        host_id: &str,
        thread_id: &str,
        now_ms: i64,
    ) -> Result<ThreadAttentionState, DeviceDatabaseError> {
        validate_id("host_id", host_id)?;
        validate_id("thread_id", thread_id)?;
        validate_timestamp("now_ms", now_ms)?;
        let connection = self.lock()?;
        connection.execute(
            "INSERT INTO thread_organization (
                 host_id, thread_id, pinned, hidden, snoozed_until_ms,
                 acknowledged_at_ms, relay_sequence, updated_at_ms
             ) VALUES (
                 ?1, ?2, 0, 0, NULL,
                 MAX(?3, COALESCE((
                     SELECT occurred_at_ms FROM thread_attention
                     WHERE host_id = ?1 AND thread_id = ?2
                 ), ?3)),
                 0, ?3
             )
             ON CONFLICT(host_id, thread_id) DO UPDATE SET
                 snoozed_until_ms = NULL,
                 acknowledged_at_ms = MAX(
                     ?3,
                     COALESCE((
                         SELECT occurred_at_ms FROM thread_attention
                         WHERE host_id = ?1 AND thread_id = ?2
                     ), ?3)
                 ),
                 updated_at_ms = MAX(thread_organization.updated_at_ms, ?3)",
            params![host_id, thread_id, now_ms],
        )?;
        drop(connection);
        self.thread_attention_state(host_id, thread_id)?
            .ok_or_else(|| DeviceDatabaseError::Database(rusqlite::Error::QueryReturnedNoRows))
    }

    pub fn snooze_thread_attention(
        &self,
        host_id: &str,
        thread_id: &str,
        now_ms: i64,
        snoozed_until_ms: i64,
    ) -> Result<ThreadAttentionState, DeviceDatabaseError> {
        validate_id("host_id", host_id)?;
        validate_id("thread_id", thread_id)?;
        validate_timestamp("now_ms", now_ms)?;
        if snoozed_until_ms <= now_ms
            || snoozed_until_ms.saturating_sub(now_ms) > MAX_SNOOZE_DURATION_MS
        {
            return Err(DeviceDatabaseError::InvalidInput(
                "snooze must end within the next 30 days".to_string(),
            ));
        }
        let connection = self.lock()?;
        connection.execute(
            "INSERT INTO thread_organization (
                 host_id, thread_id, pinned, hidden, snoozed_until_ms,
                 acknowledged_at_ms, relay_sequence, updated_at_ms
             ) VALUES (?1, ?2, 0, 0, ?3, NULL, 0, ?4)
             ON CONFLICT(host_id, thread_id) DO UPDATE SET
                 snoozed_until_ms = excluded.snoozed_until_ms,
                 updated_at_ms = MAX(thread_organization.updated_at_ms, excluded.updated_at_ms)",
            params![host_id, thread_id, snoozed_until_ms, now_ms],
        )?;
        drop(connection);
        self.thread_attention_state(host_id, thread_id)?
            .ok_or_else(|| DeviceDatabaseError::Database(rusqlite::Error::QueryReturnedNoRows))
    }

    pub fn delete_terminal_attention(
        &self,
        host_id: &str,
        thread_id: &str,
    ) -> Result<bool, DeviceDatabaseError> {
        validate_id("host_id", host_id)?;
        validate_id("thread_id", thread_id)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let deleted = transaction.execute(
            "DELETE FROM thread_attention WHERE host_id = ?1 AND thread_id = ?2",
            params![host_id, thread_id],
        )? == 1;
        transaction.execute(
            "UPDATE thread_organization SET snoozed_until_ms = NULL
             WHERE host_id = ?1 AND thread_id = ?2",
            params![host_id, thread_id],
        )?;
        transaction.commit()?;
        Ok(deleted)
    }

    pub fn thread_attention_states(
        &self,
        limit: usize,
    ) -> Result<Vec<ThreadAttentionState>, DeviceDatabaseError> {
        let limit = limit.clamp(1, MAX_THREAD_ATTENTION_ROWS) as i64;
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT organization.host_id, organization.thread_id,
                    attention.terminal_event_id, attention.occurred_at_ms,
                    COALESCE(attention.failed, 0),
                    organization.snoozed_until_ms, organization.acknowledged_at_ms
             FROM thread_organization AS organization
             LEFT JOIN thread_attention AS attention
               ON attention.host_id = organization.host_id
              AND attention.thread_id = organization.thread_id
             ORDER BY organization.updated_at_ms DESC,
                      organization.host_id ASC, organization.thread_id ASC
             LIMIT ?1",
        )?;
        let rows = statement.query_map([limit], decode_thread_attention_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn thread_attention_state(
        &self,
        host_id: &str,
        thread_id: &str,
    ) -> Result<Option<ThreadAttentionState>, DeviceDatabaseError> {
        validate_id("host_id", host_id)?;
        validate_id("thread_id", thread_id)?;
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT organization.host_id, organization.thread_id,
                        attention.terminal_event_id, attention.occurred_at_ms,
                        COALESCE(attention.failed, 0),
                        organization.snoozed_until_ms, organization.acknowledged_at_ms
                 FROM thread_organization AS organization
                 LEFT JOIN thread_attention AS attention
                   ON attention.host_id = organization.host_id
                  AND attention.thread_id = organization.thread_id
                 WHERE organization.host_id = ?1 AND organization.thread_id = ?2",
                params![host_id, thread_id],
                decode_thread_attention_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn upsert_review_note(&self, note: &ReviewNote) -> Result<(), DeviceDatabaseError> {
        validate_review_note(note)?;
        let encrypted_note = EncryptedReviewNote {
            path: note.path.clone(),
            start_line: note.start_line,
            end_line: note.end_line,
            body: note.body.clone(),
        };
        let plaintext = serde_json::to_vec(&encrypted_note)
            .map_err(|error| DeviceDatabaseError::InvalidInput(error.to_string()))?;
        let (nonce, ciphertext) =
            self.encrypt("review_note", &note.note_id, &note.host_id, &plaintext)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let existing = transaction
            .query_row(
                "SELECT host_id, thread_id, checkpoint_id, nonce,
                        encrypted_note, updated_at_ms
                 FROM review_notes WHERE note_id = ?1",
                [&note.note_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Vec<u8>>(3)?,
                        row.get::<_, Vec<u8>>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            )
            .optional()?;
        if let Some((host_id, thread_id, checkpoint_id, old_nonce, old_ciphertext, updated_at)) =
            existing
        {
            let old_payload = self.decrypt(
                "review_note",
                &note.note_id,
                &host_id,
                &old_nonce,
                &old_ciphertext,
            )?;
            let old_note: EncryptedReviewNote = serde_json::from_slice(&old_payload)
                .map_err(|_| DeviceDatabaseError::Authentication)?;
            if host_id != note.host_id
                || thread_id != note.thread_id
                || checkpoint_id != note.checkpoint_id
                || old_note.path != note.path
                || old_note.start_line != note.start_line
                || old_note.end_line != note.end_line
            {
                return Err(DeviceDatabaseError::InvalidInput(
                    "review note anchor is immutable".to_string(),
                ));
            }
            if note.updated_at_ms < updated_at {
                return Err(DeviceDatabaseError::InvalidInput(
                    "review note update is stale".to_string(),
                ));
            }
        }
        transaction.execute(
            "INSERT INTO review_notes (
                 note_id, host_id, thread_id, checkpoint_id, nonce,
                 encrypted_note, state, created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(note_id) DO UPDATE SET
                 nonce = excluded.nonce,
                 encrypted_note = excluded.encrypted_note,
                 state = excluded.state,
                 updated_at_ms = excluded.updated_at_ms",
            params![
                note.note_id,
                note.host_id,
                note.thread_id,
                note.checkpoint_id,
                nonce.as_slice(),
                ciphertext,
                note.state.as_i64(),
                note.created_at_ms,
                note.updated_at_ms,
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn review_notes_for_thread(
        &self,
        host_id: &str,
        thread_id: &str,
        limit: usize,
    ) -> Result<Vec<ReviewNote>, DeviceDatabaseError> {
        validate_id("host_id", host_id)?;
        validate_id("thread_id", thread_id)?;
        let limit = limit.clamp(1, MAX_REVIEW_NOTES) as i64;
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT note_id, checkpoint_id, nonce, encrypted_note, state,
                    created_at_ms, updated_at_ms
             FROM review_notes
             WHERE host_id = ?1 AND thread_id = ?2
             ORDER BY updated_at_ms DESC, note_id ASC
             LIMIT ?3",
        )?;
        let rows = statement.query_map(params![host_id, thread_id, limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Vec<u8>>(2)?,
                row.get::<_, Vec<u8>>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })?;
        let mut notes = Vec::new();
        for row in rows {
            let (note_id, checkpoint_id, nonce, ciphertext, state, created_at_ms, updated_at_ms) =
                row?;
            let plaintext = self.decrypt("review_note", &note_id, host_id, &nonce, &ciphertext)?;
            let encrypted: EncryptedReviewNote = serde_json::from_slice(&plaintext)
                .map_err(|_| DeviceDatabaseError::Authentication)?;
            notes.push(ReviewNote {
                note_id,
                host_id: host_id.to_string(),
                thread_id: thread_id.to_string(),
                checkpoint_id,
                path: encrypted.path,
                start_line: encrypted.start_line,
                end_line: encrypted.end_line,
                body: encrypted.body,
                state: ReviewNoteState::from_i64(state)?,
                created_at_ms,
                updated_at_ms,
            });
        }
        Ok(notes)
    }

    pub fn set_review_note_state(
        &self,
        note_id: &str,
        state: ReviewNoteState,
        updated_at_ms: i64,
    ) -> Result<bool, DeviceDatabaseError> {
        validate_id("note_id", note_id)?;
        let connection = self.lock()?;
        Ok(connection.execute(
            "UPDATE review_notes SET state = ?2, updated_at_ms = ?3
             WHERE note_id = ?1 AND updated_at_ms <= ?3",
            params![note_id, state.as_i64(), updated_at_ms],
        )? == 1)
    }

    pub fn delete_review_note(&self, note_id: &str) -> Result<bool, DeviceDatabaseError> {
        validate_id("note_id", note_id)?;
        let connection = self.lock()?;
        Ok(connection.execute("DELETE FROM review_notes WHERE note_id = ?1", [note_id])? == 1)
    }

    pub fn index_search_document(
        &self,
        document_id: &str,
        host_id: &str,
        thread_id: &str,
        body: &str,
        updated_at_ms: i64,
        protected: bool,
    ) -> Result<(), DeviceDatabaseError> {
        self.index_search_documents(
            &[SearchDocumentInput {
                document_id: document_id.to_string(),
                host_id: host_id.to_string(),
                thread_id: thread_id.to_string(),
                body: body.to_string(),
                search_text: body.to_string(),
                updated_at_ms,
                protected,
            }],
            updated_at_ms.max(1),
        )
    }

    /// Indexes a bounded summary batch in one transaction. `search_text`
    /// contains only user-visible values while `body` may carry a typed,
    /// encrypted Rust-owned payload used to reconstruct cached projections.
    pub fn index_search_documents(
        &self,
        documents: &[SearchDocumentInput],
        indexed_at_ms: i64,
    ) -> Result<(), DeviceDatabaseError> {
        if documents.is_empty() {
            return Ok(());
        }
        if documents.len() > MAX_SEARCH_INDEX_BATCH {
            return Err(DeviceDatabaseError::InvalidInput(format!(
                "search index batch exceeds {MAX_SEARCH_INDEX_BATCH} documents"
            )));
        }
        validate_timestamp("indexed_at_ms", indexed_at_ms)?;

        let mut prepared = Vec::with_capacity(documents.len());
        let mut indexed_hosts = BTreeSet::new();
        for document in documents {
            validate_id("document_id", &document.document_id)?;
            validate_id("host_id", &document.host_id)?;
            validate_id("thread_id", &document.thread_id)?;
            if document.body.len() > MAX_SEARCH_BODY_BYTES
                || document.search_text.len() > MAX_SEARCH_BODY_BYTES
            {
                return Err(DeviceDatabaseError::InvalidInput(
                    "search document exceeds 1 MiB".to_string(),
                ));
            }
            let (nonce, ciphertext) = self.encrypt(
                "search_document",
                &document.document_id,
                &document.host_id,
                document.body.as_bytes(),
            )?;
            let terms = posting_terms(&document.search_text)
                .into_iter()
                .map(|term| self.term_hash(&term))
                .collect::<Result<BTreeSet<_>, _>>()?;
            indexed_hosts.insert(document.host_id.clone());
            prepared.push((document, nonce, ciphertext, terms));
        }

        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        {
            let mut upsert = transaction.prepare(
                "INSERT INTO search_documents (
                     document_id, host_id, thread_id, nonce, encrypted_body,
                     updated_at_ms, protected
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(document_id) DO UPDATE SET
                     host_id = excluded.host_id,
                     thread_id = excluded.thread_id,
                     nonce = excluded.nonce,
                     encrypted_body = excluded.encrypted_body,
                     updated_at_ms = excluded.updated_at_ms,
                     protected = excluded.protected",
            )?;
            let mut delete_terms =
                transaction.prepare("DELETE FROM search_terms WHERE document_id = ?1")?;
            let mut insert = transaction
                .prepare("INSERT INTO search_terms(term_hash, document_id) VALUES (?1, ?2)")?;
            for (document, nonce, ciphertext, terms) in prepared {
                upsert.execute(params![
                    document.document_id,
                    document.host_id,
                    document.thread_id,
                    nonce.as_slice(),
                    ciphertext,
                    document.updated_at_ms,
                    i64::from(document.protected),
                ])?;
                delete_terms.execute([document.document_id.as_str()])?;
                for term_hash in terms {
                    insert.execute(params![term_hash.as_slice(), document.document_id])?;
                }
            }
        }
        {
            let mut upsert_host = transaction.prepare(
                "INSERT INTO search_hosts(host_id, last_indexed_at_ms)
                 VALUES (?1, ?2)
                 ON CONFLICT(host_id) DO UPDATE SET
                     last_indexed_at_ms = MAX(
                         search_hosts.last_indexed_at_ms,
                         excluded.last_indexed_at_ms
                     )",
            )?;
            for host_id in indexed_hosts {
                upsert_host.execute(params![host_id, indexed_at_ms])?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn delete_search_documents(
        &self,
        document_ids: &[String],
    ) -> Result<usize, DeviceDatabaseError> {
        if document_ids.is_empty() {
            return Ok(0);
        }
        if document_ids.len() > MAX_SEARCH_INDEX_BATCH {
            return Err(DeviceDatabaseError::InvalidInput(format!(
                "search delete batch exceeds {MAX_SEARCH_INDEX_BATCH} documents"
            )));
        }
        for document_id in document_ids {
            validate_id("document_id", document_id)?;
        }
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let mut deleted = 0;
        {
            let mut delete =
                transaction.prepare("DELETE FROM search_documents WHERE document_id = ?1")?;
            for document_id in document_ids {
                deleted += delete.execute([document_id])?;
            }
        }
        transaction.commit()?;
        Ok(deleted)
    }

    pub fn search(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchResult>, DeviceDatabaseError> {
        if query.len() > MAX_SEARCH_QUERY_BYTES {
            return Err(DeviceDatabaseError::InvalidInput(
                "search query exceeds 4096 bytes".to_string(),
            ));
        }
        let query_tokens = normalized_query_tokens(query);
        if query_tokens.is_empty() {
            return Ok(Vec::new());
        }
        let term_hashes = query_tokens
            .iter()
            .map(|token| self.term_hash(token))
            .collect::<Result<Vec<_>, _>>()?;
        let placeholders = std::iter::repeat("?")
            .take(term_hashes.len())
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            "SELECT document.document_id, document.host_id, document.thread_id,
                    document.nonce, document.encrypted_body, document.updated_at_ms
             FROM search_documents AS document
             JOIN (
                 SELECT document_id
                 FROM search_terms
                 WHERE term_hash IN ({placeholders})
                 GROUP BY document_id
                 HAVING COUNT(*) = ?
                 ORDER BY document_id
                 LIMIT ?
             ) AS candidate ON candidate.document_id = document.document_id
             ORDER BY document.updated_at_ms DESC, document.document_id ASC"
        );
        let mut parameters = term_hashes
            .iter()
            .map(|term_hash| Value::Blob(term_hash.to_vec()))
            .collect::<Vec<_>>();
        parameters.push(Value::Integer(term_hashes.len() as i64));
        parameters.push(Value::Integer(MAX_SEARCH_CANDIDATES as i64));
        let connection = self.lock()?;
        let mut statement = connection.prepare(&query)?;
        let candidates = statement
            .query_map(params_from_iter(parameters), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let limit = limit.clamp(1, MAX_SEARCH_RESULTS);
        let mut results = Vec::new();
        for (document_id, host_id, thread_id, nonce, ciphertext, updated_at_ms) in candidates {
            let body = self.decrypt(
                "search_document",
                &document_id,
                &host_id,
                &nonce,
                &ciphertext,
            )?;
            let body = String::from_utf8(body).map_err(|_| DeviceDatabaseError::Authentication)?;
            if search_tokens_match(&query_tokens, &body) {
                results.push(SearchResult {
                    document_id,
                    host_id,
                    thread_id,
                    snippet: bound_utf8(body.trim(), MAX_SNIPPET_BYTES),
                    body,
                    updated_at_ms,
                });
                if results.len() == limit {
                    break;
                }
            }
        }
        results.sort_by(|left, right| right.updated_at_ms.cmp(&left.updated_at_ms));
        Ok(results)
    }

    pub fn clear_host_search_index(&self, host_id: &str) -> Result<usize, DeviceDatabaseError> {
        validate_id("host_id", host_id)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let deleted =
            transaction.execute("DELETE FROM search_documents WHERE host_id = ?1", [host_id])?;
        transaction.execute("DELETE FROM search_hosts WHERE host_id = ?1", [host_id])?;
        transaction.commit()?;
        Ok(deleted)
    }

    pub fn search_host_states(
        &self,
        limit: usize,
    ) -> Result<Vec<SearchHostState>, DeviceDatabaseError> {
        let limit = limit.clamp(1, MAX_SEARCH_HOST_STATES) as i64;
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT host_id, last_indexed_at_ms
             FROM search_hosts
             ORDER BY last_indexed_at_ms DESC, host_id ASC
             LIMIT ?1",
        )?;
        let rows = statement.query_map([limit], |row| {
            Ok(SearchHostState {
                host_id: row.get(0)?,
                last_indexed_at_ms: row.get(1)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn prune_search_before(
        &self,
        before_ms: i64,
        limit: usize,
    ) -> Result<usize, DeviceDatabaseError> {
        let limit = limit.clamp(1, MAX_RETENTION_DELETIONS) as i64;
        let connection = self.lock()?;
        Ok(connection.execute(
            "DELETE FROM search_documents WHERE document_id IN (
                 SELECT document_id FROM search_documents AS document
                 WHERE protected = 0
                   AND updated_at_ms < ?1
                   AND NOT EXISTS (
                       SELECT 1 FROM thread_organization AS organization
                       WHERE organization.host_id = document.host_id
                         AND organization.thread_id = document.thread_id
                         AND organization.pinned = 1
                   )
                   AND NOT EXISTS (
                       SELECT 1 FROM outbox AS intent
                       WHERE intent.host_id = document.host_id
                         AND intent.thread_id = document.thread_id
                   )
                   AND NOT EXISTS (
                       SELECT 1 FROM review_notes AS note
                       WHERE note.host_id = document.host_id
                         AND note.thread_id = document.thread_id
                         AND note.state = 0
                   )
                 ORDER BY updated_at_ms ASC, document_id ASC
                 LIMIT ?2
             )",
            params![before_ms, limit],
        )?)
    }

    /// Enforces both the 90-day age limit and the 2 GiB logical index budget.
    /// SQLite page allocation is intentionally excluded because the database
    /// also contains protected outbox/review state and page reuse is safe.
    pub fn enforce_search_retention(&self, now_ms: i64) -> Result<usize, DeviceDatabaseError> {
        let before_ms = now_ms.saturating_sub(SEARCH_RETENTION_MS);
        let age_deleted = self.prune_search_before(before_ms, MAX_RETENTION_DELETIONS)?;
        let size_deleted =
            self.prune_search_to_bytes(SEARCH_RETENTION_BYTES, MAX_RETENTION_DELETIONS)?;
        Ok(age_deleted.saturating_add(size_deleted))
    }

    fn prune_search_to_bytes(
        &self,
        maximum_bytes: u64,
        limit: usize,
    ) -> Result<usize, DeviceDatabaseError> {
        let maximum_bytes = i64::try_from(maximum_bytes).map_err(|_| {
            DeviceDatabaseError::InvalidInput("search retention budget is too large".to_string())
        })?;
        let limit = limit.clamp(1, MAX_RETENTION_DELETIONS) as i64;
        let mut connection = self.lock()?;
        let logical_bytes = connection.query_row(
            "SELECT
                 COALESCE((SELECT SUM(length(nonce) + length(encrypted_body))
                           FROM search_documents), 0)
               + COALESCE((SELECT SUM(length(term_hash) + length(document_id))
                           FROM search_terms), 0)",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        if logical_bytes <= maximum_bytes {
            return Ok(0);
        }
        let bytes_to_remove = logical_bytes.saturating_sub(maximum_bytes);
        let candidates = {
            let mut statement = connection.prepare(
                "SELECT document.document_id,
                        length(document.nonce) + length(document.encrypted_body)
                        + COALESCE((
                            SELECT SUM(length(term.term_hash) + length(term.document_id))
                            FROM search_terms AS term
                            WHERE term.document_id = document.document_id
                        ), 0) AS logical_bytes
                 FROM search_documents AS document
                 WHERE document.protected = 0
                   AND NOT EXISTS (
                       SELECT 1 FROM thread_organization AS organization
                       WHERE organization.host_id = document.host_id
                         AND organization.thread_id = document.thread_id
                         AND organization.pinned = 1
                   )
                   AND NOT EXISTS (
                       SELECT 1 FROM outbox AS intent
                       WHERE intent.host_id = document.host_id
                         AND intent.thread_id = document.thread_id
                   )
                   AND NOT EXISTS (
                       SELECT 1 FROM review_notes AS note
                       WHERE note.host_id = document.host_id
                         AND note.thread_id = document.thread_id
                         AND note.state = 0
                   )
                 ORDER BY document.updated_at_ms ASC, document.document_id ASC
                 LIMIT ?1",
            )?;
            statement
                .query_map([limit], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut selected = Vec::new();
        let mut selected_bytes = 0_i64;
        for (document_id, document_bytes) in candidates {
            if document_bytes < 0 {
                return Err(DeviceDatabaseError::Authentication);
            }
            selected.push(document_id);
            selected_bytes = selected_bytes.saturating_add(document_bytes);
            if selected_bytes >= bytes_to_remove {
                break;
            }
        }
        let transaction = connection.transaction()?;
        let mut deleted = 0;
        {
            let mut delete =
                transaction.prepare("DELETE FROM search_documents WHERE document_id = ?1")?;
            for document_id in &selected {
                deleted += delete.execute([document_id])?;
            }
        }
        transaction.commit()?;
        Ok(deleted)
    }

    fn verify_or_create_key_check(&self) -> Result<(), DeviceDatabaseError> {
        let connection = self.lock()?;
        let nonce = connection
            .query_row(
                "SELECT value FROM meta WHERE key = 'key_check_nonce'",
                [],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()?;
        let ciphertext = connection
            .query_row(
                "SELECT value FROM meta WHERE key = 'key_check_ciphertext'",
                [],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()?;
        match (nonce, ciphertext) {
            (Some(nonce), Some(ciphertext)) => {
                let plaintext = self.decrypt("meta", "key-check", "device", &nonce, &ciphertext)?;
                if plaintext != b"remora-device-database-v1" {
                    return Err(DeviceDatabaseError::Authentication);
                }
            }
            (None, None) => {
                let (nonce, ciphertext) =
                    self.encrypt("meta", "key-check", "device", b"remora-device-database-v1")?;
                connection.execute(
                    "INSERT INTO meta(key, value) VALUES ('key_check_nonce', ?1)",
                    [nonce.as_slice()],
                )?;
                connection.execute(
                    "INSERT INTO meta(key, value) VALUES ('key_check_ciphertext', ?1)",
                    [ciphertext],
                )?;
            }
            _ => return Err(DeviceDatabaseError::Authentication),
        }
        Ok(())
    }

    fn set_schema_version(&self) -> Result<(), DeviceDatabaseError> {
        let connection = self.lock()?;
        let existing = connection
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            if [2_i64, 3, 4]
                .into_iter()
                .any(|version| existing == version.to_be_bytes())
            {
                connection.execute(
                    "UPDATE meta SET value = ?1 WHERE key = 'schema_version'",
                    [SCHEMA_VERSION.to_be_bytes().as_slice()],
                )?;
            } else if existing != SCHEMA_VERSION.to_be_bytes() {
                return Err(DeviceDatabaseError::Authentication);
            }
        } else {
            connection.execute(
                "INSERT INTO meta(key, value) VALUES ('schema_version', ?1)",
                [SCHEMA_VERSION.to_be_bytes().as_slice()],
            )?;
        }
        Ok(())
    }

    fn encrypt(
        &self,
        record_type: &str,
        primary_key: &str,
        host_id: &str,
        plaintext: &[u8],
    ) -> Result<([u8; NONCE_BYTES], Vec<u8>), DeviceDatabaseError> {
        let mut nonce = [0_u8; NONCE_BYTES];
        OsRng.fill_bytes(&mut nonce);
        let cipher = XChaCha20Poly1305::new(Key::from_slice(self.master_key.as_ref()));
        let aad = associated_data(record_type, primary_key, host_id);
        let ciphertext = cipher
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| DeviceDatabaseError::Authentication)?;
        Ok((nonce, ciphertext))
    }

    fn decrypt(
        &self,
        record_type: &str,
        primary_key: &str,
        host_id: &str,
        nonce: &[u8],
        ciphertext: &[u8],
    ) -> Result<Vec<u8>, DeviceDatabaseError> {
        if nonce.len() != NONCE_BYTES {
            return Err(DeviceDatabaseError::Authentication);
        }
        let cipher = XChaCha20Poly1305::new(Key::from_slice(self.master_key.as_ref()));
        let aad = associated_data(record_type, primary_key, host_id);
        cipher
            .decrypt(
                XNonce::from_slice(nonce),
                Payload {
                    msg: ciphertext,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| DeviceDatabaseError::Authentication)
    }

    fn term_hash(&self, token: &str) -> Result<[u8; 32], DeviceDatabaseError> {
        let mut key_derivation = <HmacSha256 as Mac>::new_from_slice(self.master_key.as_ref())
            .map_err(|_| DeviceDatabaseError::InvalidKey)?;
        key_derivation.update(b"remora-search-postings-v1");
        let search_key = Zeroizing::new(key_derivation.finalize().into_bytes());
        let mut mac = <HmacSha256 as Mac>::new_from_slice(search_key.as_slice())
            .map_err(|_| DeviceDatabaseError::InvalidKey)?;
        mac.update(token.as_bytes());
        Ok(mac.finalize().into_bytes().into())
    }

    fn thread_binding_hash(
        &self,
        host_id: &str,
        provider_thread_id: &str,
    ) -> Result<[u8; 32], DeviceDatabaseError> {
        let mut mac = <HmacSha256 as Mac>::new_from_slice(self.master_key.as_ref())
            .map_err(|_| DeviceDatabaseError::InvalidKey)?;
        mac.update(b"remora-thread-binding-v1\0");
        mac.update(host_id.as_bytes());
        mac.update(b"\0");
        mac.update(provider_thread_id.as_bytes());
        Ok(mac.finalize().into_bytes().into())
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, DeviceDatabaseError> {
        self.connection
            .lock()
            .map_err(|_| DeviceDatabaseError::Poisoned)
    }
}

fn take_key(bytes: &mut Vec<u8>) -> Result<Zeroizing<[u8; KEY_BYTES]>, DeviceDatabaseError> {
    if bytes.len() != KEY_BYTES {
        bytes.zeroize();
        return Err(DeviceDatabaseError::InvalidKey);
    }
    let mut key = Zeroizing::new([0_u8; KEY_BYTES]);
    key.copy_from_slice(bytes);
    bytes.zeroize();
    Ok(key)
}

fn associated_data(record_type: &str, primary_key: &str, host_id: &str) -> String {
    format!("{ENCRYPTION_RECORD_VERSION}|{record_type}|{primary_key}|{host_id}")
}

fn decode_thread_attention_row(
    row: &rusqlite::Row<'_>,
) -> Result<ThreadAttentionState, rusqlite::Error> {
    let failed = row.get::<_, i64>(4)?;
    if !matches!(failed, 0 | 1) {
        return Err(rusqlite::Error::IntegralValueOutOfRange(4, failed));
    }
    Ok(ThreadAttentionState {
        host_id: row.get(0)?,
        thread_id: row.get(1)?,
        terminal_event_id: row.get(2)?,
        occurred_at_ms: row.get(3)?,
        failed: failed == 1,
        snoozed_until_ms: row.get(5)?,
        acknowledged_at_ms: row.get(6)?,
    })
}

fn validate_timestamp(name: &str, value: i64) -> Result<(), DeviceDatabaseError> {
    if value <= 0 {
        return Err(DeviceDatabaseError::InvalidInput(format!(
            "{name} must be a positive Unix timestamp"
        )));
    }
    Ok(())
}

fn validate_id(name: &str, value: &str) -> Result<(), DeviceDatabaseError> {
    if value.is_empty() || value.len() > MAX_ID_BYTES || value.chars().any(char::is_control) {
        return Err(DeviceDatabaseError::InvalidInput(format!(
            "{name} must contain 1..={MAX_ID_BYTES} display-safe bytes"
        )));
    }
    Ok(())
}

fn validate_provider_thread_id(value: &str) -> Result<(), DeviceDatabaseError> {
    if value.is_empty()
        || value.len() > MAX_PROVIDER_THREAD_ID_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(DeviceDatabaseError::InvalidInput(format!(
            "provider_thread_id must contain 1..={MAX_PROVIDER_THREAD_ID_BYTES} display-safe bytes"
        )));
    }
    Ok(())
}

fn decode_bool(value: i64) -> Result<bool, DeviceDatabaseError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(DeviceDatabaseError::Authentication),
    }
}

fn validate_review_note(note: &ReviewNote) -> Result<(), DeviceDatabaseError> {
    validate_id("note_id", &note.note_id)?;
    validate_id("host_id", &note.host_id)?;
    validate_id("thread_id", &note.thread_id)?;
    validate_id("checkpoint_id", &note.checkpoint_id)?;
    if note.path.is_empty()
        || note.path.len() > MAX_REVIEW_PATH_BYTES
        || note.path.starts_with('/')
        || note.path.contains('\\')
        || note.path.chars().any(char::is_control)
        || note
            .path
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(DeviceDatabaseError::InvalidInput(
            "review path must be a confined relative path".to_string(),
        ));
    }
    if note.start_line == 0 || note.end_line < note.start_line {
        return Err(DeviceDatabaseError::InvalidInput(
            "review line range is invalid".to_string(),
        ));
    }
    if note.body.len() > MAX_REVIEW_BODY_BYTES || note.body.trim().is_empty() {
        return Err(DeviceDatabaseError::InvalidInput(
            "review body must contain 1..=65536 bytes".to_string(),
        ));
    }
    if note.updated_at_ms < note.created_at_ms {
        return Err(DeviceDatabaseError::InvalidInput(
            "review update precedes creation".to_string(),
        ));
    }
    Ok(())
}

fn normalized_tokens(value: &str) -> Vec<String> {
    value
        .split(|character: char| !character.is_alphanumeric())
        .filter_map(|token| {
            let normalized = token
                .chars()
                .flat_map(char::to_lowercase)
                .take(MAX_TOKEN_CHARS)
                .collect::<String>();
            (!normalized.is_empty()).then_some(normalized)
        })
        .collect()
}

fn normalized_query_tokens(value: &str) -> Vec<String> {
    normalized_tokens(value).into_iter().fold(
        Vec::with_capacity(MAX_QUERY_TOKENS),
        |mut tokens, token| {
            if tokens.len() < MAX_QUERY_TOKENS && !tokens.contains(&token) {
                tokens.push(token);
            }
            tokens
        },
    )
}

fn search_tokens_match(query_tokens: &[String], candidate: &str) -> bool {
    let candidate_tokens = normalized_tokens(candidate);
    query_tokens.iter().all(|token| {
        candidate_tokens
            .iter()
            .any(|candidate| candidate.starts_with(token))
    })
}

pub(crate) fn search_text_matches(query: &str, candidate: &str) -> bool {
    let query_tokens = normalized_query_tokens(query);
    !query_tokens.is_empty() && search_tokens_match(&query_tokens, candidate)
}

fn posting_terms(value: &str) -> BTreeSet<String> {
    let mut terms = BTreeSet::new();
    for token in normalized_tokens(value) {
        terms.insert(token.clone());
        let characters = token.chars().collect::<Vec<_>>();
        for length in 2..=characters.len().min(MAX_PREFIX_CHARS) {
            terms.insert(characters[..length].iter().collect());
        }
    }
    terms
}

fn bound_utf8(value: &str, maximum: usize) -> String {
    if value.len() <= maximum {
        return value.to_string();
    }
    let mut end = maximum;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn key(byte: u8) -> Vec<u8> {
        vec![byte; KEY_BYTES]
    }

    fn intent(id: &str, host_id: &str, payload: &[u8]) -> OutboxIntent {
        OutboxIntent {
            intent_id: id.to_string(),
            host_id: host_id.to_string(),
            thread_id: Some("thread".to_string()),
            kind: OutboxIntentKind::SendMessage,
            payload: payload.to_vec(),
            state: OutboxState::Queued,
            created_at_ms: 1,
            attempt_count: 0,
            next_attempt_at_ms: None,
        }
    }

    fn organization(sequence: u64, pinned: bool) -> ThreadOrganization {
        ThreadOrganization {
            host_id: "host".to_string(),
            thread_id: "thread".to_string(),
            pinned,
            hidden: false,
            snoozed_until_ms: None,
            acknowledged_at_ms: None,
            relay_sequence: sequence,
            updated_at_ms: sequence as i64,
        }
    }

    fn review_note(note_id: &str, thread_id: &str) -> ReviewNote {
        ReviewNote {
            note_id: note_id.to_string(),
            host_id: "host".to_string(),
            thread_id: thread_id.to_string(),
            checkpoint_id: "checkpoint".to_string(),
            path: "src/main.rs".to_string(),
            start_line: 3,
            end_line: 5,
            body: "Please simplify this branch.".to_string(),
            state: ReviewNoteState::Open,
            created_at_ms: 1,
            updated_at_ms: 1,
        }
    }

    #[test]
    fn wrong_key_fails_when_reopening_database() {
        let directory = tempdir().expect("tempdir");
        let path = directory.path().join("device.sqlite");
        drop(DeviceDatabase::open(&path, key(1)).expect("open"));

        assert!(matches!(
            DeviceDatabase::open(&path, key(2)),
            Err(DeviceDatabaseError::Authentication)
        ));
    }

    #[test]
    fn outbox_enqueue_is_idempotent_and_acknowledgement_driven() {
        let database = DeviceDatabase::open_in_memory(key(3)).expect("open");
        assert!(
            database
                .enqueue_outbox(&intent("intent", "host", b"first"))
                .unwrap()
        );
        assert!(
            !database
                .enqueue_outbox(&intent("intent", "host", b"second"))
                .unwrap()
        );

        let due = database.due_outbox(10, usize::MAX).expect("due");
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].payload, b"first");
        assert!(database.acknowledge_outbox("intent").unwrap());
        assert!(database.due_outbox(10, 100).unwrap().is_empty());
    }

    #[test]
    fn retry_deadline_tracks_the_earliest_deliverable_intent() {
        let database = DeviceDatabase::open_in_memory(key(34)).expect("open");
        database
            .enqueue_outbox(&intent("intent", "host", b"payload"))
            .expect("enqueue");
        assert_eq!(database.next_outbox_attempt_at_ms().unwrap(), Some(0));
        assert!(database.record_outbox_attempt("intent", 123).unwrap());
        assert_eq!(database.next_outbox_attempt_at_ms().unwrap(), Some(123));
        assert!(database.record_outbox_outcome_unknown("intent").unwrap());
        assert_eq!(database.next_outbox_attempt_at_ms().unwrap(), None);
    }

    #[test]
    fn retry_backoff_preserves_per_thread_fifo_without_blocking_other_threads() {
        let database = DeviceDatabase::open_in_memory(key(38)).expect("open");
        let first = intent("intent-1", "host", b"first");
        let mut second = intent("intent-2", "host", b"second");
        second.created_at_ms = 2;
        let mut other = intent("intent-3", "host", b"other");
        other.thread_id = Some("other-thread".to_string());
        other.created_at_ms = 3;
        database.enqueue_outbox(&first).unwrap();
        database.enqueue_outbox(&second).unwrap();
        database.enqueue_outbox(&other).unwrap();
        assert!(database.record_outbox_attempt("intent-1", 100).unwrap());

        let due = database.due_outbox(10, 100).unwrap();
        assert_eq!(
            due.iter()
                .map(|intent| intent.intent_id.as_str())
                .collect::<Vec<_>>(),
            vec!["intent-3"]
        );
        assert_eq!(database.next_outbox_attempt_at_ms().unwrap(), Some(0));
        database.acknowledge_outbox("intent-3").unwrap();
        assert_eq!(database.next_outbox_attempt_at_ms().unwrap(), Some(100));
        assert!(database.due_outbox(99, 100).unwrap().is_empty());
        assert_eq!(
            database.due_outbox(100, 100).unwrap()[0].intent_id,
            "intent-1"
        );
        database.acknowledge_outbox("intent-1").unwrap();
        assert_eq!(
            database.due_outbox(100, 100).unwrap()[0].intent_id,
            "intent-2"
        );
    }

    #[test]
    fn outcome_unknown_is_terminal_and_blocks_only_its_thread() {
        let database = DeviceDatabase::open_in_memory(key(31)).expect("open");
        let first = intent("intent-1", "host", b"first");
        let mut second = intent("intent-2", "host", b"second");
        second.created_at_ms = 2;
        let mut other_thread = intent("intent-3", "host", b"other");
        other_thread.thread_id = Some("other-thread".to_string());
        other_thread.created_at_ms = 3;
        database.enqueue_outbox(&first).unwrap();
        database.enqueue_outbox(&second).unwrap();
        database.enqueue_outbox(&other_thread).unwrap();

        assert!(database.record_outbox_outcome_unknown("intent-1").unwrap());
        assert!(!database.record_outbox_outcome_unknown("intent-1").unwrap());
        assert!(!database.record_outbox_attempt("intent-1", 100).unwrap());
        assert_eq!(
            database.outbox_thread_status("host", "thread").unwrap(),
            OutboxThreadStatus {
                queued_count: 1,
                outcome_unknown_count: 1,
            }
        );

        let due = database.due_outbox(10, 100).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].intent_id, "intent-3");

        assert_eq!(
            database
                .discard_outbox_outcome_unknown("host", "thread", 1)
                .unwrap(),
            1
        );
        assert_eq!(
            database.outbox_thread_status("host", "thread").unwrap(),
            OutboxThreadStatus {
                queued_count: 1,
                outcome_unknown_count: 0,
            }
        );
        assert_eq!(
            database.due_outbox(10, 100).unwrap()[0].intent_id,
            "intent-2"
        );
    }

    #[test]
    fn cold_relaunch_restores_encrypted_binding_and_outbox_fences() {
        let directory = tempdir().expect("tempdir");
        let path = directory.path().join("device.sqlite");
        let database = DeviceDatabase::open(&path, key(32)).expect("open");
        database
            .upsert_thread_binding("host", "provider-thread", "host-thread", 1)
            .expect("persist binding");
        let mut uncertain = intent("intent-1", "host", b"first");
        uncertain.thread_id = Some("host-thread".to_string());
        let mut queued = intent("intent-2", "host", b"second");
        queued.thread_id = Some("host-thread".to_string());
        queued.created_at_ms = 2;
        database.enqueue_outbox(&uncertain).expect("enqueue first");
        database.enqueue_outbox(&queued).expect("enqueue second");
        database
            .record_outbox_outcome_unknown("intent-1")
            .expect("retain uncertain copy");
        assert_eq!(database.next_outbox_attempt_at_ms().unwrap(), None);
        drop(database);

        let raw = std::fs::read(&path).expect("read database");
        assert!(
            !raw.windows(b"provider-thread".len())
                .any(|window| window == b"provider-thread"),
            "provider identity must not be plaintext in the device database"
        );

        let reopened = DeviceDatabase::open(&path, key(32)).expect("reopen");
        assert_eq!(
            reopened
                .host_thread_id_for_provider("host", "provider-thread")
                .expect("lookup binding"),
            Some("host-thread".to_string())
        );
        assert_eq!(
            reopened
                .outbox_thread_status("host", "host-thread")
                .expect("cold status"),
            OutboxThreadStatus {
                queued_count: 1,
                outcome_unknown_count: 1,
            }
        );
        assert_eq!(
            reopened
                .discard_outbox_outcome_unknown("host", "host-thread", 2)
                .expect("reject stale proof"),
            0
        );
        assert_eq!(
            reopened
                .discard_outbox_outcome_unknown("host", "host-thread", 1)
                .expect("discard proven copy"),
            1
        );
        assert_eq!(reopened.next_outbox_attempt_at_ms().unwrap(), Some(0));
    }

    #[test]
    fn thread_binding_ciphertext_tampering_fails_closed() {
        let database = DeviceDatabase::open_in_memory(key(33)).expect("open");
        database
            .upsert_thread_binding("host", "provider-thread", "host-thread", 1)
            .expect("persist binding");
        database
            .lock()
            .unwrap()
            .execute("UPDATE thread_bindings SET encrypted_binding = X'00'", [])
            .unwrap();

        assert!(matches!(
            database.host_thread_id_for_provider("host", "provider-thread"),
            Err(DeviceDatabaseError::Authentication)
        ));
    }

    #[test]
    fn outbox_associated_data_rejects_host_rebinding() {
        let database = DeviceDatabase::open_in_memory(key(4)).expect("open");
        database
            .enqueue_outbox(&intent("intent", "host-a", b"secret"))
            .unwrap();
        database
            .lock()
            .unwrap()
            .execute("UPDATE outbox SET host_id = 'host-b'", [])
            .unwrap();

        assert!(matches!(
            database.due_outbox(10, 100),
            Err(DeviceDatabaseError::Authentication)
        ));
    }

    #[test]
    fn search_is_prefix_capable_and_bounded() {
        let database = DeviceDatabase::open_in_memory(key(5)).expect("open");
        for index in 0..250 {
            database
                .index_search_document(
                    &format!("document-{index:03}"),
                    "host",
                    &format!("thread-{index:03}"),
                    &format!("Rustacean command center result {index}"),
                    index,
                    false,
                )
                .unwrap();
        }

        let results = database.search("rusta comm", usize::MAX).expect("search");
        assert_eq!(results.len(), MAX_SEARCH_RESULTS);
        assert!(
            results
                .iter()
                .all(|result| result.snippet.len() <= MAX_SNIPPET_BYTES)
        );
        assert_eq!(
            database.search("rusta rusta", usize::MAX).unwrap().len(),
            MAX_SEARCH_RESULTS
        );
        assert!(matches!(
            database.search(&"q".repeat(MAX_SEARCH_QUERY_BYTES + 1), 50),
            Err(DeviceDatabaseError::InvalidInput(_))
        ));
    }

    #[test]
    fn search_batch_uses_explicit_terms_and_records_host_freshness() {
        let database = DeviceDatabase::open_in_memory(key(20)).expect("open");
        let body = r#"{"schema_version":1,"title":"Needle workflow"}"#;
        database
            .index_search_documents(
                &[SearchDocumentInput {
                    document_id: "document".to_string(),
                    host_id: "host".to_string(),
                    thread_id: "thread".to_string(),
                    body: body.to_string(),
                    search_text: "Needle workflow".to_string(),
                    updated_at_ms: 100,
                    protected: false,
                }],
                200,
            )
            .expect("batch index");

        let result = database.search("need", 50).expect("search");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].body, body);
        assert!(database.search("schema", 50).expect("search").is_empty());
        assert_eq!(
            database.search_host_states(100).expect("host states"),
            vec![SearchHostState {
                host_id: "host".to_string(),
                last_indexed_at_ms: 200,
            }]
        );
        assert_eq!(
            database
                .delete_search_documents(&["document".to_string()])
                .expect("delete"),
            1
        );
        assert!(database.search("need", 50).expect("search").is_empty());
    }

    #[test]
    fn search_ciphertext_tampering_fails_closed() {
        let database = DeviceDatabase::open_in_memory(key(6)).expect("open");
        database
            .index_search_document("document", "host", "thread", "needle", 1, false)
            .unwrap();
        database
            .lock()
            .unwrap()
            .execute(
                "UPDATE search_documents SET encrypted_body = X'00' WHERE document_id = 'document'",
                [],
            )
            .unwrap();

        assert!(matches!(
            database.search("needle", 50),
            Err(DeviceDatabaseError::Authentication)
        ));
    }

    #[test]
    fn retention_pruning_preserves_protected_documents() {
        let database = DeviceDatabase::open_in_memory(key(7)).expect("open");
        database
            .index_search_document("old", "host", "thread", "old", 1, false)
            .unwrap();
        database
            .index_search_document("pinned", "host", "thread", "pinned", 1, true)
            .unwrap();

        assert_eq!(database.prune_search_before(2, 100).unwrap(), 1);
        assert!(database.search("old", 50).unwrap().is_empty());
        assert_eq!(database.search("pinn", 50).unwrap().len(), 1);
    }

    #[test]
    fn organization_events_apply_only_in_monotonic_relay_order() {
        let database = DeviceDatabase::open_in_memory(key(8)).expect("open");
        assert!(
            database
                .apply_thread_organization(&organization(10, true))
                .unwrap()
        );
        assert!(
            !database
                .apply_thread_organization(&organization(9, false))
                .unwrap()
        );
        assert!(
            !database
                .apply_thread_organization(&organization(10, false))
                .unwrap()
        );
        assert!(
            database
                .apply_thread_organization(&organization(11, false))
                .unwrap()
        );

        let current = database
            .thread_organization("host", "thread")
            .unwrap()
            .expect("organization");
        assert_eq!(current.relay_sequence, 11);
        assert!(!current.pinned);
    }

    #[test]
    fn terminal_attention_is_event_driven_snoozable_and_acknowledgeable() {
        let database = DeviceDatabase::open_in_memory(key(18)).expect("open");
        let first = database
            .record_terminal_attention("host", "thread", "turn-1", 100, false)
            .expect("record completion");
        assert_eq!(first.occurred_at_ms, Some(100));
        assert!(!first.failed);
        assert_eq!(first.acknowledged_at_ms, None);

        let snoozed = database
            .snooze_thread_attention("host", "thread", 101, 1_001)
            .expect("snooze");
        assert_eq!(snoozed.snoozed_until_ms, Some(1_001));

        let failed = database
            .record_terminal_attention("host", "thread", "turn-2", 102, true)
            .expect("record newer failure");
        assert_eq!(failed.occurred_at_ms, Some(102));
        assert!(failed.failed);
        assert_eq!(failed.snoozed_until_ms, None);

        let acknowledged = database
            .acknowledge_thread_attention("host", "thread", 103)
            .expect("acknowledge");
        assert_eq!(acknowledged.acknowledged_at_ms, Some(103));

        let replay = database
            .record_terminal_attention("host", "thread", "turn-2", 104, true)
            .expect("replay the acknowledged event");
        assert_eq!(replay.acknowledged_at_ms, Some(103));

        let monotonic = database
            .record_terminal_attention("host", "thread", "turn-3", 102, false)
            .expect("record a new event with a repeated clock value");
        assert_eq!(monotonic.occurred_at_ms, Some(103));
        assert!(!monotonic.failed);
        assert_eq!(monotonic.acknowledged_at_ms, None);

        database
            .snooze_thread_attention("host", "thread", 104, 1_004)
            .expect("snooze before new work");
        database
            .delete_terminal_attention("host", "thread")
            .expect("clear terminal state for new work");
        let cleared = database
            .thread_attention_state("host", "thread")
            .expect("read cleared state")
            .expect("organization state remains");
        assert_eq!(cleared.terminal_event_id, None);
        assert_eq!(cleared.occurred_at_ms, None);
        assert_eq!(cleared.snoozed_until_ms, None);
    }

    #[test]
    fn attention_layout_migrates_without_invalidating_encrypted_records() {
        let directory = tempdir().expect("tempdir");
        let path = directory.path().join("device.sqlite");
        let database = DeviceDatabase::open(&path, key(19)).expect("open");
        database
            .enqueue_outbox(&intent("intent", "host", b"preserved"))
            .expect("enqueue");
        {
            let connection = database.lock().expect("lock");
            connection
                .execute("DROP TABLE thread_attention", [])
                .expect("drop additive table");
            connection
                .execute(
                    "UPDATE meta SET value = ?1 WHERE key = 'schema_version'",
                    [2_i64.to_be_bytes().as_slice()],
                )
                .expect("restore v2 marker");
        }
        drop(database);

        let migrated = DeviceDatabase::open(&path, key(19)).expect("migrate");
        assert_eq!(
            migrated.due_outbox(10, 1).expect("read preserved")[0].payload,
            b"preserved"
        );
        migrated
            .record_terminal_attention("host", "thread", "turn", 100, false)
            .expect("new table available");
    }

    #[test]
    fn search_freshness_layout_migrates_without_invalidating_encrypted_records() {
        let directory = tempdir().expect("tempdir");
        let path = directory.path().join("device.sqlite");
        let database = DeviceDatabase::open(&path, key(21)).expect("open");
        database
            .index_search_document("document", "host", "thread", "preserved", 1, false)
            .expect("index");
        {
            let connection = database.lock().expect("lock");
            connection
                .execute("DROP TABLE search_hosts", [])
                .expect("drop additive table");
            connection
                .execute(
                    "UPDATE meta SET value = ?1 WHERE key = 'schema_version'",
                    [3_i64.to_be_bytes().as_slice()],
                )
                .expect("restore v3 marker");
        }
        drop(database);

        let migrated = DeviceDatabase::open(&path, key(21)).expect("migrate");
        assert_eq!(
            migrated.search("pres", 1).expect("read preserved")[0].body,
            "preserved"
        );
        migrated
            .index_search_document("new", "host", "thread", "fresh", 2, false)
            .expect("new table available");
        assert_eq!(
            migrated.search_host_states(1).expect("freshness")[0].host_id,
            "host"
        );
    }

    #[test]
    fn thread_binding_layout_migrates_without_invalidating_encrypted_records() {
        let directory = tempdir().expect("tempdir");
        let path = directory.path().join("device.sqlite");
        let database = DeviceDatabase::open(&path, key(35)).expect("open");
        database
            .enqueue_outbox(&intent("intent", "host", b"preserved"))
            .expect("enqueue");
        {
            let connection = database.lock().expect("lock");
            connection
                .execute("DROP TABLE thread_bindings", [])
                .expect("drop additive table");
            connection
                .execute(
                    "UPDATE meta SET value = ?1 WHERE key = 'schema_version'",
                    [4_i64.to_be_bytes().as_slice()],
                )
                .expect("restore v4 marker");
        }
        drop(database);

        let migrated = DeviceDatabase::open(&path, key(35)).expect("migrate");
        assert_eq!(
            migrated.due_outbox(10, 1).expect("read preserved")[0].payload,
            b"preserved"
        );
        migrated
            .upsert_thread_binding("host", "provider-thread", "host-thread", 1)
            .expect("new table available");
    }

    #[test]
    fn review_notes_encrypt_round_trip_and_keep_anchors_immutable() {
        let database = DeviceDatabase::open_in_memory(key(9)).expect("open");
        let mut note = review_note("note", "thread");
        database.upsert_review_note(&note).unwrap();
        let stored = database
            .review_notes_for_thread("host", "thread", usize::MAX)
            .unwrap();
        assert_eq!(stored, vec![note.clone()]);

        note.body = "Updated review".to_string();
        note.updated_at_ms = 2;
        database.upsert_review_note(&note).unwrap();
        assert_eq!(
            database
                .review_notes_for_thread("host", "thread", 1)
                .unwrap()[0]
                .body,
            "Updated review"
        );

        let mut moved = note.clone();
        moved.path = "src/other.rs".to_string();
        moved.updated_at_ms = 3;
        assert!(matches!(
            database.upsert_review_note(&moved),
            Err(DeviceDatabaseError::InvalidInput(_))
        ));
        assert!(
            !database
                .set_review_note_state("note", ReviewNoteState::Resolved, 1)
                .unwrap()
        );
        assert!(
            database
                .set_review_note_state("note", ReviewNoteState::Resolved, 3)
                .unwrap()
        );
        assert_eq!(
            database
                .review_notes_for_thread("host", "thread", 1)
                .unwrap()[0]
                .state,
            ReviewNoteState::Resolved
        );
    }

    #[test]
    fn review_note_ciphertext_tampering_fails_closed() {
        let database = DeviceDatabase::open_in_memory(key(10)).expect("open");
        database
            .upsert_review_note(&review_note("note", "thread"))
            .unwrap();
        database
            .lock()
            .unwrap()
            .execute(
                "UPDATE review_notes SET encrypted_note = X'00' WHERE note_id = 'note'",
                [],
            )
            .unwrap();

        assert!(matches!(
            database.review_notes_for_thread("host", "thread", 100),
            Err(DeviceDatabaseError::Authentication)
        ));
    }

    #[test]
    fn retention_derives_protection_from_live_device_state() {
        let database = DeviceDatabase::open_in_memory(key(11)).expect("open");
        for (document_id, thread_id, body, protected) in [
            ("free", "free-thread", "free-token", false),
            ("pinned", "pinned-thread", "pinned-token", false),
            ("queued", "queued-thread", "queued-token", false),
            ("review", "review-thread", "review-token", false),
            ("explicit", "explicit-thread", "explicit-token", true),
        ] {
            database
                .index_search_document(document_id, "host", thread_id, body, 1, protected)
                .unwrap();
        }
        let mut pinned = organization(1, true);
        pinned.thread_id = "pinned-thread".to_string();
        database.apply_thread_organization(&pinned).unwrap();
        let mut queued = intent("intent", "host", b"queued");
        queued.thread_id = Some("queued-thread".to_string());
        database.enqueue_outbox(&queued).unwrap();
        database
            .upsert_review_note(&review_note("note", "review-thread"))
            .unwrap();

        assert_eq!(database.prune_search_before(2, 100).unwrap(), 1);
        assert!(database.search("free", 50).unwrap().is_empty());
        assert_eq!(database.search("pinned", 50).unwrap().len(), 1);
        assert_eq!(database.search("queued", 50).unwrap().len(), 1);
        assert_eq!(database.search("review", 50).unwrap().len(), 1);
        assert_eq!(database.search("explicit", 50).unwrap().len(), 1);

        let mut unpinned = pinned;
        unpinned.pinned = false;
        unpinned.relay_sequence = 2;
        database.apply_thread_organization(&unpinned).unwrap();
        database.acknowledge_outbox("intent").unwrap();
        database
            .set_review_note_state("note", ReviewNoteState::Resolved, 2)
            .unwrap();
        assert_eq!(database.prune_search_before(2, 100).unwrap(), 3);
        assert_eq!(database.search("explicit", 50).unwrap().len(), 1);
    }

    #[test]
    fn logical_size_retention_evicts_oldest_eligible_documents() {
        let database = DeviceDatabase::open_in_memory(key(12)).expect("open");
        database
            .index_search_document("old", "host", "old-thread", &"old ".repeat(200), 1, false)
            .unwrap();
        database
            .index_search_document("new", "host", "new-thread", &"new ".repeat(200), 2, false)
            .unwrap();

        assert_eq!(database.prune_search_to_bytes(1_200, 1).unwrap(), 1);
        assert!(database.search("old", 50).unwrap().is_empty());
        assert_eq!(database.search("new", 50).unwrap().len(), 1);
    }
}
