//! Host-port correlation helpers kept separate from lifecycle policy.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use p256::ecdsa::Signature;
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use super::v2_ports::{CredentialCustodyPortV2, CredentialPortError, HardwareKeyV2};
use super::wire::{ProofTranscriptInput, ProofV2, RequestV2, WireError, encode_proof_transcript};

pub(super) fn proof_transcript(
    request: &RequestV2,
    challenge: &super::wire::ProofChallengeV2,
    host_endpoint_id: &str,
    client_endpoint_id: &str,
    hardware_key: &HardwareKeyV2,
) -> Result<Vec<u8>, WireError> {
    request.validate()?;
    challenge.validate()?;
    let mut key_bytes = URL_SAFE_NO_PAD
        .decode(&hardware_key.public_key)
        .map_err(|_| WireError::InvalidProof)?;
    let key_hash: [u8; 32] = Sha256::digest(&key_bytes).into();
    key_bytes.zeroize();
    let payload_hash = request.operation_payload_hash()?;
    encode_proof_transcript(ProofTranscriptInput {
        host_endpoint_id,
        client_endpoint_id,
        operation: request.operation(),
        credential_id: &challenge.credential_id,
        auth_epoch: challenge.auth_epoch,
        device_key_hash: &key_hash,
        challenge_id: &challenge.challenge_id,
        server_nonce: &challenge.server_nonce,
        client_nonce: request.client_nonce(),
        operation_payload_hash: &payload_hash,
    })
}

pub(super) async fn sign_proof(
    custody: &dyn CredentialCustodyPortV2,
    hardware_key: &HardwareKeyV2,
    challenge_id: &str,
    transcript: &[u8],
) -> Result<ProofV2, CredentialPortError> {
    let mut signature_bytes = custody.sign_message(&hardware_key.slot, transcript).await?;
    let signature =
        Signature::from_der(&signature_bytes).map_err(|_| CredentialPortError::InvalidSignature)?;
    if signature.to_der().as_bytes() != signature_bytes {
        signature_bytes.zeroize();
        return Err(CredentialPortError::InvalidSignature);
    }
    let signature_text = URL_SAFE_NO_PAD.encode(&signature_bytes);
    signature_bytes.zeroize();
    let proof = ProofV2 {
        v: super::wire::PROTOCOL_VERSION,
        challenge_id: challenge_id.to_string(),
        signature: signature_text,
    };
    proof
        .validate()
        .map_err(|_| CredentialPortError::InvalidSignature)?;
    Ok(proof)
}
