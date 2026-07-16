//! Exact, shared-Rust Remora Link v2 wire contract.
//!
//! This module deliberately has no persistence, lifecycle, or platform UI
//! policy. It is the one place that parses the v2 control plane and creates
//! the byte transcripts a platform-owned non-exportable P-256 key signs.

mod client;
mod wire;

#[allow(unused_imports)]
pub(crate) use client::{
    ControlExchangeError, read_proof_frame, read_request_frame, read_response_frame,
    write_proof_frame, write_request_frame,
};
#[allow(unused_imports)]
pub(crate) use wire::{
    ALPN, AgentCapabilitiesV2, AgentInfoV2, AgentPresentationV2, AgentWireV2, AttachKindV2,
    ConfirmationModeV2, DeviceScopeV2, EnrolledDeviceV2, EnrollmentConfirmationV2,
    EnrollmentTranscriptInput, ErrorCodeV2, InvitationInspectionV2, PendingEnrollmentV2,
    ProofChallengeV2, ProofTranscriptInput, ProofV2, RequestV2, ResponseV2, RestartResultV2,
    RestartStatusV2, RevocationReceiptV2, RuntimeOfferV2, SessionV2, WireError, derive_sas,
    encode_enrollment_transcript, encode_host_policy_transcript, encode_proof_transcript,
    encode_prospective_credential_material, enrollment_transcript_hash, host_policy_digest,
    operation_payload_hash, prospective_credential_id, validate_policy, verify_proof_signature,
};

#[cfg(test)]
mod tests;
