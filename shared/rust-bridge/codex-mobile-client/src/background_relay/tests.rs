use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use tokio::sync::{Mutex, Notify};

use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Action {
    Register(String),
    TombstoneDevice(String, u64),
    Fetch(String, u64),
    Snapshot(String),
    Repair(String, RelayRepairMode),
    LocalCommit(String, u64),
    Ack(String, u64),
    TombstoneInstallation(String),
}

#[derive(Default)]
struct MemoryJournal {
    entries: Arc<Mutex<BTreeMap<String, RelayBindingEntry>>>,
    provider_tombstone_fences: Mutex<Vec<RelayProviderTombstoneFence>>,
    actions: Arc<Mutex<Vec<Action>>>,
    list_delay: Mutex<Option<Duration>>,
    load_delay: Mutex<Option<Duration>>,
    cas_failures_before_apply: Mutex<u32>,
    staged_cas_failures_before_apply: Mutex<u32>,
    staged_cas_failures_after_apply: Mutex<u32>,
    staged_cas_gate: Mutex<Option<JournalCasGate>>,
    token_revision_cas_gate: Mutex<Option<(String, JournalCasGate)>>,
}

#[derive(Clone, Default)]
struct JournalCasGate {
    started: Arc<Notify>,
    release: Arc<Notify>,
    completed: Arc<Notify>,
}

impl JournalCasGate {
    async fn wait_until_started(&self) {
        self.started.notified().await;
    }

    fn release(&self) {
        self.release.notify_one();
    }

    async fn wait_until_completed(&self) {
        self.completed.notified().await;
    }
}

impl MemoryJournal {
    fn with_actions(actions: Arc<Mutex<Vec<Action>>>) -> Self {
        Self {
            entries: Arc::new(Mutex::new(BTreeMap::new())),
            provider_tombstone_fences: Mutex::new(Vec::new()),
            actions,
            list_delay: Mutex::new(None),
            load_delay: Mutex::new(None),
            cas_failures_before_apply: Mutex::new(0),
            staged_cas_failures_before_apply: Mutex::new(0),
            staged_cas_failures_after_apply: Mutex::new(0),
            staged_cas_gate: Mutex::new(None),
            token_revision_cas_gate: Mutex::new(None),
        }
    }

    async fn entry(&self, host: &str) -> RelayBindingEntry {
        self.entries
            .lock()
            .await
            .get(host)
            .cloned()
            .expect("binding exists")
    }

    async fn fail_next_cas_before_apply(&self) {
        *self.cas_failures_before_apply.lock().await += 1;
    }

    async fn fail_next_staged_cas_before_apply(&self) {
        *self.staged_cas_failures_before_apply.lock().await += 1;
    }

    async fn fail_next_staged_cas_after_apply(&self) {
        *self.staged_cas_failures_after_apply.lock().await += 1;
    }

    async fn gate_next_staged_cas_after_drop(&self) -> JournalCasGate {
        let gate = JournalCasGate::default();
        *self.staged_cas_gate.lock().await = Some(gate.clone());
        gate
    }

    async fn gate_next_token_revision_cas(&self, host: &str) -> JournalCasGate {
        let gate = JournalCasGate::default();
        *self.token_revision_cas_gate.lock().await = Some((host.to_owned(), gate.clone()));
        gate
    }
}

fn advances_token_revision(current: &RelayBindingEntry, replacement: &RelayBindingEntry) -> bool {
    replacement.registrations.iter().any(|replacement| {
        let current_revision = current
            .registrations
            .iter()
            .find(|current| {
                current.provider == replacement.provider
                    && current.environment == replacement.environment
            })
            .and_then(|current| current.token_revision);
        match (current_revision, replacement.token_revision) {
            (None, Some(_)) => true,
            (Some(current), Some(replacement)) => replacement > current,
            _ => false,
        }
    })
}

#[async_trait]
impl RelayBindingJournalPort for MemoryJournal {
    async fn list(&self) -> Result<Vec<RelayBindingEntry>, RelayJournalError> {
        if let Some(delay) = *self.list_delay.lock().await {
            tokio::time::sleep(delay).await;
        }
        Ok(self.entries.lock().await.values().cloned().collect())
    }

    async fn load_by_host(
        &self,
        host_id: &RelayHostId,
    ) -> Result<Option<RelayBindingEntry>, RelayJournalError> {
        let delay = *self.load_delay.lock().await;
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        Ok(self.entries.lock().await.get(&host_id.0).cloned())
    }

    async fn load_by_installation(
        &self,
        installation_id: &RelayInstallationId,
    ) -> Result<Option<RelayBindingEntry>, RelayJournalError> {
        let delay = *self.load_delay.lock().await;
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        Ok(self
            .entries
            .lock()
            .await
            .values()
            .find(|entry| entry.installation_id == *installation_id)
            .cloned())
    }

    async fn provider_tombstone_fences(
        &self,
    ) -> Result<Vec<RelayProviderTombstoneFence>, RelayJournalError> {
        Ok(self.provider_tombstone_fences.lock().await.clone())
    }

    async fn advance_provider_tombstone_fence(
        &self,
        tombstone: &PushTokenTombstone,
    ) -> Result<(), RelayJournalError> {
        let mut fences = self.provider_tombstone_fences.lock().await;
        if let Some(fence) = fences.iter_mut().find(|fence| {
            fence.provider == tombstone.provider && fence.environment == tombstone.environment
        }) {
            fence.through_local_generation = fence
                .through_local_generation
                .max(tombstone.through_local_generation);
        } else {
            fences.push(RelayProviderTombstoneFence {
                provider: tombstone.provider,
                environment: tombstone.environment,
                through_local_generation: tombstone.through_local_generation,
            });
        }
        Ok(())
    }

    async fn compare_and_swap(
        &self,
        host_id: &RelayHostId,
        expected_revision: Option<u64>,
        replacement: RelayBindingEntry,
    ) -> Result<(), RelayJournalError> {
        if replacement.host_id != *host_id {
            return Err(RelayJournalError::Conflict);
        }
        let pauses_token_revision = self
            .entries
            .lock()
            .await
            .get(&host_id.0)
            .is_some_and(|current| advances_token_revision(current, &replacement));
        let token_revision_gate = if pauses_token_revision {
            let mut gate = self.token_revision_cas_gate.lock().await;
            if gate
                .as_ref()
                .is_some_and(|(gated_host, _)| gated_host == &host_id.0)
            {
                gate.take().map(|(_, gate)| gate)
            } else {
                None
            }
        } else {
            None
        };
        if let Some(gate) = token_revision_gate {
            gate.started.notify_one();
            gate.release.notified().await;
        }
        let replacement_is_staged = replacement.state == RelayBindingState::Staged;
        if replacement_is_staged && let Some(gate) = self.staged_cas_gate.lock().await.take() {
            let entries = Arc::clone(&self.entries);
            let host_id = host_id.clone();
            let (sender, receiver) = tokio::sync::oneshot::channel();
            tokio::spawn(async move {
                gate.started.notify_one();
                gate.release.notified().await;
                let mut entries = entries.lock().await;
                let current = entries.get(&host_id.0);
                if current.is_some_and(|entry| {
                    entry.wake.applied_cursor > entry.wake.highest_seen_cursor
                        || entry
                            .wake
                            .remote_ack_ahead_cursor
                            .is_some_and(|cursor| cursor <= entry.wake.applied_cursor)
                }) {
                    let _ = sender.send(Err(RelayJournalError::Unavailable));
                    gate.completed.notify_one();
                    return;
                }
                let revision_matches = match (expected_revision, current) {
                    (None, None) => true,
                    (Some(expected), Some(current)) => current.revision == expected,
                    _ => false,
                };
                if !revision_matches {
                    let _ = sender.send(Err(RelayJournalError::Conflict));
                    gate.completed.notify_one();
                    return;
                }
                entries.insert(host_id.0, replacement);
                let _ = sender.send(Ok(()));
                gate.completed.notify_one();
            });
            return receiver
                .await
                .unwrap_or(Err(RelayJournalError::Unavailable));
        }
        let mut entries = self.entries.lock().await;
        let current = entries.get(&host_id.0);
        // Match the native opaque-journal adapter's reload behavior. It
        // decodes the current authenticated blob before a subsequent CAS, so
        // an invalid intermediate ledger must wedge this in-memory test port
        // too instead of appearing recoverable only because it stays typed.
        if current.is_some_and(|entry| {
            entry.wake.applied_cursor > entry.wake.highest_seen_cursor
                || entry
                    .wake
                    .remote_ack_ahead_cursor
                    .is_some_and(|cursor| cursor <= entry.wake.applied_cursor)
        }) {
            return Err(RelayJournalError::Unavailable);
        }
        let revision_matches = match (expected_revision, current) {
            (None, None) => true,
            (Some(expected), Some(current)) => current.revision == expected,
            _ => false,
        };
        if !revision_matches {
            return Err(RelayJournalError::Conflict);
        }
        let mut failures = self.cas_failures_before_apply.lock().await;
        if *failures > 0 {
            *failures -= 1;
            return Err(RelayJournalError::Unavailable);
        }
        drop(failures);
        if replacement_is_staged {
            let mut failures = self.staged_cas_failures_before_apply.lock().await;
            if *failures > 0 {
                *failures -= 1;
                return Err(RelayJournalError::Unavailable);
            }
        }
        let prior_applied = current.map_or(0, |entry| entry.wake.applied_cursor);
        if replacement.wake.applied_cursor > prior_applied {
            self.actions.lock().await.push(Action::LocalCommit(
                host_id.0.clone(),
                replacement.wake.applied_cursor,
            ));
        }
        entries.insert(host_id.0.clone(), replacement);
        if replacement_is_staged {
            let mut failures = self.staged_cas_failures_after_apply.lock().await;
            if *failures > 0 {
                *failures -= 1;
                return Err(RelayJournalError::Unavailable);
            }
        }
        Ok(())
    }
}

#[derive(Default)]
struct MemorySecrets {
    values: Arc<Mutex<HashMap<String, MemorySecretRecord>>>,
    write_failures: Mutex<u32>,
    write_delay: Mutex<Option<Duration>>,
    tombstone_failures_before_apply: Mutex<HashMap<String, u32>>,
    tombstone_failures_after_apply: Mutex<u32>,
    cas_failures_before_apply: Mutex<u32>,
    cas_failures_after_apply: Mutex<u32>,
    cas_delay_once: Mutex<Option<Duration>>,
    cas_gate: Mutex<Option<SecretCasGate>>,
    read_gate: Mutex<Option<(String, u32, SecretCasGate)>>,
}

struct MemorySecretRecord {
    secret: Option<OpaqueRelaySecret>,
    revision: u64,
}

#[derive(Clone, Default)]
struct SecretCasGate {
    started: Arc<Notify>,
    release: Arc<Notify>,
    completed: Arc<Notify>,
}

impl SecretCasGate {
    async fn wait_until_started(&self) {
        self.started.notified().await;
    }

    fn release(&self) {
        self.release.notify_one();
    }

    async fn wait_until_completed(&self) {
        self.completed.notified().await;
    }
}

impl MemorySecrets {
    async fn contains(&self, alias: &RelaySecretAlias) -> bool {
        self.values
            .lock()
            .await
            .get(&alias.0)
            .is_some_and(|record| record.secret.is_some())
    }

    async fn count(&self) -> usize {
        self.values
            .lock()
            .await
            .values()
            .filter(|record| record.secret.is_some())
            .count()
    }

    async fn fail_next_tombstone_before_apply(&self, alias: &RelaySecretAlias) {
        self.tombstone_failures_before_apply
            .lock()
            .await
            .insert(alias.0.clone(), 1);
    }

    async fn fail_next_tombstone_after_apply(&self) {
        *self.tombstone_failures_after_apply.lock().await += 1;
    }

    async fn fail_next_cas_before_apply(&self) {
        *self.cas_failures_before_apply.lock().await += 1;
    }

    async fn fail_second_cas_after_apply(&self) {
        *self.cas_failures_after_apply.lock().await = 2;
    }

    async fn fail_next_cas_after_apply(&self) {
        *self.cas_failures_after_apply.lock().await = 1;
    }

    async fn delay_next_cas_after_drop(&self, delay: Duration) {
        *self.cas_delay_once.lock().await = Some(delay);
    }

    async fn gate_next_cas_after_drop(&self) -> SecretCasGate {
        let gate = SecretCasGate::default();
        *self.cas_gate.lock().await = Some(gate.clone());
        gate
    }

    async fn gate_read_after(
        &self,
        alias: &RelaySecretAlias,
        skipped_matching_reads: u32,
    ) -> SecretCasGate {
        let gate = SecretCasGate::default();
        *self.read_gate.lock().await =
            Some((alias.0.clone(), skipped_matching_reads, gate.clone()));
        gate
    }
}

#[async_trait]
impl OpaqueRelaySecretPort for MemorySecrets {
    async fn read(
        &self,
        alias: &RelaySecretAlias,
    ) -> Result<Option<OpaqueRelaySecret>, RelaySecretStoreError> {
        let gate = {
            let mut gate = self.read_gate.lock().await;
            match gate.as_mut() {
                Some((gated_alias, skipped_matching_reads, _))
                    if gated_alias == &alias.0 && *skipped_matching_reads > 0 =>
                {
                    *skipped_matching_reads -= 1;
                    None
                }
                Some((gated_alias, _, _)) if gated_alias == &alias.0 => {
                    gate.take().map(|(_, _, gate)| gate)
                }
                _ => None,
            }
        };
        if let Some(gate) = gate.as_ref() {
            gate.started.notify_one();
            gate.release.notified().await;
        }
        let secret = self
            .values
            .lock()
            .await
            .get(&alias.0)
            .and_then(|record| record.secret.clone());
        if let Some(gate) = gate {
            gate.completed.notify_one();
        }
        Ok(secret)
    }

    async fn create_if_absent(
        &self,
        alias: &RelaySecretAlias,
        secret: OpaqueRelaySecret,
    ) -> Result<RelaySecretCreateOutcome, RelaySecretStoreError> {
        let delay = *self.write_delay.lock().await;
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        let mut failures = self.write_failures.lock().await;
        if *failures > 0 {
            *failures -= 1;
            return Err(RelaySecretStoreError::Unavailable);
        }
        drop(failures);
        let mut values = self.values.lock().await;
        if values.contains_key(&alias.0) {
            return Ok(RelaySecretCreateOutcome::AlreadyExists);
        }
        values.insert(
            alias.0.clone(),
            MemorySecretRecord {
                secret: Some(secret),
                revision: 1,
            },
        );
        Ok(RelaySecretCreateOutcome::Created)
    }

    async fn revision(
        &self,
        alias: &RelaySecretAlias,
    ) -> Result<RelaySecretRevision, RelaySecretStoreError> {
        Ok(self
            .values
            .lock()
            .await
            .get(&alias.0)
            .map_or(RelaySecretRevision::Missing, |record| {
                RelaySecretRevision::Found(record.revision)
            }))
    }

