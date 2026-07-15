use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use tokio::sync::Mutex;

use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Action {
    Register(String),
    Fetch(String, u64),
    Snapshot(String),
    Repair(String, RelayRepairMode),
    LocalCommit(String, u64),
    Ack(String, u64),
    TombstoneInstallation(String),
}

#[derive(Default)]
struct MemoryJournal {
    entries: Mutex<BTreeMap<String, RelayBindingEntry>>,
    actions: Arc<Mutex<Vec<Action>>>,
    list_delay: Mutex<Option<Duration>>,
    load_delay: Mutex<Option<Duration>>,
    cas_failures_before_apply: Mutex<u32>,
}

impl MemoryJournal {
    fn with_actions(actions: Arc<Mutex<Vec<Action>>>) -> Self {
        Self {
            entries: Mutex::new(BTreeMap::new()),
            actions,
            list_delay: Mutex::new(None),
            load_delay: Mutex::new(None),
            cas_failures_before_apply: Mutex::new(0),
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

    async fn compare_and_swap(
        &self,
        host_id: &RelayHostId,
        expected_revision: Option<u64>,
        replacement: RelayBindingEntry,
    ) -> Result<(), RelayJournalError> {
        if replacement.host_id != *host_id {
            return Err(RelayJournalError::Conflict);
        }
        let mut entries = self.entries.lock().await;
        let current = entries.get(&host_id.0);
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
        let prior_applied = current.map_or(0, |entry| entry.wake.applied_cursor);
        if replacement.wake.applied_cursor > prior_applied {
            self.actions.lock().await.push(Action::LocalCommit(
                host_id.0.clone(),
                replacement.wake.applied_cursor,
            ));
        }
        entries.insert(host_id.0.clone(), replacement);
        Ok(())
    }
}

#[derive(Default)]
struct MemorySecrets {
    values: Mutex<HashMap<String, OpaqueRelaySecret>>,
    write_failures: Mutex<u32>,
    write_delay: Mutex<Option<Duration>>,
    delete_failures: Mutex<HashMap<String, u32>>,
}

impl MemorySecrets {
    async fn contains(&self, alias: &RelaySecretAlias) -> bool {
        self.values.lock().await.contains_key(&alias.0)
    }

    async fn count(&self) -> usize {
        self.values.lock().await.len()
    }

    async fn fail_next_delete(&self, alias: &RelaySecretAlias) {
        self.delete_failures.lock().await.insert(alias.0.clone(), 1);
    }

    async fn fail_next_write(&self) {
        *self.write_failures.lock().await += 1;
    }
}

#[async_trait]
impl OpaqueRelaySecretPort for MemorySecrets {
    async fn read(
        &self,
        alias: &RelaySecretAlias,
    ) -> Result<Option<OpaqueRelaySecret>, RelaySecretStoreError> {
        Ok(self.values.lock().await.get(&alias.0).cloned())
    }

    async fn write(
        &self,
        alias: &RelaySecretAlias,
        secret: OpaqueRelaySecret,
    ) -> Result<(), RelaySecretStoreError> {
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
        self.values.lock().await.insert(alias.0.clone(), secret);
        Ok(())
    }

    async fn delete(&self, alias: &RelaySecretAlias) -> Result<(), RelaySecretStoreError> {
        let mut failures = self.delete_failures.lock().await;
        if let Some(remaining) = failures.get_mut(&alias.0)
            && *remaining > 0
        {
            *remaining -= 1;
            return Err(RelaySecretStoreError::Unavailable);
        }
        drop(failures);
        self.values.lock().await.remove(&alias.0);
        Ok(())
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
        *counts.entry(installation.clone()).or_default() += 1;
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
            registration_id: RelayRegistrationId::parse(format!("registration_{installation}"))
                .expect("test registration id"),
            provider: request.provider,
            environment: request.environment,
            generation: 1,
            replaced: false,
        })
    }

