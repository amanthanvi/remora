use std::{future::Future, sync::Arc, time::Duration};

use async_trait::async_trait;
use futures::{StreamExt, stream::FuturesUnordered};
use sha2::{Digest, Sha256};
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
        let global_fences = self
            .run_step(&operation, self.journal.provider_tombstone_fences())
            .await?
            .map_err(|_| RelayError::JournalUnavailable)?;
        if provider_tombstone_fence(
            &global_fences,
            observation.provider,
            observation.environment,
        )
        .is_some_and(|fence| observation.local_generation <= fence.through_local_generation)
        {
            return Err(RelayError::InvalidProviderRegistration);
        }
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
        self.run_step(
            &operation,
            self.journal.advance_provider_tombstone_fence(&tombstone),
        )
        .await?
        .map_err(|_| RelayError::JournalUnavailable)?;
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
                    | RelayBindingState::RollbackPending
                    | RelayBindingState::TombstonePending
                    | RelayBindingState::CleanupPending
            ) {
                continue;
            }
            let operation = operation.clone();
            pending.push(async move {
                let host_id = binding.host_id.clone();
                let installation_id = binding.installation_id.clone();
                let outcome = if binding.state == RelayBindingState::RollbackPending {
                    match self.resume_preparing_rollback(binding, &operation).await {
                        Ok(()) => RelayReconcileOutcome::CleanupCompleted { host_id },
                        Err(error) => RelayReconcileOutcome::Failed { host_id, error },
                    }
                } else if matches!(
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

    /// Return a bounded, secret-free view of configured relay bindings.
    pub(crate) async fn statuses(
        &self,
        operation: RelayOperationContext,
    ) -> Result<Vec<RelayBindingStatus>, RelayError> {
        operation.remaining()?;
        let _operation = self.run_step(&operation, self.operations.lock()).await?;
        let mut statuses = self
            .list_bindings(&operation)
            .await?
            .into_iter()
            .map(|binding| RelayBindingStatus {
                host_id: binding.host_id,
                installation_id: binding.installation_id,
                state: binding.state,
                highest_seen_cursor: binding.wake.highest_seen_cursor,
                applied_cursor: binding.wake.applied_cursor,
                pending_ack_cursor: binding.wake.pending_ack_cursor,
                provider_registration_count: u32::try_from(binding.registrations.len())
                    .unwrap_or(u32::MAX),
                has_pending_provider_sync: binding
                    .registrations
                    .iter()
                    .any(|registration| registration.pending_sync),
            })
            .collect::<Vec<_>>();
        statuses.sort_by(|left, right| left.host_id.0.cmp(&right.host_id.0));
        Ok(statuses)
    }

    async fn sync_token_for_binding(
        &self,
        mut binding: RelayBindingEntry,
        observation: &PushTokenObservation,
        operation: RelayOperationContext,
    ) -> Result<(), RelayError> {
        if provider_tombstone_fence(
            &binding.provider_tombstone_fences,
            observation.provider,
            observation.environment,
        )
        .is_some_and(|fence| observation.local_generation <= fence.through_local_generation)
        {
            return Err(RelayError::InvalidProviderRegistration);
        }
        let existing_index = registration_index(
            &binding.registrations,
            observation.provider,
            observation.environment,
        );
        // A newer local generation is allowed only after the older durable
        // tombstone has converged and removed its registration. Replacing this
        // row early would discard the remote cleanup intent.
        if existing_index.is_some_and(|index| {
            binding.registrations[index].disposition == RelayRegistrationDisposition::Tombstone
        }) {
            return Err(RelayError::Retryable);
        }
        let pending_retry = registration_index(
            &binding.registrations,
            observation.provider,
            observation.environment,
        )
        .is_some_and(|index| {
            let registration = &binding.registrations[index];
            registration.local_generation == observation.local_generation
                && registration.disposition == RelayRegistrationDisposition::Active
                && registration.pending_sync
        });
        if !pending_retry {
            // A new generation must retire the prior alias before it reserves
            // another one. The exact same pending generation is allowed to
            // rewrite its already-reserved token first, then retries cleanup
            // after remote synchronization.
            binding = self
                .cleanup_retired_token_aliases(binding, &operation)
                .await?;
        }
        if let Some(index) = existing_index {
            let existing = &binding.registrations[index];
            if existing.local_generation > observation.local_generation {
                return Err(RelayError::InvalidProviderRegistration);
            }
            if existing.pending_sync && existing.local_generation != observation.local_generation {
                return Err(RelayError::Retryable);
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
            token_revision: if old_alias.as_ref() == Some(&token_alias) {
                existing_index.and_then(|index| binding.registrations[index].token_revision)
            } else {
                None
            },
            relay_registration_id: if old_alias.as_ref() == Some(&token_alias) {
                existing_index
                    .and_then(|index| binding.registrations[index].relay_registration_id.clone())
            } else {
                None
            },
            relay_generation: if old_alias.as_ref() == Some(&token_alias) {
                existing_index.and_then(|index| binding.registrations[index].relay_generation)
            } else {
                None
            },
            disposition: RelayRegistrationDisposition::Active,
            pending_sync: true,
            previous: existing_index.and_then(|index| {
                let existing = &binding.registrations[index];
                if existing.token_alias == token_alias {
                    existing.previous.clone()
                } else {
                    Some(RelayPreviousProviderRegistration {
                        token_alias: existing.token_alias.clone(),
                        token_revision: existing.token_revision,
                        relay_registration_id: existing.relay_registration_id.clone(),
                        relay_generation: existing.relay_generation,
                    })
                }
            }),
        };
        replace_registration(&mut binding.registrations, pending);
        // Reserve the replacement alias and the preceding remote receipt
        // before writing the new secret. A failed or ambiguous write cannot
        // erase the last known generation needed by logout.
        binding = self.cas_bounded(binding, &operation, |entry| entry).await?;
        let token_revision = self
            .reserve_versioned_secret(&token_alias, observation.token.clone(), &operation)
            .await?;
        let index = registration_index(
            &binding.registrations,
            observation.provider,
            observation.environment,
        )
        .ok_or(RelayError::InvalidProviderRegistration)?;
        if binding.registrations[index].token_alias != token_alias
            || binding.registrations[index].local_generation != observation.local_generation
        {
            return Err(RelayError::InvalidProviderRegistration);
        }
        binding.registrations[index].token_revision = Some(token_revision);
        // Remote registration is forbidden until the exact custody revision
        // is durable in the journal. A crash before this CAS leaves a pending
        // alias that logout can still revision-tombstone safely.
        binding = self.cas_bounded(binding, &operation, |entry| entry).await?;

        let authorization = self
            .read_capability(
                &binding.manage_capability_alias,
                binding.manage_capability_revision,
                &operation,
            )
            .await?;
        let token = self
            .read_versioned_secret(&token_alias, token_revision, &operation)
            .await?;
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
        let existing_index = registration_index(
            &binding.registrations,
            tombstone.provider,
            tombstone.environment,
        );
        let fence_needs_advance = provider_tombstone_fence(
            &binding.provider_tombstone_fences,
            tombstone.provider,
            tombstone.environment,
        )
        .is_none_or(|fence| fence.through_local_generation < tombstone.through_local_generation);
        let registration_needs_tombstone = existing_index.is_some_and(|index| {
            let registration = &binding.registrations[index];
            registration.local_generation <= tombstone.through_local_generation
                && registration.disposition != RelayRegistrationDisposition::Tombstone
        });
        if fence_needs_advance || registration_needs_tombstone {
            binding = self
                .cas_bounded(binding, &operation, |mut entry| {
                    advance_provider_tombstone_fence(
                        &mut entry.provider_tombstone_fences,
                        tombstone,
                    );
                    if let Some(index) = registration_index(
                        &entry.registrations,
                        tombstone.provider,
                        tombstone.environment,
                    ) && entry.registrations[index].local_generation
                        <= tombstone.through_local_generation
                    {
                        entry.registrations[index].disposition =
                            RelayRegistrationDisposition::Tombstone;
                        entry.registrations[index].pending_sync = true;
                    }
                    entry
                })
                .await?;
        }

        let _ = self
            .resume_pending_provider_mutations(binding, &operation)
            .await?;
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
            .read_capability(
                &binding.read_capability_alias,
                binding.read_capability_revision,
                &operation,
            )
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
                let (repair, _) = self
                    .run_repair(
                        &operation,
                        binding.clone(),
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

        let (repair, repaired_binding) = self.run_repair(&operation, binding, repair_mode).await?;
        binding = repaired_binding;
        if !repair.authoritative || repair.applied_through_cursor != target_cursor {
            return Err(RelayError::RepairFailed);
        }

        // This ordering is the central correctness invariant: authoritative
        // repair, then durable local cursor commit, then remote ACK.
        binding.wake.highest_seen_cursor = binding.wake.highest_seen_cursor.max(target_cursor);
        binding.wake.applied_cursor = target_cursor;
        binding.wake.pending_ack_cursor = Some(target_cursor);
        // A remote-ahead ACK is only a divergence marker until authoritative
        // repair reaches that cursor. Clear it in the same durable commit that
        // advances `applied_cursor`; persisting equal applied/remote-ahead
        // cursors would make the authenticated journal fail closed before the
        // follow-up ACK could recover it.
        if binding
            .wake
            .remote_ack_ahead_cursor
            .is_some_and(|cursor| cursor <= target_cursor)
        {
            binding.wake.remote_ack_ahead_cursor = None;
        }
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
            .read_capability(
                &binding.read_capability_alias,
                binding.read_capability_revision,
                &operation,
            )
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
        mut binding: RelayBindingEntry,
        mode: RelayRepairMode,
    ) -> Result<(RelayRepairReceipt, RelayBindingEntry), RelayError> {
        // Allocate the native commit fence in the authenticated journal before
        // invoking native code. Retrying a journal conflict reloads the latest
        // per-host generation, so separate AppClient instances and process
        // restarts share one strictly monotonic sequence.
        let mut allocated = false;
        for _ in 0..4 {
            let generation = binding
                .repair_generation
                .checked_add(1)
                .ok_or(RelayError::JournalUnavailable)?;
            let mut replacement = binding.clone();
            replacement.revision = binding
                .revision
                .checked_add(1)
                .ok_or(RelayError::JournalUnavailable)?;
            replacement.repair_generation = generation;
            match self
                .run_step(
                    operation,
                    self.journal.compare_and_swap(
                        &binding.host_id,
                        Some(binding.revision),
                        replacement.clone(),
                    ),
                )
                .await?
            {
                Ok(()) => {
                    binding = replacement;
                    allocated = true;
                    break;
                }
                Err(RelayJournalError::Conflict) => {
                    binding = self
                        .run_step(operation, self.journal.load_by_host(&binding.host_id))
                        .await?
                        .map_err(|_| RelayError::JournalUnavailable)?
                        .ok_or(RelayError::UnknownInstallation)?;
                }
                Err(RelayJournalError::Unavailable) => {
                    return Err(RelayError::JournalUnavailable);
                }
            }
        }
        if !allocated || binding.repair_generation == 0 {
            return Err(RelayError::JournalUnavailable);
        }
        // The repair port owns deadline enforcement because native callbacks
        // may continue after their Rust future is dropped. Its native request
        // carries the same deadline plus a commit fence; wrapping it in a
        // second timeout here could drop the adapter before it establishes
        // that fence.
        match self
            .repair
            .repair(
                &binding.host_id,
                binding.repair_generation,
                mode,
                operation.clone(),
            )
            .await
        {
            Ok(receipt) => Ok((receipt, binding)),
            Err(RelayRepairError::RePairRequired) => Err(RelayError::RePairRequired),
            Err(RelayRepairError::Cancelled) => Err(RelayError::Cancelled),
            Err(RelayRepairError::DeadlineExceeded) => Err(RelayError::DeadlineExceeded),
            Err(RelayRepairError::Unavailable) => Err(RelayError::Retryable),
        }
    }

    async fn read_capability(
        &self,
        alias: &RelaySecretAlias,
        expected_revision: Option<u64>,
        operation: &RelayOperationContext,
    ) -> Result<OpaqueRelaySecret, RelayError> {
        let expected_revision = expected_revision.ok_or(RelayError::InvalidResponse)?;
        let secret = self
            .read_versioned_secret(alias, expected_revision, operation)
            .await?;
        secret.validate_capability()?;
        Ok(secret)
    }

    async fn read_versioned_secret(
        &self,
        alias: &RelaySecretAlias,
        expected_revision: u64,
        operation: &RelayOperationContext,
    ) -> Result<OpaqueRelaySecret, RelayError> {
        self.read_versioned_secret_optional(alias, Some(expected_revision), operation)
            .await?
            .ok_or(RelayError::SecureStorageUnavailable)
    }

    async fn read_versioned_secret_optional(
        &self,
        alias: &RelaySecretAlias,
        expected_revision: Option<u64>,
        operation: &RelayOperationContext,
    ) -> Result<Option<OpaqueRelaySecret>, RelayError> {
        let expected_revision = expected_revision.map(RelaySecretRevision::Found);
        if let Some(expected_revision) = expected_revision {
            let revision_before = self
                .run_step(operation, self.secrets.revision(alias))
                .await?
                .map_err(|_| RelayError::SecureStorageUnavailable)?;
            if revision_before != expected_revision {
                return Err(RelayError::SecureStorageUnavailable);
            }
        }
        let secret = self.read_secret_optional(alias, operation).await?;
        if let Some(expected_revision) = expected_revision {
            // The native secure-store surface exposes revision and value as
            // separate callbacks. Re-read the revision after copying the
            // opaque value so the expected revision is a linearization fence:
            // a concurrent CAS before, during, or immediately after the read
            // cannot make an unjournaled replacement eligible for transport.
            let revision_after = self
                .run_step(operation, self.secrets.revision(alias))
                .await?
                .map_err(|_| RelayError::SecureStorageUnavailable)?;
            if revision_after != expected_revision {
                return Err(RelayError::SecureStorageUnavailable);
            }
        }
        Ok(secret)
    }

    async fn read_secret_optional(
        &self,
        alias: &RelaySecretAlias,
        operation: &RelayOperationContext,
    ) -> Result<Option<OpaqueRelaySecret>, RelayError> {
        self.run_step(operation, self.secrets.read(alias))
            .await?
            .map_err(|_| RelayError::SecureStorageUnavailable)
    }

    async fn verify_staged_capability_revisions(
        &self,
        binding: &RelayBindingEntry,
        operation: &RelayOperationContext,
    ) -> Result<(), RelayError> {
        if !matches!(
            binding.state,
            RelayBindingState::Staged | RelayBindingState::Active
        ) {
            return Err(RelayError::InvalidResponse);
        }
        for (alias, expected) in [
            (
                &binding.read_capability_alias,
                binding.read_capability_revision,
            ),
            (
                &binding.manage_capability_alias,
                binding.manage_capability_revision,
            ),
        ] {
            let expected = expected.ok_or(RelayError::InvalidResponse)?;
            let actual = self
                .run_step(operation, self.secrets.revision(alias))
                .await?
                .map_err(|_| RelayError::SecureStorageUnavailable)?;
            if actual != RelaySecretRevision::Found(expected) {
                return Err(RelayError::SecureStorageUnavailable);
            }
        }
        Ok(())
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
        replacement.revision = current
            .revision
            .checked_add(1)
            .ok_or(RelayError::JournalUnavailable)?;
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
        let manage_capability_revision = binding
            .manage_capability_revision
            .ok_or(RelayError::InvalidResponse)?;
        let manage_capability = match self
            .read_versioned_secret_optional(
                &binding.manage_capability_alias,
                Some(manage_capability_revision),
                operation,
            )
            .await
        {
            Ok(capability) => capability,
            Err(error @ RelayError::SecureStorageUnavailable) => {
                let minimum_tombstone_revision = manage_capability_revision
                    .checked_add(1)
                    .ok_or(RelayError::SecureStorageUnavailable)?;
                if self
                    .capability_tombstone_is_authoritative(
                        &binding.manage_capability_alias,
                        minimum_tombstone_revision,
                        operation,
                    )
                    .await?
                {
                    // A prior authenticated replacement attempt may have
                    // completed local tombstoning before its journal CAS
                    // failed. A stable newer tombstone contains no bearer to
                    // send, so replay may proceed without weakening the exact
                    // revision check for any present capability.
                    None
                } else {
                    return Err(error);
                }
            }
            Err(error) => return Err(error),
        };
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
            if let Some(previous) = &registration.previous {
                self.tombstone_versioned_secret(&previous.token_alias, operation)
                    .await?;
            }
            self.tombstone_versioned_secret(&registration.token_alias, operation)
                .await?;
        }
        for alias in &binding.retired_token_aliases {
            self.tombstone_versioned_secret(alias, operation).await?;
        }
        self.tombstone_versioned_secret(&binding.read_capability_alias, operation)
            .await?;
        self.tombstone_versioned_secret(&binding.manage_capability_alias, operation)
            .await?;
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

    async fn reserve_versioned_secret(
        &self,
        alias: &RelaySecretAlias,
        capability: OpaqueRelaySecret,
        operation: &RelayOperationContext,
    ) -> Result<u64, RelayError> {
        for _ in 0..4 {
            let current_revision = self
                .run_step(operation, self.secrets.revision(alias))
                .await?
                .map_err(|_| RelayError::SecureStorageUnavailable)?;
            let (expected_revision, replacement_revision) = match current_revision {
                RelaySecretRevision::Missing => (None, 1),
                RelaySecretRevision::Found(revision) => {
                    // A missing value with a present revision is a rollback
                    // tombstone. It is intentionally reusable only through a
                    // newer CAS, so a late pre-rollback value write cannot
                    // resurrect the old capability.
                    let existing = self
                        .run_step(operation, self.secrets.read(alias))
                        .await?
                        .map_err(|_| RelayError::SecureStorageUnavailable)?;
                    // Always advance the persisted revision, even when the
                    // capability bytes are unchanged. An older queued CAS can
                    // otherwise still match this revision and overwrite a
                    // successfully retried enrollment after it resumes.
                    drop(existing);
                    (
                        Some(revision),
                        revision
                            .checked_add(1)
                            .ok_or(RelayError::SecureStorageUnavailable)?,
                    )
                }
            };
            let outcome = self
                .run_step(
                    operation,
                    self.secrets.compare_and_swap(
                        alias,
                        expected_revision,
                        replacement_revision,
                        capability.clone(),
                    ),
                )
                .await?
                .map_err(|_| RelayError::SecureStorageUnavailable)?;
            match outcome {
                RelaySecretCasOutcome::Stored => return Ok(replacement_revision),
                RelaySecretCasOutcome::Conflict => continue,
            }
        }
        Err(RelayError::Retryable)
    }
}

fn reserved_staging_aliases(
    host_id: &RelayHostId,
    _existing: Option<&RelayBindingEntry>,
) -> (RelaySecretAlias, RelaySecretAlias) {
    // The slot CAS revision is the supersession fence. Reusing one
    // deterministic pair avoids leaving an untracked alternate pair behind,
    // while a late older write can only win before the replacement advances
    // the same revision (and otherwise conflicts).
    staging_alias_pair(host_id, 0)
}

fn staging_alias_pair(host_id: &RelayHostId, slot: u8) -> (RelaySecretAlias, RelaySecretAlias) {
    (
        staging_alias(host_id, slot, b"read"),
        staging_alias(host_id, slot, b"manage"),
    )
}

fn staging_alias(host_id: &RelayHostId, slot: u8, role: &[u8]) -> RelaySecretAlias {
    let mut digest = Sha256::new();
    digest.update(b"remora-relay-staging-alias-v1\0");
    digest.update([slot]);
    digest.update(role);
    digest.update(b"\0");
    digest.update(host_id.0.as_bytes());
    let suffix = hex::encode(&digest.finalize()[..16]);
    RelaySecretAlias(format!("relay_capability_{suffix}"))
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
        let global_fences = self
            .run_step(&operation, self.journal.provider_tombstone_fences())
            .await?
            .map_err(|_| RelayError::JournalUnavailable)?;

        // Publish a non-secret Preparing reservation before touching either
        // deterministic slot. A late reservation CAS can only publish this
        // inert row. A newer authenticated command first advances the same
        // per-host generation, invalidating every older late publication CAS.
        let mut reservation = None;
        let mut retired_repair_generation = None;
        for _ in 0..4 {
            let existing = self
                .run_step(&operation, self.journal.load_by_host(&enrollment.host_id))
                .await?
                .map_err(|_| RelayError::JournalUnavailable)?;
            if let Some(existing) = existing.as_ref() {
                let same_command = enrollment_matches(existing, &enrollment);
                match existing.state {
                    RelayBindingState::Active => {
                        return if same_command {
                            Ok(())
                        } else {
                            Err(RelayError::InvalidResponse)
                        };
                    }
                    RelayBindingState::Staged if same_command => {
                        self.verify_staged_capability_revisions(existing, &operation)
                            .await?;
                        return Ok(());
                    }
                    RelayBindingState::Preparing if same_command => {
                        reservation = Some(existing.clone());
                        break;
                    }
                    RelayBindingState::Preparing | RelayBindingState::Staged => {
                        // A newer authenticated command is allowed to replace
                        // an uncommitted command, including the same remote
                        // installation. Its reservation lands before slots.
                    }
                    RelayBindingState::Tombstoned | RelayBindingState::NeedsRepair => {
                        if existing.installation_id == enrollment.installation_id {
                            return Err(RelayError::InvalidResponse);
                        }
                    }
                    RelayBindingState::RollbackPending
                    | RelayBindingState::TombstonePending
                    | RelayBindingState::CleanupPending => {
                        return Err(RelayError::InvalidResponse);
                    }
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
            if let Some(existing) = existing.as_ref()
                && existing.state == RelayBindingState::NeedsRepair
                && retired_repair_generation != Some(existing.revision)
            {
                self.retire_repair_binding_for_authenticated_replacement(existing, &operation)
                    .await?;
                retired_repair_generation = Some(existing.revision);
            }

            let (read_alias, manage_alias) =
                reserved_staging_aliases(&enrollment.host_id, existing.as_ref());
            let staging_generation = existing.as_ref().map_or(Ok(1), |entry| {
                entry
                    .staging_generation
                    .checked_add(1)
                    .ok_or(RelayError::JournalUnavailable)
            })?;
            let entry = RelayBindingEntry {
                revision: existing.as_ref().map_or(Ok(1), |entry| {
                    entry
                        .revision
                        .checked_add(1)
                        .ok_or(RelayError::JournalUnavailable)
                })?,
                staging_generation,
                staging_command_id: enrollment.command_id.clone(),
                read_capability_revision: None,
                manage_capability_revision: None,
                // Preserve the host fence across authenticated replacement so
                // a late repair for the retired installation cannot become current.
                repair_generation: existing.as_ref().map_or(0, |entry| entry.repair_generation),
                host_id: enrollment.host_id.clone(),
                origin: enrollment.origin.clone(),
                installation_id: enrollment.installation_id.clone(),
                read_capability_alias: read_alias,
                manage_capability_alias: manage_alias,
                state: RelayBindingState::Preparing,
                registrations: Vec::new(),
                provider_tombstone_fences: merged_provider_tombstone_fences(
                    existing
                        .as_ref()
                        .map(|entry| entry.provider_tombstone_fences.as_slice())
                        .unwrap_or_default(),
                    &global_fences,
                ),
                retired_token_aliases: Vec::new(),
                wake: RelayWakeLedger::default(),
            };
            let expected_revision = existing.as_ref().map(|entry| entry.revision);
            match self
                .run_step(
                    &operation,
                    self.journal
                        .compare_and_swap(&entry.host_id, expected_revision, entry.clone()),
                )
                .await?
            {
                Ok(()) => {
                    reservation = Some(entry);
                    break;
                }
                Err(RelayJournalError::Conflict) => continue,
                Err(RelayJournalError::Unavailable) => {
                    return Err(RelayError::JournalUnavailable);
                }
            }
        }
        let reservation = reservation.ok_or(RelayError::Retryable)?;

        let read_revision = self
            .reserve_versioned_secret(
                &reservation.read_capability_alias,
                enrollment.read_capability,
                &operation,
            )
            .await?;
        let manage_revision = self
            .reserve_versioned_secret(
                &reservation.manage_capability_alias,
                enrollment.manage_capability,
                &operation,
            )
            .await?;
        let mut staged = reservation.clone();
        staged.revision = reservation
            .revision
            .checked_add(1)
            .ok_or(RelayError::JournalUnavailable)?;
        staged.read_capability_revision = Some(read_revision);
        staged.manage_capability_revision = Some(manage_revision);
        staged.state = RelayBindingState::Staged;

        let publish_result = self
            .run_step(
                &operation,
                self.journal.compare_and_swap(
                    &staged.host_id,
                    Some(reservation.revision),
                    staged.clone(),
                ),
            )
            .await;
        match publish_result {
            Ok(Ok(())) => return Ok(()),
            Ok(Err(_)) => {}
            Err(error) => {
                // Timeout/cancellation drops the Rust future but cannot stop
                // an already-entered native CAS callback. An immediate reload
                // is not a fence: the callback may still commit afterward.
                // Retain the deterministic bounded aliases for replay.
                return Err(error);
            }
        }

        // A callback may apply its CAS and still report unavailable. Reload
        // under a fresh bounded recovery budget before deciding whether the
        // unpublished aliases are safe to delete.
        let recovery = RelayOperationContext::with_timeout(self.request_timeout);
        match self
            .run_step(&recovery, self.journal.load_by_host(&enrollment.host_id))
            .await
        {
            Ok(Ok(Some(published))) if published == staged => {
                self.verify_staged_capability_revisions(&published, &recovery)
                    .await
            }
            Ok(Ok(_)) => {
                // Retain the bounded fenced slots. A later enrollment with
                // different capabilities can CAS-replace them safely.
                Err(RelayError::JournalUnavailable)
            }
            Ok(Err(_)) | Err(_) => {
                // Ambiguous reload: retain the deterministic aliases. If the
                // CAS landed, Staged is recoverable; if not, a later attempt
                // reuses the same bounded slot.
                Err(RelayError::JournalUnavailable)
            }
        }
    }

    async fn commit_enrollment(
        &self,
        host_id: &RelayHostId,
        command_id: &RelayEnrollmentCommandId,
        operation: RelayOperationContext,
    ) -> Result<(), RelayError> {
        operation.remaining()?;
        let _operation = self.run_step(&operation, self.operations.lock()).await?;
        let binding = self
            .run_step(&operation, self.journal.load_by_host(host_id))
            .await?
            .map_err(|_| RelayError::JournalUnavailable)?
            .ok_or(RelayError::UnknownInstallation)?;
        if binding.staging_command_id != *command_id {
            return Err(RelayError::InvalidResponse);
        }
        if binding.state == RelayBindingState::Active {
            self.verify_staged_capability_revisions(&binding, &operation)
                .await?;
            return Ok(());
        }
        if binding.state != RelayBindingState::Staged {
            return Err(RelayError::InvalidResponse);
        }
        self.verify_staged_capability_revisions(&binding, &operation)
            .await?;
        self.read_capability(
            &binding.read_capability_alias,
            binding.read_capability_revision,
            &operation,
        )
        .await?;
        self.read_capability(
            &binding.manage_capability_alias,
            binding.manage_capability_revision,
            &operation,
        )
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
        command_id: &RelayEnrollmentCommandId,
        operation: RelayOperationContext,
    ) -> Result<(), RelayError> {
        operation.remaining()?;
        let _operation = self.run_step(&operation, self.operations.lock()).await?;
        let mut binding = self
            .run_step(&operation, self.journal.load_by_host(host_id))
            .await?
            .map_err(|_| RelayError::JournalUnavailable)?
            .ok_or(RelayError::UnknownInstallation)?;
        if binding.staging_command_id != *command_id {
            return Err(RelayError::InvalidResponse);
        }
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
                .read_capability(
                    &binding.manage_capability_alias,
                    binding.manage_capability_revision,
                    &operation,
                )
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
        if binding.state == RelayBindingState::Preparing {
            // Publish the terminal reservation before touching either slot.
            // Every late Staged publication still expects the Preparing
            // revision and therefore conflicts after this CAS commits.
            binding = self
                .cas_bounded(binding, &operation, |mut entry| {
                    entry.state = RelayBindingState::RollbackPending;
                    entry
                })
                .await?;
            return self.resume_preparing_rollback(binding, &operation).await;
        }
        if binding.state == RelayBindingState::RollbackPending {
            return self.resume_preparing_rollback(binding, &operation).await;
        }
        if binding.state == RelayBindingState::Staged {
            // Staged is published only after both capabilities are durably in
            // custody, so rollback can always authenticate remote revocation.
            self.verify_staged_capability_revisions(&binding, &operation)
                .await?;
            self.read_capability(
                &binding.read_capability_alias,
                binding.read_capability_revision,
                &operation,
            )
            .await?;
            self.read_capability(
                &binding.manage_capability_alias,
                binding.manage_capability_revision,
                &operation,
            )
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
    async fn resume_preparing_rollback(
        &self,
        binding: RelayBindingEntry,
        operation: &RelayOperationContext,
    ) -> Result<(), RelayError> {
        if binding.state != RelayBindingState::RollbackPending {
            return Err(RelayError::InvalidResponse);
        }
        self.tombstone_versioned_secret(&binding.read_capability_alias, operation)
            .await?;
        self.tombstone_versioned_secret(&binding.manage_capability_alias, operation)
            .await?;
        let _ = self
            .cas_bounded(binding, operation, |mut entry| {
                entry.state = RelayBindingState::Tombstoned;
                entry
            })
            .await?;
        Ok(())
    }

    async fn tombstone_versioned_secret(
        &self,
        alias: &RelaySecretAlias,
        operation: &RelayOperationContext,
    ) -> Result<(), RelayError> {
        for _ in 0..4 {
            let current_revision = self
                .run_step(operation, self.secrets.revision(alias))
                .await?
                .map_err(|_| RelayError::SecureStorageUnavailable)?;
            let (expected_revision, replacement_revision) = match current_revision {
                RelaySecretRevision::Missing => (None, 1),
                RelaySecretRevision::Found(revision) => (
                    Some(revision),
                    revision
                        .checked_add(1)
                        .ok_or(RelayError::SecureStorageUnavailable)?,
                ),
            };
            let result = self
                .run_step(
                    operation,
                    self.secrets.compare_and_tombstone(
                        alias,
                        expected_revision,
                        replacement_revision,
                    ),
                )
                .await;
            match result {
                Ok(Ok(RelaySecretCasOutcome::Conflict)) => continue,
                Ok(Ok(RelaySecretCasOutcome::Stored)) => {
                    if self
                        .capability_tombstone_is_authoritative(
                            alias,
                            replacement_revision,
                            operation,
                        )
                        .await?
                    {
                        return Ok(());
                    }
                    continue;
                }
                Ok(Err(_)) | Err(_) => {
                    // A native callback may durably apply and then report an
                    // error, or may outlive this dropped Rust future. Reload
                    // through a fresh budget before classifying the result.
                    let recovery = RelayOperationContext::with_timeout(self.request_timeout);
                    if self
                        .capability_tombstone_is_authoritative(
                            alias,
                            replacement_revision,
                            &recovery,
                        )
                        .await?
                    {
                        return Ok(());
                    }
                    return match result {
                        Err(error) => Err(error),
                        Ok(Err(_)) => Err(RelayError::SecureStorageUnavailable),
                        Ok(Ok(_)) => unreachable!(),
                    };
                }
            }
        }
        Err(RelayError::Retryable)
    }

    async fn capability_tombstone_is_authoritative(
        &self,
        alias: &RelaySecretAlias,
        minimum_revision: u64,
        operation: &RelayOperationContext,
    ) -> Result<bool, RelayError> {
        let revision_before = self
            .run_step(operation, self.secrets.revision(alias))
            .await?
            .map_err(|_| RelayError::SecureStorageUnavailable)?;
        if !matches!(revision_before, RelaySecretRevision::Found(actual) if actual >= minimum_revision)
        {
            return Ok(false);
        }
        let value = self
            .run_step(operation, self.secrets.read(alias))
            .await?
            .map_err(|_| RelayError::SecureStorageUnavailable)?;
        let revision_after = self
            .run_step(operation, self.secrets.revision(alias))
            .await?
            .map_err(|_| RelayError::SecureStorageUnavailable)?;
        Ok(revision_after == revision_before && value.is_none())
    }

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
            .read_capability(
                &binding.manage_capability_alias,
                binding.manage_capability_revision,
                operation,
            )
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
                .read_capability(
                    &binding.manage_capability_alias,
                    binding.manage_capability_revision,
                    operation,
                )
                .await?;
            match registration.disposition {
                RelayRegistrationDisposition::Active => {
                    let token_revision = registration
                        .token_revision
                        .ok_or(RelayError::SecureStorageUnavailable)?;
                    let token = self
                        .read_versioned_secret(&registration.token_alias, token_revision, operation)
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
                    if let Some(previous) = registration.previous.clone() {
                        let (registration_id, relay_generation) =
                            match (previous.relay_registration_id, previous.relay_generation) {
                                (Some(registration_id), Some(relay_generation)) => {
                                    (registration_id, relay_generation)
                                }
                                (None, None) => {
                                    let Some(token) = self
                                        .read_versioned_secret_optional(
                                            &previous.token_alias,
                                            previous.token_revision,
                                            operation,
                                        )
                                        .await?
                                    else {
                                        self.tombstone_versioned_secret(
                                            &previous.token_alias,
                                            operation,
                                        )
                                        .await?;
                                        binding = self
                                            .cas_bounded(binding, operation, |mut entry| {
                                                if let Some(index) = registration_index(
                                                    &entry.registrations,
                                                    registration.provider,
                                                    registration.environment,
                                                ) {
                                                    entry.registrations[index].previous = None;
                                                }
                                                entry
                                                    .retired_token_aliases
                                                    .retain(|alias| alias != &previous.token_alias);
                                                entry
                                            })
                                            .await?;
                                        continue;
                                    };
                                    let receipt = self
                                        .run_transport(
                                            operation,
                                            self.transport.register_device(
                                                transport_context(
                                                    &binding,
                                                    authorization,
                                                    operation.clone(),
                                                ),
                                                RelayRegisterDeviceRequest {
                                                    installation_id: binding
                                                        .installation_id
                                                        .clone(),
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
                                    binding.registrations[index]
                                        .previous
                                        .as_mut()
                                        .ok_or(RelayError::InvalidProviderRegistration)?
                                        .relay_registration_id = Some(receipt.registration_id);
                                    binding.registrations[index]
                                        .previous
                                        .as_mut()
                                        .ok_or(RelayError::InvalidProviderRegistration)?
                                        .relay_generation = Some(receipt.generation);
                                    binding =
                                        self.cas_bounded(binding, operation, |entry| entry).await?;
                                    continue;
                                }
                                _ => return Err(RelayError::InvalidProviderRegistration),
                            };
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
                        self.tombstone_versioned_secret(&previous.token_alias, operation)
                            .await?;
                        binding = self
                            .cas_bounded(binding, operation, |mut entry| {
                                if let Some(index) = registration_index(
                                    &entry.registrations,
                                    registration.provider,
                                    registration.environment,
                                ) {
                                    entry.registrations[index].previous = None;
                                }
                                entry
                                    .retired_token_aliases
                                    .retain(|alias| alias != &previous.token_alias);
                                entry
                            })
                            .await?;
                        continue;
                    }
                    let (registration_id, relay_generation) = match (
                        registration.relay_registration_id,
                        registration.relay_generation,
                    ) {
                        (Some(registration_id), Some(relay_generation)) => {
                            (registration_id, relay_generation)
                        }
                        (None, None) => {
                            // Registration may have committed remotely before
                            // its receipt was lost. Recover the authoritative
                            // registration identity without clearing the
                            // durable tombstone disposition, then loop back to
                            // revoke it through the returned relay generation.
                            let Some(token) = self
                                .read_versioned_secret_optional(
                                    &registration.token_alias,
                                    registration.token_revision,
                                    operation,
                                )
                                .await?
                            else {
                                // The journal reservation preceded secure
                                // custody. Fence the alias before clearing the
                                // row so a still-running native CAS cannot
                                // recreate token bytes afterward.
                                self.tombstone_versioned_secret(
                                    &registration.token_alias,
                                    operation,
                                )
                                .await?;
                                binding.registrations.remove(index);
                                binding =
                                    self.cas_bounded(binding, operation, |entry| entry).await?;
                                continue;
                            };
                            let receipt = self
                                .run_transport(
                                    operation,
                                    self.transport.register_device(
                                        transport_context(
                                            &binding,
                                            authorization,
                                            operation.clone(),
                                        ),
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
                            binding.registrations[index].relay_generation =
                                Some(receipt.generation);
                            binding = self.cas_bounded(binding, operation, |entry| entry).await?;
                            continue;
                        }
                        _ => return Err(RelayError::InvalidProviderRegistration),
                    };
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
                    self.tombstone_versioned_secret(&registration.token_alias, operation)
                        .await?;
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
        while let Some((provider, environment, previous)) =
            binding.registrations.iter().find_map(|registration| {
                if registration.pending_sync {
                    None
                } else {
                    registration
                        .previous
                        .clone()
                        .map(|previous| (registration.provider, registration.environment, previous))
                }
            })
        {
            self.tombstone_versioned_secret(&previous.token_alias, operation)
                .await?;
            binding = self
                .cas_bounded(binding, operation, |mut entry| {
                    if let Some(index) =
                        registration_index(&entry.registrations, provider, environment)
                    {
                        entry.registrations[index].previous = None;
                    }
                    entry
                        .retired_token_aliases
                        .retain(|alias| alias != &previous.token_alias);
                    entry
                })
                .await?;
        }
        // Aliases not associated with a still-pending prior receipt are safe
        // for idempotent cleanup and remain hard-capped in the journal.
        while let Some(alias) = binding.retired_token_aliases.first().cloned() {
            self.tombstone_versioned_secret(&alias, operation).await?;
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
        // provider aliases are revision-tombstoned regardless of pending
        // provider state before journal ownership is cleared.
        for alias in &binding.retired_token_aliases {
            self.tombstone_versioned_secret(alias, operation).await?;
        }
        for registration in &binding.registrations {
            if let Some(previous) = &registration.previous {
                self.tombstone_versioned_secret(&previous.token_alias, operation)
                    .await?;
            }
            self.tombstone_versioned_secret(&registration.token_alias, operation)
                .await?;
        }
        // Capability slots retain non-secret revision tombstones. Besides the
        // Preparing rollback case, this also fences a callback from an older
        // superseded enrollment that outlives a later Staged/Active cleanup.
        self.tombstone_versioned_secret(&binding.read_capability_alias, operation)
            .await?;
        self.tombstone_versioned_secret(&binding.manage_capability_alias, operation)
            .await?;
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

fn enrollment_matches(binding: &RelayBindingEntry, enrollment: &RelayEnrollment) -> bool {
    binding.installation_id == enrollment.installation_id
        && binding.origin == enrollment.origin
        && binding.staging_command_id == enrollment.command_id
}

fn merged_provider_tombstone_fences(
    binding_fences: &[RelayProviderTombstoneFence],
    global_fences: &[RelayProviderTombstoneFence],
) -> Vec<RelayProviderTombstoneFence> {
    let mut merged = binding_fences.to_vec();
    for fence in global_fences {
        if let Some(existing) = merged.iter_mut().find(|existing| {
            existing.provider == fence.provider && existing.environment == fence.environment
        }) {
            existing.through_local_generation = existing
                .through_local_generation
                .max(fence.through_local_generation);
        } else {
            merged.push(fence.clone());
        }
    }
    merged.sort_by_key(|fence| (provider_sort_key(fence.provider), fence.environment as u8));
    merged
}

fn provider_sort_key(provider: RelayPushProvider) -> u8 {
    match provider {
        RelayPushProvider::Apns => 0,
        RelayPushProvider::Fcm => 1,
    }
}

fn provider_tombstone_fence(
    fences: &[RelayProviderTombstoneFence],
    provider: RelayPushProvider,
    environment: RelayPushEnvironment,
) -> Option<&RelayProviderTombstoneFence> {
    fences
        .iter()
        .find(|fence| fence.provider == provider && fence.environment == environment)
}

fn advance_provider_tombstone_fence(
    fences: &mut Vec<RelayProviderTombstoneFence>,
    tombstone: &PushTokenTombstone,
) {
    if let Some(fence) = fences.iter_mut().find(|fence| {
        fence.provider == tombstone.provider && fence.environment == tombstone.environment
    }) {
        fence.through_local_generation = fence
            .through_local_generation
            .max(tombstone.through_local_generation);
        return;
    }
    debug_assert!(fences.len() < MAX_PROVIDER_TOMBSTONE_FENCES);
    fences.push(RelayProviderTombstoneFence {
        provider: tombstone.provider,
        environment: tombstone.environment,
        through_local_generation: tombstone.through_local_generation,
    });
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
    let high_watermark_successor = page
        .high_watermark
        .checked_add(1)
        .ok_or(RelayError::InvalidResponse)?;
    let requested_successor = requested_after
        .checked_add(1)
        .ok_or(RelayError::InvalidResponse)?;
    if page.schema_version != RELAY_SCHEMA_VERSION
        || page.requested_after != requested_after
        || page.encoded_bytes > max_response_bytes
        || page.events.len() > page_limit as usize
        || page.high_watermark < target_cursor
        || page.next_cursor > page.high_watermark
        || page.replay_floor == 0
        || page.replay_floor > high_watermark_successor
        || (requested_successor < page.replay_floor && !page.reset_required)
    {
        return Err(RelayError::InvalidResponse);
    }
    if page.reset_required {
        if !page.events.is_empty() {
            return Err(RelayError::InvalidResponse);
        }
        return Ok(());
    }
    let mut expected = requested_successor;
    let mut seen_ids = std::collections::HashSet::new();
    for event in &page.events {
        if event.cursor != expected
            || event.expires_at_ms <= now_ms
            || !seen_ids.insert(&event.event_id)
        {
            return Err(RelayError::InvalidResponse);
        }
        expected = expected.checked_add(1).ok_or(RelayError::InvalidResponse)?;
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