    async fn compare_and_swap(
        &self,
        alias: &RelaySecretAlias,
        expected_revision: Option<u64>,
        replacement_revision: u64,
        secret: OpaqueRelaySecret,
    ) -> Result<RelaySecretCasOutcome, RelaySecretStoreError> {
        let mut failures = self.cas_failures_before_apply.lock().await;
        if *failures > 0 {
            *failures -= 1;
            return Err(RelaySecretStoreError::Unavailable);
        }
        drop(failures);
        if let Some(gate) = self.cas_gate.lock().await.take() {
            let values = Arc::clone(&self.values);
            let alias = alias.0.clone();
            let (sender, receiver) = tokio::sync::oneshot::channel();
            tokio::spawn(async move {
                gate.started.notify_one();
                gate.release.notified().await;
                let mut values = values.lock().await;
                let current_revision = values.get(&alias).map(|record| record.revision);
                let outcome = if current_revision == expected_revision {
                    values.insert(
                        alias,
                        MemorySecretRecord {
                            secret: Some(secret),
                            revision: replacement_revision,
                        },
                    );
                    RelaySecretCasOutcome::Stored
                } else {
                    RelaySecretCasOutcome::Conflict
                };
                let _ = sender.send(outcome);
                gate.completed.notify_one();
            });
            return receiver
                .await
                .map_err(|_| RelaySecretStoreError::Unavailable);
        }
        if let Some(delay) = self.cas_delay_once.lock().await.take() {
            let values = Arc::clone(&self.values);
            let alias = alias.0.clone();
            let (sender, receiver) = tokio::sync::oneshot::channel();
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                let mut values = values.lock().await;
                let current_revision = values.get(&alias).map(|record| record.revision);
                let outcome = if current_revision == expected_revision {
                    values.insert(
                        alias,
                        MemorySecretRecord {
                            secret: Some(secret),
                            revision: replacement_revision,
                        },
                    );
                    RelaySecretCasOutcome::Stored
                } else {
                    RelaySecretCasOutcome::Conflict
                };
                let _ = sender.send(outcome);
            });
            return receiver
                .await
                .map_err(|_| RelaySecretStoreError::Unavailable);
        }
        let mut values = self.values.lock().await;
        let current_revision = values.get(&alias.0).map(|record| record.revision);
        if current_revision != expected_revision {
            return Ok(RelaySecretCasOutcome::Conflict);
        }
        values.insert(
            alias.0.clone(),
            MemorySecretRecord {
                secret: Some(secret),
                revision: replacement_revision,
            },
        );
        drop(values);
        let mut failures = self.cas_failures_after_apply.lock().await;
        if *failures > 0 {
            *failures -= 1;
            if *failures == 0 {
                return Err(RelaySecretStoreError::Unavailable);
            }
        }
        Ok(RelaySecretCasOutcome::Stored)
    }

    async fn compare_and_tombstone(
        &self,
        alias: &RelaySecretAlias,
        expected_revision: Option<u64>,
        replacement_revision: u64,
    ) -> Result<RelaySecretCasOutcome, RelaySecretStoreError> {
        let mut failures = self.tombstone_failures_before_apply.lock().await;
        if let Some(remaining) = failures.get_mut(&alias.0)
            && *remaining > 0
        {
            *remaining -= 1;
            return Err(RelaySecretStoreError::Unavailable);
        }
        drop(failures);
        let mut values = self.values.lock().await;
        let current_revision = values.get(&alias.0).map(|record| record.revision);
        if current_revision != expected_revision {
            return Ok(RelaySecretCasOutcome::Conflict);
        }
        values.insert(
            alias.0.clone(),
            MemorySecretRecord {
                secret: None,
                revision: replacement_revision,
            },
        );
        drop(values);
        let mut failures = self.tombstone_failures_after_apply.lock().await;
        if *failures > 0 {
            *failures -= 1;
            return Err(RelaySecretStoreError::Unavailable);
        }
        Ok(RelaySecretCasOutcome::Stored)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CapturedContext {
    origin: String,
    authorization_bytes: usize,
    follow_redirects: bool,
    max_response_bytes: usize,
}

type PageResultQueue =
    HashMap<(String, u64), VecDeque<Result<RelayEventPage, RelayTransportError>>>;
type SnapshotResultQueue =
    HashMap<String, VecDeque<Result<RelaySnapshotEnvelope, RelayTransportError>>>;
type AckResultQueue = HashMap<String, VecDeque<Result<RelayAckReceipt, RelayTransportError>>>;

#[derive(Default)]
struct FakeTransport {
    actions: Arc<Mutex<Vec<Action>>>,
    contexts: Mutex<Vec<CapturedContext>>,
    register_errors: Mutex<HashMap<String, VecDeque<RelayTransportError>>>,
    register_counts: Mutex<HashMap<String, u64>>,
    pages: Mutex<PageResultQueue>,
    snapshots: Mutex<SnapshotResultQueue>,
    ack_results: Mutex<AckResultQueue>,
    tombstone_device_results: Mutex<HashMap<String, VecDeque<Result<(), RelayTransportError>>>>,
    tombstone_installation_results:
        Mutex<HashMap<String, VecDeque<Result<(), RelayTransportError>>>>,
    fetch_delay: Mutex<Option<Duration>>,
    fetch_delays: Mutex<HashMap<String, Duration>>,
}

impl FakeTransport {
    fn with_actions(actions: Arc<Mutex<Vec<Action>>>) -> Self {
        Self {
            actions,
            ..Self::default()
        }
    }

    async fn capture(&self, context: &RelayTransportContext) {
        self.contexts.lock().await.push(CapturedContext {
            origin: context.origin.as_url().to_string(),
            authorization_bytes: context.authorization.expose_for_adapter().len(),
            follow_redirects: context.operation.follow_redirects,
            max_response_bytes: context.operation.max_response_bytes,
        });
    }

    async fn enqueue_register_error(&self, installation: &str, error: RelayTransportError) {
        self.register_errors
            .lock()
            .await
            .entry(installation.to_owned())
            .or_default()
            .push_back(error);
    }

    async fn enqueue_page(
        &self,
        installation: &str,
        after: u64,
        page: Result<RelayEventPage, RelayTransportError>,
    ) {
        self.pages
            .lock()
            .await
            .entry((installation.to_owned(), after))
            .or_default()
            .push_back(page);
    }

    async fn enqueue_snapshot(
        &self,
        installation: &str,
        snapshot: Result<RelaySnapshotEnvelope, RelayTransportError>,
    ) {
        self.snapshots
            .lock()
            .await
            .entry(installation.to_owned())
            .or_default()
            .push_back(snapshot);
    }

    async fn set_fetch_delay(&self, installation: &str, delay: Duration) {
        self.fetch_delays
            .lock()
            .await
            .insert(installation.to_owned(), delay);
    }

    async fn enqueue_ack_error(&self, installation: &str, error: RelayTransportError) {
        self.ack_results
            .lock()
            .await
            .entry(installation.to_owned())
            .or_default()
            .push_back(Err(error));
    }

    async fn enqueue_ack_receipt(&self, installation: &str, acknowledged_through: u64) {
        self.ack_results
            .lock()
            .await
            .entry(installation.to_owned())
            .or_default()
            .push_back(Ok(RelayAckReceipt {
                schema_version: RELAY_SCHEMA_VERSION,
                installation_id: RelayInstallationId::parse(installation).expect("installation id"),
                acknowledged_through,
                replayed: acknowledged_through > 0,
            }));
    }

    async fn enqueue_installation_tombstone_result(
        &self,
        installation: &str,
        result: Result<(), RelayTransportError>,
    ) {
        self.tombstone_installation_results
            .lock()
            .await
            .entry(installation.to_owned())
            .or_default()
            .push_back(result);
    }

    async fn enqueue_device_tombstone_result(
        &self,
        installation: &str,
        result: Result<(), RelayTransportError>,
    ) {
        self.tombstone_device_results
            .lock()
            .await
            .entry(installation.to_owned())
            .or_default()
            .push_back(result);
    }

    async fn register_count(&self, installation: &str) -> u64 {
        self.register_counts
            .lock()
            .await
            .get(installation)
            .copied()
            .unwrap_or_default()
    }
}

#[async_trait]
impl RelayTransportPort for FakeTransport {
    async fn register_device(
        &self,
        context: RelayTransportContext,
        request: RelayRegisterDeviceRequest,
    ) -> Result<RelayDeviceRegistrationReceipt, RelayTransportError> {
        self.capture(&context).await;
        let installation = request.installation_id.0.clone();
        self.actions
            .lock()
            .await
            .push(Action::Register(installation.clone()));
        let mut counts = self.register_counts.lock().await;
        let count = counts.entry(installation.clone()).or_default();
        *count += 1;
        let registration_generation = *count;
        drop(counts);
        if let Some(error) = self
            .register_errors
            .lock()
            .await
            .get_mut(&installation)
            .and_then(VecDeque::pop_front)
        {
            return Err(error);
        }
        assert!(request.token.expose_for_adapter().len() >= 32);
        Ok(RelayDeviceRegistrationReceipt {
            schema_version: RELAY_SCHEMA_VERSION,
            installation_id: request.installation_id,
            registration_id: RelayRegistrationId::parse(format!(
                "registration_{installation}_{registration_generation}"
            ))
            .expect("test registration id"),
            provider: request.provider,
            environment: request.environment,
            generation: registration_generation,
            replaced: false,
        })
    }

    async fn tombstone_device(
        &self,
        context: RelayTransportContext,
        request: RelayTombstoneDeviceRequest,
    ) -> Result<(), RelayTransportError> {
        self.capture(&context).await;
        self.actions.lock().await.push(Action::TombstoneDevice(
            request.installation_id.0.clone(),
            request.through_generation,
        ));
        self.tombstone_device_results
            .lock()
            .await
            .get_mut(&request.installation_id.0)
            .and_then(VecDeque::pop_front)
            .unwrap_or(Ok(()))
    }

    async fn fetch_events(
        &self,
        context: RelayTransportContext,
        request: RelayFetchEventsRequest,
    ) -> Result<RelayEventPage, RelayTransportError> {
        self.capture(&context).await;
        let installation = request.installation_id.0.clone();
        self.actions
            .lock()
            .await
            .push(Action::Fetch(installation.clone(), request.after));
        assert!(request.limit > 0);
        let host_delay = self.fetch_delays.lock().await.get(&installation).copied();
        let delay = match host_delay {
            Some(delay) => Some(delay),
            None => *self.fetch_delay.lock().await,
        };
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        self.pages
            .lock()
            .await
            .get_mut(&(installation, request.after))
            .and_then(VecDeque::pop_front)
            .unwrap_or_else(|| {
                Ok(RelayEventPage {
                    schema_version: RELAY_SCHEMA_VERSION,
                    requested_after: request.after,
                    next_cursor: request.after,
                    high_watermark: request.after,
                    replay_floor: 1,
                    reset_required: false,
                    snapshot_available: false,
                    events: Vec::new(),
                    encoded_bytes: 128,
                })
            })
    }

    async fn fetch_snapshot(
        &self,
        context: RelayTransportContext,
        request: RelayFetchSnapshotRequest,
    ) -> Result<RelaySnapshotEnvelope, RelayTransportError> {
        self.capture(&context).await;
        let installation = request.installation_id.0.clone();
        self.actions
            .lock()
            .await
            .push(Action::Snapshot(installation.clone()));
        self.snapshots
            .lock()
            .await
            .get_mut(&installation)
            .and_then(VecDeque::pop_front)
            .unwrap_or(Err(RelayTransportError::NotFound))
    }

    async fn acknowledge(
        &self,
        context: RelayTransportContext,
        request: RelayAckRequest,
    ) -> Result<RelayAckReceipt, RelayTransportError> {
        self.capture(&context).await;
        let installation = request.installation_id.0.clone();
        self.actions
            .lock()
            .await
            .push(Action::Ack(installation.clone(), request.through_cursor));
        if let Some(result) = self
            .ack_results
            .lock()
            .await
            .get_mut(&installation)
            .and_then(VecDeque::pop_front)
        {
            return result;
        }
        Ok(RelayAckReceipt {
            schema_version: RELAY_SCHEMA_VERSION,
            installation_id: request.installation_id,
            acknowledged_through: request.through_cursor,
            replayed: false,
        })
    }

    async fn tombstone_installation(
        &self,
        context: RelayTransportContext,
        request: RelayTombstoneInstallationRequest,
    ) -> Result<(), RelayTransportError> {
        self.capture(&context).await;
        let installation = request.installation_id.0;
        self.actions
            .lock()
            .await
            .push(Action::TombstoneInstallation(installation.clone()));
        self.tombstone_installation_results
            .lock()
            .await
            .get_mut(&installation)
            .and_then(VecDeque::pop_front)
            .unwrap_or(Ok(()))
    }
}

#[derive(Default)]
struct FakeRepair {
    actions: Arc<Mutex<Vec<Action>>>,
    failure: Mutex<Option<RelayRepairError>>,
    applied_cursor_override: Mutex<Option<u64>>,
}

impl FakeRepair {
    fn with_actions(actions: Arc<Mutex<Vec<Action>>>) -> Self {
        Self {
            actions,
            ..Self::default()
        }
    }
}

#[async_trait]
impl RelayAuthoritativeRepairPort for FakeRepair {
    async fn repair(
        &self,
        host_id: &RelayHostId,
        generation: u64,
        mode: RelayRepairMode,
        operation: RelayOperationContext,
    ) -> Result<RelayRepairReceipt, RelayRepairError> {
        if generation == 0 || operation.cancellation.is_cancelled() {
            return Err(RelayRepairError::Cancelled);
        }
        operation
            .remaining()
            .map_err(|_| RelayRepairError::DeadlineExceeded)?;
        self.actions
            .lock()
            .await
            .push(Action::Repair(host_id.0.clone(), mode.clone()));
        if let Some(error) = *self.failure.lock().await {
            return Err(error);
        }
        let expected_cursor = match mode {
            RelayRepairMode::Incremental { through_cursor, .. }
            | RelayRepairMode::Snapshot { through_cursor, .. }
            | RelayRepairMode::Full { through_cursor } => through_cursor,
        };
        let applied_through_cursor = self
            .applied_cursor_override
            .lock()
            .await
            .unwrap_or(expected_cursor);
        Ok(RelayRepairReceipt {
            applied_through_cursor,
            authoritative: true,
        })
    }
}

struct TestWorld {
    actions: Arc<Mutex<Vec<Action>>>,
    journal: Arc<MemoryJournal>,
    secrets: Arc<MemorySecrets>,
    transport: Arc<FakeTransport>,
    repair: Arc<FakeRepair>,
}

impl TestWorld {
    fn new() -> Self {
        let actions = Arc::new(Mutex::new(Vec::new()));
        Self {
            journal: Arc::new(MemoryJournal::with_actions(actions.clone())),
            secrets: Arc::new(MemorySecrets::default()),
            transport: Arc::new(FakeTransport::with_actions(actions.clone())),
            repair: Arc::new(FakeRepair::with_actions(actions.clone())),
            actions,
        }
    }

    fn relay(&self) -> BackgroundRelay {
        BackgroundRelay::new(
            self.journal.clone(),
            self.secrets.clone(),
            self.transport.clone(),
            self.repair.clone(),
        )
    }

    async fn enroll(&self, host: &str, installation: &str) {
        let relay = self.relay();
        let enrollment = enrollment(host, installation);
        let command_id = enrollment.command_id.clone();
        relay
            .stage_enrollment(enrollment, operation())
            .await
            .expect("stage enrollment");
        relay
            .commit_enrollment(&RelayHostId(host.to_owned()), &command_id, operation())
            .await
            .expect("commit enrollment");
    }
}

fn secret(byte: u8) -> OpaqueRelaySecret {
    OpaqueRelaySecret::new(vec![byte; 32]).expect("test secret")
}

fn enrollment(host: &str, installation: &str) -> RelayEnrollment {
    enrollment_with_secrets(host, installation, 0x41, 0x42)
}

fn command_id(installation: &str) -> RelayEnrollmentCommandId {
    RelayEnrollmentCommandId::parse(format!("command_{installation}")).expect("command id")
}

fn enrollment_with_secrets(
    host: &str,
    installation: &str,
    read_byte: u8,
    manage_byte: u8,
) -> RelayEnrollment {
    RelayEnrollment {
        host_id: RelayHostId(host.to_owned()),
        origin: ValidatedRelayOrigin::parse(&format!("https://{host}.relay.example/"), false)
            .expect("secure origin"),
        installation_id: RelayInstallationId::parse(installation).expect("installation id"),
        command_id: command_id(installation),
        read_capability: secret(read_byte),
        manage_capability: secret(manage_byte),
    }
}

fn observation(generation: u64) -> PushTokenObservation {
    observation_with_byte(generation, 0x77)
}

fn observation_with_byte(generation: u64, byte: u8) -> PushTokenObservation {
    PushTokenObservation {
        provider: RelayPushProvider::Apns,
        environment: RelayPushEnvironment::Sandbox,
        token: secret(byte),
        local_generation: generation,
        observed_at_ms: 100,
    }
}

fn operation() -> RelayOperationContext {
    RelayOperationContext::with_timeout(Duration::from_secs(5))
}

fn event(cursor: u64) -> RelayEventEnvelope {
    RelayEventEnvelope {
        event_id: RelayEventId::parse(format!("event_identifier_{cursor:04}")).expect("event id"),
        cursor,
        event_class: RelayEventClass::StateChanged,
        expires_at_ms: 10_000,
    }
}

fn page(
    requested_after: u64,
    high_watermark: u64,
    events: Vec<RelayEventEnvelope>,
) -> RelayEventPage {
    RelayEventPage {
        schema_version: RELAY_SCHEMA_VERSION,
        requested_after,
        next_cursor: events.last().map_or(requested_after, |event| event.cursor),
        high_watermark,
        replay_floor: 1,
        reset_required: false,
        snapshot_available: false,
        events,
        encoded_bytes: 512,
    }
}

fn reset_page(
    requested_after: u64,
    high_watermark: u64,
    snapshot_available: bool,
) -> RelayEventPage {
    RelayEventPage {
        schema_version: RELAY_SCHEMA_VERSION,
        requested_after,
        next_cursor: requested_after,
        high_watermark,
        replay_floor: requested_after.saturating_add(2),
        reset_required: true,
        snapshot_available,
        events: Vec::new(),
        encoded_bytes: 256,
    }
}

fn wake(installation: &str, cursor: u64) -> OpaqueWakeHint {
    OpaqueWakeHint {
        schema_version: RELAY_SCHEMA_VERSION,
        installation_id: RelayInstallationId::parse(installation).expect("installation id"),
        event_id: RelayEventId::parse(format!("wake_identifier_{cursor:04}")).expect("wake id"),
        cursor,
        event_class: RelayEventClass::StateChanged,
        expires_at_ms: 10_000,
    }
}

#[tokio::test]
async fn token_observation_fans_out_independently_to_every_active_host() {
    let world = TestWorld::new();
    let install_a = "installation_host_a_0001";
    let install_b = "installation_host_b_0001";
    world.enroll("host-a", install_a).await;
    world.enroll("host-b", install_b).await;

    let receipt = world
        .relay()
        .observe_push_token(observation(1), operation())
        .await
        .expect("fanout succeeds");

    assert_eq!(receipt.attempted, 2);
    assert_eq!(receipt.synchronized, 2);
    assert_eq!(receipt.pending_retry, 0);
    let a = world.journal.entry("host-a").await;
    let b = world.journal.entry("host-b").await;
    assert_eq!(a.registrations.len(), 1);
    assert_eq!(b.registrations.len(), 1);
    assert!(!a.registrations[0].pending_sync);
    assert!(!b.registrations[0].pending_sync);
    assert_ne!(
        a.registrations[0].token_alias,
        b.registrations[0].token_alias
    );
    assert!(
        world
            .secrets
            .contains(&a.registrations[0].token_alias)
            .await
    );
    assert!(
        world
            .secrets
            .contains(&b.registrations[0].token_alias)
            .await
    );

    let contexts = world.transport.contexts.lock().await.clone();
    assert_eq!(contexts.len(), 2);
    assert!(contexts.iter().all(|context| {
        context.origin.starts_with("https://")
            && context.authorization_bytes == 32
            && !context.follow_redirects
            && context.max_response_bytes == DEFAULT_MAX_RESPONSE_BYTES
    }));
}

#[tokio::test]
async fn partial_token_fanout_is_durable_and_retries_only_the_pending_host() {
    let world = TestWorld::new();
    let install_a = "installation_host_a_0002";
    let install_b = "installation_host_b_0002";
    world.enroll("host-a", install_a).await;
    world.enroll("host-b", install_b).await;
    world
        .transport
        .enqueue_register_error(install_b, RelayTransportError::Network)
        .await;

    let first = world
        .relay()
        .observe_push_token(observation(1), operation())
        .await
        .expect("partial fanout returns aggregate receipt");
    assert_eq!((first.synchronized, first.pending_retry), (1, 1));
    assert!(world.journal.entry("host-b").await.registrations[0].pending_sync);

    let second = world
        .relay()
        .observe_push_token(observation(1), operation())
        .await
        .expect("retry succeeds");
    assert_eq!((second.synchronized, second.pending_retry), (2, 0));
    assert_eq!(world.transport.register_count(install_a).await, 1);
    assert_eq!(world.transport.register_count(install_b).await, 2);
    assert!(!world.journal.entry("host-b").await.registrations[0].pending_sync);
}

#[tokio::test]
async fn terminal_token_tombstone_defeats_a_late_native_custody_cas() {
    let world = TestWorld::new();
    let host = RelayHostId("late-token-terminal-host".to_owned());
    let installation = "installation_late_token_terminal_0001";
    world.enroll(&host.0, installation).await;
    let gate = world.secrets.gate_next_cas_after_drop().await;
    let relay = world.relay();
    let observe = tokio::spawn(async move {
        relay
            .observe_push_token(
                observation(1),
                RelayOperationContext::with_timeout(Duration::from_millis(50)),
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), gate.wait_until_started())
        .await
        .expect("provider token enters native custody CAS");
    let receipt = tokio::time::timeout(Duration::from_secs(1), observe)
        .await
        .expect("observation respects its deadline")
        .expect("observation task joins")
        .expect("fanout classifies the timed-out host");
    assert_eq!((receipt.synchronized, receipt.pending_retry), (0, 1));

    let pending = world.journal.entry(&host.0).await;
    assert_eq!(pending.state, RelayBindingState::Active);
    assert!(pending.registrations[0].pending_sync);
    assert_eq!(pending.registrations[0].token_revision, None);
    let token_alias = pending.registrations[0].token_alias.clone();
    world
        .relay()
        .rollback_enrollment(&host, &command_id(installation), operation())
        .await
        .expect("terminal cleanup revision-tombstones the pending token alias");

    gate.release();
    tokio::time::timeout(Duration::from_secs(1), gate.wait_until_completed())
        .await
        .expect("late provider token CAS completes with a revision conflict");
    let terminal = world.journal.entry(&host.0).await;
    assert_eq!(terminal.state, RelayBindingState::Tombstoned);
    assert!(terminal.registrations.is_empty());
    assert_eq!(
        world.secrets.revision(&token_alias).await.unwrap(),
        RelaySecretRevision::Found(1)
    );
    assert!(world.secrets.read(&token_alias).await.unwrap().is_none());
    assert_eq!(world.transport.register_count(installation).await, 0);
}

#[tokio::test]
async fn ambiguous_token_custody_retries_with_a_higher_journaled_revision_after_restart() {
    let world = TestWorld::new();
    let host = "ambiguous-token-custody-host";
    let installation = "installation_ambiguous_token_custody_0001";
    world.enroll(host, installation).await;
    world.secrets.fail_next_cas_after_apply().await;

    let interrupted = world
        .relay()
        .observe_push_token(observation(1), operation())
        .await
        .expect("fanout classifies ambiguous native custody");
    assert_eq!(
        (interrupted.synchronized, interrupted.pending_retry),
        (0, 1)
    );
    let pending = world.journal.entry(host).await;
    assert!(pending.registrations[0].pending_sync);
    assert_eq!(pending.registrations[0].token_revision, None);
    let token_alias = pending.registrations[0].token_alias.clone();
    assert_eq!(
        world.secrets.revision(&token_alias).await.unwrap(),
        RelaySecretRevision::Found(1)
    );

    let recovery = world
        .relay()
        .reconcile_all(100, operation())
        .await
        .expect("fresh coordinator loads the pending registration");
    assert!(matches!(
        recovery.as_slice(),
        [RelayReconcileOutcome::Failed {
            host_id,
            error: RelayError::SecureStorageUnavailable,
        }] if host_id.0 == host
    ));
    assert_eq!(world.transport.register_count(installation).await, 0);

    let retried = world
        .relay()
        .observe_push_token(observation(1), operation())
        .await
        .expect("re-observation advances ambiguous custody via CAS");
    assert_eq!((retried.synchronized, retried.pending_retry), (1, 0));
    let active = world.journal.entry(host).await;
    assert!(!active.registrations[0].pending_sync);
    assert_eq!(active.registrations[0].token_alias, token_alias);
    assert_eq!(active.registrations[0].token_revision, Some(2));
    assert_eq!(
        world.secrets.revision(&token_alias).await.unwrap(),
        RelaySecretRevision::Found(2)
    );
    assert_eq!(world.transport.register_count(installation).await, 1);
}

#[tokio::test]
async fn exact_token_read_rejects_an_interleaved_cas_and_competing_journal_cas() {
    let world = TestWorld::new();
    let host = "exact-token-read-host";
    let host_id = RelayHostId(host.to_owned());
    let installation = "installation_exact_token_read_0001";
    world.enroll(host, installation).await;
    world
        .transport
        .enqueue_register_error(installation, RelayTransportError::Network)
        .await;
    let initial = world
        .relay()
        .observe_push_token(observation(1), operation())
        .await
        .expect("initial observation remains pending after the remote error");
    assert_eq!((initial.synchronized, initial.pending_retry), (0, 1));
    let pending = world.journal.entry(host).await;
    let token_alias = pending.registrations[0].token_alias.clone();
    assert_eq!(pending.registrations[0].token_revision, Some(1));
    assert_eq!(world.transport.register_count(installation).await, 1);

    let read_gate = world.secrets.gate_read_after(&token_alias, 1).await;
    let first_relay = world.relay();
    let first = tokio::spawn(async move {
        first_relay
            .observe_push_token(observation_with_byte(1, 0x88), operation())
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), read_gate.wait_until_started())
        .await
        .expect("first coordinator pauses between its revision reads");
    let first_journaled = world.journal.entry(host).await;
    assert_eq!(first_journaled.registrations[0].token_revision, Some(2));

    let journal_gate = world.journal.gate_next_token_revision_cas(host).await;
    let second_relay = world.relay();
    let second = tokio::spawn(async move {
        second_relay
            .observe_push_token(observation_with_byte(1, 0x99), operation())
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), journal_gate.wait_until_started())
        .await
        .expect("second coordinator advances custody before journaling its revision");
    assert_eq!(
        world.secrets.revision(&token_alias).await.unwrap(),
        RelaySecretRevision::Found(3)
    );
    assert_eq!(
        world.journal.entry(host).await.registrations[0].token_revision,
        Some(2)
    );

    // Win a separate journal CAS while the second coordinator is paused. Its
    // revision-3 custody publication must then conflict, leaving a replayable
    // pending row rather than silently blessing the unjournaled secret.
    let current = world.journal.entry(host).await;
    let mut competing = current.clone();
    competing.revision += 1;
    world
        .journal
        .compare_and_swap(&host_id, Some(current.revision), competing)
        .await
        .expect("competing journal CAS wins");

    read_gate.release();
    let first_receipt = tokio::time::timeout(Duration::from_secs(1), first)
        .await
        .expect("first coordinator observes the revision mismatch")
        .expect("first coordinator joins")
        .expect("fanout classifies the exact-read failure");
    assert_eq!(
        (first_receipt.synchronized, first_receipt.pending_retry),
        (0, 1)
    );
    assert_eq!(world.transport.register_count(installation).await, 1);

    journal_gate.release();
    let second_receipt = tokio::time::timeout(Duration::from_secs(1), second)
        .await
        .expect("second coordinator observes the competing journal CAS")
        .expect("second coordinator joins")
        .expect("fanout classifies the journal conflict");
    assert_eq!(
        (second_receipt.synchronized, second_receipt.pending_retry),
        (0, 1)
    );
    assert_eq!(world.transport.register_count(installation).await, 1);
    let still_pending = world.journal.entry(host).await;
    assert!(still_pending.registrations[0].pending_sync);
    assert_eq!(still_pending.registrations[0].token_revision, Some(2));

    let converged = world
        .relay()
        .observe_push_token(observation_with_byte(1, 0xaa), operation())
        .await
        .expect("fresh retry advances, journals, and registers exact custody");
    assert_eq!((converged.synchronized, converged.pending_retry), (1, 0));
    let active = world.journal.entry(host).await;
    assert!(!active.registrations[0].pending_sync);
    assert_eq!(active.registrations[0].token_revision, Some(4));
    assert_eq!(
        world.secrets.revision(&token_alias).await.unwrap(),
        RelaySecretRevision::Found(4)
    );
    assert_eq!(world.transport.register_count(installation).await, 2);
}

#[tokio::test]
async fn stale_binding_cannot_send_reenrolled_capability_to_its_old_origin() {
    let world = TestWorld::new();
    let host = "stale-capability-host";
    let host_id = RelayHostId(host.to_owned());
    let old_installation = "installation_stale_capability_old_0001";
    let new_installation = "installation_stale_capability_new_0001";
    world.enroll(host, old_installation).await;
    let old_binding = world.journal.entry(host).await;
    assert_eq!(old_binding.read_capability_revision, Some(1));

    let read_gate = world
        .secrets
        .gate_read_after(&old_binding.read_capability_alias, 0)
        .await;
    let stale_relay = world.relay();
    let stale_reconcile =
        tokio::spawn(async move { stale_relay.reconcile_all(100, operation()).await });
    tokio::time::timeout(Duration::from_secs(1), read_gate.wait_until_started())
        .await
        .expect("stale coordinator passes its first revision check");

    world
        .relay()
        .rollback_enrollment(&host_id, &command_id(old_installation), operation())
        .await
        .expect("old enrollment is durably revoked and cleaned up");
    let mut replacement = enrollment_with_secrets(host, new_installation, 0xa1, 0xa2);
    replacement.origin = ValidatedRelayOrigin::parse("https://replacement.relay.example/", false)
        .expect("replacement origin");
    let replacement_command = replacement.command_id.clone();
    world
        .relay()
        .stage_enrollment(replacement, operation())
        .await
        .expect("replacement reuses the fenced capability slots");
    world
        .relay()
        .commit_enrollment(&host_id, &replacement_command, operation())
        .await
        .expect("replacement enrollment activates");

    let active = world.journal.entry(host).await;
    assert_eq!(active.state, RelayBindingState::Active);
    assert_eq!(active.installation_id.0, new_installation);
    assert_eq!(
        active.read_capability_alias,
        old_binding.read_capability_alias
    );
    assert_eq!(
        active.manage_capability_alias,
        old_binding.manage_capability_alias
    );
    assert_eq!(active.read_capability_revision, Some(3));
    assert_eq!(active.manage_capability_revision, Some(3));
    let contexts_before_stale_resume = world.transport.contexts.lock().await.len();
    let actions_before_stale_resume = world.actions.lock().await.clone();

    read_gate.release();
    let stale_outcomes = tokio::time::timeout(Duration::from_secs(1), stale_reconcile)
        .await
        .expect("stale coordinator fails closed after its second revision read")
        .expect("stale coordinator joins")
        .expect("stale reconciliation returns typed outcomes");
    assert!(matches!(
        stale_outcomes.as_slice(),
        [RelayReconcileOutcome::Failed {
            host_id,
            error: RelayError::SecureStorageUnavailable,
        }] if host_id.0 == host
    ));
    assert_eq!(
        world.transport.contexts.lock().await.len(),
        contexts_before_stale_resume,
        "the replacement capability must never be sent to the old origin"
    );
    assert_eq!(*world.actions.lock().await, actions_before_stale_resume);
    assert_eq!(world.journal.entry(host).await, active);
    assert_eq!(
        world
            .secrets
            .read(&active.read_capability_alias)
            .await
            .unwrap()
            .expect("replacement read capability remains present")
            .expose_for_adapter(),
        &[0xa1; 32]
    );
}

#[tokio::test]
async fn token_rotation_reserves_aliases_before_writes_and_retries_retired_cleanup() {
    let world = TestWorld::new();
    let host = "token-rotation-host";
    let installation = "installation_token_rotation_0001";
    world.enroll(host, installation).await;
    world
        .relay()
        .observe_push_token(observation(1), operation())
        .await
        .expect("initial token");
    let initial = world.journal.entry(host).await;
    let old_alias = initial.registrations[0].token_alias.clone();
    let secret_count = world.secrets.count().await;

    world.journal.fail_next_cas_before_apply().await;
    let failed_reservation = world
        .relay()
        .observe_push_token(observation(2), operation())
        .await
        .expect("fanout classifies journal failure");
    assert_eq!(failed_reservation.pending_retry, 1);
    assert_eq!(world.secrets.count().await, secret_count);
    let unchanged = world.journal.entry(host).await;
    assert_eq!(unchanged.registrations[0].local_generation, 1);
    assert!(unchanged.retired_token_aliases.is_empty());

    world.secrets.fail_next_cas_before_apply().await;
    let failed_write = world
        .relay()
        .observe_push_token(observation(2), operation())
        .await
        .expect("durable reservation survives a secret failure");
    assert_eq!(failed_write.pending_retry, 1);
    let staged = world.journal.entry(host).await;
    assert_eq!(staged.registrations[0].local_generation, 2);
    assert!(staged.registrations[0].pending_sync);
    assert!(staged.retired_token_aliases.is_empty());
    let previous = staged.registrations[0]
        .previous
        .as_ref()
        .expect("the known prior receipt remains durable");
    assert_eq!(previous.token_alias, old_alias);
    assert!(previous.relay_registration_id.is_some());
    assert_eq!(previous.relay_generation, Some(1));
    assert!(
        !world
            .secrets
            .contains(&staged.registrations[0].token_alias)
            .await
    );
    assert!(world.secrets.contains(&old_alias).await);

    world
        .secrets
        .fail_next_tombstone_before_apply(&old_alias)
        .await;
    let failed_cleanup = world
        .relay()
        .observe_push_token(observation(2), operation())
        .await
        .expect("remote sync succeeds while cleanup remains durable");
    assert_eq!(failed_cleanup.pending_retry, 1);
    let cleanup_pending = world.journal.entry(host).await;
    assert!(!cleanup_pending.registrations[0].pending_sync);
    assert!(cleanup_pending.retired_token_aliases.is_empty());
    assert_eq!(
        cleanup_pending.registrations[0]
            .previous
            .as_ref()
            .map(|previous| &previous.token_alias),
        Some(&old_alias)
    );
    assert!(world.secrets.contains(&old_alias).await);

    world
        .relay()
        .reconcile_all(100, operation())
        .await
        .expect("foreground retry cleans the retired alias");
    let recovered = world.journal.entry(host).await;
    assert!(recovered.retired_token_aliases.is_empty());
    assert!(recovered.registrations[0].previous.is_none());
    assert!(!world.secrets.contains(&old_alias).await);
    assert!(
        world
            .secrets
            .contains(&recovered.registrations[0].token_alias)
            .await
    );
}

#[tokio::test]
async fn foreground_reconcile_resumes_a_durable_pending_token_rotation() {
    let world = TestWorld::new();
    let host = "pending-token-host";
    let installation = "installation_pending_token_0001";
    world.enroll(host, installation).await;
    world
        .relay()
        .observe_push_token(observation(1), operation())
        .await
        .expect("initial token");
    let old_alias = world.journal.entry(host).await.registrations[0]
        .token_alias
        .clone();
    world
        .transport
        .enqueue_register_error(installation, RelayTransportError::Timeout)
        .await;

    let interrupted = world
        .relay()
        .observe_push_token(observation(2), operation())
        .await
        .expect("ambiguous response remains durable");
    assert_eq!(interrupted.pending_retry, 1);
    let pending = world.journal.entry(host).await;
    assert!(pending.registrations[0].pending_sync);
    assert!(pending.retired_token_aliases.is_empty());
    assert_eq!(
        pending.registrations[0]
            .previous
            .as_ref()
            .map(|previous| &previous.token_alias),
        Some(&old_alias)
    );

    world
        .relay()
        .reconcile_all(100, operation())
        .await
        .expect("foreground resumes the provider mutation");
    let active = world.journal.entry(host).await;
    assert!(!active.registrations[0].pending_sync);
    assert!(active.retired_token_aliases.is_empty());
    assert!(active.registrations[0].previous.is_none());
    assert!(!world.secrets.contains(&old_alias).await);
}

#[tokio::test]
async fn repeated_failed_token_rotations_keep_retired_aliases_bounded() {
    let world = TestWorld::new();
    let host = "bounded-token-rotation-host";
    let installation = "installation_bounded_token_rotation_0001";
    world.enroll(host, installation).await;
    world
        .relay()
        .observe_push_token(observation(1), operation())
        .await
        .expect("initial token");

    for generation in 2..=66 {
        world
            .transport
            .enqueue_register_error(installation, RelayTransportError::Timeout)
            .await;
        let receipt = world
            .relay()
            .observe_push_token(observation(generation), operation())
            .await
            .expect("failed rotation remains a bounded durable retry");
        assert_eq!(receipt.pending_retry, 1);
        let reloaded = world.journal.entry(host).await;
        assert!(reloaded.registrations[0].pending_sync);
        assert!(reloaded.retired_token_aliases.is_empty());
        assert!(reloaded.registrations[0].previous.is_some());
        assert!(reloaded.retired_token_aliases.len() <= MAX_RETIRED_TOKEN_ALIASES);
        assert!(world.secrets.count().await <= 4);
    }
}

#[tokio::test]
async fn pending_provider_terminal_errors_become_durable_binding_states() {
    for (suffix, transport_error, relay_error, expected_state) in [
        (
            "unauthorized",
            RelayTransportError::Unauthorized,
            RelayError::RePairRequired,
            RelayBindingState::NeedsRepair,
        ),
        (
            "gone",
            RelayTransportError::Gone,
            RelayError::Tombstoned,
            RelayBindingState::Tombstoned,
        ),
    ] {
        let world = TestWorld::new();
        let host = format!("pending-terminal-{suffix}-host");
        let installation = format!("installation_pending_terminal_{suffix}_0001");
        world.enroll(&host, &installation).await;
        world
            .relay()
            .observe_push_token(observation(1), operation())
            .await
            .expect("initial token");
        world
            .transport
            .enqueue_register_error(&installation, RelayTransportError::Timeout)
            .await;
        world
            .relay()
            .observe_push_token(observation(2), operation())
            .await
            .expect("create durable pending registration");
        world
            .transport
            .enqueue_register_error(&installation, transport_error)
            .await;

        let outcomes = world
            .relay()
            .reconcile_all(100, operation())
            .await
            .expect("batch returns typed terminal outcome");
        assert!(matches!(
            outcomes.as_slice(),
            [RelayReconcileOutcome::Failed {
                host_id,
                error,
            }] if host_id.0 == host && *error == relay_error
        ));
        assert_eq!(world.journal.entry(&host).await.state, expected_state);
    }
}

#[tokio::test]
async fn terminal_token_authorization_failure_is_durable_and_not_retried_on_launch() {
    let world = TestWorld::new();
    let installation = "installation_token_auth_0001";
    world.enroll("token-auth-host", installation).await;
    world
        .transport
        .enqueue_register_error(installation, RelayTransportError::Unauthorized)
        .await;

    let receipt = world
        .relay()
        .observe_push_token(observation(1), operation())
        .await
        .expect("fanout returns classified receipt");
    assert_eq!(receipt.re_pair_required, 1);
    assert_eq!(
        world.journal.entry("token-auth-host").await.state,
        RelayBindingState::NeedsRepair
    );
    let second = world
        .relay()
        .observe_push_token(observation(1), operation())
        .await
        .expect("inactive repair binding is skipped");
    assert_eq!(second.attempted, 0);
    assert_eq!(world.transport.register_count(installation).await, 1);
}

#[tokio::test]
async fn provider_tombstone_fans_out_and_removes_only_local_token_secrets_after_remote_success() {
    let world = TestWorld::new();
    let install_a = "installation_tombstone_a_0001";
    let install_b = "installation_tombstone_b_0001";
    world.enroll("tombstone-a", install_a).await;
    world.enroll("tombstone-b", install_b).await;
    world
        .relay()
        .observe_push_token(observation(1), operation())
        .await
        .expect("initial fanout");
    let alias_a = world.journal.entry("tombstone-a").await.registrations[0]
        .token_alias
        .clone();
    let alias_b = world.journal.entry("tombstone-b").await.registrations[0]
        .token_alias
        .clone();

    let receipt = world
        .relay()
        .tombstone_push_token(
            PushTokenTombstone {
                provider: RelayPushProvider::Apns,
                environment: RelayPushEnvironment::Sandbox,
                through_local_generation: 1,
            },
            operation(),
        )
        .await
        .expect("tombstone fanout");

    assert_eq!((receipt.attempted, receipt.synchronized), (2, 2));
    assert!(
        world
            .journal
            .entry("tombstone-a")
            .await
            .registrations
            .is_empty()
    );
    assert!(
        world
            .journal
            .entry("tombstone-b")
            .await
            .registrations
            .is_empty()
    );
    assert!(!world.secrets.contains(&alias_a).await);
    assert!(!world.secrets.contains(&alias_b).await);
}

#[tokio::test]
async fn provider_tombstone_without_registration_fences_stale_observations_across_restarts() {
    let world = TestWorld::new();
    let host = "empty-tombstone-host";
    let installation = "installation_empty_tombstone_0001";
    world.enroll(host, installation).await;

    let tombstoned = world
        .relay()
        .tombstone_push_token(
            PushTokenTombstone {
                provider: RelayPushProvider::Apns,
                environment: RelayPushEnvironment::Sandbox,
                through_local_generation: 3,
            },
            operation(),
        )
        .await
        .expect("empty binding still persists a provider fence");
    assert_eq!((tombstoned.attempted, tombstoned.synchronized), (1, 1));
    let fenced = world.journal.entry(host).await;
    assert!(fenced.registrations.is_empty());
    assert_eq!(
        fenced.provider_tombstone_fences,
        vec![RelayProviderTombstoneFence {
            provider: RelayPushProvider::Apns,
            environment: RelayPushEnvironment::Sandbox,
            through_local_generation: 3,
        }]
    );

    // Each `relay()` call constructs a fresh coordinator over the same durable
    // ports, modeling another client process or a restart.
    assert_eq!(
        world
            .relay()
            .observe_push_token(observation(3), operation())
            .await,
        Err(RelayError::InvalidProviderRegistration)
    );
    assert!(world.journal.entry(host).await.registrations.is_empty());

    let newer = world
        .relay()
        .observe_push_token(observation(4), operation())
        .await
        .expect("a newer generation can establish a replacement");
    assert_eq!((newer.synchronized, newer.rejected), (1, 0));
    assert_eq!(
        world.journal.entry(host).await.registrations[0].local_generation,
        4
    );
}

#[tokio::test]
async fn device_global_provider_floor_survives_zero_bindings_and_seeds_later_enrollment() {
    let world = TestWorld::new();
    let tombstone = PushTokenTombstone {
        provider: RelayPushProvider::Apns,
        environment: RelayPushEnvironment::Sandbox,
        through_local_generation: 7,
    };
    let receipt = world
        .relay()
        .tombstone_push_token(tombstone, operation())
        .await
        .expect("a logout floor persists without any host binding");
    assert_eq!((receipt.attempted, receipt.synchronized), (0, 0));
    assert_eq!(
        world.journal.provider_tombstone_fences().await.unwrap(),
        vec![RelayProviderTombstoneFence {
            provider: RelayPushProvider::Apns,
            environment: RelayPushEnvironment::Sandbox,
            through_local_generation: 7,
        }]
    );

    let host = "post-logout-enrollment-host";
    let installation = "installation_post_logout_0001";
    world.enroll(host, installation).await;
    assert_eq!(
        world.journal.entry(host).await.provider_tombstone_fences,
        world.journal.provider_tombstone_fences().await.unwrap()
    );
    assert_eq!(
        world
            .relay()
            .observe_push_token(observation(7), operation())
            .await,
        Err(RelayError::InvalidProviderRegistration)
    );
    let newer = world
        .relay()
        .observe_push_token(observation(8), operation())
        .await
        .expect("only a post-logout generation registers");
    assert_eq!(newer.synchronized, 1);
}

#[tokio::test]
async fn device_global_provider_floor_blocks_a_binding_staged_before_logout() {
    let world = TestWorld::new();
    let host = RelayHostId("staged-during-logout-host".to_owned());
    let installation = "installation_staged_during_logout_0001";
    let enrollment = enrollment(&host.0, installation);
    let command = enrollment.command_id.clone();
    world
        .relay()
        .stage_enrollment(enrollment, operation())
        .await
        .expect("stage before logout");
    let receipt = world
        .relay()
        .tombstone_push_token(
            PushTokenTombstone {
                provider: RelayPushProvider::Apns,
                environment: RelayPushEnvironment::Sandbox,
                through_local_generation: 9,
            },
            operation(),
        )
        .await
        .expect("global floor advances while only Staged exists");
    assert_eq!(receipt.attempted, 0);
    world
        .relay()
        .commit_enrollment(&host, &command, operation())
        .await
        .expect("staged enrollment remains valid");
    assert_eq!(
        world
            .relay()
            .observe_push_token(observation(9), operation())
            .await,
        Err(RelayError::InvalidProviderRegistration)
    );
    assert!(world.journal.entry(&host.0).await.registrations.is_empty());
}

#[tokio::test]
async fn needs_repair_replacement_inherits_the_device_global_provider_floor() {
    let world = TestWorld::new();
    let host = RelayHostId("repair-replacement-floor-host".to_owned());
    let old_installation = "installation_repair_floor_old_0001";
    let new_installation = "installation_repair_floor_new_0001";
    world.enroll(&host.0, old_installation).await;
    world
        .transport
        .enqueue_page(old_installation, 0, Err(RelayTransportError::Unauthorized))
        .await;
    assert_eq!(
        world
            .relay()
            .ingest_wake(wake(old_installation, 1), 100, operation())
            .await,
        Err(RelayError::RePairRequired)
    );
    assert_eq!(
        world.journal.entry(&host.0).await.state,
        RelayBindingState::NeedsRepair
    );
    world
        .relay()
        .tombstone_push_token(
            PushTokenTombstone {
                provider: RelayPushProvider::Apns,
                environment: RelayPushEnvironment::Sandbox,
                through_local_generation: 11,
            },
            operation(),
        )
        .await
        .expect("inactive repair row cannot discard the global logout floor");
    world
        .transport
        .enqueue_installation_tombstone_result(
            old_installation,
            Err(RelayTransportError::Unauthorized),
        )
        .await;
    let replacement = enrollment(&host.0, new_installation);
    let command = replacement.command_id.clone();
    world
        .relay()
        .stage_enrollment(replacement, operation())
        .await
        .expect("authenticated replacement inherits the global floor");
    assert_eq!(
        world.journal.entry(&host.0).await.provider_tombstone_fences,
        world.journal.provider_tombstone_fences().await.unwrap()
    );
    world
        .relay()
        .commit_enrollment(&host, &command, operation())
        .await
        .expect("replacement activates across a fresh coordinator");
    assert_eq!(
        world
            .relay()
            .observe_push_token(observation(11), operation())
            .await,
        Err(RelayError::InvalidProviderRegistration)
    );
}

#[tokio::test]
async fn pending_provider_tombstone_blocks_all_generations_until_remote_cleanup_converges() {
    let world = TestWorld::new();
    let host = "pending-provider-tombstone-host";
    let installation = "installation_pending_provider_tombstone_0001";
    world.enroll(host, installation).await;
    world
        .transport
        .enqueue_register_error(installation, RelayTransportError::Timeout)
        .await;
    let pending_registration = world
        .relay()
        .observe_push_token(observation(5), operation())
        .await
        .expect("ambiguous registration stays pending");
    assert_eq!(pending_registration.pending_retry, 1);
    assert!(
        world.journal.entry(host).await.registrations[0]
            .relay_registration_id
            .is_none()
    );
    world
        .transport
        .enqueue_device_tombstone_result(installation, Err(RelayTransportError::Timeout))
        .await;

    let tombstone = PushTokenTombstone {
        provider: RelayPushProvider::Apns,
        environment: RelayPushEnvironment::Sandbox,
        through_local_generation: 5,
    };
    let interrupted = world
        .relay()
        .tombstone_push_token(tombstone, operation())
        .await
        .expect("tombstone intent remains durable after response loss");
    assert_eq!(interrupted.pending_retry, 1);
    let pending = world.journal.entry(host).await;
    assert_eq!(
        pending.provider_tombstone_fences[0].through_local_generation,
        5
    );
    assert_eq!(
        pending.registrations[0].disposition,
        RelayRegistrationDisposition::Tombstone
    );
    assert!(pending.registrations[0].relay_registration_id.is_some());
    assert_eq!(world.transport.register_count(installation).await, 2);

    assert_eq!(
        world
            .relay()
            .observe_push_token(observation(5), operation())
            .await,
        Err(RelayError::InvalidProviderRegistration)
    );
    let premature_newer = world
        .relay()
        .observe_push_token(observation(6), operation())
        .await
        .expect("newer generation waits for pending cleanup");
    assert_eq!(premature_newer.pending_retry, 1);
    assert_eq!(world.transport.register_count(installation).await, 2);

    world
        .relay()
        .reconcile_all(100, operation())
        .await
        .expect("restart convergence resumes the durable tombstone");
    assert!(world.journal.entry(host).await.registrations.is_empty());

    let converged_newer = world
        .relay()
        .observe_push_token(observation(6), operation())
        .await
        .expect("newer generation registers after cleanup");
    assert_eq!(converged_newer.synchronized, 1);
    assert_eq!(
        world.journal.entry(host).await.registrations[0].local_generation,
        6
    );
}

#[tokio::test]
async fn tombstoning_a_pending_rotation_recovers_the_latest_relay_generation_before_revocation() {
    let world = TestWorld::new();
    let host = "pending-rotation-tombstone-host";
    let installation = "installation_pending_rotation_tombstone_0001";
    world.enroll(host, installation).await;
    world
        .relay()
        .observe_push_token(observation(4), operation())
        .await
        .expect("initial registration");
    world
        .transport
        .enqueue_register_error(installation, RelayTransportError::Timeout)
        .await;
    world
        .relay()
        .observe_push_token(observation(5), operation())
        .await
        .expect("ambiguous rotation remains pending");
    let pending = world.journal.entry(host).await;
    assert!(pending.registrations[0].pending_sync);
    assert!(pending.registrations[0].relay_registration_id.is_none());
    assert_eq!(
        pending.registrations[0]
            .previous
            .as_ref()
            .and_then(|previous| previous.relay_generation),
        Some(1)
    );

    let tombstoned = world
        .relay()
        .tombstone_push_token(
            PushTokenTombstone {
                provider: RelayPushProvider::Apns,
                environment: RelayPushEnvironment::Sandbox,
                through_local_generation: 5,
            },
            operation(),
        )
        .await
        .expect("pending rotation converges before revocation");
    assert_eq!(tombstoned.synchronized, 1);
    assert!(world.journal.entry(host).await.registrations.is_empty());
    // Initial registration, ambiguous rotation, and the receipt-recovery
    // replay required before the relay-generation tombstone.
    assert_eq!(world.transport.register_count(installation).await, 3);
}

#[tokio::test]
async fn failed_rotation_write_tombstones_the_known_prior_receipt_before_local_cleanup() {
    let world = TestWorld::new();
    let host = "failed-write-logout-host";
    let installation = "installation_failed_write_logout_0001";
    world.enroll(host, installation).await;
    world
        .relay()
        .observe_push_token(observation(1), operation())
        .await
        .expect("initial remote receipt");
    let initial = world.journal.entry(host).await;
    let prior_alias = initial.registrations[0].token_alias.clone();
    world.secrets.fail_next_cas_before_apply().await;
    let failed = world
        .relay()
        .observe_push_token(observation(2), operation())
        .await
        .expect("the failed write leaves a durable pending rotation");
    assert_eq!(failed.pending_retry, 1);
    let pending = world.journal.entry(host).await;
    let current_alias = pending.registrations[0].token_alias.clone();
    let previous = pending.registrations[0]
        .previous
        .as_ref()
        .expect("known prior receipt is retained separately");
    assert_eq!(previous.token_alias, prior_alias);
    assert_eq!(previous.relay_generation, Some(1));
    assert!(!world.secrets.contains(&current_alias).await);
    assert!(world.secrets.contains(&prior_alias).await);

    let receipt = world
        .relay()
        .tombstone_push_token(
            PushTokenTombstone {
                provider: RelayPushProvider::Apns,
                environment: RelayPushEnvironment::Sandbox,
                through_local_generation: 2,
            },
            operation(),
        )
        .await
        .expect("logout revokes the known receipt without inventing a current one");
    assert_eq!((receipt.synchronized, receipt.pending_retry), (1, 0));
    assert!(world.journal.entry(host).await.registrations.is_empty());
    assert!(!world.secrets.contains(&prior_alias).await);
    assert!(!world.secrets.contains(&current_alias).await);
    assert_eq!(world.transport.register_count(installation).await, 1);
    let tombstones = world
        .actions
        .lock()
        .await
        .iter()
        .filter_map(|action| match action {
            Action::TombstoneDevice(candidate, generation) if candidate == installation => {
                Some(*generation)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(tombstones, vec![1]);
    assert_eq!(
        world
            .relay()
            .observe_push_token(observation(2), operation())
            .await,
        Err(RelayError::InvalidProviderRegistration)
    );
}

#[tokio::test]
async fn provider_tombstone_response_loss_then_gone_resumes_durable_local_cleanup() {
    let world = TestWorld::new();
    let installation = "installation_device_gone_0001";
    world.enroll("device-gone-host", installation).await;
    world
        .relay()
        .observe_push_token(observation(1), operation())
        .await
        .expect("initial registration");
    let token_alias = world.journal.entry("device-gone-host").await.registrations[0]
        .token_alias
        .clone();
    world
        .transport
        .enqueue_device_tombstone_result(installation, Err(RelayTransportError::Timeout))
        .await;
    world
        .transport
        .enqueue_device_tombstone_result(installation, Err(RelayTransportError::Gone))
        .await;
    let tombstone = PushTokenTombstone {
        provider: RelayPushProvider::Apns,
        environment: RelayPushEnvironment::Sandbox,
        through_local_generation: 1,
    };

    let first = world
        .relay()
        .tombstone_push_token(tombstone.clone(), operation())
        .await
        .expect("aggregate receipt");
    assert_eq!(first.pending_retry, 1);
    let pending = world.journal.entry("device-gone-host").await;
    assert!(pending.registrations[0].pending_sync);
    assert_eq!(
        pending.registrations[0].disposition,
        RelayRegistrationDisposition::Tombstone
    );

    let second = world
        .relay()
        .tombstone_push_token(tombstone, operation())
        .await
        .expect("Gone settles idempotent tombstone");
    assert_eq!(second.synchronized, 1);
    assert!(
        world
            .journal
            .entry("device-gone-host")
            .await
            .registrations
            .is_empty()
    );
    assert!(!world.secrets.contains(&token_alias).await);
}

#[tokio::test]
async fn wake_repair_commits_authoritatively_before_ack_and_coalesces_duplicates() {
    let world = TestWorld::new();
    let installation = "installation_wake_host_0001";
    world.enroll("wake-host", installation).await;
    world
        .transport
        .enqueue_page(installation, 0, Ok(page(0, 2, vec![event(1), event(2)])))
        .await;

    let first = world
        .relay()
        .ingest_wake(wake(installation, 2), 100, operation())
        .await
        .expect("wake reconciles");
    assert!(first.changed);
    assert_eq!(first.applied_through_cursor, 2);
    let expected = vec![
        Action::Fetch(installation.to_owned(), 0),
        Action::Repair(
            "wake-host".to_owned(),
            RelayRepairMode::Incremental {
                after_cursor: 0,
                through_cursor: 2,
            },
        ),
        Action::LocalCommit("wake-host".to_owned(), 2),
        Action::Ack(installation.to_owned(), 2),
    ];
    assert_eq!(*world.actions.lock().await, expected);

    let binding = world.journal.entry("wake-host").await;
    assert_eq!(binding.wake.applied_cursor, 2);
    assert_eq!(binding.wake.pending_ack_cursor, None);
    assert_eq!(binding.wake.recently_seen.len(), 1);

    let duplicate = world
        .relay()
        .ingest_wake(wake(installation, 2), 100, operation())
        .await
        .expect("duplicate wake is coalesced");
    assert!(!duplicate.changed);
    assert_eq!(*world.actions.lock().await, expected);
}

#[tokio::test]
async fn failed_ack_preserves_local_commit_and_restart_retries_ack_only() {
    let world = TestWorld::new();
    let installation = "installation_ack_host_0001";
    world.enroll("ack-host", installation).await;
    world
        .transport
        .enqueue_page(installation, 0, Ok(page(0, 2, vec![event(1), event(2)])))
        .await;
    world
        .transport
        .enqueue_ack_error(installation, RelayTransportError::Network)
        .await;

    assert_eq!(
        world
            .relay()
            .ingest_wake(wake(installation, 2), 100, operation())
            .await,
        Err(RelayError::Retryable)
    );
    let committed = world.journal.entry("ack-host").await;
    assert_eq!(committed.wake.applied_cursor, 2);
    assert_eq!(committed.wake.pending_ack_cursor, Some(2));
    let action_count = world.actions.lock().await.len();

    let recovered = world
        .relay()
        .ingest_wake(wake(installation, 2), 100, operation())
        .await
        .expect("reconstructed coordinator retries durable ack");
    assert!(!recovered.changed);
    assert_eq!(world.actions.lock().await.len(), action_count + 1);
    assert_eq!(
        world.actions.lock().await.last(),
        Some(&Action::Ack(installation.to_owned(), 2))
    );
    assert_eq!(
        world
            .journal
            .entry("ack-host")
            .await
            .wake
            .pending_ack_cursor,
        None
    );
}

#[tokio::test]
async fn remote_ack_ahead_survives_reload_then_repairs_and_acks_exact_cursor() {
    let world = TestWorld::new();
    let installation = "installation_ack_ahead_0001";
    world.enroll("ack-ahead-host", installation).await;
    world
        .transport
        .enqueue_page(installation, 0, Ok(page(0, 2, vec![event(1), event(2)])))
        .await;
    world
        .transport
        .enqueue_page(installation, 2, Ok(page(2, 4, vec![event(3), event(4)])))
        .await;
    world.transport.enqueue_ack_receipt(installation, 4).await;

    let receipt = world
        .relay()
        .ingest_wake(wake(installation, 2), 100, operation())
        .await
        .expect("remote-ahead ACK forces bounded authoritative catch-up");

    assert_eq!(receipt.applied_through_cursor, 4);
    assert_eq!(receipt.acknowledged_through_cursor, 4);
    let binding = world.journal.entry("ack-ahead-host").await;
    assert_eq!(binding.wake.applied_cursor, 4);
    assert_eq!(binding.wake.pending_ack_cursor, None);
    assert_eq!(binding.wake.remote_ack_ahead_cursor, None);
    let actions = world.actions.lock().await;
    assert!(actions.contains(&Action::Ack(installation.to_owned(), 2)));
    assert!(actions.contains(&Action::Ack(installation.to_owned(), 4)));
    assert!(actions.contains(&Action::LocalCommit("ack-ahead-host".to_owned(), 4)));
}

#[tokio::test]
async fn cursor_gap_prefers_snapshot_and_falls_back_to_full_repair() {
    let world = TestWorld::new();
    let snapshot_installation = "installation_snapshot_0001";
    let full_installation = "installation_snapshot_0002";
    world.enroll("snapshot-host", snapshot_installation).await;
    world.enroll("full-host", full_installation).await;
    world
        .transport
        .enqueue_page(snapshot_installation, 0, Ok(reset_page(0, 5, true)))
        .await;
    world
        .transport
        .enqueue_snapshot(
            snapshot_installation,
            Ok(RelaySnapshotEnvelope {
                schema_version: RELAY_SCHEMA_VERSION,
                revision: 7,
                through_cursor: 5,
                expires_at_ms: 10_000,
                encoded_bytes: 1_024,
            }),
        )
        .await;
    world
        .transport
        .enqueue_page(full_installation, 0, Ok(reset_page(0, 5, true)))
        .await;
    world
        .transport
        .enqueue_snapshot(
            full_installation,
            Ok(RelaySnapshotEnvelope {
                schema_version: RELAY_SCHEMA_VERSION,
                revision: 8,
                through_cursor: 4,
                expires_at_ms: 10_000,
                encoded_bytes: 1_024,
            }),
        )
        .await;

    world
        .relay()
        .ingest_wake(wake(snapshot_installation, 5), 100, operation())
        .await
        .expect("snapshot repair");
    world
        .relay()
        .ingest_wake(wake(full_installation, 5), 100, operation())
        .await
        .expect("full repair fallback");

    let actions = world.actions.lock().await;
    assert!(actions.contains(&Action::Repair(
        "snapshot-host".to_owned(),
        RelayRepairMode::Snapshot {
            revision: 7,
            through_cursor: 5,
        },
    )));
    assert!(actions.contains(&Action::Repair(
        "full-host".to_owned(),
        RelayRepairMode::Full { through_cursor: 5 },
    )));
}

#[tokio::test]
async fn expired_snapshot_hint_race_falls_back_to_full_without_tombstoning() {
    let world = TestWorld::new();
    let host = "snapshot-race-host";
    let installation = "installation_snapshot_race_0001";
    world.enroll(host, installation).await;
    world
        .transport
        .enqueue_page(installation, 0, Ok(reset_page(0, 5, true)))
        .await;
    world
        .transport
        .enqueue_snapshot(installation, Err(RelayTransportError::NotFound))
        .await;

    let receipt = world
        .relay()
        .ingest_wake(wake(installation, 5), 100, operation())
        .await
        .expect("snapshot expiry is a normal full-repair fallback");
    assert_eq!(receipt.applied_through_cursor, 5);
    assert_eq!(
        world.journal.entry(host).await.state,
        RelayBindingState::Active
    );
    assert!(world.actions.lock().await.contains(&Action::Repair(
        host.to_owned(),
        RelayRepairMode::Full { through_cursor: 5 },
    )));
}

#[tokio::test]
async fn snapshot_and_repair_cursor_accounting_never_under_records_applied_state() {
    let world = TestWorld::new();
    let snapshot_ahead = "installation_snapshot_ahead_0001";
    world.enroll("snapshot-ahead-host", snapshot_ahead).await;
    world
        .transport
        .enqueue_page(snapshot_ahead, 0, Ok(reset_page(0, 5, true)))
        .await;
    world
        .transport
        .enqueue_snapshot(
            snapshot_ahead,
            Ok(RelaySnapshotEnvelope {
                schema_version: RELAY_SCHEMA_VERSION,
                revision: 9,
                through_cursor: 6,
                expires_at_ms: 10_000,
                encoded_bytes: 1_024,
            }),
        )
        .await;
    world
        .relay()
        .ingest_wake(wake(snapshot_ahead, 5), 100, operation())
        .await
        .expect("ahead snapshot falls back to exact full repair");
    assert!(world.actions.lock().await.contains(&Action::Repair(
        "snapshot-ahead-host".to_owned(),
        RelayRepairMode::Full { through_cursor: 5 },
    )));

    let repair_ahead = "installation_repair_ahead_0001";
    world.enroll("repair-ahead-host", repair_ahead).await;
    world
        .transport
        .enqueue_page(repair_ahead, 0, Ok(page(0, 2, vec![event(1), event(2)])))
        .await;
    *world.repair.applied_cursor_override.lock().await = Some(3);
    assert_eq!(
        world
            .relay()
            .ingest_wake(wake(repair_ahead, 2), 100, operation())
            .await,
        Err(RelayError::RepairFailed)
    );
    let binding = world.journal.entry("repair-ahead-host").await;
    assert_eq!(binding.state, RelayBindingState::NeedsRepair);
    assert_eq!(binding.wake.applied_cursor, 0);
    assert!(
        !world
            .actions
            .lock()
            .await
            .contains(&Action::Ack(repair_ahead.to_owned(), 2))
    );
}

#[tokio::test]
async fn foreground_high_watermark_commit_survives_authenticated_reload_and_ack() {
    let world = TestWorld::new();
    let host = "foreground-high-watermark-host";
    let installation = "installation_foreground_high_watermark_0001";
    world.enroll(host, installation).await;
    world
        .transport
        .enqueue_page(
            installation,
            0,
            Ok(page(0, 3, vec![event(1), event(2), event(3)])),
        )
        .await;

    let outcomes = world
        .relay()
        .reconcile_all(100, operation())
        .await
        .expect("foreground discovery repairs and acknowledges high watermark");
    assert!(matches!(
        outcomes.as_slice(),
        [RelayReconcileOutcome::Applied(receipt)]
            if receipt.applied_through_cursor == 3
                && receipt.acknowledged_through_cursor == 3
    ));
    let reloaded = world.journal.entry(host).await;
    assert_eq!(reloaded.wake.highest_seen_cursor, 3);
    assert_eq!(reloaded.wake.applied_cursor, 3);
    assert_eq!(reloaded.wake.pending_ack_cursor, None);
}

#[tokio::test]
async fn no_change_foreground_repair_requires_an_exact_authoritative_cursor() {
    let world = TestWorld::new();
    let host = "no-change-ahead-host";
    let installation = "installation_no_change_ahead_0001";
    world.enroll(host, installation).await;
    *world.repair.applied_cursor_override.lock().await = Some(1);

    let outcomes = world
        .relay()
        .reconcile_all(100, operation())
        .await
        .expect("batch reports host failure");
    assert!(matches!(
        outcomes.as_slice(),
        [RelayReconcileOutcome::Failed {
            host_id,
            error: RelayError::RepairFailed,
        }] if host_id.0 == host
    ));
    let binding = world.journal.entry(host).await;
    assert_eq!(binding.wake.applied_cursor, 0);
    assert_eq!(binding.state, RelayBindingState::NeedsRepair);
    assert!(!world.actions.lock().await.iter().any(|action| {
        matches!(action, Action::LocalCommit(candidate, _) | Action::Ack(candidate, _) if candidate == host || candidate == installation)
    }));
}

#[tokio::test]
async fn duplicate_event_identity_across_pages_fails_before_repair_or_ack() {
    let world = TestWorld::new();
    let installation = "installation_duplicate_0001";
    world.enroll("duplicate-host", installation).await;
    world
        .transport
        .enqueue_page(installation, 0, Ok(page(0, 2, vec![event(1)])))
        .await;
    let mut duplicate = event(2);
    duplicate.event_id = event(1).event_id;
    world
        .transport
        .enqueue_page(installation, 1, Ok(page(1, 2, vec![duplicate])))
        .await;

    assert_eq!(
        world
            .relay()
            .ingest_wake(wake(installation, 2), 100, operation())
            .await,
        Err(RelayError::InvalidResponse)
    );
    assert!(
        !world
            .actions
            .lock()
            .await
            .iter()
            .any(|action| matches!(action, Action::Repair(_, _) | Action::Ack(_, _)))
    );
}

#[tokio::test]
async fn foreground_reconcile_is_per_host_and_does_not_starve_healthy_bindings() {
    let world = TestWorld::new();
    let broken = "installation_foreground_a_0001";
    let healthy = "installation_foreground_b_0001";
    world.enroll("a-broken-host", broken).await;
    world.enroll("b-healthy-host", healthy).await;
    world
        .transport
        .enqueue_page(broken, 0, Err(RelayTransportError::Unauthorized))
        .await;
    world
        .transport
        .enqueue_page(healthy, 0, Ok(page(0, 1, vec![event(1)])))
        .await;

    let outcomes = world
        .relay()
        .reconcile_all(100, operation())
        .await
        .expect("batch itself completes");

    assert_eq!(outcomes.len(), 2);
    assert!(matches!(
        &outcomes[0],
        RelayReconcileOutcome::Failed {
            host_id,
            error: RelayError::RePairRequired,
        } if host_id.0 == "a-broken-host"
    ));
    assert!(matches!(
        &outcomes[1],
        RelayReconcileOutcome::Applied(receipt)
            if receipt.host_id.0 == "b-healthy-host" && receipt.applied_through_cursor == 1
    ));
    assert_eq!(
        world.journal.entry("a-broken-host").await.state,
        RelayBindingState::NeedsRepair
    );
    assert_eq!(
        world
            .journal
            .entry("b-healthy-host")
            .await
            .wake
            .applied_cursor,
        1
    );
    assert!(
        world
            .actions
            .lock()
            .await
            .contains(&Action::Ack(healthy.to_owned(), 1))
    );
}

#[tokio::test]
async fn stalled_first_host_does_not_consume_the_healthy_hosts_batch_budget() {
    let world = TestWorld::new();
    let slow = "installation_foreground_slow_0001";
    let healthy = "installation_foreground_fast_0001";
    world.enroll("a-slow-host", slow).await;
    world.enroll("b-fast-host", healthy).await;
    world
        .transport
        .set_fetch_delay(slow, Duration::from_millis(500))
        .await;
    world
        .transport
        .enqueue_page(healthy, 0, Ok(page(0, 1, vec![event(1)])))
        .await;

    let outcomes = world
        .relay()
        .reconcile_all(
            100,
            RelayOperationContext::with_timeout(Duration::from_millis(150)),
        )
        .await
        .expect("batch returns independent host outcomes");
    assert!(
        outcomes.iter().any(|outcome| matches!(
            outcome,
            RelayReconcileOutcome::Failed {
                host_id,
                error: RelayError::DeadlineExceeded,
            } if host_id.0 == "a-slow-host"
        )),
        "unexpected outcomes: {outcomes:?}"
    );
    assert!(
        outcomes.iter().any(|outcome| matches!(
            outcome,
            RelayReconcileOutcome::Applied(receipt)
                if receipt.host_id.0 == "b-fast-host" && receipt.applied_through_cursor == 1
        )),
        "unexpected outcomes: {outcomes:?}"
    );
    assert_eq!(
        world.journal.entry("b-fast-host").await.wake.applied_cursor,
        1
    );
    assert!(
        world
            .actions
            .lock()
            .await
            .contains(&Action::Ack(healthy.to_owned(), 1))
    );
}

#[tokio::test]
async fn fetch_bounds_and_redirects_fail_closed_while_page_caps_use_full_repair() {
    let world = TestWorld::new();
    let oversized = "installation_bounds_0001";
    let redirected = "installation_bounds_0002";
    let paginated = "installation_bounds_0003";
    world.enroll("oversized-host", oversized).await;
    world.enroll("redirected-host", redirected).await;
    world.enroll("paginated-host", paginated).await;

    let mut oversized_page = page(0, 1, vec![event(1)]);
    oversized_page.encoded_bytes = DEFAULT_MAX_RESPONSE_BYTES + 1;
    world
        .transport
        .enqueue_page(oversized, 0, Ok(oversized_page))
        .await;
    world
        .transport
        .enqueue_page(redirected, 0, Err(RelayTransportError::RedirectRejected))
        .await;
    world
        .transport
        .enqueue_page(paginated, 0, Ok(page(0, 3, vec![event(1)])))
        .await;
    world
        .transport
        .enqueue_page(paginated, 1, Ok(page(1, 3, vec![event(2)])))
        .await;

    assert_eq!(
        world
            .relay()
            .ingest_wake(wake(oversized, 1), 100, operation())
            .await,
        Err(RelayError::PermanentFailure)
    );
    assert_eq!(
        world
            .relay()
            .ingest_wake(wake(redirected, 1), 100, operation())
            .await,
        Err(RelayError::PermanentFailure)
    );
    world
        .relay()
        .with_limits(Duration::from_secs(1), 2)
        .ingest_wake(wake(paginated, 3), 100, operation())
        .await
        .expect("bounded backlog falls back to full repair");
    let actions = world.actions.lock().await;
    assert!(actions.contains(&Action::Repair(
        "paginated-host".to_owned(),
        RelayRepairMode::Full { through_cursor: 3 },
    )));
    assert!(actions.contains(&Action::Ack(paginated.to_owned(), 3)));
    assert!(!actions.iter().any(|action| {
        matches!(
            action,
            Action::Repair(host, _) if host == "oversized-host" || host == "redirected-host"
        )
    }));
    drop(actions);
    assert!(
        world
            .transport
            .contexts
            .lock()
            .await
            .iter()
            .all(|context| !context.follow_redirects)
    );
}

#[tokio::test]
async fn page_count_floor_and_expiry_invariants_fail_closed_and_become_durable_repair_state() {
    let world = TestWorld::new();
    let over_count = "installation_page_count_0001";
    let missing_reset = "installation_page_floor_0001";
    let invalid_floor = "installation_page_floor_0002";
    let expired = "installation_page_expiry_0001";
    world.enroll("page-count-host", over_count).await;
    world.enroll("page-reset-host", missing_reset).await;
    world.enroll("page-floor-host", invalid_floor).await;
    world.enroll("page-expiry-host", expired).await;

    world
        .transport
        .enqueue_page(over_count, 0, Ok(page(0, 2, vec![event(1), event(2)])))
        .await;
    let mut count_operation = operation();
    count_operation.page_limit = 1;
    assert_eq!(
        world
            .relay()
            .ingest_wake(wake(over_count, 2), 100, count_operation)
            .await,
        Err(RelayError::InvalidResponse)
    );

    let mut reset_required = page(0, 1, vec![event(1)]);
    reset_required.replay_floor = 2;
    world
        .transport
        .enqueue_page(missing_reset, 0, Ok(reset_required))
        .await;
    assert_eq!(
        world
            .relay()
            .ingest_wake(wake(missing_reset, 1), 100, operation())
            .await,
        Err(RelayError::InvalidResponse)
    );

    let mut impossible_floor = page(0, 1, vec![event(1)]);
    impossible_floor.replay_floor = 3;
    impossible_floor.reset_required = true;
    impossible_floor.events.clear();
    impossible_floor.next_cursor = 0;
    world
        .transport
        .enqueue_page(invalid_floor, 0, Ok(impossible_floor))
        .await;
    assert_eq!(
        world
            .relay()
            .ingest_wake(wake(invalid_floor, 1), 100, operation())
            .await,
        Err(RelayError::InvalidResponse)
    );

    let mut expired_event = event(1);
    expired_event.expires_at_ms = 100;
    world
        .transport
        .enqueue_page(expired, 0, Ok(page(0, 1, vec![expired_event])))
        .await;
    assert_eq!(
        world
            .relay()
            .ingest_wake(wake(expired, 1), 100, operation())
            .await,
        Err(RelayError::InvalidResponse)
    );

    for host in [
        "page-count-host",
        "page-reset-host",
        "page-floor-host",
        "page-expiry-host",
    ] {
        assert_eq!(
            world.journal.entry(host).await.state,
            RelayBindingState::NeedsRepair
        );
    }
}

#[tokio::test]
async fn timeout_and_cancellation_never_advance_the_applied_cursor() {
    let world = TestWorld::new();
    let installation = "installation_deadline_0001";
    world.enroll("deadline-host", installation).await;
    *world.transport.fetch_delay.lock().await = Some(Duration::from_millis(50));
    world
        .transport
        .enqueue_page(installation, 0, Ok(page(0, 1, vec![event(1)])))
        .await;

    let timeout = world
        .relay()
        .with_limits(Duration::from_millis(10), DEFAULT_MAX_FETCH_PAGES)
        .ingest_wake(wake(installation, 1), 100, operation())
        .await;
    assert_eq!(timeout, Err(RelayError::DeadlineExceeded));
    let timed_out = world.journal.entry("deadline-host").await;
    assert_eq!(timed_out.wake.highest_seen_cursor, 1);
    assert_eq!(timed_out.wake.applied_cursor, 0);
    assert_eq!(timed_out.wake.pending_ack_cursor, None);

    let cancelled_operation = operation();
    cancelled_operation.cancellation.cancel();
    assert_eq!(
        world
            .relay()
            .ingest_wake(wake(installation, 2), 100, cancelled_operation)
            .await,
        Err(RelayError::Cancelled)
    );
    assert_eq!(
        world
            .journal
            .entry("deadline-host")
            .await
            .wake
            .highest_seen_cursor,
        1
    );
}

#[tokio::test]
async fn in_flight_cancellation_and_mutex_wait_honor_the_absolute_operation_budget() {
    let world = TestWorld::new();
    let installation = "installation_cancel_0001";
    world.enroll("cancel-host", installation).await;
    *world.transport.fetch_delay.lock().await = Some(Duration::from_secs(5));
    world
        .transport
        .enqueue_page(installation, 0, Ok(page(0, 1, vec![event(1)])))
        .await;
    let relay = Arc::new(
        world
            .relay()
            .with_limits(Duration::from_secs(5), DEFAULT_MAX_FETCH_PAGES),
    );
    let cancelled_operation = operation();
    let cancellation = cancelled_operation.cancellation.clone();
    let task = {
        let relay = relay.clone();
        tokio::spawn(async move {
            relay
                .ingest_wake(wake(installation, 1), 100, cancelled_operation)
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(10)).await;
    cancellation.cancel();
    assert_eq!(task.await.expect("join"), Err(RelayError::Cancelled));
    assert_eq!(
        world.journal.entry("cancel-host").await.wake.applied_cursor,
        0
    );

    *world.transport.fetch_delay.lock().await = Some(Duration::from_millis(200));
    world
        .transport
        .enqueue_page(installation, 0, Ok(page(0, 1, vec![event(1)])))
        .await;
    let holder = {
        let relay = relay.clone();
        tokio::spawn(async move {
            relay
                .ingest_wake(wake(installation, 1), 100, operation())
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(10)).await;
    let waiting_result = relay
        .observe_push_token(
            observation(1),
            RelayOperationContext::with_timeout(Duration::from_millis(20)),
        )
        .await;
    assert_eq!(waiting_result, Err(RelayError::DeadlineExceeded));
    holder
        .await
        .expect("holder joins")
        .expect("holder completes");

    *world.journal.list_delay.lock().await = Some(Duration::from_millis(50));
    assert_eq!(
        relay
            .observe_push_token(
                observation(2),
                RelayOperationContext::with_timeout(Duration::from_millis(10)),
            )
            .await,
        Err(RelayError::DeadlineExceeded)
    );
}

#[tokio::test]
async fn enrollment_is_idempotent_and_rollback_revokes_before_secret_cleanup() {
    let world = TestWorld::new();
    let host = RelayHostId("rollback-host".to_owned());
    let installation = "installation_rollback_0001";
    let relay = world.relay();
    relay
        .stage_enrollment(enrollment(&host.0, installation), operation())
        .await
        .expect("stage");
    relay
        .stage_enrollment(enrollment(&host.0, installation), operation())
        .await
        .expect("idempotent restage");
    relay
        .commit_enrollment(&host, &command_id(installation), operation())
        .await
        .expect("commit");
    let active = world.journal.entry(&host.0).await;
    let read_alias = active.read_capability_alias.clone();
    let manage_alias = active.manage_capability_alias.clone();

    relay
        .rollback_enrollment(&host, &command_id(installation), operation())
        .await
        .expect("rollback");
    assert_eq!(
        world.actions.lock().await.first(),
        Some(&Action::TombstoneInstallation(installation.to_owned()))
    );
    assert_eq!(
        world.journal.entry(&host.0).await.state,
        RelayBindingState::Tombstoned
    );
    assert!(!world.secrets.contains(&read_alias).await);
    assert!(!world.secrets.contains(&manage_alias).await);
}

#[tokio::test]
async fn tombstoned_host_can_enroll_a_new_authenticated_installation() {
    let world = TestWorld::new();
    let host = RelayHostId("reenroll-after-rollback-host".to_owned());
    let old_installation = "installation_reenroll_old_0001";
    let new_installation = "installation_reenroll_new_0001";
    world.enroll(&host.0, old_installation).await;
    world
        .relay()
        .tombstone_push_token(
            PushTokenTombstone {
                provider: RelayPushProvider::Apns,
                environment: RelayPushEnvironment::Sandbox,
                through_local_generation: 2,
            },
            operation(),
        )
        .await
        .expect("persist provider fence before installation rollback");
    world
        .relay()
        .rollback_enrollment(&host, &command_id(old_installation), operation())
        .await
        .expect("initial rollback");
    assert_eq!(
        world.journal.entry(&host.0).await.state,
        RelayBindingState::Tombstoned
    );
    let action_count = world.actions.lock().await.len();
    let secret_count = world.secrets.count().await;
    assert_eq!(
        world
            .relay()
            .stage_enrollment(enrollment(&host.0, old_installation), operation())
            .await,
        Err(RelayError::InvalidResponse)
    );
    assert_eq!(world.actions.lock().await.len(), action_count);
    assert_eq!(world.secrets.count().await, secret_count);
    assert_eq!(
        world.journal.entry(&host.0).await.state,
        RelayBindingState::Tombstoned
    );

    let relay = world.relay();
    relay
        .stage_enrollment(enrollment(&host.0, new_installation), operation())
        .await
        .expect("authenticated replacement stage");
    relay
        .commit_enrollment(&host, &command_id(new_installation), operation())
        .await
        .expect("authenticated replacement commit");
    let active = world.journal.entry(&host.0).await;
    assert_eq!(active.state, RelayBindingState::Active);
    assert_eq!(active.installation_id.0, new_installation);
    assert_eq!(
        active.provider_tombstone_fences[0].through_local_generation,
        2
    );
    assert_eq!(
        world
            .relay()
            .observe_push_token(observation(2), operation())
            .await,
        Err(RelayError::InvalidProviderRegistration)
    );
    assert!(world.journal.entry(&host.0).await.registrations.is_empty());
}

#[tokio::test]
async fn authorization_repair_can_replace_obsolete_local_authority() {
    let world = TestWorld::new();
    let host = RelayHostId("reenroll-after-auth-host".to_owned());
    let old_installation = "installation_auth_repair_old_0001";
    let new_installation = "installation_auth_repair_new_0001";
    world.enroll(&host.0, old_installation).await;
    world
        .relay()
        .observe_push_token(observation(1), operation())
        .await
        .expect("old provider token");
    let old = world.journal.entry(&host.0).await;
    let old_read = old.read_capability_alias.clone();
    let old_manage = old.manage_capability_alias.clone();
    let old_token = old.registrations[0].token_alias.clone();
    world
        .transport
        .enqueue_page(old_installation, 0, Err(RelayTransportError::Unauthorized))
        .await;
    assert_eq!(
        world
            .relay()
            .ingest_wake(wake(old_installation, 1), 100, operation())
            .await,
        Err(RelayError::RePairRequired)
    );
    assert_eq!(
        world.journal.entry(&host.0).await.state,
        RelayBindingState::NeedsRepair
    );
    world
        .transport
        .enqueue_installation_tombstone_result(
            old_installation,
            Err(RelayTransportError::Unauthorized),
        )
        .await;

    let relay = world.relay();
    relay
        .stage_enrollment(
            enrollment_with_secrets(&host.0, new_installation, 0x91, 0x92),
            operation(),
        )
        .await
        .expect("new authenticated enrollment supersedes rejected authority");
    relay
        .commit_enrollment(&host, &command_id(new_installation), operation())
        .await
        .expect("commit replacement");
    let active = world.journal.entry(&host.0).await;
    assert_eq!(active.state, RelayBindingState::Active);
    assert_eq!(active.installation_id.0, new_installation);
    assert_eq!(active.read_capability_alias, old_read);
    assert_eq!(active.manage_capability_alias, old_manage);
    assert_eq!(
        world
            .secrets
            .read(&active.read_capability_alias)
            .await
            .unwrap()
            .unwrap()
            .expose_for_adapter(),
        &[0x91; 32]
    );
    assert_eq!(
        world
            .secrets
            .read(&active.manage_capability_alias)
            .await
            .unwrap()
            .unwrap()
            .expose_for_adapter(),
        &[0x92; 32]
    );
    assert!(!world.secrets.contains(&old_token).await);
}

#[tokio::test]
async fn failed_repair_replacement_cas_remains_replayable_and_never_tombstone_sticks() {
    let world = TestWorld::new();
    let host = RelayHostId("repair-cas-recovery-host".to_owned());
    let old_installation = "installation_repair_cas_old_0001";
    let new_installation = "installation_repair_cas_new_0001";
    world.enroll(&host.0, old_installation).await;
    world
        .transport
        .enqueue_page(old_installation, 0, Err(RelayTransportError::Unauthorized))
        .await;
    assert_eq!(
        world
            .relay()
            .ingest_wake(wake(old_installation, 1), 100, operation())
            .await,
        Err(RelayError::RePairRequired)
    );
    world.journal.fail_next_cas_before_apply().await;

    assert_eq!(
        world
            .relay()
            .stage_enrollment(enrollment(&host.0, new_installation), operation())
            .await,
        Err(RelayError::JournalUnavailable)
    );
    assert_eq!(
        world.journal.entry(&host.0).await.state,
        RelayBindingState::NeedsRepair
    );
    assert_eq!(
        world
            .relay()
            .rollback_enrollment(&host, &command_id(old_installation), operation())
            .await,
        Err(RelayError::SecureStorageUnavailable)
    );
    assert_eq!(
        world.journal.entry(&host.0).await.state,
        RelayBindingState::NeedsRepair
    );

    let relay = world.relay();
    relay
        .stage_enrollment(enrollment(&host.0, new_installation), operation())
        .await
        .expect("authenticated replacement replay");
    assert_eq!(
        world.journal.entry(&host.0).await.state,
        RelayBindingState::Staged
    );
    relay
        .commit_enrollment(&host, &command_id(new_installation), operation())
        .await
        .expect("replacement commit");
    let active = world.journal.entry(&host.0).await;
    assert_eq!(active.state, RelayBindingState::Active);
    assert_eq!(active.installation_id.0, new_installation);
}

#[tokio::test]
async fn needs_repair_rollback_converges_only_with_confirmed_manage_authority() {
    let valid = TestWorld::new();
    let valid_host = RelayHostId("repair-rollback-valid-host".to_owned());
    let valid_installation = "installation_repair_rollback_valid_0001";
    valid.enroll(&valid_host.0, valid_installation).await;
    let mut malformed = page(0, 1, vec![event(1)]);
    malformed.schema_version = RELAY_SCHEMA_VERSION + 1;
    valid
        .transport
        .enqueue_page(valid_installation, 0, Ok(malformed))
        .await;
    assert_eq!(
        valid
            .relay()
            .ingest_wake(wake(valid_installation, 1), 100, operation())
            .await,
        Err(RelayError::InvalidResponse)
    );
    valid
        .relay()
        .rollback_enrollment(&valid_host, &command_id(valid_installation), operation())
        .await
        .expect("valid manage authority revokes a repair-corrupt binding");
    assert_eq!(
        valid.journal.entry(&valid_host.0).await.state,
        RelayBindingState::Tombstoned
    );

    let rejected = TestWorld::new();
    let rejected_host = RelayHostId("repair-rollback-rejected-host".to_owned());
    let rejected_installation = "installation_repair_rollback_rejected_0001";
    rejected
        .enroll(&rejected_host.0, rejected_installation)
        .await;
    rejected
        .transport
        .enqueue_page(
            rejected_installation,
            0,
            Err(RelayTransportError::Unauthorized),
        )
        .await;
    assert_eq!(
        rejected
            .relay()
            .ingest_wake(wake(rejected_installation, 1), 100, operation())
            .await,
        Err(RelayError::RePairRequired)
    );
    rejected
        .transport
        .enqueue_installation_tombstone_result(
            rejected_installation,
            Err(RelayTransportError::Unauthorized),
        )
        .await;
    assert_eq!(
        rejected
            .relay()
            .rollback_enrollment(
                &rejected_host,
                &command_id(rejected_installation),
                operation(),
            )
            .await,
        Err(RelayError::RePairRequired)
    );
    assert_eq!(
        rejected.journal.entry(&rejected_host.0).await.state,
        RelayBindingState::NeedsRepair
    );

    let missing = TestWorld::new();
    let missing_host = RelayHostId("repair-rollback-missing-host".to_owned());
    let missing_installation = "installation_repair_rollback_missing_0001";
    missing.enroll(&missing_host.0, missing_installation).await;
    let mut malformed = page(0, 1, vec![event(1)]);
    malformed.schema_version = RELAY_SCHEMA_VERSION + 1;
    missing
        .transport
        .enqueue_page(missing_installation, 0, Ok(malformed))
        .await;
    assert_eq!(
        missing
            .relay()
            .ingest_wake(wake(missing_installation, 1), 100, operation())
            .await,
        Err(RelayError::InvalidResponse)
    );
    let manage_alias = missing
        .journal
        .entry(&missing_host.0)
        .await
        .manage_capability_alias;
    missing.secrets.values.lock().await.remove(&manage_alias.0);
    assert_eq!(
        missing
            .relay()
            .rollback_enrollment(
                &missing_host,
                &command_id(missing_installation),
                operation(),
            )
            .await,
        Err(RelayError::SecureStorageUnavailable)
    );
    assert_eq!(
        missing.journal.entry(&missing_host.0).await.state,
        RelayBindingState::NeedsRepair
    );
}

#[tokio::test]
async fn enrollment_secret_cas_failure_before_apply_is_retryable() {
    let world = TestWorld::new();
    let host = RelayHostId("staged-enrollment-host".to_owned());
    let installation = "installation_staged_retry_0001";
    let relay = world.relay();
    world.secrets.fail_next_cas_before_apply().await;

    assert_eq!(
        relay
            .stage_enrollment(enrollment(&host.0, installation), operation())
            .await,
        Err(RelayError::SecureStorageUnavailable)
    );
    assert_eq!(
        world.journal.entry(&host.0).await.state,
        RelayBindingState::Preparing
    );
    assert_eq!(world.secrets.count().await, 0);

    relay
        .stage_enrollment(enrollment(&host.0, installation), operation())
        .await
        .expect("retry writes both capabilities then publishes Staged");
    let staged = world.journal.entry(&host.0).await;
    assert_eq!(staged.state, RelayBindingState::Staged);
    relay
        .commit_enrollment(&host, &command_id(installation), operation())
        .await
        .expect("complete staged enrollment");
    let active = world.journal.entry(&host.0).await;
    assert_eq!(active.state, RelayBindingState::Active);
    assert_eq!(active.read_capability_alias, staged.read_capability_alias);
    assert_eq!(
        active.manage_capability_alias,
        staged.manage_capability_alias
    );
    assert!(world.secrets.contains(&active.read_capability_alias).await);
    assert!(
        world
            .secrets
            .contains(&active.manage_capability_alias)
            .await
    );
}

#[tokio::test]
async fn enrollment_secret_cas_apply_then_unavailable_reuses_fenced_values() {
    let world = TestWorld::new();
    let host = RelayHostId("staged-rollback-host".to_owned());
    let installation = "installation_staged_rollback_0001";
    let relay = world.relay();
    world.secrets.fail_second_cas_after_apply().await;
    assert_eq!(
        relay
            .stage_enrollment(enrollment(&host.0, installation), operation())
            .await,
        Err(RelayError::SecureStorageUnavailable)
    );
    assert_eq!(
        world.journal.entry(&host.0).await.state,
        RelayBindingState::Preparing
    );
    assert_eq!(world.secrets.count().await, 2);
    relay
        .stage_enrollment(enrollment(&host.0, installation), operation())
        .await
        .expect("retry advances the same fenced capability values");
    let staged = world.journal.entry(&host.0).await;
    let values = world.secrets.values.lock().await;
    assert_eq!(values[&staged.read_capability_alias.0].revision, 2);
    assert_eq!(values[&staged.manage_capability_alias.0].revision, 2);
}

#[tokio::test]
async fn enrollment_cas_failure_before_apply_reuses_bounded_aliases() {
    let world = TestWorld::new();
    let host = RelayHostId("staged-cas-before-host".to_owned());
    let installation = "installation_staged_cas_before_0001";
    let relay = world.relay();

    for _ in 0..3 {
        world.journal.fail_next_staged_cas_before_apply().await;
        assert_eq!(
            relay
                .stage_enrollment(enrollment(&host.0, installation), operation())
                .await,
            Err(RelayError::JournalUnavailable)
        );
        assert_eq!(
            world.journal.entry(&host.0).await.state,
            RelayBindingState::Preparing
        );
        assert_eq!(world.secrets.count().await, 2);
    }

    relay
        .stage_enrollment(enrollment(&host.0, installation), operation())
        .await
        .expect("same deterministic aliases remain reusable");
    assert_eq!(world.secrets.count().await, 2);
}

#[tokio::test]
async fn enrollment_cas_apply_then_unavailable_recovers_without_deleting_capabilities() {
    let world = TestWorld::new();
    let host = RelayHostId("staged-cas-after-host".to_owned());
    let installation = "installation_staged_cas_after_0001";
    let relay = world.relay();
    world.journal.fail_next_staged_cas_after_apply().await;

    relay
        .stage_enrollment(enrollment(&host.0, installation), operation())
        .await
        .expect("authoritative reload observes the committed staged command");
    let staged = world.journal.entry(&host.0).await;
    assert_eq!(staged.state, RelayBindingState::Staged);
    assert!(world.secrets.contains(&staged.read_capability_alias).await);
    assert!(
        world
            .secrets
            .contains(&staged.manage_capability_alias)
            .await
    );
    assert_eq!(world.secrets.count().await, 2);
}

#[tokio::test]
async fn different_enrollment_replaces_crash_before_journal_slots() {
    let world = TestWorld::new();
    let host = RelayHostId("staged-replacement-host".to_owned());
    let first_installation = "installation_staged_replacement_a_0001";
    let second_installation = "installation_staged_replacement_b_0001";
    let relay = world.relay();
    world.journal.fail_next_staged_cas_before_apply().await;
    assert_eq!(
        relay
            .stage_enrollment(
                enrollment_with_secrets(&host.0, first_installation, 0x51, 0x52),
                operation(),
            )
            .await,
        Err(RelayError::JournalUnavailable)
    );
    assert_eq!(world.secrets.count().await, 2);

    relay
        .stage_enrollment(
            enrollment_with_secrets(&host.0, second_installation, 0x61, 0x62),
            operation(),
        )
        .await
        .expect("a different authenticated command CAS-replaces stale slots");
    let staged = world.journal.entry(&host.0).await;
    assert_eq!(staged.installation_id.0, second_installation);
    assert_eq!(
        world
            .secrets
            .read(&staged.read_capability_alias)
            .await
            .unwrap()
            .unwrap()
            .expose_for_adapter(),
        &[0x61; 32]
    );
    assert_eq!(
        world
            .secrets
            .read(&staged.manage_capability_alias)
            .await
            .unwrap()
            .unwrap()
            .expose_for_adapter(),
        &[0x62; 32]
    );
}

#[tokio::test]
async fn late_old_secret_cas_cannot_overwrite_new_enrollment() {
    let world = TestWorld::new();
    let host = RelayHostId("staged-late-cas-host".to_owned());
    let first_installation = "installation_staged_late_cas_a_0001";
    let second_installation = "installation_staged_late_cas_b_0001";
    let relay = world.relay();
    world
        .secrets
        .delay_next_cas_after_drop(Duration::from_millis(50))
        .await;
    assert_eq!(
        relay
            .stage_enrollment(
                enrollment_with_secrets(&host.0, first_installation, 0x71, 0x72),
                RelayOperationContext::with_timeout(Duration::from_millis(10)),
            )
            .await,
        Err(RelayError::DeadlineExceeded)
    );

    relay
        .stage_enrollment(
            enrollment_with_secrets(&host.0, second_installation, 0x81, 0x82),
            operation(),
        )
        .await
        .expect("new command wins the fenced slot CAS");
    tokio::time::sleep(Duration::from_millis(70)).await;
    let staged = world.journal.entry(&host.0).await;
    assert_eq!(staged.installation_id.0, second_installation);
    assert_eq!(
        world
            .secrets
            .read(&staged.read_capability_alias)
            .await
            .unwrap()
            .unwrap()
            .expose_for_adapter(),
        &[0x81; 32]
    );
    assert_eq!(
        world
            .secrets
            .read(&staged.manage_capability_alias)
            .await
            .unwrap()
            .unwrap()
            .expose_for_adapter(),
        &[0x82; 32]
    );
}

async fn assert_staged_command_and_capabilities(
    world: &TestWorld,
    host: &RelayHostId,
    installation: &str,
    command_id: &RelayEnrollmentCommandId,
    read_byte: u8,
    manage_byte: u8,
) {
    let staged = world.journal.entry(&host.0).await;
    assert_eq!(staged.state, RelayBindingState::Staged);
    assert_eq!(staged.installation_id.0, installation);
    assert_eq!(&staged.staging_command_id, command_id);
    let read_revision = staged
        .read_capability_revision
        .expect("staged read slot revision");
    let manage_revision = staged
        .manage_capability_revision
        .expect("staged manage slot revision");
    let values = world.secrets.values.lock().await;
    assert_eq!(values.len(), 2);
    let read = &values[&staged.read_capability_alias.0];
    let manage = &values[&staged.manage_capability_alias.0];
    assert_eq!(read.revision, read_revision);
    assert_eq!(manage.revision, manage_revision);
    assert_eq!(
        read.secret
            .as_ref()
            .expect("staged read capability")
            .expose_for_adapter(),
        &[read_byte; 32]
    );
    assert_eq!(
        manage
            .secret
            .as_ref()
            .expect("staged manage capability")
            .expose_for_adapter(),
        &[manage_byte; 32]
    );
}

#[tokio::test]
async fn newer_enrollment_wins_before_a_late_staged_journal_cas() {
    let world = TestWorld::new();
    let host = RelayHostId("late-journal-b-wins-host".to_owned());
    let installation = "installation_late_journal_same_0001";
    let command_a =
        RelayEnrollmentCommandId::parse("command_late_journal_a_0001").expect("command A");
    let command_b =
        RelayEnrollmentCommandId::parse("command_late_journal_b_0001").expect("command B");
    let mut enrollment_a = enrollment_with_secrets(&host.0, installation, 0xa1, 0xa2);
    enrollment_a.command_id = command_a.clone();
    let mut enrollment_b = enrollment_with_secrets(&host.0, installation, 0xb1, 0xb2);
    enrollment_b.command_id = command_b.clone();
    let gate = world.journal.gate_next_staged_cas_after_drop().await;
    let relay_a = world.relay();
    let task_a = tokio::spawn(async move {
        relay_a
            .stage_enrollment(
                enrollment_a,
                RelayOperationContext::with_timeout(Duration::from_millis(50)),
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), gate.wait_until_started())
        .await
        .expect("A reached its staged journal CAS");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), task_a)
            .await
            .expect("A respects its operation deadline")
            .expect("A task joins"),
        Err(RelayError::DeadlineExceeded)
    );

    world
        .relay()
        .stage_enrollment(enrollment_b.clone(), operation())
        .await
        .expect("B supersedes A before the late CAS resumes");
    gate.release();
    tokio::time::timeout(Duration::from_secs(1), gate.wait_until_completed())
        .await
        .expect("late A CAS completes with a fenced conflict");
    assert_staged_command_and_capabilities(&world, &host, installation, &command_b, 0xb1, 0xb2)
        .await;
    assert_eq!(
        world
            .relay()
            .commit_enrollment(&host, &command_a, operation())
            .await,
        Err(RelayError::InvalidResponse)
    );
    let restarted = world.relay();
    restarted
        .stage_enrollment(enrollment_b, operation())
        .await
        .expect("restart verifies B's exact staged slot revisions");
    restarted
        .commit_enrollment(&host, &command_b, operation())
        .await
        .expect("only B activates");
}

#[tokio::test]
async fn newer_enrollment_supersedes_a_staged_command_that_lands_after_timeout() {
    let world = TestWorld::new();
    let host = RelayHostId("late-journal-a-first-host".to_owned());
    let installation = "installation_late_journal_first_0001";
    let command_a =
        RelayEnrollmentCommandId::parse("command_late_journal_first_a_0001").expect("command A");
    let command_b =
        RelayEnrollmentCommandId::parse("command_late_journal_first_b_0001").expect("command B");
    let mut enrollment_a = enrollment_with_secrets(&host.0, installation, 0xc1, 0xc2);
    enrollment_a.command_id = command_a.clone();
    let mut enrollment_b = enrollment_with_secrets(&host.0, installation, 0xd1, 0xd2);
    enrollment_b.command_id = command_b.clone();
    let gate = world.journal.gate_next_staged_cas_after_drop().await;
    let relay_a = world.relay();
    let task_a = tokio::spawn(async move {
        relay_a
            .stage_enrollment(
                enrollment_a,
                RelayOperationContext::with_timeout(Duration::from_millis(50)),
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), gate.wait_until_started())
        .await
        .expect("A reached its staged journal CAS");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), task_a)
            .await
            .expect("A respects its operation deadline")
            .expect("A task joins"),
        Err(RelayError::DeadlineExceeded)
    );
    gate.release();
    tokio::time::timeout(Duration::from_secs(1), gate.wait_until_completed())
        .await
        .expect("late A publication lands");
    assert_staged_command_and_capabilities(&world, &host, installation, &command_a, 0xc1, 0xc2)
        .await;

    world
        .relay()
        .stage_enrollment(enrollment_b, operation())
        .await
        .expect("new authenticated B supersedes stale staged A");
    assert_staged_command_and_capabilities(&world, &host, installation, &command_b, 0xd1, 0xd2)
        .await;
    assert_eq!(
        world
            .relay()
            .commit_enrollment(&host, &command_a, operation())
            .await,
        Err(RelayError::InvalidResponse)
    );
    world
        .relay()
        .commit_enrollment(&host, &command_b, operation())
        .await
        .expect("B remains activatable across coordinator instances");
}

#[tokio::test]
async fn preparing_rollback_fence_defeats_a_late_staged_journal_cas() {
    let world = TestWorld::new();
    let host = RelayHostId("rollback-late-journal-fenced-host".to_owned());
    let installation = "installation_rollback_late_journal_fenced_0001";
    let command = command_id(installation);
    let gate = world.journal.gate_next_staged_cas_after_drop().await;
    let relay = world.relay();
    let pending_enrollment = enrollment(&host.0, installation);
    let stage = tokio::spawn(async move {
        relay
            .stage_enrollment(
                pending_enrollment,
                RelayOperationContext::with_timeout(Duration::from_millis(50)),
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), gate.wait_until_started())
        .await
        .expect("staged publication enters native CAS");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), stage)
            .await
            .expect("stage observes its deadline")
            .expect("stage task joins"),
        Err(RelayError::DeadlineExceeded)
    );

    let preparing = world.journal.entry(&host.0).await;
    assert_eq!(preparing.state, RelayBindingState::Preparing);
    let read_alias = preparing.read_capability_alias.clone();
    let manage_alias = preparing.manage_capability_alias.clone();
    world
        .relay()
        .rollback_enrollment(&host, &command, operation())
        .await
        .expect("rollback publishes its fence before slot cleanup");
    assert_eq!(
        world.journal.entry(&host.0).await.state,
        RelayBindingState::Tombstoned
    );

    gate.release();
    tokio::time::timeout(Duration::from_secs(1), gate.wait_until_completed())
        .await
        .expect("late staged CAS completes with a revision conflict");
    assert_eq!(
        world.journal.entry(&host.0).await.state,
        RelayBindingState::Tombstoned
    );
    assert!(!world.secrets.contains(&read_alias).await);
    assert!(!world.secrets.contains(&manage_alias).await);
    assert!(world.actions.lock().await.iter().all(|action| {
        !matches!(action, Action::TombstoneInstallation(value) if value == installation)
    }));
    assert_eq!(
        world
            .relay()
            .commit_enrollment(&host, &command, operation())
            .await,
        Err(RelayError::InvalidResponse)
    );
}

#[tokio::test]
async fn rollback_revokes_a_late_staged_journal_cas_that_lands_first() {
    let world = TestWorld::new();
    let host = RelayHostId("rollback-late-journal-staged-host".to_owned());
    let installation = "installation_rollback_late_journal_staged_0001";
    let command = command_id(installation);
    let gate = world.journal.gate_next_staged_cas_after_drop().await;
    let relay = world.relay();
    let pending_enrollment = enrollment(&host.0, installation);
    let stage = tokio::spawn(async move {
        relay
            .stage_enrollment(
                pending_enrollment,
                RelayOperationContext::with_timeout(Duration::from_millis(50)),
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), gate.wait_until_started())
        .await
        .expect("staged publication enters native CAS");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), stage)
            .await
            .expect("stage observes its deadline")
            .expect("stage task joins"),
        Err(RelayError::DeadlineExceeded)
    );

    gate.release();
    tokio::time::timeout(Duration::from_secs(1), gate.wait_until_completed())
        .await
        .expect("late staged publication lands before rollback");
    assert_eq!(
        world.journal.entry(&host.0).await.state,
        RelayBindingState::Staged
    );
    world
        .relay()
        .rollback_enrollment(&host, &command, operation())
        .await
        .expect("published Staged row is remotely revoked before cleanup");
    assert_eq!(
        world.journal.entry(&host.0).await.state,
        RelayBindingState::Tombstoned
    );
    assert_eq!(
        world
            .actions
            .lock()
            .await
            .iter()
            .filter(|action| {
                matches!(action, Action::TombstoneInstallation(value) if value == installation)
            })
            .count(),
        1
    );
}

#[tokio::test]
async fn preparing_rollback_slot_tombstone_defeats_a_late_secret_cas() {
    let world = TestWorld::new();
    let host = RelayHostId("rollback-late-secret-cas-host".to_owned());
    let installation = "installation_rollback_late_secret_cas_0001";
    let command = command_id(installation);
    let gate = world.secrets.gate_next_cas_after_drop().await;
    let relay = world.relay();
    let pending_enrollment = enrollment(&host.0, installation);
    let stage = tokio::spawn(async move {
        relay
            .stage_enrollment(
                pending_enrollment,
                RelayOperationContext::with_timeout(Duration::from_millis(50)),
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), gate.wait_until_started())
        .await
        .expect("first capability enters native CAS");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), stage)
            .await
            .expect("stage observes its deadline")
            .expect("stage task joins"),
        Err(RelayError::DeadlineExceeded)
    );

    let preparing = world.journal.entry(&host.0).await;
    assert_eq!(preparing.state, RelayBindingState::Preparing);
    let read_alias = preparing.read_capability_alias.clone();
    let manage_alias = preparing.manage_capability_alias.clone();
    world
        .relay()
        .rollback_enrollment(&host, &command, operation())
        .await
        .expect("rollback fences both capability slots");

    gate.release();
    tokio::time::timeout(Duration::from_secs(1), gate.wait_until_completed())
        .await
        .expect("late secret CAS completes with a revision conflict");
    assert_eq!(
        world.journal.entry(&host.0).await.state,
        RelayBindingState::Tombstoned
    );
    assert_eq!(
        world.secrets.revision(&read_alias).await.unwrap(),
        RelaySecretRevision::Found(1)
    );
    assert_eq!(
        world.secrets.revision(&manage_alias).await.unwrap(),
        RelaySecretRevision::Found(1)
    );
    assert!(world.secrets.read(&read_alias).await.unwrap().is_none());
    assert!(world.secrets.read(&manage_alias).await.unwrap().is_none());

    let next_installation = "installation_rollback_late_secret_cas_0002";
    let restarted = world.relay();
    restarted
        .stage_enrollment(enrollment(&host.0, next_installation), operation())
        .await
        .expect("new authenticated enrollment advances tombstoned slots");
    restarted
        .commit_enrollment(&host, &command_id(next_installation), operation())
        .await
        .expect("new enrollment activates");
    let active = world.journal.entry(&host.0).await;
    assert_eq!(active.state, RelayBindingState::Active);
    assert_eq!(active.read_capability_revision, Some(2));
    assert_eq!(active.manage_capability_revision, Some(2));
}

