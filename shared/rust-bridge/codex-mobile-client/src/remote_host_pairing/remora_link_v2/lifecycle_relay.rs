use super::super::wire::{PROTOCOL_VERSION, RelayBarrierV2, RelayCommitV2, RelayEnrollmentV2};
use super::*;

impl PairingLifecycleV2 {
    pub(crate) async fn enroll_relay(
        &self,
        host_id: &str,
    ) -> Result<RelayEnrollmentV2, LifecycleErrorV2> {
        let (response, _) = self
            .relay_exchange(host_id, |credential_id, client_nonce| {
                // The random pairing credential supplies the entropy; domain
                // separation makes retries stable even before relay staging exists.
                let idempotency_key = Self::relay_command_id(host_id, &credential_id);
                RequestV2::RelayEnroll {
                    v: PROTOCOL_VERSION,
                    credential_id,
                    client_nonce,
                    idempotency_key,
                }
            })
            .await?;
        response
            .relay_enrollment
            .ok_or(LifecycleErrorV2::ProtocolViolation)
    }

    pub(crate) async fn commit_relay(
        &self,
        host_id: &str,
        installation_id: String,
        command_id: String,
    ) -> Result<RelayCommitV2, LifecycleErrorV2> {
        let (response, _) = self
            .relay_exchange(host_id, |credential_id, client_nonce| {
                RequestV2::RelayCommit {
                    v: PROTOCOL_VERSION,
                    credential_id,
                    client_nonce,
                    installation_id,
                    idempotency_key: command_id,
                }
            })
            .await?;
        response
            .relay_commit
            .ok_or(LifecycleErrorV2::ProtocolViolation)
    }

    pub(crate) async fn relay_barrier(
        &self,
        host_id: &str,
        installation_id: String,
        through_cursor: u64,
    ) -> Result<RelayBarrierV2, LifecycleErrorV2> {
        let (response, mut expected_runtimes) = self
            .relay_exchange(host_id, |credential_id, client_nonce| {
                RequestV2::RelayBarrier {
                    v: PROTOCOL_VERSION,
                    credential_id,
                    client_nonce,
                    installation_id,
                    through_cursor,
                }
            })
            .await?;
        let barrier = response
            .relay_barrier
            .ok_or(LifecycleErrorV2::ProtocolViolation)?;
        let mut actual_runtimes = barrier.runtime_ids.clone();
        expected_runtimes.sort();
        actual_runtimes.sort();
        if actual_runtimes != expected_runtimes {
            return Err(LifecycleErrorV2::ProtocolViolation);
        }
        Ok(barrier)
    }

    async fn relay_exchange(
        &self,
        host_id: &str,
        request: impl FnOnce(String, String) -> RequestV2,
    ) -> Result<(ResponseV2, Vec<String>), LifecycleErrorV2> {
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
        for scope in [
            DeviceScopeV2::InspectRuntimes,
            DeviceScopeV2::ConnectRuntime,
        ] {
            if !credential.granted_scopes.contains(&scope) {
                return Err(LifecycleErrorV2::InvalidSelection);
            }
        }
        let request = request(credential.credential_id, self.fresh_nonce());
        let started = self.begin_round(&mut entry, &request, false).await?;
        let finished = self
            .complete_round(&mut entry, &request, started, Some(credential.auth_epoch))
            .await?;
        if finished.attachment_id.is_some() {
            return Err(LifecycleErrorV2::ProtocolViolation);
        }
        if !finished.response.ok {
            return Err(map_terminal_error(&finished.response));
        }
        Ok((finished.response, credential.selected_runtime_ids))
    }
    pub(crate) fn relay_command_id(host_id: &str, credential_id: &str) -> String {
        let mut hash = Sha256::new();
        hash.update(b"remora-relay-enrollment-v1\0");
        for value in [host_id, credential_id] {
            hash.update((value.len() as u64).to_be_bytes());
            hash.update(value.as_bytes());
        }
        format!("txn_{}", hex::encode(hash.finalize()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_command_identity_is_stable_and_bound_to_host_and_credential() {
        let relay_command_id = PairingLifecycleV2::relay_command_id;
        let command = relay_command_id("host-a", "credential-a");
        assert_eq!(command, relay_command_id("host-a", "credential-a"));
        assert_ne!(command, relay_command_id("host-b", "credential-a"));
        assert_ne!(command, relay_command_id("host-a", "credential-b"));
        assert_eq!(command.len(), 68);
    }
}
