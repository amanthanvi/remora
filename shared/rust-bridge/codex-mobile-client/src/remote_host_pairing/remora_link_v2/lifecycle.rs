//! Journal-backed Remora Link v2 pairing and credential lifecycle.
//!
//! This state machine intentionally does not reuse the compatibility
//! coordinator in the parent module. V2 has no portable grant and no
//! confirm-reconnect phase: authority is the host record plus a fresh proof
//! from the per-host hardware key on every operation.

use std::collections::HashMap;
use std::ops::Deref;
use std::sync::{Arc, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use remora_bridge_core::command_center::HostCommandCenterStatusV1;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use zeroize::{Zeroize, Zeroizing};

use super::host_port::{proof_transcript, sign_proof};
use super::v2_journal::{
    CredentialJournalV2, EnrollmentCandidateJournalV2, EnrollmentJournalV2, HostBindingJournalV2,
    InvitationJournalV2, JOURNAL_SCHEMA_VERSION, JournalPhaseV2, JournalPortErrorV2, JournalPortV2,
    MAX_ENROLLMENT_CANDIDATES, MutationJournalV2, MutationKindV2, PairingJournalEntryV2,
    PendingClaimJournalV2, QuarantineReasonV2, RestartCommandJournalV2, RestartDispositionV2,
    RevocationReceiptJournalV2, is_subset, scope_subset,
};
use super::v2_ports::{
    CredentialCustodyPortV2, CredentialPortError, EntropyPortV2, FinishedExchangeV2, HardwareKeyV2,
    HostPortErrorV2, HostPortV2, HostRouteV2, StartedExchangeV2,
};
use super::wire::{
    AgentInfoV2, ConfirmationModeV2, DeviceScopeV2, EnrolledDeviceV2, EnrollmentConfirmationV2,
    InvitationInspectionV2, PendingEnrollmentV2, RequestV2, ResponseV2, RestartStatusV2,
    RevocationReceiptV2, SessionV2, ThreadBindingReceiptV2, WireError, WorkIntentStatusV2,
    derive_sas, valid_device_name, validate_policy,
};
use crate::remote_host_pairing::identity::V2Invite;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum EnrollmentOutcomeV2 {
    Pending(PendingEnrollmentV2),
    Enrolled(EnrolledDeviceV2),
    AlreadyEnrolled(CredentialJournalV2),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReconnectOutcomeV2 {
    pub(crate) host_id: String,
    pub(crate) runtime_id: String,
    pub(crate) session: SessionV2,
    /// Exact take-once identity for the runtime stream retained by the host
    /// adapter that completed this authenticated connect round.
    pub(crate) attachment_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RestartOutcomeV2 {
    Succeeded { command_sequence: u64 },
    OutcomeUnknown { command_sequence: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WorkIntentOutcomeV2 {
    Execute,
    Reserved,
    Succeeded { turn_id: String },
    OutcomeUnknown,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ThreadBindingOutcomeV2 {
    Bound(ThreadBindingReceiptV2),
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorkIntentOperationV2 {
    Prepare,
    Begin,
    Complete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MutationOutcomeV2 {
    Revoked,
    RolledBack,
    OutcomeUnknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ForgetOutcomeV2 {
    ForgottenLocally {
        host_id: String,
        host_revocation_still_required: bool,
    },
    AlreadyForgotten {
        host_id: String,
        host_revocation_still_required: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RecoveryOutcomeV2 {
    NoJournal,
    Stable(JournalPhaseV2),
    NeedsInvitation,
    Restart(RestartOutcomeV2),
    Mutation(MutationOutcomeV2),
    Forgotten(ForgetOutcomeV2),
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum LifecycleErrorV2 {
    #[error("Remora Link v2 journal is unavailable")]
    JournalUnavailable,
    #[error("Remora Link v2 journal changed concurrently")]
    JournalConflict,
    #[error("Remora Link v2 journal is corrupt or unsupported")]
    JournalCorrupt,
    #[error("hardware-backed Remora Link credential is unavailable")]
    CredentialUnavailable,
    #[error("hardware-backed Remora Link credential is missing")]
    MissingCredential,
    #[error("hardware-backed Remora Link signature is invalid")]
    InvalidSignature,
    #[error("Remora Link host is unavailable")]
    HostUnavailable,
    #[error("Remora Link host identity changed")]
    HostIdentityDrift,
    #[error("Remora Link client transport identity changed")]
    ClientIdentityDrift,
    #[error("Remora Link hardware-key identity changed")]
    HardwareKeyDrift,
    #[error("Remora Link host policy differs from the invitation")]
    PolicyDrift,
    #[error("Remora Link enrollment confirmation is not authentic")]
    ConfirmationMismatch,
    #[error("Remora Link v2 protocol was violated")]
    ProtocolViolation,
    #[error("Remora Link invitation is unavailable or no longer valid")]
    PairingUnavailable,
    #[error("Remora Link credential is not authorized")]
    AuthorizationRequired,
    #[error("selected Remora Link runtime is unavailable")]
    AgentUnavailable,
    #[error("Remora Link operation outcome is unknown")]
    OutcomeUnknown,
    #[error("Remora Link runtime or scope selection is invalid")]
    InvalidSelection,
    #[error("Remora Link invitation does not match the staged operation")]
    InvitationMismatch,
    #[error("Remora Link operation requires an enrolled host")]
    NotEnrolled,
    #[error("a Remora Link lifecycle operation is already in progress")]
    OperationInProgress,
    #[error("Remora Link host is quarantined")]
    Quarantined,
    #[error("too many ambiguous enrollment attempts require rollback")]
    CandidateLimitReached,
}

pub(crate) struct PairingLifecycleV2 {
    host: Arc<dyn HostPortV2>,
    journal: Arc<dyn JournalPortV2>,
    custody: Arc<dyn CredentialCustodyPortV2>,
    entropy: Arc<dyn EntropyPortV2>,
    host_locks: Mutex<HashMap<String, Weak<Mutex<()>>>>,
}

/// Cancellation guard for the interval after the host has retained an
/// exchange and before `finish_exchange` has taken ownership of it. Dropping
/// a mobile future anywhere in that interval schedules best-effort abandon.
struct ExchangeLeaseV2 {
    host: Arc<dyn HostPortV2>,
    started: Option<StartedExchangeV2>,
}

impl ExchangeLeaseV2 {
    fn new(host: Arc<dyn HostPortV2>, started: StartedExchangeV2) -> Self {
        Self {
            host,
            started: Some(started),
        }
    }

    fn abandon(&mut self) {
        if let Some(started) = self.started.as_ref() {
            self.host.abandon_exchange(&started.exchange_id);
            self.started.take();
        }
    }

    fn disarm(&mut self) {
        self.started.take();
    }
}

impl Deref for ExchangeLeaseV2 {
    type Target = StartedExchangeV2;

    fn deref(&self) -> &Self::Target {
        self.started
            .as_ref()
            .expect("exchange lease is accessed only while armed")
    }
}

impl Drop for ExchangeLeaseV2 {
    fn drop(&mut self) {
        let Some(started) = self.started.take() else {
            return;
        };
        self.host.abandon_exchange(&started.exchange_id);
    }
}

impl PairingLifecycleV2 {
    pub(crate) fn new(
        host: Arc<dyn HostPortV2>,
        journal: Arc<dyn JournalPortV2>,
        custody: Arc<dyn CredentialCustodyPortV2>,
        entropy: Arc<dyn EntropyPortV2>,
    ) -> Self {
        Self {
            host,
            journal,
            custody,
            entropy,
            host_locks: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) async fn inspect(
        &self,
        invite: &V2Invite,
    ) -> Result<InvitationInspectionV2, LifecycleErrorV2> {
        let host_id = host_id(invite);
        let host_lock = self.host_lock(&host_id).await;
        let _operation = host_lock.lock().await;

        let previous = self.load(&host_id).await?;
        let hardware_key = self.custody.ensure_hardware_key(&host_id).await?;
        let mut entry = match previous.as_ref() {
            Some(existing) => self.prepare_existing_inspection(existing, invite, &hardware_key)?,
            None => PairingJournalEntryV2 {
                schema_version: JOURNAL_SCHEMA_VERSION,
                revision: 0,
                binding: HostBindingJournalV2 {
                    host_id: host_id.clone(),
                    node_id: invite.node_id.clone(),
                    host_display_name: invite.host_name.clone(),
                    relay_hint: invite.relay.clone(),
                    hardware_key_slot: hardware_key.slot.clone(),
                    device_public_key: hardware_key.public_key.clone(),
                    client_endpoint_id: None,
                },
                invitation: Some(invitation_from_invite(invite)),
                enrollment: None,
                credential: None,
                mutation: None,
                restart_high_watermark: 0,
                pending_restart: None,
                phase: JournalPhaseV2::Inspecting,
            },
        };
        entry = self.save(previous.as_ref(), entry).await?;

        let request = inspect_request(invite, &hardware_key, self.fresh_nonce());
        request
            .validate()
            .map_err(|_| LifecycleErrorV2::ProtocolViolation)?;
        let started = self.begin_round(&mut entry, &request, true).await?;
        let terminal = self
            .complete_round(&mut entry, &request, started, None)
            .await?
            .response;
        if !terminal.ok {
            return Err(map_terminal_error(&terminal));
        }
        let inspection = terminal
            .inspection
            .clone()
            .ok_or(LifecycleErrorV2::ProtocolViolation)?;
        if !inspection_matches_invite(&inspection, invite) {
            self.quarantine(&mut entry, QuarantineReasonV2::PolicyDrift)
                .await?;
            return Err(LifecycleErrorV2::PolicyDrift);
        }
        let invitation = entry
            .invitation
            .as_mut()
            .ok_or(LifecycleErrorV2::JournalCorrupt)?;
        invitation.runtime_offers = inspection.runtime_offers.clone();
        entry.phase = JournalPhaseV2::Ready;
        self.save(Some(&entry), entry.clone_for_next_revision())
            .await?;
        Ok(inspection)
    }

    pub(crate) async fn enroll(
        &self,
        invite: &V2Invite,
        display_name: String,
        selected_runtime_ids: Vec<String>,
        requested_scopes: Vec<DeviceScopeV2>,
    ) -> Result<EnrollmentOutcomeV2, LifecycleErrorV2> {
        valid_device_name(&display_name).map_err(|_| LifecycleErrorV2::InvalidSelection)?;
        let (selected_runtime_ids, requested_scopes) =
            canonicalize_selection(selected_runtime_ids, requested_scopes)?;
        let host_id = host_id(invite);
        let host_lock = self.host_lock(&host_id).await;
        let _operation = host_lock.lock().await;
        let mut entry = self
            .load(&host_id)
            .await?
            .ok_or(LifecycleErrorV2::InvitationMismatch)?;
        ensure_invitation_matches(&entry, invite)?;

        match entry.phase {
            JournalPhaseV2::Enrolled => {
                return Ok(EnrollmentOutcomeV2::AlreadyEnrolled(
                    entry.credential.ok_or(LifecycleErrorV2::JournalCorrupt)?,
                ));
            }
            JournalPhaseV2::Ready => {
                validate_selection(&entry, &selected_runtime_ids, &requested_scopes)?;
                entry.enrollment = Some(EnrollmentJournalV2 {
                    display_name,
                    selected_runtime_ids,
                    requested_scopes,
                    idempotency_key: self.entropy.fresh_idempotency_key("enrollment"),
                    prospective_credential_id: None,
                    candidates: Vec::new(),
                    pending_claim: None,
                });
                entry.phase = JournalPhaseV2::EnrollmentStaged;
                entry = self
                    .save(Some(&entry), entry.clone_for_next_revision())
                    .await?;
            }
            JournalPhaseV2::EnrollmentStaged | JournalPhaseV2::EnrollmentPending => {
                let enrollment = entry
                    .enrollment
                    .as_ref()
                    .ok_or(LifecycleErrorV2::JournalCorrupt)?;
                if enrollment.display_name != display_name
                    || enrollment.selected_runtime_ids != selected_runtime_ids
                    || enrollment.requested_scopes != requested_scopes
                {
                    return Err(LifecycleErrorV2::OperationInProgress);
                }
            }
            JournalPhaseV2::Inspecting => return Err(LifecycleErrorV2::OperationInProgress),
            JournalPhaseV2::Quarantined { .. } => {
                return Err(LifecycleErrorV2::Quarantined);
            }
            _ => return Err(LifecycleErrorV2::OperationInProgress),
        }

        self.drive_enrollment(invite, &mut entry).await
    }

    pub(crate) async fn reconnect(
        &self,
        host_id: &str,
        runtime_id: String,
        last_seq: Option<u64>,
    ) -> Result<ReconnectOutcomeV2, LifecycleErrorV2> {
        let host_lock = self.host_lock(host_id).await;
        let _operation = host_lock.lock().await;
        let mut entry = self
            .load(host_id)
            .await?
            .ok_or(LifecycleErrorV2::NotEnrolled)?;
        if !matches!(entry.phase, JournalPhaseV2::Enrolled) {
            return Err(match entry.phase {
                JournalPhaseV2::Quarantined { .. } => LifecycleErrorV2::Quarantined,
                _ => LifecycleErrorV2::NotEnrolled,
            });
        }
        let credential = entry
            .credential
            .clone()
            .ok_or(LifecycleErrorV2::JournalCorrupt)?;
        if !credential.selected_runtime_ids.contains(&runtime_id)
            || !credential
                .granted_scopes
                .contains(&DeviceScopeV2::ConnectRuntime)
        {
            return Err(LifecycleErrorV2::InvalidSelection);
        }
        let request = RequestV2::Connect {
            v: super::wire::PROTOCOL_VERSION,
            credential_id: credential.credential_id,
            client_nonce: self.fresh_nonce(),
            agent: runtime_id.clone(),
            resume: last_seq.map(|last_seq| super::wire::ResumeV2 { last_seq }),
        };
        let started = self.begin_round(&mut entry, &request, false).await?;
        let finished = self
            .complete_round(&mut entry, &request, started, Some(credential.auth_epoch))
            .await?;
        let terminal = finished.response;
        if !terminal.ok {
            return Err(map_terminal_error(&terminal));
        }
        let attachment_id = finished
            .attachment_id
            .ok_or(LifecycleErrorV2::ProtocolViolation)?;
        let session = terminal
            .session
            .ok_or(LifecycleErrorV2::ProtocolViolation)?;
        Ok(ReconnectOutcomeV2 {
            host_id: host_id.to_string(),
            runtime_id,
            session,
            attachment_id,
        })
    }

    pub(crate) async fn list_agents(
        &self,
        host_id: &str,
    ) -> Result<Vec<AgentInfoV2>, LifecycleErrorV2> {
        let host_lock = self.host_lock(host_id).await;
        let _operation = host_lock.lock().await;
        let mut entry = self
            .load(host_id)
            .await?
            .ok_or(LifecycleErrorV2::NotEnrolled)?;
        if !matches!(entry.phase, JournalPhaseV2::Enrolled) {
            return Err(LifecycleErrorV2::NotEnrolled);
        }
        let credential = entry
            .credential
            .clone()
            .ok_or(LifecycleErrorV2::JournalCorrupt)?;
        if !credential
            .granted_scopes
            .contains(&DeviceScopeV2::InspectRuntimes)
        {
            return Err(LifecycleErrorV2::InvalidSelection);
        }
        let request = RequestV2::ListAgents {
            v: super::wire::PROTOCOL_VERSION,
            credential_id: credential.credential_id,
            client_nonce: self.fresh_nonce(),
        };
        let started = self.begin_round(&mut entry, &request, false).await?;
        let terminal = self
            .complete_round(&mut entry, &request, started, Some(credential.auth_epoch))
            .await?
            .response;
        if !terminal.ok {
            return Err(map_terminal_error(&terminal));
        }
        let agents = terminal.agents.ok_or(LifecycleErrorV2::ProtocolViolation)?;
        if agents
            .iter()
            .any(|agent| !credential.selected_runtime_ids.contains(&agent.name))
        {
            return Err(LifecycleErrorV2::ProtocolViolation);
        }
        Ok(agents)
    }

    pub(crate) async fn command_center_status(
        &self,
        host_id: &str,
    ) -> Result<Option<HostCommandCenterStatusV1>, LifecycleErrorV2> {
        let host_lock = self.host_lock(host_id).await;
        let _operation = host_lock.lock().await;
        let mut entry = self
            .load(host_id)
            .await?
            .ok_or(LifecycleErrorV2::NotEnrolled)?;
        if !matches!(entry.phase, JournalPhaseV2::Enrolled) {
            return Err(LifecycleErrorV2::NotEnrolled);
        }
        let credential = entry
            .credential
            .clone()
            .ok_or(LifecycleErrorV2::JournalCorrupt)?;
        if !credential
            .granted_scopes
            .contains(&DeviceScopeV2::InspectRuntimes)
        {
            return Err(LifecycleErrorV2::InvalidSelection);
        }
        let request = RequestV2::CommandCenterStatus {
            v: super::wire::PROTOCOL_VERSION,
            credential_id: credential.credential_id,
            client_nonce: self.fresh_nonce(),
        };
        let started = self.begin_round(&mut entry, &request, false).await?;
        let terminal = self
            .complete_round(&mut entry, &request, started, Some(credential.auth_epoch))
            .await?
            .response;
        if !terminal.ok {
            if terminal.error_code == Some(super::wire::ErrorCodeV2::InvalidRequest) {
                return Ok(None);
            }
            return Err(map_terminal_error(&terminal));
        }
        terminal
            .command_center_status
            .map(Some)
            .ok_or(LifecycleErrorV2::ProtocolViolation)
    }

    pub(crate) async fn bind_provider_thread(
        &self,
        host_id: &str,
        runtime_id: &str,
        provider_thread_id: &str,
    ) -> Result<ThreadBindingOutcomeV2, LifecycleErrorV2> {
        self.provider_thread_binding(host_id, Some(runtime_id), provider_thread_id)
            .await
    }

    pub(crate) async fn resolve_thread_binding(
        &self,
        host_id: &str,
        thread_id: &str,
    ) -> Result<ThreadBindingOutcomeV2, LifecycleErrorV2> {
        self.provider_thread_binding(host_id, None, thread_id).await
    }

    async fn provider_thread_binding(
        &self,
        host_id: &str,
        runtime_id: Option<&str>,
        thread_id: &str,
    ) -> Result<ThreadBindingOutcomeV2, LifecycleErrorV2> {
        let host_lock = self.host_lock(host_id).await;
        let _operation = host_lock.lock().await;
        let mut entry = self
            .load(host_id)
            .await?
            .ok_or(LifecycleErrorV2::NotEnrolled)?;
        if !matches!(entry.phase, JournalPhaseV2::Enrolled) {
            return Err(LifecycleErrorV2::NotEnrolled);
        }
        let credential = entry
            .credential
            .clone()
            .ok_or(LifecycleErrorV2::JournalCorrupt)?;
        if !credential
            .granted_scopes
            .contains(&DeviceScopeV2::ConnectRuntime)
            || runtime_id.is_some_and(|runtime_id| {
                !credential
                    .selected_runtime_ids
                    .iter()
                    .any(|selected| selected == runtime_id)
            })
        {
            return Err(LifecycleErrorV2::InvalidSelection);
        }
        let request = match runtime_id {
            Some(runtime_id) => RequestV2::BindProviderThread {
                v: super::wire::PROTOCOL_VERSION,
                credential_id: credential.credential_id,
                client_nonce: self.fresh_nonce(),
                runtime_id: runtime_id.to_string(),
                provider_thread_id: thread_id.to_string(),
            },
            None => RequestV2::ResolveThreadBinding {
                v: super::wire::PROTOCOL_VERSION,
                credential_id: credential.credential_id,
                client_nonce: self.fresh_nonce(),
                thread_id: thread_id.to_string(),
            },
        };
        request
            .validate()
            .map_err(|_| LifecycleErrorV2::InvalidSelection)?;
        let started = self.begin_round(&mut entry, &request, false).await?;
        let terminal = self
            .complete_round(&mut entry, &request, started, Some(credential.auth_epoch))
            .await?
            .response;
        if !terminal.ok
            && terminal.error_code == Some(super::wire::ErrorCodeV2::InvalidRequest)
            && terminal.thread_binding.is_none()
        {
            return Ok(ThreadBindingOutcomeV2::Unavailable);
        }
        if !terminal.ok {
            return Err(map_terminal_error(&terminal));
        }
        terminal
            .thread_binding
            .map(ThreadBindingOutcomeV2::Bound)
            .ok_or(LifecycleErrorV2::ProtocolViolation)
    }

    pub(crate) async fn prepare_send_message_intent(
        &self,
        host_id: &str,
        intent_id: &str,
        thread_id: &str,
        request_fingerprint: &str,
    ) -> Result<WorkIntentOutcomeV2, LifecycleErrorV2> {
        self.send_message_work_intent(
            host_id,
            WorkIntentOperationV2::Prepare,
            intent_id,
            thread_id,
            request_fingerprint,
        )
        .await
    }

    pub(crate) async fn begin_send_message_intent(
        &self,
        host_id: &str,
        intent_id: &str,
        thread_id: &str,
        request_fingerprint: &str,
    ) -> Result<WorkIntentOutcomeV2, LifecycleErrorV2> {
        self.send_message_work_intent(
            host_id,
            WorkIntentOperationV2::Begin,
            intent_id,
            thread_id,
            request_fingerprint,
        )
        .await
    }

    pub(crate) async fn complete_send_message_intent(
        &self,
        host_id: &str,
        intent_id: &str,
        thread_id: &str,
        request_fingerprint: &str,
    ) -> Result<WorkIntentOutcomeV2, LifecycleErrorV2> {
        self.send_message_work_intent(
            host_id,
            WorkIntentOperationV2::Complete,
            intent_id,
            thread_id,
            request_fingerprint,
        )
        .await
    }

    async fn send_message_work_intent(
        &self,
        host_id: &str,
        operation: WorkIntentOperationV2,
        intent_id: &str,
        thread_id: &str,
        request_fingerprint: &str,
    ) -> Result<WorkIntentOutcomeV2, LifecycleErrorV2> {
        let host_lock = self.host_lock(host_id).await;
        let _operation = host_lock.lock().await;
        let mut entry = self
            .load(host_id)
            .await?
            .ok_or(LifecycleErrorV2::NotEnrolled)?;
        if !matches!(entry.phase, JournalPhaseV2::Enrolled) {
            return Err(LifecycleErrorV2::NotEnrolled);
        }
        let credential = entry
            .credential
            .clone()
            .ok_or(LifecycleErrorV2::JournalCorrupt)?;
        if !credential
            .granted_scopes
            .contains(&DeviceScopeV2::ConnectRuntime)
        {
            return Err(LifecycleErrorV2::InvalidSelection);
        }

        let request = match operation {
            WorkIntentOperationV2::Prepare => RequestV2::PrepareSendMessageIntent {
                v: super::wire::PROTOCOL_VERSION,
                credential_id: credential.credential_id,
                client_nonce: self.fresh_nonce(),
                intent_id: intent_id.to_string(),
                thread_id: thread_id.to_string(),
                request_fingerprint: request_fingerprint.to_string(),
            },
            WorkIntentOperationV2::Begin => RequestV2::BeginSendMessageIntent {
                v: super::wire::PROTOCOL_VERSION,
                credential_id: credential.credential_id,
                client_nonce: self.fresh_nonce(),
                intent_id: intent_id.to_string(),
                thread_id: thread_id.to_string(),
                request_fingerprint: request_fingerprint.to_string(),
            },
            WorkIntentOperationV2::Complete => RequestV2::CompleteSendMessageIntent {
                v: super::wire::PROTOCOL_VERSION,
                credential_id: credential.credential_id,
                client_nonce: self.fresh_nonce(),
                intent_id: intent_id.to_string(),
                thread_id: thread_id.to_string(),
                request_fingerprint: request_fingerprint.to_string(),
            },
        };
        request
            .validate()
            .map_err(|_| LifecycleErrorV2::InvalidSelection)?;
        let started = self.begin_round(&mut entry, &request, false).await?;
        let terminal = self
            .complete_round(&mut entry, &request, started, Some(credential.auth_epoch))
            .await?
            .response;
        if !terminal.ok
            && terminal.error_code == Some(super::wire::ErrorCodeV2::InvalidRequest)
            && terminal.work_intent.is_none()
        {
            return Ok(WorkIntentOutcomeV2::Unavailable);
        }
        let receipt = terminal
            .work_intent
            .as_ref()
            .ok_or_else(|| map_terminal_error(&terminal))?;
        match receipt.status {
            WorkIntentStatusV2::Execute if terminal.ok => Ok(WorkIntentOutcomeV2::Execute),
            WorkIntentStatusV2::Reserved if terminal.ok => Ok(WorkIntentOutcomeV2::Reserved),
            WorkIntentStatusV2::Succeeded if terminal.ok => Ok(WorkIntentOutcomeV2::Succeeded {
                turn_id: receipt
                    .turn_id
                    .clone()
                    .ok_or(LifecycleErrorV2::ProtocolViolation)?,
            }),
            WorkIntentStatusV2::OutcomeUnknown
                if !terminal.ok
                    && terminal.error_code == Some(super::wire::ErrorCodeV2::OutcomeUnknown) =>
            {
                Ok(WorkIntentOutcomeV2::OutcomeUnknown)
            }
            _ => Err(LifecycleErrorV2::ProtocolViolation),
        }
    }

    /// Restart one granted runtime with a lifetime-monotonic, at-most-once
    /// command identity. A prepared command is retried exactly after response
    /// loss; a returned `outcome_unknown` is never retried automatically.
    pub(crate) async fn restart(
        &self,
        host_id: &str,
        runtime_id: String,
    ) -> Result<RestartOutcomeV2, LifecycleErrorV2> {
        let host_lock = self.host_lock(host_id).await;
        let _operation = host_lock.lock().await;
        let mut entry = self
            .load(host_id)
            .await?
            .ok_or(LifecycleErrorV2::NotEnrolled)?;
        if !matches!(entry.phase, JournalPhaseV2::Enrolled) {
            return Err(LifecycleErrorV2::NotEnrolled);
        }
        let credential = entry
            .credential
            .clone()
            .ok_or(LifecycleErrorV2::JournalCorrupt)?;
        if !credential.selected_runtime_ids.contains(&runtime_id)
            || !credential
                .granted_scopes
                .contains(&DeviceScopeV2::RestartRuntime)
        {
            return Err(LifecycleErrorV2::InvalidSelection);
        }

        let command = match entry.pending_restart.clone() {
            Some(command) if command.runtime_id != runtime_id => {
                return Err(LifecycleErrorV2::OperationInProgress);
            }
            Some(command) if command.disposition == RestartDispositionV2::OutcomeUnknown => {
                return Ok(RestartOutcomeV2::OutcomeUnknown {
                    command_sequence: command.command_sequence,
                });
            }
            Some(command) => command,
            None => {
                let command_sequence = entry
                    .restart_high_watermark
                    .checked_add(1)
                    .ok_or(LifecycleErrorV2::ProtocolViolation)?;
                let command = RestartCommandJournalV2 {
                    runtime_id,
                    idempotency_key: self.entropy.fresh_idempotency_key("restart"),
                    command_sequence,
                    disposition: RestartDispositionV2::Prepared,
                };
                entry.pending_restart = Some(command.clone());
                entry = self
                    .save(Some(&entry), entry.clone_for_next_revision())
                    .await?;
                command
            }
        };

        let request = RequestV2::RestartAgent {
            v: super::wire::PROTOCOL_VERSION,
            credential_id: credential.credential_id,
            client_nonce: self.fresh_nonce(),
            agent: command.runtime_id.clone(),
            idempotency_key: command.idempotency_key.clone(),
            command_sequence: command.command_sequence,
        };
        let started = self.begin_round(&mut entry, &request, false).await?;
        let terminal = self
            .complete_round(&mut entry, &request, started, Some(credential.auth_epoch))
            .await?
            .response;
        let Some(result) = terminal.restart.as_ref() else {
            if terminal.error_code == Some(super::wire::ErrorCodeV2::AgentUnavailable) {
                // The pinned host rejects unavailable runtimes before its
                // durable high watermark advances.
                entry.pending_restart = None;
                self.save(Some(&entry), entry.clone_for_next_revision())
                    .await?;
            }
            return Err(map_terminal_error(&terminal));
        };

        match result.status {
            RestartStatusV2::Succeeded if terminal.ok => {
                entry.restart_high_watermark = command.command_sequence;
                entry.pending_restart = None;
                self.save(Some(&entry), entry.clone_for_next_revision())
                    .await?;
                Ok(RestartOutcomeV2::Succeeded {
                    command_sequence: command.command_sequence,
                })
            }
            RestartStatusV2::OutcomeUnknown
                if !terminal.ok
                    && terminal.error_code == Some(super::wire::ErrorCodeV2::OutcomeUnknown) =>
            {
                entry.restart_high_watermark = command.command_sequence;
                entry.pending_restart = Some(RestartCommandJournalV2 {
                    disposition: RestartDispositionV2::OutcomeUnknown,
                    ..command
                });
                self.save(Some(&entry), entry.clone_for_next_revision())
                    .await?;
                Ok(RestartOutcomeV2::OutcomeUnknown {
                    command_sequence: entry.restart_high_watermark,
                })
            }
            _ => Err(LifecycleErrorV2::ProtocolViolation),
        }
    }

    /// Explicit operator acknowledgement required before issuing a new
    /// lifetime sequence after a terminal ambiguous restart.
    pub(crate) async fn acknowledge_unknown_restart(
        &self,
        host_id: &str,
    ) -> Result<u64, LifecycleErrorV2> {
        let host_lock = self.host_lock(host_id).await;
        let _operation = host_lock.lock().await;
        let mut entry = self
            .load(host_id)
            .await?
            .ok_or(LifecycleErrorV2::NotEnrolled)?;
        let command = entry
            .pending_restart
            .as_ref()
            .filter(|value| value.disposition == RestartDispositionV2::OutcomeUnknown)
            .ok_or(LifecycleErrorV2::OperationInProgress)?;
        let sequence = command.command_sequence;
        entry.pending_restart = None;
        self.save(Some(&entry), entry.clone_for_next_revision())
            .await?;
        Ok(sequence)
    }

    pub(crate) async fn revoke(
        &self,
        host_id: &str,
    ) -> Result<MutationOutcomeV2, LifecycleErrorV2> {
        let host_lock = self.host_lock(host_id).await;
        let _operation = host_lock.lock().await;
        let mut entry = self
            .load(host_id)
            .await?
            .ok_or(LifecycleErrorV2::NotEnrolled)?;
        match entry.phase {
            JournalPhaseV2::RevocationPending => {}
            JournalPhaseV2::Revoked {
                key_cleanup_pending,
            } => {
                if entry.mutation.as_ref().map(|value| value.kind)
                    != Some(MutationKindV2::RevokeSelf)
                {
                    return Err(LifecycleErrorV2::NotEnrolled);
                }
                if key_cleanup_pending {
                    self.finish_revoked_cleanup(&mut entry).await?;
                }
                return Ok(MutationOutcomeV2::Revoked);
            }
            JournalPhaseV2::Enrolled => {
                let credential_id = entry
                    .credential
                    .as_ref()
                    .ok_or(LifecycleErrorV2::JournalCorrupt)?
                    .credential_id
                    .clone();
                entry.mutation = Some(MutationJournalV2 {
                    kind: MutationKindV2::RevokeSelf,
                    credential_id,
                    idempotency_key: self.entropy.fresh_idempotency_key("revocation"),
                    enrollment_idempotency_key: None,
                    receipt: None,
                });
                entry.pending_restart = None;
                entry.phase = JournalPhaseV2::RevocationPending;
                entry = self
                    .save(Some(&entry), entry.clone_for_next_revision())
                    .await?;
            }
            JournalPhaseV2::Quarantined { .. } => {
                return Err(LifecycleErrorV2::Quarantined);
            }
            _ => return Err(LifecycleErrorV2::NotEnrolled),
        }
        self.drive_mutation(&mut entry).await
    }

    pub(crate) async fn cancel_enrollment(
        &self,
        host_id: &str,
    ) -> Result<MutationOutcomeV2, LifecycleErrorV2> {
        let host_lock = self.host_lock(host_id).await;
        let _operation = host_lock.lock().await;
        let mut entry = self
            .load(host_id)
            .await?
            .ok_or(LifecycleErrorV2::InvitationMismatch)?;
        match entry.phase.clone() {
            JournalPhaseV2::RollbackPending => return self.drive_mutation(&mut entry).await,
            JournalPhaseV2::Ready => {
                // Inspection cannot create host-side credential authority.
                // Tear down the local key/journal only; `false` guarantees
                // this path never carries a remote-revocation obligation.
                let outcome = self.forget_locked(&mut entry, false).await?;
                return match outcome {
                    ForgetOutcomeV2::ForgottenLocally { .. }
                    | ForgetOutcomeV2::AlreadyForgotten { .. } => Ok(MutationOutcomeV2::RolledBack),
                };
            }
            JournalPhaseV2::EnrollmentStaged | JournalPhaseV2::EnrollmentPending => {}
            JournalPhaseV2::Revoked {
                key_cleanup_pending,
            } if entry.mutation.as_ref().map(|value| value.kind)
                == Some(MutationKindV2::RollbackEnrollment) =>
            {
                if key_cleanup_pending {
                    self.finish_revoked_cleanup(&mut entry).await?;
                }
                return Ok(MutationOutcomeV2::RolledBack);
            }
            _ => return Err(LifecycleErrorV2::OperationInProgress),
        }
        let enrollment = entry
            .enrollment
            .as_ref()
            .ok_or(LifecycleErrorV2::OperationInProgress)?;
        if enrollment.candidates.is_empty() {
            let outcome = self.forget_locked(&mut entry, false).await?;
            return match outcome {
                ForgetOutcomeV2::ForgottenLocally { .. }
                | ForgetOutcomeV2::AlreadyForgotten { .. } => Ok(MutationOutcomeV2::RolledBack),
            };
        }
        let credential_id = enrollment
            .prospective_credential_id
            .clone()
            .ok_or(LifecycleErrorV2::JournalCorrupt)?;
        entry.mutation = Some(MutationJournalV2 {
            kind: MutationKindV2::RollbackEnrollment,
            credential_id,
            idempotency_key: self.entropy.fresh_idempotency_key("rollback"),
            enrollment_idempotency_key: Some(enrollment.idempotency_key.clone()),
            receipt: None,
        });
        entry.phase = JournalPhaseV2::RollbackPending;
        entry = self
            .save(Some(&entry), entry.clone_for_next_revision())
            .await?;
        self.drive_mutation(&mut entry).await
    }

    pub(crate) async fn forget(&self, host_id: &str) -> Result<ForgetOutcomeV2, LifecycleErrorV2> {
        let host_lock = self.host_lock(host_id).await;
        let _operation = host_lock.lock().await;
        let mut entry = self
            .load(host_id)
            .await?
            .ok_or(LifecycleErrorV2::NotEnrolled)?;
        if let JournalPhaseV2::Forgotten {
            host_revocation_still_required,
        } = entry.phase
        {
            return Ok(ForgetOutcomeV2::AlreadyForgotten {
                host_id: host_id.to_string(),
                host_revocation_still_required,
            });
        }
        let remote_required = host_revocation_still_required(&entry);
        self.forget_locked(&mut entry, remote_required).await
    }

    pub(crate) async fn recover(
        &self,
        host_id: &str,
    ) -> Result<RecoveryOutcomeV2, LifecycleErrorV2> {
        let host_lock = self.host_lock(host_id).await;
        let _operation = host_lock.lock().await;
        let Some(mut entry) = self.load(host_id).await? else {
            return Ok(RecoveryOutcomeV2::NoJournal);
        };
        if matches!(entry.phase, JournalPhaseV2::Enrolled) {
            if let Some(restart) = entry.pending_restart.as_ref() {
                let command_sequence = restart.command_sequence;
                if restart.disposition == RestartDispositionV2::OutcomeUnknown {
                    return Ok(RecoveryOutcomeV2::Restart(
                        RestartOutcomeV2::OutcomeUnknown { command_sequence },
                    ));
                }
                let runtime_id = restart.runtime_id.clone();
                drop(_operation);
                return self
                    .restart(host_id, runtime_id)
                    .await
                    .map(RecoveryOutcomeV2::Restart);
            }
        }
        match entry.phase.clone() {
            JournalPhaseV2::RollbackPending | JournalPhaseV2::RevocationPending => self
                .drive_mutation(&mut entry)
                .await
                .map(RecoveryOutcomeV2::Mutation),
            JournalPhaseV2::Revoked {
                key_cleanup_pending: true,
            } => {
                self.finish_revoked_cleanup(&mut entry).await?;
                Ok(RecoveryOutcomeV2::Mutation(
                    match entry.mutation.as_ref().map(|value| value.kind) {
                        Some(MutationKindV2::RollbackEnrollment) => MutationOutcomeV2::RolledBack,
                        _ => MutationOutcomeV2::Revoked,
                    },
                ))
            }
            JournalPhaseV2::Forgetting {
                host_revocation_still_required,
            } => self
                .forget_locked(&mut entry, host_revocation_still_required)
                .await
                .map(RecoveryOutcomeV2::Forgotten),
            JournalPhaseV2::Inspecting
            | JournalPhaseV2::Ready
            | JournalPhaseV2::EnrollmentStaged
            | JournalPhaseV2::EnrollmentPending => Ok(RecoveryOutcomeV2::NeedsInvitation),
            phase => Ok(RecoveryOutcomeV2::Stable(phase)),
        }
    }

    async fn drive_enrollment(
        &self,
        invite: &V2Invite,
        entry: &mut PairingJournalEntryV2,
    ) -> Result<EnrollmentOutcomeV2, LifecycleErrorV2> {
        let hardware_key = self.load_matching_key(entry).await?;
        let enrollment = entry
            .enrollment
            .as_ref()
            .ok_or(LifecycleErrorV2::JournalCorrupt)?;
        let request = enroll_request(invite, &hardware_key, enrollment, self.fresh_nonce());
        request
            .validate()
            .map_err(|_| LifecycleErrorV2::InvalidSelection)?;
        let mut started = self.begin_round(entry, &request, false).await?;
        let challenge = started
            .challenge_response
            .challenge
            .clone()
            .ok_or(LifecycleErrorV2::ProtocolViolation)?;

        let candidate = enrollment_candidate(entry, &challenge, &request)?;
        let enrollment = entry
            .enrollment
            .as_mut()
            .ok_or(LifecycleErrorV2::JournalCorrupt)?;
        if enrollment.candidates.len() >= MAX_ENROLLMENT_CANDIDATES {
            started.abandon();
            let credential_id = enrollment
                .prospective_credential_id
                .clone()
                .ok_or(LifecycleErrorV2::JournalCorrupt)?;
            self.schedule_confirmation_rollback(entry, &credential_id)
                .await?;
            return Err(LifecycleErrorV2::CandidateLimitReached);
        }
        match enrollment.prospective_credential_id.as_deref() {
            Some(value) if value != candidate.credential_id => {
                started.abandon();
                self.quarantine(entry, QuarantineReasonV2::ClientIdentityDrift)
                    .await?;
                return Err(LifecycleErrorV2::ClientIdentityDrift);
            }
            None => {
                enrollment.prospective_credential_id = Some(candidate.credential_id.clone());
            }
            _ => {}
        }
        enrollment.candidates.push(candidate);
        *entry = match self
            .save(Some(entry), entry.clone_for_next_revision())
            .await
        {
            Ok(saved) => saved,
            Err(error) => {
                started.abandon();
                return Err(error);
            }
        };

        let terminal = self
            .complete_round(entry, &request, started, Some(0))
            .await?
            .response;
        if !terminal.ok {
            return Err(map_terminal_error(&terminal));
        }

        let (confirmation, credential_id) = if let Some(pending) = terminal.pending.as_ref() {
            (
                &pending.enrollment_confirmation,
                pending.credential_id.as_str(),
            )
        } else if let Some(enrolled) = terminal.enrolled.as_ref() {
            (
                &enrolled.enrollment_confirmation,
                enrolled.device_id.as_str(),
            )
        } else {
            self.schedule_current_enrollment_rollback(entry).await?;
            return Err(LifecycleErrorV2::ProtocolViolation);
        };
        let matched = match_confirmation(entry, invite, confirmation);
        let matched = match matched {
            Ok(candidate) => candidate.clone(),
            Err(()) => {
                self.schedule_confirmation_rollback(entry, credential_id)
                    .await?;
                return Err(LifecycleErrorV2::ConfirmationMismatch);
            }
        };

        if terminal.pending.is_some() && invite.confirmation_mode == ConfirmationModeV2::Unattended
        {
            self.schedule_current_enrollment_rollback(entry).await?;
            return Err(LifecycleErrorV2::ProtocolViolation);
        }

        if let Some(pending) = terminal.pending {
            let next_pending = PendingClaimJournalV2 {
                claim_id: pending.claim_id.clone(),
                credential_id: pending.credential_id.clone(),
                display_name: pending.display_name.clone(),
                selected_runtime_ids: pending.selected_runtime_ids.clone(),
                requested_scopes: pending.requested_scopes.clone(),
                transcript_hash: pending.enrollment_confirmation.transcript_hash.clone(),
                sas: pending.enrollment_confirmation.sas.clone(),
                created_at: pending.created_at,
                expires_at: pending.expires_at,
            };
            if entry
                .enrollment
                .as_ref()
                .and_then(|value| value.pending_claim.as_ref())
                .is_some_and(|previous| previous != &next_pending)
            {
                self.schedule_current_enrollment_rollback(entry).await?;
                return Err(LifecycleErrorV2::ProtocolViolation);
            }
            let enrollment = entry
                .enrollment
                .as_mut()
                .ok_or(LifecycleErrorV2::JournalCorrupt)?;
            enrollment.pending_claim = Some(next_pending);
            entry.phase = JournalPhaseV2::EnrollmentPending;
            *entry = self
                .save(Some(entry), entry.clone_for_next_revision())
                .await?;
            return Ok(EnrollmentOutcomeV2::Pending(pending));
        }

        let enrolled = terminal
            .enrolled
            .ok_or(LifecycleErrorV2::ProtocolViolation)?;
        let client_endpoint_id = entry
            .binding
            .client_endpoint_id
            .as_deref()
            .ok_or(LifecycleErrorV2::JournalCorrupt)?;
        if enrolled.endpoint_fingerprint != endpoint_fingerprint(client_endpoint_id) {
            self.schedule_current_enrollment_rollback(entry).await?;
            return Err(LifecycleErrorV2::ClientIdentityDrift);
        }
        if enrolled.device_key_fingerprint
            != device_key_fingerprint(&entry.binding.device_public_key)?
        {
            self.schedule_current_enrollment_rollback(entry).await?;
            return Err(LifecycleErrorV2::HardwareKeyDrift);
        }
        entry.credential = Some(CredentialJournalV2 {
            credential_id: enrolled.device_id.clone(),
            selected_runtime_ids: enrolled.selected_runtime_ids.clone(),
            granted_scopes: enrolled.granted_scopes.clone(),
            auth_epoch: enrolled.auth_epoch,
            created_at: enrolled.created_at,
            endpoint_fingerprint: enrolled.endpoint_fingerprint.clone(),
            device_key_fingerprint: enrolled.device_key_fingerprint.clone(),
            transcript_hash: enrolled.enrollment_confirmation.transcript_hash.clone(),
            sas: enrolled.enrollment_confirmation.sas.clone(),
        });
        if let Some(enrollment) = entry.enrollment.as_mut() {
            enrollment.pending_claim = None;
            // Retain the authenticated candidate, not unrelated timed-out
            // attempts, after the durable credential commit.
            enrollment
                .candidates
                .retain(|value| value.transcript_hash == matched.transcript_hash);
        }
        entry.phase = JournalPhaseV2::Enrolled;
        *entry = self
            .save(Some(entry), entry.clone_for_next_revision())
            .await?;
        Ok(EnrollmentOutcomeV2::Enrolled(enrolled))
    }

    async fn drive_mutation(
        &self,
        entry: &mut PairingJournalEntryV2,
    ) -> Result<MutationOutcomeV2, LifecycleErrorV2> {
        let mutation = entry
            .mutation
            .clone()
            .ok_or(LifecycleErrorV2::JournalCorrupt)?;
        let request = match mutation.kind {
            MutationKindV2::RevokeSelf => RequestV2::RevokeSelf {
                v: super::wire::PROTOCOL_VERSION,
                credential_id: mutation.credential_id.clone(),
                client_nonce: self.fresh_nonce(),
                idempotency_key: mutation.idempotency_key.clone(),
            },
            MutationKindV2::RollbackEnrollment => RequestV2::RollbackEnrollment {
                v: super::wire::PROTOCOL_VERSION,
                credential_id: mutation.credential_id.clone(),
                enrollment_idempotency_key: mutation
                    .enrollment_idempotency_key
                    .clone()
                    .ok_or(LifecycleErrorV2::JournalCorrupt)?,
                client_nonce: self.fresh_nonce(),
                idempotency_key: mutation.idempotency_key.clone(),
            },
        };
        let started = self.begin_round(entry, &request, false).await?;
        let expected_epoch = match mutation.kind {
            // A mutation may already have been applied when its response was
            // lost. The pinned host then challenges at the receipt's incremented
            // epoch. Before a receipt is known, authenticate the pinned host's
            // challenge and validate the returned receipt against the prior
            // credential epoch below; once known, require that exact epoch.
            MutationKindV2::RevokeSelf => {
                mutation.receipt.as_ref().map(|receipt| receipt.auth_epoch)
            }
            MutationKindV2::RollbackEnrollment => None,
        };
        let terminal = self
            .complete_round(entry, &request, started, expected_epoch)
            .await?
            .response;
        let receipt = terminal
            .revocation
            .clone()
            .ok_or_else(|| map_terminal_error(&terminal))?;
        validate_receipt_epoch(entry, mutation.kind, &receipt)?;
        let receipt_journal = receipt_to_journal(&receipt);
        if mutation
            .receipt
            .as_ref()
            .is_some_and(|previous| previous != &receipt_journal)
        {
            return Err(LifecycleErrorV2::ProtocolViolation);
        }
        if let Some(value) = entry.mutation.as_mut() {
            value.receipt = Some(receipt_journal);
        }
        if !terminal.ok {
            if terminal.error_code != Some(super::wire::ErrorCodeV2::OutcomeUnknown) {
                return Err(map_terminal_error(&terminal));
            }
            *entry = self
                .save(Some(entry), entry.clone_for_next_revision())
                .await?;
            return Ok(MutationOutcomeV2::OutcomeUnknown);
        }
        entry.phase = JournalPhaseV2::Revoked {
            key_cleanup_pending: true,
        };
        *entry = self
            .save(Some(entry), entry.clone_for_next_revision())
            .await?;
        self.finish_revoked_cleanup(entry).await?;
        Ok(match mutation.kind {
            MutationKindV2::RevokeSelf => MutationOutcomeV2::Revoked,
            MutationKindV2::RollbackEnrollment => MutationOutcomeV2::RolledBack,
        })
    }

    async fn finish_revoked_cleanup(
        &self,
        entry: &mut PairingJournalEntryV2,
    ) -> Result<(), LifecycleErrorV2> {
        self.host.close_local(&entry.binding.host_id).await;
        match self
            .custody
            .delete_hardware_key(&entry.binding.hardware_key_slot)
            .await
        {
            Ok(()) | Err(CredentialPortError::Missing) => {}
            Err(error) => return Err(error.into()),
        }
        entry.phase = JournalPhaseV2::Revoked {
            key_cleanup_pending: false,
        };
        *entry = self
            .save(Some(entry), entry.clone_for_next_revision())
            .await?;
        Ok(())
    }

    async fn forget_locked(
        &self,
        entry: &mut PairingJournalEntryV2,
        host_revocation_still_required: bool,
    ) -> Result<ForgetOutcomeV2, LifecycleErrorV2> {
        let host_id = entry.binding.host_id.clone();
        entry.phase = JournalPhaseV2::Forgetting {
            host_revocation_still_required,
        };
        *entry = self
            .save(Some(entry), entry.clone_for_next_revision())
            .await?;
        self.host.close_local(&host_id).await;
        match self
            .custody
            .delete_hardware_key(&entry.binding.hardware_key_slot)
            .await
        {
            Ok(()) | Err(CredentialPortError::Missing) => {}
            Err(error) => return Err(error.into()),
        }
        entry.invitation = None;
        entry.enrollment = None;
        entry.credential = None;
        entry.mutation = None;
        entry.restart_high_watermark = 0;
        entry.pending_restart = None;
        entry.phase = JournalPhaseV2::Forgotten {
            host_revocation_still_required,
        };
        *entry = self
            .save(Some(entry), entry.clone_for_next_revision())
            .await?;
        Ok(ForgetOutcomeV2::ForgottenLocally {
            host_id,
            host_revocation_still_required,
        })
    }

    async fn begin_round(
        &self,
        entry: &mut PairingJournalEntryV2,
        request: &RequestV2,
        allow_client_rebind: bool,
    ) -> Result<ExchangeLeaseV2, LifecycleErrorV2> {
        let route = HostRouteV2 {
            node_id: entry.binding.node_id.clone(),
            relay_hint: entry.binding.relay_hint.clone(),
        };
        let started = self.host.start_exchange(&route, request).await?;
        let mut started = ExchangeLeaseV2::new(Arc::clone(&self.host), started);
        if started.exchange_id.is_empty()
            || started.authenticated_host_endpoint_id != entry.binding.node_id
        {
            started.abandon();
            self.quarantine(entry, QuarantineReasonV2::HostIdentityDrift)
                .await?;
            return Err(LifecycleErrorV2::HostIdentityDrift);
        }
        if started.authenticated_client_endpoint_id.is_empty() {
            started.abandon();
            return Err(LifecycleErrorV2::ProtocolViolation);
        }
        if !started.challenge_response.ok && started.challenge_response.challenge.is_none() {
            let error = map_terminal_error(&started.challenge_response);
            started.abandon();
            return Err(error);
        }
        if started
            .challenge_response
            .validate_challenge_for_request(request, &started.authenticated_client_endpoint_id)
            .is_err()
        {
            started.abandon();
            return Err(LifecycleErrorV2::ProtocolViolation);
        }

        let client_changed = entry.binding.client_endpoint_id.as_deref()
            != Some(started.authenticated_client_endpoint_id.as_str());
        if client_changed {
            if entry.binding.client_endpoint_id.is_some() && !allow_client_rebind {
                started.abandon();
                self.quarantine(entry, QuarantineReasonV2::ClientIdentityDrift)
                    .await?;
                return Err(LifecycleErrorV2::ClientIdentityDrift);
            }
            entry.binding.client_endpoint_id =
                Some(started.authenticated_client_endpoint_id.clone());
            *entry = match self
                .save(Some(entry), entry.clone_for_next_revision())
                .await
            {
                Ok(saved) => saved,
                Err(error) => {
                    started.abandon();
                    return Err(error);
                }
            };
        }
        Ok(started)
    }

    async fn complete_round(
        &self,
        entry: &mut PairingJournalEntryV2,
        request: &RequestV2,
        mut started: ExchangeLeaseV2,
        expected_epoch: Option<u64>,
    ) -> Result<FinishedExchangeV2, LifecycleErrorV2> {
        let authenticated_client_endpoint_id = started.authenticated_client_endpoint_id.clone();
        let challenge = started
            .challenge_response
            .challenge
            .clone()
            .ok_or(LifecycleErrorV2::ProtocolViolation)?;
        let expires_at = match u64::try_from(challenge.expires_at) {
            Ok(expires_at) => expires_at,
            Err(_) => {
                started.abandon();
                return Err(LifecycleErrorV2::ProtocolViolation);
            }
        };
        let now = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(now) => now.as_secs(),
            Err(_) => {
                started.abandon();
                return Err(LifecycleErrorV2::ProtocolViolation);
            }
        };
        if expires_at <= now {
            started.abandon();
            return Err(LifecycleErrorV2::ProtocolViolation);
        }
        if expected_epoch.is_some_and(|epoch| challenge.auth_epoch != epoch) {
            started.abandon();
            return Err(LifecycleErrorV2::AuthorizationRequired);
        }
        let hardware_key = match self.load_matching_key(entry).await {
            Ok(key) => key,
            Err(error) => {
                started.abandon();
                return Err(error);
            }
        };
        let transcript = match proof_transcript(
            request,
            &challenge,
            &started.authenticated_host_endpoint_id,
            &started.authenticated_client_endpoint_id,
            &hardware_key,
        ) {
            Ok(transcript) => transcript,
            Err(_) => {
                started.abandon();
                return Err(LifecycleErrorV2::ProtocolViolation);
            }
        };
        let proof = match sign_proof(
            self.custody.as_ref(),
            &hardware_key,
            &challenge.challenge_id,
            &transcript,
        )
        .await
        {
            Ok(proof) => proof,
            Err(error) => {
                started.abandon();
                return Err(error.into());
            }
        };
        let terminal_result = self
            .host
            .finish_exchange(&started.exchange_id, &proof)
            .await;
        // A completed host call has already removed the retained exchange.
        // If the await is cancelled, the still-armed lease above abandons it.
        started.disarm();
        let finished = match terminal_result {
            Ok(finished) => finished,
            Err(error) => {
                if error == HostPortErrorV2::ProtocolViolation
                    && matches!(request, RequestV2::Enroll { .. })
                {
                    self.schedule_current_enrollment_rollback(entry).await?;
                }
                return Err(error.into());
            }
        };
        if finished
            .response
            .validate_terminal_shape_for_request(
                request,
                &challenge,
                &authenticated_client_endpoint_id,
            )
            .is_err()
        {
            if matches!(request, RequestV2::Enroll { .. }) {
                self.schedule_current_enrollment_rollback(entry).await?;
            }
            return Err(LifecycleErrorV2::ProtocolViolation);
        }
        Ok(finished)
    }

    async fn load_matching_key(
        &self,
        entry: &mut PairingJournalEntryV2,
    ) -> Result<HardwareKeyV2, LifecycleErrorV2> {
        let key = match self
            .custody
            .load_hardware_key(&entry.binding.hardware_key_slot)
            .await
        {
            Ok(Some(key)) => key,
            Ok(None) | Err(CredentialPortError::Missing) => {
                self.quarantine(entry, QuarantineReasonV2::HardwareKeyDrift)
                    .await?;
                return Err(LifecycleErrorV2::MissingCredential);
            }
            Err(error) => return Err(error.into()),
        };
        if key.slot != entry.binding.hardware_key_slot
            || key.public_key != entry.binding.device_public_key
        {
            self.quarantine(entry, QuarantineReasonV2::HardwareKeyDrift)
                .await?;
            return Err(LifecycleErrorV2::HardwareKeyDrift);
        }
        Ok(key)
    }

    async fn schedule_confirmation_rollback(
        &self,
        entry: &mut PairingJournalEntryV2,
        credential_id: &str,
    ) -> Result<(), LifecycleErrorV2> {
        let enrollment = entry
            .enrollment
            .as_ref()
            .ok_or(LifecycleErrorV2::JournalCorrupt)?;
        let stable_credential_id = enrollment
            .prospective_credential_id
            .as_deref()
            .ok_or(LifecycleErrorV2::JournalCorrupt)?;
        if stable_credential_id != credential_id {
            return Err(LifecycleErrorV2::ProtocolViolation);
        }
        entry.mutation = Some(MutationJournalV2 {
            kind: MutationKindV2::RollbackEnrollment,
            credential_id: stable_credential_id.to_string(),
            idempotency_key: self.entropy.fresh_idempotency_key("rollback"),
            enrollment_idempotency_key: Some(enrollment.idempotency_key.clone()),
            receipt: None,
        });
        entry.phase = JournalPhaseV2::RollbackPending;
        *entry = self
            .save(Some(entry), entry.clone_for_next_revision())
            .await?;
        Ok(())
    }

    async fn schedule_current_enrollment_rollback(
        &self,
        entry: &mut PairingJournalEntryV2,
    ) -> Result<(), LifecycleErrorV2> {
        let credential_id = entry
            .enrollment
            .as_ref()
            .and_then(|value| value.prospective_credential_id.clone())
            .ok_or(LifecycleErrorV2::JournalCorrupt)?;
        self.schedule_confirmation_rollback(entry, &credential_id)
            .await
    }

    async fn quarantine(
        &self,
        entry: &mut PairingJournalEntryV2,
        reason: QuarantineReasonV2,
    ) -> Result<(), LifecycleErrorV2> {
        entry.phase = JournalPhaseV2::Quarantined { reason };
        *entry = self
            .save(Some(entry), entry.clone_for_next_revision())
            .await?;
        Ok(())
    }

    async fn load(&self, host_id: &str) -> Result<Option<PairingJournalEntryV2>, LifecycleErrorV2> {
        let entry = self.journal.load(host_id).await?;
        if let Some(value) = entry.as_ref() {
            value
                .validate()
                .map_err(|_| LifecycleErrorV2::JournalCorrupt)?;
            if value.binding.host_id != host_id {
                return Err(LifecycleErrorV2::JournalCorrupt);
            }
        }
        Ok(entry)
    }

    async fn save(
        &self,
        previous: Option<&PairingJournalEntryV2>,
        mut replacement: PairingJournalEntryV2,
    ) -> Result<PairingJournalEntryV2, LifecycleErrorV2> {
        replacement.revision = previous
            .map(|value| value.revision.checked_add(1))
            .unwrap_or(Some(1))
            .ok_or(LifecycleErrorV2::JournalCorrupt)?;
        replacement
            .validate()
            .map_err(|_| LifecycleErrorV2::JournalCorrupt)?;
        self.journal
            .compare_and_swap(
                &replacement.binding.host_id,
                previous.map(|value| value.revision),
                replacement.clone(),
            )
            .await?;
        Ok(replacement)
    }

    async fn host_lock(&self, host_id: &str) -> Arc<Mutex<()>> {
        let mut locks = self.host_locks.lock().await;
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(host_id).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(Mutex::new(()));
        locks.insert(host_id.to_string(), Arc::downgrade(&lock));
        lock
    }

    fn fresh_nonce(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.entropy.fresh_nonce())
    }

    fn prepare_existing_inspection(
        &self,
        existing: &PairingJournalEntryV2,
        invite: &V2Invite,
        hardware_key: &HardwareKeyV2,
    ) -> Result<PairingJournalEntryV2, LifecycleErrorV2> {
        match existing.phase {
            JournalPhaseV2::Inspecting | JournalPhaseV2::Ready => {}
            JournalPhaseV2::Forgotten { .. } => {}
            JournalPhaseV2::Quarantined { .. } => return Err(LifecycleErrorV2::Quarantined),
            JournalPhaseV2::Enrolled => return Err(LifecycleErrorV2::OperationInProgress),
            _ => return Err(LifecycleErrorV2::OperationInProgress),
        }
        let mut entry = existing.clone_for_next_revision();
        entry.binding = HostBindingJournalV2 {
            host_id: host_id(invite),
            node_id: invite.node_id.clone(),
            host_display_name: invite.host_name.clone(),
            relay_hint: invite.relay.clone(),
            hardware_key_slot: hardware_key.slot.clone(),
            device_public_key: hardware_key.public_key.clone(),
            client_endpoint_id: None,
        };
        entry.invitation = Some(invitation_from_invite(invite));
        entry.enrollment = None;
        entry.credential = None;
        entry.mutation = None;
        entry.restart_high_watermark = 0;
        entry.pending_restart = None;
        entry.phase = JournalPhaseV2::Inspecting;
        Ok(entry)
    }
}

impl PairingJournalEntryV2 {
    fn clone_for_next_revision(&self) -> Self {
        self.clone()
    }
}

impl From<JournalPortErrorV2> for LifecycleErrorV2 {
    fn from(value: JournalPortErrorV2) -> Self {
        match value {
            JournalPortErrorV2::Unavailable => Self::JournalUnavailable,
            JournalPortErrorV2::Corrupt => Self::JournalCorrupt,
            JournalPortErrorV2::Conflict => Self::JournalConflict,
        }
    }
}

impl From<CredentialPortError> for LifecycleErrorV2 {
    fn from(value: CredentialPortError) -> Self {
        match value {
            CredentialPortError::Unavailable => Self::CredentialUnavailable,
            CredentialPortError::Missing => Self::MissingCredential,
            CredentialPortError::InvalidSignature => Self::InvalidSignature,
        }
    }
}

impl From<HostPortErrorV2> for LifecycleErrorV2 {
    fn from(value: HostPortErrorV2) -> Self {
        match value {
            HostPortErrorV2::Unavailable => Self::HostUnavailable,
            HostPortErrorV2::ProtocolViolation => Self::ProtocolViolation,
        }
    }
}

fn host_id(invite: &V2Invite) -> String {
    format!("remora-link:{}", invite.node_id)
}

fn invitation_from_invite(invite: &V2Invite) -> InvitationJournalV2 {
    InvitationJournalV2 {
        invitation_id: URL_SAFE_NO_PAD.encode(&invite.invitation_id),
        expires_at: invite.expires_at,
        max_runtime_ids: invite.max_runtime_ids.clone(),
        max_scopes: invite.max_scopes.clone(),
        confirmation_mode: invite.confirmation_mode,
        runtime_offers: Vec::new(),
    }
}

struct SecretRequestV2(RequestV2);

impl Deref for SecretRequestV2 {
    type Target = RequestV2;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for SecretRequestV2 {
    fn drop(&mut self) {
        match &mut self.0 {
            RequestV2::InspectInvitation { secret, .. } | RequestV2::Enroll { secret, .. } => {
                secret.zeroize();
            }
            _ => {}
        }
    }
}

fn inspect_request(
    invite: &V2Invite,
    hardware_key: &HardwareKeyV2,
    client_nonce: String,
) -> SecretRequestV2 {
    SecretRequestV2(RequestV2::InspectInvitation {
        v: super::wire::PROTOCOL_VERSION,
        invitation_id: URL_SAFE_NO_PAD.encode(&invite.invitation_id),
        secret: URL_SAFE_NO_PAD.encode(&invite.secret),
        device_public_key: hardware_key.public_key.clone(),
        client_nonce,
    })
}

fn enroll_request(
    invite: &V2Invite,
    hardware_key: &HardwareKeyV2,
    enrollment: &EnrollmentJournalV2,
    client_nonce: String,
) -> SecretRequestV2 {
    SecretRequestV2(RequestV2::Enroll {
        v: super::wire::PROTOCOL_VERSION,
        invitation_id: URL_SAFE_NO_PAD.encode(&invite.invitation_id),
        secret: URL_SAFE_NO_PAD.encode(&invite.secret),
        device_name: enrollment.display_name.clone(),
        device_public_key: hardware_key.public_key.clone(),
        selected_runtime_ids: enrollment.selected_runtime_ids.clone(),
        requested_scopes: enrollment.requested_scopes.clone(),
        idempotency_key: enrollment.idempotency_key.clone(),
        client_nonce,
    })
}

fn ensure_invitation_matches(
    entry: &PairingJournalEntryV2,
    invite: &V2Invite,
) -> Result<(), LifecycleErrorV2> {
    let expected = invitation_from_invite(invite);
    let actual = entry
        .invitation
        .as_ref()
        .ok_or(LifecycleErrorV2::InvitationMismatch)?;
    if entry.binding.node_id != invite.node_id
        || actual.invitation_id != expected.invitation_id
        || actual.expires_at != expected.expires_at
        || actual.max_runtime_ids != expected.max_runtime_ids
        || actual.max_scopes != expected.max_scopes
        || actual.confirmation_mode != expected.confirmation_mode
    {
        return Err(LifecycleErrorV2::InvitationMismatch);
    }
    Ok(())
}

fn inspection_matches_invite(inspection: &InvitationInspectionV2, invite: &V2Invite) -> bool {
    inspection.invitation_id == URL_SAFE_NO_PAD.encode(&invite.invitation_id)
        && inspection.expires_at == invite.expires_at
        && inspection.max_runtime_ids == invite.max_runtime_ids
        && inspection.max_scopes == invite.max_scopes
        && inspection.confirmation_mode == invite.confirmation_mode
}

fn validate_selection(
    entry: &PairingJournalEntryV2,
    selected_runtime_ids: &[String],
    requested_scopes: &[DeviceScopeV2],
) -> Result<(), LifecycleErrorV2> {
    let invitation = entry
        .invitation
        .as_ref()
        .ok_or(LifecycleErrorV2::JournalCorrupt)?;
    validate_policy(
        selected_runtime_ids,
        requested_scopes,
        invitation.confirmation_mode,
    )
    .map_err(|_| LifecycleErrorV2::InvalidSelection)?;
    if !is_subset(selected_runtime_ids, &invitation.max_runtime_ids)
        || !scope_subset(requested_scopes, &invitation.max_scopes)
    {
        return Err(LifecycleErrorV2::InvalidSelection);
    }
    Ok(())
}

fn canonicalize_selection(
    mut selected_runtime_ids: Vec<String>,
    mut requested_scopes: Vec<DeviceScopeV2>,
) -> Result<(Vec<String>, Vec<DeviceScopeV2>), LifecycleErrorV2> {
    selected_runtime_ids.sort();
    requested_scopes.sort();
    if selected_runtime_ids
        .windows(2)
        .any(|pair| pair[0] == pair[1])
        || requested_scopes.windows(2).any(|pair| pair[0] == pair[1])
    {
        return Err(LifecycleErrorV2::InvalidSelection);
    }
    Ok((selected_runtime_ids, requested_scopes))
}

fn enrollment_candidate(
    entry: &PairingJournalEntryV2,
    challenge: &super::wire::ProofChallengeV2,
    request: &RequestV2,
) -> Result<EnrollmentCandidateJournalV2, LifecycleErrorV2> {
    let invitation = entry
        .invitation
        .as_ref()
        .ok_or(LifecycleErrorV2::JournalCorrupt)?;
    let enrollment = entry
        .enrollment
        .as_ref()
        .ok_or(LifecycleErrorV2::JournalCorrupt)?;
    let candidate = EnrollmentCandidateJournalV2 {
        host_endpoint_id: entry.binding.node_id.clone(),
        client_endpoint_id: entry
            .binding
            .client_endpoint_id
            .clone()
            .ok_or(LifecycleErrorV2::JournalCorrupt)?,
        credential_id: challenge.credential_id.clone(),
        invitation_id: invitation.invitation_id.clone(),
        device_public_key: entry.binding.device_public_key.clone(),
        enrollment_idempotency_key: enrollment.idempotency_key.clone(),
        selected_runtime_ids: enrollment.selected_runtime_ids.clone(),
        requested_scopes: enrollment.requested_scopes.clone(),
        server_nonce: challenge.server_nonce.clone(),
        client_nonce: request.client_nonce().to_string(),
        confirmation_mode: invitation.confirmation_mode,
        max_runtime_ids: invitation.max_runtime_ids.clone(),
        max_scopes: invitation.max_scopes.clone(),
        transcript_hash: String::new(),
    };
    let hash = candidate
        .recompute_hash()
        .map_err(|_| LifecycleErrorV2::ProtocolViolation)?;
    Ok(EnrollmentCandidateJournalV2 {
        transcript_hash: URL_SAFE_NO_PAD.encode(hash),
        ..candidate
    })
}

fn match_confirmation<'a>(
    entry: &'a PairingJournalEntryV2,
    invite: &V2Invite,
    confirmation: &EnrollmentConfirmationV2,
) -> Result<&'a EnrollmentCandidateJournalV2, ()> {
    let enrollment = entry.enrollment.as_ref().ok_or(())?;
    let matches = enrollment
        .candidates
        .iter()
        .filter(|candidate| candidate.transcript_hash == confirmation.transcript_hash)
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err(());
    }
    let candidate = matches[0];
    let hash = candidate.recompute_hash().map_err(|_| ())?;
    let secret = Zeroizing::new(URL_SAFE_NO_PAD.encode(&invite.secret));
    let expected_sas = derive_sas(&secret, &hash).map_err(|_| ())?;
    if expected_sas != confirmation.sas {
        return Err(());
    }
    Ok(candidate)
}

fn validate_receipt_epoch(
    entry: &PairingJournalEntryV2,
    kind: MutationKindV2,
    receipt: &RevocationReceiptV2,
) -> Result<(), LifecycleErrorV2> {
    let previous = match kind {
        MutationKindV2::RevokeSelf => {
            entry
                .credential
                .as_ref()
                .ok_or(LifecycleErrorV2::JournalCorrupt)?
                .auth_epoch
        }
        MutationKindV2::RollbackEnrollment => entry
            .credential
            .as_ref()
            .map_or(0, |credential| credential.auth_epoch),
    };
    if previous
        .checked_add(1)
        .is_none_or(|expected| receipt.auth_epoch != expected)
    {
        return Err(LifecycleErrorV2::ProtocolViolation);
    }
    Ok(())
}

fn receipt_to_journal(receipt: &RevocationReceiptV2) -> RevocationReceiptJournalV2 {
    RevocationReceiptJournalV2 {
        credential_id: receipt.credential_id.clone(),
        auth_epoch: receipt.auth_epoch,
        revoked_at: receipt.revoked_at,
        idempotency_key: receipt.idempotency_key.clone(),
    }
}

fn endpoint_fingerprint(endpoint_id: &str) -> String {
    hex::encode(&Sha256::digest(endpoint_id.as_bytes())[..8])
}

fn device_key_fingerprint(device_public_key: &str) -> Result<String, LifecycleErrorV2> {
    let mut bytes = URL_SAFE_NO_PAD
        .decode(device_public_key)
        .map_err(|_| LifecycleErrorV2::JournalCorrupt)?;
    let fingerprint = hex::encode(&Sha256::digest(&bytes)[..8]);
    bytes.zeroize();
    Ok(fingerprint)
}

fn map_terminal_error(response: &ResponseV2) -> LifecycleErrorV2 {
    match response.error_code {
        Some(super::wire::ErrorCodeV2::PairingUnavailable) => LifecycleErrorV2::PairingUnavailable,
        Some(super::wire::ErrorCodeV2::AuthorizationRequired) => {
            LifecycleErrorV2::AuthorizationRequired
        }
        Some(super::wire::ErrorCodeV2::AgentUnavailable) => LifecycleErrorV2::AgentUnavailable,
        Some(super::wire::ErrorCodeV2::OutcomeUnknown) => LifecycleErrorV2::OutcomeUnknown,
        Some(super::wire::ErrorCodeV2::ThreadBindingRejected) => LifecycleErrorV2::InvalidSelection,
        Some(super::wire::ErrorCodeV2::WorkIntentRejected) => LifecycleErrorV2::InvalidSelection,
        Some(super::wire::ErrorCodeV2::InvalidRequest)
        | Some(super::wire::ErrorCodeV2::Internal)
        | None => LifecycleErrorV2::ProtocolViolation,
    }
}

fn host_revocation_still_required(entry: &PairingJournalEntryV2) -> bool {
    match entry.phase {
        JournalPhaseV2::Revoked { .. } | JournalPhaseV2::Forgotten { .. } => false,
        JournalPhaseV2::Forgetting {
            host_revocation_still_required,
        } => host_revocation_still_required,
        _ => {
            entry.credential.is_some()
                || entry.mutation.is_some()
                || entry.enrollment.as_ref().is_some_and(|value| {
                    value.pending_claim.is_some() || !value.candidates.is_empty()
                })
        }
    }
}

#[allow(dead_code)]
fn _wire_error_is_closed(_: WireError) {}
