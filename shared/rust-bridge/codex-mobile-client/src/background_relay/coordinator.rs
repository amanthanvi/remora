use std::{future::Future, sync::Arc, time::Duration};

use async_trait::async_trait;
use futures::{StreamExt, stream::FuturesUnordered};
use tokio::sync::Mutex;

use super::{ports::*, types::*};

const RECENT_WAKE_LIMIT: usize = 32;

struct CursorFetchWindow {
    starting_cursor: u64,
    target_cursor: u64,
    now_ms: u64,
}

/// Deep Rust module owning every cross-platform relay invariant.
pub(crate) struct BackgroundRelay {
    journal: Arc<dyn RelayBindingJournalPort>,
    secrets: Arc<dyn OpaqueRelaySecretPort>,
    transport: Arc<dyn RelayTransportPort>,
    repair: Arc<dyn RelayAuthoritativeRepairPort>,
    request_timeout: Duration,
    max_fetch_pages: usize,
    operations: Mutex<()>,
}

impl BackgroundRelay {
    pub(crate) fn new(
        journal: Arc<dyn RelayBindingJournalPort>,
        secrets: Arc<dyn OpaqueRelaySecretPort>,
        transport: Arc<dyn RelayTransportPort>,
        repair: Arc<dyn RelayAuthoritativeRepairPort>,
    ) -> Self {
        Self {
            journal,
            secrets,
            transport,
            repair,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            max_fetch_pages: DEFAULT_MAX_FETCH_PAGES,
            operations: Mutex::new(()),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_limits(mut self, request_timeout: Duration, max_fetch_pages: usize) -> Self {
        self.request_timeout = request_timeout;
        self.max_fetch_pages = max_fetch_pages;
        self
    }

    pub(crate) async fn observe_push_token(
        &self,
        observation: PushTokenObservation,
        operation: RelayOperationContext,
    ) -> Result<PushFanoutReceipt, RelayError> {
        observation.validate()?;
        operation.remaining()?;
        let _operation = self.run_step(&operation, self.operations.lock()).await?;
        let bindings: Vec<_> = self
            .list_bindings(&operation)
            .await?
            .into_iter()
            .filter(|binding| binding.state == RelayBindingState::Active)
            .collect();
        let mut receipt = PushFanoutReceipt::empty();
        receipt.attempted = u32::try_from(bindings.len()).unwrap_or(u32::MAX);
        let mut pending = FuturesUnordered::new();
        for binding in bindings {
            let installation_id = binding.installation_id.clone();
            let observation = observation.clone();
            let operation = operation.clone();
            pending.push(async move {
                let result = self
                    .sync_token_for_binding(binding, &observation, operation.clone())
                    .await;
                (installation_id, operation, result)
            });
        }
        while let Some((installation_id, child_operation, result)) = pending.next().await {
            match result {
                Ok(()) => receipt.synchronized = receipt.synchronized.saturating_add(1),
                Err(
                    RelayError::Retryable
                    | RelayError::DeadlineExceeded
                    | RelayError::Cancelled
                    | RelayError::JournalUnavailable
                    | RelayError::SecureStorageUnavailable,
                ) => receipt.pending_retry = receipt.pending_retry.saturating_add(1),
                Err(error @ (RelayError::RePairRequired | RelayError::Tombstoned)) => {
                    self.persist_binding_failure(&installation_id, error, &child_operation)
                        .await;
                    receipt.re_pair_required = receipt.re_pair_required.saturating_add(1)
                }
                Err(_) => receipt.rejected = receipt.rejected.saturating_add(1),
            }
        }
        Ok(receipt)
    }

    pub(crate) async fn tombstone_push_token(
        &self,
        tombstone: PushTokenTombstone,
        operation: RelayOperationContext,
    ) -> Result<PushFanoutReceipt, RelayError> {
        if tombstone.through_local_generation == 0
            || (tombstone.provider == RelayPushProvider::Fcm
                && tombstone.environment != RelayPushEnvironment::Production)
        {
            return Err(RelayError::InvalidProviderRegistration);
        }
        operation.remaining()?;
        let _operation = self.run_step(&operation, self.operations.lock()).await?;
        let bindings: Vec<_> = self
            .list_bindings(&operation)
            .await?
            .into_iter()
            .filter(|binding| binding.state == RelayBindingState::Active)
            .filter(|binding| {
                binding.registrations.iter().any(|registration| {
                    registration.provider == tombstone.provider
                        && registration.environment == tombstone.environment
                        && registration.local_generation <= tombstone.through_local_generation
                })
            })
            .collect();
        let mut receipt = PushFanoutReceipt::empty();
        receipt.attempted = u32::try_from(bindings.len()).unwrap_or(u32::MAX);
        let mut pending = FuturesUnordered::new();
        for binding in bindings {
            let installation_id = binding.installation_id.clone();
            let tombstone = tombstone.clone();
            let operation = operation.clone();
            pending.push(async move {
                let result = self
                    .tombstone_token_for_binding(binding, &tombstone, operation.clone())
                    .await;
                (installation_id, operation, result)
            });
        }
        while let Some((installation_id, child_operation, result)) = pending.next().await {
            match result {
                Ok(()) => receipt.synchronized = receipt.synchronized.saturating_add(1),
                Err(
                    RelayError::Retryable
                    | RelayError::DeadlineExceeded
                    | RelayError::Cancelled
                    | RelayError::JournalUnavailable
                    | RelayError::SecureStorageUnavailable,
                ) => receipt.pending_retry = receipt.pending_retry.saturating_add(1),
                Err(error @ (RelayError::RePairRequired | RelayError::Tombstoned)) => {
                    self.persist_binding_failure(&installation_id, error, &child_operation)
                        .await;
                    receipt.re_pair_required = receipt.re_pair_required.saturating_add(1)
                }
                Err(_) => receipt.rejected = receipt.rejected.saturating_add(1),
            }
        }
        Ok(receipt)
    }

    pub(crate) async fn ingest_wake(
        &self,
        hint: OpaqueWakeHint,
        now_ms: u64,
        operation: RelayOperationContext,
    ) -> Result<RelayReconcileReceipt, RelayError> {
        hint.validate(now_ms)?;
        operation.remaining()?;
        let _operation = self.run_step(&operation, self.operations.lock()).await?;
        let binding = self
            .run_step(
                &operation,
                self.journal.load_by_installation(&hint.installation_id),
            )
            .await?
            .map_err(|_| RelayError::JournalUnavailable)?
            .ok_or(RelayError::UnknownInstallation)?;
        if binding.state != RelayBindingState::Active {
            return Err(RelayError::Tombstoned);
        }
        let binding = self.persist_wake(binding, &hint, &operation).await?;
        match self
            .reconcile_binding(binding, false, now_ms, operation.clone())
            .await
        {
            Ok(receipt) => Ok(receipt),
            Err(error) => {
                self.persist_binding_failure(&hint.installation_id, error, &operation)
                    .await;
                Err(error)
            }
        }
    }

    /// Correctness path used on every foreground even if all pushes were lost.
    pub(crate) async fn reconcile_all(
        &self,
        now_ms: u64,
        operation: RelayOperationContext,
    ) -> Result<Vec<RelayReconcileOutcome>, RelayError> {
        operation.remaining()?;
        let _operation = self.run_step(&operation, self.operations.lock()).await?;
        let bindings = self.list_bindings(&operation).await?;
        let mut pending = FuturesUnordered::new();
        for (index, binding) in bindings.into_iter().enumerate() {
            if !matches!(
                binding.state,
                RelayBindingState::Active
                    | RelayBindingState::TombstonePending
                    | RelayBindingState::CleanupPending
            ) {
                continue;
            }
            let operation = operation.clone();
            pending.push(async move {
                let host_id = binding.host_id.clone();
                let installation_id = binding.installation_id.clone();
                let outcome = if matches!(
                    binding.state,
                    RelayBindingState::TombstonePending | RelayBindingState::CleanupPending
                ) {
                    match self.resume_tombstone(binding, &operation).await {
                        Ok(()) => RelayReconcileOutcome::CleanupCompleted { host_id },
                        Err(error) => RelayReconcileOutcome::Failed { host_id, error },
                    }
                } else {
                    match self
                        .resume_pending_provider_mutations(binding, &operation)
                        .await
                    {
                        Ok(binding) => match self
                            .reconcile_binding(binding, true, now_ms, operation.clone())
                            .await
                        {
                            Ok(receipt) => RelayReconcileOutcome::Applied(receipt),
                            Err(error) => {
                                self.persist_binding_failure(&installation_id, error, &operation)
                                    .await;
                                RelayReconcileOutcome::Failed { host_id, error }
                            }
                        },
                        Err(error) => {
                            self.persist_binding_failure(&installation_id, error, &operation)
                                .await;
                            RelayReconcileOutcome::Failed { host_id, error }
                        }
                    }
                };
                (index, outcome)
            });
        }
        let mut receipts = Vec::new();
        while let Some(outcome) = pending.next().await {
            receipts.push(outcome);
        }
        receipts.sort_by_key(|(index, _)| *index);
        Ok(receipts.into_iter().map(|(_, outcome)| outcome).collect())
    }

    async fn sync_token_for_binding(
        &self,
        mut binding: RelayBindingEntry,
        observation: &PushTokenObservation,
        operation: RelayOperationContext,
    ) -> Result<(), RelayError> {
        binding = self
            .cleanup_retired_token_aliases(binding, &operation)
            .await?;
        let existing_index = registration_index(
            &binding.registrations,
            observation.provider,
            observation.environment,
        );
        if let Some(index) = existing_index {
            let existing = &binding.registrations[index];
            if existing.local_generation > observation.local_generation {
                return Err(RelayError::InvalidProviderRegistration);
            }
            if existing.local_generation == observation.local_generation
                && existing.disposition == RelayRegistrationDisposition::Active
                && !existing.pending_sync
            {
                return Ok(());
            }
        }

        let old_alias =
            existing_index.map(|index| binding.registrations[index].token_alias.clone());
        let token_alias = if existing_index.is_some_and(|index| {
            binding.registrations[index].local_generation == observation.local_generation
                && binding.registrations[index].disposition == RelayRegistrationDisposition::Active
        }) {
            old_alias
                .clone()
                .expect("existing registration has an alias")
        } else {
            RelaySecretAlias(format!("relay_token_{}", uuid::Uuid::new_v4().simple()))
        };
        let pending = RelayProviderRegistration {
            provider: observation.provider,
            environment: observation.environment,
            local_generation: observation.local_generation,
            token_alias: token_alias.clone(),
            relay_registration_id: existing_index
                .and_then(|index| binding.registrations[index].relay_registration_id.clone()),
            relay_generation: existing_index
                .and_then(|index| binding.registrations[index].relay_generation),
            disposition: RelayRegistrationDisposition::Active,
            pending_sync: true,
        };
        replace_registration(&mut binding.registrations, pending);
        if let Some(old_alias) = old_alias.as_ref().filter(|alias| *alias != &token_alias)
            && !binding.retired_token_aliases.contains(old_alias)
        {
            binding.retired_token_aliases.push(old_alias.clone());
        }
        // Reserve both the active and retired aliases before writing the new
        // secret. A lost CAS response is recoverable by reloading this entry;
        // a failed CAS cannot leave an unreferenced provider token.
        binding = self.cas_bounded(binding, &operation, |entry| entry).await?;
        self.run_step(
            &operation,
            self.secrets.write(&token_alias, observation.token.clone()),
        )
        .await?
        .map_err(|_| RelayError::SecureStorageUnavailable)?;

        let authorization = self
            .read_capability(&binding.manage_capability_alias, &operation)
            .await?;
        let token = self.read_secret(&token_alias, &operation).await?;
        let response = self
            .run_transport(
                &operation,
                self.transport.register_device(
                    transport_context(&binding, authorization, operation.clone()),
                    RelayRegisterDeviceRequest {
                        installation_id: binding.installation_id.clone(),
                        provider: observation.provider,
                        environment: observation.environment,
                        token,
                    },
                ),
            )
            .await?;
        validate_registration_receipt(
            &binding,
            observation.provider,
            observation.environment,
            &response,
        )?;

        let index = registration_index(
            &binding.registrations,
            observation.provider,
            observation.environment,
        )
        .ok_or(RelayError::InvalidResponse)?;
        binding.registrations[index].relay_registration_id = Some(response.registration_id);
        binding.registrations[index].relay_generation = Some(response.generation);
        binding.registrations[index].pending_sync = false;
        binding = self.cas_bounded(binding, &operation, |entry| entry).await?;
        let _ = self
            .cleanup_retired_token_aliases(binding, &operation)
            .await?;
        Ok(())
    }

    async fn tombstone_token_for_binding(
        &self,
        mut binding: RelayBindingEntry,
        tombstone: &PushTokenTombstone,
        operation: RelayOperationContext,
    ) -> Result<(), RelayError> {
        binding = self
            .cleanup_retired_token_aliases(binding, &operation)
            .await?;
        let index = registration_index(
            &binding.registrations,
            tombstone.provider,
            tombstone.environment,
        )
        .ok_or(RelayError::InvalidProviderRegistration)?;
        let registration_id = binding.registrations[index]
            .relay_registration_id
            .clone()
            .ok_or(RelayError::Retryable)?;
        let relay_generation = binding.registrations[index]
            .relay_generation
            .ok_or(RelayError::Retryable)?;
        binding.registrations[index].disposition = RelayRegistrationDisposition::Tombstone;
        binding.registrations[index].pending_sync = true;
        binding = self.cas_bounded(binding, &operation, |entry| entry).await?;

        let authorization = self
            .read_capability(&binding.manage_capability_alias, &operation)
            .await?;
        self.run_idempotent_tombstone(
            &operation,
            self.transport.tombstone_device(
                transport_context(&binding, authorization, operation.clone()),
                RelayTombstoneDeviceRequest {
                    installation_id: binding.installation_id.clone(),
                    registration_id,
                    through_generation: relay_generation,
                },
            ),
        )
        .await?;

        let alias = binding.registrations[index].token_alias.clone();
        self.run_step(&operation, self.secrets.delete(&alias))
            .await?
            .map_err(|_| RelayError::SecureStorageUnavailable)?;
        binding.registrations.remove(index);
        let _ = self.cas_bounded(binding, &operation, |entry| entry).await?;
        Ok(())
    }

    async fn persist_wake(
        &self,
        mut binding: RelayBindingEntry,
        hint: &OpaqueWakeHint,
        operation: &RelayOperationContext,
    ) -> Result<RelayBindingEntry, RelayError> {
        if let Some(seen) = binding
            .wake
            .recently_seen
            .iter()
            .find(|seen| seen.event_id == hint.event_id)
        {
            if seen.cursor != hint.cursor {
                return Err(RelayError::InvalidWake);
            }
            return Ok(binding);
        }
        if binding
            .wake
            .recently_seen
            .iter()
            .any(|seen| seen.cursor == hint.cursor)
        {
            return Err(RelayError::InvalidWake);
        }
        if hint.cursor <= binding.wake.applied_cursor
            || hint.cursor <= binding.wake.highest_seen_cursor
        {
            return Ok(binding);
        }
        binding.wake.highest_seen_cursor = hint.cursor;
        binding.wake.recently_seen.push(SeenWake {
            cursor: hint.cursor,
            event_id: hint.event_id.clone(),
        });
        binding
            .wake
            .recently_seen
            .sort_by_key(|seen| std::cmp::Reverse(seen.cursor));
        binding.wake.recently_seen.truncate(RECENT_WAKE_LIMIT);
        self.cas_bounded(binding, operation, |entry| entry).await
    }

    async fn reconcile_binding(
        &self,
        mut binding: RelayBindingEntry,
        discover_high_watermark: bool,
        now_ms: u64,
        operation: RelayOperationContext,
    ) -> Result<RelayReconcileReceipt, RelayError> {
        if let Some(pending_ack) = binding.wake.pending_ack_cursor {
            binding = self
                .acknowledge_applied(binding, pending_ack, operation.clone())
                .await?;
        }
        let starting_cursor = binding.wake.applied_cursor;
        let mut target_cursor = binding.wake.highest_seen_cursor;
        let authorization = self
            .read_capability(&binding.read_capability_alias, &operation)
            .await?;

        let first_page = if discover_high_watermark || target_cursor > starting_cursor {
            Some(
                self.fetch_page(
                    &binding,
                    authorization.clone(),
                    starting_cursor,
                    operation.clone(),
                )
                .await?,
            )
        } else {
            None
        };
        if let Some(page) = first_page.as_ref() {
            target_cursor = target_cursor.max(page.high_watermark);
            validate_event_page(
                page,
                starting_cursor,
                target_cursor,
                operation.max_response_bytes,
                operation.page_limit,
                now_ms,
            )?;
        }

        if target_cursor <= starting_cursor {
            if discover_high_watermark {
                let repair = self
                    .run_repair(
                        &operation,
                        &binding.host_id,
                        RelayRepairMode::Full {
                            through_cursor: starting_cursor,
                        },
                    )
                    .await?;
                if !repair.authoritative || repair.applied_through_cursor != starting_cursor {
                    return Err(RelayError::RepairFailed);
                }
            }
            return Ok(RelayReconcileReceipt {
                host_id: binding.host_id,
                applied_through_cursor: starting_cursor,
                acknowledged_through_cursor: starting_cursor,
                changed: false,
            });
        }

        let repair_mode = match first_page {
            Some(page) if page.reset_required => {
                if page.snapshot_available {
                    let snapshot = self
                        .fetch_snapshot(&binding, authorization, now_ms, operation.clone())
                        .await?;
                    if let Some(snapshot) = snapshot {
                        if snapshot.through_cursor == target_cursor {
                            RelayRepairMode::Snapshot {
                                revision: snapshot.revision,
                                through_cursor: target_cursor,
                            }
                        } else {
                            RelayRepairMode::Full {
                                through_cursor: target_cursor,
                            }
                        }
                    } else {
                        RelayRepairMode::Full {
                            through_cursor: target_cursor,
                        }
                    }
                } else {
                    RelayRepairMode::Full {
                        through_cursor: target_cursor,
                    }
                }
            }
            Some(first_page) => {
                let pages_cover_target = self
                    .validate_contiguous_pages(
                        &binding,
                        authorization,
                        first_page,
                        CursorFetchWindow {
                            starting_cursor,
                            target_cursor,
                            now_ms,
                        },
                        operation.clone(),
                    )
                    .await?;
                if pages_cover_target {
                    RelayRepairMode::Incremental {
                        after_cursor: starting_cursor,
                        through_cursor: target_cursor,
                    }
                } else {
                    RelayRepairMode::Full {
                        through_cursor: target_cursor,
                    }
                }
            }
            None => unreachable!("target cursor requires a fetch"),
        };

        let repair = self
            .run_repair(&operation, &binding.host_id, repair_mode)
            .await?;
        if !repair.authoritative || repair.applied_through_cursor != target_cursor {
            return Err(RelayError::RepairFailed);
        }

        // This ordering is the central correctness invariant: authoritative
        // repair, then durable local cursor commit, then remote ACK.
        binding.wake.applied_cursor = target_cursor;
        binding.wake.pending_ack_cursor = Some(target_cursor);
        binding = self.cas_bounded(binding, &operation, |entry| entry).await?;
        binding = self
            .acknowledge_applied(binding, target_cursor, operation.clone())
            .await?;
        if binding.wake.remote_ack_ahead_cursor.is_some() {
            return Box::pin(self.reconcile_binding(binding, false, now_ms, operation)).await;
        }
        Ok(RelayReconcileReceipt {
            host_id: binding.host_id,
            applied_through_cursor: target_cursor,
            acknowledged_through_cursor: target_cursor,
            changed: target_cursor > starting_cursor,
        })
    }

    async fn validate_contiguous_pages(
        &self,
        binding: &RelayBindingEntry,
        authorization: OpaqueRelaySecret,
        first_page: RelayEventPage,
        window: CursorFetchWindow,
        operation: RelayOperationContext,
    ) -> Result<bool, RelayError> {
        let mut page = first_page;
        let mut cursor = window.starting_cursor;
        let mut seen_event_ids = std::collections::HashSet::new();
        for page_index in 0..self.max_fetch_pages {
            validate_event_page(
                &page,
                cursor,
                window.target_cursor,
                operation.max_response_bytes,
                operation.page_limit,
                window.now_ms,
            )?;
            if page.reset_required {
                return Err(RelayError::InvalidResponse);
            }
            if page
                .events
                .iter()
                .any(|event| !seen_event_ids.insert(event.event_id.clone()))
            {
                return Err(RelayError::InvalidResponse);
            }
            cursor = page.next_cursor;
            if cursor >= window.target_cursor {
                return Ok(true);
            }
            if page_index + 1 == self.max_fetch_pages {
                break;
            }
            page = self
                .fetch_page(binding, authorization.clone(), cursor, operation.clone())
                .await?;
        }
        // A valid backlog larger than the bounded fetch budget is not a
        // protocol failure. Use one authoritative full repair rather than
        // fetching without bound or permanently wedging the cursor.
        Ok(false)
    }

    async fn fetch_page(
        &self,
        binding: &RelayBindingEntry,
        authorization: OpaqueRelaySecret,
        after: u64,
        operation: RelayOperationContext,
    ) -> Result<RelayEventPage, RelayError> {
        let page = self
            .run_transport(
                &operation,
                self.transport.fetch_events(
                    transport_context(binding, authorization, operation.clone()),
                    RelayFetchEventsRequest {
                        installation_id: binding.installation_id.clone(),
                        after,
                        limit: operation.page_limit,
                    },
                ),
            )
            .await?;
        if page.encoded_bytes > operation.max_response_bytes {
            return Err(RelayError::PermanentFailure);
        }
        Ok(page)
    }

    async fn fetch_snapshot(
        &self,
        binding: &RelayBindingEntry,
        authorization: OpaqueRelaySecret,
        now_ms: u64,
        operation: RelayOperationContext,
    ) -> Result<Option<RelaySnapshotEnvelope>, RelayError> {
        if operation.follow_redirects {
            return Err(RelayError::PermanentFailure);
        }
        let snapshot = match self
            .run_request_step(
                &operation,
                self.transport.fetch_snapshot(
                    transport_context(binding, authorization, operation.clone()),
                    RelayFetchSnapshotRequest {
                        installation_id: binding.installation_id.clone(),
                    },
                ),
            )
            .await?
        {
            Ok(snapshot) => snapshot,
            // Snapshot availability is only a point-in-time hint. Absence is
            // a normal expiry race and falls back to authoritative full repair.
            Err(RelayTransportError::NotFound) => return Ok(None),
            Err(error) => return Err(map_transport_error(error)),
        };
        if snapshot.schema_version != RELAY_SCHEMA_VERSION
            || snapshot.revision == 0
            || snapshot.through_cursor == 0
            || snapshot.expires_at_ms <= now_ms
            || snapshot.encoded_bytes > operation.max_response_bytes
        {
            return Err(RelayError::InvalidResponse);
        }
        Ok(Some(snapshot))
    }

    async fn acknowledge_applied(
        &self,
        mut binding: RelayBindingEntry,
        through_cursor: u64,
        operation: RelayOperationContext,
    ) -> Result<RelayBindingEntry, RelayError> {
        let authorization = self
            .read_capability(&binding.read_capability_alias, &operation)
            .await?;
        let receipt = self
            .run_transport(
                &operation,
                self.transport.acknowledge(
                    transport_context(&binding, authorization, operation.clone()),
                    RelayAckRequest {
                        installation_id: binding.installation_id.clone(),
                        through_cursor,
                    },
                ),
            )
            .await?;
        if receipt.schema_version != RELAY_SCHEMA_VERSION
            || receipt.installation_id != binding.installation_id
            || receipt.acknowledged_through < through_cursor
        {
            return Err(RelayError::InvalidResponse);
        }
        if receipt.acknowledged_through > through_cursor {
            binding.wake.highest_seen_cursor = binding
                .wake
                .highest_seen_cursor
                .max(receipt.acknowledged_through);
            binding.wake.pending_ack_cursor = None;
            binding.wake.remote_ack_ahead_cursor = Some(receipt.acknowledged_through);
            return self.cas_bounded(binding, &operation, |entry| entry).await;
        }
        if binding
            .wake
            .pending_ack_cursor
            .is_some_and(|pending| pending <= receipt.acknowledged_through)
        {
            binding.wake.pending_ack_cursor = None;
            binding.wake.remote_ack_ahead_cursor = None;
            binding = self.cas_bounded(binding, &operation, |entry| entry).await?;
        }
        Ok(binding)
    }

    async fn run_transport<T>(
        &self,
        operation: &RelayOperationContext,
        future: impl Future<Output = Result<T, RelayTransportError>>,
    ) -> Result<T, RelayError> {
        if operation.follow_redirects {
            return Err(RelayError::PermanentFailure);
        }
        self.run_request_step(operation, future)
            .await?
            .map_err(map_transport_error)
    }

    async fn run_idempotent_tombstone(
        &self,
        operation: &RelayOperationContext,
        future: impl Future<Output = Result<(), RelayTransportError>>,
    ) -> Result<(), RelayError> {
        if operation.follow_redirects {
            return Err(RelayError::PermanentFailure);
        }
        match self.run_request_step(operation, future).await? {
            Ok(()) | Err(RelayTransportError::NotFound | RelayTransportError::Gone) => Ok(()),
            Err(error) => Err(map_transport_error(error)),
        }
    }

    async fn run_step<T>(
        &self,
        operation: &RelayOperationContext,
        future: impl Future<Output = T>,
    ) -> Result<T, RelayError> {
        let timeout = operation.remaining()?;
        tokio::select! {
            biased;
            _ = operation.cancellation.cancelled() => Err(RelayError::Cancelled),
            result = tokio::time::timeout(timeout, future) => {
                result.map_err(|_| RelayError::DeadlineExceeded)
            }
        }
    }

    async fn run_request_step<T>(
        &self,
        operation: &RelayOperationContext,
        future: impl Future<Output = T>,
    ) -> Result<T, RelayError> {
        let timeout = operation.request_timeout(self.request_timeout)?;
        tokio::select! {
            biased;
            _ = operation.cancellation.cancelled() => Err(RelayError::Cancelled),
            result = tokio::time::timeout(timeout, future) => {
                result.map_err(|_| RelayError::DeadlineExceeded)
            }
        }
    }

    async fn run_repair(
        &self,
        operation: &RelayOperationContext,
        host_id: &RelayHostId,
        mode: RelayRepairMode,
    ) -> Result<RelayRepairReceipt, RelayError> {
        match self
            .run_request_step(
                operation,
                self.repair.repair(host_id, mode, operation.clone()),
            )
            .await?
        {
            Ok(receipt) => Ok(receipt),
            Err(RelayRepairError::RePairRequired) => Err(RelayError::RePairRequired),
            Err(RelayRepairError::Cancelled) => Err(RelayError::Cancelled),
            Err(RelayRepairError::Unavailable) => Err(RelayError::Retryable),
        }
    }

    async fn read_capability(
        &self,
        alias: &RelaySecretAlias,
        operation: &RelayOperationContext,
    ) -> Result<OpaqueRelaySecret, RelayError> {
        let secret = self.read_secret(alias, operation).await?;
        secret.validate_capability()?;
        Ok(secret)
    }

    async fn read_secret(
        &self,
        alias: &RelaySecretAlias,
        operation: &RelayOperationContext,
    ) -> Result<OpaqueRelaySecret, RelayError> {
        self.run_step(operation, self.secrets.read(alias))
            .await?
            .map_err(|_| RelayError::SecureStorageUnavailable)?
            .ok_or(RelayError::SecureStorageUnavailable)
    }

    async fn list_bindings(
        &self,
        operation: &RelayOperationContext,
    ) -> Result<Vec<RelayBindingEntry>, RelayError> {
        self.run_step(operation, self.journal.list())
            .await?
            .map_err(|_| RelayError::JournalUnavailable)
    }

    async fn cas_bounded(
        &self,
        current: RelayBindingEntry,
        operation: &RelayOperationContext,
        transform: impl FnOnce(RelayBindingEntry) -> RelayBindingEntry,
    ) -> Result<RelayBindingEntry, RelayError> {
        let mut replacement = transform(current.clone());
        replacement.revision = current.revision.saturating_add(1);
        self.run_step(
            operation,
            self.journal.compare_and_swap(
                &current.host_id,
                Some(current.revision),
                replacement.clone(),
            ),
        )
        .await?
        .map_err(|_| RelayError::JournalUnavailable)?;
        Ok(replacement)
    }

    /// Retire obsolete local authority before an authenticated pairing flow
    /// replaces a `NeedsRepair` row. We try an idempotent remote tombstone when
    /// the old manage capability remains usable. Authorization rejection is
    /// the expected repair case: the new authenticated enrollment may proceed,
    /// but this path never claims that the obsolete remote installation was
    /// revoked. Retryable/ambiguous transport failures remain fail-closed.
    async fn retire_repair_binding_for_authenticated_replacement(
        &self,
        binding: &RelayBindingEntry,
        operation: &RelayOperationContext,
    ) -> Result<(), RelayError> {
        let manage_capability = self
            .run_step(
                operation,
                self.secrets.read(&binding.manage_capability_alias),
            )
            .await?
            .map_err(|_| RelayError::SecureStorageUnavailable)?;
        if let Some(manage_capability) = manage_capability {
            manage_capability.validate_capability()?;
            if operation.follow_redirects {
                return Err(RelayError::PermanentFailure);
            }
            match self
                .run_request_step(
                    operation,
                    self.transport.tombstone_installation(
                        transport_context(binding, manage_capability, operation.clone()),
                        RelayTombstoneInstallationRequest {
                            installation_id: binding.installation_id.clone(),
                        },
                    ),
                )
                .await?
            {
                Ok(())
                | Err(
                    RelayTransportError::NotFound
                    | RelayTransportError::Gone
                    | RelayTransportError::Unauthorized,
                ) => {}
                Err(error) => return Err(map_transport_error(error)),
            }
        }

        for registration in &binding.registrations {
            self.run_step(operation, self.secrets.delete(&registration.token_alias))
                .await?
                .map_err(|_| RelayError::SecureStorageUnavailable)?;
        }
        for alias in &binding.retired_token_aliases {
            self.run_step(operation, self.secrets.delete(alias))
                .await?
                .map_err(|_| RelayError::SecureStorageUnavailable)?;
        }
        self.run_step(
            operation,
            self.secrets.delete(&binding.read_capability_alias),
        )
        .await?
        .map_err(|_| RelayError::SecureStorageUnavailable)?;
        self.run_step(
            operation,
            self.secrets.delete(&binding.manage_capability_alias),
        )
        .await?
        .map_err(|_| RelayError::SecureStorageUnavailable)?;
        Ok(())
    }

    async fn persist_binding_failure(
        &self,
        installation_id: &RelayInstallationId,
        error: RelayError,
        operation: &RelayOperationContext,
    ) {
        let next_state = match error {
            RelayError::Tombstoned => RelayBindingState::CleanupPending,
            RelayError::RePairRequired
            | RelayError::InvalidResponse
            | RelayError::PermanentFailure
            | RelayError::RepairFailed => RelayBindingState::NeedsRepair,
            _ => return,
        };
        let Ok(Ok(Some(binding))) = self
            .run_step(
                operation,
                self.journal.load_by_installation(installation_id),
            )
            .await
        else {
            return;
        };
        if binding.state != RelayBindingState::Active {
            return;
        }
        let Ok(updated) = self
            .cas_bounded(binding, operation, |mut entry| {
                entry.state = next_state;
                entry
            })
            .await
        else {
            return;
        };
        if next_state == RelayBindingState::CleanupPending {
            let _ = self.cleanup_binding(updated, operation).await;
        }
    }
}

#[async_trait]
impl RemoteRelayEnrollmentPort for BackgroundRelay {
    async fn stage_enrollment(
        &self,
        enrollment: RelayEnrollment,
        operation: RelayOperationContext,
    ) -> Result<(), RelayError> {
        enrollment.read_capability.validate_capability()?;
        enrollment.manage_capability.validate_capability()?;
        operation.remaining()?;
        let _operation = self.run_step(&operation, self.operations.lock()).await?;
        let existing = self
            .run_step(&operation, self.journal.load_by_host(&enrollment.host_id))
            .await?
            .map_err(|_| RelayError::JournalUnavailable)?;
        if let Some(existing) = existing.as_ref() {
            if existing.installation_id == enrollment.installation_id
                && existing.origin == enrollment.origin
            {
                if existing.state == RelayBindingState::Active {
                    return Ok(());
                }
                if existing.state == RelayBindingState::Staged {
                    self.run_step(
                        &operation,
                        self.secrets
                            .write(&existing.read_capability_alias, enrollment.read_capability),
                    )
                    .await?
                    .map_err(|_| RelayError::SecureStorageUnavailable)?;
                    self.run_step(
                        &operation,
                        self.secrets.write(
                            &existing.manage_capability_alias,
                            enrollment.manage_capability,
                        ),
                    )
                    .await?
                    .map_err(|_| RelayError::SecureStorageUnavailable)?;
                    return Ok(());
                }
            }
            if !matches!(
                existing.state,
                RelayBindingState::Tombstoned | RelayBindingState::NeedsRepair
            ) {
                return Err(RelayError::InvalidResponse);
            }
            // This boundary has no relay-side generation/recreation contract.
            // A terminal remote installation ID is therefore never reused.
            if existing.installation_id == enrollment.installation_id {
                return Err(RelayError::InvalidResponse);
            }
        }
        if existing
            .as_ref()
            .is_none_or(|entry| entry.installation_id != enrollment.installation_id)
            && self
                .run_step(
                    &operation,
                    self.journal
                        .load_by_installation(&enrollment.installation_id),
                )
                .await?
                .map_err(|_| RelayError::JournalUnavailable)?
                .is_some()
        {
            return Err(RelayError::InvalidResponse);
        }

        let read_alias = RelaySecretAlias(format!(
            "relay_capability_{}",
            uuid::Uuid::new_v4().simple()
        ));
        let manage_alias = RelaySecretAlias(format!(
            "relay_capability_{}",
            uuid::Uuid::new_v4().simple()
        ));
        let entry = RelayBindingEntry {
            revision: existing
                .as_ref()
                .map_or(1, |entry| entry.revision.saturating_add(1)),
            host_id: enrollment.host_id.clone(),
            origin: enrollment.origin.clone(),
            installation_id: enrollment.installation_id.clone(),
            read_capability_alias: read_alias.clone(),
            manage_capability_alias: manage_alias.clone(),
            state: RelayBindingState::Staged,
            registrations: Vec::new(),
            retired_token_aliases: Vec::new(),
            wake: RelayWakeLedger::default(),
        };
        if let Some(existing) = existing {
            if existing.state == RelayBindingState::NeedsRepair {
                self.retire_repair_binding_for_authenticated_replacement(&existing, &operation)
                    .await?;
            }
            let _ = self.cas_bounded(existing, &operation, |_| entry).await?;
        } else {
            self.run_step(
                &operation,
                self.journal
                    .compare_and_swap(&enrollment.host_id, None, entry),
            )
            .await?
            .map_err(|_| RelayError::JournalUnavailable)?;
        }
        self.run_step(
            &operation,
            self.secrets.write(&read_alias, enrollment.read_capability),
        )
        .await?
        .map_err(|_| RelayError::SecureStorageUnavailable)?;
        self.run_step(
            &operation,
            self.secrets
                .write(&manage_alias, enrollment.manage_capability),
        )
        .await?
        .map_err(|_| RelayError::SecureStorageUnavailable)?;
        Ok(())
    }

    async fn commit_enrollment(
        &self,
        host_id: &RelayHostId,
        operation: RelayOperationContext,
    ) -> Result<(), RelayError> {
        operation.remaining()?;
        let _operation = self.run_step(&operation, self.operations.lock()).await?;
        let binding = self
            .run_step(&operation, self.journal.load_by_host(host_id))
            .await?
            .map_err(|_| RelayError::JournalUnavailable)?
            .ok_or(RelayError::UnknownInstallation)?;
        if binding.state == RelayBindingState::Active {
            return Ok(());
        }
        if binding.state != RelayBindingState::Staged {
            return Err(RelayError::InvalidResponse);
        }
        self.read_capability(&binding.read_capability_alias, &operation)
            .await?;
        self.read_capability(&binding.manage_capability_alias, &operation)
            .await?;
        let _ = self
            .cas_bounded(binding, &operation, |mut entry| {
                entry.state = RelayBindingState::Active;
                entry
            })
            .await?;
        Ok(())
    }

    async fn rollback_enrollment(
        &self,
        host_id: &RelayHostId,
        operation: RelayOperationContext,
    ) -> Result<(), RelayError> {
        operation.remaining()?;
        let _operation = self.run_step(&operation, self.operations.lock()).await?;
        let mut binding = self
            .run_step(&operation, self.journal.load_by_host(host_id))
            .await?
            .map_err(|_| RelayError::JournalUnavailable)?
            .ok_or(RelayError::UnknownInstallation)?;
        if binding.state == RelayBindingState::Tombstoned {
            return Ok(());
        }
        if binding.state == RelayBindingState::CleanupPending {
            return self.cleanup_binding(binding, &operation).await;
        }
        if binding.state == RelayBindingState::NeedsRepair {
            // Keep the quarantined row authoritative while attempting direct,
            // idempotent revocation. A rejected/missing capability or any
            // ambiguous failure leaves NeedsRepair unchanged; only confirmed
            // absence advances to local cleanup.
            let authorization = self
                .read_capability(&binding.manage_capability_alias, &operation)
                .await?;
            self.run_idempotent_tombstone(
                &operation,
                self.transport.tombstone_installation(
                    transport_context(&binding, authorization, operation.clone()),
                    RelayTombstoneInstallationRequest {
                        installation_id: binding.installation_id.clone(),
                    },
                ),
            )
            .await?;
            binding = self
                .cas_bounded(binding, &operation, |mut entry| {
                    entry.state = RelayBindingState::CleanupPending;
                    entry
                })
                .await?;
            return self.cleanup_binding(binding, &operation).await;
        }
        if binding.state == RelayBindingState::Staged {
            // Staging reserves the journal before secret writes. If staging was
            // interrupted, preserve the Staged transaction so the caller can
            // replay stage_enrollment with the same capabilities; entering a
            // tombstone state without the manage capability would be
            // unrecoverable and could abandon remote authority.
            self.read_capability(&binding.read_capability_alias, &operation)
                .await?;
            self.read_capability(&binding.manage_capability_alias, &operation)
                .await?;
        }
        if binding.state != RelayBindingState::TombstonePending {
            binding.state = RelayBindingState::TombstonePending;
            binding = self.cas_bounded(binding, &operation, |entry| entry).await?;
        }
        self.resume_tombstone(binding, &operation).await
    }
}

impl BackgroundRelay {
    async fn resume_tombstone(
        &self,
        mut binding: RelayBindingEntry,
        operation: &RelayOperationContext,
    ) -> Result<(), RelayError> {
        if binding.state == RelayBindingState::CleanupPending {
            return self.cleanup_binding(binding, operation).await;
        }
        if binding.state != RelayBindingState::TombstonePending {
            return Err(RelayError::InvalidResponse);
        }
        let authorization = self
            .read_capability(&binding.manage_capability_alias, operation)
            .await?;
        self.run_idempotent_tombstone(
            operation,
            self.transport.tombstone_installation(
                transport_context(&binding, authorization, operation.clone()),
                RelayTombstoneInstallationRequest {
                    installation_id: binding.installation_id.clone(),
                },
            ),
        )
        .await?;
        binding = self
            .cas_bounded(binding, operation, |mut entry| {
                entry.state = RelayBindingState::CleanupPending;
                entry
            })
            .await?;

        self.cleanup_binding(binding, operation).await
    }
}

impl BackgroundRelay {
    async fn resume_pending_provider_mutations(
        &self,
        mut binding: RelayBindingEntry,
        operation: &RelayOperationContext,
    ) -> Result<RelayBindingEntry, RelayError> {
        while let Some(index) = binding
            .registrations
            .iter()
            .position(|registration| registration.pending_sync)
        {
            let registration = binding.registrations[index].clone();
            let authorization = self
                .read_capability(&binding.manage_capability_alias, operation)
                .await?;
            match registration.disposition {
                RelayRegistrationDisposition::Active => {
                    let token = self
                        .read_secret(&registration.token_alias, operation)
                        .await?;
                    let receipt = self
                        .run_transport(
                            operation,
                            self.transport.register_device(
                                transport_context(&binding, authorization, operation.clone()),
                                RelayRegisterDeviceRequest {
                                    installation_id: binding.installation_id.clone(),
                                    provider: registration.provider,
                                    environment: registration.environment,
                                    token,
                                },
                            ),
                        )
                        .await?;
                    validate_registration_receipt(
                        &binding,
                        registration.provider,
                        registration.environment,
                        &receipt,
                    )?;
                    binding.registrations[index].relay_registration_id =
                        Some(receipt.registration_id);
                    binding.registrations[index].relay_generation = Some(receipt.generation);
                    binding.registrations[index].pending_sync = false;
                    binding = self.cas_bounded(binding, operation, |entry| entry).await?;
                }
                RelayRegistrationDisposition::Tombstone => {
                    let registration_id = registration
                        .relay_registration_id
                        .ok_or(RelayError::InvalidProviderRegistration)?;
                    let relay_generation = registration
                        .relay_generation
                        .ok_or(RelayError::InvalidProviderRegistration)?;
                    self.run_idempotent_tombstone(
                        operation,
                        self.transport.tombstone_device(
                            transport_context(&binding, authorization, operation.clone()),
                            RelayTombstoneDeviceRequest {
                                installation_id: binding.installation_id.clone(),
                                registration_id,
                                through_generation: relay_generation,
                            },
                        ),
                    )
                    .await?;
                    self.run_step(operation, self.secrets.delete(&registration.token_alias))
                        .await?
                        .map_err(|_| RelayError::SecureStorageUnavailable)?;
                    binding.registrations.remove(index);
                    binding = self.cas_bounded(binding, operation, |entry| entry).await?;
                }
            }
        }
        self.cleanup_retired_token_aliases(binding, operation).await
    }

    async fn cleanup_retired_token_aliases(
        &self,
        mut binding: RelayBindingEntry,
        operation: &RelayOperationContext,
    ) -> Result<RelayBindingEntry, RelayError> {
        // The retired alias remains authoritative until its replacement has
        // been acknowledged by the relay. Never delete rollback authority
        // while any provider registration is still staged remotely.
        if binding
            .registrations
            .iter()
            .any(|registration| registration.pending_sync)
        {
            return Ok(binding);
        }
        while let Some(alias) = binding.retired_token_aliases.first().cloned() {
            self.run_step(operation, self.secrets.delete(&alias))
                .await?
                .map_err(|_| RelayError::SecureStorageUnavailable)?;
            binding = self
                .cas_bounded(binding, operation, |mut entry| {
                    if let Some(index) = entry
                        .retired_token_aliases
                        .iter()
                        .position(|candidate| candidate == &alias)
                    {
                        entry.retired_token_aliases.remove(index);
                    }
                    entry
                })
                .await?;
        }
        Ok(binding)
    }

    async fn cleanup_binding(
        &self,
        mut binding: RelayBindingEntry,
        operation: &RelayOperationContext,
    ) -> Result<(), RelayError> {
        debug_assert_eq!(binding.state, RelayBindingState::CleanupPending);
        // Installation cleanup is terminal, so both staged/current and retired
        // provider aliases are deleted regardless of pending provider state.
        for alias in &binding.retired_token_aliases {
            self.run_step(operation, self.secrets.delete(alias))
                .await?
                .map_err(|_| RelayError::SecureStorageUnavailable)?;
        }
        for registration in &binding.registrations {
            self.run_step(operation, self.secrets.delete(&registration.token_alias))
                .await?
                .map_err(|_| RelayError::SecureStorageUnavailable)?;
        }
        self.run_step(
            operation,
            self.secrets.delete(&binding.read_capability_alias),
        )
        .await?
        .map_err(|_| RelayError::SecureStorageUnavailable)?;
        self.run_step(
            operation,
            self.secrets.delete(&binding.manage_capability_alias),
        )
        .await?
        .map_err(|_| RelayError::SecureStorageUnavailable)?;
        binding = self
            .cas_bounded(binding, operation, |mut entry| {
                entry.registrations.clear();
                entry.retired_token_aliases.clear();
                entry.state = RelayBindingState::Tombstoned;
                entry
            })
            .await?;
        debug_assert_eq!(binding.state, RelayBindingState::Tombstoned);
        Ok(())
    }
}

fn transport_context(
    binding: &RelayBindingEntry,
    authorization: OpaqueRelaySecret,
    operation: RelayOperationContext,
) -> RelayTransportContext {
    RelayTransportContext {
        origin: binding.origin.clone(),
        authorization,
        operation,
    }
}

fn registration_index(
    registrations: &[RelayProviderRegistration],
    provider: RelayPushProvider,
    environment: RelayPushEnvironment,
) -> Option<usize> {
    registrations.iter().position(|registration| {
        registration.provider == provider && registration.environment == environment
    })
}

fn replace_registration(
    registrations: &mut Vec<RelayProviderRegistration>,
    replacement: RelayProviderRegistration,
) {
    match registration_index(registrations, replacement.provider, replacement.environment) {
        Some(index) => registrations[index] = replacement,
        None => registrations.push(replacement),
    }
}

fn validate_registration_receipt(
    binding: &RelayBindingEntry,
    provider: RelayPushProvider,
    environment: RelayPushEnvironment,
    receipt: &RelayDeviceRegistrationReceipt,
) -> Result<(), RelayError> {
    if receipt.schema_version != RELAY_SCHEMA_VERSION
        || receipt.installation_id != binding.installation_id
        || receipt.provider != provider
        || receipt.environment != environment
        || receipt.generation == 0
    {
        return Err(RelayError::InvalidResponse);
    }
    Ok(())
}

fn validate_event_page(
    page: &RelayEventPage,
    requested_after: u64,
    target_cursor: u64,
    max_response_bytes: usize,
    page_limit: u32,
    now_ms: u64,
) -> Result<(), RelayError> {
    if page.schema_version != RELAY_SCHEMA_VERSION
        || page.requested_after != requested_after
        || page.encoded_bytes > max_response_bytes
        || page.events.len() > page_limit as usize
        || page.high_watermark < target_cursor
        || page.next_cursor > page.high_watermark
        || page.replay_floor == 0
        || page.replay_floor > page.high_watermark.saturating_add(1)
        || (requested_after.saturating_add(1) < page.replay_floor && !page.reset_required)
    {
        return Err(RelayError::InvalidResponse);
    }
    if page.reset_required {
        if !page.events.is_empty() {
            return Err(RelayError::InvalidResponse);
        }
        return Ok(());
    }
    let mut expected = requested_after.saturating_add(1);
    let mut seen_ids = std::collections::HashSet::new();
    for event in &page.events {
        if event.cursor != expected
            || event.expires_at_ms <= now_ms
            || !seen_ids.insert(&event.event_id)
        {
            return Err(RelayError::InvalidResponse);
        }
        expected = expected.saturating_add(1);
    }
    let actual_next = page
        .events
        .last()
        .map_or(requested_after, |event| event.cursor);
    if page.next_cursor != actual_next
        || (page.next_cursor < target_cursor && page.events.is_empty())
    {
        return Err(RelayError::InvalidResponse);
    }
    Ok(())
}
