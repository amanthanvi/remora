use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use p256::ecdsa::signature::Signer as _;
use p256::ecdsa::{Signature, SigningKey};
use sha2::{Digest, Sha256};
use tokio::sync::Notify;

use super::super::identity::V2Invite;
use super::lifecycle::*;
use super::v2_journal::*;
use super::v2_ports::*;
use super::wire::*;

const HOST_ENDPOINT_ID: &str = "af06a3e3291714e4f356c19c9b15cd1951ec6e6662aa77be07547f289383341d";
const CLIENT_ENDPOINT_ID: &str = "2df04125f0015afb47ce853aef8772094ff9498c14cb1b9e12973c2927da0fa6";

#[derive(Default)]
struct MemoryJournalV2 {
    entries: StdMutex<HashMap<String, PairingJournalEntryV2>>,
    fail_candidate_write: AtomicBool,
    fail_next_write: AtomicBool,
    fail_write_countdown: AtomicUsize,
    conflict_next_write: AtomicBool,
}

impl MemoryJournalV2 {
    fn entry(&self, host_id: &str) -> PairingJournalEntryV2 {
        self.entries.lock().unwrap()[host_id].clone()
    }

    fn fail_after_writes(&self, count: usize) {
        assert!(count > 0);
        self.fail_write_countdown.store(count, Ordering::SeqCst);
    }
}

#[async_trait]
impl JournalPortV2 for MemoryJournalV2 {
    async fn load(
        &self,
        host_id: &str,
    ) -> Result<Option<PairingJournalEntryV2>, JournalPortErrorV2> {
        Ok(self.entries.lock().unwrap().get(host_id).cloned())
    }

    async fn compare_and_swap(
        &self,
        host_id: &str,
        expected_revision: Option<u64>,
        replacement: PairingJournalEntryV2,
    ) -> Result<(), JournalPortErrorV2> {
        if self.conflict_next_write.swap(false, Ordering::SeqCst) {
            return Err(JournalPortErrorV2::Conflict);
        }
        let countdown = self.fail_write_countdown.load(Ordering::SeqCst);
        let countdown_failed =
            countdown > 0 && self.fail_write_countdown.fetch_sub(1, Ordering::SeqCst) == 1;
        if self.fail_next_write.swap(false, Ordering::SeqCst)
            || countdown_failed
            || (self.fail_candidate_write.load(Ordering::SeqCst)
                && replacement
                    .enrollment
                    .as_ref()
                    .is_some_and(|value| !value.candidates.is_empty()))
        {
            return Err(JournalPortErrorV2::Unavailable);
        }
        let mut entries = self.entries.lock().unwrap();
        if entries.get(host_id).map(|value| value.revision) != expected_revision {
            return Err(JournalPortErrorV2::Conflict);
        }
        entries.insert(host_id.to_string(), replacement);
        Ok(())
    }
}

struct TestCustodyV2 {
    signing_key: SigningKey,
    key: HardwareKeyV2,
    present: AtomicBool,
    load_missing_error: AtomicBool,
    fail_delete: AtomicBool,
    block_sign: AtomicBool,
    sign_started: Notify,
    release_sign: Notify,
    sign_count: AtomicUsize,
}

