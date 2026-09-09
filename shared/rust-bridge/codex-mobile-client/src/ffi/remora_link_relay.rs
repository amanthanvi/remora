//! Authenticated pairing owns relay enrollment and authoritative repair.
//! Native platforms supply custody and wake hints, never cursor authority.

use super::*;
use crate::background_relay::*;

pub(crate) struct PairedHostRelayRepair {
    client: std::sync::Weak<crate::MobileClient>,
    journal: Arc<dyn RelayBindingJournalPort>,
}

impl PairedHostRelayRepair {
    pub(crate) fn new(
        client: std::sync::Weak<crate::MobileClient>,
        journal: Arc<dyn RelayBindingJournalPort>,
    ) -> Self {
        Self { client, journal }
    }

    async fn repair_inner(
        &self,
        host_id: &RelayHostId,
        generation: u64,
        through_cursor: u64,
        operation: &RelayOperationContext,
    ) -> Result<RelayRepairReceipt, RelayRepairError> {
        let client = self.client.upgrade().ok_or(RelayRepairError::Cancelled)?;
        // Repair already owns the coordinator operation lock. Do not queue
        // behind a writer that may itself be waiting for provisioning's lease.
        let _relay_configuration = client
            .background_relay_configuration
            .try_read()
            .map_err(|_| RelayRepairError::Cancelled)?;
        let binding = self
            .current_binding(&client, host_id, generation, operation)
            .await?;
        let _configuration = client
            .remora_link_configuration
            .try_read()
            .map_err(|_| RelayRepairError::Cancelled)?;
        let paired = remora_link_read(&client.remora_link)
            .clone()
            .ok_or(RelayRepairError::Unavailable)?;
        let authority = paired
            .journal
            .load(&host_id.0)
            .await
            .map_err(|_| RelayRepairError::Unavailable)?
            .filter(|entry| entry.phase == JournalPhaseV2::Enrolled)
            .and_then(|entry| entry.credential)
            .ok_or(RelayRepairError::RePairRequired)?;
        let attached = client
            .connect_remora_link_runtime(Arc::clone(&paired), &host_id.0, false, None)
            .await
            .map_err(|_| RelayRepairError::Unavailable)?;
        if !attached.unavailable_runtime_ids.is_empty() {
            return Err(RelayRepairError::Unavailable);
        }
        let before = paired
            .lifecycle
            .relay_barrier(
                &host_id.0,
                binding.installation_id.0.clone(),
                through_cursor,
            )
            .await
            .map_err(repair_lifecycle_error)?;
        // A missing observer is not proof that a detached coding runtime was idle.
        if before
            .runtime_states
            .iter()
            .any(|state| !is_shell_runtime(&state.runtime_id) && state.session_id == "absent")
        {
            return Err(RelayRepairError::Unavailable);
        }
        let projection = client
            .read_relay_projection(&host_id.0)
            .await
            .map_err(|_| RelayRepairError::Unavailable)?;
        let after = paired
            .lifecycle
            .relay_barrier(
                &host_id.0,
                binding.installation_id.0.clone(),
                through_cursor,
            )
            .await
            .map_err(repair_lifecycle_error)?;
        if before != after {
            return Err(RelayRepairError::Unavailable);
        }
        let current_authority = paired
            .journal
            .load(&host_id.0)
            .await
            .map_err(|_| RelayRepairError::Unavailable)?
            .filter(|entry| entry.phase == JournalPhaseV2::Enrolled)
            .and_then(|entry| entry.credential)
            .ok_or(RelayRepairError::RePairRequired)?;
        if current_authority != authority {
            return Err(RelayRepairError::Cancelled);
        }
        let current = self
            .current_binding(&client, host_id, generation, operation)
            .await?;
        if current != binding {
            return Err(RelayRepairError::Cancelled);
        }
        operation.remaining().map_err(repair_operation_error)?;
        if !client
            .apply_relay_projection(&host_id.0, projection)
            .map_err(|_| RelayRepairError::Unavailable)?
        {
            return Err(RelayRepairError::Cancelled);
        }
        Ok(RelayRepairReceipt {
            applied_through_cursor: through_cursor,
            authoritative: true,
            verified_barrier_id: Some(after.barrier_id),
        })
    }

    async fn current_binding(
        &self,
        client: &crate::MobileClient,
        host_id: &RelayHostId,
        generation: u64,
        operation: &RelayOperationContext,
    ) -> Result<RelayBindingEntry, RelayRepairError> {
        operation.remaining().map_err(repair_operation_error)?;
        let current = client
            .background_relay
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if !current.is_some_and(|current| Arc::ptr_eq(&current.journal, &self.journal)) {
            return Err(RelayRepairError::Cancelled);
        }
        self.journal
            .load_by_host(host_id)
            .await
            .map_err(|_| RelayRepairError::Unavailable)?
            .filter(|entry| {
                entry.state == RelayBindingState::Active && entry.repair_generation == generation
            })
            .ok_or(RelayRepairError::Cancelled)
    }
}

