//! Deterministic Remora Link v2 JSON and transcript codec.

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use p256::ecdsa::signature::Verifier as _;
use p256::ecdsa::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

pub(crate) const PROTOCOL_VERSION: u32 = 2;
pub(crate) const ALPN: &[u8] = b"remora-link/2";
pub(crate) const PROOF_DOMAIN: &[u8] = b"remora-link/2/proof/v2";
pub(crate) const PAYLOAD_DOMAIN: &[u8] = b"remora-link/2/payload/v2";
pub(crate) const ENROLLMENT_DOMAIN: &[u8] = b"remora-link/2/enrollment/v2";
pub(crate) const POLICY_DOMAIN: &[u8] = b"remora-link/2/policy/v2";
pub(crate) const PROSPECTIVE_CREDENTIAL_DOMAIN: &[u8] = b"remora-link/2/prospective-credential/v2";
pub(crate) const SAS_DOMAIN: &[u8] = b"remora-link/2/sas/v2";
const MAX_RUNTIME_IDS: usize = 16;
const MAX_RUNTIME_ID_BYTES: usize = 64;
const MAX_IDEMPOTENCY_BYTES: usize = 128;
const MAX_DEVICE_NAME_BYTES: usize = 80;
const MAX_ENDPOINT_ID_BYTES: usize = 256;
const MAX_PUBLIC_KEY_TEXT: usize = 128;
const MAX_SIGNATURE_TEXT: usize = 128;
const OPAQUE_ID_BYTES: usize = 16;
const NONCE_BYTES: usize = 32;
const INVITATION_SECRET_BYTES: usize = 32;
const DEVICE_PUBLIC_KEY_BYTES: usize = 65;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DeviceScopeV2 {
    InspectRuntimes,
    ConnectRuntime,
    RestartRuntime,
    SelfRevoke,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ConfirmationModeV2 {
    Interactive,
    Unattended,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResumeV2 {
    pub(crate) last_seq: u64,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum RequestV2 {
    InspectInvitation {
        v: u32,
        invitation_id: String,
        secret: String,
        device_public_key: String,
        client_nonce: String,
    },
    Enroll {
        v: u32,
        invitation_id: String,
        secret: String,
        #[serde(default)]
        device_name: String,
        device_public_key: String,
        selected_runtime_ids: Vec<String>,
        requested_scopes: Vec<DeviceScopeV2>,
        idempotency_key: String,
        client_nonce: String,
    },
    ListAgents {
        v: u32,
        credential_id: String,
        client_nonce: String,
    },
    RestartAgent {
        v: u32,
        credential_id: String,
        client_nonce: String,
        agent: String,
        idempotency_key: String,
        command_sequence: u64,
    },
    Connect {
        v: u32,
        credential_id: String,
        client_nonce: String,
        agent: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resume: Option<ResumeV2>,
    },
    RevokeSelf {
        v: u32,
        credential_id: String,
        client_nonce: String,
        idempotency_key: String,
    },
    RollbackEnrollment {
        v: u32,
        credential_id: String,
        enrollment_idempotency_key: String,
        client_nonce: String,
        idempotency_key: String,
    },
}

impl Drop for RequestV2 {
    fn drop(&mut self) {
        match self {
            Self::InspectInvitation { secret, .. } | Self::Enroll { secret, .. } => {
                secret.zeroize();
            }
            _ => {}
        }
    }
}

impl fmt::Debug for RequestV2 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct(self.operation())
            .field("v", &self.version())
            .field("credential_id", &self.credential_id())
            .field("client_nonce", &"<redacted>")
            .finish_non_exhaustive()
    }
}

/// Field-only correlation for the second half of an already transmitted
/// control exchange. This type intentionally has no invitation-secret field,
/// so no valid bearer value can survive in a retained exchange.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RequestCorrelationV2 {
    InspectInvitation {
        invitation_id: String,
    },
    Enroll {
        invitation_id: String,
        device_public_key: String,
        selected_runtime_ids: Vec<String>,
        requested_scopes: Vec<DeviceScopeV2>,
        idempotency_key: String,
    },
    ListAgents {
        credential_id: String,
    },
    RestartAgent {
        credential_id: String,
        agent: String,
        idempotency_key: String,
        command_sequence: u64,
    },
    Connect {
        credential_id: String,
        agent: String,
    },
    RevokeSelf {
        credential_id: String,
        idempotency_key: String,
    },
    RollbackEnrollment {
        credential_id: String,
        enrollment_idempotency_key: String,
        idempotency_key: String,
    },
}

impl RequestV2 {
    pub(crate) fn decode_json(bytes: &[u8]) -> Result<Self, WireError> {
        let request: Self = serde_json::from_slice(bytes).map_err(|_| WireError::InvalidRequest)?;
        request.validate()?;
        Ok(request)
    }