impl TestCustodyV2 {
    fn new() -> Self {
        let signing_key = SigningKey::from_bytes((&[7_u8; 32]).into()).unwrap();
        let public_key = URL_SAFE_NO_PAD.encode(
            signing_key
                .verifying_key()
                .to_encoded_point(false)
                .as_bytes(),
        );
        Self {
            signing_key,
            key: HardwareKeyV2 {
                slot: "remora-link-key-slot".to_string(),
                public_key,
            },
            present: AtomicBool::new(true),
            load_missing_error: AtomicBool::new(false),
            fail_delete: AtomicBool::new(false),
            block_sign: AtomicBool::new(false),
            sign_started: Notify::new(),
            release_sign: Notify::new(),
            sign_count: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl CredentialCustodyPortV2 for TestCustodyV2 {
    async fn ensure_hardware_key(
        &self,
        _host_id: &str,
    ) -> Result<HardwareKeyV2, CredentialPortError> {
        self.present.store(true, Ordering::SeqCst);
        Ok(self.key.clone())
    }

    async fn load_hardware_key(
        &self,
        slot: &str,
    ) -> Result<Option<HardwareKeyV2>, CredentialPortError> {
        assert_eq!(slot, self.key.slot);
        if self.load_missing_error.load(Ordering::SeqCst) {
            return Err(CredentialPortError::Missing);
        }
        Ok(self
            .present
            .load(Ordering::SeqCst)
            .then(|| self.key.clone()))
    }

    async fn sign_message(
        &self,
        slot: &str,
        message: &[u8],
    ) -> Result<Vec<u8>, CredentialPortError> {
        assert_eq!(slot, self.key.slot);
        if !self.present.load(Ordering::SeqCst) {
            return Err(CredentialPortError::Missing);
        }
        if self.block_sign.load(Ordering::SeqCst) {
            self.sign_started.notify_one();
            self.release_sign.notified().await;
        }
        self.sign_count.fetch_add(1, Ordering::SeqCst);
        let signature: Signature = self.signing_key.sign(message);
        Ok(signature.to_der().as_bytes().to_vec())
    }

    async fn delete_hardware_key(&self, slot: &str) -> Result<(), CredentialPortError> {
        assert_eq!(slot, self.key.slot);
        if self.fail_delete.load(Ordering::SeqCst) {
            return Err(CredentialPortError::Unavailable);
        }
        self.present.store(false, Ordering::SeqCst);
        Ok(())
    }
}

#[derive(Default)]
struct TestEntropyV2 {
    nonce: AtomicU64,
    id: AtomicU64,
}

impl EntropyPortV2 for TestEntropyV2 {
    fn fresh_nonce(&self) -> [u8; 32] {
        let value = self.nonce.fetch_add(1, Ordering::SeqCst) + 1;
        [u8::try_from(value % 251 + 1).unwrap(); 32]
    }

    fn fresh_idempotency_key(&self, purpose: &'static str) -> String {
        format!(
            "{purpose}-operation-{}",
            self.id.fetch_add(1, Ordering::SeqCst) + 1
        )
    }
}

#[derive(Clone)]
struct RoundV2 {
    request: RequestV2,
    challenge: ProofChallengeV2,
    client_endpoint_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EnrollmentTerminal {
    Pending,
    Enrolled,
}

struct ScriptedHostV2 {
    rounds: StdMutex<HashMap<String, RoundV2>>,
    next_round: AtomicU64,
    public_key: String,
    invite: V2Invite,
    identity_drift: AtomicBool,
    invalid_challenge: AtomicBool,
    challenge_expires_at: AtomicI64,
    policy_drift: AtomicBool,
    endpoint_fingerprint_mismatch: AtomicBool,
    device_key_fingerprint_mismatch: AtomicBool,
    confirmation_mismatch: AtomicBool,
    malformed_enrollment_terminal: AtomicBool,
    pending_drift_on_replay: AtomicBool,
    lose_first_enrollment_response: AtomicBool,
    enrollment_terminal: StdMutex<EnrollmentTerminal>,
    accepted_confirmation: StdMutex<Option<EnrollmentConfirmationV2>>,
    enroll_keys: StdMutex<Vec<String>>,
    mutation_keys: StdMutex<Vec<String>>,
    host_auth_epoch: AtomicU64,
    lose_first_mutation_response: AtomicBool,
    mutation_outcome_unknown_once: AtomicBool,
    mutation_receipt_drift_on_replay: AtomicBool,
    mutation_receipt_epoch_mismatch: AtomicBool,
    restart_calls: StdMutex<Vec<(String, u64, String)>>,
    lose_first_restart_response: AtomicBool,
    restart_outcome_unknown_once: AtomicBool,
    finish_count: AtomicUsize,
    abandon_count: AtomicUsize,
    close_count: AtomicUsize,
    connect_count: AtomicUsize,
    connect_resume_cursors: StdMutex<Vec<Option<u64>>>,
    fail_connect: AtomicBool,
    include_out_of_grant_agent: AtomicBool,
}

impl ScriptedHostV2 {
    fn new(public_key: String, invite: V2Invite) -> Self {
        Self {
            rounds: StdMutex::new(HashMap::new()),
            next_round: AtomicU64::new(0),
            public_key,
            invite,
            identity_drift: AtomicBool::new(false),
            invalid_challenge: AtomicBool::new(false),
            challenge_expires_at: AtomicI64::new(1_900_000_000),
            policy_drift: AtomicBool::new(false),
            endpoint_fingerprint_mismatch: AtomicBool::new(false),
            device_key_fingerprint_mismatch: AtomicBool::new(false),
            confirmation_mismatch: AtomicBool::new(false),
            malformed_enrollment_terminal: AtomicBool::new(false),
            pending_drift_on_replay: AtomicBool::new(false),
            lose_first_enrollment_response: AtomicBool::new(false),
            enrollment_terminal: StdMutex::new(EnrollmentTerminal::Enrolled),
            accepted_confirmation: StdMutex::new(None),
            enroll_keys: StdMutex::new(Vec::new()),
            mutation_keys: StdMutex::new(Vec::new()),
            host_auth_epoch: AtomicU64::new(0),
            lose_first_mutation_response: AtomicBool::new(false),
            mutation_outcome_unknown_once: AtomicBool::new(false),
            mutation_receipt_drift_on_replay: AtomicBool::new(false),
            mutation_receipt_epoch_mismatch: AtomicBool::new(false),
            restart_calls: StdMutex::new(Vec::new()),
            lose_first_restart_response: AtomicBool::new(false),
            restart_outcome_unknown_once: AtomicBool::new(false),
            finish_count: AtomicUsize::new(0),
            abandon_count: AtomicUsize::new(0),
            close_count: AtomicUsize::new(0),
            connect_count: AtomicUsize::new(0),
            connect_resume_cursors: StdMutex::new(Vec::new()),
            fail_connect: AtomicBool::new(false),
            include_out_of_grant_agent: AtomicBool::new(false),
        }
    }

    fn terminal(ok: bool) -> ResponseV2 {
        ResponseV2 {
            v: PROTOCOL_VERSION,
            ok,
            challenge: None,
            enrolled: None,
            inspection: None,
            pending: None,
            revocation: None,
            restart: None,
            agents: None,
            session: None,
            error_code: None,
            error: None,
        }
    }

    fn confirmation(&self, round: &RoundV2) -> EnrollmentConfirmationV2 {
        let RequestV2::Enroll {
            invitation_id,
            device_public_key,
            idempotency_key,
            selected_runtime_ids,
            requested_scopes,
            client_nonce,
            ..
        } = &round.request
        else {
            panic!("enrollment request")
        };
        let hash = enrollment_transcript_hash(EnrollmentTranscriptInput {
            host_endpoint_id: HOST_ENDPOINT_ID,
            client_endpoint_id: &round.client_endpoint_id,
            invitation_id,
            device_public_key,
            idempotency_key,
            selected_runtime_ids,
            requested_scopes,
            server_nonce: &round.challenge.server_nonce,
            client_nonce,
            confirmation_mode: self.invite.confirmation_mode,
            max_runtime_ids: &self.invite.max_runtime_ids,
            max_scopes: &self.invite.max_scopes,
        })
        .unwrap();
        let secret = URL_SAFE_NO_PAD.encode(&self.invite.secret);
        EnrollmentConfirmationV2 {
            transcript_hash: URL_SAFE_NO_PAD.encode(hash),
            sas: derive_sas(&secret, &hash).unwrap(),
        }
    }

    fn verify(&self, round: &RoundV2, proof: &ProofV2) {
        let key = match &round.request {
            RequestV2::InspectInvitation {
                device_public_key, ..
            }
            | RequestV2::Enroll {
                device_public_key, ..
            } => device_public_key,
            _ => &self.public_key,
        };
        verify_proof_signature(
            &round.request,
            &round.challenge,
            proof,
            HOST_ENDPOINT_ID,
            &round.client_endpoint_id,
            key,
        )
        .unwrap();
    }
}

#[async_trait]
impl HostPortV2 for ScriptedHostV2 {
    async fn start_exchange(
        &self,
        route: &HostRouteV2,
        request: &RequestV2,
    ) -> Result<StartedExchangeV2, HostPortErrorV2> {
        assert_eq!(route.node_id, HOST_ENDPOINT_ID);
        let round_number = self.next_round.fetch_add(1, Ordering::SeqCst) + 1;
        let exchange_id = format!("exchange-{round_number}");
        let challenge_id = URL_SAFE_NO_PAD.encode([round_number as u8; 16]);
        let server_nonce = URL_SAFE_NO_PAD.encode([round_number as u8 + 40; 32]);
        let credential_id = match request {
            RequestV2::InspectInvitation { invitation_id, .. } => invitation_id.clone(),
            RequestV2::Enroll {
                invitation_id,
                device_public_key,
                idempotency_key,
                ..
            } => prospective_credential_id(
                invitation_id,
                CLIENT_ENDPOINT_ID,
                device_public_key,
                idempotency_key,
            )
            .unwrap(),
            _ => request.credential_id().unwrap().to_string(),
        };
        let auth_epoch = match request {
            RequestV2::InspectInvitation { .. } | RequestV2::Enroll { .. } => 0,
            _ => self.host_auth_epoch.load(Ordering::SeqCst),
        };
        let challenge = ProofChallengeV2 {
            challenge_id,
            credential_id: if self.invalid_challenge.load(Ordering::SeqCst) {
                "invalid-credential-id".to_string()
            } else {
                credential_id
            },
            auth_epoch,
            server_nonce,
            expires_at: self.challenge_expires_at.load(Ordering::SeqCst),
        };
        self.rounds.lock().unwrap().insert(
            exchange_id.clone(),
            RoundV2 {
                request: request.clone(),
                challenge: challenge.clone(),
                client_endpoint_id: CLIENT_ENDPOINT_ID.to_string(),
            },
        );
        Ok(StartedExchangeV2 {
            exchange_id,
            authenticated_host_endpoint_id: if self.identity_drift.load(Ordering::SeqCst) {
                "changed-host-endpoint".to_string()
            } else {
                HOST_ENDPOINT_ID.to_string()
            },
            authenticated_client_endpoint_id: CLIENT_ENDPOINT_ID.to_string(),
            challenge_response: ResponseV2 {
                challenge: Some(challenge),
                ..Self::terminal(true)
            },
        })
    }

    async fn finish_exchange(
        &self,
        exchange_id: &str,
        proof: &ProofV2,
    ) -> Result<FinishedExchangeV2, HostPortErrorV2> {
        self.finish_count.fetch_add(1, Ordering::SeqCst);
        let round = self
            .rounds
            .lock()
            .unwrap()
            .remove(exchange_id)
            .expect("known exchange");
        self.verify(&round, proof);
        match &round.request {
            RequestV2::InspectInvitation { invitation_id, .. } => {
                let mut response = Self::terminal(true);
                response.inspection = Some(InvitationInspectionV2 {
                    invitation_id: invitation_id.clone(),
                    expires_at: self.invite.expires_at
                        + i64::from(self.policy_drift.load(Ordering::SeqCst)),
                    max_runtime_ids: self.invite.max_runtime_ids.clone(),
                    max_scopes: self.invite.max_scopes.clone(),
                    confirmation_mode: self.invite.confirmation_mode,
                    runtime_offers: self
                        .invite
                        .max_runtime_ids
                        .iter()
                        .rev()
                        .enumerate()
                        .map(|(index, runtime_id)| RuntimeOfferV2 {
                            runtime_id: runtime_id.clone(),
                            display_name: runtime_id.clone(),
                            available: true,
                            recommended: index == 0,
                        })
                        .collect(),
                });
                Ok(FinishedExchangeV2 {
                    response,
                    attachment_id: None,
                })
            }
            RequestV2::Enroll {
                device_name,
                selected_runtime_ids,
                requested_scopes,
                idempotency_key,
                ..
            } => {
                let enroll_attempt = {
                    let mut keys = self.enroll_keys.lock().unwrap();
                    keys.push(idempotency_key.clone());
                    keys.len()
                };
                let confirmation = {
                    let mut accepted = self.accepted_confirmation.lock().unwrap();
                    accepted
                        .get_or_insert_with(|| self.confirmation(&round))
                        .clone()
                };
                if self
                    .lose_first_enrollment_response
                    .swap(false, Ordering::SeqCst)
                {
                    return Err(HostPortErrorV2::Unavailable);
                }
                let mut confirmation = confirmation;
                if self.confirmation_mismatch.load(Ordering::SeqCst) {
                    confirmation.sas = "000-000".to_string();
                }
                if self.malformed_enrollment_terminal.load(Ordering::SeqCst) {
                    return Ok(FinishedExchangeV2 {
                        response: Self::terminal(true),
                        attachment_id: None,
                    });
                }
                let mut response = Self::terminal(true);
                match *self.enrollment_terminal.lock().unwrap() {
                    EnrollmentTerminal::Pending => {
                        response.pending = Some(PendingEnrollmentV2 {
                            claim_id: URL_SAFE_NO_PAD.encode(
                                [if enroll_attempt > 1
                                    && self.pending_drift_on_replay.load(Ordering::SeqCst)
                                {
                                    8_u8
                                } else {
                                    9_u8
                                }; 16],
                            ),
                            credential_id: round.challenge.credential_id.clone(),
                            display_name: normalize_device_name(device_name),
                            selected_runtime_ids: selected_runtime_ids.clone(),
                            requested_scopes: requested_scopes.clone(),
                            enrollment_confirmation: confirmation,
                            created_at: 1_800_000_000,
                            expires_at: 1_900_000_000,
                        });
                    }
                    EnrollmentTerminal::Enrolled => {
                        response.enrolled = Some(EnrolledDeviceV2 {
                            device_id: round.challenge.credential_id.clone(),
                            display_name: normalize_device_name(device_name),
                            endpoint_fingerprint: if self
                                .endpoint_fingerprint_mismatch
                                .load(Ordering::SeqCst)
                            {
                                "0000000000000000".to_string()
                            } else {
                                endpoint_fingerprint(CLIENT_ENDPOINT_ID)
                            },
                            device_key_fingerprint: if self
                                .device_key_fingerprint_mismatch
                                .load(Ordering::SeqCst)
                            {
                                "0000000000000000".to_string()
                            } else {
                                device_key_fingerprint(&self.public_key)
                            },
                            selected_runtime_ids: selected_runtime_ids.clone(),
                            granted_scopes: requested_scopes.clone(),
                            auth_epoch: self.host_auth_epoch.load(Ordering::SeqCst),
                            created_at: 1_800_000_000,
                            enrollment_confirmation: confirmation,
                        });
                    }
                }
                Ok(FinishedExchangeV2 {
                    response,
                    attachment_id: None,
                })
            }
            RequestV2::Connect { resume, .. } => {
                self.connect_count.fetch_add(1, Ordering::SeqCst);
                self.connect_resume_cursors
                    .lock()
                    .unwrap()
                    .push(resume.as_ref().map(|resume| resume.last_seq));
                if self.fail_connect.load(Ordering::SeqCst) {
                    let mut response = Self::terminal(false);
                    response.error_code = Some(ErrorCodeV2::AgentUnavailable);
                    response.error = Some(ErrorCodeV2::AgentUnavailable.message().to_string());
                    return Ok(FinishedExchangeV2 {
                        response,
                        attachment_id: None,
                    });
                }
                let mut response = Self::terminal(true);
                response.session = Some(SessionV2 {
                    attached: AttachKindV2::Resumed,
                    current_seq: 44,
                    floor_seq: 10,
                });
                Ok(FinishedExchangeV2 {
                    response,
                    attachment_id: Some(format!("attachment-{exchange_id}")),
                })
            }
            RequestV2::ListAgents { .. } => {
                let mut response = Self::terminal(true);
                let mut agents = vec![AgentInfoV2 {
                    name: "codex".to_string(),
                    display_name: "Codex".to_string(),
                    wire: AgentWireV2::Jsonl,
                    available: true,
                    presentation: None,
                    capabilities: None,
                }];
                if self.include_out_of_grant_agent.load(Ordering::SeqCst) {
                    agents.push(AgentInfoV2 {
                        name: "not-granted".to_string(),
                        display_name: "Not granted".to_string(),
                        wire: AgentWireV2::Jsonl,
                        available: true,
                        presentation: None,
                        capabilities: None,
                    });
                }
                response.agents = Some(agents);
                Ok(FinishedExchangeV2 {
                    response,
                    attachment_id: None,
                })
            }
            RequestV2::RevokeSelf {
                credential_id,
                idempotency_key,
                ..
            }
            | RequestV2::RollbackEnrollment {
                credential_id,
                idempotency_key,
                ..
            } => {
                let mutation_attempt = {
                    let mut keys = self.mutation_keys.lock().unwrap();
                    keys.push(idempotency_key.clone());
                    keys.len()
                };
                let auth_epoch = self
                    .host_auth_epoch
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |epoch| {
                        Some(epoch.max(1))
                    })
                    .unwrap()
                    .max(1);
                let receipt = RevocationReceiptV2 {
                    credential_id: credential_id.clone(),
                    auth_epoch: auth_epoch
                        + u64::from(self.mutation_receipt_epoch_mismatch.load(Ordering::SeqCst)),
                    revoked_at: 1_850_000_000
                        + i64::from(
                            mutation_attempt > 1
                                && self.mutation_receipt_drift_on_replay.load(Ordering::SeqCst),
                        ),
                    idempotency_key: idempotency_key.clone(),
                };
                if self
                    .lose_first_mutation_response
                    .swap(false, Ordering::SeqCst)
                {
                    return Err(HostPortErrorV2::Unavailable);
                }
                let unknown = self
                    .mutation_outcome_unknown_once
                    .swap(false, Ordering::SeqCst);
                let mut response = Self::terminal(!unknown);
                response.revocation = Some(receipt);
                if unknown {
                    response.error_code = Some(ErrorCodeV2::OutcomeUnknown);
                    response.error = Some(ErrorCodeV2::OutcomeUnknown.message().to_string());
                }
                Ok(FinishedExchangeV2 {
                    response,
                    attachment_id: None,
                })
            }
            RequestV2::RestartAgent {
                agent,
                idempotency_key,
                command_sequence,
                ..
            } => {
                self.restart_calls.lock().unwrap().push((
                    idempotency_key.clone(),
                    *command_sequence,
                    agent.clone(),
                ));
                if self
                    .lose_first_restart_response
                    .swap(false, Ordering::SeqCst)
                {
                    return Err(HostPortErrorV2::Unavailable);
                }
                let unknown = self
                    .restart_outcome_unknown_once
                    .swap(false, Ordering::SeqCst);
                let mut response = Self::terminal(!unknown);
                response.restart = Some(RestartResultV2 {
                    agent: agent.clone(),
                    idempotency_key: idempotency_key.clone(),
                    command_sequence: *command_sequence,
                    status: if unknown {
                        RestartStatusV2::OutcomeUnknown
                    } else {
                        RestartStatusV2::Succeeded
                    },
                });
                if unknown {
                    response.error_code = Some(ErrorCodeV2::OutcomeUnknown);
                    response.error = Some(ErrorCodeV2::OutcomeUnknown.message().to_string());
                }
                Ok(FinishedExchangeV2 {
                    response,
                    attachment_id: None,
                })
            }
        }
    }

    fn abandon_exchange(&self, exchange_id: &str) {
        self.abandon_count.fetch_add(1, Ordering::SeqCst);
        self.rounds.lock().unwrap().remove(exchange_id);
    }

    async fn close_local(&self, _host_id: &str) {
        self.close_count.fetch_add(1, Ordering::SeqCst);
    }
}

struct HarnessV2 {
    lifecycle: Arc<PairingLifecycleV2>,
    journal: Arc<MemoryJournalV2>,
    custody: Arc<TestCustodyV2>,
    host: Arc<ScriptedHostV2>,
    invite: V2Invite,
    host_id: String,
}

fn invite() -> V2Invite {
    V2Invite {
        node_id: HOST_ENDPOINT_ID.to_string(),
        invitation_id: vec![1_u8; 16],
        secret: vec![2_u8; 32],
        expires_at: 1_900_000_000,
        max_runtime_ids: vec!["codex".to_string()],
        max_scopes: vec![
            DeviceScopeV2::InspectRuntimes,
            DeviceScopeV2::ConnectRuntime,
            DeviceScopeV2::RestartRuntime,
            DeviceScopeV2::SelfRevoke,
        ],
        confirmation_mode: ConfirmationModeV2::Interactive,
        host_name: Some("Studio".to_string()),
        relay: Some("https://relay.example".to_string()),
    }
}

fn harness() -> HarnessV2 {
    harness_with_invite(invite())
}

fn harness_with_invite(invite: V2Invite) -> HarnessV2 {
    let host_id = format!("remora-link:{HOST_ENDPOINT_ID}");
    let journal = Arc::new(MemoryJournalV2::default());
    let custody = Arc::new(TestCustodyV2::new());
    let host = Arc::new(ScriptedHostV2::new(
        custody.key.public_key.clone(),
        invite.clone(),
    ));
    let lifecycle = Arc::new(PairingLifecycleV2::new(
        host.clone(),
        journal.clone(),
        custody.clone(),
        Arc::new(TestEntropyV2::default()),
    ));
    HarnessV2 {
        lifecycle,
        journal,
        custody,
        host,
        invite,
        host_id,
    }
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

fn endpoint_fingerprint(endpoint_id: &str) -> String {
    hex::encode(&Sha256::digest(endpoint_id.as_bytes())[..8])
}

fn device_key_fingerprint(public_key: &str) -> String {
    let bytes = URL_SAFE_NO_PAD.decode(public_key).unwrap();
    hex::encode(&Sha256::digest(bytes)[..8])
}

fn selection() -> (String, Vec<String>, Vec<DeviceScopeV2>) {
    (
        "Remora Phone".to_string(),
        vec!["codex".to_string()],
        vec![
            DeviceScopeV2::InspectRuntimes,
            DeviceScopeV2::ConnectRuntime,
            DeviceScopeV2::RestartRuntime,
            DeviceScopeV2::SelfRevoke,
        ],
    )
}

async fn inspect_and_enroll(harness: &HarnessV2) -> EnrollmentOutcomeV2 {
    harness.lifecycle.inspect(&harness.invite).await.unwrap();
    let (name, runtimes, scopes) = selection();
    harness
        .lifecycle
        .enroll(&harness.invite, name, runtimes, scopes)
        .await
        .unwrap()
}

#[tokio::test]
async fn enrollment_commits_only_after_hash_and_sas_match_and_journal_is_nonsecret() {
    let harness = harness();
    let outcome = inspect_and_enroll(&harness).await;
    assert!(matches!(outcome, EnrollmentOutcomeV2::Enrolled(_)));

    let entry = harness.journal.entry(&harness.host_id);
    assert_eq!(entry.phase, JournalPhaseV2::Enrolled);
    assert_eq!(entry.enrollment.as_ref().unwrap().candidates.len(), 1);
    let credential = entry.credential.as_ref().unwrap();
    assert_eq!(credential.auth_epoch, 0);
    assert_eq!(
        credential.endpoint_fingerprint,
        endpoint_fingerprint(CLIENT_ENDPOINT_ID)
    );
    assert_eq!(
        credential.device_key_fingerprint,
        device_key_fingerprint(&harness.custody.key.public_key)
    );
    let serialized = serde_json::to_string(&entry).unwrap();
    assert!(!serialized.contains(&URL_SAFE_NO_PAD.encode(&harness.invite.secret)));
    assert!(!serialized.contains("private_key"));
    assert!(!serialized.contains("signature"));
    assert_eq!(harness.custody.sign_count.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn enrollment_canonicalizes_display_order_but_rejects_duplicate_selections() {
    let mut multi_runtime_invite = invite();
    multi_runtime_invite.max_runtime_ids = vec!["claude".to_string(), "codex".to_string()];
    let harness = harness_with_invite(multi_runtime_invite);
    harness.lifecycle.inspect(&harness.invite).await.unwrap();

    assert_eq!(
        harness
            .lifecycle
            .enroll(
                &harness.invite,
                "Remora Phone".to_string(),
                vec!["codex".to_string(), "codex".to_string()],
                vec![DeviceScopeV2::InspectRuntimes],
            )
            .await,
        Err(LifecycleErrorV2::InvalidSelection)
    );
    assert_eq!(
        harness
            .lifecycle
            .enroll(
                &harness.invite,
                "Remora Phone".to_string(),
                vec!["codex".to_string()],
                vec![
                    DeviceScopeV2::InspectRuntimes,
                    DeviceScopeV2::InspectRuntimes,
                ],
            )
            .await,
        Err(LifecycleErrorV2::InvalidSelection)
    );

    let outcome = harness
        .lifecycle
        .enroll(
            &harness.invite,
            "Remora Phone".to_string(),
            vec!["codex".to_string(), "claude".to_string()],
            vec![
                DeviceScopeV2::SelfRevoke,
                DeviceScopeV2::RestartRuntime,
                DeviceScopeV2::ConnectRuntime,
                DeviceScopeV2::InspectRuntimes,
            ],
        )
        .await
        .unwrap();
    assert!(matches!(outcome, EnrollmentOutcomeV2::Enrolled(_)));

    let credential = harness
        .journal
        .entry(&harness.host_id)
        .credential
        .expect("enrollment must persist a credential");
    assert_eq!(
        credential.selected_runtime_ids,
        vec!["claude".to_string(), "codex".to_string()]
    );
    assert_eq!(
        credential.granted_scopes,
        vec![
            DeviceScopeV2::InspectRuntimes,
            DeviceScopeV2::ConnectRuntime,
            DeviceScopeV2::RestartRuntime,
            DeviceScopeV2::SelfRevoke,
        ]
    );
}

#[tokio::test]
async fn invalid_display_names_fail_before_mutating_the_ready_journal() {
    let harness = harness();
    harness.lifecycle.inspect(&harness.invite).await.unwrap();
    let ready = harness.journal.entry(&harness.host_id);
    let (_, runtimes, scopes) = selection();

    for invalid_name in ["bad\nname".to_string(), "x".repeat(81)] {
        assert_eq!(
            harness
                .lifecycle
                .enroll(
                    &harness.invite,
                    invalid_name,
                    runtimes.clone(),
                    scopes.clone(),
                )
                .await,
            Err(LifecycleErrorV2::InvalidSelection)
        );
        assert_eq!(harness.journal.entry(&harness.host_id), ready);
    }
}

#[tokio::test]
async fn lost_enrollment_response_replays_exact_key_and_matches_the_older_candidate() {
    let harness = harness();
    harness.lifecycle.inspect(&harness.invite).await.unwrap();
    harness
        .host
        .lose_first_enrollment_response
        .store(true, Ordering::SeqCst);
    let (name, runtimes, scopes) = selection();
    assert_eq!(
        harness
            .lifecycle
            .enroll(
                &harness.invite,
                name.clone(),
                runtimes.clone(),
                scopes.clone()
            )
            .await,
        Err(LifecycleErrorV2::HostUnavailable)
    );
    assert_eq!(
        harness
            .journal
            .entry(&harness.host_id)
            .enrollment
            .as_ref()
            .unwrap()
            .candidates
            .len(),
        1
    );

    let retry = harness
        .lifecycle
        .enroll(&harness.invite, name, runtimes, scopes)
        .await
        .unwrap();
    assert!(matches!(retry, EnrollmentOutcomeV2::Enrolled(_)));
    let keys = harness.host.enroll_keys.lock().unwrap();
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0], keys[1]);
    assert_eq!(
        harness
            .journal
            .entry(&harness.host_id)
            .enrollment
            .unwrap()
            .candidates
            .len(),
        1,
        "only the host-selected historical candidate remains after commit"
    );
}

#[tokio::test]
async fn mismatched_confirmation_never_enrolls_and_durably_schedules_rollback() {
    let harness = harness();
    harness.lifecycle.inspect(&harness.invite).await.unwrap();
    harness
        .host
        .confirmation_mismatch
        .store(true, Ordering::SeqCst);
    let (name, runtimes, scopes) = selection();
    assert_eq!(
        harness
            .lifecycle
            .enroll(&harness.invite, name, runtimes, scopes)
            .await,
        Err(LifecycleErrorV2::ConfirmationMismatch)
    );
    let entry = harness.journal.entry(&harness.host_id);
    assert_eq!(entry.phase, JournalPhaseV2::RollbackPending);
    assert!(entry.credential.is_none());
    assert_eq!(
        entry.mutation.as_ref().unwrap().kind,
        MutationKindV2::RollbackEnrollment
    );
    assert!(harness.custody.present.load(Ordering::SeqCst));
}

#[tokio::test]
async fn missing_confirmation_never_enrolls_and_durably_schedules_rollback() {
    let harness = harness();
    harness.lifecycle.inspect(&harness.invite).await.unwrap();
    harness
        .host
        .malformed_enrollment_terminal
        .store(true, Ordering::SeqCst);
    let (name, runtimes, scopes) = selection();
    assert_eq!(
        harness
            .lifecycle
            .enroll(&harness.invite, name, runtimes, scopes)
            .await,
        Err(LifecycleErrorV2::ProtocolViolation)
    );
    let entry = harness.journal.entry(&harness.host_id);
    assert_eq!(entry.phase, JournalPhaseV2::RollbackPending);
    assert!(entry.credential.is_none());
    assert_eq!(
        entry.mutation.as_ref().unwrap().kind,
        MutationKindV2::RollbackEnrollment
    );
}

#[tokio::test]
async fn ambiguous_duplicate_confirmation_candidate_fails_closed_to_rollback() {
    let harness = harness();
    harness.lifecycle.inspect(&harness.invite).await.unwrap();
    harness
        .host
        .lose_first_enrollment_response
        .store(true, Ordering::SeqCst);
    let (name, runtimes, scopes) = selection();
    assert_eq!(
        harness
            .lifecycle
            .enroll(
                &harness.invite,
                name.clone(),
                runtimes.clone(),
                scopes.clone()
            )
            .await,
        Err(LifecycleErrorV2::HostUnavailable)
    );
    {
        let mut entries = harness.journal.entries.lock().unwrap();
        let entry = entries.get_mut(&harness.host_id).unwrap();
        let duplicate = entry.enrollment.as_ref().unwrap().candidates[0].clone();
        entry
            .enrollment
            .as_mut()
            .unwrap()
            .candidates
            .push(duplicate);
    }
    assert_eq!(
        harness
            .lifecycle
            .enroll(&harness.invite, name, runtimes, scopes)
            .await,
        Err(LifecycleErrorV2::ConfirmationMismatch)
    );
    assert_eq!(
        harness.journal.entry(&harness.host_id).phase,
        JournalPhaseV2::RollbackPending
    );
}

#[tokio::test]
async fn candidate_persistence_failure_abandons_before_sending_a_proof() {
    let harness = harness();
    harness.lifecycle.inspect(&harness.invite).await.unwrap();
    let signed_before = harness.custody.sign_count.load(Ordering::SeqCst);
    let finished_before = harness.host.finish_count.load(Ordering::SeqCst);
    harness
        .journal
        .fail_candidate_write
        .store(true, Ordering::SeqCst);
    let (name, runtimes, scopes) = selection();
    assert_eq!(
        harness
            .lifecycle
            .enroll(&harness.invite, name, runtimes, scopes)
            .await,
        Err(LifecycleErrorV2::JournalUnavailable)
    );
    assert_eq!(
        harness.custody.sign_count.load(Ordering::SeqCst),
        signed_before
    );
    assert_eq!(
        harness.host.finish_count.load(Ordering::SeqCst),
        finished_before
    );
}

#[tokio::test]
async fn enrollment_plan_write_failure_or_conflict_never_touches_transport() {
    for conflict in [false, true] {
        let harness = harness();
        harness.lifecycle.inspect(&harness.invite).await.unwrap();
        let signed_before = harness.custody.sign_count.load(Ordering::SeqCst);
        if conflict {
            harness
                .journal
                .conflict_next_write
                .store(true, Ordering::SeqCst);
        } else {
            harness
                .journal
                .fail_next_write
                .store(true, Ordering::SeqCst);
        }
        let (name, runtimes, scopes) = selection();
        assert_eq!(
            harness
                .lifecycle
                .enroll(&harness.invite, name, runtimes, scopes)
                .await,
            Err(if conflict {
                LifecycleErrorV2::JournalConflict
            } else {
                LifecycleErrorV2::JournalUnavailable
            })
        );
        assert!(harness.host.enroll_keys.lock().unwrap().is_empty());
        assert_eq!(
            harness.custody.sign_count.load(Ordering::SeqCst),
            signed_before
        );
        assert_eq!(
            harness.journal.entry(&harness.host_id).phase,
            JournalPhaseV2::Ready
        );
    }
}

#[tokio::test]
async fn final_enrollment_commit_failure_replays_the_exact_accepted_claim() {
    let harness = harness();
    harness.lifecycle.inspect(&harness.invite).await.unwrap();
    harness.journal.fail_after_writes(3);
    let (name, runtimes, scopes) = selection();
    assert_eq!(
        harness
            .lifecycle
            .enroll(
                &harness.invite,
                name.clone(),
                runtimes.clone(),
                scopes.clone(),
            )
            .await,
        Err(LifecycleErrorV2::JournalUnavailable)
    );
    let staged = harness.journal.entry(&harness.host_id);
    assert_eq!(staged.phase, JournalPhaseV2::EnrollmentStaged);
    assert!(staged.credential.is_none());
    assert_eq!(staged.enrollment.unwrap().candidates.len(), 1);

    assert!(matches!(
        harness
            .lifecycle
            .enroll(&harness.invite, name, runtimes, scopes)
            .await
            .unwrap(),
        EnrollmentOutcomeV2::Enrolled(_)
    ));
    let keys = harness.host.enroll_keys.lock().unwrap();
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0], keys[1]);
}

#[tokio::test]
async fn pending_enrollment_commit_failure_replays_the_exact_pending_claim() {
    let harness = harness();
    harness.lifecycle.inspect(&harness.invite).await.unwrap();
    *harness.host.enrollment_terminal.lock().unwrap() = EnrollmentTerminal::Pending;
    harness.journal.fail_after_writes(3);
    let (name, runtimes, scopes) = selection();
    assert_eq!(
        harness
            .lifecycle
            .enroll(
                &harness.invite,
                name.clone(),
                runtimes.clone(),
                scopes.clone(),
            )
            .await,
        Err(LifecycleErrorV2::JournalUnavailable)
    );
    assert!(matches!(
        harness
            .lifecycle
            .enroll(&harness.invite, name, runtimes, scopes)
            .await
            .unwrap(),
        EnrollmentOutcomeV2::Pending(_)
    ));
    assert_eq!(
        harness.journal.entry(&harness.host_id).phase,
        JournalPhaseV2::EnrollmentPending
    );
}

#[tokio::test]
async fn host_identity_drift_is_quarantined_before_any_proof() {
    let harness = harness();
    harness.host.identity_drift.store(true, Ordering::SeqCst);
    assert_eq!(
        harness.lifecycle.inspect(&harness.invite).await,
        Err(LifecycleErrorV2::HostIdentityDrift)
    );
    assert_eq!(harness.custody.sign_count.load(Ordering::SeqCst), 0);
    assert_eq!(
        harness.journal.entry(&harness.host_id).phase,
        JournalPhaseV2::Quarantined {
            reason: QuarantineReasonV2::HostIdentityDrift
        }
    );
}

#[tokio::test]
async fn malformed_challenge_is_abandoned_before_any_proof() {
    let harness = harness();
    harness.host.invalid_challenge.store(true, Ordering::SeqCst);
    assert_eq!(
        harness.lifecycle.inspect(&harness.invite).await,
        Err(LifecycleErrorV2::ProtocolViolation)
    );
    assert_eq!(harness.custody.sign_count.load(Ordering::SeqCst), 0);
    assert_eq!(harness.host.abandon_count.load(Ordering::SeqCst), 1);
    assert!(harness.host.rounds.lock().unwrap().is_empty());
}

#[tokio::test]
async fn stale_challenge_is_abandoned_before_signing_or_finishing() {
    for expires_at in [1, i64::MIN] {
        let harness = harness();
        harness
            .host
            .challenge_expires_at
            .store(expires_at, Ordering::SeqCst);
        assert_eq!(
            harness.lifecycle.inspect(&harness.invite).await,
            Err(LifecycleErrorV2::ProtocolViolation)
        );
        assert_eq!(harness.custody.sign_count.load(Ordering::SeqCst), 0);
        assert_eq!(harness.host.finish_count.load(Ordering::SeqCst), 0);
        assert_eq!(harness.host.abandon_count.load(Ordering::SeqCst), 1);
        assert!(harness.host.rounds.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn custody_missing_error_is_durably_quarantined() {
    let harness = harness();
    inspect_and_enroll(&harness).await;
    harness
        .custody
        .load_missing_error
        .store(true, Ordering::SeqCst);
    assert_eq!(
        harness
            .lifecycle
            .reconnect(&harness.host_id, "codex".to_string(), None)
            .await,
        Err(LifecycleErrorV2::MissingCredential)
    );
    assert_eq!(
        harness.journal.entry(&harness.host_id).phase,
        JournalPhaseV2::Quarantined {
            reason: QuarantineReasonV2::HardwareKeyDrift
        }
    );
}

#[tokio::test]
async fn policy_drift_is_quarantined_after_authenticated_inspection() {
    let harness = harness();
    harness.host.policy_drift.store(true, Ordering::SeqCst);
    assert_eq!(
        harness.lifecycle.inspect(&harness.invite).await,
        Err(LifecycleErrorV2::PolicyDrift)
    );
    assert_eq!(
        harness.journal.entry(&harness.host_id).phase,
        JournalPhaseV2::Quarantined {
            reason: QuarantineReasonV2::PolicyDrift
        }
    );
}

#[tokio::test]
async fn enrolled_endpoint_fingerprint_drift_fails_closed_to_rollback() {
    let harness = harness();
    harness.lifecycle.inspect(&harness.invite).await.unwrap();
    harness
        .host
        .endpoint_fingerprint_mismatch
        .store(true, Ordering::SeqCst);
    let (name, runtimes, scopes) = selection();
    assert_eq!(
        harness
            .lifecycle
            .enroll(&harness.invite, name, runtimes, scopes)
            .await,
        Err(LifecycleErrorV2::ClientIdentityDrift)
    );
    let entry = harness.journal.entry(&harness.host_id);
    assert_eq!(entry.phase, JournalPhaseV2::RollbackPending);
    assert!(entry.credential.is_none());
}

#[tokio::test]
async fn enrolled_device_key_fingerprint_drift_fails_closed_to_rollback() {
    let harness = harness();
    harness.lifecycle.inspect(&harness.invite).await.unwrap();
    harness
        .host
        .device_key_fingerprint_mismatch
        .store(true, Ordering::SeqCst);
    let (name, runtimes, scopes) = selection();
    assert_eq!(
        harness
            .lifecycle
            .enroll(&harness.invite, name, runtimes, scopes)
            .await,
        Err(LifecycleErrorV2::HardwareKeyDrift)
    );
    let entry = harness.journal.entry(&harness.host_id);
    assert_eq!(entry.phase, JournalPhaseV2::RollbackPending);
    assert!(entry.credential.is_none());
}

#[tokio::test]
async fn pending_is_not_success_and_approved_retry_keeps_the_enrollment_identity() {
    let harness = harness();
    harness.lifecycle.inspect(&harness.invite).await.unwrap();
    *harness.host.enrollment_terminal.lock().unwrap() = EnrollmentTerminal::Pending;
    let (name, runtimes, scopes) = selection();
    let first = harness
        .lifecycle
        .enroll(
            &harness.invite,
            name.clone(),
            runtimes.clone(),
            scopes.clone(),
        )
        .await
        .unwrap();
    assert!(matches!(first, EnrollmentOutcomeV2::Pending(_)));
    assert_eq!(
        harness.journal.entry(&harness.host_id).phase,
        JournalPhaseV2::EnrollmentPending
    );
    *harness.host.enrollment_terminal.lock().unwrap() = EnrollmentTerminal::Enrolled;
    let second = harness
        .lifecycle
        .enroll(&harness.invite, name, runtimes, scopes)
        .await
        .unwrap();
    assert!(matches!(second, EnrollmentOutcomeV2::Enrolled(_)));
    let keys = harness.host.enroll_keys.lock().unwrap();
    assert_eq!(keys[0], keys[1]);
}

#[tokio::test]
async fn unattended_pending_is_impossible_and_schedules_rollback() {
    let mut unattended = invite();
    unattended.confirmation_mode = ConfirmationModeV2::Unattended;
    unattended.max_scopes = vec![
        DeviceScopeV2::InspectRuntimes,
        DeviceScopeV2::ConnectRuntime,
        DeviceScopeV2::SelfRevoke,
    ];
    let harness = harness_with_invite(unattended);
    harness.lifecycle.inspect(&harness.invite).await.unwrap();
    *harness.host.enrollment_terminal.lock().unwrap() = EnrollmentTerminal::Pending;
    assert_eq!(
        harness
            .lifecycle
            .enroll(
                &harness.invite,
                "Remora Phone".to_string(),
                vec!["codex".to_string()],
                vec![
                    DeviceScopeV2::InspectRuntimes,
                    DeviceScopeV2::ConnectRuntime,
                    DeviceScopeV2::SelfRevoke,
                ],
            )
            .await,
        Err(LifecycleErrorV2::ProtocolViolation)
    );
    assert_eq!(
        harness.journal.entry(&harness.host_id).phase,
        JournalPhaseV2::RollbackPending
    );
}

#[tokio::test]
async fn host_normalized_pending_names_remain_valid_for_whitespace_and_empty_input() {
    for (raw, expected) in [
        ("  Aman's iPhone  ", "Aman's iPhone"),
        ("", "Remora device"),
    ] {
        let harness = harness();
        harness.lifecycle.inspect(&harness.invite).await.unwrap();
        *harness.host.enrollment_terminal.lock().unwrap() = EnrollmentTerminal::Pending;
        let (_, runtimes, scopes) = selection();
        let outcome = harness
            .lifecycle
            .enroll(&harness.invite, raw.to_string(), runtimes, scopes)
            .await
            .unwrap();
        let EnrollmentOutcomeV2::Pending(pending) = outcome else {
            panic!("expected pending enrollment")
        };
        assert_eq!(pending.display_name, expected);
    }
}

#[tokio::test]
async fn pending_exact_replay_drift_fails_closed_to_rollback() {
    let harness = harness();
    harness.lifecycle.inspect(&harness.invite).await.unwrap();
    *harness.host.enrollment_terminal.lock().unwrap() = EnrollmentTerminal::Pending;
    let (name, runtimes, scopes) = selection();
    assert!(matches!(
        harness
            .lifecycle
            .enroll(
                &harness.invite,
                name.clone(),
                runtimes.clone(),
                scopes.clone(),
            )
            .await
            .unwrap(),
        EnrollmentOutcomeV2::Pending(_)
    ));
    harness
        .host
        .pending_drift_on_replay
        .store(true, Ordering::SeqCst);
    assert_eq!(
        harness
            .lifecycle
            .enroll(&harness.invite, name, runtimes, scopes)
            .await,
        Err(LifecycleErrorV2::ProtocolViolation)
    );
    assert_eq!(
        harness.journal.entry(&harness.host_id).phase,
        JournalPhaseV2::RollbackPending
    );
}

#[tokio::test]
async fn reconnect_is_host_runtime_scoped_and_uses_a_fresh_hardware_key_proof() {
    let harness = harness();
    inspect_and_enroll(&harness).await;
    let signed_before = harness.custody.sign_count.load(Ordering::SeqCst);
    let outcome = harness
        .lifecycle
        .reconnect(&harness.host_id, "codex".to_string(), Some(41))
        .await
        .unwrap();
    assert_eq!(outcome.runtime_id, "codex");
    assert_eq!(outcome.session.current_seq, 44);
    assert!(outcome.attachment_id.starts_with("attachment-exchange-"));
    assert_eq!(harness.host.connect_count.load(Ordering::SeqCst), 1);
    assert_eq!(
        harness.custody.sign_count.load(Ordering::SeqCst),
        signed_before + 1
    );
    let next_outcome = harness
        .lifecycle
        .reconnect(&harness.host_id, "codex".to_string(), Some(44))
        .await
        .unwrap();
    assert_ne!(
        outcome.attachment_id, next_outcome.attachment_id,
        "same-runtime reconnects must retain exact distinct attachment custody"
    );
    assert_eq!(harness.host.connect_count.load(Ordering::SeqCst), 2);
    assert_eq!(
        *harness.host.connect_resume_cursors.lock().unwrap(),
        vec![Some(41), Some(44)],
        "each reconnect must use only the v2 replay cursor supplied for that attachment"
    );
    assert_eq!(
        harness
            .lifecycle
            .reconnect(&harness.host_id, "claude".to_string(), None)
            .await,
        Err(LifecycleErrorV2::InvalidSelection)
    );
}

#[tokio::test]
async fn reconnect_maps_failed_terminal_before_requiring_an_attachment() {
    let harness = harness();
    inspect_and_enroll(&harness).await;
    harness.host.fail_connect.store(true, Ordering::SeqCst);

    assert_eq!(
        harness
            .lifecycle
            .reconnect(&harness.host_id, "codex".to_string(), None)
            .await,
        Err(LifecycleErrorV2::AgentUnavailable)
    );
}

#[tokio::test]
async fn list_agents_rejects_host_results_outside_the_grant_allowlist() {
    let harness = harness();
    inspect_and_enroll(&harness).await;
    harness
        .host
        .include_out_of_grant_agent
        .store(true, Ordering::SeqCst);
    assert_eq!(
        harness.lifecycle.list_agents(&harness.host_id).await,
        Err(LifecycleErrorV2::ProtocolViolation)
    );
}

#[tokio::test]
async fn lost_restart_response_reuses_the_exact_sequence_and_idempotency_key() {
    let harness = harness();
    inspect_and_enroll(&harness).await;
    harness
        .host
        .lose_first_restart_response
        .store(true, Ordering::SeqCst);
    assert_eq!(
        harness
            .lifecycle
            .restart(&harness.host_id, "codex".to_string())
            .await,
        Err(LifecycleErrorV2::HostUnavailable)
    );
    let staged = harness.journal.entry(&harness.host_id);
    assert_eq!(staged.restart_high_watermark, 0);
    assert_eq!(
        staged.pending_restart.as_ref().unwrap().disposition,
        RestartDispositionV2::Prepared
    );

    assert_eq!(
        harness
            .lifecycle
            .restart(&harness.host_id, "codex".to_string())
            .await
            .unwrap(),
        RestartOutcomeV2::Succeeded {
            command_sequence: 1
        }
    );
    let calls = harness.host.restart_calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0], calls[1]);
    let committed = harness.journal.entry(&harness.host_id);
    assert_eq!(committed.restart_high_watermark, 1);
    assert!(committed.pending_restart.is_none());
}

#[tokio::test]
async fn recovery_replays_a_prepared_restart_from_a_new_lifecycle_instance() {
    let harness = harness();
    inspect_and_enroll(&harness).await;
    harness
        .host
        .lose_first_restart_response
        .store(true, Ordering::SeqCst);
    assert_eq!(
        harness
            .lifecycle
            .restart(&harness.host_id, "codex".to_string())
            .await,
        Err(LifecycleErrorV2::HostUnavailable)
    );
    let recovered = PairingLifecycleV2::new(
        harness.host.clone(),
        harness.journal.clone(),
        harness.custody.clone(),
        Arc::new(TestEntropyV2::default()),
    );
    assert_eq!(
        recovered.recover(&harness.host_id).await.unwrap(),
        RecoveryOutcomeV2::Restart(RestartOutcomeV2::Succeeded {
            command_sequence: 1
        })
    );
    let calls = harness.host.restart_calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0], calls[1]);
}

#[tokio::test]
async fn restart_prepare_failure_never_dispatches_and_result_commit_failure_replays_exactly() {
    let prepare = harness();
    inspect_and_enroll(&prepare).await;
    prepare
        .journal
        .fail_next_write
        .store(true, Ordering::SeqCst);
    assert_eq!(
        prepare
            .lifecycle
            .restart(&prepare.host_id, "codex".to_string())
            .await,
        Err(LifecycleErrorV2::JournalUnavailable)
    );
    assert!(prepare.host.restart_calls.lock().unwrap().is_empty());

    let result = harness();
    inspect_and_enroll(&result).await;
    result.journal.fail_after_writes(2);
    assert_eq!(
        result
            .lifecycle
            .restart(&result.host_id, "codex".to_string())
            .await,
        Err(LifecycleErrorV2::JournalUnavailable)
    );
    let staged = result.journal.entry(&result.host_id);
    assert_eq!(staged.restart_high_watermark, 0);
    assert_eq!(
        staged.pending_restart.unwrap().disposition,
        RestartDispositionV2::Prepared
    );
    assert_eq!(
        result
            .lifecycle
            .restart(&result.host_id, "codex".to_string())
            .await
            .unwrap(),
        RestartOutcomeV2::Succeeded {
            command_sequence: 1
        }
    );
    let calls = result.host.restart_calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0], calls[1]);
}

