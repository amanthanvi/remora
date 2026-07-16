use base64::Engine as _;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::client::MAX_CONTROL_FRAME_BYTES;
use super::wire::*;

const HOST_ENDPOINT_ID: &str = "af06a3e3291714e4f356c19c9b15cd1951ec6e6662aa77be07547f289383341d";
const CLIENT_ENDPOINT_ID: &str = "2df04125f0015afb47ce853aef8772094ff9498c14cb1b9e12973c2927da0fa6";
const DEVICE_PUBLIC_KEY: &str =
    "BB4YUy_UdUwC8wQdnHXOszuD_9gax85P6ILMscmLxYlupGwxHE4v9A3ZajZT5uRURdMt_khuztdcepDGoYiBwKM";
const CREDENTIAL_ID: &str = "AgICAgICAgICAgICAgICAg";
const CHALLENGE_ID: &str = "AwMDAwMDAwMDAwMDAwMDAw";
const SERVER_NONCE: &str = "ERERERERERERERERERERERERERERERERERERERERERE";
const CLIENT_NONCE: &str = "IiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiI";
const INVITATION_SECRET: &str = "REREREREREREREREREREREREREREREREREREREREREQ";

const INVITATION_ID: &str = "AQEBAQEBAQEBAQEBAQEBAQ";

#[derive(Debug, Deserialize)]
struct GoldenVectors {
    schema_version: u32,
    protocol: GoldenProtocol,
    vector: GoldenVector,
    standalone_sas_vector: StandaloneSasVector,
}

#[derive(Debug, Deserialize)]
struct GoldenProtocol {
    wire_version: u32,
    alpn: String,
    proof_domain: String,
    payload_domain: String,
    enrollment_domain: String,
    policy_domain: String,
    prospective_credential_domain: String,
    sas_domain: String,
    frame_length_encoding: String,
    binary_json_encoding: String,
}

#[derive(Debug, Deserialize)]
struct GoldenVector {
    inputs: GoldenInputs,
    prospective_credential_material_hex: String,
    prospective_credential_sha256_hex: String,
    prospective_credential_id: String,
    list_agents_request_json: String,
    operation_payload_sha256_hex: String,
    proof_transcript_hex: String,
    proof_transcript_sha256_hex: String,
    proof_signature_der_base64url: String,
    restart_request_json: String,
    restart_operation_payload_sha256_hex: String,
    restart_proof_transcript_hex: String,
    restart_proof_transcript_sha256_hex: String,
    restart_proof_signature_der_base64url: String,
    restart_succeeded_response_json: String,
    restart_outcome_unknown_response_json: String,
    host_policy_transcript_hex: String,
    host_policy_digest_sha256_hex: String,
    enrollment_transcript_hex: String,
    enrollment_transcript_sha256_hex: String,
    enrollment_transcript_sha256_base64url: String,
    sas: String,
}

