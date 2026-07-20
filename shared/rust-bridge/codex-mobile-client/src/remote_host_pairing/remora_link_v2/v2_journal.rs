//! Durable, nonsecret Remora Link v2 lifecycle journal.
//!
//! The journal contains enough public metadata to replay or compensate every
//! ambiguous operation, but it can never contain an invitation secret,
//! private key, proof signature, or portable credential secret. The only
//! credential authority is the platform hardware-key slot plus the host's
//! authoritative device record.

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use p256::PublicKey;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::wire::{
    ConfirmationModeV2, DeviceScopeV2, EnrollmentTranscriptInput, RuntimeOfferV2,
    enrollment_transcript_hash, validate_policy,
};

pub(crate) const JOURNAL_SCHEMA_VERSION: u32 = 1;
pub(super) const MAX_ENROLLMENT_CANDIDATES: usize = 64;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum QuarantineReasonV2 {
    HostIdentityDrift,
    ClientIdentityDrift,
    HardwareKeyDrift,
    PolicyDrift,
    ConfirmationMismatch,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum JournalPhaseV2 {
    Inspecting,
    Ready,
    EnrollmentStaged,
    EnrollmentPending,
    Enrolled,
    RollbackPending,
    RevocationPending,
    Revoked {
        key_cleanup_pending: bool,
    },
    Forgetting {
        host_revocation_still_required: bool,
    },
    Forgotten {
        host_revocation_still_required: bool,
    },
    Quarantined {
        reason: QuarantineReasonV2,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct HostBindingJournalV2 {
    pub(crate) host_id: String,
    pub(crate) node_id: String,
    /// Invitation-provided display label. It is never identity or authority;
    /// the pinned Iroh endpoint remains the host identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) host_display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) relay_hint: Option<String>,
    pub(crate) hardware_key_slot: String,
    pub(crate) device_public_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) client_endpoint_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct InvitationJournalV2 {
    pub(crate) invitation_id: String,
    pub(crate) expires_at: i64,
    pub(crate) max_runtime_ids: Vec<String>,
    pub(crate) max_scopes: Vec<DeviceScopeV2>,
    pub(crate) confirmation_mode: ConfirmationModeV2,
    #[serde(default)]
    pub(crate) runtime_offers: Vec<RuntimeOfferV2>,
}

/// One complete, public enrollment-confirmation transcript candidate.
///
/// Every field required to reproduce the transcript hash is copied here on
/// purpose. Recovery must not silently combine values from different journal
/// revisions after a crash.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct EnrollmentCandidateJournalV2 {
    pub(crate) host_endpoint_id: String,
    pub(crate) client_endpoint_id: String,
    pub(crate) credential_id: String,
    pub(crate) invitation_id: String,
    pub(crate) device_public_key: String,
    pub(crate) enrollment_idempotency_key: String,
    pub(crate) selected_runtime_ids: Vec<String>,
    pub(crate) requested_scopes: Vec<DeviceScopeV2>,
    pub(crate) server_nonce: String,
    pub(crate) client_nonce: String,
    pub(crate) confirmation_mode: ConfirmationModeV2,
    pub(crate) max_runtime_ids: Vec<String>,
    pub(crate) max_scopes: Vec<DeviceScopeV2>,
    /// Canonical base64url SHA-256, persisted so matching does not depend on
    /// whichever attempt happens to be newest after recovery.
    pub(crate) transcript_hash: String,
}

impl EnrollmentCandidateJournalV2 {
    pub(super) fn recompute_hash(&self) -> Result<[u8; 32], JournalValidationError> {
        enrollment_transcript_hash(EnrollmentTranscriptInput {
            host_endpoint_id: &self.host_endpoint_id,
            client_endpoint_id: &self.client_endpoint_id,
            invitation_id: &self.invitation_id,
            device_public_key: &self.device_public_key,
            idempotency_key: &self.enrollment_idempotency_key,
            selected_runtime_ids: &self.selected_runtime_ids,
            requested_scopes: &self.requested_scopes,
            server_nonce: &self.server_nonce,
            client_nonce: &self.client_nonce,
            confirmation_mode: self.confirmation_mode,
            max_runtime_ids: &self.max_runtime_ids,
            max_scopes: &self.max_scopes,
        })
        .map_err(|_| JournalValidationError::Corrupt)
    }

    pub(super) fn validate(&self) -> Result<(), JournalValidationError> {
        let expected = URL_SAFE_NO_PAD.encode(self.recompute_hash()?);
        if expected != self.transcript_hash {
            return Err(JournalValidationError::Corrupt);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct PendingClaimJournalV2 {
    pub(crate) claim_id: String,
    pub(crate) credential_id: String,
    pub(crate) display_name: String,
    pub(crate) selected_runtime_ids: Vec<String>,
    pub(crate) requested_scopes: Vec<DeviceScopeV2>,
    pub(crate) transcript_hash: String,
    pub(crate) sas: String,
    pub(crate) created_at: i64,
    pub(crate) expires_at: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct EnrollmentJournalV2 {
    pub(crate) display_name: String,
    pub(crate) selected_runtime_ids: Vec<String>,
    pub(crate) requested_scopes: Vec<DeviceScopeV2>,
    pub(crate) idempotency_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) prospective_credential_id: Option<String>,
    #[serde(default)]
    pub(crate) candidates: Vec<EnrollmentCandidateJournalV2>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) pending_claim: Option<PendingClaimJournalV2>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct CredentialJournalV2 {
    pub(crate) credential_id: String,
    pub(crate) selected_runtime_ids: Vec<String>,
    pub(crate) granted_scopes: Vec<DeviceScopeV2>,
    pub(crate) auth_epoch: u64,
    pub(crate) created_at: i64,
    pub(crate) endpoint_fingerprint: String,
    pub(crate) device_key_fingerprint: String,
    pub(crate) transcript_hash: String,
    pub(crate) sas: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MutationKindV2 {
    RollbackEnrollment,
    RevokeSelf,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MutationJournalV2 {
    pub(crate) kind: MutationKindV2,
    pub(crate) credential_id: String,
    pub(crate) idempotency_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) enrollment_idempotency_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) receipt: Option<RevocationReceiptJournalV2>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RevocationReceiptJournalV2 {
    pub(crate) credential_id: String,
    pub(crate) auth_epoch: u64,
    pub(crate) revoked_at: i64,
    pub(crate) idempotency_key: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RestartDispositionV2 {
    /// Persisted before the first network attempt. Exact retries reuse the
    /// same sequence and idempotency key.
    Prepared,
    /// The host cannot prove whether dispatch completed. Automatic retries
    /// stop until the operator explicitly acknowledges this result.
    OutcomeUnknown,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RestartCommandJournalV2 {
    pub(crate) runtime_id: String,
    pub(crate) idempotency_key: String,
    pub(crate) command_sequence: u64,
    pub(crate) disposition: RestartDispositionV2,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct PairingJournalEntryV2 {
    pub(crate) schema_version: u32,
    pub(crate) revision: u64,
    pub(crate) binding: HostBindingJournalV2,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) invitation: Option<InvitationJournalV2>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) enrollment: Option<EnrollmentJournalV2>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) credential: Option<CredentialJournalV2>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) mutation: Option<MutationJournalV2>,
    #[serde(default)]
    pub(crate) restart_high_watermark: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) pending_restart: Option<RestartCommandJournalV2>,
    pub(crate) phase: JournalPhaseV2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum JournalValidationError {
    #[error("Remora Link v2 journal is corrupt or unsupported")]
    Corrupt,
}

impl PairingJournalEntryV2 {
    pub(crate) fn validate(&self) -> Result<(), JournalValidationError> {
        if self.schema_version != JOURNAL_SCHEMA_VERSION
            || self.binding.host_id != format!("remora-link:{}", self.binding.node_id)
            || !valid_text(&self.binding.node_id, 256)
            || self
                .binding
                .host_display_name
                .as_deref()
                .is_some_and(|value| !valid_text(value, 255))
            || !valid_text(&self.binding.hardware_key_slot, 256)
            || self
                .binding
                .client_endpoint_id
                .as_deref()
                .is_some_and(|value| !valid_text(value, 256))
            || self
                .binding
                .relay_hint
                .as_deref()
                .is_some_and(|value| !valid_text(value, 2048))
            || !valid_public_key(&self.binding.device_public_key)
        {
            return Err(JournalValidationError::Corrupt);
        }

        if let Some(invitation) = &self.invitation {
            validate_policy(
                &invitation.max_runtime_ids,
                &invitation.max_scopes,
                invitation.confirmation_mode,
            )
            .map_err(|_| JournalValidationError::Corrupt)?;
            if !valid_opaque_16(&invitation.invitation_id) || invitation.expires_at < 0 {
                return Err(JournalValidationError::Corrupt);
            }
            let mut seen = std::collections::HashSet::new();
            if invitation.runtime_offers.iter().any(|offer| {
                offer.validate().is_err()
                    || !invitation.max_runtime_ids.contains(&offer.runtime_id)
                    || !seen.insert(offer.runtime_id.as_str())
            }) {
                return Err(JournalValidationError::Corrupt);
            }
        }

        if let Some(enrollment) = &self.enrollment {
            if enrollment.candidates.len() > MAX_ENROLLMENT_CANDIDATES
                || enrollment.display_name.len() > 80
                || enrollment.display_name.chars().any(char::is_control)
                || !valid_idempotency(&enrollment.idempotency_key)
            {
                return Err(JournalValidationError::Corrupt);
            }
            validate_policy(
                &enrollment.selected_runtime_ids,
                &enrollment.requested_scopes,
                ConfirmationModeV2::Interactive,
            )
            .map_err(|_| JournalValidationError::Corrupt)?;
            let invitation = self
                .invitation
                .as_ref()
                .ok_or(JournalValidationError::Corrupt)?;
            if !is_subset(
                &enrollment.selected_runtime_ids,
                &invitation.max_runtime_ids,
            ) || !scope_subset(&enrollment.requested_scopes, &invitation.max_scopes)
            {
                return Err(JournalValidationError::Corrupt);
            }
            for candidate in &enrollment.candidates {
                candidate.validate()?;
                if candidate.host_endpoint_id != self.binding.node_id
                    || candidate.invitation_id != invitation.invitation_id
                    || candidate.device_public_key != self.binding.device_public_key
                    || candidate.enrollment_idempotency_key != enrollment.idempotency_key
                    || candidate.selected_runtime_ids != enrollment.selected_runtime_ids
                    || candidate.requested_scopes != enrollment.requested_scopes
                    || candidate.confirmation_mode != invitation.confirmation_mode
                    || candidate.max_runtime_ids != invitation.max_runtime_ids
                    || candidate.max_scopes != invitation.max_scopes
                    || self.binding.client_endpoint_id.as_deref()
                        != Some(candidate.client_endpoint_id.as_str())
                    || enrollment.prospective_credential_id.as_deref()
                        != Some(candidate.credential_id.as_str())
                {
                    return Err(JournalValidationError::Corrupt);
                }
            }
            if let Some(pending) = &enrollment.pending_claim {
                if enrollment.prospective_credential_id.as_deref()
                    != Some(pending.credential_id.as_str())
                    || !valid_opaque_16(&pending.claim_id)
                    || pending.display_name != normalize_device_name(&enrollment.display_name)
                    || pending.selected_runtime_ids != enrollment.selected_runtime_ids
                    || pending.requested_scopes != enrollment.requested_scopes
                    || pending.expires_at < pending.created_at
                    || invitation.confirmation_mode != ConfirmationModeV2::Interactive
                    || !valid_sas(&pending.sas)
                    || !enrollment
                        .candidates
                        .iter()
                        .filter(|candidate| candidate.transcript_hash == pending.transcript_hash)
                        .count()
                        .eq(&1)
                {
                    return Err(JournalValidationError::Corrupt);
                }
            }
        }

        if let Some(credential) = &self.credential {
            validate_policy(
                &credential.selected_runtime_ids,
                &credential.granted_scopes,
                ConfirmationModeV2::Interactive,
            )
            .map_err(|_| JournalValidationError::Corrupt)?;
            let enrollment = self
                .enrollment
                .as_ref()
                .ok_or(JournalValidationError::Corrupt)?;
            let client_endpoint_id = self
                .binding
                .client_endpoint_id
                .as_deref()
                .ok_or(JournalValidationError::Corrupt)?;
            if !valid_opaque_16(&credential.credential_id)
                || enrollment.prospective_credential_id.as_deref()
                    != Some(credential.credential_id.as_str())
                || credential.endpoint_fingerprint != endpoint_fingerprint(client_endpoint_id)
                || credential.device_key_fingerprint
                    != device_key_fingerprint(&self.binding.device_public_key)
                || !is_subset(
                    &credential.selected_runtime_ids,
                    &enrollment.selected_runtime_ids,
                )
                || !scope_subset(&credential.granted_scopes, &enrollment.requested_scopes)
                || !valid_sas(&credential.sas)
                || enrollment
                    .candidates
                    .iter()
                    .filter(|candidate| candidate.transcript_hash == credential.transcript_hash)
                    .count()
                    != 1
            {
                return Err(JournalValidationError::Corrupt);
            }
        }

        if let Some(mutation) = &self.mutation {
            if !valid_opaque_16(&mutation.credential_id)
                || !valid_idempotency(&mutation.idempotency_key)
                || mutation.receipt.as_ref().is_some_and(|receipt| {
                    receipt.credential_id != mutation.credential_id
                        || receipt.idempotency_key != mutation.idempotency_key
                        || receipt.auth_epoch == 0
                })
                || match mutation.kind {
                    MutationKindV2::RollbackEnrollment => mutation
                        .enrollment_idempotency_key
                        .as_deref()
                        .is_none_or(|value| !valid_idempotency(value)),
                    MutationKindV2::RevokeSelf => mutation.enrollment_idempotency_key.is_some(),
                }
            {
                return Err(JournalValidationError::Corrupt);
            }
        }

        if let Some(restart) = &self.pending_restart {
            let credential = self
                .credential
                .as_ref()
                .ok_or(JournalValidationError::Corrupt)?;
            if !valid_idempotency(&restart.idempotency_key)
                || restart.command_sequence == 0
                || !credential
                    .selected_runtime_ids
                    .contains(&restart.runtime_id)
                || !credential
                    .granted_scopes
                    .contains(&DeviceScopeV2::RestartRuntime)
                || match restart.disposition {
                    RestartDispositionV2::Prepared => self
                        .restart_high_watermark
                        .checked_add(1)
                        .is_none_or(|next| restart.command_sequence != next),
                    RestartDispositionV2::OutcomeUnknown => {
                        restart.command_sequence != self.restart_high_watermark
                    }
                }
            {
                return Err(JournalValidationError::Corrupt);
            }
        }
        if self.pending_restart.is_some()
            && !matches!(
                self.phase,
                JournalPhaseV2::Enrolled | JournalPhaseV2::Quarantined { .. }
            )
        {
            return Err(JournalValidationError::Corrupt);
        }

        let phase_valid = match self.phase {
            JournalPhaseV2::Inspecting | JournalPhaseV2::Ready => {
                self.invitation.is_some()
                    && self.enrollment.is_none()
                    && self.credential.is_none()
                    && self.mutation.is_none()
            }
            JournalPhaseV2::EnrollmentStaged => {
                self.invitation.is_some()
                    && self.enrollment.is_some()
                    && self.credential.is_none()
                    && self.mutation.is_none()
            }
            JournalPhaseV2::EnrollmentPending => {
                self.enrollment
                    .as_ref()
                    .is_some_and(|value| value.pending_claim.is_some())
                    && self.credential.is_none()
                    && self.mutation.is_none()
            }
            JournalPhaseV2::Enrolled => self.credential.is_some() && self.mutation.is_none(),
            JournalPhaseV2::RollbackPending => self
                .mutation
                .as_ref()
                .is_some_and(|value| value.kind == MutationKindV2::RollbackEnrollment),
            JournalPhaseV2::RevocationPending => {
                self.mutation
                    .as_ref()
                    .is_some_and(|value| value.kind == MutationKindV2::RevokeSelf)
                    && self.credential.is_some()
            }
            JournalPhaseV2::Revoked { .. } => self
                .mutation
                .as_ref()
                .is_some_and(|value| value.receipt.is_some()),
            JournalPhaseV2::Forgetting { .. } => true,
            JournalPhaseV2::Forgotten { .. } => {
                self.invitation.is_none()
                    && self.enrollment.is_none()
                    && self.credential.is_none()
                    && self.mutation.is_none()
                    && self.pending_restart.is_none()
            }
            JournalPhaseV2::Quarantined { .. } => true,
        };
        phase_valid
            .then_some(())
            .ok_or(JournalValidationError::Corrupt)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum JournalPortErrorV2 {
    #[error("Remora Link v2 journal is unavailable")]
    Unavailable,
    #[error("Remora Link v2 journal is corrupt")]
    Corrupt,
    #[error("Remora Link v2 journal changed concurrently")]
    Conflict,
}

#[async_trait]
pub(crate) trait JournalPortV2: Send + Sync {
    async fn load(
        &self,
        host_id: &str,
    ) -> Result<Option<PairingJournalEntryV2>, JournalPortErrorV2>;

    async fn compare_and_swap(
        &self,
        host_id: &str,
        expected_revision: Option<u64>,
        replacement: PairingJournalEntryV2,
    ) -> Result<(), JournalPortErrorV2>;
}

fn valid_text(value: &str, max_bytes: usize) -> bool {
    !value.is_empty() && value.len() <= max_bytes && !value.chars().any(char::is_control)
}

fn valid_idempotency(value: &str) -> bool {
    valid_text(value, 128)
}

fn valid_opaque_16(value: &str) -> bool {
    if value.len() != 22 || value.contains('=') {
        return false;
    }
    URL_SAFE_NO_PAD
        .decode(value)
        .is_ok_and(|bytes| bytes.len() == 16 && URL_SAFE_NO_PAD.encode(bytes) == value)
}

fn valid_public_key(value: &str) -> bool {
    if value.len() > 128 || value.contains('=') {
        return false;
    }
    URL_SAFE_NO_PAD.decode(value).is_ok_and(|bytes| {
        bytes.len() == 65
            && bytes.first() == Some(&4)
            && PublicKey::from_sec1_bytes(&bytes).is_ok()
            && URL_SAFE_NO_PAD.encode(bytes) == value
    })
}

fn valid_sas(value: &str) -> bool {
    const CROCKFORD: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let bytes = value.as_bytes();
    bytes.len() == 7
        && bytes[3] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 3 || CROCKFORD.contains(byte))
}

fn endpoint_fingerprint(endpoint_id: &str) -> String {
    hex::encode(&Sha256::digest(endpoint_id.as_bytes())[..8])
}

fn device_key_fingerprint(device_public_key: &str) -> String {
    URL_SAFE_NO_PAD
        .decode(device_public_key)
        .map(|bytes| hex::encode(&Sha256::digest(bytes)[..8]))
        .unwrap_or_default()
}

fn normalize_device_name(value: &str) -> String {
    let normalized: String = value
        .trim()
        .chars()
        .filter(|value| !value.is_control())
        .take(80)
        .collect();
    if normalized.is_empty() {
        "Remora device".to_string()
    } else {
        normalized
    }
}

pub(super) fn is_subset(selected: &[String], maximum: &[String]) -> bool {
    selected
        .iter()
        .all(|value| maximum.binary_search(value).is_ok())
}

pub(super) fn scope_subset(selected: &[DeviceScopeV2], maximum: &[DeviceScopeV2]) -> bool {
    selected.iter().all(|value| maximum.contains(value))
}