#[tokio::test]
async fn restart_outcome_unknown_stops_automatic_retry_until_operator_acknowledges() {
    let harness = harness();
    inspect_and_enroll(&harness).await;
    harness
        .host
        .restart_outcome_unknown_once
        .store(true, Ordering::SeqCst);
    assert_eq!(
        harness
            .lifecycle
            .restart(&harness.host_id, "codex".to_string())
            .await
            .unwrap(),
        RestartOutcomeV2::OutcomeUnknown {
            command_sequence: 1
        }
    );
    let calls_before = harness.host.restart_calls.lock().unwrap().len();
    assert_eq!(
        harness.lifecycle.recover(&harness.host_id).await.unwrap(),
        RecoveryOutcomeV2::Restart(RestartOutcomeV2::OutcomeUnknown {
            command_sequence: 1
        })
    );
    assert_eq!(
        harness.host.restart_calls.lock().unwrap().len(),
        calls_before,
        "recovery must not retry a terminal ambiguous restart"
    );
    assert_eq!(
        harness
            .lifecycle
            .restart(&harness.host_id, "codex".to_string())
            .await
            .unwrap(),
        RestartOutcomeV2::OutcomeUnknown {
            command_sequence: 1
        }
    );
    assert_eq!(
        harness.host.restart_calls.lock().unwrap().len(),
        calls_before,
        "terminal ambiguity must not dispatch or retry automatically"
    );
    assert_eq!(
        harness
            .lifecycle
            .acknowledge_unknown_restart(&harness.host_id)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        harness
            .lifecycle
            .restart(&harness.host_id, "codex".to_string())
            .await
            .unwrap(),
        RestartOutcomeV2::Succeeded {
            command_sequence: 2
        }
    );
}