#[derive(Debug, Deserialize)]
struct GoldenInputs {
    host_iroh_endpoint_id: String,
    client_iroh_endpoint_id: String,
    device_public_key_sec1_base64url: String,
    device_public_key_sha256_hex: String,
    invitation_id: String,
    credential_id: String,
    challenge_id: String,
    server_nonce: String,
    client_nonce: String,
    invitation_secret_base64url: String,
    operation: String,
    auth_epoch: u64,
    restart_agent: String,
    restart_idempotency_key: String,
    restart_command_sequence: u64,
    enrollment_idempotency_key: String,
    selected_runtime_ids: Vec<String>,
    requested_scopes: Vec<String>,
    confirmation_mode: String,
    max_runtime_ids: Vec<String>,
    max_scopes: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct StandaloneSasVector {
    invitation_secret_base64url: String,
    enrollment_transcript_sha256_hex: String,
    sas: String,
}

fn golden_vectors() -> GoldenVectors {
    serde_json::from_str(include_str!(
        "../../../tests/fixtures/remora-link-v2/golden-vectors.json"
    ))
    .expect("pinned Remora Link v2 golden vectors must parse")
}

fn list_agents_request() -> RequestV2 {
    RequestV2::decode_json(
        format!(
            r#"{{"op":"list_agents","v":2,"credential_id":"{CREDENTIAL_ID}","client_nonce":"{CLIENT_NONCE}"}}"#
        )
        .as_bytes(),
    )
    .expect("published list-agents request is valid")
}

fn published_challenge() -> ProofChallengeV2 {
    ProofChallengeV2 {
        challenge_id: CHALLENGE_ID.to_string(),
        credential_id: CREDENTIAL_ID.to_string(),
        auth_epoch: 7,
        server_nonce: SERVER_NONCE.to_string(),
        expires_at: 1_900_000_000,
    }
}

#[test]
fn published_proof_vector_matches_and_verifies() {
    let request = list_agents_request();
    let payload_hash = request.operation_payload_hash().unwrap();
    assert_eq!(
        hex::encode(payload_hash),
        "42151db40bc779d55edc20fc6cefd8580eec3e37854e648c83910458af7fb734"
    );

    let device_key_hash: [u8; 32] = Sha256::digest(
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(DEVICE_PUBLIC_KEY)
            .unwrap(),
    )
    .into();
    let transcript = encode_proof_transcript(ProofTranscriptInput {
        host_endpoint_id: HOST_ENDPOINT_ID,
        client_endpoint_id: CLIENT_ENDPOINT_ID,
        operation: request.operation(),
        credential_id: CREDENTIAL_ID,
        auth_epoch: 7,
        device_key_hash: &device_key_hash,
        challenge_id: CHALLENGE_ID,
        server_nonce: SERVER_NONCE,
        client_nonce: CLIENT_NONCE,
        operation_payload_hash: &payload_hash,
    })
    .unwrap();
    assert_eq!(
        hex::encode(Sha256::digest(&transcript)),
        "a3a0262ed8cb684fea0691e61b74c35f4e0b1422a065bb7bc647f905d296f8ae"
    );

    let proof = ProofV2::decode_json(
        format!(
            r#"{{"v":2,"challenge_id":"{CHALLENGE_ID}","signature":"MEUCIA1jjLu3EUSEj2880Pk7IuEgmcF5TmgK_KMNyMKqJYlmAiEAi22sPZkFhV0XsBhmrNdHNmtMRcZi1xXnW-euHOUm1zA"}}"#
        )
        .as_bytes(),
    )
    .unwrap();
    verify_proof_signature(
        &request,
        &published_challenge(),
        &proof,
        HOST_ENDPOINT_ID,
        CLIENT_ENDPOINT_ID,
        DEVICE_PUBLIC_KEY,
    )
    .unwrap();
}

#[test]
fn published_sas_vectors_match() {
    let transcript_hash = [0x55; 32];
    assert_eq!(
        derive_sas(INVITATION_SECRET, &transcript_hash).unwrap(),
        "3PT-GWZ"
    );
}

#[test]
fn strict_parsers_reject_unknown_fields() {
    let request = format!(
        r#"{{"op":"list_agents","v":2,"credential_id":"{CREDENTIAL_ID}","client_nonce":"{CLIENT_NONCE}","legacy":true}}"#
    );
    assert_eq!(
        RequestV2::decode_json(request.as_bytes()),
        Err(WireError::InvalidRequest)
    );

    let proof = format!(
        r#"{{"v":2,"challenge_id":"{CHALLENGE_ID}","signature":"MEUCIA1jjLu3EUSEj2880Pk7IuEgmcF5TmgK_KMNyMKqJYlmAiEAi22sPZkFhV0XsBhmrNdHNmtMRcZi1xXnW-euHOUm1zA","legacy":true}}"#
    );
    assert_eq!(
        ProofV2::decode_json(proof.as_bytes()),
        Err(WireError::InvalidProof)
    );

    let response = br#"{"v":2,"ok":false,"error_code":"invalid_request","error":"invalid request","legacy":true}"#;
    assert_eq!(
        ResponseV2::decode_json(response),
        Err(WireError::InvalidResponse)
    );
}

#[test]
fn policy_validation_enforces_canonical_grants() {
    let runtimes = vec!["claude".to_string(), "codex".to_string()];
    let scopes = vec![
        DeviceScopeV2::InspectRuntimes,
        DeviceScopeV2::ConnectRuntime,
        DeviceScopeV2::RestartRuntime,
        DeviceScopeV2::SelfRevoke,
    ];
    assert_eq!(
        validate_policy(&runtimes, &scopes, ConfirmationModeV2::Interactive),
        Ok(())
    );
    assert_eq!(
        validate_policy(
            &["codex".to_string(), "claude".to_string()],
            &scopes,
            ConfirmationModeV2::Interactive,
        ),
        Err(WireError::InvalidPolicy)
    );
    assert_eq!(
        validate_policy(&runtimes, &scopes, ConfirmationModeV2::Unattended),
        Err(WireError::InvalidPolicy)
    );
}

#[test]
fn pinned_fixture_declares_the_exact_v2_domains_and_encoding() {
    let fixture = golden_vectors();
    assert_eq!(fixture.schema_version, 1);
    assert_eq!(fixture.protocol.wire_version, PROTOCOL_VERSION);
    assert_eq!(fixture.protocol.alpn.as_bytes(), ALPN);
    assert_eq!(fixture.protocol.proof_domain.as_bytes(), PROOF_DOMAIN);
    assert_eq!(fixture.protocol.payload_domain.as_bytes(), PAYLOAD_DOMAIN);
    assert_eq!(
        fixture.protocol.enrollment_domain.as_bytes(),
        ENROLLMENT_DOMAIN
    );
    assert_eq!(fixture.protocol.policy_domain.as_bytes(), POLICY_DOMAIN);
    assert_eq!(
        fixture.protocol.prospective_credential_domain.as_bytes(),
        PROSPECTIVE_CREDENTIAL_DOMAIN
    );
    assert_eq!(fixture.protocol.sas_domain.as_bytes(), SAS_DOMAIN);
    assert_eq!(fixture.protocol.frame_length_encoding, "u32be");
    assert_eq!(fixture.protocol.binary_json_encoding, "base64url-no-pad");
}

#[test]
fn all_seven_requests_have_exact_canonical_json_shapes() {
    let fixture = golden_vectors();
    let requests = [
        format!(
            r#"{{"op":"inspect_invitation","v":2,"invitation_id":"{INVITATION_ID}","secret":"{INVITATION_SECRET}","device_public_key":"{DEVICE_PUBLIC_KEY}","client_nonce":"{CLIENT_NONCE}"}}"#
        ),
        format!(
            r#"{{"op":"enroll","v":2,"invitation_id":"{INVITATION_ID}","secret":"{INVITATION_SECRET}","device_name":"Remora Phone","device_public_key":"{DEVICE_PUBLIC_KEY}","selected_runtime_ids":["codex"],"requested_scopes":["inspect_runtimes","connect_runtime","self_revoke"],"idempotency_key":"enroll-operation-0001","client_nonce":"{CLIENT_NONCE}"}}"#
        ),
        fixture.vector.list_agents_request_json,
        fixture.vector.restart_request_json,
        format!(
            r#"{{"op":"connect","v":2,"credential_id":"{CREDENTIAL_ID}","client_nonce":"{CLIENT_NONCE}","agent":"codex","resume":{{"last_seq":42}}}}"#
        ),
        format!(
            r#"{{"op":"revoke_self","v":2,"credential_id":"{CREDENTIAL_ID}","client_nonce":"{CLIENT_NONCE}","idempotency_key":"revoke-operation-0001"}}"#
        ),
        format!(
            r#"{{"op":"rollback_enrollment","v":2,"credential_id":"{CREDENTIAL_ID}","enrollment_idempotency_key":"enroll-operation-0001","client_nonce":"{CLIENT_NONCE}","idempotency_key":"rollback-operation-0001"}}"#
        ),
    ];

    let expected_operations = [
        "inspect_invitation",
        "enroll",
        "list_agents",
        "restart_agent",
        "connect",
        "revoke_self",
        "rollback_enrollment",
    ];
    for (json, operation) in requests.iter().zip(expected_operations) {
        let request = RequestV2::decode_json(json.as_bytes()).unwrap();
        assert_eq!(request.operation(), operation);
        assert_eq!(serde_json::to_string(&request).unwrap(), *json);
    }
}

fn decode_response(value: serde_json::Value) -> Result<ResponseV2, WireError> {
    ResponseV2::decode_json(&serde_json::to_vec(&value).unwrap())
}

fn confirmation_json() -> serde_json::Value {
    serde_json::json!({
        "transcript_hash": CLIENT_NONCE,
        "sas": "ABC-123"
    })
}

#[test]
fn typed_responses_accept_every_terminal_result_shape() {
    let inspection = serde_json::json!({
        "v": 2,
        "ok": true,
        "inspection": {
            "invitation_id": INVITATION_ID,
            "expires_at": 1_900_000_000_i64,
            "max_runtime_ids": ["claude", "codex"],
            "max_scopes": [
                "inspect_runtimes", "connect_runtime", "restart_runtime", "self_revoke"
            ],
            "confirmation_mode": "interactive",
            "runtime_offers": [
                {"runtime_id": "codex", "display_name": "Codex", "available": true, "recommended": true}
            ]
        }
    });
    let pending = serde_json::json!({
        "v": 2,
        "ok": true,
        "pending": {
            "claim_id": "BAQEBAQEBAQEBAQEBAQEBA",
            "credential_id": CREDENTIAL_ID,
            "display_name": "Remora Phone",
            "selected_runtime_ids": ["codex"],
            "requested_scopes": ["inspect_runtimes", "connect_runtime", "self_revoke"],
            "enrollment_confirmation": confirmation_json(),
            "created_at": 1_800_000_000_i64,
            "expires_at": 1_900_000_000_i64
        }
    });
    let enrolled = serde_json::json!({
        "v": 2,
        "ok": true,
        "enrolled": {
            "device_id": CREDENTIAL_ID,
            "display_name": "Remora Phone",
            "endpoint_fingerprint": "0123456789abcdef",
            "device_key_fingerprint": "fedcba9876543210",
            "selected_runtime_ids": ["codex"],
            "granted_scopes": ["inspect_runtimes", "connect_runtime", "self_revoke"],
            "auth_epoch": 1,
            "created_at": 1_800_000_000_i64,
            "enrollment_confirmation": confirmation_json()
        }
    });
    let revocation = serde_json::json!({
        "v": 2,
        "ok": true,
        "revocation": {
            "credential_id": CREDENTIAL_ID,
            "auth_epoch": 8,
            "revoked_at": 1_900_000_000_i64,
            "idempotency_key": "revoke-operation-0001"
        }
    });
    let agents = serde_json::json!({
        "v": 2,
        "ok": true,
        "agents": [{
            "name": "codex",
            "display_name": "Codex",
            "wire": "jsonl",
            "available": true,
            "presentation": {"title": "Codex", "aliases": ["codex-cli"]},
            "capabilities": {"supports_ssh_bridge": true}
        }]
    });
    let session = serde_json::json!({
        "v": 2,
        "ok": true,
        "session": {"attached": "resumed", "current_seq": 42, "floor_seq": 1}
    });

    for response in [inspection, pending, enrolled, revocation, agents, session] {
        decode_response(response).unwrap();
    }

    let fixture = golden_vectors();
    let succeeded =
        ResponseV2::decode_json(fixture.vector.restart_succeeded_response_json.as_bytes()).unwrap();
    assert_eq!(
        serde_json::to_string(&succeeded).unwrap(),
        fixture.vector.restart_succeeded_response_json
    );
    ResponseV2::decode_json(
        fixture
            .vector
            .restart_outcome_unknown_response_json
            .as_bytes(),
    )
    .unwrap();
}

#[test]
fn response_envelope_invariants_reject_ambiguous_or_incoherent_results() {
    let challenge = serde_json::json!({
        "challenge_id": CHALLENGE_ID,
        "credential_id": CREDENTIAL_ID,
        "auth_epoch": 7,
        "server_nonce": SERVER_NONCE,
        "expires_at": 1_900_000_000_i64
    });
    decode_response(serde_json::json!({"v": 2, "ok": true, "challenge": challenge})).unwrap();

    let invalid = [
        serde_json::json!({"v": 2, "ok": true}),
        serde_json::json!({"v": 2, "ok": false}),
        serde_json::json!({"v": 2, "ok": false, "error_code": "invalid_request", "error": "wrong"}),
        serde_json::json!({"v": 2, "ok": true, "challenge": challenge, "agents": []}),
        serde_json::json!({"v": 2, "ok": true, "agents": [], "session": {"attached": "fresh", "current_seq": 0, "floor_seq": 0}}),
        serde_json::json!({"v": 2, "ok": true, "restart": {"agent": "codex", "idempotency_key": "restart-1", "command_sequence": 1, "status": "outcome_unknown"}}),
    ];
    for value in invalid {
        assert_eq!(decode_response(value), Err(WireError::InvalidResponse));
    }

    decode_response(serde_json::json!({
        "v": 2,
        "ok": false,
        "error_code": "invalid_request",
        "error": "invalid request"
    }))
    .unwrap();
    decode_response(serde_json::json!({
        "v": 2,
        "ok": false,
        "error_code": "outcome_unknown",
        "error": "operation outcome unknown"
    }))
    .unwrap();
}

fn challenge_for(credential_id: &str, auth_epoch: u64) -> ProofChallengeV2 {
    ProofChallengeV2 {
        challenge_id: CHALLENGE_ID.to_string(),
        credential_id: credential_id.to_string(),
        auth_epoch,
        server_nonce: SERVER_NONCE.to_string(),
        expires_at: 1_900_000_000,
    }
}

fn assert_correlated_exchange(
    request: RequestV2,
    challenge: ProofChallengeV2,
    terminal: serde_json::Value,
) {
    let challenge_response = decode_response(serde_json::json!({
        "v": 2,
        "ok": true,
        "challenge": challenge
    }))
    .unwrap();
    let returned = challenge_response
        .validate_challenge_for_request(&request, CLIENT_ENDPOINT_ID)
        .unwrap()
        .clone();
    decode_response(terminal)
        .unwrap()
        .validate_terminal_shape_for_request(&request, &returned, CLIENT_ENDPOINT_ID)
        .unwrap();
}

#[test]
fn challenge_and_terminal_results_correlate_to_all_seven_operations() {
    let inspect = RequestV2::decode_json(
        format!(
            r#"{{"op":"inspect_invitation","v":2,"invitation_id":"{INVITATION_ID}","secret":"{INVITATION_SECRET}","device_public_key":"{DEVICE_PUBLIC_KEY}","client_nonce":"{CLIENT_NONCE}"}}"#
        )
        .as_bytes(),
    )
    .unwrap();
    assert_correlated_exchange(
        inspect,
        challenge_for(INVITATION_ID, 0),
        serde_json::json!({
            "v": 2, "ok": true,
            "inspection": {
                "invitation_id": INVITATION_ID,
                "expires_at": 1_900_000_000_i64,
                "max_runtime_ids": ["codex"],
                "max_scopes": ["inspect_runtimes", "connect_runtime", "self_revoke"],
                "confirmation_mode": "interactive",
                "runtime_offers": [{"runtime_id": "codex", "display_name": "Codex", "available": true, "recommended": true}]
            }
        }),
    );

    let enroll = RequestV2::decode_json(
        format!(
            r#"{{"op":"enroll","v":2,"invitation_id":"{INVITATION_ID}","secret":"{INVITATION_SECRET}","device_name":"Remora Phone","device_public_key":"{DEVICE_PUBLIC_KEY}","selected_runtime_ids":["codex"],"requested_scopes":["inspect_runtimes","connect_runtime","self_revoke"],"idempotency_key":"enroll-operation-0001","client_nonce":"{CLIENT_NONCE}"}}"#
        )
        .as_bytes(),
    )
    .unwrap();
    let prospective = "TVlQRnHHDOmAcm0CPoyQdw";
    assert_correlated_exchange(
        enroll,
        challenge_for(prospective, 0),
        serde_json::json!({
            "v": 2, "ok": true,
            "pending": {
                "claim_id": "BAQEBAQEBAQEBAQEBAQEBA",
                "credential_id": prospective,
                "display_name": "Remora Phone",
                "selected_runtime_ids": ["codex"],
                "requested_scopes": ["inspect_runtimes", "connect_runtime", "self_revoke"],
                "enrollment_confirmation": confirmation_json(),
                "created_at": 1_800_000_000_i64,
                "expires_at": 1_900_000_000_i64
            }
        }),
    );

    assert_correlated_exchange(
        list_agents_request(),
        published_challenge(),
        serde_json::json!({"v": 2, "ok": true, "agents": []}),
    );

    let fixture = golden_vectors();
    let restart = RequestV2::decode_json(fixture.vector.restart_request_json.as_bytes()).unwrap();
    assert_correlated_exchange(
        restart,
        published_challenge(),
        serde_json::from_str(&fixture.vector.restart_succeeded_response_json).unwrap(),
    );

    let connect = RequestV2::decode_json(
        format!(
            r#"{{"op":"connect","v":2,"credential_id":"{CREDENTIAL_ID}","client_nonce":"{CLIENT_NONCE}","agent":"codex","resume":{{"last_seq":42}}}}"#
        )
        .as_bytes(),
    )
    .unwrap();
    assert_correlated_exchange(
        connect,
        published_challenge(),
        serde_json::json!({"v": 2, "ok": true, "session": {"attached": "resumed", "current_seq": 42, "floor_seq": 1}}),
    );

    let revoke = RequestV2::decode_json(
        format!(
            r#"{{"op":"revoke_self","v":2,"credential_id":"{CREDENTIAL_ID}","client_nonce":"{CLIENT_NONCE}","idempotency_key":"revoke-operation-0001"}}"#
        )
        .as_bytes(),
    )
    .unwrap();
    assert_correlated_exchange(
        revoke,
        published_challenge(),
        serde_json::json!({"v": 2, "ok": true, "revocation": {"credential_id": CREDENTIAL_ID, "auth_epoch": 8, "revoked_at": 1_900_000_000_i64, "idempotency_key": "revoke-operation-0001"}}),
    );

    let rollback = RequestV2::decode_json(
        format!(
            r#"{{"op":"rollback_enrollment","v":2,"credential_id":"{CREDENTIAL_ID}","enrollment_idempotency_key":"enroll-operation-0001","client_nonce":"{CLIENT_NONCE}","idempotency_key":"rollback-operation-0001"}}"#
        )
        .as_bytes(),
    )
    .unwrap();
    assert_correlated_exchange(
        rollback,
        published_challenge(),
        serde_json::json!({"v": 2, "ok": true, "revocation": {"credential_id": CREDENTIAL_ID, "auth_epoch": 8, "revoked_at": 1_900_000_000_i64, "idempotency_key": "rollback-operation-0001"}}),
    );
}

#[test]
fn prospective_credential_and_proof_vectors_match_the_pinned_host() {
    let fixture = golden_vectors();
    let vector = fixture.vector;
    let input = &vector.inputs;
    assert_eq!(input.invitation_id, INVITATION_ID);
    assert_eq!(input.credential_id, CREDENTIAL_ID);
    assert_eq!(input.challenge_id, CHALLENGE_ID);
    assert_eq!(input.server_nonce, SERVER_NONCE);
    assert_eq!(input.client_nonce, CLIENT_NONCE);

    let material = encode_prospective_credential_material(
        &input.invitation_id,
        &input.client_iroh_endpoint_id,
        &input.device_public_key_sec1_base64url,
        &input.enrollment_idempotency_key,
    )
    .unwrap();
    assert_eq!(
        hex::encode(&material),
        vector.prospective_credential_material_hex
    );
    assert_eq!(
        hex::encode(Sha256::digest(&material)),
        vector.prospective_credential_sha256_hex
    );
    assert_eq!(
        prospective_credential_id(
            &input.invitation_id,
            &input.client_iroh_endpoint_id,
            &input.device_public_key_sec1_base64url,
            &input.enrollment_idempotency_key,
        )
        .unwrap(),
        vector.prospective_credential_id
    );

    let list_request = RequestV2::decode_json(vector.list_agents_request_json.as_bytes()).unwrap();
    assert_eq!(list_request.operation(), input.operation);
    let list_payload_hash = list_request.operation_payload_hash().unwrap();
    assert_eq!(
        hex::encode(list_payload_hash),
        vector.operation_payload_sha256_hex
    );
    let device_key = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&input.device_public_key_sec1_base64url)
        .unwrap();
    assert_eq!(
        hex::encode(Sha256::digest(&device_key)),
        input.device_public_key_sha256_hex
    );
    let device_key_hash: [u8; 32] = Sha256::digest(&device_key).into();
    let list_transcript = encode_proof_transcript(ProofTranscriptInput {
        host_endpoint_id: &input.host_iroh_endpoint_id,
        client_endpoint_id: &input.client_iroh_endpoint_id,
        operation: list_request.operation(),
        credential_id: &input.credential_id,
        auth_epoch: input.auth_epoch,
        device_key_hash: &device_key_hash,
        challenge_id: &input.challenge_id,
        server_nonce: &input.server_nonce,
        client_nonce: &input.client_nonce,
        operation_payload_hash: &list_payload_hash,
    })
    .unwrap();
    assert_eq!(hex::encode(&list_transcript), vector.proof_transcript_hex);
    assert_eq!(
        hex::encode(Sha256::digest(&list_transcript)),
        vector.proof_transcript_sha256_hex
    );
    let list_proof = ProofV2::decode_json(
        serde_json::json!({
            "v": 2,
            "challenge_id": input.challenge_id,
            "signature": vector.proof_signature_der_base64url
        })
        .to_string()
        .as_bytes(),
    )
    .unwrap();
    verify_proof_signature(
        &list_request,
        &published_challenge(),
        &list_proof,
        &input.host_iroh_endpoint_id,
        &input.client_iroh_endpoint_id,
        &input.device_public_key_sec1_base64url,
    )
    .unwrap();