#[tokio::test]
async fn terminal_capability_tombstone_defeats_a_superseded_late_secret_cas() {
    let world = TestWorld::new();
    let host = RelayHostId("terminal-late-secret-cas-host".to_owned());
    let first_installation = "installation_terminal_late_secret_cas_0001";
    let second_installation = "installation_terminal_late_secret_cas_0002";
    let gate = world.secrets.gate_next_cas_after_drop().await;
    let relay = world.relay();
    let pending_enrollment = enrollment_with_secrets(&host.0, first_installation, 0x91, 0x92);
    let stage = tokio::spawn(async move {
        relay
            .stage_enrollment(
                pending_enrollment,
                RelayOperationContext::with_timeout(Duration::from_millis(50)),
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), gate.wait_until_started())
        .await
        .expect("old capability enters native CAS");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), stage)
            .await
            .expect("old stage observes its deadline")
            .expect("old stage task joins"),
        Err(RelayError::DeadlineExceeded)
    );

    world
        .relay()
        .stage_enrollment(
            enrollment_with_secrets(&host.0, second_installation, 0xa1, 0xa2),
            operation(),
        )
        .await
        .expect("new command stages while old callback remains suspended");
    let staged = world.journal.entry(&host.0).await;
    let read_alias = staged.read_capability_alias.clone();
    let manage_alias = staged.manage_capability_alias.clone();
    world
        .relay()
        .rollback_enrollment(&host, &command_id(second_installation), operation())
        .await
        .expect("terminal cleanup leaves revision tombstones");

    gate.release();
    tokio::time::timeout(Duration::from_secs(1), gate.wait_until_completed())
        .await
        .expect("superseded callback conflicts with terminal tombstone");
    assert_eq!(
        world.journal.entry(&host.0).await.state,
        RelayBindingState::Tombstoned
    );
    assert!(!world.secrets.contains(&read_alias).await);
    assert!(!world.secrets.contains(&manage_alias).await);
    assert!(matches!(
        world.secrets.revision(&read_alias).await.unwrap(),
        RelaySecretRevision::Found(revision) if revision >= 2
    ));
    assert!(matches!(
        world.secrets.revision(&manage_alias).await.unwrap(),
        RelaySecretRevision::Found(revision) if revision >= 2
    ));
}