    pub(crate) fn operation(&self) -> &'static str {
        match self {
            Self::InspectInvitation { .. } => "inspect_invitation",
            Self::Enroll { .. } => "enroll",
            Self::ListAgents { .. } => "list_agents",
            Self::RestartAgent { .. } => "restart_agent",
            Self::Connect { .. } => "connect",
            Self::RevokeSelf { .. } => "revoke_self",
            Self::RollbackEnrollment { .. } => "rollback_enrollment",
        }
    }
    pub(crate) fn version(&self) -> u32 {
        match self {
            Self::InspectInvitation { v, .. }
            | Self::Enroll { v, .. }
            | Self::ListAgents { v, .. }
            | Self::RestartAgent { v, .. }
            | Self::Connect { v, .. }
            | Self::RevokeSelf { v, .. }
            | Self::RollbackEnrollment { v, .. } => *v,
        }
    }
    pub(crate) fn client_nonce(&self) -> &str {
        match self {
            Self::InspectInvitation { client_nonce, .. }
            | Self::Enroll { client_nonce, .. }
            | Self::ListAgents { client_nonce, .. }
            | Self::RestartAgent { client_nonce, .. }
            | Self::Connect { client_nonce, .. }
            | Self::RevokeSelf { client_nonce, .. }
            | Self::RollbackEnrollment { client_nonce, .. } => client_nonce,
        }
    }
    pub(crate) fn credential_id(&self) -> Option<&str> {
        match self {
            Self::InspectInvitation { .. } | Self::Enroll { .. } => None,
            Self::ListAgents { credential_id, .. }
            | Self::RestartAgent { credential_id, .. }
            | Self::Connect { credential_id, .. }
            | Self::RevokeSelf { credential_id, .. }
            | Self::RollbackEnrollment { credential_id, .. } => Some(credential_id),
        }
    }

    pub(crate) fn terminal_correlation(&self) -> Result<RequestCorrelationV2, WireError> {
        self.validate()?;
        Ok(match self {
            Self::InspectInvitation { invitation_id, .. } => {
                RequestCorrelationV2::InspectInvitation {
                    invitation_id: invitation_id.clone(),
                }
            }
            Self::Enroll {
                invitation_id,
                device_public_key,
                selected_runtime_ids,
                requested_scopes,
                idempotency_key,
                ..
            } => RequestCorrelationV2::Enroll {
                invitation_id: invitation_id.clone(),
                device_public_key: device_public_key.clone(),
                selected_runtime_ids: selected_runtime_ids.clone(),
                requested_scopes: requested_scopes.clone(),
                idempotency_key: idempotency_key.clone(),
            },
            Self::ListAgents { credential_id, .. } => RequestCorrelationV2::ListAgents {
                credential_id: credential_id.clone(),
            },
            Self::RestartAgent {
                credential_id,
                agent,
                idempotency_key,
                command_sequence,
                ..
            } => RequestCorrelationV2::RestartAgent {
                credential_id: credential_id.clone(),
                agent: agent.clone(),
                idempotency_key: idempotency_key.clone(),
                command_sequence: *command_sequence,
            },
            Self::Connect {
                credential_id,
                agent,
                ..
            } => RequestCorrelationV2::Connect {
                credential_id: credential_id.clone(),
                agent: agent.clone(),
            },
            Self::RevokeSelf {
                credential_id,
                idempotency_key,
                ..
            } => RequestCorrelationV2::RevokeSelf {
                credential_id: credential_id.clone(),
                idempotency_key: idempotency_key.clone(),
            },
            Self::RollbackEnrollment {
                credential_id,
                enrollment_idempotency_key,
                idempotency_key,
                ..
            } => RequestCorrelationV2::RollbackEnrollment {
                credential_id: credential_id.clone(),
                enrollment_idempotency_key: enrollment_idempotency_key.clone(),
                idempotency_key: idempotency_key.clone(),
            },
        })
    }

    pub(crate) fn operation_payload_hash(&self) -> Result<[u8; 32], WireError> {
        self.validate()?;
        match self {
            Self::InspectInvitation {
                invitation_id,
                secret,
                device_public_key,
                ..
            } => operation_payload_hash(&[invitation_id, secret, device_public_key]),
            Self::Enroll {
                invitation_id,
                secret,
                device_name,
                device_public_key,
                selected_runtime_ids,
                requested_scopes,
                idempotency_key,
                ..
            } => operation_payload_hash(&[
                invitation_id,
                secret,
                device_name,
                device_public_key,
                &canonical_runtime_ids(selected_runtime_ids),
                &canonical_scopes(requested_scopes),
                idempotency_key,
            ]),
            Self::ListAgents { .. } => operation_payload_hash(&[]),
            Self::RestartAgent {
                agent,
                idempotency_key,
                command_sequence,
                ..
            } => operation_payload_hash(&[agent, idempotency_key, &command_sequence.to_string()]),
            Self::Connect { agent, resume, .. } => operation_payload_hash(&[
                agent,
                &resume
                    .as_ref()
                    .map(|v| v.last_seq.to_string())
                    .unwrap_or_default(),
            ]),
            Self::RevokeSelf {
                idempotency_key, ..
            } => operation_payload_hash(&[idempotency_key]),
            Self::RollbackEnrollment {
                enrollment_idempotency_key,
                idempotency_key,
                ..
            } => operation_payload_hash(&[enrollment_idempotency_key, idempotency_key]),
        }
    }
    pub(crate) fn validate(&self) -> Result<(), WireError> {
        if self.version() != PROTOCOL_VERSION || !valid_nonce(self.client_nonce()) {
            return Err(WireError::InvalidRequest);
        }
        match self {
            Self::InspectInvitation {
                invitation_id,
                secret,
                device_public_key,
                ..
            } => {
                valid_opaque(invitation_id, OPAQUE_ID_BYTES)?;
                valid_exact_b64(secret, INVITATION_SECRET_BYTES)?;
                validate_public_key(device_public_key)?;
            }
            Self::Enroll {
                invitation_id,
                secret,
                device_name,
                device_public_key,
                selected_runtime_ids,
                requested_scopes,
                idempotency_key,
                ..
            } => {
                valid_opaque(invitation_id, OPAQUE_ID_BYTES)?;
                valid_exact_b64(secret, INVITATION_SECRET_BYTES)?;
                valid_device_name(device_name)?;
                validate_public_key(device_public_key)?;
                validate_grant(selected_runtime_ids, requested_scopes)?;
                valid_idempotency(idempotency_key)?;
            }
            Self::ListAgents { credential_id, .. } => valid_opaque(credential_id, OPAQUE_ID_BYTES)?,
            Self::RestartAgent {
                credential_id,
                agent,
                idempotency_key,
                command_sequence,
                ..
            } => {
                valid_opaque(credential_id, OPAQUE_ID_BYTES)?;
                valid_runtime_id(agent)?;
                valid_idempotency(idempotency_key)?;
                if *command_sequence == 0 {
                    return Err(WireError::InvalidRequest);
                }
            }
            Self::Connect {
                credential_id,
                agent,
                ..
            } => {
                valid_opaque(credential_id, OPAQUE_ID_BYTES)?;
                valid_runtime_id(agent)?;
            }
            Self::RevokeSelf {
                credential_id,
                idempotency_key,
                ..
            } => {
                valid_opaque(credential_id, OPAQUE_ID_BYTES)?;
                valid_idempotency(idempotency_key)?;
            }
            Self::RollbackEnrollment {
                credential_id,
                enrollment_idempotency_key,
                idempotency_key,
                ..
            } => {
                valid_opaque(credential_id, OPAQUE_ID_BYTES)?;
                valid_idempotency(enrollment_idempotency_key)?;
                valid_idempotency(idempotency_key)?;
            }
        };
        Ok(())
    }
}

impl RequestCorrelationV2 {
    fn validate(&self) -> Result<(), WireError> {
        match self {
            Self::InspectInvitation { invitation_id } => {
                valid_opaque(invitation_id, OPAQUE_ID_BYTES)?;
            }
            Self::Enroll {
                invitation_id,
                device_public_key,
                selected_runtime_ids,
                requested_scopes,
                idempotency_key,
            } => {
                valid_opaque(invitation_id, OPAQUE_ID_BYTES)?;
                validate_public_key(device_public_key)?;
                validate_grant(selected_runtime_ids, requested_scopes)?;
                valid_idempotency(idempotency_key)?;
            }
            Self::ListAgents { credential_id } => {
                valid_opaque(credential_id, OPAQUE_ID_BYTES)?;
            }
            Self::RestartAgent {
                credential_id,
                agent,
                idempotency_key,
                command_sequence,
            } => {
                valid_opaque(credential_id, OPAQUE_ID_BYTES)?;
                valid_runtime_id(agent)?;
                valid_idempotency(idempotency_key)?;
                if *command_sequence == 0 {
                    return Err(WireError::InvalidRequest);
                }
            }
            Self::Connect {
                credential_id,
                agent,
            } => {
                valid_opaque(credential_id, OPAQUE_ID_BYTES)?;
                valid_runtime_id(agent)?;
            }
            Self::RevokeSelf {
                credential_id,
                idempotency_key,
            } => {
                valid_opaque(credential_id, OPAQUE_ID_BYTES)?;
                valid_idempotency(idempotency_key)?;
            }
            Self::RollbackEnrollment {
                credential_id,
                enrollment_idempotency_key,
                idempotency_key,
            } => {
                valid_opaque(credential_id, OPAQUE_ID_BYTES)?;
                valid_idempotency(enrollment_idempotency_key)?;
                valid_idempotency(idempotency_key)?;
            }
        }
        Ok(())
    }