    let restart_request = RequestV2::decode_json(vector.restart_request_json.as_bytes()).unwrap();
    let restart_payload_hash = restart_request.operation_payload_hash().unwrap();
    assert_eq!(
        hex::encode(restart_payload_hash),
        vector.restart_operation_payload_sha256_hex
    );
    assert_eq!(input.restart_agent, "codex");
    assert_eq!(input.restart_idempotency_key, "restart-operation-0001");
    assert_eq!(input.restart_command_sequence, 1);
    let restart_transcript = encode_proof_transcript(ProofTranscriptInput {
        host_endpoint_id: &input.host_iroh_endpoint_id,
        client_endpoint_id: &input.client_iroh_endpoint_id,
        operation: restart_request.operation(),
        credential_id: &input.credential_id,
        auth_epoch: input.auth_epoch,
        device_key_hash: &device_key_hash,
        challenge_id: &input.challenge_id,
        server_nonce: &input.server_nonce,
        client_nonce: &input.client_nonce,
        operation_payload_hash: &restart_payload_hash,
    })
    .unwrap();
    assert_eq!(
        hex::encode(&restart_transcript),
        vector.restart_proof_transcript_hex
    );
    assert_eq!(
        hex::encode(Sha256::digest(&restart_transcript)),
        vector.restart_proof_transcript_sha256_hex
    );
    let restart_proof = ProofV2::decode_json(
        serde_json::json!({
            "v": 2,
            "challenge_id": input.challenge_id,
            "signature": vector.restart_proof_signature_der_base64url
        })
        .to_string()
        .as_bytes(),
    )
    .unwrap();
    verify_proof_signature(
        &restart_request,
        &published_challenge(),
        &restart_proof,
        &input.host_iroh_endpoint_id,
        &input.client_iroh_endpoint_id,
        &input.device_public_key_sec1_base64url,
    )
    .unwrap();
}