#[async_trait]
impl RelayAuthoritativeRepairPort for PairedHostRelayRepair {
    async fn repair(
        &self,
        host_id: &RelayHostId,
        generation: u64,
        mode: RelayRepairMode,
        operation: RelayOperationContext,
    ) -> Result<RelayRepairReceipt, RelayRepairError> {
        let through_cursor = match mode {
            RelayRepairMode::Incremental { through_cursor, .. }
            | RelayRepairMode::Snapshot { through_cursor, .. }
            | RelayRepairMode::Full { through_cursor } => through_cursor,
        };
        let remaining = operation.remaining().map_err(repair_operation_error)?;
        tokio::select! {
            biased;
            _ = operation.cancellation.cancelled() => Err(RelayRepairError::Cancelled),
            result = tokio::time::timeout(remaining, self.repair_inner(host_id, generation, through_cursor, &operation)) => {
                result.map_err(|_| RelayRepairError::DeadlineExceeded)?
            }
        }
    }
}

fn repair_lifecycle_error(error: LifecycleErrorV2) -> RelayRepairError {
    match error {
        LifecycleErrorV2::NotEnrolled | LifecycleErrorV2::InvalidSelection => {
            RelayRepairError::RePairRequired
        }
        _ => RelayRepairError::Unavailable,
    }
}

fn repair_operation_error(error: RelayError) -> RelayRepairError {
    match error {
        RelayError::Cancelled => RelayRepairError::Cancelled,
        _ => RelayRepairError::DeadlineExceeded,
    }
}

impl crate::MobileClient {
    pub(crate) async fn retire_paired_relay(&self, host: &str) {
        let operation = RelayOperationContext::with_timeout(Duration::from_secs(25));
        let cleanup = async {
            let Some(configuration) = self.paired_relay_configuration().await else {
                return Ok(());
            };
            let relay = &configuration.relay;
            let _provisioning = relay.provisioning.lock().await;
            let binding = relay
                .journal
                .load_by_host(&RelayHostId(host.to_owned()))
                .await
                .map_err(|_| RelayError::JournalUnavailable)?;
            if let Some(binding) = binding {
                retire_unpaired_binding(&configuration.paired, relay, &binding, &operation).await?;
            }
            Ok::<_, RelayError>(())
        };
        if !matches!(
            tokio::time::timeout(Duration::from_secs(25), cleanup).await,
            Ok(Ok(()))
        ) {
            warn!(host_id = host, "background relay cleanup pending");
        }
    }

    /// Foreground/token reconciliation also replays interrupted provisioning.
    /// Unsupported or temporarily offline hosts do not block ordinary pairing.
    pub(crate) async fn synchronize_paired_relays(&self) {
        let operation = RelayOperationContext::with_timeout(Duration::from_secs(25));
        let work = async {
            let Some(configuration) = self.paired_relay_configuration().await else {
                return;
            };
            let paired = &configuration.paired;
            let relay = &configuration.relay;
            let _provisioning = relay.provisioning.lock().await;
            let Ok(entries) = paired.journal.entries().await else {
                return;
            };
            let enrolled = entries
                .iter()
                .filter(|entry| entry.phase == JournalPhaseV2::Enrolled)
                .map(|entry| entry.binding.host_id.clone())
                .collect::<HashSet<_>>();
            // Forget/revoke survives process death: the durable pairing journal,
            // not a native callback, determines which registrations may remain.
            if let Ok(bindings) = relay.journal.list().await {
                for binding in bindings {
                    let _ = retire_unpaired_binding(paired, relay, &binding, &operation).await;
                }
            }
            let mut pending = futures::stream::iter(enrolled)
                .map(|host| {
                    let operation = &operation;
                    async move {
                        let result = provision_relay(paired, relay, &host, operation).await;
                        (host, result)
                    }
                })
                .buffer_unordered(BATCH_CONCURRENCY);
            while let Some((host, result)) = pending.next().await {
                if let Err(error) = result {
                    warn!(
                        host_id = host,
                        ?error,
                        "background relay enrollment pending"
                    );
                }
            }
        };
        let _ = tokio::time::timeout(Duration::from_secs(25), work).await;
    }

    async fn paired_relay_configuration(&self) -> Option<PairedRelayConfiguration<'_>> {
        let background = self.background_relay_configuration.read().await;
        let pairing = self.remora_link_configuration.read().await;
        let paired = remora_link_read(&self.remora_link).clone()?;
        let relay = self
            .background_relay
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()?;
        Some(PairedRelayConfiguration {
            _background: background,
            _pairing: pairing,
            paired,
            relay,
        })
    }
}