#[tokio::test]
async fn preparing_rollback_recovers_an_ambiguous_applied_slot_tombstone() {
    let world = TestWorld::new();
    let host = RelayHostId("rollback-ambiguous-tombstone-host".to_owned());
    let installation = "installation_rollback_ambiguous_tombstone_0001";
    world.journal.fail_next_staged_cas_before_apply().await;
    assert_eq!(
        world
            .relay()
            .stage_enrollment(enrollment(&host.0, installation), operation())
            .await,
        Err(RelayError::JournalUnavailable)
    );
    let preparing = world.journal.entry(&host.0).await;
    let read_alias = preparing.read_capability_alias.clone();
    let manage_alias = preparing.manage_capability_alias.clone();
    world.secrets.fail_next_tombstone_after_apply().await;

    world
        .relay()
        .rollback_enrollment(&host, &command_id(installation), operation())
        .await
        .expect("authoritative reload observes the applied tombstone");
    assert_eq!(
        world.journal.entry(&host.0).await.state,
        RelayBindingState::Tombstoned
    );
    assert_eq!(
        world.secrets.revision(&read_alias).await.unwrap(),
        RelaySecretRevision::Found(2)
    );
    assert_eq!(
        world.secrets.revision(&manage_alias).await.unwrap(),
        RelaySecretRevision::Found(2)
    );
    assert!(!world.secrets.contains(&read_alias).await);
    assert!(!world.secrets.contains(&manage_alias).await);
}