#[tokio::test]
async fn outcome_unknown_revocation_retains_key_and_exact_mutation_for_retry() {
    let harness = harness();
    inspect_and_enroll(&harness).await;
    harness
        .host
        .mutation_outcome_unknown_once
        .store(true, Ordering::SeqCst);
    assert_eq!(
        harness.lifecycle.revoke(&harness.host_id).await.unwrap(),
        MutationOutcomeV2::OutcomeUnknown
    );
    assert!(harness.custody.present.load(Ordering::SeqCst));
    assert_eq!(
        harness.journal.entry(&harness.host_id).phase,
        JournalPhaseV2::RevocationPending
    );
    assert_eq!(
        harness
            .journal
            .entry(&harness.host_id)
            .mutation
            .unwrap()
            .receipt
            .unwrap()
            .auth_epoch,
        1
    );
    assert_eq!(
        harness.lifecycle.revoke(&harness.host_id).await.unwrap(),
        MutationOutcomeV2::Revoked
    );
    assert!(!harness.custody.present.load(Ordering::SeqCst));
    assert_eq!(
        harness.journal.entry(&harness.host_id).phase,
        JournalPhaseV2::Revoked {
            key_cleanup_pending: false
        }
    );
    let keys = harness.host.mutation_keys.lock().unwrap();
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0], keys[1]);
}