    async fn tombstone_device(
        &self,
        context: RelayTransportContext,
        request: RelayTombstoneDeviceRequest,
    ) -> Result<(), RelayTransportError> {
        self.capture(&context).await;
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
        mode: RelayRepairMode,
        operation: RelayOperationContext,
    ) -> Result<RelayRepairReceipt, RelayRepairError> {
        if operation.cancellation.is_cancelled() {
            return Err(RelayRepairError::Cancelled);
        }
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
        relay
            .stage_enrollment(enrollment(host, installation), operation())
            .await
            .expect("stage enrollment");
        relay
            .commit_enrollment(&RelayHostId(host.to_owned()), operation())
            .await
            .expect("commit enrollment");
    }
}

fn secret(byte: u8) -> OpaqueRelaySecret {
    OpaqueRelaySecret::new(vec![byte; 32]).expect("test secret")
}

fn enrollment(host: &str, installation: &str) -> RelayEnrollment {
    RelayEnrollment {
        host_id: RelayHostId(host.to_owned()),
        origin: ValidatedRelayOrigin::parse(&format!("https://{host}.relay.example/"), false)
            .expect("secure origin"),
        installation_id: RelayInstallationId::parse(installation).expect("installation id"),
        read_capability: secret(0x41),
        manage_capability: secret(0x42),
    }
}