#[test]
fn enrollment_policy_and_sas_vectors_match_the_pinned_host() {
    let fixture = golden_vectors();
    let vector = fixture.vector;
    let input = &vector.inputs;
    let selected_runtime_ids = vec!["codex".to_string()];
    let requested_scopes = vec![
        DeviceScopeV2::InspectRuntimes,
        DeviceScopeV2::ConnectRuntime,
        DeviceScopeV2::SelfRevoke,
    ];
    let max_runtime_ids = vec!["claude".to_string(), "codex".to_string()];
    let max_scopes = vec![
        DeviceScopeV2::InspectRuntimes,
        DeviceScopeV2::ConnectRuntime,
        DeviceScopeV2::RestartRuntime,
        DeviceScopeV2::SelfRevoke,
    ];
    assert_eq!(input.selected_runtime_ids, selected_runtime_ids);
    assert_eq!(
        input.requested_scopes,
        ["inspect_runtimes", "connect_runtime", "self_revoke"]
    );
    assert_eq!(input.confirmation_mode, "interactive");
    assert_eq!(input.max_runtime_ids, max_runtime_ids);
    assert_eq!(
        input.max_scopes,
        [
            "inspect_runtimes",
            "connect_runtime",
            "restart_runtime",
            "self_revoke"
        ]
    );

    let policy = encode_host_policy_transcript(&max_runtime_ids, &max_scopes).unwrap();
    assert_eq!(hex::encode(&policy), vector.host_policy_transcript_hex);
    assert_eq!(
        hex::encode(host_policy_digest(&max_runtime_ids, &max_scopes).unwrap()),
        vector.host_policy_digest_sha256_hex
    );

    let enrollment_input = EnrollmentTranscriptInput {
        host_endpoint_id: &input.host_iroh_endpoint_id,
        client_endpoint_id: &input.client_iroh_endpoint_id,
        invitation_id: &input.invitation_id,
        device_public_key: &input.device_public_key_sec1_base64url,
        idempotency_key: &input.enrollment_idempotency_key,
        selected_runtime_ids: &selected_runtime_ids,
        requested_scopes: &requested_scopes,
        server_nonce: &input.server_nonce,
        client_nonce: &input.client_nonce,
        confirmation_mode: ConfirmationModeV2::Interactive,
        max_runtime_ids: &max_runtime_ids,
        max_scopes: &max_scopes,
    };
    let transcript = encode_enrollment_transcript(enrollment_input).unwrap();
    assert_eq!(hex::encode(&transcript), vector.enrollment_transcript_hex);
    let transcript_hash = enrollment_transcript_hash(enrollment_input).unwrap();
    assert_eq!(
        hex::encode(transcript_hash),
        vector.enrollment_transcript_sha256_hex
    );
    assert_eq!(
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(transcript_hash),
        vector.enrollment_transcript_sha256_base64url
    );
    assert_eq!(
        derive_sas(&input.invitation_secret_base64url, &transcript_hash).unwrap(),
        vector.sas
    );

    let standalone_hash: [u8; 32] = hex::decode(
        &fixture
            .standalone_sas_vector
            .enrollment_transcript_sha256_hex,
    )
    .unwrap()
    .try_into()
    .unwrap();
    assert_eq!(
        derive_sas(
            &fixture.standalone_sas_vector.invitation_secret_base64url,
            &standalone_hash,
        )
        .unwrap(),
        fixture.standalone_sas_vector.sas
    );
}