#[tokio::test]
async fn exact_revocation_replay_rejects_a_drifting_receipt() {
    let harness = harness();
    inspect_and_enroll(&harness).await;
    harness
        .host
        .mutation_outcome_unknown_once
        .store(true, Ordering::SeqCst);
    assert_eq!(
        harness.lifecycle.revoke(&harness.host_id).await.unwrap(),
        MutationOutcomeV2::OutcomeUnknown
    );
    harness
        .host
        .mutation_receipt_drift_on_replay
        .store(true, Ordering::SeqCst);
    assert_eq!(
        harness.lifecycle.revoke(&harness.host_id).await,
        Err(LifecycleErrorV2::ProtocolViolation)
    );
    assert_eq!(
        harness.journal.entry(&harness.host_id).phase,
        JournalPhaseV2::RevocationPending
    );
    assert!(harness.custody.present.load(Ordering::SeqCst));
}

#[tokio::test]
async fn exact_rollback_replay_rejects_a_drifting_receipt() {
    let harness = harness();
    harness.lifecycle.inspect(&harness.invite).await.unwrap();
    harness
        .host
        .confirmation_mismatch
        .store(true, Ordering::SeqCst);
    let (name, runtimes, scopes) = selection();
    assert_eq!(
        harness
            .lifecycle
            .enroll(&harness.invite, name, runtimes, scopes)
            .await,
        Err(LifecycleErrorV2::ConfirmationMismatch)
    );
    harness
        .host
        .mutation_outcome_unknown_once
        .store(true, Ordering::SeqCst);
    assert_eq!(
        harness
            .lifecycle
            .cancel_enrollment(&harness.host_id)
            .await
            .unwrap(),
        MutationOutcomeV2::OutcomeUnknown
    );
    harness
        .host
        .mutation_receipt_drift_on_replay
        .store(true, Ordering::SeqCst);
    assert_eq!(
        harness.lifecycle.cancel_enrollment(&harness.host_id).await,
        Err(LifecycleErrorV2::ProtocolViolation)
    );
    assert_eq!(
        harness.journal.entry(&harness.host_id).phase,
        JournalPhaseV2::RollbackPending
    );
}