    fn expected_credential(
        &self,
        authenticated_client_endpoint_id: &str,
    ) -> Result<(String, bool), WireError> {
        self.validate()?;
        validate_endpoint_id(authenticated_client_endpoint_id)?;
        Ok(match self {
            Self::InspectInvitation { invitation_id } => (invitation_id.clone(), true),
            Self::Enroll {
                invitation_id,
                device_public_key,
                idempotency_key,
                ..
            } => (
                prospective_credential_id(
                    invitation_id,
                    authenticated_client_endpoint_id,
                    device_public_key,
                    idempotency_key,
                )?,
                true,
            ),
            Self::ListAgents { credential_id }
            | Self::RestartAgent { credential_id, .. }
            | Self::Connect { credential_id, .. }
            | Self::RevokeSelf { credential_id, .. }
            | Self::RollbackEnrollment { credential_id, .. } => (credential_id.clone(), false),
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProofChallengeV2 {
    pub(crate) challenge_id: String,
    pub(crate) credential_id: String,
    pub(crate) auth_epoch: u64,
    pub(crate) server_nonce: String,
    pub(crate) expires_at: i64,
}

impl ProofChallengeV2 {
    pub(crate) fn validate(&self) -> Result<(), WireError> {
        valid_opaque(&self.challenge_id, OPAQUE_ID_BYTES)?;
        valid_opaque(&self.credential_id, OPAQUE_ID_BYTES)?;
        valid_exact_b64(&self.server_nonce, NONCE_BYTES)?;
        Ok(())
    }
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProofV2 {
    pub(crate) v: u32,
    pub(crate) challenge_id: String,
    pub(crate) signature: String,
}

impl ProofV2 {
    pub(crate) fn decode_json(bytes: &[u8]) -> Result<Self, WireError> {
        let proof: Self = serde_json::from_slice(bytes).map_err(|_| WireError::InvalidProof)?;
        proof.validate()?;
        Ok(proof)
    }

    pub(crate) fn validate(&self) -> Result<(), WireError> {
        if self.v != PROTOCOL_VERSION {
            return Err(WireError::InvalidProof);
        }
        valid_opaque(&self.challenge_id, OPAQUE_ID_BYTES).map_err(|_| WireError::InvalidProof)?;
        if self.signature.len() > MAX_SIGNATURE_TEXT || self.signature.contains('=') {
            return Err(WireError::InvalidProof);
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(&self.signature)
            .map_err(|_| WireError::InvalidProof)?;
        if URL_SAFE_NO_PAD.encode(&bytes) != self.signature {
            return Err(WireError::InvalidProof);
        }
        let signature = Signature::from_der(&bytes).map_err(|_| WireError::InvalidProof)?;
        if signature.to_der().as_bytes() != bytes {
            return Err(WireError::InvalidProof);
        }
        Ok(())
    }

    pub(crate) fn validate_for_challenge(
        &self,
        challenge: &ProofChallengeV2,
    ) -> Result<(), WireError> {
        self.validate()?;
        challenge.validate()?;
        if self.challenge_id != challenge.challenge_id {
            return Err(WireError::InvalidProof);
        }
        Ok(())
    }
}

impl fmt::Debug for ProofV2 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProofV2")
            .field("v", &self.v)
            .field("challenge_id", &self.challenge_id)
            .field("signature", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ErrorCodeV2 {
    PairingUnavailable,
    AuthorizationRequired,
    InvalidRequest,
    AgentUnavailable,
    OutcomeUnknown,
    Internal,
}

impl ErrorCodeV2 {
    pub(crate) fn message(&self) -> &'static str {
        match self {
            Self::PairingUnavailable => "pairing unavailable",
            Self::AuthorizationRequired => "device authorization required",
            Self::InvalidRequest => "invalid request",
            Self::AgentUnavailable => "agent unavailable",
            Self::OutcomeUnknown => "operation outcome unknown",
            Self::Internal => "request failed",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct EnrollmentConfirmationV2 {
    pub(crate) transcript_hash: String,
    pub(crate) sas: String,
}

impl EnrollmentConfirmationV2 {
    fn validate(&self) -> Result<(), WireError> {
        valid_exact_b64(&self.transcript_hash, 32).map_err(|_| WireError::InvalidResponse)?;
        if !valid_sas(&self.sas) {
            return Err(WireError::InvalidResponse);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RuntimeOfferV2 {
    pub(crate) runtime_id: String,
    pub(crate) display_name: String,
    pub(crate) available: bool,
    pub(crate) recommended: bool,
}

impl RuntimeOfferV2 {
    pub(crate) fn validate(&self) -> Result<(), WireError> {
        valid_runtime_id(&self.runtime_id).map_err(|_| WireError::InvalidResponse)?;
        valid_response_label(&self.display_name)?;
        if self.recommended && !self.available {
            return Err(WireError::InvalidResponse);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct InvitationInspectionV2 {
    pub(crate) invitation_id: String,
    pub(crate) expires_at: i64,
    pub(crate) max_runtime_ids: Vec<String>,
    pub(crate) max_scopes: Vec<DeviceScopeV2>,
    pub(crate) confirmation_mode: ConfirmationModeV2,
    pub(crate) runtime_offers: Vec<RuntimeOfferV2>,
}

impl InvitationInspectionV2 {
    fn validate(&self) -> Result<(), WireError> {
        valid_opaque(&self.invitation_id, OPAQUE_ID_BYTES)
            .map_err(|_| WireError::InvalidResponse)?;
        validate_policy(
            &self.max_runtime_ids,
            &self.max_scopes,
            self.confirmation_mode,
        )
        .map_err(|_| WireError::InvalidResponse)?;
        if self.runtime_offers.len() > self.max_runtime_ids.len() {
            return Err(WireError::InvalidResponse);
        }
        let mut seen = std::collections::HashSet::with_capacity(self.runtime_offers.len());
        for offer in &self.runtime_offers {
            offer.validate()?;
            if !self.max_runtime_ids.contains(&offer.runtime_id)
                || !seen.insert(offer.runtime_id.as_str())
            {
                return Err(WireError::InvalidResponse);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct PendingEnrollmentV2 {
    pub(crate) claim_id: String,
    pub(crate) credential_id: String,
    pub(crate) display_name: String,
    pub(crate) selected_runtime_ids: Vec<String>,
    pub(crate) requested_scopes: Vec<DeviceScopeV2>,
    pub(crate) enrollment_confirmation: EnrollmentConfirmationV2,
    pub(crate) created_at: i64,
    pub(crate) expires_at: i64,
}

impl PendingEnrollmentV2 {
    fn validate(&self) -> Result<(), WireError> {
        valid_opaque(&self.claim_id, OPAQUE_ID_BYTES).map_err(|_| WireError::InvalidResponse)?;
        valid_opaque(&self.credential_id, OPAQUE_ID_BYTES)
            .map_err(|_| WireError::InvalidResponse)?;
        valid_device_name(&self.display_name).map_err(|_| WireError::InvalidResponse)?;
        if self.display_name.trim().is_empty() || self.expires_at < self.created_at {
            return Err(WireError::InvalidResponse);
        }
        validate_grant(&self.selected_runtime_ids, &self.requested_scopes)
            .map_err(|_| WireError::InvalidResponse)?;
        self.enrollment_confirmation.validate()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct EnrolledDeviceV2 {
    pub(crate) device_id: String,
    pub(crate) display_name: String,
    pub(crate) endpoint_fingerprint: String,
    pub(crate) device_key_fingerprint: String,
    pub(crate) selected_runtime_ids: Vec<String>,
    pub(crate) granted_scopes: Vec<DeviceScopeV2>,
    pub(crate) auth_epoch: u64,
    pub(crate) created_at: i64,
    pub(crate) enrollment_confirmation: EnrollmentConfirmationV2,
}

impl EnrolledDeviceV2 {
    fn validate(&self) -> Result<(), WireError> {
        valid_opaque(&self.device_id, OPAQUE_ID_BYTES).map_err(|_| WireError::InvalidResponse)?;
        valid_device_name(&self.display_name).map_err(|_| WireError::InvalidResponse)?;
        if self.display_name.trim().is_empty()
            || !valid_fingerprint(&self.endpoint_fingerprint)
            || !valid_fingerprint(&self.device_key_fingerprint)
        {
            return Err(WireError::InvalidResponse);
        }
        validate_grant(&self.selected_runtime_ids, &self.granted_scopes)
            .map_err(|_| WireError::InvalidResponse)?;
        self.enrollment_confirmation.validate()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AgentWireV2 {
    Websocket,
    Jsonl,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentPresentationV2 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) title: Option<String>,
    #[serde(default)]
    pub(crate) is_beta: bool,
    #[serde(default)]
    pub(crate) sort_order: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) aliases: Vec<String>,
}

impl AgentPresentationV2 {
    fn validate(&self) -> Result<(), WireError> {
        if self
            .title
            .as_deref()
            .is_some_and(|value| !valid_unbounded_display_value(value))
            || self
                .description
                .as_deref()
                .is_some_and(|value| !valid_unbounded_display_value(value))
        {
            return Err(WireError::InvalidResponse);
        }
        let mut aliases = std::collections::HashSet::with_capacity(self.aliases.len());
        for alias in &self.aliases {
            valid_agent_alias(alias).map_err(|_| WireError::InvalidResponse)?;
            if !aliases.insert(alias.as_str()) {
                return Err(WireError::InvalidResponse);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentCapabilitiesV2 {
    #[serde(default)]
    pub(crate) locks_reasoning_effort_after_activity: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) visible_modes: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) supports_ssh_bridge: bool,
    #[serde(default)]
    pub(crate) uses_direct_codex_port: bool,
    #[serde(default)]
    pub(crate) supports_thread_permission_overrides: bool,
    #[serde(default)]
    pub(crate) reports_effective_thread_permissions: bool,
}

impl AgentCapabilitiesV2 {
    fn validate(&self) -> Result<(), WireError> {
        let Some(modes) = self.visible_modes.as_ref() else {
            return Ok(());
        };
        let mut seen = std::collections::HashSet::with_capacity(modes.len());
        for mode in modes {
            if !valid_unbounded_display_value(mode) || !seen.insert(mode.as_str()) {
                return Err(WireError::InvalidResponse);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentInfoV2 {
    pub(crate) name: String,
    pub(crate) display_name: String,
    pub(crate) wire: AgentWireV2,
    pub(crate) available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) presentation: Option<AgentPresentationV2>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) capabilities: Option<AgentCapabilitiesV2>,
}

impl AgentInfoV2 {
    fn validate(&self) -> Result<(), WireError> {
        valid_runtime_id(&self.name).map_err(|_| WireError::InvalidResponse)?;
        valid_response_label(&self.display_name)?;
        if let Some(presentation) = &self.presentation {
            presentation.validate()?;
        }
        if let Some(capabilities) = &self.capabilities {
            capabilities.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AttachKindV2 {
    Fresh,
    Resumed,
    DriftReload,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionV2 {
    pub(crate) attached: AttachKindV2,
    pub(crate) current_seq: u64,
    pub(crate) floor_seq: u64,
}

impl SessionV2 {
    fn validate(&self) -> Result<(), WireError> {
        if self.floor_seq > self.current_seq.saturating_add(1) {
            return Err(WireError::InvalidResponse);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RestartStatusV2 {
    Succeeded,
    OutcomeUnknown,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RestartResultV2 {
    pub(crate) agent: String,
    pub(crate) idempotency_key: String,
    pub(crate) command_sequence: u64,
    pub(crate) status: RestartStatusV2,
}

impl RestartResultV2 {
    fn validate(&self) -> Result<(), WireError> {
        valid_runtime_id(&self.agent).map_err(|_| WireError::InvalidResponse)?;
        valid_idempotency(&self.idempotency_key).map_err(|_| WireError::InvalidResponse)?;
        if self.command_sequence == 0 {
            return Err(WireError::InvalidResponse);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RevocationReceiptV2 {
    pub(crate) credential_id: String,
    pub(crate) auth_epoch: u64,
    pub(crate) revoked_at: i64,
    pub(crate) idempotency_key: String,
}

impl RevocationReceiptV2 {
    fn validate(&self) -> Result<(), WireError> {
        valid_opaque(&self.credential_id, OPAQUE_ID_BYTES)
            .map_err(|_| WireError::InvalidResponse)?;
        valid_idempotency(&self.idempotency_key).map_err(|_| WireError::InvalidResponse)?;
        if self.auth_epoch == 0 {
            return Err(WireError::InvalidResponse);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResponseV2 {
    pub(crate) v: u32,
    pub(crate) ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) challenge: Option<ProofChallengeV2>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) enrolled: Option<EnrolledDeviceV2>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) inspection: Option<InvitationInspectionV2>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) pending: Option<PendingEnrollmentV2>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) revocation: Option<RevocationReceiptV2>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) restart: Option<RestartResultV2>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) agents: Option<Vec<AgentInfoV2>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) session: Option<SessionV2>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) error_code: Option<ErrorCodeV2>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
}

impl ResponseV2 {
    pub(crate) fn decode_json(bytes: &[u8]) -> Result<Self, WireError> {
        let response: Self =
            serde_json::from_slice(bytes).map_err(|_| WireError::InvalidResponse)?;
        response.validate()?;
        Ok(response)
    }

    pub(crate) fn validate(&self) -> Result<(), WireError> {
        if self.v != PROTOCOL_VERSION {
            return Err(WireError::InvalidResponse);
        }
        if let Some(challenge) = &self.challenge {
            challenge
                .validate()
                .map_err(|_| WireError::InvalidResponse)?;
        }
        if let Some(value) = &self.enrolled {
            value.validate()?;
        }
        if let Some(value) = &self.inspection {
            value.validate()?;
        }
        if let Some(value) = &self.pending {
            value.validate()?;
        }
        if let Some(value) = &self.revocation {
            value.validate()?;
        }
        if let Some(value) = &self.restart {
            value.validate()?;
        }
        if let Some(values) = &self.agents {
            if values.len() > MAX_RUNTIME_IDS {
                return Err(WireError::InvalidResponse);
            }
            let mut names = std::collections::HashSet::with_capacity(values.len());
            for value in values {
                value.validate()?;
                if !names.insert(value.name.as_str()) {
                    return Err(WireError::InvalidResponse);
                }
            }
        }
        if let Some(value) = &self.session {
            value.validate()?;
        }

        let terminal_count = [
            self.enrolled.is_some(),
            self.inspection.is_some(),
            self.pending.is_some(),
            self.revocation.is_some(),
            self.restart.is_some(),
            self.agents.is_some(),
            self.session.is_some(),
        ]
        .into_iter()
        .filter(|present| *present)
        .count();

        if self.challenge.is_some() {
            if !self.ok || terminal_count != 0 || self.error_code.is_some() || self.error.is_some()
            {
                return Err(WireError::InvalidResponse);
            }
            return Ok(());
        }

        if self.ok {
            if terminal_count != 1 || self.error_code.is_some() || self.error.is_some() {
                return Err(WireError::InvalidResponse);
            }
            if self
                .restart
                .as_ref()
                .is_some_and(|value| value.status != RestartStatusV2::Succeeded)
            {
                return Err(WireError::InvalidResponse);
            }
            return Ok(());
        }

        let code = self.error_code.as_ref().ok_or(WireError::InvalidResponse)?;
        if self.error.as_deref() != Some(code.message()) {
            return Err(WireError::InvalidResponse);
        }
        match (self.restart.as_ref(), self.revocation.as_ref()) {
            (Some(restart), None)
                if terminal_count == 1
                    && restart.status == RestartStatusV2::OutcomeUnknown
                    && *code == ErrorCodeV2::OutcomeUnknown =>
            {
                Ok(())
            }
            (None, Some(_)) if terminal_count == 1 && *code == ErrorCodeV2::OutcomeUnknown => {
                Ok(())
            }
            (None, None) if terminal_count == 0 => Ok(()),
            _ => Err(WireError::InvalidResponse),
        }
    }

    pub(crate) fn validate_challenge_for_request(
        &self,
        request: &RequestV2,
        authenticated_client_endpoint_id: &str,
    ) -> Result<&ProofChallengeV2, WireError> {
        self.validate()?;
        request.validate()?;
        let challenge = self.challenge.as_ref().ok_or(WireError::InvalidResponse)?;
        validate_endpoint_id(authenticated_client_endpoint_id)?;
        let (expected_credential, expected_epoch) = match request {
            RequestV2::InspectInvitation { invitation_id, .. } => (invitation_id.clone(), Some(0)),
            RequestV2::Enroll {
                invitation_id,
                device_public_key,
                idempotency_key,
                ..
            } => (
                prospective_credential_id(
                    invitation_id,
                    authenticated_client_endpoint_id,
                    device_public_key,
                    idempotency_key,
                )?,
                Some(0),
            ),
            _ => (
                request
                    .credential_id()
                    .ok_or(WireError::InvalidRequest)?
                    .to_string(),
                None,
            ),
        };
        if challenge.credential_id != expected_credential
            || expected_epoch.is_some_and(|epoch| challenge.auth_epoch != epoch)
        {
            return Err(WireError::InvalidResponse);
        }
        Ok(challenge)
    }

    /// Validate the terminal envelope and request/result identity only.
    ///
    /// Enrollment callers must additionally reproduce and compare the
    /// transcript hash and SAS from locally staged enrollment inputs before
    /// committing a credential. That authentication belongs to the
    /// journal-backed lifecycle, not this stateless wire-shape validator.
    pub(crate) fn validate_terminal_shape_for_request(
        &self,
        request: &RequestV2,
        challenge: &ProofChallengeV2,
        authenticated_client_endpoint_id: &str,
    ) -> Result<(), WireError> {
        let correlation = request.terminal_correlation()?;
        self.validate_terminal_shape_for_correlation(
            &correlation,
            challenge,
            authenticated_client_endpoint_id,
        )
    }

    pub(crate) fn validate_terminal_shape_for_correlation(
        &self,
        correlation: &RequestCorrelationV2,
        challenge: &ProofChallengeV2,
        authenticated_client_endpoint_id: &str,
    ) -> Result<(), WireError> {
        self.validate()?;
        correlation.validate()?;
        validate_challenge_for_correlation(
            challenge,
            correlation,
            authenticated_client_endpoint_id,
        )?;
        if self.challenge.is_some() {
            return Err(WireError::InvalidResponse);
        }
        if !self.ok && self.restart.is_none() && self.revocation.is_none() {
            return Ok(());
        }

        match correlation {
            RequestCorrelationV2::InspectInvitation { invitation_id } => {
                let result = self.inspection.as_ref().ok_or(WireError::InvalidResponse)?;
                if result.invitation_id != *invitation_id {
                    return Err(WireError::InvalidResponse);
                }
            }
            RequestCorrelationV2::Enroll {
                selected_runtime_ids,
                requested_scopes,
                ..
            } => match (self.pending.as_ref(), self.enrolled.as_ref()) {
                (Some(pending), None) => {
                    if pending.credential_id != challenge.credential_id
                        || pending.selected_runtime_ids != *selected_runtime_ids
                        || pending.requested_scopes != *requested_scopes
                    {
                        return Err(WireError::InvalidResponse);
                    }
                }
                (None, Some(enrolled)) => {
                    if enrolled.device_id != challenge.credential_id {
                        return Err(WireError::InvalidResponse);
                    }
                    ensure_subset(&enrolled.selected_runtime_ids, selected_runtime_ids)
                        .map_err(|_| WireError::InvalidResponse)?;
                    ensure_scope_subset(&enrolled.granted_scopes, requested_scopes)
                        .map_err(|_| WireError::InvalidResponse)?;
                }
                _ => return Err(WireError::InvalidResponse),
            },
            RequestCorrelationV2::ListAgents { .. } => {
                if self.agents.is_none() {
                    return Err(WireError::InvalidResponse);
                }
            }
            RequestCorrelationV2::RestartAgent {
                agent,
                idempotency_key,
                command_sequence,
                ..
            } => {
                let result = self.restart.as_ref().ok_or(WireError::InvalidResponse)?;
                if result.agent != *agent
                    || result.idempotency_key != *idempotency_key
                    || result.command_sequence != *command_sequence
                {
                    return Err(WireError::InvalidResponse);
                }
            }
            RequestCorrelationV2::Connect { .. } => {
                if self.session.is_none() {
                    return Err(WireError::InvalidResponse);
                }
            }
            RequestCorrelationV2::RevokeSelf {
                credential_id,
                idempotency_key,
                ..
            } => {
                validate_revocation_correlation(
                    self.revocation.as_ref(),
                    credential_id,
                    idempotency_key,
                )?;
            }
            RequestCorrelationV2::RollbackEnrollment {
                credential_id,
                idempotency_key,
                ..
            } => {
                validate_revocation_correlation(
                    self.revocation.as_ref(),
                    credential_id,
                    idempotency_key,
                )?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum WireError {
    #[error("invalid Remora Link v2 request")]
    InvalidRequest,
    #[error("invalid Remora Link v2 policy")]
    InvalidPolicy,
    #[error("invalid Remora Link v2 proof")]
    InvalidProof,
    #[error("invalid Remora Link v2 response")]
    InvalidResponse,
    #[error("transcript field exceeds u32 framing")]
    FieldTooLarge,
}

pub(crate) struct ProofTranscriptInput<'a> {
    pub(crate) host_endpoint_id: &'a str,
    pub(crate) client_endpoint_id: &'a str,
    pub(crate) operation: &'a str,
    pub(crate) credential_id: &'a str,
    pub(crate) auth_epoch: u64,
    pub(crate) device_key_hash: &'a [u8; 32],
    pub(crate) challenge_id: &'a str,
    pub(crate) server_nonce: &'a str,
    pub(crate) client_nonce: &'a str,
    pub(crate) operation_payload_hash: &'a [u8; 32],
}
pub(crate) fn encode_proof_transcript(
    input: ProofTranscriptInput<'_>,
) -> Result<Vec<u8>, WireError> {
    validate_endpoint_id(input.host_endpoint_id)?;
    validate_endpoint_id(input.client_endpoint_id)?;
    if !is_operation(input.operation) {
        return Err(WireError::InvalidRequest);
    }
    valid_opaque(input.credential_id, OPAQUE_ID_BYTES)?;
    valid_opaque(input.challenge_id, OPAQUE_ID_BYTES)?;
    valid_exact_b64(input.server_nonce, NONCE_BYTES)?;
    valid_exact_b64(input.client_nonce, NONCE_BYTES)?;
    let mut out = PROOF_DOMAIN.to_vec();
    for field in [
        &PROTOCOL_VERSION.to_be_bytes()[..],
        ALPN,
        input.host_endpoint_id.as_bytes(),
        input.client_endpoint_id.as_bytes(),
        input.operation.as_bytes(),
        input.credential_id.as_bytes(),
        &input.auth_epoch.to_be_bytes()[..],
        input.device_key_hash,
        input.challenge_id.as_bytes(),
        input.server_nonce.as_bytes(),
        input.client_nonce.as_bytes(),
        input.operation_payload_hash,
    ] {
        append_field(&mut out, field)?;
    }
    Ok(out)
}
#[derive(Clone, Copy)]
pub(crate) struct EnrollmentTranscriptInput<'a> {
    pub(crate) host_endpoint_id: &'a str,
    pub(crate) client_endpoint_id: &'a str,
    pub(crate) invitation_id: &'a str,
    pub(crate) device_public_key: &'a str,
    pub(crate) idempotency_key: &'a str,
    pub(crate) selected_runtime_ids: &'a [String],
    pub(crate) requested_scopes: &'a [DeviceScopeV2],
    pub(crate) server_nonce: &'a str,
    pub(crate) client_nonce: &'a str,
    pub(crate) confirmation_mode: ConfirmationModeV2,
    pub(crate) max_runtime_ids: &'a [String],
    pub(crate) max_scopes: &'a [DeviceScopeV2],
}
pub(crate) fn encode_enrollment_transcript(
    input: EnrollmentTranscriptInput<'_>,
) -> Result<Vec<u8>, WireError> {
    validate_endpoint_id(input.host_endpoint_id)?;
    validate_endpoint_id(input.client_endpoint_id)?;
    valid_opaque(input.invitation_id, OPAQUE_ID_BYTES)?;
    valid_idempotency(input.idempotency_key)?;
    valid_exact_b64(input.server_nonce, NONCE_BYTES)?;
    valid_exact_b64(input.client_nonce, NONCE_BYTES)?;
    validate_grant(input.selected_runtime_ids, input.requested_scopes)?;
    validate_policy(
        input.max_runtime_ids,
        input.max_scopes,
        input.confirmation_mode,
    )?;
    ensure_subset(input.selected_runtime_ids, input.max_runtime_ids)?;
    ensure_scope_subset(input.requested_scopes, input.max_scopes)?;
    let key = decode_public_key(input.device_public_key)?;
    let policy = host_policy_digest(input.max_runtime_ids, input.max_scopes)?;
    let mut out = ENROLLMENT_DOMAIN.to_vec();
    for field in [
        &PROTOCOL_VERSION.to_be_bytes()[..],
        ALPN,
        input.host_endpoint_id.as_bytes(),
        input.client_endpoint_id.as_bytes(),
        input.invitation_id.as_bytes(),
        input.idempotency_key.as_bytes(),
        input.server_nonce.as_bytes(),
        input.client_nonce.as_bytes(),
        &key,
        canonical_runtime_ids(input.selected_runtime_ids).as_bytes(),
        canonical_scopes(input.requested_scopes).as_bytes(),
        &policy,
        &[match input.confirmation_mode {
            ConfirmationModeV2::Interactive => 0,
            ConfirmationModeV2::Unattended => 1,
        }],
    ] {
        append_field(&mut out, field)?;
    }
    Ok(out)
}

pub(crate) fn enrollment_transcript_hash(
    input: EnrollmentTranscriptInput<'_>,
) -> Result<[u8; 32], WireError> {
    Ok(Sha256::digest(encode_enrollment_transcript(input)?).into())
}

pub(crate) fn host_policy_digest(
    runtimes: &[String],
    scopes: &[DeviceScopeV2],
) -> Result<[u8; 32], WireError> {
    Ok(Sha256::digest(encode_host_policy_transcript(runtimes, scopes)?).into())
}

pub(crate) fn encode_host_policy_transcript(
    runtimes: &[String],
    scopes: &[DeviceScopeV2],
) -> Result<Vec<u8>, WireError> {
    validate_policy(runtimes, scopes, ConfirmationModeV2::Interactive)?;
    let mut out = POLICY_DOMAIN.to_vec();
    append_field(&mut out, canonical_runtime_ids(runtimes).as_bytes())?;
    append_field(&mut out, canonical_scopes(scopes).as_bytes())?;
    Ok(out)
}
pub(crate) fn operation_payload_hash(fields: &[&str]) -> Result<[u8; 32], WireError> {
    let mut out = PAYLOAD_DOMAIN.to_vec();
    for field in fields {
        append_field(&mut out, field.as_bytes())?;
    }
    let digest = Sha256::digest(&out).into();
    out.zeroize();
    Ok(digest)
}

pub(crate) fn prospective_credential_id(
    invitation_id: &str,
    authenticated_client_endpoint_id: &str,
    device_public_key: &str,
    enrollment_idempotency_key: &str,
) -> Result<String, WireError> {
    let mut material = encode_prospective_credential_material(
        invitation_id,
        authenticated_client_endpoint_id,
        device_public_key,
        enrollment_idempotency_key,
    )?;
    let digest = Sha256::digest(&material);
    let credential_id = URL_SAFE_NO_PAD.encode(&digest[..OPAQUE_ID_BYTES]);
    material.zeroize();
    Ok(credential_id)
}

pub(crate) fn encode_prospective_credential_material(
    invitation_id: &str,
    authenticated_client_endpoint_id: &str,
    device_public_key: &str,
    enrollment_idempotency_key: &str,
) -> Result<Vec<u8>, WireError> {
    valid_opaque(invitation_id, OPAQUE_ID_BYTES)?;
    validate_endpoint_id(authenticated_client_endpoint_id)?;
    validate_public_key(device_public_key)?;
    valid_idempotency(enrollment_idempotency_key)?;

    let mut material = PROSPECTIVE_CREDENTIAL_DOMAIN.to_vec();
    for field in [
        invitation_id.as_bytes(),
        authenticated_client_endpoint_id.as_bytes(),
        device_public_key.as_bytes(),
        enrollment_idempotency_key.as_bytes(),
    ] {
        append_field(&mut material, field)?;
    }
    Ok(material)
}
pub(crate) fn derive_sas(
    invitation_secret: &str,
    transcript_hash: &[u8; 32],
) -> Result<String, WireError> {
    let key =
        Zeroizing::new(decode_exact(invitation_secret, 32).map_err(|_| WireError::InvalidRequest)?);
    let mut message = SAS_DOMAIN.to_vec();
    message.extend_from_slice(transcript_hash);
    let digest = hmac_sha256(&key, &message);
    const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let value = u32::from_be_bytes(digest[..4].try_into().expect("four bytes")) >> 2;
    let code: Vec<u8> = (0..6)
        .map(|index| CROCKFORD[((value >> (25 - index * 5)) & 31) as usize])
        .collect();
    Ok(format!(
        "{}-{}",
        std::str::from_utf8(&code[..3]).expect("ascii"),
        std::str::from_utf8(&code[3..]).expect("ascii")
    ))
}

pub(crate) fn verify_proof_signature(
    request: &RequestV2,
    challenge: &ProofChallengeV2,
    proof: &ProofV2,
    host_endpoint_id: &str,
    client_endpoint_id: &str,
    device_public_key: &str,
) -> Result<(), WireError> {
    request.validate()?;
    challenge.validate()?;
    proof.validate_for_challenge(challenge)?;
    validate_challenge_for_request(challenge, request, client_endpoint_id)?;
    let key_bytes = decode_public_key(device_public_key)?;
    let verifying_key =
        VerifyingKey::from_sec1_bytes(&key_bytes).map_err(|_| WireError::InvalidProof)?;
    let device_key_hash: [u8; 32] = Sha256::digest(&key_bytes).into();
    let payload_hash = request.operation_payload_hash()?;
    let transcript = encode_proof_transcript(ProofTranscriptInput {
        host_endpoint_id,
        client_endpoint_id,
        operation: request.operation(),
        credential_id: &challenge.credential_id,
        auth_epoch: challenge.auth_epoch,
        device_key_hash: &device_key_hash,
        challenge_id: &challenge.challenge_id,
        server_nonce: &challenge.server_nonce,
        client_nonce: request.client_nonce(),
        operation_payload_hash: &payload_hash,
    })?;
    let signature_bytes = URL_SAFE_NO_PAD
        .decode(&proof.signature)
        .map_err(|_| WireError::InvalidProof)?;
    let signature = Signature::from_der(&signature_bytes).map_err(|_| WireError::InvalidProof)?;
    verifying_key
        .verify(&transcript, &signature)
        .map_err(|_| WireError::InvalidProof)
}
pub(crate) fn validate_policy(
    runtimes: &[String],
    scopes: &[DeviceScopeV2],
    mode: ConfirmationModeV2,
) -> Result<(), WireError> {
    validate_grant(runtimes, scopes)?;
    if mode == ConfirmationModeV2::Unattended
        && (runtimes.len() != 1
            || scopes
                != [
                    DeviceScopeV2::InspectRuntimes,
                    DeviceScopeV2::ConnectRuntime,
                    DeviceScopeV2::SelfRevoke,
                ])
    {
        return Err(WireError::InvalidPolicy);
    }
    Ok(())
}
fn validate_grant(runtimes: &[String], scopes: &[DeviceScopeV2]) -> Result<(), WireError> {
    let selects_coding_runtime = runtimes.iter().any(|runtime| runtime != "shell");
    if runtimes.is_empty()
        || runtimes.len() > MAX_RUNTIME_IDS
        || !is_canonical_runtime_ids(runtimes)
        || !is_canonical_scopes(scopes)
        || (selects_coding_runtime && !scopes.contains(&DeviceScopeV2::InspectRuntimes))
        || !scopes.contains(&DeviceScopeV2::ConnectRuntime)
        || !scopes.contains(&DeviceScopeV2::SelfRevoke)
    {
        return Err(WireError::InvalidPolicy);
    }
    for runtime in runtimes {
        valid_runtime_id(runtime)?;
    }
    Ok(())
}
fn ensure_subset(selected: &[String], maximum: &[String]) -> Result<(), WireError> {
    if selected.iter().all(|v| maximum.binary_search(v).is_ok()) {
        Ok(())
    } else {
        Err(WireError::InvalidPolicy)
    }
}
fn ensure_scope_subset(
    selected: &[DeviceScopeV2],
    maximum: &[DeviceScopeV2],
) -> Result<(), WireError> {
    if selected.iter().all(|v| maximum.contains(v)) {
        Ok(())
    } else {
        Err(WireError::InvalidPolicy)
    }
}
fn canonical_runtime_ids(values: &[String]) -> String {
    values.join("\0")
}
fn canonical_scopes(values: &[DeviceScopeV2]) -> String {
    values
        .iter()
        .map(|v| match v {
            DeviceScopeV2::InspectRuntimes => "inspect_runtimes",
            DeviceScopeV2::ConnectRuntime => "connect_runtime",
            DeviceScopeV2::RestartRuntime => "restart_runtime",
            DeviceScopeV2::SelfRevoke => "self_revoke",
        })
        .collect::<Vec<_>>()
        .join("\0")
}
fn is_canonical_runtime_ids(values: &[String]) -> bool {
    values.windows(2).all(|v| v[0] < v[1])
}
fn is_canonical_scopes(values: &[DeviceScopeV2]) -> bool {
    values.windows(2).all(|v| v[0] < v[1])
}

fn is_operation(value: &str) -> bool {
    matches!(
        value,
        "inspect_invitation"
            | "enroll"
            | "list_agents"
            | "restart_agent"
            | "connect"
            | "revoke_self"
            | "rollback_enrollment"
    )
}

fn valid_runtime_id(value: &str) -> Result<(), WireError> {
    if value.is_empty()
        || value.len() > MAX_RUNTIME_ID_BYTES
        || !value
            .bytes()
            .all(|v| v.is_ascii_alphanumeric() || matches!(v, b'.' | b'_' | b'/' | b'-'))
    {
        Err(WireError::InvalidPolicy)
    } else {
        Ok(())
    }
}

fn valid_agent_alias(value: &str) -> Result<(), WireError> {
    if value.is_empty() || value.len() > MAX_RUNTIME_ID_BYTES || value.chars().any(char::is_control)
    {
        Err(WireError::InvalidPolicy)
    } else {
        Ok(())
    }
}

fn valid_opaque(value: &str, bytes: usize) -> Result<(), WireError> {
    valid_exact_b64(value, bytes)
}

fn valid_exact_b64(value: &str, bytes: usize) -> Result<(), WireError> {
    if value.len() != bytes.saturating_mul(8).div_ceil(6) || value.contains('=') {
        return Err(WireError::InvalidRequest);
    }
    let mut decoded = decode_exact(value, bytes)?;
    let canonical_text = Zeroizing::new(URL_SAFE_NO_PAD.encode(&decoded));
    let canonical = canonical_text.as_str() == value;
    decoded.zeroize();
    canonical.then_some(()).ok_or(WireError::InvalidRequest)
}

fn valid_nonce(value: &str) -> bool {
    valid_exact_b64(value, NONCE_BYTES).is_ok()
}

fn valid_idempotency(value: &str) -> Result<(), WireError> {
    if value.is_empty()
        || value.len() > MAX_IDEMPOTENCY_BYTES
        || value.chars().any(char::is_control)
    {
        Err(WireError::InvalidRequest)
    } else {
        Ok(())
    }
}

pub(crate) fn valid_device_name(value: &str) -> Result<(), WireError> {
    if value.len() > MAX_DEVICE_NAME_BYTES || value.chars().any(char::is_control) {
        return Err(WireError::InvalidRequest);
    }
    Ok(())
}

fn valid_response_label(value: &str) -> Result<(), WireError> {
    if !valid_unbounded_display_value(value) {
        return Err(WireError::InvalidResponse);
    }
    Ok(())
}

fn valid_unbounded_display_value(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= super::client::MAX_CONTROL_FRAME_BYTES
        && !value.chars().any(char::is_control)
}

fn valid_fingerprint(value: &str) -> bool {
    value.len() == 16
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
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

fn validate_endpoint_id(value: &str) -> Result<(), WireError> {
    if value.is_empty()
        || value.len() > MAX_ENDPOINT_ID_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(WireError::InvalidRequest);
    }
    Ok(())
}

fn validate_challenge_for_request(
    challenge: &ProofChallengeV2,
    request: &RequestV2,
    authenticated_client_endpoint_id: &str,
) -> Result<(), WireError> {
    challenge
        .validate()
        .map_err(|_| WireError::InvalidResponse)?;
    request.validate()?;
    validate_endpoint_id(authenticated_client_endpoint_id)?;
    let (expected_credential_id, zero_epoch) = match request {
        RequestV2::InspectInvitation { invitation_id, .. } => (invitation_id.clone(), true),
        RequestV2::Enroll {
            invitation_id,
            device_public_key,
            idempotency_key,
            ..
        } => (
            prospective_credential_id(
                invitation_id,
                authenticated_client_endpoint_id,
                device_public_key,
                idempotency_key,
            )?,
            true,
        ),
        _ => (
            request
                .credential_id()
                .ok_or(WireError::InvalidRequest)?
                .to_string(),
            false,
        ),
    };
    if challenge.credential_id != expected_credential_id
        || (zero_epoch && challenge.auth_epoch != 0)
    {
        return Err(WireError::InvalidResponse);
    }
    Ok(())
}

fn validate_challenge_for_correlation(
    challenge: &ProofChallengeV2,
    correlation: &RequestCorrelationV2,
    authenticated_client_endpoint_id: &str,
) -> Result<(), WireError> {
    challenge
        .validate()
        .map_err(|_| WireError::InvalidResponse)?;
    let (expected_credential_id, zero_epoch) =
        correlation.expected_credential(authenticated_client_endpoint_id)?;
    if challenge.credential_id != expected_credential_id
        || (zero_epoch && challenge.auth_epoch != 0)
    {
        return Err(WireError::InvalidResponse);
    }
    Ok(())
}

fn validate_revocation_correlation(
    receipt: Option<&RevocationReceiptV2>,
    credential_id: &str,
    idempotency_key: &str,
) -> Result<(), WireError> {
    let receipt = receipt.ok_or(WireError::InvalidResponse)?;
    if receipt.credential_id != credential_id || receipt.idempotency_key != idempotency_key {
        return Err(WireError::InvalidResponse);
    }
    Ok(())
}

fn decode_exact(value: &str, length: usize) -> Result<Vec<u8>, WireError> {
    let mut decoded = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| WireError::InvalidRequest)?;
    if decoded.len() != length {
        decoded.zeroize();
        return Err(WireError::InvalidRequest);
    }
    Ok(decoded)
}
fn decode_public_key(value: &str) -> Result<Vec<u8>, WireError> {
    if value.len() > MAX_PUBLIC_KEY_TEXT {
        return Err(WireError::InvalidRequest);
    }
    valid_exact_b64(value, DEVICE_PUBLIC_KEY_BYTES)?;
    let bytes = decode_exact(value, DEVICE_PUBLIC_KEY_BYTES)?;
    if bytes.first() != Some(&4) || VerifyingKey::from_sec1_bytes(&bytes).is_err() {
        return Err(WireError::InvalidRequest);
    }
    Ok(bytes)
}
fn validate_public_key(value: &str) -> Result<(), WireError> {
    decode_public_key(value).map(|_| ())
}
fn append_field(out: &mut Vec<u8>, field: &[u8]) -> Result<(), WireError> {
    let length = u32::try_from(field.len()).map_err(|_| WireError::FieldTooLarge)?;
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(field);
    Ok(())
}
fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut normalized = [0_u8; BLOCK];
    if key.len() > BLOCK {
        normalized[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        normalized[..key.len()].copy_from_slice(key);
    }
    let mut inner = [0x36_u8; BLOCK];
    let mut outer = [0x5c_u8; BLOCK];
    for index in 0..BLOCK {
        inner[index] ^= normalized[index];
        outer[index] ^= normalized[index];
    }
    let mut ih = Sha256::new();
    ih.update(inner);
    ih.update(message);
    let mut digest: [u8; 32] = ih.finalize().into();
    let mut oh = Sha256::new();
    oh.update(outer);
    oh.update(digest);
    let result = oh.finalize().into();
    normalized.zeroize();
    inner.zeroize();
    outer.zeroize();
    digest.zeroize();
    result
}