fn valid_enroll_value() -> serde_json::Value {
    serde_json::json!({
        "op": "enroll",
        "v": 2,
        "invitation_id": INVITATION_ID,
        "secret": INVITATION_SECRET,
        "device_name": "Remora Phone",
        "device_public_key": DEVICE_PUBLIC_KEY,
        "selected_runtime_ids": ["codex"],
        "requested_scopes": ["inspect_runtimes", "connect_runtime", "self_revoke"],
        "idempotency_key": "enroll-operation-0001",
        "client_nonce": CLIENT_NONCE
    })
}

fn assert_invalid_request(value: serde_json::Value) {
    assert_eq!(
        RequestV2::decode_json(&serde_json::to_vec(&value).unwrap()),
        Err(WireError::InvalidRequest)
    );
}

fn assert_invalid_policy_request(value: serde_json::Value) {
    assert_eq!(
        RequestV2::decode_json(&serde_json::to_vec(&value).unwrap()),
        Err(WireError::InvalidPolicy)
    );
}

#[test]
fn request_scalar_bounds_and_exact_encodings_are_fail_closed() {
    let mut value = valid_enroll_value();
    value["v"] = serde_json::json!(1);
    assert_invalid_request(value);

    for invalid in ["", "short", "AQEBAQEBAQEBAQEBAQEBAQ=", CREDENTIAL_ID] {
        let mut value = valid_enroll_value();
        value["secret"] = serde_json::json!(invalid);
        assert_invalid_request(value);
    }
    for invalid in ["", "short", "IiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiI="] {
        let mut value = valid_enroll_value();
        value["client_nonce"] = serde_json::json!(invalid);
        assert_invalid_request(value);
    }
    for invalid in ["", "short", "AQEBAQEBAQEBAQEBAQEBAQ="] {
        let mut value = valid_enroll_value();
        value["invitation_id"] = serde_json::json!(invalid);
        assert_invalid_request(value);
    }

    for invalid in ["x".repeat(81), "line\nbreak".to_string()] {
        let mut value = valid_enroll_value();
        value["device_name"] = serde_json::json!(invalid);
        assert_invalid_request(value);
    }
    let mut empty_name = valid_enroll_value();
    empty_name["device_name"] = serde_json::json!("");
    RequestV2::decode_json(&serde_json::to_vec(&empty_name).unwrap()).unwrap();

    for invalid in ["".to_string(), "x".repeat(129), "line\nbreak".to_string()] {
        let mut value = valid_enroll_value();
        value["idempotency_key"] = serde_json::json!(invalid);
        assert_invalid_request(value);
    }

    let mut sequence_zero =
        serde_json::from_str::<serde_json::Value>(&golden_vectors().vector.restart_request_json)
            .unwrap();
    sequence_zero["command_sequence"] = serde_json::json!(0);
    assert_invalid_request(sequence_zero);
}