#[tokio::test]
async fn rollback_rejects_a_receipt_outside_the_initial_zero_to_one_epoch_fence() {
    let harness = harness();
    harness.lifecycle.inspect(&harness.invite).await.unwrap();
    harness
        .host
        .confirmation_mismatch
        .store(true, Ordering::SeqCst);
    let (name, runtimes, scopes) = selection();
    assert_eq!(
        harness
            .lifecycle
            .enroll(&harness.invite, name, runtimes, scopes)
            .await,
        Err(LifecycleErrorV2::ConfirmationMismatch)
    );
    harness
        .host
        .mutation_receipt_epoch_mismatch
        .store(true, Ordering::SeqCst);
    assert_eq!(
        harness.lifecycle.cancel_enrollment(&harness.host_id).await,
        Err(LifecycleErrorV2::ProtocolViolation)
    );
    assert_eq!(
        harness.journal.entry(&harness.host_id).phase,
        JournalPhaseV2::RollbackPending
    );
    assert!(harness.custody.present.load(Ordering::SeqCst));
}

#[tokio::test]
async fn lost_revocation_response_replays_at_the_hosts_incremented_epoch() {
    let harness = harness();
    inspect_and_enroll(&harness).await;
    harness
        .host
        .lose_first_mutation_response
        .store(true, Ordering::SeqCst);
    assert_eq!(
        harness.lifecycle.revoke(&harness.host_id).await,
        Err(LifecycleErrorV2::HostUnavailable)
    );
    let pending = harness.journal.entry(&harness.host_id);
    assert_eq!(pending.phase, JournalPhaseV2::RevocationPending);
    assert!(pending.mutation.as_ref().unwrap().receipt.is_none());
    assert!(harness.custody.present.load(Ordering::SeqCst));

    assert_eq!(
        harness.lifecycle.revoke(&harness.host_id).await.unwrap(),
        MutationOutcomeV2::Revoked
    );
    let keys = harness.host.mutation_keys.lock().unwrap();
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0], keys[1]);
}