fn observation(generation: u64) -> PushTokenObservation {
    PushTokenObservation {
        provider: RelayPushProvider::Apns,
        environment: RelayPushEnvironment::Sandbox,
        token: secret(0x77),
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

    world.secrets.fail_next_write().await;
    let failed_write = world
        .relay()
        .observe_push_token(observation(2), operation())
        .await
        .expect("durable reservation survives a secret failure");
    assert_eq!(failed_write.pending_retry, 1);
    let staged = world.journal.entry(host).await;
    assert_eq!(staged.registrations[0].local_generation, 2);
    assert!(staged.registrations[0].pending_sync);
    assert_eq!(staged.retired_token_aliases, vec![old_alias.clone()]);
    assert!(
        !world
            .secrets
            .contains(&staged.registrations[0].token_alias)
            .await
    );
    assert!(world.secrets.contains(&old_alias).await);

    world.secrets.fail_next_delete(&old_alias).await;
    let failed_cleanup = world
        .relay()
        .observe_push_token(observation(2), operation())
        .await
        .expect("remote sync succeeds while cleanup remains durable");
    assert_eq!(failed_cleanup.pending_retry, 1);
    let cleanup_pending = world.journal.entry(host).await;
    assert!(!cleanup_pending.registrations[0].pending_sync);
    assert_eq!(
        cleanup_pending.retired_token_aliases,
        vec![old_alias.clone()]
    );
    assert!(world.secrets.contains(&old_alias).await);

    world
        .relay()
        .reconcile_all(100, operation())
        .await
        .expect("foreground retry cleans the retired alias");
    let recovered = world.journal.entry(host).await;
    assert!(recovered.retired_token_aliases.is_empty());
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
    assert_eq!(pending.retired_token_aliases, vec![old_alias.clone()]);

    world
        .relay()
        .reconcile_all(100, operation())
        .await
        .expect("foreground resumes the provider mutation");
    let active = world.journal.entry(host).await;
    assert!(!active.registrations[0].pending_sync);
    assert!(active.retired_token_aliases.is_empty());
    assert!(!world.secrets.contains(&old_alias).await);
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
async fn remote_ack_ahead_is_persisted_as_divergence_then_repaired_to_exact_cursor() {
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
        .commit_enrollment(&host, operation())
        .await
        .expect("commit");
    let active = world.journal.entry(&host.0).await;
    let read_alias = active.read_capability_alias.clone();
    let manage_alias = active.manage_capability_alias.clone();

    relay
        .rollback_enrollment(&host, operation())
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
        .rollback_enrollment(&host, operation())
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
        .commit_enrollment(&host, operation())
        .await
        .expect("authenticated replacement commit");
    let active = world.journal.entry(&host.0).await;
    assert_eq!(active.state, RelayBindingState::Active);
    assert_eq!(active.installation_id.0, new_installation);
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
        .stage_enrollment(enrollment(&host.0, new_installation), operation())
        .await
        .expect("new authenticated enrollment supersedes rejected authority");
    relay
        .commit_enrollment(&host, operation())
        .await
        .expect("commit replacement");
    let active = world.journal.entry(&host.0).await;
    assert_eq!(active.state, RelayBindingState::Active);
    assert_eq!(active.installation_id.0, new_installation);
    assert!(!world.secrets.contains(&old_read).await);
    assert!(!world.secrets.contains(&old_manage).await);
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
        world.relay().rollback_enrollment(&host, operation()).await,
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
        .commit_enrollment(&host, operation())
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
        .rollback_enrollment(&valid_host, operation())
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
            .rollback_enrollment(&rejected_host, operation())
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
    missing
        .secrets
        .delete(&manage_alias)
        .await
        .expect("remove old manage capability");
    assert_eq!(
        missing
            .relay()
            .rollback_enrollment(&missing_host, operation())
            .await,
        Err(RelayError::SecureStorageUnavailable)
    );
    assert_eq!(
        missing.journal.entry(&missing_host.0).await.state,
        RelayBindingState::NeedsRepair
    );
}

#[tokio::test]
async fn enrollment_secret_failure_leaves_a_durable_staged_transaction_for_retry() {
    let world = TestWorld::new();
    let host = RelayHostId("staged-enrollment-host".to_owned());
    let installation = "installation_staged_retry_0001";
    let relay = world.relay();
    world.secrets.fail_next_write().await;

    assert_eq!(
        relay
            .stage_enrollment(enrollment(&host.0, installation), operation())
            .await,
        Err(RelayError::SecureStorageUnavailable)
    );
    let staged = world.journal.entry(&host.0).await;
    assert_eq!(staged.state, RelayBindingState::Staged);
    assert!(!world.secrets.contains(&staged.read_capability_alias).await);
    assert!(
        !world
            .secrets
            .contains(&staged.manage_capability_alias)
            .await
    );

    relay
        .stage_enrollment(enrollment(&host.0, installation), operation())
        .await
        .expect("retry fills the reserved aliases");
    relay
        .commit_enrollment(&host, operation())
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
async fn interrupted_staged_enrollment_must_be_restaged_before_rollback() {
    let world = TestWorld::new();
    let host = RelayHostId("staged-rollback-host".to_owned());
    let installation = "installation_staged_rollback_0001";
    let relay = world.relay();
    world.secrets.fail_next_write().await;
    assert_eq!(
        relay
            .stage_enrollment(enrollment(&host.0, installation), operation())
            .await,
        Err(RelayError::SecureStorageUnavailable)
    );

    assert_eq!(
        relay.rollback_enrollment(&host, operation()).await,
        Err(RelayError::SecureStorageUnavailable)
    );
    assert_eq!(
        world.journal.entry(&host.0).await.state,
        RelayBindingState::Staged
    );

    relay
        .stage_enrollment(enrollment(&host.0, installation), operation())
        .await
        .expect("restage fills the reserved capability aliases");
    relay
        .rollback_enrollment(&host, operation())
        .await
        .expect("rollback proceeds after restage");
    assert_eq!(
        world.journal.entry(&host.0).await.state,
        RelayBindingState::Tombstoned
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
    *world.secrets.write_delay.lock().await = Some(Duration::from_millis(50));
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
        RelayBindingState::Staged
    );

    *world.secrets.write_delay.lock().await = None;
    relay
        .stage_enrollment(enrollment(&host.0, installation), operation())
        .await
        .expect("resume reserved enrollment");
    *world.journal.load_delay.lock().await = Some(Duration::from_millis(50));
    assert_eq!(
        relay
            .commit_enrollment(
                &host,
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
        .commit_enrollment(&host, operation())
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
        world.secrets.fail_next_delete(failing_alias).await;

        assert_eq!(
            world
                .relay()
                .rollback_enrollment(&RelayHostId(host_name.clone()), operation())
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
    let retired_alias = pending.retired_token_aliases[0].clone();
    assert!(pending.registrations[0].pending_sync);

    world
        .relay()
        .rollback_enrollment(&RelayHostId(host.to_owned()), operation())
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
        world.relay().rollback_enrollment(&host, operation()).await,
        Err(RelayError::DeadlineExceeded)
    );
    assert_eq!(
        world.journal.entry(&host.0).await.state,
        RelayBindingState::TombstonePending
    );
    world
        .relay()
        .rollback_enrollment(&host, operation())
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