#[test]
fn documented_scalar_and_collection_boundaries_are_inclusive() {
    let runtimes = (0..16)
        .map(|index| format!("agent-{index:02}"))
        .collect::<Vec<_>>();
    let mut value = valid_enroll_value();
    value["device_name"] = serde_json::json!("é".repeat(40));
    value["selected_runtime_ids"] = serde_json::json!(runtimes);
    value["idempotency_key"] = serde_json::json!("i".repeat(128));
    RequestV2::decode_json(&serde_json::to_vec(&value).unwrap()).unwrap();

    let mut too_many_utf8_bytes = valid_enroll_value();
    too_many_utf8_bytes["device_name"] = serde_json::json!("é".repeat(41));
    assert_invalid_request(too_many_utf8_bytes);

    let payload_hash = list_agents_request().operation_payload_hash().unwrap();
    encode_proof_transcript(ProofTranscriptInput {
        host_endpoint_id: &"h".repeat(256),
        client_endpoint_id: CLIENT_ENDPOINT_ID,
        operation: "list_agents",
        credential_id: CREDENTIAL_ID,
        auth_epoch: 7,
        device_key_hash: &[0_u8; 32],
        challenge_id: CHALLENGE_ID,
        server_nonce: SERVER_NONCE,
        client_nonce: CLIENT_NONCE,
        operation_payload_hash: &payload_hash,
    })
    .unwrap();
    decode_response(serde_json::json!({
        "v": 2,
        "ok": true,
        "agents": [{"name": "x".repeat(64), "display_name": "x".repeat(255), "wire": "jsonl", "available": true}]
    }))
    .unwrap();
}

#[test]
fn standard_or_padded_base64_is_never_accepted_as_base64url_no_pad() {
    let standard_secret = base64::engine::general_purpose::STANDARD_NO_PAD.encode([0xfb_u8; 32]);
    assert!(standard_secret.contains('+') || standard_secret.contains('/'));
    let mut value = valid_enroll_value();
    value["secret"] = serde_json::json!(standard_secret);
    assert_invalid_request(value);

    let standard_nonce = base64::engine::general_purpose::STANDARD_NO_PAD.encode([0xfb_u8; 32]);
    let mut value = valid_enroll_value();
    value["client_nonce"] = serde_json::json!(standard_nonce);
    assert_invalid_request(value);
}

#[test]
fn request_arrays_must_be_bounded_canonical_and_authorizable() {
    let invalid_runtime_lists = [
        serde_json::json!([]),
        serde_json::json!(["codex", "claude"]),
        serde_json::json!(["codex", "codex"]),
        serde_json::json!(["bad runtime"]),
        serde_json::json!(["x".repeat(65)]),
        serde_json::json!((0..17).map(|i| format!("agent-{i:02}")).collect::<Vec<_>>()),
    ];
    for runtimes in invalid_runtime_lists {
        let mut value = valid_enroll_value();
        value["selected_runtime_ids"] = runtimes;
        assert_invalid_policy_request(value);
    }

    let invalid_scope_lists = [
        serde_json::json!([]),
        serde_json::json!(["self_revoke", "connect_runtime"]),
        serde_json::json!(["connect_runtime", "connect_runtime", "self_revoke"]),
        serde_json::json!(["inspect_runtimes", "self_revoke"]),
        serde_json::json!(["connect_runtime"]),
    ];
    for scopes in invalid_scope_lists {
        let mut value = valid_enroll_value();
        value["requested_scopes"] = scopes;
        assert_invalid_policy_request(value);
    }
}