#[tokio::test]
async fn mutation_prepare_failure_never_sends_or_deletes_the_key() {
    let harness = harness();
    inspect_and_enroll(&harness).await;
    harness
        .journal
        .fail_next_write
        .store(true, Ordering::SeqCst);
    assert_eq!(
        harness.lifecycle.revoke(&harness.host_id).await,
        Err(LifecycleErrorV2::JournalUnavailable)
    );
    assert!(harness.host.mutation_keys.lock().unwrap().is_empty());
    assert!(harness.custody.present.load(Ordering::SeqCst));
    assert_eq!(
        harness.journal.entry(&harness.host_id).phase,
        JournalPhaseV2::Enrolled
    );
}

#[tokio::test]
async fn outcome_unknown_receipt_commit_failure_retains_exact_replay_authority() {
    let harness = harness();
    inspect_and_enroll(&harness).await;
    harness
        .host
        .mutation_outcome_unknown_once
        .store(true, Ordering::SeqCst);
    harness.journal.fail_after_writes(2);
    assert_eq!(
        harness.lifecycle.revoke(&harness.host_id).await,
        Err(LifecycleErrorV2::JournalUnavailable)
    );
    let pending = harness.journal.entry(&harness.host_id);
    assert_eq!(pending.phase, JournalPhaseV2::RevocationPending);
    assert!(pending.mutation.unwrap().receipt.is_none());
    assert!(harness.custody.present.load(Ordering::SeqCst));
    assert_eq!(
        harness.lifecycle.revoke(&harness.host_id).await.unwrap(),
        MutationOutcomeV2::Revoked
    );
    let keys = harness.host.mutation_keys.lock().unwrap();
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0], keys[1]);
}

#[tokio::test]
async fn revoked_cleanup_marker_failure_recovers_after_idempotent_key_delete() {
    let harness = harness();
    inspect_and_enroll(&harness).await;
    harness.journal.fail_after_writes(3);
    assert_eq!(
        harness.lifecycle.revoke(&harness.host_id).await,
        Err(LifecycleErrorV2::JournalUnavailable)
    );
    assert!(!harness.custody.present.load(Ordering::SeqCst));
    assert_eq!(
        harness.journal.entry(&harness.host_id).phase,
        JournalPhaseV2::Revoked {
            key_cleanup_pending: true
        }
    );
    assert_eq!(
        harness.lifecycle.recover(&harness.host_id).await.unwrap(),
        RecoveryOutcomeV2::Mutation(MutationOutcomeV2::Revoked)
    );
    assert_eq!(
        harness.journal.entry(&harness.host_id).phase,
        JournalPhaseV2::Revoked {
            key_cleanup_pending: false
        }
    );
}

#[tokio::test]
async fn recover_replays_the_exact_pending_revocation() {
    let harness = harness();
    inspect_and_enroll(&harness).await;
    harness
        .host
        .mutation_outcome_unknown_once
        .store(true, Ordering::SeqCst);
    assert_eq!(
        harness.lifecycle.revoke(&harness.host_id).await.unwrap(),
        MutationOutcomeV2::OutcomeUnknown
    );
    assert_eq!(
        harness.lifecycle.recover(&harness.host_id).await.unwrap(),
        RecoveryOutcomeV2::Mutation(MutationOutcomeV2::Revoked)
    );
    let keys = harness.host.mutation_keys.lock().unwrap();
    assert_eq!(keys[0], keys[1]);
}

#[tokio::test]
async fn local_forget_never_claims_remote_revocation() {
    let harness = harness();
    inspect_and_enroll(&harness).await;
    let outcome = harness.lifecycle.forget(&harness.host_id).await.unwrap();
    assert_eq!(
        outcome,
        ForgetOutcomeV2::ForgottenLocally {
            host_id: harness.host_id.clone(),
            host_revocation_still_required: true,
        }
    );
    assert!(harness.host.mutation_keys.lock().unwrap().is_empty());
    assert!(!harness.custody.present.load(Ordering::SeqCst));
}

#[tokio::test]
async fn interrupted_local_forget_recovers_after_key_deletion_becomes_available() {
    let harness = harness();
    inspect_and_enroll(&harness).await;
    harness.custody.fail_delete.store(true, Ordering::SeqCst);
    assert_eq!(
        harness.lifecycle.forget(&harness.host_id).await,
        Err(LifecycleErrorV2::CredentialUnavailable)
    );
    assert_eq!(
        harness.journal.entry(&harness.host_id).phase,
        JournalPhaseV2::Forgetting {
            host_revocation_still_required: true
        }
    );
    harness.custody.fail_delete.store(false, Ordering::SeqCst);
    assert_eq!(
        harness.lifecycle.recover(&harness.host_id).await.unwrap(),
        RecoveryOutcomeV2::Forgotten(ForgetOutcomeV2::ForgottenLocally {
            host_id: harness.host_id.clone(),
            host_revocation_still_required: true,
        })
    );
}

