//! Exact, shared-Rust Remora Link v2 wire contract.
//!
//! This module is the one place that parses the v2 control plane, creates the
//! byte transcripts a platform-owned non-exportable P-256 key signs, and owns
//! the nonsecret crash-recovery lifecycle. Platform UI and key material stay
//! behind narrow semantic ports.

mod client;
mod host_port;
mod lifecycle;
mod runtime_transport;
mod v2_journal;
mod v2_ports;
mod wire;

#[allow(unused_imports)]
pub(crate) use client::{
    ControlExchangeError, read_response_frame, write_proof_frame, write_request_frame,
};
#[allow(unused_imports)]
pub(crate) use lifecycle::{
    EnrollmentOutcomeV2, ForgetOutcomeV2, LifecycleErrorV2, MutationOutcomeV2, PairingLifecycleV2,
    ReconnectOutcomeV2, RecoveryOutcomeV2, RestartOutcomeV2,
};
#[allow(unused_imports)]
pub(crate) use runtime_transport::{
    AttachmentCustodyErrorV2, RemoraLinkRuntimeStreamV2, RetainedAttachmentRegistryV2,
    connect_runtime_client_v2,
};
#[allow(unused_imports)]
pub(crate) use v2_journal::{
    CredentialJournalV2, EnrollmentCandidateJournalV2, EnrollmentJournalV2, HostBindingJournalV2,
    InvitationJournalV2, JOURNAL_SCHEMA_VERSION, JournalPhaseV2, JournalPortErrorV2, JournalPortV2,
    MutationJournalV2, MutationKindV2, PairingJournalEntryV2, PendingClaimJournalV2,
    QuarantineReasonV2, RestartCommandJournalV2, RestartDispositionV2, RevocationReceiptJournalV2,
};
#[allow(unused_imports)]
pub(crate) use v2_ports::{
    CredentialCustodyPortV2, CredentialPortError, EntropyPortV2, FinishedExchangeV2, HardwareKeyV2,
    HostPortErrorV2, HostPortV2, HostRouteV2, StartedExchangeV2,
};
#[allow(unused_imports)]
pub(crate) use wire::{
    ALPN, AgentCapabilitiesV2, AgentInfoV2, AgentPresentationV2, AgentWireV2, AttachKindV2,
    ConfirmationModeV2, DeviceScopeV2, EnrolledDeviceV2, EnrollmentConfirmationV2,
    EnrollmentTranscriptInput, ErrorCodeV2, InvitationInspectionV2, PendingEnrollmentV2,
    ProofChallengeV2, ProofTranscriptInput, ProofV2, RequestCorrelationV2, RequestV2, ResponseV2,
    RestartResultV2, RestartStatusV2, RevocationReceiptV2, RuntimeOfferV2, SessionV2, WireError,
    derive_sas, encode_enrollment_transcript, encode_host_policy_transcript,
    encode_proof_transcript, encode_prospective_credential_material, enrollment_transcript_hash,
    host_policy_digest, operation_payload_hash, prospective_credential_id, validate_policy,
};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod lifecycle_tests;