#[test]
fn public_keys_and_proofs_require_exact_sec1_base64url_and_canonical_der() {
    let key_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(DEVICE_PUBLIC_KEY)
        .unwrap();
    let compressed = {
        let mut bytes = Vec::with_capacity(33);
        bytes.push(if key_bytes[64] & 1 == 0 { 2 } else { 3 });
        bytes.extend_from_slice(&key_bytes[1..33]);
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    };
    let invalid_point = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([4_u8; 65]);
    for public_key in [
        compressed,
        invalid_point,
        format!("{DEVICE_PUBLIC_KEY}="),
        "x".repeat(129),
    ] {
        let mut value = valid_enroll_value();
        value["device_public_key"] = serde_json::json!(public_key);
        assert_invalid_request(value);
    }

    let fixture = golden_vectors();
    let valid_signature = fixture.vector.proof_signature_der_base64url;
    let invalid_proofs = [
        serde_json::json!({"v": 1, "challenge_id": CHALLENGE_ID, "signature": valid_signature}),
        serde_json::json!({"v": 2, "challenge_id": "short", "signature": valid_signature}),
        serde_json::json!({"v": 2, "challenge_id": CHALLENGE_ID, "signature": format!("{valid_signature}=")}),
        serde_json::json!({"v": 2, "challenge_id": CHALLENGE_ID, "signature": "AA"}),
        serde_json::json!({"v": 2, "challenge_id": CHALLENGE_ID, "signature": "x".repeat(129)}),
    ];
    for proof in invalid_proofs {
        assert_eq!(
            ProofV2::decode_json(&serde_json::to_vec(&proof).unwrap()),
            Err(WireError::InvalidProof)
        );
    }

    let proof = ProofV2::decode_json(
        &serde_json::to_vec(&serde_json::json!({
            "v": 2,
            "challenge_id": CHALLENGE_ID,
            "signature": valid_signature
        }))
        .unwrap(),
    )
    .unwrap();
    let restart = RequestV2::decode_json(fixture.vector.restart_request_json.as_bytes()).unwrap();
    assert_eq!(
        verify_proof_signature(
            &restart,
            &published_challenge(),
            &proof,
            HOST_ENDPOINT_ID,
            CLIENT_ENDPOINT_ID,
            DEVICE_PUBLIC_KEY,
        ),
        Err(WireError::InvalidProof)
    );
}

#[test]
fn malformed_duplicate_unknown_and_v1_json_never_enters_the_v2_codec() {
    for bytes in [
        &b"{"[..],
        &b"null"[..],
        &b"[]"[..],
        &b"{\"op\":\"list_agents\",\"op\":\"connect\"}"[..],
        &b"{\"op\":\"pair\",\"v\":2}"[..],
        &b"{\"op\":\"list_agents\",\"v\":1}"[..],
        &b"{\"op\":\"list_agents\",\"v\":2,\"v\":2}"[..],
    ] {
        assert_eq!(
            RequestV2::decode_json(bytes),
            Err(WireError::InvalidRequest)
        );
    }
    assert_eq!(ALPN, b"remora-link/2");
    assert_ne!(ALPN, b"alleycat/1");

    let response_duplicate =
        br#"{"v":2,"v":2,"ok":false,"error_code":"invalid_request","error":"invalid request"}"#;
    assert_eq!(
        ResponseV2::decode_json(response_duplicate),
        Err(WireError::InvalidResponse)
    );
    let proof_duplicate = format!(
        r#"{{"v":2,"v":2,"challenge_id":"{CHALLENGE_ID}","signature":"{}"}}"#,
        golden_vectors().vector.proof_signature_der_base64url
    );
    assert_eq!(
        ProofV2::decode_json(proof_duplicate.as_bytes()),
        Err(WireError::InvalidProof)
    );
}

#[test]
fn endpoint_id_and_response_ids_enforce_documented_bounds() {
    let payload_hash = list_agents_request().operation_payload_hash().unwrap();
    let key_hash = [0_u8; 32];
    for endpoint in ["".to_string(), "line\nbreak".to_string(), "x".repeat(257)] {
        assert_eq!(
            encode_proof_transcript(ProofTranscriptInput {
                host_endpoint_id: &endpoint,
                client_endpoint_id: CLIENT_ENDPOINT_ID,
                operation: "list_agents",
                credential_id: CREDENTIAL_ID,
                auth_epoch: 7,
                device_key_hash: &key_hash,
                challenge_id: CHALLENGE_ID,
                server_nonce: SERVER_NONCE,
                client_nonce: CLIENT_NONCE,
                operation_payload_hash: &payload_hash,
            }),
            Err(WireError::InvalidRequest)
        );
    }

    for (challenge_id, credential_id, server_nonce) in [
        ("short", CREDENTIAL_ID, SERVER_NONCE),
        (CHALLENGE_ID, "short", SERVER_NONCE),
        (CHALLENGE_ID, CREDENTIAL_ID, "short"),
    ] {
        assert_eq!(
            decode_response(serde_json::json!({
                "v": 2,
                "ok": true,
                "challenge": {
                    "challenge_id": challenge_id,
                    "credential_id": credential_id,
                    "auth_epoch": 7,
                    "server_nonce": server_nonce,
                    "expires_at": 1_900_000_000_i64
                }
            })),
            Err(WireError::InvalidResponse)
        );
    }
}

#[tokio::test]
async fn control_frames_use_u32be_and_enforce_the_exact_65536_byte_limit() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let request = list_agents_request();
    let expected = serde_json::to_vec(&request).unwrap();
    let (mut writer, mut reader) = tokio::io::duplex(MAX_CONTROL_FRAME_BYTES + 16);
    super::client::write_request_frame(&mut writer, &request)
        .await
        .unwrap();
    let mut prefix = [0_u8; 4];
    reader.read_exact(&mut prefix).await.unwrap();
    assert_eq!(prefix, (expected.len() as u32).to_be_bytes());
    let mut payload = vec![0_u8; expected.len()];
    reader.read_exact(&mut payload).await.unwrap();
    assert_eq!(payload, expected);

    let (mut writer, mut reader) = tokio::io::duplex(MAX_CONTROL_FRAME_BYTES + 16);
    writer
        .write_u32(MAX_CONTROL_FRAME_BYTES as u32)
        .await
        .unwrap();
    writer
        .write_all(&vec![b'x'; MAX_CONTROL_FRAME_BYTES])
        .await
        .unwrap();
    assert_eq!(
        super::client::read_frame_bytes_for_test(&mut reader)
            .await
            .unwrap()
            .len(),
        MAX_CONTROL_FRAME_BYTES
    );

    let mut sink = tokio::io::sink();
    assert_eq!(
        super::client::write_json_frame(&mut sink, &"x".repeat(MAX_CONTROL_FRAME_BYTES)).await,
        Err(super::client::ControlExchangeError::FrameTooLarge)
    );
}

