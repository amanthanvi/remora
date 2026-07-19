//! Host-port correlation helpers kept separate from lifecycle policy.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use p256::ecdsa::signature::Verifier as _;
use p256::ecdsa::{Signature, VerifyingKey};
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
    let signature = Signature::from_der(&signature_bytes).map_err(|_| {
        signature_bytes.zeroize();
        CredentialPortError::InvalidSignature
    })?;
    if signature.to_der().as_bytes() != signature_bytes {
        signature_bytes.zeroize();
        return Err(CredentialPortError::InvalidSignature);
    }
    let mut public_key = match URL_SAFE_NO_PAD.decode(&hardware_key.public_key) {
        Ok(public_key) => public_key,
        Err(_) => {
            signature_bytes.zeroize();
            return Err(CredentialPortError::InvalidSignature);
        }
    };
    let verifying_key = VerifyingKey::from_sec1_bytes(&public_key).map_err(|_| {
        public_key.zeroize();
        signature_bytes.zeroize();
        CredentialPortError::InvalidSignature
    })?;
    public_key.zeroize();
    if verifying_key.verify(transcript, &signature).is_err() {
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

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use p256::ecdsa::signature::Signer as _;
    use p256::ecdsa::{Signature, SigningKey};

    use super::*;

    struct SigningCustody {
        signing_key: SigningKey,
        message_override: Option<&'static [u8]>,
    }

    #[async_trait]
    impl CredentialCustodyPortV2 for SigningCustody {
        async fn ensure_hardware_key(
            &self,
            _host_id: &str,
        ) -> Result<HardwareKeyV2, CredentialPortError> {
            unreachable!()
        }

        async fn load_hardware_key(
            &self,
            _slot: &str,
        ) -> Result<Option<HardwareKeyV2>, CredentialPortError> {
            unreachable!()
        }

        async fn sign_message(
            &self,
            _slot: &str,
            message: &[u8],
        ) -> Result<Vec<u8>, CredentialPortError> {
            let signature: Signature = self
                .signing_key
                .sign(self.message_override.unwrap_or(message));
            Ok(signature.to_der().as_bytes().to_vec())
        }

        async fn delete_hardware_key(&self, _slot: &str) -> Result<(), CredentialPortError> {
            unreachable!()
        }
    }

    fn hardware_key(signing_key: &SigningKey) -> HardwareKeyV2 {
        HardwareKeyV2 {
            slot: "remora-link:test".to_string(),
            public_key: URL_SAFE_NO_PAD.encode(
                signing_key
                    .verifying_key()
                    .to_encoded_point(false)
                    .as_bytes(),
            ),
        }
    }

    #[tokio::test]
    async fn proof_signing_rejects_a_signature_from_the_wrong_hardware_key() {
        let expected = SigningKey::from_bytes((&[7_u8; 32]).into()).unwrap();
        let wrong = SigningKey::from_bytes((&[9_u8; 32]).into()).unwrap();
        let result = sign_proof(
            &SigningCustody {
                signing_key: wrong,
                message_override: None,
            },
            &hardware_key(&expected),
            &URL_SAFE_NO_PAD.encode([3_u8; 16]),
            b"canonical proof transcript",
        )
        .await;

        assert_eq!(result, Err(CredentialPortError::InvalidSignature));
    }

    #[tokio::test]
    async fn proof_signing_rejects_a_signature_over_the_wrong_transcript() {
        let signing_key = SigningKey::from_bytes((&[7_u8; 32]).into()).unwrap();
        let hardware_key = hardware_key(&signing_key);
        let result = sign_proof(
            &SigningCustody {
                signing_key,
                message_override: Some(b"different transcript"),
            },
            &hardware_key,
            &URL_SAFE_NO_PAD.encode([3_u8; 16]),
            b"canonical proof transcript",
        )
        .await;

        assert_eq!(result, Err(CredentialPortError::InvalidSignature));
    }
}
