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
use rusqlite::{Connection, OptionalExtension, params};
use sha2::Sha256;
use zeroize::{Zeroize, Zeroizing};

const SCHEMA_VERSION: i64 = 1;
const KEY_BYTES: usize = 32;
const NONCE_BYTES: usize = 24;
const MAX_ID_BYTES: usize = 128;
const MAX_OUTBOX_PAYLOAD_BYTES: usize = 1024 * 1024;
const MAX_SEARCH_BODY_BYTES: usize = 1024 * 1024;
const MAX_OUTBOX_ROWS: usize = 100;
const MAX_SEARCH_RESULTS: usize = 50;
const MAX_SEARCH_CANDIDATES: usize = 200;
const MAX_QUERY_TOKENS: usize = 8;
const MAX_TOKEN_CHARS: usize = 64;
const MAX_PREFIX_CHARS: usize = 24;
const MAX_SNIPPET_BYTES: usize = 512;

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
}

impl OutboxState {
    fn as_i64(self) -> i64 {
        match self {
            Self::Queued => 0,
            Self::Delivering => 1,
        }
    }

    fn from_i64(value: i64) -> Result<Self, DeviceDatabaseError> {
        match value {
            0 => Ok(Self::Queued),
            1 => Ok(Self::Delivering),
            _ => Err(DeviceDatabaseError::Authentication),
        }
    }
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
    pub updated_at_ms: i64,
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
                 updated_at_ms INTEGER NOT NULL,
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
             CREATE TABLE IF NOT EXISTS review_notes (
                 note_id TEXT PRIMARY KEY,
                 host_id TEXT NOT NULL,
                 thread_id TEXT NOT NULL,
                 checkpoint_id TEXT NOT NULL,
                 path TEXT NOT NULL,
                 start_line INTEGER NOT NULL,
                 end_line INTEGER NOT NULL,
                 nonce BLOB NOT NULL,
                 encrypted_body BLOB NOT NULL,
                 state INTEGER NOT NULL,
                 created_at_ms INTEGER NOT NULL,
                 updated_at_ms INTEGER NOT NULL
             ) STRICT;
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
                 ON search_terms(document_id);",
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
             FROM outbox
             WHERE next_attempt_at_ms IS NULL OR next_attempt_at_ms <= ?1
             ORDER BY created_at_ms ASC, intent_id ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![now_ms, limit], |row| {
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
        })?;

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
             WHERE intent_id = ?1",
            params![intent_id, OutboxState::Queued.as_i64(), next_attempt_at_ms],
        )? == 1)
    }

    /// Delete only after an authoritative Host acknowledgement is reconciled.
    pub fn acknowledge_outbox(&self, intent_id: &str) -> Result<bool, DeviceDatabaseError> {
        validate_id("intent_id", intent_id)?;
        let connection = self.lock()?;
        Ok(connection.execute("DELETE FROM outbox WHERE intent_id = ?1", [intent_id])? == 1)
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
        validate_id("document_id", document_id)?;
        validate_id("host_id", host_id)?;
        validate_id("thread_id", thread_id)?;
        if body.len() > MAX_SEARCH_BODY_BYTES {
            return Err(DeviceDatabaseError::InvalidInput(
                "search document exceeds 1 MiB".to_string(),
            ));
        }
        let (nonce, ciphertext) =
            self.encrypt("search_document", document_id, host_id, body.as_bytes())?;
        let terms = posting_terms(body)
            .into_iter()
            .map(|term| self.term_hash(&term))
            .collect::<Result<BTreeSet<_>, _>>()?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        transaction.execute(
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
            params![
                document_id,
                host_id,
                thread_id,
                nonce.as_slice(),
                ciphertext,
                updated_at_ms,
                i64::from(protected),
            ],
        )?;
        transaction.execute(
            "DELETE FROM search_terms WHERE document_id = ?1",
            [document_id],
        )?;
        {
            let mut insert = transaction
                .prepare("INSERT INTO search_terms(term_hash, document_id) VALUES (?1, ?2)")?;
            for term_hash in terms {
                insert.execute(params![term_hash.as_slice(), document_id])?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn search(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchResult>, DeviceDatabaseError> {
        let query_tokens = normalized_tokens(query)
            .into_iter()
            .take(MAX_QUERY_TOKENS)
            .collect::<Vec<_>>();
        if query_tokens.is_empty() {
            return Ok(Vec::new());
        }
        let term_hashes = query_tokens
            .iter()
            .map(|token| self.term_hash(token))
            .collect::<Result<Vec<_>, _>>()?;
        let connection = self.lock()?;
        let mut candidates = BTreeSet::new();
        {
            let mut statement = connection.prepare(
                "SELECT document_id FROM search_terms
                 WHERE term_hash = ?1
                 ORDER BY document_id
                 LIMIT ?2",
            )?;
            let rows = statement.query_map(
                params![term_hashes[0].as_slice(), MAX_SEARCH_CANDIDATES as i64],
                |row| row.get::<_, String>(0),
            )?;
            for row in rows {
                candidates.insert(row?);
            }
        }
        for term_hash in term_hashes.iter().skip(1) {
            candidates.retain(|document_id| {
                connection
                    .query_row(
                        "SELECT 1 FROM search_terms
                         WHERE term_hash = ?1 AND document_id = ?2",
                        params![term_hash.as_slice(), document_id],
                        |_| Ok(()),
                    )
                    .optional()
                    .ok()
                    .flatten()
                    .is_some()
            });
        }

        let limit = limit.clamp(1, MAX_SEARCH_RESULTS);
        let mut results = Vec::new();
        for document_id in candidates.into_iter().take(MAX_SEARCH_CANDIDATES) {
            let record = connection
                .query_row(
                    "SELECT host_id, thread_id, nonce, encrypted_body, updated_at_ms
                     FROM search_documents WHERE document_id = ?1",
                    [&document_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, Vec<u8>>(2)?,
                            row.get::<_, Vec<u8>>(3)?,
                            row.get::<_, i64>(4)?,
                        ))
                    },
                )
                .optional()?;
            let Some((host_id, thread_id, nonce, ciphertext, updated_at_ms)) = record else {
                continue;
            };
            let body = self.decrypt(
                "search_document",
                &document_id,
                &host_id,
                &nonce,
                &ciphertext,
            )?;
            let body = String::from_utf8(body).map_err(|_| DeviceDatabaseError::Authentication)?;
            if query_tokens.iter().all(|token| {
                normalized_tokens(&body)
                    .iter()
                    .any(|candidate| candidate.starts_with(token))
            }) {
                results.push(SearchResult {
                    document_id,
                    host_id,
                    thread_id,
                    snippet: bound_utf8(body.trim(), MAX_SNIPPET_BYTES),
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
        let connection = self.lock()?;
        Ok(connection.execute("DELETE FROM search_documents WHERE host_id = ?1", [host_id])?)
    }

    pub fn prune_search_before(
        &self,
        before_ms: i64,
        limit: usize,
    ) -> Result<usize, DeviceDatabaseError> {
        let limit = limit.clamp(1, 1_000) as i64;
        let connection = self.lock()?;
        Ok(connection.execute(
            "DELETE FROM search_documents WHERE document_id IN (
                 SELECT document_id FROM search_documents
                 WHERE protected = 0 AND updated_at_ms < ?1
                 ORDER BY updated_at_ms ASC
                 LIMIT ?2
             )",
            params![before_ms, limit],
        )?)
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
            if existing != SCHEMA_VERSION.to_be_bytes() {
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
    format!("{SCHEMA_VERSION}|{record_type}|{primary_key}|{host_id}")
}

fn validate_id(name: &str, value: &str) -> Result<(), DeviceDatabaseError> {
    if value.is_empty() || value.len() > MAX_ID_BYTES || value.chars().any(char::is_control) {
        return Err(DeviceDatabaseError::InvalidInput(format!(
            "{name} must contain 1..={MAX_ID_BYTES} display-safe bytes"
        )));
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
}