#[tokio::test]
async fn framed_request_proof_and_response_readers_reject_oversize_or_invalid_data() {
    use tokio::io::AsyncWriteExt;

    for reader_kind in 0..3 {
        let (mut writer, mut reader) = tokio::io::duplex(8);
        writer
            .write_u32((MAX_CONTROL_FRAME_BYTES + 1) as u32)
            .await
            .unwrap();
        let error = match reader_kind {
            0 => super::client::read_request_frame(&mut reader)
                .await
                .map(|_| ()),
            1 => super::client::read_proof_frame(&mut reader)
                .await
                .map(|_| ()),
            _ => super::client::read_response_frame(&mut reader)
                .await
                .map(|_| ()),
        };
        assert_eq!(
            error,
            Err(super::client::ControlExchangeError::FrameTooLarge)
        );
    }

    let (mut writer, mut reader) = tokio::io::duplex(8);
    writer.write_u32(1).await.unwrap();
    writer.write_u8(0xff).await.unwrap();
    assert_eq!(
        super::client::read_frame_bytes_for_test(&mut reader).await,
        Err(super::client::ControlExchangeError::InvalidJson)
    );

    let (mut writer, mut reader) = tokio::io::duplex(16);
    writer.write_u32(1).await.unwrap();
    writer.write_all(b"{").await.unwrap();
    assert_eq!(
        super::client::read_request_frame(&mut reader).await,
        Err(super::client::ControlExchangeError::InvalidMessage)
    );
}

#[test]
fn typed_response_records_reject_invalid_nested_bounds_and_relations() {
    let invalid = [
        serde_json::json!({
            "v": 2, "ok": true,
            "inspection": {
                "invitation_id": INVITATION_ID,
                "expires_at": 1_900_000_000_i64,
                "max_runtime_ids": ["codex"],
                "max_scopes": ["inspect_runtimes", "connect_runtime", "self_revoke"],
                "confirmation_mode": "interactive",
                "runtime_offers": [{"runtime_id": "codex", "display_name": "Codex", "available": false, "recommended": true}]
            }
        }),
        serde_json::json!({
            "v": 2, "ok": true,
            "pending": {
                "claim_id": "BAQEBAQEBAQEBAQEBAQEBA",
                "credential_id": CREDENTIAL_ID,
                "display_name": "Remora Phone",
                "selected_runtime_ids": ["codex"],
                "requested_scopes": ["inspect_runtimes", "connect_runtime", "self_revoke"],
                "enrollment_confirmation": confirmation_json(),
                "created_at": 20,
                "expires_at": 19
            }
        }),
        serde_json::json!({
            "v": 2, "ok": true,
            "agents": [
                {"name": "codex", "display_name": "Codex", "wire": "jsonl", "available": true},
                {"name": "codex", "display_name": "Duplicate", "wire": "jsonl", "available": true}
            ]
        }),
        serde_json::json!({"v": 2, "ok": true, "session": {"attached": "resumed", "current_seq": 4, "floor_seq": 6}}),
        serde_json::json!({"v": 2, "ok": true, "restart": {"agent": "codex", "idempotency_key": "restart-1", "command_sequence": 0, "status": "succeeded"}}),
        serde_json::json!({"v": 2, "ok": true, "revocation": {"credential_id": CREDENTIAL_ID, "auth_epoch": 0, "revoked_at": 1, "idempotency_key": "revoke-1"}}),
        serde_json::json!({"v": 2, "ok": true, "agents": [{"name": "codex", "display_name": "Codex", "wire": "jsonl", "available": true, "presentation": {"aliases": ["same", "same"]}}]}),
        serde_json::json!({"v": 2, "ok": true, "agents": [{"name": "codex", "display_name": "Codex", "wire": "jsonl", "available": true, "capabilities": {"visible_modes": ["same", "same"]}}]}),
    ];
    for value in invalid {
        assert_eq!(decode_response(value), Err(WireError::InvalidResponse));
    }
}

#[test]
fn agent_presentation_accepts_pinned_host_display_aliases_with_spaces() {
    decode_response(serde_json::json!({
        "v": 2,
        "ok": true,
        "agents": [{
            "name": "codex",
            "display_name": "Codex",
            "wire": "jsonl",
            "available": true,
            "presentation": {
                "aliases": ["amp code", "open code", "factory droid", "xai grok"]
            }
        }]
    }))
    .expect("pinned host presentation aliases are display labels, not runtime IDs");
}

#[test]
fn challenge_and_terminal_correlation_rejects_cross_operation_substitution() {
    let request = list_agents_request();
    let wrong_credential = challenge_for(INVITATION_ID, 7);
    let response = decode_response(serde_json::json!({
        "v": 2,
        "ok": true,
        "challenge": wrong_credential
    }))
    .unwrap();
    assert_eq!(
        response.validate_challenge_for_request(&request, CLIENT_ENDPOINT_ID),
        Err(WireError::InvalidResponse)
    );

    let wrong_epoch = challenge_for(INVITATION_ID, 1);
    let inspect = RequestV2::decode_json(
        format!(
            r#"{{"op":"inspect_invitation","v":2,"invitation_id":"{INVITATION_ID}","secret":"{INVITATION_SECRET}","device_public_key":"{DEVICE_PUBLIC_KEY}","client_nonce":"{CLIENT_NONCE}"}}"#
        )
        .as_bytes(),
    )
    .unwrap();
    let response = decode_response(serde_json::json!({
        "v": 2,
        "ok": true,
        "challenge": wrong_epoch
    }))
    .unwrap();
    assert_eq!(
        response.validate_challenge_for_request(&inspect, CLIENT_ENDPOINT_ID),
        Err(WireError::InvalidResponse)
    );

    let restart =
        RequestV2::decode_json(golden_vectors().vector.restart_request_json.as_bytes()).unwrap();
    let mismatched_terminal = decode_response(serde_json::json!({
        "v": 2,
        "ok": true,
        "restart": {
            "agent": "codex",
            "idempotency_key": "different-operation",
            "command_sequence": 1,
            "status": "succeeded"
        }
    }))
    .unwrap();
    assert_eq!(
        mismatched_terminal.validate_terminal_shape_for_request(
            &restart,
            &published_challenge(),
            CLIENT_ENDPOINT_ID,
        ),
        Err(WireError::InvalidResponse)
    );
}
