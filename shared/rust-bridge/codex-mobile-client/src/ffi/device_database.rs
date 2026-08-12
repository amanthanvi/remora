use std::path::Path;
use std::sync::Arc;

use crate::device_database::{
    DeviceDatabase, DeviceDatabaseError, OutboxIntent, OutboxIntentKind, OutboxState, ReviewNote,
    ReviewNoteState, SearchResult, ThreadOrganization,
};
use crate::ffi::ClientError;
use crate::ffi::background_relay::AppRelaySecretValue;

const MAX_DATABASE_PATH_BYTES: usize = 4_096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum AppOutboxIntentKind {
    SendMessage,
    CreateThread,
    SetOrganizationState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum AppOutboxState {
    Queued,
    Delivering,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AppOutboxIntent {
    pub intent_id: String,
    pub host_id: String,
    pub thread_id: Option<String>,
    pub kind: AppOutboxIntentKind,
    /// Plaintext intent while crossing the in-process native/Rust boundary.
    /// `DeviceDatabase` encrypts it before persistence and decrypts only the
    /// bounded rows selected for delivery.
    pub payload: Vec<u8>,
    pub state: AppOutboxState,
    pub created_at_ms: i64,
    pub attempt_count: u32,
    pub next_attempt_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AppSearchResult {
    pub document_id: String,
    pub host_id: String,
    pub thread_id: String,
    pub snippet: String,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AppThreadOrganization {
    pub host_id: String,
    pub thread_id: String,
    pub pinned: bool,
    pub hidden: bool,
    pub snoozed_until_ms: Option<i64>,
    pub acknowledged_at_ms: Option<i64>,
    pub relay_sequence: u64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum AppReviewNoteState {
    Open,
    Resolved,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AppReviewNote {
    pub note_id: String,
    pub host_id: String,
    pub thread_id: String,
    pub checkpoint_id: String,
    pub path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub body: String,
    pub state: AppReviewNoteState,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(uniffi::Object)]
pub struct DeviceDatabaseBridge {
    pub(crate) inner: Arc<DeviceDatabase>,
}

#[uniffi::export]
impl DeviceDatabaseBridge {
    /// Opens the device cache with a master key sourced from platform secure
    /// storage. The custom secret carrier is zeroized after Rust takes it.
    #[uniffi::constructor]
    pub fn open(path: String, master_key: AppRelaySecretValue) -> Result<Self, ClientError> {
        if path.is_empty() || path.len() > MAX_DATABASE_PATH_BYTES {
            return Err(ClientError::InvalidParams(
                "device database path must contain 1..=4096 bytes".to_string(),
            ));
        }
        let inner = Arc::new(
            DeviceDatabase::open(Path::new(&path), master_key.into_bytes())
                .map_err(map_database_error)?,
        );
        Ok(Self { inner })
    }

    pub fn enqueue_outbox(&self, intent: AppOutboxIntent) -> Result<bool, ClientError> {
        self.inner
            .enqueue_outbox(&intent.into())
            .map_err(map_database_error)
    }

    pub fn due_outbox(&self, now_ms: i64, limit: u32) -> Result<Vec<AppOutboxIntent>, ClientError> {
        self.inner
            .due_outbox(now_ms, limit as usize)
            .map(|intents| intents.into_iter().map(Into::into).collect())
            .map_err(map_database_error)
    }

    pub fn record_outbox_attempt(
        &self,
        intent_id: String,
        next_attempt_at_ms: i64,
    ) -> Result<bool, ClientError> {
        self.inner
            .record_outbox_attempt(&intent_id, next_attempt_at_ms)
            .map_err(map_database_error)
    }

    pub fn acknowledge_outbox(&self, intent_id: String) -> Result<bool, ClientError> {
        self.inner
            .acknowledge_outbox(&intent_id)
            .map_err(map_database_error)
    }

    pub fn apply_thread_organization(
        &self,
        organization: AppThreadOrganization,
    ) -> Result<bool, ClientError> {
        self.inner
            .apply_thread_organization(&organization.into())
            .map_err(map_database_error)
    }

    pub fn thread_organization(
        &self,
        host_id: String,
        thread_id: String,
    ) -> Result<Option<AppThreadOrganization>, ClientError> {
        self.inner
            .thread_organization(&host_id, &thread_id)
            .map(|organization| organization.map(Into::into))
            .map_err(map_database_error)
    }

    pub fn upsert_review_note(&self, note: AppReviewNote) -> Result<(), ClientError> {
        self.inner
            .upsert_review_note(&note.into())
            .map_err(map_database_error)
    }

    pub fn review_notes_for_thread(
        &self,
        host_id: String,
        thread_id: String,
        limit: u32,
    ) -> Result<Vec<AppReviewNote>, ClientError> {
        self.inner
            .review_notes_for_thread(&host_id, &thread_id, limit as usize)
            .map(|notes| notes.into_iter().map(Into::into).collect())
            .map_err(map_database_error)
    }

    pub fn set_review_note_state(
        &self,
        note_id: String,
        state: AppReviewNoteState,
        updated_at_ms: i64,
    ) -> Result<bool, ClientError> {
        self.inner
            .set_review_note_state(&note_id, state.into(), updated_at_ms)
            .map_err(map_database_error)
    }

    pub fn delete_review_note(&self, note_id: String) -> Result<bool, ClientError> {
        self.inner
            .delete_review_note(&note_id)
            .map_err(map_database_error)
    }

    pub fn index_search_document(
        &self,
        document_id: String,
        host_id: String,
        thread_id: String,
        body: String,
        updated_at_ms: i64,
        protected: bool,
    ) -> Result<(), ClientError> {
        self.inner
            .index_search_document(
                &document_id,
                &host_id,
                &thread_id,
                &body,
                updated_at_ms,
                protected,
            )
            .map_err(map_database_error)
    }

    pub fn search(&self, query: String, limit: u32) -> Result<Vec<AppSearchResult>, ClientError> {
        self.inner
            .search(&query, limit as usize)
            .map(|results| results.into_iter().map(Into::into).collect())
            .map_err(map_database_error)
    }

    pub fn clear_host_search_index(&self, host_id: String) -> Result<u32, ClientError> {
        let deleted = self
            .inner
            .clear_host_search_index(&host_id)
            .map_err(map_database_error)?;
        Ok(deleted.min(u32::MAX as usize) as u32)
    }

    pub fn prune_search_before(&self, before_ms: i64, limit: u32) -> Result<u32, ClientError> {
        let deleted = self
            .inner
            .prune_search_before(before_ms, limit as usize)
            .map_err(map_database_error)?;
        Ok(deleted.min(u32::MAX as usize) as u32)
    }

    pub fn enforce_search_retention(&self, now_ms: i64) -> Result<u32, ClientError> {
        let deleted = self
            .inner
            .enforce_search_retention(now_ms)
            .map_err(map_database_error)?;
        Ok(deleted.min(u32::MAX as usize) as u32)
    }
}

impl From<AppOutboxIntent> for OutboxIntent {
    fn from(value: AppOutboxIntent) -> Self {
        Self {
            intent_id: value.intent_id,
            host_id: value.host_id,
            thread_id: value.thread_id,
            kind: value.kind.into(),
            payload: value.payload,
            state: value.state.into(),
            created_at_ms: value.created_at_ms,
            attempt_count: value.attempt_count,
            next_attempt_at_ms: value.next_attempt_at_ms,
        }
    }
}

impl From<OutboxIntent> for AppOutboxIntent {
    fn from(value: OutboxIntent) -> Self {
        Self {
            intent_id: value.intent_id,
            host_id: value.host_id,
            thread_id: value.thread_id,
            kind: value.kind.into(),
            payload: value.payload,
            state: value.state.into(),
            created_at_ms: value.created_at_ms,
            attempt_count: value.attempt_count,
            next_attempt_at_ms: value.next_attempt_at_ms,
        }
    }
}

impl From<AppOutboxIntentKind> for OutboxIntentKind {
    fn from(value: AppOutboxIntentKind) -> Self {
        match value {
            AppOutboxIntentKind::SendMessage => Self::SendMessage,
            AppOutboxIntentKind::CreateThread => Self::CreateThread,
            AppOutboxIntentKind::SetOrganizationState => Self::SetOrganizationState,
        }
    }
}

impl From<OutboxIntentKind> for AppOutboxIntentKind {
    fn from(value: OutboxIntentKind) -> Self {
        match value {
            OutboxIntentKind::SendMessage => Self::SendMessage,
            OutboxIntentKind::CreateThread => Self::CreateThread,
            OutboxIntentKind::SetOrganizationState => Self::SetOrganizationState,
        }
    }
}

impl From<AppOutboxState> for OutboxState {
    fn from(value: AppOutboxState) -> Self {
        match value {
            AppOutboxState::Queued => Self::Queued,
            AppOutboxState::Delivering => Self::Delivering,
        }
    }
}

impl From<OutboxState> for AppOutboxState {
    fn from(value: OutboxState) -> Self {
        match value {
            OutboxState::Queued => Self::Queued,
            OutboxState::Delivering => Self::Delivering,
        }
    }
}

impl From<SearchResult> for AppSearchResult {
    fn from(value: SearchResult) -> Self {
        Self {
            document_id: value.document_id,
            host_id: value.host_id,
            thread_id: value.thread_id,
            snippet: value.snippet,
            updated_at_ms: value.updated_at_ms,
        }
    }
}

impl From<AppThreadOrganization> for ThreadOrganization {
    fn from(value: AppThreadOrganization) -> Self {
        Self {
            host_id: value.host_id,
            thread_id: value.thread_id,
            pinned: value.pinned,
            hidden: value.hidden,
            snoozed_until_ms: value.snoozed_until_ms,
            acknowledged_at_ms: value.acknowledged_at_ms,
            relay_sequence: value.relay_sequence,
            updated_at_ms: value.updated_at_ms,
        }
    }
}

impl From<ThreadOrganization> for AppThreadOrganization {
    fn from(value: ThreadOrganization) -> Self {
        Self {
            host_id: value.host_id,
            thread_id: value.thread_id,
            pinned: value.pinned,
            hidden: value.hidden,
            snoozed_until_ms: value.snoozed_until_ms,
            acknowledged_at_ms: value.acknowledged_at_ms,
            relay_sequence: value.relay_sequence,
            updated_at_ms: value.updated_at_ms,
        }
    }
}

impl From<AppReviewNoteState> for ReviewNoteState {
    fn from(value: AppReviewNoteState) -> Self {
        match value {
            AppReviewNoteState::Open => Self::Open,
            AppReviewNoteState::Resolved => Self::Resolved,
        }
    }
}

impl From<ReviewNoteState> for AppReviewNoteState {
    fn from(value: ReviewNoteState) -> Self {
        match value {
            ReviewNoteState::Open => Self::Open,
            ReviewNoteState::Resolved => Self::Resolved,
        }
    }
}

impl From<AppReviewNote> for ReviewNote {
    fn from(value: AppReviewNote) -> Self {
        Self {
            note_id: value.note_id,
            host_id: value.host_id,
            thread_id: value.thread_id,
            checkpoint_id: value.checkpoint_id,
            path: value.path,
            start_line: value.start_line,
            end_line: value.end_line,
            body: value.body,
            state: value.state.into(),
            created_at_ms: value.created_at_ms,
            updated_at_ms: value.updated_at_ms,
        }
    }
}

impl From<ReviewNote> for AppReviewNote {
    fn from(value: ReviewNote) -> Self {
        Self {
            note_id: value.note_id,
            host_id: value.host_id,
            thread_id: value.thread_id,
            checkpoint_id: value.checkpoint_id,
            path: value.path,
            start_line: value.start_line,
            end_line: value.end_line,
            body: value.body,
            state: value.state.into(),
            created_at_ms: value.created_at_ms,
            updated_at_ms: value.updated_at_ms,
        }
    }
}

fn map_database_error(error: DeviceDatabaseError) -> ClientError {
    match error {
        DeviceDatabaseError::InvalidKey | DeviceDatabaseError::InvalidInput(_) => {
            ClientError::InvalidParams(error.to_string())
        }
        DeviceDatabaseError::Authentication
        | DeviceDatabaseError::Database(_)
        | DeviceDatabaseError::Poisoned => ClientError::Serialization(error.to_string()),
    }
}