#[tokio::test]
async fn cancel_ready_pairing_deletes_local_authority_without_a_remote_request() {
    let harness = harness();
    harness.lifecycle.inspect(&harness.invite).await.unwrap();
    let completed_exchanges = harness.host.finish_count.load(Ordering::SeqCst);
    let started_exchanges = harness.host.next_round.load(Ordering::SeqCst);
    let ready = harness.journal.entry(&harness.host_id);
    assert_eq!(ready.phase, JournalPhaseV2::Ready);
    assert!(ready.invitation.is_some());
    assert!(harness.custody.present.load(Ordering::SeqCst));

    assert_eq!(
        harness
            .lifecycle
            .cancel_enrollment(&harness.host_id)
            .await
            .unwrap(),
        MutationOutcomeV2::RolledBack
    );

    assert_eq!(
        harness.host.next_round.load(Ordering::SeqCst),
        started_exchanges
    );
    assert_eq!(
        harness.host.finish_count.load(Ordering::SeqCst),
        completed_exchanges
    );
    assert!(harness.host.mutation_keys.lock().unwrap().is_empty());
    assert!(!harness.custody.present.load(Ordering::SeqCst));
    let cancelled = harness.journal.entry(&harness.host_id);
    assert_eq!(
        cancelled.phase,
        JournalPhaseV2::Forgotten {
            host_revocation_still_required: false
        }
    );
    assert!(cancelled.invitation.is_none());
    assert!(cancelled.enrollment.is_none());
    assert!(cancelled.credential.is_none());
    assert!(cancelled.mutation.is_none());
}

#[tokio::test]
async fn cancel_before_any_enrollment_proof_is_local_only() {
    let harness = harness();
    harness.lifecycle.inspect(&harness.invite).await.unwrap();
    {
        let mut entries = harness.journal.entries.lock().unwrap();
        let entry = entries.get_mut(&harness.host_id).unwrap();
        entry.enrollment = Some(EnrollmentJournalV2 {
            display_name: "Remora Phone".to_string(),
            selected_runtime_ids: vec!["codex".to_string()],
            requested_scopes: vec![
                DeviceScopeV2::InspectRuntimes,
                DeviceScopeV2::ConnectRuntime,
                DeviceScopeV2::RestartRuntime,
                DeviceScopeV2::SelfRevoke,
            ],
            idempotency_key: "enrollment-operation-staged".to_string(),
            prospective_credential_id: None,
            candidates: Vec::new(),
            pending_claim: None,
        });
        entry.phase = JournalPhaseV2::EnrollmentStaged;
    }
    assert_eq!(
        harness
            .lifecycle
            .cancel_enrollment(&harness.host_id)
            .await
            .unwrap(),
        MutationOutcomeV2::RolledBack
    );
    assert!(harness.host.mutation_keys.lock().unwrap().is_empty());
    assert_eq!(
        harness.journal.entry(&harness.host_id).phase,
        JournalPhaseV2::Forgotten {
            host_revocation_still_required: false
        }
    );
}

#[tokio::test]
async fn journal_validation_rejects_widened_or_unbound_enrolled_authority() {
    let harness = harness();
    inspect_and_enroll(&harness).await;
    let enrolled = harness.journal.entry(&harness.host_id);

    let mut widened = enrolled.clone();
    widened
        .credential
        .as_mut()
        .unwrap()
        .selected_runtime_ids
        .push("other".to_string());
    assert_eq!(widened.validate(), Err(JournalValidationError::Corrupt));

    let mut unbound = enrolled;
    unbound.credential.as_mut().unwrap().transcript_hash = URL_SAFE_NO_PAD.encode([0_u8; 32]);
    assert_eq!(unbound.validate(), Err(JournalValidationError::Corrupt));
}

#[tokio::test]
async fn journal_validation_rejects_corrupt_runtime_offer_presentation() {
    let harness = harness();
    harness.lifecycle.inspect(&harness.invite).await.unwrap();
    let mut ready = harness.journal.entry(&harness.host_id);
    let offer = ready
        .invitation
        .as_mut()
        .unwrap()
        .runtime_offers
        .first_mut()
        .unwrap();
    offer.available = false;
    offer.recommended = true;

    assert_eq!(ready.validate(), Err(JournalValidationError::Corrupt));
}

#[tokio::test]
async fn cancelling_after_exchange_start_abandons_the_retained_round() {
    let harness = harness();
    harness.lifecycle.inspect(&harness.invite).await.unwrap();
    harness.custody.block_sign.store(true, Ordering::SeqCst);
    let sign_started = harness.custody.sign_started.notified();
    let (name, runtimes, scopes) = selection();
    let lifecycle = Arc::clone(&harness.lifecycle);
    let invite = harness.invite.clone();
    let mut enrollment =
        Box::pin(async move { lifecycle.enroll(&invite, name, runtimes, scopes).await });

    tokio::select! {
        _ = sign_started => {}
        result = &mut enrollment => panic!("enrollment completed before blocked signing: {result:?}"),
    }
    std::thread::spawn(move || drop(enrollment))
        .join()
        .expect("foreign-thread cancellation must not panic");

    tokio::time::timeout(Duration::from_secs(1), async {
        while harness.host.abandon_count.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("foreign-thread cancellation must abandon the exchange synchronously");
    assert!(harness.host.rounds.lock().unwrap().is_empty());
}

#[tokio::test]
async fn journal_validation_rejects_pending_claim_drift_and_restart_overflow() {
    let pending_harness = harness();
    pending_harness
        .lifecycle
        .inspect(&pending_harness.invite)
        .await
        .unwrap();
    *pending_harness.host.enrollment_terminal.lock().unwrap() = EnrollmentTerminal::Pending;
    let (name, runtimes, scopes) = selection();
    pending_harness
        .lifecycle
        .enroll(&pending_harness.invite, name, runtimes, scopes)
        .await
        .unwrap();
    let mut pending = pending_harness.journal.entry(&pending_harness.host_id);
    pending
        .enrollment
        .as_mut()
        .unwrap()
        .pending_claim
        .as_mut()
        .unwrap()
        .display_name = "drifted".to_string();
    assert_eq!(pending.validate(), Err(JournalValidationError::Corrupt));

    let enrolled_harness = harness();
    inspect_and_enroll(&enrolled_harness).await;
    let mut overflow = enrolled_harness.journal.entry(&enrolled_harness.host_id);
    overflow.restart_high_watermark = u64::MAX;
    overflow.pending_restart = Some(RestartCommandJournalV2 {
        runtime_id: "codex".to_string(),
        idempotency_key: "restart-overflow".to_string(),
        command_sequence: u64::MAX,
        disposition: RestartDispositionV2::Prepared,
    });
    assert_eq!(overflow.validate(), Err(JournalValidationError::Corrupt));
}

#[tokio::test]
async fn cancel_cannot_replace_an_enrolled_or_pending_revocation_lifecycle() {
    let harness = harness();
    inspect_and_enroll(&harness).await;
    assert_eq!(
        harness.lifecycle.cancel_enrollment(&harness.host_id).await,
        Err(LifecycleErrorV2::OperationInProgress)
    );
    assert_eq!(
        harness.journal.entry(&harness.host_id).phase,
        JournalPhaseV2::Enrolled
    );
    assert!(harness.host.mutation_keys.lock().unwrap().is_empty());

    harness
        .host
        .mutation_outcome_unknown_once
        .store(true, Ordering::SeqCst);
    assert_eq!(
        harness.lifecycle.revoke(&harness.host_id).await.unwrap(),
        MutationOutcomeV2::OutcomeUnknown
    );
    let stable_mutation = harness.journal.entry(&harness.host_id).mutation.unwrap();
    assert_eq!(
        harness.lifecycle.cancel_enrollment(&harness.host_id).await,
        Err(LifecycleErrorV2::OperationInProgress)
    );
    assert_eq!(
        harness.journal.entry(&harness.host_id).mutation.unwrap(),
        stable_mutation
    );
}

#[tokio::test]
async fn completed_rollback_is_idempotent_and_is_not_reported_as_revoke_self() {
    let harness = harness();
    harness.lifecycle.inspect(&harness.invite).await.unwrap();
    harness
        .host
        .confirmation_mismatch
        .store(true, Ordering::SeqCst);
    let (name, runtimes, scopes) = selection();
    assert_eq!(
        harness
            .lifecycle
            .enroll(&harness.invite, name, runtimes, scopes)
            .await,
        Err(LifecycleErrorV2::ConfirmationMismatch)
    );
    assert_eq!(
        harness
            .lifecycle
            .cancel_enrollment(&harness.host_id)
            .await
            .unwrap(),
        MutationOutcomeV2::RolledBack
    );
    assert_eq!(
        harness
            .lifecycle
            .cancel_enrollment(&harness.host_id)
            .await
            .unwrap(),
        MutationOutcomeV2::RolledBack
    );
    assert_eq!(
        harness.lifecycle.revoke(&harness.host_id).await,
        Err(LifecycleErrorV2::NotEnrolled)
    );
}