#[tokio::test]
async fn reconcile_resumes_a_durable_preparing_rollback_fence() {
    let world = TestWorld::new();
    let host = RelayHostId("rollback-reconcile-fence-host".to_owned());
    let installation = "installation_rollback_reconcile_fence_0001";
    world.journal.fail_next_staged_cas_before_apply().await;
    assert_eq!(
        world
            .relay()
            .stage_enrollment(enrollment(&host.0, installation), operation())
            .await,
        Err(RelayError::JournalUnavailable)
    );
    let preparing = world.journal.entry(&host.0).await;
    world
        .secrets
        .fail_next_tombstone_before_apply(&preparing.read_capability_alias)
        .await;

    assert_eq!(
        world
            .relay()
            .rollback_enrollment(&host, &command_id(installation), operation())
            .await,
        Err(RelayError::SecureStorageUnavailable)
    );
    assert_eq!(
        world.journal.entry(&host.0).await.state,
        RelayBindingState::RollbackPending
    );
    assert_eq!(
        world
            .relay()
            .stage_enrollment(
                enrollment(&host.0, "installation_rollback_reconcile_fence_0002"),
                operation(),
            )
            .await,
        Err(RelayError::InvalidResponse)
    );

    let outcomes = world
        .relay()
        .reconcile_all(100, operation())
        .await
        .expect("foreground reconcile resumes local rollback cleanup");
    assert!(matches!(
        outcomes.as_slice(),
        [RelayReconcileOutcome::CleanupCompleted { host_id }] if host_id == &host
    ));
    assert_eq!(
        world.journal.entry(&host.0).await.state,
        RelayBindingState::Tombstoned
    );
    assert!(
        world
            .secrets
            .read(&preparing.read_capability_alias)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        world
            .secrets
            .read(&preparing.manage_capability_alias)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn enrollment_stage_and_commit_bound_stalled_secret_and_journal_ports() {
    let world = TestWorld::new();
    let host = RelayHostId("bounded-enrollment-host".to_owned());
    let installation = "installation_bounded_enrollment_0001";
    let relay = world.relay();

    *world.journal.load_delay.lock().await = Some(Duration::from_millis(50));
    assert_eq!(
        relay
            .stage_enrollment(
                enrollment(&host.0, installation),
                RelayOperationContext::with_timeout(Duration::from_millis(10)),
            )
            .await,
        Err(RelayError::DeadlineExceeded)
    );
    assert!(!world.journal.entries.lock().await.contains_key(&host.0));

    *world.journal.load_delay.lock().await = None;
    world
        .secrets
        .delay_next_cas_after_drop(Duration::from_millis(50))
        .await;
    assert_eq!(
        relay
            .stage_enrollment(
                enrollment(&host.0, installation),
                RelayOperationContext::with_timeout(Duration::from_millis(10)),
            )
            .await,
        Err(RelayError::DeadlineExceeded)
    );
    assert_eq!(
        world.journal.entry(&host.0).await.state,
        RelayBindingState::Preparing
    );
    tokio::time::sleep(Duration::from_millis(70)).await;
    assert!(world.secrets.count().await <= 1);

    relay
        .stage_enrollment(enrollment(&host.0, installation), operation())
        .await
        .expect("retry publishes only after both writes complete");
    *world.journal.load_delay.lock().await = Some(Duration::from_millis(50));
    assert_eq!(
        relay
            .commit_enrollment(
                &host,
                &command_id(installation),
                RelayOperationContext::with_timeout(Duration::from_millis(10)),
            )
            .await,
        Err(RelayError::DeadlineExceeded)
    );
    assert_eq!(
        world.journal.entry(&host.0).await.state,
        RelayBindingState::Staged
    );
    *world.journal.load_delay.lock().await = None;
    relay
        .commit_enrollment(&host, &command_id(installation), operation())
        .await
        .expect("bounded commit retry");
    assert_eq!(
        world.journal.entry(&host.0).await.state,
        RelayBindingState::Active
    );
}

#[tokio::test]
async fn cleanup_pending_resumes_after_every_secret_deletion_boundary() {
    for failure_boundary in 0..3 {
        let world = TestWorld::new();
        let host_name = format!("cleanup-host-{failure_boundary}");
        let installation = format!("installation_cleanup_{failure_boundary:04}");
        world.enroll(&host_name, &installation).await;
        world
            .relay()
            .observe_push_token(observation(1), operation())
            .await
            .expect("register token");
        let active = world.journal.entry(&host_name).await;
        let token_alias = active.registrations[0].token_alias.clone();
        let read_alias = active.read_capability_alias.clone();
        let manage_alias = active.manage_capability_alias.clone();
        let failing_alias = match failure_boundary {
            0 => &token_alias,
            1 => &read_alias,
            _ => &manage_alias,
        };
        world
            .secrets
            .fail_next_tombstone_before_apply(failing_alias)
            .await;

        assert_eq!(
            world
                .relay()
                .rollback_enrollment(
                    &RelayHostId(host_name.clone()),
                    &command_id(&installation),
                    operation(),
                )
                .await,
            Err(RelayError::SecureStorageUnavailable)
        );
        assert_eq!(
            world.journal.entry(&host_name).await.state,
            RelayBindingState::CleanupPending
        );

        let recovery = world
            .relay()
            .reconcile_all(100, operation())
            .await
            .expect("foreground recovery resumes cleanup");
        assert!(matches!(
            recovery.as_slice(),
            [RelayReconcileOutcome::CleanupCompleted { host_id }] if host_id.0 == host_name
        ));
        assert_eq!(
            world.journal.entry(&host_name).await.state,
            RelayBindingState::Tombstoned
        );
        assert!(!world.secrets.contains(&token_alias).await);
        assert!(!world.secrets.contains(&read_alias).await);
        assert!(!world.secrets.contains(&manage_alias).await);
        assert_eq!(
            world
                .actions
                .lock()
                .await
                .iter()
                .filter(|action| {
                    matches!(action, Action::TombstoneInstallation(value) if value == &installation)
                })
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn terminal_binding_cleanup_deletes_current_and_retired_pending_token_aliases() {
    let world = TestWorld::new();
    let host = "terminal-token-cleanup-host";
    let installation = "installation_terminal_token_cleanup_0001";
    world.enroll(host, installation).await;
    world
        .relay()
        .observe_push_token(observation(1), operation())
        .await
        .expect("initial token");
    world
        .transport
        .enqueue_register_error(installation, RelayTransportError::Timeout)
        .await;
    world
        .relay()
        .observe_push_token(observation(2), operation())
        .await
        .expect("pending rotation");
    let pending = world.journal.entry(host).await;
    let current_alias = pending.registrations[0].token_alias.clone();
    let retired_alias = pending.registrations[0]
        .previous
        .as_ref()
        .expect("prior token alias")
        .token_alias
        .clone();
    assert!(pending.registrations[0].pending_sync);

    world
        .relay()
        .rollback_enrollment(
            &RelayHostId(host.to_owned()),
            &command_id(installation),
            operation(),
        )
        .await
        .expect("terminal cleanup");
    assert_eq!(
        world.journal.entry(host).await.state,
        RelayBindingState::Tombstoned
    );
    assert!(!world.secrets.contains(&current_alias).await);
    assert!(!world.secrets.contains(&retired_alias).await);
}

#[tokio::test]
async fn tombstone_response_loss_then_gone_converges_to_local_cleanup() {
    let world = TestWorld::new();
    let host = RelayHostId("gone-host".to_owned());
    let installation = "installation_gone_0001";
    world.enroll(&host.0, installation).await;
    world
        .transport
        .enqueue_installation_tombstone_result(installation, Err(RelayTransportError::Timeout))
        .await;
    world
        .transport
        .enqueue_installation_tombstone_result(installation, Err(RelayTransportError::Gone))
        .await;

    assert_eq!(
        world
            .relay()
            .rollback_enrollment(&host, &command_id(installation), operation())
            .await,
        Err(RelayError::DeadlineExceeded)
    );
    assert_eq!(
        world.journal.entry(&host.0).await.state,
        RelayBindingState::TombstonePending
    );
    world
        .relay()
        .rollback_enrollment(&host, &command_id(installation), operation())
        .await
        .expect("Gone is success for durable tombstone intent");
    assert_eq!(
        world.journal.entry(&host.0).await.state,
        RelayBindingState::Tombstoned
    );
}

#[test]
fn origin_and_retry_policy_are_fail_closed() {
    let rendered_secret = format!("{:?}", secret(0x41));
    assert_eq!(rendered_secret, "OpaqueRelaySecret(<redacted>)");
    assert!(!rendered_secret.contains("65"));
    assert_eq!(
        ValidatedRelayOrigin::parse("http://relay.example", false),
        Err(RelayError::InsecureRelayOrigin)
    );
    assert_eq!(
        ValidatedRelayOrigin::parse("https://user@relay.example", false),
        Err(RelayError::InvalidRelayOrigin)
    );
    assert_eq!(
        ValidatedRelayOrigin::parse("https://relay.example/api", false),
        Err(RelayError::InvalidRelayOrigin)
    );
    assert!(ValidatedRelayOrigin::parse("http://127.0.0.1:8080", true).is_ok());
    assert_eq!(
        RelayTransportError::Network.retry_class(),
        RelayRetryClass::Retry
    );
    assert_eq!(
        RelayTransportError::Unauthorized.retry_class(),
        RelayRetryClass::RePairRequired
    );
    assert_eq!(
        RelayTransportError::Gone.retry_class(),
        RelayRetryClass::Tombstoned
    );
    assert_eq!(
        RelayTransportError::ResponseTooLarge.retry_class(),
        RelayRetryClass::Permanent
    );
}