struct PairedRelayConfiguration<'a> {
    _background: tokio::sync::RwLockReadGuard<'a, ()>,
    _pairing: tokio::sync::RwLockReadGuard<'a, ()>,
    paired: Arc<ConfiguredRemoraLink>,
    relay: Arc<ConfiguredBackgroundRelay>,
}

async fn retire_unpaired_binding(
    paired: &ConfiguredRemoraLink,
    relay: &ConfiguredBackgroundRelay,
    binding: &RelayBindingEntry,
    operation: &RelayOperationContext,
) -> Result<(), RelayError> {
    if binding.state == RelayBindingState::Tombstoned {
        return Ok(());
    }
    let authority = paired
        .journal
        .load(&binding.host_id.0)
        .await
        .map_err(|_| RelayError::JournalUnavailable)?;
    if authority.is_some_and(|entry| entry.phase == JournalPhaseV2::Enrolled) {
        return Ok(());
    }
    // All staging holds the provisioning mutex, so a new pairing cannot
    // replace this binding between the authority read and command-fenced retire.
    // Never hold the pairing lifecycle host lock while awaiting relay work.
    relay
        .relay
        .rollback_enrollment(
            &binding.host_id,
            &binding.staging_command_id,
            operation.clone(),
        )
        .await
}

async fn provision_relay(
    paired: &ConfiguredRemoraLink,
    relay: &ConfiguredBackgroundRelay,
    host: &str,
    operation: &RelayOperationContext,
) -> Result<(), RelayError> {
    let host_id = RelayHostId(host.to_owned());
    let command = current_pairing_command(paired, host).await?;
    let mut binding = relay
        .journal
        .load_by_host(&host_id)
        .await
        .map_err(|_| RelayError::JournalUnavailable)?;
    if let Some(previous) = &binding
        && previous.staging_command_id != command
    {
        relay
            .relay
            .rollback_enrollment(&host_id, &previous.staging_command_id, operation.clone())
            .await?;
        binding = None;
    }
    if binding.as_ref().is_none_or(|entry| {
        matches!(
            entry.state,
            RelayBindingState::Preparing
                | RelayBindingState::NeedsRepair
                | RelayBindingState::Tombstoned
        )
    }) {
        let bundle = paired
            .lifecycle
            .enroll_relay(host)
            .await
            .map_err(|_| RelayError::Retryable)?;
        if bundle.command_id != command.0 || current_pairing_command(paired, host).await? != command
        {
            return Err(RelayError::Cancelled);
        }
        relay
            .relay
            .stage_enrollment(
                RelayEnrollment {
                    host_id: host_id.clone(),
                    origin: ValidatedRelayOrigin::parse(
                        &bundle.relay_origin,
                        relay.allow_loopback_http,
                    )?,
                    installation_id: RelayInstallationId::parse(bundle.installation_id)?,
                    command_id: RelayEnrollmentCommandId::parse(bundle.command_id)?,
                    read_capability: OpaqueRelaySecret::new(
                        bundle.read_capability.as_bytes().to_vec(),
                    )?,
                    manage_capability: OpaqueRelaySecret::new(
                        bundle.manage_capability.as_bytes().to_vec(),
                    )?,
                },
                operation.clone(),
            )
            .await?;
        binding = relay
            .journal
            .load_by_host(&host_id)
            .await
            .map_err(|_| RelayError::JournalUnavailable)?;
    }
    let binding = binding.ok_or(RelayError::UnknownInstallation)?;
    if binding.staging_command_id != command
        || current_pairing_command(paired, host).await? != command
    {
        return Err(RelayError::Cancelled);
    }
    if !matches!(
        binding.state,
        RelayBindingState::Staged | RelayBindingState::Active
    ) {
        return Err(RelayError::Retryable);
    }
    relay
        .relay
        .commit_enrollment(&host_id, &binding.staging_command_id, operation.clone())
        .await?;
    // Local success is durable before host custody is released. An ambiguous
    // commit retries this exact receipt; it never rolls the device back.
    paired
        .lifecycle
        .commit_relay(
            host,
            binding.installation_id.0,
            binding.staging_command_id.0,
        )
        .await
        .map_err(|_| RelayError::Retryable)?;
    Ok(())
}

async fn current_pairing_command(
    paired: &ConfiguredRemoraLink,
    host: &str,
) -> Result<RelayEnrollmentCommandId, RelayError> {
    let authority = paired
        .journal
        .load(host)
        .await
        .map_err(|_| RelayError::JournalUnavailable)?
        .filter(|entry| entry.phase == JournalPhaseV2::Enrolled)
        .and_then(|entry| entry.credential)
        .ok_or(RelayError::RePairRequired)?;
    RelayEnrollmentCommandId::parse(PairingLifecycleV2::relay_command_id(
        host,
        &authority.credential_id,
    ))
}

#[cfg(test)]
#[path = "remora_link_relay_tests.rs"]
mod tests;
